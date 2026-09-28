//! The read-only RWS extras (Phase 2; C7, G16-G18): the controller's name and
//! RobotWare version, its event log on the charts and in recordings (every 5 s, a
//! setting turns it off), and a motor's commutator offset as a turn's target. GETs
//! only, on a thread of their own. The login is typed each session and never stored
//! (G16); RWS is used only while connected over InfoStream to the same controller,
//! checked by its system id.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use eframe::egui::{self, RichText};

use spy_core::derived::Derived;
use spy_core::log::Level;
use spy_core::rws::{self, Client, EventPoll, Identity, MotorCalib, NewEvents, RwsError, System};
use spy_core::session::Target;

use crate::app::SpyApp;
use crate::theme;
use crate::view;

/// How often the event log is looked at (G17).
pub const POLL: Duration = Duration::from_secs(5);
/// How often the controller's clock is compared with this PC's again.
const CLOCK_EVERY: Duration = Duration::from_secs(600);
/// Controller events kept for the charts.
const KEEP_EVENTS: usize = 500;

pub enum RwsCmd {
    Calib { derived: String, instance: String },
    Stop,
    /// An internal error in the thread, for a test.
    #[cfg(test)]
    Crash,
}

pub enum RwsMsg {
    Up { system: System, identity: Identity, offset_s: i64 },
    /// New events, and the controller's clock minus this PC's UTC when read.
    Events(NewEvents, i64),
    Calib { derived: String, result: Result<MotorCalib, String> },
    /// A look that failed; the next one is tried.
    Trouble(String),
    /// Stopped, and why.
    Failed(String),
}

/// A logged-in RWS session, on its thread.
pub struct RwsLink {
    pub target: Target,
    pub port: u16,
    pub system: Option<System>,
    pub identity: Option<Identity>,
    /// The controller's clock minus this PC's UTC, seconds (its local time zone and
    /// its drift).
    pub offset_s: i64,
    pub trouble: Option<String>,
    pub events_on: Arc<AtomicBool>,
    tx: Sender<RwsCmd>,
    rx: Receiver<RwsMsg>,
}

impl Drop for RwsLink {
    fn drop(&mut self) {
        // Not waited for: a request under way ends within its timeout.
        let _ = self.tx.send(RwsCmd::Stop);
    }
}

/// An entry of the controller's event log, on this PC's clock.
#[derive(Debug, Clone, PartialEq)]
pub struct ControllerEvent {
    pub utc_ms: i64,
    pub code: u32,
    pub severity: u8,
    pub title: String,
}

impl ControllerEvent {
    pub fn text(&self) -> String {
        format!("{} {}", self.code, self.title)
    }
    pub fn color(&self) -> egui::Color32 {
        match self.severity {
            3 => theme::BAD,
            2 => theme::WARN,
            _ => theme::IDLE,
        }
    }
}

/// The login form: never saved (the port is, in the settings).
#[derive(Debug, Clone)]
pub struct RwsForm {
    pub user: String,
    pub password: String,
}

impl Default for RwsForm {
    fn default() -> RwsForm {
        RwsForm { user: rws::DEFAULT_USER.into(), password: String::new() }
    }
}

fn pc_now_s() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

