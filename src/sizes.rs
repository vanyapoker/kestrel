//! Shared, bounded directory sizing, independent of listing cancellation and Rayon’s global pool.
use crate::fsops;
use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const TTL: Duration = Duration::from_secs(30);
const MAX_CACHED: usize = 32_768;
const WORKERS: usize = 4;

#[derive(Clone, Copy)]
struct Cached {
    // None records an excluded mount point, avoiding repeated attempts every frame.
    size: Option<u64>,
    device: u64,
    at: Instant,
}

struct Job {
    path: PathBuf,
    cancel: Arc<AtomicBool>,
}

enum Message {
    Directory {
        generation: u64,
        path: PathBuf,
        value: Cached,
    },
    Done(u64),
}

pub struct Sizes {
    pool: rayon::ThreadPool,
    cache: Arc<Mutex<HashMap<PathBuf, Cached>>>,
    jobs: HashMap<u64, Job>,
    generation: u64,
    tx: SyncSender<Message>,
    rx: Receiver<Message>,
    ctx: egui::Context,
    sweep_at: Instant,
}

fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

impl Sizes {
    pub fn new(ctx: egui::Context) -> Self {
        Self::with_workers(ctx, WORKERS)
    }

    fn with_workers(ctx: egui::Context, workers: usize) -> Self {
        let (tx, rx) = sync_channel(512);
        Self {
            pool: rayon::ThreadPoolBuilder::new()
                .num_threads(workers)
                .thread_name(|i| format!("folder-size-{i}"))
                .build()
                .expect("folder size pool"),
            cache: Arc::new(Mutex::new(HashMap::new())),
            jobs: HashMap::new(),
            generation: 0,
            tx,
            rx,
            ctx,
            sweep_at: Instant::now(),
        }
    }

