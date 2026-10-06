use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

use eframe::egui::{self, RichText};
use egui_plot::{HoverPosition, Legend, Line, Plot, PlotPoints, Span, VLine};

use spy_core::catalogue::flag;
use spy_core::session::{Phase, Status};
use spy_core::store::{Channel, Column, Ring};
use spy_core::timeline::Timeline;

use crate::app::SpyApp;
use crate::fields;
use crate::theme;
use crate::view::{self, Health};

#[derive(Clone)]
enum Src {
    Stream(Arc<Channel>),
    Derived(Arc<Mutex<Ring>>),
}

impl Src {
    fn lock(&self) -> MutexGuard<'_, Ring> {
        match self {
            Src::Stream(c) => c.lock(),
            Src::Derived(r) => r.lock().unwrap_or_else(|e| e.into_inner()),
        }
    }
}

#[derive(Clone)]
pub(crate) struct Member {
    pub(crate) id: String,
    pub(crate) lane: (u32, String),
    color: egui::Color32,
    factor: f64,
    hold: bool,
    reading: view::Reading,
    pub(crate) name: String,
    frozen: bool,
    pub(crate) health: Health,
    src: Option<Src>,
    min_span: f64,
    smooth_ms: u32,
    scale: Scale,
    decimals: Option<usize>,
}

