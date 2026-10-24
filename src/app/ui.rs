use super::icons::*;
use super::{Action, App, DragPaths, TabDrag};
use crate::complete;
use crate::fsops::{self, Entry};
use crate::palette::Cmd;
use crate::tab::{Tab, ViewMode};
use crate::theme::*;
use crate::thumbs::{self, Thumbs};
use crate::util;
use eframe::egui::{
    self, Align, Align2, CentralPanel, Color32, CornerRadius, DragAndDrop, FontId, Frame, Key,
    Layout, Margin, Modifiers, Painter, Panel, Pos2, Rect, RichText, ScrollArea, Sense, Stroke, Ui,
    UiBuilder, vec2,
};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

const ROW_H: f32 = 26.0;

mod drag;
mod glyphs;
mod inspector;
mod overlays;
mod preview;
mod sidebar;
mod views;

use glyphs::*;
pub(super) use preview::{paint_preview_links, preview_code_cached, preview_table};
#[cfg(test)]
pub(super) use preview::{preview_code, preview_text};

impl App {
    pub(super) fn draw(&mut self, ui: &mut Ui) {
        self.sync_columns();
        let mut actions = Vec::new();
        ui.ctx().data_mut(|d| {
            d.remove_temp::<PathBuf>(drop_target_id());
            d.remove_temp::<bool>(bookmark_hover_id());
            let rects = d
                .get_temp::<Vec<(Rect, PathBuf)>>(folder_rects_id())
                .unwrap_or_default();
            self.folder_rects = rects;
            d.remove_temp::<Vec<(Rect, PathBuf)>>(folder_rects_id());
        });
        self.dragging = DragAndDrop::payload::<DragPaths>(ui.ctx());
        ui.ctx()
            .data_mut(|d| d.insert_temp(egui::Id::new("is_split"), self.panes.len() > 1));

        Panel::bottom("status")
            .exact_size(26.0)
            .frame(
                Frame::new()
                    .fill(pal().panel)
                    .inner_margin(Margin::symmetric(12, 2)),
            )
            .show(ui, |ui| self.status_bar(ui));
        if self.picker.is_some() {
            Panel::bottom("picker")
                .exact_size(44.0)
                .frame(
                    Frame::new()
                        .fill(pal().panel)
                        .inner_margin(Margin::symmetric(12, 6)),
                )
                .show(ui, |ui| self.picker_bar(ui, &mut actions));
        }
        // Side panels yield to the file list in narrow windows.
        let width = ui.available_width();
        // Both can be toggled, or dragged closed past their minimum width and back open.
        let mut show = self.show_sidebar;
        Panel::left("sidebar")
            .default_size(self.sidebar_width)
            .min_size(120.0_f32.min(width * 0.25))
            .max_size((width * 0.25).max(120.0))
            .drag_to_open(true)
            .frame(
                Frame::new()
                    .fill(pal().panel)
                    .inner_margin(Margin::symmetric(8, 4)),
            )
            .show_collapsible(ui, &mut show, |ui| self.sidebar(ui, &mut actions));
        self.show_sidebar = show;
        let mut show = self.show_preview;
        Panel::right("preview")
            .default_size(self.preview_width)
            .min_size(160.0_f32.min(width * 0.4))
            .max_size((width * 0.4).max(160.0))
            .drag_to_open(true)
            .frame(
                Frame::new()
                    .fill(pal().panel)
                    .inner_margin(Margin::symmetric(12, 4)),
            )
            .show_collapsible(ui, &mut show, |ui| self.preview_panel(ui));
        self.show_preview = show;
        // Remember widths only while shown; a folded panel reports its animation size.
        let width_of = |id: &str| {
            egui::containers::panel::PanelState::load(ui.ctx(), egui::Id::new(id))
                .map(|s| s.size().x)
        };
        if self.show_sidebar
            && let Some(w) = width_of("sidebar")
        {
            self.sidebar_width = w;
        }
        if self.show_preview
            && let Some(w) = width_of("preview")
        {
            self.preview_width = w;
        }
        CentralPanel::default()
            .frame(Frame::new().fill(pal().bg))
            .show(ui, |ui| {
                let full = ui.max_rect();
                self.pane_rects.clear();
                if self.panes.len() == 1 {
                    self.pane_rects.push(full);
                    self.pane(ui, 0, &mut actions);
                } else {
                    let mid = full.left() + full.width() * self.split_ratio;
                    let handle = Rect::from_center_size(
                        Pos2::new(mid, full.center().y),
                        vec2(8.0, full.height()),
                    );
                    let response = ui.interact(
                        handle,
                        ui.id().with("split_divider"),
                        Sense::click_and_drag(),
                    );
                    if response.dragged_by(egui::PointerButton::Primary)
                        && let Some(pos) = response.interact_pointer_pos()
                    {
                        self.split_ratio = ((pos.x - full.left()) / full.width()).clamp(0.15, 0.85);
                    }
                    if response.double_clicked() {
                        self.split_ratio = 0.5;
                    }
                    response
                        .on_hover_cursor(egui::CursorIcon::ResizeHorizontal)
                        .on_hover_text("Drag to resize panes · double-click for equal widths");
                    let mid = full.left() + full.width() * self.split_ratio;
                    let rects = [
                        Rect::from_min_max(full.min, Pos2::new(mid - 4.0, full.max.y)),
                        Rect::from_min_max(Pos2::new(mid + 4.0, full.min.y), full.max),
                    ];
                    ui.painter()
                        .vline(mid, full.y_range(), Stroke::new(2.0, pal().border));
                    for (p, r) in rects.into_iter().enumerate() {
                        self.pane_rects.push(r);
                        ui.scope_builder(UiBuilder::new().max_rect(r).id_salt(("pane", p)), |ui| {
                            ui.set_clip_rect(r);
                            self.pane(ui, p, &mut actions);
                        });
                    }
                    // Mark which pane has focus.
                    let active = rects[self.active_pane.min(1)];
                    let accent = pal().accent;
                    ui.painter().rect_stroke(
                        active.shrink(1.0),
                        0.0,
                        Stroke::new(2.0, accent.gamma_multiply(0.7)),
                        egui::StrokeKind::Inside,
                    );
                }
            });

        self.external_drops(ui.ctx(), &mut actions);
        self.maybe_drag_out(ui.ctx());
        self.drag_chip(ui.ctx());
        self.palette_ui(ui.ctx(), &mut actions);
        self.dialog_ui(ui.ctx());
        self.toasts_ui(ui.ctx());

        // An action can open a dialog or change the view; draw that without waiting for input.
        if !actions.is_empty() {
            ui.ctx().request_repaint();
        }
        for a in actions {
            self.apply(a);
        }

        if self.show_preview {
            self.sync_inspector();
        }
        let title = format!("{} — Kestrel", self.tab().path.display());
        if title != self.window_title {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }
    }

