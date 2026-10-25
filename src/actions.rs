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
}

pub fn file() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("kestrel/actions.conf"))
}

pub fn load() -> Vec<CustomAction> {
    file()
        .and_then(|f| std::fs::read_to_string(f).ok())
        .map(|text| parse(&text))
        .unwrap_or_default()
}

pub fn parse(text: &str) -> Vec<CustomAction> {
    let mut actions: Vec<CustomAction> = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            actions.push(CustomAction {
                name: name.trim().to_string(),
                ..Default::default()
            });
        } else if let (Some(action), Some((key, value))) =
            (actions.last_mut(), line.split_once('='))
        {
            let value = value.trim();
            match key.trim() {
                "match" => action.patterns = value.to_string(),
                "run" => action.run = value.to_string(),
                "terminal" => action.terminal = value == "true",
                _ => {}
            }
        }
    }
    actions.retain(|a| !a.name.is_empty() && !a.run.is_empty());
    actions
}

const PER_ITEM: [&str; 4] = ["{file}", "{name}", "{stem}", "{ext}"];

impl CustomAction {
    fn per_item(&self) -> bool {
        PER_ITEM.iter().any(|p| self.run.contains(p))
    }

    fn uses_selection(&self) -> bool {
        self.per_item() || self.run.contains("{files}")
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

/// Runs the action in the background, one command after another. Reports the outcome
/// on `done` as `Ok(message)` or `Err(message)`.
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
        let mut result = Ok(format!("Finished \"{name}\""));
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
            let out = c
                .current_dir(&cwd)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .output();
            match out {
                Ok(o) if o.status.success() => {}
                Ok(o) => {
                    let err = String::from_utf8_lossy(&o.stderr);
                    let first = err
                        .lines()
                        .find(|l| !l.trim().is_empty())
                        .unwrap_or("failed");
                    result = Err(format!("\"{name}\": {}", first.trim()));
                    break;
                }
                Err(e) => {
                    result = Err(format!("\"{name}\": {e}"));
                    break;
                }
            }
        }
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
}
