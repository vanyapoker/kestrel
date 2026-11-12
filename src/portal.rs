//! `kestrel --portal`: an xdg-desktop-portal backend for `org.freedesktop.impl.portal.FileChooser`,
//! so file dialogs in Firefox and other apps open Kestrel. Each request runs
//! `kestrel --pick ...` as a child process and reads the chosen paths from its stdout.

use crate::picker::{self, Filter, Kind, Request};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use zbus::zvariant::{ObjectPath, OwnedValue, Value};

pub const BUS_NAME: &str = "org.freedesktop.impl.portal.desktop.kestrel";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";

/// Pickers in flight: request handle -> child pid, so `Request.Close` can cancel them.
fn running() -> &'static Mutex<HashMap<String, u32>> {
    static RUNNING: OnceLock<Mutex<HashMap<String, u32>>> = OnceLock::new();
    RUNNING.get_or_init(Default::default)
}

type Results = HashMap<String, OwnedValue>;

struct RequestObj {
    handle: String,
}

#[zbus::interface(name = "org.freedesktop.impl.portal.Request")]
impl RequestObj {
    fn close(&self) {
        if let Some(pid) = running()
            .lock()
            .expect("lock poisoned")
            .remove(&self.handle)
        {
            // SAFETY: `kill` has no memory-safety preconditions.
            unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        }
    }
}

struct FileChooser;

#[zbus::interface(name = "org.freedesktop.impl.portal.FileChooser")]
impl FileChooser {
    async fn open_file(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
        handle: ObjectPath<'_>,
        app_id: &str,
        parent_window: &str,
        title: &str,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, Results) {
        let kind = if flag(&options, "directory") {
            Kind::Folder
        } else {
            Kind::Open
        };
        let req = request(kind, title, &options, app_id, parent_window);
        serve(conn, handle, req).await
    }

    async fn save_file(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
        handle: ObjectPath<'_>,
        app_id: &str,
        parent_window: &str,
        title: &str,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, Results) {
        let req = request(Kind::Save, title, &options, app_id, parent_window);
        serve(conn, handle, req).await
    }
}

fn flag(options: &HashMap<String, OwnedValue>, key: &str) -> bool {
    options
        .get(key)
        .and_then(|v| bool::try_from(&**v).ok())
        .unwrap_or(false)
}

fn request(
    kind: Kind,
    title: &str,
    options: &HashMap<String, OwnedValue>,
    app: &str,
    parent: &str,
) -> Request {
    let filters = if kind == Kind::Folder {
        Vec::new()
    } else {
        filters(options)
    };
    let filter_index = current_filter(options)
        .and_then(|c| filters.iter().position(|f| *f == c))
        .unwrap_or(0);
    let text = |key: &str| {
        options
            .get(key)
            .and_then(|v| <&str>::try_from(&**v).ok())
            .map(str::to_owned)
    };
    // `current_folder` is a NUL-terminated byte string.
    let dir = byte_path(options, "current_folder");
    let current_file = (kind == Kind::Save)
        .then(|| byte_path(options, "current_file"))
        .flatten();
    Request {
        kind,
        multiple: kind != Kind::Save && flag(options, "multiple"),
        title: title.to_owned(),
        accept_label: text("accept_label").unwrap_or_default().replace('_', ""),
        dir: current_file
            .as_ref()
            .and_then(|p| p.parent())
            .map(PathBuf::from)
            .or(dir)
            .filter(|d| d.is_dir()),
        name: current_file
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .or_else(|| text("current_name"))
            .unwrap_or_default(),
        filters,
        filter_index,
        parent: parent.to_owned(),
        app: app.to_owned(),
    }
}

type RawFilter = (String, Vec<(u32, String)>);

fn filters(options: &HashMap<String, OwnedValue>) -> Vec<Filter> {
    let Some(v) = options.get("filters") else {
        return Vec::new();
    };
    let Some(raw) = (**v)
        .try_clone()
        .ok()
        .and_then(|v| <Vec<RawFilter>>::try_from(v).ok())
    else {
        return Vec::new();
    };
    raw.into_iter()
        .map(|(name, rules)| Filter { name, rules })
        .collect()
}

fn current_filter(options: &HashMap<String, OwnedValue>) -> Option<Filter> {
    let v = options.get("current_filter")?;
    let (name, rules) = RawFilter::try_from((**v).try_clone().ok()?).ok()?;
    Some(Filter { name, rules })
}

fn byte_path(options: &HashMap<String, OwnedValue>, key: &str) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let value = options.get(key)?;
    let bytes = Vec::<u8>::try_from((**value).try_clone().ok()?).ok()?;
    let bytes = bytes.strip_suffix(&[0]).unwrap_or(&bytes);
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
}

