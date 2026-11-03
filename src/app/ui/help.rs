//! Help (F1): how search works, then every command and key, with a filter.

use crate::palette::{COMMANDS, OTHER_KEYS, entry_score, fuzzy_score};
use crate::theme::*;
use eframe::egui::{
    Align2, CornerRadius, FontId, Grid, Key, Modifiers, RichText, ScrollArea, Sense, TextEdit, Ui,
    vec2,
};

/// Query syntax, with what each form matches.
const SEARCH: &[(&str, &str)] = &[
    ("nd", "Letters in order, gaps allowed: finds “NaDya”"),
    ("final report", "Every word must match, in any order"),
    ("\"two words\"", "Exact text, kept together"),
    ("=notes.txt", "The whole name, exactly"),
    ("ext:pdf,jpg", "Only these extensions"),
    ("type:folder", "Only folders; type:file for files"),
    (
        "type:image",
        "Also video, audio, document, archive, code, text",
    ),
];

const SEARCH_INTRO: &str = "Type in any folder to filter it. Ctrl+F searches subfolders and \
    Ctrl+Shift+F lists them all flat. In the search box, ↑/↓ choose, Enter opens, Esc closes \
    and F5 rescans. Filters combine with words: “report ext:pdf”.";

/// Draws the help window; returns true once it should close.
pub(super) fn show(ui: &mut Ui, filter: &mut String, focus: &mut bool) -> bool {
    let screen = ui.ctx().content_rect();
    ui.set_width(640.0_f32.min(screen.width() - 48.0));
    let mut close = ui.input_mut(|i| crate::app::take(i, Modifiers::NONE, Key::F1));
    ui.label(RichText::new("Help").size(16.0).strong());
    ui.add_space(4.0);
    let resp = ui.add(
        TextEdit::singleline(filter)
            .hint_text("Filter commands, keys and search syntax…")
            .desired_width(f32::INFINITY),
    );
    if std::mem::take(focus) {
        resp.request_focus();
    }
    ui.add_space(8.0);

    let q = filter.trim();
    // Rows that match the filter, best first; everything in order when it's empty.
    let ranked = |rows: Vec<(String, String, &'static str)>| -> Vec<(String, String)> {
        let mut scored: Vec<_> = rows
            .into_iter()
            .enumerate()
            .filter_map(|(i, (name, keys, keywords))| {
                // Here the keys are something to look up too: "drag" finds Shift+drag.
                let by_keys = fuzzy_score(q, &keys).map(|s| s - 2);
                let score = entry_score(q, &name, keywords, &keys).max(by_keys)?;
                Some((score, i, name, keys))
            })
            .collect();
        if !q.is_empty() {
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        }
        scored.into_iter().map(|(_, _, n, k)| (n, k)).collect()
    };
    // Search rows match on their description, or on the syntax as a keyword.
    let search = ranked(
        SEARCH
            .iter()
            .map(|&(syntax, what)| (what.to_string(), syntax.to_string(), syntax))
            .collect(),
    );
    let commands = ranked(
        COMMANDS
            .iter()
            .map(|c| (c.name.to_string(), c.keys.to_string(), c.keywords))
            .collect(),
    );
    let keys = ranked(
        OTHER_KEYS
            .iter()
            .map(|&(name, keys)| (name.to_string(), keys.to_string(), ""))
            .collect(),
    );

    let max_h = (screen.height() - 220.0).clamp(160.0, 560.0);
    // A fixed height keeps the window still while the filter narrows the list.
    ScrollArea::vertical()
        .min_scrolled_height(max_h)
        .max_height(max_h)
        .auto_shrink(false)
        .show(ui, |ui| {
            if !search.is_empty() {
                heading(ui, "SEARCH");
                if q.is_empty() {
                    ui.label(RichText::new(SEARCH_INTRO).color(pal().dim));
                    ui.add_space(6.0);
                }
                Grid::new("help_search")
                    .num_columns(2)
                    .spacing(vec2(24.0, 6.0))
                    .show(ui, |ui| {
                        for (what, syntax) in &search {
                            ui.label(RichText::new(syntax).monospace().color(pal().accent));
                            ui.label(what);
                            ui.end_row();
                        }
                    });
            }
            if !commands.is_empty() {
                heading(ui, "COMMANDS");
                ui.label(
                    RichText::new("Run any of these from the command palette (Ctrl+P).")
                        .color(pal().dim),
                );
                ui.add_space(6.0);
                key_grid(ui, &commands);
            }
            if !keys.is_empty() {
                heading(ui, "MORE KEYS");
                key_grid(ui, &keys);
            }
            if search.is_empty() && commands.is_empty() && keys.is_empty() {
                ui.label(RichText::new("No matches").color(pal().faint));
            }
        });
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        close |= ui.button("Close").clicked();
        ui.label(RichText::new("Esc or F1").size(12.0).color(pal().faint));
    });
    close
}

fn heading(ui: &mut Ui, text: &str) {
    ui.add_space(10.0);
    ui.label(RichText::new(text).size(11.0).strong().color(pal().faint));
    ui.add_space(4.0);
}

/// Names with their keys right-aligned, on alternating stripes; commands
/// without keys show a dash.
fn key_grid(ui: &mut Ui, rows: &[(String, String)]) {
    for (row, (name, keys)) in rows.iter().enumerate() {
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
        if !ui.is_rect_visible(rect) {
            continue;
        }
        let painter = ui.painter_at(rect);
        if row % 2 == 1 {
            painter.rect_filled(rect, CornerRadius::same(4), pal().surface);
        }
        let keys = if keys.is_empty() { "—" } else { keys };
        let keys = painter.text(
            rect.right_center() - vec2(8.0, 0.0),
            Align2::RIGHT_CENTER,
            keys,
            FontId::proportional(12.0),
            pal().dim,
        );
        painter
            .with_clip_rect(rect.with_max_x(keys.left() - 12.0))
            .text(
                rect.left_center() + vec2(8.0, 0.0),
                Align2::LEFT_CENTER,
                name,
                FontId::proportional(BODY),
                pal().text,
            );
    }
}
