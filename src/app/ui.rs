use super::{Action, App, Dialog, DragPaths};
use crate::fsops::{self, Entry};
use crate::palette::{self, COMMANDS, Cmd};
use crate::preview::{Content, State};
use crate::tab::{SortKey, Tab, ViewMode};
use crate::theme::*;
use crate::thumbs::{self, Thumbs};
use crate::util::{self, Category};
use eframe::egui::{
    self, Align, Align2, CentralPanel, Color32, CornerRadius, DragAndDrop, FontId, Frame, Key, Layout, Margin,
    Modifiers, Painter, Panel, Pos2, Rect, RichText, ScrollArea, Sense, Stroke, Ui, UiBuilder, vec2,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

const ROW_H: f32 = 26.0;
const TILE: egui::Vec2 = egui::Vec2::new(124.0, 142.0);

impl App {
    pub(super) fn draw(&mut self, ui: &mut Ui) {
        let mut actions = Vec::new();

        Panel::bottom("status")
            .exact_size(26.0)
            .frame(Frame::new().fill(PANEL).inner_margin(Margin::symmetric(12, 2)))
            .show(ui, |ui| self.status_bar(ui));
        Panel::left("sidebar")
            .default_size(190.0)
            .min_size(140.0)
            .frame(Frame::new().fill(PANEL).inner_margin(Margin::symmetric(8, 4)))
            .show(ui, |ui| self.sidebar(ui, &mut actions));
        if self.show_preview {
            Panel::right("preview")
                .default_size(360.0)
                .min_size(220.0)
                .frame(Frame::new().fill(PANEL).inner_margin(Margin::symmetric(12, 4)))
                .show(ui, |ui| self.preview_panel(ui));
        }
        CentralPanel::default().frame(Frame::new().fill(BG)).show(ui, |ui| {
            let full = ui.max_rect();
            self.pane_rects.clear();
            if self.panes.len() == 1 {
                self.pane_rects.push(full);
                self.pane(ui, 0, &mut actions);
            } else {
                let mid = full.center().x;
                let rects = [
                    Rect::from_min_max(full.min, Pos2::new(mid - 1.0, full.max.y)),
                    Rect::from_min_max(Pos2::new(mid + 1.0, full.min.y), full.max),
                ];
                ui.painter().vline(mid, full.y_range(), Stroke::new(2.0, BORDER));
                for (p, r) in rects.into_iter().enumerate() {
                    self.pane_rects.push(r);
                    ui.scope_builder(UiBuilder::new().max_rect(r).id_salt(("pane", p)), |ui| {
                        ui.set_clip_rect(r);
                        self.pane(ui, p, &mut actions);
                    });
                }
            }
        });

        self.external_drops(ui.ctx(), &mut actions);
        self.drag_chip(ui.ctx());
        self.palette_ui(ui.ctx(), &mut actions);
        self.dialog_ui(ui.ctx());
        self.toasts_ui(ui.ctx());

        for a in actions {
            self.apply(a);
        }

        if self.show_preview {
            let sel = self.tab().cursor_entry().map(|e| (e.path.clone(), e.is_dir));
            self.preview.request(sel.as_ref().map(|(p, d)| (p.as_path(), *d)));
        }
        let title = format!("{} — fpilot", self.tab().path.display());
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Title(title));
    }

    // ------------------------------------------------------------ panes

    fn pane(&mut self, ui: &mut Ui, p: usize, actions: &mut Vec<Action>) {
        let rect = ui.max_rect();
        let pressed_here = ui.input(|i| {
            (i.pointer.primary_pressed() || i.pointer.secondary_pressed())
                && i.pointer.interact_pos().is_some_and(|pos| rect.contains(pos))
        });
        if pressed_here && p != self.active_pane {
            actions.push(Action::FocusPane(p));
        }
        Panel::top(egui::Id::new(("tabs", p)))
            .exact_size(36.0)
            .frame(Frame::new().fill(BG).inner_margin(Margin::symmetric(8, 4)))
            .show_separator_line(false)
            .show(ui, |ui| self.tab_bar(ui, p, actions));
        Panel::top(egui::Id::new(("toolbar", p)))
            .exact_size(38.0)
            .frame(Frame::new().fill(PANEL).inner_margin(Margin::symmetric(8, 4)))
            .show(ui, |ui| self.toolbar(ui, p, actions));
        CentralPanel::default()
            .frame(Frame::new().fill(BG).inner_margin(Margin::symmetric(4, 0)))
            .show(ui, |ui| {
                let pane = &self.panes[p];
                if pane.tabs[pane.active].mode == ViewMode::Grid {
                    self.file_grid(ui, p, actions);
                } else {
                    self.file_list(ui, p, actions);
                }
            });
    }

    fn tab_bar(&mut self, ui: &mut Ui, p: usize, actions: &mut Vec<Action>) {
        let pane_active = p == self.active_pane;
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let pane = &self.panes[p];
            for (i, tab) in pane.tabs.iter().enumerate() {
                let active = i == pane.active;
                let galley = ui.painter().layout_no_wrap(tab.title(), FontId::proportional(13.0), TEXT);
                let w = (galley.size().x + 44.0).clamp(90.0, 220.0);
                let (rect, resp) = ui.allocate_exact_size(vec2(w, 26.0), Sense::click());
                let close_rect =
                    Rect::from_center_size(Pos2::new(rect.right() - 13.0, rect.center().y), vec2(16.0, 16.0));
                let close = ui.interact(close_rect, ui.id().with(("tab_close", tab.id)), Sense::click());
                let painter = ui.painter();
                let drop_hover = resp.dnd_hover_payload::<DragPaths>().is_some();
                let bg = if active || drop_hover {
                    SURFACE
                } else if resp.hovered() {
                    HOVER
                } else {
                    Color32::TRANSPARENT
                };
                painter.rect_filled(rect, CornerRadius::same(6), bg);
                if active {
                    let line = Rect::from_min_size(
                        Pos2::new(rect.left() + 8.0, rect.bottom() - 2.0),
                        vec2(rect.width() - 16.0, 2.0),
                    );
                    painter.rect_filled(line, CornerRadius::same(1), if pane_active { ACCENT } else { FAINT });
                }
                draw_folder(painter, Pos2::new(rect.left() + 14.0, rect.center().y), 0.8);
                let text_rect = Rect::from_min_max(
                    Pos2::new(rect.left() + 24.0, rect.top()),
                    Pos2::new(rect.right() - 24.0, rect.bottom()),
                );
                let color = if active { TEXT } else { DIM };
                painter
                    .with_clip_rect(text_rect)
                    .galley(Pos2::new(text_rect.left(), rect.center().y - galley.size().y / 2.0), galley, color);
                if resp.hovered() || close.hovered() || active {
                    if close.hovered() {
                        painter.rect_filled(close_rect, CornerRadius::same(4), HOVER);
                    }
                    painter.text(close_rect.center(), Align2::CENTER_CENTER, "×", FontId::proportional(14.0), DIM);
                }
                if let Some(paths) = resp.dnd_release_payload::<DragPaths>() {
                    let copy = ui.input(|i| i.modifiers.ctrl);
                    actions.push(Action::Drop { paths: paths.0.clone(), dest: tab.path.clone(), copy });
                }
                if close.clicked() || resp.middle_clicked() {
                    actions.push(Action::CloseTab(p, i));
                } else if resp.clicked() {
                    actions.push(Action::SwitchTab(p, i));
                }
            }
            if glyph_button(ui, Glyph::Plus, true, false, "New tab (Ctrl+T)") {
                actions.push(Action::FocusPane(p));
                actions.push(Action::Cmd(Cmd::NewTab));
            }
        });
    }

    fn toolbar(&mut self, ui: &mut Ui, p: usize, actions: &mut Vec<Action>) {
        let last_pane = p == self.panes.len() - 1;
        ui.horizontal_centered(|ui| {
            let pane = &self.panes[p];
            let tab = &pane.tabs[pane.active];
            let mut nav = |cmd| {
                actions.push(Action::FocusPane(p));
                actions.push(Action::Cmd(cmd));
            };
            if glyph_button(ui, Glyph::Back, !tab.back.is_empty(), false, "Back (Alt+Left)") {
                nav(Cmd::Back);
            }
            if glyph_button(ui, Glyph::Forward, !tab.fwd.is_empty(), false, "Forward (Alt+Right)") {
                nav(Cmd::Forward);
            }
            if glyph_button(ui, Glyph::Up, tab.path.parent().is_some(), false, "Up (Backspace)") {
                nav(Cmd::GoUp);
            }
            if glyph_button(ui, Glyph::Refresh, true, false, "Refresh (F5)") {
                nav(Cmd::Refresh);
            }
            ui.add_space(6.0);

            // Right-aligned controls first, so the path bar takes the remaining width.
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if last_pane {
                    let tip = if self.show_preview { "Hide preview (F3)" } else { "Show preview (F3)" };
                    if glyph_button(ui, Glyph::Panel, true, self.show_preview, tip) {
                        actions.push(Action::Cmd(Cmd::TogglePreview));
                    }
                    if glyph_button(ui, Glyph::Eye, true, self.show_hidden, "Show hidden files (Ctrl+H)") {
                        actions.push(Action::Cmd(Cmd::ToggleHidden));
                    }
                    if glyph_button(ui, Glyph::Split, true, self.panes.len() > 1, "Split view (F6)") {
                        actions.push(Action::Cmd(Cmd::ToggleSplit));
                    }
                }
                let pane = &self.panes[p];
                let tab = &pane.tabs[pane.active];
                let grid = tab.mode == ViewMode::Grid;
                if glyph_button(ui, Glyph::Grid, true, grid, "Grid view (Ctrl+G)") {
                    actions.push(Action::FocusPane(p));
                    actions.push(Action::Cmd(Cmd::ToggleGrid));
                }
                if glyph_button(ui, Glyph::Search, true, tab.search.is_some(), "Search subfolders (Ctrl+F)") {
                    actions.push(Action::FocusPane(p));
                    actions.push(if tab.search.is_some() { Action::StopSearch } else { Action::Cmd(Cmd::Search) });
                }
                if !tab.filter.is_empty() {
                    let chip = Frame::new()
                        .fill(SELECT)
                        .corner_radius(CornerRadius::same(5))
                        .inner_margin(Margin::symmetric(8, 3))
                        .show(ui, |ui| ui.label(RichText::new(format!("Filter: {}  ×", tab.filter)).color(TEXT)));
                    if chip.response.interact(Sense::click()).on_hover_text("Clear filter (Esc)").clicked() {
                        actions.push(Action::FocusPane(p));
                        actions.push(Action::ClearFilter);
                    }
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| self.path_bar(ui, p, actions));
            });
        });
    }

    fn path_bar(&mut self, ui: &mut Ui, p: usize, actions: &mut Vec<Action>) {
        let frame = Frame::new()
            .fill(BG)
            .corner_radius(CornerRadius::same(6))
            .stroke(Stroke::new(1.0, if self.tab_in(p).search.is_some() { ACCENT } else { BORDER }))
            .inner_margin(Margin::symmetric(6, 2));
        frame.show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_height(24.0);

            // Recursive search box.
            let search_focus = self.search_focus && p == self.active_pane;
            let pane = &mut self.panes[p];
            let tab = &mut pane.tabs[pane.active];
            let tab_id = tab.id;
            if let Some(s) = tab.search.as_mut() {
                ui.horizontal_centered(|ui| {
                    paint_glyph(ui.painter(), ui.cursor().min + vec2(8.0, 12.0), Glyph::Search, DIM);
                    ui.add_space(20.0);
                    let hint = format!("Search in {}", s.root.display());
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut s.query)
                            .id(egui::Id::new(("search", tab_id)))
                            .hint_text(hint)
                            .frame(Frame::NONE)
                            .desired_width(f32::INFINITY)
                            .vertical_align(Align::Center),
                    );
                    if search_focus {
                        resp.request_focus();
                    }
                    if resp.changed() {
                        s.restart_at = Some(Instant::now() + super::SEARCH_DEBOUNCE);
                        ui.ctx().request_repaint_after(super::SEARCH_DEBOUNCE);
                    }
                });
                if search_focus {
                    self.search_focus = false;
                }
                return;
            }

            if let Some((ep, text)) = &mut self.path_edit {
                if *ep == p {
                    let resp = ui.add(
                        egui::TextEdit::singleline(text)
                            .frame(Frame::NONE)
                            .desired_width(f32::INFINITY)
                            .vertical_align(Align::Center),
                    );
                    if self.path_edit_focus {
                        resp.request_focus();
                        self.path_edit_focus = false;
                    }
                    if resp.lost_focus() {
                        if ui.input(|i| i.key_pressed(Key::Enter)) {
                            let target = expand_tilde(text.trim());
                            if target.is_dir() {
                                actions.push(Action::FocusPane(p));
                                actions.push(Action::Navigate(target));
                            } else if target.is_file() {
                                fsops::open_default(&target);
                            }
                        }
                        self.path_edit = None;
                    }
                    return;
                }
            }

            let path = self.tab_in(p).path.clone();
            let bg = ui.interact(ui.max_rect(), ui.id().with(("crumbs_bg", p)), Sense::click());
            if bg.clicked() {
                actions.push(Action::FocusPane(p));
                actions.push(Action::Cmd(Cmd::EditPath));
            }
            let avail = ui.available_width();
            let clip = ui.max_rect();
            ui.set_clip_rect(clip.intersect(ui.clip_rect()));
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let mut parts: Vec<(&Path, String)> = path
                    .ancestors()
                    .map(|a| {
                        let label = a.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "/".into());
                        (a, label)
                    })
                    .collect();
                parts.reverse();
                // Drop leading segments until the rest fits; they collapse into "…".
                let font = FontId::proportional(BODY);
                let pad = ui.spacing().button_padding.x * 2.0;
                let sep_w = ui.painter().layout_no_wrap(" › ".into(), font.clone(), FAINT).size().x;
                let widths: Vec<f32> = parts
                    .iter()
                    .map(|(_, l)| ui.painter().layout_no_wrap(l.clone(), font.clone(), TEXT).size().x + pad + sep_w)
                    .collect();
                let mut skip = 0;
                let ellipsis_w = 30.0;
                while skip + 1 < parts.len() && widths[skip..].iter().sum::<f32>() + if skip > 0 { ellipsis_w } else { 0.0 } > avail - 8.0 {
                    skip += 1;
                }
                if skip > 0 {
                    let hidden = parts[skip - 1].0.to_path_buf();
                    let r = ui.add(egui::Button::new(RichText::new("…").color(DIM)).frame_when_inactive(false));
                    if r.on_hover_text(hidden.display().to_string()).clicked() {
                        actions.push(Action::FocusPane(p));
                        actions.push(Action::Navigate(hidden));
                    }
                }
                let last_index = parts.len() - 1;
                for (n, (part, label)) in parts.into_iter().enumerate().skip(skip) {
                    if n > 1 || (skip > 0 && n > 0) {
                        ui.label(RichText::new(" › ").color(FAINT));
                    }
                    let text = RichText::new(label).color(if n == last_index { TEXT } else { DIM });
                    let resp = ui.add(egui::Button::new(text).frame_when_inactive(false));
                    if let Some(paths) = resp.dnd_release_payload::<DragPaths>() {
                        let copy = ui.input(|i| i.modifiers.ctrl);
                        actions.push(Action::Drop { paths: paths.0.clone(), dest: part.to_path_buf(), copy });
                    }
                    if resp.clicked() {
                        actions.push(Action::FocusPane(p));
                        actions.push(Action::Navigate(part.to_path_buf()));
                    }
                    if resp.middle_clicked() {
                        actions.push(Action::FocusPane(p));
                        actions.push(Action::NewTab(part.to_path_buf()));
                    }
                }
            });
        });
    }

    fn tab_in(&self, p: usize) -> &Tab {
        let pane = &self.panes[p];
        &pane.tabs[pane.active]
    }

    // ------------------------------------------------------------ list view

    fn file_list(&mut self, ui: &mut Ui, p: usize, actions: &mut Vec<Action>) {
        let pane_active = p == self.active_pane;
        let sizes = &self.sizes;
        let sizing = &self.sizing;
        let cut = self.clip.as_ref().filter(|c| c.cut).map(|c| &c.paths);
        let pane = &mut self.panes[p];
        let tab = &mut pane.tabs[pane.active];
        let full = ui.available_rect_before_wrap();
        let cols = Columns::new(full.left(), full.right());
        let in_search = tab.search.is_some();

        // Header.
        let (hrect, _) = ui.allocate_exact_size(vec2(full.width(), 28.0), Sense::hover());
        let header = [
            Some((SortKey::Name, "Name", (cols.name.0 - 22.0, cols.name.1), Align2::LEFT_CENTER)),
            cols.size.map(|c| (SortKey::Size, "Size", c, Align2::RIGHT_CENTER)),
            cols.date.map(|c| (SortKey::Modified, "Modified", c, Align2::LEFT_CENTER)),
            cols.kind.map(|c| (SortKey::Kind, if in_search { "Folder" } else { "Type" }, c, Align2::LEFT_CENTER)),
        ];
        for (key, label, (x0, x1), align) in header.into_iter().flatten() {
            let r = Rect::from_x_y_ranges(x0..=x1, hrect.y_range());
            let resp = ui.interact(r, ui.id().with(("hdr", label)), Sense::click());
            let active = tab.sort == key;
            let right = align == Align2::RIGHT_CENTER;
            let pos = Pos2::new(if right { x1 } else { x0 }, r.center().y);
            let color = if active || resp.hovered() { TEXT } else { DIM };
            let text_rect = ui.painter().text(pos, align, label, FontId::proportional(12.0), color);
            if active {
                let ax = if right { text_rect.left() - 8.0 } else { text_rect.right() + 8.0 };
                sort_arrow(ui.painter(), Pos2::new(ax, r.center().y), tab.ascending);
            }
            if resp.clicked() {
                actions.push(Action::FocusPane(p));
                actions.push(Action::Sort(key));
            }
        }
        ui.painter().hline(full.x_range(), hrect.bottom(), Stroke::new(1.0, BORDER));

        if tab.view.is_empty() {
            empty_state(ui, tab);
            background_drop(ui, p, tab, full, actions);
            return;
        }

        let scroll = scroll_area(tab, ("list", tab.id), ROW_H, 1);
        let ctrl_shift = ui.input(|i| (i.modifiers.ctrl, i.modifiers.shift));
        let out = scroll.show_rows(ui, ROW_H, tab.view.len(), |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for vi in range {
                let e = &tab.entries[tab.view[vi]];
                let (rect, resp) = ui.allocate_exact_size(vec2(full.width(), ROW_H), Sense::click_and_drag());
                let selected = tab.is_selected(vi);
                let painter = ui.painter();
                let row = rect.shrink2(vec2(4.0, 1.0));
                let drop_target = e.is_dir && drop_hover(&resp, &e.path);
                if selected {
                    painter.rect_filled(row, CornerRadius::same(5), if pane_active { SELECT } else { SELECT_DIM });
                } else if resp.hovered() || drop_target {
                    painter.rect_filled(row, CornerRadius::same(5), HOVER);
                }
                if drop_target {
                    painter.rect_stroke(row, CornerRadius::same(5), Stroke::new(1.5, ACCENT), egui::StrokeKind::Inside);
                }
                if tab.cursor == Some(vi) && !tab.marked.is_empty() && pane_active {
                    painter.rect_stroke(row, CornerRadius::same(5), Stroke::new(1.0, ACCENT), egui::StrokeKind::Inside);
                }
                let faded = cut.is_some_and(|c| c.contains(&e.path));
                paint_row(painter, rect, &cols, e, sizes, sizing, faded);

                row_interactions(ui, &resp, tab, vi, e, ctrl_shift, actions, p);
            }
        });
        tab.scroll_offset = out.state.offset.y;
        tab.viewport_h = out.inner_rect.height();
        background_drop(ui, p, tab, out.inner_rect, actions);
    }

    // ------------------------------------------------------------ grid view

    fn file_grid(&mut self, ui: &mut Ui, p: usize, actions: &mut Vec<Action>) {
        let pane_active = p == self.active_pane;
        let thumbs = &mut self.thumbs;
        let pane = &mut self.panes[p];
        let tab = &mut pane.tabs[pane.active];
        let full = ui.available_rect_before_wrap();
        let cols = ((full.width() - 8.0) / TILE.x).floor().max(1.0) as usize;
        tab.grid_cols = cols;
        ui.add_space(6.0);
        if tab.view.is_empty() {
            empty_state(ui, tab);
            background_drop(ui, p, tab, full, actions);
            return;
        }
        let rows = tab.view.len().div_ceil(cols);
        let scroll = scroll_area(tab, ("grid", tab.id), TILE.y, cols);
        let ctrl_shift = ui.input(|i| (i.modifiers.ctrl, i.modifiers.shift));
        let gap = ((full.width() - 8.0) - cols as f32 * TILE.x) / cols as f32;
        let out = scroll.show_rows(ui, TILE.y, rows, |ui, range| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            for r in range {
                ui.horizontal(|ui| {
                    ui.add_space(4.0);
                    for c in 0..cols {
                        let vi = r * cols + c;
                        let Some(&ei) = tab.view.get(vi) else { break };
                        let e = &tab.entries[ei];
                        let (rect, resp) = ui.allocate_exact_size(vec2(TILE.x + gap, TILE.y), Sense::click_and_drag());
                        let tile = Rect::from_center_size(rect.center(), TILE - vec2(8.0, 8.0));
                        let selected = tab.is_selected(vi);
                        let drop_target = e.is_dir && drop_hover(&resp, &e.path);
                        let painter = ui.painter();
                        if selected {
                            painter.rect_filled(tile, CornerRadius::same(8), if pane_active { SELECT } else { SELECT_DIM });
                        } else if resp.hovered() || drop_target {
                            painter.rect_filled(tile, CornerRadius::same(8), HOVER);
                        }
                        if drop_target {
                            painter.rect_stroke(tile, CornerRadius::same(8), Stroke::new(1.5, ACCENT), egui::StrokeKind::Inside);
                        }
                        paint_tile(painter, thumbs, tile, e);
                        row_interactions(ui, &resp, tab, vi, e, ctrl_shift, actions, p);
                    }
                });
            }
        });
        tab.scroll_offset = out.state.offset.y;
        tab.viewport_h = out.inner_rect.height();
        background_drop(ui, p, tab, out.inner_rect, actions);
    }

    // ------------------------------------------------------------ sidebar / status / preview

    fn sidebar(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
            let cur = self.tab().path.clone();
            let sections: [(&str, &Vec<(String, PathBuf)>); 3] = [
                ("PLACES", &self.places.places),
                ("BOOKMARKS", &self.places.bookmarks),
                ("DEVICES", &self.places.devices),
            ];
            for (title, items) in sections {
                if items.is_empty() {
                    continue;
                }
                ui.add_space(8.0);
                ui.label(RichText::new(title).size(10.5).color(FAINT).strong());
                ui.add_space(2.0);
                for (name, path) in items {
                    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click());
                    let painter = ui.painter();
                    let selected = *path == cur;
                    let drop_target = drop_hover(&resp, path);
                    if selected {
                        painter.rect_filled(rect, CornerRadius::same(5), SELECT);
                    } else if resp.hovered() || drop_target {
                        painter.rect_filled(rect, CornerRadius::same(5), HOVER);
                    }
                    if drop_target {
                        painter.rect_stroke(rect, CornerRadius::same(5), Stroke::new(1.5, ACCENT), egui::StrokeKind::Inside);
                    }
                    draw_folder(painter, Pos2::new(rect.left() + 14.0, rect.center().y), 0.85);
                    painter.with_clip_rect(rect.shrink(2.0)).text(
                        Pos2::new(rect.left() + 28.0, rect.center().y),
                        Align2::LEFT_CENTER,
                        name,
                        FontId::proportional(BODY),
                        if selected { TEXT } else { DIM },
                    );
                    if let Some(paths) = resp.dnd_release_payload::<DragPaths>() {
                        let copy = ui.input(|i| i.modifiers.ctrl);
                        actions.push(Action::Drop { paths: paths.0.clone(), dest: path.clone(), copy });
                    }
                    let resp = resp.on_hover_text(path.display().to_string());
                    if resp.clicked() {
                        actions.push(Action::Navigate(path.clone()));
                    }
                    if resp.middle_clicked() {
                        actions.push(Action::NewTab(path.clone()));
                    }
                }
            }
        });
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        let job = self.ops.snapshot();
        let queued = self.ops.queued.load(std::sync::atomic::Ordering::Relaxed);
        let tab = self.tab();
        let small = |s: String| RichText::new(s).size(12.0).color(DIM);
        ui.horizontal_centered(|ui| {
            if let Some(s) = &tab.search {
                let n = tab.view.len();
                let more = if s.truncated { "+" } else { "" };
                ui.label(small(format!("{n}{more} results")));
                if s.running {
                    ui.add(egui::Spinner::new().size(11.0).color(DIM));
                    ui.label(small("searching".into()));
                }
            } else {
                let (mut dirs, mut files, mut bytes) = (0usize, 0usize, 0u64);
                let mut all_known = true;
                for &i in &tab.view {
                    let e = &tab.entries[i];
                    if e.is_dir {
                        dirs += 1;
                        match self.sizes.get(&e.path) {
                            Some(s) => bytes += s,
                            None => all_known &= e.is_link,
                        }
                    } else {
                        files += 1;
                        bytes += e.size;
                    }
                }
                ui.label(small(format!("{} items  ·  {dirs} folders, {files} files", tab.view.len())));
                ui.label(small(format!("·  {}{}", util::human_size(bytes), if all_known { "" } else { "+" })));
                if tab.entries.iter().any(|e| e.is_dir && self.sizing.contains(&e.path)) {
                    ui.add(egui::Spinner::new().size(11.0).color(DIM));
                }
            }
            let n = tab.selection_count();
            if n > 1 {
                let bytes: u64 = tab
                    .targets()
                    .iter()
                    .filter_map(|p| {
                        let e = tab.entries.iter().find(|e| &e.path == p)?;
                        if e.is_dir { self.sizes.get(p).copied() } else { Some(e.size) }
                    })
                    .sum();
                ui.label(small(format!("·  {n} selected ({})", util::human_size(bytes))));
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(job) = &job {
                    let cancel = ui
                        .add(egui::Button::new(RichText::new("×").size(14.0)).frame_when_inactive(false))
                        .on_hover_text("Cancel");
                    if cancel.clicked() {
                        self.ops.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    let frac = if job.bytes_total > 0 { job.bytes_done as f32 / job.bytes_total as f32 } else { 0.0 };
                    if job.bytes_total > 0 {
                        ui.add(egui::ProgressBar::new(frac).desired_width(140.0).desired_height(8.0).fill(ACCENT));
                        ui.label(small(format!(
                            "{} / {}",
                            util::human_size(job.bytes_done),
                            util::human_size(job.bytes_total)
                        )));
                    } else {
                        ui.add(egui::Spinner::new().size(11.0).color(DIM));
                    }
                    let extra = if queued > 1 { format!("  (+{} queued)", queued - 1) } else { String::new() };
                    ui.label(RichText::new(format!("{}{extra}", job.title)).size(12.0).color(TEXT));
                } else if let Some((free, total)) = tab.disk {
                    ui.label(small(format!("{} free of {}", util::human_size(free), util::human_size(total))));
                }
            });
        });
    }

    fn preview_panel(&mut self, ui: &mut Ui) {
        let tab = self.tab();
        let Some(e) = tab.cursor_entry() else {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| ui.label(RichText::new("No selection").color(FAINT)));
            return;
        };
        let cat = util::category(&e.ext, e.is_dir);

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
            draw_icon(ui.painter(), r.center(), cat);
            ui.add(egui::Label::new(RichText::new(&e.name).size(14.5).strong()).wrap());
        });
        ui.add_space(6.0);

        let mut rows: Vec<(String, String)> = Vec::new();
        let size = if e.is_dir { self.sizes.get(&e.path).copied() } else { Some(e.size) };
        rows.push(("Size".into(), size.map(util::human_size).unwrap_or_else(|| "calculating…".into())));
        if let Some(m) = e.modified {
            rows.push(("Modified".into(), util::fmt_time(m)));
        }
        rows.push(("Permissions".into(), util::fmt_mode(e.mode, e.is_dir)));
        if let State::Ready(c) = &self.preview.state {
            match c {
                Content::Image { info, .. } | Content::Binary { info } => rows.extend(info.iter().cloned()),
                Content::Text { lines, truncated, .. } => {
                    rows.push(("Lines".into(), format!("{lines}{}", if *truncated { "+" } else { "" })))
                }
                Content::Dir { items: Some(n) } => rows.push(("Items".into(), n.to_string())),
                _ => {}
            }
        }
        rows.push(("Path".into(), e.path.display().to_string()));

        Panel::bottom("preview_info")
            .frame(Frame::new().inner_margin(Margin::symmetric(0, 8)))
            .resizable(false)
            .show(ui, |ui| {
                egui::Grid::new("info").num_columns(2).spacing(vec2(12.0, 4.0)).show(ui, |ui| {
                    for (k, v) in rows {
                        ui.label(RichText::new(k).size(12.0).color(FAINT));
                        ui.add(egui::Label::new(RichText::new(v).size(12.0).color(DIM)).wrap());
                        ui.end_row();
                    }
                });
            });

        CentralPanel::default().frame(Frame::new()).show(ui, |ui| match &self.preview.state {
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
            State::Ready(Content::Text { job, truncated, .. }) => {
                Frame::new()
                    .fill(BG)
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(Margin::same(8))
                    .show(ui, |ui| {
                        ScrollArea::both().auto_shrink([false; 2]).id_salt("preview_text").show(ui, |ui| {
                            ui.add(egui::Label::new(job.clone()).wrap_mode(egui::TextWrapMode::Extend));
                            if *truncated {
                                ui.label(RichText::new("… preview truncated").color(FAINT).italics());
                            }
                        });
                    });
            }
            State::Ready(Content::Dir { items }) => {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(80.0, 64.0), Sense::hover());
                    draw_folder(ui.painter(), r.center(), 4.0);
                    ui.add_space(10.0);
                    if let Some(n) = items {
                        ui.label(RichText::new(format!("{n} items")).color(DIM));
                    }
                });
            }
            State::Ready(Content::Binary { .. }) => {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(64.0, 72.0), Sense::hover());
                    draw_icon_scaled(ui.painter(), r.center(), cat, 4.0);
                    ui.add_space(10.0);
                    ui.label(RichText::new("No preview available").color(FAINT));
                });
            }
            State::Ready(Content::Error(err)) => {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| ui.label(RichText::new(err).color(FAINT)));
            }
        });
    }

    // ------------------------------------------------------------ overlays

    fn external_drops(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let (dropped, hovering, pos) = ctx.input(|i| {
            let dropped: Vec<PathBuf> = i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect();
            (dropped, !i.raw.hovered_files.is_empty(), i.pointer.hover_pos())
        });
        let pane = pos
            .and_then(|pos| self.pane_rects.iter().position(|r| r.contains(pos)))
            .unwrap_or(self.active_pane);
        if hovering {
            if let Some(r) = self.pane_rects.get(pane) {
                let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("drop_overlay")));
                painter.rect_filled(r.shrink(6.0), CornerRadius::same(10), ACCENT.gamma_multiply(0.12));
                painter.rect_stroke(r.shrink(6.0), CornerRadius::same(10), Stroke::new(2.0, ACCENT), egui::StrokeKind::Inside);
                painter.text(r.center(), Align2::CENTER_CENTER, "Drop to copy here", FontId::proportional(16.0), TEXT);
            }
        }
        if !dropped.is_empty() {
            actions.push(Action::FocusPane(pane));
            let dest = self.tab_in(pane).path.clone();
            actions.push(Action::Drop { paths: dropped, dest, copy: true });
        }
    }

    fn drag_chip(&self, ctx: &egui::Context) {
        let Some(payload) = DragAndDrop::payload::<DragPaths>(ctx) else { return };
        let Some(pos) = ctx.pointer_latest_pos() else { return };
        let copy = ctx.input(|i| i.modifiers.ctrl);
        let n = payload.0.len();
        let what = if n == 1 { ops_name(&payload.0[0]) } else { format!("{n} items") };
        egui::Area::new(egui::Id::new("drag_chip"))
            .order(egui::Order::Tooltip)
            .fixed_pos(pos + vec2(14.0, 10.0))
            .interactable(false)
            .show(ctx, |ui| {
                Frame::new()
                    .fill(SURFACE)
                    .stroke(Stroke::new(1.0, BORDER))
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(Margin::symmetric(8, 4))
                    .show(ui, |ui| {
                        let verb = if copy { "Copy" } else { "Move" };
                        ui.label(RichText::new(format!("{verb} {what}")).color(TEXT));
                    });
            });
    }

    fn palette_ui(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        if !self.palette.open {
            return;
        }
        // Commands, then places to jump to.
        let mut items: Vec<(String, String, Action)> =
            COMMANDS.iter().map(|&(c, name, keys)| (name.to_string(), keys.to_string(), Action::Cmd(c))).collect();
        for (name, path) in self.places.places.iter().chain(&self.places.bookmarks).chain(&self.places.devices) {
            items.push((format!("Go to {name}"), path.display().to_string(), Action::Navigate(path.clone())));
        }
        let q = self.palette.query.trim().to_string();
        let mut scored: Vec<(i32, usize)> = items
            .iter()
            .enumerate()
            .filter_map(|(i, (name, _, _))| palette::fuzzy_score(&q, name).map(|s| (s, i)))
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.truncate(14);
        let n = scored.len();
        let sel = &mut self.palette.selected;
        let (mut run, mut close) = (false, false);
        ctx.input_mut(|i| {
            if super::take(i, Modifiers::NONE, Key::ArrowDown) && n > 0 {
                *sel = (*sel + 1) % n;
            }
            if super::take(i, Modifiers::NONE, Key::ArrowUp) && n > 0 {
                *sel = (*sel + n - 1) % n;
            }
            run = super::take(i, Modifiers::NONE, Key::Enter);
            close = super::take(i, Modifiers::NONE, Key::Escape);
        });
        *sel = (*sel).min(n.saturating_sub(1));

        let area = egui::Area::new(egui::Id::new("palette_area")).anchor(Align2::CENTER_TOP, vec2(0.0, 70.0));
        let modal = egui::Modal::new(egui::Id::new("palette"))
            .area(area)
            .backdrop_color(Color32::from_black_alpha(90))
            .frame(Frame::new().fill(PANEL).stroke(Stroke::new(1.0, BORDER)).corner_radius(CornerRadius::same(10)).inner_margin(Margin::same(8)))
            .show(ctx, |ui| {
                ui.set_width(520.0);
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.palette.query)
                        .hint_text("Type a command or place…")
                        .desired_width(f32::INFINITY)
                        .font(FontId::proportional(15.0))
                        .margin(Margin::symmetric(8, 6)),
                );
                if self.palette.focus {
                    resp.request_focus();
                    self.palette.focus = false;
                }
                if resp.changed() {
                    self.palette.selected = 0;
                }
                ui.add_space(6.0);
                let mut clicked = None;
                for (row, &(_, i)) in scored.iter().enumerate() {
                    let (name, keys, _) = &items[i];
                    let (rect, r) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::click());
                    let selected = row == self.palette.selected;
                    if selected || r.hovered() {
                        ui.painter().rect_filled(rect, CornerRadius::same(5), if selected { SELECT } else { HOVER });
                    }
                    ui.painter().text(rect.left_center() + vec2(10.0, 0.0), Align2::LEFT_CENTER, name, FontId::proportional(BODY), TEXT);
                    ui.painter().with_clip_rect(rect).text(rect.right_center() - vec2(10.0, 0.0), Align2::RIGHT_CENTER, keys, FontId::proportional(12.0), FAINT);
                    if r.clicked() {
                        clicked = Some(row);
                    }
                }
                if n == 0 {
                    ui.label(RichText::new("No matches").color(FAINT));
                }
                clicked
            });
        if let Some(row) = modal.inner {
            self.palette.selected = row;
            run = true;
        }
        if run {
            if let Some(&(_, i)) = scored.get(self.palette.selected) {
                let action = items.swap_remove(i).2;
                actions.push(action);
            }
            close = true;
        }
        if close || modal.should_close() {
            self.palette.open = false;
        }
    }

    fn dialog_ui(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.dialog.as_mut() else { return };
        let (mut confirm, mut cancel) = (false, false);
        let frame = Frame::new()
            .fill(PANEL)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::same(16));
        let modal = egui::Modal::new(egui::Id::new("dialog")).frame(frame).show(ctx, |ui| {
            ui.set_width(380.0);
            match dialog {
                Dialog::Rename { path, name, focus } => {
                    // Select the name without its extension, like other file managers.
                    let stem = if path.is_dir() { name.chars().count() } else { stem_chars(name) };
                    name_field(ui, "Rename", name, focus, stem, &mut confirm);
                    dialog_buttons(ui, "Rename", false, &mut confirm, &mut cancel);
                }
                Dialog::Create { name, folder, focus, .. } => {
                    let title = if *folder { "New folder" } else { "New file" };
                    let all = name.chars().count();
                    name_field(ui, title, name, focus, all, &mut confirm);
                    dialog_buttons(ui, "Create", false, &mut confirm, &mut cancel);
                }
                Dialog::ConfirmDelete { paths } => {
                    ui.label(RichText::new("Delete permanently?").size(15.0).strong());
                    ui.add_space(6.0);
                    let n = paths.len();
                    let what = if n == 1 { "This item".to_string() } else { format!("These {n} items") };
                    ui.label(RichText::new(format!("{what} will be deleted. This can't be undone.")).color(DIM));
                    ui.add_space(6.0);
                    for p in paths.iter().take(6) {
                        ui.label(RichText::new(format!("•  {}", ops_name(p))).color(TEXT));
                    }
                    if n > 6 {
                        ui.label(RichText::new(format!("…and {} more", n - 6)).color(FAINT));
                    }
                    dialog_buttons(ui, "Delete", true, &mut confirm, &mut cancel);
                }
            }
        });
        if modal.should_close() {
            cancel = true;
        }
        if confirm {
            match self.dialog.take() {
                Some(Dialog::Rename { path, name, .. }) => self.rename(&path, &name),
                Some(Dialog::Create { dir, name, folder, .. }) => self.create(&dir, &name, folder),
                Some(Dialog::ConfirmDelete { paths }) => {
                    self.ops.submit(crate::ops::Job { kind: crate::ops::JobKind::Delete, srcs: paths, dest: None })
                }
                None => {}
            }
        } else if cancel {
            self.dialog = None;
        }
    }

    fn toasts_ui(&self, ctx: &egui::Context) {
        if self.toasts.is_empty() {
            return;
        }
        egui::Area::new(egui::Id::new("toasts"))
            .anchor(Align2::RIGHT_BOTTOM, vec2(-16.0, -36.0))
            .order(egui::Order::Foreground)
            .interactable(false)
            .show(ctx, |ui| {
                for t in &self.toasts {
                    Frame::new()
                        .fill(SURFACE)
                        .stroke(Stroke::new(1.0, if t.error { DANGER } else { BORDER }))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::symmetric(12, 8))
                        .show(ui, |ui| {
                            ui.set_max_width(420.0);
                            ui.label(RichText::new(&t.text).color(if t.error { DANGER } else { TEXT }));
                        });
                    ui.add_space(6.0);
                }
            });
    }
}

