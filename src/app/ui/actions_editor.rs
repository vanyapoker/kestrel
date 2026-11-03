//! Custom actions editor: a list of actions beside fields for the selected one,
//! with a live preview of the commands it would run for the current selection.

use crate::actions::{self, CustomAction, Document, PLACEHOLDERS};
use crate::theme::*;
use eframe::egui::{
    self, Align, Button, CornerRadius, FontId, Frame, Layout, Margin, RichText, ScrollArea, Sense,
    Stroke, TextEdit, Ui, text::CCursor, text::CCursorRange, vec2,
};
use std::path::PathBuf;

/// Commands shown in the preview before "…and N more".
const PREVIEW_COMMANDS: usize = 4;

pub(in crate::app) struct ActionsEditor {
    pub(in crate::app) doc: Document,
    /// Where the document is read from and saved to.
    pub(in crate::app) file: PathBuf,
    selected: usize,
    pub(in crate::app) dirty: bool,
    /// The selection and folder when the editor opened, for previews.
    targets: Vec<PathBuf>,
    dir: PathBuf,
    focus_name: bool,
    /// Esc or a click outside with unsaved changes asks before discarding them.
    pub(in crate::app) confirm_discard: bool,
}

/// What the editor asks the app to do after a frame.
pub(in crate::app) enum Outcome {
    Keep,
    Save,
    Close,
    /// Close and open the file in the default text editor.
    OpenFile,
}

impl ActionsEditor {
    pub(in crate::app) fn new(
        doc: Document,
        file: PathBuf,
        targets: Vec<PathBuf>,
        dir: PathBuf,
    ) -> Self {
        Self {
            doc,
            file,
            selected: 0,
            dirty: false,
            targets,
            dir,
            focus_name: false,
            confirm_discard: false,
        }
    }

    fn valid(&self) -> bool {
        self.doc.actions.iter().all(|a| a.errors().is_empty())
    }

