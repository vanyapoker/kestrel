use crate::fsops::Entry;
use crate::search::Query;
use crate::util;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SortKey {
    Name,
    Size,
    Modified,
    Kind,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ViewMode {
    /// One row per item with size, date and type columns.
    Details,
    /// Compact cells with an icon and name, laid out in a grid.
    List,
    /// Thumbnail tiles; the index picks a size from `ICON_SIZES`.
    Icons(usize),
}

/// Thumbnail edge length for each icon zoom level.
pub const ICON_SIZES: [f32; 6] = [48.0, 72.0, 96.0, 128.0, 176.0, 240.0];

impl ViewMode {
    fn level(self) -> usize {
        match self {
            ViewMode::Details => 0,
            ViewMode::List => 1,
            ViewMode::Icons(i) => 2 + i.min(ICON_SIZES.len() - 1),
        }
    }

    /// Steps through Details → List → icons, smallest to largest.
    pub fn zoom(self, steps: i32) -> Self {
        let max = ICON_SIZES.len() as i32 + 1;
        match (self.level() as i32 + steps).clamp(0, max) {
            0 => ViewMode::Details,
            1 => ViewMode::List,
            l => ViewMode::Icons(l as usize - 2),
        }
    }

    /// Whether items flow in a grid (arrow keys move in two dimensions).
    pub fn is_grid(self) -> bool {
        self != ViewMode::Details
    }
}

pub struct Search {
    pub query: String,
    pub root: PathBuf,
    pub generation: u64,
    pub cancel: Arc<AtomicBool>,
    pub running: bool,
    pub truncated: bool,
    /// Follow the highest-ranked result until the user selects an item.
    pub follow_best: bool,
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
            mode: ViewMode::Details,
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
        if let Some(s) = &mut self.search {
            s.follow_best = false;
        }
        if self.view.is_empty() {
            return;
        }
        let vi = vi.min(self.view.len() - 1);
        self.marked.clear();
        self.cursor = Some(vi);
        self.anchor = Some(vi);
        self.scroll_to_cursor = true;
    }

    /// A changed query or a new search batch selects the best match until the
    /// user takes over with the mouse or keyboard.
    pub fn select_best_match(&mut self) {
        self.select_only(0);
        if let Some(s) = &mut self.search {
            s.follow_best = true;
        }
    }

    /// Ctrl+click: add or remove one item.
    pub fn toggle_mark(&mut self, vi: usize) {
        if let Some(s) = &mut self.search {
            s.follow_best = false;
        }
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
        if let Some(s) = &mut self.search {
            s.follow_best = false;
        }
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
        if let Some(s) = &mut self.search {
            s.follow_best = false;
        }
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

    /// After a background reload, puts the cursor and anchor back on the same files
    /// without scrolling or touching the marks. If the cursor's file is gone, the
    /// cursor stays at the same position, like after a delete.
    pub fn restore_selection(&mut self, cursor: Option<(usize, PathBuf)>, anchor: Option<PathBuf>) {
        let find = |p: &PathBuf| self.view.iter().position(|&i| &self.entries[i].path == p);
        if let Some((vi, path)) = cursor {
            self.cursor = find(&path).or_else(|| self.view.len().checked_sub(1).map(|last| vi.min(last)));
        }
        self.anchor = anchor.and_then(|p| find(&p)).or(self.cursor);
    }

    pub fn apply_view(&mut self, sizes: &HashMap<PathBuf, u64>, show_hidden: bool) {
        let keep = self.cursor_entry().map(|e| e.path.clone());
        let anchor = self.anchor.and_then(|vi| self.entry(vi)).map(|e| e.path.clone());
        let query = Query::new(&match &self.search {
            Some(s) => format!("{} {}", s.query, self.filter),
            None => self.filter.clone(),
        });
        let entries = &self.entries;
        let in_search = self.search.is_some();
        let mut ranked: Vec<_> = (0..entries.len())
            .filter_map(|i| {
                let e = &entries[i];
                if !show_hidden && !in_search && e.hidden {
                    return None;
                }
                query.score(&e.name).map(|score| (i, score))
            })
            .collect();
        let size_of = |e: &Entry| if e.is_dir { sizes.get(&e.path).copied().unwrap_or(0) } else { e.size };
        let (sort, asc) = (self.sort, self.ascending);
        ranked.sort_by(|&(a, a_score), &(b, b_score)| {
            let (x, y) = (&entries[a], &entries[b]);
            b_score.cmp(&a_score).then_with(|| y.is_dir.cmp(&x.is_dir)).then_with(|| {
                let ord = match sort {
                    SortKey::Name => util::natural_cmp(&x.name, &y.name),
                    SortKey::Size => size_of(x).cmp(&size_of(y)),
                    SortKey::Modified => x.modified.cmp(&y.modified),
                    SortKey::Kind => x.ext.cmp(&y.ext),
                };
                let ord = if asc { ord } else { ord.reverse() };
                ord.then_with(|| util::natural_cmp(&x.name, &y.name))
                    .then_with(|| x.path.components().count().cmp(&y.path.components().count()))
                    .then_with(|| x.path.cmp(&y.path))
            })
        });
        self.view = ranked.into_iter().map(|(i, _)| i).collect();
        let visible: HashSet<&PathBuf> = self.view.iter().map(|&i| &entries[i].path).collect();
        self.marked.retain(|p| visible.contains(p));
        self.cursor = keep
            .and_then(|p| self.view.iter().position(|&i| entries[i].path == p))
            .or(if self.view.is_empty() { None } else { Some(0) });
        self.anchor = anchor.and_then(|p| self.view.iter().position(|&i| entries[i].path == p)).or(self.cursor);
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
    fn zoom_levels() {
        let m = ViewMode::Details;
        assert_eq!(m.zoom(-1), ViewMode::Details);
        assert_eq!(m.zoom(1), ViewMode::List);
        assert_eq!(m.zoom(2), ViewMode::Icons(0));
        assert_eq!(m.zoom(100), ViewMode::Icons(ICON_SIZES.len() - 1));
        assert_eq!(ViewMode::Icons(0).zoom(-2), ViewMode::Details);
    }

    #[test]
    fn filtered_views_rank_relevance_before_folders_and_column_sort() {
        let mut t = tab_with(&["Named", "nd-notes", "NaDya", "a-nd", "ND", "other"]);
        t.entries[0].is_dir = true;
        t.entries[0].size = u64::MAX;
        t.sort = SortKey::Size;
        t.ascending = false;
        t.filter = "nd".into();
        t.apply_view(&HashMap::new(), true);
        let names: Vec<_> = t.view.iter().map(|&i| t.entries[i].name.as_str()).collect();
        assert_eq!(names, ["ND", "nd-notes", "a-nd", "NaDya", "Named"]);

        // Without a query the chosen column sort and folders-first order return.
        t.filter.clear();
        t.apply_view(&HashMap::new(), true);
        assert_eq!(t.entry(0).unwrap().name, "Named");
        assert_eq!(t.view.len(), 6);
    }

    #[test]
    fn fuzzy_filter_still_respects_hidden_files_and_natural_ties() {
        let mut t = tab_with(&[".nd", "NaDya10", "NaDya02", "NaDya01", "other"]);
        t.filter = "nd".into();
        t.apply_view(&HashMap::new(), false);
        let names: Vec<_> = t.view.iter().map(|&i| t.entries[i].name.as_str()).collect();
        assert_eq!(names, ["NaDya01", "NaDya02", "NaDya10"]);
        t.apply_view(&HashMap::new(), true);
        assert_eq!(t.entry(0).unwrap().name, ".nd");
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
