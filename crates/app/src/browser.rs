use eframe::egui::{self, RichText};

use spy_core::catalogue::{flag, Confidence, Select, Signal};
use spy_core::log::Level;
use spy_core::request::{Axis, MechUnit};
use spy_core::session::MAX_CHANNELS;
use spy_core::store::ChannelKey;

use crate::app::{chan_color, AddDialog, ChanView, SpyApp};
use crate::fields;
use crate::theme;
use crate::view;

pub fn confidence_color(c: Confidence, p: &theme::Pal) -> egui::Color32 {
    match c {
        Confidence::Confirmed => p.live,
        Confidence::Strong => egui::Color32::from_rgb(0x5A, 0x9B, 0xD5),
        Confidence::Probable => p.hold,
        Confidence::Open => egui::Color32::from_rgb(0xB0, 0x7A, 0xA1),
        Confidence::Inert => p.ink2,
    }
}

pub fn signal_badges(ui: &mut egui::Ui, s: &Signal) {
    if s.has(flag::FROZEN) {
        theme::badge(ui, "FROZEN", theme::pal(ui).hold, "Holds the last RAPID path-level value and does NOT move while EGM drives the robot. Do not use it to tell whether the robot moved.");
    }
    if s.has(flag::ZERO_FILLED) {
        theme::badge(ui, "0-FILL", theme::pal(ui).hold, "Carries a value every few samples and pads the rest with exact zeros. The chart can hold the last non-zero value (channel menu).");
    }
    if s.has(flag::SENTINEL) || s.has(flag::NAN) {
        theme::badge(ui, "PLACEHOLDER", theme::pal(ui).ink2, "Reads a placeholder (a huge value, -1 or NaN), not a measurement.");
    }
    if s.has(flag::EVENT) {
        theme::badge(ui, "TEXT EVENT", egui::Color32::from_rgb(0x76, 0xB7, 0xB2), "Sends text now and then (on change), not a stream of samples. Quiet is normal for it.");
    }
    if s.has(flag::PHYSICAL) {
        theme::badge(ui, "REAL ONLY", egui::Color32::from_rgb(0x9C, 0xA3, 0xAF), "A physical measurement (current, voltage, resolver, DC link): a virtual controller does not have it.");
    }
    if s.has(flag::VC_ONLY) {
        theme::badge(ui, "VC ONLY", egui::Color32::from_rgb(0x9C, 0xA3, 0xAF), "Only a virtual controller has it.");
    }
    if s.has(flag::WRAPPING) {
        theme::badge(ui, "0-360", egui::Color32::from_rgb(0x86, 0xBC, 0xB6), "An angle reduced to one turn: it jumps from 360 back to 0.");
    }
}

fn select_words(s: Select) -> &'static str {
    match s {
        Select::Axis => "Choose the robot and the axis. Without an axis it would silently read joint 1.",
        Select::Number => "The joint is in the signal number itself; choose only the robot.",
        Select::Robot => "One value per robot; choose the robot.",
        Select::Module => "One value per drive module; choose a robot and you get its module.",
        Select::Controller => "Controller-wide; nothing to choose.",
    }
}

impl SpyApp {
    pub(crate) fn visible(&self, s: &Signal) -> bool {
        if self.only_favourites && !self.settings.favourites.contains(&s.number) {
            return false;
        }
        let shown = (s.named && self.settings.show_named) || (self.settings.show_open && s.confidence == Confidence::Open) || (self.settings.show_inert && s.confidence == Confidence::Inert);
        if !shown && self.search.trim().chars().all(|c| c.is_ascii_digit()) && !self.search.trim().is_empty() {
            return s.number.to_string() == self.search.trim();
        }
        if !shown {
            return false;
        }
        if let Some(c) = &self.category
            && &s.category != c
        {
            return false;
        }
        if let Some(min) = self.min_confidence
            && s.confidence > min
        {
            return false;
        }
        true
    }

