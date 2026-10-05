use eframe::egui::{self, Color32, CornerRadius, FontFamily, FontId, Margin, Rect, RichText, Sense, Shadow, Stroke, TextStyle, Vec2, pos2, vec2};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pal {
    pub page: Color32,
    pub sheet: Color32,
    pub line: Color32,
    pub ink: Color32,
    pub ink2: Color32,
    pub ink3: Color32,
    pub red: Color32,
    pub link: Color32,
    pub live: Color32,
    pub hold: Color32,
    pub field: Color32,
    pub field_edge: Color32,
    pub field_edge_hover: Color32,
    pub off_face: Color32,
    pub off_edge: Color32,
    pub face_hover: Color32,
    pub face_pressed: Color32,
    pub primary: Color32,
    pub primary_hover: Color32,
    pub primary_pressed: Color32,
    pub on_primary: Color32,
    pub panic: Color32,
    pub panic_hover: Color32,
    pub panic_pressed: Color32,
    pub on_panic: Color32,
    pub plot: Color32,
    pub grid: Color32,
    pub grid_zero: Color32,
    pub chip: Color32,
    pub stale_face: Color32,
    pub stale_edge: Color32,
    pub picked: Color32,
}

pub const DARK: Pal = Pal {
    page: Color32::from_rgb(0x0e, 0x0e, 0x0e),
    sheet: Color32::from_rgb(0x1b, 0x1b, 0x1a),
    line: Color32::from_rgb(0x34, 0x34, 0x32),
    ink: Color32::from_rgb(0xef, 0xee, 0xe9),
    ink2: Color32::from_rgb(0xc9, 0xc8, 0xc2),
    ink3: Color32::from_rgb(0xba, 0xb9, 0xb3),
    red: Color32::from_rgb(0xff, 0x56, 0x46),
    link: Color32::from_rgb(0xff, 0x6a, 0x5a),
    live: Color32::from_rgb(0x52, 0xd9, 0x8a),
    hold: Color32::from_rgb(0xff, 0xb5, 0x41),
    field: Color32::from_rgb(0x11, 0x11, 0x11),
    field_edge: Color32::from_rgb(0x7f, 0x7e, 0x79),
    field_edge_hover: Color32::from_rgb(0xb3, 0xb2, 0xac),
    off_face: Color32::from_rgb(0x16, 0x16, 0x16),
    off_edge: Color32::from_rgb(0x6c, 0x6b, 0x67),
    face_hover: Color32::from_rgb(0x26, 0x26, 0x24),
    face_pressed: Color32::from_rgb(0x0e, 0x0e, 0x0e),
    primary: Color32::from_rgb(0xef, 0xee, 0xe9),
    primary_hover: Color32::from_rgb(0xff, 0xff, 0xff),
    primary_pressed: Color32::from_rgb(0xd3, 0xd2, 0xcc),
    on_primary: Color32::from_rgb(0x11, 0x11, 0x11),
    panic: Color32::from_rgb(0xd8, 0x30, 0x1f),
    panic_hover: Color32::from_rgb(0xc1, 0x28, 0x17),
    panic_pressed: Color32::from_rgb(0xa8, 0x22, 0x14),
    on_panic: Color32::from_rgb(0xff, 0xff, 0xff),
    plot: Color32::from_rgb(0x11, 0x11, 0x11),
    grid: Color32::from_rgb(0x26, 0x26, 0x24),
    grid_zero: Color32::from_rgb(0x3c, 0x3c, 0x3a),
    chip: Color32::from_rgb(0x23, 0x23, 0x22),
    stale_face: Color32::from_rgb(0x24, 0x1d, 0x0e),
    stale_edge: Color32::from_rgb(0x8a, 0x64, 0x20),
    picked: Color32::from_rgb(0x2c, 0x2c, 0x2a),
};

