//! Derived channels in the window (Phase 2): the turn to a target, the PWM duty sum
//! and the DC-link sag (`spy_core::derived`). Each is computed from channels in the
//! list, its inputs, and is never shown as more live than the least live of them.
//! Offered from an input's card menu; adding one adds any input it lacks.

use eframe::egui::{self, Color32, RichText};

use spy_core::catalogue::{flag, Select, Signal};
use spy_core::derived::{self, Derived, Live};
use spy_core::log::Level;
use spy_core::session::{Status, MAX_CHANNELS};
use spy_core::store::ChannelKey;

use crate::app::{chan_color, SpyApp, Stats};
use crate::channels::health_color;
use crate::theme;
use crate::view::{self, Health};

/// How far a derived value may trail its inputs' newest samples and still be live:
/// inputs whose samples do not line up in time give nothing to compute.
pub const LAG_MS: i64 = 1000;
/// A plateau whose two seconds spread more than this (standard deviation over mean)
/// was not taken with the DC link steady.
pub const PLATEAU_STEADY: f64 = 0.01;

pub struct DerivedView {
    pub live: Live,
    pub color: Color32,
    pub lane: u32,
    /// Since the last reset (or setting): the extremes of a sag or a duty sum.
    pub stats: Stats,
    /// A turn's target as it is being typed.
    pub target_text: String,
}

/// What a channel of this signal can be an input of.
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

/// The card menu's entry for offering it.
pub fn offer_text(d: &Derived) -> &'static str {
    match d {
        Derived::Turn { .. } => "Turn to a target...",
        Derived::DutySum { .. } => "PWM duty sum of this axis",
        Derived::Sag { .. } => "Sag below a plateau",
    }
}

/// How a derived channel's samples are read.
pub fn reading(d: &Derived) -> view::Reading {
    match d {
        Derived::Turn { .. } => view::Reading::Turn,
        _ => view::Reading::Plain,
    }
}

/// A derived value as the card and the phone show it, and whether that is ON
/// TARGET. Only ever for a live value: a stale one is shown as its number.
pub fn value_text(def: &Derived, v: Option<f64>, live: bool) -> (String, bool) {
    match (def, v) {
        (Derived::Turn { .. }, Some(x)) if live && x.abs() <= derived::ON_TARGET_DEG => ("ON TARGET".into(), true),
        (Derived::Turn { .. }, Some(x)) => (format!("{x:+.3}"), false),
        (_, Some(x)) => (view::fmt(x), false),
        (_, None) => ("--".into(), false),
    }
}

/// Whether a derived channel's newest value keeps up with the newest instant all its
/// inputs have reached. Inputs that each look live but never share a stamp give
/// nothing to compute, and the last value computed is then not current.
pub fn keeps_up(inputs_newest: Option<i64>, newest: Option<i64>) -> bool {
    matches!((inputs_newest, newest), (Some(a), Some(b)) if a - b <= LAG_MS)
}

/// The least live of some channels' states.
pub fn least_live(hs: &[Health]) -> Health {
    const ORDER: [Health; 8] = [Health::NotConnected, Health::Refused, Health::NotOnVc, Health::NoReply, Health::Waiting, Health::NoEventYet, Health::Stale, Health::Live];
    ORDER.iter().copied().find(|h| hs.contains(h)).unwrap_or(Health::NotConnected)
}

impl SpyApp {
    pub fn derived_label(&self, d: &Derived) -> String {
        d.name()
    }

    /// What it is computed from, for the hover and for files.
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

