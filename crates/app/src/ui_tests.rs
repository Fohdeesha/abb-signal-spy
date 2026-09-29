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

use spy_core::discovery::VcFinder;
use spy_core::fake::{Behaviour, FakeController, SignalDef, SignalSource};
use spy_core::session::{AskPolicy, Options, Phase};

use crate::app::SpyApp;
use crate::view;

fn temp_dir(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("spy-ui-{tag}-{}-{}", std::process::id(), Instant::now().elapsed().as_nanos()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn harness(dir: PathBuf, ask: AskPolicy) -> Harness<'static, SpyApp> {
    // Never this PC's own virtual controllers.
    harness_with(dir, Options { ask, find_vc: VcFinder::none(), ..Options::default() })
}

fn harness_with(dir: PathBuf, opts: Options) -> Harness<'static, SpyApp> {
    Harness::builder().with_size((1400.0, 900.0)).with_max_steps(20).build_eframe(move |cc| {
        let mut app = SpyApp::with_options(cc, dir.clone(), false, opts.clone());
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
    assert!(h.query_all_by_label_contains("Disconnect and Connect again").next().is_some(), "the advice is not on screen");
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
    // about (decided 2026-09-26).
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

/// Hover a chart (the first by default) at `x` on its time axis, halfway up.
fn hover_chart(h: &mut Harness<'static, SpyApp>, lane: usize, x: f64) {
    let tr = h.state().lane_transforms[lane];
    let y = (tr.bounds().min()[1] + tr.bounds().max()[1]) / 2.0;
    h.hover_at(tr.position_from_point(&egui_plot::PlotPoint::new(x, y)));
    let _ = h.run_ok();
}

/// The charts' legend entries: egui_plot draws one checkbox per named item.
fn legend(h: &Harness<'static, SpyApp>) -> Vec<String> {
    use egui_kittest::kittest::NodeT;
    h.query_all_by(|n| n.role() == egui::accesskit::Role::CheckBox).filter_map(|n| n.accesskit_node().label()).collect()
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
    // A marker and the cursors are lines on the chart, not entries in its legend (on
    // the cell, a review's events covered half of every chart that way).
    let entries = legend(&h);
    assert!(entries.iter().any(|l| l.contains("Position")), "the channel's own entry: {entries:?}");
    assert!(!entries.iter().any(|l| l.contains("marker") || l.contains("cursor")), "{entries:?}");
    // Their text is on the hover instead: over the marker's line (paused, so that the
    // chart holds still under the pointer), it is named.
    h.key_press(egui::Key::Space);
    let _ = h.run_ok();
    let _ = h.run_ok();
    let tl = h.state().session.status().timeline.clone();
    let x = tl.seconds(h.state().markers[0].t_ms);
    hover_chart(&mut h, 0, x);
    let shown = h.state().hover_text.lock().unwrap().clone();
    assert!(shown.starts_with("marker M1\n"), "the marker's line not named on hover: {shown:?}");
}

#[test]
fn a_still_resolvers_dither_does_not_fill_its_chart_live_or_reviewed() {
    // The cell (2026-09-29): a still 5138 dithered over 0.022 deg and filled its chart.
    // Here 0.022 deg of dither; its chart must span the motor-side angles' 0.05 deg.
    let mut b = Behaviour::default();
    let dither = 0.011_f32.to_radians();
    b.signals.insert(5138, SignalDef { source: SignalSource::float(move |(t, _, _)| 1.7779 + if t / 4 % 2 == 0 { dither } else { -dither }), sample_ms: 4.032 });
    let fake = FakeController::start(b).unwrap();
    let dir = temp_dir("resolver-span");
    let mut h = harness(dir.clone(), AskPolicy::Remote);
    connect(&mut h, &fake);
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming));
    add_via_dialog(&mut h, 5138, "Add");
    assert!(wait(&mut h, 5000, |a| a.session.status().channels.first().is_some_and(|c| c.samples > 100)));
    let _ = h.run_ok();
    let span = |h: &Harness<'static, SpyApp>| {
        let b = h.state().lane_transforms[0].bounds();
        b.max()[1] - b.min()[1]
    };
    assert!(span(&h) >= 0.05, "live: a chart {} deg high for 0.022 deg of dither", span(&h));
    h.get_by_label("● REC").click();
    assert!(wait(&mut h, 2000, |a| a.recorder.is_some()));
    std::thread::sleep(Duration::from_millis(600));
    h.get_by_label_contains("■ STOP").click();
    assert!(wait(&mut h, 3000, |a| a.recorder.is_none()));
    let folder = std::fs::read_dir(dir.join("recordings")).unwrap().next().unwrap().unwrap().path();
    h.state_mut().open_recording(folder);
    assert!(wait(&mut h, 5000, |a| a.review.is_some()));
    let _ = h.run_ok();
    let _ = h.run_ok();
    assert!(span(&h) >= 0.05, "reviewed: a chart {} deg high for 0.022 deg of dither", span(&h));
}

#[test]
fn the_xy_windows_save_png_with_nothing_plotted_says_so() {
    // One channel charted: the XY window is open with nothing to plot, and its Save PNG
    // said "The XY plot is not open."
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let mut h = harness(temp_dir("xy-nothing"), AskPolicy::Remote);
    connect(&mut h, &fake);
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming));
    add_via_dialog(&mut h, 4001, "Add");
    assert!(wait(&mut h, 5000, |a| a.session.status().channels.iter().all(|c| c.samples > 50)));
    h.get_by_label("XY").click();
    let _ = h.run_ok();
    assert!(h.query_all_by_label_contains("Chart at least two channels").next().is_some());
    h.get_all_by_label("Save PNG").last().unwrap().click();
    let _ = h.run_ok();
    let said: Vec<&String> = h.state().toasts.iter().map(|t| &t.text).collect();
    assert!(said.iter().any(|t| t.contains("Nothing is plotted")) && !said.iter().any(|t| t.contains("not open")), "{said:?}");
}

#[test]
fn a_window_length_chosen_while_paused_is_shown() {
    // On the cell a person paused, then chose a longer window to find what had just
    // happened, and the charts did not change.
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let mut h = harness(temp_dir("paused-window"), AskPolicy::Remote);
    connect(&mut h, &fake);
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming));
    add_via_dialog(&mut h, 4000, "Add");
    assert!(wait(&mut h, 5000, |a| a.session.status().channels.first().is_some_and(|c| c.samples > 50)));
    h.key_press(egui::Key::Space);
    let _ = h.run_ok();
    let _ = h.run_ok();
    let (a0, b0) = h.state().view_ms.unwrap();
    assert!((b0 - a0 - 10_000).abs() < 50, "paused on the 10 s window: {a0} to {b0}");
    // Chosen through the Window list: 30 s, ending where the view ended.
    h.get_by(|n| n.role() == egui::accesskit::Role::ComboBox && n.value().as_deref() == Some("10 s")).click();
    let _ = h.run_ok();
    h.get_by_role_and_label(egui::accesskit::Role::Button, "30 s").click();
    let _ = h.run_ok();
    let _ = h.run_ok();
    assert_eq!(h.state().window_s, 30.0);
    let (a1, b1) = h.state().view_ms.unwrap();
    assert!((b1 - a1 - 30_000).abs() < 50 && (b1 - b0).abs() < 50, "still {a1} to {b1}, was {a0} to {b0}");
    assert!(h.state().paused_at.is_some(), "and still paused");
    // Scrolled back (dragged right), then 1 min: it ends where the scrolled view ended,
    // not where the pause began.
    let c = h.state().lane_transforms[0].frame().center();
    h.hover_at(c);
    h.drag_at(c);
    let _ = h.run_ok();
    h.hover_at(c + egui::vec2(120.0, 0.0));
    let _ = h.run_ok();
    h.drop_at(c + egui::vec2(120.0, 0.0));
    let _ = h.run_ok();
    let _ = h.run_ok();
    let (_, b2) = h.state().view_ms.unwrap();
    assert!(b2 < b1 - 2000, "the drag did not scroll back: {b2} against {b1}");
    h.get_by(|n| n.role() == egui::accesskit::Role::ComboBox && n.value().as_deref() == Some("30 s")).click();
    let _ = h.run_ok();
    h.get_by_role_and_label(egui::accesskit::Role::Button, "1 min").click();
    let _ = h.run_ok();
    let _ = h.run_ok();
    let (a3, b3) = h.state().view_ms.unwrap();
    assert!((b3 - a3 - 60_000).abs() < 50 && (b3 - b2).abs() < 50, "{a3} to {b3}, the scrolled view ended at {b2}");
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
fn a_restarted_virtual_controller_is_followed_and_the_recording_carries_on() {
    // A VC takes a new port at every start, a warm restart included: the session
    // follows the same controller there, and the window follows the session.
    let mut old = FakeController::start(Behaviour::default()).unwrap();
    let ports = std::sync::Arc::new(std::sync::Mutex::new(vec![old.port()]));
    let found = ports.clone();
    let finder = VcFinder::new(move |timeout| {
        found.lock().unwrap().iter().filter_map(|&p| spy_core::discovery::hello(std::net::SocketAddr::from(([127, 0, 0, 1], p)), timeout).ok().map(|a| (p, a.system_id))).collect()
    });
    let dir = temp_dir("vcmoved");
    let mut h = harness_with(dir.clone(), Options { find_vc: finder, ladder: vec![Duration::from_millis(100), Duration::from_millis(200)], ..Options::default() });
    connect(&mut h, &old);
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming));
    add_via_dialog(&mut h, 4002, "Add");
    assert!(wait(&mut h, 5000, |a| a.session.status().channels.first().is_some_and(|c| c.samples > 20)));
    h.get_by_label("● REC").click();
    assert!(wait(&mut h, 2000, |a| a.recorder.is_some()));
    let old_port = old.port();
    h.state_mut().settings.controllers.push(crate::settings::SavedController { name: "RobotStudio".into(), host: "127.0.0.1".into(), port: old_port });
    // Restarting: its port closes, and no VC port answers yet.
    old.stop();
    ports.lock().unwrap().clear();
    assert!(wait(&mut h, 3000, |a| phase(a) != Phase::Streaming));
    let before = h.state().session.status().channels[0].samples;
    let new = FakeController::start(Behaviour::default()).unwrap();
    ports.lock().unwrap().push(new.port());
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming && a.port_input == new.port().to_string()), "{:?} port {}", phase(h.state()), h.state().port_input);
    assert!(wait(&mut h, 5000, |a| a.session.status().channels[0].samples > before + 20));
    let a = h.state();
    assert!(a.recorder.is_some(), "the same controller: the recording carries on");
    assert_eq!(a.settings.last_target.as_ref().map(|t| t.port), Some(new.port()), "the next start goes to the new port");
    assert!(!a.settings.recent.iter().any(|t| t.port == old_port), "nothing listens on the old one any more");
    assert_eq!(a.settings.controllers[0].port, new.port(), "a saved entry for it follows too");
    assert_eq!(a.toasts.iter().filter(|t| t.text.contains(&format!("on port {}", new.port()))).count(), 1, "the person is told, once");

    // Connect again as it stands: the same controller, so nothing is finished.
    h.get_by_label("Disconnect").click();
    assert!(wait(&mut h, 3000, |a| phase(a) == Phase::Idle));
    h.get_by_label("Connect").click();
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming));
    assert!(h.state().recorder.is_some(), "the address followed, so Connect is not to another controller");
    let dir_rec = h.state_mut().recorder.take().unwrap().stop().dir;
    let loaded = spy_core::recording::read(&dir_rec).unwrap();
    assert!(loaded.meta.complete && !loaded.meta.notes.contains("Closed when"), "{:?}", loaded.meta.notes);
    assert!(loaded.meta.events.iter().any(|e| e.text.contains(&format!("connected to 127.0.0.1:{}", new.port()))), "{:?}", loaded.meta.events);
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
    assert_eq!(h.state().png_pending, Some(crate::export::Picture::Charts), "a screenshot was asked for");
    let image = std::sync::Arc::new(egui::ColorImage::filled([1400, 900], egui::Color32::DARK_GRAY));
    h.event(egui::Event::Screenshot { viewport_id: egui::ViewportId::ROOT, user_data: egui::UserData::default(), image });
    let _ = h.run_ok();
    assert_eq!(h.state().png_pending, None);
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

