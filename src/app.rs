use crate::fsops::{self, Entry, Places};
use crate::preview::{Content, Preview, State};
use crate::theme::{self, *};
use crate::util::{self, Category};
use eframe::egui::{
    self, Align, Align2, CentralPanel, Color32, CornerRadius, FontId, Frame, Key, Layout, Margin,
    Modifiers, Painter, Panel, Pos2, Rect, RichText, ScrollArea, Sense, Stroke, Ui, vec2,
};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

const ROW_H: f32 = 26.0;
const PAGE_ROWS: usize = 20;

#[derive(Clone, Copy, PartialEq, Eq)]
enum SortKey {
    Name,
    Size,
    Modified,
    Kind,
}

struct Listing {
    tab_id: u64,
    generation: u64,
    result: Result<Vec<Entry>, String>,
}

struct SizeMsg {
    path: PathBuf,
    size: Option<u64>,
}

struct Tab {
    id: u64,
    path: PathBuf,
    back: Vec<PathBuf>,
    fwd: Vec<PathBuf>,
    entries: Vec<Entry>,
    /// Indices into `entries`, filtered and sorted.
    view: Vec<usize>,
    /// Index into `view`.
    selected: Option<usize>,
    filter: String,
    sort: SortKey,
    ascending: bool,
    loading: bool,
    error: Option<String>,
    generation: u64,
    cancel: Arc<AtomicBool>,
    select_after_load: Option<PathBuf>,
    scroll_to_selected: bool,
    force_offset: Option<f32>,
    scroll_offset: f32,
    viewport_h: f32,
    disk: Option<(u64, u64)>,
}

impl Tab {
    fn new(id: u64, path: PathBuf) -> Self {
        Self {
            id,
            path,
            back: Vec::new(),
            fwd: Vec::new(),
            entries: Vec::new(),
            view: Vec::new(),
            selected: None,
            filter: String::new(),
            sort: SortKey::Name,
            ascending: true,
            loading: false,
            error: None,
            generation: 0,
            cancel: Arc::new(AtomicBool::new(false)),
            select_after_load: None,
            scroll_to_selected: false,
            force_offset: None,
            scroll_offset: 0.0,
            viewport_h: 0.0,
            disk: None,
        }
    }

    fn title(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/".into())
    }

    fn selected_entry(&self) -> Option<&Entry> {
        self.selected
            .and_then(|i| self.view.get(i))
            .map(|&i| &self.entries[i])
    }

    fn select(&mut self, i: usize) {
        if !self.view.is_empty() {
            self.selected = Some(i.min(self.view.len() - 1));
            self.scroll_to_selected = true;
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let cur = self.selected.map(|s| s as isize).unwrap_or(-1);
        let next = (cur + delta).clamp(0, self.view.len() as isize - 1);
        if next >= 0 {
            self.select(next as usize);
        }
    }

    fn apply_view(&mut self, sizes: &HashMap<PathBuf, u64>, show_hidden: bool) {
        let keep = self.selected_entry().map(|e| e.path.clone());
        let needle = self.filter.to_lowercase();
        let entries = &self.entries;
        let mut view: Vec<usize> = (0..entries.len())
            .filter(|&i| {
                let e = &entries[i];
                (show_hidden || !e.hidden)
                    && (needle.is_empty() || e.name.to_lowercase().contains(&needle))
            })
            .collect();
        let size_of = |e: &Entry| {
            if e.is_dir {
                sizes.get(&e.path).copied().unwrap_or(0)
            } else {
                e.size
            }
        };
        let (sort, asc) = (self.sort, self.ascending);
        view.sort_by(|&a, &b| {
            let (x, y) = (&entries[a], &entries[b]);
            y.is_dir.cmp(&x.is_dir).then_with(|| {
                let ord = match sort {
                    SortKey::Name => util::natural_cmp(&x.name, &y.name),
                    SortKey::Size => size_of(x).cmp(&size_of(y)),
                    SortKey::Modified => x.modified.cmp(&y.modified),
                    SortKey::Kind => x.ext.cmp(&y.ext),
                };
                let ord = if asc { ord } else { ord.reverse() };
                ord.then_with(|| util::natural_cmp(&x.name, &y.name))
            })
        });
        self.view = view;
        self.selected = keep
            .and_then(|p| self.view.iter().position(|&i| entries[i].path == p))
            .or(if self.view.is_empty() { None } else { Some(0) });
    }
}

enum Action {
    Navigate(PathBuf),
    OpenEntry(usize),
    Select(usize),
    NewTab(PathBuf),
    CloseTab(usize),
    SwitchTab(usize),
    Back,
    Forward,
    Up,
    Refresh,
    Sort(SortKey),
    CopyPath(PathBuf),
    OpenTerminal(PathBuf),
    EditPath,
    ClearFilter,
}

pub struct App {
    ctx: egui::Context,
    tabs: Vec<Tab>,
    active: usize,
    next_tab_id: u64,
    list_tx: Sender<Listing>,
    list_rx: Receiver<Listing>,
    size_tx: Sender<SizeMsg>,
    size_rx: Receiver<SizeMsg>,
    sizes: HashMap<PathBuf, u64>,
    sizing: HashSet<PathBuf>,
    preview: Preview,
    places: Places,
    show_hidden: bool,
    show_preview: bool,
    path_edit: Option<String>,
    path_edit_focus: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext, start: PathBuf) -> Self {
        let ctx = cc.egui_ctx.clone();
        theme::apply(&ctx);
        let (list_tx, list_rx) = channel();
        let (size_tx, size_rx) = channel();
        let mut app = Self {
            preview: Preview::new(ctx.clone()),
            ctx,
            tabs: Vec::new(),
            active: 0,
            next_tab_id: 0,
            list_tx,
            list_rx,
            size_tx,
            size_rx,
            sizes: HashMap::new(),
            sizing: HashSet::new(),
            places: fsops::places(),
            show_hidden: false,
            show_preview: true,
            path_edit: None,
            path_edit_focus: false,
        };
        app.open_tab(start);
        app
    }

