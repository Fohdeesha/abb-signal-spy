//! An in-process stand-in for a controller's RobAPI InfoStream, behaving the way the
//! RW6 virtual controller did when measured on 2026-09-24 and 2026-09-25. It plays
//! the controller byte for byte (a real TCP listener, the real framing, typed sample
//! records) rather than mocking the client's internals, so a test against it
//! exercises the same code a controller does.
//!
//! What it reproduces, each from a measurement:
//!
//! * the handshake: system id and client list in two RADs, both padded with `p`
//! * command replies: a u32 status then text; refusals as the VC words them
//! * stream ids assigned controller-wide: the highest free id from 215 down, so a
//!   freed id is handed out again at once (measured 2026-09-25: an undefined 214 went
//!   to the very next define, and after another client's StreamUndefineAll its
//!   defines got 215 and 214, the ids the first client had been streaming on); or,
//!   with [`IdPools::Irc5`], the real IRC5's three pools (s24 item 4)
//! * sample frames on service 8, cause 1, txn 0, one sample per stream per frame,
//!   24 ms signals every sixth tick, integer signals as `LogsrvIntMsg`
//! * ONE subscription id for every client, and single tenancy: every sample frame
//!   goes to the connection that subscribed first; either client's StopStream,
//!   StartStream or StreamUndefineAll acts on everyone's streams. When that
//!   connection closes, a client that subscribed meanwhile gets nothing, whatever it
//!   sends (StartStream, SUBSCRIBE again); only a connection that subscribes after
//!   it becomes the new tenant (measured 2026-09-25, `tools/abb_vc_handover.py`)
//! * a define or undefine while streaming stops delivery until StartStream
//! * a closed connection's streams are reaped, and more (measured 2026-09-26,
//!   tunemaster-testsignals.md s24 items 9-11): when the tenant's connection closes,
//!   however it leaves, **every** stream goes, another connection's included; when a
//!   connection that defined a stream closes, every stream goes too and every
//!   subscriber still connected gets nothing more (only a new connection does). A
//!   connection that only handshook changes nothing when it leaves. One difference
//!   is not modelled: after the tenant left, the VC also starved a connection that had
//!   connected but not yet subscribed, where the real IRC5 served it; the fake does
//!   what the IRC5 did
//! * the handshake is answered whenever it is sent, mid-session too, with the
//!   current client list (s24 item 12)
//! * the AYA keepalive with ctrl 4000/16000; a zeroed reply drops the connection
//!   at once, no reply drops it at the timeout
//!
//! Loopback only, on a port the OS picks, so a test can never reach a controller or
//! stand in for one.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::sample::{self, Record, RecordValues, ValueKind};
use crate::wire::{self, cause, rad_format, rad_kind, service, Frame, FrameStatus, RadOut};

pub const SYSTEM_ID: &str = "{00000000-FA4E-4000-8000-000000000001}";
pub const SUBSCRIPTION_ID: u32 = 155_974_524;
pub const FIRST_STREAM_ID: u32 = 215;
pub const FIRST_TEXT_STREAM_ID: u32 = 233;
/// Text streams the VC gave before answering -50348 "no channel available". (The
/// IRC5's text pool size is unmeasured; the fake uses the same bound.)
pub const TEXT_POOL: usize = 4;

/// How a controller numbers its streams (tunemaster-testsignals.md s23 item 11, s24
/// item 4). Freed ids are handed out again at once in every pool.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum IdPools {
    /// The RW6 VC: 215 down; text from 233 up.
    #[default]
    Vc,
    /// The real IRC5: 259 down; the drive-side signals the VC does not have from 17
    /// down; text from 260 up.
    Irc5,
}

/// Signals the IRC5 numbered from its second pool, from 17 down (s24 item 4).
pub const IRC5_DRIVE_POOL_SIGNALS: [u32; 34] = [
    1188, 1531, 1887, 2332, 2772, 3680, 3896, 5027, 5722, 6093, 6740, 7000, 7001, 7002, 7003, 7004, 7005, 7006, 7007, 7008,
    7009, 7010, 7011, 7012, 7013, 7014, 7015, 7040, 7041, 7042, 7043, 7044, 7045, 9834,
];
const OK: u32 = 0x0004_8000;
const FAIL: u32 = 0xC004_FFFE;

/// Where and when a sample is taken: controller ms, mechanical unit, one-based axis.
pub type At<'a> = (u64, &'a str, u32);

