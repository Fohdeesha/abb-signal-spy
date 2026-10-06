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
    pub strong: Color32,
    pub open: Color32,
    pub teal: Color32,
}

pub const DARK: Pal = Pal {
    page: Color32::from_rgb(0x0e, 0x0e, 0x0e),
    sheet: Color32::from_rgb(0x1b, 0x1b, 0x1a),
    line: Color32::from_rgb(0x34, 0x34, 0x32),
    ink: Color32::from_rgb(0xef, 0xee, 0xe9),
    ink2: Color32::from_rgb(0xc9, 0xc8, 0xc2),
    ink3: Color32::from_rgb(0xba, 0xb9, 0xb3),
    red: Color32::from_rgb(0xff, 0x56, 0x46),
    link: Color32::from_rgb(0x8a, 0xb4, 0xff),
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
    strong: Color32::from_rgb(0x6a, 0xa7, 0xff),
    open: Color32::from_rgb(0xd6, 0x9b, 0xd0),
    teal: Color32::from_rgb(0x76, 0xb7, 0xb2),
};

pub const LIGHT: Pal = Pal {
    page: Color32::from_rgb(0xd6, 0xd5, 0xcf),
    sheet: Color32::from_rgb(0xf7, 0xf6, 0xf2),
    line: Color32::from_rgb(0xcf, 0xce, 0xc8),
    ink: Color32::from_rgb(0x14, 0x14, 0x14),
    ink2: Color32::from_rgb(0x3a, 0x39, 0x35),
    ink3: Color32::from_rgb(0x3f, 0x3e, 0x3a),
    red: Color32::from_rgb(0xa8, 0x1c, 0x10),
    link: Color32::from_rgb(0x0b, 0x4f, 0xb3),
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
    strong: Color32::from_rgb(0x17, 0x4a, 0x96),
    open: Color32::from_rgb(0x8a, 0x3d, 0x80),
    teal: Color32::from_rgb(0x14, 0x5a, 0x55),
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

const PLEX: &[u8] = include_bytes!("../fonts/IBMPlexSans-Variable.ttf");
pub const FACE: &str = "IBM Plex Sans";
pub const FACE_LICENCE: &str = include_str!("../fonts/OFL-IBMPlexSans.txt");
pub const FALLBACK_LICENCES: &str = include_str!("../fonts/LICENSES-egui-fonts.txt");
pub const FALLBACK_FACES: &str = "Ubuntu Light (Copyright 2011 Canonical Ltd., Ubuntu Font Licence 1.0), Hack (Copyright 2018 Source Foundry Authors, MIT licence; from Bitstream Vera, Copyright 2003 Bitstream, Inc.), Noto Emoji (Copyright 2013 Google Inc., SIL Open Font License 1.1) and emoji-icon-font (Copyright 2014 John Slegers, MIT licence)";

pub fn face_copyright() -> &'static str {
    FACE_LICENCE.lines().next().unwrap_or_default().trim()
}

pub fn xy_point(dark: bool) -> Color32 {
    channel_color(0, dark)
}

pub fn bold() -> FontFamily {
    FontFamily::Name("bold".into())
}

fn face(bytes: &'static [u8], weight: f32) -> egui::FontData {
    egui::FontData::from_static(bytes).tweak(egui::FontTweak { coords: egui::epaint::text::VariationCoords::new([("wght", weight)]), ..Default::default() })
}

pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let egui_fallbacks: Vec<String> = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    for (name, weight) in [("plex-400", 400.0), ("plex-700", 700.0)] {
        fonts.font_data.insert(name.into(), std::sync::Arc::new(face(PLEX, weight)));
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
    fonts.families.insert(FontFamily::Proportional, family("plex-400"));
    fonts.families.insert(bold(), family("plex-700"));
    fonts.families.insert(FontFamily::Monospace, family("plex-400"));
    ctx.set_fonts(fonts);
}

pub fn b(text: impl Into<String>) -> RichText {
    RichText::new(text).family(bold())
}

pub fn num(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text).font(FontId::new(size, bold()))
}

pub fn fit_size(ui: &egui::Ui, text: &str, width: f32, sizes: &[f32]) -> f32 {
    let smallest = sizes.last().copied().unwrap_or(14.0);
    sizes.iter().copied().find(|&s| ui.fonts_mut(|f| f.layout_no_wrap(text.to_string(), FontId::new(s, bold()), Color32::WHITE).size().x) <= width).unwrap_or(smallest)
}

