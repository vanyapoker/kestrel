//! Drives the real UI headlessly with synthetic pointer input.

use super::*;
use eframe::egui::{Event, PointerButton, Pos2, RawInput, Rect, vec2};
use std::fs;
use std::time::{Duration, Instant};

struct Driver {
    ctx: egui::Context,
    app: App,
    time: f64,
    pointer: Pos2,
    modifiers: Modifiers,
}

impl Driver {
    fn new(dir: PathBuf) -> Self {
        let ctx = egui::Context::default();
        let app = App::with_ctx(ctx.clone(), dir);
        Self { ctx, app, time: 0.0, pointer: Pos2::ZERO, modifiers: Modifiers::NONE }
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
        *self.app.row_rects.get(path).unwrap_or_else(|| panic!("no row for {}", path.display()))
    }

    fn move_to(&mut self, pos: Pos2) {
        self.pointer = pos;
        self.frame(vec![Event::PointerMoved(pos)]);
    }

    fn button(&mut self, pressed: bool) {
        let ev = Event::PointerButton { pos: self.pointer, button: PointerButton::Primary, pressed, modifiers: self.modifiers };
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

#[test]
fn drag_file_onto_folder_moves_it() {
    let dir = sandbox("move");
    let mut d = Driver::new(dir.clone());
    d.until("listing", |a| a.row_rects.contains_key(&dir.join("a.txt")));
    let (from, to) = (d.row(&dir.join("a.txt")).center(), d.row(&dir.join("dest")).center());

    d.drag(from, to, |d| {
        // While held: the payload is the file and the card targets the folder.
        let held = d.app.dragging.as_ref().expect("file should be held while dragging");
        assert_eq!(held.0, vec![dir.join("a.txt")]);
        let target = d.ctx.data(|m| m.get_temp::<PathBuf>(egui::Id::new("kestrel_drop_target")));
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
    let (from, to) = (d.row(&dir.join("b.txt")).center(), d.row(&dir.join("dest")).center());
    d.modifiers = Modifiers::CTRL;
    d.drag(from, to, |d| assert_eq!(d.app.dragging.as_ref().unwrap().0.len(), 2));
    d.modifiers = Modifiers::NONE;
    d.until("copy", |_| dir.join("dest/a.txt").exists() && dir.join("dest/b.txt").exists());
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
    assert_eq!(d.app.tab().cursor_entry().map(|e| e.name.as_str()), Some("b.txt"));
    fs::remove_dir_all(dir).unwrap();
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
    let (from, to) = (d.row(&dir.join("a.txt")).center(), d.row(&dir.join("dest")).center());
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
    assert!(dir.join("a.txt").exists(), "cancelled drag must not move the file");
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
    assert!(after.left() < before.left() - 100.0, "{before:?} -> {after:?}");
    assert!(after.right() > before.right() + 100.0, "{before:?} -> {after:?}");
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
    let key = |key| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE };
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
    d.until("navigate", |a| a.tab().path.as_os_str() == dir.join("dest").as_os_str());
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
    let key = |k: Key| Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE };
    d.modifiers = Modifiers::COMMAND;
    d.frame(vec![Event::Key { key: Key::P, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::COMMAND }]);
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
    d.frame(vec![Event::Key { key: Key::N, physical_key: None, pressed: true, repeat: false, modifiers: cs }]);
    d.frame(Vec::new());
    assert!(d.ctx.memory(|m| m.has_focus(egui::Id::new("dialog_name"))));
    fs::remove_dir_all(dir).unwrap();
}
