use crate::theme;
use crate::thumbs::{self, Renderer};
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

/// Extra metadata rows shown under the preview (dimensions, duration, pages…).
pub type Info = Vec<(String, String)>;

pub enum Content {
    Image { tex: egui::TextureHandle, info: Info },
    Text { job: LayoutJob, lines: usize, truncated: bool },
    Dir { items: Option<usize> },
    Binary { info: Info },
    Error(String),
}

pub enum State {
    Loading,
    Ready(Content),
}

enum Payload {
    Image(ColorImage, Info),
    Text(LayoutJob, usize, bool),
    Dir(Option<usize>),
    Binary(Info),
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
                Payload::Image(img, info) => Content::Image {
                    tex: ctx.load_texture("preview", img, egui::TextureOptions::LINEAR),
                    info,
                },
                Payload::Text(job, lines, truncated) => Content::Text { job, lines, truncated },
                Payload::Dir(items) => Content::Dir { items },
                Payload::Binary(info) => Content::Binary { info },
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
    if let Some(renderer) = thumbs::renderer_for(&ext) {
        let mut info = Info::new();
        match renderer {
            Renderer::Video => info = media_rows(path),
            Renderer::Pdf => {
                if let Some(n) = thumbs::pdf_pages(path) {
                    info.push(("Pages".into(), n.to_string()));
                }
            }
            _ => {}
        }
        return match thumbs::render(path, renderer, MAX_IMAGE_SIDE) {
            Ok(r) => {
                if let (Some((w, h)), true) = (r.dims, renderer == Renderer::Raster) {
                    info.insert(0, ("Dimensions".into(), format!("{w} × {h}")));
                }
                Payload::Image(r.image, info)
            }
            Err(e) if renderer == Renderer::Raster => Payload::Error(e),
            Err(_) => Payload::Binary(info),
        };
    }
    if matches!(crate::util::category(&ext, false), crate::util::Category::Audio) {
        return Payload::Binary(media_rows(path));
    }
    load_text(path)
}

/// Human-friendly rows from ffprobe output.
fn media_rows(path: &Path) -> Info {
    let raw = thumbs::media_info(path);
    let get = |k: &str| raw.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
    let mut info = Info::new();
    if let (Some(w), Some(h)) = (get("width"), get("height")) {
        info.push(("Resolution".into(), format!("{w} × {h}")));
    }
    if let Some(d) = get("duration").and_then(|d| d.parse::<f64>().ok()) {
        let s = d.round() as u64;
        let text = if s >= 3600 {
            format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
        } else {
            format!("{}:{:02}", s / 60, s % 60)
        };
        info.push(("Duration".into(), text));
    }
    let codecs: Vec<String> = raw.iter().filter(|(k, _)| k == "codec_name").map(|(_, v)| v.clone()).collect();
    if !codecs.is_empty() {
        info.push(("Codecs".into(), codecs.join(", ")));
    }
    if let Some(b) = get("bit_rate").and_then(|b| b.parse::<u64>().ok()) {
        info.push(("Bitrate".into(), format!("{} kb/s", b / 1000)));
    }
    if let Some(r) = get("sample_rate") {
        info.push(("Sample rate".into(), format!("{r} Hz")));
    }
    for (key, label) in [("title", "Title"), ("artist", "Artist"), ("album", "Album")] {
        if let Some(v) = get(key) {
            info.push((label.into(), v));
        }
    }
    info
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
        return Payload::Binary(Info::new());
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