/// A recording folder as the recorder writes one (4002 J1 in Nm), in the harness's
/// recordings folder, for review.
fn recording_on_disk(dir: &std::path::Path, name: &str, kind: &str, label: &str, csv: &str) -> PathBuf {
    let d = dir.join("recordings").join(name);
    std::fs::create_dir_all(&d).unwrap();
    let (file, interval) = if kind == "slow" { ("slow.csv", r#", "interval_ms": 1000"#) } else { ("data.csv", "") };
    let json = format!(
        r#"{{"format": "abb-signal-spy-recording", "version": 2, "kind": "{kind}", "app": "t", "label": "{label}", "started_utc": "2026-09-27T10:00:00.000Z",
            "complete": true, "controller": "t", "channels": [{{"id": "4002/ROB_1/J1", "signal": 4002, "unit": "ROB_1", "axis": 1, "name": "Torque", "units": "Nm", "sample_ms": 4.0}}],
            "anchors": [{{"controller_ms": 1000, "utc": "2026-09-27T10:00:00.000Z", "row": 0}}], "rows_written": 0, "samples_lost": 0 {interval}}}"#
    );
    std::fs::write(d.join("recording.json"), json).unwrap();
    std::fs::write(d.join(file), csv).unwrap();
    d
}

fn full_csv() -> String {
    let mut csv = String::from("controller_ms,channel,value\n");
    for t in (1000..2000).step_by(4) {
        csv += &format!("{t},4002/ROB_1/J1,1\n");
    }
    csv
}

#[test]
fn a_reviewed_slow_log_gives_its_intervals_extremes_and_saves_them() {
    let dir = temp_dir("review-slow");
    let mut h = harness(dir.clone(), AskPolicy::Remote);
    let csv = "controller_ms,channel,count,mean,min,max\n1000,4002/ROB_1/J1,250,356.5,300,357.1\n2000,4002/ROB_1/J1,250,356.5,356,357\n3000,4002/ROB_1/J1,250,356.4,356,357\n";
    let folder = recording_on_disk(&dir, "slow", "slow", "", csv);
    h.state_mut().open_recording(folder);
    assert!(wait(&mut h, 5000, |a| a.review.is_some()));
    let r = h.state().review.as_ref().unwrap().review.clone();
    let s = crate::review_view::review_stats(&h.state().catalogue, r.channel("4002/ROB_1/J1").unwrap(), r.start, r.end + 1);
    assert_eq!((s.min, s.max), (300.0, 357.1), "the dip the slow log caught, contradicted by its statistics");
    h.get_by_label("Save CSV").click();
    let _ = h.run_ok();
    assert!(wait(&mut h, 5000, |a| a.export_job.is_none()));
    let text = std::fs::read_to_string(&files_ending(&dir.join("recordings"), " view.csv")[0]).unwrap();
    assert_eq!(text.lines().next(), Some("time_utc,t_s,channel,name,units,mean,min,max,count"));
    assert!(text.lines().any(|l| l.ends_with(",356.5,300,357.1,250")), "the interval's minimum lost from the file: {text}");
}

#[test]
fn a_reviewed_recordings_saves_carry_its_own_label() {
    let dir = temp_dir("review-label");
    let mut h = harness(dir.clone(), AskPolicy::Remote);
    h.state_mut().rec_label = "robot three".into();
    let folder = recording_on_disk(&dir, "one", "full", "robot one", &full_csv());
    h.state_mut().open_recording(folder);
    assert!(wait(&mut h, 5000, |a| a.review.is_some()));
    let _ = h.run_ok();
    h.get_by_label("Save CSV").click();
    let _ = h.run_ok();
    assert!(wait(&mut h, 5000, |a| a.export_job.is_none()));
    let saved = files_ending(&dir.join("recordings"), " view.csv");
    let name = saved[0].file_name().unwrap().to_string_lossy().to_string();
    assert!(name.contains("robot one") && !name.contains("robot three"), "robot one's data saved as {name}");
}

#[test]
fn keys_while_reviewing_leave_the_live_session_alone() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let dir = temp_dir("review-keys");
    let mut h = harness(dir.clone(), AskPolicy::Remote);
    connect(&mut h, &fake);
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming));
    start_recording(&mut h);
    let live = only_recording(&dir);
    let folder = recording_on_disk(&dir, "old", "full", "", &full_csv());
    h.state_mut().open_recording(folder);
    assert!(wait(&mut h, 5000, |a| a.review.is_some()));
    h.key_press(egui::Key::M);
    h.key_press(egui::Key::Space);
    let _ = h.run_ok();
    assert!(h.state().paused_at.is_none(), "the hidden live charts paused");
    h.get_by_label("Close the recording").click();
    let _ = h.run_ok();
    h.get_by_label_contains("■ STOP").click();
    assert!(wait(&mut h, 3000, |a| a.recorder.is_none()));
    assert!(spy_core::recording::read_meta(&live).unwrap().markers.is_empty(), "a marker put into the live recording from the review");
}

#[test]
fn review_statistics_are_computed_once_for_a_stretch() {
    let dir = temp_dir("review-stats-once");
    let mut h = harness(dir.clone(), AskPolicy::Remote);
    let folder = recording_on_disk(&dir, "one", "full", "", &full_csv());
    h.state_mut().open_recording(folder);
    assert!(wait(&mut h, 5000, |a| a.review.is_some()));
    for _ in 0..6 {
        let _ = h.run_ok();
    }
    let n = h.state().review.as_ref().unwrap().stats_computed;
    assert_eq!(n, 1, "the statistics of the same stretch computed {n} times (each a pass over every sample in view)");
}

#[test]
fn a_set_partly_there_already_still_ends_in_one_chart() {
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let mut h = harness(temp_dir("sets-partly"), AskPolicy::Remote);
    connect(&mut h, &fake);
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming));
    add_via_dialog(&mut h, 4002, "Add");
    add_via_dialog(&mut h, 6000, "Add");
    let keys: Vec<spy_core::store::ChannelKey> = (1..=6).map(|a| spy_core::store::ChannelKey { signal: 4002, unit: spy_core::request::MechUnit::new("ROB_1").unwrap(), axis: spy_core::request::Axis::new(a).unwrap() }).collect();
    // J2 there too, in a chart of its own.
    assert!(h.state_mut().add_channels(vec![keys[1].clone()], false));
    assert!(h.state_mut().add_channels(keys, true), "the other four torques");
    let lanes: std::collections::BTreeSet<u32> = h.state().chans.iter().filter(|c| c.key.signal == 4002).map(|c| c.lane).collect();
    assert_eq!(lanes.len(), 1, "six torques \"in one chart\" split over {} charts", lanes.len());
    let other = h.state().chans.iter().find(|c| c.key.signal == 6000).unwrap().lane;
    assert!(!lanes.contains(&other), "the joint angle pulled into the torques' chart");
}

#[test]
fn the_same_channel_twice_in_one_request_is_added_once() {
    let mut h = harness(temp_dir("add-twice"), AskPolicy::Remote);
    let k = spy_core::store::ChannelKey { signal: 5027, unit: spy_core::request::MechUnit::new("ROB_1").unwrap(), axis: spy_core::request::Axis::new(1).unwrap() };
    assert!(h.state_mut().add_channels(vec![k.clone(), k.clone()], true));
    assert_eq!(h.state().chans.len(), 1, "a settings file naming a unit twice added its DC link twice");
}

#[test]
fn streaming_with_nothing_arriving_does_not_read_as_streaming() {
    // Another program connected InfoStream first and gets every sample (measured 2026-09-28):
    // this one is set up, and nothing arrives. Seen on the VC as a green STREAMING
    // beside the advice.
    let fake = FakeController::start(Behaviour::default()).unwrap();
    let mut other = std::net::TcpStream::connect(("127.0.0.1", fake.port())).unwrap();
    use std::io::Write;
    other.write_all(&spy_core::request::Command::StreamConnect.frame(1, "127.0.0.1")).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let mut h = harness(temp_dir("not-receiving"), AskPolicy::Remote);
    connect(&mut h, &fake);
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming));
    add_via_dialog(&mut h, 4002, "Add");
    assert!(wait(&mut h, 8000, |a| a.session.status().advice.is_some()), "no advice with nothing arriving");
    let _ = h.run_ok();
    assert!(h.query_by_label("NOT RECEIVING").is_some(), "the status word");
    assert!(h.query_by_label("STREAMING").is_none(), "STREAMING shown while nothing arrives");
    drop(other);
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
        json["channels"].as_array().unwrap().iter().find(|c| c["name"] == "Turn to target  5138 ROB_1 J1").cloned().unwrap_or_else(|| panic!("no turn in {body}"))
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
    // A plateau compares its level with 2 s before, not 20: the history below is
    // seconds long.
    h.state_mut().plateau_trend_ms = 2000;
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
    // Over its plateau: above it, not "-0.28% below".
    fake.with(|b| b.signals.insert(5027, SignalDef { source: SignalSource::float(|_| 357.5), sample_ms: 4.032 }));
    assert!(wait(&mut h, 3000, |a| a.derived[1].live.lock().last().is_some_and(|(_, v)| v == -1.0)));
    std::thread::sleep(Duration::from_millis(200));
    let _ = h.run_ok();
    assert!(h.query_by_label("0.28% above").is_some(), "a link over its plateau not read as above it");
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
fn a_commutator_offset_target_is_the_controllers_it_came_from() {
    let mut b = Behaviour::default();
    b.signals.insert(5138, SignalDef { source: SignalSource::float(|_| 1.0), sample_ms: 4.032 });
    let (fake, rws, mut h, dir) = with_rws(b, "rws-target-owner");
    add_via_dialog(&mut h, 5138, "Add");
    let k = h.state().chans.iter().find(|c| c.key.signal == 5138).unwrap().key.clone();
    assert!(h.state_mut().add_derived(spy_core::derived::Derived::Turn { angle: k, target_deg: None }));
    log_in(&mut h, "robotics");
    assert!(wait(&mut h, 5000, |a| a.rws_ready()));
    h.get_by_label("Commutator offset").click();
    assert!(wait(&mut h, 3000, |a| a.derived[0].live.def().is_set()));
    // Not kept in the settings: it is this controller's, like a plateau.
    h.state_mut().save_settings();
    let saved: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("settings.json")).unwrap()).unwrap();
    assert!(saved["derived"][0]["def"]["target_deg"].is_null(), "a controller's commutator offset saved as a target: {}", saved["derived"]);
    // Another controller behind the address: the target goes, and the person is told.
    h.get_by_label("Disconnect").click();
    assert!(wait(&mut h, 3000, |a| phase(a) == Phase::Idle));
    fake.with(|b| b.system_id = "{0000000B-0000-4000-8000-00000000000B}".into());
    h.get_by_label("Connect").click();
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming));
    assert!(wait(&mut h, 3000, |a| !a.derived[0].live.def().is_set()), "another controller's commutator offset kept as the target");
    assert!(h.state().toasts.iter().any(|t| t.text.contains("commutator offset")), "not said");
    let said: Vec<String> = h.state().log.since(0).iter().filter(|e| e.text.contains("was cleared")).map(|e| format!("{:?} {}", e.level, e.text)).collect();
    assert!(said.len() == 1 && said[0].starts_with("Warn"), "said once, as a warning: {said:?}");
    // A target typed over one read from the controller is the person's, and stays.
    rws.with(|r| r.system_id = "{0000000B-0000-4000-8000-00000000000B}".into());
    log_in(&mut h, "robotics");
    assert!(wait(&mut h, 5000, |a| a.rws_ready()));
    h.get_by_label("Commutator offset").click();
    assert!(wait(&mut h, 3000, |a| a.derived[0].live.def().is_set()));
    h.state_mut().derived[0].target_text = "12".into();
    h.state_mut().set_target(0);
    h.get_by_label("Disconnect").click();
    assert!(wait(&mut h, 3000, |a| phase(a) == Phase::Idle));
    fake.with(|b| b.system_id = "{0000000C-0000-4000-8000-00000000000C}".into());
    h.get_by_label("Connect").click();
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming));
    let _ = h.run_ok();
    assert!(h.state().derived[0].live.def().is_set(), "a typed target dropped");
}

