//! File-picker mode: `kestrel --pick open|folder|save` shows the normal window with a
//! choose bar, prints the chosen paths (one per line) on exit, and prints nothing if
//! cancelled. The portal backend (`portal.rs`) runs this for other apps' file dialogs.

use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Open,
    Folder,
    Save,
}

#[derive(Clone, Debug)]
pub struct Request {
    pub kind: Kind,
    pub multiple: bool,
    pub title: String,
    /// Optional action label supplied by the calling application.
    pub accept_label: String,
    /// Folder to start in.
    pub dir: Option<PathBuf>,
    /// Suggested file name when saving.
    pub name: String,
    pub filters: Vec<Filter>,
    pub filter_index: usize,
    /// Portal `parent_window` handle (`wayland:<xdg-foreign handle>`).
    pub parent: String,
    /// Application id of the caller.
    pub app: String,
}

/// Parses `--pick KIND [--multiple] [--title T] [--dir D] [--name N]`.
pub fn parse_args(args: &[String]) -> Option<Request> {
    let at = args.iter().position(|a| a == "--pick")?;
    let kind = match args.get(at + 1)?.as_str() {
        "open" => Kind::Open,
        "folder" => Kind::Folder,
        "save" => Kind::Save,
        _ => return None,
    };
    let value = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let filters = value("--filters")
        .and_then(|s| serde_json::from_str::<Vec<serde_json::Value>>(&s).ok())
        .map(|v| v.iter().filter_map(filter_from_json).collect::<Vec<_>>())
        .unwrap_or_default();
    let filter_index = value("--filter-index")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0)
        .min(filters.len().saturating_sub(1));
    Some(Request {
        kind,
        multiple: args.iter().any(|a| a == "--multiple"),
        title: value("--title").unwrap_or_default(),
        accept_label: value("--accept-label").unwrap_or_default(),
        dir: value("--dir").map(PathBuf::from).filter(|p| p.is_dir()),
        name: value("--name").unwrap_or_default(),
        filters,
        filter_index,
        parent: value("--parent").unwrap_or_default(),
        app: value("--app").unwrap_or_default(),
    })
}

fn filter_from_json(v: &serde_json::Value) -> Option<Filter> {
    let name = v.get("name")?.as_str()?.to_owned();
    let rules = v
        .get("rules")?
        .as_array()?
        .iter()
        .filter_map(|r| {
            let r = r.as_array()?;
            Some((u32::try_from(r.first()?.as_u64()?).ok()?, r.get(1)?.as_str()?.to_owned()))
        })
        .collect();
    Some(Filter { name, rules })
}

/// A file-type filter from the portal: rules are `(kind, pattern)`, kind 0 = glob, 1 = mime type.
#[derive(Clone, Debug, PartialEq)]
pub struct Filter {
    pub name: String,
    pub rules: Vec<(u32, String)>,
}

impl Filter {
    pub fn allows(&self, name: &str) -> bool {
        self.rules.iter().any(|(kind, pat)| match kind {
            0 => glob_match(pat, name),
            1 => mime_matches(pat, name),
            _ => false,
        })
    }
}

static ACTIVE: std::sync::RwLock<Option<Filter>> = std::sync::RwLock::new(None);

/// Sets the filter every listing in this process obeys (the picker serves one request).
pub fn set_active(filter: Option<Filter>) {
    *ACTIVE.write().expect("lock poisoned") = filter;
}

/// Whether an entry shows under the active filter; folders always show.
pub fn allows(name: &str, is_dir: bool) -> bool {
    is_dir
        || ACTIVE
            .read()
            .expect("lock poisoned")
            .as_ref()
            .is_none_or(|f| f.allows(name))
}

/// Case-sensitive shell glob: `*`, `?`, `[abc]`, `[a-z]`, `[!x]`, `[^x]`.
pub fn glob_match(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    glob_at(&p, &n)
}

fn glob_at(p: &[char], n: &[char]) -> bool {
    match p.first() {
        None => n.is_empty(),
        Some('*') => (0..=n.len()).any(|i| glob_at(&p[1..], &n[i..])),
        Some('?') => !n.is_empty() && glob_at(&p[1..], &n[1..]),
        Some('[') => {
            let Some((&c, rest)) = n.split_first() else {
                return false;
            };
            let neg = matches!(p.get(1), Some('!' | '^'));
            let start = if neg { 2 } else { 1 };
            // A `]` directly after the opener is a member, not the closer.
            let Some(end) = (start + 1..p.len()).find(|&i| p[i] == ']') else {
                return c == '[' && glob_at(&p[1..], rest);
            };
            let set = &p[start..end];
            let mut hit = false;
            let mut i = 0;
            while i < set.len() {
                if i + 2 < set.len() && set[i + 1] == '-' {
                    hit |= (set[i]..=set[i + 2]).contains(&c);
                    i += 3;
                } else {
                    hit |= set[i] == c;
                    i += 1;
                }
            }
            hit != neg && glob_at(&p[end + 1..], rest)
        }
        Some(&c) => n.first() == Some(&c) && glob_at(&p[1..], &n[1..]),
    }
}

