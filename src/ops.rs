//! Background file operations. Jobs run one at a time on a worker thread, in
//! the order they were queued. Nothing is ever overwritten: name clashes get a
//! " (2)" style suffix.

use eframe::egui;
use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JobKind {
    Copy,
    Move,
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
}

impl Job {
    pub fn new(kind: JobKind, srcs: Vec<PathBuf>, dest: Option<PathBuf>) -> Self {
        Self { kind, srcs, dest, pairs: Vec::new(), undoing: false, name: None }
    }

    /// The job that reverses `undo`.
    pub fn undo(undo: Undo) -> Self {
        let job = match undo {
            Undo::MoveBack(pairs) => Self { srcs: pairs.iter().map(|(_, cur)| cur.clone()).collect(), pairs, ..Self::new(JobKind::MoveBack, Vec::new(), None) },
            Undo::TrashCreated(paths) => Self::new(JobKind::Trash, paths, None),
            Undo::Untrash(paths) => Self::new(JobKind::Untrash, paths, None),
        };
        Self { undoing: true, ..job }
    }
}

/// How to reverse a finished operation.
#[derive(Clone, Debug, PartialEq)]
pub enum Undo {
    /// Items that were moved or renamed: (original, current).
    MoveBack(Vec<(PathBuf, PathBuf)>),
    /// Items that were created (copies, new folders): undoing trashes them.
    TrashCreated(Vec<PathBuf>),
    /// Items that were trashed, by original path.
    Untrash(Vec<PathBuf>),
}

impl Undo {
    /// "the rename of \"x\"", "moving 3 items", … for the undo toast.
    pub fn describe(&self) -> String {
        let what = |paths: Vec<&Path>| {
            if paths.len() == 1 { format!("\"{}\"", file_name(paths[0])) } else { format!("{} items", paths.len()) }
        };
        match self {
            Undo::MoveBack(p) => format!("Moved back {}", what(p.iter().map(|(o, _)| o.as_path()).collect())),
            Undo::TrashCreated(p) => format!("Moved {} to trash", what(p.iter().map(PathBuf::as_path).collect())),
            Undo::Untrash(p) => format!("Restored {} from trash", what(p.iter().map(PathBuf::as_path).collect())),
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
}

pub struct Done {
    pub kind: JobKind,
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
        Self { tx, done_rx, progress, cancel, queued }
    }

    pub fn submit(&self, job: Job) {
        if job.srcs.is_empty() {
            return;
        }
        self.queued.fetch_add(1, Ordering::Relaxed);
        let _ = self.tx.send(job);
    }

    pub fn snapshot(&self) -> Option<Progress> {
        self.progress.lock().unwrap().clone()
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
            *self.progress.lock().unwrap() = None;
            let _ = done_tx.send(done);
            self.ctx.request_repaint();
        }
    }