pub fn keep_together(text: &str) -> String {
    text.replace("(every ", "(every\u{a0}").replace(" s)", "\u{a0}s)")
}

pub const TOOL_H: f32 = 36.0;
pub const SMALL_H: f32 = TOOL_H;

fn style(dark: bool) -> egui::Style {
    let p = pal_of(dark);
    let mut s = egui::Style {
        text_styles: [
            (TextStyle::Small, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(16.0, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(16.0, bold())),
            (TextStyle::Monospace, FontId::new(16.0, FontFamily::Monospace)),
            (TextStyle::Heading, FontId::new(20.0, bold())),
        ]
        .into(),
        drag_value_text_style: TextStyle::Body,
        ..Default::default()
    };
    s.interaction.selectable_labels = false;
    let sp = &mut s.spacing;
    sp.item_spacing = vec2(8.0, 6.0);
    sp.button_padding = vec2(BUTTON_PAD, 6.0);
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
    sp.scroll.bar_width = 16.0;
    sp.scroll.handle_min_length = 40.0;
    sp.scroll.foreground_color = true;

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
    v.faint_bg_color = if dark { Color32::from_rgb(0x2a, 0x2a, 0x28) } else { Color32::from_rgb(0xe6, 0xe5, 0xdf) };
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
    Up,
    Right,
    Left,
    FoldLeft,
    Expand,
    Collapse,
    Plus,
    Minus,
    Close,
    Grid,
    Swap,
}

pub fn paint_icon(painter: &egui::Painter, rect: Rect, icon: Icon, color: Color32) {
    let c = rect.center();
    let st = Stroke::new(2.0, color);
    let line = |pts: &[(f32, f32)]| egui::Shape::line(pts.iter().map(|(x, y)| pos2(c.x + x, c.y + y)).collect(), st);
    let shapes = match icon {
        Icon::Down => vec![line(&[(-5.0, -2.5), (0.0, 2.5), (5.0, -2.5)])],
        Icon::Up => vec![line(&[(-5.0, 2.5), (0.0, -2.5), (5.0, 2.5)])],
        Icon::Right => vec![line(&[(-2.5, -5.0), (2.5, 0.0), (-2.5, 5.0)])],
        Icon::Left => vec![line(&[(2.5, -5.0), (-2.5, 0.0), (2.5, 5.0)])],
        Icon::FoldLeft => vec![line(&[(-0.5, -5.0), (-5.0, 0.0), (-0.5, 5.0)]), line(&[(5.5, -5.0), (1.0, 0.0), (5.5, 5.0)])],
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
        Icon::Swap => vec![
            line(&[(-6.0, -2.5), (6.0, -2.5)]),
            line(&[(3.0, -5.5), (6.0, -2.5), (3.0, 0.5)]),
            line(&[(6.0, 2.5), (-6.0, 2.5)]),
            line(&[(-3.0, -0.5), (-6.0, 2.5), (-3.0, 5.5)]),
        ],
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

pub fn make_room(ui: &mut egui::Ui, natural: f32) {
    let row_started = ui.cursor().left() > ui.max_rect().left() + 0.5;
    if ui.layout().main_wrap && row_started && ui.available_size_before_wrap().x < natural {
        ui.end_row();
    }
}

pub fn lockable(ui: &mut egui::Ui, enabled: bool, button: egui::Button<'_>) -> egui::Response {
    if enabled && ui.is_enabled() {
        return ui.add(button);
    }
    let under = ui.painter().add(egui::Shape::Noop);
    let r = ui.add_enabled(false, unframed(button));
    paint_locked(ui, under, r.rect);
    r
}

fn unframed(button: egui::Button<'_>) -> egui::Button<'_> {
    button.fill(Color32::TRANSPARENT).stroke(Stroke::NONE)
}

pub fn paint_locked(ui: &egui::Ui, under: egui::layers::ShapeIdx, rect: Rect) {
    let p = pal(ui);
    ui.painter().set(under, egui::Shape::rect_filled(rect, 0.0, p.off_face));
    dashed_rect(ui.painter(), rect, Stroke::new(1.0, p.off_edge));
}

pub fn primary(ui: &mut egui::Ui, text: &str, height: f32) -> egui::Response {
    primary_sized(ui, text, vec2(0.0, height))
}

pub fn primary_sized(ui: &mut egui::Ui, text: &str, size: Vec2) -> egui::Response {
    let p = pal(ui);
    filled(ui, egui::Button::new(text).min_size(size), (p.primary, p.primary_hover, p.primary_pressed), p.on_primary)
}

pub fn save_menu(ui: &mut egui::Ui, csv_tip: &str, png_tip: &str) -> (bool, bool) {
    let (mut csv, mut png) = (false, false);
    let r = drop_button(ui, "save", TOOL_H).on_hover_text("Save the samples in view (CSV) or a picture of the charts (PNG) to the recordings folder");
    egui::Popup::menu(&r).show(|ui| {
        if ui.add(egui::Button::new("save csv").min_size(vec2(220.0, TOOL_H))).on_hover_text(csv_tip).clicked() {
            csv = true;
            ui.close();
        }
        if ui.add(egui::Button::new("save png").min_size(vec2(220.0, TOOL_H))).on_hover_text(png_tip).clicked() {
            png = true;
            ui.close();
        }
    });
    (csv, png)
}

pub fn save_menu_width(ui: &egui::Ui) -> f32 {
    button_width(ui, "save") + 12.0 + ui.spacing().icon_spacing
}

pub fn widest_button(ui: &egui::Ui, texts: &[&str]) -> f32 {
    texts.iter().map(|t| button_width(ui, t)).fold(0.0, f32::max)
}

pub fn red_button(ui: &mut egui::Ui, button: egui::Button<'_>) -> egui::Response {
    let p = pal(ui);
    filled(ui, button, (p.panic, p.panic_hover, p.panic_pressed), p.on_panic)
}

fn filled(ui: &mut egui::Ui, button: egui::Button<'_>, (rest, hover, pressed): (Color32, Color32, Color32), text: Color32) -> egui::Response {
    if !ui.is_enabled() {
        return lockable(ui, false, button);
    }
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
    drop_button_text(ui, egui::WidgetText::from(text), height)
}

const CHEVRON: Vec2 = vec2(12.0, 10.0);

pub fn small_drop_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    drop_button_text(ui, b(text).into(), SMALL_H)
}

fn drop_button_text(ui: &mut egui::Ui, text: egui::WidgetText, height: f32) -> egui::Response {
    let natural = text.clone().into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, TextStyle::Button).size().x + 2.0 * ui.spacing().button_padding.x + ui.spacing().icon_spacing + CHEVRON.x;
    make_room(ui, natural);
    let id = ui.next_auto_id().with("chevron");
    let enabled = ui.is_enabled();
    let under = ui.painter().add(egui::Shape::Noop);
    let button = egui::Button::new((text, egui::Atom::custom(id, CHEVRON))).min_size(vec2(0.0, height));
    let r = if enabled { button } else { unframed(button) }.atom_ui(ui);
    if !enabled {
        paint_locked(ui, under, r.response.rect);
    }
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

const BUTTON_ICON: f32 = 16.0;

pub fn icon_text_button_width(ui: &egui::Ui, text: &str) -> f32 {
    button_width(ui, text) + BUTTON_ICON + ui.spacing().icon_spacing
}

pub fn heading_width(ui: &egui::Ui, text: &str) -> f32 {
    egui::WidgetText::from(RichText::new(text).font(FontId::new(18.0, bold()))).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, TextStyle::Body).size().x
}

pub fn icon_text_button(ui: &mut egui::Ui, icon: Icon, text: &str, height: f32, on: bool) -> egui::Response {
    make_room(ui, icon_text_button_width(ui, text));
    let id = ui.next_auto_id().with("icon");
    let p = pal(ui);
    let button = egui::Button::new((egui::Atom::custom(id, vec2(BUTTON_ICON, BUTTON_ICON)), text)).min_size(vec2(0.0, height));
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

pub fn window_room(ctx: &egui::Context) -> f32 {
    (ctx.content_rect().height() - 90.0).max(200.0)
}

pub fn window<'a>(title: impl Into<egui::WidgetText>, ctx: &egui::Context) -> egui::Window<'a> {
    egui::Window::new(title).collapsible(false).max_height(window_room(ctx)).pivot(egui::Align2::CENTER_CENTER).default_pos(ctx.content_rect().center())
}

pub fn legend() -> egui_plot::Legend {
    egui_plot::Legend::default().position(egui_plot::Corner::LeftTop).background_alpha(1.0).hidden_items(std::iter::empty::<egui::Id>())
}

pub fn value_axis<'a>(p: &Pal) -> egui_plot::AxisHints<'a> {
    let px = crate::charts::VALUE_LABEL_PX;
    egui_plot::AxisHints::new_y().formatter(|mark, _| crate::charts::value_label(mark)).label_spacing((px - 1.0)..=px).min_thickness(52.0).tick_label_font(FontId::monospace(14.0)).tick_label_color(p.ink2)
}

pub fn value_axis_across<'a>(p: &Pal) -> egui_plot::AxisHints<'a> {
    let px = crate::charts::VALUE_LABEL_ACROSS_PX;
    egui_plot::AxisHints::new_x().formatter(|mark, _| crate::charts::value_label(mark)).label_spacing((px - 1.0)..=px).tick_label_font(FontId::monospace(14.0)).tick_label_color(p.ink2)
}

pub fn collapse_icon(ui: &mut egui::Ui, openness: f32, response: &egui::Response) {
    let icon = if openness > 0.5 { Icon::Down } else { Icon::Right };
    let color = ui.style().interact(response).fg_stroke.color;
    paint_icon(ui.painter(), Rect::from_center_size(response.rect.left_center() + vec2(10.0, 0.0), vec2(12.0, 12.0)), icon, color);
}

pub const COMBO_ICON_W: f32 = 14.0;
pub const COMBO_ICON_GAP: f32 = 6.0;

pub fn combo_icon(ui: &egui::Ui, rect: Rect, visuals: &egui::style::WidgetVisuals, _open: bool) {
    paint_icon(ui.painter(), Rect::from_center_size(rect.center(), vec2(12.0, 10.0)), Icon::Down, visuals.fg_stroke.color);
}

pub const VRULE_W: f32 = 9.0;

pub fn vrule(ui: &mut egui::Ui, height: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(VRULE_W, height), Sense::hover());
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

pub fn upright_label(ui: &mut egui::Ui, text: &str, color: Color32) -> egui::Response {
    let galley = egui::WidgetText::from(b(text).size(15.0)).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, TextStyle::Button);
    let (w, h) = (galley.size().x, galley.size().y);
    let (rect, r) = ui.allocate_exact_size(vec2(ui.available_width(), w), Sense::hover());
    r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, text));
    let at = pos2(rect.center().x - h / 2.0, rect.bottom());
    ui.painter().add(egui::epaint::TextShape::new(at, galley, color).with_angle(-std::f32::consts::FRAC_PI_2));
    r
}

