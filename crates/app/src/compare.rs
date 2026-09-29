//! Compare one channel with every other charted one (Phase 3, C11: the open-signal
//! explorer). Over the stretch the charts show, live or in a recording under review,
//! each other channel is paired with it tick by tick (the XY plot's rule: a partner
//! within 1 ms or no pair) and fitted with the least-squares line; the list is ranked by
//! how closely the two follow a straight line (|r|). That is how most of the catalogue's
//! signals were identified: an unknown against the rulers (joint angles, speeds,
//! torques), looking for the one it is a straight line of. One click puts a pair in the
//! XY plot, to look at before believing a number.
//!
//! Computed once when asked (and again on "Compare again"), not every frame: a live
//! ten-minute view pairs a hundred thousand samples per channel.

use std::time::SystemTime;

use eframe::egui::{self, RichText};

use spy_core::derived::same_ticks;

use crate::app::SpyApp;
use crate::theme;
use crate::view::{self, Health};
use crate::xy::{fit, Candidate, Fit, XyState};

/// Fewer pairs than this and r says nothing: two points always lie on a line.
pub(crate) const MIN_PAIRS: usize = 10;

/// One other channel against the subject.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Row {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) units: String,
    pub(crate) pairs: usize,
    /// The subject as a line of this channel: `subject = slope x this + offset`.
    pub(crate) fit: Option<Fit>,
    /// How many samples it had in the stretch.
    pub(crate) samples: usize,
    pub(crate) health: Option<Health>,
}

impl Row {
    /// The correlation, where it means something: enough pairs, and both change.
    pub(crate) fn r(&self) -> Option<f64> {
        if self.pairs < MIN_PAIRS {
            return None;
        }
        self.fit.and_then(|f| f.r)
    }
}

/// What a comparison found, and over what.
pub(crate) struct Compared {
    pub(crate) subject: String,
    pub(crate) subject_title: String,
    pub(crate) subject_units: String,
    pub(crate) subject_samples: usize,
    pub(crate) from: i64,
    pub(crate) to: i64,
    pub(crate) rows: Vec<Row>,
    pub(crate) at: SystemTime,
    pub(crate) reviewing: bool,
}

/// The compare window: which channel, and the last comparison.
pub struct CompareState {
    pub subject: String,
    pub(crate) result: Option<Compared>,
    /// Compare (again) at the next frame.
    pub(crate) pending: bool,
}

impl CompareState {
    pub fn new(subject: String) -> CompareState {
        CompareState { subject, result: None, pending: true }
    }
}

