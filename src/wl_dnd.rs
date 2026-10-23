//! System-wide drag and drop on Wayland, which winit doesn't provide.
//!
//! We open a second wayland-client `Connection` on winit's own `wl_display`
//! and talk to the compositor's data-device protocol directly:
//! * drag out: when an in-app drag leaves the window, `start_drag` hands the
//!   files to the compositor as `text/uri-list`, so browsers, chat apps and
//!   other file managers accept them;
//! * drag in: drops from other apps on our window arrive as `DndEvent::Drop`.
//!
//! Only the Copy action is ever offered or accepted, so no other app will
//! delete files on our behalf (or expect us to delete theirs).

use crate::util;
use eframe::egui::{self, ColorImage, Pos2};
use std::ffi::c_void;
use std::io::{Read, Write};
use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use wayland_client::backend::{Backend, ObjectId};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_data_device::{self, WlDataDevice};
use wayland_client::protocol::wl_data_device_manager::{DndAction, WlDataDeviceManager};
use wayland_client::protocol::wl_data_offer::{self, WlDataOffer};
use wayland_client::protocol::wl_data_source::{self, WlDataSource};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_keyboard, wl_keyboard::WlKeyboard, wl_pointer, wl_pointer::WlPointer, wl_registry::WlRegistry,
    wl_seat::WlSeat, wl_shm, wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum, event_created_child};

const URI_LIST: &str = "text/uri-list";

// Prefer image bytes to browser URLs: URLs can refer to remote or blob resources,
// while Firefox can supply the actual pixels without a separate download.
fn drop_mime(mimes: &[String]) -> Option<&'static str> {
    ["image/png", "image/jpeg", "image/jpg", "image/webp", "image/gif", URI_LIST]
        .into_iter()
        .find(|mime| mimes.iter().any(|m| m == mime))
}

fn image_name(mime: &str) -> Option<&'static str> {
    match mime {
        "image/png" => Some("image.png"),
        "image/jpeg" | "image/jpg" => Some("image.jpg"),
        "image/webp" => Some("image.webp"),
        "image/gif" => Some("image.gif"),
        _ => None,
    }
}

/// `KESTREL_DEBUG=1` traces the drag-and-drop protocol on stderr.
macro_rules! trace {
    ($($arg:tt)*) => {
        if std::env::var_os("KESTREL_DEBUG").is_some() {
            eprintln!("[dnd] {}", format!($($arg)*));
        }
    };
}

/// What the system clipboard holds, from our point of view.
pub enum ClipRead {
    /// We set it ourselves (the app's internal clipboard is authoritative).
    Ours,
    Files { paths: Vec<PathBuf>, cut: bool },
    Nothing,
}

const GNOME_FILES: &str = "x-special/gnome-copied-files";

pub enum DndEvent {
    /// Something from another app (or our own outgoing drag) hovers the window.
    Hover { pos: Pos2 },
    Leave,
    Drop { paths: Vec<PathBuf>, pos: Pos2, own: bool },
    Image { name: String, bytes: Vec<u8>, pos: Pos2 },
    Error(String),
}

#[derive(Default)]
struct Shared {
    /// Serial of the last button press on our window; `start_drag` needs it.
    press_serial: Option<u32>,
    pointer_on_us: bool,
    /// Our outgoing drag is in progress.
    dragging_out: bool,
    /// Serial of the latest key or button event; `set_selection` needs one.
    input_serial: Option<u32>,
    /// Generation of the clipboard source we own, if we own the selection.
    own_selection: Option<u64>,
    /// Plain text of the clipboard source we own.
    own_text: String,
    own_files: bool,
    selection_gen: u64,
    /// The current selection (clipboard) offer from another client.
    selection: Option<WlDataOffer>,
}

struct SourceData {
    uri_list: String,
    plain: String,
    gnome: String,
    /// Clipboard sources carry their generation; drag sources don't.
    clipboard: Option<u64>,
    /// Keeps the drag icon's buffer alive for the duration of the drag.
    icon: Mutex<Option<Icon>>,
}

