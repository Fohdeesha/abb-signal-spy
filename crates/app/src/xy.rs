//! The XY plot (Phase 3, C6): one channel against another over the stretch the charts
//! show, live or in a recording under review, so pausing, scrolling or zooming the
//! charts picks the stretch. A point is a pair of samples of the same controller tick
//! (the derived channels' rule, [`same_ticks`]: a partner within 1 ms or no point,
//! never interpolated). Every pair is drawn, thinned only where two land on the same
//! pixel, so an outlier shows like any other point. Beside it: how many pairs, their
//! correlation and the least-squares line, the way most of the catalogue's signals
//! were identified (a resolver angle against the motor angle: slope 0.9999 to 1.0001).

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};
use egui_plot::{HoverPosition, Line, Plot, PlotPoints, Points};

use spy_core::derived::same_ticks;
use spy_core::review::Review;

use crate::app::SpyApp;
use crate::charts::{autoscale, min_span, Member};
use crate::export::Picture;
use crate::theme;
use crate::view::{self, Health};

const POINT: egui::Color32 = egui::Color32::from_rgb(0x5A, 0x9B, 0xD5);

/// The XY window: which channels, by id (a live channel and its recording share one,
/// so a choice carries over to a review), and the pairs last computed.
#[derive(Default)]
pub struct XyState {
    pub x: Option<String>,
    pub y: Option<String>,
    pub(crate) cache: Option<(Key, Pairs)>,
    /// The person dragged or zoomed the plot (G27): it keeps their view until a
    /// double-click, or other channels, bring back the whole stretch.
    pub(crate) zoomed: bool,
    /// The ranges of x and y the plot showed last, which its points were thinned for.
    pub(crate) shown: Option<((f64, f64), (f64, f64))>,
}

/// What pairs were computed from: the channels, the stretch, and the data then.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Key {
    pub(crate) x: String,
    pub(crate) y: String,
    from: i64,
    to: i64,
    data: Data,
}

/// The state of the data: live, the newest sample and the history's epoch; a review,
/// which recording (it never changes once open).
#[derive(Debug, Clone, Copy, PartialEq)]
enum Data {
    Live(Option<i64>, u64),
    Review(usize, i64),
}

pub(crate) struct Pairs {
    /// (time, x, y) of each tick both have a sample of, in the display units.
    pub(crate) points: Vec<(i64, f64, f64)>,
    pub(crate) fit: Option<Fit>,
    /// How many samples each had in the stretch.
    pub(crate) counts: (usize, usize),
    /// The smallest and largest x, and y.
    extent: Option<((f64, f64), (f64, f64))>,
    /// When they were paired, and how long that took.
    at: Instant,
    took: Duration,
}

/// The plot spends at most about one part in this of its time pairing: a live view of
/// ten minutes (150,000 pairs, measured: 73 % of a core with the plot open against 41 %
/// without, pairing every frame) moves on a few times a second instead of every frame.
const PAIRING_SHARE: u32 = 10;

/// Whether to pair again for `key`: at once for other channels; for a moved stretch
/// or new data, once the last pairing's cost has been paid back [`PAIRING_SHARE`] times.
fn pair_again(cache: Option<&(Key, Pairs)>, key: &Key) -> bool {
    match cache {
        None => true,
        Some((k, p)) => k.x != key.x || k.y != key.y || (k != key && p.at.elapsed() >= p.took * PAIRING_SHARE),
    }
}

/// The least-squares line `y = slope x + offset` and the correlation, where y changes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fit {
    pub slope: f64,
    pub offset: f64,
    pub r: Option<f64>,
}

/// The least-squares line through the finite pairs and their correlation. Nothing for
/// fewer than two, or where x does not change (no line then).
pub fn fit(pairs: &[(i64, f64, f64)]) -> Option<Fit> {
    let pts = || pairs.iter().filter(|p| p.1.is_finite() && p.2.is_finite());
    let n = pts().count();
    if n < 2 {
        return None;
    }
    let (sx, sy) = pts().fold((0.0, 0.0), |(a, b), p| (a + p.1, b + p.2));
    let (mx, my) = (sx / n as f64, sy / n as f64);
    let (sxx, syy, sxy) = pts().fold((0.0, 0.0, 0.0), |(a, b, c), p| {
        let (dx, dy) = (p.1 - mx, p.2 - my);
        (a + dx * dx, b + dy * dy, c + dx * dy)
    });
    if sxx <= 0.0 {
        return None;
    }
    let slope = sxy / sxx;
    Some(Fit { slope, offset: my - slope * mx, r: (syy > 0.0).then(|| (sxy / (sxx * syy).sqrt()).clamp(-1.0, 1.0)) })
}

