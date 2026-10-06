use std::collections::BTreeMap;
use std::net::ToSocketAddrs;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use spy_core::discovery;
use spy_core::log::LogBook;
use spy_core::request::{Axis, MechUnit};
use spy_core::sample::ValueKind;
use spy_core::session::{AskPolicy, ChannelState, Options, Phase, SampleBatch, Session, Tap, TapEvent, Target, ROBAPI_PORT};
use spy_core::store::{ChannelKey, Store};

const USAGE: &str = "\
signal-spy-probe: console probe for ABB IRC5 InfoStream. It reads test signals: it sets up
streams for them, and never commands motion or writes RAPID, configuration or I/O.

usage:
  signal-spy-probe list
        find local virtual controllers (the ports RobVC.exe / Vrchost64.exe listen on)
        and handshake each one
  signal-spy-probe hello HOST[:PORT]
        handshake only: system id and the connected RobAPI clients
  signal-spy-probe stream HOST[:PORT] [--unit ROB_1] [--seconds 5] [--take] SIGNAL[:AXIS] ...
        define the signals (axis one-based, default 1), stream, and report rates,
        timestamp steps, record types and values
  signal-spy-probe typed HOST[:PORT] [--unit ROB_1] [--seconds 1.5] [--take] [--allow-remote] NUMBERS
        stream NUMBERS (e.g. 523-526,849,9869) twelve at a time and report which
        record type carries each: float, int, string, silent or refused. At most
        20000 numbers; more than 120 on a controller that is not on this PC only
        with --allow-remote (each refused number is an entry in its event log)
  signal-spy-probe tenancy HOST[:PORT] [--unit ROB_1] [--allow-remote]
        two sessions at once: the first should stop, quietly, as soon as the
        second starts; the second gets nothing until it connects again, and then
        streams. It disturbs every other program streaming from the controller.
        Loopback only unless --allow-remote.
  signal-spy-probe selftest
        run 'stream' against the built-in fake controller; touches no real controller
  signal-spy-probe rws HOST[:PORT] [USER]
        read-only RWS 1.0 (port 80 by default): the system, the identity, the clock
        against this PC's, the newest events, and ROB_1's motor calibration; then
        looks at the event log again after 5 s. GETs only. The password is read
        from SPY_RWS_PASSWORD; USER defaults to RobotWare's default user

PORT defaults to 5515 (an IRC5). A virtual controller uses its own port; see 'list'.
--take answers yes to taking InfoStream when other RobAPI clients are connected to a
controller elsewhere; without it the probe lists them and stops. A virtual controller on
this PC is not asked about: the probe takes InfoStream from it without asking. Every
refused signal number writes one 50228 entry into the controller's event log.
Exit status: 0 when the run went through, 1 when the session stopped or failed, 2 for a
mistake in the command.";

const TYPED_MAX: usize = 20_000;
const TYPED_REMOTE_FREE: usize = 120;

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

#[cfg(windows)]
fn install_ctrl_c() {
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    unsafe extern "system" fn handler(_ctrl: u32) -> windows_sys::core::BOOL {
        INTERRUPTED.store(true, Ordering::SeqCst);
        1
    }
    unsafe {
        SetConsoleCtrlHandler(Some(handler), 1);
    }
}

#[cfg(not(windows))]
fn install_ctrl_c() {}

fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

fn ended(stopped: bool) -> ExitCode {
    if stopped { ExitCode::from(1) } else { ExitCode::SUCCESS }
}

struct Args {
    rest: Vec<String>,
    unit: String,
    seconds: f64,
    take: bool,
    allow_remote: bool,
}

fn parse_args(raw: &[String]) -> Result<Args, String> {
    let mut a = Args { rest: Vec::new(), unit: "ROB_1".into(), seconds: 0.0, take: false, allow_remote: false };
    let mut i = 0;
    while i < raw.len() {
        let s = &raw[i];
        let value = |i: usize| raw.get(i + 1).cloned().ok_or_else(|| format!("{s} needs a value"));
        match s.as_str() {
            "--unit" => {
                a.unit = value(i)?;
                i += 1;
            }
            "--seconds" => {
                a.seconds = value(i)?.parse().map_err(|_| "--seconds needs a number".to_string())?;
                if !(a.seconds > 0.0 && a.seconds <= 86_400.0) {
                    return Err("--seconds must be between 0 and 86400".into());
                }
                i += 1;
            }
            "--take" => a.take = true,
            "--allow-remote" => a.allow_remote = true,
            x if x.starts_with("--") => return Err(format!("unknown option {x}")),
            _ => a.rest.push(s.clone()),
        }
        i += 1;
    }
    Ok(a)
}

