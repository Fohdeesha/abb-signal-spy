//! The charts: stacked lanes on one controller-time axis (F2), a rolling window of
//! 1 s to 10 min (10 s by default, F4), pause and scroll back through the 10-minute
//! history, per-lane autoscale with a lock, hover with the wall-clock time, two
//! cursors with the time and value difference, and statistics of the visible window
//! (C6). A gap is drawn as a gap, never interpolated across, and a stale channel's
//! empty stretch is shaded.

use std::collections::HashMap;

use eframe::egui::{self, RichText};
use egui_plot::{HoverPosition, Legend, Line, Plot, PlotPoints, Span, VLine};

use spy_core::catalogue::flag;
use spy_core::session::Phase;
use spy_core::store::ChannelKey;
use spy_core::timeline::Timeline;

use crate::app::SpyApp;
use crate::theme;
use crate::view::{self, Health};

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
    fn charted(&self, i: usize, st: &spy_core::session::Status) -> bool {
        let c = &self.chans[i];
        if self.catalogue.get(c.key.signal).is_some_and(|s| s.value_type.as_deref() == Some("string")) {
            return false;
        }
        if self.session.store().get(&c.key).is_some_and(|ch| ch.lock().kind == Some(spy_core::sample::ValueKind::String)) {
            return false;
        }
        !st.channels.iter().any(|s| s.key == c.key && matches!(s.state, spy_core::session::ChannelState::Refused { .. }))
    }

    fn lanes(&self, st: &spy_core::session::Status) -> Vec<u32> {
        let mut v: Vec<u32> = Vec::new();
        for (i, c) in self.chans.iter().enumerate() {
            if self.charted(i, st) && !v.contains(&c.lane) {
                v.push(c.lane);
            }
        }
        v
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
        let lanes = self.lanes(&st);
        if lanes.is_empty() {
            ui.centered_and_justified(|ui| ui.label(RichText::new("Nothing to chart: the channels are text signals or were refused (see the table).").weak()));
            return;
        }
        let stats_h = if self.cursors_on { 26.0 * (self.chans.len() as f32 + 2.0) } else { 0.0 };
        let lane_h = ((ui.available_height() - stats_h) / lanes.len() as f32 - 22.0).max(80.0);
        let connected = st.phase.is_connected();
        let mut clicked_a: Option<f64> = None;
        let mut clicked_b: Option<f64> = None;
        let mut visible_x = (x_min, x_max);

        egui::ScrollArea::vertical().id_salt("lanes").auto_shrink([false, false]).max_height(ui.available_height() - stats_h).show(ui, |ui| {
            for lane in &lanes {
                let members: Vec<usize> = (0..self.chans.len()).filter(|&i| self.chans[i].lane == *lane && self.charted(i, &st)).collect();
                let first = &self.chans[members[0]];
                let sig0 = self.catalogue.get(first.key.signal);
                let disp = view::display(sig0, first.radians);
                let title = if members.len() == 1 {
                    view::label(&self.catalogue, &first.key)
                } else {
                    format!("{} (+{} overlaid)", view::label(&self.catalogue, &first.key), members.len() - 1)
                };
                let locked = self.lane_lock.get(lane).copied();
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&title).small().strong());
                    ui.label(RichText::new(format!("[{}]", disp.units)).small().weak());
                    if members.iter().any(|&i| self.catalogue.get(self.chans[i].key.signal).is_some_and(|s| s.has(flag::FROZEN))) {
                        theme::badge(ui, "FROZEN", theme::WARN, "Holds the last RAPID path position; does not move while EGM drives the robot.");
                    }
                    let mut lock = locked.is_some();
                    if ui.toggle_value(&mut lock, "🔒").on_hover_text("Lock this chart's vertical scale").changed() {
                        if lock {
                            self.lane_lock.insert(*lane, (f64::NAN, f64::NAN));
                        } else {
                            self.lane_lock.remove(lane);
                        }
                    }
                });

                let tl2 = tl.clone();
                let units = disp.units.clone();
                let mut plot = Plot::new(("lane", *lane))
                    .height(lane_h)
                    .link_axis("x-link", [true, false])
                    .link_cursor("x-link", [true, false])
                    .legend(Legend::default().position(egui_plot::Corner::LeftTop))
                    .y_axis_min_width(56.0)
                    .show_axes([true, true])
                    .label_formatter(move |pos| hover_label(pos, &tl2, &units));
                plot = if live {
                    plot.allow_drag(false).allow_zoom(false).allow_scroll(false).allow_boxed_zoom(false).allow_double_click_reset(false)
                } else {
                    plot.allow_drag([true, false]).allow_zoom([true, false]).allow_scroll([true, false]).allow_boxed_zoom(false).allow_double_click_reset(false)
                };

                let chans: Vec<(ChannelKey, egui::Color32, f64, bool, bool, String)> = members
                    .iter()
                    .map(|&i| {
                        let c = &self.chans[i];
                        let d = view::display(self.catalogue.get(c.key.signal), c.radians);
                        (c.key.clone(), c.color, d.factor, c.hold_nonzero, false, view::short_label(&self.catalogue, &c.key))
                    })
                    .collect();
                let healths: HashMap<ChannelKey, (Health, Option<i64>)> = chans
                    .iter()
                    .map(|(k, ..)| {
                        let cs = st.channels.iter().find(|c| &c.key == k);
                        let h = view::health(cs, connected, self.catalogue.get(k.signal), st.loopback);
                        let last = self.session.store().get(k).and_then(|ch| ch.lock().last().map(|(t, _)| t));
                        (k.clone(), (h, last))
                    })
                    .collect();
                let store = self.session.store().clone();
                let markers: Vec<(f64, String)> = self.markers.iter().map(|m| (tl.seconds(m.t_ms), m.label.clone())).collect();
                let (ca, cb) = (self.cursor_a, self.cursor_b);
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
                    for (key, color, factor, hold, _, name) in &chans {
                        let Some(ch) = store.get(key) else { continue };
                        let ring = ch.lock();
                        let mut held = f64::NAN;
                        let segs = ring.decimate(from, to, px, |v| {
                            let v = if *hold {
                                if v != 0.0 {
                                    held = v;
                                    v
                                } else {
                                    held
                                }
                            } else {
                                v
                            };
                            v * factor
                        });
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
                            pu.line(Line::new(name.clone(), PlotPoints::from(pts)).color(*color).width(1.4).id(egui::Id::new((key.id(), si))));
                        }
                        // A stale channel: shade from its last sample to the right edge.
                        if let Some((h, last)) = healths.get(key)
                            && matches!(h, Health::Stale | Health::NotConnected)
                                && let Some(t) = last {
                                    let x = (*t - origin) as f64 / 1000.0;
                                    if x < vx1 {
                                        pu.span(Span::new(format!("{name} stale"), x..=vx1).fill(theme::WARN.gamma_multiply(0.08)).border_width(0.0));
                                    }
                                }
                    }
                    for (x, label) in &markers {
                        if *x >= vx0 && *x <= vx1 {
                            pu.vline(VLine::new(format!("marker {label}"), *x).color(theme::WARN).width(1.0));
                        }
                    }
                    if cursors_on {
                        if let Some(a) = ca {
                            pu.vline(VLine::new("cursor A", a).color(egui::Color32::from_rgb(0x5A, 0x9B, 0xD5)).width(1.5));
                        }
                        if let Some(bx) = cb {
                            pu.vline(VLine::new("cursor B", bx).color(egui::Color32::from_rgb(0xD3, 0x72, 0x95)).width(1.5));
                        }
                    }
                    if live || pause_fresh {
                        pu.set_plot_bounds_x(x_min..=x_max);
                    }
                    let (y0, y1) = match locked {
                        Some((a, bb)) if a.is_finite() && bb.is_finite() => (a, bb),
                        _ if ymin.is_finite() => autoscale(ymin, ymax),
                        _ => (-1.0, 1.0),
                    };
                    pu.set_plot_bounds_y(y0..=y1);
                    let clicked = pu.response().clicked();
                    let secondary = pu.response().secondary_clicked();
                    let x_at = pu.pointer_coordinate().map(|p| p.x);
                    ((vx0, vx1), (y0, y1), clicked, secondary, x_at)
                });
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
        self.pause_fresh = false;
        if let Some(a) = clicked_a {
            self.cursor_a = Some(a);
        }
        if let Some(b) = clicked_b {
            self.cursor_b = Some(b);
        }
        if self.cursors_on {
            self.cursor_table(ui, &tl, visible_x);
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
        });
    }

    fn cursor_table(&mut self, ui: &mut egui::Ui, tl: &Timeline, visible: (f64, f64)) {
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
        let at = |key: &ChannelKey, x: f64| -> Option<f64> {
            let ch = self.session.store().get(key)?;
            let r = ch.lock();
            let t = origin + (x * 1000.0).round() as i64;
            let i = r.lower_bound(t + 1);
            if i == 0 {
                return None;
            }
            r.range(i64::MIN, t + 1).last().map(|(_, v)| v)
        };
        egui::Grid::new("cursor-stats").striped(true).num_columns(8).show(ui, |ui| {
            for h in ["channel", "at A", "at B", "B − A", "mean", "min", "max", "std dev"] {
                ui.label(RichText::new(h).small().strong());
            }
            ui.end_row();
            for c in &self.chans {
                let d = view::display(self.catalogue.get(c.key.signal), c.radians);
                let va = a.and_then(|x| at(&c.key, x)).map(|v| v * d.factor);
                let vb = b.and_then(|x| at(&c.key, x)).map(|v| v * d.factor);
                let s = match self.session.store().get(&c.key) {
                    Some(ch) => {
                        let r = ch.lock();
                        let from = origin + (range.0 * 1000.0).floor() as i64;
                        let to = origin + (range.1 * 1000.0).ceil() as i64 + 1;
                        range_stats(r.range(from, to).map(|(_, v)| v * d.factor))
                    }
                    None => RangeStats::default(),
                };
                let f = |v: Option<f64>| v.map(view::fmt).unwrap_or_else(|| "--".into());
                ui.label(RichText::new(view::label(&self.catalogue, &c.key)).small().color(c.color));
                ui.label(RichText::new(f(va)).small().monospace());
                ui.label(RichText::new(f(vb)).small().monospace());
                ui.label(RichText::new(f(va.zip(vb).map(|(x, y)| y - x))).small().monospace());
                let has = s.n > 0;
                ui.label(RichText::new(if has { view::fmt(s.mean) } else { "--".into() }).small().monospace());
                ui.label(RichText::new(if has { view::fmt(s.min) } else { "--".into() }).small().monospace());
                ui.label(RichText::new(if has { view::fmt(s.max) } else { "--".into() }).small().monospace());
                ui.label(RichText::new(if has { format!("{} {}", view::fmt(s.sd), d.units) } else { "--".into() }).small().monospace());
                ui.end_row();
            }
        });
    }
}

