use eframe::egui::{self, vec2, RichText, Stroke, Vec2};

use spy_core::session::Status;

use crate::app::SpyApp;
use crate::theme;
use crate::view::{self, Health};

const GAP: f32 = 12.0;
const PAD: Vec2 = vec2(16.0, 12.0);
const HEAD_H: f32 = 28.0;
const FOOT_SIZE: f32 = 18.0;
const MIN_NUMBER: f32 = 20.0;
const UNITS_SHARE: f32 = 0.4;
const UNITS_MIN: f32 = 16.0;
const PROBE: f32 = 100.0;
const FIT_MARGIN: f32 = 0.97;
const AGE_PROBE: std::time::Duration = std::time::Duration::from_secs(100);

struct Tile {
    name: String,
    color: egui::Color32,
    health: Health,
    value: String,
    units: String,
    range: Option<String>,
    extremes: Vec<String>,
    typical: String,
    old: bool,
    note: Option<String>,
    foot_probe: Option<(String, bool)>,
    on_target: bool,
    also_fits: Vec<String>,
    sized_units: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fit {
    pub number: f32,
    pub across: f32,
    pub height_per_px: f32,
}

fn typical_value(decimals: Option<usize>) -> String {
    match decimals {
        None => "-000.000".into(),
        Some(0) => "-000".into(),
        Some(n) => format!("-000.{}", "0".repeat(n)),
    }
}

pub fn grid(n: usize, area: Vec2, number: impl Fn(Vec2) -> Fit) -> (usize, Vec2) {
    let n = n.max(1);
    let layouts: Vec<(usize, Vec2, Fit)> = (1..=n)
        .rev()
        .map(|cols| {
            let rows = n.div_ceil(cols);
            let tile = vec2(((area.x - GAP * (cols - 1) as f32) / cols as f32).floor(), ((area.y - GAP * (rows - 1) as f32) / rows as f32).floor());
            (cols, tile, number(tile))
        })
        .collect();
    let fits_across = layouts.iter().any(|l| l.2.across >= MIN_NUMBER);
    layouts
        .into_iter()
        .filter(|l| !fits_across || l.2.across >= MIN_NUMBER)
        .max_by(|a, b| a.2.number.total_cmp(&b.2.number))
        .map_or((1, area), |(cols, tile, _)| (cols, tile))
}

fn number_room(tile: Vec2, foot: f32) -> Vec2 {
    vec2(tile.x - 2.0 * PAD.x, tile.y - 2.0 * PAD.y - HEAD_H - foot - 2.0 * GAP)
}

fn min_tile_h(foot: f32) -> f32 {
    2.0 * PAD.y + HEAD_H + foot + 2.0 * GAP + 1.5 * MIN_NUMBER
}

fn size_per_px(ui: &egui::Ui, text: RichText) -> Vec2 {
    egui::WidgetText::from(text).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Body).size() / PROBE
}

fn units_text(units: &str, number: f32) -> RichText {
    RichText::new(units).size((number * UNITS_SHARE).max(UNITS_MIN))
}

fn number_fit(ui: &egui::Ui, values: &[&str], units: &str, room: Vec2) -> Fit {
    let value = values.iter().map(|v| size_per_px(ui, theme::num(*v, PROBE))).fold(Vec2::ZERO, Vec2::max);
    let (units_w, gap) = if units.is_empty() { (0.0, 0.0) } else { (size_per_px(ui, RichText::new(units).size(PROBE)).x, GAP) };
    let width = room.x * FIT_MARGIN - gap;
    let by_width = width / (value.x + UNITS_SHARE * units_w);
    let by_width = if by_width * UNITS_SHARE < UNITS_MIN { (width - UNITS_MIN * units_w) / value.x } else { by_width };
    Fit { number: by_width.min(room.y / value.y), across: by_width, height_per_px: value.y }
}

fn foot_text(text: &str, mono: bool) -> RichText {
    if mono { RichText::new(text).monospace().size(FOOT_SIZE) } else { theme::b(text).size(FOOT_SIZE) }
}

fn foot_height(ui: &egui::Ui, probe: Option<&(String, bool)>, width: f32) -> f32 {
    let line = ui.fonts_mut(|f| f.row_height(&egui::FontId::new(FOOT_SIZE, theme::bold())));
    let wrapped = probe.map_or(0.0, |(text, mono)| egui::WidgetText::from(foot_text(text, *mono)).into_galley(ui, Some(egui::TextWrapMode::Wrap), width.max(1.0), egui::TextStyle::Body).size().y);
    wrapped.max(line)
}