impl Member {
    pub(crate) fn values(&self, from: i64, to: i64) -> Vec<(i64, f64)> {
        let Some(src) = &self.src else { return Vec::new() };
        let ring = src.lock();
        let mut zh = view::ZeroHold::new();
        let prime = if self.hold { from - view::ZERO_HOLD_MS.ceil() as i64 - 1 } else { from };
        ring.range(prime, to)
            .filter_map(|(t, v)| {
                let v = if self.hold { zh.apply_at(t, v) } else { v };
                (t >= from).then_some((t, v * self.factor))
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CursorReading {
    pub a: Option<f64>,
    pub b: Option<f64>,
    pub between: RangeStats,
}
pub const XY_HOVER: &str = "Plot one channel against another over the stretch in view (pause and scroll to pick it)";

pub const WINDOWS: [(f64, &str); 9] = [(1.0, "1 s"), (2.0, "2 s"), (5.0, "5 s"), (10.0, "10 s"), (30.0, "30 s"), (60.0, "1 min"), (120.0, "2 min"), (300.0, "5 min"), (600.0, "10 min")];

pub const SMOOTHING: [(u32, &str); 6] = [(0, "off"), (10, "10 ms"), (50, "50 ms"), (100, "100 ms"), (500, "500 ms"), (1000, "1 s")];

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Scale {
    Fit { floor: Option<f64> },
    Fixed { lo: f64, hi: f64 },
    Centred { half: f64 },
}

impl Default for Scale {
    fn default() -> Scale {
        Scale::Fit { floor: None }
    }
}

impl Scale {
    pub fn is_valid(&self) -> bool {
        match *self {
            Scale::Fit { floor: None } => true,
            Scale::Fit { floor: Some(f) } => f.is_finite() && f >= 0.0,
            Scale::Fixed { lo, hi } => lo.is_finite() && hi.is_finite() && lo < hi,
            Scale::Centred { half } => half.is_finite() && half > 0.0,
        }
    }

    pub fn range(&self, lo: f64, hi: f64, unit_floor: f64) -> (f64, f64) {
        match *self {
            Scale::Fixed { lo, hi } => (lo, hi),
            Scale::Centred { half } => (-half, half),
            Scale::Fit { floor } if lo.is_finite() && hi.is_finite() => autoscale(lo, hi, floor.unwrap_or(unit_floor)),
            Scale::Fit { .. } => (-1.0, 1.0),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RangeStats {
    pub n: usize,
    pub mean: f64,
    pub min: f64,
    pub max: f64,
    pub sd: f64,
}

pub fn range_stats(values: impl Iterator<Item = f64>) -> RangeStats {
    let (mut n, mut mean, mut m2, mut min, mut max) = (0usize, 0.0f64, 0.0f64, f64::INFINITY, f64::NEG_INFINITY);
    for v in values.filter(|v| v.is_finite()) {
        n += 1;
        let d = v - mean;
        mean += d / n as f64;
        m2 += d * (v - mean);
        min = min.min(v);
        max = max.max(v);
    }
    RangeStats { n, mean, min, max, sd: if n > 1 { (m2 / (n - 1) as f64).sqrt() } else { 0.0 } }
}

pub(crate) fn smoothed(ring: &Ring, from: i64, to: i64, columns: usize, window_ms: i64, mut transform: impl FnMut(i64, f64) -> f64) -> Vec<Vec<Column>> {
    let mut segments: Vec<Vec<Column>> = Vec::new();
    if columns == 0 || to <= from {
        return segments;
    }
    let span = (to - from) as f64;
    let gap = ring.gap_ms();
    let mut current: Vec<Column> = Vec::new();
    let mut col_idx: Option<usize> = None;
    let mut window: VecDeque<(i64, f64)> = VecDeque::new();
    let mut sum = 0.0;
    let mut prev_t: Option<i64> = None;
    let first_shown = from - gap.ceil() as i64;
    let after = ring.range(to, i64::MAX).next().map(|(t, _)| t + 1).unwrap_or(to);
    for (t, raw) in ring.range(from.saturating_sub(window_ms), after) {
        let v = transform(t, raw);
        let broken = prev_t.is_some_and(|p| (t - p) as f64 > gap);
        prev_t = Some(t);
        if broken || !v.is_finite() {
            if !current.is_empty() {
                segments.push(std::mem::take(&mut current));
            }
            col_idx = None;
            window.clear();
            sum = 0.0;
            if !v.is_finite() {
                continue;
            }
        }
        window.push_back((t, v));
        sum += v;
        while window.front().is_some_and(|&(t0, _)| t0 <= t - window_ms) {
            if let Some((_, old)) = window.pop_front() {
                sum -= old;
            }
        }
        if t < first_shown {
            continue;
        }
        let m = sum / window.len() as f64;
        let c = ((((t - from) as f64 / span) * columns as f64).floor().clamp(-1.0, columns as f64) as i64).max(0) as usize;
        if col_idx == Some(c)
            && let Some(last) = current.last_mut()
        {
            last.min = last.min.min(m);
            last.max = last.max.max(m);
            last.last_v = m;
        } else {
            current.push(Column { t, min: m, max: m, first_v: m, last_v: m });
            col_idx = Some(c);
        }
    }
    if !current.is_empty() {
        segments.push(current);
    }
    segments
}

#[derive(Default)]
struct LaneEvents {
    view: Option<(f64, f64)>,
    clicked_at: Option<f64>,
    secondary_at: Option<f64>,
    double: bool,
    drag_live: bool,
    wheel: f32,
    zoom_y: Option<(f64, f64)>,
    y: Option<(f64, f64)>,
}

const TIME_STEPS: [f64; 22] = [0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1200.0, 1800.0, 3600.0];

pub const TIME_LABEL_PX: f64 = 92.0;

pub(crate) fn time_marks(input: egui_plot::GridInput, anchor: f64) -> Vec<egui_plot::GridMark> {
    let (lo, hi) = input.bounds;
    if !(input.base_step_size > 0.0 && hi > lo && anchor.is_finite()) {
        return Vec::new();
    }
    let px = f64::from(GRID_PX) / input.base_step_size;
    let last = TIME_STEPS[TIME_STEPS.len() - 1];
    let major = TIME_STEPS.iter().copied().find(|s| s * px >= TIME_LABEL_PX).unwrap_or(last);
    let minor = TIME_STEPS.iter().copied().find(|s| s * px >= 24.0 && ((major / s).round() * s - major).abs() < 1e-9).unwrap_or(major);
    let mut out = Vec::new();
    for step in [minor, major] {
        let (k0, k1) = (((lo - anchor) / step).ceil() as i64, ((hi - anchor) / step).floor() as i64);
        if k1.saturating_sub(k0) > 10_000 {
            continue;
        }
        out.extend((k0..=k1).map(|k| egui_plot::GridMark { value: anchor + k as f64 * step, step_size: step }));
    }
    out.sort_by(|a, b| a.value.total_cmp(&b.value));
    out.dedup_by(|later, earlier| {
        let same = (later.value - earlier.value).abs() < minor * 0.1;
        if same && later.step_size > earlier.step_size {
            *earlier = *later;
        }
        same
    });
    out
}

pub const GRID_PX: f32 = 8.0;

fn time_anchor(bounds: (f64, f64), live_end: Option<f64>, tl: &Timeline) -> f64 {
    if let Some(end) = live_end {
        return end;
    }
    let mid = (bounds.0 + bounds.1) / 2.0;
    let wall = tl.wall(tl.origin().unwrap_or(0) + (mid * 1000.0).round() as i64).and_then(|w| w.duration_since(std::time::UNIX_EPOCH).ok());
    match wall {
        Some(d) => mid - f64::from(d.subsec_millis()) / 1000.0,
        None => 0.0,
    }
}

fn time_label(mark: egui_plot::GridMark, live_end: Option<f64>, tl: &Timeline) -> String {
    let x = mark.value;
    match live_end {
        Some(end) => {
            let d = x - end;
            if d.abs() < mark.step_size * 1e-3 { "now".into() } else { format!("{} s", view::fmt_short((d * 1000.0).round() / 1000.0)) }
        }
        None => match tl.wall(tl.origin().unwrap_or(0) + (x * 1000.0).round() as i64) {
            Some(w) if mark.step_size < 0.999 => {
                let t = view::local_time(w);
                t.get(..10).map_or(t.clone(), str::to_string)
            }
            Some(w) => view::local_hms(w),
            None => format!("{x:.1} s"),
        },
    }
}

impl SpyApp {
    pub(crate) fn charted(&self, i: usize, st: &spy_core::session::Status) -> bool {
        let c = &self.chans[i];
        if self.catalogue.get(c.key.signal).is_some_and(|s| s.value_type.as_deref() == Some("string")) {
            return false;
        }
        if self.session.store().get(&c.key).is_some_and(|ch| ch.lock().kind == Some(spy_core::sample::ValueKind::String)) {
            return false;
        }
        !st.channels.iter().any(|s| s.key == c.key && matches!(s.state, spy_core::session::ChannelState::Refused { .. }))
    }

    pub(crate) fn lanes(&self, charted: &[bool]) -> Vec<(u32, String)> {
        let mut v: Vec<(u32, String)> = Vec::new();
        for (i, c) in self.chans.iter().enumerate() {
            if !charted[i] {
                continue;
            }
            let lane = (c.lane, self.units_of(i));
            if !v.contains(&lane) {
                v.push(lane);
            }
        }
        for d in &self.derived {
            let lane = (d.lane, d.live.def().units().to_string());
            if !v.contains(&lane) {
                v.push(lane);
            }
        }
        v
    }

    pub(crate) fn members(&self, charted: &[bool], st: &Status) -> Vec<Member> {
        let connected = view::session_live(&st.phase);
        let store = self.session.store();
        let mut out = Vec::new();
        for (i, c) in self.chans.iter().enumerate() {
            if !charted[i] {
                continue;
            }
            let sig = self.catalogue.get(c.key.signal);
            let d = view::display(sig, c.radians).with_decimals(c.decimals);
            let cs = st.channels.iter().find(|s| s.key == c.key);
            let scale = self.chans.iter().enumerate().find(|(j, o)| o.lane == c.lane && charted[*j] && self.units_of(*j) == d.units).map_or(c.scale, |(_, o)| o.scale);
            out.push(Member {
                id: c.key.id(),
                lane: (c.lane, d.units.clone()),
                color: c.color,
                factor: d.factor,
                hold: c.hold_nonzero,
                reading: view::reading(sig),
                name: view::short_label(&self.catalogue, &c.key),
                frozen: sig.is_some_and(|s| s.has(flag::FROZEN)),
                health: view::health(cs, connected, sig, st.loopback),
                src: store.get(&c.key).map(Src::Stream),
                min_span: min_span_for(&d.units, sig),
                smooth_ms: c.smooth_ms,
                scale,
                decimals: d.decimals,
            });
        }
        for (i, d) in self.derived.iter().enumerate() {
            let def = d.live.def();
            out.push(Member {
                id: def.id(),
                lane: (d.lane, def.units().to_string()),
                color: d.color,
                factor: 1.0,
                hold: false,
                reading: crate::derived_view::reading(def),
                name: self.derived_label(def),
                frozen: false,
                health: self.derived_health(i, st),
                src: Some(Src::Derived(d.live.ring())),
                min_span: min_span(def.units()),
                smooth_ms: 0,
                scale: Scale::default(),
                decimals: crate::derived_view::decimals(def),
            });
        }
        out
    }

    fn units_of(&self, i: usize) -> String {
        let c = &self.chans[i];
        view::display(self.catalogue.get(c.key.signal), c.radians).units
    }

    pub fn lane_view_range(&self, lane: u32, units: &str) -> Option<(f64, f64)> {
        self.lane_ranges.get(&(lane, units.to_string())).copied()
    }

    pub(crate) fn cursor_readings(&self, st: &Status) -> HashMap<String, CursorReading> {
        let charted: Vec<bool> = (0..self.chans.len()).map(|i| self.charted(i, st)).collect();
        let origin = st.timeline.origin().unwrap_or(0);
        let (a, b) = (self.cursor_a, self.cursor_b);
        let (from, to) = match (a, b) {
            (Some(a), Some(b)) => (origin + (a.min(b) * 1000.0).floor() as i64, origin + (a.max(b) * 1000.0).ceil() as i64 + 1),
            _ => self.view_ms.unwrap_or((i64::MIN, i64::MAX)),
        };
        let mut out = HashMap::new();
        for m in self.members(&charted, st) {
            let Some(src) = &m.src else {
                out.insert(m.id.clone(), CursorReading::default());
                continue;
            };
            let r = src.lock();
            let at = |x: f64| view::value_at(&r, m.reading, origin + (x * 1000.0).round() as i64).map(|v| v * m.factor);
            let s = view::window_stats(&r, m.reading, from, to);
            let between = RangeStats { n: s.n, mean: s.mean * m.factor, min: s.min * m.factor, max: s.max * m.factor, sd: s.sd * m.factor };
            out.insert(m.id.clone(), CursorReading { a: a.and_then(at), b: b.and_then(at), between });
        }
        out
    }

    pub fn charts(&mut self, ui: &mut egui::Ui) {
        let p = theme::pal(ui);
        self.chart_toolbar(ui);
        let st = self.session.status().clone();
        if self.chans.is_empty() {
            ui.centered_and_justified(|ui| ui.label(RichText::new("Charts appear here once channels are added.").color(p.ink2)));
            return;
        }
        let tl = st.timeline.clone();
        let Some(newest) = self.session.store().newest() else {
            ui.centered_and_justified(|ui| ui.label(RichText::new(if st.phase.is_connected() { "Waiting for the first samples..." } else { "Connect to a controller to see the charts." }).color(p.ink2)));
            return;
        };
        if st.phase != Phase::Streaming && self.paused_at.is_none() {
            ui.horizontal(|ui| {
                theme::square(ui, p.hold, 10.0);
                ui.label(theme::b("Not streaming: the charts show the last data received.").color(p.hold));
            });
        }
        let end_ms = self.paused_at.unwrap_or(newest);
        let x_max = tl.seconds(end_ms);
        let x_min = x_max - self.window_s;
        let live = self.paused_at.is_none();
        let x_right = if live { x_max + self.window_s * 18.0 / f64::from((ui.available_width() - 60.0).max(100.0)) } else { x_max };
        let charted: Vec<bool> = (0..self.chans.len()).map(|i| self.charted(i, &st)).collect();
        let all_lanes = self.lanes(&charted);
        if all_lanes.is_empty() {
            ui.centered_and_justified(|ui| ui.label(RichText::new("Nothing to chart: the channels are text signals or were refused (see their rows).").color(p.ink2)));
            return;
        }
        let all = self.members(&charted, &st);
        if self.expanded.as_ref().is_some_and(|e| !all_lanes.contains(e)) {
            self.expanded = None;
        }
        let lanes: Vec<(u32, String)> = match &self.expanded {
            Some(e) => vec![e.clone()],
            None => all_lanes.clone(),
        };
        if self.expanded.is_some() {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("showing 1 of {} charts", all_lanes.len())).color(p.ink2));
                if theme::icon_text_button(ui, theme::Icon::Collapse, "show all charts", 34.0, false).clicked() {
                    self.expanded = None;
                }
            });
        }
        let title_h = 30.0;
        let axis_h = 26.0;
        let lane_gap = 8.0;
        let lane_overhead = title_h + lane_gap + 2.0 * ui.spacing().item_spacing.y;
        let fit = (ui.available_height() - axis_h) / lanes.len() as f32 - lane_overhead;
        let lane_h = fit.max(90.0);
        let every_axis = fit < 90.0;

        let mark_tol = self.view_ms.map_or(self.window_s, |(a, b)| (b - a) as f64 / 1000.0) * 6.0 / f64::from(ui.available_width().max(100.0));
        let markers: Vec<(f64, String)> = self.markers.iter().map(|m| (tl.seconds(m.t_ms), m.label.clone())).collect();
        let events: Vec<(f64, egui::Color32, String)> = self.events_on_timeline(&tl).into_iter().map(|(t, e)| (tl.seconds(t), e.color(p), format!("controller: {}", e.text()))).collect();
        let (ca, cb, cursors_on) = (self.cursor_a, self.cursor_b, self.cursors_on);
        let mut marks: Vec<(f64, String)> = markers.iter().map(|(x, l)| (*x, format!("marker {l}"))).collect();
        marks.extend(events.iter().map(|(x, _, l)| (*x, l.clone())));
        if cursors_on {
            marks.extend(ca.map(|a| (a, "cursor A".to_string())));
            marks.extend(cb.map(|b| (b, "cursor B".to_string())));
        }
        let pause_fresh = self.pause_fresh;
        let origin = tl.origin().unwrap_or(0);
        let shown_x = match self.view_ms {
            Some((a, b)) if !live => ((a - origin) as f64 / 1000.0, (b - origin) as f64 / 1000.0),
            _ => (x_min, x_max),
        };
        let mut transforms = Vec::new();
        let mut events_out: Vec<((u32, String), LaneEvents)> = Vec::new();
        let mut title_acts: Vec<((u32, String), TitleAct)> = Vec::new();
        egui::ScrollArea::vertical().id_salt("lanes").auto_shrink([false, false]).show(ui, |ui| {
            for (k, lane) in lanes.iter().enumerate() {
                let members: Vec<&Member> = all.iter().filter(|m| &m.lane == lane).collect();
                let Some(first) = members.first() else { continue };
                let hover_id = egui::Id::new(("lane-hovered", lane.0, &lane.1));
                let hovered_before = ui.data(|d| d.get_temp::<bool>(hover_id)).unwrap_or(false);
                let zoomed = self.lane_zoom.get(lane).copied();
                let top = ui.cursor().top();
                let in_view: Vec<&str> = markers.iter().filter(|(x, _)| (shown_x.0..=shown_x.1).contains(x)).map(|(_, l)| l.as_str()).collect();
                let act = lane_title(ui, &members, zoomed.is_some(), self.expanded.as_ref() == Some(lane), hovered_before, if k == 0 { in_view.last().copied() } else { None });
                if act != TitleAct::None {
                    title_acts.push((lane.clone(), act));
                }
                let tl2 = tl.clone();
                let units = lane.1.clone();
                let decimals = first.decimals;
                let shown = self.hover_text.clone();
                let marks2 = marks.clone();
                let last_lane = k + 1 == lanes.len();
                let (tl3, tl4) = (tl.clone(), tl.clone());
                let live_end = live.then_some(x_max);
                let mut plot = Plot::new(("lane", lane.0, &lane.1))
                    .height(lane_h)
                    .link_axis("x-link", [true, false])
                    .link_cursor("x-link", [true, false])
                    .custom_x_axes(vec![theme::time_axis(p, move |mark, _| time_label(mark, live_end, &tl3))])
                    .grid_spacing(GRID_PX..=300.0)
                    .x_grid_spacer(move |input| {
                        let anchor = time_anchor(input.bounds, live_end, &tl4);
                        time_marks(input, anchor)
                    })
                    .custom_y_axes(vec![theme::value_axis(p)])
                    .show_axes([last_lane || every_axis, true])
                    .label_formatter(move |pos| remember(&shown, hover_label(pos, &tl2, &units, decimals, &marks2, mark_tol)))
                    .allow_drag([true, false])
                    .allow_zoom(false)
                    .allow_scroll(false)
                    .allow_boxed_zoom(false)
                    .allow_double_click_reset(false);
                if members.len() > 1 {
                    plot = plot.legend(Legend::default().position(egui_plot::Corner::LeftTop));
                }
                let (scale, min_span) = (first.scale, members.iter().map(|m| m.min_span).fold(0.0, f64::max));
                let resp = plot.show(ui, |pu| {
                    let mut ev = LaneEvents::default();
                    let b = pu.plot_bounds();
                    let dragged = pu.response().dragged();
                    let (mut vx0, mut vx1) = if (live && !dragged) || pause_fresh || !b.is_valid_x() { (x_min, x_right) } else { (b.min()[0], b.max()[0]) };
                    let hovered = pu.response().hovered();
                    let (zoom, scroll) = if hovered { pu.ctx().input(|i| (i.zoom_delta(), i.smooth_scroll_delta.y)) } else { (1.0, 0.0) };
                    if hovered && (zoom != 1.0 || scroll != 0.0) {
                        pu.ctx().input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
                    }
                    if scroll != 0.0 {
                        if live {
                            ev.wheel = scroll;
                        } else if let Some(px) = pu.pointer_coordinate().map(|q| q.x) {
                            let f = f64::from((-scroll * 0.0025).exp());
                            let w = ((vx1 - vx0) * f).clamp(0.05, 600.0);
                            let k = (px - vx0) / (vx1 - vx0).max(1e-9);
                            (vx0, vx1) = (px - w * k, px - w * k + w);
                            pu.set_plot_bounds_x(vx0..=vx1);
                        }
                    }
                    let from = origin + (vx0 * 1000.0).floor() as i64;
                    let to = origin + (vx1 * 1000.0).ceil() as i64 + 1;
                    let px = pu.response().rect.width().max(50.0) as usize;
                    let (mut ymin, mut ymax) = (f64::INFINITY, f64::NEG_INFINITY);
                    for m in &members {
                        let Some(src) = &m.src else { continue };
                        let (factor, hold) = (m.factor, m.hold);
                        let ring = src.lock();
                        let mut zh = view::ZeroHold::new();
                        let smooth = i64::from(m.smooth_ms);
                        if hold && smooth == 0 {
                            for (t, v) in ring.range(from - view::ZERO_HOLD_MS.ceil() as i64 - 1, from) {
                                zh.apply_at(t, v);
                            }
                        }
                        let segs = if smooth > 0 {
                            smoothed(&ring, from, to, px, smooth, |t, v| (if hold { zh.apply_at(t, v) } else { v }) * factor)
                        } else {
                            ring.decimate_at(from, to, px, |t, v| (if hold { zh.apply_at(t, v) } else { v }) * factor)
                        };
                        let last = ring.last().map(|(t, _)| t);
                        drop(ring);
                        for (si, seg) in segs.iter().enumerate() {
                            let mut pts: Vec<[f64; 2]> = Vec::with_capacity(seg.len() * 2);
                            for c in seg {
                                let x = (c.t - origin) as f64 / 1000.0;
                                pts.push([x, c.first_v]);
                                if c.min != c.max {
                                    pts.push([x, c.min]);
                                    pts.push([x, c.max]);
                                }
                                if c.last_v != c.first_v {
                                    pts.push([x, c.last_v]);
                                }
                                if x >= vx0 && x <= vx1 {
                                    ymin = ymin.min(c.min);
                                    ymax = ymax.max(c.max);
                                }
                            }
                            if pts.len() == 1 {
                                pts.push([pts[0][0] + 0.0005, pts[0][1]]);
                            }
                            pu.line(Line::new(m.name.clone(), PlotPoints::from(pts)).color(m.color).width(2.0).id(egui::Id::new((&m.id, si))));
                        }
                        if matches!(m.health, Health::Stale | Health::NotConnected)
                            && let Some(t) = last
                        {
                            let x = (t - origin) as f64 / 1000.0;
                            if x < vx1 {
                                pu.span(Span::new(format!("{} stale", m.name), x..=vx1).fill(p.hold.gamma_multiply(0.10)).border_width(0.0));
                            }
                        }
                    }
                    for (x, _) in &markers {
                        if *x >= vx0 && *x <= vx1 {
                            pu.vline(VLine::new("", *x).color(p.hold).width(2.0).style(egui_plot::LineStyle::dashed_dense()));
                        }
                    }
                    for (x, color, _) in &events {
                        if *x >= vx0 && *x <= vx1 {
                            pu.vline(VLine::new("", *x).color(*color).width(1.5).style(egui_plot::LineStyle::dashed_loose()));
                        }
                    }
                    if cursors_on {
                        if let Some(a) = ca {
                            pu.vline(VLine::new("", a).color(p.ink).width(2.5));
                        }
                        if let Some(bx) = cb {
                            pu.vline(VLine::new("", bx).color(p.ink).width(2.0).style(egui_plot::LineStyle::dashed_dense()));
                        }
                    }
                    if (live && !dragged) || pause_fresh {
                        pu.set_plot_bounds_x(x_min..=x_right);
                    }
                    ev.drag_live = live && dragged;
                    let (mut y0, mut y1) = match zoomed {
                        Some(z) => z,
                        None => scale.range(ymin, ymax, min_span),
                    };
                    if zoom != 1.0
                        && let Some(py) = pu.pointer_coordinate().map(|q| q.y)
                    {
                        let z = f64::from(zoom);
                        (y0, y1) = (py - (py - y0) / z, py + (y1 - py) / z);
                        ev.zoom_y = Some((y0, y1));
                    }
                    pu.set_plot_bounds_y(y0..=y1);
                    ev.view = Some((vx0, vx1));
                    ev.y = Some((y0, y1));
                    let x_at = pu.pointer_coordinate().map(|q| q.x);
                    if pu.response().clicked() {
                        ev.clicked_at = x_at;
                    }
                    if pu.response().secondary_clicked() {
                        ev.secondary_at = x_at;
                    }
                    ev.double = pu.response().double_clicked();
                    ev
                });
                transforms.push(resp.transform);
                let lane_rect = egui::Rect::from_min_max(egui::pos2(resp.response.rect.left(), top), resp.response.rect.right_bottom());
                let now_hovered = ui.rect_contains_pointer(lane_rect.expand2(egui::vec2(60.0, 0.0)));
                ui.data_mut(|d| d.insert_temp(hover_id, now_hovered));
                if now_hovered != hovered_before {
                    ui.ctx().request_repaint();
                }
                events_out.push((lane.clone(), resp.inner));
                ui.add_space(lane_gap);
            }
        });
        self.lane_transforms = transforms;
        self.pause_fresh = false;
        let mut visible_x = (x_min, x_max);
        for (lane, ev) in events_out {
            if let Some(v) = ev.view {
                visible_x = v;
            }
            if let Some(y) = ev.y {
                self.lane_ranges.insert(lane.clone(), y);
            }
            if let Some(z) = ev.zoom_y {
                self.lane_zoom.insert(lane.clone(), z);
            }
            if ev.drag_live {
                self.paused_at = Some(newest);
            }
            if ev.wheel != 0.0 {
                self.wheel_window(ev.wheel);
            }
            if ev.double {
                self.lane_zoom.remove(&lane);
                if self.paused_at.is_some() {
                    self.pause_fresh = true;
                }
            }
            if self.cursors_on {
                if let Some(a) = ev.clicked_at {
                    self.cursor_a = Some(a);
                }
                if let Some(b) = ev.secondary_at {
                    self.cursor_b = Some(b);
                }
            }
        }
        for (lane, act) in title_acts {
            match act {
                TitleAct::Expand => self.expanded = Some(lane),
                TitleAct::Collapse => self.expanded = None,
                TitleAct::ZoomIn | TitleAct::ZoomOut => {
                    if let Some((a, b)) = self.lane_zoom.get(&lane).copied().or_else(|| self.lane_ranges.get(&lane).copied()) {
                        let (c, half) = ((a + b) / 2.0, (b - a) / 2.0 * if act == TitleAct::ZoomIn { 0.5 } else { 2.0 });
                        self.lane_zoom.insert(lane, (c - half, c + half));
                    }
                }
                TitleAct::ResetScale => {
                    self.lane_zoom.remove(&lane);
                }
                TitleAct::None => {}
            }
        }
        self.view_ms = Some((origin + (visible_x.0 * 1000.0).floor() as i64, origin + (visible_x.1 * 1000.0).ceil() as i64 + 1));
    }

    fn wheel_window(&mut self, delta: f32) {
        self.wheel_acc += delta;
        const NOTCH: f32 = 40.0;
        while self.wheel_acc.abs() >= NOTCH - 0.5 {
            let up = self.wheel_acc > 0.0;
            self.wheel_acc -= NOTCH.copysign(self.wheel_acc);
            let i = WINDOWS.iter().position(|(s, _)| *s >= self.window_s - 1e-9).unwrap_or(WINDOWS.len() - 1);
            let j = if up { i.saturating_sub(1) } else { (i + 1).min(WINDOWS.len() - 1) };
            if WINDOWS[j].0 != self.window_s {
                self.window_s = WINDOWS[j].0;
                self.mark_settings_dirty();
            }
        }
    }

    fn chart_toolbar(&mut self, ui: &mut egui::Ui) {
        let p = theme::pal(ui);
        let paused = self.paused_at.is_some();
        let window_s = self.window_s;
        let cursors_on = self.cursors_on;
        let placed = self.cursor_a.is_some() || self.cursor_b.is_some();
        let markers = self.markers.len();
        let xy_open = self.xy.is_some();
        let mut label = std::mem::take(&mut self.marker_text);
        let (mut window, mut pause, mut cursors, mut clear_cursors, mut marker, mut clear_markers) = (None, false, false, false, false, false);
        let (mut xy, mut png, mut csv) = (false, false, false);
        ui.scope(|ui| {
            ui.spacing_mut().button_padding.x = 9.0;
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.spacing_mut().interact_size.y = theme::TOOL_H;
            let cur = WINDOWS.iter().find(|(s, _)| (s - window_s).abs() < 1e-9).map(|(_, l)| *l).unwrap_or("custom");
            let cursors_text = if cursors_on { "cursors: on" } else { "cursors" };
            let mut left_texts = vec![cur, if paused { "back to live" } else { "pause" }, cursors_text, "marker"];
            if cursors_on && placed {
                left_texts.push("clear cursors");
            }
            let extra = 20.0 + 32.0 + if paused { 80.0 } else { 0.0 };
            let left_w = theme::buttons_width(ui, &left_texts, extra);
            let right_w = theme::buttons_width(ui, &["save csv", "save png", "xy plot"], 0.0);
            theme::section_tools(
                ui,
                "02",
                "charts",
                left_w,
                right_w,
                |ui| {
                    let r = theme::drop_button(ui, cur, theme::TOOL_H).on_hover_text("How much the charts show: the wheel over a chart changes it too");
                    egui::Popup::menu(&r).show(|ui| {
                        for (s, l) in WINDOWS {
                            if ui.selectable_label((s - window_s).abs() < 1e-9, l).clicked() {
                                window = Some(s);
                                ui.close();
                            }
                        }
                    });
                    if paused {
                        pause = theme::primary(ui, "back to live", theme::TOOL_H).on_hover_text("Space").clicked();
                        theme::badge(ui, "paused", p.hold, "The charts are held: drag to move through the last 10 minutes, the wheel zooms time, Ctrl + wheel the vertical scale.");
                    } else {
                        pause = theme::tool(ui, "pause").on_hover_text("Space. While paused, drag to move back through the last 10 minutes; the wheel zooms time.").clicked();
                    }
                    cursors = ui.add(egui::Button::new(cursors_text).selected(cursors_on).min_size(egui::vec2(0.0, theme::TOOL_H))).on_hover_text("Click a chart to place cursor A, right-click for cursor B: each channel's row reads them").clicked();
                    if cursors_on && placed {
                        clear_cursors = theme::tool(ui, "clear cursors").clicked();
                    }
                    marker = theme::split(
                        ui,
                        "Marker label",
                        |ui| theme::tool(ui, "marker").on_hover_text("M: mark this moment on the charts and in any running recording"),
                        |ui| {
                            ui.label(theme::b("the next marker's label"));
                            fields::line(ui, &mut label, "Marker label", |t| t.hint_text(format!("M{}", markers + 1)).desired_width(220.0));
                            if markers > 0 && ui.button("clear the markers").clicked() {
                                clear_markers = true;
                            }
                        },
                    )
                    .clicked();
                },
                |ui| {
                    xy = ui.add(egui::Button::new("xy plot").selected(xy_open).min_size(egui::vec2(0.0, theme::TOOL_H))).on_hover_text(XY_HOVER).clicked();
                    png = theme::tool(ui, "save png").on_hover_text("Save a picture of the charts as drawn to the recordings folder").clicked();
                    csv = theme::tool(ui, "save csv").on_hover_text("Save every channel's samples in view, as they came (never smoothed), to a CSV file in the recordings folder").clicked();
                },
            );
        });
        self.marker_text = label;
        if let Some(s) = window {
            self.window_s = s;
            self.mark_settings_dirty();
            if self.paused_at.is_some() {
                if let Some((_, end)) = self.view_ms {
                    self.paused_at = Some(end - 1);
                }
                self.pause_fresh = true;
            }
        }
        if pause {
            self.toggle_pause();
        }
        if cursors {
            self.cursors_on = !self.cursors_on;
        }
        if clear_cursors {
            self.cursor_a = None;
            self.cursor_b = None;
        }
        if marker {
            self.add_marker();
        }
        if clear_markers {
            self.markers.clear();
        }
        if xy {
            self.toggle_xy();
        }
        if png {
            self.request_png(crate::export::Picture::Charts);
        }
        if csv {
            self.export_live_csv();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TitleAct {
    None,
    ZoomIn,
    ZoomOut,
    ResetScale,
    Expand,
    Collapse,
}

fn lane_title(ui: &mut egui::Ui, members: &[&Member], zoomed: bool, expanded: bool, hovered: bool, marker: Option<&str>) -> TitleAct {
    let p = theme::pal(ui);
    let Some(first) = members.first() else { return TitleAct::None };
    let mut act = TitleAct::None;
    let smooth: Vec<u32> = members.iter().map(|m| m.smooth_ms).filter(|&s| s > 0).collect();
    let smoothed = smooth.first().map(|&s| {
        if smooth.iter().all(|&x| x == s) && smooth.len() == members.len() { format!("smoothed {}", SMOOTHING.iter().find(|(ms, _)| *ms == s).map_or_else(|| format!("{s} ms"), |(_, t)| t.to_string())) } else { "partly smoothed".into() }
    });
    ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 30.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let size = egui::vec2(32.0, 28.0);
        if hovered || zoomed || expanded {
            ui.scope(|ui| {
                ui.visuals_mut().widgets.inactive.weak_bg_fill = p.chip;
                if expanded {
                    if theme::icon_button(ui, theme::Icon::Collapse, "Back to all the charts", size).clicked() {
                        act = TitleAct::Collapse;
                    }
                } else if theme::icon_button(ui, theme::Icon::Expand, "Fill the middle with this chart", size).clicked() {
                    act = TitleAct::Expand;
                }
                if theme::icon_button(ui, theme::Icon::Plus, "Zoom in the vertical scale", size).clicked() {
                    act = TitleAct::ZoomIn;
                }
                if theme::icon_button(ui, theme::Icon::Minus, "Zoom out the vertical scale", size).clicked() {
                    act = TitleAct::ZoomOut;
                }
                if zoomed && ui.add(egui::Button::new(theme::b("reset scale").size(15.0)).min_size(egui::vec2(0.0, 28.0))).clicked() {
                    act = TitleAct::ResetScale;
                }
            });
        }
        if let Some(m) = marker {
            ui.add_space(4.0);
            ui.label(theme::b(m).color(p.hold)).on_hover_text("The newest marker in view");
        }
        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            for m in members.iter().take(6) {
                theme::square(ui, m.color, 12.0);
            }
            let tag_w = |t: &str| egui::WidgetText::from(theme::b(t).size(14.0)).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Small).size().x + 20.0;
            let mut reserve = egui::WidgetText::from(first.lane.1.as_str()).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Body).size().x + 8.0;
            reserve += smoothed.as_deref().map_or(0.0, tag_w) + if zoomed { tag_w("scale zoomed") } else { 0.0 } + if members.iter().any(|m| m.frozen) { tag_w("FROZEN") } else { 0.0 };
            let title = if members.len() == 1 { first.name.clone() } else { format!("{} (+{} overlaid)", first.name, members.len() - 1) };
            let room = (ui.available_width() - reserve).max(60.0);
            ui.allocate_ui_with_layout(egui::vec2(room, 30.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add(egui::Label::new(theme::b(title)).truncate());
            });
            ui.label(RichText::new(&first.lane.1).color(p.ink3));
            if let Some(text) = &smoothed {
                theme::badge(ui, text, p.ink2, "Smoothed on the screen only (the channel's options). Recordings and saved files keep every sample.");
            }
            if zoomed {
                theme::badge(ui, "scale zoomed", p.hold, "Ctrl + wheel or the buttons set this scale; 'reset scale' or a double-click gives the chart's own back.");
            }
            if members.iter().any(|m| m.frozen) {
                theme::badge(ui, "FROZEN", p.hold, "Holds the last RAPID path position; does not move while EGM drives the robot.");
            }
        });
    });
    act
}

