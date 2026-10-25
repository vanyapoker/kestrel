mod columns;
mod goto;
mod icons;
mod preview_text;
mod tabs;
mod ui;
#[cfg(test)]
mod ui_tests;

use crate::fsops::{self, Entry, Places};
use crate::ops::{self, Job, JobKind, Ops};
use crate::palette::{Cmd, Palette};
use crate::preview::Preview;
use crate::search::{self, SearchMsg};
use crate::tab::{Search, Select, SortKey, Tab, ViewMode};
use crate::theme;
use crate::thumbs::Thumbs;
use crate::watch::Watcher;
use crate::wl_dnd::ClipOp;
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
    listing_id: u64,
    generation: u64,
    soft: bool,
    result: Result<Vec<Entry>, String>,
}

pub struct Pane {
    tabs: Vec<Tab>,
    active: usize,
}

struct Clip {
    paths: Vec<PathBuf>,
    op: ClipOp,
    /// What we put on the system clipboard, to tell our copy from someone else's.
    text: String,
}

enum Dialog {
    SaveWorkspace {
        name: String,
        focus: bool,
    },
    Workspaces,
    BatchRename(Box<crate::rename::Editor>),
    Rename {
        path: PathBuf,
        name: String,
        focus: bool,
    },
    Create {
        dir: PathBuf,
        name: String,
        kind: NewKind,
        focus: bool,
    },
    ConfirmDelete {
        paths: Vec<PathBuf>,
    },
    Compress {
        srcs: Vec<PathBuf>,
        dest: PathBuf,
        name: String,
        format: ops::Format,
        level: ops::Level,
        focus: bool,
    },
    ConfirmEmptyTrash,
    Permissions(Box<PermissionsEdit>),
    SelectPattern {
        pattern: String,
        focus: bool,
    },
    /// A copy or move whose items already exist in the destination.
    Conflicts(Box<Conflicts>),
}

/// Editor for permissions and ownership of `paths`.
struct PermissionsEdit {
    paths: Vec<PathBuf>,
    /// Mode bits as shown (rwx for owner/group/others, plus setuid/setgid/sticky).
    mode: u32,
    original_mode: u32,
    owner: String,
    group: String,
    original_owner: String,
    original_group: String,
    recursive: bool,
    any_dir: bool,
}

/// Asks, one item at a time, what to do with names that already exist in `job.dest`.
struct Conflicts {
    /// The transfer to run once every conflict is settled; its `srcs` shrink as items are skipped.
    job: Job,
    /// Sources whose target exists, in order.
    pending: Vec<PathBuf>,
    /// Index into `pending` of the item being asked about.
    current: usize,
    apply_to_all: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Resolution {
    Replace,
    ReplaceIfNewer,
    Skip,
    KeepBoth,
}

struct Toast {
    text: String,
    error: bool,
    until: Instant,
}

/// Drag-and-drop payload.
struct DragPaths(Vec<PathBuf>);

struct TabDrag {
    id: u64,
}

/// How a drop transfers files. Ctrl forces a copy, Shift forces a move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DropMode {
    /// Move within a filesystem, copy across.
    Auto,
    Copy,
    Move,
}

impl DropMode {
    pub(super) fn held(ctrl: bool, shift: bool) -> Self {
        if ctrl {
            Self::Copy
        } else if shift {
            Self::Move
        } else {
            Self::Auto
        }
    }

    fn from_input(ui: &egui::Ui) -> Self {
        ui.input(|i| Self::held(i.modifiers.ctrl, i.modifiers.shift))
    }
}

/// Whether navigating records the old folder for Back.
#[derive(Clone, Copy, PartialEq, Eq)]
enum History {
    Push,
    Replace,
}

/// What the "new" dialog creates.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NewKind {
    File,
    Folder,
}

enum Action {
    InColumn {
        tab_id: u64,
        directory: PathBuf,
        action: Box<Action>,
    },
    ColumnStep(i32),
    SetView(ViewMode),
    Navigate(PathBuf),
    OpenEntry(usize),
    Click {
        vi: usize,
        ctrl: bool,
        shift: bool,
    },
    NewTab(PathBuf),
    CloseTab(usize, usize),
    SwitchTab(usize, usize),
    MoveTab {
        id: u64,
        pane: usize,
        before: Option<u64>,
    },
    ReopenClosed(usize),
    FocusPane(usize),
    Sort(SortKey),
    CopyPath(PathBuf),
    OpenTerminal(PathBuf),
    ClearFilter,
    Cmd(Cmd),
    /// Run the custom action at this index on the selection.
    Custom(usize),
    Drop {
        paths: Vec<PathBuf>,
        dest: PathBuf,
        mode: DropMode,
    },
    Reveal(PathBuf),
    StopSearch,
    /// Step the active tab's view mode (Ctrl+wheel).
    Zoom(i32),
    ToggleSection(&'static str),
    Bookmark(Vec<PathBuf>),
    Unbookmark(PathBuf),
    OpenWith(crate::apps::DesktopApp, Vec<PathBuf>),
    /// App id, file type, and a label for the toast (".md files").
    SetDefaultApp(crate::apps::DesktopApp, String, String),
    Device(crate::devices::DevOp, PathBuf),
    PickConfirm,
    /// Choose the folder being viewed (folder pickers).
    PickHere,
    PickCancel,
}

/// State of a file-picker window (`kestrel --pick`).
struct Picker {
    req: crate::picker::Request,
    /// File name being typed when saving.
    name: String,
    focus: bool,
    /// What was chosen; printed on exit.
    chosen: Vec<PathBuf>,
}

pub struct App {
    ctx: egui::Context,
    panes: Vec<Pane>,
    active_pane: usize,
    split_ratio: f32,
    closed_tabs: Vec<tabs::Closed>,
    workspaces: std::collections::BTreeMap<String, crate::session::Session>,
    workspace_file: Option<PathBuf>,
    next_tab_id: u64,
    list_tx: Sender<Listing>,
    list_rx: Receiver<Listing>,
    size_service: crate::sizes::Sizes,
    search_tx: Sender<SearchMsg>,
    search_rx: Receiver<SearchMsg>,
    sizes: HashMap<PathBuf, u64>,
    sizing: HashSet<PathBuf>,
    preview: Preview,
    /// Inspector navigation is independent of the main folder and its selection.
    inspector: Vec<Entry>,
    thumbs: Thumbs,
    watcher: Watcher,
    /// Follows `omarchy theme set`; kept alive for its notifications.
    theme_watch: Option<(notify::RecommendedWatcher, std::sync::mpsc::Receiver<()>)>,
    theme_reload_at: Option<Instant>,
    ops: Ops,
    places: Places,
    devices: Vec<crate::devices::Device>,
    /// Rescans from the device watcher (`None` in tests).
    devices_rx: Option<Receiver<Vec<crate::devices::Device>>>,
    dev_tx: Sender<crate::devices::DevResult>,
    custom_actions: Arc<Vec<crate::actions::CustomAction>>,
    /// Owner and link info for the inspected item.
    props_cache: Option<(PathBuf, Option<crate::props::Props>)>,
    checksums: HashMap<PathBuf, crate::props::Sums>,
    checksums_pending: HashSet<PathBuf>,
    checksum_tx: Sender<(PathBuf, crate::props::Sums)>,
    checksum_rx: Receiver<(PathBuf, crate::props::Sums)>,
    action_tx: Sender<Result<String, String>>,
    action_rx: Receiver<Result<String, String>>,
    dev_rx: Receiver<crate::devices::DevResult>,
    /// Devices with a mount, unmount or eject in progress.
    dev_busy: HashSet<PathBuf>,
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
    recent_dirs: Vec<PathBuf>,
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
    #[cfg(test)]
    tab_rects: HashMap<u64, (egui::Rect, egui::Rect)>,
    /// Session file; `None` in tests, so they never touch the real one.
    session_file: Option<PathBuf>,
    /// The session text last written, to skip identical saves.
    saved_session: String,
    next_session_check: Instant,
    /// Reversible operations, most recent last.
    undo: Vec<ops::Undo>,
    /// Panel widths to restore, and the last widths seen while shown.
    sidebar_width: f32,
    preview_width: f32,
    picker: Option<Picker>,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext,
        start: Option<PathBuf>,
        session: Option<crate::session::Session>,
        picker: Option<crate::picker::Request>,
    ) -> Self {
        let start = start.or_else(|| picker.as_ref().and_then(|p| p.dir.clone()));
        let mut app = Self::with_ctx(
            cc.egui_ctx.clone(),
            start.clone().unwrap_or_else(fsops::home_dir),
        );
        if let Some(req) = picker {
            app.show_preview = false;
            app.picker = Some(Picker {
                name: req.name.clone(),
                focus: true,
                chosen: Vec::new(),
                req,
            });
            app.devices = crate::devices::scan();
            app.devices_rx = Some(crate::devices::watch(app.ctx.clone()));
            return app;
        }
        if let Some(s) = session {
            app.restore(&s, start.is_none());
            app.saved_session = s.to_text();
        }
        app.session_file = crate::session::file();
        app.workspace_file = crate::workspaces::file();
        if let Some(file) = &app.workspace_file {
            match crate::workspaces::load(file) {
                Ok(workspaces) => app.workspaces = workspaces,
                Err(error) => app.toast_error(format!("Could not load workspaces: {error}")),
            }
        }
        app.devices = crate::devices::scan();
        app.devices_rx = Some(crate::devices::watch(app.ctx.clone()));
        app
    }

