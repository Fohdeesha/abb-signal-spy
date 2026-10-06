use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::reading::ZeroHold;
use crate::recording::{self, ChannelEntry, CsvRows, Kind, Meta};
use crate::store::{ChannelKey, Column};

const BLOCK: usize = 256;
pub const MAX_FILE_BYTES: u64 = 2 << 30;
const PEAK_PER_FILE_BYTE: f64 = 1.75;
const RESTART_BACK_MS: i64 = 2_000;
const MAX_CONTROLLER_MS: i64 = 1 << 40;

#[derive(Debug, Clone, Copy, PartialEq)]
struct Block {
    start: usize,
    end: usize,
    t0: i64,
    t1: i64,
    min: f64,
    max: f64,
    first: f64,
    last: f64,
}

#[derive(Debug, Clone)]
pub struct ReviewChannel {
    pub id: String,
    pub key: Option<ChannelKey>,
    pub entry: Option<ChannelEntry>,
    pub t: Vec<i64>,
    pub v: Vec<f64>,
    pub raw: Option<Vec<f64>>,
    pub band: Option<(Vec<f64>, Vec<f64>)>,
    pub counts: Option<Vec<u64>>,
    pub text: Vec<(i64, String)>,
    pub gap_ms: f64,
    pub derived: Option<crate::derived::Derived>,
    blocks: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReviewMark {
    pub t: i64,
    pub kind: String,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct Review {
    pub dir: PathBuf,
    pub meta: Meta,
    pub channels: Vec<ReviewChannel>,
    pub wall_clock: bool,
    pub start: i64,
    pub end: i64,
    pub bad_rows: usize,
    pub bad_lines: Vec<usize>,
    pub out_of_order: usize,
    pub notes: Vec<String>,
    pub marks: Vec<ReviewMark>,
}

enum Mapping {
    ByRow(Vec<(u64, i64, i64)>),
    BySegment(Vec<Vec<(i64, i64)>>),
    Controller,
}

struct Mapper {
    mapping: Mapping,
    segment: usize,
    seg_max: Option<i64>,
    at: usize,
    ran_out: bool,
}

impl Mapper {
    fn map(&mut self, row: u64, cms: i64) -> i64 {
        match &self.mapping {
            Mapping::Controller => cms,
            Mapping::ByRow(a) => {
                while self.at + 1 < a.len() && a[self.at + 1].0 <= row {
                    self.at += 1;
                }
                let (_, c, u) = a[self.at];
                u + (cms - c)
            }
            Mapping::BySegment(segs) => {
                if let Some(m) = self.seg_max
                    && cms < m - RESTART_BACK_MS
                {
                    self.segment += 1;
                    self.seg_max = None;
                }
                self.seg_max = Some(self.seg_max.map_or(cms, |m| m.max(cms)));
                let seg = match segs.get(self.segment) {
                    Some(s) => s,
                    None => {
                        self.ran_out = true;
                        segs.last().expect("at least one segment")
                    }
                };
                let (c, u) = seg.iter().rev().find(|(c, _)| *c <= cms).or(seg.first()).copied().expect("a segment has an anchor");
                u + (cms - c)
            }
        }
    }
}

fn utc_ms(iso: &str) -> Option<i64> {
    let t = crate::util::parse_iso(iso)?;
    Some(match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_millis() as i64,
        Err(e) => -(e.duration().as_millis() as i64),
    })
}

pub fn open(dir: &Path, hold: &[u32]) -> Result<Review, String> {
    open_limited(dir, hold, size_limit(crate::util::free_memory()))
}

pub fn size_limit(free_memory: Option<u64>) -> u64 {
    free_memory.map_or(MAX_FILE_BYTES, |free| ((free as f64 / PEAK_PER_FILE_BYTE) as u64).min(MAX_FILE_BYTES))
}

fn gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / (1u64 << 30) as f64)
}