fn target(s: &str) -> Result<Target, String> {
    let (host, port) = match s.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') || h.starts_with('[') => (h.trim_matches(['[', ']']).to_string(), p.parse::<u16>().map_err(|_| format!("bad port in {s}"))?),
        _ => (s.to_string(), ROBAPI_PORT),
    };
    if host.is_empty() || port == 0 {
        return Err(format!("bad target {s}"));
    }
    Ok(Target { host, port })
}

fn is_loopback(t: &Target) -> bool {
    (t.host.as_str(), t.port).to_socket_addrs().map(|mut a| a.all(|x| x.ip().is_loopback())).unwrap_or(false)
}

fn numbers(spec: &str) -> Result<Vec<u32>, String> {
    let mut out = Vec::new();
    for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        if let Some((a, b)) = part.split_once('-') {
            let (a, b): (u32, u32) = (a.parse().map_err(|_| format!("bad range {part}"))?, b.parse().map_err(|_| format!("bad range {part}"))?);
            if b < a || b - a > 20_000 {
                return Err(format!("bad range {part}"));
            }
            out.extend(a..=b);
        } else {
            out.push(part.parse().map_err(|_| format!("bad number {part}"))?);
        }
        if out.len() > TYPED_MAX {
            return Err(format!("at most {TYPED_MAX} numbers in one run"));
        }
    }
    out.dedup();
    if out.is_empty() {
        return Err("no signal numbers".into());
    }
    Ok(out)
}

fn channel(spec: &str, unit: &MechUnit) -> Result<ChannelKey, String> {
    let (sig, axis) = match spec.split_once(':') {
        Some((s, a)) => (s, a.parse::<u8>().map_err(|_| format!("bad axis in {spec}"))?),
        None => (spec, 1),
    };
    let signal = sig.parse::<u32>().map_err(|_| format!("bad signal number {sig}"))?;
    let axis = Axis::new(axis).ok_or_else(|| format!("axis in {spec} must be 1..6 (one-based)"))?;
    Ok(ChannelKey { signal, unit: unit.clone(), axis })
}

fn new_session(ask: AskPolicy) -> (Session, Arc<LogBook>) {
    let log = Arc::new(LogBook::new());
    let opts = Options { ask, ..Options::default() };
    (Session::spawn(opts, log.clone(), Arc::new(Store::new()), Arc::new(|| {})), log)
}

struct LogTail {
    next: u64,
    tag: &'static str,
}

impl LogTail {
    fn pump(&mut self, log: &LogBook) {
        for e in log.since(self.next) {
            self.next = e.seq + 1;
            println!("{}  {:5} {}", self.tag, format!("{:?}", e.level).to_uppercase(), e.text);
        }
    }
}

