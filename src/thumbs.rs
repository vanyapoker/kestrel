//! Rendering files to images (raster images in-process; SVG, PDF and video via
//! rsvg-convert, pdftoppm and ffmpeg when installed) plus a thumbnail cache for
//! the grid view.

use eframe::egui::{self, ColorImage};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const MAX_IMAGE_FILE: u64 = 256 * 1024 * 1024;
const TOOL_TIMEOUT: Duration = Duration::from_secs(8);
pub const THUMB_SIZE: u32 = 256;
const CACHE_CAP: usize = 600;
const WORKERS: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Renderer {
    Raster,
    Svg,
    Pdf,
    Video,
}

pub fn renderer_for(ext: &str) -> Option<Renderer> {
    Some(match ext {
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "ico" | "tif" | "tiff" | "tga"
        | "qoi" | "pnm" | "hdr" | "exr" | "avif" => Renderer::Raster,
        "svg" | "svgz" => Renderer::Svg,
        "pdf" => Renderer::Pdf,
        "mp4" | "mkv" | "webm" | "avi" | "mov" | "wmv" | "flv" | "m4v" | "mpg" | "mpeg" => {
            Renderer::Video
        }
        _ => return None,
    })
}

pub struct Rendered {
    pub image: ColorImage,
    /// Original dimensions, when meaningful.
    pub dims: Option<(u32, u32)>,
}

pub fn render(path: &Path, renderer: Renderer, max: u32) -> Result<Rendered, String> {
    match renderer {
        Renderer::Raster => render_raster(path, max),
        Renderer::Svg => {
            let png = run_tool(
                Command::new("rsvg-convert")
                    .args(["-a", "-w", &max.to_string(), "-h", &max.to_string()])
                    .arg(path),
            )?;
            decode_png(&png, None)
        }
        Renderer::Pdf => {
            let png = run_tool(
                Command::new("pdftoppm")
                    .args(["-png", "-f", "1", "-l", "1", "-scale-to", &max.to_string()])
                    .arg(path),
            )?;
            decode_png(&png, None)
        }
        Renderer::Video => {
            // Grab a frame ~10% in (capped at 30s) to skip black intros.
            let info = media_info(path);
            let dur = info
                .iter()
                .find(|(k, _)| k == "duration")
                .and_then(|(_, v)| v.parse::<f64>().ok());
            let at = dur.map(|d| (d * 0.1).min(30.0)).unwrap_or(0.0);
            let scale = format!("scale='min({max},iw)':-2");
            let frame = |t: f64| {
                run_tool(
                    Command::new("ffmpeg")
                        .args(["-v", "error", "-ss", &format!("{t:.2}"), "-i"])
                        .arg(path)
                        .args([
                            "-frames:v",
                            "1",
                            "-vf",
                            &scale,
                            "-f",
                            "image2pipe",
                            "-c:v",
                            "png",
                            "-",
                        ]),
                )
            };
            let png = frame(at).or_else(|_| frame(0.0))?;
            let w = info
                .iter()
                .find(|(k, _)| k == "width")
                .and_then(|(_, v)| v.parse().ok());
            let h = info
                .iter()
                .find(|(k, _)| k == "height")
                .and_then(|(_, v)| v.parse().ok());
            decode_png(&png, w.zip(h))
        }
    }
}

fn render_raster(path: &Path, max: u32) -> Result<Rendered, String> {
    if std::fs::metadata(path)
        .map(|m| m.len() > MAX_IMAGE_FILE)
        .unwrap_or(false)
    {
        return Err("Image too large to preview".into());
    }
    let img = image::ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .decode()
        .map_err(|e| e.to_string())?;
    let dims = (img.width(), img.height());
    let img = if dims.0 > max || dims.1 > max {
        img.thumbnail(max, max)
    } else {
        img
    };
    Ok(Rendered {
        image: to_color_image(img),
        dims: Some(dims),
    })
}

fn decode_png(bytes: &[u8], dims: Option<(u32, u32)>) -> Result<Rendered, String> {
    let img = image::load_from_memory(bytes).map_err(|e| e.to_string())?;
    let dims = dims.or(Some((img.width(), img.height())));
    Ok(Rendered {
        image: to_color_image(img),
        dims,
    })
}

