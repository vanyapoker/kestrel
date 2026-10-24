use crate::app::icons::*;
use crate::app::{Action, App, DragPaths, DropMode};
use crate::theme::*;
use crate::thumbs;
use crate::util;
use eframe::egui::{
    self, Align2, Color32, CornerRadius, DragAndDrop, FontId, Frame, Margin, Pos2, Rect, RichText,
    Sense, Stroke, vec2,
};
use std::path::PathBuf;

use super::{UV, bookmark_hover_id, drop_target_id, ops_name, same_filesystem};

impl App {
    pub(in crate::app) fn external_drops(
        &mut self,
        ctx: &egui::Context,
        actions: &mut Vec<Action>,
    ) {
        // Drops via winit (X11); on Wayland they come through `wl_dnd` instead.
        let (dropped, winit_hover, pointer) = ctx.input(|i| {
            let dropped: Vec<PathBuf> = i
                .raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect();
            (
                dropped,
                !i.raw.hovered_files.is_empty(),
                i.pointer.hover_pos(),
            )
        });
        let pos = self.ext_hover.or(if winit_hover { pointer } else { None });
        if let Some(pos) = pos {
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("drop_overlay"),
            ));
            if let Some((r, _)) = self.folder_rects.iter().find(|(r, _)| r.contains(pos)) {
                painter.rect_filled(*r, CornerRadius::same(5), pal().accent.gamma_multiply(0.18));
                painter.rect_stroke(
                    *r,
                    CornerRadius::same(5),
                    Stroke::new(1.5, pal().accent),
                    egui::StrokeKind::Inside,
                );
            } else if let Some(r) = self.pane_rects.iter().find(|r| r.contains(pos)) {
                painter.rect_filled(
                    r.shrink(6.0),
                    CornerRadius::same(10),
                    pal().accent.gamma_multiply(0.10),
                );
                painter.rect_stroke(
                    r.shrink(6.0),
                    CornerRadius::same(10),
                    Stroke::new(2.0, pal().accent),
                    egui::StrokeKind::Inside,
                );
                painter.text(
                    r.center(),
                    Align2::CENTER_CENTER,
                    "Drop to copy here",
                    FontId::proportional(16.0),
                    pal().text,
                );
            }
        }
        if !dropped.is_empty()
            && let Some((dest, pane)) = self.drop_destination(pointer.unwrap_or_default())
        {
            actions.push(Action::FocusPane(pane));
            actions.push(Action::Drop {
                paths: dropped,
                dest,
                mode: DropMode::Copy,
            });
        }
    }

    /// Folder under `pos` (row, tile or sidebar entry), else the folder of the pane under it.
    pub(in crate::app) fn drop_destination(&self, pos: Pos2) -> Option<(PathBuf, usize)> {
        let pane = self
            .pane_rects
            .iter()
            .position(|r| r.contains(pos))
            .unwrap_or(self.active_pane);
        if let Some((_, p)) = self.folder_rects.iter().find(|(r, _)| r.contains(pos)) {
            return Some((p.clone(), pane));
        }
        let tab = self.tab_in(pane);
        tab.search.is_none().then(|| (tab.path.clone(), pane))
    }

    /// Hands the in-app drag to the compositor once it leaves the window.
    pub(in crate::app) fn maybe_drag_out(&mut self, ctx: &egui::Context) {
        let (Some(wl), Some(payload)) = (&self.wl, &self.dragging) else {
            return;
        };
        if wl.dragging_out() {
            return;
        }
        let Some(pos) = ctx.pointer_latest_pos() else {
            return;
        };
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
    pub(in crate::app) fn drag_chip(&mut self, ctx: &egui::Context) {
        let Some(payload) = DragAndDrop::payload::<DragPaths>(ctx) else {
            return;
        };
        let Some(first) = payload.0.first() else {
            return;
        };
        // Nothing is "held" until the pointer actually moves away from the press.
        if !ctx.input(|i| i.pointer.is_decidedly_dragging()) {
            return;
        }
        let Some(pos) = ctx.pointer_latest_pos() else {
            return;
        };
        ctx.set_cursor_icon(egui::CursorIcon::Grabbing);

        let n = payload.0.len();
        let target = ctx.data(|d| d.get_temp::<PathBuf>(drop_target_id()));
        let mode = ctx.input(|i| DropMode::held(i.modifiers.ctrl, i.modifiers.shift));
        let bookmarking = ctx
            .data(|d| d.get_temp::<bool>(bookmark_hover_id()))
            .unwrap_or(false);
        let hint = target.as_ref().map(|t| {
            let copy = match mode {
                DropMode::Copy => true,
                DropMode::Move => false,
                DropMode::Auto => !same_filesystem(first, t),
            };
            (copy, ops_name(t))
        });
        let is_dir = first.is_dir();
        let ext = first
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let cat = util::category(&ext, is_dir);
        let renderer = if is_dir {
            None
        } else {
            thumbs::renderer_for(&ext)
        };
        let thumb = renderer
            .and_then(|r| self.thumbs.get(first, r))
            .map(|t| (t.id(), t.size_vec2()));
        let name = if n == 1 {
            ops_name(first)
        } else {
            format!("{} and {} more", ops_name(first), n - 1)
        };

        let origin = pos + vec2(16.0, 12.0);
        let stack_size = (n > 1).then(|| {
            ctx.data(|d| d.get_temp::<egui::Vec2>(egui::Id::new("drag_card_size")))
                .unwrap_or(vec2(220.0, 64.0))
        });
        let shown = egui::Area::new(egui::Id::new("drag_card"))
            .order(egui::Order::Tooltip)
            .fixed_pos(origin)
            .interactable(false)
            .show(ctx, |ui| {
                let card = Frame::new()
                    .fill(pal().surface.gamma_multiply(0.96))
                    .stroke(Stroke::new(
                        1.0,
                        if hint.is_some() || bookmarking {
                            pal().accent
                        } else {
                            pal().border
                        },
                    ))
                    .corner_radius(CornerRadius::same(8))
                    .shadow(egui::Shadow {
                        offset: [0, 4],
                        blur: 14,
                        spread: 0,
                        color: Color32::from_black_alpha(110),
                    })
                    .inner_margin(Margin::symmetric(10, 8));
                // Cards peeking out behind signal a stack of several items (sized from last frame).
                if let Some(size) = stack_size {
                    for k in [2.0, 1.0] {
                        let r = Rect::from_min_size(origin + vec2(5.0, 5.0) * k, size);
                        ui.painter()
                            .rect_filled(r, CornerRadius::same(8), pal().panel);
                        ui.painter().rect_stroke(
                            r,
                            CornerRadius::same(8),
                            Stroke::new(1.0, pal().border),
                            egui::StrokeKind::Inside,
                        );
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
                                ui.painter().image(
                                    id,
                                    Rect::from_center_size(r.center(), size * scale),
                                    UV,
                                    Color32::WHITE,
                                );
                            }
                            None => draw_entry_icon(ui.painter(), r.center(), 2.6, first, cat),
                        }
                        if n > 1 {
                            let c = r.right_top() + vec2(-2.0, 2.0);
                            ui.painter().circle_filled(c, 9.0, pal().accent);
                            ui.painter().text(
                                c,
                                Align2::CENTER_CENTER,
                                n.to_string(),
                                FontId::proportional(10.5),
                                Color32::WHITE,
                            );
                        }
                        ui.add_space(4.0);
                        ui.vertical(|ui| {
                            ui.add_space(5.0);
                            ui.add(
                                egui::Label::new(RichText::new(name).color(pal().text)).truncate(),
                            );
                            let (text, color) = match &hint {
                                _ if bookmarking => ("Add to bookmarks".to_string(), pal().accent),
                                Some((true, t)) => (format!("Copy to {t}"), pal().accent),
                                Some((false, t)) => (format!("Move to {t}"), pal().accent),
                                None => ("Drop on a folder".to_string(), pal().faint),
                            };
                            ui.add(
                                egui::Label::new(RichText::new(text).size(12.0).color(color))
                                    .truncate(),
                            );
                        });
                    });
                });
            });
        ctx.data_mut(|d| {
            d.insert_temp(egui::Id::new("drag_card_size"), shown.response.rect.size())
        });
        ctx.request_repaint();
    }
}