    pub fn browser(&mut self, ui: &mut egui::Ui) {
        let p = theme::pal(ui);
        self.signals_rect = Some(ui.max_rect());
        theme::section(ui, "01", "signals", false, |ui| {
            if ui.link(theme::b("add a set")).on_hover_text("Both DC links, one robot's torques, joint positions or resolver angles, or the 8000-8009 block, in one go").clicked() {
                self.open_sets();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::icon_button(ui, theme::Icon::FoldLeft, "Fold the signal list", egui::vec2(34.0, 30.0)).clicked() {
                    self.settings.signals_folded = true;
                    self.mark_settings_dirty();
                }
            });
        });
        let w = ui.available_width();
        fields::line(ui, &mut self.search, "Search the signals", |t| t.hint_text("search: number, name or unit").desired_width(w));
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if theme::small_chip(ui, self.settings.show_named, "identified").on_hover_text("The signals with a known quantity").clicked() {
                self.settings.show_named = !self.settings.show_named;
                self.mark_settings_dirty();
            }
            if theme::small_chip(ui, self.settings.show_open, "not yet").on_hover_text("Signals that respond but are not yet identified").clicked() {
                self.settings.show_open = !self.settings.show_open;
                self.mark_settings_dirty();
            }
            let filters = (self.settings.show_inert as usize) + (self.only_favourites as usize) + (self.category.is_some() as usize) + (self.min_confidence.is_some() as usize);
            let text = if filters > 0 { format!("filters ({filters})") } else { "filters".to_string() };
            let r = theme::small_drop_button(ui, &text);
            egui::Popup::from_toggle_button_response(&r).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
                ui.set_min_width(260.0);
                if ui.checkbox(&mut self.settings.show_inert, "inert ones too").on_hover_text("Signals that returned nothing on the measured cell (features it did not use)").changed() {
                    self.mark_settings_dirty();
                }
                ui.checkbox(&mut self.only_favourites, "favourites only");
                let mut cats: Vec<String> = self.catalogue.signals.iter().map(|s| s.category.clone()).filter(|c| !c.is_empty()).collect();
                cats.sort();
                cats.dedup();
                ui.label(theme::b("kind"));
                egui::ComboBox::from_id_salt("category").selected_text(self.category.clone().unwrap_or_else(|| "all kinds".into())).width(240.0).icon(theme::combo_icon).show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.category, None, "all kinds");
                    for c in cats {
                        ui.selectable_value(&mut self.category, Some(c.clone()), c);
                    }
                });
                ui.label(theme::b("confidence"));
                let conf_text = match self.min_confidence {
                    None => "any confidence",
                    Some(Confidence::Confirmed) => "confirmed only",
                    Some(Confidence::Strong) => "strong or better",
                    Some(Confidence::Probable) => "probable or better",
                    Some(_) => "any confidence",
                };
                egui::ComboBox::from_id_salt("confidence").selected_text(conf_text).width(240.0).icon(theme::combo_icon).show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.min_confidence, None, "any confidence");
                    ui.selectable_value(&mut self.min_confidence, Some(Confidence::Confirmed), "confirmed only");
                    ui.selectable_value(&mut self.min_confidence, Some(Confidence::Strong), "strong or better");
                    ui.selectable_value(&mut self.min_confidence, Some(Confidence::Probable), "probable or better");
                });
            });
        });

        let query = self.search.clone();
        let mut rows: Vec<(u32, usize)> = Vec::new();
        let mut seen_groups: std::collections::HashSet<String> = std::collections::HashSet::new();
        let groups = self.catalogue.groups();
        let matched: Vec<&Signal> = self.catalogue.search(&query).filter(|s| self.visible(s)).collect();
        for s in &matched {
            match &s.group {
                Some(g) if !query.trim().chars().all(|c| c.is_ascii_digit()) || query.trim().is_empty() => {
                    if seen_groups.insert(g.clone()) {
                        rows.push((s.number, groups.get(g).map(|v| v.len()).unwrap_or(1)));
                    }
                }
                _ => rows.push((s.number, 1)),
            }
        }
        if !query.trim().is_empty() {
            ui.label(RichText::new(format!("{} {} match", rows.len(), if rows.len() == 1 { "signal" } else { "signals" })).size(14.0).color(p.ink2));
        }
        if rows.is_empty() {
            ui.label(RichText::new("Nothing matches. Turn on 'not yet', or the inert ones in the filters, or look a number up below.").color(p.ink2));
        }
        let bottom = 50.0;
        let row_h = 38.0;
        let mut clicked = None;
        let mut add = None;
        egui::ScrollArea::vertical().id_salt("catalogue-list").max_height((ui.available_height() - bottom).max(0.0)).auto_shrink([false, false]).show_rows(ui, row_h, rows.len(), |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for &(n, count) in &rows[range] {
                let Some((name, units, frozen)) = self.catalogue.get(n).map(|s| (s.display_name(), crate::view::shown_units(&s.units).to_string(), s.has(flag::FROZEN))) else { continue };
                let noted = self.notes.get(n).is_some();
                let r = signal_row(ui, n, &name, &units, count, self.selected == Some(n), noted, frozen, row_h);
                if r.double_clicked() {
                    add = Some(n);
                } else if r.clicked() {
                    clicked = Some(n);
                }
            }
        });
        if let Some(n) = clicked {
            self.selected = if self.selected == Some(n) { None } else { Some(n) };
        }
        if let Some(n) = add {
            self.selected = Some(n);
            self.open_add(n);
        }

        ui.add_space(4.0);
        let y = ui.cursor().top();
        ui.painter().hline(ui.max_rect().x_range(), y, egui::Stroke::new(2.0, p.ink));
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let look = theme::b("look it up").size(14.0);
            let button_w = egui::WidgetText::from(look.clone()).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Button).size().x + 2.0 * ui.spacing().button_padding.x;
            let w = (ui.available_width() - button_w - 6.0 - 20.0).max(60.0);
            let r = fields::line(ui, &mut self.raw_number, "Raw signal number", |t| t.min_size(egui::vec2(0.0, theme::SMALL_H)).desired_width(w).font(egui::FontId::proportional(14.0)).hint_text("number not listed"));
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.add(egui::Button::new(look).min_size(egui::vec2(0.0, theme::SMALL_H))).clicked() || enter {
                match self.raw_number.trim().parse::<u32>() {
                    Ok(n) if n > 0 => self.selected = Some(n),
                    _ => self.toast(Level::Error, "Type a signal number (a whole number above 0)."),
                }
            }
        });
    }

    pub fn signals_strip(&mut self, ui: &mut egui::Ui) {
        let p = theme::pal(ui);
        ui.vertical_centered(|ui| {
            ui.spacing_mut().item_spacing.y = 10.0;
            ui.label(RichText::new("01").font(egui::FontId::new(18.0, theme::bold())).color(p.red));
            let unfold = theme::icon_button(ui, theme::Icon::FoldRight, "Unfold the signal list", egui::vec2(40.0, 40.0)).clicked();
            let add = theme::upright_button(ui, "add signals", egui::vec2(40.0, 110.0)).clicked();
            if unfold || add {
                self.settings.signals_folded = false;
                self.mark_settings_dirty();
            }
        });
    }

    pub fn signal_details(&mut self, ctx: &egui::Context) {
        let (Some(n), Some(list)) = (self.selected, self.signals_rect) else { return };
        let mut close = false;
        let height = (list.height() + 20.0).max(200.0);
        egui::Area::new(egui::Id::new("signal-details"))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(list.right() + 18.0, list.top() - 10.0))
            .show(ctx, |ui| {
                let p = theme::pal(ui);
                egui::Frame::new().fill(p.sheet).stroke(egui::Stroke::new(2.0, p.ink)).inner_margin(egui::Margin::same(14)).show(ui, |ui| {
                    ui.set_width(400.0);
                    ui.set_max_height(height - 28.0);
                    ui.horizontal(|ui| {
                        ui.label(theme::num(n.to_string(), 20.0));
                        let name = self.catalogue.get(n).map(|s| s.display_name()).unwrap_or_else(|| format!("Signal {n}"));
                        ui.add(egui::Label::new(RichText::new(name).font(egui::FontId::new(20.0, theme::bold()))).truncate());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if theme::icon_button(ui, theme::Icon::Close, "Close (Esc)", egui::vec2(40.0, 40.0)).clicked() {
                                close = true;
                            }
                            if self.catalogue.get(n).is_some() {
                                let fav = self.settings.favourites.contains(&n);
                                if theme::chip(ui, fav, "favourite").on_hover_text("Favourites can be shown alone (filters)").clicked() {
                                    if fav {
                                        self.settings.favourites.retain(|&x| x != n);
                                    } else {
                                        self.settings.favourites.push(n);
                                    }
                                    self.mark_settings_dirty();
                                }
                            }
                        });
                    });
                    egui::ScrollArea::vertical().id_salt("details").auto_shrink([false, true]).show(ui, |ui| self.details(ui, n));
                });
            });
        if close {
            self.selected = None;
        }
    }

    fn details(&mut self, ui: &mut egui::Ui, n: u32) {
        let p = theme::pal(ui);
        let Some(s) = self.catalogue.get(n).cloned() else {
            ui.label("Not in the catalogue. It can still be added; the controller will say whether it knows it.");
            ui.add_space(6.0);
            if theme::primary(ui, "add as a channel...", fields::HEIGHT).clicked() {
                self.open_add(n);
            }
            self.notes_section(ui, n);
            return;
        };
        ui.horizontal_wrapped(|ui| {
            theme::badge(ui, &s.confidence.label().to_uppercase(), confidence_color(s.confidence, p), "confirmed: named by ABB or pinned exactly · strong: reproduced on 12 readings · probable: fits, not forced · open: responds, not identified · inert: returned nothing on the measured cell");
            if !crate::view::shown_units(&s.units).is_empty() {
                theme::badge(ui, &s.units, p.ink2, "Its unit");
            }
            if let Some(ms) = s.sample_ms {
                theme::badge(ui, &format!("a sample every {} ms", view::fmt_short(ms)), p.ink2, "How often the controller sends it");
            }
            if let Some(t) = &s.value_type {
                theme::badge(ui, t, p.ink2, "Its record type");
            }
            signal_badges(ui, &s);
        });
        ui.label(RichText::new(select_words(s.select)).size(14.0).color(p.ink2));
        ui.horizontal_wrapped(|ui| {
            if theme::primary(ui, "add as a channel...", fields::HEIGHT).clicked() {
                self.open_add(n);
            }
            let (ids, others) = self.charted_ids_of(n);
            if let Some(id) = ids.first()
                && others > 0
                && ui
                    .add(egui::Button::new("compare with the charted channels").min_size(egui::vec2(0.0, fields::HEIGHT)))
                    .on_hover_text("How closely it follows each other charted channel in a straight line (r), over the stretch in view: chart known signals beside it (joint angles, speeds, torques) and see which it is a line of")
                    .clicked()
            {
                self.open_compare(id.clone());
            }
        });
        if !s.description.is_empty() {
            ui.add_space(4.0);
            ui.label(&s.description);
        }
        if let Some(a) = &s.abb {
            let src = match a.source.as_str() {
                "TuneMaster" => "TuneMaster's signal table",
                "TRM" => "the RAPID manual",
                "RobAPI" => "RobotStudio's RobAPI",
                "forum" => "ABB's forum",
                o => o,
            };
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("ABB's name:").color(p.ink2));
                ui.label(format!("{} (from {src})", a.name));
            });
        }
        if let Some(g) = &s.group {
            let members = self.catalogue.groups().get(g).cloned().unwrap_or_default();
            if members.len() > 1 {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(format!("the same quantity under {} numbers:", members.len())).color(p.ink2));
                    for m in members {
                        if m == n {
                            ui.label(theme::num(m.to_string(), 16.0));
                        } else if ui.link(theme::num(m.to_string(), 16.0)).clicked() {
                            self.selected = Some(m);
                        }
                    }
                });
            }
        }
        if !s.cell && s.vc {
            ui.colored_label(p.hold, "Seen only on a virtual controller.");
        }
        if s.confidence == Confidence::Inert {
            ui.colored_label(p.ink2, "Returned nothing on the measured cell. It may on a controller that uses the feature.");
        }
        for (title, text) in [("evidence", &s.evidence), ("ruled out", &s.ruled_out), ("open question", &s.open_question), ("next test", &s.next_test)] {
            if !text.is_empty() {
                egui::CollapsingHeader::new(theme::b(title)).id_salt((title, n)).show(ui, |ui| {
                    ui.label(text.as_str());
                });
            }
        }
        self.notes_section(ui, n);
    }
    pub fn open_add(&mut self, signal: u32) {
        let unit = self.settings.units.first().cloned().unwrap_or_else(|| "ROB_1".into());
        let axis = self.catalogue.get(signal).and_then(|s| s.joint).unwrap_or(1);
        self.add = Some(AddDialog { signal, unit, axis });
    }

    pub fn add_channels(&mut self, keys: Vec<ChannelKey>, overlay: bool) -> bool {
        let mut fresh: Vec<ChannelKey> = Vec::new();
        for k in &keys {
            if !self.chans.iter().any(|c| &c.key == k) && !fresh.contains(k) {
                fresh.push(k.clone());
            }
        }
        if fresh.is_empty() {
            self.toast(Level::Warn, "That channel is already there.");
            return false;
        }
        if self.chans.len() + fresh.len() > MAX_CHANNELS {
            self.toast(Level::Error, format!("At most {MAX_CHANNELS} channels at once: {} free.", MAX_CHANNELS - self.chans.len()));
            return false;
        }
        let present: Vec<usize> = if overlay { self.chans.iter().enumerate().filter(|(_, c)| keys.contains(&c.key)).map(|(i, _)| i).collect() } else { Vec::new() };
        let lane = match present.first() {
            Some(&i) => self.chans[i].lane,
            None => self.next_lane_and_bump(),
        };
        for &i in &present {
            self.chans[i].lane = lane;
        }
        for (j, k) in fresh.into_iter().enumerate() {
            let l = if overlay || j == 0 { lane } else { self.next_lane_and_bump() };
            let i = self.chans.len();
            self.chans.push(ChanView { hold_nonzero: self.catalogue.get(k.signal).is_some_and(|s| s.has(flag::ZERO_FILLED)), ..ChanView::new(k.clone(), chan_color(i, self.settings.dark), l) });
            if let Some(r) = &self.recorder {
                r.info(k.clone(), self.channel_info(&k));
            }
            if let Some(r) = &self.slow {
                r.info(k.clone(), self.channel_info(&k));
            }
        }
        self.sync_channels();
        true
    }

    fn next_lane_and_bump(&mut self) -> u32 {
        let l = self.next_lane;
        self.next_lane += 1;
        l
    }

    pub fn add_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.add.take() else { return };
        let sig = self.catalogue.get(d.signal).cloned();
        let select = sig.as_ref().map(|s| s.select).unwrap_or(Select::Axis);
        let loopback = self.session.status().loopback;
        let mut close = false;
        let mut to_add: Option<(Vec<ChannelKey>, bool)> = None;
        let free = MAX_CHANNELS - self.chans.len();

        let modal = egui::Modal::new(egui::Id::new("add-channel")).show(ctx, |ui| {
            ui.set_max_width(500.0);
            let title = sig.as_ref().map(|s| s.display_name()).unwrap_or_else(|| format!("Signal {}", d.signal));
            ui.heading(format!("Add {title} ({})", d.signal));
            if let Some(s) = &sig {
                ui.horizontal_wrapped(|ui| signal_badges(ui, s));
            }
            ui.label(RichText::new(select_words(select)).color(theme::pal(ui).ink2));
            ui.add_space(6.0);

            let unit_ok = MechUnit::new(&d.unit).is_ok();
            if select != Select::Controller {
                ui.horizontal(|ui| {
                    ui.add_sized([150.0, fields::HEIGHT], egui::Label::new(theme::b("robot")).halign(egui::Align::Min)).on_hover_text("The mechanical unit");
                    ui.spacing_mut().interact_size.y = fields::HEIGHT;
                    egui::ComboBox::from_id_salt("unit").selected_text(d.unit.clone()).width(110.0).icon(theme::combo_icon).show_ui(ui, |ui| {
                        for u in self.settings.units.clone() {
                            ui.selectable_value(&mut d.unit, u.clone(), u);
                        }
                    });
                    fields::line(ui, &mut d.unit, "Mechanical unit", |t| t.desired_width(90.0)).on_hover_text("Another unit name, e.g. ROB_3 or STN_1");
                });
                if !unit_ok {
                    ui.colored_label(theme::pal(ui).red, "A mechanical unit name is letters, digits and _ (like ROB_1).");
                }
                if select == Select::Module {
                    ui.label(RichText::new("The robot picks the drive module that feeds it.").small());
                }
            }
            if select == Select::Axis {
                ui.horizontal(|ui| {
                    ui.add_sized([150.0, fields::HEIGHT], egui::Label::new(theme::b("axis")).halign(egui::Align::Min));
                    ui.spacing_mut().item_spacing.x = 6.0;
                    for a in 1..=6u8 {
                        if theme::chip(ui, d.axis == a, &a.to_string()).clicked() {
                            d.axis = a;
                        }
                    }
                });
            }
            if select == Select::Number
                && let Some(j) = sig.as_ref().and_then(|s| s.joint) {
                    ui.label(format!("This number reads joint {j}."));
                }

            if let Some(s) = &sig {
                if s.has(flag::PHYSICAL) && loopback {
                    ui.colored_label(theme::pal(ui).hold, "You are connected to a virtual controller, which has no physical measurements: nothing will arrive for this signal.");
                }
                if s.has(flag::FROZEN) {
                    ui.colored_label(theme::pal(ui).hold, "FROZEN under EGM: it holds the last RAPID path position. For the live pose use 6040-6046.");
                }
                if s.confidence == Confidence::Inert {
                    ui.colored_label(theme::pal(ui).ink2, "Returned nothing on the measured cell.");
                }
            }
            ui.label(RichText::new(format!("{free} of {MAX_CHANNELS} channels free.")).small().weak());
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                let unit = MechUnit::new(&known_unit(&self.settings.units, &d.unit)).unwrap_or_else(|_| MechUnit::new("ROB_1").unwrap());
                let key = |signal: u32, axis: u8| ChannelKey { signal, unit: unit.clone(), axis: Axis::new(axis).unwrap_or(Axis::new(1).unwrap()) };
                let missing = |keys: Vec<ChannelKey>| keys.iter().filter(|k| !self.chans.iter().any(|c| &c.key == *k)).count();
                let enabled = unit_ok && free > 0;
                let why_not = |n: usize| if !unit_ok { "Type a mechanical unit name first (like ROB_1).".to_string() } else { format!("Needs {n} free channel(s); {free} of {MAX_CHANNELS} free. Remove a channel first.") };
                if ui.add_enabled_ui(enabled, |ui| theme::primary(ui, "add", fields::HEIGHT)).inner.on_disabled_hover_text(why_not(1)).clicked() {
                    let axis = if select == Select::Axis { d.axis } else { 1 };
                    to_add = Some((vec![key(d.signal, axis)], false));
                }
                if select == Select::Axis {
                    let need = missing((1..=6).map(|a| key(d.signal, a)).collect());
                    if ui.add_enabled(unit_ok && need > 0 && need <= free, egui::Button::new("add all six axes").min_size(egui::vec2(0.0, fields::HEIGHT))).on_hover_text("Six channels, overlaid in one chart").on_disabled_hover_text(if need == 0 { "All six are there already.".to_string() } else { why_not(need) }).clicked() {
                        to_add = Some(((1..=6).map(|a| key(d.signal, a)).collect(), true));
                    }
                }
                if select == Select::Number
                    && let Some(base) = sig.as_ref().and_then(|s| s.joint).and_then(|j| d.signal.checked_sub(u32::from(j.max(1) - 1))).filter(|b| *b > 0 && b.checked_add(5).is_some())
                {
                    let need = missing((base..base + 6).map(|n| key(n, 1)).collect());
                    if ui.add_enabled(unit_ok && need > 0 && need <= free, egui::Button::new(format!("add the block {}-{}", base, base + 5)).min_size(egui::vec2(0.0, fields::HEIGHT))).on_hover_text("All six joints, overlaid in one chart").on_disabled_hover_text(if need == 0 { "The block is there already.".to_string() } else { why_not(need) }).clicked() {
                        to_add = Some(((base..base + 6).map(|n| key(n, 1)).collect(), true));
                    }
                }
                if ui.add(egui::Button::new("cancel").min_size(egui::vec2(0.0, fields::HEIGHT))).clicked() {
                    close = true;
                }
            });
        });
        if modal.should_close() {
            close = true;
        }
        if let Some((keys, overlay)) = to_add
            && self.add_channels(keys, overlay)
        {
            close = true;
        }
        if !close {
            self.add = Some(d);
        }
    }
}

