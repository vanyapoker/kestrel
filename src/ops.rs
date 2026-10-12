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
}

pub struct Job {
    pub kind: JobKind,
    pub srcs: Vec<PathBuf>,
    /// Destination folder for copy/move.
    pub dest: Option<PathBuf>,
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
        };
        for src in &job.srcs {
            if let Some(parent) = src.parent() {
                push_unique(&mut done.dirs, parent.to_path_buf());
            }
        }
        if let Some(dest) = &job.dest {
            push_unique(&mut done.dirs, dest.clone());
        }

        match job.kind {
            JobKind::Trash => {
                if let Err(e) = trash::delete_all(&job.srcs) {
                    done.errors.push(format!("Trash failed: {e}"));
                }
                done.summary = format!("Moved {what} to trash");
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
                self.transfer(job.kind, &job.srcs, &dest, &mut done);
                let verb = if job.kind == JobKind::Copy { "Copied" } else { "Moved" };
                done.summary = format!("{verb} {what} to {}", file_name(&dest));
            }
        }
        done.cancelled = self.cancelled();
        done
    }

    fn transfer(&mut self, kind: JobKind, srcs: &[PathBuf], dest: &Path, done: &mut Done) {
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
                    Ok(()) => done.created.push(target),
                    Err(e) => done.errors.push(format!("{}: {e}", src.display())),
                }
            } else {
                need_copy.push(src.clone());
            }
        }
        if need_copy.is_empty() {
            return;
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
            done.created.push(target);
            // A cross-device move removes the source only if every file copied.
            if kind == JobKind::Move && done.errors.len() == errors_before {
                let res = if src.is_dir() && !src.is_symlink() {
                    fs::remove_dir_all(src)
                } else {
                    fs::remove_file(src)
                };
                if let Err(e) = res {
                    done.errors.push(format!("{}: {e}", src.display()));
                }
            }
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

        let done = run(Job { kind: JobKind::Copy, srcs: vec![src.clone()], dest: Some(dst.clone()) });
        assert!(done.errors.is_empty(), "{:?}", done.errors);
        assert_eq!(fs::read_to_string(dst.join("src/sub/f.txt")).unwrap(), "hello");
        assert!(dst.join("src/link").is_symlink());

        // Copying again must not overwrite.
        let done = run(Job { kind: JobKind::Copy, srcs: vec![src.clone()], dest: Some(dst.clone()) });
        assert_eq!(done.created, vec![dst.join("src (2)")]);

        // Into itself is refused.
        let done = run(Job { kind: JobKind::Copy, srcs: vec![src.clone()], dest: Some(src.join("sub")) });
        assert_eq!(done.errors.len(), 1);

        let done = run(Job { kind: JobKind::Move, srcs: vec![src.clone()], dest: Some(dst.clone()) });
        assert!(done.errors.is_empty());
        assert!(!src.exists());
        assert!(dst.join("src (3)/sub/f.txt").exists());
        fs::remove_dir_all(d).unwrap();
    }
}