fn open_limited(dir: &Path, hold: &[u32], max_bytes: u64) -> Result<Review, String> {
    let meta = recording::read_meta(dir)?;
    let file = if meta.kind == Kind::Slow { "slow.csv" } else { "data.csv" };
    let path = dir.join(file);
    let size = std::fs::metadata(&path).map_err(|e| format!("cannot read {file}: {e}"))?.len();
    if size > max_bytes {
        let why = if max_bytes < MAX_FILE_BYTES { format!("the memory this PC has free now allows about {}", gb(max_bytes)) } else { format!("this program opens at most {}", gb(MAX_FILE_BYTES)) };
        return Err(format!(
            "{file} is {}, more than can be opened here ({why}). Close other programs and try again, or read the file with a script: it is plain CSV, one sample a row.",
            gb(size)
        ));
    }
    let mut notes = Vec::new();
    let anchors: Vec<(Option<u64>, i64, i64)> = meta.anchors.iter().filter_map(|a| Some((a.row, a.controller_ms, utc_ms(&a.utc)?))).collect();
    if anchors.len() < meta.anchors.len() {
        notes.push(format!("{} of the recording's wall-clock anchors could not be read and were left out.", meta.anchors.len() - anchors.len()));
    }
    let mapping = if anchors.is_empty() {
        notes.push("The recording has no wall-clock anchor: times are the controller's own clock.".into());
        Mapping::Controller
    } else if anchors.iter().all(|a| a.0.is_some()) {
        let mut v: Vec<(u64, i64, i64)> = anchors.iter().map(|a| (a.0.unwrap_or(0), a.1, a.2)).collect();
        v.sort_by_key(|a| a.0);
        Mapping::ByRow(v)
    } else {
        let mut segs: Vec<Vec<(i64, i64)>> = vec![Vec::new()];
        let mut prev: Option<i64> = None;
        for &(_, c, u) in &anchors {
            if prev.is_some_and(|p| c < p - RESTART_BACK_MS) {
                segs.push(Vec::new());
            }
            prev = Some(c);
            segs.last_mut().expect("one segment").push((c, u));
        }
        Mapping::BySegment(segs)
    };
    let mut mapper = Mapper { mapping, segment: 0, seg_max: None, at: 0, ran_out: false };

    let f = std::fs::File::open(&path).map_err(|e| format!("cannot read {file}: {e}"))?;
    let whole = recording::ends_with_a_whole_row(&path);
    let mut rows = CsvRows { r: std::io::BufReader::with_capacity(256 * 1024, f), line: 0, error: None, strict_end: !meta.complete || !whole };
    let slow = meta.kind == Kind::Slow;
    let want = if slow { 6 } else { 3 };
    let mut by_id: BTreeMap<String, ReviewChannel> = BTreeMap::new();
    let mut ids: BTreeMap<String, String> = BTreeMap::new();
    let (mut bad_rows, mut bad_lines, mut out_of_order) = (0usize, Vec::new(), 0usize);
    let mut header = true;
    let mut row: u64 = 0;
    while let Some(r) = rows.next_row() {
        if std::mem::take(&mut header) {
            continue;
        }
        let this = row;
        row += 1;
        let parsed = r.ok().filter(|(_, f)| f.len() == want).and_then(|(line, f)| Some((line, f[0].0.parse::<i64>().ok().filter(|c| (0..MAX_CONTROLLER_MS).contains(c))?, f)));
        let Some((_, cms, fields)) = parsed else {
            bad_rows += 1;
            if bad_lines.len() < 20 {
                bad_lines.push(rows.line);
            }
            continue;
        };
        let raw_id = &fields[1].0;
        let id = ids.entry(raw_id.clone()).or_insert_with(|| ChannelKey::parse_id(raw_id).map(|k| k.id()).unwrap_or_else(|| raw_id.clone())).clone();
        let t = mapper.map(this, cms);
        let ch = by_id.entry(id.clone()).or_insert_with(|| ReviewChannel {
            key: ChannelKey::parse_id(&id),
            entry: meta.channels.iter().find(|c| c.id == id).cloned(),
            id: id.clone(),
            t: Vec::new(),
            v: Vec::new(),
            raw: None,
            band: slow.then(|| (Vec::new(), Vec::new())),
            counts: slow.then(Vec::new),
            text: Vec::new(),
            gap_ms: 0.0,
            derived: None,
            blocks: Vec::new(),
        });
        if !slow && fields[2].1 {
            ch.text.push((t, fields[2].0.clone()));
            continue;
        }
        if ch.t.last().is_some_and(|&l| t < l) {
            out_of_order += 1;
            continue;
        }
        let value = |i: usize| fields[i].0.parse::<f64>().unwrap_or(f64::NAN);
        ch.t.push(t);
        ch.v.push(value(if slow { 3 } else { 2 }));
        if let Some((lo, hi)) = &mut ch.band {
            lo.push(value(4));
            hi.push(value(5));
        }
        if let Some(n) = &mut ch.counts {
            n.push(fields[2].0.parse().unwrap_or(0));
        }
    }
    if let Some(e) = rows.error {
        return Err(format!("{file}: {e}"));
    }
    if meta.complete && row < meta.rows_written {
        notes.push(format!("{file} holds {row} of the {} rows the recording wrote: it is cut short (a copy that did not finish?).", meta.rows_written));
    } else if meta.complete && !whole {
        notes.push(format!("{file} does not end with a whole row: its last line was left out."));
    }
    if mapper.ran_out {
        notes.push("The controller's clock restarted more often than the recording has anchors: the times after the last one are estimates.".into());
    }
    if bad_rows > 0 {
        notes.push(format!("{bad_rows} row(s) could not be read and were left out (a recording cut short by a crash ends in a partial row)."));
    }
    if out_of_order > 0 {
        notes.push(format!("{out_of_order} sample(s) went back in time within their channel and were left out."));
    }
    let interval = meta.interval_ms.map(f64::from);
    let mut channels: Vec<ReviewChannel> = by_id.into_values().collect();
    for ch in &mut channels {
        let sample_ms = interval.or(ch.entry.as_ref().and_then(|e| e.sample_ms)).filter(|m| m.is_finite() && *m > 0.0).unwrap_or(4.032);
        ch.gap_ms = sample_ms * 1.5;
        if !slow && ch.key.as_ref().is_some_and(|k| hold.contains(&k.signal)) {
            ch.raw = Some(ch.v.clone());
            let mut h = ZeroHold::new();
            for (t, v) in ch.t.iter().zip(ch.v.iter_mut()) {
                *v = h.apply_at(*t, *v);
            }
        }
        ch.blocks = blocks(&ch.t, &ch.v, ch.band.as_ref(), ch.gap_ms);
    }
    derive(&meta, &mut channels, &mut notes);
    let firsts = channels.iter().filter_map(|c| c.t.first().copied()).chain(channels.iter().filter_map(|c| c.text.first().map(|x| x.0)));
    let lasts = channels.iter().filter_map(|c| c.t.last().copied()).chain(channels.iter().filter_map(|c| c.text.last().map(|x| x.0)));
    let (start, end) = (firsts.min().unwrap_or(0), lasts.max().unwrap_or(0));
    let wall_clock = !matches!(mapper.mapping, Mapping::Controller);
    let through_anchor = |c: i64, u: i64| -> i64 {
        let a = anchors.iter().rev().find(|a| a.2 <= u).or(anchors.first());
        match a {
            Some(&(_, ac, au)) if ((au + (c - ac)) - u).abs() <= 60_000 => au + (c - ac),
            _ => u,
        }
    };
    let mut marks = Vec::new();
    for m in &meta.markers {
        let t = match (wall_clock, m.controller_ms, utc_ms(&m.utc)) {
            (false, Some(c), _) => Some(c),
            (true, Some(c), Some(u)) => Some(through_anchor(c, u)),
            (true, None, Some(u)) => Some(u),
            _ => None,
        };
        if let Some(t) = t {
            marks.push(ReviewMark { t, kind: "marker".into(), text: m.text.clone() });
        }
    }
    if wall_clock {
        for e in &meta.events {
            if let Some(t) = utc_ms(&e.utc) {
                marks.push(ReviewMark { t, kind: e.kind.clone(), text: e.text.clone() });
            }
        }
    }
    marks.sort_by_key(|m| m.t);
    Ok(Review { dir: dir.to_path_buf(), meta, channels, wall_clock, start, end, bad_rows, bad_lines, out_of_order, notes, marks })
}

