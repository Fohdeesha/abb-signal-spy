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
        let mut app = SpyApp::with_options(cc, dir.clone(), false, Options { ask: AskPolicy::Remote, find_vc: VcFinder::none(), vc_pause_patience: Duration::ZERO, ..Options::default() });
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
