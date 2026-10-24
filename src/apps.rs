//! "Open with": which installed apps handle a file, and launching them.
//!
//! File types come from the shared-mime-info database by name (fast, and what
//! file managers show), apps from the `.desktop` files in the XDG data dirs,
//! and defaults from `mimeapps.list`, like `xdg-mime`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct DesktopApp {
    /// Desktop file id, e.g. `org.gnome.TextEditor.desktop`.
    pub id: String,
    pub name: String,
    exec: String,
    terminal: bool,
    mimes: Vec<String>,
}

struct Db {
    loaded: Instant,
    /// (weight, pattern, mime), highest weight first.
    globs: Vec<(u32, String, String)>,
    parents: HashMap<String, Vec<String>>,
    aliases: HashMap<String, String>,
    apps: Vec<DesktopApp>,
    defaults: HashMap<String, Vec<String>>,
    added: HashMap<String, Vec<String>>,
}

/// Rescan at most this often, so newly installed apps show up.
const MAX_AGE: Duration = Duration::from_secs(30);

static DB: Mutex<Option<Db>> = Mutex::new(None);

fn with_db<T>(f: impl FnOnce(&Db) -> T) -> T {
    let mut guard = DB.lock().expect("lock poisoned");
    if guard.as_ref().is_none_or(|d| d.loaded.elapsed() > MAX_AGE) {
        *guard = Some(Db::load());
    }
    f(guard.get_or_insert_with(Db::load))
}

fn data_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let user = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share"));
    let system = std::env::var("XDG_DATA_DIRS").unwrap_or_default();
    let system: Vec<PathBuf> = if system.is_empty() {
        vec!["/usr/local/share".into(), "/usr/share".into()]
    } else {
        system
            .split(':')
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect()
    };
    std::iter::once(user).chain(system).collect()
}

fn config_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let user = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    let system = std::env::var("XDG_CONFIG_DIRS").unwrap_or_else(|_| "/etc/xdg".into());
    std::iter::once(user)
        .chain(
            system
                .split(':')
                .filter(|s| !s.is_empty())
                .map(PathBuf::from),
        )
        .collect()
}

impl Db {
    fn load() -> Self {
        let mut db = Db {
            loaded: Instant::now(),
            globs: Vec::new(),
            parents: HashMap::new(),
            aliases: HashMap::new(),
            apps: Vec::new(),
            defaults: HashMap::new(),
            added: HashMap::new(),
        };
        let dirs = data_dirs();
        for dir in dirs.iter().rev() {
            let mime = dir.join("mime");
            if let Ok(s) = std::fs::read_to_string(mime.join("globs2")) {
                for line in s.lines().filter(|l| !l.starts_with('#')) {
                    let mut it = line.split(':');
                    if let (Some(w), Some(m), Some(g)) = (it.next(), it.next(), it.next()) {
                        db.globs
                            .push((w.parse().unwrap_or(50), g.to_lowercase(), m.to_string()));
                    }
                }
            }
            if let Ok(s) = std::fs::read_to_string(mime.join("subclasses")) {
                for (child, parent) in s.lines().filter_map(|l| l.split_once(' ')) {
                    db.parents
                        .entry(child.to_string())
                        .or_default()
                        .push(parent.to_string());
                }
            }
            if let Ok(s) = std::fs::read_to_string(mime.join("aliases")) {
                for (alias, canon) in s.lines().filter_map(|l| l.split_once(' ')) {
                    db.aliases.insert(alias.to_string(), canon.to_string());
                }
            }
        }
        // Longer (more specific) patterns win ties, so `*.tar.gz` beats `*.gz`.
        db.globs
            .sort_by(|a, b| b.0.cmp(&a.0).then(b.1.len().cmp(&a.1.len())));

        // Earlier dirs (the user's) take precedence for the same id.
        let mut seen = HashSet::new();
        for dir in &dirs {
            scan_apps(&dir.join("applications"), "", &mut seen, &mut db.apps);
        }
        db.apps.sort_by_key(|a| a.name.to_lowercase());

        // Like xdg-mime: user config first, then system, then per-data-dir lists.
        let lists = config_dirs()
            .into_iter()
            .map(|d| d.join("mimeapps.list"))
            .chain(dirs.iter().map(|d| d.join("applications/mimeapps.list")));
        for list in lists {
            let Ok(s) = std::fs::read_to_string(list) else {
                continue;
            };
            let mut section = "";
            for line in s.lines().map(str::trim) {
                if line.starts_with('[') {
                    section = line;
                    continue;
                }
                let Some((mime, ids)) = line.split_once('=') else {
                    continue;
                };
                let target = match section {
                    "[Default Applications]" => &mut db.defaults,
                    "[Added Associations]" => &mut db.added,
                    _ => continue,
                };
                let entry = target.entry(mime.trim().to_string()).or_default();
                for id in ids.split(';').map(str::trim).filter(|i| !i.is_empty()) {
                    if !entry.iter().any(|e| e == id) {
                        entry.push(id.to_string());
                    }
                }
            }
        }
        db
    }

