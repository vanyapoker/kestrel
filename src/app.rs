mod icons;
mod ui;
#[cfg(test)]
mod ui_tests;

use crate::fsops::{self, Entry, Places};
use crate::ops::{self, Job, JobKind, Ops};
use crate::palette::{Cmd, Palette};
use crate::preview::Preview;
use crate::search::{self, SearchMsg};
use crate::tab::{Search, SortKey, Tab, ViewMode};
use crate::theme;
use crate::thumbs::Thumbs;
use crate::watch::Watcher;
use eframe::egui::{self, Event, InputState, Key, Modifiers};
use std::collections::{HashMap, HashSet};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

const PAGE_ROWS: isize = 20;
const WATCH_DEBOUNCE: Duration = Duration::from_millis(150);
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(120);

struct Listing {
    tab_id: u64,
    generation: u64,
    soft: bool,
    result: Result<Vec<Entry>, String>,
}

struct SizeMsg {
    path: PathBuf,
    size: Option<u64>,
}

pub struct Pane {
    tabs: Vec<Tab>,
    active: usize,
}

struct Clip {
    paths: Vec<PathBuf>,
    cut: bool,
    /// What we put on the system clipboard, to tell our copy from someone else's.
    text: String,
}

enum Dialog {
    Rename { path: PathBuf, name: String, focus: bool },
    Create { dir: PathBuf, name: String, folder: bool, focus: bool },
    ConfirmDelete { paths: Vec<PathBuf> },
}

struct Toast {
    text: String,
    error: bool,
    until: Instant,
}

/// Drag-and-drop payload.
struct DragPaths(Vec<PathBuf>);

enum Action {
    Navigate(PathBuf),
    OpenEntry(usize),
    Click { vi: usize, ctrl: bool, shift: bool },
    NewTab(PathBuf),
    CloseTab(usize, usize),
    SwitchTab(usize, usize),
    FocusPane(usize),
    Sort(SortKey),
    CopyPath(PathBuf),
    OpenTerminal(PathBuf),
    ClearFilter,
    Cmd(Cmd),
    Drop { paths: Vec<PathBuf>, dest: PathBuf, copy: bool },
    Reveal(PathBuf),
    StopSearch,
    /// Step the active tab's view mode (Ctrl+wheel).
    Zoom(i32),
    ToggleSection(&'static str),
    Bookmark(Vec<PathBuf>),
    Unbookmark(PathBuf),
}

pub struct App {
    ctx: egui::Context,
    panes: Vec<Pane>,
    active_pane: usize,
    next_tab_id: u64,
    list_tx: Sender<Listing>,
    list_rx: Receiver<Listing>,
    size_tx: Sender<SizeMsg>,
    size_rx: Receiver<SizeMsg>,
    search_tx: Sender<SearchMsg>,
    search_rx: Receiver<SearchMsg>,
    sizes: HashMap<PathBuf, u64>,
    sizing: HashSet<PathBuf>,
    preview: Preview,
    thumbs: Thumbs,
    watcher: Watcher,
    /// Follows `omarchy theme set`; kept alive for its notifications.
    theme_watch: Option<(notify::RecommendedWatcher, std::sync::mpsc::Receiver<()>)>,
    theme_reload_at: Option<Instant>,
    ops: Ops,
    places: Places,
    show_hidden: bool,
    show_preview: bool,
    show_sidebar: bool,
    /// Sidebar sections folded shut by clicking their header.
    collapsed: HashSet<&'static str>,
    /// Fractional Ctrl+wheel steps not yet applied to the view mode.
    zoom_accum: f32,
    path_edit: Option<(usize, String)>,
    path_edit_focus: bool,
    completer: crate::complete::Completer,
    /// Highlighted path completion, an index into the dropdown.
    path_hl: Option<usize>,
    search_focus: bool,
    clip: Option<Clip>,
    dialog: Option<Dialog>,
    palette: Palette,
    toasts: Vec<Toast>,
    /// Screen rects of the panes from the last frame, for external drops.
    pane_rects: Vec<egui::Rect>,
    pending_paste: Option<String>,
    /// Payload of the drag in progress, refreshed every frame.
    dragging: Option<Arc<DragPaths>>,
    window_title: String,
    /// System drag and drop (Wayland only; `None` in tests and on X11).
    wl: Option<crate::wl_dnd::WlDnd>,
    wl_tried: bool,
    /// Pointer position of a drag from another app hovering us.
    ext_hover: Option<egui::Pos2>,
    /// Send egui a synthetic button release (after handing a drag to the compositor).
    inject_release: bool,
    /// Folders drawn last frame, for resolving external drops.
    folder_rects: Vec<(egui::Rect, PathBuf)>,
    /// Where each visible row was drawn last frame (for UI tests).
    #[cfg(test)]
    row_rects: HashMap<PathBuf, egui::Rect>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext, start: PathBuf) -> Self {
        Self::with_ctx(cc.egui_ctx.clone(), start)
    }

    fn with_ctx(ctx: egui::Context, start: PathBuf) -> Self {
        theme::apply(&ctx);
        let (list_tx, list_rx) = channel();
        let (size_tx, size_rx) = channel();
        let (search_tx, search_rx) = channel();
        let mut app = Self {
            preview: Preview::new(ctx.clone()),
            thumbs: Thumbs::new(ctx.clone()),
            watcher: Watcher::new(ctx.clone()),
            theme_watch: theme::watch(ctx.clone()),
            theme_reload_at: None,
            ops: Ops::new(ctx.clone()),
            ctx,
            panes: vec![Pane { tabs: Vec::new(), active: 0 }],
            active_pane: 0,
            next_tab_id: 0,
            list_tx,
            list_rx,
            size_tx,
            size_rx,
            search_tx,
            search_rx,
            sizes: HashMap::new(),
            sizing: HashSet::new(),
            places: fsops::places(),
            show_hidden: false,
            show_preview: true,
            show_sidebar: true,
            collapsed: HashSet::new(),
            zoom_accum: 0.0,
            path_edit: None,
            path_edit_focus: false,
            completer: Default::default(),
            path_hl: None,
            search_focus: false,
            clip: None,
            dialog: None,
            palette: Palette::default(),
            toasts: Vec::new(),
            pane_rects: Vec::new(),
            pending_paste: None,
            dragging: None,
            window_title: String::new(),
            wl: None,
            wl_tried: false,
            ext_hover: None,
            inject_release: false,
            folder_rects: Vec::new(),
            #[cfg(test)]
            row_rects: HashMap::new(),
        };
        app.open_tab(start);
        app
    }

