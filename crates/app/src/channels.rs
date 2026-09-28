//! The channel table (right panel): one card per channel with its value (a 150 ms
//! mean, F3; the charts stay raw), min / max / mean since reset, the achieved rate,
//! the age of the last sample, and one status word. A value that is not LIVE is
//! dimmed and labelled, never shown as current (rule 2).

use std::time::Instant;

use eframe::egui::{self, Color32, RichText};

use spy_core::catalogue::flag;
use spy_core::log::Level;
use spy_core::session::ChannelState;

use crate::app::{SpyApp, Stats};
use crate::theme;
use crate::view::{self, Health};

pub fn health_color(h: Health) -> Color32 {
    match h {
        Health::Live => theme::OK,
        Health::Stale | Health::NoReply | Health::Waiting | Health::NoEventYet => theme::WARN,
        Health::Refused | Health::NotOnVc => theme::BAD,
        Health::NotConnected => theme::IDLE,
    }
}

fn age_text(at: Option<Instant>) -> String {
    match at {
        None => "never".into(),
        Some(t) => {
            let ms = t.elapsed().as_millis();
            if ms < 1000 { format!("{ms} ms") } else if ms < 120_000 { format!("{:.1} s", ms as f64 / 1000.0) } else { format!("{} min", ms / 60_000) }
        }
    }
}

impl SpyApp {
    /// Fold the samples that arrived since the last frame into each channel's
    /// since-reset statistics.
    pub fn update_stats(&mut self) {
        // The session started the history afresh (a different controller, at another
        // address or behind the same one): statistics, markers and cursors on the
        // old clock mean nothing now, and the old `upto` would hide the new samples.
        let epoch = self.session.store().epoch();
        if epoch != self.store_epoch {
            self.store_epoch = epoch;
            for c in &mut self.chans {
                c.stats = Stats::default();
            }
            for d in &mut self.derived {
                d.stats = Stats::default();
            }
            self.markers.clear();
            self.controller_events.clear();
            self.cursor_a = None;
            self.cursor_b = None;
            self.paused_at = None;
        }
        for c in &mut self.chans {
            let Some(ch) = self.session.store().get(&c.key) else { continue };
            let reading = view::reading(self.catalogue.get(c.key.signal));
            let r = ch.lock();
            let from = if c.stats.upto == i64::MIN { i64::MIN } else { c.stats.upto + 1 };
            let mut last = c.stats.upto;
            let hold = c.stats.hold.get_or_insert_with(view::ZeroHold::new);
            let mut hold = *hold;
            for (t, v) in r.range(from, i64::MAX) {
                last = t;
                let v = if reading == view::Reading::ZeroFilled { hold.apply_at(t, v) } else { v };
                if v.is_finite() {
                    c.stats.n += 1;
                    c.stats.sum += v;
                    c.stats.min = c.stats.min.min(v);
                    c.stats.max = c.stats.max.max(v);
                    c.stats.sum_sin += v.sin();
                    c.stats.sum_cos += v.cos();
                }
            }
            c.stats.hold = Some(hold);
            c.stats.upto = last;
        }
    }

