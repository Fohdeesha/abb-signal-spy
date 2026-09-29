//! The read-only RWS extras: the controller's name and
//! RobotWare version, its event log on the charts and in recordings (every 5 s, a
//! setting turns it off), and a motor's commutator offset as a turn's target. GETs
//! only, on a thread of their own. The login is typed each session and never stored;
//! RWS is used only while connected over InfoStream to the same controller,
//! checked by its system id.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use eframe::egui::{self, RichText};

use spy_core::derived::Derived;
use spy_core::log::Level;
use spy_core::recording::EventEntry;
use spy_core::rws::{self, Client, EventPoll, Identity, MotorCalib, NewEvents, RwsError, System};
use spy_core::session::Target;

use crate::app::SpyApp;
use crate::theme;
use crate::view;

/// How often the event log is looked at.
pub const POLL: Duration = Duration::from_secs(5);
/// With the event log off, the controller's clock is read every this many looks
/// (with it on, at every look): often enough to notice a refused login too.
const CLOCK_LOOKS: u32 = 12;
/// Controller events kept for the charts.
const KEEP_EVENTS: usize = 500;
/// How long a recording just closed (or a "Save last" just written) still takes in
/// controller events that arrive late: a look's interval and its longest request.
const LATE_FOR: Duration = Duration::from_secs(20);

pub enum RwsCmd {
    Calib { derived: String, instance: String },
    /// Look at the event log now (a recording just stopped).
    LookNow,
    Stop,
    /// An internal error in the thread, for a test.
    #[cfg(test)]
    Crash,
}

pub enum RwsMsg {
    Up { system: System, identity: Option<Identity>, offset_ms: i64 },
    /// New events, and the controller's clock minus this PC's UTC (ms) when read.
    Events(NewEvents, i64),
    /// The controller's clock minus this PC's UTC (ms), read again and changed.
    Clock(i64),
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
    /// A display name only: `None` where this RobotWare does not give it.
    pub identity: Option<Identity>,
    /// The controller's clock minus this PC's UTC, ms (its local time zone and its
    /// drift), as last read.
    pub offset_ms: i64,
    pub trouble: Option<String>,
    pub events_on: Arc<AtomicBool>,
    /// Unreadable entries of the event log have been said once.
    told_unreadable: bool,
    tx: Sender<RwsCmd>,
    rx: Receiver<RwsMsg>,
}

/// A recording closed a moment ago, or a "Save last" just written: controller events
/// still on their way whose time falls within it go into its recording.json.
pub struct LateWindow {
    pub dir: PathBuf,
    pub from_ms: i64,
    pub to_ms: i64,
    pub until: Instant,
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
    /// The log's own id (rises with time).
    pub id: u64,
    pub utc_ms: i64,
    pub code: u32,
    pub severity: u8,
    pub title: String,
}

