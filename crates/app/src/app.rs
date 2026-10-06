use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText};

use spy_core::catalogue::Catalogue;
use spy_core::discovery::{self, LocalController};
use spy_core::log::{Level, LogBook};
use spy_core::recording::{ChannelInfo, Recorder};
use spy_core::request::{Axis, MechUnit};
use spy_core::session::{Options, Phase, Session, Target, ROBAPI_PORT};
use spy_core::store::{ChannelKey, Store};

use crate::fields;
use crate::net::{self, Hostnames};
use crate::phone::{PhoneServer, Snapshot};
use crate::settings::{SavedChannel, SavedController, Settings};
use crate::theme;
use crate::view;

pub struct ChanView {
    pub key: ChannelKey,
    pub color: Color32,
    pub radians: bool,
    pub hold_nonzero: bool,
    pub lane: u32,
    pub stats: Stats,
    pub smooth_ms: u32,
    pub scale: crate::charts::Scale,
    pub decimals: Option<u8>,
}

impl ChanView {
    pub fn new(key: ChannelKey, color: Color32, lane: u32) -> ChanView {
        ChanView { key, color, radians: false, hold_nonzero: false, lane, stats: Stats::default(), smooth_ms: 0, scale: crate::charts::Scale::default(), decimals: None }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Stats {
    pub n: u64,
    pub sum: f64,
    pub min: f64,
    pub max: f64,
    pub sum_sin: f64,
    pub sum_cos: f64,
    pub hold: Option<view::ZeroHold>,
    pub upto: i64,
}

impl Default for Stats {
    fn default() -> Stats {
        Stats { n: 0, sum: 0.0, min: f64::INFINITY, max: f64::NEG_INFINITY, sum_sin: 0.0, sum_cos: 0.0, hold: None, upto: i64::MIN }
    }
}

impl Stats {
    pub fn mean(&self, r: view::Reading) -> Option<f64> {
        if self.n == 0 {
            return None;
        }
        Some(if r == view::Reading::Wrapping { self.sum_sin.atan2(self.sum_cos).rem_euclid(std::f64::consts::TAU) } else { self.sum / self.n as f64 })
    }
}

pub struct Marker {
    pub t_ms: i64,
    pub label: String,
}

pub enum Discovery {
    Idle,
    Running(JoinHandle<Result<Vec<LocalController>, String>>),
    Done(Result<Vec<LocalController>, String>),
}

pub struct Toast {
    pub shown: Option<Instant>,
    pub text: String,
    pub level: Level,
}

const MAX_TOASTS: usize = 8;
pub const CLOSE_PATIENCE: Duration = Duration::from_secs(20);

pub struct AddDialog {
    pub signal: u32,
    pub unit: String,
    pub axis: u8,
}

pub const SIGNALS_WIDEST: f32 = 388.0;

pub const FOOTER_BUTTON_H: f32 = 30.0;
pub const TOAST_ABOVE: f32 = 48.0;

pub fn toast_life(level: Level) -> Duration {
    match level {
        Level::Info => Duration::from_secs(6),
        Level::Warn => Duration::from_secs(10),
        Level::Error => Duration::from_secs(15),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowKind {
    About,
    Guide,
    Catalogue,
    RecordDir,
    Diag,
    Recordings,
    Rws,
    Compare,
    Xy,
}
pub const SIGNALS_LEAST: f32 = 248.0;
pub const CHANNELS_LEAST: f32 = 260.0;
pub const CHANNELS_WIDEST: f32 = 338.0;
pub const CHARTS_LEAST: f32 = 360.0;
const LOG_OPENING: f32 = 190.0;
const LOG_LEAST: f32 = 90.0;
pub const LOG_MOST_SHARE: f32 = 0.3;
const LOG_OPENING_SHARE: f32 = 0.25;
const SIGNALS_MOST: f32 = 470.0;
const CHANNELS_MOST: f32 = 490.0;

pub fn side_most(window: f32, least: f32, most: f32) -> f32 {
    ((window - CHARTS_LEAST) / 2.0).clamp(least, most)
}

pub fn signals_opening(window: f32) -> f32 {
    (window * 0.3).clamp(SIGNALS_LEAST, SIGNALS_WIDEST)
}

pub fn channels_opening(window: f32) -> f32 {
    (window * 0.25).clamp(CHANNELS_LEAST, CHANNELS_WIDEST)
}
const BAR_SLACK: f32 = 24.0;

pub struct SpyApp {
    pub ctx: egui::Context,
    pub session: Session,
    pub session_opts: Options,
    pub connected_to: Option<Target>,
    pub log: Arc<LogBook>,
    pub catalogue: Catalogue,
    pub settings: Settings,
    pub settings_path: PathBuf,
    pub settings_dirty: Option<Instant>,
    pub settings_base: serde_json::Map<String, serde_json::Value>,
    pub settings_locked: Option<String>,
    pub chans: Vec<ChanView>,
    pub next_lane: u32,
    pub store_epoch: u64,

    pub host_input: String,
    pub port_input: String,
    pub name_input: String,
    pub discovery: Discovery,

    pub search: String,
    pub selected: Option<u32>,
    pub raw_number: String,
    pub category: Option<String>,
    pub min_confidence: Option<spy_core::catalogue::Confidence>,
    pub only_favourites: bool,
    pub add: Option<AddDialog>,
    pub sets: Option<crate::sets::SetDialog>,
    pub derived: Vec<crate::derived_view::DerivedView>,
    pub plateau_trend_ms: i64,
    pub lane_transforms: Vec<egui_plot::PlotTransform>,
    pub column_cache: crate::charts::ColumnCache,
    pub hover_text: std::sync::Arc<std::sync::Mutex<String>>,

    pub rws: Option<crate::rws_view::RwsLink>,
    pub rws_form: crate::rws_view::RwsForm,
    pub rws_poll: Duration,
    pub show_rws: bool,
    pub controller_events: Vec<crate::rws_view::ControllerEvent>,
    pub late_windows: Vec<crate::rws_view::LateWindow>,
    pub snapshot_span: Option<(i64, i64)>,

    pub window_s: f64,
    pub paused_at: Option<i64>,
    pub pause_fresh: bool,
    pub cursors_on: bool,
    pub cursor_a: Option<f64>,
    pub cursor_b: Option<f64>,
    pub cursor_a_undo: Option<(Option<f64>, Instant)>,
    pub markers: Vec<Marker>,
    pub lane_zoom: HashMap<(u32, String), (f64, f64)>,
    pub lane_ranges: HashMap<(u32, String), (f64, f64)>,
    pub wheel_acc: f32,

    pub recorder: Option<Recorder>,
    pub slow: Option<Recorder>,
    pub stopping: Vec<(&'static str, spy_core::recording::Stopping)>,
    pub snapshot_job: Option<JoinHandle<Result<(PathBuf, u64), String>>>,
    pub marker_text: String,
    pub rec_label: String,
    pub last_folder: Option<PathBuf>,

    pub phone: Option<PhoneServer>,
    pub phone_snapshot: Arc<Mutex<Snapshot>>,
    pub phone_built: Instant,

    pub view_ms: Option<(i64, i64)>,
    pub charts_rect: Option<egui::Rect>,
    pub signals_rect: Option<egui::Rect>,
    pub png_pending: Option<crate::export::Picture>,
    pub xy: Option<crate::xy::XyState>,
    pub xy_rect: Option<egui::Rect>,
    pub xy_window_rect: Option<egui::Rect>,
    pub compare: Option<crate::compare::CompareState>,
    pub notes: crate::notes::Notes,
    pub note_edit: Option<crate::notes::Edit>,
    pub export_job: Option<crate::export::ExportJob>,
    pub export_note: &'static str,
    pub export_stop: Arc<std::sync::atomic::AtomicBool>,

    pub review: Option<crate::review_view::ReviewState>,
    pub review_job: Option<crate::review_view::ReviewJob>,
    pub show_recordings: bool,
    pub recordings_list: Option<Vec<(PathBuf, spy_core::recording::Meta)>>,
    pub recording_path_input: String,

    pub options_for: Option<String>,
    pub expanded: Option<(u32, String)>,
    pub dashboard: bool,
    pub show_log: bool,

    pub confirm_reset: bool,
    pub confirm_remove_all: bool,
    pub show_record_dir: bool,
    pub record_dir_input: String,
    pub show_about: bool,
    pub show_diag: bool,
    pub show_guide: bool,
    pub show_catalogue_info: bool,
    pub window_stack: Vec<WindowKind>,
    pub scroll_list_to: Option<u32>,
    pub forget_ask: Option<usize>,
    pub windows_opened_now: Vec<WindowKind>,
    pub focus_next: Option<&'static str>,
    pub hostnames: Hostnames,
    pub toasts: Vec<Toast>,
    pub toast_rect: Option<egui::Rect>,
    pub rates: (Instant, u64, u64, f64, f64),
    pub windows: crate::net::Windows,
    pub others_open: usize,
    pub others_checked: Option<Instant>,
    pub moves_seen: u64,
    pub attention_asked: bool,
    pub bar_need: f32,
    pub log_filter_warn: bool,
    pub title: String,
}

pub fn chan_color(i: usize, dark: bool) -> Color32 {
    theme::channel_color(i, dark)
}

impl SpyApp {
    pub fn new(cc: &eframe::CreationContext<'_>, data_dir: PathBuf, windows: crate::net::Windows) -> SpyApp {
        SpyApp::with_options(cc, data_dir, windows, Options::default())
    }

    pub fn with_options(cc: &eframe::CreationContext<'_>, data_dir: PathBuf, windows: crate::net::Windows, opts: Options) -> SpyApp {
        let ctx = cc.egui_ctx.clone();
        let settings_path = data_dir.join(crate::settings::FILE);
        let first_run = !settings_path.exists();
        let loaded = Settings::load_full(&settings_path);
        let (settings, note, settings_locked, settings_base) = (loaded.settings, loaded.note, loaded.locked, loaded.base);
        let (notes, notes_note) = crate::notes::Notes::load(&data_dir.join(crate::notes::FILE));
        theme::install_fonts(&ctx);
        theme::apply(&ctx, settings.dark, settings.ui_scale);

        let log = Arc::new(LogBook::new());
        if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(data_dir.join("signal-spy.log")) {
            if f.metadata().map(|m| m.len() > 5 * 1024 * 1024).unwrap_or(false) {
                if let Ok(f2) = std::fs::File::create(data_dir.join("signal-spy.log")) {
                    log.attach_file(f2);
                }
            } else {
                log.attach_file(f);
            }
        }
        log.info(format!("ABB Signal Spy {} started. Settings: {}", env!("CARGO_PKG_VERSION"), settings_path.display()));
        let crash = data_dir.join("crash.txt");
        let crash_note = crash.is_file().then(|| {
            let memory = std::fs::read_to_string(&crash).is_ok_and(|t| crate::oom::is_memory_note(&t));
            let kept = data_dir.join(format!("crash-{}.txt", spy_core::util::local_stamp(std::time::SystemTime::now())));
            let kept = if std::fs::rename(&crash, &kept).is_ok() { kept } else { crash.clone() };
            if memory {
                format!("ABB Signal Spy stopped last time when Windows refused it memory (the PC's memory was used up). What happened is in {}.", kept.display())
            } else {
                format!("ABB Signal Spy hit an internal error last time. What happened is in {}: please include that file when reporting it.", kept.display())
            }
        });

        let (catalogue, cat_note) = match &settings.catalogue_file {
            Some(p) => match Catalogue::load(p) {
                Ok(c) => (c, None),
                Err(e) => (Catalogue::builtin(), Some(format!("The catalogue file could not be loaded ({e}); using the built-in one."))),
            },
            None => (Catalogue::builtin(), None),
        };
        if let Some(n) = cat_note {
            log.warn(n);
        }

        let repaint_ctx = ctx.clone();
        let store = Arc::new(Store::new());
        let session = Session::spawn(opts.clone(), log.clone(), store, Arc::new(move || repaint_ctx.request_repaint()));

        let (host_input, port_input) = match &settings.last_target {
            Some(t) => (t.host.clone(), t.port.to_string()),
            None => (String::new(), ROBAPI_PORT.to_string()),
        };
        let window_s = settings.window_s;
        let mut app = SpyApp {
            ctx,
            session,
            session_opts: opts,
            connected_to: None,
            log,
            catalogue,
            settings,
            settings_path,
            settings_dirty: None,
            settings_base,
            settings_locked,
            chans: Vec::new(),
            next_lane: 1,
            store_epoch: 0,
            host_input,
            port_input,
            name_input: String::new(),
            discovery: Discovery::Idle,
            search: String::new(),
            selected: None,
            raw_number: String::new(),
            category: None,
            min_confidence: None,
            only_favourites: false,
            add: None,
            sets: None,
            derived: Vec::new(),
            plateau_trend_ms: spy_core::derived::PLATEAU_TREND_MS,
            lane_transforms: Vec::new(),
            column_cache: Default::default(),
            hover_text: Default::default(),
            rws: None,
            rws_form: crate::rws_view::RwsForm::default(),
            rws_poll: crate::rws_view::POLL,
            show_rws: false,
            controller_events: Vec::new(),
            late_windows: Vec::new(),
            snapshot_span: None,
            window_s,
            paused_at: None,
            pause_fresh: false,
            cursors_on: false,
            cursor_a: None,
            cursor_b: None,
            cursor_a_undo: None,
            markers: Vec::new(),
            lane_zoom: HashMap::new(),
            lane_ranges: HashMap::new(),
            wheel_acc: 0.0,
            recorder: None,
            slow: None,
            stopping: Vec::new(),
            snapshot_job: None,
            marker_text: String::new(),
            rec_label: String::new(),
            last_folder: None,
            phone: None,
            phone_snapshot: Arc::new(Mutex::new(Snapshot::default())),
            phone_built: Instant::now(),
            view_ms: None,
            charts_rect: None,
            signals_rect: None,
            png_pending: None,
            xy: None,
            xy_rect: None,
            xy_window_rect: None,
            compare: None,
            notes,
            note_edit: None,
            export_job: None,
            export_note: "",
            export_stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            review: None,
            review_job: None,
            show_recordings: false,
            recordings_list: None,
            recording_path_input: String::new(),
            options_for: None,
            expanded: None,
            dashboard: false,
            show_log: false,
            confirm_reset: false,
            confirm_remove_all: false,
            show_record_dir: false,
            record_dir_input: String::new(),
            show_about: false,
            show_diag: false,
            show_guide: false,
            show_catalogue_info: false,
            window_stack: Vec::new(),
            scroll_list_to: None,
            forget_ask: None,
            windows_opened_now: Vec::new(),
            focus_next: None,
            hostnames: Arc::new(Mutex::new(HashMap::new())),
            toasts: Vec::new(),
            toast_rect: None,
            rates: (Instant::now(), 0, 0, 0.0, 0.0),
            windows,
            others_open: 0,
            others_checked: None,
            moves_seen: 0,
            attention_asked: false,
            bar_need: 0.0,
            log_filter_warn: false,
            title: String::new(),
        };
        let saved = app.settings.channels.clone();
        app.next_lane = saved.iter().map(|c| c.lane + 1).max().unwrap_or(1).max(1);
        for c in saved {
            if let (Ok(unit), Some(axis)) = (MechUnit::new(&c.unit), Axis::new(c.axis)) {
                let key = ChannelKey { signal: c.signal, unit, axis };
                if app.chans.iter().any(|x| x.key == key) {
                    continue;
                }
                let i = app.chans.len();
                let lane = if c.lane == 0 {
                    app.next_lane += 1;
                    app.next_lane - 1
                } else {
                    c.lane
                };
                app.chans.push(ChanView { radians: c.radians, hold_nonzero: c.hold_nonzero, smooth_ms: c.smooth_ms, scale: c.scale, decimals: c.decimals, ..ChanView::new(key, chan_color(i, app.settings.dark), lane) });
            }
        }
        app.sync_channels();
        for d in app.settings.derived.clone() {
            if app.derived.iter().any(|x| x.live.def().same(&d.def)) {
                continue;
            }
            if let Some(k) = d.def.inputs().into_iter().find(|k| !app.chans.iter().any(|c| &c.key == k)) {
                app.log.warn(format!("{} was not restored: its input {k} is not among the channels.", app.derived_label(&d.def)));
                continue;
            }
            let lane = if d.lane == 0 {
                app.next_lane += 1;
                app.next_lane - 1
            } else {
                app.next_lane = app.next_lane.max(d.lane + 1);
                d.lane
            };
            let color = chan_color(app.chans.len() + app.derived.len(), app.settings.dark);
            let target_text = match &d.def {
                spy_core::derived::Derived::Turn { target_deg: Some(t), .. } => view::fmt(*t),
                _ => String::new(),
            };
            app.derived.push(crate::derived_view::DerivedView { live: spy_core::derived::Live::new(d.def), color, lane, stats: Stats::default(), target_text, target_from: None });
        }
        if let Some(n) = note {
            app.toast(Level::Warn, n);
        }
        if let Some(n) = notes_note {
            app.log.warn(n.clone());
            app.toast(Level::Warn, n);
        }
        if let Some(n) = crash_note {
            app.toast(Level::Warn, n);
        }
        app.show_guide = first_run;
        app
    }

    pub fn toast(&mut self, level: Level, text: impl Into<String>) {
        let text = text.into();
        self.log.push(level, text.clone());
        self.show_toast(level, text);
    }

    pub fn show_toast(&mut self, level: Level, text: String) {
        self.toasts.push(Toast { shown: None, text, level });
        let excess = self.toasts.len().saturating_sub(MAX_TOASTS);
        self.toasts.drain(..excess);
    }

    pub fn mark_settings_dirty(&mut self) {
        self.settings_dirty = Some(Instant::now());
    }

    pub fn save_settings(&mut self) {
        self.settings.window_s = self.window_s;
        self.settings.channels = self
            .chans
            .iter()
            .map(|c| SavedChannel { signal: c.key.signal, unit: c.key.unit.to_string(), axis: c.key.axis.one_based(), radians: c.radians, hold_nonzero: c.hold_nonzero, lane: c.lane, smooth_ms: c.smooth_ms, scale: c.scale, decimals: c.decimals })
            .collect();
        self.settings.derived = self
            .derived
            .iter()
            .map(|d| {
                let def = match d.live.def() {
                    spy_core::derived::Derived::Sag { link, .. } => spy_core::derived::Derived::Sag { link: link.clone(), plateau_v: None },
                    spy_core::derived::Derived::Turn { angle, .. } if d.target_from.is_some() => spy_core::derived::Derived::Turn { angle: angle.clone(), target_deg: None },
                    other => other.clone(),
                };
                crate::settings::SavedDerived { def, lane: d.lane }
            })
            .collect();
        self.settings_dirty = None;
        if self.settings_locked.is_some() {
            return;
        }
        if let Err(e) = self.settings.save_changes(&self.settings_path, &mut self.settings_base) {
            self.log.warn(format!("Could not save the settings: {e}"));
        }
    }

    pub fn sync_channels(&mut self) {
        let keys: Vec<ChannelKey> = self.chans.iter().map(|c| c.key.clone()).collect();
        for ch in self.session.store().all() {
            if !keys.contains(&ch.key) {
                self.session.store().remove(&ch.key);
            }
        }
        let text: Vec<ChannelKey> = keys.iter().filter(|k| self.catalogue.get(k.signal).is_some_and(view::is_text)).cloned().collect();
        self.session.set_channels_expecting_text(keys, text);
        self.prune_derived();
        self.recolor();
        self.mark_settings_dirty();
    }

    pub fn recolor(&mut self) {
        let dark = self.settings.dark;
        for (i, c) in self.chans.iter_mut().enumerate() {
            c.color = chan_color(i, dark);
        }
        let n = self.chans.len();
        for (j, d) in self.derived.iter_mut().enumerate() {
            d.color = chan_color(n + j, dark);
        }
    }

    pub fn channel_info(&self, key: &ChannelKey) -> ChannelInfo {
        let s = self.catalogue.get(key.signal);
        ChannelInfo {
            name: view::label(&self.catalogue, key),
            units: s.map(|s| s.units.clone()).unwrap_or_default(),
            description: s.map(|s| s.description.clone()).unwrap_or_default(),
        }
    }

    pub fn infos(&self) -> HashMap<ChannelKey, ChannelInfo> {
        self.chans.iter().map(|c| (c.key.clone(), self.channel_info(&c.key))).collect()
    }

    pub fn record_dir(&self) -> PathBuf {
        self.settings.record_dir.clone().unwrap_or_else(crate::paths::default_record_dir)
    }

    fn parse_target(&self) -> Result<Target, String> {
        let host = self.host_input.trim().trim_matches(['[', ']']).to_string();
        if host.is_empty() {
            return Err("Type the controller's address (for example 192.168.125.1), or pick a virtual controller from the list.".into());
        }
        if host.contains(char::is_whitespace) || host.contains('/') {
            return Err(format!("\"{host}\" is not an address."));
        }
        let port: u16 = self.port_input.trim().parse().map_err(|_| format!("\"{}\" is not a port number (an IRC5 uses 5515).", self.port_input.trim()))?;
        if port == 0 {
            return Err("Port 0 is not a port.".into());
        }
        Ok(Target { host, port })
    }

    pub fn connect(&mut self) {
        match self.parse_target() {
            Ok(t) => {
                if !self.session.is_running() {
                    for r in [self.recorder.take(), self.slow.take()].into_iter().flatten() {
                        self.toast(Level::Warn, format!("Recording finished when the connection worker was restarted: {}", r.status().dir.display()));
                        self.stopping.push(("worker restarted", r.stop_in_background()));
                    }
                    self.log.warn("The connection worker had stopped after an internal error; starting a new one.");
                    let c = self.ctx.clone();
                    let store = self.session.store().clone();
                    self.session = Session::spawn(self.session_opts.clone(), self.log.clone(), store, Arc::new(move || c.request_repaint()));
                    self.sync_channels();
                }
                let previous = self.session.status().target.clone();
                if previous.as_ref().is_some_and(|p| p != &t) {
                    let mut stopped = Vec::new();
                    for r in [self.recorder.take(), self.slow.take()].into_iter().flatten() {
                        stopped.push(r.status().dir);
                        self.stopping.push(("another controller", r.stop_in_background()));
                    }
                    if !stopped.is_empty() {
                        self.toast(Level::Warn, format!("Recording finished before connecting to a different controller: {}", stopped.iter().map(|d| d.display().to_string()).collect::<Vec<_>>().join(", ")));
                    }
                    self.markers.clear();
                    self.cursor_a = None;
                    self.cursor_b = None;
                    self.paused_at = None;
                }
                self.settings.remember(&t);
                self.mark_settings_dirty();
                self.connected_to = Some(t.clone());
                self.moves_seen = self.session.status().moves;
                self.session.connect(t);
            }
            Err(e) => self.toast(Level::Error, e),
        }
    }

    fn follow_moved_controller(&mut self) {
        let (moved, moves) = {
            let st = self.session.status();
            (st.moved.clone(), st.moves)
        };
        let follow = move_to_follow(moved, moves, self.moves_seen, self.connected_to.as_ref());
        self.moves_seen = moves;
        let Some((from, to)) = follow else { return };
        if self.parse_target().ok().as_ref() == Some(&from) {
            self.port_input = to.port.to_string();
        }
        self.settings.recent.retain(|t| t != &from);
        self.settings.remember(&to);
        for c in &mut self.settings.controllers {
            if c.host == from.host && c.port == from.port {
                c.port = to.port;
            }
        }
        self.mark_settings_dirty();
        self.toast(Level::Info, format!("The virtual controller restarted, on port {} (it was on {}): connecting there. A virtual controller takes a new port at every start.", to.port, from.port));
        self.connected_to = Some(to);
    }

    fn menu(&mut self, ui: &mut egui::Ui) {
        fn plain(ui: &mut egui::Ui) {
            ui.style_mut().override_text_style = Some(egui::TextStyle::Body);
        }
        ui.scope(|ui| {
            plain(ui);
            ui.spacing_mut().interact_size.y = 28.0;
            ui.spacing_mut().button_padding = egui::vec2(12.0, 2.0);
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("file", |ui| {
                    plain(ui);
                    if ui.button("open a recording...").on_hover_text("Chart a recording made earlier. The live session carries on meanwhile.").clicked() {
                        self.show_recordings = true;
                        self.recordings_list = None;
                        ui.close();
                    }
                    if self.review.is_some() && ui.button("close the recording").clicked() {
                        self.review = None;
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("open the recordings folder").clicked() {
                        let d = self.record_dir();
                        let _ = std::fs::create_dir_all(&d);
                        crate::paths::open_folder(&d);
                        ui.close();
                    }
                    if ui.button("recordings folder...").on_hover_text("Where recordings, saved CSVs and pictures go").clicked() {
                        self.show_record_dir = true;
                        self.record_dir_input = self.record_dir().display().to_string();
                        ui.close();
                    }
                    if ui.button("open the settings folder").clicked() {
                        if let Some(p) = self.settings_path.parent() {
                            crate::paths::open_folder(p);
                        }
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("quit").clicked() {
                        self.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                ui.menu_button("controller", |ui| {
                    plain(ui);
                    let connected = self.session.status().phase.is_connected();
                    if ui.button("controller details (RWS)...").on_hover_text("Read-only: the controller's name and RobotWare version, its event log on the charts, a motor's commutator offset. Needs the controller's RWS login, which is not stored.").clicked() {
                        self.show_rws = true;
                        ui.close();
                    }
                    if theme::lockable(ui, connected, egui::Button::new("reset InfoStream..."))
                        .on_hover_text("Removes EVERY client's test-signal streams on the controller. Only for when a crashed program left streams behind.")
                        .on_disabled_hover_text("Connect to the controller first.")
                        .clicked()
                    {
                        self.confirm_reset = true;
                        ui.close();
                    }
                    if ui.button("connection details").clicked() {
                        self.show_diag = true;
                        ui.close();
                    }
                });
                ui.menu_button("catalogue", |ui| {
                    plain(ui);
                    if ui.button("about this catalogue").clicked() {
                        self.show_catalogue_info = true;
                        ui.close();
                    }
                    if ui.button("load a catalogue file...").on_hover_text("For another robot or RobotWare version: a catalogue file made with the scan tool or shared by someone else.").clicked() {
                        ui.close();
                        self.load_catalogue_dialog();
                    }
                    if theme::lockable(ui, self.settings.catalogue_file.is_some(), egui::Button::new("back to the built-in catalogue")).on_disabled_hover_text("The built-in catalogue is the one in use.").clicked() {
                        self.catalogue = Catalogue::builtin();
                        self.settings.catalogue_file = None;
                        self.mark_settings_dirty();
                        self.toast(Level::Info, "Using the built-in catalogue.");
                        ui.close();
                    }
                    ui.separator();
                    self.export_notes_button(ui);
                });
                ui.menu_button("view", |ui| {
                    plain(ui);
                    if theme::check(ui, &mut self.settings.dark, "dark").changed() {
                        theme::apply(&self.ctx, self.settings.dark, self.settings.ui_scale);
                        self.recolor();
                        self.mark_settings_dirty();
                    }
                    ui.horizontal(|ui| {
                        ui.label("text size");
                        for (label, s) in [("S", 0.9f32), ("M", 1.0), ("L", 1.2), ("XL", 1.45)] {
                            if theme::chip(ui, (self.settings.ui_scale - s).abs() < 0.01, label).clicked() {
                                self.settings.ui_scale = s;
                                theme::apply(&self.ctx, self.settings.dark, s);
                                self.mark_settings_dirty();
                            }
                        }
                    });
                    ui.separator();
                    theme::check(ui, &mut self.dashboard, "live dashboard").on_hover_text("Big numbers in place of the charts, to read from a step away.");
                    if theme::check(ui, &mut self.settings.signals_folded, "fold the signal list").changed() {
                        self.mark_settings_dirty();
                    }
                    theme::check(ui, &mut self.show_log, "messages");
                    ui.separator();
                    self.phone_switch(ui);
                });
                ui.menu_button("help", |ui| {
                    plain(ui);
                    if ui.button("quick guide").clicked() {
                        self.show_guide = true;
                        ui.close();
                    }
                    if ui.button("about").clicked() {
                        self.show_about = true;
                        ui.close();
                    }
                });
            });
        });
    }

    fn load_catalogue_dialog(&mut self) {
        self.show_catalogue_info = true;
        self.focus_next = Some("catalogue-path");
    }

    fn controller_bar(&mut self, ui: &mut egui::Ui) {
        theme::sheet_frame(ui).inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
            let need = if ui.available_width() >= self.bar_need {
                ui.horizontal(|ui| {
                    ui.set_min_height(fields::HEIGHT);
                    let a = self.target_controls(ui);
                    let x = ui.cursor().min.x;
                    theme::vrule(ui, 32.0);
                    let rule = ui.cursor().min.x - x;
                    a + rule + self.action_controls(ui)
                })
                .inner
            } else {
                let a = ui
                    .horizontal(|ui| {
                        ui.set_min_height(fields::HEIGHT);
                        self.target_controls(ui)
                    })
                    .inner;
                ui.add_space(6.0);
                let b = ui
                    .horizontal(|ui| {
                        ui.set_min_height(fields::HEIGHT);
                        self.action_controls(ui)
                    })
                    .inner;
                a + 2.0 * ui.spacing().item_spacing.x + b
            };
            self.bar_need = need + BAR_SLACK;
        });
    }

    fn target_controls(&mut self, ui: &mut egui::Ui) -> f32 {
        let x0 = ui.cursor().min.x;
        let p = theme::pal(ui);
        let phase = self.session.status().phase.clone();
        let active = phase.is_active();
        ui.label(theme::b("controller").color(p.ink2));
        let host = ui
            .add_enabled_ui(!active, |ui| {
                let width = fields::width_for(ui, "192.168.125.1").max(170.0);
                fields::line(ui, &mut self.host_input, "Controller address", |t| t.hint_text("e.g. 192.168.125.1").desired_width(width))
                    .on_hover_text("The controller's IP address or name. A real IRC5 answers on port 5515. Enter connects.")
                    .on_disabled_hover_text("Disconnect to change the address.")
            })
            .inner;
        ui.label(theme::b("port").color(p.ink2));
        let port = ui
            .add_enabled_ui(!active, |ui| {
                let width = fields::width_for(ui, "65535");
                let r = fields::line(ui, &mut self.port_input, "Controller port", |t| t.desired_width(width))
                    .on_hover_text("5515 on an IRC5. A RobotStudio virtual controller picks a new port at every start: use the list. Enter connects.")
                    .on_disabled_hover_text("Disconnect to change the port.");
                self.target_menu(ui);
                r
            })
            .inner;
        if !active && (fields::entered(ui, &host) || fields::entered(ui, &port)) {
            self.connect();
        }
        match &phase {
            Phase::Idle | Phase::Stopped { .. } => {
                if theme::primary(ui, "connect", fields::HEIGHT).clicked() {
                    self.connect();
                }
            }
            _ => {
                if theme::primary(ui, "disconnect", fields::HEIGHT).clicked() {
                    self.session.disconnect();
                }
            }
        }
        ui.cursor().min.x - x0
    }

    fn action_controls(&mut self, ui: &mut egui::Ui) -> f32 {
        let x0 = ui.cursor().min.x;
        self.record_controls(ui);
        let recording = ui.cursor().min.x - x0;
        let dash = ui
            .with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (text, icon) = if self.dashboard { ("back to the charts", theme::Icon::Collapse) } else { ("live dashboard", theme::Icon::Grid) };
                let r = theme::icon_text_button(ui, icon, text, fields::HEIGHT, self.dashboard).on_hover_text("Big numbers in place of the charts, to read from a step away");
                if r.clicked() {
                    self.dashboard = !self.dashboard;
                }
                r.rect.width()
            })
            .inner;
        recording + ui.spacing().item_spacing.x + dash
    }

    fn target_menu(&mut self, ui: &mut egui::Ui) {
        let p = theme::pal(ui);
        let button = theme::drop_button(ui, "list", fields::HEIGHT).on_hover_text("Virtual controllers on this PC, saved controllers and recent ones").on_disabled_hover_text("Disconnect to pick another controller.");
        if button.clicked() && matches!(self.discovery, Discovery::Idle) {
            self.start_discovery();
        }
        let room = (ui.ctx().content_rect().height() - 160.0).max(200.0);
        egui::Popup::menu(&button).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
            ui.set_min_width(420.0);
            egui::ScrollArea::vertical().id_salt("controller-list").max_height(room).show(ui, |ui| {
                let row = |text: String| egui::Button::new(text).frame(true).min_size(egui::vec2(380.0, theme::TOOL_H));
                ui.label(theme::b("virtual controllers on this PC"));
                match &self.discovery {
                    Discovery::Idle => {
                        ui.label("Not searched yet.");
                    }
                    Discovery::Running(_) => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Searching...");
                        });
                    }
                    Discovery::Done(Err(e)) => {
                        ui.colored_label(p.red, e.as_str());
                    }
                    Discovery::Done(Ok(list)) => {
                        let list = list.clone();
                        let answering: Vec<&LocalController> = list.iter().filter(|c| c.hello.is_ok()).collect();
                        if answering.is_empty() {
                            ui.label("None found. Start the virtual controller in RobotStudio first.");
                        }
                        for c in answering {
                            let sys = c.hello.as_ref().ok().and_then(|a| a.system_id.clone()).unwrap_or_default();
                            let kind = if c.process.eq_ignore_ascii_case("RobVC.exe") { "RobotWare 6 VC" } else { "RobotWare 7 VC" };
                            if ui.add(row(format!("{kind}  127.0.0.1:{}  {}", c.port, short_id(&sys)))).clicked() {
                                self.host_input = "127.0.0.1".into();
                                self.port_input = c.port.to_string();
                                ui.close();
                            }
                        }
                        let silent = list.iter().filter(|c| c.hello.is_err()).count();
                        if silent > 0 {
                            ui.label(RichText::new(format!("{silent} other VC port(s) did not answer RobAPI (a RobotWare 7 VC does not serve it).")).small().weak());
                        }
                    }
                }
                if ui.add(egui::Button::new("search again").frame(true).min_size(egui::vec2(0.0, theme::TOOL_H))).clicked() {
                    self.start_discovery();
                }
                ui.separator();
                ui.label(theme::b("saved controllers"));
                if self.settings.controllers.is_empty() {
                    ui.label(RichText::new("None yet.").weak());
                }
                let mut remove = None;
                for (i, c) in self.settings.controllers.clone().iter().enumerate() {
                    ui.horizontal(|ui| {
                        if self.forget_ask == Some(i) {
                            ui.label(theme::b(format!("forget {}?", c.name)).color(p.red));
                            if theme::red_button(ui, egui::Button::new("forget it").min_size(egui::vec2(0.0, theme::TOOL_H))).clicked() {
                                remove = Some(i);
                                self.forget_ask = None;
                            }
                            if ui.add(egui::Button::new("keep it").frame(true).min_size(egui::vec2(0.0, theme::TOOL_H))).clicked() {
                                self.forget_ask = None;
                            }
                            return;
                        }
                        if ui.add(egui::Button::new(format!("{}  {}:{}", c.name, c.host, c.port)).frame(true).min_size(egui::vec2(332.0, theme::TOOL_H))).clicked() {
                            self.host_input = c.host.clone();
                            self.port_input = c.port.to_string();
                            ui.close();
                        }
                        if theme::icon_button(ui, theme::Icon::Close, "Forget this controller", egui::vec2(theme::TOOL_H, theme::TOOL_H)).clicked() {
                            self.forget_ask = Some(i);
                        }
                    });
                }
                if let Some(i) = remove {
                    self.settings.controllers.remove(i);
                    self.mark_settings_dirty();
                }
                ui.horizontal(|ui| {
                    let r = fields::line(ui, &mut self.name_input, "Name for the saved controller", |t| t.hint_text("name").desired_width(140.0));
                    if ui.add(egui::Button::new("save the address").min_size(egui::vec2(0.0, fields::HEIGHT))).clicked() || fields::entered(ui, &r) {
                        match self.parse_target() {
                            Ok(t) => {
                                let name = if self.name_input.trim().is_empty() { t.host.clone() } else { self.name_input.trim().to_string() };
                                self.settings.controllers.retain(|c| !(c.host == t.host && c.port == t.port));
                                self.settings.controllers.push(SavedController { name, host: t.host, port: t.port });
                                self.name_input.clear();
                                self.mark_settings_dirty();
                            }
                            Err(e) => self.toast(Level::Error, e),
                        }
                    }
                });
                if !self.settings.recent.is_empty() {
                    ui.separator();
                    ui.label(theme::b("recent"));
                    for t in self.settings.recent.clone() {
                        if ui.add(row(t.to_string())).clicked() {
                            self.host_input = t.host.clone();
                            self.port_input = t.port.to_string();
                            ui.close();
                        }
                    }
                }
            });
        });
    }

    pub fn start_discovery(&mut self) {
        if matches!(self.discovery, Discovery::Running(_)) {
            return;
        }
        let ctx = self.ctx.clone();
        let h = std::thread::spawn(move || {
            let r = discovery::local_controllers(Duration::from_millis(1500));
            ctx.request_repaint();
            r
        });
        self.discovery = Discovery::Running(h);
    }

    fn poll_background(&mut self) {
        if let Discovery::Running(h) = &self.discovery
            && h.is_finished()
                && let Discovery::Running(h) = std::mem::replace(&mut self.discovery, Discovery::Idle) {
                    self.discovery = Discovery::Done(h.join().unwrap_or_else(|_| Err("the search failed".into())));
                }
        if self.snapshot_job.as_ref().is_some_and(|j| j.is_finished())
            && let Some(j) = self.snapshot_job.take() {
                match j.join() {
                    Ok(Ok((dir, rows))) => {
                        self.toast(Level::Info, format!("Saved {rows} samples to {}", dir.display()));
                        if let Some((from, to)) = self.snapshot_span.take() {
                            self.rws_after_recording(dir.clone(), from, to);
                        }
                        self.last_folder = Some(dir);
                    }
                    Ok(Err(e)) => self.toast(Level::Error, format!("Could not save: {e}")),
                    Err(_) => self.toast(Level::Error, "Saving failed unexpectedly."),
                }
            }
    }

    fn update_rates(&mut self, st: &spy_core::session::Status) {
        let now = Instant::now();
        let dt = now.duration_since(self.rates.0).as_secs_f64();
        if dt >= 1.0 {
            self.rates = (now, st.counters.frames, st.counters.samples, (st.counters.frames.saturating_sub(self.rates.1)) as f64 / dt, (st.counters.samples.saturating_sub(self.rates.2)) as f64 / dt);
        }
    }

    pub fn phase_word(st: &spy_core::session::Status, p: &theme::Pal) -> (String, Color32, theme::Mark) {
        let (word, color) = match &st.phase {
            Phase::Idle => ("not connected".to_string(), p.ink2),
            Phase::Connecting => ("connecting".into(), p.hold),
            Phase::Handshaking => ("handshake".into(), p.hold),
            Phase::AwaitingApproval => ("waiting for your answer".into(), p.hold),
            Phase::SettingUp => ("setting up".into(), p.hold),
            Phase::Streaming if st.advice.is_some() => ("not receiving".into(), p.hold),
            Phase::Streaming => ("streaming".into(), p.live),
            Phase::Reconnecting { attempt, retry_in } => (format!("reconnecting, try {attempt} (every {:.0} s)", retry_in.as_secs_f64()), p.hold),
            Phase::TearingDown => ("disconnecting".into(), p.hold),
            Phase::Stopped { .. } => ("stopped".into(), p.red),
        };
        let mark = if matches!(st.phase, Phase::Idle | Phase::Stopped { .. }) { theme::Mark::Off } else { theme::Mark::On };
        (word, color, mark)
    }

    pub fn others_shown(st: &spy_core::session::Status) -> Vec<&spy_core::session::OtherClient> {
        st.others.iter().filter(|o| !o.pendant).collect()
    }

    fn notices(&self, st: &spy_core::session::Status, p: &theme::Pal) -> Vec<(String, Color32)> {
        let mut lines: Vec<(String, Color32)> = Vec::new();
        if self.others_open > 0 {
            let which = if self.others_open == 1 { "Another ABB Signal Spy window is open".to_string() } else { format!("{} other ABB Signal Spy windows are open", self.others_open) };
            lines.push((format!("{which}. Two windows on the same controller break each other's streams: only one program at a time can stream test signals. The windows share the settings and your notes: each saves only what it changed."), p.hold));
        }
        if let Phase::Stopped { reason } = &st.phase {
            lines.push((reason.clone(), p.red));
        }
        if let Some(a) = &st.advice {
            lines.push((a.clone(), p.hold));
        }
        let lost = st.counters.dropped_to_taps;
        if lost > 0 {
            lines.push((format!("{lost} samples lost by the recorder (the disk could not keep up)."), p.red));
        }
        lines
    }

    pub fn client_names(&self, others: &[&spy_core::session::OtherClient]) -> Vec<String> {
        let names = self.hostnames.lock().unwrap_or_else(|e| e.into_inner()).clone();
        others
            .iter()
            .map(|o| {
                let mut s = o.address.clone();
                if let Some(Some(h)) = names.get(&o.address) {
                    s.push_str(&format!(" ({h})"));
                }
                if o.same_pc {
                    s.push_str(" [this PC]");
                }
                let extra: Vec<String> = o.attributes.iter().filter(|(k, _)| k != "a").map(|(k, v)| format!("{k}={v}")).collect();
                if !extra.is_empty() {
                    s.push_str(&format!(" {}", extra.join(" ")));
                }
                s
            })
            .collect()
    }

    fn approval_dialog(&mut self, ctx: &egui::Context) {
        let st = self.session.status().clone();
        if st.phase != Phase::AwaitingApproval {
            return;
        }
        let others = Self::others_shown(&st);
        for o in &others {
            let c = ctx.clone();
            net::lookup(&self.hostnames, &o.address, move || c.request_repaint());
        }
        let names = self.client_names(&others);
        let modal = egui::Modal::new(egui::Id::new("approval")).show(ctx, |ui| {
            ui.set_max_width(540.0);
            ui.heading("other programs are connected to this controller");
            ui.add_space(6.0);
            for n in &names {
                ui.horizontal(|ui| {
                    theme::square(ui, theme::pal(ui).hold, 10.0);
                    ui.label(RichText::new(n).monospace());
                });
            }
            ui.add_space(6.0);
            ui.label("Another tool, such as RobotStudio or TuneMaster, may be streaming test signals from this controller. Only one program at a time can: taking InfoStream will stop its streams.");
            ui.label(RichText::new("RobotStudio counts as a client whenever it is connected, even when it is not streaming anything.").weak());
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if theme::red_button(ui, egui::Button::new("take InfoStream").min_size(egui::vec2(0.0, fields::HEIGHT))).clicked() {
                    self.session.answer(true, &st.others);
                }
                if theme::primary(ui, "cancel", fields::HEIGHT).on_hover_text("Esc").clicked() {
                    self.session.answer(false, &st.others);
                }
            });
        });
        if modal.should_close() {
            self.session.answer(false, &st.others);
        }
    }

    fn reset_dialog(&mut self, ctx: &egui::Context) {
        if !self.confirm_reset {
            return;
        }
        let modal = egui::Modal::new(egui::Id::new("reset")).show(ctx, |ui| {
            ui.set_max_width(480.0);
            ui.heading("reset InfoStream?");
            ui.label("This sends StreamUndefineAll, which removes EVERY program's test-signal streams on this controller: RobotStudio's, TuneMaster's, any other tool's, and this program's (which are then set up again).");
            ui.label("Use it only when the controller refuses new channels (\"no channel available\") because a program that crashed left its streams behind.");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if theme::red_button(ui, egui::Button::new("reset InfoStream").min_size(egui::vec2(0.0, fields::HEIGHT))).clicked() {
                    self.session.reset_infostream();
                    self.confirm_reset = false;
                }
                if ui.add(egui::Button::new("cancel").min_size(egui::vec2(0.0, fields::HEIGHT))).on_hover_text("Esc").clicked() {
                    self.confirm_reset = false;
                }
            });
        });
        if modal.should_close() {
            self.confirm_reset = false;
        }
    }

    fn info_windows(&mut self, ctx: &egui::Context) {
        let mut open = self.show_about;
        theme::window("about ABB Signal Spy", ctx).open(&mut open).resizable(false).vscroll(true).default_width(480.0).show(ctx, |ui| {
            ui.heading(format!("ABB Signal Spy {}", env!("CARGO_PKG_VERSION")));
            ui.label("Reads, charts and records the motion test signals an ABB IRC5 controller streams over RobAPI InfoStream.");
            ui.label("It only reads: it never commands motion, never writes RAPID, configuration or I/O, and never takes mastership.");
            ui.add_space(6.0);
            ui.label(RichText::new("Not affiliated with or endorsed by ABB. ABB and IRC5 are trademarks of ABB.").strong());
            ui.label("The InfoStream protocol is not a documented interface: this program was checked against real controllers, and a RobotWare update could change it. When something is off, this program says so rather than showing a stale value as live.");
            ui.add_space(6.0);
            ui.label("Copyright (C) 2026 Jon Sands. Free software under the GNU General Public License, version 3 or later: you may share and change it under its terms. It comes with ABSOLUTELY NO WARRANTY.");
            ui.label(RichText::new(format!("Catalogue: {} ({})", self.catalogue.title, self.catalogue.source)).weak());
            ui.label(RichText::new(format!("Set in {}: {}, under the SIL Open Font License 1.1.", theme::FACE, theme::face_copyright())).weak());
            ui.label(RichText::new(format!("Characters it lacks come from the faces egui brings: {}.", theme::FALLBACK_FACES)).weak());
            egui::CollapsingHeader::new("the font licences").icon(theme::collapse_icon).show(ui, |ui| {
                egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                    ui.label(RichText::new(theme::FACE_LICENCE).monospace().size(14.0));
                    ui.separator();
                    ui.label(RichText::new(theme::FALLBACK_LICENCES).monospace().size(14.0));
                });
            });
        });
        self.show_about = open;

        let mut open = self.show_guide;
        theme::window("quick guide", ctx).open(&mut open).vscroll(true).default_width(520.0).show(ctx, |ui| {
            ui.label(theme::b("1. Connect"));
            ui.label("Real IRC5: type its address (the port is 5515) and press connect. RobotStudio virtual controller: open the list beside the port and pick it; its port changes every time it starts.");
            ui.label(theme::b("2. Only one program at a time"));
            ui.label("A controller streams test signals to one program at a time. Close TuneMaster's signal logging or RobotStudio's signal tools first. If other programs are connected to a controller elsewhere, this program lists them and asks before taking over; a virtual controller on this PC is not asked about.");
            ui.label(theme::b("3. Add channels"));
            ui.label("Pick a signal in the list on the left, then add it. The dialog asks only what that signal needs: the robot, the axis, or nothing. 'add a set' adds a common group in one go (both DC links, one robot's torques). Up to 12 channels.");
            ui.label(theme::b("4. Read and chart"));
            ui.label("Values show on the right; click a channel there for its options: smoothing, the vertical scale, its chart. Smoothing changes only the screen. Space pauses the charts so you can look back through the last 10 minutes. A reading that stops updating is marked stale, never shown as live.");
            ui.label(theme::b("5. Record"));
            ui.label("record keeps every sample. 'save last' saves what just happened, even if nothing was recording. 'slow log' logs averages for runs of hours. The arrow beside each sets its name, seconds or interval. M drops a marker.");
            ui.label(theme::b("6. Look back"));
            ui.label("file > open a recording (or drop its folder on the window) charts it again, marked REVIEWING: not live. 'save' above the charts saves what is in view as a CSV or a picture, live or reviewed; a CSV always holds the samples as they came.");
            ui.add_space(6.0);
            ui.label(RichText::new("Angles are in degrees; a channel's options switch one to radians.").weak());
        });
        self.show_guide = open;

        let mut open = self.show_catalogue_info;
        let mut load_path: Option<String> = None;
        theme::window("catalogue", ctx).vscroll(true).open(&mut open).default_width(560.0).show(ctx, |ui| {
            ui.heading(&self.catalogue.title);
            ui.label(format!("Source: {}", self.catalogue.source));
            ui.label(&self.catalogue.measured_on);
            ui.label(RichText::new(&self.catalogue.caveat).color(theme::pal(ui).hold));
            ui.label(RichText::new(&self.catalogue.credits).weak());
            let named = self.catalogue.signals.iter().filter(|s| s.named).count();
            ui.label(format!("{} signal numbers, {named} with a named quantity.", self.catalogue.signals.len()));
            ui.separator();
            ui.label("Load a catalogue file (for another robot or RobotWare version):");
            let focus_path = self.focus_next == Some("catalogue-path");
            if focus_path {
                self.focus_next = None;
            }
            let id = egui::Id::new("catalogue-path");
            let mut path: String = ui.data_mut(|d| d.get_temp::<String>(id)).unwrap_or_default();
            ui.horizontal(|ui| {
                let r = fields::line(ui, &mut path, "Catalogue file", |t| t.hint_text("C:\\path\\to\\catalogue.json").desired_width(380.0));
                if focus_path {
                    r.request_focus();
                }
                if theme::primary(ui, "load", fields::HEIGHT).clicked() || fields::entered(ui, &r) {
                    load_path = Some(path.clone());
                }
            });
            ui.data_mut(|d| d.insert_temp(id, path));
        });
        self.show_catalogue_info = open;
        if let Some(p) = load_path {
            let p = PathBuf::from(p.trim().trim_matches('"'));
            match Catalogue::load(&p) {
                Ok(c) => {
                    self.toast(Level::Info, format!("Loaded the catalogue {} ({} signals).", p.display(), c.signals.len()));
                    self.catalogue = c;
                    self.settings.catalogue_file = Some(p);
                    self.mark_settings_dirty();
                    self.show_catalogue_info = false;
                }
                Err(e) => self.toast(Level::Error, e),
            }
        }

        let focus_folder = self.windows_opened_now.contains(&WindowKind::RecordDir);
        let mut open = self.show_record_dir;
        let (mut use_typed, mut use_default) = (false, false);
        theme::window("recordings folder", ctx).open(&mut open).vscroll(true).default_width(560.0).show(ctx, |ui| {
            ui.label(format!("Recordings, saved CSVs and pictures go to {}", self.record_dir().display()));
            ui.label(RichText::new("A change applies to the next recording: one running now carries on where it is.").weak());
            ui.add_space(6.0);
            let width = 520.0f32.min(ui.available_width());
            let r = fields::line(ui, &mut self.record_dir_input, "Recordings folder path", |t| t.hint_text("C:\\path\\to\\a folder").desired_width(width));
            if focus_folder {
                r.request_focus();
            }
            let entered = fields::entered(ui, &r);
            ui.horizontal_wrapped(|ui| {
                use_typed = theme::primary(ui, "use this folder", fields::HEIGHT).clicked() || entered;
                use_default = theme::lockable(ui, self.settings.record_dir.is_some(), egui::Button::new("back to Documents\\TestSignals").min_size(egui::vec2(0.0, fields::HEIGHT)))
                    .on_disabled_hover_text("Recordings already go to Documents\\TestSignals.")
                    .clicked();
                if ui.add(egui::Button::new("open it").min_size(egui::vec2(0.0, fields::HEIGHT))).clicked() {
                    let d = self.record_dir();
                    let _ = std::fs::create_dir_all(&d);
                    crate::paths::open_folder(&d);
                }
            });
        });
        self.show_record_dir = open;
        if use_typed {
            let p = PathBuf::from(self.record_dir_input.trim().trim_matches('"'));
            match crate::record::usable_folder(&p) {
                Ok(()) => {
                    self.settings.record_dir = Some(p.clone());
                    self.mark_settings_dirty();
                    self.toast(Level::Info, format!("Recordings now go to {}.", p.display()));
                    self.show_record_dir = false;
                }
                Err(e) => self.toast(Level::Error, e),
            }
        }
        if use_default {
            self.settings.record_dir = None;
            self.record_dir_input = self.record_dir().display().to_string();
            self.mark_settings_dirty();
            self.toast(Level::Info, format!("Recordings now go to {}.", self.record_dir().display()));
        }

        let mut open = self.show_diag;
        let rws_system = self.rws.as_ref().and_then(|l| l.system.as_ref()).map(|s| format!("{} · RobotWare {}", s.name, s.rw_version));
        let rates = (self.rates.3, self.rates.4);
        let others = {
            let st = self.session.status().clone();
            self.client_names(&Self::others_shown(&st)).join(", ")
        };
        theme::window("connection details", ctx).vscroll(true).open(&mut open).default_width(560.0).show(ctx, |ui| {
            let st = self.session.status().clone();
            let c = &st.counters;
            let (word, _, _) = Self::phase_word(&st, theme::pal(ui));
            egui::Grid::new("diag").striped(true).show(ui, |ui| {
                let mut row = |k: &str, v: String| {
                    ui.label(RichText::new(k).color(theme::pal(ui).ink2));
                    ui.label(RichText::new(v).monospace());
                    ui.end_row();
                };
                row("phase", match &st.phase {
                    Phase::Stopped { reason } => format!("{word}: {reason}"),
                    _ => word,
                });
                row("controller", st.target.as_ref().map(|t| t.to_string()).unwrap_or_default());
                if let Some(s) = &rws_system {
                    row("from its RWS", s.clone());
                }
                row("frames a second", format!("{:.0}", rates.0));
                row("samples a second", format!("{:.0}", rates.1));
                row("other programs", if others.is_empty() { "none".into() } else { others.clone() });
                row("local address", st.local.map(|a| a.to_string()).unwrap_or_default());
                row("system id", st.announce.as_ref().and_then(|a| a.system_id.clone()).unwrap_or_default());
                row("client list as sent", st.announce.as_ref().and_then(|a| a.raw_client_list.clone()).unwrap_or_default());
                row("subscription", st.subscription.map(|s| s.to_string()).unwrap_or_default());
                row("frames / bytes", format!("{} / {}", c.frames, c.bytes));
                row("sample frames / samples", format!("{} / {}", c.sample_frames, c.samples));
                row("keepalives answered", c.ayas.to_string());
                row("reconnect attempts", c.reconnects.to_string());
                row("lost frame sync", c.desyncs.to_string());
                row("frames without trailer", c.no_trailer.to_string());
                row("records for others' streams", c.foreign_records.to_string());
                row("other subscriptions' frames", c.foreign_subscription.to_string());
                row("unexpected frames", c.unexpected_frames.to_string());
                row("controller clock resets", c.clock_resets.to_string());
                row("lost by recorders", c.dropped_to_taps.to_string());
                for (k, v) in &c.defects {
                    row(k, v.to_string());
                }
            });
        });
        self.show_diag = open;
    }

    fn log_pane(&mut self, ui: &mut egui::Ui) {
        let p = theme::pal(ui);
        ui.horizontal(|ui| {
            ui.label(RichText::new("messages").font(egui::FontId::new(18.0, theme::bold())));
            theme::check(ui, &mut self.log_filter_warn, "warnings only");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::icon_button(ui, theme::Icon::Close, "Close the messages", egui::vec2(32.0, 30.0)).clicked() {
                    self.show_log = false;
                }
            });
        });
        let entries = self.log.since(self.log.next_seq().saturating_sub(400));
        egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
            ui.style_mut().interaction.selectable_labels = true;
            for e in entries.iter().filter(|e| !self.log_filter_warn || e.level >= Level::Warn) {
                let color = match e.level {
                    Level::Info => p.ink,
                    Level::Warn => p.hold,
                    Level::Error => p.red,
                };
                ui.label(RichText::new(view::log_line(e)).color(color).monospace());
            }
        });
    }

    fn footer(&mut self, ui: &mut egui::Ui) {
        let p = theme::pal(ui);
        let rect = ui.max_rect();
        ui.painter().hline(rect.x_range(), rect.top() + 1.0, egui::Stroke::new(2.0, p.ink));
        ui.add_space(2.0);
        let (said, folder): (Option<RichText>, Option<std::path::PathBuf>) = if let Some(h) = self.mouse_hint() {
            (Some(RichText::new(h).color(p.ink)), None)
        } else if let Some(r) = self.recorder.as_ref().or(self.slow.as_ref()) {
            let dir = r.status().dir;
            (Some(RichText::new(format!("recording to {}", dir.display())).color(p.ink)), Some(dir))
        } else if let Some(e) = self.log.since(self.log.next_seq().saturating_sub(1)).last().filter(|e| !self.toasts.iter().any(|t| t.text == e.text)) {
            let color = match e.level {
                Level::Info => p.ink2,
                Level::Warn => p.hold,
                Level::Error => p.red,
            };
            let folder = self.last_folder.clone().filter(|d| e.text.contains(&d.display().to_string()));
            (Some(RichText::new(&e.text).color(color)), folder)
        } else {
            (None, None)
        };
        let mut open_folder = None;
        ui.horizontal(|ui| {
            ui.set_min_height(30.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let n = self.log.next_seq();
                let text = format!("messages ({n})");
                if ui.add(egui::Button::new(theme::b(text)).selected(self.show_log).min_size(egui::vec2(0.0, FOOTER_BUTTON_H))).clicked() {
                    self.show_log = !self.show_log;
                }
                if let Some(d) = &folder
                    && ui.add(egui::Button::new(theme::b("open the folder")).min_size(egui::vec2(0.0, FOOTER_BUTTON_H))).on_hover_text(d.display().to_string()).clicked()
                {
                    open_folder = Some(d.clone());
                }
                if let Some(ph) = &self.phone {
                    ui.label(RichText::new(format!("phone view on, port {}", ph.port())).color(p.ink2)).on_hover_text(ph.urls.join("\n"));
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.label(theme::num(view::local_hms(std::time::SystemTime::now()), 16.0).color(p.red));
                    ui.add_space(4.0);
                    if let Some(text) = said {
                        ui.add(egui::Label::new(text).truncate());
                    }
                });
            });
        });
        if let Some(d) = open_folder {
            crate::paths::open_folder(&d);
        }
    }

    fn mouse_hint(&self) -> Option<String> {
        const MOVE: &str = "drag to move through time · wheel zooms time · ctrl + wheel zooms the vertical scale · double-click goes back";
        if let Some(rs) = &self.review {
            return Some(if rs.cursors_on { format!("click a chart for cursor A, right-click for B · {MOVE}") } else { MOVE.into() });
        }
        if self.dashboard {
            return None;
        }
        let apart = match (self.cursors_on, self.cursor_a, self.cursor_b) {
            (true, Some(a), Some(b)) => format!("A and B {:.3} s apart · ", (b - a).abs()),
            _ => String::new(),
        };
        if self.paused_at.is_some() {
            return Some(format!("{apart}paused. {MOVE} · space: live"));
        }
        if self.cursors_on {
            return Some(format!("{apart}click a chart for cursor A, right-click for B · space pauses"));
        }
        None
    }

    fn toasts(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        let held = ctx.pointer_hover_pos().is_some_and(|at| self.toast_rect.is_some_and(|r| r.contains(at)));
        if held {
            for t in &mut self.toasts {
                t.shown = Some(now);
            }
        }
        self.toasts.retain_mut(|t| now.duration_since(*t.shown.get_or_insert(now)) < toast_life(t.level));
        if self.toasts.is_empty() {
            self.toast_rect = None;
            return;
        }
        let over = self.charts_rect.unwrap_or_else(|| ctx.content_rect());
        let width = (over.width() - 32.0).clamp(200.0, 440.0);
        let shown = egui::Area::new(egui::Id::new("toasts")).pivot(egui::Align2::CENTER_BOTTOM).fixed_pos(egui::pos2(over.center().x, over.bottom() - TOAST_ABOVE)).show(ctx, |ui| {
            let p = theme::pal(ui);
            let mut closed = None;
            for (i, t) in self.toasts.iter().enumerate() {
                let color = match t.level {
                    Level::Info => p.live,
                    Level::Warn => p.hold,
                    Level::Error => p.red,
                };
                let r = egui::Frame::popup(ui.style())
                    .stroke(egui::Stroke::new(2.0, color))
                    .inner_margin(egui::Margin::same(12))
                    .show(ui, |ui| {
                        ui.set_max_width(width);
                        ui.label(&t.text);
                    })
                    .response
                    .interact(egui::Sense::click())
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_text("Click to close. It stays while the pointer is on it; every message is in the messages pane too.");
                if r.clicked() {
                    closed = Some(i);
                }
            }
            closed
        });
        self.toast_rect = Some(shown.response.rect);
        if let Some(i) = shown.inner {
            self.toasts.remove(i);
        }
        ctx.request_repaint_after(Duration::from_millis(500));
    }

    pub fn dialog_open(&self) -> bool {
        self.add.is_some() || self.note_edit.is_some() || self.sets.is_some() || self.confirm_reset || self.confirm_remove_all || self.session.status().phase == Phase::AwaitingApproval
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() || self.dialog_open() {
            return;
        }
        let (space, m, esc) = ctx.input(|i| (i.key_pressed(egui::Key::Space), i.key_pressed(egui::Key::M), i.key_pressed(egui::Key::Escape)));
        let popup_open = egui::Popup::is_any_open(ctx) || ctx.any_popup_open();
        if esc && !popup_open {
            if self.selected.is_some() {
                self.selected = None;
            } else if let Some(top) = self.window_stack.pop() {
                self.close_window(top);
            } else if self.options_for.is_some() {
                self.options_for = None;
            } else if self.expanded.is_some() {
                self.expanded = None;
            }
        }
        if self.dashboard && self.review.is_none() {
            if m {
                self.add_marker();
            }
            return;
        }
        if self.review.is_some() {
            if m {
                self.toast(Level::Info, "M puts a marker on the live charts: close the recording under review first.");
            }
            return;
        }
        if space {
            self.toggle_pause();
        }
        if m {
            self.add_marker();
        }
    }

    fn windows_open(&self) -> [(WindowKind, bool); 9] {
        [
            (WindowKind::About, self.show_about),
            (WindowKind::Guide, self.show_guide),
            (WindowKind::Catalogue, self.show_catalogue_info),
            (WindowKind::RecordDir, self.show_record_dir),
            (WindowKind::Diag, self.show_diag),
            (WindowKind::Recordings, self.show_recordings),
            (WindowKind::Rws, self.show_rws),
            (WindowKind::Compare, self.compare.is_some()),
            (WindowKind::Xy, self.xy.is_some()),
        ]
    }

    pub fn track_windows(&mut self) {
        let open = self.windows_open();
        self.window_stack.retain(|k| open.iter().any(|(o, on)| o == k && *on));
        self.windows_opened_now.clear();
        for (k, on) in open {
            if on && !self.window_stack.contains(&k) {
                self.window_stack.push(k);
                self.windows_opened_now.push(k);
            }
        }
    }

    fn close_window(&mut self, k: WindowKind) {
        match k {
            WindowKind::About => self.show_about = false,
            WindowKind::Guide => self.show_guide = false,
            WindowKind::Catalogue => self.show_catalogue_info = false,
            WindowKind::RecordDir => self.show_record_dir = false,
            WindowKind::Diag => self.show_diag = false,
            WindowKind::Recordings => self.show_recordings = false,
            WindowKind::Rws => self.show_rws = false,
            WindowKind::Compare => self.compare = None,
            WindowKind::Xy => self.xy = None,
        }
    }

    pub fn toggle_pause(&mut self) {
        if self.paused_at.is_some() {
            self.paused_at = None;
        } else {
            self.paused_at = self.session.store().newest();
            self.pause_fresh = true;
        }
    }

    pub fn add_marker(&mut self) {
        let Some(t) = self.session.store().newest() else {
            self.toast(Level::Warn, "No data yet to put a marker on.");
            return;
        };
        let label = if self.marker_text.trim().is_empty() { format!("M{}", self.markers.len() + 1) } else { self.marker_text.trim().to_string() };
        let controller_ms = Some(self.session.status().timeline.controller_ms(t));
        if let Some(r) = &self.recorder {
            r.marker(&label, controller_ms);
        }
        if let Some(r) = &self.slow {
            r.marker(&label, controller_ms);
        }
        self.log.info(format!("Marker \"{label}\"."));
        self.markers.push(Marker { t_ms: t, label });
        self.marker_text.clear();
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let st = self.session.status();
        let state = title_word(&st.phase, st.advice.is_some());
        let asking = st.phase == Phase::AwaitingApproval;
        let mut t = String::from("ABB Signal Spy");
        if !state.is_empty() {
            t.push_str(&format!(" · {state}"));
            if let Some(target) = &st.target {
                t.push_str(&format!(" {target}"));
            }
        }
        drop(st);
        if self.recorder.is_some() {
            t.push_str(" · REC");
        }
        if self.slow.is_some() {
            t.push_str(" · SLOW LOG");
        }
        if t != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(t.clone()));
            self.title = t;
        }
        if asking && !self.attention_asked {
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(egui::UserAttentionType::Critical));
        }
        self.attention_asked = asking;
    }

    fn publish_phone(&mut self) {
        if self.phone.is_none() || self.phone_built.elapsed() < Duration::from_millis(250) {
            return;
        }
        self.phone_built = Instant::now();
        let st = self.session.status().clone();
        let connected = view::session_live(&st.phase);
        let mut chans = Vec::new();
        for c in &self.chans {
            let sig = self.catalogue.get(c.key.signal);
            let cs = st.channels.iter().find(|x| x.key == c.key);
            let h = view::health(cs, connected, sig, st.loopback);
            let d = view::display(sig, c.radians).with_decimals(c.decimals);
            let reading = view::reading(sig);
            let value = self.session.store().get(&c.key).and_then(|ch| {
                let r = ch.lock();
                if r.kind == Some(spy_core::sample::ValueKind::String) { r.last_text.clone() } else { view::readout(&r, reading).map(|v| d.fmt(v)) }
            });
            chans.push(serde_json::json!({
                "name": view::label(&self.catalogue, &c.key),
                "value": value.unwrap_or_else(|| "--".into()),
                "units": d.units,
                "stale": !h.is_live(),
                "status": h.word(),
            }));
        }
        for i in 0..self.derived.len() {
            let h = self.derived_health(i, &st);
            let def = self.derived[i].live.def();
            let v = view::readout(&self.derived[i].live.lock(), crate::derived_view::reading(def));
            let (value, on_target) = crate::derived_view::value_text(def, v, h.is_live());
            let (number, _) = crate::derived_view::value_text(def, v, false);
            chans.push(serde_json::json!({
                "name": self.derived_label(def),
                "value": value,
                "units": if on_target { "" } else { def.units() },
                "stale": !h.is_live(),
                "status": h.word(),
                "on_target": on_target,
                "number": number,
                "number_units": def.units(),
            }));
        }
        let (state, _, _) = Self::phase_word(&st, theme::pal_of(self.settings.dark));
        let receiving = st.phase == Phase::Streaming && st.advice.is_none();
        let body = serde_json::json!({ "controller": st.target.map(|t| t.to_string()).unwrap_or_default(), "state": state, "streaming": receiving, "channels": chans });
        let mut g = self.phone_snapshot.lock().unwrap_or_else(|e| e.into_inner());
        g.body = body.to_string();
        g.built = Some(Instant::now());
    }
}