fn derive(meta: &Meta, channels: &mut Vec<ReviewChannel>, notes: &mut Vec<String>) {
    if meta.derived.is_empty() {
        return;
    }
    if meta.kind == Kind::Slow {
        notes.push("Derived channels are not computed from a slow log's interval averages.".into());
        return;
    }
    let mut out = Vec::new();
    for def in &meta.derived {
        if !def.legs_valid() {
            notes.push(format!("{} was left out: it is not the three PWM legs of one axis (the recording's description was edited).", def.name()));
            continue;
        }
        let inputs: Option<Vec<&ReviewChannel>> = def.inputs().iter().map(|k| channels.iter().find(|c| c.id == k.id())).collect();
        let Some(inputs) = inputs else {
            notes.push(format!("{} could not be computed: an input of it is not in the recording.", def.name()));
            continue;
        };
        if !def.is_set() {
            notes.push(format!("{} has no values: its {} was never set while recording.", def.name(), if matches!(def, crate::derived::Derived::Turn { .. }) { "target" } else { "plateau" }));
        }
        let series: Vec<Vec<(i64, f64)>> = inputs.iter().map(|c| c.t.iter().copied().zip(c.raw.as_ref().unwrap_or(&c.v).iter().copied()).collect()).collect();
        let refs: Vec<&[(i64, f64)]> = series.iter().map(|s| s.as_slice()).collect();
        let (t, v): (Vec<i64>, Vec<f64>) = def.combine(&refs).into_iter().unzip();
        let first = inputs[0];
        let entry = ChannelEntry {
            id: def.id(),
            signal: 0,
            unit: first.key.as_ref().map(|k| k.unit.to_string()).unwrap_or_default(),
            axis: first.key.as_ref().map_or(1, |k| k.axis.one_based()),
            name: def.name(),
            units: def.units().into(),
            description: "Computed from the recorded channels; not itself recorded.".into(),
            stream_id: None,
            sample_ms: first.entry.as_ref().and_then(|e| e.sample_ms),
            value_type: None,
            samples: t.len() as u64,
            first_controller_ms: None,
            last_controller_ms: None,
        };
        let gap_ms = first.gap_ms;
        let b = blocks(&t, &v, None, gap_ms);
        out.push(ReviewChannel { id: def.id(), key: None, entry: Some(entry), t, v, raw: None, band: None, counts: None, text: Vec::new(), gap_ms, derived: Some(def.clone()), blocks: b });
    }
    if meta.events.iter().any(|e| e.kind == "derived-setting") && !out.is_empty() {
        notes.push("A target or plateau was set or cleared during the recording: the derived values here are all computed with the last one (the changes are marked).".into());
    }
    channels.extend(out);
}

fn blocks(t: &[i64], v: &[f64], band: Option<&(Vec<f64>, Vec<f64>)>, gap_ms: f64) -> Vec<Block> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < t.len() {
        let start = i;
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        loop {
            let x = v[i];
            let (a, b) = match band {
                Some((l, h)) => (l[i].min(x), h[i].max(x)),
                None => (x, x),
            };
            if a.is_finite() {
                lo = lo.min(a);
            }
            if b.is_finite() {
                hi = hi.max(b);
            }
            i += 1;
            if i >= t.len() || i - start >= BLOCK || (t[i] - t[i - 1]) as f64 > gap_ms || !v[i].is_finite() || !v[i - 1].is_finite() {
                break;
            }
        }
        out.push(Block { start, end: i, t0: t[start], t1: t[i - 1], min: lo, max: hi, first: v[start], last: v[i - 1] });
    }
    out
}

impl ReviewChannel {
    fn lower_bound(&self, t: i64) -> usize {
        self.t.partition_point(|&x| x < t)
    }

