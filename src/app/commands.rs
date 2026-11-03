//! What the command palette shows about each command: whether it can do
//! anything right now, and the current state of toggles and sorting. Like the
//! Go To list, this performs no filesystem I/O.

use super::App;
use crate::fsops;
use crate::palette::Cmd;
use crate::tab::{ICON_SIZES, SortKey, ViewMode};
use std::sync::atomic::Ordering;

impl App {
    /// Whether running `cmd` would do something, so the palette can hide it otherwise.
    pub(super) fn cmd_applies(&self, cmd: Cmd) -> bool {
        let tab = self.tab();
        let has_targets = tab.selection_count() > 0;
        let in_trash = fsops::in_trash(&tab.path);
        let bookmarked = self.places.bookmarks.iter().any(|(_, p)| *p == tab.path);
        let archives = || tab.targets().iter().any(|p| crate::ops::is_archive(p));
        match cmd {
            Cmd::ReopenTab => !self.closed_tabs.is_empty(),
            Cmd::NextTab | Cmd::PrevTab => self.panes[self.active_pane].tabs.len() > 1,
            Cmd::FocusOtherPane => self.panes.len() > 1,
            Cmd::CopyToOtherPane | Cmd::MoveToOtherPane => {
                self.panes.len() > 1
                    && has_targets
                    && self.tab_in_pane(1 - self.active_pane).path != tab.path
            }
            Cmd::NewFolder | Cmd::NewFile | Cmd::Paste => tab.search.is_none(),
            Cmd::Rename
            | Cmd::BatchRename
            | Cmd::Copy
            | Cmd::Cut
            | Cmd::Duplicate
            | Cmd::Compress
            | Cmd::CompressAs
            | Cmd::Properties
            | Cmd::CopyPath
            | Cmd::DeletePermanently => has_targets,
            // Inside the trash, Delete permanently is the only way to remove.
            Cmd::Trash => has_targets && !in_trash,
            Cmd::Restore => has_targets && in_trash,
            Cmd::Extract | Cmd::ExtractFolder => archives(),
            Cmd::ExtractOther => self.panes.len() > 1 && archives(),
            Cmd::Undo => !self.undo.is_empty(),
            Cmd::Back => !tab.back.is_empty(),
            Cmd::Forward => !tab.fwd.is_empty(),
            Cmd::GoUp => tab.path.parent().is_some(),
            Cmd::ZoomIn => {
                tab.mode != ViewMode::Columns && tab.mode != ViewMode::Icons(ICON_SIZES.len() - 1)
            }
            Cmd::ZoomOut => !matches!(tab.mode, ViewMode::Columns | ViewMode::Details),
            Cmd::BookmarkFolder => !bookmarked,
            Cmd::UnbookmarkFolder => bookmarked,
            Cmd::CancelJob => self.ops.queued.load(Ordering::Relaxed) > 0,
            _ => true,
        }
    }

    /// Palette name and detail for commands whose wording depends on the layout:
    /// "Move to right pane", with the destination folder as the detail.
    pub(super) fn cmd_label(&self, cmd: Cmd) -> Option<(String, String)> {
        let verb = match cmd {
            Cmd::CopyToOtherPane => "Copy",
            Cmd::MoveToOtherPane => "Move",
            _ => return None,
        };
        let other = 1 - self.active_pane.min(1);
        let side = if other == 1 { "right" } else { "left" };
        let dest = &self.tab_in_pane(other).path;
        let folder = dest.file_name().map_or_else(
            || dest.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        Some((format!("{verb} to {side} pane"), folder))
    }

    /// Current state shown beside a command, and whether it reads as "on".
    pub(super) fn cmd_state(&self, cmd: Cmd) -> Option<(&'static str, bool)> {
        let tab = self.tab();
        let toggle = |on: bool| Some((if on { "On" } else { "Off" }, on));
        let mode = match tab.mode {
            ViewMode::Details => "Details",
            ViewMode::Columns => "Columns",
            ViewMode::List => "List",
            ViewMode::Icons(_) => "Icons",
        };
        let order = match (tab.sort, tab.ascending) {
            (SortKey::Name | SortKey::Kind, true) => "A → Z",
            (SortKey::Name | SortKey::Kind, false) => "Z → A",
            (SortKey::Size, true) => "Smallest first",
            (SortKey::Size, false) => "Largest first",
            (SortKey::Modified, true) => "Oldest first",
            (SortKey::Modified, false) => "Newest first",
        };
        let sort = |key: SortKey| (tab.sort == key).then_some((order, true));
        match cmd {
            Cmd::ToggleHidden => toggle(self.show_hidden),
            Cmd::TogglePreview => toggle(self.show_preview),
            Cmd::ToggleSplit => toggle(self.panes.len() > 1),
            Cmd::ToggleColumns => toggle(tab.mode == ViewMode::Columns),
            Cmd::Flatten => toggle(tab.search.as_ref().is_some_and(|s| s.flatten)),
            Cmd::ToggleSidebar => Some(if self.show_sidebar {
                ("Expanded", true)
            } else {
                ("Icons only", false)
            }),
            Cmd::ToggleGrid | Cmd::ZoomIn | Cmd::ZoomOut => Some((mode, false)),
            Cmd::SortName => sort(SortKey::Name),
            Cmd::SortSize => sort(SortKey::Size),
            Cmd::SortModified => sort(SortKey::Modified),
            Cmd::SortType => sort(SortKey::Kind),
            Cmd::ReverseSort => Some((order, false)),
            _ => None,
        }
    }
}
