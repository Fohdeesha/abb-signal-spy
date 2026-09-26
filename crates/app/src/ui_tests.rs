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

use spy_core::fake::{Behaviour, FakeController};
use spy_core::session::{AskPolicy, Phase};

use crate::app::SpyApp;

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
    while Instant::now() < end {
        let _ = h.run_ok();
        if f(h.state()) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = h.run_ok();
    f(h.state())
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

    // Past twelve is refused with a message, never silently truncated.
    h.state_mut().add = None;
    add_via_dialog(&mut h, 4003, "Add all six axes");
    assert_eq!(h.state().chans.len(), 7, "six more would make 13");
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
        assert!(loaded.data["4002/ROB_1/1"].iter().all(|&(_, v)| v == 101.0));
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
