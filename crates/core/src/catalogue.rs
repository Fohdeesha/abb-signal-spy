use std::collections::BTreeMap;
use std::path::Path;

pub const FORMAT: &str = "abb-signal-spy-catalogue";
pub const VERSION: u32 = 1;
pub const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_SIGNALS: usize = 70_000;

const BUILTIN: &str = include_str!("../../../catalogue/irb2600-rw616.json");

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    Confirmed,
    Strong,
    Probable,
    Open,
    Inert,
}

impl Confidence {
    pub fn label(self) -> &'static str {
        match self {
            Confidence::Confirmed => "confirmed",
            Confidence::Strong => "strong",
            Confidence::Probable => "probable",
            Confidence::Open => "open",
            Confidence::Inert => "inert",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Select {
    Axis,
    Number,
    Robot,
    Module,
    Controller,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AbbName {
    pub name: String,
    #[serde(default)]
    pub units: String,
    #[serde(default)]
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Signal {
    pub number: u32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub named: bool,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub units: String,
    #[serde(default)]
    pub category: String,
    #[serde(default = "default_confidence")]
    pub confidence: Confidence,
    #[serde(default = "default_select")]
    pub select: Select,
    #[serde(default)]
    pub joint: Option<u8>,
    #[serde(default)]
    pub sample_ms: Option<f64>,
    #[serde(default, rename = "type")]
    pub value_type: Option<String>,
    #[serde(default)]
    pub cell: bool,
    #[serde(default)]
    pub vc: bool,
    #[serde(default)]
    pub flags: Vec<String>,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub abb: Option<AbbName>,
    #[serde(default)]
    pub evidence: String,
    #[serde(default)]
    pub ruled_out: String,
    #[serde(default)]
    pub open_question: String,
    #[serde(default)]
    pub next_test: String,
}

fn default_confidence() -> Confidence {
    Confidence::Open
}
fn default_select() -> Select {
    Select::Axis
}

pub mod flag {
    pub const FROZEN: &str = "frozen";
    pub const ZERO_FILLED: &str = "zero_filled";
    pub const SENTINEL: &str = "sentinel";
    pub const NAN: &str = "nan";
    pub const WRAPPING: &str = "wrapping";
    pub const EVENT: &str = "event";
    pub const PHYSICAL: &str = "physical";
    pub const VC_ONLY: &str = "vc_only";
    pub const NO_DATA: &str = "no_data_2026_09";
}

impl Signal {
    pub fn has(&self, flag: &str) -> bool {
        self.flags.iter().any(|f| f == flag)
    }

    pub fn display_name(&self) -> String {
        if !self.name.is_empty() {
            self.name.clone()
        } else if let Some(a) = &self.abb {
            a.name.clone()
        } else {
            format!("Signal {}", self.number)
        }
    }

    pub fn is_angle(&self) -> bool {
        angle_unit(&self.units).is_some()
    }
}

pub fn angle_unit(units: &str) -> Option<(&'static str, f64)> {
    let u = units.trim();
    let k = 180.0 / std::f64::consts::PI;
    match u {
        "rad" => Some(("deg", k)),
        "rad/s" => Some(("deg/s", k)),
        "rad/s2" | "rad/s^2" => Some(("deg/s2", k)),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Catalogue {
    pub format: String,
    pub version: u32,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub measured_on: String,
    #[serde(default)]
    pub caveat: String,
    #[serde(default)]
    pub credits: String,
    pub signals: Vec<Signal>,
    #[serde(skip)]
    pub source: String,
}

impl Catalogue {
    pub fn builtin() -> Catalogue {
        let mut c = Catalogue::parse(BUILTIN).expect("the built-in catalogue is valid (tested)");
        c.source = "built in".into();
        c
    }

    pub fn load(path: &Path) -> Result<Catalogue, String> {
        let meta = std::fs::metadata(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        if meta.len() > MAX_FILE_BYTES {
            return Err(format!("{} is {} MB; a catalogue is never that large", path.display(), meta.len() / (1024 * 1024)));
        }
        let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let mut c = Catalogue::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        c.source = path.display().to_string();
        Ok(c)
    }

    pub fn parse(text: &str) -> Result<Catalogue, String> {
        let c: Catalogue = serde_json::from_str(crate::util::strip_bom(text)).map_err(|e| format!("not a catalogue file ({e})"))?;
        c.validate()?;
        Ok(c)
    }

    fn validate(&self) -> Result<(), String> {
        if self.format != FORMAT {
            return Err(format!("the file says it is \"{}\", not an ABB Signal Spy catalogue", self.format));
        }
        if self.version > VERSION {
            return Err(format!("catalogue format {} is newer than this program understands ({VERSION}); update the program", self.version));
        }
        if self.signals.len() > MAX_SIGNALS {
            return Err(format!("{} signals is more than any controller has", self.signals.len()));
        }
        let mut seen = std::collections::HashSet::new();
        for s in &self.signals {
            if s.number == 0 {
                return Err("signal number 0 is not a signal".into());
            }
            if !seen.insert(s.number) {
                return Err(format!("signal {} is listed twice", s.number));
            }
            if s.select == Select::Number && !s.joint.is_some_and(|j| (1..=6).contains(&j)) {
                return Err(format!("signal {} is selected by its number but names no joint 1..6", s.number));
            }
            if let Some(ms) = s.sample_ms
                && !(ms.is_finite() && ms > 0.0 && ms < 100_000.0) {
                    return Err(format!("signal {}: sample time {ms} ms", s.number));
                }
            if let Some(t) = &s.value_type
                && !matches!(t.as_str(), "float" | "int" | "string") {
                    return Err(format!("signal {}: unknown type \"{t}\"", s.number));
                }
        }
        Ok(())
    }

    pub fn get(&self, number: u32) -> Option<&Signal> {
        self.signals.iter().find(|s| s.number == number)
    }

    pub fn groups(&self) -> BTreeMap<String, Vec<u32>> {
        let mut g: BTreeMap<String, Vec<u32>> = BTreeMap::new();
        for s in &self.signals {
            if let Some(id) = &s.group {
                g.entry(id.clone()).or_default().push(s.number);
            }
        }
        for v in g.values_mut() {
            v.sort_unstable();
        }
        g
    }

    pub fn search<'a>(&'a self, query: &str) -> impl Iterator<Item = &'a Signal> + 'a {
        let words: Vec<String> = query.split_whitespace().map(|w| w.to_lowercase()).collect();
        self.signals.iter().filter(move |s| {
            words.iter().all(|w| {
                if w.chars().all(|c| c.is_ascii_digit()) && s.number.to_string().starts_with(w.as_str()) {
                    return true;
                }
                let hay = [&s.name, &s.units, &s.category, &s.description]
                    .iter()
                    .map(|x| x.to_lowercase())
                    .chain(s.abb.as_ref().map(|a| a.name.to_lowercase()))
                    .collect::<Vec<_>>();
                hay.iter().any(|h| h.contains(w.as_str()))
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_loads_and_keeps_its_invariants() {
        let c = Catalogue::builtin();
        assert_eq!(c.format, FORMAT);
        assert_eq!(c.signals.len(), 535);
        assert_eq!(c.signals.iter().filter(|s| s.cell).count(), 498);
        assert_eq!(c.signals.iter().filter(|s| s.vc).count(), 319);
        assert_eq!(c.signals.iter().filter(|s| s.confidence == Confidence::Inert).count(), 141);
        assert!(c.signals.iter().filter(|s| s.named).count() >= 173);
        for n in 4000..=4003 {
            let s = c.get(n).unwrap();
            assert_eq!(s.confidence, Confidence::Confirmed, "{n}");
            assert_eq!(s.select, Select::Axis, "{n}");
        }
        let dc = c.get(5027).unwrap();
        assert_eq!(dc.select, Select::Module);
        assert!(dc.has(flag::PHYSICAL));
        for (i, n) in (6000..=6005).enumerate() {
            let s = c.get(n).unwrap();
            assert_eq!(s.select, Select::Number);
            assert_eq!(s.joint, Some(i as u8 + 1));
        }
        assert!(c.get(6010).unwrap().has(flag::ZERO_FILLED));
        assert!(c.get(516).unwrap().has(flag::FROZEN));
        assert!(!c.get(6040).unwrap().has(flag::FROZEN), "6040-6046 is the LIVE pose");
        assert!(c.get(5138).unwrap().has(flag::WRAPPING));
        assert_eq!(c.get(9872).unwrap().value_type.as_deref(), Some("string"));
        assert_eq!(c.get(9888).unwrap().value_type.as_deref(), Some("int"));
        assert_eq!(c.get(1519).unwrap().abb.as_ref().unwrap().source, "TuneMaster");
        let g = c.get(1717).unwrap().group.clone().unwrap();
        assert!(c.groups()[&g].len() >= 17);
        let text = serde_json::to_string(&c.signals).unwrap();
        for bad in ["192.168.", "bridge", ".md", "--mech", "decompil"] {
            assert!(!text.to_lowercase().contains(bad), "{bad}");
        }
    }

    #[test]
    fn search_finds_by_number_name_and_unit() {
        let c = Catalogue::builtin();
        assert!(c.search("5027").any(|s| s.number == 5027));
        assert!(c.search("dc link").any(|s| s.number == 5027));
        assert!(c.search("DC bus").any(|s| s.number == 5027), "ABB's name is searchable");
        assert!(c.search("nm torque").any(|s| s.number == 4002));
        assert_eq!(c.search("zzzz-nothing").count(), 0);
    }

    #[test]
    fn a_minimal_user_file_loads() {
        let c = Catalogue::parse(r#"{"format":"abb-signal-spy-catalogue","version":1,"signals":[{"number":42},{"number":43,"name":"x","unknown_future_field":1}]}"#).unwrap();
        assert_eq!(c.signals.len(), 2);
        assert_eq!(c.signals[0].select, Select::Axis);
        assert_eq!(c.signals[0].confidence, Confidence::Open);
        assert_eq!(c.get(42).unwrap().display_name(), "Signal 42");
        assert!(Catalogue::parse("\u{FEFF}{\"format\":\"abb-signal-spy-catalogue\",\"version\":1,\"signals\":[]}").is_ok());
    }

    #[test]
    fn bad_files_are_refused_with_a_reason() {
        let bad = [
            (r#"{"format":"x","version":1,"signals":[]}"#, "not an ABB Signal Spy catalogue"),
            (r#"{"format":"abb-signal-spy-catalogue","version":99,"signals":[]}"#, "newer"),
            (r#"{"format":"abb-signal-spy-catalogue","version":1,"signals":[{"number":5},{"number":5}]}"#, "twice"),
            (r#"{"format":"abb-signal-spy-catalogue","version":1,"signals":[{"number":5,"select":"number"}]}"#, "no joint"),
            (r#"{"format":"abb-signal-spy-catalogue","version":1,"signals":[{"number":5,"confidence":"certain"}]}"#, "not a catalogue"),
            (r#"{"format":"abb-signal-spy-catalogue","version":1,"signals":[{"number":5,"sample_ms":-1}]}"#, "sample time"),
            (r#"{"format":"abb-signal-spy-catalogue","version":1,"signals":[{"number":5,"type":"double"}]}"#, "unknown type"),
            ("not json", "not a catalogue"),
        ];
        for (text, why) in bad {
            let e = Catalogue::parse(text).unwrap_err();
            assert!(e.contains(why), "{text}: {e}");
        }
    }

    #[test]
    fn degrees_for_radian_units_only() {
        assert_eq!(angle_unit("rad").map(|x| x.0), Some("deg"));
        assert_eq!(angle_unit("rad/s").map(|x| x.0), Some("deg/s"));
        assert_eq!(angle_unit("Nm"), None);
        assert_eq!(angle_unit("deg"), None);
    }
}
