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

pub fn old_label(ui: &mut egui::Ui, h: Health, age: Option<std::time::Duration>) {
    let p = theme::pal(ui);
    let text = age.map_or_else(|| "old".to_string(), |a| format!("{} old", view::age_words(a)));
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.label(theme::b(text).color(p.hold)).on_hover_text(view::old_note(h, true, age).unwrap_or_default());
    });
}

pub fn count_text(streams: usize, derived: usize) -> String {
    let base = format!("{streams} of {}", spy_core::session::MAX_CHANNELS);
    if derived == 0 { base } else { format!("{base} · {derived} derived") }
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
            let count = count_text(self.chans.len(), self.derived.len());
            ui.add(egui::Label::new(RichText::new(&count).color(p.ink2)).truncate()).on_hover_text(&count);
            let hint = RichText::new("click for options").size(14.0).color(p.ink3);
            let hint_w = egui::WidgetText::from(hint.clone()).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Body).size().x;
            if !self.chans.is_empty() && ui.available_width() >= hint_w + 12.0 {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(hint);
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
        let pair = ["reset all min/max", "remove all"];
        let rows = if theme::pair_fits(ui, pair) { 1.0 } else { 2.0 };
        let bottom = 6.0 + rows * (theme::SMALL_H + ui.spacing().item_spacing.y) + 4.0;
        egui::ScrollArea::vertical().auto_shrink([false, false]).max_height((ui.available_height() - bottom).max(theme::LIST_LEAST_H)).show(ui, |ui| {
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
        let (mut reset, mut remove_all) = (false, false);
        theme::pair(ui, pair, theme::SMALL_H, |ui, k, size| {
            if k == 0 {
                reset = ui.add(egui::Button::new(pair[0]).min_size(size)).on_hover_text("Start min, max and mean afresh for every channel").clicked();
            } else {
                remove_all = ui.add(egui::Button::new(pair[1]).min_size(size)).clicked();
            }
        });
        if reset {
            self.reset_stats();
        }
        if remove_all {
            self.confirm_remove_all = true;
        }
        if open.is_some() {
            self.options_for = open;
        }
    }

    pub fn remove_all_dialog(&mut self, ctx: &egui::Context) {
        if !self.confirm_remove_all {
            return;
        }
        let (n, d) = (self.chans.len(), self.derived.len());
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::new("remove-all")).show(ctx, |ui| {
            ui.set_max_width(480.0);
            ui.heading(format!("Remove all {n} channels?"));
            let derived = if d == 0 { String::new() } else { format!(" and {d} derived value{}", if d == 1 { "" } else { "s" }) };
            ui.label(format!("Every channel{derived} goes, with its options (smoothing, scale, decimals) and the charts' layout. A plateau or target set on a derived value goes too."));
            if self.recorder.is_some() || self.slow.is_some() {
                ui.label(RichText::new("The recording carries on, with no channels to record.").color(theme::pal(ui).hold));
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if theme::red_button(ui, egui::Button::new("remove them all").min_size(egui::vec2(0.0, fields::HEIGHT))).clicked() {
                    self.chans.clear();
                    self.options_for = None;
                    self.sync_channels();
                    close = true;
                }
                if ui.add(egui::Button::new("cancel").min_size(egui::vec2(0.0, fields::HEIGHT))).clicked() {
                    close = true;
                }
            });
        });
        if close || modal.should_close() {
            self.confirm_remove_all = false;
        }
    }

    fn channel_row(&mut self, ui: &mut egui::Ui, i: usize, st: &Status, cursor: Option<&CursorReading>) -> egui::Response {
        let p = theme::pal(ui);
        let connected = view::session_live(&st.phase);
        let key = self.chans[i].key.clone();
        let sig = self.catalogue.get(key.signal).cloned();
        let cs = st.channels.iter().find(|c| c.key == key).cloned();
        let h = view::health(cs.as_ref(), connected, sig.as_ref(), st.loopback);
        let d = view::display(sig.as_ref(), self.chans[i].radians).with_decimals(self.chans[i].decimals);
        let reading = view::reading(sig.as_ref());
        let smooth = self.chans[i].smooth_ms;
        let (value, is_text, last_t) = match self.session.store().get(&key) {
            Some(ch) => {
                let r = ch.lock();
                let last_t = r.last().map(|(t, _)| t);
                if r.kind == Some(spy_core::sample::ValueKind::String) {
                    (r.last_text.clone(), true, last_t)
                } else {
                    (view::readout_ms(&r, reading, view::readout_window(smooth)).map(|v| d.fmt(v)), false, last_t)
                }
            }
            None => (None, sig.as_ref().is_some_and(view::is_text), None),
        };
        let color = self.chans[i].color;
        let name = view::short_label(&self.catalogue, &key);
        let old = view::is_old(h, value.is_some());
        let age = cs.as_ref().and_then(|c| c.last_arrival).map(|t| t.elapsed()).or_else(|| view::age_of(last_t, &st.timeline));
        let s = self.chans[i].stats;
        let (r, _) = clickable_row(ui, &format!("Options for {name}"), old, |ui| {
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
            ui.horizontal_wrapped(|ui| {
                let v = value.clone().unwrap_or_else(|| "--".into());
                let mut text = if is_text { theme::num(v, 20.0) } else { theme::num(v, 28.0) };
                if old {
                    text = text.color(p.ink2).strikethrough();
                } else if !h.is_live() {
                    text = text.color(p.ink2);
                }
                ui.label(text).on_hover_text(readout_tip(reading, smooth));
                if !is_text {
                    ui.label(RichText::new(&d.units).size(18.0).color(p.ink2));
                }
                if old {
                    old_label(ui, h, age);
                } else if h == Health::Stale {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(theme::b("no sample yet").color(p.hold));
                    });
                }
            });
            if let Some(ChannelState::Refused { text, .. }) = cs.as_ref().map(|c| &c.state) {
                ui.label(RichText::new(text).size(14.0).color(p.red));
            }
            if !is_text {
                let f = |x: f64| if x.is_finite() { d.fmt(x) } else { "--".into() };
                match cursor {
                    Some(c) => cursor_lines(ui, c, p, d.decimals),
                    None if !old => {
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
        let d = view::display(sig.as_ref(), self.chans[i].radians).with_decimals(self.chans[i].decimals);
        let name = view::short_label(&self.catalogue, &key);
        let (back, remove) = options_header(ui, &format!("Remove {name} from the channels"));
        let pair = ["compare...", "reset min/max"];
        let rows = if theme::pair_fits(ui, pair) { 1.0 } else { 2.0 };
        let bottom = 6.0 + rows * (fields::HEIGHT + ui.spacing().item_spacing.y) + 4.0;
        egui::ScrollArea::vertical().auto_shrink([false, false]).max_height((ui.available_height() - bottom).max(theme::LIST_LEAST_H)).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.horizontal(|ui| {
                theme::square(ui, self.chans[i].color, 14.0);
                ui.add(egui::Label::new(RichText::new(&name).font(egui::FontId::new(20.0, theme::bold()))).wrap());
            });
            ui.horizontal_wrapped(|ui| {
                let value = self.session.store().get(&key).and_then(|ch| {
                    let r = ch.lock();
                    if r.kind == Some(spy_core::sample::ValueKind::String) { r.last_text.clone() } else { view::readout_ms(&r, view::reading(sig.as_ref()), view::readout_window(self.chans[i].smooth_ms)).map(|v| format!("{} {}", d.fmt(v), d.units)) }
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
        let can = self.charted(i, &st) && charted >= 2;
        let (mut compare, mut reset) = (false, false);
        theme::pair(ui, pair, fields::HEIGHT, |ui, k, size| {
            if k == 0 {
                compare = theme::lockable(ui, can, egui::Button::new("compare...").min_size(size))
                    .on_hover_text("How closely it follows each other charted channel in a straight line (r), over the stretch in view: how an unknown signal is matched against known ones")
                    .on_disabled_hover_text("Chart at least two channels to compare them.")
                    .clicked();
            } else {
                reset = ui.add(egui::Button::new("reset min/max").min_size(size)).clicked();
            }
        });
        if compare {
            self.open_compare(key.id());
        }
        if reset {
            let upto = self.chans[i].stats.upto;
            self.chans[i].stats = Stats { upto, ..Stats::default() };
        }
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
            egui::ComboBox::from_id_salt(("smoothing", &key)).selected_text(cur).width(w).icon(theme::combo_icon).truncate().show_ui(ui, |ui| {
                for (ms, t) in SMOOTHING {
                    if ui.selectable_value(&mut self.chans[i].smooth_ms, ms, t).changed() {
                        changed = true;
                    }
                }
            });
        });
        ui.label(RichText::new("also smooths the value (150 ms at least).").size(14.0).color(p.ink3)).on_hover_text("On the screen only: recordings and saved files keep every sample.");
        let auto = match view::auto_decimals(&units) {
            Some(n) => format!("auto ({})", view::decimals_text(n)),
            None => "auto (6 digits)".to_string(),
        };
        option_row(ui, label_w, "decimals", |ui, w| {
            let cur = self.chans[i].decimals.map_or_else(|| auto.clone(), |n| view::decimals_text(n.into()));
            egui::ComboBox::from_id_salt(("decimals", &key)).selected_text(cur).width(w).icon(theme::combo_icon).truncate().show_ui(ui, |ui| {
                if ui.selectable_value(&mut self.chans[i].decimals, None, auto.as_str()).changed() {
                    changed = true;
                }
                for n in 0..=view::MAX_DECIMALS {
                    if ui.selectable_value(&mut self.chans[i].decimals, Some(n), view::decimals_text(n.into())).changed() {
                        changed = true;
                    }
                }
            })
            .response
            .on_hover_text("How the numbers read on the screen. Recordings and saved files keep every digit.");
        });
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
            egui::ComboBox::from_id_salt(("chart", &key)).selected_text(current).width(w).icon(theme::combo_icon).truncate().show_ui(ui, |ui| {
                if ui.selectable_label(own, "its own chart").clicked() && !own {
                    self.chans[i].lane = self.next_lane;
                    self.next_lane += 1;
                    changed = true;
                }
                for (l, n) in &lanes {
                    if ui.selectable_label(*l == lane, format!("with {n}")).clicked() && *l != lane {
                        self.chans[i].lane = *l;
                        let cat = &self.catalogue;
                        if let Some(first) = self.chans.iter().find(|c| c.lane == *l && c.key != key && view::display(cat.get(c.key.signal), c.radians).units == units) {
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
                egui::ComboBox::from_id_salt(("units", &key)).selected_text(cur).width(w).icon(theme::combo_icon).truncate().show_ui(ui, |ui| {
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
            && theme::check(ui, &mut self.chans[i].hold_nonzero, "chart: hold the last non-zero value")
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
        let cat = &self.catalogue;
        let on_this_chart = |c: &crate::app::ChanView| c.lane == lane && view::display(cat.get(c.key.signal), c.radians).units == units;
        let first = self.chans.iter().position(on_this_chart).unwrap_or(i);
        let scale = self.chans[first].scale;
        let unit_floor = crate::charts::min_span_for(&units, sig.as_ref());
        let shared = self.chans.iter().filter(|c| on_this_chart(c)).count() > 1;
        ui.horizontal_wrapped(|ui| {
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
        let one_box = NUM_W;
        let two_boxes = 2.0 * NUM_W + TO_W + 2.0 * ui.spacing().item_spacing.x;
        scale_row(ui, matches!(scale, Scale::Fit { .. }), "fit, at least", "Fit what is in view, never tighter than this", one_box, |ui, chosen| {
            if num_box(ui, id.with("floor"), &mut floor, &format!("Smallest span in {units}"), matches!(scale, Scale::Fit { .. })) || chosen {
                set = Some(Scale::Fit { floor: if (floor - unit_floor).abs() <= unit_floor * 1e-9 { None } else { Some(floor.max(0.0)) } });
            }
        });
        scale_row(ui, matches!(scale, Scale::Fixed { .. }), "fixed", "Always from the first value to the second", two_boxes, |ui, chosen| {
            let on = matches!(scale, Scale::Fixed { .. });
            let a = num_box(ui, id.with("hi"), &mut hi, &format!("Top of the scale in {units}"), on);
            ui.add_sized([TO_W, fields::HEIGHT], egui::Label::new(RichText::new("to").color(p.ink2)));
            let b = num_box(ui, id.with("lo"), &mut lo, &format!("Bottom of the scale in {units}"), on);
            if a || b || chosen {
                set = Some(Scale::Fixed { lo, hi });
            }
        });
        scale_row(ui, matches!(scale, Scale::Centred { .. }), "around zero, ±", "From minus this to plus this", one_box, |ui, chosen| {
            if num_box(ui, id.with("half"), &mut half, &format!("Half the scale in {units}"), matches!(scale, Scale::Centred { .. })) || chosen {
                set = Some(Scale::Centred { half });
            }
        });
        if let Some(s) = set
            && s.is_valid()
            && s != scale
        {
            for c in self.chans.iter_mut().filter(|c| on_this_chart(c)) {
                c.scale = s;
            }
            self.lane_zoom.remove(&(lane, units.clone()));
            self.mark_settings_dirty();
        }
    }
}

pub(crate) fn options_header(ui: &mut egui::Ui, remove_tip: &str) -> (bool, bool) {
    let p = theme::pal(ui);
    let (mut back, mut remove) = (false, false);
    let gap = ui.spacing().item_spacing.x;
    let need = theme::heading_width(ui, "03") + theme::icon_text_button_width(ui, "all channels") + theme::button_width(ui, "remove") + 3.0 * gap;
    let number = if need <= ui.available_width() { "03" } else { "" };
    theme::section(ui, number, "", true, |ui| {
        if theme::icon_text_button(ui, theme::Icon::Left, "all channels", theme::SMALL_H, false).clicked() {
            back = true;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if theme::outline_button(ui, "remove", p.red, theme::SMALL_H).on_hover_text(remove_tip).clicked() {
                remove = true;
            }
        });
    });
    (back, remove)
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

pub fn cursor_lines(ui: &mut egui::Ui, c: &CursorReading, p: &theme::Pal, decimals: Option<usize>) {
    let f = |v: Option<f64>| v.map(|x| view::fmt_to(x, decimals)).unwrap_or_else(|| "--".into());
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
    stats_grid(ui, &c.between, if c.a.is_some() && c.b.is_some() { "between A and B" } else { "in view" }, p, decimals);
}

pub fn stats_grid(ui: &mut egui::Ui, s: &crate::charts::RangeStats, what: &str, p: &theme::Pal, decimals: Option<usize>) {
    let has = s.n > 0;
    let num = |v: f64| RichText::new(if has { view::fmt_to(v, decimals) } else { "--".into() }).monospace().size(14.0).color(p.ink2);
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
            let sp = ui.spacing_mut();
            sp.interact_size.y = fields::HEIGHT;
            sp.icon_width = theme::COMBO_ICON_W;
            sp.icon_spacing = theme::COMBO_ICON_GAP;
            add(ui, w);
        });
    });
}

const NUM_W: f32 = 64.0;
const TO_W: f32 = 26.0;

fn scale_row(ui: &mut egui::Ui, on: bool, text: &str, tip: &str, boxes_w: f32, add: impl FnOnce(&mut egui::Ui, bool)) {
    let beside = theme::chip_tall_width(ui, text) + ui.spacing().item_spacing.x + boxes_w <= ui.available_width();
    let mut chosen = false;
    let mut add = Some(add);
    ui.horizontal(|ui| {
        ui.set_min_height(theme::TOOL_H);
        chosen = theme::chip_tall(ui, on, text).on_hover_text(tip).clicked() && !on;
        if beside && let Some(add) = add.take() {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| add(ui, chosen));
        }
    });
    if let Some(add) = add {
        ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), fields::HEIGHT), egui::Layout::right_to_left(egui::Align::Center), |ui| add(ui, chosen));
    }
}

fn num_box(ui: &mut egui::Ui, id: egui::Id, v: &mut f64, name: &str, on: bool) -> bool {
    let mut text: String = ui.data_mut(|d| d.get_temp::<String>(id)).unwrap_or_else(|| view::fmt_short(*v));
    let r = fields::line(ui, &mut text, name, |t| t.desired_width(NUM_W).min_size(egui::vec2(0.0, fields::HEIGHT)).horizontal_align(egui::Align::Max).id(id).char_limit(16));
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