/// The points to draw of `pairs` inside the ranges `x` and `y`: one per cell of a `w`
/// by `h` grid (a pixel each), so a stretch of a hundred thousand pairs draws as fast as
/// the pixels it covers, and every cell holding a pair shows one: nothing is thinned
/// away that would show. Pairs outside the ranges, or not finite, are left out.
pub fn thin(pairs: &[(i64, f64, f64)], x: (f64, f64), y: (f64, f64), w: usize, h: usize) -> Vec<[f64; 2]> {
    let (w, h) = (w.max(1), h.max(1));
    let (kx, ky) = (w as f64 / (x.1 - x.0), h as f64 / (y.1 - y.0));
    let mut seen = vec![0u64; (w * h).div_ceil(64)];
    let mut out = Vec::new();
    for &(_, px, py) in pairs {
        // Also leaves out NaN, which compares false.
        if !(px >= x.0 && px <= x.1 && py >= y.0 && py <= y.1) {
            continue;
        }
        let cell = (((py - y.0) * ky) as usize).min(h - 1) * w + (((px - x.0) * kx) as usize).min(w - 1);
        let (word, bit) = (cell / 64, 1u64 << (cell % 64));
        if seen[word] & bit == 0 {
            seen[word] |= bit;
            out.push([px, py]);
        }
    }
    out
}

/// A channel the plot can take.
struct Candidate {
    id: String,
    title: String,
    units: String,
    source: Source,
}

enum Source {
    Live(Member),
    /// A recorded channel (by index) and its display factor.
    Recorded(Arc<Review>, usize, f64),
}

impl Candidate {
    /// Its samples over `[from, to)` in its display unit.
    fn values(&self, from: i64, to: i64) -> Vec<(i64, f64)> {
        match &self.source {
            Source::Live(m) => m.values(from, to),
            Source::Recorded(r, i, factor) => r.channels[*i].range(from, to).map(|(t, v)| (t, v * factor)).collect(),
        }
    }

    fn health(&self) -> Option<Health> {
        match &self.source {
            Source::Live(m) => Some(m.health),
            Source::Recorded(..) => None,
        }
    }
}

impl SpyApp {
    /// What the plot can show: the channels charted, the stretch in view, and the
    /// state of the data. A recording under review has the charts, so it has the plot.
    fn xy_sources(&self) -> (Vec<Candidate>, Option<(i64, i64)>, Data) {
        if let Some(rs) = &self.review {
            let r = rs.review.clone();
            let cands = r
                .channels
                .iter()
                .enumerate()
                .filter(|(_, ch)| !ch.v.is_empty())
                .map(|(i, ch)| {
                    let (units, factor) = crate::review_view::display_of(ch);
                    Candidate { id: ch.id.clone(), title: crate::review_view::name_of(&self.catalogue, ch), units, source: Source::Recorded(r.clone(), i, factor) }
                })
                .collect();
            return (cands, Some(rs.stretch()), Data::Review(Arc::as_ptr(&r) as usize, r.end));
        }
        let st = self.session.status().clone();
        let charted: Vec<bool> = (0..self.chans.len()).map(|i| self.charted(i, &st)).collect();
        let cands = self.members(&charted, &st).into_iter().map(|m| Candidate { id: m.id.clone(), title: m.title.clone(), units: m.lane.1.clone(), source: Source::Live(m) }).collect();
        let store = self.session.store();
        (cands, self.view_ms, Data::Live(store.newest(), store.epoch()))
    }

    pub fn toggle_xy(&mut self) {
        self.xy = match self.xy {
            Some(_) => None,
            None => Some(XyState::default()),
        };
    }

