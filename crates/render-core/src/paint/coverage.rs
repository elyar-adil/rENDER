//! Anti-aliased polygon coverage: the area-fill primitive of the CPU
//! rasterizer that canvas path fills, strokes and clips are built on.
//!
//! Coverage is computed per pixel as the fraction of the pixel's area that
//! lies inside the polygons under the fill rule. Horizontal extent is exact
//! (each span contributes its overlap with the pixel column), and vertical
//! extent is sampled at [`SUBROWS`] sub-scanlines per pixel row, so an edge
//! that falls on a pixel boundary yields full or zero coverage exactly.

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

use render_js::canvas::FillRule;

/// Sub-scanlines per pixel row. Four keeps horizontal edges within a quarter
/// pixel while sampling only as much as the coverage values can express.
const SUBROWS: usize = 4;

/// The coverage of one axis-aligned region, row-major, `0.0..=1.0` per pixel.
#[derive(Clone, Debug, PartialEq)]
pub struct Coverage {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    values: Vec<f32>,
}

impl Coverage {
    /// The top-left corner of the covered region.
    #[must_use]
    pub const fn origin(&self) -> (u32, u32) {
        (self.x, self.y)
    }

    /// The size of the covered region.
    #[must_use]
    pub const fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The coverage at pixel `(x, y)`, zero outside the covered region.
    #[must_use]
    pub fn at(&self, x: u32, y: u32) -> f32 {
        if x < self.x || y < self.y || x >= self.x + self.width || y >= self.y + self.height {
            return 0.0;
        }
        let index = (y - self.y) as usize * self.width as usize + (x - self.x) as usize;
        self.values.get(index).copied().unwrap_or(0.0)
    }
}