#[allow(clippy::too_many_arguments)]
fn worker(host: String, port: u16, user: String, password: String, expect: String, events_on: Arc<AtomicBool>, poll: Duration, rx: Receiver<RwsCmd>, tx: Sender<RwsMsg>, ctx: egui::Context) {
    let send = |m: RwsMsg| {
        let _ = tx.send(m);
        ctx.request_repaint();
    };
    // Only the controller streaming: RWS on another port could be another (a second
    // virtual one), and another behind the same address answers a later login.
    let mut c = Client::new(&host, port, &user, &password).expect_system(&expect);
    let why = |e: RwsError| match e {
        RwsError::OtherController { found, expected } => format!("RWS at {host}:{port} is a different controller (system id {found}) from the one streaming ({expected})"),
        e => e.to_string(),
    };
    let up = (|| -> Result<(System, Identity, i64), RwsError> {
        let s = c.system()?;
        let i = c.identity()?;
        let offset = c.clock()? - pc_now_s();
        Ok((s, i, offset))
    })();
    let (system, identity, mut offset) = match up {
        Ok(x) => x,
        Err(e) => return send(RwsMsg::Failed(why(e))),
    };
    send(RwsMsg::Up { system, identity, offset_s: offset });
    let mut events = EventPoll::default();
    let mut next_look = Instant::now();
    let mut next_clock = Instant::now() + CLOCK_EVERY;
    loop {
        let wait = next_look.saturating_duration_since(Instant::now()).max(Duration::from_millis(20));
        match rx.recv_timeout(wait) {
            Ok(RwsCmd::Stop) | Err(RecvTimeoutError::Disconnected) => return,
            Ok(RwsCmd::Calib { derived, instance }) => match c.motor_calib(&instance) {
                Err(e @ RwsError::OtherController { .. }) => return send(RwsMsg::Failed(why(e))),
                result => send(RwsMsg::Calib { derived, result: result.map_err(|e| e.to_string()) }),
            },
            #[cfg(test)]
            Ok(RwsCmd::Crash) => panic!("a crash a test asked for"),
            Err(RecvTimeoutError::Timeout) => {}
        }
        if Instant::now() < next_look {
            continue;
        }
        next_look = Instant::now() + poll;
        if Instant::now() >= next_clock {
            match c.clock() {
                Ok(t) => {
                    offset = t - pc_now_s();
                    next_clock = Instant::now() + CLOCK_EVERY;
                }
                Err(e @ RwsError::OtherController { .. }) => return send(RwsMsg::Failed(why(e))),
                // Tried again at the next look.
                Err(_) => {}
            }
        }
        if !events_on.load(Ordering::SeqCst) {
            continue;
        }
        match events.look(&mut c) {
            Ok(n) if n.events.is_empty() && !n.skipped => {}
            Ok(n) => send(RwsMsg::Events(n, offset)),
            Err(e @ (RwsError::Login | RwsError::OtherController { .. })) => return send(RwsMsg::Failed(why(e))),
            Err(e) => send(RwsMsg::Trouble(e.to_string())),
        }
    }
}

impl SpyApp {
    /// Logged in, and the controller known.
    pub fn rws_ready(&self) -> bool {
        self.rws.as_ref().is_some_and(|l| l.system.is_some())
    }

