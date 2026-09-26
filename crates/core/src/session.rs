//! The InfoStream session: one worker thread per [`Session`], owning the connection.
//!
//! ```text
//!   window / probe --requests--> worker thread <--frames-- reader thread <-- TCP
//!                  <--status---     |   answers every AYA the moment it is parsed
//!                                   +--> Store (history)  +--> taps (recorder, slow log)
//! ```
//!
//! The rules it keeps, each learned the hard way (proposal section 3):
//!
//! * **Read-only.** It sends only what [`crate::request`] can build.
//! * **Answer the keepalive**, echoing its ctrl values, before anything else.
//! * **Match replies by transaction.** Request txns are never 0 (the controller's
//!   unsolicited frames use 0) and never reused within a session. A StreamDefine
//!   stays pending for the whole session, so a late reply still maps the signal it
//!   belongs to.
//! * **Restart delivery after a change.** Measured on the RW6 VC (2026-09-25): a
//!   stream defined while streaming delivers nothing, and an accepted define or any
//!   undefine stops delivery for every stream, until StartStream is sent again (a
//!   refused define does not; a StartStream while streaming carries on without a
//!   gap). So every define or undefine on a started session is followed by one
//!   StartStream, whatever its reply, once no define is still waiting for one.
//! * **Never show another client's signal as ours.** Stream ids are controller-wide
//!   and a freed one is handed out again at once (measured: after another client's
//!   StreamUndefineAll, its defines got the ids this program was streaming on, and
//!   their samples came here). Every stream pausing at once without this program
//!   having changed anything, or a stream changing record type, means exactly that:
//!   the session stops before filing anything more, as quietly as for a stall.
//! * **Never show stale as live.** A channel with no sample inside its stale bound
//!   is reported stale; the window freezes and dims it.
//! * **Single tenancy (option A).** Before defining anything on a real controller
//!   with other RobAPI clients connected, it lists them and waits for an answer. If
//!   its streams stop delivering on a live session it says so and stops, and does
//!   not grab InfoStream back; and it closes that session quietly, because
//!   StopStream is controller-wide and "its" stream ids may by then belong to
//!   whoever took over.
//! * **Tear down on every exit path**: StopStream, StreamUndefine for each of its
//!   own ids, StreamDisconnect. Never StreamUndefineAll, except behind the explicit
//!   reset request.
//! * **Treat every byte as hostile.** Framing and decoding are bounded (see
//!   [`crate::wire`], [`crate::sample`]); a desync drops the connection.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::io::{Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use crate::log::LogBook;
use crate::reply::{self, Announce, Reply};
use crate::request::{self, Axis, Command, Define};
use crate::sample::{self, Defect, RecordValues, ValueKind};
use crate::store::{Channel, ChannelKey, Store};
use crate::timeline::{ClockEvent, Timeline};
use crate::wire::{self, service, Frame, FrameStatus};

/// The app's fixed channel limit (decision C2).
pub const MAX_CHANNELS: usize = 12;
/// The IRC5's RobAPI port. A virtual controller picks its own at every start.
pub const ROBAPI_PORT: u16 = 5515;
/// Receive buffer cap: twice the largest frame, so a legitimate frame always fits
/// with the next read behind it.
const MAX_BUFFER: usize = 2 * wire::MAX_FRAME as usize;
/// Frames the reader may queue ahead of the worker before it waits (backpressure).
const MAX_QUEUED_FRAMES: usize = 8192;
/// The reconnect ladder, the bridge's.
const LADDER: [u64; 5] = [1, 2, 5, 15, 30];
/// A pause in the samples that began within this many controller milliseconds of
/// the newest sample this program had when it last defined, undefined or started
/// streams is its own doing: the controller acts on a request a delivery latency
/// after the newest sample seen, never before it.
const OWN_CHANGE_MARGIN_MS: u64 = 1_000;
/// Another client's streams that deliver when this program's first do (its
/// leftovers) arrive in the same frames, or within one slow sample time.
const LEFTOVER_MARGIN_MS: u64 = 200;
/// A StartStream of this program's sent this soon before it began owing another
/// may be the one that ended the pause.
const OWN_START_SLACK: Duration = Duration::from_secs(1);
/// With no controller stamp to go by, a channel's first sample arriving this long
/// after its StartStream is not from that StartStream.
const FIRST_SAMPLE_WALL_MARGIN: Duration = Duration::from_millis(1500);

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

/// Timing knobs. The defaults are the product's; tests shorten them.
#[derive(Debug, Clone)]
pub struct Options {
    pub connect_timeout: Duration,
    pub handshake_timeout: Duration,
    pub reply_timeout: Duration,
    /// A channel with no sample for this long (or 25 sample times, if longer) is stale.
    pub stale_after: Duration,
    /// A started session that has streamed, then gets no sample from any of its
    /// streams for this long, has lost InfoStream: it stops.
    pub stall_after: Duration,
    pub teardown_wait: Duration,
    /// Seconds per rung of the reconnect ladder; the product uses [`LADDER`].
    pub ladder: Vec<Duration>,
    pub ask: AskPolicy,
    /// How many times in a row to reconnect when a reconnect gets no samples at
    /// all. After a network fault the controller holds the broken connection as the
    /// one it sends to until its keepalive times out (about 16 s), and a connection
    /// made meanwhile never gets a sample (measured); a later one does.
    pub orphan_retries: u32,
}

/// When to ask before taking InfoStream while other RobAPI clients are connected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskPolicy {
    /// On any controller not reached over loopback (B1, B2). The product's setting:
    /// a local virtual controller always has RobotStudio connected, and asking about
    /// it every time would teach people to click through the question.
    Remote,
    /// On every controller, loopback included (the tests, against the fake).
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
            teardown_wait: Duration::from_millis(1500),
            ladder: LADDER.iter().map(|&s| Duration::from_secs(s)).collect(),
            ask: AskPolicy::Remote,
            // On the ladder (1, 2, 5, 15, 30 s) plus the wait for samples, five
            // attempts span about a minute: well past the controller letting go.
            orphan_retries: 5,
        }
    }
}