    // ------------------------------------------------------------ access

    fn tab(&self) -> &Tab {
        let pane = &self.panes[self.active_pane];
        &pane.tabs[pane.active]
    }

    fn tab_mut(&mut self) -> &mut Tab {
        let pane = &mut self.panes[self.active_pane];
        &mut pane.tabs[pane.active]
    }

    fn tabs_mut(&mut self) -> impl Iterator<Item = &mut Tab> {
        self.panes.iter_mut().flat_map(|p| p.tabs.iter_mut())
    }

    fn tab_by_id(&mut self, id: u64) -> Option<&mut Tab> {
        self.tabs_mut().find(|t| t.id == id)
    }

    fn refilter_all(&mut self) {
        let (sizes, hidden) = (&self.sizes, self.show_hidden);
        for tab in self.panes.iter_mut().flat_map(|p| p.tabs.iter_mut()) {
            tab.apply_view(sizes, hidden);
        }
    }

    fn toast(&mut self, text: impl Into<String>, error: bool) {
        let secs = if error { 6 } else { 3 };
        self.toasts.push(Toast { text: text.into(), error, until: Instant::now() + Duration::from_secs(secs) });
        self.ctx.request_repaint_after(Duration::from_secs(secs));
    }

    // ------------------------------------------------------------ tabs & navigation

    fn new_tab(&mut self, path: PathBuf) -> Tab {
        let mut tab = Tab::new(self.next_tab_id, path);
        self.next_tab_id += 1;
        start_listing(&mut tab, &self.list_tx, &self.ctx, false);
        tab
    }

    fn open_tab(&mut self, path: PathBuf) {
        let tab = self.new_tab(path);
        let pane = &mut self.panes[self.active_pane];
        pane.tabs.push(tab);
        pane.active = pane.tabs.len() - 1;
    }

    fn close_tab(&mut self, p: usize, i: usize) {
        let pane = &mut self.panes[p];
        if pane.tabs.len() == 1 {
            if self.panes.len() > 1 {
                self.close_split(p);
            } else {
                self.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            return;
        }
        let tab = pane.tabs.remove(i);
        tab.cancel.store(true, Ordering::Relaxed);
        if pane.active >= pane.tabs.len() || pane.active > i {
            pane.active = pane.active.saturating_sub(1);
        }
    }

    fn toggle_split(&mut self) {
        if self.panes.len() > 1 {
            self.close_split(1 - self.active_pane);
        } else {
            let tab = self.new_tab(self.tab().path.clone());
            self.panes.push(Pane { tabs: vec![tab], active: 0 });
            self.active_pane = 1;
        }
    }

    fn close_split(&mut self, p: usize) {
        for tab in self.panes.remove(p).tabs {
            tab.cancel.store(true, Ordering::Relaxed);
        }
        self.active_pane = 0;
    }

    fn reload_active(&mut self, soft: bool) {
        let pane = &mut self.panes[self.active_pane];
        start_listing(&mut pane.tabs[pane.active], &self.list_tx, &self.ctx, soft);
    }

    fn navigate(&mut self, path: PathBuf, push_history: bool) {
        let tab = self.tab_mut();
        if path == tab.path && tab.search.is_none() {
            return;
        }
        if let Some(s) = tab.search.take() {
            s.cancel.store(true, Ordering::Relaxed);
        }
        if path != tab.path {
            if push_history {
                let old = std::mem::replace(&mut tab.path, path);
                tab.back.push(old);
                tab.fwd.clear();
            } else {
                tab.path = path;
            }
        }
        tab.filter.clear();
        tab.marked.clear();
        self.thumbs.cancel_pending();
        self.reload_active(false);
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
            let cur = tab.path.clone();
            tab.fwd.push(cur.clone());
            tab.select_after_load = Some(cur);
            self.navigate(p, false);
        }
    }

    fn go_forward(&mut self) {
        let tab = self.tab_mut();
        if let Some(p) = tab.fwd.pop() {
            let cur = tab.path.clone();
            tab.back.push(cur);
            self.navigate(p, false);
        }
    }

    fn refresh(&mut self) {
        let path = self.tab().path.clone();
        self.sizes.retain(|p, _| !p.starts_with(&path));
        self.preview.invalidate();
        let tab = self.tab_mut();
        if tab.search.is_some() {
            self.restart_search();
            return;
        }
        tab.select_after_load = tab.cursor_entry().map(|e| e.path.clone());
        self.reload_active(true);
    }

    fn start_search(&mut self) {
        let tab = self.tab_mut();
        if tab.search.is_none() {
            let query = std::mem::take(&mut tab.filter);
            tab.search = Some(Search {
                query,
                root: tab.path.clone(),
                generation: 0,
                cancel: Arc::new(AtomicBool::new(false)),
                running: false,
                truncated: false,
                restart_at: None,
            });
            self.restart_search();
        }
        self.search_focus = true;
    }

