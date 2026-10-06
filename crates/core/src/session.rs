use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::io::{Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use crate::discovery::{self, VcFinder};
use crate::log::LogBook;
use crate::reply::{self, Announce, Reply};
use crate::request::{self, Axis, Command, Define};
use crate::sample::{self, Defect, RecordValues, ValueKind};
use crate::store::{Channel, ChannelKey, Store};
use crate::timeline::{ClockEvent, Timeline};
use crate::wire::{self, service, Frame, FrameStatus};

pub const MAX_CHANNELS: usize = 12;
pub const ROBAPI_PORT: u16 = 5515;
const MAX_BUFFER: usize = 2 * wire::MAX_FRAME as usize;
const MAX_QUEUED_FRAMES: usize = 8192;
const READER_WAKE: Duration = Duration::from_millis(100);
const LADDER: [u64; 5] = [1, 2, 5, 15, 30];
const OWN_CHANGE_MARGIN_MS: u64 = 1_000;
const LEFTOVER_MARGIN_MS: u64 = 200;
const OWN_START_SLACK: Duration = Duration::from_secs(1);
const FIRST_SAMPLE_WALL_MARGIN: Duration = Duration::from_millis(1500);
pub const MARK_ROOM: usize = 256;
pub const SAMPLE_MS_USED: std::ops::RangeInclusive<f64> = 1.0..=1000.0;
const SAMPLE_MS_TAKEN: f64 = 4.032;
const SAMPLE_MS_LENIENT: f64 = 24.192;

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Target {
    pub host: String,
    pub port: u16,
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.host.contains(':') { write!(f, "[{}]:{}", self.host, self.port) } else { write!(f, "{}:{}", self.host, self.port) }
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    pub connect_timeout: Duration,
    pub handshake_timeout: Duration,
    pub reply_timeout: Duration,
    pub stale_after: Duration,
    pub stall_after: Duration,
    pub probe_after: Duration,
    pub teardown_wait: Duration,
    pub ladder: Vec<Duration>,
    pub ask: AskPolicy,
    pub held_wait: Duration,
    pub held_poll: Duration,
    pub recovery_retry: Duration,
    pub recovery_handshake: Duration,
    pub recovery_for: Duration,
    pub vc_pause_patience: Duration,
    pub find_vc: VcFinder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskPolicy {
    Remote,
    Always,
    Never,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            connect_timeout: Duration::from_secs(3),
            handshake_timeout: Duration::from_secs(3),
            reply_timeout: Duration::from_secs(3),
            stale_after: Duration::from_millis(1000),
            stall_after: Duration::from_secs(3),
            probe_after: Duration::from_millis(300),
            teardown_wait: Duration::from_millis(1500),
            ladder: LADDER.iter().map(|&s| Duration::from_secs(s)).collect(),
            ask: AskPolicy::Remote,
            held_wait: Duration::from_secs(60),
            held_poll: Duration::from_secs(2),
            recovery_retry: Duration::from_secs(1),
            recovery_handshake: Duration::from_secs(1),
            recovery_for: Duration::from_secs(60),
            vc_pause_patience: Duration::from_secs(120),
            find_vc: VcFinder::local(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    Idle,
    Connecting,
    Handshaking,
    AwaitingApproval,
    SettingUp,
    Streaming,
    Reconnecting { attempt: u32, retry_in: Duration },
    TearingDown,
    Stopped { reason: String },
}

impl Phase {
    pub fn is_connected(&self) -> bool {
        matches!(self, Phase::Handshaking | Phase::AwaitingApproval | Phase::SettingUp | Phase::Streaming | Phase::TearingDown)
    }
    pub fn is_active(&self) -> bool {
        !matches!(self, Phase::Idle | Phase::Stopped { .. })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ChannelState {
    Waiting,
    Defining,
    NoReply,
    Defined { stream: u32 },
    Refused { code: Option<i64>, text: String },
}

#[derive(Debug, Clone)]
pub struct ChannelStatus {
    pub key: ChannelKey,
    pub state: ChannelState,
    pub sample_ms: Option<f64>,
    pub kind: Option<ValueKind>,
    pub samples: u64,
    pub last_arrival: Option<Instant>,
    pub rate: f64,
    pub gaps: u64,
    pub stale: bool,
    pub stale_bound: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct OtherClient {
    pub address: String,
    pub attributes: Vec<(String, String)>,
    pub same_pc: bool,
    pub pendant: bool,
}

pub fn is_pendant_address(address: &str) -> bool {
    address.parse::<std::net::Ipv4Addr>().is_ok_and(|ip| matches!(ip.octets(), [192, 168, 126, _]))
}

#[derive(Debug, Clone, Default)]
pub struct Counters {
    pub frames: u64,
    pub bytes: u64,
    pub sample_frames: u64,
    pub samples: u64,
    pub ayas: u64,
    pub reconnects: u64,
    pub desyncs: u64,
    pub no_trailer: u64,
    pub foreign_records: u64,
    pub foreign_subscription: u64,
    pub sample_services: BTreeMap<u8, u64>,
    pub marker_offsets: BTreeMap<usize, u64>,
    pub unexpected_frames: u64,
    pub liveness_checks: u64,
    pub defects: BTreeMap<String, u64>,
    pub clock_resets: u64,
    pub dropped_to_taps: u64,
    pub dropped_events_to_taps: u64,
}

#[derive(Debug, Clone)]
pub struct Status {
    pub phase: Phase,
    pub target: Option<Target>,
    pub peer: Option<SocketAddr>,
    pub local: Option<SocketAddr>,
    pub loopback: bool,
    pub announce: Option<Announce>,
    pub others: Vec<OtherClient>,
    pub subscription: Option<u32>,
    pub channels: Vec<ChannelStatus>,
    pub counters: Counters,
    pub connected_since: Option<Instant>,
    pub streaming_since: Option<Instant>,
    pub timeline: Timeline,
    pub advice: Option<String>,
    pub moved: Option<(Target, Target)>,
    pub moves: u64,
}

impl Default for Status {
    fn default() -> Status {
        Status {
            phase: Phase::Idle,
            target: None,
            peer: None,
            local: None,
            loopback: false,
            announce: None,
            others: Vec::new(),
            subscription: None,
            channels: Vec::new(),
            counters: Counters::default(),
            connected_since: None,
            streaming_since: None,
            timeline: Timeline::new(),
            advice: None,
            moved: None,
            moves: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SampleBatch {
    pub key: ChannelKey,
    pub kind: ValueKind,
    pub raw_ms: Vec<u64>,
    pub timeline_ms: Vec<i64>,
    pub values: BatchValues,
    pub arrived: SystemTime,
}

#[derive(Debug, Clone)]
pub enum BatchValues {
    Number(Vec<f64>),
    Text(Vec<String>),
}

#[derive(Debug, Clone)]
pub enum Mark {
    Connected { target: Target, system_id: Option<String> },
    Defined { key: ChannelKey, stream: u32, sample_ms: f64 },
    Lost { reason: String },
    Disconnected,
    ClockReset { from_raw: u64, to_raw: u64 },
}

#[derive(Debug, Clone)]
pub enum TapEvent {
    Samples(SampleBatch),
    Mark { wall: SystemTime, mark: Mark },
}

pub struct Tap {
    rx: Receiver<TapEvent>,
    queued: Arc<AtomicUsize>,
    pub dropped: Arc<AtomicU64>,
    pub dropped_events: Arc<AtomicU64>,
}

impl Tap {
    pub fn recv_timeout(&self, wait: Duration) -> Result<TapEvent, RecvTimeoutError> {
        let r = self.rx.recv_timeout(wait);
        if r.is_ok() {
            self.taken();
        }
        r
    }

    pub fn try_recv(&self) -> Result<TapEvent, mpsc::TryRecvError> {
        let r = self.rx.try_recv();
        if r.is_ok() {
            self.taken();
        }
        r
    }

    fn taken(&self) {
        let _ = self.queued.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1));
    }

    #[cfg(feature = "fake")]
    #[doc(hidden)]
    pub fn by_hand(capacity: usize) -> (SyncSender<TapEvent>, Tap) {
        let (tap, slot) = new_tap(capacity);
        (slot.tx, tap)
    }
}

struct TapSlot {
    tx: SyncSender<TapEvent>,
    queued: Arc<AtomicUsize>,
    samples_room: usize,
    dropped: Arc<AtomicU64>,
    dropped_events: Arc<AtomicU64>,
}

fn new_tap(capacity: usize) -> (Tap, TapSlot) {
    let samples_room = capacity.max(1);
    let (tx, rx) = mpsc::sync_channel(samples_room + MARK_ROOM);
    let queued = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicU64::new(0));
    let dropped_events = Arc::new(AtomicU64::new(0));
    let tap = Tap { rx, queued: queued.clone(), dropped: dropped.clone(), dropped_events: dropped_events.clone() };
    (tap, TapSlot { tx, queued, samples_room, dropped, dropped_events })
}

enum Request {
    Connect(Target),
    Disconnect,
    SetChannels(Vec<ChannelKey>, BTreeSet<ChannelKey>),
    Answer(bool, Vec<String>),
    ResetInfoStream,
    Shutdown,
    #[cfg(feature = "fake")]
    Crash,
}

enum Event {
    Request(Request),
    Frame { generation: u64, bytes: Vec<u8>, at: Instant },
    Closed { generation: u64, why: String, desync: bool },
}

#[derive(Default)]
struct Emergency {
    stream: Option<TcpStream>,
    host: String,
    own: Vec<u32>,
    next_txn: u16,
    opened: bool,
    tenant: bool,
}

pub struct Session {
    tx: Sender<Event>,
    status: Arc<Mutex<Status>>,
    log: Arc<LogBook>,
    store: Arc<Store>,
    taps: Arc<Mutex<Vec<TapSlot>>>,
    done: Receiver<()>,
    thread: Option<JoinHandle<()>>,
}

impl Session {
    pub fn spawn(options: Options, log: Arc<LogBook>, store: Arc<Store>, notify: Arc<dyn Fn() + Send + Sync>) -> Session {
        let (tx, rx) = mpsc::channel();
        let status = Arc::new(Mutex::new(Status::default()));
        let taps: Arc<Mutex<Vec<TapSlot>>> = Arc::new(Mutex::new(Vec::new()));
        let (done_tx, done) = mpsc::channel();
        let emergency = Arc::new(Mutex::new(Emergency::default()));
        let worker = Worker::new(options, rx, tx.clone(), status.clone(), log.clone(), store.clone(), taps.clone(), emergency.clone(), notify);
        let log2 = log.clone();
        let status2 = status.clone();
        let thread = std::thread::Builder::new()
            .name("infostream".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    let mut w = worker;
                    w.run();
                }));
                if let Err(p) = result {
                    let what = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_else(|| "unknown".into());
                    emergency_teardown(&emergency);
                    log2.error(format!("Internal error in the session worker ({what}); the session was torn down."));
                    let mut s = status2.lock().unwrap_or_else(|e| e.into_inner());
                    s.phase = Phase::Stopped { reason: format!("internal error: {what}") };
                }
                let _ = done_tx.send(());
            })
            .expect("the session worker thread could not be started");
        Session { tx, status, log, store, taps, done, thread: Some(thread) }
    }

    fn send(&self, r: Request) {
        let _ = self.tx.send(Event::Request(r));
    }

    pub fn connect(&self, target: Target) {
        self.send(Request::Connect(target))
    }
    pub fn disconnect(&self) {
        self.send(Request::Disconnect)
    }
    pub fn set_channels(&self, keys: Vec<ChannelKey>) {
        self.send(Request::SetChannels(keys, BTreeSet::new()))
    }
    pub fn set_channels_expecting_text(&self, keys: Vec<ChannelKey>, text: impl IntoIterator<Item = ChannelKey>) {
        self.send(Request::SetChannels(keys, text.into_iter().collect()))
    }
    pub fn answer(&self, take_infostream: bool, shown: &[OtherClient]) {
        self.send(Request::Answer(take_infostream, shown.iter().map(|o| o.address.clone()).collect()))
    }
    pub fn reset_infostream(&self) {
        self.send(Request::ResetInfoStream)
    }

    #[cfg(feature = "fake")]
    #[doc(hidden)]
    pub fn crash_for_test(&self) {
        self.send(Request::Crash)
    }

    pub fn status(&self) -> MutexGuard<'_, Status> {
        self.status.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn is_running(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }
    pub fn log(&self) -> &Arc<LogBook> {
        &self.log
    }
    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    pub fn tap(&self, capacity: usize) -> Tap {
        let (tap, slot) = new_tap(capacity);
        self.taps.lock().unwrap_or_else(|e| e.into_inner()).push(slot);
        tap
    }

    pub fn shutdown(mut self, wait: Duration) -> bool {
        self.stop(wait)
    }

    fn stop(&mut self, wait: Duration) -> bool {
        self.send(Request::Shutdown);
        let finished = self.done.recv_timeout(wait).is_ok();
        if finished
            && let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        finished
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.thread.is_some() {
            self.stop(Duration::from_secs(4));
        }
    }
}

fn emergency_teardown(e: &Mutex<Emergency>) {
    let mut g = e.lock().unwrap_or_else(|p| p.into_inner());
    let host = g.host.clone();
    let own = std::mem::take(&mut g.own);
    let (opened, tenant) = (g.opened, g.tenant);
    let mut txn = g.next_txn;
    if let Some(mut s) = g.stream.take() {
        if opened {
            let _ = s.set_write_timeout(Some(Duration::from_millis(300)));
            let mut next = || {
                txn = txn.wrapping_add(1).max(1);
                txn
            };
            if tenant && !own.is_empty() {
                let _ = s.write_all(&Command::StopStream.frame(next(), &host));
            }
            for &id in &own {
                let _ = s.write_all(&Command::Undefine(id).frame(next(), &host));
            }
            if !tenant && !own.is_empty() {
                let _ = s.write_all(&Command::StartStream.frame(next(), &host));
            }
            let _ = s.write_all(&Command::StreamDisconnect.frame(next(), &host));
            std::thread::sleep(Duration::from_millis(200));
        }
        let _ = s.shutdown(Shutdown::Both);
    }
}

struct Conn {
    generation: u64,
    stream: TcpStream,
    peer: SocketAddr,
    local: SocketAddr,
    host: String,
    reader: Option<JoinHandle<()>>,
    queued: Arc<AtomicUsize>,
    closed: Arc<AtomicBool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Hello,
    Approval,
    Preamble,
    Defining,
    Streaming,
    TearingDown,
}

#[derive(Debug, Clone)]
enum Pending {
    Preamble(&'static str),
    Subscribe,
    Define { key: ChannelKey, sent: Instant, reported: bool, void: bool },
    Undefine { stream: u32 },
    Start,
    Stop,
    UndefineAll,
    Disconnect,
    Probe { sent: Instant, samples: u64 },
}

struct Chan {
    key: ChannelKey,
    slot: u8,
    state: ChannelState,
    sample_ms: Option<f64>,
    kind: Option<ValueKind>,
    samples: u64,
    session_samples: u64,
    last_arrival: Option<Instant>,
    defined_at: Option<Instant>,
    last_raw: Option<u64>,
    rate: f64,
    rate_mark: (Instant, u64),
    gaps: u64,
    stale_reported: bool,
    store: Option<Arc<Channel>>,
    expect_text: bool,
}

impl Chan {
    fn new(key: ChannelKey, slot: u8) -> Chan {
        Chan {
            key,
            slot,
            state: ChannelState::Waiting,
            sample_ms: None,
            kind: None,
            samples: 0,
            session_samples: 0,
            last_arrival: None,
            defined_at: None,
            last_raw: None,
            rate: 0.0,
            rate_mark: (Instant::now(), 0),
            gaps: 0,
            stale_reported: false,
            store: None,
            expect_text: false,
        }
    }
    fn stream(&self) -> Option<u32> {
        match self.state {
            ChannelState::Defined { stream } => Some(stream),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum After {
    Idle,
    Exit,
    Stopped(String),
    Retry(String),
}

struct Worker {
    opt: Options,
    rx: Receiver<Event>,
    tx: Sender<Event>,
    shared: Arc<Mutex<Status>>,
    log: Arc<LogBook>,
    store: Arc<Store>,
    taps: Arc<Mutex<Vec<TapSlot>>>,
    emergency: Arc<Mutex<Emergency>>,
    notify: Arc<dyn Fn() + Send + Sync>,

    phase: Phase,
    target: Option<Target>,
    conn: Option<Conn>,
    generation: u64,
    stage: Stage,
    stage_deadline: Option<Instant>,
    hello_txn: u16,
    txn: u16,
    pending: HashMap<u16, Pending>,
    sent_at: HashMap<u16, Instant>,
    chans: Vec<Chan>,
    by_stream: HashMap<u32, usize>,
    subscription: Option<u32>,
    started: bool,
    restart_needed: bool,
    last_start: Option<Instant>,
    told_no_samples: bool,
    advice: Option<String>,
    newest_raw: Option<u64>,
    newest_at: Option<Instant>,
    first_raw: Option<u64>,
    known_foreign: BTreeSet<u32>,
    retiring: BTreeSet<u32>,
    restart_owed_at: Option<Instant>,
    last_frame_at: Option<Instant>,
    auto_reconnect: bool,
    delivered_before: bool,
    baseline: Option<Vec<reply::ClientEntry>>,
    candidate: Option<Vec<reply::ClientEntry>>,
    lost_at: Option<Instant>,
    held_since: Option<Instant>,
    recovering_since: Option<Instant>,
    told_recovery_over: bool,
    hello_wait: Duration,
    leaver_retry: bool,
    leaver_retry_next: bool,
    system_id: Option<String>,
    moved: Option<(Target, Target)>,
    moves: u64,
    followed_restart: bool,
    vc_hold_retried: bool,
    told_other_vc: bool,
    told_unanswered: BTreeSet<&'static str>,
    vc_paused_since: Option<Instant>,
    last_probe_at: Option<Instant>,
    own_change_at: Option<u64>,
    own_change_sent: Option<Instant>,
    deferred: VecDeque<Request>,
    had_samples: bool,
    last_any_sample: Option<Instant>,
    announce: Option<Announce>,
    others: Vec<OtherClient>,
    approved: Option<BTreeSet<String>>,
    established: bool,
    rung: usize,
    retry_at: Option<Instant>,
    after_teardown: After,
    teardown_deadline: Option<Instant>,
    timeline: Timeline,
    counters: Counters,
    connected_since: Option<Instant>,
    streaming_since: Option<Instant>,
    dirty: bool,
    last_publish: Instant,
    exit: bool,
    records: Vec<sample::Record>,
    defects: Vec<Defect>,
    defect_logged: BTreeSet<String>,
}

impl Worker {
    #[allow(clippy::too_many_arguments)]
    fn new(
        opt: Options,
        rx: Receiver<Event>,
        tx: Sender<Event>,
        shared: Arc<Mutex<Status>>,
        log: Arc<LogBook>,
        store: Arc<Store>,
        taps: Arc<Mutex<Vec<TapSlot>>>,
        emergency: Arc<Mutex<Emergency>>,
        notify: Arc<dyn Fn() + Send + Sync>,
    ) -> Worker {
        Worker {
            opt,
            rx,
            tx,
            shared,
            log,
            store,
            taps,
            emergency,
            notify,
            phase: Phase::Idle,
            target: None,
            conn: None,
            generation: 0,
            stage: Stage::Hello,
            stage_deadline: None,
            hello_txn: 0,
            txn: 0,
            pending: HashMap::new(),
            sent_at: HashMap::new(),
            chans: Vec::new(),
            by_stream: HashMap::new(),
            subscription: None,
            started: false,
            restart_needed: false,
            last_start: None,
            told_no_samples: false,
            advice: None,
            newest_raw: None,
            newest_at: None,
            first_raw: None,
            known_foreign: BTreeSet::new(),
            retiring: BTreeSet::new(),
            restart_owed_at: None,
            last_frame_at: None,
            auto_reconnect: false,
            delivered_before: false,
            baseline: None,
            candidate: None,
            lost_at: None,
            held_since: None,
            recovering_since: None,
            told_recovery_over: false,
            hello_wait: Duration::ZERO,
            leaver_retry: false,
            leaver_retry_next: false,
            system_id: None,
            moved: None,
            moves: 0,
            followed_restart: false,
            vc_hold_retried: false,
            told_other_vc: false,
            told_unanswered: BTreeSet::new(),
            vc_paused_since: None,
            last_probe_at: None,
            own_change_at: None,
            own_change_sent: None,
            deferred: VecDeque::new(),
            had_samples: false,
            last_any_sample: None,
            announce: None,
            others: Vec::new(),
            approved: None,
            established: false,
            rung: 0,
            retry_at: None,
            after_teardown: After::Idle,
            teardown_deadline: None,
            timeline: Timeline::new(),
            counters: Counters::default(),
            connected_since: None,
            streaming_since: None,
            dirty: true,
            last_publish: Instant::now(),
            exit: false,
            records: Vec::new(),
            defects: Vec::new(),
            defect_logged: BTreeSet::new(),
        }
    }

    fn run(&mut self) {
        while !self.exit {
            let wait = self.next_wake();
            match self.rx.recv_timeout(wait) {
                Ok(ev) => {
                    self.handle(ev);
                    for _ in 0..256 {
                        match self.rx.try_recv() {
                            Ok(ev) => self.handle(ev),
                            Err(_) => break,
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    self.begin_teardown(After::Exit);
                    self.finish_teardown_now();
                }
            }
            self.tick();
            self.publish(false);
        }
        self.drop_conn();
        self.publish(true);
    }

    fn next_wake(&self) -> Duration {
        Duration::from_millis(if self.phase.is_active() { 20 } else { 200 })
    }

    fn handle(&mut self, ev: Event) {
        match ev {
            Event::Request(r) => {
                self.request(r);
                while !self.exit {
                    let Some(r) = self.deferred.pop_front() else { break };
                    self.request(r);
                }
            }
            Event::Frame { generation, bytes, at } => self.frame_event(generation, &bytes, at),
            Event::Closed { generation, why, desync } => {
                if self.conn.as_ref().is_some_and(|c| c.generation == generation) {
                    if desync {
                        self.counters.desyncs += 1;
                    }
                    self.lost(&why, desync);
                }
            }
        }
    }

    fn frame_event(&mut self, generation: u64, bytes: &[u8], at: Instant) {
        let Some(c) = &self.conn else { return };
        if c.generation != generation {
            return;
        }
        c.queued.fetch_sub(1, Ordering::Relaxed);
        self.frame(bytes, at);
    }

    fn request(&mut self, r: Request) {
        match r {
            Request::Connect(t) => {
                if self.conn.is_some() {
                    self.log.info(format!("Reconnecting to {t}."));
                    self.emit_mark(Mark::Disconnected);
                    self.begin_teardown(After::Idle);
                    self.finish_teardown_now();
                    if self.exit {
                        return;
                    }
                }
                if self.target.as_ref().is_some_and(|old| *old != t) {
                    self.fresh_start(&t);
                    self.system_id = None;
                }
                self.target = Some(t);
                self.moved = None;
                self.established = false;
                self.approved = None;
                self.rung = 0;
                self.retry_at = None;
                self.auto_reconnect = false;
                self.delivered_before = false;
                self.baseline = None;
                self.candidate = None;
                self.lost_at = None;
                self.held_since = None;
                self.recovering_since = None;
                self.told_recovery_over = false;
                self.leaver_retry = false;
                self.leaver_retry_next = false;
                for c in &mut self.chans {
                    if matches!(c.state, ChannelState::Refused { .. } | ChannelState::NoReply) {
                        c.state = ChannelState::Waiting;
                    }
                }
                self.open();
            }
            Request::Disconnect => {
                self.retry_at = None;
                self.held_since = None;
                self.leaver_retry_next = false;
                if self.conn.is_some() {
                    self.emit_mark(Mark::Disconnected);
                    self.begin_teardown(After::Idle);
                } else {
                    self.set_phase(Phase::Idle);
                }
            }
            Request::SetChannels(keys, text) => {
                self.set_channels(keys);
                for c in &mut self.chans {
                    c.expect_text = text.contains(&c.key);
                }
            }
            Request::Answer(take, shown) => {
                if self.stage != Stage::Approval || self.conn.is_none() {
                    return;
                }
                let asked: BTreeSet<&str> = self.others.iter().map(|o| o.address.as_str()).collect();
                let answered: BTreeSet<&str> = shown.iter().map(String::as_str).collect();
                if take && asked != answered {
                    self.log.warn("An answer given for a different list of clients was ignored; the question stands.");
                    self.dirty = true;
                    return;
                }
                if take {
                    self.approved = Some(self.others.iter().map(|o| o.address.clone()).collect());
                    self.log.warn(format!(
                        "Taking InfoStream with {} other RobAPI client(s) connected: {}.",
                        self.others.len(),
                        self.others.iter().map(|o| o.address.as_str()).collect::<Vec<_>>().join(", ")
                    ));
                    self.begin_preamble();
                } else {
                    self.log.info("Not taking InfoStream; disconnected without defining anything.");
                    self.drop_conn();
                    self.set_phase(Phase::Stopped { reason: "Not connected: you chose not to take InfoStream from the other client(s).".into() });
                }
            }
            Request::ResetInfoStream => {
                if self.conn.is_none() || !matches!(self.stage, Stage::Defining | Stage::Streaming) {
                    self.log.warn("Reset InfoStream needs a live session; nothing was sent.");
                    return;
                }
                self.log.warn("Reset InfoStream: StreamUndefineAll sent. Every client's streams on this controller are removed.");
                self.send_cmd(Command::UndefineAll, Pending::UndefineAll);
                for p in self.pending.values_mut() {
                    if let Pending::Define { void, .. } = p {
                        *void = true;
                    }
                }
                for c in &mut self.chans {
                    c.state = ChannelState::Waiting;
                }
                self.retiring.extend(self.by_stream.keys().copied());
                self.by_stream.clear();
                self.emergency.lock().unwrap_or_else(|e| e.into_inner()).own.clear();
                self.started = false;
                self.restart_needed = false;
                self.stage = Stage::Defining;
                self.stage_deadline = None;
                self.define_waiting();
                self.check_defining_done();
            }
            Request::Shutdown => {
                if self.conn.is_some() {
                    self.begin_teardown(After::Exit);
                } else {
                    self.exit = true;
                }
            }
            #[cfg(feature = "fake")]
            Request::Crash => panic!("crash requested by a test"),
        }
    }

    fn set_channels(&mut self, keys: Vec<ChannelKey>) {
        let mut wanted: Vec<ChannelKey> = Vec::new();
        for k in keys {
            if !wanted.contains(&k) {
                wanted.push(k);
            }
        }
        if wanted.len() > MAX_CHANNELS {
            self.log.warn(format!("Only {MAX_CHANNELS} channels at once; {} ignored.", wanted.len() - MAX_CHANNELS));
            wanted.truncate(MAX_CHANNELS);
        }
        let mut i = 0;
        while i < self.chans.len() {
            if wanted.contains(&self.chans[i].key) {
                i += 1;
                continue;
            }
            let c = self.chans.remove(i);
            if let Some(s) = c.stream() {
                self.by_stream.remove(&s);
                self.emergency.lock().unwrap_or_else(|e| e.into_inner()).own.retain(|&x| x != s);
                if self.conn.is_some() && matches!(self.stage, Stage::Defining | Stage::Streaming) {
                    self.undefine_own(s);
                }
            }
        }
        for k in wanted.iter() {
            if !self.chans.iter().any(|c| &c.key == k) {
                let used: BTreeSet<u8> = self.chans.iter().map(|c| c.slot).collect();
                let slot = (0..MAX_CHANNELS as u8).find(|s| !used.contains(s)).unwrap_or(0);
                self.chans.push(Chan::new(k.clone(), slot));
            }
        }
        self.chans.sort_by_key(|c| wanted.iter().position(|k| k == &c.key).unwrap_or(usize::MAX));
        self.reindex();
        if self.conn.is_some() && matches!(self.stage, Stage::Defining | Stage::Streaming) {
            self.define_waiting();
            self.check_defining_done();
            self.maybe_restart();
        }
        self.dirty = true;
    }

    fn reindex(&mut self) {
        self.by_stream = self.chans.iter().enumerate().filter_map(|(i, c)| c.stream().map(|s| (s, i))).collect();
    }

    fn fresh_start(&mut self, t: &Target) {
        self.store.clear();
        self.timeline = Timeline::new();
        self.counters = Counters::default();
        let now = Instant::now();
        for c in &mut self.chans {
            let (key, slot, state, text) = (c.key.clone(), c.slot, std::mem::replace(&mut c.state, ChannelState::Waiting), c.expect_text);
            *c = Chan::new(key, slot);
            c.state = state;
            c.expect_text = text;
            c.rate_mark = (now, 0);
        }
        self.log.info(format!("Connecting to a different controller ({t}): the charts start afresh."));
        self.dirty = true;
    }

    fn next_txn(&mut self) -> u16 {
        self.txn = self.txn.wrapping_add(1);
        if self.txn == 0 {
            self.txn = 1;
        }
        while self.pending.contains_key(&self.txn) || self.txn == self.hello_txn {
            self.txn = self.txn.wrapping_add(1).max(1);
        }
        self.txn
    }

    fn set_phase(&mut self, p: Phase) {
        if self.phase != p {
            self.phase = p;
            self.dirty = true;
            self.publish(true);
        }
    }

    fn open(&mut self) {
        self.open_once(true);
    }

    fn open_once(&mut self, may_follow: bool) {
        let Some(target) = self.target.clone() else { return };
        self.set_phase(Phase::Connecting);
        let addrs: Vec<SocketAddr> = match (target.host.as_str(), target.port).to_socket_addrs() {
            Ok(a) => a.collect(),
            Err(e) => {
                self.connect_failed(&format!("cannot resolve \"{}\": {e}", target.host));
                return;
            }
        };
        let mut last_err = String::from("no address");
        let mut refused = false;
        let mut stream = None;
        for a in &addrs {
            match TcpStream::connect_timeout(a, self.opt.connect_timeout) {
                Ok(s) => {
                    stream = Some((s, *a));
                    break;
                }
                Err(e) => {
                    refused = e.kind() == std::io::ErrorKind::ConnectionRefused;
                    last_err = describe_io(&e);
                }
            }
        }
        let Some((stream, peer)) = stream else {
            if may_follow && let Some(port) = self.restarted_port(&target, &addrs) {
                let to = Target { host: target.host.clone(), port };
                self.log.info(format!("The controller now answers on port {port}, not {}: a virtual controller takes a new port at every start. Connecting there.", target.port));
                self.moved = Some((target, to.clone()));
                self.moves += 1;
                self.target = Some(to);
                self.followed_restart = true;
                self.open_once(false);
                return;
            }
            if refused && !addrs.iter().any(|a| self.vc_at(a.ip())) {
                self.drop_conn();
                if self.retry_while_recovering(&format!("{target} refused the connection")) {
                    return;
                }
            }
            let hint = if !self.established && discovery::on_this_pc(&addrs) {
                "; a virtual controller takes a new port at every start, and the list of local controllers has its current one"
            } else {
                ""
            };
            self.connect_failed(&format!("cannot connect to {target}: {last_err}{hint}"));
            return;
        };
        let _ = stream.set_nodelay(true);
        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
        let local = stream.local_addr().unwrap_or(peer);
        let Ok(reader_stream) = stream.try_clone() else {
            self.connect_failed("cannot start the socket reader");
            return;
        };
        self.generation += 1;
        let generation = self.generation;
        let queued = Arc::new(AtomicUsize::new(0));
        let closed = Arc::new(AtomicBool::new(false));
        let reader = {
            let tx = self.tx.clone();
            let (queued, closed) = (queued.clone(), closed.clone());
            std::thread::Builder::new().name("infostream-reader".into()).spawn(move || reader_loop(reader_stream, generation, tx, queued, closed)).ok()
        };
        if reader.is_none() {
            self.connect_failed("cannot start the socket reader thread");
            return;
        }
        let host = peer.ip().to_string();
        self.conn = Some(Conn { generation, stream, peer, local, host: host.clone(), reader, queued, closed });
        {
            let mut e = self.emergency.lock().unwrap_or_else(|p| p.into_inner());
            e.stream = self.conn.as_ref().and_then(|c| c.stream.try_clone().ok());
            e.host = host;
            e.own.clear();
            e.opened = false;
            e.tenant = false;
        }
        self.pending.clear();
        self.sent_at.clear();
        self.by_stream.clear();
        for c in &mut self.chans {
            if !matches!(c.state, ChannelState::Refused { .. }) {
                c.state = ChannelState::Waiting;
            }
            c.session_samples = 0;
        }
        self.subscription = None;
        self.started = false;
        self.restart_needed = false;
        self.last_start = None;
        self.told_no_samples = false;
        self.told_other_vc = false;
        self.advice = None;
        self.newest_raw = None;
        self.newest_at = None;
        self.first_raw = None;
        self.known_foreign.clear();
        self.retiring.clear();
        self.restart_owed_at = None;
        self.last_frame_at = None;
        self.own_change_at = None;
        self.own_change_sent = None;
        self.had_samples = false;
        self.last_any_sample = None;
        self.announce = None;
        self.others.clear();
        self.candidate = None;
        self.leaver_retry = self.leaver_retry_next && self.auto_reconnect;
        self.told_unanswered.clear();
        self.vc_paused_since = None;
        self.last_probe_at = None;
        self.timeline.session_start();
        self.connected_since = Some(Instant::now());
        self.streaming_since = None;
        self.hello_txn = 0;
        self.hello_txn = self.next_txn();
        self.stage = Stage::Hello;
        self.hello_wait = if self.recovering() && !self.vc_at(peer.ip()) { self.opt.recovery_handshake } else { self.opt.handshake_timeout };
        self.stage_deadline = Some(Instant::now() + self.hello_wait);
        self.set_phase(Phase::Handshaking);
        let f = request::hello(self.hello_txn);
        self.write(&f);
    }

    fn connect_failed(&mut self, why: &str) {
        self.drop_conn();
        if self.established {
            self.schedule_reconnect(why);
        } else {
            self.log.error(format!("{}.", capitalize(why)));
            self.set_phase(Phase::Stopped { reason: capitalize(why) });
        }
    }

    fn restarted_port(&mut self, target: &Target, addrs: &[SocketAddr]) -> Option<u16> {
        let system = self.system_id.clone()?;
        if !discovery::on_this_pc(addrs) {
            return None;
        }
        let found = self.opt.find_vc.find(self.opt.handshake_timeout);
        let port = discovery::restarted_port(&system, target.port, &found);
        let elsewhere: Vec<&(u16, Option<String>)> = found.iter().filter(|(p, _)| *p != target.port).collect();
        if port.is_none() && !elsewhere.is_empty() && !self.told_other_vc {
            self.told_other_vc = true;
            let same: Vec<String> = elsewhere.iter().filter(|(_, id)| id.as_deref() == Some(system.as_str())).map(|(p, _)| p.to_string()).collect();
            if same.len() > 1 {
                self.log.warn(format!("The controller (system {system}) answers on more than one port ({}): not guessing which. Connect to one from the list.", same.join(", ")));
            } else if same.is_empty() {
                let others = elsewhere.iter().map(|(p, id)| format!("port {p}, system {}", id.as_deref().unwrap_or("unknown"))).collect::<Vec<_>>().join("; ");
                self.log.warn(format!("A virtual controller answers on this PC ({others}), but not the one connected to before (system {system}): not following it. Connect to it from the list if it is the one wanted."));
            }
        }
        port
    }

    fn vc_at(&self, ip: IpAddr) -> bool {
        !self.opt.vc_pause_patience.is_zero() && is_loopback(ip)
    }

    fn recovering(&self) -> bool {
        self.recovering_since.is_some_and(|since| since.elapsed() < self.opt.recovery_for)
    }

    fn retry_while_recovering(&mut self, what: &str) -> bool {
        if !(self.established && self.auto_reconnect) {
            return false;
        }
        if self.recovering_since.is_none() {
            self.recovering_since = Some(Instant::now());
            self.log.warn(format!(
                "{}: the controller can be reached again but is not answering yet. After a network fault a controller holds back every new connection until it has dropped the one that broke (measured on an IRC5). Trying a fresh connection every {} s, for up to {:.0} s.",
                capitalize(what),
                self.opt.recovery_retry.as_secs_f64(),
                self.opt.recovery_for.as_secs_f64()
            ));
        } else if !self.recovering() {
            if !self.told_recovery_over {
                self.told_recovery_over = true;
                self.log.warn(format!("The controller is still not answering, {:.0} s after it first did not: trying again on the usual schedule.", self.opt.recovery_for.as_secs_f64()));
            }
            return false;
        }
        self.retry_at = Some(Instant::now() + self.opt.recovery_retry);
        self.set_phase(Phase::Reconnecting { attempt: self.rung as u32 + 1, retry_in: self.opt.recovery_retry });
        true
    }

    fn schedule_reconnect(&mut self, why: &str) {
        let delay = self.opt.ladder.get(self.rung).or(self.opt.ladder.last()).copied().unwrap_or(Duration::from_secs(5));
        let attempt = self.rung as u32 + 1;
        if self.rung + 1 < self.opt.ladder.len() {
            self.rung += 1;
        }
        self.retry_at = Some(Instant::now() + delay);
        self.log.warn(format!("{}; retrying in {} s (attempt {attempt}).", capitalize(why), delay.as_secs_f64()));
        self.set_phase(Phase::Reconnecting { attempt, retry_in: delay });
    }

    fn drop_conn(&mut self) {
        if let Some(mut c) = self.conn.take() {
            c.closed.store(true, Ordering::SeqCst);
            let _ = c.stream.shutdown(Shutdown::Both);
            if let Some(r) = c.reader.take() {
                let _ = r.join();
            }
        }
        let mut e = self.emergency.lock().unwrap_or_else(|p| p.into_inner());
        e.stream = None;
        e.own.clear();
        drop(e);
        self.pending.clear();
        self.sent_at.clear();
        self.by_stream.clear();
        self.retiring.clear();
        for c in &mut self.chans {
            if !matches!(c.state, ChannelState::Refused { .. }) {
                c.state = ChannelState::Waiting;
            }
        }
        self.stage = Stage::Hello;
        self.stage_deadline = None;
        self.started = false;
        self.restart_needed = false;
        self.restart_owed_at = None;
        self.connected_since = None;
        self.streaming_since = None;
        self.dirty = true;
    }

    fn write(&mut self, bytes: &[u8]) -> bool {
        let Some(c) = self.conn.as_mut() else { return false };
        match c.stream.write_all(bytes) {
            Ok(()) => true,
            Err(e) => {
                let why = format!("sending to the controller failed: {}", describe_io(&e));
                let generation = c.generation;
                let _ = self.tx.send(Event::Closed { generation, why, desync: false });
                false
            }
        }
    }

    fn send_cmd(&mut self, cmd: Command, p: Pending) -> u16 {
        let txn = self.next_txn();
        let host = self.conn.as_ref().map(|c| c.host.clone()).unwrap_or_default();
        let f = cmd.frame(txn, &host);
        if matches!(cmd, Command::Define(_) | Command::Undefine(_) | Command::UndefineAll | Command::StartStream | Command::StopStream) {
            self.own_change_at = self.controller_now();
            self.own_change_sent = Some(Instant::now());
        }
        self.pending.insert(txn, p);
        self.sent_at.insert(txn, Instant::now());
        self.emergency.lock().unwrap_or_else(|e| e.into_inner()).next_txn = txn;
        self.write(&f);
        txn
    }

    fn own_change_patience(&self) -> Duration {
        self.opt.reply_timeout * 4
    }

    fn own_change_pending(&self) -> bool {
        let patience = self.own_change_patience();
        self.restart_needed
            || self.defines_in_flight()
            || self.pending.iter().any(|(txn, p)| matches!(p, Pending::Start | Pending::Undefine { .. } | Pending::UndefineAll) && self.sent_at.get(txn).is_some_and(|t| t.elapsed() < patience))
    }

    fn sweep_unanswered(&mut self, now: Instant) {
        let late: Vec<(u16, &'static str)> = self
            .pending
            .iter()
            .filter_map(|(txn, p)| {
                let patience = self.own_change_patience();
                let (limit, what) = match p {
                    Pending::Probe { .. } => (self.probe_patience(), "the liveness check"),
                    Pending::Start => (patience, "StartStream"),
                    Pending::Stop => (patience, "StopStream"),
                    Pending::Undefine { .. } => (patience, "StreamUndefine"),
                    Pending::UndefineAll => (patience, "StreamUndefineAll"),
                    _ => return None,
                };
                let sent = self.sent_at.get(txn)?;
                (now.duration_since(*sent) >= limit).then_some((*txn, what))
            })
            .collect();
        for (txn, what) in late {
            self.pending.remove(&txn);
            self.sent_at.remove(&txn);
            if self.told_unanswered.insert(what) {
                if what == "the liveness check" {
                    self.log.warn(format!(
                        "The controller did not answer the liveness check (its handshake, sent again when every stream fell silent) within {:.1} s. Another will be sent when the streams next fall silent; meanwhile a program taking InfoStream is noticed only by the {:.0} s stall.",
                        self.probe_patience().as_secs_f64(),
                        self.stall_bound().as_secs_f64()
                    ));
                } else {
                    self.log.warn(format!("The controller did not answer {what} within {:.1} s; carrying on without the answer.", self.own_change_patience().as_secs_f64()));
                }
            }
        }
    }

    fn lost(&mut self, why: &str, desync: bool) {
        let tearing = self.stage == Stage::TearingDown;
        let in_hello = self.stage == Stage::Hello;
        self.drop_conn();
        if tearing {
            self.after_teardown_done();
            return;
        }
        self.emit_mark(Mark::Lost { reason: why.to_string() });
        if self.had_samples {
            self.lost_at = Some(Instant::now());
        }
        if self.established {
            self.schedule_reconnect(&format!("connection lost ({why})"));
            return;
        }
        let t = self.target.as_ref().map(|t| t.to_string()).unwrap_or_default();
        let reason = if in_hello && desync {
            format!("{t} answered, but not like a controller's RobAPI port ({why}). Check the port number")
        } else if in_hello {
            format!("{t} closed the connection during the RobAPI handshake ({why})")
        } else {
            format!("Connection lost during setup: {why}")
        };
        self.log.error(format!("{reason}."));
        self.set_phase(Phase::Stopped { reason });
    }

    fn handshake_done(&mut self, frame: &Frame<'_>) {
        if let Some(since) = self.recovering_since.take() {
            self.told_recovery_over = false;
            self.log.info(format!("The controller answers again, {:.1} s after it first did not.", since.elapsed().as_secs_f64()));
        }
        let announce = Announce::from_rads(frame.rads(), frame.ctrl1());
        let (local, peer) = match &self.conn {
            Some(c) => (c.local, c.peer),
            None => return,
        };
        let local_ip = local.ip().to_string();
        let mut others: Vec<OtherClient> = Vec::new();
        let mut me_found = false;
        for c in &announce.clients {
            if !me_found && c.address == local_ip {
                me_found = true;
                continue;
            }
            others.push(OtherClient { address: c.address.clone(), attributes: c.attributes.clone(), same_pc: c.address == local_ip, pendant: is_pendant_address(&c.address) });
        }
        self.log.info(format!(
            "Connected to {} (system {}). RobAPI clients: {}.",
            self.target.as_ref().map(|t| t.to_string()).unwrap_or_default(),
            announce.system_id.as_deref().unwrap_or("unknown"),
            if announce.clients.is_empty() { "none listed".to_string() } else { announce.clients.iter().map(|c| c.address.as_str()).collect::<Vec<_>>().join(", ") }
        ));
        if !me_found && !announce.clients.is_empty() {
            self.log.warn(format!("This PC's address ({local_ip}) is not in the controller's client list; one of the listed clients is this program."));
        }
        self.emit_mark(Mark::Connected { target: self.target.clone().unwrap_or(Target { host: String::new(), port: 0 }), system_id: announce.system_id.clone() });
        let t = self.target.as_ref().map(|t| t.to_string()).unwrap_or_default();
        if let (Some(was), Some(now)) = (self.system_id.clone(), announce.system_id.clone())
            && was != now
        {
            if self.auto_reconnect {
                let reason = format!(
                    "Connected again to {t}, but it is a different controller (system {now}; before, {was}). Not streaming from it unasked: its values would continue the other controller's charts. Connect again to use it."
                );
                self.log.error(reason.clone());
                self.drop_conn();
                self.set_phase(Phase::Stopped { reason });
                return;
            }
            let t2 = self.target.clone().unwrap_or(Target { host: String::new(), port: 0 });
            self.fresh_start(&t2);
            self.log.warn(format!("{t} is a different controller from the one connected to before (system {now}; before, {was}): the charts start afresh."));
            self.approved = None;
            self.baseline = None;
        }
        if announce.system_id.is_some() {
            self.system_id = announce.system_id.clone();
        }
        let loopback = peer.ip().is_loopback();
        let ask = match self.opt.ask {
            AskPolicy::Remote => !loopback,
            AskPolicy::Always => true,
            AskPolicy::Never => false,
        };
        let clients = announce.clients.clone();
        self.announce = Some(announce);
        self.others = others;
        if self.auto_reconnect
            && let Some(base) = self.baseline.clone()
            && !self.reconnect_guard(&base, &clients, &local_ip, ask, &t)
        {
            return;
        }
        if !self.auto_reconnect {
            self.baseline = Some(clients.clone());
        }
        self.candidate = Some(clients);
        let unapproved: Vec<&OtherClient> = self.others.iter().filter(|o| !o.pendant && !self.approved.as_ref().is_some_and(|a| a.contains(&o.address))).collect();
        if ask && !unapproved.is_empty() {
            self.stage = Stage::Approval;
            self.stage_deadline = None;
            self.log.warn(format!(
                "Other RobAPI client(s) connected: {}. Waiting for your decision before taking InfoStream.",
                unapproved.iter().map(|o| o.address.as_str()).collect::<Vec<_>>().join(", ")
            ));
            self.set_phase(Phase::AwaitingApproval);
            return;
        }
        self.begin_preamble();
    }

    fn reconnect_guard(&mut self, base: &[reply::ClientEntry], now: &[reply::ClientEntry], local_ip: &str, ask: bool, t: &str) -> bool {
        let (mut joined, _) = client_changes(base, now);
        joined.retain(|a| !is_pendant_address(a));
        let own = joined.iter().filter(|a| a.as_str() == local_ip).count();
        let others: Vec<String> = joined.into_iter().filter(|a| a != local_ip).collect();
        let held_may_linger = self.lost_at.is_some() && self.held_since.is_none_or(|since| since.elapsed() < self.opt.held_wait);
        if others.is_empty() && own == 1 && held_may_linger {
            if self.held_since.is_none() {
                self.log.warn(format!(
                    "The controller still holds the connection that broke, and would send it every sample. It lets go once it hears from this PC again, which it can now, or at a timeout of its own. Waiting for that, up to {:.0} s, before setting anything up.",
                    self.opt.held_wait.as_secs_f64()
                ));
                self.held_since = Some(Instant::now());
            }
            self.drop_conn();
            self.retry_at = Some(Instant::now() + self.opt.held_poll);
            self.set_phase(Phase::Reconnecting { attempt: self.rung as u32 + 1, retry_in: self.opt.held_poll });
            return false;
        }
        if others.is_empty() && own == 0 {
            if self.held_since.take().is_some() {
                self.log.info("The controller has let go of the connection that broke; connecting afresh.");
                self.drop_conn();
                self.retry_at = Some(Instant::now());
                self.set_phase(Phase::Reconnecting { attempt: self.rung as u32 + 1, retry_in: Duration::ZERO });
                return false;
            }
            return true;
        }
        let lingering = others.is_empty() && own == 1 && self.lost_at.is_some();
        let mut names = others;
        if own > 0 {
            names.push(format!("another program on this PC ({local_ip})"));
        }
        let names = names.join(" and ");
        if ask {
            self.approved = None;
            if lingering {
                self.log.warn(format!(
                    "One more connection from this PC ({local_ip}) is listed than before the connection was lost, still {:.0} s after the controller could be reached again: another program here, or the controller still holding the connection that broke. Asking before taking InfoStream again: another program may be showing test signals now.",
                    self.opt.held_wait.as_secs_f64()
                ));
            } else {
                self.log.warn(format!("While the connection was down, {names} connected to the controller. Asking before taking InfoStream again: it may be showing test signals now."));
            }
            return true;
        }
        let reason = if lingering {
            format!(
                "Reconnected to {t}, but one more connection from this PC is listed than before, still {:.0} s after the controller could be reached again: another program here, or the controller still holding the connection that broke. Nothing was set up, so as not to take InfoStream from another program. Connect again when it is free.",
                self.opt.held_wait.as_secs_f64()
            )
        } else {
            format!("While the connection was down, {names} connected to {t} and is still connected. It may be showing test signals now, and taking InfoStream back would stop them, so nothing was set up. Connect again when it is free.")
        };
        self.log.error(reason.clone());
        self.drop_conn();
        self.set_phase(Phase::Stopped { reason });
        false
    }

    fn begin_preamble(&mut self) {
        self.stage = Stage::Preamble;
        self.stage_deadline = Some(Instant::now() + self.opt.reply_timeout);
        self.set_phase(Phase::SettingUp);
        self.send_cmd(Command::SetProtocolProtobuf, Pending::Preamble("SetProtocol"));
        self.send_cmd(Command::StreamConnect, Pending::Preamble("StreamConnect"));
        let txn = self.next_txn();
        let host = self.conn.as_ref().map(|c| c.host.clone()).unwrap_or_default();
        self.pending.insert(txn, Pending::Subscribe);
        self.sent_at.insert(txn, Instant::now());
        let f = request::subscribe(txn, &host);
        self.write(&f);
        self.emergency.lock().unwrap_or_else(|e| e.into_inner()).opened = true;
    }

    fn preamble_done(&mut self) {
        self.stage = Stage::Defining;
        self.stage_deadline = None;
        self.define_waiting();
        self.check_defining_done();
    }

    fn define_waiting(&mut self) {
        for i in 0..self.chans.len() {
            if self.chans[i].state != ChannelState::Waiting {
                continue;
            }
            let c = &self.chans[i];
            let d = Define { channel: c.slot, signal: c.key.signal, unit: c.key.unit.clone(), axis: c.key.axis };
            let key = c.key.clone();
            self.chans[i].state = ChannelState::Defining;
            self.send_cmd(Command::Define(d), Pending::Define { key, sent: Instant::now(), reported: false, void: false });
            self.owe_restart();
        }
        self.dirty = true;
    }

    fn defines_in_flight(&self) -> bool {
        self.chans.iter().any(|c| c.state == ChannelState::Defining)
    }

    fn check_defining_done(&mut self) {
        if self.stage != Stage::Defining || self.defines_in_flight() {
            return;
        }
        self.stage = Stage::Streaming;
        self.stage_deadline = None;
        self.established = true;
        let n = self.chans.iter().filter(|c| c.stream().is_some()).count();
        if n > 0 {
            self.start_stream();
        }
        self.log.info(format!("Streaming {n} of {} channel(s).", self.chans.len()));
        self.streaming_since = Some(Instant::now());
        self.set_phase(Phase::Streaming);
    }

    fn start_stream(&mut self) {
        self.send_cmd(Command::StartStream, Pending::Start);
        self.started = true;
        self.restart_needed = false;
        self.restart_owed_at = None;
        self.last_start = Some(Instant::now());
    }

    fn owe_restart(&mut self) {
        if !self.started {
            return;
        }
        if !self.restart_needed {
            self.restart_owed_at = Some(Instant::now());
        }
        self.restart_needed = true;
    }

    fn undefine_own(&mut self, s: u32) {
        self.retiring.insert(s);
        self.send_cmd(Command::Undefine(s), Pending::Undefine { stream: s });
        self.owe_restart();
    }

    fn maybe_restart(&mut self) {
        if self.stage != Stage::Streaming || self.defines_in_flight() {
            return;
        }
        let any = self.chans.iter().any(|c| c.stream().is_some());
        if any && (self.restart_needed || !self.started) {
            self.start_stream();
        } else if !any {
            if self.restart_needed && self.started {
                self.send_cmd(Command::StartStream, Pending::Start);
                self.last_start = Some(Instant::now());
            }
            self.restart_needed = false;
            self.restart_owed_at = None;
        }
    }

    fn controller_now(&self) -> Option<u64> {
        self.newest_raw.map(|r| r.saturating_add(self.newest_at.map_or(0, |a| a.elapsed().as_millis() as u64)))
    }

    fn begin_teardown(&mut self, after: After) {
        if self.conn.is_none() {
            self.after_teardown = after;
            self.after_teardown_done();
            return;
        }
        if self.stage == Stage::TearingDown {
            if after == After::Exit || (after == After::Idle && matches!(self.after_teardown, After::Retry(_))) {
                self.after_teardown = after;
            }
            return;
        }
        let opened = matches!(self.stage, Stage::Preamble | Stage::Defining | Stage::Streaming);
        self.stage = Stage::TearingDown;
        self.after_teardown = after;
        self.set_phase(Phase::TearingDown);
        self.pending.retain(|_, p| matches!(p, Pending::Define { void: false, .. }));
        if opened {
            let own: Vec<u32> = self.chans.iter().filter_map(|c| c.stream()).collect();
            let tenant = self.had_samples;
            if tenant && !own.is_empty() {
                self.send_cmd(Command::StopStream, Pending::Stop);
            }
            for &s in &own {
                self.send_cmd(Command::Undefine(s), Pending::Undefine { stream: s });
            }
            if !tenant && !own.is_empty() {
                self.send_cmd(Command::StartStream, Pending::Start);
            }
            self.send_cmd(Command::StreamDisconnect, Pending::Disconnect);
            self.teardown_deadline = Some(Instant::now() + self.opt.teardown_wait);
        } else {
            self.teardown_deadline = Some(Instant::now());
        }
    }

    fn finish_teardown_now(&mut self) {
        let deadline = self.teardown_deadline.unwrap_or_else(Instant::now);
        while self.conn.is_some() && Instant::now() < deadline && !self.pending.is_empty() {
            match self.rx.recv_timeout(Duration::from_millis(10)) {
                Ok(Event::Frame { generation, bytes, at }) => self.frame_event(generation, &bytes, at),
                Ok(Event::Closed { generation, .. }) => {
                    if self.conn.as_ref().is_some_and(|c| c.generation == generation) {
                        break;
                    }
                }
                Ok(Event::Request(Request::Shutdown)) => self.after_teardown = After::Exit,
                Ok(Event::Request(r)) => self.deferred.push_back(r),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {}
            }
        }
        if self.stage == Stage::TearingDown {
            self.complete_teardown();
        }
    }

    fn complete_teardown(&mut self) {
        let unanswered = self.pending.len();
        if unanswered > 0 {
            self.log.warn(format!("Teardown: {unanswered} request(s) unanswered before closing."));
        }
        self.drop_conn();
        self.after_teardown_done();
    }

    fn after_teardown_done(&mut self) {
        self.stage = Stage::Hello;
        self.teardown_deadline = None;
        match std::mem::replace(&mut self.after_teardown, After::Idle) {
            After::Idle => {
                self.log.info("Disconnected.");
                self.set_phase(Phase::Idle);
            }
            After::Exit => {
                self.log.info("Disconnected; the session is closed.");
                self.set_phase(Phase::Idle);
                self.exit = true;
            }
            After::Stopped(reason) => {
                self.set_phase(Phase::Stopped { reason });
            }
            After::Retry(why) => self.schedule_reconnect(&why),
        }
    }

    fn stalled(&mut self) {
        let secs = self.stall_bound().as_secs_f64();
        let silent = self.last_frame_at.is_none_or(|f| self.last_any_sample.is_some_and(|s| f <= s));
        let reason = if self.vc_patient() {
            format!(
                "The virtual controller has sent no samples for {secs:.0} s{}. It may have stopped, or the PC may be too busy for it; another program showing signals could also have taken InfoStream. Connect again when it is running.",
                if silent { ", and has not answered either" } else { "" }
            )
        } else if silent {
            let asked = self.last_probe_at.filter(|&p| self.last_any_sample.is_none_or(|s| p > s));
            match asked {
                None => {
                    self.send_probe(Instant::now());
                    return;
                }
                Some(p) if p.elapsed() < self.opt.stall_after.saturating_sub(self.probe_bound()) => return,
                Some(_) => {}
            }
            let why = format!(
                "nothing at all from the controller for {secs:.0} s, not even an answer to the liveness check: the network or the controller has stalled (a program taking InfoStream would still answer), so connecting again by itself, and setting nothing up until the controller's client list shows nobody new came meanwhile"
            );
            self.lost_at = Some(Instant::now());
            self.leave_quietly(After::Retry(why), "nothing from the controller");
            return;
        } else {
            format!(
                "The controller stopped sending samples ({secs:.0} s with none on a live connection). Another tool, such as RobotStudio or TuneMaster, has most likely taken InfoStream. Connect again when it is free."
            )
        };
        self.quit_quietly(reason, "samples stopped");
    }

    fn vc_patient(&self) -> bool {
        !self.opt.vc_pause_patience.is_zero() && self.conn.as_ref().is_some_and(|c| is_loopback(c.peer.ip()))
    }

    fn stall_bound(&self) -> Duration {
        if self.vc_patient() { self.opt.vc_pause_patience.max(self.opt.stall_after) } else { self.opt.stall_after }
    }

    fn probe_patience(&self) -> Duration {
        if self.vc_patient() { self.stall_bound().max(self.opt.handshake_timeout) } else { self.opt.handshake_timeout }
    }

    fn probe_bound(&self) -> Duration {
        let fastest = self
            .chans
            .iter()
            .filter(|c| c.stream().is_some() && c.session_samples > 0 && c.kind != Some(ValueKind::String))
            .filter_map(|c| c.sample_ms)
            .fold(f64::INFINITY, f64::min);
        let by_rate = if fastest.is_finite() { Duration::from_secs_f64(fastest * 25.0 / 1000.0) } else { Duration::ZERO };
        self.opt.probe_after.max(by_rate)
    }

    fn send_probe(&mut self, now: Instant) {
        let txn = self.next_txn();
        self.pending.insert(txn, Pending::Probe { sent: now, samples: self.counters.samples });
        self.sent_at.insert(txn, now);
        self.last_probe_at = Some(now);
        self.emergency.lock().unwrap_or_else(|e| e.into_inner()).next_txn = txn;
        self.counters.liveness_checks += 1;
        self.dirty = true;
        let f = request::hello(txn);
        self.write(&f);
    }

    fn probe_answered(&mut self, frame: &Frame<'_>, sent: Instant, samples_then: u64) {
        if self.stage != Stage::Streaming || !self.started {
            return;
        }
        let now_list = Announce::from_rads(frame.rads(), frame.ctrl1()).clients;
        if self.counters.samples > samples_then || self.last_any_sample.is_some_and(|s| s > sent) {
            if self.had_samples {
                self.baseline = Some(now_list);
            }
            return;
        }
        let own_change = self.own_change_pending() || self.own_change_sent.is_some_and(|t| t > sent) || self.last_start.is_some_and(|t| t > sent);
        if own_change {
            return;
        }
        let then_list = self.baseline.clone().or_else(|| self.announce.as_ref().map(|a| a.clients.clone())).unwrap_or_default();
        let (mut joined, mut left) = client_changes(&then_list, &now_list);
        joined.retain(|a| !is_pendant_address(a));
        left.retain(|a| !is_pendant_address(a));
        let local_ip = self.conn.as_ref().map(|c| c.local.ip().to_string()).unwrap_or_default();
        let name = |a: &String| if *a == local_ip { format!("another program on this PC ({a})") } else { a.clone() };
        let names = |v: &[String]| v.iter().map(name).collect::<Vec<_>>().join(" and ");
        let (reason, mark) = if !joined.is_empty() {
            (
                format!(
                    "Every stream stopped at once, and {} connected to the controller after this program did: another program has taken InfoStream. Stopped straight away, before it sets up its signals (leaving any later clears them). Connect again when it is free.",
                    names(&joined)
                ),
                "another client took InfoStream",
            )
        } else if !left.is_empty() {
            if !self.leaver_retry {
                let why = format!(
                    "every stream stopped at once when {} disconnected from the controller (a program that had set up test signals ends InfoStream for every program connected when it leaves); nobody is taking it, so connecting again, once",
                    names(&left)
                );
                self.baseline = Some(now_list);
                self.leaver_retry_next = true;
                self.lost_at = Some(Instant::now());
                self.leave_quietly(After::Retry(why), "another client left");
                return;
            }
            (
                format!(
                    "Every stream stopped at once when {} disconnected from the controller, again: a program that had set up test signals and leaves ends InfoStream for every program connected. Connect again.",
                    names(&left)
                ),
                "another client left",
            )
        } else if self.vc_patient() {
            if self.vc_paused_since.is_none() {
                self.vc_paused_since = Some(Instant::now());
                self.log.warn(format!(
                    "Every stream stopped while the virtual controller still answers, and no program connected or left. A virtual controller pauses when the PC is too busy for it: waiting up to {:.0} s for it to carry on, the values shown as stale meanwhile.",
                    self.opt.vc_pause_patience.as_secs_f64()
                ));
            }
            self.advice = Some("The virtual controller has paused (the PC may be too busy for it). Waiting for it to carry on; the values are stale meanwhile.".into());
            self.dirty = true;
            return;
        } else {
            (
                "Every stream stopped at once while the controller still answers: another program has most likely taken InfoStream (RobotStudio or TuneMaster showing signals, for example), or one that had set up signals has left, which can end InfoStream for every program connected. Stopped straight away, before a newcomer sets up its signals (leaving any later clears them). Connect again when it is free.".to_string(),
                "another client took InfoStream",
            )
        };
        self.quit_quietly(reason, mark);
    }

    fn taken_over(&mut self, what: &str) {
        let reason = format!(
            "{}. Another tool, such as RobotStudio or TuneMaster, has most likely taken InfoStream, so what arrives now could be its signals under these channels' names: stopped before showing any of it. Connect again when it is free.",
            capitalize(what)
        );
        self.quit_quietly(reason, "another client took InfoStream");
    }

    fn quit_quietly(&mut self, reason: String, mark: &str) {
        self.log.error(reason.clone());
        self.leave_quietly(After::Stopped(reason), mark);
    }

    fn leave_quietly(&mut self, after: After, mark: &str) {
        self.emit_mark(Mark::Lost { reason: mark.into() });
        self.by_stream.clear();
        self.emergency.lock().unwrap_or_else(|e| e.into_inner()).own.clear();
        self.stage = Stage::TearingDown;
        if self.after_teardown != After::Exit {
            self.after_teardown = after;
        }
        self.set_phase(Phase::TearingDown);
        self.pending.clear();
        self.sent_at.clear();
        self.send_cmd(Command::StreamDisconnect, Pending::Disconnect);
        self.teardown_deadline = Some(Instant::now() + self.opt.teardown_wait.min(Duration::from_millis(500)));
    }

    fn tick(&mut self) {
        let now = Instant::now();

        if let Some(at) = self.retry_at
            && now >= at && self.conn.is_none() {
                self.retry_at = None;
                self.counters.reconnects += 1;
                self.auto_reconnect = true;
                self.open();
            }

        if self.stage == Stage::TearingDown && (self.pending.is_empty() || self.teardown_deadline.is_some_and(|d| now >= d)) {
            if self.conn.is_some() {
                self.complete_teardown();
            } else {
                self.after_teardown_done();
            }
            return;
        }

        if let Some(d) = self.stage_deadline
            && now >= d && self.conn.is_some() {
                match self.stage {
                    Stage::Hello => {
                        let t = self.target.as_ref().map(|t| t.to_string()).unwrap_or_default();
                        let vc = self.conn.as_ref().is_some_and(|c| self.vc_at(c.peer.ip()));
                        self.stage_deadline = None;
                        self.drop_conn();
                        let wait = self.hello_wait.as_secs_f64();
                        let why = if self.system_id.is_some() {
                            format!("{t} took the connection but did not answer the RobAPI handshake within {wait:.0} s")
                        } else {
                            format!("{t} accepted the connection but did not answer the RobAPI handshake within {wait:.0} s; it is probably not a controller's RobAPI port")
                        };
                        if !vc && self.retry_while_recovering(&why) {
                            return;
                        }
                        if self.established { self.schedule_reconnect(&why) } else {
                            self.log.error(format!("{}.", capitalize(&why)));
                            self.set_phase(Phase::Stopped { reason: capitalize(&why) });
                        }
                        return;
                    }
                    Stage::Preamble => {
                        self.stage_deadline = None;
                        self.log.error("The controller did not answer the InfoStream setup; closing.");
                        self.begin_teardown(After::Stopped("The controller did not answer the InfoStream setup.".into()));
                        return;
                    }
                    _ => self.stage_deadline = None,
                }
            }

        if self.conn.is_some() && matches!(self.stage, Stage::Defining | Stage::Streaming) {
            let overdue = self.pending.values().any(|p| matches!(p, Pending::Define { sent, reported: false, void: false, .. } if now.duration_since(*sent) >= self.opt.reply_timeout));
            if overdue {
                self.defines_timed_out(now);
            }
            self.check_defining_done();
            self.maybe_restart();
        }

        if self.conn.is_some() && self.stage == Stage::Streaming && self.started {
            for c in &mut self.chans {
                if c.stream().is_none() || c.kind == Some(ValueKind::String) || (c.kind.is_none() && c.expect_text) {
                    continue;
                }
                let bound = stale_bound(self.opt.stale_after, c.sample_ms);
                let since = c.last_arrival.or(match (c.defined_at, self.last_start) {
                    (Some(d), Some(s)) => Some(d.max(s)),
                    (d, s) => d.or(s),
                });
                let since = since.unwrap_or(now);
                let stale = now.duration_since(since) > bound;
                if stale && !c.stale_reported {
                    c.stale_reported = true;
                    self.dirty = true;
                    if c.last_arrival.is_some() {
                        self.log.warn(format!("{}: no samples for {:.1} s; shown as stale.", c.key, bound.as_secs_f64()));
                    } else {
                        self.log.warn(format!("{}: defined, but no samples in the first {:.1} s.", c.key, bound.as_secs_f64()));
                    }
                }
            }
            self.sweep_unanswered(now);
            let backlog = self.conn.as_ref().is_some_and(|c| c.queued.load(Ordering::Relaxed) > 0);
            let own_change_pending = self.own_change_pending();
            let nothing_yet = !self.had_samples
                && !own_change_pending
                && !backlog
                && self.chans.iter().any(|c| c.stream().is_some())
                && self.last_start.is_some_and(|s| now.duration_since(s) > self.opt.stall_after);
            let on_vc = self.conn.as_ref().is_some_and(|c| is_loopback(c.peer.ip()));
            if nothing_yet && self.auto_reconnect && self.delivered_before {
                if on_vc && self.followed_restart && !self.vc_hold_retried && self.others.iter().all(|o| o.same_pc) {
                    self.vc_hold_retried = true;
                    self.lost_at = Some(Instant::now());
                    self.emit_mark(Mark::Lost { reason: "no samples after the virtual controller restarted".into() });
                    self.begin_teardown(After::Retry(
                        "no samples after the virtual controller restarted: RobotStudio's own connection takes InfoStream as a virtual controller starts, and this program leaving ends that, so connecting again, once".into(),
                    ));
                    return;
                }
                let reason = if self.leaver_retry {
                    "Connected again once, after another program's exit had ended InfoStream, but no samples arrive: another program may have taken InfoStream since. Connect again when it is free.".to_string()
                } else if on_vc && self.vc_hold_retried {
                    "Connected again once more after the virtual controller restarted, but still no samples arrive: another program on this PC is most likely showing test signals (TuneMaster, RobotStudio's Signal Analyzer). Close it, then connect again.".to_string()
                } else if on_vc {
                    "Connected again after the connection was lost, but no samples arrive: another connection has InfoStream. On a virtual controller that is usually RobotStudio's own, which takes it whenever the controller starts, and this program leaving ends that: Connect again. (If a program on this PC is showing test signals, TuneMaster or RobotStudio's Signal Analyzer, close it first.)".to_string()
                } else {
                    "Connected again after the connection was lost, but no samples arrive: another program has most likely taken InfoStream while the connection was down (the controller sends every sample to one program). Connect again when it is free.".to_string()
                };
                self.log.error(reason.clone());
                self.emit_mark(Mark::Lost { reason: "no samples after reconnecting".into() });
                self.begin_teardown(After::Stopped(reason));
                return;
            }
            let programs = self.others.iter().filter(|o| !o.pendant).count();
            if nothing_yet && !self.told_no_samples && programs > 0 {
                self.told_no_samples = true;
                let (what, advice) = no_samples_advice(on_vc);
                self.log.warn(format!(
                    "No samples at all in {:.0} s. {} other program(s) are connected to this controller, and {what} If this program's own earlier connection broke, the controller may still hold it (it lets go once it hears from this PC again, or at a timeout of its own): connect again in a minute.",
                    self.opt.stall_after.as_secs_f64(),
                    programs
                ));
                self.advice = Some(advice.into());
                self.dirty = true;
            }
            let delivering = self.chans.iter().any(|c| c.stream().is_some() && c.session_samples > 0 && c.kind != Some(ValueKind::String));
            if delivering && !own_change_pending && !backlog {
                let since = match (self.last_any_sample, self.last_start) {
                    (Some(a), Some(b)) => a.max(b),
                    (a, b) => a.or(b).unwrap_or(now),
                };
                let silent = now.duration_since(since);
                if silent > self.stall_bound() {
                    self.stalled();
                    return;
                }
                let spaced = self.last_probe_at.is_none_or(|t| now.duration_since(t) >= self.probe_bound());
                if silent > self.probe_bound() && spaced && !self.pending.values().any(|p| matches!(p, Pending::Probe { .. })) {
                    self.send_probe(now);
                }
            }
        }

        for c in &mut self.chans {
            let dt = now.duration_since(c.rate_mark.0).as_secs_f64();
            if dt >= 1.0 {
                c.rate = (c.samples - c.rate_mark.1) as f64 / dt;
                c.rate_mark = (now, c.samples);
                self.dirty = true;
            }
        }
    }

    fn defines_timed_out(&mut self, now: Instant) {
        let mut late = Vec::new();
        for p in self.pending.values_mut() {
            if let Pending::Define { key, sent, reported, void: false } = p
                && !*reported && now.duration_since(*sent) >= self.opt.reply_timeout {
                    *reported = true;
                    late.push(key.clone());
                }
        }
        for key in late {
            if let Some(c) = self.chans.iter_mut().find(|c| c.key == key && c.state == ChannelState::Defining) {
                c.state = ChannelState::NoReply;
            }
            self.log.warn(format!("{key}: no reply to the define within {:.0} s; it will still be used if the reply comes later.", self.opt.reply_timeout.as_secs_f64()));
        }
        self.dirty = true;
        self.check_defining_done();
        self.maybe_restart();
    }

    fn frame(&mut self, bytes: &[u8], at: Instant) {
        let Some(frame) = Frame::parse(bytes) else {
            self.counters.desyncs += 1;
            self.lost("a malformed frame", true);
            return;
        };
        self.counters.frames += 1;
        self.counters.bytes += bytes.len() as u64;
        self.last_frame_at = Some(at);
        if !frame.has_trailer() {
            self.counters.no_trailer += 1;
            if self.counters.no_trailer == 1 {
                self.log.warn("A frame without the usual 0xDE trailer arrived; counted in the diagnostics.");
            }
        }

        if frame.service() == service::AYA {
            self.counters.ayas += 1;
            let f = request::aya_reply(frame.txn(), frame.ctrl1(), frame.ctrl2());
            self.write(&f);
            return;
        }

        if self.stage == Stage::Hello && frame.service() == service::CONTROL && frame.txn() == self.hello_txn {
            self.handshake_done(&frame);
            return;
        }
        if frame.service() == service::CONTROL
            && let Some(Pending::Probe { sent, samples }) = self.pending.get(&frame.txn()).cloned()
        {
            self.pending.remove(&frame.txn());
            self.sent_at.remove(&frame.txn());
            self.probe_answered(&frame, sent, samples);
            return;
        }

        let txn = frame.txn();
        let mut handled = false;
        for rad in frame.rads() {
            if let Some(offset) = sample::find_marker(rad.data) {
                *self.counters.sample_services.entry(frame.service()).or_default() += 1;
                *self.counters.marker_offsets.entry(offset).or_default() += 1;
                handled = true;
                if self.stage == Stage::TearingDown {
                    return;
                }
                self.samples(rad.data, at);
                continue;
            }
            if txn != 0 && self.pending.contains_key(&txn) {
                let r = Reply::from_rad(&rad);
                self.reply(txn, r);
                handled = true;
                continue;
            }
            if frame.service() == service::SEND && rad.format == wire::rad_format::EVENT && rad.data.len() >= 8 {
                let sub = u32::from_be_bytes(rad.data[4..8].try_into().unwrap());
                if self.subscription.is_some_and(|s| s != sub) {
                    self.counters.foreign_subscription += 1;
                }
                handled = true;
            }
        }
        if !handled && frame.rad_count() > 0 {
            self.counters.unexpected_frames += 1;
        }
    }

    fn reply(&mut self, txn: u16, r: Reply) {
        let Some(p) = self.pending.remove(&txn) else { return };
        self.sent_at.remove(&txn);
        if matches!(p, Pending::Define { void: false, .. } | Pending::Undefine { .. } | Pending::UndefineAll | Pending::Start) {
            self.own_change_at = self.controller_now();
        }
        match p {
            Pending::Preamble(what) => {
                if r.is_failure() {
                    let msg = format!("The controller refused {what}: {}", r.summary());
                    self.log.error(msg.clone());
                    self.begin_teardown(After::Stopped(msg));
                    return;
                }
                self.check_preamble();
            }
            Pending::Subscribe => match reply::parse_subscription(&r.text) {
                Some(id) => {
                    self.subscription = Some(id);
                    self.dirty = true;
                    self.check_preamble();
                }
                None => {
                    let msg = format!("The controller refused the sample subscription: {}", r.summary());
                    self.log.error(msg.clone());
                    self.begin_teardown(After::Stopped(msg));
                }
            },
            Pending::Define { void: true, .. } => {}
            Pending::Define { key, reported, .. } => self.define_reply(key, reported, r),
            Pending::Undefine { stream } => {
                self.retiring.remove(&stream);
                if r.is_failure() {
                    self.log.warn(format!("Undefining stream {stream}: {}", r.summary()));
                }
            }
            Pending::Start | Pending::Stop | Pending::Disconnect => {
                if matches!(p, Pending::Start) {
                    self.last_start = Some(Instant::now());
                }
                if r.is_failure() {
                    self.log.warn(format!("The controller answered: {}", r.summary()));
                }
            }
            Pending::UndefineAll => {
                self.retiring.clear();
                self.log.info(if r.is_failure() { format!("StreamUndefineAll answered: {}", r.summary()) } else { "StreamUndefineAll done; redefining this program's channels.".into() });
            }
            Pending::Probe { .. } => {}
        }
    }

    fn check_preamble(&mut self) {
        if self.stage != Stage::Preamble {
            return;
        }
        let waiting = self.pending.values().any(|p| matches!(p, Pending::Preamble(_) | Pending::Subscribe));
        if !waiting {
            self.preamble_done();
        }
    }

    fn define_reply(&mut self, key: ChannelKey, was_late: bool, r: Reply) {
        let stream = r.stream_id();
        if self.stage == Stage::TearingDown {
            if let (Some(s), false) = (stream, r.is_failure()) {
                self.send_cmd(Command::Undefine(s), Pending::Undefine { stream: s });
            }
            return;
        }
        let idx = self.chans.iter().position(|c| c.key == key);
        if let Some(i) = idx
            && let Some(existing) = self.chans[i].stream()
        {
            match stream {
                Some(s) if !r.is_failure() && s != existing => self.undefine_own(s),
                Some(_) if !r.is_failure() => {}
                _ => self.log.info(format!("{key}: an earlier define was answered \"{}\"; the channel keeps stream {existing}.", r.summary())),
            }
            self.check_defining_done();
            self.maybe_restart();
            return;
        }
        let Some(idx) = idx else {
            if let Some(s) = stream
                && !r.is_failure()
                && (self.stage == Stage::Streaming || self.stage == Stage::Defining)
            {
                self.undefine_own(s);
            }
            self.check_defining_done();
            self.maybe_restart();
            return;
        };
        match stream {
            Some(s) if !r.is_failure() => {
                if let Some(&other) = self.by_stream.get(&s)
                    && other != idx
                {
                    let msg = format!("the controller gave {key} stream {s}, which {} was still using, so another client has removed this program's streams", self.chans[other].key);
                    self.taken_over(&msg);
                    return;
                }
                let sample_ms = match usable_sample_ms(r.sample_time_ms()) {
                    Ok(ms) => ms,
                    Err((reported, taken)) => {
                        self.log.warn(format!("{key}: the controller reported a sample time of {reported} ms, which no controller uses (an IRC5 reports 4.032 or 24.192 ms); taking {taken} ms."));
                        taken
                    }
                };
                let ch = self.store.channel(&key, sample_ms);
                let c = &mut self.chans[idx];
                c.state = ChannelState::Defined { stream: s };
                c.sample_ms = Some(sample_ms);
                c.store = Some(ch);
                c.stale_reported = false;
                c.last_raw = None;
                c.last_arrival = None;
                c.defined_at = Some(Instant::now());
                self.by_stream.insert(s, idx);
                self.emergency.lock().unwrap_or_else(|e| e.into_inner()).own.push(s);
                self.log.info(format!("{key} -> stream {s} ({sample_ms} ms){}.", if was_late { ", after its reply came late" } else { "" }));
                self.emit_mark(Mark::Defined { key, stream: s, sample_ms });
                self.owe_restart();
            }
            _ => {
                let code = r.controller_status();
                let meaning = code.and_then(reply::describe_status);
                let text = match meaning {
                    Some(m) => format!("{m} (status {})", code.unwrap_or_default()),
                    None => r.summary(),
                };
                self.log.warn(format!("{key} refused: {text}. Note: {}.", reply::EVENT_LOG_NOTE));
                self.chans[idx].state = ChannelState::Refused { code, text };
            }
        }
        self.dirty = true;
        self.check_defining_done();
        self.maybe_restart();
    }

    fn samples(&mut self, rad: &[u8], at: Instant) {
        self.records.clear();
        self.defects.clear();
        let mut records = std::mem::take(&mut self.records);
        let mut defects = std::mem::take(&mut self.defects);
        let header = sample::decode(rad, &mut records, &mut defects);
        self.counters.sample_frames += 1;
        if let (Some(h), Some(sub)) = (header, self.subscription)
            && h.subscription != sub && h.marker_at == sample::MARKER_OFFSET {
                self.counters.foreign_subscription += 1;
            }
        for d in &defects {
            let k = d.to_string();
            *self.counters.defects.entry(k.clone()).or_default() += 1;
            if self.defect_logged.insert(k.clone()) {
                self.log.warn(format!("Sample data: {k}. Counted in the diagnostics; the rest of the frame was used."));
            }
        }
        if let Some(what) = self.takeover_in(&records, at) {
            self.records = records;
            self.defects = defects;
            self.taken_over(&what);
            return;
        }
        let taps_active = !self.taps.lock().unwrap_or_else(|e| e.into_inner()).is_empty();
        let arrived = SystemTime::now().checked_sub(at.elapsed()).unwrap_or_else(SystemTime::now);
        let mut batches = Vec::new();
        for rec in &records {
            let Some(&idx) = self.by_stream.get(&rec.stream) else {
                if self.retiring.contains(&rec.stream) {
                    continue;
                }
                self.counters.foreign_records += 1;
                if self.known_foreign.insert(rec.stream) {
                    self.log.warn(format!(
                        "Samples arrive for stream {}, which is not one of this program's channels (another client's, or a define of this program's that was never answered). Ignored, and counted in the diagnostics.",
                        rec.stream
                    ));
                }
                continue;
            };
            let n = rec.stamps.len();
            if n == 0 {
                continue;
            }
            if self.chans.get(idx).and_then(Chan::stream) != Some(rec.stream) {
                debug_assert!(false, "stream map out of date for stream {}", rec.stream);
                self.reindex();
                continue;
            }
            let c = &mut self.chans[idx];
            if c.kind != Some(rec.kind) {
                if let Some(old) = c.kind {
                    self.log.warn(format!("{}: record type changed from {} to {}.", c.key, old.label(), rec.kind.label()));
                }
                c.kind = Some(rec.kind);
                self.dirty = true;
            }
            let mut raw_ms = Vec::with_capacity(n);
            let mut tl = Vec::with_capacity(n);
            for &st in &rec.stamps {
                if let Some(prev) = c.last_raw {
                    let step = st.wrapping_sub(prev) as f64;
                    if let Some(ms) = c.sample_ms
                        && st > prev && step > ms * 1.5 {
                            c.gaps += 1;
                        }
                }
                c.last_raw = Some(st);
                self.newest_raw = Some(st);
                self.newest_at = Some(at);
                self.first_raw.get_or_insert(st);
                let (u, t, ev) = self.timeline.map(st, at);
                if ev != ClockEvent::None {
                    self.own_change_at = None;
                    self.first_raw = Some(st);
                }
                if let ClockEvent::Reset { from_raw, to_raw } = ev {
                    self.counters.clock_resets += 1;
                    if to_raw < from_raw {
                        self.log.warn(format!("The controller's clock went back from {from_raw} to {to_raw} ms: it has restarted. The charts continue after the gap."));
                    } else {
                        self.log.warn(format!("The controller's clock jumped forward from {from_raw} to {to_raw} ms, further than the time that passed. The charts continue after the gap."));
                    }
                    let wall = SystemTime::now();
                    broadcast(&self.taps, TapEvent::Mark { wall, mark: Mark::ClockReset { from_raw, to_raw } }, &mut self.counters);
                }
                raw_ms.push(u);
                tl.push(t);
            }
            let c = &mut self.chans[idx];
            let values = match &rec.values {
                RecordValues::Float(v) => BatchValues::Number(v.iter().map(|&x| f64::from(x)).collect()),
                RecordValues::Int(v) => BatchValues::Number(v.iter().map(|&x| x as f64).collect()),
                RecordValues::String(v) => BatchValues::Text(v.clone()),
            };
            let mut accepted = false;
            if let Some(ch) = &c.store {
                let mut ring = ch.lock();
                ring.kind = Some(rec.kind);
                match &values {
                    BatchValues::Number(v) => {
                        for (t, x) in tl.iter().zip(v) {
                            accepted |= ring.push(*t, *x);
                        }
                    }
                    BatchValues::Text(v) => {
                        for (t, text) in tl.iter().zip(v) {
                            if ring.push(*t, f64::NAN) {
                                accepted = true;
                                ring.last_text = Some(text.clone());
                            }
                        }
                    }
                }
            }
            c.samples += n as u64;
            c.session_samples += n as u64;
            if accepted {
                c.last_arrival = Some(at);
                if c.stale_reported {
                    c.stale_reported = false;
                    let k = c.key.clone();
                    self.log.info(format!("{k}: samples arriving again."));
                }
            }
            self.counters.samples += n as u64;
            if !self.had_samples {
                self.had_samples = true;
                self.emergency.lock().unwrap_or_else(|e| e.into_inner()).tenant = true;
                if let Some(c) = self.candidate.take() {
                    self.baseline = Some(c);
                }
                self.rung = 0;
                self.leaver_retry = false;
                self.leaver_retry_next = false;
                self.followed_restart = false;
                self.vc_hold_retried = false;
                self.lost_at = None;
                self.held_since = None;
            }
            self.delivered_before = true;
            self.advice = None;
            let silent_since = self.last_any_sample.replace(at);
            if self.vc_paused_since.take().is_some() {
                let silent = silent_since.map(|t| at.saturating_duration_since(t)).unwrap_or_default();
                self.log.info(format!("The virtual controller is sending again, after {:.1} s without samples.", silent.as_secs_f64()));
            }
            if taps_active {
                batches.push(SampleBatch { key: self.chans[idx].key.clone(), kind: rec.kind, raw_ms, timeline_ms: tl, values, arrived });
            }
        }
        for b in batches {
            broadcast(&self.taps, TapEvent::Samples(b), &mut self.counters);
        }
        self.records = records;
        self.defects = defects;
        self.dirty = true;
    }

    fn takeover_in(&self, records: &[sample::Record], at: Instant) -> Option<String> {
        if self.conn.is_none() || !matches!(self.stage, Stage::Defining | Stage::Streaming) {
            return None;
        }
        let streaming = self.stage == Stage::Streaming && self.started;
        let settled = streaming && !self.restart_needed && !self.defines_in_flight();
        let pause_is_ours_to_end =
            streaming && self.restart_needed && self.restart_owed_at.is_some_and(|owed| self.last_start.is_none_or(|s| s + OWN_START_SLACK < owed));
        let own_doing = |stamp: u64| match (self.own_change_at, self.first_raw) {
            (Some(s), _) => stamp <= s.saturating_add(OWN_CHANGE_MARGIN_MS),
            (None, Some(f)) => stamp <= f.saturating_add(LEFTOVER_MARGIN_MS),
            (None, None) => true,
        };
        for rec in records {
            let Some(&idx) = self.by_stream.get(&rec.stream) else {
                if settled
                    && rec.kind != ValueKind::String
                    && !self.retiring.contains(&rec.stream)
                    && !self.known_foreign.contains(&rec.stream)
                    && rec.stamps.first().is_some_and(|&s| !own_doing(s))
                {
                    return Some(format!("samples for stream {}, which this program did not define, began arriving: another tool has started streaming from this controller", rec.stream));
                }
                continue;
            };
            let Some(c) = self.chans.get(idx).filter(|c| c.stream() == Some(rec.stream)) else { continue };
            if let Some(k) = c.kind
                && k != rec.kind
            {
                return Some(format!("{} changed from {} to {} records, which a stream never does", c.key, k.label(), rec.kind.label()));
            }
            if c.kind == Some(ValueKind::String) || rec.kind == ValueKind::String {
                continue;
            }
            let Some(&first) = rec.stamps.first() else { continue };
            let Some(prev) = c.last_raw else {
                let late = match (self.own_change_at, self.first_raw) {
                    (Some(s), _) => first > s.saturating_add(OWN_CHANGE_MARGIN_MS),
                    (None, Some(f)) => first > f.saturating_add(OWN_CHANGE_MARGIN_MS),
                    (None, None) => !self.vc_patient() && c.defined_at.max(self.last_start).is_some_and(|s| at.saturating_duration_since(s) > FIRST_SAMPLE_WALL_MARGIN),
                };
                if settled && late {
                    return Some(format!("{} sent its first sample long after this program started it, as a stream another client has just started does", c.key));
                }
                continue;
            };
            let Some(ms) = c.sample_ms else { continue };
            let step = first.saturating_sub(prev) as f64;
            if first <= prev || step <= (ms * 2.5).max(20.0) {
                continue;
            }
            if pause_is_ours_to_end {
                return Some(format!("{} resumed after a {step:.0} ms pause that only this program's own StartStream should have ended, before it sent one", c.key));
            }
            if !settled {
                continue;
            }
            if self.own_change_at.is_some_and(|s| prev <= s.saturating_add(OWN_CHANGE_MARGIN_MS)) {
                continue;
            }
            let middle = prev + (first - prev) / 2;
            let others_went_on = self.chans.iter().enumerate().any(|(j, d)| {
                j != idx
                    && d.stream().is_some()
                    && d.kind != Some(ValueKind::String)
                    && d.sample_ms.is_some_and(|m| m * 2.0 < step)
                    && d.last_raw.is_some_and(|t| t > middle && t < first)
            });
            if others_went_on {
                continue;
            }
            return Some(format!("every stream skipped {step:.0} ms of samples at once, and this program had changed nothing"));
        }
        None
    }

    fn emit_mark(&mut self, mark: Mark) {
        broadcast(&self.taps, TapEvent::Mark { wall: SystemTime::now(), mark }, &mut self.counters);
    }

    fn publish(&mut self, force: bool) {
        let now = Instant::now();
        if !force && (!self.dirty || now.duration_since(self.last_publish) < Duration::from_millis(50)) {
            return;
        }
        self.dirty = false;
        self.last_publish = now;
        let mut s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        s.phase = self.phase.clone();
        s.target = self.target.clone();
        s.peer = self.conn.as_ref().map(|c| c.peer);
        s.local = self.conn.as_ref().map(|c| c.local);
        s.loopback = self.conn.as_ref().is_some_and(|c| is_loopback(c.peer.ip()));
        s.announce = self.announce.clone();
        s.others = self.others.clone();
        s.subscription = self.subscription;
        s.counters = self.counters.clone();
        s.connected_since = self.connected_since;
        s.streaming_since = self.streaming_since;
        s.timeline = self.timeline.clone();
        s.advice = if self.conn.is_some() { self.advice.clone() } else { None };
        s.moved = self.moved.clone();
        s.moves = self.moves;
        s.channels = self
            .chans
            .iter()
            .map(|c| ChannelStatus {
                key: c.key.clone(),
                state: c.state.clone(),
                sample_ms: c.sample_ms,
                kind: c.kind,
                samples: c.samples,
                last_arrival: c.last_arrival,
                rate: c.rate,
                gaps: c.gaps,
                stale: c.stale_reported,
                stale_bound: stale_bound(self.opt.stale_after, c.sample_ms),
            })
            .collect();
        drop(s);
        (self.notify)();
    }
}

fn broadcast(taps: &Mutex<Vec<TapSlot>>, ev: TapEvent, counters: &mut Counters) {
    let mut g = taps.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_empty() {
        return;
    }
    let n = g.len();
    let mut i = 0;
    let mut ev = Some(ev);
    while i < g.len() {
        let e = if i + 1 == n { ev.take().unwrap() } else { ev.clone().unwrap() };
        let slot = &g[i];
        if let TapEvent::Samples(b) = &e
            && slot.queued.load(Ordering::SeqCst) >= slot.samples_room
        {
            slot.dropped.fetch_add(b.raw_ms.len() as u64, Ordering::Relaxed);
            counters.dropped_to_taps += b.raw_ms.len() as u64;
            i += 1;
            continue;
        }
        slot.queued.fetch_add(1, Ordering::SeqCst);
        let sent = slot.tx.try_send(e);
        if sent.is_err() {
            let _ = slot.queued.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1));
        }
        match sent {
            Ok(()) => i += 1,
            Err(TrySendError::Full(TapEvent::Samples(b))) => {
                g[i].dropped.fetch_add(b.raw_ms.len() as u64, Ordering::Relaxed);
                counters.dropped_to_taps += b.raw_ms.len() as u64;
                i += 1;
            }
            Err(TrySendError::Full(TapEvent::Mark { .. })) => {
                g[i].dropped_events.fetch_add(1, Ordering::Relaxed);
                counters.dropped_events_to_taps += 1;
                i += 1;
            }
            Err(TrySendError::Disconnected(_)) => {
                g.remove(i);
            }
        }
    }
}

fn usable_sample_ms(reported: Option<f64>) -> Result<f64, (f64, f64)> {
    let Some(ms) = reported else { return Ok(SAMPLE_MS_TAKEN) };
    if SAMPLE_MS_USED.contains(&ms) {
        return Ok(ms);
    }
    let scaled = [ms / 1000.0, ms * 1000.0].into_iter().find(|v| SAMPLE_MS_USED.contains(v));
    Err((ms, scaled.unwrap_or(SAMPLE_MS_LENIENT)))
}

fn stale_bound(base: Duration, sample_ms: Option<f64>) -> Duration {
    let by_rate = sample_ms.map(|ms| Duration::from_secs_f64(ms * 25.0 / 1000.0)).unwrap_or(Duration::ZERO);
    base.max(by_rate)
}

fn client_changes(then: &[reply::ClientEntry], now: &[reply::ClientEntry]) -> (Vec<String>, Vec<String>) {
    let count = |v: &[reply::ClientEntry]| {
        let mut m: BTreeMap<String, usize> = BTreeMap::new();
        for c in v {
            *m.entry(c.address.clone()).or_default() += 1;
        }
        m
    };
    let (before, after) = (count(then), count(now));
    let mut joined = Vec::new();
    let mut left = Vec::new();
    for (addr, &n) in &after {
        for _ in before.get(addr).copied().unwrap_or(0)..n {
            joined.push(addr.clone());
        }
    }
    for (addr, &n) in &before {
        for _ in after.get(addr).copied().unwrap_or(0)..n {
            left.push(addr.clone());
        }
    }
    (joined, left)
}

fn is_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => v.is_loopback(),
        IpAddr::V6(v) => v.is_loopback() || v.to_ipv4_mapped().is_some_and(|m| m.is_loopback()),
    }
}

fn no_samples_advice(on_vc: bool) -> (&'static str, &'static str) {
    if on_vc {
        (
            "one of them gets every sample (InfoStream sends them all to one program). On a virtual controller that is usually RobotStudio's own connection, which takes InfoStream whenever the controller starts: Disconnect and Connect again, and this program leaving ends that hold. If a program on this PC is showing test signals (TuneMaster, RobotStudio's Signal Analyzer), close it first.",
            "No samples yet: another connection gets them all. On a virtual controller that is usually RobotStudio's own, which takes InfoStream whenever the controller starts: Disconnect and Connect again. If a program is showing test signals (TuneMaster, RobotStudio's Signal Analyzer), close it first.",
        )
    } else {
        (
            "the one that opened InfoStream first is getting every sample (InfoStream sends them all to that program), whether or not it shows them: a signal view (RobotStudio, TuneMaster), or a tool that opened InfoStream and sits idle. Close it, then connect again.",
            "No samples yet: another program connected to this controller opened InfoStream first and gets all of them, even if it shows nothing. Close it (or its signal view: RobotStudio, TuneMaster), then connect again.",
        )
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

fn describe_io(e: &std::io::Error) -> String {
    use std::io::ErrorKind::*;
    match e.kind() {
        ConnectionRefused => "the connection was refused (nothing is listening on that port)".into(),
        TimedOut | WouldBlock => "no answer (timed out): check the address, the cable, and that the controller is on".into(),
        ConnectionReset => "the controller reset the connection".into(),
        ConnectionAborted => "the connection was aborted".into(),
        HostUnreachable | NetworkUnreachable => "the network or host is unreachable from this PC".into(),
        _ => e.to_string(),
    }
}

fn reader_loop(mut stream: TcpStream, generation: u64, tx: Sender<Event>, queued: Arc<AtomicUsize>, closed: Arc<AtomicBool>) {
    let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
    let mut chunk = vec![0u8; 64 * 1024];
    let mut desynced = false;
    let _ = stream.set_read_timeout(Some(READER_WAKE));
    let mut peeked = [0u8; 1];
    let why = loop {
        match stream.peek(&mut peeked) {
            Ok(0) => break "the controller closed the connection".to_string(),
            Ok(_) => {}
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted) => continue,
            Err(e) => break describe_io(&e),
        }
        let n = match stream.read(&mut chunk) {
            Ok(0) => break "the controller closed the connection".to_string(),
            Ok(n) => n,
            Err(e) if matches!(e.kind(), std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => continue,
            Err(e) => break describe_io(&e),
        };
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > MAX_BUFFER {
            break format!("more than {} KiB arrived without a complete frame", MAX_BUFFER / 1024);
        }
        let mut pos = 0;
        let mut desync = None;
        loop {
            match wire::frame_at(&buf[pos..]) {
                FrameStatus::NeedMore => break,
                FrameStatus::Desync(d) => {
                    desync = Some(d);
                    break;
                }
                FrameStatus::Complete(len) => {
                    while queued.load(Ordering::Relaxed) > MAX_QUEUED_FRAMES {
                        if closed.load(Ordering::SeqCst) {
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    queued.fetch_add(1, Ordering::Relaxed);
                    if tx.send(Event::Frame { generation, bytes: buf[pos..pos + len].to_vec(), at: Instant::now() }).is_err() {
                        return;
                    }
                    pos += len;
                }
            }
        }
        if let Some(d) = desync {
            desynced = true;
            break format!("the byte stream lost frame sync: {d}");
        }
        buf.drain(..pos);
    };
    let _ = tx.send(Event::Closed { generation, why, desync: desynced });
}

pub fn wire_axis(axis: Axis) -> u8 {
    axis.wire()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn batch(n: usize) -> TapEvent {
        let key = ChannelKey::parse_id("4002/ROB_1/J1").unwrap();
        TapEvent::Samples(SampleBatch { key, kind: ValueKind::Float, raw_ms: vec![0; n], timeline_ms: vec![0; n], values: BatchValues::Number(vec![1.0; n]), arrived: SystemTime::now() })
    }

    #[test]
    fn a_recorder_too_slow_for_its_samples_still_gets_every_connection_mark() {
        let taps = Mutex::new(Vec::new());
        let (tap, slot) = new_tap(2);
        taps.lock().unwrap().push(slot);
        let mut counters = Counters::default();
        for _ in 0..5 {
            broadcast(&taps, batch(3), &mut counters);
        }
        let other = Target { host: "192.0.2.77".into(), port: 5515 };
        broadcast(&taps, TapEvent::Mark { wall: SystemTime::now(), mark: Mark::Connected { target: other, system_id: Some("{B}".into()) } }, &mut counters);
        assert_eq!((counters.dropped_to_taps, counters.dropped_events_to_taps), (9, 0), "three batches over the room, and no mark");
        let got: Vec<TapEvent> = std::iter::from_fn(|| tap.try_recv().ok()).collect();
        assert_eq!(got.len(), 3);
        assert!(matches!(got[2], TapEvent::Mark { mark: Mark::Connected { .. }, .. }), "the mark that closes a recording was dropped");
        broadcast(&taps, batch(3), &mut counters);
        assert!(matches!(tap.try_recv(), Ok(TapEvent::Samples(_))), "a drained queue takes samples again");
        assert_eq!(counters.dropped_to_taps, 9);
    }

    #[test]
    fn only_a_sample_time_a_controller_uses_is_taken() {
        assert_eq!(usable_sample_ms(Some(4.032)), Ok(4.032));
        assert_eq!(usable_sample_ms(Some(24.192)), Ok(24.192));
        assert_eq!(usable_sample_ms(None), Ok(SAMPLE_MS_TAKEN));
        assert_eq!(usable_sample_ms(Some(1e-14)), Err((1e-14, SAMPLE_MS_LENIENT)), "a tiny one sized the history to nothing");
        assert_eq!(usable_sample_ms(Some(f64::MAX)), Err((f64::MAX, SAMPLE_MS_LENIENT)));
        assert_eq!(usable_sample_ms(Some(24192.0)), Err((24192.0, 24.192)), "microseconds: a huge one kept a stopped stream live for ten minutes");
        assert!(matches!(usable_sample_ms(Some(0.004032)), Err((_, v)) if (v - 4.032).abs() < 1e-9), "seconds");
    }

    #[test]
    fn with_no_samples_a_remote_controllers_advice_names_an_idle_holder_too() {
        let (what, advice) = no_samples_advice(false);
        assert!(what.contains("opened InfoStream first") && what.contains("whether or not it shows them"), "{what}");
        assert!(advice.contains("opened InfoStream first") && advice.contains("even if it shows nothing"), "{advice}");
        let (what, advice) = no_samples_advice(true);
        assert!(what.contains("RobotStudio's own connection") && advice.contains("Disconnect and Connect again"));
    }
}