struct Icon {
    surface: WlSurface,
    _buffer: WlBuffer,
    _pool: WlShmPool,
    _fd: OwnedFd,
}

#[derive(Default)]
struct OfferData {
    mimes: Mutex<Vec<String>>,
}

/// Shares the existing data device; opening another one would steal drag-and-drop.
struct ClipboardWriter {
    conn: Connection,
    qh: QueueHandle<State>,
    shared: Arc<Mutex<Shared>>,
    manager: WlDataDeviceManager,
    device: WlDataDevice,
}

impl ClipboardWriter {
    fn set_text(&self, text: &str) -> bool {
        let data = SourceData {
            uri_list: String::new(),
            plain: text.to_string(),
            gnome: String::new(),
            clipboard: None,
            icon: Mutex::new(None),
        };
        self.set_selection(data, &[])
    }

    fn set_selection(&self, mut data: SourceData, extra: &[&str]) -> bool {
        let mut shared = self.shared.lock().unwrap();
        let Some(serial) = shared.input_serial else { return false };
        shared.selection_gen += 1;
        let generation = shared.selection_gen;
        data.clipboard = Some(generation);
        let plain = data.plain.clone();
        let files = !data.uri_list.is_empty();
        let source = self.manager.create_data_source(&self.qh, Arc::new(data));
        for mime in extra.iter().copied().chain(["text/plain;charset=utf-8", "text/plain", "UTF8_STRING", "STRING", "TEXT"]) {
            source.offer(mime.into());
        }
        self.device.set_selection(Some(&source), serial);
        shared.own_selection = Some(generation);
        shared.own_text = plain;
        shared.own_files = files;
        drop(shared);
        self.conn.flush().is_ok()
    }
}

struct ClipboardOutput {
    writer: Option<ClipboardWriter>,
}

impl egui::Plugin for ClipboardOutput {
    fn debug_name(&self) -> &'static str { "Kestrel clipboard" }

    fn output_hook(&mut self, _ctx: &egui::Context, output: &mut egui::FullOutput) {
        // This runs after label selection's end-of-pass copy output, too.
        if let Some(writer) = &self.writer {
            route_text_output(output, |text| writer.set_text(text));
        }
    }
}

fn route_text_output(output: &mut egui::FullOutput, mut write: impl FnMut(&str) -> bool) {
    output.platform_output.commands.retain(|command| {
        !matches!(command, egui::OutputCommand::CopyText(text) if write(text))
    });
}