pub fn check(ui: &mut egui::Ui, on: &mut bool, text: impl Into<egui::WidgetText>) -> egui::Response {
    let edge = Stroke::new(1.0, pal(ui).ink);
    ui.scope(|ui| {
        let w = &mut ui.visuals_mut().widgets;
        for v in [&mut w.inactive, &mut w.hovered, &mut w.active] {
            v.bg_stroke = edge;
        }
        ui.checkbox(on, text)
    })
    .inner
}

pub fn chip(ui: &mut egui::Ui, on: bool, text: &str) -> egui::Response {
    chip_sized(ui, on, b(text), (24.0, TOOL_H))
}

pub fn chip_tall(ui: &mut egui::Ui, on: bool, text: &str) -> egui::Response {
    chip_sized(ui, on, b(text), (24.0, 40.0))
}

pub fn chip_tall_width(ui: &egui::Ui, text: &str) -> f32 {
    egui::WidgetText::from(b(text)).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, TextStyle::Button).size().x + 24.0
}

pub const CHIP_PAD: f32 = 18.0;
pub const BUTTON_PAD: f32 = 12.0;
pub const LIST_LEAST_H: f32 = 60.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChipFit {
    pub pad: f32,
    pub button_pad: f32,
    pub gap: f32,
}

pub const CHIP_FITS: [ChipFit; 3] = [ChipFit { pad: CHIP_PAD, button_pad: BUTTON_PAD, gap: 6.0 }, ChipFit { pad: 10.0, button_pad: 6.0, gap: 6.0 }, ChipFit { pad: 6.0, button_pad: 4.0, gap: 4.0 }];