#[test]
fn a_plateau_of_no_voltage_is_refused() {
    let mut b = Behaviour::default();
    b.signals.insert(5027, SignalDef { source: SignalSource::float(|_| 0.0), sample_ms: 4.032 });
    let fake = FakeController::start(b).unwrap();
    let mut h = harness(temp_dir("plateau-zero"), AskPolicy::Remote);
    connect(&mut h, &fake);
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming));
    add_via_dialog(&mut h, 5027, "Add");
    card_menu(&mut h, 0, "Sag below a plateau");
    std::thread::sleep(Duration::from_millis(2300));
    let _ = h.run_ok();
    h.get_by_label("Set the plateau").click();
    let _ = h.run_ok();
    assert!(!h.state().derived[0].live.def().is_set(), "a plateau of 0 V: every sag after it is the whole link");
    assert!(h.state().toasts.iter().any(|t| t.text.contains("No plateau")), "not said");
    // Motors off, the link reads 16 V on the cell (s24): steady, and still no plateau.
    fake.with(|b| b.signals.insert(5027, SignalDef { source: SignalSource::float(|_| 16.0), sample_ms: 4.032 }));
    std::thread::sleep(Duration::from_millis(2300));
    let _ = h.run_ok();
    h.get_by_label("Set the plateau").click();
    let _ = h.run_ok();
    assert!(!h.state().derived[0].live.def().is_set(), "a motors-off plateau (16 V) taken: every sag after arming reads -360 V");
    assert!(h.state().toasts.iter().any(|t| t.text.contains("arm")), "not told to arm the robot");
}

/// A DC link that moves `per_s` volts a second from `from`, counted from its first
/// sample (the fake's clock does not start at 0).
fn ramp(from: f32, per_s: f32) -> SignalDef {
    use std::sync::atomic::{AtomicU64, Ordering};
    let start = std::sync::Arc::new(AtomicU64::new(u64::MAX));
    SignalDef {
        source: SignalSource::float(move |(t, _, _)| {
            let t0 = match start.compare_exchange(u64::MAX, t, Ordering::SeqCst, Ordering::SeqCst) {
                Ok(_) => t,
                Err(t0) => t0,
            };
            from + per_s * t.saturating_sub(t0) as f32 / 1000.0
        }),
        sample_ms: 4.032,
    }
}

#[test]
fn a_plateau_is_refused_while_the_link_drains_or_charges() {
    // The cell (2026-09-29): 16 s after motors off the link read 327 V, draining 2.6 %
    // every 10 s and steadier over two seconds than an armed link; a plateau was taken
    // from it. Here the comparison spans 3 s instead of 20, and the link drains 5 V a
    // second from 380 V: 4 % over the span, and over two seconds a standard deviation
    // of about 0.8 %, inside the steadiness rule (1 %). (3 s, not less: each step below
    // stays right with the PC up to about 2.7 s late, as a loaded one can be.)
    let mut b = Behaviour::default();
    b.signals.insert(5027, SignalDef { source: SignalSource::float(|(t, _, _)| if t / 4 % 2 == 0 { 379.0 } else { 381.0 }), sample_ms: 4.032 });
    let fake = FakeController::start(b).unwrap();
    let mut h = harness(temp_dir("plateau-trend"), AskPolicy::Remote);
    connect(&mut h, &fake);
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming));
    add_via_dialog(&mut h, 5027, "Add");
    card_menu(&mut h, 0, "Sag below a plateau");
    let set = |h: &mut Harness<'static, SpyApp>| {
        let _ = h.run_ok();
        h.get_by_label("Set the plateau").click();
        let _ = h.run_ok();
        h.state().derived[0].live.def().is_set()
    };
    // Two seconds of an armed link, but not the span before them: too short a history
    // (a span no history here reaches, whatever the load on the PC).
    h.state_mut().plateau_trend_ms = 60_000;
    std::thread::sleep(Duration::from_millis(2300));
    assert!(!set(&mut h), "set with no history to compare its level with");
    assert!(h.state().toasts.iter().any(|t| t.text.contains("No plateau yet") && t.text.contains("draining")), "not said why: {:?}", h.state().toasts.iter().map(|t| &t.text).collect::<Vec<_>>());
    h.state_mut().plateau_trend_ms = 3000;
    // Motors off: the link drains (the plateau's two seconds and the span's three).
    fake.with(|b| b.signals.insert(5027, ramp(380.0, -5.0)));
    std::thread::sleep(Duration::from_millis(5500));
    assert!(!set(&mut h), "a plateau taken from a draining link");
    assert!(h.state().toasts.iter().any(|t| t.text.contains("fell from") && t.text.contains("motors")), "not said: {:?}", h.state().toasts.iter().map(|t| &t.text).collect::<Vec<_>>());
    // Motors on: the link charges in a moment and then holds; two seconds after, the
    // span still reaches back into the charge.
    fake.with(|b| b.signals.insert(5027, SignalDef { source: SignalSource::float(|_| 16.0), sample_ms: 4.032 }));
    std::thread::sleep(Duration::from_millis(2500));
    fake.with(|b| b.signals.insert(5027, SignalDef { source: SignalSource::float(|(t, _, _)| if t / 4 % 2 == 0 { 379.0 } else { 381.0 }), sample_ms: 4.032 }));
    std::thread::sleep(Duration::from_millis(2300));
    assert!(!set(&mut h), "a plateau taken while the link was still coming up");
    assert!(h.state().toasts.iter().any(|t| t.text.contains("rose from")), "not said: {:?}", h.state().toasts.iter().map(|t| &t.text).collect::<Vec<_>>());
    // Held level over the whole span: taken.
    std::thread::sleep(Duration::from_millis(3500));
    assert!(set(&mut h), "an armed, steady link refused: {:?}", h.state().toasts.iter().map(|t| &t.text).collect::<Vec<_>>());
    assert!(matches!(h.state().derived[0].live.def(), spy_core::derived::Derived::Sag { plateau_v: Some(p), .. } if (p - 380.0).abs() < 0.5));
    // Said once in the log (on the cell every plateau was logged twice), and shown.
    let logged = h.state().log.since(0).iter().filter(|e| e.text.contains("plateau set to")).count();
    assert_eq!(logged, 1, "the plateau logged {logged} times");
    assert!(h.state().toasts.iter().any(|t| t.text.contains("plateau set to")), "and not shown");
}

#[test]
fn a_derived_setting_changed_while_recording_is_in_the_recording() {
    // A recording keeps the definitions, and a change of target or plateau while it
    // runs is an event in it, so that a review can say its values use the last one.
    let mut b = Behaviour::default();
    b.signals.insert(5138, SignalDef { source: SignalSource::float(|_| 1.0), sample_ms: 4.032 });
    let fake = FakeController::start(b).unwrap();
    let dir = temp_dir("derived-setting");
    let mut h = harness(dir.clone(), AskPolicy::Remote);
    connect(&mut h, &fake);
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming));
    add_via_dialog(&mut h, 5138, "Add");
    let k = h.state().chans[0].key.clone();
    assert!(h.state_mut().add_derived(spy_core::derived::Derived::Turn { angle: k, target_deg: None }));
    h.get_by_label("● REC").click();
    assert!(wait(&mut h, 2000, |a| a.recorder.is_some()));
    h.state_mut().derived[0].target_text = "12".into();
    h.get_by_label("Set").click();
    let _ = h.run_ok();
    std::thread::sleep(Duration::from_millis(300));
    h.get_by_label_contains("■ STOP").click();
    assert!(wait(&mut h, 3000, |a| a.recorder.is_none()));
    let folder = std::fs::read_dir(dir.join("recordings")).unwrap().next().unwrap().unwrap().path();
    let meta = spy_core::recording::read_meta(&folder).unwrap();
    let settings: Vec<&str> = meta.events.iter().filter(|e| e.kind == "derived-setting").map(|e| e.text.as_str()).collect();
    assert!(settings.iter().any(|t| t.contains("target set to 12")), "the change is not in the recording: {:?}", meta.events);
}

#[test]
fn a_plateau_goes_when_another_controller_streams_and_is_said_once() {
    let mut b = Behaviour::default();
    b.signals.insert(5027, SignalDef { source: SignalSource::float(|(t, _, _)| if t / 4 % 2 == 0 { 379.0 } else { 381.0 }), sample_ms: 4.032 });
    let fake = FakeController::start(b).unwrap();
    let mut h = harness(temp_dir("plateau-other"), AskPolicy::Remote);
    h.state_mut().plateau_trend_ms = 2000;
    connect(&mut h, &fake);
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming));
    add_via_dialog(&mut h, 5027, "Add");
    card_menu(&mut h, 0, "Sag below a plateau");
    std::thread::sleep(Duration::from_millis(4500));
    let _ = h.run_ok();
    h.get_by_label("Set the plateau").click();
    assert!(wait(&mut h, 2000, |a| a.derived[0].live.def().is_set()), "{:?}", h.state().toasts.iter().map(|t| &t.text).collect::<Vec<_>>());
    // Another controller behind the address: its link is not measured against this one's.
    h.get_by_label("Disconnect").click();
    assert!(wait(&mut h, 3000, |a| phase(a) == Phase::Idle));
    fake.with(|b| b.system_id = "{0000000B-0000-4000-8000-00000000000B}".into());
    h.get_by_label("Connect").click();
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming));
    assert!(wait(&mut h, 3000, |a| !a.derived[0].live.def().is_set()), "the other controller's plateau kept");
    let said: Vec<String> = h.state().log.since(0).iter().filter(|e| e.text.contains("was cleared")).map(|e| format!("{:?} {}", e.level, e.text)).collect();
    assert!(said.len() == 1 && said[0].starts_with("Warn"), "said once, as a warning: {said:?}");
}

#[test]
fn the_deepest_sag_counts_from_its_plateau_on() {
    let mut b = Behaviour::default();
    b.signals.insert(5027, SignalDef { source: SignalSource::float(|_| 16.0), sample_ms: 4.032 });
    let fake = FakeController::start(b).unwrap();
    let mut h = harness(temp_dir("plateau-deepest"), AskPolicy::Remote);
    h.state_mut().plateau_trend_ms = 2000;
    connect(&mut h, &fake);
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming));
    add_via_dialog(&mut h, 5027, "Add");
    card_menu(&mut h, 0, "Sag below a plateau");
    // Motors off (16 V) for a while, then on and steady for longer than the plateau's
    // comparison reaches back.
    std::thread::sleep(Duration::from_millis(800));
    fake.with(|b| b.signals.insert(5027, SignalDef { source: SignalSource::float(|_| 356.0), sample_ms: 4.032 }));
    std::thread::sleep(Duration::from_millis(4300));
    let _ = h.run_ok();
    h.get_by_label("Set the plateau").click();
    assert!(wait(&mut h, 2000, |a| a.derived[0].live.def().is_set() && a.derived[0].stats.n > 20));
    let deepest = h.state().derived[0].stats.max;
    assert!(deepest < 1.0, "deepest {deepest} V: the motors-off history before the plateau counted");
}

