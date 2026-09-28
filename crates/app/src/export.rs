//! Saving what is in view (Phase 2): the samples of the charted stretch as one CSV
//! file, and the charts as a PNG. Both go to the recordings folder, named for the
//! local time, and the person is told where.
//!
//! The CSV describes itself, one row per sample: `time_utc,t_s,channel,name,units,
//! value`. Values are as sent, in the unit the row names (degrees where the window
//! shows degrees); a zero-filled signal keeps its padding zeros, as a recording does.
//! Text signals' values are quoted, as in a recording.
//!
//! A view can hold millions of samples (ten minutes of twelve channels live, or a
//! long recording), so the file is written in the background, under a `.part` name
//! that becomes the real one only once every row is on disk.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;

use eframe::egui;

use spy_core::log::Level;
use spy_core::util::{local_stamp, wall_iso};

use crate::app::SpyApp;
use crate::view;

/// Above this many samples the person is told the save has started.
const TELL_FROM: usize = 200_000;

/// A CSV being written: the file it becomes, and the rows written.
pub type ExportJob = JoinHandle<Result<(PathBuf, usize), String>>;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Number(f64),
    Text(String),
    /// A slow log's interval: its mean, minimum, maximum and sample count.
    Interval { mean: f64, min: f64, max: f64, count: u64 },
}

/// One channel's samples in the stretch, in time order.
pub struct Series<'a> {
    pub id: String,
    pub name: String,
    pub units: String,
    pub samples: Box<dyn Iterator<Item = (i64, Value)> + Send + 'a>,
}

fn quoted(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// A field as it is where it is safe, quoted where it would break its row (a comma,
/// a quote, a line break) or read as a formula in a spreadsheet.
fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\r', '\n']) || s.starts_with(['=', '+', '-', '@', '\t']) { quoted(s) } else { s.to_string() }
}

fn number(v: f64) -> String {
    if v.is_nan() { "NaN".to_string() } else { format!("{v}") }
}

/// Write the series to `out`, merged into time order (ties in id order). `from` is
/// the stretch's start (`t_s` counts from it); `utc_offset` turns a sample's time
/// into UTC ms since 1970, where the wall clock is known. Returns the rows written.
#[cfg(test)]
pub fn write_rows(out: &mut impl Write, from: i64, utc_offset: Option<i64>, series: Vec<Series>) -> std::io::Result<usize> {
    write_rows_until(out, from, utc_offset, series, &std::sync::atomic::AtomicBool::new(false))
}

/// [`write_rows`], stopping with an error once `stop` is set (the window closing).
/// A slow log's intervals get their minimum, maximum and count beside their mean.
fn write_rows_until(out: &mut impl Write, from: i64, utc_offset: Option<i64>, mut series: Vec<Series>, stop: &std::sync::atomic::AtomicBool) -> std::io::Result<usize> {
    series.sort_by(|a, b| a.id.cmp(&b.id));
    let mut heads: Vec<Option<(i64, Value)>> = series.iter_mut().map(|s| s.samples.next()).collect();
    let intervals = heads.iter().flatten().any(|(_, v)| matches!(v, Value::Interval { .. }));
    out.write_all(if intervals { b"time_utc,t_s,channel,name,units,mean,min,max,count\n" } else { b"time_utc,t_s,channel,name,units,value\n" })?;
    // Written once per series, not once per row. The window's labels space their
    // parts widely; a file gets single spaces.
    let tidy = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let fixed: Vec<String> = series.iter().map(|s| format!("{},{},{}", csv_field(&s.id), quoted(&tidy(&s.name)), quoted(&s.units))).collect();
    let mut rows = 0;
    // At most a couple of dozen series: the earliest head by a straight look.
    while let Some(i) = (0..heads.len()).filter(|&i| heads[i].is_some()).min_by_key(|&i| heads[i].as_ref().map(|h| h.0)) {
        if rows % 4096 == 0 && stop.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(std::io::Error::other("stopped: the window was closing"));
        }
        let Some((t, v)) = std::mem::replace(&mut heads[i], series[i].samples.next()) else { break };
        let when = utc_offset.and_then(|o| t.checked_add(o)).and_then(utc_text).unwrap_or_default();
        let blank = if intervals { ",,," } else { "" };
        let value = match v {
            Value::Number(v) => format!("{}{blank}", number(v)),
            Value::Text(s) => format!("{}{blank}", quoted(&s)),
            Value::Interval { mean, min, max, count } => format!("{},{},{},{count}", number(mean), number(min), number(max)),
        };
        writeln!(out, "{when},{:.3},{},{value}", (t - from) as f64 / 1000.0, fixed[i])?;
        rows += 1;
    }
    Ok(rows)
}

