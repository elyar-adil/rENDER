//! The current path of a 2D context and the geometry that builds it.
//!
//! The specification transforms each point by the current transformation
//! matrix when the point is added, so a path is stored in device coordinates
//! as flattened polylines. Curves are flattened to a tolerance measured in
//! device pixels. The transformation matrix is `[a, b, c, d, e, f]`, mapping
//! `(x, y)` to `(a x + c y + e, b x + d y + f)`.

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

use std::f64::consts::{PI, TAU};

/// An affine transformation matrix `[a, b, c, d, e, f]`.
pub type Matrix = [f64; 6];

/// The identity transformation.
pub const IDENTITY: Matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// A point in some coordinate space.
pub type Point = (f64, f64);

/// `current · next`: the transformation that applies `next` first.
#[must_use]
pub fn multiply(current: Matrix, next: Matrix) -> Matrix {
    let [a1, b1, c1, d1, e1, f1] = current;
    let [a, b, c, d, e, f] = next;
    [
        a1 * a + c1 * b,
        b1 * a + d1 * b,
        a1 * c + c1 * d,
        b1 * c + d1 * d,
        a1 * e + c1 * f + e1,
        b1 * e + d1 * f + f1,
    ]
}

/// The point `point` mapped by `matrix`.
#[must_use]
pub fn apply(matrix: Matrix, point: Point) -> Point {
    let [a, b, c, d, e, f] = matrix;
    (a * point.0 + c * point.1 + e, b * point.0 + d * point.1 + f)
}

/// The inverse of `matrix`, or `None` when it is singular or non-finite.
#[must_use]
pub fn invert(matrix: Matrix) -> Option<Matrix> {
    let [a, b, c, d, e, f] = matrix;
    let determinant = a * d - b * c;
    if !determinant.is_finite() || determinant.abs() < 1e-12 {
        return None;
    }
    Some([
        d / determinant,
        -b / determinant,
        -c / determinant,
        a / determinant,
        (c * f - d * e) / determinant,
        (b * e - a * f) / determinant,
    ])
}

/// The largest scale factor the matrix applies to a unit length.
#[must_use]
pub fn max_scale(matrix: Matrix) -> f64 {
    let [a, b, c, d, _, _] = matrix;
    let x = (a * a + b * b).sqrt();
    let y = (c * c + d * d).sqrt();
    x.max(y)
}

/// One connected run of the path: its points, in device coordinates, and
/// whether it was closed.
#[derive(Clone, Debug, PartialEq)]
pub struct Subpath {
    pub points: Vec<Point>,
    pub closed: bool,
}

/// The current path of a context.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Path {
    subpaths: Vec<Subpath>,
    current: Option<Point>,
}

impl Path {
    /// The subpaths, in device coordinates.
    #[must_use]
    pub fn subpaths(&self) -> &[Subpath] {
        &self.subpaths
    }

    /// Discards every subpath (`beginPath`).
    pub fn clear(&mut self) {
        self.subpaths.clear();
        self.current = None;
    }

    /// The point the next segment starts from, in device coordinates.
    #[must_use]
    pub const fn current_point(&self) -> Option<Point> {
        self.current
    }

    /// Starts a new subpath at `point` (`moveTo`).
    pub fn move_to(&mut self, point: Point) {
        self.subpaths.push(Subpath {
            points: vec![point],
            closed: false,
        });
        self.current = Some(point);
    }

    /// Adds a straight segment to `point` (`lineTo`), starting a subpath when
    /// there is none, and a new one at the current point after a close.
    pub fn line_to(&mut self, point: Point) {
        let Some(current) = self.current else {
            self.move_to(point);
            return;
        };
        match self.subpaths.last_mut() {
            Some(last) if !last.closed => last.points.push(point),
            _ => self.subpaths.push(Subpath {
                points: vec![current, point],
                closed: false,
            }),
        }
        self.current = Some(point);
    }