/// A fake controller and its RWS, one system, the app logged in to both.
fn with_rws(b: Behaviour, tag: &str) -> (FakeController, spy_core::fake_rws::FakeRws, Harness<'static, SpyApp>, PathBuf) {
    with_rws_as(b, spy_core::fake_rws::RwsBehaviour::default(), tag)
}

/// The same, with the RWS stand-in's behaviour given (its system id is the fake's).
fn with_rws_as(b: Behaviour, rb: spy_core::fake_rws::RwsBehaviour, tag: &str) -> (FakeController, spy_core::fake_rws::FakeRws, Harness<'static, SpyApp>, PathBuf) {
    let id = b.system_id.clone();
    let fake = FakeController::start(b).unwrap();
    let rws = spy_core::fake_rws::FakeRws::start(spy_core::fake_rws::RwsBehaviour { system_id: id, ..rb }).unwrap();
    let dir = temp_dir(tag);
    let mut h = harness(dir.clone(), AskPolicy::Remote);
    h.state_mut().settings.rws_port = rws.port();
    h.state_mut().rws_poll = Duration::from_millis(150);
    connect(&mut h, &fake);
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming));
    (fake, rws, h, dir)
}

fn log_in(h: &mut Harness<'static, SpyApp>, password: &str) {
    h.state_mut().show_rws = true;
    let _ = h.run_ok();
    h.state_mut().rws_form.password = password.into();
    h.get_by_label("Log in").click();
    let _ = h.run_ok();
}

#[test]
fn rws_names_the_controller_and_puts_its_events_on_the_charts_and_in_recordings() {
    let mut b = Behaviour::default();
    b.signals.insert(5138, SignalDef { source: SignalSource::float(|_| 1.0), sample_ms: 4.032 });
    let (_fake, rws, mut h, dir) = with_rws(b, "rws");
    add_via_dialog(&mut h, 4002, "Add");
    assert!(wait(&mut h, 5000, |a| a.session.status().channels.first().is_some_and(|c| c.samples > 20)));
    // An event from before the recording, which the first look after logging in reads:
    // well before it, past the slack whole-second stamps need (2 s) and their placing
    // error (a second).
    rws.push_event(10000, 1, "Before the recording");
    std::thread::sleep(Duration::from_millis(3300));
    h.get_by_label("● REC").click();
    assert!(wait(&mut h, 2000, |a| a.recorder.is_some()));
    log_in(&mut h, "robotics");
    assert!(wait(&mut h, 5000, |a| a.rws_ready() && a.controller_events.iter().any(|e| e.code == 10000)), "not logged in");
    assert!(h.query_by_label("IRB2600 · RobotWare 6.16.2027").is_some(), "the session line names the controller");
    assert!(h.query_by_label_contains("-4 h 00 min from UTC (its local time").is_some(), "its clock, from UTC");

    // Recorded, and on the charts at the time it happened.
    std::thread::sleep(Duration::from_millis(1100));
    rws.push_event(10010, 1, "Motors OFF state");
    rws.push_event(20205, 3, "Auto stop open");
    assert!(wait(&mut h, 5000, |a| a.controller_events.iter().any(|e| e.code == 20205)), "the events did not arrive");
    let newest = h.state().session.store().newest().unwrap();
    let tl = h.state().session.status().timeline.clone();
    let on = h.state().events_on_timeline(&tl);
    let (t, e) = on.iter().find(|(_, e)| e.code == 10010).unwrap();
    assert!((newest - t).abs() < 2500, "placed {} ms from now", newest - t);
    assert_eq!(e.color(), crate::theme::IDLE);
    assert!(on.iter().any(|(_, e)| e.code == 20205 && e.color() == crate::theme::BAD));
    // Drawn as lines, not piled into every chart's legend: looked at once the charts
    // show them (an event's second can place it just past the newest sample), and
    // recorded past them.
    let t_event = *t;
    assert!(wait(&mut h, 5000, |a| a.view_ms.is_some_and(|(from, to)| from <= t_event && t_event <= to)), "the event never came into view");
    let _ = h.run_ok();
    let entries = legend(&h);
    assert!(entries.iter().any(|l| l.contains("Torque")) && !entries.iter().any(|l| l.contains("Motors OFF") || l.contains("Auto stop")), "{entries:?}");
    std::thread::sleep(Duration::from_millis(1000));
    h.get_by_label_contains("■ STOP").click();
    assert!(wait(&mut h, 3000, |a| a.recorder.is_none()));
    let folder = std::fs::read_dir(dir.join("recordings")).unwrap().next().unwrap().unwrap().path();
    let meta = spy_core::recording::read_meta(&folder).unwrap();
    let kept: Vec<&str> = meta.events.iter().filter(|e| e.kind == "controller-event").map(|e| e.text.as_str()).collect();
    assert_eq!(kept, ["10010 Motors OFF state (information)", "20205 Auto stop open (error)"], "only what happened while recording");
    // Nor in a review of the recording.
    h.state_mut().open_recording(folder.clone());
    assert!(wait(&mut h, 5000, |a| a.review.is_some()));
    let _ = h.run_ok();
    let rs = h.state().review.as_ref().unwrap();
    let in_view = |m: &spy_core::review::ReviewMark| {
        let x = (m.t - rs.review.start) as f64 / 1000.0;
        rs.view.0 <= x && x <= rs.view.1
    };
    assert!(rs.review.marks.iter().any(|m| m.kind == "controller-event" && in_view(m)), "no event in the review's view: {:?}", rs.review.marks);
    let off = rs.review.marks.iter().find(|m| m.text.contains("Motors OFF")).map(|m| (m.t - rs.review.start) as f64 / 1000.0).unwrap();
    let entries = legend(&h);
    assert!(entries.iter().any(|l| l.contains("Torque")) && !entries.iter().any(|l| l.contains("Motors OFF") || l.contains("Auto stop") || l.contains("controller-event")), "{entries:?}");
    // A review's cursors are lines too.
    if let Some(rs) = h.state_mut().review.as_mut() {
        rs.cursors_on = true;
        rs.cursor_a = Some(off + 0.5);
        rs.cursor_b = Some(off + 1.0);
    }
    let _ = h.run_ok();
    let entries = legend(&h);
    assert!(!entries.iter().any(|l| l.contains("cursor")), "{entries:?}");
    // (The RWS window, open since the login, would sit between the pointer and the chart.)
    h.state_mut().show_rws = false;
    let _ = h.run_ok();
    hover_chart(&mut h, 0, off);
    let shown = h.state().hover_text.lock().unwrap().clone();
    assert!(shown.contains("controller-event: 10010 Motors OFF state"), "the event's line not named on hover in a review: {shown:?}");
    h.get_by_label("Close the recording").click();
    assert!(wait(&mut h, 3000, |a| a.review.is_none()));
    h.state_mut().show_rws = true;
    let _ = h.run_ok();

    // Switched off: no more looks.
    h.get_by_label("Event log on the charts and in recordings (a look every 5 s)").click();
    let _ = h.run_ok();
    assert!(!h.state().settings.rws_events);
    let asked = rws.requests().len();
    rws.push_event(10011, 1, "Motors ON state");
    std::thread::sleep(Duration::from_millis(600));
    let _ = h.run_ok();
    assert!(!h.state().controller_events.iter().any(|e| e.code == 10011));
    assert_eq!(rws.requests().len(), asked, "nothing asked while off");

    // A turn's target from the motor's commutator offset.
    add_via_dialog(&mut h, 5138, "Add");
    let k = h.state().chans.iter().find(|c| c.key.signal == 5138).unwrap().key.clone();
    assert!(h.state_mut().add_derived(spy_core::derived::Derived::Turn { angle: k, target_deg: None }));
    let _ = h.run_ok();
    h.get_by_label("Commutator offset").click();
    assert!(wait(&mut h, 3000, |a| a.derived[0].live.def().is_set()));
    let spy_core::derived::Derived::Turn { target_deg: Some(t), .. } = h.state().derived[0].live.def().clone() else { panic!() };
    assert!((t - 1.5707999f64.to_degrees()).abs() < 1e-3, "{t}");
    assert!(rws.requests().iter().any(|(_, p)| p.starts_with("/rw/cfg/MOC/MOTOR_CALIB/instances/rob1_1")), "ROB_1 J1's motor");

    assert!(rws.requests().iter().all(|(m, _)| m == "GET"), "read-only: {:?}", rws.requests());
    assert_eq!(rws.logins(), 1);
    h.state_mut().save_settings();
    let saved = std::fs::read_to_string(&h.state().settings_path).unwrap();
    assert!(!saved.contains("robotics") && !saved.contains("Default User"), "the login reached the settings file");
    assert!(saved.contains(&format!("\"rws_port\": {}", rws.port())));
    h.get_by_label("Log out").click();
    let _ = h.run_ok();
    assert!(h.state().rws.is_none());
    assert!(h.state().rws_form.password.is_empty(), "the password is not kept past the session");
}

#[test]
fn rws_is_refused_for_another_controller_or_a_wrong_login_and_ends_with_the_connection() {
    let (fake, rws, mut h, _dir) = with_rws(Behaviour::default(), "rws-refused");
    log_in(&mut h, "wrong");
    assert!(wait(&mut h, 5000, |a| a.rws.is_none()));
    assert!(h.state().toasts.iter().any(|t| t.text.contains("refused the login")), "said why");

    rws.with(|b| b.system_id = "{11111111-2222-3333-4444-555555555555}".into());
    log_in(&mut h, "robotics");
    assert!(wait(&mut h, 5000, |a| a.rws.is_none()));
    assert!(h.state().toasts.iter().any(|t| t.text.contains("different controller")), "a second VC's RWS on another port");

    rws.with(|b| b.system_id = fake.with(|f| f.system_id.clone()));
    log_in(&mut h, "robotics");
    assert!(wait(&mut h, 5000, |a| a.rws_ready()));
    h.get_by_label("Disconnect").click();
    assert!(wait(&mut h, 5000, |a| a.rws.is_none()), "RWS outlived the connection");
}

#[test]
fn rws_stops_when_another_controller_answers_after_a_new_login() {
    let (_fake, rws, mut h, _dir) = with_rws(Behaviour::default(), "rws-other");
    log_in(&mut h, "robotics");
    assert!(wait(&mut h, 5000, |a| a.rws_ready()));
    // A laptop's cable moved to another IRC5 (every one's service port is 192.168.125.1)
    // before InfoStream's reconnect has seen it; the same default login works there.
    rws.replace_controller("{22222222-2222-4222-8222-222222222222}");
    rws.push_event(20205, 3, "the other controller's");
    assert!(wait(&mut h, 5000, |a| a.rws.is_none()), "RWS carried on with another controller");
    assert!(h.state().toasts.iter().any(|t| t.text.contains("different controller") && t.text.contains("{22222222")), "said why");
    assert!(!h.state().controller_events.iter().any(|e| e.code == 20205), "the other controller's event was shown");
}

