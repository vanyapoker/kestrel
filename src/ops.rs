//! Background file operations. Jobs run one at a time on a worker thread, in
//! the order they were queued. Nothing is overwritten silently: name clashes get a
//! " (2)" style suffix, unless the user chose to replace (the old item goes to the trash).

use eframe::egui;
use std::collections::{HashSet, VecDeque};
use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JobKind {
    Copy,
    Move,
    Rename,
    Trash,
    Delete,
    /// Undo: move each item in `pairs` from its current path back to its original one.
    MoveBack,
    /// Undo: restore `srcs` (original paths) from the trash.
    Untrash,
    /// Unpack each archive in `srcs` next to itself.
    Extract,
    /// Zip `srcs` into `dest`, named `name`.
    Compress,
    /// Save image bytes received from another application.
    SaveImage,
    /// Put items in the trash folder back where they came from.
    Restore,
    /// Permanently delete everything in the trash.
    EmptyTrash,
    /// Save the clipboard's image (MIME type in `Job::name`) into `dest` as a new file.
    PasteImage,
    /// Change permissions and/or ownership of `srcs` as described by `Job::change`.
    Properties,
}

/// Archive format for `Compress`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    Zip,
    TarGz,
    TarXz,
    TarZst,
    TarBz2,
    SevenZip,
}

impl Format {
    pub const ALL: [Format; 6] = [
        Format::Zip,
        Format::TarGz,
        Format::TarXz,
        Format::TarZst,
        Format::TarBz2,
        Format::SevenZip,
    ];

    pub fn ext(self) -> &'static str {
        match self {
            Format::Zip => ".zip",
            Format::TarGz => ".tar.gz",
            Format::TarXz => ".tar.xz",
            Format::TarZst => ".tar.zst",
            Format::TarBz2 => ".tar.bz2",
            Format::SevenZip => ".7z",
        }
    }

    /// bsdtar arguments selecting this format at `level`.
    fn args(self, level: Level) -> Vec<String> {
        // (fast, normal, best) in each codec's own scale.
        let pick = |lv: (u8, u8, u8)| match level {
            Level::Store | Level::Fast => lv.0,
            Level::Normal => lv.1,
            Level::Best => lv.2,
        };
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        match self {
            Format::Zip if level == Level::Store => {
                a(&["--format", "zip", "--options", "zip:compression=store"])
            }
            Format::Zip => a(&[
                "--format",
                "zip",
                "--options",
                &format!("zip:compression-level={}", pick((1, 6, 9))),
            ]),
            Format::TarGz => a(&[
                "--gzip",
                "--options",
                &format!("gzip:compression-level={}", pick((1, 6, 9))),
            ]),
            Format::TarXz => a(&[
                "--xz",
                "--options",
                &format!("xz:compression-level={}", pick((1, 6, 9))),
            ]),
            Format::TarZst => a(&[
                "--zstd",
                "--options",
                &format!("zstd:compression-level={}", pick((3, 10, 19))),
            ]),
            Format::TarBz2 => a(&[
                "--bzip2",
                "--options",
                &format!("bzip2:compression-level={}", pick((1, 6, 9))),
            ]),
            Format::SevenZip => a(&[
                "--format",
                "7zip",
                "--options",
                &format!("7zip:compression-level={}", pick((1, 6, 9))),
            ]),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Format::Zip => "zip",
            Format::TarGz => "tar.gz",
            Format::TarXz => "tar.xz",
            Format::TarZst => "tar.zst",
            Format::TarBz2 => "tar.bz2",
            Format::SevenZip => "7z",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    /// No compression (zip only).
    Store,
    Fast,
    Normal,
    Best,
}

/// Where `Extract` puts its output.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub enum ExtractTo {
    /// Next to the archive; a lone top-level item comes out as is.
    #[default]
    Here,
    /// Next to the archive, always inside a folder named after it.
    Folder,
    /// Into another folder (`Job::dest`), like `Here`.
    Dir,
}

pub struct Job {
    pub kind: JobKind,
    pub srcs: Vec<PathBuf>,
    /// Destination folder for copy/move.
    pub dest: Option<PathBuf>,
    /// (original, current) paths for `MoveBack`.
    pub pairs: Vec<(PathBuf, PathBuf)>,
    /// This job reverses an earlier one, so it isn't itself undoable.
    pub undoing: bool,
    /// File name for `Compress`.
    pub name: Option<String>,
    pub format: Format,
    pub level: Level,
    pub extract_to: ExtractTo,
    pub image: Option<(String, Vec<u8>)>,
    /// Sources of a copy/move whose same-named item in `dest` is replaced (trashed first).
    pub replace: HashSet<PathBuf>,
    pub change: Option<crate::props::Change>,
    /// Trash entries to put back once the job is done (undoing a replace).
    pub restore: Vec<(PathBuf, std::ffi::OsString)>,
}

impl Job {
    pub fn new(kind: JobKind, srcs: Vec<PathBuf>, dest: Option<PathBuf>) -> Self {
        Self {
            kind,
            srcs,
            dest,
            pairs: Vec::new(),
            undoing: false,
            name: None,
            format: Format::Zip,
            level: Level::Normal,
            extract_to: ExtractTo::Here,
            image: None,
            replace: HashSet::new(),
            change: None,
            restore: Vec::new(),
        }
    }

    /// The job that reverses `undo`.
    pub fn undo(undo: Undo) -> Self {
        let job = match undo {
            Undo::RenameBack(pairs) => Self {
                srcs: pairs.iter().map(|(_, to)| to.clone()).collect(),
                pairs: pairs.into_iter().map(|(from, to)| (to, from)).collect(),
                ..Self::new(JobKind::Rename, Vec::new(), None)
            },
            Undo::MoveBack(pairs) => Self {
                srcs: pairs.iter().map(|(_, cur)| cur.clone()).collect(),
                pairs,
                ..Self::new(JobKind::MoveBack, Vec::new(), None)
            },
            Undo::TrashCreated(paths) => Self::new(JobKind::Trash, paths, None),
            Undo::Untrash(paths) => Self::new(JobKind::Untrash, paths, None),
            Undo::Replaced(inner, restore) => Self {
                restore,
                ..Self::undo(*inner)
            },
        };
        Self {
            undoing: true,
            ..job
        }
    }
}

/// How to reverse a finished operation.
#[derive(Clone, Debug, PartialEq)]
pub enum Undo {
    /// A group of renames, reversed together so swaps and cycles remain valid.
    RenameBack(Vec<(PathBuf, PathBuf)>),
    /// Items that were moved or renamed: (original, current).
    MoveBack(Vec<(PathBuf, PathBuf)>),
    /// Items that were created (copies, new folders): undoing trashes them.
    TrashCreated(Vec<PathBuf>),
    /// Items that were trashed, by original path.
    Untrash(Vec<PathBuf>),
    /// Undo the first, then bring back the items it replaced from the trash.
    Replaced(Box<Undo>, Vec<(PathBuf, std::ffi::OsString)>),
}

impl Undo {
    /// "the rename of \"x\"", "moving 3 items", … for the undo toast.
    pub fn describe(&self) -> String {
        let what = |paths: Vec<&Path>| {
            if paths.len() == 1 {
                format!("\"{}\"", file_name(paths[0]))
            } else {
                format!("{} items", paths.len())
            }
        };
        match self {
            Undo::RenameBack(p) => format!(
                "Renamed {}",
                what(p.iter().map(|(o, _)| o.as_path()).collect())
            ),
            Undo::MoveBack(p) => format!(
                "Moved back {}",
                what(p.iter().map(|(o, _)| o.as_path()).collect())
            ),
            Undo::TrashCreated(p) => format!(
                "Moved {} to trash",
                what(p.iter().map(PathBuf::as_path).collect())
            ),
            Undo::Replaced(inner, _) => inner.describe(),
            Undo::Untrash(p) => format!(
                "Restored {} from trash",
                what(p.iter().map(PathBuf::as_path).collect())
            ),
        }
    }
}

#[derive(Clone, Default)]
pub struct Progress {
    pub title: String,
    pub bytes_total: u64,
    pub bytes_done: u64,
    pub files_total: u64,
    pub files_done: u64,
    pub current: String,
    rate_samples: VecDeque<(Instant, u64)>,
}

#[derive(Clone, Copy, Debug)]
pub struct Throughput {
    pub bytes_per_second: f64,
    pub remaining: Option<Duration>,
}