    /// The editable path field with a dropdown of matching folders.
    fn path_editor(&mut self, ui: &mut Ui, p: usize, actions: &mut Vec<Action>) {
        let id = egui::Id::new(("path_edit", p));
        let Some((_, text)) = self.path_edit.as_mut() else {
            return;
        };
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
                st.cursor
                    .set_char_range(Some(egui::text::CCursorRange::one(end)));
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
            let x = field
                .left()
                .min(screen.right() - width - 8.0)
                .max(screen.left() + 8.0);
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
                                let (rect, item) = ui.allocate_exact_size(
                                    vec2(ui.available_width(), 26.0),
                                    Sense::click(),
                                );
                                let lit = self.path_hl == Some(n);
                                if lit || item.hovered() {
                                    ui.painter().rect_filled(
                                        rect,
                                        CornerRadius::same(4),
                                        if lit { pal().select } else { pal().hover },
                                    );
                                }
                                draw_folder(
                                    ui.painter(),
                                    Pos2::new(rect.left() + 14.0, rect.center().y),
                                    0.8,
                                );
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
                                ui.label(
                                    RichText::new(format!("  {} more…", cands.len() - shown))
                                        .size(12.0)
                                        .color(pal().faint),
                                );
                            }
                        });
                });
        }

        let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        let target = picked.or_else(|| {
            enter.then(|| {
                self.path_hl
                    .map_or_else(|| text.clone(), |h| cands[h].clone())
            })
        });
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
                && i.pointer
                    .interact_pos()
                    .is_some_and(|pos| rect.contains(pos))
        });
        if pressed_here && p != self.active_pane {
            actions.push(Action::FocusPane(p));
        }
        Panel::top(egui::Id::new(("tabs", p)))
            .exact_size(36.0)
            .frame(Frame::new().fill(pal().bg).inner_margin(Margin {
                left: 8,
                right: 8,
                top: 4,
                bottom: 0,
            }))
            .show_separator_line(false)
            .show(ui, |ui| self.tab_bar(ui, p, actions));
        Panel::top(egui::Id::new(("toolbar", p)))
            .exact_size(38.0)
            .frame(
                Frame::new()
                    .fill(pal().panel)
                    .inner_margin(Margin::symmetric(8, 4)),
            )
            .show(ui, |ui| self.toolbar(ui, p, actions));
        CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(pal().bg)
                    .inner_margin(Margin::symmetric(4, 0)),
            )
            .show(ui, |ui| {
                self.zoom_wheel(ui, p, actions);
                let pane = &self.panes[p];
                let tab = &pane.tabs[pane.active];
                if tab.mode == ViewMode::Columns && tab.search.is_none() {
                    self.miller_columns(ui, p, actions);
                } else if tab.mode.is_grid() {
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
                    egui::Event::MouseWheel {
                        unit,
                        delta,
                        modifiers,
                        ..
                    } if modifiers.ctrl || modifiers.command => match unit {
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
        let background = ui.interact(
            ui.max_rect(),
            ui.id().with(("tab_strip", p)),
            Sense::hover(),
        );
        let active_id = self.panes[p].tabs[self.panes[p].active].id;
        let reveal = ui.data_mut(|d| {
            let id = egui::Id::new(("visible_tab", p));
            let changed = d.get_temp::<u64>(id) != Some(active_id);
            d.insert_temp(id, active_id);
            changed
        });
        let natural: Vec<f32> = self.panes[p]
            .tabs
            .iter()
            .map(|tab| {
                (ui.painter()
                    .layout_no_wrap(tab.title(), FontId::proportional(13.0), pal().text)
                    .size()
                    .x
                    + 60.0)
                    .max(100.0)
            })
            .collect();
        // Reserve the new-tab button and spacing, then shrink only the tabs that
        // need it. Once every tab reaches 100px the strip scrolls instead.
        let budget = (ui.available_width() - 28.0 - natural.len() as f32 * 2.0).max(0.0);
        let widths = tab_widths(&natural, budget);
        ui.scope(|ui| {
            ui.style_mut().always_scroll_the_only_direction = true;
            ScrollArea::horizontal()
                .id_salt(("tab_scroll", p))
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.horizontal_centered(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        let pane = &self.panes[p];
                        let top_round = CornerRadius {
                            nw: 8,
                            ne: 8,
                            sw: 0,
                            se: 0,
                        };
                        for (i, tab) in pane.tabs.iter().enumerate() {
                            let active = i == pane.active;
                            let color = if active { pal().text } else { pal().dim };
                            let w = widths[i];
                            let mut job = egui::text::LayoutJob::simple(
                                tab.title(),
                                FontId::proportional(13.0),
                                color,
                                (w - 60.0).max(1.0),
                            );
                            job.wrap.max_rows = 1;
                            job.wrap.break_anywhere = true;
                            let galley = ui.painter().layout_job(job);
                            let (rect, resp) = ui.allocate_exact_size(
                                vec2(w, ui.available_height()),
                                Sense::click_and_drag(),
                            );
                            if active && reveal {
                                resp.scroll_to_me(Some(Align::Center));
                            }
                            if resp.drag_started_by(egui::PointerButton::Primary) {
                                DragAndDrop::set_payload(ui.ctx(), TabDrag { id: tab.id });
                            }
                            let close_rect = Rect::from_center_size(
                                Pos2::new(rect.right() - 15.0, rect.center().y),
                                vec2(18.0, 18.0),
                            );
                            let close = ui.interact(
                                close_rect,
                                ui.id().with(("tab_close", tab.id)),
                                Sense::click(),
                            );
                            resp.clone().on_hover_text(tab.path.display().to_string());
                            #[cfg(test)]
                            self.tab_rects.insert(tab.id, (rect, close_rect));
                            let painter = ui.painter();
                            let drop_hover = resp.dnd_hover_payload::<DragPaths>().is_some();
                            // The active tab flows into the toolbar below it.
                            let body = if active {
                                rect.expand2(vec2(0.0, 0.0)).with_max_y(rect.bottom() + 4.0)
                            } else {
                                rect
                            };
                            if active {
                                painter.rect_filled(body, top_round, pal().panel);
                                let line = Rect::from_min_size(
                                    rect.min + vec2(0.0, 0.0),
                                    vec2(rect.width(), 2.0),
                                );
                                painter.rect_filled(
                                    line,
                                    CornerRadius {
                                        nw: 8,
                                        ne: 8,
                                        sw: 0,
                                        se: 0,
                                    },
                                    if pane_active {
                                        pal().accent
                                    } else {
                                        pal().faint
                                    },
                                );
                            } else if resp.hovered() || drop_hover {
                                painter.rect_filled(
                                    rect.shrink2(vec2(0.0, 3.0)).with_max_y(rect.bottom() - 3.0),
                                    CornerRadius::same(6),
                                    pal().hover,
                                );
                            }
                            if drop_hover {
                                painter.rect_stroke(
                                    body,
                                    top_round,
                                    Stroke::new(1.5, pal().accent),
                                    egui::StrokeKind::Inside,
                                );
                            }
                            draw_folder(
                                painter,
                                Pos2::new(rect.left() + 16.0, rect.center().y + 1.0),
                                0.8,
                            );
                            let text_rect = Rect::from_min_max(
                                Pos2::new(rect.left() + 28.0, rect.top()),
                                Pos2::new(close_rect.left() - 6.0, rect.bottom()),
                            );
                            painter
                                .with_clip_rect(text_rect.intersect(painter.clip_rect()))
                                .galley(
                                    Pos2::new(
                                        text_rect.left(),
                                        rect.center().y - galley.size().y / 2.0 + 1.0,
                                    ),
                                    galley,
                                    color,
                                );
                            if resp.hovered() || close.hovered() || active {
                                if close.hovered() {
                                    painter.circle_filled(close_rect.center(), 9.0, pal().select);
                                }
                                let c = close_rect.center();
                                let arm = 3.0;
                                let stroke = Stroke::new(
                                    1.4,
                                    if close.hovered() {
                                        pal().text
                                    } else {
                                        pal().dim
                                    },
                                );
                                painter.line_segment(
                                    [c + vec2(-arm, -arm), c + vec2(arm, arm)],
                                    stroke,
                                );
                                painter.line_segment(
                                    [c + vec2(-arm, arm), c + vec2(arm, -arm)],
                                    stroke,
                                );
                            }
                            if resp.dnd_hover_payload::<TabDrag>().is_some() {
                                let left = ui
                                    .input(|i| i.pointer.interact_pos())
                                    .is_some_and(|pos| pos.x < rect.center().x);
                                let before = if left {
                                    Some(tab.id)
                                } else {
                                    pane.tabs.get(i + 1).map(|t| t.id)
                                };
                                painter.vline(
                                    if left { rect.left() } else { rect.right() },
                                    rect.y_range(),
                                    Stroke::new(3.0, pal().accent),
                                );
                                if ui.input(|i| i.pointer.primary_released())
                                    && let Some(drag) =
                                        DragAndDrop::take_payload::<TabDrag>(ui.ctx())
                                {
                                    actions.push(Action::MoveTab {
                                        id: drag.id,
                                        pane: p,
                                        before,
                                    });
                                }
                            }
                            if let Some(paths) = take_file_drop(&resp) {
                                let copy = ui.input(|i| i.modifiers.ctrl);
                                actions.push(Action::Drop {
                                    paths: paths.0.clone(),
                                    dest: tab.path.clone(),
                                    copy,
                                });
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
                });
        });
        if background.dnd_hover_payload::<TabDrag>().is_some()
            && ui.input(|i| i.pointer.primary_released())
            && let Some(drag) = DragAndDrop::take_payload::<TabDrag>(ui.ctx())
        {
            actions.push(Action::MoveTab {
                id: drag.id,
                pane: p,
                before: None,
            });
        }
    }

    fn toolbar(&mut self, ui: &mut Ui, p: usize, actions: &mut Vec<Action>) {
        let last_pane = p == self.panes.len() - 1;
        ui.horizontal_centered(|ui| {
            if p == 0 {
                let tip = if self.show_sidebar {
                    "Hide sidebar (Ctrl+B)"
                } else {
                    "Show sidebar (Ctrl+B)"
                };
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
            if glyph_button(
                ui,
                Glyph::Back,
                !tab.back.is_empty(),
                false,
                "Back (Alt+Left)",
            ) {
                nav(Cmd::Back);
            }
            if glyph_button(
                ui,
                Glyph::Forward,
                !tab.fwd.is_empty(),
                false,
                "Forward (Alt+Right)",
            ) {
                nav(Cmd::Forward);
            }
            if glyph_button(
                ui,
                Glyph::Up,
                tab.path.parent().is_some(),
                false,
                "Up (Backspace)",
            ) {
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
                    let tip = if self.show_preview {
                        "Hide preview (F3)"
                    } else {
                        "Show preview (F3)"
                    };
                    if glyph_button(ui, Glyph::Panel, true, self.show_preview, tip) {
                        actions.push(Action::Cmd(Cmd::TogglePreview));
                    }
                    if !compact {
                        if glyph_button(
                            ui,
                            Glyph::Eye,
                            true,
                            self.show_hidden,
                            "Show hidden files (Ctrl+H)",
                        ) {
                            actions.push(Action::Cmd(Cmd::ToggleHidden));
                        }
                        if glyph_button(
                            ui,
                            Glyph::Split,
                            true,
                            self.panes.len() > 1,
                            "Split view (F6)",
                        ) {
                            actions.push(Action::Cmd(Cmd::ToggleSplit));
                        }
                    }
                }
                let pane = &self.panes[p];
                let tab = &pane.tabs[pane.active];
                if !compact {
                    ui.menu_button("View", |ui| {
                        for (mode, label) in [
                            (ViewMode::Details, "Details"),
                            (ViewMode::List, "List"),
                            (ViewMode::Icons(2), "Icons"),
                            (ViewMode::Columns, "Miller columns"),
                        ] {
                            let selected = tab.mode == mode
                                || (mode == ViewMode::Icons(2)
                                    && matches!(tab.mode, ViewMode::Icons(_)));
                            let shortcut = match mode {
                                ViewMode::Columns => "Ctrl+Shift+M",
                                ViewMode::Details if tab.mode.is_grid() => "Ctrl+G",
                                ViewMode::Icons(_) if !tab.mode.is_grid() => "Ctrl+G",
                                _ => "",
                            };
                            if ui
                                .add(
                                    egui::Button::new(label)
                                        .selected(selected)
                                        .shortcut_text(shortcut),
                                )
                                .clicked()
                            {
                                actions.push(Action::FocusPane(p));
                                actions.push(Action::SetView(mode));
                                ui.close();
                            }
                        }
                        ui.separator();
                        ui.label(
                            RichText::new("Ctrl+G · Icons / Details")
                                .small()
                                .color(pal().dim),
                        );
                        ui.label(
                            RichText::new("Ctrl+scroll · Details → List → Icons")
                                .small()
                                .color(pal().dim),
                        );
                        if tab.mode == ViewMode::Columns {
                            ui.label(
                                RichText::new("Zoom is inactive in columns")
                                    .small()
                                    .color(pal().dim),
                            );
                        }
                    })
                    .response
                    .on_hover_text("View mode · columns: Ctrl+Shift+M · icons/details: Ctrl+G");
                }
                if (!compact || tab.search.is_some())
                    && glyph_button(
                        ui,
                        Glyph::Search,
                        true,
                        tab.search.is_some(),
                        "Search subfolders (Ctrl+F)",
                    )
                {
                    actions.push(Action::FocusPane(p));
                    actions.push(if tab.search.is_some() {
                        Action::StopSearch
                    } else {
                        Action::Cmd(Cmd::Search)
                    });
                }
                if !tab.filter.is_empty() {
                    let chip = Frame::new()
                        .fill(pal().select)
                        .corner_radius(CornerRadius::same(5))
                        .inner_margin(Margin::symmetric(8, 3))
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new(format!("Filter: {}  ×", tab.filter))
                                    .color(pal().text),
                            )
                        });
                    if chip
                        .response
                        .interact(Sense::click())
                        .on_hover_text("Clear filter (Esc)")
                        .clicked()
                    {
                        actions.push(Action::FocusPane(p));
                        actions.push(Action::ClearFilter);
                    }
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    self.path_bar(ui, p, actions)
                });
            });
        });
    }

    fn path_bar(&mut self, ui: &mut Ui, p: usize, actions: &mut Vec<Action>) {
        let frame = Frame::new()
            .fill(pal().bg)
            .corner_radius(CornerRadius::same(6))
            .stroke(Stroke::new(
                1.0,
                if self.tab_in(p).search.is_some() {
                    pal().accent
                } else {
                    pal().border
                },
            ))
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
                    let hint = format!("{} {}", if s.flatten { "Filter flattened folder:" } else { "Search in" }, s.root.display());
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
                    resp.on_hover_text("Fuzzy names · ext:pdf,jpg · type:folder / type:file\n\"literal phrase\" · =exact-name.txt · F5 rescans folders");
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
                    if let Some(paths) = take_file_drop(&resp) {
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

    fn status_bar(&mut self, ui: &mut Ui) {
        let job = self.ops.snapshot();
        let queued = self.ops.queued.load(std::sync::atomic::Ordering::Relaxed);
        let tab = self.tab();
        let small = |s: String| RichText::new(s).size(12.0).color(pal().dim);
        ui.horizontal_centered(|ui| {
            if let Some(s) = &tab.search {
                let n = tab.view.len();
                let more = if s.truncated { "+" } else { "" };
                ui.label(small(format!(
                    "{n}{more} {}",
                    if s.flatten {
                        "items · subfolders included"
                    } else {
                        "results"
                    }
                )));
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
                ui.label(small(format!(
                    "{} items  ·  {dirs} folders, {files} files",
                    tab.view.len()
                )));
                ui.label(small(format!(
                    "·  {}{}",
                    util::human_size(bytes),
                    if all_known { "" } else { "+" }
                )));
                if tab
                    .entries
                    .iter()
                    .any(|e| e.is_dir && self.sizing.contains(&e.path))
                {
                    ui.add(egui::Spinner::new().size(11.0).color(pal().dim));
                }
            }
            let n = tab.selection_count();
            if n > 1 {
                let bytes: u64 = tab
                    .entries
                    .iter()
                    .filter(|e| tab.marked.contains(&e.path))
                    .filter_map(|e| {
                        if e.is_dir {
                            self.sizes.get(&e.path).copied()
                        } else {
                            Some(e.size)
                        }
                    })
                    .sum();
                ui.label(small(format!(
                    "·  {n} selected ({})",
                    util::human_size(bytes)
                )));
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(job) = &job {
                    let cancel = ui
                        .add(
                            egui::Button::new(RichText::new("×").size(14.0))
                                .frame_when_inactive(false),
                        )
                        .on_hover_text("Cancel");
                    if cancel.clicked() {
                        self.ops
                            .cancel
                            .store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    let frac = if job.bytes_total > 0 {
                        job.bytes_done as f32 / job.bytes_total as f32
                    } else {
                        0.0
                    };
                    if job.bytes_total > 0 {
                        ui.add(
                            egui::ProgressBar::new(frac)
                                .desired_width(140.0)
                                .desired_height(8.0)
                                .fill(pal().accent),
                        );
                        ui.label(small(format!(
                            "{:.0}%  ·  {} / {}",
                            frac * 100.0,
                            util::human_size(job.bytes_done),
                            util::human_size(job.bytes_total)
                        )));
                    } else {
                        ui.add(egui::Spinner::new().size(11.0).color(pal().dim));
                    }
                    let extra = if queued > 1 {
                        format!("  (+{} queued)", queued - 1)
                    } else {
                        String::new()
                    };
                    ui.label(
                        RichText::new(format!("{}{extra}", job.title))
                            .size(12.0)
                            .color(pal().text),
                    );
                } else if let Some((free, total)) = tab.disk {
                    ui.label(small(format!(
                        "{} free of {}",
                        util::human_size(free),
                        util::human_size(total)
                    )));
                }
            });
        });
    }
}

/// Preserve natural label widths when possible; cap longer tabs first on overflow.
fn tab_widths(natural: &[f32], budget: f32) -> Vec<f32> {
    if natural.iter().sum::<f32>() <= budget {
        return natural.to_vec();
    }
    let (mut low, mut high) = (100.0_f32, natural.iter().copied().fold(100.0_f32, f32::max));
    for _ in 0..24 {
        let cap = (low + high) * 0.5;
        if natural.iter().map(|w| w.min(cap)).sum::<f32>() > budget {
            high = cap;
        } else {
            low = cap;
        }
    }
    natural.iter().map(|w| w.min(low)).collect()
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
        if s.flatten && s.running {
            "Scanning this folder and all subfolders…".into()
        } else if s.flatten && s.query.trim().is_empty() {
            "No items in this folder or its subfolders".into()
        } else if s.query.trim().is_empty() {
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

/// Rubber-band selection: dragging from empty space selects every item the box touches.
/// `item_rect` gives an item's current on-screen rect; ctrl adds to the existing selection.
fn rubber_band(
    ui: &Ui,
    p: usize,
    tab: &mut Tab,
    inner: Rect,
    over_item: bool,
    ctrl: bool,
    item_rect: impl Fn(usize) -> Rect,
) {
    struct Band {
        /// Start point: x on screen, y relative to the content top.
        start: Pos2,
        base: HashSet<PathBuf>,
    }
    let id = ui.id().with(("rubber", p));
    let ctx = ui.ctx();
    let (pos, pressed, down) = ctx.input(|i| {
        (
            i.pointer.latest_pos(),
            i.pointer.primary_pressed(),
            i.pointer.primary_down(),
        )
    });
    let mut band = ctx
        .data(|d| d.get_temp::<Option<std::sync::Arc<Band>>>(id))
        .flatten();
    if !down {
        if band.is_some() {
            ctx.data_mut(|d| d.insert_temp::<Option<std::sync::Arc<Band>>>(id, None));
        }
        return;
    }
    let offset = tab.scroll_offset;
    if band.is_none() && pressed {
        let Some(pos) = pos else { return };
        let on_scrollbar = pos.x > inner.right() - 14.0;
        // Menus and dialogs float above the list; clicks on them are not ours.
        let covered = ctx.layer_id_at(pos).is_some_and(|l| l != ui.layer_id());
        if !inner.contains(pos)
            || over_item
            || on_scrollbar
            || covered
            || DragAndDrop::has_any_payload(ctx)
        {
            return;
        }
        if let Some(s) = &mut tab.search {
            s.follow_best = false;
        }
        let base = if ctrl {
            tab.targets().into_iter().collect()
        } else {
            HashSet::new()
        };
        if !ctrl {
            tab.marked.clear();
            tab.cursor = None;
        }
        let b = std::sync::Arc::new(Band {
            start: Pos2::new(pos.x, pos.y - inner.top() + offset),
            base,
        });
        ctx.data_mut(|d| d.insert_temp(id, Some(b.clone())));
        band = Some(b);
    }
    let (Some(band), Some(pos)) = (band, pos) else {
        return;
    };
    let start = Pos2::new(band.start.x, inner.top() + band.start.y - offset);
    let cur = pos.clamp(inner.min, inner.max);
    let boxr = Rect::from_two_pos(start, cur).intersect(inner);
    let mut hits: Vec<usize> = (0..tab.view.len())
        .filter(|&vi| item_rect(vi).intersects(boxr))
        .collect();
    hits.sort_unstable();
    let mut marked = band.base.clone();
    marked.extend(
        hits.iter()
            .map(|&vi| tab.entries[tab.view[vi]].path.clone()),
    );
    tab.cursor = hits.last().copied().or(tab.cursor);
    tab.anchor = tab.cursor;
    tab.marked = marked;
    let accent = pal().accent;
    let painter = ui.painter().with_clip_rect(inner);
    painter.rect_filled(boxr, 0.0, accent.gamma_multiply(0.18));
    painter.rect_stroke(
        boxr,
        0.0,
        Stroke::new(1.0, accent),
        egui::StrokeKind::Inside,
    );
    // Scroll while the pointer is held beyond the top or bottom edge.
    const EDGE: f32 = 24.0;
    let speed = if pos.y < inner.top() + EDGE {
        pos.y - (inner.top() + EDGE)
    } else if pos.y > inner.bottom() - EDGE {
        pos.y - (inner.bottom() - EDGE)
    } else {
        0.0
    };
    if speed != 0.0 {
        tab.force_offset = Some((tab.scroll_offset + speed * 0.4).max(0.0));
    }
    ctx.request_repaint();
}

/// (free, total) bytes of the filesystem at `path`, cached for a few seconds.
fn disk_usage(ctx: &egui::Context, path: &Path) -> Option<(u64, u64)> {
    const TTL: std::time::Duration = std::time::Duration::from_secs(5);
    let id = egui::Id::new(("disk_usage", path));
    if let Some((at, usage)) = ctx.data(|d| d.get_temp::<(Instant, Option<(u64, u64)>)>(id))
        && at.elapsed() < TTL
    {
        return usage;
    }
    let usage = fsops::disk_space(path);
    ctx.data_mut(|d| d.insert_temp(id, (Instant::now(), usage)));
    usage
}

/// Scrolls the list while a drag hovers near its top or bottom edge.
fn drag_autoscroll(ui: &Ui, tab: &mut Tab, viewport: Rect) {
    if !DragAndDrop::has_payload_of_type::<DragPaths>(ui.ctx()) {
        return;
    }
    let Some(pos) = ui.ctx().pointer_latest_pos() else {
        return;
    };
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
fn bookmark_zone(
    ui: &Ui,
    resp: &egui::Response,
    dirs: &[PathBuf],
    actions: &mut Vec<Action>,
) -> bool {
    let hot = resp.dnd_hover_payload::<DragPaths>().is_some();
    if hot {
        ui.ctx()
            .data_mut(|d| d.insert_temp(bookmark_hover_id(), true));
    }
    if take_file_drop(resp).is_some() {
        actions.push(Action::Bookmark(dirs.to_vec()));
    }
    hot
}

fn bookmark_hover_id() -> egui::Id {
    egui::Id::new("kestrel_bookmark_hover")
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
    ctx.data_mut(|d| {
        d.get_temp_mut_or_default::<Vec<(Rect, PathBuf)>>(folder_rects_id())
            .push((rect, path.to_path_buf()))
    });
}

fn drop_target_id() -> egui::Id {
    egui::Id::new("kestrel_drop_target")
}

/// True while a drag hovers `target`; also records it so the drag card can say where it goes.
fn drop_hover(resp: &egui::Response, target: &Path) -> bool {
    let hovering = resp
        .dnd_hover_payload::<DragPaths>()
        .is_some_and(|p| !p.0.iter().any(|s| s == target));
    if hovering {
        resp.ctx
            .data_mut(|d| d.insert_temp(drop_target_id(), target.to_path_buf()));
    }
    hovering
}

/// Only the left button can finish a file drag. egui's generic DnD helper
/// accepts any button release, including the back/forward navigation buttons.
fn take_file_drop(resp: &egui::Response) -> Option<Arc<DragPaths>> {
    if resp.contains_pointer()
        && resp.ctx.input(|i| i.pointer.primary_released())
        && DragAndDrop::has_payload_of_type::<DragPaths>(&resp.ctx)
    {
        DragAndDrop::take_payload::<DragPaths>(&resp.ctx)
    } else {
        None
    }
}

/// Dropping onto empty space in a pane moves/copies into that pane's folder.
fn background_drop(ui: &mut Ui, tab: &Tab, rect: Rect, actions: &mut Vec<Action>) {
    if tab.search.is_some() {
        return;
    }
    // Not `Response::contains_pointer`: it is false whenever a file row covers the pointer, which
    // would restrict the drop to empty space. Rows that are folders take the payload first.
    let ctx = ui.ctx();
    let over = ctx
        .input(|i| i.pointer.interact_pos())
        .is_some_and(|pos| rect.contains(pos))
        && ctx.layer_id_at(ctx.input(|i| i.pointer.interact_pos()).unwrap_or_default())
            == Some(ui.layer_id());
    if !over {
        return;
    }
    // Rows are checked first; only claim the hint if no folder row did.
    let claimed = ctx.data(|d| d.get_temp::<PathBuf>(drop_target_id()).is_some());
    if !claimed
        && let Some(d) = DragAndDrop::payload::<DragPaths>(ctx)
        && d.0.iter().any(|s| s.parent() != Some(tab.path.as_path()))
    {
        ctx.data_mut(|d| d.insert_temp(drop_target_id(), tab.path.clone()));
    }
    if ctx.input(|i| i.pointer.primary_released())
        && let Some(paths) = DragAndDrop::take_payload::<DragPaths>(ctx)
    {
        let copy = ctx.input(|i| i.modifiers.ctrl);
        actions.push(Action::Drop {
            paths: paths.0.clone(),
            dest: tab.path.clone(),
            copy,
        });
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "immediate-mode paint helper; a parameter struct would just mirror the call site"
)]
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
    if resp.drag_started_by(egui::PointerButton::Primary) {
        let paths = if tab.is_selected(vi) {
            tab.targets()
        } else {
            vec![e.path.clone()]
        };
        DragAndDrop::set_payload(ui.ctx(), DragPaths(paths));
    }
    if let Some(paths) = take_file_drop(resp)
        && e.is_dir
        && !paths.0.contains(&e.path)
    {
        actions.push(Action::Drop {
            paths: paths.0.clone(),
            dest: e.path.clone(),
            copy: ctrl,
        });
    }
    if resp.double_clicked() {
        actions.push(Action::FocusPane(p));
        actions.push(Action::OpenEntry(vi));
    } else if resp.clicked() {
        actions.push(Action::FocusPane(p));
        actions.push(Action::Click { vi, ctrl, shift });
    } else if resp.secondary_clicked() && !tab.is_selected(vi) {
        actions.push(Action::FocusPane(p));
        actions.push(Action::Click {
            vi,
            ctrl: false,
            shift: false,
        });
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
                item(
                    ui,
                    "Remove from bookmarks",
                    "",
                    Action::Unbookmark(e.path.clone()),
                );
            } else {
                let dirs: Vec<PathBuf> = if tab.is_selected(vi) {
                    tab.targets()
                } else {
                    vec![e.path.clone()]
                };
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
        let targets = if tab.is_selected(vi) {
            tab.targets()
        } else {
            vec![e.path.clone()]
        };
        if targets.iter().any(|t| crate::ops::is_archive(t)) {
            item(ui, "Extract here", "", Action::Cmd(Cmd::Extract));
            item(ui, "Extract to folder", "", Action::Cmd(Cmd::ExtractFolder));
            if ui
                .ctx()
                .data(|d| d.get_temp::<bool>(egui::Id::new("is_split")))
                .unwrap_or(false)
            {
                item(
                    ui,
                    "Extract to other pane",
                    "",
                    Action::Cmd(Cmd::ExtractOther),
                );
            }
        }
        item(ui, "Compress to .zip", "", Action::Cmd(Cmd::Compress));
        item(ui, "Compress…", "", Action::Cmd(Cmd::CompressAs));
        ui.separator();
        item(ui, "Copy path", "Alt+C", Action::CopyPath(e.path.clone()));
        let term = if e.is_dir {
            e.path.clone()
        } else {
            tab.path.clone()
        };
        item(ui, "Open terminal here", "", Action::OpenTerminal(term));
        ui.separator();
        item(ui, "Move to trash", "Delete", Action::Cmd(Cmd::Trash));
        item(
            ui,
            "Delete permanently",
            "Shift+Del",
            Action::Cmd(Cmd::DeletePermanently),
        );
        actions.extend(open_with);
    });
}

/// "Open with" submenu: apps for this file type, default first, and a way to change the default.
fn open_with_menu(
    ui: &mut Ui,
    tab: &Tab,
    vi: usize,
    e: &Entry,
    p: usize,
    actions: &mut Vec<Action>,
) {
    let choices = crate::apps::choices(&e.path);
    if choices.apps.is_empty() {
        return;
    }
    let paths: Vec<PathBuf> = if tab.is_selected(vi) {
        tab.targets().into_iter().filter(|t| !t.is_dir()).collect()
    } else {
        vec![e.path.clone()]
    };
    let what = match e.path.extension() {
        Some(ext) => format!(".{} files", ext.to_string_lossy()),
        None => "files like this".into(),
    };
    const SHOWN: usize = 12;
    ui.menu_button("Open with", |ui| {
        ui.set_min_width(200.0);
        for app in choices.apps.iter().take(SHOWN) {
            let default = choices.default.as_deref() == Some(app.id.as_str());
            if ui
                .add(egui::Button::new(&app.name).shortcut_text(if default {
                    "default"
                } else {
                    ""
                }))
                .clicked()
            {
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
                if ui
                    .add_enabled(!default, egui::Button::new(&app.name))
                    .clicked()
                {
                    actions.push(Action::SetDefaultApp(
                        app.clone(),
                        choices.mime.clone(),
                        what.clone(),
                    ));
                    ui.close();
                }
            }
        });
    });
}

#[expect(
    clippy::too_many_arguments,
    reason = "immediate-mode paint helper; a parameter struct would just mirror the call site"
)]
fn paint_row(
    painter: &Painter,
    rect: Rect,
    cols: &Columns,
    e: &Entry,
    sizes: &HashMap<PathBuf, u64>,
    sizing: &std::collections::HashSet<PathBuf>,
    faded: bool,
    query: &crate::search::Query,
) {
    let cy = rect.center().y;
    let alpha = if faded { 0.45 } else { 1.0 };
    let cat = util::category(&e.ext, e.is_dir);
    draw_entry_icon(
        painter,
        Pos2::new(cols.name.0 - 12.0, cy),
        1.0,
        &e.path,
        cat,
    );
    let name_color = if e.hidden { pal().dim } else { pal().text };
    let name_clip = painter.with_clip_rect(Rect::from_x_y_ranges(
        cols.name.0..=cols.name.1,
        rect.y_range(),
    ));
    let name_rect = paint_name(
        &name_clip,
        Pos2::new(cols.name.0, cy),
        &e.name,
        query,
        FontId::proportional(BODY),
        name_color.gamma_multiply(alpha),
    );
    if e.is_link {
        painter.text(
            Pos2::new(cols.name.0 - 6.0, cy + 6.0),
            Align2::CENTER_CENTER,
            "↗",
            FontId::proportional(9.0),
            pal().accent,
        );
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
        painter.text(
            Pos2::new(size.1, cy),
            Align2::RIGHT_CENTER,
            size_text,
            small.clone(),
            pal().dim.gamma_multiply(alpha),
        );
    }
    if let (Some(m), Some(date)) = (e.modified, cols.date) {
        painter.text(
            Pos2::new(date.0, cy),
            Align2::LEFT_CENTER,
            util::fmt_time(m),
            small.clone(),
            pal().dim.gamma_multiply(alpha),
        );
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
            .text(
                Pos2::new(k.0, cy),
                Align2::LEFT_CENTER,
                text,
                small,
                pal().faint,
            );
    } else if let Some(rel) = e.rel.as_ref().filter(|r| !r.is_empty()) {
        name_clip.text(
            name_rect.right_center() + vec2(8.0, 0.0),
            Align2::LEFT_CENTER,
            rel,
            small,
            pal().faint,
        );
    }
}

