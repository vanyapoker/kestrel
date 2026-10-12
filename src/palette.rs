//! Command palette (Ctrl+P): every command plus "go to" entries for places.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cmd {
    NewTab,
    CloseTab,
    NewFolder,
    NewFile,
    Rename,
    Trash,
    DeletePermanently,
    Copy,
    Cut,
    Paste,
    Duplicate,
    SelectAll,
    Search,
    Refresh,
    ToggleHidden,
    TogglePreview,
    ToggleSidebar,
    BookmarkFolder,
    ZoomIn,
    ZoomOut,
    ToggleSplit,
    ToggleGrid,
    CopyPath,
    OpenTerminal,
    EditPath,
    GoHome,
    GoUp,
    Back,
    Forward,
    SortName,
    SortSize,
    SortModified,
    SortType,
    CancelJob,
}

pub const COMMANDS: &[(Cmd, &str, &str)] = &[
    (Cmd::Search, "Search in this folder (recursive)", "Ctrl+F"),
    (Cmd::NewTab, "New tab", "Ctrl+T"),
    (Cmd::CloseTab, "Close tab", "Ctrl+W"),
    (Cmd::ToggleSplit, "Toggle split view", "F6"),
    (Cmd::ToggleGrid, "Toggle icons / details view", "Ctrl+G"),
    (Cmd::ZoomIn, "Larger view (details → list → icons)", "Ctrl+Wheel up"),
    (Cmd::ZoomOut, "Smaller view", "Ctrl+Wheel down"),
    (Cmd::TogglePreview, "Toggle preview panel", "F3"),
    (Cmd::ToggleSidebar, "Toggle sidebar", "Ctrl+B"),
    (Cmd::BookmarkFolder, "Bookmark this folder", ""),
    (Cmd::ToggleHidden, "Toggle hidden files", "Ctrl+H"),
    (Cmd::NewFolder, "New folder", "Ctrl+Shift+N"),
    (Cmd::NewFile, "New empty file", ""),
    (Cmd::Rename, "Rename", "F2"),
    (Cmd::Copy, "Copy", "Ctrl+C"),
    (Cmd::Cut, "Cut", "Ctrl+X"),
    (Cmd::Paste, "Paste", "Ctrl+V"),
    (Cmd::Duplicate, "Duplicate", "Ctrl+D"),
    (Cmd::Trash, "Move to trash", "Delete"),
    (Cmd::DeletePermanently, "Delete permanently", "Shift+Delete"),
    (Cmd::SelectAll, "Select all", "Ctrl+A"),
    (Cmd::CopyPath, "Copy path", "Ctrl+Shift+C"),
    (Cmd::OpenTerminal, "Open terminal here", "Ctrl+Alt+T"),
    (Cmd::EditPath, "Go to path…", "Ctrl+L"),
    (Cmd::GoHome, "Go home", "Alt+Home"),
    (Cmd::GoUp, "Go up", "Backspace"),
    (Cmd::Back, "Back", "Alt+Left"),
    (Cmd::Forward, "Forward", "Alt+Right"),
    (Cmd::Refresh, "Refresh", "F5"),
    (Cmd::SortName, "Sort by name", ""),
    (Cmd::SortSize, "Sort by size", ""),
    (Cmd::SortModified, "Sort by date modified", ""),
    (Cmd::SortType, "Sort by type", ""),
    (Cmd::CancelJob, "Cancel current file operation", ""),
];

#[derive(Default)]
pub struct Palette {
    pub open: bool,
    pub query: String,
    pub selected: usize,
    pub focus: bool,
}

impl Palette {
    pub fn show(&mut self) {
        self.open = true;
        self.query.clear();
        self.selected = 0;
        self.focus = true;
    }
}

/// Subsequence match score; higher is better. Rewards consecutive matches and
/// matches at word starts, so "nf" ranks "New folder" above "Cancel… operation".
pub fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let text_chars: Vec<char> = text.chars().flat_map(char::to_lowercase).collect();
    let mut score = 0;
    let mut ti = 0;
    let mut prev_match: Option<usize> = None;
    for qc in query.chars().flat_map(char::to_lowercase) {
        if qc == ' ' {
            continue;
        }
        let pos = (ti..text_chars.len()).find(|&i| text_chars[i] == qc)?;
        let word_start = pos == 0 || !text_chars[pos - 1].is_alphanumeric();
        score += 1;
        if word_start {
            score += 8;
        }
        if prev_match == Some(pos.wrapping_sub(1)) {
            score += 5;
        }
        score -= (pos - ti).min(10) as i32 / 3;
        prev_match = Some(pos);
        ti = pos + 1;
    }
    Some(score)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy() {
        assert!(fuzzy_score("nf", "New folder").unwrap() > fuzzy_score("nf", "Cancel file operation").unwrap_or(0));
        assert!(fuzzy_score("xyz", "New folder").is_none());
        assert!(fuzzy_score("split", "Toggle split view").is_some());
    }
}