impl Progress {
    fn start_bytes(&mut self, total: u64, now: Instant) {
        self.bytes_total = total;
        self.bytes_done = 0;
        self.rate_samples.clear();
        self.rate_samples.push_back((now, 0));
    }

    fn record(&mut self, now: Instant) {
        let Some(&(last, bytes)) = self.rate_samples.back() else {
            return;
        };
        if self.bytes_done < bytes {
            self.start_bytes(self.bytes_total, now);
            return;
        }
        if now.saturating_duration_since(last) >= Duration::from_millis(250) {
            self.rate_samples.push_back((now, self.bytes_done));
        }
        // Retain a baseline just before the window, including during a stall.
        while self.rate_samples.len() > 2
            && now.saturating_duration_since(self.rate_samples[1].0) >= Duration::from_secs(3)
        {
            self.rate_samples.pop_front();
        }
    }

    /// Recent processing speed and estimated time to consume the remaining input.
    /// No estimate is available while starting, stalled, or finishing the operation.
    pub fn throughput(&self, now: Instant) -> Option<Throughput> {
        let &(start, bytes) = self.rate_samples.front()?;
        let elapsed = now.saturating_duration_since(start).as_secs_f64();
        if elapsed < 0.5 || self.bytes_total == 0 {
            return None;
        }
        let last = self.rate_samples.back()?.0;
        let bytes_per_second = if now.saturating_duration_since(last) > Duration::from_secs(3) {
            0.0
        } else {
            self.bytes_done.saturating_sub(bytes) as f64 / elapsed
        };
        let remaining = self.bytes_total.saturating_sub(self.bytes_done);
        let remaining = if bytes_per_second > 0.0 && remaining > 0 {
            Duration::try_from_secs_f64(remaining as f64 / bytes_per_second).ok()
        } else {
            None
        };
        Some(Throughput {
            bytes_per_second,
            remaining,
        })
    }
}

pub struct Done {
    pub kind: JobKind,
    /// Items a transfer replaced: (their path, id of the trash entry holding them).
    pub replaced: Vec<(PathBuf, std::ffi::OsString)>,
    /// Folders whose contents changed.
    pub dirs: Vec<PathBuf>,
    /// Newly created top-level items, so the UI can select them.
    pub created: Vec<PathBuf>,
    pub errors: Vec<String>,
    pub cancelled: bool,
    pub summary: String,
    /// How to reverse what this job did, if anything.
    pub undo: Option<Undo>,
    pub undoing: bool,
}

pub struct Ops {
    tx: Sender<Job>,
    pub done_rx: Receiver<Done>,
    pub progress: Arc<Mutex<Option<Progress>>>,
    pub cancel: Arc<AtomicBool>,
    pub queued: Arc<AtomicUsize>,
}

impl Ops {
    pub fn new(ctx: egui::Context) -> Self {
        let (tx, rx) = channel::<Job>();
        let (done_tx, done_rx) = channel();
        let progress = Arc::new(Mutex::new(None));
        let cancel = Arc::new(AtomicBool::new(false));
        let queued = Arc::new(AtomicUsize::new(0));
        let worker = Worker {
            ctx,
            progress: progress.clone(),
            cancel: cancel.clone(),
            last_repaint: Instant::now(),
        };
        let q = queued.clone();
        std::thread::Builder::new()
            .name("file-ops".into())
            .spawn(move || worker.run(rx, done_tx, q))
            .expect("spawn file-ops thread");
        Self {
            tx,
            done_rx,
            progress,
            cancel,
            queued,
        }
    }

    pub fn submit(&self, job: Job) {
        if job.srcs.is_empty()
            && job.image.is_none()
            && !matches!(job.kind, JobKind::EmptyTrash | JobKind::PasteImage)
        {
            return;
        }
        self.queued.fetch_add(1, Ordering::Relaxed);
        let _ = self.tx.send(job);
    }

    pub fn snapshot(&self) -> Option<Progress> {
        self.progress.lock().expect("lock poisoned").clone()
    }
}

struct Worker {
    ctx: egui::Context,
    progress: Arc<Mutex<Option<Progress>>>,
    cancel: Arc<AtomicBool>,
    last_repaint: Instant,
}

impl Worker {
    fn run(mut self, rx: Receiver<Job>, done_tx: Sender<Done>, queued: Arc<AtomicUsize>) {
        while let Ok(job) = rx.recv() {
            self.cancel.store(false, Ordering::Relaxed);
            let done = self.execute(job);
            queued.fetch_sub(1, Ordering::Relaxed);
            *self.progress.lock().expect("lock poisoned") = None;
            let _ = done_tx.send(done);
            self.ctx.request_repaint();
        }
    }

