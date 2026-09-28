//! The window's own code, driven through its widgets with simulated input (egui's
//! test harness: no real mouse, no desktop) against the in-process fake controller.
//! These are the paths a person takes: connect, answer the other-clients question,
//! add channels through the dialog, record, save what just happened, pause, place
//! cursors, switch the phone view on, reset InfoStream, and come back to the same
//! channels next time.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;

use spy_core::fake::{Behaviour, FakeController, SignalDef, SignalSource};
use spy_core::session::{AskPolicy, Phase};

use crate::app::SpyApp;
use crate::view;

fn temp_dir(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("spy-ui-{tag}-{}-{}", std::process::id(), Instant::now().elapsed().as_nanos()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn harness(dir: PathBuf, ask: AskPolicy) -> Harness<'static, SpyApp> {
    Harness::builder().with_size((1400.0, 900.0)).with_max_steps(20).build_eframe(move |cc| {
        let mut app = SpyApp::with_policy(cc, dir.clone(), false, ask);
        app.settings.record_dir = Some(dir.join("recordings"));
        app.show_guide = false;
        app
    })
}

/// Step the window until `f` holds, as a person waits for the screen to change.
fn wait(h: &mut Harness<'static, SpyApp>, ms: u64, mut f: impl FnMut(&SpyApp) -> bool) -> bool {
    let end = Instant::now() + Duration::from_millis(ms);
    loop {
        let _ = h.run_ok();
        if f(h.state()) {
            // Drawn once more: the worker may have moved on after the frame just drawn,
            // and a query would read that older frame.
            let _ = h.run_ok();
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn phase(a: &SpyApp) -> Phase {
    a.session.status().phase.clone()
}

fn connect(h: &mut Harness<'static, SpyApp>, fake: &FakeController) {
    h.state_mut().host_input = "127.0.0.1".into();
    h.state_mut().port_input = fake.port().to_string();
    h.get_by_label("Connect").click();
}

/// Add a signal through the add-channel dialog, as a person would.
fn add_via_dialog(h: &mut Harness<'static, SpyApp>, signal: u32, button: &str) {
    h.state_mut().open_add(signal);
    let _ = h.run_ok();
    h.get_by_label(button).click();
    let _ = h.run_ok();
}

#[test]
fn connect_add_a_channel_and_read_it_live() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let mut h = harness(temp_dir("connect"), AskPolicy::Remote);
    connect(&mut h, &fake);
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming), "{:?}", phase(h.state()));

    add_via_dialog(&mut h, 4002, "Add");
    assert_eq!(h.state().chans.len(), 1);
    assert!(h.state().add.is_none(), "the dialog closes after adding");
    assert!(wait(&mut h, 5000, |a| a.session.status().channels.first().is_some_and(|c| c.samples > 20)));
    // The card says LIVE and shows the value (the fake's 4002 on ROB_1 axis 1 is 101).
    assert!(h.query_by_label("LIVE").is_some());
    assert!(h.query_by_label("101.000").is_some(), "the 150 ms mean is shown");

    // A second add of the same channel is refused, not duplicated.
    add_via_dialog(&mut h, 4002, "Add");
    assert_eq!(h.state().chans.len(), 1);

    // "All six axes" adds six channels overlaid in one chart.
    h.state_mut().add = None;
    add_via_dialog(&mut h, 4000, "Add all six axes");
    assert_eq!(h.state().chans.len(), 7);
    let lanes: std::collections::BTreeSet<u32> = h.state().chans[1..].iter().map(|c| c.lane).collect();
    assert_eq!(lanes.len(), 1, "the six share one chart");

    // Past twelve: the dialog says how many are free and greys out what does not
    // fit, and adding past it anyway is refused with a message, never truncated.
    h.state_mut().add = None;
    h.state_mut().open_add(4003);
    let _ = h.run_ok();
    assert!(h.query_by_label_contains("5 of 12 channels free").is_some());
    h.get_by_label("Add all six axes").click();
    let _ = h.run_ok();
    assert_eq!(h.state().chans.len(), 7, "six more would make 13");
    h.state_mut().add = None;
    let six: Vec<spy_core::store::ChannelKey> = (1..=6).map(|a| spy_core::store::ChannelKey { signal: 4003, unit: spy_core::request::MechUnit::new("ROB_1").unwrap(), axis: spy_core::request::Axis::new(a).unwrap() }).collect();
    assert!(!h.state_mut().add_channels(six, true));
    assert_eq!(h.state().chans.len(), 7);
    assert!(h.state().toasts.iter().any(|t| t.text.contains("At most 12 channels at once: 5 free")), "the refusal is said");
}

#[test]
fn a_starved_session_says_what_to_do_where_it_is_seen() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    // Another program subscribed first, so every sample goes to it (RobotStudio's
    // signal analyzer left open, say).
    let mut other = std::net::TcpStream::connect(("127.0.0.1", fake.port())).unwrap();
    std::io::Write::write_all(&mut other, &spy_core::request::subscribe(1, "127.0.0.1")).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let mut h = harness(temp_dir("starved"), AskPolicy::Remote);
    connect(&mut h, &fake);
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming));
    add_via_dialog(&mut h, 4002, "Add");
    assert!(wait(&mut h, 8000, |a| a.session.status().advice.is_some()), "{:?}", phase(h.state()));
    let _ = h.run_ok();
    assert!(h.query_all_by_label_contains("is getting all of them: close its signal view").next().is_some(), "the advice is not on screen");
    drop(other);
}

#[test]
fn the_other_clients_question_is_asked_and_answered() {
    let b = Behaviour { extra_clients: vec!["192.0.2.27".into()], ..Behaviour::default() };
    let fake = FakeController::start(b).unwrap();
    let mut h = harness(temp_dir("ask"), AskPolicy::Always);
    connect(&mut h, &fake);
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::AwaitingApproval));
    assert!(h.query_by_label("Other programs are connected to this controller").is_some());
    // Named in the question, and in the session line behind it.
    assert!(h.query_all_by_label_contains("192.0.2.27").count() >= 2, "the other client is named");
    h.get_by_label("Cancel").click();
    assert!(wait(&mut h, 3000, |a| matches!(phase(a), Phase::Stopped { .. })));
    assert!(fake.seen().is_empty(), "declining sends nothing");

    h.get_by_label("Connect").click();
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::AwaitingApproval));
    h.get_by_label("Take InfoStream").click();
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming));
}

