//! Reviewing a recording (Phase 2): open one from the Recordings window, by its path,
//! or by dropping its folder on the window; chart it on its own clock (the wall
//! clock, exact across controller restarts) with its markers and connection events,
//! cursors and the statistics of the stretch in view. The live session carries on
//! underneath. Nothing here is ever presented as live: a banner says REVIEWING, and
//! the values shown are statistics of what is in view, not readouts.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, UNIX_EPOCH};

use eframe::egui::{self, RichText};
use egui_plot::{HoverPosition, Legend, Line, Plot, PlotPoints, VLine};

use spy_core::catalogue::{self, flag};
use spy_core::log::Level;
use spy_core::recording::{Kind, Meta};
use spy_core::review::{Review, ReviewChannel};

use crate::app::SpyApp;
use crate::charts::{autoscale, min_span_for, RangeStats};
use crate::theme;
use crate::view;

/// A recording open for review.
pub struct ReviewState {
    pub review: Arc<Review>,
    /// The stretch in view, seconds from the recording's first sample.
    pub view: (f64, f64),
    /// The next frame sets the charts to `view` (on opening, and after "Show all").
    pub fresh: bool,
    pub cursors_on: bool,
    pub cursor_a: Option<f64>,
    pub cursor_b: Option<f64>,
    /// Times the channels' statistics were computed (each is a pass over every sample
    /// in the stretch).
    pub stats_computed: u64,
    /// The last statistics computed and their stretch: the panel's (0), the cursors' (1).
    stats_cache: [Option<StatsFor>; 2],
}

/// Each channel's statistics, and the stretch `[from, to)` they are of.
type StatsFor = ((i64, i64), Vec<RangeStats>);

impl ReviewState {
    fn new(review: Review) -> ReviewState {
        let dur = ((review.end - review.start) as f64 / 1000.0).max(0.001);
        ReviewState { review: Arc::new(review), view: (0.0, dur), fresh: true, cursors_on: false, cursor_a: None, cursor_b: None, stats_computed: 0, stats_cache: [None, None] }
    }

    fn duration(&self) -> f64 {
        ((self.review.end - self.review.start) as f64 / 1000.0).max(0.001)
    }

    /// Seconds on the chart to the recording's time axis (ms); saturating, however far
    /// the charts are zoomed or dragged.
    fn t_of(&self, x: f64) -> i64 {
        self.review.start.saturating_add((x * 1000.0).round() as i64)
    }

    /// The stretch in view on the recording's time axis, `[from, to)`.
    pub(crate) fn stretch(&self) -> (i64, i64) {
        (self.t_of(self.view.0), self.t_of(self.view.1).saturating_add(1))
    }
}

pub type ReviewJob = (PathBuf, JoinHandle<Result<Review, String>>);

/// The folder a dropped or typed path means: the recording folder itself, or one of
/// its files.
pub fn recording_dir(p: &Path) -> Option<PathBuf> {
    let dir = if p.is_dir() { p.to_path_buf() } else { p.parent()?.to_path_buf() };
    dir.join("recording.json").is_file().then_some(dir)
}

/// A recording's start in local time, for lists and the banner.
fn local_start(meta: &Meta) -> String {
    match spy_core::util::parse_iso(&meta.started_utc) {
        Some(t) => spy_core::util::local_parts(t).map(|(y, mo, d, h, mi, s, _)| format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")).unwrap_or_else(|| meta.started_utc.clone()),
        None => meta.started_utc.clone(),
    }
}

fn kind_word(k: Kind) -> &'static str {
    match k {
        Kind::Full => "every sample",
        Kind::Slow => "slow log",
        Kind::Snapshot => "saved last seconds",
    }
}

fn clock_text(secs: f64) -> String {
    let s = secs.max(0.0);
    if s < 120.0 { format!("{s:.1} s") } else if s < 7200.0 { format!("{:.1} min", s / 60.0) } else { format!("{:.1} h", s / 3600.0) }
}

impl SpyApp {
    /// Open a recording folder for review, in the background.
    pub fn open_recording(&mut self, dir: PathBuf) {
        if self.review_job.is_some() {
            self.toast(Level::Warn, "A recording is already being opened.");
            return;
        }
        // The catalogue's zero-filled signals are read with their padding undone, as
        // live (their recording keeps every sample as sent).
        let hold: Vec<u32> = self.catalogue.signals.iter().filter(|s| s.has(flag::ZERO_FILLED)).map(|s| s.number).collect();
        let ctx = self.ctx.clone();
        let d = dir.clone();
        let job = std::thread::spawn(move || {
            let r = spy_core::review::open(&d, &hold);
            ctx.request_repaint();
            r
        });
        self.log.info(format!("Opening the recording {} ...", dir.display()));
        self.review_job = Some((dir, job));
    }

