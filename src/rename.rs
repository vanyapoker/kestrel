//! Batch rename planning and transactional application. All filesystem work runs
//! on a planner or the file-operation worker, never while drawing the dialog.

use crate::fsops::Entry;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{
    Arc,
    mpsc::{self, Receiver, Sender},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Case {
    #[default]
    Keep,
    Lower,
    Upper,
    Title,
}

#[derive(Clone)]
pub struct Options {
    pub template: String,
    pub find: String,
    pub replace: String,
    pub case: Case,
    pub start: u64,
    pub digits: usize,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            template: "{name}{ext}".into(),
            find: String::new(),
            replace: String::new(),
            case: Case::Keep,
            start: 1,
            digits: 3,
        }
    }
}

fn new_name(e: &Entry, options: &Options, i: usize) -> Result<String, String> {
    let path = Path::new(&e.name);
    let (stem, ext) = if e.is_dir {
        (e.name.clone(), String::new())
    } else {
        (
            path.file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            path.extension()
                .map(|e| format!(".{}", e.to_string_lossy()))
                .unwrap_or_default(),
        )
    };
    let stem = if options.find.is_empty() {
        stem
    } else {
        stem.replace(&options.find, &options.replace)
    };
    let stem = match options.case {
        Case::Keep => stem,
        Case::Lower => stem.to_lowercase(),
        Case::Upper => stem.to_uppercase(),
        Case::Title => {
            let mut start = true;
            let mut title = String::new();
            for c in stem.chars() {
                title.extend(if start {
                    c.to_uppercase().collect::<Vec<_>>()
                } else {
                    c.to_lowercase().collect()
                });
                start = !c.is_alphanumeric();
            }
            title
        }
    };
    let n = options
        .start
        .checked_add(i as u64)
        .ok_or("Numbering overflow")?;
    let number = format!("{n:0width$}", width = options.digits.min(12));
    let date = e
        .modified
        .map(|t| {
            chrono::DateTime::<chrono::Local>::from(t)
                .format("%Y-%m-%d")
                .to_string()
        })
        .unwrap_or_default();
    let mut name = String::new();
    let mut chars = options.template.chars();
    while let Some(c) = chars.next() {
        if c != '{' {
            name.push(c);
            continue;
        }
        let mut token = String::new();
        let mut closed = false;
        for c in chars.by_ref() {
            if c == '}' {
                closed = true;
                break;
            }
            token.push(c);
        }
        if !closed {
            return Err("Unclosed template placeholder".into());
        }
        name.push_str(match token.as_str() {
            "name" => &stem,
            "ext" => &ext,
            "n" => &number,
            "date" => &date,
            _ => return Err(format!("Unknown placeholder {{{token}}}")),
        });
    }
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains(['/', '\0'])
        || name.len() > 255
    {
        return Err("Invalid filename".into());
    }
    Ok(name)
}

pub struct Row {
    pub from: PathBuf,
    pub to: PathBuf,
    pub error: Option<String>,
}
pub struct Plan {
    pub rows: Vec<Row>,
}
impl Plan {
    pub fn build(entries: &[Entry], options: &Options) -> Self {
        let mut rows: Vec<_> = entries
            .iter()
            .enumerate()
            .map(|(i, e)| match new_name(e, options, i) {
                Ok(name) => Row {
                    from: e.path.clone(),
                    to: e.path.with_file_name(name),
                    error: None,
                },
                Err(error) => Row {
                    from: e.path.clone(),
                    to: e.path.clone(),
                    error: Some(error),
                },
            })
            .collect();
        let sources: HashSet<_> = rows
            .iter()
            .filter(|r| r.from != r.to)
            .map(|r| r.from.clone())
            .collect();
        let all_sources: HashSet<_> = rows.iter().map(|r| r.from.clone()).collect();
        let mut counts = HashMap::new();
        for row in &rows {
            *counts.entry(row.to.clone()).or_insert(0) += 1;
        }
        for row in &mut rows {
            if row.error.is_some() {
                continue;
            }
            row.error = if counts[&row.to] > 1 {
                Some("Several items would have this name".into())
            } else if row
                .from
                .ancestors()
                .skip(1)
                .any(|p| all_sources.contains(p))
            {
                Some("Rename parent and child folders separately".into())
            } else if std::fs::symlink_metadata(&row.from).is_err() {
                Some("Source is no longer available".into())
            } else if row.from != row.to
                && !sources.contains(&row.to)
                && std::fs::symlink_metadata(&row.to).is_ok()
            {
                Some("Destination already exists".into())
            } else {
                None
            };
        }
        Self { rows }
    }
    pub fn changes(&self) -> Vec<(PathBuf, PathBuf)> {
        self.rows
            .iter()
            .filter(|r| r.from != r.to)
            .map(|r| (r.from.clone(), r.to.clone()))
            .collect()
    }
    pub fn valid(&self) -> bool {
        self.rows.iter().all(|r| r.error.is_none()) && self.rows.iter().any(|r| r.from != r.to)
    }
}

