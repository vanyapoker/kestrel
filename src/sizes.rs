//! Shared, bounded directory sizing, independent of listing cancellation and Rayon’s global pool.
use crate::fsops::{self, Usage};
use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const TTL: Duration = Duration::from_secs(30);
/// How soon a total that changed while it was computed is computed again, so a
/// folder that keeps changing (a download, a running VM) is rescanned at this
/// pace instead of back to back.
const RETRY: Duration = Duration::from_secs(1);
const MAX_CACHED: usize = 32_768;
/// First line of the saved sizes file.
const MAGIC: &[u8] = b"kestrel folder sizes 1\n";

/// Walking is kernel-bound and scales with cores; leave half for everything else.
fn workers() -> usize {
    std::thread::available_parallelism()
        .map_or(4, |n| n.get() / 2)
        .clamp(2, 8)
}

#[derive(Clone)]
struct Cached {
    // None records an excluded mount point, avoiding repeated attempts every frame.
    usage: Option<Usage>,
    device: u64,
    at: Instant,
}

impl Cached {
    fn size(&self) -> Option<u64> {
        self.usage.as_ref().map(|u| u.size)
    }
}

struct Job {
    path: PathBuf,
    cancel: Arc<AtomicBool>,
    /// Something inside changed after the job started.
    stale: Arc<AtomicBool>,
}

type Cache = Mutex<HashMap<PathBuf, Cached>>;

/// When an entry made at `now` counts as `wait` short of expiring.
fn expiring(now: Instant, wait: Duration) -> Instant {
    now.checked_sub(TTL - wait).unwrap_or(now)
}

pub struct Sizes {
    pool: rayon::ThreadPool,
    cache: Arc<Cache>,
    jobs: HashMap<u64, Job>,
    generation: u64,
    /// Finished job generations.
    tx: SyncSender<u64>,
    rx: Receiver<u64>,
    ctx: egui::Context,
    sweep_at: Instant,
    /// Totals from the last session, shown until they are recomputed.
    previous: HashMap<PathBuf, u64>,
    store: Option<PathBuf>,
}

pub fn store_file() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("kestrel/folder-sizes"))
}

fn encode(sizes: &[(&Path, u64)]) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    let mut out = MAGIC.to_vec();
    for (path, size) in sizes {
        let bytes = path.as_os_str().as_bytes();
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(bytes);
    }
    out
}

/// Anything malformed yields what was read before it.
fn decode(data: &[u8]) -> HashMap<PathBuf, u64> {
    use std::os::unix::ffi::OsStrExt;
    let mut sizes = HashMap::new();
    let Some(mut rest) = data.strip_prefix(MAGIC) else {
        return sizes;
    };
    while let Some((size, tail)) = rest.split_first_chunk::<8>()
        && let Some((len, tail)) = tail.split_first_chunk::<4>()
        && let Some((path, tail)) = tail.split_at_checked(u32::from_le_bytes(*len) as usize)
    {
        let path = Path::new(std::ffi::OsStr::from_bytes(path));
        if path.is_absolute() {
            sizes.insert(path.to_path_buf(), u64::from_le_bytes(*size));
        }
        rest = tail;
    }
    sizes
}

fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// Workers store totals directly, so a deep walk never waits for the UI to
/// drain a queue. Checking `stale` under the cache lock pairs with
/// `invalidate`, which sets it while holding the lock: a total either lands
/// before and is expired there, or lands after, already due for a retry. Past
/// twice the history budget only the requested root is kept, so a huge walk
/// cannot outgrow the next sweep.
fn publish(
    cache: &Cache,
    job: (&AtomicBool, &AtomicBool),
    root: &Path,
    path: &Path,
    mut value: Cached,
) {
    let (cancel, stale) = job;
    let mut cache = cache.lock().expect("lock poisoned");
    if stale.load(Ordering::Relaxed) {
        value.at = value.at.min(expiring(Instant::now(), RETRY));
    }
    if !cancel.load(Ordering::Relaxed) && (path == root || cache.len() < 2 * MAX_CACHED) {
        cache.insert(path.to_path_buf(), value);
    }
}

