//! Recursive filename search, parallel over the directory tree. Results stream
//! back in batches so the first matches show up immediately.

use crate::fsops::{Entry, make_entry};
use eframe::egui;
use ignore::{DirEntry, ParallelVisitor, ParallelVisitorBuilder, WalkBuilder, WalkState};
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;

pub const MAX_RESULTS: usize = 20_000;
const BATCH: usize = 256;

/// A case-insensitive filename query. Space-separated terms must all match;
/// letters within each term may have gaps, as in "nd" matching "NaDya".
pub struct Query {
    literal: String,
    terms: Vec<Vec<char>>,
    literals: Vec<String>,
    exact: Option<String>,
    extensions: Vec<String>,
    directory: Option<bool>,
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
        let mut q = Self { literal: String::new(), terms: Vec::new(), literals: Vec::new(), exact: None, extensions: Vec::new(), directory: None };
        let mut token = String::new();
        let mut quoted = false;
        let mut literal = false;
        let mut tokens = Vec::new();
        for c in query.chars().chain(std::iter::once(' ')) {
            if c == '"' {
                quoted = !quoted;
                literal = true;
            } else if c.is_whitespace() && !quoted {
                if !token.is_empty() {
                    tokens.push((std::mem::take(&mut token), literal));
                    literal = false;
                }
            } else {
                token.extend(c.to_lowercase());
            }
        }
        if !token.is_empty() { tokens.push((token.trim_end().to_string(), literal)); }
        let mut words = Vec::new();
        for (token, literal) in tokens {
            if let Some(ext) = token.strip_prefix("ext:").filter(|_| !literal) {
                q.extensions.extend(ext.split(',').map(|e| e.trim_start_matches('.').to_string()).filter(|e| !e.is_empty()));
            } else if !literal && (token == "type:folder" || token == "type:dir") {
                q.directory = Some(true);
            } else if !literal && token == "type:file" {
                q.directory = Some(false);
            } else if let Some(exact) = token.strip_prefix('=') {
                q.exact = Some(exact.to_string());
                words.push(exact.to_string());
            } else {
                if literal { q.literals.push(token.clone()); }
                q.terms.push(token.chars().collect());
                words.push(token);
            }
        }
        q.literal = words.join(" ");
        q
    }

    pub fn score_name(&self, name: &str, is_dir: bool) -> Option<MatchScore> {
        if self.directory.is_some_and(|d| d != is_dir) { return None; }
        if !self.extensions.is_empty() {
            let ext = Path::new(name).extension().unwrap_or_default().to_string_lossy().to_lowercase();
            if is_dir || !self.extensions.contains(&ext) { return None; }
        }
        self.score(name)
    }

    /// Cheap eligibility check for the directory walker; scoring happens once
    /// per result when the tab builds its view, never inside the sort comparator.
    pub fn matches(&self, name: &str) -> bool {
        let lower = name.to_lowercase();
        !self.exact.as_ref().is_some_and(|e| e != &lower)
            && self.literals.iter().all(|s| lower.contains(s))
            && self.terms.iter().all(|term| {
            let mut chars = lower.chars();
            term.iter().all(|&q| chars.any(|c| c == q))
        })
    }

    pub fn score(&self, name: &str) -> Option<MatchScore> {
        if !self.matches(name) { return None; }
        if self.exact.is_some() {
            return Some(MatchScore { kind: 5, ..Default::default() });
        }
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

    /// Character offsets, so highlights remain correct for UTF-8 and lowercase
    /// expansions (for example capital dotted İ).
    pub fn highlights(&self, name: &str) -> Vec<usize> {
        if !self.matches(name) { return Vec::new(); }
        if self.exact.is_some() { return (0..name.chars().count()).collect(); }
        let mut text = Vec::new();
        let mut original = Vec::new();
        for (i, c) in name.chars().enumerate() {
            for lower in c.to_lowercase() { text.push(lower); original.push(i); }
        }
        let mut matched = Vec::new();
        for term in &self.terms {
            if let Some(start) = text.windows(term.len()).position(|w| w == term.as_slice()) {
                matched.extend(original[start..start + term.len()].iter().copied());
            } else {
                let mut start = 0;
                for &q in term {
                    if let Some(offset) = text[start..].iter().position(|&c| c == q) {
                        start += offset;
                        matched.push(original[start]);
                        start += 1;
                    }
                }
            }
        }
        matched.sort_unstable();
        matched.dedup();
        matched
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

/// A lightweight, reusable directory scan: metadata is loaded only for results.
pub struct Index {
    pub candidates: Vec<Candidate>,
    pub show_hidden: bool,
}

pub struct Candidate {
    pub path: PathBuf,
    pub is_dir: bool,
}

pub struct SearchMsg {
    pub tab_id: u64,
    pub generation: u64,
    pub batch: Vec<Entry>,
    pub done: bool,
    pub truncated: bool,
    pub replace: bool,
    pub index: Option<Arc<Index>>,
}

pub struct Request {
    pub tab_id: u64,
    pub generation: u64,
    pub root: PathBuf,
    pub query: String,
    pub show_hidden: bool,
    pub cancel: Arc<AtomicBool>,
    pub index: Option<Arc<Index>>,
}

/// Keep the best matches across the entire scan. The limit bounds displayed
/// results, never the traversal; a late exact match can replace an early fuzzy one.
fn best_matches(index: &Index, query: &Query, limit: usize, cancel: &AtomicBool) -> (Vec<usize>, bool) {
    let mut best = BinaryHeap::new();
    let mut total = 0;
    for (i, c) in index.candidates.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) { break; }
        let name = c.path.file_name().unwrap_or_default().to_string_lossy();
        if let Some(score) = query.score_name(&name, c.is_dir) {
            total += 1;
            let rank = (score, Reverse(&c.path), i);
            if best.len() < limit { best.push(Reverse(rank)); }
            else if best.peek().is_some_and(|worst| rank > worst.0) {
                best.pop();
                best.push(Reverse(rank));
            }
        }
    }
    (best.into_sorted_vec().into_iter().map(|Reverse((_, _, i))| i).collect(), total > limit)
}

