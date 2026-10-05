use std::f64::consts::{PI, TAU};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};

use crate::store::{ChannelKey, Ring, Store};

pub const ON_TARGET_DEG: f64 = 0.25;
pub const PLATEAU_MS: i64 = 2000;
pub const PLATEAU_MIN_MS: i64 = 1500;
pub const PLATEAU_MIN_V: f64 = 50.0;
pub const PLATEAU_TREND_MS: i64 = 20_000;
pub const PLATEAU_TREND_MAX: f64 = 0.02;
pub const DUTY_SUM: f64 = 1.5;
pub const PWM_LEGS: [u32; 3] = [5020, 5021, 5022];
pub const SAME_TICK_MS: i64 = 1;
pub const RESOLVER_ANGLE: u32 = 5138;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Derived {
    Turn {
        angle: ChannelKey,
        #[serde(default)]
        target_deg: Option<f64>,
    },
    DutySum { legs: [ChannelKey; 3] },
    Sag {
        link: ChannelKey,
        #[serde(default)]
        plateau_v: Option<f64>,
    },
}

pub fn turn_deg(target_deg: f64, angle_rad: f64) -> f64 {
    let d = (target_deg.to_radians() - angle_rad).rem_euclid(TAU);
    (if d > PI { d - TAU } else { d }).to_degrees()
}

impl Derived {
    pub fn duty_sum(key: &ChannelKey) -> Derived {
        Derived::DutySum { legs: PWM_LEGS.map(|signal| ChannelKey { signal, unit: key.unit.clone(), axis: key.axis }) }
    }

    pub fn inputs(&self) -> Vec<ChannelKey> {
        match self {
            Derived::Turn { angle, .. } => vec![angle.clone()],
            Derived::DutySum { legs } => legs.to_vec(),
            Derived::Sag { link, .. } => vec![link.clone()],
        }
    }

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

    pub fn same(&self, other: &Derived) -> bool {
        self.id() == other.id()
    }

    pub fn legs_valid(&self) -> bool {
        match self {
            Derived::DutySum { legs } => legs.iter().zip(PWM_LEGS).all(|(k, s)| k.signal == s && k.unit == legs[0].unit && k.axis == legs[0].axis),
            _ => true,
        }
    }

    pub fn is_set(&self) -> bool {
        match self {
            Derived::Turn { target_deg, .. } => target_deg.is_some_and(f64::is_finite),
            Derived::DutySum { .. } => true,
            Derived::Sag { plateau_v, .. } => plateau_v.is_some_and(f64::is_finite),
        }
    }

    pub fn value(&self, v: &[f64]) -> Option<f64> {
        match self {
            Derived::Turn { target_deg: Some(t), .. } if t.is_finite() => Some(turn_deg(*t, v[0])),
            Derived::DutySum { .. } => Some(v[0] + v[1] + v[2]),
            Derived::Sag { plateau_v: Some(p), .. } if p.is_finite() => Some(p - v[0]),
            _ => None,
        }
    }

    pub fn combine(&self, inputs: &[&[(i64, f64)]]) -> Vec<(i64, f64)> {
        let mut out = Vec::new();
        if self.is_set() && inputs.len() == self.inputs().len() {
            same_ticks(inputs, |t, v| out.extend(self.value(v).map(|x| (t, x))));
        }
        out
    }
}

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
            let near = [at[k], at[k] + 1].into_iter().filter_map(|i| s.get(i).map(|&(tk, vk)| ((tk - t).abs(), vk))).filter(|&(d, _)| d <= SAME_TICK_MS).min_by_key(|&(d, _)| d);
            match near {
                Some((_, vk)) => vals[k + 1] = vk,
                None => continue 'samples,
            }
        }
        f(t, &vals);
    }
}

pub fn plateau(ring: &Ring) -> Result<(f64, f64), String> {
    let Some((last, _)) = ring.last() else { return Err("nothing has arrived from the DC link yet".into()) };
    stretch(ring, last).ok_or_else(|| format!("only {:.1} s of the DC link has arrived; it needs {:.0} s", covered(ring, last) as f64 / 1000.0, PLATEAU_MS as f64 / 1000.0))
}

pub fn level_holds(before: f64, now: f64) -> bool {
    ((now - before) / now).abs() <= PLATEAU_TREND_MAX
}

pub fn level_before(ring: &Ring, span_ms: i64) -> Result<f64, String> {
    let Some((last, _)) = ring.last() else { return Err("nothing has arrived from the DC link yet".into()) };
    match stretch(ring, last - span_ms) {
        Some((mean, _)) => Ok(mean),
        None => {
            let charted = ring.first_t().map_or(0, |first| last - first);
            let need = (span_ms + PLATEAU_MS) as f64 / 1000.0;
            Err(if (charted as f64) < need * 1000.0 {
                format!("the DC link has been charted for only {:.0} s, and a plateau needs its last {need:.0} s, to tell an armed drive from one still draining after the motors went off", charted as f64 / 1000.0)
            } else {
                format!("the DC link's history {:.0} s ago has a gap, and a plateau compares with it, to tell an armed drive from one still draining after the motors went off", span_ms as f64 / 1000.0)
            })
        }
    }
}