    fn update(&mut self, f: impl FnOnce(&mut Progress)) {
        if let Some(p) = self.progress.lock().expect("lock poisoned").as_mut() {
            f(p);
            p.record(Instant::now());
        }
        if self.last_repaint.elapsed() > Duration::from_millis(50) {
            self.last_repaint = Instant::now();
            self.ctx.request_repaint();
        }
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn execute(&mut self, job: Job) -> Done {
        let n = job.srcs.len();
        let what = if n == 1 {
            format!("\"{}\"", file_name(&job.srcs[0]))
        } else {
            format!("{n} items")
        };
        let title = match job.kind {
            JobKind::Copy => format!("Copying {what}"),
            JobKind::Move => format!("Moving {what}"),
            JobKind::Rename => format!("Renaming {what}"),
            JobKind::Trash => format!("Moving {what} to trash"),
            JobKind::Delete => format!("Deleting {what}"),
            JobKind::MoveBack => format!("Moving back {what}"),
            JobKind::Untrash => format!("Restoring {what}"),
            JobKind::Extract => format!("Extracting {what}"),
            JobKind::Compress => format!("Compressing {what}"),
            JobKind::SaveImage => "Saving dropped image".into(),
            JobKind::Restore => format!("Restoring {what}"),
            JobKind::EmptyTrash => "Emptying the trash".into(),
            JobKind::PasteImage => "Pasting image".into(),
            JobKind::Properties => format!("Changing properties of {what}"),
        };
        *self.progress.lock().expect("lock poisoned") = Some(Progress {
            title,
            ..Default::default()
        });
        self.ctx.request_repaint();

        let mut done = Done {
            kind: job.kind,
            replaced: Vec::new(),
            dirs: Vec::new(),
            created: Vec::new(),
            errors: Vec::new(),
            cancelled: false,
            summary: String::new(),
            undo: None,
            undoing: job.undoing,
        };
        for src in &job.srcs {
            if let Some(parent) = src.parent() {
                push_unique(&mut done.dirs, parent.to_path_buf());
            }
        }
        if let Some(dest) = &job.dest {
            push_unique(&mut done.dirs, dest.clone());
        }
        for (orig, _) in &job.pairs {
            if let Some(parent) = orig.parent() {
                push_unique(&mut done.dirs, parent.to_path_buf());
            }
        }

        // Set when an undo has already put back the items a replace trashed.
        let mut restored = false;
        match job.kind {
            JobKind::SaveImage => {
                if let (Some(dest), Some((name, bytes))) = (job.dest.as_deref(), job.image) {
                    match self.save_image(dest, &name, &bytes) {
                        Ok(path) => {
                            done.created.push(path);
                            done.undo = Some(Undo::TrashCreated(done.created.clone()));
                        }
                        Err(e) => done
                            .errors
                            .push(format!("Could not save dropped image: {e}")),
                    }
                }
                done.summary = "Saved dropped image".into();
            }
            JobKind::Properties => {
                if let Some(change) = &job.change {
                    for src in &job.srcs {
                        if self.cancelled() {
                            break;
                        }
                        self.update(|p| p.current = file_name(src));
                        done.errors.extend(crate::props::apply(src, change));
                    }
                }
                done.summary = format!("Updated properties of {what}");
            }
            JobKind::PasteImage => {
                if let (Some(dest), Some(mime)) = (job.dest.as_deref(), job.name.as_deref()) {
                    match self.paste_image(dest, mime) {
                        Ok(path) => {
                            done.created.push(path);
                            done.undo = Some(Undo::TrashCreated(done.created.clone()));
                        }
                        Err(e) => done.errors.push(format!("Could not paste image: {e}")),
                    }
                }
                done.summary = "Pasted image".into();
            }
            JobKind::Rename => {
                if self.cancelled() {
                    done.cancelled = true;
                } else {
                    match crate::rename::apply(&job.pairs) {
                        Ok(()) => {
                            done.created = job.pairs.iter().map(|(_, to)| to.clone()).collect();
                            if !job.undoing {
                                done.undo = Some(Undo::RenameBack(job.pairs));
                            }
                        }
                        Err(error) => done.errors.push(format!("Rename failed: {error}")),
                    }
                }
                done.summary = format!("Renamed {what}");
            }
            JobKind::Trash => {
                if let Err(e) = trash::delete_all(&job.srcs) {
                    done.errors.push(format!("Trash failed: {e}"));
                }
                let gone: Vec<PathBuf> = job
                    .srcs
                    .iter()
                    .filter(|p| fs::symlink_metadata(p).is_err())
                    .cloned()
                    .collect();
                if !gone.is_empty() {
                    done.undo = Some(Undo::Untrash(gone));
                }
                done.summary = format!("Moved {what} to trash");
            }
            JobKind::MoveBack => {
                // An item may belong inside the one it replaced (moving "foo/foo" up
                // over "foo"). Park it, bring the replaced item back, then finish.
                let mut parked = Vec::new();
                for (orig, cur) in &job.pairs {
                    if !job.restore.iter().any(|(r, _)| orig.starts_with(r)) {
                        self.move_back(orig, cur, &mut done);
                        continue;
                    }
                    let dir = cur.parent().unwrap_or(Path::new("/"));
                    let temp = unique_dest(dir, &format!(".{}.undoing", file_name(cur)));
                    match fs::rename(cur, &temp) {
                        Ok(()) => parked.push((orig, cur, temp)),
                        Err(e) => done.errors.push(format!("{}: {e}", cur.display())),
                    }
                }
                if !parked.is_empty() {
                    if done.errors.is_empty() {
                        restore_by_id(&job.restore, &mut done);
                        restored = true;
                    }
                    for (orig, cur, temp) in parked {
                        if done.errors.is_empty() {
                            self.move_back(orig, &temp, &mut done);
                        } else if let Err(e) = fs::rename(&temp, cur) {
                            done.errors.push(format!(
                                "{e}: \"{}\" was left at {}",
                                file_name(cur),
                                temp.display()
                            ));
                        }
                    }
                }
                done.summary = format!("Moved back {what}");
            }
            JobKind::Untrash => {
                untrash(&job.srcs, &mut done);
                done.summary = format!("Restored {what}");
            }
            JobKind::Extract => {
                for src in &job.srcs {
                    if self.cancelled() {
                        break;
                    }
                    self.update(|p| p.current = file_name(src));
                    self.extract(
                        src,
                        job.dest.as_deref(),
                        job.extract_to == ExtractTo::Folder,
                        &mut done,
                    );
                }
                if !done.created.is_empty() {
                    done.undo = Some(Undo::TrashCreated(done.created.clone()));
                }
                done.summary = format!("Extracted {what}");
            }
            JobKind::Compress => {
                let dest = job.dest.clone().expect("compress needs a destination");
                let name = job
                    .name
                    .clone()
                    .unwrap_or_else(|| format!("Archive{}", job.format.ext()));
                self.compress(&job.srcs, &dest, &name, job.format, job.level, &mut done);
                if !done.created.is_empty() {
                    done.undo = Some(Undo::TrashCreated(done.created.clone()));
                }
                done.summary = format!(
                    "Compressed {what} to \"{}\"",
                    done.created.first().map(|p| file_name(p)).unwrap_or(name)
                );
            }
            JobKind::Restore => {
                restore_from_trash(&job.srcs, &mut done);
                if !done.created.is_empty() {
                    done.undo = Some(Undo::TrashCreated(done.created.clone()));
                }
                done.summary = format!("Restored {what}");
            }
            JobKind::EmptyTrash => {
                match trash::os_limited::list().and_then(trash::os_limited::purge_all) {
                    Ok(()) => {}
                    Err(e) => done.errors.push(format!("Could not empty the trash: {e}")),
                }
                done.dirs.push(crate::fsops::trash_dir());
                done.summary = "Emptied the trash".into();
            }
            JobKind::Delete => {
                let (trashed, plain): (Vec<_>, Vec<_>) =
                    job.srcs.iter().partition(|p| crate::fsops::in_trash(p));
                purge_from_trash(&trashed, &mut done);
                for src in plain {
                    let res = match fs::symlink_metadata(src) {
                        Ok(m) if m.is_dir() => fs::remove_dir_all(src),
                        Ok(_) => fs::remove_file(src),
                        Err(e) => Err(e),
                    };
                    if let Err(e) = res {
                        done.errors.push(format!("{}: {e}", src.display()));
                    }
                }
                done.summary = format!("Deleted {what}");
            }
            JobKind::Copy | JobKind::Move => {
                let dest = job.dest.clone().expect("copy/move needs a destination");
                let moved = self.transfer(job.kind, &job.srcs, &dest, &job.replace, &mut done);
                done.undo = match job.kind {
                    JobKind::Move if !moved.is_empty() => Some(Undo::MoveBack(moved)),
                    JobKind::Copy if !done.created.is_empty() => {
                        Some(Undo::TrashCreated(done.created.clone()))
                    }
                    _ => None,
                };
                if !done.replaced.is_empty() {
                    done.undo = done
                        .undo
                        .take()
                        .map(|u| Undo::Replaced(Box::new(u), done.replaced.clone()));
                }
                let verb = if job.kind == JobKind::Copy {
                    "Copied"
                } else {
                    "Moved"
                };
                done.summary = format!("{verb} {what} to {}", file_name(&dest));
            }
        }
        if !restored && !job.restore.is_empty() && done.errors.is_empty() {
            restore_by_id(&job.restore, &mut done);
        }
        done.cancelled = self.cancelled();
        done
    }

    /// Saves the clipboard's image as `Screenshot-<date>.<ext>`.
    fn paste_image(&mut self, dest: &Path, mime: &str) -> io::Result<PathBuf> {
        let ext = image_ext(mime).ok_or_else(|| io::Error::other(format!("unsupported {mime}")))?;
        let mut cmd = std::process::Command::new("wl-paste");
        cmd.args(["--no-newline", "--type", mime]);
        let bytes = crate::thumbs::run_tool_for(&mut cmd, Duration::from_secs(10))
            .map_err(io::Error::other)?;
        let name = format!(
            "Screenshot-{}.{ext}",
            chrono::Local::now().format("%Y-%m-%d_%H-%M-%S")
        );
        self.save_image(dest, &name, &bytes)
    }

    /// Saves a browser's encoded image without overwriting existing files.
    fn save_image(&mut self, dest: &Path, name: &str, bytes: &[u8]) -> io::Result<PathBuf> {
        // Validate the payload before creating a file. Keep the source's encoded bytes.
        image::load_from_memory(bytes).map_err(io::Error::other)?;
        let (path, mut file) = loop {
            let path = unique_dest(dest, name);
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => break (path, file),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        };
        self.update(|p| p.start_bytes(bytes.len() as u64, Instant::now()));
        let result = (|| {
            for chunk in bytes.chunks(64 * 1024) {
                if self.cancelled() {
                    return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
                }
                file.write_all(chunk)?;
                self.update(|p| p.bytes_done += chunk.len() as u64);
            }
            file.flush()
        })();
        if let Err(e) = result {
            drop(file);
            let _ = fs::remove_file(&path);
            return Err(e);
        }
        Ok(path)
    }

    /// Copies or moves `srcs` into `dest`. Returns (source, new path) for each item moved.
    fn transfer(
        &mut self,
        kind: JobKind,
        srcs: &[PathBuf],
        dest: &Path,
        replace: &HashSet<PathBuf>,
        done: &mut Done,
    ) -> Vec<(PathBuf, PathBuf)> {
        let mut moved = Vec::new();
        // Same-filesystem moves are renames and need no byte counting.
        let dest_dev = fs::metadata(dest).map(|m| m.dev()).ok();
        let mut need_copy = Vec::new();
        for src in srcs {
            if src.is_dir() && dest.starts_with(src) {
                done.errors
                    .push(format!("Cannot put \"{}\" inside itself", file_name(src)));
                continue;
            }
            if kind == JobKind::Move && src.parent() == Some(dest) {
                continue; // already there
            }
            let same_dev = fs::symlink_metadata(src)
                .map(|m| Some(m.dev()) == dest_dev)
                .unwrap_or(false);
            if kind == JobKind::Move && same_dev {
                let landing = Landing::new(dest, src, replace);
                match fs::rename(src, &landing.work) {
                    Ok(()) if landing.finish(done) => {
                        moved.push((src.clone(), landing.target.clone()));
                        done.created.push(landing.target);
                    }
                    Ok(()) => {}
                    Err(e) => done.errors.push(format!("{}: {e}", src.display())),
                }
            } else {
                need_copy.push(src.clone());
            }
        }
        if need_copy.is_empty() {
            return moved;
        }

        let (mut bytes, mut files) = (0, 0);
        for src in &need_copy {
            let (b, f) = measure(src);
            bytes += b;
            files += f;
        }
        self.update(|p| {
            p.start_bytes(bytes, Instant::now());
            p.files_total = files;
        });

        for src in &need_copy {
            if self.cancelled() {
                break;
            }
            let landing = Landing::new(dest, src, replace);
            let errors_before = done.errors.len();
            self.copy_tree(src, &landing.work, &mut done.errors);
            if self.cancelled() || done.errors.len() != errors_before {
                landing.discard();
                if self.cancelled() {
                    break;
                }
                continue;
            }
            if !landing.finish(done) {
                continue;
            }
            let target = landing.target;
            done.created.push(target.clone());
            // A cross-device move removes the source only if every file copied. When
            // the source sat inside the item it replaced, it's already in the trash.
            if kind == JobKind::Move && fs::symlink_metadata(src).is_ok() {
                let res = if src.is_dir() && !src.is_symlink() {
                    fs::remove_dir_all(src)
                } else {
                    fs::remove_file(src)
                };
                match res {
                    Ok(()) => moved.push((src.clone(), target.clone())),
                    Err(e) => done.errors.push(format!("{}: {e}", src.display())),
                }
            }
        }
        moved
    }

    /// Runs an archive tool, killing it if the job is cancelled. Errors carry its stderr.
    /// `total` is the number of input bytes the tool is expected to read; its progress is
    /// taken from the bytes it has read so far (/proc/<pid>/io).
    fn run_tool(&mut self, cmd: &mut std::process::Command, total: u64) -> Result<(), String> {
        use std::process::Stdio;
        let mut child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("bsdtar: {e}"))?;
        // Drain stderr on a thread so a chatty tool can't block on a full pipe.
        let mut stderr = child.stderr.take();
        let reader = std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(e) = stderr.as_mut() {
                let _ = e.read_to_string(&mut s);
            }
            s
        });
        let io = format!("/proc/{}/io", child.id());
        self.update(|p| {
            p.start_bytes(total, Instant::now());
        });
        let status = loop {
            if self.cancelled() {
                let _ = child.kill();
                let _ = child.wait();
                return Err("cancelled".into());
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    if total > 0
                        && let Some(read) = read_chars(&io)
                    {
                        self.update(|p| p.bytes_done = read.min(total));
                    }
                    std::thread::sleep(Duration::from_millis(30));
                }
                Err(e) => return Err(e.to_string()),
            }
        };
        let err = reader.join().unwrap_or_default();
        if status.success() {
            Ok(())
        } else {
            Err(err
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("failed")
                .trim()
                .to_string())
        }
    }

    /// Unpacks `archive` into `out` (default: next to itself). A single top-level item comes
    /// out as is, unless `into_folder`; anything else goes into a folder named after the archive.
    fn extract(&mut self, archive: &Path, out: Option<&Path>, into_folder: bool, done: &mut Done) {
        let Some(dir) = out.or_else(|| archive.parent()) else {
            return;
        };
        let tmp = unique_dest(dir, &format!(".kestrel-extract-{}", file_name(archive)));
        if let Err(e) = fs::create_dir(&tmp) {
            return done.errors.push(format!("{}: {e}", file_name(archive)));
        }
        let total = fs::metadata(archive).map(|m| m.len()).unwrap_or(0);
        let res = self.run_tool(
            std::process::Command::new("bsdtar")
                .arg("-xf")
                .arg(archive)
                .arg("-C")
                .arg(&tmp),
            total,
        );
        let items: Vec<PathBuf> = fs::read_dir(&tmp)
            .map(|rd| rd.flatten().map(|d| d.path()).collect())
            .unwrap_or_default();
        if let Err(e) = &res {
            if e != "cancelled" {
                done.errors.push(format!("{}: {e}", file_name(archive)));
            }
            // Keep whatever came out of a partly readable archive, but not a cancelled one.
            if items.is_empty() || e == "cancelled" {
                let _ = fs::remove_dir_all(&tmp);
                return;
            }
        }
        let placed = match items.as_slice() {
            [single] if !into_folder => {
                let target = unique_dest(dir, &file_name(single));
                fs::rename(single, &target).map(|()| {
                    let _ = fs::remove_dir(&tmp);
                    target
                })
            }
            _ => {
                let target = unique_dest(dir, &archive_stem(archive));
                fs::rename(&tmp, &target).map(|()| target)
            }
        };
        match placed {
            Ok(target) => done.created.push(target),
            Err(e) => done.errors.push(format!("{}: {e}", file_name(archive))),
        }
    }

    /// Archives `srcs` into `dest/name`, writing to a temporary file first so a
    /// failed or cancelled run leaves nothing half-written behind.
    fn compress(
        &mut self,
        srcs: &[PathBuf],
        dest: &Path,
        name: &str,
        format: Format,
        level: Level,
        done: &mut Done,
    ) {
        let target = unique_dest(dest, name);
        let part = dest.join(format!(".{}.part", file_name(&target)));
        let mut cmd = std::process::Command::new("bsdtar");
        cmd.args(format.args(level)).arg("-cf").arg(&part);
        for src in srcs {
            let (Some(parent), Some(base)) = (src.parent(), src.file_name()) else {
                continue;
            };
            // `-C` per item lets search results from different folders go in one archive.
            let mut entry = std::ffi::OsString::new();
            if base.to_string_lossy().starts_with('-') {
                entry.push("./"); // not an option
            }
            entry.push(base);
            cmd.arg("-C").arg(parent).arg(entry);
        }
        let total: u64 = srcs.iter().map(|s| tree_size(s)).sum();
        match self
            .run_tool(&mut cmd, total)
            .and_then(|()| fs::rename(&part, &target).map_err(|e| e.to_string()))
        {
            Ok(()) => done.created.push(target),
            Err(e) => {
                let _ = fs::remove_file(&part);
                if e != "cancelled" {
                    done.errors.push(format!("Compress failed: {e}"));
                }
            }
        }
    }

    /// Moves `cur` back to exactly `orig`, never replacing something already there.
    fn move_back(&mut self, orig: &Path, cur: &Path, done: &mut Done) {
        if fs::symlink_metadata(orig).is_ok() {
            return done.errors.push(format!(
                "Can't undo: \"{}\" already exists",
                file_name(orig)
            ));
        }
        if fs::symlink_metadata(cur).is_err() {
            return done
                .errors
                .push(format!("Can't undo: \"{}\" is gone", file_name(cur)));
        }
        if !orig.parent().is_some_and(Path::is_dir) {
            return done.errors.push(format!(
                "Can't undo: the folder of \"{}\" is gone",
                file_name(orig)
            ));
        }
        match fs::rename(cur, orig) {
            Ok(()) => done.created.push(orig.to_path_buf()),
            Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {
                let before = done.errors.len();
                self.copy_tree(cur, orig, &mut done.errors);
                if done.errors.len() == before && !self.cancelled() {
                    let res = if cur.is_dir() && !cur.is_symlink() {
                        fs::remove_dir_all(cur)
                    } else {
                        fs::remove_file(cur)
                    };
                    if let Err(e) = res {
                        done.errors.push(format!("{}: {e}", cur.display()));
                    }
                    done.created.push(orig.to_path_buf());
                }
            }
            Err(e) => done.errors.push(format!("{}: {e}", cur.display())),
        }
    }

    fn copy_tree(&mut self, src: &Path, dst: &Path, errors: &mut Vec<String>) {
        if self.cancelled() {
            return;
        }
        let meta = match fs::symlink_metadata(src) {
            Ok(m) => m,
            Err(e) => return errors.push(format!("{}: {e}", src.display())),
        };
        let res = if meta.file_type().is_symlink() {
            fs::read_link(src).and_then(|t| std::os::unix::fs::symlink(t, dst))
        } else if meta.is_dir() {
            match fs::create_dir(dst) {
                Ok(()) => {
                    match fs::read_dir(src) {
                        Ok(rd) => {
                            for de in rd.flatten() {
                                self.copy_tree(&de.path(), &dst.join(de.file_name()), errors);
                            }
                        }
                        Err(e) => errors.push(format!("{}: {e}", src.display())),
                    }
                    fs::set_permissions(dst, meta.permissions())
                }
                Err(e) => Err(e),
            }
        } else {
            let name = file_name(src);
            self.update(|p| p.current = name);
            let r = self.copy_file(src, dst, &meta);
            self.update(|p| p.files_done += 1);
            r
        };
        if let Err(e) = res
            && e.kind() != io::ErrorKind::Interrupted
        {
            errors.push(format!("{}: {e}", src.display()));
        }
    }

    fn copy_file(&mut self, src: &Path, dst: &Path, meta: &fs::Metadata) -> io::Result<()> {
        if crate::util::is_gvfs(src) {
            return self.copy_file_gvfs(src, dst);
        }
        let mut input = fs::File::open(src)?;
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dst)?;
        let result = self.copy_contents(&mut input, &mut output);
        if result.is_err() {
            drop(output);
            let _ = fs::remove_file(dst);
            return result;
        }
        output.set_permissions(meta.permissions())?;
        if let Ok(m) = meta.modified() {
            let _ = output.set_modified(m);
        }
        Ok(())
    }

    /// Reads through gio instead of the gvfs-fuse mount: the mount serves a camera file
    /// about ten times slower. Bytes show up as the destination grows.
    fn copy_file_gvfs(&mut self, src: &Path, dst: &Path) -> io::Result<()> {
        let mut child = Command::new("gio")
            .arg("copy")
            .arg(src)
            .arg(dst)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut seen = 0;
        let status = loop {
            if self.cancelled() {
                let _ = child.kill();
                let _ = child.wait();
                let _ = fs::remove_file(dst);
                return Err(io::ErrorKind::Interrupted.into());
            }
            let status = child.try_wait()?;
            let len = fs::metadata(dst).map(|m| m.len()).unwrap_or(seen);
            self.update(|p| p.bytes_done += len.saturating_sub(seen));
            seen = seen.max(len);
            if let Some(status) = status {
                break status;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        if status.success() {
            return Ok(());
        }
        let mut err = String::new();
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_string(&mut err);
        }
        let _ = fs::remove_file(dst);
        let line = err.lines().next().unwrap_or("gio copy failed");
        Err(io::Error::other(
            line.strip_prefix("gio: ").unwrap_or(line).to_string(),
        ))
    }

    /// Copies with `copy_file_range` (kernel side, reflinks on btrfs/xfs), falling
    /// back to a userspace loop when unsupported. Chunked for progress and cancel.
    fn copy_contents(&mut self, input: &mut fs::File, output: &mut fs::File) -> io::Result<()> {
        const CHUNK: usize = 16 * 1024 * 1024;
        let mut kernel = true;
        let mut buf = Vec::new();
        loop {
            if self.cancelled() {
                return Err(io::ErrorKind::Interrupted.into());
            }
            let n = if kernel {
                // SAFETY: both fds are open for the duration of the call and the null
                // offset pointers make the kernel use and advance the file offsets.
                let r = unsafe {
                    libc::copy_file_range(
                        input.as_raw_fd(),
                        std::ptr::null_mut(),
                        output.as_raw_fd(),
                        std::ptr::null_mut(),
                        CHUNK,
                        0,
                    )
                };
                if r < 0 {
                    let err = io::Error::last_os_error();
                    match err.raw_os_error() {
                        Some(
                            libc::EXDEV
                            | libc::ENOSYS
                            | libc::EINVAL
                            | libc::EOPNOTSUPP
                            | libc::EPERM,
                        ) => {
                            kernel = false;
                            continue;
                        }
                        _ => return Err(err),
                    }
                }
                r as usize
            } else {
                if buf.is_empty() {
                    buf = vec![0u8; 1024 * 1024];
                }
                let n = input.read(&mut buf)?;
                output.write_all(&buf[..n])?;
                n
            };
            if n == 0 {
                return Ok(());
            }
            self.update(|p| p.bytes_done += n as u64);
        }
    }
}