#[test]
fn a_commutator_offset_from_another_controller_is_not_taken_and_rws_stops() {
    let mut b = Behaviour::default();
    b.signals.insert(5138, SignalDef { source: SignalSource::float(|_| 1.0), sample_ms: 4.032 });
    let (_fake, rws, mut h, _dir) = with_rws(b, "rws-other-calib");
    add_via_dialog(&mut h, 5138, "Add");
    let k = h.state().chans.iter().find(|c| c.key.signal == 5138).unwrap().key.clone();
    assert!(h.state_mut().add_derived(spy_core::derived::Derived::Turn { angle: k, target_deg: None }));
    // Looks far apart: the clock is read every twelfth, well after this test ends, so
    // only the commutator offset's own read can find the other controller.
    h.state_mut().rws_poll = Duration::from_secs(3);
    log_in(&mut h, "robotics");
    assert!(wait(&mut h, 5000, |a| a.rws_ready()));
    // The event log off: the commutator offset is the only thing read.
    h.get_by_label("Event log on the charts and in recordings (a look every 5 s)").click();
    let _ = h.run_ok();
    rws.replace_controller("{22222222-2222-4222-8222-222222222222}");
    h.get_by_label("Commutator offset").click();
    assert!(wait(&mut h, 5000, |a| a.rws.is_none()), "RWS carried on with another controller");
    assert!(h.state().toasts.iter().any(|t| t.text.contains("different controller")), "said why");
    assert!(!h.state().derived[0].live.def().is_set(), "another controller's offset taken as the target");
}

#[test]
fn rws_stops_and_says_so_after_an_internal_error() {
    let (_fake, _rws, mut h, _dir) = with_rws(Behaviour::default(), "rws-crash");
    log_in(&mut h, "robotics");
    assert!(wait(&mut h, 5000, |a| a.rws_ready()));
    h.state().crash_rws_for_test();
    assert!(wait(&mut h, 5000, |a| a.rws.is_none()), "a dead RWS thread still shown logged in");
    assert!(h.state().toasts.iter().any(|t| t.text.contains("internal error")), "said why");
}

fn log_texts(h: &Harness<'static, SpyApp>) -> Vec<String> {
    h.state().log.since(0).into_iter().map(|e| e.text).collect()
}

/// The controller events a recording folder's recording.json kept.
fn kept_events(folder: &std::path::Path) -> Vec<String> {
    spy_core::recording::read_meta(folder).map(|m| m.events.into_iter().filter(|e| e.kind == "controller-event").map(|e| e.text).collect()).unwrap_or_default()
}

fn only_recording(dir: &std::path::Path) -> PathBuf {
    std::fs::read_dir(dir.join("recordings")).unwrap().next().unwrap().unwrap().path()
}

/// REC with one channel streaming.
fn start_recording(h: &mut Harness<'static, SpyApp>) {
    add_via_dialog(h, 4002, "Add");
    assert!(wait(h, 5000, |a| a.session.status().channels.first().is_some_and(|c| c.samples > 20)));
    h.get_by_label("● REC").click();
    assert!(wait(h, 2000, |a| a.recorder.is_some()));
}

#[test]
fn a_login_again_during_a_recording_files_no_event_twice_and_reads_back_what_it_missed() {
    let (_fake, rws, mut h, dir) = with_rws(Behaviour::default(), "rws-again");
    start_recording(&mut h);
    log_in(&mut h, "robotics");
    assert!(wait(&mut h, 5000, |a| a.rws_ready()));
    rws.push_event(10010, 1, "first");
    assert!(wait(&mut h, 5000, |a| a.controller_events.iter().any(|e| e.code == 10010)));
    // Out and in again with two events meanwhile: the first comes back in the newest page.
    h.get_by_label("Log out").click();
    let _ = h.run_ok();
    rws.push_event(20000, 1, "meanwhile");
    rws.push_event(20001, 1, "meanwhile");
    log_in(&mut h, "robotics");
    assert!(wait(&mut h, 5000, |a| a.controller_events.iter().any(|e| e.code == 20001)));
    // Again, with more than a page meanwhile: all of them, not just the newest page.
    h.get_by_label("Log out").click();
    let _ = h.run_ok();
    for i in 0..12 {
        rws.push_event(30000 + i, 1, "meanwhile");
    }
    log_in(&mut h, "robotics");
    assert!(wait(&mut h, 5000, |a| a.controller_events.iter().any(|e| e.code == 30011)));
    std::thread::sleep(Duration::from_millis(400));
    let _ = h.run_ok();
    let codes: Vec<u32> = h.state().controller_events.iter().map(|e| e.code).collect();
    let want: Vec<u32> = [10010, 20000, 20001].into_iter().chain(30000..30012).collect();
    assert_eq!(codes, want, "each event once, none missed");
    h.get_by_label_contains("■ STOP").click();
    assert!(wait(&mut h, 3000, |a| a.recorder.is_none()));
    let kept = kept_events(&only_recording(&dir));
    for code in &want {
        assert_eq!(kept.iter().filter(|t| t.starts_with(&format!("{code} "))).count(), 1, "{code} in the recording: {kept:?}");
    }
}

#[test]
fn an_unreadable_controller_event_is_said_and_the_rest_arrive() {
    let (_fake, rws, mut h, _dir) = with_rws(Behaviour::default(), "rws-unreadable");
    log_in(&mut h, "robotics");
    assert!(wait(&mut h, 5000, |a| a.rws_ready()));
    rws.with(|b| {
        let id = b.events.last().map_or(1000, |e| e.id + 1);
        b.events.push(spy_core::fake_rws::FakeEvent { id, code: 66666, severity: 3, time: 1_200_000_000_000_000, title: "stamped in year 38 million".into() });
    });
    rws.push_event(10010, 1, "readable");
    assert!(wait(&mut h, 5000, |a| a.controller_events.iter().any(|e| e.code == 10010)));
    assert!(h.state().rws_ready());
    assert!(log_texts(&h).iter().any(|t| t.contains("could not be read")), "the unreadable entry is not said");
}

#[test]
fn the_clock_offset_follows_a_change_of_the_controllers_clock() {
    let (_fake, rws, mut h, _dir) = with_rws(Behaviour::default(), "rws-clock");
    log_in(&mut h, "robotics");
    assert!(wait(&mut h, 5000, |a| a.rws_ready()));
    // Its clock set an hour on (a DST change, or someone correcting it): the window
    // follows within a look, and events after it are placed through it.
    rws.with(|b| b.clock_offset_s += 3600);
    assert!(wait(&mut h, 3000, |a| a.rws.as_ref().is_some_and(|l| (l.offset_ms + 3 * 3_600_000).abs() < 2000)), "the offset did not follow the clock");
    assert!(wait(&mut h, 1000, |_| true) && h.query_by_label_contains("-3 h 00 min from UTC (its local time").is_some(), "the window still shows the old offset");
    rws.push_event(10010, 1, "after the change");
    assert!(wait(&mut h, 5000, |a| a.controller_events.iter().any(|e| e.code == 10010)));
    let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
    let e = h.state().controller_events.iter().find(|e| e.code == 10010).unwrap().clone();
    assert!((now_ms - e.utc_ms).abs() < 3000, "placed {} s from when it happened", (now_ms - e.utc_ms) / 1000);
}

#[test]
fn an_event_logged_as_the_clock_is_set_mid_look_is_placed_through_the_new_clock() {
    // The clock set on between a look's clock read and its events read, and an event
    // logged on the new clock: placed through the old offset it would be an hour off
    // (and a recording would take it for one from before its start).
    let (_fake, rws, mut h, _dir) = with_rws(Behaviour::default(), "rws-clock-mid-look");
    log_in(&mut h, "robotics");
    assert!(wait(&mut h, 5000, |a| a.rws_ready()));
    rws.with(|b| b.clock_step_at_events = 3600);
    assert!(wait(&mut h, 5000, |a| a.controller_events.iter().any(|e| e.code == 99998)));
    let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
    let e = h.state().controller_events.iter().find(|e| e.code == 99998).unwrap().clone();
    assert!((now_ms - e.utc_ms).abs() < 3000, "placed {} s from when it happened", (now_ms - e.utc_ms) / 1000);
}

#[test]
fn with_the_event_log_off_a_refused_login_still_ends_rws() {
    let (_fake, rws, mut h, _dir) = with_rws(Behaviour::default(), "rws-off-refused");
    log_in(&mut h, "robotics");
    assert!(wait(&mut h, 5000, |a| a.rws_ready()));
    h.get_by_label("Event log on the charts and in recordings (a look every 5 s)").click();
    let _ = h.run_ok();
    // The password changed on the controller, and the session timed out.
    rws.with(|b| b.password = "changed".into());
    rws.expire_sessions();
    assert!(wait(&mut h, 6000, |a| a.rws.is_none()), "shown logged in though the controller refuses the login");
    assert!(h.state().toasts.iter().any(|t| t.text.contains("refused the login")));
}

#[test]
fn rws_log_lines_carry_no_login_and_say_when_an_event_happened() {
    let (_fake, rws, mut h, _dir) = with_rws(Behaviour::default(), "rws-log");
    log_in(&mut h, "robotics");
    assert!(wait(&mut h, 5000, |a| a.rws_ready()));
    assert!(!log_texts(&h).iter().any(|t| t.contains("Default User")), "the user name reached the log file");
    rws.push_event(20205, 3, "Auto stop open");
    assert!(wait(&mut h, 5000, |a| a.controller_events.iter().any(|e| e.code == 20205)));
    let e = h.state().controller_events.iter().find(|e| e.code == 20205).unwrap().clone();
    let at = std::time::UNIX_EPOCH + Duration::from_millis(e.utc_ms as u64);
    let (_, _, _, hh, mm, ss, _) = spy_core::util::local_parts(at).unwrap();
    let line = log_texts(&h).into_iter().find(|t| t.contains("20205")).expect("an error event is logged");
    assert!(line.contains(&format!("{hh:02}:{mm:02}:{ss:02}")), "the log does not say when it happened: {line}");
}

#[test]
fn the_rws_window_shows_the_login_while_infostream_reconnects() {
    let (mut fake, _rws, mut h, _dir) = with_rws(Behaviour::default(), "rws-reconnecting");
    log_in(&mut h, "robotics");
    assert!(wait(&mut h, 5000, |a| a.rws_ready()));
    fake.stop();
    assert!(wait(&mut h, 5000, |a| matches!(phase(a), Phase::Reconnecting { .. })));
    assert!(h.state().rws.is_some(), "kept while InfoStream reconnects to the same address");
    assert!(h.query_by_label("Log out").is_some(), "the window hides a login still in use");
    assert!(h.query_by_label_contains("Connect to the controller first").is_none());
}

#[test]
fn rws_does_without_the_controllers_identity() {
    let rb = spy_core::fake_rws::RwsBehaviour { identity: false, ..Default::default() };
    let (_fake, _rws, mut h, _dir) = with_rws_as(Behaviour::default(), rb, "rws-no-identity");
    log_in(&mut h, "robotics");
    assert!(wait(&mut h, 5000, |a| a.rws_ready()), "a display name made RWS unusable");
    assert!(h.query_by_label("IRB2600 · RobotWare 6.16.2027").is_some());
}

#[test]
fn the_rws_password_goes_when_the_connection_ends() {
    let (_fake, _rws, mut h, _dir) = with_rws(Behaviour::default(), "rws-password");
    log_in(&mut h, "robotics");
    assert!(wait(&mut h, 5000, |a| a.rws_ready()));
    h.get_by_label("Disconnect").click();
    assert!(wait(&mut h, 5000, |a| a.rws.is_none()));
    assert!(h.state().rws_form.password.is_empty(), "the password outlived the session");
}