    fn restart_search(&mut self) {
        let hidden = self.show_hidden;
        let pane = &mut self.panes[self.active_pane];
        let tab = &mut pane.tabs[pane.active];
        let tab_id = tab.id;
        let Some(s) = tab.search.as_mut() else { return };
        s.cancel.store(true, Ordering::Relaxed);
        s.cancel = Arc::new(AtomicBool::new(false));
        s.generation += 1;
        s.restart_at = None;
        s.truncated = false;
        tab.entries.clear();
        tab.view.clear();
        tab.marked.clear();
        tab.cursor = None;
        tab.force_offset = Some(0.0);
        let s = tab.search.as_mut().unwrap();
        s.running = !s.query.trim().is_empty();
        if s.running {
            let req = search::Request {
                tab_id,
                generation: s.generation,
                root: s.root.clone(),
                query: s.query.clone(),
                show_hidden: hidden,
                cancel: s.cancel.clone(),
            };
            search::spawn(req, self.search_tx.clone(), self.ctx.clone());
        }
        tab.apply_view(&self.sizes, hidden);
    }

    fn stop_search(&mut self) {
        let tab = self.tab_mut();
        if let Some(s) = tab.search.take() {
            s.cancel.store(true, Ordering::Relaxed);
            tab.select_after_load = tab.cursor_entry().filter(|e| e.path.parent() == Some(&tab.path)).map(|e| e.path.clone());
            self.reload_active(false);
        }
    }

    // ------------------------------------------------------------ background results