/// Total bytes and file count under `path` (not following symlinks).
fn measure(path: &Path) -> (u64, u64) {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return (0, 0);
    };
    if !meta.is_dir() {
        return (meta.len(), 1);
    }
    let mut total = (0, 0);
    if let Ok(rd) = fs::read_dir(path) {
        for de in rd.flatten() {
            let (b, f) = measure(&de.path());
            total.0 += b;
            total.1 += f;
        }
    }
    total
}

/// Clipboard image types we can save, with their file extensions.
pub const CLIPBOARD_IMAGES: [(&str, &str); 5] = [
    ("image/png", "png"),
    ("image/jpeg", "jpg"),
    ("image/webp", "webp"),
    ("image/bmp", "bmp"),
    ("image/gif", "gif"),
];

fn image_ext(mime: &str) -> Option<&'static str> {
    CLIPBOARD_IMAGES
        .iter()
        .find(|(m, _)| *m == mime)
        .map(|(_, e)| *e)
}

pub fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

/// `dir/name`, or `dir/name (2).ext`, `dir/name (3).ext`… if taken.
pub fn unique_dest(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if fs::symlink_metadata(&first).is_err() {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| fs::symlink_metadata(p).is_err())
        .expect("some free name")
}

/// Where a transferred item lands. When replacing, the item is first put under a
/// temporary name, so the old item can go to the trash even if the new one came from
/// inside it (cutting `foo/foo` into the folder holding `foo`).
struct Landing {
    target: PathBuf,
    /// Where the data is written; differs from `target` only when replacing.
    work: PathBuf,
}