fn to_color_image(img: image::DynamicImage) -> ColorImage {
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    ColorImage::from_rgba_unmultiplied(size, rgba.as_raw())
}

/// `ffprobe` key/value info: duration, width, height, codec_name, bit_rate…
pub fn media_info(path: &Path) -> Vec<(String, String)> {
    let out = run_tool(
        Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "stream=codec_type,codec_name,width,height,sample_rate,channels:format=duration,bit_rate:format_tags=title,artist,album",
                "-of",
                "default=nw=1",
            ])
            .arg(path),
    )
    .unwrap_or_default();
    String::from_utf8_lossy(&out)
        .lines()
        .filter_map(|l| l.split_once('='))
        .filter(|(_, v)| !v.is_empty() && *v != "N/A")
        .map(|(k, v)| (k.trim_start_matches("TAG:").to_lowercase(), v.to_string()))
        .collect()
}

pub fn pdf_pages(path: &Path) -> Option<u32> {
    let out = run_tool(Command::new("pdfinfo").arg(path)).ok()?;
    String::from_utf8_lossy(&out)
        .lines()
        .find_map(|l| l.strip_prefix("Pages:").and_then(|v| v.trim().parse().ok()))
}

thread_local! {
    static CANCEL: std::cell::RefCell<Option<Box<dyn Fn() -> bool>>> = const { std::cell::RefCell::new(None) };
}

/// Lets the current thread's tool runs be killed as soon as `cancelled` turns true.
pub fn set_cancel(cancelled: Box<dyn Fn() -> bool>) {
    CANCEL.with(|c| *c.borrow_mut() = Some(cancelled));
}

fn is_cancelled() -> bool {
    CANCEL.with(|c| c.borrow().as_ref().is_some_and(|f| f()))
}

/// Runs a tool, returning stdout. Killed after a timeout so corrupt files can't hang us.
fn run_tool(cmd: &mut Command) -> Result<Vec<u8>, String> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("{}: {e}", cmd.get_program().to_string_lossy()))?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() > TOOL_TIMEOUT || is_cancelled() => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Timed out".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(e) => return Err(e.to_string()),
        }
    }
    let buf = reader.join().unwrap_or_default();
    if buf.is_empty() {
        Err("No output".into())
    } else {
        Ok(buf)
    }
}

// A tiny sample is enough for document thumbnails. Never open streams or devices.
const TEXT_BYTES: usize = 8 * 1024;
const TEXT_LINES: usize = 24;
const TEXT_COLUMNS: usize = 160;

fn text_thumbnail(path: &Path) -> Option<egui::text::LayoutJob> {
    if !std::fs::metadata(path).ok()?.is_file() {
        return None;
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(TEXT_BYTES as u64).read_to_end(&mut bytes).ok()?;
    let text = match std::str::from_utf8(&bytes) {
        Ok(text) => text,
        Err(e)
            if e.error_len().is_none()
                && bytes.len() == TEXT_BYTES
                && meta.len() > bytes.len() as u64 =>
        {
            std::str::from_utf8(&bytes[..e.valid_up_to()]).ok()?
        }
        Err(_) => return None,
    }
    .trim_start_matches('\u{feff}');
    if text
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return None;
    }
    let mut sample = String::new();
    for line in text.lines().take(TEXT_LINES) {
        sample.extend(line.replace('\t', "    ").chars().take(TEXT_COLUMNS));
        sample.push('\n');
    }
    if sample.trim().is_empty() {
        return None;
    }
    Some(crate::preview::highlight_text(path, &sample, TEXT_LINES, egui::FontId::monospace(12.5)).0)
}

// ---------------------------------------------------------------- cache

pub enum Thumb {
    Pending,
    Ready(egui::TextureHandle),
    Text(Arc<egui::text::LayoutJob>),
    Failed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Image(Renderer),
    Text(u64), // palette generation
}

struct Cached {
    request: u64,
    kind: Kind,
    value: Thumb,
}

struct Request {
    path: PathBuf,
    id: u64,
    kind: Kind,
}