    pub fn range(&self, from: i64, to: i64) -> impl Iterator<Item = (i64, f64)> + '_ {
        let (a, b) = (self.lower_bound(from), self.lower_bound(to));
        (a..b).map(move |i| (self.t[i], self.v[i]))
    }

    pub fn at_or_before(&self, t: i64) -> Option<(i64, f64)> {
        let i = self.lower_bound(t.saturating_add(1));
        (i > 0).then(|| (self.t[i - 1], self.v[i - 1]))
    }

    pub fn value_at(&self, t: i64) -> Option<f64> {
        let (at, v) = self.at_or_before(t)?;
        ((t - at) as f64 <= self.gap_ms).then_some(v)
    }

    pub fn extremes(&self, from: i64, to: i64) -> Option<(f64, f64)> {
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for i in self.lower_bound(from)..self.lower_bound(to) {
            let x = self.v[i];
            let (l, h) = match &self.band {
                Some((bl, bh)) => (bl[i].min(x), bh[i].max(x)),
                None => (x, x),
            };
            if l.is_finite() {
                lo = lo.min(l);
            }
            if h.is_finite() {
                hi = hi.max(h);
            }
        }
        (lo <= hi).then_some((lo, hi))
    }

    pub fn recorded(&self, from: i64, to: i64) -> impl Iterator<Item = (i64, f64)> + '_ {
        let (a, b) = (self.lower_bound(from), self.lower_bound(to));
        let vals = self.raw.as_ref().unwrap_or(&self.v);
        (a..b).map(move |i| (self.t[i], vals[i]))
    }

    pub fn decimate(&self, from: i64, to: i64, columns: usize, scale: f64) -> Vec<Vec<Column>> {
        let mut segments: Vec<Vec<Column>> = Vec::new();
        if columns == 0 || to <= from || self.t.is_empty() {
            return segments;
        }
        let span = (to - from) as f64;
        let col = |t: i64| (((t - from) as f64 / span) * columns as f64).floor().clamp(0.0, columns as f64) as usize;
        let a = self.lower_bound(from).saturating_sub(1);
        let b = (self.lower_bound(to) + 1).min(self.t.len());
        let mut current: Vec<Column> = Vec::new();
        let mut col_idx: Option<usize> = None;
        let push = |current: &mut Vec<Column>, col_idx: &mut Option<usize>, c: usize, t: i64, lo: f64, hi: f64, first: f64, last: f64| {
            if *col_idx == Some(c)
                && let Some(l) = current.last_mut()
            {
                l.min = l.min.min(lo);
                l.max = l.max.max(hi);
                l.last_v = last;
            } else {
                current.push(Column { t, min: lo, max: hi, first_v: first, last_v: last });
                *col_idx = Some(c);
            }
        };
        let mut prev: Option<i64> = None;
        let sample = |i: usize, current: &mut Vec<Column>, col_idx: &mut Option<usize>, segments: &mut Vec<Vec<Column>>, prev: &mut Option<i64>| {
            let (t, x) = (self.t[i], self.v[i] * scale);
            if prev.is_some_and(|p| (t - p) as f64 > self.gap_ms) && !current.is_empty() {
                segments.push(std::mem::take(current));
                *col_idx = None;
            }
            *prev = Some(t);
            if !x.is_finite() {
                if !current.is_empty() {
                    segments.push(std::mem::take(current));
                }
                *col_idx = None;
                return;
            }
            let (lo, hi) = match &self.band {
                Some((l, h)) => ((l[i] * scale).min(x), (h[i] * scale).max(x)),
                None => (x, x),
            };
            push(current, col_idx, col(t), t, lo, hi, x, x);
        };
        if b - a <= columns * 8 {
            for i in a..b {
                sample(i, &mut current, &mut col_idx, &mut segments, &mut prev);
            }
        } else {
            let (inside_a, inside_b) = (self.lower_bound(from), self.lower_bound(to));
            let first_block = self.blocks.partition_point(|bl| bl.end <= a);
            for bl in &self.blocks[first_block..] {
                if bl.start >= b {
                    break;
                }
                let whole = bl.start >= inside_a && bl.end <= inside_b && col(bl.t0) == col(bl.t1) && bl.min.is_finite() && bl.first.is_finite() && bl.last.is_finite();
                if !whole {
                    for i in bl.start.max(a)..bl.end.min(b) {
                        sample(i, &mut current, &mut col_idx, &mut segments, &mut prev);
                    }
                    continue;
                }
                if prev.is_some_and(|p| (bl.t0 - p) as f64 > self.gap_ms) && !current.is_empty() {
                    segments.push(std::mem::take(&mut current));
                    col_idx = None;
                }
                prev = Some(bl.t1);
                let (lo, hi) = if scale >= 0.0 { (bl.min * scale, bl.max * scale) } else { (bl.max * scale, bl.min * scale) };
                push(&mut current, &mut col_idx, col(bl.t0), bl.t0, lo, hi, bl.first * scale, bl.last * scale);
            }
        }
        if !current.is_empty() {
            segments.push(current);
        }
        segments
    }
}

impl Review {
    pub fn channel(&self, id: &str) -> Option<&ReviewChannel> {
        self.channels.iter().find(|c| c.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testdir::TestDir;

    fn temp(tag: &str) -> TestDir {
        TestDir::new(&format!("review-{tag}"))
    }

    fn folder(tag: &str, kind: &str, anchors: &str, extra: &str, csv: &str) -> TestDir {
        let d = temp(tag);
        let file = if kind == "slow" { "slow.csv" } else { "data.csv" };
        let json = format!(
            r#"{{"format": "abb-signal-spy-recording", "version": 2, "kind": "{kind}", "app": "t", "started_utc": "2026-09-27T10:00:00.000Z",
                "complete": true, "controller": "t", "channels": [{{"id": "4002/ROB_1/J1", "signal": 4002, "unit": "ROB_1", "axis": 1, "name": "Torque", "units": "Nm", "sample_ms": 4.0}}],
                "anchors": [{anchors}], "rows_written": 0, "samples_lost": 0 {extra}}}"#
        );
        std::fs::write(d.join("recording.json"), json).unwrap();
        std::fs::write(d.join(file), csv).unwrap();
        d
    }

