use crate::theme;
use crate::thumbs::{self, Renderer};
use eframe::egui::{self, text::LayoutJob, Color32, ColorImage, FontId, TextFormat};
use std::io::Read;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use std::sync::mpsc::{Receiver, Sender, channel};
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

const MAX_TEXT_BYTES: usize = 256 * 1024;
const MAX_TEXT_LINES: usize = 3000;
const MAX_IMAGE_SIDE: u32 = 1600;
const MAX_TABLE_ROWS: usize = 5000;
const MAX_CELL_CHARS: usize = 200;
const LOAD_TIMEOUT: Duration = Duration::from_secs(20);

/// Extra metadata rows shown under the preview (dimensions, duration, pages…).
pub type Info = Vec<(String, String)>;

pub enum Content {
    Image { tex: egui::TextureHandle, info: Info },
    Text { job: LayoutJob, links: Vec<TextLink>, lines: usize, truncated: bool, plain: bool, layout: Box<CodeLayout> },
    Table(Table),
    Dir { entries: Vec<crate::fsops::Entry> },
    Binary { info: Info },
    Error(String),
}

/// Prepared text geometry belongs to a single immutable preview payload. Replacing
/// that payload (file change, theme reload, or navigation) discards this cache.
#[derive(Default)]
pub struct CodeLayout {
    pub(crate) chunks: Option<Box<ChunkLayout>>,
    pub(crate) galley: Option<Arc<egui::Galley>>,
    pub(crate) rows: Vec<CodeRow>,
    pub(crate) width: f32,
    pub(crate) pending_width: f32,
    pub(crate) changed_at: f64,
    pub(crate) pixels_per_point: f32,
    #[cfg(test)]
    pub(crate) builds: usize,
    #[cfg(test)]
    pub(crate) chunk_builds: usize,
}

/// Chunk geometry is disposable; selection always addresses the immutable document.
#[derive(Default)]
pub(crate) struct ChunkLayout {
    pub chunks: Vec<TextChunk>,
    pub selection: Option<(usize, usize)>,
    pub selecting: bool,
    pub active: bool,
    pub rect: Option<egui::Rect>,
    pub total_chars: usize,
    #[cfg(test)]
    pub scroll_id: Option<egui::Id>,
}

pub(crate) struct TextChunk {
    pub bytes: Range<usize>,
    pub chars: usize,
    pub line: usize,
    pub starts_line: bool,
    pub height: f32,
    pub width: f32,
    pub galley: Option<Arc<egui::Galley>>,
}

pub(crate) struct CodeRow {
    pub line: usize,
    pub chars: usize,
    pub starts_line: bool,
}

pub enum State {
    Loading,
    Ready(Content),
}

enum Payload {
    Image(ColorImage, Info),
    Text(LayoutJob, Vec<TextLink>, usize, bool, bool),
    Table(Table),
    Dir(Vec<crate::fsops::Entry>),
    Binary(Info),
    Error(String),
}

struct Request {
    id: u64,
    path: PathBuf,
    is_dir: bool,
}

/// A parsed CSV/TSV file; the first row is the header.
pub struct Table {
    pub rows: Vec<Vec<String>>,
    /// Widest cell per column, in chars.
    pub widths: Vec<usize>,
    pub truncated: bool,
}

pub struct TextLink {
    pub url: String,
    pub chars: Range<usize>,
}

