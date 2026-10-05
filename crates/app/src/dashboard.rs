//! The live dashboard (G47): every channel's value as a big number in place of the
//! charts, to read from a step away. The numbers are the rows' (a mean of at least
//! 150 ms, longer for a smoothed channel); one that is not live is marked, struck
//! through and aged, never shown as current.

use eframe::egui::{self, RichText, Stroke};

use spy_core::session::Status;

use crate::app::SpyApp;
use crate::settings::DASH_SIZES;
use crate::theme;
use crate::view::{self, Health};

/// One tile's contents.
struct Tile {
    name: String,
    color: egui::Color32,
    health: Health,
    value: String,
    units: String,
    /// Since reset: "349.1 to 358.0".
    range: Option<String>,
    /// The newest sample's age, for a stale one.
    age: Option<String>,
    on_target: bool,
}

/// Columns for `n` tiles: the fewest that keep them wide.
pub fn columns(n: usize) -> usize {
    match n {
        0..=3 => n.max(1),
        4 => 2,
        5 | 6 => 3,
        _ => 4,
    }
}

impl SpyApp {
    fn tiles(&self, st: &Status) -> Vec<Tile> {
        let connected = view::session_live(&st.phase);
        let mut out = Vec::new();
        for c in &self.chans {
            let sig = self.catalogue.get(c.key.signal);
            let cs = st.channels.iter().find(|s| s.key == c.key);
            let h = view::health(cs, connected, sig, st.loopback);
            let d = view::display(sig, c.radians);
            let reading = view::reading(sig);
            let (value, text) = match self.session.store().get(&c.key) {
                Some(ch) => {
                    let r = ch.lock();
                    if r.kind == Some(spy_core::sample::ValueKind::String) { (r.last_text.clone(), true) } else { (view::readout_ms(&r, reading, view::readout_window(c.smooth_ms)).map(|v| view::fmt(v * d.factor)), false) }
                }
                None => (None, false),
            };
            let f = |x: f64| view::fmt(x * d.factor);
            out.push(Tile {
                name: view::short_label(&self.catalogue, &c.key),
                color: c.color,
                health: h,
                value: value.unwrap_or_else(|| "--".into()),
                units: if text { String::new() } else { d.units.clone() },
                range: (!text && c.stats.n > 0).then(|| format!("{} to {}", f(c.stats.min), f(c.stats.max))),
                age: cs.map(|c| crate::channels::age_text(c.last_arrival)),
                on_target: false,
            });
        }
        for i in 0..self.derived.len() {
            let h = self.derived_health(i, st);
            let d = &self.derived[i];
            let def = d.live.def();
            let v = view::readout(&d.live.lock(), crate::derived_view::reading(def));
            let (value, on_target) = crate::derived_view::value_text(def, v, h.is_live());
            out.push(Tile {
                name: self.derived_label(def),
                color: d.color,
                health: h,
                value,
                units: if on_target { String::new() } else { def.units().to_string() },
                range: (d.stats.n > 0).then(|| format!("{} to {}", view::fmt(d.stats.min), view::fmt(d.stats.max))),
                age: None,
                on_target,
            });
        }
        out
    }

    pub fn dashboard_ui(&mut self, ui: &mut egui::Ui) {
        let p = theme::pal(ui);
        let st = self.session.status().clone();
        let size = self.settings.dash_size;
        let mut resize = None;
        theme::section(ui, "02", "live dashboard", false, |ui| {
            ui.label(RichText::new("each number is the mean of its last 150 ms, or of its smoothing if longer").color(p.ink2));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let i = DASH_SIZES.iter().position(|s| (s - size).abs() < 0.5).unwrap_or(2);
                if ui.add_enabled(i + 1 < DASH_SIZES.len(), egui::Button::new("bigger").min_size(egui::vec2(0.0, theme::TOOL_H))).clicked() {
                    resize = Some(DASH_SIZES[i + 1]);
                }
                if ui.add_enabled(i > 0, egui::Button::new("smaller").min_size(egui::vec2(0.0, theme::TOOL_H))).clicked() {
                    resize = Some(DASH_SIZES[i - 1]);
                }
                ui.label(theme::b("size").color(p.ink2));
            });
        });
        if let Some(s) = resize {
            self.settings.dash_size = s;
            self.mark_settings_dirty();
        }
        let tiles = self.tiles(&st);
        if tiles.is_empty() {
            ui.centered_and_justified(|ui| ui.label(RichText::new("Add channels to see their numbers here.").color(p.ink2)));
            return;
        }
        let cols = columns(tiles.len());
        let rows = tiles.len().div_ceil(cols);
        let gap = 12.0;
        let w = ((ui.available_width() - gap * (cols as f32 - 1.0)) / cols as f32).floor();
        let h = ((ui.available_height() - gap * (rows as f32 - 1.0)) / rows as f32).floor().max(size + 90.0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(gap, gap);
            for row in tiles.chunks(cols) {
                ui.horizontal(|ui| {
                    for t in row {
                        tile(ui, t, egui::vec2(w, h), size);
                    }
                });
            }
        });
    }
}

fn tile(ui: &mut egui::Ui, t: &Tile, size: egui::Vec2, number: f32) {
    let p = theme::pal(ui);
    let stale = t.health == Health::Stale;
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    if stale {
        ui.painter().rect(rect, 0.0, p.stale_face, Stroke::new(2.0, p.stale_edge), egui::StrokeKind::Inside);
    } else {
        ui.painter().rect_stroke(rect, 0.0, Stroke::new(1.0, p.line), egui::StrokeKind::Inside);
    }
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(egui::vec2(16.0, 12.0))).layout(egui::Layout::top_down(egui::Align::Min)));
    let ui = &mut child;
    // The status at the right first, so the name truncates before it.
    ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 28.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
        crate::channels::status_word(ui, t.health);
        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            theme::square(ui, t.color, 16.0);
            ui.add(egui::Label::new(theme::b(&t.name).size(20.0)).truncate());
        });
    });
    // As big as asked, or as fits the tile.
    let chars = t.value.chars().count().max(1) as f32 + (t.units.chars().count() as f32 * 0.5);
    let fit = (ui.available_width() * 0.95 / (chars * 0.62)).floor();
    let size = number.min(fit).max(20.0);
    ui.horizontal(|ui| {
        let mut v = theme::num(&t.value, size);
        if !t.health.is_live() {
            v = v.color(p.ink2);
            if stale {
                v = v.strikethrough();
            }
        } else if t.on_target {
            v = v.color(p.live);
        }
        ui.label(v);
        if !t.units.is_empty() {
            ui.label(RichText::new(&t.units).size((size * 0.4).max(16.0)).color(p.ink2));
        }
    });
    if stale {
        let age = t.age.clone().unwrap_or_else(|| "a while".into());
        ui.add(egui::Label::new(theme::b(format!("No sample for {age}: this is not the value now")).size(18.0).color(p.hold)).wrap());
    } else if let Some(r) = &t.range {
        ui.label(RichText::new(r).monospace().size(18.0).color(p.ink2)).on_hover_text("The lowest and highest since the last reset");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tiles_stay_wide() {
        assert_eq!([1, 2, 3, 4, 5, 6, 7, 8, 12].map(columns), [1, 2, 3, 2, 3, 3, 4, 4, 4]);
        assert_eq!(columns(0), 1, "no tiles is never no columns");
    }
}