    /// Closes the last subpath (`closePath`); the current point becomes its start.
    pub fn close(&mut self) {
        if let Some(last) = self.subpaths.last_mut() {
            if !last.closed {
                last.closed = true;
                self.current = last.points.first().copied();
            }
        }
    }

    /// Adds a closed rectangle subpath, as `rect()` does.
    pub fn add_rect(&mut self, corners: [Point; 4]) {
        self.move_to(corners[0]);
        for corner in &corners[1..] {
            self.line_to(*corner);
        }
        self.close();
    }

    /// Adds a cubic Bézier from the current point through `control_1`,
    /// `control_2` to `end` (`bezierCurveTo`). Points are already in device
    /// coordinates.
    pub fn cubic_to(&mut self, control_1: Point, control_2: Point, end: Point) {
        let start = match self.current {
            Some(start) => start,
            None => {
                self.move_to(control_1);
                control_1
            }
        };
        let flattened = flatten_cubic(start, control_1, control_2, end);
        for point in flattened {
            self.line_to(point);
        }
    }

    /// Adds a quadratic Bézier from the current point (`quadraticCurveTo`).
    pub fn quadratic_to(&mut self, control: Point, end: Point) {
        let start = match self.current {
            Some(start) => start,
            None => {
                self.move_to(control);
                control
            }
        };
        let control_1 = (
            start.0 + 2.0 / 3.0 * (control.0 - start.0),
            start.1 + 2.0 / 3.0 * (control.1 - start.1),
        );
        let control_2 = (
            end.0 + 2.0 / 3.0 * (control.0 - end.0),
            end.1 + 2.0 / 3.0 * (control.1 - end.1),
        );
        self.cubic_to(control_1, control_2, end);
    }

    /// The polygons a fill covers: every subpath, implicitly closed.
    #[must_use]
    pub fn polygons(&self) -> Vec<Vec<Point>> {
        self.subpaths
            .iter()
            .filter(|subpath| subpath.points.len() >= 2)
            .map(|subpath| subpath.points.clone())
            .collect()
    }
}

/// The device-space points of an elliptical arc in user space, mapped through
/// `matrix`. The arc runs from `start` to `end` (radians) in the direction
/// `counter_clockwise` names, with the specification's full-circle rule.
#[must_use]
pub fn arc_points(
    matrix: Matrix,
    center: Point,
    radius_x: f64,
    radius_y: f64,
    rotation: f64,
    start: f64,
    end: f64,
    counter_clockwise: bool,
) -> Vec<Point> {
    let sweep = arc_sweep(start, end, counter_clockwise);
    sweep_points(matrix, center, radius_x, radius_y, rotation, start, sweep)
}

/// The device-space points of an elliptical arc that starts at `start` and
/// turns by the signed `sweep` (radians).
#[must_use]
pub fn sweep_points(
    matrix: Matrix,
    center: Point,
    radius_x: f64,
    radius_y: f64,
    rotation: f64,
    start: f64,
    sweep: f64,
) -> Vec<Point> {
    let (sin_rotation, cos_rotation) = rotation.sin_cos();
    let scale = max_scale(matrix) * radius_x.abs().max(radius_y.abs());
    let step = if scale > 0.0 {
        (0.4 / scale).sqrt().clamp(1e-4, 0.5)
    } else {
        0.5
    };
    let count = ((sweep.abs() / step).ceil() as usize).clamp(1, 4096);
    (0..=count)
        .map(|index| {
            let angle = start + sweep * index as f64 / count as f64;
            let (sin_angle, cos_angle) = angle.sin_cos();
            let x = radius_x * cos_angle;
            let y = radius_y * sin_angle;
            let user = (
                center.0 + x * cos_rotation - y * sin_rotation,
                center.1 + x * sin_rotation + y * cos_rotation,
            );
            apply(matrix, user)
        })
        .collect()
}

/// The signed sweep from `start` to `end`: positive for clockwise, negative
/// for counter-clockwise, and a full turn when the span is at least a turn.
#[must_use]
pub fn arc_sweep(start: f64, end: f64, counter_clockwise: bool) -> f64 {
    if counter_clockwise {
        if start - end >= TAU {
            -TAU
        } else {
            -((start - end).rem_euclid(TAU))
        }
    } else if end - start >= TAU {
        TAU
    } else {
        (end - start).rem_euclid(TAU)
    }
}