// ---------------------------------------------------------------- row helpers

fn scroll_area(tab: &mut Tab, id: (&str, u64), row_h: f32, per_row: usize) -> ScrollArea {
    let mut scroll = ScrollArea::vertical().auto_shrink([false; 2]).id_salt(id);
    let base = tab.force_offset.take();
    if tab.scroll_to_cursor || base.is_some() {
        let mut off = base.unwrap_or(tab.scroll_offset);
        if tab.scroll_to_cursor {
            if let Some(c) = tab.cursor {
                let top = (c / per_row.max(1)) as f32 * row_h;
                let h = tab.viewport_h.max(row_h);
                if top < off {
                    off = top;
                } else if top + row_h > off + h {
                    off = top + row_h - h;
                }
            }
            tab.scroll_to_cursor = false;
        }
        scroll = scroll.vertical_scroll_offset(off);
    }
    scroll
}

fn empty_state(ui: &mut Ui, tab: &Tab) {
    let msg = if let Some(e) = &tab.error {
        format!("Cannot open folder: {e}")
    } else if let Some(s) = &tab.search {
        if s.query.trim().is_empty() {
            "Type to search this folder and all subfolders".into()
        } else if s.running {
            "Searching…".into()
        } else {
            format!("Nothing found for \"{}\"", s.query.trim())
        }
    } else if tab.loading {
        "Loading…".into()
    } else if !tab.filter.is_empty() {
        format!("No items match \"{}\"", tab.filter)
    } else {
        "This folder is empty".into()
    };
    ui.add_space(40.0);
    ui.vertical_centered(|ui| ui.label(RichText::new(msg).color(DIM)));
}