/// Find web URLs in the rendered text.
pub fn text_links(text: &str) -> Vec<TextLink> {
    let mut links = Vec::new();
    let mut scan = text.char_indices().peekable();
    let mut chars = 0;
    while let Some((start, c)) = scan.next() {
        let char_start = chars;
        chars += 1;
        if c != 'h' || !(text[start..].starts_with("https://") || text[start..].starts_with("http://")) {
            continue;
        }
        let mut end = start + 1;
        let mut count = 1;
        let mut balance = [0isize; 3];
        while let Some(&(at, ch)) = scan.peek() {
            if ch.is_whitespace() || matches!(ch, '<' | '>' | '"' | '\'' | '`') { break; }
            match ch {
                '(' => balance[0] += 1, '[' => balance[1] += 1, '{' => balance[2] += 1,
                ')' => balance[0] -= 1, ']' => balance[1] -= 1, '}' => balance[2] -= 1,
                _ => {}
            }
            scan.next();
            end = at + ch.len_utf8();
            count += 1;
            chars += 1;
        }
        let mut url = &text[start..end];
        // Each removed ASCII character is visited once, including punctuation
        // after an unmatched closing delimiter (e.g. Markdown's ").").
        while let Some(last) = url.as_bytes().last().copied() {
            let close = match last { b')' => Some(0), b']' => Some(1), b'}' => Some(2), _ => None };
            if matches!(last, b'.' | b',' | b';' | b':' | b'!' | b'?') {
                url = &url[..url.len() - 1]; count -= 1;
            } else if let Some(i) = close.filter(|&i| balance[i] < 0) {
                balance[i] += 1;
                url = &url[..url.len() - 1]; count -= 1;
            } else { break; }
        }
        let scheme_len = if url.starts_with("https://") { 8 } else { 7 };
        if url.len() > scheme_len && !url[scheme_len..].starts_with(['/', ')', ']', '}']) {
            links.push(TextLink { url: url.to_owned(), chars: char_start..char_start + count });
        }
    }
    links
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_formatting_preserves_tokens_and_rejects_invalid_input() {
        let raw = r#"{"z":123456789012345678901234567890,"z":1e+400,"a":[{},[],"Привет\\\"",true,null]}"#;
        let formatted = pretty_json(raw).unwrap();
        assert_eq!(formatted, concat!(
            "{\n",
            "  \"z\": 123456789012345678901234567890,\n",
            "  \"z\": 1e+400,\n",
            "  \"a\": [\n",
            "    {},\n",
            "    [],\n",
            "    \"Привет\\\\\\\"\",\n",
            "    true,\n",
            "    null\n",
            "  ]\n",
            "}",
        ));
        assert_eq!(pretty_json(&formatted).unwrap(), formatted);
        for raw in ["", "{", "[1,]", "{} {}", "{\"x\": NaN}"] {
            assert!(pretty_json(raw).is_none(), "accepted {raw:?}");
        }
        assert_eq!(pretty_json(" [ ] ").as_deref(), Some("[]"));
        assert_eq!(pretty_json(" true ").as_deref(), Some("true"));
    }

    #[test]
    fn json_preview_formats_only_complete_json_without_writing_the_file() {
        let dir = std::env::temp_dir().join(format!("kestrel-json-preview-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (name, raw, expected) in [
            ("data.json", "{\"b\":1,\"a\":[2,3]}", "{\n  \"b\": 1,\n  \"a\": [\n    2,\n    3\n  ]\n}"),
            ("data.JSON", "{\"a\":true}", "{\n  \"a\": true\n}"),
            ("invalid.json", "{\"a\":", "{\"a\":"),
            ("raw.txt", "{\"a\":true}", "{\"a\":true}"),
        ] {
            let path = dir.join(name);
            std::fs::write(&path, raw).unwrap();
            let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
            let Payload::Text(job, _, _, truncated, _) = load_text(&path) else { panic!("no text preview for {name}") };
            assert_eq!(job.text, expected);
            assert!(!truncated);
            assert_eq!(std::fs::read(&path).unwrap(), raw.as_bytes());
            assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), modified);
        }
        // A bounded read ending in valid JSON must still not format a partial file.
        let path = dir.join("large.json");
        let raw = format!("{{\"a\":1}}{}trailing", " ".repeat(MAX_TEXT_BYTES));
        std::fs::write(&path, &raw).unwrap();
        let Payload::Text(job, _, _, truncated, _) = load_text(&path) else { panic!("no text preview") };
        assert!(truncated);
        assert_eq!(job.text, &raw[..MAX_TEXT_BYTES]);
        let path = dir.join("long.txt");
        let raw = "слово ".repeat(500);
        std::fs::write(&path, &raw).unwrap();
        let Payload::Text(job, _, _, truncated, _) = load_text(&path) else { panic!("no text preview") };
        assert_eq!(job.text, raw, "long lines must wrap without losing content");
        assert!(!truncated);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn web_links_handle_unicode_and_surrounding_punctuation() {
        let text = "   1  Привет [site](https://example.com/a_(b)).\n   2  <http://example.org/?a=1&b=2>, file:///tmp/a https://";
        let links = text_links(text);
        assert_eq!(links.iter().map(|l| l.url.as_str()).collect::<Vec<_>>(),
            vec!["https://example.com/a_(b)", "http://example.org/?a=1&b=2"]);
        for link in links {
            let selected: String = text.chars().skip(link.chars.start).take(link.chars.len()).collect();
            assert_eq!(selected, link.url);
        }
    }

    #[test]
    fn code_link_policy_uses_extension_filename_and_shebang() {
        let ss = syntax_set();
        let source = "// https://example.com/source\nconst url = \"https://example.com/string\";";
        for name in ["file.js", "file.JS", "file.rs", "data.json", "Makefile", "Dockerfile", "CMakeLists.txt", "file.ts"] {
            assert!(detect_syntax(ss, Path::new(name), source).1, "{name}");
        }
        assert!(detect_syntax(ss, Path::new("script"), "#!/usr/bin/env python3\nprint('https://example.com')").1);
        for name in ["README.md", "notes.TXT", "data.csv", "data.tsv", "notes.rst"] {
            assert!(!detect_syntax(ss, Path::new(name), source).1, "{name}");
        }
        let dir = std::env::temp_dir().join(format!("kestrel-link-policy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (name, expected) in [("code.js", 0), ("code.RS", 0), ("data.json", 0), ("README.md", 2), ("notes.txt", 2)] {
            let path = dir.join(name);
            std::fs::write(&path, source).unwrap();
            let Payload::Text(_, links, _, _, _) = load_text(&path) else { panic!("expected text") };
            assert_eq!(links.len(), expected, "{name}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn bounded_highlighting_preserves_text_and_stops_obsolete_work() {
        let font = FontId::monospace(12.5);
        let text = format!("const value = \"{}\";\nnext line\n", "я".repeat(20_001));
        let (job, lines, plain, code) = highlight_bounded(Path::new("large.js"), &text, 3000, font.clone(), &|| false);
        assert!(plain && code);
        assert_eq!(job.text, text);
        assert_eq!(lines, 2);
        assert_eq!(job.sections.len(), 1);
        let checked = std::cell::Cell::new(0);
        let source = "/* comment\ncontinued comment */\nconst value = 42;\n".repeat(1000);
        let (job, lines, _, _) = highlight_bounded(Path::new("source.js"), &source, 3000, font, &|| {
            checked.set(checked.get() + 1);
            checked.get() > 3
        });
        assert_eq!(lines, 3);
        assert_eq!(job.text, "/* comment\ncontinued comment */\nconst value = 42;\n");
        let comment = job.sections.iter().find(|s| s.byte_range.contains(&egui::text::ByteIndex(0))).unwrap().format.color;
        let continued = job.sections.iter().find(|s| s.byte_range.contains(&egui::text::ByteIndex(12))).unwrap().format.color;
        assert_eq!(comment, continued, "multiline parser state was lost");
    }

    #[test]
    fn web_links_scan_mixed_schemes_and_long_delimiters() {
        let text = format!("я https://example.com/a_(b)). http://example.org/x{} https://end.test/path", ")".repeat(20_000));
        let links = text_links(&text);
        assert_eq!(links.iter().map(|l| l.url.as_str()).collect::<Vec<_>>(),
            ["https://example.com/a_(b)", "http://example.org/x", "https://end.test/path"]);
        for link in links {
            assert_eq!(text.chars().skip(link.chars.start).take(link.chars.len()).collect::<String>(), link.url);
        }
    }

    #[test]
    fn csv_quotes_newlines_and_delimiters() {
        let text = "name,note\r\n\"Smith, J\",\"said \"\"hi\"\"\nthen left\"\nДоe,\n";
        let (rows, complete) = parse_table(text, sniff_delimiter(text));
        assert!(complete);
        assert_eq!(rows, vec![
            vec!["name", "note"],
            vec!["Smith, J", "said \"hi\" then left"],
            vec!["Доe", ""],
        ]);
        assert_eq!(sniff_delimiter("a;b;\"c,d,e\"\n"), ';');
        assert_eq!(sniff_delimiter("a\tb\n"), '\t');
    }
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
    generation: Arc<AtomicU64>,
    pub path: Option<PathBuf>,
    /// Path being reloaded in place; its old content stays visible meanwhile.
    reloading: Option<PathBuf>,
    pub state: State,
}

impl Preview {
    pub fn new(ctx: egui::Context) -> Self {
        let (req_tx, req_rx) = channel::<Request>();
        let (res_tx, res_rx) = channel::<Response>();
        let generation = Arc::new(AtomicU64::new(0));
        let worker_generation = generation.clone();
        std::thread::Builder::new()
            .name("preview".into())
            .spawn(move || {
                while let Ok(mut req) = req_rx.recv() {
                    while let Ok(newer) = req_rx.try_recv() {
                        req = newer;
                    }
                    let cancelled = || worker_generation.load(Ordering::Relaxed) != req.id;
                    if cancelled() { continue; }
                    // Loads run on their own thread so a stuck read or tool can never
                    // block later previews; an abandoned load just finishes unseen.
                    let (done_tx, done_rx) = channel();
                    let (path, is_dir, id, generation) = (req.path.clone(), req.is_dir, req.id, worker_generation.clone());
                    let spawned = std::thread::Builder::new().name("preview-load".into()).spawn(move || {
                        let for_tools = generation.clone();
                        thumbs::set_cancel(Box::new(move || for_tools.load(Ordering::Relaxed) != id));
                        let cancelled = move || generation.load(Ordering::Relaxed) != id;
                        let _ = done_tx.send(load_cancellable(&path, is_dir, &cancelled));
                    });
                    let started = Instant::now();
                    let payload = loop {
                        if spawned.is_err() { break Some(Payload::Error("Could not start preview".into())); }
                        if cancelled() { break None; }
                        match done_rx.recv_timeout(Duration::from_millis(50)) {
                            Ok(payload) => break Some(payload),
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout) if started.elapsed() > LOAD_TIMEOUT => {
                                break Some(Payload::Error("Preview timed out".into()));
                            }
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                                break Some(Payload::Error("Preview failed".into()));
                            }
                        }
                    };
                    let Some(payload) = payload else { continue };
                    if cancelled() { continue; }
                    if res_tx.send(Response { id: req.id, payload }).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
            })
            .expect("spawn preview thread");
        Self { req_tx, res_rx, next_id: 0, generation, path: None, reloading: None, state: State::Loading }
    }

    pub fn request(&mut self, path: Option<(&Path, bool)>) {
        let Some((path, is_dir)) = path else {
            self.generation.fetch_add(1, Ordering::Relaxed);
            self.next_id = self.generation.load(Ordering::Relaxed);
            self.path = None;
            self.reloading = None;
            return;
        };
        if self.path.as_deref() == Some(path) {
            return;
        }
        self.next_id = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        self.path = Some(path.to_path_buf());
        if self.reloading.take().as_deref() != Some(path) {
            self.state = State::Loading;
        }
        let _ = self.req_tx.send(Request { id: self.next_id, path: path.to_path_buf(), is_dir });
    }

    /// Forces a reload of the current path (e.g. after refresh).
    pub fn invalidate(&mut self) {
        self.next_id = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        if let Some(path) = self.path.take() { self.reloading = Some(path); }
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
                Payload::Text(job, links, lines, truncated, plain) => Content::Text { job, links, lines, truncated, plain, layout: Box::default() },
                Payload::Table(table) => Content::Table(table),
                Payload::Dir(entries) => Content::Dir { entries },
                Payload::Binary(info) => Content::Binary { info },
                Payload::Error(e) => Content::Error(e),
            });
        }
    }
}