    pub(in crate::app) fn show(&mut self, ui: &mut Ui) -> Outcome {
        let screen = ui.ctx().content_rect();
        ui.set_width(860.0_f32.min(screen.width() - 48.0));
        let mut outcome = Outcome::Keep;
        ui.horizontal(|ui| {
            ui.label(RichText::new("Custom actions").size(16.0).strong());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let exists = self.file.exists();
                let open = ui
                    .add_enabled(exists && !self.dirty, Button::new("Edit file directly"))
                    .on_disabled_hover_text(if self.dirty {
                        "Save or discard your changes first"
                    } else {
                        "Save an action first to create the file"
                    });
                if open.clicked() {
                    outcome = Outcome::OpenFile;
                }
                ui.label(
                    RichText::new(tilde(&self.file))
                        .size(12.0)
                        .color(pal().faint),
                );
            });
        });
        ui.label(
            RichText::new(
                "Shell commands for the selected files, in the command palette and the \
                 right-click menu. Kestrel quotes the paths it fills in.",
            )
            .color(pal().dim),
        );
        if !self.doc.problems.is_empty() {
            ui.add_space(6.0);
            problems(
                ui,
                "The file had problems; saving rewrites these lines:",
                &self.doc.problems,
            );
        }
        ui.add_space(8.0);

        let height = (screen.height() - 260.0).clamp(240.0, 460.0);
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(vec2(200.0, height), Layout::top_down(Align::Min), |ui| {
                self.list(ui, height)
            });
            // A fixed-height divider: a separator would take the row's height from
            // the last frame and grow the window a little every frame.
            let (line, _) = ui.allocate_exact_size(vec2(17.0, height), Sense::hover());
            ui.painter().vline(
                line.center().x,
                line.y_range(),
                Stroke::new(1.0, pal().border),
            );
            ui.allocate_ui_with_layout(
                vec2(ui.available_width(), height),
                Layout::top_down(Align::Min),
                |ui| {
                    ScrollArea::vertical()
                        .id_salt("action_fields")
                        .max_height(height)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            if self.doc.actions.is_empty() {
                                self.empty(ui);
                            } else {
                                self.fields(ui);
                            }
                        });
                },
            );
        });

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if self.confirm_discard {
                ui.label(RichText::new("Discard unsaved changes?").color(pal().danger));
                if ui.button("Discard").clicked() {
                    outcome = Outcome::Close;
                }
                if ui.button("Keep editing").clicked() {
                    self.confirm_discard = false;
                }
                return;
            }
            // Saving also rewrites lines the file had problems with.
            let changes = self.dirty || !self.doc.problems.is_empty();
            let save = ui
                .add_enabled(changes && self.valid(), Button::new("Save"))
                .on_disabled_hover_text(if changes {
                    "Fix the problems marked in red first"
                } else {
                    "Nothing changed yet"
                });
            if save.clicked() {
                outcome = Outcome::Save;
            }
            if ui
                .button(if self.dirty { "Cancel" } else { "Close" })
                .clicked()
            {
                outcome = if self.dirty {
                    self.confirm_discard = true;
                    Outcome::Keep
                } else {
                    Outcome::Close
                };
            }
            if self.dirty {
                ui.label(
                    RichText::new("Unsaved changes")
                        .size(12.0)
                        .color(pal().faint),
                );
            }
        });
        outcome
    }

    fn list(&mut self, ui: &mut Ui, height: f32) {
        ScrollArea::vertical()
            .id_salt("action_list")
            .max_height(height - 36.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for (i, a) in self.doc.actions.iter().enumerate() {
                    let name = if a.name.trim().is_empty() {
                        "(unnamed)"
                    } else {
                        a.name.trim()
                    };
                    let (rect, resp) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click());
                    let fill = if i == self.selected {
                        pal().select
                    } else if resp.hovered() {
                        pal().hover
                    } else {
                        egui::Color32::TRANSPARENT
                    };
                    ui.painter().rect_filled(rect, CornerRadius::same(5), fill);
                    let color = if a.errors().is_empty() {
                        pal().text
                    } else {
                        pal().danger
                    };
                    ui.painter().with_clip_rect(rect.shrink(4.0)).text(
                        rect.left_center() + vec2(8.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        name,
                        FontId::proportional(BODY),
                        color,
                    );
                    if resp.clicked() {
                        self.selected = i;
                    }
                }
            });
        ui.add_space(6.0);
        if ui.button("+ New action").clicked() {
            self.add(CustomAction {
                name: "New action".into(),
                ..Default::default()
            });
        }
    }

    fn empty(&mut self, ui: &mut Ui) {
        ui.add_space(24.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("No custom actions yet").size(15.0));
            ui.label(RichText::new("Start from a few examples, or add your own.").color(pal().dim));
            ui.add_space(10.0);
            if ui.button("Add examples").clicked() {
                for a in actions::examples() {
                    self.add(a);
                }
                self.selected = 0;
                self.focus_name = false;
            }
        });
    }

    fn add(&mut self, action: CustomAction) {
        self.doc.actions.push(action);
        self.selected = self.doc.actions.len() - 1;
        self.dirty = true;
        self.focus_name = true;
    }

    fn fields(&mut self, ui: &mut Ui) {
        self.selected = self.selected.min(self.doc.actions.len() - 1);
        let i = self.selected;
        let mut changed = false;
        let a = &mut self.doc.actions[i];

        label(ui, "Name");
        let name = ui.add(
            TextEdit::singleline(&mut a.name)
                .id_salt(("action_name", i))
                .desired_width(f32::INFINITY),
        );
        if std::mem::take(&mut self.focus_name) {
            name.request_focus();
        }
        changed |= name.changed();

        label(ui, "Only for names matching");
        changed |= ui
            .add(
                TextEdit::singleline(&mut a.patterns)
                    .id_salt(("action_match", i))
                    .hint_text("*.png, *.jpg   (empty: any item)")
                    .desired_width(f32::INFINITY),
            )
            .changed();

        label(ui, "Command");
        let run_id = ui.make_persistent_id(("action_run", i));
        changed |= ui
            .add(
                TextEdit::singleline(&mut a.run)
                    .id(run_id)
                    .font(FontId::monospace(13.0))
                    .hint_text("magick {file} {dir}/{stem}.webp")
                    .desired_width(f32::INFINITY),
            )
            .changed();
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Insert:").size(12.0).color(pal().faint));
            for (placeholder, what) in PLACEHOLDERS {
                let chip = ui
                    .add(
                        Button::new(
                            RichText::new(*placeholder)
                                .monospace()
                                .size(12.0)
                                .color(pal().accent),
                        )
                        .small(),
                    )
                    .on_hover_text(*what);
                if chip.clicked() {
                    insert_at_cursor(ui.ctx(), run_id, &mut a.run, placeholder);
                    changed = true;
                }
            }
        });
        ui.add_space(4.0);
        changed |= ui
            .checkbox(&mut a.terminal, "Run in a terminal window")
            .on_hover_text("For interactive programs such as editors; uses xdg-terminal-exec")
            .changed();
        ui.label(RichText::new(a.describe()).color(pal().dim));

        let (errors, warnings) = (a.errors(), a.warnings());
        if !errors.is_empty() {
            problems(ui, "", &errors);
        }
        if !warnings.is_empty() {
            for w in &warnings {
                ui.label(RichText::new(format!("⚠ {w}")).color(pal().dim));
            }
        }
        if errors.is_empty() {
            preview(ui, a, &self.targets, &self.dir);
        }

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let n = self.doc.actions.len();
            if ui.button("Duplicate").clicked() {
                let mut copy = self.doc.actions[i].clone();
                copy.name = format!("{} copy", copy.name.trim());
                copy.kept = Default::default();
                self.doc.actions.insert(i + 1, copy);
                self.selected = i + 1;
                self.focus_name = true;
                changed = true;
            }
            if ui.add_enabled(i > 0, Button::new("Move up")).clicked() {
                self.doc.actions.swap(i, i - 1);
                self.selected = i - 1;
                changed = true;
            }
            if ui
                .add_enabled(i + 1 < n, Button::new("Move down"))
                .clicked()
            {
                self.doc.actions.swap(i, i + 1);
                self.selected = i + 1;
                changed = true;
            }
            if ui
                .add(Button::new(RichText::new("Delete").color(pal().danger)))
                .clicked()
            {
                self.doc.actions.remove(i);
                self.selected = i.saturating_sub(1);
                changed = true;
            }
        });
        self.dirty |= changed;
    }
}

