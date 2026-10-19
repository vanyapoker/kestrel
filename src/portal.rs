//! `kestrel --portal`: an xdg-desktop-portal backend for `org.freedesktop.impl.portal.FileChooser`,
//! so file dialogs in Firefox and other apps open Kestrel. Each request runs
//! `kestrel --pick ...` as a child process and reads the chosen paths from its stdout.

use crate::picker::{self, Kind, Request};
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
        if let Some(pid) = running().lock().unwrap().remove(&self.handle) {
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
        _app_id: &str,
        _parent_window: &str,
        title: &str,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, Results) {
        let kind = if flag(&options, "directory") { Kind::Folder } else { Kind::Open };
        let req = request(kind, title, &options);
        serve(conn, handle, req).await
    }

    async fn save_file(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
        handle: ObjectPath<'_>,
        _app_id: &str,
        _parent_window: &str,
        title: &str,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, Results) {
        let req = request(Kind::Save, title, &options);
        serve(conn, handle, req).await
    }
}

fn flag(options: &HashMap<String, OwnedValue>, key: &str) -> bool {
    options.get(key).and_then(|v| bool::try_from(&**v).ok()).unwrap_or(false)
}

fn request(kind: Kind, title: &str, options: &HashMap<String, OwnedValue>) -> Request {
    let text = |key: &str| options.get(key).and_then(|v| <&str>::try_from(&**v).ok()).map(str::to_owned);
    // `current_folder` is a NUL-terminated byte string.
    let dir = options.get("current_folder").and_then(|v| {
        let bytes: Vec<u8> = <Vec<u8>>::try_from((**v).try_clone().ok()?).ok()?;
        let bytes = bytes.strip_suffix(&[0]).unwrap_or(&bytes);
        {
            use std::os::unix::ffi::OsStrExt;
            Some(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
        }
    });
    Request {
        kind,
        multiple: kind == Kind::Open && flag(options, "multiple"),
        title: title.to_owned(),
        dir: dir.filter(|d| d.is_dir()),
        name: text("current_name").unwrap_or_default(),
    }
}

/// Runs a picker and answers the portal: 0 with `uris` when something was chosen, 1 when cancelled.
async fn serve(conn: &zbus::Connection, handle: ObjectPath<'_>, req: Request) -> (u32, Results) {
    let handle = handle.to_string();
    // Registered for the length of the request so the portal can cancel it.
    let _ = conn.object_server().at(handle.as_str(), RequestObj { handle: handle.clone() }).await;
    let (tx, rx) = async_channel::bounded(1);
    let key = handle.clone();
    std::thread::spawn(move || {
        let _ = tx.send_blocking(run_picker(&req, &key));
    });
    let paths = rx.recv().await.unwrap_or_default();
    running().lock().unwrap().remove(&handle);
    let _ = conn.object_server().remove::<RequestObj, _>(handle.as_str()).await;

    if paths.is_empty() {
        return (1, Results::new());
    }
    let uris: Vec<String> = paths.iter().map(|p| picker::uri(p)).collect();
    let mut results = Results::new();
    if let Ok(v) = OwnedValue::try_from(Value::from(uris)) {
        results.insert("uris".into(), v);
    }
    (0, results)
}

fn run_picker(req: &Request, key: &str) -> Vec<PathBuf> {
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
        return Vec::new();
    };
    running().lock().unwrap().insert(key.to_owned(), child.id());
    let Ok(out) = child.wait_with_output() else { return Vec::new() };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.starts_with('/'))
        .map(PathBuf::from)
        .collect()
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