/// The anti-aliased coverage of `polygons` under `rule`, clipped to the
/// `limit_width` by `limit_height` pixel grid. `None` when nothing is covered.
///
/// Every polygon is implicitly closed. Points must be finite; the caller
/// filters non-finite geometry before it gets here.
#[must_use]
pub fn polygon_coverage(
    polygons: &[Vec<(f64, f64)>],
    rule: FillRule,
    limit_width: u32,
    limit_height: u32,
) -> Option<Coverage> {
    let mut bounds = (
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    for polygon in polygons {
        for &(x, y) in polygon {
            bounds.0 = bounds.0.min(x);
            bounds.1 = bounds.1.min(y);
            bounds.2 = bounds.2.max(x);
            bounds.3 = bounds.3.max(y);
        }
    }
    if !bounds.0.is_finite() || !bounds.2.is_finite() {
        return None;
    }
    let x0 = clamp_pixel(bounds.0.floor(), limit_width);
    let y0 = clamp_pixel(bounds.1.floor(), limit_height);
    let x1 = clamp_pixel(bounds.2.ceil(), limit_width);
    let y1 = clamp_pixel(bounds.3.ceil(), limit_height);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let width = x1 - x0;
    let height = y1 - y0;
    let mut values = vec![0.0_f32; width as usize * height as usize];

    let mut edges: Vec<(f64, f64, f64, f64)> = Vec::new();
    for polygon in polygons {
        for (index, &(ax, ay)) in polygon.iter().enumerate() {
            let (bx, by) = polygon[(index + 1) % polygon.len()];
            if ay != by {
                edges.push((ax, ay, bx, by));
            }
        }
    }

    let weight = 1.0 / SUBROWS as f32;
    let mut crossings: Vec<(f64, i32)> = Vec::new();
    for row in 0..height as usize {
        let line = &mut values[row * width as usize..(row + 1) * width as usize];
        for sub in 0..SUBROWS {
            let sample_y = f64::from(y0) + row as f64 + (sub as f64 + 0.5) / SUBROWS as f64;
            crossings.clear();
            for &(ax, ay, bx, by) in &edges {
                let (low, high, direction) = if ay < by { (ay, by, 1) } else { (by, ay, -1) };
                if sample_y >= low && sample_y < high {
                    let t = (sample_y - ay) / (by - ay);
                    crossings.push((ax + t * (bx - ax), direction));
                }
            }
            crossings.sort_by(|left, right| left.0.total_cmp(&right.0));
            let mut winding = 0_i32;
            for index in 0..crossings.len() {
                winding += crossings[index].1;
                let Some(next) = crossings.get(index + 1) else {
                    break;
                };
                if inside(rule, winding) {
                    add_span(line, f64::from(x0), crossings[index].0, next.0, weight);
                }
            }
        }
    }
    Some(Coverage {
        x: x0,
        y: y0,
        width,
        height,
        values,
    })
}

/// Whether the point `(x, y)` lies inside `polygons` under `rule`.
#[must_use]
pub fn contains(polygons: &[Vec<(f64, f64)>], rule: FillRule, x: f64, y: f64) -> bool {
    let mut winding = 0_i32;
    for polygon in polygons {
        for (index, &(ax, ay)) in polygon.iter().enumerate() {
            let (bx, by) = polygon[(index + 1) % polygon.len()];
            if ay <= y {
                if by > y && is_left(ax, ay, bx, by, x, y) > 0.0 {
                    winding += 1;
                }
            } else if by <= y && is_left(ax, ay, bx, by, x, y) < 0.0 {
                winding -= 1;
            }
        }
    }
    inside(rule, winding)
}

fn is_left(ax: f64, ay: f64, bx: f64, by: f64, px: f64, py: f64) -> f64 {
    (bx - ax) * (py - ay) - (px - ax) * (by - ay)
}

const fn inside(rule: FillRule, winding: i32) -> bool {
    match rule {
        FillRule::NonZero => winding != 0,
        FillRule::EvenOdd => winding % 2 != 0,
    }
}

fn clamp_pixel(value: f64, limit: u32) -> u32 {
    if value.is_nan() || value <= 0.0 {
        0
    } else if value >= f64::from(limit) {
        limit
    } else {
        value as u32
    }
}

/// Adds `weight` times the overlap of the span `[start, end)` with each pixel
/// column of `line`, where the line starts at `origin`.
fn add_span(line: &mut [f32], origin: f64, start: f64, end: f64, weight: f32) {
    let length = line.len() as f64;
    let start = (start - origin).max(0.0);
    let end = (end - origin).min(length);
    if end <= start {
        return;
    }
    let first = start.floor() as usize;
    let last = (end.ceil() as usize).min(line.len());
    for (column, value) in line.iter_mut().enumerate().take(last).skip(first) {
        let left = start.max(column as f64);
        let right = end.min(column as f64 + 1.0);
        if right > left {
            *value += (right - left) as f32 * weight;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{contains, polygon_coverage};
    use render_js::canvas::FillRule;

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<(f64, f64)> {
        vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
    }

    #[test]
    fn pixel_aligned_rectangle_is_fully_covered_and_nothing_else() {
        let coverage = polygon_coverage(&[rect(1.0, 2.0, 4.0, 5.0)], FillRule::NonZero, 10, 10)
            .expect("the rectangle covers pixels");
        assert_eq!(coverage.origin(), (1, 2));
        assert_eq!(coverage.size(), (3, 3));
        for y in 2..5 {
            for x in 1..4 {
                assert!((coverage.at(x, y) - 1.0).abs() < 1e-6, "({x},{y})");
            }
        }
        assert_eq!(coverage.at(0, 2), 0.0);
        assert_eq!(coverage.at(4, 2), 0.0);
    }

    #[test]
    fn fractional_edge_gives_partial_coverage() {
        let coverage = polygon_coverage(&[rect(0.0, 0.0, 2.5, 1.0)], FillRule::NonZero, 4, 4)
            .expect("covered");
        assert!((coverage.at(0, 0) - 1.0).abs() < 1e-6);
        assert!((coverage.at(2, 0) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn evenodd_leaves_an_overlap_empty_and_nonzero_fills_it() {
        let outer = rect(0.0, 0.0, 4.0, 4.0);
        let inner = rect(1.0, 1.0, 3.0, 3.0);
        let both = [outer.clone(), inner.clone()];
        let evenodd = polygon_coverage(&both, FillRule::EvenOdd, 4, 4).expect("covered");
        assert_eq!(evenodd.at(1, 1), 0.0);
        assert!((evenodd.at(0, 0) - 1.0).abs() < 1e-6);
        let nonzero = polygon_coverage(&both, FillRule::NonZero, 4, 4).expect("covered");
        assert!((nonzero.at(1, 1) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn point_containment_follows_the_fill_rule() {
        let square = [rect(0.0, 0.0, 4.0, 4.0)];
        assert!(contains(&square, FillRule::NonZero, 2.0, 2.0));
        assert!(!contains(&square, FillRule::NonZero, 5.0, 2.0));
    }
}