#[test]
fn a_real_controllers_flexpendant_alone_asks_nothing() {
    // Every real IRC5 lists its FlexPendant on its internal network: named, not asked
    // about (the operator's decision, 2026-09-26).
    let b = Behaviour { extra_clients: vec!["192.168.126.10".into()], ..Behaviour::default() };
    let fake = FakeController::start(b).unwrap();
    let mut h = harness(temp_dir("pendant"), AskPolicy::Always);
    connect(&mut h, &fake);
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming), "{:?}", phase(h.state()));
    assert!(h.query_by_label("Other programs are connected to this controller").is_none());
    assert!(h.query_all_by_label_contains("192.168.126.10 [FlexPendant]").count() >= 1, "the pendant is named as such");
}

#[test]
fn record_save_last_and_slow_log_write_their_folders() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let dir = temp_dir("rec");
    let mut h = harness(dir.clone(), AskPolicy::Remote);
    connect(&mut h, &fake);
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming));
    add_via_dialog(&mut h, 4002, "Add");
    assert!(wait(&mut h, 5000, |a| a.session.status().channels.first().is_some_and(|c| c.samples > 50)));

    h.get_by_label("● REC").click();
    assert!(wait(&mut h, 2000, |a| a.recorder.is_some()));
    std::thread::sleep(Duration::from_millis(800));
    h.get_by_label_contains("■ STOP").click();
    assert!(wait(&mut h, 3000, |a| a.recorder.is_none()));

    h.get_by_label("Slow log").click();
    assert!(wait(&mut h, 2000, |a| a.slow.is_some()));
    std::thread::sleep(Duration::from_millis(1500));
    h.get_by_label_contains("■ Slow log").click();
    assert!(wait(&mut h, 3000, |a| a.slow.is_none()));

    h.get_by_label("Save last").click();
    assert!(wait(&mut h, 5000, |a| a.snapshot_job.is_none() && a.last_folder.is_some()));

    let rec = dir.join("recordings");
    let mut kinds = Vec::new();
    for e in std::fs::read_dir(&rec).unwrap() {
        let loaded = spy_core::recording::read(&e.unwrap().path()).unwrap();
        assert!(loaded.meta.complete);
        assert!(loaded.data["4002/ROB_1/J1"].iter().all(|&(_, v)| v == 101.0));
        kinds.push(format!("{:?}", loaded.meta.kind));
    }
    kinds.sort();
    assert_eq!(kinds, vec!["Full", "Slow", "Snapshot"]);
}

#[test]
fn pause_cursors_and_markers() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let mut h = harness(temp_dir("pause"), AskPolicy::Remote);
    connect(&mut h, &fake);
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming));
    add_via_dialog(&mut h, 4000, "Add");
    assert!(wait(&mut h, 5000, |a| a.session.status().channels.first().is_some_and(|c| c.samples > 50)));

    h.key_press(egui::Key::Space);
    let _ = h.run_ok();
    assert!(h.state().paused_at.is_some(), "Space pauses");
    assert!(h.query_by_label_contains("PAUSED").is_some());
    h.get_by_label("▶ Live").click();
    let _ = h.run_ok();
    assert!(h.state().paused_at.is_none());

    h.key_press(egui::Key::M);
    let _ = h.run_ok();
    assert_eq!(h.state().markers.len(), 1, "M drops a marker");

    h.get_by_label("Cursors").click();
    let _ = h.run_ok();
    h.state_mut().cursor_a = Some(0.1);
    h.state_mut().cursor_b = Some(0.3);
    let _ = h.run_ok();
    assert!(h.query_by_label("B − A").is_some(), "the cursor table shows");
    assert!(h.query_by_label_contains("Δt 0.200 s").is_some());
}

#[test]
fn the_phone_view_serves_what_the_window_shows() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let mut h = harness(temp_dir("phone"), AskPolicy::Remote);
    h.state_mut().settings.phone_port = 0; // any free port
    connect(&mut h, &fake);
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming));
    add_via_dialog(&mut h, 5027, "Add");
    assert!(wait(&mut h, 5000, |a| a.session.status().channels.first().is_some_and(|c| c.samples > 50)));
    h.get_by_label("Phone view").click();
    assert!(wait(&mut h, 2000, |a| a.phone.is_some()));
    let port = h.state().phone.as_ref().unwrap().port();
    std::thread::sleep(Duration::from_millis(300));
    let _ = h.run_ok();
    let body = {
        use std::io::{Read, Write};
        let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.write_all(b"GET /data HTTP/1.1\r\n\r\n").unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out
    };
    assert!(body.contains("DC-link voltage") && body.contains("356.700") && body.contains("\"stale\":false"), "{body}");
    h.get_by_label("Phone view").click();
    assert!(wait(&mut h, 2000, |a| a.phone.is_none()));
}

#[test]
fn reset_infostream_asks_first() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let mut h = harness(temp_dir("reset"), AskPolicy::Remote);
    connect(&mut h, &fake);
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming));
    // "Controller" is both the menu and the label on the address bar; the menu comes first.
    h.get_all_by_label("Controller").next().unwrap().click();
    let _ = h.run_ok();
    h.get_by_label("Reset InfoStream...").click();
    let _ = h.run_ok();
    assert!(h.query_by_label("Reset InfoStream?").is_some(), "it asks");
    assert!(!fake.seen_props().iter().any(|p| p == "StreamUndefineAll"), "nothing sent before the answer");
    h.get_by_label("Reset InfoStream").click();
    let _ = h.run_ok();
    assert!(wait(&mut h, 2000, |_| fake.seen_props().iter().any(|p| p == "StreamUndefineAll")));
}