fn entry(root: &Path, path: &Path) -> Option<Entry> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    let name = path.file_name()?.to_string_lossy().into_owned();
    let mut e = make_entry(path.to_path_buf(), name, meta);
    e.rel = Some(relative_parent(root, path));
    Some(e)
}

pub fn spawn(req: Request, tx: Sender<SearchMsg>, ctx: egui::Context) {
    std::thread::spawn(move || {
        let query = Query::new(&req.query);
        let index = match req.index.clone() {
            Some(index) if index.show_hidden == req.show_hidden => index,
            _ => {
                let shared = Shared {
                    req: &req, query: &query, count: AtomicUsize::new(0),
                    candidates: Mutex::new(Vec::new()), tx: &tx, ctx: &ctx,
                };
                WalkBuilder::new(&req.root)
                    .hidden(!req.show_hidden).ignore(false).git_ignore(false)
                    .git_global(false).git_exclude(false).parents(false)
                    .follow_links(false).same_file_system(true)
                    .threads(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4))
                    .build_parallel().visit(&mut VisitorBuilder(&shared));
                Arc::new(Index { candidates: shared.candidates.into_inner().unwrap(), show_hidden: req.show_hidden })
            }
        };
        if req.cancel.load(Ordering::Relaxed) { return; }
        let (matches, truncated) = best_matches(&index, &query, MAX_RESULTS, &req.cancel);
        let mut batch = Vec::new();
        for i in matches {
            if req.cancel.load(Ordering::Relaxed) { return; }
            if let Some(e) = entry(&req.root, &index.candidates[i].path) { batch.push(e); }
        }
        let _ = tx.send(SearchMsg {
            tab_id: req.tab_id, generation: req.generation, batch,
            done: true, truncated, replace: true, index: Some(index),
        });
        ctx.request_repaint();
    });
}