    fn tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    fn tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }

    fn open_tab(&mut self, path: PathBuf) {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        self.tabs.push(Tab::new(id, path));
        self.active = self.tabs.len() - 1;
        self.load(self.active);
    }

    fn close_tab(&mut self, i: usize) {
        if self.tabs.len() == 1 {
            self.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        let tab = self.tabs.remove(i);
        tab.cancel.store(true, Ordering::Relaxed);
        if self.active >= self.tabs.len() || self.active > i {
            self.active = self.active.saturating_sub(1);
        }
    }

    fn navigate(&mut self, path: PathBuf, push_history: bool) {
        let tab = self.tab_mut();
        if path == tab.path {
            return;
        }
        if push_history {
            let old = std::mem::replace(&mut tab.path, path);
            tab.back.push(old);
            tab.fwd.clear();
        } else {
            tab.path = path;
        }
        tab.filter.clear();
        self.load(self.active);
    }

    fn go_up(&mut self) {
        let tab = self.tab_mut();
        if let Some(parent) = tab.path.parent().map(Path::to_path_buf) {
            tab.select_after_load = Some(tab.path.clone());
            self.navigate(parent, true);
        }
    }

    fn go_back(&mut self) {
        let tab = self.tab_mut();
        if let Some(p) = tab.back.pop() {
            let cur = std::mem::replace(&mut tab.path, p);
            tab.select_after_load = Some(cur.clone());
            tab.fwd.push(cur);
            tab.filter.clear();
            self.load(self.active);
        }
    }

    fn go_forward(&mut self) {
        let tab = self.tab_mut();
        if let Some(p) = tab.fwd.pop() {
            let cur = std::mem::replace(&mut tab.path, p);
            tab.back.push(cur);
            tab.filter.clear();
            self.load(self.active);
        }
    }

    fn refresh(&mut self) {
        let path = self.tab().path.clone();
        self.sizes.retain(|p, _| !p.starts_with(&path));
        self.preview.invalidate();
        let tab = self.tab_mut();
        tab.select_after_load = tab.selected_entry().map(|e| e.path.clone());
        self.load(self.active);
    }

    /// Starts an async directory read for tab `i`.
    fn load(&mut self, i: usize) {
        let tab = &mut self.tabs[i];
        tab.generation += 1;
        tab.loading = true;
        tab.cancel.store(true, Ordering::Relaxed);
        tab.cancel = Arc::new(AtomicBool::new(false));
        let (tx, ctx) = (self.list_tx.clone(), self.ctx.clone());
        let (tab_id, generation, path) = (tab.id, tab.generation, tab.path.clone());
        std::thread::spawn(move || {
            let result = fsops::list_dir(&path);
            let _ = tx.send(Listing { tab_id, generation, result });
            ctx.request_repaint();
        });
    }

    /// Queues background size calculation for every subfolder of tab `i`.
    fn start_sizing(&mut self, i: usize) {
        let tab = &self.tabs[i];
        let Some(dev) = fsops::device_of(&tab.path) else { return };
        for e in tab.entries.iter().filter(|e| e.is_dir && !e.is_link) {
            if self.sizes.contains_key(&e.path) || self.sizing.contains(&e.path) {
                continue;
            }
            if fsops::device_of(&e.path) != Some(dev) {
                continue; // mount point: skip
            }
            self.sizing.insert(e.path.clone());
            let (path, cancel) = (e.path.clone(), tab.cancel.clone());
            let (tx, ctx) = (self.size_tx.clone(), self.ctx.clone());
            rayon::spawn(move || {
                let size = fsops::dir_size(&path, dev, &cancel);
                let _ = tx.send(SizeMsg { path, size });
                ctx.request_repaint();
            });
        }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(l) = self.list_rx.try_recv() {
            let Some(i) = self.tabs.iter().position(|t| t.id == l.tab_id) else { continue };
            let tab = &mut self.tabs[i];
            if tab.generation != l.generation {
                continue;
            }
            tab.loading = false;
            tab.disk = fsops::disk_space(&tab.path);
            tab.selected = None;
            tab.force_offset = Some(0.0);
            match l.result {
                Ok(entries) => {
                    tab.entries = entries;
                    tab.error = None;
                }
                Err(e) => {
                    tab.entries.clear();
                    tab.error = Some(e);
                }
            }
            tab.apply_view(&self.sizes, self.show_hidden);
            if let Some(p) = tab.select_after_load.take() {
                if let Some(pos) = tab.view.iter().position(|&j| tab.entries[j].path == p) {
                    tab.select(pos);
                }
            }
            self.start_sizing(i);
        }

        let mut resort = false;
        while let Ok(msg) = self.size_rx.try_recv() {
            self.sizing.remove(&msg.path);
            match msg.size {
                Some(size) => {
                    self.sizes.insert(msg.path, size);
                    resort = true;
                }
                None => {
                    // Cancelled; another open tab may still want it.
                    if let Some(parent) = msg.path.parent() {
                        if let Some(i) = self.tabs.iter().position(|t| t.path == parent) {
                            self.start_sizing(i);
                        }
                    }
                }
            }
        }
        if resort {
            for tab in self.tabs.iter_mut().filter(|t| t.sort == SortKey::Size) {
                tab.apply_view(&self.sizes, self.show_hidden);
            }
        }
        self.preview.poll(ctx);
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let mut actions = Vec::new();
        let mut filter_changed = false;
        ctx.input_mut(|i| {
            let none = Modifiers::NONE;
            let tab = &mut self.tabs[self.active];
            if i.consume_key(none, Key::ArrowDown) {
                tab.move_selection(1);
            }
            if i.consume_key(none, Key::ArrowUp) {
                tab.move_selection(-1);
            }
            if i.consume_key(none, Key::PageDown) {
                tab.move_selection(PAGE_ROWS as isize);
            }
            if i.consume_key(none, Key::PageUp) {
                tab.move_selection(-(PAGE_ROWS as isize));
            }
            if i.consume_key(none, Key::Home) {
                tab.select(0);
            }
            if i.consume_key(none, Key::End) {
                tab.select(usize::MAX);
            }
            if i.consume_key(none, Key::Enter) {
                if let Some(s) = tab.selected {
                    actions.push(Action::OpenEntry(s));
                }
            }
            if i.consume_key(none, Key::Backspace) {
                if tab.filter.pop().is_some() {
                    filter_changed = true;
                } else {
                    actions.push(Action::Up);
                }
            }
            if i.consume_key(none, Key::Escape) && !tab.filter.is_empty() {
                actions.push(Action::ClearFilter);
            }
            if i.consume_key(Modifiers::ALT, Key::ArrowLeft) {
                actions.push(Action::Back);
            }
            if i.consume_key(Modifiers::ALT, Key::ArrowRight) {
                actions.push(Action::Forward);
            }
            if i.consume_key(Modifiers::ALT, Key::ArrowUp) {
                actions.push(Action::Up);
            }
            if i.pointer.button_pressed(egui::PointerButton::Extra1) {
                actions.push(Action::Back);
            }
            if i.pointer.button_pressed(egui::PointerButton::Extra2) {
                actions.push(Action::Forward);
            }
            if i.consume_key(none, Key::F5) {
                actions.push(Action::Refresh);
            }
            if i.consume_key(none, Key::F3) {
                self.show_preview = !self.show_preview;
            }
            if i.consume_key(Modifiers::COMMAND, Key::H) {
                self.show_hidden = !self.show_hidden;
                filter_changed = true;
            }
            if i.consume_key(Modifiers::COMMAND, Key::L) {
                actions.push(Action::EditPath);
            }
            if i.consume_key(Modifiers::COMMAND, Key::T) {
                let p = tab
                    .selected_entry()
                    .filter(|e| e.is_dir)
                    .map(|e| e.path.clone())
                    .unwrap_or_else(|| tab.path.clone());
                actions.push(Action::NewTab(p));
            }
            if i.consume_key(Modifiers::COMMAND, Key::W) {
                actions.push(Action::CloseTab(self.active));
            }
            let n = self.tabs.len();
            if i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Tab) {
                actions.push(Action::SwitchTab((self.active + n - 1) % n));
            } else if i.consume_key(Modifiers::COMMAND, Key::Tab) {
                actions.push(Action::SwitchTab((self.active + 1) % n));
            }
            for (k, key) in [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9]
                .into_iter()
                .enumerate()
            {
                if i.consume_key(Modifiers::COMMAND, key) && k < n {
                    actions.push(Action::SwitchTab(k));
                }
            }
            if i.modifiers.command || i.modifiers.alt {
                return;
            }
            // Type-to-filter.
            let tab = &mut self.tabs[self.active];
            for ev in &i.events {
                if let egui::Event::Text(t) = ev {
                    tab.filter.push_str(t);
                    filter_changed = true;
                }
            }
        });
        if filter_changed {
            let (sizes, hidden) = (&self.sizes, self.show_hidden);
            for tab in &mut self.tabs {
                tab.apply_view(sizes, hidden);
            }
            self.tab_mut().scroll_to_selected = true;
        }
        for a in actions {
            self.apply(a);
        }
    }

    fn apply(&mut self, action: Action) {
        match action {
            Action::Navigate(p) => self.navigate(p, true),
            Action::OpenEntry(i) => {
                let tab = self.tab();
                let Some(&ei) = tab.view.get(i) else { return };
                let e = &tab.entries[ei];
                if e.is_dir {
                    let p = e.path.clone();
                    self.navigate(p, true);
                } else {
                    fsops::open_default(&e.path);
                }
            }
            Action::Select(i) => self.tab_mut().selected = Some(i),
            Action::NewTab(p) => self.open_tab(p),
            Action::CloseTab(i) => self.close_tab(i),
            Action::SwitchTab(i) => self.active = i.min(self.tabs.len() - 1),
            Action::Back => self.go_back(),
            Action::Forward => self.go_forward(),
            Action::Up => self.go_up(),
            Action::Refresh => self.refresh(),
            Action::Sort(key) => {
                let tab = self.tab_mut();
                if tab.sort == key {
                    tab.ascending = !tab.ascending;
                } else {
                    tab.sort = key;
                    // Size and date are more useful largest/newest first.
                    tab.ascending = matches!(key, SortKey::Name | SortKey::Kind);
                }
                let (sizes, hidden) = (&self.sizes, self.show_hidden);
                self.tabs[self.active].apply_view(sizes, hidden);
            }
            Action::CopyPath(p) => self.ctx.copy_text(p.display().to_string()),
            Action::OpenTerminal(p) => fsops::open_terminal(&p),
            Action::EditPath => {
                self.path_edit = Some(self.tab().path.display().to_string());
                self.path_edit_focus = true;
            }
            Action::ClearFilter => {
                self.tab_mut().filter.clear();
                let (sizes, hidden) = (&self.sizes, self.show_hidden);
                self.tabs[self.active].apply_view(sizes, hidden);
                self.tab_mut().scroll_to_selected = true;
            }
        }
    }

    // ---------------------------------------------------------------- UI

    fn tab_bar(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for (i, tab) in self.tabs.iter().enumerate() {
                let active = i == self.active;
                let title = tab.title();
                let font = FontId::proportional(13.0);
                let galley = ui.painter().layout_no_wrap(title, font, if active { TEXT } else { DIM });
                let w = (galley.size().x + 44.0).clamp(90.0, 220.0);
                let (rect, resp) = ui.allocate_exact_size(vec2(w, 26.0), Sense::click());
                let close_rect = Rect::from_center_size(Pos2::new(rect.right() - 13.0, rect.center().y), vec2(16.0, 16.0));
                let close = ui.interact(close_rect, ui.id().with(("tab_close", tab.id)), Sense::click());
                let p = ui.painter();
                let bg = if active {
                    SURFACE
                } else if resp.hovered() {
                    HOVER
                } else {
                    Color32::TRANSPARENT
                };
                p.rect_filled(rect, CornerRadius::same(6), bg);
                if active {
                    p.rect_filled(
                        Rect::from_min_size(Pos2::new(rect.left() + 8.0, rect.bottom() - 2.0), vec2(rect.width() - 16.0, 2.0)),
                        CornerRadius::same(1),
                        ACCENT,
                    );
                }
                draw_folder(p, Pos2::new(rect.left() + 14.0, rect.center().y), 0.8);
                let text_rect = Rect::from_min_max(Pos2::new(rect.left() + 24.0, rect.top()), Pos2::new(rect.right() - 24.0, rect.bottom()));
                p.with_clip_rect(text_rect)
                    .galley(Pos2::new(text_rect.left(), rect.center().y - galley.size().y / 2.0), galley, TEXT);
                if resp.hovered() || close.hovered() || active {
                    if close.hovered() {
                        p.rect_filled(close_rect, CornerRadius::same(4), HOVER);
                    }
                    p.text(close_rect.center(), Align2::CENTER_CENTER, "×", FontId::proportional(14.0), DIM);
                }
                if close.clicked() || resp.middle_clicked() {
                    actions.push(Action::CloseTab(i));
                } else if resp.clicked() {
                    actions.push(Action::SwitchTab(i));
                }
            }
            if glyph_button(ui, Glyph::Plus, true, false, "New tab (Ctrl+T)") {
                actions.push(Action::NewTab(self.tab().path.clone()));
            }
        });
    }

    fn toolbar(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        ui.horizontal_centered(|ui| {
            let tab = &self.tabs[self.active];
            if glyph_button(ui, Glyph::Back, !tab.back.is_empty(), false, "Back (Alt+Left)") {
                actions.push(Action::Back);
            }
            if glyph_button(ui, Glyph::Forward, !tab.fwd.is_empty(), false, "Forward (Alt+Right)") {
                actions.push(Action::Forward);
            }
            if glyph_button(ui, Glyph::Up, tab.path.parent().is_some(), false, "Up (Backspace)") {
                actions.push(Action::Up);
            }
            if glyph_button(ui, Glyph::Refresh, true, false, "Refresh (F5)") {
                actions.push(Action::Refresh);
            }
            ui.add_space(6.0);

            // Right-aligned controls are laid out first so the path bar can take the rest.
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let preview_tip = if self.show_preview { "Hide preview (F3)" } else { "Show preview (F3)" };
                if glyph_button(ui, Glyph::Panel, true, self.show_preview, preview_tip) {
                    self.show_preview = !self.show_preview;
                }
                if glyph_button(ui, Glyph::Eye, true, self.show_hidden, "Show hidden files (Ctrl+H)") {
                    self.show_hidden = !self.show_hidden;
                    let (sizes, hidden) = (&self.sizes, self.show_hidden);
                    for tab in &mut self.tabs {
                        tab.apply_view(sizes, hidden);
                    }
                }
                let tab = &self.tabs[self.active];
                if !tab.filter.is_empty() {
                    let chip = Frame::new()
                        .fill(SELECT)
                        .corner_radius(CornerRadius::same(5))
                        .inner_margin(Margin::symmetric(8, 3))
                        .show(ui, |ui| {
                            ui.label(RichText::new(format!("Filter: {}  ×", tab.filter)).color(TEXT));
                        });
                    if chip.response.interact(Sense::click()).on_hover_text("Clear filter (Esc)").clicked() {
                        actions.push(Action::ClearFilter);
                    }
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| self.path_bar(ui, actions));
            });
        });
    }

    fn path_bar(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let frame = Frame::new()
            .fill(BG)
            .corner_radius(CornerRadius::same(6))
            .stroke(Stroke::new(1.0, BORDER))
            .inner_margin(Margin::symmetric(6, 2));
        frame.show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_height(24.0);
            if let Some(text) = &mut self.path_edit {
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
                        let p = expand_tilde(text.trim());
                        if p.is_dir() {
                            actions.push(Action::Navigate(p));
                        } else if p.is_file() {
                            fsops::open_default(&p);
                        }
                    }
                    self.path_edit = None;
                }
                return;
            }
            let path = self.tabs[self.active].path.clone();
            let bg = ui.interact(ui.max_rect(), ui.id().with("crumbs_bg"), Sense::click());
            if bg.clicked() {
                actions.push(Action::EditPath);
            }
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let parts: Vec<&Path> = path.ancestors().collect();
                for (n, p) in parts.iter().rev().enumerate() {
                    let label = p
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "/".into());
                    if n > 1 {
                        ui.label(RichText::new(" › ").color(FAINT));
                    }
                    let last = n == parts.len() - 1;
                    let text = RichText::new(label).color(if last { TEXT } else { DIM });
                    let resp = ui.add(egui::Button::new(text).frame_when_inactive(false));
                    if resp.clicked() {
                        actions.push(Action::Navigate(p.to_path_buf()));
                    }
                    if resp.middle_clicked() {
                        actions.push(Action::NewTab(p.to_path_buf()));
                    }
                }
            });
        });
    }

    fn sidebar(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
            let cur = self.tabs[self.active].path.clone();
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
                    let p = ui.painter();
                    let selected = *path == cur;
                    if selected {
                        p.rect_filled(rect, CornerRadius::same(5), SELECT);
                    } else if resp.hovered() {
                        p.rect_filled(rect, CornerRadius::same(5), HOVER);
                    }
                    draw_folder(p, Pos2::new(rect.left() + 14.0, rect.center().y), 0.85);
                    p.with_clip_rect(rect.shrink(2.0)).text(
                        Pos2::new(rect.left() + 28.0, rect.center().y),
                        Align2::LEFT_CENTER,
                        name,
                        FontId::proportional(BODY),
                        if selected { TEXT } else { DIM },
                    );
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
        let tab = &self.tabs[self.active];
        ui.horizontal_centered(|ui| {
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
            let small = |s: String| RichText::new(s).size(12.0).color(DIM);
            ui.label(small(format!("{} items  ·  {dirs} folders, {files} files", tab.view.len())));
            ui.label(small(format!("·  {}{}", util::human_size(bytes), if all_known { "" } else { "+" })));
            if let Some(e) = tab.selected_entry() {
                let size = if e.is_dir { self.sizes.get(&e.path).copied() } else { Some(e.size) };
                if let Some(s) = size {
                    ui.label(small(format!("·  selected: {}", util::human_size(s))));
                }
            }
            let sizing = tab.entries.iter().any(|e| e.is_dir && self.sizing.contains(&e.path));
            if sizing {
                ui.add(egui::Spinner::new().size(11.0).color(DIM));
                ui.label(small("calculating folder sizes".into()));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some((free, total)) = tab.disk {
                    ui.label(small(format!(
                        "{} free of {}",
                        util::human_size(free),
                        util::human_size(total)
                    )));
                }
            });
        });
    }

    fn file_list(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let tab = &mut self.tabs[self.active];
        let full = ui.available_rect_before_wrap();
        let cols = Columns::new(full.left(), full.right());

        // Header.
        let (hrect, _) = ui.allocate_exact_size(vec2(full.width(), 28.0), Sense::hover());
        let header = [
            Some((SortKey::Name, "Name", (cols.name.0 - 22.0, cols.name.1), Align2::LEFT_CENTER)),
            Some((SortKey::Size, "Size", cols.size, Align2::RIGHT_CENTER)),
            cols.date.map(|c| (SortKey::Modified, "Modified", c, Align2::LEFT_CENTER)),
            cols.kind.map(|c| (SortKey::Kind, "Type", c, Align2::LEFT_CENTER)),
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
                // Sort direction arrow: left of right-aligned labels, right of the others.
                let ax = if right { text_rect.left() - 8.0 } else { text_rect.right() + 8.0 };
                let (a, b) = if tab.ascending { (-2.0, 2.0) } else { (2.0, -2.0) };
                let c = Pos2::new(ax, r.center().y);
                ui.painter().line(
                    vec![c + vec2(-3.5, -a), c + vec2(0.0, -b), c + vec2(3.5, -a)],
                    Stroke::new(1.4, ACCENT),
                );
            }
            if resp.clicked() {
                actions.push(Action::Sort(key));
            }
        }
        ui.painter().hline(full.x_range(), hrect.bottom(), Stroke::new(1.0, BORDER));

        if tab.view.is_empty() {
            let msg = if let Some(e) = &tab.error {
                format!("Cannot open folder: {e}")
            } else if tab.loading {
                "Loading…".into()
            } else if !tab.filter.is_empty() {
                format!("No items match \"{}\"", tab.filter)
            } else {
                "This folder is empty".into()
            };
            ui.add_space(40.0);
            ui.vertical_centered(|ui| ui.label(RichText::new(msg).color(DIM)));
            return;
        }

        let mut scroll = ScrollArea::vertical().auto_shrink([false; 2]).id_salt(("list", tab.id));
        let base = tab.force_offset.take();
        if tab.scroll_to_selected || base.is_some() {
            let mut off = base.unwrap_or(tab.scroll_offset);
            if tab.scroll_to_selected {
                if let Some(sel) = tab.selected {
                    let top = sel as f32 * ROW_H;
                    let h = tab.viewport_h.max(ROW_H);
                    if top < off {
                        off = top;
                    } else if top + ROW_H > off + h {
                        off = top + ROW_H - h;
                    }
                }
                tab.scroll_to_selected = false;
            }
            scroll = scroll.vertical_scroll_offset(off);
        }

        let sizes = &self.sizes;
        let sizing = &self.sizing;
        let out = scroll.show_rows(ui, ROW_H, tab.view.len(), |ui, range| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for vi in range {
                let e = &tab.entries[tab.view[vi]];
                let (rect, resp) = ui.allocate_exact_size(vec2(full.width(), ROW_H), Sense::click());
                let p = ui.painter();
                let selected = tab.selected == Some(vi);
                let row = rect.shrink2(vec2(4.0, 1.0));
                if selected {
                    p.rect_filled(row, CornerRadius::same(5), SELECT);
                } else if resp.hovered() {
                    p.rect_filled(row, CornerRadius::same(5), HOVER);
                }
                let cy = rect.center().y;
                let cat = util::category(&e.ext, e.is_dir);
                draw_icon(p, Pos2::new(cols.name.0 - 12.0, cy), cat);
                let name_color = if e.hidden { DIM } else { TEXT };
                p.with_clip_rect(Rect::from_x_y_ranges(cols.name.0..=cols.name.1, rect.y_range())).text(
                    Pos2::new(cols.name.0, cy),
                    Align2::LEFT_CENTER,
                    &e.name,
                    FontId::proportional(BODY),
                    name_color,
                );
                if e.is_link {
                    p.text(Pos2::new(cols.name.0 - 6.0, cy + 6.0), Align2::CENTER_CENTER, "↗", FontId::proportional(9.0), ACCENT);
                }
                let size_text = if e.is_dir {
                    match sizes.get(&e.path) {
                        Some(s) => util::human_size(*s),
                        None if sizing.contains(&e.path) => "…".into(),
                        None => String::new(),
                    }
                } else {
                    util::human_size(e.size)
                };
                let small = FontId::proportional(12.5);
                p.text(Pos2::new(cols.size.1, cy), Align2::RIGHT_CENTER, size_text, small.clone(), DIM);
                if let (Some(m), Some(date)) = (e.modified, cols.date) {
                    p.text(Pos2::new(date.0, cy), Align2::LEFT_CENTER, util::fmt_time(m), small.clone(), DIM);
                }
                let kind = if e.is_dir {
                    "Folder".to_string()
                } else if e.ext.is_empty() {
                    "File".to_string()
                } else {
                    e.ext.to_uppercase()
                };
                if let Some(k) = cols.kind {
                    p.with_clip_rect(Rect::from_x_y_ranges(k.0..=k.1, rect.y_range()))
                        .text(Pos2::new(k.0, cy), Align2::LEFT_CENTER, kind, small, FAINT);
                }

                if resp.double_clicked() {
                    actions.push(Action::OpenEntry(vi));
                } else if resp.clicked() || resp.secondary_clicked() {
                    actions.push(Action::Select(vi));
                }
                if resp.middle_clicked() && e.is_dir {
                    actions.push(Action::NewTab(e.path.clone()));
                }
                resp.context_menu(|ui| {
                    if ui.button("Open").clicked() {
                        actions.push(Action::OpenEntry(vi));
                        ui.close();
                    }
                    if e.is_dir && ui.button("Open in new tab").clicked() {
                        actions.push(Action::NewTab(e.path.clone()));
                        ui.close();
                    }
                    if ui.button("Copy path").clicked() {
                        actions.push(Action::CopyPath(e.path.clone()));
                        ui.close();
                    }
                    let term_dir = if e.is_dir { e.path.clone() } else { tab.path.clone() };
                    if ui.button("Open terminal here").clicked() {
                        actions.push(Action::OpenTerminal(term_dir));
                        ui.close();
                    }
                });
            }
        });
        tab.scroll_offset = out.state.offset.y;
        tab.viewport_h = out.inner_rect.height();
    }

    fn preview_panel(&mut self, ui: &mut Ui) {
        let tab = &self.tabs[self.active];
        let Some(e) = tab.selected_entry() else {
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

        // Metadata at the bottom, content fills the middle.
        let mut rows: Vec<(&str, String)> = Vec::new();
        let size = if e.is_dir { self.sizes.get(&e.path).copied() } else { Some(e.size) };
        rows.push(("Size", size.map(util::human_size).unwrap_or_else(|| "calculating…".into())));
        if let Some(m) = e.modified {
            rows.push(("Modified", util::fmt_time(m)));
        }
        rows.push(("Permissions", util::fmt_mode(e.mode, e.is_dir)));
        if let State::Ready(c) = &self.preview.state {
            match c {
                Content::Image { w, h, .. } => rows.push(("Dimensions", format!("{w} × {h}"))),
                Content::Text { lines, truncated, .. } => {
                    rows.push(("Lines", format!("{lines}{}", if *truncated { "+" } else { "" })))
                }
                Content::Dir { items: Some(n) } => rows.push(("Items", n.to_string())),
                _ => {}
            }
        }
        rows.push(("Path", e.path.display().to_string()));

        Panel::bottom("preview_info")
            .frame(Frame::new().inner_margin(Margin::symmetric(0, 8)))
            .resizable(false)
            .show_separator_line(true)
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
                    egui::Image::from_texture(egui::load::SizedTexture::new(tex.id(), size)).paint_at(ui, r);
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
            State::Ready(Content::Binary) => {
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
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll(ctx);
        self.handle_keys(ctx);
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let mut actions = Vec::new();
        let bar = |m: (i8, i8)| Frame::new().fill(BG).inner_margin(Margin::symmetric(m.0, m.1));

        Panel::top("tabs")
            .exact_size(36.0)
            .frame(bar((8, 4)))
            .show_separator_line(false)
            .show(ui, |ui| self.tab_bar(ui, &mut actions));
        Panel::top("toolbar")
            .exact_size(38.0)
            .frame(Frame::new().fill(PANEL).inner_margin(Margin::symmetric(8, 4)))
            .show(ui, |ui| self.toolbar(ui, &mut actions));
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
                .default_size(380.0)
                .min_size(220.0)
                .frame(Frame::new().fill(PANEL).inner_margin(Margin::symmetric(12, 4)))
                .show(ui, |ui| self.preview_panel(ui));
        }
        CentralPanel::default()
            .frame(Frame::new().fill(BG).inner_margin(Margin::symmetric(4, 0)))
            .show(ui, |ui| self.file_list(ui, &mut actions));

        for a in actions {
            self.apply(a);
        }

        if self.show_preview {
            let sel = self.tab().selected_entry().map(|e| (e.path.clone(), e.is_dir));
            self.preview.request(sel.as_ref().map(|(p, d)| (p.as_path(), *d)));
        }
        let title = format!("{} — fpilot", self.tab().path.display());
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Title(title));
    }
}

