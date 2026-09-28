//! How values are shown: units (degrees by default, F1), number formatting, labels,
//! and the one status word per channel. Pure functions, tested below.

use spy_core::catalogue::{self, Catalogue, Select, Signal};
use spy_core::session::{ChannelState, ChannelStatus};
use spy_core::store::{ChannelKey, Ring};

/// The unit a channel is shown in and the factor from its native unit.
#[derive(Debug, Clone, PartialEq)]
pub struct Display {
    pub units: String,
    pub factor: f64,
    /// Shown as an angle reduced to one turn (0..360 deg or 0..2pi rad).
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

/// How a channel's samples are read for its value, its statistics and its chart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reading {
    Plain,
    /// Pads between the values it reports with exact zeros (the joint speeds
    /// 6010-6015 and 6030-6035). A zero within [`ZERO_HOLD_MS`] of a reported value is
    /// padding and stands for that value; a longer run of zeros is a real zero, the
    /// joint at rest.
    ZeroFilled,
    /// An angle reduced to one turn, in radians (0..2pi): averaged on the circle, so
    /// a value dithering across the 2pi-to-0 jump does not average to half a turn.
    Wrapping,
}

pub fn reading(sig: Option<&Signal>) -> Reading {
    match sig {
        Some(s) if s.has(catalogue::flag::ZERO_FILLED) => Reading::ZeroFilled,
        Some(s) if s.has(catalogue::flag::WRAPPING) && catalogue::angle_unit(&s.units).is_some() => Reading::Wrapping,
        _ => Reading::Plain,
    }
}

pub use spy_core::reading::{ZeroHold, ZERO_HOLD_MS};

/// The F3 readout window.
pub const READOUT_MS: i64 = 150;

/// Below this spread (the mean resultant length of the angles in the window) a
/// wrapping angle is moving too fast for an average to mean anything: the newest
/// sample is shown instead. 0.99 is a spread of about 8 degrees.
const STEADY_ANGLE: f64 = 0.99;

/// The last `window_ms` of a ring as the signal means it, oldest first: padding
/// undone for a zero-filled signal (with the hold primed from before the window).
fn read_window(ring: &Ring, r: Reading, from: i64, to: i64) -> Vec<f64> {
    match r {
        Reading::ZeroFilled => {
            let mut hold = ZeroHold::new(ring.sample_ms);
            let prime = from - ZERO_HOLD_MS.ceil() as i64 - 1;
            ring.range(prime, to).filter_map(|(t, v)| {
                let x = hold.apply(v);
                (t >= from).then_some(x)
            }).collect()
        }
        _ => ring.range(from, to).map(|(_, v)| v).collect(),
    }
}

/// The value a card shows, in the native unit: the mean of the last 150 ms (F3).
/// Padding undone for a zero-filled signal; the mean on the circle for a wrapping
/// angle, or its newest sample when it turns too fast to average.
pub fn readout(ring: &Ring, r: Reading) -> Option<f64> {
    let (last_t, _) = ring.last()?;
    let v: Vec<f64> = read_window(ring, r, last_t - READOUT_MS, last_t + 1).into_iter().filter(|x| x.is_finite()).collect();
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

/// The value at timeline point `t` as the signal means it (the last sample at or
/// before it, padding undone), for the cursors.
pub fn value_at(ring: &Ring, r: Reading, t: i64) -> Option<f64> {
    let (at, v) = ring.at_or_before(t)?;
    match r {
        Reading::ZeroFilled => read_window(ring, r, at, at + 1).last().copied(),
        _ => Some(v),
    }
}

/// Statistics of a stretch as the signal means it, in the native unit: padding
/// undone; for a wrapping angle the mean and standard deviation on the circle.
pub fn window_stats(ring: &Ring, r: Reading, from: i64, to: i64) -> crate::charts::RangeStats {
    stats_of(&read_window(ring, r, from, to), r)
}

/// Statistics of values already read as the signal means them (padding undone), in
/// the native unit; for a wrapping angle the mean and standard deviation on the
/// circle. The live window and a reviewed recording both use it.
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
    s
}

/// A number with sensible precision: about six significant figures, scientific for
/// the tiny and the huge, and never a misleading "-0".
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

/// Channel name for people: the catalogue's name, plus what selects it.
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

/// Local wall-clock time `HH:MM:SS.mmm`, daylight saving as it was on that date.
pub fn local_time(t: std::time::SystemTime) -> String {
    if let Some((_, _, _, h, mi, s, ms)) = spy_core::util::local_parts(t) {
        return format!("{h:02}:{mi:02}:{s:02}.{ms:03}");
    }
    let iso = spy_core::util::wall_iso(t);
    format!("{} UTC", &iso[11..23])
}

