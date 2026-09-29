//! The real session worker against the in-process fake controller, which plays the
//! RW6 VC byte for byte (see `spy_core::fake`). Every scenario runs on its own fake,
//! on a port the OS picks, over loopback.

// Each scenario states its departures from the fake's defaults one line at a time.
#![allow(clippy::field_reassign_with_default)]

use std::io::Write;
use std::net::{SocketAddr, TcpListener};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use spy_core::discovery::{self, VcFinder};
use spy_core::fake::{Behaviour, FakeController, IdPools, SignalDef, SignalSource};
use spy_core::log::LogBook;
use spy_core::request::{Axis, Command, Define, MechUnit};
use spy_core::sample::{self, Record, RecordValues, ValueKind};
use spy_core::session::{AskPolicy, ChannelState, Options, Phase, Session, Target};
use spy_core::store::{ChannelKey, Store};
use spy_core::wire::{self, cause, rad_format, rad_kind, service, RadOut};

fn opts() -> Options {
    Options {
        connect_timeout: Duration::from_millis(1500),
        handshake_timeout: Duration::from_millis(1500),
        reply_timeout: Duration::from_millis(800),
        stale_after: Duration::from_millis(300),
        stall_after: Duration::from_millis(1000),
        // Past the stale bound, so a dead feed shows stale before it stops.
        probe_after: Duration::from_millis(500),
        teardown_wait: Duration::from_millis(1000),
        ladder: vec![Duration::from_millis(100), Duration::from_millis(200), Duration::from_millis(400)],
        ask: AskPolicy::Never,
        held_wait: Duration::from_secs(4),
        held_poll: Duration::from_millis(200),
        // The fake is reached over loopback like a VC; its tests are of the fast exit
        // a real controller gets, except where a test turns this on.
        vc_pause_patience: Duration::ZERO,
        // Never this PC's own virtual controllers: only the tests that hand in fakes.
        find_vc: VcFinder::none(),
    }
}

/// Handshakes these ports, as the product handshakes every port a VC process
/// listens on, and gives each one that answered with its system id.
fn finder(ports: Arc<Mutex<Vec<u16>>>) -> VcFinder {
    VcFinder::new(move |timeout| ports.lock().unwrap().iter().filter_map(|&p| discovery::hello(SocketAddr::from(([127, 0, 0, 1], p)), timeout).ok().map(|a| (p, a.system_id))).collect())
}

/// Requests (by property) from connections other than those listed.
fn props_from_others(f: &FakeController, not: &[usize], prop: &str) -> usize {
    f.seen().iter().filter(|x| !not.contains(&x.conn) && x.property == prop).count()
}

fn spawn(o: Options) -> Session {
    Session::spawn(o, Arc::new(LogBook::new()), Arc::new(Store::new()), Arc::new(|| {}))
}

fn key(signal: u32, unit: &str, axis: u8) -> ChannelKey {
    ChannelKey { signal, unit: MechUnit::new(unit).unwrap(), axis: Axis::new(axis).unwrap() }
}

fn target(f: &FakeController) -> Target {
    Target { host: "127.0.0.1".into(), port: f.port() }
}

fn wait_for(ms: u64, mut f: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    f()
}

fn phase(s: &Session) -> Phase {
    s.status().phase.clone()
}

fn last(s: &Session, k: &ChannelKey) -> Option<f64> {
    s.store().get(k).and_then(|c| c.lock().last().map(|(_, v)| v))
}

fn samples(s: &Session, k: &ChannelKey) -> u64 {
    s.status().channels.iter().find(|c| &c.key == k).map(|c| c.samples).unwrap_or(0)
}

fn state(s: &Session, k: &ChannelKey) -> Option<ChannelState> {
    s.status().channels.iter().find(|c| &c.key == k).map(|c| c.state.clone())
}

fn streaming_with_samples(s: &Session, keys: &[ChannelKey]) -> bool {
    phase(s) == Phase::Streaming && keys.iter().all(|k| samples(s, k) > 5)
}

fn log_text(s: &Session) -> String {
    s.log().since(0).iter().map(|e| e.text.clone()).collect::<Vec<_>>().join("\n")
}

fn stopped_reason(s: &Session) -> String {
    match phase(s) {
        Phase::Stopped { reason } => reason,
        other => format!("not stopped: {other:?}"),
    }
}

/// Another tool driving InfoStream by hand, the way any RobAPI client does: one
/// command, then a pause for its reply (the fake answers at once; a controller takes
/// milliseconds to seconds).
struct OtherTool {
    s: std::net::TcpStream,
    txn: u16,
    pause: Duration,
}

impl OtherTool {
    fn connect(f: &FakeController, pause: Duration) -> OtherTool {
        let s = std::net::TcpStream::connect(("127.0.0.1", f.port())).unwrap();
        OtherTool { s, txn: 0, pause }
    }
    fn send(&mut self, c: Command) {
        self.txn += 1;
        self.s.write_all(&c.frame(self.txn, "127.0.0.1")).unwrap();
        std::thread::sleep(self.pause);
    }
    fn subscribe(&mut self) {
        self.txn += 1;
        self.s.write_all(&spy_core::request::subscribe(self.txn, "127.0.0.1")).unwrap();
        std::thread::sleep(self.pause);
    }
    fn define(&mut self, channel: u8, signal: u32, axis: u8) {
        self.send(Command::Define(Define { channel, signal, unit: MechUnit::new("ROB_1").unwrap(), axis: Axis::new(axis).unwrap() }));
    }
}

/// Every value in a channel's history.
fn history(s: &Session, k: &ChannelKey) -> Vec<f64> {
    s.store().get(k).map(|c| c.lock().range(i64::MIN, i64::MAX).map(|(_, v)| v).collect()).unwrap_or_default()
}

#[test]
fn setup_defines_streams_and_tears_down() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(opts());
    let keys = vec![key(4002, "ROB_1", 1), key(4002, "ROB_2", 3), key(9888, "ROB_1", 1), key(6000, "ROB_1", 1)];
    s.set_channels(keys.clone());
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, &keys)), "never streamed: {:?}\n{}", phase(&s), log_text(&s));

    // The protocol, in order, and the zero-based axis on the wire.
    let seen = fake.seen();
    let props: Vec<&str> = seen.iter().map(|x| x.property.as_str()).collect();
    assert_eq!(&props[..3], &["SetProtocol", "StreamConnect", ""][..], "{props:?}");
    assert_eq!(seen[2].verb, "SUBSCRIBE");
    let defines: Vec<&str> = seen.iter().filter(|x| x.property == "StreamDefine").map(|x| x.args.as_str()).collect();
    assert_eq!(defines.len(), 4);
    assert!(defines[0].contains("-Signal 4002 -MechUnit ROB_1 -Axis 0 "), "{}", defines[0]);
    assert!(defines[1].contains("-Signal 4002 -MechUnit ROB_2 -Axis 2 "), "{}", defines[1]);
    assert!(props.contains(&"StartStream"));
    assert!(!props.contains(&"StreamUndefineAll"), "a normal connect must never undefine other clients' streams");

    // Each value under its own channel: the fake's stream ids are 215, 214 ... and
    // never equal a channel number, so this only passes if mapping is by stream id.
    assert_eq!(last(&s, &keys[0]), Some(101.0));
    assert_eq!(last(&s, &keys[1]), Some(203.0));
    assert_eq!(last(&s, &keys[2]), Some(-1.0));
    let st = s.status();
    assert_eq!(st.channels[2].kind, Some(ValueKind::Int), "the int signal must be decoded as int");
    assert_eq!(st.channels[0].sample_ms, Some(4.032));
    assert_eq!(st.channels[2].sample_ms, Some(24.192));
    assert_eq!(st.subscription, Some(spy_core::fake::SUBSCRIPTION_ID));
    drop(st);
    // Controller time: consecutive 4 ms samples step by exactly 4.
    let ch = s.store().get(&keys[3]).unwrap();
    let r = ch.lock();
    let pts: Vec<i64> = r.range(i64::MIN, i64::MAX).map(|(t, _)| t).collect();
    assert!(pts.windows(2).all(|w| w[1] - w[0] == 4), "stamp steps: {:?}", &pts[..pts.len().min(20)]);
    drop(r);

    let before = fake.seen().len();
    s.disconnect();
    assert!(wait_for(3000, || phase(&s) == Phase::Idle), "{:?}", phase(&s));
    let tail: Vec<String> = fake.seen()[before..].iter().map(|x| format!("{} {}", x.property, x.args)).collect();
    assert_eq!(tail[0], "StopStream ", "{tail:?}");
    assert_eq!(tail.iter().filter(|t| t.starts_with("StreamUndefine -StreamId")).count(), 4, "{tail:?}");
    assert_eq!(tail.last().unwrap(), "StreamDisconnect ");
    assert!(!tail.iter().any(|t| t.starts_with("StreamUndefineAll")));
    assert!(fake.streams().is_empty());
    assert!(wait_for(2000, || fake.open_connections() == 0));
}

#[test]
fn refusals_are_reported_and_not_retried_on_reconnect() {
    let mut b = Behaviour::default();
    b.units.insert("STN_1".into(), 1);
    let fake = FakeController::start(b).unwrap();
    let s = spawn(opts());
    let good = key(4002, "ROB_1", 1);
    let unknown_signal = key(1, "ROB_1", 1);
    let unknown_unit = key(4002, "NOPE_1", 1);
    let no_joint = key(4002, "STN_1", 2);
    s.set_channels(vec![good.clone(), unknown_signal.clone(), unknown_unit.clone(), no_joint.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&good))), "{}", log_text(&s));
    assert!(matches!(state(&s, &unknown_signal), Some(ChannelState::Refused { code: Some(-50228), .. })), "{:?}", state(&s, &unknown_signal));
    assert!(matches!(state(&s, &unknown_unit), Some(ChannelState::Refused { code: Some(-50229), .. })));
    assert!(matches!(state(&s, &no_joint), Some(ChannelState::Refused { code: Some(-303), .. })));
    assert!(log_text(&s).contains("controller's event log"), "the event-log cost must be stated");

    // A reconnect does not define the refused ones again: each would cost another
    // event-log entry on the controller.
    fake.drop_connections();
    assert!(wait_for(5000, || fake.connections_total() >= 2 && streaming_with_samples(&s, std::slice::from_ref(&good))));
    let n = fake.seen().iter().filter(|x| x.property == "StreamDefine" && x.args.contains("-Signal 1 ")).count();
    assert_eq!(n, 1, "the refused signal was defined again after a reconnect");
}

#[test]
fn a_dropped_connection_reconnects_and_keeps_history() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(opts());
    let k = key(6000, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    let held = s.store().get(&k).unwrap().lock().len();
    fake.drop_connections();
    assert!(wait_for(3000, || matches!(phase(&s), Phase::Reconnecting { .. }) || fake.connections_total() >= 2), "{:?}", phase(&s));
    assert!(wait_for(5000, || fake.connections_total() >= 2 && phase(&s) == Phase::Streaming && s.store().get(&k).unwrap().lock().len() > held + 20));
    // The outage is a gap in the history, not an interpolation.
    let r = s.store().get(&k).unwrap();
    let r = r.lock();
    let segs = r.decimate(r.first_t().unwrap(), r.last().unwrap().0 + 1, 100_000, |v| v);
    assert!(segs.len() >= 2, "the reconnect gap must break the trace");
}

#[test]
fn unreachable_controllers_fail_fast_and_clearly() {
    // Nothing listening: refused.
    let port = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let s = spawn(opts());
    s.connect(Target { host: "127.0.0.1".into(), port });
    assert!(wait_for(5000, || matches!(phase(&s), Phase::Stopped { .. })));
    let Phase::Stopped { reason } = phase(&s) else { unreachable!() };
    assert!(reason.contains("refused") || reason.contains("no answer"), "{reason}");
    // On this PC it is most likely a virtual controller started again since.
    assert!(reason.contains("new port at every start"), "{reason}");

    // A host that drops the SYN (TEST-NET-1, RFC 5737): bounded by the connect
    // timeout, and shutdown must not hang behind it.
    let mut o = opts();
    o.connect_timeout = Duration::from_millis(500);
    let s = spawn(o);
    let t0 = Instant::now();
    s.connect(Target { host: "192.0.2.1".into(), port: 5515 });
    assert!(wait_for(4000, || matches!(phase(&s), Phase::Stopped { .. })), "{:?}", phase(&s));
    assert!(!stopped_reason(&s).contains("virtual controller"), "no VC hint for a controller on the network: {}", stopped_reason(&s));
    assert!(t0.elapsed() < Duration::from_secs(4));
    let t1 = Instant::now();
    assert!(s.shutdown(Duration::from_secs(3)));
    assert!(t1.elapsed() < Duration::from_secs(3));

    // An unresolvable name.
    let s = spawn(opts());
    s.connect(Target { host: "no-such-controller.invalid".into(), port: 5515 });
    assert!(wait_for(8000, || matches!(phase(&s), Phase::Stopped { .. })));
}