#[test]
fn connecting_to_another_controller_finishes_the_recording() {
    let first = FakeController::start(Behaviour::default()).unwrap();
    let second = FakeController::start(Behaviour::default()).unwrap();
    let dir = temp_dir("elsewhere");
    let mut h = harness(dir.clone(), AskPolicy::Remote);
    connect(&mut h, &first);
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming));
    add_via_dialog(&mut h, 4002, "Add");
    assert!(wait(&mut h, 5000, |a| a.session.status().channels.first().is_some_and(|c| c.samples > 20)));
    h.get_by_label("● REC").click();
    assert!(wait(&mut h, 2000, |a| a.recorder.is_some()));
    h.key_press(egui::Key::M);
    let _ = h.run_ok();
    h.get_by_label("Disconnect").click();
    assert!(wait(&mut h, 3000, |a| phase(a) == Phase::Idle));
    assert!(h.state().recorder.is_some(), "a disconnect alone does not end the recording");

    h.state_mut().port_input = second.port().to_string();
    h.get_by_label("Connect").click();
    let _ = h.run_ok();
    assert!(h.state().recorder.is_none(), "another controller must not continue the same recording");
    assert!(h.state().markers.is_empty(), "markers on the old clock are dropped");
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming));
    let rec = std::fs::read_dir(dir.join("recordings")).unwrap().next().unwrap().unwrap().path();
    let loaded = spy_core::recording::read(&rec).unwrap();
    assert!(loaded.meta.complete);
    assert_eq!(loaded.meta.controller, format!("127.0.0.1:{}", first.port()));
}

#[test]
fn statistics_since_reset_start_afresh_on_another_controller() {
    // The first controller has been up far longer: every sample of the second is
    // earlier on the clock than where the statistics had got to, and used to be
    // passed over (the card kept the first controller's min/max/mean, undimmed).
    let first = FakeController::start(Behaviour::default()).unwrap();
    first.set_clock_ms(900_000_000);
    let mut b = Behaviour::default();
    b.signals.insert(4002, SignalDef { source: SignalSource::float(|_| 555.0), sample_ms: 4.032 });
    let second = FakeController::start(b).unwrap();
    let mut h = harness(temp_dir("stats"), AskPolicy::Remote);
    connect(&mut h, &first);
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming));
    add_via_dialog(&mut h, 4002, "Add");
    assert!(wait(&mut h, 5000, |a| a.chans[0].stats.n > 50));
    assert_eq!(h.state().chans[0].stats.max, 101.0);
    h.get_by_label("Disconnect").click();
    assert!(wait(&mut h, 3000, |a| phase(a) == Phase::Idle));
    h.state_mut().port_input = second.port().to_string();
    h.get_by_label("Connect").click();
    assert!(wait(&mut h, 5000, |a| a.chans[0].stats.n > 50 && a.chans[0].stats.max == 555.0), "stats: {:?}", h.state().chans[0].stats);
    assert_eq!(h.state().chans[0].stats.min, 555.0, "nothing of the first controller's left in them");
}

#[test]
fn a_recording_closes_itself_on_a_different_controller_behind_the_address() {
    // Same address, another robot (the cable moved): the recording must not carry
    // on with the second controller's samples under the same channel ids.
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let dir = temp_dir("otherbox");
    let mut h = harness(dir.clone(), AskPolicy::Remote);
    connect(&mut h, &fake);
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming));
    add_via_dialog(&mut h, 4002, "Add");
    assert!(wait(&mut h, 5000, |a| a.session.status().channels.first().is_some_and(|c| c.samples > 20)));
    h.get_by_label("● REC").click();
    assert!(wait(&mut h, 2000, |a| a.recorder.is_some()));
    h.get_by_label("Disconnect").click();
    assert!(wait(&mut h, 3000, |a| phase(a) == Phase::Idle));
    fake.with(|b| b.system_id = "{0000000B-0000-4000-8000-00000000000B}".into());
    h.get_by_label("Connect").click();
    assert!(wait(&mut h, 5000, |a| a.recorder.is_none()), "the recording carried on");
    assert!(h.state().toasts.iter().any(|t| t.text.contains("Recording closed") && t.text.contains("different one")), "the person is told");
    let rec = std::fs::read_dir(dir.join("recordings")).unwrap().next().unwrap().unwrap().path();
    let loaded = spy_core::recording::read(&rec).unwrap();
    assert!(loaded.meta.complete && loaded.meta.notes.contains("0000000B"), "{:?}", loaded.meta.notes);
}

#[test]
fn connect_works_again_after_the_worker_hit_an_internal_error() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let mut h = harness(temp_dir("respawn"), AskPolicy::Remote);
    connect(&mut h, &fake);
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming));
    add_via_dialog(&mut h, 4002, "Add");
    assert!(wait(&mut h, 5000, |a| a.session.status().channels.first().is_some_and(|c| c.samples > 20)));
    h.state().session.crash_for_test();
    assert!(wait(&mut h, 5000, |a| matches!(phase(a), Phase::Stopped { .. })), "{:?}", phase(h.state()));
    h.get_by_label("Connect").click();
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming && a.session.status().channels.first().is_some_and(|c| c.samples > 20)), "Connect did nothing after the internal error: {:?}", phase(h.state()));
}

#[test]
fn an_internal_error_last_time_is_said_once_at_the_next_start() {
    let dir = temp_dir("crash");
    std::fs::write(dir.join("crash.txt"), "ABB Signal Spy crashed at ...").unwrap();
    let h = harness(dir.clone(), AskPolicy::Remote);
    assert!(h.state().toasts.iter().any(|t| t.text.contains("internal error last time")), "not said");
    assert!(!dir.join("crash.txt").exists(), "kept under its own name, so it is said once");
    assert!(std::fs::read_dir(&dir).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().starts_with("crash-")));
    drop(h);
    let h = harness(dir, AskPolicy::Remote);
    assert!(!h.state().toasts.iter().any(|t| t.text.contains("internal error last time")), "said again");
}

#[test]
fn a_recording_opens_for_review_and_is_never_shown_as_live() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let dir = temp_dir("review");
    let mut h = harness(dir.clone(), AskPolicy::Remote);
    connect(&mut h, &fake);
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming));
    add_via_dialog(&mut h, 4002, "Add");
    assert!(wait(&mut h, 5000, |a| a.session.status().channels.first().is_some_and(|c| c.samples > 20)));
    h.get_by_label("● REC").click();
    assert!(wait(&mut h, 2000, |a| a.recorder.is_some()));
    std::thread::sleep(Duration::from_millis(800));
    h.get_by_label_contains("■ STOP").click();
    assert!(wait(&mut h, 3000, |a| a.recorder.is_none()));

    // File, Open a recording: the recordings folder's recordings are listed.
    h.state_mut().show_recordings = true;
    let _ = h.run_ok();
    assert!(h.query_by_label("every sample").is_some(), "the recording is listed");
    h.get_all_by_label("Open").next().expect("an Open button per recording").click();
    assert!(wait(&mut h, 5000, |a| a.review.is_some()), "the recording did not open");
    let _ = h.run_ok();
    assert!(h.query_by_label("REVIEWING").is_some(), "the banner says what is shown");
    assert!(h.query_all_by_label_contains("Not live.").next().is_some());
    assert!(h.query_by_label("LIVE").is_none(), "no status word of the live cards while reviewing");
    let r = h.state().review.as_ref().unwrap().review.clone();
    assert!(r.wall_clock && r.channel("4002/ROB_1/J1").is_some_and(|c| c.v.len() > 100 && c.v.iter().all(|&v| v == 101.0)));
    assert!(h.query_all_by_label_contains("samples in view").next().is_some(), "statistics of the stretch in view");
    assert_eq!(phase(h.state()), Phase::Streaming, "the live session carries on underneath");

    h.get_by_label("Close the recording").click();
    let _ = h.run_ok();
    assert!(h.state().review.is_none());
    assert!(wait(&mut h, 2000, |a| a.review.is_none()) && h.query_by_label("LIVE").is_some(), "back to live");
}