/// Runs a picker and answers the portal: 0 with `uris` when something was chosen, 1 when cancelled.
async fn serve(conn: &zbus::Connection, handle: ObjectPath<'_>, req: Request) -> (u32, Results) {
    let handle = handle.to_string();
    // Registered for the length of the request so the portal can cancel it.
    let _ = conn
        .object_server()
        .at(
            handle.as_str(),
            RequestObj {
                handle: handle.clone(),
            },
        )
        .await;
    let (tx, rx) = async_channel::bounded(1);
    let key = handle.clone();
    let child_req = req.clone();
    std::thread::spawn(move || {
        let _ = tx.send_blocking(run_picker(&child_req, &key));
    });
    let (paths, filter) = rx.recv().await.unwrap_or_default();
    running().lock().expect("lock poisoned").remove(&handle);
    let _ = conn
        .object_server()
        .remove::<RequestObj, _>(handle.as_str())
        .await;

    if paths.is_empty() {
        return (1, Results::new());
    }
    let uris: Vec<String> = paths.iter().map(|p| picker::uri(p)).collect();
    let mut results = Results::new();
    if let Ok(v) = OwnedValue::try_from(Value::from(uris)) {
        results.insert("uris".into(), v);
    }
    if let Some(f) = filter.and_then(|i| req.filters.get(i)) {
        let raw: RawFilter = (f.name.clone(), f.rules.clone());
        if let Ok(v) = OwnedValue::try_from(Value::from(raw)) {
            results.insert("current_filter".into(), v);
        }
    }
    (0, results)
}

/// Returns the chosen paths and, when the picker reported one, the index of the filter in use.
fn run_picker(req: &Request, key: &str) -> (Vec<PathBuf>, Option<usize>) {
    let mut exe = std::env::current_exe().unwrap_or_else(|_| "kestrel".into());
    // After an update the running binary has been replaced on disk.
    if let Some(s) = exe.to_str().and_then(|s| s.strip_suffix(" (deleted)")) {
        exe = s.into();
    }
    let child = Command::new(exe)
        .args(req.to_args())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn();
    let Ok(child) = child else {
        eprintln!("kestrel: could not start the picker");
        return (Vec::new(), None);
    };
    running()
        .lock()
        .expect("lock poisoned")
        .insert(key.to_owned(), child.id());
    let Ok(out) = child.wait_with_output() else {
        return (Vec::new(), None);
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let paths = text
        .lines()
        .filter(|l| l.starts_with('/'))
        .map(PathBuf::from)
        .collect();
    let filter = text
        .lines()
        .find_map(|l| l.strip_prefix("#filter "))
        .and_then(|n| n.trim().parse().ok());
    (paths, filter)
}

/// Serves the backend until the process is killed.
pub fn run() -> zbus::Result<()> {
    let _conn = zbus::blocking::connection::Builder::session()?
        .name(BUS_NAME)?
        .serve_at(PORTAL_PATH, FileChooser)?
        .build()?;
    loop {
        std::thread::park();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_existing_file_uses_its_parent_and_name() {
        let dir = std::env::temp_dir();
        let file = dir.join("existing.txt");
        let mut bytes = file.as_os_str().as_encoded_bytes().to_vec();
        bytes.push(0);
        let options = HashMap::from([
            (
                "current_file".into(),
                OwnedValue::try_from(Value::from(bytes)).unwrap(),
            ),
            (
                "current_name".into(),
                OwnedValue::from(zbus::zvariant::Str::from("ignored.txt")),
            ),
            (
                "accept_label".into(),
                OwnedValue::from(zbus::zvariant::Str::from("_Export")),
            ),
        ]);
        let req = request(Kind::Save, "Export file", &options, "", "");
        assert_eq!(req.dir, Some(dir));
        assert_eq!(req.name, "existing.txt");
        assert_eq!(req.accept_label, "Export");
        assert_eq!(req.title, "Export file");
    }

    #[test]
    fn folder_picker_honors_multiple_selection() {
        let options = HashMap::from([("multiple".into(), OwnedValue::from(true))]);
        assert!(request(Kind::Folder, "Folders", &options, "", "").multiple);
        assert!(!request(Kind::Save, "Save", &options, "", "").multiple);
    }

    #[test]
    fn filters_and_current_filter_are_read() {
        let images = ("Images".to_string(), vec![(0u32, "*.png".to_string())]);
        let text = ("Text".to_string(), vec![(0u32, "*.txt".to_string())]);
        let options = HashMap::from([
            (
                "filters".into(),
                OwnedValue::try_from(Value::from(vec![images.clone(), text.clone()])).unwrap(),
            ),
            (
                "current_filter".into(),
                OwnedValue::try_from(Value::from(text)).unwrap(),
            ),
        ]);
        let req = request(Kind::Open, "Open", &options, "app", "wayland:h");
        assert_eq!(req.filters.len(), 2);
        assert_eq!(req.filter_index, 1);
        assert_eq!((req.app.as_str(), req.parent.as_str()), ("app", "wayland:h"));
        assert!(request(Kind::Folder, "F", &options, "", "").filters.is_empty());
    }
}
