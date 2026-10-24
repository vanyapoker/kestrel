//! Watches the folders open in tabs (non-recursively) so listings refresh by themselves.

use eframe::egui;
use notify::{EventKind, RecursiveMode, Watcher as _};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};

pub struct Watcher {
    inner: Option<notify::RecommendedWatcher>,
    watched: HashSet<PathBuf>,
    rx: Receiver<PathBuf>,
}

impl Watcher {
    pub fn new(ctx: egui::Context) -> Self {
        let (tx, rx) = channel();
        let inner = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(ev) = res else { return };
            // Opening/reading files (including our own previews) must not trigger reloads.
            if matches!(ev.kind, EventKind::Access(_)) {
                return;
            }
            for p in ev.paths {
                let _ = tx.send(p);
            }
            ctx.request_repaint();
        })
        .ok();
        Self {
            inner,
            watched: HashSet::new(),
            rx,
        }
    }

    /// Makes the watched set equal to `wanted`.
    pub fn sync(&mut self, wanted: HashSet<PathBuf>) {
        let Some(w) = self.inner.as_mut() else { return };
        for p in self.watched.difference(&wanted) {
            let _ = w.unwatch(p);
        }
        for p in wanted.difference(&self.watched) {
            let _ = w.watch(p, RecursiveMode::NonRecursive);
        }
        self.watched = wanted;
    }

    /// Paths that changed since the last call.
    pub fn drain(&self) -> Vec<PathBuf> {
        self.rx.try_iter().collect()
    }
}
