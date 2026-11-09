//! Watches the folders open in tabs (non-recursively) so listings refresh by themselves.

use eframe::egui;
use notify::{EventKind, RecursiveMode, Watcher as _};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub struct Watcher {
    inner: Option<notify::RecommendedWatcher>,
    watched: HashSet<PathBuf>,
    /// Changed paths since the last `drain`; bounded by distinct paths, not event count.
    pending: Arc<Mutex<HashSet<PathBuf>>>,
}

impl Watcher {
    pub fn new(ctx: egui::Context) -> Self {
        let pending = Arc::new(Mutex::new(HashSet::new()));
        let sink = pending.clone();
        let inner = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(ev) = res else { return };
            // Opening/reading files (including our own previews) must not trigger reloads.
            if matches!(ev.kind, EventKind::Access(_)) {
                return;
            }
            let was_empty = {
                let mut set = sink.lock().unwrap_or_else(|e| e.into_inner());
                let was_empty = set.is_empty();
                set.extend(ev.paths);
                was_empty
            };
            if was_empty {
                ctx.request_repaint();
            }
        })
        .ok();
        Self {
            inner,
            watched: HashSet::new(),
            pending,
        }
    }

    /// Makes the watched set equal to `wanted`; paths that cannot be watched are retried next sync.
    pub fn sync(&mut self, wanted: HashSet<PathBuf>) {
        let Some(w) = self.inner.as_mut() else { return };
        self.watched.retain(|p| {
            let keep = wanted.contains(p);
            if !keep {
                let _ = w.unwatch(p);
            }
            keep
        });
        let mut reported = false;
        for p in wanted {
            if self.watched.contains(&p) {
                continue;
            }
            match w.watch(&p, RecursiveMode::NonRecursive) {
                Ok(()) => {
                    self.watched.insert(p);
                }
                Err(e) if !reported => {
                    reported = true;
                    eprintln!("kestrel: cannot watch {}: {e}", p.display());
                }
                Err(_) => {}
            }
        }
    }

    /// Distinct paths that changed since the last call.
    pub fn drain(&self) -> Vec<PathBuf> {
        std::mem::take(&mut *self.pending.lock().unwrap_or_else(|e| e.into_inner()))
            .into_iter()
            .collect()
    }
}
