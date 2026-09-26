//! The catalogue browser (left panel) and the add-channel dialog.
//!
//! By default only named signals show (D2); the open and the inert ones sit behind
//! toggles. Numbers that report the same quantity collapse into one row ("Motor
//! speed, 18 numbers, any works"). Adding a channel asks only what the signal's
//! selector needs, so the wrong choice cannot be made (proposal section 6).

use eframe::egui::{self, RichText};

use spy_core::catalogue::{flag, Confidence, Select, Signal};
use spy_core::log::Level;
use spy_core::request::{Axis, MechUnit};
use spy_core::session::MAX_CHANNELS;
use spy_core::store::ChannelKey;

use crate::app::{chan_color, AddDialog, ChanView, SpyApp, Stats};
use crate::theme;

pub fn confidence_color(c: Confidence) -> egui::Color32 {
    match c {
        Confidence::Confirmed => theme::OK,
        Confidence::Strong => egui::Color32::from_rgb(0x5A, 0x9B, 0xD5),
        Confidence::Probable => theme::WARN,
        Confidence::Open => egui::Color32::from_rgb(0xB0, 0x7A, 0xA1),
        Confidence::Inert => theme::IDLE,
    }
}

/// The badges a signal carries, with what each means.
pub fn signal_badges(ui: &mut egui::Ui, s: &Signal) {
    if s.has(flag::FROZEN) {
        theme::badge(ui, "FROZEN", theme::WARN, "Holds the last RAPID path-level value and does NOT move while EGM drives the robot. Do not use it to tell whether the robot moved.");
    }
    if s.has(flag::ZERO_FILLED) {
        theme::badge(ui, "0-FILL", theme::WARN, "Carries a value every few samples and pads the rest with exact zeros. The chart can hold the last non-zero value (channel menu).");
    }
    if s.has(flag::SENTINEL) || s.has(flag::NAN) {
        theme::badge(ui, "PLACEHOLDER", theme::IDLE, "Reads a placeholder (a huge value, -1 or NaN), not a measurement.");
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
        let shown = s.named || (self.settings.show_open && s.confidence == Confidence::Open) || (self.settings.show_inert && s.confidence == Confidence::Inert);
        if !shown && self.search.trim().chars().all(|c| c.is_ascii_digit()) && !self.search.trim().is_empty() {
            // A number typed in full finds its signal whatever the toggles say.
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
        // Confidence orders confirmed < strong < probable < open < inert.
        if let Some(min) = self.min_confidence
            && s.confidence > min
        {
            return false;
        }
        true
    }

    pub fn browser(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Signals").strong());
            ui.label(RichText::new(&self.catalogue.title).small().weak()).on_hover_text(format!("{}\n{}", self.catalogue.measured_on, self.catalogue.caveat));
        });
        ui.add(egui::TextEdit::singleline(&mut self.search).hint_text("search: number, name or unit").desired_width(f32::INFINITY));
        ui.horizontal_wrapped(|ui| {
            if ui.toggle_value(&mut self.settings.show_open, "open").on_hover_text("Also show signals that respond but are not yet identified").changed() {
                self.mark_settings_dirty();
            }
            if ui.toggle_value(&mut self.settings.show_inert, "inert").on_hover_text("Also show signals that returned nothing on the measured cell (features it did not use)").changed() {
                self.mark_settings_dirty();
            }
            ui.toggle_value(&mut self.only_favourites, "★").on_hover_text("Favourites only");
            let mut cats: Vec<String> = self.catalogue.signals.iter().map(|s| s.category.clone()).filter(|c| !c.is_empty()).collect();
            cats.sort();
            cats.dedup();
            egui::ComboBox::from_id_salt("category").selected_text(self.category.clone().unwrap_or_else(|| "all kinds".into())).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.category, None, "all kinds");
                for c in cats {
                    ui.selectable_value(&mut self.category, Some(c.clone()), c);
                }
            });
            let conf_text = match self.min_confidence {
                None => "any confidence",
                Some(Confidence::Confirmed) => "confirmed only",
                Some(Confidence::Strong) => "strong or better",
                Some(Confidence::Probable) => "probable or better",
                Some(_) => "any confidence",
            };
            egui::ComboBox::from_id_salt("confidence").selected_text(conf_text).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.min_confidence, None, "any confidence");
                ui.selectable_value(&mut self.min_confidence, Some(Confidence::Confirmed), "confirmed only");
                ui.selectable_value(&mut self.min_confidence, Some(Confidence::Strong), "strong or better");
                ui.selectable_value(&mut self.min_confidence, Some(Confidence::Probable), "probable or better");
            });
        });

        // Rows: one per alias group, in number order.
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

        let list_height = (ui.available_height() * 0.55).max(120.0);
        egui::ScrollArea::vertical().id_salt("catalogue-list").max_height(list_height).auto_shrink([false, false]).show_rows(ui, 22.0, rows.len(), |ui, range| {
            for &(n, count) in &rows[range] {
                let Some((name, units, frozen)) = self.catalogue.get(n).map(|s| (s.display_name(), s.units.clone(), s.has(flag::FROZEN))) else { continue };
                let selected = self.selected == Some(n);
                let text = if count > 1 { format!("{n:>5} +{:<2} {name}", count - 1) } else { format!("{n:>5}     {name}") };
                ui.horizontal(|ui| {
                    let r = ui.selectable_label(selected, RichText::new(text).monospace());
                    if r.clicked() {
                        self.selected = Some(n);
                    }
                    if r.double_clicked() {
                        self.open_add(n);
                    }
                    ui.label(RichText::new(units).small().weak());
                    if frozen {
                        ui.label(RichText::new("FROZEN").small().color(theme::WARN));
                    }
                });
            }
        });
        if rows.is_empty() {
            ui.label(RichText::new("Nothing matches. Turn on 'open' or 'inert', or add a raw number below.").weak());
        }

        ui.horizontal(|ui| {
            ui.label("Raw number");
            ui.add(egui::TextEdit::singleline(&mut self.raw_number).desired_width(70.0).hint_text("e.g. 4002"));
            if ui.button("Add...").clicked() {
                match self.raw_number.trim().parse::<u32>() {
                    Ok(n) if n > 0 => {
                        self.selected = Some(n);
                        self.open_add(n);
                    }
                    _ => self.toast(Level::Error, "Type a signal number (a whole number above 0)."),
                }
            }
        });
        ui.separator();
        self.details(ui);
    }

    fn details(&mut self, ui: &mut egui::Ui) {
        let Some(n) = self.selected else {
            ui.label(RichText::new("Pick a signal to see what it is and add it as a channel. Double-click adds it.").weak());
            return;
        };
        egui::ScrollArea::vertical().id_salt("details").auto_shrink([false, false]).show(ui, |ui| {
            let Some(s) = self.catalogue.get(n).cloned() else {
                ui.label(RichText::new(format!("Signal {n}")).heading());
                ui.label("Not in the catalogue. It can still be added; the controller will say whether it knows it.");
                if ui.button("Add as a channel...").clicked() {
                    self.open_add(n);
                }
                return;
            };
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(s.display_name()).heading());
                let fav = self.settings.favourites.contains(&n);
                if ui.selectable_label(fav, "★").on_hover_text("Favourite").clicked() {
                    if fav {
                        self.settings.favourites.retain(|&x| x != n);
                    } else {
                        self.settings.favourites.push(n);
                    }
                    self.mark_settings_dirty();
                }
            });
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(format!("{n}")).monospace().strong());
                ui.label(RichText::new(s.confidence.label()).color(confidence_color(s.confidence))).on_hover_text("confirmed: named by ABB or pinned exactly · strong: reproduced on 12 readings · probable: fits, not forced · open: responds, not identified · inert: returned nothing on the measured cell");
                if !s.units.is_empty() {
                    ui.label(format!("[{}]", s.units));
                }
                if let Some(ms) = s.sample_ms {
                    ui.label(format!("{ms} ms"));
                }
                if let Some(t) = &s.value_type {
                    ui.label(t.as_str());
                }
                signal_badges(ui, &s);
            });
            if let Some(g) = &s.group {
                let members = self.catalogue.groups().get(g).cloned().unwrap_or_default();
                if members.len() > 1 {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(format!("The same quantity under {} numbers:", members.len())).small());
                        for m in members {
                            if ui.selectable_label(m == n, RichText::new(m.to_string()).small().monospace()).clicked() {
                                self.selected = Some(m);
                            }
                        }
                    });
                }
            }
            if let Some(a) = &s.abb {
                let src = match a.source.as_str() {
                    "TuneMaster" => "TuneMaster's signal table",
                    "TRM" => "the RAPID manual",
                    "RobAPI" => "RobotStudio's RobAPI",
                    "forum" => "ABB's forum",
                    o => o,
                };
                ui.label(RichText::new(format!("ABB's name: {} (from {src})", a.name)).small());
            }
            ui.label(RichText::new(select_words(s.select)).small().italics());
            ui.add_space(4.0);
            if !s.description.is_empty() {
                ui.label(&s.description);
            }
            if !s.cell && s.vc {
                ui.colored_label(theme::WARN, "Seen only on a virtual controller.");
            }
            if s.confidence == Confidence::Inert {
                ui.colored_label(theme::IDLE, "Returned nothing on the measured cell. It may on a controller that uses the feature.");
            }
            for (title, text) in [("Evidence", &s.evidence), ("Ruled out", &s.ruled_out), ("Open question", &s.open_question), ("Next test", &s.next_test)] {
                if !text.is_empty() {
                    egui::CollapsingHeader::new(title).id_salt((title, n)).show(ui, |ui| {
                        ui.label(RichText::new(text.as_str()).small());
                    });
                }
            }
            ui.add_space(6.0);
            if ui.add(egui::Button::new(RichText::new("Add as a channel...").strong())).clicked() {
                self.open_add(n);
            }
        });
    }

    pub fn open_add(&mut self, signal: u32) {
        let unit = self.settings.units.first().cloned().unwrap_or_else(|| "ROB_1".into());
        let axis = self.catalogue.get(signal).and_then(|s| s.joint).unwrap_or(1);
        self.add = Some(AddDialog { signal, unit, axis });
    }

    /// Add channels; false (and a message) when that would break the rules.
    pub fn add_channels(&mut self, keys: Vec<ChannelKey>, overlay: bool) -> bool {
        let fresh: Vec<ChannelKey> = keys.into_iter().filter(|k| !self.chans.iter().any(|c| &c.key == k)).collect();
        if fresh.is_empty() {
            self.toast(Level::Warn, "That channel is already there.");
            return false;
        }
        if self.chans.len() + fresh.len() > MAX_CHANNELS {
            self.toast(Level::Error, format!("At most {MAX_CHANNELS} channels at once: {} free.", MAX_CHANNELS - self.chans.len()));
            return false;
        }
        let lane = self.next_lane;
        self.next_lane += 1;
        for (j, k) in fresh.into_iter().enumerate() {
            // Overlaid channels share the first one's chart; otherwise each gets its own.
            let l = if overlay || j == 0 { lane } else { self.next_lane_and_bump() };
            let i = self.chans.len();
            self.chans.push(ChanView { key: k.clone(), color: chan_color(i), radians: false, hold_nonzero: self.catalogue.get(k.signal).is_some_and(|s| s.has(flag::ZERO_FILLED)), lane: l, stats: Stats::default() });
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
            ui.heading(format!("Add {}  ({})", title, d.signal));
            if let Some(s) = &sig {
                ui.horizontal_wrapped(|ui| signal_badges(ui, s));
            }
            ui.label(RichText::new(select_words(select)).italics());
            ui.add_space(6.0);

            let unit_ok = MechUnit::new(&d.unit).is_ok();
            if select != Select::Controller {
                ui.horizontal(|ui| {
                    ui.label("Robot (mechanical unit)");
                    egui::ComboBox::from_id_salt("unit").selected_text(d.unit.clone()).show_ui(ui, |ui| {
                        for u in self.settings.units.clone() {
                            ui.selectable_value(&mut d.unit, u.clone(), u);
                        }
                    });
                    ui.add(egui::TextEdit::singleline(&mut d.unit).desired_width(90.0)).on_hover_text("Another unit name, e.g. ROB_3 or STN_1");
                });
                if !unit_ok {
                    ui.colored_label(theme::BAD, "A mechanical unit name is letters, digits and _ (like ROB_1).");
                }
                if select == Select::Module {
                    ui.label(RichText::new("The robot picks the drive module that feeds it.").small());
                }
            }
            if select == Select::Axis {
                ui.horizontal(|ui| {
                    ui.label("Axis");
                    for a in 1..=6u8 {
                        ui.selectable_value(&mut d.axis, a, format!(" {a} "));
                    }
                });
            }
            if select == Select::Number
                && let Some(j) = sig.as_ref().and_then(|s| s.joint) {
                    ui.label(format!("This number reads joint {j}."));
                }

            // Warnings that stop the known mistakes.
            if let Some(s) = &sig {
                if s.has(flag::PHYSICAL) && loopback {
                    ui.colored_label(theme::WARN, "You are connected to a virtual controller, which has no physical measurements: nothing will arrive for this signal.");
                }
                if s.has(flag::FROZEN) {
                    ui.colored_label(theme::WARN, "FROZEN under EGM: it holds the last RAPID path position. For the live pose use 6040-6046.");
                }
                if s.confidence == Confidence::Inert {
                    ui.colored_label(theme::IDLE, "Returned nothing on the measured cell.");
                }
            }
            ui.label(RichText::new(format!("{free} of {MAX_CHANNELS} channels free.")).small().weak());
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                let unit = MechUnit::new(&d.unit).unwrap_or_else(|_| MechUnit::new("ROB_1").unwrap());
                let key = |signal: u32, axis: u8| ChannelKey { signal, unit: unit.clone(), axis: Axis::new(axis).unwrap_or(Axis::new(1).unwrap()) };
                let enabled = unit_ok && free > 0;
                if ui.add_enabled(enabled, egui::Button::new(RichText::new("Add").strong())).clicked() {
                    let axis = if select == Select::Axis { d.axis } else { 1 };
                    to_add = Some((vec![key(d.signal, axis)], false));
                }
                if select == Select::Axis && ui.add_enabled(unit_ok && free >= 6, egui::Button::new("Add all six axes")).on_hover_text("Six channels, overlaid in one chart").clicked() {
                    to_add = Some(((1..=6).map(|a| key(d.signal, a)).collect(), true));
                }
                // A loaded catalogue could claim joint 6 for signal 3: then there is
                // no block, rather than an underflow.
                if select == Select::Number
                    && let Some(base) = sig.as_ref().and_then(|s| s.joint).and_then(|j| d.signal.checked_sub(u32::from(j.max(1) - 1))).filter(|b| *b > 0 && b.checked_add(5).is_some())
                    && ui.add_enabled(unit_ok && free >= 6, egui::Button::new(format!("Add the block {}-{}", base, base + 5))).on_hover_text("All six joints, overlaid in one chart").clicked()
                {
                    to_add = Some(((base..base + 6).map(|n| key(n, 1)).collect(), true));
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });
        if modal.should_close() {
            close = true;
        }
        if let Some((keys, overlay)) = to_add {
            if let Ok(u) = MechUnit::new(&d.unit)
                && !self.settings.units.contains(&u.to_string()) {
                    self.settings.units.push(u.to_string());
                    self.mark_settings_dirty();
                }
            if self.add_channels(keys, overlay) {
                close = true;
            }
        }
        if !close {
            self.add = Some(d);
        }
    }
}
