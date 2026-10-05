//! Recording controls in the top bar: REC (every sample), "save the last N
//! seconds" (what just happened, even if nothing was recording), slow logging
//! (averages for runs of hours), and the phone view switch.

use std::sync::Arc;

use eframe::egui::{self, RichText};

use spy_core::log::Level;
use spy_core::recording::{self, RecState, Recorder};

use crate::app::SpyApp;
use crate::fields;
use crate::phone::PhoneServer;
use crate::theme;

const SLOW_INTERVALS: [(u32, &str); 5] = [(500, "0.5 s"), (1000, "1 s"), (5000, "5 s"), (10_000, "10 s"), (60_000, "1 min")];

fn size_text(bytes: u64) -> String {
    if bytes < 1024 * 1024 { format!("{} KB", bytes / 1024) } else { format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0)) }
}

fn clock(secs: u64) -> String {
    format!("{:02}:{:02}:{:02}", secs / 3600, (secs / 60) % 60, secs % 60)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

impl SpyApp {
    pub fn record_controls(&mut self, ui: &mut egui::Ui) {
        let have = !self.chans.is_empty();
        // REC
        match &self.recorder {
            None => {
                fields::line(ui, &mut self.rec_label, "Recording name", |t| t.hint_text("recording name").desired_width(110.0));
                if ui.add_enabled(have, egui::Button::new(RichText::new("● REC").color(theme::REC).strong())).on_hover_text("Record every sample of every channel to a new folder").clicked() {
                    match Recorder::start(&self.session, &self.record_dir(), &self.rec_label, None, &self.infos()) {
                        Ok(r) => {
                            let s = r.status();
                            self.log.info(format!("Recording to {}", s.dir.display()));
                            self.last_folder = Some(s.dir);
                            r.derived(self.derived.iter().map(|d| d.live.def().clone()).collect(), None);
                            self.recorder = Some(r);
                        }
                        Err(e) => self.toast(Level::Error, format!("Could not start recording: {e}")),
                    }
                }
            }
            Some(r) => {
                let s = r.status();
                let text = format!("■ STOP  {}  {}  {} rows", clock(s.started.elapsed().as_secs()), size_text(s.bytes), s.rows);
                if ui.add(egui::Button::new(RichText::new(text).color(theme::REC).strong())).on_hover_text(s.dir.display().to_string()).clicked()
                    && let Some(r) = self.recorder.take() {
                        let s = r.stop();
                        match s.state {
                            RecState::Failed(e) => self.toast(Level::Error, format!("The recording ended with an error: {e}")),
                            _ => self.toast(Level::Info, format!("Recorded {} rows to {}", s.rows, s.dir.display())),
                        }
                        // Controller events of its last seconds are still on their way.
                        let to = now_ms();
                        self.rws_after_recording(s.dir.clone(), to - s.started.elapsed().as_millis() as i64, to);
                        self.last_folder = Some(s.dir);
                    }
                if s.lost > 0 {
                    ui.label(RichText::new(format!("{} lost", s.lost)).color(theme::BAD)).on_hover_text("The disk could not keep up: this recording has holes. It says so in its recording.json.");
                }
                if let Some(w) = &s.warning {
                    ui.label(RichText::new("⚠").color(theme::WARN)).on_hover_text(w);
                }
            }
        }
        // Save the last N seconds.
        ui.add(egui::DragValue::new(&mut self.settings.snapshot_s).range(1.0..=600.0).speed(1.0).suffix(" s"));
        let busy = self.snapshot_job.is_some();
        if ui.add_enabled(have && !busy, egui::Button::new("Save last")).on_hover_text("Save the last seconds of every channel from the live history, e.g. right after a trip").clicked() {
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
            self.mark_settings_dirty();
        }
        // Slow log.
        match &self.slow {
            None => {
                let cur = SLOW_INTERVALS.iter().find(|(v, _)| *v == self.settings.slow_interval_ms).map(|(_, l)| *l).unwrap_or("1 s");
                egui::ComboBox::from_id_salt("slow-interval").selected_text(cur).width(58.0).show_ui(ui, |ui| {
                    for (v, l) in SLOW_INTERVALS {
                        if ui.selectable_value(&mut self.settings.slow_interval_ms, v, l).changed() {
                            self.mark_settings_dirty();
                        }
                    }
                });
                if ui.add_enabled(have, egui::Button::new("Slow log")).on_hover_text("Log count, mean, min and max per interval instead of every sample: for runs of hours").clicked() {
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
            }
            Some(r) => {
                let s = r.status();
                if ui.add(egui::Button::new(RichText::new(format!("■ Slow log {}  {} rows", clock(s.started.elapsed().as_secs()), s.rows)).color(theme::WARN))).on_hover_text(s.dir.display().to_string()).clicked()
                    && let Some(r) = self.slow.take() {
                        let s = r.stop();
                        match s.state {
                            RecState::Failed(e) => self.toast(Level::Error, format!("The slow log ended with an error: {e}")),
                            _ => self.toast(Level::Info, format!("Slow log: {} rows in {}", s.rows, s.dir.display())),
                        }
                        let to = now_ms();
                        self.rws_after_recording(s.dir.clone(), to - s.started.elapsed().as_millis() as i64, to);
                    }
                if let Some(w) = &s.warning {
                    ui.label(RichText::new("⚠").color(theme::WARN)).on_hover_text(w);
                }
            }
        }
        if let Some(d) = self.last_folder.clone()
            && ui.small_button("📂").on_hover_text(format!("Open {}", d.display())).clicked() {
                crate::paths::open_folder(&d);
            }
        ui.separator();
        // Phone view: off until switched on.
        let mut on = self.phone.is_some();
        if ui.toggle_value(&mut on, "Phone view").on_hover_text("A read-only page for a phone on the same network. Off until switched on; it opens a listening port on this PC while it is on.").changed() {
            if on {
                match PhoneServer::start(self.settings.phone_port, Arc::clone(&self.phone_snapshot)) {
                    Ok(s) => {
                        let moved = if self.settings.phone_port != 0 && s.port() != self.settings.phone_port { format!(" (port {} was in use by another program)", self.settings.phone_port) } else { String::new() };
                        self.toast(Level::Info, format!("Phone view on: open {} on a phone on the same network{moved}. (Windows may ask to allow it through the firewall.)", if s.urls.is_empty() { format!("port {}", s.port()) } else { s.urls.join(" or ") }));
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
            ui.label(RichText::new(format!(":{}", p.port())).small()).on_hover_text(p.urls.join("\n"));
        }
    }

    /// A recorder that failed (disk full, folder gone), or closed itself (the
    /// controller behind the address changed), is reported and dropped.
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