fn load_cancellable(path: &Path, is_dir: bool, cancelled: &dyn Fn() -> bool) -> Payload {
    if is_dir {
        return match crate::fsops::list_dir(path) {
            Ok(mut entries) => {
                entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir)
                    .then_with(|| crate::util::natural_cmp(&a.name, &b.name)));
                Payload::Dir(entries)
            }
            Err(error) => Payload::Error(error),
        };
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
    if matches!(ext.as_str(), "csv" | "tsv")
        && let Some(table) = load_table(path, ext == "tsv")
    {
        return table;
    }
    load_text_cancellable(path, cancelled)
}

/// Opens only regular files, without blocking on FIFOs, devices or dead mounts' open path.
fn open_regular(path: &Path) -> Result<std::fs::File, String> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(path)
        .map_err(|e| e.to_string())?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("Not a regular file".into());
    }
    Ok(file)
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

/// Validate first, then change only whitespace outside strings. Keeping the raw
/// tokens preserves key order, duplicate keys, number precision and escapes.
fn pretty_json(text: &str) -> Option<String> {
    serde_json::from_str::<&serde_json::value::RawValue>(text).ok()?;
    let mut out = String::new();
    let (mut depth, mut quoted, mut escaped) = (0usize, false, false);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                quoted = false;
            }
        } else {
            match c {
                '"' => { quoted = true; out.push(c); }
                '{' | '[' => {
                    out.push(c);
                    while chars.peek().is_some_and(|c| c.is_ascii_whitespace()) { chars.next(); }
                    let close = if c == '{' { '}' } else { ']' };
                    if chars.peek() == Some(&close) {
                        out.push(chars.next().unwrap());
                    } else {
                        depth += 1;
                        out.push('\n');
                        out.push_str(&"  ".repeat(depth));
                    }
                }
                '}' | ']' => {
                    depth -= 1;
                    out.push('\n');
                    out.push_str(&"  ".repeat(depth));
                    out.push(c);
                }
                ',' => {
                    out.push_str(",\n");
                    out.push_str(&"  ".repeat(depth));
                }
                ':' => out.push_str(": "),
                c if c.is_ascii_whitespace() => {}
                _ => out.push(c),
            }
        }
        // Deeply nested input can expand considerably when indented.
        if out.len() > MAX_TEXT_BYTES * 4 { return None; }
    }
    Some(out)
}