impl Sizes {
    pub fn new(ctx: egui::Context) -> Self {
        Self::with_workers(ctx, workers())
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
            previous: HashMap::new(),
            store: None,
        }
    }

    /// Shows the totals saved in `file` until they are recomputed, and saves
    /// there on `save`.
    pub fn persist(&mut self, file: PathBuf) {
        self.previous = std::fs::read(&file).map(|d| decode(&d)).unwrap_or_default();
        self.store = Some(file);
    }

    /// Keeps the newest totals, up to the history budget.
    pub fn save(&self) {
        let Some(file) = &self.store else { return };
        let cache = self.cache.lock().expect("lock poisoned");
        let mut fresh: Vec<_> = cache
            .iter()
            .filter_map(|(p, v)| Some((p.as_path(), v.size()?, v.at)))
            .collect();
        fresh.sort_unstable_by_key(|&(_, _, at)| std::cmp::Reverse(at));
        let mut sizes: Vec<_> = fresh.into_iter().map(|(p, size, _)| (p, size)).collect();
        sizes.extend(
            self.previous
                .iter()
                .filter(|(p, _)| !cache.contains_key(*p))
                .map(|(p, &size)| (p.as_path(), size)),
        );
        sizes.truncate(MAX_CACHED);
        if let Err(e) = crate::session::save_bytes(file, &encode(&sizes)) {
            eprintln!("kestrel: could not save folder sizes: {e}");
        }
    }

    /// Expires the totals of `path`, its ancestors and its descendants (a
    /// rename, removal or refresh). They stay on screen until recomputed.
    /// Overlapping jobs run on, so a folder that keeps changing still gets a
    /// total, and what they publish is due again after `RETRY`.
    pub fn invalidate(&mut self, path: &Path) {
        self.expire(path, true);
    }

    /// Like `invalidate`, for a change the file watcher reported. A file being
    /// written reports one many times a second, so only a folder already sized
    /// pays for finding its descendants.
    pub fn changed(&mut self, path: &Path) {
        self.expire(path, false);
    }

    fn expire(&mut self, path: &Path, deep: bool) {
        let mut cache = self.cache.lock().expect("lock poisoned");
        for job in self.jobs.values().filter(|j| overlaps(&j.path, path)) {
            job.stale.store(true, Ordering::Relaxed);
        }
        let expired = expiring(Instant::now(), Duration::ZERO);
        if deep || cache.contains_key(path) {
            for (p, v) in cache.iter_mut() {
                if p.starts_with(path) {
                    v.at = v.at.min(expired);
                }
            }
        }
        for ancestor in path.ancestors() {
            if let Some(v) = cache.get_mut(ancestor) {
                v.at = v.at.min(expired);
            }
        }
    }

    /// Reconcile interests from all tabs/columns. An ancestor job covers child
    /// requests; an ancestor request waits for an existing child job, then reuses
    /// its cached total. Cancelled jobs retain their reservation until Done.
    /// An expired total, or one from the last session, stays on screen while
    /// it is recomputed.
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
        for id in self.rx.try_iter() {
            self.jobs.remove(&id);
        }
        let mut changed = false;
        let mut next_expiry = None::<Instant>;
        {
            let mut cache = self.cache.lock().expect("lock poisoned");
            if now >= self.sweep_at || cache.len() > MAX_CACHED.max(pinned.len()) {
                cache
                    .retain(|p, v| now.saturating_duration_since(v.at) < TTL || pinned.contains(p));
                // Open rows stay pinned, even when a directory has more entries
                // than the history budget. Otherwise eviction would cause an
                // endless cycle of rescanning those rows. Evict history in chunks.
                if cache.len() > MAX_CACHED.max(pinned.len()) {
                    let mut oldest: Vec<_> = cache
                        .iter()
                        .filter(|(p, _)| !pinned.contains(*p))
                        .map(|(p, v)| (p.clone(), v.at))
                        .collect();
                    let remove = (cache.len() - MAX_CACHED.max(pinned.len()))
                        .max(MAX_CACHED / 8)
                        .min(oldest.len());
                    if remove < oldest.len() {
                        oldest.select_nth_unstable_by_key(remove, |(_, at)| *at);
                    }
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
                if let Some(value) = cache.get(path) {
                    changed |= match value.size() {
                        Some(size) => sizes.insert(path.clone(), size) != Some(size),
                        None => sizes.remove(path).is_some(),
                    };
                    self.previous.remove(path);
                    let expiry = value.at + TTL;
                    if now < expiry {
                        next_expiry = Some(next_expiry.map_or(expiry, |t| t.min(expiry)));
                        continue;
                    }
                } else if let Some(&size) = self.previous.get(path) {
                    changed |= sizes.insert(path.clone(), size) != Some(size);
                } else {
                    changed |= sizes.remove(path).is_some();
                    sizing.insert(path.clone());
                }
                if self.jobs.len() >= self.pool.current_num_threads()
                    || self.jobs.values().any(|j| overlaps(&j.path, path))
                {
                    continue;
                }
                self.generation += 1;
                let generation = self.generation;
                let cancel = Arc::new(AtomicBool::new(false));
                let stale = Arc::new(AtomicBool::new(false));
                self.jobs.insert(
                    generation,
                    Job {
                        path: path.clone(),
                        cancel: cancel.clone(),
                        stale: stale.clone(),
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
                                    .cloned();
                                let value = cached?;
                                let usage = value.usage?;
                                let mut oldest = as_of.lock().expect("lock poisoned");
                                *oldest = (*oldest).min(value.at);
                                Some(usage)
                            },
                            &|p, usage| {
                                let value = Cached {
                                    usage: Some(usage.clone()),
                                    device,
                                    at: *as_of.lock().expect("lock poisoned"),
                                };
                                publish(&cache, (&cancel, &stale), &path, p, value);
                                // Coalesced: shows nested folders open in other
                                // columns as they finish, without a frame each.
                                ctx.request_repaint_after(Duration::from_millis(50));
                            },
                        );
                    } else {
                        let value = Cached {
                            usage: None,
                            device: device.unwrap_or(0),
                            at: Instant::now(),
                        };
                        publish(&cache, (&cancel, &stale), &path, &path, value);
                    }
                    let _ = tx.send(generation);
                    ctx.request_repaint();
                });
            }
        }
        // Nonrecursive watches cannot detect every descendant edit. Expiry is a
        // freshness backstop, including while the window is otherwise idle.
        if let Some(at) = next_expiry {
            self.ctx.request_repaint_after(
                at.saturating_duration_since(now)
                    .max(Duration::from_secs(1)),
            );
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
        assert_eq!(d.sizes[&child], 17);
        let generations = d.service.generation;
        d.finish(&[parent.clone(), child, grandchild.clone()]);
        assert_eq!(
            d.service.generation, generations,
            "opening a cached subtree must not spawn scans"
        );
        assert_eq!(d.sizes[&parent], 20);
        assert_eq!(d.sizes[&grandchild], 17);
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
        d.service.invalidate(&child);
        d.finish(&[parent.clone(), child.clone()]);
        assert_eq!(d.sizes[&parent], 32);
        assert_eq!(d.sizes[&child], 29);
    }

    #[test]
    fn invalidation_expires_overlapping_totals_in_place() {
        let tree = Tree::new();
        tree.file("folder/child/grandchild/data", 11);
        tree.file("folder/sibling/data", 3);
        tree.file("folder/child-2/data", 5);
        let path = tree.0.join("folder");
        let child = path.join("child");
        let mut d = Driver::new(1);
        d.finish(std::slice::from_ref(&path));
        d.service.invalidate(&child);
        let now = Instant::now();
        let cache = d.service.cache.lock().expect("lock poisoned");
        let expired = |p: &Path| cache[p].at + TTL <= now;
        assert!(expired(&path) && expired(&child) && expired(&child.join("grandchild")));
        assert!(!expired(&path.join("sibling")) && !expired(&path.join("child-2")));
        drop(cache);
        // A watched file expires the folders above it.
        d.finish(std::slice::from_ref(&path));
        d.service.changed(&path.join("sibling/data"));
        let now = Instant::now();
        let cache = d.service.cache.lock().expect("lock poisoned");
        let expired = |p: &Path| cache[p].at + TTL <= now;
        assert!(expired(&path) && expired(&path.join("sibling")));
        assert!(!expired(&child));
    }

    #[test]
    fn a_folder_that_keeps_changing_still_gets_totals() {
        let tree = Tree::new();
        tree.file("folder/data", 11);
        let path = tree.0.join("folder");
        let mut d = Driver::new(1);
        d.finish(std::slice::from_ref(&path));
        let gate = Gate::new(&d.service);
        d.service.invalidate(&path);
        d.sync(std::slice::from_ref(&path));
        assert_eq!(
            d.sizes[&path], 11,
            "the old total stays while it is recomputed"
        );
        assert!(d.sizing.is_empty());
        let generation = d.service.generation;
        // Changes during the scan neither cancel nor restart it.
        tree.file("folder/data", 29);
        d.service.invalidate(&path);
        d.sync(std::slice::from_ref(&path));
        assert_eq!(d.service.generation, generation);
        drop(gate);
        d.finish(std::slice::from_ref(&path));
        assert_eq!(d.sizes[&path], 29);
        assert_eq!(d.service.generation, generation, "no rescan right away");
        let at = d.service.cache.lock().expect("lock poisoned")[&path].at;
        assert!(
            at + TTL <= Instant::now() + RETRY,
            "the total is due again soon"
        );
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
        d.sync(std::slice::from_ref(&path));
        assert_eq!(d.sizes[&path], 11, "an expired total stays visible");
        assert!(d.sizing.is_empty(), "a refresh shows no placeholder");
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
            usage: Some(Usage {
                size: 0,
                ..Default::default()
            }),
            device: 0,
            at: Instant::now(),
        };
        for i in 0..=MAX_CACHED {
            let path = tree.0.join(format!("cached{i}"));
            d.service
                .cache
                .lock()
                .expect("lock poisoned")
                .insert(path.clone(), value.clone());
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
            usage: Some(Usage {
                size: 7,
                ..Default::default()
            }),
            device: 0,
            at: Instant::now(),
        };
        for path in &paths {
            d.service
                .cache
                .lock()
                .expect("lock poisoned")
                .insert(path.clone(), value.clone());
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
        assert_eq!(size.map(|u| u.size), Some(16));
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
            fsops::dir_size(&path, dev, &cancel, &|_| None, &|_, _| {}).map(|u| u.size),
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

    #[test]
    fn hard_links_count_once_even_across_cached_subtrees() {
        let tree = Tree::new();
        let x = tree.file("folder/a/x", 10);
        tree.file("folder/c/z", 3);
        fs::create_dir_all(tree.0.join("folder/b")).unwrap();
        fs::hard_link(&x, tree.0.join("folder/b/y")).unwrap();
        fs::hard_link(&x, tree.0.join("folder/a/twin")).unwrap();
        let path = tree.0.join("folder");
        let dev = fsops::device_of(&path).unwrap();
        let cancel = AtomicBool::new(false);
        let walked = Mutex::new(HashMap::new());
        let usage = fsops::dir_size(&path, dev, &cancel, &|_| None, &|p, u| {
            walked
                .lock()
                .expect("lock poisoned")
                .insert(p.to_path_buf(), u.clone());
        })
        .unwrap();
        assert_eq!(usage.size, 13);
        assert!(usage.open.is_empty(), "every link of x is inside folder");
        let walked = walked.into_inner().expect("lock poisoned");
        let a = &walked[&path.join("a")];
        assert_eq!((a.size, a.open.len()), (10, 1), "x has a link outside a");
        assert_eq!(walked[&path.join("b")].size, 10);
        // A parent reusing cached children still counts x once.
        let reused = fsops::dir_size(
            &path,
            dev,
            &cancel,
            &|p| (p != path).then(|| walked[p].clone()),
            &|_, _| {},
        )
        .unwrap();
        assert_eq!(reused.size, 13);
        // A link outside the walk keeps x open at the top.
        fs::hard_link(&x, tree.0.join("outside")).unwrap();
        let usage = fsops::dir_size(&path, dev, &cancel, &|_| None, &|_, _| {}).unwrap();
        assert_eq!(usage.size, 13);
        assert_eq!(usage.open.len(), 1);
        assert_eq!(
            fsops::dir_size(&tree.0, dev, &cancel, &|_| None, &|_, _| {})
                .unwrap()
                .size,
            13
        );
    }

    #[test]
    fn saved_sizes_show_until_recomputed() {
        let tree = Tree::new();
        tree.file("folder/data", 11);
        tree.file("other/data", 5);
        let (path, other) = (tree.0.join("folder"), tree.0.join("other"));
        let file = tree.0.join("cache/folder-sizes");
        crate::session::save_bytes(&file, &encode(&[(&path, 99), (&other, 42)])).unwrap();
        let mut d = Driver::new(1);
        d.service.persist(file.clone());
        let gate = Gate::new(&d.service);
        d.sync(&[path.clone(), other.clone()]);
        assert_eq!(d.sizes[&path], 99, "last session's total shows at once");
        assert!(d.sizing.is_empty());

        drop(gate);
        d.finish(&[path.clone(), other.clone()]);
        assert_eq!((d.sizes[&path], d.sizes[&other]), (11, 5));
        d.service.save();
        let saved = decode(&fs::read(&file).unwrap());
        assert_eq!((saved[&path], saved[&other]), (11, 5));
    }

    #[test]
    fn saved_sizes_decode_what_is_intact() {
        let data = encode(&[(Path::new("/a"), 1), (Path::new("/b"), 2)]);
        assert_eq!(decode(&data).len(), 2);
        assert_eq!(
            decode(&data[..data.len() - 1]),
            HashMap::from([("/a".into(), 1)])
        );
        assert!(decode(b"something else").is_empty());
        assert!(decode(&encode(&[(Path::new("relative"), 1)])).is_empty());
    }
}
