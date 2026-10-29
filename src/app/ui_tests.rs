//! Drives the real UI headlessly with synthetic pointer input.

use super::*;
use eframe::egui::{Color32, Event, FontId, PointerButton, Pos2, RawInput, Rect, vec2};
use std::fs;
use std::time::{Duration, Instant};

struct Driver {
    ctx: egui::Context,
    app: App,
    time: f64,
    pointer: Pos2,
    modifiers: Modifiers,
    /// Text drawn in the last frame, with its on-screen rect.
    texts: Vec<(String, Rect)>,
    commands: Vec<egui::OutputCommand>,
    opened_urls: Vec<String>,
}

impl Driver {
    fn new(dir: PathBuf) -> Self {
        let ctx = egui::Context::default();
        let app = App::with_ctx(ctx.clone(), dir);
        Self {
            ctx,
            app,
            time: 0.0,
            pointer: Pos2::ZERO,
            modifiers: Modifiers::NONE,
            texts: Vec::new(),
            commands: Vec::new(),
            opened_urls: Vec::new(),
        }
    }

    fn frame(&mut self, mut events: Vec<Event>) {
        self.time += 1.0 / 60.0;
        events.insert(0, Event::ModifiersChanged(self.modifiers));
        let raw = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1400.0, 900.0))),
            time: Some(self.time),
            events,
            ..Default::default()
        };
        let app = &mut self.app;
        let mut out = self.ctx.run_ui(raw, |ui| {
            let ctx = ui.ctx().clone();
            app.poll(&ctx);
            app.handle_keys(&ctx);
            app.draw(ui);
        });
        out.textures_delta.clear(); // no renderer in tests
        self.commands = out.platform_output.commands;
        self.opened_urls
            .extend(self.commands.iter().filter_map(|c| match c {
                egui::OutputCommand::OpenUrl(url) => Some(url.url.clone()),
                _ => None,
            }));
        fn collect_text(shape: &egui::epaint::Shape, texts: &mut Vec<(String, Rect)>) {
            match shape {
                egui::epaint::Shape::Text(t) => {
                    texts.push((t.galley.text().to_string(), t.visual_bounding_rect()))
                }
                egui::epaint::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect_text(shape, texts);
                    }
                }
                _ => {}
            }
        }
        self.texts.clear();
        for shape in &out.shapes {
            collect_text(&shape.shape, &mut self.texts);
        }
    }

    /// Runs frames until `cond` holds (background work needs real time).
    fn until(&mut self, what: &str, mut cond: impl FnMut(&App) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !cond(&self.app) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
            self.frame(Vec::new());
        }
    }

    fn row(&self, path: &Path) -> Rect {
        *self
            .app
            .row_rects
            .get(path)
            .unwrap_or_else(|| panic!("no row for {}", path.display()))
    }

    fn move_to(&mut self, pos: Pos2) {
        self.pointer = pos;
        self.frame(vec![Event::PointerMoved(pos)]);
    }

    fn click_text(&mut self, label: &str) {
        let rect = self
            .texts
            .iter()
            .find(|(text, _)| text == label)
            .unwrap_or_else(|| panic!("no visible text {label:?}"))
            .1;
        self.move_to(rect.center());
        self.button(true);
        self.button(false);
    }

    fn button(&mut self, pressed: bool) {
        self.mouse_button(PointerButton::Primary, pressed);
    }

    fn mouse_button(&mut self, button: PointerButton, pressed: bool) {
        let ev = Event::PointerButton {
            pos: self.pointer,
            button,
            pressed,
            modifiers: self.modifiers,
        };
        self.frame(vec![ev]);
    }

    /// Press on `from`, move in small steps to `to` (holding), release.
    fn drag(&mut self, from: Pos2, to: Pos2, mut while_held: impl FnMut(&mut Self)) {
        self.move_to(from);
        self.button(true);
        for i in 1..=12 {
            self.move_to(from + (to - from) * (i as f32 / 12.0));
        }
        while_held(self);
        self.button(false);
        self.frame(Vec::new());
    }
}

fn sandbox(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("kestrel-ui-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(d.join("dest")).unwrap();
    fs::write(d.join("a.txt"), "a").unwrap();
    fs::write(d.join("b.txt"), "b").unwrap();
    d
}

fn press(key: Key, modifiers: Modifiers) -> Event {
    Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}

fn columns_driver(name: &str) -> (PathBuf, Driver) {
    let dir = sandbox(name);
    fs::create_dir_all(dir.join("dest/nested/deep")).unwrap();
    fs::write(dir.join("dest/nested/note.txt"), "column preview").unwrap();
    let mut d = Driver::new(dir.clone());
    d.app.show_sidebar = false;
    d.app.show_preview = false;
    d.until("listing", |a| !a.tab().loading);
    d.app.set_view(ViewMode::Columns);
    d.until("parent and child columns", |a| {
        a.tab()
            .columns
            .as_ref()
            .is_some_and(|c| c.items.len() == 3 && !c.items[0].loading && !c.items[2].loading)
    });
    d.frame(Vec::new());
    (dir, d)
}

#[test]
fn icon_documents_show_contents_at_every_size_and_refresh_after_file_changes() {
    let dir = sandbox("text-icons");
    let path = dir.join("README.md");
    fs::write(&path, "# ICON_CONTENT_MARKER\n\nA small **document**.\n").unwrap();
    let mut d = Driver::new(dir.clone());
    d.app.show_preview = false;
    d.app.show_sidebar = false;
    d.until("listing", |a| !a.tab().loading);
    for i in 0..crate::tab::ICON_SIZES.len() {
        d.app.set_view(ViewMode::Icons(i));
        d.frame(Vec::new());
        let before = d.row(&path);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !d
            .texts
            .iter()
            .any(|(text, _)| text.contains("ICON_CONTENT_MARKER"))
        {
            assert!(Instant::now() < deadline, "missing text at icon size {i}");
            std::thread::sleep(Duration::from_millis(5));
            d.frame(Vec::new());
        }
        assert_eq!(d.row(&path), before, "content must not resize the tile");
    }
    d.app.tab_mut().select_path(&path);
    d.app.tab_mut().scroll_to_cursor = true;
    d.frame(Vec::new());
    assert_eq!(d.app.tab().targets(), vec![path.clone()]);
    d.modifiers = Modifiers::COMMAND;
    d.frame(vec![press(Key::Space, Modifiers::COMMAND)]);
    assert!(d.app.tab().marked.contains(&path));
    assert!(!d.app.show_preview);
    d.modifiers = Modifiers::NONE;
    fs::write(&path, "# UPDATED_ICON_CONTENT\n").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !d
        .texts
        .iter()
        .any(|(text, _)| text.contains("UPDATED_ICON_CONTENT"))
    {
        assert!(Instant::now() < deadline, "thumbnail did not refresh");
        std::thread::sleep(Duration::from_millis(5));
        d.frame(Vec::new());
    }
    // Compact lists and columns keep their established small file icons.
    for mode in [ViewMode::Details, ViewMode::List, ViewMode::Columns] {
        d.app.set_view(mode);
        d.frame(Vec::new());
        assert!(
            !d.texts
                .iter()
                .any(|(text, _)| text.contains("UPDATED_ICON_CONTENT"))
        );
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn icon_document_text_is_clipped_inside_its_page() {
    let ctx = egui::Context::default();
    let sample = egui::text::LayoutJob::simple(
        "long content ".repeat(160),
        FontId::monospace(12.5),
        Color32::WHITE,
        f32::INFINITY,
    );
    for side in crate::tab::ICON_SIZES {
        let area = Rect::from_min_size(egui::pos2(20.0, 20.0), vec2(side, side));
        let mut output = ctx.run_ui(
            RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(400.0, 400.0))),
                ..Default::default()
            },
            |ui| {
                super::ui::paint_text_thumbnail(ui.painter(), area, &sample, 1.0);
            },
        );
        output.textures_delta.clear();
        let text = output
            .shapes
            .iter()
            .find(|s| matches!(s.shape, egui::epaint::Shape::Text(_)))
            .unwrap();
        assert!(area.contains_rect(text.clip_rect));
        assert!(text.clip_rect.width() < side * 0.78);
        assert!(text.clip_rect.height() < side);
    }
}