/// The vertical range for data spanning `lo..hi`: an 8% margin, and for a constant
/// (or all but constant) signal a band around it wide enough to read, rather than
/// a range of 1e-9 whose axis labels are all zeros.
pub fn autoscale(lo: f64, hi: f64) -> (f64, f64) {
    let mag = lo.abs().max(hi.abs());
    let span = hi - lo;
    if span <= mag * 1e-6 + 1e-12 {
        let c = (lo + hi) / 2.0;
        let half = (c.abs() * 0.05).max(1.0);
        return (c - half, c + half);
    }
    (lo - span * 0.08, hi + span * 0.08)
}

fn hover_label(pos: &HoverPosition<'_>, tl: &Timeline, units: &str) -> Option<String> {
    let (name, p) = match pos {
        HoverPosition::NearDataPoint { plot_name, position, .. } => (Some(*plot_name), *position),
        HoverPosition::Elsewhere { position } => (None, *position),
    };
    let t = tl.origin().unwrap_or(0) + (p.x * 1000.0).round() as i64;
    let wall = tl.wall(t).map(view::local_time).unwrap_or_default();
    Some(match name {
        Some(n) => format!("{n}\n{} {units}\nt = {:.3} s   {wall}", view::fmt(p.y), p.x),
        None => format!("t = {:.3} s   {wall}\n{} {units}", p.x, view::fmt(p.y)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(autoscale(0.0, 0.0), (-1.0, 1.0));
        let (a, b) = autoscale(356.7, 356.7);
        assert!((a - 338.865).abs() < 1e-9 && (b - 374.535).abs() < 1e-9);
        let (a, b) = autoscale(7.6757e-7, 7.6757e-7);
        assert!(a < -0.99 && b > 0.99);
        let (a, b) = autoscale(10.0, 20.0);
        assert_eq!((a, b), (9.2, 20.8));
    }
}
