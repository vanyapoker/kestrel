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
    Some(Request {
        kind,
        multiple: args.iter().any(|a| a == "--multiple"),
        title: value("--title").unwrap_or_default(),
        accept_label: value("--accept-label").unwrap_or_default(),
        dir: value("--dir").map(PathBuf::from).filter(|p| p.is_dir()),
        name: value("--name").unwrap_or_default(),
    })
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
        };
        let back = parse_args(&req.to_args()).unwrap();
        assert_eq!(
            (back.kind, back.multiple, back.title, back.dir, back.name),
            (req.kind, true, req.title, req.dir, req.name)
        );
        assert_eq!(back.accept_label, req.accept_label);
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
