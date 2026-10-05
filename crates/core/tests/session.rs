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
        probe_after: Duration::from_millis(500),
        teardown_wait: Duration::from_millis(1000),
        ladder: vec![Duration::from_millis(100), Duration::from_millis(200), Duration::from_millis(400)],
        ask: AskPolicy::Never,
        held_wait: Duration::from_secs(4),
        held_poll: Duration::from_millis(200),
        recovery_retry: Duration::from_millis(100),
        recovery_handshake: Duration::from_millis(300),
        recovery_for: Duration::from_secs(4),
        vc_pause_patience: Duration::ZERO,
        find_vc: VcFinder::none(),
    }
}

fn finder(ports: Arc<Mutex<Vec<u16>>>) -> VcFinder {
    VcFinder::new(move |timeout| ports.lock().unwrap().iter().filter_map(|&p| discovery::hello(SocketAddr::from(([127, 0, 0, 1], p)), timeout).ok().map(|a| (p, a.system_id))).collect())
}

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

    assert_eq!(last(&s, &keys[0]), Some(101.0));
    assert_eq!(last(&s, &keys[1]), Some(203.0));
    assert_eq!(last(&s, &keys[2]), Some(-1.0));
    let st = s.status();
    assert_eq!(st.channels[2].kind, Some(ValueKind::Int), "the int signal must be decoded as int");
    assert_eq!(st.channels[0].sample_ms, Some(4.032));
    assert_eq!(st.channels[2].sample_ms, Some(24.192));
    assert_eq!(st.subscription, Some(spy_core::fake::SUBSCRIPTION_ID));
    drop(st);
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
    let r = s.store().get(&k).unwrap();
    let r = r.lock();
    let segs = r.decimate(r.first_t().unwrap(), r.last().unwrap().0 + 1, 100_000, |v| v);
    assert!(segs.len() >= 2, "the reconnect gap must break the trace");
}

#[test]
fn unreachable_controllers_fail_fast_and_clearly() {
    let port = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let s = spawn(opts());
    s.connect(Target { host: "127.0.0.1".into(), port });
    assert!(wait_for(5000, || matches!(phase(&s), Phase::Stopped { .. })));
    let Phase::Stopped { reason } = phase(&s) else { unreachable!() };
    assert!(reason.contains("refused") || reason.contains("no answer"), "{reason}");
    assert!(reason.contains("new port at every start"), "{reason}");

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

    let s = spawn(opts());
    s.connect(Target { host: "no-such-controller.invalid".into(), port: 5515 });
    assert!(wait_for(8000, || matches!(phase(&s), Phase::Stopped { .. })));
}

