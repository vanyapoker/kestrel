use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle,
    Visuals, vec2,
};
use std::sync::Arc;

pub const BG: Color32 = Color32::from_rgb(0x16, 0x18, 0x1c);
pub const PANEL: Color32 = Color32::from_rgb(0x1c, 0x1f, 0x24);
pub const SURFACE: Color32 = Color32::from_rgb(0x24, 0x27, 0x2e);
pub const HOVER: Color32 = Color32::from_rgb(0x2a, 0x2e, 0x36);
pub const SELECT: Color32 = Color32::from_rgb(0x2b, 0x45, 0x75);
pub const SELECT_DIM: Color32 = Color32::from_rgb(0x2a, 0x33, 0x44);
pub const ACCENT: Color32 = Color32::from_rgb(0x5b, 0x9b, 0xff);
pub const ACCENT_STRONG: Color32 = Color32::from_rgb(0x2f, 0x6f, 0xe0);
pub const DANGER: Color32 = Color32::from_rgb(0xe0, 0x5a, 0x5a);
pub const TEXT: Color32 = Color32::from_rgb(0xd8, 0xdb, 0xe1);
pub const DIM: Color32 = Color32::from_rgb(0x8d, 0x93, 0x9e);
pub const FAINT: Color32 = Color32::from_rgb(0x58, 0x5e, 0x69);
pub const BORDER: Color32 = Color32::from_rgb(0x2c, 0x30, 0x37);

pub const FOLDER: Color32 = Color32::from_rgb(0xe8, 0xb8, 0x4a);

pub const BODY: f32 = 13.5;

const UI_FONTS: &[&str] = &[
    "/usr/share/fonts/inter/Inter.ttc",
    "/usr/share/fonts/inter/InterVariable.ttf",
    "/usr/share/fonts/TTF/Inter-Regular.ttf",
    "/usr/share/fonts/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/cantarell/Cantarell-VF.otf",
];
const MONO_FONTS: &[&str] = &[
    "/usr/share/fonts/TTF/JetBrainsMono-Regular.ttf",
    "/usr/share/fonts/TTF/JetBrainsMonoNerdFont-Regular.ttf",
    "/usr/share/fonts/TTF/FiraCode-Regular.ttf",
];

pub fn apply(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let mut add = |name: &str, family: FontFamily, paths: &[&str]| {
        if let Some(bytes) = paths.iter().find_map(|p| std::fs::read(p).ok()) {
            fonts.font_data.insert(name.into(), Arc::new(FontData::from_owned(bytes)));
            fonts.families.entry(family).or_default().insert(0, name.into());
        }
    };
    add("ui", FontFamily::Proportional, UI_FONTS);
    add("mono", FontFamily::Monospace, MONO_FONTS);
    ctx.set_fonts(fonts);

    ctx.set_theme(egui::Theme::Dark);
    let mut v = Visuals::dark();
    v.override_text_color = Some(TEXT);
    v.panel_fill = PANEL;
    v.window_fill = PANEL;
    v.extreme_bg_color = BG;
    v.faint_bg_color = SURFACE;
    v.selection.bg_fill = SELECT;
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.hyperlink_color = ACCENT;
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
    v.widgets.inactive.bg_fill = SURFACE;
    v.widgets.inactive.bg_stroke = Stroke::NONE;
    v.widgets.hovered.weak_bg_fill = HOVER;
    v.widgets.hovered.bg_fill = HOVER;
    v.widgets.hovered.bg_stroke = Stroke::NONE;
    v.widgets.active.weak_bg_fill = SELECT;
    v.widgets.active.bg_fill = SELECT;
    v.widgets.active.bg_stroke = Stroke::NONE;
    v.widgets.open.weak_bg_fill = HOVER;
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = CornerRadius::same(5);
    }
    v.window_corner_radius = CornerRadius::same(8);
    v.menu_corner_radius = CornerRadius::same(6);
    ctx.set_visuals(v);

    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = vec2(6.0, 4.0);
        s.spacing.button_padding = vec2(7.0, 3.0);
        s.spacing.interact_size.y = 24.0;
        s.text_styles = [
            (TextStyle::Heading, FontId::proportional(16.0)),
            (TextStyle::Body, FontId::proportional(BODY)),
            (TextStyle::Button, FontId::proportional(BODY)),
            (TextStyle::Small, FontId::proportional(11.5)),
            (TextStyle::Monospace, FontId::monospace(12.5)),
        ]
        .into();
    });
}
