use crate::app::icons::draw_icon_scaled;
use crate::archive::Tree;
use crate::preview::{Detail, ImageView, Table, TextLink};
use crate::theme::*;
use eframe::egui::{
    self, Align2, Color32, FontId, Pos2, Rect, RichText, ScrollArea, Sense, Stroke, Ui, UiBuilder,
    vec2,
};
use std::sync::Arc;

#[cfg(test)]
pub(in crate::app) fn preview_code(ui: &mut Ui, job: &egui::text::LayoutJob, links: &[TextLink]) {
    preview_code_cached(ui, job, links, &mut crate::preview::CodeLayout::default());
}

pub(in crate::app) fn preview_code_cached(
    ui: &mut Ui,
    job: &egui::text::LayoutJob,
    links: &[TextLink],
    cache: &mut crate::preview::CodeLayout,
) {
    if job.text.len() > 32 * 1024 {
        crate::app::preview_text::show(ui, job, links, cache);
        return;
    }
    let font = FontId::monospace(12.5);
    let glyph = ui.fonts_mut(|f| f.glyph_width(&font, '0'));
    let width = (ui.available_width() - glyph * 6.0).max(1.0);
    let pixels = ui.ctx().pixels_per_point();
    if cache.galley.is_none() || cache.pixels_per_point != pixels || cache.width != width {
        let mut wrapped = job.clone();
        wrapped.wrap.max_width = width;
        let galley = ui.fonts_mut(|f| f.layout_job(wrapped));
        cache.rows.clear();
        let (mut line, mut chars, mut starts_line) = (1, 0, true);
        for row in &galley.rows {
            cache.rows.push(crate::preview::CodeRow {
                line,
                chars,
                starts_line,
            });
            chars += row.char_count_excluding_newline().0 + usize::from(row.ends_with_newline);
            starts_line = row.ends_with_newline;
            if starts_line {
                line += 1;
            }
        }
        cache.galley = Some(galley);
        cache.width = width;
        cache.pixels_per_point = pixels;
        #[cfg(test)]
        {
            cache.builds += 1;
        }
    }
    let galley = cache.galley.as_ref().unwrap().clone();
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.horizontal_top(|ui| {
            let (rect, _) =
                ui.allocate_exact_size(vec2(glyph * 6.0, galley.size().y), Sense::hover());
            let top = ui.clip_rect().top() - rect.top();
            let bottom = ui.clip_rect().bottom() - rect.top();
            let first = galley.rows.partition_point(|r| r.pos.y + r.size.y < top);
            for (index, row) in galley
                .rows
                .iter()
                .enumerate()
                .skip(first)
                .take_while(|(_, r)| r.pos.y <= bottom)
            {
                if cache.rows[index].starts_line {
                    ui.painter().text(
                        egui::pos2(
                            rect.right() - glyph * 2.0,
                            rect.top() + row.pos.y + row.size.y / 2.0,
                        ),
                        Align2::RIGHT_CENTER,
                        cache.rows[index].line.to_string(),
                        font.clone(),
                        pal().faint,
                    );
                }
            }
            // Allocate only the current width, even while an older, wider galley
            // is being displayed. Otherwise its minimum size fights the divider.
            let sense = if ui.input(|i| i.has_touch_screen()) {
                Sense::click()
            } else {
                Sense::click_and_drag()
            } - Sense::FOCUSABLE;
            let (text_rect, response) = ui.allocate_exact_size(vec2(width, galley.size().y), sense);
            paint_preview_text(
                ui,
                text_rect.min,
                &galley,
                &response,
                links,
                Some(&cache.rows),
            );
        });
    });
}

#[cfg(test)]
pub(in crate::app) fn preview_text(ui: &mut Ui, job: &egui::text::LayoutJob, links: &[TextLink]) {
    let (pos, galley, response) = egui::Label::new(job.clone())
        .selectable(true)
        .wrap_mode(egui::TextWrapMode::Wrap)
        .layout_in_ui(ui);
    paint_preview_text(ui, pos, &galley, &response, links, None);
}

