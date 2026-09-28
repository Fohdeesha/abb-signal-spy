//! Derived channels (Phase 2; C5 as decided 2026-09-27): values the window computes
//! from streamed channels. They are never streamed, and never written as data: a
//! recording keeps what the controller sent plus these definitions, and a review
//! computes them again.
//!
//! - **Turn to target**: a resolver angle's signed shortest turn to a target, for
//!   turning a resolver onto its mark (what `abb_resolver_web.py` showed).
//! - **PWM duty sum**: the three leg duty ratios of one axis (5020-5022) added up:
//!   1.50 by construction under space-vector modulation (measured 1.50003, sd 0.008).
//! - **DC-link sag**: how far a DC link sits below its charged plateau, which the
//!   person sets (the mean of the last two seconds, robot armed and still).
//!
//! A value exists only where every input has a sample of the same controller tick.
//! Streams in one signal group share their stamps exactly; two groups (the drive
//! module's and the motion signals) stamp the same tick up to 1 ms apart at about 3 %
//! of samples, and a neighbouring tick is always at least 2 ms away (the cell's
//! recording, tunemaster-testsignals.md s25 item 4). So a partner within
//! [`SAME_TICK_MS`] is the same tick, and a missing one is a gap, never filled from a
//! neighbour.

use std::f64::consts::{PI, TAU};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};

use crate::store::{ChannelKey, Ring, Store};

/// Within this of the target (either way) a turn reads ON TARGET: the tolerance
/// `abb_resolver_web.py` used.
pub const ON_TARGET_DEG: f64 = 0.25;
/// A plateau is the mean of this much of the newest history.
pub const PLATEAU_MS: i64 = 2000;
/// A plateau needs at least this much of that stretch to have arrived.
pub const PLATEAU_MIN_MS: i64 = 1500;
/// A plateau below this is not an armed drive's DC link: motors off it read 16 V on
/// the cell, armed 378-397 V (tunemaster-testsignals.md s24), and every IRC5 drive's
/// charged link is hundreds of volts. Set below it, every sag after arming would read
/// about the whole link (the operator's decision, 2026-09-28).
pub const PLATEAU_MIN_V: f64 = 50.0;
/// What the three duty ratios add up to under space-vector modulation.
pub const DUTY_SUM: f64 = 1.5;
/// The three PWM leg duty ratios (knowledge TSV: confirmed; which leg is U, V or W
/// was not determined).
pub const PWM_LEGS: [u32; 3] = [5020, 5021, 5022];
/// Two inputs' samples this close (ms) are the same controller tick.
pub const SAME_TICK_MS: i64 = 1;
/// The resolver angle in the calibration's own frame, `(cal_offset + motor) mod 2pi`
/// (tunemaster-testsignals.md s13.6, s16): the frame a motor's commutator offset is
/// in. 5000 and 7325 read the resolver too, but a fixed per-axis offset away.
pub const RESOLVER_ANGLE: u32 = 5138;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Derived {
    /// `target - angle`, the short way round, in degrees within (-180, 180]: how far
    /// to turn, and which way. Nothing until a target is set.
    Turn {
        angle: ChannelKey,
        #[serde(default)]
        target_deg: Option<f64>,
    },
    /// The three leg duty ratios added up.
    DutySum { legs: [ChannelKey; 3] },
    /// `plateau - voltage`, in volts: positive below the plateau. Nothing until a
    /// plateau is set.
    Sag {
        link: ChannelKey,
        #[serde(default)]
        plateau_v: Option<f64>,
    },
}

/// The signed shortest turn from `angle_rad` to `target_deg`, in degrees within
/// (-180, 180].
pub fn turn_deg(target_deg: f64, angle_rad: f64) -> f64 {
    let d = (target_deg.to_radians() - angle_rad).rem_euclid(TAU);
    (if d > PI { d - TAU } else { d }).to_degrees()
}

impl Derived {
    /// The duty sum of the axis `key` names, whichever leg it is.
    pub fn duty_sum(key: &ChannelKey) -> Derived {
        Derived::DutySum { legs: PWM_LEGS.map(|signal| ChannelKey { signal, unit: key.unit.clone(), axis: key.axis }) }
    }