#[test]
fn replies_are_matched_by_transaction() {
    let mut b = Behaviour::default();
    // Signal 4002's reply is held back and released just after the next define's
    // reply: the replies come in the other order from their requests, so matching
    // them in order would swap the two channels' streams.
    b.hold_define_of = Some(4002);
    b.release_held_after = true;
    let fake = FakeController::start(b).unwrap();
    let s = spawn(opts());
    let a = key(4002, "ROB_1", 1);
    let c = key(4000, "ROB_1", 1);
    s.set_channels(vec![a.clone(), c.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, &[a.clone(), c.clone()])), "{}", log_text(&s));
    assert_eq!((state(&s, &a), state(&s, &c)), (Some(ChannelState::Defined { stream: 215 }), Some(ChannelState::Defined { stream: 214 })));
    assert_eq!(last(&s, &a), Some(101.0), "4002 carries another signal's values");
    let v = last(&s, &c).unwrap();
    assert!((10.0..11.0).contains(&v), "4000 carries another signal's values: {v}");

    // The keepalive: service 4, cause 5, the controller's txn, ctrl echoed.
    fake.send_aya();
    assert!(wait_for(2000, || !fake.aya_answers().is_empty()));
    let ay = fake.aya_answers()[0].clone();
    assert_eq!((ay.txn, ay.cause, ay.ctrl1, ay.ctrl2), (0, 5, 4000, 16000));
    assert!(phase(&s) == Phase::Streaming, "answering the AYA must not disturb the session");

    // Every request carries its own txn, never 0.
    let txns: Vec<u16> = fake.seen().iter().map(|x| x.txn).collect();
    let uniq: std::collections::BTreeSet<u16> = txns.iter().copied().collect();
    assert_eq!(uniq.len(), txns.len(), "a txn was reused: {txns:?}");
    assert!(!uniq.contains(&0));
}

#[test]
fn a_reply_later_than_the_timeout_is_still_mapped() {
    let mut b = Behaviour::default();
    b.hold_define_of = Some(4002);
    let fake = FakeController::start(b).unwrap();
    let s = spawn(opts());
    let a = key(4002, "ROB_1", 1);
    s.set_channels(vec![a.clone()]);
    s.connect(target(&fake));
    // Only one define: its reply stays held, the define times out.
    assert!(wait_for(4000, || state(&s, &a) == Some(ChannelState::NoReply)), "{:?}", state(&s, &a));
    // Another define releases the held reply, after its window.
    let c = key(4000, "ROB_1", 1);
    s.set_channels(vec![a.clone(), c.clone()]);
    assert!(wait_for(5000, || streaming_with_samples(&s, &[a.clone(), c.clone()])), "{:?}\n{}", s.status().channels, log_text(&s));
    assert_eq!(last(&s, &a), Some(101.0));
    assert!(log_text(&s).contains("came late"));
}

#[test]
fn a_dead_feed_on_a_live_session_stops_without_grabbing_back() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(opts());
    let k = key(4002, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    let before = fake.seen().len();
    fake.with(|b| b.mute_all = true);
    // Stale first...
    assert!(wait_for(2000, || s.status().channels[0].stale));
    // ...then stopped, saying why.
    assert!(wait_for(4000, || matches!(phase(&s), Phase::Stopped { .. })), "{:?}", phase(&s));
    let Phase::Stopped { reason } = phase(&s) else { unreachable!() };
    assert!(reason.contains("taken InfoStream"), "{reason}");
    // Quietly: no controller-wide StopStream, no undefining ids that may be someone
    // else's by now, and no second attempt.
    let tail: Vec<String> = fake.seen()[before..].iter().map(|x| x.property.clone()).collect();
    assert_eq!(tail, vec!["StreamDisconnect".to_string()], "{tail:?}");
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(fake.connections_total(), 1, "it reconnected after losing InfoStream");
}

#[test]
fn a_silent_signal_is_stale_but_does_not_stop_the_session() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(opts());
    let k = key(9999, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || phase(&s) == Phase::Streaming));
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(phase(&s), Phase::Streaming);
    assert!(s.status().channels[0].stale, "a channel that never delivered must read stale");
    assert!(log_text(&s).contains("no samples in the first"));
}

#[test]
fn string_events_are_neither_stale_nor_a_stall() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(opts());
    // Four string events fill the VC's pool; a fifth is refused -50348.
    let texts: Vec<ChannelKey> = [221u32, 222, 225, 9872].iter().map(|&n| key(n, "ROB_1", 1)).collect();
    let fifth = key(9873, "ROB_1", 1);
    let mut all = texts.clone();
    all.push(fifth.clone());
    s.set_channels(all);
    s.connect(target(&fake));
    assert!(wait_for(5000, || phase(&s) == Phase::Streaming && texts.iter().all(|k| samples(&s, k) >= 1)), "{:?}", s.status().channels);
    assert!(matches!(state(&s, &fifth), Some(ChannelState::Refused { code: Some(-50348), .. })));
    let ch = s.store().get(&texts[3]).unwrap();
    assert_eq!(ch.lock().last_text.as_deref(), Some("wobj0"));
    assert_eq!(ch.lock().kind, Some(ValueKind::String));
    // Sent once, then quiet: that is normal for them, not a dead feed.
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(phase(&s), Phase::Streaming, "{}", log_text(&s));
    assert!(s.status().channels.iter().filter(|c| texts.contains(&c.key)).all(|c| !c.stale));
    assert!(texts.iter().all(|k| samples(&s, k) == 1), "a string event arrives once per StartStream");
}

#[test]
fn single_tenancy_is_reproduced_and_handled() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let a = spawn(opts());
    let ka = key(4002, "ROB_1", 1);
    a.set_channels(vec![ka.clone()]);
    a.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&a, std::slice::from_ref(&ka))));
    // (A client that starts in the same instant as A cannot be told from one whose
    // streams were already there; this one comes a moment later.)
    std::thread::sleep(Duration::from_millis(300));

    // A second client: defines, subscribes and starts like the first. Every sample
    // goes to the first subscriber (measured on the RW6 VC, 2026-09-24), so B gets
    // nothing while A is there. B's streams' samples start arriving at A mid-way,
    // which only another client starting to stream explains: A stops. (On the VC
    // B's define and StartStream also leave a pause in A's streams; the fake answers
    // too fast for one, so here it is the new stream that gives B away.)
    let b = spawn(opts());
    let kb = key(4000, "ROB_1", 2);
    b.set_channels(vec![kb.clone()]);
    b.connect(target(&fake));
    assert!(wait_for(5000, || phase(&b) == Phase::Streaming));
    assert!(wait_for(3000, || matches!(phase(&a), Phase::Stopped { .. })), "{:?}\n{}\n{:?}", phase(&a), log_text(&a), a.status().counters);
    assert!(stopped_reason(&a).contains("taken InfoStream"), "{}", stopped_reason(&a));
    // With A gone, B still gets nothing on its connection (measured: the controller
    // does not pass InfoStream on to a client that was already there), and is told
    // why and what to do...
    assert!(wait_for(3000, || log_text(&b).contains("InfoStream sends them all to one program")), "{}", log_text(&b));
    assert_eq!(samples(&b, &kb), 0);
    // ...which works: connected again, B streams.
    b.connect(target(&fake));
    assert!(wait_for(5000, || samples(&b, &kb) > 10), "{}", log_text(&b));
    assert_eq!(phase(&b), Phase::Streaming);
}

#[test]
fn other_clients_are_named_and_asked_about() {
    let mut beh = Behaviour::default();
    beh.extra_clients = vec!["192.0.2.27".into()];
    let fake = FakeController::start(beh).unwrap();
    let mut o = opts();
    o.ask = AskPolicy::Always;
    let s = spawn(o);
    let k = key(4002, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(4000, || phase(&s) == Phase::AwaitingApproval), "{:?}", phase(&s));
    let others = s.status().others.clone();
    assert_eq!(others.len(), 1, "{others:?}");
    assert_eq!(others[0].address, "192.0.2.27");
    assert!(!others[0].same_pc);
    assert!(fake.seen().is_empty(), "nothing may be sent before the answer");

    // No: nothing defined, nothing opened, connection closed.
    s.answer(false, &others);
    assert!(wait_for(3000, || matches!(phase(&s), Phase::Stopped { .. })));
    assert!(fake.seen().is_empty());
    assert!(wait_for(2000, || fake.open_connections() == 0));

    // Yes: streams.
    s.connect(target(&fake));
    assert!(wait_for(4000, || phase(&s) == Phase::AwaitingApproval));
    s.answer(true, &s.status().others.clone());
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));

    // A reconnect with the same clients does not ask again...
    let n = fake.connections_total();
    fake.drop_connections();
    assert!(wait_for(5000, || fake.connections_total() > n && phase(&s) == Phase::Streaming));
    // ...one with a new client does.
    fake.with(|b| b.extra_clients.push("10.0.0.9".into()));
    fake.drop_connections();
    assert!(wait_for(5000, || phase(&s) == Phase::AwaitingApproval), "{:?}", phase(&s));
    assert!(s.status().others.iter().any(|o| o.address == "10.0.0.9"));
}

#[test]
fn hostile_bytes_on_a_live_session() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(opts());
    let k = key(4002, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    let stream = match state(&s, &k) {
        Some(ChannelState::Defined { stream }) => stream,
        other => panic!("{other:?}"),
    };

    // A sample RAD whose record length points past the payload, and one whose
    // samples length wraps (a length that would overflow a position), on the mapped
    // stream.
    let mut rad = sample::encode_rad(spy_core::fake::SUBSCRIPTION_ID, 0, &[Record { stream, kind: ValueKind::Float, stamps: vec![1], values: RecordValues::Float(vec![777.0]) }], None);
    let rec_len_at = 16 + 8 + 4 + 1;
    rad[rec_len_at] = 0xFF;
    rad[rec_len_at + 1] = 0xFF;
    let frame = wire::encode_frame(0, service::EVENT, cause::EVENT, 0, 0, &[RadOut { kind: rad_kind::REPLY, format: rad_format::EVENT, data: &rad }]);
    fake.inject(&frame);
    let mut m = vec![0x08];
    wire::put_varint(&mut m, u64::from(stream));
    m.extend_from_slice(&[0x12, 0x00, 0x1A]);
    wire::put_varint(&mut m, u64::MAX - 11);
    m.extend_from_slice(&[0x55; 8]);
    let mut body = vec![1u8];
    body.extend_from_slice(&(m.len() as u16).to_be_bytes());
    body.extend_from_slice(&m);
    body.push(100);
    let mut rad2 = vec![0u8; 16];
    rad2.extend_from_slice(b"protobuf");
    rad2.extend_from_slice(&(body.len() as u32).to_be_bytes());
    rad2.extend_from_slice(&body);
    fake.inject(&wire::encode_frame(0, service::EVENT, cause::EVENT, 0, 0, &[RadOut { kind: rad_kind::REPLY, format: rad_format::EVENT, data: &rad2 }]));
    assert!(wait_for(2000, || s.status().counters.defects.values().sum::<u64>() >= 2), "{:?}", s.status().counters.defects);
    let n = samples(&s, &k);
    assert!(wait_for(2000, || samples(&s, &k) > n + 10), "decoding stopped after hostile records");
    assert_eq!(last(&s, &k), Some(101.0), "a hostile record's value got through");
    assert_eq!(fake.connections_total(), 1, "hostile records must not cost the session");

    // A header whose length is below any frame: a desync, the connection is
    // dropped and re-established at once.
    let mut h = vec![0xA1, 0xA2, 1, 12];
    h.extend_from_slice(&10u32.to_be_bytes());
    h.extend_from_slice(&[0, 0, 1, 0]);
    fake.inject(&h);
    assert!(wait_for(5000, || fake.connections_total() >= 2 && streaming_with_samples(&s, std::slice::from_ref(&k))), "{:?}", phase(&s));
    assert!(s.status().counters.desyncs >= 1);
}

#[test]
fn channels_can_change_while_streaming() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(opts());
    let a = key(4002, "ROB_1", 1);
    let c = key(4000, "ROB_1", 1);
    s.set_channels(vec![a.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&a))));
    // Answer like a controller, not instantly: each change then leaves a real pause
    // in every stream, which must read as this program's own doing, not a takeover.
    fake.with(|b| b.reply_delay = Duration::from_millis(40));
    // Added mid-stream: delivers only if StartStream follows the define.
    s.set_channels(vec![a.clone(), c.clone()]);
    assert!(wait_for(3000, || samples(&s, &c) > 5), "a channel added while streaming never delivered");
    let na = samples(&s, &a);
    assert!(wait_for(2000, || samples(&s, &a) > na + 5));
    // Removed mid-stream: undefined, and the rest keeps delivering (it stops unless
    // StartStream follows the undefine).
    let starts = || fake.seen_props().iter().filter(|p| *p == "StartStream").count();
    let started_before = starts();
    s.set_channels(vec![c.clone()]);
    assert!(wait_for(2000, || fake.streams().len() == 1), "{:?}", fake.streams());
    assert!(wait_for(2000, || starts() > started_before), "no StartStream followed the undefine: {:?}", fake.seen_props());
    // The pause shows as a gap once delivery resumes. Counting samples instead races:
    // the status can still be catching up on samples sent before the pause.
    assert!(wait_for(2000, || s.status().channels.iter().all(|ch| ch.gaps >= 1)), "the pauses were real: {:?}", s.status().channels);
    let nc = samples(&s, &c);
    assert!(wait_for(2000, || samples(&s, &c) > nc + 10), "delivery stopped after a mid-stream undefine");
    assert!(s.store().get(&a).is_some(), "a removed channel's history is the window's to drop, not the session's");
    assert_eq!(phase(&s), Phase::Streaming, "{}", log_text(&s));
    // The removed channel's last samples, still on their way when it was undefined,
    // are neither a channel's nor another client's.
    assert!(!log_text(&s).contains("not one of this program's channels"), "{}", log_text(&s));
    assert_eq!(s.status().counters.foreign_records, 0);
}