#[cfg(test)]
fn load_text(path: &Path) -> Payload { load_text_cancellable(path, &|| false) }

fn load_text_cancellable(path: &Path, cancelled: &dyn Fn() -> bool) -> Payload {
    let mut buf = Vec::new();
    let file_len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    match open_regular(path) {
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
    let mut truncated = file_len as usize > buf.len();
    let raw = String::from_utf8_lossy(&buf);
    let formatted = if !truncated && path.extension().is_some_and(|e| e.eq_ignore_ascii_case("json")) {
        pretty_json(&raw)
    } else {
        None
    };
    let text = formatted.as_deref().unwrap_or(&raw).replace('\t', "    ");

    let (job, lines, plain, code) = highlight_bounded(path, &text, MAX_TEXT_LINES, FontId::monospace(12.5), cancelled);
    truncated |= text.lines().count() > MAX_TEXT_LINES;
    let links = if code || cancelled() { Vec::new() } else { text_links(&job.text) };
    Payload::Text(job, links, lines, truncated, plain)
}

/// Shared bounded, theme-aware syntax layout for previews and document thumbnails.
/// Does not read the file: callers control the sample and line budget.
pub(crate) fn highlight_text(path: &Path, text: &str, max_lines: usize, font: FontId) -> (LayoutJob, usize) {
    let (job, lines, _, _) = highlight_bounded(path, text, max_lines, font, &|| false);
    (job, lines)
}

fn syntax_set() -> &'static SyntaxSet {
    static SYNTAX: OnceLock<SyntaxSet> = OnceLock::new();
    SYNTAX.get_or_init(SyntaxSet::load_defaults_newlines)
}

