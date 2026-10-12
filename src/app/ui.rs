use super::icons::*;
use super::{Action, App, Dialog, DragPaths};
use crate::complete;
use crate::fsops::{self, Entry};
use crate::palette::{self, COMMANDS, Cmd};
use crate::preview::{Content, State};
use crate::tab::{ICON_SIZES, SortKey, Tab, ViewMode};
use crate::theme::*;
use crate::thumbs::{self, Thumbs};
use crate::util;
use eframe::egui::{
    self, Align, Align2, CentralPanel, Color32, CornerRadius, DragAndDrop, FontId, Frame, Key, Layout, Margin,
    Modifiers, Painter, Panel, Pos2, Rect, RichText, ScrollArea, Sense, Stroke, Ui, UiBuilder, vec2,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

const ROW_H: f32 = 26.0;

impl App {
    pub(super) fn draw(&mut self, ui: &mut Ui) {
        let mut actions = Vec::new();
        ui.ctx().data_mut(|d| {
            d.remove_temp::<PathBuf>(drop_target_id());
            d.remove_temp::<bool>(bookmark_hover_id());
            let rects = d.get_temp::<Vec<(Rect, PathBuf)>>(folder_rects_id()).unwrap_or_default();
            self.folder_rects = rects;
            d.remove_temp::<Vec<(Rect, PathBuf)>>(folder_rects_id());
        });
        self.dragging = DragAndDrop::payload::<DragPaths>(ui.ctx());

        Panel::bottom("status")
            .exact_size(26.0)
            .frame(Frame::new().fill(pal().panel).inner_margin(Margin::symmetric(12, 2)))
            .show(ui, |ui| self.status_bar(ui));
        // Side panels yield to the file list in narrow windows.
        let width = ui.available_width();
        // Both can be toggled, or dragged closed past their minimum width and back open.
        let mut show = self.show_sidebar;
        Panel::left("sidebar")
            .default_size(self.sidebar_width)
            .min_size(120.0_f32.min(width * 0.25))
            .max_size((width * 0.25).max(120.0))
            .drag_to_open(true)
            .frame(Frame::new().fill(pal().panel).inner_margin(Margin::symmetric(8, 4)))
            .show_collapsible(ui, &mut show, |ui| self.sidebar(ui, &mut actions));
        self.show_sidebar = show;
        let mut show = self.show_preview;
        Panel::right("preview")
            .default_size(self.preview_width)
            .min_size(160.0_f32.min(width * 0.4))
            .max_size((width * 0.4).max(160.0))
            .drag_to_open(true)
            .frame(Frame::new().fill(pal().panel).inner_margin(Margin::symmetric(12, 4)))
            .show_collapsible(ui, &mut show, |ui| self.preview_panel(ui));
        self.show_preview = show;
        // Remember widths only while shown; a folded panel reports its animation size.
        let width_of = |id: &str| egui::containers::panel::PanelState::load(ui.ctx(), egui::Id::new(id)).map(|s| s.size().x);
        if self.show_sidebar && let Some(w) = width_of("sidebar") {
            self.sidebar_width = w;
        }
        if self.show_preview && let Some(w) = width_of("preview") {
            self.preview_width = w;
        }
        CentralPanel::default().frame(Frame::new().fill(pal().bg)).show(ui, |ui| {
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
                ui.painter().vline(mid, full.y_range(), Stroke::new(2.0, pal().border));
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
        self.maybe_drag_out(ui.ctx());
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
        let title = format!("{} — Kestrel", self.tab().path.display());
        if title != self.window_title {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }
    }

    /// The editable path field with a dropdown of matching folders.
    fn path_editor(&mut self, ui: &mut Ui, p: usize, actions: &mut Vec<Action>) {
        let id = egui::Id::new(("path_edit", p));
        let Some((_, text)) = self.path_edit.as_mut() else { return };
        let cands = self.completer.candidates(text, self.show_hidden);
        let shown = cands.len().min(complete::SHOWN);
        if self.path_hl.is_some_and(|h| h >= shown) {
            self.path_hl = None;
        }
        // Take the keys the completion uses before the text field sees them.
        let focused = ui.memory(|m| m.has_focus(id));
        let (tab, down, up) = if focused && shown > 0 {
            ui.input_mut(|i| {
                (
                    i.consume_key(Modifiers::NONE, Key::Tab),
                    i.consume_key(Modifiers::NONE, Key::ArrowDown),
                    i.consume_key(Modifiers::NONE, Key::ArrowUp),
                )
            })
        } else {
            (false, false, false)
        };
        if down {
            self.path_hl = Some(self.path_hl.map_or(0, |h| (h + 1) % shown));
        }
        if up {
            self.path_hl = Some(self.path_hl.map_or(shown - 1, |h| (h + shown - 1) % shown));
        }
        let mut replaced = None;
        if tab {
            // Tab takes the highlighted folder, else the shared prefix, else the first match.
            replaced = Some(match self.path_hl {
                Some(h) => cands[h].clone(),
                None => complete::common_prefix(&cands)
                    .filter(|c| c.chars().count() > text.chars().count())
                    .unwrap_or_else(|| cands[0].clone()),
            });
        }
        if let Some(new) = replaced {
            *text = new;
            self.path_hl = None;
            if let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), id) {
                let end = egui::text::CCursor::new(text.chars().count());
                st.cursor.set_char_range(Some(egui::text::CCursorRange::one(end)));
                st.store(ui.ctx(), id);
            }
        }

        let resp = ui.add(
            egui::TextEdit::singleline(text)
                .id(id)
                .lock_focus(true) // Tab completes instead of moving focus
                .frame(Frame::NONE)
                .desired_width(f32::INFINITY)
                .vertical_align(Align::Center),
        );
        if resp.changed() {
            self.path_hl = None;
        }
        if self.path_edit_focus {
            resp.request_focus();
            self.path_edit_focus = false;
        }

        // Dropdown. Items react on press: the click would otherwise end editing first.
        let mut picked = None;
        if shown > 0 && (resp.has_focus() || resp.lost_focus()) {
            let field = ui.min_rect().expand2(vec2(6.0, 2.0));
            let screen = ui.ctx().content_rect();
            let width = field.width().max(320.0_f32.min(screen.width() - 16.0));
            let x = field.left().min(screen.right() - width - 8.0).max(screen.left() + 8.0);
            egui::Area::new(id.with("popup"))
                .order(egui::Order::Foreground)
                .fixed_pos(Pos2::new(x, field.bottom() + 4.0))
                .show(ui.ctx(), |ui| {
                    Frame::new()
                        .fill(pal().panel)
                        .stroke(Stroke::new(1.0, pal().border))
                        .corner_radius(CornerRadius::same(6))
                        .inner_margin(Margin::same(4))
                        .show(ui, |ui| {
                            ui.set_width(width - 8.0);
                            ui.spacing_mut().item_spacing.y = 0.0;
                            for (n, c) in cands.iter().take(shown).enumerate() {
                                let (rect, item) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click());
                                let lit = self.path_hl == Some(n);
                                if lit || item.hovered() {
                                    ui.painter().rect_filled(rect, CornerRadius::same(4), if lit { pal().select } else { pal().hover });
                                }
                                draw_folder(ui.painter(), Pos2::new(rect.left() + 14.0, rect.center().y), 0.8);
                                let name = c.trim_end_matches('/').rsplit('/').next().unwrap_or(c);
                                ui.painter().with_clip_rect(rect).text(
                                    Pos2::new(rect.left() + 28.0, rect.center().y),
                                    Align2::LEFT_CENTER,
                                    name,
                                    FontId::proportional(BODY),
                                    pal().text,
                                );
                                if item.hovered() && ui.input(|i| i.pointer.primary_pressed()) {
                                    picked = Some(c.clone());
                                }
                            }
                            if cands.len() > shown {
                                ui.label(RichText::new(format!("  {} more…", cands.len() - shown)).size(12.0).color(pal().faint));
                            }
                        });
                });
        }

        let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        let target = picked.or_else(|| enter.then(|| self.path_hl.map_or_else(|| text.clone(), |h| cands[h].clone())));
        if let Some(t) = target {
            // Rebuilding from components drops trailing slashes and "./".
            let target: PathBuf = complete::expand_tilde(t.trim()).components().collect();
            if target.is_dir() {
                actions.push(Action::FocusPane(p));
                actions.push(Action::Navigate(target));
            } else if target.is_file() {
                fsops::open_default(&target);
            }
            self.path_edit = None;
        } else if resp.lost_focus() {
            self.path_edit = None;
        }
        if self.path_edit.is_none() {
            self.path_hl = None;
        }
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
            .frame(Frame::new().fill(pal().bg).inner_margin(Margin::symmetric(8, 4)))
            .show_separator_line(false)
            .show(ui, |ui| self.tab_bar(ui, p, actions));
        Panel::top(egui::Id::new(("toolbar", p)))
            .exact_size(38.0)
            .frame(Frame::new().fill(pal().panel).inner_margin(Margin::symmetric(8, 4)))
            .show(ui, |ui| self.toolbar(ui, p, actions));
        CentralPanel::default()
            .frame(Frame::new().fill(pal().bg).inner_margin(Margin::symmetric(4, 0)))
            .show(ui, |ui| {
                self.zoom_wheel(ui, p, actions);
                let pane = &self.panes[p];
                if pane.tabs[pane.active].mode.is_grid() {
                    self.file_grid(ui, p, actions);
                } else {
                    self.file_list(ui, p, actions);
                }
            });
    }

    /// Ctrl+wheel over the file area steps through the view modes.
    fn zoom_wheel(&mut self, ui: &mut Ui, p: usize, actions: &mut Vec<Action>) {
        let rect = ui.max_rect();
        let delta = ui.input(|i| {
            if !i.pointer.hover_pos().is_some_and(|pos| rect.contains(pos)) {
                return 0.0;
            }
            i.raw
                .events
                .iter()
                .map(|e| match e {
                    egui::Event::MouseWheel { unit, delta, modifiers, .. } if modifiers.ctrl || modifiers.command => match unit {
                        egui::MouseWheelUnit::Point => delta.y / 60.0,
                        _ => delta.y.signum(),
                    },
                    _ => 0.0,
                })
                .sum::<f32>()
        });
        if delta == 0.0 {
            return;
        }
        // Touchpads send many small deltas; accumulate them into whole steps.
        let acc = &mut self.zoom_accum;
        *acc += delta;
        let steps = acc.trunc() as i32;
        if steps != 0 {
            *acc -= steps as f32;
            actions.push(Action::FocusPane(p));
            actions.push(Action::Zoom(steps));
        }
    }

    fn tab_bar(&mut self, ui: &mut Ui, p: usize, actions: &mut Vec<Action>) {
        let pane_active = p == self.active_pane;
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let pane = &self.panes[p];
            for (i, tab) in pane.tabs.iter().enumerate() {
                let active = i == pane.active;
                let galley = ui.painter().layout_no_wrap(tab.title(), FontId::proportional(13.0), pal().text);
                let w = (galley.size().x + 44.0).clamp(90.0, 220.0);
                let (rect, resp) = ui.allocate_exact_size(vec2(w, 26.0), Sense::click());
                let close_rect =
                    Rect::from_center_size(Pos2::new(rect.right() - 13.0, rect.center().y), vec2(16.0, 16.0));
                let close = ui.interact(close_rect, ui.id().with(("tab_close", tab.id)), Sense::click());
                let painter = ui.painter();
                let drop_hover = resp.dnd_hover_payload::<DragPaths>().is_some();
                let bg = if active || drop_hover {
                    pal().surface
                } else if resp.hovered() {
                    pal().hover
                } else {
                    Color32::TRANSPARENT
                };
                painter.rect_filled(rect, CornerRadius::same(6), bg);
                if active {
                    let line = Rect::from_min_size(
                        Pos2::new(rect.left() + 8.0, rect.bottom() - 2.0),
                        vec2(rect.width() - 16.0, 2.0),
                    );
                    painter.rect_filled(line, CornerRadius::same(1), if pane_active { pal().accent } else { pal().faint });
                }
                draw_folder(painter, Pos2::new(rect.left() + 14.0, rect.center().y), 0.8);
                let text_rect = Rect::from_min_max(
                    Pos2::new(rect.left() + 24.0, rect.top()),
                    Pos2::new(rect.right() - 24.0, rect.bottom()),
                );
                let color = if active { pal().text } else { pal().dim };
                painter
                    .with_clip_rect(text_rect)
                    .galley(Pos2::new(text_rect.left(), rect.center().y - galley.size().y / 2.0), galley, color);
                if resp.hovered() || close.hovered() || active {
                    if close.hovered() {
                        painter.rect_filled(close_rect, CornerRadius::same(4), pal().hover);
                    }
                    painter.text(close_rect.center(), Align2::CENTER_CENTER, "×", FontId::proportional(14.0), pal().dim);
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
            if p == 0 {
                let tip = if self.show_sidebar { "Hide sidebar (Ctrl+B)" } else { "Show sidebar (Ctrl+B)" };
                if glyph_button(ui, Glyph::Sidebar, true, self.show_sidebar, tip) {
                    actions.push(Action::Cmd(Cmd::ToggleSidebar));
                }
            }
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
            // Narrow panes drop the buttons that have shortcuts, so the path stays usable.
            let editing = self.path_edit.as_ref().is_some_and(|(ep, _)| *ep == p);
            let compact = ui.available_width() < if editing { 560.0 } else { 400.0 };
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if last_pane {
                    let tip = if self.show_preview { "Hide preview (F3)" } else { "Show preview (F3)" };
                    if glyph_button(ui, Glyph::Panel, true, self.show_preview, tip) {
                        actions.push(Action::Cmd(Cmd::TogglePreview));
                    }
                    if !compact {
                        if glyph_button(ui, Glyph::Eye, true, self.show_hidden, "Show hidden files (Ctrl+H)") {
                            actions.push(Action::Cmd(Cmd::ToggleHidden));
                        }
                        if glyph_button(ui, Glyph::Split, true, self.panes.len() > 1, "Split view (F6)") {
                            actions.push(Action::Cmd(Cmd::ToggleSplit));
                        }
                    }
                }
                let pane = &self.panes[p];
                let tab = &pane.tabs[pane.active];
                let grid = tab.mode.is_grid();
                if !compact
                    && glyph_button(ui, Glyph::Grid, true, grid, "Icons / details view (Ctrl+G, Ctrl+scroll to resize)")
                {
                    actions.push(Action::FocusPane(p));
                    actions.push(Action::Cmd(Cmd::ToggleGrid));
                }
                if (!compact || tab.search.is_some())
                    && glyph_button(ui, Glyph::Search, true, tab.search.is_some(), "Search subfolders (Ctrl+F)")
                {
                    actions.push(Action::FocusPane(p));
                    actions.push(if tab.search.is_some() { Action::StopSearch } else { Action::Cmd(Cmd::Search) });
                }
                if !tab.filter.is_empty() {
                    let chip = Frame::new()
                        .fill(pal().select)
                        .corner_radius(CornerRadius::same(5))
                        .inner_margin(Margin::symmetric(8, 3))
                        .show(ui, |ui| ui.label(RichText::new(format!("Filter: {}  ×", tab.filter)).color(pal().text)));
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
            .fill(pal().bg)
            .corner_radius(CornerRadius::same(6))
            .stroke(Stroke::new(1.0, if self.tab_in(p).search.is_some() { pal().accent } else { pal().border }))
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
                    paint_glyph(ui.painter(), ui.cursor().min + vec2(8.0, 12.0), Glyph::Search, pal().dim);
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

            if self.path_edit.as_ref().is_some_and(|(ep, _)| *ep == p) {
                self.path_editor(ui, p, actions);
                return;
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
                let sep_w = ui.painter().layout_no_wrap(" › ".into(), font.clone(), pal().faint).size().x;
                let widths: Vec<f32> = parts
                    .iter()
                    .map(|(_, l)| ui.painter().layout_no_wrap(l.clone(), font.clone(), pal().text).size().x + pad + sep_w)
                    .collect();
                let mut skip = 0;
                let ellipsis_w = 30.0;
                while skip + 1 < parts.len() && widths[skip..].iter().sum::<f32>() + if skip > 0 { ellipsis_w } else { 0.0 } > avail - 8.0 {
                    skip += 1;
                }
                if skip > 0 {
                    let hidden = parts[skip - 1].0.to_path_buf();
                    let r = ui.add(egui::Button::new(RichText::new("…").color(pal().dim)).frame_when_inactive(false));
                    if r.on_hover_text(hidden.display().to_string()).clicked() {
                        actions.push(Action::FocusPane(p));
                        actions.push(Action::Navigate(hidden));
                    }
                }
                let last_index = parts.len() - 1;
                for (n, (part, label)) in parts.into_iter().enumerate().skip(skip) {
                    if n > 1 || (skip > 0 && n > 0) {
                        ui.label(RichText::new(" › ").color(pal().faint));
                    }
                    let text = RichText::new(label).color(if n == last_index { pal().text } else { pal().dim });
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
        let dragging = self.dragging.as_deref();
        let bookmarks = &self.places.bookmarks;
        #[cfg(test)]
        let row_rects = &mut self.row_rects;
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
            let color = if active || resp.hovered() { pal().text } else { pal().dim };
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
        ui.painter().hline(full.x_range(), hrect.bottom(), Stroke::new(1.0, pal().border));

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
                if e.is_dir {
                    record_folder(ui.ctx(), row, &e.path);
                }
                if selected {
                    painter.rect_filled(row, CornerRadius::same(5), if pane_active { pal().select } else { pal().select_dim });
                } else if resp.hovered() || drop_target {
                    painter.rect_filled(row, CornerRadius::same(5), pal().hover);
                }
                if drop_target {
                    painter.rect_stroke(row, CornerRadius::same(5), Stroke::new(1.5, pal().accent), egui::StrokeKind::Inside);
                }
                if tab.cursor == Some(vi) && !tab.marked.is_empty() && pane_active {
                    painter.rect_stroke(row, CornerRadius::same(5), Stroke::new(1.0, pal().accent), egui::StrokeKind::Inside);
                }
                let faded = cut.is_some_and(|c| c.contains(&e.path)) || dragging.is_some_and(|d| d.0.contains(&e.path));
                paint_row(painter, rect, &cols, e, sizes, sizing, faded);
                #[cfg(test)]
                row_rects.insert(e.path.clone(), rect);

                row_interactions(ui, &resp, tab, vi, e, ctrl_shift, actions, p, bookmarks);
            }
        });
        tab.scroll_offset = out.state.offset.y;
        tab.viewport_h = out.inner_rect.height();
        drag_autoscroll(ui, tab, out.inner_rect);
        background_drop(ui, p, tab, out.inner_rect, actions);
    }

    // ------------------------------------------------------------ grid view

    fn file_grid(&mut self, ui: &mut Ui, p: usize, actions: &mut Vec<Action>) {
        let pane_active = p == self.active_pane;
        let thumbs = &mut self.thumbs;
        let bookmarks = &self.places.bookmarks;
        let cut = self.clip.as_ref().filter(|c| c.cut).map(|c| &c.paths);
        let dragging = self.dragging.as_deref();
        let pane = &mut self.panes[p];
        let tab = &mut pane.tabs[pane.active];
        let full = ui.available_rect_before_wrap();
        let mode = tab.mode;
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
            background_drop(ui, p, tab, full, actions);
            return;
        }
        let rows = tab.view.len().div_ceil(cols);
        let scroll = scroll_area(tab, ("grid", tab.id), cell.y, cols);
        let ctrl_shift = ui.input(|i| (i.modifiers.ctrl, i.modifiers.shift));
        let gap = ((full.width() - 8.0) - cols as f32 * cell.x) / cols as f32;
        let out = scroll.show_rows(ui, cell.y, rows, |ui, range| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            for r in range {
                ui.horizontal(|ui| {
                    ui.add_space(4.0);
                    for c in 0..cols {
                        let vi = r * cols + c;
                        let Some(&ei) = tab.view.get(vi) else { break };
                        let e = &tab.entries[ei];
                        let (rect, resp) = ui.allocate_exact_size(vec2(cell.x + gap, cell.y), Sense::click_and_drag());
                        let tile = match mode {
                            ViewMode::Icons(_) => Rect::from_center_size(rect.center(), cell - vec2(8.0, 8.0)),
                            _ => rect.shrink2(vec2(4.0, 1.0)),
                        };
                        let selected = tab.is_selected(vi);
                        let drop_target = e.is_dir && drop_hover(&resp, &e.path);
                        if e.is_dir {
                            record_folder(ui.ctx(), tile, &e.path);
                        }
                        let painter = ui.painter();
                        let corner = CornerRadius::same(radius);
                        if selected {
                            painter.rect_filled(tile, corner, if pane_active { pal().select } else { pal().select_dim });
                        } else if resp.hovered() || drop_target {
                            painter.rect_filled(tile, corner, pal().hover);
                        }
                        if drop_target {
                            painter.rect_stroke(tile, corner, Stroke::new(1.5, pal().accent), egui::StrokeKind::Inside);
                        }
                        if tab.cursor == Some(vi) && !tab.marked.is_empty() && pane_active {
                            painter.rect_stroke(tile, corner, Stroke::new(1.0, pal().accent), egui::StrokeKind::Inside);
                        }
                        let faded = cut.is_some_and(|c| c.contains(&e.path))
                            || dragging.is_some_and(|d| d.0.contains(&e.path));
                        match mode {
                            ViewMode::Icons(_) => paint_tile(painter, thumbs, tile, e, faded),
                            _ => paint_cell(painter, tile, e, faded),
                        }
                        row_interactions(ui, &resp, tab, vi, e, ctrl_shift, actions, p, bookmarks);
                    }
                });
            }
        });
        tab.scroll_offset = out.state.offset.y;
        tab.viewport_h = out.inner_rect.height();
        drag_autoscroll(ui, tab, out.inner_rect);
        background_drop(ui, p, tab, out.inner_rect, actions);
    }

    // ------------------------------------------------------------ sidebar / status / preview

    fn sidebar(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        // Folders being dragged can be dropped on the Bookmarks section to bookmark them.
        let dragged_dirs: Vec<PathBuf> = self
            .dragging
            .as_ref()
            .map(|d| d.0.iter().filter(|p| p.is_dir()).cloned().collect())
            .unwrap_or_default();
        ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
            let cur = self.tab().path.clone();
            let sections: [(&'static str, &Vec<(String, PathBuf)>); 3] = [
                ("PLACES", &self.places.places),
                ("BOOKMARKS", &self.places.bookmarks),
                ("DEVICES", &self.places.devices),
            ];
            for (title, items) in sections {
                let bookmarks = title == "BOOKMARKS";
                let can_drop = bookmarks && !dragged_dirs.is_empty();
                if items.is_empty() && !can_drop {
                    continue;
                }
                ui.add_space(8.0);
                let collapsed = self.collapsed.contains(title);
                let (hrect, header) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::click());
                let hot = header.hovered();
                let color = if hot { pal().dim } else { pal().faint };
                ui.painter().text(
                    Pos2::new(hrect.left() + 2.0, hrect.center().y),
                    Align2::LEFT_CENTER,
                    title,
                    FontId::proportional(10.5),
                    color,
                );
                if hot || collapsed {
                    chevron(ui.painter(), Pos2::new(hrect.right() - 8.0, hrect.center().y), !collapsed, color);
                }
                if header.clicked() {
                    actions.push(Action::ToggleSection(title));
                }
                if can_drop && bookmark_zone(ui, &header, &dragged_dirs, actions) {
                    ui.painter().rect_stroke(hrect, CornerRadius::same(5), Stroke::new(1.5, pal().accent), egui::StrokeKind::Inside);
                }
                ui.add_space(2.0);
                if collapsed {
                    continue;
                }
                for (name, path) in items {
                    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click());
                    let painter = ui.painter();
                    let selected = *path == cur;
                    let drop_target = drop_hover(&resp, path);
                    record_folder(ui.ctx(), rect, path);
                    if selected {
                        painter.rect_filled(rect, CornerRadius::same(5), pal().select);
                    } else if resp.hovered() || drop_target {
                        painter.rect_filled(rect, CornerRadius::same(5), pal().hover);
                    }
                    if drop_target {
                        painter.rect_stroke(rect, CornerRadius::same(5), Stroke::new(1.5, pal().accent), egui::StrokeKind::Inside);
                    }
                    let kind = match title {
                        "DEVICES" => Some(fsops::Special::Drive),
                        _ => fsops::special(path),
                    };
                    let icon_color = if selected { pal().accent } else { pal().dim };
                    draw_place(painter, Pos2::new(rect.left() + 14.0, rect.center().y), kind, icon_color);
                    painter.with_clip_rect(rect.shrink(2.0)).text(
                        Pos2::new(rect.left() + 30.0, rect.center().y),
                        Align2::LEFT_CENTER,
                        name,
                        FontId::proportional(BODY),
                        if selected { pal().text } else { pal().dim },
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
                    let marked = bookmarks || self.places.bookmarks.iter().any(|(_, b)| b == path);
                    resp.context_menu(|ui| {
                        ui.set_min_width(180.0);
                        if ui.button("Open in new tab").clicked() {
                            actions.push(Action::NewTab(path.clone()));
                            ui.close();
                        }
                        if ui.button("Open terminal here").clicked() {
                            actions.push(Action::OpenTerminal(path.clone()));
                            ui.close();
                        }
                        ui.separator();
                        if marked {
                            if ui.button("Remove from bookmarks").clicked() {
                                actions.push(Action::Unbookmark(path.clone()));
                                ui.close();
                            }
                        } else if ui.button("Add to bookmarks").clicked() {
                            actions.push(Action::Bookmark(vec![path.clone()]));
                            ui.close();
                        }
                    });
                }
                // While folders are dragged, an explicit target under the bookmarks.
                if can_drop {
                    ui.add_space(2.0);
                    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::hover());
                    let hot = bookmark_zone(ui, &resp, &dragged_dirs, actions);
                    let painter = ui.painter();
                    painter.rect_filled(rect, CornerRadius::same(5), if hot { pal().select_dim } else { pal().bg });
                    painter.rect_stroke(
                        rect,
                        CornerRadius::same(5),
                        Stroke::new(1.0, if hot { pal().accent } else { pal().border }),
                        egui::StrokeKind::Inside,
                    );
                    draw_emblem(painter, Pos2::new(rect.left() + 14.0, rect.center().y), fsops::Special::Bookmark, 0.9, if hot { pal().accent } else { pal().dim }, 1.4);
                    painter.text(
                        Pos2::new(rect.left() + 30.0, rect.center().y),
                        Align2::LEFT_CENTER,
                        "Drop to bookmark",
                        FontId::proportional(12.5),
                        if hot { pal().text } else { pal().dim },
                    );
                }
            }
        });
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        let job = self.ops.snapshot();
        let queued = self.ops.queued.load(std::sync::atomic::Ordering::Relaxed);
        let tab = self.tab();
        let small = |s: String| RichText::new(s).size(12.0).color(pal().dim);
        ui.horizontal_centered(|ui| {
            if let Some(s) = &tab.search {
                let n = tab.view.len();
                let more = if s.truncated { "+" } else { "" };
                ui.label(small(format!("{n}{more} results")));
                if s.running {
                    ui.add(egui::Spinner::new().size(11.0).color(pal().dim));
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
                    ui.add(egui::Spinner::new().size(11.0).color(pal().dim));
                }
            }
            let n = tab.selection_count();
            if n > 1 {
                let bytes: u64 = tab
                    .entries
                    .iter()
                    .filter(|e| tab.marked.contains(&e.path))
                    .filter_map(|e| if e.is_dir { self.sizes.get(&e.path).copied() } else { Some(e.size) })
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
                        ui.add(egui::ProgressBar::new(frac).desired_width(140.0).desired_height(8.0).fill(pal().accent));
                        ui.label(small(format!(
                            "{} / {}",
                            util::human_size(job.bytes_done),
                            util::human_size(job.bytes_total)
                        )));
                    } else {
                        ui.add(egui::Spinner::new().size(11.0).color(pal().dim));
                    }
                    let extra = if queued > 1 { format!("  (+{} queued)", queued - 1) } else { String::new() };
                    ui.label(RichText::new(format!("{}{extra}", job.title)).size(12.0).color(pal().text));
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
                        ui.label(RichText::new(k).size(12.0).color(pal().faint));
                        ui.add(egui::Label::new(RichText::new(v).size(12.0).color(pal().dim)).wrap());
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
                    .fill(pal().bg)
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(Margin::same(8))
                    .show(ui, |ui| {
                        ScrollArea::both().auto_shrink([false; 2]).id_salt("preview_text").show(ui, |ui| {
                            ui.add(egui::Label::new(job.clone()).wrap_mode(egui::TextWrapMode::Extend));
                            if *truncated {
                                ui.label(RichText::new("… preview truncated").color(pal().faint).italics());
                            }
                        });
                    });
            }
            State::Ready(Content::Dir { items }) => {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(80.0, 64.0), Sense::hover());
                    draw_path_folder(ui.painter(), r.center(), 4.0, &e.path);
                    ui.add_space(10.0);
                    if let Some(n) = items {
                        ui.label(RichText::new(format!("{n} items")).color(pal().dim));
                    }
                });
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
                ui.vertical_centered(|ui| ui.label(RichText::new(err).color(pal().faint)));
            }
        });
    }

    // ------------------------------------------------------------ overlays

    fn external_drops(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        // Drops via winit (X11); on Wayland they come through `wl_dnd` instead.
        let (dropped, winit_hover, pointer) = ctx.input(|i| {
            let dropped: Vec<PathBuf> = i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect();
            (dropped, !i.raw.hovered_files.is_empty(), i.pointer.hover_pos())
        });
        let pos = self.ext_hover.or(if winit_hover { pointer } else { None });
        if let Some(pos) = pos {
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("drop_overlay")));
            if let Some((r, _)) = self.folder_rects.iter().find(|(r, _)| r.contains(pos)) {
                painter.rect_filled(*r, CornerRadius::same(5), pal().accent.gamma_multiply(0.18));
                painter.rect_stroke(*r, CornerRadius::same(5), Stroke::new(1.5, pal().accent), egui::StrokeKind::Inside);
            } else if let Some(r) = self.pane_rects.iter().find(|r| r.contains(pos)) {
                painter.rect_filled(r.shrink(6.0), CornerRadius::same(10), pal().accent.gamma_multiply(0.10));
                painter.rect_stroke(r.shrink(6.0), CornerRadius::same(10), Stroke::new(2.0, pal().accent), egui::StrokeKind::Inside);
                painter.text(r.center(), Align2::CENTER_CENTER, "Drop to copy here", FontId::proportional(16.0), pal().text);
            }
        }
        if !dropped.is_empty() {
            if let Some((dest, pane)) = self.drop_destination(pointer.unwrap_or_default()) {
                actions.push(Action::FocusPane(pane));
                actions.push(Action::Drop { paths: dropped, dest, copy: true });
            }
        }
    }

    /// Folder under `pos` (row, tile or sidebar entry), else the folder of the pane under it.
    pub(super) fn drop_destination(&self, pos: Pos2) -> Option<(PathBuf, usize)> {
        let pane = self.pane_rects.iter().position(|r| r.contains(pos)).unwrap_or(self.active_pane);
        if let Some((_, p)) = self.folder_rects.iter().find(|(r, _)| r.contains(pos)) {
            return Some((p.clone(), pane));
        }
        let tab = self.tab_in(pane);
        tab.search.is_none().then(|| (tab.path.clone(), pane))
    }

    /// Hands the in-app drag to the compositor once it leaves the window.
    fn maybe_drag_out(&mut self, ctx: &egui::Context) {
        let (Some(wl), Some(payload)) = (&self.wl, &self.dragging) else { return };
        if wl.dragging_out() {
            return;
        }
        let Some(pos) = ctx.pointer_latest_pos() else { return };
        if ctx.content_rect().shrink(2.0).contains(pos) {
            return;
        }
        if std::env::var_os("KESTREL_DEBUG").is_some() {
            eprintln!("[dnd] pointer left window at {pos:?} while dragging");
        }
        if wl.start_drag(&payload.0) {
            DragAndDrop::clear_payload(ctx);
            self.dragging = None;
            // The compositor now owns the pointer; egui never sees the release.
            self.inject_release = true;
            ctx.request_repaint();
        }
    }

    /// The card that follows the cursor while dragging: what you hold and where it will go.
    fn drag_chip(&mut self, ctx: &egui::Context) {
        let Some(payload) = DragAndDrop::payload::<DragPaths>(ctx) else { return };
        let Some(first) = payload.0.first() else { return };
        // Nothing is "held" until the pointer actually moves away from the press.
        if !ctx.input(|i| i.pointer.is_decidedly_dragging()) {
            return;
        }
        let Some(pos) = ctx.pointer_latest_pos() else { return };
        ctx.set_cursor_icon(egui::CursorIcon::Grabbing);

        let n = payload.0.len();
        let target = ctx.data(|d| d.get_temp::<PathBuf>(drop_target_id()));
        let force_copy = ctx.input(|i| i.modifiers.ctrl);
        let bookmarking = ctx.data(|d| d.get_temp::<bool>(bookmark_hover_id())).unwrap_or(false);
        let hint = target.as_ref().map(|t| {
            let copy = force_copy || !same_filesystem(first, t);
            (copy, ops_name(t))
        });
        let is_dir = first.is_dir();
        let ext = first.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        let cat = util::category(&ext, is_dir);
        let renderer = if is_dir { None } else { thumbs::renderer_for(&ext) };
        let thumb = renderer.and_then(|r| self.thumbs.get(first, r)).map(|t| (t.id(), t.size_vec2()));
        let name = if n == 1 { ops_name(first) } else { format!("{} and {} more", ops_name(first), n - 1) };

        let origin = pos + vec2(16.0, 12.0);
        let stack_size = (n > 1).then(|| {
            ctx.data(|d| d.get_temp::<egui::Vec2>(egui::Id::new("drag_card_size"))).unwrap_or(vec2(220.0, 64.0))
        });
        let shown = egui::Area::new(egui::Id::new("drag_card"))
            .order(egui::Order::Tooltip)
            .fixed_pos(origin)
            .interactable(false)
            .show(ctx, |ui| {
                let card = Frame::new()
                    .fill(pal().surface.gamma_multiply(0.96))
                    .stroke(Stroke::new(1.0, if hint.is_some() || bookmarking { pal().accent } else { pal().border }))
                    .corner_radius(CornerRadius::same(8))
                    .shadow(egui::Shadow { offset: [0, 4], blur: 14, spread: 0, color: Color32::from_black_alpha(110) })
                    .inner_margin(Margin::symmetric(10, 8));
                // Cards peeking out behind signal a stack of several items (sized from last frame).
                if let Some(size) = stack_size {
                    for k in [2.0, 1.0] {
                        let r = Rect::from_min_size(origin + vec2(5.0, 5.0) * k, size);
                        ui.painter().rect_filled(r, CornerRadius::same(8), pal().panel);
                        ui.painter().rect_stroke(r, CornerRadius::same(8), Stroke::new(1.0, pal().border), egui::StrokeKind::Inside);
                    }
                }
                card.show(ui, |ui| {
                    ui.set_min_width(180.0);
                    ui.set_max_width(280.0);
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(48.0, 48.0), Sense::hover());
                        match thumb {
                            Some((id, size)) => {
                                let scale = (r.width() / size.x).min(r.height() / size.y);
                                ui.painter().image(id, Rect::from_center_size(r.center(), size * scale), UV, Color32::WHITE);
                            }
                            None => draw_entry_icon(ui.painter(), r.center(), 2.6, first, cat),
                        }
                        if n > 1 {
                            let c = r.right_top() + vec2(-2.0, 2.0);
                            ui.painter().circle_filled(c, 9.0, pal().accent);
                            ui.painter().text(c, Align2::CENTER_CENTER, n.to_string(), FontId::proportional(10.5), Color32::WHITE);
                        }
                        ui.add_space(4.0);
                        ui.vertical(|ui| {
                            ui.add_space(5.0);
                            ui.add(egui::Label::new(RichText::new(name).color(pal().text)).truncate());
                            let (text, color) = match &hint {
                                _ if bookmarking => ("Add to bookmarks".to_string(), pal().accent),
                                Some((true, t)) => (format!("Copy to {t}"), pal().accent),
                                Some((false, t)) => (format!("Move to {t}"), pal().accent),
                                None => ("Drop on a folder".to_string(), pal().faint),
                            };
                            ui.add(egui::Label::new(RichText::new(text).size(12.0).color(color)).truncate());
                        });
                    });
                });
            });
        ctx.data_mut(|d| d.insert_temp(egui::Id::new("drag_card_size"), shown.response.rect.size()));
        ctx.request_repaint();
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
            .frame(Frame::new().fill(pal().panel).stroke(Stroke::new(1.0, pal().border)).corner_radius(CornerRadius::same(10)).inner_margin(Margin::same(8)))
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
                        ui.painter().rect_filled(rect, CornerRadius::same(5), if selected { pal().select } else { pal().hover });
                    }
                    ui.painter().text(rect.left_center() + vec2(10.0, 0.0), Align2::LEFT_CENTER, name, FontId::proportional(BODY), pal().text);
                    ui.painter().with_clip_rect(rect).text(rect.right_center() - vec2(10.0, 0.0), Align2::RIGHT_CENTER, keys, FontId::proportional(12.0), pal().faint);
                    if r.clicked() {
                        clicked = Some(row);
                    }
                }
                if n == 0 {
                    ui.label(RichText::new("No matches").color(pal().faint));
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
            .fill(pal().panel)
            .stroke(Stroke::new(1.0, pal().border))
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
                    ui.label(RichText::new(format!("{what} will be deleted. This can't be undone.")).color(pal().dim));
                    ui.add_space(6.0);
                    for p in paths.iter().take(6) {
                        ui.label(RichText::new(format!("•  {}", ops_name(p))).color(pal().text));
                    }
                    if n > 6 {
                        ui.label(RichText::new(format!("…and {} more", n - 6)).color(pal().faint));
                    }
                    dialog_buttons(ui, "Delete", true, &mut confirm, &mut cancel);
                    // Esc closes the modal on its own; Backspace cancels too.
                    ui.input_mut(|i| {
                        confirm |= super::take(i, Modifiers::NONE, Key::Enter);
                        cancel |= super::take(i, Modifiers::NONE, Key::Backspace);
                    });
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
                    self.ops.submit(crate::ops::Job::new(crate::ops::JobKind::Delete, paths, None))
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
                        .fill(pal().surface)
                        .stroke(Stroke::new(1.0, if t.error { pal().danger } else { pal().border }))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::symmetric(12, 8))
                        .show(ui, |ui| {
                            ui.set_max_width(420.0);
                            ui.label(RichText::new(&t.text).color(if t.error { pal().danger } else { pal().text }));
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
    ui.vertical_centered(|ui| ui.label(RichText::new(msg).color(pal().dim)));
}

/// Scrolls the list while a drag hovers near its top or bottom edge.
fn drag_autoscroll(ui: &Ui, tab: &mut Tab, viewport: Rect) {
    if !DragAndDrop::has_payload_of_type::<DragPaths>(ui.ctx()) {
        return;
    }
    let Some(pos) = ui.ctx().pointer_latest_pos() else { return };
    if !viewport.x_range().contains(pos.x) {
        return;
    }
    const EDGE: f32 = 36.0;
    let speed = if pos.y < viewport.top() + EDGE && pos.y > viewport.top() - EDGE {
        -(viewport.top() + EDGE - pos.y)
    } else if pos.y > viewport.bottom() - EDGE && pos.y < viewport.bottom() + EDGE {
        pos.y - (viewport.bottom() - EDGE)
    } else {
        return;
    };
    tab.force_offset = Some((tab.scroll_offset + speed * 0.4).max(0.0));
    ui.ctx().request_repaint();
}

/// Drop zone that bookmarks dragged folders; returns whether a drag hovers it.
fn bookmark_zone(ui: &Ui, resp: &egui::Response, dirs: &[PathBuf], actions: &mut Vec<Action>) -> bool {
    let hot = resp.dnd_hover_payload::<DragPaths>().is_some();
    if hot {
        ui.ctx().data_mut(|d| d.insert_temp(bookmark_hover_id(), true));
    }
    if resp.dnd_release_payload::<DragPaths>().is_some() {
        actions.push(Action::Bookmark(dirs.to_vec()));
    }
    hot
}

fn bookmark_hover_id() -> egui::Id {
    egui::Id::new("kestrel_bookmark_hover")
}

fn chevron(p: &Painter, c: Pos2, open: bool, color: Color32) {
    let pts = if open {
        vec![c + vec2(-3.5, -1.5), c + vec2(0.0, 2.0), c + vec2(3.5, -1.5)]
    } else {
        vec![c + vec2(-1.5, -3.5), c + vec2(2.0, 0.0), c + vec2(-1.5, 3.5)]
    };
    p.line(pts, Stroke::new(1.4, color));
}

fn same_filesystem(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::symlink_metadata(a), std::fs::metadata(b)) {
        (Ok(x), Ok(y)) => x.dev() == y.dev(),
        _ => false,
    }
}