#[test]
fn events_already_on_their_way_when_the_session_ends_are_kept() {
    let (_fake, rws, mut h, _dir) = with_rws(Behaviour::default(), "rws-drain");
    log_in(&mut h, "robotics");
    assert!(wait(&mut h, 5000, |a| a.rws_ready()));
    rws.push_event(10010, 1, "on its way");
    // The RWS thread sends it while the window draws nothing; then the session ends.
    std::thread::sleep(Duration::from_millis(600));
    h.state().session.crash_for_test();
    let end = Instant::now() + Duration::from_secs(5);
    while !matches!(h.state().session.status().phase, Phase::Stopped { .. }) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = h.run_ok();
    assert!(h.state().rws.is_none());
    assert!(h.state().controller_events.iter().any(|e| e.code == 10010), "an event already received was dropped with the link");
}

#[test]
fn controller_events_that_arrive_after_stop_reach_the_recording() {
    let (_fake, rws, mut h, dir) = with_rws(Behaviour::default(), "rws-late");
    start_recording(&mut h);
    // Looks far apart: an event logged just before STOP arrives after it.
    h.state_mut().rws_poll = Duration::from_secs(4);
    log_in(&mut h, "robotics");
    assert!(wait(&mut h, 5000, |a| a.rws_ready()));
    std::thread::sleep(Duration::from_millis(300));
    rws.push_event(50204, 3, "Motion supervision");
    std::thread::sleep(Duration::from_millis(200));
    h.get_by_label_contains("■ STOP").click();
    assert!(wait(&mut h, 3000, |a| a.recorder.is_none()));
    let folder = only_recording(&dir);
    assert!(wait(&mut h, 1500, |_| kept_events(&folder).iter().any(|t| t.starts_with("50204 "))), "the trip's controller event missed its recording: {:?}", kept_events(&folder));
}

#[test]
fn save_last_keeps_the_controller_events_of_its_stretch() {
    let (_fake, rws, mut h, _dir) = with_rws(Behaviour::default(), "rws-save-last");
    add_via_dialog(&mut h, 4002, "Add");
    assert!(wait(&mut h, 5000, |a| a.session.status().channels.first().is_some_and(|c| c.samples > 20)));
    log_in(&mut h, "robotics");
    assert!(wait(&mut h, 5000, |a| a.rws_ready()));
    rws.push_event(20205, 3, "Auto stop open");
    assert!(wait(&mut h, 5000, |a| a.controller_events.iter().any(|e| e.code == 20205)));
    h.get_by_label("Save last").click();
    assert!(wait(&mut h, 5000, |a| a.snapshot_job.is_none() && a.last_folder.is_some()));
    let folder = h.state().last_folder.clone().unwrap();
    assert!(wait(&mut h, 1500, |_| kept_events(&folder).iter().any(|t| t.starts_with("20205 "))), "{:?}", kept_events(&folder));
}