#[test]
fn miller_columns_reveal_focus_and_keep_operations_in_the_active_directory() {
    let (dir, mut d) = columns_driver("columns-focus");
    let c = d.app.tab().columns.as_ref().unwrap();
    assert_eq!(c.active, 1);
    assert_eq!(c.items[0].cursor_entry().unwrap().path, dir);
    assert_eq!(c.items[2].path, dir.join("dest"));
    assert_eq!(d.app.tab().path, dir);
    assert!(d.app.tab().back.is_empty());
    let id = d.app.tab().id;
    d.frame(vec![press(Key::ArrowRight, Modifiers::NONE)]);
    assert_eq!(d.app.tab().path, dir.join("dest"));
    assert_eq!(d.app.tab().id, id);
    d.until("nested column", |a| {
        a.tab()
            .columns
            .as_ref()
            .is_some_and(|c| c.items.len() == 4 && !c.items[3].loading)
    });
    assert_eq!(d.app.tab().targets(), vec![dir.join("dest/nested")]);
    d.frame(vec![press(Key::ArrowRight, Modifiers::NONE)]);
    assert_eq!(d.app.tab().path, dir.join("dest/nested"));
    d.until("deep column", |a| {
        a.tab()
            .columns
            .as_ref()
            .is_some_and(|c| c.items.len() == 5 && !c.items[4].loading)
    });
    d.frame(vec![press(Key::ArrowDown, Modifiers::NONE)]);
    d.frame(Vec::new());
    assert_eq!(d.app.tab().cursor_entry().unwrap().name, "note.txt");
    assert_eq!(d.app.tab().columns.as_ref().unwrap().items.len(), 4);
    d.app.run_cmd(Cmd::Copy);
    assert_eq!(
        d.app.clip.as_ref().unwrap().paths,
        vec![dir.join("dest/nested/note.txt")]
    );
    d.frame(vec![
        press(Key::Space, Modifiers::NONE),
        Event::Text(" ".into()),
    ]);
    assert!(d.app.show_preview);
    assert!(d.app.tab().filter.is_empty());
    d.until("file preview", |a| {
        matches!(
            a.preview.state,
            crate::preview::State::Ready(crate::preview::Content::Text { .. })
        )
    });
    d.frame(vec![press(Key::ArrowLeft, Modifiers::NONE)]);
    assert_eq!(d.app.tab().path, dir.join("dest"));
    assert_eq!(
        d.app.tab().cursor_entry().unwrap().path,
        dir.join("dest/nested")
    );
    assert_eq!(d.app.tab().targets(), vec![dir.join("dest/nested")]);
    // Alt+Left is history, not column movement.
    d.modifiers = Modifiers::ALT;
    d.frame(vec![press(Key::ArrowLeft, Modifiers::ALT)]);
    assert_eq!(d.app.tab().path, dir.join("dest/nested"));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn miller_columns_shortcuts_respect_modifiers_text_fields_search_and_zoom() {
    let (dir, mut d) = columns_driver("columns-keys");
    let path = d.app.tab().path.clone();
    d.modifiers = Modifiers::SHIFT;
    d.frame(vec![press(Key::ArrowRight, Modifiers::SHIFT)]);
    assert_eq!(d.app.tab().path, path);
    d.frame(vec![press(Key::ArrowDown, Modifiers::SHIFT)]);
    assert_eq!(d.app.tab().selection_count(), 2);
    d.modifiers = Modifiers::NONE;
    d.app.zoom(2);
    assert_eq!(d.app.tab().mode, ViewMode::Columns);
    d.frame(vec![Event::Text("b".into())]);
    assert_eq!(d.app.tab().targets(), vec![dir.join("b.txt")]);
    assert_eq!(d.app.tab().columns.as_ref().unwrap().items.len(), 2);
    d.frame(vec![press(Key::Escape, Modifiers::NONE)]);
    d.app.run_cmd(Cmd::EditPath);
    d.frame(Vec::new());
    d.frame(vec![press(Key::ArrowLeft, Modifiers::NONE)]);
    assert_eq!(d.app.tab().path, path);
    d.frame(vec![press(Key::Escape, Modifiers::NONE)]);
    d.app.run_cmd(Cmd::Search);
    d.frame(Vec::new());
    d.frame(vec![Event::Text("note".into())]);
    d.frame(vec![press(Key::ArrowRight, Modifiers::NONE)]);
    assert_eq!(d.app.tab().path, path);
    assert!(d.app.tab().columns.is_none());
    assert_eq!(d.app.tab().mode, ViewMode::Columns);
    d.frame(vec![press(Key::Escape, Modifiers::NONE)]);
    d.until("columns restored", |a| {
        a.tab().search.is_none() && a.tab().columns.is_some()
    });
    d.app.run_cmd(Cmd::Flatten);
    d.frame(Vec::new());
    assert!(d.app.tab().search.as_ref().unwrap().flatten);
    assert!(d.app.tab().columns.is_none());
    assert_eq!(d.app.tab().mode, ViewMode::Columns);
    d.app.run_cmd(Cmd::Flatten);
    d.until("columns after flatten", |a| {
        a.tab().search.is_none() && a.tab().columns.is_some()
    });
    d.modifiers = Modifiers::COMMAND | Modifiers::SHIFT;
    d.frame(vec![press(Key::M, d.modifiers)]);
    assert_eq!(d.app.tab().mode, ViewMode::Details);
    d.frame(vec![press(Key::M, d.modifiers)]);
    assert_eq!(d.app.tab().mode, ViewMode::Columns);
    d.modifiers = Modifiers::COMMAND;
    d.frame(vec![press(Key::G, d.modifiers)]);
    assert_eq!(d.app.tab().mode, ViewMode::Icons(2));
    assert!(d.app.tab().columns.is_none());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn miller_columns_restore_modes_and_navigate_ancestors_and_empty_folders() {
    let (dir, mut d) = columns_driver("columns-restore");
    let saved = d.app.session();
    let roundtrip = crate::session::parse(&saved.to_text());
    let mut restored = Driver::new(dir.clone());
    restored.app.restore(&roundtrip, true);
    restored.until("restored columns", |a| {
        a.tab().mode == ViewMode::Columns && a.tab().columns.is_some()
    });
    d.frame(vec![press(Key::ArrowRight, Modifiers::NONE)]);
    d.until("child loaded", |a| !a.tab().loading);
    d.frame(vec![press(Key::Enter, Modifiers::NONE)]);
    d.until("nested loaded", |a| {
        a.tab().path == dir.join("dest/nested") && !a.tab().loading
    });
    d.frame(vec![press(Key::Enter, Modifiers::NONE)]);
    d.until("empty column", |a| {
        a.tab().path == dir.join("dest/nested/deep") && !a.tab().loading
    });
    assert!(d.app.tab().view.is_empty());
    d.frame(vec![press(Key::ArrowRight, Modifiers::NONE)]);
    assert_eq!(d.app.tab().path, dir.join("dest/nested/deep"));
    for _ in 0..5 {
        d.frame(vec![press(Key::ArrowLeft, Modifiers::NONE)]);
    }
    assert_eq!(d.app.tab().path, dir.parent().unwrap().parent().unwrap());
    d.app.navigate(PathBuf::from("/"), History::Push);
    d.until("root", |a| !a.tab().loading);
    d.frame(vec![press(Key::ArrowLeft, Modifiers::NONE)]);
    assert_eq!(d.app.tab().path, PathBuf::from("/"));
    assert_eq!(d.app.tab().columns.as_ref().unwrap().active, 0);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn miller_columns_pointer_selection_context_actions_and_drop_use_the_correct_column() {
    let (dir, mut d) = columns_driver("columns-pointer");
    // Click an item in a revealed but inactive child column.
    let row = d.row(&dir.join("dest/nested"));
    assert!(row.width() > 100.0);
    d.move_to(row.center());
    d.button(true);
    d.button(false);
    assert_eq!(d.app.tab().path, dir.join("dest"));
    d.until("revealed nested folder", |a| {
        a.tab()
            .columns
            .as_ref()
            .is_some_and(|c| c.items.len() == 4 && !c.items[3].loading)
    });
    d.frame(Vec::new());
    let row = d.row(&dir.join("dest/nested/note.txt"));
    d.move_to(row.center());
    d.button(true);
    d.button(false);
    assert_eq!(d.app.tab().path, dir.join("dest/nested"));
    assert_eq!(
        d.app.tab().targets(),
        vec![dir.join("dest/nested/note.txt")]
    );
    // The context menu and command target the newly focused column too.
    d.mouse_button(PointerButton::Secondary, true);
    d.mouse_button(PointerButton::Secondary, false);
    d.frame(Vec::new());
    d.click_text("Duplicate");
    d.until("duplicated file", |_| {
        dir.join("dest/nested/note (2).txt").exists()
    });
    d.app.column_step(-1);
    d.app.column_step(-1);
    d.frame(Vec::new());
    let source = d.row(&dir.join("a.txt")).center();
    let destination = Pos2::new(d.row(&dir.join("dest/nested")).center().x, 700.0);
    d.drag(source, destination, |_| {});
    d.until("drop into child column", |_| {
        dir.join("dest/a.txt").exists()
    });
    assert!(!dir.join("a.txt").exists());
    assert_eq!(d.app.tab().path, dir.join("dest"));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn miller_columns_watch_passive_directories_and_reject_stale_or_pruned_results() {
    let (dir, mut d) = columns_driver("columns-watch");
    let (id, generation) = {
        let child = &d.app.tab().columns.as_ref().unwrap().items[2];
        (child.listing_id, child.generation)
    };
    // Listing identity follows its directory when it becomes active.
    d.app.column_step(1);
    d.app
        .list_tx
        .send(Listing {
            listing_id: id,
            generation: generation.saturating_sub(1),
            soft: true,
            result: Err("stale result".into()),
        })
        .unwrap();
    d.frame(Vec::new());
    assert!(d.app.tab().error.is_none());
    assert_eq!(d.app.tab().path, dir.join("dest"));
    d.app.column_step(-1);
    d.frame(Vec::new());
    fs::write(dir.join("dest/new.txt"), "watcher update").unwrap();
    d.until("passive directory watcher", |a| {
        a.tab().columns.as_ref().is_some_and(|c| {
            c.items
                .get(2)
                .is_some_and(|t| t.entries.iter().any(|e| e.name == "new.txt"))
        })
    });
    // Select a file to prune the child, then deliver an old result for that child.
    d.app.tab_mut().select_path(&dir.join("a.txt"));
    d.frame(Vec::new());
    assert_eq!(d.app.tab().columns.as_ref().unwrap().items.len(), 2);
    d.app
        .list_tx
        .send(Listing {
            listing_id: id,
            generation,
            soft: false,
            result: Err("pruned result".into()),
        })
        .unwrap();
    d.frame(Vec::new());
    assert!(d.app.tab().error.is_none());
    d.app.tab_mut().select_path(&dir.join("dest"));
    d.frame(Vec::new());
    d.until("child restored", |a| {
        a.tab()
            .columns
            .as_ref()
            .is_some_and(|c| c.items.len() == 3 && !c.items[2].loading)
    });
    fs::remove_dir_all(dir.join("dest")).unwrap();
    d.until("removed branch", |a| {
        !a.tab().loading
            && a.tab().columns.as_ref().is_some_and(|c| {
                c.items.len() == 2 && a.tab().entries.iter().all(|e| e.name != "dest")
            })
    });
    assert_eq!(d.app.tab().path, dir);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn miller_columns_filters_scroll_and_split_state_survive_focus_changes() {
    let (dir, mut d) = columns_driver("columns-state");
    d.app.column_step(1);
    d.until("nested revealed", |a| {
        a.tab()
            .columns
            .as_ref()
            .is_some_and(|c| c.items.len() == 4 && !c.items[3].loading)
    });
    d.app.column_step(1);
    d.frame(vec![Event::Text("note".into())]);
    assert_eq!(d.app.tab().filter, "note");
    d.app.column_step(-1);
    assert!(d.app.tab().filter.is_empty());
    d.app.column_step(1);
    assert_eq!(d.app.tab().filter, "note");
    d.app.toggle_split();
    let other = d.app.tab().path.clone();
    d.frame(vec![press(Key::Tab, Modifiers::NONE)]);
    assert_eq!(d.app.active_pane, 0);
    assert_eq!(d.app.tab().mode, ViewMode::Columns);
    assert_eq!(d.app.tab().filter, "note");
    d.frame(vec![press(Key::ArrowLeft, Modifiers::NONE)]);
    assert_eq!(d.app.tab().path, dir.join("dest"));
    assert_eq!(d.app.panes[1].tabs[0].path, other);
    // Restore the columns mode through workspace and closed-tab state.
    d.app.save_workspace("Columns");
    d.app.open_tab(dir.clone());
    d.app.close_tab(0, 0);
    d.app.run_cmd(Cmd::ReopenTab);
    assert_eq!(d.app.tab().mode, ViewMode::Columns);
    d.frame(Vec::new());
    d.app.load_workspace("Columns");
    assert_eq!(d.app.panes[0].tabs[0].mode, ViewMode::Columns);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn miller_columns_scroll_active_columns_into_view_and_preserve_vertical_offsets() {
    let (dir, mut d) = columns_driver("columns-scroll");
    for i in 0..100 {
        fs::write(dir.join(format!("dest/nested/file{i:03}.txt")), "x").unwrap();
    }
    d.app.column_step(1);
    d.until("nested listing", |a| {
        a.tab()
            .columns
            .as_ref()
            .is_some_and(|c| c.items.len() == 4 && !c.items[3].loading)
    });
    d.app.column_step(1);
    d.app.refresh();
    d.until("100 files", |a| {
        !a.tab().loading && a.tab().entries.len() == 102
    });
    d.frame(vec![press(Key::End, Modifiers::NONE)]);
    let offset = d.app.tab().scroll_offset;
    assert!(offset > 1000.0, "offset {offset}");
    let target = d.app.tab().cursor_entry().unwrap().path.clone();
    d.app.column_step(-1);
    d.frame(Vec::new());
    d.app.column_step(1);
    d.frame(Vec::new());
    assert_eq!(d.app.tab().cursor_entry().unwrap().path, target);
    assert!((d.app.tab().scroll_offset - offset).abs() < 1.0);
    for _ in 0..3 {
        d.frame(Vec::new());
    }
    let row = d.row(&target);
    assert!(row.width() > 100.0);
    assert!(d.app.pane_rects[0].contains(row.center()));
    // A deep branch requires horizontal scrolling; its active row stays visible.
    d.app.navigate(dir.join("dest/nested/deep"), History::Push);
    let mut current = dir.join("dest/nested/deep");
    for _ in 0..6 {
        current = current.join("child");
        fs::create_dir(&current).unwrap();
    }
    d.app.refresh();
    for _ in 0..6 {
        d.until("active directory", |a| {
            !a.tab().loading && !a.tab().view.is_empty()
        });
        d.frame(vec![press(Key::ArrowRight, Modifiers::NONE)]);
    }
    d.until("deep directory", |a| !a.tab().loading);
    assert_eq!(d.app.tab().path, current);
    // Walk back to a populated column and inspect its real clipped row geometry.
    d.frame(vec![press(Key::ArrowLeft, Modifiers::NONE)]);
    d.frame(Vec::new());
    let row = d.row(&current);
    assert!(row.width() > 100.0, "clipped active row {row:?}");
    assert!(d.app.pane_rects[0].contains(row.center()));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn miller_columns_error_recovery_refresh_and_deleted_active_ancestors() {
    let (dir, mut d) = columns_driver("columns-errors");
    let (listing_id, generation) = {
        let child = &d.app.tab().columns.as_ref().unwrap().items[2];
        (child.listing_id, child.generation)
    };
    d.app
        .list_tx
        .send(Listing {
            listing_id,
            generation,
            soft: false,
            result: Err("Permission denied".into()),
        })
        .unwrap();
    d.frame(Vec::new());
    assert!(
        d.app.tab().columns.as_ref().unwrap().items[2]
            .error
            .as_ref()
            .unwrap()
            .contains("Permission denied")
    );
    assert!(d.texts.iter().any(|(text, _)| text == "Permission denied"));
    assert_eq!(d.app.tab().path, dir);
    d.frame(vec![press(Key::F5, Modifiers::NONE)]);
    d.until("retry passive column", |a| {
        a.tab().columns.as_ref().is_some_and(|c| {
            c.items.len() == 3 && !c.items[2].loading && c.items[2].error.is_none()
        })
    });
    d.app.column_step(1);
    d.until("nested", |a| {
        a.tab()
            .columns
            .as_ref()
            .is_some_and(|c| c.items.len() == 4 && !c.items[3].loading)
    });
    d.app.column_step(1);
    fs::remove_dir_all(dir.join("dest")).unwrap();
    d.until("nearest surviving directory", |a| {
        a.tab().path == dir && !a.tab().loading
    });
    assert!(
        d.app
            .tab()
            .columns
            .as_ref()
            .is_some_and(|c| c.items[c.active].path == dir)
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn tab_drag_crosses_panes_and_collapses_an_empty_source_pane() {
    let dir = sandbox("tab-cross-drag");
    for name in ["one", "two"] {
        fs::create_dir(dir.join(name)).unwrap();
    }
    let mut d = Driver::new(dir.clone());
    d.app.show_preview = false;
    d.app.show_sidebar = false;
    d.app.toggle_split();
    d.app.navigate(dir.join("two"), History::Push);
    d.app.active_pane = 0;
    d.app.open_tab(dir.join("one"));
    d.frame(Vec::new());
    d.frame(Vec::new());
    let at = |d: &Driver, name: &str| {
        d.texts
            .iter()
            .find(|(t, r)| t == name && r.top() < 40.0)
            .unwrap()
            .1
    };
    let from = at(&d, "one").center();
    let to = at(&d, "two").center() + vec2(25.0, 0.0);
    d.drag(from, to, |_| {});
    assert_eq!(d.app.panes[0].tabs.len(), 1);
    assert_eq!(d.app.panes[1].tabs.len(), 2);
    assert_eq!(d.app.active_pane, 1);
    assert_eq!(d.app.tab().path, dir.join("one"));
    let name = dir.file_name().unwrap().to_str().unwrap();
    let rect = at(&d, name);
    let from = egui::pos2(rect.left() + 20.0, rect.center().y);
    let to = at(&d, "two").left_center() - vec2(10.0, 0.0);
    d.drag(from, to, |_| {});
    assert_eq!(d.app.panes.len(), 1);
    assert_eq!(d.app.panes[0].tabs.len(), 3);
    assert_eq!(d.app.tab().path, dir);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn file_drop_on_a_tab_still_moves_files_with_draggable_tabs() {
    let dir = sandbox("file-to-tab");
    let mut d = Driver::new(dir.clone());
    d.app.open_tab(dir.join("dest"));
    d.app.panes[0].active = 0;
    d.until("listing", |a| a.row_rects.contains_key(&dir.join("a.txt")));
    let from = d.row(&dir.join("a.txt")).center();
    let to = d
        .texts
        .iter()
        .find(|(t, r)| t == "dest" && r.top() < 40.0 && r.center().x < d.app.pane_rects[0].right())
        .unwrap()
        .1
        .center();
    d.drag(from, to, |d| {
        assert!(
            egui::DragAndDrop::has_payload_of_type::<DragPaths>(&d.ctx),
            "file drag payload disappeared before release"
        );
    });
    d.until("file moved to tab", |_| dir.join("dest/a.txt").exists());
    assert!(!dir.join("a.txt").exists());
    assert_eq!(d.app.panes[0].tabs.len(), 2);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn tabs_keep_full_names_when_they_fit_and_reserve_close_button_space() {
    let dir = sandbox("tab-label-space");
    let long = "quarterly-planning-and-delivery-notes";
    for name in ["diagnostics", long] {
        fs::create_dir(dir.join(name)).unwrap();
    }
    let mut d = Driver::new(dir.clone());
    d.app.show_preview = false;
    d.app.show_sidebar = false;
    d.app.open_tab(dir.join("diagnostics"));
    d.app.open_tab(dir.join(long));
    for _ in 0..3 {
        d.frame(Vec::new());
    }
    for tab in &d.app.panes[0].tabs {
        let title = tab.title();
        let (body, close) = d.app.tab_rects[&tab.id];
        let label = d
            .texts
            .iter()
            .find(|(text, r)| text == &title && r.top() < 40.0 && body.contains(r.center()))
            .unwrap()
            .1;
        let natural = d
            .ctx
            .fonts_mut(|f| f.layout_no_wrap(title, FontId::proportional(13.0), Color32::WHITE))
            .size()
            .x;
        assert!(
            (label.width() - natural).abs() < 3.0,
            "full label should fit: {label:?}, {natural}"
        );
        assert!(
            label.right() <= close.left() - 4.0,
            "label overlaps close button"
        );
        if tab.path.ends_with(long) {
            assert!(
                body.width() > 230.0,
                "roomy tabs should not use the old width cap"
            );
        }
    }
    // Close still targets the button, without activating or dragging a different tab.
    let first = d.app.panes[0].tabs[1].id;
    let close = d.app.tab_rects[&first].1.center();
    d.move_to(close);
    d.button(true);
    d.button(false);
    assert!(d.app.panes[0].tabs.iter().all(|t| t.id != first));
    assert_eq!(d.app.tab().path, dir.join(long));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn crowded_tabs_elide_names_without_painting_under_close_buttons() {
    let dir = sandbox("crowded-tabs");
    let mut d = Driver::new(dir.clone());
    d.app.show_preview = false;
    d.app.show_sidebar = false;
    for i in 0..18 {
        let path = dir.join(format!("diagnostics-results-{i:02}"));
        fs::create_dir(&path).unwrap();
        d.app.open_tab(path);
    }
    for _ in 0..20 {
        d.frame(Vec::new());
    }
    for tab in d.app.panes[0].tabs.iter().skip(1) {
        let (body, close) = d.app.tab_rects[&tab.id];
        let title = tab.title();
        let label = d
            .texts
            .iter()
            .find(|(text, r)| text == &title && r.top() < 40.0)
            .unwrap()
            .1;
        let natural = d
            .ctx
            .fonts_mut(|f| f.layout_no_wrap(title, FontId::proportional(13.0), Color32::WHITE))
            .size()
            .x;
        assert!(body.width() >= 100.0);
        assert!(
            label.width() < natural - 10.0,
            "crowded labels should be elided"
        );
        assert!(label.right() <= close.left() - 4.0);
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn plain_mouse_wheel_scrolls_the_hovered_tab_strip_without_switching_tabs_or_zooming() {
    let dir = sandbox("tab-wheel");
    let mut d = Driver::new(dir.clone());
    d.app.show_preview = false;
    d.app.show_sidebar = false;
    for i in 0..25 {
        let path = dir.join(format!("folder-{i:02}"));
        fs::create_dir(&path).unwrap();
        d.app.open_tab(path);
    }
    d.app.panes[0].active = 0;
    for _ in 0..20 {
        d.frame(Vec::new());
    }
    let active = d.app.tab().id;
    let mode = d.app.tab().mode;
    let location = |d: &Driver| d.app.tab_rects[&active].0.left();
    let start = location(&d);
    d.move_to(egui::pos2(650.0, 20.0));
    let wheel = |delta, unit| Event::MouseWheel {
        unit,
        delta,
        modifiers: Modifiers::NONE,
        phase: egui::TouchPhase::Move,
    };
    d.frame(vec![wheel(vec2(0.0, -300.0), egui::MouseWheelUnit::Point)]);
    for _ in 0..20 {
        d.frame(Vec::new());
    }
    let after_vertical = location(&d);
    assert!(
        after_vertical < start - 100.0,
        "vertical wheel did not move tabs: {start} -> {after_vertical}"
    );
    d.frame(vec![wheel(vec2(-200.0, 0.0), egui::MouseWheelUnit::Point)]);
    for _ in 0..20 {
        d.frame(Vec::new());
    }
    assert!(
        location(&d) < after_vertical - 100.0,
        "horizontal wheel stopped working"
    );
    d.frame(vec![wheel(vec2(0.0, 4.0), egui::MouseWheelUnit::Line)]);
    for _ in 0..20 {
        d.frame(Vec::new());
    }
    assert!(
        location(&d) > after_vertical - 150.0,
        "reverse wheel did not scroll back"
    );
    assert_eq!(d.app.tab().id, active);
    assert_eq!(d.app.tab().mode, mode);
    // Wheel input over the file pane must not keep moving the tab strip.
    let before_files = location(&d);
    d.move_to(egui::pos2(650.0, 450.0));
    d.frame(vec![wheel(vec2(0.0, -300.0), egui::MouseWheelUnit::Point)]);
    for _ in 0..20 {
        d.frame(Vec::new());
    }
    assert!((location(&d) - before_files).abs() < 1.0);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn long_tab_strips_scroll_to_the_active_tab() {
    let dir = sandbox("tab-scroll");
    for i in 0..20 {
        fs::create_dir(dir.join(format!("folder-{i:02}"))).unwrap();
    }
    let mut d = Driver::new(dir.clone());
    d.app.show_preview = false;
    d.app.show_sidebar = false;
    for i in 0..20 {
        d.app.open_tab(dir.join(format!("folder-{i:02}")));
    }
    for _ in 0..20 {
        d.frame(Vec::new());
    }
    let rect = d
        .texts
        .iter()
        .find(|(t, r)| t == "folder-19" && r.top() < 40.0)
        .unwrap()
        .1;
    assert!(rect.left() >= d.app.pane_rects[0].left());
    assert!(rect.right() <= d.app.pane_rects[0].right());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn dragging_tabs_reorders_them_without_file_operations() {
    let dir = sandbox("tab-drag");
    for name in ["one", "two", "three"] {
        fs::create_dir(dir.join(name)).unwrap();
    }
    let mut d = Driver::new(dir.clone());
    d.app.show_preview = false;
    d.app.show_sidebar = false;
    for name in ["one", "two", "three"] {
        d.app.open_tab(dir.join(name));
    }
    d.frame(Vec::new());
    d.frame(Vec::new());
    let at = |d: &Driver, name: &str| {
        d.texts
            .iter()
            .find(|(t, r)| t == name && r.top() < 40.0)
            .unwrap()
            .1
            .center()
    };
    let from = at(&d, "one");
    let to = at(&d, "three") + vec2(25.0, 0.0);
    d.drag(from, to, |d| {
        assert!(egui::DragAndDrop::has_payload_of_type::<TabDrag>(&d.ctx))
    });
    let names: Vec<_> = d.app.panes[0]
        .tabs
        .iter()
        .map(|t| t.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(&names[1..], ["two", "three", "one"]);
    assert_eq!(d.app.tab().path, dir.join("one"));
    assert_eq!(d.app.ops.queued.load(Ordering::Relaxed), 0);
    assert!(dir.join("one").is_dir() && dir.join("three").is_dir());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn moving_tabs_between_panes_keeps_live_tab_state_and_removes_empty_panes() {
    let dir = sandbox("tab-cross-pane");
    let mut d = Driver::new(dir.clone());
    d.app.tab_mut().mode = ViewMode::Icons(2);
    let id = d.app.tab().id;
    d.app.toggle_split();
    d.app.move_tab(id, 1, None);
    assert_eq!(d.app.panes.len(), 1);
    assert_eq!(d.app.panes[0].tabs.len(), 2);
    assert_eq!(d.app.active_pane, 0);
    assert_eq!(d.app.tab().id, id);
    assert_eq!(d.app.tab().mode, ViewMode::Icons(2));
    assert!(
        d.app.closed_tabs.is_empty(),
        "moving a tab must not create a closed tab"
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn closed_tabs_and_splits_reopen_with_their_views() {
    let dir = sandbox("reopen-tabs");
    let mut d = Driver::new(dir.clone());
    d.app.open_tab(dir.join("dest"));
    d.app.tab_mut().mode = ViewMode::Icons(2);
    d.app.tab_mut().sort = SortKey::Size;
    d.app.close_tab(0, 1);
    assert!(
        d.app
            .goto_items()
            .iter()
            .any(|(_, detail, action)| detail.starts_with("Closed tab")
                && matches!(action, Action::ReopenClosed(_)))
    );
    d.modifiers = Modifiers::COMMAND | Modifiers::SHIFT;
    d.frame(vec![Event::Key {
        key: Key::T,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: d.modifiers,
    }]);
    assert_eq!(d.app.tab().path, dir.join("dest"));
    assert_eq!(d.app.tab().mode, ViewMode::Icons(2));
    assert_eq!(d.app.tab().sort, SortKey::Size);
    d.app.toggle_split();
    d.app.close_split(0);
    assert_eq!(d.app.panes.len(), 1);
    d.app.run_cmd(Cmd::ReopenTab);
    assert_eq!(d.app.panes.len(), 2);
    assert_eq!(d.app.panes[0].tabs.len(), 2);
    assert_eq!(d.app.panes[0].tabs[1].mode, ViewMode::Icons(2));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn split_divider_resizes_and_restores_through_session() {
    let dir = sandbox("split-divider");
    let mut d = Driver::new(dir.clone());
    d.app.show_preview = false;
    d.app.show_sidebar = false;
    d.app.toggle_split();
    d.frame(Vec::new());
    let mid = (d.app.pane_rects[0].right() + d.app.pane_rects[1].left()) / 2.0;
    d.drag(
        egui::pos2(mid, 450.0),
        egui::pos2(mid + 180.0, 450.0),
        |_| {},
    );
    assert!(d.app.split_ratio > 0.6);
    assert!(d.app.pane_rects[0].width() > d.app.pane_rects[1].width());
    let session = crate::session::parse(&d.app.session().to_text());
    let mut restored = Driver::new(dir.clone());
    restored.app.restore(&session, true);
    assert_eq!(restored.app.split_ratio, d.app.split_ratio);
    assert_eq!(restored.app.panes.len(), 2);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn named_workspaces_save_load_delete_and_persist_layouts() {
    let dir = sandbox("workspaces");
    let file = dir.join("state/workspaces.json");
    let mut d = Driver::new(dir.clone());
    d.app.workspace_file = Some(file.clone());
    d.app.open_tab(dir.join("dest"));
    d.app.tab_mut().mode = ViewMode::Icons(3);
    d.app.toggle_split();
    d.app.split_ratio = 0.65;
    d.app.preview_width = 440.0;
    d.app.sidebar_width = 250.0;
    d.app.show_sidebar = false;
    d.app.run_cmd(Cmd::SaveWorkspace);
    d.frame(Vec::new());
    d.frame(vec![Event::Text("Development".into())]);
    d.frame(Vec::new());
    d.click_text("Save workspace");
    assert!(d.app.dialog.is_none());
    assert!(file.exists());
    let mut restored = Driver::new(dir.clone());
    restored.app.workspace_file = Some(file.clone());
    restored.app.workspaces = crate::workspaces::load(&file).unwrap();
    restored.app.run_cmd(Cmd::Workspaces);
    restored.frame(Vec::new());
    restored.frame(Vec::new());
    restored.click_text("Load");
    assert_eq!(restored.app.panes.len(), 2);
    assert_eq!(restored.app.panes[0].tabs.len(), 2);
    assert_eq!(restored.app.panes[0].tabs[1].mode, ViewMode::Icons(3));
    assert_eq!(restored.app.split_ratio, 0.65);
    assert!(!restored.app.show_sidebar);
    restored.frame(Vec::new());
    let preview =
        egui::containers::panel::PanelState::load(&restored.ctx, egui::Id::new("preview"))
            .unwrap()
            .size()
            .x;
    assert!((preview - 440.0).abs() < 1.0);
    assert_eq!(restored.app.sidebar_width, 250.0);
    restored.app.run_cmd(Cmd::Workspaces);
    restored.frame(Vec::new());
    restored.frame(Vec::new());
    restored.click_text("Delete");
    assert!(restored.app.workspaces.is_empty());
    assert!(crate::workspaces::load(&file).unwrap().is_empty());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn batch_rename_previews_applies_and_undoes_as_one_operation() {
    let dir = sandbox("batch-rename");
    let mut d = Driver::new(dir.clone());
    d.until("listing", |a| !a.tab().loading);
    d.app.tab_mut().select_all();
    d.frame(vec![Event::Key {
        key: Key::F2,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    }]);
    let Some(Dialog::BatchRename(editor)) = &mut d.app.dialog else {
        panic!("F2 on a selection must open batch rename")
    };
    editor.options.template = "{n}-{name}{ext}".into();
    editor.request();
    d.until("rename preview", |a| matches!(&a.dialog, Some(Dialog::BatchRename(e)) if e.plan.as_ref().is_some_and(|p| p.valid())));
    assert!(dir.join("a.txt").exists() && dir.join("b.txt").exists());
    assert!(d.texts.iter().any(|(t, _)| t == "a.txt → 002-a.txt"));
    d.click_text("Rename all");
    d.until("renamed selection", |a| {
        a.undo.len() == 1
            && dir.join("001-dest").is_dir()
            && dir.join("002-a.txt").exists()
            && dir.join("003-b.txt").exists()
    });
    assert!(!dir.join("a.txt").exists());
    d.app.run_cmd(Cmd::Undo);
    d.until("undo batch", |a| {
        a.ops.queued.load(Ordering::Relaxed) == 0
            && dir.join("dest").is_dir()
            && dir.join("a.txt").exists()
            && dir.join("b.txt").exists()
    });
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 3);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn batch_rename_conflicts_disable_applying_and_cancel_keeps_files() {
    let dir = sandbox("batch-conflicts");
    let mut d = Driver::new(dir.clone());
    d.until("listing", |a| !a.tab().loading);
    d.app.tab_mut().select_all();
    d.app.run_cmd(Cmd::BatchRename);
    let Some(Dialog::BatchRename(editor)) = &mut d.app.dialog else {
        panic!("missing rename dialog")
    };
    editor.options.template = "same".into();
    editor.request();
    d.until(
        "conflict preview",
        |a| matches!(&a.dialog, Some(Dialog::BatchRename(e)) if e.plan.is_some()),
    );
    d.frame(Vec::new());
    let Some(Dialog::BatchRename(editor)) = &d.app.dialog else {
        unreachable!()
    };
    assert!(!editor.plan.as_ref().unwrap().valid());
    d.click_text("Rename all");
    assert!(d.app.dialog.is_some());
    assert_eq!(d.app.ops.queued.load(Ordering::Relaxed), 0);
    d.click_text("Cancel");
    assert!(d.app.dialog.is_none());
    assert!(dir.join("a.txt").exists() && dir.join("b.txt").exists());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn flatten_view_browses_descendants_filters_and_returns_to_folder() {
    let dir = sandbox("flatten");
    fs::create_dir_all(dir.join("dest/deeper")).unwrap();
    fs::write(dir.join("dest/deeper/nested.txt"), "nested").unwrap();
    fs::write(dir.join("dest/.hidden.txt"), "hidden").unwrap();
    std::os::unix::fs::symlink(&dir, dir.join("dest/loop")).unwrap();
    let mut d = Driver::new(dir.clone());
    d.until("listing", |a| !a.tab().loading);
    d.modifiers = Modifiers::COMMAND | Modifiers::SHIFT;
    let toggle = Event::Key {
        key: Key::F,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: d.modifiers,
    };
    d.frame(vec![toggle.clone()]);
    d.until("flattened scan", |a| {
        a.tab()
            .search
            .as_ref()
            .is_some_and(|s| s.flatten && !s.running)
    });
    assert_eq!(d.app.tab().path, dir);
    assert_eq!(d.app.tab().view.len(), 6); // root files, dest, deeper, loop and nested.txt
    let nested = d
        .app
        .tab()
        .entries
        .iter()
        .find(|e| e.name == "nested.txt")
        .unwrap();
    assert_eq!(nested.rel.as_deref(), Some("dest/deeper"));
    let link = dir.join("dest/loop");
    assert!(
        !d.app
            .tab()
            .entries
            .iter()
            .any(|e| e.hidden || (e.path != link && e.path.starts_with(&link)))
    );
    let index = d.app.tab().search.as_ref().unwrap().index.clone().unwrap();
    d.app.tab_mut().search.as_mut().unwrap().query = "ext:txt type:file".into();
    d.app.restart_search();
    d.until("flat filter", |a| !a.tab().search.as_ref().unwrap().running);
    assert_eq!(d.app.tab().view.len(), 3);
    assert!(Arc::ptr_eq(
        &index,
        d.app.tab().search.as_ref().unwrap().index.as_ref().unwrap()
    ));
    // Toggle works while the flattened view's text field owns keyboard focus.
    d.frame(vec![toggle]);
    d.until("normal folder", |a| {
        a.tab().search.is_none() && !a.tab().loading
    });
    assert_eq!(d.app.tab().entries.len(), 3);
    assert_eq!(d.app.tab().path, dir);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn folder_inspector_browses_nested_folders_and_files_independently() {
    let dir = sandbox("inspector-browse");
    fs::create_dir_all(dir.join("dest/nested")).unwrap();
    fs::write(dir.join("dest/nested/message.txt"), "inspector content").unwrap();
    let mut d = Driver::new(dir.clone());
    d.until("listing", |a| !a.tab().loading);
    d.app.tab_mut().select_path(&dir.join("dest"));
    d.frame(Vec::new());
    d.until("folder preview", |a| matches!(&a.preview.state, crate::preview::State::Ready(crate::preview::Content::Dir { entries }) if entries.iter().any(|e| e.name == "nested")));
    d.click_text("nested");
    d.until("nested preview", |a| matches!(&a.preview.state, crate::preview::State::Ready(crate::preview::Content::Dir { entries }) if entries.iter().any(|e| e.name == "message.txt")));
    d.click_text("message.txt");
    d.until("file preview", |a| matches!(&a.preview.state, crate::preview::State::Ready(crate::preview::Content::Text { job, .. }) if job.text == "inspector content"));
    assert_eq!(d.app.tab().path, dir);
    assert_eq!(d.app.tab().cursor_entry().unwrap().path, dir.join("dest"));
    assert_eq!(
        d.app.preview.path,
        Some(dir.join("dest/nested/message.txt"))
    );
    d.click_text("‹ Back");
    d.until("back to nested", |a| {
        matches!(
            &a.preview.state,
            crate::preview::State::Ready(crate::preview::Content::Dir { .. })
        )
    });
    assert_eq!(d.app.preview.path, Some(dir.join("dest/nested")));
    d.click_text("‹ Back");
    d.until("back to root", |a| {
        matches!(
            &a.preview.state,
            crate::preview::State::Ready(crate::preview::Content::Dir { .. })
        )
    });
    assert_eq!(d.app.preview.path, Some(dir.join("dest")));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn folder_inspector_tracks_changes_hides_dotfiles_and_opens_in_main_view() {
    let dir = sandbox("inspector-refresh");
    fs::create_dir_all(dir.join("dest/nested")).unwrap();
    fs::write(dir.join("dest/.secret"), "hidden").unwrap();
    let mut d = Driver::new(dir.clone());
    d.until("listing", |a| !a.tab().loading);
    d.app.tab_mut().select_path(&dir.join("dest"));
    d.frame(Vec::new());
    d.until("folder preview", |a| {
        matches!(
            &a.preview.state,
            crate::preview::State::Ready(crate::preview::Content::Dir { .. })
        )
    });
    assert!(!d.texts.iter().any(|(text, _)| text == ".secret"));
    fs::write(dir.join("dest/new.txt"), "new").unwrap();
    d.until("live inspector", |a| matches!(&a.preview.state, crate::preview::State::Ready(crate::preview::Content::Dir { entries }) if entries.iter().any(|e| e.name == "new.txt")));
    d.click_text("nested");
    d.until("nested preview", |a| {
        matches!(
            &a.preview.state,
            crate::preview::State::Ready(crate::preview::Content::Dir { .. })
        )
    });
    d.click_text("Open folder in main view ↗");
    assert_eq!(d.app.tab().path, dir.join("dest/nested"));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn goto_switcher_fuzzy_matches_and_switches_existing_tabs() {
    let dir = sandbox("goto-tabs");
    fs::create_dir(dir.join("NaDya")).unwrap();
    let mut d = Driver::new(dir.clone());
    d.until("listing", |a| !a.tab().loading);
    d.app.open_tab(dir.join("NaDya"));
    d.app.open_tab(dir.clone());
    d.modifiers = Modifiers::COMMAND;
    d.frame(vec![Event::Key {
        key: Key::K,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: d.modifiers,
    }]);
    assert!(d.app.palette.open && d.app.palette.goto);
    d.modifiers = Modifiers::NONE;
    d.frame(vec![Event::Text("nd".into())]);
    d.frame(Vec::new());
    assert!(d.texts.iter().any(|(t, _)| t == "NaDya"));
    d.frame(vec![Event::Key {
        key: Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: d.modifiers,
    }]);
    assert_eq!(d.app.panes[0].active, 1);
    assert_eq!(d.app.tab().path, dir.join("NaDya"));
    assert_eq!(d.app.panes[0].tabs.len(), 3);
    assert!(!d.app.palette.open);
    assert!(d.app.tab().filter.is_empty());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn goto_switcher_jumps_to_bookmarks_recents_and_cancels() {
    let dir = sandbox("goto-folders");
    let mut d = Driver::new(dir.clone());
    d.until("listing", |a| !a.tab().loading);
    d.app.places.bookmarks = vec![("Family".into(), dir.join("dest"))];
    d.app.run_cmd(Cmd::GoTo);
    d.frame(Vec::new());
    d.frame(vec![Event::Text("fm".into())]);
    d.frame(Vec::new());
    d.frame(vec![Event::Key {
        key: Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    }]);
    assert_eq!(d.app.tab().path, dir.join("dest"));
    d.app.navigate(dir.clone(), History::Push);
    d.app.places.bookmarks.clear();
    assert!(
        d.app
            .goto_items()
            .iter()
            .any(|(_, detail, _)| detail == &format!("Recent · {}", dir.join("dest").display()))
    );
    d.app.run_cmd(Cmd::GoTo);
    d.frame(Vec::new());
    d.frame(vec![Event::Text("does-not-exist".into())]);
    d.frame(Vec::new());
    assert!(d.texts.iter().any(|(t, _)| t == "No matches"));
    d.frame(vec![Event::Key {
        key: Key::Escape,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    }]);
    assert!(!d.app.palette.open);
    assert_eq!(d.app.tab().path, dir);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn goto_shortcut_works_in_search_and_recents_survive_session_restore() {
    let dir = sandbox("goto-session");
    let mut d = Driver::new(dir.clone());
    d.until("listing", |a| !a.tab().loading);
    d.app.navigate(dir.join("dest"), History::Push);
    d.app.navigate(dir.clone(), History::Push);
    let session = crate::session::parse(&d.app.session().to_text());
    let mut restored = Driver::new(dir.clone());
    restored.app.restore(&session, true);
    assert_eq!(
        restored.app.recent_dirs,
        vec![dir.clone(), dir.join("dest")]
    );
    assert_eq!(
        restored
            .app
            .goto_items()
            .iter()
            .filter(|(_, detail, _)| detail.ends_with(&dir.display().to_string()))
            .count(),
        1
    );
    restored.app.start_search();
    restored.frame(Vec::new());
    restored.modifiers = Modifiers::COMMAND;
    restored.frame(vec![Event::Key {
        key: Key::K,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: restored.modifiers,
    }]);
    assert!(restored.app.palette.open && restored.app.palette.goto);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn typing_a_filter_selects_the_best_fuzzy_match() {
    let dir = sandbox("fuzzy-filter");
    for name in ["Named", "NaDya", "nd-notes", "ND"] {
        fs::create_dir(dir.join(name)).unwrap();
    }
    let mut d = Driver::new(dir.clone());
    d.until("listing", |a| !a.tab().loading);
    d.app.tab_mut().select_path(&dir.join("Named"));
    d.frame(vec![Event::Text("nd".into())]);
    assert_eq!(d.app.tab().cursor_entry().unwrap().name, "ND");
    let names: Vec<_> = d
        .app
        .tab()
        .view
        .iter()
        .map(|&i| d.app.tab().entries[i].name.as_str())
        .collect();
    assert_eq!(names, ["ND", "nd-notes", "NaDya", "Named"]);
    d.frame(vec![Event::Text("y".into())]);
    assert_eq!(d.app.tab().cursor_entry().unwrap().name, "NaDya");
    d.frame(vec![Event::Key {
        key: Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    }]);
    assert_eq!(d.app.tab().path, dir.join("NaDya"));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn recursive_search_finds_and_ranks_fuzzy_matches() {
    let dir = sandbox("fuzzy-recursive");
    fs::create_dir_all(dir.join("dest/deeper")).unwrap();
    for name in ["ND", "nd-notes", "a-nd", "NaDya", "Named", "other"] {
        fs::write(dir.join("dest/deeper").join(name), "text").unwrap();
    }
    let mut d = Driver::new(dir.clone());
    d.until("listing", |a| !a.tab().loading);
    d.app.tab_mut().filter = "nd".into();
    d.app.start_search();
    d.until("recursive fuzzy search", |a| {
        !a.tab().search.as_ref().unwrap().running
    });
    let names: Vec<_> = d
        .app
        .tab()
        .view
        .iter()
        .map(|&i| d.app.tab().entries[i].name.as_str())
        .collect();
    assert_eq!(names, ["ND", "nd-notes", "a-nd", "NaDya", "Named"]);
    assert_eq!(d.app.tab().cursor_entry().unwrap().name, "ND");
    d.frame(vec![Event::Key {
        key: Key::ArrowDown,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    }]);
    assert_eq!(d.app.tab().cursor_entry().unwrap().name, "nd-notes");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn recursive_search_reuses_its_scan_and_refresh_rebuilds_it() {
    let dir = sandbox("cached-search");
    fs::write(dir.join("NaDya.txt"), "text").unwrap();
    fs::write(dir.join("dest/report.pdf"), "pdf").unwrap();
    let mut d = Driver::new(dir.clone());
    d.until("listing", |a| !a.tab().loading);
    d.app.tab_mut().filter = "nd".into();
    d.app.start_search();
    d.until("scan", |a| !a.tab().search.as_ref().unwrap().running);
    let index = d.app.tab().search.as_ref().unwrap().index.clone().unwrap();
    d.app.tab_mut().search.as_mut().unwrap().query = "ext:pdf type:file".into();
    d.app.restart_search();
    d.until("cached query", |a| {
        !a.tab().search.as_ref().unwrap().running
    });
    assert_eq!(d.app.tab().view.len(), 1);
    assert_eq!(d.app.tab().entry(0).unwrap().name, "report.pdf");
    assert!(Arc::ptr_eq(
        &index,
        d.app.tab().search.as_ref().unwrap().index.as_ref().unwrap()
    ));
    fs::write(dir.join("dest/new.pdf"), "pdf").unwrap();
    d.app.refresh();
    d.until("fresh scan", |a| !a.tab().search.as_ref().unwrap().running);
    assert_eq!(d.app.tab().view.len(), 2);
    assert!(!Arc::ptr_eq(
        &index,
        d.app.tab().search.as_ref().unwrap().index.as_ref().unwrap()
    ));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn search_highlights_matched_letters_in_every_view() {
    for mode in [ViewMode::Details, ViewMode::List, ViewMode::Icons(1)] {
        let dir = sandbox(&format!("highlights-{mode:?}"));
        fs::write(dir.join("NaDya.txt"), "text").unwrap();
        let mut d = Driver::new(dir.clone());
        d.app.tab_mut().mode = mode;
        d.until("listing", |a| !a.tab().loading);
        d.frame(vec![Event::Text("nd ext:txt".into())]);
        assert_eq!(d.app.tab().view.len(), 1);
        let tab = d.app.tab();
        let query = tab.query();
        let job = super::ui::name_job(
            "NaDya.txt",
            &query,
            FontId::proportional(13.0),
            Color32::WHITE,
        );
        assert_eq!(job.text, "NaDya.txt");
        let highlighted: String = job
            .sections
            .iter()
            .filter(|s| s.format.underline != egui::Stroke::NONE)
            .map(|s| &job.text[s.byte_range.start.0..s.byte_range.end.0])
            .collect();
        assert_eq!(highlighted, "ND");
        assert!(d.texts.iter().any(|(t, _)| t == "NaDya.txt"));
        fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn streamed_search_promotes_better_matches_and_keeps_manual_selection() {
    let dir = sandbox("fuzzy-streaming");
    let mut d = Driver::new(dir.clone());
    d.until("listing", |a| !a.tab().loading);
    // An empty search does not spawn a worker; feed batches in a known order.
    d.app.start_search();
    d.app.tab_mut().search.as_mut().unwrap().query = "nd".into();
    let tab_id = d.app.tab().id;
    let generation = d.app.tab().search.as_ref().unwrap().generation;
    let send = |d: &mut Driver, name: &str| {
        let path = dir.join(name);
        fs::write(&path, "text").unwrap();
        let entry = fsops::make_entry(path.clone(), name.into(), fs::metadata(&path).unwrap());
        d.app
            .search_tx
            .send(SearchMsg {
                tab_id,
                generation,
                batch: vec![entry],
                done: false,
                truncated: false,
                replace: false,
                index: None,
            })
            .unwrap();
        d.frame(Vec::new());
    };
    send(&mut d, "Named");
    send(&mut d, "NaDya");
    assert_eq!(d.app.tab().cursor_entry().unwrap().name, "NaDya");
    send(&mut d, "a-nd");
    assert_eq!(d.app.tab().cursor_entry().unwrap().name, "a-nd");
    d.app.tab_mut().select_path(&dir.join("a-nd"));
    send(&mut d, "ND");
    assert_eq!(d.app.tab().entry(0).unwrap().name, "ND");
    assert_eq!(d.app.tab().cursor_entry().unwrap().name, "a-nd");
    assert_eq!(d.app.tab().anchor, d.app.tab().cursor);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn folder_updates_keep_selection_and_scroll() {
    let dir = sandbox("refresh-keep");
    for i in 0..100 {
        fs::write(dir.join(format!("file-{i:03}.txt")), "text").unwrap();
    }
    let mut d = Driver::new(dir.clone());
    d.until("listing", |a| !a.tab().loading);
    let cursor_name = |a: &App| a.tab().cursor_entry().map(|e| e.name.clone());
    d.app.tab_mut().select_path(&dir.join("file-090.txt"));
    d.frame(Vec::new());
    d.frame(Vec::new());
    let offset = d.app.tab().scroll_offset;
    assert!(offset > 0.0);

    // A new file sorting above the cursor shifts every row by one.
    fs::write(dir.join("a-new.txt"), "new").unwrap();
    d.until("folder update", |a| {
        a.tab().entries.iter().any(|e| e.name == "a-new.txt")
    });
    d.frame(Vec::new());
    assert_eq!(cursor_name(&d.app).as_deref(), Some("file-090.txt"));
    assert_eq!(d.app.tab().scroll_offset, offset);

    // Marks survive an update too.
    let vi = d.app.tab().cursor.unwrap() + 2;
    d.app.tab_mut().toggle_mark(vi);
    let marked = d.app.tab().marked.clone();
    assert_eq!(marked.len(), 2);
    fs::write(dir.join("b-new.txt"), "new").unwrap();
    d.until("second update", |a| {
        a.tab().entries.iter().any(|e| e.name == "b-new.txt")
    });
    assert_eq!(d.app.tab().marked, marked);
    assert_eq!(cursor_name(&d.app).as_deref(), Some("file-092.txt"));

    // When the cursor's file goes away, the cursor stays in place, on its neighbour.
    d.app.tab_mut().select_path(&dir.join("file-090.txt"));
    fs::remove_file(dir.join("file-090.txt")).unwrap();
    d.until("removal", |a| {
        a.tab().entries.iter().all(|e| e.name != "file-090.txt")
    });
    assert_eq!(cursor_name(&d.app).as_deref(), Some("file-091.txt"));

    d.app.refresh();
    d.until("refresh", |a| !a.tab().loading);
    assert_eq!(cursor_name(&d.app).as_deref(), Some("file-091.txt"));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn preview_web_link_click_opens_url() {
    let ctx = egui::Context::default();
    let url = "https://example.com/?a=1&b=2";
    let job = egui::text::LayoutJob::simple(
        format!("Привет {url} trailing"),
        FontId::monospace(12.5),
        Color32::WHITE,
        f32::INFINITY,
    );
    let links = vec![crate::preview::TextLink {
        url: url.into(),
        chars: 7..7 + url.chars().count(),
    }];
    let mut target = Pos2::ZERO;
    for pressed in [None, Some(true), Some(false)] {
        let mut events = vec![Event::PointerMoved(target)];
        if let Some(pressed) = pressed {
            events.push(Event::PointerButton {
                pos: target,
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            });
        }
        let mut out = ctx.run_ui(
            RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 300.0))),
                events,
                ..Default::default()
            },
            |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    let galley = ui.fonts_mut(|f| f.layout_job(job.clone()));
                    target = ui.cursor().min
                        + vec2(
                            galley.rows[0].x_offset(egui::text::CharIndex(8)),
                            galley.rows[0].size.y / 2.0,
                        );
                    super::ui::preview_text(ui, &job, &links);
                });
            },
        );
        out.textures_delta.clear();
        if pressed == Some(false) {
            assert!(out.platform_output.commands.iter().any(|command| {
                matches!(command, egui::OutputCommand::OpenUrl(open) if open.url == url)
            }));
        }
    }
}

#[test]
fn csv_preview_scrolls_sideways_and_opens_links() {
    let ctx = egui::Context::default();
    let url = "https://example.com/csv";
    let wide = "x".repeat(40);
    let table = crate::preview::Table {
        rows: vec![
            vec!["a".into(), "url".into(), "wide".into()],
            vec!["1".into(), url.into(), wide.clone()],
        ],
        widths: vec![1, url.len(), wide.len()],
        truncated: false,
    };
    let mut time = 0.0;
    // Returns the frame's output, a point inside the link, and the scroll area's id.
    let mut run = |mut events: Vec<Event>, modifiers: Modifiers| {
        time += 1.0 / 60.0;
        events.insert(0, Event::ModifiersChanged(modifiers));
        let (mut link_at, mut scroll_id) = (Pos2::ZERO, egui::Id::NULL);
        let mut out = ctx.run_ui(
            RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(300.0, 200.0))),
                time: Some(time),
                events,
                ..Default::default()
            },
            |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    scroll_id = ui.make_persistent_id(egui::IdSalt::new("preview_table"));
                    let font = FontId::monospace(12.5);
                    let (row_h, glyph) =
                        ui.fonts_mut(|f| (f.row_height(&font) + 6.0, f.glyph_width(&font, '0')));
                    // Inside the URL in the first body row: past the gutter and the one-char column.
                    link_at = ui.cursor().min
                        + vec2(2.0 * (glyph + 16.0) + 8.0 + 5.0 * glyph, row_h * 1.5);
                    super::ui::preview_table(ui, &table);
                });
            },
        );
        out.textures_delta.clear();
        (out.platform_output, link_at, scroll_id)
    };
    let (_, link_at, scroll_id) = run(Vec::new(), Modifiers::NONE);

    let press = |pressed| Event::PointerButton {
        pos: link_at,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    run(vec![Event::PointerMoved(link_at)], Modifiers::NONE);
    run(
        vec![Event::PointerMoved(link_at), press(true)],
        Modifiers::NONE,
    );
    let (out, ..) = run(vec![press(false)], Modifiers::NONE);
    assert!(
        out.commands
            .iter()
            .any(|c| matches!(c, egui::OutputCommand::OpenUrl(open) if open.url == url))
    );

    // Shift + mouse wheel scrolls sideways.
    let wheel = Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: vec2(0.0, -200.0),
        modifiers: Modifiers::SHIFT,
        phase: egui::TouchPhase::Move,
    };
    run(vec![Event::PointerMoved(link_at), wheel], Modifiers::SHIFT);
    for _ in 0..30 {
        run(Vec::new(), Modifiers::NONE);
    }
    let offset = egui::scroll_area::State::load(&ctx, scroll_id)
        .unwrap()
        .offset;
    assert!(offset.x > 0.0, "no horizontal scroll: {offset:?}");
}

#[test]
fn drag_file_onto_folder_moves_it() {
    let dir = sandbox("move");
    let mut d = Driver::new(dir.clone());
    d.until("listing", |a| a.row_rects.contains_key(&dir.join("a.txt")));
    let (from, to) = (
        d.row(&dir.join("a.txt")).center(),
        d.row(&dir.join("dest")).center(),
    );

    d.drag(from, to, |d| {
        // While held: the payload is the file and the card targets the folder.
        let held = d
            .app
            .dragging
            .as_ref()
            .expect("file should be held while dragging");
        assert_eq!(held.0, vec![dir.join("a.txt")]);
        let target = d
            .ctx
            .data(|m| m.get_temp::<PathBuf>(egui::Id::new("kestrel_drop_target")));
        assert_eq!(target, Some(dir.join("dest")));
    });
    d.until("move", |_| dir.join("dest/a.txt").exists());
    assert!(!dir.join("a.txt").exists());
    assert!(d.app.dragging.is_none());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn ctrl_drag_copies_selection() {
    let dir = sandbox("copy");
    let mut d = Driver::new(dir.clone());
    d.until("listing", |a| a.row_rects.contains_key(&dir.join("b.txt")));
    // Select both files, then drag them with Ctrl held.
    d.app.tab_mut().select_path(&dir.join("a.txt"));
    d.app.tab_mut().select_range(2);
    assert_eq!(d.app.tab().selection_count(), 2);
    let (from, to) = (
        d.row(&dir.join("b.txt")).center(),
        d.row(&dir.join("dest")).center(),
    );
    d.modifiers = Modifiers::CTRL;
    d.drag(from, to, |d| {
        assert_eq!(d.app.dragging.as_ref().unwrap().0.len(), 2)
    });
    d.modifiers = Modifiers::NONE;
    d.until("copy", |_| {
        dir.join("dest/a.txt").exists() && dir.join("dest/b.txt").exists()
    });
    assert!(dir.join("a.txt").exists() && dir.join("b.txt").exists());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn drop_back_onto_own_folder_is_noop() {
    let dir = sandbox("noop");
    let mut d = Driver::new(dir.clone());
    d.until("listing", |a| a.row_rects.contains_key(&dir.join("a.txt")));
    let from = d.row(&dir.join("a.txt")).center();
    // Release on empty space below the rows: same folder, nothing to do.
    let below = from + vec2(0.0, 300.0);
    d.drag(from, below, |_| {});
    for _ in 0..20 {
        d.frame(Vec::new());
    }
    assert!(dir.join("a.txt").exists());
    assert!(fs::read_dir(dir.join("dest")).unwrap().next().is_none());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn click_without_moving_does_not_drag() {
    let dir = sandbox("click");
    let mut d = Driver::new(dir.clone());
    d.until("listing", |a| a.row_rects.contains_key(&dir.join("b.txt")));
    d.move_to(d.row(&dir.join("b.txt")).center());
    d.button(true);
    assert!(d.app.dragging.is_none());
    d.button(false);
    d.frame(Vec::new());
    assert_eq!(
        d.app.tab().cursor_entry().map(|e| e.name.as_str()),
        Some("b.txt")
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn non_primary_buttons_do_not_drag_folders() {
    for mode in [ViewMode::Details, ViewMode::List, ViewMode::Icons(2)] {
        for button in [
            PointerButton::Secondary,
            PointerButton::Middle,
            PointerButton::Extra1,
            PointerButton::Extra2,
        ] {
            let dir = sandbox(&format!("non-primary-{mode:?}-{button:?}"));
            fs::create_dir(dir.join("source")).unwrap();
            let mut d = Driver::new(dir.clone());
            d.app.tab_mut().mode = mode;
            d.until("listing", |a| a.row_rects.contains_key(&dir.join("source")));
            let from = d.row(&dir.join("source")).center();
            let to = d.row(&dir.join("dest")).center();
            d.move_to(from);
            d.mouse_button(button, true);
            d.move_to(from + vec2(16.0, 0.0));
            assert!(
                !egui::DragAndDrop::has_any_payload(&d.ctx),
                "{button:?} started a folder drag in {mode:?}"
            );
            d.move_to(to);
            d.mouse_button(button, false);
            assert!(dir.join("source").is_dir());
            assert!(!dir.join("dest/source").exists());
            fs::remove_dir_all(dir).unwrap();
        }
    }
}

#[test]
fn side_buttons_navigate_without_dragging_folders() {
    for button in [PointerButton::Extra1, PointerButton::Extra2] {
        let dir = sandbox(&format!("mouse-history-{button:?}"));
        fs::create_dir(dir.join("dest/child")).unwrap();
        let mut d = Driver::new(dir.clone());
        d.app.navigate(dir.join("dest"), History::Push);
        if button == PointerButton::Extra2 {
            d.app.go_back();
        }
        d.until("listing", |a| !a.tab().loading);
        let (hover, target) = if button == PointerButton::Extra1 {
            (dir.join("dest/child"), dir.clone())
        } else {
            (dir.join("dest"), dir.join("dest"))
        };
        d.move_to(d.row(&hover).center());
        d.mouse_button(button, true);
        assert_eq!(d.app.tab().path, target);
        d.until("history listing", |a| !a.tab().loading);
        let folder = if button == PointerButton::Extra1 {
            dir.join("dest")
        } else {
            dir.join("dest/child")
        };
        let from = d.row(&folder).center();
        d.move_to(from);
        d.move_to(from + vec2(16.0, 0.0));
        assert!(
            !egui::DragAndDrop::has_any_payload(&d.ctx),
            "history button started a folder drag"
        );
        d.mouse_button(button, false);
        assert!(dir.join("dest/child").is_dir());
        fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn non_primary_release_does_not_drop_files() {
    for button in [
        PointerButton::Secondary,
        PointerButton::Middle,
        PointerButton::Extra1,
        PointerButton::Extra2,
    ] {
        for on_folder in [true, false] {
            let dir = sandbox(&format!("non-primary-release-{button:?}-{on_folder}"));
            let mut d = Driver::new(dir.clone());
            d.until("listing", |a| a.row_rects.contains_key(&dir.join("a.txt")));
            let from = d.row(&dir.join("a.txt")).center();
            let to = if on_folder {
                d.row(&dir.join("dest")).center()
            } else {
                from + vec2(0.0, 300.0)
            };
            d.move_to(from);
            d.button(true);
            d.move_to(from + vec2(16.0, 0.0));
            assert!(egui::DragAndDrop::has_any_payload(&d.ctx));
            d.move_to(to);
            // Use another folder's file for the background case so a spurious drop
            // would submit a move instead of the same-folder no-op.
            fs::write(dir.join("dest/held.txt"), "held").unwrap();
            egui::DragAndDrop::set_payload(&d.ctx, DragPaths(vec![dir.join("dest/held.txt")]));
            if on_folder {
                egui::DragAndDrop::set_payload(&d.ctx, DragPaths(vec![dir.join("a.txt")]));
            }
            d.mouse_button(button, true);
            d.mouse_button(button, false);
            d.until("idle file operations", |a| {
                a.ops.queued.load(Ordering::Relaxed) == 0
            });
            assert!(
                dir.join("a.txt").exists(),
                "{button:?} release moved a file onto a folder"
            );
            assert!(
                dir.join("dest/held.txt").exists(),
                "{button:?} release moved a file onto the background"
            );
            assert!(!dir.join("dest/a.txt").exists());
            assert!(!dir.join("held.txt").exists());
            d.button(false);
            fs::remove_dir_all(dir).unwrap();
        }
    }
}

#[test]
fn drop_onto_a_file_row_lands_in_the_viewed_folder() {
    let dir = sandbox("drop-on-file-row");
    fs::write(dir.join("dest/held.txt"), "held").unwrap();
    let mut d = Driver::new(dir.clone());
    d.until("listing", |a| a.row_rects.contains_key(&dir.join("b.txt")));
    let from = d.row(&dir.join("a.txt")).center();
    let file_row = d.row(&dir.join("b.txt")).center();
    d.move_to(from);
    d.button(true);
    d.move_to(from + vec2(16.0, 0.0));
    d.move_to(file_row);
    egui::DragAndDrop::set_payload(&d.ctx, DragPaths(vec![dir.join("dest/held.txt")]));
    d.button(false);
    d.frame(Vec::new());
    d.until("drop", |_| dir.join("held.txt").exists());
    assert!(!dir.join("dest/held.txt").exists(), "same filesystem moves");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn drop_mode_follows_modifiers() {
    assert_eq!(DropMode::held(false, false), DropMode::Auto);
    assert_eq!(DropMode::held(true, false), DropMode::Copy);
    assert_eq!(DropMode::held(false, true), DropMode::Move);
    assert_eq!(DropMode::held(true, true), DropMode::Copy);
}

#[test]
fn deleted_folder_falls_back_to_parent() {
    let dir = sandbox("gone");
    let inner = dir.join("dest");
    let mut d = Driver::new(inner.clone());
    d.until("listing", |a| !a.tab().loading);
    fs::remove_dir(&inner).unwrap();
    d.until("fallback", |a| a.tab().path == dir && !a.tab().loading);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn escape_cancels_drag() {
    let dir = sandbox("escape");
    let mut d = Driver::new(dir.clone());
    d.until("listing", |a| a.row_rects.contains_key(&dir.join("a.txt")));
    let (from, to) = (
        d.row(&dir.join("a.txt")).center(),
        d.row(&dir.join("dest")).center(),
    );
    d.drag(from, to, |d| {
        let esc = Event::Key {
            key: Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        d.frame(vec![esc]);
        d.frame(Vec::new());
        assert!(d.app.dragging.is_none(), "Escape should drop the held file");
    });
    for _ in 0..20 {
        d.frame(Vec::new());
    }
    assert!(
        dir.join("a.txt").exists(),
        "cancelled drag must not move the file"
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn ctrl_wheel_steps_view_mode() {
    let dir = sandbox("zoom");
    let mut d = Driver::new(dir.clone());
    d.until("listing", |a| a.row_rects.contains_key(&dir.join("a.txt")));
    d.move_to(Pos2::new(700.0, 400.0));
    let wheel = |y: f32, unit| Event::MouseWheel {
        unit,
        delta: vec2(0.0, y),
        modifiers: Modifiers::CTRL,
        phase: egui::TouchPhase::Move,
    };
    d.modifiers = Modifiers::CTRL;
    d.frame(vec![wheel(1.0, egui::MouseWheelUnit::Line)]);
    assert_eq!(d.app.tab().mode, ViewMode::List);
    d.frame(vec![wheel(1.0, egui::MouseWheelUnit::Line)]);
    d.frame(vec![wheel(1.0, egui::MouseWheelUnit::Line)]);
    assert_eq!(d.app.tab().mode, ViewMode::Icons(1));
    // Touchpad: small deltas add up to one step.
    for _ in 0..3 {
        d.frame(vec![wheel(-25.0, egui::MouseWheelUnit::Point)]);
    }
    assert_eq!(d.app.tab().mode, ViewMode::Icons(0));
    d.modifiers = Modifiers::NONE;
    // A plain wheel scrolls instead of zooming.
    d.frame(vec![wheel(1.0, egui::MouseWheelUnit::Line)]);
    assert_eq!(d.app.tab().mode, ViewMode::Icons(0));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn sidebars_hide() {
    let dir = sandbox("panels");
    let mut d = Driver::new(dir.clone());
    d.frame(Vec::new());
    let before = d.app.pane_rects[0];
    d.app.run_cmd(Cmd::ToggleSidebar);
    d.app.run_cmd(Cmd::TogglePreview);
    for _ in 0..30 {
        d.frame(Vec::new()); // let the slide animation finish
    }
    let after = d.app.pane_rects[0];
    assert!(
        after.left() < before.left() - 100.0,
        "{before:?} -> {after:?}"
    );
    assert!(
        after.right() > before.right() + 100.0,
        "{before:?} -> {after:?}"
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn path_bar_completes_folders() {
    let dir = sandbox("complete");
    fs::create_dir_all(dir.join("dest/inner")).unwrap();
    fs::create_dir_all(dir.join("deep")).unwrap();
    let mut d = Driver::new(dir.clone());
    d.until("listing", |a| a.row_rects.contains_key(&dir.join("a.txt")));
    d.app.run_cmd(Cmd::EditPath);
    d.frame(Vec::new());
    d.frame(Vec::new());
    let key = |key| Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    };
    let text = |d: &Driver| d.app.path_edit.as_ref().map(|(_, t)| t.clone());
    let base = format!("{}/", dir.display());
    assert_eq!(text(&d), Some(base.clone()));

    d.frame(vec![Event::Text("de".into())]);
    // "deep" and "dest" share only "de": Tab falls back to the first match.
    d.frame(vec![key(Key::Tab)]);
    assert_eq!(text(&d), Some(format!("{base}deep/")));
    // Arrow keys pick from the dropdown; Enter goes there.
    d.frame(vec![key(Key::Backspace); 3]);
    d.frame(vec![Event::Text("s".into())]);
    d.frame(vec![key(Key::ArrowDown)]);
    assert_eq!(d.app.path_hl, Some(0));
    d.frame(vec![key(Key::Enter)]);
    d.until("navigate", |a| {
        a.tab().path.as_os_str() == dir.join("dest").as_os_str()
    });
    assert!(d.app.path_edit.is_none());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn sidebar_sections_collapse() {
    let dir = sandbox("collapse");
    let mut d = Driver::new(dir.clone());
    d.frame(Vec::new());
    d.frame(Vec::new());
    // The PLACES header sits at the top of the sidebar.
    d.move_to(Pos2::new(40.0, 21.0));
    d.button(true);
    d.button(false);
    d.frame(Vec::new());
    assert!(d.app.collapsed.contains("PLACES"));
    d.button(true);
    d.button(false);
    d.frame(Vec::new());
    assert!(!d.app.collapsed.contains("PLACES"));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn paste_reaches_create_dialog_opened_from_palette() {
    let dir = sandbox("palette-paste");
    let mut d = Driver::new(dir.clone());
    d.until("listing", |a| a.row_rects.contains_key(&dir.join("a.txt")));
    let key = |k: Key| Event::Key {
        key: k,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    };
    d.modifiers = Modifiers::COMMAND;
    d.frame(vec![Event::Key {
        key: Key::P,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::COMMAND,
    }]);
    d.frame(vec![Event::Text("new folder".into())]);
    d.frame(vec![key(Key::Enter)]);
    // The dialog is first drawn on the frame that carries the paste.
    d.frame(vec![Event::Paste("pasted".into())]);
    d.frame(Vec::new());
    let name = match &d.app.dialog {
        Some(Dialog::Create { name, .. }) => name.clone(),
        _ => panic!("no create dialog"),
    };
    fs::remove_dir_all(dir).unwrap();
    assert_eq!(name, "pasted");
}

#[test]
fn create_dialog_field_is_focused() {
    let dir = sandbox("dialog-focus");
    let mut d = Driver::new(dir.clone());
    d.until("listing", |a| a.row_rects.contains_key(&dir.join("a.txt")));
    let cs = Modifiers::COMMAND | Modifiers::SHIFT;
    d.modifiers = cs;
    d.frame(vec![Event::Key {
        key: Key::N,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: cs,
    }]);
    d.frame(Vec::new());
    assert!(d.ctx.memory(|m| m.has_focus(egui::Id::new("dialog_name"))));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn compress_dialog_creates_archive() {
    let dir = sandbox("compress-dialog");
    let mut d = Driver::new(dir.clone());
    d.until("listing", |a| a.row_rects.contains_key(&dir.join("a.txt")));
    let r = d.row(&dir.join("a.txt"));
    d.move_to(r.center());
    d.button(true);
    d.button(false);
    d.app.run_cmd(Cmd::CompressAs);
    d.frame(Vec::new());
    d.frame(Vec::new());
    assert!(
        matches!(d.app.dialog, Some(Dialog::Compress { .. })),
        "dialog not open"
    );
    d.frame(vec![Event::Key {
        key: Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    }]);
    d.frame(Vec::new());
    assert!(d.app.dialog.is_none(), "dialog still open");
    d.until("archive", |_| dir.join("a.zip").exists());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn compress_from_context_menu() {
    let dir = sandbox("ctx-compress");
    let mut d = Driver::new(dir.clone());
    d.until("listing", |a| a.row_rects.contains_key(&dir.join("a.txt")));
    let start = d.row(&dir.join("a.txt")).center();
    d.move_to(start);
    let ev = |pressed| Event::PointerButton {
        pos: start,
        button: PointerButton::Secondary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    d.frame(vec![ev(true)]);
    d.frame(vec![ev(false)]);
    d.frame(Vec::new());
    let target = d
        .texts
        .iter()
        .find(|(t, _)| t == "Compress…")
        .map(|(_, r)| r.center())
        .expect("menu has no Compress… item");
    d.move_to(target);
    d.button(true);
    d.button(false);
    d.frame(Vec::new());
    d.frame(Vec::new());
    assert!(
        matches!(d.app.dialog, Some(Dialog::Compress { .. })),
        "no dialog after menu click"
    );
    fs::remove_dir_all(dir).unwrap();
}

fn picker_app(dir: &Path, kind: crate::picker::Kind, name: &str) -> Driver {
    let mut d = Driver::new(dir.to_path_buf());
    let req = crate::picker::Request {
        kind,
        multiple: false,
        title: String::new(),
        accept_label: String::new(),
        dir: None,
        name: name.into(),
    };
    d.app.picker = Some(Picker {
        name: req.name.clone(),
        focus: false,
        chosen: Vec::new(),
        req,
    });
    d.until("listing", |a| a.row_rects.contains_key(&dir.join("a.txt")));
    d
}

#[test]
fn picker_chooses_files_folders_and_save_names() {
    let dir = sandbox("picker");
    let (file, sub) = (dir.join("a.txt"), dir.join("dest"));

    let mut d = picker_app(&dir, crate::picker::Kind::Open, "");
    assert!(d.app.pick_choice().is_empty(), "nothing selected yet");
    d.app.apply(Action::Click {
        vi: d
            .app
            .tab()
            .view
            .iter()
            .position(|&i| d.app.tab().entries[i].path == sub)
            .unwrap(),
        ctrl: false,
        shift: false,
    });
    assert!(d.app.pick_choice().is_empty(), "a folder is not a file");
    d.app.apply(Action::Click {
        vi: d
            .app
            .tab()
            .view
            .iter()
            .position(|&i| d.app.tab().entries[i].path == file)
            .unwrap(),
        ctrl: false,
        shift: false,
    });
    assert_eq!(d.app.pick_choice(), vec![file.clone()]);

    let mut d = picker_app(&dir, crate::picker::Kind::Folder, "");
    d.app.apply(Action::Click {
        vi: d
            .app
            .tab()
            .view
            .iter()
            .position(|&i| d.app.tab().entries[i].path == sub)
            .unwrap(),
        ctrl: false,
        shift: false,
    });
    assert_eq!(d.app.pick_choice(), vec![sub]);

    let mut d = picker_app(&dir, crate::picker::Kind::Save, "new.txt");
    assert_eq!(d.app.pick_choice(), vec![dir.join("new.txt")]);
    d.app.picker.as_mut().unwrap().name = "a/b".into();
    assert!(
        d.app.pick_choice().is_empty(),
        "slashes are not allowed in a name"
    );
    d.app.picker.as_mut().unwrap().name = "  ".into();
    assert!(d.app.pick_choice().is_empty());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn space_toggles_preview_without_filtering_and_still_types_in_search() {
    let dir = sandbox("space-preview");
    let mut d = Driver::new(dir.clone());
    d.app.show_preview = false;
    d.until("listing", |a| a.row_rects.contains_key(&dir.join("a.txt")));
    let row = d.row(&dir.join("a.txt"));
    d.move_to(row.center());
    d.button(true);
    d.button(false);
    let selected = (d.app.tab().cursor, d.app.tab().marked.clone());
    let key = |modifiers| Event::Key {
        key: Key::Space,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    };
    d.frame(vec![key(Modifiers::NONE), Event::Text(" ".into())]);
    assert!(d.app.show_preview);
    assert!(d.app.tab().filter.is_empty());
    assert!(d.app.tab().search.is_none());
    assert_eq!((d.app.tab().cursor, d.app.tab().marked.clone()), selected);
    d.until("preview", |a| {
        a.preview.path.as_deref() == Some(dir.join("a.txt").as_path())
            && matches!(
                a.preview.state,
                crate::preview::State::Ready(crate::preview::Content::Text { .. })
            )
    });
    d.frame(vec![key(Modifiers::NONE), Event::Text(" ".into())]);
    assert!(!d.app.show_preview);
    assert!(d.app.tab().filter.is_empty());

    // Ctrl+Space keeps its selection shortcut.
    d.modifiers = Modifiers::COMMAND;
    d.frame(vec![key(Modifiers::COMMAND)]);
    assert!(!d.app.show_preview);
    assert!(d.app.tab().marked.contains(&dir.join("a.txt")));
    d.modifiers = Modifiers::NONE;

    d.app.run_cmd(Cmd::Search);
    d.frame(Vec::new());
    d.frame(vec![Event::Text("annual".into())]);
    d.frame(vec![key(Modifiers::NONE), Event::Text(" ".into())]);
    assert!(!d.app.show_preview);
    assert_eq!(d.app.tab().search.as_ref().unwrap().query, "annual ");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn preview_width_survives_resize_toggle_and_session() {
    let dir = sandbox("preview-width");
    let mut d = Driver::new(dir.clone());
    d.frame(Vec::new());
    let edge = d.app.pane_rects[0].right();
    d.drag(
        egui::pos2(edge, 400.0),
        egui::pos2(edge - 100.0, 400.0),
        |_| {},
    );
    let width = 1400.0 - d.app.pane_rects[0].right();
    assert!((width - 460.0).abs() < 3.0, "resized to {width}");
    assert!(
        (d.app.preview_width - width).abs() < 2.0,
        "remembered {} vs {width}",
        d.app.preview_width
    );
    d.app.run_cmd(Cmd::TogglePreview);
    for _ in 0..30 {
        d.frame(Vec::new());
    }
    d.app.run_cmd(Cmd::TogglePreview);
    for _ in 0..30 {
        d.frame(Vec::new());
    }
    assert!((1400.0 - d.app.pane_rects[0].right() - width).abs() < 2.0);
    // Drag closed, release, then pull the collapsed grab handle back open.
    let edge = d.app.pane_rects[0].right();
    d.drag(egui::pos2(edge, 400.0), egui::pos2(1390.0, 400.0), |_| {});
    for _ in 0..30 {
        d.frame(Vec::new());
    }
    assert!(!d.app.show_preview);
    assert!((d.app.preview_width - width).abs() < 2.0);
    d.drag(egui::pos2(1399.0, 400.0), egui::pos2(edge, 400.0), |_| {});
    for _ in 0..30 {
        d.frame(Vec::new());
    }
    assert!(d.app.show_preview);
    assert!((1400.0 - d.app.pane_rects[0].right() - width).abs() < 2.0);
    let saved = d.app.session();
    let mut restarted = Driver::new(dir.clone());
    restarted.app.restore(&saved, false);
    restarted.frame(Vec::new());
    assert!((1400.0 - restarted.app.pane_rects[0].right() - width).abs() < 2.0);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn path_editor_copies_selected_text() {
    let dir = sandbox("path-copy");
    let mut d = Driver::new(dir.clone());
    d.until("listing", |a| !a.tab().loading);
    d.app.run_cmd(Cmd::EditPath);
    d.frame(Vec::new());
    d.frame(Vec::new());
    let id = egui::Id::new(("path_edit", 0usize));
    let mut state = egui::TextEdit::load_state(&d.ctx, id).unwrap();
    state
        .cursor
        .set_char_range(Some(egui::text::CCursorRange::two(
            egui::text::CCursor::new(1),
            egui::text::CCursor::new(4),
        )));
    state.store(&d.ctx, id);
    d.frame(vec![Event::Copy]);
    assert!(
        d.commands
            .iter()
            .any(|c| matches!(c, egui::OutputCommand::CopyText(t) if t == "tmp")),
        "{:?}",
        d.commands
    );
    assert!(d.app.clip.is_none());
    assert!(d.app.path_edit.is_some());
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn preview_selects_unicode_multiline_text_and_keeps_file_copy_separate() {
    let dir = sandbox("preview-copy");
    let text = "Привет world\nsecond line";
    fs::write(dir.join("a.txt"), text).unwrap();
    let mut d = Driver::new(dir.clone());
    d.until("listing", |a| !a.tab().loading);
    d.app.tab_mut().select_path(&dir.join("a.txt"));
    d.frame(Vec::new());
    d.until("preview", |a| {
        matches!(
            a.preview.state,
            crate::preview::State::Ready(crate::preview::Content::Text { .. })
        )
    });
    d.frame(Vec::new());
    let rect = d
        .texts
        .iter()
        .find(|(t, _)| t.starts_with(text))
        .unwrap_or_else(|| panic!("texts: {:?}", d.texts))
        .1;
    d.drag(
        rect.left_top() + vec2(0.0, 3.0),
        rect.right_bottom() + vec2(8.0, -3.0),
        |_| {},
    );
    d.frame(vec![Event::Copy]);
    assert!(
        d.commands
            .iter()
            .any(|c| matches!(c, egui::OutputCommand::CopyText(t) if t == text)),
        "{:?}",
        d.commands
    );
    assert!(d.app.clip.is_none(), "text copy triggered file copy");
    d.app.tab_mut().select_path(&dir.join("b.txt"));
    d.frame(Vec::new());
    d.frame(vec![Event::Copy]);
    assert!(
        d.app.clip.is_some(),
        "old text selection intercepted file copy"
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn large_code_preview_select_all_copies_offscreen_source_without_file_shortcuts() {
    let dir = sandbox("large-code-copy");
    let text = "const привет = 'https://example.com';\n".repeat(2500);
    let path = dir.join("large.js");
    fs::write(&path, &text).unwrap();
    let mut d = Driver::new(dir.clone());
    d.until("listing", |a| !a.tab().loading);
    d.app.tab_mut().select_path(&path);
    d.until("chunked preview", |a| matches!(&a.preview.state,
        crate::preview::State::Ready(crate::preview::Content::Text { layout, .. }) if layout.chunks.is_some()));
    let at = match &d.app.preview.state {
        crate::preview::State::Ready(crate::preview::Content::Text { layout, links, .. }) => {
            assert!(links.is_empty());
            layout.chunks.as_ref().unwrap().rect.unwrap().min + vec2(60.0, 5.0)
        }
        _ => unreachable!(),
    };
    d.move_to(at);
    d.button(true);
    d.button(false);
    d.frame(vec![press(Key::A, Modifiers::COMMAND)]);
    d.frame(vec![Event::Copy]);
    assert!(
        d.commands
            .iter()
            .any(|c| matches!(c, egui::OutputCommand::CopyText(t) if t == &text))
    );
    assert!(d.app.clip.is_none(), "preview copy ran file copy");
    assert!(d.opened_urls.is_empty(), "code URL was clickable");
    d.app.tab_mut().select_path(&dir.join("b.txt"));
    d.frame(Vec::new());
    d.frame(vec![Event::Copy]);
    assert!(
        d.app.clip.is_some(),
        "obsolete preview selection captured file copy"
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn preview_table_cells_and_details_are_selectable() {
    for (name, filename, text, selected) in [
        ("csv-copy", "data.csv", "name,value\nПривет,42\n", "Привет"),
        ("metadata-copy", "a.txt", "body", "Permissions"),
    ] {
        let dir = sandbox(name);
        fs::write(dir.join(filename), text).unwrap();
        let mut d = Driver::new(dir.clone());
        d.until("listing", |a| !a.tab().loading);
        d.app.tab_mut().select_path(&dir.join(filename));
        d.frame(Vec::new());
        d.until("preview", |a| {
            matches!(a.preview.state, crate::preview::State::Ready(_))
        });
        d.frame(Vec::new());
        let selected = if name == "metadata-copy" {
            dir.join(filename).display().to_string()
        } else {
            selected.to_string()
        };
        let rect = d.texts.iter().find(|(t, _)| t == &selected).unwrap().1;
        d.drag(
            rect.left_top() + vec2(0.0, 2.0),
            rect.right_bottom() + vec2(2.0, -2.0),
            |_| {},
        );
        d.frame(vec![Event::Copy]);
        assert!(
            d.commands
                .iter()
                .any(|c| matches!(c, egui::OutputCommand::CopyText(t) if t == &selected)),
            "{name}: {:?}",
            d.commands
        );
        assert!(d.app.clip.is_none());
        fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn dragging_over_preview_link_selects_without_opening() {
    let dir = sandbox("link-select");
    let text = "https://example.com/hello";
    fs::write(dir.join("a.txt"), text).unwrap();
    let mut d = Driver::new(dir.clone());
    d.until("listing", |a| !a.tab().loading);
    d.app.tab_mut().select_path(&dir.join("a.txt"));
    d.frame(Vec::new());
    d.until("preview", |a| {
        matches!(
            a.preview.state,
            crate::preview::State::Ready(crate::preview::Content::Text { .. })
        )
    });
    d.frame(Vec::new());
    let rect = d.texts.iter().find(|(t, _)| t == text).unwrap().1;
    d.drag(
        rect.left_center(),
        rect.right_center() + vec2(1.0, 0.0),
        |_| {},
    );
    assert!(d.opened_urls.is_empty());
    d.frame(vec![Event::Copy]);
    assert!(
        d.commands
            .iter()
            .any(|c| matches!(c, egui::OutputCommand::CopyText(t) if t == text)),
        "{:?}",
        d.commands
    );
    // Opening the path editor with the keyboard must replace the label selection.
    d.app.run_cmd(Cmd::EditPath);
    d.frame(Vec::new());
    d.frame(Vec::new());
    let id = egui::Id::new(("path_edit", 0usize));
    let mut state = egui::TextEdit::load_state(&d.ctx, id).unwrap();
    state
        .cursor
        .set_char_range(Some(egui::text::CCursorRange::two(
            egui::text::CCursor::new(1),
            egui::text::CCursor::new(4),
        )));
    state.store(&d.ctx, id);
    d.frame(vec![Event::Copy]);
    let copies: Vec<_> = d
        .commands
        .iter()
        .filter_map(|c| match c {
            egui::OutputCommand::CopyText(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(copies, ["tmp"]);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn large_preview_keeps_one_layout_during_resize_and_reflows_after_settling() {
    let ctx = egui::Context::default();
    let text = "const message = \"Привет world\"; // a long preview source line\n".repeat(3000);
    let job =
        egui::text::LayoutJob::simple(text, FontId::monospace(12.5), Color32::WHITE, f32::INFINITY);
    let mut cache = crate::preview::CodeLayout::default();
    let mut time = 0.0;
    let mut draw = |width: f32, events: Vec<Event>, cache: &mut crate::preview::CodeLayout| {
        time += 1.0 / 60.0;
        let mut out = ctx.run_ui(
            RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(width, 700.0))),
                time: Some(time),
                events,
                ..Default::default()
            },
            |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    super::ui::preview_code_cached(ui, &job, &[], cache)
                });
            },
        );
        out.textures_delta.clear();
    };
    draw(650.0, vec![], &mut cache);
    let initial = cache.chunks.as_ref().unwrap().chunks[0]
        .galley
        .as_ref()
        .unwrap()
        .clone();
    assert_eq!(cache.builds, 1);
    draw(
        650.0,
        vec![Event::PointerButton {
            pos: egui::pos2(2.0, 2.0),
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        }],
        &mut cache,
    );
    for i in 1..=40 {
        draw(650.0 - i as f32 * 5.0, vec![], &mut cache);
    }
    assert_eq!(
        cache.builds, 1,
        "dragging should not re-layout the document"
    );
    assert!(std::sync::Arc::ptr_eq(
        &initial,
        cache.chunks.as_ref().unwrap().chunks[0]
            .galley
            .as_ref()
            .unwrap()
    ));
    draw(
        450.0,
        vec![Event::PointerButton {
            pos: egui::pos2(2.0, 2.0),
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }],
        &mut cache,
    );
    for _ in 0..10 {
        draw(450.0, vec![], &mut cache);
    }
    assert_eq!(cache.builds, 2, "resize should perform one final reflow");
    assert!(
        cache.chunks.as_ref().unwrap().chunks[0]
            .galley
            .as_ref()
            .unwrap()
            .size()
            .x
            <= cache.width
    );
    for _ in 0..10 {
        draw(450.0, vec![], &mut cache);
    }
    assert_eq!(
        cache.builds, 2,
        "idle frames should reuse the prepared layout"
    );
    // Window-manager resizing may not send a pressed button to the app either.
    for i in 1..=20 {
        draw(450.0 + i as f32 * 5.0, vec![], &mut cache);
    }
    assert_eq!(cache.builds, 2);
    for _ in 0..10 {
        draw(550.0, vec![], &mut cache);
    }
    assert_eq!(cache.builds, 3);
}

#[test]
fn preview_divider_drag_does_not_relayout_large_files_or_change_the_preview_request() {
    let dir = sandbox("preview-resize-work");
    let path = dir.join("large.js");
    fs::write(
        &path,
        "function example() { return \"hello\"; }\n".repeat(5000),
    )
    .unwrap();
    let mut d = Driver::new(dir.clone());
    d.until("listing", |a| !a.tab().loading);
    d.app.tab_mut().select_path(&path);
    d.until("prepared preview", |a| matches!(&a.preview.state, crate::preview::State::Ready(crate::preview::Content::Text { layout, .. }) if layout.chunks.is_some()));
    for _ in 0..10 {
        d.frame(Vec::new());
    }
    let builds = |d: &Driver| match &d.app.preview.state {
        crate::preview::State::Ready(crate::preview::Content::Text { layout, .. }) => layout.builds,
        _ => panic!("unexpected preview reload"),
    };
    let initial = builds(&d);
    let edge = d.app.pane_rects[0].right();
    d.drag(
        egui::pos2(edge, 400.0),
        egui::pos2(edge - 140.0, 400.0),
        |d| {
            assert_eq!(
                builds(d),
                initial,
                "divider drag laid out the large document"
            );
            assert_eq!(d.app.preview.path.as_deref(), Some(path.as_path()));
        },
    );
    for _ in 0..12 {
        d.frame(Vec::new());
    }
    assert_eq!(builds(&d), initial + 1);
    let replacement = "// UPDATED PREVIEW\nfunction updated() { return 42; }\n".repeat(1000);
    fs::write(&path, &replacement).unwrap();
    d.until("reloaded document geometry", |a| matches!(&a.preview.state,
        crate::preview::State::Ready(crate::preview::Content::Text { job, layout, .. }) if job.text == replacement && layout.chunks.is_some()));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "manual preview resize timing; optional KESTREL_PREVIEW_BENCH_FILE"]
fn benchmark_large_preview_resize() {
    use std::io::Read;
    let text = if let Some(path) = std::env::var_os("KESTREL_PREVIEW_BENCH_FILE") {
        let mut bytes = Vec::new();
        fs::File::open(path)
            .unwrap()
            .take(256 * 1024)
            .read_to_end(&mut bytes)
            .unwrap();
        String::from_utf8_lossy(&bytes).replace('\t', "    ")
    } else {
        "function example(value) { return \"hello\" + value; }\n".repeat(5000)
    };
    let parse = Instant::now();
    let job = crate::preview::highlight_text(
        Path::new("example.js"),
        &text,
        3000,
        FontId::monospace(12.5),
    )
    .0;
    let parse_ms = parse.elapsed().as_secs_f64() * 1000.0;
    let run = |legacy: bool| {
        let ctx = egui::Context::default();
        let mut cache = crate::preview::CodeLayout::default();
        let mut drag = Vec::new();
        let mut settle = Vec::new();
        for i in 0..70 {
            // Warm up the initial geometry, drag for 30 frames, then settle.
            let width = if i < 15 {
                650.0
            } else if i < 45 {
                650.0 - (i - 15) as f32 * 5.0
            } else {
                505.0
            };
            let events = match i {
                15 => vec![Event::PointerButton {
                    pos: egui::pos2(2.0, 2.0),
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers: Modifiers::NONE,
                }],
                45 => vec![Event::PointerButton {
                    pos: egui::pos2(2.0, 2.0),
                    button: PointerButton::Primary,
                    pressed: false,
                    modifiers: Modifiers::NONE,
                }],
                _ => vec![],
            };
            let start = Instant::now();
            let mut out = ctx.run_ui(
                RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(width, 700.0))),
                    time: Some(i as f64 / 60.0),
                    events,
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        if legacy {
                            let mut whole = job.clone();
                            whole.wrap.max_width = (ui.available_width() - 45.0).max(1.0);
                            let galley = ui.fonts_mut(|f| f.layout_job(whole));
                            ui.painter()
                                .galley(ui.min_rect().min, galley, Color32::WHITE);
                        } else {
                            super::ui::preview_code_cached(ui, &job, &[], &mut cache);
                        }
                    });
                },
            );
            // Include CPU mesh preparation, not just layout. No GPU/window is
            // created by this benchmark, so it does not measure GPU presentation.
            let _meshes = ctx.tessellate(std::mem::take(&mut out.shapes), out.pixels_per_point);
            out.textures_delta.clear();
            let ms = start.elapsed().as_secs_f64() * 1000.0;
            if (16..45).contains(&i) {
                drag.push(ms);
            }
            if i >= 45 {
                settle.push(ms);
            }
        }
        (
            drag.iter().sum::<f64>() / drag.len() as f64,
            settle.into_iter().fold(0.0f64, f64::max),
        )
    };
    let legacy = run(true);
    let chunks = run(false);
    println!(
        "{} bytes, {} syntax sections; parse {:.2}ms; CPU layout + tessellation: whole-document drag {:.2}ms/frame, chunked drag {:.2}ms/frame; worst settle frame whole {:.2}ms, chunked {:.2}ms",
        job.text.len(),
        job.sections.len(),
        parse_ms,
        legacy.0,
        chunks.0,
        legacy.1,
        chunks.1
    );
}

#[test]
fn preview_wraps_to_width_with_logical_line_numbers() {
    let ctx = egui::Context::default();
    let text = format!("{}\nsecond line", "Привет world ".repeat(30));
    let job = egui::text::LayoutJob::simple(
        text.clone(),
        FontId::monospace(12.5),
        Color32::WHITE,
        f32::INFINITY,
    );
    let draw = |width| {
        let mut out = ctx.run_ui(
            RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(width, 1200.0))),
                ..Default::default()
            },
            |ui| {
                egui::CentralPanel::default().show(ui, |ui| super::ui::preview_code(ui, &job, &[]));
            },
        );
        out.textures_delta.clear();
        let labels: Vec<_> = out
            .shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::epaint::Shape::Text(t) => Some(t),
                _ => None,
            })
            .collect();
        let code = labels.iter().find(|t| t.galley.text() == text).unwrap();
        assert!(
            code.galley.size().x <= width - 16.0 - 40.0,
            "overflow: {:?}",
            code.galley.size()
        );
        assert_eq!(labels.iter().filter(|t| t.galley.text() == "1").count(), 1);
        assert_eq!(labels.iter().filter(|t| t.galley.text() == "2").count(), 1);
        assert!(
            !labels.iter().any(|t| t.galley.text() == "3"),
            "soft wraps were numbered as source lines"
        );
        code.galley.rows.len()
    };
    let narrow = draw(260.0);
    let wide = draw(500.0);
    assert!(
        narrow > wide && wide > 2,
        "rows: narrow={narrow}, wide={wide}"
    );
}

#[test]
fn wrapped_links_are_clickable_and_copy_without_soft_newlines() {
    let ctx = egui::Context::default();
    let url = format!("https://example.com/{}", "long-path-".repeat(10));
    let job = egui::text::LayoutJob::simple(
        url.clone(),
        FontId::monospace(12.5),
        Color32::WHITE,
        f32::INFINITY,
    );
    let links = crate::preview::text_links(&url);
    let mut time = 0.0;
    let mut run = |events| {
        time += 1.0 / 60.0;
        let mut out = ctx.run_ui(
            RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(200.0, 700.0))),
                time: Some(time),
                events,
                ..Default::default()
            },
            |ui| {
                egui::CentralPanel::default()
                    .show(ui, |ui| super::ui::preview_text(ui, &job, &links));
            },
        );
        out.textures_delta.clear();
        let code = out
            .shapes
            .iter()
            .find_map(|c| match &c.shape {
                egui::epaint::Shape::Text(t) if t.galley.text() == url => Some(t),
                _ => None,
            })
            .unwrap();
        assert!(code.galley.rows.len() > 1);
        let row = &code.galley.rows[1];
        let link_at = code.pos + row.pos.to_vec2() + vec2(8.0, row.size.y / 2.0);
        let start = code.pos + vec2(0.0, code.galley.rows[0].size.y / 2.0);
        let last = code.galley.rows.last().unwrap();
        let end = code.pos + last.pos.to_vec2() + vec2(last.size.x + 1.0, last.size.y / 2.0);
        (out.platform_output, link_at, start, end)
    };
    let (_, link_at, start, end) = run(Vec::new());
    let button = |pos, pressed| Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    run(vec![Event::PointerMoved(link_at)]);
    run(vec![button(link_at, true)]);
    let (out, ..) = run(vec![button(link_at, false)]);
    assert!(
        out.commands
            .iter()
            .any(|c| matches!(c, egui::OutputCommand::OpenUrl(open) if open.url == url))
    );
    run(vec![Event::PointerMoved(start)]);
    run(vec![button(start, true)]);
    run(vec![Event::PointerMoved(end)]);
    let (out, ..) = run(vec![button(end, false)]);
    assert!(
        !out.commands
            .iter()
            .any(|c| matches!(c, egui::OutputCommand::OpenUrl(_)))
    );
    let (out, ..) = run(vec![Event::Copy]);
    assert!(
        out.commands
            .iter()
            .any(|c| matches!(c, egui::OutputCommand::CopyText(t) if t == &url)),
        "{:?}",
        out.commands
    );
}

#[test]
fn folder_sizes_sort_refresh_and_update_passive_columns() {
    let dir = sandbox("folder-sizes");
    let small = dir.join("small");
    let large = dir.join("large");
    fs::create_dir_all(small.join("nested")).unwrap();
    fs::create_dir_all(&large).unwrap();
    fs::write(small.join("nested/data"), vec![0; 11]).unwrap();
    fs::write(large.join("data"), vec![0; 29]).unwrap();
    let mut d = Driver::new(dir.clone());
    d.app.show_sidebar = false;
    d.app.show_preview = false;
    d.until("folder totals", |a| {
        !a.tab().loading && a.sizes.get(&small) == Some(&11) && a.sizes.get(&large) == Some(&29)
    });
    d.app.tab_mut().sort = SortKey::Size;
    d.app.tab_mut().ascending = true;
    d.app.refilter_all();
    let position = |a: &App, path: &Path| {
        a.tab()
            .view
            .iter()
            .position(|&i| a.tab().entries[i].path == path)
            .unwrap()
    };
    assert!(position(&d.app, &small) < position(&d.app, &large));
    // This edit is below the nonrecursive watcher. Explicit refresh must clear
    // both the descendant cache and ancestor totals before scanning again.
    fs::write(small.join("nested/data"), vec![0; 41]).unwrap();
    d.app.refresh();
    d.until("refreshed totals and sort", |a| {
        !a.tab().loading && a.sizes.get(&small) == Some(&41) && a.sizes.get(&large) == Some(&29)
    });
    assert!(position(&d.app, &large) < position(&d.app, &small));

    // Passive columns contribute size interests too, and their sort order updates
    // independently of which directory is focused.
    d.app.tab_mut().select_path(&small);
    d.app.set_view(ViewMode::Columns);
    let nested = small.join("nested");
    d.until("passive child totals", |a| {
        a.tab()
            .columns
            .as_ref()
            .is_some_and(|c| c.items.iter().any(|t| t.path == small && !t.loading))
            && a.sizes.get(&nested) == Some(&41)
    });
    // A watched immediate file edit also invalidates cached ancestor sizes.
    fs::write(small.join("new"), vec![0; 2]).unwrap();
    d.until("watched column totals", |a| {
        a.sizes.get(&small) == Some(&43) && a.sizes.get(&nested) == Some(&41)
    });
    drop(d);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn name_conflicts_ask_and_apply_the_chosen_resolution() {
    let dir = sandbox("conflicts");
    let (dest, a) = (dir.join("dest"), dir.join("a.txt"));
    fs::write(dest.join("a.txt"), "existing").unwrap();
    let mut d = Driver::new(dir.clone());
    d.until("listing", |a| a.row_rects.contains_key(&dir.join("a.txt")));

    d.app
        .drop_paths(vec![a.clone()], dest.clone(), DropMode::Copy);
    d.frame(Vec::new());
    d.frame(Vec::new());
    assert!(matches!(d.app.dialog, Some(Dialog::Conflicts(_))));
    d.click_text("Skip");
    d.frame(Vec::new());
    assert!(d.app.dialog.is_none());
    assert_eq!(fs::read_to_string(dest.join("a.txt")).unwrap(), "existing");
    assert!(!dest.join("a (2).txt").exists());

    d.app.drop_paths(vec![a], dest.clone(), DropMode::Copy);
    d.frame(Vec::new());
    d.frame(Vec::new());
    d.click_text("Keep both");
    d.until("copy", |_| dest.join("a (2).txt").exists());
    assert_eq!(fs::read_to_string(dest.join("a.txt")).unwrap(), "existing");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn permissions_dialog_changes_the_mode_and_select_pattern_marks_matches() {
    use std::os::unix::fs::PermissionsExt;
    let dir = sandbox("permissions");
    let mut d = Driver::new(dir.clone());
    d.until("listing", |a| a.row_rects.contains_key(&dir.join("a.txt")));
    fs::set_permissions(dir.join("a.txt"), fs::Permissions::from_mode(0o644)).unwrap();

    d.app.select_pattern("*.txt");
    assert_eq!(d.app.tab().targets().len(), 2);
    d.app.tab_mut().select_path(&dir.join("a.txt"));

    d.app.run_cmd(Cmd::Properties);
    d.frame(Vec::new());
    d.frame(Vec::new());
    let Some(Dialog::Permissions(edit)) = &mut d.app.dialog else {
        panic!("no permissions dialog");
    };
    assert_eq!(edit.mode, 0o644);
    edit.mode |= 0o100;
    d.click_text("Apply");
    d.until("chmod", |_| {
        fs::metadata(dir.join("a.txt"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777
            == 0o744
    });
    fs::remove_dir_all(dir).unwrap();
}

/// A point inside the preview panel's content area, between the file name heading
/// and the info rows.
fn preview_area_point(d: &Driver, name: &str) -> Pos2 {
    let heading = d
        .texts
        .iter()
        .filter(|(text, _)| text == name)
        .max_by(|a, b| a.1.center().x.total_cmp(&b.1.center().x))
        .expect("preview heading")
        .1;
    let info = d
        .texts
        .iter()
        .find(|(text, r)| text == "Size" && r.left() > heading.left() - 40.0)
        .expect("info rows")
        .1;
    Pos2::new(
        heading.center().x + 40.0,
        (heading.bottom() + info.top()) / 2.0,
    )
}

#[test]
fn image_preview_zooms_pans_and_loads_a_sharper_copy() {
    use crate::preview::{Content, Detail, State};
    let dir = sandbox("image-zoom");
    let path = dir.join("wide.png");
    image::RgbImage::from_fn(4000, 2000, |x, _| image::Rgb([(x % 256) as u8, 90, 160]))
        .save(&path)
        .unwrap();
    let mut d = Driver::new(dir.clone());
    d.until("listing", |a| !a.tab().loading);
    d.app.tab_mut().select_path(&path);
    d.frame(Vec::new());
    d.until("image preview", |a| {
        matches!(&a.preview.state, State::Ready(Content::Image { .. }))
    });
    d.frame(Vec::new());
    let view = |d: &Driver| match &d.app.preview.state {
        State::Ready(Content::Image {
            view, tex, dims, ..
        }) => (*view, tex.size(), *dims),
        _ => panic!("not an image"),
    };
    let (start, texture, dims) = view(&d);
    assert_eq!(start.scale, None, "starts fitted");
    assert_eq!(dims, (4000, 2000));
    assert!(texture[0] <= 1600, "preview texture is scaled down");

    let at = preview_area_point(&d, "wide.png");
    d.move_to(at);
    assert!(
        d.texts
            .iter()
            .any(|(t, _)| t.ends_with("% · scroll to zoom")),
        "zoom hint on hover"
    );
    let wheel = Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: vec2(0.0, 400.0),
        modifiers: Modifiers::NONE,
        phase: egui::TouchPhase::Move,
    };
    d.frame(vec![wheel]);
    for _ in 0..30 {
        d.frame(Vec::new()); // the wheel is smoothed over several frames
    }
    let zoomed = view(&d).0;
    let scale = zoomed.scale.expect("zoomed in");
    assert_ne!(
        zoomed.detail,
        Detail::None,
        "zooming past the texture asks for more pixels"
    );
    d.until("sharper copy", |a| {
        matches!(&a.preview.state, State::Ready(Content::Image { view, tex, .. })
            if view.detail == Detail::Done && tex.size()[0] > texture[0])
    });
    // The point under the pointer stays put, so zooming at the panel's left half
    // moves the view's center left of the middle.
    assert!(zoomed.center.x != 0.5 || zoomed.center.y != 0.5);

    // Dragging left reveals more of the right side.
    let before = view(&d).0.center;
    d.drag(at, at - vec2(120.0, 0.0), |_| {});
    let after = view(&d).0;
    assert!(after.center.x > before.x, "{before:?} → {after:?}");
    assert_eq!(after.scale, Some(scale));

    // Double-click returns to fitting the panel.
    d.move_to(at);
    d.button(true);
    d.button(false);
    d.button(true);
    d.button(false);
    let fitted = view(&d).0;
    assert_eq!(fitted.scale, None);
    assert_eq!(fitted.center, vec2(0.5, 0.5));
    // ... and again zooms to 100% around the pointer.
    for _ in 0..60 {
        d.frame(Vec::new()); // a second apart, or egui counts a triple click
    }
    d.button(true);
    d.button(false);
    d.button(true);
    d.button(false);
    let full = view(&d).0.scale.expect("100%");
    assert!(
        (full * d.ctx.pixels_per_point() - 1.0).abs() < 1e-3,
        "{full}"
    );
    assert!(d.texts.iter().any(|(t, _)| t == "100%"));
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn archive_preview_lists_contents_and_browses_folders() {
    use crate::preview::{Content, State};
    let dir = sandbox("archive-preview");
    let src = dir.join("pack");
    fs::create_dir_all(src.join("src/deep")).unwrap();
    fs::write(src.join("src/deep/notes.txt"), "hello").unwrap();
    fs::write(src.join("README.md"), "readme!").unwrap();
    let made = std::process::Command::new("bsdtar")
        .current_dir(&src)
        .args(["-a", "-cf", "../bundle.zip", "src", "README.md"])
        .status();
    if !made.is_ok_and(|s| s.success()) {
        return; // bsdtar isn't installed
    }
    fs::remove_dir_all(&src).unwrap();
    let mut d = Driver::new(dir.clone());
    d.until("listing", |a| !a.tab().loading);
    d.app.tab_mut().select_path(&dir.join("bundle.zip"));
    d.frame(Vec::new());
    d.until("archive preview", |a| {
        matches!(&a.preview.state, State::Ready(Content::Archive { .. }))
    });
    d.frame(Vec::new());
    let has = |d: &Driver, text: &str| d.texts.iter().any(|(t, _)| t == text);
    assert!(has(&d, "2 files, 2 folders"), "contents row");
    assert!(has(&d, "12 B"), "unpacked size row");
    assert!(has(&d, "src") && has(&d, "README.md"));
    d.click_text("src");
    d.frame(Vec::new());
    assert!(has(&d, "deep") && !has(&d, "README.md"));
    d.click_text("deep");
    d.frame(Vec::new());
    assert!(has(&d, "notes.txt"));
    assert!(has(&d, "src / deep"), "location within the archive");
    d.click_text("‹ Up");
    d.click_text("‹ Up");
    d.frame(Vec::new());
    assert!(has(&d, "README.md"));
    match &d.app.preview.state {
        State::Ready(Content::Archive { cwd, .. }) => assert!(cwd.is_empty()),
        _ => panic!("not an archive"),
    }
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn picker_rejects_directory_save_and_confirms_overwrite() {
    let dir = sandbox("picker-overwrite");
    let mut d = picker_app(&dir, crate::picker::Kind::Save, "dest");
    assert!(d.app.pick_choice().is_empty(), "cannot save over a folder");
    d.app.picker.as_mut().unwrap().name = "a.txt".into();
    d.app.pick_confirm();
    assert!(
        matches!(&d.app.dialog, Some(Dialog::ConfirmOverwrite { path }) if path == &dir.join("a.txt"))
    );
    assert!(d.app.picker.as_ref().unwrap().chosen.is_empty());
    d.frame(Vec::new());
    d.click_text("Cancel");
    assert!(d.app.dialog.is_none());
    assert!(d.app.picker.as_ref().unwrap().chosen.is_empty());
    d.app.pick_confirm();
    d.frame(Vec::new());
    d.click_text("Replace");
    assert_eq!(
        d.app.picker.as_ref().unwrap().chosen,
        vec![dir.join("a.txt")]
    );
    assert_eq!(
        fs::read_to_string(dir.join("a.txt")).unwrap(),
        "a",
        "picker never writes the file itself"
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn picker_confirms_from_name_field_with_ctrl_enter() {
    let dir = sandbox("picker-keyboard");
    let mut d = picker_app(&dir, crate::picker::Kind::Save, "new.txt");
    d.app.picker.as_mut().unwrap().focus = true;
    d.frame(Vec::new());
    assert!(d.ctx.text_edit_focused());
    d.modifiers = Modifiers::COMMAND;
    d.frame(vec![Event::Key {
        key: Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::COMMAND,
    }]);
    assert_eq!(
        d.app.picker.as_ref().unwrap().chosen,
        vec![dir.join("new.txt")]
    );
    fs::remove_dir_all(dir).unwrap();
}
