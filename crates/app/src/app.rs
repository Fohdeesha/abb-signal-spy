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
    pub at: Instant,
    pub text: String,
    pub level: Level,
}

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
    /// A chart's locked vertical scale, by (lane, display unit): see `charts::lanes`.
    pub lane_lock: HashMap<(u32, String), (f64, f64)>,

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
    pub png_pending: bool,
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

pub fn chan_color(i: usize) -> Color32 {
    theme::PALETTE[i % theme::PALETTE.len()]
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
            lane_lock: HashMap::new(),
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
            png_pending: false,
            export_job: None,
            export_note: "",
            export_stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            review: None,
            review_job: None,
            show_recordings: false,
            recordings_list: None,
            recording_path_input: String::new(),
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
                app.chans.push(ChanView { key, color: chan_color(i), radians: c.radians, hold_nonzero: c.hold_nonzero, lane, stats: Stats::default() });
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
            let color = chan_color(app.chans.len() + app.derived.len());
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
        self.toasts.push(Toast { at: Instant::now(), text, level });
    }

    pub fn mark_settings_dirty(&mut self) {
        self.settings_dirty = Some(Instant::now());
    }

    pub fn save_settings(&mut self) {
        self.settings.window_s = self.window_s;
        self.settings.channels = self
            .chans
            .iter()
            .map(|c| SavedChannel { signal: c.key.signal, unit: c.key.unit.to_string(), axis: c.key.axis.one_based(), radians: c.radians, hold_nonzero: c.hold_nonzero, lane: c.lane })
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
        for (i, c) in self.chans.iter_mut().enumerate() {
            c.color = chan_color(i);
        }
        self.prune_derived();
        self.mark_settings_dirty();
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
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("Open a recording...").on_hover_text("Chart a recording made earlier. The live session carries on meanwhile.").clicked() {
                    self.show_recordings = true;
                    self.recordings_list = None;
                    ui.close();
                }
                if self.review.is_some() && ui.button("Close the recording").clicked() {
                    self.review = None;
                    ui.close();
                }
                ui.separator();
                if ui.button("Open the recordings folder").clicked() {
                    let d = self.record_dir();
                    let _ = std::fs::create_dir_all(&d);
                    crate::paths::open_folder(&d);
                    ui.close();
                }
                if ui.button("Open the settings folder").clicked() {
                    if let Some(p) = self.settings_path.parent() {
                        crate::paths::open_folder(p);
                    }
                    ui.close();
                }
                ui.separator();
                if ui.button("Quit").clicked() {
                    self.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("Controller", |ui| {
                let connected = self.session.status().phase.is_connected();
                if ui.button("Controller details (RWS)...").on_hover_text("Read-only: the controller's name and RobotWare version, its event log on the charts, a motor's commutator offset. Needs the controller's RWS login, which is not stored.").clicked() {
                    self.show_rws = true;
                    ui.close();
                }
                if ui.add_enabled(connected, egui::Button::new("Reset InfoStream...")).on_hover_text("Removes EVERY client's test-signal streams on the controller. Only for when a crashed program left streams behind.").clicked() {
                    self.confirm_reset = true;
                    ui.close();
                }
                if ui.button("Diagnostics").clicked() {
                    self.show_diag = true;
                    ui.close();
                }
            });
            ui.menu_button("Catalogue", |ui| {
                if ui.button("About this catalogue").clicked() {
                    self.show_catalogue_info = true;
                    ui.close();
                }
                if ui.button("Load a catalogue file...").on_hover_text("For another robot or RobotWare version: a catalogue file made with the scan tool or shared by someone else.").clicked() {
                    ui.close();
                    self.load_catalogue_dialog();
                }
                if ui.add_enabled(self.settings.catalogue_file.is_some(), egui::Button::new("Back to the built-in catalogue")).clicked() {
                    self.catalogue = Catalogue::builtin();
                    self.settings.catalogue_file = None;
                    self.mark_settings_dirty();
                    self.toast(Level::Info, "Using the built-in catalogue.");
                    ui.close();
                }
            });
            ui.menu_button("View", |ui| {
                if ui.checkbox(&mut self.settings.dark, "Dark").changed() {
                    theme::apply(&self.ctx, self.settings.dark, self.settings.ui_scale);
                    self.mark_settings_dirty();
                }
                ui.horizontal(|ui| {
                    ui.label("Text size");
                    for (label, s) in [("S", 0.9f32), ("M", 1.0), ("L", 1.2), ("XL", 1.45)] {
                        if ui.selectable_label((self.settings.ui_scale - s).abs() < 0.01, label).clicked() {
                            self.settings.ui_scale = s;
                            theme::apply(&self.ctx, self.settings.dark, s);
                            self.mark_settings_dirty();
                        }
                    }
                });
            });
            ui.menu_button("Help", |ui| {
                if ui.button("Quick guide").clicked() {
                    self.show_guide = true;
                    ui.close();
                }
                if ui.button("About").clicked() {
                    self.show_about = true;
                    ui.close();
                }
            });
        });
    }

    fn load_catalogue_dialog(&mut self) {
        // A plain path field, rather than a file-dialog dependency: see the window.
        self.show_catalogue_info = true;
    }

    fn controller_bar(&mut self, ui: &mut egui::Ui) {
        let phase = self.session.status().phase.clone();
        let active = phase.is_active();
        ui.horizontal(|ui| {
            ui.label(RichText::new("Controller").strong());
            ui.add_enabled_ui(!active, |ui| {
                ui.add(egui::TextEdit::singleline(&mut self.host_input).hint_text("address, e.g. 192.168.125.1").desired_width(170.0))
                    .on_hover_text("The controller's IP address or name. A real IRC5 answers on port 5515.");
                ui.label(":");
                ui.add(egui::TextEdit::singleline(&mut self.port_input).desired_width(48.0)).on_hover_text("5515 on an IRC5. A RobotStudio virtual controller picks a new port at every start: use the list.");
                self.target_menu(ui);
            });
            match &phase {
                Phase::Idle | Phase::Stopped { .. } => {
                    if ui.add(egui::Button::new(RichText::new("Connect").strong()).min_size(egui::vec2(80.0, 0.0))).clicked() {
                        self.connect();
                    }
                }
                _ => {
                    if ui.add(egui::Button::new("Disconnect").min_size(egui::vec2(80.0, 0.0))).clicked() {
                        self.session.disconnect();
                    }
                }
            }
            ui.separator();
            self.record_controls(ui);
        });
    }

    fn target_menu(&mut self, ui: &mut egui::Ui) {
        let resp = ui.menu_button("▾ List", |ui| {
            ui.set_min_width(360.0);
            ui.label(RichText::new("Virtual controllers on this PC").strong());
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
                    ui.colored_label(theme::BAD, e.as_str());
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
            if ui.button("Search again").clicked() || matches!(self.discovery, Discovery::Idle) {
                self.start_discovery();
            }
            ui.separator();
            ui.label(RichText::new("Saved controllers").strong());
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
                    if ui.small_button("🗑").on_hover_text("Forget this controller").clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                self.settings.controllers.remove(i);
                self.mark_settings_dirty();
            }
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.name_input).hint_text("name").desired_width(120.0));
                if ui.button("Save the address above").clicked() {
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
                ui.label(RichText::new("Recent").strong());
                for t in self.settings.recent.clone() {
                    if ui.button(t.to_string()).clicked() {
                        self.host_input = t.host.clone();
                        self.port_input = t.port.to_string();
                        ui.close();
                    }
                }
            }
        });
        resp.response.on_hover_text("Local virtual controllers, saved controllers and recent ones");
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

    fn session_line(&mut self, ui: &mut egui::Ui) {
        let st = self.session.status().clone();
        let now = Instant::now();
        let dt = now.duration_since(self.rates.0).as_secs_f64();
        if dt >= 1.0 {
            self.rates = (now, st.counters.frames, st.counters.samples, (st.counters.frames.saturating_sub(self.rates.1)) as f64 / dt, (st.counters.samples.saturating_sub(self.rates.2)) as f64 / dt);
        }
        ui.horizontal_wrapped(|ui| {
            let (word, color) = match &st.phase {
                Phase::Idle => ("NOT CONNECTED".to_string(), theme::IDLE),
                Phase::Connecting => ("CONNECTING".into(), theme::WARN),
                Phase::Handshaking => ("HANDSHAKE".into(), theme::WARN),
                Phase::AwaitingApproval => ("WAITING FOR YOUR ANSWER".into(), theme::WARN),
                Phase::SettingUp => ("SETTING UP".into(), theme::WARN),
                // Set up, and nothing arriving (another program gets the samples, a VC
                // paused): the advice beside it says why. Not a green STREAMING.
                Phase::Streaming if st.advice.is_some() => ("NOT RECEIVING".into(), theme::WARN),
                Phase::Streaming => ("STREAMING".into(), theme::OK),
                Phase::Reconnecting { attempt, retry_in } => (format!("RECONNECTING (attempt {attempt}, every {:.0} s)", retry_in.as_secs_f64()), theme::WARN),
                Phase::TearingDown => ("DISCONNECTING".into(), theme::WARN),
                Phase::Stopped { .. } => ("STOPPED".into(), theme::BAD),
            };
            ui.label(RichText::new("●").color(color));
            ui.label(RichText::new(word).strong().color(color));
            if let Some(t) = &st.target {
                ui.label(RichText::new(t.to_string()).monospace());
            }
            if let Phase::Stopped { reason } = &st.phase {
                ui.label(RichText::new(reason).color(theme::BAD));
            }
            if let Some(a) = &st.advice {
                ui.label(RichText::new(a).color(theme::WARN).strong());
            }
            if st.phase.is_connected() {
                ui.separator();
                let live = st.channels.iter().filter(|c| matches!(c.state, spy_core::session::ChannelState::Defined { .. })).count();
                ui.label(format!("{live} ch"));
                ui.label(format!("{:.0} frames/s", self.rates.3));
                ui.label(format!("{:.0} samples/s", self.rates.4));
                ui.label(format!("keepalives {}", st.counters.ayas)).on_hover_text("The controller's 'are you alive' checks, all answered. Unanswered, it drops the connection after about 16 s.");
                if st.counters.reconnects > 0 {
                    ui.label(RichText::new(format!("reconnects {}", st.counters.reconnects)).color(theme::WARN));
                }
                if let Some(a) = &st.announce
                    && let Some(id) = &a.system_id {
                        ui.label(RichText::new(short_id(id)).weak()).on_hover_text(format!("Controller system id {id}"));
                    }
                if let Some(s) = self.rws.as_ref().and_then(|l| l.system.as_ref()) {
                    ui.label(RichText::new(format!("{} · RobotWare {}", s.name, s.rw_version)).weak()).on_hover_text("From the controller's RWS (Controller menu)");
                }
            }
            if !st.others.is_empty() {
                ui.separator();
                let names = self.client_names(&st.others);
                let text = format!("Other RobAPI clients: {}", names.join(", "));
                // The pendant alone is every real IRC5's normal state: said, not warned.
                let label = if st.others.iter().all(|o| o.pendant) { RichText::new(text).weak() } else { RichText::new(text).color(theme::WARN) };
                ui.label(label).on_hover_text("Other programs connected to this controller over RobAPI. RobotStudio counts whenever it is connected, even when it is not streaming. The controller's FlexPendant is always connected, on its internal network (192.168.126.x).");
            }
            let lost = st.counters.dropped_to_taps;
            if lost > 0 {
                ui.label(RichText::new(format!("{lost} samples lost by the recorder (the disk could not keep up)")).color(theme::BAD));
            }
        });
    }

    pub fn client_names(&self, others: &[spy_core::session::OtherClient]) -> Vec<String> {
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
                if o.pendant {
                    s.push_str(" [FlexPendant]");
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
        for o in &st.others {
            let c = ctx.clone();
            net::lookup(&self.hostnames, &o.address, move || c.request_repaint());
        }
        let names = self.client_names(&st.others);
        egui::Modal::new(egui::Id::new("approval")).show(ctx, |ui| {
            ui.set_max_width(520.0);
            ui.heading("Other programs are connected to this controller");
            ui.add_space(6.0);
            for n in &names {
                ui.label(RichText::new(format!("•  {n}")).monospace());
            }
            ui.add_space(6.0);
            ui.label("Another tool, such as RobotStudio or TuneMaster, may be streaming test signals from this controller. Only one program at a time can: taking InfoStream will stop its streams.");
            ui.label(RichText::new("RobotStudio counts as a client whenever it is connected, even when it is not streaming anything.").weak());
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button(RichText::new("Take InfoStream").strong()).clicked() {
                    self.session.answer(true, &st.others);
                }
                if ui.button("Cancel").clicked() {
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
                if ui.button(RichText::new("Reset InfoStream").color(theme::BAD)).clicked() {
                    self.session.reset_infostream();
                    self.confirm_reset = false;
                }
                if ui.button("Cancel").clicked() {
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
            ui.label(RichText::new(format!("Catalogue: {} ({})", self.catalogue.title, self.catalogue.source)).weak());
        });
        self.show_about = open;

        let mut open = self.show_guide;
        egui::Window::new("Quick guide").open(&mut open).collapsible(false).default_width(520.0).pivot(egui::Align2::CENTER_CENTER).default_pos(ctx.content_rect().center()).show(ctx, |ui| {
            ui.label(RichText::new("1. Connect").strong());
            ui.label("Real IRC5: type its address (the port is 5515) and press Connect. RobotStudio virtual controller: open ▾ List and pick it; its port changes every time it starts.");
            ui.label(RichText::new("2. Only one program at a time").strong());
            ui.label("A controller streams test signals to one program at a time. Close TuneMaster's signal logging or RobotStudio's signal tools first. If other programs are connected, this program lists them and asks before taking over.");
            ui.label(RichText::new("3. Add channels").strong());
            ui.label("Pick a signal in the catalogue on the left, then Add. The dialog asks only what that signal needs: the robot, the axis, or nothing. 'Channel sets...' adds a common group in one go (both DC links, one robot's torques). Up to 12 channels.");
            ui.label(RichText::new("4. Read and chart").strong());
            ui.label("Values show on the right (a 150 ms average); charts are raw. Space pauses the charts so you can scroll back through the last 10 minutes. A reading that stops updating is dimmed and marked STALE, never shown as live.");
            ui.label(RichText::new("5. Record").strong());
            ui.label("REC records every sample. 'Save last' saves what just happened, even if nothing was recording. 'Slow log' logs averages for runs of hours. M drops a marker.");
            ui.label(RichText::new("6. Look back").strong());
            ui.label("File > Open a recording (or drop its folder on the window) charts it again, marked REVIEWING: not live. 'Save CSV' and 'Save PNG' above the charts save what is in view, live or reviewed.");
            ui.add_space(6.0);
            ui.label(RichText::new("Angles are in degrees; click an angle's unit in the channel table to switch it to radians.").weak());
        });
        self.show_guide = open;

        let mut open = self.show_catalogue_info;
        let mut load_path: Option<String> = None;
        egui::Window::new("Catalogue").open(&mut open).default_width(560.0).show(ctx, |ui| {
            ui.heading(&self.catalogue.title);
            ui.label(format!("Source: {}", self.catalogue.source));
            ui.label(&self.catalogue.measured_on);
            ui.label(RichText::new(&self.catalogue.caveat).color(theme::WARN));
            ui.label(RichText::new(&self.catalogue.credits).weak());
            let named = self.catalogue.signals.iter().filter(|s| s.named).count();
            ui.label(format!("{} signal numbers, {named} with a named quantity.", self.catalogue.signals.len()));
            ui.separator();
            ui.label("Load a catalogue file (for another robot or RobotWare version):");
            let id = egui::Id::new("catalogue-path");
            let mut path: String = ui.data_mut(|d| d.get_temp::<String>(id)).unwrap_or_default();
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut path).hint_text("C:\\path\\to\\catalogue.json").desired_width(380.0));
                if ui.button("Load").clicked() {
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
        egui::Window::new("Diagnostics").open(&mut open).default_width(520.0).show(ctx, |ui| {
            let st = self.session.status().clone();
            let c = &st.counters;
            egui::Grid::new("diag").striped(true).show(ui, |ui| {
                let mut row = |k: &str, v: String| {
                    ui.label(k);
                    ui.label(RichText::new(v).monospace());
                    ui.end_row();
                };
                row("Phase", format!("{:?}", st.phase));
                row("Controller", st.target.as_ref().map(|t| t.to_string()).unwrap_or_default());
                row("Local address", st.local.map(|a| a.to_string()).unwrap_or_default());
                row("System id", st.announce.as_ref().and_then(|a| a.system_id.clone()).unwrap_or_default());
                row("Client list as sent", st.announce.as_ref().and_then(|a| a.raw_client_list.clone()).unwrap_or_default());
                row("Subscription", st.subscription.map(|s| s.to_string()).unwrap_or_default());
                row("Frames / bytes", format!("{} / {}", c.frames, c.bytes));
                row("Sample frames / samples", format!("{} / {}", c.sample_frames, c.samples));
                row("Keepalives answered", c.ayas.to_string());
                row("Reconnect attempts", c.reconnects.to_string());
                row("Lost frame sync", c.desyncs.to_string());
                row("Frames without trailer", c.no_trailer.to_string());
                row("Records for others' streams", c.foreign_records.to_string());
                row("Other subscriptions' frames", c.foreign_subscription.to_string());
                row("Unexpected frames", c.unexpected_frames.to_string());
                row("Controller clock resets", c.clock_resets.to_string());
                row("Lost by recorders", c.dropped_to_taps.to_string());
                for (k, v) in &c.defects {
                    row(k, v.to_string());
                }
            });
        });
        self.show_diag = open;
    }

    fn log_pane(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Log").strong());
            ui.checkbox(&mut self.log_filter_warn, "warnings only");
        });
        let entries = self.log.since(self.log.next_seq().saturating_sub(400));
        egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
            for e in entries.iter().filter(|e| !self.log_filter_warn || e.level >= Level::Warn) {
                let color = match e.level {
                    Level::Info => ui.visuals().text_color(),
                    Level::Warn => theme::WARN,
                    Level::Error => theme::BAD,
                };
                ui.label(RichText::new(view::log_line(e)).color(color).monospace());
            }
        });
    }

    fn toasts(&mut self, ctx: &egui::Context) {
        self.toasts.retain(|t| t.at.elapsed() < Duration::from_secs(6));
        if self.toasts.is_empty() {
            return;
        }
        egui::Area::new(egui::Id::new("toasts")).anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-12.0, -12.0)).show(ctx, |ui| {
            for t in &self.toasts {
                let color = match t.level {
                    Level::Info => theme::OK,
                    Level::Warn => theme::WARN,
                    Level::Error => theme::BAD,
                };
                egui::Frame::popup(ui.style()).stroke(egui::Stroke::new(1.5, color)).show(ui, |ui| {
                    ui.set_max_width(420.0);
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
        let (space, m) = ctx.input(|i| (i.key_pressed(egui::Key::Space), i.key_pressed(egui::Key::M)));
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
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_background();
        self.poll_review();
        self.poll_export();
        self.poll_rws();
        self.follow_moved_controller();
        self.take_dropped(&ctx);
        self.take_screenshot(&ctx);
        self.shortcuts(&ctx);
        self.update_stats();
        self.update_derived();

        egui::Panel::top("top").show(ui, |ui| {
            self.menu(ui);
            if self.another_instance {
                ui.colored_label(theme::WARN, "Another ABB Signal Spy window is open. Two windows on the same controller break each other's streams: only one program at a time can stream test signals.");
            }
            self.controller_bar(ui);
            self.session_line(ui);
            self.review_banner(ui);
        });
        egui::Panel::bottom("log").resizable(true).default_size(130.0).min_size(60.0).show(ui, |ui| self.log_pane(ui));
        egui::Panel::left("catalogue").resizable(true).default_size(330.0).min_size(240.0).show(ui, |ui| self.browser(ui));
        // While a recording is reviewed it has the charts and the right panel; the
        // live session carries on underneath, and its line above says so.
        let central = if self.review.is_some() {
            egui::Panel::right("channels").resizable(true).default_size(430.0).min_size(300.0).show(ui, |ui| self.review_table(ui));
            egui::CentralPanel::default().show(ui, |ui| self.review_charts(ui))
        } else {
            egui::Panel::right("channels").resizable(true).default_size(430.0).min_size(300.0).show(ui, |ui| self.channel_table(ui));
            egui::CentralPanel::default().show(ui, |ui| self.charts(ui))
        };
        self.charts_rect = Some(central.response.rect);

        self.recordings_window(&ctx);
        self.rws_window(&ctx);
        self.add_dialog(&ctx);
        self.sets_dialog(&ctx);
        self.approval_dialog(&ctx);
        self.reset_dialog(&ctx);
        self.info_windows(&ctx);
        self.toasts(&ctx);
        self.publish_phone();
        self.check_recorders();
        self.update_title(&ctx);

        if self.settings_dirty.is_some_and(|t| t.elapsed() > Duration::from_secs(2)) {
            self.save_settings();
        }
        // Live charts move with the controller clock; nothing else needs a timer.
        if self.session.status().phase == Phase::Streaming && self.paused_at.is_none() {
            ctx.request_repaint_after(Duration::from_millis(33));
        } else {
            ctx.request_repaint_after(Duration::from_millis(250));
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