pub fn autoscale(lo: f64, hi: f64, min_span: f64) -> (f64, f64) {
    let mag = lo.abs().max(hi.abs());
    let span = hi - lo;
    if span <= mag * 1e-6 + 1e-12 {
        let c = (lo + hi) / 2.0;
        let half = (c.abs() * 0.05).max(1.0).max(min_span / 2.0);
        return (c - half, c + half);
    }
    if span < min_span {
        let c = (lo + hi) / 2.0;
        let half = min_span * 1.08 / 2.0;
        return (c - half, c + half);
    }
    (lo - span * 0.08, hi + span * 0.08)
}

pub fn nice(v: f64, up: bool) -> f64 {
    if !v.is_finite() || v == 0.0 {
        return if v.is_finite() { 0.0 } else { v };
    }
    let exp = v.abs().log10().floor() as i32;
    let step = 10f64.powi(exp - 1);
    let k = v / step;
    let r = if up { k.ceil() } else { k.floor() } * step;
    let digits = (1 - exp).max(0) as usize;
    format!("{r:.digits$}").parse().unwrap_or(r)
}

pub fn min_span(units: &str) -> f64 {
    match units.trim() {
        "deg" => 0.01,
        "rad" => 0.01_f64.to_radians(),
        "deg/s" => 0.5,
        "rad/s" => 0.5_f64.to_radians(),
        "deg/s2" => 1.0,
        "rad/s2" | "rad/s^2" => 1.0_f64.to_radians(),
        "V" | "Nm" => 1.0,
        "A" => 0.1,
        "m" => 0.0001,
        "m/s" => 0.001,
        "fraction" | "0..1" | "quaternion" => 0.01,
        "0 or 1" | "count" | "index" => 1.0,
        _ => 0.0,
    }
}