fn wait_streaming(s: &Session, log: &LogBook, tail: &mut LogTail, take: bool, timeout: Duration) -> bool {
    let end = Instant::now() + timeout;
    let mut answered: Option<Vec<String>> = None;
    while Instant::now() < end && !interrupted() {
        tail.pump(log);
        let phase = s.status().phase.clone();
        match phase {
            Phase::Streaming => return true,
            Phase::Stopped { reason } => {
                println!("stopped: {reason}");
                return false;
            }
            Phase::AwaitingApproval if answered.as_ref() != Some(&s.status().others.iter().map(|o| o.address.clone()).collect()) => {
                let others = s.status().others.clone();
                println!("other RobAPI clients are connected to this controller:");
                for o in &others {
                    let attrs: Vec<String> = o.attributes.iter().filter(|(k, _)| k != "a").map(|(k, v)| format!("{k}={v}")).collect();
                    println!("    {}{}{}{}", o.address, if o.same_pc { "  (this PC)" } else { "" }, if o.pendant { "  (FlexPendant)" } else { "" }, if attrs.is_empty() { String::new() } else { format!("  {}", attrs.join(" ")) });
                }
                println!("another tool, such as RobotStudio or TuneMaster, may be streaming test signals from it;");
                println!("taking InfoStream will stop its streams.");
                answered = Some(others.iter().map(|o| o.address.clone()).collect());
                if take {
                    println!("--take given: taking InfoStream.");
                    s.answer(true, &others);
                } else {
                    println!("not taking it (run again with --take to do so).");
                    s.answer(false, &others);
                }
            }
            _ => {}
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    tail.pump(log);
    false
}

fn cmd_list() -> ExitCode {
    match discovery::local_controllers(Duration::from_millis(1500)) {
        Err(e) => {
            eprintln!("cannot list listening ports: {e}");
            ExitCode::from(2)
        }
        Ok(list) if list.is_empty() => {
            println!("no virtual controller processes are listening (RobVC.exe, Vrchost64.exe)");
            ExitCode::SUCCESS
        }
        Ok(list) => {
            for c in list {
                match c.hello {
                    Ok(a) => println!(
                        "{:<14} pid {:<6} port {:<5}  RobAPI: system {}  clients: {}",
                        c.process,
                        c.pid,
                        c.port,
                        a.system_id.as_deref().unwrap_or("?"),
                        a.clients.iter().map(|x| x.address.as_str()).collect::<Vec<_>>().join(", ")
                    ),
                    Err(e) => println!("{:<14} pid {:<6} port {:<5}  not RobAPI: {e}", c.process, c.pid, c.port),
                }
            }
            ExitCode::SUCCESS
        }
    }
}

fn cmd_rws(host: &str, port: u16, user: &str, password: &str) -> ExitCode {
    use spy_core::rws::{calib_instance, Client, EventPoll};
    let run = || -> Result<(), spy_core::rws::RwsError> {
        let mut c = Client::new(host, port, user, password);
        let s = c.system()?;
        println!("system    : {} RobotWare {} id {}", s.name, s.rw_version, s.system_id);
        let i = c.identity()?;
        println!("identity  : {} ({})", i.name, i.kind);
        let pc = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        let clock = c.clock()?;
        println!("clock     : controller minus PC (UTC) = {} s", clock - pc);
        let mut poll = EventPoll::default();
        let first = poll.look(&mut c)?;
        println!("events    : newest {} (first look)", first.events.len());
        for e in first.events.iter().rev().take(5) {
            println!("  {} {} {:>5} {}", e.id, e.severity_word(), e.code, e.title);
        }
        for axis in 1..=6 {
            let inst = calib_instance("ROB_1", axis).unwrap_or_default();
            match c.motor_calib(&inst) {
                Ok(m) => println!("calib     : {inst} com_offset {} ({}) cal_offset {} ({})", m.com_offset, if m.com_valid { "valid" } else { "NOT valid" }, m.cal_offset, if m.cal_valid { "valid" } else { "NOT valid" }),
                Err(e) => println!("calib     : {inst}: {e}"),
            }
        }
        std::thread::sleep(Duration::from_secs(5));
        let again = poll.look(&mut c)?;
        println!("events    : {} new after 5 s{}", again.events.len(), if again.skipped { " (more than one look reads)" } else { "" });
        Ok(())
    };
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{host}:{port}: {e}");
            ExitCode::from(1)
        }
    }
}

fn cmd_hello(t: &Target) -> ExitCode {
    let addr = match (t.host.as_str(), t.port).to_socket_addrs().ok().and_then(|mut a| a.next()) {
        Some(a) => a,
        None => {
            eprintln!("cannot resolve {t}");
            return ExitCode::from(2);
        }
    };
    match discovery::hello(addr, Duration::from_secs(3)) {
        Ok(a) => {
            println!("system id : {}", a.system_id.as_deref().unwrap_or("(none)"));
            println!("ctrl1     : {}", a.ctrl1);
            println!("client list as sent: {}", a.raw_client_list.as_deref().unwrap_or("(none)"));
            for c in &a.clients {
                println!("  client {}  {}", c.address, c.attributes.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" "));
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{t}: {e}");
            ExitCode::from(1)
        }
    }
}

fn last_text(s: &Session, k: &ChannelKey) -> Option<String> {
    let ch = s.store().get(k)?;
    let r = ch.lock();
    if r.kind == Some(spy_core::sample::ValueKind::String) {
        return r.last_text.as_ref().map(|t| format!("{t:?}"));
    }
    r.last().map(|(_, v)| format!("{v}"))
}