pub fn move_to_follow(moved: Option<(Target, Target)>, moves: u64, seen: u64, connected_to: Option<&Target>) -> Option<(Target, Target)> {
    if moves == seen {
        return None;
    }
    let (from, to) = moved?;
    (connected_to == Some(&from)).then_some((from, to))
}

pub fn title_word(phase: &Phase, advice: bool) -> &'static str {
    match phase {
        Phase::Streaming if advice => "NOT RECEIVING",
        Phase::Streaming => "STREAMING",
        Phase::Reconnecting { .. } => "RECONNECTING",
        Phase::Stopped { .. } => "STOPPED",
        Phase::Idle => "",
        Phase::AwaitingApproval => "WAITING FOR YOUR ANSWER",
        Phase::TearingDown => "DISCONNECTING",
        Phase::Connecting | Phase::Handshaking | Phase::SettingUp => "CONNECTING",
    }
}

pub fn short_id(id: &str) -> String {
    let t = id.trim_matches(['{', '}']);
    if t.chars().count() > 8 { format!("{{{}…}}", t.chars().take(8).collect::<String>()) } else { id.to_string() }
}

impl eframe::App for SpyApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_background();
        self.poll_stopping();
        self.check_other_windows();
        self.learn_units();
        self.poll_review();
        self.poll_export();
        self.poll_rws();
        self.follow_moved_controller();
        self.update_stats();
        self.update_derived();
        self.check_recorders();
        self.publish_phone();
        self.update_title(ctx);
        if self.settings_dirty.is_some_and(|t| t.elapsed() > Duration::from_secs(2)) {
            self.save_settings();
        }
        ctx.request_repaint_after(Duration::from_millis(250));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.take_dropped(&ctx);
        self.take_screenshot(&ctx);
        self.track_windows();
        if self.settings.signals_folded || (self.dashboard && self.review.is_none()) {
            self.selected = None;
        }
        self.shortcuts(&ctx);

        let p = theme::pal(ui);
        let page = egui::Frame::new().fill(p.page);
        egui::Panel::top("menu").frame(page.inner_margin(egui::Margin::symmetric(4, 1))).show_separator_line(false).show(ui, |ui| {
            self.menu(ui);
            let r = ui.max_rect();
            ui.painter().hline(r.x_range(), r.bottom() + 1.0, egui::Stroke::new(1.0, p.line));
        });
        egui::Panel::top("bar").frame(page.inner_margin(egui::Margin { left: 8, right: 8, top: 8, bottom: 8 })).show_separator_line(false).show(ui, |ui| self.controller_bar(ui));
        let st = self.session.status().clone();
        self.update_rates(&st);
        let notices = self.notices(&st, p);
        if !notices.is_empty() {
            egui::Panel::top("notices").frame(page.inner_margin(egui::Margin { left: 8, right: 8, top: 0, bottom: 8 })).show_separator_line(false).show(ui, |ui| {
                for (text, color) in notices {
                    ui.horizontal(|ui| {
                        theme::square(ui, color, 10.0);
                        ui.add(egui::Label::new(theme::b(text).color(color)).wrap());
                    });
                }
            });
        }
        egui::Panel::bottom("footer").frame(page.inner_margin(egui::Margin { left: 16, right: 8, top: 0, bottom: 2 })).show_separator_line(false).show(ui, |ui| self.footer(ui));
        let sheet = |left: i8, right: i8| egui::Frame::new().fill(p.sheet).inner_margin(egui::Margin::same(10)).outer_margin(egui::Margin { left, right, top: 0, bottom: 8 });
        let column = egui::Frame::new().outer_margin(egui::Margin { left: 8, right: 0, top: 0, bottom: 8 });
        if self.show_log {
            let tall = ctx.content_rect().height();
            egui::Panel::bottom("log").frame(sheet(8, 8)).resizable(true).default_size(LOG_OPENING.min(tall * LOG_OPENING_SHARE)).size_range(LOG_LEAST..=(tall * LOG_MOST_SHARE).max(LOG_LEAST)).show_separator_line(false).show(ui, |ui| self.log_pane(ui));
        }
        if self.dashboard && self.review.is_none() {
            let central = egui::CentralPanel::default().frame(sheet(8, 8)).show(ui, |ui| self.dashboard_ui(ui));
            self.charts_rect = Some(central.response.rect);
            self.follow_live_view();
        } else {
            if self.settings.signals_folded {
                egui::Panel::left("signals-folded").frame(column).exact_size(60.0).resizable(false).show_separator_line(false).show(ui, |ui| {
                    let rail = egui::Frame::new().fill(p.sheet).inner_margin(egui::Margin::symmetric(6, 10));
                    rail.show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        if self.review.is_some() {
                            self.review_rail(ui);
                        } else {
                            self.status_rail(ui);
                        }
                    });
                    ui.add_space(8.0);
                    rail.show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.set_min_height(ui.available_height());
                        self.signals_strip(ui);
                    });
                });
            } else {
                egui::Panel::left("signals").frame(column).resizable(true).default_size(signals_opening(ctx.content_rect().width())).size_range(SIGNALS_LEAST..=side_most(ctx.content_rect().width(), SIGNALS_LEAST, SIGNALS_MOST)).show_separator_line(false).show(ui, |ui| {
                    let top = theme::sheet_frame(ui).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        if self.review.is_some() {
                            self.review_block(ui);
                            None
                        } else {
                            Some(self.status_block(ui))
                        }
                    });
                    if let Some(rule) = top.inner {
                        let r = top.response.rect;
                        ui.painter().hline(r.x_range(), r.top() + 1.0, egui::Stroke::new(2.0, rule));
                    }
                    ui.add_space(8.0);
                    theme::sheet_frame(ui).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.set_min_height(ui.available_height());
                        self.signals_rect = Some(ui.max_rect());
                        let room = ui.available_height();
                        egui::ScrollArea::vertical().id_salt("signals-sheet").auto_shrink([false, true]).show(ui, |ui| self.browser(ui, room));
                    });
                });
            }
            egui::Panel::right("channels").frame(sheet(0, 8)).resizable(true).default_size(channels_opening(ctx.content_rect().width())).size_range(CHANNELS_LEAST..=side_most(ctx.content_rect().width(), CHANNELS_LEAST, CHANNELS_MOST)).show_separator_line(false).show(ui, |ui| {
                if self.review.is_some() {
                    self.review_table(ui);
                } else if self.options_for.is_some() {
                    self.channel_options(ui);
                } else {
                    self.channel_table(ui);
                }
            });
            let central = egui::CentralPanel::default().frame(sheet(8, 8)).show(ui, |ui| {
                if self.review.is_some() {
                    self.review_charts(ui);
                } else {
                    self.charts(ui);
                }
            });
            self.charts_rect = Some(central.response.rect);
            if !self.settings.signals_folded {
                self.signal_details(&ctx);
            }
        }

        self.recordings_window(&ctx);
        self.rws_window(&ctx);
        self.compare_window(&ctx);
        self.xy_window(&ctx);
        self.add_dialog(&ctx);
        self.notes_dialog(&ctx);
        self.sets_dialog(&ctx);
        self.approval_dialog(&ctx);
        self.reset_dialog(&ctx);
        self.remove_all_dialog(&ctx);
        self.info_windows(&ctx);
        self.toasts(&ctx);

        if self.session.status().phase == Phase::Streaming && self.paused_at.is_none() {
            ctx.request_repaint_after(Duration::from_millis(33));
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.shutdown();
    }
}