pub struct WlDnd {
    ctx: egui::Context,
    conn: Connection,
    qh: QueueHandle<State>,
    shared: Arc<Mutex<Shared>>,
    manager: WlDataDeviceManager,
    device: WlDataDevice,
    surface: WlSurface,
    compositor: WlCompositor,
    shm: WlShm,
    rx: Receiver<DndEvent>,
    /// Writing to this stops the dispatch thread.
    wake_tx: OwnedFd,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for WlDnd {
    /// Stops the dispatch thread while the display is still alive, so its
    /// proxies aren't destroyed after winit has freed the connection.
    fn drop(&mut self) {
        // The output plugin must release its proxies before winit frees the display.
        self.ctx.plugin::<ClipboardOutput>().lock().writer = None;
        let _ = unsafe { libc::write(std::os::fd::AsRawFd::as_raw_fd(&self.wake_tx), [1u8].as_ptr().cast(), 1) };
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Dispatches our event queue until `wake` becomes readable or the connection fails.
fn dispatch_loop(mut queue: wayland_client::EventQueue<State>, mut state: State, wake: OwnedFd) {
    use std::os::fd::AsRawFd;
    loop {
        if queue.dispatch_pending(&mut state).is_err() || queue.flush().is_err() {
            break;
        }
        let Some(guard) = queue.prepare_read() else { continue };
        let mut fds = [
            libc::pollfd { fd: guard.connection_fd().as_raw_fd(), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: wake.as_raw_fd(), events: libc::POLLIN, revents: 0 },
        ];
        if unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) } < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            break;
        }
        if fds[1].revents != 0 {
            return; // WlDnd dropped; the display is still alive, so normal cleanup is safe.
        }
        match guard.read() {
            Ok(_) => {}
            Err(wayland_client::backend::WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => break,
        }
    }
    // The connection is broken and may already be freed: skip the proxy destructors.
    std::mem::forget(queue);
    std::mem::forget(state);
}

impl WlDnd {
    /// Hooks into winit's Wayland connection.
    ///
    /// # Safety
    /// `display` and `surface` must be the live `wl_display` / `wl_surface`
    /// pointers of our window and outlive this object.
    pub unsafe fn new(display: *mut c_void, surface: *mut c_void, ctx: egui::Context) -> Result<Self, String> {
        let backend = unsafe { Backend::from_foreign_display(display.cast()) };
        let conn = Connection::from_backend(backend);
        let (globals, queue) = registry_queue_init::<State>(&conn).map_err(|e| e.to_string())?;
        let qh = queue.handle();
        let seat: WlSeat = globals.bind(&qh, 1..=7, ()).map_err(|e| e.to_string())?;
        let manager: WlDataDeviceManager = globals.bind(&qh, 3..=3, ()).map_err(|e| e.to_string())?;
        let compositor: WlCompositor = globals.bind(&qh, 4..=6, ()).map_err(|e| e.to_string())?;
        let shm: WlShm = globals.bind(&qh, 1..=1, ()).map_err(|e| e.to_string())?;
        let surface_id = unsafe { ObjectId::from_ptr(WlSurface::interface(), surface.cast()) }.map_err(|e| e.to_string())?;
        let surface = WlSurface::from_id(&conn, surface_id).map_err(|e| e.to_string())?;
        let _pointer = seat.get_pointer(&qh, ());
        let _keyboard = seat.get_keyboard(&qh, ());
        let device = manager.get_data_device(&seat, &qh, ());

        let shared = Arc::new(Mutex::new(Shared::default()));
        let (tx, rx) = channel();
        let state = State {
            our_surface: surface.id(),
            shared: shared.clone(),
            tx,
            ctx: ctx.clone(),
            offer: None,
            pos: Pos2::ZERO,
        };
        let mut fds = [0; 2];
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let (wake_rx, wake_tx) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        let thread = std::thread::Builder::new()
            .name("wayland-dnd".into())
            .spawn(move || dispatch_loop(queue, state, wake_rx))
            .map_err(|e| e.to_string())?;
        conn.flush().map_err(|e| e.to_string())?;
        let wl = Self { ctx, conn, qh, shared, manager, device, surface, compositor, shm, rx, wake_tx, thread: Some(thread) };
        wl.ctx.add_plugin(ClipboardOutput { writer: Some(wl.clipboard_writer()) });
        Ok(wl)
    }

    pub fn poll(&self) -> Vec<DndEvent> {
        self.rx.try_iter().collect()
    }

    pub fn dragging_out(&self) -> bool {
        self.shared.lock().unwrap().dragging_out
    }

