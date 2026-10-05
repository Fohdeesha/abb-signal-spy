use std::time::Instant;

use eframe::egui::{self, Color32, RichText, Sense, Stroke};

use spy_core::catalogue::flag;
use spy_core::session::{ChannelState, Status};

use crate::app::{SpyApp, Stats};
use crate::charts::{CursorReading, Scale, SMOOTHING};
use crate::fields;
use crate::theme;
use crate::view::{self, Health};

pub fn health_color(h: Health, p: &theme::Pal) -> Color32 {
    match h {
        Health::Live => p.live,
        Health::Stale | Health::NoReply | Health::Waiting | Health::NoEventYet => p.hold,
        Health::Refused | Health::NotOnVc => p.red,
        Health::NotConnected => p.ink2,
    }
}

pub fn age_text(at: Option<Instant>) -> String {
    match at {
        None => "never".into(),
        Some(t) => {
            let ms = t.elapsed().as_millis();
            if ms < 1000 { format!("{ms} ms") } else if ms < 120_000 { format!("{:.1} s", ms as f64 / 1000.0) } else { format!("{} min", ms / 60_000) }
        }
    }
}

pub fn health_tip(h: Health) -> &'static str {
    match h {
        Health::Stale => "No sample arrived within its stale bound. The value shown is the last one received, NOT current.",
        Health::NotOnVc => "A physical signal: a virtual controller does not have it.",
        Health::Refused => "The controller refused this definition.",
        Health::NoReply => "The controller has not answered the definition yet.",
        Health::NoEventYet => "A text event: it sends only when its value changes.",
        Health::Waiting => "Being set up on the controller.",
        Health::NotConnected => "Not connected.",
        Health::Live => "Arriving now.",
    }
}

pub fn status_word(ui: &mut egui::Ui, h: Health) -> egui::Response {
    let p = theme::pal(ui);
    let c = health_color(h, p);
    let rtl = ui.layout().prefer_right_to_left();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let word = |ui: &mut egui::Ui| {
            ui.label(theme::b(h.word().to_lowercase()).size(14.0).color(c));
        };
        let mark = |ui: &mut egui::Ui| {
            if h.is_live() {
                theme::square(ui, c, 8.0);
            } else {
                theme::hollow(ui, c, 8.0);
            }
        };
        if rtl {
            word(ui);
            mark(ui);
        } else {
            mark(ui);
            word(ui);
        }
    })
    .response
    .on_hover_text(health_tip(h))
}

pub fn clickable_row<R>(ui: &mut egui::Ui, name: &str, stale: bool, add: impl FnOnce(&mut egui::Ui) -> R) -> (egui::Response, R) {
    let p = theme::pal(ui);
    let under = ui.painter().add(egui::Shape::Noop);
    let inner = ui.scope_builder(egui::UiBuilder::new().sense(Sense::click()), |ui| {
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(if stale { 8 } else { 4 }, 6))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                add(ui)
            })
            .inner
    });
    let r = inner.response;
    r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name));
    let rect = r.rect;
    if stale {
        ui.painter().set(under, egui::Shape::Rect(egui::epaint::RectShape::new(rect, 0.0, p.stale_face, Stroke::new(2.0, p.stale_edge), egui::StrokeKind::Inside)));
    } else {
        if r.hovered() {
            ui.painter().set(under, egui::Shape::rect_filled(rect, 0.0, p.face_hover));
        }
        ui.painter().hline(rect.x_range(), rect.bottom(), Stroke::new(1.0, p.line));
    }
    (r, inner.inner)
}

