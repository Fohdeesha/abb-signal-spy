use spy_core::catalogue::{self, Catalogue, Select, Signal};
use spy_core::session::{ChannelState, ChannelStatus};
use spy_core::store::{ChannelKey, Ring};

#[derive(Debug, Clone, PartialEq)]
pub struct Display {
    pub units: String,
    pub factor: f64,
    pub wraps: bool,
}

pub fn display(sig: Option<&Signal>, radians: bool) -> Display {
    let Some(s) = sig else { return Display { units: String::new(), factor: 1.0, wraps: false } };
    let wraps = s.has(catalogue::flag::WRAPPING);
    match catalogue::angle_unit(&s.units) {
        Some((deg, k)) if !radians => Display { units: deg.to_string(), factor: k, wraps },
        _ => Display { units: s.units.clone(), factor: 1.0, wraps },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reading {
    Plain,
    ZeroFilled,
    Wrapping,
    Turn,
}

pub fn reading(sig: Option<&Signal>) -> Reading {
    match sig {
        Some(s) if s.has(catalogue::flag::ZERO_FILLED) => Reading::ZeroFilled,
        Some(s) if s.has(catalogue::flag::WRAPPING) && catalogue::angle_unit(&s.units).is_some() => Reading::Wrapping,
        _ => Reading::Plain,
    }
}

pub use spy_core::reading::{ZeroHold, ZERO_HOLD_MS};

pub const READOUT_MS: i64 = 150;

const STEADY_ANGLE: f64 = 0.99;

fn read_window(ring: &Ring, r: Reading, from: i64, to: i64) -> Vec<f64> {
    match r {
        Reading::ZeroFilled => {
            let mut hold = ZeroHold::new();
            let prime = from - ZERO_HOLD_MS.ceil() as i64 - 1;
            ring.range(prime, to).filter_map(|(t, v)| {
                let x = hold.apply_at(t, v);
                (t >= from).then_some(x)
            }).collect()
        }
        _ => ring.range(from, to).map(|(_, v)| v).collect(),
    }
}

pub fn readout(ring: &Ring, r: Reading) -> Option<f64> {
    readout_ms(ring, r, READOUT_MS)
}

pub fn readout_window(smooth_ms: u32) -> i64 {
    READOUT_MS.max(i64::from(smooth_ms))
}

pub fn readout_ms(ring: &Ring, r: Reading, window_ms: i64) -> Option<f64> {
    let (last_t, newest) = ring.last()?;
    if r == Reading::Turn {
        return newest.is_finite().then_some(newest);
    }
    let v: Vec<f64> = read_window(ring, r, last_t - window_ms, last_t + 1).into_iter().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return None;
    }
    if r == Reading::Wrapping {
        let (s, c) = v.iter().fold((0.0, 0.0), |(s, c), a| (s + a.sin(), c + a.cos()));
        let spread = (s * s + c * c).sqrt() / v.len() as f64;
        if spread < STEADY_ANGLE {
            return v.last().copied();
        }
        return Some(s.atan2(c).rem_euclid(std::f64::consts::TAU));
    }
    Some(v.iter().sum::<f64>() / v.len() as f64)
}

pub fn value_at(ring: &Ring, r: Reading, t: i64) -> Option<f64> {
    let (at, v) = ring.at_or_before(t)?;
    if (t - at) as f64 > ring.gap_ms() {
        return None;
    }
    match r {
        Reading::ZeroFilled => read_window(ring, r, at, at + 1).last().copied(),
        _ => Some(v),
    }
}

pub fn window_stats(ring: &Ring, r: Reading, from: i64, to: i64) -> crate::charts::RangeStats {
    stats_of(&read_window(ring, r, from, to), r)
}

pub fn stats_of(v: &[f64], r: Reading) -> crate::charts::RangeStats {
    let mut s = crate::charts::range_stats(v.iter().copied());
    let circle = |v: &mut dyn Iterator<Item = f64>, n: usize| {
        let (sn, cs) = v.fold((0.0, 0.0), |(a, b), x| (a + x.sin(), b + x.cos()));
        let len = ((sn * sn + cs * cs).sqrt() / n as f64).clamp(1e-12, 1.0);
        (sn.atan2(cs), (-2.0 * len.ln()).sqrt())
    };
    if r == Reading::Wrapping && s.n > 0 {
        let (mean, sd) = circle(&mut v.iter().copied().filter(|x| x.is_finite()), s.n);
        s.mean = mean.rem_euclid(std::f64::consts::TAU);
        s.sd = sd;
    }
    if r == Reading::Turn && s.n > 0 {
        let (mean, sd) = circle(&mut v.iter().copied().filter(|x| x.is_finite()).map(f64::to_radians), s.n);
        s.mean = mean.to_degrees();
        s.sd = sd.to_degrees();
    }
    s
}

pub fn fmt(v: f64) -> String {
    if v.is_nan() {
        return "NaN".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if v == 0.0 {
        return "0".into();
    }
    let a = v.abs();
    if !(1e-4..1e9).contains(&a) {
        return format!("{v:.4e}");
    }
    let decimals = (5 - a.log10().floor() as i32).clamp(0, 7) as usize;
    let s = format!("{v:.decimals$}");
    if s.trim_start_matches('-').chars().all(|c| c == '0' || c == '.') { "0".into() } else { s }
}

pub fn fmt_short(v: f64) -> String {
    if !v.is_finite() {
        return String::new();
    }
    let s = format!("{v:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.to_string() }
}

pub fn label(cat: &Catalogue, key: &ChannelKey) -> String {
    let (name, select) = match cat.get(key.signal) {
        Some(s) => (s.display_name(), s.select),
        None => (format!("Signal {}", key.signal), Select::Axis),
    };
    match select {
        Select::Axis => format!("{name}  {} J{}", key.unit, key.axis.one_based()),
        Select::Number => {
            let j = cat.get(key.signal).and_then(|s| s.joint).map(|j| format!(" J{j}")).unwrap_or_default();
            if name.contains("J1..J6") { format!("{}  {}{j}", name.replace(" J1..J6", ""), key.unit) } else { format!("{name}  {}{j}", key.unit) }
        }
        Select::Robot | Select::Module => format!("{name}  {}", key.unit),
        Select::Controller => name,
    }
}

pub fn local_time(t: std::time::SystemTime) -> String {
    if let Some((_, _, _, h, mi, s, ms)) = spy_core::util::local_parts(t) {
        return format!("{h:02}:{mi:02}:{s:02}.{ms:03}");
    }
    let iso = spy_core::util::wall_iso(t);
    format!("{} UTC", &iso[11..23])
}

pub fn local_hms(t: std::time::SystemTime) -> String {
    let s = local_time(t);
    match s.split_once('.') {
        Some((hms, rest)) if rest.ends_with("UTC") => format!("{hms} UTC"),
        Some((hms, _)) => hms.to_string(),
        None => s,
    }
}

pub fn log_line(e: &spy_core::log::Entry) -> String {
    format!("{}  {}", local_hms(e.wall), e.text)
}

pub fn short_label(cat: &Catalogue, key: &ChannelKey) -> String {
    format!("{} · {}", key.signal, label(cat, key))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Live,
    Stale,
    Waiting,
    Refused,
    NoReply,
    NotConnected,
    NotOnVc,
    NoEventYet,
}

impl Health {
    pub fn word(self) -> &'static str {
        match self {
            Health::Live => "LIVE",
            Health::Stale => "STALE",
            Health::Waiting => "WAITING",
            Health::Refused => "REFUSED",
            Health::NoReply => "NO REPLY",
            Health::NotConnected => "OFFLINE",
            Health::NotOnVc => "NOT ON VC",
            Health::NoEventYet => "NO EVENT YET",
        }
    }
    pub fn is_live(self) -> bool {
        self == Health::Live
    }
}

pub fn is_text(s: &Signal) -> bool {
    s.value_type.as_deref() == Some("string") || s.has(catalogue::flag::EVENT)
}

pub fn session_live(phase: &spy_core::session::Phase) -> bool {
    phase.is_connected() && *phase != spy_core::session::Phase::TearingDown
}

pub fn health(st: Option<&ChannelStatus>, connected: bool, sig: Option<&Signal>, loopback: bool) -> Health {
    let physical = sig.is_some_and(|s| s.has(catalogue::flag::PHYSICAL));
    let event = sig.is_some_and(|s| s.has(catalogue::flag::EVENT)) || st.is_some_and(|c| c.kind == Some(spy_core::sample::ValueKind::String));
    let Some(c) = st else { return Health::NotConnected };
    match &c.state {
        ChannelState::Refused { .. } if physical && loopback => Health::NotOnVc,
        ChannelState::Refused { .. } => Health::Refused,
        ChannelState::NoReply => Health::NoReply,
        _ if !connected => Health::NotConnected,
        ChannelState::Waiting | ChannelState::Defining => Health::Waiting,
        ChannelState::Defined { .. } if event && c.last_arrival.is_none() => Health::NoEventYet,
        ChannelState::Defined { .. } if event => Health::Live,
        ChannelState::Defined { .. } if c.stale || c.last_arrival.is_none() => {
            if physical && loopback { Health::NotOnVc } else { Health::Stale }
        }
        ChannelState::Defined { .. } => Health::Live,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spy_core::request::{Axis, MechUnit};

    #[test]
    fn numbers() {
        assert_eq!(fmt(356.7), "356.700");
        assert_eq!(fmt(0.942708), "0.942708");
        assert_eq!(fmt(-15.0), "-15.0000");
        assert_eq!(fmt(0.0), "0");
        assert_eq!(fmt(-0.0000001), "-1.0000e-7");
        assert_eq!(fmt(3.4e38), "3.4000e38");
        assert_eq!(fmt(f64::NAN), "NaN");
        assert_eq!(fmt(123456.0), "123456");
    }

    #[test]
    fn short_numbers() {
        assert_eq!(fmt_short(500.0), "500");
        assert_eq!(fmt_short(0.01), "0.01");
        assert_eq!(fmt_short(-356.5), "-356.5");
        assert_eq!(fmt_short(-0.0000001), "0", "never -0");
        assert_eq!(fmt_short(f64::NAN), "");
    }

    #[test]
    fn a_smoothed_value_averages_its_smoothing_or_150_ms() {
        assert_eq!(readout_window(0), 150);
        assert_eq!(readout_window(50), 150);
        assert_eq!(readout_window(500), 500);
        let mut v = vec![0.0; 100];
        v.extend(vec![10.0; 75]);
        let r = ring(&v);
        assert_eq!(readout_ms(&r, Reading::Plain, readout_window(0)), Some(10.0));
        let half = readout_ms(&r, Reading::Plain, readout_window(500)).unwrap();
        assert!(half > 2.0 && half < 8.0, "{half}");
    }

    #[test]
    fn degrees_by_default_radians_on_request() {
        let cat = Catalogue::builtin();
        let angle = cat.get(6000);
        let d = display(angle, false);
        assert_eq!(d.units, "deg");
        assert!((d.factor - 57.29578).abs() < 1e-4);
        assert_eq!(display(angle, true).units, "rad");
        assert_eq!(display(cat.get(5027), false).units, "V");
        assert!(display(cat.get(5138), false).wraps);
    }

    #[test]
    fn the_log_pane_shows_the_same_clock_as_the_charts() {
        let wall = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_790_410_000);
        let e = spy_core::log::Entry { seq: 0, wall, level: spy_core::log::Level::Info, text: "x".into() };
        let line = log_line(&e);
        assert!(line.starts_with(&local_time(wall)[..8]), "{line} / {}", local_time(wall));
        assert!(line.ends_with("  x"), "{line}");
    }

    fn ring(values: &[f64]) -> Ring {
        let mut r = Ring::new(4.0);
        for (i, &v) in values.iter().enumerate() {
            r.push(i as i64 * 4, v);
        }
        r
    }

    #[test]
    fn a_zero_filled_speed_reads_its_reported_values_not_the_padding() {
        let v = 0.0262;
        let pattern: Vec<f64> = (0..200).map(|i| if [0, 1, 4, 5, 8].contains(&(i % 10)) { v } else { 0.0 }).collect();
        let r = ring(&pattern);
        let got = readout(&r, Reading::ZeroFilled).unwrap();
        assert!((got - v).abs() < 1e-12, "{got}: the padding zeros were averaged in");
        assert!(readout(&r, Reading::Plain).unwrap() < v * 0.6);
    }

    #[test]
    fn a_cursor_in_a_gap_reads_nothing() {
        let mut r = Ring::new(4.0);
        for i in 0..10 {
            r.push(i * 4, 1.0);
        }
        for i in 0..10 {
            r.push(5000 + i * 4, 2.0);
        }
        assert_eq!(value_at(&r, Reading::Plain, 20), Some(1.0));
        assert_eq!(value_at(&r, Reading::Plain, 22), Some(1.0), "between two samples");
        assert_eq!(value_at(&r, Reading::Plain, 2500), None, "the last value before a gap, read inside it");
        assert_eq!(value_at(&r, Reading::Plain, 5020), Some(2.0));
        assert_eq!(value_at(&r, Reading::Plain, 9000), None, "long after the newest sample");
    }

    #[test]
    fn a_zero_filled_speed_reads_zero_once_the_joint_stops() {
        let mut values = vec![0.5; 100];
        values.extend(vec![0.0; 75]);
        let r = ring(&values);
        assert_eq!(readout(&r, Reading::ZeroFilled), Some(0.0), "a stopped joint must not keep its last speed");
        let mut h = ZeroHold::new();
        let out: Vec<f64> = values.iter().enumerate().map(|(i, &x)| h.apply_at(i as i64 * 4, x)).collect();
        assert_eq!(out[100], 0.5, "padding right after a value is held");
        assert_eq!(out[100 + 24], 0.5, "held for the hold time (100 ms after the value)");
        assert_eq!(out[100 + 25], 0.0, "and not beyond it");
        assert_eq!(*out.last().unwrap(), 0.0);
        let mut h = ZeroHold::new();
        assert_eq!([(0, 1.0), (4, 0.0), (8, -2.0), (12, 0.0)].map(|(t, x)| h.apply_at(t, x)), [1.0, 1.0, -2.0, -2.0]);
    }

    #[test]
    fn a_resolver_angle_averages_on_the_circle() {
        use std::f64::consts::TAU;
        let dither: Vec<f64> = (0..60).map(|i| if i % 2 == 0 { TAU - 0.001 } else { 0.001 }).collect();
        let r = ring(&dither);
        let got = readout(&r, Reading::Wrapping).unwrap();
        assert!(got < 1e-6 || TAU - got < 1e-6, "{got}");
        assert!((readout(&r, Reading::Plain).unwrap() - std::f64::consts::PI).abs() < 0.01, "the old readout");
        let r = ring(&[1.0, 1.02, 0.98, 1.0]);
        assert!((readout(&r, Reading::Wrapping).unwrap() - 1.0).abs() < 1e-3);
        let spinning: Vec<f64> = (0..38).map(|i| (i as f64 * 1.3).rem_euclid(TAU)).collect();
        assert_eq!(readout(&ring(&spinning), Reading::Wrapping), spinning.last().copied());
        let s = window_stats(&ring(&dither), Reading::Wrapping, 0, 1000);
        assert!(s.mean < 1e-6 || TAU - s.mean < 1e-6, "{}", s.mean);
        assert!(s.sd < 0.01, "{}", s.sd);
    }

    #[test]
    fn the_cursor_value_of_a_zero_filled_signal_is_not_a_padding_zero() {
        let r = ring(&[0.3, 0.0, 0.0, 0.4, 0.0]);
        assert_eq!(value_at(&r, Reading::ZeroFilled, 8), Some(0.3));
        assert_eq!(value_at(&r, Reading::ZeroFilled, 17), Some(0.4));
        assert_eq!(value_at(&r, Reading::Plain, 8), Some(0.0));
        assert_eq!(value_at(&r, Reading::Plain, -1), None);
    }

    fn status(state: ChannelState, samples: u64, arrived: bool, stale: bool, kind: Option<spy_core::sample::ValueKind>) -> ChannelStatus {
        ChannelStatus {
            key: ChannelKey { signal: 4002, unit: spy_core::request::MechUnit::new("ROB_1").unwrap(), axis: spy_core::request::Axis::new(1).unwrap() },
            state,
            sample_ms: Some(4.032),
            kind,
            samples,
            last_arrival: arrived.then(std::time::Instant::now),
            rate: 0.0,
            gaps: 0,
            stale,
        }
    }

    #[test]
    fn only_a_current_sample_is_live() {
        let cat = Catalogue::builtin();
        let torque = cat.get(4002);
        let def = ChannelState::Defined { stream: 215 };
        assert_eq!(health(Some(&status(def.clone(), 10, true, false, None)), true, torque, false), Health::Live);
        assert_eq!(health(Some(&status(def.clone(), 10, true, true, None)), true, torque, false), Health::Stale);
        assert_eq!(health(Some(&status(def.clone(), 10, false, false, None)), true, torque, false), Health::Stale, "defined on this connection, nothing yet");
        assert_eq!(health(Some(&status(def.clone(), 10, true, false, None)), false, torque, false), Health::NotConnected);
        assert_eq!(health(None, true, torque, false), Health::NotConnected);
        assert_eq!(health(Some(&status(ChannelState::Waiting, 0, false, false, None)), true, torque, false), Health::Waiting);
        let dc = cat.get(5027);
        assert_eq!(health(Some(&status(def.clone(), 0, false, true, None)), true, dc, true), Health::NotOnVc);
    }

    #[test]
    fn nothing_is_live_while_disconnecting() {
        use spy_core::session::Phase;
        assert!(session_live(&Phase::Streaming) && session_live(&Phase::SettingUp));
        assert!(!session_live(&Phase::TearingDown), "nothing arriving then is filed");
        assert!(!session_live(&Phase::Idle));
        let cat = Catalogue::builtin();
        let s = status(ChannelState::Defined { stream: 215 }, 10, true, false, None);
        assert_eq!(health(Some(&s), session_live(&Phase::TearingDown), cat.get(4002), false), Health::NotConnected);
    }

    #[test]
    fn a_text_event_from_before_a_reconnect_is_not_live() {
        let cat = Catalogue::builtin();
        let seg = cat.get(9875);
        let def = ChannelState::Defined { stream: 260 };
        let text = Some(spy_core::sample::ValueKind::String);
        assert_eq!(health(Some(&status(def.clone(), 3, false, false, text)), true, seg, false), Health::NoEventYet);
        assert_eq!(health(Some(&status(def.clone(), 3, true, false, text)), true, seg, false), Health::Live);
        assert_eq!(health(Some(&status(def, 0, false, false, None)), true, seg, false), Health::NoEventYet);
    }

    #[test]
    fn labels_say_what_selects_them() {
        let cat = Catalogue::builtin();
        let k = |s, u: &str, a| ChannelKey { signal: s, unit: MechUnit::new(u).unwrap(), axis: Axis::new(a).unwrap() };
        assert_eq!(label(&cat, &k(4002, "ROB_1", 2)), "Torque  ROB_1 J2");
        assert_eq!(label(&cat, &k(5027, "ROB_2", 1)), "DC-link voltage  ROB_2");
        assert_eq!(label(&cat, &k(6001, "ROB_1", 1)), "Joint reference (EGM)  ROB_1 J2");
        assert_eq!(label(&cat, &k(99999, "ROB_1", 3)), "Signal 99999  ROB_1 J3");
    }
}