impl ControllerEvent {
    pub fn text(&self) -> String {
        format!("{} {}", self.code, self.title)
    }
    /// As a recording keeps it, or `None` for a time no clock can show.
    pub fn entry(&self) -> Option<EventEntry> {
        let wall = UNIX_EPOCH.checked_add(Duration::from_millis(u64::try_from(self.utc_ms).ok()?))?;
        Some(EventEntry { utc: spy_core::util::wall_iso(wall), kind: "controller-event".into(), text: format!("{} ({})", self.text(), rws::severity_word(self.severity)), controller_ms: None })
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

fn pc_now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// The controller's clock minus this PC's UTC, ms, read now: a read that took too
/// long to place events by is tried again, three times at most.
fn read_offset(c: &mut Client) -> Result<i64, RwsError> {
    for _ in 0..3 {
        let before = pc_now_ms();
        let t = c.clock()?;
        if let Some(o) = rws::clock_offset_ms(t, before, pc_now_ms()) {
            return Ok(o);
        }
    }
    Err(RwsError::Format(format!("the controller's clock answered too slowly (over {} ms, three times) to place its events to the second", rws::MAX_CLOCK_RTT_MS)))
}

/// Whether an RWS error ends the session: the login refused, or another controller.
fn ends_session(e: &RwsError) -> bool {
    matches!(e, RwsError::Login | RwsError::OtherController { .. })
}

#[allow(clippy::too_many_arguments)]
fn worker(host: String, port: u16, user: String, password: String, expect: String, seed: Option<u64>, events_on: Arc<AtomicBool>, poll: Duration, rx: Receiver<RwsCmd>, tx: Sender<RwsMsg>, ctx: egui::Context) {
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
    let up = (|| -> Result<(System, Option<Identity>, i64), RwsError> {
        let s = c.system()?;
        // A display name only: a RobotWare without it still gives everything else.
        let i = match c.identity() {
            Ok(i) => Some(i),
            Err(e) if ends_session(&e) => return Err(e),
            Err(_) => None,
        };
        let offset = read_offset(&mut c)?;
        Ok((s, i, offset))
    })();
    let (system, identity, mut offset) = match up {
        Ok(x) => x,
        Err(e) => return send(RwsMsg::Failed(why(e))),
    };
    send(RwsMsg::Up { system, identity, offset_ms: offset });
    // A login again reads back to the newest event already known.
    let mut events = seed.map_or_else(EventPoll::default, EventPoll::after);
    let mut next_look = Instant::now();
    let mut looks = 0u32;
    loop {
        let wait = next_look.saturating_duration_since(Instant::now()).max(Duration::from_millis(20));
        match rx.recv_timeout(wait) {
            Ok(RwsCmd::Stop) | Err(RecvTimeoutError::Disconnected) => return,
            Ok(RwsCmd::Calib { derived, instance }) => match c.motor_calib(&instance) {
                Err(e) if ends_session(&e) => return send(RwsMsg::Failed(why(e))),
                result => send(RwsMsg::Calib { derived, result: result.map_err(|e| e.to_string()) }),
            },
            Ok(RwsCmd::LookNow) => next_look = Instant::now(),
            #[cfg(test)]
            Ok(RwsCmd::Crash) => panic!("a crash a test asked for"),
            Err(RecvTimeoutError::Timeout) => {}
        }
        if Instant::now() < next_look {
            continue;
        }
        next_look = Instant::now() + poll;
        let on = events_on.load(Ordering::SeqCst);
        looks += 1;
        // The clock at every look while the event log is read, whose whole-second
        // stamps are placed through it (a clock set on is followed within a look);
        // otherwise every CLOCK_LOOKS looks, which also keeps the login checked.
        if on || looks >= CLOCK_LOOKS {
            looks = 0;
            match read_offset(&mut c) {
                // Two reads of an unchanged clock differ by less than a second (its
                // whole seconds); a second or more is the clock itself set on or back.
                Ok(o) if (o - offset).abs() >= 1000 => {
                    offset = o;
                    send(RwsMsg::Clock(o));
                }
                Ok(_) => {}
                Err(e) if ends_session(&e) => return send(RwsMsg::Failed(why(e))),
                Err(e) => {
                    send(RwsMsg::Trouble(e.to_string()));
                    continue;
                }
            }
        }
        if !on {
            continue;
        }
        let mark = events.mark();
        match events.look(&mut c) {
            Ok(n) if n.events.is_empty() && !n.skipped && n.unreadable == 0 && !n.renumbered => {}
            Ok(n) => {
                // The clock read again: set on or back during the look, it leaves these
                // events stamped on either clock, placed by neither reading for sure.
                // Read again at the next look, with the clock settled.
                match read_offset(&mut c) {
                    Ok(o) if (o - offset).abs() >= 1000 => {
                        events.restore(mark);
                        offset = o;
                        send(RwsMsg::Clock(o));
                        next_look = Instant::now();
                    }
                    Err(e) if ends_session(&e) => return send(RwsMsg::Failed(why(e))),
                    _ => send(RwsMsg::Events(n, offset)),
                }
            }
            Err(e) if ends_session(&e) => return send(RwsMsg::Failed(why(e))),
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
        // The events already shown are this controller's (they go when another's
        // history starts): a login again reads back to the newest of them.
        let seed = self.controller_events.iter().map(|e| e.id).max();
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (msg_tx, msg_rx) = mpsc::channel();
        let on = events_on.clone();
        let started = std::thread::Builder::new()
            .name("rws".into())
            .spawn(move || {
                // An internal error ends RWS and says so, rather than leave it shown
                // logged in with nothing arriving.
                let (tx, repaint) = (msg_tx.clone(), ctx.clone());
                if let Err(p) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || worker(host, port, user, password, expect, seed, on, poll, cmd_rx, msg_tx, ctx))) {
                    let what = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_else(|| "unknown".into());
                    let _ = tx.send(RwsMsg::Failed(format!("stopped after an internal error ({what})")));
                    repaint.request_repaint();
                }
            });
        if let Err(e) = started {
            return self.toast(Level::Error, format!("RWS: its thread could not be started ({e})."));
        }
        // No login in the log file: not even the user name.
        self.log.info(format!("Logging in to RWS at {}:{port}.", target.host));
        self.rws = Some(RwsLink { target, port, system: None, identity: None, offset_ms: 0, trouble: None, events_on, told_unreadable: false, tx: cmd_tx, rx: msg_rx });
    }

    /// End the RWS session, whatever the reason; its password goes with it.
    fn close_rws(&mut self) {
        self.rws = None;
        self.rws_form.password.clear();
    }

    #[cfg(test)]
    pub fn crash_rws_for_test(&self) {
        if let Some(l) = &self.rws {
            let _ = l.tx.send(RwsCmd::Crash);
        }
    }

    pub fn rws_logout(&mut self) {
        if self.rws.is_some() {
            self.log.info("Logged out of RWS.");
        }
        self.close_rws();
    }

    /// Each frame: what the RWS thread sent, and whether it still belongs to the
    /// controller connected.
    pub fn poll_rws(&mut self) {
        self.late_windows.retain(|w| w.until > Instant::now());
        let (same_target, active) = {
            let Some(link) = &self.rws else { return };
            let st = self.session.status();
            (st.target.as_ref() == Some(&link.target), st.phase.is_active())
        };
        // What the thread already sent from this controller is taken in first, even
        // when the session has just ended: an event received is not dropped.
        if same_target {
            let msgs: Vec<RwsMsg> = self.rws.as_ref().map(|l| l.rx.try_iter().collect()).unwrap_or_default();
            for m in msgs {
                self.take_rws(m);
            }
        }
        if self.rws.is_some() && !(same_target && active) {
            self.close_rws();
            self.log.info("RWS closed: the controller connection ended or changed.");
        }
    }

    fn take_rws(&mut self, m: RwsMsg) {
        match m {
            RwsMsg::Up { system, identity, offset_ms } => {
                let name = identity.as_ref().map_or_else(|| "its name not given".to_string(), |i| i.name.clone());
                self.log.info(format!("RWS: {} ({name}), RobotWare {}, controller clock {} from UTC.", system.name, system.rw_version, offset_text(offset_ms)));
                if let Some(l) = &mut self.rws {
                    l.system = Some(system);
                    l.identity = identity;
                    l.offset_ms = offset_ms;
                    l.trouble = None;
                }
            }
            RwsMsg::Events(n, offset_ms) => self.take_events(n, offset_ms),
            RwsMsg::Clock(offset_ms) => {
                self.log.info(format!("The controller's clock is now {} from UTC (it was set, or changed to or from summer time); its events are placed through that.", offset_text(offset_ms)));
                if let Some(l) = &mut self.rws {
                    l.offset_ms = offset_ms;
                }
            }
            RwsMsg::Calib { derived, result } => self.take_calib(&derived, result),
            RwsMsg::Trouble(e) => {
                if let Some(l) = &mut self.rws {
                    l.trouble = Some(e.clone());
                }
                self.log.warn(format!("RWS: {e}; trying again."));
            }
            RwsMsg::Failed(e) => {
                self.close_rws();
                self.toast(Level::Error, format!("RWS: {e}."));
            }
        }
    }

    fn take_events(&mut self, n: NewEvents, offset_ms: i64) {
        if n.skipped {
            self.log.warn("More controller events arrived in one look than are read at once: the oldest of them are not shown.");
        }
        if n.renumbered {
            self.log.info("The controller's event log was cleared or renumbered: read afresh.");
        }
        if let Some(l) = &mut self.rws {
            l.trouble = None;
            if n.unreadable > 0 && !std::mem::replace(&mut l.told_unreadable, true) {
                let text = format!("{} entr{} of the controller's event log could not be read (an odd time stamp or name) and {} left out.", n.unreadable, if n.unreadable == 1 { "y" } else { "ies" }, if n.unreadable == 1 { "is" } else { "are" });
                self.log.warn(text);
            }
        }
        // Recorders keep those since they started, with the slack the whole-second
        // stamps need; recordings just closed, those of their stretch.
        let now = SystemTime::now();
        let starts: Vec<Option<i64>> = [&self.recorder, &self.slow].iter().map(|r| r.as_ref().map(|r| now.checked_sub(r.status().started.elapsed()).and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as i64))).collect();
        let mut late: Vec<(PathBuf, EventEntry)> = Vec::new();
        // A login again reads on from the newest event already shown (the poll's seed),
        // so none comes twice; a cleared log starts its ids again, so an id here says
        // nothing about one already shown.
        for e in n.events {
            let ev = ControllerEvent { id: e.id, utc_ms: rws::event_utc_ms(e.time, offset_ms), code: e.code, severity: e.severity, title: e.title.clone() };
            let Some(entry) = ev.entry() else { continue };
            let wall = spy_core::util::parse_iso(&entry.utc).unwrap_or(now);
            for (r, start) in [&self.recorder, &self.slow].into_iter().zip(&starts) {
                if let (Some(r), Some(s)) = (r, start)
                    && ev.utc_ms >= s - rws::EVENT_SLACK_MS
                {
                    r.event("controller-event", &entry.text, wall);
                }
            }
            for w in &self.late_windows {
                if ev.utc_ms >= w.from_ms - rws::EVENT_SLACK_MS && ev.utc_ms <= w.to_ms + rws::EVENT_SLACK_MS {
                    late.push((w.dir.clone(), entry.clone()));
                }
            }
            if e.severity >= 3 {
                self.log.warn(format!("Controller event at {} (this PC's clock, to the second): {}", local_hms(ev.utc_ms), ev.text()));
            }
            self.controller_events.push(ev);
        }
        let excess = self.controller_events.len().saturating_sub(KEEP_EVENTS);
        self.controller_events.drain(..excess);
        let mut dirs: Vec<PathBuf> = late.iter().map(|(d, _)| d.clone()).collect();
        dirs.dedup();
        for dir in dirs {
            let add: Vec<EventEntry> = late.iter().filter(|(d, _)| *d == dir).map(|(_, e)| e.clone()).collect();
            if let Err(e) = spy_core::recording::append_events(&dir, &add) {
                self.log.warn(format!("A controller event that arrived after {} closed could not be added to it: {e}", dir.display()));
            }
        }
    }

