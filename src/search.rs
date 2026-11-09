//! Recursive filename search, parallel over the directory tree. Walker threads
//! only collect names; a streaming loop scores them against whatever the query
//! currently is, so the first matches show up immediately and editing the query
//! mid-scan re-checks everything found so far.

use crate::fsops::{Entry, make_entry};
use crate::util::{self, Category};
use eframe::egui;
use ignore::{DirEntry, ParallelVisitor, ParallelVisitorBuilder, WalkBuilder, WalkState};
use rayon::prelude::*;
use std::borrow::Cow;
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::ffi::OsStr;
use std::ops::Range;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const MAX_RESULTS: usize = 20_000;
/// Items a walker thread gathers before adding them to the shared index.
const CHUNK: usize = 2048;
/// How often new matches stream in while the walk runs.
const STREAM_EVERY: Duration = Duration::from_millis(40);

/// A case-insensitive filename query. Space-separated terms must all match;
/// letters within each term may have gaps, as in "nd" matching "NaDya".
pub struct Query {
    literal: String,
    terms: Vec<Vec<char>>,
    literals: Vec<String>,
    exact: Option<String>,
    extensions: Vec<String>,
    directory: Option<bool>,
    category: Option<Category>,
    /// Tokens shaped like a filter (`size:>1M`) that aren't one; they are
    /// still matched against names, but the UI warns about them.
    pub unknown: Vec<String>,
}

/// Higher scores rank first. Match quality takes precedence over filename length.
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct MatchScore {
    /// Fuzzy < separate terms < substring < prefix < exact name.
    kind: u8,
    quality: i64,
    length: Reverse<usize>,
}

/// File categories `type:` accepts, besides folder and file.
const TYPES: &[(&str, Category)] = &[
    ("image", Category::Image),
    ("video", Category::Video),
    ("audio", Category::Audio),
    ("music", Category::Audio),
    ("archive", Category::Archive),
    ("document", Category::Document),
    ("doc", Category::Document),
    ("code", Category::Code),
    ("text", Category::Text),
];

/// Filters for the search box hint and the unknown-filter warning.
pub const FILTER_HELP: &str =
    "ext:pdf,jpg · type:folder, file, image, video, audio, document, code";

impl Query {
    pub fn new(query: &str) -> Self {
        let mut q = Self {
            literal: String::new(),
            terms: Vec::new(),
            literals: Vec::new(),
            exact: None,
            extensions: Vec::new(),
            directory: None,
            category: None,
            unknown: Vec::new(),
        };
        let mut token = String::new();
        // The token as typed, for warnings.
        let mut typed = String::new();
        let mut quoted = false;
        let mut literal = false;
        let mut tokens = Vec::new();
        for c in query.chars().chain(std::iter::once(' ')) {
            if c == '"' {
                quoted = !quoted;
                literal = true;
            } else if c.is_whitespace() && !quoted {
                if !token.is_empty() {
                    tokens.push((
                        std::mem::take(&mut token),
                        std::mem::take(&mut typed),
                        literal,
                    ));
                    literal = false;
                }
            } else {
                token.extend(c.to_lowercase());
                typed.push(c);
            }
        }
        if !token.is_empty() {
            tokens.push((token.trim_end().to_string(), typed, literal));
        }
        let mut words = Vec::new();
        for (token, typed, literal) in tokens {
            let filter = (!literal).then(|| token.split_once(':')).flatten();
            match filter {
                Some(("ext", ext)) if !ext.is_empty() => q.extensions.extend(
                    ext.split(',')
                        .map(|e| e.trim_start_matches('.').to_string())
                        .filter(|e| !e.is_empty()),
                ),
                Some(("type", "folder" | "dir")) => q.directory = Some(true),
                Some(("type", "file")) => q.directory = Some(false),
                Some(("type", kind)) if TYPES.iter().any(|(name, _)| *name == kind) => {
                    q.directory = Some(false);
                    q.category = TYPES.iter().find(|(name, _)| *name == kind).map(|t| t.1);
                }
                _ => {
                    if let Some(exact) = token.strip_prefix('=') {
                        q.exact = Some(exact.to_string());
                        words.push(exact.to_string());
                        continue;
                    }
                    // "word:" looks like a filter; a drive letter or time ("c:", "10:30") doesn't.
                    if filter.is_some_and(|(key, _)| {
                        key.len() > 1 && key.chars().all(|c| c.is_ascii_alphabetic())
                    }) {
                        q.unknown.push(typed);
                    }
                    if literal {
                        q.literals.push(token.clone());
                    }
                    q.terms.push(token.chars().collect());
                    words.push(token);
                }
            }
        }
        q.literal = words.join(" ");
        q
    }