/// UTC ms since 1970 as text; `None` for a time no clock shows (Windows' starts in
/// 1601 and ends in 30827: past either, SystemTime arithmetic panics).
fn utc_text(ms: i64) -> Option<String> {
    let d = std::time::Duration::from_millis(ms.unsigned_abs());
    let t = if ms >= 0 { std::time::UNIX_EPOCH.checked_add(d) } else { std::time::UNIX_EPOCH.checked_sub(d) }?;
    Some(wall_iso(t))
}

/// Write the series to `path` through `path.part`: the finished name appears only
/// once the file is complete, and a failed save leaves nothing behind.
#[cfg(test)]
pub fn write_csv(path: &Path, from: i64, utc_offset: Option<i64>, series: Vec<Series>) -> Result<usize, String> {
    write_csv_until(path, from, utc_offset, series, &std::sync::atomic::AtomicBool::new(false))
}

/// [`write_csv`], stopped once `stop` is set: nothing is left behind then either.
pub fn write_csv_until(path: &Path, from: i64, utc_offset: Option<i64>, series: Vec<Series>, stop: &std::sync::atomic::AtomicBool) -> Result<usize, String> {
    let part = part_name(path);
    let io = |e: std::io::Error| format!("writing {} failed: {e}", part.display());
    let f = std::fs::OpenOptions::new().write(true).create_new(true).open(&part).map_err(|e| format!("cannot create {}: {e}", part.display()))?;
    let result = (|| {
        let mut out = std::io::BufWriter::with_capacity(1 << 20, f);
        let rows = write_rows_until(&mut out, from, utc_offset, series, stop).map_err(io)?;
        let f = out.into_inner().map_err(|e| io(e.into_error()))?;
        f.sync_all().map_err(io)?;
        drop(f);
        std::fs::rename(&part, path).map_err(|e| format!("cannot rename {} to {}: {e}", part.display(), path.display()))?;
        Ok(rows)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    result
}

fn part_name(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

/// The part of a screenshot inside `rect` (in points), as PNG bytes.
pub fn crop_png(image: &egui::ColorImage, rect: egui::Rect, pixels_per_point: f32) -> Result<Vec<u8>, String> {
    let [w, h] = image.size;
    let x0 = ((rect.min.x * pixels_per_point).floor().max(0.0) as usize).min(w);
    let y0 = ((rect.min.y * pixels_per_point).floor().max(0.0) as usize).min(h);
    let x1 = ((rect.max.x * pixels_per_point).ceil().max(0.0) as usize).min(w);
    let y1 = ((rect.max.y * pixels_per_point).ceil().max(0.0) as usize).min(h);
    if x1 <= x0 || y1 <= y0 {
        return Err("the charts were not on screen".into());
    }
    let mut bytes = Vec::with_capacity((x1 - x0) * (y1 - y0) * 4);
    for y in y0..y1 {
        for px in &image.pixels[y * w + x0..y * w + x1] {
            bytes.extend_from_slice(&px.to_srgba_unmultiplied());
        }
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, (x1 - x0) as u32, (y1 - y0) as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().map_err(|e| e.to_string())?;
        w.write_image_data(&bytes).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

impl SpyApp {
    /// A new file in the recordings folder, named for now, the label and what it is:
    /// a reviewed recording's own label while one is open (its rows carry no
    /// controller or system id to tell them apart), the REC label otherwise.
    fn export_path(&self, what: &str, ext: &str) -> Result<PathBuf, String> {
        let dir = self.record_dir();
        std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let label = self.review.as_ref().map_or(self.rec_label.as_str(), |rs| rs.review.meta.label.as_str());
        let label: String = label.chars().map(|c| if c.is_alphanumeric() || " -_.".contains(c) { c } else { '_' }).take(60).collect();
        let label = label.trim_matches(|c: char| c == ' ' || c == '.').to_string();
        let stem = if label.is_empty() { format!("{} {what}", local_stamp(std::time::SystemTime::now())) } else { format!("{} {label} {what}", local_stamp(std::time::SystemTime::now())) };
        for i in 1..1000 {
            let p = dir.join(if i == 1 { format!("{stem}.{ext}") } else { format!("{stem} ({i}).{ext}") });
            if !p.exists() && !part_name(&p).exists() {
                return Ok(p);
            }
        }
        Err(format!("no free file name in {}", dir.display()))
    }

    fn export_busy(&mut self) -> bool {
        if self.export_job.is_some() {
            self.toast(Level::Warn, "Still saving the last file; try again when it is done.");
        }
        self.export_job.is_some()
    }

    /// The live charts' stretch in view, every channel's samples, to a CSV.
    pub fn export_live_csv(&mut self) {
        if self.export_busy() {
            return;
        }
        let Some((from, to)) = self.view_ms else {
            self.toast(Level::Warn, "Nothing charted yet to save.");
            return;
        };
        let utc_offset = self.session.status().timeline.anchor().and_then(|(at, w)| w.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_millis() as i64 - at));
        let mut series = Vec::new();
        let mut skipped_text = false;
        let mut total = 0;
        for c in &self.chans {
            let Some(ch) = self.session.store().get(&c.key) else { continue };
            let sig = self.catalogue.get(c.key.signal);
            let d = view::display(sig, c.radians);
            // Copied out under the lock, briefly: the worker files samples into it.
            let samples: Vec<(i64, f64)> = {
                let r = ch.lock();
                if r.kind == Some(spy_core::sample::ValueKind::String) {
                    skipped_text = true;
                    continue;
                }
                r.range(from, to).collect()
            };
            total += samples.len();
            let factor = d.factor;
            series.push(Series { id: c.key.id(), name: view::label(&self.catalogue, &c.key), units: d.units.clone(), samples: Box::new(samples.into_iter().map(move |(t, v)| (t, Value::Number(v * factor)))) });
        }
        // Derived channels, as computed here; their ids say what from.
        for d in &self.derived {
            let def = d.live.def();
            let samples: Vec<(i64, f64)> = d.live.lock().range(from, to).collect();
            total += samples.len();
            series.push(Series { id: def.id(), name: crate::derived_view::file_name(def), units: def.units().into(), samples: Box::new(samples.into_iter().map(|(t, v)| (t, Value::Number(v)))) });
        }
        let note = if skipped_text { " (text signals are not kept in the live history: record them with REC)" } else { "" };
        self.start_export(total, note, move |p, stop| write_csv_until(p, from, utc_offset, series, stop));
    }

    /// The reviewed recording's stretch in view, to a CSV.
    pub fn export_review_csv(&mut self) {
        if self.export_busy() {
            return;
        }
        let Some(rs) = &self.review else { return };
        let r = rs.review.clone();
        let (from, to) = (r.start + (rs.view.0 * 1000.0).floor() as i64, r.start + (rs.view.1 * 1000.0).ceil() as i64 + 1);
        let names: Vec<(String, f64, String)> = r
            .channels
            .iter()
            .map(|ch| {
                let (units, factor) = crate::review_view::display_of(ch);
                // A derived channel with the setting it was computed with.
                let name = ch.derived.as_ref().map_or_else(|| crate::review_view::name_of(&self.catalogue, ch), crate::derived_view::file_name);
                (units, factor, name)
            })
            .collect();
        let within = |t: &[i64]| t.partition_point(|&x| x < to) - t.partition_point(|&x| x < from);
        let total = r.channels.iter().map(|ch| within(&ch.t) + ch.text.iter().filter(|(t, _)| (from..to).contains(t)).count()).sum();
        let utc_offset = r.wall_clock.then_some(0);
        // The recording is shared, not copied: the thread reads it where it is.
        self.start_export(total, "", move |p, stop| {
            let series = r
                .channels
                .iter()
                .zip(names)
                .map(|(ch, (units, factor, name))| {
                    let mut text: Vec<(i64, Value)> = ch.text.iter().filter(|(t, _)| (from..to).contains(t)).map(|(t, s)| (*t, Value::Text(s.clone()))).collect();
                    // Text rows are kept as they came; the merge needs them in order.
                    text.sort_by_key(|(t, _)| *t);
                    let numbers = recorded_values(ch, from, to, factor);
                    let units = if ch.text.is_empty() { units } else { String::new() };
                    Series { id: ch.id.clone(), name, units, samples: Box::new(merge_two(numbers, text.into_iter())) }
                })
                .collect();
            write_csv_until(p, from, utc_offset, series, stop)
        });
    }

    fn start_export(&mut self, total: usize, note: &'static str, write: impl FnOnce(&Path, &std::sync::atomic::AtomicBool) -> Result<usize, String> + Send + 'static) {
        if total == 0 {
            self.toast(Level::Warn, "No samples in view to save.");
            return;
        }
        let path = match self.export_path("view", "csv") {
            Ok(p) => p,
            Err(e) => return self.toast(Level::Error, format!("Could not save: {e}")),
        };
        if total >= TELL_FROM {
            self.toast(Level::Info, format!("Saving {total} samples to {} ...", path.display()));
        }
        let ctx = self.ctx.clone();
        self.export_note = note;
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.export_stop = stop.clone();
        self.export_job = Some(std::thread::spawn(move || {
            let r = write(&path, &stop).map(|n| (path, n));
            ctx.request_repaint();
            r
        }));
    }

    pub fn poll_export(&mut self) {
        if !self.export_job.as_ref().is_some_and(|j| j.is_finished()) {
            return;
        }
        let Some(job) = self.export_job.take() else { return };
        match job.join() {
            Ok(Ok((p, n))) => {
                self.toast(Level::Info, format!("Saved {n} samples in view to {}{}", p.display(), self.export_note));
                self.last_folder = p.parent().map(Path::to_path_buf);
            }
            Ok(Err(e)) => self.toast(Level::Error, format!("Could not save: {e}")),
            Err(_) => self.toast(Level::Error, "Saving failed unexpectedly."),
        }
    }

    /// Ask the window for a screenshot; the charts' part is saved when it arrives.
    pub fn request_png(&mut self) {
        if self.charts_rect.is_none() {
            self.toast(Level::Warn, "The charts are not on screen.");
            return;
        }
        self.png_pending = true;
        self.ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
    }

    /// A screenshot arrived: crop it to the charts and save it.
    pub fn take_screenshot(&mut self, ctx: &egui::Context) {
        if !self.png_pending {
            return;
        }
        let shot = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        let Some(image) = shot else { return };
        self.png_pending = false;
        let Some(rect) = self.charts_rect else { return };
        let result = crop_png(&image, rect, ctx.pixels_per_point()).and_then(|bytes| {
            let p = self.export_path("charts", "png")?;
            std::fs::write(&p, bytes).map_err(|e| format!("cannot write {}: {e}", p.display()))?;
            Ok(p)
        });
        match result {
            Ok(p) => {
                self.toast(Level::Info, format!("Saved the charts to {}", p.display()));
                self.last_folder = p.parent().map(Path::to_path_buf);
            }
            Err(e) => self.toast(Level::Error, format!("Could not save the picture: {e}")),
        }
    }
}

/// A recorded channel's values over `[from, to)` as recorded (a held signal's padding
/// kept), in its display unit: a slow log's intervals with their minimum, maximum and
/// count beside the mean.
fn recorded_values<'a>(ch: &'a spy_core::review::ReviewChannel, from: i64, to: i64, factor: f64) -> Box<dyn Iterator<Item = (i64, Value)> + Send + 'a> {
    match (&ch.band, &ch.counts) {
        (Some((lo, hi)), Some(counts)) => {
            let a = ch.t.partition_point(|&x| x < from);
            let b = ch.t.partition_point(|&x| x < to);
            Box::new((a..b).map(move |i| (ch.t[i], Value::Interval { mean: ch.v[i] * factor, min: lo[i] * factor, max: hi[i] * factor, count: counts[i] })))
        }
        _ => Box::new(ch.recorded(from, to).map(move |(t, v)| (t, Value::Number(v * factor)))),
    }
}

/// Two time-ordered streams as one.
fn merge_two<'a>(a: impl Iterator<Item = (i64, Value)> + Send + 'a, b: impl Iterator<Item = (i64, Value)> + Send + 'a) -> impl Iterator<Item = (i64, Value)> + Send + 'a {
    let (mut a, mut b) = (a.peekable(), b.peekable());
    std::iter::from_fn(move || match (a.peek(), b.peek()) {
        (Some(x), Some(y)) if y.0 < x.0 => b.next(),
        (Some(_), _) => a.next(),
        (None, _) => b.next(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbers(v: Vec<(i64, f64)>) -> Box<dyn Iterator<Item = (i64, Value)> + Send> {
        Box::new(v.into_iter().map(|(t, v)| (t, Value::Number(v))))
    }

    #[test]
    fn a_view_saves_as_rows_that_describe_themselves() {
        let series = vec![
            Series { id: "6000/ROB_1/J1".into(), name: "Joint reference (EGM)  ROB_1 J1".into(), units: "deg".into(), samples: numbers(vec![(1004, 12.5), (1008, f64::NAN)]) },
            Series { id: "4002/ROB_1/J1".into(), name: "Torque, \"J1\"".into(), units: "Nm".into(), samples: numbers(vec![(1000, -3.25), (1004, 7.0)]) },
            Series { id: "9872/ROB_1/J1".into(), name: "Work object".into(), units: String::new(), samples: Box::new(vec![(1002, Value::Text("wobj0, \"a\"".into()))].into_iter()) },
        ];
        let mut out = Vec::new();
        assert_eq!(write_rows(&mut out, 1000, Some(1_790_412_439_500), series).unwrap(), 5);
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "time_utc,t_s,channel,name,units,value");
        assert_eq!(lines[1], "2026-09-26T08:47:20.500Z,0.000,4002/ROB_1/J1,\"Torque, \"\"J1\"\"\",\"Nm\",-3.25", "in time order, names quoted");
        assert_eq!(lines[2], "2026-09-26T08:47:20.502Z,0.002,9872/ROB_1/J1,\"Work object\",\"\",\"wobj0, \"\"a\"\"\"", "text quoted");
        assert_eq!(lines[3], "2026-09-26T08:47:20.504Z,0.004,4002/ROB_1/J1,\"Torque, \"\"J1\"\"\",\"Nm\",7", "a tie in id order");
        assert_eq!(lines[4], "2026-09-26T08:47:20.504Z,0.004,6000/ROB_1/J1,\"Joint reference (EGM) ROB_1 J1\",\"deg\",12.5", "single spaces");
        assert_eq!(lines[5], "2026-09-26T08:47:20.508Z,0.008,6000/ROB_1/J1,\"Joint reference (EGM) ROB_1 J1\",\"deg\",NaN");
        assert_eq!(lines.len(), 6);

        // No wall clock: no time_utc.
        let mut out = Vec::new();
        write_rows(&mut out, 1000, None, vec![Series { id: "4002/ROB_1/J1".into(), name: String::new(), units: String::new(), samples: numbers(vec![(1250, 1.0)]) }]).unwrap();
        assert_eq!(String::from_utf8(out).unwrap().lines().nth(1), Some(",0.250,4002/ROB_1/J1,\"\",\"\",1"));
    }

    #[test]
    fn an_id_that_needs_quotes_gets_them_and_times_past_any_clock_do_not_crash() {
        // A damaged recording can name a channel anything; a row keeps its six fields.
        let mut out = Vec::new();
        write_rows(&mut out, 0, None, vec![Series { id: "x,y\n\"z\"".into(), name: "n".into(), units: String::new(), samples: numbers(vec![(0, 1.0)]) }]).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text.lines().nth(1), Some(",0.000,\"x,y"), "the id's line break stays inside its quotes");
        assert!(text.contains("\"x,y\n\"\"z\"\"\",\"n\",\"\",1"), "{text}");
        let mut out = Vec::new();
        write_rows(&mut out, 0, None, vec![Series { id: "=HYPERLINK(1)".into(), name: "n".into(), units: String::new(), samples: numbers(vec![(0, 1.0)]) }]).unwrap();
        assert!(String::from_utf8(out).unwrap().contains(",\"=HYPERLINK(1)\","), "a formula-like id quoted");
        // Before 1601 (Windows' clock starts there) and past its range: no time, no panic.
        for offset in [-20_000_000_000_000i64, i64::MAX - 5, i64::MIN / 2] {
            let mut out = Vec::new();
            write_rows(&mut out, 0, Some(offset), vec![Series { id: "a".into(), name: "n".into(), units: String::new(), samples: numbers(vec![(1, 1.0)]) }]).unwrap();
            assert!(String::from_utf8(out).unwrap().lines().nth(1).unwrap().starts_with(",0.001,"), "{offset}");
        }
    }

    #[test]
    fn a_slow_logs_intervals_save_with_their_extremes_and_counts() {
        let mut out = Vec::new();
        let rows = vec![(1000, Value::Interval { mean: 356.5, min: 300.0, max: 357.1, count: 250 })];
        write_rows(&mut out, 1000, None, vec![Series { id: "5027/ROB_1/J1".into(), name: "DC link".into(), units: "V".into(), samples: Box::new(rows.into_iter()) }]).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text.lines().next(), Some("time_utc,t_s,channel,name,units,mean,min,max,count"));
        assert_eq!(text.lines().nth(1), Some(",0.000,5027/ROB_1/J1,\"DC link\",\"V\",356.5,300,357.1,250"), "the dip the slow log caught is in the file");
    }

    #[test]
    fn a_save_stopped_partway_leaves_nothing() {
        // The window closing while a long save runs: it stops, and no part file stays.
        let dir = std::env::temp_dir().join(format!("spy-export-stop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("x view.csv");
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let s2 = stop.clone();
        let samples = (0..200_000i64).map(move |t| {
            if t == 1000 {
                s2.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            (t, Value::Number(1.0))
        });
        let r = write_csv_until(&p, 0, None, vec![Series { id: "a".into(), name: "n".into(), units: String::new(), samples: Box::new(samples) }], &stop);
        assert!(r.is_err(), "finished though told to stop: {r:?}");
        assert!(!p.exists() && !part_name(&p).exists(), "a stopped save left a file behind");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_text_channel_merges_in_time_order() {
        let a = vec![(1, Value::Number(1.0)), (5, Value::Number(5.0))];
        let b = vec![(0, Value::Text("x".into())), (3, Value::Text("y".into())), (9, Value::Text("z".into()))];
        let t: Vec<i64> = merge_two(a.into_iter(), b.into_iter()).map(|(t, _)| t).collect();
        assert_eq!(t, vec![0, 1, 3, 5, 9]);
    }

    #[test]
    fn a_save_appears_only_when_complete() {
        let dir = std::env::temp_dir().join(format!("spy-export-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("x view.csv");
        assert_eq!(write_csv(&p, 0, None, vec![Series { id: "4002/ROB_1/J1".into(), name: "n".into(), units: "Nm".into(), samples: numbers(vec![(0, 1.0), (4, 2.0)]) }]), Ok(2));
        assert_eq!(std::fs::read_to_string(&p).unwrap().lines().count(), 3);
        assert!(!part_name(&p).exists());

        // A failing series (a disk that fills, say): no file, no part left behind.
        struct Failing;
        impl Write for Failing {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("disk full"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        assert!(write_rows(&mut Failing, 0, None, vec![]).is_err());
        let q = dir.join("y view.csv");
        std::fs::write(part_name(&q), b"someone else's").unwrap();
        assert!(write_csv(&q, 0, None, vec![]).is_err(), "an existing part file is not overwritten");
        assert!(!q.exists());
        assert_eq!(std::fs::read(part_name(&q)).unwrap(), b"someone else's");
        // Written but not renamed (the name taken meanwhile by a folder): the part goes.
        let r = dir.join("z view.csv");
        std::fs::create_dir(&r).unwrap();
        assert!(write_csv(&r, 0, None, vec![Series { id: "4002/ROB_1/J1".into(), name: "n".into(), units: "Nm".into(), samples: numbers(vec![(0, 1.0)]) }]).is_err());
        assert!(!part_name(&r).exists(), "a failed save leaves no part file");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_screenshot_is_cropped_to_the_charts() {
        let mut img = egui::ColorImage::filled([20, 10], egui::Color32::BLACK);
        img.pixels[3 * 20 + 5] = egui::Color32::RED;
        // The charts at (2.5, 1.5)-(5, 4) points on a 2x display: pixels (5, 3)-(10, 8).
        let png = crop_png(&img, egui::Rect::from_min_max(egui::pos2(2.5, 1.5), egui::pos2(5.0, 4.0)), 2.0).unwrap();
        let dec = png::Decoder::new(std::io::Cursor::new(png));
        let mut r = dec.read_info().unwrap();
        let mut buf = vec![0; r.output_buffer_size().unwrap()];
        let info = r.next_frame(&mut buf).unwrap();
        assert_eq!((info.width, info.height), (5, 5));
        assert_eq!(&buf[0..4], &[255, 0, 0, 255], "the red pixel is the crop's first");
        assert!(crop_png(&img, egui::Rect::from_min_max(egui::pos2(30.0, 30.0), egui::pos2(40.0, 40.0)), 1.0).is_err());
    }
}