#[test]
fn replies_are_matched_by_transaction() {
    let mut b = Behaviour::default();
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

    fake.send_aya();
    assert!(wait_for(2000, || !fake.aya_answers().is_empty()));
    let ay = fake.aya_answers()[0].clone();
    assert_eq!((ay.txn, ay.cause, ay.ctrl1, ay.ctrl2), (0, 5, 4000, 16000));
    assert!(phase(&s) == Phase::Streaming, "answering the AYA must not disturb the session");

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
    assert!(wait_for(4000, || state(&s, &a) == Some(ChannelState::NoReply)), "{:?}", state(&s, &a));
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
    assert!(wait_for(2000, || s.status().channels[0].stale));
    assert!(wait_for(4000, || matches!(phase(&s), Phase::Stopped { .. })), "{:?}", phase(&s));
    let Phase::Stopped { reason } = phase(&s) else { unreachable!() };
    assert!(reason.contains("taken InfoStream"), "{reason}");
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
    std::thread::sleep(Duration::from_millis(300));

    let b = spawn(opts());
    let kb = key(4000, "ROB_1", 2);
    b.set_channels(vec![kb.clone()]);
    b.connect(target(&fake));
    assert!(wait_for(5000, || phase(&b) == Phase::Streaming));
    assert!(wait_for(3000, || matches!(phase(&a), Phase::Stopped { .. })), "{:?}\n{}\n{:?}", phase(&a), log_text(&a), a.status().counters);
    assert!(stopped_reason(&a).contains("taken InfoStream"), "{}", stopped_reason(&a));
    assert!(wait_for(3000, || log_text(&b).contains("InfoStream sends them all to one program")), "{}", log_text(&b));
    assert_eq!(samples(&b, &kb), 0);
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

    s.answer(false, &others);
    assert!(wait_for(3000, || matches!(phase(&s), Phase::Stopped { .. })));
    assert!(fake.seen().is_empty());
    assert!(wait_for(2000, || fake.open_connections() == 0));

    s.connect(target(&fake));
    assert!(wait_for(4000, || phase(&s) == Phase::AwaitingApproval));
    s.answer(true, &s.status().others.clone());
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));

    let n = fake.connections_total();
    fake.drop_connections();
    assert!(wait_for(5000, || fake.connections_total() > n && phase(&s) == Phase::Streaming));
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
    fake.with(|b| b.reply_delay = Duration::from_millis(40));
    s.set_channels(vec![a.clone(), c.clone()]);
    assert!(wait_for(3000, || samples(&s, &c) > 5), "a channel added while streaming never delivered");
    let na = samples(&s, &a);
    assert!(wait_for(2000, || samples(&s, &a) > na + 5));
    let starts = || fake.seen_props().iter().filter(|p| *p == "StartStream").count();
    let started_before = starts();
    s.set_channels(vec![c.clone()]);
    assert!(wait_for(2000, || fake.streams().len() == 1), "{:?}", fake.streams());
    assert!(wait_for(2000, || starts() > started_before), "no StartStream followed the undefine: {:?}", fake.seen_props());
    assert!(wait_for(2000, || s.status().channels.iter().all(|ch| ch.gaps >= 1)), "the pauses were real: {:?}", s.status().channels);
    let nc = samples(&s, &c);
    assert!(wait_for(2000, || samples(&s, &c) > nc + 10), "delivery stopped after a mid-stream undefine");
    assert!(s.store().get(&a).is_some(), "a removed channel's history is the window's to drop, not the session's");
    assert_eq!(phase(&s), Phase::Streaming, "{}", log_text(&s));
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
    let mut old = FakeController::start(Behaviour::default()).unwrap();
    let ports = Arc::new(Mutex::new(vec![old.port()]));
    let s = spawn(Options { find_vc: finder(ports.clone()), ..opts() });
    let k = key(6000, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&old));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    let from = target(&old);
    let held = history(&s, &k).len();
    old.stop();
    ports.lock().unwrap().clear();
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
    let (mut second, rs2) = restart(&mut first, &ports, Behaviour::default());
    drop(rs);
    assert!(streaming_again(samples(&s, &k)), "{:?}\n{}", phase(&s), log_text(&s));
    let (third, rs3) = restart(&mut second, &ports, Behaviour::default());
    drop(rs2);
    assert!(streaming_again(samples(&s, &k)), "{:?}\n{}", phase(&s), log_text(&s));
    assert_eq!(log_text(&s).matches("connecting again, once").count(), 2, "{}", log_text(&s));
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
    let v = restarted_vc(opts(), &["192.0.2.27"]);
    assert!(wait_for(15000, || matches!(phase(&v.s), Phase::Stopped { .. })), "{:?}\n{}", phase(&v.s), log_text(&v.s));
    let reason = stopped_reason(&v.s);
    assert!(reason.contains("RobotStudio's own") && reason.contains("Connect again"), "{reason}");
    assert!(!log_text(&v.s).contains("connecting again, once"), "{}", log_text(&v.s));
    let n = samples(&v.s, &v.k);
    v.s.connect(target(&v.new));
    assert!(wait_for(5000, || phase(&v.s) == Phase::Streaming && samples(&v.s, &v.k) > n + 20), "{:?}\n{}", phase(&v.s), log_text(&v.s));
}