fn foot_of(tiles: &[Tile], ui: &egui::Ui, tile: Vec2) -> f32 {
    tiles.iter().map(|t| foot_height(ui, t.foot_probe.as_ref(), tile.x - 2.0 * PAD.x)).fold(0.0, f32::max)
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
            let (value, text, last_t) = match self.session.store().get(&c.key) {
                Some(ch) => {
                    let r = ch.lock();
                    let last_t = r.last().map(|(t, _)| t);
                    if r.kind == Some(spy_core::sample::ValueKind::String) { (r.last_text.clone(), true, last_t) } else { (view::readout_ms(&r, reading, view::readout_window(c.smooth_ms)).map(|v| d.fmt(v)), false, last_t) }
                }
                None => (None, false, None),
            };
            let f = |x: f64| d.fmt(x);
            let range = (!text && c.stats.n > 0).then(|| format!("{} to {}", f(c.stats.min), f(c.stats.max)));
            let age = cs.and_then(|c| c.last_arrival).map(|t| t.elapsed()).or_else(|| view::age_of(last_t, &st.timeline));
            out.push(self.tile(view::short_label(&self.catalogue, &c.key), c.color, h, value, if text { String::new() } else { d.units.clone() }, range, d.decimals, age));
            if let Some(t) = out.last_mut()
                && !text
                && c.stats.n > 0
            {
                t.extremes = vec![f(c.stats.min), f(c.stats.max)];
            }
        }
        for i in 0..self.derived.len() {
            let h = self.derived_health(i, st);
            let d = &self.derived[i];
            let def = d.live.def();
            let decimals = crate::derived_view::decimals(def);
            let (v, last_t) = {
                let ring = d.live.lock();
                (view::readout(&ring, crate::derived_view::reading(def)), ring.last().map(|(t, _)| t))
            };
            let (value, on_target) = crate::derived_view::value_text(def, v, h.is_live());
            let range = (d.stats.n > 0).then(|| format!("{} to {}", view::fmt_to(d.stats.min, decimals), view::fmt_to(d.stats.max, decimals)));
            let units = if on_target { String::new() } else { def.units().to_string() };
            let shown = (value != "--").then_some(value);
            let mut t = self.tile(self.derived_label(def), d.color, h, shown, units, range, decimals, view::age_of(last_t, &st.timeline));
            t.on_target = on_target;
            if d.stats.n > 0 {
                t.extremes = vec![view::fmt_to(d.stats.min, decimals), view::fmt_to(d.stats.max, decimals)];
            }
            if matches!(def, spy_core::derived::Derived::Turn { .. }) {
                t.also_fits.push(crate::derived_view::ON_TARGET.into());
                t.sized_units = def.units().to_string();
            }
            out.push(t);
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn tile(&self, name: String, color: egui::Color32, health: Health, value: Option<String>, units: String, range: Option<String>, decimals: Option<usize>, age: Option<std::time::Duration>) -> Tile {
        let has_value = value.is_some();
        let note = view::old_note(health, has_value, age);
        let probe_note = view::old_note(health, has_value, Some(AGE_PROBE));
        let typical = typical_value(decimals);
        let foot_probe = match (&probe_note, &range) {
            (Some(n), _) => Some((n.clone(), false)),
            (None, Some(_)) => Some((format!("{typical} to {typical}"), true)),
            (None, None) => None,
        };
        Tile {
            name,
            color,
            health,
            old: view::is_old(health, has_value),
            value: value.unwrap_or_else(|| "--".into()),
            range,
            extremes: Vec::new(),
            typical,
            note,
            foot_probe,
            on_target: false,
            also_fits: Vec::new(),
            sized_units: units.clone(),
            units,
        }
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
        let (cols, size) = layout(ui, &tiles, ui.available_size());
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(GAP, GAP);
            for row in tiles.chunks(cols) {
                ui.horizontal(|ui| {
                    for t in row {
                        tile(ui, t, size, &tiles);
                    }
                });
            }
        });
    }
}

