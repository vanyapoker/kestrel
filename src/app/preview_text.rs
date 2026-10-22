//! Virtualized geometry for large, immutable text previews. Parsing never depends
//! on panel width. Selection uses document offsets, independent of cached galleys.
use crate::preview::{ChunkLayout, CodeLayout, TextChunk, TextLink};
use crate::theme::pal;
use eframe::egui::{self, Align2, FontId, Key, Modifiers, Rect, ScrollArea, Sense, Ui, pos2, vec2};
use egui::text::{ByteIndex, CharIndex, LayoutJob};
use std::time::{Duration, Instant};

const MAX_CHUNKS_PER_FRAME: usize = 8;
const REFLOW_BUDGET: Duration = Duration::from_millis(4);

fn chunks(job: &LayoutJob, width: f32, glyph: f32, row_h: f32) -> ChunkLayout {
    let mut chunks = Vec::new();
    let (mut start, mut chars, mut line) = (0, 0, 1);
    while start < job.text.len() {
        let mut end = start;
        for (lines, source) in job.text[start..].split_inclusive('\n').enumerate() {
            if end > start && (lines == 32 || end - start + source.len() > 8192) { break; }
            if source.len() > 8192 {
                end = start + 8192;
                while !job.text.is_char_boundary(end) { end -= 1; }
                break;
            }
            end += source.len();
        }
        let text = &job.text[start..end];
        let height = text.split_inclusive('\n').map(|source|
            ((source.trim_end_matches(['\r', '\n']).chars().count() as f32 * glyph / width).ceil()).max(1.0) * row_h
        ).sum();
        chunks.push(TextChunk {
            bytes: start..end, chars, line, height, width: 0.0, galley: None,
            starts_line: start == 0 || job.text.as_bytes()[start - 1] == b'\n',
        });
        chars += text.chars().count();
        line += text.bytes().filter(|&b| b == b'\n').count();
        start = end;
    }
    ChunkLayout { chunks, total_chars: chars, ..Default::default() }
}

fn layout(ui: &Ui, job: &LayoutJob, chunk: &mut TextChunk, width: f32) {
    // O(log sections + sections in chunk), with no whole-document clone.
    let end = if job.text[..chunk.bytes.end].ends_with('\n') { chunk.bytes.end - 1 } else { chunk.bytes.end };
    let mut part = LayoutJob { text: job.text[chunk.bytes.start..end].to_owned(), ..Default::default() };
    part.wrap.max_width = width;
    let first = job.sections.partition_point(|s| s.byte_range.end.0 <= chunk.bytes.start);
    for section in job.sections[first..].iter().take_while(|s| s.byte_range.start.0 < end) {
        let mut section = section.clone();
        section.byte_range = ByteIndex(section.byte_range.start.0.max(chunk.bytes.start) - chunk.bytes.start)
            ..ByteIndex(section.byte_range.end.0.min(end) - chunk.bytes.start);
        part.sections.push(section);
    }
    // An empty source line still needs its font's normal row height.
    if part.sections.is_empty() {
        part.append("", 0.0, egui::TextFormat { font_id: FontId::monospace(12.5), color: pal().text, ..Default::default() });
    }
    let galley = ui.fonts_mut(|f| f.layout_job(part));
    chunk.height = galley.size().y;
    chunk.galley = Some(galley);
    chunk.width = width;
}

fn positions(chunks: &[TextChunk]) -> Vec<f32> {
    let mut ys = Vec::with_capacity(chunks.len() + 1);
    ys.push(0.0);
    for chunk in chunks { ys.push(ys.last().unwrap() + chunk.height); }
    ys
}