pub const LIGHT: Pal = Pal {
    page: Color32::from_rgb(0xd6, 0xd5, 0xcf),
    sheet: Color32::from_rgb(0xf7, 0xf6, 0xf2),
    line: Color32::from_rgb(0xcf, 0xce, 0xc8),
    ink: Color32::from_rgb(0x14, 0x14, 0x14),
    ink2: Color32::from_rgb(0x3a, 0x39, 0x35),
    ink3: Color32::from_rgb(0x3f, 0x3e, 0x3a),
    red: Color32::from_rgb(0xa8, 0x1c, 0x10),
    link: Color32::from_rgb(0xa8, 0x1c, 0x10),
    live: Color32::from_rgb(0x0f, 0x5f, 0x30),
    hold: Color32::from_rgb(0x7a, 0x49, 0x00),
    field: Color32::from_rgb(0xff, 0xff, 0xff),
    field_edge: Color32::from_rgb(0x6c, 0x6b, 0x67),
    field_edge_hover: Color32::from_rgb(0x3a, 0x39, 0x35),
    off_face: Color32::from_rgb(0xec, 0xeb, 0xe6),
    off_edge: Color32::from_rgb(0x8f, 0x8e, 0x89),
    face_hover: Color32::from_rgb(0xe6, 0xe5, 0xe0),
    face_pressed: Color32::from_rgb(0xd6, 0xd5, 0xcf),
    primary: Color32::from_rgb(0x14, 0x14, 0x14),
    primary_hover: Color32::from_rgb(0x00, 0x00, 0x00),
    primary_pressed: Color32::from_rgb(0x3a, 0x39, 0x35),
    on_primary: Color32::from_rgb(0xf7, 0xf6, 0xf2),
    panic: Color32::from_rgb(0xc2, 0x21, 0x14),
    panic_hover: Color32::from_rgb(0xa8, 0x1c, 0x10),
    panic_pressed: Color32::from_rgb(0x8f, 0x17, 0x0d),
    on_panic: Color32::from_rgb(0xff, 0xff, 0xff),
    plot: Color32::from_rgb(0xff, 0xff, 0xff),
    grid: Color32::from_rgb(0xe9, 0xe8, 0xe3),
    grid_zero: Color32::from_rgb(0xc4, 0xc3, 0xbd),
    chip: Color32::from_rgb(0xef, 0xee, 0xe9),
    stale_face: Color32::from_rgb(0xfb, 0xf1, 0xda),
    stale_edge: Color32::from_rgb(0xb0, 0x7a, 0x10),
    picked: Color32::from_rgb(0xd9, 0xd8, 0xd2),
};

pub fn pal_of(dark: bool) -> &'static Pal {
    if dark { &DARK } else { &LIGHT }
}

pub fn pal(ui: &egui::Ui) -> &'static Pal {
    pal_of(ui.visuals().dark_mode)
}

const CHANNELS_DARK: [Color32; 12] = [
    Color32::from_rgb(0x6a, 0xa7, 0xff),
    Color32::from_rgb(0xff, 0x9f, 0x43),
    Color32::from_rgb(0xe9, 0x8b, 0xe0),
    Color32::from_rgb(0x3f, 0xd0, 0xc4),
    Color32::from_rgb(0xe3, 0xd3, 0x6a),
    Color32::from_rgb(0x9b, 0xe3, 0x6a),
    Color32::from_rgb(0xc5, 0x9b, 0xff),
    Color32::from_rgb(0xff, 0x7b, 0x7b),
    Color32::from_rgb(0x5f, 0xd3, 0xff),
    Color32::from_rgb(0xd9, 0xb4, 0x8f),
    Color32::from_rgb(0xa8, 0xe6, 0xcf),
    Color32::from_rgb(0xff, 0xb3, 0xd1),
];

const CHANNELS_LIGHT: [Color32; 12] = [
    Color32::from_rgb(0x1f, 0x5f, 0xbf),
    Color32::from_rgb(0xb3, 0x53, 0x00),
    Color32::from_rgb(0xa4, 0x33, 0x9a),
    Color32::from_rgb(0x00, 0x79, 0x6b),
    Color32::from_rgb(0x8a, 0x74, 0x00),
    Color32::from_rgb(0x3d, 0x7a, 0x1a),
    Color32::from_rgb(0x6b, 0x3f, 0xbf),
    Color32::from_rgb(0xc0, 0x39, 0x2b),
    Color32::from_rgb(0x00, 0x74, 0x9e),
    Color32::from_rgb(0x7a, 0x52, 0x30),
    Color32::from_rgb(0x2e, 0x7d, 0x5b),
    Color32::from_rgb(0xb0, 0x3a, 0x6f),
];

pub fn channel_color(i: usize, dark: bool) -> Color32 {
    let p = if dark { &CHANNELS_DARK } else { &CHANNELS_LIGHT };
    p[i % p.len()]
}

const NEXT: &[u8] = include_bytes!("../fonts/AtkinsonHyperlegibleNext-Variable.ttf");
const MONO: &[u8] = include_bytes!("../fonts/AtkinsonHyperlegibleMono-Variable.ttf");

pub fn bold() -> FontFamily {
    FontFamily::Name("bold".into())
}