/// Rank the rows: those with a correlation first, from the largest |r| down; then the
/// rest (too few pairs, or a channel that does not change), in the order they came.
pub(crate) fn rank(rows: &mut [Row]) {
    // A stable sort keeps the charted order among equals and among the unranked.
    rows.sort_by(|a, b| match (a.r(), b.r()) {
        (Some(x), Some(y)) => y.abs().total_cmp(&x.abs()),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
}

/// The subject's samples against another channel's over the same stretch.
pub(crate) fn row(subject: &[(i64, f64)], other: &Candidate, from: i64, to: i64) -> Row {
    let xs = other.values(from, to);
    let mut pairs = Vec::new();
    same_ticks(&[&xs, subject], |t, v| pairs.push((t, v[0], v[1])));
    Row { id: other.id.clone(), title: other.title.clone(), units: other.units.clone(), pairs: pairs.len(), fit: fit(&pairs), samples: xs.len(), health: other.health() }
}

/// What a row says about the pair, in words.
pub(crate) fn verdict(row: &Row, subject_samples: usize) -> String {
    if subject_samples == 0 {
        return "no samples of it in view".into();
    }
    if row.samples == 0 {
        return "no samples in view".into();
    }
    if row.pairs == 0 {
        return "never on the same ticks".into();
    }
    if row.pairs < MIN_PAIRS {
        return format!("only {} pairs", row.pairs);
    }
    match row.fit {
        None => "constant in view".into(),
        Some(Fit { r: None, .. }) => "the channel compared is constant in view".into(),
        Some(Fit { r: Some(r), .. }) => format!("r = {r:.4}"),
    }
}

/// The local time of day, `14:03:07` (UTC, said so, where local time is unknown).
fn clock(t: SystemTime) -> String {
    match spy_core::util::local_parts(t) {
        Some((_, _, _, h, mi, s, _)) => format!("{h:02}:{mi:02}:{s:02}"),
        None => format!("{} UTC", &spy_core::util::wall_iso(t)[11..19]),
    }
}

impl SpyApp {
    /// Open the compare window for a channel (by id), comparing at once.
    pub fn open_compare(&mut self, subject: String) {
        self.compare = Some(CompareState::new(subject));
    }

    /// The charted channels of a signal number, by id, and how many others are charted.
    pub(crate) fn charted_ids_of(&self, signal: u32) -> (Vec<String>, usize) {
        let prefix = format!("{signal}/");
        let (mine, others): (Vec<String>, Vec<String>) = self.xy_sources().0.into_iter().map(|c| c.id).partition(|id| id.starts_with(&prefix));
        (mine, others.len())
    }

    fn run_compare(&mut self, st: &mut CompareState) -> Result<(), String> {
        let (cands, stretch, _) = self.xy_sources();
        let Some(subject) = cands.iter().find(|c| c.id == st.subject) else { return Err("That channel is no longer charted.".into()) };
        let Some((from, to)) = stretch else { return Err("Nothing charted yet.".into()) };
        let ys = subject.values(from, to);
        let mut rows: Vec<Row> = cands.iter().filter(|c| c.id != subject.id).map(|c| row(&ys, c, from, to)).collect();
        rank(&mut rows);
        st.result = Some(Compared {
            subject: subject.id.clone(),
            subject_title: subject.title.clone(),
            subject_units: subject.units.clone(),
            subject_samples: ys.len(),
            from,
            to,
            rows,
            at: SystemTime::now(),
            reviewing: self.review.is_some(),
        });
        Ok(())
    }

    pub fn compare_window(&mut self, ctx: &egui::Context) {
        let Some(mut st) = self.compare.take() else { return };
        let cands: Vec<(String, String)> = self.xy_sources().0.into_iter().map(|c| (c.id, c.title)).collect();
        if st.pending {
            st.pending = false;
            if let Err(e) = self.run_compare(&mut st) {
                st.result = None;
                self.toast(spy_core::log::Level::Warn, e);
            }
        }
        let mut open = true;
        let mut plot: Option<(String, String)> = None;
        egui::Window::new("Compare").id(egui::Id::new("compare-window")).open(&mut open).default_size([640.0, 420.0]).resizable(true).show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Channel").strong());
                let text = cands.iter().find(|(id, _)| *id == st.subject).map_or_else(|| "choose a channel".to_string(), |(_, t)| t.clone());
                egui::ComboBox::from_id_salt("compare-subject").selected_text(text).width(220.0).show_ui(ui, |ui| {
                    for (id, title) in &cands {
                        if ui.selectable_label(st.subject == *id, title).clicked() {
                            st.subject = id.clone();
                            st.pending = true;
                        }
                    }
                });
                if ui.button("Compare again").on_hover_text("Pair it again with every other charted channel, over the stretch the charts show now").clicked() {
                    st.pending = true;
                }
            });
            ui.label(
                RichText::new("Each other charted channel against it, sample by sample at the same controller tick, over the stretch the charts show. r near +1 or -1: it is a straight line of that channel there (the line is shown); near 0: no straight-line relation. Look at the pair in the XY plot before trusting a number.")
                    .small()
                    .weak(),
            );
            let Some(res) = &st.result else {
                ui.label(RichText::new(if cands.len() < 2 { "Chart at least two channels to compare one with the others." } else { "Nothing compared yet." }).weak());
                return;
            };
            let when = clock(res.at);
            let secs = (res.to - res.from) as f64 / 1000.0;
            ui.label(format!(
                "{} over the {secs:.1} s in view{}, compared at {when}: {} samples of it.",
                res.subject_title,
                if res.reviewing { " (a recording under review)" } else { "" },
                res.subject_samples
            ));
            if res.rows.is_empty() {
                ui.label(RichText::new("No other channel is charted.").weak());
                return;
            }
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                egui::Grid::new("compare-rows").striped(true).num_columns(5).show(ui, |ui| {
                    for h in ["Channel (this)", "pairs", "", &format!("{} =", res.subject_title), ""] {
                        ui.label(RichText::new(h).strong());
                    }
                    ui.end_row();
                    for r in &res.rows {
                        let stale = r.health.is_some_and(|h| h != Health::Live);
                        let mut name = RichText::new(&r.title);
                        if stale {
                            name = name.color(theme::WARN);
                        }
                        let name = ui.label(name);
                        if let Some(h) = r.health.filter(|_| stale) {
                            name.on_hover_text(format!("{}: its samples in view are the last received.", h.word()));
                        }
                        ui.label(r.pairs.to_string());
                        let v = verdict(r, res.subject_samples);
                        let strong = r.r().is_some_and(|x| x.abs() >= 0.99);
                        ui.label(if strong { RichText::new(v).strong() } else { RichText::new(v) });
                        match (r.r(), r.fit) {
                            (Some(_), Some(f)) => {
                                let sign = if f.offset < 0.0 { "−" } else { "+" };
                                ui.label(RichText::new(format!("{} × this {sign} {} {}", view::fmt(f.slope), view::fmt(f.offset.abs()), res.subject_units)).monospace());
                            }
                            _ => {
                                ui.label("");
                            }
                        }
                        if ui.add_enabled(r.pairs > 0, egui::Button::new("Show in XY")).on_hover_text(format!("Plot {} (Y) against {} (X)", res.subject_title, r.title)).clicked() {
                            plot = Some((r.id.clone(), res.subject.clone()));
                        }
                        ui.end_row();
                    }
                });
            });
        });
        self.compare = open.then_some(st);
        if let Some((x, y)) = plot {
            self.xy = Some(XyState { x: Some(x), y: Some(y), ..XyState::default() });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row_of(id: &str, pairs: usize, r: Option<f64>) -> Row {
        Row { id: id.into(), title: id.into(), units: String::new(), pairs, fit: Some(Fit { slope: 1.0, offset: 0.0, r }), samples: 500, health: None }
    }

    #[test]
    fn the_closest_straight_line_comes_first_whatever_its_sign() {
        let mut rows = vec![row_of("weak", 500, Some(0.3)), row_of("few", 5, Some(1.0)), row_of("flat", 500, None), row_of("anti", 500, Some(-0.999)), row_of("near", 500, Some(0.98))];
        rank(&mut rows);
        let order: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(order, ["anti", "near", "weak", "few", "flat"], "by |r|, then the unranked in the order they came");
    }

    #[test]
    fn a_handful_of_pairs_says_nothing() {
        // Two points always lie on a line: r = 1 from a handful of pairs is no evidence.
        assert_eq!(row_of("x", MIN_PAIRS - 1, Some(1.0)).r(), None);
        assert_eq!(row_of("x", MIN_PAIRS, Some(1.0)).r(), Some(1.0));
        assert_eq!(verdict(&row_of("x", 3, Some(1.0)), 100), "only 3 pairs");
        assert_eq!(verdict(&row_of("x", 0, None), 100), "never on the same ticks");
        let none = Row { samples: 0, ..row_of("x", 0, None) };
        assert_eq!(verdict(&none, 100), "no samples in view");
        assert_eq!(verdict(&row_of("x", 50, Some(0.5)), 0), "no samples of it in view");
        assert_eq!(verdict(&row_of("x", 50, None), 100), "the channel compared is constant in view");
        assert_eq!(verdict(&Row { fit: None, ..row_of("x", 50, None) }, 100), "constant in view");
        assert_eq!(verdict(&row_of("x", 50, Some(-0.12345)), 100), "r = -0.1235");
    }
}