pub(super) fn show(ui: &mut Ui, job: &LayoutJob, links: &[TextLink], cache: &mut CodeLayout) {
    let font = FontId::monospace(12.5);
    let (glyph, row_h) = ui.fonts_mut(|f| (f.glyph_width(&font, '0'), f.row_height(&font)));
    let (now, dragging) = ui.input(|i| (i.time, i.pointer.primary_down()));
    let pixels = ui.ctx().pixels_per_point();
    let mut anchor_delta = 0.0;
    let out = ScrollArea::vertical().auto_shrink([false; 2]).id_salt("preview_text")
        .show_viewport(ui, |ui, viewport| {
        let width = (ui.available_width() - glyph * 6.0).max(1.0);
        if cache.pending_width != width { cache.pending_width = width; cache.changed_at = now; }
        let settled = !dragging && now - cache.changed_at >= 0.080;
        if let Some(state) = cache.chunks.as_mut() {
            if cache.pixels_per_point != pixels || (cache.width != width && settled) {
                cache.width = width;
                cache.pixels_per_point = pixels;
                // Keep old heights/geometry until each replacement is ready.
                for chunk in &mut state.chunks { chunk.width = 0.0; }
                #[cfg(test)] { cache.builds += 1; }
            }
        } else {
            cache.chunks = Some(Box::new(chunks(job, width, glyph, row_h)));
            cache.width = width;
            cache.pixels_per_point = pixels;
            #[cfg(test)] { cache.builds += 1; }
        }
        if cache.width != width && !dragging {
            ui.ctx().request_repaint_after(Duration::from_secs_f64((0.080 - (now - cache.changed_at)).max(0.001)));
        }
        let state = cache.chunks.as_mut().unwrap();
        let old_ys = positions(&state.chunks);
        let first = old_ys[1..].partition_point(|&y| y <= viewport.min.y).min(state.chunks.len().saturating_sub(1));
        let last = old_ys[..state.chunks.len()].partition_point(|&y| y <= viewport.max.y);
        let overscan_first = old_ys[1..].partition_point(|&y| y < viewport.min.y - viewport.height());
        let overscan_last = old_ys[..state.chunks.len()].partition_point(|&y| y < viewport.max.y + viewport.height());
        let anchor = state.chunks.get(first).and_then(|c| c.galley.as_ref())
            .map(|g| {
                let cursor = g.cursor_from_pos(vec2(0.0, viewport.min.y - old_ys[first]));
                (cursor, g.pos_from_cursor(cursor).top())
            });
        let started = Instant::now();
        // Visible chunks first, then overscan, then incremental background reflow.
        let order = (first..last).chain(overscan_first..overscan_last).chain(0..state.chunks.len());
        let mut built = 0;
        for i in order {
            if dragging && (i < overscan_first || i >= overscan_last) { continue; }
            let chunk = &mut state.chunks[i];
            if chunk.width == cache.width && chunk.galley.is_some() { continue; }
            if built >= MAX_CHUNKS_PER_FRAME || (built > 0 && started.elapsed() >= REFLOW_BUDGET) { break; }
            layout(ui, job, chunk, cache.width);
            built += 1;
            #[cfg(test)] { cache.chunk_builds += 1; }
        }
        if state.chunks.iter().enumerate().any(|(i, c)| (!dragging || (i >= overscan_first && i < overscan_last)) && (c.width != cache.width || c.galley.is_none())) { ui.ctx().request_repaint(); }
        let ys = positions(&state.chunks);
        anchor_delta = ys[first] - old_ys[first];
        if let Some((anchor, old_y)) = anchor && let Some(galley) = &state.chunks[first].galley {
            anchor_delta += galley.pos_from_cursor(anchor).top() - old_y;
        }
        // Only compensate when geometry actually changed; cursor snapping must
        // not continuously pull a freely scrolling viewport to a row boundary.
        if built == 0 { anchor_delta = 0.0; }
        let total = *ys.last().unwrap_or(&row_h);
        let (rect, response) = ui.allocate_exact_size(vec2(width + glyph * 6.0, total), Sense::click_and_drag() - Sense::FOCUSABLE);
        state.rect = Some(rect.intersect(ui.clip_rect()));
        let origin = rect.min + vec2(glyph * 6.0, 0.0);
        let pointer = ui.input(|i| i.pointer.interact_pos());
        if ui.input(|i| i.pointer.any_pressed()) && !response.hovered() {
            state.active = false;
            state.selection = None;
        }
        if response.hovered() && ui.input(|i| i.pointer.primary_pressed()) {
            state.active = true;
            state.selecting = true;
            ui.ctx().plugin::<egui::text_selection::LabelSelectionState>().lock().clear_selection();
            if let Some(pos) = pointer {
                let at = character_at(state, &ys, pos - origin);
                let anchor = if ui.input(|i| i.modifiers.shift) { state.selection.map_or(at, |s| s.0) } else { at };
                state.selection = Some((anchor, at));
            }
        }
        if state.selecting && ui.input(|i| i.pointer.primary_down()) && let Some(pos) = pointer {
            let at = character_at(state, &ys, pos - origin);
            if let Some(selection) = &mut state.selection { selection.1 = at; }
            let clip = ui.clip_rect();
            let delta = if pos.y < clip.top() { clip.top() - pos.y } else if pos.y > clip.bottom() { clip.bottom() - pos.y } else { 0.0 };
            if delta != 0.0 { ui.scroll_with_delta(vec2(0.0, delta.clamp(-30.0, 30.0))); ui.ctx().request_repaint(); }
        }
        if ui.input(|i| i.pointer.primary_released()) { state.selecting = false; }
        if response.double_clicked() && let Some(pos) = pointer {
            let at = character_at(state, &ys, pos - origin);
            let chars: Vec<_> = job.text.chars().collect();
            let (mut start, mut end) = (at.min(chars.len()), at.min(chars.len()));
            let word = |c: char| c.is_alphanumeric() || c == '_';
            while start > 0 && word(chars[start - 1]) { start -= 1; }
            while end < chars.len() && word(chars[end]) { end += 1; }
            state.selection = Some((start, end));
        }
        let selection = state.selection.map(|(a, b)| a.min(b)..a.max(b));
        let top = ui.clip_rect().top() - rect.top();
        let bottom = ui.clip_rect().bottom() - rect.top();
        let first_paint = ys[1..].partition_point(|&y| y < top);
        for (i, chunk) in state.chunks.iter().enumerate().skip(first_paint).take_while(|(i, _)| ys[*i] <= bottom) {
            let Some(galley) = &chunk.galley else { continue; };
            if galley.pixels_per_point != pixels { continue; }
            let pos = origin + vec2(0.0, ys[i]);
            let (mut line, mut chars, mut starts_line) = (chunk.line, chunk.chars, chunk.starts_line);
            for row in &galley.rows {
                let row_rect = row.rect().translate(pos.to_vec2());
                let end = chars + row.char_count_excluding_newline().0;
                if ui.is_rect_visible(row_rect) {
                    if starts_line {
                        ui.painter().text(pos2(rect.left() + glyph * 4.0, row_rect.center().y), Align2::RIGHT_CENTER,
                            line.to_string(), font.clone(), pal().faint);
                    }
                    if let Some(s) = &selection && s.start < end && s.end > chars {
                        let x1 = row.x_offset(CharIndex(s.start.saturating_sub(chars)));
                        let x2 = row.x_offset(CharIndex(s.end.min(end) - chars));
                        ui.painter().rect_filled(Rect::from_min_max(pos2(pos.x + x1, row_rect.top()), pos2(pos.x + x2, row_rect.bottom())), 0.0,
                            ui.visuals().selection.bg_fill);
                    }
                }
                chars = end + usize::from(row.ends_with_newline);
                starts_line = row.ends_with_newline;
                if starts_line { line += 1; }
            }
            ui.painter().galley(pos, galley.clone(), pal().text);
            if !links.is_empty() {
                let end = chunk.chars + galley.text().chars().count();
                let first = links.partition_point(|l| l.chars.end <= chunk.chars);
                let local: Vec<_> = links[first..].iter().take_while(|l| l.chars.start < end).map(|l| TextLink {
                    url: l.url.clone(), chars: l.chars.start.max(chunk.chars) - chunk.chars..l.chars.end.min(end) - chunk.chars,
                }).collect();
                super::ui::paint_preview_links(ui, pos, galley, &response, &local, None);
            }
        }
    });
    #[cfg(test)]
    { cache.chunks.as_mut().unwrap().scroll_id = Some(out.id); }
    if anchor_delta.abs() > 0.1 {
        let mut scroll = out.state;
        scroll.offset.y = (scroll.offset.y + anchor_delta).max(0.0);
        scroll.store(ui.ctx(), out.id);
        ui.ctx().request_repaint();
    }
    // Also supports standalone preview widgets used by tests.
    if let Some(state) = cache.chunks.as_mut() { selection_keys(ui.ctx(), job, state); }
}