pub struct Editor {
    pub options: Options,
    pub plan: Option<Plan>,
    entries: Arc<Vec<Entry>>,
    tx: Sender<(u64, Options, Arc<Vec<Entry>>)>,
    rx: Receiver<(u64, Plan)>,
    generation: u64,
}
impl Editor {
    pub fn new(entries: Vec<Entry>, ctx: eframe::egui::Context) -> Self {
        let (tx, requests) = mpsc::channel::<(u64, Options, Arc<Vec<Entry>>)>();
        let (results, rx) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(mut request) = requests.recv() {
                while let Ok(newer) = requests.try_recv() {
                    request = newer;
                }
                let (id, options, entries) = request;
                let plan = Plan::build(&entries, &options);
                if results.send((id, plan)).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        });
        let mut editor = Self {
            options: Options::default(),
            plan: None,
            entries: Arc::new(entries),
            tx,
            rx,
            generation: 0,
        };
        editor.request();
        editor
    }
    pub fn request(&mut self) {
        self.generation += 1;
        self.plan = None;
        let _ = self
            .tx
            .send((self.generation, self.options.clone(), self.entries.clone()));
    }
    pub fn poll(&mut self) {
        while let Ok((id, plan)) = self.rx.try_recv() {
            if id == self.generation {
                self.plan = Some(plan);
            }
        }
    }
}