fn files_ending(dir: &std::path::Path, suffix: &str) -> Vec<PathBuf> {
    std::fs::read_dir(dir).map(|r| r.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.to_string_lossy().ends_with(suffix)).collect()).unwrap_or_default()
}

/// 6010 as measured: 0.5 rad/s with runs of padding zeros between.
fn padded_speed() -> Behaviour {
    let mut b = Behaviour::default();
    b.signals.insert(6010, SignalDef { source: SignalSource::float(|(t, _, _)| if [0, 1, 4, 5, 8].contains(&(t / 4 % 10)) { 0.5 } else { 0.0 }), sample_ms: 4.032 });
    b
}

/// A saved view's rows of one channel: its values, and every row's time checked.
fn saved_rows(text: &str, id: &str, units: &str) -> Vec<f64> {
    let rows: Vec<&str> = text.lines().skip(1).filter(|r| r.contains(&format!(",{id},"))).collect();
    let now = std::time::SystemTime::now();
    for r in &rows {
        assert!(r.contains(&format!(",\"{units}\",")), "{r}");
        let utc = spy_core::util::parse_iso(r.split(',').next().unwrap()).unwrap_or_else(|| panic!("no time in {r}"));
        let off = now.duration_since(utc).map(|d| d.as_secs_f64()).unwrap_or_else(|e| -e.duration().as_secs_f64());
        assert!((0.0..30.0).contains(&off), "{r} is {off} s from now");
    }
    rows.iter().map(|r| r.rsplit(',').next().unwrap().parse().unwrap()).collect()
}

/// As recorded: the padding zeros kept, the speed in degrees.
fn assert_padded_speed(v: &[f64]) {
    assert!(v.len() > 50, "{} rows", v.len());
    assert!(v.iter().all(|&x| x == 0.0 || (x - 28.64788975654116).abs() < 1e-9), "{:?}", &v[..10]);
    assert!(v.iter().filter(|&&x| x == 0.0).count() > v.len() / 4, "the padding zeros are kept: {:?}", &v[..10]);
}

#[test]
fn what_is_in_view_saves_as_csv_and_png() {
    let fake = FakeController::start(padded_speed()).unwrap();
    let dir = temp_dir("export");
    let mut h = harness(dir.clone(), AskPolicy::Remote);
    connect(&mut h, &fake);
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming));
    add_via_dialog(&mut h, 4002, "Add");
    add_via_dialog(&mut h, 6010, "Add");
    assert!(wait(&mut h, 5000, |a| a.session.status().channels.len() == 2 && a.session.status().channels.iter().all(|c| c.samples > 100) && a.view_ms.is_some()));
    h.get_by_label("Save CSV").click();
    let _ = h.run_ok();
    assert!(wait(&mut h, 5000, |a| a.export_job.is_none()));
    let rec = dir.join("recordings");
    assert!(files_ending(&rec, ".part").is_empty());
    let csvs = files_ending(&rec, " view.csv");
    assert_eq!(csvs.len(), 1, "one CSV saved: {csvs:?}");
    let text = std::fs::read_to_string(&csvs[0]).unwrap();
    let mut lines = text.lines();
    assert_eq!(lines.next(), Some("time_utc,t_s,channel,name,units,value"));
    let rows: Vec<&str> = lines.collect();
    let torque = saved_rows(&text, "4002/ROB_1/J1", "Nm");
    assert!(torque.len() > 50 && torque.iter().all(|&v| v == 101.0), "{:?}", &torque[..2]);
    assert_padded_speed(&saved_rows(&text, "6010/ROB_1/J1", "deg/s"));
    assert_eq!(rows.len(), torque.len() + saved_rows(&text, "6010/ROB_1/J1", "deg/s").len());
    let t: Vec<f64> = rows.iter().map(|r| r.split(',').nth(1).unwrap().parse().unwrap()).collect();
    assert!(t.windows(2).all(|w| w[0] <= w[1]), "rows in time order");

    // The picture: the screenshot the window asks for arrives as an event.
    h.get_by_label("Save PNG").click();
    let _ = h.run_ok();
    assert!(h.state().png_pending, "a screenshot was asked for");
    let image = std::sync::Arc::new(egui::ColorImage::filled([1400, 900], egui::Color32::DARK_GRAY));
    h.event(egui::Event::Screenshot { viewport_id: egui::ViewportId::ROOT, user_data: egui::UserData::default(), image });
    let _ = h.run_ok();
    assert!(!h.state().png_pending);
    let pngs = files_ending(&rec, " charts.png");
    assert_eq!(pngs.len(), 1, "{pngs:?}");
    assert!(std::fs::read(&pngs[0]).unwrap().starts_with(b"\x89PNG"));
}

