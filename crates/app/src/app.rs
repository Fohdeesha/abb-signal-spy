//! The window: state, layout, the controller bar, the session line, dialogs and the
//! log pane. The catalogue browser, the channel table, the charts and the recording
//! controls live in their own modules as further `impl SpyApp` blocks.

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

/// One channel as the window keeps it.
pub struct ChanView {
    pub key: ChannelKey,
    pub color: Color32,
    pub radians: bool,
    pub hold_nonzero: bool,
    pub lane: u32,
    pub stats: Stats,
    /// Display only: its line and value smoothed over this many ms (0: off).
    pub smooth_ms: u32,
    /// Its chart's vertical scale (the first channel's, for an overlaid chart).
    pub scale: crate::charts::Scale,
}

impl ChanView {
    pub fn new(key: ChannelKey, color: Color32, lane: u32) -> ChanView {
        ChanView { key, color, radians: false, hold_nonzero: false, lane, stats: Stats::default(), smooth_ms: 0, scale: crate::charts::Scale::default() }
    }
}

/// min / max / mean since the last reset, in the native unit, of the samples as the
/// signal means them (a zero-filled signal's padding undone; see [`view::Reading`]).
#[derive(Debug, Clone, Copy)]
pub struct Stats {
    pub n: u64,
    pub sum: f64,
    pub min: f64,
    pub max: f64,
    /// For a wrapping angle's mean, taken on the circle.
    pub sum_sin: f64,
    pub sum_cos: f64,
    /// A zero-filled signal's padding, undone across frames.
    pub hold: Option<view::ZeroHold>,
    /// Newest timeline ms already counted.
    pub upto: i64,
}

impl Default for Stats {
    fn default() -> Stats {
        Stats { n: 0, sum: 0.0, min: f64::INFINITY, max: f64::NEG_INFINITY, sum_sin: 0.0, sum_cos: 0.0, hold: None, upto: i64::MIN }
    }
}

impl Stats {
    /// The mean since reset, in the native unit.
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
    /// When it was first drawn: it stays six seconds from then, so one said while the
    /// window was minimised is still there when the window is shown again.
    pub shown: Option<Instant>,
    pub text: String,
    pub level: Level,
}

/// Toasts on screen at once; older ones are in the log pane. Unseen, they pile up
/// while the window is minimised (a virtual controller restarting again and again).
const MAX_TOASTS: usize = 8;

/// What the add-channel dialog is adding.
pub struct AddDialog {
    pub signal: u32,
    pub unit: String,
    pub axis: u8,
}

pub struct SpyApp {
    pub ctx: egui::Context,
    pub session: Session,
    /// The connection worker's options, for starting a new one (the product's: the
    /// defaults, which ask before taking InfoStream only on a remote controller).
    pub session_opts: Options,
    /// The address the session was last asked to connect to; it follows the session
    /// to a restarted virtual controller's new port.
    pub connected_to: Option<Target>,
    pub log: Arc<LogBook>,
    pub catalogue: Catalogue,
    pub settings: Settings,
    pub settings_path: PathBuf,
    pub settings_dirty: Option<Instant>,
    pub chans: Vec<ChanView>,
    pub next_lane: u32,
    /// The store's epoch the statistics were counted in.
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
    /// Derived channels, computed from channels in `chans`.
    pub derived: Vec<crate::derived_view::DerivedView>,
    /// How far back a sag's plateau compares the DC link's level: 20 s. The tests
    /// shorten it, with a link that drains as much faster.
    pub plateau_trend_ms: i64,
    /// Each chart's mapping from its data to the screen, as drawn in the last frame
    /// (live or reviewed): where a test points to hover a place on a chart.
    pub lane_transforms: Vec<egui_plot::PlotTransform>,
    /// The text the charts' hover showed last (a tooltip's text is not in the
    /// accessibility tree, so this is how a test reads it).
    pub hover_text: std::sync::Arc<std::sync::Mutex<String>>,

    /// The RWS extras: a logged-in session, the login being typed (never saved), how
    /// often the event log is looked at, the window, and the events for the charts.
    pub rws: Option<crate::rws_view::RwsLink>,
    pub rws_form: crate::rws_view::RwsForm,
    pub rws_poll: Duration,
    pub show_rws: bool,
    pub controller_events: Vec<crate::rws_view::ControllerEvent>,
    /// Recordings just closed that controller events still on their way belong to.
    pub late_windows: Vec<crate::rws_view::LateWindow>,
    /// The stretch (UTC ms) a "Save last" being written covers.
    pub snapshot_span: Option<(i64, i64)>,

    pub window_s: f64,
    pub paused_at: Option<i64>,
    /// Set when pausing: the next frame fixes the charts on the paused window, and
    /// after that the person scrolls and zooms freely.
    pub pause_fresh: bool,
    pub cursors_on: bool,
    pub cursor_a: Option<f64>,
    pub cursor_b: Option<f64>,
    pub markers: Vec<Marker>,
    /// A chart's vertical scale as Ctrl + wheel left it, by (lane, display unit) (see
    /// `charts::lanes`), until "reset scale" or a double-click.
    pub lane_zoom: HashMap<(u32, String), (f64, f64)>,
    /// The vertical range each chart showed last.
    pub lane_ranges: HashMap<(u32, String), (f64, f64)>,
    /// The wheel over a live chart, between notches.
    pub wheel_acc: f32,

    pub recorder: Option<Recorder>,
    pub slow: Option<Recorder>,
    pub snapshot_job: Option<JoinHandle<Result<(PathBuf, u64), String>>>,
    pub marker_text: String,
    pub rec_label: String,
    pub last_folder: Option<PathBuf>,

    pub phone: Option<PhoneServer>,
    pub phone_snapshot: Arc<Mutex<Snapshot>>,
    pub phone_built: Instant,

