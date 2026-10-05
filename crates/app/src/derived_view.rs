use eframe::egui::{self, Color32, RichText};

use spy_core::catalogue::{flag, Select, Signal};
use spy_core::derived::{self, Derived, Live};
use spy_core::log::Level;
use spy_core::session::{Status, MAX_CHANNELS};
use spy_core::store::ChannelKey;

use crate::app::{chan_color, SpyApp, Stats};
use crate::fields;
use crate::theme;
use crate::view::{self, Health};

pub const LAG_MS: i64 = 1000;
pub const PLATEAU_STEADY: f64 = 0.01;

pub struct DerivedView {
    pub live: Live,
    pub color: Color32,
    pub lane: u32,
    pub stats: Stats,
    pub target_text: String,
    pub target_from: Option<String>,
}

pub fn offers(sig: Option<&Signal>, key: &ChannelKey) -> Vec<Derived> {
    let Some(s) = sig else { return Vec::new() };
    let mut v = Vec::new();
    if s.has(flag::WRAPPING) && s.units.trim() == "rad" {
        v.push(Derived::Turn { angle: key.clone(), target_deg: None });
    }
    if derived::PWM_LEGS.contains(&key.signal) && s.name.to_lowercase().contains("duty") {
        v.push(Derived::duty_sum(key));
    }
    if s.units.trim() == "V" && s.select == Select::Module {
        v.push(Derived::Sag { link: key.clone(), plateau_v: None });
    }
    v
}

pub fn commutator_instance(angle: &ChannelKey) -> Result<String, &'static str> {
    if angle.signal != derived::RESOLVER_ANGLE {
        return Err("A commutator offset is what the resolver angle 5138 reads at the commutation position. 5000 and 7325 read the resolver a fixed offset away from it, and the electrical angles (5028, 5029, 7022) turn five times per motor turn: add a turn on 5138 for this.");
    }
    spy_core::rws::calib_instance(angle.unit.as_str(), angle.axis.one_based()).ok_or("Only a robot's axes (ROB_1, ROB_2, ...) have their calibration named this way.")
}

pub fn file_name(def: &Derived) -> String {
    match def {
        Derived::Turn { target_deg: Some(t), .. } => format!("{}, target {} deg", def.name(), view::fmt(*t)),
        Derived::Sag { plateau_v: Some(p), .. } => format!("{}, plateau {} V", def.name(), view::fmt(*p)),
        _ => def.name(),
    }
}

pub fn offer_text(d: &Derived) -> &'static str {
    match d {
        Derived::Turn { .. } => "Turn to a target...",
        Derived::DutySum { .. } => "PWM duty sum of this axis",
        Derived::Sag { .. } => "Sag below a plateau",
    }
}

pub fn sag_share(sag: f64, plateau: f64) -> String {
    let share = 100.0 * sag / plateau;
    if share < 0.0 { format!("{:.2}% above", -share) } else { format!("{share:.2}% below") }
}

pub fn reading(d: &Derived) -> view::Reading {
    match d {
        Derived::Turn { .. } => view::Reading::Turn,
        _ => view::Reading::Plain,
    }
}

pub fn value_text(def: &Derived, v: Option<f64>, live: bool) -> (String, bool) {
    match (def, v) {
        (Derived::Turn { .. }, Some(x)) if live && x.abs() <= derived::ON_TARGET_DEG => ("ON TARGET".into(), true),
        (Derived::Turn { .. }, Some(x)) => (format!("{x:+.3}"), false),
        (_, Some(x)) => (view::fmt(x), false),
        (_, None) => ("--".into(), false),
    }
}

pub fn keeps_up(inputs_newest: Option<i64>, newest: Option<i64>) -> bool {
    matches!((inputs_newest, newest), (Some(a), Some(b)) if a - b <= LAG_MS)
}