    /// Hands `paths` to the compositor as a system drag. Must be called while
    /// the mouse button that started the drag is still held.
    pub fn start_drag(&self, paths: &[PathBuf]) -> bool {
        let mut shared = self.shared.lock().unwrap();
        trace!("start_drag {} paths, serial {:?}, on_us {}", paths.len(), shared.press_serial, shared.pointer_on_us);
        let Some(serial) = shared.press_serial else { return false };
        if paths.is_empty() || shared.dragging_out {
            return false;
        }
        let data = Arc::new(source_data(paths, false, None));
        let source = self.manager.create_data_source(&self.qh, data.clone());
        for mime in [URI_LIST, "text/plain;charset=utf-8", "text/plain", "UTF8_STRING"] {
            source.offer(mime.into());
        }
        source.set_actions(DndAction::Copy);
        let icon = self.make_icon(&drag_icon(paths));
        self.device.start_drag(Some(&source), &self.surface, icon.as_ref().map(|i| &i.surface), serial);
        if let Some(icon) = icon {
            icon.surface.commit();
            *data.icon.lock().unwrap() = Some(icon);
        }
        shared.dragging_out = true;
        drop(shared);
        let _ = self.conn.flush();

        // Swap in a real thumbnail for images once it's rendered.
        let renderer = paths.first().and_then(|p| {
            let ext = p.extension()?.to_string_lossy().to_lowercase();
            crate::thumbs::renderer_for(&ext).filter(|_| paths.len() == 1)
        });
        if let Some(r) = renderer {
            let (path, conn) = (paths[0].clone(), self.conn.clone());
            let (shm, qh, data) = (self.shm.clone(), self.qh.clone(), data.clone());
            std::thread::spawn(move || {
                let Ok(rendered) = crate::thumbs::render(&path, r, 96) else { return };
                let mut slot = data.icon.lock().unwrap();
                let Some(icon) = slot.as_mut() else { return }; // drag already over
                if let Some((buffer, pool, fd)) = shm_buffer(&shm, &qh, &rendered.image) {
                    icon.surface.attach(Some(&buffer), 0, 0);
                    icon.surface.damage_buffer(0, 0, i32::MAX, i32::MAX);
                    icon.surface.commit();
                    *icon = Icon { surface: icon.surface.clone(), _buffer: buffer, _pool: pool, _fd: fd };
                    let _ = conn.flush();
                }
            });
        }
        true
    }

    /// Puts files on the system clipboard, in the formats file managers and browsers read.
    pub fn set_clipboard_files(&self, paths: &[PathBuf], cut: bool) -> bool {
        self.set_selection(source_data(paths, cut, None), &[URI_LIST, GNOME_FILES])
    }

    pub fn set_clipboard_text(&self, text: &str) -> bool {
        self.clipboard_writer().set_text(text)
    }

    fn clipboard_writer(&self) -> ClipboardWriter {
        ClipboardWriter {
            conn: self.conn.clone(), qh: self.qh.clone(), shared: self.shared.clone(),
            manager: self.manager.clone(), device: self.device.clone(),
        }
    }

    fn set_selection(&self, data: SourceData, extra: &[&str]) -> bool {
        self.clipboard_writer().set_selection(data, extra)
    }

    /// Reads text from the clipboard. egui-winit's own clipboard is X11-only here
    /// (smithay-clipboard is patched out), so text pastes come from our data device.
    pub fn read_clipboard_text(&self) -> Option<String> {
        let offer = {
            let shared = self.shared.lock().unwrap();
            if shared.own_selection.is_some() {
                trace!("paste text: ours");
                return Some(shared.own_text.clone());
            }
            trace!("paste text: offer {:?}", shared.selection.is_some());
            shared.selection.clone()?
        };
        let mimes = offer.data::<OfferData>().map(|d| d.mimes.lock().unwrap().clone()).unwrap_or_default();
        let mime = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain", "STRING", "TEXT"]
            .into_iter()
            .find(|m| mimes.iter().any(|x| x == m));
        trace!("paste text: mimes {mimes:?} -> {mime:?}");
        let text = receive(&self.conn, &offer, mime?);
        trace!("paste text: got {:?}", text.as_ref().map(|t| t.len()));
        text
    }

