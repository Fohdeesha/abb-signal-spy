use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;

use spy_core::discovery::VcFinder;
use spy_core::fake::{Behaviour, FakeController, SignalDef, SignalSource};
use spy_core::request::{Axis, MechUnit};
use spy_core::session::{AskPolicy, Options, Phase};
use spy_core::store::ChannelKey;
use spy_core::testdir::TestDir;

use crate::app::{Marker, SpyApp};

const FIELD_LAPTOP: (f32, f32) = (1366.0, 700.0);
const README_PAGE: (f32, f32) = (1366.0, 1024.0);
const DIP_EVERY_MS: i64 = 20_000;
const DIP_AT_MS: i64 = 7_000;

fn out_dir() -> PathBuf {
    let dir = PathBuf::from(std::env::var_os("ABB_SIGNAL_SPY_SHOTS").expect("set ABB_SIGNAL_SPY_SHOTS to the folder the pictures go in"));
    std::fs::create_dir_all(&dir).expect("the pictures' folder");
    dir
}

fn save(h: &mut Harness<'static, SpyApp>, name: &str) {
    let img = h.render().expect("the window renders");
    let path = out_dir().join(format!("{name}.png"));
    let f = std::fs::File::create(&path).expect("the picture's file");
    let mut enc = png::Encoder::new(std::io::BufWriter::new(f), img.width(), img.height());
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().and_then(|mut w| w.write_image_data(img.as_raw())).expect("the picture is written");
}

fn resolver_rad(t: u64) -> f64 {
    (t as f64 / 1000.0 * 0.3) % std::f64::consts::TAU
}

fn pwm_leg(t: u64, leg: f64) -> f32 {
    let s = t as f64 / 1000.0;
    let electrical = 4.0 * resolver_rad(t) - leg * std::f64::consts::TAU / 3.0;
    (0.5 + 0.32 * electrical.sin() + 0.004 * (s * 977.3 + leg).sin()) as f32
}

fn first_dip_after(t_ms: i64) -> i64 {
    let dip = t_ms - t_ms.rem_euclid(DIP_EVERY_MS) + DIP_AT_MS;
    if dip > t_ms { dip } else { dip + DIP_EVERY_MS }
}

fn showcase() -> Behaviour {
    let mut b = Behaviour::default();
    let float = |f: fn((u64, &str, u32)) -> f32| SignalDef { source: SignalSource::Float(Arc::new(f)), sample_ms: 4.032 };
    b.signals.insert(5027, float(|(t, _, _)| {
        let s = t as f64 / 1000.0;
        let from_dip_s = ((t as i64).rem_euclid(DIP_EVERY_MS) - DIP_AT_MS) as f64 / 1000.0;
        let dip = (-from_dip_s.powi(2) / 0.02).exp();
        (356.4 + 0.6 * (s * 91.7).sin() + 0.3 * (s * 271.3).sin() - 9.0 * dip) as f32
    }));
    b.signals.insert(4002, float(|(t, _, a)| {
        let s = t as f64 / 1000.0;
        (410.0 / a as f64 * (std::f64::consts::TAU * 0.16 * s).sin() + 6.0 * (s * 913.7).sin()) as f32
    }));
    b.signals.insert(5005, float(|(t, _, _)| {
        let s = t as f64 / 1000.0;
        (18.0 * (std::f64::consts::TAU * 0.16 * s + 0.25).sin() + 0.9 * (s * 271.3).sin()) as f32
    }));
    b.signals.insert(4001, float(|(t, _, _)| {
        let s = t as f64 / 1000.0;
        (0.5 * (std::f64::consts::TAU * 0.16 * s + 1.57).cos()) as f32
    }));
    b.signals.insert(5138, float(|(t, _, _)| resolver_rad(t) as f32));
    b.signals.insert(5028, float(|(t, _, _)| ((4.0 * resolver_rad(t)) % std::f64::consts::TAU) as f32));
    b.signals.insert(5020, float(|(t, _, _)| pwm_leg(t, 0.0)));
    b.signals.insert(5021, float(|(t, _, _)| pwm_leg(t, 1.0)));
    b.signals.insert(5022, float(|(t, _, _)| pwm_leg(t, 2.0)));
    b
}

