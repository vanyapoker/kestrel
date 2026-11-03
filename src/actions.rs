//! User-defined actions from `~/.config/kestrel/actions.conf`, shown in the command
//! palette and the context menu:
//!
//! ```text
//! # Lines starting with # are comments.
//! [Convert to WebP]
//! match = *.png, *.jpg
//! run = magick {file} {dir}/{stem}.webp
//!
//! [Edit in Neovim]
//! run = nvim {files}
//! terminal = true
//! ```
//!
//! `run` goes to `sh -c` in the current folder. Placeholders are replaced by quoted
//! paths, so don't quote them yourself: `{file}` (full path), `{name}`, `{stem}`
//! (name without extension), `{ext}`, `{files}` (all selected paths) and `{dir}`.
//! A command with a per-item placeholder (`{file}`, `{name}`, `{stem}`, `{ext}`) runs
//! once for each selected item, with `{dir}` being that item's folder. `match` limits
//! the action to selections where every name matches one of its patterns.
//!
//! The in-app editor reads the file as a [`Document`] and writes it back, keeping
//! comments and settings it doesn't know.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CustomAction {
    pub name: String,
    /// Comma-separated name patterns (see `util::glob_any`); empty matches anything.
    pub patterns: String,
    pub run: String,
    /// Run inside a terminal window (via `xdg-terminal-exec`).
    pub terminal: bool,
    /// Lines of the file that aren't settings, written back unchanged.
    pub kept: Kept,
}

/// Comments, blank lines and unknown settings of one action's section.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Kept {
    /// Above the `[Name]` line.
    before: Vec<String>,
    /// Inside the section; written after its settings.
    inside: Vec<String>,
}

/// The actions file as written: actions plus the lines around them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Document {
    pub actions: Vec<CustomAction>,
    /// Lines the reader couldn't use, as "Line 4: …".
    pub problems: Vec<String>,
    /// Comments above the first action and below the last.
    head: Vec<String>,
    tail: Vec<String>,
}

/// Placeholders `run` accepts, with what they become.
pub const PLACEHOLDERS: &[(&str, &str)] = &[
    (
        "{file}",
        "Each selected item's full path; runs once per item",
    ),
    ("{files}", "All selected paths, in one command"),
    ("{dir}", "The item's folder, or the current folder"),
    ("{name}", "Each item's name with extension"),
    ("{stem}", "Each item's name without extension"),
    ("{ext}", "Each item's extension, without the dot"),
];

pub fn file() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("kestrel/actions.conf"))
}

/// The actions file, or an empty document when there is none yet.
pub fn load_document(path: &Path) -> Document {
    std::fs::read_to_string(path)
        .map(|text| read(&text))
        .unwrap_or_default()
}

#[cfg(test)]
pub fn parse(text: &str) -> Vec<CustomAction> {
    runnable(read(text))
}

/// Actions that can run: every one with a name and a command.
pub fn runnable(doc: Document) -> Vec<CustomAction> {
    doc.actions
        .into_iter()
        .filter(|a| a.errors().is_empty())
        .collect()
}

/// Writes the document over the actions file, creating its folder if needed.
pub fn save(doc: &Document, path: &Path) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    // Write beside it, then rename, so a failed write never leaves half a file.
    let tmp = path.with_extension("conf.tmp");
    std::fs::write(&tmp, doc.to_text())
        .and_then(|()| std::fs::rename(&tmp, path))
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Reads every section, even incomplete ones, so the editor can show and fix them.
pub fn read(text: &str) -> Document {
    let mut doc = Document::default();
    // Comments and blank lines not yet attached to a section.
    let mut pending: Vec<String> = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let (n, raw, line) = (i + 1, raw.trim_end(), raw.trim());
        if line.is_empty() || line.starts_with('#') {
            pending.push(raw.to_string());
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            let before = std::mem::take(&mut pending);
            let mut action = CustomAction {
                name: name.trim().to_string(),
                ..Default::default()
            };
            // Comments above the first action describe the file, not that action.
            if doc.actions.is_empty() {
                doc.head = before;
            } else {
                action.kept.before = before;
            }
            doc.actions.push(action);
            continue;
        }
        let Some(action) = doc.actions.last_mut() else {
            doc.problems.push(format!(
                "Line {n}: “{line}” comes before any [Action name] line"
            ));
            pending.push(raw.to_string());
            continue;
        };
        action.kept.inside.append(&mut pending);
        let Some((key, value)) = line.split_once('=') else {
            doc.problems
                .push(format!("Line {n}: “{line}” isn't a setting like run = …"));
            action.kept.inside.push(raw.to_string());
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "match" => action.patterns = value.to_string(),
            "run" => action.run = value.to_string(),
            "terminal" => match parse_bool(value) {
                Some(on) => action.terminal = on,
                None => doc.problems.push(format!(
                    "Line {n}: terminal should be true or false, not “{value}”"
                )),
            },
            other => {
                doc.problems.push(format!(
                    "Line {n}: unknown setting “{other}” in [{}]",
                    action.name
                ));
                action.kept.inside.push(raw.to_string());
            }
        }
    }
    if doc.actions.is_empty() {
        doc.head.append(&mut pending);
    } else {
        doc.tail = pending;
    }
    doc
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" => Some(false),
        _ => None,
    }
}