    pub fn poll_review(&mut self) {
        if !self.review_job.as_ref().is_some_and(|(_, j)| j.is_finished()) {
            return;
        }
        let Some((dir, job)) = self.review_job.take() else { return };
        match job.join() {
            Ok(Ok(r)) => {
                let what = format!("{} channel(s), {} of recording", r.channels.len(), clock_text((r.end - r.start) as f64 / 1000.0));
                for n in &r.notes {
                    self.toast(Level::Warn, format!("{}: {n}", dir.display()));
                }
                self.log.info(format!("Reviewing {}: {what}.", dir.display()));
                self.review = Some(ReviewState::new(r));
                self.show_recordings = false;
            }
            Ok(Err(e)) => self.toast(Level::Error, format!("Could not open {}: {e}", dir.display())),
            Err(_) => self.toast(Level::Error, format!("Opening {} failed unexpectedly.", dir.display())),
        }
    }

    /// A folder (or a file in one) dropped on the window.
    pub fn take_dropped(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        if let Some(p) = dropped.first() {
            match recording_dir(p) {
                Some(d) => self.open_recording(d),
                None => self.toast(Level::Warn, format!("{} is not a recording folder (it has no recording.json).", p.display())),
            }
        }
    }

    /// The Recordings window: the recordings folder's recordings, newest first, and a
    /// path for one kept elsewhere.
    pub fn recordings_window(&mut self, ctx: &egui::Context) {
        if !self.show_recordings {
            return;
        }
        if self.recordings_list.is_none() {
            self.recordings_list = Some(spy_core::recording::list(&self.record_dir()));
        }
        let mut open = true;
        let mut chosen: Option<PathBuf> = None;
        let mut refresh = false;
        egui::Window::new("Recordings").open(&mut open).default_width(640.0).default_height(420.0).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(self.record_dir().display().to_string()).weak());
                if ui.small_button("Refresh").clicked() {
                    refresh = true;
                }
            });
            ui.label(RichText::new("Or drop a recording's folder anywhere on the window.").small().weak());
            ui.separator();
            let list = self.recordings_list.clone().unwrap_or_default();
            if list.is_empty() {
                ui.label("No recordings here yet.");
            }
            egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                egui::Grid::new("recordings").striped(true).num_columns(6).show(ui, |ui| {
                    for h in ["started", "name", "kind", "controller", "", ""] {
                        ui.label(RichText::new(h).small().strong());
                    }
                    ui.end_row();
                    for (dir, meta) in &list {
                        ui.label(RichText::new(local_start(meta)).monospace());
                        ui.label(if meta.label.is_empty() { dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default() } else { meta.label.clone() });
                        ui.label(kind_word(meta.kind));
                        ui.label(&meta.controller);
                        if !meta.complete {
                            ui.label(RichText::new("cut short").color(theme::WARN)).on_hover_text("Not closed properly (the program or the PC stopped while recording): the data up to then is there.");
                        } else {
                            ui.label("");
                        }
                        if ui.button("Open").clicked() {
                            chosen = Some(dir.clone());
                        }
                        ui.end_row();
                    }
                });
            });
            ui.separator();
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.recording_path_input).hint_text("C:\\path\\to\\a recording folder").desired_width(420.0));
                if ui.button("Open").clicked() {
                    let p = PathBuf::from(self.recording_path_input.trim().trim_matches('"'));
                    match recording_dir(&p) {
                        Some(d) => chosen = Some(d),
                        None => self.toast(Level::Warn, format!("{} is not a recording folder (it has no recording.json).", p.display())),
                    }
                }
            });
            if self.review_job.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Opening...");
                });
            }
        });
        if refresh {
            self.recordings_list = None;
        }
        if let Some(d) = chosen {
            self.open_recording(d);
        }
        if !open {
            self.show_recordings = false;
            self.recordings_list = None;
        }
    }

    /// The line under the controller bar while reviewing: what is shown, that it is
    /// not live, and the way back.
    pub fn review_banner(&mut self, ui: &mut egui::Ui) {
        let Some(rs) = &self.review else { return };
        let m = &rs.review.meta;
        let name = rs.review.dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let mut close = false;
        egui::Frame::new().fill(theme::REVIEW_BG).inner_margin(egui::Margin::symmetric(6, 3)).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("REVIEWING").strong().color(theme::REVIEW));
                ui.label(RichText::new(&name).strong());
                ui.label(format!("recorded {} on {}, {}, {}. Not live.", local_start(m), if m.controller.is_empty() { "?" } else { &m.controller }, kind_word(m.kind), clock_text(rs.duration())));
                if !m.complete {
                    ui.label(RichText::new("Cut short.").color(theme::WARN));
                }
                if m.samples_lost > 0 {
                    ui.label(RichText::new(format!("{} samples lost while recording.", m.samples_lost)).color(theme::BAD));
                }
                if ui.button("Close the recording").clicked() {
                    close = true;
                }
            });
        });
        if close {
            self.review = None;
        }
    }

    /// Charts of the recording: each signal (with its axes overlaid) in a chart of its
    /// own unit, on the recording's clock, free to drag and zoom.
    pub fn review_charts(&mut self, ui: &mut egui::Ui) {
        let (mut save_csv, mut save_png) = (false, false);
        let (xy_open, mut toggle_xy) = (self.xy.is_some(), false);
        let Some(rs) = &mut self.review else { return };
        let dur = rs.duration();
        ui.horizontal_wrapped(|ui| {
            if ui.button("Show all").clicked() {
                rs.view = (0.0, dur);
                rs.fresh = true;
            }
            ui.toggle_value(&mut rs.cursors_on, "Cursors").on_hover_text("Click a chart to place cursor A, right-click for cursor B.");
            if rs.cursors_on && ui.small_button("clear").clicked() {
                rs.cursor_a = None;
                rs.cursor_b = None;
            }
            ui.label(RichText::new("Drag to scroll, wheel to zoom.").weak());
            ui.separator();
            if ui.button("Save CSV").on_hover_text("Save the samples in view to a CSV file in the recordings folder").clicked() {
                save_csv = true;
            }
            if ui.button("Save PNG").on_hover_text("Save a picture of the charts to the recordings folder").clicked() {
                save_png = true;
            }
            ui.separator();
            toggle_xy = ui.selectable_label(xy_open, "XY").on_hover_text(crate::charts::XY_HOVER).clicked();
        });
        if toggle_xy {
            self.toggle_xy();
        }
        if save_csv || save_png {
            if save_csv {
                self.export_review_csv();
            }
            if save_png {
                self.request_png(crate::export::Picture::Charts);
            }
            return;
        }
        let hover_text = self.hover_text.clone();
        let cat = &self.catalogue;
        let Some(rs) = &mut self.review else { return };
        let review = rs.review.clone();
        // Lanes: one per signal and unit, in the recording's order.
        let mut lanes: Vec<((u32, String), Vec<usize>)> = Vec::new();
        for (i, ch) in review.channels.iter().enumerate() {
            if ch.v.is_empty() {
                continue;
            }
            // A derived channel shares a chart only with its own kind (two DC links'
            // sags), never with a recorded channel that happens to have its units.
            let group = match (&ch.derived, &ch.key) {
                (Some(spy_core::derived::Derived::Turn { .. }), _) => u32::MAX,
                (Some(spy_core::derived::Derived::DutySum { .. }), _) => u32::MAX - 1,
                (Some(spy_core::derived::Derived::Sag { .. }), _) => u32::MAX - 2,
                (None, Some(k)) => k.signal,
                (None, None) => 0,
            };
            let lane = (group, display(ch).0);
            match lanes.iter_mut().find(|(l, _)| *l == lane) {
                Some((_, m)) => m.push(i),
                None => lanes.push((lane, vec![i])),
            }
        }
        if lanes.is_empty() {
            ui.centered_and_justified(|ui| ui.label(RichText::new("Nothing to chart: the recording holds only text signals, or no samples.").weak()));
            return;
        }
        let stats_h = if rs.cursors_on { 26.0 * (review.channels.len() as f32 + 2.0) } else { 0.0 };
        let lane_h = ((ui.available_height() - stats_h) / lanes.len() as f32 - 22.0).max(80.0);
        let (mut view_out, fresh) = (rs.view, rs.fresh);
        let (ca, cb, cursors_on) = (rs.cursor_a, rs.cursor_b, rs.cursors_on);
        let (mut clicked_a, mut clicked_b) = (None, None);
        let start = review.start;
        let wall = review.wall_clock;
        // What each vertical line is, for the hover: the lines have no names, so that
        // they stay out of the legend (on the cell, a review's events covered half of
        // every chart there).
        let mut marks: Vec<(f64, String)> = review.marks.iter().map(|m| ((m.t - start) as f64 / 1000.0, format!("{}: {}", m.kind, m.text))).collect();
        if cursors_on {
            marks.extend(ca.map(|a| (a, "cursor A".to_string())));
            marks.extend(cb.map(|b| (b, "cursor B".to_string())));
        }
        // Six pixels either side of a mark's line, in the chart's seconds.
        let mark_tol = (rs.view.1 - rs.view.0).abs() * 6.0 / f64::from(ui.available_width().max(100.0));
        let mut transforms = Vec::new();
        egui::ScrollArea::vertical().id_salt("review-lanes").auto_shrink([false, false]).max_height(ui.available_height() - stats_h).show(ui, |ui| {
            for ((signal, units), members) in &lanes {
                let first = &review.channels[members[0]];
                let title = if members.len() == 1 { name(cat, first) } else { format!("{} (+{} overlaid)", name(cat, first), members.len() - 1) };
                ui.label(RichText::new(format!("{title}  [{units}]")).small().strong());
                let u2 = units.clone();
                let marks2 = marks.clone();
                let shown = hover_text.clone();
                let plot = Plot::new(("review", *signal, units.as_str()))
                    .height(lane_h)
                    .link_axis("review-x", [true, false])
                    .link_cursor("review-x", [true, false])
                    .legend(Legend::default().position(egui_plot::Corner::LeftTop))
                    .y_axis_min_width(56.0)
                    .allow_drag([true, false])
                    .allow_zoom([true, false])
                    .allow_scroll([true, false])
                    .allow_boxed_zoom(false)
                    .allow_double_click_reset(false)
                    .label_formatter(move |pos| crate::charts::remember(&shown, hover(pos, start, wall, &u2, &marks2, mark_tol)));
                // The widest its channels need (a motor's angle beside a joint's).
                let min_span = members.iter().map(|&i| min_span_for(units, review.channels[i].key.as_ref().and_then(|k| cat.get(k.signal)))).fold(0.0, f64::max);
                let resp = plot.show(ui, |pu| {
                    let b = pu.plot_bounds();
                    let (vx0, vx1) = if fresh || !b.is_valid_x() { view_out } else { (b.min()[0], b.max()[0]) };
                    let from = start + (vx0 * 1000.0).floor() as i64;
                    let to = start + (vx1 * 1000.0).ceil() as i64 + 1;
                    let px = pu.response().rect.width().max(50.0) as usize;
                    let (mut ymin, mut ymax) = (f64::INFINITY, f64::NEG_INFINITY);
                    for (n, &i) in members.iter().enumerate() {
                        let ch = &review.channels[i];
                        let (_, factor) = display(ch);
                        let color = theme::PALETTE[i % theme::PALETTE.len()];
                        for (si, seg) in ch.decimate(from, to, px, factor).iter().enumerate() {
                            let mut pts: Vec<[f64; 2]> = Vec::with_capacity(seg.len() * 2);
                            for c in seg {
                                let x = (c.t - start) as f64 / 1000.0;
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
                            pu.line(Line::new(short(cat, ch), PlotPoints::from(pts)).color(color).width(1.4).id(egui::Id::new(("review-line", i, si, n))));
                        }
                    }
                    for m in &review.marks {
                        let x = (m.t - start) as f64 / 1000.0;
                        if x >= vx0 && x <= vx1 {
                            let (c, w) = if m.kind == "marker" { (theme::WARN, 1.0) } else { (theme::IDLE, 1.0) };
                            pu.vline(VLine::new("", x).color(c).width(w));
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
                    if fresh {
                        pu.set_plot_bounds_x(view_out.0..=view_out.1);
                    }
                    let (y0, y1) = if ymin.is_finite() { autoscale(ymin, ymax, min_span) } else { (-1.0, 1.0) };
                    pu.set_plot_bounds_y(y0..=y1);
                    ((vx0, vx1), pu.response().clicked(), pu.response().secondary_clicked(), pu.pointer_coordinate().map(|p| p.x))
                });
                transforms.push(resp.transform);
                let ((vx0, vx1), clicked, secondary, x_at) = resp.inner;
                view_out = (vx0, vx1);
                if cursors_on {
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
        if let Some(rs) = &mut self.review {
            rs.view = view_out;
            rs.fresh = false;
            if clicked_a.is_some() {
                rs.cursor_a = clicked_a;
            }
            if clicked_b.is_some() {
                rs.cursor_b = clicked_b;
            }
        }
        if cursors_on {
            self.review_cursor_table(ui);
        }
    }

    /// Each channel's statistics over `[from, to)`, in its display unit, computed once
    /// for a stretch and kept (slot 0 the panel's stretch in view, 1 the cursors'): a
    /// long recording's stretch holds tens of millions of samples, and the window
    /// repaints many times a second.
    fn review_stats_cached(&mut self, slot: usize, from: i64, to: i64) -> Vec<RangeStats> {
        let cat = &self.catalogue;
        let Some(rs) = &mut self.review else { return Vec::new() };
        if let Some((k, v)) = &rs.stats_cache[slot]
            && *k == (from, to)
        {
            return v.clone();
        }
        let v: Vec<RangeStats> = rs.review.channels.iter().map(|ch| review_stats(cat, ch, from, to)).collect();
        rs.stats_computed += 1;
        rs.stats_cache[slot] = Some(((from, to), v.clone()));
        v
    }

    fn review_cursor_table(&mut self, ui: &mut egui::Ui) {
        let Some(rs) = &self.review else { return };
        let (a, b) = (rs.cursor_a, rs.cursor_b);
        let range = match (a, b) {
            (Some(a), Some(b)) => (a.min(b), a.max(b)),
            _ => rs.view,
        };
        let (from, to) = (rs.t_of(range.0), rs.t_of(range.1).saturating_add(1));
        let stats = self.review_stats_cached(1, from, to);
        let Some(rs) = &self.review else { return };
        ui.separator();
        match (a, b) {
            (Some(a), Some(b)) => ui.label(RichText::new(format!("A {a:.3} s   B {b:.3} s   Δt {:.3} s  (statistics between A and B)", b - a)).strong()),
            (Some(a), None) => ui.label(format!("A {a:.3} s   right-click to place B   (statistics of the stretch in view)")),
            _ => ui.label("Click a chart to place cursor A, right-click for B   (statistics of the stretch in view)"),
        };
        egui::Grid::new("review-cursor-stats").striped(true).num_columns(5).show(ui, |ui| {
            for h in ["channel", "at A", "at B", "B − A", "mean / min / max"] {
                ui.label(RichText::new(h).small().strong());
            }
            ui.end_row();
            for (i, ch) in rs.review.channels.iter().enumerate().filter(|(_, c)| !c.v.is_empty()) {
                let (units, factor) = display(ch);
                let at = |x: Option<f64>| x.and_then(|x| at_cursor(rs, ch, x, factor));
                let (va, vb) = (at(a), at(b));
                let s = stats.get(i).copied().unwrap_or_default();
                let f = |v: Option<f64>| v.map(view::fmt).unwrap_or_else(|| "--".into());
                ui.label(RichText::new(name(&self.catalogue, ch)).small());
                ui.label(RichText::new(f(va)).small().monospace());
                ui.label(RichText::new(f(vb)).small().monospace());
                ui.label(RichText::new(f(va.zip(vb).map(|(x, y)| y - x))).small().monospace());
                ui.label(RichText::new(if s.n > 0 { format!("{} / {} / {} {units}", view::fmt(s.mean), view::fmt(s.min), view::fmt(s.max)) } else { "--".into() }).small().monospace());
                ui.end_row();
            }
        });
    }

    /// The right panel while reviewing: the recording's description and each channel's
    /// statistics over the stretch in view. No status word, no readout: nothing here
    /// is live.
    pub fn review_table(&mut self, ui: &mut egui::Ui) {
        let Some(rs) = &self.review else { return };
        let (from, to) = rs.stretch();
        let stats = self.review_stats_cached(0, from, to);
        let Some(rs) = &self.review else { return };
        let r = &rs.review;
        let m = &r.meta;
        ui.label(RichText::new("Recording").strong());
        egui::Grid::new("review-meta").num_columns(2).show(ui, |ui| {
            let mut row = |k: &str, v: String| {
                ui.label(RichText::new(k).small().weak());
                ui.label(RichText::new(v).small());
                ui.end_row();
            };
            row("started", local_start(m));
            row("length", clock_text(rs.duration()));
            row("kind", kind_word(m.kind).to_string());
            if let Some(i) = m.interval_ms {
                row("interval", format!("{i} ms"));
            }
            row("controller", m.controller.clone());
            if let Some(s) = &m.system_id {
                row("system", s.clone());
            }
            row("rows", m.rows_written.to_string());
            if !m.label.is_empty() {
                row("label", m.label.clone());
            }
        });
        for n in &r.notes {
            ui.label(RichText::new(n).small().color(theme::WARN));
        }
        if !r.wall_clock {
            ui.label(RichText::new("Times are the controller's own clock (no wall-clock anchor).").small().color(theme::WARN));
        }
        ui.separator();
        ui.label(RichText::new(format!("In view: {:.1} s to {:.1} s", rs.view.0, rs.view.1)).small().weak());
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for (i, ch) in r.channels.iter().enumerate() {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 14.0), egui::Sense::hover());
                        ui.painter().rect_filled(rect, 2.0, theme::PALETTE[i % theme::PALETTE.len()]);
                        ui.label(RichText::new(name(&self.catalogue, ch)).strong()).on_hover_text(about(&self.catalogue, ch));
                    });
                    if !ch.text.is_empty() {
                        let texts: Vec<&(i64, String)> = ch.text.iter().filter(|(t, _)| *t >= from && *t < to).collect();
                        let last_before = ch.text.iter().rev().find(|(t, _)| *t < to).map(|(_, s)| s.as_str()).unwrap_or("--");
                        ui.label(RichText::new(last_before).monospace());
                        ui.label(RichText::new(format!("{} change(s) in view, {} in all", texts.len(), ch.text.len())).small().weak());
                        return;
                    }
                    let (units, _) = display(ch);
                    let s = stats.get(i).copied().unwrap_or_default();
                    if s.n == 0 {
                        ui.label(RichText::new("no samples in view").small().weak());
                        return;
                    }
                    ui.label(RichText::new(format!("mean {} {units}", view::fmt(s.mean))).monospace());
                    ui.label(RichText::new(format!("min {}  max {}  sd {}", view::fmt(s.min), view::fmt(s.max), view::fmt(s.sd))).small().monospace());
                    ui.label(RichText::new(format!("{} samples in view, {} in all", s.n, ch.v.len())).small().weak());
                });
            }
        });
    }
}