    fn with_ctx(ctx: egui::Context, start: PathBuf) -> Self {
        theme::apply(&ctx);
        let (list_tx, list_rx) = channel();
        let (search_tx, search_rx) = channel();
        let (dev_tx, dev_rx) = channel();
        let (action_tx, action_rx) = channel();
        let (checksum_tx, checksum_rx) = channel();
        let mut app = Self {
            preview: Preview::new(ctx.clone()),
            inspector: Vec::new(),
            thumbs: Thumbs::new(ctx.clone()),
            watcher: Watcher::new(ctx.clone()),
            theme_watch: theme::watch(ctx.clone()),
            theme_reload_at: None,
            ops: Ops::new(ctx.clone()),
            ctx: ctx.clone(),
            panes: vec![Pane {
                tabs: Vec::new(),
                active: 0,
            }],
            active_pane: 0,
            split_ratio: 0.5,
            closed_tabs: Vec::new(),
            workspaces: Default::default(),
            workspace_file: None,
            next_tab_id: 0,
            list_tx,
            list_rx,
            size_service: crate::sizes::Sizes::new(ctx.clone()),
            search_tx,
            search_rx,
            sizes: HashMap::new(),
            sizing: HashSet::new(),
            places: fsops::places(),
            devices: Vec::new(),
            devices_rx: None,
            dev_tx,
            dev_rx,
            custom_actions: Arc::new(crate::actions::load()),
            props_cache: None,
            checksums: HashMap::new(),
            checksums_pending: HashSet::new(),
            checksum_tx,
            checksum_rx,
            action_tx,
            action_rx,
            dev_busy: HashSet::new(),
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
            recent_dirs: Vec::new(),
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
            #[cfg(test)]
            tab_rects: HashMap::new(),
            session_file: None,
            undo: Vec::new(),
            saved_session: String::new(),
            next_session_check: Instant::now(),
            sidebar_width: 190.0,
            preview_width: 360.0,
            picker: None,
        };
        app.open_tab(start);
        app
    }

    // ------------------------------------------------------------ access

    fn tab(&self) -> &Tab {
        let pane = &self.panes[self.active_pane];
        &pane.tabs[pane.active]
    }

    fn tab_in_pane(&self, p: usize) -> &Tab {
        let pane = &self.panes[p];
        &pane.tabs[pane.active]
    }

    fn tab_mut(&mut self) -> &mut Tab {
        let pane = &mut self.panes[self.active_pane];
        &mut pane.tabs[pane.active]
    }

    fn tabs_mut(&mut self) -> impl Iterator<Item = &mut Tab> {
        self.panes.iter_mut().flat_map(|p| p.tabs.iter_mut())
    }

    fn refilter_all(&mut self) {
        let (sizes, hidden) = (&self.sizes, self.show_hidden);
        for tab in self.panes.iter_mut().flat_map(|p| p.tabs.iter_mut()) {
            tab.apply_view(sizes, hidden);
            if let Some(c) = &mut tab.columns {
                for (i, item) in c.items.iter_mut().enumerate() {
                    if i != c.active {
                        item.apply_view(sizes, hidden);
                    }
                }
            }
        }
    }

    fn toast(&mut self, text: impl Into<String>) {
        self.push_toast(text.into(), false);
    }

    fn toast_error(&mut self, text: impl Into<String>) {
        self.push_toast(text.into(), true);
    }

    fn push_toast(&mut self, text: String, error: bool) {
        let secs = if error { 6 } else { 3 };
        self.toasts.push(Toast {
            text,
            error,
            until: Instant::now() + Duration::from_secs(secs),
        });
        self.ctx.request_repaint_after(Duration::from_secs(secs));
    }

    // ------------------------------------------------------------ session

    /// Applies saved settings, and the saved tabs too when `tabs` is set.
    fn restore(&mut self, s: &crate::session::Session, tabs: bool) {
        self.split_ratio = s.split_ratio;
        self.recent_dirs = s.recent_dirs.clone();
        self.show_hidden = s.show_hidden;
        self.show_preview = s.show_preview;
        self.show_sidebar = s.show_sidebar;
        // Sections are keyed by their static titles; ignore names we no longer have.
        self.collapsed = ["PLACES", "BOOKMARKS", "DEVICES"]
            .into_iter()
            .filter(|t| s.collapsed.iter().any(|c| c == t))
            .collect();
        if let Some(w) = s.sidebar_width {
            self.sidebar_width = w;
        }
        if let Some(w) = s.preview_width {
            self.preview_width = w;
        }
        // Existing panel memory otherwise overrides the newly restored widths.
        self.ctx.data_mut(|d| {
            d.remove::<egui::containers::panel::PanelState>(egui::Id::new("sidebar"));
            d.remove::<egui::containers::panel::PanelState>(egui::Id::new("preview"));
        });
        if !tabs || s.panes.is_empty() {
            // Opened on a specific folder: keep it, but in the view it was last seen in.
            if let Some(t) = s
                .panes
                .get(s.active_pane)
                .and_then(|p| p.tabs.get(p.active))
            {
                let tab = self.tab_mut();
                (tab.sort, tab.ascending, tab.mode) = (t.sort, t.ascending, t.mode);
            }
            self.remember_dir(self.tab().path.clone());
            return;
        }
        for pane in std::mem::take(&mut self.panes) {
            for tab in pane.tabs {
                tab.cancel.store(true, Ordering::Relaxed);
                if let Some(s) = tab.search {
                    s.cancel.store(true, Ordering::Relaxed);
                }
            }
        }
        for ps in &s.panes {
            let mut tabs = Vec::new();
            for t in &ps.tabs {
                // A folder may have been removed since; open its nearest surviving parent.
                let Some(dir) = t.path.ancestors().find(|a| a.is_dir()) else {
                    continue;
                };
                let mut tab = self.new_tab(dir.to_path_buf());
                (tab.sort, tab.ascending, tab.mode) = (t.sort, t.ascending, t.mode);
                tabs.push(tab);
            }
            if !tabs.is_empty() {
                let active = ps.active.min(tabs.len() - 1);
                self.panes.push(Pane { tabs, active });
            }
        }
        if self.panes.is_empty() {
            let tab = self.new_tab(fsops::home_dir());
            self.panes.push(Pane {
                tabs: vec![tab],
                active: 0,
            });
        }
        self.active_pane = s.active_pane.min(self.panes.len() - 1);
        self.remember_dir(self.tab().path.clone());
    }

    fn session(&self) -> crate::session::Session {
        use crate::session::{PaneState, Session, TabState};
        let panes = self
            .panes
            .iter()
            .map(|p| PaneState {
                tabs: p
                    .tabs
                    .iter()
                    .map(|t| TabState {
                        path: t.path.clone(),
                        sort: t.sort,
                        ascending: t.ascending,
                        mode: t.mode,
                    })
                    .collect(),
                active: p.active,
            })
            .collect();
        let mut collapsed: Vec<String> = self.collapsed.iter().map(|s| s.to_string()).collect();
        collapsed.sort();
        Session {
            panes,
            active_pane: self.active_pane,
            show_hidden: self.show_hidden,
            show_preview: self.show_preview,
            show_sidebar: self.show_sidebar,
            collapsed,
            sidebar_width: Some(self.sidebar_width),
            preview_width: Some(self.preview_width),
            recent_dirs: self.recent_dirs.clone(),
            split_ratio: self.split_ratio,
        }
    }

    /// Writes the session if it changed. Checked every couple of seconds, so a
    /// crash loses little, and once more on exit.
    fn save_session(&mut self, force: bool) {
        let Some(file) = self.session_file.clone() else {
            return;
        };
        let now = Instant::now();
        if !force && now < self.next_session_check {
            return;
        }
        self.next_session_check = now + Duration::from_secs(2);
        let text = self.session().to_text();
        if text != self.saved_session {
            match crate::session::save(&file, &text) {
                Ok(()) => self.saved_session = text,
                Err(e) => eprintln!("kestrel: could not save session: {e}"),
            }
        }
    }

    // ------------------------------------------------------------ tabs & navigation

    fn new_tab(&mut self, path: PathBuf) -> Tab {
        let mut tab = Tab::new(self.next_tab_id, path);
        self.next_tab_id += 1;
        start_listing(&mut tab, &self.list_tx, &self.ctx, false);
        tab
    }

