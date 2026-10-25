//! Vector icons: file-type badges, folders, and emblems for well-known folders.

use crate::fsops::{self, Special};
use crate::theme::*;
use crate::util::Category;
use eframe::egui::{
    Color32, CornerRadius, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2, epaint::PathShape, vec2,
};
use std::path::Path;

pub fn draw_icon_scaled(p: &Painter, c: Pos2, cat: Category, s: f32) {
    if cat == Category::Folder {
        draw_folder(p, c, s);
        return;
    }
    let color = category_color(cat);
    let r = Rect::from_center_size(c, vec2(11.0, 14.0) * s);
    let radius = CornerRadius::same((2.0 * s).min(255.0) as u8);
    p.rect_filled(r, radius, color.gamma_multiply(0.22));
    p.rect_stroke(r, radius, Stroke::new(s.sqrt(), color), StrokeKind::Inside);
    let band = Rect::from_min_size(
        Pos2::new(r.left(), r.bottom() - 4.0 * s),
        vec2(r.width(), 4.0 * s),
    );
    p.rect_filled(
        band,
        CornerRadius {
            nw: 0,
            ne: 0,
            sw: radius.sw,
            se: radius.se,
        },
        color,
    );
}

pub fn draw_folder(p: &Painter, c: Pos2, s: f32) {
    let body = Rect::from_center_size(c + vec2(0.0, 1.0) * s, vec2(15.0, 11.0) * s);
    let tab = Rect::from_min_size(body.left_top() - vec2(0.0, 2.0 * s), vec2(6.5, 4.0) * s);
    let radius = CornerRadius::same((1.5 * s).min(255.0) as u8);
    p.rect_filled(tab, radius, pal().folder.gamma_multiply(0.75));
    p.rect_filled(body, radius, pal().folder);
}

/// A folder icon, with an emblem when `path` is a well-known folder.
pub fn draw_path_folder(p: &Painter, c: Pos2, s: f32, path: &Path) {
    match fsops::special(path) {
        // At row size an emblem on a folder is a blur; the bare glyph reads better.
        Some(kind) if s < 1.5 => draw_emblem(p, c, kind, s, pal().folder, 1.4),
        Some(kind) => {
            draw_folder(p, c, s);
            draw_emblem(
                p,
                c + vec2(0.0, 1.5) * s,
                kind,
                s * 0.42,
                pal().emblem,
                (1.6 * s.sqrt()).min(3.5),
            );
        }
        None => draw_folder(p, c, s),
    }
}

/// An icon for a filesystem entry: folders get emblems, files a type badge.
pub fn draw_entry_icon(p: &Painter, c: Pos2, s: f32, path: &Path, cat: Category) {
    if cat == Category::Folder {
        draw_path_folder(p, c, s, path);
    } else {
        draw_icon_scaled(p, c, cat, s);
    }
}

/// A sidebar glyph: line art for well-known folders, a folder otherwise.
pub fn draw_place(p: &Painter, c: Pos2, kind: Option<Special>, color: Color32) {
    match kind {
        Some(k) => draw_emblem(p, c, k, 1.0, color, 1.4),
        None => draw_folder(p, c, 0.85),
    }
}

