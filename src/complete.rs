//! Folder completion for the editable path bar.

use crate::fsops;
use std::path::{Path, PathBuf};

/// Directories scanned beyond this many entries are cut off to keep typing snappy.
const SCAN_CAP: usize = 20_000;
pub const SHOWN: usize = 12;

#[derive(Default)]
pub struct Completer {
    /// Folder whose subfolders are cached in `names`.
    dir: Option<PathBuf>,
    names: Vec<String>,
}

impl Completer {
    /// Completions for `text`, each a full replacement ending in `/`.
    /// Prefix matches come first, then names that merely contain the typed part.
    pub fn candidates(&mut self, text: &str, show_hidden: bool) -> Vec<String> {
        let Some(slash) = text.rfind('/') else {
            return Vec::new();
        };
        let (head, typed) = text.split_at(slash + 1);
        let dir = expand_tilde(head);
        if self.dir.as_deref() != Some(dir.as_path()) {
            self.names = subfolders(&dir);
            self.dir = Some(dir);
        }
        let needle = typed.to_lowercase();
        let hidden_ok = show_hidden || typed.starts_with('.');
        let mut prefix = Vec::new();
        let mut inner = Vec::new();
        for name in &self.names {
            if name.starts_with('.') && !hidden_ok {
                continue;
            }
            let lower = name.to_lowercase();
            if lower.starts_with(&needle) {
                prefix.push(name);
            } else if !needle.is_empty() && lower.contains(&needle) {
                inner.push(name);
            }
        }
        // Typed name already complete and nothing longer to offer.
        if prefix.len() == 1 && inner.is_empty() && *prefix[0] == typed {
            return Vec::new();
        }
        prefix
            .into_iter()
            .chain(inner)
            .map(|n| format!("{head}{n}/"))
            .collect()
    }
}

/// Longest prefix shared by all candidates (case-insensitive), in the first one's spelling.
pub fn common_prefix(cands: &[String]) -> Option<String> {
    let first = cands.first()?;
    let mut end = first.len();
    for c in &cands[1..] {
        let shared = first
            .char_indices()
            .zip(c.chars())
            .take_while(|((_, a), b)| a.to_lowercase().eq(b.to_lowercase()))
            .last()
            .map(|((i, a), _)| i + a.len_utf8())
            .unwrap_or(0);
        end = end.min(shared);
    }
    Some(first[..end].to_string())
}

fn subfolders(dir: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = rd
        .take(SCAN_CAP)
        .flatten()
        .filter(|e| match e.file_type() {
            Ok(t) if t.is_symlink() => e.path().is_dir(),
            Ok(t) => t.is_dir(),
            Err(_) => false,
        })
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort_by(|a, b| crate::util::natural_cmp(a, b));
    names
}

pub fn expand_tilde(s: &str) -> PathBuf {
    if let Some(rest) = s.strip_prefix('~') {
        return fsops::home_dir().join(rest.trim_start_matches('/'));
    }
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completes_folders() {
        let d = std::env::temp_dir().join(format!("kestrel-complete-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        for n in ["Documents", "Downloads", "music", ".config", "my docs"] {
            std::fs::create_dir_all(d.join(n)).unwrap();
        }
        std::fs::write(d.join("Dfile"), "").unwrap();
        let base = format!("{}/", d.display());
        let mut c = Completer::default();

        let got = c.candidates(&format!("{base}do"), false);
        assert_eq!(
            got,
            [
                format!("{base}Documents/"),
                format!("{base}Downloads/"),
                format!("{base}my docs/")
            ]
        );
        assert_eq!(common_prefix(&got[..2]).unwrap(), format!("{base}Do"));
        assert_eq!(c.candidates(&base, false).len(), 4); // hidden and files left out
        assert_eq!(
            c.candidates(&format!("{base}.c"), false),
            [format!("{base}.config/")]
        );
        assert!(c.candidates(&format!("{base}music"), false).is_empty());
        assert!(c.candidates("no-slash", false).is_empty());
        std::fs::remove_dir_all(d).unwrap();
    }
}