    /// The live charts' stretch in view (timeline ms), and where the charts are on
    /// screen, for saving what is in view; a picture of them asked for.
    pub view_ms: Option<(i64, i64)>,
    pub charts_rect: Option<egui::Rect>,
    /// Where the signal list is: its details open beside it.
    pub signals_rect: Option<egui::Rect>,
    pub png_pending: Option<crate::export::Picture>,
    /// The XY plot, while its window is open, and where its plot is on screen.
    pub xy: Option<crate::xy::XyState>,
    pub xy_rect: Option<egui::Rect>,
    /// The XY window around it, while a plot is drawn: what its Save PNG keeps.
    pub xy_window_rect: Option<egui::Rect>,
    /// The compare window (one channel against every other charted one), while open.
    pub compare: Option<crate::compare::CompareState>,
    /// The person's own notes on signals, and the editor while open.
    pub notes: crate::notes::Notes,
    pub note_edit: Option<crate::notes::Edit>,
    /// A CSV being written, and what to add to its "saved" message.
    pub export_job: Option<crate::export::ExportJob>,
    pub export_note: &'static str,
    /// Set to stop the save under way (the window closing): it leaves nothing behind.
    pub export_stop: Arc<std::sync::atomic::AtomicBool>,

    /// A recording open for review, and one being opened.
    pub review: Option<crate::review_view::ReviewState>,
    pub review_job: Option<crate::review_view::ReviewJob>,
    pub show_recordings: bool,
    pub recordings_list: Option<Vec<(PathBuf, spy_core::recording::Meta)>>,
    pub recording_path_input: String,

    /// The channel (its id) whose options fill the right panel instead of the list.
    pub options_for: Option<String>,
    /// The chart (lane and unit) filling the middle on its own.
    pub expanded: Option<(u32, String)>,
    /// The big numbers in place of the charts (G47).
    pub dashboard: bool,
    /// The messages (the log) open above the footer.
    pub show_log: bool,

    pub confirm_reset: bool,
    pub show_about: bool,
    pub show_diag: bool,
    pub show_guide: bool,
    pub show_catalogue_info: bool,
    pub hostnames: Hostnames,
    pub toasts: Vec<Toast>,
    pub rates: (Instant, u64, u64, f64, f64),
    pub another_instance: bool,
    pub log_filter_warn: bool,
    pub title: String,
}

pub fn chan_color(i: usize, dark: bool) -> Color32 {
    theme::channel_color(i, dark)
}

impl SpyApp {
    pub fn new(cc: &eframe::CreationContext<'_>, data_dir: PathBuf, another_instance: bool) -> SpyApp {
        SpyApp::with_options(cc, data_dir, another_instance, Options::default())
    }

