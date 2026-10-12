use rayon::prelude::*;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub ext: String,
    pub is_dir: bool,
    pub is_link: bool,
    pub hidden: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub mode: u32,
}

/// Reads a directory. Metadata is fetched in parallel, which matters on large folders.
pub fn list_dir(path: &Path) -> Result<Vec<Entry>, String> {
    let rd = fs::read_dir(path).map_err(|e| e.to_string())?;
    let raw: Vec<fs::DirEntry> = rd.flatten().collect();
    Ok(raw
        .par_iter()
        .filter_map(|de| {
            let path = de.path();
            let lmeta = de.metadata().ok()?; // does not follow symlinks
            let is_link = lmeta.file_type().is_symlink();
            // For symlinks, report the target's type/size when it resolves.
            let meta = if is_link {
                fs::metadata(&path).unwrap_or(lmeta)
            } else {
                lmeta
            };
            let name = de.file_name().to_string_lossy().into_owned();
            let is_dir = meta.is_dir();
            let ext = if is_dir {
                String::new()
            } else {
                Path::new(&name)
                    .extension()
                    .map(|e| e.to_string_lossy().to_lowercase())
                    .unwrap_or_default()
            };
            Some(Entry {
                hidden: name.starts_with('.'),
                name,
                path,
                ext,
                is_dir,
                is_link,
                size: if is_dir { 0 } else { meta.len() },
                modified: meta.modified().ok(),
                mode: meta.mode(),
            })
        })
        .collect())
}

pub fn device_of(path: &Path) -> Option<u64> {
    fs::symlink_metadata(path).ok().map(|m| m.dev())
}

/// Recursive apparent size of a directory. Does not follow symlinks or cross
/// filesystem boundaries (so /proc, network mounts etc. are skipped).
/// Returns `None` if cancelled.
pub fn dir_size(path: &Path, dev: u64, cancel: &AtomicBool) -> Option<u64> {
    if cancel.load(Ordering::Relaxed) {
        return None;
    }
    let Ok(rd) = fs::read_dir(path) else {
        return Some(0);
    };
    let mut total = 0u64;
    let mut subdirs = Vec::new();
    for de in rd.flatten() {
        let Ok(ft) = de.file_type() else { continue };
        if ft.is_dir() {
            subdirs.push(de.path());
        } else if ft.is_file() {
            if let Ok(m) = de.metadata() {
                total += m.len();
            }
        }
    }
    let sub: Option<u64> = subdirs
        .par_iter()
        .filter(|d| device_of(d) == Some(dev))
        .map(|d| dir_size(d, dev, cancel))
        .sum();
    Some(total + sub?)
}

pub fn count_entries(path: &Path) -> Option<usize> {
    fs::read_dir(path).ok().map(|rd| rd.count())
}

/// (free, total) bytes of the filesystem containing `path`.
pub fn disk_space(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let bs = st.f_frsize as u64;
    Some((st.f_bavail as u64 * bs, st.f_blocks as u64 * bs))
}

pub struct Places {
    pub places: Vec<(String, PathBuf)>,
    pub bookmarks: Vec<(String, PathBuf)>,
    pub devices: Vec<(String, PathBuf)>,
}

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

pub fn places() -> Places {
    let home = home_dir();
    let mut places = vec![("Home".to_string(), home.clone())];
    for (name, key) in [
        ("Desktop", "XDG_DESKTOP_DIR"),
        ("Documents", "XDG_DOCUMENTS_DIR"),
        ("Downloads", "XDG_DOWNLOAD_DIR"),
        ("Pictures", "XDG_PICTURES_DIR"),
        ("Music", "XDG_MUSIC_DIR"),
        ("Videos", "XDG_VIDEOS_DIR"),
    ] {
        let p = xdg_user_dir(&home, key).unwrap_or_else(|| home.join(name));
        if p.is_dir() && p != home {
            places.push((name.to_string(), p));
        }
    }
    places.push(("Root".to_string(), PathBuf::from("/")));

    let mut bookmarks = Vec::new();
    if let Ok(s) = fs::read_to_string(home.join(".config/gtk-3.0/bookmarks")) {
        for line in s.lines() {
            let (uri, label) = line.split_once(' ').unwrap_or((line, ""));
            if let Some(p) = uri.strip_prefix("file://") {
                let p = PathBuf::from(percent_decode(p));
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
    }

    let mut devices = Vec::new();
    if let Ok(s) = fs::read_to_string("/proc/mounts") {
        for line in s.lines() {
            let mut it = line.split_whitespace();
            let (Some(src), Some(mnt)) = (it.next(), it.next()) else { continue };
            let mnt = mnt.replace("\\040", " ");
            if src.starts_with("/dev/")
                && (mnt.starts_with("/mnt") || mnt.starts_with("/media") || mnt.starts_with("/run/media"))
            {
                let p = PathBuf::from(&mnt);
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or(mnt.clone());
                devices.push((name, p));
            }
        }
    }
    Places { places, bookmarks, devices }
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

fn percent_decode(s: &str) -> String {
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
        let t = Instant::now();
        let size = dir_size(dir, device_of(dir).unwrap(), &AtomicBool::new(false)).unwrap();
        println!("dir_size /usr: {} in {:?}", crate::util::human_size(size), t.elapsed());
    }
}