/// Where the session is.
#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    Idle,
    Connecting,
    Handshaking,
    /// Other RobAPI clients are connected; waiting for the person to decide.
    AwaitingApproval,
    SettingUp,
    Streaming,
    /// The connection dropped after it had worked; trying again.
    Reconnecting { attempt: u32, retry_in: Duration },
    TearingDown,
    /// Stopped for a reason the person must see.
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

/// A channel's definition state.
#[derive(Debug, Clone, PartialEq)]
pub enum ChannelState {
    /// Not yet sent (not connected, or waiting for the setup to reach it).
    Waiting,
    /// Sent; no answer yet.
    Defining,
    /// Sent; no answer within the reply timeout. Still mapped if one comes.
    NoReply,
    Defined { stream: u32 },
    /// The controller said no. Not retried automatically: every refusal costs an
    /// entry in the controller's event log.
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
    /// Samples per second over the last second.
    pub rate: f64,
    pub gaps: u64,
    pub stale: bool,
}

/// Another RobAPI client in the handshake's list.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct OtherClient {
    pub address: String,
    pub attributes: Vec<(String, String)>,
    /// Same address as this PC's end of the connection: another program here.
    pub same_pc: bool,
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
    /// Sample records for streams this program did not define: another client's
    /// streams, or leftovers of one that crashed.
    pub foreign_records: u64,
    /// Sample frames or notices under a subscription id that is not ours. Counted,
    /// not dropped: every client gets the same subscription id (measured), so it
    /// cannot tell whose a sample is; the stream id and the takeover check do.
    pub foreign_subscription: u64,
    /// Which RobAPI service the sample frames came on (8 on the RW6 VC), and at what
    /// offset in the RAD the `protobuf` marker sat (16): Phase 0 check 1 on a real
    /// controller.
    pub sample_services: BTreeMap<u8, u64>,
    pub marker_offsets: BTreeMap<usize, u64>,
    pub unexpected_frames: u64,
    pub defects: BTreeMap<String, u64>,
    pub clock_resets: u64,
    /// Samples a recorder could not keep up with.
    pub dropped_to_taps: u64,
    pub dropped_events_to_taps: u64,
}

/// A snapshot of the session for the window.
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
    /// Timeline ms to chart seconds, and timeline ms to wall clock.
    pub timeline: Timeline,
    /// Something the person should do, while it applies (for the window's status
    /// line, not only the log).
    pub advice: Option<String>,
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
        }
    }
}

/// Samples as they arrive, for the recorder and the slow log.
#[derive(Debug, Clone)]
pub struct SampleBatch {
    pub key: ChannelKey,
    pub kind: ValueKind,
    /// The controller's stamps as sent (32-bit wraps undone), for `controller_ms`.
    pub raw_ms: Vec<u64>,
    /// The monotonic timeline the charts use.
    pub timeline_ms: Vec<i64>,
    pub values: BatchValues,
    /// When the frame carrying them arrived, on the wall clock: a recorder working
    /// through a backlog still anchors controller time to the right moment.
    pub arrived: SystemTime,
}

#[derive(Debug, Clone)]
pub enum BatchValues {
    Number(Vec<f64>),
    Text(Vec<String>),
}

/// Things a recording notes beside the samples.
#[derive(Debug, Clone)]
pub enum Mark {
    Connected { target: Target, system_id: Option<String> },
    Defined { key: ChannelKey, stream: u32, sample_ms: f64 },
    Lost { reason: String },
    /// The person disconnected (or chose another controller).
    Disconnected,
    /// The controller's clock went back (a restart) or jumped forward further than
    /// time passed; the timeline was rebased either way.
    ClockReset { from_raw: u64, to_raw: u64 },
}

#[derive(Debug, Clone)]
pub enum TapEvent {
    Samples(SampleBatch),
    Mark { wall: SystemTime, mark: Mark },
}

/// A subscriber's end. Bounded: a slow consumer loses events and the loss is
/// counted, never hidden and never allowed to stall the socket.
pub struct Tap {
    pub rx: Receiver<TapEvent>,
    /// Samples lost (not batches: a batch carries one or more).
    pub dropped: Arc<AtomicU64>,
    /// Marks lost.
    pub dropped_events: Arc<AtomicU64>,
}

struct TapSlot {
    tx: SyncSender<TapEvent>,
    dropped: Arc<AtomicU64>,
    dropped_events: Arc<AtomicU64>,
}

enum Request {
    Connect(Target),
    Disconnect,
    SetChannels(Vec<ChannelKey>),
    Answer(bool, Vec<String>),
    ResetInfoStream,
    Shutdown,
    /// Test-only: make the worker panic, to prove the panic path tears down.
    #[cfg(feature = "fake")]
    Crash,
}

enum Event {
    Request(Request),
    Frame { generation: u64, bytes: Vec<u8>, at: Instant },
    /// `desync`: the bytes stopped being RobAPI frames, as opposed to the socket
    /// closing or failing.
    Closed { generation: u64, why: String, desync: bool },
}

/// What the panic path needs to tear a session down without the worker's state.
#[derive(Default)]
struct Emergency {
    stream: Option<TcpStream>,
    host: String,
    own: Vec<u32>,
    next_txn: u16,
}