fn drop_hover(resp: &egui::Response, target: &Path) -> bool {
    resp.dnd_hover_payload::<DragPaths>().is_some_and(|p| !p.0.iter().any(|s| s == target))
}

/// Dropping onto empty space in a pane moves/copies into that pane's folder.
fn background_drop(ui: &mut Ui, p: usize, tab: &Tab, rect: Rect, actions: &mut Vec<Action>) {
    if tab.search.is_some() {
        return;
    }
    let resp = ui.interact(rect, ui.id().with(("pane_drop", p)), Sense::hover());
    if let Some(paths) = resp.dnd_release_payload::<DragPaths>() {
        let copy = ui.input(|i| i.modifiers.ctrl);
        actions.push(Action::Drop { paths: paths.0.clone(), dest: tab.path.clone(), copy });
    }
}

#[allow(clippy::too_many_arguments)]
fn row_interactions(
    ui: &Ui,
    resp: &egui::Response,
    tab: &Tab,
    vi: usize,
    e: &Entry,
    (ctrl, shift): (bool, bool),
    actions: &mut Vec<Action>,
    p: usize,
) {
    if resp.drag_started() {
        let paths = if tab.is_selected(vi) { tab.targets() } else { vec![e.path.clone()] };
        DragAndDrop::set_payload(ui.ctx(), DragPaths(paths));
    }
    if let Some(paths) = resp.dnd_release_payload::<DragPaths>() {
        if e.is_dir && !paths.0.contains(&e.path) {
            actions.push(Action::Drop { paths: paths.0.clone(), dest: e.path.clone(), copy: ctrl });
        }
    }
    if resp.double_clicked() {
        actions.push(Action::FocusPane(p));
        actions.push(Action::OpenEntry(vi));
    } else if resp.clicked() {
        actions.push(Action::FocusPane(p));
        actions.push(Action::Click { vi, ctrl, shift });
    } else if resp.secondary_clicked() && !tab.is_selected(vi) {
        actions.push(Action::FocusPane(p));
        actions.push(Action::Click { vi, ctrl: false, shift: false });
    }
    if resp.middle_clicked() && e.is_dir {
        actions.push(Action::NewTab(e.path.clone()));
    }
    resp.context_menu(|ui| {
        ui.set_min_width(190.0);
        let mut item = |ui: &mut Ui, label: &str, keys: &str, action: Action| {
            let r = ui.add(egui::Button::new(label).shortcut_text(keys));
            if r.clicked() {
                actions.push(Action::FocusPane(p));
                actions.push(action);
                ui.close();
            }
        };
        item(ui, "Open", "Enter", Action::OpenEntry(vi));
        if e.is_dir {
            item(ui, "Open in new tab", "", Action::NewTab(e.path.clone()));
        }
        if tab.search.is_some() {
            item(ui, "Show in folder", "", Action::Reveal(e.path.clone()));
        }
        ui.separator();
        item(ui, "Cut", "Ctrl+X", Action::Cmd(Cmd::Cut));
        item(ui, "Copy", "Ctrl+C", Action::Cmd(Cmd::Copy));
        if tab.search.is_none() {
            item(ui, "Paste", "Ctrl+V", Action::Cmd(Cmd::Paste));
        }
        item(ui, "Duplicate", "Ctrl+D", Action::Cmd(Cmd::Duplicate));
        item(ui, "Rename", "F2", Action::Cmd(Cmd::Rename));
        ui.separator();
        item(ui, "Copy path", "Alt+C", Action::CopyPath(e.path.clone()));
        let term = if e.is_dir { e.path.clone() } else { tab.path.clone() };
        item(ui, "Open terminal here", "", Action::OpenTerminal(term));
        ui.separator();
        item(ui, "Move to trash", "Delete", Action::Cmd(Cmd::Trash));
        item(ui, "Delete permanently", "Shift+Del", Action::Cmd(Cmd::DeletePermanently));
    });
}

