use eframe::egui::{self, vec2, RichText, Stroke, Vec2};

use spy_core::session::Status;

use crate::app::SpyApp;
use crate::theme;
use crate::view::{self, Health};

const GAP: f32 = 12.0;
const PAD: Vec2 = vec2(16.0, 12.0);
const HEAD_H: f32 = 28.0;
const FOOT_H: f32 = 48.0;
const MIN_NUMBER: f32 = 20.0;
const MIN_TILE_H: f32 = 2.0 * PAD.y + HEAD_H + FOOT_H + 2.0 * GAP + 1.5 * MIN_NUMBER;
const UNITS_SHARE: f32 = 0.4;
const UNITS_MIN: f32 = 16.0;
const PROBE: f32 = 100.0;
const FIT_MARGIN: f32 = 0.97;

struct Tile {
    name: String,
    color: egui::Color32,
    health: Health,
    value: String,
    units: String,
    range: Option<String>,
    extremes: Vec<String>,
    typical: String,
    age: Option<String>,
    on_target: bool,
}

fn typical_value(decimals: Option<usize>) -> String {
    match decimals {
        None => "-000.000".into(),
        Some(0) => "-000".into(),
        Some(n) => format!("-000.{}", "0".repeat(n)),
    }
}

pub fn grid(n: usize, area: Vec2, number: impl Fn(Vec2) -> f32) -> (usize, Vec2) {
    let n = n.max(1);
    (1..=n)
        .rev()
        .map(|cols| {
            let rows = n.div_ceil(cols);
            let tile = vec2(((area.x - GAP * (cols - 1) as f32) / cols as f32).floor(), ((area.y - GAP * (rows - 1) as f32) / rows as f32).floor());
            (cols, tile, number(tile))
        })
        .max_by(|a, b| a.2.total_cmp(&b.2))
        .map_or((1, area), |(cols, tile, _)| (cols, tile))
}

fn number_room(tile: Vec2) -> Vec2 {
    vec2(tile.x - 2.0 * PAD.x, tile.y - 2.0 * PAD.y - HEAD_H - FOOT_H - 2.0 * GAP)
}

fn size_per_px(ui: &egui::Ui, text: RichText) -> Vec2 {
    egui::WidgetText::from(text).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Body).size() / PROBE
}

fn units_text(units: &str, number: f32) -> RichText {
    RichText::new(units).size((number * UNITS_SHARE).max(UNITS_MIN))
}

fn number_size(ui: &egui::Ui, values: &[&str], units: &str, room: Vec2) -> f32 {
    let value = values.iter().map(|v| size_per_px(ui, theme::num(*v, PROBE))).fold(Vec2::ZERO, Vec2::max);
    let (units_w, gap) = if units.is_empty() { (0.0, 0.0) } else { (size_per_px(ui, RichText::new(units).size(PROBE)).x, GAP) };
    let width = room.x * FIT_MARGIN - gap;
    let by_width = width / (value.x + UNITS_SHARE * units_w);
    let by_width = if by_width * UNITS_SHARE < UNITS_MIN { (width - UNITS_MIN * units_w) / value.x } else { by_width };
    by_width.min(room.y / value.y)
}

impl SpyApp {
    fn tiles(&self, st: &Status) -> Vec<Tile> {
        let connected = view::session_live(&st.phase);
        let mut out = Vec::new();
        for c in &self.chans {
            let sig = self.catalogue.get(c.key.signal);
            let cs = st.channels.iter().find(|s| s.key == c.key);
            let h = view::health(cs, connected, sig, st.loopback);
            let d = view::display(sig, c.radians).with_decimals(c.decimals);
            let reading = view::reading(sig);
            let (value, text) = match self.session.store().get(&c.key) {
                Some(ch) => {
                    let r = ch.lock();
                    if r.kind == Some(spy_core::sample::ValueKind::String) { (r.last_text.clone(), true) } else { (view::readout_ms(&r, reading, view::readout_window(c.smooth_ms)).map(|v| d.fmt(v)), false) }
                }
                None => (None, false),
            };
            let f = |x: f64| d.fmt(x);
            out.push(Tile {
                name: view::short_label(&self.catalogue, &c.key),
                color: c.color,
                health: h,
                value: value.unwrap_or_else(|| "--".into()),
                units: if text { String::new() } else { d.units.clone() },
                range: (!text && c.stats.n > 0).then(|| format!("{} to {}", f(c.stats.min), f(c.stats.max))),
                extremes: if !text && c.stats.n > 0 { vec![f(c.stats.min), f(c.stats.max)] } else { Vec::new() },
                typical: typical_value(d.decimals),
                age: cs.map(|c| crate::channels::age_text(c.last_arrival)),
                on_target: false,
            });
        }
        for i in 0..self.derived.len() {
            let h = self.derived_health(i, st);
            let d = &self.derived[i];
            let def = d.live.def();
            let decimals = crate::derived_view::decimals(def);
            let v = view::readout(&d.live.lock(), crate::derived_view::reading(def));
            let (value, on_target) = crate::derived_view::value_text(def, v, h.is_live());
            out.push(Tile {
                name: self.derived_label(def),
                color: d.color,
                health: h,
                value,
                units: if on_target { String::new() } else { def.units().to_string() },
                range: (d.stats.n > 0).then(|| format!("{} to {}", view::fmt_to(d.stats.min, decimals), view::fmt_to(d.stats.max, decimals))),
                extremes: if d.stats.n > 0 { vec![view::fmt_to(d.stats.min, decimals), view::fmt_to(d.stats.max, decimals)] } else { Vec::new() },
                typical: typical_value(decimals),
                age: None,
                on_target,
            });
        }
        out
    }