/// Line-art emblem on a 12×12 grid centered on `c`, scaled by `s`.
pub fn draw_emblem(p: &Painter, c: Pos2, kind: Special, s: f32, color: Color32, width: f32) {
    let pt = |x: f32, y: f32| c + vec2(x, y) * s;
    let stroke = Stroke::new(width, color);
    let line = |pts: &[(f32, f32)]| p.line(pts.iter().map(|&(x, y)| pt(x, y)).collect(), stroke);
    let rect = |x0: f32, y0: f32, x1: f32, y1: f32, r: f32| {
        let rc = CornerRadius::same((r * s).round().min(255.0) as u8);
        p.rect_stroke(
            Rect::from_min_max(pt(x0, y0), pt(x1, y1)),
            rc,
            stroke,
            StrokeKind::Middle,
        );
    };
    match kind {
        Special::Home => {
            line(&[(-6.5, 0.0), (0.0, -6.0), (6.5, 0.0)]);
            line(&[(-4.5, -1.5), (-4.5, 6.0), (4.5, 6.0), (4.5, -1.5)]);
            line(&[(-1.5, 6.0), (-1.5, 2.0), (1.5, 2.0), (1.5, 6.0)]);
        }
        Special::Desktop => {
            rect(-6.0, -5.5, 6.0, 2.5, 1.0);
            line(&[(0.0, 2.5), (0.0, 5.5)]);
            line(&[(-3.0, 5.5), (3.0, 5.5)]);
        }
        Special::Documents => {
            line(&[
                (1.5, -6.0),
                (-4.5, -6.0),
                (-4.5, 6.0),
                (4.5, 6.0),
                (4.5, -3.0),
                (1.5, -6.0),
                (1.5, -3.0),
                (4.5, -3.0),
            ]);
            for y in [0.0, 3.0] {
                line(&[(-2.0, y), (2.0, y)]);
            }
        }
        Special::Downloads => {
            line(&[(0.0, -6.0), (0.0, 2.5)]);
            line(&[(-3.5, -1.0), (0.0, 2.5), (3.5, -1.0)]);
            line(&[(-6.0, 3.0), (-6.0, 6.0), (6.0, 6.0), (6.0, 3.0)]);
        }
        Special::Pictures => {
            rect(-6.0, -5.0, 6.0, 5.0, 1.5);
            line(&[
                (-6.0, 3.5),
                (-2.0, -0.5),
                (1.0, 2.5),
                (3.0, 0.5),
                (6.0, 3.5),
            ]);
            p.circle_filled(pt(2.5, -2.2), 1.3 * s, color);
        }
        Special::Music => {
            line(&[(-2.0, 3.5), (-2.0, -5.0), (5.0, -6.5), (5.0, 2.0)]);
            p.circle_filled(pt(-4.0, 3.8), 2.3 * s, color);
            p.circle_filled(pt(3.0, 2.3), 2.3 * s, color);
        }
        Special::Videos => {
            rect(-6.0, -4.5, 6.0, 4.5, 1.5);
            p.add(PathShape::convex_polygon(
                vec![pt(-1.5, -2.5), pt(2.8, 0.0), pt(-1.5, 2.5)],
                color,
                Stroke::NONE,
            ));
        }
        Special::Projects => {
            line(&[(-2.5, -4.0), (-6.0, 0.0), (-2.5, 4.0)]);
            line(&[(2.5, -4.0), (6.0, 0.0), (2.5, 4.0)]);
            line(&[(1.2, -5.5), (-1.2, 5.5)]);
        }
        Special::Work => {
            rect(-6.0, -2.5, 6.0, 5.5, 1.5);
            line(&[(-2.5, -2.5), (-2.5, -5.0), (2.5, -5.0), (2.5, -2.5)]);
            line(&[(-6.0, 1.0), (6.0, 1.0)]);
        }
        Special::Public => {
            p.circle_stroke(pt(0.0, 0.0), 6.0 * s, stroke);
            line(&[(-6.0, 0.0), (6.0, 0.0)]);
            p.add(egui_ellipse(pt(0.0, 0.0), vec2(2.6, 6.0) * s, stroke));
        }
        Special::Templates => {
            for (x0, y0, x1, y1) in [
                (-6.0, -6.0, -1.0, -1.0),
                (1.0, -6.0, 6.0, -1.0),
                (-6.0, 1.0, -1.0, 6.0),
                (1.0, 1.0, 6.0, 6.0),
            ] {
                rect(x0, y0, x1, y1, 1.0);
            }
        }
        Special::Root | Special::Drive => {
            rect(-6.5, -3.5, 6.5, 3.5, 1.5);
            p.circle_filled(pt(3.8, 0.0), 1.0 * s, color);
            line(&[(-4.0, 0.0), (0.5, 0.0)]);
        }
        Special::Trash => {
            line(&[(-6.0, -3.5), (6.0, -3.5)]);
            line(&[(-1.5, -3.5), (-1.5, -6.0), (1.5, -6.0), (1.5, -3.5)]);
            line(&[(-4.5, -3.5), (-3.8, 6.0), (3.8, 6.0), (4.5, -3.5)]);
            line(&[(-1.3, -0.5), (-1.3, 3.0)]);
            line(&[(1.3, -0.5), (1.3, 3.0)]);
        }
        Special::Bookmark => {
            line(&[
                (-4.0, -6.0),
                (4.0, -6.0),
                (4.0, 6.0),
                (0.0, 2.5),
                (-4.0, 6.0),
                (-4.0, -6.0),
            ]);
        }
    }
}

fn egui_ellipse(c: Pos2, r: Vec2, stroke: Stroke) -> PathShape {
    let pts = (0..24)
        .map(|i| {
            let a = i as f32 / 24.0 * std::f32::consts::TAU;
            c + vec2(a.cos() * r.x, a.sin() * r.y)
        })
        .collect();
    PathShape::closed_line(pts, stroke)
}