pub fn least_live(hs: &[Health]) -> Health {
    const ORDER: [Health; 8] = [Health::NotConnected, Health::Refused, Health::NotOnVc, Health::NoReply, Health::Waiting, Health::NoEventYet, Health::Stale, Health::Live];
    ORDER.iter().copied().find(|h| hs.contains(h)).unwrap_or(Health::NotConnected)
}

impl SpyApp {
    pub fn derived_label(&self, d: &Derived) -> String {
        d.name()
    }

    pub fn derived_formula(&self, d: &Derived) -> String {
        match d {
            Derived::Turn { angle, target_deg } => format!(
                "target{} − {} ({}), the short way round, in degrees within ±180. Positive: the angle has to increase to reach the target. ON TARGET within {} deg.",
                target_deg.map(|t| format!(" {} deg", view::fmt(t))).unwrap_or_default(),
                view::label(&self.catalogue, angle),
                angle.signal,
                derived::ON_TARGET_DEG
            ),
            Derived::DutySum { legs } => format!(
                "{} + {} + {} on {} J{}: the three PWM leg duty ratios, which add up to {:.2} by construction (space-vector modulation).",
                legs[0].signal,
                legs[1].signal,
                legs[2].signal,
                legs[0].unit,
                legs[0].axis.one_based(),
                derived::DUTY_SUM
            ),
            Derived::Sag { link, plateau_v } => format!(
                "plateau{} − {} ({}): positive below the plateau.",
                plateau_v.map(|p| format!(" {} V", view::fmt(p))).unwrap_or_default(),
                view::label(&self.catalogue, link),
                link.signal
            ),
        }
    }

    pub fn add_derived(&mut self, def: Derived) -> bool {
        if self.derived.iter().any(|d| d.live.def().same(&def)) {
            self.toast(Level::Warn, format!("{} is there already.", self.derived_label(&def)));
            return false;
        }
        let missing: Vec<ChannelKey> = def.inputs().into_iter().filter(|k| !self.chans.iter().any(|c| &c.key == k)).collect();
        if !missing.is_empty() {
            if self.chans.len() + missing.len() > MAX_CHANNELS {
                self.toast(Level::Error, format!("{} needs {} more channel(s) for its inputs; {} of {MAX_CHANNELS} are free.", self.derived_label(&def), missing.len(), MAX_CHANNELS - self.chans.len()));
                return false;
            }
            if !self.add_channels(missing, true) {
                return false;
            }
        }
        let lane = self.next_lane;
        self.next_lane += 1;
        let label = self.derived_label(&def);
        let color = chan_color(self.chans.len() + self.derived.len(), self.settings.dark);
        self.derived.push(DerivedView { live: Live::new(def), color, lane, stats: Stats::default(), target_text: String::new(), target_from: None });
        self.derived_changed(Some(format!("Added {label}.")));
        true
    }

    pub fn derived_changed(&mut self, what: Option<String>) {
        if let Some(w) = &what {
            self.log.info(w.clone());
        }
        self.derived_recorded(what);
    }

    pub fn derived_recorded(&mut self, what: Option<String>) {
        let defs: Vec<Derived> = self.derived.iter().map(|d| d.live.def().clone()).collect();
        for r in [&self.recorder, &self.slow].into_iter().flatten() {
            r.derived(defs.clone(), what.clone());
        }
        self.mark_settings_dirty();
    }

    pub fn prune_derived(&mut self) {
        let mut gone = Vec::new();
        self.derived.retain(|d| {
            let lost = d.live.def().inputs().into_iter().find(|k| !self.chans.iter().any(|c| &c.key == k));
            if let Some(k) = &lost {
                gone.push((d.live.def().clone(), k.clone()));
            }
            lost.is_none()
        });
        for (def, k) in gone {
            let text = format!("Removed {}: its input {k} was removed.", self.derived_label(&def));
            self.derived_changed(Some(text));
        }
    }

