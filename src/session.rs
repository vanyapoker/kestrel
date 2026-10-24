//! Remembers open tabs, panels and view settings between runs.
//!
//! Stored as a small line-based text file in `$XDG_STATE_HOME/kestrel/session`:
//! one `key value` per line, with `pane` starting each pane and `tab` lines
//! (sort, order, view mode, then the path to the end of the line) inside it.

use crate::tab::{SortKey, ViewMode};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub struct TabState {
    pub path: PathBuf,
    pub sort: SortKey,
    pub ascending: bool,
    pub mode: ViewMode,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PaneState {
    pub tabs: Vec<TabState>,
    pub active: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    pub panes: Vec<PaneState>,
    pub active_pane: usize,
    pub show_hidden: bool,
    pub show_preview: bool,
    pub show_sidebar: bool,
    pub collapsed: Vec<String>,
    pub sidebar_width: Option<f32>,
    pub preview_width: Option<f32>,
    pub recent_dirs: Vec<PathBuf>,
    pub split_ratio: f32,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            panes: Vec::new(),
            active_pane: 0,
            show_hidden: false,
            show_preview: true,
            show_sidebar: true,
            collapsed: Vec::new(),
            sidebar_width: None,
            preview_width: None,
            recent_dirs: Vec::new(),
            split_ratio: 0.5,
        }
    }
}

pub fn file() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(base.join("kestrel/session"))
}

pub fn load() -> Option<Session> {
    Some(parse(&std::fs::read_to_string(file()?).ok()?))
}

/// Writes atomically, so a crash mid-write can't leave a truncated session.
pub fn save(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(tmp, path)
}

fn sort_name(s: SortKey) -> &'static str {
    match s {
        SortKey::Name => "name",
        SortKey::Size => "size",
        SortKey::Modified => "modified",
        SortKey::Kind => "type",
    }
}

fn mode_name(m: ViewMode) -> String {
    match m {
        ViewMode::Details => "details".into(),
        ViewMode::Columns => "columns".into(),
        ViewMode::List => "list".into(),
        ViewMode::Icons(i) => format!("icons{i}"),
    }
}

fn parse_mode(s: &str) -> Option<ViewMode> {
    match s {
        "details" => Some(ViewMode::Details),
        "columns" => Some(ViewMode::Columns),
        "list" => Some(ViewMode::List),
        _ => s
            .strip_prefix("icons")?
            .parse()
            .ok()
            .map(|i: usize| ViewMode::Icons(i.min(crate::tab::ICON_SIZES.len() - 1))),
    }
}

impl Session {
    pub fn to_text(&self) -> String {
        let flag = |b: bool| if b { "1" } else { "0" };
        let mut out = String::new();
        out += &format!(
            "hidden {}\npreview {}\nsidebar {}\n",
            flag(self.show_hidden),
            flag(self.show_preview),
            flag(self.show_sidebar)
        );
        for c in &self.collapsed {
            out += &format!("collapsed {c}\n");
        }
        if let Some(w) = self.sidebar_width {
            out += &format!("sidebar_width {}\n", w.round());
        }
        if let Some(w) = self.preview_width {
            out += &format!("preview_width {}\n", w.round());
        }
        out += &format!("active_pane {}\n", self.active_pane);
        out += &format!("split_ratio {}\n", self.split_ratio);
        for path in self.recent_dirs.iter().take(64) {
            let path = path.to_string_lossy();
            if !path.contains('\n') {
                out += &format!("recent {path}\n");
            }
        }
        for pane in &self.panes {
            out += &format!("pane {}\n", pane.active);
            for t in &pane.tabs {
                // A newline in the path would break the format; such folders just aren't restored.
                let path = t.path.to_string_lossy();
                if !path.contains('\n') {
                    let order = if t.ascending { "asc" } else { "desc" };
                    out += &format!(
                        "tab {} {order} {} {path}\n",
                        sort_name(t.sort),
                        mode_name(t.mode)
                    );
                }
            }
        }
        out
    }
}