/// How a signal behaves on this fake controller.
#[derive(Clone)]
pub enum SignalSource {
    /// A float that follows `f(at)`.
    Float(Arc<dyn Fn(At<'_>) -> f32 + Send + Sync>),
    /// An integer signal, sent as `LogsrvIntMsg`.
    Int(Arc<dyn Fn(At<'_>) -> i64 + Send + Sync>),
    /// A string event (`LogsrvStringMsg`): sent after each StartStream and then only
    /// when it changes, from a small pool of stream ids counting up from 233 (the
    /// RW6 VC's 221, 222, 225 and 9872, measured 2026-09-25).
    Text(Arc<dyn Fn(At<'_>) -> String + Send + Sync>),
    /// Accepted but never sends a sample.
    Silent,
}

impl SignalSource {
    pub fn float(f: impl Fn(At<'_>) -> f32 + Send + Sync + 'static) -> SignalSource {
        SignalSource::Float(Arc::new(f))
    }
    pub fn int(f: impl Fn(At<'_>) -> i64 + Send + Sync + 'static) -> SignalSource {
        SignalSource::Int(Arc::new(f))
    }
    pub fn text(f: impl Fn(At<'_>) -> String + Send + Sync + 'static) -> SignalSource {
        SignalSource::Text(Arc::new(f))
    }
}

impl std::fmt::Debug for SignalSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SignalSource::Float(_) => "Float(fn)",
            SignalSource::Int(_) => "Int(fn)",
            SignalSource::Text(_) => "Text(fn)",
            SignalSource::Silent => "Silent",
        })
    }
}

#[derive(Clone, Debug)]
pub struct SignalDef {
    pub source: SignalSource,
    /// Reported sample time: 4.032 or 24.192.
    pub sample_ms: f64,
}

/// What the client asked for, per connection, for the tests' assertions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seen {
    pub conn: usize,
    pub txn: u16,
    pub verb: String,
    pub property: String,
    pub args: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AyaAnswer {
    pub conn: usize,
    pub txn: u16,
    pub cause: u8,
    pub ctrl1: u32,
    pub ctrl2: u32,
}

#[derive(Clone, Debug)]
struct Stream {
    id: u32,
    owner: usize,
    signal: u32,
    unit: String,
    axis0: u32,
    sample_ms: f64,
    /// From the text pool.
    text: bool,
}

struct Conn {
    id: usize,
    writer: TcpStream,
    subscribed: bool,
    /// Subscribed while another connection was the tenant, or still subscribed when
    /// another connection's streams were torn down: never gets a sample again.
    orphan: bool,
    /// Defined a stream at some point: its leaving tears InfoStream down.
    defined: bool,
    /// Bytes kept back while `hold_delivery` is set.
    held: Vec<u8>,
    /// Broken, but the controller has not noticed: kept (as the tenant, if it was)
    /// until then, though nothing reaches the client.
    zombie_until: Option<Instant>,
    last_aya_answer: Instant,
    peer: SocketAddr,
}

/// Knobs a test turns.
pub struct Behaviour {
    pub signals: HashMap<u32, SignalDef>,
    /// Mechanical units and how many joints each has.
    pub units: HashMap<String, u32>,
    /// Tick of the sample clock. The VC ticks at exactly 4 ms.
    pub tick: Duration,
    /// Controller-clock milliseconds per tick; 4 on the VC. A test can make it wrap.
    pub stamp_step: u64,
    pub aya_interval: Duration,
    pub aya_timeout: Duration,
    /// Hold the reply to a define of this signal until the next define arrives on
    /// the same connection.
    pub hold_define_of: Option<u32>,
    /// Send the stamps modulo 2^32, as a controller that keeps them in 32 bits does.
    pub wrap32: bool,
    /// Answer every request after this delay (a slow controller).
    pub reply_delay: Duration,
    /// Stop sending samples for these signals (a dead feed on a live session).
    pub mute: HashSet<u32>,
    /// Stop all sample delivery (the whole feed dead, keepalives still flowing).
    pub mute_all: bool,
    /// Do not answer the handshake at all.
    pub mute_handshake: bool,
    /// Extra entries in the handshake's client list, beyond the connections.
    pub extra_clients: Vec<String>,
    /// Refuse the next define of each of these signals with this status, once.
    pub refuse_next: HashMap<u32, i64>,
    /// A stall on the network: everything for the clients (samples, replies,
    /// keepalives) is kept back, then delivered in order when this is cleared, as TCP
    /// delivers after a hiccup. The controller carries on meanwhile.
    pub hold_delivery: bool,
    /// How stream ids are numbered: the VC's way or the IRC5's.
    pub id_pools: IdPools,
}

impl Default for Behaviour {
    fn default() -> Behaviour {
        let mut signals = HashMap::new();
        let float = |f: fn(At<'_>) -> f32| SignalDef { source: SignalSource::Float(Arc::new(f)), sample_ms: 4.032 };
        // Joint angles, TCP x, the documented 4000-4003, a DC link, and a 24 ms
        // integer signal, with values a test can recognise: axis-indexed ones carry
        // their axis and unit in the value, so a misattributed stream shows.
        for n in 6000..=6005u32 {
            signals.insert(n, float(|(t, _, _)| ((t as f64) * 0.001).sin() as f32));
        }
        signals.insert(4000, float(|(t, _, a)| a as f32 * 10.0 + (t % 1000) as f32 * 0.001));
        signals.insert(4001, float(|_| 0.0));
        signals.insert(4002, float(|(_, u, a)| if u == "ROB_2" { 200.0 } else { 100.0 } + a as f32));
        signals.insert(4003, float(|_| -0.5));
        signals.insert(5027, float(|(_, u, _)| if u == "ROB_2" { 356.3 } else { 356.7 }));
        signals.insert(6040, float(|_| 0.9427));
        signals.insert(9888, SignalDef { source: SignalSource::int(|_| -1), sample_ms: 24.192 });
        signals.insert(9872, SignalDef { source: SignalSource::text(|_| "wobj0".into()), sample_ms: 4.032 });
        for n in [221u32, 222, 225, 9873, 9875] {
            signals.insert(n, SignalDef { source: SignalSource::text(move |_| format!("text{n}")), sample_ms: 4.032 });
        }
        signals.insert(9999, SignalDef { source: SignalSource::Silent, sample_ms: 4.032 });
        let mut units = HashMap::new();
        units.insert("ROB_1".to_string(), 6);
        units.insert("ROB_2".to_string(), 6);
        Behaviour {
            signals,
            units,
            tick: Duration::from_millis(4),
            stamp_step: 4,
            aya_interval: Duration::from_millis(4000),
            aya_timeout: Duration::from_millis(16000),
            hold_define_of: None,
            wrap32: false,
            reply_delay: Duration::ZERO,
            mute: HashSet::new(),
            mute_all: false,
            mute_handshake: false,
            extra_clients: Vec::new(),
            refuse_next: HashMap::new(),
            hold_delivery: false,
            id_pools: IdPools::Vc,
        }
    }
}

struct State {
    behaviour: Behaviour,
    conns: BTreeMap<usize, Conn>,
    next_conn: usize,
    streams: BTreeMap<u32, Stream>,
    /// What each text stream last sent; cleared by StartStream so they resend.
    text_sent: HashMap<u32, String>,
    streaming: bool,
    clock_ms: u64,
    tick_count: u64,
    seen: Vec<Seen>,
    aya_answers: Vec<AyaAnswer>,
    held_reply: Option<(usize, Vec<u8>)>,
    connections_total: usize,
    samples_sent: u64,
    handshakes: u64,
}

pub struct FakeController {
    addr: SocketAddr,
    state: Arc<Mutex<State>>,
    running: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

fn lock(s: &Mutex<State>) -> MutexGuard<'_, State> {
    // A panicking test thread must not wedge every other test through poisoning.
    s.lock().unwrap_or_else(|e| e.into_inner())
}

impl FakeController {
    pub fn start(behaviour: Behaviour) -> std::io::Result<FakeController> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        listener.set_nonblocking(true)?;
        let state = Arc::new(Mutex::new(State {
            behaviour,
            conns: BTreeMap::new(),
            next_conn: 1,
            streams: BTreeMap::new(),
            text_sent: HashMap::new(),
            streaming: false,
            clock_ms: 128_434_653,
            tick_count: 0,
            seen: Vec::new(),
            aya_answers: Vec::new(),
            held_reply: None,
            connections_total: 0,
            samples_sent: 0,
            handshakes: 0,
        }));
        let running = Arc::new(AtomicBool::new(true));
        let mut threads = Vec::new();

        {
            let (state, running) = (state.clone(), running.clone());
            threads.push(std::thread::Builder::new().name("fake-accept".into()).spawn(move || accept_loop(listener, state, running))?);
        }
        {
            let (state, running) = (state.clone(), running.clone());
            threads.push(std::thread::Builder::new().name("fake-tick".into()).spawn(move || tick_loop(state, running))?);
        }
        Ok(FakeController { addr, state, running, threads })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// Change the behaviour while running.
    pub fn with<R>(&self, f: impl FnOnce(&mut Behaviour) -> R) -> R {
        f(&mut lock(&self.state).behaviour)
    }

    pub fn seen(&self) -> Vec<Seen> {
        lock(&self.state).seen.clone()
    }
    pub fn seen_props(&self) -> Vec<String> {
        lock(&self.state).seen.iter().map(|s| s.property.clone()).collect()
    }
    pub fn aya_answers(&self) -> Vec<AyaAnswer> {
        lock(&self.state).aya_answers.clone()
    }
    /// Currently defined streams as (id, signal, unit, one-based axis).
    pub fn streams(&self) -> Vec<(u32, u32, String, u32)> {
        lock(&self.state).streams.values().map(|s| (s.id, s.signal, s.unit.clone(), s.axis0 + 1)).collect()
    }
    pub fn streaming(&self) -> bool {
        lock(&self.state).streaming
    }
    pub fn open_connections(&self) -> usize {
        lock(&self.state).conns.len()
    }
    pub fn connections_total(&self) -> usize {
        lock(&self.state).connections_total
    }
    pub fn samples_sent(&self) -> u64 {
        lock(&self.state).samples_sent
    }
    /// Handshake (control) frames received, from every connection.
    pub fn handshakes(&self) -> u64 {
        lock(&self.state).handshakes
    }
    pub fn clock_ms(&self) -> u64 {
        lock(&self.state).clock_ms
    }
    pub fn set_clock_ms(&self, ms: u64) {
        lock(&self.state).clock_ms = ms;
    }

    /// Send an AYA to every connection now.
    pub fn send_aya(&self) {
        let mut st = lock(&self.state);
        let frame = wire::encode_frame(0, service::AYA, cause::AYA, 4000, 16000, &[]);
        broadcast(&mut st, &frame);
    }

    /// Raw bytes to every open connection, as-is.
    pub fn inject(&self, bytes: &[u8]) {
        let mut st = lock(&self.state);
        broadcast(&mut st, bytes);
    }

    /// A sample frame with exactly these records, to the current tenant.
    pub fn inject_samples(&self, records: &[Record]) {
        let mut st = lock(&self.state);
        let rad = sample::encode_rad(SUBSCRIPTION_ID, 0x01DD_4CF3_AA5D_7B80, records, Some(0xAD));
        let frame = wire::encode_frame(0, service::EVENT, cause::EVENT, 0, 0, &[RadOut { kind: rad_kind::REPLY, format: rad_format::EVENT, data: &rad }]);
        if let Some(t) = tenant(&st) {
            send_to(&mut st, t, &frame);
        }
    }

    /// Drop every connection, the way a controller restart does.
    pub fn drop_connections(&self) {
        let st = lock(&self.state);
        for c in st.conns.values() {
            let _ = c.writer.shutdown(Shutdown::Both);
        }
    }

    /// Break every connection the way a network fault does: the clients see it
    /// drop, but the controller only lets go of it after `linger` (a real one does
    /// at its keepalive timeout), and until then it stays the tenant, so a client
    /// that connects again meanwhile subscribes as an orphan and gets nothing
    /// (measured 2026-09-25, `tools/abb_vc_dead_tenant.py`).
    pub fn break_connections(&self, linger: Duration) {
        let mut st = lock(&self.state);
        let until = Instant::now() + linger;
        for c in st.conns.values_mut() {
            c.zombie_until = Some(until);
            let _ = c.writer.shutdown(Shutdown::Both);
        }
    }

    pub fn stop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        self.drop_connections();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

impl Drop for FakeController {
    fn drop(&mut self) {
        self.stop();
    }
}

fn broadcast(st: &mut State, bytes: &[u8]) {
    let ids: Vec<usize> = st.conns.keys().copied().collect();
    for id in ids {
        send_to(st, id, bytes);
    }
}

fn send_to(st: &mut State, conn: usize, bytes: &[u8]) {
    let hold = st.behaviour.hold_delivery;
    let failed = match st.conns.get_mut(&conn) {
        Some(c) if hold => {
            c.held.extend_from_slice(bytes);
            false
        }
        Some(c) => {
            let held = std::mem::take(&mut c.held);
            (!held.is_empty() && c.writer.write_all(&held).is_err()) || c.writer.write_all(bytes).is_err()
        }
        None => false,
    };
    if failed {
        close_conn(st, conn);
    }
}

/// Where every sample goes: the earliest subscribed connection still open that did
/// not subscribe while another was the tenant.
fn tenant(st: &State) -> Option<usize> {
    st.conns.values().filter(|c| c.subscribed && !c.orphan).map(|c| c.id).min()
}

fn close_conn(st: &mut State, conn: usize) {
    // A broken connection the controller has not noticed yet stays until it does.
    if st.conns.get(&conn).is_some_and(|c| c.zombie_until.is_some_and(|t| Instant::now() < t)) {
        return;
    }
    let was_tenant = tenant(st) == Some(conn);
    let Some(c) = st.conns.remove(&conn) else { return };
    let _ = c.writer.shutdown(Shutdown::Both);
    if was_tenant {
        // s24 item 10: the tenant's exit, whatever it sent, clears every stream.
        st.streams.clear();
    } else if c.defined {
        // s24 item 11: so does the exit of a connection that defined a stream, and
        // the subscribers left get nothing more on their connections. Not a broken
        // connection the controller still holds: that one keeps the tenancy until it
        // lets go (s23 item 16), and what a leaving client does to it is unmeasured.
        st.streams.clear();
        for other in st.conns.values_mut() {
            if other.subscribed && other.zombie_until.is_none() {
                other.orphan = true;
            }
        }
    } else {
        st.streams.retain(|_, s| s.owner != conn);
    }
    if st.streams.is_empty() {
        st.streaming = false;
    }
}

fn accept_loop(listener: TcpListener, state: Arc<Mutex<State>>, running: Arc<AtomicBool>) {
    let mut readers: Vec<JoinHandle<()>> = Vec::new();
    while running.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, peer)) => {
                let _ = stream.set_nodelay(true);
                let _ = stream.set_nonblocking(false);
                let Ok(writer) = stream.try_clone() else { continue };
                let id = {
                    let mut st = lock(&state);
                    let id = st.next_conn;
                    st.next_conn += 1;
                    st.connections_total += 1;
                    st.conns.insert(id, Conn { id, writer, subscribed: false, orphan: false, defined: false, held: Vec::new(), zombie_until: None, last_aya_answer: Instant::now(), peer });
                    id
                };
                let (state, running) = (state.clone(), running.clone());
                if let Ok(h) = std::thread::Builder::new().name(format!("fake-conn-{id}")).spawn(move || conn_loop(id, stream, state, running)) {
                    readers.push(h);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(5)),
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    for h in readers {
        let _ = h.join();
    }
}

fn conn_loop(id: usize, mut stream: TcpStream, state: Arc<Mutex<State>>, running: Arc<AtomicBool>) {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(20)));
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    while running.load(Ordering::SeqCst) {
        if !lock(&state).conns.contains_key(&id) {
            break;
        }
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => continue,
            Err(_) => break,
        }
        loop {
            match wire::frame_at(&buf) {
                FrameStatus::NeedMore => break,
                FrameStatus::Desync(_) => {
                    // A controller drops a client that sends garbage.
                    let mut st = lock(&state);
                    close_conn(&mut st, id);
                    return;
                }
                FrameStatus::Complete(n) => {
                    let frame: Vec<u8> = buf.drain(..n).collect();
                    let delay = lock(&state).behaviour.reply_delay;
                    if !delay.is_zero() {
                        std::thread::sleep(delay);
                    }
                    handle_frame(id, &frame, &state);
                }
            }
        }
    }
    let mut st = lock(&state);
    close_conn(&mut st, id);
}

fn text_fields(d: &[u8]) -> Vec<String> {
    d.split(|&b| b == 0).map(|p| p.iter().map(|&b| b as char).collect()).collect()
}

fn reply_frame(txn: u16, service_no: u8, kind: u8, format: u8, cause_no: u8, data: &[u8], pad: bool) -> Vec<u8> {
    let mut f = wire::encode_frame(txn, service_no, cause_no, 0, 0, &[RadOut { kind, format, data }]);
    if pad {
        // The VC pads kind-0x23 RADs to four bytes with 'p' after the RAD.
        let rad_len = 4 + data.len();
        let padding = (4 - rad_len % 4) % 4;
        if padding > 0 {
            f.pop(); // trailer
            f.extend(std::iter::repeat_n(wire::PAD, padding));
            f.push(wire::TRAILER);
            let total = f.len() as u32;
            f[4..8].copy_from_slice(&total.to_be_bytes());
        }
    }
    f
}

fn status_text(status: u32, text: &str) -> Vec<u8> {
    let mut d = status.to_be_bytes().to_vec();
    d.extend_from_slice(text.as_bytes());
    d.push(0);
    d
}

fn handshake_reply(st: &State, txn: u16) -> Vec<u8> {
    let mut clients: Vec<String> = st.conns.values().map(|c| c.peer.ip().to_string()).collect();
    clients.extend(st.behaviour.extra_clients.iter().cloned());
    let list = format!("<i><cs>{}</cs></i>", clients.iter().map(|a| format!("<c a={a}/>")).collect::<String>());
    let mut sys = SYSTEM_ID.as_bytes().to_vec();
    sys.push(0);
    let mut lst = list.into_bytes();
    lst.push(0);
    // Built by hand: two RADs with 'p' padding between and after, then the
    // unexplained extra byte the VC sends before the trailer.
    let mut body = Vec::new();
    for (kind, data) in [(rad_kind::SYSTEM_ID, &sys), (rad_kind::REQUEST, &lst)] {
        let len = 4 + data.len();
        body.extend_from_slice(&(len as u16).to_be_bytes());
        body.push(kind);
        body.push(rad_format::TEXT << 1);
        body.extend_from_slice(data);
        body.extend(std::iter::repeat_n(wire::PAD, (4 - len % 4) % 4));
    }
    body.push(0x3B);
    let mut f = wire::encode_frame(txn, service::CONTROL, cause::RESPONSE, 257, 0, &[]);
    f.truncate(wire::HEADER_LEN);
    f[15] = 2;
    f.extend_from_slice(&body);
    f.push(wire::TRAILER);
    let total = f.len() as u32;
    f[4..8].copy_from_slice(&total.to_be_bytes());
    f
}

fn handle_frame(conn: usize, bytes: &[u8], state: &Arc<Mutex<State>>) {
    let Some(frame) = Frame::parse(bytes) else { return };
    let mut st = lock(state);
    match frame.service() {
        service::CONTROL => {
            st.handshakes += 1;
            if !st.behaviour.mute_handshake {
                let r = handshake_reply(&st, frame.txn());
                send_to(&mut st, conn, &r);
            }
        }
        service::AYA => {
            st.aya_answers.push(AyaAnswer { conn, txn: frame.txn(), cause: frame.cause(), ctrl1: frame.ctrl1(), ctrl2: frame.ctrl2() });
            if frame.ctrl1() == 0 && frame.ctrl2() == 0 {
                // Measured on the cell: a zeroed answer is rejected outright.
                close_conn(&mut st, conn);
            } else if let Some(c) = st.conns.get_mut(&conn) {
                c.last_aya_answer = Instant::now();
            }
        }
        service::REQUEST => {
            let Some(rad) = frame.rads().next() else { return };
            let fields = text_fields(rad.data);
            let verb = fields.first().cloned().unwrap_or_default();
            let prop = fields.get(2).cloned().unwrap_or_default();
            let args = fields.get(3).cloned().unwrap_or_default();
            let txn = frame.txn();
            st.seen.push(Seen { conn, txn, verb: verb.clone(), property: prop.clone(), args: args.clone() });
            if verb == "SUBSCRIBE" {
                let tenant_now = tenant(&st);
                if let Some(c) = st.conns.get_mut(&conn)
                    && !c.subscribed
                {
                    // Subscribing again changes nothing (the VC answered "TRUE 1").
                    c.subscribed = true;
                    c.orphan = tenant_now.is_some();
                }
                let r = reply_frame(txn, service::RESPONSE, rad_kind::REQUEST, rad_format::STATUS_TEXT, cause::NOTICE, &status_text(0, &format!("TRUE 0 {SUBSCRIPTION_ID}")), true);
                send_to(&mut st, conn, &r);
                // The small subscription notice the VC sends on service 3.
                let mut notice = 0x0004_8000u32.to_be_bytes().to_vec();
                notice.extend_from_slice(&SUBSCRIPTION_ID.to_be_bytes());
                notice.extend_from_slice(&[0x23, 0x3A, 0x4F, 0x50, 0x01, 0xDD, 0x4C, 0xD2, 0]);
                let n = reply_frame(0, service::SEND, rad_kind::REQUEST, rad_format::EVENT, cause::NOTICE, &notice, true);
                send_to(&mut st, conn, &n);
                return;
            }
            let reply = command(&mut st, conn, &prop, &args);
            let (status, text, is_define_of) = reply;
            let r = reply_frame(txn, service::RESPONSE, rad_kind::REPLY, rad_format::STATUS_TEXT, cause::RESPONSE, &status_text(status, &text), false);
            if prop == "StreamDefine" {
                // A reply held back for an earlier define on this connection goes out
                // just ahead of this one's.
                if st.held_reply.as_ref().is_some_and(|(c, _)| *c == conn)
                    && let Some((c, held)) = st.held_reply.take()
                {
                    send_to(&mut st, c, &held);
                }
                if is_define_of.is_some() && st.behaviour.hold_define_of == is_define_of {
                    st.behaviour.hold_define_of = None;
                    st.held_reply = Some((conn, r));
                    return;
                }
            }
            send_to(&mut st, conn, &r);
        }
        _ => {}
    }
}

fn arg(args: &str, key: &str) -> Option<String> {
    let mut it = args.split_whitespace();
    while let Some(tok) = it.next() {
        if tok == key {
            return it.next().map(str::to_string);
        }
    }
    None
}

/// Returns (status, text, Some(signal) for a define).
fn command(st: &mut State, conn: usize, prop: &str, args: &str) -> (u32, String, Option<u32>) {
    let refuse = |code: i64| {
        (FAIL, format!("ERROR: C:\\fake\\rdh_infostream.cpp[379]: code: 0xc004fffe Failed to define moc signal, status {code} streamId -1; "), None)
    };
    match prop {
        "SetProtocol" | "StartStream" => {
            if prop == "StartStream" {
                st.streaming = true;
                st.text_sent.clear();
            }
            (OK, String::new(), None)
        }
        "StreamConnect" => (0, String::new(), None),
        "StopStream" => {
            st.streaming = false; // controller-wide, measured
            (OK, String::new(), None)
        }
        "StreamDisconnect" => (OK, String::new(), None),
        "StreamUndefineAll" => {
            st.streams.clear(); // every client's, measured
            st.streaming = false;
            (OK, String::new(), None)
        }
        "StreamUndefine" => {
            if let Some(id) = arg(args, "-StreamId").and_then(|v| v.parse::<u32>().ok()) {
                st.streams.remove(&id);
            }
            // Measured: an undefine while streaming stops delivery for all streams.
            st.streaming = false;
            (OK, String::new(), None)
        }
        "StreamDefine" => {
            let signal = arg(args, "-Signal").and_then(|v| v.parse::<u32>().ok());
            let unit = arg(args, "-MechUnit").unwrap_or_default();
            let axis0 = arg(args, "-Axis").and_then(|v| v.parse::<u32>().ok());
            let (Some(signal), Some(axis0)) = (signal, axis0) else { return refuse(-50228) };
            let Some(&joints) = st.behaviour.units.get(&unit) else {
                let mut r = refuse(-50229);
                r.2 = Some(signal);
                return r;
            };
            let Some(def) = st.behaviour.signals.get(&signal).cloned() else {
                let mut r = refuse(-50228);
                r.2 = Some(signal);
                return r;
            };
            if axis0 >= joints {
                let mut r = refuse(-303);
                r.2 = Some(signal);
                return r;
            }
            if let Some(code) = st.behaviour.refuse_next.remove(&signal) {
                let mut r = refuse(code);
                r.2 = Some(signal);
                return r;
            }
            let is_text = matches!(def.source, SignalSource::Text(_));
            let no_channel = |signal| {
                let mut r = refuse(-50348);
                r.2 = Some(signal);
                r
            };
            let pools = st.behaviour.id_pools;
            let first_text = if pools == IdPools::Irc5 { 260 } else { FIRST_TEXT_STREAM_ID };
            // Each numeric pool: (its top, its bottom). The IRC5's main pool stops above
            // its second one, which a dozen channels never come near.
            let (top, bottom) = match pools {
                IdPools::Vc => (FIRST_STREAM_ID, 1),
                IdPools::Irc5 if IRC5_DRIVE_POOL_SIGNALS.contains(&signal) => (17, 1),
                IdPools::Irc5 => (259, 18),
            };
            let id = if is_text {
                if st.streams.values().filter(|s| s.text).count() >= TEXT_POOL {
                    return no_channel(signal);
                }
                // The lowest free: the VC gave 233 to 221, and later to 9872 and to
                // 9875 once it was free again (typed-vc-rw6.txt); the IRC5 260 up.
                match (first_text..).find(|id| !st.streams.contains_key(id)) {
                    Some(id) => id,
                    None => return no_channel(signal),
                }
            } else {
                // The highest free, from the pool's top down.
                match (bottom..=top).rev().find(|id| !st.streams.contains_key(id)) {
                    Some(id) => id,
                    None => return no_channel(signal),
                }
            };
            st.streams.insert(id, Stream { id, owner: conn, signal, unit, axis0, sample_ms: def.sample_ms, text: is_text });
            if let Some(c) = st.conns.get_mut(&conn) {
                c.defined = true;
            }
            // Measured: a define while streaming does not deliver until StartStream.
            st.streaming = false;
            (OK, format!("-StreamId {id} -SampleTime {}", fmt_ms(def.sample_ms)), Some(signal))
        }
        _ => (FAIL, format!("ERROR: unknown property {prop}"), None),
    }
}

fn fmt_ms(v: f64) -> String {
    let s = format!("{v:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn tick_loop(state: Arc<Mutex<State>>, running: Arc<AtomicBool>) {
    let start = Instant::now();
    let mut next_aya = Instant::now();
    let mut ticks: u64 = 0;
    while running.load(Ordering::SeqCst) {
        let tick = lock(&state).behaviour.tick;
        ticks += 1;
        let due = start + tick * ticks.min(u32::MAX as u64) as u32;
        let now = Instant::now();
        if due > now {
            std::thread::sleep(due - now);
        }
        let mut st = lock(&state);
        st.tick_count += 1;
        let step = st.behaviour.stamp_step;
        st.clock_ms = st.clock_ms.wrapping_add(step);

        // A network stall that has cleared: what was kept back goes out, in order.
        if !st.behaviour.hold_delivery {
            let waiting: Vec<usize> = st.conns.values().filter(|c| !c.held.is_empty()).map(|c| c.id).collect();
            for id in waiting {
                send_to(&mut st, id, &[]);
            }
        }

        // Broken connections the controller now notices.
        let now = Instant::now();
        let expired: Vec<usize> = st.conns.values().filter(|c| c.zombie_until.is_some_and(|t| now >= t)).map(|c| c.id).collect();
        for id in expired {
            if let Some(c) = st.conns.get_mut(&id) {
                c.zombie_until = None;
            }
            close_conn(&mut st, id);
        }

        // Keepalives, and the timeout for a client that never answers.
        if Instant::now() >= next_aya {
            next_aya = Instant::now() + st.behaviour.aya_interval;
            let frame = wire::encode_frame(0, service::AYA, cause::AYA, 4000, 16000, &[]);
            broadcast(&mut st, &frame);
            let timeout = st.behaviour.aya_timeout;
            let dead: Vec<usize> = st.conns.values().filter(|c| c.last_aya_answer.elapsed() > timeout).map(|c| c.id).collect();
            for id in dead {
                close_conn(&mut st, id);
            }
        }

        if !st.streaming || st.behaviour.mute_all {
            continue;
        }
        let Some(to) = tenant(&st) else { continue };
        let t = if st.behaviour.wrap32 { st.clock_ms & 0xFFFF_FFFF } else { st.clock_ms };
        let tick_no = st.tick_count;
        let mut records = Vec::new();
        let mut texts_sent = Vec::new();
        for s in st.streams.values() {
            if st.behaviour.mute.contains(&s.signal) {
                continue;
            }
            // 24 ms signals every sixth tick.
            if s.sample_ms > 10.0 && !tick_no.is_multiple_of(6) {
                continue;
            }
            let Some(def) = st.behaviour.signals.get(&s.signal) else { continue };
            let at: At<'_> = (t, s.unit.as_str(), s.axis0 + 1);
            let (kind, values) = match &def.source {
                SignalSource::Float(f) => (ValueKind::Float, RecordValues::Float(vec![f(at)])),
                SignalSource::Int(f) => (ValueKind::Int, RecordValues::Int(vec![f(at)])),
                SignalSource::Text(f) => {
                    let v = f(at);
                    if st.text_sent.get(&s.id) == Some(&v) {
                        continue;
                    }
                    texts_sent.push((s.id, v.clone()));
                    (ValueKind::String, RecordValues::String(vec![v]))
                }
                SignalSource::Silent => continue,
            };
            records.push(Record { stream: s.id, kind, stamps: vec![t], values });
        }
        for (id, v) in texts_sent {
            st.text_sent.insert(id, v);
        }
        // Integer records first, as on the VC.
        records.sort_by_key(|r| (r.kind != ValueKind::Int, r.stream));
        if records.is_empty() {
            continue;
        }
        st.samples_sent += records.len() as u64;
        let rad = sample::encode_rad(SUBSCRIPTION_ID, 0x01DD_4CF3_AA5D_7B80, &records, Some(0x04));
        let frame = wire::encode_frame(0, service::EVENT, cause::EVENT, 0, 0, &[RadOut { kind: rad_kind::REPLY, format: rad_format::EVENT, data: &rad }]);
        send_to(&mut st, to, &frame);
    }
}