    fn ms(iso: &str) -> i64 {
        utc_ms(iso).unwrap()
    }

    #[test]
    fn derived_channels_are_computed_again_from_the_recording() {
        let mut csv = String::from("controller_ms,channel,value\n");
        for (c, legs) in [(10000, [0.5, 0.5, 0.5]), (10004, [0.5, f64::NAN, 0.6]), (10008, [0.4, 0.6, 0.5]), (10002, [0.5, 0.5, 0.4]), (10006, [0.5, 0.5, 0.5])] {
            for (n, v) in [5020, 5021, 5022].iter().zip(legs) {
                if !v.is_nan() {
                    csv += &format!("{c},{n}/ROB_1/J2,{v}\n");
                }
            }
            csv += &format!("{c},5027/ROB_1/J1,350\n");
        }
        let anchors = r#"{"controller_ms": 10000, "utc": "2026-09-27T10:00:00.000Z", "row": 0}, {"controller_ms": 10002, "utc": "2026-09-27T10:01:00.000Z", "row": 11}"#;
        let derived = r#", "derived": [
            {"kind": "duty_sum", "legs": [{"signal": 5020, "unit": "ROB_1", "axis": 2}, {"signal": 5021, "unit": "ROB_1", "axis": 2}, {"signal": 5022, "unit": "ROB_1", "axis": 2}]},
            {"kind": "duty_sum", "legs": [{"signal": 5020, "unit": "ROB_1", "axis": 2}, {"signal": 5027, "unit": "ROB_1", "axis": 1}, {"signal": 5022, "unit": "ROB_1", "axis": 2}]},
            {"kind": "sag", "link": {"signal": 5027, "unit": "ROB_1", "axis": 1}, "plateau_v": 356.0},
            {"kind": "turn", "angle": {"signal": 5138, "unit": "ROB_1", "axis": 3}, "target_deg": 90.0}
        ], "events": [{"utc": "2026-09-27T10:00:30.000Z", "kind": "derived-setting", "text": "plateau set"}]"#;
        let d = folder("derived", "full", anchors, derived, &csv);
        let r = open(&d, &[]).unwrap();
        let (t0, t1) = (ms("2026-09-27T10:00:00.000Z"), ms("2026-09-27T10:01:00.000Z"));
        let sum = r.channel("duty-sum:ROB_1/J2").expect("the duty sum");
        assert_eq!(sum.t, vec![t0, t0 + 8, t1, t1 + 4], "only where all three legs have a sample: not at 10004");
        assert_eq!(sum.v.iter().map(|v| (v * 1e6).round() / 1e6).collect::<Vec<_>>(), vec![1.5, 1.5, 1.4, 1.5]);
        assert!(sum.derived.is_some() && sum.key.is_none());
        assert_eq!(sum.entry.as_ref().unwrap().name, "PWM duty sum  ROB_1 J2");
        let sag = r.channel("sag:5027/ROB_1/J1").unwrap();
        assert!(sag.v.len() == 5 && sag.v.iter().all(|&v| v == 6.0));
        assert!(r.channel("turn:5138/ROB_1/J3").is_none());
        assert_eq!(r.channels.iter().filter(|c| c.derived.is_some() && c.id.starts_with("duty-sum")).count(), 1, "a DC link summed with two duty legs as a duty sum");
        assert!(r.notes.iter().any(|n| n.contains("not the three PWM legs")), "{:?}", r.notes);
        assert!(r.notes.iter().any(|n| n.contains("Turn to target") && n.contains("not in the recording")), "{:?}", r.notes);
        assert!(r.notes.iter().any(|n| n.contains("computed with the last one")), "a setting changed while recording: {:?}", r.notes);

        let slow = folder("derived-slow", "slow", r#"{"controller_ms": 10000, "utc": "2026-09-27T10:00:00.000Z", "row": 0}"#, derived, "controller_ms,channel,count,mean,min,max\n10000,5027/ROB_1/J1,25,350,349,351\n");
        let r = open(&slow, &[]).unwrap();
        assert!(r.channel("sag:5027/ROB_1/J1").is_none());
        assert!(r.notes.iter().any(|n| n.contains("slow log")), "{:?}", r.notes);
    }

    #[test]
    fn rows_on_both_sides_of_a_restart_map_to_their_own_anchor() {
        let csv = "controller_ms,channel,value\n10000,4002/ROB_1/J1,1\n10004,4002/ROB_1/J1,2\n10008,4002/ROB_1/J1,3\n10002,4002/ROB_1/J1,4\n10006,4002/ROB_1/J1,5\n";
        let anchors = r#"{"controller_ms": 10000, "utc": "2026-09-27T10:00:00.000Z", "row": 0}, {"controller_ms": 10002, "utc": "2026-09-27T10:01:00.000Z", "row": 3}"#;
        let d = folder("rows", "full", anchors, "", csv);
        let r = open(&d, &[]).unwrap();
        assert!(r.wall_clock);
        let ch = r.channel("4002/ROB_1/J1").unwrap();
        let (t0, t1) = (ms("2026-09-27T10:00:00.000Z"), ms("2026-09-27T10:01:00.000Z"));
        assert_eq!(ch.t, vec![t0, t0 + 4, t0 + 8, t1, t1 + 4]);
        assert_eq!(ch.v, vec![1.0, 2.0, 3.0, 4.0, 5.0]);
        assert_eq!((r.start, r.end, r.out_of_order), (t0, t1 + 4, 0));
    }