pub fn small_chip(ui: &mut egui::Ui, on: bool, text: &str) -> egui::Response {
    small_chip_padded(ui, on, text, CHIP_PAD)
}

pub fn small_chip_padded(ui: &mut egui::Ui, on: bool, text: &str, pad: f32) -> egui::Response {
    chip_sized(ui, on, b(text), (pad, SMALL_H))
}

pub fn bold_text_w(ui: &egui::Ui, text: &str) -> f32 {
    egui::WidgetText::from(b(text)).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, TextStyle::Button).size().x
}

pub fn drop_button_width(ui: &egui::Ui, text: &str, padding: f32) -> f32 {
    bold_text_w(ui, text) + 2.0 * padding + ui.spacing().icon_spacing + CHEVRON.x
}

fn chip_sized(ui: &mut egui::Ui, on: bool, text: RichText, (pad, height): (f32, f32)) -> egui::Response {
    let p = pal(ui);
    let galley = egui::WidgetText::from(text).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, TextStyle::Button);
    let size = vec2(galley.size().x + pad, height);
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
            ui.painter().rect_stroke(rect, 0.0, Stroke::new(1.0, p.field_edge_hover), egui::StrokeKind::Inside);
            p.ink
        };
        ui.painter().galley(rect.center() - galley.size() / 2.0, galley, text_color);
    }
    r
}

