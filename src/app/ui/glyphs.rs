use crate::theme::*;
use eframe::egui::{self, Color32, CornerRadius, Painter, Pos2, Rect, Stroke, Ui, vec2};

/// A triangle over a bar.
pub(super) fn eject_icon(p: &Painter, c: Pos2, color: Color32) {
    let tri = vec![c + vec2(-4.5, 1.0), c + vec2(0.0, -4.5), c + vec2(4.5, 1.0)];
    p.add(egui::Shape::convex_polygon(tri, color, Stroke::NONE));
    p.rect_filled(
        Rect::from_min_max(c + vec2(-4.5, 2.8), c + vec2(4.5, 4.6)),
        CornerRadius::same(1),
        color,
    );
}

pub(super) fn chevron(p: &Painter, c: Pos2, open: bool, color: Color32) {
    let pts = if open {
        vec![
            c + vec2(-3.5, -1.5),
            c + vec2(0.0, 2.0),
            c + vec2(3.5, -1.5),
        ]
    } else {
        vec![
            c + vec2(-1.5, -3.5),
            c + vec2(2.0, 0.0),
            c + vec2(-1.5, 3.5),
        ]
    };
    p.line(pts, Stroke::new(1.4, color));
}

#[derive(Clone, Copy)]
pub(super) enum Glyph {
    Details,
    List,
    Icons,
    Columns,
    Refresh,
    Plus,
    Eye,
    Panel,
    Sidebar,
    Split,
    Search,
}

pub(super) fn glyph_button(ui: &mut Ui, glyph: Glyph, enabled: bool, on: bool, tip: &str) -> bool {
    let resp = ui
        .add_enabled(
            enabled,
            egui::Button::new("")
                .frame_when_inactive(false)
                .selected(on)
                .min_size(vec2(28.0, 26.0)),
        )
        .on_hover_text(tip);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, tip));
    #[cfg(test)]
    ui.ctx()
        .data_mut(|d| d.insert_temp(egui::Id::new(("toolbar_button", tip)), resp.rect));
    let color = if !enabled {
        pal().faint
    } else if on {
        pal().accent
    } else if resp.hovered() {
        pal().text
    } else {
        pal().dim
    };
    paint_glyph(ui.painter(), resp.rect.center(), glyph, color);
    resp.clicked()
}

pub(super) fn paint_glyph(p: &Painter, c: Pos2, glyph: Glyph, color: Color32) {
    let stroke = Stroke::new(1.6, color);
    let pt = |x: f32, y: f32| c + vec2(x, y);
    match glyph {
        Glyph::Details => {
            for y in [-5.0, 0.0, 5.0] {
                p.line_segment([pt(-7.0, y), pt(2.0, y)], stroke);
                p.line_segment([pt(5.0, y), pt(7.0, y)], stroke);
            }
        }
        Glyph::List => {
            for y in [-5.0, 0.0, 5.0] {
                p.line_segment([pt(-7.0, y), pt(7.0, y)], stroke);
            }
        }
        Glyph::Icons => {
            for x in [-4.5, 4.5] {
                for y in [-4.5, 4.5] {
                    p.rect_stroke(
                        Rect::from_center_size(pt(x, y), vec2(5.0, 5.0)),
                        CornerRadius::same(1),
                        stroke,
                        egui::StrokeKind::Middle,
                    );
                }
            }
        }
        Glyph::Columns => {
            for x in [-6.0, 0.0, 6.0] {
                p.rect_stroke(
                    Rect::from_center_size(pt(x, 0.0), vec2(3.0, 13.0)),
                    CornerRadius::same(1),
                    stroke,
                    egui::StrokeKind::Middle,
                );
            }
        }
        Glyph::Plus => {
            p.line_segment([pt(-5.0, 0.0), pt(5.0, 0.0)], stroke);
            p.line_segment([pt(0.0, -5.0), pt(0.0, 5.0)], stroke);
        }
        Glyph::Refresh => {
            let r = 5.5;
            let arc: Vec<Pos2> = (0..=24)
                .map(|i| {
                    let a = -0.9 + i as f32 / 24.0 * 5.0;
                    pt(r * a.cos(), r * a.sin())
                })
                .collect();
            let tip = arc[0];
            p.line(arc, stroke);
            p.line(
                vec![tip + vec2(-3.5, -0.5), tip, tip + vec2(0.5, -3.5)],
                stroke,
            );
        }
        Glyph::Eye => {
            let top: Vec<Pos2> = (0..=16)
                .map(|i| {
                    let t = i as f32 / 16.0 * std::f32::consts::PI;
                    pt(-7.0 * t.cos(), -4.5 * t.sin())
                })
                .collect();
            let bottom: Vec<Pos2> = top
                .iter()
                .map(|q| Pos2::new(q.x, 2.0 * c.y - q.y))
                .collect();
            p.line(top, stroke);
            p.line(bottom, stroke);
            p.circle_filled(c, 2.0, color);
        }
        Glyph::Panel => {
            let r = Rect::from_center_size(c, vec2(15.0, 12.0));
            p.rect_stroke(r, CornerRadius::same(2), stroke, egui::StrokeKind::Middle);
            let side = Rect::from_min_max(Pos2::new(r.right() - 5.5, r.top()), r.max);
            p.rect_filled(
                side,
                CornerRadius {
                    nw: 0,
                    sw: 0,
                    ne: 2,
                    se: 2,
                },
                color,
            );
        }
        Glyph::Sidebar => {
            let r = Rect::from_center_size(c, vec2(15.0, 12.0));
            p.rect_stroke(r, CornerRadius::same(2), stroke, egui::StrokeKind::Middle);
            let side = Rect::from_min_max(r.min, Pos2::new(r.left() + 5.5, r.bottom()));
            p.rect_filled(
                side,
                CornerRadius {
                    nw: 2,
                    sw: 2,
                    ne: 0,
                    se: 0,
                },
                color,
            );
        }
        Glyph::Split => {
            let r = Rect::from_center_size(c, vec2(15.0, 12.0));
            p.rect_stroke(r, CornerRadius::same(2), stroke, egui::StrokeKind::Middle);
            p.line_segment(
                [Pos2::new(c.x, r.top()), Pos2::new(c.x, r.bottom())],
                stroke,
            );
        }

        Glyph::Search => {
            p.circle_stroke(pt(-1.5, -1.5), 4.5, stroke);
            p.line_segment([pt(1.8, 1.8), pt(5.5, 5.5)], Stroke::new(2.0, color));
        }
    }
}

pub(super) fn sort_arrow(p: &Painter, c: Pos2, ascending: bool) {
    let (a, b) = if ascending { (-2.0, 2.0) } else { (2.0, -2.0) };
    p.line(
        vec![c + vec2(-3.5, -a), c + vec2(0.0, -b), c + vec2(3.5, -a)],
        Stroke::new(1.4, pal().accent),
    );
}

pub(super) fn draw_checker(p: &Painter, r: Rect) {
    let cell = 8.0;
    p.rect_filled(r, CornerRadius::ZERO, pal().checker.0);
    let (nx, ny) = (
        (r.width() / cell).ceil() as usize,
        (r.height() / cell).ceil() as usize,
    );
    if nx * ny > 40_000 {
        return;
    }
    let painter = p.with_clip_rect(r);
    for y in 0..ny {
        for x in (y % 2..nx).step_by(2) {
            let min = r.min + vec2(x as f32, y as f32) * cell;
            painter.rect_filled(
                Rect::from_min_size(min, vec2(cell, cell)),
                CornerRadius::ZERO,
                pal().checker.1,
            );
        }
    }
}