    #[test]
    fn a_format_1_recording_maps_by_its_clock_segments() {
        let csv = "controller_ms,channel,value\n900000,4002/ROB_1/1,1\n900004,4002/ROB_1/1,2\n960000,4002/ROB_1/1,3\n5000,4002/ROB_1/1,4\n5004,4002/ROB_1/1,5\n";
        let anchors = r#"{"controller_ms": 900000, "utc": "2026-09-27T10:00:00.000Z"}, {"controller_ms": 960000, "utc": "2026-09-27T10:01:00.500Z"}, {"controller_ms": 5000, "utc": "2026-09-27T10:05:00.000Z"}"#;
        let d = folder("v1", "full", anchors, "", csv);
        let r = open(&d, &[]).unwrap();
        let ch = r.channel("4002/ROB_1/J1").expect("the format-1 id read under the new one");
        let (a, b, c) = (ms("2026-09-27T10:00:00.000Z"), ms("2026-09-27T10:01:00.500Z"), ms("2026-09-27T10:05:00.000Z"));
        assert_eq!(ch.t, vec![a, a + 4, b, c, c + 4], "the reconnect's anchor re-times its rows (the PC's clock drifted 0.5 s)");
    }

    #[test]
    fn a_recording_without_an_anchor_keeps_the_controller_clock_and_says_so() {
        let d = folder("noanchor", "full", "", "", "controller_ms,channel,value\n100,4002/ROB_1/J1,1\n104,4002/ROB_1/J1,2\n");
        let r = open(&d, &[]).unwrap();
        assert!(!r.wall_clock);
        assert_eq!(r.channel("4002/ROB_1/J1").unwrap().t, vec![100, 104]);
        assert!(r.notes.iter().any(|n| n.contains("no wall-clock anchor")), "{:?}", r.notes);
    }