pub const SPLIT_ARROW_W: f32 = 32.0;

pub fn split(ui: &mut egui::Ui, arrow_name: &str, main_w: f32, main: impl FnOnce(&mut egui::Ui) -> egui::Response, settings: impl FnOnce(&mut egui::Ui, bool)) -> egui::Response {
    make_room(ui, main_w + SPLIT_ARROW_W);
    let r = main(ui);
    ui.add_space(-ui.spacing().item_spacing.x);
    let arrow = icon_button(ui, Icon::Down, arrow_name, vec2(SPLIT_ARROW_W, r.rect.height()));
    let opened = arrow.clicked();
    egui::Popup::from_toggle_button_response(&arrow).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        ui.set_min_width(260.0);
        settings(ui, opened);
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
    if !number.is_empty() {
        ui.label(RichText::new(number).font(FontId::new(18.0, bold())).color(p.red));
    }
    if !title.is_empty() {
        ui.label(RichText::new(title).font(FontId::new(18.0, bold())).color(p.ink));
    }
}

pub fn section_tools(ui: &mut egui::Ui, number: &str, title: &str, left_w: f32, right_w: f32, left: impl FnOnce(&mut egui::Ui), right: impl FnOnce(&mut egui::Ui)) {
    let p = pal(ui);
    let title_w = [number, title].iter().filter(|t| !t.is_empty()).map(|t| egui::WidgetText::from(RichText::new(*t).font(FontId::new(18.0, bold()))).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, TextStyle::Body).size().x + ui.spacing().item_spacing.x).sum::<f32>();
    let one_row = title_w + left_w + right_w + 24.0 <= ui.available_width();
    let mut right = Some(right);
    ui.horizontal_wrapped(|ui| {
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

pub fn pair_fits(ui: &egui::Ui, texts: [&str; 2]) -> bool {
    texts.iter().map(|t| button_width(ui, t)).sum::<f32>() + ui.spacing().item_spacing.x <= ui.available_width()
}

pub fn pair(ui: &mut egui::Ui, texts: [&str; 2], height: f32, mut add: impl FnMut(&mut egui::Ui, usize, Vec2)) {
    let room = ui.available_width();
    let half = ((room - ui.spacing().item_spacing.x) / 2.0).floor();
    if texts.iter().all(|t| button_width(ui, t) <= half) {
        ui.horizontal(|ui| {
            for k in 0..2 {
                add(ui, k, vec2(half, height));
            }
        });
    } else if pair_fits(ui, texts) {
        ui.horizontal(|ui| {
            for k in 0..2 {
                add(ui, k, vec2(0.0, height));
            }
        });
    } else {
        for k in 0..2 {
            add(ui, k, vec2(room, height));
        }
    }
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
}

pub fn badge(ui: &mut egui::Ui, text: &str, color: Color32, tip: &str) -> egui::Response {
    let galley = egui::WidgetText::from(b(text).size(14.0)).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, TextStyle::Small);
    let (rect, r) = ui.allocate_exact_size(galley.size() + vec2(12.0, 2.0), Sense::hover());
    r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, galley.text()));
    ui.painter().rect_stroke(rect, 0.0, Stroke::new(1.0, color), egui::StrokeKind::Inside);
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, color);
    if tip.is_empty() { r } else { r.on_hover_text(tip) }
}