impl Document {
    pub fn to_text(&self) -> String {
        let mut lines: Vec<String> = self.head.clone();
        for (i, a) in self.actions.iter().enumerate() {
            // Keep sections apart, without doubling a blank line that's already there.
            // The first one stays where the file put it, right under its comments.
            if i > 0
                && a.kept.before.is_empty()
                && lines.last().is_some_and(|l| !l.trim().is_empty())
            {
                lines.push(String::new());
            }
            // Moved sections bring their separator; don't stack it on another one.
            let at_break = lines.last().is_none_or(|l| l.trim().is_empty());
            let before = a.kept.before.iter();
            let skip = if at_break {
                a.kept
                    .before
                    .iter()
                    .take_while(|l| l.trim().is_empty())
                    .count()
            } else {
                0
            };
            lines.extend(before.skip(skip).cloned());
            lines.push(format!("[{}]", a.name.trim()));
            if !a.patterns.trim().is_empty() {
                lines.push(format!("match = {}", a.patterns.trim()));
            }
            lines.push(format!("run = {}", a.run.trim()));
            if a.terminal {
                lines.push("terminal = true".into());
            }
            lines.extend(a.kept.inside.iter().cloned());
        }
        lines.extend(self.tail.iter().cloned());
        lines.iter().map(|l| format!("{l}\n")).collect()
    }
}

/// Ready-made actions for an empty file.
pub fn examples() -> Vec<CustomAction> {
    [
        ("Edit in Neovim", "", "nvim {files}", true),
        (
            "Convert to WebP",
            "*.png, *.jpg, *.jpeg",
            "magick {file} {dir}/{stem}.webp",
            false,
        ),
        ("Make executable", "", "chmod +x {files}", false),
    ]
    .into_iter()
    .map(|(name, patterns, run, terminal)| CustomAction {
        name: name.into(),
        patterns: patterns.into(),
        run: run.into(),
        terminal,
        kept: Kept::default(),
    })
    .collect()
}

const PER_ITEM: [&str; 4] = ["{file}", "{name}", "{stem}", "{ext}"];

impl CustomAction {
    fn per_item(&self) -> bool {
        PER_ITEM.iter().any(|p| self.run.contains(p))
    }

    /// Whether the command uses the selected items at all.
    pub fn uses_selection(&self) -> bool {
        self.per_item() || self.run.contains("{files}")
    }

    /// Problems that keep the action from running or saving.
    pub fn errors(&self) -> Vec<String> {
        let mut errors = Vec::new();
        let name = self.name.trim();
        if name.is_empty() {
            errors.push("Give the action a name".to_string());
        } else if name.contains(['[', ']', '\n']) {
            errors.push("The name can't contain [ or ]".to_string());
        }
        if self.run.trim().is_empty() {
            errors.push("Add a command to run".to_string());
        } else if self.run.contains('\n') {
            errors.push("The command must fit on one line".to_string());
        }
        errors
    }

    /// Likely mistakes that still let the action run.
    pub fn warnings(&self) -> Vec<String> {
        unknown_placeholders(&self.run)
            .into_iter()
            .map(|p| format!("{p} isn't a placeholder, so it stays as written"))
            .collect()
    }

    /// How the command runs, in words.
    pub fn describe(&self) -> &'static str {
        if self.per_item() {
            "Runs once for each selected item, in that item's folder."
        } else if self.uses_selection() {
            "Runs once with all selected items, in the current folder."
        } else {
            "Runs in the current folder and works without a selection."
        }
    }

    /// Whether the action is offered for this selection.
    pub fn applies(&self, targets: &[PathBuf]) -> bool {
        if self.uses_selection() && targets.is_empty() {
            return false;
        }
        self.patterns.trim().is_empty()
            || targets
                .iter()
                .all(|t| crate::util::glob_any(&self.patterns, &file_name(t)))
    }

    /// The shell commands to run, each with the folder to run it in.
    pub fn commands(&self, targets: &[PathBuf], dir: &Path) -> Vec<(String, PathBuf)> {
        if !self.per_item() {
            let files = targets
                .iter()
                .map(|t| quote(t))
                .collect::<Vec<_>>()
                .join(" ");
            let cmd = expand(&self.run, |key| match key {
                "files" => Some(files.clone()),
                "dir" => Some(quote(dir)),
                _ => None,
            });
            return vec![(cmd, dir.to_path_buf())];
        }
        targets
            .iter()
            .map(|t| {
                let parent = t.parent().unwrap_or(dir);
                let name = file_name(t);
                let (stem, ext) = match name.rfind('.') {
                    Some(i) if i > 0 => (&name[..i], &name[i + 1..]),
                    _ => (name.as_str(), ""),
                };
                let cmd = expand(&self.run, |key| match key {
                    "files" | "file" => Some(quote(t)),
                    "name" => Some(quote(Path::new(&name))),
                    "stem" => Some(quote(Path::new(stem))),
                    "ext" => Some(quote(Path::new(ext))),
                    "dir" => Some(quote(parent)),
                    _ => None,
                });
                (cmd, parent.to_path_buf())
            })
            .collect()
    }
}