    /// `opts`: the connection worker's. The product always uses the defaults; the
    /// tests ask on loopback too, to reach the question, and hand in their own
    /// virtual controllers rather than look for this PC's.
    pub fn with_options(cc: &eframe::CreationContext<'_>, data_dir: PathBuf, another_instance: bool, opts: Options) -> SpyApp {
        let ctx = cc.egui_ctx.clone();
        let settings_path = data_dir.join("settings.json");
        let first_run = !settings_path.exists();
        let (settings, note) = Settings::load(&settings_path);
        let (notes, notes_note) = crate::notes::Notes::load(&data_dir.join(crate::notes::FILE));
        theme::install_fonts(&ctx);
        theme::apply(&ctx, settings.dark, settings.ui_scale);

        let log = Arc::new(LogBook::new());
        if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(data_dir.join("signal-spy.log")) {
            // Keep the file bounded: start afresh past 5 MB.
            if f.metadata().map(|m| m.len() > 5 * 1024 * 1024).unwrap_or(false) {
                if let Ok(f2) = std::fs::File::create(data_dir.join("signal-spy.log")) {
                    log.attach_file(f2);
                }
            } else {
                log.attach_file(f);
            }
        }
        log.info(format!("ABB Signal Spy {} started. Settings: {}", env!("CARGO_PKG_VERSION"), settings_path.display()));
        // An internal error last time (the panic hook's crash.txt): said once, with
        // the file kept under a name of its own for a report.
        let crash = data_dir.join("crash.txt");
        let crash_note = crash.is_file().then(|| {
            let kept = data_dir.join(format!("crash-{}.txt", spy_core::util::local_stamp(std::time::SystemTime::now())));
            let kept = if std::fs::rename(&crash, &kept).is_ok() { kept } else { crash.clone() };
            format!("ABB Signal Spy hit an internal error last time. What happened is in {}: please include that file when reporting it.", kept.display())
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
            markers: Vec::new(),
            lane_zoom: HashMap::new(),
            lane_ranges: HashMap::new(),
            wheel_acc: 0.0,
            recorder: None,
            slow: None,
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
            show_about: false,
            show_diag: false,
            show_guide: false,
            show_catalogue_info: false,
            hostnames: Arc::new(Mutex::new(HashMap::new())),
            toasts: Vec::new(),
            rates: (Instant::now(), 0, 0, 0.0, 0.0),
            another_instance,
            log_filter_warn: false,
            title: String::new(),
        };
        // Restore the channel set (the controller is not contacted until Connect).
        let saved = app.settings.channels.clone();
        // Lanes count from 1; 0 is a file that never said (trimmed by hand, or older):
        // each such channel gets a chart of its own, after every lane the file names,
        // rather than all of them sharing one with volts beside degrees.
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
                app.chans.push(ChanView { radians: c.radians, hold_nonzero: c.hold_nonzero, smooth_ms: c.smooth_ms, scale: c.scale, ..ChanView::new(key, chan_color(i, app.settings.dark), lane) });
            }
        }
        app.sync_channels();
        // And the derived channels whose inputs came back with them.
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
        // On screen, not only in the log: something the person wrote was not used.
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
        // The guide opens by itself once, on the first run; afterwards it is in Help.
        app.show_guide = first_run;
        app
    }

    // ------------------------------------------------------------------ helpers

    pub fn toast(&mut self, level: Level, text: impl Into<String>) {
        let text = text.into();
        self.log.push(level, text.clone());
        self.show_toast(level, text);
    }

    /// A toast for something logged by other means (said once in the log, not twice).
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
            .map(|c| SavedChannel { signal: c.key.signal, unit: c.key.unit.to_string(), axis: c.key.axis.one_based(), radians: c.radians, hold_nonzero: c.hold_nonzero, lane: c.lane, smooth_ms: c.smooth_ms, scale: c.scale })
            .collect();
        // A plateau is not kept: it was measured on the controller of the moment; nor a
        // target read from a controller (its commutator offset). A typed target is.
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
        if let Err(e) = self.settings.save(&self.settings_path) {
            self.log.warn(format!("Could not save the settings: {e}"));
        }
        self.settings_dirty = None;
    }

    /// Push the channel list to the session, drop removed channels' history, and
    /// remember the set.
    pub fn sync_channels(&mut self) {
        let keys: Vec<ChannelKey> = self.chans.iter().map(|c| c.key.clone()).collect();
        for ch in self.session.store().all() {
            if !keys.contains(&ch.key) {
                self.session.store().remove(&ch.key);
            }
        }
        // Text events send only on a change: the session is told which they are, so
        // their silence before a first record is not reported as a fault.
        let text: Vec<ChannelKey> = keys.iter().filter(|k| self.catalogue.get(k.signal).is_some_and(view::is_text)).cloned().collect();
        self.session.set_channels_expecting_text(keys, text);
        self.prune_derived();
        self.recolor();
        self.mark_settings_dirty();
    }

    /// Each channel's colour by its place, from the theme in use: after a change of
    /// channels, and of theme.
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
                // The connection worker stopped after an internal error (its status
                // says so): a request to it would go nowhere, so Connect starts a new
                // one, on the same history. A recording fed by the old one has ended.
                if !self.session.is_running() {
                    for r in [self.recorder.take(), self.slow.take()].into_iter().flatten() {
                        let s = r.stop();
                        self.toast(Level::Warn, format!("Recording finished when the connection worker was restarted: {}", s.dir.display()));
                    }
                    self.log.warn("The connection worker had stopped after an internal error; starting a new one.");
                    let c = self.ctx.clone();
                    let store = self.session.store().clone();
                    self.session = Session::spawn(self.session_opts.clone(), self.log.clone(), store, Arc::new(move || c.request_repaint()));
                    self.sync_channels();
                }
                // Another controller: a running recording must not carry on with a
                // second controller's samples under the same channel names, and
                // markers placed on the first one's clock mean nothing on the next.
                let previous = self.session.status().target.clone();
                if previous.as_ref().is_some_and(|p| p != &t) {
                    let mut stopped = Vec::new();
                    for r in [self.recorder.take(), self.slow.take()].into_iter().flatten() {
                        stopped.push(r.stop().dir);
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
                self.session.connect(t);
            }
            Err(e) => self.toast(Level::Error, e),
        }
    }

    /// The session followed a restarted virtual controller to its new port: the
    /// address above, the recent list and a saved entry follow it too, or the next
    /// Connect would go back to the port nothing listens on any more (and finish a
    /// recording as if for another controller).
    fn follow_moved_controller(&mut self) {
        let Some((from, to)) = self.session.status().moved.clone() else { return };
        // A move from somewhere else: already followed, or one reported just before
        // the person connected elsewhere.
        if self.connected_to.as_ref() != Some(&from) {
            return;
        }
        // Not over what the person is typing.
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

    // ------------------------------------------------------------------ top bar

    fn menu(&mut self, ui: &mut egui::Ui) {
        // Menu entries in the plain face: a menu of bold lines is hard to scan.
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
                    if ui.add_enabled(connected, egui::Button::new("reset InfoStream...")).on_hover_text("Removes EVERY client's test-signal streams on the controller. Only for when a crashed program left streams behind.").clicked() {
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
                    if ui.add_enabled(self.settings.catalogue_file.is_some(), egui::Button::new("back to the built-in catalogue")).clicked() {
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
                    if ui.checkbox(&mut self.settings.dark, "dark").changed() {
                        theme::apply(&self.ctx, self.settings.dark, self.settings.ui_scale);
                        self.recolor();
                        self.mark_settings_dirty();
                    }
                    ui.horizontal(|ui| {
                        ui.label("text size");
                        for (label, s) in [("S", 0.9f32), ("M", 1.0), ("L", 1.2), ("XL", 1.45)] {
                            if ui.selectable_label((self.settings.ui_scale - s).abs() < 0.01, label).clicked() {
                                self.settings.ui_scale = s;
                                theme::apply(&self.ctx, self.settings.dark, s);
                                self.mark_settings_dirty();
                            }
                        }
                    });
                    ui.separator();
                    ui.checkbox(&mut self.dashboard, "live dashboard").on_hover_text("Big numbers in place of the charts, to read from a step away.");
                    if ui.checkbox(&mut self.settings.signals_folded, "fold the signal list").changed() {
                        self.mark_settings_dirty();
                    }
                    if ui.checkbox(&mut self.show_log, "messages").changed() {
                        ui.close();
                    }
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
        // A plain path field, rather than a file-dialog dependency: see the window.
        self.show_catalogue_info = true;
    }

    /// The controller's address, connecting, and the recording buttons: one sheet across
    /// the top.
    fn controller_bar(&mut self, ui: &mut egui::Ui) {
        let p = theme::pal(ui);
        let phase = self.session.status().phase.clone();
        let active = phase.is_active();
        theme::sheet_frame(ui).inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.set_min_height(fields::HEIGHT);
                ui.label(theme::b("controller").color(p.ink2));
                // Locked while connected: the address of the session under way.
                ui.add_enabled_ui(!active, |ui| {
                    fields::line(ui, &mut self.host_input, "Controller address", |t| t.hint_text("e.g. 192.168.125.1").desired_width(150.0))
                        .on_hover_text("The controller's IP address or name. A real IRC5 answers on port 5515.")
                        .on_disabled_hover_text("Disconnect to change the address.");
                });
                ui.label(theme::b("port").color(p.ink2));
                ui.add_enabled_ui(!active, |ui| {
                    fields::line(ui, &mut self.port_input, "Controller port", |t| t.desired_width(64.0))
                        .on_hover_text("5515 on an IRC5. A RobotStudio virtual controller picks a new port at every start: use the list.")
                        .on_disabled_hover_text("Disconnect to change the port.");
                    self.target_menu(ui);
                });
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
                theme::vrule(ui, 32.0);
                self.record_controls(ui);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (text, icon) = if self.dashboard { ("back to the charts", theme::Icon::Collapse) } else { ("live dashboard", theme::Icon::Grid) };
                    if theme::icon_text_button(ui, icon, text, fields::HEIGHT, self.dashboard).on_hover_text("Big numbers in place of the charts, to read from a step away").clicked() {
                        self.dashboard = !self.dashboard;
                    }
                });
            });
        });
    }

    fn target_menu(&mut self, ui: &mut egui::Ui) {
        let p = theme::pal(ui);
        let button = theme::drop_button(ui, "list", fields::HEIGHT).on_hover_text("Virtual controllers on this PC, saved controllers and recent ones");
        if button.clicked() && matches!(self.discovery, Discovery::Idle) {
            self.start_discovery();
        }
        egui::Popup::menu(&button).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
            ui.set_min_width(420.0);
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
                        if ui.button(format!("{kind}  127.0.0.1:{}  {}", c.port, short_id(&sys))).clicked() {
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
            if ui.button("search again").clicked() {
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
                    if ui.button(format!("{}  {}:{}", c.name, c.host, c.port)).clicked() {
                        self.host_input = c.host.clone();
                        self.port_input = c.port.to_string();
                        ui.close();
                    }
                    if theme::icon_button(ui, theme::Icon::Close, "Forget this controller", egui::vec2(36.0, 36.0)).clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                self.settings.controllers.remove(i);
                self.mark_settings_dirty();
            }
            ui.horizontal(|ui| {
                fields::line(ui, &mut self.name_input, "Name for the saved controller", |t| t.hint_text("name").desired_width(140.0));
                if ui.button("save the address").clicked() {
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
                    if ui.button(t.to_string()).clicked() {
                        self.host_input = t.host.clone();
                        self.port_input = t.port.to_string();
                        ui.close();
                    }
                }
            }
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

    /// Frames and samples a second, over the last second or so.
    fn update_rates(&mut self, st: &spy_core::session::Status) {
        let now = Instant::now();
        let dt = now.duration_since(self.rates.0).as_secs_f64();
        if dt >= 1.0 {
            self.rates = (now, st.counters.frames, st.counters.samples, (st.counters.frames.saturating_sub(self.rates.1)) as f64 / dt, (st.counters.samples.saturating_sub(self.rates.2)) as f64 / dt);
        }
    }

    /// The connection's state in a word or two, its colour, and whether its square is
    /// filled (a session under way) or empty.
    pub fn phase_word(st: &spy_core::session::Status, p: &theme::Pal) -> (String, Color32, theme::Mark) {
        let (word, color) = match &st.phase {
            Phase::Idle => ("not connected".to_string(), p.ink2),
            Phase::Connecting => ("connecting".into(), p.hold),
            Phase::Handshaking => ("handshake".into(), p.hold),
            Phase::AwaitingApproval => ("waiting for your answer".into(), p.hold),
            Phase::SettingUp => ("setting up".into(), p.hold),
            // Set up, and nothing arriving (another program gets the samples, a VC
            // paused): the notice under the strip says why. Not a green "streaming".
            Phase::Streaming if st.advice.is_some() => ("not receiving".into(), p.hold),
            Phase::Streaming => ("streaming".into(), p.live),
            Phase::Reconnecting { attempt, retry_in } => (format!("reconnecting, try {attempt} (every {:.0} s)", retry_in.as_secs_f64()), p.hold),
            Phase::TearingDown => ("disconnecting".into(), p.hold),
            Phase::Stopped { .. } => ("stopped".into(), p.red),
        };
        let mark = if matches!(st.phase, Phase::Idle | Phase::Stopped { .. }) { theme::Mark::Off } else { theme::Mark::On };
        (word, color, mark)
    }

    /// The other programs on the controller worth naming: every real IRC5 also lists its
    /// pendant, always there, so it is left out (G48).
    pub fn others_shown(st: &spy_core::session::Status) -> Vec<&spy_core::session::OtherClient> {
        st.others.iter().filter(|o| !o.pendant).collect()
    }

    /// The strip under the controller bar (G49, the bridge's): the connection, the
    /// controller, the channels, the recording and the other programs, each a labelled
    /// cell under a rule.
    fn status_strip(&mut self, ui: &mut egui::Ui) {
        let p = theme::pal(ui);
        let st = self.session.status().clone();
        self.update_rates(&st);
        let gap = 16.0;
        let link_w = 64.0;
        let w = ((ui.available_width() - link_w - gap * 5.0) / 5.0).floor().max(80.0);
        // Top-aligned: every cell's rule on one line.
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            let (word, color, mark) = Self::phase_word(&st, p);
            let rule = if mark == theme::Mark::Off && color == p.ink2 { p.ink } else { color };
            theme::status_cell(ui, w, "connection", &word, color, mark, rule);

            let controller = match &st.target {
                Some(t) if st.loopback => format!("VC {} : {}", t.host, t.port),
                Some(t) => format!("{} : {}", t.host, t.port),
                None => "none".into(),
            };
            let r = theme::status_cell(ui, w, "controller", &controller, if st.target.is_some() { p.ink } else { p.ink2 }, theme::Mark::None, p.ink);
            if let Some(s) = self.rws.as_ref().and_then(|l| l.system.as_ref()) {
                r.on_hover_text(format!("{} · RobotWare {} (from its RWS)", s.name, s.rw_version));
            }

            let live: Vec<f64> = st.channels.iter().filter(|c| matches!(c.state, spy_core::session::ChannelState::Defined { .. }) && c.rate > 0.0).map(|c| c.rate).collect();
            let rate = match (live.iter().cloned().fold(f64::INFINITY, f64::min), live.iter().cloned().fold(0.0, f64::max)) {
                _ if live.is_empty() => String::new(),
                (lo, hi) if hi - lo <= hi * 0.05 => format!(" · {hi:.0} /s each"),
                (lo, hi) => format!(" · {lo:.0} to {hi:.0} /s"),
            };
            theme::status_cell(ui, w, "channels", &format!("{} of {}{rate}", self.chans.len() + self.derived.len(), spy_core::session::MAX_CHANNELS), p.ink, theme::Mark::None, p.ink);

            let (rec, rec_on) = match (&self.recorder, &self.slow) {
                (Some(r), slow) => {
                    let s = r.status();
                    (format!("{} · {}{}", crate::record::clock(s.started.elapsed().as_secs()), crate::record::size_text(s.bytes), if slow.is_some() { " + slow log" } else { "" }), true)
                }
                (None, Some(r)) => {
                    let s = r.status();
                    (format!("slow log {} · {} rows", crate::record::clock(s.started.elapsed().as_secs()), s.rows), true)
                }
                (None, None) => ("off".into(), false),
            };
            let r = if rec_on { theme::status_cell(ui, w, "recording", &rec, p.red, theme::Mark::On, p.red) } else { theme::status_cell(ui, w, "recording", &rec, p.ink2, theme::Mark::Off, p.ink) };
            if let Some(r2) = self.recorder.as_ref().or(self.slow.as_ref()) {
                r.on_hover_text(r2.status().dir.display().to_string());
            }

            let others = Self::others_shown(&st);
            if others.is_empty() {
                theme::status_cell(ui, w, "other programs", "none", p.ink, theme::Mark::Off, p.ink);
            } else {
                for o in &others {
                    let c = ui.ctx().clone();
                    net::lookup(&self.hostnames, &o.address, move || c.request_repaint());
                }
                let names = self.client_names(&others);
                let text = if names.len() == 1 { names[0].clone() } else { format!("{}: {}", names.len(), names.join(", ")) };
                theme::status_cell(ui, w, "other programs", &text, p.hold, theme::Mark::On, p.hold)
                    .on_hover_text(format!("Connected to this controller over RobAPI:\n{}\n\nRobotStudio counts whenever it is connected, even when it is not streaming.", names.join("\n")));
            }

            ui.allocate_ui_with_layout(egui::vec2(link_w, 46.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top() + 1.0, egui::Stroke::new(2.0, p.ink));
                ui.add_space(14.0);
                if ui.link(theme::b("details")).on_hover_text("The connection's counters, rates and identity").clicked() {
                    self.show_diag = true;
                }
            });
        });
        self.notices(ui, &st);
    }

    /// What needs saying under the strip, only while it is true: why the session
    /// stopped, what to do about samples not arriving, a second window, samples lost.
    fn notices(&mut self, ui: &mut egui::Ui, st: &spy_core::session::Status) {
        let p = theme::pal(ui);
        let mut lines: Vec<(String, Color32)> = Vec::new();
        if self.another_instance {
            lines.push(("Another ABB Signal Spy window is open. Two windows on the same controller break each other's streams: only one program at a time can stream test signals.".into(), p.hold));
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
        for (text, color) in lines {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                theme::square(ui, color, 10.0);
                ui.add(egui::Label::new(theme::b(text).color(color)).wrap());
            });
        }
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

    // ------------------------------------------------------------------ dialogs

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
        egui::Modal::new(egui::Id::new("approval")).show(ctx, |ui| {
            ui.set_max_width(540.0);
            ui.heading("Other programs are connected to this controller");
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
                if theme::primary(ui, "take InfoStream", fields::HEIGHT).clicked() {
                    self.session.answer(true, &st.others);
                }
                if ui.add(egui::Button::new("cancel").min_size(egui::vec2(0.0, fields::HEIGHT))).clicked() {
                    self.session.answer(false, &st.others);
                }
            });
        });
    }

    fn reset_dialog(&mut self, ctx: &egui::Context) {
        if !self.confirm_reset {
            return;
        }
        egui::Modal::new(egui::Id::new("reset")).show(ctx, |ui| {
            ui.set_max_width(480.0);
            ui.heading("Reset InfoStream?");
            ui.label("This sends StreamUndefineAll, which removes EVERY program's test-signal streams on this controller: RobotStudio's, TuneMaster's, any other tool's, and this program's (which are then set up again).");
            ui.label("Use it only when the controller refuses new channels (\"no channel available\") because a program that crashed left its streams behind.");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if theme::red_button(ui, egui::Button::new("reset InfoStream").min_size(egui::vec2(0.0, fields::HEIGHT))).clicked() {
                    self.session.reset_infostream();
                    self.confirm_reset = false;
                }
                if ui.add(egui::Button::new("cancel").min_size(egui::vec2(0.0, fields::HEIGHT))).clicked() {
                    self.confirm_reset = false;
                }
            });
        });
    }

    fn info_windows(&mut self, ctx: &egui::Context) {
        let mut open = self.show_about;
        egui::Window::new("About ABB Signal Spy").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            ui.heading(format!("ABB Signal Spy {}", env!("CARGO_PKG_VERSION")));
            ui.label("Reads, charts and records the motion test signals an ABB IRC5 controller streams over RobAPI InfoStream.");
            ui.label("It only reads: it never commands motion, never writes RAPID, configuration or I/O, and never takes mastership.");
            ui.add_space(6.0);
            ui.label(RichText::new("Not affiliated with or endorsed by ABB. ABB and IRC5 are trademarks of ABB.").strong());
            ui.label("The InfoStream protocol is not a documented interface: this program was checked against real controllers, and a RobotWare update could change it. When something is off, this program says so rather than showing a stale value as live.");
            ui.add_space(6.0);
            ui.label("Copyright (C) 2026 Jon Sands. Free software under the GNU General Public License, version 3 or later: you may share and change it under its terms. It comes with ABSOLUTELY NO WARRANTY.");
            ui.label(RichText::new(format!("Catalogue: {} ({})", self.catalogue.title, self.catalogue.source)).weak());
            ui.label(RichText::new("Set in Atkinson Hyperlegible Next and Mono, by the Braille Institute of America: copyright 2020-2024 The Atkinson Hyperlegible Next and Mono Project Authors, under the SIL Open Font License 1.1.").weak());
        });
        self.show_about = open;

        let mut open = self.show_guide;
        egui::Window::new("Quick guide").open(&mut open).collapsible(false).default_width(520.0).pivot(egui::Align2::CENTER_CENTER).default_pos(ctx.content_rect().center()).show(ctx, |ui| {
            ui.label(theme::b("1. Connect"));
            ui.label("Real IRC5: type its address (the port is 5515) and press connect. RobotStudio virtual controller: open the list beside the port and pick it; its port changes every time it starts.");
            ui.label(theme::b("2. Only one program at a time"));
            ui.label("A controller streams test signals to one program at a time. Close TuneMaster's signal logging or RobotStudio's signal tools first. If other programs are connected, this program lists them and asks before taking over.");
            ui.label(theme::b("3. Add channels"));
            ui.label("Pick a signal in the list on the left, then add it. The dialog asks only what that signal needs: the robot, the axis, or nothing. 'add a set' adds a common group in one go (both DC links, one robot's torques). Up to 12 channels.");
            ui.label(theme::b("4. Read and chart"));
            ui.label("Values show on the right; click a channel there for its options: smoothing, the vertical scale, its chart. Smoothing changes only the screen. Space pauses the charts so you can look back through the last 10 minutes. A reading that stops updating is marked stale, never shown as live.");
            ui.label(theme::b("5. Record"));
            ui.label("record keeps every sample. 'save last' saves what just happened, even if nothing was recording. 'slow log' logs averages for runs of hours. The arrow beside each sets its name, seconds or interval. M drops a marker.");
            ui.label(theme::b("6. Look back"));
            ui.label("file > open a recording (or drop its folder on the window) charts it again, marked REVIEWING: not live. 'save csv' and 'save png' above the charts save what is in view, live or reviewed; a CSV always holds the samples as they came.");
            ui.add_space(6.0);
            ui.label(RichText::new("Angles are in degrees; a channel's options switch one to radians.").weak());
        });
        self.show_guide = open;

        let mut open = self.show_catalogue_info;
        let mut load_path: Option<String> = None;
        egui::Window::new("Catalogue").open(&mut open).default_width(560.0).show(ctx, |ui| {
            ui.heading(&self.catalogue.title);
            ui.label(format!("Source: {}", self.catalogue.source));
            ui.label(&self.catalogue.measured_on);
            ui.label(RichText::new(&self.catalogue.caveat).color(theme::pal(ui).hold));
            ui.label(RichText::new(&self.catalogue.credits).weak());
            let named = self.catalogue.signals.iter().filter(|s| s.named).count();
            ui.label(format!("{} signal numbers, {named} with a named quantity.", self.catalogue.signals.len()));
            ui.separator();
            ui.label("Load a catalogue file (for another robot or RobotWare version):");
            let id = egui::Id::new("catalogue-path");
            let mut path: String = ui.data_mut(|d| d.get_temp::<String>(id)).unwrap_or_default();
            ui.horizontal(|ui| {
                fields::line(ui, &mut path, "Catalogue file", |t| t.hint_text("C:\\path\\to\\catalogue.json").desired_width(380.0));
                if ui.button("load").clicked() {
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
                }
                Err(e) => self.toast(Level::Error, e),
            }
        }

        let mut open = self.show_diag;
        let rws_system = self.rws.as_ref().and_then(|l| l.system.as_ref()).map(|s| format!("{} · RobotWare {}", s.name, s.rw_version));
        let rates = (self.rates.3, self.rates.4);
        let others = {
            let st = self.session.status().clone();
            self.client_names(&Self::others_shown(&st)).join(", ")
        };
        egui::Window::new("Connection details").open(&mut open).default_width(560.0).show(ctx, |ui| {
            let st = self.session.status().clone();
            let c = &st.counters;
            egui::Grid::new("diag").striped(true).show(ui, |ui| {
                let mut row = |k: &str, v: String| {
                    ui.label(RichText::new(k).color(theme::pal(ui).ink2));
                    ui.label(RichText::new(v).monospace());
                    ui.end_row();
                };
                row("phase", format!("{:?}", st.phase));
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
            ui.label(RichText::new("messages").font(egui::FontId::new(18.0, theme::heavy())));
            ui.checkbox(&mut self.log_filter_warn, "warnings only");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::icon_button(ui, theme::Icon::Close, "Close the messages", egui::vec2(32.0, 30.0)).clicked() {
                    self.show_log = false;
                }
            });
        });
        let entries = self.log.since(self.log.next_seq().saturating_sub(400));
        egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
            // Lines to copy from, for a report.
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

    /// The line along the bottom: the time, what the charts do with the mouse while
    /// they can be moved, where a recording goes, or the newest message; and the
    /// messages themselves a click away.
    fn footer(&mut self, ui: &mut egui::Ui) {
        let p = theme::pal(ui);
        let rect = ui.max_rect();
        ui.painter().hline(rect.x_range(), rect.top() + 1.0, egui::Stroke::new(2.0, p.ink));
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.set_min_height(30.0);
            ui.label(theme::num(view::local_hms(std::time::SystemTime::now()), 16.0).color(p.red));
            ui.add_space(4.0);
            let hint = self.mouse_hint();
            let mut open_folder = None;
            if let Some(h) = hint {
                ui.add(egui::Label::new(RichText::new(h).color(p.ink)).truncate());
            } else if let Some(r) = self.recorder.as_ref().or(self.slow.as_ref()) {
                let dir = r.status().dir;
                ui.add(egui::Label::new(format!("recording to {}", dir.display())).truncate());
                if ui.link(theme::b("open the folder")).clicked() {
                    open_folder = Some(dir);
                }
            } else if let Some(e) = self.log.since(self.log.next_seq().saturating_sub(1)).last().filter(|e| !self.toasts.iter().any(|t| t.text == e.text)) {
                // (Not while a toast says it: once on screen is enough.)
                let color = match e.level {
                    Level::Info => p.ink2,
                    Level::Warn => p.hold,
                    Level::Error => p.red,
                };
                ui.add(egui::Label::new(RichText::new(&e.text).color(color)).truncate());
                if let Some(d) = &self.last_folder
                    && e.text.contains(&d.display().to_string())
                    && ui.link(theme::b("open the folder")).clicked()
                {
                    open_folder = Some(d.clone());
                }
            }
            if let Some(d) = open_folder {
                crate::paths::open_folder(&d);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let n = self.log.next_seq();
                let text = format!("messages ({n})");
                if ui.add(egui::Button::new(theme::b(text).size(15.0)).selected(self.show_log).min_size(egui::vec2(0.0, 26.0))).clicked() {
                    self.show_log = !self.show_log;
                }
                if let Some(ph) = &self.phone {
                    ui.label(RichText::new(format!("phone view on, port {}", ph.port())).color(p.ink2)).on_hover_text(ph.urls.join("\n"));
                }
            });
        });
    }

    /// While the charts can be moved (paused, or a recording), how: said where it is
    /// read, with Ctrl + wheel for the vertical scale (G48).
    fn mouse_hint(&self) -> Option<String> {
        const MOVE: &str = "drag to move through time · wheel zooms time · ctrl + wheel zooms the vertical scale · double-click goes back";
        if let Some(rs) = &self.review {
            return Some(if rs.cursors_on { format!("click a chart for cursor A, right-click for B · {MOVE}") } else { MOVE.into() });
        }
        if self.dashboard {
            return None;
        }
        // The cursors' distance apart, while both are placed (the rows read their values).
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
        self.toasts.retain_mut(|t| now.duration_since(*t.shown.get_or_insert(now)) < Duration::from_secs(6));
        if self.toasts.is_empty() {
            return;
        }
        // Clicks pass through: a message over the channels must not take a click meant
        // for a button under it.
        egui::Area::new(egui::Id::new("toasts")).anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-12.0, -44.0)).interactable(false).show(ctx, |ui| {
            let p = theme::pal(ui);
            for t in &self.toasts {
                let color = match t.level {
                    Level::Info => p.live,
                    Level::Warn => p.hold,
                    Level::Error => p.red,
                };
                egui::Frame::popup(ui.style()).stroke(egui::Stroke::new(2.0, color)).inner_margin(egui::Margin::same(12)).show(ui, |ui| {
                    ui.set_max_width(440.0);
                    ui.label(&t.text);
                });
            }
        });
        ctx.request_repaint_after(Duration::from_millis(500));
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let (space, m, esc) = ctx.input(|i| (i.key_pressed(egui::Key::Space), i.key_pressed(egui::Key::M), i.key_pressed(egui::Key::Escape)));
        // Escape closes a signal's details (no dialog over them: a dialog takes it first).
        if esc && self.add.is_none() && self.note_edit.is_none() && self.sets.is_none() {
            self.selected = None;
        }
        // While reviewing, the live charts are hidden: pausing them unseen, or putting a
        // marker into a live recording from what is under review, would mislead.
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
        // The controller's clock at the marker, so a recorder working through a
        // backlog still files it at the moment it was placed.
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

    /// The window title carries the state, so the taskbar says whether it is live
    /// and recording while the window is minimized.
    fn update_title(&mut self, ctx: &egui::Context) {
        let st = self.session.status();
        let state = match &st.phase {
            Phase::Streaming if st.advice.is_some() => "NOT RECEIVING",
            Phase::Streaming => "STREAMING",
            Phase::Reconnecting { .. } => "RECONNECTING",
            Phase::Stopped { .. } => "STOPPED",
            Phase::Idle => "",
            _ => "CONNECTING",
        };
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
            let d = view::display(sig, c.radians);
            let reading = view::reading(sig);
            let value = self.session.store().get(&c.key).and_then(|ch| {
                let r = ch.lock();
                if r.kind == Some(spy_core::sample::ValueKind::String) { r.last_text.clone() } else { view::readout(&r, reading).map(|v| view::fmt(v * d.factor)) }
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
            // The number too: a snapshot the window stops updating is shown with it,
            // never with ON TARGET (the phone server's rule).
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
        let state = match &st.phase {
            Phase::Streaming => "Streaming".to_string(),
            p => format!("{p:?}"),
        };
        let body = serde_json::json!({ "controller": st.target.map(|t| t.to_string()).unwrap_or_default(), "state": state, "channels": chans });
        let mut g = self.phone_snapshot.lock().unwrap_or_else(|e| e.into_inner());
        g.body = body.to_string();
        g.built = Some(Instant::now());
    }
}

/// The first eight characters of a system id. By characters, not bytes: the id is
/// the controller's text, and a byte slice through a multi-byte character panics.
pub fn short_id(id: &str) -> String {
    let t = id.trim_matches(['{', '}']);
    if t.chars().count() > 8 { format!("{{{}…}}", t.chars().take(8).collect::<String>()) } else { id.to_string() }
}

impl eframe::App for SpyApp {
    /// The upkeep, which must not stop when the drawing does. eframe calls this before
    /// every frame and, while the window is minimised, on its own whenever a repaint
    /// was asked for, with no frame drawn: the phone view, the taskbar title, the
    /// derived channels and the recorders' checks go on meanwhile (measured: with all
    /// of it in `ui`, the phone read NOT CURRENT a second after minimising).
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_background();
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
        // The next look, drawn or not: the phone's snapshot is built every 250 ms.
        ctx.request_repaint_after(Duration::from_millis(250));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.take_dropped(&ctx);
        self.take_screenshot(&ctx);
        self.shortcuts(&ctx);

        // The page, and square sheets on it 8 px apart (G49).
        let p = theme::pal(ui);
        let page = egui::Frame::new().fill(p.page);
        egui::Panel::top("menu").frame(page.inner_margin(egui::Margin::symmetric(4, 1))).show_separator_line(false).show(ui, |ui| {
            self.menu(ui);
            let r = ui.max_rect();
            ui.painter().hline(r.x_range(), r.bottom() + 1.0, egui::Stroke::new(1.0, p.line));
        });
        egui::Panel::top("bar").frame(page.inner_margin(egui::Margin { left: 8, right: 8, top: 8, bottom: 0 })).show_separator_line(false).show(ui, |ui| self.controller_bar(ui));
        egui::Panel::top("strip").frame(page.inner_margin(egui::Margin { left: 8, right: 8, top: 8, bottom: 8 })).show_separator_line(false).show(ui, |ui| {
            if self.review.is_some() {
                self.review_strip(ui);
            } else {
                self.status_strip(ui);
            }
        });
        egui::Panel::bottom("footer").frame(page.inner_margin(egui::Margin { left: 16, right: 8, top: 0, bottom: 2 })).show_separator_line(false).show(ui, |ui| self.footer(ui));
        let sheet = |left: i8, right: i8| egui::Frame::new().fill(p.sheet).inner_margin(egui::Margin::same(10)).outer_margin(egui::Margin { left, right, top: 0, bottom: 8 });
        if self.show_log {
            egui::Panel::bottom("log").frame(sheet(8, 8)).resizable(true).default_size(190.0).size_range(90.0..=600.0).show_separator_line(false).show(ui, |ui| self.log_pane(ui));
        }
        if self.dashboard && self.review.is_none() {
            let central = egui::CentralPanel::default().frame(sheet(8, 8)).show(ui, |ui| self.dashboard_ui(ui));
            self.charts_rect = Some(central.response.rect);
        } else {
            if self.settings.signals_folded {
                egui::Panel::left("signals-folded").frame(sheet(8, 0).inner_margin(egui::Margin::symmetric(6, 10))).exact_size(60.0).resizable(false).show_separator_line(false).show(ui, |ui| self.signals_strip(ui));
            } else {
                egui::Panel::left("signals").frame(sheet(8, 0)).resizable(true).default_size(298.0).size_range(248.0..=470.0).show_separator_line(false).show(ui, |ui| self.browser(ui));
            }
            // While a recording is reviewed it has the charts and the right panel; the
            // live session carries on underneath, and the strip says so.
            egui::Panel::right("channels").frame(sheet(0, 8)).resizable(true).default_size(338.0).size_range(308.0..=490.0).show_separator_line(false).show(ui, |ui| {
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
        // Before the XY window: its "XY" buttons open that plot in the same frame.
        self.compare_window(&ctx);
        self.xy_window(&ctx);
        self.add_dialog(&ctx);
        self.notes_dialog(&ctx);
        self.sets_dialog(&ctx);
        self.approval_dialog(&ctx);
        self.reset_dialog(&ctx);
        self.info_windows(&ctx);
        self.toasts(&ctx);

        // Live charts move with the controller clock (the upkeep asks for its own look).
        if self.session.status().phase == Phase::Streaming && self.paused_at.is_none() {
            ctx.request_repaint_after(Duration::from_millis(33));
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.shutdown();
    }
}

impl SpyApp {
    /// Everything that must happen on the way out, whichever way out it is: finish
    /// the recordings properly, stop listening on the LAN, save, and tear the
    /// controller session down.
    pub fn shutdown(&mut self) {
        if let Some(r) = self.recorder.take() {
            let s = r.stop();
            self.log.info(format!("Recording finished: {} rows in {}", s.rows, s.dir.display()));
        }
        if let Some(r) = self.slow.take() {
            r.stop();
        }
        // A save under way stops, and its part file goes with it.
        if let Some(job) = self.export_job.take() {
            self.export_stop.store(true, std::sync::atomic::Ordering::SeqCst);
            let _ = job.join();
            self.log.info("A CSV being saved was stopped: the window closed before it was done.");
        }
        self.phone = None;
        self.save_settings();
        self.session.disconnect();
    }
}

impl Drop for SpyApp {
    fn drop(&mut self) {
        // Also on a panic: the recorders close their files and the session handle's
        // own drop tears the controller session down.
        if let Some(r) = self.recorder.take() {
            let _ = r.stop();
        }
        if let Some(r) = self.slow.take() {
            let _ = r.stop();
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
    fn short_ids_never_split_a_character() {
        assert_eq!(super::short_id("{12345678-9ABC-4DEF-8123-456789ABCDEF}"), "{12345678…}");
        assert_eq!(super::short_id("{abc}"), "{abc}");
        // Latin-1 text from a controller: multi-byte in UTF-8, with byte 8 falling
        // inside a character ("A" is one byte, each "Ä" two).
        assert_eq!(super::short_id("{AÄÄÄÄÄÄÄÄÄ}"), "{AÄÄÄÄÄÄÄ…}");
        assert_eq!(super::short_id(""), "");
    }
}