    fn open_tab(&mut self, path: PathBuf) {
        self.remember_dir(path.clone());
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
        self.closed_tabs.push(tabs::Closed::Tab {
            pane: p,
            position: i,
            state: tabs::state(&tab),
        });
        if self.closed_tabs.len() > 32 {
            self.closed_tabs.remove(0);
        }
        tab.cancel.store(true, Ordering::Relaxed);
        if let Some(s) = tab.search {
            s.cancel.store(true, Ordering::Relaxed);
        }
        if pane.active >= pane.tabs.len() || pane.active > i {
            pane.active = pane.active.saturating_sub(1);
        }
    }

    fn toggle_split(&mut self) {
        if self.panes.len() > 1 {
            self.close_split(1 - self.active_pane);
        } else {
            let tab = self.new_tab(self.tab().path.clone());
            self.panes.push(Pane {
                tabs: vec![tab],
                active: 0,
            });
            self.active_pane = 1;
        }
    }

    fn close_split(&mut self, p: usize) {
        let pane = self.panes.remove(p);
        self.closed_tabs.push(tabs::Closed::Pane {
            position: p,
            state: crate::session::PaneState {
                tabs: pane.tabs.iter().map(tabs::state).collect(),
                active: pane.active,
            },
        });
        if self.closed_tabs.len() > 32 {
            self.closed_tabs.remove(0);
        }
        for tab in pane.tabs {
            tab.cancel.store(true, Ordering::Relaxed);
            if let Some(s) = tab.search {
                s.cancel.store(true, Ordering::Relaxed);
            }
        }
        self.active_pane = 0;
    }

    fn reload_active(&mut self, soft: bool) {
        let pane = &mut self.panes[self.active_pane];
        start_listing(&mut pane.tabs[pane.active], &self.list_tx, &self.ctx, soft);
    }