#[test]
fn a_vc_that_did_not_restart_is_not_connected_again_by_itself() {
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
    let v = restarted_vc(Options { ladder: vec![Duration::from_millis(100), Duration::from_millis(1000)], ..opts() }, &[]);
    let RestartedVc { s, new, rs, .. } = v;
    let again = std::thread::scope(|sc| {
        sc.spawn(|| {
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

    let port = old.port();
    let mut back = FakeController::start_on(port, Behaviour::default()).unwrap();
    assert!(wait_for(8000, || phase(&s) == Phase::Streaming && back.seen_props().iter().any(|p| p == "StartStream")), "{}", log_text(&s));
    back.stop();
    assert!(wait_for(15000, || log_text(&s).matches("{0000000B-0000-4000-8000-00000000000B}").count() == 2), "{}", log_text(&s));
}

#[test]
fn another_tool_taking_infostream_is_caught_before_its_signals_show() {
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
    for k in [&k1, &k2] {
        let h = history(&a, k);
        assert!(h.iter().all(|v| v.abs() <= 1.0), "{k} shows another tool's signal: {:?}", h.iter().filter(|v| v.abs() > 1.0).take(3).collect::<Vec<_>>());
    }
    let a_tail: Vec<String> = fake.seen()[before..].iter().filter(|x| x.conn == 1).map(|x| x.property.clone()).collect();
    assert_eq!(a_tail, vec!["StreamDisconnect".to_string()], "{a_tail:?}");
    assert!(wait_for(1000, || fake.streams().is_empty()), "{:?}", fake.streams());
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(fake.connections_total(), 2, "it went back for InfoStream");
}

#[test]
fn a_takeover_is_left_before_the_newcomer_sets_up_its_signals() {
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
    let a_tail: Vec<String> = fake.seen()[before..].iter().filter(|x| x.conn == 1).map(|x| x.property.clone()).collect();
    assert_eq!(a_tail, vec!["StreamDisconnect".to_string()], "{a_tail:?}");
    assert_eq!(a.status().counters.liveness_checks, 1);
}

#[test]
fn a_network_stall_that_clears_is_not_a_takeover() {
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
    fake.with(|b| {
        b.reply_delay = Duration::from_millis(600);
        b.mute_all = true;
    });
    assert!(wait_for(2000, || a.status().counters.liveness_checks == 1), "no liveness check\n{}", log_text(&a));
    a.set_channels(vec![x.clone(), y.clone()]);
    assert!(wait_for(3000, || fake.handshakes() == 2), "the check was never answered");
    std::thread::sleep(Duration::from_millis(100));
    fake.with(|b| b.mute_all = false);
    assert!(wait_for(5000, || samples(&a, &y) > 20), "{:?}\n{}", phase(&a), log_text(&a));
    assert_eq!(phase(&a), Phase::Streaming, "{}", log_text(&a));
}

#[test]
fn the_flexpendant_alone_is_not_asked_about() {
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
    fake.with(|b| b.mute.insert(4002));
    std::thread::sleep(Duration::from_millis(150));
    fake.with(|b| b.mute.remove(&4002));
    let n = samples(&a, &k2);
    assert!(wait_for(2000, || samples(&a, &k2) > n + 20));
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(phase(&a), Phase::Streaming, "{}", log_text(&a));
    assert!(a.status().channels.iter().find(|c| c.key == k2).unwrap().gaps >= 1);
    fake.with(|b| b.mute_all = true);
    std::thread::sleep(Duration::from_millis(150));
    fake.with(|b| b.mute_all = false);
    assert!(wait_for(3000, || matches!(phase(&a), Phase::Stopped { .. })), "{:?}", phase(&a));
    assert!(stopped_reason(&a).contains("taken InfoStream"), "{}", stopped_reason(&a));
}

#[test]
fn a_define_that_goes_unanswered_mid_stream_does_not_stop_the_rest() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let mut o = opts();
    o.reply_timeout = Duration::from_millis(1500);
    let s = spawn(o);
    let a = key(4002, "ROB_1", 1);
    let c = key(4000, "ROB_1", 1);
    s.set_channels(vec![a.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&a))));
    fake.with(|b| b.hold_define_of = Some(4000));
    s.set_channels(vec![a.clone(), c.clone()]);
    assert!(wait_for(3000, || state(&s, &c) == Some(ChannelState::NoReply)), "{:?}\n{}", state(&s, &c), log_text(&s));
    let n = samples(&s, &a);
    assert!(wait_for(2000, || samples(&s, &a) > n + 20), "delivery never resumed after an unanswered define\n{}", log_text(&s));
    assert_eq!(phase(&s), Phase::Streaming, "{}", log_text(&s));
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
    fake.with(|b| b.hold_define_of = Some(4000));
    s.set_channels(vec![c.clone(), a.clone()]);
    assert!(wait_for(2000, || fake.streams().len() == 2));
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
    s.connect(target(&two));
    s.set_channels(vec![k2.clone()]);
    s.disconnect();
    assert!(wait_for(4000, || phase(&s) == Phase::Idle && two.open_connections() == 0), "{:?}", phase(&s));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(phase(&s), Phase::Idle, "a disconnect made during the switch was lost");
    let chans: Vec<ChannelKey> = s.status().channels.iter().map(|c| c.key.clone()).collect();
    assert_eq!(chans, vec![k2.clone()], "a channel change made during the switch was lost");
    assert!(two.streams().is_empty());

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
    s.connect(target(&one));
    assert!(wait_for(5000, || phase(&s) == Phase::Streaming && samples(&s, &k) > 150));
    assert!(history(&s, &k).len() > 150);
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
    assert!(wait_for(3000, || matches!(state(&s, &a), Some(ChannelState::Defined { .. }))));
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
    fake.with(|b| b.hold_define_of = Some(4000));
    a.set_channels(vec![x.clone(), key(4000, "ROB_1", 1)]);
    assert!(wait_for(1000, || fake.seen_props().iter().filter(|p| *p == "StreamDefine").count() == 2));
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
    a.set_channels(vec![x.clone(), y.clone()]);
    assert!(wait_for(3000, || samples(&a, &y) > 10));
    assert!(wait_for(6000, || fake.clock_ms() > (1u64 << 32) + 1_500));
    assert_eq!(phase(&a), Phase::Streaming, "{}", log_text(&a));
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
    fake.break_connections(Duration::from_millis(2500));
    let n = samples(&s, &k);
    assert!(wait_for(12000, || phase(&s) == Phase::Streaming && samples(&s, &k) > n + 50), "{:?}\n{}", phase(&s), log_text(&s));
    assert!(log_text(&s).contains("still holds the connection that broke"), "{}", log_text(&s));
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
    fake.with(|b| b.extra_clients = vec!["192.0.2.50".into()]);
    fake.drop_connections();
    assert!(wait_for(5000, || matches!(phase(&s), Phase::Stopped { .. })), "{:?}\n{}", phase(&s), log_text(&s));
    let reason = stopped_reason(&s);
    assert!(reason.contains("192.0.2.50") && reason.contains("While the connection was down"), "{reason}");
    assert_eq!(props_from_others(&fake, &[1], "StreamDefine"), 0, "nothing set up on the reconnect");
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
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))), "{}", log_text(&s));
    assert!(log_text(&s).contains("the charts start afresh"), "{}", log_text(&s));
    let first_t = s.store().get(&k).unwrap().lock().first_t().unwrap();
    assert!(first_t > first_last_t, "the first controller's history is still there ({first_t} <= {first_last_t})");
}