pub(in crate::app) fn paint_preview_text(
    ui: &Ui,
    pos: Pos2,
    galley: &Arc<egui::Galley>,
    response: &egui::Response,
    links: &[TextLink],
    rows: Option<&[crate::preview::CodeRow]>,
) {
    egui::text_selection::LabelSelectionState::label_text_selection(
        ui,
        response,
        pos,
        galley.clone(),
        pal().text,
        Stroke::NONE,
    );
    paint_preview_links(ui, pos, galley, response, links, rows);
}

pub(in crate::app) fn paint_preview_links(
    ui: &Ui,
    pos: Pos2,
    galley: &Arc<egui::Galley>,
    response: &egui::Response,
    links: &[TextLink],
    rows: Option<&[crate::preview::CodeRow]>,
) {
    if links.is_empty() {
        return;
    }
    let top = ui.clip_rect().top() - pos.y;
    let bottom = ui.clip_rect().bottom() - pos.y;
    let first = galley.rows.partition_point(|r| r.pos.y + r.size.y < top);
    if first == galley.rows.len() {
        return;
    }
    let mut row_start = rows
        .and_then(|r| r.get(first))
        .map(|r| r.chars)
        .unwrap_or_else(|| {
            galley.rows[..first]
                .iter()
                .map(|r| r.char_count_excluding_newline().0 + usize::from(r.ends_with_newline))
                .sum()
        });
    for row in galley
        .rows
        .iter()
        .skip(first)
        .take_while(|r| r.pos.y <= bottom)
    {
        let row_end = row_start + row.char_count_excluding_newline().0;
        if ui.is_rect_visible(row.rect().translate(pos.to_vec2())) {
            let first = links.partition_point(|link| link.chars.end <= row_start);
            for link in links
                .iter()
                .skip(first)
                .take_while(|link| link.chars.start < row_end)
            {
                let start = link.chars.start.max(row_start) - row_start;
                let end = link.chars.end.min(row_end) - row_start;
                let rect = Rect::from_min_max(
                    pos + row.pos.to_vec2() + vec2(row.x_offset(egui::text::CharIndex(start)), 0.0),
                    pos + row.pos.to_vec2()
                        + vec2(row.x_offset(egui::text::CharIndex(end)), row.size.y),
                );
                ui.painter().line_segment(
                    [rect.left_bottom(), rect.right_bottom()],
                    Stroke::new(1.0, pal().accent),
                );
                if response.hovered() && ui.rect_contains_pointer(rect) {
                    response
                        .clone()
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .on_hover_text(&link.url);
                    if response.clicked() {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(&link.url));
                    }
                }
            }
        }
        row_start = row_end + usize::from(row.ends_with_newline);
    }
}