    pub fn score_name(&self, name: &str, is_dir: bool) -> Option<MatchScore> {
        if self.directory.is_some_and(|d| d != is_dir) {
            return None;
        }
        if !self.extensions.is_empty() || self.category.is_some() {
            let ext = Path::new(name)
                .extension()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase();
            if is_dir
                || (!self.extensions.is_empty() && !self.extensions.contains(&ext))
                || self
                    .category
                    .is_some_and(|c| util::category(&ext, false) != c)
            {
                return None;
            }
        }
        self.score(name)
    }

    /// Cheap eligibility check for the directory walker; scoring happens once
    /// per result when the tab builds its view, never inside the sort comparator.
    pub fn matches(&self, name: &str) -> bool {
        if name.is_ascii() {
            // Most names are ASCII: compare bytes without lowercasing a copy.
            // A non-ASCII query letter can never match an ASCII name.
            let b = name.as_bytes();
            let contains = |s: &str| {
                s.is_empty()
                    || b.windows(s.len())
                        .any(|w| w.eq_ignore_ascii_case(s.as_bytes()))
            };
            return !self
                .exact
                .as_ref()
                .is_some_and(|e| !b.eq_ignore_ascii_case(e.as_bytes()))
                && self.literals.iter().all(|s| contains(s))
                && self.terms.iter().all(|term| {
                    let mut bytes = b.iter();
                    term.iter()
                        .all(|&q| q.is_ascii() && bytes.any(|c| c.to_ascii_lowercase() == q as u8))
                });
        }
        let lower = name.to_lowercase();
        !self.exact.as_ref().is_some_and(|e| e != &lower)
            && self.literals.iter().all(|s| lower.contains(s))
            && self.terms.iter().all(|term| {
                let mut chars = lower.chars();
                term.iter().all(|&q| chars.any(|c| c == q))
            })
    }

    pub fn score(&self, name: &str) -> Option<MatchScore> {
        if !self.matches(name) {
            return None;
        }
        if self.exact.is_some() {
            return Some(MatchScore {
                kind: 5,
                ..Default::default()
            });
        }
        if self.terms.is_empty() {
            return Some(MatchScore::default());
        }
        thread_local! {
            // Ranking scores millions of names; reuse the buffers.
            static FOLDED: std::cell::RefCell<Folded> = std::cell::RefCell::new(Folded::default());
        }
        FOLDED.with_borrow_mut(|folded| {
            folded.fill(name);
            let mut kind = 2;
            let mut quality = 0;
            for term in &self.terms {
                if let Some((contiguous, _)) = folded.contiguous(term) {
                    quality += contiguous;
                } else {
                    kind = 1;
                    quality += folded.subsequence(term, false)?.0;
                }
            }
            // A complete phrase beats the same words scattered through a name.
            let lower = name.to_lowercase();
            kind = if lower == self.literal {
                5
            } else if lower.starts_with(&self.literal) {
                4
            } else if lower.contains(&self.literal) {
                3
            } else {
                kind
            };
            Some(MatchScore {
                kind,
                quality,
                length: Reverse(folded.text.len()),
            })
        })
    }