const MIME: &[(&str, &str)] = &[
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("svg", "image/svg+xml"),
    ("bmp", "image/bmp"),
    ("heic", "image/heic"),
    ("heif", "image/heif"),
    ("tif", "image/tiff"),
    ("tiff", "image/tiff"),
    ("txt", "text/plain"),
    ("md", "text/markdown"),
    ("html", "text/html"),
    ("htm", "text/html"),
    ("css", "text/css"),
    ("csv", "text/csv"),
    ("json", "application/json"),
    ("xml", "application/xml"),
    ("pdf", "application/pdf"),
    ("zip", "application/zip"),
    ("gz", "application/gzip"),
    ("tar", "application/x-tar"),
    ("mp3", "audio/mpeg"),
    ("flac", "audio/flac"),
    ("ogg", "audio/ogg"),
    ("wav", "audio/x-wav"),
    ("mp4", "video/mp4"),
    ("mkv", "video/x-matroska"),
    ("webm", "video/webm"),
    (
        "docx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    ),
    (
        "xlsx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    ),
    ("odt", "application/vnd.oasis.opendocument.text"),
];

/// Whether the file's mime type (guessed from its extension) matches `pattern`.
pub fn mime_matches(pattern: &str, name: &str) -> bool {
    if pattern == "*/*" || pattern == "application/octet-stream" {
        return true;
    }
    let Some((_, ext)) = name.rsplit_once('.') else {
        return false;
    };
    let ext = ext.to_ascii_lowercase();
    let Some((_, mime)) = MIME.iter().find(|(e, _)| *e == ext) else {
        return false;
    };
    match pattern.strip_suffix('*') {
        Some(prefix) if pattern.ends_with("/*") => mime.starts_with(prefix),
        _ => *mime == pattern,
    }
}

impl Request {
    pub fn to_args(&self) -> Vec<String> {
        let mut args = vec![
            "--pick".to_string(),
            match self.kind {
                Kind::Open => "open",
                Kind::Folder => "folder",
                Kind::Save => "save",
            }
            .to_string(),
        ];
        if self.multiple {
            args.push("--multiple".into());
        }
        let mut opt = |flag: &str, v: String| {
            if !v.is_empty() {
                args.push(flag.into());
                args.push(v);
            }
        };
        opt("--title", self.title.clone());
        opt("--accept-label", self.accept_label.clone());
        opt(
            "--dir",
            self.dir
                .as_ref()
                .map(|d| d.to_string_lossy().into_owned())
                .unwrap_or_default(),
        );
        opt("--name", self.name.clone());
        opt("--parent", self.parent.clone());
        opt("--app", self.app.clone());
        if !self.filters.is_empty() {
            let json: Vec<serde_json::Value> = self
                .filters
                .iter()
                .map(|f| serde_json::json!({ "name": f.name, "rules": f.rules }))
                .collect();
            args.push("--filters".into());
            args.push(serde_json::Value::Array(json).to_string());
            args.push("--filter-index".into());
            args.push(self.filter_index.to_string());
        }
        args
    }

    /// Heading for the bar and the window title.
    pub fn heading(&self) -> String {
        if !self.title.is_empty() {
            return self.title.clone();
        }
        match self.kind {
            Kind::Open if self.multiple => "Select files",
            Kind::Open => "Select a file",
            Kind::Folder => "Select a folder",
            Kind::Save => "Save as",
        }
        .into()
    }

    pub fn confirm_label(&self) -> &str {
        if !self.accept_label.is_empty() {
            return &self.accept_label;
        }
        match self.kind {
            Kind::Open => "Open",
            Kind::Folder => "Select folder",
            Kind::Save => "Save",
        }
    }

    /// Window title: the heading, plus the name of the calling application.
    pub fn window_title(&self) -> String {
        if self.app.is_empty() {
            self.heading()
        } else {
            format!("{} — {}", self.heading(), self.app)
        }
    }
}

/// `file://` URI for an absolute path.
pub fn uri(path: &std::path::Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_round_trip() {
        let req = Request {
            kind: Kind::Save,
            multiple: true,
            title: "Save it".into(),
            accept_label: "Export".into(),
            dir: Some(std::env::temp_dir()),
            name: "a b.txt".into(),
            filters: vec![
                Filter {
                    name: "Images".into(),
                    rules: vec![(0, "*.png".into()), (1, "image/png".into())],
                },
                Filter {
                    name: "Text".into(),
                    rules: vec![(0, "*.txt".into())],
                },
            ],
            filter_index: 1,
            parent: "wayland:abc".into(),
            app: "firefox".into(),
        };
        let back = parse_args(&req.to_args()).unwrap();
        assert_eq!(back.filters, req.filters);
        assert_eq!(back.filter_index, 1);
        assert_eq!(back.parent, "wayland:abc");
        assert_eq!(back.app, "firefox");
        assert_eq!(back.window_title(), "Save it — firefox");
    }

    #[test]
    fn glob_matching() {
        assert!(glob_match("*.[pP][nN][gG]", "A.PNG"));
        assert!(!glob_match("*.[pP][nN][gG]", "a.png.txt"));
        assert!(!glob_match("[!a]b", "ab"));
        assert!(glob_match("[a-c]?", "bz"));
        assert!(!glob_match("*.png", "a.PNG"));
    }

    #[test]
    fn mime_matching() {
        assert!(mime_matches("image/*", "x.JPG"));
        assert!(!mime_matches("image/png", "x.jpg"));
        assert!(mime_matches("*/*", "noext"));
        assert!(!mime_matches("image/*", "noext"));
    }

    #[test]
    fn args_round_trip_basic() {
        let req = parse_args(&["--pick".into(), "open".into()]).unwrap();
        assert!(req.filters.is_empty());
        assert_eq!(req.filter_index, 0);
        assert!(parse_args(&["kestrel".into()]).is_none());
    }

    #[test]
    fn uris_are_encoded() {
        assert_eq!(
            uri(std::path::Path::new("/tmp/a b/é#.txt")),
            "file:///tmp/a%20b/%C3%A9%23.txt"
        );
    }
}
