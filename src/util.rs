use std::cmp::Ordering;
use std::time::SystemTime;

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if v >= 100.0 {
        format!("{v:.0} {}", UNITS[unit])
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

pub fn fmt_time(t: SystemTime) -> String {
    let dt: chrono::DateTime<chrono::Local> = t.into();
    dt.format("%Y-%m-%d %H:%M").to_string()
}

pub fn fmt_mode(mode: u32, is_dir: bool) -> String {
    let mut s = String::with_capacity(10);
    s.push(if is_dir { 'd' } else { '-' });
    for shift in [6, 3, 0] {
        let bits = (mode >> shift) & 7;
        s.push(if bits & 4 != 0 { 'r' } else { '-' });
        s.push(if bits & 2 != 0 { 'w' } else { '-' });
        s.push(if bits & 1 != 0 { 'x' } else { '-' });
    }
    s
}

/// Case-insensitive natural ordering: "file2" < "file10".
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let na = take_number(&mut a);
                let nb = take_number(&mut b);
                // Compare by length first (no overflow on huge numbers), then lexically.
                let ord = na
                    .trim_start_matches('0')
                    .len()
                    .cmp(&nb.trim_start_matches('0').len())
                    .then_with(|| na.trim_start_matches('0').cmp(nb.trim_start_matches('0')));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(x), Some(y)) => {
                let ord = x.to_lowercase().cmp(y.to_lowercase());
                if ord != Ordering::Equal {
                    return ord;
                }
                a.next();
                b.next();
            }
        }
    }
}

fn take_number(it: &mut std::iter::Peekable<std::str::Chars>) -> String {
    let mut s = String::new();
    while let Some(c) = it.peek().copied() {
        if !c.is_ascii_digit() {
            break;
        }
        s.push(c);
        it.next();
    }
    s
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Folder,
    Image,
    Video,
    Audio,
    Archive,
    Code,
    Text,
    Document,
    Executable,
    Other,
}

pub fn category(ext: &str, is_dir: bool) -> Category {
    if is_dir {
        return Category::Folder;
    }
    match ext {
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "ico" | "tif" | "tiff" | "tga" | "qoi"
        | "svg" | "avif" | "heic" => Category::Image,
        "mp4" | "mkv" | "webm" | "avi" | "mov" | "wmv" | "flv" | "m4v" => Category::Video,
        "mp3" | "flac" | "wav" | "ogg" | "opus" | "m4a" | "aac" => Category::Audio,
        "zip" | "tar" | "gz" | "xz" | "zst" | "bz2" | "7z" | "rar" | "tgz" | "deb" | "rpm" => {
            Category::Archive
        }
        "pdf" | "doc" | "docx" | "odt" | "xls" | "xlsx" | "ods" | "ppt" | "pptx" | "epub" => {
            Category::Document
        }
        "exe" | "appimage" | "bin" | "run" => Category::Executable,
        "txt" | "md" | "log" | "csv" | "rst" | "org" | "ini" | "cfg" | "conf" => Category::Text,
        _ if is_code_ext(ext) => Category::Code,
        _ => Category::Other,
    }
}

pub fn is_code_ext(ext: &str) -> bool {
    matches!(
        ext,
        "rs" | "c" | "h" | "cpp" | "hpp" | "cc" | "cs" | "go" | "py" | "js" | "ts" | "jsx" | "tsx"
            | "java" | "kt" | "rb" | "php" | "lua" | "sh" | "bash" | "zsh" | "fish" | "zig"
            | "odin" | "json" | "toml" | "yaml" | "yml" | "xml" | "html" | "css" | "scss" | "sql"
            | "swift" | "hs" | "ml" | "ex" | "exs" | "erl" | "clj" | "scala" | "dart" | "vue"
            | "svelte" | "nix" | "cmake" | "mk" | "dockerfile" | "glsl" | "hlsl" | "wgsl" | "lock"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        assert_eq!(natural_cmp("file2", "file10"), Ordering::Less);
        assert_eq!(natural_cmp("File", "file"), Ordering::Equal);
        assert_eq!(natural_cmp("a", "b"), Ordering::Less);
        assert_eq!(natural_cmp("x007", "x7"), Ordering::Equal);
    }

    #[test]
    fn sizes() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(500 * 1024 * 1024), "500 MB");
    }
}