    /// Character offsets of the letters `score` matched, so highlights show the
    /// alignment that ranked the name. Offsets stay correct for UTF-8 and
    /// lowercase expansions (for example capital dotted İ).
    pub fn highlights(&self, name: &str) -> Vec<usize> {
        if !self.matches(name) {
            return Vec::new();
        }
        if self.exact.is_some() {
            return (0..name.chars().count()).collect();
        }
        let folded = Folded::new(name);
        let mut matched = Vec::new();
        for term in &self.terms {
            let positions: Vec<usize> = match folded.contiguous(term) {
                Some((_, start)) => (start..start + term.len()).collect(),
                None => folded
                    .subsequence(term, true)
                    .map(|m| m.1)
                    .unwrap_or_default(),
            };
            matched.extend(positions.into_iter().map(|p| folded.original[p]));
        }
        matched.sort_unstable();
        matched.dedup();
        matched
    }
}

/// A name lowercased for matching, with word starts and the original character
/// index of each lowercase character.
#[derive(Default)]
struct Folded {
    text: Vec<char>,
    boundaries: Vec<bool>,
    original: Vec<usize>,
}

impl Folded {
    fn new(name: &str) -> Self {
        let mut f = Folded::default();
        f.fill(name);
        f
    }

    fn fill(&mut self, name: &str) {
        self.text.clear();
        self.boundaries.clear();
        self.original.clear();
        let mut chars = name.chars().enumerate().peekable();
        let mut prev: Option<char> = None;
        while let Some((i, c)) = chars.next() {
            let boundary = prev.is_none_or(|p| {
                !p.is_alphanumeric()
                    || (p.is_lowercase() && c.is_uppercase())
                    || (c.is_uppercase() && chars.peek().is_some_and(|(_, n)| n.is_lowercase()))
            });
            if c.is_ascii() {
                self.text.push(c.to_ascii_lowercase());
                self.boundaries.push(boundary);
                self.original.push(i);
            } else {
                for (j, lower) in c.to_lowercase().enumerate() {
                    self.text.push(lower);
                    self.boundaries.push(boundary && j == 0);
                    self.original.push(i);
                }
            }
            prev = Some(c);
        }
    }

    /// Best place the term appears unbroken, preferring word starts, then early.
    fn contiguous(&self, term: &[char]) -> Option<(i64, usize)> {
        if term.len() > self.text.len() {
            return None;
        }
        self.text
            .windows(term.len())
            .enumerate()
            .filter(|(_, window)| *window == term)
            .map(|(start, _)| {
                let bonus = if self.boundaries[start] { 24 } else { 0 };
                (bonus - start as i64, start)
            })
            .max_by_key(|&(quality, start)| (quality, Reverse(start)))
    }