/// Unknown or malformed lines are skipped, so old or hand-edited files still load.
pub fn parse(text: &str) -> Session {
    let mut s = Session::default();
    for line in text.lines() {
        let (key, rest) = line.split_once(' ').unwrap_or((line, ""));
        let on = rest == "1";
        match key {
            "hidden" => s.show_hidden = on,
            "preview" => s.show_preview = on,
            "sidebar" => s.show_sidebar = on,
            "collapsed" if !rest.is_empty() => s.collapsed.push(rest.to_string()),
            "sidebar_width" => s.sidebar_width = rest.parse().ok(),
            "preview_width" => s.preview_width = rest.parse().ok(),
            "active_pane" => s.active_pane = rest.parse().unwrap_or(0),
            "split_ratio" => {
                if let Ok(ratio) = rest.parse::<f32>()
                    && ratio.is_finite()
                {
                    s.split_ratio = ratio.clamp(0.15, 0.85);
                }
            }
            "recent" if s.recent_dirs.len() < 64 && Path::new(rest).is_absolute() => {
                let path = PathBuf::from(rest);
                if !s.recent_dirs.contains(&path) {
                    s.recent_dirs.push(path);
                }
            }
            "pane" => s.panes.push(PaneState {
                tabs: Vec::new(),
                active: rest.parse().unwrap_or(0),
            }),
            "tab" => {
                let mut it = rest.splitn(4, ' ');
                let (Some(sort), Some(order), Some(mode), Some(path)) =
                    (it.next(), it.next(), it.next(), it.next())
                else {
                    continue;
                };
                let sort = match sort {
                    "size" => SortKey::Size,
                    "modified" => SortKey::Modified,
                    "type" => SortKey::Kind,
                    _ => SortKey::Name,
                };
                let Some(mode) = parse_mode(mode) else {
                    continue;
                };
                let tab = TabState {
                    path: PathBuf::from(path),
                    sort,
                    ascending: order != "desc",
                    mode,
                };
                if let Some(pane) = s.panes.last_mut() {
                    pane.tabs.push(tab);
                }
            }
            _ => {}
        }
    }
    s.panes.retain(|p| !p.tabs.is_empty());
    s.panes.truncate(2);
    for p in &mut s.panes {
        p.active = p.active.min(p.tabs.len() - 1);
    }
    s.active_pane = s.active_pane.min(s.panes.len().saturating_sub(1));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let s = Session {
            panes: vec![
                PaneState {
                    tabs: vec![
                        TabState {
                            path: "/home/a b".into(),
                            sort: SortKey::Size,
                            ascending: false,
                            mode: ViewMode::Icons(3),
                        },
                        TabState {
                            path: "/tmp".into(),
                            sort: SortKey::Name,
                            ascending: true,
                            mode: ViewMode::Details,
                        },
                    ],
                    active: 1,
                },
                PaneState {
                    tabs: vec![TabState {
                        path: "/".into(),
                        sort: SortKey::Kind,
                        ascending: true,
                        mode: ViewMode::List,
                    }],
                    active: 0,
                },
            ],
            active_pane: 1,
            show_hidden: true,
            show_preview: false,
            show_sidebar: true,
            collapsed: vec!["DEVICES".into()],
            sidebar_width: Some(210.0),
            preview_width: Some(400.0),
            recent_dirs: vec!["/home/a b".into(), "/tmp".into()],
            split_ratio: 0.65,
        };
        assert_eq!(parse(&s.to_text()), s);
    }

    #[test]
    fn tolerates_junk() {
        let s = parse("garbage\ntab size\npane x\ntab name asc nonsense /x\nactive_pane 7\n");
        assert!(s.panes.is_empty());
        assert_eq!(s.active_pane, 0);
        assert_eq!(parse("split_ratio NaN\n").split_ratio, 0.5);
        assert_eq!(parse("split_ratio 50\n").split_ratio, 0.85);
    }
}