    /// The streamed channels it is computed from, in the order `value` takes them.
    pub fn inputs(&self) -> Vec<ChannelKey> {
        match self {
            Derived::Turn { angle, .. } => vec![angle.clone()],
            Derived::DutySum { legs } => legs.to_vec(),
            Derived::Sag { link, .. } => vec![link.clone()],
        }
    }

    /// Stable text id, for files: what is derived, from what. The settings (a target,
    /// a plateau) are not part of it.
    pub fn id(&self) -> String {
        match self {
            Derived::Turn { angle, .. } => format!("turn:{}", angle.id()),
            Derived::DutySum { legs } => format!("duty-sum:{}/J{}", legs[0].unit, legs[0].axis.one_based()),
            Derived::Sag { link, .. } => format!("sag:{}", link.id()),
        }
    }

    pub fn kind_name(&self) -> &'static str {
        match self {
            Derived::Turn { .. } => "Turn to target",
            Derived::DutySum { .. } => "PWM duty sum",
            Derived::Sag { .. } => "DC-link sag",
        }
    }

    /// For people, in the form channel labels take: what, then where. A turn names its
    /// angle's signal: several wrapping angles can each have one on the same axis.
    pub fn name(&self) -> String {
        match self {
            Derived::Turn { angle: k, .. } => format!("{}  {} {} J{}", self.kind_name(), k.signal, k.unit, k.axis.one_based()),
            Derived::DutySum { legs: [k, ..] } => format!("{}  {} J{}", self.kind_name(), k.unit, k.axis.one_based()),
            Derived::Sag { link, .. } => format!("{}  {}", self.kind_name(), link.unit),
        }
    }

    pub fn units(&self) -> &'static str {
        match self {
            Derived::Turn { .. } => "deg",
            Derived::DutySum { .. } => "",
            Derived::Sag { .. } => "V",
        }
    }

    /// The same derivation (a changed target or plateau is still the same one).
    pub fn same(&self, other: &Derived) -> bool {
        self.id() == other.id()
    }

    /// A duty sum is of the three PWM legs of one axis, in order (a hand-edited file
    /// could name any three channels); the others have nothing to check.
    pub fn legs_valid(&self) -> bool {
        match self {
            Derived::DutySum { legs } => legs.iter().zip(PWM_LEGS).all(|(k, s)| k.signal == s && k.unit == legs[0].unit && k.axis == legs[0].axis),
            _ => true,
        }
    }

    /// Whether it has what it needs to give values (a target, a plateau).
    pub fn is_set(&self) -> bool {
        match self {
            Derived::Turn { target_deg, .. } => target_deg.is_some_and(f64::is_finite),
            Derived::DutySum { .. } => true,
            Derived::Sag { plateau_v, .. } => plateau_v.is_some_and(f64::is_finite),
        }
    }

    /// The value at one instant from the inputs' values then, in `inputs` order.
    pub fn value(&self, v: &[f64]) -> Option<f64> {
        match self {
            Derived::Turn { target_deg: Some(t), .. } if t.is_finite() => Some(turn_deg(*t, v[0])),
            Derived::DutySum { .. } => Some(v[0] + v[1] + v[2]),
            Derived::Sag { plateau_v: Some(p), .. } if p.is_finite() => Some(p - v[0]),
            _ => None,
        }
    }

    /// The derived series from the inputs' series (each in time order, in `inputs`
    /// order): a value at every instant all of them have a sample for.
    pub fn combine(&self, inputs: &[&[(i64, f64)]]) -> Vec<(i64, f64)> {
        let mut out = Vec::new();
        if self.is_set() && inputs.len() == self.inputs().len() {
            same_ticks(inputs, |t, v| out.extend(self.value(v).map(|x| (t, x))));
        }
        out
    }
}