/// A List-mode cell: icon and name on one line.
fn paint_cell(painter: &Painter, cell: Rect, e: &Entry, faded: bool, query: &crate::search::Query) {
    let alpha = if faded { 0.45 } else { 1.0 };
    let cy = cell.center().y;
    draw_entry_icon(
        painter,
        Pos2::new(cell.left() + 14.0, cy),
        1.0,
        &e.path,
        util::category(&e.ext, e.is_dir),
    );
    let color = if e.hidden { pal().dim } else { pal().text };
    paint_name(
        &painter.with_clip_rect(cell.shrink2(vec2(6.0, 0.0)).intersect(painter.clip_rect())),
        Pos2::new(cell.left() + 30.0, cy),
        &e.name,
        query,
        FontId::proportional(BODY),
        color.gamma_multiply(alpha),
    );
}

fn paint_tile(
    painter: &Painter,
    thumbs: &mut Thumbs,
    tile: Rect,
    e: &Entry,
    faded: bool,
    query: &crate::search::Query,
) {
    let alpha = if faded { 0.45 } else { 1.0 };
    let side = tile.width().min(tile.height() - 48.0) - 16.0;
    let thumb_area = Rect::from_center_size(
        Pos2::new(tile.center().x, tile.top() + 8.0 + side / 2.0),
        vec2(tile.width() - 16.0, side),
    );
    let renderer = if e.is_dir {
        None
    } else {
        thumbs::renderer_for(&e.ext)
    };
    let mut drew = false;
    if let Some(r) = renderer
        && let Some(tex) = thumbs.get(&e.path, r)
    {
        let ts = tex.size_vec2();
        let scale = (thumb_area.width() / ts.x)
            .min(thumb_area.height() / ts.y)
            .min(1.0);
        let rect = Rect::from_center_size(thumb_area.center(), ts * scale);
        painter.image(tex.id(), rect, UV, Color32::WHITE.gamma_multiply(alpha));
        drew = true;
    }
    if renderer.is_none()
        && !e.is_dir
        && !matches!(
            util::category(&e.ext, false),
            util::Category::Audio
                | util::Category::Archive
                | util::Category::Document
                | util::Category::Executable
        )
        && let Some(job) = thumbs.get_text(&e.path)
    {
        paint_text_thumbnail(painter, thumb_area, job, alpha);
        drew = true;
    }
    if !drew {
        let cat = util::category(&e.ext, e.is_dir);
        draw_entry_icon(painter, thumb_area.center(), side / 24.0, &e.path, cat);
    }
    let color = if e.hidden { pal().dim } else { pal().text }.gamma_multiply(alpha);
    let mut job = name_job(&e.name, query, FontId::proportional(12.5), color);
    job.wrap.max_width = tile.width() - 12.0;
    job.wrap.max_rows = 2;
    job.wrap.break_anywhere = true;
    job.halign = Align::Center;
    let galley = painter.layout_job(job);
    painter.galley(
        Pos2::new(tile.center().x, thumb_area.bottom() + 8.0),
        galley,
        color,
    );
}