#[derive(Default)]
struct RunStats {
    steps: BTreeMap<ChannelKey, (Option<i64>, BTreeMap<i64, usize>)>,
    last_arrival: Option<SystemTime>,
    max_wait: Duration,
    waits_over_100ms: u64,
}

impl RunStats {
    fn feed(&mut self, b: &SampleBatch) {
        let e = self.steps.entry(b.key.clone()).or_default();
        for &t in &b.timeline_ms {
            if let Some(p) = e.0 {
                *e.1.entry(t - p).or_insert(0) += 1;
            }
            e.0 = Some(t);
        }
        if let Some(prev) = self.last_arrival
            && let Ok(w) = b.arrived.duration_since(prev)
        {
            self.max_wait = self.max_wait.max(w);
            if w > Duration::from_millis(100) {
                self.waits_over_100ms += 1;
            }
        }
        self.last_arrival = Some(self.last_arrival.map_or(b.arrived, |p| p.max(b.arrived)));
    }

    fn drain(&mut self, tap: &Tap) {
        while let Ok(ev) = tap.try_recv() {
            if let TapEvent::Samples(b) = ev {
                self.feed(&b);
            }
        }
    }

    fn steps_of(&self, k: &ChannelKey) -> BTreeMap<i64, usize> {
        self.steps.get(k).map(|(_, h)| h.clone()).unwrap_or_default()
    }
}