/// Every controller tick all the series (each in time order) have a sample of: each
/// of the first series' samples, with every other series' closest sample within
/// [`SAME_TICK_MS`] of it. None is a gap, never filled from a neighbour. `f` gets the
/// first series' time and the values, in `inputs` order.
pub fn same_ticks(inputs: &[&[(i64, f64)]], mut f: impl FnMut(i64, &[f64])) {
    let Some((first, rest)) = inputs.split_first() else { return };
    let mut at = vec![0usize; rest.len()];
    let mut vals = vec![0.0; inputs.len()];
    'samples: for &(t, v0) in *first {
        vals[0] = v0;
        for (k, s) in rest.iter().enumerate() {
            while at[k] < s.len() && s[at[k]].0 < t - SAME_TICK_MS {
                at[k] += 1;
            }
            // The same tick: the closest sample within SAME_TICK_MS.
            let near = [at[k], at[k] + 1].into_iter().filter_map(|i| s.get(i).map(|&(tk, vk)| ((tk - t).abs(), vk))).filter(|&(d, _)| d <= SAME_TICK_MS).min_by_key(|&(d, _)| d);
            match near {
                Some((_, vk)) => vals[k + 1] = vk,
                None => continue 'samples,
            }
        }
        f(t, &vals);
    }
}

/// The plateau for a sag: the mean of the newest [`PLATEAU_MS`] of a DC link's
/// history, with its standard deviation. Refused, with the reason, when too little
/// of that stretch has arrived: counted in samples, so a gap inside it is not coverage.
pub fn plateau(ring: &Ring) -> Result<(f64, f64), String> {
    let Some((last, _)) = ring.last() else { return Err("nothing has arrived from the DC link yet".into()) };
    let v: Vec<(i64, f64)> = ring.range(last - PLATEAU_MS + 1, last + 1).filter(|(_, v)| v.is_finite()).collect();
    let gap = ring.gap_ms();
    let covered: i64 = v.windows(2).map(|w| w[1].0 - w[0].0).filter(|&step| step as f64 <= gap).sum();
    if v.len() < 2 || covered < PLATEAU_MIN_MS {
        return Err(format!("only {:.1} s of the DC link has arrived; it needs {:.0} s", covered as f64 / 1000.0, PLATEAU_MS as f64 / 1000.0));
    }
    let n = v.len() as f64;
    let mean = v.iter().map(|x| x.1).sum::<f64>() / n;
    let sd = (v.iter().map(|x| (x.1 - mean).powi(2)).sum::<f64>() / (n - 1.0)).sqrt();
    Ok((mean, sd))
}

/// A derived channel's live history, kept up with its inputs' histories in the
/// store. Only ever as far as the slowest input: a partner still on its way is not
/// taken for a missing one.
#[derive(Debug)]
pub struct Live {
    def: Derived,
    ring: Arc<Mutex<Ring>>,
    /// Newest controller time already combined.
    upto: i64,
    /// The store's epoch the history belongs to.
    epoch: u64,
}

impl Live {
    pub fn new(def: Derived) -> Live {
        Live { def, ring: Arc::new(Mutex::new(Ring::new(4.032))), upto: i64::MIN, epoch: u64::MAX }
    }

    pub fn def(&self) -> &Derived {
        &self.def
    }

    pub fn ring(&self) -> Arc<Mutex<Ring>> {
        self.ring.clone()
    }

    pub fn lock(&self) -> MutexGuard<'_, Ring> {
        self.ring.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A new target or plateau: the whole history is computed again against it.
    pub fn set(&mut self, def: Derived) {
        self.def = def;
        self.lock().clear();
        self.upto = i64::MIN;
    }