    /// The XY window, after the charts have set the stretch in view this frame.
    pub fn xy_window(&mut self, ctx: &egui::Context) {
        let Some(mut xy) = self.xy.take() else { return };
        let (cands, stretch, data) = self.xy_sources();
        let find = |id: &Option<String>| id.as_ref().and_then(|id| cands.iter().find(|c| &c.id == id));
        // The first two charted, until the person chooses.
        if xy.x.is_none() && xy.y.is_none() && cands.len() >= 2 {
            xy.x = Some(cands[0].id.clone());
            xy.y = Some(cands[1].id.clone());
        }
        let reviewing = self.review.is_some();
        let streaming = self.session.status().phase == spy_core::session::Phase::Streaming;
        let mut open = true;
        let (mut png, mut rect) = (false, None);
        let title = if reviewing { "XY plot · reviewing, not live" } else { "XY plot" };
        egui::Window::new(title).id(egui::Id::new("xy-window")).open(&mut open).default_size([560.0, 520.0]).resizable(true).show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                for (axis, choice) in [("X", &mut xy.x), ("Y", &mut xy.y)] {
                    ui.label(RichText::new(axis).strong());
                    let text = find(choice).map_or_else(|| "choose a channel".to_string(), |c| c.title.clone());
                    egui::ComboBox::from_id_salt(("xy-axis", axis)).selected_text(text).width(200.0).show_ui(ui, |ui| {
                        for c in &cands {
                            ui.selectable_value(choice, Some(c.id.clone()), &c.title);
                        }
                    });
                }
                if ui.button("⇄").on_hover_text("Swap X and Y").clicked() {
                    std::mem::swap(&mut xy.x, &mut xy.y);
                }
                if ui.button("Save PNG").on_hover_text("Save a picture of the plot to the recordings folder").clicked() {
                    png = true;
                }
                if xy.zoomed {
                    ui.label(RichText::new("Zoomed: double-click the plot for the whole stretch.").weak()).on_hover_text("The numbers below are of every pair in the stretch, not only those in view.");
                }
            });
            let (Some(cx), Some(cy)) = (find(&xy.x), find(&xy.y)) else {
                ui.label(RichText::new(if cands.len() < 2 { "Chart at least two channels to plot one against the other." } else { "Choose a channel for X and one for Y." }).weak());
                return;
            };
            let Some((from, to)) = stretch else {
                ui.label(RichText::new("Nothing charted yet.").weak());
                return;
            };
            // Other channels than those last paired (or the window just opened): the
            // whole stretch of them, not the view of the last ones.
            let chosen_again = xy.cache.as_ref().is_none_or(|(k, _)| k.x != cx.id || k.y != cy.id);
            let key = Key { x: cx.id.clone(), y: cy.id.clone(), from, to, data };
            if pair_again(xy.cache.as_ref(), &key) {
                let at = Instant::now();
                let (xs, ys) = (cx.values(from, to), cy.values(from, to));
                let mut points = Vec::new();
                same_ticks(&[&xs, &ys], |t, v| points.push((t, v[0], v[1])));
                let fit = fit(&points);
                let extent = extent(points.iter().map(|q| q.1)).zip(extent(points.iter().map(|q| q.2)));
                xy.cache = Some((key, Pairs { points, fit, extent, counts: (xs.len(), ys.len()), at, took: at.elapsed() }));
            }
            let Some((k, p)) = &xy.cache else { return };
            for c in [cx, cy] {
                if let Some(h) = c.health()
                    && (!streaming || h != Health::Live)
                {
                    ui.colored_label(theme::WARN, format!("{} is {}: the plot shows the last pairs received.", c.title, if streaming { h.word() } else { "not streaming" }));
                }
            }
            ui.label(pairs_text(p, cx, cy, (k.to - k.from) as f64 / 1000.0));
            let Some((xr, yr)) = p.extent else { return };
            let (xb, yb) = (autoscale(xr.0, xr.1, min_span(&cx.units)), autoscale(yr.0, yr.1, min_span(&cy.units)));
            let (xu, yu) = (cx.units.clone(), cy.units.clone());
            // egui_plot fits its bounds to the whole stretch (the items drawn never reach
            // past it) until the person drags or zooms; a double-click fits them again.
            let resp = Plot::new("xy-plot")
                .x_axis_label(format!("{}  [{}]", cx.title, cx.units))
                .y_axis_label(format!("{}  [{}]", cy.title, cy.units))
                .y_axis_min_width(56.0)
                .include_x(xb.0)
                .include_x(xb.1)
                .include_y(yb.0)
                .include_y(yb.1)
                .set_margin_fraction(egui::Vec2::ZERO)
                .allow_boxed_zoom(false)
                .allow_double_click_reset(true)
                .label_formatter(move |pos| {
                    let p = match pos {
                        HoverPosition::NearDataPoint { position, .. } | HoverPosition::Elsewhere { position } => *position,
                    };
                    Some(format!("X {} {xu}\nY {} {yu}", view::fmt(p.x), view::fmt(p.y)))
                })
                .show(ui, |pu| {
                    if chosen_again {
                        pu.set_auto_bounds(true);
                    }
                    let auto = pu.auto_bounds();
                    let whole = chosen_again || (auto.x && auto.y);
                    let (vx, vy) = if whole {
                        (xb, yb)
                    } else {
                        let b = pu.plot_bounds();
                        ((b.min()[0], b.max()[0]), (b.min()[1], b.max()[1]))
                    };
                    let size = pu.response().rect.size();
                    let pts = thin(&p.points, vx, vy, size.x as usize, size.y as usize);
                    pu.points(Points::new("pairs", PlotPoints::from(pts)).radius(1.5).color(POINT));
                    if let Some(line) = p.fit.and_then(|f| line_in(f, vx, vy)) {
                        pu.line(Line::new("least-squares line", PlotPoints::from(line.to_vec())).color(theme::IDLE).style(egui_plot::LineStyle::dashed_loose()));
                    }
                    // Where it is now.
                    if !reviewing && let Some(&(_, x, y)) = p.points.last() {
                        pu.points(Points::new("newest", PlotPoints::from(vec![[x, y]])).radius(4.5).color(theme::WARN));
                    }
                    (!whole, (vx, vy))
                });
            (xy.zoomed, xy.shown) = (resp.inner.0, Some(resp.inner.1));
            rect = Some(resp.response.rect);
        });
        self.xy_rect = rect;
        self.xy = open.then_some(xy);
        if png {
            self.request_png(Picture::Xy);
        }
    }
}