#[test]
fn a_reviewed_stretch_saves_as_csv() {
    let mut b = padded_speed();
    // Text is sent when it changes: this changes every 40 ms.
    b.signals.insert(9872, SignalDef { source: SignalSource::text(|(t, _, _)| format!("wobj{}", t / 40 % 2)), sample_ms: 4.032 });
    let fake = FakeController::start(b).unwrap();
    let dir = temp_dir("review-export");
    let mut h = harness(dir.clone(), AskPolicy::Remote);
    connect(&mut h, &fake);
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming));
    add_via_dialog(&mut h, 4002, "Add");
    add_via_dialog(&mut h, 6010, "Add");
    add_via_dialog(&mut h, 9872, "Add");
    assert!(wait(&mut h, 5000, |a| a.session.status().channels.len() == 3 && a.session.status().channels.iter().all(|c| c.samples > 5)));
    h.get_by_label("● REC").click();
    assert!(wait(&mut h, 2000, |a| a.recorder.is_some()));
    std::thread::sleep(Duration::from_millis(600));
    h.get_by_label_contains("■ STOP").click();
    assert!(wait(&mut h, 3000, |a| a.recorder.is_none()));
    let folder = std::fs::read_dir(dir.join("recordings")).unwrap().next().unwrap().unwrap().path();
    h.state_mut().open_recording(folder);
    assert!(wait(&mut h, 5000, |a| a.review.is_some()));
    let _ = h.run_ok();
    h.get_by_label("Save CSV").click();
    let _ = h.run_ok();
    assert!(wait(&mut h, 5000, |a| a.export_job.is_none()));
    let csvs = files_ending(&dir.join("recordings"), " view.csv");
    assert_eq!(csvs.len(), 1, "{csvs:?}");
    let text = std::fs::read_to_string(&csvs[0]).unwrap();
    let torque = saved_rows(&text, "4002/ROB_1/J1", "Nm");
    assert!(torque.len() > 50 && torque.iter().all(|&v| v == 101.0), "{}", text.lines().take(3).collect::<Vec<_>>().join("\n"));
    // A held signal is saved as recorded, padding and all, not as the charts read it.
    assert_padded_speed(&saved_rows(&text, "6010/ROB_1/J1", "deg/s"));
    let words: Vec<&str> = text.lines().filter(|r| r.contains(",9872/ROB_1/J1,")).collect();
    assert!(words.len() > 5 && words.iter().all(|r| r.ends_with(",\"\",\"wobj0\"") || r.ends_with(",\"\",\"wobj1\"")), "{:?}", &words[..words.len().min(2)]);
    let t: Vec<f64> = text.lines().skip(1).map(|r| r.split(',').nth(1).unwrap().parse().unwrap()).collect();
    assert!(t.windows(2).all(|w| w[0] <= w[1]), "rows in time order");
    assert_eq!(t.len(), torque.len() + words.len() + saved_rows(&text, "6010/ROB_1/J1", "deg/s").len());
}

#[test]
fn a_channel_set_adds_or_replaces_in_one_go() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let mut h = harness(temp_dir("sets"), AskPolicy::Remote);
    connect(&mut h, &fake);
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming));
    let ids = |a: &SpyApp| a.chans.iter().map(|c| c.key.id()).collect::<Vec<_>>();

    // The first set, the DC links, on both robots, in one chart.
    h.get_by_label("Channel sets...").click();
    let _ = h.run_ok();
    assert!(h.query_by_label_contains("no physical measurements").is_some(), "a virtual controller has no DC link, and the dialog says so");
    h.get_by_label("Add").click();
    let _ = h.run_ok();
    assert!(h.state().sets.is_none(), "the dialog closes");
    assert_eq!(ids(h.state()), ["5027/ROB_1/J1", "5027/ROB_2/J1"]);
    assert_eq!(h.state().lanes(&[true; 2]).len(), 1);

    // One robot's torques: the second robot's.
    h.get_by_label("Channel sets...").click();
    let _ = h.run_ok();
    h.get_by_label("Torques").click();
    let _ = h.run_ok();
    h.state_mut().sets.as_mut().unwrap().unit = "ROB_2".into();
    let _ = h.run_ok();
    h.get_by_label("Add").click();
    let _ = h.run_ok();
    assert_eq!(ids(h.state())[2..], ["4002/ROB_2/J1", "4002/ROB_2/J2", "4002/ROB_2/J3", "4002/ROB_2/J4", "4002/ROB_2/J5", "4002/ROB_2/J6"]);
    assert_eq!(h.state().lanes(&[true; 8]).len(), 2);
    assert!(wait(&mut h, 5000, |a| a.session.status().channels.iter().filter(|c| c.key.signal == 4002).all(|c| c.samples > 20)));
    assert!(h.query_by_label("203.000").is_some(), "ROB_2 J3's torque (the fake's 203) is read");

    // Six resolver angles do not fit in the four free: Add does nothing, Replace does.
    h.get_by_label("Channel sets...").click();
    let _ = h.run_ok();
    h.get_by_label("Resolver angles").click();
    let _ = h.run_ok();
    assert!(egui_kittest::kittest::NodeT::accesskit_node(&h.get_by_label("Add")).is_disabled(), "greyed out, its hover saying why");
    h.get_by_label("Add").click();
    let _ = h.run_ok();
    assert_eq!(h.state().chans.len(), 8, "a set that does not fit is not half added");
    assert!(h.state().sets.is_some());
    h.get_by_label("Replace all 8 channels").click();
    let _ = h.run_ok();
    assert_eq!(ids(h.state()), (1..=6).map(|a| format!("5138/ROB_1/J{a}")).collect::<Vec<_>>());
    assert_eq!(h.state().lanes(&[true; 6]).len(), 6, "a chart each");
    assert!(wait(&mut h, 5000, |a| { let s = a.session.status(); s.channels.len() == 6 && s.channels.iter().all(|c| c.key.signal == 5138) }), "the session follows");

    // Already there: nothing to add.
    h.get_by_label("Channel sets...").click();
    let _ = h.run_ok();
    h.get_by_label("Resolver angles").click();
    let _ = h.run_ok();
    h.get_by_label("Add").click();
    let _ = h.run_ok();
    assert_eq!(h.state().chans.len(), 6);
    h.get_by_label("Cancel").click();
    let _ = h.run_ok();
    assert!(h.state().sets.is_none());

    // A replacement refused leaves the channels as they were.
    assert!(!h.state_mut().replace_channels(Vec::new(), false));
    assert_eq!(h.state().chans.len(), 6);
}

/// The first card's menu, then one of its entries.
fn card_menu(h: &mut Harness<'static, SpyApp>, card: usize, entry: &str) {
    h.get_all_by_label("⋯").nth(card).unwrap_or_else(|| panic!("no card {card}")).click();
    let _ = h.run_ok();
    h.get_by_label(entry).click();
    let _ = h.run_ok();
}

fn derived_health(h: &Harness<'static, SpyApp>, i: usize) -> view::Health {
    let st = h.state().session.status().clone();
    h.state().derived_health(i, &st)
}