pub fn min_span_for(units: &str, sig: Option<&spy_core::catalogue::Signal>) -> f64 {
    let motor_angle = sig.is_some_and(|s| s.category == "motor position" || (s.category == "drive / inverter" && matches!(s.units.as_str(), "rad" | "deg")));
    match (units.trim(), motor_angle) {
        ("deg", true) => 0.05,
        ("rad", true) => 0.05_f64.to_radians(),
        (u, _) => min_span(u),
    }
}

fn hover_label(pos: &HoverPosition<'_>, tl: &Timeline, units: &str, decimals: Option<usize>, marks: &[(f64, String)], tol: f64) -> Option<String> {
    let (name, p) = match pos {
        HoverPosition::NearDataPoint { plot_name, position, .. } => (Some(*plot_name), *position),
        HoverPosition::Elsewhere { position } => (None, *position),
    };
    let t = tl.origin().unwrap_or(0) + (p.x * 1000.0).round() as i64;
    let wall = tl.wall(t).map(view::local_time).unwrap_or_default();
    let value = view::fmt_to(p.y, decimals);
    let text = match name {
        Some(n) => format!("{n}\n{value} {units}\nt = {:.3} s   {wall}", p.x),
        None => format!("t = {:.3} s   {wall}\n{value} {units}", p.x),
    };
    Some(with_mark(text, p.x, tol, marks))
}