/// Draws a CSV/TSV table with a sticky header row and a row-number gutter.
/// Web links in cells are underlined and open on click.
pub(in crate::app) fn preview_table(ui: &mut Ui, table: &Table) {
    const MAX_COL_CHARS: usize = 40;
    const PAD: f32 = 8.0;
    let font = egui::FontId::monospace(12.5);
    let row_h = ui.fonts_mut(|f| f.row_height(&font)) + 6.0;
    let glyph = ui.fonts_mut(|f| f.glyph_width(&font, '0'));
    let body = table.rows.len().saturating_sub(1);
    let gutter = body.max(1).to_string().len() as f32 * glyph + 2.0 * PAD;
    let mut xs = vec![gutter];
    for &w in &table.widths {
        xs.push(xs.last().unwrap() + w.clamp(1, MAX_COL_CHARS) as f32 * glyph + 2.0 * PAD);
    }
    let total = vec2(*xs.last().unwrap(), row_h * (body + 1) as f32);
    let p = pal();
    // Solid bars: floating ones are invisible until hovered, hiding that wide tables scroll sideways.
    ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
    ScrollArea::both()
        .auto_shrink([false; 2])
        .id_salt("preview_table")
        .show_viewport(ui, |ui, viewport| {
            let (rect, response) = ui.allocate_exact_size(total, Sense::hover());
            let clip = ui.clip_rect();
            let painter = ui.painter_at(clip);
            // Rows scroll under the sticky header, which must not pass clicks through.
            let body_clip = clip.with_min_y(clip.top() + row_h);
            let hover = response.hover_pos();
            let hover_row = hover
                .filter(|h| clip.contains(*h) && h.y > clip.top() + row_h)
                .map(|h| ((h.y - rect.top()) / row_h) as usize);
            // Columns whose first rows are all numbers are right-aligned.
            let numeric: Vec<bool> = (0..xs.len() - 1)
                .map(|c| {
                    let mut cells = table
                        .rows
                        .iter()
                        .skip(1)
                        .take(50)
                        .filter_map(|r| r.get(c))
                        .filter(|t| !t.is_empty())
                        .peekable();
                    cells.peek().is_some()
                        && cells.all(|t| t.trim().replace(',', "").parse::<f64>().is_ok())
                })
                .collect();
            let (mut tooltip, mut on_link) = (None, false);
            let mut draw_row = |row_index: usize, y: f32, color: Color32, clip: Rect| {
                for (col, text) in table.rows[row_index].iter().enumerate().take(xs.len() - 1) {
                    let r = Rect::from_min_max(
                        egui::pos2(rect.left() + xs[col], y),
                        egui::pos2(rect.left() + xs[col + 1], y + row_h),
                    );
                    let text_clip = r.shrink2(vec2(PAD, 0.0)).intersect(clip);
                    if !text_clip.is_positive() {
                        continue;
                    }
                    let galley = painter.layout_no_wrap(text.clone(), font.clone(), color);
                    let x = if numeric[col] && galley.size().x <= r.width() - 2.0 * PAD {
                        r.right() - PAD - galley.size().x
                    } else {
                        r.left() + PAD
                    };
                    let pos = egui::pos2(x, r.center().y - galley.size().y / 2.0);
                    let mut cell_ui = ui.new_child(
                        UiBuilder::new()
                            .id_salt(("csv_cell", row_index, col))
                            .max_rect(r),
                    );
                    cell_ui.set_clip_rect(text_clip);
                    let cell_response = cell_ui.interact(
                        r.intersect(text_clip),
                        cell_ui.id(),
                        Sense::click_and_drag() - Sense::FOCUSABLE,
                    );
                    egui::text_selection::LabelSelectionState::label_text_selection(
                        &cell_ui,
                        &cell_response,
                        pos,
                        galley.clone(),
                        color,
                        Stroke::NONE,
                    );
                    if row_index > 0
                        && let Some(line) = galley.rows.first()
                    {
                        for link in &crate::preview::text_links(text) {
                            let x = |c| pos.x + line.x_offset(egui::text::CharIndex(c));
                            let link_rect = Rect::from_x_y_ranges(
                                x(link.chars.start)..=x(link.chars.end),
                                r.y_range(),
                            )
                            .intersect(text_clip);
                            if !link_rect.is_positive() {
                                continue;
                            }
                            let underline_y = pos.y + line.size.y;
                            painter.hline(
                                link_rect.x_range(),
                                underline_y,
                                Stroke::new(1.0, p.accent),
                            );
                            if cell_response.hovered() && cell_ui.rect_contains_pointer(link_rect) {
                                on_link = true;
                                cell_response
                                    .clone()
                                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                                    .on_hover_text(&link.url);
                                if cell_response.clicked() {
                                    ui.ctx().open_url(egui::OpenUrl::new_tab(&link.url));
                                }
                            }
                        }
                    }
                    if !on_link
                        && text.chars().count() > MAX_COL_CHARS
                        && hover.is_some_and(|h| text_clip.contains(h))
                    {
                        tooltip = Some(text.clone());
                    }
                }
            };
            let first = ((viewport.min.y / row_h) as usize).max(1);
            let last = ((viewport.max.y / row_h).ceil() as usize + 1).min(body + 1);
            for i in first..last {
                let y = rect.top() + i as f32 * row_h;
                let stripe = Rect::from_min_size(egui::pos2(rect.left(), y), vec2(total.x, row_h));
                if hover_row == Some(i) {
                    painter.rect_filled(stripe, 0.0, p.select_dim);
                } else if i % 2 == 0 {
                    painter.rect_filled(stripe, 0.0, p.panel);
                }
                painter.text(
                    egui::pos2(rect.left() + gutter - PAD, y + row_h / 2.0),
                    Align2::RIGHT_CENTER,
                    i.to_string(),
                    font.clone(),
                    p.faint,
                );
                draw_row(i, y, p.text, body_clip);
            }
            // Header sticks to the top of the viewport.
            let y = clip.top();
            let header = Rect::from_min_size(egui::pos2(rect.left(), y), vec2(total.x, row_h));
            painter.rect_filled(header, 0.0, p.surface);
            painter.hline(
                header.x_range(),
                header.bottom(),
                Stroke::new(1.0, p.border),
            );
            for &x in &xs[1..xs.len() - 1] {
                painter.vline(
                    rect.left() + x,
                    header.y_range(),
                    Stroke::new(1.0, p.border),
                );
            }
            draw_row(0, y, p.accent, clip);
            if let Some(text) = tooltip {
                response.on_hover_text(text);
            }
        });
}