    pub fn dashboard_ui(&mut self, ui: &mut egui::Ui) {
        let p = theme::pal(ui);
        let st = self.session.status().clone();
        theme::section(ui, "02", "live dashboard", false, |ui| {
            self.status_line(ui);
            ui.add(egui::Label::new(RichText::new("each number is the mean of its last 150 ms, or of its smoothing if longer").color(p.ink2)).truncate());
        });
        let tiles = self.tiles(&st);
        if tiles.is_empty() {
            ui.centered_and_justified(|ui| ui.label(RichText::new("Add channels to see their numbers here.").color(p.ink2)));
            return;
        }
        let (cols, size) = grid(tiles.len(), ui.available_size(), |tile| {
            let room = number_room(tile);
            tiles.iter().map(|t| number_size(ui, &[&t.typical], &t.units, room)).fold(f32::INFINITY, f32::min)
        });
        let size = vec2(size.x, size.y.max(MIN_TILE_H));
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(GAP, GAP);
            for row in tiles.chunks(cols) {
                ui.horizontal(|ui| {
                    for t in row {
                        tile(ui, t, size);
                    }
                });
            }
        });
    }
}

fn tile(ui: &mut egui::Ui, t: &Tile, size: Vec2) {
    let p = theme::pal(ui);
    let stale = t.health == Health::Stale;
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    if stale {
        ui.painter().rect(rect, 0.0, p.stale_face, Stroke::new(2.0, p.stale_edge), egui::StrokeKind::Inside);
    } else {
        ui.painter().rect_stroke(rect, 0.0, Stroke::new(1.0, p.line), egui::StrokeKind::Inside);
    }
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(PAD)).layout(egui::Layout::top_down(egui::Align::Min)));
    let ui = &mut child;
    ui.allocate_ui_with_layout(vec2(ui.available_width(), HEAD_H), egui::Layout::right_to_left(egui::Align::Center), |ui| {
        crate::channels::status_word(ui, t.health);
        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            theme::square(ui, t.color, 16.0);
            ui.add(egui::Label::new(theme::b(&t.name).size(20.0)).truncate());
        });
    });
    let values: Vec<&str> = std::iter::once(t.value.as_str()).chain(t.extremes.iter().map(String::as_str)).collect();
    let number = number_size(ui, &values, &t.units, number_room(size)).floor().max(MIN_NUMBER);
    ui.horizontal(|ui| {
        let mut v = theme::num(&t.value, number);
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
            ui.label(units_text(&t.units, number).color(p.ink2));
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

    fn eight_characters_wide(tile: Vec2) -> f32 {
        (tile.x / 5.5).min((tile.y - 124.0) / 1.3)
    }

    #[test]
    fn the_grid_gives_the_numbers_the_most_room() {
        assert_eq!(grid(6, vec2(1340.0, 520.0), eight_characters_wide).0, 3, "a wide window: three across");
        assert_eq!(grid(6, vec2(1340.0, 805.0), eight_characters_wide).0, 2, "a taller one: two across, bigger numbers");
        assert_eq!(grid(1, vec2(1340.0, 805.0), eight_characters_wide), (1, vec2(1340.0, 805.0)), "one number has it all");
        assert_eq!(grid(0, vec2(1340.0, 805.0), eight_characters_wide).0, 1, "no tiles is never no columns");
        let (cols, tile) = grid(5, vec2(1340.0, 520.0), eight_characters_wide);
        assert!(tile.x * cols as f32 + GAP * (cols - 1) as f32 <= 1340.0, "the tiles and their gaps fit across");
    }
}