#[test]
fn a_port_that_is_not_robapi_is_named_as_such() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let t = std::thread::spawn(move || {
        if let Ok((mut c, _)) = l.accept() {
            let _ = c.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n");
            std::thread::sleep(Duration::from_millis(500));
        }
    });
    let s = spawn(opts());
    s.connect(Target { host: "127.0.0.1".into(), port });
    assert!(wait_for(4000, || matches!(phase(&s), Phase::Stopped { .. })));
    let Phase::Stopped { reason } = phase(&s) else { unreachable!() };
    assert!(reason.contains("not like a controller's RobAPI port"), "{reason}");
    t.join().unwrap();

    let mut b = Behaviour::default();
    b.mute_handshake = true;
    let fake = FakeController::start(b).unwrap();
    let s = spawn(opts());
    s.connect(target(&fake));
    assert!(wait_for(4000, || matches!(phase(&s), Phase::Stopped { .. })));
    let Phase::Stopped { reason } = phase(&s) else { unreachable!() };
    assert!(reason.contains("did not answer the RobAPI handshake"), "{reason}");
}

#[test]
fn reset_infostream_undefines_all_and_carries_on() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(opts());
    let k = key(4002, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    s.reset_infostream();
    assert!(wait_for(2000, || fake.seen_props().iter().any(|p| p == "StreamUndefineAll")));
    let n = samples(&s, &k);
    assert!(wait_for(3000, || samples(&s, &k) > n + 10), "{}", log_text(&s));
    assert_eq!(fake.streams().len(), 1);
}

#[test]
fn a_worker_panic_still_tears_the_session_down() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(opts());
    let keys = vec![key(4002, "ROB_1", 1), key(4000, "ROB_1", 1)];
    s.set_channels(keys.clone());
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, &keys)));
    let before = fake.seen().len();
    s.crash_for_test();
    assert!(wait_for(3000, || matches!(phase(&s), Phase::Stopped { .. })));
    assert!(wait_for(2000, || fake.seen().len() >= before + 4));
    let tail: Vec<String> = fake.seen()[before..].iter().map(|x| x.property.clone()).collect();
    assert_eq!(tail, vec!["StopStream", "StreamUndefine", "StreamUndefine", "StreamDisconnect"], "{tail:?}");
}

#[test]
fn dropping_the_handle_tears_down() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(opts());
    let k = key(4002, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    drop(s);
    let props = fake.seen_props();
    assert!(props.iter().any(|p| p == "StopStream") && props.iter().any(|p| p == "StreamUndefine") && props.last().map(String::as_str) == Some("StreamDisconnect"), "{props:?}");
    assert!(wait_for(2000, || fake.open_connections() == 0));
}

#[test]
fn a_controller_restart_rebases_the_timeline_forward() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(opts());
    let k = key(6000, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    let t_before = s.store().get(&k).unwrap().lock().last().unwrap().0;
    fake.set_clock_ms(1_000);
    fake.drop_connections();
    assert!(wait_for(5000, || s.status().counters.clock_resets == 1), "{}", log_text(&s));
    assert!(wait_for(3000, || s.store().get(&k).unwrap().lock().last().unwrap().0 > t_before + 50));
}

#[test]
fn a_restarted_virtual_controller_is_followed_to_its_new_port() {
    // A VC takes a new port at every start, a warm restart included (the RW6 VC's
    // moved from 45198 to 62097 on 2026-09-28): the old port refuses from then on,
    // and retrying it forever never reconnects.
    let mut old = FakeController::start(Behaviour::default()).unwrap();
    let ports = Arc::new(Mutex::new(vec![old.port()]));
    let s = spawn(Options { find_vc: finder(ports.clone()), ..opts() });
    let k = key(6000, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&old));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    let from = target(&old);
    let held = history(&s, &k).len();
    // Restarting: its port closes, and nothing answers anywhere yet (the product
    // handshakes only the ports a VC process listens on).
    old.stop();
    ports.lock().unwrap().clear();
    // A refused connection on Windows loopback takes about 2 s (the SYN is retried).
    assert!(wait_for(12000, || matches!(phase(&s), Phase::Reconnecting { attempt: 3.., .. })), "{:?}\n{}", phase(&s), log_text(&s));
    let new = FakeController::start(Behaviour::default()).unwrap();
    ports.lock().unwrap().push(new.port());
    assert!(
        wait_for(8000, || phase(&s) == Phase::Streaming && s.status().target == Some(target(&new)) && history(&s, &k).len() > held + 20),
        "{:?} {:?}\n{}",
        phase(&s),
        s.status().target,
        log_text(&s)
    );
    assert_eq!(s.status().moved, Some((from, target(&new))));
    assert!(log_text(&s).contains(&format!("now answers on port {}", new.port())), "{}", log_text(&s));
    assert_eq!(history(&s, &k)[..held].len(), held, "the same controller: its history carries on");
    assert!(new.seen_props().iter().any(|p| p == "StreamDefine"));
}

/// A session streaming from a VC with a RobotStudio-like client listed beside it
/// (handshake only); the VC then restarts on a new port, where that client
/// reconnects first and sends StreamConnect (measured 2026-09-28), and the session
/// follows.
/// `extra`: clients elsewhere, listed by both.
struct RestartedVc {
    s: Session,
    k: ChannelKey,
    new: FakeController,
    rs: OtherTool,
    n: u64,
}

fn restarted_vc(o: Options, extra: &[&str]) -> RestartedVc {
    let b = || Behaviour { extra_clients: extra.iter().map(|a| a.to_string()).collect(), ..Behaviour::default() };
    let mut old = FakeController::start(b()).unwrap();
    let mut rs = OtherTool::connect(&old, Duration::from_millis(20));
    rs.s.write_all(&spy_core::request::hello(1)).unwrap();
    let ports = Arc::new(Mutex::new(vec![old.port()]));
    let s = spawn(Options { find_vc: finder(ports.clone()), ..o });
    let k = key(6000, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&old));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    let n = samples(&s, &k);
    let (new, rs2) = restart(&mut old, &ports, b());
    drop(rs);
    RestartedVc { s, k, new, rs: rs2, n }
}

/// The VC restarts on a new port: RobotStudio's connection reconnects there first
/// and takes InfoStream (measured 2026-09-28); the finder then sees the new port.
fn restart(old: &mut FakeController, ports: &Arc<Mutex<Vec<u16>>>, b: Behaviour) -> (FakeController, OtherTool) {
    old.stop();
    ports.lock().unwrap().clear();
    let new = FakeController::start(b).unwrap();
    let mut rs = OtherTool::connect(&new, Duration::from_millis(20));
    rs.s.write_all(&spy_core::request::hello(1)).unwrap();
    rs.send(Command::StreamConnect);
    ports.lock().unwrap().push(new.port());
    (new, rs)
}

#[test]
fn each_outage_gets_its_own_one_reconnect() {
    let mut first = FakeController::start(Behaviour::default()).unwrap();
    let mut rs = OtherTool::connect(&first, Duration::from_millis(20));
    rs.s.write_all(&spy_core::request::hello(1)).unwrap();
    let ports = Arc::new(Mutex::new(vec![first.port()]));
    let s = spawn(Options { find_vc: finder(ports.clone()), ..opts() });
    let k = key(6000, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&first));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    let streaming_again = |n: u64| wait_for(15000, || phase(&s) == Phase::Streaming && samples(&s, &k) > n + 20);
    // Two restarts: each followed, each connected again once, by itself.
    let (mut second, rs2) = restart(&mut first, &ports, Behaviour::default());
    drop(rs);
    assert!(streaming_again(samples(&s, &k)), "{:?}\n{}", phase(&s), log_text(&s));
    let (third, rs3) = restart(&mut second, &ports, Behaviour::default());
    drop(rs2);
    assert!(streaming_again(samples(&s, &k)), "{:?}\n{}", phase(&s), log_text(&s));
    assert_eq!(log_text(&s).matches("connecting again, once").count(), 2, "{}", log_text(&s));
    // Then a drop with no restart: the other connection's hold is not taken by itself.
    third.drop_connections();
    drop(rs3);
    let mut rs4 = OtherTool::connect(&third, Duration::from_millis(20));
    rs4.s.write_all(&spy_core::request::hello(1)).unwrap();
    rs4.send(Command::StreamConnect);
    assert!(wait_for(15000, || matches!(phase(&s), Phase::Stopped { .. })), "{:?}\n{}", phase(&s), log_text(&s));
    assert_eq!(log_text(&s).matches("connecting again, once").count(), 2, "{}", log_text(&s));
}

#[test]
fn a_restarted_vc_whose_own_robotstudio_took_infostream_is_connected_again_once() {
    // Measured on the RW6 VC (2026-09-28): this program leaving ends RobotStudio's
    // hold, and the next connection gets the samples. So the session leaves and comes
    // back by itself, once.
    let v = restarted_vc(opts(), &[]);
    let (s, k) = (&v.s, &v.k);
    let mut stopped = false;
    assert!(
        wait_for(15000, || {
            stopped |= matches!(phase(s), Phase::Stopped { .. });
            phase(s) == Phase::Streaming && samples(s, k) > v.n + 20
        }),
        "{:?}\n{}",
        phase(s),
        log_text(s)
    );
    assert!(!stopped, "it came back by itself\n{}", log_text(s));
    assert_eq!(log_text(s).matches("connecting again, once").count(), 1, "{}", log_text(s));
    let connects = v.new.seen().iter().filter(|x| x.property == "StreamConnect").count();
    assert_eq!(connects, 3, "the other client's, the follow's, and the one reconnect's");
}

#[test]
fn a_restarted_vc_is_not_connected_again_by_itself_with_a_client_elsewhere() {
    // Someone on another PC may be the one showing signals: asked of the person.
    let v = restarted_vc(opts(), &["192.0.2.27"]);
    assert!(wait_for(15000, || matches!(phase(&v.s), Phase::Stopped { .. })), "{:?}\n{}", phase(&v.s), log_text(&v.s));
    let reason = stopped_reason(&v.s);
    assert!(reason.contains("RobotStudio's own") && reason.contains("Connect again"), "{reason}");
    assert!(!log_text(&v.s).contains("connecting again, once"), "{}", log_text(&v.s));
    // As it says: connected again, this program gets the samples.
    let n = samples(&v.s, &v.k);
    v.s.connect(target(&v.new));
    assert!(wait_for(5000, || phase(&v.s) == Phase::Streaming && samples(&v.s, &v.k) > n + 20), "{:?}\n{}", phase(&v.s), log_text(&v.s));
}