    /// Log in to the connected controller's RWS with the form's login.
    pub fn rws_login(&mut self) {
        let st = self.session.status().clone();
        let (Some(target), Some(expect)) = (st.target.clone(), st.announce.as_ref().and_then(|a| a.system_id.clone())) else {
            return self.toast(Level::Warn, "Connect to the controller first: RWS is checked against it.");
        };
        if self.rws_form.password.is_empty() {
            return self.toast(Level::Warn, "Type the RWS password (RobotWare's default user has \"robotics\").");
        }
        let port = self.settings.rws_port;
        let events_on = Arc::new(AtomicBool::new(self.settings.rws_events));
        let (ctx, host, user, password, poll) = (self.ctx.clone(), target.host.clone(), self.rws_form.user.clone(), self.rws_form.password.clone(), self.rws_poll);
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (msg_tx, msg_rx) = mpsc::channel();
        let on = events_on.clone();
        let started = std::thread::Builder::new()
            .name("rws".into())
            .spawn(move || {
                // An internal error ends RWS and says so, rather than leave it shown
                // logged in with nothing arriving.
                let (tx, repaint) = (msg_tx.clone(), ctx.clone());
                if let Err(p) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || worker(host, port, user, password, expect, on, poll, cmd_rx, msg_tx, ctx))) {
                    let what = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_else(|| "unknown".into());
                    let _ = tx.send(RwsMsg::Failed(format!("stopped after an internal error ({what})")));
                    repaint.request_repaint();
                }
            });
        if let Err(e) = started {
            return self.toast(Level::Error, format!("RWS: its thread could not be started ({e})."));
        }
        self.log.info(format!("Logging in to RWS at {}:{port} as {}.", target.host, self.rws_form.user));
        self.rws = Some(RwsLink { target, port, system: None, identity: None, offset_s: 0, trouble: None, events_on, tx: cmd_tx, rx: msg_rx });
    }

    #[cfg(test)]
    pub fn crash_rws_for_test(&self) {
        if let Some(l) = &self.rws {
            let _ = l.tx.send(RwsCmd::Crash);
        }
    }

    pub fn rws_logout(&mut self) {
        // Typed each session (G16), and not kept past it.
        self.rws_form.password.clear();
        if self.rws.take().is_some() {
            self.log.info("Logged out of RWS.");
        }
    }

    /// Each frame: what the RWS thread sent, and whether it still belongs to the
    /// controller connected.
    pub fn poll_rws(&mut self) {
        let same = {
            let Some(link) = &self.rws else { return };
            let st = self.session.status();
            st.target.as_ref() == Some(&link.target) && st.phase.is_active()
        };
        if !same {
            self.rws = None;
            self.log.info("RWS closed: the controller connection ended or changed.");
            return;
        }
        let msgs: Vec<RwsMsg> = self.rws.as_ref().map(|l| l.rx.try_iter().collect()).unwrap_or_default();
        for m in msgs {
            match m {
                RwsMsg::Up { system, identity, offset_s } => {
                    self.log.info(format!("RWS: {} ({}), RobotWare {}, controller clock {offset_s:+} s from UTC.", system.name, identity.name, system.rw_version));
                    if let Some(l) = &mut self.rws {
                        l.system = Some(system);
                        l.identity = Some(identity);
                        l.offset_s = offset_s;
                        l.trouble = None;
                    }
                }
                RwsMsg::Events(n, offset_s) => self.take_events(n, offset_s),
                RwsMsg::Calib { derived, result } => self.take_calib(&derived, result),
                RwsMsg::Trouble(e) => {
                    if let Some(l) = &mut self.rws {
                        l.trouble = Some(e.clone());
                    }
                    self.log.warn(format!("RWS: {e}; trying again."));
                }
                RwsMsg::Failed(e) => {
                    self.rws = None;
                    self.toast(Level::Error, format!("RWS: {e}."));
                }
            }
        }
    }

    fn take_events(&mut self, n: NewEvents, offset_s: i64) {
        if n.skipped {
            self.log.warn("More controller events arrived in one look than are read at once: the oldest of them are not shown.");
        }
        if let Some(l) = &mut self.rws {
            l.trouble = None;
        }
        // Recorders keep those since they started (the log's stamps are whole seconds).
        let now = SystemTime::now();
        let starts: Vec<Option<i64>> = [&self.recorder, &self.slow].iter().map(|r| r.as_ref().map(|r| now.checked_sub(r.status().started.elapsed()).and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as i64))).collect();
        for e in n.events {
            let ev = ControllerEvent { utc_ms: (e.time - offset_s) * 1000, code: e.code, severity: e.severity, title: e.title.clone() };
            for (r, start) in [&self.recorder, &self.slow].into_iter().zip(&starts) {
                if let (Some(r), Some(s)) = (r, start)
                    && ev.utc_ms >= s - 1000
                {
                    let wall = UNIX_EPOCH + Duration::from_millis(ev.utc_ms.max(0) as u64);
                    r.event("controller-event", &format!("{} ({})", ev.text(), e.severity_word()), wall);
                }
            }
            if e.severity >= 3 {
                self.log.warn(format!("Controller event: {}", ev.text()));
            }
            self.controller_events.push(ev);
        }
        let excess = self.controller_events.len().saturating_sub(KEEP_EVENTS);
        self.controller_events.drain(..excess);
    }

    /// Ask the controller for a turn's target: its motor's commutator offset.
    pub fn request_com_offset(&mut self, i: usize) {
        let Derived::Turn { angle, .. } = self.derived[i].live.def().clone() else { return };
        let Some(instance) = rws::calib_instance(angle.unit.as_str(), angle.axis.one_based()) else {
            return self.toast(Level::Warn, "Only a robot's axes (ROB_1, ROB_2, ...) have their calibration named this way.");
        };
        let id = self.derived[i].live.def().id();
        if let Some(l) = &self.rws {
            let _ = l.tx.send(RwsCmd::Calib { derived: id, instance });
        }
    }

    fn take_calib(&mut self, derived: &str, result: Result<MotorCalib, String>) {
        let Some(i) = self.derived.iter().position(|d| d.live.def().id() == derived) else { return };
        match result {
            Ok(m) => {
                let deg = m.com_offset.to_degrees();
                self.derived[i].target_text = view::fmt(deg);
                self.set_target(i);
                let text = format!("Target from the controller: MOTOR_CALIB {} com_offset (Commutator Offset) {} rad = {} deg.", m.instance, m.com_offset, view::fmt(deg));
                if m.com_valid {
                    self.toast(Level::Info, text);
                } else {
                    self.toast(Level::Warn, format!("{text} The controller marks it NOT valid."));
                }
            }
            Err(e) => self.toast(Level::Error, format!("Could not read the commutator offset: {e}.")),
        }
    }

    /// The controller events on the live timeline: (timeline ms, event).
    pub fn events_on_timeline(&self, tl: &spy_core::timeline::Timeline) -> Vec<(i64, ControllerEvent)> {
        let Some((at, wall)) = tl.anchor() else { return Vec::new() };
        let Ok(w) = wall.duration_since(UNIX_EPOCH) else { return Vec::new() };
        let wall_ms = w.as_millis() as i64;
        self.controller_events.iter().map(|e| (at + (e.utc_ms - wall_ms), e.clone())).collect()
    }

    pub fn rws_window(&mut self, ctx: &egui::Context) {
        if !self.show_rws {
            return;
        }
        let mut open = true;
        let mut login = false;
        let mut logout = false;
        let mut dirty = false;
        // Centred: in the corner it covered the controller bar and its Disconnect.
        egui::Window::new("Controller details (RWS)").open(&mut open).collapsible(false).default_width(460.0).pivot(egui::Align2::CENTER_CENTER).default_pos(ctx.content_rect().center()).show(ctx, |ui| {
            ui.label(RichText::new("Read-only: the controller's name and RobotWare version, its event log on the charts, and a motor's commutator offset. The login is not stored anywhere.").small().weak());
            let st = self.session.status().clone();
            let Some(target) = st.target.clone().filter(|_| st.phase.is_connected()) else {
                ui.colored_label(theme::WARN, "Connect to the controller first: RWS is checked against it.");
                return;
            };
            match &self.rws {
                None => {
                    egui::Grid::new("rws-login").num_columns(2).show(ui, |ui| {
                        ui.label("Controller");
                        ui.label(RichText::new(&target.host).monospace());
                        ui.end_row();
                        ui.label("RWS port");
                        ui.add(egui::DragValue::new(&mut self.settings.rws_port).range(1..=65535)).on_hover_text("80 on a real IRC5; a virtual controller's may differ");
                        ui.end_row();
                        ui.label("User");
                        ui.text_edit_singleline(&mut self.rws_form.user);
                        ui.end_row();
                        ui.label("Password");
                        let r = ui.add(egui::TextEdit::singleline(&mut self.rws_form.password).password(true));
                        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            login = true;
                        }
                        ui.end_row();
                    });
                    if ui.button(RichText::new("Log in").strong()).clicked() {
                        login = true;
                    }
                }
                Some(l) => match (&l.system, &l.identity) {
                    (Some(s), Some(i)) => {
                        egui::Grid::new("rws-info").num_columns(2).show(ui, |ui| {
                            ui.label("Controller");
                            ui.label(format!("{} ({})", i.name, i.kind));
                            ui.end_row();
                            ui.label("Robot system");
                            ui.label(&s.name);
                            ui.end_row();
                            ui.label("RobotWare");
                            ui.label(&s.rw_version);
                            ui.end_row();
                            ui.label("System id");
                            ui.label(RichText::new(format!("{} (the same as InfoStream's)", s.system_id)).small());
                            ui.end_row();
                            ui.label("Its clock");
                            ui.label(format!("{} from UTC (its local time; event times shown to the second)", offset_text(l.offset_s)));
                            ui.end_row();
                        });
                        let mut on = self.settings.rws_events;
                        if ui.checkbox(&mut on, "Event log on the charts and in recordings (a look every 5 s)").changed() {
                            self.settings.rws_events = on;
                            l.events_on.store(on, Ordering::SeqCst);
                            dirty = true;
                        }
                        if let Some(t) = &l.trouble {
                            ui.colored_label(theme::WARN, format!("Last look failed: {t}"));
                        }
                        if ui.button("Log out").clicked() {
                            logout = true;
                        }
                    }
                    _ => {
                        ui.label(format!("Logging in to {}:{} ...", l.target.host, l.port));
                    }
                },
            }
        });
        if login {
            self.rws_login();
        }
        if logout {
            self.rws_logout();
        }
        if dirty {
            self.mark_settings_dirty();
        }
        self.show_rws = open;
    }
}

/// `+4 h 00 min`, `-0 min 12 s`.
pub fn offset_text(s: i64) -> String {
    let sign = if s < 0 { "-" } else { "+" };
    let a = s.unsigned_abs();
    if a >= 3600 { format!("{sign}{} h {:02} min", a / 3600, a / 60 % 60) } else { format!("{sign}{} min {:02} s", a / 60, a % 60) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clock_offset_reads_for_people() {
        assert_eq!(offset_text(-14_400), "-4 h 00 min");
        assert_eq!(offset_text(3_630), "+1 h 00 min");
        assert_eq!(offset_text(-12), "-0 min 12 s");
        assert_eq!(offset_text(0), "+0 min 00 s");
    }
}
