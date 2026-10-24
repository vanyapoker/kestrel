use super::{App, Pane};
use crate::session::{PaneState, TabState};
use crate::tab::Tab;

pub(super) enum Closed {
    Tab {
        pane: usize,
        position: usize,
        state: TabState,
    },
    Pane {
        position: usize,
        state: PaneState,
    },
}
impl Closed {
    pub fn active(&self) -> Option<&TabState> {
        match self {
            Self::Tab { state, .. } => Some(state),
            Self::Pane { state, .. } => state.tabs.get(state.active),
        }
    }
}

pub(super) fn state(tab: &Tab) -> TabState {
    TabState {
        path: tab.path.clone(),
        sort: tab.sort,
        ascending: tab.ascending,
        mode: tab.mode,
    }
}

impl App {
    pub(super) fn move_tab(&mut self, id: u64, pane: usize, before: Option<u64>) {
        if before == Some(id) || pane >= self.panes.len() {
            return;
        }
        let Some((source, index)) = self
            .panes
            .iter()
            .enumerate()
            .find_map(|(p, pane)| pane.tabs.iter().position(|t| t.id == id).map(|i| (p, i)))
        else {
            return;
        };
        let active: Vec<_> = self.panes.iter().map(|p| p.tabs[p.active].id).collect();
        let tab = self.panes[source].tabs.remove(index);
        let position = before
            .and_then(|id| self.panes[pane].tabs.iter().position(|t| t.id == id))
            .unwrap_or(self.panes[pane].tabs.len());
        self.panes[pane].tabs.insert(position, tab);
        for (p, active) in self.panes.iter_mut().zip(active) {
            p.active = p
                .tabs
                .iter()
                .position(|t| t.id == active)
                .unwrap_or_else(|| p.active.min(p.tabs.len().saturating_sub(1)));
        }
        self.panes[pane].active = position;
        self.active_pane = pane;
        if self.panes[source].tabs.is_empty() {
            self.panes.remove(source);
            if self.active_pane > source {
                self.active_pane -= 1;
            }
        }
        self.remember_dir(self.tab().path.clone());
    }

    fn restored_tab(&mut self, state: &TabState) -> Tab {
        let path = state
            .path
            .ancestors()
            .find(|p| p.is_dir())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(crate::fsops::home_dir);
        let mut tab = self.new_tab(path);
        (tab.sort, tab.ascending, tab.mode) = (state.sort, state.ascending, state.mode);
        tab
    }

    pub(super) fn reopen_closed(&mut self, index: usize) {
        if index >= self.closed_tabs.len() {
            return;
        }
        match self.closed_tabs.remove(index) {
            Closed::Tab {
                pane,
                position,
                state,
            } => {
                let tab = self.restored_tab(&state);
                let pane = pane.min(self.panes.len() - 1);
                let position = position.min(self.panes[pane].tabs.len());
                self.panes[pane].tabs.insert(position, tab);
                self.panes[pane].active = position;
                self.active_pane = pane;
            }
            Closed::Pane { position, state } => {
                let tabs: Vec<_> = state.tabs.iter().map(|s| self.restored_tab(s)).collect();
                if tabs.is_empty() {
                    return;
                }
                let active = state.active.min(tabs.len() - 1);
                if self.panes.len() < 2 {
                    let position = position.min(self.panes.len());
                    self.panes.insert(position, Pane { tabs, active });
                    self.active_pane = position;
                } else {
                    let pane = position.min(self.panes.len() - 1);
                    let start = self.panes[pane].tabs.len();
                    self.panes[pane].tabs.extend(tabs);
                    self.panes[pane].active = start + active;
                    self.active_pane = pane;
                }
            }
        }
        self.remember_dir(self.tab().path.clone());
    }

    pub(super) fn save_workspace(&mut self, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            self.toast("Give the workspace a name", true);
            return;
        }
        let mut session = self.session();
        session.recent_dirs.clear(); // Navigation history remains global.
        let previous = self.workspaces.insert(name.to_string(), session);
        if let Some(path) = &self.workspace_file
            && let Err(error) = crate::workspaces::save(path, &self.workspaces) {
                match previous {
                    Some(previous) => {
                        self.workspaces.insert(name.to_string(), previous);
                    }
                    None => {
                        self.workspaces.remove(name);
                    }
                }
                self.toast(format!("Could not save workspace: {error}"), true);
                return;
            }
        self.toast(format!("Saved workspace “{name}”"), false);
    }

    pub(super) fn load_workspace(&mut self, name: &str) {
        let Some(session) = self.workspaces.get(name).cloned() else {
            return;
        };
        let recents = self.recent_dirs.clone();
        self.restore(&session, true);
        for path in recents.into_iter().rev() {
            self.remember_dir(path);
        }
        self.remember_dir(self.tab().path.clone());
        self.inspector.clear();
        self.path_edit = None;
        self.preview.invalidate();
        self.toast(format!("Loaded workspace “{name}”"), false);
    }

    pub(super) fn delete_workspace(&mut self, name: &str) {
        let Some(previous) = self.workspaces.remove(name) else {
            return;
        };
        if let Some(path) = &self.workspace_file
            && let Err(error) = crate::workspaces::save(path, &self.workspaces) {
                self.workspaces.insert(name.to_string(), previous);
                self.toast(format!("Could not delete workspace: {error}"), true);
            }
    }
}