/// Draws an image that zooms around the pointer with the wheel or a pinch, pans by
/// dragging, and switches between fitting the panel and 100% on double-click.
/// Returns the size worth reloading the image at once the texture is too coarse
/// for the current zoom.
pub(in crate::app) fn preview_image(
    ui: &mut Ui,
    tex: &egui::TextureHandle,
    dims: (u32, u32),
    view: &mut ImageView,
) -> Option<u32> {
    let area = ui.available_rect_before_wrap();
    let response = ui.allocate_rect(area, Sense::click_and_drag());
    let orig = vec2(dims.0.max(1) as f32, dims.1.max(1) as f32);
    let texture = tex.size_vec2();
    let ppp = ui.ctx().pixels_per_point();
    // Fit, but don't blow small images up past twice their size.
    let fit = (area.width() / orig.x)
        .min(area.height() / orig.y)
        .min(2.0 * texture.x / orig.x)
        .max(f32::MIN_POSITIVE);
    let max = (8.0 / ppp).max(fit);
    let mut scale = view.scale.unwrap_or(fit);

    // Moves to `new` keeping the image point under `anchor` where it is.
    let zoom_to = |view: &mut ImageView, scale: &mut f32, new: f32, anchor: Pos2| {
        let new = new.clamp(fit, max);
        let offset = anchor - area.center();
        let point = view.center + offset / (orig * *scale);
        view.center = point - offset / (orig * new);
        *scale = new;
        view.scale = (new > fit * 1.001).then_some(new);
    };
    if let Some(pointer) = response.hover_pos() {
        let (wheel, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
        let factor = pinch * (wheel / 200.0).exp();
        if factor != 1.0 {
            let new = scale * factor;
            zoom_to(view, &mut scale, new, pointer);
        }
    }
    if response.double_clicked() {
        let pointer = response.interact_pointer_pos().unwrap_or(area.center());
        if view.scale.is_some() {
            *view = ImageView {
                detail: view.detail,
                ..ImageView::default()
            };
            scale = fit;
        } else {
            zoom_to(view, &mut scale, (1.0 / ppp).max(fit * 1.01), pointer);
        }
    }
    if response.dragged() && view.scale.is_some() {
        view.center -= response.drag_delta() / (orig * scale);
    }
    if view.scale.is_none() {
        view.center = vec2(0.5, 0.5);
    }

    // A smaller-than-panel side stays centered; a larger one can't leave a gap.
    let size = orig * scale;
    for axis in 0..2 {
        if size[axis] <= area.size()[axis] {
            view.center[axis] = 0.5;
        } else {
            let half = area.size()[axis] / (2.0 * size[axis]);
            view.center[axis] = view.center[axis].clamp(half, 1.0 - half);
        }
    }
    let rect = Rect::from_center_size(area.center() + (vec2(0.5, 0.5) - view.center) * size, size);
    let painter = ui.painter_at(area);
    super::draw_checker(&painter, rect.intersect(area));
    painter.image(tex.id(), rect, super::UV, Color32::WHITE);

    let pannable = size.x > area.width() + 0.5 || size.y > area.height() + 0.5;
    if pannable && response.hovered() {
        ui.ctx().set_cursor_icon(if response.dragged() {
            egui::CursorIcon::Grabbing
        } else {
            egui::CursorIcon::Grab
        });
    }
    if response.hovered() || view.scale.is_some() {
        let percent = (scale * ppp * 100.0).round();
        let mut text = if view.scale.is_some() {
            format!("{percent}%")
        } else {
            format!("{percent}% · scroll to zoom")
        };
        if view.detail == Detail::Loading {
            text.push_str(" · sharpening…");
        }
        let p = pal();
        let galley = painter.layout_no_wrap(text, FontId::proportional(11.5), p.dim);
        let badge = Rect::from_min_size(
            area.right_bottom() - galley.size() - vec2(18.0, 14.0),
            galley.size() + vec2(12.0, 6.0),
        );
        painter.rect_filled(badge, 4.0, p.surface.gamma_multiply(0.92));
        painter.galley(badge.min + vec2(6.0, 3.0), galley, p.dim);
    }

    // Texture pixels each take more than one screen pixel: a sharper copy would help.
    let coarse = scale * ppp * orig.x / texture.x > 1.05;
    (coarse && texture.x < orig.x && view.detail == Detail::None).then(|| {
        dims.0
            .max(dims.1)
            .min(ui.ctx().input(|i| i.max_texture_side) as u32)
    })
}

/// Lists an archive's folder `cwd`, with a way back up. Clicking a folder enters it.
pub(in crate::app) fn preview_archive(ui: &mut Ui, tree: &Tree, cwd: &mut Vec<String>) {
    const ROW: f32 = 24.0;
    let p = pal();
    let Some(dir) = tree.find(cwd) else {
        cwd.clear();
        return;
    };
    if tree.truncated {
        ui.label(
            RichText::new("Showing part of the archive: it has too many items to list quickly.")
                .small()
                .color(p.faint),
        );
    }
    if !cwd.is_empty() {
        ui.horizontal(|ui| {
            if ui.small_button("‹ Up").clicked() {
                cwd.pop();
            }
            ui.add(
                egui::Label::new(RichText::new(cwd.join(" / ")).size(12.0).color(p.dim)).truncate(),
            );
        });
    }
    let children = &tree.nodes[dir].children;
    if children.is_empty() {
        ui.label(RichText::new("Empty folder").color(p.faint));
        return;
    }
    let mut enter = None;
    ScrollArea::vertical()
        .id_salt(("archive", cwd.len()))
        .auto_shrink([false; 2])
        .show_rows(ui, ROW, children.len(), |ui, range| {
            for &i in &children[range] {
                let node = &tree.nodes[i];
                let (rect, response) =
                    ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
                if response.hovered() {
                    ui.painter().rect_filled(rect, 4.0, p.hover);
                }
                let ext = node
                    .name
                    .rsplit_once('.')
                    .filter(|(stem, _)| !stem.is_empty())
                    .map(|(_, e)| e.to_lowercase())
                    .unwrap_or_default();
                let cat = crate::util::category(&ext, node.is_dir);
                draw_icon_scaled(
                    ui.painter(),
                    egui::pos2(rect.left() + 14.0, rect.center().y),
                    cat,
                    0.75,
                );
                let size = crate::util::human_size(node.size);
                let size_galley =
                    ui.painter()
                        .layout_no_wrap(size, FontId::proportional(11.5), p.faint);
                let name_width = rect.width() - 36.0 - size_galley.size().x - 12.0;
                let name = ui.painter().layout_no_wrap(
                    node.name.clone(),
                    FontId::proportional(12.5),
                    p.text,
                );
                let clipped = name.size().x > name_width;
                let name_rect = Rect::from_min_size(
                    egui::pos2(rect.left() + 30.0, rect.center().y - name.size().y / 2.0),
                    vec2(name_width.max(0.0), name.size().y),
                );
                ui.painter()
                    .with_clip_rect(name_rect.intersect(ui.clip_rect()))
                    .galley(name_rect.min, name, p.text);
                ui.painter().galley(
                    egui::pos2(
                        rect.right() - 6.0 - size_galley.size().x,
                        rect.center().y - size_galley.size().y / 2.0,
                    ),
                    size_galley,
                    p.faint,
                );
                if node.is_dir && response.clicked() {
                    enter = Some(node.name.clone());
                }
                let response = if node.is_dir {
                    response.on_hover_cursor(egui::CursorIcon::PointingHand)
                } else {
                    response
                };
                if clipped {
                    response.on_hover_text(&node.name);
                }
            }
        });
    if let Some(name) = enter {
        cwd.push(name);
    }
}