/// The handle. Dropping it tears the session down.
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
    /// Start the worker. `notify` is called whenever the status changes, so a
    /// window can repaint (it must be cheap and must not block).
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
        // The worker only goes away at shutdown; a request after that has no one to
        // act on it and nothing to undo.
        let _ = self.tx.send(Event::Request(r));
    }

    pub fn connect(&self, target: Target) {
        self.send(Request::Connect(target))
    }
    pub fn disconnect(&self) {
        self.send(Request::Disconnect)
    }
    /// The channels wanted, in order. At most [`MAX_CHANNELS`]; extras are ignored
    /// and logged. Duplicates are dropped.
    pub fn set_channels(&self, keys: Vec<ChannelKey>) {
        self.send(Request::SetChannels(keys))
    }
    /// The person's answer to the other-clients question, with the clients it was
    /// shown for (the status's `others` at the time). A yes counts only while those
    /// are still the clients in question: the connection can drop and come back with
    /// a different list between the question being drawn and the click.
    pub fn answer(&self, take_infostream: bool, shown: &[OtherClient]) {
        self.send(Request::Answer(take_infostream, shown.iter().map(|o| o.address.clone()).collect()))
    }
    /// StreamUndefineAll: removes EVERY client's streams, then redefines ours. The
    /// window must have asked first.
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
    pub fn log(&self) -> &Arc<LogBook> {
        &self.log
    }
    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    /// Receive every sample and mark from now on.
    pub fn tap(&self, capacity: usize) -> Tap {
        let (tx, rx) = mpsc::sync_channel(capacity.max(1));
        let dropped = Arc::new(AtomicU64::new(0));
        let dropped_events = Arc::new(AtomicU64::new(0));
        self.taps.lock().unwrap_or_else(|e| e.into_inner()).push(TapSlot { tx, dropped: dropped.clone(), dropped_events: dropped_events.clone() });
        Tap { rx, dropped, dropped_events }
    }

    /// Tear down and stop the worker, waiting at most `wait`.
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
    let mut txn = g.next_txn;
    if let Some(mut s) = g.stream.take() {
        let _ = s.set_write_timeout(Some(Duration::from_millis(300)));
        let mut next = || {
            txn = txn.wrapping_add(1).max(1);
            txn
        };
        let _ = s.write_all(&Command::StopStream.frame(next(), &host));
        for id in own {
            let _ = s.write_all(&Command::Undefine(id).frame(next(), &host));
        }
        let _ = s.write_all(&Command::StreamDisconnect.frame(next(), &host));
        std::thread::sleep(Duration::from_millis(200));
        let _ = s.shutdown(Shutdown::Both);
    }
}

// ---------------------------------------------------------------------------- worker

struct Conn {
    generation: u64,
    stream: TcpStream,
    peer: SocketAddr,
    local: SocketAddr,
    /// The controller's address as it goes in resource URLs.
    host: String,
    reader: Option<JoinHandle<()>>,
    /// Frames this connection's reader has queued and the worker not yet taken.
    queued: Arc<AtomicUsize>,
    /// Set before the reader is joined, so a reader waiting on backpressure gives
    /// up instead of waiting for a worker that is waiting for it.
    closed: Arc<AtomicBool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Hello,
    Approval,
    /// SetProtocol, StreamConnect, SUBSCRIBE sent; waiting for their replies.
    Preamble,
    /// Defines sent; waiting for their replies, then StartStream.
    Defining,
    Streaming,
    TearingDown,
}

#[derive(Debug, Clone)]
enum Pending {
    Preamble(&'static str),
    Subscribe,
    /// `void`: sent before a StreamUndefineAll that has since removed whatever it
    /// made; its reply is ignored, and the channel defined again.
    Define { key: ChannelKey, sent: Instant, reported: bool, void: bool },
    Undefine { stream: u32 },
    Start,
    Stop,
    UndefineAll,
    Disconnect,
}

struct Chan {
    key: ChannelKey,
    slot: u8,
    state: ChannelState,
    sample_ms: Option<f64>,
    kind: Option<ValueKind>,
    samples: u64,
    /// Samples in the current session only: the stall check needs to know which
    /// defined channels have ever delivered on this connection.
    session_samples: u64,
    last_arrival: Option<Instant>,
    /// When the controller confirmed this channel's stream: its stale clock starts
    /// here, not at an earlier StartStream it was not part of.
    defined_at: Option<Instant>,
    last_raw: Option<u64>,
    rate: f64,
    rate_mark: (Instant, u64),
    gaps: u64,
    stale_reported: bool,
    store: Option<Arc<Channel>>,
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
        }
    }
    fn stream(&self) -> Option<u32> {
        match self.state {
            ChannelState::Defined { stream } => Some(stream),
            _ => None,
        }
    }
}