    fn poll(&mut self, ctx: &egui::Context) {
        let now = Instant::now();

        while let Ok(l) = self.list_rx.try_recv() {
            let (sizes, hidden) = (&self.sizes, self.show_hidden);
            let Some(tab) = self.panes.iter_mut().flat_map(|p| p.tabs.iter_mut()).find(|t| t.id == l.tab_id) else {
                continue;
            };
            if tab.generation != l.generation || tab.search.is_some() {
                continue;
            }
            tab.loading = false;
            tab.disk = fsops::disk_space(&tab.path);
            if !l.soft {
                tab.cursor = None;
                tab.force_offset = Some(0.0);
            }
            // `view` indexes the old entries; selection is restored by path below.
            tab.view.clear();
            match l.result {
                Ok(entries) => {
                    tab.entries = entries;
                    tab.error = None;
                }
                Err(_) if l.soft && !tab.path.exists() => {
                    // The folder was deleted or renamed under us: fall back to the nearest parent.
                    if let Some(up) = tab.path.ancestors().find(|a| a.is_dir()).map(Path::to_path_buf) {
                        tab.select_after_load = None;
                        tab.path = up;
                        start_listing(tab, &self.list_tx, &self.ctx, false);
                    }
                    continue;
                }
                Err(e) => {
                    tab.entries.clear();
                    tab.error = Some(e);
                }
            }
            tab.apply_view(sizes, hidden);
            if let Some(p) = tab.select_after_load.take() {
                tab.select_path(&p);
            }
            let id = tab.id;
            self.start_sizing(id);
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
                    let ids: Vec<u64> = self
                        .tabs_mut()
                        .filter(|t| Some(t.path.as_path()) == msg.path.parent())
                        .map(|t| t.id)
                        .collect();
                    for id in ids {
                        self.start_sizing(id);
                    }
                }
            }
        }
        if resort {
            let (sizes, hidden) = (&self.sizes, self.show_hidden);
            for tab in self.panes.iter_mut().flat_map(|p| p.tabs.iter_mut()).filter(|t| t.sort == SortKey::Size) {
                tab.apply_view(sizes, hidden);
            }
        }

        let mut searched = HashSet::new();
        while let Ok(msg) = self.search_rx.try_recv() {
            let Some(tab) = self.tab_by_id(msg.tab_id) else { continue };
            let Some(s) = tab.search.as_mut() else { continue };
            if s.generation != msg.generation {
                continue;
            }
            if msg.done {
                s.running = false;
                s.truncated = msg.truncated;
            }
            tab.entries.extend(msg.batch);
            searched.insert(msg.tab_id);
        }
        if !searched.is_empty() {
            let (sizes, hidden) = (&self.sizes, self.show_hidden);
            for tab in self.panes.iter_mut().flat_map(|p| p.tabs.iter_mut()).filter(|t| searched.contains(&t.id)) {
                tab.apply_view(sizes, hidden);
            }
        }

        while let Ok(done) = self.ops.done_rx.try_recv() {
            self.job_done(done);
        }

        // Theme switches rewrite several files; wait for them to settle, then restyle.
        if let Some((_, rx)) = &self.theme_watch {
            if rx.try_iter().count() > 0 {
                self.theme_reload_at = Some(now + Duration::from_millis(300));
                ctx.request_repaint_after(Duration::from_millis(300));
            }
        }
        match self.theme_reload_at {
            Some(t) if now >= t => {
                self.theme_reload_at = None;
                if theme::reload(ctx) {
                    self.preview.invalidate(); // re-highlight code in the new colors
                }
            }
            // A repaint can land just short of the deadline; ask again.
            Some(t) => ctx.request_repaint_after(t - now),
            None => {}
        }

        // File watcher: debounce, then soft-reload affected tabs.
        for p in self.watcher.drain() {
            for a in p.ancestors() {
                self.sizes.remove(a);
            }
            self.thumbs.invalidate(&p);
            if self.preview.path.as_deref() == Some(&p) {
                self.preview.invalidate();
            }
            for tab in self.tabs_mut() {
                if tab.search.is_none() && (Some(tab.path.as_path()) == p.parent() || tab.path == p) {
                    tab.reload_at.get_or_insert(now + WATCH_DEBOUNCE);
                }
            }
        }
        let mut next_wake: Option<Instant> = None;
        let mut search_due = false;
        let active_id = self.tab().id;
        let (list_tx, ctx2) = (&self.list_tx, &self.ctx);
        for tab in self.panes.iter_mut().flat_map(|p| p.tabs.iter_mut()) {
            if let Some(at) = tab.reload_at {
                if at <= now {
                    tab.reload_at = None;
                    start_listing(tab, list_tx, ctx2, true);
                } else {
                    next_wake = Some(next_wake.map_or(at, |w| w.min(at)));
                }
            }
            if let Some(at) = tab.search.as_ref().and_then(|s| s.restart_at) {
                if at <= now && tab.id == active_id {
                    search_due = true;
                } else {
                    next_wake = Some(next_wake.map_or(at, |w| w.min(at)));
                }
            }
        }
        if search_due {
            self.restart_search();
        }
        if let Some(w) = next_wake {
            ctx.request_repaint_after(w.saturating_duration_since(now));
        }

        let wanted: HashSet<PathBuf> = self
            .panes
            .iter()
            .flat_map(|p| p.tabs.iter())
            .filter(|t| t.search.is_none())
            .map(|t| t.path.clone())
            .collect();
        self.watcher.sync(wanted);

        self.toasts.retain(|t| t.until > now);
        if self.ops.queued.load(Ordering::Relaxed) > 0 {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        self.thumbs.poll(ctx);
        self.preview.poll(ctx);
    }

    /// Queues background size calculation for every subfolder shown in a tab.
    fn start_sizing(&mut self, tab_id: u64) {
        let Some(tab) = self.panes.iter().flat_map(|p| p.tabs.iter()).find(|t| t.id == tab_id) else { return };
        if tab.search.is_some() {
            return;
        }
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

    fn job_done(&mut self, done: ops::Done) {
        for d in &done.dirs {
            for a in d.ancestors() {
                self.sizes.remove(a);
            }
        }
        let select = done.created.first().cloned();
        let (list_tx, ctx) = (&self.list_tx, &self.ctx);
        let active_id = {
            let pane = &self.panes[self.active_pane];
            pane.tabs[pane.active].id
        };
        for tab in self.panes.iter_mut().flat_map(|p| p.tabs.iter_mut()) {
            if tab.search.is_none() && done.dirs.contains(&tab.path) {
                if tab.id == active_id && select.as_ref().and_then(|p| p.parent()) == Some(tab.path.as_path()) {
                    tab.select_after_load = select.clone();
                }
                tab.reload_at = None;
                start_listing(tab, list_tx, ctx, true);
            }
        }
        if done.cancelled {
            self.toast("Operation cancelled", false);
        } else if let Some(first) = done.errors.first() {
            let more = if done.errors.len() > 1 { format!(" (+{} more errors)", done.errors.len() - 1) } else { String::new() };
            self.toast(format!("{first}{more}"), true);
        } else if matches!(done.kind, JobKind::Copy | JobKind::Move | JobKind::Trash | JobKind::Delete) {
            self.toast(done.summary, false);
        }
    }

    // ------------------------------------------------------------ file operations

    fn copy_to_clipboard(&mut self, cut: bool) {
        let paths = self.tab().targets();
        if paths.is_empty() {
            return;
        }
        let text = paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n");
        let on_system = self.wl.as_ref().is_some_and(|wl| wl.set_clipboard_files(&paths, cut));
        if !on_system {
            self.ctx.copy_text(text.clone());
        }
        let n = paths.len();
        self.clip = Some(Clip { paths, cut, text });
        let what = if n == 1 { "1 item".into() } else { format!("{n} items") };
        self.toast(format!("{} {what}", if cut { "Cut" } else { "Copied" }), false);
    }

    fn set_text_clipboard(&mut self, text: String) {
        if !self.wl.as_ref().is_some_and(|wl| wl.set_clipboard_text(&text)) {
            self.ctx.copy_text(text);
        }
    }

    /// `text` is the system clipboard contents, if egui delivered a paste event.
    fn paste(&mut self, text: Option<String>) {
        let Some(dest) = self.paste_target() else { return };
        if let Some(wl) = &self.wl {
            match wl.read_clipboard() {
                crate::wl_dnd::ClipRead::Ours => {}
                crate::wl_dnd::ClipRead::Files { paths, cut } => {
                    let kind = if cut { JobKind::Move } else { JobKind::Copy };
                    self.ops.submit(Job { kind, srcs: paths, dest: Some(dest) });
                    return;
                }
                crate::wl_dnd::ClipRead::Nothing => {
                    if self.clip.is_none() {
                        self.toast("Clipboard has no files to paste", false);
                        return;
                    }
                }
            }
            // Ours: the internal clipboard knows whether it was a cut.
            if let Some(clip) = self.clip.take() {
                let kind = if clip.cut { JobKind::Move } else { JobKind::Copy };
                if !clip.cut {
                    self.clip = Some(Clip { paths: clip.paths.clone(), cut: false, text: clip.text });
                }
                self.ops.submit(Job { kind, srcs: clip.paths, dest: Some(dest) });
            }
            return;
        }
        let ours = match (&self.clip, &text) {
            (Some(c), Some(t)) => c.text.trim() == t.trim(),
            (Some(_), None) => true,
            _ => false,
        };
        if ours {
            let clip = self.clip.take().unwrap();
            let kind = if clip.cut { JobKind::Move } else { JobKind::Copy };
            if !clip.cut {
                self.clip = Some(Clip { paths: clip.paths.clone(), cut: false, text: clip.text });
            }
            self.ops.submit(Job { kind, srcs: clip.paths, dest: Some(dest) });
            return;
        }
        // Files copied in another app: plain paths, file:// URIs, or a uri-list.
        let mut paths = text.as_deref().map(parse_path_list).unwrap_or_default();
        if paths.is_empty() {
            paths = wl_paste_uri_list();
        }
        if paths.is_empty() {
            self.toast("Clipboard has no files to paste", false);
        } else {
            self.ops.submit(Job { kind: JobKind::Copy, srcs: paths, dest: Some(dest) });
        }
    }

    fn paste_target(&mut self) -> Option<PathBuf> {
        if self.tab().search.is_some() {
            self.toast("Can't paste into search results", true);
            return None;
        }
        Some(self.tab().path.clone())
    }

    fn drop_paths(&mut self, paths: Vec<PathBuf>, dest: PathBuf, force_copy: bool) {
        let paths: Vec<PathBuf> = paths.into_iter().filter(|p| *p != dest).collect();
        if paths.is_empty() || !dest.is_dir() {
            return;
        }
        // Like other file managers: move within a filesystem, copy across.
        let dest_dev = std::fs::metadata(&dest).map(|m| m.dev()).ok();
        let same_fs = paths.iter().all(|p| std::fs::symlink_metadata(p).map(|m| Some(m.dev()) == dest_dev).unwrap_or(false));
        let kind = if force_copy || !same_fs { JobKind::Copy } else { JobKind::Move };
        if kind == JobKind::Move && paths.iter().all(|p| p.parent() == Some(dest.as_path())) {
            return;
        }
        self.ops.submit(Job { kind, srcs: paths, dest: Some(dest) });
    }

    fn rename(&mut self, path: &Path, name: &str) {
        let name = name.trim();
        if name.is_empty() || name.contains('/') || name == "." || name == ".." {
            return self.toast("Invalid name", true);
        }
        let target = path.with_file_name(name);
        if target == path {
            return;
        }
        if std::fs::symlink_metadata(&target).is_ok() {
            return self.toast(format!("\"{name}\" already exists"), true);
        }
        match std::fs::rename(path, &target) {
            Ok(()) => {
                if let Some(s) = self.sizes.remove(path) {
                    self.sizes.insert(target.clone(), s);
                }
                let tab = self.tab_mut();
                tab.marked.clear();
                tab.select_after_load = Some(target);
                if tab.search.is_none() {
                    self.reload_active(true);
                }
            }
            Err(e) => self.toast(format!("Rename failed: {e}"), true),
        }
    }

    fn create(&mut self, dir: &Path, name: &str, folder: bool) {
        let name = name.trim();
        if name.is_empty() || name.contains('/') || name == "." || name == ".." {
            return self.toast("Invalid name", true);
        }
        let target = dir.join(name);
        let res = if folder {
            std::fs::create_dir(&target)
        } else {
            std::fs::OpenOptions::new().write(true).create_new(true).open(&target).map(|_| ())
        };
        match res {
            Ok(()) => {
                let tab = self.tab_mut();
                tab.marked.clear();
                tab.select_after_load = Some(target);
                self.reload_active(true);
            }
            Err(e) => self.toast(format!("Couldn't create \"{name}\": {e}"), true),
        }
    }

    // ------------------------------------------------------------ commands

    fn run_cmd(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::NewTab => {
                let tab = self.tab();
                let p = tab.cursor_entry().filter(|e| e.is_dir && tab.marked.is_empty()).map(|e| e.path.clone());
                let p = p.unwrap_or_else(|| tab.path.clone());
                self.open_tab(p);
            }
            Cmd::CloseTab => {
                let p = self.active_pane;
                self.close_tab(p, self.panes[p].active);
            }
            Cmd::NewFolder | Cmd::NewFile => {
                if self.tab().search.is_some() {
                    return;
                }
                let folder = cmd == Cmd::NewFolder;
                let dir = self.tab().path.clone();
                let base = if folder { "New folder" } else { "New file.txt" };
                let name = ops::file_name(&ops::unique_dest(&dir, base));
                self.dialog = Some(Dialog::Create { dir, name, folder, focus: true });
            }
            Cmd::Rename => {
                if let Some(e) = self.tab().cursor_entry() {
                    self.dialog = Some(Dialog::Rename { path: e.path.clone(), name: e.name.clone(), focus: true });
                }
            }
            Cmd::Trash => {
                let srcs = self.tab().targets();
                self.ops.submit(Job { kind: JobKind::Trash, srcs, dest: None });
            }
            Cmd::DeletePermanently => {
                let paths = self.tab().targets();
                if !paths.is_empty() {
                    self.dialog = Some(Dialog::ConfirmDelete { paths });
                }
            }
            Cmd::Copy => self.copy_to_clipboard(false),
            Cmd::Cut => self.copy_to_clipboard(true),
            Cmd::Paste => self.paste(None),
            Cmd::Duplicate => {
                let srcs = self.tab().targets();
                if let Some(dest) = srcs.first().and_then(|p| p.parent()).map(Path::to_path_buf) {
                    let srcs = srcs.into_iter().filter(|p| p.parent() == Some(&dest)).collect();
                    self.ops.submit(Job { kind: JobKind::Copy, srcs, dest: Some(dest) });
                }
            }
            Cmd::SelectAll => self.tab_mut().select_all(),
            Cmd::Search => self.start_search(),
            Cmd::Refresh => self.refresh(),
            Cmd::ToggleHidden => {
                self.show_hidden = !self.show_hidden;
                self.refilter_all();
                if self.tab().search.is_some() {
                    self.restart_search();
                }
            }
            Cmd::TogglePreview => self.show_preview = !self.show_preview,
            Cmd::ToggleSidebar => self.show_sidebar = !self.show_sidebar,
            Cmd::BookmarkFolder => self.apply(Action::Bookmark(vec![self.tab().path.clone()])),
            Cmd::ZoomIn => self.zoom(1),
            Cmd::ZoomOut => self.zoom(-1),
            Cmd::ToggleSplit => self.toggle_split(),
            Cmd::ToggleGrid => {
                let tab = self.tab_mut();
                tab.mode = if tab.mode.is_grid() { ViewMode::Details } else { ViewMode::Icons(2) };
                tab.scroll_to_cursor = true;
            }
            Cmd::CopyPath => {
                let text = self.tab().targets().iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n");
                if !text.is_empty() {
                    self.set_text_clipboard(text);
                    self.toast("Path copied", false);
                }
            }
            Cmd::OpenTerminal => fsops::open_terminal(&self.tab().path.clone()),
            Cmd::EditPath => {
                // A trailing slash lists the subfolders straight away.
                let path = self.tab().path.display().to_string();
                let text = if path.ends_with('/') { path } else { format!("{path}/") };
                self.path_edit = Some((self.active_pane, text));
                self.completer = Default::default(); // pick up folders created since
                self.path_edit_focus = true;
            }
            Cmd::GoHome => self.navigate(fsops::home_dir(), true),
            Cmd::GoUp => self.go_up(),
            Cmd::Back => self.go_back(),
            Cmd::Forward => self.go_forward(),
            Cmd::SortName => self.apply(Action::Sort(SortKey::Name)),
            Cmd::SortSize => self.apply(Action::Sort(SortKey::Size)),
            Cmd::SortModified => self.apply(Action::Sort(SortKey::Modified)),
            Cmd::SortType => self.apply(Action::Sort(SortKey::Kind)),
            Cmd::CancelJob => self.ops.cancel.store(true, Ordering::Relaxed),
        }
    }

    fn apply(&mut self, action: Action) {
        match action {
            Action::Navigate(p) => self.navigate(p, true),
            Action::OpenEntry(vi) => {
                let Some(e) = self.tab().entry(vi) else { return };
                if e.is_dir {
                    let p = e.path.clone();
                    self.navigate(p, true);
                } else {
                    fsops::open_default(&e.path);
                }
            }
            Action::Click { vi, ctrl, shift } => {
                let tab = self.tab_mut();
                if shift {
                    tab.select_range(vi);
                } else if ctrl {
                    tab.toggle_mark(vi);
                } else {
                    tab.select_only(vi);
                }
                tab.scroll_to_cursor = false;
            }
            Action::NewTab(p) => self.open_tab(p),
            Action::CloseTab(p, i) => self.close_tab(p, i),
            Action::SwitchTab(p, i) => {
                self.active_pane = p.min(self.panes.len() - 1);
                let pane = &mut self.panes[self.active_pane];
                pane.active = i.min(pane.tabs.len() - 1);
            }
            Action::FocusPane(p) => self.active_pane = p.min(self.panes.len() - 1),
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
                let pane = &mut self.panes[self.active_pane];
                pane.tabs[pane.active].apply_view(sizes, hidden);
            }
            Action::CopyPath(p) => self.set_text_clipboard(p.display().to_string()),
            Action::OpenTerminal(p) => fsops::open_terminal(&p),
            Action::ClearFilter => {
                self.tab_mut().filter.clear();
                let (sizes, hidden) = (&self.sizes, self.show_hidden);
                let pane = &mut self.panes[self.active_pane];
                let tab = &mut pane.tabs[pane.active];
                tab.apply_view(sizes, hidden);
                tab.scroll_to_cursor = true;
            }
            Action::Cmd(c) => self.run_cmd(c),
            Action::Drop { paths, dest, copy } => self.drop_paths(paths, dest, copy),
            Action::Reveal(p) => {
                if let Some(parent) = p.parent().map(Path::to_path_buf) {
                    self.tab_mut().select_after_load = Some(p);
                    self.navigate(parent, true);
                }
            }
            Action::StopSearch => self.stop_search(),
            Action::Zoom(steps) => self.zoom(steps),
            Action::ToggleSection(s) => {
                if !self.collapsed.remove(s) {
                    self.collapsed.insert(s);
                }
            }
            Action::Bookmark(paths) => {
                let mut added = 0;
                for p in &paths {
                    match fsops::add_bookmark(p) {
                        Ok(true) => added += 1,
                        Ok(false) => {}
                        Err(e) => self.toast(format!("Could not save bookmark: {e}"), true),
                    }
                }
                self.places = fsops::places();
                self.collapsed.remove("BOOKMARKS");
                match (added, paths.first()) {
                    (0, Some(_)) => self.toast("Already bookmarked", false),
                    (1, Some(p)) => self.toast(format!("Bookmarked {}", p.file_name().unwrap_or(p.as_os_str()).to_string_lossy()), false),
                    (n, _) if n > 1 => self.toast(format!("Bookmarked {n} folders"), false),
                    _ => {}
                }
            }
            Action::Unbookmark(p) => {
                if let Err(e) = fsops::remove_bookmark(&p) {
                    self.toast(format!("Could not remove bookmark: {e}"), true);
                }
                self.places = fsops::places();
            }
        }
    }

    fn zoom(&mut self, steps: i32) {
        let tab = self.tab_mut();
        tab.mode = tab.mode.zoom(steps);
        tab.scroll_to_cursor = true;
    }

    // ------------------------------------------------------------ keyboard

    fn handle_keys(&mut self, ctx: &egui::Context) {
        if self.palette.open || self.dialog.is_some() {
            return; // they handle their own keys
        }
        let search_id = egui::Id::new(("search", self.tab().id));
        if ctx.memory(|m| m.has_focus(search_id)) {
            self.search_box_keys(ctx);
            return;
        }
        if ctx.text_edit_focused() {
            return;
        }
        if egui::DragAndDrop::has_any_payload(ctx) && ctx.input_mut(|i| take(i, Modifiers::NONE, Key::Escape)) {
            egui::DragAndDrop::clear_payload(ctx); // cancel the drag
            return;
        }
        // Tab focus-navigation can park focus on a button; keys belong to the file list.
        if let Some(id) = ctx.memory(|m| m.focused()) {
            ctx.memory_mut(|m| m.surrender_focus(id));
        }
        let mut actions: Vec<Action> = Vec::new();
        let mut filter_changed = false;
        let grid_cols = if self.tab().mode.is_grid() { self.tab().grid_cols.max(1) as isize } else { 0 };
        let split = self.panes.len() > 1;
        ctx.input_mut(|i| {
            use Cmd::*;
            let (none, ctrl, shift, alt) = (Modifiers::NONE, Modifiers::COMMAND, Modifiers::SHIFT, Modifiers::ALT);
            let cs = ctrl | shift;
            let pane = &mut self.panes[self.active_pane];
            let (n, pane_tab) = (pane.tabs.len(), pane.active);
            let tab = &mut pane.tabs[pane.active];

            let vertical = if grid_cols > 0 { grid_cols } else { 1 };
            for (key, delta) in [
                (Key::ArrowDown, vertical),
                (Key::ArrowUp, -vertical),
                (Key::PageDown, PAGE_ROWS * vertical),
                (Key::PageUp, -PAGE_ROWS * vertical),
                (Key::Home, isize::MIN / 2),
                (Key::End, isize::MAX / 2),
            ] {
                if take(i, shift, key) {
                    tab.move_cursor(delta, true);
                } else if take(i, none, key) {
                    tab.move_cursor(delta, false);
                }
            }
            if grid_cols > 0 {
                for (key, delta) in [(Key::ArrowRight, 1), (Key::ArrowLeft, -1)] {
                    if take(i, shift, key) {
                        tab.move_cursor(delta, true);
                    } else if take(i, none, key) {
                        tab.move_cursor(delta, false);
                    }
                }
            }
            if take(i, ctrl, Key::Space) {
                if let Some(c) = tab.cursor {
                    tab.toggle_mark(c);
                }
            }
            if take(i, none, Key::Enter) {
                if let Some(c) = tab.cursor {
                    actions.push(Action::OpenEntry(c));
                }
            }
            if take(i, none, Key::Backspace) {
                if tab.filter.pop().is_some() {
                    filter_changed = true;
                } else {
                    actions.push(Action::Cmd(GoUp));
                }
            }
            if take(i, none, Key::Escape) {
                if !tab.filter.is_empty() {
                    actions.push(Action::ClearFilter);
                } else if !tab.marked.is_empty() {
                    tab.marked.clear();
                } else if tab.search.is_some() {
                    actions.push(Action::StopSearch);
                }
            }
            if split && take(i, none, Key::Tab) {
                actions.push(Action::FocusPane(1 - self.active_pane));
            }
            if i.pointer.button_pressed(egui::PointerButton::Extra1) {
                actions.push(Action::Cmd(Back));
            }
            if i.pointer.button_pressed(egui::PointerButton::Extra2) {
                actions.push(Action::Cmd(Forward));
            }
            if take(i, cs, Key::Tab) {
                actions.push(Action::SwitchTab(self.active_pane, (pane_tab + n - 1) % n));
            } else if take(i, ctrl, Key::Tab) {
                actions.push(Action::SwitchTab(self.active_pane, (pane_tab + 1) % n));
            }
            let nums = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];
            for (k, key) in nums.into_iter().enumerate() {
                if take(i, ctrl, key) && k < n {
                    actions.push(Action::SwitchTab(self.active_pane, k));
                }
            }
            let bindings: &[(Modifiers, Key, Cmd)] = &[
                (alt, Key::ArrowLeft, Back),
                (alt, Key::ArrowRight, Forward),
                (alt, Key::ArrowUp, GoUp),
                (alt, Key::Home, GoHome),
                (none, Key::F2, Rename),
                (none, Key::F3, TogglePreview),
                (none, Key::F5, Refresh),
                (none, Key::F6, ToggleSplit),
                (ctrl, Key::Backslash, ToggleSplit),
                (shift, Key::Delete, DeletePermanently),
                (none, Key::Delete, Trash),
                (cs, Key::N, NewFolder),
                (ctrl, Key::A, SelectAll),
                (ctrl, Key::D, Duplicate),
                (ctrl, Key::F, Cmd::Search),
                (ctrl, Key::B, ToggleSidebar),
                (ctrl, Key::G, ToggleGrid),
                (ctrl, Key::H, ToggleHidden),
                (ctrl, Key::L, EditPath),
                (ctrl, Key::T, NewTab),
                (ctrl, Key::W, CloseTab),
                (alt, Key::C, CopyPath),
                (ctrl | alt, Key::T, OpenTerminal),
            ];
            if take(i, ctrl, Key::P) || take(i, cs, Key::P) {
                self.palette.show();
            }
            for &(m, key, cmd) in bindings {
                if take(i, m, key) {
                    actions.push(Action::Cmd(cmd));
                }
            }
            // Clipboard shortcuts arrive as events, not keys.
            let mut paste = None;
            for ev in &i.events {
                match ev {
                    Event::Copy => actions.push(Action::Cmd(Copy)),
                    Event::Cut => actions.push(Action::Cmd(Cut)),
                    Event::Paste(t) => paste = Some(t.clone()),
                    _ => {}
                }
            }
            if let Some(t) = paste {
                self.pending_paste = Some(t);
            }
            if i.modifiers.command || i.modifiers.alt {
                return;
            }
            // Type-to-filter.
            for ev in &i.events {
                if let Event::Text(t) = ev {
                    tab.filter.push_str(t);
                    filter_changed = true;
                }
            }
        });
        if filter_changed {
            let (sizes, hidden) = (&self.sizes, self.show_hidden);
            let pane = &mut self.panes[self.active_pane];
            let tab = &mut pane.tabs[pane.active];
            tab.apply_view(sizes, hidden);
            tab.scroll_to_cursor = true;
        }
        for a in actions {
            self.apply(a);
        }
        if let Some(text) = self.pending_paste.take() {
            self.paste(Some(text));
        }
    }

    /// While the search box has focus, arrows and Enter drive the results.
    fn search_box_keys(&mut self, ctx: &egui::Context) {
        let mut open = None;
        let mut stop = false;
        ctx.input_mut(|i| {
            let tab = self.tab_mut();
            if take(i, Modifiers::NONE, Key::ArrowDown) {
                tab.move_cursor(1, false);
            }
            if take(i, Modifiers::NONE, Key::ArrowUp) {
                tab.move_cursor(-1, false);
            }
            if take(i, Modifiers::NONE, Key::Enter) {
                open = tab.cursor;
            }
            if take(i, Modifiers::NONE, Key::Escape) {
                stop = true;
            }
        });
        if let Some(vi) = open {
            self.apply(Action::OpenEntry(vi));
        }
        if stop {
            ctx.memory_mut(|m| m.surrender_focus(egui::Id::new(("search", self.tab().id))));
            self.stop_search();
        }
    }
}