fn character_at(state: &ChunkLayout, ys: &[f32], pos: egui::Vec2) -> usize {
    if pos.y >= *ys.last().unwrap_or(&0.0) { return state.total_chars; }
    let i = ys[1..].partition_point(|&y| y <= pos.y).min(state.chunks.len().saturating_sub(1));
    let Some(chunk) = state.chunks.get(i) else { return 0; };
    chunk.chars + chunk.galley.as_ref().map_or(0, |g| g.cursor_from_pos(vec2(pos.x, pos.y - ys[i])).index.0)
}

pub(super) fn selection_keys(ctx: &egui::Context, job: &LayoutJob, state: &mut ChunkLayout) -> bool {
    if !state.active || ctx.text_edit_focused() { return false; }
    if ctx.input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| !state.rect.is_some_and(|r| r.contains(p)))) {
        state.active = false;
        state.selection = None;
        return false;
    }
    let all = ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::A));
    if all { state.selection = Some((0, state.total_chars)); }
    let copy = ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy)));
    if copy && let Some((a, b)) = state.selection && a != b {
        ctx.copy_text(job.text.chars().skip(a.min(b)).take(a.abs_diff(b)).collect());
        ctx.input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::Copy)));
        return true;
    }
    all
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Color32, Event, PointerButton, RawInput};

    fn frame(ctx: &egui::Context, job: &LayoutJob, cache: &mut CodeLayout, time: f64, events: Vec<Event>) -> egui::FullOutput {
        let mut out = ctx.run_ui(RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(500.0, 700.0))),
            time: Some(time), events, ..Default::default()
        }, |ui| { egui::CentralPanel::default().show(ui, |ui| show(ui, job, &[], cache)); });
        out.textures_delta.clear();
        out
    }

    #[test]
    fn chunks_bound_minified_unicode_lines_without_changing_source_offsets() {
        let text = format!("{}\nlast line", "界const value = 42; ".repeat(10_000));
        let job = LayoutJob::simple(text.clone(), FontId::monospace(12.5), Color32::WHITE, f32::INFINITY);
        let state = chunks(&job, 400.0, 7.5, 15.0);
        assert!(state.chunks.len() > 10);
        let mut rebuilt = String::new();
        for chunk in &state.chunks {
            assert!(chunk.bytes.len() <= 8192);
            assert_eq!(chunk.chars, rebuilt.chars().count());
            assert_eq!(chunk.line, rebuilt.bytes().filter(|&b| b == b'\n').count() + 1);
            assert_eq!(chunk.starts_line, rebuilt.is_empty() || rebuilt.ends_with('\n'));
            rebuilt.push_str(&text[chunk.bytes.clone()]);
        }
        assert_eq!(rebuilt, text);
    }

    #[test]
    fn viewport_builds_visible_chunks_first_and_copies_across_unlaid_text() {
        let ctx = egui::Context::default();
        let text = "Привет world — unicode source line\n".repeat(3000);
        let job = LayoutJob::simple(text.clone(), FontId::monospace(12.5), Color32::WHITE, f32::INFINITY);
        let mut cache = CodeLayout::default();
        frame(&ctx, &job, &mut cache, 0.0, vec![]);
        let state = cache.chunks.as_ref().unwrap();
        assert!(state.chunks[0].galley.is_some());
        assert!(state.chunks.iter().any(|c| c.galley.is_none()), "initial frame laid out the entire document");
        let at = state.rect.unwrap().min + vec2(60.0, 5.0);
        let button = |pressed| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        frame(&ctx, &job, &mut cache, 0.02, vec![Event::PointerMoved(at), button(true)]);
        frame(&ctx, &job, &mut cache, 0.04, vec![button(false)]);
        let all = Event::Key { key: Key::A, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::COMMAND };
        frame(&ctx, &job, &mut cache, 0.06, vec![all]);
        let out = frame(&ctx, &job, &mut cache, 0.08, vec![Event::Copy]);
        assert!(out.platform_output.commands.iter().any(|c| matches!(c, egui::OutputCommand::CopyText(t) if t == &text)));
        // A nontrivial range spans chunk boundaries and off-screen text. Copy is
        // from the source, including real newlines, rather than painted labels.
        cache.chunks.as_mut().unwrap().selection = Some((13, 10_005));
        let expected: String = text.chars().skip(13).take(9992).collect();
        let out = frame(&ctx, &job, &mut cache, 0.1, vec![Event::Copy]);
        assert!(out.platform_output.commands.iter().any(|c| matches!(c, egui::OutputCommand::CopyText(t) if t == &expected)));
    }

    #[test]
    fn incremental_reflow_preserves_the_visible_logical_line() {
        let ctx = egui::Context::default();
        let text = (0..3000).map(|i| format!("line {i:04} {}\n", "hello world ".repeat(8))).collect::<String>();
        let job = LayoutJob::simple(text.clone(), FontId::monospace(12.5), Color32::WHITE, f32::INFINITY);
        let mut cache = CodeLayout::default();
        for i in 0..40 { frame(&ctx, &job, &mut cache, i as f64 / 60.0, vec![]); }
        let id = cache.chunks.as_ref().unwrap().scroll_id.unwrap();
        let mut scroll = egui::scroll_area::State::load(&ctx, id).unwrap();
        scroll.offset.y = 12_000.0;
        scroll.store(&ctx, id);
        frame(&ctx, &job, &mut cache, 1.0, vec![]);
        let visible_line = |cache: &CodeLayout| {
            let state = cache.chunks.as_ref().unwrap();
            let scroll = egui::scroll_area::State::load(&ctx, state.scroll_id.unwrap()).unwrap();
            let at = character_at(state, &positions(&state.chunks), vec2(0.0, scroll.offset.y));
            text.chars().take(at).filter(|&c| c == '\n').count()
        };
        let before = visible_line(&cache);
        let selected = (37, 100_000);
        cache.chunks.as_mut().unwrap().selection = Some(selected);
        for i in 0..45 {
            let mut out = ctx.run_ui(RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(300.0, 700.0))),
                time: Some(1.1 + i as f64 / 60.0), ..Default::default()
            }, |ui| { egui::CentralPanel::default().show(ui, |ui| show(ui, &job, &[], &mut cache)); });
            out.textures_delta.clear();
        }
        let after = visible_line(&cache);
        assert!(before.abs_diff(after) <= 1, "resize moved the visible logical line: {before} -> {after}");
        assert_eq!(cache.chunks.as_ref().unwrap().selection, Some(selected));
    }

    #[test]
    fn pointer_drag_crosses_chunk_boundary_with_document_offsets() {
        let ctx = egui::Context::default();
        let text = "hello Привет\n".repeat(3000);
        let job = LayoutJob::simple(text.clone(), FontId::monospace(12.5), Color32::WHITE, f32::INFINITY);
        let mut cache = CodeLayout::default();
        frame(&ctx, &job, &mut cache, 0.0, vec![]);
        let state = cache.chunks.as_ref().unwrap();
        let rect = state.rect.unwrap();
        let start = rect.min + vec2(45.0, 2.0);
        let end = rect.min + vec2(160.0, state.chunks[0].height + 10.0);
        let button = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        frame(&ctx, &job, &mut cache, 0.02, vec![Event::PointerMoved(start), button(start, true)]);
        frame(&ctx, &job, &mut cache, 0.04, vec![Event::PointerMoved(end)]);
        frame(&ctx, &job, &mut cache, 0.06, vec![button(end, false)]);
        let (a, b) = cache.chunks.as_ref().unwrap().selection.unwrap();
        assert_eq!(a, 0);
        assert!(b > cache.chunks.as_ref().unwrap().chunks[1].chars);
        let out = frame(&ctx, &job, &mut cache, 0.08, vec![Event::Copy]);
        let expected: String = text.chars().take(b).collect();
        assert!(out.platform_output.commands.iter().any(|c| matches!(c, egui::OutputCommand::CopyText(t) if t == &expected)));
    }
}