enum Payload {
    Image(ColorImage),
    Text(egui::text::LayoutJob),
}

struct ResultMsg {
    request: Request,
    payload: Option<Payload>,
}

/// Most recently requested first, so visible tiles render before old queued work.
struct Queue {
    stack: Mutex<Vec<Request>>,
    ready: Condvar,
}

pub struct Thumbs {
    cache: HashMap<PathBuf, Cached>,
    order: VecDeque<PathBuf>,
    queue: Arc<Queue>,
    rx: Receiver<ResultMsg>,
    next_request: u64,
    /// Requested this frame; protected from eviction.
    touched: HashSet<PathBuf>,
}

impl Thumbs {
    pub fn new(ctx: egui::Context) -> Self {
        let queue = Arc::new(Queue {
            stack: Mutex::new(Vec::new()),
            ready: Condvar::new(),
        });
        let (tx, rx) = channel();
        for i in 0..WORKERS {
            let (queue, tx, ctx): (_, Sender<ResultMsg>, _) =
                (queue.clone(), tx.clone(), ctx.clone());
            std::thread::Builder::new()
                .name(format!("thumb-{i}"))
                .spawn(move || {
                    loop {
                        let request = {
                            let mut stack = queue.stack.lock().expect("lock poisoned");
                            loop {
                                if let Some(job) = stack.pop() {
                                    break job;
                                }
                                stack = queue.ready.wait(stack).expect("lock poisoned");
                            }
                        };
                        let payload = match request.kind {
                            Kind::Image(renderer) => render(&request.path, renderer, THUMB_SIZE)
                                .ok()
                                .map(|r| Payload::Image(r.image)),
                            Kind::Text(_) => text_thumbnail(&request.path).map(Payload::Text),
                        };
                        if tx.send(ResultMsg { request, payload }).is_err() {
                            return;
                        }
                        ctx.request_repaint();
                    }
                })
                .expect("spawn thumbnail thread");
        }
        Self {
            cache: HashMap::new(),
            order: VecDeque::new(),
            queue,
            rx,
            next_request: 0,
            touched: HashSet::new(),
        }
    }

    fn request(&mut self, path: &Path, kind: Kind) {
        self.touched.insert(path.to_path_buf());
        if self.cache.get(path).is_some_and(|c| c.kind == kind) {
            return;
        }
        self.invalidate(path);
        self.next_request += 1;
        self.cache.insert(
            path.to_path_buf(),
            Cached {
                request: self.next_request,
                kind,
                value: Thumb::Pending,
            },
        );
        self.order.push_back(path.to_path_buf());
        self.queue
            .stack
            .lock()
            .expect("lock poisoned")
            .push(Request {
                path: path.to_path_buf(),
                id: self.next_request,
                kind,
            });
        self.queue.ready.notify_one();
    }

    /// Returns the image if ready, scheduling it otherwise.
    pub fn get(&mut self, path: &Path, renderer: Renderer) -> Option<&egui::TextureHandle> {
        self.request(path, Kind::Image(renderer));
        match self.cache.get(path).map(|c| &c.value) {
            Some(Thumb::Ready(t)) => Some(t),
            _ => None,
        }
    }

    /// Text is drawn as vectors so it remains crisp at every icon zoom level.
    pub fn get_text(&mut self, path: &Path) -> Option<&egui::text::LayoutJob> {
        self.request(path, Kind::Text(crate::theme::generation()));
        match self.cache.get(path).map(|c| &c.value) {
            Some(Thumb::Text(job)) => Some(job),
            _ => None,
        }
    }

    /// Cancel queued and in-flight requests; late results cannot refill the cache.
    pub fn cancel_pending(&mut self) {
        self.queue.stack.lock().expect("lock poisoned").clear();
        self.cache.retain(|_, c| !matches!(c.value, Thumb::Pending));
        self.order.retain(|p| self.cache.contains_key(p));
    }

    pub fn invalidate(&mut self, path: &Path) {
        self.cache.remove(path);
        self.order.retain(|p| p != path);
        self.queue
            .stack
            .lock()
            .expect("lock poisoned")
            .retain(|r| r.path != path);
    }