impl SpyApp {
    pub fn update_stats(&mut self) {
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

    pub fn reset_stats(&mut self) {
        for c in &mut self.chans {
            let upto = c.stats.upto;
            c.stats = Stats { upto, ..Stats::default() };
        }
        for d in &mut self.derived {
            let upto = d.stats.upto;
            d.stats = Stats { upto, ..Stats::default() };
        }
    }

    pub fn channel_table(&mut self, ui: &mut egui::Ui) {
        let p = theme::pal(ui);
        theme::section(ui, "03", "channels", true, |ui| {
            ui.label(RichText::new(format!("{} of {}", self.chans.len() + self.derived.len(), spy_core::session::MAX_CHANNELS)).color(p.ink2));
            if !self.chans.is_empty() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new("click for options").size(14.0).color(p.ink3));
                });
            }
        });
        if self.chans.is_empty() {
            ui.add_space(12.0);
            ui.label(RichText::new("No channels yet.").color(p.ink2));
            ui.add_space(6.0);
            ui.label(RichText::new("Pick a signal on the left and add it. For example the DC-link voltage (5027) on a real controller, or the joint angles (6000-6005) on a virtual one.").color(p.ink2));
            return;
        }
        let st = self.session.status().clone();
        let cursors = if self.cursors_on { Some(self.cursor_readings(&st)) } else { None };
        let mut open = None;
        let bottom = 46.0;
        egui::ScrollArea::vertical().auto_shrink([false, false]).max_height((ui.available_height() - bottom).max(80.0)).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            for i in 0..self.chans.len() {
                let id = self.chans[i].key.id();
                let reading = cursors.as_ref().map(|c| c.get(&id).cloned().unwrap_or_default());
                if self.channel_row(ui, i, &st, reading.as_ref()).clicked() {
                    open = Some(id);
                }
            }
            if let Some(id) = self.derived_rows(ui, &st, cursors.as_ref()) {
                open = Some(id);
            }
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.add(egui::Button::new("reset min/max").min_size(egui::vec2(0.0, theme::SMALL_H))).on_hover_text("Start min, max and mean afresh for every channel").clicked() {
                self.reset_stats();
            }
            if ui.add(egui::Button::new("remove all").min_size(egui::vec2(0.0, theme::SMALL_H))).clicked() {
                self.chans.clear();
                self.options_for = None;
                self.sync_channels();
            }
        });
        if open.is_some() {
            self.options_for = open;
        }
    }

    fn channel_row(&mut self, ui: &mut egui::Ui, i: usize, st: &Status, cursor: Option<&CursorReading>) -> egui::Response {
        let p = theme::pal(ui);
        let connected = view::session_live(&st.phase);
        let key = self.chans[i].key.clone();
        let sig = self.catalogue.get(key.signal).cloned();
        let cs = st.channels.iter().find(|c| c.key == key).cloned();
        let h = view::health(cs.as_ref(), connected, sig.as_ref(), st.loopback);
        let d = view::display(sig.as_ref(), self.chans[i].radians);
        let reading = view::reading(sig.as_ref());
        let smooth = self.chans[i].smooth_ms;
        let (value, is_text) = match self.session.store().get(&key) {
            Some(ch) => {
                let r = ch.lock();
                if r.kind == Some(spy_core::sample::ValueKind::String) {
                    (r.last_text.clone(), true)
                } else {
                    (view::readout_ms(&r, reading, view::readout_window(smooth)).map(|v| view::fmt(v * d.factor)), false)
                }
            }
            None => (None, sig.as_ref().is_some_and(view::is_text)),
        };
        let color = self.chans[i].color;
        let name = view::short_label(&self.catalogue, &key);
        let stale = matches!(h, Health::Stale);
        let s = self.chans[i].stats;
        let (r, _) = clickable_row(ui, &format!("Options for {name}"), stale, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 22.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 22.0), Sense::hover());
                theme::paint_icon(ui.painter(), rect, theme::Icon::Right, p.ink3);
                status_word(ui, h);
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                theme::square(ui, color, 12.0);
                ui.add(egui::Label::new(theme::b(&name)).truncate()).on_hover_text(format!(
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
                });
            });
            ui.horizontal(|ui| {
                let v = value.clone().unwrap_or_else(|| "--".into());
                let mut text = if is_text { theme::num(v, 20.0) } else { theme::num(v, 28.0) };
                if !h.is_live() {
                    text = text.color(p.ink2);
                    if stale {
                        text = text.strikethrough();
                    }
                }
                ui.label(text).on_hover_text(readout_tip(reading, smooth));
                if !is_text {
                    ui.label(RichText::new(&d.units).size(18.0).color(p.ink2));
                }
                if stale && let Some(c) = &cs {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(theme::b(format!("{} old", age_text(c.last_arrival))).color(p.hold));
                    });
                }
            });
            if let Some(ChannelState::Refused { text, .. }) = cs.as_ref().map(|c| &c.state) {
                ui.label(RichText::new(text).size(14.0).color(p.red));
            }
            if !is_text {
                let f = |x: f64| if x.is_finite() { view::fmt(x * d.factor) } else { "--".into() };
                match cursor {
                    Some(c) => cursor_lines(ui, c, p),
                    None if !stale => {
                        let mean = s.mean(reading).unwrap_or(f64::NAN);
                        let one = format!("min {}  max {}  mean {}", f(s.min), f(s.max), f(mean));
                        let fits = ui.fonts_mut(|fo| fo.layout_no_wrap(one.clone(), egui::FontId::monospace(14.0), p.ink2).size().x) <= ui.available_width();
                        let text = if fits { one } else { format!("min {}  max {}\nmean {}", f(s.min), f(s.max), f(mean)) };
                        ui.add(egui::Label::new(RichText::new(text).monospace().size(14.0).color(p.ink2)).wrap());
                        if let Some(c) = &cs
                            && c.gaps > 0
                        {
                            ui.label(theme::b(format!("{} gaps", c.gaps)).size(14.0).color(p.hold)).on_hover_text("Steps in the controller's timestamps longer than one sample: samples the controller did not send.");
                        }
                    }
                    None => {}
                }
            }
        });
        r
    }

    pub fn channel_options(&mut self, ui: &mut egui::Ui) {
        let Some(id) = self.options_for.clone() else { return };
        let Some(i) = self.chans.iter().position(|c| c.key.id() == id) else {
            if let Some(j) = self.derived.iter().position(|d| d.live.def().id() == id) {
                self.derived_options(ui, j);
            } else {
                self.options_for = None;
            }
            return;
        };
        let p = theme::pal(ui);
        let st = self.session.status().clone();
        let key = self.chans[i].key.clone();
        let sig = self.catalogue.get(key.signal).cloned();
        let cs = st.channels.iter().find(|c| c.key == key).cloned();
        let h = view::health(cs.as_ref(), view::session_live(&st.phase), sig.as_ref(), st.loopback);
        let d = view::display(sig.as_ref(), self.chans[i].radians);
        let name = view::short_label(&self.catalogue, &key);
        let mut back = false;
        let mut remove = false;
        theme::section(ui, "03", "", true, |ui| {
            if theme::icon_text_button(ui, theme::Icon::Left, "all channels", theme::SMALL_H, false).clicked() {
                back = true;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::outline_button(ui, "remove", p.red, theme::SMALL_H).on_hover_text(format!("Remove {name} from the channels")).clicked() {
                    remove = true;
                }
            });
        });
        egui::ScrollArea::vertical().auto_shrink([false, false]).max_height((ui.available_height() - 52.0).max(80.0)).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.horizontal(|ui| {
                theme::square(ui, self.chans[i].color, 14.0);
                ui.add(egui::Label::new(RichText::new(&name).font(egui::FontId::new(20.0, theme::heavy()))).wrap());
            });
            ui.horizontal_wrapped(|ui| {
                let value = self.session.store().get(&key).and_then(|ch| {
                    let r = ch.lock();
                    if r.kind == Some(spy_core::sample::ValueKind::String) { r.last_text.clone() } else { view::readout_ms(&r, view::reading(sig.as_ref()), view::readout_window(self.chans[i].smooth_ms)).map(|v| format!("{} {}", view::fmt(v * d.factor), d.units)) }
                });
                ui.label(theme::num(value.unwrap_or_else(|| "--".into()), 18.0).color(if h.is_live() { p.ink } else { p.ink2 }));
                status_word(ui, h);
                if let Some(c) = &cs {
                    let gaps = if c.gaps > 0 { format!(", {} gaps", c.gaps) } else { String::new() };
                    ui.label(RichText::new(format!("{:.0} /s{gaps}", c.rate)).size(15.0).color(p.ink2)).on_hover_text("Samples a second; gaps are steps in the controller's timestamps longer than one sample");
                }
            });
            rule(ui, p);
            self.display_options(ui, i);
            rule(ui, p);
            self.scale_options(ui, i);
            let offers = crate::derived_view::offers(sig.as_ref(), &key);
            if !offers.is_empty() {
                ui.add_space(4.0);
                rule(ui, p);
                ui.label(theme::b("derived values"));
                for o in offers {
                    if ui.button(crate::derived_view::offer_text(&o)).on_hover_text(self.derived_formula(&o)).clicked() {
                        self.add_derived(o);
                    }
                }
            }
        });
        let charted = (0..self.chans.len()).filter(|&j| self.charted(j, &st)).count();
        ui.add_space(6.0);
        ui.columns(2, |cols| {
            let can = self.charted(i, &st) && charted >= 2;
            if cols[0]
                .add_enabled(can, egui::Button::new("compare...").min_size(egui::vec2(cols[0].available_width(), fields::HEIGHT)))
                .on_hover_text("How closely it follows each other charted channel in a straight line (r), over the stretch in view: how an unknown signal is matched against known ones")
                .on_disabled_hover_text("Chart at least two channels to compare them.")
                .clicked()
            {
                self.open_compare(key.id());
            }
            if cols[1].add(egui::Button::new("reset min/max").min_size(egui::vec2(cols[1].available_width(), fields::HEIGHT))).clicked() {
                let upto = self.chans[i].stats.upto;
                self.chans[i].stats = Stats { upto, ..Stats::default() };
            }
        });
        if back {
            self.options_for = None;
        }
        if remove {
            let k = self.chans.remove(i).key;
            self.log.info(format!("Removed {k}."));
            self.options_for = None;
            self.sync_channels();
        }
    }

    fn display_options(&mut self, ui: &mut egui::Ui, i: usize) {
        let p = theme::pal(ui);
        let key = self.chans[i].key.clone();
        let sig = self.catalogue.get(key.signal).cloned();
        let units = view::display(sig.as_ref(), self.chans[i].radians).units;
        let mut changed = false;
        let label_w = 96.0;
        option_row(ui, label_w, "smoothing", |ui, w| {
            let cur = SMOOTHING.iter().find(|(ms, _)| *ms == self.chans[i].smooth_ms).map(|(_, t)| *t).unwrap_or("off");
            egui::ComboBox::from_id_salt(("smoothing", &key)).selected_text(cur).width(w).icon(theme::combo_icon).show_ui(ui, |ui| {
                for (ms, t) in SMOOTHING {
                    if ui.selectable_value(&mut self.chans[i].smooth_ms, ms, t).changed() {
                        changed = true;
                    }
                }
            });
        });
        ui.label(RichText::new("also smooths the value (150 ms at least).").size(14.0).color(p.ink3)).on_hover_text("On the screen only: recordings and saved files keep every sample.");
        let lane = self.chans[i].lane;
        let own = self.chans.iter().filter(|c| c.lane == lane).count() == 1;
        let mut lanes: Vec<(u32, String)> = Vec::new();
        for (j, c) in self.chans.iter().enumerate() {
            if j == i || lanes.iter().any(|(l, _)| *l == c.lane) {
                continue;
            }
            if view::display(self.catalogue.get(c.key.signal), c.radians).units == units {
                lanes.push((c.lane, view::short_label(&self.catalogue, &c.key)));
            }
        }
        option_row(ui, label_w, "chart", |ui, w| {
            let current = if own { "its own chart".to_string() } else { lanes.iter().find(|(l, _)| *l == lane).map(|(_, n)| format!("with {n}")).unwrap_or_else(|| "shared".into()) };
            egui::ComboBox::from_id_salt(("chart", &key)).selected_text(current).width(w).icon(theme::combo_icon).show_ui(ui, |ui| {
                if ui.selectable_label(own, "its own chart").clicked() && !own {
                    self.chans[i].lane = self.next_lane;
                    self.next_lane += 1;
                    changed = true;
                }
                for (l, n) in &lanes {
                    if ui.selectable_label(*l == lane, format!("with {n}")).clicked() && *l != lane {
                        self.chans[i].lane = *l;
                        if let Some(first) = self.chans.iter().find(|c| c.lane == *l && c.key != key) {
                            self.chans[i].scale = first.scale;
                        }
                        changed = true;
                    }
                }
            });
        });
        if sig.as_ref().is_some_and(|s| s.is_angle()) {
            option_row(ui, label_w, "units", |ui, w| {
                let cur = if self.chans[i].radians { "radians" } else { "degrees" };
                egui::ComboBox::from_id_salt(("units", &key)).selected_text(cur).width(w).icon(theme::combo_icon).show_ui(ui, |ui| {
                    for (rad, t) in [(false, "degrees"), (true, "radians")] {
                        if ui.selectable_value(&mut self.chans[i].radians, rad, t).changed() {
                            self.chans[i].scale = Scale::default();
                            changed = true;
                        }
                    }
                });
            });
        }
        if sig.as_ref().is_some_and(|s| s.has(flag::ZERO_FILLED))
            && ui
                .checkbox(&mut self.chans[i].hold_nonzero, "chart: hold the last non-zero value")
                .on_hover_text("This signal pads between its values with exact zeros; holding makes the chart readable. The recording keeps the zeros.")
                .changed()
        {
            changed = true;
        }
        if changed {
            self.mark_settings_dirty();
        }
    }

    fn scale_options(&mut self, ui: &mut egui::Ui, i: usize) {
        let p = theme::pal(ui);
        let key = self.chans[i].key.clone();
        let sig = self.catalogue.get(key.signal).cloned();
        let units = view::display(sig.as_ref(), self.chans[i].radians).units;
        let lane = self.chans[i].lane;
        let first = self.chans.iter().position(|c| c.lane == lane).unwrap_or(i);
        let scale = self.chans[first].scale;
        let unit_floor = crate::charts::min_span_for(&units, sig.as_ref());
        let shared = self.chans.iter().filter(|c| c.lane == lane).count() > 1;
        ui.horizontal(|ui| {
            ui.label(theme::b(if units.is_empty() { "vertical scale".to_string() } else { format!("vertical scale, in {units}") }));
            if shared {
                ui.label(RichText::new("(the chart's, for all on it)").size(14.0).color(p.ink3));
            }
        });
        let mut set: Option<Scale> = None;
        let id = egui::Id::new(("scale", key.id()));
        let (mut lo, mut hi) = match scale {
            Scale::Fixed { lo, hi } => (lo, hi),
            _ => self.lane_view_range(lane, &units).map_or((-1.0, 1.0), |(a, b)| (crate::charts::nice(a, false), crate::charts::nice(b, true))),
        };
        let mut half = match scale {
            Scale::Centred { half } => half,
            _ => crate::charts::nice(lo.abs().max(hi.abs()), true),
        };
        let mut floor = match scale {
            Scale::Fit { floor } => floor.unwrap_or(unit_floor),
            _ => unit_floor,
        };
        scale_row(ui, matches!(scale, Scale::Fit { .. }), "fit, at least", "Fit what is in view, never tighter than this", |ui, chosen| {
            if num_box(ui, id.with("floor"), &mut floor, &format!("Smallest span in {units}"), matches!(scale, Scale::Fit { .. })) || chosen {
                set = Some(Scale::Fit { floor: if (floor - unit_floor).abs() <= unit_floor * 1e-9 { None } else { Some(floor.max(0.0)) } });
            }
        });
        scale_row(ui, matches!(scale, Scale::Fixed { .. }), "fixed", "Always from the first value to the second", |ui, chosen| {
            let on = matches!(scale, Scale::Fixed { .. });
            let a = num_box(ui, id.with("hi"), &mut hi, &format!("Top of the scale in {units}"), on);
            ui.add_sized([26.0, fields::HEIGHT], egui::Label::new(RichText::new("to").color(p.ink2)));
            let b = num_box(ui, id.with("lo"), &mut lo, &format!("Bottom of the scale in {units}"), on);
            if a || b || chosen {
                set = Some(Scale::Fixed { lo, hi });
            }
        });
        scale_row(ui, matches!(scale, Scale::Centred { .. }), "around zero, ±", "From minus this to plus this", |ui, chosen| {
            if num_box(ui, id.with("half"), &mut half, &format!("Half the scale in {units}"), matches!(scale, Scale::Centred { .. })) || chosen {
                set = Some(Scale::Centred { half });
            }
        });
        if let Some(s) = set
            && s.is_valid()
            && s != scale
        {
            for c in self.chans.iter_mut().filter(|c| c.lane == lane) {
                c.scale = s;
            }
            self.lane_zoom.retain(|(l, _), _| *l != lane);
            self.mark_settings_dirty();
        }
    }
}