impl Landing {
    fn new(dest: &Path, src: &Path, replace: &HashSet<PathBuf>) -> Self {
        let name = file_name(src);
        let target = dest.join(&name);
        if replace.contains(src) && target != src && fs::symlink_metadata(&target).is_ok() {
            let work = unique_dest(dest, &format!(".{name}.replacing"));
            return Self { target, work };
        }
        let target = unique_dest(dest, &name);
        Self {
            work: target.clone(),
            target,
        }
    }

    fn replacing(&self) -> bool {
        self.work != self.target
    }

    /// Swaps the finished `work` item in for the old `target`, which goes to the trash.
    /// Returns whether `target` now holds the new item.
    fn finish(&self, done: &mut Done) -> bool {
        if !self.replacing() {
            return true;
        }
        let name = file_name(&self.target);
        if let Err(e) = trash::delete(&self.target) {
            done.errors
                .push(format!("Could not replace \"{name}\": {e}"));
            self.discard();
            return false;
        }
        // Remember the trashed item so undo can bring it back.
        let id = trash::os_limited::list().ok().and_then(|items| {
            items
                .into_iter()
                .filter(|i| i.original_path() == self.target)
                .max_by_key(|i| i.time_deleted)
                .map(|i| i.id)
        });
        if let Some(id) = id {
            done.replaced.push((self.target.clone(), id));
        }
        if let Err(e) = fs::rename(&self.work, &self.target) {
            done.errors.push(format!(
                "Could not finish replacing \"{name}\": {e} (the new item is at {})",
                self.work.display()
            ));
            return false;
        }
        true
    }

    /// Removes the temporary item after a failure.
    fn discard(&self) {
        if self.replacing() {
            let _ = match fs::symlink_metadata(&self.work) {
                Ok(m) if m.is_dir() => fs::remove_dir_all(&self.work),
                _ => fs::remove_file(&self.work),
            };
        }
    }
}

fn push_unique(v: &mut Vec<PathBuf>, p: PathBuf) {
    if !v.contains(&p) {
        v.push(p);
    }
}

const ARCHIVE_EXTS: &[&str] = &[
    ".tar.gz",
    ".tar.xz",
    ".tar.bz2",
    ".tar.zst",
    ".tar.lz4",
    ".tar.lzma",
    ".tar",
    ".tgz",
    ".txz",
    ".tbz",
    ".tbz2",
    ".tzst",
    ".zip",
    ".7z",
    ".rar",
    ".jar",
    ".cpio",
    ".iso",
    ".cab",
    ".xar",
    ".deb",
];