    /// Reads files from the clipboard (blocks briefly while the owner sends them).
    pub fn read_clipboard(&self) -> ClipRead {
        let offer = {
            let shared = self.shared.lock().unwrap();
            if shared.own_selection.is_some() {
                if shared.own_files {
                    return ClipRead::Ours;
                }
                let paths = crate::app::parse_path_list(&shared.own_text);
                return if paths.is_empty() { ClipRead::Nothing } else { ClipRead::Files { paths, cut: false } };
            }
            match &shared.selection {
                Some(o) => o.clone(),
                None => return ClipRead::Nothing,
            }
        };
        let mimes = offer.data::<OfferData>().map(|d| d.mimes.lock().unwrap().clone()).unwrap_or_default();
        let pick = [GNOME_FILES, URI_LIST, "text/plain;charset=utf-8", "text/plain", "UTF8_STRING"]
            .into_iter()
            .find(|m| mimes.iter().any(|x| x == m));
        let Some(mime) = pick else { return ClipRead::Nothing };
        let Some(text) = receive(&self.conn, &offer, mime) else { return ClipRead::Nothing };
        let (paths, cut) = match mime {
            GNOME_FILES => {
                let mut lines = text.lines();
                let cut = lines.next() == Some("cut");
                (parse_uri_list(&lines.collect::<Vec<_>>().join("\n")), cut)
            }
            URI_LIST => (parse_uri_list(&text), false),
            _ => (crate::app::parse_path_list(&text), false),
        };
        if paths.is_empty() { ClipRead::Nothing } else { ClipRead::Files { paths, cut } }
    }

    fn make_icon(&self, img: &ColorImage) -> Option<Icon> {
        let (buffer, pool, fd) = shm_buffer(&self.shm, &self.qh, img)?;
        let surface = self.compositor.create_surface(&self.qh, ());
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, i32::MAX, i32::MAX);
        Some(Icon { surface, _buffer: buffer, _pool: pool, _fd: fd })
    }
}

fn source_data(paths: &[PathBuf], cut: bool, clipboard: Option<u64>) -> SourceData {
    let uris: Vec<String> = paths.iter().map(|p| file_uri(p)).collect();
    SourceData {
        uri_list: uris.iter().map(|u| format!("{u}\r\n")).collect(),
        plain: paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n"),
        gnome: format!("{}\n{}", if cut { "cut" } else { "copy" }, uris.join("\n")),
        clipboard,
        icon: Mutex::new(None),
    }
}

/// Asks the offer's owner for `mime` and reads it, giving up after a second.
fn receive(conn: &Connection, offer: &WlDataOffer, mime: &str) -> Option<String> {
    let mut fds = [0; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return None;
    }
    let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    offer.receive(mime.into(), write.as_fd());
    drop(write);
    conn.flush().ok()?;
    let mut file = std::fs::File::from(read);
    let mut out = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    let mut buf = [0u8; 8192];
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now()).as_millis() as i32;
        let mut pfd = libc::pollfd { fd: std::os::fd::AsRawFd::as_raw_fd(&file), events: libc::POLLIN, revents: 0 };
        if left == 0 || unsafe { libc::poll(&mut pfd, 1, left) } <= 0 {
            return None;
        }
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(_) => return None,
        }
    }
    Some(String::from_utf8_lossy(&out).into_owned())
}