    /// Best subsequence alignment, rather than greedily taking the first occurrence
    /// of each letter. Rewards word/camel-case starts, consecutive letters and small
    /// gaps. With `trace`, also returns the matched positions.
    fn subsequence(&self, term: &[char], trace: bool) -> Option<(i64, Vec<usize>)> {
        const MISS: i64 = i64::MIN / 4;
        let (text, n) = (&self.text, self.text.len());
        if term.is_empty() || term.len() > n {
            return None;
        }
        let mut previous = vec![MISS; n];
        let mut current = vec![MISS; n];
        // from[qi * n + i]: where the previous letter matched, for the best score
        // that matches letter qi at position i.
        let mut from = if trace {
            vec![0; term.len() * n]
        } else {
            Vec::new()
        };
        for (qi, &q) in term.iter().enumerate() {
            let (mut best_gap, mut gap_at) = (MISS, 0);
            for (i, &c) in text.iter().enumerate() {
                if i > 0 && previous[i - 1] + 2 * (i - 1) as i64 >= best_gap {
                    best_gap = previous[i - 1] + 2 * (i - 1) as i64;
                    gap_at = i - 1;
                }
                current[i] = MISS;
                if c != q {
                    continue;
                }
                let (base, prev) = if qi == 0 {
                    (-(i as i64), 0)
                } else {
                    let consecutive = if i > 0 { previous[i - 1] + 24 } else { MISS };
                    let gap = best_gap - 2 * i.saturating_sub(1) as i64;
                    if consecutive >= gap {
                        (consecutive, i.saturating_sub(1))
                    } else {
                        (gap, gap_at)
                    }
                };
                if base > MISS / 2 {
                    current[i] = base + 16 + if self.boundaries[i] { 24 } else { 0 };
                    if trace {
                        from[qi * n + i] = prev;
                    }
                }
            }
            std::mem::swap(&mut previous, &mut current);
        }
        let (end, score) = previous
            .iter()
            .copied()
            .enumerate()
            .max_by_key(|&(i, s)| (s, Reverse(i)))
            .filter(|&(_, s)| s > MISS / 2)?;
        let mut positions = Vec::new();
        if trace {
            let mut i = end;
            for qi in (0..term.len()).rev() {
                positions.push(i);
                i = from[qi * n + i];
            }
            positions.reverse();
        }
        Some((score, positions))
    }
}

/// Every name under the search root, reusable across queries. Names are packed
/// into one buffer and folders are shared by their items, which takes about a
/// quarter of the memory of a full path per item.
pub struct Index {
    pub show_hidden: bool,
    dirs: Vec<PathBuf>,
    names: Vec<u8>,
    items: Vec<Item>,
}

#[derive(Clone, Copy)]
struct Item {
    name: u32,
    len: u16,
    dir: u32,
    is_dir: bool,
}

impl Index {
    fn new(show_hidden: bool) -> Self {
        Self {
            show_hidden,
            dirs: Vec::new(),
            names: Vec::new(),
            items: Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    fn push(&mut self, path: &Path, is_dir: bool) {
        let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
            return;
        };
        // A folder's entries arrive together, so checking the last folder dedupes them.
        if self.dirs.last().map(PathBuf::as_path) != Some(dir) {
            self.dirs.push(dir.to_path_buf());
        }
        let bytes = name.as_bytes();
        self.items.push(Item {
            name: self.names.len() as u32,
            len: bytes.len().min(u16::MAX as usize) as u16,
            dir: (self.dirs.len() - 1) as u32,
            is_dir,
        });
        self.names
            .extend_from_slice(&bytes[..bytes.len().min(u16::MAX as usize)]);
    }

    /// Moves another index's items to the end of this one.
    fn append(&mut self, other: &mut Index) {
        let (names, dirs) = (self.names.len() as u32, self.dirs.len() as u32);
        self.items.extend(other.items.drain(..).map(|item| Item {
            name: item.name + names,
            dir: item.dir + dirs,
            ..item
        }));
        self.names.append(&mut other.names);
        self.dirs.append(&mut other.dirs);
    }

    fn name_bytes(&self, i: usize) -> &[u8] {
        let item = self.items[i];
        &self.names[item.name as usize..item.name as usize + item.len as usize]
    }

    fn name(&self, i: usize) -> Cow<'_, str> {
        String::from_utf8_lossy(self.name_bytes(i))
    }

    fn path(&self, i: usize) -> PathBuf {
        self.dirs[self.items[i].dir as usize].join(OsStr::from_bytes(self.name_bytes(i)))
    }