fn rename_no_replace(from: &Path, to: &Path) -> Result<(), String> {
    use std::os::unix::ffi::OsStrExt;
    let from_c = std::ffi::CString::new(from.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    let to_c = std::ffi::CString::new(to.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    // SAFETY: both paths are NUL-terminated and outlive the call.
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from_c.as_ptr(),
            libc::AT_FDCWD,
            to_c.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(format!(
            "{} → {}: {}",
            from.display(),
            to.display(),
            std::io::Error::last_os_error()
        ))
    }
}

/// Stage all sources beside their originals, then install every new name using
/// RENAME_NOREPLACE. This supports cycles and never replaces a concurrently-created file.
pub fn apply(pairs: &[(PathBuf, PathBuf)]) -> Result<(), String> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let sources: HashSet<_> = pairs.iter().map(|(from, _)| from).collect();
    if sources.len() != pairs.len() {
        return Err("Duplicate rename source".into());
    }
    let mut targets = HashSet::new();
    for (from, to) in pairs {
        if from == to || from.parent() != to.parent() || !targets.insert(to) {
            return Err("Invalid or duplicate rename destination".into());
        }
        if from
            .ancestors()
            .skip(1)
            .any(|p| sources.contains(&p.to_path_buf()))
        {
            return Err("Rename parent and child folders separately".into());
        }
        if std::fs::symlink_metadata(from).is_err() {
            return Err(format!("{} is no longer available", from.display()));
        }
        if !sources.contains(to) && std::fs::symlink_metadata(to).is_ok() {
            return Err(format!("{} already exists", to.display()));
        }
    }
    let mut staged: Vec<(PathBuf, PathBuf, PathBuf)> = Vec::new();
    for (from, to) in pairs {
        let mut temporary;
        loop {
            temporary = from.with_file_name(format!(
                ".kestrel-rename-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            if std::fs::symlink_metadata(&temporary).is_err() {
                break;
            }
        }
        if let Err(error) = rename_no_replace(from, &temporary) {
            return Err(rollback(&staged, error));
        }
        staged.push((from.clone(), temporary, to.clone()));
    }
    for i in 0..staged.len() {
        if let Err(mut error) = rename_no_replace(&staged[i].1, &staged[i].2) {
            // Re-stage installed names before restoring originals, including cycles.
            for (_, temporary, destination) in staged.iter().take(i).rev() {
                if let Err(e) = rename_no_replace(destination, temporary) {
                    error.push_str(&format!("; rollback: {e}"));
                }
            }
            return Err(rollback(&staged, error));
        }
    }
    Ok(())
}

fn rollback(staged: &[(PathBuf, PathBuf, PathBuf)], mut error: String) -> String {
    for (original, temporary, _) in staged.iter().rev() {
        if let Err(e) = rename_no_replace(temporary, original) {
            error.push_str(&format!("; recovery file {}: {e}", temporary.display()));
        }
    }
    error
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fixture(name: &str) -> (PathBuf, Vec<Entry>) {
        let root =
            std::env::temp_dir().join(format!("kestrel-rename-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        for name in ["Original Photo.JPG", "another.JPG"] {
            fs::write(root.join(name), name).unwrap();
        }
        let mut entries = crate::fsops::list_dir(&root).unwrap();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        (root, entries)
    }

    #[test]
    fn preview_supports_replacement_case_numbers_and_file_dates() {
        let (root, mut entries) = fixture("format");
        entries[0].modified = Some(std::time::UNIX_EPOCH);
        let options = Options {
            template: "{date}-{n}-{name}{ext}".into(),
            find: "Original".into(),
            replace: "Holiday".into(),
            case: Case::Lower,
            start: 5,
            digits: 2,
        };
        let plan = Plan::build(&entries, &options);
        assert!(plan.valid());
        let date = chrono::DateTime::<chrono::Local>::from(std::time::UNIX_EPOCH)
            .format("%Y-%m-%d")
            .to_string();
        assert_eq!(
            plan.rows[0].to,
            root.join(format!("{date}-05-holiday photo.JPG"))
        );
        assert!(
            root.join("Original Photo.JPG").exists(),
            "preview must not rename anything"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn preview_rejects_collisions_invalid_names_and_nested_selections() {
        let (root, entries) = fixture("conflicts");
        let mut options = Options {
            template: "same{ext}".into(),
            ..Default::default()
        };
        assert!(
            Plan::build(&entries, &options)
                .rows
                .iter()
                .all(|r| r.error.is_some())
        );
        fs::write(root.join("existing.JPG"), "keep").unwrap();
        options.template = "existing{ext}".into();
        assert!(!Plan::build(&entries[..1], &options).valid());
        for invalid in ["../{name}", "", "{unknown}", "{name"] {
            options.template = invalid.into();
            assert!(!Plan::build(&entries, &options).valid());
        }
        fs::create_dir(root.join("parent")).unwrap();
        fs::write(root.join("parent/child"), "child").unwrap();
        let nested: Vec<_> = [root.join("parent"), root.join("parent/child")]
            .into_iter()
            .map(|p| {
                crate::fsops::make_entry(
                    p.clone(),
                    p.file_name().unwrap().to_string_lossy().into_owned(),
                    fs::metadata(&p).unwrap(),
                )
            })
            .collect();
        options.template = "{n}-{name}{ext}".into();
        assert!(!Plan::build(&nested, &options).valid());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cyclic_renames_reverse_without_overwriting() {
        let (root, entries) = fixture("cycle");
        let a = entries[0].path.clone();
        let b = entries[1].path.clone();
        let original_a = fs::read(&a).unwrap();
        let original_b = fs::read(&b).unwrap();
        let pairs = vec![(a.clone(), b.clone()), (b.clone(), a.clone())];
        apply(&pairs).unwrap();
        assert_eq!(fs::read(&a).unwrap(), original_b);
        assert_eq!(fs::read(&b).unwrap(), original_a);
        apply(
            &pairs
                .iter()
                .map(|(a, b)| (b.clone(), a.clone()))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert_eq!(fs::read(&a).unwrap(), original_a);
        assert_eq!(fs::read(&b).unwrap(), original_b);
        fs::write(root.join("occupied"), "keep").unwrap();
        assert!(apply(&[(a.clone(), root.join("occupied"))]).is_err());
        assert_eq!(fs::read_to_string(root.join("occupied")).unwrap(), "keep");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn installation_failure_rolls_back_all_staged_names() {
        let (root, entries) = fixture("rollback");
        let a = entries[0].path.clone();
        let b = entries[1].path.clone();
        let error = apply(&[
            (a.clone(), root.join("new-name")),
            (b.clone(), root.join("x".repeat(300))),
        ]);
        assert!(error.is_err());
        assert!(a.exists() && b.exists());
        assert_eq!(
            fs::read_dir(&root).unwrap().count(),
            2,
            "no temporary or installed names should remain"
        );
        fs::remove_dir_all(root).unwrap();
    }
}