#[test]
fn a_vc_that_did_not_restart_is_not_connected_again_by_itself() {
    // The connection dropped, the port did not move: the controller did not restart,
    // so nothing says the other connection's hold is RobotStudio's automatic one.
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let mut rs = OtherTool::connect(&fake, Duration::from_millis(20));
    rs.s.write_all(&spy_core::request::hello(1)).unwrap();
    let s = spawn(Options { find_vc: finder(Arc::new(Mutex::new(vec![fake.port()]))), ladder: vec![Duration::from_millis(400)], ..opts() });
    let k = key(6000, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    fake.drop_connections();
    let mut rs = OtherTool::connect(&fake, Duration::from_millis(20));
    rs.s.write_all(&spy_core::request::hello(1)).unwrap();
    rs.send(Command::StreamConnect);
    assert!(wait_for(15000, || matches!(phase(&s), Phase::Stopped { .. })), "{:?}\n{}", phase(&s), log_text(&s));
    assert!(!log_text(&s).contains("connecting again, once"), "{}", log_text(&s));
    assert!(!log_text(&s).contains("now answers on port"), "{}", log_text(&s));
}

#[test]
fn the_one_reconnect_after_a_vc_restart_is_not_repeated() {
    // A program on this PC takes InfoStream again straight after this program left,
    // on a fresh connection of its own (an orphaned one gets nothing, whatever it
    // sends, measured 2026-09-25): one showing signals, not RobotStudio's hold at the
    // start.
    let v = restarted_vc(Options { ladder: vec![Duration::from_millis(100), Duration::from_millis(1000)], ..opts() }, &[]);
    let RestartedVc { s, new, rs, .. } = v;
    let again = std::thread::scope(|sc| {
        sc.spawn(|| {
            // This program set up there (the other client defines nothing), then left.
            assert!(wait_for(15000, || !new.streams().is_empty()));
            assert!(wait_for(15000, || new.open_connections() < 2));
            drop(rs);
            let mut again = OtherTool::connect(&new, Duration::from_millis(20));
            again.s.write_all(&spy_core::request::hello(1)).unwrap();
            again.send(Command::StreamConnect);
            again
        })
        .join()
        .unwrap()
    });
    assert!(wait_for(15000, || matches!(phase(&s), Phase::Stopped { .. })), "{:?}\n{}", phase(&s), log_text(&s));
    let reason = stopped_reason(&s);
    assert!(reason.contains("still no samples") && reason.contains("showing test signals"), "{reason}");
    assert_eq!(log_text(&s).matches("connecting again, once").count(), 1, "{}", log_text(&s));
    drop(again);
}

#[test]
fn a_controller_found_somewhere_new_at_every_look_is_followed_once_per_attempt() {
    // Each look finds it on yet another port, where nothing then answers: following
    // on from there would chase it inside one attempt, never retrying or stopping.
    let mut fake = FakeController::start(Behaviour::default()).unwrap();
    let looks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let n = looks.clone();
    let finder = VcFinder::new(move |_| {
        n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        vec![(port, Some(spy_core::fake::SYSTEM_ID.to_string()))]
    });
    let s = spawn(Options { find_vc: finder, connect_timeout: Duration::from_millis(300), ..opts() });
    let k = key(6000, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    fake.stop();
    assert!(wait_for(8000, || log_text(&s).matches("Cannot connect").count() >= 3), "{:?}\n{}", phase(&s), log_text(&s));
    s.disconnect();
    assert!(wait_for(3000, || phase(&s) == Phase::Idle));
    let (tries, follows) = (log_text(&s).matches("Cannot connect").count(), log_text(&s).matches("now answers on port").count());
    assert_eq!(follows, tries, "one follow per attempt:\n{}", log_text(&s));
    assert_eq!(looks.load(std::sync::atomic::Ordering::SeqCst), tries);
}

#[test]
fn a_controller_answering_on_two_new_ports_is_not_guessed_at() {
    // Two copies of one system in two stations, say: which one was being read?
    let mut fake = FakeController::start(Behaviour::default()).unwrap();
    let finder = VcFinder::new(|_| vec![(40001, Some(spy_core::fake::SYSTEM_ID.to_string())), (40002, Some(spy_core::fake::SYSTEM_ID.to_string()))]);
    let s = spawn(Options { find_vc: finder, connect_timeout: Duration::from_millis(300), ..opts() });
    let k = key(6000, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    let at = target(&fake);
    fake.stop();
    assert!(wait_for(8000, || log_text(&s).matches("Cannot connect").count() >= 3), "{:?}\n{}", phase(&s), log_text(&s));
    assert_eq!(s.status().target, Some(at));
    assert_eq!(s.status().moved, None);
    assert_eq!(log_text(&s).matches("answers on more than one port (40001, 40002): not guessing").count(), 1, "said once:\n{}", log_text(&s));
}

#[test]
fn another_virtual_controller_is_never_taken_for_a_restarted_one() {
    let mut old = FakeController::start(Behaviour::default()).unwrap();
    let mut b = Behaviour::default();
    b.system_id = "{0000000B-0000-4000-8000-00000000000B}".into();
    let other = FakeController::start(b).unwrap();
    let ports = Arc::new(Mutex::new(vec![old.port(), other.port()]));
    let s = spawn(Options { find_vc: finder(ports.clone()), ..opts() });
    let k = key(6000, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&old));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    old.stop();
    ports.lock().unwrap().retain(|&p| p == other.port());
    assert!(wait_for(15000, || log_text(&s).matches("Cannot connect").count() >= 4), "{:?}\n{}", phase(&s), log_text(&s));
    assert_eq!(s.status().target, Some(target(&old)), "it keeps trying its own controller's address");
    assert_eq!(s.status().moved, None);
    assert!(other.seen().is_empty(), "nothing but handshakes went to the other controller: {:?}", other.seen_props());
    let log = log_text(&s);
    assert_eq!(log.matches("{0000000B-0000-4000-8000-00000000000B}").count(), 1, "the other one is named, once:\n{log}");

    // Back where it was, then gone again: a new outage, named again.
    let port = old.port();
    let mut back = FakeController::start_on(port, Behaviour::default()).unwrap();
    assert!(wait_for(8000, || phase(&s) == Phase::Streaming && back.seen_props().iter().any(|p| p == "StartStream")), "{}", log_text(&s));
    back.stop();
    assert!(wait_for(15000, || log_text(&s).matches("{0000000B-0000-4000-8000-00000000000B}").count() == 2), "{}", log_text(&s));
}

#[test]
fn another_tool_taking_infostream_is_caught_before_its_signals_show() {
    // Measured on the RW6 VC (2026-09-25): a second client's StreamUndefineAll
    // removes this program's streams, its defines get the freed ids, and its samples
    // arrive here, the first subscriber, under this program's ids. Its signals would
    // be shown as this program's channels, live.
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let a = spawn(opts());
    let k1 = key(6000, "ROB_1", 1);
    let k2 = key(6001, "ROB_1", 1);
    a.set_channels(vec![k1.clone(), k2.clone()]);
    a.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&a, &[k1.clone(), k2.clone()])));
    let before = fake.seen().len();

    let mut b = OtherTool::connect(&fake, Duration::from_millis(40));
    b.send(Command::StreamConnect);
    b.send(Command::UndefineAll);
    b.define(0, 4002, 1);
    b.define(1, 4000, 3);
    let ids: Vec<(u32, u32)> = fake.streams().iter().map(|s| (s.0, s.1)).collect();
    assert_eq!(ids, vec![(214, 4000), (215, 4002)], "the fake must hand out the freed ids, as the VC does");
    b.send(Command::StartStream);

    assert!(wait_for(3000, || matches!(phase(&a), Phase::Stopped { .. })), "{:?}\n{}", phase(&a), log_text(&a));
    let reason = stopped_reason(&a);
    assert!(reason.contains("taken InfoStream"), "{reason}");
    // Nothing of the other tool's reached these channels: 4002 reads 101 and 4000
    // reads 30.x here, and a joint angle from the fake never leaves [-1, 1].
    for k in [&k1, &k2] {
        let h = history(&a, k);
        assert!(h.iter().all(|v| v.abs() <= 1.0), "{k} shows another tool's signal: {:?}", h.iter().filter(|v| v.abs() > 1.0).take(3).collect::<Vec<_>>());
    }
    // Quietly: no controller-wide StopStream, and no undefining ids that are now the
    // other tool's.
    let a_tail: Vec<String> = fake.seen()[before..].iter().filter(|x| x.conn == 1).map(|x| x.property.clone()).collect();
    assert_eq!(a_tail, vec!["StreamDisconnect".to_string()], "{a_tail:?}");
    // Yet they are gone: the first subscriber's exit, whatever it sends, clears every
    // stream on the controller (measured 2026-09-26). Only leaving before they are
    // defined spares them.
    assert!(wait_for(1000, || fake.streams().is_empty()), "{:?}", fake.streams());
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(fake.connections_total(), 2, "it went back for InfoStream");
}

#[test]
fn a_takeover_is_left_before_the_newcomer_sets_up_its_signals() {
    // Measured 2026-09-26: this program's exit, whatever it sends, clears every stream
    // on the controller, the newcomer's included. A test-signal client seen clearing
    // every stream waited 500 ms before its first define: leaving inside that wait
    // spares all of it.
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let a = spawn(opts());
    let k = key(6000, "ROB_1", 1);
    a.set_channels(vec![k.clone()]);
    a.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&a, std::slice::from_ref(&k))));
    let before = fake.seen().len();
    let mut b = OtherTool::connect(&fake, Duration::from_millis(20));
    b.send(Command::StreamConnect);
    b.send(Command::UndefineAll);
    // The newcomer's wait; this test's probe_after is 500 ms.
    std::thread::sleep(Duration::from_millis(800));
    assert!(matches!(phase(&a), Phase::Stopped { .. }), "still {:?} when the newcomer defines\n{}", phase(&a), log_text(&a));
    b.define(0, 4002, 1);
    b.define(1, 4000, 3);
    std::thread::sleep(Duration::from_millis(300));
    let ids: Vec<(u32, u32)> = fake.streams().iter().map(|s| (s.0, s.1)).collect();
    assert_eq!(ids, vec![(214, 4000), (215, 4002)], "the newcomer's streams must survive");
    let reason = stopped_reason(&a);
    assert!(reason.contains("taken InfoStream"), "{reason}");
    assert!(reason.contains("on this PC"), "the newcomer is named: {reason}");
    // Quietly, as ever: StreamDisconnect only.
    let a_tail: Vec<String> = fake.seen()[before..].iter().filter(|x| x.conn == 1).map(|x| x.property.clone()).collect();
    assert_eq!(a_tail, vec!["StreamDisconnect".to_string()], "{a_tail:?}");
    assert_eq!(a.status().counters.liveness_checks, 1);
}

#[test]
fn a_network_stall_that_clears_is_not_a_takeover() {
    // Everything held on the way (samples, replies), then delivered in order: the
    // samples still in flight arrive before the answer to the liveness check, so the
    // check sees them and the session carries on.
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let a = spawn(opts());
    let k = key(6000, "ROB_1", 1);
    a.set_channels(vec![k.clone()]);
    a.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&a, std::slice::from_ref(&k))));
    fake.with(|b| b.hold_delivery = true);
    assert!(wait_for(2000, || a.status().counters.liveness_checks == 1), "no liveness check\n{}", log_text(&a));
    std::thread::sleep(Duration::from_millis(100));
    fake.with(|b| b.hold_delivery = false);
    let n = samples(&a, &k);
    assert!(wait_for(2000, || samples(&a, &k) > n + 50), "{:?}\n{}", phase(&a), log_text(&a));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(phase(&a), Phase::Streaming, "{}", log_text(&a));
}

#[test]
fn a_change_of_this_programs_own_while_the_check_is_out_is_not_a_takeover() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let mut o = opts();
    o.stall_after = Duration::from_secs(3);
    let a = spawn(o);
    let x = key(6000, "ROB_1", 1);
    let y = key(6001, "ROB_1", 1);
    a.set_channels(vec![x.clone()]);
    a.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&a, std::slice::from_ref(&x))));
    // A slow controller (every request answered 600 ms late), and the feed pauses.
    fake.with(|b| {
        b.reply_delay = Duration::from_millis(600);
        b.mute_all = true;
    });
    assert!(wait_for(2000, || a.status().counters.liveness_checks == 1), "no liveness check\n{}", log_text(&a));
    // While the check is out the person adds a channel: its define pauses delivery
    // until this program's StartStream, so the silence is now its own doing.
    a.set_channels(vec![x.clone(), y.clone()]);
    // The answer goes out with still no sample since the check (the define is
    // queued behind it on the slow controller); only then does the feed come back.
    assert!(wait_for(3000, || fake.handshakes() == 2), "the check was never answered");
    std::thread::sleep(Duration::from_millis(100));
    fake.with(|b| b.mute_all = false);
    assert!(wait_for(5000, || samples(&a, &y) > 20), "{:?}\n{}", phase(&a), log_text(&a));
    assert_eq!(phase(&a), Phase::Streaming, "{}", log_text(&a));
}

#[test]
fn the_flexpendant_alone_is_not_asked_about() {
    // Every real IRC5 lists its FlexPendant, on its internal network (192.168.126.10 on
    // the measured cell). Asking about it on every connection teaches people to click
    // through the question (decided 2026-09-26).
    let mut beh = Behaviour::default();
    beh.extra_clients = vec!["192.168.126.10".into()];
    let fake = FakeController::start(beh).unwrap();
    let mut o = opts();
    o.ask = AskPolicy::Always;
    let s = spawn(o);
    let k = key(4002, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))), "{:?}\n{}", phase(&s), log_text(&s));
    let others = s.status().others.clone();
    assert_eq!(others.len(), 1, "{others:?}");
    assert!(others[0].pendant, "{others:?}");
}

#[test]
fn another_client_beside_the_flexpendant_is_asked_about() {
    let mut beh = Behaviour::default();
    beh.extra_clients = vec!["192.168.126.10".into(), "192.0.2.27".into()];
    let fake = FakeController::start(beh).unwrap();
    let mut o = opts();
    o.ask = AskPolicy::Always;
    let s = spawn(o);
    s.set_channels(vec![key(4002, "ROB_1", 1)]);
    s.connect(target(&fake));
    assert!(wait_for(4000, || phase(&s) == Phase::AwaitingApproval), "{:?}", phase(&s));
    let waiting = log_text(&s).lines().find(|l| l.contains("Waiting for your decision")).unwrap_or_default().to_string();
    assert!(waiting.contains("192.0.2.27") && !waiting.contains("192.168.126.10"), "{waiting}");
}

#[test]
fn the_flexpendant_is_not_taken_for_a_program_getting_the_samples() {
    // Nothing arrives on this connection: the advice to close another program's
    // signal view must not point at the pendant, which every real IRC5 lists.
    let mut beh = Behaviour::default();
    beh.extra_clients = vec!["192.168.126.10".into()];
    beh.mute_all = true;
    let fake = FakeController::start(beh).unwrap();
    let s = spawn(opts());
    s.set_channels(vec![key(4002, "ROB_1", 1)]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || phase(&s) == Phase::Streaming));
    std::thread::sleep(Duration::from_millis(1600));
    assert!(!log_text(&s).contains("other program(s) are connected"), "{}", log_text(&s));
    assert!(s.status().advice.is_none(), "{:?}", s.status().advice);
}

#[test]
fn the_pendant_reconnecting_is_not_blamed_for_a_takeover() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let a = spawn(opts());
    let k = key(6000, "ROB_1", 1);
    a.set_channels(vec![k.clone()]);
    a.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&a, std::slice::from_ref(&k))));
    fake.with(|b| {
        b.extra_clients = vec!["192.168.126.10".into()];
        b.mute_all = true;
    });
    assert!(wait_for(2000, || matches!(phase(&a), Phase::Stopped { .. })), "{:?}\n{}", phase(&a), log_text(&a));
    let reason = stopped_reason(&a);
    assert!(!reason.contains("192.168.126.10"), "the pendant was blamed: {reason}");
    assert!(reason.contains("taken InfoStream"), "{reason}");
}

