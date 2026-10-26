use rayon::prelude::*;
use std::collections::{HashMap, hash_map};
use std::ffi::CStr;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

#[derive(Clone)]
pub struct Entry {
    pub name: String,
    /// Case-folded `name`, computed once so sorting never has to lowercase.
    pub sort_name: String,
    pub path: PathBuf,
    pub ext: String,
    pub is_dir: bool,
    pub is_link: bool,
    pub hidden: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub mode: u32,
    /// Parent folder relative to the search root (search results only).
    pub rel: Option<String>,
}

/// Reads a directory. Metadata is fetched in parallel, which matters on large folders.
pub fn list_dir(path: &Path) -> Result<Vec<Entry>, String> {
    let rd = fs::read_dir(path).map_err(|e| e.to_string())?;
    let raw: Vec<fs::DirEntry> = rd.flatten().collect();
    Ok(raw
        .par_iter()
        .filter_map(|de| {
            let lmeta = de.metadata().ok()?; // does not follow symlinks
            let name = de.file_name().to_string_lossy().into_owned();
            Some(make_entry(de.path(), name, lmeta))
        })
        .collect())
}

/// Builds an entry from `symlink_metadata`; symlinks report their target's type and size.
pub fn make_entry(path: PathBuf, name: String, lmeta: fs::Metadata) -> Entry {
    let is_link = lmeta.file_type().is_symlink();
    let meta = if is_link {
        fs::metadata(&path).unwrap_or(lmeta)
    } else {
        lmeta
    };
    let is_dir = meta.is_dir();
    let ext = if is_dir {
        String::new()
    } else {
        Path::new(&name)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    };
    Entry {
        hidden: name.starts_with('.'),
        sort_name: name.chars().flat_map(char::to_lowercase).collect(),
        name,
        path,
        ext,
        is_dir,
        is_link,
        size: if is_dir { 0 } else { meta.len() },
        modified: meta.modified().ok(),
        mode: meta.mode(),
        rel: None,
    }
}

pub fn device_of(path: &Path) -> Option<u64> {
    fs::symlink_metadata(path).ok().map(|m| m.dev())
}

/// A file with several hard links, of which `seen` lie inside a folder. The
/// folder counts it once; while links remain outside, a parent folder that
/// reaches it through another path must not count it again.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Link {
    pub ino: u64,
    pub size: u64,
    pub seen: u64,
    pub nlink: u64,
}

/// Apparent size of a folder, counting each hard-linked file once, like `du`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Usage {
    pub size: u64,
    /// Hard-linked files with links outside the folder.
    pub open: Arc<[Link]>,
}

/// Records a link; false when its file was already counted.
fn add_link(links: &mut HashMap<u64, Link>, link: Link) -> bool {
    match links.entry(link.ino) {
        hash_map::Entry::Vacant(v) => {
            v.insert(link);
            true
        }
        hash_map::Entry::Occupied(mut o) => {
            o.get_mut().seen += link.seen;
            false
        }
    }
}

struct Dir(*mut libc::DIR);

impl Drop for Dir {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from a successful `fdopendir` and is closed only here.
        unsafe { libc::closedir(self.0) };
    }
}

/// Recursive apparent size of a directory. Does not follow symlinks or cross
/// filesystem boundaries (so /proc, network mounts etc. are skipped).
/// Reuses cached subtree totals and publishes each completed directory bottom-up.
/// Returns `None` if cancelled.
pub fn dir_size(
    path: &Path,
    dev: u64,
    cancel: &AtomicBool,
    cached: &(impl Fn(&Path) -> Option<Usage> + Sync),
    completed: &(impl Fn(&Path, &Usage) + Sync),
) -> Option<Usage> {
    use std::os::unix::ffi::OsStrExt;
    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    walk(libc::AT_FDCWD, &name, path, dev, cancel, cached, completed)
}

