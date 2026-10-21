//! Recursive filename search, parallel over the directory tree. Results stream
//! back in batches so the first matches show up immediately.

use crate::fsops::{Entry, make_entry};
use eframe::egui;
use ignore::{DirEntry, ParallelVisitor, ParallelVisitorBuilder, WalkBuilder, WalkState};
use std::cmp::Reverse;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;

pub const MAX_RESULTS: usize = 20_000;
const BATCH: usize = 256;

/// A case-insensitive filename query. Space-separated terms must all match;
/// letters within each term may have gaps, as in "nd" matching "NaDya".
pub struct Query {
    literal: String,
    terms: Vec<Vec<char>>,
}

/// Higher scores rank first. Match quality takes precedence over filename length.
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct MatchScore {
    /// Fuzzy < separate terms < substring < prefix < exact name.
    kind: u8,
    quality: i64,
    length: Reverse<usize>,
}

impl Query {
    pub fn new(query: &str) -> Self {
        let literal = query.trim().to_lowercase();
        let terms = literal.split_whitespace().map(|t| t.chars().collect()).collect();
        Self { literal, terms }
    }

    /// Cheap eligibility check for the directory walker; scoring happens once
    /// per result when the tab builds its view, never inside the sort comparator.
    pub fn matches(&self, name: &str) -> bool {
        let lower = name.to_lowercase();
        self.terms.iter().all(|term| {
            let mut chars = lower.chars();
            term.iter().all(|&q| chars.any(|c| c == q))
        })
    }

    pub fn score(&self, name: &str) -> Option<MatchScore> {
        if self.terms.is_empty() {
            return Some(MatchScore::default());
        }
        let original: Vec<char> = name.chars().collect();
        let mut text = Vec::new();
        let mut boundaries = Vec::new();
        for (i, &c) in original.iter().enumerate() {
            let boundary = i == 0 || !original[i - 1].is_alphanumeric()
                || (original[i - 1].is_lowercase() && c.is_uppercase())
                || (c.is_uppercase() && original.get(i + 1).is_some_and(|c| c.is_lowercase()));
            for (j, lower) in c.to_lowercase().enumerate() {
                text.push(lower);
                boundaries.push(boundary && j == 0);
            }
        }
        let mut kind = 2;
        let mut quality = 0;
        for term in &self.terms {
            if term.len() > text.len() {
                return None;
            }
            if let Some(contiguous) = text.windows(term.len()).enumerate()
                .filter(|(_, window)| *window == term.as_slice())
                .map(|(start, _)| (if boundaries[start] { 24 } else { 0 }) - start as i64)
                .max()
            {
                quality += contiguous;
            } else {
                kind = 1;
                quality += subsequence_score(term, &text, &boundaries)?;
            }
        }
        // A complete phrase beats the same words scattered through a name.
        let lower = name.to_lowercase();
        kind = if lower == self.literal { 5 }
            else if lower.starts_with(&self.literal) { 4 }
            else if lower.contains(&self.literal) { 3 }
            else { kind };
        Some(MatchScore { kind, quality, length: Reverse(text.len()) })
    }
}

/// Best subsequence alignment, rather than greedily taking the first occurrence
/// of each letter. Rewards word/camel-case starts, consecutive letters and small gaps.
fn subsequence_score(term: &[char], text: &[char], boundaries: &[bool]) -> Option<i64> {
    const MISS: i64 = i64::MIN / 4;
    let mut previous = vec![MISS; text.len()];
    let mut current = vec![MISS; text.len()];
    for (qi, &q) in term.iter().enumerate() {
        let mut best_gap = MISS;
        for (i, &c) in text.iter().enumerate() {
            if i > 0 {
                best_gap = best_gap.max(previous[i - 1] + 2 * (i - 1) as i64);
            }
            current[i] = MISS;
            if c != q {
                continue;
            }
            let base = if qi == 0 { -(i as i64) } else {
                let consecutive = if i > 0 { previous[i - 1] + 24 } else { MISS };
                consecutive.max(best_gap - 2 * i.saturating_sub(1) as i64)
            };
            if base > MISS / 2 {
                current[i] = base + 16 + if boundaries[i] { 24 } else { 0 };
            }
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous.into_iter().max().filter(|&s| s > MISS / 2)
}

pub struct SearchMsg {
    pub tab_id: u64,
    pub generation: u64,
    pub batch: Vec<Entry>,
    pub done: bool,
    pub truncated: bool,
}

pub struct Request {
    pub tab_id: u64,
    pub generation: u64,
    pub root: PathBuf,
    pub query: String,
    pub show_hidden: bool,
    pub cancel: Arc<AtomicBool>,
}

pub fn spawn(req: Request, tx: Sender<SearchMsg>, ctx: egui::Context) {
    std::thread::spawn(move || {
        let query = Query::new(&req.query);
        let count = Arc::new(AtomicUsize::new(0));
        let shared = Shared {
            tab_id: req.tab_id,
            generation: req.generation,
            root: req.root.clone(),
            query,
            cancel: req.cancel.clone(),
            count: count.clone(),
            tx: tx.clone(),
            ctx: ctx.clone(),
        };
        WalkBuilder::new(&req.root)
            .hidden(!req.show_hidden)
            .ignore(false)
            .git_ignore(false)
            .git_global(false)
            .git_exclude(false)
            .parents(false)
            .follow_links(false)
            .same_file_system(true)
            .threads(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4))
            .build_parallel()
            .visit(&mut VisitorBuilder(&shared));
        if !req.cancel.load(Ordering::Relaxed) {
            let _ = tx.send(SearchMsg {
                tab_id: req.tab_id,
                generation: req.generation,
                batch: Vec::new(),
                done: true,
                truncated: count.load(Ordering::Relaxed) >= MAX_RESULTS,
            });
            ctx.request_repaint();
        }
    });
}

struct Shared {
    tab_id: u64,
    generation: u64,
    root: PathBuf,
    query: Query,
    cancel: Arc<AtomicBool>,
    count: Arc<AtomicUsize>,
    tx: Sender<SearchMsg>,
    ctx: egui::Context,
}

struct VisitorBuilder<'a>(&'a Shared);