    fn accept(&mut self, ctx: &egui::Context, msg: ResultMsg) {
        let request = msg.request;
        let Some(cached) = self.cache.get_mut(&request.path) else {
            return;
        };
        if cached.request != request.id || cached.kind != request.kind {
            return;
        }
        if matches!(request.kind, Kind::Text(g) if g != crate::theme::generation()) {
            return;
        }
        cached.value = match msg.payload {
            Some(Payload::Image(img)) => Thumb::Ready(ctx.load_texture(
                format!("thumb:{}", request.path.display()),
                img,
                egui::TextureOptions::LINEAR,
            )),
            Some(Payload::Text(job)) => Thumb::Text(Arc::new(job)),
            None => Thumb::Failed,
        };
    }

    pub fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.rx.try_recv() {
            self.accept(ctx, msg);
        }
        // Evict oldest entries that weren't visible this frame, including pending work.
        let mut guard = self.order.len();
        while self.cache.len() > CACHE_CAP && guard > 0 {
            guard -= 1;
            let Some(p) = self.order.pop_front() else {
                break;
            };
            if self.touched.contains(&p) {
                self.order.push_back(p);
            } else {
                self.cache.remove(&p);
                self.queue
                    .stack
                    .lock()
                    .expect("lock poisoned")
                    .retain(|r| r.path != p);
            }
        }
        self.order.retain(|p| self.cache.contains_key(p));
        self.touched.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn directory() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "kestrel-text-thumbs-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn document_samples_support_source_markdown_unicode_and_extensionless_files() {
        let dir = directory();
        for (name, text) in [
            ("README.md", "# Hello\n\nA **small** document.\n"),
            ("build.py", "#!/usr/bin/env python3\nprint(\"hello\")\n"),
            ("main.rs", "fn main() { println!(\"hello\"); }\n"),
            ("app.ts", "const title: string = \"Aurora\";\n"),
            ("data.json", "{\"name\": \"Aurora\", \"count\": 3}\n"),
            ("notes.txt", "Привет, 世界!\nA second line.\n"),
            ("LICENSE", "A short license\nAll rights reserved.\n"),
            ("tool", "#!/bin/sh\necho hello\n"),
            (".gitignore", "target/\n*.log\n"),
        ] {
            let path = dir.join(name);
            fs::write(&path, text).unwrap();
            let job = text_thumbnail(&path).unwrap_or_else(|| panic!("missing {name}"));
            assert_eq!(job.text, text);
            assert!(!job.sections.is_empty());
        }
        let job = text_thumbnail(&dir.join("main.rs")).unwrap();
        let colors: HashSet<_> = job.sections.iter().map(|s| s.format.color).collect();
        assert!(colors.len() > 1, "code should retain syntax colors");
        assert!(
            renderer_for("ts").is_none(),
            "TypeScript uses document thumbnails"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn document_samples_bound_reads_lines_and_columns_without_breaking_utf8() {
        let dir = directory();
        let path = dir.join("large.txt");
        fs::write(
            &path,
            format!("{}\n{}", "界".repeat(300), "next line\n".repeat(1000)),
        )
        .unwrap();
        let job = text_thumbnail(&path).unwrap();
        assert_eq!(job.text.lines().count(), TEXT_LINES);
        assert_eq!(
            job.text.lines().next().unwrap().chars().count(),
            TEXT_COLUMNS
        );
        fs::write(
            &path,
            format!("{}界after the byte limit", "a".repeat(TEXT_BYTES - 1)),
        )
        .unwrap();
        assert!(
            text_thumbnail(&path).is_some(),
            "a byte cap may split the final character"
        );
        fs::write(&path, b"\xef\xbb\xbfhello\tworld\r\n").unwrap();
        assert_eq!(text_thumbnail(&path).unwrap().text, "hello    world\n");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn binary_empty_unreadable_and_nonregular_files_keep_their_icons() {
        use std::os::unix::fs::PermissionsExt;
        let dir = directory();
        for (name, bytes) in [
            ("binary.wgt", b"PK\x03\x04\0archive".as_slice()),
            ("invalid.txt", b"text\xff\xfe".as_slice()),
            ("incomplete.txt", b"text\xe7".as_slice()),
            ("control", b"text\x01more".as_slice()),
            ("empty", b"".as_slice()),
            ("whitespace", b" \t\n".as_slice()),
        ] {
            let path = dir.join(name);
            fs::write(&path, bytes).unwrap();
            assert!(text_thumbnail(&path).is_none(), "{name}");
        }
        assert!(text_thumbnail(&dir).is_none());
        assert!(text_thumbnail(&dir.join("missing")).is_none());
        let fifo = dir.join("fifo");
        let cpath = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: `cpath` is a NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
        assert!(text_thumbnail(&fifo).is_none());
        assert!(text_thumbnail(Path::new("/dev/null")).is_none());
        let locked = dir.join("locked.txt");
        fs::write(&locked, "not readable").unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        // SAFETY: `geteuid` has no preconditions.
        if unsafe { libc::geteuid() } != 0 {
            assert!(text_thumbnail(&locked).is_none());
        }
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o600)).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    fn empty_cache() -> Thumbs {
        let (_, rx) = channel();
        Thumbs {
            cache: HashMap::new(),
            order: VecDeque::new(),
            queue: Arc::new(Queue {
                stack: Mutex::new(Vec::new()),
                ready: Condvar::new(),
            }),
            rx,
            next_request: 0,
            touched: HashSet::new(),
        }
    }

    fn result(request: Request, text: &str) -> ResultMsg {
        ResultMsg {
            request,
            payload: Some(Payload::Text(egui::text::LayoutJob::simple(
                text.into(),
                egui::FontId::monospace(12.5),
                egui::Color32::WHITE,
                f32::INFINITY,
            ))),
        }
    }

    #[test]
    fn invalidated_cancelled_and_outdated_theme_results_cannot_refill_the_cache() {
        let ctx = egui::Context::default();
        let path = Path::new("demo.rs");
        let mut cache = empty_cache();
        cache.get_text(path);
        let old = cache
            .queue
            .stack
            .lock()
            .expect("lock poisoned")
            .pop()
            .unwrap();
        cache.invalidate(path);
        cache.get_text(path);
        let fresh = cache
            .queue
            .stack
            .lock()
            .expect("lock poisoned")
            .pop()
            .unwrap();
        cache.accept(&ctx, result(fresh, "fresh"));
        cache.accept(&ctx, result(old, "stale"));
        assert_eq!(cache.get_text(path).unwrap().text, "fresh");
        cache.invalidate(path);
        cache.get_text(path);
        let cancelled = cache
            .queue
            .stack
            .lock()
            .expect("lock poisoned")
            .pop()
            .unwrap();
        cache.cancel_pending();
        cache.accept(&ctx, result(cancelled, "cancelled"));
        assert!(!cache.cache.contains_key(path));
        cache.request(path, Kind::Text(crate::theme::generation().wrapping_sub(1)));
        let old_theme = cache
            .queue
            .stack
            .lock()
            .expect("lock poisoned")
            .pop()
            .unwrap();
        cache.accept(&ctx, result(old_theme, "old theme"));
        assert!(matches!(cache.cache[path].value, Thumb::Pending));
        let id = cache.cache[path].request;
        cache.get_text(path);
        assert_ne!(cache.cache[path].request, id);
    }

    #[test]
    fn pending_work_is_bounded_and_visible_entries_are_retained() {
        let ctx = egui::Context::default();
        let mut cache = empty_cache();
        for i in 0..CACHE_CAP {
            cache.get_text(Path::new(&format!("file{i}.txt")));
        }
        cache.poll(&ctx); // finish the frame that requested the original files
        let visible = Path::new("file0.txt");
        cache.get_text(visible);
        cache.get_text(Path::new("new.txt"));
        cache.poll(&ctx);
        assert_eq!(cache.cache.len(), CACHE_CAP);
        assert!(cache.cache.contains_key(visible));
        assert!(cache.cache.contains_key(Path::new("new.txt")));
        assert_eq!(
            cache.queue.stack.lock().expect("lock poisoned").len(),
            CACHE_CAP
        );
    }
}
