use crate::app::icons::*;
use crate::app::{Action, App, DropMode};
use crate::devices::DevOp;
use crate::fsops;
use crate::theme::*;
use crate::util;
use eframe::egui::{
    self, Align, Align2, CornerRadius, FontId, Key, Layout, Pos2, Rect, RichText, ScrollArea,
    Sense, Stroke, Ui, vec2,
};
use std::path::PathBuf;

use super::{
    bookmark_zone, chevron, disk_usage, drop_hover, eject_icon, record_folder, take_file_drop,
};

impl App {
    pub(in crate::app) fn sidebar(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        // Folders being dragged can be dropped on the Bookmarks section to bookmark them.
        let dragged_dirs: Vec<PathBuf> = self
            .dragging
            .as_ref()
            .map(|d| d.0.iter().filter(|p| p.is_dir()).cloned().collect())
            .unwrap_or_default();
        ScrollArea::vertical()
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                let cur = self.tab().path.clone();
                let device_items: Vec<(String, PathBuf)> = self
                    .devices
                    .iter()
                    .map(|d| (d.name.clone(), d.path().to_path_buf()))
                    .collect();
                let sections: [(&'static str, &Vec<(String, PathBuf)>); 3] = [
                    ("PLACES", &self.places.places),
                    ("BOOKMARKS", &self.places.bookmarks),
                    ("DEVICES", &device_items),
                ];
                for (title, items) in sections {
                    let bookmarks = title == "BOOKMARKS";
                    let can_drop = bookmarks && !dragged_dirs.is_empty();
                    if items.is_empty() && !can_drop {
                        continue;
                    }
                    ui.add_space(8.0);
                    let collapsed = self.collapsed.contains(title);
                    let (hrect, header) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::click());
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
                        chevron(
                            ui.painter(),
                            Pos2::new(hrect.right() - 8.0, hrect.center().y),
                            !collapsed,
                            color,
                        );
                    }
                    if header.clicked() {
                        actions.push(Action::ToggleSection(title));
                    }
                    if can_drop && bookmark_zone(ui, &header, &dragged_dirs, actions) {
                        ui.painter().rect_stroke(
                            hrect,
                            CornerRadius::same(5),
                            Stroke::new(1.5, pal().accent),
                            egui::StrokeKind::Inside,
                        );
                    }
                    ui.add_space(2.0);
                    if collapsed {
                        continue;
                    }
                    for (name, path) in items {
                        let device = if title == "DEVICES" {
                            self.devices.iter().find(|d| d.path() == path.as_path())
                        } else {
                            None
                        };
                        let unmounted = device.is_some_and(|d| d.mount.is_none());
                        let busy = device.is_some_and(|d| self.dev_busy.contains(&d.dev));
                        let ejectable = device.is_some_and(|d| d.removable && d.mount.is_some());
                        let (rect, resp) = ui
                            .allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click());
                        let selected = *path == cur;
                        let drop_target = !unmounted && drop_hover(&resp, path);
                        if !unmounted {
                            record_folder(ui.ctx(), rect, path);
                        }
                        // Eject sits on top of the row, so its click doesn't open the drive.
                        let eject_rect = Rect::from_center_size(
                            Pos2::new(rect.right() - 13.0, rect.center().y),
                            vec2(20.0, 20.0),
                        );
                        let eject = (ejectable && !busy).then(|| {
                            ui.interact(eject_rect, ui.id().with(("eject", path)), Sense::click())
                        });
                        let painter = ui.painter();
                        if selected {
                            painter.rect_filled(rect, CornerRadius::same(5), pal().select);
                        } else if resp.hovered() || drop_target {
                            painter.rect_filled(rect, CornerRadius::same(5), pal().hover);
                        }
                        if drop_target {
                            painter.rect_stroke(
                                rect,
                                CornerRadius::same(5),
                                Stroke::new(1.5, pal().accent),
                                egui::StrokeKind::Inside,
                            );
                        }
                        let kind = match title {
                            "DEVICES" => Some(fsops::Special::Drive),
                            _ => fsops::special(path),
                        };
                        let icon_color = if selected {
                            pal().accent
                        } else if unmounted {
                            pal().faint
                        } else {
                            pal().dim
                        };
                        draw_place(
                            painter,
                            Pos2::new(rect.left() + 14.0, rect.center().y),
                            kind,
                            icon_color,
                        );
                        let mut text_rect = rect.shrink(2.0);
                        if ejectable || busy {
                            text_rect.max.x = eject_rect.left();
                        }
                        painter.with_clip_rect(text_rect).text(
                            Pos2::new(rect.left() + 30.0, rect.center().y),
                            Align2::LEFT_CENTER,
                            name,
                            FontId::proportional(BODY),
                            if selected {
                                pal().text
                            } else if unmounted {
                                pal().faint
                            } else {
                                pal().dim
                            },
                        );
                        // Thin usage bar along the bottom of a mounted drive's row.
                        let usage = device
                            .filter(|_| !unmounted)
                            .and_then(|_| disk_usage(ui.ctx(), path));
                        if let Some((free, total)) = usage.filter(|u| u.1 > 0) {
                            let used = 1.0 - free as f32 / total as f32;
                            let track = Rect::from_min_max(
                                Pos2::new(rect.left() + 10.0, rect.bottom() - 3.0),
                                Pos2::new(rect.right() - 10.0, rect.bottom() - 1.5),
                            );
                            let fill = Rect::from_min_size(
                                track.min,
                                vec2(track.width() * used.clamp(0.0, 1.0), track.height()),
                            );
                            let color = if used > 0.9 {
                                pal().danger
                            } else {
                                pal().accent
                            };
                            ui.painter()
                                .rect_filled(track, CornerRadius::same(1), pal().border);
                            ui.painter().rect_filled(fill, CornerRadius::same(1), color);
                        }
                        if busy {
                            let spin = egui::Spinner::new().size(11.0).color(pal().dim);
                            ui.put(eject_rect, spin);
                        }
                        if let Some(eject) = eject {
                            if eject.hovered() {
                                ui.painter().rect_filled(
                                    eject_rect,
                                    CornerRadius::same(4),
                                    pal().select_dim,
                                );
                            }
                            let color = if eject.hovered() {
                                pal().text
                            } else {
                                pal().dim
                            };
                            eject_icon(ui.painter(), eject_rect.center(), color);
                            if eject.on_hover_text("Eject").clicked() {
                                actions.push(Action::Device(
                                    DevOp::Eject,
                                    device.unwrap().dev.clone(),
                                ));
                            }
                        }
                        if !unmounted && let Some(paths) = take_file_drop(&resp) {
                            actions.push(Action::Drop {
                                paths: paths.0.clone(),
                                dest: path.clone(),
                                mode: DropMode::from_input(ui),
                            });
                        }
                        let hover = if unmounted {
                            format!("{} is not mounted. Click to mount", path.display())
                        } else if let Some((free, total)) = usage {
                            format!(
                                "{}\n{} free of {}",
                                path.display(),
                                util::human_size(free),
                                util::human_size(total)
                            )
                        } else {
                            path.display().to_string()
                        };
                        let resp = resp.on_hover_text(hover);
                        if resp.clicked() && !busy {
                            actions.push(match device {
                                Some(d) if unmounted => Action::Device(DevOp::Mount, d.dev.clone()),
                                _ => Action::Navigate(path.clone()),
                            });
                        }
                        if unmounted {
                            resp.context_menu(|ui| {
                                ui.set_min_width(180.0);
                                let d = device.unwrap();
                                if ui.add_enabled(!busy, egui::Button::new("Mount")).clicked() {
                                    actions.push(Action::Device(DevOp::Mount, d.dev.clone()));
                                    ui.close();
                                }
                                if d.removable
                                    && ui.add_enabled(!busy, egui::Button::new("Eject")).clicked()
                                {
                                    actions.push(Action::Device(DevOp::Eject, d.dev.clone()));
                                    ui.close();
                                }
                            });
                            continue;
                        }
                        if resp.middle_clicked() {
                            actions.push(Action::NewTab(path.clone()));
                        }
                        let marked =
                            bookmarks || self.places.bookmarks.iter().any(|(_, b)| b == path);
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
                            if let Some(d) = device {
                                if ui
                                    .add_enabled(!busy, egui::Button::new("Unmount"))
                                    .clicked()
                                {
                                    actions.push(Action::Device(DevOp::Unmount, d.dev.clone()));
                                    ui.close();
                                }
                                if d.removable
                                    && ui.add_enabled(!busy, egui::Button::new("Eject")).clicked()
                                {
                                    actions.push(Action::Device(DevOp::Eject, d.dev.clone()));
                                    ui.close();
                                }
                                ui.separator();
                            }
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
                        let (rect, resp) = ui
                            .allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::hover());
                        let hot = bookmark_zone(ui, &resp, &dragged_dirs, actions);
                        let painter = ui.painter();
                        painter.rect_filled(
                            rect,
                            CornerRadius::same(5),
                            if hot { pal().select_dim } else { pal().bg },
                        );
                        painter.rect_stroke(
                            rect,
                            CornerRadius::same(5),
                            Stroke::new(1.0, if hot { pal().accent } else { pal().border }),
                            egui::StrokeKind::Inside,
                        );
                        draw_emblem(
                            painter,
                            Pos2::new(rect.left() + 14.0, rect.center().y),
                            fsops::Special::Bookmark,
                            0.9,
                            if hot { pal().accent } else { pal().dim },
                            1.4,
                        );
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