impl SpyApp {
    pub fn shutdown(&mut self) {
        if let Some(r) = self.recorder.take() {
            let s = r.stop();
            self.log.info(format!("Recording finished: {} rows in {}", s.rows, s.dir.display()));
        }
        if let Some(r) = self.slow.take() {
            r.stop();
        }
        for (_, s) in std::mem::take(&mut self.stopping) {
            if let Some(s) = s.wait() {
                self.log.info(format!("Recording finished: {} rows in {}", s.rows, s.dir.display()));
            }
        }
        let until = Instant::now() + CLOSE_PATIENCE;
        if let Some(job) = self.snapshot_job.take() {
            while !job.is_finished() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(20));
            }
            if job.is_finished() {
                match job.join() {
                    Ok(Ok((dir, rows))) => self.log.info(format!("Saved {rows} samples to {}", dir.display())),
                    Ok(Err(e)) => self.log.warn(format!("Could not save: {e}")),
                    Err(_) => self.log.warn("Saving failed unexpectedly."),
                }
            } else {
                self.log.warn(format!("A save of the last seconds was still writing {:.0} s after the window closed: its folder keeps what was written, marked as cut short.", CLOSE_PATIENCE.as_secs_f64()));
            }
        }
        if let Some(job) = self.export_job.take() {
            while !job.is_finished() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(20));
            }
            if !job.is_finished() {
                self.export_stop.store(true, std::sync::atomic::Ordering::SeqCst);
                self.log.info("A CSV being saved was stopped: it was still writing well after the window closed.");
            }
            match job.join() {
                Ok(Ok((p, n))) => self.log.info(format!("Saved {n} samples in view to {}", p.display())),
                Ok(Err(e)) => self.log.warn(format!("Could not save: {e}")),
                Err(_) => self.log.warn("Saving failed unexpectedly."),
            }
        }
        self.phone = None;
        self.save_settings();
        self.session.disconnect();
    }

    pub fn poll_stopping(&mut self) {
        let mut done = Vec::new();
        let mut i = 0;
        while i < self.stopping.len() {
            if self.stopping[i].1.is_finished() {
                done.push(self.stopping.remove(i));
            } else {
                i += 1;
            }
        }
        for (what, s) in done {
            let Some(s) = s.wait() else {
                self.toast(Level::Error, "Closing a recording failed unexpectedly.");
                continue;
            };
            let slow = s.kind == spy_core::recording::Kind::Slow;
            match &s.state {
                spy_core::recording::RecState::Failed(e) => self.toast(Level::Error, format!("The {} ended with an error: {e}", if slow { "slow log" } else { "recording" })),
                _ if slow => self.toast(Level::Info, format!("Slow log: {} rows in {}", s.rows, s.dir.display())),
                _ => self.toast(Level::Info, format!("Recorded {} rows to {}", s.rows, s.dir.display())),
            }
            if what == "stopped" {
                let to = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
                self.rws_after_recording(s.dir.clone(), to - s.started.elapsed().as_millis() as i64, to);
            }
            if !slow {
                self.last_folder = Some(s.dir);
            }
        }
    }

    fn learn_units(&mut self) {
        let accepted: Vec<String> = self.session.status().channels.iter().filter(|c| matches!(c.state, spy_core::session::ChannelState::Defined { .. })).map(|c| c.key.unit.to_string()).collect();
        for u in accepted {
            if !self.settings.units.iter().any(|k| k.eq_ignore_ascii_case(&u)) {
                self.settings.units.push(u);
                self.mark_settings_dirty();
            }
        }
    }

    fn check_other_windows(&mut self) {
        if self.others_checked.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
            return;
        }
        self.others_checked = Some(Instant::now());
        if let Some(n) = self.windows.others() {
            self.others_open = n;
        }
    }

    pub fn follow_live_view(&mut self) {
        if self.paused_at.is_some() {
            return;
        }
        if let Some(newest) = self.session.store().newest() {
            let width = (self.window_s * 1000.0).round() as i64;
            self.view_ms = Some((newest.saturating_sub(width), newest.saturating_add(1)));
        }
    }
}

