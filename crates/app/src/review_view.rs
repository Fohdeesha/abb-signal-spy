//! Reviewing a recording: open one from the Recordings window, by its path,
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
use crate::fields;
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
    /// A chart's vertical scale as Ctrl + wheel left it, by (signal, unit).
    pub zoom: std::collections::HashMap<(u32, String), (f64, f64)>,
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
        ReviewState { review: Arc::new(review), view: (0.0, dur), fresh: true, cursors_on: false, cursor_a: None, cursor_b: None, zoom: Default::default(), stats_computed: 0, stats_cache: [None, None] }
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

/// A reviewed chart: its signal (or kind of derived channel) and display unit.
type Lane = (u32, String);

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
                if ui.small_button("refresh").clicked() {
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
                            ui.label(RichText::new("cut short").color(theme::pal(ui).hold)).on_hover_text("Not closed properly (the program or the PC stopped while recording): the data up to then is there.");
                        } else {
                            ui.label("");
                        }
                        if ui.button("open").clicked() {
                            chosen = Some(dir.clone());
                        }
                        ui.end_row();
                    }
                });
            });
            ui.separator();
            ui.horizontal(|ui| {
                fields::line(ui, &mut self.recording_path_input, "Recording folder", |t| t.hint_text("C:\\path\\to\\a recording folder").desired_width(420.0));
                if ui.button("open").clicked() {
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

    /// The strip while reviewing (G49): REVIEWING, not live, in a block of its own;
    /// what is shown; the live session still running underneath; and the way back.
    pub fn review_strip(&mut self, ui: &mut egui::Ui) {
        let p = theme::pal(ui);
        let st = self.session.status().clone();
        let Some(rs) = &self.review else { return };
        let m = rs.review.meta.clone();
        let name = rs.review.dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let length = clock_text(rs.duration());
        let mut close = false;
        let gap = 16.0;
        let (block, button) = (230.0, 210.0);
        let w = ((ui.available_width() - block - button - gap * 4.0) / 3.0).floor().max(80.0);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            let (rect, _) = ui.allocate_exact_size(egui::vec2(block, 48.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, 0.0, p.primary);
            let mut inner = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(egui::vec2(12.0, 3.0))).layout(egui::Layout::top_down(egui::Align::Min)));
            inner.spacing_mut().item_spacing.y = 0.0;
            inner.label(RichText::new("REVIEWING").font(egui::FontId::new(20.0, theme::heavy())).color(p.on_primary));
            inner.label(theme::b("not live").size(14.0).color(p.on_primary));
            theme::status_cell(ui, w, "recording", if m.label.is_empty() { &name } else { &m.label }, p.ink, theme::Mark::None, p.ink).on_hover_text(rs_dir_text(&self.review));
            let started = local_start(&m);
            theme::status_cell(ui, w, "recorded", &format!("{}, {length}", started.get(..16).unwrap_or(&started)), p.ink, theme::Mark::None, p.ink).on_hover_text(format!("{started}: {}", kind_word(m.kind)));
            let (word, color, mark) = SpyApp::phase_word(&st, p);
            theme::status_cell(ui, w, "live session, still running", &word, color, mark, if mark == theme::Mark::Off { p.ink } else { color });
            if ui.add(egui::Button::new("close the recording").min_size(egui::vec2(button, 46.0))).clicked() {
                close = true;
            }
        });
        let mut notes: Vec<(String, egui::Color32)> = Vec::new();
        if !m.complete {
            notes.push(("Cut short: not closed properly (the program or the PC stopped while recording). The data up to then is here.".into(), p.hold));
        }
        if m.samples_lost > 0 {
            notes.push((format!("{} samples lost while recording: the disk could not keep up.", m.samples_lost), p.red));
        }
        for (text, color) in notes {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                theme::square(ui, color, 10.0);
                ui.label(theme::b(text).color(color));
            });
        }
        if close {
            self.review = None;
        }
    }

    /// Charts of the recording: each signal (with its axes overlaid) in a chart of its
    /// own unit, on the recording's clock. Drag to move, the wheel zooms time, Ctrl +
    /// wheel a chart's vertical scale, a double-click goes back to the whole of it.
    pub fn review_charts(&mut self, ui: &mut egui::Ui) {
        let p = theme::pal(ui);
        let (mut save_csv, mut save_png) = (false, false);
        let (xy_open, mut toggle_xy) = (self.xy.is_some(), false);
        let Some(rs) = &mut self.review else { return };
        let dur = rs.duration();
        let (cursors_on, placed) = (rs.cursors_on, rs.cursor_a.is_some() || rs.cursor_b.is_some());
        let (mut show_all, mut cursors, mut clear) = (false, false, false);
        ui.scope(|ui| {
            ui.spacing_mut().button_padding.x = 9.0;
            ui.spacing_mut().item_spacing.x = 6.0;
            let cursors_text = if cursors_on { "cursors: on" } else { "cursors" };
            let mut left = vec!["show all", cursors_text];
            if cursors_on && placed {
                left.push("clear cursors");
            }
            let (left_w, right_w) = (theme::buttons_width(ui, &left, 0.0), theme::buttons_width(ui, &["save csv", "save png", "xy plot"], 0.0));
            theme::section_tools(
                ui,
                "02",
                "recording",
                left_w,
                right_w,
                |ui| {
                    show_all = theme::tool(ui, "show all").on_hover_text("The whole recording").clicked();
                    cursors = ui.add(egui::Button::new(cursors_text).selected(cursors_on).min_size(egui::vec2(0.0, theme::TOOL_H))).on_hover_text("Click a chart to place cursor A, right-click for cursor B: each channel's row reads them").clicked();
                    if cursors_on && placed {
                        clear = theme::tool(ui, "clear cursors").clicked();
                    }
                },
                |ui| {
                    toggle_xy = ui.add(egui::Button::new("xy plot").selected(xy_open).min_size(egui::vec2(0.0, theme::TOOL_H))).on_hover_text(crate::charts::XY_HOVER).clicked();
                    save_png = theme::tool(ui, "save png").on_hover_text("Save a picture of the charts to the recordings folder").clicked();
                    save_csv = theme::tool(ui, "save csv").on_hover_text("Save the samples in view, as recorded, to a CSV file in the recordings folder").clicked();
                },
            );
        });
        if show_all {
            rs.view = (0.0, dur);
            rs.fresh = true;
            rs.zoom.clear();
        }
        if cursors {
            rs.cursors_on = !rs.cursors_on;
        }
        if clear {
            rs.cursor_a = None;
            rs.cursor_b = None;
        }
        // How to place the cursors, step by step (G48).
        if rs.cursors_on {
            ui.horizontal_wrapped(|ui| {
                ui.label(theme::b("cursors"));
                let step = |ui: &mut egui::Ui, n: &str, text: String, current: bool| {
                    let galley_text = RichText::new(text);
                    egui::Frame::new()
                        .stroke(egui::Stroke::new(if current { 2.0 } else { 1.0 }, if current { p.hold } else { p.off_edge }))
                        .inner_margin(egui::Margin::symmetric(10, 5))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(n).font(egui::FontId::new(16.0, theme::heavy())).color(if current { p.hold } else { p.ink }));
                                ui.label(if current { galley_text.color(p.ink).family(theme::bold()) } else { galley_text.color(p.ink2) });
                            });
                        });
                };
                match (rs.cursor_a, rs.cursor_b) {
                    (None, _) => {
                        step(ui, "1", "click a chart to place A".into(), true);
                        step(ui, "2", "then right-click one to place B".into(), false);
                    }
                    (Some(a), None) => {
                        step(ui, "1", format!("A placed at {}", clock_text(a)), false);
                        step(ui, "2", "now right-click a chart to place B".into(), true);
                    }
                    (Some(a), Some(b)) => {
                        step(ui, "1", format!("A at {}", clock_text(a)), false);
                        step(ui, "2", format!("B at {}, {:.3} s after A", clock_text(b), b - a), false);
                    }
                }
            });
        }
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
            ui.centered_and_justified(|ui| ui.label(RichText::new("Nothing to chart: the recording holds only text signals, or no samples.").color(p.ink2)));
            return;
        }
        let fit = (ui.available_height() - 26.0) / lanes.len() as f32 - 30.0 - 8.0;
        let lane_h = fit.max(90.0);
        // More charts than fit: each its own time axis, the last one's being out of sight.
        let every_axis = fit < 90.0;
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
        let dark = ui.visuals().dark_mode;
        let mut transforms = Vec::new();
        // A chart's new vertical zoom, or `None` for its own scale back.
        let mut zooms: Vec<(Lane, Option<(f64, f64)>)> = Vec::new();
        let mut back = false;
        egui::ScrollArea::vertical().id_salt("review-lanes").auto_shrink([false, false]).show(ui, |ui| {
            for (k, ((signal, units), members)) in lanes.iter().enumerate() {
                let first = &review.channels[members[0]];
                ui.horizontal(|ui| {
                    ui.set_height(30.0);
                    ui.spacing_mut().item_spacing.x = 8.0;
                    for &i in members.iter().take(6) {
                        theme::square(ui, theme::channel_color(i, dark), 12.0);
                    }
                    let title = if members.len() == 1 { short(cat, first) } else { format!("{} (+{} overlaid)", short(cat, first), members.len() - 1) };
                    ui.add(egui::Label::new(theme::b(title)).truncate());
                    ui.label(RichText::new(units).color(p.ink3));
                    if rs.zoom.contains_key(&(*signal, units.clone())) {
                        theme::badge(ui, "scale zoomed", p.hold, "Ctrl + wheel set this scale; a double-click gives the chart's own back.");
                    }
                });
                let u2 = units.clone();
                let marks2 = marks.clone();
                let shown = hover_text.clone();
                let last_lane = k + 1 == lanes.len();
                let mut plot = Plot::new(("review", *signal, units.as_str()))
                    .height(lane_h)
                    .link_axis("review-x", [true, false])
                    .link_cursor("review-x", [true, false])
                    .custom_x_axes(vec![theme::time_axis(p, |mark, _| clock_text(mark.value))])
                    .grid_spacing(crate::charts::GRID_PX..=300.0)
                    .x_grid_spacer(|input| crate::charts::time_marks(input, 0.0))
                    .custom_y_axes(vec![theme::value_axis(p)])
                    .show_axes([last_lane || every_axis, true])
                    .allow_drag([true, false])
                    .allow_zoom(false)
                    .allow_scroll(false)
                    .allow_boxed_zoom(false)
                    .allow_double_click_reset(false)
                    .label_formatter(move |pos| crate::charts::remember(&shown, hover(pos, start, wall, &u2, &marks2, mark_tol)));
                if members.len() > 1 {
                    plot = plot.legend(Legend::default().position(egui_plot::Corner::LeftTop));
                }
                // The widest its channels need (a motor's angle beside a joint's).
                let min_span = members.iter().map(|&i| min_span_for(units, review.channels[i].key.as_ref().and_then(|k| cat.get(k.signal)))).fold(0.0, f64::max);
                let zoomed = rs.zoom.get(&(*signal, units.clone())).copied();
                let resp = plot.show(ui, |pu| {
                    let b = pu.plot_bounds();
                    let (mut vx0, mut vx1) = if fresh || !b.is_valid_x() { view_out } else { (b.min()[0], b.max()[0]) };
                    let hovered = pu.response().hovered();
                    let (zoom, scroll) = if hovered { pu.ctx().input(|i| (i.zoom_delta(), i.smooth_scroll_delta.y)) } else { (1.0, 0.0) };
                    if hovered && (zoom != 1.0 || scroll != 0.0) {
                        pu.ctx().input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
                    }
                    if scroll != 0.0
                        && let Some(px) = pu.pointer_coordinate().map(|q| q.x)
                    {
                        let f = f64::from((-scroll * 0.0025).exp());
                        let w = ((vx1 - vx0) * f).clamp(0.01, (dur * 1.2).max(1.0));
                        let k = (px - vx0) / (vx1 - vx0).max(1e-9);
                        (vx0, vx1) = (px - w * k, px - w * k + w);
                        pu.set_plot_bounds_x(vx0..=vx1);
                    }
                    let from = start + (vx0 * 1000.0).floor() as i64;
                    let to = start + (vx1 * 1000.0).ceil() as i64 + 1;
                    let px = pu.response().rect.width().max(50.0) as usize;
                    let (mut ymin, mut ymax) = (f64::INFINITY, f64::NEG_INFINITY);
                    for (n, &i) in members.iter().enumerate() {
                        let ch = &review.channels[i];
                        let (_, factor) = display(ch);
                        let color = theme::channel_color(i, dark);
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
                            pu.line(Line::new(short(cat, ch), PlotPoints::from(pts)).color(color).width(2.0).id(egui::Id::new(("review-line", i, si, n))));
                        }
                    }
                    for m in &review.marks {
                        let x = (m.t - start) as f64 / 1000.0;
                        if x >= vx0 && x <= vx1 {
                            let c = if m.kind == "marker" { p.hold } else { p.ink2 };
                            pu.vline(VLine::new("", x).color(c).width(1.5).style(egui_plot::LineStyle::dashed_dense()));
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
                    if fresh {
                        pu.set_plot_bounds_x(view_out.0..=view_out.1);
                    }
                    let (mut y0, mut y1) = zoomed.unwrap_or_else(|| if ymin.is_finite() { autoscale(ymin, ymax, min_span) } else { (-1.0, 1.0) });
                    let mut new_zoom = None;
                    if zoom != 1.0
                        && let Some(py) = pu.pointer_coordinate().map(|q| q.y)
                    {
                        let z = f64::from(zoom);
                        (y0, y1) = (py - (py - y0) / z, py + (y1 - py) / z);
                        new_zoom = Some((y0, y1));
                    }
                    pu.set_plot_bounds_y(y0..=y1);
                    ((vx0, vx1), pu.response().clicked(), pu.response().secondary_clicked(), pu.pointer_coordinate().map(|p| p.x), new_zoom, pu.response().double_clicked())
                });
                transforms.push(resp.transform);
                let ((vx0, vx1), clicked, secondary, x_at, new_zoom, double) = resp.inner;
                view_out = (vx0, vx1);
                if new_zoom.is_some() {
                    zooms.push(((*signal, units.clone()), new_zoom));
                }
                if double {
                    zooms.push(((*signal, units.clone()), None));
                    back = true;
                }
                if cursors_on {
                    if clicked {
                        clicked_a = x_at;
                    }
                    if secondary {
                        clicked_b = x_at;
                    }
                }
                ui.add_space(8.0);
            }
        });
        self.lane_transforms = transforms;
        if let Some(rs) = &mut self.review {
            rs.view = view_out;
            rs.fresh = false;
            for (lane, z) in zooms {
                match z {
                    Some(z) => {
                        rs.zoom.insert(lane, z);
                    }
                    None => {
                        rs.zoom.remove(&lane);
                    }
                }
            }
            if back {
                // Back to the whole recording.
                rs.view = (0.0, rs.duration());
                rs.fresh = true;
            }
            if clicked_a.is_some() {
                rs.cursor_a = clicked_a;
            }
            if clicked_b.is_some() {
                rs.cursor_b = clicked_b;
            }
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

    /// The right sheet while reviewing: each channel's statistics over the stretch in
    /// view, or its readings at the cursors (G47: on its row), and the recording's
    /// description. No status word, no readout: nothing here is live.
    pub fn review_table(&mut self, ui: &mut egui::Ui) {
        let p = theme::pal(ui);
        let Some(rs) = &self.review else { return };
        let (from, to) = rs.stretch();
        let (a, b, cursors_on) = (rs.cursor_a, rs.cursor_b, rs.cursors_on);
        let between = match (a, b) {
            (Some(a), Some(b)) => Some((rs.t_of(a.min(b)), rs.t_of(a.max(b)).saturating_add(1))),
            _ => None,
        };
        let stats = self.review_stats_cached(0, from, to);
        let between_stats = between.map(|(f, t)| self.review_stats_cached(1, f, t));
        let Some(rs) = &self.review else { return };
        let r = &rs.review;
        let m = &r.meta;
        theme::section(ui, "03", "channels", true, |ui| {
            ui.label(RichText::new("in the recording").color(p.ink2));
        });
        let dark = ui.visuals().dark_mode;
        let info_h = 150.0;
        egui::ScrollArea::vertical().auto_shrink([false, false]).max_height((ui.available_height() - info_h).max(80.0)).show(ui, |ui| {
            for (i, ch) in r.channels.iter().enumerate() {
                ui.scope(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.horizontal(|ui| {
                        theme::square(ui, theme::channel_color(i, dark), 12.0);
                        ui.add(egui::Label::new(theme::b(short(&self.catalogue, ch))).truncate()).on_hover_text(about(&self.catalogue, ch));
                    });
                    if !ch.text.is_empty() {
                        let texts: Vec<&(i64, String)> = ch.text.iter().filter(|(t, _)| *t >= from && *t < to).collect();
                        let last_before = ch.text.iter().rev().find(|(t, _)| *t < to).map(|(_, s)| s.as_str()).unwrap_or("--");
                        ui.label(theme::num(last_before, 20.0));
                        ui.label(RichText::new(format!("{} change(s) in view, {} in all", texts.len(), ch.text.len())).size(14.0).color(p.ink2));
                    } else {
                        let (units, factor) = display(ch);
                        let s = stats.get(i).copied().unwrap_or_default();
                        let f = |v: Option<f64>| v.map(view::fmt).unwrap_or_else(|| "--".into());
                        if cursors_on {
                            let va = a.and_then(|x| at_cursor(rs, ch, x, factor));
                            let vb = b.and_then(|x| at_cursor(rs, ch, x, factor));
                            ui.horizontal(|ui| {
                                ui.label(theme::b("A"));
                                ui.label(theme::num(f(va), 24.0));
                                ui.label(RichText::new(&units).color(p.ink2));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| match (va.is_some() || a.is_some(), b) {
                                    (_, Some(_)) => {
                                        ui.label(theme::num(f(vb), 18.0));
                                        ui.label(theme::b("B"));
                                    }
                                    (true, None) => {
                                        ui.label(theme::b("B: right-click").size(14.0).color(p.hold));
                                    }
                                    (false, None) => {
                                        ui.label(theme::b("A: click a chart").size(14.0).color(p.hold));
                                    }
                                });
                            });
                            if let (Some(x), Some(y)) = (va, vb) {
                                ui.label(RichText::new(format!("B − A {} {units}", view::fmt(y - x))).monospace().size(14.0));
                            }
                            match between_stats.as_ref().and_then(|v| v.get(i)) {
                                Some(bs) => crate::channels::stats_grid(ui, bs, "between A and B", p),
                                None => crate::channels::stats_grid(ui, &s, "in view", p),
                            }
                        } else if s.n == 0 {
                            ui.label(RichText::new("no samples in view").color(p.ink2));
                        } else {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("mean").color(p.ink2));
                                ui.label(theme::num(view::fmt(s.mean), 24.0));
                                ui.label(RichText::new(&units).color(p.ink2));
                            });
                            crate::channels::stats_grid(ui, &s, &format!("{} samples in view, {} in all", s.n, ch.v.len()), p);
                        }
                    }
                });
                let y = ui.cursor().top() + 2.0;
                ui.painter().hline(ui.max_rect().x_range(), y, egui::Stroke::new(1.0, p.line));
                ui.add_space(8.0);
            }
        });
        // The recording itself, along the bottom.
        let y = ui.cursor().top() + 4.0;
        ui.painter().hline(ui.max_rect().x_range(), y, egui::Stroke::new(2.0, p.ink));
        ui.add_space(10.0);
        egui::ScrollArea::vertical().id_salt("review-meta").auto_shrink([false, true]).show(ui, |ui| {
            egui::Grid::new("review-meta").num_columns(2).spacing(egui::vec2(12.0, 2.0)).show(ui, |ui| {
                let mut row = |k: &str, v: String| {
                    ui.label(RichText::new(k).size(15.0).color(p.ink2));
                    ui.label(RichText::new(v).size(15.0));
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
                row("samples", format!("{} rows, {}", m.rows_written, if m.samples_lost > 0 { format!("{} lost", m.samples_lost) } else { "none lost".into() }));
                if !m.label.is_empty() {
                    row("label", m.label.clone());
                }
            });
            for n in &r.notes {
                ui.label(RichText::new(n).size(14.0).color(p.hold));
            }
            if !r.wall_clock {
                ui.label(RichText::new("Times are the controller's own clock (no wall-clock anchor).").size(14.0).color(p.hold));
            }
        });
    }
}

