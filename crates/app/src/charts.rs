//! The charts: stacked lanes on one controller-time axis, a rolling window of
//! 1 s to 10 min (10 s by default), pause and scroll back through the 10-minute
//! history, per-lane autoscale with a lock, hover with the wall-clock time, two
//! cursors with the time and value difference, and statistics of the visible window.
//! A gap is drawn as a gap, never interpolated across, and a stale channel's
//! empty stretch is shaded.

use std::sync::{Arc, Mutex, MutexGuard};

use eframe::egui::{self, RichText};
use egui_plot::{HoverPosition, Legend, Line, Plot, PlotPoints, Span, VLine};

use spy_core::catalogue::flag;
use spy_core::session::{Phase, Status};
use spy_core::store::{Channel, Ring};
use spy_core::timeline::Timeline;

use crate::app::SpyApp;
use crate::theme;
use crate::view::{self, Health};

/// Where a chart line's samples come from: a channel's history in the store, or a
/// derived channel's.
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

/// One line on the charts, and one row of the cursor table.
#[derive(Clone)]
pub(crate) struct Member {
    pub(crate) id: String,
    /// The chart's lane and the display unit.
    pub(crate) lane: (u32, String),
    color: egui::Color32,
    factor: f64,
    hold: bool,
    reading: view::Reading,
    /// For the legend.
    name: String,
    /// For a chart's title and the cursor table.
    pub(crate) title: String,
    frozen: bool,
    pub(crate) health: Health,
    /// Nothing yet in the store.
    src: Option<Src>,
    /// The smallest span its chart autoscales to.
    min_span: f64,
}

impl Member {
    /// Its samples over `[from, to)` as the chart draws them: in the display unit, a
    /// zero-filled signal's padding undone (the hold primed from just before).
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

pub const XY_HOVER: &str = "Plot one channel against another over the stretch in view (pause and scroll to pick it)";

pub const WINDOWS: [(f64, &str); 9] = [(1.0, "1 s"), (2.0, "2 s"), (5.0, "5 s"), (10.0, "10 s"), (30.0, "30 s"), (60.0, "1 min"), (120.0, "2 min"), (300.0, "5 min"), (600.0, "10 min")];

/// Statistics of one channel over a time range, in display units.
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

impl SpyApp {
    /// Whether a channel gets a chart: not a text signal (text is not a number), and
    /// not one the controller refused (it has nothing to draw; the table says why).
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

    /// The charts to draw: one per lane and display unit, in the channels' order. A
    /// lane whose channels show different units (one switched to radians, or a hand-
    /// edited settings file) becomes one chart per unit: a shared axis in two units
    /// would mislead. `charted` is decided once per frame, since a channel's record
    /// type can change under it (its first sample) between two looks.
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

    /// Everything charted, channels first, in the order `lanes` gives their charts.
    pub(crate) fn members(&self, charted: &[bool], st: &Status) -> Vec<Member> {
        let connected = view::session_live(&st.phase);
        let store = self.session.store();
        let mut out = Vec::new();
        for (i, c) in self.chans.iter().enumerate() {
            if !charted[i] {
                continue;
            }
            let sig = self.catalogue.get(c.key.signal);
            let d = view::display(sig, c.radians);
            let cs = st.channels.iter().find(|s| s.key == c.key);
            out.push(Member {
                id: c.key.id(),
                lane: (c.lane, d.units.clone()),
                color: c.color,
                factor: d.factor,
                hold: c.hold_nonzero,
                reading: view::reading(sig),
                name: view::short_label(&self.catalogue, &c.key),
                title: view::label(&self.catalogue, &c.key),
                frozen: sig.is_some_and(|s| s.has(flag::FROZEN)),
                health: view::health(cs, connected, sig, st.loopback),
                src: store.get(&c.key).map(Src::Stream),
                min_span: min_span_for(&d.units, sig),
            });
        }
        for (i, d) in self.derived.iter().enumerate() {
            let def = d.live.def();
            let title = self.derived_label(def);
            out.push(Member {
                id: def.id(),
                lane: (d.lane, def.units().to_string()),
                color: d.color,
                factor: 1.0,
                hold: false,
                reading: crate::derived_view::reading(def),
                name: title.clone(),
                title,
                frozen: false,
                health: self.derived_health(i, st),
                src: Some(Src::Derived(d.live.ring())),
                min_span: min_span(def.units()),
            });
        }
        out
    }