pub fn heavy() -> FontFamily {
    FontFamily::Name("heavy".into())
}

pub fn mono_bold() -> FontFamily {
    FontFamily::Name("mono-bold".into())
}

fn face(bytes: &'static [u8], weight: f32) -> egui::FontData {
    egui::FontData::from_static(bytes).tweak(egui::FontTweak { coords: egui::epaint::text::VariationCoords::new([("wght", weight)]), ..Default::default() })
}

pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let egui_fallbacks: Vec<String> = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    for (name, bytes, weight) in [("next-400", NEXT, 400.0), ("next-700", NEXT, 700.0), ("next-800", NEXT, 800.0), ("mono-400", MONO, 400.0), ("mono-700", MONO, 700.0)] {
        fonts.font_data.insert(name.into(), std::sync::Arc::new(face(bytes, weight)));
    }
    let mut windows = Vec::new();
    let windir = std::env::var_os("WINDIR").map(std::path::PathBuf::from).unwrap_or_else(|| "C:\\Windows".into());
    for (name, file) in [("segoe-ui-symbol", "seguisym.ttf"), ("segoe-ui", "segoeui.ttf")] {
        if let Ok(bytes) = std::fs::read(windir.join("Fonts").join(file)) {
            fonts.font_data.insert(name.into(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
            windows.push(name.to_string());
        }
    }
    let family = |first: &str| -> Vec<String> { std::iter::once(first.to_string()).chain(windows.iter().cloned()).chain(egui_fallbacks.iter().cloned()).collect() };
    fonts.families.insert(FontFamily::Proportional, family("next-400"));
    fonts.families.insert(bold(), family("next-700"));
    fonts.families.insert(heavy(), family("next-800"));
    fonts.families.insert(FontFamily::Monospace, family("mono-400"));
    fonts.families.insert(mono_bold(), family("mono-700"));
    ctx.set_fonts(fonts);
}

pub fn b(text: impl Into<String>) -> RichText {
    RichText::new(text).family(bold())
}

pub fn num(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text).font(FontId::new(size, mono_bold()))
}

pub const TOOL_H: f32 = 36.0;
pub const SMALL_H: f32 = 30.0;

