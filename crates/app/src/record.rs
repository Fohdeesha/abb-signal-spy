use std::sync::Arc;

use eframe::egui::{self, RichText};

use spy_core::log::Level;
use spy_core::recording::{self, RecState, Recorder};

use crate::app::SpyApp;
use crate::fields;
use crate::phone::PhoneServer;
use crate::theme;

const SLOW_INTERVALS: [(u32, &str); 5] = [(500, "0.5 s"), (1000, "1 s"), (5000, "5 s"), (10_000, "10 s"), (60_000, "1 min")];
const SAVE_LAST: [(f64, &str); 6] = [(10.0, "10 s"), (30.0, "30 s"), (60.0, "1 min"), (120.0, "2 min"), (300.0, "5 min"), (600.0, "10 min")];

pub fn size_text(bytes: u64) -> String {
    if bytes < 1024 * 1024 { format!("{} KB", bytes / 1024) } else { format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0)) }
}

pub fn clock(secs: u64) -> String {
    if secs < 3600 { format!("{:02}:{:02}", secs / 60, secs % 60) } else { format!("{}:{:02}:{:02}", secs / 3600, (secs / 60) % 60, secs % 60) }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

pub fn usable_folder(p: &std::path::Path) -> Result<(), String> {
    if p.as_os_str().is_empty() {
        return Err("Type the folder recordings should go to.".into());
    }
    if !p.is_absolute() {
        return Err(format!("\"{}\" is not a whole path (one like D:\\Recordings).", p.display()));
    }
    std::fs::create_dir_all(p).map_err(|e| format!("Cannot use {} for recordings: {e}", p.display()))?;
    let probe = p.join(format!(".abb-signal-spy-{}.tmp", std::process::id()));
    std::fs::write(&probe, b"").map_err(|e| format!("Cannot write in {}: {e}", p.display()))?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

pub fn seconds_text(s: f64) -> String {
    SAVE_LAST.iter().find(|(v, _)| (v - s).abs() < 1e-9).map(|(_, t)| t.to_string()).unwrap_or_else(|| format!("{s:.0} s"))
}

fn red_with_mark(ui: &mut egui::Ui, text: &str, square: bool, enabled: bool) -> egui::Response {
    let id = ui.next_auto_id().with("mark");
    let p = theme::pal(ui);
    let r = ui
        .add_enabled_ui(enabled, |ui| {
            let w = &mut ui.visuals_mut().widgets;
            for (v, fill) in [(&mut w.inactive, p.panic), (&mut w.hovered, p.panic_hover), (&mut w.active, p.panic_pressed)] {
                v.weak_bg_fill = fill;
                v.bg_stroke = egui::Stroke::new(1.0, p.panic);
                v.fg_stroke = egui::Stroke::new(1.5, p.on_panic);
            }
            egui::Button::new((egui::Atom::custom(id, egui::vec2(12.0, 12.0)), text)).min_size(egui::vec2(0.0, fields::HEIGHT)).atom_ui(ui)
        })
        .inner;
    if let Some(rect) = r.rect(id) {
        let c = if enabled { p.on_panic } else { p.on_panic.gamma_multiply(0.75) };
        if square {
            ui.painter().rect_filled(rect, 0.0, c);
        } else {
            ui.painter().circle_filled(rect.center(), 6.0, c);
        }
    }
    r.response
}

impl SpyApp {
    pub fn record_controls(&mut self, ui: &mut egui::Ui) {
        let have = !self.chans.is_empty();
        let why_not = "Add a channel first.";
        let status = self.recorder.as_ref().map(|r| r.status());
        let dir = self.record_dir();
        let mut clicked = false;
        let mut label = std::mem::take(&mut self.rec_label);
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let r = match &status {
                None => red_with_mark(ui, "record", false, have).on_hover_text("Record every sample of every channel to a new folder").on_disabled_hover_text(why_not),
                Some(s) => red_with_mark(ui, &format!("stop {}", clock(s.started.elapsed().as_secs())), true, true).on_hover_text(format!("Recording to {}: {} rows, {}", s.dir.display(), s.rows, size_text(s.bytes))),
            };
            clicked = r.clicked();
            let arrow = theme::icon_button(ui, theme::Icon::Down, "Recording settings", egui::vec2(32.0, fields::HEIGHT));
            egui::Popup::from_toggle_button_response(&arrow).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
                ui.set_min_width(320.0);
                ui.label(theme::b("name for the next recording"));
                fields::line(ui, &mut label, "Recording name", |t| t.hint_text("optional, e.g. J2 dip").desired_width(300.0));
                ui.label(RichText::new(format!("in {}", dir.display())).weak());
                if let Some(s) = &status {
                    ui.separator();
                    ui.label(format!("recording: {} rows, {}", s.rows, size_text(s.bytes)));
                    if s.lost > 0 {
                        ui.colored_label(theme::pal(ui).red, format!("{} samples lost: the disk could not keep up. The recording says so in its recording.json.", s.lost));
                    }
                    if let Some(w) = &s.warning {
                        ui.colored_label(theme::pal(ui).hold, w);
                    }
                }
            });
        });
        self.rec_label = label;
        if clicked {
            self.toggle_recording();
        }
        if let Some(s) = &status
            && (s.lost > 0 || s.warning.is_some())
        {
            let p = theme::pal(ui);
            let tip = s.warning.clone().unwrap_or_else(|| format!("{} samples lost: the disk could not keep up.", s.lost));
            ui.add_space(4.0);
            ui.label(theme::b(if s.lost > 0 { format!("{} lost", s.lost) } else { "warning".into() }).color(if s.lost > 0 { p.red } else { p.hold })).on_hover_text(tip);
        }

        ui.add_space(8.0);
        let busy = self.snapshot_job.is_some();
        let secs = self.settings.snapshot_s;
        let mut save = false;
        let mut chosen = None;
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let r = ui
                .add_enabled(have && !busy, egui::Button::new(format!("save last {}", seconds_text(secs))).min_size(egui::vec2(0.0, fields::HEIGHT)))
                .on_hover_text("Save the last seconds of every channel from the live history, e.g. right after a trip")
                .on_disabled_hover_text(if busy { "Saving..." } else { why_not });
            save = r.clicked();
            let arrow = theme::icon_button(ui, theme::Icon::Down, "Save last: how many seconds", egui::vec2(32.0, fields::HEIGHT));
            egui::Popup::menu(&arrow).show(|ui| {
                ui.label(theme::b("save the last"));
                for (v, t) in SAVE_LAST {
                    if ui.selectable_label((v - secs).abs() < 1e-9, t).clicked() {
                        chosen = Some(v);
                        ui.close();
                    }
                }
            });
        });
        if let Some(v) = chosen {
            self.settings.snapshot_s = v;
            self.mark_settings_dirty();
        }
        if save {
            self.save_last();
        }

        ui.add_space(8.0);
        let mut toggle = false;
        let mut interval = None;
        let current = self.settings.slow_interval_ms;
        let slow = self.slow.as_ref().map(|r| r.status());
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let r = match &slow {
                None => ui
                    .add_enabled(have, egui::Button::new("slow log").min_size(egui::vec2(0.0, fields::HEIGHT)))
                    .on_hover_text("Log count, mean, min and max per interval instead of every sample: for runs of hours")
                    .on_disabled_hover_text(why_not),
                Some(s) => red_with_mark(ui, &format!("stop slow log {}", clock(s.started.elapsed().as_secs())), true, true).on_hover_text(format!("{}: {} rows", s.dir.display(), s.rows)),
            };
            toggle = r.clicked();
            let arrow = theme::icon_button(ui, theme::Icon::Down, "Slow log interval", egui::vec2(32.0, fields::HEIGHT));
            egui::Popup::menu(&arrow).show(|ui| {
                ui.label(theme::b("one line every"));
                for (v, t) in SLOW_INTERVALS {
                    if ui.selectable_label(v == current, t).clicked() {
                        interval = Some(v);
                        ui.close();
                    }
                }
                if let Some(w) = slow.as_ref().and_then(|s| s.warning.clone()) {
                    ui.colored_label(theme::pal(ui).hold, w);
                }
            });
        });
        if let Some(v) = interval {
            self.settings.slow_interval_ms = v;
            self.mark_settings_dirty();
        }
        if toggle {
            self.toggle_slow_log();
        }
    }

    pub fn toggle_recording(&mut self) {
        match self.recorder.take() {
            None => match Recorder::start(&self.session, &self.record_dir(), &self.rec_label, None, &self.infos()) {
                Ok(r) => {
                    let s = r.status();
                    self.log.info(format!("Recording to {}", s.dir.display()));
                    self.last_folder = Some(s.dir);
                    r.derived(self.derived.iter().map(|d| d.live.def().clone()).collect(), None);
                    self.recorder = Some(r);
                }
                Err(e) => self.toast(Level::Error, format!("Could not start recording: {e}")),
            },
            Some(r) => {
                self.last_folder = Some(r.status().dir);
                self.stopping.push(("stopped", r.stop_in_background()));
            }
        }
    }

    pub fn save_last(&mut self) {
        if self.snapshot_job.is_some() || self.chans.is_empty() {
            return;
        }
        let store = self.session.store().clone();
        let st = self.session.status().clone();
        let keys: Vec<_> = self.chans.iter().map(|c| c.key.clone()).collect();
        let infos = self.infos();
        let dir = self.record_dir();
        let secs = self.settings.snapshot_s;
        let label = if self.rec_label.trim().is_empty() { format!("last {secs:.0} s") } else { self.rec_label.clone() };
        let controller = st.target.as_ref().map(|t| t.to_string()).unwrap_or_default();
        let system_id = st.announce.as_ref().and_then(|a| a.system_id.clone());
        let derived: Vec<_> = self.derived.iter().map(|d| d.live.def().clone()).collect();
        let to = now_ms();
        self.snapshot_span = Some((to - (secs * 1000.0) as i64, to));
        let ctx = self.ctx.clone();
        self.snapshot_job = Some(std::thread::spawn(move || {
            let r = recording::write_snapshot(&dir, &label, &store, &st.timeline, &keys, &infos, secs, &controller, system_id, &derived);
            ctx.request_repaint();
            r
        }));
    }

    pub fn toggle_slow_log(&mut self) {
        match self.slow.take() {
            None => {
                let label = if self.rec_label.trim().is_empty() { "slow".to_string() } else { format!("{} slow", self.rec_label.trim()) };
                match Recorder::start(&self.session, &self.record_dir(), &label, Some(self.settings.slow_interval_ms), &self.infos()) {
                    Ok(r) => {
                        self.last_folder = Some(r.status().dir);
                        r.derived(self.derived.iter().map(|d| d.live.def().clone()).collect(), None);
                        self.slow = Some(r);
                    }
                    Err(e) => self.toast(Level::Error, format!("Could not start the slow log: {e}")),
                }
            }
            Some(r) => self.stopping.push(("stopped", r.stop_in_background())),
        }
    }

    pub fn phone_switch(&mut self, ui: &mut egui::Ui) {
        let mut on = self.phone.is_some();
        if ui.checkbox(&mut on, "phone view").on_hover_text("A read-only page for a phone on the same network. Off until switched on; it opens a listening port on this PC while it is on.").changed() {
            if on {
                match PhoneServer::start(self.settings.phone_port, Arc::clone(&self.phone_snapshot)) {
                    Ok(s) => {
                        let moved = if self.settings.phone_port != 0 && s.port() != self.settings.phone_port { format!(" (port {} was in use by another program)", self.settings.phone_port) } else { String::new() };
                        self.toast(Level::Info, format!("Phone view on: open {} on a phone on the same network{moved}. (Windows may ask to allow it through the firewall.)", if s.urls.is_empty() { format!("port {}, {}", s.port(), s.path()) } else { s.urls.join(" or ") }));
                        self.phone = Some(s);
                    }
                    Err(e) => self.toast(Level::Error, format!("Phone view: {e}")),
                }
            } else {
                self.phone = None;
                self.log.info("Phone view off.");
            }
        }
        if let Some(p) = &self.phone {
            for u in &p.urls {
                ui.label(RichText::new(u).monospace());
            }
        }
    }

    pub fn check_recorders(&mut self) {
        let mut said = Vec::new();
        for slot in [&mut self.recorder, &mut self.slow] {
            let Some(r) = slot else { continue };
            let st = r.status();
            said.push(match st.state {
                RecState::Failed(e) => (Level::Error, format!("Recording stopped: {e} ({})", st.dir.display())),
                RecState::Ended(why) => (Level::Warn, format!("Recording closed: {why} ({})", st.dir.display())),
                _ => continue,
            });
            *slot = None;
        }
        for (level, text) in said {
            self.toast(level, text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recordings_folder_must_be_a_whole_path_that_can_be_written() {
        let dir = spy_core::testdir::TestDir::new("rec-folder");
        assert!(usable_folder(&dir.join("new").join("deeper")).is_ok(), "made when it is not there yet");
        assert!(dir.join("new").join("deeper").is_dir());
        std::fs::write(dir.join("a file"), b"x").unwrap();
        assert!(usable_folder(&dir.join("a file")).is_err());
        assert!(usable_folder(std::path::Path::new("relative\\folder")).unwrap_err().contains("whole path"));
        assert!(usable_folder(std::path::Path::new("")).is_err());
        assert_eq!(std::fs::read_dir(dir.join("new").join("deeper")).unwrap().count(), 0, "the check left a file behind");
    }

    #[test]
    fn a_running_time_reads_as_minutes_until_an_hour() {
        assert_eq!(clock(0), "00:00");
        assert_eq!(clock(192), "03:12");
        assert_eq!(clock(3599), "59:59");
        assert_eq!(clock(3600), "1:00:00");
        assert_eq!(clock(36_061), "10:01:01");
        assert_eq!(seconds_text(30.0), "30 s");
        assert_eq!(seconds_text(120.0), "2 min");
        assert_eq!(seconds_text(45.0), "45 s", "a hand-edited setting is still said");
    }
}