    pub fn update_derived(&mut self) {
        let store = self.session.store().clone();
        let streaming = self.session.status().announce.as_ref().and_then(|a| a.system_id.clone());
        let mut dropped = Vec::new();
        let mut targets_gone = Vec::new();
        for d in &mut self.derived {
            if let (Some(from), Some(now)) = (&d.target_from, &streaming)
                && !from.eq_ignore_ascii_case(now)
                && let Derived::Turn { angle, .. } = d.live.def().clone()
            {
                d.live.set(Derived::Turn { angle, target_deg: None });
                d.target_from = None;
                d.target_text.clear();
                d.stats = Stats::default();
                targets_gone.push(d.live.def().clone());
            }
            let before = d.live.def().is_set();
            d.live.update(&store);
            if before && !d.live.def().is_set() {
                d.stats = Stats::default();
                dropped.push(d.live.def().clone());
            }
            let ring = d.live.lock();
            let from = if d.stats.upto == i64::MIN { i64::MIN } else { d.stats.upto + 1 };
            for (t, v) in ring.range(from, i64::MAX) {
                d.stats.upto = t;
                if v.is_finite() {
                    d.stats.n += 1;
                    d.stats.sum += v;
                    d.stats.min = d.stats.min.min(v);
                    d.stats.max = d.stats.max.max(v);
                }
            }
        }
        for def in dropped {
            let text = format!("The plateau of {} was cleared: the history started afresh (another controller), and its DC link is not measured against the old one's. Set it again.", self.derived_label(&def));
            self.toast(Level::Warn, text.clone());
            self.derived_recorded(Some(text));
        }
        for def in targets_gone {
            let text = format!("The target of {} was cleared: it was the commutator offset of another controller than the one streaming now. Read it again from this one, or type one.", self.derived_label(&def));
            self.toast(Level::Warn, text.clone());
            self.derived_recorded(Some(text));
        }
    }

    fn stats_from_now(&self, def: &Derived) -> Stats {
        let upto = def.inputs().iter().filter_map(|k| self.session.store().get(k).and_then(|c| c.lock().last().map(|(t, _)| t))).min().unwrap_or(i64::MIN);
        Stats { upto, ..Stats::default() }
    }

    pub fn derived_health(&self, i: usize, st: &Status) -> Health {
        let d = &self.derived[i];
        let def = d.live.def();
        let connected = view::session_live(&st.phase);
        let inputs = def.inputs();
        let hs: Vec<Health> = inputs.iter().map(|k| view::health(st.channels.iter().find(|c| &c.key == k), connected, self.catalogue.get(k.signal), st.loopback)).collect();
        let worst = least_live(&hs);
        if worst != Health::Live {
            return worst;
        }
        if !def.is_set() {
            return Health::Waiting;
        }
        let newest_in = inputs.iter().map(|k| self.session.store().get(k).and_then(|c| c.lock().last().map(|(t, _)| t))).collect::<Option<Vec<i64>>>().and_then(|v| v.into_iter().min());
        let newest = d.live.lock().last().map(|(t, _)| t);
        if keeps_up(newest_in, newest) { Health::Live } else { Health::Stale }
    }

    pub fn set_target(&mut self, i: usize) {
        let text = self.derived[i].target_text.trim().trim_end_matches("deg").trim_end_matches('°').trim().replace(',', ".");
        let Ok(t) = text.parse::<f64>() else {
            self.toast(Level::Error, format!("\"{}\" is not an angle in degrees.", self.derived[i].target_text.trim()));
            return;
        };
        if !t.is_finite() {
            self.toast(Level::Error, "The target must be a number of degrees.");
            return;
        }
        let Derived::Turn { angle, .. } = self.derived[i].live.def().clone() else { return };
        let def = Derived::Turn { angle, target_deg: Some(t) };
        let label = self.derived_label(&def);
        self.derived[i].stats = self.stats_from_now(&def);
        self.derived[i].live.set(def);
        self.derived[i].target_from = None;
        self.derived_changed(Some(format!("{label}: target set to {} deg.", view::fmt(t))));
    }