fn folder_rects_id() -> egui::Id {
    egui::Id::new("kestrel_folder_rects")
}

/// Remembers where folders are drawn so drops from other apps can target them.
fn record_folder(ctx: &egui::Context, rect: Rect, path: &Path) {
    ctx.data_mut(|d| d.get_temp_mut_or_default::<Vec<(Rect, PathBuf)>>(folder_rects_id()).push((rect, path.to_path_buf())));
}

fn drop_target_id() -> egui::Id {
    egui::Id::new("kestrel_drop_target")
}

/// True while a drag hovers `target`; also records it so the drag card can say where it goes.
fn drop_hover(resp: &egui::Response, target: &Path) -> bool {
    let hovering = resp.dnd_hover_payload::<DragPaths>().is_some_and(|p| !p.0.iter().any(|s| s == target));
    if hovering {
        resp.ctx.data_mut(|d| d.insert_temp(drop_target_id(), target.to_path_buf()));
    }
    hovering
}

/// Dropping onto empty space in a pane moves/copies into that pane's folder.
fn background_drop(ui: &mut Ui, p: usize, tab: &Tab, rect: Rect, actions: &mut Vec<Action>) {
    if tab.search.is_some() {
        return;
    }
    let resp = ui.interact(rect, ui.id().with(("pane_drop", p)), Sense::hover());
    // Rows are checked first; only claim the hint if no folder row did.
    let claimed = ui.ctx().data(|d| d.get_temp::<PathBuf>(drop_target_id()).is_some());
    if !claimed {
        let into_other_folder = resp
            .dnd_hover_payload::<DragPaths>()
            .is_some_and(|d| d.0.iter().any(|s| s.parent() != Some(tab.path.as_path())));
        if into_other_folder {
            ui.ctx().data_mut(|d| d.insert_temp(drop_target_id(), tab.path.clone()));
        }
    }
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
    bookmarks: &[(String, PathBuf)],
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
        let mut open_with = Vec::new();
        if !e.is_dir {
            open_with_menu(ui, tab, vi, e, p, &mut open_with);
        }
        if e.is_dir {
            item(ui, "Open in new tab", "", Action::NewTab(e.path.clone()));
            if bookmarks.iter().any(|(_, b)| *b == e.path) {
                item(ui, "Remove from bookmarks", "", Action::Unbookmark(e.path.clone()));
            } else {
                let dirs: Vec<PathBuf> = if tab.is_selected(vi) { tab.targets() } else { vec![e.path.clone()] };
                let dirs: Vec<PathBuf> = dirs.into_iter().filter(|p| p.is_dir()).collect();
                item(ui, "Add to bookmarks", "", Action::Bookmark(dirs));
            }
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
        let targets = if tab.is_selected(vi) { tab.targets() } else { vec![e.path.clone()] };
        if targets.iter().any(|t| crate::ops::is_archive(t)) {
            item(ui, "Extract here", "", Action::Cmd(Cmd::Extract));
        }
        item(ui, "Compress to .zip", "", Action::Cmd(Cmd::Compress));
        ui.separator();
        item(ui, "Copy path", "Alt+C", Action::CopyPath(e.path.clone()));
        let term = if e.is_dir { e.path.clone() } else { tab.path.clone() };
        item(ui, "Open terminal here", "", Action::OpenTerminal(term));
        ui.separator();
        item(ui, "Move to trash", "Delete", Action::Cmd(Cmd::Trash));
        item(ui, "Delete permanently", "Shift+Del", Action::Cmd(Cmd::DeletePermanently));
        actions.extend(open_with);
    });
}

