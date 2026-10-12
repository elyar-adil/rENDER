//! Stroke outlines for the 2D context, as polygons in user space.
//!
//! A stroke is the union of its segment rectangles, its joins and its caps.
//! Every piece is emitted with the same orientation, so a nonzero fill of the
//! set paints exactly that union. Dashes split the polyline into open pieces
//! before the outline is built.

#![allow(
    // Geometry and pixel code converts between float coordinates and pixel
    // indices. Every conversion is preceded by a clamp or a floor, and the
    // bounds are the canvas or path extent, so truncation is the intent.
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap,
    // Exact comparisons are intended: a degenerate segment is exactly zero
    // length, and an edge is exactly horizontal.
    clippy::float_cmp,
    // The specification's own names (x1, cp2y, ...) are kept.
    clippy::many_single_char_names,
    clippy::similar_names,
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::collapsible_if,
    clippy::single_match_else
)]

use std::f64::consts::TAU;

use super::path::Point;
use super::style::{LineCap, LineJoin};

/// The most dash intervals one subpath is split into.
const MAX_DASH_INTERVALS: usize = 1 << 20;

/// The parameters a stroke is built with, in user-space units.
#[derive(Clone, Debug, PartialEq)]
pub struct StrokeStyle {
    pub width: f64,
    pub cap: LineCap,
    pub join: LineJoin,
    pub miter_limit: f64,
    /// The dash pattern, even-length and non-negative. Empty means solid.
    pub dash: Vec<f64>,
    pub dash_offset: f64,
}

/// The polygons of a stroke of `subpaths`, each `(points, closed)` in user space.
#[must_use]
pub fn stroke_polygons(subpaths: &[(Vec<Point>, bool)], style: &StrokeStyle) -> Vec<Vec<Point>> {
    let half = style.width / 2.0;
    let mut out = Vec::new();
    for (points, closed) in subpaths {
        if style.dash.iter().sum::<f64>() > 0.0 {
            for piece in dash_pieces(points, *closed, &style.dash, style.dash_offset) {
                stroke_line(&piece, false, half, style, &mut out);
            }
        } else {
            stroke_line(points, *closed, half, style, &mut out);
        }
    }
    out
}

/// Splits `points` into the pieces the dash pattern leaves on. A closed
/// subpath is walked around its closing segment too.
fn dash_pieces(points: &[Point], closed: bool, dash: &[f64], offset: f64) -> Vec<Vec<Point>> {
    let mut vertices = points.to_vec();
    if closed && let Some(first) = vertices.first().copied() {
        vertices.push(first);
    }
    let total: f64 = dash.iter().sum();
    let mut index = 0_usize;
    let mut remaining_in_dash = offset.rem_euclid(total);
    while remaining_in_dash >= dash[index] {
        remaining_in_dash -= dash[index];
        index = (index + 1) % dash.len();
    }
    remaining_in_dash = dash[index] - remaining_in_dash;
    let mut on = index.is_multiple_of(2);
    let mut current: Option<Vec<Point>> = if on {
        vertices.first().map(|p| vec![*p])
    } else {
        None
    };
    let mut pieces = Vec::new();
    // A dash far shorter than the path would split it into an unbounded number of
    // pieces. Past this many intervals the rest of the path is left undashed,
    // which is visually the same stroke.
    let mut intervals = 0_usize;
    for window in vertices.windows(2) {
        let (a, b) = (window[0], window[1]);
        let length = distance(a, b);
        if length == 0.0 {
            continue;
        }
        let mut walked = 0.0;
        while length - walked > remaining_in_dash {
            intervals += 1;
            if intervals > MAX_DASH_INTERVALS {
                break;
            }
            walked += remaining_in_dash;
            let point = lerp(a, b, walked / length);
            if let Some(mut piece) = current.take() {
                piece.push(point);
                pieces.push(piece);
            } else {
                current = Some(vec![point]);
            }
            on = !on;
            index = (index + 1) % dash.len();
            remaining_in_dash = dash[index];
        }
        remaining_in_dash -= length - walked;
        if on {
            if let Some(piece) = current.as_mut() {
                piece.push(b);
            }
        }
    }
    if let Some(piece) = current {
        if piece.len() >= 2 {
            pieces.push(piece);
        }
    }
    pieces
}