    fn canonical(&self, mime: &str) -> String {
        self.aliases
            .get(mime)
            .cloned()
            .unwrap_or_else(|| mime.to_string())
    }

    /// `mime` followed by its ancestors, nearest first.
    fn lineage(&self, mime: &str) -> Vec<String> {
        let mut out = vec![self.canonical(mime)];
        let mut i = 0;
        while i < out.len() {
            let mut next: Vec<String> = self.parents.get(&out[i]).cloned().unwrap_or_default();
            // Every text format can be edited as plain text.
            if out[i].starts_with("text/") && out[i] != "text/plain" {
                next.push("text/plain".into());
            }
            for p in next {
                let p = self.canonical(&p);
                if !out.contains(&p) {
                    out.push(p);
                }
            }
            i += 1;
        }
        out
    }

    fn mime_of(&self, path: &Path) -> String {
        if path.is_dir() {
            return "inode/directory".into();
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        for (_, glob, mime) in &self.globs {
            if glob_match(glob, &name) {
                return mime.clone();
            }
        }
        if looks_like_text(path) {
            "text/plain".into()
        } else {
            "application/octet-stream".into()
        }
    }
}

fn glob_match(glob: &str, name: &str) -> bool {
    if let Some(suffix) = glob.strip_prefix('*')
        && !suffix.contains(['*', '?', '['])
    {
        return name.ends_with(suffix);
    }
    if !glob.contains(['*', '?', '[']) {
        return glob == name;
    }
    false // rarer patterns (character classes etc.) aren't worth a matcher here
}

fn looks_like_text(path: &Path) -> bool {
    use std::io::Read;
    let mut buf = [0u8; 4096];
    let Ok(n) = std::fs::File::open(path).and_then(|mut f| f.read(&mut buf)) else {
        return false;
    };
    // A cut-off multibyte character at the end still counts as text.
    n > 0
        && !buf[..n].contains(&0)
        && std::str::from_utf8(&buf[..n]).map_or_else(|e| e.valid_up_to() + 4 > n, |_| true)
}

fn scan_apps(dir: &Path, prefix: &str, seen: &mut HashSet<String>, out: &mut Vec<DesktopApp>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for de in rd.flatten() {
        let path = de.path();
        let name = de.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            // Subfolders give ids like `kde-foo.desktop` (desktop entry spec).
            scan_apps(&path, &format!("{prefix}{name}-"), seen, out);
            continue;
        }
        if !name.ends_with(".desktop") {
            continue;
        }
        let id = format!("{prefix}{name}");
        if !seen.insert(id.clone()) {
            continue;
        }
        if let Some(app) = parse_desktop(&id, &path) {
            out.push(app);
        }
    }
}

fn parse_desktop(id: &str, path: &Path) -> Option<DesktopApp> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut in_entry = false;
    let (mut name, mut exec, mut mimes, mut terminal) = (None, None, Vec::new(), false);
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        match k.trim() {
            "Name" => name = Some(v.trim().to_string()),
            "Exec" => exec = Some(v.trim().to_string()),
            "MimeType" => {
                mimes = v
                    .split(';')
                    .map(str::trim)
                    .filter(|m| !m.is_empty())
                    .map(String::from)
                    .collect()
            }
            "Terminal" => terminal = v.trim() == "true",
            "Type" if v.trim() != "Application" => return None,
            "Hidden" if v.trim() == "true" => return None,
            _ => {}
        }
    }
    Some(DesktopApp {
        id: id.to_string(),
        name: name?,
        exec: exec?,
        terminal,
        mimes,
    })
}

pub struct Choices {
    pub mime: String,
    /// The default app first, then the others that handle the type.
    pub apps: Vec<DesktopApp>,
    pub default: Option<String>,
}

/// Apps that can open `path`, and its file type.
pub fn choices(path: &Path) -> Choices {
    with_db(|db| {
        let mime = db.mime_of(path);
        let lineage = db.lineage(&mime);
        let by_id = |id: &str| db.apps.iter().find(|a| a.id == id);
        let default = lineage
            .iter()
            .flat_map(|m| db.defaults.get(m).into_iter().flatten())
            .find(|id| by_id(id).is_some())
            .cloned();
        let mut apps: Vec<DesktopApp> =
            default.iter().filter_map(|id| by_id(id).cloned()).collect();
        // Exact type first, then apps for broader parent types.
        for m in &lineage {
            let added = db
                .added
                .get(m)
                .into_iter()
                .flatten()
                .filter_map(|id| by_id(id));
            let declared = db
                .apps
                .iter()
                .filter(|a| a.mimes.iter().any(|am| db.canonical(am) == *m));
            for a in added.chain(declared) {
                if !apps.iter().any(|x| x.id == a.id) {
                    apps.push(a.clone());
                }
            }
        }
        Choices {
            mime,
            apps,
            default,
        }
    })
}