impl<'s> ParallelVisitorBuilder<'s> for VisitorBuilder<'s> {
    fn build(&mut self) -> Box<dyn ParallelVisitor + 's> {
        Box::new(Visitor { shared: self.0, buf: Vec::new() })
    }
}

struct Visitor<'a> {
    shared: &'a Shared,
    buf: Vec<Entry>,
}

impl Visitor<'_> {
    fn flush(&mut self) {
        if self.buf.is_empty() || self.shared.cancel.load(Ordering::Relaxed) {
            return;
        }
        let _ = self.shared.tx.send(SearchMsg {
            tab_id: self.shared.tab_id,
            generation: self.shared.generation,
            batch: std::mem::take(&mut self.buf),
            done: false,
            truncated: false,
        });
        self.shared.ctx.request_repaint();
    }
}

impl ParallelVisitor for Visitor<'_> {
    fn visit(&mut self, entry: Result<DirEntry, ignore::Error>) -> WalkState {
        let s = self.shared;
        if s.cancel.load(Ordering::Relaxed) || s.count.load(Ordering::Relaxed) >= MAX_RESULTS {
            return WalkState::Quit;
        }
        let Ok(de) = entry else { return WalkState::Continue };
        if de.depth() == 0 {
            return WalkState::Continue;
        }
        let name = de.file_name().to_string_lossy();
        if !s.query.matches(&name) {
            return WalkState::Continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(de.path()) else { return WalkState::Continue };
        s.count.fetch_add(1, Ordering::Relaxed);
        let mut e = make_entry(de.path().to_path_buf(), name.into_owned(), meta);
        e.rel = Some(relative_parent(&s.root, de.path()));
        self.buf.push(e);
        if self.buf.len() >= BATCH {
            self.flush();
        }
        WalkState::Continue
    }
}

impl Drop for Visitor<'_> {
    fn drop(&mut self) {
        self.flush();
    }
}

fn relative_parent(root: &Path, path: &Path) -> String {
    path.parent()
        .and_then(|p| p.strip_prefix(root).ok())
        .map(|p| p.display().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_queries_are_case_insensitive_subsequences() {
        for (query, name, matches) in [
            ("nd", "NaDya", true),
            ("ND", "nadya", true),
            ("дн", "ДоНя.txt", true),
            ("cf", "Café", true),
            ("i", "İstanbul", true),
            ("dn", "NaDya", false),
            ("nn", "NaDya", false),
            ("xyz", "NaDya", false),
            ("", "anything", true),
            ("  \t ", "anything", true),
        ] {
            let q = Query::new(query);
            assert_eq!(q.matches(name), matches, "{query:?} in {name:?}");
            assert_eq!(q.score(name).is_some(), matches, "scoring {query:?} in {name:?}");
        }
    }

    #[test]
    fn exact_prefix_and_substring_matches_beat_fuzzy_matches() {
        let q = Query::new("nd");
        let ranked = ["ND", "nd-notes.txt", "a-nd.txt", "NaDya", "Named"];
        for pair in ranked.windows(2) {
            assert!(q.score(pair[0]) > q.score(pair[1]), "{} should beat {}", pair[0], pair[1]);
        }
        let q = Query::new("nf");
        assert!(q.score("New Folder") > q.score("unified"));
        // The best alignment uses the later 'n', rather than the first one.
        let q = Query::new("nd");
        assert!(q.score("n----------------n-aD") > q.score("n-----------D"));
    }

    #[test]
    fn every_query_term_must_match_in_any_order() {
        let q = Query::new("  fn   rp  ");
        assert!(q.matches("Report-final.txt"));
        assert!(q.score("Report-final.txt").is_some());
        assert!(!q.matches("final.txt"));
        assert!(q.score("final.txt").is_none());
        let q = Query::new("final report");
        assert!(q.score("Final Report.txt") > q.score("Report final.txt"));
    }
}