/// Copies an image into a shared-memory buffer (ARGB8888, premultiplied).
fn shm_buffer(shm: &WlShm, qh: &QueueHandle<State>, img: &ColorImage) -> Option<(WlBuffer, WlShmPool, OwnedFd)> {
    let [w, h] = img.size;
    let stride = w * 4;
    let size = stride * h;
    let fd = unsafe { libc::memfd_create(c"kestrel-drag-icon".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return None;
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let mut file = std::fs::File::from(fd.try_clone().ok()?);
    let mut bytes = Vec::with_capacity(size);
    for px in &img.pixels {
        // Color32 is premultiplied RGBA; Wayland wants little-endian ARGB, i.e. BGRA bytes.
        let [r, g, b, a] = px.to_array();
        bytes.extend_from_slice(&[b, g, r, a]);
    }
    file.write_all(&bytes).ok()?;
    let pool = shm.create_pool(fd.as_fd(), size as i32, qh, ());
    let buffer = pool.create_buffer(0, w as i32, h as i32, stride as i32, wl_shm::Format::Argb8888, qh, ());
    Some((buffer, pool, fd))
}

/// A simple file/folder pictogram with a count badge, drawn without a renderer.
fn drag_icon(paths: &[PathBuf]) -> ColorImage {
    const S: usize = 64;
    let mut img = ColorImage::filled([S, S], egui::Color32::TRANSPARENT);
    let first = &paths[0];
    let is_dir = first.is_dir();
    let ext = first.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let color = crate::theme::category_color(util::category(&ext, is_dir));
    if is_dir {
        fill_rect(&mut img, 8, 14, 24, 8, color.gamma_multiply(0.75));
        fill_rect(&mut img, 8, 20, 48, 34, color);
    } else {
        fill_rect(&mut img, 14, 6, 36, 50, color.gamma_multiply(0.3));
        fill_rect(&mut img, 14, 44, 36, 12, color);
    }
    if paths.len() > 1 {
        // Badge dots: one per item, up to five.
        for i in 0..paths.len().min(5) {
            fill_circle(&mut img, 54.0 - i as f32 * 9.0, 9.0, 4.5, crate::theme::pal().accent);
        }
    }
    img
}

fn fill_rect(img: &mut ColorImage, x: usize, y: usize, w: usize, h: usize, c: egui::Color32) {
    let width = img.size[0];
    for yy in y..(y + h).min(img.size[1]) {
        for xx in x..(x + w).min(width) {
            img.pixels[yy * width + xx] = c;
        }
    }
}

fn fill_circle(img: &mut ColorImage, cx: f32, cy: f32, r: f32, c: egui::Color32) {
    let width = img.size[0];
    for y in 0..img.size[1] {
        for x in 0..width {
            let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
            if d <= r {
                img.pixels[y * width + x] = c;
            }
        }
    }
}

pub fn file_uri(p: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut s = String::from("file://");
    for &b in p.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            s.push(b as char);
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    s
}

fn parse_uri_list(text: &str) -> Vec<PathBuf> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.strip_prefix("file://"))
        // file://host/path: drop an optional host part.
        .map(|rest| if rest.starts_with('/') { rest } else { rest.find('/').map_or(rest, |i| &rest[i..]) })
        .map(|p| PathBuf::from(crate::fsops::percent_decode(p)))
        .collect()
}

// ---------------------------------------------------------------- event handling

