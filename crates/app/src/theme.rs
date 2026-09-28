//! The look: dark by default with a light toggle (E3), and a channel palette that
//! stays distinguishable for twelve channels on either background.

use eframe::egui::{self, Color32};

/// Twelve distinct channel colours (Tableau 10 plus two), readable on dark and light.
pub const PALETTE: [Color32; 12] = [
    Color32::from_rgb(0x4E, 0x79, 0xA7),
    Color32::from_rgb(0xF2, 0x8E, 0x2B),
    Color32::from_rgb(0x59, 0xA1, 0x4F),
    Color32::from_rgb(0xE1, 0x57, 0x59),
    Color32::from_rgb(0x76, 0xB7, 0xB2),
    Color32::from_rgb(0xED, 0xC9, 0x48),
    Color32::from_rgb(0xB0, 0x7A, 0xA1),
    Color32::from_rgb(0xFF, 0x9D, 0xA7),
    Color32::from_rgb(0x9C, 0x75, 0x5F),
    Color32::from_rgb(0xBA, 0xB0, 0xAC),
    Color32::from_rgb(0x86, 0xBC, 0xB6),
    Color32::from_rgb(0xD3, 0x72, 0x95),
];

pub const OK: Color32 = Color32::from_rgb(0x4C, 0xB8, 0x6A);
pub const WARN: Color32 = Color32::from_rgb(0xE8, 0xB0, 0x30);
pub const BAD: Color32 = Color32::from_rgb(0xE0, 0x55, 0x4B);
pub const IDLE: Color32 = Color32::from_rgb(0x8B, 0x93, 0xA1);
pub const REC: Color32 = Color32::from_rgb(0xE0, 0x3C, 0x3C);
/// Reviewing a recording: a colour of its own, used for nothing live.
pub const REVIEW: Color32 = Color32::from_rgb(0x9B, 0x7B, 0xE0);
/// Behind the reviewing banner; readable under both themes' text.
pub const REVIEW_BG: Color32 = Color32::from_rgba_premultiplied(0x3A, 0x2C, 0x5C, 0x70);

/// egui's built-in fonts lack many symbols (● ■ ▾ ★ and the like render as empty
/// boxes). Windows' own symbol and UI fonts are added as fallbacks, which also
/// covers any unusual character in a controller or unit name. Missing files are
/// skipped: the program then looks plainer, not broken.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let windir = std::env::var_os("WINDIR").map(std::path::PathBuf::from).unwrap_or_else(|| "C:\\Windows".into());
    for (name, file) in [("segoe-ui-symbol", "seguisym.ttf"), ("segoe-ui", "segoeui.ttf")] {
        if let Ok(bytes) = std::fs::read(windir.join("Fonts").join(file)) {
            fonts.font_data.insert(name.to_string(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
            for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                if let Some(list) = fonts.families.get_mut(&fam) {
                    list.push(name.to_string());
                }
            }
        }
    }
    ctx.set_fonts(fonts);
}

pub fn apply(ctx: &egui::Context, dark: bool, scale: f32) {
    ctx.set_theme(if dark { egui::Theme::Dark } else { egui::Theme::Light });
    ctx.set_zoom_factor(scale);
}

/// A small coloured badge, for FROZEN, STALE and the like.
pub fn badge(ui: &mut egui::Ui, text: &str, color: Color32, tip: &str) -> egui::Response {
    let t = egui::RichText::new(text).small().strong().color(Color32::BLACK).background_color(color);
    let r = ui.label(t);
    if tip.is_empty() { r } else { r.on_hover_text(tip) }
}