/// "Open with" submenu: apps for this file type, default first, and a way to change the default.
fn open_with_menu(ui: &mut Ui, tab: &Tab, vi: usize, e: &Entry, p: usize, actions: &mut Vec<Action>) {
    let choices = crate::apps::choices(&e.path);
    if choices.apps.is_empty() {
        return;
    }
    let paths: Vec<PathBuf> = if tab.is_selected(vi) { tab.targets().into_iter().filter(|t| !t.is_dir()).collect() } else { vec![e.path.clone()] };
    let what = match e.path.extension() {
        Some(ext) => format!(".{} files", ext.to_string_lossy()),
        None => "files like this".into(),
    };
    const SHOWN: usize = 12;
    ui.menu_button("Open with", |ui| {
        ui.set_min_width(200.0);
        for app in choices.apps.iter().take(SHOWN) {
            let default = choices.default.as_deref() == Some(app.id.as_str());
            if ui.add(egui::Button::new(&app.name).shortcut_text(if default { "default" } else { "" })).clicked() {
                actions.push(Action::FocusPane(p));
                actions.push(Action::OpenWith(app.clone(), paths.clone()));
                ui.close();
            }
        }
        ui.separator();
        ui.menu_button(format!("Always open {what} with"), |ui| {
            ui.set_min_width(200.0);
            for app in choices.apps.iter().take(SHOWN) {
                let default = choices.default.as_deref() == Some(app.id.as_str());
                if ui.add_enabled(!default, egui::Button::new(&app.name)).clicked() {
                    actions.push(Action::SetDefaultApp(app.clone(), choices.mime.clone(), what.clone()));
                    ui.close();
                }
            }
        });
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
    draw_entry_icon(painter, Pos2::new(cols.name.0 - 12.0, cy), 1.0, &e.path, cat);
    let name_color = if e.hidden { pal().dim } else { pal().text };
    let name_clip = painter.with_clip_rect(Rect::from_x_y_ranges(cols.name.0..=cols.name.1, rect.y_range()));
    let name_rect = name_clip.text(
        Pos2::new(cols.name.0, cy),
        Align2::LEFT_CENTER,
        &e.name,
        FontId::proportional(BODY),
        name_color.gamma_multiply(alpha),
    );
    if e.is_link {
        painter.text(Pos2::new(cols.name.0 - 6.0, cy + 6.0), Align2::CENTER_CENTER, "↗", FontId::proportional(9.0), pal().accent);
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
        painter.text(Pos2::new(size.1, cy), Align2::RIGHT_CENTER, size_text, small.clone(), pal().dim.gamma_multiply(alpha));
    }
    if let (Some(m), Some(date)) = (e.modified, cols.date) {
        painter.text(Pos2::new(date.0, cy), Align2::LEFT_CENTER, util::fmt_time(m), small.clone(), pal().dim.gamma_multiply(alpha));
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
            .text(Pos2::new(k.0, cy), Align2::LEFT_CENTER, text, small, pal().faint);
    } else if let Some(rel) = e.rel.as_ref().filter(|r| !r.is_empty()) {
        name_clip.text(name_rect.right_center() + vec2(8.0, 0.0), Align2::LEFT_CENTER, rel, small, pal().faint);
    }
}

/// A List-mode cell: icon and name on one line.
fn paint_cell(painter: &Painter, cell: Rect, e: &Entry, faded: bool) {
    let alpha = if faded { 0.45 } else { 1.0 };
    let cy = cell.center().y;
    draw_entry_icon(painter, Pos2::new(cell.left() + 14.0, cy), 1.0, &e.path, util::category(&e.ext, e.is_dir));
    let color = if e.hidden { pal().dim } else { pal().text };
    painter.with_clip_rect(cell.shrink2(vec2(6.0, 0.0)).intersect(painter.clip_rect())).text(
        Pos2::new(cell.left() + 30.0, cy),
        Align2::LEFT_CENTER,
        &e.name,
        FontId::proportional(BODY),
        color.gamma_multiply(alpha),
    );
}

fn paint_tile(painter: &Painter, thumbs: &mut Thumbs, tile: Rect, e: &Entry, faded: bool) {
    let alpha = if faded { 0.45 } else { 1.0 };
    let side = tile.width().min(tile.height() - 48.0) - 16.0;
    let thumb_area = Rect::from_center_size(
        Pos2::new(tile.center().x, tile.top() + 8.0 + side / 2.0),
        vec2(tile.width() - 16.0, side),
    );
    let renderer = if e.is_dir { None } else { thumbs::renderer_for(&e.ext) };
    let mut drew = false;
    if let Some(r) = renderer {
        if let Some(tex) = thumbs.get(&e.path, r) {
            let ts = tex.size_vec2();
            let scale = (thumb_area.width() / ts.x).min(thumb_area.height() / ts.y).min(1.0);
            let rect = Rect::from_center_size(thumb_area.center(), ts * scale);
            painter.image(tex.id(), rect, UV, Color32::WHITE.gamma_multiply(alpha));
            drew = true;
        }
    }
    if !drew {
        let cat = util::category(&e.ext, e.is_dir);
        draw_entry_icon(painter, thumb_area.center(), side / 24.0, &e.path, cat);
    }
    let color = if e.hidden { pal().dim } else { pal().text }.gamma_multiply(alpha);
    let mut job = egui::text::LayoutJob::simple(e.name.clone(), FontId::proportional(12.5), color, tile.width() - 12.0);
    job.wrap.max_rows = 2;
    job.wrap.break_anywhere = true;
    job.halign = Align::Center;
    let galley = painter.layout_job(job);
    painter.galley(Pos2::new(tile.center().x, thumb_area.bottom() + 8.0), galley, color);
}

// ---------------------------------------------------------------- dialogs

fn name_field(ui: &mut Ui, title: &str, name: &mut String, focus: &mut bool, select: usize, confirm: &mut bool) {
    ui.label(RichText::new(title).size(15.0).strong());
    ui.add_space(8.0);
    let id = egui::Id::new("dialog_name");
    // Focus and selection go in before the TextEdit is laid out, so a paste in the same
    // frame replaces the selected name. A dialog opened from the palette is first drawn on
    // the frame that carries the Ctrl+V; an unfocused TextEdit drops it.
    let select_now = std::mem::take(focus);
    if select_now {
        let mut state = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
        let range = egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(select));
        state.cursor.set_char_range(Some(range));
        state.store(ui.ctx(), id);
    }
    let resp = ui.add(
        egui::TextEdit::singleline(name)
            .id(id)
            .desired_width(f32::INFINITY)
            .margin(Margin::symmetric(8, 5)),
    );
    if select_now {
        resp.request_focus();
    }
    if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
        *confirm = true;
    }
}