#[test]
fn the_browser_shows_named_signals_and_finds_any_number() {
    let mut h = harness(temp_dir("browse"), AskPolicy::Remote);
    let get = |h: &Harness<'static, SpyApp>, n: u32| h.state().catalogue.get(n).unwrap().clone();
    let (open, inert, named, strong) = (get(&h, 8000), get(&h, 1101), get(&h, 4002), get(&h, 1717));
    // Named only by default.
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

/// One look of a minimised window, as eframe takes it: `logic` alone, no frame drawn.
/// What it asked of the window (a new title, say) comes back.
fn look_minimised(h: &mut Harness<'static, SpyApp>) -> Vec<egui::ViewportCommand> {
    let ctx = h.ctx.clone();
    let mut frame = eframe::Frame::_new_kittest();
    let out = ctx.run_logic(&egui::RawInput::default(), |ctx| eframe::App::logic(h.state_mut(), ctx, &mut frame));
    out.viewport_commands.into_values().flatten().collect()
}

/// Minimised: a look every 100 ms (eframe's pace for a hidden window) until `f` holds
/// or `ms` pass. The titles set on the way are added to `titles`.
fn minimised_until(h: &mut Harness<'static, SpyApp>, ms: u64, titles: &mut Vec<String>, mut f: impl FnMut(&SpyApp) -> bool) -> bool {
    let end = Instant::now() + Duration::from_millis(ms);
    loop {
        for c in look_minimised(h) {
            if let egui::ViewportCommand::Title(t) = c {
                titles.push(t);
            }
        }
        if f(h.state()) {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The phone view's snapshot: how old it is, and what it says.
fn phone_snapshot(a: &SpyApp) -> (Duration, serde_json::Value) {
    let g = a.phone_snapshot.lock().unwrap();
    (g.built.map_or(Duration::MAX, |b| b.elapsed()), serde_json::from_str(&g.body).unwrap_or_default())
}

#[test]
fn a_minimised_window_keeps_the_phone_the_turn_and_the_title_current() {
    // Minimised, eframe draws no frame: it runs the upkeep alone (`logic`). Measured
    // on the VC with the upkeep in `ui`: the phone read NOT CURRENT a second after
    // minimising, for as long as the window stayed so, and the taskbar title, there
    // to say whether it is live and recording, stopped following the session.
    let turning = || {
        let mut b = Behaviour::default();
        b.signals.insert(5138, SignalDef { source: SignalSource::float(|_| 1.0), sample_ms: 4.032 });
        b
    };
    let mut old = FakeController::start(turning()).unwrap();
    let ports = std::sync::Arc::new(std::sync::Mutex::new(vec![old.port()]));
    let found = ports.clone();
    let finder = VcFinder::new(move |timeout| {
        found.lock().unwrap().iter().filter_map(|&p| spy_core::discovery::hello(std::net::SocketAddr::from(([127, 0, 0, 1], p)), timeout).ok().map(|a| (p, a.system_id))).collect()
    });
    let mut h = harness_with(temp_dir("minimised"), Options { find_vc: finder, ladder: vec![Duration::from_millis(100), Duration::from_millis(200)], ..Options::default() });
    h.state_mut().settings.phone_port = 0;
    connect(&mut h, &old);
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming));
    add_via_dialog(&mut h, 5138, "Add");
    assert!(wait(&mut h, 5000, |a| a.session.status().channels.first().is_some_and(|c| c.samples > 20)));
    card_menu(&mut h, 0, "Turn to a target...");
    h.state_mut().derived[0].target_text = "57.3".into();
    let _ = h.run_ok();
    h.get_by_label("Set").click();
    h.get_by_label("Phone view").click();
    h.get_by_label("● REC").click();
    assert!(wait(&mut h, 3000, |a| a.phone.is_some() && a.recorder.is_some()));
    let turn = |a: &SpyApp| {
        let (age, json) = phone_snapshot(a);
        let row = json["channels"].as_array().and_then(|c| c.iter().find(|c| c["name"] == "Turn to target  5138 ROB_1 J1").cloned()).unwrap_or_default();
        (age, json["controller"].as_str().unwrap_or_default().to_string(), row)
    };

    // Minimised from here on: not one frame drawn.
    let mut titles = Vec::new();
    let counted = h.state().chans[0].stats.n;
    minimised_until(&mut h, 1500, &mut titles, |_| false);
    let (age, _, row) = turn(h.state());
    assert!(age < Duration::from_millis(500), "the phone's snapshot is {age:?} old: the phone says NOT CURRENT");
    assert!(row["value"] == "ON TARGET" && row["stale"] == false, "{row}");
    assert!(h.state().chans[0].stats.n > counted + 100, "the statistics since reset stopped counting");

    // The virtual controller restarts, on another port, while nobody looks.
    old.stop();
    ports.lock().unwrap().clear();
    assert!(minimised_until(&mut h, 3000, &mut titles, |a| phase(a) != Phase::Streaming));
    let new = FakeController::start(turning()).unwrap();
    ports.lock().unwrap().push(new.port());
    let there = format!("127.0.0.1:{}", new.port());
    assert!(
        minimised_until(&mut h, 8000, &mut titles, |a| {
            let (age, controller, row) = turn(a);
            a.port_input == new.port().to_string() && age < Duration::from_millis(500) && controller == there && row["value"] == "ON TARGET" && row["stale"] == false
        }),
        "the address {}, the phone {:?}",
        h.state().port_input,
        turn(h.state())
    );
    assert!(titles.iter().any(|t| t.contains("RECONNECTING")), "{titles:?}");
    assert_eq!(titles.last().map(String::as_str), Some(format!("ABB Signal Spy · STREAMING {there} · REC").as_str()), "the taskbar's title");
    // Saved meanwhile: a PC shut down with the window minimised starts on the new port.
    let path = h.state().settings_path.clone();
    let saved = |_: &SpyApp| std::fs::read_to_string(&path).is_ok_and(|s| s.contains(&format!("\"port\": {}", new.port())));
    assert!(minimised_until(&mut h, 4000, &mut titles, saved), "the settings were not saved while minimised");
    h.state_mut().phone = None;
}

#[test]
fn a_recording_that_closes_while_minimised_is_said_when_the_window_is_shown() {
    let (fake, rws, mut h, dir) = with_rws(Behaviour::default(), "minimised-rec");
    add_via_dialog(&mut h, 4002, "Add");
    assert!(wait(&mut h, 5000, |a| a.session.status().channels.first().is_some_and(|c| c.samples > 20)));
    log_in(&mut h, "robotics");
    assert!(wait(&mut h, 5000, |a| a.rws_ready()));
    h.get_by_label("● REC").click();
    assert!(wait(&mut h, 2000, |a| a.recorder.is_some()));

    // Minimised through a long run: the controller's events are filed as they come.
    let mut titles = Vec::new();
    rws.push_event(10010, 1, "Motors OFF state");
    let rec = std::fs::read_dir(dir.join("recordings")).unwrap().next().unwrap().unwrap().path();
    let filed = |_: &SpyApp| spy_core::recording::read_meta(&rec).is_ok_and(|m| m.events.iter().any(|e| e.text.starts_with("10010 ")));
    assert!(minimised_until(&mut h, 5000, &mut titles, filed), "a controller event was not recorded while minimised");

    // And the cable moves to the next robot (every IRC5's service port is
    // 192.168.125.1): the automatic reconnect reaches it.
    fake.with(|b| b.system_id = "{0000000B-0000-4000-8000-00000000000B}".into());
    fake.drop_connections();
    assert!(minimised_until(&mut h, 8000, &mut titles, |a| a.recorder.is_none() && matches!(phase(a), Phase::Stopped { .. })), "{:?}", phase(h.state()));
    let title = titles.last().cloned().unwrap_or_default();
    assert!(title.contains("STOPPED") && !title.contains("REC"), "the taskbar still says it records: {titles:?}");
    assert!(h.state().rws.is_none() && h.state().rws_form.password.is_empty(), "RWS and its password outlived the session");

    // Longer than a toast stays up, then shown: what was said meanwhile is there (as
    // a toast; the log pane has it too, after the time).
    minimised_until(&mut h, 6500, &mut titles, |_| false);
    let _ = h.run_ok();
    let said = h.state().log.since(0).into_iter().find(|e| e.text.starts_with("Recording closed")).expect("not said at all").text;
    assert!(h.query_by_label(&said).is_some(), "said while minimised, and gone unseen");

    // Nothing else asks for a look now: the upkeep asks for its own, or it stops.
    look_minimised(&mut h);
    look_minimised(&mut h);
    assert!(h.ctx.has_requested_repaint(), "a minimised window's upkeep stops for good when nothing else wakes it");
}

#[test]
fn toasts_left_unseen_are_kept_to_the_newest_few() {
    let mut h = harness(temp_dir("toasts"), AskPolicy::Remote);
    for i in 0..30 {
        h.state_mut().toast(spy_core::log::Level::Info, format!("said {i}"));
    }
    let _ = h.run_ok();
    assert!(h.query_by_label("said 29").is_some() && h.query_by_label("said 22").is_some());
    assert!(h.query_by_label("said 21").is_none(), "{} toasts on screen", h.state().toasts.len());
    assert!(h.state().log.since(0).iter().any(|e| e.text == "said 0"), "the older ones are in the log");
}

/// A speed on every tick (4001), the same at the 24 ms group's ticks (318), a torque
/// from it (4002 = 2 x 4001 + 3), an angle in radians (1298), and a zero-filled joint
/// speed (6010, as measured): pairs with a known line.
fn xy_signals() -> Behaviour {
    fn speed(t: u64) -> f32 {
        10.0 * (t as f32 / 300.0).sin()
    }
    let mut b = padded_speed();
    b.signals.insert(4001, SignalDef { source: SignalSource::float(|(t, _, _)| speed(t)), sample_ms: 4.032 });
    b.signals.insert(4002, SignalDef { source: SignalSource::float(|(t, _, _)| 2.0 * speed(t) + 3.0), sample_ms: 4.032 });
    b.signals.insert(318, SignalDef { source: SignalSource::float(|(t, _, _)| speed(t)), sample_ms: 24.192 });
    b.signals.insert(1298, SignalDef { source: SignalSource::float(|(t, _, _)| (t % 1000) as f32 * 0.001), sample_ms: 4.032 });
    b
}

/// The XY plot's last pairs, and the channels they are of.
fn xy_pairs(a: &SpyApp) -> Option<(&crate::xy::Key, &crate::xy::Pairs)> {
    a.xy.as_ref()?.cache.as_ref().map(|(k, p)| (k, p))
}

/// Every pair on the line `y = 2 x + 3` (the fake's values are single precision).
fn on_the_line(p: &crate::xy::Pairs) -> bool {
    p.points.iter().all(|&(_, x, y)| (y - (2.0 * x + 3.0)).abs() < 1e-4)
}

#[test]
fn the_xy_plot_pairs_two_channels_tick_by_tick_over_the_stretch_in_view() {
    let fake = FakeController::start(xy_signals()).unwrap();
    let mut h = harness(temp_dir("xy"), AskPolicy::Remote);
    connect(&mut h, &fake);
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming));
    for n in [4001, 4002, 318, 1298, 6010] {
        add_via_dialog(&mut h, n, "Add");
    }
    assert!(wait(&mut h, 5000, |a| a.session.status().channels.len() == 5 && a.session.status().channels.iter().all(|c| c.samples > 40)));
    h.get_by_label("XY").click();
    // The first two charted, until the person chooses: torque against speed, a pair
    // on every tick, every one on the line.
    assert!(wait(&mut h, 3000, |a| xy_pairs(a).is_some_and(|(k, p)| k.y == "4002/ROB_1/J1" && p.points.len() > 200)));
    let (_, p) = xy_pairs(h.state()).unwrap();
    assert!(on_the_line(p), "a pair of two different ticks");
    let f = p.fit.unwrap();
    assert!((f.slope - 2.0).abs() < 1e-5 && (f.offset - 3.0).abs() < 1e-4 && f.r.unwrap() > 0.99999, "{f:?}");
    assert!(h.query_all_by_label_contains("r = 1.0000").next().is_some(), "the correlation is not shown");
    assert!(h.query_all_by_label_contains("line: Y = 2.00000 × X + 3.00000 Nm").next().is_some(), "the line is not shown");

    // Its own zoom: dragged, it keeps the person's view and says how to get the
    // whole stretch back; a double-click does, and so do other channels.
    let zoomed = |h: &Harness<'static, SpyApp>| h.state().xy.as_ref().unwrap().zoomed;
    let drag = |h: &mut Harness<'static, SpyApp>| {
        let c = h.state().xy_rect.unwrap().center();
        h.hover_at(c);
        h.drag_at(c);
        let _ = h.run_ok();
        h.hover_at(c + egui::vec2(60.0, 30.0));
        let _ = h.run_ok();
        h.drop_at(c + egui::vec2(60.0, 30.0));
        let _ = h.run_ok();
        let _ = h.run_ok();
    };
    assert!(!zoomed(&h));
    let shown = |h: &Harness<'static, SpyApp>| h.state().xy.as_ref().unwrap().shown.unwrap();
    let whole = shown(&h);
    drag(&mut h);
    assert!(zoomed(&h), "a drag did not move the plot");
    // Dragged right and down, the view moves left and up, and the points are thinned
    // for it (for the whole stretch, a view zoomed in would be drawn sparse).
    let (moved, width) = (shown(&h), whole.0.1 - whole.0.0);
    assert!(whole.0.0 - moved.0.0 > 0.05 * width && moved.1.0 > whole.1.0, "the points are thinned for {whole:?}, not the view {moved:?}");
    assert!(h.query_by_label("Zoomed: double-click the plot for the whole stretch.").is_some());
    // A double-click: the harness takes a quarter second a frame, too slow for one.
    let c = h.state().xy_rect.unwrap().center();
    h.hover_at(c);
    h.step();
    let t0 = h.ctx.input(|i| i.time);
    for (k, pressed) in [true, false, true, false].into_iter().enumerate() {
        h.input_mut().time = Some(t0 + 0.05 * (k + 1) as f64);
        h.event(egui::Event::PointerButton { pos: c, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
        h.step();
    }
    let _ = h.run_ok();
    assert!(!zoomed(&h), "a double-click did not bring back the whole stretch");
    drag(&mut h);
    assert!(zoomed(&h));

    // The 24 ms group against a signal on every tick: a pair at each of its samples,
    // with the sample of that same tick (the two are one quantity: y = x exactly).
    h.state_mut().xy.as_mut().unwrap().y = Some("318/ROB_1/J1".into());
    assert!(wait(&mut h, 3000, |a| xy_pairs(a).is_some_and(|(k, p)| k.y == "318/ROB_1/J1" && p.points.len() > 20)));
    assert!(!zoomed(&h), "other channels kept the last ones' view");
    let (_, p) = xy_pairs(h.state()).unwrap();
    assert!(p.points.iter().all(|&(_, x, y)| x == y), "paired with a neighbouring tick");
    assert!(p.points.len() + 1 >= p.counts.1 && p.points.len() * 5 < p.counts.0, "{} pairs of {:?} samples", p.points.len(), p.counts);

    // In the unit the charts show: radians as degrees.
    h.state_mut().xy.as_mut().unwrap().x = Some("1298/ROB_1/J1".into());
    assert!(wait(&mut h, 3000, |a| xy_pairs(a).is_some_and(|(k, p)| k.x == "1298/ROB_1/J1" && !p.points.is_empty())));
    let (_, p) = xy_pairs(h.state()).unwrap();
    let most = p.points.iter().map(|q| q.1).fold(f64::MIN, f64::max);
    assert!(most > 50.0 && most < 57.3, "an angle of up to 1 rad reads {most} at most");
    // A zero-filled speed as the chart reads it: its padding is the speed, not a
    // stop (only zeros before its first value in the history are zeros).
    h.state_mut().xy.as_mut().unwrap().x = Some("6010/ROB_1/J1".into());
    assert!(wait(&mut h, 3000, |a| xy_pairs(a).is_some_and(|(k, p)| k.x == "6010/ROB_1/J1" && p.points.len() > 100)));
    let (_, p) = xy_pairs(h.state()).unwrap();
    let zeros = p.points.iter().filter(|q| q.1 == 0.0).count();
    assert!(zeros <= 3 && p.points.iter().all(|q| q.1 == 0.0 || (q.1 - 28.64788975654116).abs() < 1e-9), "{zeros} padding zeros plotted as a stop");

    // Nothing arriving: said, the pairs kept as the last received.
    fake.with(|b| b.freeze = true);
    assert!(wait(&mut h, 5000, |a| a.chans.iter().all(|c| a.session.status().channels.iter().any(|s| s.key == c.key && s.stale))), "the channels did not go stale");
    let _ = h.run_ok();
    assert!(h.query_all_by_label_contains("the plot shows the last pairs received").next().is_some(), "a stale plot is not said to be");
}

#[test]
fn compare_ranks_the_other_channels_by_how_closely_it_follows_a_line_of_each() {
    // The open-signal explorer: an unknown against the rulers charted beside it. Here 4002 is
    // 2 x 4001 + 3 on every tick, and 2 x 318 + 3 on the 24 ms group's ticks; 1298 is a
    // sawtooth, 6010 a zero-filled constant.
    let fake = FakeController::start(xy_signals()).unwrap();
    let mut h = harness(temp_dir("compare"), AskPolicy::Remote);
    connect(&mut h, &fake);
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming));
    for n in [4001, 4002, 318, 1298, 6010] {
        add_via_dialog(&mut h, n, "Add");
    }
    assert!(wait(&mut h, 5000, |a| a.session.status().channels.len() == 5 && a.session.status().channels.iter().all(|c| c.samples > 100)));
    // From the channel's own menu, as a person does.
    h.get_all_by_label("⋯").nth(1).unwrap().click();
    let _ = h.run_ok();
    h.get_by_label("Compare with the other channels").click();
    let _ = h.run_ok();
    fn result(a: &SpyApp) -> Option<&crate::compare::Compared> {
        a.compare.as_ref().and_then(|c| c.result.as_ref())
    }
    assert!(wait(&mut h, 3000, |a| result(a).is_some()), "nothing compared");
    let res = result(h.state()).unwrap();
    assert_eq!(res.subject, "4002/ROB_1/J1");
    let ids: Vec<String> = res.rows.iter().map(|r| r.id.clone()).collect();
    assert_eq!(ids.len(), 4, "{ids:?}");
    let top: std::collections::BTreeSet<&str> = ids[..2].iter().map(String::as_str).collect();
    assert_eq!(top, ["318/ROB_1/J1", "4001/ROB_1/J1"].into(), "the two it is a line of come first: {ids:?}");
    let row = |id: &str| res.rows.iter().find(|r| r.id == id).unwrap();
    let f = row("4001/ROB_1/J1").fit.unwrap();
    assert!((f.slope - 2.0).abs() < 1e-5 && (f.offset - 3.0).abs() < 1e-4 && row("4001/ROB_1/J1").r().unwrap() > 0.99999, "the compared channel as a line of the other: {f:?}");
    assert!(row("318/ROB_1/J1").r().unwrap() > 0.99999 && row("318/ROB_1/J1").pairs * 5 < row("4001/ROB_1/J1").pairs, "318 pairs only on its own ticks");
    assert!(h.query_all_by_label_contains("r = 1.0000").next().is_some(), "the correlation is not shown");
    let header = format!("{} =", res.subject_title);
    assert!(h.query_by_label(&header).is_some(), "the line's header does not name the channel compared ({header})");
    assert!(h.query_all_by_label("2.00000 × this + 3.00000 Nm").count() == 2, "the line is not shown");
    // One click: the pair in the XY plot, the channel compared on Y.
    h.get_all_by_label("Show in XY").next().unwrap().click();
    let _ = h.run_ok();
    let first = ids[0].clone();
    let xy = h.state().xy.as_ref().expect("the XY plot did not open");
    assert_eq!((xy.x.as_deref(), xy.y.as_deref()), (Some(first.as_str()), Some("4002/ROB_1/J1")));
    assert!(wait(&mut h, 3000, |a| xy_pairs(a).is_some_and(|(k, p)| k.x == first && k.y == "4002/ROB_1/J1" && !p.points.is_empty())));

    // From the catalogue: a charted signal's details offer it.
    h.state_mut().compare = None;
    h.state_mut().xy = None;
    h.state_mut().selected = Some(1298);
    let _ = h.run_ok();
    h.get_by_label("Compare with the charted channels").click();
    let _ = h.run_ok();
    assert!(wait(&mut h, 3000, |a| result(a).is_some_and(|r| r.subject == "1298/ROB_1/J1" && r.rows.len() == 4)));
    // Once, not every frame: the stretch moves on live, the comparison stays until asked.
    let to = result(h.state()).unwrap().to;
    std::thread::sleep(Duration::from_millis(400));
    let _ = h.run_ok();
    assert_eq!(result(h.state()).unwrap().to, to, "compared again unasked");
    h.get_by_label("Compare again").click();
    assert!(wait(&mut h, 3000, |a| result(a).is_some_and(|r| r.to > to)), "Compare again did not compare the stretch now in view");
    // Not for a signal that is not charted.
    h.state_mut().selected = Some(4003);
    let _ = h.run_ok();
    assert!(h.query_by_label("Compare with the charted channels").is_none());
    // Nor with nothing else charted.
    h.state_mut().compare = None;
    h.state_mut().chans.retain(|c| c.key.signal == 1298);
    h.state_mut().sync_channels();
    h.state_mut().selected = Some(1298);
    let _ = h.run_ok();
    assert!(h.query_by_label("Compare with the charted channels").is_none(), "offered with nothing to compare with");
    h.get_all_by_label("⋯").next().unwrap().click();
    let _ = h.run_ok();
    assert!(h.query_by_label("Compare with the other channels").is_none(), "offered with nothing to compare with");
}

