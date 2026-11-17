//! Git integration: status marks, lazygit, init and clone.
use super::*;

pub(super) struct GitListing {
    pub(super) listing_id: u64,
    pub(super) generation: u64,
    pub(super) path: PathBuf,
    pub(super) marks: Option<Arc<crate::git::Marks>>,
}

/// Computes git status for `tab` on a thread; coalesces runs per listing.
pub(super) fn start_git(
    tab: &Tab,
    inflight: &mut HashMap<u64, bool>,
    tx: &Sender<GitListing>,
    ctx: &egui::Context,
) {
    if let Some(again) = inflight.get_mut(&tab.listing_id) {
        *again = true;
        return;
    }
    inflight.insert(tab.listing_id, false);
    let (listing_id, generation, path) = (tab.listing_id, tab.generation, tab.path.clone());
    let (tx, ctx) = (tx.clone(), ctx.clone());
    std::thread::spawn(move || {
        let marks = crate::git::folder_marks(&path).map(Arc::new);
        let _ = tx.send(GitListing {
            listing_id,
            generation,
            path,
            marks,
        });
        ctx.request_repaint();
    });
}

impl App {
    pub(super) fn refresh_git(&mut self) {
        let Self {
            panes,
            git_inflight,
            git_tx,
            ctx,
            ..
        } = self;
        for tab in panes.iter().flat_map(|p| &p.tabs) {
            if tab.search.is_none() {
                start_git(tab, git_inflight, git_tx, ctx);
            }
            if let Some(cols) = &tab.columns {
                for (i, item) in cols.items.iter().enumerate() {
                    if i != cols.active {
                        start_git(item, git_inflight, git_tx, ctx);
                    }
                }
            }
        }
    }

    pub(super) fn clear_git(&mut self) {
        for tab in self.panes.iter_mut().flat_map(|p| &mut p.tabs) {
            tab.git = None;
            if let Some(cols) = &mut tab.columns {
                for item in &mut cols.items {
                    item.git = None;
                }
            }
        }
    }
}

impl App {
    pub(super) fn spawn_in_terminal(&mut self, cwd: &Path, args: &[&std::ffi::OsStr]) {
        match std::process::Command::new("xdg-terminal-exec")
            .args(args)
            .current_dir(cwd)
            .spawn()
        {
            Ok(child) => fsops::reap(child),
            Err(e) => self.toast_error(format!("Could not open terminal: {e}")),
        }
    }

    pub(super) fn open_lazygit(&mut self, root: &Path, filter: Option<&Path>) {
        if !crate::git::on_path("lazygit") {
            self.toast_error("lazygit is not installed");
            return;
        }
        let mut args: Vec<&std::ffi::OsStr> = vec!["lazygit".as_ref()];
        if let Some(f) = filter {
            args.push("-f".as_ref());
            args.push(f.as_os_str());
        }
        self.spawn_in_terminal(root, &args);
    }

    pub(super) fn git_init(&mut self, dir: PathBuf) {
        let (tx, ctx) = (self.action_tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let out = std::process::Command::new("git")
                .arg("init")
                .current_dir(&dir)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .output();
            let result = match out {
                Ok(o) if o.status.success() => {
                    let name = dir
                        .file_name()
                        .map_or_else(|| dir.display().to_string(), |n| n.to_string_lossy().into());
                    Ok(format!("Initialized git repository in \"{name}\""))
                }
                Ok(o) => Err(String::from_utf8_lossy(&o.stderr)
                    .lines()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("git init failed")
                    .to_string()),
                Err(e) => Err(format!("git init failed: {e}")),
            };
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }

    pub(super) fn open_clone_dialog(&mut self, dir: PathBuf) {
        let url = self
            .wl
            .as_ref()
            .and_then(|wl| wl.read_clipboard_text())
            .map(|t| t.trim().to_string())
            .filter(|t| crate::git::looks_like_url(t))
            .unwrap_or_default();
        let name = crate::git::repo_name(&url);
        self.dialog = Some(Dialog::CloneRepo {
            dir,
            url,
            name,
            name_edited: false,
            focus: true,
        });
        self.ctx
            .memory_mut(|m| m.request_focus(egui::Id::new("dialog_name")));
    }
}
