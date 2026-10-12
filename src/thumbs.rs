//! Rendering files to images (raster images in-process; SVG, PDF and video via
//! rsvg-convert, pdftoppm and ffmpeg when installed) plus a thumbnail cache for
//! the grid view.

use eframe::egui::{self, ColorImage};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Read;
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
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "ico" | "tif" | "tiff" | "tga" | "qoi"
        | "pnm" | "hdr" | "exr" | "avif" => Renderer::Raster,
        "svg" | "svgz" => Renderer::Svg,
        "pdf" => Renderer::Pdf,
        "mp4" | "mkv" | "webm" | "avi" | "mov" | "wmv" | "flv" | "m4v" | "mpg" | "mpeg" | "ts" => {
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
            let dur = info.iter().find(|(k, _)| k == "duration").and_then(|(_, v)| v.parse::<f64>().ok());
            let at = dur.map(|d| (d * 0.1).min(30.0)).unwrap_or(0.0);
            let scale = format!("scale='min({max},iw)':-2");
            let frame = |t: f64| {
                run_tool(
                    Command::new("ffmpeg")
                        .args(["-v", "error", "-ss", &format!("{t:.2}"), "-i"])
                        .arg(path)
                        .args(["-frames:v", "1", "-vf", &scale, "-f", "image2pipe", "-c:v", "png", "-"]),
                )
            };
            let png = frame(at).or_else(|_| frame(0.0))?;
            let w = info.iter().find(|(k, _)| k == "width").and_then(|(_, v)| v.parse().ok());
            let h = info.iter().find(|(k, _)| k == "height").and_then(|(_, v)| v.parse().ok());
            decode_png(&png, w.zip(h))
        }
    }
}

fn render_raster(path: &Path, max: u32) -> Result<Rendered, String> {
    if std::fs::metadata(path).map(|m| m.len() > MAX_IMAGE_FILE).unwrap_or(false) {
        return Err("Image too large to preview".into());
    }
    let img = image::ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .decode()
        .map_err(|e| e.to_string())?;
    let dims = (img.width(), img.height());
    let img = if dims.0 > max || dims.1 > max { img.thumbnail(max, max) } else { img };
    Ok(Rendered { image: to_color_image(img), dims: Some(dims) })
}

fn decode_png(bytes: &[u8], dims: Option<(u32, u32)>) -> Result<Rendered, String> {
    let img = image::load_from_memory(bytes).map_err(|e| e.to_string())?;
    let dims = dims.or(Some((img.width(), img.height())));
    Ok(Rendered { image: to_color_image(img), dims })
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
            Ok(None) if start.elapsed() > TOOL_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Timed out".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(e) => return Err(e.to_string()),
        }
    }
    let buf = reader.join().unwrap_or_default();
    if buf.is_empty() { Err("No output".into()) } else { Ok(buf) }
}

// ---------------------------------------------------------------- cache

pub enum Thumb {
    Pending,
    Ready(egui::TextureHandle),
    Failed,
}

/// Most recently requested first, so the visible tiles render before anything
/// that scrolled out of view.
struct Queue {
    stack: Mutex<Vec<(PathBuf, Renderer)>>,
    ready: Condvar,
}

pub struct Thumbs {
    cache: HashMap<PathBuf, Thumb>,
    order: VecDeque<PathBuf>,
    queue: Arc<Queue>,
    rx: Receiver<(PathBuf, Option<ColorImage>)>,
    /// Requested this frame; protected from eviction.
    touched: HashSet<PathBuf>,
}

impl Thumbs {
    pub fn new(ctx: egui::Context) -> Self {
        let queue = Arc::new(Queue { stack: Mutex::new(Vec::new()), ready: Condvar::new() });
        let (tx, rx) = channel();
        for i in 0..WORKERS {
            let (queue, tx, ctx): (_, Sender<_>, _) = (queue.clone(), tx.clone(), ctx.clone());
            std::thread::Builder::new()
                .name(format!("thumb-{i}"))
                .spawn(move || {
                    loop {
                        let job = {
                            let mut stack = queue.stack.lock().unwrap();
                            loop {
                                if let Some(job) = stack.pop() {
                                    break job;
                                }
                                stack = queue.ready.wait(stack).unwrap();
                            }
                        };
                        let img = render(&job.0, job.1, THUMB_SIZE).ok().map(|r| r.image);
                        if tx.send((job.0, img)).is_err() {
                            return;
                        }
                        ctx.request_repaint();
                    }
                })
                .expect("spawn thumbnail thread");
        }
        Self { cache: HashMap::new(), order: VecDeque::new(), queue, rx, touched: HashSet::new() }
    }

    /// Returns the thumbnail if ready, scheduling it otherwise.
    pub fn get(&mut self, path: &Path, renderer: Renderer) -> Option<&egui::TextureHandle> {
        self.touched.insert(path.to_path_buf());
        if !self.cache.contains_key(path) {
            self.cache.insert(path.to_path_buf(), Thumb::Pending);
            self.order.push_back(path.to_path_buf());
            self.queue.stack.lock().unwrap().push((path.to_path_buf(), renderer));
            self.queue.ready.notify_one();
        }
        match self.cache.get(path) {
            Some(Thumb::Ready(t)) => Some(t),
            _ => None,
        }
    }

    /// Drops queued work that is no longer on screen (e.g. after navigating away).
    pub fn cancel_pending(&mut self) {
        let dropped: Vec<PathBuf> = std::mem::take(&mut *self.queue.stack.lock().unwrap())
            .into_iter()
            .map(|(p, _)| p)
            .collect();
        for p in dropped {
            self.cache.remove(&p);
        }
    }

    pub fn invalidate(&mut self, path: &Path) {
        self.cache.remove(path);
    }

    pub fn poll(&mut self, ctx: &egui::Context) {
        while let Ok((path, img)) = self.rx.try_recv() {
            if !self.cache.contains_key(&path) {
                continue; // cancelled meanwhile
            }
            let thumb = match img {
                Some(img) => Thumb::Ready(ctx.load_texture(
                    format!("thumb:{}", path.display()),
                    img,
                    egui::TextureOptions::LINEAR,
                )),
                None => Thumb::Failed,
            };
            self.cache.insert(path, thumb);
        }
        // Evict oldest entries that weren't on screen last frame.
        let mut guard = self.order.len();
        while self.cache.len() > CACHE_CAP && guard > 0 {
            guard -= 1;
            let Some(p) = self.order.pop_front() else { break };
            if self.touched.contains(&p) {
                self.order.push_back(p);
            } else if !matches!(self.cache.get(&p), Some(Thumb::Pending)) {
                self.cache.remove(&p);
            } else {
                self.order.push_back(p);
            }
        }
        self.order.retain(|p| self.cache.contains_key(p));
        self.touched.clear();
    }
}