fn style(dark: bool) -> egui::Style {
    let p = pal_of(dark);
    let mut s = egui::Style {
        text_styles: [
            (TextStyle::Small, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(16.0, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(16.0, bold())),
            (TextStyle::Monospace, FontId::new(16.0, FontFamily::Monospace)),
            (TextStyle::Heading, FontId::new(20.0, heavy())),
        ]
        .into(),
        drag_value_text_style: TextStyle::Body,
        ..Default::default()
    };
    s.interaction.selectable_labels = false;
    let sp = &mut s.spacing;
    sp.item_spacing = vec2(8.0, 6.0);
    sp.button_padding = vec2(12.0, 6.0);
    sp.interact_size = vec2(40.0, TOOL_H);
    sp.window_margin = Margin::same(14);
    sp.menu_margin = Margin::same(6);
    sp.indent = 20.0;
    sp.icon_width = 20.0;
    sp.icon_width_inner = 10.0;
    sp.icon_spacing = 8.0;
    sp.combo_width = 100.0;
    sp.text_edit_width = 200.0;
    sp.tooltip_width = 460.0;
    sp.menu_width = 360.0;
    sp.combo_height = 400.0;
    sp.scroll = egui::style::ScrollStyle::solid();
    sp.scroll.bar_width = 12.0;
    sp.scroll.handle_min_length = 32.0;

    let v = &mut s.visuals;
    *v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.dark_mode = dark;
    v.override_text_color = None;
    v.weak_text_color = Some(p.ink2);
    let square = CornerRadius::ZERO;
    let w = &mut v.widgets;
    w.noninteractive = egui::style::WidgetVisuals { bg_fill: p.sheet, weak_bg_fill: p.sheet, bg_stroke: Stroke::new(1.0, p.line), corner_radius: square, fg_stroke: Stroke::new(1.0, p.ink), expansion: 0.0 };
    w.inactive = egui::style::WidgetVisuals { bg_fill: p.field, weak_bg_fill: p.sheet, bg_stroke: Stroke::new(1.0, p.ink), corner_radius: square, fg_stroke: Stroke::new(1.5, p.ink), expansion: 0.0 };
    w.hovered = egui::style::WidgetVisuals { bg_fill: p.face_hover, weak_bg_fill: p.face_hover, bg_stroke: Stroke::new(1.0, p.ink), corner_radius: square, fg_stroke: Stroke::new(1.5, p.ink), expansion: 0.0 };
    w.active = egui::style::WidgetVisuals { bg_fill: p.face_pressed, weak_bg_fill: p.face_pressed, bg_stroke: Stroke::new(1.0, p.ink), corner_radius: square, fg_stroke: Stroke::new(2.0, p.ink), expansion: 0.0 };
    w.open = w.hovered;
    v.selection.bg_fill = p.picked;
    v.selection.stroke = Stroke::new(1.0, p.ink);
    v.hyperlink_color = p.link;
    v.faint_bg_color = if dark { Color32::from_rgb(0x22, 0x22, 0x21) } else { Color32::from_rgb(0xee, 0xed, 0xe8) };
    v.extreme_bg_color = p.field;
    v.text_edit_bg_color = Some(p.field);
    v.code_bg_color = p.field;
    v.warn_fg_color = p.hold;
    v.error_fg_color = p.red;
    v.window_corner_radius = square;
    v.window_shadow = Shadow::NONE;
    v.window_fill = p.sheet;
    v.window_stroke = Stroke::new(1.0, p.ink);
    v.window_highlight_topmost = false;
    v.menu_corner_radius = square;
    v.panel_fill = p.page;
    v.popup_shadow = Shadow::NONE;
    v.text_cursor.stroke = Stroke::new(2.0, p.ink);
    v.collapsing_header_frame = false;
    v.indent_has_left_vline = true;
    v.striped = false;
    v.handle_shape = egui::style::HandleShape::Rect { aspect_ratio: 0.6 };
    v.disabled_alpha = 0.75;
    s
}

pub fn apply(ctx: &egui::Context, dark: bool, scale: f32) {
    ctx.set_style_of(egui::Theme::Dark, style(true));
    ctx.set_style_of(egui::Theme::Light, style(false));
    ctx.set_theme(if dark { egui::Theme::Dark } else { egui::Theme::Light });
    ctx.set_zoom_factor(scale);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Down,
    Right,
    Left,
    FoldLeft,
    FoldRight,
    Expand,
    Collapse,
    Plus,
    Minus,
    Close,
    Grid,
}

pub fn paint_icon(painter: &egui::Painter, rect: Rect, icon: Icon, color: Color32) {
    let c = rect.center();
    let st = Stroke::new(2.0, color);
    let line = |pts: &[(f32, f32)]| egui::Shape::line(pts.iter().map(|(x, y)| pos2(c.x + x, c.y + y)).collect(), st);
    let shapes = match icon {
        Icon::Down => vec![line(&[(-5.0, -2.5), (0.0, 2.5), (5.0, -2.5)])],
        Icon::Right => vec![line(&[(-2.5, -5.0), (2.5, 0.0), (-2.5, 5.0)])],
        Icon::Left => vec![line(&[(2.5, -5.0), (-2.5, 0.0), (2.5, 5.0)])],
        Icon::FoldLeft => vec![line(&[(-0.5, -5.0), (-5.0, 0.0), (-0.5, 5.0)]), line(&[(5.5, -5.0), (1.0, 0.0), (5.5, 5.0)])],
        Icon::FoldRight => vec![line(&[(0.5, -5.0), (5.0, 0.0), (0.5, 5.0)]), line(&[(-5.5, -5.0), (-1.0, 0.0), (-5.5, 5.0)])],
        Icon::Expand => vec![
            line(&[(1.0, -5.5), (5.5, -5.5), (5.5, -1.0)]),
            line(&[(5.5, -5.5), (1.0, -1.0)]),
            line(&[(-1.0, 5.5), (-5.5, 5.5), (-5.5, 1.0)]),
            line(&[(-5.5, 5.5), (-1.0, 1.0)]),
        ],
        Icon::Collapse => vec![
            line(&[(-5.5, -1.0), (-1.0, -1.0), (-1.0, -5.5)]),
            line(&[(-1.0, -1.0), (-5.5, -5.5)]),
            line(&[(5.5, 1.0), (1.0, 1.0), (1.0, 5.5)]),
            line(&[(1.0, 1.0), (5.5, 5.5)]),
        ],
        Icon::Plus => vec![line(&[(-5.0, 0.0), (5.0, 0.0)]), line(&[(0.0, -5.0), (0.0, 5.0)])],
        Icon::Minus => vec![line(&[(-5.0, 0.0), (5.0, 0.0)])],
        Icon::Close => vec![line(&[(-4.5, -4.5), (4.5, 4.5)]), line(&[(-4.5, 4.5), (4.5, -4.5)])],
        Icon::Grid => {
            let r = |x: f32, y: f32| egui::Shape::rect_stroke(Rect::from_min_size(pos2(c.x + x, c.y + y), vec2(5.0, 5.0)), 0.0, st, egui::StrokeKind::Inside);
            vec![r(-6.5, -6.5), r(1.5, -6.5), r(-6.5, 1.5), r(1.5, 1.5)]
        }
    };
    painter.extend(shapes);
}

pub fn icon_button(ui: &mut egui::Ui, icon: Icon, name: &str, size: Vec2) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(size, Sense::click());
    r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), name));
    if ui.is_rect_visible(rect) {
        let v = ui.style().interact(&r);
        ui.painter().rect(rect, 0.0, v.weak_bg_fill, v.bg_stroke, egui::StrokeKind::Inside);
        paint_icon(ui.painter(), rect, icon, v.fg_stroke.color);
    }
    r.on_hover_text(name)
}