/// A channel's value at cursor `x` (seconds on the chart), in its display unit:
/// nothing in a gap, or after the channel ended.
pub(crate) fn at_cursor(rs: &ReviewState, ch: &ReviewChannel, x: f64, factor: f64) -> Option<f64> {
    ch.value_at(rs.t_of(x)).map(|v| v * factor)
}

/// Statistics of a recorded channel over `[from, to)`, in its display unit, read as the
/// signal means it: a wrapping angle's mean on the circle (computed in radians, then
/// scaled). A zero-filled signal's padding was undone when the recording was opened.
pub(crate) fn review_stats(cat: &catalogue::Catalogue, ch: &ReviewChannel, from: i64, to: i64) -> RangeStats {
    let recorded = ch.key.as_ref().map_or(view::Reading::Plain, |k| view::reading(cat.get(k.signal)));
    let r = match ch.derived.as_ref().map_or(recorded, crate::derived_view::reading) {
        view::Reading::ZeroFilled => view::Reading::Plain,
        r => r,
    };
    let v: Vec<f64> = ch.range(from, to).map(|(_, v)| v).collect();
    let (_, factor) = display(ch);
    let mut s = view::stats_of(&v, r);
    // A slow log's extremes are its intervals' recorded minimum and maximum, not the
    // extremes of their means (a dip it caught would be contradicted otherwise).
    if ch.band.is_some()
        && let Some((lo, hi)) = ch.extremes(from, to)
    {
        (s.min, s.max) = (lo, hi);
    }
    RangeStats { n: s.n, mean: s.mean * factor, min: s.min * factor, max: s.max * factor, sd: s.sd * factor }
}