#[test]
fn the_irc5s_stream_id_pools_change_nothing() {
    // The real IRC5 numbers streams from three pools (measured 2026-09-26): 259 down, its
    // drive-side signals from 17 down, text from 260 up. Nothing may lean on the VC's
    // 215 and 233.
    let mut beh = Behaviour::default();
    beh.id_pools = IdPools::Irc5;
    let fake = FakeController::start(beh).unwrap();
    let a = spawn(opts());
    let dc = key(5027, "ROB_1", 1);
    let tq = key(4002, "ROB_1", 1);
    let wo = key(9872, "ROB_1", 1);
    a.set_channels(vec![dc.clone(), tq.clone(), wo.clone()]);
    a.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&a, &[dc.clone(), tq.clone()])), "{:?}\n{}", phase(&a), log_text(&a));
    assert_eq!(state(&a, &dc), Some(ChannelState::Defined { stream: 17 }));
    assert_eq!(state(&a, &tq), Some(ChannelState::Defined { stream: 259 }));
    assert_eq!(state(&a, &wo), Some(ChannelState::Defined { stream: 260 }));
    assert!(last(&a, &dc).is_some_and(|v| (v - 356.7).abs() < 0.01), "{:?}", last(&a, &dc));
    assert_eq!(last(&a, &tq), Some(101.0));
    // A takeover on these ids is still caught: the newcomer's ROB_2 DC link is handed
    // 17, this program's ROB_1 DC link's id.
    let mut b = OtherTool::connect(&fake, Duration::from_millis(20));
    b.send(Command::UndefineAll);
    b.send(Command::Define(Define { channel: 0, signal: 5027, unit: MechUnit::new("ROB_2").unwrap(), axis: Axis::new(1).unwrap() }));
    assert_eq!(fake.streams().first().map(|s| (s.0, s.2.clone())), Some((17, "ROB_2".to_string())));
    b.send(Command::StartStream);
    assert!(wait_for(3000, || matches!(phase(&a), Phase::Stopped { .. })), "{:?}\n{}", phase(&a), log_text(&a));
    assert!(history(&a, &dc).iter().all(|v| (v - 356.7).abs() < 0.01), "ROB_2's DC link was shown as ROB_1's");
}

#[test]
fn a_fast_takeover_is_caught_by_the_record_type() {
    // A takeover quicker than one sample leaves no gap; a stream that changes record
    // type still cannot be this program's.
    let mut beh = Behaviour::default();
    beh.signals.insert(3010, SignalDef { source: SignalSource::int(|_| 7), sample_ms: 4.032 });
    let fake = FakeController::start(beh).unwrap();
    let a = spawn(opts());
    let k = key(6000, "ROB_1", 1);
    a.set_channels(vec![k.clone()]);
    a.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&a, std::slice::from_ref(&k))));
    let mut b = OtherTool::connect(&fake, Duration::ZERO);
    b.send(Command::UndefineAll);
    b.define(0, 3010, 1);
    b.send(Command::StartStream);
    assert!(wait_for(3000, || matches!(phase(&a), Phase::Stopped { .. })), "{:?}", phase(&a));
    assert!(stopped_reason(&a).contains("taken InfoStream"), "{}", stopped_reason(&a));
    let ch = a.store().get(&k).unwrap();
    assert_eq!(ch.lock().kind, Some(ValueKind::Float), "an int record was filed under a float channel");
    assert!(history(&a, &k).iter().all(|&v| v != 7.0));
}

#[test]
fn a_pause_in_one_signal_is_a_gap_but_a_pause_in_all_of_them_is_not_trusted() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let a = spawn(opts());
    let k1 = key(6000, "ROB_1", 1);
    let k2 = key(4002, "ROB_1", 1);
    a.set_channels(vec![k1.clone(), k2.clone()]);
    a.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&a, &[k1.clone(), k2.clone()])));
    // One signal pauses while the other carries on: a gap in that channel, nothing more.
    fake.with(|b| b.mute.insert(4002));
    std::thread::sleep(Duration::from_millis(150));
    fake.with(|b| b.mute.remove(&4002));
    let n = samples(&a, &k2);
    assert!(wait_for(2000, || samples(&a, &k2) > n + 20));
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(phase(&a), Phase::Streaming, "{}", log_text(&a));
    assert!(a.status().channels.iter().find(|c| c.key == k2).unwrap().gaps >= 1);
    // Every stream pauses at once and resumes without this program asking: that is
    // what another client's takeover looks like, and nothing after it can be trusted.
    fake.with(|b| b.mute_all = true);
    std::thread::sleep(Duration::from_millis(150));
    fake.with(|b| b.mute_all = false);
    assert!(wait_for(3000, || matches!(phase(&a), Phase::Stopped { .. })), "{:?}", phase(&a));
    assert!(stopped_reason(&a).contains("taken InfoStream"), "{}", stopped_reason(&a));
}

#[test]
fn a_define_that_goes_unanswered_mid_stream_does_not_stop_the_rest() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    // A reply timeout longer than the stall timeout: the pause this program's own
    // define causes must not read as a lost feed while it waits.
    let mut o = opts();
    o.reply_timeout = Duration::from_millis(1500);
    let s = spawn(o);
    let a = key(4002, "ROB_1", 1);
    let c = key(4000, "ROB_1", 1);
    s.set_channels(vec![a.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&a))));
    // The define pauses delivery (measured) and its reply never comes.
    fake.with(|b| b.hold_define_of = Some(4000));
    s.set_channels(vec![a.clone(), c.clone()]);
    assert!(wait_for(3000, || state(&s, &c) == Some(ChannelState::NoReply)), "{:?}\n{}", state(&s, &c), log_text(&s));
    let n = samples(&s, &a);
    assert!(wait_for(2000, || samples(&s, &a) > n + 20), "delivery never resumed after an unanswered define\n{}", log_text(&s));
    assert_eq!(phase(&s), Phase::Streaming, "{}", log_text(&s));
    // Its reply comes after all, with the stream long since started: mapped, and
    // delivering, with nothing mistaken for a takeover.
    let d = key(4003, "ROB_1", 1);
    s.set_channels(vec![a.clone(), c.clone(), d.clone()]);
    assert!(wait_for(3000, || matches!(state(&s, &c), Some(ChannelState::Defined { .. })) && samples(&s, &c) > 20 && samples(&s, &d) > 20), "{:?}\n{}", s.status().channels, log_text(&s));
    assert!(log_text(&s).contains("came late"));
    assert_eq!(phase(&s), Phase::Streaming, "{}", log_text(&s));
    let v = last(&s, &c).unwrap();
    assert!((10.0..11.0).contains(&v), "4000 shows another signal: {v}");
}

#[test]
fn a_reset_with_an_unanswered_redefine_still_restarts_the_rest() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(opts());
    let a = key(4000, "ROB_1", 1);
    let c = key(4002, "ROB_1", 1);
    s.set_channels(vec![a.clone(), c.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, &[a.clone(), c.clone()])));
    fake.with(|b| b.hold_define_of = Some(4002));
    s.reset_infostream();
    assert!(wait_for(3000, || state(&s, &c) == Some(ChannelState::NoReply)), "{:?}", state(&s, &c));
    let n = samples(&s, &a);
    assert!(wait_for(2000, || samples(&s, &a) > n + 20), "the reset never restarted delivery\n{}", log_text(&s));
    assert_eq!(phase(&s), Phase::Streaming);
}

#[test]
fn a_reset_during_a_define_does_not_bind_the_channel_to_a_removed_stream() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(opts());
    let a = key(4002, "ROB_1", 1);
    let c = key(4000, "ROB_1", 1);
    s.set_channels(vec![a.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&a))));
    // 4000's define is processed (the stream exists, 214) but its reply is held back...
    fake.with(|b| b.hold_define_of = Some(4000));
    s.set_channels(vec![c.clone(), a.clone()]);
    assert!(wait_for(2000, || fake.streams().len() == 2));
    // ...and the reset's StreamUndefineAll removes that stream before the reply lands.
    // The redefines go out 4000 first, so the held reply ("214") arrives just before
    // 4000's new one ("215"), and 4002 gets 214: the stale reply names a stream that
    // now carries another signal.
    s.reset_infostream();
    assert!(wait_for(4000, || samples(&s, &c) > 20 && samples(&s, &a) > 20), "{:?}\n{}", s.status().channels, log_text(&s));
    let n = samples(&s, &c);
    assert!(wait_for(2000, || samples(&s, &c) > n + 20), "4000 is bound to a stream that no longer exists");
    assert_eq!(state(&s, &c), Some(ChannelState::Defined { stream: 215 }));
    assert_eq!(state(&s, &a), Some(ChannelState::Defined { stream: 214 }));
    assert_eq!(last(&s, &a), Some(101.0));
    let v = last(&s, &c).unwrap();
    assert!((10.0..11.0).contains(&v), "4000 shows another signal: {v}");
}

#[test]
fn a_late_refusal_cannot_unbind_a_defined_channel() {
    let mut beh = Behaviour::default();
    beh.hold_define_of = Some(4002);
    let fake = FakeController::start(beh).unwrap();
    let s = spawn(opts());
    let x = key(4002, "ROB_1", 1);
    s.set_channels(vec![x.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(3000, || state(&s, &x) == Some(ChannelState::NoReply)), "{:?}", state(&s, &x));
    // Off and on again: a second define. It releases the first one's reply (stream
    // 215) and is itself refused.
    fake.with(|b| {
        b.refuse_next.insert(4002, -50348);
    });
    s.set_channels(vec![]);
    s.set_channels(vec![x.clone()]);
    assert!(wait_for(3000, || fake.seen_props().iter().filter(|p| *p == "StreamDefine").count() == 2));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(state(&s, &x), Some(ChannelState::Defined { stream: 215 }), "{}", log_text(&s));
    let n = samples(&s, &x);
    assert!(wait_for(2000, || samples(&s, &x) > n + 20), "{:?}", phase(&s));
    // And its stream is given back at the end rather than leaked.
    s.disconnect();
    assert!(wait_for(3000, || phase(&s) == Phase::Idle));
    assert!(fake.seen().iter().any(|x| x.property == "StreamUndefine" && x.args.contains("-StreamId 215")), "{:?}", fake.seen_props());
}

#[test]
fn requests_made_while_switching_controllers_are_kept() {
    let one = FakeController::start(Behaviour::default()).unwrap();
    let two = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(opts());
    let k = key(4002, "ROB_1", 1);
    let k2 = key(4000, "ROB_1", 2);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&one));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    // Switch, change the channels and disconnect, all while the first controller's
    // teardown is still running: each one counts, in order.
    s.connect(target(&two));
    s.set_channels(vec![k2.clone()]);
    s.disconnect();
    assert!(wait_for(4000, || phase(&s) == Phase::Idle && two.open_connections() == 0), "{:?}", phase(&s));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(phase(&s), Phase::Idle, "a disconnect made during the switch was lost");
    let chans: Vec<ChannelKey> = s.status().channels.iter().map(|c| c.key.clone()).collect();
    assert_eq!(chans, vec![k2.clone()], "a channel change made during the switch was lost");
    assert!(two.streams().is_empty());

    // Closing the program during a switch does not open the new connection.
    s.connect(target(&one));
    assert!(wait_for(5000, || phase(&s) == Phase::Streaming));
    let three = FakeController::start(Behaviour::default()).unwrap();
    s.connect(target(&three));
    drop(s);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(three.connections_total(), 0, "shutting down during a switch still connected to the new controller");
}

#[test]
fn samples_going_to_a_program_that_was_there_first_are_explained() {
    // Another program subscribed first: every sample goes to it, this one gets none.
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let mut b = OtherTool::connect(&fake, Duration::from_millis(20));
    b.subscribe();
    b.define(0, 4002, 1);
    b.send(Command::StartStream);
    let s = spawn(opts());
    let k = key(6000, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || phase(&s) == Phase::Streaming));
    assert!(wait_for(3000, || log_text(&s).contains("InfoStream sends them all to one program")), "{}", log_text(&s));
    assert_eq!(samples(&s, &k), 0);
    assert_eq!(phase(&s), Phase::Streaming, "a signal can be silent: this is a hint, not a stop");
    assert_eq!(log_text(&s).matches("sends them all").count(), 1, "said once");
    // Shown where the person looks, not only in the log, and gone once it no longer
    // applies.
    // Over loopback, as on a VC: where the samples most likely go, and what ends it.
    assert!(
        wait_for(1000, || s.status().advice.as_deref().is_some_and(|a| a.contains("RobotStudio's") && a.contains("Disconnect and Connect again"))),
        "{:?}",
        s.status().advice
    );
    s.disconnect();
    assert!(wait_for(3000, || phase(&s) == Phase::Idle));
    assert!(wait_for(1000, || s.status().advice.is_none()));
}