/// Whether bsdtar can unpack this file, judging by its name.
pub fn is_archive(path: &Path) -> bool {
    let name = file_name(path).to_lowercase();
    ARCHIVE_EXTS
        .iter()
        .any(|ext| name.len() > ext.len() && name.ends_with(ext))
}

/// Bytes read so far by a process, from its /proc io file.
fn read_chars(path: &str) -> Option<u64> {
    fs::read_to_string(path)
        .ok()?
        .lines()
        .find_map(|l| l.strip_prefix("rchar: ")?.trim().parse().ok())
}

/// Total size of the files under `path` (not following symlinks).
fn tree_size(path: &Path) -> u64 {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => fs::read_dir(path)
            .map(|rd| rd.flatten().map(|d| tree_size(&d.path())).sum())
            .unwrap_or(0),
        Ok(m) => m.len(),
        Err(_) => 0,
    }
}

/// "photos.tar.gz" → "photos".
fn archive_stem(path: &Path) -> String {
    let name = file_name(path);
    let lower = name.to_lowercase();
    ARCHIVE_EXTS
        .iter()
        .find(|ext| lower.len() > ext.len() && lower.ends_with(*ext))
        .map(|ext| name[..name.len() - ext.len()].to_string())
        .unwrap_or(name)
}

/// The archive name without extension for compressing `srcs`: the item's name for one item, else "Archive".
pub fn archive_base_name(srcs: &[PathBuf]) -> String {
    match srcs {
        [one] if one.is_dir() => file_name(one),
        [one] => one
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| file_name(one)),
        _ => "Archive".into(),
    }
}

pub fn zip_name(srcs: &[PathBuf]) -> String {
    format!("{}.zip", archive_base_name(srcs))
}

/// Restores each of `paths` (original locations) from the trash, taking the
/// most recently trashed item when the trash holds several from one path.
/// The trash entries for items inside the trash folder (the entry is named after its info file).
fn trash_entries(paths: &[&PathBuf]) -> Result<Vec<trash::TrashItem>, String> {
    let info = crate::fsops::trash_dir().with_file_name("info");
    let ids: Vec<PathBuf> = paths
        .iter()
        .map(|p| info.join(format!("{}.trashinfo", file_name(p))))
        .collect();
    let items = trash::os_limited::list().map_err(|e| format!("Can't read the trash: {e}"))?;
    Ok(items
        .into_iter()
        .filter(|i| ids.iter().any(|id| id.as_os_str() == i.id))
        .collect())
}

/// Puts back the trash entries with these ids (see `Done::replaced`).
fn restore_by_id(wanted: &[(PathBuf, std::ffi::OsString)], done: &mut Done) {
    let items = match trash::os_limited::list() {
        Ok(items) => items,
        Err(e) => return done.errors.push(format!("Can't read the trash: {e}")),
    };
    let mut restore = Vec::new();
    for (path, id) in wanted {
        match items.iter().find(|i| i.id == *id) {
            Some(item) if fs::symlink_metadata(path).is_err() => restore.push(item.clone()),
            Some(_) => done.errors.push(format!(
                "Can't restore: \"{}\" already exists",
                file_name(path)
            )),
            None => done
                .errors
                .push(format!("\"{}\" is no longer in the trash", file_name(path))),
        }
    }
    let restored: Vec<PathBuf> = restore.iter().map(|i| i.original_path()).collect();
    if let Err(e) = trash::os_limited::restore_all(restore) {
        return done.errors.push(format!("Restore failed: {e}"));
    }
    done.created.extend(restored);
}

fn restore_from_trash(paths: &[PathBuf], done: &mut Done) {
    let paths: Vec<&PathBuf> = paths.iter().collect();
    let items = match trash_entries(&paths) {
        Ok(items) => items,
        Err(e) => return done.errors.push(e),
    };
    let mut restore = Vec::new();
    for item in items {
        let original = item.original_path();
        if fs::symlink_metadata(&original).is_ok() {
            done.errors.push(format!(
                "Can't restore: \"{}\" already exists in {}",
                file_name(&original),
                item.original_parent.display()
            ));
        } else {
            restore.push(item);
        }
    }
    let restored: Vec<PathBuf> = restore.iter().map(|i| i.original_path()).collect();
    if let Err(e) = trash::os_limited::restore_all(restore) {
        done.errors.push(format!("Restore failed: {e}"));
        return;
    }
    for p in restored {
        if let Some(parent) = p.parent() {
            push_unique(&mut done.dirs, parent.to_path_buf());
        }
        done.created.push(p);
    }
}

fn purge_from_trash(paths: &[&PathBuf], done: &mut Done) {
    if paths.is_empty() {
        return;
    }
    match trash_entries(paths).and_then(|items| {
        trash::os_limited::purge_all(items).map_err(|e| format!("Could not delete: {e}"))
    }) {
        Ok(()) => {}
        Err(e) => done.errors.push(e),
    }
}

