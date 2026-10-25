use crate::app::{Action, App, Dialog, NewKind, Resolution};
use crate::devices::DevOp;
use crate::palette::{self, COMMANDS};
use crate::theme::*;
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Frame, Key, Layout, Margin, Modifiers,
    RichText, ScrollArea, Sense, Stroke, vec2,
};

use super::{dialog_buttons, name_field, ops_name, stem_chars};

impl App {
    pub(in crate::app) fn palette_ui(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        if !self.palette.open {
            return;
        }
        let goto = self.palette.goto;
        // Commands, then places to jump to. Go To instead lists folders and tabs.
        let mut items: Vec<(String, String, Action)> = COMMANDS
            .iter()
            .map(|&(c, name, keys)| (name.to_string(), keys.to_string(), Action::Cmd(c)))
            .collect();
        for (name, path) in self.places.places.iter().chain(&self.places.bookmarks) {
            items.push((
                format!("Go to {name}"),
                path.display().to_string(),
                Action::Navigate(path.clone()),
            ));
        }
        let targets = self.tab().targets();
        for (i, a) in self.custom_actions.iter().enumerate() {
            if a.applies(&targets) {
                items.push((
                    format!("Action: {}", a.name),
                    String::new(),
                    Action::Custom(i),
                ));
            }
        }
        for d in &self.devices {
            let action = match &d.mount {
                Some(m) => Action::Navigate(m.clone()),
                None => Action::Device(DevOp::Mount, d.dev.clone()),
            };
            items.push((
                format!("Go to {}", d.name),
                d.path().display().to_string(),
                action,
            ));
        }
        for d in self
            .devices
            .iter()
            .filter(|d| d.removable && d.mount.is_some())
        {
            items.push((
                format!("Eject {}", d.name),
                d.dev.display().to_string(),
                Action::Device(DevOp::Eject, d.dev.clone()),
            ));
        }
        if goto {
            items = self.goto_items();
        }
        let q = self.palette.query.trim().to_string();
        let mut scored: Vec<usize> = if goto {
            let query = crate::search::Query::new(&q);
            let mut ranked: Vec<_> = items
                .iter()
                .enumerate()
                .filter_map(|(i, (name, path, _))| {
                    query
                        .score(name)
                        .max(query.score(path))
                        .map(|score| (score, i))
                })
                .collect();
            ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            ranked.into_iter().map(|(_, i)| i).collect()
        } else {
            let mut ranked: Vec<_> = items
                .iter()
                .enumerate()
                .filter_map(|(i, (name, _, _))| palette::fuzzy_score(&q, name).map(|s| (s, i)))
                .collect();
            ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            ranked.into_iter().map(|(_, i)| i).collect()
        };
        scored.truncate(if goto { 9 } else { 14 });
        let n = scored.len();
        let sel = &mut self.palette.selected;
        let (mut run, mut close) = (false, false);
        ctx.input_mut(|i| {
            if crate::app::take(i, Modifiers::NONE, Key::ArrowDown) && n > 0 {
                *sel = (*sel + 1) % n;
            }
            if crate::app::take(i, Modifiers::NONE, Key::ArrowUp) && n > 0 {
                *sel = (*sel + n - 1) % n;
            }
            run = crate::app::take(i, Modifiers::NONE, Key::Enter);
            close = crate::app::take(i, Modifiers::NONE, Key::Escape);
        });
        *sel = (*sel).min(n.saturating_sub(1));

        let area = egui::Area::new(egui::Id::new("palette_area"))
            .anchor(Align2::CENTER_TOP, vec2(0.0, 70.0));
        let modal = egui::Modal::new(egui::Id::new("palette"))
            .area(area)
            .backdrop_color(Color32::from_black_alpha(90))
            .frame(
                Frame::new()
                    .fill(pal().panel)
                    .stroke(Stroke::new(1.0, pal().border))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::same(8)),
            )
            .show(ctx, |ui| {
                ui.set_width(520.0);
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.palette.query)
                        .hint_text(if goto {
                            "Go To: folder, open or recently closed tab…"
                        } else {
                            "Type a command or place…"
                        })
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
                for (row, &i) in scored.iter().enumerate() {
                    let (name, keys, _) = &items[i];
                    let (rect, r) = ui.allocate_exact_size(
                        vec2(ui.available_width(), if goto { 48.0 } else { 28.0 }),
                        Sense::click(),
                    );
                    let selected = row == self.palette.selected;
                    if selected || r.hovered() {
                        ui.painter().rect_filled(
                            rect,
                            CornerRadius::same(5),
                            if selected { pal().select } else { pal().hover },
                        );
                    }
                    if goto {
                        let painter = ui.painter().with_clip_rect(rect.shrink(6.0));
                        painter.text(
                            rect.left_top() + vec2(10.0, 4.0),
                            Align2::LEFT_TOP,
                            name,
                            FontId::proportional(BODY),
                            pal().text,
                        );
                        painter.text(
                            rect.left_top() + vec2(10.0, 26.0),
                            Align2::LEFT_TOP,
                            keys,
                            FontId::proportional(12.0),
                            pal().faint,
                        );
                        r.clone().on_hover_text(keys);
                    } else {
                        ui.painter().text(
                            rect.left_center() + vec2(10.0, 0.0),
                            Align2::LEFT_CENTER,
                            name,
                            FontId::proportional(BODY),
                            pal().text,
                        );
                        ui.painter().with_clip_rect(rect).text(
                            rect.right_center() - vec2(10.0, 0.0),
                            Align2::RIGHT_CENTER,
                            keys,
                            FontId::proportional(12.0),
                            pal().faint,
                        );
                    }
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
            if let Some(&i) = scored.get(self.palette.selected) {
                let action = items.swap_remove(i).2;
                actions.push(action);
            }
            close = true;
        }
        if close || modal.should_close() {
            self.palette.open = false;
        }
    }

