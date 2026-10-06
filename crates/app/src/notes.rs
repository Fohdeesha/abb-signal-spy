use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use eframe::egui::{self, RichText};

use spy_core::log::Level;

use crate::app::SpyApp;
use crate::fields;
use crate::theme;

pub const FILE: &str = "signal-notes.json";
const FORMAT: &str = "abb-signal-spy-notes";
const VERSION: u32 = 1;
pub const COLUMNS: [&str; 12] = ["signal", "name", "description", "units", "category", "indexing", "scope", "confidence", "evidence", "ruled_out", "open_question", "next_test"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Leaning {
    #[default]
    Open,
    Probable,
}

impl Leaning {
    pub fn label(self) -> &'static str {
        match self {
            Leaning::Open => "open",
            Leaning::Probable => "probable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Note {
    pub name: String,
    pub description: String,
    pub units: String,
    pub category: String,
    pub confidence: Leaning,
    pub evidence: String,
    pub ruled_out: String,
    pub open_question: String,
    pub next_test: String,
    pub saved: String,
}

impl Note {
    pub fn texts(&self) -> [(&'static str, &str); 8] {
        [
            ("name", &self.name),
            ("description", &self.description),
            ("units", &self.units),
            ("category", &self.category),
            ("evidence", &self.evidence),
            ("ruled_out", &self.ruled_out),
            ("open_question", &self.open_question),
            ("next_test", &self.next_test),
        ]
    }

    pub fn is_empty(&self) -> bool {
        self.texts().iter().all(|(_, t)| t.trim().is_empty())
    }
}

pub struct Notes {
    pub map: BTreeMap<u32, Note>,
    path: PathBuf,
    locked: Option<String>,
}

impl Notes {
    pub fn load(path: &Path) -> (Notes, Option<String>) {
        let mut notes = Notes { map: BTreeMap::new(), path: path.to_path_buf(), locked: None };
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (notes, None),
            Err(e) => {
                let why = format!("Your notes on signals ({}) could not be read ({e}); they are left as they are, and new notes cannot be saved until the program is started again.", path.display());
                notes.locked = Some(why.clone());
                return (notes, Some(why));
            }
        };
        let obj = match serde_json::from_str::<serde_json::Value>(spy_core::util::strip_bom(&text)) {
            Ok(serde_json::Value::Object(m)) if m.get("format").and_then(|f| f.as_str()).is_none_or(|f| f == FORMAT) => m,
            other => {
                let why = match other {
                    Err(e) => e.to_string(),
                    Ok(_) => "it is not a notes file".into(),
                };
                let stamp = spy_core::util::local_stamp(std::time::SystemTime::now());
                let aside = (1..1000)
                    .map(|i| path.with_extension(if i == 1 { format!("json.bad-{stamp}") } else { format!("json.bad-{stamp}-{i}") }))
                    .find(|p| !p.exists())
                    .unwrap_or_else(|| path.with_extension(format!("json.bad-{stamp}-last")));
                if let Err(e) = std::fs::rename(path, &aside) {
                    let why = format!("Your notes on signals ({}) could not be read ({why}), nor set aside ({e}); they are left as they are, and new notes cannot be saved until the file is moved.", path.display());
                    notes.locked = Some(why.clone());
                    return (notes, Some(why));
                }
                return (notes, Some(format!("Your notes on signals could not be read ({why}); the file was kept as {}.", aside.display())));
            }
        };
        let (map, bad) = parse(&obj);
        notes.map = map;
        let note = (!bad.is_empty()).then(|| format!("Some of your notes on signals ({}) could not be read and were left out: {}. The file keeps them.", path.display(), bad.join("; ")));
        (notes, note)
    }

    pub fn get(&self, n: u32) -> Option<&Note> {
        self.map.get(&n)
    }

    pub fn put(&mut self, n: u32, note: Note) -> Result<(), String> {
        if let Some(why) = &self.locked {
            return Err(why.clone());
        }
        let mut doc = match std::fs::read_to_string(&self.path) {
            Ok(text) => match serde_json::from_str::<serde_json::Value>(spy_core::util::strip_bom(&text)) {
                Ok(serde_json::Value::Object(m)) if m.get("format").and_then(|f| f.as_str()).is_none_or(|f| f == FORMAT) => m,
                _ => return Err(format!("{} was changed outside this window and cannot be read now; nothing was saved over it", self.path.display())),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let notes: serde_json::Map<String, serde_json::Value> = self.map.iter().map(|(n, note)| (n.to_string(), serde_json::to_value(note).unwrap_or_default())).collect();
                serde_json::Map::from_iter([("notes".to_string(), serde_json::Value::Object(notes))])
            }
            Err(e) => return Err(format!("cannot read {} ({e}); nothing was saved over it", self.path.display())),
        };
        let notes = doc.entry("notes").or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        if !notes.is_object() {
            *notes = serde_json::Value::Object(serde_json::Map::new());
        }
        if let Some(list) = notes.as_object_mut() {
            if note.is_empty() {
                list.remove(&n.to_string());
            } else {
                list.insert(n.to_string(), serde_json::to_value(&note).map_err(|e| e.to_string())?);
            }
        }
        doc.insert("format".into(), FORMAT.into());
        doc.insert("version".into(), VERSION.into());
        let text = serde_json::to_string_pretty(&serde_json::Value::Object(doc.clone())).map_err(|e| e.to_string())? + "\n";
        spy_core::util::write_whole(&self.path, text.as_bytes())?;
        self.map = parse(&doc).0;
        Ok(())
    }
}

fn parse(obj: &serde_json::Map<String, serde_json::Value>) -> (BTreeMap<u32, Note>, Vec<String>) {
    let mut map = BTreeMap::new();
    let mut bad: Vec<String> = Vec::new();
    if let Some(list) = obj.get("notes").and_then(|v| v.as_object()) {
        for (k, v) in list {
            match (k.parse::<u32>(), serde_json::from_value::<Note>(v.clone())) {
                (Ok(n), Ok(note)) if n > 0 => {
                    map.insert(n, note);
                }
                (_, Err(e)) => bad.push(format!("\"{k}\" ({e})")),
                _ => bad.push(format!("\"{k}\" (not a signal number)")),
            }
        }
    }
    (map, bad)
}

pub struct Origin<'a> {
    pub app_version: &'a str,
    pub date: &'a str,
    pub robotware: Option<&'a str>,
}

fn cell(s: &str) -> String {
    s.split(|c: char| c.is_control() || c.is_whitespace()).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
}

pub fn tsv(map: &BTreeMap<u32, Note>, o: &Origin) -> String {
    let mut out = String::new();
    out.push_str("# Notes on ABB test signals, written in ABB Signal Spy, in the columns of its catalogue's research files.\n");
    out.push_str(&format!("# Exported {} by ABB Signal Spy {}.\n", o.date, o.app_version));
    match o.robotware {
        Some(v) => out.push_str(&format!("# RobotWare {} (read from the controller's RWS).\n", cell(v))),
        None => out.push_str("# RobotWare: not known (not logged in to the controller's RWS when exported).\n"),
    }
    out.push_str("# The confidence is the note-taker's own: open, or probable.\n");
    out.push_str(&format!("# {}\n", COLUMNS.join("\t")));
    for (n, note) in map.iter().filter(|(_, note)| !note.is_empty()) {
        let row = [
            n.to_string(),
            cell(&note.name),
            cell(&note.description),
            cell(&note.units),
            cell(&note.category),
            String::new(),
            String::new(),
            note.confidence.label().to_string(),
            cell(&note.evidence),
            cell(&note.ruled_out),
            cell(&note.open_question),
            cell(&note.next_test),
        ];
        out.push_str(&row.join("\t"));
        out.push('\n');
    }
    out
}

pub fn private_text(map: &BTreeMap<u32, Note>) -> Vec<(u32, &'static str, String)> {
    let mut found = Vec::new();
    for (&n, note) in map {
        for (col, text) in note.texts() {
            if let Some(t) = find_private(text) {
                found.push((n, col, t));
            }
        }
    }
    found
}

fn find_private(s: &str) -> Option<String> {
    let b = s.as_bytes();
    for i in 0..b.len() {
        let before = if i == 0 { None } else { Some(b[i - 1]) };
        if let Some(len) = ipv4_at(b, i).filter(|_| !before.is_some_and(|c| c.is_ascii_digit() || c == b'.')) {
            return Some(s[i..i + len].to_string());
        }
        if !before.is_some_and(|c| c.is_ascii_hexdigit()) && guid_at(b, i) {
            return Some(s[i..i + 36].to_string());
        }
        let path = (b[i].is_ascii_alphabetic() && b.get(i + 1) == Some(&b':') && matches!(b.get(i + 2), Some(b'\\') | Some(b'/')) && !before.is_some_and(|c| c.is_ascii_alphanumeric()))
            || (b[i] == b'\\' && b.get(i + 1) == Some(&b'\\') && b.get(i + 2).is_some_and(|c| c.is_ascii_alphanumeric()));
        if path {
            let end = s[i..].find(char::is_whitespace).map_or(s.len(), |e| i + e);
            return Some(s[i..end].to_string());
        }
    }
    None
}

fn ipv4_at(b: &[u8], i: usize) -> Option<usize> {
    let mut j = i;
    for part in 0..4 {
        let start = j;
        while j < b.len() && b[j].is_ascii_digit() && j - start < 4 {
            j += 1;
        }
        let digits = j - start;
        if digits == 0 || digits > 3 || std::str::from_utf8(&b[start..j]).ok()?.parse::<u32>().ok()? > 255 {
            return None;
        }
        if part < 3 {
            if b.get(j) != Some(&b'.') {
                return None;
            }
            j += 1;
        }
    }
    let more = b.get(j).is_some_and(|c| c.is_ascii_digit()) || (b.get(j) == Some(&b'.') && b.get(j + 1).is_some_and(|c| c.is_ascii_digit()));
    (!more).then_some(j - i)
}

fn guid_at(b: &[u8], i: usize) -> bool {
    let Some(g) = b.get(i..i + 36) else { return false };
    let dash = |k: usize| matches!(k, 8 | 13 | 18 | 23);
    g.iter().enumerate().all(|(k, &c)| if dash(k) { c == b'-' } else { c.is_ascii_hexdigit() }) && !b.get(i + 36).is_some_and(|c| c.is_ascii_hexdigit())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NotesAsk {
    Discard,
    Delete,
}

pub struct Edit {
    pub signal: u32,
    pub draft: Note,
}

impl SpyApp {
    pub(crate) fn notes_section(&mut self, ui: &mut egui::Ui, n: u32) {
        ui.add_space(6.0);
        ui.separator();
        let note = self.notes.get(n).cloned();
        ui.horizontal_wrapped(|ui| {
            ui.label(crate::theme::b("your notes")).on_hover_text(format!(
                "Kept on this PC, beside the settings ({FILE}). Catalogue menu, \"export your notes...\": a file in the catalogue's own columns, to send to whoever keeps the catalogue."
            ));
            let text = if note.is_some() { "edit your notes..." } else { "add your notes..." };
            if ui.button(text).on_hover_text("What you found out about this signal: a name if you have one, what it seems to be, the evidence, what is ruled out, what is still open and the test that would settle it").clicked() {
                self.note_edit = Some(Edit { signal: n, draft: note.clone().unwrap_or_default() });
            }
        });
        let Some(note) = note else {
            ui.label(RichText::new("None yet.").small().weak());
            return;
        };
        ui.horizontal_wrapped(|ui| {
            if !note.name.is_empty() {
                ui.label(RichText::new(&note.name).strong());
            }
            ui.label(RichText::new(note.confidence.label()).color(crate::browser::confidence_color(match note.confidence {
                Leaning::Open => spy_core::catalogue::Confidence::Open,
                Leaning::Probable => spy_core::catalogue::Confidence::Probable,
            }, theme::pal(ui))));
            if !note.units.is_empty() {
                ui.label(format!("[{}]", note.units));
            }
            if !note.category.is_empty() {
                ui.label(RichText::new(&note.category).weak());
            }
        });
        if !note.description.is_empty() {
            ui.label(&note.description);
        }
        for (title, text) in [("evidence", &note.evidence), ("ruled out", &note.ruled_out), ("open question", &note.open_question), ("next test", &note.next_test)] {
            if !text.is_empty() {
                ui.label(RichText::new(format!("{title}: {text}")).small());
            }
        }
        if let Some((date, time)) = note.saved.split_once('_') {
            ui.label(RichText::new(format!("Saved {date} {}", time.replace('-', ":"))).small().weak());
        }
    }

    pub fn notes_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut e) = self.note_edit.take() else { return };
        let title = match self.catalogue.get(e.signal) {
            Some(s) if !s.name.is_empty() || s.abb.is_some() => format!("{} ({})", s.display_name(), e.signal),
            _ => format!("signal {}", e.signal),
        };
        let existed = self.notes.get(e.signal).is_some();
        let unchanged = self.notes.get(e.signal).cloned().unwrap_or_default() == e.draft;
        let ask_id = egui::Id::new("notes-editor-asking");
        let mut asking: Option<NotesAsk> = ctx.data(|d| d.get_temp(ask_id));
        let first_id = egui::Id::new("notes-editor-focused");
        let fresh = ctx.data(|d| d.get_temp::<u32>(first_id)) != Some(e.signal);
        let (mut save, mut delete, mut close) = (false, false, false);
        let room = (ctx.content_rect().height() - 230.0).max(140.0);
        let modal = egui::Modal::new(egui::Id::new("notes-editor")).show(ctx, |ui| {
            let p = theme::pal(ui);
            ui.set_width(560.0f32.min(ctx.content_rect().width() - 60.0));
            ui.heading(format!("your notes on {title}"));
            ui.label(RichText::new("Kept on this PC. Write what you would want the next person to know; leave out addresses and names of your cell (an export refuses them).").size(14.0).color(p.ink2));
            ui.add_space(4.0);
            let d = &mut e.draft;
            egui::ScrollArea::vertical().id_salt("notes-fields-scroll").max_height(room).auto_shrink([false, true]).show(ui, |ui| {
                egui::Grid::new("notes-fields").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                    let line = |ui: &mut egui::Ui, label: &str, hint: &str, text: &mut String| -> egui::Response {
                        let l = ui.label(label);
                        let r = fields::line(ui, text, label, |t| t.hint_text(hint).desired_width(f32::INFINITY)).labelled_by(l.id);
                        ui.end_row();
                        r
                    };
                    let name = line(ui, "name", "a name, if you have one", &mut d.name);
                    if fresh {
                        name.request_focus();
                    }
                    line(ui, "description", "what it seems to be", &mut d.description);
                    line(ui, "units", "e.g. rad, Nm, A", &mut d.units);
                    line(ui, "category", "e.g. motor, drive, unknown", &mut d.category);
                    ui.label("confidence");
                    ui.horizontal(|ui| {
                        if theme::chip(ui, d.confidence == Leaning::Open, "open").on_hover_text("It responds, but what it is remains open").clicked() {
                            d.confidence = Leaning::Open;
                        }
                        if theme::chip(ui, d.confidence == Leaning::Probable, "probable").on_hover_text("It fits, and no alternative survives, but it is not forced").clicked() {
                            d.confidence = Leaning::Probable;
                        }
                    });
                    ui.end_row();
                    let block = |ui: &mut egui::Ui, label: &str, hint: &str, text: &mut String| {
                        let l = ui.label(label);
                        fields::lines(ui, text, label, |t| t.hint_text(hint).desired_rows(2).desired_width(f32::INFINITY)).labelled_by(l.id);
                        ui.end_row();
                    };
                    block(ui, "evidence", "what was seen, and against what", &mut d.evidence);
                    block(ui, "ruled out", "what it is not, and why", &mut d.ruled_out);
                    block(ui, "open question", "what is still not known", &mut d.open_question);
                    block(ui, "next test", "the experiment that would settle it", &mut d.next_test);
                });
            });
            ui.add_space(8.0);
            match asking {
                Some(NotesAsk::Discard) => {
                    ui.label(theme::b("Close without saving what you typed?").color(p.hold));
                    ui.horizontal(|ui| {
                        if theme::red_button(ui, egui::Button::new("discard it").min_size(egui::vec2(0.0, fields::HEIGHT))).clicked() {
                            close = true;
                        }
                        if ui.add(egui::Button::new("keep editing").min_size(egui::vec2(0.0, fields::HEIGHT))).clicked() {
                            asking = None;
                        }
                    });
                }
                Some(NotesAsk::Delete) => {
                    ui.label(theme::b(format!("Delete your notes on {title}? This cannot be undone.")).color(p.red));
                    ui.horizontal(|ui| {
                        if theme::red_button(ui, egui::Button::new("delete them").min_size(egui::vec2(0.0, fields::HEIGHT))).clicked() {
                            delete = true;
                        }
                        if ui.add(egui::Button::new("keep them").min_size(egui::vec2(0.0, fields::HEIGHT))).clicked() {
                            asking = None;
                        }
                    });
                }
                None => {
                    ui.horizontal(|ui| {
                        if theme::primary(ui, "save notes", fields::HEIGHT).clicked() {
                            save = true;
                        }
                        if ui.add(egui::Button::new("cancel").min_size(egui::vec2(0.0, fields::HEIGHT))).clicked() {
                            close = true;
                        }
                        if existed && theme::red_button(ui, egui::Button::new("delete these notes").min_size(egui::vec2(0.0, fields::HEIGHT))).clicked() {
                            asking = Some(NotesAsk::Delete);
                        }
                    });
                }
            }
        });
        if modal.should_close() && asking.is_none() {
            if unchanged {
                close = true;
            } else {
                asking = Some(NotesAsk::Discard);
            }
        }
        ctx.data_mut(|d| d.insert_temp(first_id, e.signal));
        if save || delete {
            let n = e.signal;
            let note = if delete { Note::default() } else { Note { saved: spy_core::util::local_stamp(std::time::SystemTime::now()), ..e.draft.clone() } };
            let empty = note.is_empty();
            match self.notes.put(n, note) {
                Ok(()) => {
                    self.toast(Level::Info, if delete || empty { format!("Your notes on {n} are deleted.") } else { format!("Your notes on {n} are saved.") });
                    close = true;
                }
                Err(why) => self.toast(Level::Error, format!("Your notes on {n} were not saved: {why}")),
            }
        }
        if close {
            ctx.data_mut(|d| {
                d.remove::<u32>(first_id);
                d.remove::<NotesAsk>(ask_id);
            });
        } else {
            ctx.data_mut(|d| match asking {
                Some(a) => {
                    d.insert_temp(ask_id, a);
                }
                None => {
                    d.remove::<NotesAsk>(ask_id);
                }
            });
            self.note_edit = Some(e);
        }
    }

    pub fn export_notes(&mut self) {
        if self.notes.map.values().all(Note::is_empty) {
            self.toast(Level::Warn, "No notes to export yet: add yours in a signal's details.");
            return;
        }
        let private = private_text(&self.notes.map);
        if !private.is_empty() {
            let list = private.iter().take(4).map(|(n, col, t)| format!("{n} ({}): \"{t}\"", col.replace('_', " "))).collect::<Vec<_>>().join("; ");
            let more = if private.len() > 4 { format!(" and {} more", private.len() - 4) } else { String::new() };
            let why = format!("Not exported: your notes hold what identifies a controller or a PC (an address, a system id or a path), and the file is meant to be shared. Take it out of {list}{more}, then export again.");
            self.log.warn(why.clone());
            self.show_toast(Level::Error, why);
            return;
        }
        let now = std::time::SystemTime::now();
        let date = match spy_core::util::local_parts(now) {
            Some((y, mo, d, h, mi, _, _)) => format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}"),
            None => format!("{} UTC", &spy_core::util::wall_iso(now)[..16].replace('T', " ")),
        };
        let robotware = self.rws.as_ref().and_then(|l| l.system.as_ref()).map(|s| s.rw_version.clone()).filter(|v| !v.is_empty());
        let text = tsv(&self.notes.map, &Origin { app_version: env!("CARGO_PKG_VERSION"), date: &date, robotware: robotware.as_deref() });
        let dir = self.record_dir();
        let written = std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display())).and_then(|()| {
            let stem = format!("{} signal-notes", spy_core::util::local_stamp(now));
            let path = (1..1000).map(|i| dir.join(if i == 1 { format!("{stem}.tsv") } else { format!("{stem} ({i}).tsv") })).find(|p| !p.exists()).ok_or_else(|| format!("no free file name in {}", dir.display()))?;
            std::fs::write(&path, text).map(|()| path.clone()).map_err(|e| format!("cannot write {}: {e}", path.display()))
        });
        match written {
            Ok(path) => {
                let n = self.notes.map.values().filter(|x| !x.is_empty()).count();
                self.toast(Level::Info, format!("Exported your notes on {n} signal(s) to {}.", path.display()));
            }
            Err(e) => self.toast(Level::Error, format!("Your notes were not exported: {e}")),
        }
    }

    pub(crate) fn export_notes_button(&mut self, ui: &mut egui::Ui) {
        let any = !self.notes.map.is_empty();
        if theme::lockable(ui, any, egui::Button::new("export your notes..."))
            .on_hover_text("Your notes on signals, as a file in the catalogue's own columns (in the recordings folder), to send to whoever keeps the catalogue. It names this program's version, the date and the RobotWare version (when logged in to RWS), nothing else about the controller.")
            .on_disabled_hover_text("No notes yet: add yours in a signal's details.")
            .clicked()
        {
            self.export_notes();
            ui.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spy_core::testdir::TestDir;

    fn temp(tag: &str) -> (TestDir, PathBuf) {
        let d = TestDir::new(&format!("notes-{tag}"));
        let p = d.join(FILE);
        (d, p)
    }

    fn note(name: &str, evidence: &str) -> Note {
        Note { name: name.into(), evidence: evidence.into(), ..Note::default() }
    }

    #[test]
    fn notes_survive_a_save_and_a_load_and_an_empty_one_is_removed() {
        let (_dir, p) = temp("round");
        let (mut notes, msg) = Notes::load(&p);
        assert!(msg.is_none() && notes.map.is_empty(), "no file yet is no notes, and nothing to say");
        let full = Note { name: "gear wind-up".into(), description: "d".into(), units: "rad".into(), category: "motor".into(), confidence: Leaning::Probable, evidence: "e".into(), ruled_out: "r".into(), open_question: "o".into(), next_test: "n".into(), saved: "2026-09-29_14-03-07".into() };
        notes.put(6914, full.clone()).unwrap();
        notes.put(5015, note("", "only evidence")).unwrap();
        let (back, msg) = Notes::load(&p);
        assert!(msg.is_none(), "{msg:?}");
        assert_eq!(back.get(6914), Some(&full), "every field kept");
        assert_eq!(back.map.len(), 2);
        notes.put(5015, Note { confidence: Leaning::Probable, ..Note::default() }).unwrap();
        assert_eq!(Notes::load(&p).0.map.keys().copied().collect::<Vec<_>>(), vec![6914]);
    }

    #[test]
    fn a_bad_note_costs_that_note_and_a_bad_file_is_set_aside_not_overwritten() {
        let (_dir, p) = temp("bad");
        std::fs::write(&p, r#"{"format":"abb-signal-spy-notes","version":1,"notes":{"1403":{"name":"x"},"abc":{"name":"y"},"0":{},"5007":{"confidence":"certain"}}}"#).unwrap();
        let (notes, msg) = Notes::load(&p);
        assert_eq!(notes.map.keys().copied().collect::<Vec<_>>(), vec![1403]);
        let msg = msg.expect("the person is told");
        assert!(msg.contains("\"abc\"") && msg.contains("\"0\"") && msg.contains("\"5007\""), "{msg}");
        std::fs::write(&p, "{ not json").unwrap();
        let (mut notes, msg) = Notes::load(&p);
        assert!(msg.unwrap().contains("kept as"));
        assert!(!p.exists(), "set aside, not left to be overwritten");
        let aside: Vec<_> = std::fs::read_dir(p.parent().unwrap()).unwrap().filter_map(|e| e.ok()).filter(|e| e.file_name().to_string_lossy().contains(".bad-")).collect();
        assert_eq!(aside.len(), 1);
        assert_eq!(std::fs::read_to_string(aside[0].path()).unwrap(), "{ not json");
        notes.put(1, note("a", "")).unwrap();
        assert!(p.exists());
        std::fs::write(&p, r#"{"format":"abb-signal-spy-catalogue","signals":[]}"#).unwrap();
        assert!(Notes::load(&p).1.unwrap().contains("not a notes file"));
        let kept: Vec<String> = std::fs::read_dir(p.parent().unwrap()).unwrap().filter_map(|e| e.ok()).filter(|e| e.file_name().to_string_lossy().contains(".bad-")).map(|e| std::fs::read_to_string(e.path()).unwrap()).collect();
        assert_eq!(kept.len(), 2, "{kept:?}");
        assert!(kept.contains(&"{ not json".to_string()), "the first file set aside was lost");
    }

    #[test]
    fn two_windows_keep_each_others_notes() {
        let (_dir, p) = temp("two");
        let (mut a, _) = Notes::load(&p);
        let (mut b, _) = Notes::load(&p);
        a.put(1403, note("a", "seen in window A")).unwrap();
        b.put(5007, note("b", "seen in window B")).unwrap();
        let back = Notes::load(&p).0;
        assert_eq!(back.map.keys().copied().collect::<Vec<_>>(), vec![1403, 5007], "a note saved in one window was lost when the other saved");
        assert_eq!(b.map.len(), 2, "the window that saved last sees both");
        a.put(1403, Note::default()).unwrap();
        assert_eq!(Notes::load(&p).0.map.keys().copied().collect::<Vec<_>>(), vec![5007]);
    }

    #[test]
    fn a_note_that_could_not_be_read_is_kept_through_a_save() {
        let (_dir, p) = temp("keep-bad");
        std::fs::write(&p, r#"{"format":"abb-signal-spy-notes","version":1,"notes":{"1403":{"name":"x"},"5007":{"confidence":"certain"}}}"#).unwrap();
        let (mut notes, msg) = Notes::load(&p);
        assert!(msg.unwrap().contains("\"5007\""));
        notes.put(6914, note("y", "")).unwrap();
        let raw = std::fs::read_to_string(&p).unwrap();
        assert!(raw.contains("\"5007\"") && raw.contains("certain"), "a note this version cannot read was dropped: {raw}");
        std::fs::write(&p, "{ broken by hand").unwrap();
        let e = notes.put(1, note("z", "")).unwrap_err();
        assert!(e.contains("cannot be read now"), "{e}");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{ broken by hand", "saved over");
    }

    #[test]
    fn a_file_that_cannot_be_read_is_never_overwritten() {
        let (_dir, p) = temp("locked");
        std::fs::create_dir_all(&p).unwrap();
        let (mut notes, msg) = Notes::load(&p);
        assert!(msg.unwrap().contains("could not be read"));
        let e = notes.put(1, note("a", "")).unwrap_err();
        assert!(e.contains("cannot be saved"), "{e}");
        assert!(notes.map.is_empty(), "a failed save changes nothing");
    }

    #[test]
    fn the_export_is_the_research_files_schema_one_line_per_signal() {
        let mut map = BTreeMap::new();
        map.insert(6914, Note { name: "".into(), description: "tiny\tposition-like".into(), category: "unknown".into(), evidence: "line one\nline two\r\n  indented".into(), next_test: "hang a payload".into(), ..Note::default() });
        map.insert(1403, Note { name: "a guess".into(), confidence: Leaning::Probable, units: "rad".into(), ..Note::default() });
        map.insert(99, Note::default());
        let t = tsv(&map, &Origin { app_version: "0.1.0", date: "2026-09-29 14:03", robotware: Some("6.16.2027") });
        let lines: Vec<&str> = t.lines().collect();
        assert!(lines[..5].iter().all(|l| l.starts_with('#')), "{t}");
        assert!(t.contains("# Exported 2026-09-29 14:03 by ABB Signal Spy 0.1.0.\n# RobotWare 6.16.2027 (read from the controller's RWS).\n"), "{t}");
        assert_eq!(lines[4], "# signal\tname\tdescription\tunits\tcategory\tindexing\tscope\tconfidence\tevidence\truled_out\topen_question\tnext_test");
        assert_eq!(lines.len(), 7, "the empty note is not a row: {t}");
        assert_eq!(lines[5], "1403\ta guess\t\trad\t\t\t\tprobable\t\t\t\t", "in number order");
        let row: Vec<&str> = lines[6].split('\t').collect();
        assert_eq!(row.len(), 12, "a tab or a line break inside a field splits the row");
        assert_eq!((row[0], row[2], row[4], row[7], row[8], row[11]), ("6914", "tiny position-like", "unknown", "open", "line one line two indented", "hang a payload"));
        let unknown = tsv(&map, &Origin { app_version: "0.1.0", date: "d", robotware: None });
        assert!(unknown.contains("# RobotWare: not known"), "{unknown}");
    }

    #[test]
    fn what_identifies_a_controller_or_a_pc_is_found() {
        for (text, found) in [
            ("seen on 192.0.2.77 at the cell", "192.0.2.77"),
            ("10.0.0.2", "10.0.0.2"),
            ("system {12345678-9ABC-4DEF-8123-456789ABCDEF} on the VC", "12345678-9ABC-4DEF-8123-456789ABCDEF"),
            ("from C:\\Users\\someone\\run.csv here", "C:\\Users\\someone\\run.csv"),
            ("on \\\\server\\share\\x", "\\\\server\\share\\x"),
        ] {
            let map = BTreeMap::from([(7, note("", text))]);
            assert_eq!(private_text(&map), vec![(7, "evidence", found.to_string())], "{text}");
        }
        for text in ["RobotWare 6.16.2027", "com_offset 1.5707999", "2.6% per 10 s; tau 385 s", "on 2026-09-29, 4.032 ms", "6.16.2027.1", "999.1.1.1", "1.2.3", "1.2.3.4.5", "a:b", "gear ratio 1:120"] {
            let map = BTreeMap::from([(7, note("", text))]);
            assert!(private_text(&map).is_empty(), "{text}: {:?}", private_text(&map));
        }
        let map = BTreeMap::from([(1, note("at 192.168.125.1", "")), (2, Note { next_test: "on D:\\data".into(), ..Note::default() })]);
        assert_eq!(private_text(&map).iter().map(|(n, c, _)| (*n, *c)).collect::<Vec<_>>(), vec![(1, "name"), (2, "next_test")]);
    }
}