fn paint_row(
    painter: &Painter,
    rect: Rect,
    cols: &Columns,
    e: &Entry,
    sizes: &HashMap<PathBuf, u64>,
    sizing: &std::collections::HashSet<PathBuf>,
    faded: bool,
) {
    let cy = rect.center().y;
    let alpha = if faded { 0.45 } else { 1.0 };
    let cat = util::category(&e.ext, e.is_dir);
    draw_icon(painter, Pos2::new(cols.name.0 - 12.0, cy), cat);
    let name_color = if e.hidden { DIM } else { TEXT };
    let name_clip = painter.with_clip_rect(Rect::from_x_y_ranges(cols.name.0..=cols.name.1, rect.y_range()));
    let name_rect = name_clip.text(
        Pos2::new(cols.name.0, cy),
        Align2::LEFT_CENTER,
        &e.name,
        FontId::proportional(BODY),
        name_color.gamma_multiply(alpha),
    );
    if e.is_link {
        painter.text(Pos2::new(cols.name.0 - 6.0, cy + 6.0), Align2::CENTER_CENTER, "↗", FontId::proportional(9.0), ACCENT);
    }
    let small = FontId::proportional(12.5);
    let size_text = if e.is_dir {
        match sizes.get(&e.path) {
            Some(s) => util::human_size(*s),
            None if sizing.contains(&e.path) => "…".into(),
            None => String::new(),
        }
    } else {
        util::human_size(e.size)
    };
    if let Some(size) = cols.size {
        painter.text(Pos2::new(size.1, cy), Align2::RIGHT_CENTER, size_text, small.clone(), DIM.gamma_multiply(alpha));
    }
    if let (Some(m), Some(date)) = (e.modified, cols.date) {
        painter.text(Pos2::new(date.0, cy), Align2::LEFT_CENTER, util::fmt_time(m), small.clone(), DIM.gamma_multiply(alpha));
    }
    if let Some(k) = cols.kind {
        // Search results show where the match lives instead of the type.
        let text = match &e.rel {
            Some(rel) if rel.is_empty() => ".".to_string(),
            Some(rel) => rel.clone(),
            None if e.is_dir => "Folder".into(),
            None if e.ext.is_empty() => "File".into(),
            None => e.ext.to_uppercase(),
        };
        painter
            .with_clip_rect(Rect::from_x_y_ranges(k.0..=k.1, rect.y_range()))
            .text(Pos2::new(k.0, cy), Align2::LEFT_CENTER, text, small, FAINT);
    } else if let Some(rel) = e.rel.as_ref().filter(|r| !r.is_empty()) {
        name_clip.text(name_rect.right_center() + vec2(8.0, 0.0), Align2::LEFT_CENTER, rel, small, FAINT);
    }
}