/// Opens folders relative to their parent and stats files relative to their
/// folder, so the kernel never resolves a full path.
fn walk(
    parent: libc::c_int,
    name: &CStr,
    path: &Path,
    dev: u64,
    cancel: &AtomicBool,
    cached: &(impl Fn(&Path) -> Option<Usage> + Sync),
    completed: &(impl Fn(&Path, &Usage) + Sync),
) -> Option<Usage> {
    use std::os::unix::ffi::OsStrExt;
    if cancel.load(Ordering::Relaxed) {
        return None;
    }
    if let Some(usage) = cached(path) {
        return Some(usage);
    }
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    // SAFETY: `name` is NUL-terminated and `parent` is an open folder or AT_FDCWD.
    let fd = unsafe { libc::openat(parent, name.as_ptr(), flags) };
    let dirp = if fd < 0 {
        std::ptr::null_mut()
    } else {
        // SAFETY: all-zero bytes are a valid `stat`.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: `fd` is open and `st` is writable.
        if unsafe { libc::fstat(fd, &mut st) } == 0 && st.st_dev != dev {
            // SAFETY: `fd` is open and ours.
            unsafe { libc::close(fd) };
            return Some(Usage::default());
        }
        // SAFETY: `fd` is open; on success the stream owns it.
        let dirp = unsafe { libc::fdopendir(fd) };
        if dirp.is_null() {
            // SAFETY: fdopendir failed, so `fd` is still ours to close.
            unsafe { libc::close(fd) };
        }
        dirp
    };
    if dirp.is_null() {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let usage = Usage::default();
        completed(path, &usage);
        return Some(usage);
    }
    let dir = Dir(dirp);
    let mut total = 0u64;
    let mut links = HashMap::new();
    let mut subdirs = Vec::new();
    loop {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        // SAFETY: `dir` is open and only this thread reads it.
        let ent = unsafe { libc::readdir64(dir.0) };
        if ent.is_null() {
            break;
        }
        // SAFETY: readdir returned an entry, valid until the next call.
        let (kind, name) = unsafe { ((*ent).d_type, CStr::from_ptr((*ent).d_name.as_ptr())) };
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        if kind == libc::DT_DIR {
            subdirs.push(name.to_owned());
            continue;
        }
        if kind != libc::DT_REG && kind != libc::DT_UNKNOWN {
            continue;
        }
        // SAFETY: all-zero bytes are a valid `stat`.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: `name` is NUL-terminated, relative to the open folder `fd`.
        if unsafe { libc::fstatat(fd, name.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) } != 0 {
            continue;
        }
        match st.st_mode & libc::S_IFMT {
            libc::S_IFDIR => subdirs.push(name.to_owned()),
            libc::S_IFREG => {
                let size = st.st_size as u64;
                let link = Link {
                    ino: st.st_ino,
                    size,
                    seen: 1,
                    nlink: st.st_nlink,
                };
                if st.st_nlink < 2 || add_link(&mut links, link) {
                    total += size;
                }
            }
            _ => {}
        }
    }
    let children: Option<Vec<Usage>> = subdirs
        .par_iter()
        .map(|n| {
            let child = path.join(std::ffi::OsStr::from_bytes(n.to_bytes()));
            walk(fd, n, &child, dev, cancel, cached, completed)
        })
        .collect();
    drop(dir);
    for child in children? {
        total += child.size;
        for &link in child.open.iter() {
            if !add_link(&mut links, link) {
                total = total.saturating_sub(link.size);
            }
        }
    }
    if cancel.load(Ordering::Relaxed) {
        return None;
    }
    let usage = Usage {
        size: total,
        open: links.into_values().filter(|l| l.seen < l.nlink).collect(),
    };
    completed(path, &usage);
    Some(usage)
}

/// (free, total) bytes of the filesystem containing `path`.
pub fn disk_space(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: `statvfs` is a plain C struct for which all-zero bytes are a valid value.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a NUL-terminated path and `st` is a valid, writable `statvfs`.
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let bs = st.f_frsize as u64;
    Some((st.f_bavail as u64 * bs, st.f_blocks as u64 * bs))
}

pub struct Places {
    pub places: Vec<(String, PathBuf)>,
    pub bookmarks: Vec<(String, PathBuf)>,
}

/// The home trash's `files` folder, where trashed items live.
pub fn trash_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home_dir().join(".local/share"))
        .join("Trash/files")
}

