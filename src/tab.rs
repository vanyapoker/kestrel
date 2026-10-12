use crate::fsops::Entry;
use crate::util;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Size,
    Modified,
    Kind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    List,
    Grid,
}

pub struct Search {
    pub query: String,
    pub root: PathBuf,
    pub generation: u64,
    pub cancel: Arc<AtomicBool>,
    pub running: bool,
    pub truncated: bool,
    /// Restart the search once typing pauses.
    pub restart_at: Option<Instant>,
}

pub struct Tab {
    pub id: u64,
    pub path: PathBuf,
    pub back: Vec<PathBuf>,
    pub fwd: Vec<PathBuf>,
    pub entries: Vec<Entry>,
    /// Indices into `entries`, filtered and sorted.
    pub view: Vec<usize>,
    /// Keyboard cursor, index into `view`.
    pub cursor: Option<usize>,
    /// Explicit multi-selection. When empty, the cursor item is the selection.
    pub marked: HashSet<PathBuf>,
    /// Start of shift-selection ranges, index into `view`.
    pub anchor: Option<usize>,
    pub filter: String,
    pub sort: SortKey,
    pub ascending: bool,
    pub mode: ViewMode,
    pub loading: bool,
    pub error: Option<String>,
    pub generation: u64,
    pub cancel: Arc<AtomicBool>,
    pub select_after_load: Option<PathBuf>,
    pub scroll_to_cursor: bool,
    pub force_offset: Option<f32>,
    pub scroll_offset: f32,
    pub viewport_h: f32,
    pub grid_cols: usize,
    pub disk: Option<(u64, u64)>,
    pub search: Option<Search>,
    /// Debounced reload requested by the file watcher.
    pub reload_at: Option<Instant>,
}

impl Tab {
    pub fn new(id: u64, path: PathBuf) -> Self {
        Self {
            id,
            path,
            back: Vec::new(),
            fwd: Vec::new(),
            entries: Vec::new(),
            view: Vec::new(),
            cursor: None,
            marked: HashSet::new(),
            anchor: None,
            filter: String::new(),
            sort: SortKey::Name,
            ascending: true,
            mode: ViewMode::List,
            loading: false,
            error: None,
            generation: 0,
            cancel: Arc::new(AtomicBool::new(false)),
            select_after_load: None,
            scroll_to_cursor: false,
            force_offset: None,
            scroll_offset: 0.0,
            viewport_h: 0.0,
            grid_cols: 1,
            disk: None,
            search: None,
            reload_at: None,
        }
    }

    pub fn title(&self) -> String {
        let name = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/".into());
        if self.search.is_some() { format!("Search: {name}") } else { name }
    }

    pub fn entry(&self, vi: usize) -> Option<&Entry> {
        self.view.get(vi).and_then(|&i| self.entries.get(i))
    }

    pub fn cursor_entry(&self) -> Option<&Entry> {
        self.cursor.and_then(|vi| self.entry(vi))
    }

    pub fn is_selected(&self, vi: usize) -> bool {
        match self.entry(vi) {
            Some(e) if !self.marked.is_empty() => self.marked.contains(&e.path),
            Some(_) => self.cursor == Some(vi),
            None => false,
        }
    }

    /// Paths an operation should act on, in view order.
    pub fn targets(&self) -> Vec<PathBuf> {
        if self.marked.is_empty() {
            return self.cursor_entry().map(|e| vec![e.path.clone()]).unwrap_or_default();
        }
        self.view
            .iter()
            .map(|&i| &self.entries[i].path)
            .filter(|p| self.marked.contains(*p))
            .cloned()
            .collect()
    }

    pub fn selection_count(&self) -> usize {
        if self.marked.is_empty() { self.cursor.is_some() as usize } else { self.marked.len() }
    }

    /// Plain click / plain cursor move: single selection.
    pub fn select_only(&mut self, vi: usize) {
        if self.view.is_empty() {
            return;
        }
        let vi = vi.min(self.view.len() - 1);
        self.marked.clear();
        self.cursor = Some(vi);
        self.anchor = Some(vi);
        self.scroll_to_cursor = true;
    }

    /// Ctrl+click: add or remove one item.
    pub fn toggle_mark(&mut self, vi: usize) {
        let Some(path) = self.entry(vi).map(|e| e.path.clone()) else { return };
        if self.marked.is_empty() {
            if let Some(cur) = self.cursor_entry().map(|e| e.path.clone()) {
                if cur != path {
                    self.marked.insert(cur);
                }
            }
            self.marked.insert(path);
        } else if !self.marked.remove(&path) {
            self.marked.insert(path);
        }
        self.cursor = Some(vi);
        self.anchor = Some(vi);
    }