/// `{word}` placeholders that Kestrel doesn't fill; `${VAR}` is the shell's.
fn unknown_placeholders(run: &str) -> Vec<String> {
    let mut unknown: Vec<String> = Vec::new();
    for (start, _) in run.match_indices('{') {
        if run[..start].ends_with('$') {
            continue;
        }
        let Some(len) = run[start..].find('}') else {
            continue;
        };
        let key = &run[start..=start + len];
        let word = &key[1..key.len() - 1];
        if !word.is_empty()
            && word.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !PLACEHOLDERS.iter().any(|(p, _)| *p == key)
            && !unknown.iter().any(|u| u == key)
        {
            unknown.push(key.to_string());
        }
    }
    unknown
}

/// Fills `{key}` placeholders in one left-to-right pass, so text that comes from a
/// value (a file name containing `{dir}`) is never expanded again.
fn expand(template: &str, value: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        match rest
            .find('}')
            .and_then(|end| value(&rest[1..end]).map(|v| (end, v)))
        {
            Some((end, v)) => {
                out.push_str(&v);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('{');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Single-quotes `p` for `sh`.
fn quote(p: &Path) -> String {
    format!("'{}'", p.to_string_lossy().replace('\'', r"'\''"))
}

/// Runs the action in the background, one command after another; a failure doesn't
/// stop the rest. Reports the outcome on `done` as `Ok(message)` or `Err(message)`.
pub fn spawn(
    action: &CustomAction,
    targets: &[PathBuf],
    dir: &Path,
    done: Sender<Result<String, String>>,
    ctx: eframe::egui::Context,
) {
    let commands = action.commands(targets, dir);
    let (name, terminal) = (action.name.clone(), action.terminal);
    std::thread::spawn(move || {
        let total = commands.len();
        let mut failed = 0;
        let mut first_error = None;
        for (cmd, cwd) in commands {
            let mut c = if terminal {
                let mut c = Command::new("xdg-terminal-exec");
                c.args(["sh", "-c", &cmd]);
                c
            } else {
                let mut c = Command::new("sh");
                c.args(["-c", &cmd]);
                c
            };
            let error = match c
                .current_dir(&cwd)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .output()
            {
                Ok(o) if o.status.success() => continue,
                Ok(o) => String::from_utf8_lossy(&o.stderr)
                    .lines()
                    .find(|l| !l.trim().is_empty())
                    .map_or_else(|| format!("exited with {}", o.status), |l| l.trim().into()),
                Err(e) => e.to_string(),
            };
            failed += 1;
            first_error.get_or_insert(error);
        }
        let result = match first_error {
            None if total > 1 => Ok(format!("Finished \"{name}\" for {total} items")),
            None => Ok(format!("Finished \"{name}\"")),
            Some(error) if total > 1 => Err(format!(
                "\"{name}\" failed for {failed} of {total} items: {error}"
            )),
            Some(error) => Err(format!("\"{name}\": {error}")),
        };
        // A terminal returns when its window closes; there's nothing to announce.
        if !(terminal && result.is_ok()) {
            let _ = done.send(result);
            ctx.request_repaint();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONF: &str = "# my actions\n[Convert to WebP]\nmatch = *.png, *.jpg\nrun = magick {file} {dir}/{stem}.webp\n\n[Edit]\nrun = nvim {files}\nterminal = true\n[Broken]\nmatch = *.x\n";

    #[test]
    fn parses_sections_and_drops_incomplete_ones() {
        let actions = parse(CONF);
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0].name, "Convert to WebP");
        assert_eq!(actions[0].patterns, "*.png, *.jpg");
        assert!(actions[1].terminal);
    }

    #[test]
    fn applies_to_matching_selections_only() {
        let actions = parse(CONF);
        let png = vec![PathBuf::from("/a/x.PNG")];
        let txt = vec![PathBuf::from("/a/x.txt")];
        assert!(actions[0].applies(&png));
        assert!(!actions[0].applies(&txt));
        assert!(!actions[0].applies(&[]));
        assert!(actions[1].applies(&txt));
    }

    #[test]
    fn names_that_look_like_placeholders_are_not_expanded_again() {
        let action = CustomAction {
            name: "x".into(),
            run: "echo {file} {dir}".into(),
            ..Default::default()
        };
        let evil = PathBuf::from("/tmp/a;echo INJECTED;/{dir}.png");
        let cmds = action.commands(&[evil], Path::new("/cwd"));
        assert_eq!(
            cmds[0].0,
            "echo '/tmp/a;echo INJECTED;/{dir}.png' '/tmp/a;echo INJECTED;'"
        );
    }

    #[test]
    fn expands_placeholders_per_item_or_once() {
        let actions = parse(CONF);
        let files = vec![PathBuf::from("/a/it's.png"), PathBuf::from("/b/y.tar.jpg")];
        let cmds = actions[0].commands(&files, Path::new("/cwd"));
        assert_eq!(cmds[0].0, r"magick '/a/it'\''s.png' '/a'/'it'\''s'.webp");
        assert_eq!(cmds[0].1, PathBuf::from("/a"));
        assert_eq!(cmds[1].0, "magick '/b/y.tar.jpg' '/b'/'y.tar'.webp");
        let once = actions[1].commands(&files, Path::new("/cwd"));
        assert_eq!(once.len(), 1);
        assert_eq!(once[0].0, r"nvim '/a/it'\''s.png' '/b/y.tar.jpg'");
    }

    #[test]
    fn documents_round_trip_and_keep_comments_through_edits() {
        let text = "# Kestrel actions\n\n[Convert]\nmatch = *.png\nrun = magick {file} {dir}/{stem}.webp\n# keep me\nicon = image\n\n# Opens files\n[Edit]\nrun = nvim {files}\nterminal = true\n# end\n";
        let mut doc = read(text);
        assert_eq!(doc.to_text(), text);
        assert_eq!(
            doc.problems,
            ["Line 7: unknown setting “icon” in [Convert]"]
        );
        doc.actions[1].run = "hx {files}".into();
        doc.actions.swap(0, 1);
        doc.actions.push(CustomAction {
            name: "New".into(),
            run: "true".into(),
            ..Default::default()
        });
        let saved = doc.to_text();
        assert!(
            saved.starts_with("# Kestrel actions\n\n# Opens files\n[Edit]\nrun = hx {files}\n")
        );
        assert!(saved.contains("# keep me\nicon = image\n"));
        assert!(saved.ends_with("\n[New]\nrun = true\n# end\n"));
        assert_eq!(read(&saved).actions.len(), 3);
    }

    #[test]
    fn reading_reports_problems_instead_of_dropping_them() {
        let doc =
            read("run = orphan\n[A]\nterminal = YES\n[B]\nrun = x\nterminal = maybe\nnonsense\n");
        assert_eq!(doc.actions.len(), 2);
        assert!(doc.actions[0].terminal);
        assert!(!doc.actions[1].terminal);
        assert_eq!(doc.problems.len(), 3, "{:?}", doc.problems);
        assert!(doc.problems[0].starts_with("Line 1:"));
        assert!(doc.problems[1].contains("not “maybe”"));
        assert_eq!(doc.actions[0].errors(), ["Add a command to run"]);
        // Incomplete actions are shown in the editor but never run.
        assert_eq!(runnable(doc).len(), 1);
    }

    #[test]
    fn unknown_placeholders_are_warnings_but_shell_variables_are_not() {
        let action = CustomAction {
            name: "x".into(),
            run: "cp {flie} ${HOME}/{dir} {a,b} {flie}".into(),
            ..Default::default()
        };
        assert!(action.errors().is_empty());
        assert_eq!(
            action.warnings(),
            ["{flie} isn't a placeholder, so it stays as written"]
        );
        assert!(
            examples()
                .iter()
                .all(|a| a.errors().is_empty() && a.warnings().is_empty())
        );
    }

    #[test]
    fn per_item_failures_are_counted_without_stopping() {
        let dir = std::env::temp_dir().join(format!("kestrel-actions-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b, c) = (dir.join("a"), dir.join("missing"), dir.join("c"));
        std::fs::write(&a, "").unwrap();
        std::fs::write(&c, "").unwrap();
        let action = CustomAction {
            name: "Check".into(),
            run: "test -e {file} && touch {file}.seen".into(),
            ..Default::default()
        };
        let (tx, rx) = std::sync::mpsc::channel();
        spawn(
            &action,
            &[a, b, c.clone()],
            &dir,
            tx,
            eframe::egui::Context::default(),
        );
        let result = rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        assert_eq!(
            result.unwrap_err(),
            "\"Check\" failed for 1 of 3 items: exited with exit status: 1"
        );
        assert!(dir.join("c.seen").exists(), "later items still ran");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