pub fn known_unit(units: &[String], typed: &str) -> String {
    let typed = typed.trim();
    units.iter().find(|u| u.eq_ignore_ascii_case(typed)).cloned().unwrap_or_else(|| typed.to_string())
}

#[allow(clippy::too_many_arguments)]
fn signal_row(ui: &mut egui::Ui, n: u32, name: &str, units: &str, count: usize, picked: bool, noted: bool, frozen: bool, height: f32) -> egui::Response {
    let p = theme::pal(ui);
    let (rect, r) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::click());
    r.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, picked, format!("{n} {name}{}{}", if frozen { " (frozen)" } else { "" }, if noted { " (notes)" } else { "" })));
    if !ui.is_rect_visible(rect) {
        return r;
    }
    let painter = ui.painter_at(rect);
    if picked {
        painter.rect_filled(rect, 0.0, p.picked);
    } else if r.hovered() {
        painter.rect_filled(rect, 0.0, p.face_hover);
    }
    painter.hline(rect.x_range(), rect.bottom() - 0.5, egui::Stroke::new(1.0, p.line));
    let galley = |text: RichText, w: f32| egui::WidgetText::from(text).into_galley(ui, Some(egui::TextWrapMode::Truncate), w, egui::TextStyle::Body);
    let mid = rect.center().y;
    let num = galley(theme::num(n.to_string(), 16.0), 60.0);
    painter.galley(egui::pos2(rect.left() + 4.0, mid - num.size().y / 2.0), num, p.ink);
    let mut right = rect.right() - 6.0;
    if !units.is_empty() {
        let u = galley(RichText::new(units).size(14.0), 80.0);
        right -= u.size().x;
        painter.galley(egui::pos2(right, mid - u.size().y / 2.0), u, p.ink3);
        right -= 8.0;
    }
    for (on, word, color) in [(frozen, "FROZEN", p.hold), (noted, "notes", p.live)] {
        if on {
            let g = galley(theme::b(word).size(14.0), 80.0);
            right -= g.size().x;
            painter.galley(egui::pos2(right, mid - g.size().y / 2.0), g, color);
            right -= 8.0;
        }
    }
    let left = rect.left() + 62.0;
    let more = if count > 1 { format!("  +{}", count - 1) } else { String::new() };
    let mut job = egui::text::LayoutJob::default();
    let body = egui::FontId::new(16.0, egui::FontFamily::Proportional);
    job.append(name, 0.0, egui::TextFormat::simple(body.clone(), p.ink));
    job.append(&more, 0.0, egui::TextFormat::simple(egui::FontId::new(14.0, egui::FontFamily::Proportional), p.ink3));
    job.wrap = egui::text::TextWrapping::truncate_at_width((right - left).max(20.0));
    let g = ui.fonts_mut(|f| f.layout_job(job));
    painter.galley(egui::pos2(left, mid - g.size().y / 2.0), g, p.ink);
    r
}