/// The part of the line `y = slope x + offset` inside the ranges `x` by `y`: drawn
/// past them, it would widen the bounds the plot fits to the pairs.
pub fn line_in(f: Fit, x: (f64, f64), y: (f64, f64)) -> Option<[[f64; 2]; 2]> {
    let (mut x0, mut x1) = x;
    if f.slope != 0.0 {
        let (a, b) = ((y.0 - f.offset) / f.slope, (y.1 - f.offset) / f.slope);
        x0 = x0.max(a.min(b));
        x1 = x1.min(a.max(b));
    } else if !(y.0..=y.1).contains(&f.offset) {
        return None;
    }
    (x0 < x1).then_some([[x0, f.slope * x0 + f.offset], [x1, f.slope * x1 + f.offset]])
}

fn extent(v: impl Iterator<Item = f64>) -> Option<(f64, f64)> {
    let (lo, hi) = v.filter(|x| x.is_finite()).fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), x| (a.min(x), b.max(x)));
    (lo <= hi).then_some((lo, hi))
}

/// How many pairs, and what follows from them; or why there are none.
fn pairs_text(p: &Pairs, x: &Candidate, y: &Candidate, secs: f64) -> String {
    let n = p.points.len();
    if n == 0 {
        return match p.counts {
            (0, _) => format!("{} has no samples in view.", x.title),
            (_, 0) => format!("{} has no samples in view.", y.title),
            _ => "No samples of the two at the same instant in view: they are not stamped on the same controller ticks.".to_string(),
        };
    }
    let head = format!("{n} pairs in the {secs:.1} s in view.");
    match p.fit {
        None if n < 2 => head,
        None => format!("{head} X does not change: no line."),
        Some(f) => {
            let r = f.r.map_or_else(|| "Y does not change".to_string(), |r| format!("r = {r:.4}"));
            let sign = if f.offset < 0.0 { "−" } else { "+" };
            format!("{head}   {r}   line: Y = {} × X {sign} {} {}", view::fmt(f.slope), view::fmt(f.offset.abs()), y.units)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_line_and_correlation_are_the_least_squares_ones() {
        let line: Vec<(i64, f64, f64)> = (0..100).map(|i| (i, i as f64 * 0.1, 2.0 * i as f64 * 0.1 + 3.0)).collect();
        let f = fit(&line).unwrap();
        assert!((f.slope - 2.0).abs() < 1e-12 && (f.offset - 3.0).abs() < 1e-12 && (f.r.unwrap() - 1.0).abs() < 1e-12, "{f:?}");
        let down: Vec<(i64, f64, f64)> = (0..10).map(|i| (i, i as f64, -0.5 * i as f64)).collect();
        assert!((fit(&down).unwrap().r.unwrap() + 1.0).abs() < 1e-12);
        // A known small case: (0,1) (1,3) (2,2): slope 0.5, offset 1.5, r 0.5.
        let f = fit(&[(0, 0.0, 1.0), (1, 1.0, 3.0), (2, 2.0, 2.0)]).unwrap();
        assert!((f.slope - 0.5).abs() < 1e-12 && (f.offset - 1.5).abs() < 1e-12 && (f.r.unwrap() - 0.5).abs() < 1e-12, "{f:?}");
        // A pair that is not a number is left out, not spread through the sums.
        let f = fit(&[(0, 0.0, 1.0), (1, f64::NAN, 5.0), (2, 1.0, 3.0), (3, 2.0, 2.0), (4, 3.0, f64::INFINITY)]).unwrap();
        assert!((f.slope - 0.5).abs() < 1e-12, "{f:?}");
        assert_eq!(fit(&[(0, 1.0, 2.0), (1, 1.0, 3.0)]), None, "x does not change: no line");
        assert_eq!(fit(&[(0, 1.0, 2.0), (1, 2.0, 2.0)]).unwrap().r, None, "y does not change: no correlation");
        assert_eq!(fit(&[(0, 1.0, 2.0)]), None);
    }

    #[test]
    fn a_moving_view_is_paired_again_once_its_last_pairing_is_paid_back() {
        let key = |x: &str, y: &str, to: i64| Key { x: x.into(), y: y.into(), from: 0, to, data: Data::Live(Some(to), 0) };
        let paired = |took_ms: u64, ago_ms: u64| Pairs {
            points: Vec::new(),
            fit: None,
            counts: (0, 0),
            extent: None,
            at: Instant::now().checked_sub(Duration::from_millis(ago_ms)).unwrap(),
            took: Duration::from_millis(took_ms),
        };
        let (x, y) = ("4001/ROB_1/J1", "4002/ROB_1/J1");
        let k = key(x, y, 1000);
        assert!(pair_again(None, &k), "nothing paired yet");
        let just = (k.clone(), paired(20, 0));
        assert!(!pair_again(Some(&just), &k), "nothing changed");
        assert!(!pair_again(Some(&just), &key(x, y, 1004)), "moved on 4 ms: the last pairing's 20 ms are not paid back yet");
        assert!(pair_again(Some(&just), &key("318/ROB_1/J1", y, 1000)) && pair_again(Some(&just), &key(x, "318/ROB_1/J1", 1000)), "other channels are paired at once");
        let older = (k.clone(), paired(20, 250));
        assert!(pair_again(Some(&older), &key(x, y, 1004)), "paid back ten times over by now");
        assert!(!pair_again(Some(&older), &k), "nothing changed, however long ago");
    }

    #[test]
    fn the_line_is_drawn_only_inside_the_plot() {
        let line = |slope, offset| Fit { slope, offset, r: Some(1.0) };
        let (x, y) = ((0.0, 10.0), (0.0, 10.0));
        assert_eq!(line_in(line(2.0, 3.0), x, y), Some([[0.0, 3.0], [3.5, 10.0]]), "cut where it leaves the top");
        assert_eq!(line_in(line(-2.0, 13.0), x, y), Some([[1.5, 10.0], [6.5, 0.0]]), "falling, cut at both");
        assert_eq!(line_in(line(0.0, 5.0), x, y), Some([[0.0, 5.0], [10.0, 5.0]]));
        assert_eq!(line_in(line(0.0, 50.0), x, y), None, "flat, above the plot");
        assert_eq!(line_in(line(1.0, 100.0), x, y), None, "passes the plot by");
    }

    #[test]
    fn thinning_keeps_one_point_in_every_pixel_that_has_one() {
        // A dense curve, and one outlier far from it.
        let mut pairs: Vec<(i64, f64, f64)> = (0..100_000).map(|i| (i, (i as f64 * 0.001).sin(), (i as f64 * 0.0013).cos())).collect();
        pairs.push((100_000, 0.9, -0.9));
        let (x, y, w, h) = ((-1.1, 1.1), (-1.1, 1.1), 400, 300);
        let cell = |p: [f64; 2]| ((((p[0] - x.0) * w as f64 / (x.1 - x.0)) as usize).min(w - 1), (((p[1] - y.0) * h as f64 / (y.1 - y.0)) as usize).min(h - 1));
        let all: std::collections::HashSet<_> = pairs.iter().map(|q| cell([q.1, q.2])).collect();
        let drawn = thin(&pairs, x, y, w, h);
        let cells: std::collections::HashSet<_> = drawn.iter().map(|&p| cell(p)).collect();
        assert_eq!(cells, all, "a pixel with a pair drew none, or one drew two");
        assert_eq!(drawn.len(), cells.len());
        assert!(drawn.len() < 20_000, "{} points drawn for a curve", drawn.len());
        assert!(drawn.contains(&[0.9, -0.9]), "the outlier was thinned away");
        // Outside the ranges, or not a number: not drawn.
        assert!(thin(&[(0, 5.0, 0.0), (1, 0.0, f64::NAN), (2, 0.0, 0.0)], x, y, w, h) == vec![[0.0, 0.0]]);
    }
}
