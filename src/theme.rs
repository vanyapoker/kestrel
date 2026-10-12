use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle,
    Visuals, vec2,
};
use crate::util::Category;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicPtr, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, channel};

/// Colors the UI draws with. Built from the current Omarchy theme when there is one.
#[derive(Clone, PartialEq, Debug)]
pub struct Palette {
    pub dark: bool,
    pub bg: Color32,
    pub panel: Color32,
    pub surface: Color32,
    pub hover: Color32,
    pub select: Color32,
    pub select_dim: Color32,
    pub accent: Color32,
    pub accent_strong: Color32,
    pub danger: Color32,
    pub text: Color32,
    pub dim: Color32,
    pub faint: Color32,
    pub border: Color32,
    pub folder: Color32,
    pub emblem: Color32,
    /// Checkerboard behind transparent images.
    pub checker: (Color32, Color32),
    pub image: Color32,
    pub video: Color32,
    pub audio: Color32,
    pub archive: Color32,
    pub code: Color32,
    pub document: Color32,
    pub executable: Color32,
    /// Syntax colors: comment, keyword, string, number, function, type.
    pub syntax: [Color32; 6],
}

impl Palette {
    /// fpilot's own dark look, used when no Omarchy theme is found.
    pub fn builtin() -> Self {
        let rgb = Color32::from_rgb;
        Self {
            dark: true,
            bg: rgb(0x16, 0x18, 0x1c),
            panel: rgb(0x1c, 0x1f, 0x24),
            surface: rgb(0x24, 0x27, 0x2e),
            hover: rgb(0x2a, 0x2e, 0x36),
            select: rgb(0x2b, 0x45, 0x75),
            select_dim: rgb(0x2a, 0x33, 0x44),
            accent: rgb(0x5b, 0x9b, 0xff),
            accent_strong: rgb(0x2f, 0x6f, 0xe0),
            danger: rgb(0xe0, 0x5a, 0x5a),
            text: rgb(0xd8, 0xdb, 0xe1),
            dim: rgb(0x8d, 0x93, 0x9e),
            faint: rgb(0x58, 0x5e, 0x69),
            border: rgb(0x2c, 0x30, 0x37),
            folder: rgb(0xe8, 0xb8, 0x4a),
            emblem: rgb(0x8f, 0x67, 0x14),
            checker: (Color32::from_gray(0x2a), Color32::from_gray(0x33)),
            image: rgb(0xc6, 0x78, 0xdd),
            video: rgb(0xe0, 0x6c, 0x75),
            audio: rgb(0x56, 0xb6, 0xc2),
            archive: rgb(0xd1, 0x9a, 0x66),
            code: rgb(0x61, 0xaf, 0xef),
            document: rgb(0xe5, 0xc0, 0x7b),
            executable: rgb(0x98, 0xc3, 0x79),
            syntax: [
                rgb(0x65, 0x73, 0x7e),
                rgb(0xb4, 0x8e, 0xad),
                rgb(0xa3, 0xbe, 0x8c),
                rgb(0xd0, 0x87, 0x70),
                rgb(0x8f, 0xa1, 0xb3),
                rgb(0xeb, 0xcb, 0x8b),
            ],
        }
    }