/// Decorative document content: tile responses continue to own every interaction.
pub(super) fn paint_text_thumbnail(
    painter: &Painter,
    area: Rect,
    source: &egui::text::LayoutJob,
    alpha: f32,
) {
    let page = Rect::from_center_size(area.center(), vec2(area.height() * 0.78, area.height()));
    let radius = (page.width() * 0.04).clamp(2.0, 6.0) as u8;
    painter.rect_filled(page, radius, pal().panel.gamma_multiply(alpha));
    painter.rect_stroke(
        page,
        radius,
        Stroke::new(1.0, pal().dim.gamma_multiply(alpha * 0.6)),
        egui::StrokeKind::Inside,
    );
    let padding = (page.width() * 0.08).clamp(3.0, 12.0);
    let body = page.shrink(padding);
    let mut job = source.clone();
    let font_size = (page.height() / 19.0).clamp(4.0, 11.0);
    for section in &mut job.sections {
        section.format.font_id = FontId::monospace(font_size);
        section.format.color = section.format.color.gamma_multiply(alpha);
    }
    job.wrap.max_width = f32::INFINITY;
    job.wrap.max_rows = 24;
    job.halign = Align::Min;
    let galley = painter.layout_job(job);
    painter
        .with_clip_rect(body.intersect(painter.clip_rect()))
        .galley(body.min, galley, pal().text.gamma_multiply(alpha));
}