pub fn square(ui: &mut egui::Ui, color: Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, color);
}

pub fn hollow(ui: &mut egui::Ui, color: Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    ui.painter().rect_stroke(rect, 0.0, Stroke::new(1.5, color), egui::StrokeKind::Inside);
}

pub fn dashed_rect(painter: &egui::Painter, rect: Rect, stroke: Stroke) {
    let r = rect.shrink(stroke.width / 2.0);
    let pts = vec![r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()];
    painter.extend(egui::Shape::dashed_line(&pts, stroke, 5.0, 3.0));
}

pub fn primary(ui: &mut egui::Ui, text: &str, height: f32) -> egui::Response {
    let p = pal(ui);
    filled(ui, egui::Button::new(text).min_size(vec2(0.0, height)), (p.primary, p.primary_hover, p.primary_pressed), p.on_primary)
}

pub fn red_button(ui: &mut egui::Ui, button: egui::Button<'_>) -> egui::Response {
    let p = pal(ui);
    filled(ui, button, (p.panic, p.panic_hover, p.panic_pressed), p.on_panic)
}

fn filled(ui: &mut egui::Ui, button: egui::Button<'_>, (rest, hover, pressed): (Color32, Color32, Color32), text: Color32) -> egui::Response {
    ui.scope(|ui| {
        let w = &mut ui.visuals_mut().widgets;
        for (v, fill) in [(&mut w.inactive, rest), (&mut w.hovered, hover), (&mut w.active, pressed)] {
            v.weak_bg_fill = fill;
            v.bg_fill = fill;
            v.bg_stroke = Stroke::new(1.0, rest);
            v.fg_stroke = Stroke::new(1.5, text);
        }
        ui.add(button)
    })
    .inner
}

pub fn tool(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(egui::Button::new(text).min_size(vec2(0.0, TOOL_H)))
}

pub fn drop_button(ui: &mut egui::Ui, text: &str, height: f32) -> egui::Response {
    let id = ui.next_auto_id().with("chevron");
    let r = egui::Button::new((text, egui::Atom::custom(id, vec2(12.0, 10.0)))).min_size(vec2(0.0, height)).atom_ui(ui);
    if let Some(rect) = r.rect(id) {
        let color = ui.style().interact(&r.response).fg_stroke.color;
        paint_icon(ui.painter(), rect, Icon::Down, color);
    }
    r.response
}

pub fn outline_button(ui: &mut egui::Ui, text: &str, color: Color32, height: f32) -> egui::Response {
    ui.scope(|ui| {
        let w = &mut ui.visuals_mut().widgets;
        for v in [&mut w.inactive, &mut w.hovered, &mut w.active] {
            v.bg_stroke = Stroke::new(1.0, color);
            v.fg_stroke = Stroke::new(1.5, color);
        }
        ui.add(egui::Button::new(text).min_size(vec2(0.0, height)))
    })
    .inner
}

pub fn icon_text_button(ui: &mut egui::Ui, icon: Icon, text: &str, height: f32, on: bool) -> egui::Response {
    let id = ui.next_auto_id().with("icon");
    let p = pal(ui);
    let button = egui::Button::new((egui::Atom::custom(id, vec2(16.0, 16.0)), text)).min_size(vec2(0.0, height));
    let r = if on {
        ui.scope(|ui| {
            let w = &mut ui.visuals_mut().widgets;
            for (v, fill) in [(&mut w.inactive, p.primary), (&mut w.hovered, p.primary_hover), (&mut w.active, p.primary_pressed)] {
                v.weak_bg_fill = fill;
                v.bg_stroke = Stroke::new(1.0, p.primary);
                v.fg_stroke = Stroke::new(1.5, p.on_primary);
            }
            button.atom_ui(ui)
        })
        .inner
    } else {
        button.atom_ui(ui)
    };
    if let Some(rect) = r.rect(id) {
        let color = if on { p.on_primary } else { ui.style().interact(&r.response).fg_stroke.color };
        paint_icon(ui.painter(), rect, icon, color);
    }
    r.response
}