#[test]
fn a_resolver_turns_onto_its_target_and_a_stale_one_is_never_on_target() {
    // 5138 at 1 rad (57.2958 deg).
    let mut b = Behaviour::default();
    b.signals.insert(5138, SignalDef { source: SignalSource::float(|_| 1.0), sample_ms: 4.032 });
    let fake = FakeController::start(b).unwrap();
    let mut h = harness(temp_dir("turn"), AskPolicy::Remote);
    connect(&mut h, &fake);
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming));
    add_via_dialog(&mut h, 5138, "Add");
    assert!(wait(&mut h, 5000, |a| a.session.status().channels.first().is_some_and(|c| c.samples > 20)));
    card_menu(&mut h, 0, "Turn to a target...");
    assert_eq!(h.state().derived.len(), 1);
    assert_eq!(derived_health(&h, 0), view::Health::Waiting, "no target yet");
    assert!(h.query_by_label("ON TARGET").is_none());

    let set = |h: &mut Harness<'static, SpyApp>, t: &str| {
        h.state_mut().derived[0].target_text = t.into();
        let _ = h.run_ok();
        h.get_by_label("Set").click();
        let _ = h.run_ok();
    };
    set(&mut h, "57.3");
    assert!(wait(&mut h, 3000, |a| a.derived[0].live.lock().len() > 10));
    let _ = h.run_ok();
    assert!(h.query_by_label("ON TARGET").is_some(), "0.004 deg from the target");
    set(&mut h, "60");
    assert!(wait(&mut h, 3000, |_| true));
    assert!(h.query_by_label("+2.704").is_some(), "60 - 57.296: turn it forward 2.7 deg");
    set(&mut h, "-300");
    let _ = h.run_ok();
    assert!(h.query_by_label("+2.704").is_some(), "-300 deg is 60 deg: the short way round");
    set(&mut h, "50°");
    let _ = h.run_ok();
    assert!(h.query_by_label("-7.296").is_some(), "a target behind the angle: turn it back");
    set(&mut h, "-300");
    set(&mut h, "fifty");
    assert!(h.state().derived[0].live.def() == &spy_core::derived::Derived::Turn { angle: h.state().chans[0].key.clone(), target_deg: Some(-300.0) }, "a word is refused, the target kept");

    // The stream stops: the turn goes stale, and ON TARGET is never shown for it,
    // on the card or on the phone.
    set(&mut h, "57.3");
    let _ = h.run_ok();
    assert!(h.query_by_label("ON TARGET").is_some());
    h.state_mut().settings.phone_port = 0;
    h.get_by_label("Phone view").click();
    assert!(wait(&mut h, 2000, |a| a.phone.is_some()));
    let phone = |h: &mut Harness<'static, SpyApp>| {
        std::thread::sleep(Duration::from_millis(300));
        let _ = h.run_ok();
        let body = h.state().phone_snapshot.lock().unwrap().body.clone();
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        json["channels"].as_array().unwrap().iter().find(|c| c["name"] == "Turn to target  ROB_1 J1").cloned().unwrap_or_else(|| panic!("no turn in {body}"))
    };
    let row = phone(&mut h);
    assert!(row["value"] == "ON TARGET" && row["stale"] == false, "{row}");
    fake.with(|b| b.freeze = true);
    assert!(wait(&mut h, 8000, |a| {
        let st = a.session.status().clone();
        a.derived_health(0, &st) != view::Health::Live
    }));
    let _ = h.run_ok();
    assert!(h.query_by_label("ON TARGET").is_none(), "a stale turn read ON TARGET");
    assert!(h.query_by_label("+0.004").is_some(), "the number is shown instead, dimmed");
    let row = phone(&mut h);
    assert!(row["value"] == "+0.004" && row["stale"] == true, "{row}");
    h.get_by_label("Phone view").click();
}