// ---------------------------------------------------------------- dialogs

pub(super) fn name_job(
    name: &str,
    query: &crate::search::Query,
    font: FontId,
    color: Color32,
) -> egui::text::LayoutJob {
    let hits = query.highlights(name);
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = f32::INFINITY;
    for (i, c) in name.chars().enumerate() {
        let hit = hits.binary_search(&i).is_ok();
        job.append(
            &c.to_string(),
            0.0,
            egui::TextFormat {
                font_id: font.clone(),
                color: if hit { pal().accent } else { color },
                underline: if hit {
                    Stroke::new(1.0, pal().accent)
                } else {
                    Stroke::NONE
                },
                ..Default::default()
            },
        );
    }
    job
}

fn paint_name(
    painter: &Painter,
    pos: Pos2,
    name: &str,
    query: &crate::search::Query,
    font: FontId,
    color: Color32,
) -> Rect {
    let galley = painter.layout_job(name_job(name, query, font, color));
    let rect = Align2::LEFT_CENTER.anchor_size(pos, galley.size());
    painter.galley(rect.min, galley, color);
    rect
}

fn name_field(
    ui: &mut Ui,
    title: &str,
    name: &mut String,
    focus: &mut bool,
    select: usize,
    confirm: &mut bool,
) {
    ui.label(RichText::new(title).size(15.0).strong());
    ui.add_space(8.0);
    let id = egui::Id::new("dialog_name");
    // Focus and selection go in before the TextEdit is laid out, so a paste in the same
    // frame replaces the selected name. A dialog opened from the palette is first drawn on
    // the frame that carries the Ctrl+V; an unfocused TextEdit drops it.
    let select_now = std::mem::take(focus);
    if select_now {
        let mut state = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
        let range = egui::text::CCursorRange::two(
            egui::text::CCursor::new(0),
            egui::text::CCursor::new(select),
        );
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
            .fill(if danger {
                pal().danger
            } else {
                pal().accent_strong
            })
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

const UV: Rect = Rect {
    min: Pos2::new(0.0, 0.0),
    max: Pos2::new(1.0, 1.0),
};

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
        Self {
            name,
            size,
            date,
            kind,
        }
    }
}
