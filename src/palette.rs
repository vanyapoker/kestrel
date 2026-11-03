//! Command palette (Ctrl+P): every command plus "go to" entries for places.

use crate::apps::Choices;
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cmd {
    NewTab,
    CloseTab,
    ReopenTab,
    SaveWorkspace,
    Workspaces,
    NewFolder,
    NewFile,
    Rename,
    BatchRename,
    Trash,
    DeletePermanently,
    Restore,
    EmptyTrash,
    Copy,
    Cut,
    Paste,
    Duplicate,
    SelectAll,
    SelectPattern,
    InvertSelection,
    Properties,
    Search,
    Flatten,
    Refresh,
    ToggleHidden,
    TogglePreview,
    ToggleSidebar,
    BookmarkFolder,
    ZoomIn,
    ZoomOut,
    ToggleSplit,
    ToggleGrid,
    ToggleColumns,
    CopyPath,
    OpenTerminal,
    EditPath,
    GoTo,
    GoHome,
    GoUp,
    Back,
    Forward,
    NextTab,
    PrevTab,
    FocusOtherPane,
    UnbookmarkFolder,
    ReverseSort,
    Help,
    CopyToOtherPane,
    MoveToOtherPane,
    SortName,
    SortSize,
    SortModified,
    SortType,
    CancelJob,
    Undo,
    Extract,
    ExtractFolder,
    ExtractOther,
    Compress,
    CompressAs,
}

/// A palette entry for a built-in command.
pub struct Command {
    pub cmd: Cmd,
    pub name: &'static str,
    /// Shortcut label; alternatives are separated by " / ".
    pub keys: &'static str,
    /// Extra words the command is found by, never shown.
    pub keywords: &'static str,
}

const fn c(cmd: Cmd, name: &'static str, keys: &'static str, keywords: &'static str) -> Command {
    Command {
        cmd,
        name,
        keys,
        keywords,
    }
}

#[rustfmt::skip]
pub const COMMANDS: &[Command] = &[
    c(Cmd::Search, "Search in this folder (recursive)", "Ctrl+F", "find"),
    c(Cmd::Flatten, "Toggle flattened folder view", "Ctrl+Shift+F", "recursive all files"),
    c(Cmd::NewTab, "New tab", "Ctrl+T", "open"),
    c(Cmd::CloseTab, "Close tab", "Ctrl+W", ""),
    c(Cmd::NextTab, "Next tab", "Ctrl+Tab", "switch"),
    c(Cmd::PrevTab, "Previous tab", "Ctrl+Shift+Tab", "switch"),
    c(Cmd::ReopenTab, "Reopen closed tab or split", "Ctrl+Shift+T", "restore undo"),
    c(Cmd::SaveWorkspace, "Save current workspace…", "", "session"),
    c(Cmd::Workspaces, "Workspaces: load or delete…", "", "session"),
    c(Cmd::ToggleSplit, "Toggle split view", "F6 / Ctrl+\\", "dual"),
    c(Cmd::FocusOtherPane, "Switch to other pane", "Tab", "focus split"),
    c(Cmd::ToggleColumns, "Toggle Miller columns", "Ctrl+Shift+M", "view"),
    c(Cmd::ToggleGrid, "Toggle icons / details view", "Ctrl+G", "grid list"),
    c(Cmd::ZoomIn, "Larger view (details → list → icons)", "Ctrl+Wheel up", "zoom bigger"),
    c(Cmd::ZoomOut, "Smaller view", "Ctrl+Wheel down", "zoom"),
    c(Cmd::TogglePreview, "Toggle preview panel", "Space / F3", "inspector"),
    c(Cmd::ToggleSidebar, "Expand / collapse sidebar labels", "Ctrl+B", ""),
    c(Cmd::BookmarkFolder, "Bookmark this folder", "", "favorite add"),
    c(Cmd::UnbookmarkFolder, "Remove this folder from bookmarks", "", "unbookmark favorite"),
    c(Cmd::ToggleHidden, "Toggle hidden files", "Ctrl+H", "dotfiles show hide"),
    c(Cmd::NewFolder, "New folder", "Ctrl+Shift+N", "mkdir directory create"),
    c(Cmd::NewFile, "New empty file", "", "touch create"),
    c(Cmd::Undo, "Undo", "Ctrl+Z", ""),
    c(Cmd::Rename, "Rename", "F2", "mv"),
    c(Cmd::BatchRename, "Batch rename selection…", "", "bulk mass"),
    c(Cmd::Copy, "Copy", "Ctrl+C", ""),
    c(Cmd::Cut, "Cut", "Ctrl+X", "move"),
    c(Cmd::Paste, "Paste", "Ctrl+V", ""),
    c(Cmd::CopyToOtherPane, "Copy selection to the other pane", "", "transfer split left right"),
    c(Cmd::MoveToOtherPane, "Move selection to the other pane", "", "transfer split left right"),
    c(Cmd::Duplicate, "Duplicate", "Ctrl+D", "clone"),
    c(Cmd::Extract, "Extract here", "", "unzip unpack decompress"),
    c(Cmd::ExtractFolder, "Extract to a folder named after the archive", "", "unzip unpack"),
    c(Cmd::ExtractOther, "Extract to the other pane's folder", "", "unzip unpack"),
    c(Cmd::Compress, "Compress to .zip", "", "archive pack"),
    c(Cmd::CompressAs, "Compress… (format, level, name)", "", "archive pack tar 7z"),
    c(Cmd::Trash, "Move to trash", "Delete", "trash delete remove rm"),
    c(Cmd::DeletePermanently, "Delete permanently", "Shift+Delete", "remove rm"),
    c(Cmd::Restore, "Restore from trash", "", "untrash recover"),
    c(Cmd::EmptyTrash, "Empty trash…", "", "clear"),
    c(Cmd::SelectAll, "Select all", "Ctrl+A", ""),
    c(Cmd::SelectPattern, "Select by pattern…", "Ctrl+S", "glob wildcard"),
    c(Cmd::Properties, "Permissions and owner…", "", "chmod chown properties"),
    c(Cmd::InvertSelection, "Invert selection", "Ctrl+Shift+A", ""),
    c(Cmd::CopyPath, "Copy path", "Alt+C", "location"),
    c(Cmd::OpenTerminal, "Open terminal here", "Ctrl+Alt+T", "shell console"),
    c(Cmd::EditPath, "Go to path…", "Ctrl+L", "location address"),
    c(Cmd::GoTo, "Go To: folders and open tabs…", "Ctrl+K", "jump switch"),
    c(Cmd::GoHome, "Go home", "Alt+Home", ""),
    c(Cmd::GoUp, "Go up", "Backspace / Alt+Up", "parent"),
    c(Cmd::Back, "Back", "Alt+Left", "previous history"),
    c(Cmd::Forward, "Forward", "Alt+Right", "next history"),
    c(Cmd::Refresh, "Refresh", "F5", "reload"),
    c(Cmd::SortName, "Sort by name", "", "order"),
    c(Cmd::SortSize, "Sort by size", "", "order"),
    c(Cmd::SortModified, "Sort by date modified", "", "order time"),
    c(Cmd::SortType, "Sort by type", "", "order extension kind"),
    c(Cmd::ReverseSort, "Reverse sort order", "", "ascending descending"),
    c(Cmd::CancelJob, "Cancel current file operation", "", "stop abort"),
    c(Cmd::Help, "Help: commands and search syntax", "F1", "shortcuts keys keyboard filters"),
];