/// Detection is shared by highlighting and link policy; explicit prose extensions
/// stay prose even if their first line resembles source code.
fn detect_syntax<'a>(ss: &'a SyntaxSet, path: &Path, text: &str) -> (&'a syntect::parsing::SyntaxReference, bool) {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let syntax = ss.find_syntax_by_extension(&ext)
        .or_else(|| ss.find_syntax_by_extension(name))
        .or_else(|| text.lines().next().and_then(|l| ss.find_syntax_by_first_line(l)))
        .unwrap_or_else(|| ss.find_syntax_plain_text());
    let prose = !name.eq_ignore_ascii_case("CMakeLists.txt") && matches!(ext.as_str(), "md" | "markdown" | "txt" | "log" | "csv" | "tsv" | "rst" | "org");
    let code = !prose && (crate::util::is_code_ext(&ext)
        || !std::ptr::eq(syntax, ss.find_syntax_plain_text())
        || matches!(name.to_ascii_lowercase().as_str(), "makefile" | "gnumakefile" | "dockerfile" | "cmakelists.txt")
        || text.starts_with("#!"));
    (syntax, code)
}

fn highlight_bounded(path: &Path, text: &str, max_lines: usize, font: FontId, cancelled: &dyn Fn() -> bool) -> (LayoutJob, usize, bool, bool) {
    let ss = syntax_set();
    let (syntax, code) = detect_syntax(ss, path, text);
    let theme = syntax_theme();
    let mut hl = HighlightLines::new(syntax, &theme);
    let mut job = LayoutJob::default();
    let mut lines = 0;
    let mut end = 0;
    let mut plain = false;
    let started = Instant::now();
    for line in LinesWithEndings::from(text).take(max_lines) {
        if cancelled() { break; }
        lines += 1;
        end += line.len();
        if !plain && (line.chars().count() > 20_000 || started.elapsed() > Duration::from_millis(100)) {
            plain = true;
        }
        if plain { continue; }
        match hl.highlight_line(line, ss) {
            Ok(ranges) => {
                for (style, piece) in ranges {
                    let c = style.foreground;
                    job.append(piece, 0.0, TextFormat {
                        font_id: font.clone(), color: Color32::from_rgb(c.r, c.g, c.b), ..Default::default()
                    });
                }
                if job.sections.len() > 32_768 || started.elapsed() > Duration::from_millis(100) { plain = true; }
            }
            Err(_) => plain = true,
        }
    }
    if plain {
        job = LayoutJob::simple(text[..end].to_owned(), font, theme::pal().text, f32::INFINITY);
    }
    (job, lines, plain, code)
}