pub fn button_width(ui: &egui::Ui, text: &str) -> f32 {
    let galley = egui::WidgetText::from(text).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, TextStyle::Button);
    galley.size().x + 2.0 * ui.spacing().button_padding.x
}

pub fn time_axis<'a>(p: &Pal, fmt: impl Fn(egui_plot::GridMark, &std::ops::RangeInclusive<f64>) -> String + 'a) -> egui_plot::AxisHints<'a> {
    egui_plot::AxisHints::new_x().formatter(fmt).label_spacing(92.0..=93.0).tick_label_font(FontId::monospace(14.0)).tick_label_color(p.ink2)
}

pub fn value_axis<'a>(p: &Pal) -> egui_plot::AxisHints<'a> {
    egui_plot::AxisHints::new_y().label_spacing(22.0..=23.0).min_thickness(52.0).tick_label_font(FontId::monospace(14.0)).tick_label_color(p.ink2)
}

pub fn combo_icon(ui: &egui::Ui, rect: Rect, visuals: &egui::style::WidgetVisuals, _open: bool) {
    paint_icon(ui.painter(), Rect::from_center_size(rect.center(), vec2(12.0, 10.0)), Icon::Down, visuals.fg_stroke.color);
}

pub fn vrule(ui: &mut egui::Ui, height: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(9.0, height), Sense::hover());
    ui.painter().vline(rect.center().x, rect.y_range(), Stroke::new(1.0, pal(ui).line));
}

pub fn upright_button(ui: &mut egui::Ui, text: &str, size: Vec2) -> egui::Response {
    let p = pal(ui);
    let (rect, r) = ui.allocate_exact_size(size, Sense::click());
    r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), text));
    if ui.is_rect_visible(rect) {
        let fill = if r.is_pointer_button_down_on() { p.primary_pressed } else if r.hovered() { p.primary_hover } else { p.primary };
        ui.painter().rect_filled(rect, 0.0, fill);
        let galley = egui::WidgetText::from(b(text)).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, TextStyle::Button);
        let (w, h) = (galley.size().x, galley.size().y);
        let at = pos2(rect.center().x - h / 2.0, rect.center().y + w / 2.0);
        ui.painter().add(egui::epaint::TextShape::new(at, galley, p.on_primary).with_angle(-std::f32::consts::FRAC_PI_2));
    }
    r
}

pub fn chip(ui: &mut egui::Ui, on: bool, text: &str) -> egui::Response {
    let p = pal(ui);
    let galley = egui::WidgetText::from(b(text)).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, TextStyle::Button);
    let size = vec2(galley.size().x + 24.0, 34.0);
    let (rect, r) = ui.allocate_exact_size(size, Sense::click());
    r.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, ui.is_enabled(), on, galley.text()));
    if ui.is_rect_visible(rect) {
        let text_color = if on {
            ui.painter().rect_filled(rect, 0.0, if r.hovered() { p.primary_hover } else { p.primary });
            p.on_primary
        } else {
            if r.hovered() {
                ui.painter().rect_filled(rect, 0.0, p.face_hover);
            }
            dashed_rect(ui.painter(), rect, Stroke::new(1.0, p.field_edge_hover));
            p.ink
        };
        ui.painter().galley(rect.center() - galley.size() / 2.0, galley, text_color);
    }
    r
}

pub fn split(ui: &mut egui::Ui, arrow_name: &str, main: impl FnOnce(&mut egui::Ui) -> egui::Response, settings: impl FnOnce(&mut egui::Ui)) -> egui::Response {
    let r = main(ui);
    ui.add_space(-ui.spacing().item_spacing.x);
    let arrow = icon_button(ui, Icon::Down, arrow_name, vec2(32.0, r.rect.height()));
    egui::Popup::from_toggle_button_response(&arrow).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        ui.set_min_width(260.0);
        settings(ui);
    });
    r
}