    /// The bar at the bottom of a file-picker window: heading, name field when saving, buttons.
    pub(in crate::app) fn picker_bar(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let choice = self.pick_choice();
        let dir = self.tab().path.clone();
        let Some(p) = &mut self.picker else { return };
        let saving = p.req.kind == crate::picker::Kind::Save;
        let replacing = saving && choice.first().is_some_and(|c| c.exists());
        let label = if replacing {
            "Replace"
        } else {
            p.req.confirm_label()
        };
        let mut enter = false;
        ui.horizontal_centered(|ui| {
            ui.label(RichText::new(p.req.heading()).strong());
            ui.add_space(8.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Cancel").clicked() {
                    actions.push(Action::PickCancel);
                }
                let text = match choice.len() {
                    n if n > 1 => format!("{label} {n}"),
                    _ => label.to_string(),
                };
                if ui
                    .add_enabled(!choice.is_empty(), egui::Button::new(text))
                    .clicked()
                {
                    actions.push(Action::PickConfirm);
                }
                if saving {
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut p.name)
                            .hint_text("File name")
                            .desired_width(f32::INFINITY),
                    );
                    if std::mem::take(&mut p.focus) {
                        r.request_focus();
                    }
                    enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                } else if p.req.kind == crate::picker::Kind::Folder {
                    let name = dir
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "/".into());
                    if ui.button(format!("Use “{name}”")).clicked() {
                        actions.push(Action::PickHere);
                    }
                }
            });
        });
        if enter {
            actions.push(Action::PickConfirm);
        }
    }
}