    pub fn channel_table(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Channels").strong());
            ui.label(RichText::new(format!("{} of {}", self.chans.len(), spy_core::session::MAX_CHANNELS)).weak());
            if ui.small_button("Reset min/max").on_hover_text("Start min, max and mean afresh for every channel").clicked() {
                for c in &mut self.chans {
                    let upto = c.stats.upto;
                    c.stats = Stats { upto, ..Stats::default() };
                }
                for d in &mut self.derived {
                    let upto = d.stats.upto;
                    d.stats = Stats { upto, ..Stats::default() };
                }
            }
            if !self.chans.is_empty() && ui.small_button("Remove all").clicked() {
                self.chans.clear();
                self.sync_channels();
            }
        });
        if self.chans.is_empty() {
            ui.add_space(20.0);
            ui.label(RichText::new("No channels yet.\n\nPick a signal on the left and press 'Add as a channel...'. For example the DC-link voltage (5027) on a real controller, or the joint angles (6000-6005) on a virtual one.").weak());
            return;
        }
        let st = self.session.status().clone();
        let connected = view::session_live(&st.phase);
        let mut remove = None;
        let mut changed = false;
        let mut derive: Option<spy_core::derived::Derived> = None;
        let lanes: Vec<(u32, String)> = {
            let mut v: Vec<(u32, String)> = Vec::new();
            for c in &self.chans {
                if !v.iter().any(|(l, _)| *l == c.lane) {
                    v.push((c.lane, view::label(&self.catalogue, &c.key)));
                }
            }
            v
        };
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for i in 0..self.chans.len() {
                let key = self.chans[i].key.clone();
                let sig = self.catalogue.get(key.signal).cloned();
                let cs = st.channels.iter().find(|c| c.key == key).cloned();
                let h = view::health(cs.as_ref(), connected, sig.as_ref(), st.loopback);
                let d = view::display(sig.as_ref(), self.chans[i].radians);
                let reading = view::reading(sig.as_ref());
                let (value, is_text) = match self.session.store().get(&key) {
                    Some(ch) => {
                        let r = ch.lock();
                        if r.kind == Some(spy_core::sample::ValueKind::String) {
                            (r.last_text.clone(), true)
                        } else {
                            (view::readout(&r, reading).map(|v| view::fmt(v * d.factor)), false)
                        }
                    }
                    None => (None, false),
                };
                let color = self.chans[i].color;
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 14.0), egui::Sense::hover());
                        ui.painter().rect_filled(rect, 2.0, color);
                        let name = view::label(&self.catalogue, &key);
                        ui.label(RichText::new(&name).strong()).on_hover_text(format!(
                            "{}\nsignal {}, {} axis {}{}",
                            sig.as_ref().map(|s| s.description.as_str()).unwrap_or(""),
                            key.signal,
                            key.unit,
                            key.axis.one_based(),
                            match cs.as_ref().map(|c| &c.state) {
                                Some(ChannelState::Defined { stream }) => format!(", stream {stream}"),
                                _ => String::new(),
                            }
                        ));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.menu_button("⋯", |ui| {
                                if ui.button("Remove").clicked() {
                                    remove = Some(i);
                                    ui.close();
                                }
                                if ui.button("Reset min/max/mean").clicked() {
                                    let upto = self.chans[i].stats.upto;
                                    self.chans[i].stats = Stats { upto, ..Stats::default() };
                                    ui.close();
                                }
                                if sig.as_ref().is_some_and(|s| s.is_angle()) && ui.checkbox(&mut self.chans[i].radians, "Show in radians").changed() {
                                    changed = true;
                                }
                                if sig.as_ref().is_some_and(|s| s.has(flag::ZERO_FILLED)) && ui.checkbox(&mut self.chans[i].hold_nonzero, "Chart: hold the last non-zero value").on_hover_text("This signal pads between its values with exact zeros; holding makes the chart readable. The recording keeps the zeros.").changed() {
                                    changed = true;
                                }
                                let offers = crate::derived_view::offers(sig.as_ref(), &key);
                                if !offers.is_empty() {
                                    ui.separator();
                                    ui.label("Derived");
                                    for d in offers {
                                        if ui.button(crate::derived_view::offer_text(&d)).on_hover_text(self.derived_formula(&d)).clicked() {
                                            derive = Some(d);
                                            ui.close();
                                        }
                                    }
                                }
                                ui.separator();
                                ui.label("Chart");
                                let own = lanes.iter().filter(|(l, _)| *l == self.chans[i].lane).count() == 1 && self.chans.iter().filter(|c| c.lane == self.chans[i].lane).count() == 1;
                                if !own && ui.button("Own chart").clicked() {
                                    self.chans[i].lane = self.next_lane;
                                    self.next_lane += 1;
                                    changed = true;
                                    ui.close();
                                }
                                for (l, first) in &lanes {
                                    if *l == self.chans[i].lane {
                                        continue;
                                    }
                                    // Overlay only where the units agree: a shared axis in
                                    // two different units would mislead.
                                    let first_key = self.chans.iter().find(|c| c.lane == *l).map(|c| c.key.clone());
                                    let other_units = first_key.as_ref().map(|k| view::display(self.catalogue.get(k.signal), self.chans.iter().find(|c| &c.key == k).is_some_and(|c| c.radians)).units);
                                    if other_units.as_deref() == Some(d.units.as_str()) && ui.button(format!("Overlay on: {first}")).clicked() {
                                        self.chans[i].lane = *l;
                                        changed = true;
                                        ui.close();
                                    }
                                }
                            });
                            let w = h.word();
                            ui.label(RichText::new(w).small().strong().color(health_color(h))).on_hover_text(match h {
                                Health::Stale => "No sample arrived within its stale bound. The value shown is the last one received, NOT current.",
                                Health::NotOnVc => "A physical signal: a virtual controller does not have it.",
                                Health::Refused => "The controller refused this definition.",
                                Health::NoReply => "The controller has not answered the definition yet.",
                                Health::NoEventYet => "A text event: it sends only when its value changes.",
                                _ => "",
                            });
                        });
                    });
                    // The value line.
                    ui.horizontal(|ui| {
                        let v = value.clone().unwrap_or_else(|| "--".into());
                        let mut text = RichText::new(v).monospace().size(20.0);
                        if !h.is_live() {
                            text = text.weak();
                        }
                        let how = match reading {
                            view::Reading::Plain => "The mean of the last 150 ms. The charts are raw.",
                            view::Reading::ZeroFilled => "The mean of the last 150 ms of the values this signal reports. It pads between them with exact zeros, which are left out; a run of zeros longer than 0.1 s is a real zero (the joint at rest). The recording keeps every sample as sent.",
                            view::Reading::Wrapping => "The mean of the last 150 ms taken on the circle (an angle within one turn), or the newest sample while it turns too fast to average.",
                            view::Reading::Turn => "The newest sample.",
                        };
                        ui.label(text).on_hover_text(how);
                        if !is_text {
                            if sig.as_ref().is_some_and(|s| s.is_angle()) {
                                if ui.small_button(&d.units).on_hover_text("Click to switch degrees / radians").clicked() {
                                    self.chans[i].radians = !self.chans[i].radians;
                                    changed = true;
                                }
                            } else {
                                ui.label(RichText::new(&d.units).weak());
                            }
                        }
                    });
                    if let Some(ChannelState::Refused { text, .. }) = cs.as_ref().map(|c| &c.state) {
                        ui.label(RichText::new(text).small().color(theme::BAD));
                    }
                    if !is_text {
                        let s = self.chans[i].stats;
                        ui.horizontal_wrapped(|ui| {
                            let f = |x: f64| if x.is_finite() { view::fmt(x * d.factor) } else { "--".into() };
                            let mean = s.mean(reading).unwrap_or(f64::NAN);
                            ui.label(RichText::new(format!("min {}  max {}  mean {}", f(s.min), f(s.max), f(mean))).small().monospace());
                        });
                    }
                    ui.horizontal(|ui| {
                        if let Some(c) = &cs {
                            ui.label(RichText::new(format!("{:.0}/s", c.rate)).small().weak());
                            ui.label(RichText::new(format!("age {}", age_text(c.last_arrival))).small().weak());
                            if let Some(ms) = c.sample_ms {
                                ui.label(RichText::new(format!("{ms} ms")).small().weak());
                            }
                            if c.gaps > 0 {
                                ui.label(RichText::new(format!("{} gaps", c.gaps)).small().color(theme::WARN)).on_hover_text("Steps in the controller's timestamps longer than one sample: samples the controller did not send.");
                            }
                        }
                    });
                });
            }
            self.derived_cards(ui, &st);
        });
        if let Some(d) = derive {
            self.add_derived(d);
        }
        if let Some(i) = remove {
            let k = self.chans.remove(i).key;
            self.log.info(format!("Removed {k}."));
            self.sync_channels();
        }
        if changed {
            self.mark_settings_dirty();
        }
        let _ = Level::Info;
    }
}