#[test]
fn a_different_controller_starts_the_history_afresh() {
    let one = FakeController::start(Behaviour::default()).unwrap();
    let two = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(opts());
    let k = key(4002, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&one));
    assert!(wait_for(5000, || samples(&s, &k) > 100));
    // The same controller again keeps the history...
    s.connect(target(&one));
    assert!(wait_for(5000, || phase(&s) == Phase::Streaming && samples(&s, &k) > 150));
    assert!(history(&s, &k).len() > 150);
    // ...another one starts it afresh: one controller's data does not run on into
    // another's under the same channel.
    s.connect(target(&two));
    assert!(wait_for(5000, || two.samples_sent() > 20 && phase(&s) == Phase::Streaming && samples(&s, &k) > 20), "{:?}", phase(&s));
    let n = samples(&s, &k);
    assert!(n < 150, "the sample count carried over: {n}");
    assert!(history(&s, &k).len() as u64 <= n + 5, "{} points of history for {n} samples", history(&s, &k).len());
    assert!(s.status().counters.frames < 1000);
}

#[test]
fn an_answer_only_counts_for_the_clients_it_was_given_for() {
    let mut beh = Behaviour::default();
    beh.extra_clients = vec!["192.0.2.27".into()];
    let fake = FakeController::start(beh).unwrap();
    let mut o = opts();
    o.ask = AskPolicy::Always;
    let s = spawn(o);
    let k = key(4002, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(4000, || phase(&s) == Phase::AwaitingApproval));
    // A yes given while a different list was on screen (the connection dropped and
    // came back with another client meanwhile) takes nothing.
    let stale = vec![spy_core::session::OtherClient { address: "10.0.0.9".into(), attributes: vec![], same_pc: false, pendant: false }];
    s.answer(true, &stale);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(phase(&s), Phase::AwaitingApproval);
    assert!(fake.seen().is_empty());
    let shown = s.status().others.clone();
    s.answer(true, &shown);
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
}

#[test]
fn a_connection_lost_during_setup_is_not_shown_as_streaming() {
    let mut beh = Behaviour::default();
    beh.hold_define_of = Some(4000);
    let fake = FakeController::start(beh).unwrap();
    let s = spawn(opts());
    let a = key(4002, "ROB_1", 1);
    s.set_channels(vec![a.clone(), key(4000, "ROB_1", 1)]);
    s.connect(target(&fake));
    // 4002 answered, 4000's reply held back: setup waits on it...
    assert!(wait_for(3000, || matches!(state(&s, &a), Some(ChannelState::Defined { .. }))));
    // ...when the connection goes.
    fake.drop_connections();
    assert!(wait_for(3000, || matches!(phase(&s), Phase::Stopped { .. })), "{:?}", phase(&s));
    std::thread::sleep(Duration::from_millis(300));
    assert!(matches!(phase(&s), Phase::Stopped { .. }), "{:?}", phase(&s));
    assert!(!log_text(&s).contains("Streaming 0 of"), "{}", log_text(&s));
}

#[test]
fn a_slow_controller_does_not_make_this_programs_own_changes_look_like_a_takeover() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(opts());
    let a = key(4002, "ROB_1", 1);
    let c = key(4000, "ROB_1", 1);
    s.set_channels(vec![a.clone(), c.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, &[a.clone(), c.clone()])));
    // A controller that takes 1.3 s to act on each request: longer than the stall
    // timeout, and than the margin for this program's own changes. It keeps
    // delivering for 1.3 s after the undefine was sent, then pauses for 1.3 s.
    fake.with(|b| b.reply_delay = Duration::from_millis(1300));
    s.set_channels(vec![c.clone()]);
    std::thread::sleep(Duration::from_millis(4000));
    assert_eq!(phase(&s), Phase::Streaming, "{}", log_text(&s));
    let n = samples(&s, &c);
    assert!(wait_for(2000, || samples(&s, &c) > n + 20), "{}", log_text(&s));
}

#[test]
fn a_takeover_of_a_channel_that_has_not_delivered_yet_is_caught() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let a = spawn(opts());
    // Accepted, never sends: nothing to compare a stranger's samples with.
    let k = key(9999, "ROB_1", 1);
    a.set_channels(vec![k.clone()]);
    a.connect(target(&fake));
    assert!(wait_for(5000, || state(&a, &k) == Some(ChannelState::Defined { stream: 215 })));
    std::thread::sleep(Duration::from_millis(2500));
    let mut b = OtherTool::connect(&fake, Duration::from_millis(20));
    b.send(Command::UndefineAll);
    b.define(0, 4002, 1);
    assert_eq!(fake.streams().first().map(|s| (s.0, s.1)), Some((215, 4002)));
    b.send(Command::StartStream);
    assert!(wait_for(3000, || matches!(phase(&a), Phase::Stopped { .. })), "{:?}\n{}", phase(&a), log_text(&a));
    assert!(stopped_reason(&a).contains("taken InfoStream"), "{}", stopped_reason(&a));
    assert!(history(&a, &k).is_empty(), "another tool's signal was filed under a silent channel: {:?}", &history(&a, &k)[..3.min(history(&a, &k).len())]);
}

#[test]
fn a_stream_id_this_program_still_holds_being_handed_out_is_a_takeover() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let a = spawn(opts());
    let x = key(4002, "ROB_1", 1);
    a.set_channels(vec![x.clone()]);
    a.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&a, std::slice::from_ref(&x))));
    // Another client undefines this program's stream by its id, so the controller
    // hands that id to this program's next define.
    let mut b = OtherTool::connect(&fake, Duration::from_millis(20));
    b.send(Command::Undefine(215));
    a.set_channels(vec![x.clone(), key(4000, "ROB_1", 1)]);
    assert!(wait_for(3000, || matches!(phase(&a), Phase::Stopped { .. })), "{:?}\n{}", phase(&a), log_text(&a));
    assert!(stopped_reason(&a).contains("taken InfoStream"), "{}", stopped_reason(&a));
    assert!(history(&a, &x).iter().all(|&v| v == 101.0), "4002's channel shows another signal");
}

#[test]
fn a_takeover_while_this_programs_own_define_waits_is_caught() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let a = spawn(opts());
    let x = key(6000, "ROB_1", 1);
    a.set_channels(vec![x.clone()]);
    a.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&a, std::slice::from_ref(&x))));
    std::thread::sleep(Duration::from_millis(1500));
    // This program's define pauses delivery, and its reply is slow to come...
    fake.with(|b| b.hold_define_of = Some(4000));
    a.set_channels(vec![x.clone(), key(4000, "ROB_1", 1)]);
    assert!(wait_for(1000, || fake.seen_props().iter().filter(|p| *p == "StreamDefine").count() == 2));
    // ...and meanwhile another client takes over, and its StartStream ends the pause.
    let mut b = OtherTool::connect(&fake, Duration::from_millis(20));
    b.send(Command::UndefineAll);
    b.define(0, 4002, 1);
    b.send(Command::StartStream);
    assert!(wait_for(3000, || matches!(phase(&a), Phase::Stopped { .. })), "{:?}\n{}", phase(&a), log_text(&a));
    assert!(stopped_reason(&a).contains("taken InfoStream"), "{}", stopped_reason(&a));
    assert!(history(&a, &x).iter().all(|v| v.abs() <= 1.0), "a joint angle shows another tool's torque");
}

#[test]
fn takeover_detection_carries_on_across_a_32_bit_clock_wrap() {
    let mut beh = Behaviour::default();
    beh.wrap32 = true;
    let fake = FakeController::start(beh).unwrap();
    fake.set_clock_ms((1u64 << 32) - 2_500);
    let a = spawn(opts());
    let x = key(6000, "ROB_1", 1);
    let y = key(6001, "ROB_1", 1);
    a.set_channels(vec![x.clone()]);
    a.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&a, std::slice::from_ref(&x))));
    // A change of this program's own shortly before the wrap...
    a.set_channels(vec![x.clone(), y.clone()]);
    assert!(wait_for(3000, || samples(&a, &y) > 10));
    // ...then the clock wraps, and the session carries on through it...
    assert!(wait_for(6000, || fake.clock_ms() > (1u64 << 32) + 1_500));
    assert_eq!(phase(&a), Phase::Streaming, "{}", log_text(&a));
    // ...and a takeover after it is still caught.
    let mut b = OtherTool::connect(&fake, Duration::from_millis(20));
    b.send(Command::UndefineAll);
    b.define(0, 4002, 1);
    b.define(1, 4000, 3);
    b.send(Command::StartStream);
    assert!(wait_for(3000, || matches!(phase(&a), Phase::Stopped { .. })), "{:?}\n{}", phase(&a), log_text(&a));
    for k in [&x, &y] {
        assert!(history(&a, k).iter().all(|v| v.abs() <= 1.0), "{k} shows another tool's signal");
    }
}

#[test]
fn a_reconnect_waits_out_the_connection_the_controller_still_holds() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(opts());
    let k = key(4002, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    // A network fault: this end sees the connection drop at once; the controller
    // keeps it (listed, and as the one it sends every sample to) until its keepalive
    // times out, and a connection subscribed meanwhile never gets a sample
    // (measured 2026-09-25; the listing, 2026-09-27).
    fake.break_connections(Duration::from_millis(2500));
    let n = samples(&s, &k);
    assert!(wait_for(12000, || phase(&s) == Phase::Streaming && samples(&s, &k) > n + 50), "{:?}\n{}", phase(&s), log_text(&s));
    assert!(log_text(&s).contains("still holds the connection that broke"), "{}", log_text(&s));
    // While it held on, every look only handshook: one connection after it let go
    // subscribed and defined, once. (Defining on a connection the controller would
    // not serve, then leaving it, ends InfoStream for whoever else has it.)
    let seen = fake.seen();
    let subscribers: Vec<usize> = seen.iter().filter(|x| x.verb == "SUBSCRIBE").map(|x| x.conn).collect();
    assert_eq!(subscribers.len(), 2, "subscribed on {subscribers:?}");
    assert_eq!(seen.iter().filter(|x| x.property == "StreamDefine").count(), 2);
    assert!(fake.connections_total() >= 4, "some looks, then a fresh connection");
    assert_eq!(last(&s, &k), Some(101.0));
}

#[test]
fn a_held_connection_that_never_goes_ends_in_a_clear_stop_with_nothing_set_up() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(opts());
    let k = key(4002, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    fake.break_connections(Duration::from_secs(60));
    assert!(wait_for(12000, || matches!(phase(&s), Phase::Stopped { .. })), "{:?}\n{}", phase(&s), log_text(&s));
    assert!(stopped_reason(&s).contains("one more connection from this PC"), "{}", stopped_reason(&s));
    assert_eq!(props_from_others(&fake, &[1], "StreamDefine"), 0, "nothing set up after the break");
    assert_eq!(fake.seen().iter().filter(|x| x.verb == "SUBSCRIBE").count(), 1);
}

#[test]
fn a_program_that_came_while_the_connection_was_down_is_never_taken_from_unasked() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(Options { ask: AskPolicy::Remote, ..opts() });
    let k = key(4002, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    // The controller restarts (every connection drops), and another PC's program is
    // connected when this one comes back: it may be the tenant by now.
    fake.with(|b| b.extra_clients = vec!["192.0.2.50".into()]);
    fake.drop_connections();
    assert!(wait_for(5000, || matches!(phase(&s), Phase::Stopped { .. })), "{:?}\n{}", phase(&s), log_text(&s));
    let reason = stopped_reason(&s);
    assert!(reason.contains("192.0.2.50") && reason.contains("While the connection was down"), "{reason}");
    assert_eq!(props_from_others(&fake, &[1], "StreamDefine"), 0, "nothing set up on the reconnect");
    // Where asking is possible, it asks instead.
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(Options { ask: AskPolicy::Always, ..opts() });
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    fake.with(|b| b.extra_clients = vec!["192.0.2.50".into()]);
    fake.drop_connections();
    assert!(wait_for(5000, || phase(&s) == Phase::AwaitingApproval), "{:?}\n{}", phase(&s), log_text(&s));
    assert!(s.status().others.iter().any(|o| o.address == "192.0.2.50"));
    assert_eq!(props_from_others(&fake, &[1], "StreamDefine"), 0);
}

#[test]
fn a_program_on_this_pc_that_took_infostream_meanwhile_is_left_alone() {
    // The case the client list cannot tell from this program's own broken
    // connection: another program on this PC. Waited out as if it were that, then
    // left alone, with nothing set up.
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(Options { ladder: vec![Duration::from_millis(600)], ..opts() });
    let k = key(6000, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    fake.drop_connections();
    let mut other = OtherTool::connect(&fake, Duration::from_millis(20));
    other.send(Command::StreamConnect);
    other.subscribe();
    other.define(0, 4002, 1);
    other.send(Command::StartStream);
    assert!(wait_for(10000, || matches!(phase(&s), Phase::Stopped { .. })), "{:?}\n{}", phase(&s), log_text(&s));
    assert!(stopped_reason(&s).contains("one more connection from this PC"), "{}", stopped_reason(&s));
    let others_conn = fake.seen().iter().find(|x| x.property == "StreamDefine" && x.args.contains("-Signal 4002")).map(|x| x.conn).unwrap();
    assert_eq!(props_from_others(&fake, &[1, others_conn], "StreamDefine"), 0, "this program set nothing up again");
    assert!(fake.streams().iter().any(|st| st.1 == 4002), "the other program's stream survives: {:?}", fake.streams());
    assert!(fake.streaming());
    drop(other);
}

#[test]
fn a_different_controller_at_the_address_is_not_streamed_from_unasked() {
    // Every IRC5's service port is 192.168.125.1: a cable moved to the next robot
    // reconnects to a different controller at the same address.
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(opts());
    let k = key(4002, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    let held = history(&s, &k).len();
    let first_last_t = s.store().get(&k).unwrap().lock().last().unwrap().0;
    fake.with(|b| b.system_id = "{0000000B-0000-4000-8000-00000000000B}".into());
    fake.drop_connections();
    assert!(wait_for(5000, || matches!(phase(&s), Phase::Stopped { .. })), "{:?}\n{}", phase(&s), log_text(&s));
    let reason = stopped_reason(&s);
    assert!(reason.contains("different controller") && reason.contains("0000000B"), "{reason}");
    assert_eq!(props_from_others(&fake, &[1], "StreamDefine"), 0);
    assert!(history(&s, &k).len() <= held + 5, "nothing of the other controller's filed");
    // Connected to deliberately, it is used, with the charts started afresh.
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))), "{}", log_text(&s));
    assert!(log_text(&s).contains("the charts start afresh"), "{}", log_text(&s));
    let first_t = s.store().get(&k).unwrap().lock().first_t().unwrap();
    assert!(first_t > first_last_t, "the first controller's history is still there ({first_t} <= {first_last_t})");
}