/// Click into a text field (by its label) and type, as a person does.
fn type_into(h: &mut Harness<'static, SpyApp>, role: egui::accesskit::Role, label: &str, text: &str) {
    h.get_by_role_and_label(role, label).click();
    let _ = h.run_ok();
    h.get_by_role_and_label(role, label).type_text(text);
    let _ = h.run_ok();
}

#[test]
fn your_notes_on_a_signal_are_kept_shown_and_there_next_time() {
    use egui::accesskit::Role;
    let dir = temp_dir("notes");
    let mut h = harness(dir.clone(), AskPolicy::Remote);
    h.state_mut().settings.show_open = true;
    h.state_mut().selected = Some(1403);
    let _ = h.run_ok();
    assert!(h.query_by_label("None yet.").is_some());
    h.get_by_label("Add your notes...").click();
    let _ = h.run_ok();
    // Typed, as a person does.
    type_into(&mut h, Role::TextInput, "Name", "wrist configuration vector");
    type_into(&mut h, Role::MultilineTextInput, "Evidence", "follows joint 5 at rest");
    h.get_by_label("probable").click();
    let _ = h.run_ok();
    h.get_by_label("Save").click();
    let _ = h.run_ok();
    assert!(h.state().note_edit.is_none(), "the editor stays open after saving");
    let file = dir.join(crate::notes::FILE);
    let saved = std::fs::read_to_string(&file).unwrap();
    assert!(saved.contains("\"wrist configuration vector\"") && saved.contains("\"follows joint 5 at rest\"") && saved.contains("\"probable\""), "{saved}");
    // In the details beneath the catalogue's own, and marked in the list.
    let _ = h.run_ok();
    assert!(h.query_by_label("wrist configuration vector").is_some(), "the name is not shown");
    assert!(h.query_by_label("Evidence: follows joint 5 at rest").is_some(), "the evidence is not shown");
    // The list draws the rows in view: looked for, as a person would.
    assert!(h.query_by_label("✎ notes").is_none());
    h.state_mut().search = "1403".into();
    let _ = h.run_ok();
    assert!(h.query_by_label("✎ notes").is_some(), "the list does not mark it");

    // Next time: the same notes.
    drop(h);
    let mut h = harness(dir.clone(), AskPolicy::Remote);
    h.state_mut().settings.show_open = true;
    h.state_mut().selected = Some(1403);
    let _ = h.run_ok();
    assert!(h.query_by_label("wrist configuration vector").is_some(), "the notes did not come back");
    // The editor holds them; Cancel keeps nothing typed; Delete removes them.
    h.get_by_label("Edit your notes...").click();
    let _ = h.run_ok();
    assert!(h.query_by_label("Your notes on signal 1403").is_some(), "an unnamed signal's title");
    assert_eq!(h.get_by_role_and_label(Role::TextInput, "Name").value().as_deref(), Some("wrist configuration vector"));
    type_into(&mut h, Role::TextInput, "Name", " (maybe)");
    assert!(h.state().note_edit.as_ref().is_some_and(|e| e.draft.name.contains("(maybe)")), "the typing did not reach the editor");
    h.get_by_label("Cancel").click();
    let _ = h.run_ok();
    assert!(h.state().note_edit.is_none());
    assert_eq!(h.state().notes.get(1403).unwrap().name, "wrist configuration vector", "Cancel kept what was typed");
    assert!(!std::fs::read_to_string(&file).unwrap().contains("(maybe)"));
    h.get_by_label("Edit your notes...").click();
    let _ = h.run_ok();
    h.get_by_label("Delete these notes").click();
    let _ = h.run_ok();
    assert!(h.state().notes.get(1403).is_none());
    assert!(!std::fs::read_to_string(&file).unwrap().contains("wrist"), "deleted from the file too");
    // A number the catalogue does not have takes notes too.
    h.state_mut().selected = Some(99_999);
    let _ = h.run_ok();
    assert!(h.query_by_label("Add your notes...").is_some());
}

#[test]
fn your_notes_export_in_the_catalogues_columns_and_never_with_an_address() {
    let (_fake, _rws, mut h, dir) = with_rws(Behaviour::default(), "notes-export");
    let export = |h: &mut Harness<'static, SpyApp>| {
        h.get_by_label("Catalogue").click();
        let _ = h.run_ok();
        h.get_by_label("Export your notes...").click();
        let _ = h.run_ok();
    };
    let exported = |dir: &std::path::Path| -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(dir.join("recordings")).map(|r| r.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.to_string_lossy().ends_with("signal-notes.tsv")).collect()).unwrap_or_default();
        v.sort();
        v
    };
    let note = |name: &str, evidence: &str| crate::notes::Note { name: name.into(), evidence: evidence.into(), ..Default::default() };
    h.state_mut().notes.put(1403, note("a guess", "follows joint 5")).unwrap();
    h.state_mut().notes.put(6914, note("", "seen at the cell, 192.0.2.77, at rest")).unwrap();
    // An address in a note: not exported, and the person told where it is.
    export(&mut h);
    assert!(exported(&dir).is_empty(), "exported with an address in it");
    assert!(h.state().toasts.iter().any(|t| t.text.contains("6914 (evidence): \"192.0.2.77\"")), "{:?}", h.state().toasts.iter().map(|t| &t.text).collect::<Vec<_>>());
    // Taken out: exported, saying RobotWare is not known (not logged in yet).
    h.state_mut().notes.put(6914, note("", "seen at the cell, at rest")).unwrap();
    export(&mut h);
    let files = exported(&dir);
    assert_eq!(files.len(), 1);
    let text = std::fs::read_to_string(&files[0]).unwrap();
    assert!(text.contains("# RobotWare: not known") && text.contains(&format!("by ABB Signal Spy {}.", env!("CARGO_PKG_VERSION"))), "{text}");
    assert!(text.contains("\n1403\ta guess\t") && text.contains("\n6914\t\t\t\t\t\t\topen\tseen at the cell, at rest\t"), "{text}");
    // Logged in to RWS: its RobotWare version, and still nothing else of the controller.
    log_in(&mut h, "robotics");
    assert!(wait(&mut h, 5000, |a| a.rws_ready()), "not logged in");
    export(&mut h);
    let files = exported(&dir);
    assert_eq!(files.len(), 2, "{files:?}");
    let text = std::fs::read_to_string(&files[1]).unwrap();
    assert!(text.contains("# RobotWare 6.16.2027 (read from the controller's RWS)."), "{text}");
    let system_id = spy_core::fake::SYSTEM_ID.trim_matches(|c| c == '{' || c == '}');
    for private in ["127.0.0.1", system_id, "IRB2600"] {
        assert!(!text.contains(private), "the export names {private}: {text}");
    }
}

#[test]
fn the_xy_plot_takes_a_recording_under_review_and_its_stretch_in_view() {
    let fake = FakeController::start(xy_signals()).unwrap();
    let dir = temp_dir("xy-review");
    let mut h = harness(dir.clone(), AskPolicy::Remote);
    connect(&mut h, &fake);
    assert!(wait(&mut h, 5000, |a| phase(a) == Phase::Streaming));
    add_via_dialog(&mut h, 4001, "Add");
    add_via_dialog(&mut h, 4002, "Add");
    assert!(wait(&mut h, 5000, |a| a.session.status().channels.iter().all(|c| c.samples > 20)));
    h.get_by_label("● REC").click();
    assert!(wait(&mut h, 2000, |a| a.recorder.is_some()));
    std::thread::sleep(Duration::from_millis(1500));
    h.get_by_label_contains("■ STOP").click();
    assert!(wait(&mut h, 3000, |a| a.recorder.is_none()));
    let rec = std::fs::read_dir(dir.join("recordings")).unwrap().next().unwrap().unwrap().path();
    h.state_mut().open_recording(rec);
    assert!(wait(&mut h, 5000, |a| a.review.is_some()));

    h.get_by_label("XY").click();
    let recorded = h.state().review.as_ref().unwrap().review.channel("4001/ROB_1/J1").unwrap().v.len();
    assert!(wait(&mut h, 3000, |a| xy_pairs(a).is_some_and(|(_, p)| p.points.len() + 2 >= recorded)), "the whole recording is in view: {recorded} samples");
    assert!(h.query_by_label("XY plot · reviewing, not live").is_some(), "the plot does not say it is not live");
    let (_, p) = xy_pairs(h.state()).unwrap();
    assert!(on_the_line(p) && p.points.len() <= recorded);

    // The first half in view: the pairs of the first half.
    let all = p.points.len();
    {
        let rs = h.state_mut().review.as_mut().unwrap();
        rs.view.1 /= 2.0;
        rs.fresh = true;
    }
    assert!(wait(&mut h, 3000, |a| xy_pairs(a).is_some_and(|(_, p)| p.points.len() < all * 6 / 10 && p.points.len() > all * 4 / 10)), "the plot is not of the stretch in view");

    // Its picture: the plot's part of the window.
    h.get_all_by_label("Save PNG").last().unwrap().click();
    let _ = h.run_ok();
    assert_eq!(h.state().png_pending, Some(crate::export::Picture::Xy), "the plot's Save PNG asked for the charts");
    let image = std::sync::Arc::new(egui::ColorImage::filled([1400, 900], egui::Color32::DARK_GRAY));
    h.event(egui::Event::Screenshot { viewport_id: egui::ViewportId::ROOT, user_data: egui::UserData::default(), image });
    let _ = h.run_ok();
    let pngs = files_ending(&dir.join("recordings"), " xy.png");
    assert_eq!(pngs.len(), 1, "{pngs:?}");
    let png = std::fs::read(&pngs[0]).unwrap();
    assert!(png.starts_with(b"\x89PNG"));
    // The XY window's part, not the charts': the plot with its axes and their labels,
    // and above it which channels, how many pairs, r and the line (a picture of
    // the plot alone said none of it). Its header's width and height are the window's.
    let size = |at: usize| u32::from_be_bytes(png[at..at + 4].try_into().unwrap()) as f32;
    let (w, plot) = (h.state().xy_window_rect.unwrap(), h.state().xy_rect.unwrap());
    assert!((size(16) - w.width()).abs() <= 2.0 && (size(20) - w.height()).abs() <= 2.0, "{} x {} for a window of {w:?}", size(16), size(20));
    assert!(w.contains_rect(plot) && w.height() > plot.height() + 40.0, "the window {w:?} and its plot {plot:?}");
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