fn key(signal: u32, unit: &str, axis: u8) -> ChannelKey {
    ChannelKey { signal, unit: MechUnit::new(unit).unwrap(), axis: Axis::new(axis).unwrap() }
}

fn window(dir: &std::path::Path, dark: bool, size: (f32, f32)) -> Harness<'static, SpyApp> {
    let dir = dir.to_path_buf();
    Harness::builder().with_size(size).with_max_steps(8).with_theme(if dark { egui::Theme::Dark } else { egui::Theme::Light }).wgpu().build_eframe(move |cc| {
        let mut app = SpyApp::with_options(cc, dir.clone(), crate::net::Windows::none(), Options { ask: AskPolicy::Remote, find_vc: VcFinder::none(), vc_pause_patience: Duration::ZERO, ..Options::default() });
        app.settings.record_dir = Some(dir.join("recordings"));
        app.settings.dark = dark;
        crate::theme::apply(&app.ctx, dark, 1.0);
        app.recolor();
        app.show_guide = false;
        app
    })
}

fn settle(h: &mut Harness<'static, SpyApp>, ms: u64) {
    let end = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < end {
        let _ = h.run_ok();
        std::thread::sleep(Duration::from_millis(40));
    }
    let _ = h.run_ok();
}

fn settle_until(h: &mut Harness<'static, SpyApp>, t_ms: i64) {
    let end = Instant::now() + Duration::from_secs(40);
    while h.state().session.store().newest().is_none_or(|n| n < t_ms) && Instant::now() < end {
        settle(h, 50);
    }
    assert!(h.state().session.store().newest().is_some_and(|n| n >= t_ms), "the fake's clock reaches {t_ms}");
}

fn connected(fake: &FakeController, dir: &std::path::Path, dark: bool, size: (f32, f32)) -> Harness<'static, SpyApp> {
    let mut h = window(dir, dark, size);
    h.state_mut().host_input = "127.0.0.1".into();
    h.state_mut().port_input = fake.port().to_string();
    h.state_mut().connect();
    let end = Instant::now() + Duration::from_secs(5);
    while h.state().session.status().phase != Phase::Streaming && Instant::now() < end {
        settle(&mut h, 50);
    }
    for k in [key(5027, "ROB_1", 1), key(4002, "ROB_1", 2), key(5005, "ROB_1", 2), key(4001, "ROB_1", 2), key(5138, "ROB_1", 3)] {
        assert!(h.state_mut().add_channels(vec![k], false));
    }
    h.state_mut().chans[1].smooth_ms = 100;
    h
}

fn streaming(fake: &FakeController, dir: &std::path::Path, dark: bool) -> Harness<'static, SpyApp> {
    let mut h = connected(fake, dir, dark, FIELD_LAPTOP);
    settle(&mut h, 3_000);
    let newest = h.state().session.store().newest().unwrap();
    h.state_mut().markers.push(Marker { t_ms: newest - 500, label: "dip".into() });
    settle(&mut h, 3_500);
    fake.with(|b| {
        b.mute.insert(5138);
    });
    settle(&mut h, 4_000);
    h
}

fn readme_window(fake: &FakeController, dir: &std::path::Path, dark: bool) -> Harness<'static, SpyApp> {
    let mut h = connected(fake, dir, dark, README_PAGE);
    h.state_mut().category = Some("drive / inverter".into());
    settle(&mut h, 500);
    let first = h.state().session.store().newest().expect("samples arrive");
    let dip = first_dip_after(first + 4_000);
    settle_until(&mut h, dip);
    h.state_mut().markers.push(Marker { t_ms: dip, label: "dip".into() });
    settle_until(&mut h, dip + 6_000);
    h
}

fn pause_with_cursors(h: &mut Harness<'static, SpyApp>, chan: usize, units: &str) {
    let lane = (h.state().chans[chan].lane, units.to_string());
    h.state_mut().expanded = Some(lane);
    h.state_mut().toggle_pause();
    h.state_mut().cursors_on = true;
    settle(h, 300);
    let x = h.state().lane_transforms.first().map(|t| t.bounds().max()[0]).unwrap_or(0.0);
    h.state_mut().cursor_a = Some(x - 6.0);
    h.state_mut().cursor_b = Some(x - 2.5);
    settle(h, 300);
}