    pub fn set_plateau(&mut self, i: usize) {
        let Derived::Sag { link, .. } = self.derived[i].live.def().clone() else { return };
        let st = self.session.status().clone();
        let h = view::health(st.channels.iter().find(|c| c.key == link), view::session_live(&st.phase), self.catalogue.get(link.signal), st.loopback);
        if !h.is_live() {
            self.toast(Level::Error, format!("The DC link is not live ({}): a plateau needs its last two seconds.", h.word()));
            return;
        }
        let span = self.plateau_trend_ms;
        let got = match self.session.store().get(&link) {
            Some(c) => {
                let r = c.lock();
                derived::plateau(&r).map(|p| (p, derived::level_before(&r, span)))
            }
            None => Err("nothing has arrived from the DC link yet".into()),
        };
        let ((mean, sd), before) = match got {
            Ok(x) => x,
            Err(e) => return self.toast(Level::Error, format!("No plateau: {e}.")),
        };
        if mean.is_nan() || mean < derived::PLATEAU_MIN_V {
            self.toast(Level::Error, format!("No plateau: the DC link reads {} V, not an armed drive's (below {} V). Arm the robot (motors on), keep it still, then set the plateau.", view::fmt(mean), derived::PLATEAU_MIN_V));
            return;
        }
        if sd > mean.abs() * PLATEAU_STEADY {
            self.toast(Level::Error, format!("No plateau: the DC link was not steady (mean {} V, standard deviation {} V over the last two seconds). Set it with the robot armed and still.", view::fmt(mean), view::fmt(sd)));
            return;
        }
        let secs = span as f64 / 1000.0;
        let before = match before {
            Ok(b) => b,
            Err(e) => return self.toast(Level::Error, format!("No plateau yet: {e}. Keep the robot armed and still, and set it again in a moment.")),
        };
        if !derived::level_holds(before, mean) {
            let why = if mean < before {
                "it is draining, as it does for about 20 minutes after the motors go off. Arm the robot (motors on)"
            } else {
                "it is still coming up after the motors came on. Wait"
            };
            self.toast(
                Level::Error,
                format!("No plateau: the DC link {} {} V to {} V in the last {secs:.0} s: {why}, keep the robot still, and set the plateau once the link has held level for {secs:.0} s.", if mean < before { "fell from" } else { "rose from" }, view::fmt(before), view::fmt(mean)),
            );
            return;
        }
        let def = Derived::Sag { link, plateau_v: Some(mean) };
        let label = self.derived_label(&def);
        self.derived[i].stats = self.stats_from_now(&def);
        self.derived[i].live.set(def);
        let text = format!("{label}: plateau set to {} V (the mean of the last two seconds; standard deviation {} V).", view::fmt(mean), view::fmt(sd));
        self.show_toast(Level::Info, text.clone());
        self.derived_changed(Some(text));
    }