    #[test]
    fn a_slow_log_carries_its_minimum_and_maximum() {
        let csv = "controller_ms,channel,count,mean,min,max\n1000,4002/ROB_1/J1,25,2.5,1,4\n2000,4002/ROB_1/J1,25,3,2,9\n";
        let anchors = r#"{"controller_ms": 1000, "utc": "2026-09-27T10:00:00.000Z", "row": 0}"#;
        let d = folder("slow", "slow", anchors, r#", "interval_ms": 1000"#, csv);
        let r = open(&d, &[]).unwrap();
        let ch = r.channel("4002/ROB_1/J1").unwrap();
        assert_eq!(ch.v, vec![2.5, 3.0]);
        assert_eq!(ch.band, Some((vec![1.0, 2.0], vec![4.0, 9.0])));
        assert_eq!(ch.counts, Some(vec![25, 25]), "the samples in each interval");
        assert_eq!(ch.extremes(r.start, r.end + 1), Some((1.0, 9.0)), "a slow log's extremes are its intervals' minimum and maximum, not its means'");
        assert_eq!(ch.gap_ms, 1500.0, "one interval and a half");
        let cols = ch.decimate(r.start, r.end + 1, 100, 1.0);
        assert_eq!(cols.len(), 1);
        assert_eq!(cols[0].iter().map(|c| c.max).fold(f64::MIN, f64::max), 9.0, "the band's maximum is charted");
    }

    #[test]
    fn rows_that_cannot_be_read_and_samples_going_back_are_counted_not_kept() {
        let csv = "controller_ms,channel,value\n1000,4002/ROB_1/J1,1\nnot a row\n1004,4002/ROB_1/J1,2\n990,4002/ROB_1/J1,9\n1008,4002/ROB_1/J1,3\n";
        let anchors = r#"{"controller_ms": 1000, "utc": "2026-09-27T10:00:00.000Z", "row": 0}"#;
        let d = folder("bad", "full", anchors, "", csv);
        let r = open(&d, &[]).unwrap();
        assert_eq!(r.channel("4002/ROB_1/J1").unwrap().v, vec![1.0, 2.0, 3.0]);
        assert_eq!((r.bad_rows, r.out_of_order), (1, 1));
        assert_eq!(r.bad_lines, vec![3]);
        assert_eq!(r.notes.len(), 2, "{:?}", r.notes);
    }

    #[test]
    fn a_file_too_large_is_refused_with_a_reason() {
        let d = folder("big", "full", "", "", "controller_ms,channel,value\n100,4002/ROB_1/J1,1\n");
        let e = open_limited(&d, &[], 10).unwrap_err();
        assert!(e.contains("more than can be opened here") && e.contains("memory this PC has free"), "{e}");
        assert!(!e.contains("README") && !e.contains("spreadsheet"), "it pointed somewhere that does not help: {e}");
    }

    #[test]
    fn the_size_opened_follows_the_memory_free() {
        assert_eq!(size_limit(None), MAX_FILE_BYTES, "not known: the fixed limit");
        assert_eq!(size_limit(Some(u64::MAX)), MAX_FILE_BYTES, "never above it");
        assert_eq!(size_limit(Some(1_750_000_000)), 1_000_000_000, "the peak of opening fits what is free");
        assert_eq!(size_limit(Some(0)), 0);
    }

    #[test]
    fn a_zero_filled_signal_is_held_when_asked() {
        let mut csv = String::from("controller_ms,channel,value\n");
        for (i, v) in [0.5, 0.0, 0.0, 0.7, 0.0].iter().enumerate() {
            csv.push_str(&format!("{},6010/ROB_1/J1,{v}\n", 1000 + 4 * i));
        }
        let anchors = r#"{"controller_ms": 1000, "utc": "2026-09-27T10:00:00.000Z", "row": 0}"#;
        let d = folder("hold", "full", anchors, "", &csv);
        let held = open(&d, &[6010]).unwrap();
        let ch = held.channel("6010/ROB_1/J1").unwrap();
        assert_eq!(ch.v, vec![0.5, 0.5, 0.5, 0.7, 0.7]);
        assert_eq!(ch.recorded(i64::MIN, i64::MAX).map(|(_, v)| v).collect::<Vec<_>>(), vec![0.5, 0.0, 0.0, 0.7, 0.0], "the values as recorded stay available");
        assert_eq!(open(&d, &[]).unwrap().channel("6010/ROB_1/J1").unwrap().v, vec![0.5, 0.0, 0.0, 0.7, 0.0]);
    }

    #[test]
    fn markers_and_events_land_on_the_time_axis() {
        let anchors = r#"{"controller_ms": 1000, "utc": "2026-09-27T10:00:00.000Z", "row": 0}"#;
        let extra = r#", "markers": [{"utc": "2026-09-27T10:00:00.500Z", "kind": "marker", "text": "dip", "controller_ms": 1500}],
                       "events": [{"utc": "2026-09-27T10:00:00.200Z", "kind": "lost", "text": "connection lost"}]"#;
        let d = folder("marks", "full", anchors, extra, "controller_ms,channel,value\n1000,4002/ROB_1/J1,1\n");
        let r = open(&d, &[]).unwrap();
        let t0 = ms("2026-09-27T10:00:00.000Z");
        assert_eq!(r.marks, vec![ReviewMark { t: t0 + 200, kind: "lost".into(), text: "connection lost".into() }, ReviewMark { t: t0 + 500, kind: "marker".into(), text: "dip".into() }]);
    }

    fn channel(t: Vec<i64>, v: Vec<f64>) -> ReviewChannel {
        let gap_ms = 6.0;
        let blocks = blocks(&t, &v, None, gap_ms);
        ReviewChannel { id: "x".into(), key: None, entry: None, t, v, raw: None, band: None, counts: None, text: Vec::new(), gap_ms, derived: None, blocks }
    }

    #[test]
    fn a_zoomed_out_view_keeps_its_spikes_where_they_are() {
        let n = 20_000usize;
        let t: Vec<i64> = (0..n as i64).map(|i| i * 4).collect();
        let mut v = vec![0.0; n];
        v[2530] = 1000.0;
        v[5100] = 500.0;
        v[12530] = 800.0;
        let ch = channel(t, v);
        let (from, to) = (10_000, 50_000);
        let cols: Vec<Column> = ch.decimate(from, to, 800, 1.0).into_iter().flatten().collect();
        let width = (to - from) / 800;
        assert!(cols.iter().any(|c| c.max == 1000.0 && c.t >= from), "the spike at 10.12 s is not in view: {:?}", cols.iter().find(|c| c.max == 1000.0).map(|c| c.t));
        let mid = cols.iter().find(|c| c.max == 500.0).expect("the spike at 20.4 s");
        assert!((mid.t - 20_400).abs() <= width, "drawn at {} ms, {} ms from where it is", mid.t, mid.t - 20_400);
        assert!(!cols.iter().any(|c| c.max == 800.0), "a spike from outside the view drawn in it");
        assert!(cols.iter().all(|c| c.t >= from - 4 && c.t <= to + 4), "a column from outside the view (beyond the one sample either side a line enters by)");
        let mut v = vec![0.0; n];
        v[2400] = 700.0;
        let ch = channel((0..n as i64).map(|i| i * 4).collect(), v);
        let wide: Vec<Column> = ch.decimate(from, to, 10, 1.0).into_iter().flatten().collect();
        assert!(!wide.iter().any(|c| c.max == 700.0), "a spike from before the view drawn at its edge");
        assert!(wide.iter().all(|c| c.t >= from - 4), "a column from before the view");
    }

    #[test]
    fn a_gap_between_two_whole_summary_blocks_still_breaks_the_line() {
        let t: Vec<i64> = (0..1000).map(|i| i * 4).chain((0..256).map(|i| 5000 + i * 4)).collect();
        let v = vec![1.0; t.len()];
        let ch = channel(t, v);
        let segs = ch.decimate(0, 40_000, 10, 1.0);
        assert_eq!(segs.len(), 2, "a line drawn across a one-second gap");
    }

    #[test]
    fn a_cursor_in_a_recorded_gap_reads_nothing() {
        let ch = channel(vec![0, 4, 8, 5000, 5004], vec![1.0, 2.0, 3.0, 4.0, 5.0]);
        assert_eq!(ch.value_at(6), Some(2.0));
        assert_eq!(ch.value_at(2500), None, "the value before a gap, read inside it");
        assert_eq!(ch.value_at(5004), Some(5.0));
        assert_eq!(ch.value_at(90_000), None, "long after the channel's last sample");
        assert_eq!(ch.value_at(-5), None);
    }

    #[test]
    fn a_marker_sits_on_the_sample_it_was_put_on() {
        let anchors = r#"{"controller_ms": 1000, "utc": "2026-09-27T10:00:00.000Z", "row": 0}"#;
        let extra = r#", "markers": [{"utc": "2026-09-27T11:00:00.400Z", "kind": "marker", "text": "M1", "controller_ms": 3601000}]"#;
        let d = folder("marker-drift", "full", anchors, extra, "controller_ms,channel,value\n1000,4002/ROB_1/J1,1\n3601000,4002/ROB_1/J1,2\n");
        let r = open(&d, &[]).unwrap();
        let m = r.marks.iter().find(|m| m.kind == "marker").unwrap();
        assert_eq!(m.t, ms("2026-09-27T11:00:00.000Z"), "{} ms off its sample", m.t - ms("2026-09-27T11:00:00.000Z"));
    }

    #[test]
    fn a_garbled_timestamp_is_a_bad_row_not_the_end_of_its_channel() {
        let anchors = r#"{"controller_ms": 1000, "utc": "2026-09-27T10:00:00.000Z", "row": 0}"#;
        let csv = "controller_ms,channel,value\n1000,4002/ROB_1/J1,1\n1004,4002/ROB_1/J1,2\n1000000000000000,4002/ROB_1/J1,9\n-5,4002/ROB_1/J1,9\n1008,4002/ROB_1/J1,3\n1012,4002/ROB_1/J1,4\n";
        let d = folder("garbled", "full", anchors, "", csv);
        let r = open(&d, &[]).unwrap();
        assert_eq!(r.channel("4002/ROB_1/J1").unwrap().v, vec![1.0, 2.0, 3.0, 4.0], "the rows after the garbled ones were dropped as out of order");
        assert_eq!(r.bad_rows, 2);
        assert!(r.end - r.start < 1000, "the stretch spans {} ms", r.end - r.start);
    }

    #[test]
    fn a_row_cut_off_at_the_end_of_an_unfinished_recording_is_left_out() {
        let anchors = r#"{"controller_ms": 1000, "utc": "2026-09-27T10:00:00.000Z", "row": 0}"#;
        let csv = "controller_ms,channel,value\n1000,4002/ROB_1/J1,356.7\n1004,4002/ROB_1/J1,35";
        let d = folder("cut", "full", anchors, "", csv);
        let json = std::fs::read_to_string(d.join("recording.json")).unwrap().replace("\"complete\": true", "\"complete\": false");
        std::fs::write(d.join("recording.json"), json).unwrap();
        let r = open(&d, &[]).unwrap();
        assert_eq!(r.channel("4002/ROB_1/J1").unwrap().v, vec![356.7], "a cut-off value read as data");
        assert_eq!(r.bad_rows, 1);
        let d = folder("whole", "full", anchors, "", &format!("{csv}6.1\n"));
        assert_eq!(open(&d, &[]).unwrap().channel("4002/ROB_1/J1").unwrap().v, vec![356.7, 356.1], "a complete file's last whole row is its own");
    }

    #[test]
    fn a_copy_of_a_complete_recording_cut_short_is_said_and_its_last_part_row_left_out() {
        let anchors = r#"{"controller_ms": 1000, "utc": "2026-09-27T10:00:00.000Z", "row": 0}"#;
        let d = folder("copy-cut", "full", anchors, "", "controller_ms,channel,value\n1000,4002/ROB_1/J1,356.7\n1004,4002/ROB_1/J1,35");
        let json = std::fs::read_to_string(d.join("recording.json")).unwrap().replace("\"rows_written\": 0", "\"rows_written\": 3");
        std::fs::write(d.join("recording.json"), json).unwrap();
        let r = open(&d, &[]).unwrap();
        assert_eq!(r.channel("4002/ROB_1/J1").unwrap().v, vec![356.7], "a cut value read as 35.0");
        assert!(r.notes.iter().any(|n| n.contains("2 of the 3 rows") && n.contains("cut short")), "{:?}", r.notes);
        let d = folder("copy-whole", "full", anchors, "", "controller_ms,channel,value\n1000,4002/ROB_1/J1,356.7\n");
        let json = std::fs::read_to_string(d.join("recording.json")).unwrap().replace("\"rows_written\": 0", "\"rows_written\": 1");
        std::fs::write(d.join("recording.json"), json).unwrap();
        assert!(open(&d, &[]).unwrap().notes.is_empty(), "nothing to say about a whole file");
    }

    #[test]
    fn hours_decimate_through_the_summary_keeping_every_spike_and_gap() {
        let n = 1_000_000usize;
        let t: Vec<i64> = (0..n as i64).map(|i| i * 4 + if i >= 600_000 { 10_000 } else { 0 }).collect();
        let mut v: Vec<f64> = (0..n).map(|i| (i % 7) as f64).collect();
        v[123_457] = 1000.0;
        let ch = channel(t.clone(), v);
        assert!(ch.blocks.iter().all(|b| b.end - b.start <= BLOCK));
        assert!(ch.blocks.iter().all(|b| !(b.start < 600_000 && b.end > 600_000)), "a block spans the gap");
        let segs = ch.decimate(0, t[n - 1] + 1, 800, 1.0);
        assert_eq!(segs.len(), 2, "the gap must split the trace");
        let cols: usize = segs.iter().map(|s| s.len()).sum();
        assert!(cols <= 810, "{cols} columns for 800 pixels");
        assert!(segs.iter().flatten().any(|c| c.max == 1000.0), "the spike survives");
        assert!(segs.iter().flatten().all(|c| c.min == 0.0), "each column spans the full swing");
        let close = ch.decimate(t[123_450], t[123_465], 800, 2.0);
        assert_eq!(close.len(), 1);
        assert_eq!(close[0].iter().map(|c| c.max).fold(f64::MIN, f64::max), 2000.0, "scaled");
    }
}