/// What to do once a teardown finishes.
#[derive(Debug, Clone, PartialEq)]
enum After {
    Idle,
    Exit,
    Stopped(String),
    /// Connect again on the reconnect ladder, saying why.
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
    chans: Vec<Chan>,
    by_stream: HashMap<u32, usize>,
    subscription: Option<u32>,
    started: bool,
    restart_needed: bool,
    last_start: Option<Instant>,
    /// The "every sample may be going to another program" hint was given on this
    /// connection.
    told_no_samples: bool,
    advice: Option<String>,
    /// The newest controller stamp received on this connection, when its frame
    /// arrived, and the first stamp.
    newest_raw: Option<u64>,
    newest_at: Option<Instant>,
    first_raw: Option<u64>,
    /// Streams this program did not define that were already delivering when its
    /// own did (another client's leftovers): counted, not a takeover.
    known_foreign: BTreeSet<u32>,
    /// This program's own streams it has asked to undefine and not yet heard back
    /// about: their samples still in flight are neither a channel's nor foreign.
    retiring: BTreeSet<u32>,
    /// When this program started owing a StartStream (an accepted define of its own
    /// pauses delivery until one): only its own may end that pause.
    restart_owed_at: Option<Instant>,
    /// When any frame last arrived, samples or not.
    last_frame_at: Option<Instant>,
    /// This connection was opened by the reconnect ladder, not by the person.
    auto_reconnect: bool,
    /// Samples have arrived on an earlier connection to this target.
    delivered_before: bool,
    /// Reconnects in a row that got no samples at all.
    orphan_retries: u32,
    /// The controller's clock, as best known, when this program last did something
    /// that pauses or resumes delivery (`None`: before any sample arrived).
    own_change_at: Option<u64>,
    /// Requests that arrived while a switch of controller was tearing the old
    /// session down; handled, in order, once it is done.
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
    // Scratch buffers, reused across frames.
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
            orphan_retries: 0,
            own_change_at: None,
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
                    // Drain what is already queued before the timers, so a burst of
                    // frames is handled as one batch.
                    for _ in 0..256 {
                        match self.rx.try_recv() {
                            Ok(ev) => self.handle(ev),
                            Err(_) => break,
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                // Every handle is gone: nobody can ask for a teardown any more, so do
                // it now.
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
        // Short enough for the stale and stall checks and the teardown deadline;
        // the socket's frames wake the worker on their own.
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

    /// A frame from a reader. Only the current connection's frames count, and only
    /// they touch its queue counter: a frame an old reader queued before its
    /// connection was dropped must not unbalance the new one's.
    fn frame_event(&mut self, generation: u64, bytes: &[u8], at: Instant) {
        let Some(c) = &self.conn else { return };
        if c.generation != generation {
            return;
        }
        c.queued.fetch_sub(1, Ordering::Relaxed);
        self.frame(bytes, at);
    }

    // ------------------------------------------------------------------ requests

    fn request(&mut self, r: Request) {
        match r {
            Request::Connect(t) => {
                if self.conn.is_some() {
                    self.log.info(format!("Reconnecting to {t}."));
                    self.emit_mark(Mark::Disconnected);
                    self.begin_teardown(After::Idle);
                    self.finish_teardown_now();
                    // Closing the program while the old session was torn down.
                    if self.exit {
                        return;
                    }
                }
                if self.target.as_ref().is_some_and(|old| *old != t) {
                    self.fresh_start(&t);
                }
                self.target = Some(t);
                self.established = false;
                self.approved = None;
                self.rung = 0;
                self.retry_at = None;
                self.auto_reconnect = false;
                self.delivered_before = false;
                self.orphan_retries = 0;
                // A deliberate connect retries what the controller refused last time.
                for c in &mut self.chans {
                    if matches!(c.state, ChannelState::Refused { .. } | ChannelState::NoReply) {
                        c.state = ChannelState::Waiting;
                    }
                }
                self.open();
            }
            Request::Disconnect => {
                self.retry_at = None;
                if self.conn.is_some() {
                    self.emit_mark(Mark::Disconnected);
                    self.begin_teardown(After::Idle);
                } else {
                    self.set_phase(Phase::Idle);
                }
            }
            Request::SetChannels(keys) => self.set_channels(keys),
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
                    // Nothing was defined and no InfoStream session was opened: just close.
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
                // Whatever this program had, or had asked for and not heard back
                // about, is gone with everyone else's: a define still in flight was
                // handled before the StreamUndefineAll, so its stream went too.
                for p in self.pending.values_mut() {
                    if let Pending::Define { void, .. } = p {
                        *void = true;
                    }
                }
                // The reset is how the person frees the controller's channels, so
                // refusals are tried again, as on a deliberate connect.
                for c in &mut self.chans {
                    c.state = ChannelState::Waiting;
                }
                // Samples of the old streams still in flight are this program's, not
                // another client's.
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
        // Removed channels: undefine what is defined.
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
            // A define still in flight for it: its reply will find no channel and
            // undefine the stream it names.
        }
        // New channels.
        for k in wanted.iter() {
            if !self.chans.iter().any(|c| &c.key == k) {
                let used: BTreeSet<u8> = self.chans.iter().map(|c| c.slot).collect();
                let slot = (0..MAX_CHANNELS as u8).find(|s| !used.contains(s)).unwrap_or(0);
                self.chans.push(Chan::new(k.clone(), slot));
            }
        }
        // Keep the requested order.
        self.chans.sort_by_key(|c| wanted.iter().position(|k| k == &c.key).unwrap_or(usize::MAX));
        // Removing and reordering moved channels: the stream map points at
        // positions, so it is rebuilt from the channels' own states.
        self.reindex();
        if self.conn.is_some() && matches!(self.stage, Stage::Defining | Stage::Streaming) {
            self.define_waiting();
            // A removed channel may have been the last one setup was waiting for.
            self.check_defining_done();
            self.maybe_restart();
        }
        self.dirty = true;
    }

    fn reindex(&mut self) {
        self.by_stream = self.chans.iter().enumerate().filter_map(|(i, c)| c.stream().map(|s| (s, i))).collect();
    }

    /// A different controller: its clock, its streams and its history have nothing
    /// to do with the last one's, so the charts, the timeline and the counters start
    /// afresh rather than run one controller's data on into another's.
    fn fresh_start(&mut self, t: &Target) {
        self.store.clear();
        self.timeline = Timeline::new();
        self.counters = Counters::default();
        let now = Instant::now();
        for c in &mut self.chans {
            let (key, slot, state) = (c.key.clone(), c.slot, std::mem::replace(&mut c.state, ChannelState::Waiting));
            *c = Chan::new(key, slot);
            c.state = state;
            c.rate_mark = (now, 0);
        }
        self.log.info(format!("Connecting to a different controller ({t}): the charts start afresh."));
        self.dirty = true;
    }

    // ---------------------------------------------------------------- connection

    fn next_txn(&mut self) -> u16 {
        self.txn = self.txn.wrapping_add(1);
        if self.txn == 0 {
            self.txn = 1;
        }
        // Never reuse a txn that still has a reply outstanding.
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
        let mut stream = None;
        for a in &addrs {
            match TcpStream::connect_timeout(a, self.opt.connect_timeout) {
                Ok(s) => {
                    stream = Some((s, *a));
                    break;
                }
                Err(e) => last_err = describe_io(&e),
            }
        }
        let Some((stream, peer)) = stream else {
            self.connect_failed(&format!("cannot connect to {target}: {last_err}"));
            return;
        };
        let _ = stream.set_nodelay(true);
        // A controller that stops reading must not wedge this thread in a write.
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
        }
        self.pending.clear();
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
        self.advice = None;
        self.newest_raw = None;
        self.newest_at = None;
        self.first_raw = None;
        self.known_foreign.clear();
        self.retiring.clear();
        self.restart_owed_at = None;
        self.last_frame_at = None;
        self.own_change_at = None;
        self.had_samples = false;
        self.last_any_sample = None;
        self.announce = None;
        self.others.clear();
        self.timeline.session_start();
        self.connected_since = Some(Instant::now());
        self.streaming_since = None;
        self.hello_txn = 0;
        self.hello_txn = self.next_txn();
        self.stage = Stage::Hello;
        self.stage_deadline = Some(Instant::now() + self.opt.handshake_timeout);
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
                // The shutdown ends its blocking read at once.
                let _ = r.join();
            }
        }
        let mut e = self.emergency.lock().unwrap_or_else(|p| p.into_inner());
        e.stream = None;
        e.own.clear();
        drop(e);
        self.pending.clear();
        self.by_stream.clear();
        self.retiring.clear();
        for c in &mut self.chans {
            if !matches!(c.state, ChannelState::Refused { .. }) {
                c.state = ChannelState::Waiting;
            }
        }
        // With no connection there is no stage: left as it was, the setup's "all
        // defines answered" check would call a dead session streaming.
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
                // Handled through the event queue, like a read failure, so the state
                // change happens in one place.
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
            // Refreshed again when the reply comes: a slow controller acts on it
            // well after it was sent.
            self.own_change_at = self.controller_now();
        }
        self.pending.insert(txn, p);
        self.emergency.lock().unwrap_or_else(|e| e.into_inner()).next_txn = txn;
        self.write(&f);
        txn
    }

    /// The connection is gone (read error, peer closed, desync, write failure).
    fn lost(&mut self, why: &str, desync: bool) {
        let tearing = self.stage == Stage::TearingDown;
        let in_hello = self.stage == Stage::Hello;
        self.drop_conn();
        if tearing {
            // Expected: the controller closed after our StreamDisconnect.
            self.after_teardown_done();
            return;
        }
        self.emit_mark(Mark::Lost { reason: why.to_string() });
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

    // ---------------------------------------------------------------- the stages

    fn handshake_done(&mut self, frame: &Frame<'_>) {
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
            others.push(OtherClient { address: c.address.clone(), attributes: c.attributes.clone(), same_pc: c.address == local_ip });
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
        self.announce = Some(announce);
        self.others = others;
        let loopback = peer.ip().is_loopback();
        let unapproved: Vec<&OtherClient> = self.others.iter().filter(|o| !self.approved.as_ref().is_some_and(|a| a.contains(&o.address))).collect();
        let ask = match self.opt.ask {
            AskPolicy::Remote => !loopback,
            AskPolicy::Always => true,
            AskPolicy::Never => false,
        };
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

    fn begin_preamble(&mut self) {
        self.stage = Stage::Preamble;
        self.stage_deadline = Some(Instant::now() + self.opt.reply_timeout);
        self.set_phase(Phase::SettingUp);
        self.send_cmd(Command::SetProtocolProtobuf, Pending::Preamble("SetProtocol"));
        self.send_cmd(Command::StreamConnect, Pending::Preamble("StreamConnect"));
        let txn = self.next_txn();
        let host = self.conn.as_ref().map(|c| c.host.clone()).unwrap_or_default();
        self.pending.insert(txn, Pending::Subscribe);
        let f = request::subscribe(txn, &host);
        self.write(&f);
    }

    fn preamble_done(&mut self) {
        self.stage = Stage::Defining;
        // Each define keeps its own reply timer (the tick checks them in this stage
        // and while streaming), so a channel added late in setup is timed too.
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
            // An accepted define pauses every stream until StartStream, and the
            // reply may never come: the restart is owed from the moment it is sent.
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
        self.rung = 0;
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

    /// This program has done something that pauses delivery on a started session:
    /// it owes one StartStream.
    fn owe_restart(&mut self) {
        if !self.started {
            return;
        }
        if !self.restart_needed {
            self.restart_owed_at = Some(Instant::now());
        }
        self.restart_needed = true;
    }

    /// Give back one of this program's own streams.
    fn undefine_own(&mut self, s: u32) {
        self.retiring.insert(s);
        self.send_cmd(Command::Undefine(s), Pending::Undefine { stream: s });
        self.owe_restart();
    }

    /// After a define or undefine on a started session: one StartStream once no
    /// define is in flight (measured: delivery stays stopped until then).
    fn maybe_restart(&mut self) {
        if self.stage != Stage::Streaming || self.defines_in_flight() {
            return;
        }
        let any = self.chans.iter().any(|c| c.stream().is_some());
        if any && (self.restart_needed || !self.started) {
            self.start_stream();
        } else if !any {
            self.restart_needed = false;
            self.restart_owed_at = None;
        }
    }

    /// The controller's clock now, as best known: the newest stamp plus the time
    /// since it arrived (while delivery is paused the newest stamp falls behind).
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
            // Already on the way out; a stronger request (exit) wins.
            if after == After::Exit {
                self.after_teardown = After::Exit;
            }
            return;
        }
        let opened = matches!(self.stage, Stage::Preamble | Stage::Defining | Stage::Streaming);
        self.stage = Stage::TearingDown;
        self.after_teardown = after;
        self.set_phase(Phase::TearingDown);
        // Defines still in flight stay pending: if one of their replies names a
        // stream, it is undefined as it lands (define_reply) rather than left behind.
        // A voided one's stream went with a StreamUndefineAll: nothing to wait for.
        self.pending.retain(|_, p| matches!(p, Pending::Define { void: false, .. }));
        if opened {
            // Pipelined, then one wait for all the replies: the VC has answered
            // StreamUndefine anywhere from 10 ms to over a second.
            self.send_cmd(Command::StopStream, Pending::Stop);
            let own: Vec<u32> = self.chans.iter().filter_map(|c| c.stream()).collect();
            for s in own {
                self.send_cmd(Command::Undefine(s), Pending::Undefine { stream: s });
            }
            self.send_cmd(Command::StreamDisconnect, Pending::Disconnect);
            self.teardown_deadline = Some(Instant::now() + self.opt.teardown_wait);
        } else {
            // Nothing was opened on the controller: just close.
            self.teardown_deadline = Some(Instant::now());
        }
    }

    /// For exits that cannot wait for the event loop: pump the replies here.
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
                // Kept, in order, for when the switch is done.
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

    /// Streams stopped arriving on a live session: InfoStream was very likely taken
    /// by another client.
    fn stalled(&mut self) {
        let secs = self.opt.stall_after.as_secs_f64();
        // Nothing at all since the last sample, keepalives included, fits a stalled
        // network as well as a takeover (the VC sends keepalives rarely while
        // streaming, so their absence proves nothing): say both.
        let silent = self.last_frame_at.is_none_or(|f| self.last_any_sample.is_some_and(|s| f <= s));
        let reason = if silent {
            format!(
                "Nothing at all from the controller for {secs:.0} s on a live connection: another tool, such as RobotStudio or TuneMaster, may have taken InfoStream, or the network or the controller stalled. Connect again when it is free."
            )
        } else {
            format!(
                "The controller stopped sending samples ({secs:.0} s with none on a live connection). Another tool, such as RobotStudio or TuneMaster, has most likely taken InfoStream. Connect again when it is free."
            )
        };
        self.quit_quietly(reason, "samples stopped");
    }

    /// Another client has taken InfoStream, and what arrives next may be its
    /// signals under this program's stream ids.
    fn taken_over(&mut self, what: &str) {
        let reason = format!(
            "{}. Another tool, such as RobotStudio or TuneMaster, has most likely taken InfoStream, so what arrives now could be its signals under these channels' names: stopped before showing any of it. Connect again when it is free.",
            capitalize(what)
        );
        self.quit_quietly(reason, "another client took InfoStream");
    }

    /// Say why and stop; do not take InfoStream back, and send neither the
    /// controller-wide StopStream nor an undefine for ids that may by now be someone
    /// else's. StreamDisconnect only, then close.
    fn quit_quietly(&mut self, reason: String, mark: &str) {
        self.log.error(reason.clone());
        self.leave_quietly(After::Stopped(reason), mark);
    }

    fn leave_quietly(&mut self, after: After, mark: &str) {
        self.emit_mark(Mark::Lost { reason: mark.into() });
        // Nothing more is filed under those ids, and the panic path must not
        // undefine them either.
        self.by_stream.clear();
        self.emergency.lock().unwrap_or_else(|e| e.into_inner()).own.clear();
        self.stage = Stage::TearingDown;
        // A shutdown already on its way out stays one.
        if self.after_teardown != After::Exit {
            self.after_teardown = after;
        }
        self.set_phase(Phase::TearingDown);
        self.pending.clear();
        self.send_cmd(Command::StreamDisconnect, Pending::Disconnect);
        self.teardown_deadline = Some(Instant::now() + self.opt.teardown_wait.min(Duration::from_millis(500)));
    }

    // -------------------------------------------------------------------- timers

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
                        self.stage_deadline = None;
                        self.drop_conn();
                        let why = format!("{t} accepted the connection but did not answer the RobAPI handshake within {:.0} s; it is probably not a controller's RobAPI port", self.opt.handshake_timeout.as_secs_f64());
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

        // Defines that are overdue, in setup and while streaming alike.
        if self.conn.is_some() && matches!(self.stage, Stage::Defining | Stage::Streaming) {
            let overdue = self.pending.values().any(|p| matches!(p, Pending::Define { sent, reported: false, void: false, .. } if now.duration_since(*sent) >= self.opt.reply_timeout));
            if overdue {
                self.defines_timed_out(now);
            }
            // Whatever path emptied the set of defines in flight: setup moves on, and
            // a restart that is owed is sent.
            self.check_defining_done();
            self.maybe_restart();
        }

        // Stale channels, and the stall.
        if self.conn.is_some() && self.stage == Stage::Streaming && self.started {
            for c in &mut self.chans {
                // String signals are events, not a stream: on the RW6 VC (2026-09-25)
                // 221, 222, 225 and 9872 each sent their text (the work object, the
                // tool, a program position) once and then only on change. Quiet is
                // their normal state, so they are never called stale.
                if c.stream().is_none() || c.kind == Some(ValueKind::String) {
                    continue;
                }
                let bound = stale_bound(self.opt.stale_after, c.sample_ms);
                // Never delivered: timed from the later of its define and the last
                // StartStream (its delivery only begins at a StartStream after it).
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
            let own_change_pending = self.restart_needed
                || self.defines_in_flight()
                || self.pending.values().any(|p| matches!(p, Pending::Start | Pending::Undefine { .. } | Pending::UndefineAll));
            let nothing_yet = !self.had_samples
                && !own_change_pending
                && self.chans.iter().any(|c| c.stream().is_some())
                && self.last_start.is_some_and(|s| now.duration_since(s) > self.opt.stall_after);
            // A reconnect that gets nothing, where the connection before it did: after
            // a network fault the controller keeps sending to the broken connection
            // until its keepalive times out, and never to one made meanwhile
            // (measured). Connect again, a bounded number of times.
            if nothing_yet && self.auto_reconnect && self.delivered_before {
                self.orphan_retries += 1;
                if self.orphan_retries > self.opt.orphan_retries {
                    self.quit_quietly(
                        format!(
                            "Connected again {} times, but no samples arrive. Another tool, such as RobotStudio or TuneMaster, may have taken InfoStream while the connection was down. Connect again when it is free.",
                            self.opt.orphan_retries + 1
                        ),
                        "no samples after reconnecting",
                    );
                } else {
                    let why = "connected again, but no samples arrive: the controller is most likely still holding the connection that broke (it lets go of it after about 16 s)";
                    self.log.warn(format!("{}.", capitalize(why)));
                    self.leave_quietly(After::Retry(why.into()), "no samples after reconnecting");
                }
                return;
            }
            // Nothing at all on this connection, with other programs connected: the
            // likeliest reason is that one of them subscribed first and gets every
            // sample. Said once; not a stop, since a signal can also just be silent.
            if nothing_yet && !self.told_no_samples && !self.others.is_empty() {
                self.told_no_samples = true;
                self.log.warn(format!(
                    "No samples at all in {:.0} s. {} other program(s) are connected to this controller. If one of them is showing test signals, it is getting every sample (InfoStream sends them all to one program): close its signal view (RobotStudio, TuneMaster), then connect again. If this program's own earlier connection broke, the controller lets go of it after about 16 s: connect again then.",
                    self.opt.stall_after.as_secs_f64(),
                    self.others.len()
                ));
                self.advice = Some("No samples yet. If another program connected to this controller is showing test signals, it is getting all of them: close its signal view (RobotStudio, TuneMaster), then connect again.".into());
                self.dirty = true;
            }
            // Only a channel that has delivered on this connection can say the feed
            // stopped; one that never delivered (a silent signal) says nothing. And
            // while this program's own define, undefine or restart is in play, the
            // pause is its own.
            let delivering = self.chans.iter().any(|c| c.stream().is_some() && c.session_samples > 0 && c.kind != Some(ValueKind::String));
            if delivering && !own_change_pending {
                let since = match (self.last_any_sample, self.last_start) {
                    (Some(a), Some(b)) => a.max(b),
                    (a, b) => a.or(b).unwrap_or(now),
                };
                if now.duration_since(since) > self.opt.stall_after {
                    self.stalled();
                    return;
                }
            }
        }

        // Rates, once a second.
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

    // -------------------------------------------------------------------- frames

    fn frame(&mut self, bytes: &[u8], at: Instant) {
        let Some(frame) = Frame::parse(bytes) else {
            // The reader only forwards complete frames; this cannot happen, and if
            // it does the stream is not what we think it is.
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

        // The keepalive first, before anything else can delay it.
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

        let txn = frame.txn();
        let mut handled = false;
        for rad in frame.rads() {
            if let Some(offset) = sample::find_marker(rad.data) {
                *self.counters.sample_services.entry(frame.service()).or_default() += 1;
                *self.counters.marker_offsets.entry(offset).or_default() += 1;
                self.samples(rad.data, at);
                handled = true;
                if self.stage == Stage::TearingDown {
                    // Taken over: nothing more from this frame either.
                    return;
                }
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
        // The controller has acted on this program's change by now, however long it
        // took: a pause that began up to here is still its own doing.
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
            // Its stream, if it made one, went with the StreamUndefineAll, and the id
            // may already be handed out again: nothing to map and nothing to undefine.
            Pending::Define { void: true, .. } => {}
            Pending::Define { key, reported, .. } => self.define_reply(key, reported, r),
            Pending::Undefine { stream } => {
                // Its samples came before this reply; any from now on under that id
                // are whoever gets it next.
                self.retiring.remove(&stream);
                if r.is_failure() {
                    self.log.warn(format!("Undefining stream {stream}: {}", r.summary()));
                }
            }
            Pending::Start | Pending::Stop | Pending::Disconnect => {
                // Delivery restarts now, not when the StartStream was sent (a slow
                // controller acts on it much later): the stall clock starts here.
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
        // Another reply for a channel that already has its stream (it was removed
        // and added back while the first define was still in flight): it keeps that
        // stream, whatever this reply says. An extra stream is given back; a refusal
        // changes nothing (it must not orphan the stream the channel holds).
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
            // The channel was removed while its define was in flight.
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
                    // The controller handed out an id one of this program's streams
                    // still holds: someone removed that stream behind its back, and
                    // what arrives under the id is no longer that channel's.
                    let msg = format!("the controller gave {key} stream {s}, which {} was still using, so another client has removed this program's streams", self.chans[other].key);
                    self.taken_over(&msg);
                    return;
                }
                let sample_ms = r.sample_time_ms().unwrap_or(4.032);
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
        // Checked for the whole frame before any of it is filed.
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
                // One of this program's own streams on its way out, sent before its
                // undefine was acted on.
                if self.retiring.contains(&rec.stream) {
                    continue;
                }
                self.counters.foreign_records += 1;
                // takeover_in let it through: delivering since this program's own
                // start or change, so a leftover, not someone starting now.
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
            // The map is rebuilt whenever channels move; a stale entry here is a
            // bug, handled by dropping the record rather than misfiling it.
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
                // The latest, not the largest: after a 32-bit wrap the largest would
                // stay ahead of every stamp to come.
                self.newest_raw = Some(st);
                self.newest_at = Some(at);
                self.first_raw.get_or_insert(st);
                let (u, t, ev) = self.timeline.map(st, at);
                if ev != ClockEvent::None {
                    // Stamps before a wrap or a reset say nothing about the ones after
                    // it: the takeover check starts again from here.
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
            if let Some(ch) = &c.store {
                let mut ring = ch.lock();
                ring.kind = Some(rec.kind);
                match &values {
                    BatchValues::Number(v) => {
                        for (t, x) in tl.iter().zip(v) {
                            ring.push(*t, *x);
                        }
                    }
                    BatchValues::Text(v) => {
                        // Charted as a hole; the text is shown in the table.
                        for t in &tl {
                            ring.push(*t, f64::NAN);
                        }
                        ring.last_text = v.last().cloned();
                    }
                }
            }
            c.samples += n as u64;
            c.session_samples += n as u64;
            c.last_arrival = Some(at);
            if c.stale_reported {
                c.stale_reported = false;
                let k = c.key.clone();
                self.log.info(format!("{k}: samples arriving again."));
            }
            self.counters.samples += n as u64;
            self.had_samples = true;
            self.delivered_before = true;
            self.orphan_retries = 0;
            self.advice = None;
            self.last_any_sample = Some(at);
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

    /// Whether this frame shows that another client has taken InfoStream, and how.
    ///
    /// Stream ids are controller-wide and reused at once, so once another client has
    /// removed this program's streams (StreamUndefineAll, as the bridge does on
    /// connect) and defined its own, its samples arrive here under this program's
    /// ids, looking live. Two things give it away (measured on the RW6 VC,
    /// 2026-09-25, where a 1.9 s takeover showed both):
    ///
    /// * a stream changing record type, which no stream does;
    /// * every stream pausing at once, then resuming, with this program having
    ///   changed nothing. Removing and defining streams pauses delivery until
    ///   StartStream, and the VC otherwise delivered 1.8 million samples over ten
    ///   minutes without missing one. A pause in one stream while others carry on is
    ///   only a gap in that channel.
    ///
    /// And some say another client has just started streaming here, which ends in
    /// the same place (and leaves it with nothing, since every sample comes here):
    ///
    /// * samples for a stream this program did not define, first arriving mid-way.
    ///   Another client's leftovers deliver from this program's own start, and are
    ///   only counted; string events are left out, as they only come on a change;
    /// * a channel that has not delivered since its define getting its first sample
    ///   long after this program's StartStream (its own come right after it): the
    ///   one check that covers a channel with no history to compare against;
    /// * samples resuming after a pause this program's own define caused, before
    ///   it sent the StartStream that alone should end it.
    ///
    /// Blind spots: a takeover faster than two samples that keeps type and rate on
    /// ids that were all this program's; one whose pause starts within about a
    /// second of this program's own change; and string channels whose id is reused
    /// for another client's strings (the VC reuses text ids too).
    fn takeover_in(&self, records: &[sample::Record], at: Instant) -> Option<String> {
        // Only a live session that is setting up or streaming is judged; one on its
        // way out is not (and a shutdown must stay a shutdown).
        if self.conn.is_none() || !matches!(self.stage, Stage::Defining | Stage::Streaming) {
            return None;
        }
        let streaming = self.stage == Stage::Streaming && self.started;
        // While this program's own define, undefine or restart is in play, a pause
        // is its own doing.
        let settled = streaming && !self.restart_needed && !self.defines_in_flight();
        // A pause this program's own define caused, with no StartStream of its own
        // sent since (or just before): only someone else's can end it.
        let pause_is_ours_to_end =
            streaming && self.restart_needed && self.restart_owed_at.is_some_and(|owed| self.last_start.is_none_or(|s| s + OWN_START_SLACK < owed));
        // Within the delivery latency of this program's own last change, or with
        // its very first samples: what arrives then followed from what it did.
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
                // Nothing from it since its define: its own first samples follow this
                // program's StartStream at once.
                let late = match (self.own_change_at, self.first_raw) {
                    (Some(s), _) => first > s.saturating_add(OWN_CHANGE_MARGIN_MS),
                    (None, Some(f)) => first > f.saturating_add(OWN_CHANGE_MARGIN_MS),
                    // No stamp to compare with at all: the wall clock, from the later
                    // of its define and the last StartStream.
                    (None, None) => c.defined_at.max(self.last_start).is_some_and(|s| at.saturating_duration_since(s) > FIRST_SAMPLE_WALL_MARGIN),
                };
                if settled && late {
                    return Some(format!("{} sent its first sample long after this program started it, as a stream another client has just started does", c.key));
                }
                continue;
            };
            let Some(ms) = c.sample_ms else { continue };
            // Back (a restart, a wrap) is the timeline's business; forward by about
            // one sample is normal.
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
            // This program's own define, undefine or restart, acted on as the pause
            // began.
            if self.own_change_at.is_some_and(|s| prev <= s.saturating_add(OWN_CHANGE_MARGIN_MS)) {
                continue;
            }
            // Did any other stream, fast enough to have shown it, deliver well inside
            // the hole (not just one tick out of step at its start)? Then only this
            // one paused.
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

    // ------------------------------------------------------------------ publishing

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
        match g[i].tx.try_send(e) {
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

fn stale_bound(base: Duration, sample_ms: Option<f64>) -> Duration {
    let by_rate = sample_ms.map(|ms| Duration::from_secs_f64(ms * 25.0 / 1000.0)).unwrap_or(Duration::ZERO);
    base.max(by_rate)
}

fn is_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => v.is_loopback(),
        IpAddr::V6(v) => v.is_loopback() || v.to_ipv4_mapped().is_some_and(|m| m.is_loopback()),
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

/// The socket reader: frames in, nothing else. It never blocks the worker, and the
/// worker never waits on it except to join it after shutting its socket down.
fn reader_loop(mut stream: TcpStream, generation: u64, tx: Sender<Event>, queued: Arc<AtomicUsize>, closed: Arc<AtomicBool>) {
    let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
    let mut chunk = vec![0u8; 64 * 1024];
    let mut desynced = false;
    let why = loop {
        let n = match stream.read(&mut chunk) {
            Ok(0) => break "the controller closed the connection".to_string(),
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
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
                    // Backpressure: a worker far behind makes the reader wait, which
                    // makes TCP push back, rather than letting memory grow.
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
            // There is no resync marker in this protocol; guessing at a boundary
            // could turn garbage into samples.
            desynced = true;
            break format!("the byte stream lost frame sync: {d}");
        }
        buf.drain(..pos);
    };
    let _ = tx.send(Event::Closed { generation, why, desync: desynced });
}

/// Hint for the window: which one-based axis a key would read on the wire.
pub fn wire_axis(axis: Axis) -> u8 {
    axis.wire()
}