/// Parses a CSV/TSV file, or `None` to fall back to the plain-text preview.
fn load_table(path: &Path, tsv: bool) -> Option<Payload> {
    let file_len = std::fs::metadata(path).ok()?.len();
    let mut buf = Vec::new();
    open_regular(path).ok()?.take(MAX_TEXT_BYTES as u64).read_to_end(&mut buf).ok()?;
    if buf[..buf.len().min(8192)].contains(&0) {
        return None;
    }
    let text = String::from_utf8_lossy(&buf);
    let delim = if tsv { '\t' } else { sniff_delimiter(&text) };
    let (rows, complete) = parse_table(&text, delim);
    if rows.is_empty() {
        return None;
    }
    let mut widths = vec![0; rows.iter().map(Vec::len).max().unwrap_or(0)];
    for row in &rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }
    let truncated = !complete || file_len as usize > buf.len();
    Some(Payload::Table(Table { rows, widths, truncated }))
}

/// Picks the separator that occurs most on the first line (`,` `;` tab or `|`).
fn sniff_delimiter(text: &str) -> char {
    let first = text.lines().next().unwrap_or("");
    let mut quoted = false;
    let mut counts = [(',', 0), (';', 0), ('\t', 0), ('|', 0)];
    for c in first.chars() {
        if c == '"' {
            quoted = !quoted;
        } else if !quoted && let Some(n) = counts.iter_mut().find(|(d, _)| *d == c) {
            n.1 += 1;
        }
    }
    counts.iter().fold((',', 0), |best, &n| if n.1 > best.1 { n } else { best }).0
}