fn label(ui: &mut Ui, text: &str) {
    ui.add_space(6.0);
    ui.label(RichText::new(text).size(12.0).color(pal().faint));
}

fn problems(ui: &mut Ui, title: &str, lines: &[String]) {
    Frame::new()
        .fill(pal().danger.gamma_multiply(0.12))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            if !title.is_empty() {
                ui.label(RichText::new(title).color(pal().danger));
            }
            for line in lines {
                ui.label(RichText::new(line).color(pal().danger));
            }
        });
}

/// The commands the action would run now, with the paths filled in and quoted.
fn preview(ui: &mut Ui, a: &CustomAction, targets: &[PathBuf], dir: &std::path::Path) {
    label(ui, "Preview for the current selection");
    let text = if a.uses_selection() && targets.is_empty() {
        Err("Select items in the folder to preview this action.".to_string())
    } else if !a.applies(targets) {
        Err(format!(
            "Not offered for this selection: every name must match “{}”.",
            a.patterns.trim()
        ))
    } else {
        let commands = a.commands(targets, dir);
        let mut lines: Vec<String> = commands
            .iter()
            .take(PREVIEW_COMMANDS)
            .map(|(cmd, _)| cmd.clone())
            .collect();
        if commands.len() > PREVIEW_COMMANDS {
            lines.push(format!("…and {} more", commands.len() - PREVIEW_COMMANDS));
        }
        Ok(lines.join("\n"))
    };
    Frame::new()
        .fill(pal().surface)
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            match text {
                Ok(text) => ui.add(egui::Label::new(RichText::new(text).monospace()).wrap()),
                Err(text) => ui.label(RichText::new(text).color(pal().dim)),
            };
        });
}

/// Inserts `text` where the cursor was in the field, or at the end, and keeps
/// the cursor after it.
fn insert_at_cursor(ctx: &egui::Context, id: egui::Id, field: &mut String, text: &str) {
    let mut state = TextEdit::load_state(ctx, id).unwrap_or_default();
    let len = field.chars().count();
    let at = state
        .cursor
        .char_range()
        .map_or(len, |r| r.primary.index.0.min(len));
    let byte = field.char_indices().nth(at).map_or(field.len(), |(b, _)| b);
    field.insert_str(byte, text);
    let after = at + text.chars().count();
    state
        .cursor
        .set_char_range(Some(CCursorRange::one(CCursor::new(after))));
    state.store(ctx, id);
    ctx.memory_mut(|m| m.request_focus(id));
}

/// The path with the home folder shown as ~.
fn tilde(path: &std::path::Path) -> String {
    let home = crate::fsops::home_dir();
    match path.strip_prefix(&home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}