fn stretch(ring: &Ring, end: i64) -> Option<(f64, f64)> {
    let v: Vec<(i64, f64)> = ring.range(end - PLATEAU_MS + 1, end + 1).filter(|(_, v)| v.is_finite()).collect();
    if v.len() < 2 || covered(ring, end) < PLATEAU_MIN_MS {
        return None;
    }
    let n = v.len() as f64;
    let mean = v.iter().map(|x| x.1).sum::<f64>() / n;
    let sd = (v.iter().map(|x| (x.1 - mean).powi(2)).sum::<f64>() / (n - 1.0)).sqrt();
    Some((mean, sd))
}

fn covered(ring: &Ring, end: i64) -> i64 {
    let gap = ring.gap_ms();
    let t: Vec<i64> = ring.range(end - PLATEAU_MS + 1, end + 1).filter(|(_, v)| v.is_finite()).map(|(t, _)| t).collect();
    t.windows(2).map(|w| w[1] - w[0]).filter(|&step| step as f64 <= gap).sum()
}

#[derive(Debug)]
pub struct Live {
    def: Derived,
    ring: Arc<Mutex<Ring>>,
    upto: i64,
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

    pub fn set(&mut self, def: Derived) {
        self.def = def;
        self.lock().clear();
        self.upto = i64::MIN;
    }

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
        assert!(close(turn_deg(1.0, 359f64.to_radians()), 2.0));
        assert!(close(turn_deg(359.0, 1f64.to_radians()), -2.0));
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
        assert_eq!(d.combine(&[&u, &v, &w]), vec![(0, 1.5), (8, 1.5), (12, 1.5)]);
        let x = [(0, 0.5), (4, 0.5), (8, 0.5)];
        let y = [(1, 0.5), (3, 0.6), (10, 0.5)];
        let z = [(0, 0.5), (5, 0.5), (8, 0.5)];
        let sums = d.combine(&[&x, &y, &z]);
        assert_eq!(sums.iter().map(|&(t, v)| (t, (v * 1e9).round() / 1e9)).collect::<Vec<_>>(), vec![(0, 1.5), (4, 1.6)]);
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
        fill(&store, &c, &[(1, 0.5)]);
        live.update(&store);
        assert_eq!(all(&live), vec![(0, 1.5)]);
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

    fn link(v: impl Fn(f64) -> f64) -> Ring {
        let mut r = Ring::new(4.0);
        for i in 0..6250 {
            r.push(i * 4, v(i as f64 * 0.004));
        }
        r
    }

    #[test]
    fn a_plateau_compares_its_level_with_twenty_seconds_before() {
        let held = |r: &Ring| level_holds(level_before(r, PLATEAU_TREND_MS).unwrap(), plateau(r).unwrap().0);
        let armed = link(|s| 386.0 * (1.0 + 0.0071 * s / 20.0) + 3.0 * (s * 2.0 * PI * 3.0).sin());
        assert!(held(&armed), "an armed link refused");
        assert!(plateau(&armed).unwrap().1 > 1.0, "the ripple is there");
        let draining = link(|s| 338.0 * (-s / 385.0).exp());
        assert!(plateau(&draining).unwrap().1 < plateau(&armed).unwrap().1, "the drain's two seconds are the steadier");
        assert!(!held(&draining), "a draining link taken for an armed one");
        let charging = link(|s| if s < 8.0 { 16.0 } else { 386.0 });
        assert!(!held(&charging));
    }

    #[test]
    fn a_plateau_needs_the_links_history_twenty_seconds_back() {
        let mut r = Ring::new(4.0);
        for i in 0..2500 {
            r.push(i * 4, 386.0);
        }
        assert!(plateau(&r).is_ok(), "ten seconds: enough for the mean");
        let e = level_before(&r, PLATEAU_TREND_MS).unwrap_err();
        assert!(e.contains("charted for only 10 s") && e.contains("22 s"), "{e}");
        let mut r = Ring::new(4.0);
        for i in (0..6250).filter(|i| !(1000..2000).contains(i)) {
            r.push(i * 4, 386.0);
        }
        let e = level_before(&r, PLATEAU_TREND_MS).unwrap_err();
        assert!(e.contains("has a gap"), "{e}");
        assert_eq!(level_before(&link(|_| 386.0), PLATEAU_TREND_MS), Ok(386.0));
    }

    #[test]
    fn a_plateau_needs_two_seconds_of_samples_not_of_span() {
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
        store.clear();
        fill(&store, &k, &[(100, 340.0)]);
        live.update(&store);
        assert!(live.lock().is_empty());
        assert_eq!(live.def(), &Derived::Sag { link: k, plateau_v: None });
    }
}