impl Drop for SpyApp {
    fn drop(&mut self) {
        if let Some(r) = self.recorder.take() {
            let _ = r.stop();
        }
        if let Some(r) = self.slow.take() {
            let _ = r.stop();
        }
        for (_, s) in std::mem::take(&mut self.stopping) {
            let _ = s.wait();
        }
        if let Some(job) = self.snapshot_job.take() {
            let until = Instant::now() + CLOSE_PATIENCE;
            while !job.is_finished() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{view, Stats};

    #[test]
    fn a_wrapping_angles_mean_since_reset_is_taken_on_the_circle() {
        let mut s = Stats::default();
        for a in [std::f64::consts::TAU - 0.01, 0.01, std::f64::consts::TAU - 0.02, 0.02] {
            s.n += 1;
            s.sum += a;
            s.sum_sin += a.sin();
            s.sum_cos += a.cos();
        }
        let m = s.mean(view::Reading::Wrapping).unwrap();
        assert!(m < 1e-9 || std::f64::consts::TAU - m < 1e-9, "{m}");
        assert!((s.mean(view::Reading::Plain).unwrap() - std::f64::consts::PI).abs() < 1e-9);
        assert_eq!(Stats::default().mean(view::Reading::Plain), None);
    }

    #[test]
    fn a_restart_is_followed_once_and_never_one_from_before_the_last_connect() {
        use spy_core::session::Target;
        let (a, b) = (Target { host: "127.0.0.1".into(), port: 61000 }, Target { host: "127.0.0.1".into(), port: 61002 });
        assert_eq!(super::move_to_follow(Some((a.clone(), b.clone())), 1, 0, Some(&a)), Some((a.clone(), b.clone())), "a new restart of the controller connected to");
        assert_eq!(super::move_to_follow(Some((a.clone(), b.clone())), 1, 1, Some(&a)), None, "seen already, or from before a connect by hand to the old port");
        assert_eq!(super::move_to_follow(Some((a.clone(), b.clone())), 2, 1, Some(&b)), None, "another controller's");
        assert_eq!(super::move_to_follow(None, 2, 1, Some(&a)), None);
    }

    #[test]
    fn the_title_never_says_connecting_while_disconnecting_or_asking() {
        use spy_core::session::Phase;
        assert_eq!(super::title_word(&Phase::TearingDown, false), "DISCONNECTING");
        assert_eq!(super::title_word(&Phase::AwaitingApproval, false), "WAITING FOR YOUR ANSWER");
        assert_eq!(super::title_word(&Phase::Streaming, true), "NOT RECEIVING");
        assert_eq!(super::title_word(&Phase::Handshaking, false), "CONNECTING");
    }

    #[test]
    fn short_ids_never_split_a_character() {
        assert_eq!(super::short_id("{12345678-9ABC-4DEF-8123-456789ABCDEF}"), "{12345678…}");
        assert_eq!(super::short_id("{abc}"), "{abc}");
        assert_eq!(super::short_id("{AÄÄÄÄÄÄÄÄÄ}"), "{AÄÄÄÄÄÄÄ…}");
        assert_eq!(super::short_id(""), "");
    }
}
