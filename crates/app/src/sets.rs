use eframe::egui::{self, RichText};

use spy_core::catalogue::flag;
use spy_core::log::Level;
use spy_core::request::{Axis, MechUnit};
use spy_core::session::MAX_CHANNELS;
use spy_core::store::ChannelKey;

use crate::app::SpyApp;
use crate::fields;
use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    OneRobot,
    EachRobot,
    Controller,
}

#[derive(Debug)]
pub struct ChannelSet {
    pub name: &'static str,
    pub about: &'static str,
    pub scope: Scope,
    pub signals: &'static [u32],
    pub six_axes: bool,
    pub overlay: bool,
}

pub const SETS: &[ChannelSet] = &[
    ChannelSet {
        name: "DC links",
        about: "The DC-link voltage (5027) of each drive module, in one chart. A robot picks the module that feeds it: tick one robot per module.",
        scope: Scope::EachRobot,
        signals: &[5027],
        six_axes: false,
        overlay: true,
    },
    ChannelSet { name: "torques", about: "One robot's six joint torques (4002), in one chart.", scope: Scope::OneRobot, signals: &[4002], six_axes: true, overlay: true },
    ChannelSet { name: "joint positions", about: "One robot's six joint positions (4000), in one chart.", scope: Scope::OneRobot, signals: &[4000], six_axes: true, overlay: true },
    ChannelSet {
        name: "resolver angles",
        about: "One robot's six resolver angles (5138: the motor's angle within one turn), a chart each.",
        scope: Scope::OneRobot,
        signals: &[5138],
        six_axes: true,
        overlay: false,
    },
    ChannelSet {
        name: "the 8000-8009 block",
        about: "Ten slow, controller-wide measurements, not yet identified, a chart each. Made for hours of slow logging (Slow log).",
        scope: Scope::Controller,
        signals: &[8000, 8001, 8002, 8003, 8004, 8005, 8006, 8007, 8008, 8009],
        six_axes: false,
        overlay: false,
    },
];

pub fn keys(set: &ChannelSet, units: &[MechUnit]) -> Vec<ChannelKey> {
    let units: &[MechUnit] = match set.scope {
        Scope::EachRobot => units,
        Scope::OneRobot | Scope::Controller => &units[..units.len().min(1)],
    };
    let axes: &[u8] = if set.six_axes { &[1, 2, 3, 4, 5, 6] } else { &[1] };
    let mut out = Vec::new();
    for u in units {
        for &signal in set.signals {
            for &a in axes {
                out.push(ChannelKey { signal, unit: u.clone(), axis: Axis::new(a).expect("axes 1 to 6") });
            }
        }
    }
    out
}

#[derive(Debug, Clone)]
pub struct SetDialog {
    pub set: usize,
    pub unit: String,
    pub ticked: Vec<String>,
    pub asking_replace: bool,
}

fn channels(n: usize) -> String {
    if n == 1 { "1 channel".into() } else { format!("{n} channels") }
}

impl SpyApp {
    pub fn open_sets(&mut self) {
        let unit = self.settings.units.first().cloned().unwrap_or_else(|| "ROB_1".into());
        self.sets = Some(SetDialog { set: 0, unit, ticked: self.settings.units.clone(), asking_replace: false });
    }

    pub fn replace_channels(&mut self, keys: Vec<ChannelKey>, overlay: bool) -> bool {
        if keys.len() > MAX_CHANNELS {
            self.toast(Level::Error, format!("At most {MAX_CHANNELS} channels at once."));
            return false;
        }
        let old = std::mem::take(&mut self.chans);
        if self.add_channels(keys, overlay) {
            self.log.info(format!("Replaced {} channel(s) with a set of {}.", old.len(), self.chans.len()));
            true
        } else {
            self.chans = old;
            self.sync_channels();
            false
        }
    }