fn untrash(paths: &[PathBuf], done: &mut Done) {
    let items = match trash::os_limited::list() {
        Ok(items) => items,
        Err(e) => return done.errors.push(format!("Can't read the trash: {e}")),
    };
    let mut restore = Vec::new();
    for p in paths {
        if fs::symlink_metadata(p).is_ok() {
            done.errors
                .push(format!("Can't undo: \"{}\" already exists", file_name(p)));
            continue;
        }
        match items
            .iter()
            .filter(|i| i.original_path() == *p)
            .max_by_key(|i| i.time_deleted)
        {
            Some(item) => restore.push(item.clone()),
            None => done
                .errors
                .push(format!("\"{}\" is no longer in the trash", file_name(p))),
        }
    }
    let restored: Vec<PathBuf> = restore.iter().map(|i| i.original_path()).collect();
    if let Err(e) = trash::os_limited::restore_all(restore) {
        done.errors.push(format!("Restore failed: {e}"));
    }
    done.created.extend(
        restored
            .into_iter()
            .filter(|p| fs::symlink_metadata(p).is_ok()),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throughput_tracks_recent_speed_and_eta_without_startup_spikes() {
        let start = Instant::now();
        let mut p = Progress::default();
        assert!(p.throughput(start).is_none());
        p.start_bytes(10_000, start);
        p.bytes_done = 100;
        p.record(start + Duration::from_millis(100));
        assert!(p.throughput(start + Duration::from_millis(100)).is_none());
        for second in 1..=10 {
            p.bytes_done = second * 100;
            p.record(start + Duration::from_secs(second));
        }
        let rate = p.throughput(start + Duration::from_secs(10)).unwrap();
        assert_eq!(rate.bytes_per_second, 100.0);
        assert_eq!(rate.remaining, Some(Duration::from_secs(90)));
        // Faster work replaces the earlier samples instead of averaging the whole job.
        for second in 11..=14 {
            p.bytes_done += 1000;
            p.record(start + Duration::from_secs(second));
        }
        let rate = p.throughput(start + Duration::from_secs(14)).unwrap();
        assert_eq!(rate.bytes_per_second, 1000.0);
        assert_eq!(rate.remaining, Some(Duration::from_secs(5)));
        assert!(p.rate_samples.len() <= 5, "sample history stays bounded");
    }

    #[test]
    fn throughput_handles_stalls_completion_and_new_archive_inputs() {
        let start = Instant::now();
        let mut p = Progress::default();
        p.start_bytes(1000, start);
        p.bytes_done = 500;
        p.record(start + Duration::from_secs(1));
        let stalled = p.throughput(start + Duration::from_secs(5)).unwrap();
        assert_eq!(stalled.bytes_per_second, 0.0);
        assert!(
            stalled.remaining.is_none(),
            "blocked IO has no reliable ETA"
        );
        for second in 2..=5 {
            p.record(start + Duration::from_secs(second));
        }
        assert_eq!(
            p.throughput(start + Duration::from_secs(5))
                .unwrap()
                .bytes_per_second,
            0.0
        );
        p.bytes_done = 1000;
        p.record(start + Duration::from_secs(6));
        assert!(
            p.throughput(start + Duration::from_secs(6))
                .unwrap()
                .remaining
                .is_none(),
            "consuming all input still leaves archive finalization"
        );
        p.start_bytes(2000, start + Duration::from_secs(10));
        assert_eq!(p.bytes_done, 0);
        assert!(
            p.throughput(start + Duration::from_millis(10_100))
                .is_none()
        );
        p.bytes_done = 200;
        p.record(start + Duration::from_secs(11));
        assert_eq!(
            p.throughput(start + Duration::from_secs(11))
                .unwrap()
                .remaining,
            Some(Duration::from_secs(9))
        );
    }

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("kestrel-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn run(job: Job) -> Done {
        let ops = Ops::new(egui::Context::default());
        ops.submit(job);
        ops.done_rx.recv_timeout(Duration::from_secs(10)).unwrap()
    }

    #[test]
    fn dropped_image_preserves_bytes_handles_collisions_and_supports_undo() {
        let dir = tmpdir("dropped-image");
        let mut encoded = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(2, 2)
            .write_to(&mut encoded, image::ImageFormat::Png)
            .unwrap();
        let bytes = encoded.into_inner();
        fs::write(dir.join("image.png"), b"existing image").unwrap();
        let mut job = Job::new(JobKind::SaveImage, Vec::new(), Some(dir.clone()));
        job.image = Some(("image.png".into(), bytes.clone()));
        let done = run(job);
        let saved = dir.join("image (2).png");
        assert!(done.errors.is_empty(), "{:?}", done.errors);
        assert_eq!(fs::read(dir.join("image.png")).unwrap(), b"existing image");
        assert_eq!(fs::read(&saved).unwrap(), bytes);
        assert_eq!(done.created, vec![saved.clone()]);
        assert_eq!(done.dirs, vec![dir.clone()]);
        assert_eq!(done.undo, Some(Undo::TrashCreated(vec![saved])));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn invalid_dropped_image_reports_error_without_creating_file() {
        let dir = tmpdir("invalid-dropped-image");
        let mut job = Job::new(JobKind::SaveImage, Vec::new(), Some(dir.clone()));
        job.image = Some(("image.png".into(), b"not an image".to_vec()));
        let done = run(job);
        assert_eq!(done.errors.len(), 1);
        assert!(done.created.is_empty());
        assert!(done.undo.is_none());
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn every_format_and_level_round_trips() {
        let d = tmpdir("formats");
        fs::create_dir_all(d.join("proj")).unwrap();
        fs::write(d.join("proj/a.txt"), "hello ".repeat(1000)).unwrap();
        for format in Format::ALL {
            for level in [Level::Store, Level::Fast, Level::Normal, Level::Best] {
                if level == Level::Store && format != Format::Zip {
                    continue;
                }
                let mut job = Job::new(JobKind::Compress, vec![d.join("proj")], Some(d.clone()));
                job.name = Some(format!("out-{}-{level:?}{}", format.label(), format.ext()));
                (job.format, job.level) = (format, level);
                let done = run(job);
                assert!(
                    done.errors.is_empty(),
                    "{format:?} {level:?}: {:?}",
                    done.errors
                );
                let archive = done.created[0].clone();
                assert!(is_archive(&archive), "{archive:?}");
                // Extract into a folder named after the archive, whatever the layout.
                let mut job = Job::new(JobKind::Extract, vec![archive.clone()], None);
                job.extract_to = ExtractTo::Folder;
                let done = run(job);
                assert!(done.errors.is_empty(), "{format:?}: {:?}", done.errors);
                let out = done.created[0].clone();
                assert_eq!(out, d.join(archive_stem(&archive)));
                assert_eq!(
                    fs::read_to_string(out.join("proj/a.txt")).unwrap().len(),
                    6000
                );
            }
        }
        // Extracting into another folder.
        fs::create_dir(d.join("elsewhere")).unwrap();
        let mut job = Job::new(
            JobKind::Extract,
            vec![d.join("out-zip-Normal.zip")],
            Some(d.join("elsewhere")),
        );
        job.extract_to = ExtractTo::Dir;
        let done = run(job);
        assert_eq!(done.created, vec![d.join("elsewhere/proj")]);
        assert!(done.dirs.contains(&d.join("elsewhere")));
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn archive_jobs_report_progress() {
        let d = tmpdir("progress");
        fs::write(d.join("big.bin"), vec![7u8; 40 << 20]).unwrap();
        let ops = Ops::new(egui::Context::default());
        let mut job = Job::new(JobKind::Compress, vec![d.join("big.bin")], Some(d.clone()));
        job.name = Some("big.tar.xz".into());
        job.format = Format::TarXz;
        job.level = Level::Best;
        ops.submit(job);
        let mut max_done = 0;
        let mut total = 0;
        let start = Instant::now();
        while ops.done_rx.try_recv().is_err() && start.elapsed() < Duration::from_secs(30) {
            if let Some(p) = ops.snapshot() {
                total = total.max(p.bytes_total);
                max_done = max_done.max(p.bytes_done);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(total, 40 << 20);
        assert!(max_done > 0, "no progress reported");
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn unique_names() {
        let d = tmpdir("unique");
        fs::write(d.join("a.txt"), "x").unwrap();
        fs::write(d.join("a (2).txt"), "x").unwrap();
        assert_eq!(unique_dest(&d, "a.txt"), d.join("a (3).txt"));
        assert_eq!(unique_dest(&d, "b"), d.join("b"));
        fs::create_dir(d.join(".cfg")).unwrap();
        assert_eq!(unique_dest(&d, ".cfg"), d.join(".cfg (2)"));
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn copy_and_move_tree() {
        let d = tmpdir("copy");
        let src = d.join("src");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("sub/f.txt"), "hello").unwrap();
        std::os::unix::fs::symlink("sub/f.txt", src.join("link")).unwrap();
        let dst = d.join("dst");
        fs::create_dir(&dst).unwrap();

        let done = run(Job::new(
            JobKind::Copy,
            vec![src.clone()],
            Some(dst.clone()),
        ));
        assert!(done.errors.is_empty(), "{:?}", done.errors);
        assert_eq!(
            fs::read_to_string(dst.join("src/sub/f.txt")).unwrap(),
            "hello"
        );
        assert!(dst.join("src/link").is_symlink());

        // Copying again must not overwrite.
        let done = run(Job::new(
            JobKind::Copy,
            vec![src.clone()],
            Some(dst.clone()),
        ));
        assert_eq!(done.created, vec![dst.join("src (2)")]);

        // Into itself is refused.
        let done = run(Job::new(
            JobKind::Copy,
            vec![src.clone()],
            Some(src.join("sub")),
        ));
        assert_eq!(done.errors.len(), 1);

        let done = run(Job::new(
            JobKind::Move,
            vec![src.clone()],
            Some(dst.clone()),
        ));
        assert!(done.errors.is_empty());
        assert!(!src.exists());
        assert!(dst.join("src (3)/sub/f.txt").exists());
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn replace_trashes_the_existing_item_and_keeps_the_name() {
        let d = tmpdir("replace");
        let (a, b) = (d.join("a"), d.join("b"));
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("f.txt"), "new").unwrap();
        fs::write(a.join("g.txt"), "new g").unwrap();
        fs::write(b.join("f.txt"), "old").unwrap();
        fs::write(b.join("g.txt"), "old g").unwrap();

        let mut job = Job::new(
            JobKind::Copy,
            vec![a.join("f.txt"), a.join("g.txt")],
            Some(b.clone()),
        );
        job.replace.insert(a.join("f.txt"));
        let done = run(job);
        assert!(done.errors.is_empty(), "{:?}", done.errors);
        assert_eq!(fs::read_to_string(b.join("f.txt")).unwrap(), "new");
        // Not marked for replacement, so it keeps both.
        assert_eq!(fs::read_to_string(b.join("g.txt")).unwrap(), "old g");
        assert_eq!(fs::read_to_string(b.join("g (2).txt")).unwrap(), "new g");
        fs::remove_dir_all(d).unwrap();
    }

    /// A scratch folder on the home filesystem, so trashing works like in real use.
    fn home_tmp(name: &str) -> PathBuf {
        let d =
            crate::fsops::home_dir().join(format!(".cache/kestrel-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn replacing_a_folder_with_its_own_child_collapses_it() {
        let d = home_tmp("collapse");
        fs::create_dir_all(d.join("foo/foo")).unwrap();
        fs::write(d.join("foo/foo/inner.txt"), "inner").unwrap();
        fs::write(d.join("foo/outer.txt"), "outer").unwrap();
        let mut job = Job::new(JobKind::Move, vec![d.join("foo/foo")], Some(d.clone()));
        job.replace.insert(d.join("foo/foo"));
        let done = run(job);
        assert!(done.errors.is_empty(), "{:?}", done.errors);
        assert_eq!(
            fs::read_to_string(d.join("foo/inner.txt")).unwrap(),
            "inner"
        );
        assert!(!d.join("foo/outer.txt").exists());
        assert!(!d.join("foo/foo").exists());

        // Undo has to bring "foo" back before "foo/foo" can return inside it.
        let done = run(Job::undo(done.undo.expect("undoable")));
        assert!(done.errors.is_empty(), "{:?}", done.errors);
        assert_eq!(
            fs::read_to_string(d.join("foo/outer.txt")).unwrap(),
            "outer"
        );
        assert_eq!(
            fs::read_to_string(d.join("foo/foo/inner.txt")).unwrap(),
            "inner"
        );
        let names: Vec<_> = fs::read_dir(&d)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(names, ["foo"], "no temporary items are left behind");
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn undoing_a_replacing_move_puts_both_items_back() {
        let d = home_tmp("undo-move-replace");
        let (a, b) = (d.join("a"), d.join("b"));
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("f.txt"), "new").unwrap();
        fs::write(b.join("f.txt"), "old").unwrap();
        let mut job = Job::new(JobKind::Move, vec![a.join("f.txt")], Some(b.clone()));
        job.replace.insert(a.join("f.txt"));
        let done = run(job);
        assert_eq!(fs::read_to_string(b.join("f.txt")).unwrap(), "new");
        let done = run(Job::undo(done.undo.expect("undoable")));
        assert!(done.errors.is_empty(), "{:?}", done.errors);
        assert_eq!(fs::read_to_string(a.join("f.txt")).unwrap(), "new");
        assert_eq!(fs::read_to_string(b.join("f.txt")).unwrap(), "old");
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn undoing_a_replace_brings_back_the_old_item() {
        let d = home_tmp("undo-replace");
        let (a, b) = (d.join("a"), d.join("b"));
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("f.txt"), "new").unwrap();
        fs::write(b.join("f.txt"), "old").unwrap();
        let mut job = Job::new(JobKind::Copy, vec![a.join("f.txt")], Some(b.clone()));
        job.replace.insert(a.join("f.txt"));
        let done = run(job);
        assert_eq!(fs::read_to_string(b.join("f.txt")).unwrap(), "new");
        let undo = done.undo.expect("undoable");
        let done = run(Job::undo(undo));
        assert!(done.errors.is_empty(), "{:?}", done.errors);
        assert_eq!(fs::read_to_string(b.join("f.txt")).unwrap(), "old");
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn trashed_items_restore_and_purge_by_their_trash_path() {
        // Must share a filesystem with the home trash, which /tmp usually doesn't.
        let d = crate::fsops::home_dir()
            .join(format!(".cache/kestrel-trash-test-{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        let in_trash = |orig: &Path| {
            let item = trash::os_limited::list()
                .unwrap()
                .into_iter()
                .filter(|i| i.original_path() == orig)
                .max_by_key(|i| i.time_deleted)
                .expect("in trash");
            let id = PathBuf::from(&item.id);
            crate::fsops::trash_dir().join(id.file_stem().unwrap())
        };
        let (a, b) = (d.join("a.txt"), d.join("b.txt"));
        fs::write(&a, "a").unwrap();
        fs::write(&b, "b").unwrap();
        run(Job::new(JobKind::Trash, vec![a.clone(), b.clone()], None));
        assert!(!a.exists() && !b.exists());

        let (ta, tb) = (in_trash(&a), in_trash(&b));
        let done = run(Job::new(JobKind::Restore, vec![ta], None));
        assert!(done.errors.is_empty(), "{:?}", done.errors);
        assert_eq!(fs::read_to_string(&a).unwrap(), "a");
        assert_eq!(done.undo, Some(Undo::TrashCreated(vec![a.clone()])));

        let done = run(Job::new(JobKind::Delete, vec![tb.clone()], None));
        assert!(done.errors.is_empty(), "{:?}", done.errors);
        assert!(!tb.exists());
        assert!(
            trash::os_limited::list()
                .unwrap()
                .iter()
                .all(|i| i.original_path() != b)
        );
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn undo_records_and_reverses() {
        let d = tmpdir("undo");
        let (a, b) = (d.join("a"), d.join("b"));
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("f.txt"), "hi").unwrap();

        let done = run(Job::new(
            JobKind::Copy,
            vec![a.join("f.txt")],
            Some(b.clone()),
        ));
        assert_eq!(done.undo, Some(Undo::TrashCreated(vec![b.join("f.txt")])));

        let done = run(Job::new(
            JobKind::Move,
            vec![a.join("f.txt")],
            Some(b.clone()),
        ));
        let undo = done.undo.unwrap();
        assert_eq!(
            undo,
            Undo::MoveBack(vec![(a.join("f.txt"), b.join("f (2).txt"))])
        );

        let done = run(Job::undo(undo.clone()));
        assert!(done.undoing && done.undo.is_none());
        assert!(done.errors.is_empty(), "{:?}", done.errors);
        assert_eq!(fs::read_to_string(a.join("f.txt")).unwrap(), "hi");
        assert!(!b.join("f (2).txt").exists());

        // Undo never replaces: put the file back where the original now lives.
        fs::write(b.join("f (2).txt"), "new").unwrap();
        let done = run(Job::undo(undo));
        assert_eq!(done.errors.len(), 1);
        assert_eq!(fs::read_to_string(a.join("f.txt")).unwrap(), "hi");
        assert!(b.join("f (2).txt").exists());
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn compress_and_extract() {
        let d = tmpdir("zip");
        fs::create_dir_all(d.join("proj/src")).unwrap();
        fs::write(d.join("proj/src/main.rs"), "fn main() {}").unwrap();
        fs::write(d.join("-dash.txt"), "x").unwrap();
        fs::write(d.join("b.txt"), "b").unwrap();

        let mut job = Job::new(JobKind::Compress, vec![d.join("proj")], Some(d.clone()));
        job.name = Some(zip_name(&job.srcs));
        let done = run(job);
        assert!(done.errors.is_empty(), "{:?}", done.errors);
        assert_eq!(done.created, vec![d.join("proj.zip")]);
        assert!(!d.join(".proj.zip.part").exists());

        // One top-level folder comes out as itself (renamed, never merged).
        let done = run(Job::new(JobKind::Extract, vec![d.join("proj.zip")], None));
        assert!(done.errors.is_empty(), "{:?}", done.errors);
        assert_eq!(done.created, vec![d.join("proj (2)")]);
        assert_eq!(
            fs::read_to_string(d.join("proj (2)/src/main.rs")).unwrap(),
            "fn main() {}"
        );

        // Loose files go into a folder named after the archive.
        let mut job = Job::new(
            JobKind::Compress,
            vec![d.join("-dash.txt"), d.join("b.txt")],
            Some(d.clone()),
        );
        job.name = Some(zip_name(&job.srcs));
        assert!(run(job).errors.is_empty());
        let done = run(Job::new(
            JobKind::Extract,
            vec![d.join("Archive.zip")],
            None,
        ));
        assert_eq!(done.created, vec![d.join("Archive")]);
        assert!(d.join("Archive/-dash.txt").exists() && d.join("Archive/b.txt").exists());
        assert!(
            fs::read_dir(&d)
                .unwrap()
                .flatten()
                .all(|e| !e.file_name().to_string_lossy().starts_with(".kestrel"))
        );

        // A broken archive reports an error and leaves no temp folder.
        fs::write(d.join("bad.zip"), "not a zip").unwrap();
        let done = run(Job::new(JobKind::Extract, vec![d.join("bad.zip")], None));
        assert_eq!(done.errors.len(), 1);
        assert!(
            fs::read_dir(&d)
                .unwrap()
                .flatten()
                .all(|e| !e.file_name().to_string_lossy().starts_with(".kestrel"))
        );
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn archive_names() {
        assert!(is_archive(Path::new("/x/Photos.TAR.GZ")));
        assert!(!is_archive(Path::new("/x/.zip")));
        assert_eq!(archive_stem(Path::new("/x/photos.tar.gz")), "photos");
        assert_eq!(
            zip_name(&[PathBuf::from("/nonexistent/report.pdf")]),
            "report.zip"
        );
    }
}
