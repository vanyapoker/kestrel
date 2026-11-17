//! Pure git helpers: repository lookup, status marks, clone URL handling.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Status of one item, in ascending precedence.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Mark {
    Ignored,
    Untracked,
    Added,
    Modified,
    Conflict,
}

impl Mark {
    pub fn letter(self) -> Option<&'static str> {
        match self {
            Mark::Ignored => None,
            Mark::Untracked => Some("U"),
            Mark::Added => Some("A"),
            Mark::Modified => Some("M"),
            Mark::Conflict => Some("!"),
        }
    }
}

/// Key: absolute path of a direct child of the listed folder.
pub type Marks = HashMap<PathBuf, Mark>;

/// First ancestor of `dir` (inclusive) holding a `.git` entry (file or folder).
pub fn repo_root(dir: &Path) -> Option<PathBuf> {
    dir.ancestors()
        .find(|a| a.join(".git").symlink_metadata().is_ok())
        .map(Path::to_path_buf)
}

/// True when an executable called `name` is in `$PATH`.
pub fn on_path(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|d| {
        d.join(name)
            .metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    })
}

/// Git status of the children of `dir`; `None` outside a repository.
pub fn folder_marks(dir: &Path) -> Option<Marks> {
    let run = |args: &[&str]| {
        Command::new("git")
            .arg("--no-optional-locks")
            .args(args)
            .current_dir(dir)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
    };
    let prefix = run(&["rev-parse", "--show-prefix"])?;
    let prefix = prefix.stdout.strip_suffix(b"\n").unwrap_or(&prefix.stdout).to_vec();
    let status = run(&[
        "status",
        "--porcelain=v1",
        "-z",
        "--ignored=matching",
        "--untracked-files=normal",
        "--",
        ".",
    ])?;
    Some(parse_status(dir, &prefix, &status.stdout))
}

pub fn parse_status(dir: &Path, prefix: &[u8], out: &[u8]) -> Marks {
    let mut marks = Marks::new();
    let mut records = out.split(|&b| b == 0);
    while let Some(rec) = records.next() {
        if rec.len() < 4 {
            continue;
        }
        let (xy, path) = (&rec[0..2], &rec[3..]);
        if xy[0] == b'R' || xy[0] == b'C' {
            records.next();
        }
        let mark = match xy {
            b"??" => Mark::Untracked,
            b"!!" => Mark::Ignored,
            b"DD" | b"AU" | b"UD" | b"UA" | b"DU" | b"AA" | b"UU" => Mark::Conflict,
            _ if matches!(xy[0], b'A' | b'R' | b'C') => Mark::Added,
            _ => Mark::Modified,
        };
        let Some(rel) = path.strip_prefix(prefix) else {
            continue;
        };
        let rel = rel.strip_suffix(b"/").unwrap_or(rel);
        let (first, rest) = match rel.iter().position(|&b| b == b'/') {
            Some(i) => (&rel[..i], &rel[i + 1..]),
            None => (rel, &rel[..0]),
        };
        if first.is_empty() || (!rest.is_empty() && mark == Mark::Ignored) {
            continue;
        }
        let child = dir.join(OsStr::from_bytes(first));
        let slot = marks.entry(child).or_insert(mark);
        *slot = (*slot).max(mark);
    }
    marks
}

pub fn looks_like_url(text: &str) -> bool {
    !text.is_empty()
        && !text.chars().any(char::is_whitespace)
        && (["https://", "http://", "git://", "ssh://", "git@", "file://"]
            .iter()
            .any(|p| text.starts_with(p))
            || text.ends_with(".git"))
}

/// Default folder name for a clone URL.
pub fn repo_name(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    let name = url.rsplit(['/', ':']).next().unwrap_or("");
    name.strip_suffix(".git").unwrap_or(name).to_string()
}

pub fn folder_name_ok(name: &str) -> bool {
    let n = name.trim();
    !n.is_empty() && !n.contains('/') && n != "." && n != ".."
}

/// `"Receiving objects:  42% (10/24)"` → `"Receiving objects 42%"`.
pub fn progress_step(line: &str) -> Option<String> {
    let line = line.strip_prefix("remote: ").unwrap_or(line);
    let (phase, rest) = line.split_once(": ")?;
    let (num, _) = rest.trim_start().split_once('%')?;
    let pct: u8 = num.parse().ok()?;
    Some(format!("{phase} {pct}%"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_marks() {
        let recs: &[&[u8]] = &[
            b" M src/a.rs",
            b"?? src/new/",
            b"!! src/target/",
            b"R  src/b.rs",
            b"src/old.rs",
            b" M other/x",
            b"UU src/deep/c.rs",
            b" D src/deep2/gone.rs",
            b"!! src/only/x.log",
            b"!! src/lib/cache/",
            b" M src/lib/mod.rs",
        ];
        let out = recs.join(&0u8);
        let m = parse_status(Path::new("/r/src"), b"src/", &out);
        let at = |n: &str| m.get(&Path::new("/r/src").join(n)).copied();
        assert_eq!(at("a.rs"), Some(Mark::Modified));
        assert_eq!(at("new"), Some(Mark::Untracked));
        assert_eq!(at("target"), Some(Mark::Ignored));
        assert_eq!(at("b.rs"), Some(Mark::Added));
        assert_eq!(at("deep"), Some(Mark::Conflict));
        assert_eq!(at("deep2"), Some(Mark::Modified));
        assert_eq!(at("lib"), Some(Mark::Modified));
        assert_eq!(at("old.rs"), None);
        assert_eq!(at("only"), None);
        assert_eq!(m.len(), 7);
    }

    #[test]
    fn names_and_urls() {
        assert_eq!(repo_name("https://github.com/a/kestrel.git"), "kestrel");
        assert_eq!(repo_name("git@github.com:a/kestrel.git"), "kestrel");
        assert_eq!(repo_name("https://github.com/a/kestrel/"), "kestrel");
        assert_eq!(repo_name("/tmp/x/origin"), "origin");
        assert_eq!(repo_name(""), "");
        assert!(looks_like_url("https://github.com/a/b"));
        assert!(looks_like_url("git@github.com:a/b.git"));
        assert!(!looks_like_url("hello world"));
        assert!(!looks_like_url("notes.txt"));
    }

    #[test]
    fn progress() {
        assert_eq!(
            progress_step("Receiving objects:  42% (10/24), 1.2 MiB").as_deref(),
            Some("Receiving objects 42%")
        );
        assert_eq!(
            progress_step("remote: Counting objects: 100% (5/5), done.").as_deref(),
            Some("Counting objects 100%")
        );
        assert_eq!(progress_step("Cloning into 'x'..."), None);
    }
}