    fn update(&mut self, f: impl FnOnce(&mut Progress)) {
        if let Some(p) = self.progress.lock().unwrap().as_mut() {
            f(p);
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
            JobKind::Trash => format!("Moving {what} to trash"),
            JobKind::Delete => format!("Deleting {what}"),
            JobKind::MoveBack => format!("Moving back {what}"),
            JobKind::Untrash => format!("Restoring {what}"),
            JobKind::Extract => format!("Extracting {what}"),
            JobKind::Compress => format!("Compressing {what}"),
        };
        *self.progress.lock().unwrap() = Some(Progress { title, ..Default::default() });
        self.ctx.request_repaint();

        let mut done = Done {
            kind: job.kind,
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

        match job.kind {
            JobKind::Trash => {
                if let Err(e) = trash::delete_all(&job.srcs) {
                    done.errors.push(format!("Trash failed: {e}"));
                }
                let gone: Vec<PathBuf> = job.srcs.iter().filter(|p| fs::symlink_metadata(p).is_err()).cloned().collect();
                if !gone.is_empty() {
                    done.undo = Some(Undo::Untrash(gone));
                }
                done.summary = format!("Moved {what} to trash");
            }
            JobKind::MoveBack => {
                for (orig, cur) in &job.pairs {
                    self.move_back(orig, cur, &mut done);
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
                    self.extract(src, &mut done);
                }
                if !done.created.is_empty() {
                    done.undo = Some(Undo::TrashCreated(done.created.clone()));
                }
                done.summary = format!("Extracted {what}");
            }
            JobKind::Compress => {
                let dest = job.dest.clone().expect("compress needs a destination");
                let name = job.name.clone().unwrap_or_else(|| "Archive.zip".into());
                self.compress(&job.srcs, &dest, &name, &mut done);
                if !done.created.is_empty() {
                    done.undo = Some(Undo::TrashCreated(done.created.clone()));
                }
                done.summary = format!("Compressed {what} to \"{}\"", done.created.first().map(|p| file_name(p)).unwrap_or(name));
            }
            JobKind::Delete => {
                for src in &job.srcs {
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
                let moved = self.transfer(job.kind, &job.srcs, &dest, &mut done);
                done.undo = match job.kind {
                    JobKind::Move if !moved.is_empty() => Some(Undo::MoveBack(moved)),
                    JobKind::Copy if !done.created.is_empty() => Some(Undo::TrashCreated(done.created.clone())),
                    _ => None,
                };
                let verb = if job.kind == JobKind::Copy { "Copied" } else { "Moved" };
                done.summary = format!("{verb} {what} to {}", file_name(&dest));
            }
        }
        done.cancelled = self.cancelled();
        done
    }

    /// Copies or moves `srcs` into `dest`. Returns (source, new path) for each item moved.
    fn transfer(&mut self, kind: JobKind, srcs: &[PathBuf], dest: &Path, done: &mut Done) -> Vec<(PathBuf, PathBuf)> {
        let mut moved = Vec::new();
        // Same-filesystem moves are renames and need no byte counting.
        let dest_dev = fs::metadata(dest).map(|m| m.dev()).ok();
        let mut need_copy = Vec::new();
        for src in srcs {
            if src.is_dir() && dest.starts_with(src) {
                done.errors.push(format!("Cannot put \"{}\" inside itself", file_name(src)));
                continue;
            }
            if kind == JobKind::Move && src.parent() == Some(dest) {
                continue; // already there
            }
            let same_dev = fs::symlink_metadata(src).map(|m| Some(m.dev()) == dest_dev).unwrap_or(false);
            if kind == JobKind::Move && same_dev {
                let target = unique_dest(dest, &file_name(src));
                match fs::rename(src, &target) {
                    Ok(()) => {
                        moved.push((src.clone(), target.clone()));
                        done.created.push(target);
                    }
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
            p.bytes_total = bytes;
            p.files_total = files;
        });

        for src in &need_copy {
            if self.cancelled() {
                break;
            }
            let target = unique_dest(dest, &file_name(src));
            let errors_before = done.errors.len();
            self.copy_tree(src, &target, &mut done.errors);
            if self.cancelled() {
                break;
            }
            done.created.push(target.clone());
            // A cross-device move removes the source only if every file copied.
            if kind == JobKind::Move && done.errors.len() == errors_before {
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
    fn run_tool(&self, cmd: &mut std::process::Command) -> Result<(), String> {
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
        let status = loop {
            if self.cancelled() {
                let _ = child.kill();
                let _ = child.wait();
                return Err("cancelled".into());
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => std::thread::sleep(Duration::from_millis(30)),
                Err(e) => return Err(e.to_string()),
            }
        };
        let err = reader.join().unwrap_or_default();
        if status.success() {
            Ok(())
        } else {
            Err(err.lines().find(|l| !l.trim().is_empty()).unwrap_or("failed").trim().to_string())
        }
    }

    /// Unpacks `archive` next to itself. A single top-level item comes out as
    /// is; anything else goes into a folder named after the archive.
    fn extract(&mut self, archive: &Path, done: &mut Done) {
        let Some(dir) = archive.parent() else { return };
        let tmp = unique_dest(dir, &format!(".kestrel-extract-{}", file_name(archive)));
        if let Err(e) = fs::create_dir(&tmp) {
            return done.errors.push(format!("{}: {e}", file_name(archive)));
        }
        let res = self.run_tool(std::process::Command::new("bsdtar").arg("-xf").arg(archive).arg("-C").arg(&tmp));
        let items: Vec<PathBuf> = fs::read_dir(&tmp).map(|rd| rd.flatten().map(|d| d.path()).collect()).unwrap_or_default();
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
        let placed = if let [single] = items.as_slice() {
            let target = unique_dest(dir, &file_name(single));
            fs::rename(single, &target).map(|()| {
                let _ = fs::remove_dir(&tmp);
                target
            })
        } else {
            let target = unique_dest(dir, &archive_stem(archive));
            fs::rename(&tmp, &target).map(|()| target)
        };
        match placed {
            Ok(target) => done.created.push(target),
            Err(e) => done.errors.push(format!("{}: {e}", file_name(archive))),
        }
    }

    /// Zips `srcs` into `dest/name`, writing to a temporary file first so a
    /// failed or cancelled run leaves nothing half-written behind.
    fn compress(&mut self, srcs: &[PathBuf], dest: &Path, name: &str, done: &mut Done) {
        let target = unique_dest(dest, name);
        let part = dest.join(format!(".{}.part", file_name(&target)));
        let mut cmd = std::process::Command::new("bsdtar");
        cmd.args(["--format", "zip", "-cf"]).arg(&part);
        for src in srcs {
            let (Some(parent), Some(base)) = (src.parent(), src.file_name()) else { continue };
            // `-C` per item lets search results from different folders go in one zip.
            let mut entry = std::ffi::OsString::new();
            if base.to_string_lossy().starts_with('-') {
                entry.push("./"); // not an option
            }
            entry.push(base);
            cmd.arg("-C").arg(parent).arg(entry);
        }
        match self.run_tool(&mut cmd).and_then(|()| fs::rename(&part, &target).map_err(|e| e.to_string())) {
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
            return done.errors.push(format!("Can't undo: \"{}\" already exists", file_name(orig)));
        }
        if fs::symlink_metadata(cur).is_err() {
            return done.errors.push(format!("Can't undo: \"{}\" is gone", file_name(cur)));
        }
        if !orig.parent().is_some_and(Path::is_dir) {
            return done.errors.push(format!("Can't undo: the folder of \"{}\" is gone", file_name(orig)));
        }
        match fs::rename(cur, orig) {
            Ok(()) => done.created.push(orig.to_path_buf()),
            Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {
                let before = done.errors.len();
                self.copy_tree(cur, orig, &mut done.errors);
                if done.errors.len() == before && !self.cancelled() {
                    let res = if cur.is_dir() && !cur.is_symlink() { fs::remove_dir_all(cur) } else { fs::remove_file(cur) };
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
        if let Err(e) = res {
            if e.kind() != io::ErrorKind::Interrupted {
                errors.push(format!("{}: {e}", src.display()));
            }
        }
    }

    fn copy_file(&mut self, src: &Path, dst: &Path, meta: &fs::Metadata) -> io::Result<()> {
        let mut input = fs::File::open(src)?;
        let mut output = fs::OpenOptions::new().write(true).create_new(true).open(dst)?;
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
                        Some(libc::EXDEV | libc::ENOSYS | libc::EINVAL | libc::EOPNOTSUPP | libc::EPERM) => {
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
    let Ok(meta) = fs::symlink_metadata(path) else { return (0, 0) };
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

fn push_unique(v: &mut Vec<PathBuf>, p: PathBuf) {
    if !v.contains(&p) {
        v.push(p);
    }
}

const ARCHIVE_EXTS: &[&str] = &[
    ".tar.gz", ".tar.xz", ".tar.bz2", ".tar.zst", ".tar.lz4", ".tar.lzma", ".tar", ".tgz", ".txz", ".tbz", ".tbz2", ".tzst",
    ".zip", ".7z", ".rar", ".jar", ".cpio", ".iso", ".cab", ".xar", ".deb",
];

/// Whether bsdtar can unpack this file, judging by its name.
pub fn is_archive(path: &Path) -> bool {
    let name = file_name(path).to_lowercase();
    ARCHIVE_EXTS.iter().any(|ext| name.len() > ext.len() && name.ends_with(ext))
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

/// The zip name for compressing `srcs`: the item's name for one item, else "Archive.zip".
pub fn zip_name(srcs: &[PathBuf]) -> String {
    match srcs {
        [one] if one.is_dir() => format!("{}.zip", file_name(one)),
        [one] => format!("{}.zip", one.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| file_name(one))),
        _ => "Archive.zip".into(),
    }
}

/// Restores each of `paths` (original locations) from the trash, taking the
/// most recently trashed item when the trash holds several from one path.
fn untrash(paths: &[PathBuf], done: &mut Done) {
    let items = match trash::os_limited::list() {
        Ok(items) => items,
        Err(e) => return done.errors.push(format!("Can't read the trash: {e}")),
    };
    let mut restore = Vec::new();
    for p in paths {
        if fs::symlink_metadata(p).is_ok() {
            done.errors.push(format!("Can't undo: \"{}\" already exists", file_name(p)));
            continue;
        }
        match items.iter().filter(|i| i.original_path() == *p).max_by_key(|i| i.time_deleted) {
            Some(item) => restore.push(item.clone()),
            None => done.errors.push(format!("\"{}\" is no longer in the trash", file_name(p))),
        }
    }
    let restored: Vec<PathBuf> = restore.iter().map(|i| i.original_path()).collect();
    if let Err(e) = trash::os_limited::restore_all(restore) {
        done.errors.push(format!("Restore failed: {e}"));
    }
    done.created.extend(restored.into_iter().filter(|p| fs::symlink_metadata(p).is_ok()));
}

#[cfg(test)]
mod tests {
    use super::*;

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

        let done = run(Job::new(JobKind::Copy, vec![src.clone()], Some(dst.clone())));
        assert!(done.errors.is_empty(), "{:?}", done.errors);
        assert_eq!(fs::read_to_string(dst.join("src/sub/f.txt")).unwrap(), "hello");
        assert!(dst.join("src/link").is_symlink());

        // Copying again must not overwrite.
        let done = run(Job::new(JobKind::Copy, vec![src.clone()], Some(dst.clone())));
        assert_eq!(done.created, vec![dst.join("src (2)")]);

        // Into itself is refused.
        let done = run(Job::new(JobKind::Copy, vec![src.clone()], Some(src.join("sub"))));
        assert_eq!(done.errors.len(), 1);

        let done = run(Job::new(JobKind::Move, vec![src.clone()], Some(dst.clone())));
        assert!(done.errors.is_empty());
        assert!(!src.exists());
        assert!(dst.join("src (3)/sub/f.txt").exists());
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn undo_records_and_reverses() {
        let d = tmpdir("undo");
        let (a, b) = (d.join("a"), d.join("b"));
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(a.join("f.txt"), "hi").unwrap();

        let done = run(Job::new(JobKind::Copy, vec![a.join("f.txt")], Some(b.clone())));
        assert_eq!(done.undo, Some(Undo::TrashCreated(vec![b.join("f.txt")])));

        let done = run(Job::new(JobKind::Move, vec![a.join("f.txt")], Some(b.clone())));
        let undo = done.undo.unwrap();
        assert_eq!(undo, Undo::MoveBack(vec![(a.join("f.txt"), b.join("f (2).txt"))]));

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
        assert_eq!(fs::read_to_string(d.join("proj (2)/src/main.rs")).unwrap(), "fn main() {}");

        // Loose files go into a folder named after the archive.
        let mut job = Job::new(JobKind::Compress, vec![d.join("-dash.txt"), d.join("b.txt")], Some(d.clone()));
        job.name = Some(zip_name(&job.srcs));
        assert!(run(job).errors.is_empty());
        let done = run(Job::new(JobKind::Extract, vec![d.join("Archive.zip")], None));
        assert_eq!(done.created, vec![d.join("Archive")]);
        assert!(d.join("Archive/-dash.txt").exists() && d.join("Archive/b.txt").exists());
        assert!(fs::read_dir(&d).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().starts_with(".kestrel")));

        // A broken archive reports an error and leaves no temp folder.
        fs::write(d.join("bad.zip"), "not a zip").unwrap();
        let done = run(Job::new(JobKind::Extract, vec![d.join("bad.zip")], None));
        assert_eq!(done.errors.len(), 1);
        assert!(fs::read_dir(&d).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().starts_with(".kestrel")));
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn archive_names() {
        assert!(is_archive(Path::new("/x/Photos.TAR.GZ")));
        assert!(!is_archive(Path::new("/x/.zip")));
        assert_eq!(archive_stem(Path::new("/x/photos.tar.gz")), "photos");
        assert_eq!(zip_name(&[PathBuf::from("/nonexistent/report.pdf")]), "report.zip");
    }
}