/// Makes `app_id` the default for `mime` (writes `~/.config/mimeapps.list` via xdg-mime).
pub fn set_default(app_id: &str, mime: &str) -> Result<(), String> {
    let out = std::process::Command::new("xdg-mime")
        .args(["default", app_id, mime])
        .output()
        .map_err(|e| format!("xdg-mime: {e}"))?;
    *DB.lock().expect("lock poisoned") = None; // pick the change up on the next menu
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Splits an Exec value into arguments (desktop entry spec quoting).
fn split_exec(exec: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let (mut quoted, mut escape, mut any) = (false, false, false);
    for c in exec.chars() {
        if escape {
            cur.push(c);
            escape = false;
        } else if quoted && c == '\\' {
            escape = true;
        } else if c == '"' {
            quoted = !quoted;
            any = true;
        } else if c.is_whitespace() && !quoted {
            if any || !cur.is_empty() {
                args.push(std::mem::take(&mut cur));
                any = false;
            }
        } else {
            cur.push(c);
        }
    }
    if any || !cur.is_empty() {
        args.push(cur);
    }
    args
}

fn file_uri(p: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut s = String::from("file://");
    for &b in p.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
            s.push(b as char);
        } else {
            s += &format!("%{b:02X}");
        }
    }
    s
}

/// The command line that opens `paths` with `app`.
fn command_line(app: &DesktopApp, paths: &[PathBuf]) -> Vec<String> {
    let mut out = Vec::new();
    let mut used = false;
    for arg in split_exec(&app.exec) {
        match arg.as_str() {
            "%f" | "%u" => {
                // Single-file apps get the first file; `launch` starts one per extra file.
                if let Some(p) = paths.first() {
                    out.push(if arg == "%f" {
                        p.to_string_lossy().into_owned()
                    } else {
                        file_uri(p)
                    });
                }
                used = true;
            }
            "%F" => {
                out.extend(paths.iter().map(|p| p.to_string_lossy().into_owned()));
                used = true;
            }
            "%U" => {
                out.extend(paths.iter().map(|p| file_uri(p)));
                used = true;
            }
            "%i" | "%c" | "%k" => {}
            _ => {
                // Drop field codes embedded in an argument; %% is a literal percent.
                let mut cleaned = String::new();
                let mut chars = arg.chars();
                while let Some(c) = chars.next() {
                    if c != '%' {
                        cleaned.push(c);
                    } else if chars.next() == Some('%') {
                        cleaned.push('%');
                    }
                }
                if !cleaned.is_empty() {
                    out.push(cleaned);
                }
            }
        }
    }
    if !used {
        out.extend(paths.iter().map(|p| p.to_string_lossy().into_owned()));
    }
    out
}

/// Opens `paths` with `app`. Apps that take one file at a time get one launch per file.
pub fn launch(app: &DesktopApp, paths: &[PathBuf]) -> Result<(), String> {
    let single = split_exec(&app.exec).iter().any(|a| a == "%f" || a == "%u");
    let groups: Vec<Vec<PathBuf>> = if single {
        paths.iter().map(|p| vec![p.clone()]).collect()
    } else {
        vec![paths.to_vec()]
    };
    for group in groups {
        let mut argv = command_line(app, &group);
        if app.terminal {
            argv.insert(0, "xdg-terminal-exec".into());
        }
        let Some((prog, args)) = argv.split_first() else {
            return Err(format!("{} has no command", app.name));
        };
        use std::os::unix::process::CommandExt;
        let mut cmd = std::process::Command::new(prog);
        cmd.args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0); // keep running after Kestrel exits
        if let Some(dir) = group.first().and_then(|p| p.parent()) {
            cmd.current_dir(dir);
        }
        cmd.spawn()
            .map_err(|e| format!("Couldn't start {}: {e}", app.name))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(exec: &str) -> DesktopApp {
        DesktopApp {
            id: "t.desktop".into(),
            name: "T".into(),
            exec: exec.into(),
            terminal: false,
            mimes: vec![],
        }
    }

    #[test]
    fn exec_field_codes() {
        let files = [PathBuf::from("/a b/c.txt"), PathBuf::from("/d.txt")];
        assert_eq!(
            command_line(&app("ed %F"), &files),
            ["ed", "/a b/c.txt", "/d.txt"]
        );
        assert_eq!(
            command_line(&app("ed --new %f"), &files[..1]),
            ["ed", "--new", "/a b/c.txt"]
        );
        assert_eq!(
            command_line(&app("web %U"), &files[..1]),
            ["web", "file:///a%20b/c.txt"]
        );
        assert_eq!(
            command_line(&app("\"/opt/my app/run\" %i -x"), &files[..1]),
            ["/opt/my app/run", "-x", "/a b/c.txt"]
        );
        assert_eq!(command_line(&app("pct 100%%"), &[]), ["pct", "100%"]);
    }

    #[test]
    fn globs() {
        assert!(glob_match("*.md", "readme.md"));
        assert!(glob_match("makefile", "makefile"));
        assert!(!glob_match("*.md", "readme.mdx"));
    }
}
