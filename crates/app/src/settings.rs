//! Saved settings. Nothing ships preset: no controller address, no signal set (rule
//! 11). Every field has a default, so a settings file from an older version, or one
//! a person trimmed by hand, still loads; a file that does not parse at all is set
//! aside (renamed) and reported, never silently lost.

use std::path::{Path, PathBuf};

use spy_core::session::Target;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SavedController {
    pub name: String,
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SavedChannel {
    pub signal: u32,
    pub unit: String,
    pub axis: u8,
    #[serde(default)]
    pub radians: bool,
    #[serde(default)]
    pub hold_nonzero: bool,
    #[serde(default)]
    pub lane: u32,
}

/// A derived channel (its plateau, if a sag, is never kept).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SavedDerived {
    pub def: spy_core::derived::Derived,
    #[serde(default)]
    pub lane: u32,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Settings {
    pub version: u32,
    pub controllers: Vec<SavedController>,
    pub recent: Vec<Target>,
    pub last_target: Option<Target>,
    pub dark: bool,
    /// Chart window, seconds (F4: 10 s).
    pub window_s: f64,
    pub channels: Vec<SavedChannel>,
    pub record_dir: Option<PathBuf>,
    pub slow_interval_ms: u32,
    pub snapshot_s: f64,
    pub phone_port: u16,
    pub catalogue_file: Option<PathBuf>,
    pub show_open: bool,
    pub show_inert: bool,
    pub favourites: Vec<u32>,
    pub units: Vec<String>,
    pub ui_scale: f32,
    pub derived: Vec<SavedDerived>,
    /// RWS's port (no login is ever kept, G16), and whether its event log is looked at.
    pub rws_port: u16,
    pub rws_events: bool,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            version: 1,
            controllers: Vec::new(),
            recent: Vec::new(),
            last_target: None,
            dark: true,
            window_s: 10.0,
            channels: Vec::new(),
            record_dir: None,
            slow_interval_ms: 1000,
            snapshot_s: 30.0,
            phone_port: 8090,
            catalogue_file: None,
            show_open: false,
            show_inert: false,
            favourites: Vec::new(),
            units: vec!["ROB_1".into(), "ROB_2".into()],
            ui_scale: 1.0,
            derived: Vec::new(),
            rws_port: spy_core::rws::DEFAULT_PORT,
            rws_events: true,
        }
    }
}