fn paint_tile(painter: &Painter, thumbs: &mut Thumbs, tile: Rect, e: &Entry) {
    let thumb_area = Rect::from_min_size(tile.min + vec2(8.0, 8.0), vec2(tile.width() - 16.0, 86.0));
    let renderer = if e.is_dir { None } else { thumbs::renderer_for(&e.ext) };
    let mut drew = false;
    if let Some(r) = renderer {
        if let Some(tex) = thumbs.get(&e.path, r) {
            let ts = tex.size_vec2();
            let scale = (thumb_area.width() / ts.x).min(thumb_area.height() / ts.y).min(1.0);
            let rect = Rect::from_center_size(thumb_area.center(), ts * scale);
            painter.image(tex.id(), rect, UV, Color32::WHITE);
            drew = true;
        }
    }
    if !drew {
        let cat = util::category(&e.ext, e.is_dir);
        draw_icon_scaled(painter, thumb_area.center(), cat, 3.6);
    }
    let mut job = egui::text::LayoutJob::simple(e.name.clone(), FontId::proportional(12.5), if e.hidden { DIM } else { TEXT }, tile.width() - 12.0);
    job.wrap.max_rows = 2;
    job.wrap.break_anywhere = true;
    job.halign = Align::Center;
    let galley = painter.layout_job(job);
    painter.galley(Pos2::new(tile.center().x, thumb_area.bottom() + 8.0), galley, TEXT);
}