    /// Invalidate descendants as well as ancestors (rename, removal, refresh), and
    /// reject all later messages from overlapping jobs, including queued messages.
    pub fn invalidate(&mut self, path: &Path, sizes: &mut HashMap<PathBuf, u64>) {
        self.cache
            .lock()
            .expect("lock poisoned")
            .retain(|p, _| !overlaps(p, path));
        sizes.retain(|p, _| !overlaps(p, path));
        for job in self.jobs.values().filter(|j| overlaps(&j.path, path)) {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Reconcile interests from all tabs/columns. An ancestor job covers child
    /// requests; an ancestor request waits for an existing child job, then reuses
    /// its cached total. Cancelled jobs retain their reservation until Done.
    pub fn sync(
        &mut self,
        mut wanted: Vec<PathBuf>,
        sizes: &mut HashMap<PathBuf, u64>,
        sizing: &mut HashSet<PathBuf>,
        now: Instant,
    ) -> bool {
        wanted.sort_by(|a, b| {
            a.components()
                .count()
                .cmp(&b.components().count())
                .then(a.cmp(b))
        });
        wanted.dedup();
        let pinned: HashSet<_> = wanted.iter().cloned().collect();
        for job in self.jobs.values() {
            if !wanted.iter().any(|p| overlaps(p, &job.path)) {
                job.cancel.store(true, Ordering::Relaxed);
            }
        }
        let mut changed = false;
        {
            let mut cache = self.cache.lock().expect("lock poisoned");
            // Bound UI work per frame as well as the worker-to-UI queue.
            for msg in self.rx.try_iter().take(1024) {
                match msg {
                    Message::Directory {
                        generation,
                        path,
                        value,
                    } if self
                        .jobs
                        .get(&generation)
                        .is_some_and(|j| !j.cancel.load(Ordering::Relaxed)) =>
                    {
                        if let Some(size) = value.size {
                            changed |= sizes.insert(path.clone(), size) != Some(size);
                        }
                        cache.insert(path, value);
                    }
                    Message::Done(id) => {
                        self.jobs.remove(&id);
                    }
                    _ => {}
                }
            }
            if now >= self.sweep_at || cache.len() > MAX_CACHED.max(pinned.len()) {
                cache.retain(|_, v| now.saturating_duration_since(v.at) < TTL);
                // Open rows stay pinned, even when a directory has more entries
                // than the history budget. Otherwise eviction would cause an
                // endless cycle of rescanning those rows. Evict history in chunks.
                if cache.len() > MAX_CACHED.max(pinned.len()) {
                    let mut oldest: Vec<_> = cache
                        .iter()
                        .filter(|(p, _)| !pinned.contains(*p))
                        .map(|(p, v)| (p.clone(), v.at))
                        .collect();
                    oldest.sort_unstable_by_key(|(_, at)| *at);
                    let remove = (cache.len() - MAX_CACHED.max(pinned.len())).max(MAX_CACHED / 8);
                    for (p, _) in oldest.into_iter().take(remove) {
                        cache.remove(&p);
                    }
                }
                sizes.retain(|p, _| {
                    let keep = cache.contains_key(p);
                    changed |= !keep;
                    keep
                });
                self.sweep_at = now + Duration::from_secs(1);
            }
            sizing.clear();
            for path in &wanted {
                if let Some(value) = cache
                    .get(path)
                    .filter(|v| now.saturating_duration_since(v.at) < TTL)
                {
                    if let Some(size) = value.size {
                        changed |= sizes.insert(path.clone(), size) != Some(size);
                    }
                    continue;
                }
                changed |= sizes.remove(path).is_some();
                sizing.insert(path.clone());
                if self.jobs.len() >= self.pool.current_num_threads()
                    || self.jobs.values().any(|j| overlaps(&j.path, path))
                {
                    continue;
                }
                self.generation += 1;
                let generation = self.generation;
                let cancel = Arc::new(AtomicBool::new(false));
                self.jobs.insert(
                    generation,
                    Job {
                        path: path.clone(),
                        cancel: cancel.clone(),
                    },
                );
                let (path, cache, tx, ctx) = (
                    path.clone(),
                    self.cache.clone(),
                    self.tx.clone(),
                    self.ctx.clone(),
                );
                self.pool.spawn(move || {
                    // Aggregation must not renew the lifetime of reused totals.
                    // A job-wide minimum is conservative for sibling branches and
                    // ensures every parent expires with its oldest dependency.
                    let as_of = Mutex::new(Instant::now());
                    let device = fsops::device_of(&path);
                    let allowed = device.is_some()
                        && path.parent().and_then(fsops::device_of) == device
                        && std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir());
                    if let Some(device) = device.filter(|_| allowed) {
                        fsops::dir_size(
                            &path,
                            device,
                            &cancel,
                            &|p| {
                                let cached = cache
                                    .lock()
                                    .expect("lock poisoned")
                                    .get(p)
                                    .filter(|v| v.device == device && v.at.elapsed() < TTL)
                                    .copied();
                                if let Some(value) = cached.filter(|v| v.size.is_some()) {
                                    let mut oldest = as_of.lock().expect("lock poisoned");
                                    *oldest = (*oldest).min(value.at);
                                    value.size
                                } else {
                                    None
                                }
                            },
                            &|p, size| {
                                let value = Cached {
                                    size: Some(size),
                                    device,
                                    at: *as_of.lock().expect("lock poisoned"),
                                };
                                let _ = tx.send(Message::Directory {
                                    generation,
                                    path: p.to_path_buf(),
                                    value,
                                });
                                ctx.request_repaint();
                            },
                        );
                    } else {
                        let value = Cached {
                            size: None,
                            device: device.unwrap_or(0),
                            at: Instant::now(),
                        };
                        let _ = tx.send(Message::Directory {
                            generation,
                            path,
                            value,
                        });
                    }
                    let _ = tx.send(Message::Done(generation));
                    ctx.request_repaint();
                });
            }
        }
        // Nonrecursive watches cannot detect every descendant edit. Expiry is a
        // freshness backstop, including while the window is otherwise idle.
        if !wanted.is_empty() {
            self.ctx.request_repaint_after(Duration::from_secs(1));
        }
        changed
    }
}

impl Drop for Sizes {
    fn drop(&mut self) {
        for job in self.jobs.values() {
            job.cancel.store(true, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::AtomicU64;
    use std::sync::mpsc::{Sender, channel};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Tree(PathBuf);
    impl Tree {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "kestrel-sizes-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn file(&self, relative: &str, bytes: usize) -> PathBuf {
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, vec![0; bytes]).unwrap();
            path
        }
    }
    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    // Hold a pool worker so request/cancellation order is deterministic, without
    // relying on how quickly the filesystem completes a scan.
    struct Gate(Option<Sender<()>>);
    impl Gate {
        fn new(service: &Sizes) -> Self {
            let (started_tx, started_rx) = channel();
            let (release_tx, release_rx) = channel();
            service.pool.spawn(move || {
                started_tx.send(()).unwrap();
                let _ = release_rx.recv_timeout(Duration::from_secs(10));
            });
            started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            Self(Some(release_tx))
        }
    }
    impl Drop for Gate {
        fn drop(&mut self) {
            if let Some(tx) = self.0.take() {
                let _ = tx.send(());
            }
        }
    }

    struct Driver {
        service: Sizes,
        sizes: HashMap<PathBuf, u64>,
        sizing: HashSet<PathBuf>,
    }
    impl Driver {
        fn new(workers: usize) -> Self {
            Self {
                service: Sizes::with_workers(egui::Context::default(), workers),
                sizes: HashMap::new(),
                sizing: HashSet::new(),
            }
        }
        fn sync(&mut self, paths: &[PathBuf]) {
            self.service.sync(
                paths.to_vec(),
                &mut self.sizes,
                &mut self.sizing,
                Instant::now(),
            );
        }
        fn finish(&mut self, paths: &[PathBuf]) {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                self.sync(paths);
                if self.sizing.is_empty() && self.service.jobs.is_empty() {
                    break;
                }
                assert!(Instant::now() < deadline, "size jobs did not finish");
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }

    #[test]
    fn subtree_cache_and_shared_parent_requests() {
        let tree = Tree::new();
        tree.file("parent/child/grandchild/data", 17);
        tree.file("parent/other", 3);
        let parent = tree.0.join("parent");
        let child = parent.join("child");
        let grandchild = child.join("grandchild");
        let mut d = Driver::new(1);
        let gate = Gate::new(&d.service);
        d.sync(&[parent.clone(), child.clone(), parent.clone()]);
        assert_eq!(d.service.jobs.len(), 1);
        assert_eq!(d.service.jobs.values().next().unwrap().path, parent);
        // A second tab still needs the child after the original parent tab closes.
        d.sync(std::slice::from_ref(&child));
        assert!(
            !d.service
                .jobs
                .values()
                .next()
                .unwrap()
                .cancel
                .load(Ordering::Relaxed)
        );
        drop(gate);
        d.finish(std::slice::from_ref(&child));
        assert_eq!(d.sizes[&parent], 20);
        assert_eq!(d.sizes[&child], 17);
        assert_eq!(d.sizes[&grandchild], 17);
        let generations = d.service.generation;
        d.finish(&[child, grandchild]);
        assert_eq!(
            d.service.generation, generations,
            "opening a cached subtree must not spawn scans"
        );
    }

    #[test]
    fn ancestor_waits_for_child_then_reuses_completed_total() {
        let tree = Tree::new();
        tree.file("parent/child/data", 17);
        tree.file("parent/other", 3);
        let parent = tree.0.join("parent");
        let child = parent.join("child");
        let mut d = Driver::new(2);
        let gate1 = Gate::new(&d.service);
        let gate2 = Gate::new(&d.service);
        d.sync(std::slice::from_ref(&child));
        d.sync(&[parent.clone(), child.clone()]);
        assert_eq!(
            d.service.jobs.len(),
            1,
            "ancestor must wait even when another worker is free"
        );
        assert_eq!(d.service.jobs.values().next().unwrap().path, child);
        drop(gate1);
        drop(gate2);
        d.finish(std::slice::from_ref(&child));
        // An unobserved edit demonstrates that scanning the parent really reuses
        // the child cache rather than traversing the child's files a second time.
        tree.file("parent/child/data", 29);
        let child_at = Instant::now() - TTL + Duration::from_secs(5);
        d.service
            .cache
            .lock()
            .expect("lock poisoned")
            .get_mut(&child)
            .unwrap()
            .at = child_at;
        d.finish(std::slice::from_ref(&parent));
        assert_eq!(d.sizes[&parent], 20);
        assert!(
            d.service.cache.lock().expect("lock poisoned")[&parent].at <= child_at,
            "aggregating a cached child must not extend its freshness"
        );
        d.service.invalidate(&child, &mut d.sizes);
        d.finish(&[parent.clone(), child.clone()]);
        assert_eq!(d.sizes[&parent], 32);
        assert_eq!(d.sizes[&child], 29);
    }

    #[test]
    fn invalidation_rejects_queued_and_late_generations() {
        let tree = Tree::new();
        tree.file("folder/data", 11);
        let path = tree.0.join("folder");
        let mut d = Driver::new(1);
        let gate = Gate::new(&d.service);
        d.sync(std::slice::from_ref(&path));
        let generation = d.service.generation;
        let stale = || Message::Directory {
            generation,
            path: path.clone(),
            value: Cached {
                size: Some(999),
                device: fsops::device_of(&path).unwrap(),
                at: Instant::now(),
            },
        };
        d.service.tx.send(stale()).unwrap();
        d.service.invalidate(&path, &mut d.sizes);
        d.sync(std::slice::from_ref(&path));
        assert!(!d.sizes.contains_key(&path));
        assert!(
            !d.service
                .cache
                .lock()
                .expect("lock poisoned")
                .contains_key(&path)
        );
        assert_eq!(
            d.service.jobs.len(),
            1,
            "cancelled jobs must hold their reservation until they exit"
        );
        drop(gate);
        d.finish(std::slice::from_ref(&path));
        assert!(d.service.generation > generation);
        assert_eq!(d.sizes[&path], 11);
        d.service.tx.send(stale()).unwrap();
        d.sync(std::slice::from_ref(&path));
        assert_eq!(d.sizes[&path], 11);
    }

    #[test]
    fn expiry_revalidates_unwatched_descendants() {
        let tree = Tree::new();
        tree.file("folder/child/data", 11);
        let path = tree.0.join("folder");
        let mut d = Driver::new(1);
        d.finish(std::slice::from_ref(&path));
        tree.file("folder/child/data", 29);
        for value in d.service.cache.lock().expect("lock poisoned").values_mut() {
            value.at = Instant::now() - TTL;
        }
        d.service.sweep_at = Instant::now();
        let generations = d.service.generation;
        d.finish(std::slice::from_ref(&path));
        assert_eq!(d.sizes[&path], 29);
        assert!(d.service.generation > generations);
    }

    #[test]
    fn abandoned_work_is_cancelled_and_can_be_requested_again() {
        let tree = Tree::new();
        tree.file("folder/data", 11);
        let path = tree.0.join("folder");
        let mut d = Driver::new(1);
        let gate = Gate::new(&d.service);
        d.sync(std::slice::from_ref(&path));
        d.sync(&[]);
        assert!(d.sizing.is_empty());
        assert!(
            d.service
                .jobs
                .values()
                .all(|j| j.cancel.load(Ordering::Relaxed))
        );
        drop(gate);
        d.finish(&[]);
        assert!(!d.sizes.contains_key(&path));
        d.finish(std::slice::from_ref(&path));
        assert_eq!(d.sizes[&path], 11);
    }

    #[test]
    fn pool_jobs_and_cache_are_bounded() {
        let tree = Tree::new();
        let paths: Vec<_> = (0..8)
            .map(|i| {
                tree.file(&format!("folder{i}/data"), i);
                tree.0.join(format!("folder{i}"))
            })
            .collect();
        let mut d = Driver::new(2);
        let gate1 = Gate::new(&d.service);
        let gate2 = Gate::new(&d.service);
        d.sync(&paths);
        assert_eq!(d.service.pool.current_num_threads(), 2);
        assert_eq!(d.service.jobs.len(), 2);
        assert_eq!(d.sizing.len(), 8);
        drop(gate1);
        drop(gate2);
        d.finish(&paths);
        assert_eq!(d.sizes.len(), 8);
        let value = Cached {
            size: Some(0),
            device: 0,
            at: Instant::now(),
        };
        for i in 0..=MAX_CACHED {
            let path = tree.0.join(format!("cached{i}"));
            d.service
                .cache
                .lock()
                .expect("lock poisoned")
                .insert(path.clone(), value);
            d.sizes.insert(path, 0);
        }
        d.sync(&[]);
        let len = d.service.cache.lock().expect("lock poisoned").len();
        assert!(len <= MAX_CACHED);
        assert_eq!(d.sizes.len(), len);
    }

    #[test]
    fn open_rows_are_not_evicted_into_an_endless_rescan_cycle() {
        let tree = Tree::new();
        let mut d = Driver::new(1);
        let paths: Vec<_> = (0..=MAX_CACHED)
            .map(|i| tree.0.join(format!("row{i}")))
            .collect();
        let value = Cached {
            size: Some(7),
            device: 0,
            at: Instant::now(),
        };
        for path in &paths {
            d.service
                .cache
                .lock()
                .expect("lock poisoned")
                .insert(path.clone(), value);
        }
        d.sync(&paths);
        assert_eq!(d.sizes.len(), paths.len());
        assert!(d.sizing.is_empty());
        assert_eq!(d.service.generation, 0);
        // Closing those rows restores the normal history bound.
        d.sync(&[]);
        assert!(d.service.cache.lock().expect("lock poisoned").len() <= MAX_CACHED);
    }

    #[test]
    fn nested_walks_stay_in_the_dedicated_pool_and_excluded_roots_settle() {
        let tree = Tree::new();
        for i in 0..16 {
            tree.file(&format!("folder/child{i}/data"), 1);
        }
        let path = tree.0.join("folder");
        let dev = fsops::device_of(&path).unwrap();
        let mut d = Driver::new(2);
        let visited = Mutex::new(HashSet::new());
        let size = d.service.pool.install(|| {
            fsops::dir_size(&path, dev, &AtomicBool::new(false), &|_| None, &|p, _| {
                assert!(
                    std::thread::current()
                        .name()
                        .unwrap()
                        .starts_with("folder-size-")
                );
                visited
                    .lock()
                    .expect("lock poisoned")
                    .insert(p.to_path_buf());
            })
        });
        assert_eq!(size, Some(16));
        assert_eq!(visited.into_inner().expect("lock poisoned").len(), 17);
        let link = tree.0.join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        let mut excluded = vec![link.clone()];
        let proc = PathBuf::from("/proc");
        if fsops::device_of(&proc) != fsops::device_of(Path::new("/")) {
            excluded.push(proc);
        }
        d.finish(&excluded);
        assert!(excluded.iter().all(|p| !d.sizes.contains_key(p)));
        let generation = d.service.generation;
        d.finish(&excluded);
        assert_eq!(
            d.service.generation, generation,
            "excluded roots must not be retried every frame"
        );
    }

    #[test]
    fn scanner_skips_symlinks_and_stops_before_publishing_cancelled_work() {
        let tree = Tree::new();
        tree.file("folder/child/data", 11);
        tree.file("outside", 99);
        let path = tree.0.join("folder");
        std::os::unix::fs::symlink(tree.0.join("outside"), path.join("link")).unwrap();
        std::os::unix::fs::symlink(&path, path.join("loop")).unwrap();
        let dev = fsops::device_of(&path).unwrap();
        let cancel = AtomicBool::new(false);
        assert_eq!(
            fsops::dir_size(&path, dev, &cancel, &|_| None, &|_, _| {}),
            Some(11)
        );
        assert_eq!(
            fsops::dir_size(
                &path,
                dev,
                &cancel,
                &|_| {
                    cancel.store(true, Ordering::Relaxed);
                    None
                },
                &|_, _| panic!("cancelled work must not be published")
            ),
            None
        );
        assert_eq!(
            fsops::dir_size(
                &path,
                dev,
                &cancel,
                &|_| panic!("already cancelled"),
                &|_, _| {}
            ),
            None
        );
    }
}
