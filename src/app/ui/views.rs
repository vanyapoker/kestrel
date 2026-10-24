use crate::app::icons::*;
use crate::app::{Action, App};
use crate::tab::{ICON_SIZES, SortKey, ViewMode};
use crate::theme::*;
use crate::util;
use crate::wl_dnd::ClipOp;
use eframe::egui::{
    self, Align, Align2, CornerRadius, FontId, Layout, Pos2, Rect, ScrollArea, Sense, Stroke, Ui,
    vec2,
};

use super::{
    Columns, ROW_H, RowEnv, RowPaint, background_drop, chevron, drag_autoscroll, drop_hover,
    empty_state, paint_cell, paint_name, paint_row, paint_tile, record_folder, row_interactions,
    rubber_band, scroll_area, sort_arrow,
};

impl App {
    pub(in crate::app) fn miller_columns(
        &mut self,
        ui: &mut Ui,
        p: usize,
        actions: &mut Vec<Action>,
    ) {
        let pane_active = p == self.active_pane;
        let bookmarks = &self.places.bookmarks;
        let cut = self
            .clip
            .as_ref()
            .filter(|c| c.op == ClipOp::Cut)
            .map(|c| &c.paths);
        let pane = &mut self.panes[p];
        let tab = &mut pane.tabs[pane.active];
        let id = tab.id;
        let Some(mut cols) = tab.columns.take() else {
            return;
        };
        let height = (ui.available_height() - 12.0).max(60.0);
        let follow = cols.scroll_to_active;
        cols.scroll_to_active = false;
        ScrollArea::horizontal()
            .id_salt(("miller", id))
            .animated(false)
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 1.0;
                ui.horizontal_top(|ui| {
                    for (index, stored) in cols.items.iter_mut().enumerate() {
                        let active = index == cols.active;
                        let directory = if active { &mut *tab } else { stored };
                        let path = directory.path.clone();
                        let mut local = Vec::new();
                        let out = ui.allocate_ui_with_layout(
                            vec2(280.0, height),
                            Layout::top_down(Align::Min),
                            |ui| {
                                ui.set_min_size(vec2(280.0, height));
                                ui.set_max_width(280.0);
                                let full = ui.max_rect().intersect(ui.clip_rect());
                                let (header, response) =
                                    ui.allocate_exact_size(vec2(280.0, 28.0), Sense::click());
                                ui.painter().rect_filled(
                                    header,
                                    0.0,
                                    if active && pane_active {
                                        pal().select_dim
                                    } else {
                                        pal().panel
                                    },
                                );
                                let name = path
                                    .file_name()
                                    .map(|s| s.to_string_lossy())
                                    .unwrap_or_else(|| "/".into());
                                ui.painter().with_clip_rect(header.shrink(6.0)).text(
                                    header.left_center() + vec2(8.0, 0.0),
                                    Align2::LEFT_CENTER,
                                    name,
                                    FontId::proportional(BODY),
                                    if active && pane_active {
                                        pal().accent
                                    } else {
                                        pal().dim
                                    },
                                );
                                response.on_hover_text(path.display().to_string());
                                let pointer = ui.input(|i| i.pointer.interact_pos());
                                if pointer.is_some_and(|pos| full.contains(pos))
                                    && ui.input(|i| {
                                        i.pointer.primary_pressed() || i.pointer.secondary_pressed()
                                    })
                                    && pointer.and_then(|pos| ui.ctx().layer_id_at(pos))
                                        == Some(ui.layer_id())
                                {
                                    local.push(Action::FocusPane(p));
                                }
                                // Virtualized row geometry and keyboard scrolling use the same stride.
                                ui.spacing_mut().item_spacing.y = 0.0;
                                let query = directory.query();
                                let modifiers = ui.input(|i| (i.modifiers.ctrl, i.modifiers.shift));
                                let mut over_item = false;
                                // Column positions can change when ancestors are prepended. The
                                // directory owns its offset, independently of the UI child order.
                                directory
                                    .force_offset
                                    .get_or_insert(directory.scroll_offset);
                                let scroll = scroll_area(
                                    directory,
                                    ("column", directory.listing_id),
                                    ROW_H,
                                    1,
                                );
                                let out = scroll.show_rows(
                                    ui,
                                    ROW_H,
                                    directory.view.len(),
                                    |ui, range| {
                                        ui.spacing_mut().item_spacing.y = 0.0;
                                        for vi in range {
                                            let e = &directory.entries[directory.view[vi]];
                                            let (rect, resp) = ui.allocate_exact_size(
                                                vec2(ui.available_width(), ROW_H),
                                                Sense::click_and_drag(),
                                            );
                                            over_item |=
                                                pointer.is_some_and(|pos| rect.contains(pos));
                                            let selected = directory.is_selected(vi);
                                            let drop = e.is_dir && drop_hover(&resp, &e.path);
                                            if selected || resp.hovered() || drop {
                                                ui.painter().rect_filled(
                                                    rect.shrink(2.0),
                                                    4.0,
                                                    if selected {
                                                        if active && pane_active {
                                                            pal().select
                                                        } else {
                                                            pal().select_dim
                                                        }
                                                    } else {
                                                        pal().hover
                                                    },
                                                );
                                            }
                                            if e.is_dir {
                                                record_folder(
                                                    ui.ctx(),
                                                    rect.intersect(ui.clip_rect()),
                                                    &e.path,
                                                );
                                            }
                                            let faded =
                                                cut.is_some_and(|paths| paths.contains(&e.path));
                                            draw_entry_icon(
                                                ui.painter(),
                                                rect.left_center() + vec2(15.0, 0.0),
                                                1.0,
                                                &e.path,
                                                util::category(&e.ext, e.is_dir),
                                            );
                                            let painter = ui
                                                .painter()
                                                .with_clip_rect(rect.shrink2(vec2(30.0, 0.0)));
                                            paint_name(
                                                &painter,
                                                rect.left_center() + vec2(30.0, 0.0),
                                                &e.name,
                                                &query,
                                                FontId::proportional(BODY),
                                                if faded || e.hidden {
                                                    pal().dim
                                                } else {
                                                    pal().text
                                                },
                                            );
                                            if e.is_dir {
                                                chevron(
                                                    ui.painter(),
                                                    rect.right_center() - vec2(10.0, 0.0),
                                                    false,
                                                    pal().faint,
                                                );
                                            }
                                            #[cfg(test)]
                                            self.row_rects.insert(
                                                e.path.clone(),
                                                rect.intersect(ui.clip_rect()),
                                            );
                                            row_interactions(
                                                ui,
                                                &resp,
                                                RowEnv {
                                                    tab: directory,
                                                    pane: p,
                                                    modifiers,
                                                    bookmarks,
                                                },
                                                vi,
                                                e,
                                                &mut local,
                                            );
                                        }
                                    },
                                );
                                directory.scroll_offset = out.state.offset.y;
                                directory.viewport_h = out.inner_rect.height();
                                drag_autoscroll(ui, directory, out.inner_rect);
                                let (inner, offset) = (out.inner_rect, out.state.offset.y);
                                rubber_band(
                                    ui,
                                    p,
                                    directory,
                                    inner,
                                    over_item,
                                    modifiers.0,
                                    |vi| {
                                        Rect::from_min_size(
                                            inner.min + vec2(0.0, vi as f32 * ROW_H - offset),
                                            vec2(inner.width(), ROW_H),
                                        )
                                    },
                                );
                                if directory.view.is_empty() {
                                    // Paint inside the viewport so errors and loading do not grow the column.
                                    let message = directory.error.as_deref().unwrap_or(
                                        if directory.loading {
                                            "Loading…"
                                        } else if directory.filter.is_empty() {
                                            "This folder is empty"
                                        } else {
                                            "No matches"
                                        },
                                    );
                                    ui.painter().with_clip_rect(inner).text(
                                        inner.left_top() + vec2(8.0, 16.0),
                                        Align2::LEFT_TOP,
                                        message,
                                        FontId::proportional(12.0),
                                        pal().dim,
                                    );
                                }
                                background_drop(
                                    ui,
                                    directory,
                                    inner.intersect(ui.clip_rect()),
                                    &mut local,
                                );
                                record_folder(ui.ctx(), full, &path);
                                ui.painter().vline(
                                    full.right(),
                                    full.y_range(),
                                    Stroke::new(1.0, pal().border),
                                );
                            },
                        );
                        if follow && active {
                            ui.scroll_to_rect(out.response.rect, None);
                        }
                        for action in local {
                            actions.push(Action::InColumn {
                                tab_id: id,
                                directory: path.clone(),
                                action: Box::new(action),
                            });
                        }
                    }
                });
            });
        tab.columns = Some(cols);
    }

    pub(in crate::app) fn file_list(&mut self, ui: &mut Ui, p: usize, actions: &mut Vec<Action>) {
        let pane_active = p == self.active_pane;
        let sizes = &self.sizes;
        let sizing = &self.sizing;
        let cut = self
            .clip
            .as_ref()
            .filter(|c| c.op == ClipOp::Cut)
            .map(|c| &c.paths);
        let dragging = self.dragging.as_deref();
        let bookmarks = &self.places.bookmarks;
        #[cfg(test)]
        let row_rects = &mut self.row_rects;
        let pane = &mut self.panes[p];
        let tab = &mut pane.tabs[pane.active];
        let full = ui.available_rect_before_wrap();
        let cols = Columns::new(full.left(), full.right());
        let query = tab.query();
        let in_search = tab.search.is_some();

        // Header.
        let (hrect, _) = ui.allocate_exact_size(vec2(full.width(), 28.0), Sense::hover());
        let header = [
            Some((
                SortKey::Name,
                "Name",
                (cols.name.0 - 22.0, cols.name.1),
                Align2::LEFT_CENTER,
            )),
            cols.size
                .map(|c| (SortKey::Size, "Size", c, Align2::RIGHT_CENTER)),
            cols.date
                .map(|c| (SortKey::Modified, "Modified", c, Align2::LEFT_CENTER)),
            cols.kind.map(|c| {
                (
                    SortKey::Kind,
                    if in_search { "Folder" } else { "Type" },
                    c,
                    Align2::LEFT_CENTER,
                )
            }),
        ];
        for (key, label, (x0, x1), align) in header.into_iter().flatten() {
            let r = Rect::from_x_y_ranges(x0..=x1, hrect.y_range());
            let resp = ui.interact(r, ui.id().with(("hdr", label)), Sense::click());
            let active = tab.sort == key;
            let right = align == Align2::RIGHT_CENTER;
            let pos = Pos2::new(if right { x1 } else { x0 }, r.center().y);
            let color = if active || resp.hovered() {
                pal().text
            } else {
                pal().dim
            };
            let text_rect = ui
                .painter()
                .text(pos, align, label, FontId::proportional(12.0), color);
            if active {
                let ax = if right {
                    text_rect.left() - 8.0
                } else {
                    text_rect.right() + 8.0
                };
                sort_arrow(ui.painter(), Pos2::new(ax, r.center().y), tab.ascending);
            }
            if resp.clicked() {
                actions.push(Action::FocusPane(p));
                actions.push(Action::Sort(key));
            }
        }
        ui.painter().hline(
            full.x_range(),
            hrect.bottom(),
            Stroke::new(1.0, pal().border),
        );

        if tab.view.is_empty() {
            empty_state(ui, tab);
            background_drop(ui, tab, full, actions);
            return;
        }

        let scroll = scroll_area(tab, ("list", tab.id), ROW_H, 1);
        let ctrl_shift = ui.input(|i| (i.modifiers.ctrl, i.modifiers.shift));
        let pointer = ui.input(|i| i.pointer.hover_pos());
        let mut over_item = false;
        let out = scroll.show_rows(ui, ROW_H, tab.view.len(), |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for vi in range {
                let e = &tab.entries[tab.view[vi]];
                let (rect, resp) =
                    ui.allocate_exact_size(vec2(full.width(), ROW_H), Sense::click_and_drag());
                over_item |= pointer.is_some_and(|pos| rect.contains(pos));
                let selected = tab.is_selected(vi);
                let painter = ui.painter();
                let row = rect.shrink2(vec2(4.0, 1.0));
                let drop_target = e.is_dir && drop_hover(&resp, &e.path);
                if e.is_dir {
                    record_folder(ui.ctx(), row, &e.path);
                }
                if selected {
                    painter.rect_filled(
                        row,
                        CornerRadius::same(5),
                        if pane_active {
                            pal().select
                        } else {
                            pal().select_dim
                        },
                    );
                } else if resp.hovered() || drop_target {
                    painter.rect_filled(row, CornerRadius::same(5), pal().hover);
                }
                if drop_target {
                    painter.rect_stroke(
                        row,
                        CornerRadius::same(5),
                        Stroke::new(1.5, pal().accent),
                        egui::StrokeKind::Inside,
                    );
                }
                if tab.cursor == Some(vi) && !tab.marked.is_empty() && pane_active {
                    painter.rect_stroke(
                        row,
                        CornerRadius::same(5),
                        Stroke::new(1.0, pal().accent),
                        egui::StrokeKind::Inside,
                    );
                }
                let faded = cut.is_some_and(|c| c.contains(&e.path))
                    || dragging.is_some_and(|d| d.0.contains(&e.path));
                let style = RowPaint {
                    cols: &cols,
                    sizes,
                    sizing,
                    query: &query,
                };
                paint_row(painter, rect, &style, e, faded);
                #[cfg(test)]
                row_rects.insert(e.path.clone(), rect);

                let env = RowEnv {
                    tab,
                    pane: p,
                    modifiers: ctrl_shift,
                    bookmarks,
                };
                row_interactions(ui, &resp, env, vi, e, actions);
            }
        });
        tab.scroll_offset = out.state.offset.y;
        tab.viewport_h = out.inner_rect.height();
        drag_autoscroll(ui, tab, out.inner_rect);
        let (inner, offset) = (out.inner_rect, out.state.offset.y);
        rubber_band(ui, p, tab, inner, over_item, ctrl_shift.0, |vi| {
            Rect::from_min_size(
                inner.min + vec2(0.0, vi as f32 * ROW_H - offset),
                vec2(inner.width(), ROW_H),
            )
        });
        background_drop(ui, tab, out.inner_rect, actions);
    }

    // ------------------------------------------------------------ grid view

    pub(in crate::app) fn file_grid(&mut self, ui: &mut Ui, p: usize, actions: &mut Vec<Action>) {
        let pane_active = p == self.active_pane;
        #[cfg(test)]
        let row_rects = &mut self.row_rects;
        let thumbs = &mut self.thumbs;
        let bookmarks = &self.places.bookmarks;
        let cut = self
            .clip
            .as_ref()
            .filter(|c| c.op == ClipOp::Cut)
            .map(|c| &c.paths);
        let dragging = self.dragging.as_deref();
        let pane = &mut self.panes[p];
        let tab = &mut pane.tabs[pane.active];
        let full = ui.available_rect_before_wrap();
        let mode = tab.mode;
        let query = tab.query();
        let (cell, radius) = match mode {
            ViewMode::Icons(i) => {
                let s = ICON_SIZES[i.min(ICON_SIZES.len() - 1)];
                (vec2((s + 28.0).max(96.0), s + 56.0), 8)
            }
            _ => (vec2(240.0_f32.min(full.width() - 8.0).max(120.0), ROW_H), 5),
        };
        let cols = ((full.width() - 8.0) / cell.x).floor().max(1.0) as usize;
        tab.grid_cols = cols;
        ui.add_space(6.0);
        if tab.view.is_empty() {
            empty_state(ui, tab);
            background_drop(ui, tab, full, actions);
            return;
        }
        let rows = tab.view.len().div_ceil(cols);
        let scroll = scroll_area(tab, ("grid", tab.id), cell.y, cols);
        let ctrl_shift = ui.input(|i| (i.modifiers.ctrl, i.modifiers.shift));
        let gap = ((full.width() - 8.0) - cols as f32 * cell.x) / cols as f32;
        let pointer = ui.input(|i| i.pointer.hover_pos());
        let mut over_item = false;
        let out = scroll.show_rows(ui, cell.y, rows, |ui, range| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            for r in range {
                ui.horizontal(|ui| {
                    ui.add_space(4.0);
                    for c in 0..cols {
                        let vi = r * cols + c;
                        let Some(&ei) = tab.view.get(vi) else { break };
                        let e = &tab.entries[ei];
                        let (rect, resp) = ui.allocate_exact_size(
                            vec2(cell.x + gap, cell.y),
                            Sense::click_and_drag(),
                        );
                        let tile = match mode {
                            ViewMode::Icons(_) => {
                                Rect::from_center_size(rect.center(), cell - vec2(8.0, 8.0))
                            }
                            _ => rect.shrink2(vec2(4.0, 1.0)),
                        };
                        over_item |= pointer.is_some_and(|pos| tile.contains(pos));
                        let selected = tab.is_selected(vi);
                        let drop_target = e.is_dir && drop_hover(&resp, &e.path);
                        if e.is_dir {
                            record_folder(ui.ctx(), tile, &e.path);
                        }
                        let painter = ui.painter();
                        let corner = CornerRadius::same(radius);
                        if selected {
                            painter.rect_filled(
                                tile,
                                corner,
                                if pane_active {
                                    pal().select
                                } else {
                                    pal().select_dim
                                },
                            );
                        } else if resp.hovered() || drop_target {
                            painter.rect_filled(tile, corner, pal().hover);
                        }
                        if drop_target {
                            painter.rect_stroke(
                                tile,
                                corner,
                                Stroke::new(1.5, pal().accent),
                                egui::StrokeKind::Inside,
                            );
                        }
                        if tab.cursor == Some(vi) && !tab.marked.is_empty() && pane_active {
                            painter.rect_stroke(
                                tile,
                                corner,
                                Stroke::new(1.0, pal().accent),
                                egui::StrokeKind::Inside,
                            );
                        }
                        let faded = cut.is_some_and(|c| c.contains(&e.path))
                            || dragging.is_some_and(|d| d.0.contains(&e.path));
                        match mode {
                            ViewMode::Icons(_) => {
                                paint_tile(painter, thumbs, tile, e, faded, &query)
                            }
                            _ => paint_cell(painter, tile, e, faded, &query),
                        }
                        #[cfg(test)]
                        row_rects.insert(e.path.clone(), rect);
                        let env = RowEnv {
                            tab,
                            pane: p,
                            modifiers: ctrl_shift,
                            bookmarks,
                        };
                        row_interactions(ui, &resp, env, vi, e, actions);
                    }
                });
            }
        });
        tab.scroll_offset = out.state.offset.y;
        tab.viewport_h = out.inner_rect.height();
        drag_autoscroll(ui, tab, out.inner_rect);
        let (inner, offset) = (out.inner_rect, out.state.offset.y);
        let step = cell.x + gap;
        rubber_band(ui, p, tab, inner, over_item, ctrl_shift.0, |vi| {
            let min = inner.min
                + vec2(
                    4.0 + (vi % cols) as f32 * step,
                    (vi / cols) as f32 * cell.y - offset,
                );
            Rect::from_min_size(min, cell).shrink2(vec2(4.0, 1.0))
        });
        background_drop(ui, tab, out.inner_rect, actions);
    }

    // ------------------------------------------------------------ sidebar / status / preview
}
