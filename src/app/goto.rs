//! Folder and tab switcher. Candidate building performs no filesystem I/O,
//! so disconnected bookmarks cannot stall the popup.

use super::{Action, App};
use std::collections::HashSet;
use std::path::PathBuf;

impl App {
    pub(super) fn remember_dir(&mut self, path: PathBuf) {
        self.recent_dirs.retain(|p| p != &path);
        self.recent_dirs.insert(0, path);
        self.recent_dirs.truncate(64);
    }

    pub(super) fn goto_items(&self) -> Vec<(String, String, Action)> {
        let mut items = Vec::new();
        let mut seen = HashSet::new();
        // Open tabs take precedence: jumping to an already-open folder switches
        // to its tab instead of opening the same folder again in the current tab.
        for (pi, pane) in self.panes.iter().enumerate() {
            for (ti, tab) in pane.tabs.iter().enumerate() {
                if seen.insert(tab.path.clone()) {
                    let name = self
                        .places
                        .bookmarks
                        .iter()
                        .find(|(_, path)| path == &tab.path)
                        .map(|(name, _)| name.clone())
                        .unwrap_or_else(|| tab.title());
                    items.push((
                        name,
                        format!("Open tab · {}", tab.path.display()),
                        Action::SwitchTab(pi, ti),
                    ));
                }
            }
        }
        for (i, closed) in self.closed_tabs.iter().enumerate().rev() {
            if let Some(tab) = closed.active()
                && seen.insert(tab.path.clone()) {
                    let name = tab
                        .path
                        .file_name()
                        .unwrap_or(tab.path.as_os_str())
                        .to_string_lossy();
                    items.push((
                        name.into_owned(),
                        format!("Closed tab · {}", tab.path.display()),
                        Action::ReopenClosed(i),
                    ));
                }
        }
        for (name, path) in &self.places.bookmarks {
            if seen.insert(path.clone()) {
                items.push((
                    name.clone(),
                    format!("Bookmark · {}", path.display()),
                    Action::Navigate(path.clone()),
                ));
            }
        }
        for path in &self.recent_dirs {
            if seen.insert(path.clone()) {
                let name = path
                    .file_name()
                    .unwrap_or(path.as_os_str())
                    .to_string_lossy()
                    .into_owned();
                items.push((
                    name,
                    format!("Recent · {}", path.display()),
                    Action::Navigate(path.clone()),
                ));
            }
        }
        for (name, path) in &self.places.places {
            if seen.insert(path.clone()) {
                items.push((
                    name.clone(),
                    format!("Place · {}", path.display()),
                    Action::Navigate(path.clone()),
                ));
            }
        }
        items
    }
}