    fn units_of(&self, i: usize) -> String {
        let c = &self.chans[i];
        view::display(self.catalogue.get(c.key.signal), c.radians).units
    }

    pub fn charts(&mut self, ui: &mut egui::Ui) {
        self.chart_toolbar(ui);
        let st = self.session.status().clone();
        if self.chans.is_empty() {
            ui.centered_and_justified(|ui| ui.label(RichText::new("Charts appear here once channels are added.").weak()));
            return;
        }
        let tl = st.timeline.clone();
        let Some(newest) = self.session.store().newest() else {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new(if st.phase.is_connected() { "Waiting for the first samples..." } else { "Connect to a controller to see the charts." }).weak())
            });
            return;
        };
        if st.phase != Phase::Streaming && self.paused_at.is_none() {
            ui.colored_label(theme::WARN, "Not streaming: the charts show the last data received.");
        }
        let end_ms = self.paused_at.unwrap_or(newest);
        let x_max = tl.seconds(end_ms);
        let x_min = x_max - self.window_s;
        let live = self.paused_at.is_none();
        let charted: Vec<bool> = (0..self.chans.len()).map(|i| self.charted(i, &st)).collect();
        let lanes = self.lanes(&charted);
        if lanes.is_empty() {
            ui.centered_and_justified(|ui| ui.label(RichText::new("Nothing to chart: the channels are text signals or were refused (see the table).").weak()));
            return;
        }
        let all = self.members(&charted, &st);
        let stats_h = if self.cursors_on { 26.0 * (all.len() as f32 + 2.0) } else { 0.0 };
        let lane_h = ((ui.available_height() - stats_h) / lanes.len() as f32 - 22.0).max(80.0);
        let mut clicked_a: Option<f64> = None;
        let mut clicked_b: Option<f64> = None;
        let mut visible_x = (x_min, x_max);

        // Six pixels either side of a mark's line, in the chart's seconds (the stretch the
        // last frame showed).
        let mark_tol = self.view_ms.map_or(self.window_s, |(a, b)| (b - a) as f64 / 1000.0) * 6.0 / f64::from(ui.available_width().max(100.0));
        let mut transforms = Vec::new();
        egui::ScrollArea::vertical().id_salt("lanes").auto_shrink([false, false]).max_height(ui.available_height() - stats_h).show(ui, |ui| {
            for lane in &lanes {
                let members: Vec<&Member> = all.iter().filter(|m| &m.lane == lane).collect();
                let Some(first) = members.first() else { continue };
                let title = if members.len() == 1 { first.title.clone() } else { format!("{} (+{} overlaid)", first.title, members.len() - 1) };
                let locked = self.lane_lock.get(lane).copied();
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&title).small().strong());
                    ui.label(RichText::new(format!("[{}]", lane.1)).small().weak());
                    if members.iter().any(|m| m.frozen) {
                        theme::badge(ui, "FROZEN", theme::WARN, "Holds the last RAPID path position; does not move while EGM drives the robot.");
                    }
                    let mut lock = locked.is_some();
                    if ui.toggle_value(&mut lock, "🔒").on_hover_text("Lock this chart's vertical scale").changed() {
                        if lock {
                            self.lane_lock.insert(lane.clone(), (f64::NAN, f64::NAN));
                        } else {
                            self.lane_lock.remove(lane);
                        }
                    }
                });

                let markers: Vec<(f64, String)> = self.markers.iter().map(|m| (tl.seconds(m.t_ms), m.label.clone())).collect();
                // The controller's own events (RWS), to the second.
                let events: Vec<(f64, egui::Color32, String)> = self.events_on_timeline(&tl).into_iter().map(|(t, e)| (tl.seconds(t), e.color(), format!("controller: {}", e.text()))).collect();
                let (ca, cb) = (self.cursor_a, self.cursor_b);
                // What each vertical line is, for the hover: the lines have no names, so
                // that they stay out of the legend (a review's events once covered half of
                // every chart there).
                let mut marks: Vec<(f64, String)> = markers.iter().map(|(x, l)| (*x, format!("marker {l}"))).collect();
                marks.extend(events.iter().map(|(x, _, l)| (*x, l.clone())));
                if self.cursors_on {
                    marks.extend(ca.map(|a| (a, "cursor A".to_string())));
                    marks.extend(cb.map(|b| (b, "cursor B".to_string())));
                }

                let tl2 = tl.clone();
                let units = lane.1.clone();
                let shown = self.hover_text.clone();
                // The widest its channels need (a motor's angle beside a joint's).
                let min_span = members.iter().map(|m| m.min_span).fold(0.0, f64::max);
                let mut plot = Plot::new(("lane", lane.0, &lane.1))
                    .height(lane_h)
                    .link_axis("x-link", [true, false])
                    .link_cursor("x-link", [true, false])
                    .legend(Legend::default().position(egui_plot::Corner::LeftTop))
                    .y_axis_min_width(56.0)
                    .show_axes([true, true])
                    .label_formatter(move |pos| remember(&shown, hover_label(pos, &tl2, &units, &marks, mark_tol)));
                plot = if live {
                    plot.allow_drag(false).allow_zoom(false).allow_scroll(false).allow_boxed_zoom(false).allow_double_click_reset(false)
                } else {
                    plot.allow_drag([true, false]).allow_zoom([true, false]).allow_scroll([true, false]).allow_boxed_zoom(false).allow_double_click_reset(false)
                };

                let pause_fresh = self.pause_fresh;
                let cursors_on = self.cursors_on;
                let origin = tl.origin().unwrap_or(0);

                let resp = plot.show(ui, |pu| {
                    let b = pu.plot_bounds();
                    let (vx0, vx1) = if live || pause_fresh || !b.is_valid_x() { (x_min, x_max) } else { (b.min()[0], b.max()[0]) };
                    let from = origin + (vx0 * 1000.0).floor() as i64;
                    let to = origin + (vx1 * 1000.0).ceil() as i64 + 1;
                    let px = pu.response().rect.width().max(50.0) as usize;
                    let (mut ymin, mut ymax) = (f64::INFINITY, f64::NEG_INFINITY);
                    for m in &members {
                        let Some(src) = &m.src else { continue };
                        let (factor, hold) = (m.factor, m.hold);
                        let ring = src.lock();
                        // A zero-filled signal's padding, undone for at most the hold
                        // time: a longer run of zeros is a stop and is drawn as zero.
                        // Primed from just before the window, so its left edge is right.
                        let mut zh = view::ZeroHold::new();
                        if hold {
                            for (t, v) in ring.range(from - view::ZERO_HOLD_MS.ceil() as i64 - 1, from) {
                                zh.apply_at(t, v);
                            }
                        }
                        let segs = ring.decimate_at(from, to, px, |t, v| (if hold { zh.apply_at(t, v) } else { v }) * factor);
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
                                // A lone point would be invisible as a line.
                                pts.push([pts[0][0] + 0.0005, pts[0][1]]);
                            }
                            pu.line(Line::new(m.name.clone(), PlotPoints::from(pts)).color(m.color).width(1.4).id(egui::Id::new((&m.id, si))));
                        }
                        // A stale channel: shade from its last sample to the right edge.
                        if matches!(m.health, Health::Stale | Health::NotConnected)
                            && let Some(t) = last
                        {
                            let x = (t - origin) as f64 / 1000.0;
                            if x < vx1 {
                                pu.span(Span::new(format!("{} stale", m.name), x..=vx1).fill(theme::WARN.gamma_multiply(0.08)).border_width(0.0));
                            }
                        }
                    }
                    for (x, _) in &markers {
                        if *x >= vx0 && *x <= vx1 {
                            pu.vline(VLine::new("", *x).color(theme::WARN).width(1.0));
                        }
                    }
                    for (x, color, _) in &events {
                        if *x >= vx0 && *x <= vx1 {
                            pu.vline(VLine::new("", *x).color(*color).width(1.0).style(egui_plot::LineStyle::dashed_loose()));
                        }
                    }
                    if cursors_on {
                        if let Some(a) = ca {
                            pu.vline(VLine::new("", a).color(egui::Color32::from_rgb(0x5A, 0x9B, 0xD5)).width(1.5));
                        }
                        if let Some(bx) = cb {
                            pu.vline(VLine::new("", bx).color(egui::Color32::from_rgb(0xD3, 0x72, 0x95)).width(1.5));
                        }
                    }
                    if live || pause_fresh {
                        pu.set_plot_bounds_x(x_min..=x_max);
                    }
                    let (y0, y1) = match locked {
                        Some((a, bb)) if a.is_finite() && bb.is_finite() => (a, bb),
                        _ if ymin.is_finite() => autoscale(ymin, ymax, min_span),
                        _ => (-1.0, 1.0),
                    };
                    pu.set_plot_bounds_y(y0..=y1);
                    let clicked = pu.response().clicked();
                    let secondary = pu.response().secondary_clicked();
                    let x_at = pu.pointer_coordinate().map(|p| p.x);
                    ((vx0, vx1), (y0, y1), clicked, secondary, x_at)
                });
                transforms.push(resp.transform);
                let ((vx0, vx1), (y0, y1), clicked, secondary, x_at) = resp.inner;
                visible_x = (vx0, vx1);
                // A lock taken this frame captures the scale in view.
                if let Some(l) = self.lane_lock.get_mut(lane)
                    && !l.0.is_finite() {
                        *l = (y0, y1);
                    }
                if self.cursors_on {
                    if clicked {
                        clicked_a = x_at;
                    }
                    if secondary {
                        clicked_b = x_at;
                    }
                }
            }
        });
        self.lane_transforms = transforms;
        self.pause_fresh = false;
        let origin = tl.origin().unwrap_or(0);
        self.view_ms = Some((origin + (visible_x.0 * 1000.0).floor() as i64, origin + (visible_x.1 * 1000.0).ceil() as i64 + 1));
        if let Some(a) = clicked_a {
            self.cursor_a = Some(a);
        }
        if let Some(b) = clicked_b {
            self.cursor_b = Some(b);
        }
        if self.cursors_on {
            self.cursor_table(ui, &tl, visible_x, &all);
        }
    }

    fn chart_toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label("Window");
            let cur = WINDOWS.iter().find(|(s, _)| (s - self.window_s).abs() < 1e-9).map(|(_, l)| *l).unwrap_or("custom");
            egui::ComboBox::from_id_salt("window").selected_text(cur).width(70.0).show_ui(ui, |ui| {
                for (s, l) in WINDOWS {
                    if ui.selectable_label((s - self.window_s).abs() < 1e-9, l).clicked() {
                        self.window_s = s;
                        self.mark_settings_dirty();
                        // Paused, the charts keep the person's view; a length chosen now is
                        // shown at once, ending where the view ends (the charts did not
                        // change on the cell, and the person looked for the change).
                        if self.paused_at.is_some() {
                            if let Some((_, end)) = self.view_ms {
                                self.paused_at = Some(end - 1);
                            }
                            self.pause_fresh = true;
                        }
                    }
                }
            });
            let paused = self.paused_at.is_some();
            if ui.button(if paused { "▶ Live" } else { "⏸ Pause" }).on_hover_text("Space. While paused, drag to scroll back and use the wheel to zoom, through the last 10 minutes.").clicked() {
                self.toggle_pause();
            }
            if paused {
                ui.label(RichText::new("PAUSED: drag to scroll, wheel to zoom").color(theme::WARN));
            }
            ui.separator();
            ui.toggle_value(&mut self.cursors_on, "Cursors").on_hover_text("Click a chart to place cursor A, right-click for cursor B. Shows the time and value differences, and statistics.");
            if self.cursors_on && ui.small_button("clear").clicked() {
                self.cursor_a = None;
                self.cursor_b = None;
            }
            ui.separator();
            ui.add(egui::TextEdit::singleline(&mut self.marker_text).hint_text("marker label").desired_width(110.0));
            if ui.button("Marker").on_hover_text("M: mark this moment on the charts and in any running recording").clicked() {
                self.add_marker();
            }
            if !self.markers.is_empty() && ui.small_button("clear markers").clicked() {
                self.markers.clear();
            }
            ui.separator();
            if ui.button("Save CSV").on_hover_text("Save every channel's samples in view to a CSV file in the recordings folder").clicked() {
                self.export_live_csv();
            }
            if ui.button("Save PNG").on_hover_text("Save a picture of the charts to the recordings folder").clicked() {
                self.request_png(crate::export::Picture::Charts);
            }
            ui.separator();
            if ui.selectable_label(self.xy.is_some(), "XY").on_hover_text(XY_HOVER).clicked() {
                self.toggle_xy();
            }
        });
    }

    fn cursor_table(&mut self, ui: &mut egui::Ui, tl: &Timeline, visible: (f64, f64), members: &[Member]) {
        ui.separator();
        let origin = tl.origin().unwrap_or(0);
        let (a, b) = (self.cursor_a, self.cursor_b);
        let range = match (a, b) {
            (Some(a), Some(b)) => (a.min(b), a.max(b)),
            _ => visible,
        };
        ui.horizontal(|ui| {
            match (a, b) {
                (Some(a), Some(b)) => ui.label(RichText::new(format!("A {:.3} s   B {:.3} s   Δt {:.3} s  (statistics between A and B)", a, b, b - a)).strong()),
                (Some(a), None) => ui.label(format!("A {a:.3} s   right-click to place B   (statistics of the visible window)")),
                _ => ui.label("Click a chart to place cursor A, right-click for B   (statistics of the visible window)"),
            };
        });
        let at = |m: &Member, x: f64| -> Option<f64> {
            let r = m.src.as_ref()?.lock();
            view::value_at(&r, m.reading, origin + (x * 1000.0).round() as i64)
        };
        egui::Grid::new("cursor-stats").striped(true).num_columns(8).show(ui, |ui| {
            for h in ["channel", "at A", "at B", "B − A", "mean", "min", "max", "std dev"] {
                ui.label(RichText::new(h).small().strong());
            }
            ui.end_row();
            for m in members {
                let va = a.and_then(|x| at(m, x)).map(|v| v * m.factor);
                let vb = b.and_then(|x| at(m, x)).map(|v| v * m.factor);
                let s = match &m.src {
                    Some(src) => {
                        let r = src.lock();
                        let from = origin + (range.0 * 1000.0).floor() as i64;
                        let to = origin + (range.1 * 1000.0).ceil() as i64 + 1;
                        let s = view::window_stats(&r, m.reading, from, to);
                        RangeStats { n: s.n, mean: s.mean * m.factor, min: s.min * m.factor, max: s.max * m.factor, sd: s.sd * m.factor }
                    }
                    None => RangeStats::default(),
                };
                let f = |v: Option<f64>| v.map(view::fmt).unwrap_or_else(|| "--".into());
                ui.label(RichText::new(&m.title).small().color(m.color));
                ui.label(RichText::new(f(va)).small().monospace());
                ui.label(RichText::new(f(vb)).small().monospace());
                ui.label(RichText::new(f(va.zip(vb).map(|(x, y)| y - x))).small().monospace());
                let has = s.n > 0;
                ui.label(RichText::new(if has { view::fmt(s.mean) } else { "--".into() }).small().monospace());
                ui.label(RichText::new(if has { view::fmt(s.min) } else { "--".into() }).small().monospace());
                ui.label(RichText::new(if has { view::fmt(s.max) } else { "--".into() }).small().monospace());
                ui.label(RichText::new(if has { format!("{} {}", view::fmt(s.sd), m.lane.1) } else { "--".into() }).small().monospace());
                ui.end_row();
            }
        });
    }
}

