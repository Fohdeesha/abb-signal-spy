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
        }
    }
}

impl Settings {
    /// Load, or defaults. The second value is a note for the log when something
    /// was wrong with the file.
    pub fn load(path: &Path) -> (Settings, Option<String>) {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Settings::default(), None),
            Err(e) => return (Settings::default(), Some(format!("Could not read the settings ({e}); using defaults."))),
        };
        match serde_json::from_str::<Settings>(spy_core::util::strip_bom(&text)) {
            Ok(mut s) => {
                s.sanitize();
                (s, None)
            }
            Err(e) => {
                let aside = path.with_extension(format!("json.bad-{}", spy_core::util::local_stamp(std::time::SystemTime::now())));
                let _ = std::fs::rename(path, &aside);
                (Settings::default(), Some(format!("The settings file could not be read ({e}); it was kept as {} and defaults are in use.", aside.display())))
            }
        }
    }

    /// Clamp anything a hand edit could have put out of range.
    fn sanitize(&mut self) {
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
        if !(self.ui_scale.is_finite() && (0.6..=2.5).contains(&self.ui_scale)) {
            self.ui_scale = 1.0;
        }
        self.channels.truncate(spy_core::session::MAX_CHANNELS);
        self.recent.truncate(8);
        self.units.retain(|u| spy_core::request::MechUnit::new(u).is_ok());
        if self.units.is_empty() {
            self.units = vec!["ROB_1".into(), "ROB_2".into()];
        }
    }

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
}