fn layout(ui: &egui::Ui, tiles: &[Tile], area: Vec2) -> (usize, Vec2) {
    let fit = |area: Vec2| {
        grid(tiles.len(), area, |tile| {
            let room = number_room(tile, foot_of(tiles, ui, tile));
            let fits = tiles.iter().map(|t| {
                let mut values = vec![t.typical.as_str()];
                values.extend(t.also_fits.iter().map(String::as_str));
                number_fit(ui, &values, &t.sized_units, room)
            });
            fits.fold(Fit { number: f32::INFINITY, across: f32::INFINITY, height_per_px: 0.0 }, |a, b| Fit { number: a.number.min(b.number), across: a.across.min(b.across), height_per_px: a.height_per_px.max(b.height_per_px) })
        })
    };
    let rows_h = |cols: usize, size: Vec2| {
        let rows = tiles.len().div_ceil(cols.max(1));
        let h = size.y.max(min_tile_h(foot_of(tiles, ui, size)));
        rows as f32 * h + (rows.saturating_sub(1)) as f32 * GAP
    };
    let (cols, size) = fit(area);
    let (cols, size) = if rows_h(cols, size) > area.y + 0.5 { fit(vec2(area.x - ui.spacing().scroll.allocated_width(), area.y)) } else { (cols, size) };
    (cols, vec2(size.x, size.y.max(min_tile_h(foot_of(tiles, ui, size)))))
}

fn tile(ui: &mut egui::Ui, t: &Tile, size: Vec2, all: &[Tile]) {
    let p = theme::pal(ui);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    if t.old {
        ui.painter().rect(rect, 0.0, p.stale_face, Stroke::new(2.0, p.stale_edge), egui::StrokeKind::Inside);
    } else {
        ui.painter().rect_stroke(rect, 0.0, Stroke::new(1.0, p.line), egui::StrokeKind::Inside);
    }
    let inner = rect.shrink2(PAD);
    let clip = rect.intersect(ui.clip_rect());
    let slot = |ui: &mut egui::Ui, r: egui::Rect, layout: egui::Layout| {
        let mut c = ui.new_child(egui::UiBuilder::new().max_rect(r).layout(layout));
        c.set_clip_rect(r.expand2(vec2(0.0, PAD.y)).intersect(clip));
        c.spacing_mut().interact_size.y = 0.0;
        c
    };
    let head = egui::Rect::from_min_size(inner.min, vec2(inner.width(), HEAD_H));
    let mut h = slot(ui, head, egui::Layout::right_to_left(egui::Align::Center));
    h.spacing_mut().interact_size.y = HEAD_H;
    crate::channels::status_word(&mut h, t.health);
    h.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
        theme::square(ui, t.color, 16.0);
        ui.add(egui::Label::new(theme::b(&t.name).size(20.0)).truncate());
    });
    let mut values: Vec<&str> = std::iter::once(t.value.as_str()).chain(t.extremes.iter().map(String::as_str)).collect();
    values.extend(t.also_fits.iter().map(String::as_str));
    let foot = foot_of(all, ui, size);
    let fit = number_fit(ui, &values, &t.sized_units, number_room(size, foot));
    let number = fit.number.floor().max(MIN_NUMBER);
    let row = egui::Rect::from_min_size(egui::pos2(inner.left(), head.bottom() + GAP), vec2(inner.width(), (number * fit.height_per_px).ceil()));
    let mut n = slot(ui, row, egui::Layout::left_to_right(egui::Align::Center));
    let mut v = theme::num(&t.value, number);
    if t.old {
        v = v.color(p.ink2).strikethrough();
    } else if !t.health.is_live() {
        v = v.color(p.ink2);
    } else if t.on_target {
        v = v.color(p.live);
    }
    n.label(v);
    if !t.units.is_empty() {
        n.label(units_text(&t.units, number).color(p.ink2));
    }
    let below = egui::Rect::from_min_max(egui::pos2(inner.left(), row.bottom() + GAP), inner.max);
    let mut f = slot(ui, below, egui::Layout::top_down(egui::Align::Min));
    if let Some(note) = &t.note {
        f.add(egui::Label::new(foot_text(note, false).color(p.hold)).wrap());
    } else if let Some(r) = &t.range {
        f.add(egui::Label::new(foot_text(r, true).color(p.ink2)).wrap()).on_hover_text("The lowest and highest since the last reset");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eight_characters_wide(tile: Vec2) -> Fit {
        let across = tile.x / 5.5;
        Fit { number: across.min((tile.y - 124.0) / 1.3), across, height_per_px: 1.3 }
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

    #[test]
    fn a_layout_whose_numbers_would_run_out_of_their_cards_is_not_taken() {
        let short_and_wide = |tile: Vec2| Fit { number: (tile.y / 4.0).min(tile.x / 6.0), across: tile.x / 6.0, height_per_px: 1.3 };
        let (cols, tile) = grid(12, vec2(880.0, 60.0), short_and_wide);
        assert!(tile.x / 6.0 >= MIN_NUMBER, "{cols} across: numbers {} px wide at most, under the {MIN_NUMBER} px floor", tile.x / 6.0);
        assert!(cols < 12);
    }
}
