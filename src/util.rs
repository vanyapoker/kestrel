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

/// Whether `name` matches any of the shell-style patterns in `patterns`, separated by
/// `,` or `;`. `*` and `?` are wildcards and case is ignored. A pattern without
/// wildcards matches names containing it.
pub fn glob_any(patterns: &str, name: &str) -> bool {
    let name: Vec<char> = name.chars().flat_map(char::to_lowercase).collect();
    patterns
        .split([',', ';'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .any(|p| {
            let p: Vec<char> = p.chars().flat_map(char::to_lowercase).collect();
            if p.iter().any(|&c| c == '*' || c == '?') {
                glob(&p, &name)
            } else {
                name.windows(p.len()).any(|w| w == p.as_slice())
            }
        })
}

fn glob(p: &[char], s: &[char]) -> bool {
    let (mut pi, mut si) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while si < s.len() {
        match p.get(pi) {
            Some('*') => {
                star = Some((pi, si));
                pi += 1;
            }
            Some(&c) if c == '?' || c == s[si] => {
                pi += 1;
                si += 1;
            }
            _ => match star {
                Some((sp, ss)) => {
                    pi = sp + 1;
                    si = ss + 1;
                    star = Some((sp, ss + 1));
                }
                None => return false,
            },
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

/// Case-insensitive natural ordering: "file2" < "file10".
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    natural_cmp_impl(a, b, false)
}

/// `natural_cmp` for names already case-folded (`Entry::sort_name`).
pub fn natural_cmp_folded(a: &str, b: &str) -> Ordering {
    natural_cmp_impl(a, b, true)
}

fn natural_cmp_impl(mut a: &str, mut b: &str, folded: bool) -> Ordering {
    loop {
        match (a.chars().next(), b.chars().next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let (na, rest_a) = split_digits(a);
                let (nb, rest_b) = split_digits(b);
                // Compare by length first (no overflow on huge numbers), then lexically.
                let (na, nb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let ord = na.len().cmp(&nb.len()).then_with(|| na.cmp(nb));
                if ord != Ordering::Equal {
                    return ord;
                }
                (a, b) = (rest_a, rest_b);
            }
            (Some(x), Some(y)) => {
                let ord = if folded {
                    x.cmp(&y)
                } else {
                    x.to_lowercase().cmp(y.to_lowercase())
                };
                if ord != Ordering::Equal {
                    return ord;
                }
                (a, b) = (&a[x.len_utf8()..], &b[y.len_utf8()..]);
            }
        }
    }
}

fn split_digits(s: &str) -> (&str, &str) {
    s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()))
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
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "ico" | "tif" | "tiff" | "tga"
        | "qoi" | "svg" | "avif" | "heic" => Category::Image,
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
        "rs" | "c"
            | "h"
            | "cpp"
            | "hpp"
            | "cc"
            | "cs"
            | "go"
            | "py"
            | "js"
            | "ts"
            | "jsx"
            | "tsx"
            | "java"
            | "kt"
            | "rb"
            | "php"
            | "lua"
            | "sh"
            | "bash"
            | "zsh"
            | "fish"
            | "zig"
            | "odin"
            | "json"
            | "toml"
            | "yaml"
            | "yml"
            | "xml"
            | "html"
            | "css"
            | "scss"
            | "sql"
            | "swift"
            | "hs"
            | "ml"
            | "ex"
            | "exs"
            | "erl"
            | "clj"
            | "scala"
            | "dart"
            | "vue"
            | "svelte"
            | "nix"
            | "cmake"
            | "mk"
            | "dockerfile"
            | "glsl"
            | "hlsl"
            | "wgsl"
            | "lock"
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
        assert!(glob_any("*.jpg", "Photo.JPG"));
        assert!(!glob_any("*.jpg", "photo.jpeg"));
        assert!(glob_any("img_??.png; *.txt", "IMG_01.png"));
        assert!(glob_any("*.txt, *.md", "notes.md"));
        assert!(glob_any("report", "Annual report 2025.pdf"));
        assert!(glob_any("a*b*c", "a--b--c"));
        assert!(!glob_any("a*b*c", "a--c--b"));
        assert!(!glob_any("", "x"));
        assert!(glob_any("*", ""));
        assert_eq!(natural_cmp("x007", "x7"), Ordering::Equal);
    }

    #[test]
    fn sizes() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(500 * 1024 * 1024), "500 MB");
    }
}