impl Settings {
    /// Load, or defaults. The second value is a note for the person when something
    /// was wrong with the file: what was set aside, and why.
    ///
    /// Field by field: a hand edit that spoils one value (an axis of 300, a word
    /// where a number goes) costs that value, not the saved controllers and channels
    /// with it. Only a file that is not a JSON object at all is set aside whole.
    pub fn load(path: &Path) -> (Settings, Option<String>) {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Settings::default(), None),
            Err(e) => return (Settings::default(), Some(format!("Could not read the settings ({e}); using defaults."))),
        };
        let obj = match serde_json::from_str::<serde_json::Value>(spy_core::util::strip_bom(&text)) {
            Ok(serde_json::Value::Object(m)) => m,
            other => {
                let why = match other {
                    Err(e) => e.to_string(),
                    Ok(_) => "it is not a settings object".into(),
                };
                let aside = path.with_extension(format!("json.bad-{}", spy_core::util::local_stamp(std::time::SystemTime::now())));
                let _ = std::fs::rename(path, &aside);
                return (Settings::default(), Some(format!("The settings file could not be read ({why}); it was kept as {} and defaults are in use.", aside.display())));
            }
        };
        let mut notes: Vec<String> = Vec::new();
        let d = Settings::default();
        let mut s = Settings {
            version: field(&obj, "version", d.version, &mut notes),
            controllers: field(&obj, "controllers", d.controllers, &mut notes),
            recent: field(&obj, "recent", d.recent, &mut notes),
            last_target: field(&obj, "last_target", d.last_target, &mut notes),
            dark: field(&obj, "dark", d.dark, &mut notes),
            window_s: field(&obj, "window_s", d.window_s, &mut notes),
            channels: channels(&obj, &mut notes),
            record_dir: field(&obj, "record_dir", d.record_dir, &mut notes),
            slow_interval_ms: field(&obj, "slow_interval_ms", d.slow_interval_ms, &mut notes),
            snapshot_s: field(&obj, "snapshot_s", d.snapshot_s, &mut notes),
            phone_port: field(&obj, "phone_port", d.phone_port, &mut notes),
            catalogue_file: field(&obj, "catalogue_file", d.catalogue_file, &mut notes),
            show_open: field(&obj, "show_open", d.show_open, &mut notes),
            show_inert: field(&obj, "show_inert", d.show_inert, &mut notes),
            favourites: field(&obj, "favourites", d.favourites, &mut notes),
            units: field(&obj, "units", d.units, &mut notes),
            ui_scale: field(&obj, "ui_scale", d.ui_scale, &mut notes),
            derived: derived(&obj, &mut notes),
            rws_port: field(&obj, "rws_port", d.rws_port, &mut notes),
            rws_events: field(&obj, "rws_events", d.rws_events, &mut notes),
        };
        s.sanitize(&mut notes);
        let note = (!notes.is_empty()).then(|| format!("Parts of the settings file ({}) could not be used and were left out (the rest loaded): {}.", path.display(), notes.join("; ")));
        (s, note)
    }

    /// Clamp anything a hand edit could have put out of range.
    fn sanitize(&mut self, notes: &mut Vec<String>) {
        if !(self.window_s.is_finite() && (1.0..=600.0).contains(&self.window_s)) {
            self.window_s = 10.0;
        }
        if !(self.snapshot_s.is_finite() && (1.0..=600.0).contains(&self.snapshot_s)) {
            self.snapshot_s = 30.0;
        }
        if !(100..=3_600_000).contains(&self.slow_interval_ms) {
            self.slow_interval_ms = 1000;
        }
        if self.phone_port < 1024 {
            self.phone_port = 8090;
        }
        if self.rws_port == 0 {
            self.rws_port = spy_core::rws::DEFAULT_PORT;
        }
        if !(self.ui_scale.is_finite() && (0.6..=2.5).contains(&self.ui_scale)) {
            self.ui_scale = 1.0;
        }
        if self.channels.len() > spy_core::session::MAX_CHANNELS {
            notes.push(format!("{} channels, of which only the first {} can be used", self.channels.len(), spy_core::session::MAX_CHANNELS));
            self.channels.truncate(spy_core::session::MAX_CHANNELS);
        }
        self.recent.truncate(8);
        self.units.retain(|u| spy_core::request::MechUnit::new(u).is_ok());
        if self.units.is_empty() {
            self.units = vec!["ROB_1".into(), "ROB_2".into()];
        }
    }
}

/// One field of the file, or its default with a note saying why.
fn field<T: serde::de::DeserializeOwned>(obj: &serde_json::Map<String, serde_json::Value>, name: &str, default: T, notes: &mut Vec<String>) -> T {
    match obj.get(name) {
        None => default,
        Some(v) => serde_json::from_value(v.clone()).unwrap_or_else(|e| {
            notes.push(format!("\"{name}\" ({e})"));
            default
        }),
    }
}

/// The channels, entry by entry: one that does not describe a channel (a bad unit or
/// axis, a missing signal) or repeats an earlier one is left out and named; the cut
/// to twelve comes after, so it never falls on a duplicate's place.
fn channels(obj: &serde_json::Map<String, serde_json::Value>, notes: &mut Vec<String>) -> Vec<SavedChannel> {
    let Some(v) = obj.get("channels") else { return Vec::new() };
    let Some(list) = v.as_array() else {
        notes.push("\"channels\" (not a list)".into());
        return Vec::new();
    };
    let mut out: Vec<SavedChannel> = Vec::new();
    for (i, e) in list.iter().enumerate() {
        let c = match serde_json::from_value::<SavedChannel>(e.clone()) {
            Ok(c) => c,
            Err(err) => {
                notes.push(format!("channel {} ({err})", i + 1));
                continue;
            }
        };
        if spy_core::request::MechUnit::new(&c.unit).is_err() || spy_core::request::Axis::new(c.axis).is_none() {
            notes.push(format!("channel {} (signal {}: \"{}\" axis {} is not a mechanical unit and axis)", i + 1, c.signal, c.unit, c.axis));
            continue;
        }
        if out.iter().any(|o| o.signal == c.signal && o.unit == c.unit && o.axis == c.axis) {
            notes.push(format!("channel {} (signal {} {} axis {} a second time)", i + 1, c.signal, c.unit, c.axis));
            continue;
        }
        out.push(c);
    }
    out
}