fn rs_dir_text(review: &Option<ReviewState>) -> String {
    review.as_ref().map(|r| r.review.dir.display().to_string()).unwrap_or_default()
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

pub(crate) fn short_of(cat: &catalogue::Catalogue, ch: &ReviewChannel) -> String {
    short(cat, ch)
}

/// A recorded channel's display unit and factor (degrees for radians).
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
    use spy_core::testdir::TestDir;

    /// A small recording on disk, opened: 4002 J1 at 1000-1040 ms, then after a 5 s gap
    /// at 6000-6040.
    fn gapped_review() -> Review {
        // A folder of its own per call: two tests run this at once, and one removing the
        // folder while the other reads it failed the other now and then.
        let d = TestDir::new("review-gap");
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
        spy_core::review::open(&d, &[]).unwrap()
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
        let d = TestDir::new("dropped");
        assert_eq!(recording_dir(&d), None, "no recording.json: not a recording");
        std::fs::write(d.join("recording.json"), "{}").unwrap();
        std::fs::write(d.join("data.csv"), "").unwrap();
        assert_eq!(recording_dir(&d), Some(d.to_path_buf()));
        assert_eq!(recording_dir(&d.join("data.csv")), Some(d.to_path_buf()));
        assert_eq!(recording_dir(&d.join("recording.json")), Some(d.to_path_buf()));
    }

    #[test]
    fn a_recorded_channel_goes_by_the_catalogues_name_now() {
        // Recorded on the cell on 2026-09-26 before 6000 was found to be the EGM
        // reference: its old name stays in the recording, the review shows the right one.
        let d = TestDir::new("review-name");
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
    }

    #[test]
    fn a_recorded_resolver_angle_averages_on_the_circle() {
        let d = TestDir::new("review-wrap");
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
    }
}