/// The circular arc of an `arcTo` segment, in user space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ArcTo {
    /// Where the straight segment from the current point meets the arc.
    pub tangent_1: Point,
    /// Where the arc meets the segment towards the target; the path ends here.
    pub tangent_2: Point,
    pub centre: Point,
    pub start_angle: f64,
    /// The signed sweep from `start_angle` (radians).
    pub sweep: f64,
}

/// The circular arc of `arcTo(corner, target, radius)` from the user-space
/// current point `from`, per the specification's tangent construction.
///
/// Returns `None` when the segment is degenerate (collinear or zero-length),
/// where the specification draws a straight line to the corner instead.
#[must_use]
pub fn arc_to_geometry(from: Point, corner: Point, target: Point, radius: f64) -> Option<ArcTo> {
    let to_from = (from.0 - corner.0, from.1 - corner.1);
    let to_target = (target.0 - corner.0, target.1 - corner.1);
    let length_from = to_from.0.hypot(to_from.1);
    let length_target = to_target.0.hypot(to_target.1);
    if length_from == 0.0 || length_target == 0.0 || radius == 0.0 {
        return None;
    }
    let unit_from = (to_from.0 / length_from, to_from.1 / length_from);
    let unit_target = (to_target.0 / length_target, to_target.1 / length_target);
    let cross = unit_from.0 * unit_target.1 - unit_from.1 * unit_target.0;
    if cross.abs() < 1e-12 {
        return None;
    }
    let dot = (unit_from.0 * unit_target.0 + unit_from.1 * unit_target.1).clamp(-1.0, 1.0);
    let angle = dot.acos();
    let distance = radius / (angle / 2.0).tan();
    let tangent_1 = (
        corner.0 + unit_from.0 * distance,
        corner.1 + unit_from.1 * distance,
    );
    let tangent_2 = (
        corner.0 + unit_target.0 * distance,
        corner.1 + unit_target.1 * distance,
    );
    let bisector = (unit_from.0 + unit_target.0, unit_from.1 + unit_target.1);
    let bisector_length = bisector.0.hypot(bisector.1);
    let centre_distance = radius / (angle / 2.0).sin();
    let centre = (
        corner.0 + bisector.0 / bisector_length * centre_distance,
        corner.1 + bisector.1 / bisector_length * centre_distance,
    );
    let start_angle = (tangent_1.1 - centre.1).atan2(tangent_1.0 - centre.0);
    let end_angle = (tangent_2.1 - centre.1).atan2(tangent_2.0 - centre.0);
    // The minor arc between the two tangent points is the one that bends
    // round the corner, so the sweep is taken in (-π, π].
    let sweep = (end_angle - start_angle + PI).rem_euclid(TAU) - PI;
    Some(ArcTo {
        tangent_1,
        tangent_2,
        centre,
        start_angle,
        sweep,
    })
}

/// Flattens a cubic Bézier into device-space points, excluding `start` and
/// including `end`.
fn flatten_cubic(start: Point, control_1: Point, control_2: Point, end: Point) -> Vec<Point> {
    let control_length =
        distance(start, control_1) + distance(control_1, control_2) + distance(control_2, end);
    let count = ((control_length / 1.0).ceil() as usize).clamp(8, 512);
    (1..=count)
        .map(|index| {
            let t = index as f64 / count as f64;
            let u = 1.0 - t;
            let a = u * u * u;
            let b = 3.0 * u * u * t;
            let c = 3.0 * u * t * t;
            let d = t * t * t;
            (
                a * start.0 + b * control_1.0 + c * control_2.0 + d * end.0,
                a * start.1 + b * control_1.1 + c * control_2.1 + d * end.1,
            )
        })
        .collect()
}

fn distance(a: Point, b: Point) -> f64 {
    (b.0 - a.0).hypot(b.1 - a.1)
}