/// RFC 4180-style parsing: quoted fields may hold separators, `""` and newlines.
/// Returns the rows and whether the whole text was consumed.
fn parse_table(text: &str, delim: char) -> (Vec<Vec<String>>, bool) {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut cell = String::new();
    let mut chars = text.chars().peekable();
    let mut quoted = false;
    let push = |cell: &mut String, row: &mut Vec<String>| {
        let mut c = std::mem::take(cell);
        if c.chars().count() > MAX_CELL_CHARS {
            c = c.chars().take(MAX_CELL_CHARS).collect::<String>() + "…";
        }
        row.push(c);
    };
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    cell.push('"');
                }
                '"' => quoted = false,
                '\r' => {}
                '\n' | '\t' => cell.push(' '),
                c => cell.push(c),
            }
        } else if c == '"' && cell.is_empty() {
            quoted = true;
        } else if c == delim {
            push(&mut cell, &mut row);
        } else if c == '\n' {
            push(&mut cell, &mut row);
            rows.push(std::mem::take(&mut row));
            if rows.len() >= MAX_TABLE_ROWS {
                return (rows, chars.peek().is_none());
            }
        } else if c != '\r' {
            cell.push(if c == '\t' { ' ' } else { c });
        }
    }
    if !cell.is_empty() || !row.is_empty() {
        push(&mut cell, &mut row);
        rows.push(row);
    }
    (rows, true)
}

/// A highlighting theme in the current palette's colors, rebuilt when the palette changes.
fn syntax_theme() -> Arc<Theme> {
    static CACHE: Mutex<Option<(u64, Arc<Theme>)>> = Mutex::new(None);
    let generation = theme::generation();
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, t)) = cache.as_ref().filter(|(g, _)| *g == generation) {
        return t.clone();
    }
    let p = theme::pal();
    let c = |c: Color32| syntect::highlighting::Color { r: c.r(), g: c.g(), b: c.b(), a: 255 };
    let [comment, keyword, string, number, function, ty] = p.syntax;
    let rules: [(&str, Color32); 10] = [
        ("comment, punctuation.definition.comment", comment),
        ("keyword, storage, keyword.operator.word", keyword),
        ("string, markup.raw", string),
        ("constant.numeric, constant.language, constant.character", number),
        ("entity.name.function, support.function, meta.function-call", function),
        ("entity.name.type, entity.name.class, support.type, support.class, storage.type", ty),
        ("entity.name.tag, markup.heading", keyword),
        ("entity.other.attribute-name, variable.parameter", number),
        ("constant.other, variable.other.constant, support.constant", number),
        ("markup.bold, markup.italic", p.text),
    ];
    let mut t = ThemeSet::load_defaults().themes.remove("base16-ocean.dark").expect("bundled theme");
    t.settings.foreground = Some(c(p.text));
    t.settings.background = Some(c(p.panel));
    t.scopes = rules
        .iter()
        .filter_map(|(sel, color)| {
            Some(syntect::highlighting::ThemeItem {
                scope: sel.parse().ok()?,
                style: syntect::highlighting::StyleModifier { foreground: Some(c(*color)), background: None, font_style: None },
            })
        })
        .collect();
    let t = Arc::new(t);
    *cache = Some((generation, t.clone()));
    t
}

#[cfg(test)]
mod hang_tests {
    use super::*;

    #[test]
    fn fifo_preview_fails_fast_instead_of_blocking() {
        let path = std::env::temp_dir().join(format!("kestrel-fifo-{}", std::process::id()));
        let c = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let started = Instant::now();
        assert!(matches!(load_text(&path), Payload::Error(_)));
        assert!(started.elapsed() < Duration::from_secs(2));
        std::fs::remove_file(path).unwrap();
    }
}