    /// Maps an Omarchy `colors.toml` onto the palette. Missing keys fall back sensibly.
    pub fn from_omarchy(toml: &str) -> Option<Self> {
        let mut map = std::collections::HashMap::new();
        let mut dark = true;
        for line in toml.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim().trim_matches('"').trim_matches('\''));
            if k == "mode" {
                dark = v != "light";
            } else if let Some(c) = parse_hex(v) {
                map.insert(k.to_string(), c);
            }
        }
        let bg = *map.get("background")?;
        let fg = *map.get("foreground")?;
        let get = |k: &str, or: Color32| map.get(k).copied().unwrap_or(or);
        let accent = get("accent", get("blue", fg));
        let panel = get("dark_background", mix(bg, Color32::BLACK, 0.2));
        let yellow = get("yellow", Color32::from_rgb(0xe8, 0xb8, 0x4a));
        let red = get("red", Color32::from_rgb(0xe0, 0x5a, 0x5a));
        let comment = get("dark_foreground", mix(fg, bg, 0.5));
        Some(Self {
            dark,
            bg,
            panel,
            surface: get("lighter_background", mix(bg, fg, 0.06)),
            hover: mix(bg, fg, if dark { 0.08 } else { 0.07 }),
            select: mix(bg, accent, if dark { 0.30 } else { 0.22 }),
            select_dim: mix(bg, accent, 0.12),
            accent,
            accent_strong: mix(accent, if dark { Color32::BLACK } else { Color32::WHITE }, 0.2),
            danger: red,
            text: fg,
            dim: mix(fg, bg, 0.3),
            faint: mix(fg, bg, 0.55),
            border: mix(panel, fg, 0.1),
            folder: yellow,
            emblem: mix(yellow, Color32::BLACK, 0.42),
            checker: (mix(bg, fg, 0.08), mix(bg, fg, 0.14)),
            image: get("magenta", accent),
            video: red,
            audio: get("cyan", accent),
            archive: get("orange", yellow),
            code: get("blue", accent),
            document: yellow,
            executable: get("green", accent),
            syntax: [
                comment,
                get("magenta", accent),
                get("green", fg),
                get("orange", yellow),
                get("blue", accent),
                get("cyan", yellow),
            ],
        })
    }
}

fn parse_hex(v: &str) -> Option<Color32> {
    let h = v.strip_prefix('#')?;
    if h.len() != 6 {
        return None;
    }
    let n = u32::from_str_radix(h, 16).ok()?;
    Some(Color32::from_rgb((n >> 16) as u8, (n >> 8) as u8, n as u8))
}

/// Linear blend from `a` to `b`.
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

static PALETTE: AtomicPtr<Palette> = AtomicPtr::new(std::ptr::null_mut());
static GENERATION: AtomicU64 = AtomicU64::new(0);

/// The current palette. Cheap enough to call per widget.
pub fn pal() -> &'static Palette {
    let p = PALETTE.load(Ordering::Acquire);
    if p.is_null() {
        set_palette(Palette::builtin());
        return pal();
    }
    // SAFETY: palettes are leaked on install and never freed, so the pointer stays valid.
    unsafe { &*p }
}

/// Bumps on every palette change, so caches of themed output can tell they are stale.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

fn set_palette(p: Palette) {
    // Old palettes leak on purpose: a few hundred bytes per theme switch, and
    // other threads may still hold references to them.
    PALETTE.store(Box::into_raw(Box::new(p)), Ordering::Release);
    GENERATION.fetch_add(1, Ordering::AcqRel);
}

/// Where Omarchy keeps the active theme.
fn omarchy_dir() -> PathBuf {
    crate::fsops::home_dir().join(".local/state/omarchy/current")
}

fn load_palette() -> Palette {
    // FPILOT_OMARCHY_THEME=<theme folder> previews a theme without switching the desktop.
    let dir = std::env::var_os("FPILOT_OMARCHY_THEME").map(PathBuf::from).unwrap_or_else(|| omarchy_dir().join("theme"));
    std::fs::read_to_string(dir.join("colors.toml"))
        .ok()
        .and_then(|s| Palette::from_omarchy(&s))
        .unwrap_or_else(Palette::builtin)
}

/// Re-reads the Omarchy theme; returns whether the colors changed.
pub fn reload(ctx: &egui::Context) -> bool {
    let next = load_palette();
    if PALETTE.load(Ordering::Acquire).is_null() || *pal() != next {
        set_palette(next);
        apply_visuals(ctx);
        true
    } else {
        false
    }
}