/// Keys that aren't palette commands, listed in Help with the commands.
pub const OTHER_KEYS: &[(&str, &str)] = &[
    ("Command palette", "Ctrl+P"),
    ("Filter this folder", "Start typing"),
    ("Clear filter or selection", "Esc"),
    ("Open", "Enter"),
    ("Switch to tab 1–9", "Ctrl+1…9"),
    ("Select or deselect item", "Ctrl+Space / Ctrl+click"),
    ("Extend selection", "Shift+arrows / Shift+click"),
    (
        "Select with a box; Ctrl adds to the selection",
        "Drag on empty space",
    ),
    (
        "Drop files: move on the same drive, copy to another",
        "Drag",
    ),
    ("Always move dropped files", "Shift+drag"),
    ("Always copy dropped files", "Ctrl+drag"),
    ("Move between Miller columns", "← / →"),
    ("Back / forward", "Mouse back / forward"),
];

/// Entries remembered as recently run.
const RECENT: usize = 8;

#[derive(Default)]
pub struct Palette {
    pub open: bool,
    pub goto: bool,
    pub query: String,
    pub selected: usize,
    pub focus: bool,
    /// Bring the selected row into view on the next frame.
    pub scroll_to_selected: bool,
    /// Names of recently run entries, newest first; listed first for an empty query.
    pub recent: Vec<String>,
    /// "Open with" choices for the file under the cursor, looked up once per file.
    pub open_with: Option<(PathBuf, Choices)>,
}

impl Palette {
    pub fn show(&mut self) {
        self.open = true;
        self.goto = false;
        self.query.clear();
        self.selected = 0;
        self.focus = true;
        self.scroll_to_selected = true;
        self.open_with = None;
    }

    pub fn show_goto(&mut self) {
        self.show();
        self.goto = true;
    }

    /// Switches between commands and Go To, keeping what was typed.
    pub fn switch_mode(&mut self, goto: bool) {
        self.goto = goto;
        self.selected = 0;
        self.focus = true;
        self.scroll_to_selected = true;
    }