    fn navigate(&mut self, path: PathBuf, history: History) {
        self.remember_dir(path.clone());
        let tab = self.tab_mut();
        if path == tab.path && tab.search.is_none() {
            return;
        }
        tab.columns = None;
        if let Some(s) = tab.search.take() {
            s.cancel.store(true, Ordering::Relaxed);
        }
        if path != tab.path {
            if history == History::Push {
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
            self.navigate(parent, History::Push);
        }
    }

    fn go_back(&mut self) {
        let tab = self.tab_mut();
        if let Some(p) = tab.back.pop() {
            let cur = tab.path.clone();
            tab.fwd.push(cur.clone());
            tab.select_after_load = Some(cur);
            self.navigate(p, History::Replace);
        }
    }

    fn go_forward(&mut self) {
        let tab = self.tab_mut();
        if let Some(p) = tab.fwd.pop() {
            let cur = tab.path.clone();
            tab.back.push(cur);
            self.navigate(p, History::Replace);
        }
    }

    fn refresh(&mut self) {
        let path = self.tab().path.clone();
        self.size_service.invalidate(&path, &mut self.sizes);
        self.preview.invalidate();
        let tab = self.tab_mut();
        if let Some(s) = &mut tab.search {
            s.index = None;
            s.running = false;
            self.restart_search();
            return;
        }
        let (tx, ctx) = (&self.list_tx, &self.ctx);
        let pane = &mut self.panes[self.active_pane];
        if let Some(c) = &mut pane.tabs[pane.active].columns {
            for (i, item) in c.items.iter_mut().enumerate() {
                if i != c.active {
                    start_listing(item, tx, ctx, true);
                }
            }
        }
        self.reload_active(true);
    }

    fn start_search(&mut self) {
        let tab = self.tab_mut();
        if tab.search.is_none() {
            let query = std::mem::take(&mut tab.filter);
            tab.search = Some(Search {
                query,
                flatten: false,
                root: tab.path.clone(),
                generation: 0,
                cancel: Arc::new(AtomicBool::new(false)),
                running: false,
                truncated: false,
                follow_best: true,
                restart_at: None,
                index: None,
                active_query: String::new(),
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
        if s.running && s.index.is_none() {
            // Finish the query-independent scan instead of restarting the walk
            // at every keystroke. The final index reranks the latest query.
            s.restart_at = None;
            s.follow_best = true;
            tab.apply_view(&self.sizes, hidden);
            tab.select_best_match();
            return;
        }
        s.cancel.store(true, Ordering::Relaxed);
        s.cancel = Arc::new(AtomicBool::new(false));
        s.generation += 1;
        s.restart_at = None;
        s.truncated = false;
        s.follow_best = true;
        tab.entries.clear();
        tab.view.clear();
        tab.marked.clear();
        tab.cursor = None;
        tab.force_offset = Some(0.0);
        let s = tab.search.as_mut().unwrap();
        s.running = s.flatten || !s.query.trim().is_empty();
        s.active_query = s.query.clone();
        if s.running {
            let req = search::Request {
                tab_id,
                generation: s.generation,
                root: s.root.clone(),
                query: s.query.clone(),
                show_hidden: hidden,
                cancel: s.cancel.clone(),
                index: s.index.clone(),
            };
            search::spawn(req, self.search_tx.clone(), self.ctx.clone());
        }
        tab.apply_view(&self.sizes, hidden);
    }

    fn stop_search(&mut self) {
        let tab = self.tab_mut();
        if let Some(s) = tab.search.take() {
            s.cancel.store(true, Ordering::Relaxed);
            tab.select_after_load = tab
                .cursor_entry()
                .filter(|e| e.path.parent() == Some(&tab.path))
                .map(|e| e.path.clone());
            self.reload_active(false);
        }
    }

    // ------------------------------------------------------------ background results

    fn poll(&mut self, ctx: &egui::Context) {
        let now = Instant::now();

        if let Some(rx) = &self.devices_rx
            && let Some(devices) = rx.try_iter().last()
        {
            self.devices = devices;
        }
        while let Ok(r) = self.dev_rx.try_recv() {
            self.device_done(r);
        }
        while let Ok((path, sums)) = self.checksum_rx.try_recv() {
            self.checksums_pending.remove(&path);
            self.checksums.insert(path, sums);
        }
        while let Ok(r) = self.action_rx.try_recv() {
            match r {
                Ok(text) => self.toast(text),
                Err(text) => self.toast_error(text),
            }
        }

        while let Ok(l) = self.list_rx.try_recv() {
            if l.result.is_err() && self.repair_missing_column(l.listing_id, l.generation) {
                continue;
            }
            let (sizes, hidden) = (&self.sizes, self.show_hidden);
            let Some(tab) = columns::find_listing(&mut self.panes, l.listing_id) else {
                continue;
            };
            if tab.generation != l.generation || tab.search.is_some() {
                continue;
            }
            tab.loading = false;
            tab.disk = fsops::disk_space(&tab.path);
            // `view` indexes the old entries; selection is restored by path below.
            let path_at =
                |vi: Option<usize>| vi.and_then(|vi| tab.entry(vi)).map(|e| e.path.clone());
            let kept = (tab.cursor.zip(path_at(tab.cursor)), path_at(tab.anchor));
            if !l.soft {
                tab.cursor = None;
                tab.anchor = None;
                tab.force_offset = Some(0.0);
            }
            tab.view.clear();
            match l.result {
                Ok(entries) => {
                    tab.entries = entries;
                    tab.error = None;
                }
                Err(_) if (l.soft || tab.mode == ViewMode::Columns) && !tab.path.exists() => {
                    // The folder was deleted or renamed under us: fall back to the nearest parent.
                    if let Some(up) = tab
                        .path
                        .ancestors()
                        .find(|a| a.is_dir())
                        .map(Path::to_path_buf)
                    {
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
                tab.center_cursor = std::mem::take(&mut tab.center_after_load);
            } else if l.soft {
                tab.restore_selection(kept.0, kept.1);
            }
        }

        let mut searched = HashSet::new();
        while let Ok(msg) = self.search_rx.try_recv() {
            let (sizes, hidden) = (&self.sizes, self.show_hidden);
            let Some(tab) = self
                .panes
                .iter_mut()
                .flat_map(|p| &mut p.tabs)
                .find(|t| t.id == msg.tab_id)
            else {
                continue;
            };
            let kept = (
                tab.cursor.zip(tab.cursor_entry().map(|e| e.path.clone())),
                tab.anchor
                    .and_then(|vi| tab.entry(vi))
                    .map(|e| e.path.clone()),
            );
            let Some(s) = tab.search.as_mut() else {
                continue;
            };
            if s.generation != msg.generation {
                continue;
            }
            if msg.done {
                s.running = false;
                s.truncated = msg.truncated;
                if let Some(index) = msg.index {
                    s.index = Some(index);
                }
                if s.active_query != s.query {
                    s.restart_at = Some(now);
                }
            }
            if msg.replace {
                tab.entries = msg.batch;
                tab.view.clear();
                tab.apply_view(sizes, hidden);
                if !tab.search.as_ref().unwrap().follow_best {
                    tab.restore_selection(kept.0, kept.1);
                }
            } else {
                tab.entries.extend(msg.batch);
            }
            searched.insert(msg.tab_id);
        }
        if !searched.is_empty() {
            let (sizes, hidden) = (&self.sizes, self.show_hidden);
            for tab in self
                .panes
                .iter_mut()
                .flat_map(|p| p.tabs.iter_mut())
                .filter(|t| searched.contains(&t.id))
            {
                let follow_best =
                    tab.search.as_ref().is_some_and(|s| s.follow_best) && tab.marked.is_empty();
                tab.apply_view(sizes, hidden);
                if follow_best {
                    tab.select_best_match();
                }
            }
        }

        while let Ok(done) = self.ops.done_rx.try_recv() {
            self.job_done(done);
        }

        // Theme switches rewrite several files; wait for them to settle, then restyle.
        if let Some((_, rx)) = &self.theme_watch
            && rx.try_iter().count() > 0
        {
            self.theme_reload_at = Some(now + Duration::from_millis(300));
            ctx.request_repaint_after(Duration::from_millis(300));
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
            self.size_service.invalidate(&p, &mut self.sizes);
            self.thumbs.invalidate(&p);
            if let Some(parent) = p.parent() {
                self.thumbs.invalidate(parent); // a folder tile previews its children
            }
            if self
                .preview
                .path
                .as_deref()
                .is_some_and(|path| path == p || p.parent() == Some(path))
            {
                self.preview.invalidate();
            }
            for tab in self.tabs_mut() {
                columns::changed(tab, &p, now);
                if tab.search.is_none() && (Some(tab.path.as_path()) == p.parent() || tab.path == p)
                {
                    tab.reload_at.get_or_insert(now + WATCH_DEBOUNCE);
                }
                if let Some(s) = &mut tab.search
                    && p.starts_with(&s.root)
                {
                    s.cancel.store(true, Ordering::Relaxed);
                    s.index = None;
                    s.running = false;
                    s.restart_at = Some(now + WATCH_DEBOUNCE);
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

        self.sync_columns();
        let wanted: HashSet<PathBuf> = self
            .panes
            .iter()
            .flat_map(|p| p.tabs.iter())
            .filter(|t| t.search.is_none())
            .map(|t| t.path.clone())
            .chain(
                self.panes
                    .iter()
                    .flat_map(|p| &p.tabs)
                    .flat_map(columns::watched_paths),
            )
            .chain(
                self.inspector
                    .last()
                    .filter(|_| self.show_preview)
                    .and_then(|e| {
                        if e.is_dir {
                            Some(e.path.clone())
                        } else {
                            e.path.parent().map(Path::to_path_buf)
                        }
                    }),
            )
            .collect();
        self.watcher.sync(wanted);
        self.sync_sizes(now);

        self.toasts.retain(|t| t.until > now);
        if self.ops.queued.load(Ordering::Relaxed) > 0 {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        self.thumbs.poll(ctx);
        self.preview.poll(ctx);
    }

    /// Gather interests independently of tab listing cancellation, including passive columns.
    fn sync_sizes(&mut self, now: Instant) {
        let mut wanted = Vec::new();
        for tab in self
            .panes
            .iter()
            .flat_map(|p| &p.tabs)
            .filter(|t| t.search.is_none())
        {
            let mut add = |tab: &Tab| {
                wanted.extend(
                    tab.entries
                        .iter()
                        .filter(|e| e.is_dir && !e.is_link)
                        .map(|e| e.path.clone()),
                );
            };
            add(tab);
            if let Some(c) = &tab.columns {
                for (i, item) in c.items.iter().enumerate() {
                    if i != c.active {
                        add(item);
                    }
                }
            }
        }
        if self
            .size_service
            .sync(wanted, &mut self.sizes, &mut self.sizing, now)
        {
            let (sizes, hidden) = (&self.sizes, self.show_hidden);
            for tab in self.panes.iter_mut().flat_map(|p| &mut p.tabs) {
                if tab.sort == SortKey::Size {
                    tab.apply_view(sizes, hidden);
                }
                if let Some(c) = &mut tab.columns {
                    for (i, item) in c.items.iter_mut().enumerate() {
                        if i != c.active && item.sort == SortKey::Size {
                            item.apply_view(sizes, hidden);
                        }
                    }
                }
            }
        }
    }

    fn job_done(&mut self, done: ops::Done) {
        for d in &done.dirs {
            self.size_service.invalidate(d, &mut self.sizes);
        }
        let select = done.created.first().cloned();
        let (list_tx, ctx) = (&self.list_tx, &self.ctx);
        let active_id = {
            let pane = &self.panes[self.active_pane];
            pane.tabs[pane.active].id
        };
        for tab in self.panes.iter_mut().flat_map(|p| p.tabs.iter_mut()) {
            if let Some(c) = &mut tab.columns {
                for (i, item) in c.items.iter_mut().enumerate() {
                    if i != c.active && done.dirs.contains(&item.path) {
                        start_listing(item, list_tx, ctx, true);
                    }
                }
            }
            if tab.search.is_none() && done.dirs.contains(&tab.path) {
                if tab.id == active_id
                    && select.as_ref().and_then(|p| p.parent()) == Some(tab.path.as_path())
                {
                    tab.select_after_load = select.clone();
                }
                tab.reload_at = None;
                start_listing(tab, list_tx, ctx, true);
            }
        }
        if let Some(u) = done.undo.clone().filter(|_| !done.undoing) {
            self.push_undo(u);
        }
        if done.undoing && done.errors.is_empty() && !done.cancelled {
            return; // `undo` already announced it
        }
        if done.cancelled {
            self.toast("Operation cancelled");
        } else if let Some(first) = done.errors.first() {
            let more = if done.errors.len() > 1 {
                format!(" (+{} more errors)", done.errors.len() - 1)
            } else {
                String::new()
            };
            self.toast_error(format!("{first}{more}"));
        } else if matches!(
            done.kind,
            JobKind::Copy
                | JobKind::Move
                | JobKind::Rename
                | JobKind::Trash
                | JobKind::Delete
                | JobKind::Extract
                | JobKind::Compress
                | JobKind::SaveImage
                | JobKind::Restore
                | JobKind::EmptyTrash
                | JobKind::PasteImage
                | JobKind::Properties
        ) {
            self.toast(done.summary);
        }
    }

    // ------------------------------------------------------------ file operations

    fn push_undo(&mut self, u: ops::Undo) {
        const MAX_UNDO: usize = 50;
        self.undo.push(u);
        if self.undo.len() > MAX_UNDO {
            self.undo.remove(0);
        }
    }

    fn undo(&mut self) {
        let Some(u) = self.undo.pop() else {
            return self.toast("Nothing to undo");
        };
        self.toast(u.describe());
        self.ops.submit(Job::undo(u));
    }

    fn copy_to_clipboard(&mut self, op: ClipOp) {
        let paths = self.tab().targets();
        if paths.is_empty() {
            return;
        }
        let text = paths
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let on_system = self
            .wl
            .as_ref()
            .is_some_and(|wl| wl.set_clipboard_files(&paths, op));
        if !on_system {
            self.ctx.copy_text(text.clone());
        }
        let n = paths.len();
        self.clip = Some(Clip { paths, op, text });
        let what = if n == 1 {
            "1 item".into()
        } else {
            format!("{n} items")
        };
        self.toast(format!(
            "{} {what}",
            if op == ClipOp::Cut { "Cut" } else { "Copied" }
        ));
    }

    /// Pastes the app's own clipboard. A copy stays available for further pastes, a cut is consumed.
    /// Submits a copy or move, first asking about items that already exist in the destination.
    fn submit_transfer(&mut self, job: Job) {
        let dest = job.dest.clone().unwrap_or_default();
        let pending: Vec<PathBuf> = job
            .srcs
            .iter()
            .filter(|src| {
                let target = dest.join(ops::file_name(src));
                // Copying next to the original duplicates it; moving there is a no-op.
                src.parent() != Some(dest.as_path()) && std::fs::symlink_metadata(&target).is_ok()
            })
            .cloned()
            .collect();
        if pending.is_empty() {
            return self.ops.submit(job);
        }
        self.dialog = Some(Dialog::Conflicts(Box::new(Conflicts {
            job,
            pending,
            current: 0,
            apply_to_all: false,
        })));
    }

    /// Records the answer for the current conflict (and the rest, if applying to all);
    /// submits the job once none are left.
    fn resolve_conflict(&mut self, mut c: Box<Conflicts>, how: Resolution) {
        let end = if c.apply_to_all {
            c.pending.len()
        } else {
            c.current + 1
        };
        let dest = c.job.dest.clone().unwrap_or_default();
        for src in c.pending[c.current..end].iter().cloned() {
            let newer = || {
                let modified = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
                modified(&src) > modified(&dest.join(ops::file_name(&src)))
            };
            match how {
                Resolution::Replace => {
                    c.job.replace.insert(src);
                }
                Resolution::ReplaceIfNewer if newer() => {
                    c.job.replace.insert(src);
                }
                Resolution::ReplaceIfNewer | Resolution::Skip => c.job.srcs.retain(|s| *s != src),
                Resolution::KeepBoth => {}
            }
        }
        c.current = end;
        if c.current < c.pending.len() {
            self.dialog = Some(Dialog::Conflicts(c));
        } else {
            self.ops.submit(c.job);
        }
    }

    fn paste_internal(&mut self, dest: PathBuf) {
        let Some(clip) = self.clip.take() else {
            return;
        };
        if clip.op == ClipOp::Copy {
            self.clip = Some(Clip {
                paths: clip.paths.clone(),
                op: ClipOp::Copy,
                text: clip.text,
            });
        }
        self.submit_transfer(Job::new(clip.op.job_kind(), clip.paths, Some(dest)));
    }

    /// Computes checksums of `path` in the background.
    fn start_checksums(&mut self, path: PathBuf, modified: Option<std::time::SystemTime>) {
        if !self.checksums_pending.insert(path.clone()) {
            return;
        }
        let (tx, ctx) = (self.checksum_tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let sums = crate::props::Algo::ALL
                .into_iter()
                .map(|a| (a, crate::props::checksum(&path, a)))
                .collect();
            let _ = tx.send((path, (modified, sums)));
            ctx.request_repaint();
        });
    }

    fn edit_permissions(&mut self) {
        let paths = self.tab().targets();
        let Some(first) = paths.first() else { return };
        let Some(meta) = std::fs::metadata(first).ok() else {
            return self.toast_error("Can't read the permissions");
        };
        let props = crate::props::read(first);
        let (owner, group) = props.map(|p| (p.owner, p.group)).unwrap_or_default();
        let mode = std::os::unix::fs::MetadataExt::mode(&meta) & 0o7777;
        self.dialog = Some(Dialog::Permissions(Box::new(PermissionsEdit {
            any_dir: paths.iter().any(|p| p.is_dir()),
            paths,
            mode,
            original_mode: mode,
            original_owner: owner.clone(),
            original_group: group.clone(),
            owner,
            group,
            recursive: false,
        })));
    }

    fn run_custom(&mut self, index: usize) {
        let Some(action) = self.custom_actions.get(index).cloned() else {
            return;
        };
        let targets = self.tab().targets();
        if !action.applies(&targets) {
            return self.toast(format!(
                "\"{}\" doesn't apply to this selection",
                action.name
            ));
        }
        let dir = self.tab().path.clone();
        crate::actions::spawn(
            &action,
            &targets,
            &dir,
            self.action_tx.clone(),
            self.ctx.clone(),
        );
    }

    fn select_pattern(&mut self, pattern: &str) {
        if pattern.trim().is_empty() {
            return;
        }
        if self.tab_mut().select_matching(pattern) == 0 {
            self.toast("No items match the pattern");
        }
    }

    fn paste_image(&mut self, dest: PathBuf, mime: impl Into<String>) {
        let mut job = Job::new(JobKind::PasteImage, Vec::new(), Some(dest));
        job.name = Some(mime.into());
        self.ops.submit(job);
    }

    fn set_text_clipboard(&mut self, text: String) {
        if !self
            .wl
            .as_ref()
            .is_some_and(|wl| wl.set_clipboard_text(&text))
        {
            self.ctx.copy_text(text);
        }
    }

    /// `text` is the system clipboard contents, if egui delivered a paste event.
    fn paste(&mut self, text: Option<String>) {
        let Some(dest) = self.paste_target() else {
            return;
        };
        if let Some(wl) = &self.wl {
            match wl.read_clipboard() {
                crate::wl_dnd::ClipRead::Ours => {}
                crate::wl_dnd::ClipRead::Files { paths, op } => {
                    self.ops.submit(Job::new(op.job_kind(), paths, Some(dest)));
                    return;
                }
                crate::wl_dnd::ClipRead::Image(mime) => {
                    self.paste_image(dest, mime);
                    return;
                }
                crate::wl_dnd::ClipRead::Nothing => {
                    self.toast("Clipboard has no files to paste");
                    return;
                }
            }
            // Ours: the internal clipboard knows whether it was a cut.
            self.paste_internal(dest);
            return;
        }
        let ours = match (&self.clip, &text) {
            (Some(c), Some(t)) => c.text.trim() == t.trim(),
            (Some(_), None) => true,
            _ => false,
        };
        if ours {
            self.paste_internal(dest);
            return;
        }
        // Files copied in another app: plain paths, file:// URIs, or a uri-list.
        let mut paths = text.as_deref().map(parse_path_list).unwrap_or_default();
        if paths.is_empty() {
            paths = wl_paste_uri_list();
        }
        if paths.is_empty() {
            match wl_paste_image_type() {
                Some(mime) => self.paste_image(dest, mime),
                None => self.toast("Clipboard has no files to paste"),
            }
        } else {
            self.submit_transfer(Job::new(JobKind::Copy, paths, Some(dest)));
        }
    }

    fn paste_target(&mut self) -> Option<PathBuf> {
        if fsops::in_trash(&self.tab().path) {
            self.toast_error("Can't paste into the trash");
            return None;
        }
        if self.tab().search.is_some() {
            self.toast_error("Can't paste into search results");
            return None;
        }
        Some(self.tab().path.clone())
    }

    fn drop_paths(&mut self, paths: Vec<PathBuf>, dest: PathBuf, mode: DropMode) {
        let paths: Vec<PathBuf> = paths.into_iter().filter(|p| *p != dest).collect();
        if paths.is_empty() || !dest.is_dir() {
            return;
        }
        // Dropping on the trash folder trashes properly (with restore info).
        if dest == fsops::trash_dir() {
            let paths = paths.into_iter().filter(|p| !fsops::in_trash(p)).collect();
            return self.ops.submit(Job::new(JobKind::Trash, paths, None));
        }
        if fsops::in_trash(&dest) {
            return self.toast_error("Can't move files into the trash folder; drop on Trash");
        }
        // Like other file managers: move within a filesystem, copy across.
        let dest_dev = std::fs::metadata(&dest).map(|m| m.dev()).ok();
        let same_fs = paths.iter().all(|p| {
            std::fs::symlink_metadata(p)
                .map(|m| Some(m.dev()) == dest_dev)
                .unwrap_or(false)
        });
        let kind = match mode {
            DropMode::Copy => JobKind::Copy,
            DropMode::Move => JobKind::Move,
            DropMode::Auto if same_fs => JobKind::Move,
            DropMode::Auto => JobKind::Copy,
        };
        if kind == JobKind::Move && paths.iter().all(|p| p.parent() == Some(dest.as_path())) {
            return;
        }
        self.submit_transfer(Job::new(kind, paths, Some(dest)));
    }

    fn rename(&mut self, path: &Path, name: &str) {
        let name = name.trim();
        if name.is_empty() || name.contains('/') || name == "." || name == ".." {
            return self.toast_error("Invalid name");
        }
        let target = path.with_file_name(name);
        if target == path {
            return;
        }
        if std::fs::symlink_metadata(&target).is_ok() {
            return self.toast_error(format!("\"{name}\" already exists"));
        }
        match std::fs::rename(path, &target) {
            Ok(()) => {
                self.push_undo(ops::Undo::MoveBack(vec![(
                    path.to_path_buf(),
                    target.clone(),
                )]));
                self.size_service.invalidate(path, &mut self.sizes);
                self.size_service.invalidate(&target, &mut self.sizes);
                let tab = self.tab_mut();
                tab.marked.clear();
                tab.select_after_load = Some(target);
                if tab.search.is_none() {
                    self.reload_active(true);
                }
            }
            Err(e) => self.toast_error(format!("Rename failed: {e}")),
        }
    }

    fn create(&mut self, dir: &Path, name: &str, kind: NewKind) {
        let name = name.trim();
        if name.is_empty() || name.contains('/') || name == "." || name == ".." {
            return self.toast_error("Invalid name");
        }
        let target = dir.join(name);
        let res = if kind == NewKind::Folder {
            std::fs::create_dir(&target)
        } else {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
                .map(|_| ())
        };
        match res {
            Ok(()) => {
                self.push_undo(ops::Undo::TrashCreated(vec![target.clone()]));
                let tab = self.tab_mut();
                tab.marked.clear();
                tab.select_after_load = Some(target);
                tab.center_after_load = true;
                self.reload_active(true);
            }
            Err(e) => self.toast_error(format!("Couldn't create \"{name}\": {e}")),
        }
    }

    // ------------------------------------------------------------ commands

    fn run_cmd(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::ReopenTab => {
                if let Some(i) = self.closed_tabs.len().checked_sub(1) {
                    self.reopen_closed(i);
                }
            }
            Cmd::SaveWorkspace => {
                self.dialog = Some(Dialog::SaveWorkspace {
                    name: String::new(),
                    focus: true,
                })
            }
            Cmd::Workspaces => self.dialog = Some(Dialog::Workspaces),
            Cmd::GoTo => self.palette.show_goto(),
            Cmd::NewTab => {
                let tab = self.tab();
                let p = tab
                    .cursor_entry()
                    .filter(|e| e.is_dir && tab.marked.is_empty())
                    .map(|e| e.path.clone());
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
                let kind = if cmd == Cmd::NewFolder {
                    NewKind::Folder
                } else {
                    NewKind::File
                };
                let dir = self.tab().path.clone();
                let base = match kind {
                    NewKind::Folder => "New folder",
                    NewKind::File => "New file.txt",
                };
                let name = ops::file_name(&ops::unique_dest(&dir, base));
                self.dialog = Some(Dialog::Create {
                    dir,
                    name,
                    kind,
                    focus: true,
                });
                self.ctx
                    .memory_mut(|m| m.request_focus(egui::Id::new("dialog_name")));
            }
            Cmd::Rename | Cmd::BatchRename => {
                let tab = self.tab();
                if cmd == Cmd::BatchRename || tab.selection_count() > 1 {
                    let entries: Vec<_> = tab
                        .view
                        .iter()
                        .enumerate()
                        .filter(|(vi, _)| tab.is_selected(*vi))
                        .map(|(_, &i)| tab.entries[i].clone())
                        .collect();
                    if !entries.is_empty() {
                        self.dialog = Some(Dialog::BatchRename(Box::new(
                            crate::rename::Editor::new(entries, self.ctx.clone()),
                        )));
                    }
                } else if let Some(e) = tab.cursor_entry() {
                    self.dialog = Some(Dialog::Rename {
                        path: e.path.clone(),
                        name: e.name.clone(),
                        focus: true,
                    });
                }
            }
            // Inside the trash, "trash" can only mean deleting for good.
            Cmd::Trash if fsops::in_trash(&self.tab().path) => self.run_cmd(Cmd::DeletePermanently),
            Cmd::Trash => {
                let srcs = self.tab().targets();
                self.ops.submit(Job::new(JobKind::Trash, srcs, None));
            }
            Cmd::Restore => {
                let srcs: Vec<PathBuf> = self
                    .tab()
                    .targets()
                    .into_iter()
                    .filter(|p| fsops::in_trash(p))
                    .collect();
                if srcs.is_empty() {
                    self.toast("Select items in the trash to restore");
                } else {
                    self.ops.submit(Job::new(JobKind::Restore, srcs, None));
                }
            }
            Cmd::EmptyTrash => self.dialog = Some(Dialog::ConfirmEmptyTrash),
            Cmd::DeletePermanently => {
                let paths = self.tab().targets();
                if !paths.is_empty() {
                    self.dialog = Some(Dialog::ConfirmDelete { paths });
                }
            }
            Cmd::Undo => self.undo(),
            Cmd::Extract | Cmd::ExtractFolder | Cmd::ExtractOther => {
                let archives: Vec<PathBuf> = self
                    .tab()
                    .targets()
                    .into_iter()
                    .filter(|p| ops::is_archive(p))
                    .collect();
                let other = (self.panes.len() > 1)
                    .then(|| self.tab_in_pane(1 - self.active_pane).path.clone());
                if archives.is_empty() {
                    self.toast("Select an archive to extract");
                } else if cmd == Cmd::ExtractOther && other.is_none() {
                    self.toast("Open split view (F6) to extract into the other pane");
                } else {
                    let mut job = Job::new(JobKind::Extract, archives, None);
                    match cmd {
                        Cmd::ExtractFolder => job.extract_to = ops::ExtractTo::Folder,
                        Cmd::ExtractOther => {
                            job.extract_to = ops::ExtractTo::Dir;
                            job.dest = other;
                        }
                        _ => {}
                    }
                    self.ops.submit(job);
                }
            }
            Cmd::Compress => {
                let srcs = self.tab().targets();
                // Search results can come from several folders; the zip goes next to the first.
                if let Some(dest) = srcs.first().and_then(|p| p.parent()).map(Path::to_path_buf) {
                    let mut job = Job::new(JobKind::Compress, srcs, Some(dest));
                    job.name = Some(ops::zip_name(&job.srcs));
                    self.ops.submit(job);
                }
            }
            Cmd::CompressAs => {
                let srcs = self.tab().targets();
                if let Some(dest) = srcs.first().and_then(|p| p.parent()).map(Path::to_path_buf) {
                    let name = ops::archive_base_name(&srcs);
                    self.dialog = Some(Dialog::Compress {
                        srcs,
                        dest,
                        name,
                        format: ops::Format::Zip,
                        level: ops::Level::Normal,
                        focus: true,
                    });
                }
            }
            Cmd::Copy => self.copy_to_clipboard(ClipOp::Copy),
            Cmd::Cut => self.copy_to_clipboard(ClipOp::Cut),
            Cmd::Paste => self.paste(None),
            Cmd::Duplicate => {
                let srcs = self.tab().targets();
                if let Some(dest) = srcs.first().and_then(|p| p.parent()).map(Path::to_path_buf) {
                    let srcs = srcs
                        .into_iter()
                        .filter(|p| p.parent() == Some(&dest))
                        .collect();
                    self.ops.submit(Job::new(JobKind::Copy, srcs, Some(dest)));
                }
            }
            Cmd::SelectAll => self.tab_mut().select_all(),
            Cmd::Properties => self.edit_permissions(),
            Cmd::InvertSelection => self.tab_mut().invert_selection(),
            Cmd::SelectPattern => {
                self.dialog = Some(Dialog::SelectPattern {
                    pattern: "*.".into(),
                    focus: true,
                })
            }
            Cmd::Search => self.start_search(),
            Cmd::Flatten => {
                if self.tab().search.as_ref().is_some_and(|s| s.flatten) {
                    self.stop_search();
                } else {
                    self.start_search();
                    let s = self.tab_mut().search.as_mut().unwrap();
                    s.flatten = true;
                    s.query.clear();
                    self.restart_search();
                }
            }
            Cmd::Refresh => {
                self.custom_actions = Arc::new(crate::actions::load());
                self.refresh()
            }
            Cmd::ToggleHidden => {
                self.show_hidden = !self.show_hidden;
                self.refilter_all();
                if self.tab().search.is_some() {
                    self.refresh();
                }
            }
            Cmd::TogglePreview => self.show_preview = !self.show_preview,
            Cmd::ToggleSidebar => self.show_sidebar = !self.show_sidebar,
            Cmd::BookmarkFolder => self.apply(Action::Bookmark(vec![self.tab().path.clone()])),
            Cmd::ZoomIn => self.zoom(1),
            Cmd::ZoomOut => self.zoom(-1),
            Cmd::ToggleSplit => self.toggle_split(),
            Cmd::ToggleColumns => {
                let mode = if self.tab().mode == ViewMode::Columns {
                    self.tab().previous_view
                } else {
                    ViewMode::Columns
                };
                self.set_view(mode);
            }
            Cmd::ToggleGrid => {
                let mode = if self.tab().mode.is_grid() {
                    ViewMode::Details
                } else {
                    ViewMode::Icons(2)
                };
                self.set_view(mode);
            }
            Cmd::CopyPath => {
                let text = self
                    .tab()
                    .targets()
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join("\n");
                if !text.is_empty() {
                    self.set_text_clipboard(text);
                    self.toast("Path copied");
                }
            }
            Cmd::OpenTerminal => fsops::open_terminal(&self.tab().path.clone()),
            Cmd::EditPath => {
                // A trailing slash lists the subfolders straight away.
                let path = self.tab().path.display().to_string();
                let text = if path.ends_with('/') {
                    path
                } else {
                    format!("{path}/")
                };
                self.path_edit = Some((self.active_pane, text));
                self.completer = Default::default(); // pick up folders created since
                self.path_edit_focus = true;
            }
            Cmd::GoHome => self.navigate(fsops::home_dir(), History::Push),
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

    /// Finishes the picker with one file.
    fn pick_file(&mut self, path: PathBuf) {
        let Some(p) = &mut self.picker else { return };
        match p.req.kind {
            // A double-clicked file fills in the name; the Save button decides.
            crate::picker::Kind::Save => {
                p.name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
            }
            crate::picker::Kind::Folder => {}
            crate::picker::Kind::Open => {
                p.chosen = vec![path];
                self.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    /// The paths the Select / Save button would return right now.
    fn pick_choice(&self) -> Vec<PathBuf> {
        use crate::picker::Kind;
        let Some(p) = &self.picker else {
            return Vec::new();
        };
        let tab = self.tab();
        match p.req.kind {
            Kind::Open => {
                let mut files: Vec<PathBuf> = tab
                    .targets()
                    .into_iter()
                    .filter(|f| tab.entries.iter().any(|e| &e.path == f && !e.is_dir))
                    .collect();
                if !p.req.multiple {
                    files.truncate(1);
                }
                files
            }
            Kind::Folder => {
                let dirs: Vec<PathBuf> = tab
                    .targets()
                    .into_iter()
                    .filter(|f| tab.entries.iter().any(|e| &e.path == f && e.is_dir))
                    .collect();
                if p.req.multiple {
                    dirs
                } else {
                    dirs.into_iter().take(1).collect()
                }
            }
            Kind::Save => {
                let name = p.name.trim();
                if name.is_empty() || name.contains('/') || name == "." || name == ".." {
                    Vec::new()
                } else {
                    vec![tab.path.join(name)]
                }
            }
        }
    }

    fn pick_confirm(&mut self) {
        let chosen = self.pick_choice();
        if chosen.is_empty() {
            return;
        }
        if let Some(p) = &mut self.picker {
            p.chosen = chosen;
        }
        self.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    fn apply(&mut self, action: Action) {
        match action {
            Action::InColumn {
                tab_id,
                directory,
                action,
            } => {
                if self.activate_column_path(tab_id, &directory)
                    && !matches!(*action, Action::FocusPane(_))
                {
                    self.apply(*action);
                }
            }
            Action::ColumnStep(delta) => self.column_step(delta),
            Action::SetView(mode) => self.set_view(mode),
            Action::MoveTab { id, pane, before } => self.move_tab(id, pane, before),
            Action::ReopenClosed(i) => self.reopen_closed(i),
            Action::Navigate(p) => self.navigate(p, History::Push),
            Action::OpenEntry(vi) => {
                let Some(e) = self.tab().entry(vi) else {
                    return;
                };
                if e.is_dir {
                    if self.tab().mode == ViewMode::Columns && self.tab().search.is_none() {
                        self.tab_mut().select_only(vi);
                        self.column_step(1);
                    } else {
                        let p = e.path.clone();
                        self.navigate(p, History::Push);
                    }
                } else if self.picker.is_some() {
                    // Opening a file in a picker means choosing it.
                    let path = e.path.clone();
                    self.pick_file(path);
                } else {
                    fsops::open_default(&e.path);
                }
            }
            Action::PickConfirm => self.pick_confirm(),
            Action::PickHere => {
                let here = self.tab().path.clone();
                if let Some(p) = &mut self.picker {
                    p.chosen = vec![here];
                    self.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            Action::PickCancel => self.ctx.send_viewport_cmd(egui::ViewportCommand::Close),
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
                // Clicking a file while saving reuses its name.
                let clicked = tab.entry(vi).filter(|e| !e.is_dir).map(|e| e.name.clone());
                if let (Some(p), Some(name)) = (&mut self.picker, clicked)
                    && p.req.kind == crate::picker::Kind::Save
                {
                    p.name = name;
                }
            }
            Action::NewTab(p) => self.open_tab(p),
            Action::CloseTab(p, i) => self.close_tab(p, i),
            Action::SwitchTab(p, i) => {
                self.active_pane = p.min(self.panes.len() - 1);
                let pane = &mut self.panes[self.active_pane];
                pane.active = i.min(pane.tabs.len() - 1);
                self.remember_dir(self.tab().path.clone());
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
                tab.refilter(sizes, hidden);
                tab.scroll_to_cursor = true;
            }
            Action::Cmd(c) => self.run_cmd(c),
            Action::Custom(i) => self.run_custom(i),
            Action::Drop { paths, dest, mode } => self.drop_paths(paths, dest, mode),
            Action::Reveal(p) => {
                if let Some(parent) = p.parent().map(Path::to_path_buf) {
                    self.tab_mut().select_after_load = Some(p);
                    self.navigate(parent, History::Push);
                }
            }
            Action::StopSearch => self.stop_search(),
            Action::Zoom(steps) => self.zoom(steps),
            Action::OpenWith(app, paths) => {
                if let Err(e) = crate::apps::launch(&app, &paths) {
                    self.toast_error(e);
                }
            }
            Action::SetDefaultApp(app, mime, what) => {
                match crate::apps::set_default(&app.id, &mime) {
                    Ok(()) => self.toast(format!("{what} now open with {}", app.name)),
                    Err(e) => self.toast_error(format!("Couldn't set the default app: {e}")),
                }
            }
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
                        Err(e) => self.toast_error(format!("Could not save bookmark: {e}")),
                    }
                }
                self.places = fsops::places();
                self.collapsed.remove("BOOKMARKS");
                match (added, paths.first()) {
                    (0, Some(_)) => self.toast("Already bookmarked"),
                    (1, Some(p)) => self.toast(format!(
                        "Bookmarked {}",
                        p.file_name().unwrap_or(p.as_os_str()).to_string_lossy()
                    )),
                    (n, _) if n > 1 => self.toast(format!("Bookmarked {n} folders")),
                    _ => {}
                }
            }
            Action::Device(op, dev) => self.device_op(op, &dev),
            Action::Unbookmark(p) => {
                if let Err(e) = fsops::remove_bookmark(&p) {
                    self.toast_error(format!("Could not remove bookmark: {e}"));
                }
                self.places = fsops::places();
            }
        }
    }

    // ------------------------------------------------------------ devices

    fn device_op(&mut self, op: crate::devices::DevOp, dev: &Path) {
        use crate::devices::DevOp;
        let Some(device) = self.devices.iter().find(|d| d.dev == dev) else {
            return;
        };
        if !self.dev_busy.insert(device.dev.clone()) {
            return;
        }
        // Eject unmounts every mounted partition of the disk first.
        let siblings = match op {
            DevOp::Eject => self
                .devices
                .iter()
                .filter(|d| d.disk == device.disk && d.mount.is_some())
                .map(|d| d.dev.clone())
                .collect(),
            _ => Vec::new(),
        };
        crate::devices::run(op, device, siblings, self.dev_tx.clone(), self.ctx.clone());
    }

    fn device_done(&mut self, r: crate::devices::DevResult) {
        use crate::devices::DevOp;
        self.dev_busy.remove(&r.dev);
        let left = self
            .devices
            .iter()
            .find(|d| d.dev == r.dev)
            .and_then(|d| d.mount.clone());
        // Rescan now rather than wait for the watcher, so the sidebar updates at once.
        self.devices = crate::devices::scan();
        let verb = match r.op {
            DevOp::Mount => "mount",
            DevOp::Unmount => "unmount",
            DevOp::Eject => "eject",
        };
        match r.result {
            Err(e) => self.toast_error(format!("Could not {verb} {}: {e}", r.name)),
            Ok(mounted) => {
                match r.op {
                    DevOp::Mount if r.read_only => self.toast_error(format!(
                        "{} was not safely removed, so it is read-only. Run ntfsfix -d {} to fix",
                        r.name,
                        r.dev.display()
                    )),
                    DevOp::Mount => {}
                    DevOp::Unmount => self.toast(format!("Unmounted {}", r.name)),
                    DevOp::Eject => self.toast(format!("{} can be unplugged", r.name)),
                }
                if let Some(p) = mounted {
                    self.navigate(p, History::Push);
                }
                // Tabs left on the drive go home.
                if let Some(old) = left.filter(|_| r.op != DevOp::Mount) {
                    let home = fsops::home_dir();
                    for pi in 0..self.panes.len() {
                        for ti in 0..self.panes[pi].tabs.len() {
                            if self.panes[pi].tabs[ti].path.starts_with(&old) {
                                let (ap, at) = (self.active_pane, self.panes[pi].active);
                                self.active_pane = pi;
                                self.panes[pi].active = ti;
                                self.navigate(home.clone(), History::Push);
                                self.active_pane = ap;
                                self.panes[pi].active = at;
                            }
                        }
                    }
                }
            }
        }
    }

    fn zoom(&mut self, steps: i32) {
        let tab = self.tab_mut();
        if tab.mode == ViewMode::Columns {
            return;
        }
        tab.mode = tab.mode.zoom(steps);
        tab.scroll_to_cursor = true;
    }

    // ------------------------------------------------------------ keyboard

    fn handle_keys(&mut self, ctx: &egui::Context) {
        if self.palette.open || self.dialog.is_some() || ctx.text_edit_focused() {
            if let crate::preview::State::Ready(crate::preview::Content::Text { layout, .. }) =
                &mut self.preview.state
                && let Some(chunks) = &mut layout.chunks
            {
                chunks.active = false;
                chunks.selection = None;
            }
            ctx.plugin::<egui::text_selection::LabelSelectionState>()
                .lock()
                .clear_selection();
        }
        if self.palette.open || self.dialog.is_some() {
            return; // they handle their own keys
        }
        if ctx.input_mut(|i| take(i, Modifiers::COMMAND, Key::K)) {
            self.palette.show_goto();
            return;
        }
        if ctx.input_mut(|i| take(i, Modifiers::COMMAND | Modifiers::SHIFT, Key::F)) {
            self.run_cmd(Cmd::Flatten);
            return;
        }
        if ctx.input_mut(|i| take(i, Modifiers::COMMAND | Modifiers::SHIFT, Key::T)) {
            self.run_cmd(Cmd::ReopenTab);
            return;
        }
        let search_id = egui::Id::new(("search", self.tab().id));
        if ctx.memory(|m| m.has_focus(search_id)) {
            self.search_box_keys(ctx);
            return;
        }
        if ctx.text_edit_focused() {
            return;
        }
        if egui::DragAndDrop::has_any_payload(ctx)
            && ctx.input_mut(|i| take(i, Modifiers::NONE, Key::Escape))
        {
            egui::DragAndDrop::clear_payload(ctx); // cancel the drag
            return;
        }
        if self.show_preview
            && let crate::preview::State::Ready(crate::preview::Content::Text {
                job, layout, ..
            }) = &mut self.preview.state
            && let Some(chunks) = &mut layout.chunks
            && preview_text::selection_keys(ctx, job, chunks)
        {
            return;
        }
        // Tab focus-navigation can park focus on a button; keys belong to the file list.
        if let Some(id) = ctx.memory(|m| m.focused()) {
            ctx.memory_mut(|m| m.surrender_focus(id));
        }
        let mut actions: Vec<Action> = Vec::new();
        let mut filter_changed = false;
        let preview_selected = self.show_preview
            && matches!(&self.preview.state,
            crate::preview::State::Ready(crate::preview::Content::Text { layout, .. })
                if layout.chunks.as_ref().is_some_and(|c| c.active && c.selection.is_some_and(|(a, b)| a != b)));
        let text_selected = (preview_selected
            || ctx
                .plugin::<egui::text_selection::LabelSelectionState>()
                .lock()
                .has_selection())
            && !ctx.input(|i| i.pointer.any_pressed());
        let columns = self.tab().mode == ViewMode::Columns && self.tab().search.is_none();
        let grid_cols = if self.tab().mode.is_grid() {
            self.tab().grid_cols.max(1) as isize
        } else {
            0
        };
        let split = self.panes.len() > 1;
        ctx.input_mut(|i| {
            use Cmd::*;
            let (none, ctrl, shift, alt) = (
                Modifiers::NONE,
                Modifiers::COMMAND,
                Modifiers::SHIFT,
                Modifiers::ALT,
            );
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
                    tab.move_cursor(delta, Select::Extend);
                } else if take(i, none, key) {
                    tab.move_cursor(delta, Select::Only);
                }
            }
            if grid_cols > 0 {
                for (key, delta) in [(Key::ArrowRight, 1), (Key::ArrowLeft, -1)] {
                    if take(i, shift, key) {
                        tab.move_cursor(delta, Select::Extend);
                    } else if take(i, none, key) {
                        tab.move_cursor(delta, Select::Only);
                    }
                }
            }
            if columns {
                if take(i, none, Key::ArrowLeft) {
                    actions.push(Action::ColumnStep(-1));
                }
                if take(i, none, Key::ArrowRight) {
                    actions.push(Action::ColumnStep(1));
                }
            }
            if take(i, ctrl, Key::Space)
                && let Some(c) = tab.cursor
            {
                tab.toggle_mark(c);
            }
            if take(i, none, Key::Enter)
                && let Some(c) = tab.cursor
            {
                actions.push(Action::OpenEntry(c));
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
                } else if self.picker.is_some() {
                    actions.push(Action::PickCancel);
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
            let nums = [
                Key::Num1,
                Key::Num2,
                Key::Num3,
                Key::Num4,
                Key::Num5,
                Key::Num6,
                Key::Num7,
                Key::Num8,
                Key::Num9,
            ];
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
                (none, Key::Space, TogglePreview),
                (none, Key::F5, Refresh),
                (none, Key::F6, ToggleSplit),
                (ctrl, Key::Backslash, ToggleSplit),
                (shift, Key::Delete, DeletePermanently),
                (none, Key::Delete, Trash),
                (cs, Key::N, NewFolder),
                (ctrl, Key::A, SelectAll),
                (cs, Key::A, InvertSelection),
                (ctrl, Key::S, SelectPattern),
                (ctrl, Key::Z, Cmd::Undo),
                (ctrl, Key::D, Duplicate),
                (ctrl, Key::F, Cmd::Search),
                (ctrl, Key::B, ToggleSidebar),
                (ctrl, Key::G, ToggleGrid),
                (cs, Key::M, ToggleColumns),
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
                    Event::Copy if !text_selected => actions.push(Action::Cmd(Copy)),
                    Event::Cut if !text_selected => actions.push(Action::Cmd(Cut)),
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
                    // Space belongs to the preview shortcut outside text fields.
                    if t.trim().is_empty() {
                        continue;
                    }
                    tab.filter.push_str(t);
                    filter_changed = true;
                }
            }
        });
        if filter_changed {
            let (sizes, hidden) = (&self.sizes, self.show_hidden);
            let pane = &mut self.panes[self.active_pane];
            let tab = &mut pane.tabs[pane.active];
            tab.refilter(sizes, hidden);
            tab.select_best_match();
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
                tab.move_cursor(1, Select::Only);
            }
            if take(i, Modifiers::NONE, Key::ArrowUp) {
                tab.move_cursor(-1, Select::Only);
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
        use raw_window_handle::{
            HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle,
        };
        self.wl_tried = true;
        let Some(win) = frame.winit_window() else {
            return;
        };
        let (Ok(d), Ok(w)) = (win.display_handle(), win.window_handle()) else {
            return;
        };
        if let (RawDisplayHandle::Wayland(d), RawWindowHandle::Wayland(w)) =
            (d.as_raw(), w.as_raw())
        {
            // Safety: both pointers belong to our live window, which outlives the app.
            match unsafe {
                crate::wl_dnd::WlDnd::new(d.display.as_ptr(), w.surface.as_ptr(), self.ctx.clone())
            } {
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
                crate::wl_dnd::DndEvent::Error(error) => {
                    self.ext_hover = None;
                    self.toast_error(error);
                }
                crate::wl_dnd::DndEvent::Image { name, bytes, pos } => {
                    self.ext_hover = None;
                    if let Some((dest, pane)) = self.drop_destination(pos) {
                        self.active_pane = pane;
                        let mut job = Job::new(JobKind::SaveImage, Vec::new(), Some(dest));
                        job.image = Some((name, bytes));
                        self.ops.submit(job);
                    }
                }
                crate::wl_dnd::DndEvent::Drop { paths, pos, own } => {
                    self.ext_hover = None;
                    if let Some((dest, pane)) = self.drop_destination(pos) {
                        self.active_pane = pane;
                        // Our own drag coming back behaves like an in-app drop; others always copy.
                        self.drop_paths(
                            paths,
                            dest,
                            if own { DropMode::Auto } else { DropMode::Copy },
                        );
                    }
                }
            }
        }
    }
}

impl eframe::App for App {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(p) = &self.picker {
            use std::io::Write;
            let mut out = std::io::stdout().lock();
            for path in &p.chosen {
                let _ = writeln!(out, "{}", path.display());
            }
            let _ = out.flush();
        }
        self.save_session(true);
        // Stop the drag-and-drop thread while winit's display is still alive.
        self.wl = None;
    }

    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw: &mut egui::RawInput) {
        // egui-winit reads the clipboard through X11 only, which is empty or stale on
        // Wayland; take paste text from our own data device so text fields get it.
        if let Some(wl) = &self.wl {
            for ev in &mut raw.events {
                if let Event::Paste(text) = ev
                    && let Some(t) = wl.read_clipboard_text()
                {
                    *text = t.replace("\r\n", "\n");
                }
            }
        }
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
        self.save_session(false);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }
}

/// Consumes a key press with exactly these modifiers.
fn take(i: &mut InputState, mods: Modifiers, key: Key) -> bool {
    let mut found = false;
    i.events.retain(|e| match e {
        Event::Key {
            key: k,
            pressed: true,
            modifiers,
            ..
        } if !found && *k == key && modifiers.matches_exact(mods) => {
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
    let (tx, ctx) = (tx.clone(), ctx.clone());
    let (listing_id, generation, path) = (tab.listing_id, tab.generation, tab.path.clone());
    let cancel = tab.cancel.clone();
    std::thread::spawn(move || {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let result = fsops::list_dir(&path);
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let _ = tx.send(Listing {
            listing_id,
            generation,
            soft,
            result,
        });
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

/// The first image type the clipboard offers, if any.
fn wl_paste_image_type() -> Option<&'static str> {
    let out = crate::thumbs::run_tool_for(
        std::process::Command::new("wl-paste").arg("--list-types"),
        std::time::Duration::from_secs(2),
    )
    .ok()?;
    let types = String::from_utf8_lossy(&out);
    ops::CLIPBOARD_IMAGES
        .iter()
        .find(|(m, _)| types.lines().any(|t| t.trim() == *m))
        .map(|(m, _)| *m)
}

/// Files copied in GTK/Qt file managers are offered as text/uri-list.
fn wl_paste_uri_list() -> Vec<PathBuf> {
    // The clipboard owner may be unresponsive, so don't wait on it forever.
    let out = crate::thumbs::run_tool_for(
        std::process::Command::new("wl-paste").args(["--no-newline", "--type", "text/uri-list"]),
        std::time::Duration::from_secs(2),
    );
    out.map(|o| parse_path_list(&String::from_utf8_lossy(&o)))
        .unwrap_or_default()
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
