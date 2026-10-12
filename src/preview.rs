use crate::theme;
use eframe::egui::{self, text::LayoutJob, Color32, ColorImage, FontId, TextFormat};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::mpsc::{Receiver, Sender, channel};
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

const MAX_TEXT_BYTES: usize = 256 * 1024;
const MAX_TEXT_LINES: usize = 3000;
const MAX_LINE_CHARS: usize = 1000;
const MAX_IMAGE_SIDE: u32 = 1600;
const MAX_IMAGE_FILE: u64 = 256 * 1024 * 1024;

pub enum Content {
    Image { tex: egui::TextureHandle, w: u32, h: u32 },
    Text { job: LayoutJob, lines: usize, truncated: bool },
    Dir { items: Option<usize> },
    Binary,
    Error(String),
}

pub enum State {
    Loading,
    Ready(Content),
}

enum Payload {
    Image(ColorImage, u32, u32),
    Text(LayoutJob, usize, bool),
    Dir(Option<usize>),
    Binary,
    Error(String),
}

struct Request {
    id: u64,
    path: PathBuf,
    is_dir: bool,
}

struct Response {
    id: u64,
    payload: Payload,
}

/// Loads previews on a single background thread. Rapid selection changes
/// collapse to the most recent request, so scrolling never queues up work.
pub struct Preview {
    req_tx: Sender<Request>,
    res_rx: Receiver<Response>,
    next_id: u64,
    pub path: Option<PathBuf>,
    pub state: State,
}

impl Preview {
    pub fn new(ctx: egui::Context) -> Self {
        let (req_tx, req_rx) = channel::<Request>();
        let (res_tx, res_rx) = channel::<Response>();
        std::thread::Builder::new()
            .name("preview".into())
            .spawn(move || {
                while let Ok(mut req) = req_rx.recv() {
                    while let Ok(newer) = req_rx.try_recv() {
                        req = newer;
                    }
                    let payload = load(&req.path, req.is_dir);
                    if res_tx.send(Response { id: req.id, payload }).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
            })
            .expect("spawn preview thread");
        Self { req_tx, res_rx, next_id: 0, path: None, state: State::Loading }
    }

    pub fn request(&mut self, path: Option<(&Path, bool)>) {
        let Some((path, is_dir)) = path else {
            self.path = None;
            return;
        };
        if self.path.as_deref() == Some(path) {
            return;
        }
        self.next_id += 1;
        self.path = Some(path.to_path_buf());
        self.state = State::Loading;
        let _ = self.req_tx.send(Request { id: self.next_id, path: path.to_path_buf(), is_dir });
    }

    /// Forces a reload of the current path (e.g. after refresh).
    pub fn invalidate(&mut self) {
        self.path = None;
    }

    pub fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(res) = self.res_rx.try_recv() {
            if res.id != self.next_id {
                continue;
            }
            self.state = State::Ready(match res.payload {
                Payload::Image(img, w, h) => Content::Image {
                    tex: ctx.load_texture("preview", img, egui::TextureOptions::LINEAR),
                    w,
                    h,
                },
                Payload::Text(job, lines, truncated) => Content::Text { job, lines, truncated },
                Payload::Dir(items) => Content::Dir { items },
                Payload::Binary => Content::Binary,
                Payload::Error(e) => Content::Error(e),
            });
        }
    }
}

fn load(path: &Path, is_dir: bool) -> Payload {
    if is_dir {
        return Payload::Dir(crate::fsops::count_entries(path));
    }
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if matches!(
        ext.as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "ico" | "tif" | "tiff" | "tga" | "qoi"
            | "pnm" | "hdr" | "exr" | "avif"
    ) {
        return load_image(path);
    }
    load_text(path)
}

fn load_image(path: &Path) -> Payload {
    if std::fs::metadata(path).map(|m| m.len() > MAX_IMAGE_FILE).unwrap_or(false) {
        return Payload::Error("Image too large to preview".into());
    }
    let img = match image::ImageReader::open(path)
        .map_err(|e| e.to_string())
        .and_then(|r| r.with_guessed_format().map_err(|e| e.to_string()))
        .and_then(|r| r.decode().map_err(|e| e.to_string()))
    {
        Ok(img) => img,
        Err(e) => return Payload::Error(e),
    };
    let (w, h) = (img.width(), img.height());
    let img = if w > MAX_IMAGE_SIDE || h > MAX_IMAGE_SIDE {
        img.thumbnail(MAX_IMAGE_SIDE, MAX_IMAGE_SIDE)
    } else {
        img
    };
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Payload::Image(ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()), w, h)
}

fn load_text(path: &Path) -> Payload {
    let mut buf = Vec::new();
    let file_len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    match std::fs::File::open(path) {
        Ok(f) => {
            if let Err(e) = f.take(MAX_TEXT_BYTES as u64).read_to_end(&mut buf) {
                return Payload::Error(e.to_string());
            }
        }
        Err(e) => return Payload::Error(e.to_string()),
    }
    if buf[..buf.len().min(8192)].contains(&0) {
        return Payload::Binary;
    }
    let text = String::from_utf8_lossy(&buf).replace('\t', "    ");
    let mut truncated = file_len as usize > buf.len();

    static SYNTAX: OnceLock<(SyntaxSet, Theme)> = OnceLock::new();
    let (ss, theme) = SYNTAX.get_or_init(|| {
        let ss = SyntaxSet::load_defaults_newlines();
        let mut ts = ThemeSet::load_defaults();
        let theme = ts.themes.remove("base16-ocean.dark").expect("bundled theme");
        (ss, theme)
    });
    let syntax = ss
        .find_syntax_for_file(path)
        .ok()
        .flatten()
        .or_else(|| text.lines().next().and_then(|l| ss.find_syntax_by_first_line(l)))
        .unwrap_or_else(|| ss.find_syntax_plain_text());
    let mut hl = HighlightLines::new(syntax, theme);

    let font = FontId::monospace(12.5);
    let mut job = LayoutJob::default();
    let gutter = TextFormat { font_id: font.clone(), color: theme::FAINT, ..Default::default() };
    let mut lines = 0;
    for (n, line) in LinesWithEndings::from(&text).enumerate() {
        if n >= MAX_TEXT_LINES {
            truncated = true;
            break;
        }
        lines += 1;
        job.append(&format!("{:>4}  ", n + 1), 0.0, gutter.clone());
        let line = if line.chars().count() > MAX_LINE_CHARS {
            let cut: String = line.chars().take(MAX_LINE_CHARS).collect();
            std::borrow::Cow::Owned(cut + "…\n")
        } else {
            std::borrow::Cow::Borrowed(line)
        };
        match hl.highlight_line(&line, ss) {
            Ok(ranges) => {
                for (style, piece) in ranges {
                    let c = style.foreground;
                    job.append(
                        piece,
                        0.0,
                        TextFormat {
                            font_id: font.clone(),
                            color: Color32::from_rgb(c.r, c.g, c.b),
                            ..Default::default()
                        },
                    );
                }
            }
            Err(_) => job.append(
                &line,
                0.0,
                TextFormat { font_id: font.clone(), color: theme::TEXT, ..Default::default() },
            ),
        }
    }
    Payload::Text(job, lines, truncated)
}