// ---------------------------------------------------------------- dialogs

fn name_field(ui: &mut Ui, title: &str, name: &mut String, focus: &mut bool, select: usize, confirm: &mut bool) {
    ui.label(RichText::new(title).size(15.0).strong());
    ui.add_space(8.0);
    let id = egui::Id::new("dialog_name");
    let resp = ui.add(
        egui::TextEdit::singleline(name)
            .id(id)
            .desired_width(f32::INFINITY)
            .margin(Margin::symmetric(8, 5)),
    );
    if *focus {
        *focus = false;
        resp.request_focus();
        if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), id) {
            let range = egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(select));
            state.cursor.set_char_range(Some(range));
            state.store(ui.ctx(), id);
        }
    }
    if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
        *confirm = true;
    }
}

fn dialog_buttons(ui: &mut Ui, ok: &str, danger: bool, confirm: &mut bool, cancel: &mut bool) {
    ui.add_space(14.0);
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let ok_button = egui::Button::new(RichText::new(ok).color(Color32::WHITE))
            .fill(if danger { DANGER } else { ACCENT_STRONG })
            .min_size(vec2(84.0, 28.0));
        if ui.add(ok_button).clicked() {
            *confirm = true;
        }
        if ui.add(egui::Button::new("Cancel").fill(SURFACE).min_size(vec2(84.0, 28.0))).clicked() {
            *cancel = true;
        }
    });
}

