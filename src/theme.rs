//! Design system: the palette and egui styling from `mockup.html`.
//!
//! One place to change how the app looks. UI code references colors through
//! these constants and the helpers below, never through raw literals.

use egui::{Color32, Context, FontId, Vec2, Visuals, style::Selection};

// -- palette (mirrors mockup.html :root) -----------------------------------

/// Window / grid background.
pub const BG_WINDOW: Color32 = Color32::from_rgb(0x16, 0x18, 0x1d);
/// Panels, toolbars, popovers.
pub const BG_PANEL: Color32 = Color32::from_rgb(0x1d, 0x20, 0x27);
/// Table header background.
pub const BG_HEADER: Color32 = Color32::from_rgb(0x20, 0x23, 0x2b);
/// Status bar.
pub const BG_STATUS: Color32 = Color32::from_rgb(0x12, 0x14, 0x1a);
/// Hover highlight inside the grid.
pub const BG_HOVER: Color32 = Color32::from_rgb(0x1f, 0x23, 0x2b);
/// Row selection.
pub const BG_SELECT: Color32 = Color32::from_rgb(0x2b, 0x3a, 0x55);
/// The one selected cell, brighter than the row tint.
pub const BG_SELECT_CELL: Color32 = Color32::from_rgb(0x38, 0x4d, 0x6e);
/// Inline editor background.
pub const BG_EDITOR: Color32 = Color32::from_rgb(0x10, 0x13, 0x1a);
/// Hairlines: cell borders.
pub const LINE: Color32 = Color32::from_rgb(0x26, 0x2a, 0x33);
/// Stronger lines: header underline, widget borders.
pub const LINE_STRONG: Color32 = Color32::from_rgb(0x2f, 0x34, 0x3f);

/// Primary text.
pub const TEXT: Color32 = Color32::from_rgb(0xc8, 0xce, 0xdb);
/// Secondary text: row numbers, hints, labels.
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x5c, 0x64, 0x70);
/// Column headers, button text.
pub const TEXT_HEADER: Color32 = Color32::from_rgb(0xae, 0xb6, 0xc4);

/// The single accent. Edits, sort state, active filters, pending status.
pub const ACCENT: Color32 = Color32::from_rgb(0xe8, 0xa1, 0x3c);
/// Text on accent-tinted surfaces (chips, edited cell text, status).
pub const ACCENT_TEXT: Color32 = Color32::from_rgb(0xf5, 0xc8, 0x77);
/// Tinted accent surface: sort chips, focused widget borders.
pub const ACCENT_SOFT: Color32 = Color32::from_rgb(0x3a, 0x32, 0x22);
pub const ACCENT_BORDER: Color32 = Color32::from_rgb(0x6b, 0x54, 0x2a);
/// Errors in the filter bar / dialogs.
pub const DANGER: Color32 = Color32::from_rgb(0xd9, 0x6c, 0x5f);
/// Null dots.
pub const NULL_DOT: Color32 = TEXT_DIM;

// -- layout tokens ----------------------------------------------------------

/// Row height of the data grid in points.
pub const ROW_HEIGHT: f32 = 24.0;
pub const BUTTON_PADDING: Vec2 = Vec2::new(10.0, 4.0);
pub const ITEM_SPACING: Vec2 = Vec2::new(8.0, 6.0);
/// Corner rounding for buttons, popovers, dialogs.
pub const ROUNDING: f32 = 6.0;

// -- fonts ------------------------------------------------------------------

/// Monospaced family used by the grid and value-bearing UI. Registered in
/// `install_fonts` when the bundled font is present, otherwise egui's default
/// proportional font is used.
pub const MONO: &str = "pq-mono";

pub fn mono(size: f32) -> FontId {
    FontId::proportional(size) // proportional == default family
}

/// Apply the theme to egui. Call once at startup.
pub fn install(ctx: &Context) {
    ctx.all_styles_mut(|style| {
        style.visuals = visuals();
        style.spacing.item_spacing = ITEM_SPACING;
        style.spacing.button_padding = BUTTON_PADDING;
        style.spacing.interact_size = Vec2::new(0.0, 26.0);
        style.spacing.menu_margin = egui::Margin::symmetric(8, 8);
        style.text_styles = [
            (egui::TextStyle::Body, mono(13.0)),
            (egui::TextStyle::Button, mono(13.0)),
            (egui::TextStyle::Small, mono(10.0)),
            (egui::TextStyle::Heading, mono(18.0)),
            (egui::TextStyle::Monospace, mono(13.0)),
        ]
        .into_iter()
        .collect();
    });
}

fn visuals() -> Visuals {
    let mut v = Visuals::dark();
    v.panel_fill = BG_WINDOW;
    v.window_fill = BG_PANEL;
    v.extreme_bg_color = BG_EDITOR;
    v.text_edit_bg_color = Some(BG_EDITOR);
    v.faint_bg_color = BG_HEADER; // striped rows / subtle surfaces
    v.window_stroke = egui::Stroke::new(1.0, LINE_STRONG);
    v.window_corner_radius = egui::CornerRadius::same(ROUNDING as u8);
    v.menu_corner_radius = egui::CornerRadius::same(ROUNDING as u8);
    v.widgets.noninteractive.bg_fill = BG_PANEL;
    v.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, TEXT);
    v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, LINE);
    for state in [
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        state.bg_fill = BG_PANEL;
        state.bg_stroke = egui::Stroke::new(1.0, LINE_STRONG);
        state.fg_stroke = egui::Stroke::new(1.0, TEXT_HEADER);
        state.corner_radius = egui::CornerRadius::same(ROUNDING as u8);
    }
    v.widgets.hovered.bg_fill = BG_HOVER;
    v.widgets.active.bg_fill = ACCENT_SOFT;
    v.widgets.active.bg_stroke = egui::Stroke::new(1.0, ACCENT_BORDER);
    v.widgets.active.fg_stroke = egui::Stroke::new(1.0, ACCENT_TEXT);
    v.selection = Selection {
        bg_fill: BG_SELECT,
        stroke: egui::Stroke::new(1.0, BG_SELECT),
    };
    v.hyperlink_color = ACCENT_TEXT;
    v.override_text_color = Some(TEXT);
    v
}

/// Register the bundled mono font as the default family if the asset is
/// present. Font file: assets/fonts/JetBrainsMono-Regular.ttf (OFL license,
/// see assets/fonts/LICENSE). Missing file is not an error; egui falls back
/// to its default fonts.
pub fn install_fonts(ctx: &Context) {
    const FONT_BYTES: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf");
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        MONO.to_string(),
        egui::FontData::from_static(FONT_BYTES).into(),
    );
    // Default family: our mono first, so everything is monospaced like the
    // mockup; egui's proportional fonts fill in missing glyphs.
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, MONO.to_string());
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .insert(0, MONO.to_string());
    ctx.set_fonts(fonts);
}