    pub fn derived_rows(&mut self, ui: &mut egui::Ui, st: &Status, cursors: Option<&std::collections::HashMap<String, crate::charts::CursorReading>>) -> Option<String> {
        if self.derived.is_empty() {
            return None;
        }
        let p = theme::pal(ui);
        ui.add_space(4.0);
        ui.label(theme::b("derived").color(p.ink2)).on_hover_text("Computed here from the channels above; never streamed, and never written into a recording as data (a recording keeps how, and a review computes them again).");
        let mut open = None;
        let mut set_target = None;
        let mut set_plateau = None;
        let mut from_controller = None;
        let rws = self.rws_ready();
        for i in 0..self.derived.len() {
            let h = self.derived_health(i, st);
            let def = self.derived[i].live.def().clone();
            let label = self.derived_label(&def);
            let formula = self.derived_formula(&def);
            let value = view::readout(&self.derived[i].live.lock(), reading(&def));
            let color = self.derived[i].color;
            let units = def.units();
            let stale = h == Health::Stale;
            let s = self.derived[i].stats;
            let cursor = cursors.map(|c| c.get(&def.id()).cloned().unwrap_or_default());
            let (r, _) = crate::channels::clickable_row(ui, &format!("Options for {label}"), stale, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 22.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    {
                        let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 14.0), egui::Sense::hover());
                        theme::paint_icon(ui.painter(), rect, theme::Icon::Right, p.ink3);
                        let why = match h {
                            Health::Waiting if !def.is_set() => match def {
                                Derived::Turn { .. } => "Type the target angle below.",
                                _ => "Set the plateau below.",
                            },
                            Health::Stale => "Nothing new to show: an input is stale, or the inputs' samples do not line up in time. The value shown is NOT current.",
                            _ => "The least live of its inputs.",
                        };
                        crate::channels::status_word(ui, h).on_hover_text(why);
                    }
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        theme::square(ui, color, 12.0);
                        ui.add(egui::Label::new(theme::b(&label)).truncate()).on_hover_text(&formula);
                    });
                });
                ui.horizontal(|ui| {
                    let (text, on_target) = value_text(&def, value, h.is_live());
                    let mut rt = theme::num(text, 28.0);
                    if !h.is_live() {
                        rt = rt.color(p.ink2);
                        if stale {
                            rt = rt.strikethrough();
                        }
                    } else if on_target {
                        rt = rt.color(p.live);
                    }
                    let how = match def {
                        Derived::Turn { .. } => "The newest sample: a turn is never averaged.",
                        _ => "The mean of the last 150 ms.",
                    };
                    ui.label(rt).on_hover_text(how);
                    if !on_target && !units.is_empty() {
                        ui.label(RichText::new(units).size(18.0).color(p.ink2));
                    }
                    if let (Derived::Sag { plateau_v: Some(pl), .. }, Some(v)) = (&def, value)
                        && *pl != 0.0
                    {
                        ui.label(RichText::new(sag_share(v, *pl)).size(14.0).color(p.ink2));
                    }
                });
                let f = |x: f64| if x.is_finite() { view::fmt(x) } else { "--".into() };
                if let Some(c) = &cursor {
                    crate::channels::cursor_lines(ui, c, p);
                }
                match &def {
                    Derived::Turn { target_deg, .. } => {
                        ui.horizontal(|ui| {
                            ui.label(theme::b("target").size(14.0));
                            let hint = target_deg.map(view::fmt).unwrap_or_else(|| "deg".into());
                            let r = fields::line(ui, &mut self.derived[i].target_text, &format!("Target of {label}"), |t| t.desired_width(80.0).hint_text(hint));
                            if ui.add(egui::Button::new("set").min_size(egui::vec2(0.0, fields::HEIGHT))).clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                                set_target = Some(i);
                            }
                            if let Some(t) = target_deg {
                                ui.label(RichText::new(format!("{} deg", view::fmt(*t))).monospace().size(14.0));
                            }
                        });
                        if let Derived::Turn { angle, .. } = &def {
                            let instance = commutator_instance(angle);
                            let why: &str = match &instance {
                                Err(why) => why,
                                Ok(_) => "Log in to the controller's RWS first (controller menu).",
                            };
                            if ui
                                .add_enabled(rws && instance.is_ok(), egui::Button::new("commutator offset").min_size(egui::vec2(0.0, theme::SMALL_H)))
                                .on_hover_text("Read this motor's Commutator Offset (MOTOR_CALIB com_offset) from the controller as the target: what the resolver reads at the commutation position.")
                                .on_disabled_hover_text(why)
                                .clicked()
                            {
                                from_controller = Some(i);
                            }
                        }
                    }
                    Derived::DutySum { .. } => {
                        ui.label(RichText::new(format!("{:.2} expected   min {}  max {}", derived::DUTY_SUM, f(s.min), f(s.max))).monospace().size(14.0).color(p.ink2));
                    }
                    Derived::Sag { plateau_v, .. } => {
                        ui.horizontal_wrapped(|ui| {
                            match plateau_v {
                                Some(pl) => {
                                    ui.label(RichText::new(format!("plateau {} V   deepest {} V", view::fmt(*pl), f(s.max))).monospace().size(14.0).color(p.ink2));
                                }
                                None => {
                                    ui.label(RichText::new("With the robot armed and still:").size(14.0));
                                }
                            }
                            let text = if plateau_v.is_some() { "set again" } else { "set the plateau" };
                            if ui
                                .add(egui::Button::new(text).min_size(egui::vec2(0.0, theme::SMALL_H)))
                                .on_hover_text("The mean of the DC link's last two seconds, with the robot armed and still. Refused until the link has held that level for 20 s: after the motors go off it drains slowly, for about 20 minutes.")
                                .clicked()
                            {
                                set_plateau = Some(i);
                            }
                        });
                    }
                }
            });
            if r.clicked() {
                open = Some(def.id());
            }
        }
        if let Some(i) = set_target {
            self.set_target(i);
        }
        if let Some(i) = set_plateau {
            self.set_plateau(i);
        }
        if let Some(i) = from_controller {
            self.request_com_offset(i);
        }
        open
    }

    pub fn derived_options(&mut self, ui: &mut egui::Ui, i: usize) {
        let p = theme::pal(ui);
        let def = self.derived[i].live.def().clone();
        let label = self.derived_label(&def);
        let mut back = false;
        let mut remove = false;
        theme::section(ui, "03", "", true, |ui| {
            if theme::icon_text_button(ui, theme::Icon::Left, "all channels", theme::SMALL_H, false).clicked() {
                back = true;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::outline_button(ui, "remove", p.red, theme::SMALL_H).on_hover_text(format!("Remove {label}")).clicked() {
                    remove = true;
                }
            });
        });
        ui.horizontal(|ui| {
            theme::square(ui, self.derived[i].color, 14.0);
            ui.add(egui::Label::new(RichText::new(&label).font(egui::FontId::new(20.0, theme::heavy()))).wrap());
        });
        ui.add(egui::Label::new(RichText::new(self.derived_formula(&def)).size(14.0).color(p.ink2)).wrap());
        ui.label(RichText::new("Computed here from its inputs; never streamed, and never written into a recording as data (a recording keeps how, and a review computes it again).").size(14.0).color(p.ink3));
        ui.add_space(8.0);
        let own = !self.chans.iter().any(|c| c.lane == self.derived[i].lane) && self.derived.iter().filter(|d| d.lane == self.derived[i].lane).count() == 1;
        ui.horizontal(|ui| {
            if ui.add_enabled(!own, egui::Button::new("its own chart").min_size(egui::vec2(0.0, fields::HEIGHT))).clicked() {
                self.derived[i].lane = self.next_lane;
                self.next_lane += 1;
                self.mark_settings_dirty();
            }
            if ui.add(egui::Button::new("reset min/max").min_size(egui::vec2(0.0, fields::HEIGHT))).clicked() {
                let upto = self.derived[i].stats.upto;
                self.derived[i].stats = Stats { upto, ..Stats::default() };
            }
        });
        if back {
            self.options_for = None;
        }
        if remove {
            let d = self.derived.remove(i);
            let text = format!("Removed {}.", self.derived_label(d.live.def()));
            self.options_for = None;
            self.derived_changed(Some(text));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spy_core::catalogue::Catalogue;
    use spy_core::request::{Axis, MechUnit};

    fn key(signal: u32, axis: u8) -> ChannelKey {
        ChannelKey { signal, unit: MechUnit::new("ROB_1").unwrap(), axis: Axis::new(axis).unwrap() }
    }

    #[test]
    fn a_sag_reads_below_or_above_its_plateau() {
        assert_eq!(sag_share(10.0, 356.5), "2.81% below");
        assert_eq!(sag_share(-0.939, 385.601), "0.24% above", "a link above its plateau read \"-0.24% below\"");
        assert_eq!(sag_share(0.0, 385.6), "0.00% below");
    }

    #[test]
    fn each_input_is_offered_what_it_can_feed() {
        let cat = Catalogue::builtin();
        let kinds = |n: u32| offers(cat.get(n), &key(n, 2)).iter().map(Derived::kind_name).collect::<Vec<_>>();
        assert_eq!(kinds(5138), ["Turn to target"], "a resolver angle");
        assert_eq!(kinds(5000), ["Turn to target"]);
        for leg in derived::PWM_LEGS {
            assert_eq!(kinds(leg), ["PWM duty sum"]);
        }
        assert_eq!(kinds(5027), ["DC-link sag"]);
        assert!(kinds(4002).is_empty() && kinds(6000).is_empty() && kinds(5013).is_empty(), "not a torque, a joint angle, or Uq (volts per axis)");
        assert!(offers(None, &key(5138, 1)).is_empty(), "nothing for a signal the catalogue does not know");
    }

    #[test]
    fn the_commutator_offset_is_a_target_only_for_the_resolver_angle() {
        assert_eq!(commutator_instance(&key(5138, 2)).as_deref(), Ok("rob1_2"));
        for n in [5000, 7325, 5028, 5029, 7022] {
            let why = commutator_instance(&key(n, 2)).expect_err("offered for an angle it does not describe");
            assert!(why.contains("5138"), "{n}: {why}");
        }
        let stn = ChannelKey { signal: 5138, unit: MechUnit::new("STN_1").unwrap(), axis: Axis::new(1).unwrap() };
        assert!(commutator_instance(&stn).is_err());
    }

    #[test]
    fn a_derived_channel_in_a_file_says_what_it_was_computed_with() {
        let t = Derived::Turn { angle: key(5138, 1), target_deg: Some(90.0) };
        assert!(file_name(&t).contains("target 90"), "{}", file_name(&t));
        let s = Derived::Sag { link: key(5027, 1), plateau_v: Some(356.5) };
        assert!(file_name(&s).contains("plateau 356.5"), "{}", file_name(&s));
        let d = Derived::duty_sum(&key(5020, 1));
        assert_eq!(file_name(&d), d.name(), "nothing to add for a duty sum");
    }

    #[test]
    fn on_target_only_when_live() {
        let turn = Derived::Turn { angle: key(5138, 1), target_deg: Some(90.0) };
        assert_eq!(value_text(&turn, Some(0.2), true), ("ON TARGET".into(), true));
        assert_eq!(value_text(&turn, Some(-0.25), true), ("ON TARGET".into(), true), "the tolerance either way");
        assert_eq!(value_text(&turn, Some(0.2), false), ("+0.200".into(), false), "a stale turn is a number, never ON TARGET");
        assert_eq!(value_text(&turn, Some(-0.26), true), ("-0.260".into(), false));
        assert_eq!(value_text(&turn, None, true), ("--".into(), false));
        let sag = Derived::Sag { link: key(5027, 1), plateau_v: Some(356.0) };
        assert_eq!(value_text(&sag, Some(0.1), true), ("0.100000".into(), false), "only a turn is ever on target");
    }

    #[test]
    fn a_derived_value_that_falls_behind_its_inputs_is_not_live() {
        assert!(keeps_up(Some(10_000), Some(10_000)));
        assert!(keeps_up(Some(10_000), Some(10_000 - LAG_MS)));
        assert!(!keeps_up(Some(10_000), Some(10_000 - LAG_MS - 1)), "inputs a second on, the value not");
        assert!(!keeps_up(Some(10_000), None), "inputs arriving, nothing computed from them");
        assert!(!keeps_up(None, Some(10_000)));
    }

    #[test]
    fn the_least_live_input_decides() {
        use Health::*;
        assert_eq!(least_live(&[Live, Live, Live]), Live);
        assert_eq!(least_live(&[Live, Stale, Live]), Stale);
        assert_eq!(least_live(&[Stale, Refused]), Refused);
        assert_eq!(least_live(&[Live, NotOnVc]), NotOnVc);
        assert_eq!(least_live(&[]), NotConnected, "no inputs is not live");
    }
}