fn stem_chars(name: &str) -> usize {
    match name.rfind('.') {
        Some(i) if i > 0 => name[..i].chars().count(),
        _ => name.chars().count(),
    }
}

// ---------------------------------------------------------------- drawing helpers

const UV: Rect = Rect { min: Pos2::new(0.0, 0.0), max: Pos2::new(1.0, 1.0) };

fn ops_name(p: &Path) -> String {
    crate::ops::file_name(p)
}

struct Columns {
    name: (f32, f32),
    size: Option<(f32, f32)>,
    date: Option<(f32, f32)>,
    kind: Option<(f32, f32)>,
}

impl Columns {
    /// Lays out columns right-to-left, dropping Type and then Modified when narrow.
    fn new(left: f32, right: f32) -> Self {
        let width = right - left;
        let mut x = right - 8.0;
        let kind = (width > 560.0).then(|| {
            let r = (x - 110.0, x);
            x = r.0;
            r
        });
        let date = (width > 440.0).then(|| {
            let r = (x - 130.0, x - 12.0);
            x = r.0;
            r
        });
        let size = (width > 260.0).then(|| {
            let r = (x - 96.0, x - 20.0);
            x = r.0;
            r
        });
        let name = (left + 34.0, x - 8.0);
        Self { name, size, date, kind }
    }
}

#[derive(Clone, Copy)]
enum Glyph {
    Back,
    Forward,
    Up,
    Refresh,
    Plus,
    Eye,
    Panel,
    Split,
    Grid,
    Search,
}

