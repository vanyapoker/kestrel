use crate::theme;
use crate::thumbs::{self, Renderer};
use eframe::egui::{self, text::LayoutJob, Color32, ColorImage, FontId, TextFormat};
use std::io::Read;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
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

/// Extra metadata rows shown under the preview (dimensions, duration, pages…).
pub type Info = Vec<(String, String)>;

pub enum Content {
    Image { tex: egui::TextureHandle, info: Info },
    Text { job: LayoutJob, links: Vec<TextLink>, lines: usize, truncated: bool },
    Table(Table),
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
    Text(LayoutJob, Vec<TextLink>, usize, bool),
    Table(Table),
    Dir(Option<usize>),
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
    let mut offset = 0;
    let mut char_offset = 0;
    while let Some(start) = text[offset..].find("http://").into_iter()
        .chain(text[offset..].find("https://")).min()
    {
        let start = offset + start;
        let rest = &text[start..];
        let end = rest.find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'' | '`'))
            .unwrap_or(rest.len());
        let mut url = rest[..end].trim_end_matches(['.', ',', ';', ':', '!', '?']);
        // Strip prose/Markdown closing delimiters, retaining balanced URL parentheses.
        while let Some(close) = url.chars().last().filter(|c| matches!(c, ')' | ']' | '}')) {
            let open = match close { ')' => '(', ']' => '[', _ => '{' };
            if url.matches(close).count() <= url.matches(open).count() {
                break;
            }
            url = &url[..url.len() - 1];
        }
        let scheme_len = if url.starts_with("https://") { 8 } else { 7 };
        if url.len() > scheme_len && !url[scheme_len..].starts_with(['/', ')', ']', '}']) {
            let chars = char_offset + text[offset..start].chars().count();
            links.push(TextLink { url: url.to_owned(), chars: chars..chars + url.chars().count() });
        }
        char_offset += text[offset..start + end].chars().count();
        offset = start + end;
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
            let Payload::Text(job, _, _, truncated) = load_text(&path) else { panic!("no text preview for {name}") };
            assert_eq!(job.text, expected);
            assert!(!truncated);
            assert_eq!(std::fs::read(&path).unwrap(), raw.as_bytes());
            assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), modified);
        }
        // A bounded read ending in valid JSON must still not format a partial file.
        let path = dir.join("large.json");
        let raw = format!("{{\"a\":1}}{}trailing", " ".repeat(MAX_TEXT_BYTES));
        std::fs::write(&path, &raw).unwrap();
        let Payload::Text(job, _, _, truncated) = load_text(&path) else { panic!("no text preview") };
        assert!(truncated);
        assert_eq!(job.text, &raw[..MAX_TEXT_BYTES]);
        let path = dir.join("long.txt");
        let raw = "слово ".repeat(500);
        std::fs::write(&path, &raw).unwrap();
        let Payload::Text(job, _, _, truncated) = load_text(&path) else { panic!("no text preview") };
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
                Payload::Text(job, links, lines, truncated) => Content::Text { job, links, lines, truncated },
                Payload::Table(table) => Content::Table(table),
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
    if matches!(ext.as_str(), "csv" | "tsv")
        && let Some(table) = load_table(path, ext == "tsv")
    {
        return table;
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
    let mut truncated = file_len as usize > buf.len();
    let raw = String::from_utf8_lossy(&buf);
    let formatted = if !truncated && path.extension().is_some_and(|e| e.eq_ignore_ascii_case("json")) {
        pretty_json(&raw)
    } else {
        None
    };
    let text = formatted.as_deref().unwrap_or(&raw).replace('\t', "    ");

    static SYNTAX: OnceLock<SyntaxSet> = OnceLock::new();
    let ss = SYNTAX.get_or_init(SyntaxSet::load_defaults_newlines);
    let theme = &*syntax_theme();
    let syntax = ss
        .find_syntax_for_file(path)
        .ok()
        .flatten()
        .or_else(|| text.lines().next().and_then(|l| ss.find_syntax_by_first_line(l)))
        .unwrap_or_else(|| ss.find_syntax_plain_text());
    let mut hl = HighlightLines::new(syntax, theme);

    let font = FontId::monospace(12.5);
    let mut job = LayoutJob::default();
    let mut lines = 0;
    for (n, line) in LinesWithEndings::from(&text).enumerate() {
        if n >= MAX_TEXT_LINES {
            truncated = true;
            break;
        }
        lines += 1;
        match hl.highlight_line(line, ss) {
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
                line,
                0.0,
                TextFormat { font_id: font.clone(), color: theme::pal().text, ..Default::default() },
            ),
        }
    }
    let links = text_links(&job.text);
    Payload::Text(job, links, lines, truncated)
}

/// Parses a CSV/TSV file, or `None` to fall back to the plain-text preview.
fn load_table(path: &Path, tsv: bool) -> Option<Payload> {
    let file_len = std::fs::metadata(path).ok()?.len();
    let mut buf = Vec::new();
    std::fs::File::open(path).ok()?.take(MAX_TEXT_BYTES as u64).read_to_end(&mut buf).ok()?;
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