/// For the exports: a recorded channel's display unit and factor, and its name.
pub(crate) fn display_of(ch: &ReviewChannel) -> (String, f64) {
    display(ch)
}

pub(crate) fn name_of(cat: &catalogue::Catalogue, ch: &ReviewChannel) -> String {
    name(cat, ch)
}

/// A recorded channel's display unit and factor (degrees for radians, F1).
fn display(ch: &ReviewChannel) -> (String, f64) {
    let units = ch.entry.as_ref().map(|e| e.units.clone()).unwrap_or_default();
    match catalogue::angle_unit(&units) {
        Some((deg, k)) => (deg.to_string(), k),
        None => (units, 1.0),
    }
}

/// A recorded channel's name: the catalogue's, as for a live channel. A name is an
/// interpretation, not data, and one can be corrected after a recording was made
/// (6000 was "joint angle, measured" until the cell showed it is the EGM reference).
/// The name it was recorded under, where the catalogue does not know the signal.
fn name(cat: &catalogue::Catalogue, ch: &ReviewChannel) -> String {
    match &ch.key {
        Some(k) if cat.get(k.signal).is_some() => view::label(cat, k),
        _ => recorded_name(ch),
    }
}

fn recorded_name(ch: &ReviewChannel) -> String {
    ch.entry.as_ref().map(|e| e.name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| ch.id.clone())
}