    /// Combine whatever the inputs have gained. A history started afresh (another
    /// controller) starts this one afresh too, and a plateau measured on the old
    /// one is dropped: it says nothing about this one.
    pub fn update(&mut self, store: &Store) {
        let epoch = store.epoch();
        if epoch != self.epoch {
            if self.epoch != u64::MAX
                && let Derived::Sag { plateau_v, .. } = &mut self.def
            {
                *plateau_v = None;
            }
            self.epoch = epoch;
            self.lock().clear();
            self.upto = i64::MIN;
        }
        let Some(chans) = self.def.inputs().iter().map(|k| store.get(k)).collect::<Option<Vec<_>>>() else { return };
        let Some(newest) = chans.iter().map(|c| c.lock().last().map(|(t, _)| t)).collect::<Option<Vec<i64>>>().and_then(|v| v.into_iter().min()) else { return };
        if newest <= self.upto {
            return;
        }
        // Every input has reached `newest`, and a stream's samples arrive in order: a
        // partner stamped up to SAME_TICK_MS after its tick's first sample is in (the
        // next tick is 2 ms or more on), even for the first input's sample at `newest`
        // itself, whose partner can be stamped just after it. A partner stamped up to
        // that much before is looked for from that much before.
        let from = self.upto.saturating_add(1);
        let series: Vec<Vec<(i64, f64)>> = chans
            .iter()
            .enumerate()
            .map(|(k, c)| if k == 0 { c.lock().range(from, newest + 1).collect() } else { c.lock().range(from.saturating_sub(SAME_TICK_MS), newest.saturating_add(SAME_TICK_MS + 1)).collect() })
            .collect();
        let sample_ms = chans[0].lock().sample_ms;
        let refs: Vec<&[(i64, f64)]> = series.iter().map(|s| s.as_slice()).collect();
        let out = self.def.combine(&refs);
        let mut ring = self.lock();
        if ring.is_empty() && ring.sample_ms != sample_ms {
            *ring = Ring::new(sample_ms);
        }
        for (t, v) in out {
            ring.push(t, v);
        }
        drop(ring);
        self.upto = newest;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::{Axis, MechUnit};

    fn key(signal: u32, axis: u8) -> ChannelKey {
        ChannelKey { signal, unit: MechUnit::new("ROB_1").unwrap(), axis: Axis::new(axis).unwrap() }
    }

    #[test]
    fn a_turn_is_the_short_way_round() {
        let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
        assert!(close(turn_deg(90.0, 80f64.to_radians()), 10.0));
        assert!(close(turn_deg(90.0, 100f64.to_radians()), -10.0));
        // Across the 0/360 jump: from 359 to 1 is +2, from 1 to 359 is -2.
        assert!(close(turn_deg(1.0, 359f64.to_radians()), 2.0));
        assert!(close(turn_deg(359.0, 1f64.to_radians()), -2.0));
        // Never more than half a turn either way.
        for a in 0..720 {
            let t = turn_deg(37.0, (a as f64 * 0.5).to_radians());
            assert!(t > -180.0 - 1e-9 && t <= 180.0 + 1e-9, "{t}");
        }
        assert!(turn_deg(10.0, f64::NAN).is_nan());
    }

    #[test]
    fn values_only_where_every_input_has_one() {
        let d = Derived::duty_sum(&key(5021, 2));
        assert_eq!(d.inputs(), vec![key(5020, 2), key(5021, 2), key(5022, 2)]);
        assert_eq!(d.id(), "duty-sum:ROB_1/J2");
        let u = [(0, 0.5), (4, 0.6), (8, 0.7), (12, 0.4)];
        let v = [(0, 0.5), (8, 0.5), (12, 0.6)];
        let w = [(0, 0.5), (4, 0.4), (8, 0.3), (12, 0.5), (16, 0.5)];
        // 4 has no V sample: a gap, not a sum of two.
        assert_eq!(d.combine(&[&u, &v, &w]), vec![(0, 1.5), (8, 1.5), (12, 1.5)]);
        // Another signal group stamps the same tick up to 1 ms off (s25 item 4): the
        // same tick. 2 ms off is not.
        let x = [(0, 0.5), (4, 0.5), (8, 0.5)];
        let y = [(1, 0.5), (3, 0.6), (10, 0.5)];
        let z = [(0, 0.5), (5, 0.5), (8, 0.5)];
        let sums = d.combine(&[&x, &y, &z]);
        assert_eq!(sums.iter().map(|&(t, v)| (t, (v * 1e9).round() / 1e9)).collect::<Vec<_>>(), vec![(0, 1.5), (4, 1.6)]);
        // The closest, where two are near.
        let y = [(0, 0.5), (4, 0.6), (5, 0.9), (8, 0.5)];
        assert_eq!(d.combine(&[&x, &y, &x])[1], (4, 1.6));
        assert!(d.combine(&[&u, &v]).is_empty(), "the wrong number of inputs gives nothing");

        let s = Derived::Sag { link: key(5027, 1), plateau_v: None };
        assert!(s.combine(&[&[(0, 350.0)]]).is_empty(), "no plateau, no sag");
        let s = Derived::Sag { link: key(5027, 1), plateau_v: Some(356.5) };
        assert_eq!(s.combine(&[&[(0, 350.0), (4, 357.0)]]), vec![(0, 6.5), (4, -0.5)]);
        let t = Derived::Turn { angle: key(5138, 2), target_deg: None };
        assert!(!t.is_set() && t.combine(&[&[(0, 1.0)]]).is_empty());
    }

    #[test]
    fn a_definition_reads_back_from_its_file_form() {
        for d in [Derived::Turn { angle: key(5138, 2), target_deg: Some(90.0) }, Derived::duty_sum(&key(5020, 6)), Derived::Sag { link: key(5027, 1), plateau_v: None }] {
            let text = serde_json::to_string(&d).unwrap();
            assert_eq!(serde_json::from_str::<Derived>(&text).unwrap(), d, "{text}");
        }
        let text = serde_json::to_string(&Derived::Sag { link: key(5027, 1), plateau_v: Some(356.5) }).unwrap();
        assert_eq!(text, r#"{"kind":"sag","link":{"signal":5027,"unit":"ROB_1","axis":1},"plateau_v":356.5}"#);
        // A bad axis in a hand-edited file is refused, not read as another joint.
        assert!(serde_json::from_str::<Derived>(r#"{"kind":"sag","link":{"signal":5027,"unit":"ROB_1","axis":9}}"#).is_err());
    }

    #[test]
    fn a_plateau_needs_two_seconds() {
        let mut r = Ring::new(4.0);
        assert!(plateau(&r).is_err());
        for i in 0..300 {
            r.push(i * 4, 356.0 + if i % 2 == 0 { 0.5 } else { -0.5 });
        }
        assert!(plateau(&r).unwrap_err().contains("1.2 s"), "{:?}", plateau(&r));
        // A dip until 1.8 s, then steady to 4 s: the plateau is the steady part.
        for i in 300..1000 {
            r.push(i * 4, if i < 450 { 300.0 } else { 356.0 + if i % 2 == 0 { 0.5 } else { -0.5 } });
        }
        let (mean, sd) = plateau(&r).unwrap();
        assert!((mean - 356.0).abs() < 1e-9, "the newest two seconds only: {mean}");
        assert!((sd - 0.5).abs() < 0.01, "{sd}");
    }

    fn fill(store: &Store, k: &ChannelKey, samples: &[(i64, f64)]) {
        let c = store.channel(k, 4.0);
        let mut r = c.lock();
        for &(t, v) in samples {
            r.push(t, v);
        }
    }

    #[test]
    fn the_live_history_waits_for_the_slowest_input() {
        let store = Store::new();
        let d = Derived::duty_sum(&key(5020, 1));
        let mut live = Live::new(d);
        live.update(&store);
        assert!(live.lock().is_empty(), "no inputs yet");
        let [a, b, c] = PWM_LEGS.map(|s| key(s, 1));
        let all = |live: &Live| live.lock().range(i64::MIN, i64::MAX).map(|(t, v)| (t, (v * 1e9).round() / 1e9)).collect::<Vec<_>>();
        fill(&store, &a, &[(0, 0.5), (4, 0.5), (8, 0.5)]);
        fill(&store, &b, &[(0, 0.5), (4, 0.5), (8, 0.5)]);
        // The third leg is another signal group's: its ticks stamped 1 ms later.
        fill(&store, &c, &[(1, 0.5)]);
        live.update(&store);
        assert_eq!(all(&live), vec![(0, 1.5)]);
        // Its 5 and 9 arrive after the others' 4 and 8: 4 is summed once 5 is in, and 8
        // waits for 9.
        fill(&store, &c, &[(5, 0.4)]);
        live.update(&store);
        assert_eq!(all(&live), vec![(0, 1.5), (4, 1.4)]);
        fill(&store, &c, &[(9, 0.6)]);
        fill(&store, &a, &[(12, 0.5)]);
        fill(&store, &b, &[(12, 0.5)]);
        live.update(&store);
        assert_eq!(all(&live), vec![(0, 1.5), (4, 1.4), (8, 1.6)]);
        live.update(&store);
        assert_eq!(live.lock().len(), 3, "nothing twice");

        // A group stamped 1 ms earlier: the partner of the first leg's 20 is 19, which
        // came with an update before 20 was combined (that waits until every input
        // has reached 20).
        let store = Store::new();
        let mut live = Live::new(Derived::duty_sum(&key(5020, 1)));
        fill(&store, &a, &[(16, 0.5), (20, 0.4)]);
        fill(&store, &b, &[(16, 0.5), (20, 0.4)]);
        fill(&store, &c, &[(15, 0.5), (19, 0.5)]);
        live.update(&store);
        assert_eq!(all(&live), vec![(16, 1.5)]);
        fill(&store, &c, &[(23, 0.5)]);
        live.update(&store);
        assert_eq!(all(&live), vec![(16, 1.5), (20, 1.3)]);
    }

    #[test]
    fn the_live_history_joins_a_partner_stamped_just_after_the_newest() {
        // The first input the earlier-stamping group: its newest sample's partner,
        // stamped 1 ms later, is already in. Not left out, and not lost for good.
        let store = Store::new();
        let [a, b, c] = PWM_LEGS.map(|s| key(s, 1));
        let mut live = Live::new(Derived::duty_sum(&a));
        fill(&store, &a, &[(0, 0.5)]);
        fill(&store, &b, &[(0, 0.5)]);
        fill(&store, &c, &[(1, 0.5)]);
        live.update(&store);
        assert_eq!(live.lock().range(i64::MIN, i64::MAX).collect::<Vec<_>>(), vec![(0, 1.5)]);
        fill(&store, &a, &[(4, 0.5)]);
        fill(&store, &b, &[(4, 0.5)]);
        fill(&store, &c, &[(5, 0.5)]);
        live.update(&store);
        assert_eq!(live.lock().range(i64::MIN, i64::MAX).map(|(t, _)| t).collect::<Vec<_>>(), vec![0, 4]);
    }

    #[test]
    fn a_plateau_needs_two_seconds_of_samples_not_of_span() {
        // Samples from 1.6 s to 2.0 s, a gap, then 3.2 s to 3.6 s: the newest two
        // seconds run from first sample to last, but only 0.8 s of them has samples.
        let mut r = Ring::new(4.0);
        for i in 0..=100 {
            r.push(1600 + i * 4, 356.0);
        }
        for i in 0..=100 {
            r.push(3200 + i * 4, 356.0);
        }
        assert!(plateau(&r).is_err(), "0.8 s of samples in a 2 s stretch: {:?}", plateau(&r));
    }

    #[test]
    fn a_turn_names_its_angle() {
        let a = Derived::Turn { angle: key(5138, 1), target_deg: None };
        let b = Derived::Turn { angle: key(5000, 1), target_deg: None };
        assert_ne!(a.name(), b.name(), "two turns on one axis read the same everywhere");
        assert!(a.name().contains("5138"), "{}", a.name());
    }

    #[test]
    fn a_duty_sum_is_of_the_three_legs_of_one_axis() {
        assert!(Derived::duty_sum(&key(5021, 3)).legs_valid());
        let mixed = Derived::DutySum { legs: [key(5020, 1), key(5021, 2), key(5022, 1)] };
        let wrong = Derived::DutySum { legs: [key(5020, 1), key(4002, 1), key(5022, 1)] };
        assert!(!mixed.legs_valid() && !wrong.legs_valid(), "any three channels summed and called a duty sum");
        assert!(Derived::Turn { angle: key(5138, 1), target_deg: None }.legs_valid());
    }

    #[test]
    fn a_new_setting_or_controller_starts_the_history_again() {
        let store = Store::new();
        let k = key(5027, 1);
        fill(&store, &k, &[(0, 350.0), (4, 352.0)]);
        let mut live = Live::new(Derived::Sag { link: k.clone(), plateau_v: Some(356.0) });
        live.update(&store);
        assert_eq!(live.lock().range(i64::MIN, i64::MAX).collect::<Vec<_>>(), vec![(0, 6.0), (4, 4.0)]);
        live.set(Derived::Sag { link: k.clone(), plateau_v: Some(355.0) });
        live.update(&store);
        assert_eq!(live.lock().range(i64::MIN, i64::MAX).collect::<Vec<_>>(), vec![(0, 5.0), (4, 3.0)], "the whole history against the new plateau");
        // Another controller: its DC link is not measured against this one's plateau.
        store.clear();
        fill(&store, &k, &[(100, 340.0)]);
        live.update(&store);
        assert!(live.lock().is_empty());
        assert_eq!(live.def(), &Derived::Sag { link: k, plateau_v: None });
    }
}