/// The derived channels, entry by entry, as the channels are read. A plateau in the
/// file (put there by hand) is dropped: it belongs to the controller it was measured
/// on, at the time.
fn derived(obj: &serde_json::Map<String, serde_json::Value>, notes: &mut Vec<String>) -> Vec<SavedDerived> {
    let Some(v) = obj.get("derived") else { return Vec::new() };
    let Some(list) = v.as_array() else {
        notes.push("\"derived\" (not a list)".into());
        return Vec::new();
    };
    let mut out: Vec<SavedDerived> = Vec::new();
    for (i, e) in list.iter().enumerate() {
        match serde_json::from_value::<SavedDerived>(e.clone()) {
            Ok(mut d) => {
                if let spy_core::derived::Derived::Sag { plateau_v, .. } = &mut d.def {
                    *plateau_v = None;
                }
                if let spy_core::derived::Derived::Turn { target_deg: Some(t), .. } = &d.def
                    && !t.is_finite()
                {
                    notes.push(format!("derived channel {} (a target that is not a number)", i + 1));
                    continue;
                }
                if out.iter().any(|o| o.def.same(&d.def)) {
                    notes.push(format!("derived channel {} (a second time)", i + 1));
                    continue;
                }
                out.push(d);
            }
            Err(err) => notes.push(format!("derived channel {} ({err})", i + 1)),
        }
    }
    out
}