/// The vertical range for data spanning `lo..hi`: an 8% margin; for a constant (or
/// all but constant) signal a band around it wide enough to read, rather than a range
/// of 1e-9 whose axis labels are all zeros; and never tighter than `min_span`, so a
/// still joint's dither of a ten-thousandth of a degree does not fill the chart and
/// look like violent motion (seen on the cell; decided 2026-09-26).
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

/// The smallest vertical span a chart autoscales to, per display unit: small enough
/// for any real motion or change to fill most of the chart, large enough that the
/// noise of a signal at rest shows as the thin line it is. Unknown units: none. A
/// still joint's speed dithers by about 0.25 deg/s (the cell, 2026-09-29): 0.5 deg/s
/// (it was 0.1).
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

/// The smallest span for one channel's chart: its unit's, and wider for a motor-side
/// angle (a resolver's, or the drive's electrical angle), which a still motor dithers
/// by about 0.02 deg where a still arm joint dithers by 0.0001 deg (the cell,
/// 2026-09-29). Arm angles keep 0.01 deg (decided 2026-09-29). A signal the
/// catalogue does not know goes by its unit.
pub fn min_span_for(units: &str, sig: Option<&spy_core::catalogue::Signal>) -> f64 {
    let motor_angle = sig.is_some_and(|s| s.category == "motor position" || (s.category == "drive / inverter" && matches!(s.units.as_str(), "rad" | "deg")));
    match (units.trim(), motor_angle) {
        ("deg", true) => 0.05,
        ("rad", true) => 0.05_f64.to_radians(),
        (u, _) => min_span(u),
    }
}