#[test]
fn the_first_program_to_connect_infostream_gets_the_samples_subscribed_or_not() {
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
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(opts());
    let k = key(6000, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    let before = s.status().counters.clone();
    {
        let ch = s.store().get(&k).unwrap();
        let _held = ch.lock();
        std::thread::sleep(Duration::from_millis(3500));
    }
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(phase(&s), Phase::Streaming, "{}", log_text(&s));
    let after = s.status().counters.clone();
    assert_eq!((after.liveness_checks, after.reconnects), (before.liveness_checks, before.reconnects), "{}", log_text(&s));
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
    fake.with(|b| {
        b.unanswered.insert("StartStream".into());
    });
    s.set_channels(vec![x.clone(), y.clone()]);
    assert!(wait_for(3000, || samples(&s, &y) > 10), "{}", log_text(&s));
    fake.with(|b| {
        b.mute_all = true;
        b.reply_delay = Duration::from_millis(250);
    });
    assert!(wait_for(6000, || matches!(phase(&s), Phase::Stopped { .. })), "a dead feed went unnoticed\n{}", log_text(&s));
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
    fake.with(|b| {
        b.mute_handshake = true;
        b.hold_delivery = true;
    });
    assert!(wait_for(2000, || s.status().counters.liveness_checks == 1));
    fake.with(|b| b.hold_delivery = false);
    std::thread::sleep(Duration::from_millis(1800));
    fake.with(|b| b.hold_delivery = true);
    assert!(wait_for(2000, || s.status().counters.liveness_checks == 2), "{}", log_text(&s));
    fake.with(|b| b.hold_delivery = false);
    assert!(log_text(&s).contains("did not answer the liveness check"), "{}", log_text(&s));
}

#[test]
fn the_irc5s_tick_is_neither_a_gap_nor_a_takeover() {
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
    fake.cut_network(Duration::from_millis(2500), Duration::from_millis(4000));
    let n = samples(&s, &k);
    assert!(wait_for(15000, || phase(&s) == Phase::Streaming && samples(&s, &k) > n + 50), "{:?}\n{}", phase(&s), log_text(&s));
    let log = log_text(&s);
    assert!(log.contains("not even an answer to the liveness check"), "{log}");
    assert!(log.contains("still holds the connection that broke"), "{log}");
    let seen = fake.seen();
    assert_eq!(seen.iter().filter(|x| x.verb == "SUBSCRIBE").count(), 2, "{seen:?}");
    assert_eq!(seen.iter().filter(|x| x.property == "StreamDefine").count(), 2);
    assert_eq!(last(&s, &k), Some(101.0));
}

#[test]
fn a_long_outage_still_waits_out_the_held_connection_once_the_network_is_back() {
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
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(opts());
    let k = key(6000, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    fake.cut_network(Duration::from_secs(30), Duration::from_secs(30));
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
    fake.cut_network(Duration::from_millis(2000), Duration::from_millis(1000));
    fake.with(|b| b.extra_clients = vec!["192.0.2.50".into()]);
    assert!(wait_for(10000, || matches!(phase(&s), Phase::Stopped { .. })), "{:?}\n{}", phase(&s), log_text(&s));
    let reason = stopped_reason(&s);
    assert!(reason.contains("192.0.2.50") && reason.contains("While the connection was down"), "{reason}");
    assert_eq!(fake.seen().iter().filter(|x| x.verb == "SUBSCRIBE").count(), 1, "nothing set up on the reconnect");
}

fn irc5_stalling(tail: Duration, refuses: bool) -> FakeController {
    let mut b = Behaviour::default();
    b.stall_while_held = Some(tail);
    b.stall_refuses = refuses;
    FakeController::start(b).unwrap()
}

fn through_a_stall(s: &Session, fake: &FakeController, outage: Duration, hold: Duration) -> Duration {
    let k = key(4002, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(fake));
    assert!(wait_for(5000, || streaming_with_samples(s, std::slice::from_ref(&k))));
    let t0 = Instant::now();
    fake.cut_network(outage, hold);
    let n = samples(s, &k);
    assert!(wait_for(20000, || phase(s) == Phase::Streaming && samples(s, &k) > n + 50), "{:?}\n{}", phase(s), log_text(s));
    t0.elapsed()
}

#[test]
fn the_fakes_irc5_stall_outlasts_the_held_connection_by_its_tail() {
    let fake = irc5_stalling(Duration::from_millis(1500), false);
    let _held = std::net::TcpStream::connect(fake.addr()).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let t0 = Instant::now();
    fake.cut_network(Duration::from_millis(300), Duration::from_millis(1000));
    let answered_at = |ms: u64| {
        std::thread::sleep(Duration::from_millis(ms).saturating_sub(t0.elapsed()));
        discovery::hello(fake.addr(), Duration::from_millis(300)).is_ok()
    };
    assert!(!answered_at(500), "answered while the broken connection is held");
    assert!(!answered_at(1400), "answered right after letting it go");
    assert!(answered_at(2800), "not answered after the stall");
}

#[test]
fn an_irc5_that_holds_back_its_answers_after_a_fault_is_connected_through_once_it_answers() {
    let fake = irc5_stalling(Duration::from_millis(1000), false);
    let s = spawn(Options { ladder: vec![Duration::from_millis(100), Duration::from_millis(4000)], ..opts() });
    let took = through_a_stall(&s, &fake, Duration::from_millis(1000), Duration::from_millis(2500));
    let log = log_text(&s);
    assert!(took < Duration::from_millis(5500), "streaming again {took:?} after the cut\n{log}");
    assert!(!log.contains("probably not a controller"), "{log}");
    assert!(log.contains("not answering yet"), "{log}");
    assert!(log.contains("answers again"), "{log}");
}

#[test]
fn an_irc5_that_refuses_connections_after_a_fault_is_tried_again_soon() {
    let fake = irc5_stalling(Duration::from_millis(1000), true);
    let s = spawn(Options { connect_timeout: Duration::from_secs(3), ladder: vec![Duration::from_millis(100), Duration::from_millis(6000)], ..opts() });
    let took = through_a_stall(&s, &fake, Duration::from_millis(1000), Duration::from_millis(4000));
    let log = log_text(&s);
    assert!(took < Duration::from_millis(8000), "streaming again {took:?} after the cut\n{log}");
    assert!(log.contains("refused") && log.contains("not answering yet"), "{log}");
}

#[test]
fn while_an_irc5_recovers_each_try_is_a_fresh_connection_soon_after_the_last() {
    let fake = irc5_stalling(Duration::from_millis(1000), false);
    let s = spawn(Options { ladder: vec![Duration::from_millis(100), Duration::from_millis(4000)], ..opts() });
    let k = key(4002, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    let before = fake.handshakes();
    fake.cut_network(Duration::from_millis(1000), Duration::from_millis(5000));
    let mut short_retry = false;
    let n = samples(&s, &k);
    assert!(
        wait_for(20000, || {
            short_retry |= matches!(phase(&s), Phase::Reconnecting { retry_in, .. } if retry_in == Duration::from_millis(100));
            phase(&s) == Phase::Streaming && samples(&s, &k) > n + 50
        }),
        "{:?}\n{}",
        phase(&s),
        log_text(&s)
    );
    let tries = fake.handshakes() - before;
    assert!(tries >= 6, "{tries} handshakes from the cut to streaming again\n{}", log_text(&s));
    assert!(short_retry, "never shown retrying on the short spacing");
}

#[test]
fn an_irc5_that_stays_unanswering_goes_back_to_the_ladder() {
    let fake = irc5_stalling(Duration::from_millis(1000), false);
    let s = spawn(Options { ladder: vec![Duration::from_millis(100), Duration::from_millis(700)], ..opts() });
    let k = key(4002, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    fake.cut_network(Duration::from_millis(1000), Duration::from_secs(30));
    assert!(wait_for(15000, || log_text(&s).contains("usual schedule")), "{:?}\n{}", phase(&s), log_text(&s));
    assert!(wait_for(3000, || matches!(phase(&s), Phase::Reconnecting { retry_in, .. } if retry_in == Duration::from_millis(700))), "{:?}\n{}", phase(&s), log_text(&s));
    std::thread::sleep(Duration::from_millis(3000));
    let log = log_text(&s);
    assert_eq!(log.matches("not answering yet").count(), 1, "{log}");
    assert_eq!(log.matches("usual schedule").count(), 1, "{log}");
}

#[test]
fn each_outage_gets_its_own_wait_for_the_irc5_to_recover() {
    let fake = irc5_stalling(Duration::from_millis(1000), false);
    let s = spawn(Options { ladder: vec![Duration::from_millis(100), Duration::from_millis(4000)], ..opts() });
    let first = through_a_stall(&s, &fake, Duration::from_millis(1000), Duration::from_millis(2500));
    std::thread::sleep(Duration::from_millis(2000));
    let k = key(4002, "ROB_1", 1);
    let t0 = Instant::now();
    fake.cut_network(Duration::from_millis(1000), Duration::from_millis(2500));
    let n = samples(&s, &k);
    assert!(wait_for(20000, || phase(&s) == Phase::Streaming && samples(&s, &k) > n + 50), "{:?}\n{}", phase(&s), log_text(&s));
    let second = t0.elapsed();
    let log = log_text(&s);
    assert!(second < Duration::from_millis(5500), "first {first:?}, second {second:?}\n{log}");
    assert_eq!(log.matches("not answering yet").count(), 2, "{log}");
    assert_eq!(log.matches("answers again").count(), 2, "{log}");
}

#[test]
fn a_virtual_controller_keeps_the_ladder_when_its_handshake_goes_unanswered() {
    let fake = irc5_stalling(Duration::from_millis(1000), false);
    let s = spawn(Options { ladder: vec![Duration::from_millis(100), Duration::from_millis(700)], ..patient() });
    let k = key(4002, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    fake.cut_network(Duration::from_millis(1000), Duration::from_millis(2500));
    fake.drop_connections();
    assert!(wait_for(8000, || log_text(&s).contains("did not answer the RobAPI handshake")), "{:?}\n{}", phase(&s), log_text(&s));
    assert!(!log_text(&s).contains("not answering yet"), "{}", log_text(&s));
}

fn patient() -> Options {
    Options { vc_pause_patience: Duration::from_secs(10), ..opts() }
}

#[test]
fn a_virtual_controller_that_pauses_is_waited_for() {
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
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let s = spawn(opts());
    let k = key(6000, "ROB_1", 1);
    s.set_channels(vec![k.clone()]);
    s.connect(target(&fake));
    assert!(wait_for(5000, || streaming_with_samples(&s, std::slice::from_ref(&k))));
    fake.with(|b| b.mute_all = true);
    fake.drop_connections();
    assert!(wait_for(2000, || matches!(phase(&s), Phase::Reconnecting { attempt: 1, .. })), "{:?}", phase(&s));
    assert!(wait_for(3000, || fake.connections_total() == 2 && phase(&s) == Phase::Streaming && matches!(state(&s, &k), Some(ChannelState::Defined { .. }))), "{:?}", phase(&s));
    fake.drop_connections();
    assert!(wait_for(2000, || matches!(phase(&s), Phase::Reconnecting { .. })), "{:?}", phase(&s));
    assert!(matches!(phase(&s), Phase::Reconnecting { attempt: 2, retry_in } if retry_in == Duration::from_millis(200)), "{:?}", phase(&s));
}

#[test]
fn a_text_channel_is_not_reported_for_sending_nothing() {
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