impl Settings {
    /// Write atomically: a crash while saving leaves the previous file intact.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let body = serde_json::to_string_pretty(self).map_err(|e| e.to_string())? + "\n";
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, body).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("cannot replace {}: {e}", path.display()))
    }

    pub fn remember(&mut self, t: &Target) {
        self.recent.retain(|r| r != t);
        self.recent.insert(0, t.clone());
        self.recent.truncate(8);
        self.last_target = Some(t.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_partial_or_old_file_loads_with_defaults() {
        let dir = std::env::temp_dir().join(format!("spy-settings-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("settings.json");
        std::fs::write(&p, r#"{"dark": false, "window_s": 99999, "units": ["ROB_1", "bad unit"], "unknown": 1}"#).unwrap();
        let (s, note) = Settings::load(&p);
        assert!(note.is_none());
        assert!(!s.dark);
        assert_eq!(s.window_s, 10.0, "out-of-range values are clamped");
        assert_eq!(s.units, vec!["ROB_1".to_string()]);
        assert!(s.controllers.is_empty(), "nothing ships preset");
        // A byte-order mark, as Notepad and PowerShell 5 write it, is accepted.
        std::fs::write(&p, "\u{FEFF}{\"dark\": false}").unwrap();
        let (s, note) = Settings::load(&p);
        assert!(note.is_none(), "{note:?}");
        assert!(!s.dark);
        std::fs::write(&p, "{ not json").unwrap();
        let (s, note) = Settings::load(&p);
        assert_eq!(s, Settings::default());
        assert!(note.unwrap().contains("kept as"));
        assert!(!p.exists(), "the bad file is set aside, not overwritten");
        s.save(&p).unwrap();
        assert_eq!(Settings::load(&p).0, s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn load_text(tag: &str, text: &str) -> (Settings, Option<String>) {
        let dir = std::env::temp_dir().join(format!("spy-settings-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("settings.json");
        std::fs::write(&p, text).unwrap();
        let r = Settings::load(&p);
        let _ = std::fs::remove_dir_all(&dir);
        r
    }

    #[test]
    fn one_bad_value_costs_that_value_not_the_file() {
        let (s, note) = load_text(
            "field",
            r#"{"window_s": "ten", "controllers": [{"name": "cell", "host": "192.0.2.77", "port": 5515}],
                "channels": [{"signal": 5027, "unit": "ROB_1", "axis": 300}, {"signal": 4002, "unit": "ROB_1", "axis": 2}]}"#,
        );
        assert_eq!(s.window_s, 10.0);
        assert_eq!(s.controllers.len(), 1, "the saved controllers survive a bad value elsewhere");
        assert_eq!(s.channels.len(), 1, "the good channel survives the bad one");
        assert_eq!(s.channels[0].signal, 4002);
        let note = note.expect("the person is told");
        assert!(note.contains("window_s") && note.contains("channel 1"), "{note}");
    }

    #[test]
    fn duplicates_go_before_the_cut_to_twelve() {
        // A, A, then eleven more: twelve distinct channels, the second A named.
        let mut list = vec![r#"{"signal": 4002, "unit": "ROB_1", "axis": 1}"#.to_string(); 2];
        for a in 1..=6 {
            list.push(format!(r#"{{"signal": 4000, "unit": "ROB_1", "axis": {a}}}"#));
        }
        for a in 1..=5 {
            list.push(format!(r#"{{"signal": 4000, "unit": "ROB_2", "axis": {a}}}"#));
        }
        let (s, note) = load_text("dup", &format!(r#"{{"channels": [{}]}}"#, list.join(",")));
        assert_eq!(s.channels.len(), 12, "{:?}", s.channels);
        assert_eq!((s.channels[11].unit.as_str(), s.channels[11].axis), ("ROB_2", 5), "the last one was cut for the duplicate's place");
        assert!(note.unwrap().contains("channel 2 (signal 4002 ROB_1 axis 1 a second time)"));
        // Thirteen distinct: the cut is said too.
        list.remove(0);
        list.push(r#"{"signal": 5027, "unit": "ROB_1", "axis": 1}"#.into());
        assert_eq!(list.len(), 13);
        let (s, note) = load_text("cut", &format!(r#"{{"channels": [{}]}}"#, list.join(",")));
        assert_eq!(s.channels.len(), 12);
        assert!(note.unwrap().contains("13 channels"));
    }

    #[test]
    fn every_setting_survives_a_save_and_a_load() {
        // Guards the field-by-field loader: a setting it forgot would come back as
        // its default.
        let s = Settings {
            version: 3,
            controllers: vec![SavedController { name: "cell".into(), host: "192.0.2.77".into(), port: 5515 }],
            recent: vec![Target { host: "10.0.0.2".into(), port: 5515 }],
            last_target: Some(Target { host: "10.0.0.2".into(), port: 5515 }),
            dark: false,
            window_s: 30.0,
            channels: vec![SavedChannel { signal: 6000, unit: "ROB_2".into(), axis: 1, radians: true, hold_nonzero: true, lane: 4 }],
            record_dir: Some(PathBuf::from(r"D:\rec")),
            slow_interval_ms: 5000,
            snapshot_s: 60.0,
            phone_port: 9000,
            catalogue_file: Some(PathBuf::from(r"D:\cat.json")),
            show_open: true,
            show_inert: true,
            favourites: vec![5027],
            units: vec!["ROB_1".into(), "STN_1".into()],
            ui_scale: 1.2,
            derived: vec![SavedDerived { def: spy_core::derived::Derived::Turn { angle: key(5138, "ROB_2", 3), target_deg: Some(90.0) }, lane: 5 }],
            rws_port: 8080,
            rws_events: false,
        };
        assert_ne!(s, Settings::default());
        let dir = std::env::temp_dir().join(format!("spy-settings-all-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("settings.json");
        s.save(&p).unwrap();
        let (back, note) = Settings::load(&p);
        assert_eq!(note, None);
        assert_eq!(back, s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn key(signal: u32, unit: &str, axis: u8) -> spy_core::store::ChannelKey {
        spy_core::store::ChannelKey { signal, unit: spy_core::request::MechUnit::new(unit).unwrap(), axis: spy_core::request::Axis::new(axis).unwrap() }
    }

    #[test]
    fn derived_channels_load_one_by_one_and_never_with_a_plateau() {
        let dir = std::env::temp_dir().join(format!("spy-settings-derived-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("settings.json");
        std::fs::write(
            &p,
            r#"{"derived": [
                {"def": {"kind": "sag", "link": {"signal": 5027, "unit": "ROB_1", "axis": 1}, "plateau_v": 356.5}, "lane": 2},
                {"def": {"kind": "turn", "angle": {"signal": 5138, "unit": "ROB_1", "axis": 9}}},
                {"def": {"kind": "sag", "link": {"signal": 5027, "unit": "ROB_1", "axis": 1}}},
                {"def": {"kind": "duty_sum", "legs": [{"signal": 5020, "unit": "ROB_1", "axis": 2}, {"signal": 5021, "unit": "ROB_1", "axis": 2}, {"signal": 5022, "unit": "ROB_1", "axis": 2}]}}
            ]}"#,
        )
        .unwrap();
        let (s, note) = Settings::load(&p);
        let note = note.unwrap();
        assert!(note.contains("derived channel 2") && note.contains("derived channel 3 (a second time)"), "{note}");
        assert_eq!(s.derived.len(), 2);
        assert_eq!(s.derived[0].def, spy_core::derived::Derived::Sag { link: key(5027, "ROB_1", 1), plateau_v: None }, "a plateau in the file is not used");
        assert_eq!(s.derived[0].lane, 2);
        assert_eq!(s.derived[1].def.id(), "duty-sum:ROB_1/J2");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