fn readout_tip(reading: view::Reading, smooth: u32) -> String {
    let span = view::readout_window(smooth);
    match reading {
        view::Reading::Plain => format!("The mean of the last {span} ms. The charts are raw unless smoothed."),
        view::Reading::ZeroFilled => format!("The mean of the last {span} ms of the values this signal reports. It pads between them with exact zeros, which are left out; a run of zeros longer than 0.1 s is a real zero (the joint at rest). The recording keeps every sample as sent."),
        view::Reading::Wrapping => format!("The mean of the last {span} ms taken on the circle (an angle within one turn), or the newest sample while it turns too fast to average."),
        view::Reading::Turn => "The newest sample.".into(),
    }
}

pub fn cursor_lines(ui: &mut egui::Ui, c: &CursorReading, p: &theme::Pal) {
    let f = |v: Option<f64>| v.map(view::fmt).unwrap_or_else(|| "--".into());
    let mono = |t: String| RichText::new(t).monospace().size(14.0);
    let word = |t: &str| theme::b(t).size(14.0);
    if c.a.is_none() && c.b.is_none() {
        ui.label(word("A: click a chart").color(p.hold));
    }
    if c.a.is_some() || c.b.is_some() {
        egui::Grid::new(ui.next_auto_id()).num_columns(4).spacing(egui::vec2(8.0, 1.0)).show(ui, |ui| {
            ui.label(word("A"));
            ui.label(mono(f(c.a)));
            if c.b.is_some() {
                ui.label(word("B"));
                ui.label(mono(f(c.b)));
                ui.end_row();
                ui.label(word("B − A"));
                ui.label(mono(f(c.a.zip(c.b).map(|(a, b)| b - a))));
            } else {
                ui.label(word("B").color(p.hold));
                ui.label(word("right-click a chart").color(p.hold));
            }
            ui.end_row();
        });
    }
    stats_grid(ui, &c.between, if c.a.is_some() && c.b.is_some() { "between A and B" } else { "in view" }, p);
}

