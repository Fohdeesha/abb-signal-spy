//! Recording to disk.
//!
//! A recording is a folder (decision C3):
//!
//! ```text
//! 2026-09-25_14-03-07 dc link dip/
//!   data.csv         controller_ms,channel,value        one row per sample (full rate,
//!                                                       and "save the last N seconds")
//!   slow.csv         controller_ms,channel,count,mean,min,max   one row per channel per
//!                                                       interval (slow logging)
//!   recording.json   what was recorded, from where, and what happened meanwhile
//! ```
//!
//! `controller_ms` is the controller's own clock in milliseconds (its uptime on a
//! virtual controller). It only goes backwards if the controller restarted during
//! the recording, and `recording.json` lists every such reset. `channel` is an id
//! like `4002/ROB_1/2` (signal / mechanical unit / one-based axis) that
//! `recording.json` expands into a name, units and a description.
//!
//! Loading one in Python:
//!
//! ```python
//! import json, pandas as pd
//! meta = json.load(open("recording.json"))
//! df = pd.read_csv("data.csv")
//! wide = df.pivot_table(index="controller_ms", columns="channel", values="value")
//! ```
//!
//! The recorder writes from its own thread off a bounded queue: a slow disk can
//! lose samples (counted, shown, and written into `recording.json`), but it can
//! never stall the socket. `recording.json` is rewritten atomically every few
//! seconds, so a recording cut short by a crash is still described; `complete`
//! says whether it was closed properly.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufRead, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use crate::sample::ValueKind;
use crate::session::{BatchValues, Mark, Session, Tap, TapEvent};
use crate::store::{ChannelKey, Store};
use crate::timeline::Timeline;
use crate::util::{local_stamp, wall_iso};

pub const FORMAT: &str = "abb-signal-spy-recording";
/// 2 (2026-09-27): channel ids name the joint (`4002/ROB_1/J2`, was `4002/ROB_1/2`),
/// and every anchor says the first row it maps.
pub const VERSION: u32 = 2;
/// Events the recorder may fall behind by before samples are lost.
pub const QUEUE: usize = 65_536;
const FLUSH_EVERY: Duration = Duration::from_secs(1);
const SIDECAR_EVERY: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Every sample.
    Full,
    /// Count, mean, min and max per interval.
    Slow,
    /// The live history's last N seconds.
    Snapshot,
}

/// What the window knows about a channel, beyond its key.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ChannelInfo {
    pub name: String,
    pub units: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ChannelEntry {
    pub id: String,
    pub signal: u32,
    pub unit: String,
    pub axis: u8,
    pub name: String,
    pub units: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub stream_id: Option<u32>,
    #[serde(default)]
    pub sample_ms: Option<f64>,
    #[serde(default, rename = "type")]
    pub value_type: Option<ValueKind>,
    #[serde(default)]
    pub samples: u64,
    #[serde(default)]
    pub first_controller_ms: Option<i64>,
    #[serde(default)]
    pub last_controller_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EventEntry {
    pub utc: String,
    pub kind: String,
    pub text: String,
    #[serde(default)]
    pub controller_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Anchor {
    pub controller_ms: i64,
    pub utc: String,
    /// The first data row (counted from 0, header not counted) this anchor maps; it
    /// applies until the next anchor's row. Absent in format 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Meta {
    pub format: String,
    pub version: u32,
    pub kind: Kind,
    pub app: String,
    pub started_utc: String,
    #[serde(default)]
    pub ended_utc: Option<String>,
    pub complete: bool,
    #[serde(default)]
    pub controller: String,
    #[serde(default)]
    pub system_id: Option<String>,
    #[serde(default)]
    pub robotware: Option<String>,
    #[serde(default)]
    pub interval_ms: Option<u32>,
    #[serde(default)]
    pub label: String,
    pub channels: Vec<ChannelEntry>,
    /// The wall clock at controller times, one per connection: `controller_ms` to
    /// UTC is `utc + (t - controller_ms)` from the nearest earlier anchor.
    #[serde(default)]
    pub anchors: Vec<Anchor>,
    #[serde(default)]
    pub events: Vec<EventEntry>,
    #[serde(default)]
    pub markers: Vec<EventEntry>,
    pub rows_written: u64,
    /// Samples the recorder could not keep up with. Nonzero means the data has holes.
    pub samples_lost: u64,
    /// Connection events (connected, lost, clock resets) lost the same way: the
    /// `events` list is then incomplete. Every loss re-anchors the wall clock.
    #[serde(default)]
    pub events_lost: u64,
    #[serde(default)]
    pub notes: String,
    /// The derived channels shown while recording, as last set (a review computes
    /// them again from the recorded inputs; their changes are in `events`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub derived: Vec<crate::derived::Derived>,
}

impl Meta {
    fn new(kind: Kind, label: &str, controller: &str, system_id: Option<String>) -> Meta {
        Meta {
            format: FORMAT.into(),
            version: VERSION,
            kind,
            app: format!("ABB Signal Spy {}", env!("CARGO_PKG_VERSION")),
            started_utc: wall_iso(SystemTime::now()),
            ended_utc: None,
            complete: false,
            controller: controller.into(),
            system_id,
            robotware: None,
            interval_ms: None,
            label: label.into(),
            channels: Vec::new(),
            anchors: Vec::new(),
            events: Vec::new(),
            markers: Vec::new(),
            rows_written: 0,
            samples_lost: 0,
            events_lost: 0,
            notes: String::new(),
            derived: Vec::new(),
        }
    }
}

fn entry(key: &ChannelKey, info: Option<&ChannelInfo>) -> ChannelEntry {
    let info = info.cloned().unwrap_or_default();
    ChannelEntry {
        id: key.id(),
        signal: key.signal,
        unit: key.unit.to_string(),
        axis: key.axis.one_based(),
        name: info.name,
        units: info.units,
        description: info.description,
        stream_id: None,
        sample_ms: None,
        value_type: None,
        samples: 0,
        first_controller_ms: None,
        last_controller_ms: None,
    }
}

/// A new, unused folder under `base` named for now and the label.
pub fn new_folder(base: &Path, label: &str) -> Result<PathBuf, String> {
    // Cut to length first, then trimmed: Windows drops a trailing space or dot when
    // it creates a folder, and the files would then be written under a name that
    // does not exist.
    let label: String = label.chars().map(|c| if c.is_alphanumeric() || " -_.".contains(c) { c } else { '_' }).take(60).collect();
    let label = label.trim_matches(|c: char| c == ' ' || c == '.').to_string();
    // The local clock: the one the window shows and a person looks for a recording by
    // (recording.json keeps exact UTC).
    let now = local_stamp(SystemTime::now());
    let stem = if label.is_empty() { now } else { format!("{now} {label}") };
    std::fs::create_dir_all(base).map_err(|e| format!("cannot create {}: {e}", base.display()))?;
    for i in 1..1000 {
        let name = if i == 1 { stem.clone() } else { format!("{stem} ({i})") };
        let p = base.join(name);
        match std::fs::create_dir(&p) {
            Ok(()) => return Ok(p),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("cannot create {}: {e}", p.display())),
        }
    }
    Err(format!("no free folder name under {}", base.display()))
}

fn write_meta(dir: &Path, meta: &Meta) -> Result<(), String> {
    let body = serde_json::to_string_pretty(meta).map_err(|e| e.to_string())? + "\n";
    let tmp = dir.join("recording.json.tmp");
    let fin = dir.join("recording.json");
    // On disk before the rename: after a power cut the renamed file could otherwise
    // be empty, and the whole recording then unreadable.
    let write = || -> std::io::Result<()> {
        let mut f = File::create(&tmp)?;
        f.write_all(body.as_bytes())?;
        f.sync_all()
    };
    write().map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &fin).map_err(|e| format!("cannot replace {}: {e}", fin.display()))
}