    pub fn sets_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.sets.take() else { return };
        let mut close = false;
        let mut act: Option<(Vec<ChannelKey>, bool, bool)> = None;
        let loopback = self.session.status().loopback;
        let modal = egui::Modal::new(egui::Id::new("channel-sets")).show(ctx, |ui| {
            ui.set_max_width(520.0);
            ui.heading("add a channel set");
            let room = (ctx.content_rect().height() - 230.0).max(160.0);
            let (keys, fresh, free, overlay) = egui::ScrollArea::vertical()
                .id_salt("sets-body")
                .max_height(room)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        for (i, s) in SETS.iter().enumerate() {
                            if theme::chip(ui, d.set == i, s.name).clicked() {
                                d.set = i;
                            }
                        }
                    });
                    let set = &SETS[d.set.min(SETS.len() - 1)];
                    ui.add_space(4.0);
                    ui.label(set.about);
                    ui.add_space(6.0);
                    match set.scope {
                        Scope::OneRobot => {
                            ui.horizontal(|ui| {
                                ui.label("robot (mechanical unit)");
                                let units = self.settings.units.clone();
                                fields::with_choices(ui, &mut d.unit, "Mechanical unit", &units, 120.0).on_hover_text("ROB_1, ROB_2, or another unit name such as ROB_3: the arrow lists the ones known");
                            });
                        }
                        Scope::EachRobot => {
                            ui.horizontal_wrapped(|ui| {
                                ui.label("robots");
                                for u in &self.settings.units {
                                    let mut on = d.ticked.contains(u);
                                    if theme::check(ui, &mut on, u).changed() {
                                        if on {
                                            d.ticked.push(u.clone());
                                        } else {
                                            d.ticked.retain(|x| x != u);
                                        }
                                    }
                                }
                            });
                            ui.label(RichText::new("A robot the controller does not have is refused, and says so.").small().weak());
                        }
                        Scope::Controller => {}
                    }
                    let units: Result<Vec<MechUnit>, String> = match set.scope {
                        Scope::OneRobot => MechUnit::new(&crate::browser::known_unit(&self.settings.units, &d.unit)).map(|u| vec![u]).map_err(|_| "A mechanical unit name is letters, digits and _ (like ROB_1).".to_string()),
                        Scope::EachRobot => self.settings.units.iter().filter(|u| d.ticked.contains(u)).map(|u| MechUnit::new(u).map_err(|_| format!("{u} is not a mechanical unit name."))).collect(),
                        Scope::Controller => Ok(vec![self.settings.units.first().and_then(|u| MechUnit::new(u).ok()).unwrap_or_else(|| MechUnit::new("ROB_1").expect("a valid name"))]),
                    };
                    let keys = match &units {
                        Ok(u) if u.is_empty() => Err("Tick at least one robot.".to_string()),
                        Ok(u) => Ok(keys(set, u)),
                        Err(e) => Err(e.clone()),
                    };
                    match &keys {
                        Ok(k) => {
                            let names: Vec<String> = k.iter().map(|k| crate::view::label(&self.catalogue, k)).collect();
                            ui.label(RichText::new(format!("{}: {}", channels(k.len()), names.join(", "))).small());
                        }
                        Err(e) => {
                            ui.colored_label(theme::pal(ui).red, e);
                        }
                    }
                    if loopback && set.signals.iter().any(|&n| self.catalogue.get(n).is_some_and(|s| s.has(flag::PHYSICAL))) {
                        ui.colored_label(theme::pal(ui).hold, "You are connected to a virtual controller, which has no physical measurements: nothing will arrive for these.");
                    }
                    let fresh = keys.as_ref().map(|k| k.iter().filter(|k| !self.chans.iter().any(|c| &c.key == *k)).count()).unwrap_or(0);
                    let free = MAX_CHANNELS - self.chans.len();
                    ui.label(RichText::new(format!("{free} of {MAX_CHANNELS} channels free.")).small().weak());
                    (keys, fresh, free, set.overlay)
                })
                .inner;
            ui.add_space(8.0);
            let ok = keys.is_ok();
            let why = if let Err(e) = &keys { e.clone() } else if fresh == 0 { "They are all there already.".into() } else { format!("Needs {} free; {free} of {MAX_CHANNELS} free. Replace the channels instead, or remove some first.", channels(fresh)) };
            let can_add = ok && fresh > 0 && fresh <= free;
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            if d.asking_replace {
                let n = keys.as_ref().map_or(0, Vec::len);
                ui.label(theme::b(format!("Remove the {} there now and add these {} instead?", channels(self.chans.len()), channels(n))).color(theme::pal(ui).red));
                ui.horizontal(|ui| {
                    if theme::red_button(ui, egui::Button::new("replace them").min_size(egui::vec2(0.0, fields::HEIGHT))).clicked()
                        && let Ok(k) = &keys
                    {
                        act = Some((k.clone(), overlay, true));
                        d.asking_replace = false;
                    }
                    if ui.add(egui::Button::new("keep them").min_size(egui::vec2(0.0, fields::HEIGHT))).clicked() {
                        d.asking_replace = false;
                    }
                });
            } else {
                ui.horizontal(|ui| {
                    let add = ui.add_enabled_ui(can_add, |ui| theme::primary(ui, "add", fields::HEIGHT)).inner.on_hover_text("Enter").on_disabled_hover_text(&why);
                    if (add.clicked() || (can_add && enter))
                        && let Ok(k) = &keys
                    {
                        act = Some((k.clone(), overlay, false));
                    }
                    if !self.chans.is_empty()
                        && theme::lockable(ui, ok, egui::Button::new(format!("replace all {}", channels(self.chans.len()))).min_size(egui::vec2(0.0, fields::HEIGHT)))
                            .on_hover_text("Remove the channels there now and add this set instead")
                            .on_disabled_hover_text(&why)
                            .clicked()
                    {
                        d.asking_replace = true;
                    }
                    if ui.add(egui::Button::new("cancel").min_size(egui::vec2(0.0, fields::HEIGHT))).clicked() {
                        close = true;
                    }
                });
            }
        });
        if modal.should_close() {
            close = true;
        }
        if let Some((keys, overlay, replace)) = act {
            let set = &SETS[d.set.min(SETS.len() - 1)];
            let done = if replace { self.replace_channels(keys, overlay) } else { self.add_channels(keys, overlay) };
            if done {
                close = true;
                if set.scope == Scope::Controller && self.slow.is_none() {
                    self.toast(Level::Info, "Added. For hours of logging, start the Slow log.");
                }
            }
        }
        if !close {
            self.sets = Some(d);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(s: &str) -> MechUnit {
        MechUnit::new(s).unwrap()
    }

    #[test]
    fn a_set_names_its_channels() {
        let by_name = |n: &str| SETS.iter().find(|s| s.name == n).unwrap();
        let ids = |k: Vec<ChannelKey>| k.iter().map(|k| k.id()).collect::<Vec<_>>();
        assert_eq!(ids(keys(by_name("DC links"), &[unit("ROB_1"), unit("ROB_2")])), ["5027/ROB_1/J1", "5027/ROB_2/J1"]);
        assert_eq!(ids(keys(by_name("torques"), &[unit("ROB_2"), unit("ROB_1")])), ["4002/ROB_2/J1", "4002/ROB_2/J2", "4002/ROB_2/J3", "4002/ROB_2/J4", "4002/ROB_2/J5", "4002/ROB_2/J6"], "one robot: the first");
        assert_eq!(keys(by_name("resolver angles"), &[unit("ROB_1")]).iter().map(|k| k.signal).collect::<Vec<_>>(), [5138; 6]);
        let block = keys(by_name("the 8000-8009 block"), &[unit("ROB_1"), unit("ROB_2")]);
        assert_eq!(block.iter().map(|k| k.signal).collect::<Vec<_>>(), (8000..=8009).collect::<Vec<_>>(), "controller-wide: once");
        assert!(keys(by_name("DC links"), &[]).is_empty());
        for s in SETS {
            assert!(keys(s, &[unit("ROB_1"), unit("ROB_2")]).len() <= MAX_CHANNELS, "{}", s.name);
        }
    }

    #[test]
    fn every_set_signal_is_selected_the_way_the_set_asks() {
        let cat = spy_core::catalogue::Catalogue::builtin();
        for s in SETS {
            for &n in s.signals {
                let sig = cat.get(n).unwrap_or_else(|| panic!("{n} of {} is not in the catalogue", s.name));
                let want = match (s.scope, s.six_axes) {
                    (_, true) => spy_core::catalogue::Select::Axis,
                    (Scope::EachRobot, false) => spy_core::catalogue::Select::Module,
                    (Scope::Controller, false) => spy_core::catalogue::Select::Controller,
                    (Scope::OneRobot, false) => spy_core::catalogue::Select::Robot,
                };
                assert_eq!(sig.select, want, "{n} in {}", s.name);
            }
        }
    }
}