struct Shared<'a> {
    req: &'a Request,
    query: &'a Query,
    count: AtomicUsize,
    candidates: Mutex<Vec<Candidate>>,
    tx: &'a Sender<SearchMsg>,
    ctx: &'a egui::Context,
}
struct VisitorBuilder<'a>(&'a Shared<'a>);
impl<'s> ParallelVisitorBuilder<'s> for VisitorBuilder<'s> {
    fn build(&mut self) -> Box<dyn ParallelVisitor + 's> {
        Box::new(Visitor { shared: self.0, buf: Vec::new(), candidates: Vec::new() })
    }
}
struct Visitor<'a> {
    shared: &'a Shared<'a>,
    buf: Vec<Entry>,
    candidates: Vec<Candidate>,
}
impl Visitor<'_> {
    fn flush(&mut self) {
        let s = self.shared;
        if !self.buf.is_empty() && !s.req.cancel.load(Ordering::Relaxed) {
            let _ = s.tx.send(SearchMsg {
                tab_id: s.req.tab_id, generation: s.req.generation,
                batch: std::mem::take(&mut self.buf), done: false, truncated: false,
                replace: false, index: None,
            });
            s.ctx.request_repaint();
        }
    }
}
impl ParallelVisitor for Visitor<'_> {
    fn visit(&mut self, de: Result<DirEntry, ignore::Error>) -> WalkState {
        let s = self.shared;
        if s.req.cancel.load(Ordering::Relaxed) { return WalkState::Quit; }
        let Ok(de) = de else { return WalkState::Continue };
        if de.depth() == 0 { return WalkState::Continue; }
        let is_dir = de.file_type().is_some_and(|t| t.is_dir() || (t.is_symlink() && de.path().is_dir()));
        self.candidates.push(Candidate { path: de.path().to_path_buf(), is_dir });
        let name = de.file_name().to_string_lossy();
        if s.query.score_name(&name, is_dir).is_some()
            && s.count.fetch_add(1, Ordering::Relaxed) < MAX_RESULTS
        {
            if let Some(e) = entry(&s.req.root, de.path()) { self.buf.push(e); }
            if self.buf.len() >= BATCH { self.flush(); }
        }
        WalkState::Continue
    }
}
impl Drop for Visitor<'_> {
    fn drop(&mut self) {
        self.flush();
        self.shared.candidates.lock().unwrap().append(&mut self.candidates);
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
    fn query_filters_literals_and_exact_names() {
        for (text, name, is_dir, expected) in [
            ("nd ext:pdf", "NaDya.PDF", false, true),
            ("nd ext:pdf", "NaDya.jpg", false, false),
            ("ext:pdf,jpg type:file", "photo.JPG", false, true),
            ("ext:pdf type:folder", "folder.pdf", true, false),
            ("nd type:folder", "NaDya", true, true),
            ("nd type:folder", "NaDya", false, false),
            ("\"nd\"", "NaDya", false, false),
            ("\"nd\"", "a-ND.txt", false, true),
            ("=report.pdf", "Report.PDF", false, true),
            ("=report.pdf", "old-report.pdf", false, false),
            ("=\"annual report.pdf\"", "Annual Report.pdf", false, true),
            ("\"ext:pdf\"", "ext:pdf", false, true),
        ] {
            assert_eq!(Query::new(text).score_name(name, is_dir).is_some(), expected, "{text:?} / {name}");
        }
        assert_eq!(Query::new("nd ext:txt").highlights("NaDya.txt"), [0, 2]);
        assert_eq!(Query::new("дн").highlights("ДоНя.txt"), [0, 2]);
        assert_eq!(Query::new("i").highlights("İstanbul"), [0]);
        assert_eq!(Query::new("=ND").highlights("ND"), [0, 1]);
    }

    #[test]
    fn late_best_matches_survive_the_result_limit() {
        let mut candidates: Vec<_> = (0..MAX_RESULTS + 5).map(|i| Candidate {
            path: PathBuf::from(format!("/root/n---d-{i:06}")), is_dir: false,
        }).collect();
        let exact = candidates.len();
        candidates.push(Candidate { path: "/root/ND".into(), is_dir: false });
        let index = Index { candidates, show_hidden: false };
        let (best, truncated) = best_matches(&index, &Query::new("nd"), MAX_RESULTS, &AtomicBool::new(false));
        assert!(truncated);
        assert_eq!(best.len(), MAX_RESULTS);
        assert_eq!(best[0], exact);
        assert!(best.iter().all(|&i| i < index.candidates.len()));
    }

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