fn back_to_live(h: &mut Harness<'static, SpyApp>) {
    h.state_mut().expanded = None;
    h.state_mut().toggle_pause();
    h.state_mut().cursors_on = false;
}

#[test]
#[ignore = "the README's pictures: run with ABB_SIGNAL_SPY_SHOTS set"]
fn readme() {
    let fake = FakeController::start(showcase()).unwrap();
    {
        let dir = TestDir::new("readme-dark");
        let mut h = readme_window(&fake, &dir, true);
        save(&mut h, "live");
        assert!(h.state_mut().add_channels(vec![key(5028, "ROB_1", 3)], false));
        settle(&mut h, 1_000);
        h.state_mut().dashboard = true;
        settle(&mut h, 300);
        save(&mut h, "dashboard");
        h.state_mut().dashboard = false;
        let pwm = h.state().chans.len();
        assert!(h.state_mut().add_channels(vec![key(5020, "ROB_1", 3), key(5021, "ROB_1", 3), key(5022, "ROB_1", 3)], true));
        let added = h.state().session.store().newest().expect("samples arrive");
        settle_until(&mut h, added + 10_500);
        pause_with_cursors(&mut h, pwm, "0..1");
        let first_card = h.get_by(|n| n.label().is_some_and(|l| l.starts_with("Options for 5027"))).rect();
        h.hover_at(first_card.center());
        h.event(egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Line, delta: egui::vec2(0.0, -60.0), phase: egui::TouchPhase::Move, modifiers: egui::Modifiers::NONE });
        settle(&mut h, 500);
        h.event(egui::Event::PointerGone);
        settle(&mut h, 300);
        save(&mut h, "cursors");
        h.state_mut().session.disconnect();
        settle(&mut h, 500);
    }
    {
        let dir = TestDir::new("readme-light");
        let mut h = readme_window(&fake, &dir, false);
        save(&mut h, "light");
        h.state_mut().session.disconnect();
        settle(&mut h, 300);
    }
}

#[test]
#[ignore = "pictures for a person to look at: run with ABB_SIGNAL_SPY_SHOTS set"]
fn crowded() {
    let fake = FakeController::start(showcase()).unwrap();
    for (name, size, log) in [("crowded-1-smallest", (crate::SMALLEST[0], crate::SMALLEST[1]), false), ("crowded-2-messages-open", FIELD_LAPTOP, true)] {
        let dir = TestDir::new("shots-crowded");
        let mut h = window(&dir, true, size);
        h.state_mut().host_input = "127.0.0.1".into();
        h.state_mut().port_input = fake.port().to_string();
        h.state_mut().connect();
        let end = Instant::now() + Duration::from_secs(5);
        while h.state().session.status().phase != Phase::Streaming && Instant::now() < end {
            settle(&mut h, 50);
        }
        let mut keys: Vec<ChannelKey> = (1..=6).map(|a| key(4002, "ROB_1", a)).collect();
        keys.extend([key(5027, "ROB_1", 1), key(5005, "ROB_1", 2), key(4001, "ROB_1", 2), key(5138, "ROB_1", 3), key(5028, "ROB_1", 3), key(5020, "ROB_1", 3)]);
        assert!(h.state_mut().add_channels(keys, false));
        h.state_mut().show_log = log;
        h.state_mut().dashboard = true;
        settle(&mut h, 2_500);
        fake.with(|b| {
            b.mute.insert(4002);
            b.mute.insert(5138);
        });
        settle(&mut h, 4_000);
        save(&mut h, name);
        fake.with(|b| b.mute.clear());
        h.state_mut().session.disconnect();
        settle(&mut h, 300);
    }
}

