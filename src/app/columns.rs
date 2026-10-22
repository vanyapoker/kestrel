//! Passive columns keep directory state; the focused directory lives in the real Tab.
//! Listing identities travel with directory state, independently of stable tab IDs.
use super::*;

use crate::tab::Columns;

impl Drop for Columns {
    fn drop(&mut self) {
        for (i, tab) in self.items.iter().enumerate() {
            if i != self.active { tab.cancel.store(true, Ordering::Relaxed); }
        }
    }
}

fn swap_directory(a: &mut Tab, b: &mut Tab) {
    macro_rules! swap { ($($field:ident),* $(,)?) => { $(std::mem::swap(&mut a.$field, &mut b.$field);)* }; }
    swap!(path, listing_id, entries, view, cursor, marked, anchor, filter, loading, error,
        generation, cancel, select_after_load, scroll_to_cursor, force_offset, scroll_offset,
        viewport_h, grid_cols, disk, reload_at);
}

pub(super) fn find_listing(panes: &mut [Pane], id: u64) -> Option<&mut Tab> {
    for tab in panes.iter_mut().flat_map(|p| &mut p.tabs) {
        if tab.listing_id == id { return Some(tab); }
        if let Some(cols) = &mut tab.columns {
            for (i, column) in cols.items.iter_mut().enumerate() {
                if i != cols.active && column.listing_id == id { return Some(column); }
            }
        }
    }
    None
}

pub(super) fn watched_paths(tab: &Tab) -> impl Iterator<Item = PathBuf> + '_ {
    tab.columns.iter().filter(|_| tab.search.is_none()).flat_map(|c| c.items.iter().enumerate()
        .filter(move |(i, _)| *i != c.active).map(|(_, t)| t.path.clone()))
}

pub(super) fn changed(tab: &mut Tab, path: &Path, now: Instant) {
    if tab.search.is_some() { return; }
    if let Some(c) = &mut tab.columns {
        for (i, t) in c.items.iter_mut().enumerate() {
            if i != c.active && (t.path == path || Some(t.path.as_path()) == path.parent()) {
                t.reload_at.get_or_insert(now + WATCH_DEBOUNCE);
            }
        }
    }
}

fn new_column(path: PathBuf, next: &mut u64, tx: &Sender<Listing>, ctx: &egui::Context, sort: SortKey, asc: bool) -> Tab {
    let mut tab = Tab::new(*next, path);
    *next += 1;
    tab.sort = sort;
    tab.ascending = asc;
    start_listing(&mut tab, tx, ctx, false);
    tab
}

impl App {
    /// A failed passive listing must not silently turn into a duplicate ancestor column.
    pub(super) fn repair_missing_column(&mut self, listing_id: u64, generation: u64) -> bool {
        let found = self.panes.iter().enumerate().find_map(|(p, pane)| pane.tabs.iter().enumerate().find_map(|(t, tab)| {
            let c = tab.columns.as_ref()?;
            c.items.iter().enumerate().find(|(i, item)| *i != c.active && item.listing_id == listing_id && item.generation == generation)
                .map(|(i, item)| (p, t, i, item.path.clone(), c.active))
        }));
        let Some((p, t, index, path, active)) = found else { return false };
        if path.is_dir() { return false; }
        let tab = &mut self.panes[p].tabs[t];
        if index <= active {
            let fallback = tab.path.ancestors().find(|p| p.is_dir()).map(Path::to_path_buf).unwrap_or_else(fsops::home_dir);
            tab.columns = None;
            tab.path = fallback;
            start_listing(tab, &self.list_tx, &self.ctx, false);
        } else {
            let c = tab.columns.as_mut().unwrap();
            for item in &c.items[index..] { item.cancel.store(true, Ordering::Relaxed); }
            c.items.truncate(index);
            // Reload the source directory too, removing the vanished folder from its cursor.
            start_listing(tab, &self.list_tx, &self.ctx, true);
        }
        true
    }

    pub(super) fn set_view(&mut self, mode: ViewMode) {
        let tab = self.tab_mut();
        if tab.mode == mode { return; }
        if mode == ViewMode::Columns { tab.previous_view = tab.mode; }
        else { tab.columns = None; }
        tab.mode = mode;
        tab.scroll_to_cursor = true;
        self.sync_columns();
    }