/// Watches Omarchy's current-theme folder; `true` is sent whenever it changes.
pub fn watch(ctx: egui::Context) -> Option<(notify::RecommendedWatcher, Receiver<()>)> {
    use notify::Watcher as _;
    let (tx, rx) = channel();
    let mut w = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if res.is_ok_and(|e| !matches!(e.kind, notify::EventKind::Access(_))) {
            let _ = tx.send(());
            ctx.request_repaint();
        }
    })
    .ok()?;
    if let Some(dir) = std::env::var_os("FPILOT_OMARCHY_THEME") {
        w.watch(Path::new(&dir), notify::RecursiveMode::NonRecursive).ok()?;
    } else {
        // `omarchy theme set` swaps the whole theme folder, so watch its parent.
        w.watch(&omarchy_dir(), notify::RecursiveMode::NonRecursive).ok()?;
        let _ = w.watch(&omarchy_dir().join("theme"), notify::RecursiveMode::NonRecursive);
    }
    Some((w, rx))
}

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
    reload(ctx);
    apply_visuals(ctx);
}

fn apply_visuals(ctx: &egui::Context) {
    let p = pal();
    let theme = if p.dark { egui::Theme::Dark } else { egui::Theme::Light };
    ctx.set_theme(theme);
    let mut v = if p.dark { Visuals::dark() } else { Visuals::light() };
    v.override_text_color = Some(p.text);
    v.panel_fill = p.panel;
    v.window_fill = p.panel;
    v.extreme_bg_color = p.bg;
    v.faint_bg_color = p.surface;
    v.selection.bg_fill = p.select;
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v.hyperlink_color = p.accent;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    v.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
    v.widgets.inactive.bg_fill = p.surface;
    v.widgets.inactive.bg_stroke = Stroke::NONE;
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, p.text);
    v.widgets.hovered.weak_bg_fill = p.hover;
    v.widgets.hovered.bg_fill = p.hover;
    v.widgets.hovered.bg_stroke = Stroke::NONE;
    v.widgets.hovered.fg_stroke = Stroke::new(1.5, p.text);
    v.widgets.active.weak_bg_fill = p.select;
    v.widgets.active.bg_fill = p.select;
    v.widgets.active.bg_stroke = Stroke::NONE;
    v.widgets.active.fg_stroke = Stroke::new(1.5, p.text);
    v.widgets.open.weak_bg_fill = p.hover;
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
    ctx.set_visuals_of(theme, v);
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

pub fn category_color(cat: Category) -> Color32 {
    let p = pal();
    match cat {
        Category::Folder => p.folder,
        Category::Image => p.image,
        Category::Video => p.video,
        Category::Audio => p.audio,
        Category::Archive => p.archive,
        Category::Code => p.code,
        Category::Text => p.dim,
        Category::Document => p.document,
        Category::Executable => p.executable,
        Category::Other => p.faint,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stock_omarchy_themes() {
        let Ok(dirs) = std::fs::read_dir("/usr/share/omarchy/themes") else { return };
        for d in dirs.flatten() {
            let Ok(text) = std::fs::read_to_string(d.path().join("colors.toml")) else { continue };
            let p = Palette::from_omarchy(&text).unwrap_or_else(|| panic!("{:?} did not parse", d.path()));
            // Text must stand out from the background in either mode.
            let lum = |c: Color32| 0.2126 * c.r() as f32 + 0.7152 * c.g() as f32 + 0.0722 * c.b() as f32;
            assert!((lum(p.text) - lum(p.bg)).abs() > 60.0, "{:?}: low contrast", d.path());
            assert_eq!(p.dark, !text.contains("mode = \"light\""), "{:?}", d.path());
        }
    }

    #[test]
    fn mapping() {
        let p = Palette::from_omarchy("mode = \"light\"\nbackground = \"#ffffff\"\nforeground = '#000000'\naccent=\"#ff0000\"").unwrap();
        assert!(!p.dark);
        assert_eq!(p.accent, Color32::from_rgb(255, 0, 0));
        assert_eq!(p.bg, Color32::WHITE);
        assert!(Palette::from_omarchy("accent = \"#ff0000\"").is_none());
    }
}
