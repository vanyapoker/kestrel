use crate::app::icons::*;
use crate::app::{App, History};
use crate::preview::{Content, State};
use crate::theme::*;
use crate::util;
use eframe::egui::{
    self, CentralPanel, Color32, CornerRadius, Frame, Margin, Panel, RichText, ScrollArea, Sense,
    Ui, vec2,
};
use std::path::PathBuf;

use super::{INSPECTOR_TILE, UV, draw_checker, paint_tile, preview_code_cached, preview_table};

impl App {
    pub(in crate::app) fn sync_inspector(&mut self) {
        let selected = self.tab().cursor_entry();
        let origin = self.inspector.first().map(|e| &e.path);
        if origin != selected.map(|e| &e.path) {
            self.inspector = selected.filter(|e| e.is_dir).cloned().into_iter().collect();
        }
        let target = self
            .inspector
            .last()
            .or_else(|| self.tab().cursor_entry())
            .map(|e| (e.path.clone(), e.is_dir));
        self.preview
            .request(target.as_ref().map(|(path, dir)| (path.as_path(), *dir)));
    }

    pub(in crate::app) fn preview_panel(&mut self, ui: &mut Ui) {
        self.sync_inspector();
        if self.inspector.len() > 1 {
            ui.horizontal(|ui| {
                if ui
                    .button("‹ Back")
                    .on_hover_text("Back within the inspector")
                    .clicked()
                {
                    self.inspector.pop();
                    self.sync_inspector();
                }
                ui.label(RichText::new("Inspector").size(12.0).color(pal().faint));
            });
        }
        // Keep label selections scoped to the file whose content was selected.
        let path = self
            .inspector
            .last()
            .or_else(|| self.tab().cursor_entry())
            .map(|e| e.path.clone());
        let selection_id = ui.id().with("selection_path");
        let previous = ui.data_mut(|d| d.get_temp::<Option<PathBuf>>(selection_id));
        if previous.as_ref() != Some(&path) {
            ui.ctx()
                .plugin::<egui::text_selection::LabelSelectionState>()
                .lock()
                .clear_selection();
            ui.data_mut(|d| d.insert_temp(selection_id, path));
        }
        ui.style_mut().interaction.selectable_labels = true;
        let Some(e) = self
            .inspector
            .last()
            .or_else(|| self.tab().cursor_entry())
            .cloned()
        else {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| ui.label(RichText::new("No selection").color(pal().faint)));
            return;
        };
        let cat = util::category(&e.ext, e.is_dir);

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
            draw_entry_icon(ui.painter(), r.center(), 1.0, &e.path, cat);
            ui.add(egui::Label::new(RichText::new(&e.name).size(14.5).strong()).wrap());
        });
        ui.add_space(6.0);

        let mut rows: Vec<(String, String)> = Vec::new();
        let size = if e.is_dir {
            self.sizes.get(&e.path).copied()
        } else {
            Some(e.size)
        };
        rows.push((
            "Size".into(),
            size.map(util::human_size)
                .unwrap_or_else(|| "calculating…".into()),
        ));
        if let Some(m) = e.modified {
            rows.push(("Modified".into(), util::fmt_time(m)));
        }
        rows.push(("Permissions".into(), util::fmt_mode(e.mode, e.is_dir)));
        if let State::Ready(c) = &self.preview.state {
            match c {
                Content::Image { info, .. } | Content::Binary { info } => {
                    rows.extend(info.iter().cloned())
                }
                Content::Text {
                    lines, truncated, ..
                } => rows.push((
                    "Lines".into(),
                    format!("{lines}{}", if *truncated { "+" } else { "" }),
                )),
                Content::Table(t) => {
                    let plus = if t.truncated { "+" } else { "" };
                    rows.push((
                        "Rows".into(),
                        format!("{}{plus}", t.rows.len().saturating_sub(1)),
                    ));
                    rows.push(("Columns".into(), t.widths.len().to_string()));
                }
                Content::Dir { entries } => rows.push((
                    "Items".into(),
                    entries
                        .iter()
                        .filter(|e| self.show_hidden || !e.hidden)
                        .count()
                        .to_string(),
                )),
                _ => {}
            }
        }
        rows.push(("Path".into(), e.path.display().to_string()));

        Panel::bottom("preview_info")
            .frame(Frame::new().inner_margin(Margin::symmetric(0, 8)))
            .resizable(false)
            .show(ui, |ui| {
                egui::Grid::new("info")
                    .num_columns(2)
                    .spacing(vec2(12.0, 4.0))
                    .show(ui, |ui| {
                        for (k, v) in rows {
                            ui.label(RichText::new(k).size(12.0).color(pal().faint));
                            ui.add(
                                egui::Label::new(RichText::new(v).size(12.0).color(pal().dim))
                                    .wrap(),
                            );
                            ui.end_row();
                        }
                    });
            });

        let mut inspect = None;
        let mut open_folder = None;
        if e.is_dir && ui.small_button("Open folder in main view ↗").clicked() {
            open_folder = Some(e.path.clone());
        }
        CentralPanel::default()
            .frame(Frame::new())
            .show(ui, |ui| match &mut self.preview.state {
                State::Loading => {
                    ui.add_space(30.0);
                    ui.vertical_centered(|ui| ui.spinner());
                }
                State::Ready(Content::Image { tex, .. }) => {
                    let avail = ui.available_size();
                    let ts = tex.size_vec2();
                    let scale = (avail.x / ts.x).min(avail.y / ts.y).min(2.0);
                    let size = ts * scale;
                    ui.vertical_centered(|ui| {
                        ui.add_space(((avail.y - size.y) / 2.0).max(0.0));
                        let (r, _) = ui.allocate_exact_size(size, Sense::hover());
                        draw_checker(ui.painter(), r);
                        ui.painter().image(tex.id(), r, UV, Color32::WHITE);
                    });
                }
                State::Ready(Content::Text {
                    job,
                    links,
                    truncated,
                    plain,
                    layout,
                    ..
                }) => {
                    Frame::new()
                        .fill(pal().bg)
                        .corner_radius(CornerRadius::same(6))
                        .inner_margin(Margin::same(8))
                        .show(ui, |ui| {
                            if *plain {
                                ui.label(
                                    RichText::new(
                                        "Plain-text preview: highlighting limit reached.",
                                    )
                                    .small()
                                    .color(pal().faint),
                                );
                            }
                            if *truncated {
                                ui.label(
                                    RichText::new("… preview truncated")
                                        .color(pal().faint)
                                        .italics(),
                                );
                            }
                            if job.text.len() > 32 * 1024 {
                                preview_code_cached(ui, job, links, layout);
                            } else {
                                ScrollArea::vertical()
                                    .auto_shrink([false; 2])
                                    .id_salt("preview_text")
                                    .show(ui, |ui| {
                                        preview_code_cached(ui, job, links, layout);
                                    });
                            }
                        });
                }
                State::Ready(Content::Table(table)) => {
                    Frame::new()
                        .fill(pal().bg)
                        .corner_radius(CornerRadius::same(6))
                        .inner_margin(Margin::same(8))
                        .show(ui, |ui| preview_table(ui, table));
                }
                State::Ready(Content::Dir { entries }) => {
                    let entries: Vec<_> = entries
                        .iter()
                        .filter(|e| self.show_hidden || !e.hidden)
                        .collect();
                    if entries.is_empty() {
                        ui.label(RichText::new("Empty folder").color(pal().faint));
                    }
                    let gap = 6.0;
                    let width = ui.available_width();
                    let columns = ((width / (INSPECTOR_TILE + gap)) as usize).max(1);
                    let tile_w = (width / columns as f32 - gap).min(INSPECTOR_TILE * 1.4);
                    // `paint_tile` reserves 48px under the icon for the name.
                    let tile_h = tile_w + 48.0;
                    let query = crate::search::Query::new("");
                    ScrollArea::vertical()
                        .id_salt(("inspector", &e.path))
                        .auto_shrink([false; 2])
                        .show_rows(
                            ui,
                            tile_h + gap,
                            entries.len().div_ceil(columns),
                            |ui, rows| {
                                for row in rows {
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x = gap;
                                        for entry in
                                            entries.iter().skip(row * columns).take(columns)
                                        {
                                            let (rect, resp) = ui.allocate_exact_size(
                                                vec2(tile_w, tile_h),
                                                Sense::click(),
                                            );
                                            if resp.hovered() {
                                                ui.painter().rect_filled(
                                                    rect,
                                                    CornerRadius::same(6),
                                                    pal().hover,
                                                );
                                            }
                                            paint_tile(
                                                ui.painter(),
                                                &mut self.thumbs,
                                                rect,
                                                entry,
                                                false,
                                                &query,
                                            );
                                            if resp.clicked() {
                                                inspect = Some((*entry).clone());
                                            }
                                            resp.on_hover_text(format!(
                                                "Inspect {}",
                                                entry.path.display()
                                            ));
                                        }
                                    });
                                    ui.add_space(gap);
                                }
                            },
                        );
                }
                State::Ready(Content::Binary { .. }) => {
                    ui.add_space(40.0);
                    ui.vertical_centered(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(64.0, 72.0), Sense::hover());
                        draw_icon_scaled(ui.painter(), r.center(), cat, 4.0);
                        ui.add_space(10.0);
                        ui.label(RichText::new("No preview available").color(pal().faint));
                    });
                }
                State::Ready(Content::Error(err)) => {
                    ui.add_space(40.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new(err.as_str()).color(pal().faint))
                    });
                }
            });
        if let Some(entry) = inspect {
            self.inspector.push(entry);
            self.sync_inspector();
        }
        if let Some(path) = open_folder {
            self.navigate(path, History::Push);
        }
    }

    // ------------------------------------------------------------ overlays
}