#[test]
fn the_first_program_to_connect_infostream_gets_the_samples_subscribed_or_not() {
    // Measured on the RW6 VC (2026-09-28) and the IRC5 (2026-09-29): a connection that
    // only sent StreamConnect, before this one, gets every sample; this one none.
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let mut other = OtherTool::connect(&fake, Duration::from_millis(20));
    other.send(Command::StreamConnect);
    let s = spawn(opts());
    let k = key(6000, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || matches!(state(&s, &k), Some(ChannelState::Defined { .. })) && phase(&s) == Phase::Streaming));
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(samples(&s, &k), 0, "the program that connected InfoStream first is the tenant");
    drop(other);
}

#[test]
fn a_program_leaving_that_ends_infostream_is_named_and_connected_again_once() {
    // The leaver is connected before this program (so listed in its handshake) but
    // sets nothing up until later: one that set up first would be the tenant
    // (measured 2026-09-28). It then connects InfoStream, defines a stream and leaves,
    // as a tool that exits or crashes after its setup: its define pauses this program's
    // feed, its exit ends it (measured 2026-09-26), and only a new connection is
    // served. The same sequence was verified on the VC (2026-09-28) and the IRC5
    // (2026-09-29).
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let mut other = OtherTool::connect(&fake, Duration::from_millis(20));
    let a = spawn(opts());
    let k = key(6000, "ROB_1", 1);
    a.set_channels(vec![k.clone()]);
    a.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&a, std::slice::from_ref(&k))), "{}", log_text(&a));
    other.send(Command::StreamConnect);
    other.define(0, 4002, 1);
    drop(other);
    assert!(wait_for(5000, || fake.connections_total() == 3 && phase(&a) == Phase::Streaming && samples(&a, &k) > 0 && a.status().counters.reconnects == 1), "{:?}\n{}", phase(&a), log_text(&a));
    let n = samples(&a, &k);
    assert!(wait_for(3000, || samples(&a, &k) > n + 50), "{}", log_text(&a));
    let log = log_text(&a);
    assert!(log.contains("disconnected from the controller") && log.contains("connecting again, once"), "{log}");
    assert_eq!(log.matches("connecting again, once").count(), 1, "the same long reason logged twice: {log}");
}

#[test]
fn after_a_program_leaves_a_reconnect_that_gets_nothing_stops_once() {
    let mut beh = Behaviour::default();
    beh.extra_clients = vec!["192.0.2.9".into()];
    let fake = FakeController::start(beh).unwrap();
    let a = spawn(opts());
    let k = key(6000, "ROB_1", 1);
    a.set_channels(vec![k.clone()]);
    a.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&a, std::slice::from_ref(&k))));
    fake.with(|b| {
        b.extra_clients.clear();
        b.mute_all = true;
    });
    assert!(wait_for(6000, || matches!(phase(&a), Phase::Stopped { .. })), "{:?}\n{}", phase(&a), log_text(&a));
    assert!(log_text(&a).contains("192.0.2.9 disconnected from the controller"), "{}", log_text(&a));
    assert!(stopped_reason(&a).contains("Connected again once"), "{}", stopped_reason(&a));
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(fake.connections_total(), 2, "once, and no more");
}

#[test]
fn disconnect_while_leaving_for_an_automatic_reconnect_stays_disconnected() {
    let mut beh = Behaviour::default();
    beh.extra_clients = vec!["192.0.2.9".into()];
    let fake = FakeController::start(beh).unwrap();
    let a = spawn(opts());
    let k = key(6000, "ROB_1", 1);
    a.set_channels(vec![k.clone()]);
    a.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&a, std::slice::from_ref(&k))));
    // The leaver goes; this program's quiet exit before reconnecting takes a while.
    fake.with(|b| {
        b.extra_clients.clear();
        b.mute_all = true;
        b.reply_delay = Duration::from_millis(250);
    });
    assert!(wait_for(4000, || phase(&a) == Phase::TearingDown), "{:?}\n{}", phase(&a), log_text(&a));
    a.disconnect();
    assert!(wait_for(3000, || phase(&a) == Phase::Idle), "{:?}", phase(&a));
    std::thread::sleep(Duration::from_millis(1000));
    assert_eq!(phase(&a), Phase::Idle, "{}", log_text(&a));
    assert_eq!(fake.connections_total(), 1, "it connected again after the person pressed Disconnect");
}

#[test]
fn removing_the_last_channel_restarts_the_feed_of_the_program_that_gets_the_samples() {
    // Another program subscribed first and gets every sample. This program's
    // undefine pauses every stream on the controller until a StartStream; with no
    // stream of its own left it still owes one.
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let mut other = OtherTool::connect(&fake, Duration::from_millis(20));
    other.send(Command::StreamConnect);
    other.subscribe();
    other.define(0, 4002, 1);
    other.send(Command::StartStream);
    let s = spawn(opts());
    let k = key(6000, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || matches!(state(&s, &k), Some(ChannelState::Defined { .. })) && phase(&s) == Phase::Streaming));
    assert!(wait_for(2000, || fake.streaming()));
    s.set_channels(vec![]);
    assert!(wait_for(2000, || fake.seen_props().iter().any(|p| p == "StreamUndefine")));
    assert!(wait_for(1500, || fake.streaming()), "the other program's feed was left paused\n{}", log_text(&s));
    drop(other);
}

#[test]
fn a_session_that_never_got_a_sample_stops_nobody_elses_feed_on_its_way_out() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let mut other = OtherTool::connect(&fake, Duration::from_millis(20));
    other.send(Command::StreamConnect);
    other.subscribe();
    other.define(0, 4002, 1);
    other.send(Command::StartStream);
    let s = spawn(opts());
    let k = key(6000, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || matches!(state(&s, &k), Some(ChannelState::Defined { .. })) && phase(&s) == Phase::Streaming));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(samples(&s, &k), 0, "the other program is the tenant");
    let before = fake.seen().len();
    s.disconnect();
    assert!(wait_for(3000, || phase(&s) == Phase::Idle));
    let tail: Vec<String> = fake.seen()[before..].iter().filter(|x| x.conn == 2).map(|x| x.property.clone()).collect();
    assert_eq!(tail, vec!["StreamUndefine", "StartStream", "StreamDisconnect"], "no controller-wide StopStream from a program that was not the tenant");
    // With nothing of its own defined, only the disconnect.
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(opts());
    let refused = key(1, "ROB_1", 1);
    s.set_channels(vec![refused.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || matches!(state(&s, &refused), Some(ChannelState::Refused { .. }))));
    let before = fake.seen().len();
    s.disconnect();
    assert!(wait_for(3000, || phase(&s) == Phase::Idle));
    let tail: Vec<String> = fake.seen()[before..].iter().map(|x| x.property.clone()).collect();
    assert_eq!(tail, vec!["StreamDisconnect"]);
    drop(other);
}

#[test]
fn a_crash_before_anything_is_set_up_sends_nothing() {
    let mut beh = Behaviour::default();
    beh.extra_clients = vec!["192.0.2.27".into()];
    let fake = FakeController::start(beh).unwrap();
    let s = spawn(Options { ask: AskPolicy::Always, ..opts() });
    s.set_channels(vec![key(6000, "ROB_1", 1)]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || phase(&s) == Phase::AwaitingApproval));
    s.crash_for_test();
    assert!(wait_for(3000, || fake.open_connections() == 0), "the connection was not closed");
    assert!(fake.seen().is_empty(), "sent {:?}", fake.seen_props());
}

#[test]
fn nothing_is_filed_while_disconnecting() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(opts());
    let k = key(6000, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    // A slow controller: samples keep coming until it acts on the StopStream.
    fake.with(|b| b.reply_delay = Duration::from_millis(300));
    s.disconnect();
    assert!(wait_for(2000, || phase(&s) == Phase::TearingDown));
    let n = history(&s, &k).len();
    assert!(wait_for(4000, || phase(&s) == Phase::Idle));
    assert_eq!(history(&s, &k).len(), n, "samples were filed after the disconnect began");
}

#[test]
fn a_held_up_worker_does_not_read_its_own_backlog_as_silence() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(opts());
    let k = key(6000, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    // The window holds a channel's history for 2 s (a slow draw, a paging laptop):
    // the worker waits, and the frames queue behind it.
    {
        let ch = s.store().get(&k).unwrap();
        let _held = ch.lock();
        std::thread::sleep(Duration::from_millis(2000));
    }
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(phase(&s), Phase::Streaming, "{}", log_text(&s));
}

#[test]
fn a_worker_held_up_past_the_stall_time_does_not_stop() {
    // Longer than the stall: the backlog's oldest frames alone would read as a
    // silence past it, straight to the stall with no liveness check involved.
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(opts());
    let k = key(6000, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    {
        let ch = s.store().get(&k).unwrap();
        let _held = ch.lock();
        std::thread::sleep(Duration::from_millis(3500));
    }
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(phase(&s), Phase::Streaming, "{}", log_text(&s));
}

#[test]
fn a_start_stream_that_is_never_answered_does_not_switch_off_the_stall() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(opts());
    let x = key(6000, "ROB_1", 1);
    let y = key(6001, "ROB_1", 1);
    s.set_channels(vec![x.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&x))));
    // A controller that loses the reply to a StartStream (it acts on it, though).
    fake.with(|b| {
        b.unanswered.insert("StartStream".into());
    });
    s.set_channels(vec![x.clone(), y.clone()]);
    assert!(wait_for(3000, || samples(&s, &y) > 10), "{}", log_text(&s));
    // Then the feed dies: that must still be noticed. The controller's answers now
    // take a moment, as a real one's can.
    fake.with(|b| {
        b.mute_all = true;
        b.reply_delay = Duration::from_millis(250);
    });
    assert!(wait_for(6000, || matches!(phase(&s), Phase::Stopped { .. })), "a dead feed went unnoticed\n{}", log_text(&s));
    // For what it is: the controller still answers (its liveness check, held back
    // while that StartStream was in play, went out before any verdict and had its
    // time), so it is neither the network nor a reason to connect again.
    assert!(stopped_reason(&s).contains("still answers"), "{}", log_text(&s));
    assert!(!log_text(&s).contains("not even an answer"), "{}", log_text(&s));
}

#[test]
fn an_unanswered_liveness_check_does_not_block_the_next() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(Options { stall_after: Duration::from_secs(3), ..opts() });
    let k = key(6000, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    // A controller that does not answer the handshake mid-session, and a stall that
    // clears.
    fake.with(|b| {
        b.mute_handshake = true;
        b.hold_delivery = true;
    });
    assert!(wait_for(2000, || s.status().counters.liveness_checks == 1));
    fake.with(|b| b.hold_delivery = false);
    // Past the check's own timeout, a second silence gets a second check.
    std::thread::sleep(Duration::from_millis(1800));
    fake.with(|b| b.hold_delivery = true);
    assert!(wait_for(2000, || s.status().counters.liveness_checks == 2), "{}", log_text(&s));
    fake.with(|b| b.hold_delivery = false);
    assert!(log_text(&s).contains("did not answer the liveness check"), "{}", log_text(&s));
}

#[test]
fn the_irc5s_tick_is_neither_a_gap_nor_a_takeover() {
    // Stamp steps of 4 ms with a 5 every 31.25 samples, and 24 or 25 for the 24 ms
    // signals (measured on the IRC5, 2026-09-26): all normal, and all inside the gap
    // and takeover bounds.
    let mut b = Behaviour::default();
    b.irc5_clock = true;
    b.id_pools = IdPools::Irc5;
    let fake = FakeController::start(b).unwrap();
    let s = spawn(opts());
    let fast = key(6000, "ROB_1", 1);
    let slow = key(9888, "ROB_1", 1);
    s.set_channels(vec![fast.clone(), slow.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, &[fast.clone(), slow.clone()])));
    std::thread::sleep(Duration::from_millis(2000));
    assert_eq!(phase(&s), Phase::Streaming, "{}", log_text(&s));
    let steps = |k: &ChannelKey| -> std::collections::BTreeSet<i64> {
        let ch = s.store().get(k).unwrap();
        let r = ch.lock();
        let t: Vec<i64> = r.range(i64::MIN, i64::MAX).map(|(t, _)| t).collect();
        t.windows(2).map(|w| w[1] - w[0]).collect()
    };
    assert_eq!(steps(&fast), [4, 5].into(), "the fake's IRC5 clock");
    assert_eq!(steps(&slow), [24, 25].into());
    let st = s.status();
    assert!(st.channels.iter().all(|c| c.gaps == 0), "{:?}", st.channels.iter().map(|c| c.gaps).collect::<Vec<_>>());
}