pub fn in_trash(path: &Path) -> bool {
    path.starts_with(trash_dir())
}

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// Well-known folders that get their own icon.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Special {
    Home,
    Desktop,
    Documents,
    Downloads,
    Pictures,
    Music,
    Videos,
    Projects,
    Work,
    Public,
    Templates,
    Root,
    Drive,
    Bookmark,
    Trash,
}

const XDG_DIRS: [(&str, &str, Special); 9] = [
    ("Desktop", "XDG_DESKTOP_DIR", Special::Desktop),
    ("Documents", "XDG_DOCUMENTS_DIR", Special::Documents),
    ("Downloads", "XDG_DOWNLOAD_DIR", Special::Downloads),
    ("Pictures", "XDG_PICTURES_DIR", Special::Pictures),
    ("Music", "XDG_MUSIC_DIR", Special::Music),
    ("Videos", "XDG_VIDEOS_DIR", Special::Videos),
    ("Projects", "XDG_PROJECTS_DIR", Special::Projects),
    ("Public", "XDG_PUBLICSHARE_DIR", Special::Public),
    ("Templates", "XDG_TEMPLATES_DIR", Special::Templates),
];

/// The icon kind for `path`, if it is a well-known folder.
pub fn special(path: &Path) -> Option<Special> {
    static MAP: std::sync::OnceLock<HashMap<PathBuf, Special>> = std::sync::OnceLock::new();
    MAP.get_or_init(|| {
        let home = home_dir();
        let mut m = HashMap::new();
        for (name, key, kind) in XDG_DIRS {
            m.insert(
                xdg_user_dir(&home, key).unwrap_or_else(|| home.join(name)),
                kind,
            );
        }
        for (name, kind) in [
            ("Work", Special::Work),
            ("Code", Special::Projects),
            ("code", Special::Projects),
            ("src", Special::Projects),
            ("dev", Special::Projects),
            ("repos", Special::Projects),
            ("git", Special::Projects),
        ] {
            m.entry(home.join(name)).or_insert(kind);
        }
        // Unused XDG dirs often point at $HOME itself.
        m.insert(home, Special::Home);
        m.insert(PathBuf::from("/"), Special::Root);
        m.insert(trash_dir(), Special::Trash);
        m
    })
    .get(path)
    .copied()
}

pub fn places() -> Places {
    let home = home_dir();
    let mut places = vec![("Home".to_string(), home.clone())];
    for (name, key, _) in &XDG_DIRS[..7] {
        let p = xdg_user_dir(&home, key).unwrap_or_else(|| home.join(name));
        if p.is_dir() && p != home {
            places.push((name.to_string(), p));
        }
    }
    places.push(("Root".to_string(), PathBuf::from("/")));
    // Make sure there is a trash to open, even before anything has been trashed.
    let trash = trash_dir();
    if let Some(root) = trash.parent() {
        let _ = fs::create_dir_all(root.join("info"));
    }
    if fs::create_dir_all(&trash).is_ok() {
        places.push(("Trash".to_string(), trash));
    }

    let mut bookmarks = Vec::new();
    if let Ok(s) = fs::read_to_string(bookmarks_file()) {
        for line in s.lines() {
            let Some((p, label)) = parse_bookmark(line) else {
                continue;
            };
            let label = if label.is_empty() {
                p.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| p.display().to_string())
            } else {
                label.to_string()
            };
            bookmarks.push((label, p));
        }
    }

    Places { places, bookmarks }
}

/// Bookmarks live in GTK's file, so they are shared with Nautilus and file pickers.
fn bookmarks_file() -> PathBuf {
    home_dir().join(".config/gtk-3.0/bookmarks")
}

fn parse_bookmark(line: &str) -> Option<(PathBuf, &str)> {
    let (uri, label) = line.split_once(' ').unwrap_or((line, ""));
    let p = uri.strip_prefix("file://")?;
    Some((PathBuf::from(percent_decode(p)), label))
}

pub fn add_bookmark(path: &Path) -> std::io::Result<bool> {
    add_bookmark_in(&bookmarks_file(), path)
}