fn glyph_button(ui: &mut Ui, glyph: Glyph, enabled: bool, on: bool, tip: &str) -> bool {
    let resp = ui
        .add_enabled(enabled, egui::Button::new("").frame_when_inactive(false).min_size(vec2(28.0, 26.0)))
        .on_hover_text(tip);
    let color = if !enabled {
        FAINT
    } else if on {
        ACCENT
    } else if resp.hovered() {
        TEXT
    } else {
        DIM
    };
    paint_glyph(ui.painter(), resp.rect.center(), glyph, color);
    resp.clicked()
}

fn paint_glyph(p: &Painter, c: Pos2, glyph: Glyph, color: Color32) {
    let stroke = Stroke::new(1.6, color);
    let pt = |x: f32, y: f32| c + vec2(x, y);
    match glyph {
        Glyph::Back => {
            p.line(vec![pt(2.5, -5.0), pt(-2.5, 0.0), pt(2.5, 5.0)], stroke);
        }
        Glyph::Forward => {
            p.line(vec![pt(-2.5, -5.0), pt(2.5, 0.0), pt(-2.5, 5.0)], stroke);
        }
        Glyph::Up => {
            p.line(vec![pt(-5.0, 2.5), pt(0.0, -2.5), pt(5.0, 2.5)], stroke);
        }
        Glyph::Plus => {
            p.line_segment([pt(-5.0, 0.0), pt(5.0, 0.0)], stroke);
            p.line_segment([pt(0.0, -5.0), pt(0.0, 5.0)], stroke);
        }
        Glyph::Refresh => {
            let r = 5.5;
            let arc: Vec<Pos2> = (0..=24)
                .map(|i| {
                    let a = -0.9 + i as f32 / 24.0 * 5.0;
                    pt(r * a.cos(), r * a.sin())
                })
                .collect();
            let tip = arc[0];
            p.line(arc, stroke);
            p.line(vec![tip + vec2(-3.5, -0.5), tip, tip + vec2(0.5, -3.5)], stroke);
        }
        Glyph::Eye => {
            let top: Vec<Pos2> = (0..=16)
                .map(|i| {
                    let t = i as f32 / 16.0 * std::f32::consts::PI;
                    pt(-7.0 * t.cos(), -4.5 * t.sin())
                })
                .collect();
            let bottom: Vec<Pos2> = top.iter().map(|q| Pos2::new(q.x, 2.0 * c.y - q.y)).collect();
            p.line(top, stroke);
            p.line(bottom, stroke);
            p.circle_filled(c, 2.0, color);
        }
        Glyph::Panel => {
            let r = Rect::from_center_size(c, vec2(15.0, 12.0));
            p.rect_stroke(r, CornerRadius::same(2), stroke, egui::StrokeKind::Middle);
            let side = Rect::from_min_max(Pos2::new(r.right() - 5.5, r.top()), r.max);
            p.rect_filled(side, CornerRadius { nw: 0, sw: 0, ne: 2, se: 2 }, color);
        }
        Glyph::Split => {
            let r = Rect::from_center_size(c, vec2(15.0, 12.0));
            p.rect_stroke(r, CornerRadius::same(2), stroke, egui::StrokeKind::Middle);
            p.line_segment([Pos2::new(c.x, r.top()), Pos2::new(c.x, r.bottom())], stroke);
        }
        Glyph::Grid => {
            for (dx, dy) in [(-3.5, -3.5), (3.5, -3.5), (-3.5, 3.5), (3.5, 3.5)] {
                p.rect_stroke(
                    Rect::from_center_size(pt(dx, dy), vec2(5.0, 5.0)),
                    CornerRadius::same(1),
                    Stroke::new(1.3, color),
                    egui::StrokeKind::Middle,
                );
            }
        }
        Glyph::Search => {
            p.circle_stroke(pt(-1.5, -1.5), 4.5, stroke);
            p.line_segment([pt(1.8, 1.8), pt(5.5, 5.5)], Stroke::new(2.0, color));
        }
    }
}

fn sort_arrow(p: &Painter, c: Pos2, ascending: bool) {
    let (a, b) = if ascending { (-2.0, 2.0) } else { (2.0, -2.0) };
    p.line(vec![c + vec2(-3.5, -a), c + vec2(0.0, -b), c + vec2(3.5, -a)], Stroke::new(1.4, ACCENT));
}

fn category_color(cat: Category) -> Color32 {
    match cat {
        Category::Folder => FOLDER,
        Category::Image => Color32::from_rgb(0xc6, 0x78, 0xdd),
        Category::Video => Color32::from_rgb(0xe0, 0x6c, 0x75),
        Category::Audio => Color32::from_rgb(0x56, 0xb6, 0xc2),
        Category::Archive => Color32::from_rgb(0xd1, 0x9a, 0x66),
        Category::Code => Color32::from_rgb(0x61, 0xaf, 0xef),
        Category::Text => Color32::from_rgb(0xab, 0xb2, 0xbf),
        Category::Document => Color32::from_rgb(0xe5, 0xc0, 0x7b),
        Category::Executable => Color32::from_rgb(0x98, 0xc3, 0x79),
        Category::Other => Color32::from_rgb(0x7f, 0x84, 0x8e),
    }
}

fn draw_icon(p: &Painter, c: Pos2, cat: Category) {
    draw_icon_scaled(p, c, cat, 1.0);
}

fn draw_icon_scaled(p: &Painter, c: Pos2, cat: Category, s: f32) {
    if cat == Category::Folder {
        draw_folder(p, c, s);
        return;
    }
    let color = category_color(cat);
    let r = Rect::from_center_size(c, vec2(11.0, 14.0) * s);
    let radius = CornerRadius::same((2.0 * s).min(255.0) as u8);
    p.rect_filled(r, radius, color.gamma_multiply(0.22));
    p.rect_stroke(r, radius, Stroke::new(s.sqrt(), color), egui::StrokeKind::Inside);
    let band = Rect::from_min_size(Pos2::new(r.left(), r.bottom() - 4.0 * s), vec2(r.width(), 4.0 * s));
    p.rect_filled(band, CornerRadius { nw: 0, ne: 0, sw: radius.sw, se: radius.se }, color);
}

fn draw_folder(p: &Painter, c: Pos2, s: f32) {
    let body = Rect::from_center_size(c + vec2(0.0, 1.0) * s, vec2(15.0, 11.0) * s);
    let tab = Rect::from_min_size(body.left_top() - vec2(0.0, 2.0 * s), vec2(6.5, 4.0) * s);
    let radius = CornerRadius::same((1.5 * s).min(255.0) as u8);
    p.rect_filled(tab, radius, FOLDER.gamma_multiply(0.75));
    p.rect_filled(body, radius, FOLDER);
}

fn draw_checker(p: &Painter, r: Rect) {
    let cell = 8.0;
    p.rect_filled(r, CornerRadius::ZERO, Color32::from_gray(0x2a));
    let (nx, ny) = ((r.width() / cell).ceil() as usize, (r.height() / cell).ceil() as usize);
    if nx * ny > 40_000 {
        return;
    }
    let painter = p.with_clip_rect(r);
    for y in 0..ny {
        for x in (y % 2..nx).step_by(2) {
            let min = r.min + vec2(x as f32, y as f32) * cell;
            painter.rect_filled(Rect::from_min_size(min, vec2(cell, cell)), CornerRadius::ZERO, Color32::from_gray(0x33));
        }
    }
}

fn expand_tilde(s: &str) -> PathBuf {
    if let Some(rest) = s.strip_prefix('~') {
        return fsops::home_dir().join(rest.trim_start_matches('/'));
    }
    PathBuf::from(s)
}
