//! Disk cache for thumbnails that are slow to render (SVG, PDF, video), stored in the
//! shared freedesktop.org location so other file managers can reuse them:
//! `$XDG_CACHE_HOME/thumbnails/large/<md5 of the file URI>.png`.

use eframe::egui::ColorImage;
use std::fs;
use std::io::{Cursor, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// "large" thumbnails are 256 px, which is `thumbs::THUMB_SIZE`.
const SIZE_DIR: &str = "large";
/// The shared folder is pruned at startup to stay below this many bytes.
const MAX_BYTES: u64 = 256 * 1024 * 1024;

fn dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("thumbnails").join(SIZE_DIR))
}

fn uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut s = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                s.push(b as char)
            }
            _ => s.push_str(&format!("%{b:02X}")),
        }
    }
    s
}

fn mtime_secs(meta: &fs::Metadata) -> i64 {
    meta.mtime()
}

fn cache_file(uri: &str) -> Option<PathBuf> {
    Some(dir()?.join(format!("{}.png", md5_hex(uri.as_bytes()))))
}

/// A thumbnail saved earlier for this file, if it is still current.
pub fn load(path: &Path) -> Option<ColorImage> {
    let uri = uri(path);
    let bytes = fs::read(cache_file(&uri)?).ok()?;
    let mtime = mtime_secs(&fs::metadata(path).ok()?);
    let decoder = png::Decoder::new(Cursor::new(&bytes));
    let reader = decoder.read_info().ok()?;
    let text = &reader.info().uncompressed_latin1_text;
    let field = |key: &str| {
        text.iter()
            .find(|t| t.keyword == key)
            .map(|t| t.text.as_str())
    };
    if field("Thumb::URI")? != uri || field("Thumb::MTime")?.parse::<i64>().ok()? != mtime {
        return None;
    }
    let img = image::load_from_memory(&bytes).ok()?;
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Some(ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()))
}

/// Saves a thumbnail; failures are ignored since the cache is only an optimization.
pub fn store(path: &Path, image: &ColorImage) {
    let _ = try_store(path, image);
}

fn try_store(path: &Path, image: &ColorImage) -> Option<()> {
    let uri = uri(path);
    let target = cache_file(&uri)?;
    let mtime = mtime_secs(&fs::metadata(path).ok()?);
    let dir = target.parent()?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .ok()?;

    let mut pixels = Vec::with_capacity(image.pixels.len() * 4);
    for p in &image.pixels {
        pixels.extend_from_slice(&p.to_srgba_unmultiplied());
    }
    let mut png_bytes = Vec::new();
    let mut encoder =
        png::Encoder::new(&mut png_bytes, image.width() as u32, image.height() as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.add_text_chunk("Thumb::URI".into(), uri).ok()?;
    encoder
        .add_text_chunk("Thumb::MTime".into(), mtime.to_string())
        .ok()?;
    encoder
        .write_header()
        .ok()?
        .write_image_data(&pixels)
        .ok()?;

    // Write beside the target, then rename, so readers never see a partial file.
    let tmp = dir.join(format!(".tmp-{}-{}", std::process::id(), md5_hex(&pixels)));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .ok()?;
    if file.write_all(&png_bytes).is_err() || fs::rename(&tmp, &target).is_err() {
        let _ = fs::remove_file(&tmp);
        return None;
    }
    Some(())
}

/// Deletes the oldest thumbnails until the folder fits in `MAX_BYTES`.
pub fn prune() {
    let Some(dir) = dir() else { return };
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut files: Vec<_> = rd
        .flatten()
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            m.is_file()
                .then(|| (m.modified().unwrap_or(UNIX_EPOCH), m.len(), e.path()))
        })
        .collect();
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    if total <= MAX_BYTES {
        return;
    }
    files.sort();
    for (_, len, path) in files {
        if total <= MAX_BYTES {
            break;
        }
        if fs::remove_file(path).is_ok() {
            total -= len;
        }
    }
}

fn md5_hex(data: &[u8]) -> String {
    md5(data).iter().map(|b| format!("{b:02x}")).collect()
}

/// MD5, which the thumbnail spec mandates for naming cache files (not for security).
fn md5(data: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k: Vec<u32> = (0..64)
        .map(|i| ((i as f64 + 1.0).sin().abs() * 4294967296.0) as u32)
        .collect();
    let mut state: [u32; 4] = [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64).wrapping_mul(8)).to_le_bytes());
    for chunk in msg.chunks(64) {
        let m: Vec<u32> = chunk
            .chunks(4)
            .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
            .collect();
        let [mut a, mut b, mut c, mut d] = state;
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let rotated = a
                .wrapping_add(f)
                .wrapping_add(k[i])
                .wrapping_add(m[g])
                .rotate_left(S[i]);
            (a, d, c) = (d, c, b);
            b = b.wrapping_add(rotated);
        }
        for (s, v) in state.iter_mut().zip([a, b, c, d]) {
            *s = s.wrapping_add(v);
        }
    }
    let mut out = [0u8; 16];
    for (o, s) in out.chunks_mut(4).zip(state) {
        o.copy_from_slice(&s.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_matches_known_vectors() {
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        let long =
            "12345678901234567890123456789012345678901234567890123456789012345678901234567890";
        assert_eq!(md5_hex(long.as_bytes()), "57edf4a22be3c955ac49da2e2107b67a");
    }

    #[test]
    fn uris_are_percent_encoded() {
        assert_eq!(uri(Path::new("/a b/ü.pdf")), "file:///a%20b/%C3%BC.pdf");
    }
}

#[cfg(test)]
mod roundtrip {
    use super::*;

    #[test]
    #[ignore = "needs XDG_CACHE_HOME pointing at a scratch directory"]
    fn store_then_load() {
        let file = std::env::temp_dir().join(format!("kestrel-rt-{}.svg", std::process::id()));
        fs::write(&file, "x").unwrap();
        let img = ColorImage::from_rgba_unmultiplied([2, 1], &[255, 0, 0, 255, 0, 255, 0, 128]);
        assert!(load(&file).is_none());
        store(&file, &img);
        let back = load(&file).expect("cached");
        assert_eq!(back.size, [2, 1]);
        assert_eq!(back.pixels[0], img.pixels[0]);
        fs::write(&file, "changed").unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        fs::File::open(&file).unwrap().set_modified(later).unwrap();
        assert!(load(&file).is_none(), "stale after mtime change");
        let _ = fs::remove_file(&file);
    }
}