fn dialog_buttons(ui: &mut Ui, ok: &str, danger: bool, confirm: &mut bool, cancel: &mut bool) {
    ui.add_space(14.0);
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        // Key hints, so it's clear Enter confirms and Esc cancels.
        let hint = |key: &str, color: Color32| RichText::new(key).size(11.0).color(color);
        let ok_button = egui::Button::new(RichText::new(ok).color(Color32::WHITE))
            .shortcut_text(hint("Enter", Color32::from_white_alpha(170)))
            .fill(if danger { pal().danger } else { pal().accent_strong })
            .min_size(vec2(84.0, 28.0));
        if ui.add(ok_button).clicked() {
            *confirm = true;
        }
        let cancel_button = egui::Button::new("Cancel")
            .shortcut_text(hint("Esc", pal().faint))
            .fill(pal().surface)
            .min_size(vec2(84.0, 28.0));
        if ui.add(cancel_button).clicked() {
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
    Sidebar,
    Split,
    Grid,
    Search,
}

fn glyph_button(ui: &mut Ui, glyph: Glyph, enabled: bool, on: bool, tip: &str) -> bool {
    let resp = ui
        .add_enabled(enabled, egui::Button::new("").frame_when_inactive(false).min_size(vec2(28.0, 26.0)))
        .on_hover_text(tip);
    let color = if !enabled {
        pal().faint
    } else if on {
        pal().accent
    } else if resp.hovered() {
        pal().text
    } else {
        pal().dim
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
        Glyph::Sidebar => {
            let r = Rect::from_center_size(c, vec2(15.0, 12.0));
            p.rect_stroke(r, CornerRadius::same(2), stroke, egui::StrokeKind::Middle);
            let side = Rect::from_min_max(r.min, Pos2::new(r.left() + 5.5, r.bottom()));
            p.rect_filled(side, CornerRadius { nw: 2, sw: 2, ne: 0, se: 0 }, color);
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
    p.line(vec![c + vec2(-3.5, -a), c + vec2(0.0, -b), c + vec2(3.5, -a)], Stroke::new(1.4, pal().accent));
}

fn draw_checker(p: &Painter, r: Rect) {
    let cell = 8.0;
    p.rect_filled(r, CornerRadius::ZERO, pal().checker.0);
    let (nx, ny) = ((r.width() / cell).ceil() as usize, (r.height() / cell).ceil() as usize);
    if nx * ny > 40_000 {
        return;
    }
    let painter = p.with_clip_rect(r);
    for y in 0..ny {
        for x in (y % 2..nx).step_by(2) {
            let min = r.min + vec2(x as f32, y as f32) * cell;
            painter.rect_filled(Rect::from_min_size(min, vec2(cell, cell)), CornerRadius::ZERO, pal().checker.1);
        }
    }
}