fn hover_label(pos: &HoverPosition<'_>, tl: &Timeline, units: &str, marks: &[(f64, String)], tol: f64) -> Option<String> {
    let (name, p) = match pos {
        HoverPosition::NearDataPoint { plot_name, position, .. } => (Some(*plot_name), *position),
        HoverPosition::Elsewhere { position } => (None, *position),
    };
    let t = tl.origin().unwrap_or(0) + (p.x * 1000.0).round() as i64;
    let wall = tl.wall(t).map(view::local_time).unwrap_or_default();
    let text = match name {
        Some(n) => format!("{n}\n{} {units}\nt = {:.3} s   {wall}", view::fmt(p.y), p.x),
        None => format!("t = {:.3} s   {wall}\n{} {units}", p.x, view::fmt(p.y)),
    };
    Some(with_mark(text, p.x, tol, marks))
}

/// Keep the hover's text where a test can read it, and pass it on.
pub fn remember(shown: &std::sync::Mutex<String>, text: Option<String>) -> Option<String> {
    if let (Some(t), Ok(mut s)) = (&text, shown.lock()) {
        s.clone_from(t);
    }
    text
}

/// A hover's text, headed by what the vertical lines under the pointer are (markers,
/// controller events, cursors: `marks`, x and what it is), every one within `tol` of
/// `x` (a controller often logs several events in one second). The lines carry no
/// names, so that they stay out of the legend, and egui_plot hovers no vertical line:
/// this is where their text is shown.
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
        let at = |x: f64| hover_label(&HoverPosition::Elsewhere { position: egui_plot::PlotPoint::new(x, 1.0) }, &tl, "Nm", &marks, 0.1).unwrap();
        assert!(at(4.05).starts_with("controller: 10010 Motors OFF state (information)\nt = 4.050 s"), "{}", at(4.05));
        assert!(at(4.42).starts_with("cursor A\n"), "the nearest: {}", at(4.42));
        assert!(at(0.95).starts_with("marker M1\n"), "{}", at(0.95));
        assert!(at(2.0).starts_with("t = 2.000 s"), "no mark within reach: {}", at(2.0));
        // Two events in the same second: both.
        let two = vec![(4.0, "controller: 10002 Program pointer has been reset (information)".to_string()), (4.0, "controller: 10011 Motors ON state (information)".to_string())];
        let text = hover_label(&HoverPosition::Elsewhere { position: egui_plot::PlotPoint::new(4.01, 1.0) }, &tl, "Nm", &two, 0.1).unwrap();
        assert!(text.starts_with("controller: 10002") && text.contains("\ncontroller: 10011 Motors ON state (information)\nt = "), "{text}");
        // Over a channel's sample beside a mark: both named, the mark first.
        let near = HoverPosition::NearDataPoint { plot_name: "4002 · Torque", position: egui_plot::PlotPoint::new(3.95, 1.0), index: 0 };
        let text = hover_label(&near, &tl, "Nm", &marks, 0.1).unwrap();
        assert!(text.starts_with("controller: 10010") && text.contains("\n4002 · Torque\n"), "{text}");
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
        // The cell (2026-09-29): a still resolver (5138) dithered over 0.022 deg and a
        // still J1 speed (4001) over about 0.25 deg/s, and both filled their charts like
        // violent motion. Wider spans for them (decided 2026-09-29).
        let cat = spy_core::catalogue::Catalogue::builtin();
        let fill = |lo: f64, hi: f64, span: f64| {
            let (a, b) = autoscale(lo, hi, span);
            (hi - lo) / (b - a)
        };
        assert!(fill(101.860, 101.882, min_span_for("deg", cat.get(5138))) < 0.5, "a still resolver fills its chart");
        assert!(fill(-0.147, 0.103, min_span_for("deg/s", cat.get(4001))) < 0.5, "a still joint's speed fills its chart");
        // Motor-side angles by their category, electrical ones included, in either unit;
        // arm angles keep the finer span (a joint's real 0.02 deg move fills its chart).
        assert_eq!(min_span_for("deg", cat.get(5000)), 0.05);
        assert_eq!(min_span_for("deg", cat.get(5028)), 0.05, "an electrical angle is a motor-side angle");
        assert_eq!(min_span_for("rad", cat.get(5138)), 0.05_f64.to_radians());
        assert_eq!(min_span_for("deg", cat.get(4000)), 0.01);
        assert_eq!(min_span_for("deg", cat.get(6000)), 0.01);
        assert_eq!(min_span_for("deg", None), 0.01, "a signal the catalogue does not know goes by its unit");
    }

    #[test]
    fn a_still_joints_dither_does_not_fill_its_chart() {
        // On the cell a joint at rest dithered by about a ten-thousandth of a degree,
        // and the chart scaled that to its full height.
        let (lo, hi) = (45.0 - 0.0001, 45.0 + 0.0001);
        let (a, b) = autoscale(lo, hi, min_span("deg"));
        assert!(b - a >= 0.01, "a span of {} deg", b - a);
        assert!((hi - lo) / (b - a) < 0.05, "the dither fills {:.0}% of the chart", 100.0 * (hi - lo) / (b - a));
        assert!(a < lo && b > hi, "centred on the data");
        // Real motion still fills it.
        let (a, b) = autoscale(40.0, 50.0, min_span("deg"));
        assert!((a, b) == (39.2, 50.8));
        // Every unit the built-in catalogue shows gets a span, bar the unitless.
        let cat = spy_core::catalogue::Catalogue::builtin();
        for s in &cat.signals {
            let u = view::display(Some(s), false).units;
            if !["", "-", "text"].contains(&u.as_str()) {
                assert!(min_span(&u) > 0.0, "no minimum span for \"{u}\" ({})", s.number);
            }
        }
    }
}