pub fn stats_grid(ui: &mut egui::Ui, s: &crate::charts::RangeStats, what: &str, p: &theme::Pal) {
    let has = s.n > 0;
    let num = |v: f64| RichText::new(if has { view::fmt(v) } else { "--".into() }).monospace().size(14.0).color(p.ink2);
    let word = |t: &str| theme::b(t).size(14.0).color(p.ink2);
    ui.label(RichText::new(what).size(14.0).color(p.ink3));
    egui::Grid::new(ui.next_auto_id()).num_columns(4).spacing(egui::vec2(8.0, 1.0)).show(ui, |ui| {
        ui.label(word("mean"));
        ui.label(num(s.mean));
        ui.label(word("sd"));
        ui.label(num(s.sd));
        ui.end_row();
        ui.label(word("min"));
        ui.label(num(s.min));
        ui.label(word("max"));
        ui.label(num(s.max));
        ui.end_row();
    });
}

fn rule(ui: &mut egui::Ui, p: &theme::Pal) {
    let y = ui.cursor().top();
    ui.painter().hline(ui.max_rect().x_range(), y, Stroke::new(1.0, p.line));
    ui.add_space(6.0);
}

fn option_row(ui: &mut egui::Ui, label_w: f32, label: &str, add: impl FnOnce(&mut egui::Ui, f32)) {
    ui.horizontal(|ui| {
        ui.set_min_height(fields::HEIGHT);
        ui.allocate_ui_with_layout(egui::vec2(label_w, fields::HEIGHT), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.set_width(label_w);
            ui.label(theme::b(label));
        });
        let w = ui.available_width();
        ui.scope(|ui| {
            ui.spacing_mut().interact_size.y = fields::HEIGHT;
            add(ui, w);
        });
    });
}