    pub(in crate::app) fn dialog_ui(&mut self, ctx: &egui::Context) {
        let Some(dialog) = self.dialog.as_mut() else {
            return;
        };
        let (mut confirm, mut cancel) = (false, false);
        let (mut load_workspace, mut delete_workspace) = (None, None);
        let mut resolved = None;
        let frame = Frame::new()
            .fill(pal().panel)
            .stroke(Stroke::new(1.0, pal().border))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::same(16));
        let modal = egui::Modal::new(egui::Id::new("dialog")).frame(frame).show(ctx, |ui| {
            ui.set_width(380.0);
            match dialog {
                Dialog::SaveWorkspace { name, focus } => {
                    name_field(ui, "Save current workspace", name, focus, name.chars().count(), &mut confirm);
                    let exists = self.workspaces.contains_key(name.trim());
                    if exists { ui.label(RichText::new("This replaces the saved workspace with the same name.").color(pal().dim)); }
                    ui.horizontal(|ui| {
                        if ui.add_enabled(!name.trim().is_empty(), egui::Button::new(if exists { "Replace workspace" } else { "Save workspace" })).clicked() { confirm = true; }
                        if ui.button("Cancel").clicked() { cancel = true; }
                    });
                    confirm &= !name.trim().is_empty();
                }
                Dialog::Workspaces => {
                    ui.set_width(540.0_f32.min(ctx.content_rect().width() - 48.0));
                    ui.label(RichText::new("Workspaces").size(16.0).strong());
                    if self.workspaces.is_empty() { ui.label("No saved workspaces. Use “Save current workspace” in Ctrl+P."); }
                    ScrollArea::vertical().max_height(380.0).show(ui, |ui| {
                        for (name, session) in &self.workspaces {
                            ui.push_id(name, |ui| {
                                ui.horizontal(|ui| {
                                    ui.vertical(|ui| {
                                        ui.label(RichText::new(name).strong());
                                        ui.label(RichText::new(format!("{} panes · {} tabs", session.panes.len(), session.panes.iter().map(|p| p.tabs.len()).sum::<usize>())).size(12.0).color(pal().dim));
                                    });
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        if ui.button("Delete").clicked() { delete_workspace = Some(name.clone()); }
                                        if ui.button("Load").clicked() { load_workspace = Some(name.clone()); }
                                    });
                                });
                            });
                            ui.separator();
                        }
                    });
                    if ui.button("Close").clicked() { cancel = true; }
                }
                Dialog::BatchRename(editor) => {
                    ui.set_width(660.0_f32.min(ctx.content_rect().width() - 48.0));
                    editor.poll();
                    ui.label(RichText::new("Batch rename").size(16.0).strong());
                    let mut changed = false;
                    egui::Grid::new("batch_options").num_columns(2).spacing(vec2(12.0, 6.0)).show(ui, |ui| {
                        ui.label("Template");
                        changed |= ui.text_edit_singleline(&mut editor.options.template).changed();
                        ui.end_row();
                        ui.label("Find in name");
                        changed |= ui.text_edit_singleline(&mut editor.options.find).changed();
                        ui.end_row();
                        ui.label("Replace with");
                        changed |= ui.text_edit_singleline(&mut editor.options.replace).changed();
                        ui.end_row();
                        ui.label("Name case");
                        ui.horizontal(|ui| {
                            for (case, label) in [(crate::rename::Case::Keep, "Keep"), (crate::rename::Case::Lower, "lower"), (crate::rename::Case::Upper, "UPPER"), (crate::rename::Case::Title, "Title")] {
                                changed |= ui.selectable_value(&mut editor.options.case, case, label).changed();
                            }
                        });
                        ui.end_row();
                        ui.label("Numbering");
                        ui.horizontal(|ui| {
                            ui.label("Start");
                            changed |= ui.add(egui::DragValue::new(&mut editor.options.start)).changed();
                            ui.label("Digits");
                            changed |= ui.add(egui::DragValue::new(&mut editor.options.digits).range(1..=12)).changed();
                        });
                        ui.end_row();
                    });
                    ui.label(RichText::new("{name} name · {ext} original extension · {n} number · {date} modified date (YYYY-MM-DD)").size(12.0).color(pal().dim));
                    if changed { editor.request(); }
                    ui.separator();
                    if let Some(plan) = &editor.plan {
                        let errors = plan.rows.iter().filter(|r| r.error.is_some()).count();
                        ui.label(format!("{} changes · {errors} conflicts", plan.rows.iter().filter(|r| r.from != r.to).count()));
                        ScrollArea::vertical().max_height(280.0).id_salt("rename_preview")
                            .show_rows(ui, 42.0, plan.rows.len(), |ui, range| {
                                for i in range {
                                    let row = &plan.rows[i];
                                    let text = format!("{} → {}", ops_name(&row.from), ops_name(&row.to));
                                    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 42.0), Sense::hover());
                                    let painter = ui.painter().with_clip_rect(rect);
                                    painter.text(rect.left_top(), Align2::LEFT_TOP, text, FontId::proportional(BODY), if row.error.is_some() { pal().danger } else { pal().text });
                                    painter.text(rect.left_top() + vec2(0.0, 23.0), Align2::LEFT_TOP, row.error.as_deref().unwrap_or(""), FontId::proportional(11.0), pal().danger);
                                    resp.on_hover_text(format!("{} → {}", row.from.display(), row.to.display()));
                                }
                            });
                    } else { ui.horizontal(|ui| { ui.spinner(); ui.label("Checking names and collisions…"); }); }
                    ui.horizontal(|ui| {
                        let valid = editor.plan.as_ref().is_some_and(|p| p.valid());
                        if ui.add_enabled(valid, egui::Button::new("Rename all")).clicked() { confirm = true; }
                        if ui.button("Cancel").clicked() { cancel = true; }
                    });
                }
                Dialog::Rename { path, name, focus } => {
                    // Select the name without its extension, like other file managers.
                    let stem = if path.is_dir() { name.chars().count() } else { stem_chars(name) };
                    name_field(ui, "Rename", name, focus, stem, &mut confirm);
                    dialog_buttons(ui, "Rename", false, &mut confirm, &mut cancel);
                }
                Dialog::Create { name, kind, focus, .. } => {
                    let title = if *kind == NewKind::Folder { "New folder" } else { "New file" };
                    let all = name.chars().count();
                    name_field(ui, title, name, focus, all, &mut confirm);
                    dialog_buttons(ui, "Create", false, &mut confirm, &mut cancel);
                }
                Dialog::Compress { name, format, level, focus, .. } => {
                    let all = name.chars().count();
                    name_field(ui, "Compress", name, focus, all, &mut confirm);
                    ui.add_space(8.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("Format").color(pal().dim));
                        for f in crate::ops::Format::ALL {
                            ui.selectable_value(format, f, f.label());
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Level ").color(pal().dim));
                        if *format != crate::ops::Format::Zip && *level == crate::ops::Level::Store {
                            *level = crate::ops::Level::Normal;
                        }
                        if *format == crate::ops::Format::Zip {
                            ui.selectable_value(level, crate::ops::Level::Store, "Store");
                        }
                        ui.selectable_value(level, crate::ops::Level::Fast, "Fast");
                        ui.selectable_value(level, crate::ops::Level::Normal, "Normal");
                        ui.selectable_value(level, crate::ops::Level::Best, "Best");
                    });
                    ui.add_space(2.0);
                    ui.label(RichText::new(format!("{}{}", name, format.ext())).size(12.0).color(pal().faint));
                    dialog_buttons(ui, "Compress", false, &mut confirm, &mut cancel);
                }
                Dialog::Permissions(edit) => {
                    let title = match edit.paths.as_slice() {
                        [one] => format!("Permissions of \"{}\"", ops_name(one)),
                        many => format!("Permissions of {} items", many.len()),
                    };
                    ui.label(RichText::new(title).size(15.0).strong());
                    ui.add_space(8.0);
                    egui::Grid::new("permission_bits").spacing(vec2(16.0, 4.0)).show(ui, |ui| {
                        ui.label("");
                        for h in ["Read", "Write", "Execute"] {
                            ui.label(RichText::new(h).size(12.0).color(pal().faint));
                        }
                        ui.end_row();
                        for (who, shift) in [("Owner", 6), ("Group", 3), ("Others", 0)] {
                            ui.label(RichText::new(who).color(pal().dim));
                            for bit in [4u32, 2, 1] {
                                let mask = bit << shift;
                                let mut on = edit.mode & mask != 0;
                                if ui.checkbox(&mut on, "").changed() {
                                    edit.mode = if on { edit.mode | mask } else { edit.mode & !mask };
                                }
                            }
                            ui.end_row();
                        }
                    });
                    ui.horizontal(|ui| {
                        for (label, mask) in [("Set user ID", 0o4000u32), ("Set group ID", 0o2000), ("Sticky", 0o1000)] {
                            let mut on = edit.mode & mask != 0;
                            if ui.checkbox(&mut on, label).changed() {
                                edit.mode = if on { edit.mode | mask } else { edit.mode & !mask };
                            }
                        }
                    });
                    ui.label(RichText::new(format!("{}  ({:04o})", crate::util::fmt_mode(edit.mode, false), edit.mode)).size(12.0).color(pal().faint));
                    ui.add_space(8.0);
                    egui::Grid::new("permission_owner").spacing(vec2(12.0, 6.0)).show(ui, |ui| {
                        ui.label(RichText::new("Owner").color(pal().dim));
                        ui.add(egui::TextEdit::singleline(&mut edit.owner).desired_width(200.0));
                        ui.end_row();
                        ui.label(RichText::new("Group").color(pal().dim));
                        ui.add(egui::TextEdit::singleline(&mut edit.group).desired_width(200.0));
                        ui.end_row();
                    });
                    if edit.owner != edit.original_owner {
                        ui.label(RichText::new("Changing the owner usually requires administrator rights.").size(12.0).color(pal().faint));
                    }
                    if edit.any_dir {
                        ui.add_space(4.0);
                        ui.checkbox(&mut edit.recursive, "Apply to contents")
                            .on_hover_text("Files only keep an execute bit if they already had one");
                    }
                    dialog_buttons(ui, "Apply", false, &mut confirm, &mut cancel);
                }
                Dialog::SelectPattern { pattern, focus } => {
                    let all = pattern.chars().count();
                    name_field(ui, "Select by pattern", pattern, focus, all, &mut confirm);
                    ui.add_space(4.0);
                    ui.label(RichText::new("Wildcards * and ?, separated by commas: *.jpg, IMG_??.png").size(12.0).color(pal().faint));
                    dialog_buttons(ui, "Select", false, &mut confirm, &mut cancel);
                    confirm &= !pattern.trim().is_empty();
                }
                Dialog::ConfirmEmptyTrash => {
                    ui.label(RichText::new("Empty the trash?").size(15.0).strong());
                    ui.add_space(6.0);
                    ui.label(RichText::new("Everything in the trash will be deleted. This can't be undone.").color(pal().dim));
                    dialog_buttons(ui, "Empty trash", true, &mut confirm, &mut cancel);
                    ui.input_mut(|i| {
                        confirm |= crate::app::take(i, Modifiers::NONE, Key::Enter);
                        cancel |= crate::app::take(i, Modifiers::NONE, Key::Backspace);
                    });
                }
                Dialog::Conflicts(c) => {
                    let src = c.pending[c.current].clone();
                    let target = c.job.dest.clone().unwrap_or_default().join(ops_name(&src));
                    ui.label(RichText::new("Name already exists").size(15.0).strong());
                    ui.add_space(6.0);
                    ui.label(RichText::new(format!("\"{}\" is already in this folder.", ops_name(&src))).color(pal().dim));
                    ui.add_space(6.0);
                    let describe = |label: &str, path: &std::path::Path| {
                        let meta = std::fs::metadata(path).ok();
                        let size = meta.as_ref().filter(|m| m.is_file()).map(|m| format!("{}, ", crate::util::human_size(m.len()))).unwrap_or_default();
                        let time = meta.and_then(|m| m.modified().ok()).map(crate::util::fmt_time).unwrap_or_default();
                        RichText::new(format!("{label}  {size}{time}")).color(pal().text)
                    };
                    ui.label(describe("Existing", &target));
                    ui.label(describe("New     ", &src));
                    if c.pending.len() > 1 {
                        ui.add_space(6.0);
                        let left = c.pending.len() - c.current;
                        ui.checkbox(&mut c.apply_to_all, format!("Apply to all {left} remaining conflicts"));
                    }
                    ui.add_space(8.0);
                    ui.horizontal_wrapped(|ui| {
                        if ui.button("Replace").on_hover_text("The existing item goes to the trash").clicked() { resolved = Some(Resolution::Replace); }
                        if ui.button("Replace if newer").clicked() { resolved = Some(Resolution::ReplaceIfNewer); }
                        if ui.button("Skip").clicked() { resolved = Some(Resolution::Skip); }
                        if ui.button("Keep both").clicked() { resolved = Some(Resolution::KeepBoth); }
                        if ui.button("Cancel").clicked() { cancel = true; }
                    });
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
                        confirm |= crate::app::take(i, Modifiers::NONE, Key::Enter);
                        cancel |= crate::app::take(i, Modifiers::NONE, Key::Backspace);
                    });
                }
            }
        });
        if modal.should_close() {
            cancel = true;
        }
        if let (Some(how), Some(Dialog::Conflicts(_))) = (resolved, &self.dialog) {
            if let Some(Dialog::Conflicts(c)) = self.dialog.take() {
                self.resolve_conflict(c, how);
            }
            return;
        }
        if let Some(name) = load_workspace {
            self.dialog = None;
            self.load_workspace(&name);
            return;
        }
        if let Some(name) = delete_workspace {
            self.delete_workspace(&name);
        }
        if confirm {
            match self.dialog.take() {
                Some(Dialog::SaveWorkspace { name, .. }) => self.save_workspace(&name),
                Some(Dialog::Workspaces) => {}
                Some(Dialog::BatchRename(editor)) => {
                    if let Some(plan) = editor.plan.filter(|p| p.valid()) {
                        let pairs = plan.changes();
                        let mut job = crate::ops::Job::new(
                            crate::ops::JobKind::Rename,
                            pairs.iter().map(|(from, _)| from.clone()).collect(),
                            None,
                        );
                        job.pairs = pairs;
                        self.ops.submit(job);
                    }
                }
                Some(Dialog::Rename { path, name, .. }) => self.rename(&path, &name),
                Some(Dialog::Create {
                    dir, name, kind, ..
                }) => self.create(&dir, &name, kind),
                Some(Dialog::Compress {
                    srcs,
                    dest,
                    name,
                    format,
                    level,
                    ..
                }) => {
                    let name = name.trim();
                    if !name.is_empty() {
                        let mut job =
                            crate::ops::Job::new(crate::ops::JobKind::Compress, srcs, Some(dest));
                        job.name = Some(if name.ends_with(format.ext()) {
                            name.to_string()
                        } else {
                            format!("{name}{}", format.ext())
                        });
                        (job.format, job.level) = (format, level);
                        self.ops.submit(job);
                    }
                }
                Some(Dialog::ConfirmDelete { paths }) => self.ops.submit(crate::ops::Job::new(
                    crate::ops::JobKind::Delete,
                    paths,
                    None,
                )),
                Some(Dialog::Permissions(edit)) => {
                    let change = crate::props::Change {
                        mode: (edit.mode != edit.original_mode || edit.recursive)
                            .then_some(edit.mode),
                        owner: (edit.owner != edit.original_owner)
                            .then(|| edit.owner.trim().to_string()),
                        group: (edit.group != edit.original_group)
                            .then(|| edit.group.trim().to_string()),
                        recursive: edit.recursive,
                    };
                    if change.mode.is_some() || change.owner.is_some() || change.group.is_some() {
                        let mut job =
                            crate::ops::Job::new(crate::ops::JobKind::Properties, edit.paths, None);
                        job.change = Some(change);
                        self.ops.submit(job);
                    }
                    self.props_cache = None;
                }
                Some(Dialog::SelectPattern { pattern, .. }) => self.select_pattern(&pattern),
                Some(Dialog::ConfirmEmptyTrash) => self.ops.submit(crate::ops::Job::new(
                    crate::ops::JobKind::EmptyTrash,
                    Vec::new(),
                    None,
                )),
                Some(Dialog::Conflicts(_)) | None => {}
            }
        } else if cancel {
            self.dialog = None;
        }
    }

    pub(in crate::app) fn toasts_ui(&self, ctx: &egui::Context) {
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
                        .stroke(Stroke::new(
                            1.0,
                            if t.error { pal().danger } else { pal().border },
                        ))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::symmetric(12, 8))
                        .show(ui, |ui| {
                            ui.set_max_width(420.0);
                            ui.label(RichText::new(&t.text).color(if t.error {
                                pal().danger
                            } else {
                                pal().text
                            }));
                        });
                    ui.add_space(6.0);
                }
            });
    }
}