    /// The best matches among `range`, best first, and how many matched in all.
    /// The limit bounds displayed results, never the scan: a late exact match can
    /// replace an early fuzzy one.
    fn best_matches(
        &self,
        range: Range<usize>,
        query: &Query,
        limit: usize,
        cancel: &AtomicBool,
    ) -> (Vec<usize>, usize) {
        // Ties go to the earlier item; the view orders equal matches itself.
        type Ranked = Reverse<(MatchScore, Reverse<usize>)>;
        let keep = |heap: &mut BinaryHeap<Ranked>, rank: Ranked| {
            if heap.len() < limit {
                heap.push(rank);
            } else if heap.peek().is_some_and(|worst| rank < *worst) {
                heap.pop();
                heap.push(rank);
            }
        };
        let (best, total) = range
            .into_par_iter()
            .with_min_len(4096)
            .fold(
                || (BinaryHeap::new(), 0usize),
                |(mut heap, total), i| {
                    if cancel.load(Ordering::Relaxed) {
                        return (heap, total);
                    }
                    match query.score_name(&self.name(i), self.items[i].is_dir) {
                        Some(score) => {
                            keep(&mut heap, Reverse((score, Reverse(i))));
                            (heap, total + 1)
                        }
                        None => (heap, total),
                    }
                },
            )
            .reduce(
                || (BinaryHeap::new(), 0),
                |(a, x), (b, y)| {
                    let (mut a, b) = if a.len() >= b.len() { (a, b) } else { (b, a) };
                    for rank in b {
                        keep(&mut a, rank);
                    }
                    (a, x + y)
                },
            );
        (
            best.into_sorted_vec()
                .into_iter()
                .map(|Reverse((_, Reverse(i)))| i)
                .collect(),
            total,
        )
    }

    fn paths(&self, items: &[usize]) -> Vec<PathBuf> {
        items.iter().map(|&i| self.path(i)).collect()
    }
}

/// Entries for the given paths, read in parallel; paths deleted since the scan
/// are skipped.
fn entries(root: &Path, paths: Vec<PathBuf>, cancel: &AtomicBool) -> Vec<Entry> {
    paths
        .into_par_iter()
        .filter_map(|path| {
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            let meta = std::fs::symlink_metadata(&path).ok()?;
            let name = path.file_name()?.to_string_lossy().into_owned();
            let rel = relative_parent(root, &path);
            let mut e = make_entry(path, name, meta);
            e.rel = Some(rel);
            Some(e)
        })
        .collect()
}

pub struct SearchMsg {
    pub tab_id: u64,
    pub generation: u64,
    /// Which query revision of the search the results are for.
    pub rev: u64,
    pub batch: Vec<Entry>,
    pub done: bool,
    pub truncated: bool,
    pub replace: bool,
    pub index: Option<Arc<Index>>,
}

/// A new query for a search whose scan is still running. `None` shows nothing
/// while the scan goes on building the index.
pub struct Update {
    pub rev: u64,
    pub query: Option<String>,
}

pub struct Request {
    pub tab_id: u64,
    pub generation: u64,
    pub root: PathBuf,
    pub query: Option<String>,
    pub show_hidden: bool,
    pub cancel: Arc<AtomicBool>,
    pub index: Option<Arc<Index>>,
    /// Query changes while scanning; unused when `index` can be reused.
    pub updates: Receiver<Update>,
}

pub fn spawn(req: Request, tx: Sender<SearchMsg>, ctx: egui::Context) {
    std::thread::spawn(move || {
        let job = Job {
            req: &req,
            tx: &tx,
            ctx: &ctx,
        };
        let reusable = req
            .index
            .clone()
            .filter(|index| index.show_hidden == req.show_hidden);
        // `streamed`: how many matched during the scan, when those results are
        // already the full answer for the final query.
        let (index, mut rev, mut query, mut streamed) = match reusable {
            Some(index) => (index, 0, req.query.as_deref().map(Query::new), None),
            None => match job.scan() {
                Some(s) => (s.index, s.rev, s.query, Some(s.matched)),
                None => return,
            },
        };
        // Pick up a query typed just as the scan finished.
        while let Ok(update) = req.updates.try_recv() {
            rev = update.rev;
            query = update.query.as_deref().map(Query::new);
            streamed = None;
        }
        if streamed.is_some_and(|total| total <= MAX_RESULTS) {
            job.send(SearchMsg {
                rev,
                done: true,
                index: Some(index),
                ..job.msg()
            });
            return;
        }
        let (best, total) = match &query {
            Some(q) => index.best_matches(0..index.len(), q, MAX_RESULTS, &req.cancel),
            None => (Vec::new(), 0),
        };
        let batch = entries(&req.root, index.paths(&best), &req.cancel);
        if req.cancel.load(Ordering::Relaxed) {
            return;
        }
        job.send(SearchMsg {
            rev,
            batch,
            done: true,
            truncated: total > MAX_RESULTS,
            replace: true,
            index: Some(index),
            ..job.msg()
        });
    });
}

