//! Recording, slow logging and "save the last N seconds", end to end against the
//! fake controller, read back and checked value for value.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use spy_core::fake::{Behaviour, FakeController, SignalDef, SignalSource};
use spy_core::log::LogBook;
use spy_core::recording::{self, ChannelInfo, Kind, RecState, Recorder};
use spy_core::request::{Axis, MechUnit};
use spy_core::sample::ValueKind;
use spy_core::session::{AskPolicy, Options, Phase, Session, Target};
use spy_core::store::{ChannelKey, Store};

fn key(signal: u32, axis: u8) -> ChannelKey {
    ChannelKey { signal, unit: MechUnit::new("ROB_1").unwrap(), axis: Axis::new(axis).unwrap() }
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

fn temp_dir(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("spy-rec-{tag}-{}-{}", std::process::id(), Instant::now().elapsed().as_nanos()));
    let _ = std::fs::remove_dir_all(&p);
    p
}

fn streaming(keys: &[ChannelKey]) -> (FakeController, Session) {
    streaming_with(Behaviour::default(), keys)
}

fn streaming_with(b: Behaviour, keys: &[ChannelKey]) -> (FakeController, Session) {
    let fake = FakeController::start(b).unwrap();
    let opts = Options { ask: AskPolicy::Never, ladder: vec![Duration::from_millis(100)], ..Options::default() };
    let s = Session::spawn(opts, Arc::new(LogBook::new()), Arc::new(Store::new()), Arc::new(|| {}));
    s.set_channels(keys.to_vec());
    s.connect(Target { host: "127.0.0.1".into(), port: fake.port() });
    assert!(wait_for(5000, || s.status().phase == Phase::Streaming && s.status().channels.iter().all(|c| c.samples > 0 || c.key.signal == 9872)));
    (fake, s)
}

fn infos(keys: &[ChannelKey]) -> HashMap<ChannelKey, ChannelInfo> {
    keys.iter().map(|k| (k.clone(), ChannelInfo { name: format!("signal {}", k.signal), units: "Nm".into(), description: String::new() })).collect()
}