pub fn remove_bookmark(path: &Path) -> std::io::Result<bool> {
    remove_bookmark_in(&bookmarks_file(), path)
}

/// Appends `path` unless it is already bookmarked. Returns whether it was added.
fn add_bookmark_in(file: &Path, path: &Path) -> std::io::Result<bool> {
    let text = fs::read_to_string(file).unwrap_or_default();
    if text
        .lines()
        .filter_map(parse_bookmark)
        .any(|(p, _)| p == path)
    {
        return Ok(false);
    }
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut out = text;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&crate::wl_dnd::file_uri(path));
    out.push('\n');
    fs::write(file, out)?;
    Ok(true)
}

/// Drops every line pointing at `path`, keeping other lines as they were.
fn remove_bookmark_in(file: &Path, path: &Path) -> std::io::Result<bool> {
    let text = fs::read_to_string(file)?;
    let kept: Vec<&str> = text
        .lines()
        .filter(|l| parse_bookmark(l).is_none_or(|(p, _)| p != path))
        .collect();
    if kept.len() == text.lines().count() {
        return Ok(false);
    }
    let mut out = kept.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    fs::write(file, out)?;
    Ok(true)
}

fn xdg_user_dir(home: &Path, key: &str) -> Option<PathBuf> {
    let s = fs::read_to_string(home.join(".config/user-dirs.dirs")).ok()?;
    for line in s.lines() {
        if let Some(v) = line.strip_prefix(key).and_then(|r| r.strip_prefix('=')) {
            let v = v.trim().trim_matches('"');
            let v = v.replace("$HOME", &home.to_string_lossy());
            return Some(PathBuf::from(v));
        }
    }
    None
}

pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("");
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn open_default(path: &Path) {
    let _ = std::process::Command::new("xdg-open")
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

pub fn open_terminal(dir: &Path) {
    let candidates: Vec<String> = std::env::var("TERMINAL")
        .ok()
        .into_iter()
        .chain(["xdg-terminal-exec", "alacritty", "kitty", "foot", "ghostty"].map(String::from))
        .collect();
    for term in candidates {
        let spawned = std::process::Command::new(&term)
            .current_dir(dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        if spawned.is_ok() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn bookmark_file_edits() {
        let d = std::env::temp_dir().join(format!("kestrel-bm-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        let file = d.join("gtk-3.0/bookmarks");
        let a = Path::new("/home/x/My Stuff");
        assert!(add_bookmark_in(&file, a).unwrap());
        assert!(!add_bookmark_in(&file, a).unwrap()); // no duplicates
        fs::write(
            &file,
            format!(
                "file:///keep Kept label\n{}",
                fs::read_to_string(&file).unwrap()
            ),
        )
        .unwrap();
        assert!(add_bookmark_in(&file, Path::new("/b")).unwrap());
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "file:///keep Kept label\nfile:///home/x/My%20Stuff\nfile:///b\n"
        );
        assert!(remove_bookmark_in(&file, a).unwrap());
        assert!(!remove_bookmark_in(&file, a).unwrap());
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "file:///keep Kept label\nfile:///b\n"
        );
        fs::remove_dir_all(d).unwrap();
    }

    /// `cargo test --release -- --ignored --nocapture bench`
    #[test]
    #[ignore]
    fn bench_listing_and_sizes() {
        for dir in ["/usr/lib", "/usr/bin"] {
            let t = Instant::now();
            let n = list_dir(Path::new(dir)).unwrap().len();
            println!("list_dir {dir}: {n} entries in {:?}", t.elapsed());
        }
        let dir = Path::new("/usr");
        let mut times = Vec::new();
        let mut size = 0;
        for _ in 0..15 {
            let t = Instant::now();
            size = dir_size(
                dir,
                device_of(dir).unwrap(),
                &AtomicBool::new(false),
                &|_| None,
                &|_, _| {},
            )
            .unwrap()
            .size;
            times.push(t.elapsed());
        }
        times.sort();
        println!(
            "dir_size /usr: {} median {:?} min {:?}",
            crate::util::human_size(size),
            times[times.len() / 2],
            times[0]
        );
    }
}