/// The outline of one polyline (open or closed) under `style`.
fn stroke_line(
    points: &[Point],
    closed: bool,
    half: f64,
    style: &StrokeStyle,
    out: &mut Vec<Vec<Point>>,
) {
    let mut vertices: Vec<Point> = Vec::with_capacity(points.len());
    for &point in points {
        if vertices
            .last()
            .is_none_or(|last| distance(*last, point) > 1e-12)
        {
            vertices.push(point);
        }
    }
    if closed && vertices.len() > 1 {
        if let (Some(first), Some(last)) = (vertices.first().copied(), vertices.last().copied()) {
            if distance(first, last) <= 1e-12 {
                vertices.pop();
            }
        }
    }
    if vertices.is_empty() {
        return;
    }
    if vertices.len() == 1 {
        let point = vertices[0];
        match style.cap {
            LineCap::Butt => {}
            LineCap::Round => out.push(circle(point, half)),
            LineCap::Square => out.push(oriented(vec![
                (point.0 - half, point.1 - half),
                (point.0 + half, point.1 - half),
                (point.0 + half, point.1 + half),
                (point.0 - half, point.1 + half),
            ])),
        }
        return;
    }
    let count = vertices.len();
    let segments = if closed { count } else { count - 1 };
    for index in 0..segments {
        let a = vertices[index];
        let b = vertices[(index + 1) % count];
        let (nx, ny) = normal(a, b, half);
        out.push(oriented(vec![
            (a.0 + nx, a.1 + ny),
            (b.0 + nx, b.1 + ny),
            (b.0 - nx, b.1 - ny),
            (a.0 - nx, a.1 - ny),
        ]));
    }
    let joins: Vec<usize> = if closed {
        (0..count).collect()
    } else {
        (1..count - 1).collect()
    };
    for index in joins {
        let previous = vertices[(index + count - 1) % count];
        let vertex = vertices[index];
        let next = vertices[(index + 1) % count];
        add_join(previous, vertex, next, half, style, out);
    }
    if !closed {
        let first_direction = unit(vertices[0], vertices[1]);
        let last_direction = unit(vertices[count - 2], vertices[count - 1]);
        add_cap(
            vertices[0],
            (-first_direction.0, -first_direction.1),
            half,
            style.cap,
            out,
        );
        add_cap(vertices[count - 1], last_direction, half, style.cap, out);
    }
}

fn add_join(
    previous: Point,
    vertex: Point,
    next: Point,
    half: f64,
    style: &StrokeStyle,
    out: &mut Vec<Vec<Point>>,
) {
    let (d1x, d1y) = unit(previous, vertex);
    let (d2x, d2y) = unit(vertex, next);
    let cross = d1x * d2y - d1y * d2x;
    let dot = d1x * d2x + d1y * d2y;
    if cross.abs() < 1e-12 && dot > 0.0 {
        return;
    }
    // The outer side of the turn: the right side of a left turn, and vice versa.
    let side = if cross > 0.0 { -1.0 } else { 1.0 };
    let outer_1 = (vertex.0 + side * -d1y * half, vertex.1 + side * d1x * half);
    let outer_2 = (vertex.0 + side * -d2y * half, vertex.1 + side * d2x * half);
    match style.join {
        LineJoin::Round => out.push(circle(vertex, half)),
        LineJoin::Bevel => out.push(oriented(vec![vertex, outer_1, outer_2])),
        LineJoin::Miter => {
            let ratio_squared = 2.0 / (1.0 + dot).max(1e-12);
            if ratio_squared.sqrt() <= style.miter_limit {
                let tip = (
                    vertex.0
                        + ((outer_1.0 - vertex.0) + (outer_2.0 - vertex.0))
                            / (1.0 + dot).max(1e-12),
                    vertex.1
                        + ((outer_1.1 - vertex.1) + (outer_2.1 - vertex.1))
                            / (1.0 + dot).max(1e-12),
                );
                out.push(oriented(vec![vertex, outer_1, tip, outer_2]));
            } else {
                out.push(oriented(vec![vertex, outer_1, outer_2]));
            }
        }
    }
}

/// A cap at `end`, extending outwards along `outward`.
fn add_cap(end: Point, outward: (f64, f64), half: f64, cap: LineCap, out: &mut Vec<Vec<Point>>) {
    match cap {
        LineCap::Butt => {}
        LineCap::Round => out.push(circle(end, half)),
        LineCap::Square => {
            let (nx, ny) = (-outward.1 * half, outward.0 * half);
            let (ox, oy) = (outward.0 * half, outward.1 * half);
            out.push(oriented(vec![
                (end.0 + nx, end.1 + ny),
                (end.0 + nx + ox, end.1 + ny + oy),
                (end.0 - nx + ox, end.1 - ny + oy),
                (end.0 - nx, end.1 - ny),
            ]));
        }
    }
}

/// A circle polygon of `radius` about `centre`.
fn circle(centre: Point, radius: f64) -> Vec<Point> {
    let count = ((radius * 4.0).ceil() as usize).clamp(16, 96);
    let points = (0..count)
        .map(|index| {
            let angle = TAU * index as f64 / count as f64;
            (
                centre.0 + radius * angle.cos(),
                centre.1 + radius * angle.sin(),
            )
        })
        .collect();
    oriented(points)
}

/// The polygon with its signed area made non-negative, so every piece of a
/// stroke has the same orientation and a nonzero fill unions them.
fn oriented(mut points: Vec<Point>) -> Vec<Point> {
    let mut area = 0.0;
    for index in 0..points.len() {
        let (x1, y1) = points[index];
        let (x2, y2) = points[(index + 1) % points.len()];
        area += x1 * y2 - x2 * y1;
    }
    if area < 0.0 {
        points.reverse();
    }
    points
}

/// The left-hand normal of the segment `a` to `b`, scaled to `half`.
fn normal(a: Point, b: Point, half: f64) -> (f64, f64) {
    let (dx, dy) = unit(a, b);
    (-dy * half, dx * half)
}

fn unit(a: Point, b: Point) -> (f64, f64) {
    let length = distance(a, b);
    if length == 0.0 {
        (1.0, 0.0)
    } else {
        ((b.0 - a.0) / length, (b.1 - a.1) / length)
    }
}

fn distance(a: Point, b: Point) -> f64 {
    (b.0 - a.0).hypot(b.1 - a.1)
}

fn lerp(a: Point, b: Point, t: f64) -> Point {
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
}
