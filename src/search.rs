//! Recursive filename search, parallel over the directory tree. Results stream
//! back in batches so the first matches show up immediately.

use crate::fsops::{Entry, make_entry};
use eframe::egui;
use ignore::{DirEntry, ParallelVisitor, ParallelVisitorBuilder, WalkBuilder, WalkState};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;

pub const MAX_RESULTS: usize = 20_000;
const BATCH: usize = 256;

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
        let terms: Vec<String> = req.query.to_lowercase().split_whitespace().map(String::from).collect();
        let count = Arc::new(AtomicUsize::new(0));
        let shared = Shared {
            tab_id: req.tab_id,
            generation: req.generation,
            root: req.root.clone(),
            terms,
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
    terms: Vec<String>,
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
        let lower = name.to_lowercase();
        if !s.terms.iter().all(|t| lower.contains(t.as_str())) {
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