pub fn section<R>(ui: &mut egui::Ui, number: &str, title: &str, strong: bool, right: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let p = pal(ui);
    let inner = ui
        .horizontal(|ui| {
            ui.set_min_height(30.0);
            heading(ui, number, title);
            right(ui)
        })
        .inner;
    let y = ui.cursor().top() + 1.0;
    let x = ui.max_rect().x_range();
    if strong {
        ui.painter().hline(x, y, Stroke::new(2.0, p.ink));
    } else {
        ui.painter().hline(x, y, Stroke::new(1.0, p.line));
    }
    ui.add_space(8.0);
    inner
}

fn heading(ui: &mut egui::Ui, number: &str, title: &str) {
    let p = pal(ui);
    ui.label(RichText::new(number).font(FontId::new(18.0, heavy())).color(p.red));
    if !title.is_empty() {
        ui.label(RichText::new(title).font(FontId::new(18.0, heavy())).color(p.ink));
    }
}

pub fn section_tools(ui: &mut egui::Ui, number: &str, title: &str, left_w: f32, right_w: f32, left: impl FnOnce(&mut egui::Ui), right: impl FnOnce(&mut egui::Ui)) {
    let p = pal(ui);
    let title_w = [number, title].iter().filter(|t| !t.is_empty()).map(|t| egui::WidgetText::from(RichText::new(*t).font(FontId::new(18.0, heavy()))).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, TextStyle::Body).size().x + ui.spacing().item_spacing.x).sum::<f32>();
    let one_row = title_w + left_w + right_w + 24.0 <= ui.available_width();
    let mut right = Some(right);
    ui.horizontal(|ui| {
        ui.set_min_height(TOOL_H);
        heading(ui, number, title);
        left(ui);
        if one_row && let Some(r) = right.take() {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), r);
        }
    });
    if let Some(r) = right {
        ui.allocate_ui_with_layout(vec2(ui.available_width(), TOOL_H), egui::Layout::right_to_left(egui::Align::Center), r);
    }
    let y = ui.cursor().top() + 1.0;
    ui.painter().hline(ui.max_rect().x_range(), y, Stroke::new(1.0, p.line));
    ui.add_space(8.0);
}

pub fn buttons_width(ui: &egui::Ui, texts: &[&str], extra: f32) -> f32 {
    texts.iter().map(|t| button_width(ui, t)).sum::<f32>() + ui.spacing().item_spacing.x * texts.len() as f32 + extra
}

pub fn sheet_frame(ui: &egui::Ui) -> egui::Frame {
    egui::Frame::new().fill(pal(ui).sheet).inner_margin(Margin::same(10))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    On,
    Off,
    None,
}

pub fn status_cell(ui: &mut egui::Ui, width: f32, key: &str, value: &str, color: Color32, mark: Mark, rule: Color32) -> egui::Response {
    let p = pal(ui);
    ui.allocate_ui_with_layout(vec2(width, 46.0), egui::Layout::top_down(egui::Align::Min), |ui| {
        ui.set_width(width);
        ui.set_height(46.0);
        let top = ui.cursor().top();
        ui.painter().hline(ui.max_rect().x_range(), top + 1.0, Stroke::new(2.0, rule));
        ui.add_space(4.0);
        ui.spacing_mut().item_spacing.y = 0.0;
        ui.label(RichText::new(key).size(14.0).color(p.ink2));
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            match mark {
                Mark::On => square(ui, color, 10.0),
                Mark::Off => hollow(ui, color, 10.0),
                Mark::None => {}
            }
            ui.add(egui::Label::new(b(value).size(17.0).color(color)).truncate());
        });
    })
    .response
}