#[test]
fn a_duty_sum_brings_its_legs_and_a_sag_needs_a_steady_plateau() {
    let mut b = Behaviour::default();
    for (n, v) in [(5020u32, 0.5f32), (5021, 0.6), (5022, 0.4)] {
        b.signals.insert(n, SignalDef { source: SignalSource::float(move |_| v), sample_ms: 4.032 });
    }
    let fake = FakeController::start(b).unwrap();
    let dir = temp_dir("derived");
    let mut h = harness(dir.clone(), AskPolicy::Remote);
    connect(&mut h, &fake);
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming));
    add_via_dialog(&mut h, 5020, "Add");
    card_menu(&mut h, 0, "PWM duty sum of this axis");
    let ids: Vec<String> = h.state().chans.iter().map(|c| c.key.id()).collect();
    assert_eq!(ids, ["5020/ROB_1/J1", "5021/ROB_1/J1", "5022/ROB_1/J1"], "the other two legs came with it");
    assert!(wait(&mut h, 5000, |a| a.derived[0].live.lock().len() > 20));
    let _ = h.run_ok();
    assert!(h.query_by_label("1.50000").is_some(), "0.5 + 0.6 + 0.4");
    assert_eq!(derived_health(&h, 0), view::Health::Live);
    let d_lane = (h.state().derived[0].lane, String::new());
    assert!(h.state().lanes(&[true; 3]).contains(&d_lane), "the sum has a chart of its own");
    // Saved with what is in view, under an id that says what it is computed from.
    assert!(wait(&mut h, 2000, |a| a.view_ms.is_some()));
    h.get_by_label("Save CSV").click();
    let _ = h.run_ok();
    assert!(wait(&mut h, 5000, |a| a.export_job.is_none()));
    let csv = std::fs::read_to_string(&files_ending(&dir.join("recordings"), " view.csv")[0]).unwrap();
    let sums: Vec<f64> = csv.lines().filter(|r| r.contains(",duty-sum:ROB_1/J1,\"PWM duty sum ROB_1 J1\",\"\",")).map(|r| r.rsplit(',').next().unwrap().parse().unwrap()).collect();
    assert!(sums.len() > 20 && sums.iter().all(|v| (v - 1.5).abs() < 1e-6), "{} sums", sums.len());
    for f in files_ending(&dir.join("recordings"), " view.csv") {
        std::fs::remove_file(f).unwrap();
    }

    // The DC link's sag: no plateau from too short a history, or an unsteady one.
    add_via_dialog(&mut h, 5027, "Add");
    card_menu(&mut h, 3, "Sag below a plateau");
    assert_eq!(h.state().derived.len(), 2);
    h.get_by_label("Set the plateau").click();
    let _ = h.run_ok();
    assert!(!h.state().derived[1].live.def().is_set(), "set from a fraction of a second");
    fake.with(|b| b.signals.insert(5027, SignalDef { source: SignalSource::float(|(t, _, _)| if t / 4 % 2 == 0 { 350.0 } else { 360.0 }), sample_ms: 4.032 }));
    std::thread::sleep(Duration::from_millis(2300));
    let _ = h.run_ok();
    h.get_by_label("Set the plateau").click();
    let _ = h.run_ok();
    assert!(!h.state().derived[1].live.def().is_set(), "set from a DC link swinging 10 V");
    assert!(h.state().toasts.iter().any(|t| t.text.contains("not steady")), "and says why");
    fake.with(|b| b.signals.insert(5027, SignalDef { source: SignalSource::float(|_| 356.5), sample_ms: 4.032 }));
    std::thread::sleep(Duration::from_millis(2300));
    let _ = h.run_ok();
    h.get_by_label("Set the plateau").click();
    let _ = h.run_ok();
    assert_eq!(h.state().derived[1].live.def(), &spy_core::derived::Derived::Sag { link: h.state().chans[3].key.clone(), plateau_v: Some(356.5) });

    // A dip of 10 V, recorded; the review computes the sag again from the recording.
    h.get_by_label("● REC").click();
    assert!(wait(&mut h, 2000, |a| a.recorder.is_some()));
    fake.with(|b| b.signals.insert(5027, SignalDef { source: SignalSource::float(|_| 346.5), sample_ms: 4.032 }));
    assert!(wait(&mut h, 3000, |a| a.derived[1].live.lock().last().is_some_and(|(_, v)| v == 10.0)));
    let _ = h.run_ok();
    assert!(h.query_by_label("10.0000").is_some() && h.query_by_label("2.81% below").is_some());
    std::thread::sleep(Duration::from_millis(300));
    h.get_by_label_contains("■ STOP").click();
    assert!(wait(&mut h, 3000, |a| a.recorder.is_none()));
    let folder = std::fs::read_dir(dir.join("recordings")).unwrap().next().unwrap().unwrap().path();
    let meta = spy_core::recording::read_meta(&folder).unwrap();
    assert_eq!(meta.derived.len(), 2, "the definitions, not the values");
    let data = std::fs::read_to_string(folder.join("data.csv")).unwrap();
    assert!(!data.contains("sag") && !data.contains("duty"), "no derived values written as data");
    let review = spy_core::review::open(&folder, &[]).unwrap();
    let sag = review.channels.iter().find(|c| c.id == "sag:5027/ROB_1/J1").expect("the sag, computed again");
    assert!(sag.v.contains(&10.0), "{:?}", &sag.v[..sag.v.len().min(5)]);
    let sum = review.channels.iter().find(|c| c.id == "duty-sum:ROB_1/J1").unwrap();
    assert!(sum.v.len() > 50 && sum.v.iter().all(|v| (v - 1.5).abs() < 1e-6));

    // An input removed takes its derived channel with it.
    card_menu(&mut h, 1, "Remove");
    assert_eq!(h.state().derived.len(), 1);
    assert_eq!(h.state().derived[0].live.def().kind_name(), "DC-link sag");
}

#[test]
fn derived_channels_come_back_without_their_plateau() {
    let dir = temp_dir("derived-persist");
    {
        let mut h = harness(dir.clone(), AskPolicy::Remote);
        add_via_dialog(&mut h, 5138, "Add");
        add_via_dialog(&mut h, 5027, "Add");
        let k = h.state().chans[0].key.clone();
        let l = h.state().chans[1].key.clone();
        assert!(h.state_mut().add_derived(spy_core::derived::Derived::Turn { angle: k.clone(), target_deg: None }));
        assert!(h.state_mut().add_derived(spy_core::derived::Derived::Sag { link: l.clone(), plateau_v: None }));
        assert!(!h.state_mut().add_derived(spy_core::derived::Derived::Turn { angle: k.clone(), target_deg: Some(1.0) }), "the same one twice");
        h.state_mut().derived[0].live.set(spy_core::derived::Derived::Turn { angle: k, target_deg: Some(90.0) });
        h.state_mut().derived[1].live.set(spy_core::derived::Derived::Sag { link: l, plateau_v: Some(356.5) });
        h.state_mut().save_settings();
    }
    let file = std::fs::read_to_string(dir.join("settings.json")).unwrap();
    assert!(file.contains("\"target_deg\": 90.0") && !file.contains("356.5"), "no plateau in the file: {file}");
    let h = harness(dir, AskPolicy::Remote);
    let defs: Vec<spy_core::derived::Derived> = h.state().derived.iter().map(|d| d.live.def().clone()).collect();
    assert_eq!(defs.len(), 2);
    assert!(matches!(defs[0], spy_core::derived::Derived::Turn { target_deg: Some(t), .. } if t == 90.0), "a target is the person's: kept");
    assert!(matches!(defs[1], spy_core::derived::Derived::Sag { plateau_v: None, .. }), "a plateau was the controller's of the moment: not kept");
    assert_eq!(h.state().derived[0].target_text, "90.0000");
}

#[test]
fn the_browser_shows_named_signals_and_finds_any_number() {
    let mut h = harness(temp_dir("browse"), AskPolicy::Remote);
    let get = |h: &Harness<'static, SpyApp>, n: u32| h.state().catalogue.get(n).unwrap().clone();
    let (open, inert, named, strong) = (get(&h, 8000), get(&h, 1101), get(&h, 4002), get(&h, 1717));
    // D2: named only by default.
    assert!(h.state().visible(&named));
    assert!(!h.state().visible(&open));
    assert!(!h.state().visible(&inert));
    // A number typed in full finds its signal whatever the toggles say.
    h.state_mut().search = "8000".into();
    assert!(h.state().visible(&open));
    h.state_mut().search.clear();
    h.state_mut().settings.show_open = true;
    assert!(h.state().visible(&open));
    // The confidence filter.
    h.state_mut().min_confidence = Some(spy_core::catalogue::Confidence::Confirmed);
    assert!(h.state().visible(&named));
    assert!(!h.state().visible(&strong));
    assert!(!h.state().visible(&open));
    // And the panel draws with the filter set.
    let _ = h.run_ok();
}