/// Local wall-clock time `HH:MM:SS`, as the log pane shows it: the same clock as the
/// chart's hover. (The log file keeps ISO UTC, marked `Z`.)
pub fn local_hms(t: std::time::SystemTime) -> String {
    let s = local_time(t);
    match s.split_once('.') {
        Some((hms, rest)) if rest.ends_with("UTC") => format!("{hms} UTC"),
        Some((hms, _)) => hms.to_string(),
        None => s,
    }
}

/// One line of the log pane.
pub fn log_line(e: &spy_core::log::Entry) -> String {
    format!("{}  {}", local_hms(e.wall), e.text)
}

/// Short form for chart legends and the phone view.
pub fn short_label(cat: &Catalogue, key: &ChannelKey) -> String {
    format!("{} · {}", key.signal, label(cat, key))
}

/// The one-word status shown for a channel, and whether its value may be shown as
/// live. The rule it keeps: anything but LIVE is never presented as current.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Live,
    Stale,
    Waiting,
    Refused,
    NoReply,
    NotConnected,
    /// A physical signal on a virtual controller: nothing will ever arrive.
    NotOnVc,
    /// A string event with no text yet; normal for them.
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

/// A text event by the catalogue: typed as a string, or flagged as an event.
pub fn is_text(s: &Signal) -> bool {
    s.value_type.as_deref() == Some("string") || s.has(catalogue::flag::EVENT)
}

/// Whether a session in this phase can have a live channel: connected, and not on
/// its way out (nothing arriving then is filed).
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
        // Nothing on this connection yet (its stream is defined afresh at every
        // connect): any text shown is from before, and may have changed meanwhile.
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
        // The chart hover shows local time; the log pane used to show UTC unmarked,
        // four hours apart on the cell's PC. (Discriminating only off UTC.)
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
        // The measured shape (2026-09-02, 6010 moving): values with runs of one to
        // three padding zeros between them.
        let v = 0.0262;
        let pattern: Vec<f64> = (0..200).map(|i| if [0, 1, 4, 5, 8].contains(&(i % 10)) { v } else { 0.0 }).collect();
        let r = ring(&pattern);
        let got = readout(&r, Reading::ZeroFilled).unwrap();
        assert!((got - v).abs() < 1e-12, "{got}: the padding zeros were averaged in");
        // Read plainly, the same samples say half the speed: what the card showed.
        assert!(readout(&r, Reading::Plain).unwrap() < v * 0.6);
    }

    #[test]
    fn a_zero_filled_speed_reads_zero_once_the_joint_stops() {
        let mut values = vec![0.5; 100];
        values.extend(vec![0.0; 75]); // 300 ms at rest
        let r = ring(&values);
        assert_eq!(readout(&r, Reading::ZeroFilled), Some(0.0), "a stopped joint must not keep its last speed");
        // The chart's hold lets go after the hold time too: a flat line at the last
        // speed forever was the old behaviour.
        let mut h = ZeroHold::new(4.0);
        let out: Vec<f64> = values.iter().map(|&x| h.apply(x)).collect();
        assert_eq!(out[100], 0.5, "padding right after a value is held");
        assert_eq!(out[100 + 24], 0.5, "held for the hold time (25 zeros of 4 ms)");
        assert_eq!(out[100 + 25], 0.0, "and not beyond it");
        assert_eq!(*out.last().unwrap(), 0.0);
        // A true value of zero amid motion is a sample like any other: the next
        // non-zero value is shown at once.
        let mut h = ZeroHold::new(4.0);
        assert_eq!([1.0, 0.0, -2.0, 0.0].map(|x| h.apply(x)), [1.0, 1.0, -2.0, -2.0]);
    }

    #[test]
    fn a_resolver_angle_averages_on_the_circle() {
        use std::f64::consts::TAU;
        // Dithering across the 2pi-to-0 jump: the plain mean is half a turn off.
        let dither: Vec<f64> = (0..60).map(|i| if i % 2 == 0 { TAU - 0.001 } else { 0.001 }).collect();
        let r = ring(&dither);
        let got = readout(&r, Reading::Wrapping).unwrap();
        assert!(got < 1e-6 || TAU - got < 1e-6, "{got}");
        assert!((readout(&r, Reading::Plain).unwrap() - std::f64::consts::PI).abs() < 0.01, "the old readout");
        // Steady elsewhere: the mean.
        let r = ring(&[1.0, 1.02, 0.98, 1.0]);
        assert!((readout(&r, Reading::Wrapping).unwrap() - 1.0).abs() < 1e-3);
        // Turning several times within the window: no average means anything; the
        // newest sample is shown.
        let spinning: Vec<f64> = (0..38).map(|i| (i as f64 * 1.3).rem_euclid(TAU)).collect();
        assert_eq!(readout(&ring(&spinning), Reading::Wrapping), spinning.last().copied());
        // And the statistics of a stretch, on the circle too.
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
        // 9875 sent nothing at StartStream on the cell: after a reconnect the text on
        // the card is the old connection's, and may have changed meanwhile.
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