fn cmd_stream(t: Target, a: &Args, keys: Vec<ChannelKey>) -> ExitCode {
    let seconds = if a.seconds > 0.0 { a.seconds } else { 5.0 };
    let (s, log) = new_session(AskPolicy::Remote);
    let mut tail = LogTail { next: 0, tag: "log" };
    let tap = s.tap(1 << 16);
    let mut run = RunStats::default();
    s.set_channels(keys.clone());
    println!("connecting to {t} ...");
    s.connect(t);
    if !wait_streaming(&s, &log, &mut tail, a.take, Duration::from_secs(15)) {
        drop(s);
        return ExitCode::from(1);
    }
    let start = Instant::now();
    let mut next_line = Instant::now() + Duration::from_secs(1);
    while start.elapsed().as_secs_f64() < seconds && !interrupted() {
        tail.pump(&log);
        run.drain(&tap);
        if matches!(s.status().phase, Phase::Stopped { .. }) {
            break;
        }
        if Instant::now() >= next_line {
            next_line += Duration::from_secs(1);
            let st = s.status();
            let parts: Vec<String> = st
                .channels
                .iter()
                .map(|c| format!("{}:{} {:.0}/s {}", c.key.signal, c.key.axis.one_based(), c.rate, last_text(&s, &c.key).unwrap_or("-".into())))
                .collect();
            println!("[{:5.1} s] {}", start.elapsed().as_secs_f64(), parts.join("  |  "));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    if interrupted() {
        println!("interrupted; tearing down");
    }
    let live = s.status().clone();
    let stopped = matches!(live.phase, Phase::Stopped { .. });
    if !stopped {
        s.disconnect();
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end && s.status().phase.is_active() {
            tail.pump(&log);
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    let st = s.status().clone();
    run.drain(&tap);
    println!();
    println!("summary ({:.1} s):", start.elapsed().as_secs_f64());
    for c in &st.channels {
        let was = live.channels.iter().find(|l| l.key == c.key).map(|l| &l.state).unwrap_or(&c.state);
        let state = match was {
            ChannelState::Defined { stream } => format!("stream {stream}"),
            ChannelState::Refused { text, .. } => format!("REFUSED: {text}"),
            other => format!("{other:?}"),
        };
        let spacing = if c.kind == Some(ValueKind::String) {
            format!("events {}", c.samples)
        } else {
            format!("samples {}  gaps {}  stamp steps (ms: count) {:?}", c.samples, c.gaps, run.steps_of(&c.key))
        };
        println!(
            "  {}  {}  type {}  reported {} ms  {}",
            c.key,
            state,
            c.kind.map(|k| k.label()).unwrap_or("-"),
            c.sample_ms.map(|v| v.to_string()).unwrap_or("-".into()),
            spacing
        );
    }
    println!(
        "  longest wait between sample frames {:.0} ms  waits over 100 ms {}  samples the summary missed {}",
        run.max_wait.as_secs_f64() * 1000.0,
        run.waits_over_100ms,
        tap.dropped.load(Ordering::Relaxed)
    );
    let k = &st.counters;
    println!(
        "  frames {}  sample frames {}  samples {}  AYA answered {}  foreign records {}  foreign subscription {}  liveness checks {}  defects {:?}  no-trailer {}",
        k.frames, k.sample_frames, k.samples, k.ayas, k.foreign_records, k.foreign_subscription, k.liveness_checks, k.defects, k.no_trailer
    );
    println!("  sample frames by service {:?}  protobuf marker at offset {:?}  unexpected frames {}", k.sample_services, k.marker_offsets, k.unexpected_frames);
    if let Phase::Stopped { reason } = &st.phase {
        println!("  STOPPED: {reason}");
    }
    let stopped = matches!(st.phase, Phase::Stopped { .. });
    let ok = s.shutdown(Duration::from_secs(5));
    tail.pump(&log);
    println!("{}", if ok { "torn down cleanly" } else { "teardown did not finish in time" });
    ended(stopped || !ok)
}

fn cmd_typed(t: Target, a: &Args, nums: Vec<u32>, unit: MechUnit) -> ExitCode {
    let dwell = Duration::from_secs_f64(if a.seconds > 0.0 { a.seconds } else { 1.5 });
    let (s, log) = new_session(AskPolicy::Remote);
    let mut tail = LogTail { next: 0, tag: "log" };
    println!("connecting to {t} ...");
    s.connect(t);
    if !wait_streaming(&s, &log, &mut tail, a.take, Duration::from_secs(15)) {
        return ExitCode::from(1);
    }
    let mut results: Vec<(u32, String, Option<String>, String)> = Vec::new();
    let mut stopped = false;
    for batch in nums.chunks(12) {
        if interrupted() {
            break;
        }
        let keys: Vec<ChannelKey> = batch.iter().map(|&n| ChannelKey { signal: n, unit: unit.clone(), axis: Axis::new(1).unwrap() }).collect();
        s.set_channels(keys.clone());
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end && !interrupted() {
            let st = s.status();
            let settled = keys.iter().all(|k| st.channels.iter().any(|c| &c.key == k && !matches!(c.state, ChannelState::Waiting | ChannelState::Defining)));
            drop(st);
            if settled {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(dwell);
        tail.pump(&log);
        let st = s.status().clone();
        if matches!(st.phase, Phase::Stopped { .. }) {
            println!("the session stopped; results so far below");
            stopped = true;
            break;
        }
        for k in &keys {
            let c = st.channels.iter().find(|c| &c.key == k);
            let (verdict, ms) = match c.map(|c| (&c.state, c.kind, c.samples, c.sample_ms)) {
                Some((ChannelState::Refused { code, .. }, ..)) => (format!("refused {}", code.map(|c| c.to_string()).unwrap_or_default()), String::new()),
                Some((ChannelState::Defined { .. }, Some(kind), n, ms)) if n > 0 => (kind.label().to_string(), ms.map(|m| m.to_string()).unwrap_or_default()),
                Some((ChannelState::Defined { .. }, _, _, ms)) => ("silent".to_string(), ms.map(|m| m.to_string()).unwrap_or_default()),
                Some((other, ..)) => (format!("{other:?}"), String::new()),
                None => ("missing".into(), String::new()),
            };
            results.push((k.signal, verdict, last_text(&s, k), ms));
        }
    }
    s.set_channels(Vec::new());
    std::thread::sleep(Duration::from_millis(300));
    let ok = s.shutdown(Duration::from_secs(5));
    tail.pump(&log);
    println!();
    println!("# signal\ttype\tsample_ms\tlast value   ({} on {})", results.len(), unit);
    for (n, verdict, v, ms) in &results {
        println!("{n}\t{verdict}\t{ms}\t{}", v.clone().unwrap_or_default());
    }
    let mut by: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, verdict, _, _) in &results {
        *by.entry(verdict.split_whitespace().next().unwrap_or("")).or_default() += 1;
    }
    println!("# {by:?}");
    println!("{}", if ok { "torn down cleanly" } else { "teardown did not finish in time" });
    ended(stopped || !ok)
}

fn cmd_tenancy(t: Target, a: &Args, unit: MechUnit) -> ExitCode {
    if !is_loopback(&t) && !a.allow_remote {
        eprintln!("tenancy disturbs every other InfoStream client on the controller; it runs on loopback");
        eprintln!("(a local virtual controller) unless --allow-remote is given.");
        return ExitCode::from(2);
    }
    let (sa, la) = new_session(AskPolicy::Never);
    let (sb, lb) = new_session(AskPolicy::Never);
    let mut ta = LogTail { next: 0, tag: "A  " };
    let mut tb = LogTail { next: 0, tag: "  B" };
    let ka = ChannelKey { signal: 6000, unit: unit.clone(), axis: Axis::new(1).unwrap() };
    let kb = ChannelKey { signal: 6001, unit, axis: Axis::new(1).unwrap() };
    sa.set_channels(vec![ka.clone()]);
    sa.connect(t.clone());
    if !wait_streaming(&sa, &la, &mut ta, true, Duration::from_secs(15)) {
        return ExitCode::from(1);
    }
    std::thread::sleep(Duration::from_millis(1000));
    let a0 = sa.status().channels[0].samples;
    println!("A alone: {a0} samples of 6000 in 1 s");
    sb.set_channels(vec![kb.clone()]);
    let t2 = t.clone();
    sb.connect(t);
    let b_up = wait_streaming(&sb, &lb, &mut tb, true, Duration::from_secs(15));
    println!("B's session {}", if b_up { "was set up like A's: no refusal, no busy signal" } else { "did not get to streaming" });
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end && !matches!(sa.status().phase, Phase::Stopped { .. }) && !interrupted() {
        ta.pump(&la);
        tb.pump(&lb);
        std::thread::sleep(Duration::from_millis(50));
    }
    ta.pump(&la);
    tb.pump(&lb);
    let sa_st = sa.status().clone();
    let a_stopped = matches!(sa_st.phase, Phase::Stopped { .. });
    match &sa_st.phase {
        Phase::Stopped { reason } => println!("A stopped: {reason}"),
        other => println!("A is still {other:?}: it should have stopped when B started"),
    }
    println!("A received {} samples of its stream, and {} records of streams it did not define", sa_st.channels[0].samples - a0, sa_st.counters.foreign_records);
    let b0 = sb.status().channels.first().map(|c| c.samples).unwrap_or(0);
    std::thread::sleep(Duration::from_millis(2000));
    tb.pump(&lb);
    let sb_st = sb.status().clone();
    let b1 = sb_st.channels.first().map(|c| c.samples).unwrap_or(0);
    println!("B then: {} samples in 2 s{}", b1 - b0, if b1 > b0 { ", now that A has gone" } else { " (not handed on to a client already connected)" });
    println!("subscription ids: A {:?}, B {:?}", sa_st.subscription, sb_st.subscription);
    let ok_a = sa.shutdown(Duration::from_secs(5));
    if b1 == b0 {
        println!("B connects again ...");
        sb.connect(t2);
        if wait_streaming(&sb, &lb, &mut tb, true, Duration::from_secs(15)) {
            let c0 = sb.status().channels.first().map(|c| c.samples).unwrap_or(0);
            std::thread::sleep(Duration::from_millis(1000));
            tb.pump(&lb);
            let c1 = sb.status().channels.first().map(|c| c.samples).unwrap_or(0);
            println!("B, connected again: {} samples in 1 s", c1 - c0);
        }
    }
    let ok_b = sb.shutdown(Duration::from_secs(5));
    println!("{}", if ok_a && ok_b { "both torn down" } else { "a teardown did not finish in time" });
    ended(!(b_up && a_stopped && ok_a && ok_b))
}

fn cmd_selftest() -> ExitCode {
    let fake = match spy_core::fake::FakeController::start(spy_core::fake::Behaviour::default()) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("cannot start the fake controller: {e}");
            return ExitCode::from(2);
        }
    };
    println!("fake controller on 127.0.0.1:{}", fake.port());
    let unit = MechUnit::new("ROB_1").unwrap();
    let keys = ["4002:2", "6000", "9888"].iter().map(|s| channel(s, &unit).unwrap()).collect();
    let args = Args { rest: vec![], unit: "ROB_1".into(), seconds: 2.0, take: false, allow_remote: false };
    cmd_stream(Target { host: "127.0.0.1".into(), port: fake.port() }, &args, keys)
}

enum Plan {
    List,
    Hello(Target),
    Stream(Target, Vec<ChannelKey>),
    Typed(Target, Vec<u32>, MechUnit),
    Tenancy(Target, MechUnit),
    Selftest,
    Rws { host: String, port: u16, user: String },
    Help,
}

fn plan(cmd: &str, a: &Args) -> Result<Plan, String> {
    let unit = MechUnit::new(&a.unit).map_err(|e| e.to_string())?;
    let need_target = || -> Result<Target, String> { target(a.rest.first().ok_or("a controller address is needed")?) };
    Ok(match cmd {
        "list" => Plan::List,
        "hello" => Plan::Hello(need_target()?),
        "stream" => {
            let t = need_target()?;
            let keys: Vec<ChannelKey> = a.rest.get(1..).unwrap_or_default().iter().map(|s| channel(s, &unit)).collect::<Result<_, _>>()?;
            if keys.is_empty() {
                return Err("name at least one SIGNAL[:AXIS]".into());
            }
            if keys.len() > 12 {
                return Err("at most 12 channels".into());
            }
            Plan::Stream(t, keys)
        }
        "typed" => {
            let t = need_target()?;
            let nums = numbers(a.rest.get(1).ok_or("give the signal numbers, e.g. 523-526,849")?)?;
            if nums.len() > TYPED_REMOTE_FREE && !a.allow_remote && !is_loopback(&t) {
                return Err(format!(
                    "{} numbers on a controller that is not on this PC: each one it refuses is an entry in its event log. At most {TYPED_REMOTE_FREE} without --allow-remote.",
                    nums.len()
                ));
            }
            Plan::Typed(t, nums, unit)
        }
        "tenancy" => Plan::Tenancy(need_target()?, unit),
        "selftest" => Plan::Selftest,
        "rws" => {
            let spec = a.rest.first().ok_or("a controller address is needed")?;
            let (host, port) = match spec.rsplit_once(':') {
                Some((h, p)) => (h.to_string(), p.parse::<u16>().map_err(|_| format!("bad port in {spec}"))?),
                None => (spec.clone(), spy_core::rws::DEFAULT_PORT),
            };
            let user = a.rest.get(1).cloned().unwrap_or_else(|| spy_core::rws::DEFAULT_USER.to_string());
            Plan::Rws { host, port, user }
        }
        "help" | "--help" | "-h" => Plan::Help,
        other => return Err(format!("unknown command {other}")),
    })
}

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = raw.first().cloned() else {
        println!("{USAGE}");
        return ExitCode::from(2);
    };
    let planned = parse_args(&raw[1..]).and_then(|a| plan(&cmd, &a).map(|p| (a, p)));
    let (a, p) = match planned {
        Ok(x) => x,
        Err(e) => {
            eprintln!("{e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    if matches!(p, Plan::Stream(..) | Plan::Typed(..) | Plan::Tenancy(..) | Plan::Selftest) {
        install_ctrl_c();
    }
    match p {
        Plan::List => cmd_list(),
        Plan::Hello(t) => cmd_hello(&t),
        Plan::Stream(t, keys) => cmd_stream(t, &a, keys),
        Plan::Typed(t, nums, unit) => cmd_typed(t, &a, nums, unit),
        Plan::Tenancy(t, unit) => cmd_tenancy(t, &a, unit),
        Plan::Selftest => cmd_selftest(),
        Plan::Rws { host, port, user } => match std::env::var("SPY_RWS_PASSWORD") {
            Ok(password) => cmd_rws(&host, port, &user, &password),
            Err(_) => {
                eprintln!("set SPY_RWS_PASSWORD to the RWS password");
                ExitCode::from(2)
            }
        },
        Plan::Help => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spy_core::session::BatchValues;

    fn batch(key: &ChannelKey, stamps: &[i64], arrived_ms: u64) -> SampleBatch {
        SampleBatch {
            key: key.clone(),
            kind: ValueKind::Float,
            raw_ms: stamps.iter().map(|&t| t as u64).collect(),
            timeline_ms: stamps.to_vec(),
            values: BatchValues::Number(vec![0.0; stamps.len()]),
            arrived: SystemTime::UNIX_EPOCH + Duration::from_millis(arrived_ms),
        }
    }

    fn args(rest: &[&str], allow_remote: bool) -> Args {
        Args { rest: rest.iter().map(|s| s.to_string()).collect(), unit: "ROB_1".into(), seconds: 0.0, take: false, allow_remote }
    }

    fn refused(cmd: &str, a: &Args) -> String {
        match plan(cmd, a) {
            Err(e) => e,
            Ok(_) => panic!("{cmd} {:?} was taken", a.rest),
        }
    }

    #[test]
    fn a_command_missing_its_parts_is_refused_not_a_crash() {
        assert_eq!(refused("stream", &args(&[], false)), "a controller address is needed");
        assert_eq!(refused("stream", &args(&["127.0.0.1"], false)), "name at least one SIGNAL[:AXIS]");
        assert!(matches!(plan("stream", &args(&["127.0.0.1", "4002:2"], false)), Ok(Plan::Stream(_, k)) if k.len() == 1));
        assert_eq!(refused("typed", &args(&[], false)), "a controller address is needed");
        assert_eq!(refused("hello", &args(&[], false)), "a controller address is needed");
        assert_eq!(refused("rws", &args(&[], false)), "a controller address is needed");
        assert!(refused("dance", &args(&[], false)).contains("unknown command"));
    }

    #[test]
    fn a_long_typed_run_on_a_controller_elsewhere_needs_saying_so() {
        let e = refused("typed", &args(&["192.0.2.77", "1-200"], false));
        assert!(e.contains("200 numbers") && e.contains("--allow-remote"), "{e}");
        assert!(matches!(plan("typed", &args(&["192.0.2.77", "1-200"], true)), Ok(Plan::Typed(_, n, _)) if n.len() == 200));
        assert!(matches!(plan("typed", &args(&["192.0.2.77", "1-120"], false)), Ok(Plan::Typed(..))));
        assert!(matches!(plan("typed", &args(&["127.0.0.1:61000", "1-200"], false)), Ok(Plan::Typed(..))), "a virtual controller on this PC");
        assert!(numbers("1-20000,20001-40000").unwrap_err().contains("at most 20000"));
        assert_eq!(numbers("1-20000").map(|n| n.len()), Ok(20_000));
    }

    #[test]
    fn a_stream_the_session_stops_ends_with_a_failure() {
        let fake = spy_core::fake::FakeController::start(spy_core::fake::Behaviour::default()).unwrap();
        let port = fake.port();
        let other = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(1500));
            let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            use std::io::Write;
            for frame in [spy_core::request::hello(1), spy_core::request::Command::StreamConnect.frame(2, "127.0.0.1"), spy_core::request::Command::UndefineAll.frame(3, "127.0.0.1")] {
                s.write_all(&frame).unwrap();
                std::thread::sleep(Duration::from_millis(50));
            }
            std::thread::sleep(Duration::from_secs(6));
        });
        let unit = MechUnit::new("ROB_1").unwrap();
        let a = Args { seconds: 6.0, ..args(&[], false) };
        let code = cmd_stream(Target { host: "127.0.0.1".into(), port }, &a, vec![channel("6000", &unit).unwrap()]);
        assert_eq!(code, ExitCode::from(1), "a session another program stopped ended as a success");
        other.join().unwrap();
        let fine = cmd_stream(Target { host: "127.0.0.1".into(), port }, &Args { seconds: 1.0, ..args(&[], false) }, vec![channel("6000", &unit).unwrap()]);
        assert_eq!(fine, ExitCode::SUCCESS);
    }

    #[test]
    fn every_step_of_a_run_is_counted_across_batches() {
        let k = ChannelKey { signal: 6000, unit: MechUnit::new("ROB_1").unwrap(), axis: Axis::new(1).unwrap() };
        let mut r = RunStats::default();
        r.feed(&batch(&k, &[0, 4], 1000));
        r.feed(&batch(&k, &[8, 13], 1004));
        r.feed(&batch(&k, &[17], 1250));
        assert_eq!(r.steps_of(&k), BTreeMap::from([(4, 3), (5, 1)]));
        assert_eq!(r.max_wait, Duration::from_millis(246));
        assert_eq!(r.waits_over_100ms, 1);
    }
}