pub const CHIP_H: f32 = 28.0;

pub fn badge_button_width(ui: &egui::Ui, text: &str) -> f32 {
    egui::WidgetText::from(b(text).size(14.0)).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, TextStyle::Small).size().x + 10.0 + 6.0 + 14.0 + 8.0
}

pub fn badge_button(ui: &mut egui::Ui, text: &str, color: Color32, tip: &str) -> egui::Response {
    let galley = egui::WidgetText::from(b(text).size(14.0)).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, TextStyle::Small);
    let mark = 14.0;
    let (rect, r) = ui.allocate_exact_size(vec2(10.0 + galley.size().x + 6.0 + mark + 8.0, CHIP_H), Sense::click());
    r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), galley.text()));
    if ui.is_rect_visible(rect) {
        let p = pal(ui);
        let (fill, edge) = if r.is_pointer_button_down_on() { (p.face_pressed, 2.0) } else if r.hovered() { (p.face_hover, 2.0) } else { (Color32::TRANSPARENT, 1.0) };
        ui.painter().rect(rect, 0.0, fill, Stroke::new(edge, color), egui::StrokeKind::Inside);
        ui.painter().galley(pos2(rect.left() + 10.0, rect.center().y - galley.size().y / 2.0), galley, color);
        let at = Rect::from_center_size(pos2(rect.right() - 8.0 - mark / 2.0, rect.center().y), vec2(mark, mark));
        paint_icon(ui.painter(), at, Icon::Close, color);
    }
    r.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tip)
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
            assert!(p.link.b() > p.link.r(), "{name}: a link reads as an error when it is red");
            for (word, t) in [("red", p.red), ("link", p.link), ("live", p.live), ("hold", p.hold), ("strong", p.strong), ("open", p.open), ("teal", p.teal)] {
                for (bg, g) in [("sheet", p.sheet), ("page", p.page), ("stale face", p.stale_face)] {
                    let c = contrast(t, g);
                    assert!(c >= 4.5, "{name}: {word} on {bg} is {c:.2}:1");
                }
            }
            let faint = style(name == "dark").visuals.faint_bg_color;
            assert!(contrast(faint, p.sheet) >= 1.15, "{name}: striped rows do not show: {:.2}:1", contrast(faint, p.sheet));
            for t in [p.ink, p.ink2, p.ink3] {
                assert!(contrast(t, faint) >= 7.0, "{name}: text on a stripe is {:.2}:1", contrast(t, faint));
            }
            let a = crate::charts::STALE_SHADE;
            let mix = |h: u8, g: u8| (f32::from(h) * a + f32::from(g) * (1.0 - a)).round() as u8;
            let shaded = Color32::from_rgb(mix(p.hold.r(), p.plot.r()), mix(p.hold.g(), p.plot.g()), mix(p.hold.b(), p.plot.b()));
            assert!(contrast(shaded, p.plot) >= 1.35, "{name}: the stale shading is {:.2}:1 on the plot", contrast(shaded, p.plot));
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

    fn family_name(font: &[u8]) -> Option<String> {
        let u16_at = |i: usize| Some(u16::from_be_bytes([*font.get(i)?, *font.get(i + 1)?]));
        let u32_at = |i: usize| Some(u32::from_be_bytes(font.get(i..i + 4)?.try_into().ok()?));
        let tables = u16_at(4)? as usize;
        let name = (0..tables).map(|t| 12 + 16 * t).find(|&r| font.get(r..r + 4) == Some(b"name".as_slice()))?;
        let at = u32_at(name + 8)? as usize;
        let (count, strings) = (u16_at(at + 2)? as usize, at + u16_at(at + 4)? as usize);
        (0..count).map(|k| at + 6 + 12 * k).find_map(|r| {
            let (platform, id, len, off) = (u16_at(r)?, u16_at(r + 6)?, u16_at(r + 8)? as usize, u16_at(r + 10)? as usize);
            if platform != 3 || id != 1 {
                return None;
            }
            let units: Vec<u16> = font.get(strings + off..strings + off + len)?.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
            String::from_utf16(&units).ok()
        })
    }

    #[test]
    fn the_face_credited_is_the_face_embedded_with_its_notice() {
        assert_eq!(family_name(PLEX).as_deref(), Some(FACE), "the embedded font is not the one credited");
        assert_eq!(face_copyright(), "Copyright © 2017 IBM Corp. with Reserved Font Name \"Plex\"");
        assert!(FACE_LICENCE.contains("SIL OPEN FONT LICENSE Version 1.1"));
        for face in ["Ubuntu", "Hack", "Noto Emoji", "emoji-icon-font"] {
            assert!(FALLBACK_FACES.contains(face) && FALLBACK_LICENCES.contains(face), "{face}");
        }
        assert!(FALLBACK_LICENCES.contains("UBUNTU FONT LICENCE") && FALLBACK_LICENCES.contains("BITSTREAM VERA LICENSE") && FALLBACK_LICENCES.contains("SIL OPEN FONT LICENSE"));
        let fallbacks = egui::FontDefinitions::default().font_data.into_keys().collect::<Vec<_>>();
        for name in &fallbacks {
            let credited = [("Ubuntu", "Ubuntu"), ("Hack", "Hack"), ("NotoEmoji", "Noto Emoji"), ("emoji-icon-font", "emoji-icon-font")].iter().any(|(k, c)| name.contains(k) && FALLBACK_FACES.contains(c));
            assert!(credited, "egui compiles in {name}, which is not credited");
        }
    }

    #[test]
    fn the_window_wears_the_programs_own_icon() {
        let icon = eframe::icon_data::from_png_bytes(crate::WINDOW_ICON).expect("the window icon decodes");
        assert_eq!((icon.width, icon.height), (64, 64));
        let alpha = |x: u32, y: u32| icon.rgba[((y * icon.width + x) * 4 + 3) as usize];
        assert_eq!(alpha(0, 0), 0, "the tile's rounded corner is see-through");
        assert_eq!(alpha(32, 32), 255);
        let viewport = crate::options(eframe::Renderer::Glow).viewport;
        assert_eq!(viewport.icon.map(|i| (i.width, i.height)), Some((64, 64)), "the window opens with egui's own icon");
    }

    #[cfg(windows)]
    #[test]
    fn the_program_file_carries_its_icon_for_explorer() {
        use windows_sys::Win32::System::LibraryLoader::{FindResourceW, GetModuleHandleW};
        use windows_sys::Win32::UI::WindowsAndMessaging::RT_GROUP_ICON;
        let first_icon_group = 1usize as windows_sys::core::PCWSTR;
        let found = unsafe { FindResourceW(GetModuleHandleW(std::ptr::null()), first_icon_group, RT_GROUP_ICON) };
        assert!(!found.is_null(), "the program file has no icon, so Explorer and the taskbar show a blank one");
    }

    #[test]
    fn the_exe_icon_holds_every_size_windows_asks_for() {
        let ico: &[u8] = include_bytes!("../assets/icon.ico");
        let u16_at = |i: usize| u16::from_le_bytes([ico[i], ico[i + 1]]);
        let u32_at = |i: usize| u32::from_le_bytes([ico[i], ico[i + 1], ico[i + 2], ico[i + 3]]);
        assert_eq!((u16_at(0), u16_at(2)), (0, 1), "an icon file");
        let mut sizes = Vec::new();
        for i in 0..usize::from(u16_at(4)) {
            let at = 6 + 16 * i;
            let side = if ico[at] == 0 { 256 } else { u32::from(ico[at]) };
            let (len, offset) = (u32_at(at + 8) as usize, u32_at(at + 12) as usize);
            let png = &ico[offset..offset + len];
            let image = eframe::icon_data::from_png_bytes(png).expect("each size decodes");
            assert_eq!((image.width, image.height), (side, side), "the directory and the picture disagree");
            sizes.push(side);
        }
        for side in [16, 20, 24, 32, 40, 48, 64, 256] {
            assert!(sizes.contains(&side), "no {side} px picture: {sizes:?}");
        }
    }

    #[test]
    fn a_scroll_bar_is_plain_to_see_and_to_grab() {
        for (name, dark, p) in [("dark", true, DARK), ("light", false, LIGHT)] {
            let s = style(dark);
            let bar = &s.spacing.scroll;
            assert!(!bar.floating, "{name}: a floating bar hides until the pointer finds it");
            assert!(bar.bar_width >= 16.0, "{name}: {} px is hard to grab on a touchpad", bar.bar_width);
            let track = s.visuals.extreme_bg_color;
            let w = &s.visuals.widgets;
            for (state, v) in [("at rest", &w.inactive), ("hovered", &w.hovered), ("dragged", &w.active)] {
                let handle = if bar.foreground_color { v.fg_stroke.color } else { v.bg_fill };
                for (under, g) in [("its track", track), ("a sheet", p.sheet), ("the page", p.page)] {
                    let c = contrast(handle, g);
                    assert!(c >= 4.5, "{name}: the scroll handle {state} on {under} is {c:.2}:1");
                }
            }
        }
    }

    #[test]
    fn the_xy_points_stand_out_on_the_plot() {
        for (dark, p) in [(true, DARK), (false, LIGHT)] {
            assert!(contrast(xy_point(dark), p.plot) >= 3.0, "{:.2}:1", contrast(xy_point(dark), p.plot));
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
        for (family, first) in [(FontFamily::Proportional, "plex-400"), (bold(), "plex-700"), (FontFamily::Monospace, "plex-400")] {
            assert_eq!(defs.families[&family].first().map(String::as_str), Some(first), "{family:?}");
        }
        let weight = egui::FontData::from_static(PLEX).variation_axes().into_iter().find(|a| format!("{}", a.tag) == "wght").expect("a weight axis");
        assert!(weight.range.min <= 400.0 && weight.range.max >= 700.0 && weight.default != 700.0, "{weight:?}");
        for (face, weight) in [("plex-400", 400.0), ("plex-700", 700.0)] {
            assert_eq!(defs.font_data[face].tweak.coords, egui::epaint::text::VariationCoords::new([("wght", weight)]), "{face}");
        }
        let text = "abcdefghijklmnopqrstuvwxyz ABCDEFGHIJKLMNOPQRSTUVWXYZ 0123456789 .,:;!?'\"()[]{}<>/\\|-_+=*&%$#@~ · ± ° Δ − – — … × ÅÄÖåäöÜüßéèçñ";
        for (family, face) in [(FontFamily::Proportional, "plex-400"), (bold(), "plex-700")] {
            let held = ctx.fonts_mut(|f| f.fonts.font(&family).characters().clone());
            let missing: String = text.chars().filter(|c| !c.is_whitespace() && held.get(c).and_then(|faces| faces.first()).map(String::as_str) != Some(face)).collect();
            assert!(missing.is_empty(), "not in {face}: {missing:?}");
        }
    }

    #[test]
    fn every_digit_is_as_wide_as_the_others_so_a_live_value_never_jitters() {
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        apply(&ctx, true, 1.0);
        ctx.run_ui(egui::RawInput::default(), |_| {}).textures_delta.clear();
        for family in [FontFamily::Proportional, bold()] {
            for size in [14.0, 28.0, 64.0] {
                let widths: Vec<f32> = ('0'..='9').map(|d| ctx.fonts_mut(|f| f.layout_no_wrap(d.to_string(), FontId::new(size, family.clone()), Color32::WHITE).size().x)).collect();
                assert!(widths.iter().all(|w| (w - widths[0]).abs() < 0.01), "{family:?} at {size} px: {widths:?}");
            }
        }
    }

    #[test]
    fn a_retry_interval_wraps_whole_or_not_at_all() {
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        apply(&ctx, true, 1.0);
        let word = keep_together("reconnecting, try 12 (every 30 s)");
        let mut rows = Vec::new();
        ctx.run_ui(egui::RawInput::default(), |ui| {
            for width in [150.0, 200.0, 240.0] {
                let job = egui::text::LayoutJob::simple(word.clone(), FontId::new(14.0, bold()), Color32::WHITE, width);
                let galley = ui.fonts_mut(|f| f.layout_job(job));
                rows.push(galley.rows.iter().map(|r| r.text()).collect::<Vec<_>>());
            }
        })
        .textures_delta
        .clear();
        for lines in rows {
            assert!(lines.iter().skip(1).all(|l| !l.contains("s)") || l.trim_start().starts_with("(every")), "{lines:?}");
            assert!(lines.iter().all(|l| !l.trim().starts_with('s') && !l.trim().starts_with("30")), "{lines:?}");
        }
    }
}