/// A finished scan and the query revision it ended on.
struct Scanned {
    index: Arc<Index>,
    rev: u64,
    query: Option<Query>,
    /// Items matching that query; the streamed results are complete when it fits the limit.
    matched: usize,
}

struct Job<'a> {
    req: &'a Request,
    tx: &'a Sender<SearchMsg>,
    ctx: &'a egui::Context,
}

impl Job<'_> {
    fn msg(&self) -> SearchMsg {
        SearchMsg {
            tab_id: self.req.tab_id,
            generation: self.req.generation,
            rev: 0,
            batch: Vec::new(),
            done: false,
            truncated: false,
            replace: false,
            index: None,
        }
    }

    fn send(&self, msg: SearchMsg) {
        let _ = self.tx.send(msg);
        self.ctx.request_repaint();
    }

    /// Walks the tree into a new index while streaming matches for the latest
    /// query. Returns the index with the query revision current at the end, or
    /// `None` when cancelled.
    fn scan(&self) -> Option<Scanned> {
        let req = self.req;
        let shared = Mutex::new(Index::new(req.show_hidden));
        let walking = AtomicBool::new(true);
        let mut rev = 0;
        let mut query = req.query.as_deref().map(Query::new);
        let mut matched = 0;
        let (root, show_hidden, cancel) = (&req.root, req.show_hidden, &*req.cancel);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                WalkBuilder::new(root)
                    .hidden(!show_hidden)
                    .ignore(false)
                    .git_ignore(false)
                    .git_global(false)
                    .git_exclude(false)
                    .parents(false)
                    .follow_links(false)
                    .same_file_system(true)
                    .threads(
                        std::thread::available_parallelism()
                            .map(|n| n.get())
                            .unwrap_or(4),
                    )
                    .build_parallel()
                    .visit(&mut VisitorBuilder {
                        shared: &shared,
                        cancel,
                    });
                walking.store(false, Ordering::Release);
            });
            // Items scored against the current query, and matches sent for it.
            let (mut scored, mut sent, mut replace) = (0, 0, true);
            let mut updates_open = true;
            loop {
                let finished = !walking.load(Ordering::Acquire);
                if req.cancel.load(Ordering::Relaxed) {
                    break;
                }
                let (paths, end) = {
                    let index = shared.lock().expect("lock poisoned");
                    let end = index.len();
                    let best = match &query {
                        Some(q) => {
                            let (best, total) =
                                index.best_matches(scored..end, q, MAX_RESULTS - sent, &req.cancel);
                            matched += total;
                            best
                        }
                        None => Vec::new(),
                    };
                    (index.paths(&best), end)
                };
                let batch = entries(&req.root, paths, &req.cancel);
                scored = end;
                if replace || !batch.is_empty() {
                    sent += batch.len();
                    self.send(SearchMsg {
                        rev,
                        batch,
                        replace,
                        ..self.msg()
                    });
                    replace = false;
                }
                if finished {
                    break;
                }
                let update = if updates_open {
                    match req.updates.recv_timeout(STREAM_EVERY) {
                        Ok(update) => Some(update),
                        Err(RecvTimeoutError::Timeout) => None,
                        Err(RecvTimeoutError::Disconnected) => {
                            updates_open = false;
                            None
                        }
                    }
                } else {
                    std::thread::sleep(STREAM_EVERY);
                    None
                };
                if let Some(mut update) = update {
                    while let Ok(newer) = req.updates.try_recv() {
                        update = newer;
                    }
                    rev = update.rev;
                    query = update.query.as_deref().map(Query::new);
                    (scored, sent, matched, replace) = (0, 0, 0, true);
                }
            }
        });
        if req.cancel.load(Ordering::Relaxed) {
            return None;
        }
        let index = shared.into_inner().expect("lock poisoned");
        Some(Scanned {
            index: Arc::new(index),
            rev,
            query,
            matched,
        })
    }
}