    pub fn remember(&mut self, name: &str) {
        self.recent.retain(|r| r != name);
        self.recent.insert(0, name.to_string());
        self.recent.truncate(RECENT);
    }
}

/// Score for a palette entry: the best of its name, its hidden keywords, and
/// its shortcut typed out ("ctrl+h", "f6").
pub fn entry_score(query: &str, name: &str, keywords: &str, keys: &str) -> Option<i32> {
    let compact = |s: &str| -> String {
        s.chars()
            .filter(|c| !c.is_whitespace() && *c != '+')
            .flat_map(char::to_lowercase)
            .collect()
    };
    let typed = compact(query);
    let shortcut =
        (!typed.is_empty() && keys.split(" / ").any(|k| compact(k) == typed)).then_some(50);
    // Each keyword on its own, so letters can't be picked across unrelated words.
    let keyword = keywords
        .split(' ')
        .filter_map(|k| fuzzy_score(query, k))
        .max()
        .map(|s| s - 2);
    [fuzzy_score(query, name), keyword, shortcut]
        .into_iter()
        .flatten()
        .max()
}

/// Subsequence match score; higher is better. Takes the best alignment rather
/// than the first, rewarding word starts, consecutive runs and whole words, and
/// charging for words skipped, so "bm" ranks "Bookmark…" above "Sort by date
/// modified" and "pane" finds "other pane" before "preview panel".
pub fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    let q: Vec<char> = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    if q.is_empty() {
        return Some(0);
    }
    let t: Vec<char> = text.chars().flat_map(char::to_lowercase).collect();
    let n = t.len();
    let start: Vec<bool> = (0..n)
        .map(|i| t[i].is_alphanumeric() && (i == 0 || !t[i - 1].is_alphanumeric()))
        .collect();
    // words[i]: word starts before position i, to count the words a match skips.
    let mut words = vec![0i32; n + 1];
    for i in 0..n {
        words[i + 1] = words[i] + start[i] as i32;
    }
    // best[j]: best score with the latest query char matched at text position j.
    let mut best: Vec<Option<i32>> = vec![None; n];
    for (qi, &qc) in q.iter().enumerate() {
        let mut next = vec![None; n];
        for j in (0..n).filter(|&j| t[j] == qc) {
            let from = if qi == 0 {
                Some(if j == 0 { 4 } else { 0 } - 2 * words[j])
            } else {
                (0..j)
                    .filter_map(|k| {
                        let gap = j - k - 1;
                        let step = if gap == 0 {
                            5
                        } else {
                            -(gap.min(10) as i32) / 3
                        };
                        best[k].map(|s| s + step - 3 * (words[j] - words[k + 1]))
                    })
                    .max()
            };
            let word_end = j + 1 == n || !t[j + 1].is_alphanumeric();
            let run_end = word_end && j > 0 && best[j - 1].is_some();
            next[j] =
                from.map(|s| s + 1 + if start[j] { 8 } else { 0 } + if run_end { 3 } else { 0 });
        }
        best = next;
    }
    best.into_iter().flatten().max()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy() {
        assert!(
            fuzzy_score("nf", "New folder").unwrap()
                > fuzzy_score("nf", "Cancel file operation").unwrap_or(0)
        );
        assert!(fuzzy_score("xyz", "New folder").is_none());
        assert!(fuzzy_score("split", "Toggle split view").is_some());
    }

    fn top(query: &str) -> &'static str {
        COMMANDS
            .iter()
            .filter_map(|c| entry_score(query, c.name, c.keywords, c.keys).map(|s| (s, c.name)))
            .max_by(|a, b| a.0.cmp(&b.0).then(b.1.len().cmp(&a.1.len())))
            .unwrap()
            .1
    }

    #[test]
    fn ranking_prefers_clean_matches() {
        assert_eq!(top("bm"), "Bookmark this folder");
        assert_eq!(top("pane"), "Switch to other pane");
        assert_eq!(top("term"), "Open terminal here");
        assert_eq!(top("copy"), "Copy");
        assert_eq!(top("trash"), "Move to trash");
        assert_eq!(top("mkdir"), "New folder");
        assert_eq!(top("ctrl+h"), "Toggle hidden files");
        assert_eq!(top("F6"), "Toggle split view");
        assert_eq!(top("alt up"), "Go up");
    }

    #[test]
    fn recent_entries_are_unique_and_bounded() {
        let mut p = Palette::default();
        for i in 0..20 {
            p.remember(&format!("cmd {}", i % 10));
        }
        p.remember("cmd 3");
        assert_eq!(p.recent.len(), RECENT);
        assert_eq!(p.recent[0], "cmd 3");
        assert_eq!(p.recent.iter().filter(|r| *r == "cmd 3").count(), 1);
    }
}