    /// Shift+click / shift+arrows: select the range from the anchor.
    pub fn select_range(&mut self, vi: usize) {
        if self.view.is_empty() {
            return;
        }
        let vi = vi.min(self.view.len() - 1);
        let anchor = self.anchor.unwrap_or(self.cursor.unwrap_or(0)).min(self.view.len() - 1);
        let (a, b) = if anchor <= vi { (anchor, vi) } else { (vi, anchor) };
        self.marked = (a..=b).map(|i| self.entries[self.view[i]].path.clone()).collect();
        self.cursor = Some(vi);
        self.anchor = Some(anchor);
        self.scroll_to_cursor = true;
    }

    pub fn select_all(&mut self) {
        self.marked = self.view.iter().map(|&i| self.entries[i].path.clone()).collect();
    }

    pub fn move_cursor(&mut self, delta: isize, extend: bool) {
        if self.view.is_empty() {
            return;
        }
        let cur = self.cursor.map(|s| s as isize).unwrap_or(-1);
        let next = (cur + delta).clamp(0, self.view.len() as isize - 1) as usize;
        if extend { self.select_range(next) } else { self.select_only(next) }
    }

    pub fn select_path(&mut self, path: &std::path::Path) -> bool {
        match self.view.iter().position(|&i| self.entries[i].path == path) {
            Some(vi) => {
                self.select_only(vi);
                true
            }
            None => false,
        }
    }

    pub fn apply_view(&mut self, sizes: &HashMap<PathBuf, u64>, show_hidden: bool) {
        let keep = self.cursor_entry().map(|e| e.path.clone());
        let needle = self.filter.to_lowercase();
        let entries = &self.entries;
        let in_search = self.search.is_some();
        let mut view: Vec<usize> = (0..entries.len())
            .filter(|&i| {
                let e = &entries[i];
                (show_hidden || in_search || !e.hidden)
                    && (needle.is_empty() || e.name.to_lowercase().contains(&needle))
            })
            .collect();
        let size_of = |e: &Entry| if e.is_dir { sizes.get(&e.path).copied().unwrap_or(0) } else { e.size };
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
        let visible: HashSet<&PathBuf> = self.view.iter().map(|&i| &entries[i].path).collect();
        self.marked.retain(|p| visible.contains(p));
        self.cursor = keep
            .and_then(|p| self.view.iter().position(|&i| entries[i].path == p))
            .or(if self.view.is_empty() { None } else { Some(0) });
        if self.anchor.is_some_and(|a| a >= self.view.len()) {
            self.anchor = self.cursor;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsops::make_entry;

    fn tab_with(names: &[&str]) -> Tab {
        let dir = std::env::temp_dir();
        let meta = std::fs::metadata(&dir).unwrap();
        let mut t = Tab::new(0, dir.clone());
        t.entries = names
            .iter()
            .map(|n| {
                let mut e = make_entry(dir.join(n), n.to_string(), meta.clone());
                e.is_dir = false;
                e
            })
            .collect();
        t.apply_view(&HashMap::new(), true);
        t
    }

    #[test]
    fn selection_model() {
        let mut t = tab_with(&["a", "b", "c", "d"]);
        assert_eq!(t.cursor, Some(0));
        assert_eq!(t.targets().len(), 1);
        t.toggle_mark(2);
        assert_eq!(t.selection_count(), 2); // a and c
        t.select_only(1);
        t.select_range(3);
        assert_eq!(t.selection_count(), 3); // b..d
        t.move_cursor(-1, false);
        assert_eq!(t.selection_count(), 1);
        t.select_all();
        assert_eq!(t.targets().len(), 4);
        t.filter = "a".into();
        t.apply_view(&HashMap::new(), true);
        assert_eq!(t.selection_count(), 1); // hidden items drop out of the selection
    }

    #[test]
    fn reload_with_fewer_entries() {
        let mut t = tab_with(&["a", "b", "c"]);
        t.select_only(2);
        // A reload replaces entries; view must not index the old ones.
        t.entries.truncate(1);
        t.view.clear();
        t.apply_view(&HashMap::new(), true);
        assert_eq!(t.cursor, Some(0));
        assert!(t.entry(5).is_none());
    }
}