struct VisitorBuilder<'a> {
    shared: &'a Mutex<Index>,
    cancel: &'a AtomicBool,
}
impl<'s> ParallelVisitorBuilder<'s> for VisitorBuilder<'s> {
    fn build(&mut self) -> Box<dyn ParallelVisitor + 's> {
        Box::new(Visitor {
            shared: self.shared,
            cancel: self.cancel,
            chunk: Index::new(false),
            flushed: Instant::now(),
        })
    }
}
struct Visitor<'a> {
    shared: &'a Mutex<Index>,
    cancel: &'a AtomicBool,
    chunk: Index,
    flushed: Instant,
}
impl Visitor<'_> {
    fn flush(&mut self) {
        if !self.chunk.items.is_empty() {
            self.shared
                .lock()
                .expect("lock poisoned")
                .append(&mut self.chunk);
        }
        self.flushed = Instant::now();
    }
}
impl ParallelVisitor for Visitor<'_> {
    fn visit(&mut self, de: Result<DirEntry, ignore::Error>) -> WalkState {
        if self.cancel.load(Ordering::Relaxed) {
            return WalkState::Quit;
        }
        let Ok(de) = de else {
            return WalkState::Continue;
        };
        if de.depth() == 0 {
            return WalkState::Continue;
        }
        let is_dir = de
            .file_type()
            .is_some_and(|t| t.is_dir() || (t.is_symlink() && de.path().is_dir()));
        self.chunk.push(de.path(), is_dir);
        // Hand items over regularly, so a slow walk still streams results.
        let n = self.chunk.items.len();
        if n >= CHUNK || (n.is_multiple_of(64) && self.flushed.elapsed() >= STREAM_EVERY) {
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
            assert_eq!(
                Query::new(text).score_name(name, is_dir).is_some(),
                expected,
                "{text:?} / {name}"
            );
        }
        assert_eq!(Query::new("nd ext:txt").highlights("NaDya.txt"), [0, 2]);
        assert_eq!(Query::new("дн").highlights("ДоНя.txt"), [0, 2]);
        assert_eq!(Query::new("i").highlights("İstanbul"), [0]);
        assert_eq!(Query::new("=ND").highlights("ND"), [0, 1]);
    }

    #[test]
    fn late_best_matches_survive_the_result_limit() {
        let mut index = Index::new(false);
        for i in 0..MAX_RESULTS + 5 {
            index.push(
                Path::new(&format!("/root/sub{}/n---d-{i:06}", i % 7)),
                false,
            );
        }
        let exact = index.len();
        index.push(Path::new("/root/ND"), false);
        let (best, total) = index.best_matches(
            0..index.len(),
            &Query::new("nd"),
            MAX_RESULTS,
            &AtomicBool::new(false),
        );
        assert!(total > MAX_RESULTS);
        assert_eq!(best.len(), MAX_RESULTS);
        assert_eq!(best[0], exact);
        assert_eq!(index.path(exact), Path::new("/root/ND"));
        assert_eq!(index.path(3), Path::new("/root/sub3/n---d-000003"));
    }

    #[test]
    fn zero_limit_still_counts_matches() {
        let mut index = Index::new(false);
        for i in 0..10 {
            index.push(Path::new(&format!("/root/n---d-{i}")), false);
        }
        let (best, total) = index.best_matches(
            0..index.len(),
            &Query::new("nd"),
            0,
            &AtomicBool::new(false),
        );
        assert_eq!((best, total), (vec![], 10));
    }

    #[test]
    fn index_chunks_keep_paths_when_merged() {
        let mut a = Index::new(false);
        a.push(Path::new("/r/a/one"), false);
        a.push(Path::new("/r/a/two"), true);
        let mut b = Index::new(false);
        b.push(Path::new("/r/b/three"), false);
        b.push(Path::new("/r/b/four"), false);
        a.append(&mut b);
        assert!(b.items.is_empty());
        assert_eq!(a.dirs.len(), 2);
        let paths: Vec<_> = (0..a.len()).map(|i| a.path(i)).collect();
        assert_eq!(
            paths,
            ["/r/a/one", "/r/a/two", "/r/b/three", "/r/b/four"].map(PathBuf::from)
        );
        assert!(a.items[1].is_dir && !a.items[2].is_dir);
    }

    #[test]
    fn highlights_follow_the_alignment_that_ranked_the_name() {
        // "port" ranks on the word "Port", not the end of "Export".
        assert_eq!(
            Query::new("port").highlights("Export-Port.txt"),
            [7, 8, 9, 10]
        );
        // The best subsequence takes the later, word-start 'n'.
        assert_eq!(Query::new("nd").highlights("n------nice-Doc"), [7, 12]);
        assert_eq!(Query::new("nd").highlights("NaDya"), [0, 2]);
    }

    #[test]
    fn type_categories_and_unknown_filters() {
        let q = Query::new("type:image cat");
        assert!(q.unknown.is_empty());
        assert!(q.score_name("Cat.JPG", false).is_some());
        assert!(q.score_name("cat.txt", false).is_none());
        assert!(q.score_name("cat.jpg", true).is_none());
        assert!(Query::new("type:doc").score_name("cv.pdf", false).is_some());
        assert_eq!(Query::new("size:>1M report").unknown, ["size:>1M"]);
        assert_eq!(Query::new("type:pictures").unknown, ["type:pictures"]);
        assert_eq!(Query::new("ext:").unknown, ["ext:"]);
        // Not filter-shaped: a drive letter, a time, quoted text.
        assert!(Query::new("c: 10:30 \"a:b\"").unknown.is_empty());
    }

    #[test]
    fn ascii_fast_path_agrees_with_unicode_matching() {
        for (query, name) in [
            ("nd", "NaDya"),
            ("ND", "a-nd.TXT"),
            ("\"Nd\"", "a-ND"),
            ("=readme.md", "README.md"),
            ("é", "cafe"),
            ("rpt fin", "Report-final"),
        ] {
            let q = Query::new(query);
            let lower = name.to_lowercase();
            let slow = !q.exact.as_ref().is_some_and(|e| e != &lower)
                && q.literals.iter().all(|s| lower.contains(s))
                && q.terms.iter().all(|t| {
                    let mut c = lower.chars();
                    t.iter().all(|&x| c.any(|y| y == x))
                });
            assert_eq!(q.matches(name), slow, "{query:?} / {name}");
        }
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
            assert_eq!(
                q.score(name).is_some(),
                matches,
                "scoring {query:?} in {name:?}"
            );
        }
    }

    #[test]
    fn exact_prefix_and_substring_matches_beat_fuzzy_matches() {
        let q = Query::new("nd");
        let ranked = ["ND", "nd-notes.txt", "a-nd.txt", "NaDya", "Named"];
        for pair in ranked.windows(2) {
            assert!(
                q.score(pair[0]) > q.score(pair[1]),
                "{} should beat {}",
                pair[0],
                pair[1]
            );
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