pub fn remember(shown: &std::sync::Mutex<String>, text: Option<String>) -> Option<String> {
    if let (Some(t), Ok(mut s)) = (&text, shown.lock()) {
        s.clone_from(t);
    }
    text
}

pub fn with_mark(text: String, x: f64, tol: f64, marks: &[(f64, String)]) -> String {
    let near: Vec<&str> = marks.iter().filter(|m| (m.0 - x).abs() <= tol).map(|m| m.1.as_str()).collect();
    if near.is_empty() { text } else { format!("{}\n{text}", near.join("\n")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hover_over_a_marks_line_names_it() {
        let marks = vec![(1.0, "marker M1".to_string()), (4.0, "controller: 10010 Motors OFF state (information)".to_string()), (4.5, "cursor A".to_string())];
        let tl = Timeline::default();
        let at = |x: f64| hover_label(&HoverPosition::Elsewhere { position: egui_plot::PlotPoint::new(x, 1.0) }, &tl, "Nm", None, &marks, 0.1).unwrap();
        assert!(at(4.05).starts_with("controller: 10010 Motors OFF state (information)\nt = 4.050 s"), "{}", at(4.05));
        assert!(at(4.42).starts_with("cursor A\n"), "the nearest: {}", at(4.42));
        assert!(at(0.95).starts_with("marker M1\n"), "{}", at(0.95));
        assert!(at(2.0).starts_with("t = 2.000 s"), "no mark within reach: {}", at(2.0));
        let two = vec![(4.0, "controller: 10002 Program pointer has been reset (information)".to_string()), (4.0, "controller: 10011 Motors ON state (information)".to_string())];
        let text = hover_label(&HoverPosition::Elsewhere { position: egui_plot::PlotPoint::new(4.01, 1.0) }, &tl, "Nm", None, &two, 0.1).unwrap();
        assert!(text.starts_with("controller: 10002") && text.contains("\ncontroller: 10011 Motors ON state (information)\nt = "), "{text}");
        let near = HoverPosition::NearDataPoint { plot_name: "4002 · Torque", position: egui_plot::PlotPoint::new(3.95, 1.0), index: 0 };
        let text = hover_label(&near, &tl, "Nm", None, &marks, 0.1).unwrap();
        assert!(text.starts_with("controller: 10010") && text.contains("\n4002 · Torque\n"), "{text}");
        let text = hover_label(&near, &tl, "Nm", Some(1), &marks, 0.1).unwrap();
        assert!(text.contains("\n1.0 Nm\n"), "the chart's decimals: {text}");
    }

    fn ring(t0: i64, values: &[f64]) -> Ring {
        let mut r = Ring::new(4.0);
        for (i, &v) in values.iter().enumerate() {
            r.push(t0 + i as i64 * 4, v);
        }
        r
    }

    fn points(segs: &[Vec<Column>]) -> Vec<(i64, f64)> {
        segs.iter().flatten().flat_map(|c| [(c.t, c.first_v), (c.t, c.last_v)]).collect()
    }

    #[test]
    fn smoothing_is_a_trailing_mean_that_never_reaches_across_a_gap() {
        let swing: Vec<f64> = (0..200).map(|i| if i % 2 == 0 { 0.0 } else { 10.0 }).collect();
        let r = ring(0, &swing);
        let segs = smoothed(&r, 200, 800, 10_000, 100, |_, v| v);
        let pts = points(&segs);
        assert!(!pts.is_empty());
        assert!(pts.iter().all(|&(_, v)| (v - 5.0).abs() <= 0.21), "the swing is not smoothed: {:?}", &pts[..4]);
        let mut step = vec![0.0; 250];
        step.extend(vec![10.0; 100]);
        let r2 = ring(0, &step);
        let pts = points(&smoothed(&r2, 900, 1400, 10_000, 100, |_, v| v));
        assert!(pts.iter().any(|&(t, v)| t < 1000 && v == 0.0), "before the step: {:?}", &pts[..2]);
        let late: Vec<&(i64, f64)> = pts.iter().filter(|&&(t, _)| t >= 1100).collect();
        assert!(!late.is_empty() && late.iter().all(|&&(_, v)| v == 10.0), "the mean still holds samples from before its window: {:?}", &late[..late.len().min(3)]);
        let raw = points(&r.decimate_at(200, 800, 10_000, |_, v| v));
        assert!(raw.iter().any(|&(_, v)| v == 0.0) && raw.iter().any(|&(_, v)| v == 10.0));
        let mut r = ring(0, &[0.0; 50]);
        for i in 0..50 {
            r.push(1200 + i * 4, 10.0);
        }
        let segs = smoothed(&r, 0, 1400, 10_000, 500, |_, v| v);
        assert_eq!(segs.len(), 2, "averaged across the gap");
        assert_eq!(segs[1][0].first_v, 10.0, "the first sample after the gap carries the one before it");
        let mut seen = Vec::new();
        let r = ring(0, &swing);
        let _ = smoothed(&r, 400, 600, 100, 100, |t, v| {
            seen.push(t);
            v
        });
        assert!(seen.first().is_some_and(|&t| t <= 300) && seen.windows(2).all(|w| w[0] < w[1]), "{:?}", &seen[..3]);
    }

    #[test]
    fn a_scale_is_fit_with_a_floor_fixed_or_around_zero() {
        assert_eq!(Scale::default().range(10.0, 20.0, 1.0), (9.2, 20.8));
        let (a, b) = Scale::Fit { floor: None }.range(45.0 - 0.0001, 45.0 + 0.0001, 0.01);
        assert!(b - a >= 0.01, "the unit's floor: {}", b - a);
        let (a, b) = Scale::Fit { floor: Some(4.0) }.range(45.0 - 0.0001, 45.0 + 0.0001, 0.01);
        assert!(b - a >= 4.0 && a < 45.0 && b > 45.0, "a chosen floor: {a} to {b}");
        assert_eq!(Scale::Fit { floor: None }.range(f64::INFINITY, f64::NEG_INFINITY, 1.0), (-1.0, 1.0), "nothing in view");
        assert_eq!(Scale::Fixed { lo: -240.0, hi: 240.0 }.range(0.0, 1000.0, 1.0), (-240.0, 240.0));
        assert_eq!(Scale::Centred { half: 50.0 }.range(10.0, 20.0, 1.0), (-50.0, 50.0));
        assert!(Scale::Fixed { lo: 1.0, hi: 2.0 }.is_valid() && Scale::Centred { half: 0.5 }.is_valid() && Scale::Fit { floor: Some(0.0) }.is_valid());
        for bad in [Scale::Fixed { lo: 2.0, hi: 2.0 }, Scale::Fixed { lo: 3.0, hi: 2.0 }, Scale::Fixed { lo: f64::NAN, hi: 2.0 }, Scale::Centred { half: 0.0 }, Scale::Centred { half: f64::INFINITY }, Scale::Fit { floor: Some(-1.0) }] {
            assert!(!bad.is_valid(), "{bad:?}");
        }
    }

    #[test]
    fn the_time_axis_ticks_back_from_now_and_on_whole_seconds_paused() {
        let input = |lo: f64, hi: f64| egui_plot::GridInput { bounds: (lo, hi), base_step_size: (hi - lo) / 650.0 * f64::from(GRID_PX) };
        let px = |step: f64| step * 650.0 / 10.0;
        let marks = time_marks(input(100.37, 110.37), 110.37);
        let labelled: Vec<f64> = marks.iter().filter(|m| px(m.step_size) >= TIME_LABEL_PX).map(|m| m.value).collect();
        assert!(labelled.len() >= 4, "{labelled:?}");
        let step = marks.iter().map(|m| m.step_size).fold(0.0, f64::max);
        assert!(labelled.iter().all(|v| ((110.37 - v) / step).fract().abs() < 1e-9 || ((110.37 - v) / step).fract().abs() > 1.0 - 1e-9), "{labelled:?} by {step}");
        assert!(labelled.iter().any(|v| (v - 110.37).abs() < 1e-9), "the live edge has its tick");
        let tl = Timeline::default();
        let now = egui_plot::GridMark { value: 110.37, step_size: step };
        assert_eq!(time_label(now, Some(110.37), &tl), "now");
        assert_eq!(time_label(egui_plot::GridMark { value: 110.37 - 2.0 * step, step_size: step }, Some(110.37), &tl), format!("-{} s", view::fmt_short(2.0 * step)));
        assert!(marks.windows(2).all(|w| w[1].value - w[0].value > 1e-6), "two marks in one place");
        assert!(marks.iter().any(|m| m.step_size < step), "no finer grid");
        let marks = time_marks(input(100.37, 110.37), 100.0);
        assert!(marks.iter().filter(|m| px(m.step_size) >= TIME_LABEL_PX).all(|m| (m.value - m.value.round()).abs() < 1e-9), "{marks:?}");
        assert!(time_marks(input(5.0, 5.0), 5.0).is_empty());
    }

    #[test]
    fn a_fixed_scale_starts_from_round_ends_outside_what_was_shown() {
        assert_eq!((nice(-237.9, false), nice(237.98, true)), (-240.0, 240.0));
        assert_eq!((nice(346.57, false), nice(357.3, true)), (340.0, 360.0));
        assert_eq!((nice(0.0123, false), nice(0.0123, true)), (0.012, 0.013));
        assert_eq!(nice(500.0, true), 500.0, "already round");
        assert_eq!(nice(0.0, true), 0.0);
        assert!(nice(f64::NAN, true).is_nan());
    }

    #[test]
    fn statistics_are_exact_and_skip_nan() {
        let s = range_stats([2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0, f64::NAN].into_iter());
        assert_eq!(s.n, 8);
        assert_eq!(s.mean, 5.0);
        assert_eq!((s.min, s.max), (2.0, 9.0));
        assert!((s.sd - 2.138089935).abs() < 1e-8, "sample standard deviation: {}", s.sd);
        assert_eq!(range_stats(std::iter::empty()).n, 0);
    }

    #[test]
    fn a_flat_signal_gets_a_readable_band() {
        assert_eq!(autoscale(0.0, 0.0, 0.0), (-1.0, 1.0));
        let (a, b) = autoscale(356.7, 356.7, 1.0);
        assert!((a - 338.865).abs() < 1e-9 && (b - 374.535).abs() < 1e-9);
        let (a, b) = autoscale(7.6757e-7, 7.6757e-7, 0.0);
        assert!(a < -0.99 && b > 0.99);
        let (a, b) = autoscale(10.0, 20.0, 1.0);
        assert_eq!((a, b), (9.2, 20.8));
    }

    #[test]
    fn a_still_resolver_and_a_still_speed_do_not_fill_their_charts() {
        let cat = spy_core::catalogue::Catalogue::builtin();
        let fill = |lo: f64, hi: f64, span: f64| {
            let (a, b) = autoscale(lo, hi, span);
            (hi - lo) / (b - a)
        };
        assert!(fill(101.860, 101.882, min_span_for("deg", cat.get(5138))) < 0.5, "a still resolver fills its chart");
        assert!(fill(-0.147, 0.103, min_span_for("deg/s", cat.get(4001))) < 0.5, "a still joint's speed fills its chart");
        assert_eq!(min_span_for("deg", cat.get(5000)), 0.05);
        assert_eq!(min_span_for("deg", cat.get(5028)), 0.05, "an electrical angle is a motor-side angle");
        assert_eq!(min_span_for("rad", cat.get(5138)), 0.05_f64.to_radians());
        assert_eq!(min_span_for("deg", cat.get(4000)), 0.01);
        assert_eq!(min_span_for("deg", cat.get(6000)), 0.01);
        assert_eq!(min_span_for("deg", None), 0.01, "a signal the catalogue does not know goes by its unit");
    }

    #[test]
    fn a_still_joints_dither_does_not_fill_its_chart() {
        let (lo, hi) = (45.0 - 0.0001, 45.0 + 0.0001);
        let (a, b) = autoscale(lo, hi, min_span("deg"));
        assert!(b - a >= 0.01, "a span of {} deg", b - a);
        assert!((hi - lo) / (b - a) < 0.05, "the dither fills {:.0}% of the chart", 100.0 * (hi - lo) / (b - a));
        assert!(a < lo && b > hi, "centred on the data");
        let (a, b) = autoscale(40.0, 50.0, min_span("deg"));
        assert!((a, b) == (39.2, 50.8));
        let cat = spy_core::catalogue::Catalogue::builtin();
        for s in &cat.signals {
            let u = view::display(Some(s), false).units;
            if !["", "-", "text"].contains(&u.as_str()) {
                assert!(min_span(&u) > 0.0, "no minimum span for \"{u}\" ({})", s.number);
            }
        }
    }
}