#[test]
fn a_full_recording_reads_back_exactly() {
    let keys = vec![key(4002, 1), key(9888, 1), key(9872, 1)];
    let (_fake, s) = streaming(&keys);
    let base = temp_dir("full");
    let rec = Recorder::start(&s, &base, "dip test", None, &infos(&keys)).unwrap();
    std::thread::sleep(Duration::from_millis(600));
    rec.marker("before the dip", None);
    rec.marker("placed at a time", Some(128_434_999));
    std::thread::sleep(Duration::from_millis(900));
    let st = rec.stop();
    assert_eq!(st.state, RecState::Finished, "{:?}", st.state);
    assert_eq!(st.lost, 0);

    let back = recording::read(&st.dir).unwrap();
    assert!(back.bad_rows.is_empty(), "{:?}", back.bad_rows);
    assert_eq!(back.meta.kind, Kind::Full);
    assert!(back.meta.complete);
    assert_eq!(back.meta.samples_lost, 0);
    assert_eq!(back.meta.markers.len(), 2);
    assert_eq!(back.meta.markers[0].text, "before the dip");
    assert!(back.meta.markers[0].controller_ms.is_some());
    assert_eq!(back.meta.markers[1].controller_ms, Some(128_434_999), "the time the marker was placed at, not when it was written");
    assert_eq!(back.meta.anchors.len(), 1);
    assert!(back.meta.label == "dip test" && st.dir.to_string_lossy().ends_with("dip test"));

    let torque = &back.data["4002/ROB_1/J1"];
    assert!(torque.len() > 250, "{} rows in 1.5 s", torque.len());
    assert!(torque.iter().all(|&(_, v)| v == 101.0), "a value changed on its way to disk");
    assert!(torque.windows(2).all(|w| w[1].0 - w[0].0 == 4), "controller_ms is not the 4 ms clock");
    let ints = &back.data["4002/ROB_1/J1"].len().min(back.data["9888/ROB_1/J1"].len());
    assert!(*ints > 30);
    assert!(back.data["9888/ROB_1/J1"].iter().all(|&(_, v)| v == -1.0));
    let total: usize = back.data.values().map(|v| v.len()).sum();
    assert_eq!(total as u64, back.meta.rows_written);

    // The description of every channel, including what the controller said.
    let ch = back.meta.channels.iter().find(|c| c.id == "4002/ROB_1/J1").unwrap();
    assert_eq!(ch.stream_id, Some(215));
    assert_eq!(ch.sample_ms, Some(4.032));
    assert_eq!(ch.value_type, Some(ValueKind::Float));
    assert_eq!(ch.name, "signal 4002");
    assert_eq!(back.meta.channels.iter().find(|c| c.id == "9888/ROB_1/J1").unwrap().value_type, Some(ValueKind::Int));
    // The string event is written as quoted text.
    let csv = std::fs::read_to_string(st.dir.join("data.csv")).unwrap();
    assert!(csv.starts_with("controller_ms,channel,value\n"));
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn the_slow_log_aggregates_per_interval() {
    let keys = vec![key(4002, 2), key(4000, 1)];
    let (_fake, s) = streaming(&keys);
    let base = temp_dir("slow");
    let rec = Recorder::start(&s, &base, "", Some(100), &infos(&keys)).unwrap();
    std::thread::sleep(Duration::from_millis(1300));
    let st = rec.stop();
    assert_eq!(st.state, RecState::Finished);
    let back = recording::read(&st.dir).unwrap();
    assert_eq!(back.meta.kind, Kind::Slow);
    assert_eq!(back.meta.interval_ms, Some(100));
    let rows = &back.data["4002/ROB_1/J2"];
    assert!((10..=15).contains(&rows.len()), "{} intervals in 1.3 s", rows.len());
    assert!(rows.iter().all(|&(_, mean)| mean == 102.0));
    assert!(rows.iter().all(|&(t, _)| t % 100 == 0), "intervals are aligned to the controller clock");
    // count column: 25 samples per full 100 ms interval at a 4 ms tick.
    let text = std::fs::read_to_string(st.dir.join("slow.csv")).unwrap();
    let counts: Vec<u64> = text.lines().skip(1).filter(|l| l.contains("4002/ROB_1/J2")).map(|l| l.split(',').nth(2).unwrap().parse().unwrap()).collect();
    assert!(counts[1..counts.len() - 1].iter().all(|&c| c == 25), "{counts:?}");
    // 4000 ramps (t % 1000 * 0.001 + 10): its min and max differ within an interval.
    let ramp: Vec<Vec<f64>> = text.lines().skip(1).filter(|l| l.contains("4000/ROB_1/J1")).map(|l| l.split(',').skip(3).map(|x| x.parse().unwrap()).collect()).collect();
    assert!(ramp.iter().any(|r| r[2] > r[1]), "min/max must span the interval");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn save_the_last_seconds_from_history() {
    let keys = vec![key(4002, 1), key(6000, 1)];
    let (fake, s) = streaming(&keys);
    std::thread::sleep(Duration::from_millis(1500));
    let base = temp_dir("snap");
    let timeline = s.status().timeline.clone();
    let (dir, rows) = recording::write_snapshot(&base, "trip", s.store(), &timeline, &keys, &infos(&keys), 0.5, "127.0.0.1:x", None).unwrap();
    let back = recording::read(&dir).unwrap();
    assert_eq!(back.meta.kind, Kind::Snapshot);
    assert!(back.meta.complete);
    assert_eq!(back.meta.rows_written, rows);
    for k in &keys {
        let d = &back.data[&k.id()];
        assert!((120..=130).contains(&d.len()), "{} rows for 0.5 s of {}", d.len(), k);
    }
    // controller_ms is the controller's clock, not the chart's timeline.
    let last = back.data["4002/ROB_1/J1"].last().unwrap().0;
    let now = fake.clock_ms() as i64;
    assert!((now - last).abs() < 1000, "snapshot time {last} vs controller {now}");
    // Time order across channels.
    let csv = std::fs::read_to_string(dir.join("data.csv")).unwrap();
    let ts: Vec<i64> = csv.lines().skip(1).map(|l| l.split(',').next().unwrap().parse().unwrap()).collect();
    assert!(ts.windows(2).all(|w| w[1] >= w[0]));
    assert!(recording::write_snapshot(&base, "x", s.store(), &timeline, &keys, &infos(&keys), -1.0, "", None).is_err());
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_recording_carries_on_across_a_reconnect() {
    let keys = vec![key(4002, 1)];
    let (fake, s) = streaming(&keys);
    let base = temp_dir("reconnect");
    let rec = Recorder::start(&s, &base, "", None, &infos(&keys)).unwrap();
    std::thread::sleep(Duration::from_millis(400));
    fake.drop_connections();
    assert!(wait_for(5000, || fake.connections_total() >= 2 && s.status().phase == Phase::Streaming));
    std::thread::sleep(Duration::from_millis(400));
    // A deliberate disconnect is noted as one, not as a loss.
    s.disconnect();
    assert!(wait_for(3000, || s.status().phase == Phase::Idle));
    let st = rec.stop();
    let back = recording::read(&st.dir).unwrap();
    assert!(back.meta.events.iter().any(|e| e.kind == "lost"), "{:?}", back.meta.events);
    assert_eq!(back.meta.events.last().map(|e| e.kind.as_str()), Some("disconnected"), "{:?}", back.meta.events);
    assert!(back.meta.events.iter().filter(|e| e.kind == "connected").count() >= 1);
    assert_eq!(back.meta.anchors.len(), 2, "one wall-clock anchor per connection");
    let d = &back.data["4002/ROB_1/J1"];
    // The outage is a hole in controller time, not filled in.
    assert!(d.windows(2).any(|w| w[1].0 - w[0].0 > 20));
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn strings_with_commas_quotes_and_line_breaks_read_back_whole() {
    // A controller string is data, and can hold anything a CSV cares about.
    let mut b = Behaviour::default();
    b.signals.insert(9873, SignalDef { source: SignalSource::text(|(t, _, _)| format!("line one\r\nline \"two\", {}", t / 200 % 1000)), sample_ms: 4.032 });
    let keys = vec![key(9873, 1), key(4002, 1)];
    let (_fake, s) = streaming_with(b, &keys);
    let base = temp_dir("text");
    let rec = Recorder::start(&s, &base, "", None, &infos(&keys)).unwrap();
    std::thread::sleep(Duration::from_millis(1100));
    let st = rec.stop();
    assert_eq!(st.state, RecState::Finished, "{:?}", st.state);
    let back = recording::read(&st.dir).unwrap();
    assert!(back.bad_rows.is_empty(), "{:?}", back.bad_rows);
    let texts = &back.text["9873/ROB_1/J1"];
    assert!(texts.len() >= 3, "{texts:?}");
    assert!(texts.iter().all(|(_, v)| v.starts_with("line one\r\nline \"two\", ")), "{texts:?}");
    let torque = &back.data["4002/ROB_1/J1"];
    assert!(torque.len() > 200 && torque.iter().all(|&(_, v)| v == 101.0), "a string row bled into the numbers");
    assert!(!back.data.contains_key("9873/ROB_1/J1"));
    assert_eq!((texts.len() + torque.len()) as u64, back.meta.rows_written);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_slow_consumer_loses_events_countably() {
    let keys = vec![key(4002, 1)];
    let (_fake, s) = streaming(&keys);
    let tap = s.tap(4);
    std::thread::sleep(Duration::from_millis(300));
    let dropped = tap.dropped.load(std::sync::atomic::Ordering::Relaxed);
    assert!(dropped > 10, "a full tap must count what it drops: {dropped}");
    // The status snapshot is published at most every 50 ms; it catches up.
    assert!(wait_for(1000, || s.status().counters.dropped_to_taps >= dropped));
    assert_eq!(s.status().phase, Phase::Streaming, "a slow consumer must not affect the session");
    drop(tap);
    std::thread::sleep(Duration::from_millis(100));
}