impl App {
    fn init_wayland(&mut self, frame: &eframe::Frame) {
        use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
        self.wl_tried = true;
        let Some(win) = frame.winit_window() else { return };
        let (Ok(d), Ok(w)) = (win.display_handle(), win.window_handle()) else { return };
        if let (RawDisplayHandle::Wayland(d), RawWindowHandle::Wayland(w)) = (d.as_raw(), w.as_raw()) {
            // Safety: both pointers belong to our live window, which outlives the app.
            match unsafe { crate::wl_dnd::WlDnd::new(d.display.as_ptr(), w.surface.as_ptr(), self.ctx.clone()) } {
                Ok(wl) => self.wl = Some(wl),
                Err(e) => eprintln!("kestrel: system drag and drop unavailable: {e}"),
            }
        }
    }

    fn poll_wayland(&mut self) {
        let Some(wl) = &self.wl else { return };
        for ev in wl.poll() {
            match ev {
                crate::wl_dnd::DndEvent::Hover { pos } => self.ext_hover = Some(pos),
                crate::wl_dnd::DndEvent::Leave => self.ext_hover = None,
                crate::wl_dnd::DndEvent::Drop { paths, pos, own } => {
                    self.ext_hover = None;
                    if let Some((dest, pane)) = self.drop_destination(pos) {
                        self.active_pane = pane;
                        // Our own drag coming back behaves like an in-app drop; others always copy.
                        self.drop_paths(paths, dest, !own);
                    }
                }
            }
        }
    }
}

