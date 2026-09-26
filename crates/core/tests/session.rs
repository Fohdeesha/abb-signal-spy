//! The real session worker against the in-process fake controller, which plays the
//! RW6 VC byte for byte (see `spy_core::fake`). Every scenario runs on its own fake,
//! on a port the OS picks, over loopback.

// Each scenario states its departures from the fake's defaults one line at a time.
#![allow(clippy::field_reassign_with_default)]

use std::io::Write;
use std::net::TcpListener;
use std::sync::Arc;
use std::time::{Duration, Instant};

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
        orphan_retries: 5,
    }
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

    // A host that drops the SYN (TEST-NET-1, RFC 5737): bounded by the connect
    // timeout, and shutdown must not hang behind it.
    let mut o = opts();
    o.connect_timeout = Duration::from_millis(500);
    let s = spawn(o);
    let t0 = Instant::now();
    s.connect(Target { host: "192.0.2.1".into(), port: 5515 });
    assert!(wait_for(4000, || matches!(phase(&s), Phase::Stopped { .. })), "{:?}", phase(&s));
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
    // Signal 4002's reply is held back and released just ahead of the next define's
    // reply: a controller slower than one define, made deterministic.
    b.hold_define_of = Some(4002);
    let fake = FakeController::start(b).unwrap();
    let s = spawn(opts());
    let a = key(4002, "ROB_1", 1);
    let c = key(4000, "ROB_1", 1);
    s.set_channels(vec![a.clone(), c.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, &[a.clone(), c.clone()])), "{}", log_text(&s));
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
    // samples length wraps (the bridge's M34 shape), on the mapped stream.
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
    // stream on the controller (measured 2026-09-26, tunemaster-testsignals.md s24
    // items 9-10). Only leaving before they are defined spares them.
    assert!(wait_for(1000, || fake.streams().is_empty()), "{:?}", fake.streams());
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(fake.connections_total(), 2, "it went back for InfoStream");
}

#[test]
fn a_takeover_is_left_before_the_newcomer_sets_up_its_signals() {
    // Measured 2026-09-26 (tunemaster-testsignals.md s24 items 9-10): this program's
    // exit, whatever it sends, clears every stream on the controller, the newcomer's
    // included. The bridge's test-signal client clears every stream, then waits
    // 500 ms before its first define: leaving inside that wait spares all of it.
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
fn a_program_leaving_that_ends_infostream_is_named() {
    // Measured on the VC (s24 item 11): a client that set up streams and leaves ends
    // InfoStream for every program still connected.
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
    assert!(wait_for(2000, || matches!(phase(&a), Phase::Stopped { .. })), "{:?}\n{}", phase(&a), log_text(&a));
    let reason = stopped_reason(&a);
    assert!(reason.contains("192.0.2.9") && reason.contains("disconnected"), "{reason}");
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
    // through the question (the operator's decision, 2026-09-26).
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
    // The real IRC5 numbers streams from three pools (s24 item 4): 259 down, its
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
    assert!(wait_for(1000, || s.status().advice.as_deref().is_some_and(|a| a.contains("connect again"))), "{:?}", s.status().advice);
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
fn a_reconnect_the_controller_does_not_serve_yet_is_retried_until_it_does() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(opts());
    let k = key(4002, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    // A network fault: this end sees the connection drop at once, the controller
    // keeps sending to it for a while yet, and a connection made meanwhile gets
    // nothing (measured). The reconnect is dropped and made again until one works.
    fake.break_connections(Duration::from_millis(2500));
    let n = samples(&s, &k);
    assert!(wait_for(12000, || phase(&s) == Phase::Streaming && samples(&s, &k) > n + 50), "{:?}\n{}", phase(&s), log_text(&s));
    assert!(log_text(&s).contains("still holding the connection that broke"), "{}", log_text(&s));
    assert!(fake.connections_total() >= 3);
    assert_eq!(last(&s, &k), Some(101.0));
}

#[test]
fn reconnects_that_never_get_samples_end_in_a_clear_stop() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let mut o = opts();
    o.orphan_retries = 2;
    let s = spawn(o);
    let k = key(4002, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    fake.break_connections(Duration::from_secs(60));
    assert!(wait_for(12000, || matches!(phase(&s), Phase::Stopped { .. })), "{:?}\n{}", phase(&s), log_text(&s));
    assert!(stopped_reason(&s).contains("Connected again 3 times"), "{}", stopped_reason(&s));
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