#[test]
#[ignore = "pictures for a person to look at: run with ABB_SIGNAL_SPY_SHOTS set"]
fn shots() {
    let fake = FakeController::start(showcase()).unwrap();
    {
        let dir = TestDir::new("shots-dark");
        let mut h = window(&dir, true, FIELD_LAPTOP);
        save(&mut h, "dark-0-idle");
        drop(h);
        let mut h = streaming(&fake, &dir, true);
        save(&mut h, "dark-1-live");
        h.state_mut().options_for = Some(key(4002, "ROB_1", 2).id());
        settle(&mut h, 300);
        save(&mut h, "dark-2-options");
        h.state_mut().options_for = None;
        pause_with_cursors(&mut h, 1, "Nm");
        save(&mut h, "dark-3-expanded-paused");
        back_to_live(&mut h);
        h.state_mut().dashboard = true;
        settle(&mut h, 300);
        save(&mut h, "dark-4-dashboard");
        h.state_mut().dashboard = false;
        h.state_mut().selected = Some(4002);
        settle(&mut h, 300);
        save(&mut h, "dark-5-details");
        h.state_mut().selected = None;
        h.state_mut().open_add(4002);
        settle(&mut h, 300);
        save(&mut h, "dark-8-add");
        h.state_mut().add = None;
        h.state_mut().open_sets();
        settle(&mut h, 300);
        save(&mut h, "dark-9-sets");
        h.state_mut().sets = None;
        h.state_mut().show_diag = true;
        settle(&mut h, 300);
        save(&mut h, "dark-10-connection-details");
        h.state_mut().show_diag = false;
        h.state_mut().show_log = true;
        settle(&mut h, 300);
        save(&mut h, "dark-11-messages");
        h.state_mut().show_log = false;
        h.state_mut().settings.signals_folded = true;
        h.state_mut().toggle_recording();
        settle(&mut h, 2_500);
        save(&mut h, "dark-6-folded-recording");
        h.state_mut().toggle_recording();
        h.state_mut().settings.signals_folded = false;
        settle(&mut h, 500);
        let rec = h.state().last_folder.clone().expect("a recording");
        h.state_mut().open_recording(rec);
        let end = Instant::now() + Duration::from_secs(10);
        while h.state().review.is_none() && Instant::now() < end {
            settle(&mut h, 100);
        }
        if let Some(rs) = &mut h.state_mut().review {
            rs.cursors_on = true;
            rs.cursor_a = Some(0.6);
        }
        settle(&mut h, 300);
        save(&mut h, "dark-7-review");
        h.state_mut().review = None;
        h.state_mut().settings.status_open = true;
        settle(&mut h, 300);
        save(&mut h, "dark-12-status-open");
        h.state_mut().settings.status_open = false;
        h.state_mut().toggle_recording();
        settle(&mut h, 1_000);
        fake.cut_network(Duration::from_secs(12), Duration::from_secs(12));
        let end = Instant::now() + Duration::from_secs(15);
        while !matches!(h.state().session.status().phase, Phase::Reconnecting { attempt, .. } if attempt >= 2) && Instant::now() < end {
            let _ = h.run_ok();
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = h.run_ok();
        save(&mut h, "dark-13-reconnecting");
        h.state_mut().toggle_recording();
        h.state_mut().session.disconnect();
        settle(&mut h, 12_000);
    }
    fake.with(|b| {
        b.mute.clear();
    });
    {
        let dir = TestDir::new("shots-light");
        let mut h = streaming(&fake, &dir, false);
        save(&mut h, "light-1-live");
        h.state_mut().options_for = Some(key(4002, "ROB_1", 2).id());
        settle(&mut h, 300);
        save(&mut h, "light-2-options");
        h.state_mut().session.disconnect();
        settle(&mut h, 300);
    }
}

fn snap(h: &mut Harness<'static, SpyApp>, size: (f32, f32), n: &mut u32, name: &str) {
    settle(h, 300);
    *n += 1;
    save(h, &format!("audit-{}-{:02}-{name}", size.0 as u32, n));
}

fn press(h: &mut Harness<'static, SpyApp>, label: &str) {
    if let Some(at) = h.query_by_label(label).map(|n| n.rect().center()) {
        h.hover_at(at);
        let _ = h.run_ok();
        for pressed in [true, false] {
            h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
        }
        let _ = h.run_ok();
    }
    settle(h, 200);
}

fn escape(h: &mut Harness<'static, SpyApp>) {
    h.key_press(egui::Key::Escape);
    settle(h, 200);
}

#[test]
#[ignore = "pictures for a person to look at: run with ABB_SIGNAL_SPY_SHOTS set"]
fn audit() {
    let fake = FakeController::start(showcase()).unwrap();
    for size in [(1366.0, 768.0), (crate::SMALLEST[0], crate::SMALLEST[1])] {
        let dir = TestDir::new("shots-audit");
        let mut n = 0;
        let mut h = window(&dir, true, size);
        snap(&mut h, size, &mut n, "idle");
        drop(h);
        let mut h = connected(&fake, &dir, true, size);
        settle(&mut h, 3_000);
        snap(&mut h, size, &mut n, "live");
        press(&mut h, "list");
        snap(&mut h, size, &mut n, "controller-list");
        escape(&mut h);
        press(&mut h, "filters");
        snap(&mut h, size, &mut n, "filters");
        escape(&mut h);
        press(&mut h, "view");
        snap(&mut h, size, &mut n, "view-menu");
        escape(&mut h);
        h.state_mut().selected = Some(520);
        snap(&mut h, size, &mut n, "details-long-name");
        h.state_mut().selected = None;
        h.state_mut().open_add(4002);
        snap(&mut h, size, &mut n, "add");
        h.state_mut().add = None;
        h.state_mut().open_sets();
        snap(&mut h, size, &mut n, "sets");
        h.state_mut().sets = None;
        h.state_mut().note_edit = Some(crate::notes::Edit { signal: 4002, draft: Default::default() });
        snap(&mut h, size, &mut n, "notes-editor");
        h.state_mut().note_edit = None;
        h.state_mut().options_for = Some(key(4002, "ROB_1", 2).id());
        snap(&mut h, size, &mut n, "options");
        h.state_mut().options_for = None;
        pause_with_cursors(&mut h, 1, "Nm");
        snap(&mut h, size, &mut n, "paused-full-height");
        back_to_live(&mut h);
        h.state_mut().dashboard = true;
        snap(&mut h, size, &mut n, "dashboard");
        h.state_mut().dashboard = false;
        for (flag, name) in [(0, "connection-details"), (1, "messages"), (2, "about"), (3, "guide"), (4, "catalogue"), (5, "recordings-folder"), (6, "rws")] {
            let set = |a: &mut SpyApp, on: bool| match flag {
                0 => a.show_diag = on,
                1 => a.show_log = on,
                2 => a.show_about = on,
                3 => a.show_guide = on,
                4 => a.show_catalogue_info = on,
                5 => a.show_record_dir = on,
                _ => a.show_rws = on,
            };
            set(h.state_mut(), true);
            snap(&mut h, size, &mut n, name);
            set(h.state_mut(), false);
        }
        h.state_mut().open_compare(key(4002, "ROB_1", 2).id());
        snap(&mut h, size, &mut n, "compare");
        h.state_mut().compare = None;
        h.state_mut().xy = Some(crate::xy::XyState { x: Some(key(4001, "ROB_1", 2).id()), y: Some(key(4002, "ROB_1", 2).id()), ..Default::default() });
        snap(&mut h, size, &mut n, "xy");
        h.state_mut().xy = None;
        assert!(h.state_mut().add_derived(spy_core::derived::Derived::Turn { angle: key(5138, "ROB_1", 3), target_deg: Some(90.0) }));
        snap(&mut h, size, &mut n, "derived");
        h.state_mut().settings.signals_folded = true;
        snap(&mut h, size, &mut n, "folded");
        h.state_mut().settings.signals_folded = false;
        h.state_mut().toggle_recording();
        settle(&mut h, 2_000);
        snap(&mut h, size, &mut n, "recording");
        h.state_mut().toggle_recording();
        settle(&mut h, 800);
        h.state_mut().show_recordings = true;
        snap(&mut h, size, &mut n, "recordings");
        h.state_mut().show_recordings = false;
        let rec = h.state().last_folder.clone().expect("a recording");
        h.state_mut().open_recording(rec);
        let end = Instant::now() + Duration::from_secs(10);
        while h.state().review.is_none() && Instant::now() < end {
            settle(&mut h, 100);
        }
        if let Some(rs) = &mut h.state_mut().review {
            rs.cursors_on = true;
            rs.cursor_a = Some(0.6);
        }
        snap(&mut h, size, &mut n, "review");
        h.state_mut().review = None;
        h.state_mut().session.disconnect();
        settle(&mut h, 500);
        snap(&mut h, size, &mut n, "disconnected-with-channels");
    }
}