    pub(super) fn sync_columns(&mut self) {
        let (tx, ctx, next) = (&self.list_tx, &self.ctx, &mut self.next_tab_id);
        for tab in self.panes.iter_mut().flat_map(|p| &mut p.tabs) {
            if tab.mode != ViewMode::Columns || tab.search.is_some() {
                tab.columns = None;
                continue;
            }
            if tab.columns.as_ref().is_some_and(|c| c.items[c.active].path != tab.path) {
                tab.columns = None;
            }
            if tab.columns.is_none() {
                let mut items = Vec::new();
                if let Some(parent) = tab.path.parent() {
                    let mut t = new_column(parent.to_path_buf(), next, tx, ctx, tab.sort, tab.ascending);
                    t.select_after_load = Some(tab.path.clone());
                    items.push(t);
                }
                let active = items.len();
                items.push(Tab::new(*next, tab.path.clone()));
                *next += 1;
                tab.columns = Some(Columns { items, active, scroll_to_active: true });
            }
            let child = tab.cursor_entry().filter(|e| e.is_dir).map(|e| e.path.clone());
            let cols = tab.columns.as_mut().unwrap();
            // Only the active cursor reveals a child. Loading passive columns never cascades.
            if !tab.loading {
                let expected = cols.items.get(cols.active + 1).map(|t| t.path.clone());
                if child != expected {
                    for t in &cols.items[cols.active + 1..] { t.cancel.store(true, Ordering::Relaxed); }
                    cols.items.truncate(cols.active + 1);
                    if let Some(path) = child {
                        cols.items.push(new_column(path, next, tx, ctx, tab.sort, tab.ascending));
                    }
                }
            }
            for (i, t) in cols.items.iter_mut().enumerate() {
                if i == cols.active { continue; }
                if t.sort != tab.sort || t.ascending != tab.ascending {
                    t.sort = tab.sort;
                    t.ascending = tab.ascending;
                    t.apply_view(&self.sizes, self.show_hidden);
                }
                if t.reload_at.is_some_and(|at| at <= Instant::now()) {
                    t.reload_at = None;
                    start_listing(t, tx, ctx, true);
                } else if let Some(at) = t.reload_at {
                    ctx.request_repaint_after(at.saturating_duration_since(Instant::now()));
                }
            }
        }
    }

    pub(super) fn activate_column_path(&mut self, id: u64, directory: &Path) -> bool {
        let Some((p, t)) = self.panes.iter().enumerate().find_map(|(p, pane)|
            pane.tabs.iter().position(|t| t.id == id).map(|t| (p, t))) else { return false };
        self.active_pane = p;
        self.panes[p].active = t;
        let tab = self.tab_mut();
        let Some(cols) = &tab.columns else { return false };
        let Some(index) = cols.items.iter().position(|t| t.path == directory) else { return false };
        self.activate_column(index);
        true
    }

    fn activate_column(&mut self, index: usize) {
        let tab = self.tab_mut();
        let Some(mut cols) = tab.columns.take() else { return };
        if index >= cols.items.len() || index == cols.active {
            tab.columns = Some(cols);
            return;
        }
        let old = tab.path.clone();
        swap_directory(tab, &mut cols.items[cols.active]);
        swap_directory(tab, &mut cols.items[index]);
        cols.items[index].path = tab.path.clone();
        cols.active = index;
        cols.scroll_to_active = true;
        tab.scroll_to_cursor = true;
        if tab.path != old {
            tab.back.push(old);
            tab.fwd.clear();
        }
        tab.columns = Some(cols);
        self.remember_dir(self.tab().path.clone());
        self.sync_sizes(Instant::now());
    }

    pub(super) fn column_step(&mut self, delta: i32) {
        self.sync_columns();
        if self.tab().search.is_some() || self.tab().mode != ViewMode::Columns { return; }
        let Some(cols) = &self.tab().columns else { return };
        let active = cols.active;
        if delta > 0 {
            if cols.items.len() > active + 1 { self.activate_column(active + 1); }
        } else if active > 0 {
            self.activate_column(active - 1);
        } else if let Some(parent) = self.tab().path.parent().map(Path::to_path_buf) {
            let path = self.tab().path.clone();
            let (sort, asc) = (self.tab().sort, self.tab().ascending);
            let mut t = new_column(parent, &mut self.next_tab_id, &self.list_tx, &self.ctx, sort, asc);
            t.select_after_load = Some(path);
            let c = self.tab_mut().columns.as_mut().unwrap();
            c.items.insert(0, t);
            c.active += 1;
            self.activate_column(0);
        }
    }
}