    /// A recording just closed, or a "Save last" just written, covering `from_ms` to
    /// `to_ms` (UTC): controller events of that stretch still on their way go into it,
    /// and the event log is looked at straight away. Those already shown go in now.
    pub fn rws_after_recording(&mut self, dir: PathBuf, from_ms: i64, to_ms: i64) {
        let known: Vec<EventEntry> = self.controller_events.iter().filter(|e| e.utc_ms >= from_ms - rws::EVENT_SLACK_MS && e.utc_ms <= to_ms + rws::EVENT_SLACK_MS).filter_map(ControllerEvent::entry).collect();
        if !known.is_empty()
            && let Err(e) = spy_core::recording::append_events(&dir, &known)
        {
            self.log.warn(format!("The controller events could not be added to {}: {e}", dir.display()));
        }
        if let Some(l) = &self.rws
            && l.system.is_some()
            && l.events_on.load(Ordering::SeqCst)
        {
            self.late_windows.push(LateWindow { dir, from_ms, to_ms, until: Instant::now() + LATE_FOR });
            let _ = l.tx.send(RwsCmd::LookNow);
        }
    }

    /// Ask the controller for a turn's target: its motor's commutator offset.
    pub fn request_com_offset(&mut self, i: usize) {
        let Derived::Turn { angle, .. } = self.derived[i].live.def().clone() else { return };
        let instance = match crate::derived_view::commutator_instance(&angle) {
            Ok(i) => i,
            Err(why) => return self.toast(Level::Warn, why),
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
                // This controller's value: it goes when another is streaming, and is
                // not saved (a typed target is the person's).
                self.derived[i].target_from = self.session.status().announce.as_ref().and_then(|a| a.system_id.clone());
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
            match &self.rws {
                None => {
                    let Some(target) = st.target.clone().filter(|_| st.phase.is_connected()) else {
                        ui.colored_label(theme::WARN, "Connect to the controller first: RWS is checked against it.");
                        return;
                    };
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
                Some(l) => match &l.system {
                    Some(s) => {
                        if !st.phase.is_connected() {
                            // The link stays through a reconnect to the same address;
                            // every login checks it is still the same controller.
                            ui.colored_label(theme::WARN, "InfoStream is reconnecting. RWS carries on with the same controller (checked by its system id at every login).");
                        }
                        egui::Grid::new("rws-info").num_columns(2).show(ui, |ui| {
                            ui.label("Controller");
                            ui.label(l.identity.as_ref().map_or_else(|| "(this RobotWare does not give its name)".to_string(), |i| format!("{} ({})", i.name, i.kind)));
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
                            ui.label(format!("{} from UTC (its local time; event times shown to the second)", offset_text(l.offset_ms)));
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
                    None => {
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

/// A clock offset in ms, to the second: `+4 h 00 min`, `-0 min 12 s`.
pub fn offset_text(ms: i64) -> String {
    let s = (ms as f64 / 1000.0).round() as i64;
    let sign = if s < 0 { "-" } else { "+" };
    let a = s.unsigned_abs();
    if a >= 3600 { format!("{sign}{} h {:02} min", a / 3600, a / 60 % 60) } else { format!("{sign}{} min {:02} s", a / 60, a % 60) }
}

/// A UTC time in ms as this PC's local clock, to the second.
fn local_hms(utc_ms: i64) -> String {
    u64::try_from(utc_ms).ok().and_then(|ms| UNIX_EPOCH.checked_add(Duration::from_millis(ms))).and_then(spy_core::util::local_parts).map_or_else(|| "an unknown time".into(), |(_, _, _, h, m, s, _)| format!("{h:02}:{m:02}:{s:02}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clock_offset_reads_for_people() {
        assert_eq!(offset_text(-14_400_000), "-4 h 00 min");
        assert_eq!(offset_text(3_630_000), "+1 h 00 min");
        assert_eq!(offset_text(-12_000), "-0 min 12 s");
        assert_eq!(offset_text(-12_400), "-0 min 12 s", "to the second");
        assert_eq!(offset_text(0), "+0 min 00 s");
    }
}