    /// Add a derived channel, and whichever of its inputs is not in the list yet.
    /// False (and a message) when it is there already or its inputs do not fit.
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
        let color = chan_color(self.chans.len() + self.derived.len());
        self.derived.push(DerivedView { live: Live::new(def), color, lane, stats: Stats::default(), target_text: String::new() });
        self.derived_changed(Some(format!("Added {label}.")));
        true
    }

    /// Settings or recordings follow a change of the derived channels; `what`, when
    /// given, goes into running recordings' events.
    pub fn derived_changed(&mut self, what: Option<String>) {
        let defs: Vec<Derived> = self.derived.iter().map(|d| d.live.def().clone()).collect();
        for r in [&self.recorder, &self.slow].into_iter().flatten() {
            r.derived(defs.clone(), what.clone());
        }
        if let Some(w) = what {
            self.log.info(w);
        }
        self.mark_settings_dirty();
    }

    /// Drop the derived channels whose inputs have gone from the list.
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

    /// Each frame: bring every derived history up to date, and its statistics.
    pub fn update_derived(&mut self) {
        let store = self.session.store().clone();
        let mut dropped = Vec::new();
        for d in &mut self.derived {
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
            self.derived_changed(Some(text));
        }
    }

    /// A derived channel's one status word: the least live of its inputs', WAITING
    /// until it has a target or plateau, and STALE when its values stop keeping up
    /// with its inputs (their samples do not line up in time).
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

    /// Set a turn's target from its text field.
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
        self.derived[i].live.set(def);
        self.derived[i].stats = Stats::default();
        self.derived_changed(Some(format!("{label}: target set to {} deg.", view::fmt(t))));
    }

    /// Set a sag's plateau: the mean of the DC link's last two seconds, refused while
    /// the link is not live or not steady.
    pub fn set_plateau(&mut self, i: usize) {
        let Derived::Sag { link, .. } = self.derived[i].live.def().clone() else { return };
        let st = self.session.status().clone();
        let h = view::health(st.channels.iter().find(|c| c.key == link), view::session_live(&st.phase), self.catalogue.get(link.signal), st.loopback);
        if !h.is_live() {
            self.toast(Level::Error, format!("The DC link is not live ({}): a plateau needs its last two seconds.", h.word()));
            return;
        }
        let got = match self.session.store().get(&link) {
            Some(c) => derived::plateau(&c.lock()),
            None => Err("nothing has arrived from the DC link yet".into()),
        };
        let (mean, sd) = match got {
            Ok(x) => x,
            Err(e) => return self.toast(Level::Error, format!("No plateau: {e}.")),
        };
        if sd > mean.abs() * PLATEAU_STEADY {
            self.toast(Level::Error, format!("No plateau: the DC link was not steady (mean {} V, standard deviation {} V over the last two seconds). Set it with the robot armed and still.", view::fmt(mean), view::fmt(sd)));
            return;
        }
        let def = Derived::Sag { link, plateau_v: Some(mean) };
        let label = self.derived_label(&def);
        self.derived[i].live.set(def);
        self.derived[i].stats = Stats::default();
        let text = format!("{label}: plateau set to {} V (the mean of the last two seconds; standard deviation {} V).", view::fmt(mean), view::fmt(sd));
        self.toast(Level::Info, text.clone());
        self.derived_changed(Some(text));
    }

    /// The derived channels' cards, below the channels'.
    pub fn derived_cards(&mut self, ui: &mut egui::Ui, st: &Status) {
        if self.derived.is_empty() {
            return;
        }
        ui.add_space(4.0);
        ui.label(RichText::new("Derived").strong()).on_hover_text("Computed here from the channels above; never streamed, and never written into a recording as data (a recording keeps how, and a review computes them again).");
        let mut remove = None;
        let mut set_target = None;
        let mut set_plateau = None;
        let mut from_controller = None;
        let rws = self.rws_ready();
        let mut changed = false;
        for i in 0..self.derived.len() {
            let h = self.derived_health(i, st);
            let def = self.derived[i].live.def().clone();
            let label = self.derived_label(&def);
            let formula = self.derived_formula(&def);
            let value = view::readout(&self.derived[i].live.lock(), reading(&def));
            let color = self.derived[i].color;
            let units = def.units();
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 14.0), egui::Sense::hover());
                    ui.painter().rect_filled(rect, 2.0, color);
                    ui.label(RichText::new(&label).strong()).on_hover_text(&formula);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.menu_button("⋯", |ui| {
                            if ui.button("Remove").clicked() {
                                remove = Some(i);
                                ui.close();
                            }
                            if ui.button("Reset min/max").clicked() {
                                let upto = self.derived[i].stats.upto;
                                self.derived[i].stats = Stats { upto, ..Stats::default() };
                                ui.close();
                            }
                            if ui.button("Own chart").clicked() {
                                self.derived[i].lane = self.next_lane;
                                self.next_lane += 1;
                                changed = true;
                                ui.close();
                            }
                        });
                        let why = match h {
                            Health::Waiting if !def.is_set() => match def {
                                Derived::Turn { .. } => "Type the target angle below.",
                                _ => "Set the plateau below.",
                            },
                            Health::Stale => "Nothing new to show: an input is stale, or the inputs' samples do not line up in time. The value shown is NOT current.",
                            _ => "The least live of its inputs.",
                        };
                        ui.label(RichText::new(h.word()).small().strong().color(health_color(h))).on_hover_text(why);
                    });
                });
                // The value line.
                ui.horizontal(|ui| {
                    let (text, on_target) = value_text(&def, value, h.is_live());
                    let mut rt = RichText::new(text).monospace().size(20.0);
                    if !h.is_live() {
                        rt = rt.weak();
                    } else if on_target {
                        rt = rt.color(theme::OK).strong();
                    }
                    let how = match def {
                        Derived::Turn { .. } => "The newest sample: a turn is never averaged.",
                        _ => "The mean of the last 150 ms.",
                    };
                    ui.label(rt).on_hover_text(how);
                    if !on_target && !units.is_empty() {
                        ui.label(RichText::new(units).weak());
                    }
                    if let (Derived::Sag { plateau_v: Some(p), .. }, Some(v)) = (&def, value)
                        && *p != 0.0
                    {
                        ui.label(RichText::new(format!("{:.2}% below", 100.0 * v / p)).small().weak());
                    }
                });
                let s = self.derived[i].stats;
                let f = |x: f64| if x.is_finite() { view::fmt(x) } else { "--".into() };
                match &def {
                    Derived::Turn { target_deg, .. } => {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("target").small());
                            let r = ui.add(egui::TextEdit::singleline(&mut self.derived[i].target_text).desired_width(70.0).hint_text(target_deg.map(view::fmt).unwrap_or_else(|| "deg".into())));
                            if ui.small_button("Set").clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                                set_target = Some(i);
                            }
                            if let Some(t) = target_deg {
                                ui.label(RichText::new(format!("{} deg", view::fmt(*t))).small().monospace());
                            }
                        });
                        if let Derived::Turn { angle, .. } = &def {
                            let named = spy_core::rws::calib_instance(angle.unit.as_str(), angle.axis.one_based()).is_some();
                            let why = if !rws { "Log in to the controller's RWS first (Controller menu)." } else { "Only a robot's axes (ROB_1, ROB_2, ...) have their calibration named this way." };
                            if ui
                                .add_enabled(rws && named, egui::Button::new("Commutator offset").small())
                                .on_hover_text("Read this motor's Commutator Offset (MOTOR_CALIB com_offset) from the controller as the target: what the resolver reads at the commutation position.")
                                .on_disabled_hover_text(why)
                                .clicked()
                            {
                                from_controller = Some(i);
                            }
                        }
                    }
                    Derived::DutySum { .. } => {
                        ui.label(RichText::new(format!("{:.2} expected   min {}  max {}", derived::DUTY_SUM, f(s.min), f(s.max))).small().monospace());
                    }
                    Derived::Sag { plateau_v, .. } => {
                        ui.horizontal_wrapped(|ui| {
                            match plateau_v {
                                Some(p) => {
                                    ui.label(RichText::new(format!("plateau {} V   deepest {} V", view::fmt(*p), f(s.max))).small().monospace());
                                }
                                None => {
                                    ui.label(RichText::new("With the robot armed and still:").small());
                                }
                            }
                            let text = if plateau_v.is_some() { "Set again" } else { "Set the plateau" };
                            if ui.small_button(text).on_hover_text("The mean of the DC link's last two seconds").clicked() {
                                set_plateau = Some(i);
                            }
                        });
                    }
                }
            });
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
        if let Some(i) = remove {
            let d = self.derived.remove(i);
            let text = format!("Removed {}.", self.derived_label(d.live.def()));
            self.derived_changed(Some(text));
        }
        if changed {
            self.mark_settings_dirty();
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