#[test]
fn a_controller_that_falls_silent_altogether_is_connected_to_again() {
    // Nothing at all arrives, the liveness check's answer included: the network or
    // the controller stalled (a program taking InfoStream would still answer). The
    // session connects again by itself rather than stop; here the network only
    // held everything up, and delivers it late.
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(opts());
    let k = key(6000, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    fake.with(|b| b.hold_delivery = true);
    assert!(wait_for(4000, || matches!(phase(&s), Phase::Reconnecting { .. })), "{:?}\n{}", phase(&s), log_text(&s));
    assert!(log_text(&s).contains("not even an answer to the liveness check"), "{}", log_text(&s));
    assert_eq!(s.status().counters.liveness_checks, 1);
    // Still trying, not stopped, while nothing gets through.
    std::thread::sleep(Duration::from_millis(2000));
    assert!(!matches!(phase(&s), Phase::Stopped { .. }), "{}", log_text(&s));
    fake.with(|b| b.hold_delivery = false);
    let n = samples(&s, &k);
    assert!(wait_for(8000, || phase(&s) == Phase::Streaming && samples(&s, &k) > n + 50), "{:?}\n{}", phase(&s), log_text(&s));
}

#[test]
fn a_network_fault_is_connected_through_by_itself_once_the_controller_lets_go() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(opts());
    let k = key(4002, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    // A fault out in the network (a cable pulled between switches, the controller's
    // switch port shut): silence both ways and no error on either end, so this end
    // hears of it only by its liveness check going unanswered. The network is back
    // after 2.5 s; the controller holds the broken connection until 4 s.
    fake.cut_network(Duration::from_millis(2500), Duration::from_millis(4000));
    let n = samples(&s, &k);
    assert!(wait_for(15000, || phase(&s) == Phase::Streaming && samples(&s, &k) > n + 50), "{:?}\n{}", phase(&s), log_text(&s));
    let log = log_text(&s);
    assert!(log.contains("not even an answer to the liveness check"), "{log}");
    assert!(log.contains("still holds the connection that broke"), "{log}");
    // Nothing was set up while the controller held the broken connection: one
    // connection after it let go subscribed and defined, once. (Defining on a
    // connection the controller would not serve, then leaving it, ends InfoStream for
    // whoever else has it.)
    let seen = fake.seen();
    assert_eq!(seen.iter().filter(|x| x.verb == "SUBSCRIBE").count(), 2, "{seen:?}");
    assert_eq!(seen.iter().filter(|x| x.property == "StreamDefine").count(), 2);
    assert_eq!(last(&s, &k), Some(101.0));
}

#[test]
fn a_long_outage_still_waits_out_the_held_connection_once_the_network_is_back() {
    // The wait for the controller to let go of the broken connection counts from when
    // it can be reached again, not from the break: a controller lets go when it next
    // hears from this PC, which it cannot while the network is down (a switch
    // rebooting takes a minute). Here the outage (4 s) is longer than that wait (4 s
    // in these tests), and the controller holds on 2 s after it.
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(opts());
    let k = key(4002, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    fake.cut_network(Duration::from_millis(4000), Duration::from_millis(6000));
    let n = samples(&s, &k);
    assert!(wait_for(15000, || phase(&s) == Phase::Streaming && samples(&s, &k) > n + 50), "{:?}\n{}", phase(&s), log_text(&s));
    assert!(log_text(&s).contains("still holds the connection that broke"), "{}", log_text(&s));
    assert_eq!(fake.seen().iter().filter(|x| x.verb == "SUBSCRIBE").count(), 2, "nothing set up while it held on");
}

#[test]
fn leaving_a_controller_cut_off_by_the_network_does_not_wait_on_it() {
    // Neither this end's goodbye nor the controller's close gets through, and on
    // Windows shutting the socket down does not end a read blocked on it (measured
    // 2026-09-29): the worker must not hang on its reader until TCP gives up.
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(opts());
    let k = key(6000, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    fake.cut_network(Duration::from_secs(30), Duration::from_secs(30));
    // The stall (1 s), then the goodbye's 0.5 s, then the ladder.
    assert!(wait_for(4000, || matches!(phase(&s), Phase::Reconnecting { .. })), "{:?}\n{}", phase(&s), log_text(&s));
}

#[test]
fn disconnecting_while_the_network_is_down_is_prompt() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(opts());
    let k = key(6000, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    fake.cut_network(Duration::from_secs(30), Duration::from_secs(30));
    s.disconnect();
    // The teardown's replies never come (1 s), then the socket is let go.
    assert!(wait_for(4000, || phase(&s) == Phase::Idle), "{:?}\n{}", phase(&s), log_text(&s));
}

#[test]
fn a_program_that_came_during_a_network_fault_is_never_taken_from_unasked() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(opts());
    let k = key(4002, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    // The controller lets go of the broken connection during the outage, and another
    // PC's program is connected when the network is back: it may be the tenant now.
    fake.cut_network(Duration::from_millis(2000), Duration::from_millis(1000));
    fake.with(|b| b.extra_clients = vec!["192.0.2.50".into()]);
    assert!(wait_for(10000, || matches!(phase(&s), Phase::Stopped { .. })), "{:?}\n{}", phase(&s), log_text(&s));
    let reason = stopped_reason(&s);
    assert!(reason.contains("192.0.2.50") && reason.contains("While the connection was down"), "{reason}");
    assert_eq!(fake.seen().iter().filter(|x| x.verb == "SUBSCRIBE").count(), 1, "nothing set up on the reconnect");
}

fn patient() -> Options {
    Options { vc_pause_patience: Duration::from_secs(10), ..opts() }
}

#[test]
fn a_virtual_controller_that_pauses_is_waited_for() {
    // Measured (2026-09-27): a VC the PC is too busy for stops InfoStream and its
    // clock for as long as the load lasts, answers meanwhile, then carries on.
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(patient());
    let k = key(6000, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    fake.with(|b| b.freeze = true);
    assert!(wait_for(3000, || s.status().advice.as_deref().is_some_and(|a| a.contains("paused"))), "{}", log_text(&s));
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(phase(&s), Phase::Streaming, "past the stall, still waiting\n{}", log_text(&s));
    assert!(s.status().channels[0].stale, "shown as stale meanwhile");
    let checks = s.status().counters.liveness_checks;
    assert!((2..=10).contains(&checks), "each check, spaced by the 0.5 s bound, asks again who is connected: {checks} in about 3 s");
    fake.with(|b| b.freeze = false);
    let n = samples(&s, &k);
    assert!(wait_for(3000, || samples(&s, &k) > n + 50), "{}", log_text(&s));
    assert_eq!(phase(&s), Phase::Streaming, "{}", log_text(&s));
    assert!(log_text(&s).contains("sending again"), "{}", log_text(&s));
    assert_eq!(s.status().advice, None);
}

#[test]
fn a_pause_is_timed_from_the_last_sample() {
    // Seen on the RW6 VC (2026-09-28): a pause of about 2 s, whose liveness check was
    // answered only as the samples came back, was logged as "after 0.0 s".
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(Options { stall_after: Duration::from_secs(5), ..patient() });
    let k = key(6000, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    fake.with(|b| {
        b.freeze = true;
        b.hold_delivery = true;
    });
    std::thread::sleep(Duration::from_millis(1500));
    fake.with(|b| {
        b.hold_delivery = false;
        b.freeze = false;
    });
    assert!(wait_for(3000, || log_text(&s).contains("sending again")), "{}", log_text(&s));
    let log = log_text(&s);
    let line = log.lines().find(|l| l.contains("sending again")).unwrap();
    let secs: f64 = line.split("after ").nth(1).and_then(|r| r.split(' ').next()).and_then(|n| n.parse().ok()).unwrap_or_else(|| panic!("{line}"));
    assert!((1.3..3.0).contains(&secs), "the pause was about 1.5 s: {line}\n{log}");
}

#[test]
fn a_program_that_connects_during_a_pause_is_left_at_once() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(patient());
    let k = key(6000, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    fake.with(|b| b.freeze = true);
    assert!(wait_for(3000, || s.status().advice.as_deref().is_some_and(|a| a.contains("paused"))));
    let other = OtherTool::connect(&fake, Duration::from_millis(20));
    assert!(wait_for(3000, || matches!(phase(&s), Phase::Stopped { .. })), "{:?}\n{}", phase(&s), log_text(&s));
    assert!(stopped_reason(&s).contains("connected to the controller after this program did"), "{}", stopped_reason(&s));
    drop(other);
}

#[test]
fn a_takeover_by_a_program_already_connected_shows_when_its_samples_come() {
    // The case waiting cannot tell from a pause: a program that was connected all
    // along (RobotStudio, say) clears every stream and sets up its own. Its samples
    // then come here under this program's ids, with the clock run on: caught there,
    // before any is filed.
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let mut other = OtherTool::connect(&fake, Duration::from_millis(20));
    let s = spawn(patient());
    let k1 = key(6000, "ROB_1", 1);
    let k2 = key(6001, "ROB_1", 1);
    s.set_channels(vec![k1.clone(), k2.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, &[k1.clone(), k2.clone()])));
    other.send(Command::StreamConnect);
    other.send(Command::UndefineAll);
    assert!(wait_for(3000, || s.status().advice.as_deref().is_some_and(|a| a.contains("paused"))), "{}", log_text(&s));
    other.define(0, 4002, 1);
    other.define(1, 4000, 3);
    other.send(Command::StartStream);
    assert!(wait_for(3000, || matches!(phase(&s), Phase::Stopped { .. })), "{:?}\n{}", phase(&s), log_text(&s));
    for k in [&k1, &k2] {
        let h = history(&s, k);
        assert!(h.iter().all(|v| v.abs() <= 1.0), "{k} shows another program's signal");
    }
}

#[test]
fn a_virtual_controller_that_neither_sends_nor_answers_is_given_up_on() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(Options { vc_pause_patience: Duration::from_secs(3), ..opts() });
    let k = key(6000, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    let t0 = Instant::now();
    fake.with(|b| {
        b.freeze = true;
        b.mute_handshake = true;
    });
    assert!(wait_for(6000, || matches!(phase(&s), Phase::Stopped { .. })), "{:?}", phase(&s));
    let took = t0.elapsed();
    assert!(took >= Duration::from_millis(2900), "gave up after {took:?}, before its patience");
    let reason = stopped_reason(&s);
    assert!(reason.contains("virtual controller has sent no samples for 3 s") && reason.contains("not answered"), "{reason}");
}

#[test]
fn a_reconnect_that_never_delivered_climbs_the_ladder() {
    // Setting up is not working: the ladder starts over only once samples arrive.
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(opts());
    let k = key(6000, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    fake.with(|b| b.mute_all = true);
    fake.drop_connections();
    assert!(wait_for(2000, || matches!(phase(&s), Phase::Reconnecting { attempt: 1, .. })), "{:?}", phase(&s));
    // It connects again and sets up (nothing is delivered), and drops again.
    assert!(wait_for(3000, || fake.connections_total() == 2 && phase(&s) == Phase::Streaming && matches!(state(&s, &k), Some(ChannelState::Defined { .. }))), "{:?}", phase(&s));
    fake.drop_connections();
    assert!(wait_for(2000, || matches!(phase(&s), Phase::Reconnecting { .. })), "{:?}", phase(&s));
    assert!(matches!(phase(&s), Phase::Reconnecting { attempt: 2, retry_in } if retry_in == Duration::from_millis(200)), "{:?}", phase(&s));
}

#[test]
fn a_text_channel_is_not_reported_for_sending_nothing() {
    // 9875 sent nothing at StartStream on the cell: a text event is quiet until it
    // changes, and the window knows which signals are text from its catalogue.
    let mut beh = Behaviour::default();
    beh.signals.insert(9875, SignalDef { source: SignalSource::Silent, sample_ms: 4.032 });
    let fake = FakeController::start(beh).unwrap();
    let text = key(9875, "ROB_1", 1);
    let angle = key(6000, "ROB_1", 1);
    let s = spawn(opts());
    s.set_channels_expecting_text(vec![text.clone(), angle.clone()], [text.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&angle))));
    std::thread::sleep(Duration::from_millis(800));
    assert!(!log_text(&s).contains("9875 ROB_1 axis 1: defined, but no samples"), "{}", log_text(&s));
    // Not said to be text, the same silence is reported (the check discriminates).
    let s2 = spawn(opts());
    s.disconnect();
    assert!(wait_for(3000, || phase(&s) == Phase::Idle));
    s2.set_channels(vec![text.clone(), angle.clone()]);
    s2.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s2, std::slice::from_ref(&angle))));
    std::thread::sleep(Duration::from_millis(800));
    assert!(log_text(&s2).contains("9875 ROB_1 axis 1: defined, but no samples"), "{}", log_text(&s2));
}

#[test]
fn signal_sources_can_be_replaced() {
    // The fake's knobs themselves: a custom integer signal decodes as int.
    let mut b = Behaviour::default();
    b.signals.insert(3010, SignalDef { source: SignalSource::int(|_| 20), sample_ms: 4.032 });
    let fake = FakeController::start(b).unwrap();
    let s = spawn(opts());
    let k = key(3010, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    assert_eq!(last(&s, &k), Some(20.0));
}