// ---------------------------------------------------------------- helpers

struct Columns {
    name: (f32, f32),
    size: (f32, f32),
    date: Option<(f32, f32)>,
    kind: Option<(f32, f32)>,
}

impl Columns {
    /// Lays out columns right-to-left, dropping Type and then Modified when narrow.
    fn new(left: f32, right: f32) -> Self {
        let width = right - left;
        let mut x = right - 8.0;
        let kind = (width > 560.0).then(|| {
            let r = (x - 70.0, x);
            x = r.0;
            r
        });
        let date = (width > 440.0).then(|| {
            let r = (x - 130.0, x - 12.0);
            x = r.0;
            r
        });
        let size = (x - 96.0, x - 20.0);
        let name = (left + 34.0, size.0 - 8.0);
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
}

fn glyph_button(ui: &mut Ui, glyph: Glyph, enabled: bool, on: bool, tip: &str) -> bool {
    let resp = ui
        .add_enabled(
            enabled,
            egui::Button::new("").frame_when_inactive(false).min_size(vec2(28.0, 26.0)),
        )
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
    }
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
    p.rect_stroke(r, radius, Stroke::new(1.0 * s.sqrt(), color), egui::StrokeKind::Inside);
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
        let home = fsops::home_dir();
        return home.join(rest.trim_start_matches('/'));
    }
    PathBuf::from(s)
}