fn scale_row(ui: &mut egui::Ui, on: bool, text: &str, tip: &str, add: impl FnOnce(&mut egui::Ui, bool)) {
    ui.horizontal(|ui| {
        ui.set_min_height(theme::TOOL_H);
        let chosen = ui.radio(on, text).on_hover_text(tip).clicked() && !on;
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| add(ui, chosen));
    });
}

fn num_box(ui: &mut egui::Ui, id: egui::Id, v: &mut f64, name: &str, on: bool) -> bool {
    let mut text: String = ui.data_mut(|d| d.get_temp::<String>(id)).unwrap_or_else(|| view::fmt_short(*v));
    let r = fields::line(ui, &mut text, name, |t| t.desired_width(64.0).min_size(egui::vec2(0.0, theme::TOOL_H)).horizontal_align(egui::Align::Max).id(id).char_limit(16));
    if !on && !r.has_focus() {
        let p = theme::pal(ui);
        ui.painter().rect_stroke(r.rect, 0.0, Stroke::new(1.0, p.field), egui::StrokeKind::Inside);
        theme::dashed_rect(ui.painter(), r.rect, Stroke::new(1.0, p.off_edge));
    }
    let mut taken = false;
    if r.changed()
        && let Ok(x) = text.trim().replace(',', ".").parse::<f64>()
        && x.is_finite()
    {
        *v = x;
        taken = true;
    }
    if r.has_focus() {
        ui.data_mut(|d| d.insert_temp(id, text));
    } else {
        ui.data_mut(|d| d.remove::<String>(id));
    }
    taken
}


