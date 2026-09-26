//! How values are shown: units (degrees by default, F1), number formatting, labels,
//! and the one status word per channel. Pure functions, tested below.

use spy_core::catalogue::{self, Catalogue, Select, Signal};
use spy_core::session::{ChannelState, ChannelStatus};
use spy_core::store::ChannelKey;

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
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
        use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
        if let Ok(d) = t.duration_since(std::time::UNIX_EPOCH) {
            // FILETIME: 100 ns ticks since 1601.
            let ticks = d.as_nanos() / 100 + 116_444_736_000_000_000;
            let ft = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
            unsafe {
                let mut utc: SYSTEMTIME = std::mem::zeroed();
                let mut local: SYSTEMTIME = std::mem::zeroed();
                if FileTimeToSystemTime(&ft, &mut utc) != 0 && SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) != 0 {
                    return format!("{:02}:{:02}:{:02}.{:03}", local.wHour, local.wMinute, local.wSecond, local.wMilliseconds);
                }
            }
        }
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
        ChannelState::Defined { .. } if event && c.samples == 0 => Health::NoEventYet,
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