fn short(cat: &catalogue::Catalogue, ch: &ReviewChannel) -> String {
    match &ch.key {
        Some(k) => format!("{} · {}", k.signal, name(cat, ch)),
        None => name(cat, ch),
    }
}

/// The hover text of a recorded channel: what it is, and the name it was recorded
/// under when the catalogue now names it differently.
fn about(cat: &catalogue::Catalogue, ch: &ReviewChannel) -> String {
    let now = name(cat, ch);
    let then = recorded_name(ch);
    let what = ch.key.as_ref().and_then(|k| cat.get(k.signal)).map(|s| s.description.clone()).or_else(|| ch.entry.as_ref().map(|e| e.description.clone())).unwrap_or_default();
    if then != now && ch.entry.as_ref().is_some_and(|e| !e.name.is_empty()) {
        format!("{what}\n\nRecorded as \"{then}\"; the catalogue has named it differently since.")
    } else {
        what
    }
}

fn hover(pos: &HoverPosition<'_>, start: i64, wall: bool, units: &str, marks: &[(f64, String)], tol: f64) -> Option<String> {
    let (nm, p) = match pos {
        HoverPosition::NearDataPoint { plot_name, position, .. } => (Some(*plot_name), *position),
        HoverPosition::Elsewhere { position } => (None, *position),
    };
    let t = start.saturating_add((p.x * 1000.0).round() as i64);
    let when = if wall {
        // Checked: zoomed out before 1601 or past year 30827, SystemTime arithmetic
        // panics on Windows.
        let d = Duration::from_millis(t.unsigned_abs());
        let at = if t >= 0 { UNIX_EPOCH.checked_add(d) } else { UNIX_EPOCH.checked_sub(d) };
        at.map_or_else(|| "a time no clock shows".to_string(), view::local_time)
    } else {
        format!("controller {t} ms")
    };
    let text = match nm {
        Some(n) => format!("{n}\n{} {units}\nt = {:.3} s   {when}", view::fmt(p.y), p.x),
        None => format!("t = {:.3} s   {when}\n{} {units}", p.x, view::fmt(p.y)),
    };
    Some(crate::charts::with_mark(text, p.x, tol, marks))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small recording on disk, opened: 4002 J1 at 1000-1040 ms, then after a 5 s gap
    /// at 6000-6040.
    fn gapped_review() -> Review {
        // A folder of its own per call: two tests run this at once, and one removing the
        // folder while the other reads it failed the other now and then.
        static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let call = CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let d = std::env::temp_dir().join(format!("spy-review-gap-{}-{call}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("recording.json"),
            r#"{"format": "abb-signal-spy-recording", "version": 2, "kind": "full", "app": "t", "started_utc": "2026-09-27T10:00:00.000Z", "complete": true,
                "channels": [{"id": "4002/ROB_1/J1", "signal": 4002, "unit": "ROB_1", "axis": 1, "name": "Torque", "units": "Nm", "sample_ms": 4.0}],
                "anchors": [{"controller_ms": 1000, "utc": "2026-09-27T10:00:00.000Z", "row": 0}], "rows_written": 0, "samples_lost": 0}"#,
        )
        .unwrap();
        let mut csv = String::from("controller_ms,channel,value\n");
        for t in (1000..=1040).step_by(4).chain((6000..=6040).step_by(4)) {
            csv += &format!("{t},4002/ROB_1/J1,{}\n", if t < 5000 { 1 } else { 2 });
        }
        std::fs::write(d.join("data.csv"), csv).unwrap();
        let r = spy_core::review::open(&d, &[]).unwrap();
        let _ = std::fs::remove_dir_all(&d);
        r
    }

    #[test]
    fn a_review_cursor_in_a_gap_reads_nothing() {
        let rs = ReviewState::new(gapped_review());
        let ch = rs.review.channel("4002/ROB_1/J1").unwrap().clone();
        assert_eq!(at_cursor(&rs, &ch, 0.02, 1.0), Some(1.0));
        assert_eq!(at_cursor(&rs, &ch, 2.5, 1.0), None, "the value before a 5 s gap, read in it");
        assert_eq!(at_cursor(&rs, &ch, 5.02, 1.0), Some(2.0));
    }

    #[test]
    fn extreme_zoom_and_hovering_before_1601_do_not_crash() {
        let rs = ReviewState::new(gapped_review());
        for x in [1e16, -1e16, f64::MAX, f64::MIN, -11_644_473_700.0] {
            let _ = rs.t_of(x);
            let pos = HoverPosition::Elsewhere { position: egui_plot::PlotPoint::new(x, 0.0) };
            assert!(hover(&pos, rs.review.start, true, "Nm", &[], 0.1).is_some(), "{x}");
        }
    }

    #[test]
    fn a_dropped_folder_or_one_of_its_files_means_the_recording() {
        let d = std::env::temp_dir().join(format!("spy-dropped-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        assert_eq!(recording_dir(&d), None, "no recording.json: not a recording");
        std::fs::write(d.join("recording.json"), "{}").unwrap();
        std::fs::write(d.join("data.csv"), "").unwrap();
        assert_eq!(recording_dir(&d), Some(d.clone()));
        assert_eq!(recording_dir(&d.join("data.csv")), Some(d.clone()));
        assert_eq!(recording_dir(&d.join("recording.json")), Some(d.clone()));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_recorded_channel_goes_by_the_catalogues_name_now() {
        // Recorded on the cell on 2026-09-26 before 6000 was found to be the EGM
        // reference: its old name stays in the recording, the review shows the right one.
        let d = std::env::temp_dir().join(format!("spy-review-name-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("recording.json"),
            r#"{"format": "abb-signal-spy-recording", "version": 1, "kind": "full", "app": "t", "started_utc": "2026-09-26T08:47:20.000Z", "complete": true,
                "channels": [{"id": "6000/ROB_1/1", "signal": 6000, "unit": "ROB_1", "axis": 1, "name": "Joint angle (measured)  ROB_1 J1", "units": "rad"},
                             {"id": "77777/ROB_1/1", "signal": 77777, "unit": "ROB_1", "axis": 1, "name": "Something new", "units": "V"}],
                "rows_written": 2, "samples_lost": 0}"#,
        )
        .unwrap();
        std::fs::write(d.join("data.csv"), "controller_ms,channel,value\n10,6000/ROB_1/1,0.5\n10,77777/ROB_1/1,1\n").unwrap();
        let r = spy_core::review::open(&d, &[]).unwrap();
        let cat = catalogue::Catalogue::builtin();
        let ch = r.channel("6000/ROB_1/J1").unwrap();
        assert_eq!(name(&cat, ch), "Joint reference (EGM)  ROB_1 J1");
        assert!(about(&cat, ch).contains("Recorded as \"Joint angle (measured)  ROB_1 J1\""), "{}", about(&cat, ch));
        let unknown = r.channel("77777/ROB_1/J1").unwrap();
        assert_eq!(name(&cat, unknown), "Something new", "a signal the catalogue does not know keeps its recorded name");
        assert_eq!(display(ch), ("deg".to_string(), 180.0 / std::f64::consts::PI));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_recorded_resolver_angle_averages_on_the_circle() {
        let d = std::env::temp_dir().join(format!("spy-review-wrap-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("recording.json"),
            r#"{"format": "abb-signal-spy-recording", "version": 2, "kind": "full", "app": "t", "started_utc": "2026-09-27T10:00:00.000Z", "complete": true,
                "channels": [{"id": "5138/ROB_1/J1", "signal": 5138, "unit": "ROB_1", "axis": 1, "name": "Resolver angle", "units": "rad", "sample_ms": 4.032}],
                "rows_written": 4, "samples_lost": 0}"#,
        )
        .unwrap();
        let tau = std::f64::consts::TAU;
        std::fs::write(d.join("data.csv"), format!("controller_ms,channel,value\n0,5138/ROB_1/J1,{}\n4,5138/ROB_1/J1,0.001\n8,5138/ROB_1/J1,{}\n12,5138/ROB_1/J1,0.001\n", tau - 0.001, tau - 0.001)).unwrap();
        let r = spy_core::review::open(&d, &[]).unwrap();
        let cat = catalogue::Catalogue::builtin();
        let s = review_stats(&cat, r.channel("5138/ROB_1/J1").unwrap(), 0, 100);
        assert_eq!(s.n, 4);
        assert!(s.mean < 0.01 || s.mean > 359.99, "the mean is {} deg: averaged across the wrap", s.mean);
        let _ = std::fs::remove_dir_all(&d);
    }
}