#[test]
fn a_settings_file_without_lanes_gives_each_channel_its_own_chart() {
    // Every settings field has a default so a trimmed or older file still loads; a
    // missing lane must not put volts and degrees on one chart.
    let dir = temp_dir("lanes");
    std::fs::write(
        dir.join("settings.json"),
        r#"{"channels": [
            {"signal": 5027, "unit": "ROB_1", "axis": 1},
            {"signal": 6000, "unit": "ROB_1", "axis": 1},
            {"signal": 4002, "unit": "ROB_1", "axis": 2, "lane": 7}
        ]}"#,
    )
    .unwrap();
    let h = harness(dir, AskPolicy::Remote);
    let lanes: Vec<u32> = h.state().chans.iter().map(|c| c.lane).collect();
    assert_eq!(lanes.len(), 3);
    assert_eq!(lanes[2], 7, "a saved lane is kept");
    assert!(lanes[0] != lanes[1] && !lanes[..2].contains(&7) && !lanes[..2].contains(&0), "{lanes:?}");
    assert!(h.state().next_lane > *lanes.iter().max().unwrap(), "{lanes:?}, next {}", h.state().next_lane);
}

#[test]
fn a_padded_speed_and_a_wrapping_angle_read_true_on_the_card_and_the_phone() {
    // 6010 as measured: 0.5 rad/s reported with runs of padding zeros between (half
    // the samples), which the plain mean read as half the speed; and a resolver angle
    // dithering across 2pi-to-0, which the plain mean put half a turn away.
    let mut b = Behaviour::default();
    b.signals.insert(6010, SignalDef { source: SignalSource::float(|(t, _, _)| if [0, 1, 4, 5, 8].contains(&(t / 4 % 10)) { 0.5 } else { 0.0 }), sample_ms: 4.032 });
    b.signals.insert(5138, SignalDef { source: SignalSource::float(|(t, _, _)| if t / 4 % 2 == 0 { std::f32::consts::TAU - 0.002 } else { 0.001 }), sample_ms: 4.032 });
    let fake = FakeController::start(b).unwrap();
    let mut h = harness(temp_dir("readings"), AskPolicy::Remote);
    h.state_mut().settings.phone_port = 0;
    connect(&mut h, &fake);
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming));
    add_via_dialog(&mut h, 6010, "Add");
    add_via_dialog(&mut h, 5138, "Add");
    assert_eq!(h.state().chans.len(), 2);
    assert!(wait(&mut h, 5000, |a| a.session.status().channels.iter().all(|c| c.samples > 100)));
    // 0.5 rad/s is 28.6479 deg/s.
    assert!(h.query_by_label("28.6479").is_some(), "the card does not show the speed the signal reports");
    h.get_by_label("Phone view").click();
    assert!(wait(&mut h, 2000, |a| a.phone.is_some()));
    std::thread::sleep(Duration::from_millis(300));
    let _ = h.run_ok();
    let body = h.state().phone_snapshot.lock().unwrap().body.clone();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let value = |i: usize| json["channels"][i]["value"].as_str().unwrap().parse::<f64>().unwrap();
    assert!((value(0) - 28.6479).abs() < 1e-3, "{body}");
    let angle = value(1);
    assert!(!(0.5..=359.5).contains(&angle), "the resolver angle read {angle} deg: averaged across the wrap");
    h.get_by_label("Phone view").click();
}

#[test]
fn one_chart_never_mixes_two_units() {
    // Six joint angles overlaid in degrees; one switched to radians must get a chart
    // of its own, not share the degree axis (it drew near zero, and its hover said
    // "0.52 deg").
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let mut h = harness(temp_dir("units"), AskPolicy::Remote);
    connect(&mut h, &fake);
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming));
    add_via_dialog(&mut h, 6000, "Add the block 6000-6005");
    assert!(wait(&mut h, 5000, |a| a.session.status().channels.iter().all(|c| c.samples > 10)));
    let all = vec![true; 6];
    assert_eq!(h.state().lanes(&all).len(), 1);
    // The first card's unit button, as a person clicks it.
    assert_eq!(h.get_all_by_label("deg").count(), 6, "a unit button per card");
    h.get_all_by_label("deg").next().unwrap().click();
    let _ = h.run_ok();
    let toggled: Vec<bool> = h.state().chans.iter().map(|c| c.radians).collect();
    assert_eq!(toggled, vec![true, false, false, false, false, false], "the click switched J1 to radians");
    let lanes = h.state().lanes(&all);
    assert_eq!(lanes.iter().map(|l| l.1.as_str()).collect::<Vec<_>>(), vec!["rad", "deg"], "{lanes:?}");
    assert!(h.query_by_label("[rad]").is_some() && h.query_by_label("[deg]").is_some(), "each chart names its own unit");
}

#[test]
fn a_settings_file_that_puts_two_units_in_one_lane_gets_two_charts() {
    let dir = temp_dir("mixed");
    std::fs::write(
        dir.join("settings.json"),
        r#"{"channels": [
            {"signal": 5027, "unit": "ROB_1", "axis": 1, "lane": 3},
            {"signal": 6000, "unit": "ROB_1", "axis": 1, "lane": 3}
        ]}"#,
    )
    .unwrap();
    let h = harness(dir, AskPolicy::Remote);
    let lanes = h.state().lanes(&[true, true]);
    assert_eq!(lanes, vec![(3, "V".to_string()), (3, "deg".to_string())]);
}

#[test]
fn channels_come_back_next_time() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let dir = temp_dir("persist");
    {
        let mut h = harness(dir.clone(), AskPolicy::Remote);
        add_via_dialog(&mut h, 6000, "Add the block 6000-6005");
        assert_eq!(h.state().chans.len(), 6);
        h.state_mut().host_input = "127.0.0.1".into();
        h.state_mut().port_input = fake.port().to_string();
        h.state_mut().connect();
        let _ = h.run_ok();
        h.state_mut().shutdown();
    }
    let mut h = harness(dir, AskPolicy::Remote);
    assert_eq!(h.state().chans.len(), 6, "the channel set is restored");
    assert_eq!(h.state().port_input, fake.port().to_string(), "the last controller is filled in");
    h.get_by_label("Connect").click();
    assert!(wait(&mut h, 5000, |a| a.session.status().channels.iter().all(|c| c.samples > 5)));
}