/// `write_meta`, tried a few times over about `patience`: on Windows the replace
/// fails while another program (a sync client such as OneDrive, a virus scanner, a
/// previewer) has the file open, which is usually brief.
fn write_meta_patiently(dir: &Path, meta: &Meta, patience: Duration) -> Result<(), String> {
    let end = Instant::now() + patience;
    loop {
        match write_meta(dir, meta) {
            Ok(()) => return Ok(()),
            Err(e) if Instant::now() >= end => return Err(e),
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// CSV text for a string sample: always quoted, quotes doubled.
fn csv_text(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// A number as the controller sent it: an f32 prints in its shortest exact form, an
/// int as an integer.
fn csv_number(v: f64, kind: ValueKind) -> String {
    match kind {
        ValueKind::Int if v.is_finite() => format!("{}", v as i64),
        _ => {
            let f = v as f32;
            if f.is_nan() {
                "NaN".into()
            } else if f.is_infinite() {
                if f > 0.0 { "inf".into() } else { "-inf".into() }
            } else {
                format!("{f}")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RecState {
    Recording,
    Finished,
    /// Closed properly by the recorder itself, for the reason given (the controller
    /// behind the address changed).
    Ended(String),
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct RecStatus {
    pub state: RecState,
    pub kind: Kind,
    pub dir: PathBuf,
    pub rows: u64,
    pub lost: u64,
    pub started: Instant,
    pub bytes: u64,
    /// Something the person should know that has not stopped the recording (the
    /// description file could not be rewritten just now).
    pub warning: Option<String>,
}

enum Cmd {
    Marker { label: String, controller_ms: Option<i64>, wall: SystemTime },
    Info(ChannelKey, ChannelInfo),
    Derived { defs: Vec<crate::derived::Derived>, change: Option<String>, wall: SystemTime },
    Stop,
}

/// A running recording (full rate or slow). Stop it to close it properly; dropping
/// it also closes it.
pub struct Recorder {
    tx: Sender<Cmd>,
    status: Arc<Mutex<RecStatus>>,
    thread: Option<JoinHandle<()>>,
}

impl Recorder {
    /// Start recording every sample the session receives from now on, into a new
    /// folder under `base`. `interval_ms` makes it a slow log instead.
    pub fn start(
        session: &Session,
        base: &Path,
        label: &str,
        interval_ms: Option<u32>,
        infos: &HashMap<ChannelKey, ChannelInfo>,
    ) -> Result<Recorder, String> {
        let (controller, system_id) = {
            let st = session.status();
            (st.target.as_ref().map(|t| t.to_string()).unwrap_or_default(), st.announce.as_ref().and_then(|a| a.system_id.clone()))
        };
        let tap = session.tap(QUEUE);
        Recorder::start_with(tap, base, label, interval_ms, infos, &controller, system_id, session_defined(session))
    }

    #[allow(clippy::too_many_arguments)]
    fn start_with(
        tap: Tap,
        base: &Path,
        label: &str,
        interval_ms: Option<u32>,
        infos: &HashMap<ChannelKey, ChannelInfo>,
        controller: &str,
        system_id: Option<String>,
        defined: Vec<Defined>,
    ) -> Result<Recorder, String> {
        if let Some(i) = interval_ms
            && !(10..=3_600_000).contains(&i) {
                return Err(format!("a slow-log interval of {i} ms is outside 10 ms .. 1 h"));
            }
        let kind = if interval_ms.is_some() { Kind::Slow } else { Kind::Full };
        let dir = new_folder(base, label)?;
        let file_name = if kind == Kind::Slow { "slow.csv" } else { "data.csv" };
        let path = dir.join(file_name);
        let file = File::create(&path).map_err(|e| format!("cannot create {}: {e}", path.display()))?;
        let mut out = BufWriter::with_capacity(256 * 1024, file);
        let header = if kind == Kind::Slow { "controller_ms,channel,count,mean,min,max\n" } else { "controller_ms,channel,value\n" };
        out.write_all(header.as_bytes()).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        let mut meta = Meta::new(kind, label, controller, system_id);
        meta.interval_ms = interval_ms;
        // Channels already streaming when the recording starts are listed up front,
        // with what the controller said about them.
        for (key, stream, ms, vt) in defined {
            let mut e = entry(&key, infos.get(&key));
            e.stream_id = stream;
            e.sample_ms = ms;
            e.value_type = vt;
            meta.channels.push(e);
        }
        write_meta_patiently(&dir, &meta, Duration::from_millis(500))?;
        let status = Arc::new(Mutex::new(RecStatus { state: RecState::Recording, kind, dir: dir.clone(), rows: 0, lost: 0, started: Instant::now(), bytes: header.len() as u64, warning: None }));
        let (tx, rx) = mpsc::channel();
        let infos = infos.clone();
        let st2 = status.clone();
        let thread = std::thread::Builder::new()
            .name("recorder".into())
            .spawn(move || {
                let mut w = Writer { dir, out, meta, infos, tap, rx, status: st2, slow: HashMap::new(), interval: interval_ms.map(i64::from), bytes: header.len() as u64, need_anchor: true, segment_break: false, ended: None, seen_lost: (0, 0) };
                w.run();
            })
            .map_err(|e| format!("cannot start the recorder thread: {e}"))?;
        Ok(Recorder { tx, status, thread: Some(thread) })
    }

    /// A marker at `controller_ms` (the controller's clock at the moment it was
    /// placed; the newest sample written so far if `None`).
    pub fn marker(&self, label: &str, controller_ms: Option<i64>) {
        let _ = self.tx.send(Cmd::Marker { label: label.to_string(), controller_ms, wall: SystemTime::now() });
    }

    /// Name, units and description for a channel added after the recording began.
    pub fn info(&self, key: ChannelKey, info: ChannelInfo) {
        let _ = self.tx.send(Cmd::Info(key, info));
    }

    /// The derived channels now shown, and what changed (kept as an event).
    pub fn derived(&self, defs: Vec<crate::derived::Derived>, change: Option<String>) {
        let _ = self.tx.send(Cmd::Derived { defs, change, wall: SystemTime::now() });
    }

    pub fn status(&self) -> RecStatus {
        self.lock().clone()
    }

    fn lock(&self) -> MutexGuard<'_, RecStatus> {
        self.status.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Close the recording and wait for the files to be written.
    pub fn stop(mut self) -> RecStatus {
        self.finish();
        self.status()
    }

    fn finish(&mut self) {
        let _ = self.tx.send(Cmd::Stop);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.finish();
    }
}

/// A channel already streaming when a recording starts: key, stream id, sample time
/// and record type, as the controller reported them.
type Defined = (ChannelKey, Option<u32>, Option<f64>, Option<ValueKind>);

fn session_defined(session: &Session) -> Vec<Defined> {
    session
        .status()
        .channels
        .iter()
        .filter_map(|c| match c.state {
            crate::session::ChannelState::Defined { stream } => Some((c.key.clone(), Some(stream), c.sample_ms, c.kind)),
            _ => None,
        })
        .collect()
}

#[derive(Default)]
struct Bucket {
    start: i64,
    count: u64,
    sum: f64,
    min: f64,
    max: f64,
}

struct Writer {
    dir: PathBuf,
    out: BufWriter<File>,
    meta: Meta,
    infos: HashMap<ChannelKey, ChannelInfo>,
    tap: Tap,
    rx: Receiver<Cmd>,
    status: Arc<Mutex<RecStatus>>,
    slow: HashMap<String, Bucket>,
    interval: Option<i64>,
    bytes: u64,
    /// The next sample anchors controller time to the wall clock: the first one,
    /// the first after every reconnect (a restarted controller's clock is new), and
    /// the first after any loss (the lost events may have included a reconnect).
    need_anchor: bool,
    /// The clock the open slow-log intervals were counted on has ended (a reconnect
    /// or a reset): they are written before the next anchor, so its row does not
    /// claim them.
    segment_break: bool,
    /// Why the recorder closed itself: the controller behind the address changed, and
    /// nothing more may be filed as this recording's.
    ended: Option<String>,
    /// The tap's loss counters (samples, events) last looked at.
    seen_lost: (u64, u64),
}

impl Writer {
    fn run(&mut self) {
        let mut last_flush = Instant::now();
        let mut last_meta = Instant::now();
        let mut last_ctrl: Option<i64> = None;
        let result: Result<(), String> = (|| {
            loop {
                match self.rx.try_recv() {
                    Ok(Cmd::Stop) | Err(mpsc::TryRecvError::Disconnected) => break,
                    Ok(Cmd::Marker { label, controller_ms, wall }) => {
                        let e = EventEntry { utc: wall_iso(wall), kind: "marker".into(), text: label, controller_ms: controller_ms.or(last_ctrl) };
                        self.meta.markers.push(e);
                        self.save_meta();
                    }
                    Ok(Cmd::Info(key, info)) => {
                        if let Some(c) = self.meta.channels.iter_mut().find(|c| c.id == key.id()) {
                            c.name = info.name.clone();
                            c.units = info.units.clone();
                            c.description = info.description.clone();
                        }
                        self.infos.insert(key, info);
                    }
                    Ok(Cmd::Derived { defs, change, wall }) => {
                        // A target or plateau changed (not a derived channel added or
                        // removed): a review computes everything with the last one,
                        // and says so.
                        let setting = defs.iter().any(|d| self.meta.derived.iter().any(|o| o.same(d) && o != d));
                        self.meta.derived = defs;
                        if let Some(text) = change {
                            let kind = if setting { "derived-setting" } else { "derived" };
                            self.meta.events.push(EventEntry { utc: wall_iso(wall), kind: kind.into(), text, controller_ms: last_ctrl });
                        }
                        self.save_meta();
                    }
                    Err(mpsc::TryRecvError::Empty) => {}
                }
                match self.tap.rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(TapEvent::Samples(b)) => {
                        self.note_losses();
                        if let Some(&t) = b.raw_ms.last() {
                            last_ctrl = Some(t as i64);
                        }
                        self.samples(b)?;
                    }
                    Ok(TapEvent::Mark { wall, mark }) => self.mark(wall, mark),
                    Err(RecvTimeoutError::Timeout) => {}
                    // The session is gone; nothing more will come.
                    Err(RecvTimeoutError::Disconnected) => break,
                }
                if self.ended.is_some() {
                    break;
                }
                if last_flush.elapsed() >= FLUSH_EVERY {
                    last_flush = Instant::now();
                    self.note_losses();
                    self.out.flush().map_err(|e| format!("writing the recording failed: {e}"))?;
                    let mut s = self.status.lock().unwrap_or_else(|e| e.into_inner());
                    s.rows = self.meta.rows_written;
                    s.lost = self.meta.samples_lost;
                    s.bytes = self.bytes;
                }
                if last_meta.elapsed() >= SIDECAR_EVERY {
                    last_meta = Instant::now();
                    self.save_meta();
                }
            }
            // Write what was already queued when the stop came, and no more: the
            // session keeps feeding the tap until this thread lets go of it, so
            // "until empty" could chase a live feed for as long as it runs. After
            // the controller changed, what is queued is the other one's.
            for _ in 0..QUEUE {
                if self.ended.is_some() {
                    break;
                }
                let Ok(ev) = self.tap.rx.try_recv() else { break };
                match ev {
                    TapEvent::Samples(b) => {
                        self.note_losses();
                        self.samples(b)?
                    }
                    TapEvent::Mark { wall, mark } => self.mark(wall, mark),
                }
            }
            self.flush_slow()?;
            self.out.flush().map_err(|e| format!("writing the recording failed: {e}"))?;
            Ok(())
        })();
        self.note_losses();
        self.meta.ended_utc = Some(wall_iso(SystemTime::now()));
        self.meta.complete = result.is_ok();
        let meta_result = write_meta_patiently(&self.dir, &self.meta, Duration::from_secs(2))
            .map_err(|e| format!("{e}. The samples in the CSV file are complete; recording.json is from before the end"));
        let mut s = self.status.lock().unwrap_or_else(|e| e.into_inner());
        s.rows = self.meta.rows_written;
        s.lost = self.meta.samples_lost;
        s.bytes = self.bytes;
        s.state = match (result, meta_result) {
            (Ok(()), Ok(())) => match self.ended.take() {
                Some(why) => RecState::Ended(why),
                None => RecState::Finished,
            },
            (Err(e), _) | (_, Err(e)) => RecState::Failed(e),
        };
    }

    /// Rewrite recording.json. A failure does not end the recording (the CSV file
    /// is the data; the description is rewritten every few seconds anyway): it is
    /// shown, and cleared by the next write that works.
    fn save_meta(&mut self) {
        let r = write_meta_patiently(&self.dir, &self.meta, Duration::from_millis(200));
        let mut s = self.status.lock().unwrap_or_else(|e| e.into_inner());
        s.warning = r.err().map(|e| format!("{e}; still recording, and trying again"));
    }

    /// Take in the tap's loss counters. Lost events may have included a reconnect,
    /// so the next sample anchors the wall clock afresh.
    fn note_losses(&mut self) {
        let now = (self.tap.dropped.load(Ordering::Relaxed), self.tap.dropped_events.load(Ordering::Relaxed));
        if now != self.seen_lost {
            self.seen_lost = now;
            self.meta.samples_lost = now.0;
            self.meta.events_lost = now.1;
            self.need_anchor = true;
        }
    }

    fn channel(&mut self, key: &ChannelKey) -> &mut ChannelEntry {
        let id = key.id();
        if let Some(i) = self.meta.channels.iter().position(|c| c.id == id) {
            return &mut self.meta.channels[i];
        }
        let e = entry(key, self.infos.get(key));
        self.meta.channels.push(e);
        self.meta.channels.last_mut().unwrap()
    }

    fn mark(&mut self, wall: SystemTime, mark: Mark) {
        let utc = wall_iso(wall);
        let (kind, text) = match mark {
            Mark::Connected { target, system_id } => {
                self.need_anchor = true;
                self.segment_break = true;
                let target = target.to_string();
                // Another controller: a cable moved to the next robot at the same
                // service address, or a reconnect that reached a different one. Its
                // samples must not continue this recording under the same channel ids.
                let other = match (&self.meta.system_id, &system_id) {
                    (Some(was), Some(now)) if was != now => Some(format!("the controller at {target} is a different one (system {now}; this recording is of system {was})")),
                    _ if !self.meta.controller.is_empty() && self.meta.controller != target => Some(format!("the program connected to {target}; this recording is of {}", self.meta.controller)),
                    _ => None,
                };
                if let Some(why) = other {
                    self.meta.notes = format!("{}Closed when {why}.", if self.meta.notes.is_empty() { String::new() } else { format!("{} ", self.meta.notes) });
                    self.ended = Some(format!("{why}. The recording was closed there"));
                }
                if self.meta.system_id.is_none() {
                    self.meta.system_id = system_id.clone();
                }
                if self.meta.controller.is_empty() {
                    self.meta.controller = target.clone();
                }
                ("connected", format!("connected to {target}{}", system_id.map(|s| format!(" (system {s})")).unwrap_or_default()))
            }
            Mark::Defined { key, stream, sample_ms } => {
                let c = self.channel(&key);
                c.stream_id = Some(stream);
                c.sample_ms = Some(sample_ms);
                ("defined", format!("{key} is stream {stream} ({sample_ms} ms)"))
            }
            Mark::Lost { reason } => ("lost", reason),
            Mark::Disconnected => ("disconnected", "disconnected".into()),
            Mark::ClockReset { from_raw, to_raw } => {
                // A clock that restarted or jumped: the old anchor no longer maps it.
                self.need_anchor = true;
                self.segment_break = true;
                if to_raw < from_raw {
                    ("clock_reset", format!("controller clock went back from {from_raw} to {to_raw} ms (a restart)"))
                } else {
                    ("clock_reset", format!("controller clock jumped forward from {from_raw} to {to_raw} ms, further than the time that passed"))
                }
            }
        };
        self.meta.events.push(EventEntry { utc, kind: kind.into(), text, controller_ms: None });
    }

    fn samples(&mut self, b: crate::session::SampleBatch) -> Result<(), String> {
        let n = b.raw_ms.len();
        if n == 0 {
            return Ok(());
        }
        let id = b.key.id();
        if self.need_anchor && std::mem::take(&mut self.segment_break) {
            self.flush_slow()?;
        }
        {
            let first = b.raw_ms[0] as i64;
            let last = b.raw_ms[n - 1] as i64;
            let c = self.channel(&b.key);
            c.value_type = Some(b.kind);
            c.samples += n as u64;
            c.last_controller_ms = Some(last);
            c.first_controller_ms.get_or_insert(first);
            if self.need_anchor {
                self.need_anchor = false;
                // When the frame arrived, not when this thread got to it: with a
                // backlog those differ by the backlog.
                self.meta.anchors.push(Anchor { controller_ms: first, utc: wall_iso(b.arrived), row: Some(self.meta.rows_written) });
            }
        }
        if let Some(interval) = self.interval {
            let BatchValues::Number(values) = &b.values else { return Ok(()) };
            for (i, &v) in values.iter().enumerate() {
                if !v.is_finite() {
                    continue;
                }
                let t = b.raw_ms[i] as i64;
                let start = t - t.rem_euclid(interval);
                let bucket = self.slow.entry(id.clone()).or_insert_with(|| Bucket { start, count: 0, sum: 0.0, min: f64::INFINITY, max: f64::NEG_INFINITY });
                if bucket.start != start {
                    // A new interval (or a controller restart): the old one is done.
                    let done = std::mem::replace(bucket, Bucket { start, count: 0, sum: 0.0, min: f64::INFINITY, max: f64::NEG_INFINITY });
                    write_bucket(&mut self.out, &id, &done, &mut self.bytes, &mut self.meta.rows_written)?;
                }
                let bucket = self.slow.get_mut(&id).unwrap();
                bucket.count += 1;
                bucket.sum += v;
                bucket.min = bucket.min.min(v);
                bucket.max = bucket.max.max(v);
            }
            return Ok(());
        }
        let mut line = String::with_capacity(48);
        for i in 0..n {
            line.clear();
            let value = match &b.values {
                BatchValues::Number(v) => csv_number(v[i], b.kind),
                BatchValues::Text(v) => csv_text(&v[i]),
            };
            line.push_str(&b.raw_ms[i].to_string());
            line.push(',');
            line.push_str(&id);
            line.push(',');
            line.push_str(&value);
            line.push('\n');
            self.out.write_all(line.as_bytes()).map_err(|e| format!("writing the recording failed: {e}"))?;
            self.bytes += line.len() as u64;
        }
        self.meta.rows_written += n as u64;
        Ok(())
    }

    fn flush_slow(&mut self) -> Result<(), String> {
        let mut ids: Vec<String> = self.slow.keys().cloned().collect();
        ids.sort();
        for id in ids {
            if let Some(b) = self.slow.remove(&id) {
                write_bucket(&mut self.out, &id, &b, &mut self.bytes, &mut self.meta.rows_written)?;
            }
        }
        Ok(())
    }
}

fn write_bucket(out: &mut BufWriter<File>, id: &str, b: &Bucket, bytes: &mut u64, rows: &mut u64) -> Result<(), String> {
    if b.count == 0 {
        return Ok(());
    }
    let line = format!("{},{},{},{},{},{}\n", b.start, id, b.count, b.sum / b.count as f64, b.min, b.max);
    out.write_all(line.as_bytes()).map_err(|e| format!("writing the slow log failed: {e}"))?;
    *bytes += line.len() as u64;
    *rows += 1;
    Ok(())
}

/// "Save the last N seconds": write the live history's last `seconds` for these
/// channels into a new folder, with the derived channels shown. Rows are in time
/// order across channels.
#[allow(clippy::too_many_arguments)]
pub fn write_snapshot(
    base: &Path,
    label: &str,
    store: &Store,
    timeline: &Timeline,
    keys: &[ChannelKey],
    infos: &HashMap<ChannelKey, ChannelInfo>,
    seconds: f64,
    controller: &str,
    system_id: Option<String>,
    derived: &[crate::derived::Derived],
) -> Result<(PathBuf, u64), String> {
    if !(seconds.is_finite() && seconds > 0.0) {
        return Err("the length to save must be a positive number of seconds".into());
    }
    let newest = keys.iter().filter_map(|k| store.get(k)).filter_map(|c| c.lock().last().map(|(t, _)| t)).max().ok_or("there is no history to save yet")?;
    let from = newest - (seconds * 1000.0).round() as i64;
    // Copy out under each channel's lock briefly, then write without holding any.
    let mut rows: Vec<(i64, usize, f64)> = Vec::new();
    let mut meta = Meta::new(Kind::Snapshot, label, controller, system_id);
    meta.derived = derived.to_vec();
    for (idx, k) in keys.iter().enumerate() {
        let mut e = entry(k, infos.get(k));
        if let Some(c) = store.get(k) {
            let r = c.lock();
            e.sample_ms = Some(r.sample_ms);
            e.value_type = r.kind;
            if r.kind == Some(ValueKind::String) {
                // The live history keeps only when a string event came, not its text.
                meta.notes = "String signals are not kept in the live history; record them with REC to keep their text.".into();
                meta.channels.push(e);
                continue;
            }
            let before = rows.len();
            rows.extend(r.range(from, newest + 1).map(|(t, v)| (t, idx, v)));
            e.samples = (rows.len() - before) as u64;
        }
        meta.channels.push(e);
    }
    rows.sort_by_key(|&(t, idx, _)| (t, idx));
    let dir = new_folder(base, label)?;
    let path = dir.join("data.csv");
    let file = File::create(&path).map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    let mut out = BufWriter::with_capacity(256 * 1024, file);
    let io = |e: std::io::Error| format!("writing {} failed: {e}", path.display());
    out.write_all(b"controller_ms,channel,value\n").map_err(io)?;
    let ids: Vec<String> = keys.iter().map(|k| k.id()).collect();
    let mut segment: Option<i64> = None;
    for (row, &(t, idx, v)) in rows.iter().enumerate() {
        let kind = meta.channels[idx].value_type.unwrap_or(ValueKind::Float);
        let c = timeline.controller_ms(t);
        // One anchor per clock segment, at its first row: across a restart the
        // controller's clock starts again, and each side needs its own mapping. The
        // chart timeline runs on with the wall clock across the gap, so it gives both.
        let seg = timeline.segment_start(t);
        if segment != Some(seg) {
            segment = Some(seg);
            if let Some(wall) = timeline.wall(t) {
                meta.anchors.push(Anchor { controller_ms: c, utc: wall_iso(wall), row: Some(row as u64) });
            }
        }
        writeln!(out, "{c},{},{}", ids[idx], csv_number(v, kind)).map_err(io)?;
        let ch = &mut meta.channels[idx];
        ch.first_controller_ms.get_or_insert(c);
        ch.last_controller_ms = Some(c);
    }
    out.flush().map_err(io)?;
    meta.rows_written = rows.len() as u64;
    meta.complete = true;
    meta.ended_utc = Some(wall_iso(SystemTime::now()));
    write_meta(&dir, &meta)?;
    Ok((dir, rows.len() as u64))
}

/// A recording read back: its description and every channel's samples.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub meta: Meta,
    /// channel id -> (controller_ms, value); for a slow log, the mean.
    pub data: BTreeMap<String, Vec<(i64, f64)>>,
    /// channel id -> (controller_ms, text), for string signals.
    pub text: BTreeMap<String, Vec<(i64, String)>>,
    /// Rows that did not parse, with the line each started on (the first 20).
    pub bad_rows: Vec<usize>,
}

/// The recordings in `base`'s subfolders, newest first (by when they started). A
/// folder without a readable description is not a recording and is left out.
pub fn list(base: &Path) -> Vec<(PathBuf, Meta)> {
    let Ok(entries) = std::fs::read_dir(base) else { return Vec::new() };
    let mut out: Vec<(PathBuf, Meta)> = entries.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_dir()).filter_map(|p| read_meta(&p).ok().map(|m| (p, m))).collect();
    out.sort_by(|a, b| b.1.started_utc.cmp(&a.1.started_utc).then_with(|| b.0.cmp(&a.0)));
    out
}

/// A recording's description, checked, with every channel under the id this program
/// writes now, whichever form the file has (format 1 wrote the bare axis number); an
/// id that does not parse stays as it is.
pub fn read_meta(dir: &Path) -> Result<Meta, String> {
    let meta_text = std::fs::read_to_string(dir.join("recording.json")).map_err(|e| format!("cannot read recording.json: {e}"))?;
    let mut meta: Meta = serde_json::from_str(crate::util::strip_bom(&meta_text)).map_err(|e| format!("recording.json is not a recording description ({e})"))?;
    if meta.format != FORMAT {
        return Err("recording.json is not an ABB Signal Spy recording".into());
    }
    if meta.version > VERSION {
        return Err(format!("recording format {} is newer than this program ({VERSION})", meta.version));
    }
    for c in &mut meta.channels {
        c.id = ChannelKey::parse_id(&c.id).map(|k| k.id()).unwrap_or_else(|| c.id.clone());
    }
    Ok(meta)
}

/// Read a recording folder. Rows that do not parse are skipped and reported, not
/// fatal: a recording cut short by a crash ends in a partial line. The CSV is read
/// as CSV (a quoted string may hold commas, quotes and line breaks), a row at a
/// time.
pub fn read(dir: &Path) -> Result<Loaded, String> {
    let meta = read_meta(dir)?;
    let mut ids: HashMap<String, String> = HashMap::new();
    let mut canonical = |id: &str| -> String {
        ids.entry(id.to_string()).or_insert_with(|| ChannelKey::parse_id(id).map(|k| k.id()).unwrap_or_else(|| id.to_string())).clone()
    };
    let file = if meta.kind == Kind::Slow { "slow.csv" } else { "data.csv" };
    let f = File::open(dir.join(file)).map_err(|e| format!("cannot read {file}: {e}"))?;
    let mut rows = CsvRows { r: std::io::BufReader::with_capacity(256 * 1024, f), line: 0, error: None };
    let mut data: BTreeMap<String, Vec<(i64, f64)>> = BTreeMap::new();
    let mut text: BTreeMap<String, Vec<(i64, String)>> = BTreeMap::new();
    let mut bad = Vec::new();
    let mut header = true;
    let want = if meta.kind == Kind::Slow { 6 } else { 3 };
    while let Some(row) = rows.next_row() {
        let (line, fields) = match row {
            Ok(r) => r,
            Err(line) => {
                if bad.len() < 20 {
                    bad.push(line);
                }
                continue;
            }
        };
        if std::mem::take(&mut header) {
            continue;
        }
        let parsed = (fields.len() == want).then_some(()).and_then(|()| Some((fields[0].0.parse::<i64>().ok()?, &fields[1].0)));
        let Some((t, ch)) = parsed else {
            if bad.len() < 20 {
                bad.push(line);
            }
            continue;
        };
        let (v, quoted) = if meta.kind == Kind::Slow { (&fields[3].0, false) } else { (&fields[2].0, fields[2].1) };
        let ch = canonical(ch);
        if quoted {
            text.entry(ch).or_default().push((t, v.clone()));
        } else {
            data.entry(ch).or_default().push((t, v.parse::<f64>().unwrap_or(f64::NAN)));
        }
    }
    if let Some(e) = rows.error {
        return Err(format!("{file}: {e}"));
    }
    Ok(Loaded { meta, data, text, bad_rows: bad })
}

/// RFC 4180 rows from a reader: fields split on commas, a quoted field may hold
/// commas, doubled quotes and line breaks.
pub(crate) struct CsvRows<R: std::io::BufRead> {
    pub(crate) r: R,
    pub(crate) line: usize,
    /// A read failed (a share gone, a bad sector): the rows end there, and the
    /// reason is kept for the caller rather than retried forever.
    pub(crate) error: Option<String>,
}

/// Longer than any row this program writes by far; bounds what an unterminated
/// quote can make it gather.
const MAX_ROW: usize = 1 << 20;

/// A row: the line it started on, and each field with whether it was quoted.
pub(crate) type CsvRow = (usize, Vec<(String, bool)>);

impl<R: std::io::BufRead> CsvRows<R> {
    /// The next row, `Err(line)` for one that is malformed, `None` at the end.
    pub(crate) fn next_row(&mut self) -> Option<Result<CsvRow, usize>> {
        if self.error.is_some() {
            return None;
        }
        let start = self.line + 1;
        let mut fields: Vec<(String, bool)> = Vec::new();
        let mut field = String::new();
        let mut quoted = false;
        let mut in_quotes = false;
        let mut bytes = Vec::new();
        let mut row_bytes = 0usize;
        loop {
            bytes.clear();
            // Bounded: a line with no end (a damaged or foreign file) is read no
            // further than MAX_ROW, then skipped to its end.
            let room = (MAX_ROW + 1).saturating_sub(row_bytes);
            match (&mut self.r).take(room as u64).read_until(b'\n', &mut bytes) {
                Ok(0) if self.line + 1 == start => return None,
                // The file ends inside a row: cut short.
                Ok(0) => return Some(Err(start)),
                Ok(_) => self.line += 1,
                Err(e) => {
                    self.error = Some(format!("reading line {start} failed: {e}"));
                    return None;
                }
            }
            row_bytes += bytes.len();
            if row_bytes > MAX_ROW {
                if bytes.last() != Some(&b'\n') {
                    self.skip_line();
                }
                return Some(Err(start));
            }
            let text = String::from_utf8_lossy(&bytes);
            let mut chars = text.chars().peekable();
            while let Some(c) = chars.next() {
                if in_quotes {
                    if c == '"' {
                        if chars.peek() == Some(&'"') {
                            chars.next();
                            field.push('"');
                        } else {
                            in_quotes = false;
                        }
                    } else {
                        field.push(c);
                    }
                    continue;
                }
                match c {
                    '"' if field.is_empty() && !quoted => {
                        in_quotes = true;
                        quoted = true;
                    }
                    ',' => fields.push((std::mem::take(&mut field), std::mem::replace(&mut quoted, false))),
                    '\r' if chars.peek() == Some(&'\n') => {}
                    '\n' => {}
                    c => field.push(c),
                }
            }
            if !in_quotes {
                fields.push((field, quoted));
                return Some(Ok((start, fields)));
            }
        }
    }

    /// Discard the rest of an over-long line, a buffer at a time.
    fn skip_line(&mut self) {
        loop {
            let (n, done) = match self.r.fill_buf() {
                Ok([]) => return,
                Ok(buf) => match buf.iter().position(|&b| b == b'\n') {
                    Some(i) => (i + 1, true),
                    None => (buf.len(), false),
                },
                Err(e) => {
                    self.error = Some(format!("reading line {} failed: {e}", self.line + 1));
                    return;
                }
            };
            self.r.consume(n);
            if done {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_print_as_sent() {
        // The shortest text that reads back as the same f32: exact, and no longer.
        assert_eq!(csv_number(f64::from(0.942_707_96_f32), ValueKind::Float), "0.94270796");
        assert_eq!("0.94270796".parse::<f32>().unwrap(), 0.942_707_96_f32);
        assert_eq!(csv_number(356.7_f32 as f64, ValueKind::Float), "356.7");
        assert_eq!(csv_number(-1.0, ValueKind::Int), "-1");
        assert_eq!(csv_number(f64::NAN, ValueKind::Float), "NaN");
        assert_eq!(csv_text("a \"b\", c"), "\"a \"\"b\"\", c\"");
    }

    #[test]
    fn folder_names_are_safe_and_unique() {
        let base = std::env::temp_dir().join(format!("spy-rec-test-{}", std::process::id()));
        let a = new_folder(&base, "dc link: dip/1?").unwrap();
        let b = new_folder(&base, "dc link: dip/1?").unwrap();
        assert_ne!(a, b);
        let name = a.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.ends_with("dc link_ dip_1_"), "{name}");
        // Cut at 60 characters right after a space: Windows would drop the space
        // from the folder and the files would go to a name that does not exist.
        let long = format!("{} b", "a".repeat(59));
        let c = new_folder(&base, &long).unwrap();
        let name = c.file_name().unwrap().to_string_lossy().to_string();
        assert!(!name.ends_with(' ') && !name.ends_with('.'), "{name:?}");
        std::fs::write(c.join("data.csv"), "x").unwrap();
        // Named on the local clock, the one the window shows and a person looks for
        // the recording by (discriminating only off UTC).
        let before = crate::util::local_stamp(SystemTime::now());
        let d = new_folder(&base, "").unwrap();
        let after = crate::util::local_stamp(SystemTime::now());
        let name = d.file_name().unwrap().to_string_lossy().to_string();
        assert!(name == before || name == after, "{name} is not {before} (local time)");
        let _ = std::fs::remove_dir_all(&base);
    }

    fn temp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("spy-rec-unit-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    fn manual_tap(capacity: usize) -> (mpsc::SyncSender<TapEvent>, Tap) {
        let (tx, rx) = mpsc::sync_channel(capacity);
        (tx, Tap { rx, dropped: Arc::new(std::sync::atomic::AtomicU64::new(0)), dropped_events: Arc::new(std::sync::atomic::AtomicU64::new(0)) })
    }

    fn batch(t: u64, n: usize) -> TapEvent {
        let key = ChannelKey::parse_id("4002/ROB_1/J1").unwrap();
        TapEvent::Samples(crate::session::SampleBatch {
            key,
            kind: ValueKind::Float,
            raw_ms: (0..n as u64).map(|i| t + 4 * i).collect(),
            timeline_ms: (0..n as i64).map(|i| t as i64 + 4 * i).collect(),
            values: BatchValues::Number(vec![1.5; n]),
            arrived: SystemTime::now(),
        })
    }

    #[test]
    fn stopping_writes_the_backlog_but_does_not_chase_a_live_feed() {
        let base = temp("chase");
        let (tx, tap) = manual_tap(QUEUE);
        let rec = Recorder::start_with(tap, &base, "", None, &HashMap::new(), "test", None, Vec::new()).unwrap();
        // A feed that keeps coming faster than the disk takes it.
        let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let feeder = {
            let running = running.clone();
            std::thread::spawn(move || {
                let mut t = 0;
                while running.load(Ordering::Relaxed) {
                    if tx.send(batch(t, 4)).is_err() {
                        break;
                    }
                    t += 16;
                }
            })
        };
        std::thread::sleep(Duration::from_millis(200));
        let (done_tx, done_rx) = mpsc::channel();
        let stopper = std::thread::spawn(move || {
            let st = rec.stop();
            let _ = done_tx.send(st);
        });
        let st = done_rx.recv_timeout(Duration::from_secs(10));
        // Unstick the old behaviour either way, so a failure does not hang the run.
        running.store(false, Ordering::Relaxed);
        let st = st.expect("stop() was still chasing the live feed after 10 s");
        stopper.join().unwrap();
        feeder.join().unwrap();
        assert_eq!(st.state, RecState::Finished, "{:?}", st.state);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn losses_are_counted_in_samples_and_re_anchor_the_clock() {
        let base = temp("loss");
        let (tx, tap) = manual_tap(16);
        let dropped = tap.dropped.clone();
        let dropped_events = tap.dropped_events.clone();
        let rec = Recorder::start_with(tap, &base, "", None, &HashMap::new(), "test", None, Vec::new()).unwrap();
        // Anchored at when the frame arrived, not when the writer got to it.
        let mut first = batch(1000, 3);
        let arrived = SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000);
        if let TapEvent::Samples(b) = &mut first {
            b.arrived = arrived;
        }
        tx.send(first).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        // The session lost 5 samples and a mark to this recorder meanwhile.
        dropped.fetch_add(5, Ordering::Relaxed);
        dropped_events.fetch_add(1, Ordering::Relaxed);
        tx.send(batch(2000, 3)).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let st = rec.stop();
        assert_eq!(st.lost, 5);
        let back = read(&st.dir).unwrap();
        assert_eq!((back.meta.samples_lost, back.meta.events_lost), (5, 1));
        let at: Vec<i64> = back.meta.anchors.iter().map(|a| a.controller_ms).collect();
        assert_eq!(at, vec![1000, 2000], "the first sample after a loss anchors the wall clock again");
        assert_eq!(back.meta.anchors[0].utc, wall_iso(arrived));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(windows)]
    #[test]
    fn a_locked_description_file_does_not_end_the_recording() {
        use std::os::windows::fs::OpenOptionsExt;
        let base = temp("lock");
        let (tx, tap) = manual_tap(64);
        let rec = Recorder::start_with(tap, &base, "", None, &HashMap::new(), "test", None, Vec::new()).unwrap();
        let dir = rec.status().dir;
        // Open without delete sharing, as some sync clients and scanners hold files:
        // replacing it fails until they let go.
        const FILE_SHARE_READ: u32 = 1;
        let lock = std::fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(dir.join("recording.json")).unwrap();
        rec.marker("while locked", Some(1));
        tx.send(batch(1000, 3)).unwrap();
        std::thread::sleep(Duration::from_millis(600));
        let s = rec.status();
        assert_eq!(s.state, RecState::Recording, "a locked description file ended the recording");
        assert!(s.warning.is_some());
        drop(lock);
        rec.marker("after", Some(2));
        tx.send(batch(2000, 3)).unwrap();
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(rec.status().warning, None, "the warning must clear once a write works");
        let st = rec.stop();
        assert_eq!(st.state, RecState::Finished, "{:?}", st.state);
        let back = read(&dir).unwrap();
        assert_eq!(back.meta.markers.len(), 2);
        assert_eq!(back.data["4002/ROB_1/J1"].len(), 6);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_read_error_ends_the_rows_instead_of_repeating() {
        struct Gone;
        impl std::io::Read for Gone {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("the share is gone"))
            }
        }
        let mut rows = CsvRows { r: std::io::BufReader::new(Gone), line: 0, error: None };
        assert!(rows.next_row().is_none(), "a failing read must end the rows, not repeat");
        assert!(rows.error.as_deref().is_some_and(|e| e.contains("the share is gone")), "{:?}", rows.error);
        assert!(rows.next_row().is_none());
    }

    #[test]
    fn a_clock_jump_anchors_the_wall_clock_again() {
        let base = temp("jump");
        let (tx, tap) = manual_tap(16);
        let rec = Recorder::start_with(tap, &base, "", None, &HashMap::new(), "test", None, Vec::new()).unwrap();
        tx.send(batch(1000, 3)).unwrap();
        tx.send(TapEvent::Mark { wall: SystemTime::now(), mark: Mark::ClockReset { from_raw: 1008, to_raw: 900_000 } }).unwrap();
        tx.send(batch(900_000, 3)).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let st = rec.stop();
        let back = read(&st.dir).unwrap();
        let at: Vec<i64> = back.meta.anchors.iter().map(|a| a.controller_ms).collect();
        assert_eq!(at, vec![1000, 900_000]);
        assert!(back.meta.events.iter().any(|e| e.kind == "clock_reset" && e.text.contains("forward")));
        let _ = std::fs::remove_dir_all(&base);
    }

    fn connected(system: &str) -> TapEvent {
        TapEvent::Mark { wall: SystemTime::now(), mark: Mark::Connected { target: crate::session::Target { host: "192.168.125.1".into(), port: 5515 }, system_id: Some(system.into()) } }
    }

    #[test]
    fn a_different_controller_behind_the_address_ends_the_recording() {
        // Every IRC5's service port is 192.168.125.1: a cable moved from one robot to
        // the next reconnects to a different controller at the same address.
        let base = temp("other");
        let (tx, tap) = manual_tap(64);
        let rec = Recorder::start_with(tap, &base, "", None, &HashMap::new(), "192.168.125.1:5515", Some("{A}".into()), Vec::new()).unwrap();
        tx.send(batch(1000, 3)).unwrap();
        tx.send(connected("{A}")).unwrap();
        tx.send(batch(2000, 3)).unwrap();
        tx.send(connected("{B}")).unwrap();
        tx.send(batch(3000, 3)).unwrap();
        assert!((0..100).any(|_| {
            std::thread::sleep(Duration::from_millis(20));
            matches!(rec.status().state, RecState::Ended(_))
        }), "{:?}", rec.status().state);
        let st = rec.stop();
        let RecState::Ended(why) = &st.state else { panic!("{:?}", st.state) };
        assert!(why.contains("{A}") && why.contains("{B}"), "{why}");
        let back = read(&st.dir).unwrap();
        assert!(back.meta.complete, "closed properly, not cut short");
        assert_eq!(back.meta.system_id.as_deref(), Some("{A}"));
        assert_eq!(back.data["4002/ROB_1/J1"].iter().map(|r| r.0).collect::<Vec<_>>(), vec![1000, 1004, 1008, 2000, 2004, 2008], "nothing of the second controller's");
        assert!(back.meta.notes.contains("{B}"), "{}", back.meta.notes);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_recording_begun_without_a_system_id_takes_the_first_one_it_sees() {
        let base = temp("noid");
        let (tx, tap) = manual_tap(64);
        let rec = Recorder::start_with(tap, &base, "", None, &HashMap::new(), "192.168.125.1:5515", None, Vec::new()).unwrap();
        tx.send(connected("{A}")).unwrap();
        tx.send(batch(1000, 3)).unwrap();
        tx.send(connected("{A}")).unwrap();
        tx.send(batch(2000, 3)).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let st = rec.stop();
        assert_eq!(st.state, RecState::Finished, "{:?}", st.state);
        assert_eq!(read(&st.dir).unwrap().meta.system_id.as_deref(), Some("{A}"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn anchors_say_which_rows_they_map() {
        // After a restart the controller's clock starts again, so the same
        // controller_ms can occur on both sides: the anchor that applies to a row is
        // the last one at or before it in the file, not the nearest in value.
        let base = temp("rows");
        let (tx, tap) = manual_tap(64);
        let rec = Recorder::start_with(tap, &base, "", None, &HashMap::new(), "t", None, Vec::new()).unwrap();
        tx.send(batch(900_000, 3)).unwrap();
        tx.send(TapEvent::Mark { wall: SystemTime::now(), mark: Mark::ClockReset { from_raw: 900_008, to_raw: 5000 } }).unwrap();
        tx.send(batch(5000, 2)).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let back = read(&rec.stop().dir).unwrap();
        let rows: Vec<(i64, Option<u64>)> = back.meta.anchors.iter().map(|a| (a.controller_ms, a.row)).collect();
        assert_eq!(rows, vec![(900_000, Some(0)), (5000, Some(3))]);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_slow_log_closes_its_open_intervals_at_a_restart() {
        // The interval open when the clock restarted belongs to the old clock: it is
        // written before the new anchor, so the anchor's row does not claim it.
        let base = temp("slowreset");
        let (tx, tap) = manual_tap(64);
        let rec = Recorder::start_with(tap, &base, "", Some(100), &HashMap::new(), "t", None, Vec::new()).unwrap();
        tx.send(batch(900_000, 3)).unwrap();
        tx.send(TapEvent::Mark { wall: SystemTime::now(), mark: Mark::ClockReset { from_raw: 900_008, to_raw: 5000 } }).unwrap();
        tx.send(batch(5000, 3)).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let st = rec.stop();
        let text = std::fs::read_to_string(st.dir.join("slow.csv")).unwrap();
        let starts: Vec<&str> = text.lines().skip(1).map(|l| l.split(',').next().unwrap()).collect();
        assert_eq!(starts, vec!["900000", "5000"], "{text}");
        let back = read(&st.dir).unwrap();
        let rows: Vec<(i64, Option<u64>)> = back.meta.anchors.iter().map(|a| (a.controller_ms, a.row)).collect();
        assert_eq!(rows, vec![(900_000, Some(0)), (5000, Some(1))]);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_recording_from_before_the_joint_ids_reads_back_under_them() {
        // The cell's recordings of 2026-09-26 wrote 4002/ROB_1/2; this program now
        // writes 4002/ROB_1/J2. Both read back the same.
        let dir = temp("v1");
        std::fs::create_dir_all(&dir).unwrap();
        let mut meta = Meta::new(Kind::Full, "", "192.0.2.77:5515", None);
        meta.version = 1;
        let mut e = entry(&ChannelKey::parse_id("4002/ROB_1/2").unwrap(), None);
        e.id = "4002/ROB_1/2".into();
        meta.channels.push(e);
        meta.anchors.push(Anchor { controller_ms: 10, utc: "2026-09-26T08:47:20.523Z".into(), row: None });
        write_meta(&dir, &meta).unwrap();
        std::fs::write(dir.join("data.csv"), "controller_ms,channel,value\n10,4002/ROB_1/2,1.5\n14,4002/ROB_1/2,2.5\n18,not/a/channel,3\n").unwrap();
        let back = read(&dir).unwrap();
        assert_eq!(back.data["4002/ROB_1/J2"], vec![(10, 1.5), (14, 2.5)]);
        assert_eq!(back.meta.channels[0].id, "4002/ROB_1/J2");
        assert_eq!(back.data["not/a/channel"], vec![(18, 3.0)], "an id it cannot parse is kept as written");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recordings_are_listed_newest_first_and_other_folders_left_out() {
        let base = temp("list");
        for (name, started) in [("a", "2026-09-27T10:00:00.000Z"), ("b", "2026-09-27T12:00:00.000Z"), ("c", "2026-09-26T09:00:00.000Z")] {
            let d = base.join(name);
            std::fs::create_dir_all(&d).unwrap();
            let mut m = Meta::new(Kind::Full, name, "t", None);
            m.started_utc = started.into();
            write_meta(&d, &m).unwrap();
        }
        std::fs::create_dir_all(base.join("not a recording")).unwrap();
        std::fs::write(base.join("stray.txt"), "x").unwrap();
        let names: Vec<String> = list(&base).into_iter().map(|(_, m)| m.label).collect();
        assert_eq!(names, vec!["b", "a", "c"]);
        assert!(list(&base.join("missing")).is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_line_without_an_end_is_one_bad_row() {
        // 50 MB with no newline (a crash can leave a run of zero bytes in a file):
        // one bad row, read a bounded piece at a time and skipped to its end, and the
        // rows after it read as usual.
        struct Runaway {
            left: usize,
            tail: std::io::Cursor<&'static [u8]>,
        }
        impl std::io::Read for Runaway {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.left == 0 {
                    return self.tail.read(buf);
                }
                let n = buf.len().min(self.left);
                buf[..n].fill(0);
                self.left -= n;
                Ok(n)
            }
        }
        let r = Runaway { left: 50 << 20, tail: std::io::Cursor::new(b"\n7,4002/ROB_1/J1,1.5\n") };
        let mut rows = CsvRows { r: std::io::BufReader::new(r), line: 0, error: None };
        assert!(matches!(rows.next_row(), Some(Err(1))), "a runaway line is a bad row");
        let next = rows.next_row();
        assert!(matches!(&next, Some(Ok((_, f))) if f.len() == 3 && f[0].0 == "7"), "{next:?}");
        assert!(rows.next_row().is_none() && rows.error.is_none());
    }

    #[test]
    fn a_snapshot_across_a_restart_anchors_each_clock() {
        // "Save the last N seconds" right after a controller restart: the rows before
        // it are on the old clock and those after on the new one, each with its own
        // anchor at its first row.
        let store = Store::new();
        let key = ChannelKey::parse_id("4002/ROB_1/J1").unwrap();
        let ch = store.channel(&key, 4.0);
        let mut tl = Timeline::new();
        let t0 = Instant::now();
        for i in 0..5 {
            let (_, t, _) = tl.map(500_000 + 4 * i, t0);
            ch.lock().push(t, 1.0);
        }
        let later = t0 + Duration::from_secs(20);
        for i in 0..3 {
            let (_, t, _) = tl.map(10_000 + 4 * i, later);
            ch.lock().push(t, 2.0);
        }
        let base = temp("snapreset");
        let (dir, rows) = write_snapshot(&base, "", &store, &tl, std::slice::from_ref(&key), &HashMap::new(), 60.0, "t", None, &[]).unwrap();
        assert_eq!(rows, 8);
        let back = read(&dir).unwrap();
        let a: Vec<(i64, Option<u64>)> = back.meta.anchors.iter().map(|a| (a.controller_ms, a.row)).collect();
        assert_eq!(a, vec![(500_000, Some(0)), (10_000, Some(5))]);
        let utc = |i: usize| crate::util::parse_iso(&back.meta.anchors[i].utc).unwrap();
        let gap = utc(1).duration_since(utc(0)).unwrap().as_secs_f64();
        assert!((19.9..20.1).contains(&gap), "the anchors are {gap} s apart; 20 s passed");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_reader_takes_csv_as_csv() {
        let text = "controller_ms,channel,value\n1,a,1.5\n2,b,\"x, \"\"y\"\"\r\nz\"\n3,a,NaN\nnot a row\n4,a\n5,c,\"unterminated\n";
        let mut rows = CsvRows { r: std::io::Cursor::new(text.as_bytes()), line: 0, error: None };
        let mut got = Vec::new();
        while let Some(r) = rows.next_row() {
            got.push(r.map(|(line, f)| (line, f.into_iter().map(|(v, q)| format!("{}{v}", if q { "q:" } else { "" })).collect::<Vec<_>>())));
        }
        assert_eq!(got[1], Ok((2, vec!["1".into(), "a".into(), "1.5".into()])));
        assert_eq!(got[2], Ok((3, vec!["2".into(), "b".into(), "q:x, \"y\"\r\nz".into()])));
        assert_eq!(got[3], Ok((5, vec!["3".into(), "a".into(), "NaN".into()])));
        assert_eq!(got[4], Ok((6, vec!["not a row".into()])));
        assert_eq!(got[5], Ok((7, vec!["4".into(), "a".into()])));
        assert_eq!(got[6], Err(8), "a file that ends inside a quote is a cut-short row");
        assert_eq!(got.len(), 7);
    }
}