pub fn badge(ui: &mut egui::Ui, text: &str, color: Color32, tip: &str) -> egui::Response {
    let galley = egui::WidgetText::from(b(text).size(14.0)).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, TextStyle::Small);
    let (rect, r) = ui.allocate_exact_size(galley.size() + vec2(12.0, 2.0), Sense::hover());
    r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, galley.text()));
    ui.painter().rect_stroke(rect, 0.0, Stroke::new(1.0, color), egui::StrokeKind::Inside);
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, color);
    if tip.is_empty() { r } else { r.on_hover_text(tip) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contrast(a: Color32, b: Color32) -> f64 {
        let lum = |c: Color32| {
            let f = |u: u8| {
                let c = f64::from(u) / 255.0;
                if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
            };
            0.2126 * f(c.r()) + 0.7152 * f(c.g()) + 0.0722 * f(c.b())
        };
        let (x, y) = (lum(a), lum(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    #[test]
    fn every_text_colour_reads_at_seven_to_one_and_every_state_at_four_and_a_half() {
        for (name, p) in [("dark", DARK), ("light", LIGHT)] {
            for (text, t) in [("ink", p.ink), ("ink2", p.ink2), ("ink3", p.ink3)] {
                for (bg, g) in [("sheet", p.sheet), ("page", p.page), ("field", p.field), ("plot", p.plot), ("stale face", p.stale_face), ("hover", p.face_hover), ("picked", p.picked)] {
                    let c = contrast(t, g);
                    assert!(c >= 7.0, "{name}: {text} on {bg} is {c:.2}:1");
                }
            }
            for (word, t) in [("red", p.red), ("link", p.link), ("live", p.live), ("hold", p.hold)] {
                for (bg, g) in [("sheet", p.sheet), ("page", p.page), ("stale face", p.stale_face)] {
                    let c = contrast(t, g);
                    assert!(c >= 4.5, "{name}: {word} on {bg} is {c:.2}:1");
                }
            }
            assert!(contrast(p.on_primary, p.primary) >= 7.0, "{name}: a primary button's label");
            assert!(contrast(p.on_primary, p.primary_hover) >= 7.0 && contrast(p.on_primary, p.primary_pressed) >= 7.0, "{name}: a primary button's label, pressed or hovered");
            for f in [p.panic, p.panic_hover, p.panic_pressed] {
                assert!(contrast(p.on_panic, f) >= 4.5, "{name}: the record button's label on {f:?}");
            }
            for (bg, g) in [("sheet", p.sheet), ("field", p.field)] {
                assert!(contrast(p.field_edge, g) >= 3.0, "{name}: a field's edge on {bg}");
                assert!(contrast(p.off_edge, g) >= 3.0, "{name}: a dashed edge on {bg}");
            }
        }
    }

    #[test]
    fn every_channel_colour_stands_out_on_its_chart() {
        for i in 0..12 {
            for (dark, p) in [(true, DARK), (false, LIGHT)] {
                let c = channel_color(i, dark);
                assert!(contrast(c, p.plot) >= 4.5, "channel {i} on the {} chart: {:.2}:1", if dark { "dark" } else { "light" }, contrast(c, p.plot));
                assert!(contrast(c, p.sheet) >= 3.0, "channel {i}'s square on the sheet");
            }
        }
        assert_eq!(channel_color(12, true), channel_color(0, true), "the colours go round");
    }

    #[test]
    fn text_is_never_under_fourteen_pixels() {
        for dark in [true, false] {
            let s = style(dark);
            for (t, f) in &s.text_styles {
                assert!(f.size >= 14.0, "{t:?} is {} px", f.size);
            }
            assert_eq!(s.text_styles[&TextStyle::Body].size, 16.0);
            assert!(s.spacing.interact_size.y >= 36.0, "controls are at least 36 px tall");
            assert_eq!(s.visuals.weak_text_color, Some(pal_of(dark).ink2), "weak text is the second ink, not faded ink");
        }
    }

    #[test]
    fn the_built_in_faces_carry_the_text_and_their_weights() {
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        apply(&ctx, true, 1.0);
        ctx.run_ui(egui::RawInput::default(), |_| {}).textures_delta.clear();
        let defs = ctx.fonts(|f| f.definitions().clone());
        for (family, first) in [(FontFamily::Proportional, "next-400"), (bold(), "next-700"), (heavy(), "next-800"), (FontFamily::Monospace, "mono-400"), (mono_bold(), "mono-700")] {
            assert_eq!(defs.families[&family].first().map(String::as_str), Some(first), "{family:?}");
        }
        let mono = egui::FontData::from_static(MONO);
        assert_eq!(mono.variation_axes().first().map(|a| a.default), Some(200.0));
        assert_eq!(defs.font_data["mono-400"].tweak.coords, egui::epaint::text::VariationCoords::new([("wght", 400.0)]));
        let text = "abcdefghijklmnopqrstuvwxyz ABCDEFGHIJKLMNOPQRSTUVWXYZ 0123456789 .,:;!?'\"()[]{}<>/\\|-_+=*&%$#@~ · ± ° Δ − – — … × ÅÄÖåäöÜüßéèçñ";
        for (family, face) in [(FontFamily::Proportional, "next-400"), (mono_bold(), "mono-700")] {
            let held = ctx.fonts_mut(|f| f.fonts.font(&family).characters().clone());
            let missing: String = text.chars().filter(|c| !c.is_whitespace() && held.get(c).and_then(|faces| faces.first()).map(String::as_str) != Some(face)).collect();
            assert!(missing.is_empty(), "not in {face}: {missing:?}");
        }
    }
}