struct State {
    our_surface: ObjectId,
    shared: Arc<Mutex<Shared>>,
    tx: Sender<DndEvent>,
    ctx: egui::Context,
    offer: Option<(WlDataOffer, &'static str)>,
    pos: Pos2,
}

impl State {
    fn emit(&self, ev: DndEvent) {
        let _ = self.tx.send(ev);
        self.ctx.request_repaint();
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(_: &mut Self, _: &WlRegistry, _: <WlRegistry as Proxy>::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

macro_rules! ignore_events {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for State {
            fn event(_: &mut Self, _: &$t, _: <$t as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore_events!(WlSeat, WlDataDeviceManager, WlCompositor, WlSurface, WlShm, WlShmPool, WlBuffer);

impl Dispatch<WlPointer, ()> for State {
    fn event(state: &mut Self, _: &WlPointer, event: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        let mut shared = state.shared.lock().unwrap();
        match event {
            wl_pointer::Event::Enter { surface, .. } => {
                shared.pointer_on_us = surface.id() == state.our_surface;
                trace!("pointer enter, ours: {}", shared.pointer_on_us);
            }
            wl_pointer::Event::Leave { .. } => shared.pointer_on_us = false,
            wl_pointer::Event::Button { serial, state: WEnum::Value(wl_pointer::ButtonState::Pressed), .. }
                if shared.pointer_on_us =>
            {
                trace!("press serial {serial}");
                shared.press_serial = Some(serial);
                shared.input_serial = Some(serial);
            }
            _ => {}
        }
    }
}

impl Dispatch<WlKeyboard, ()> for State {
    fn event(state: &mut Self, _: &WlKeyboard, event: wl_keyboard::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        let serial = match event {
            wl_keyboard::Event::Enter { serial, .. } | wl_keyboard::Event::Key { serial, .. } => serial,
            _ => return, // the keymap fd is closed when dropped
        };
        state.shared.lock().unwrap().input_serial = Some(serial);
    }
}

impl Dispatch<WlDataSource, Arc<SourceData>> for State {
    fn event(
        state: &mut Self,
        source: &WlDataSource,
        event: wl_data_source::Event,
        data: &Arc<SourceData>,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_source::Event::Send { mime_type, fd } => {
                trace!("source send {mime_type}");
                let bytes = match mime_type.as_str() {
                    URI_LIST => data.uri_list.clone(),
                    GNOME_FILES => data.gnome.clone(),
                    _ => data.plain.clone(),
                };
                // A pipe holds ~64 KiB; write off-thread so a slow reader can't stall dispatch.
                std::thread::spawn(move || {
                    let _ = std::fs::File::from(fd).write_all(bytes.as_bytes());
                });
            }
            wl_data_source::Event::Cancelled | wl_data_source::Event::DndFinished => {
                trace!("source ended: {event:?}");
                source.destroy();
                let mut shared = state.shared.lock().unwrap();
                match data.clipboard {
                    // Another client took the clipboard.
                    Some(generation) if shared.own_selection == Some(generation) => shared.own_selection = None,
                    Some(_) => {}
                    None => {
                        data.icon.lock().unwrap().take();
                        shared.dragging_out = false;
                    }
                }
                state.ctx.request_repaint();
            }
            _ => {}
        }
    }
}

impl Dispatch<WlDataDevice, ()> for State {
    fn event(state: &mut Self, _: &WlDataDevice, event: wl_data_device::Event, _: &(), conn: &Connection, _: &QueueHandle<Self>) {
        match event {
            wl_data_device::Event::Enter { serial, surface, x, y, id } => {
                trace!("device enter ours={} offer={}", surface.id() == state.our_surface, id.is_some());
                if let Some((old, _)) = state.offer.take() {
                    old.destroy();
                }
                if surface.id() != state.our_surface {
                    return;
                }
                let Some(offer) = id else { return };
                let mimes = offer.data::<OfferData>().map(|d| d.mimes.lock().unwrap().clone()).unwrap_or_default();
                let mime = drop_mime(&mimes);
                trace!("drop formats {mimes:?}, accepting {mime:?}");
                if let Some(mime) = mime {
                    offer.accept(serial, Some(mime.into()));
                    offer.set_actions(DndAction::Copy, DndAction::Copy);
                    state.pos = Pos2::new(x as f32, y as f32);
                    state.emit(DndEvent::Hover { pos: state.pos });
                    state.offer = Some((offer, mime));
                } else {
                    offer.accept(serial, None);
                    offer.destroy();
                }
            }
            wl_data_device::Event::Motion { x, y, .. } => {
                if state.offer.is_some() {
                    state.pos = Pos2::new(x as f32, y as f32);
                    state.emit(DndEvent::Hover { pos: state.pos });
                }
            }
            wl_data_device::Event::Leave => {
                if let Some((offer, _)) = state.offer.take() {
                    offer.destroy();
                }
                state.emit(DndEvent::Leave);
            }
            wl_data_device::Event::Selection { id } => {
                let mut shared = state.shared.lock().unwrap();
                if let Some(old) = std::mem::replace(&mut shared.selection, id) {
                    old.destroy();
                }
            }
            wl_data_device::Event::Drop => {
                trace!("device drop, offer={}", state.offer.is_some());
                let Some((offer, mime)) = state.offer.take() else { return };
                state.emit(DndEvent::Leave);
                let mut fds = [0; 2];
                if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
                    offer.destroy();
                    return;
                }
                let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
                offer.receive(mime.into(), write.as_fd());
                drop(write); // libwayland dup'd it when marshalling
                let _ = conn.flush();
                let (tx, ctx, conn, pos) = (state.tx.clone(), state.ctx.clone(), conn.clone(), state.pos);
                let own = state.shared.lock().unwrap().dragging_out;
                std::thread::spawn(move || {
                    let mut bytes = Vec::new();
                    let result = std::fs::File::from(read).read_to_end(&mut bytes);
                    offer.finish();
                    offer.destroy();
                    let _ = conn.flush();
                    let event = match result {
                        Err(e) => DndEvent::Error(format!("Could not receive dropped image or files: {e}")),
                        Ok(_) if bytes.is_empty() => DndEvent::Error("The source sent no image or files".into()),
                        Ok(_) => match image_name(mime) {
                            Some(name) => DndEvent::Image { name: name.into(), bytes, pos },
                            None => {
                                let paths = parse_uri_list(&String::from_utf8_lossy(&bytes));
                                if paths.is_empty() {
                                    DndEvent::Error("The drop contains no local files or image data".into())
                                } else {
                                    DndEvent::Drop { paths, pos, own }
                                }
                            }
                        },
                    };
                    let _ = tx.send(event);
                    ctx.request_repaint();
                });
            }
            _ => {}
        }
    }

    event_created_child!(State, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, OfferData::default()),
    ]);
}

impl Dispatch<WlDataOffer, OfferData> for State {
    fn event(_: &mut Self, _: &WlDataOffer, event: wl_data_offer::Event, data: &OfferData, _: &Connection, _: &QueueHandle<Self>) {
        if let wl_data_offer::Event::Offer { mime_type } = event {
            data.mimes.lock().unwrap().push(mime_type);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_images_prefer_binary_data_and_file_drags_keep_uri_lists() {
        let firefox = [URI_LIST, "text/x-moz-url", "text/html", "image/jpeg", "image/png"]
            .map(String::from);
        assert_eq!(drop_mime(&firefox), Some("image/png"));
        assert_eq!(image_name(drop_mime(&firefox).unwrap()), Some("image.png"));
        assert_eq!(drop_mime(&[URI_LIST.into(), "text/plain".into()]), Some(URI_LIST));
        assert_eq!(drop_mime(&["image/jpeg".into()]), Some("image/jpeg"));
        assert_eq!(drop_mime(&["text/html".into(), "text/plain".into()]), None);
    }

    #[test]
    fn text_output_uses_wayland_and_keeps_backend_fallback() {
        let mut output = egui::FullOutput::default();
        output.platform_output.commands = vec![
            egui::OutputCommand::CopyText("Привет\nworld".into()),
            egui::OutputCommand::OpenUrl(egui::OpenUrl::new_tab("https://example.com")),
            egui::OutputCommand::CopyText("fallback".into()),
        ];
        let mut copied = Vec::new();
        route_text_output(&mut output, |text| {
            copied.push(text.to_owned());
            text != "fallback"
        });
        assert_eq!(copied, ["Привет\nworld", "fallback"]);
        assert_eq!(output.platform_output.commands.len(), 2);
        assert!(matches!(&output.platform_output.commands[0], egui::OutputCommand::OpenUrl(url) if url.url == "https://example.com"));
        assert!(matches!(&output.platform_output.commands[1], egui::OutputCommand::CopyText(text) if text == "fallback"));
    }

    #[test]
    fn uri_round_trip() {
        let p = PathBuf::from("/tmp/a b/ü#1.txt");
        let uri = file_uri(&p);
        assert_eq!(uri, "file:///tmp/a%20b/%C3%BC%231.txt");
        assert_eq!(parse_uri_list(&format!("{uri}\r\n# comment\r\nfile://host/etc/x\r\n")), vec![p, PathBuf::from("/etc/x")]);
    }
}