impl eframe::App for App {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Stop the drag-and-drop thread while winit's display is still alive.
        self.wl = None;
    }

    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw: &mut egui::RawInput) {
        if std::mem::take(&mut self.inject_release) {
            let pos = self.ctx.pointer_latest_pos().unwrap_or_default();
            raw.events.push(Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            });
        }
    }

    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // Ask for a frame callback with this frame, so winit holds the next redraw
        // until the compositor wants one (none arrive on a hidden workspace).
        if let Some(win) = frame.winit_window() {
            win.pre_present_notify();
        }
        if !self.wl_tried {
            self.init_wayland(frame);
        }
        self.poll_wayland();
        self.poll(ctx);
        self.handle_keys(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }
}

/// Consumes a key press with exactly these modifiers.
fn take(i: &mut InputState, mods: Modifiers, key: Key) -> bool {
    let mut found = false;
    i.events.retain(|e| match e {
        Event::Key { key: k, pressed: true, modifiers, .. } if !found && *k == key && modifiers.matches_exact(mods) => {
            found = true;
            false
        }
        _ => true,
    });
    found
}

/// Starts an async directory read. A soft reload keeps scroll position and selection.
fn start_listing(tab: &mut Tab, tx: &Sender<Listing>, ctx: &egui::Context, soft: bool) {
    tab.generation += 1;
    tab.loading = true;
    tab.cancel.store(true, Ordering::Relaxed);
    tab.cancel = Arc::new(AtomicBool::new(false));
    if soft && tab.select_after_load.is_none() {
        tab.select_after_load = tab.cursor_entry().map(|e| e.path.clone());
    }
    let (tx, ctx) = (tx.clone(), ctx.clone());
    let (tab_id, generation, path) = (tab.id, tab.generation, tab.path.clone());
    std::thread::spawn(move || {
        let result = fsops::list_dir(&path);
        let _ = tx.send(Listing { tab_id, generation, soft, result });
        ctx.request_repaint();
    });
}

/// Paths from clipboard text: one per line, plain or as file:// URIs.
pub(crate) fn parse_path_list(text: &str) -> Vec<PathBuf> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| match l.strip_prefix("file://") {
            Some(rest) => PathBuf::from(fsops::percent_decode(rest)),
            None => PathBuf::from(l),
        })
        .filter(|p| p.is_absolute() && std::fs::symlink_metadata(p).is_ok())
        .collect()
}

/// Files copied in GTK/Qt file managers are offered as text/uri-list.
fn wl_paste_uri_list() -> Vec<PathBuf> {
    let out = std::process::Command::new("wl-paste")
        .args(["--no-newline", "--type", "text/uri-list"])
        .stderr(std::process::Stdio::null())
        .output();
    match out {
        Ok(o) if o.status.success() => parse_path_list(&String::from_utf8_lossy(&o.stdout)),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_paths() {
        let tmp = std::env::temp_dir();
        let text = format!("file://{}\nnot a path\n{}\n", tmp.display(), tmp.display());
        assert_eq!(parse_path_list(&text), vec![tmp.clone(), tmp]);
    }
}
