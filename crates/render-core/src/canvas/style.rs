//! Colour and line-style values of the 2D context: parsing with the CSS colour
//! grammar the engine already has, the specification's serialization, and the
//! keyword sets of the enumerated attributes.

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

use super::bitmap::round_byte;
use crate::css::properties::{CssColor, TypedPropertyValue, parse_typed_property};
use render_js::canvas::GradientKind;

/// A colour as the context stores it: opaque RGB and a CSS alpha in `0.0..=1.0`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Paint {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub alpha: f32,
}

impl Paint {
    /// Opaque black, the initial `fillStyle` and `strokeStyle`.
    pub const BLACK: Self = Self {
        red: 0,
        green: 0,
        blue: 0,
        alpha: 1.0,
    };
}

/// The line end style (HTML §4.12.5.1.11).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineCap {
    Butt,
    Round,
    Square,
}

/// The line join style (HTML §4.12.5.1.11).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineJoin {
    Bevel,
    Miter,
    Round,
}

impl LineCap {
    #[must_use]
    pub fn parse(keyword: &str) -> Option<Self> {
        match keyword {
            "butt" => Some(Self::Butt),
            "round" => Some(Self::Round),
            "square" => Some(Self::Square),
            _ => None,
        }
    }

    #[must_use]
    pub const fn keyword(self) -> &'static str {
        match self {
            Self::Butt => "butt",
            Self::Round => "round",
            Self::Square => "square",
        }
    }
}

impl LineJoin {
    #[must_use]
    pub fn parse(keyword: &str) -> Option<Self> {
        match keyword {
            "bevel" => Some(Self::Bevel),
            "miter" => Some(Self::Miter),
            "round" => Some(Self::Round),
            _ => None,
        }
    }

    #[must_use]
    pub const fn keyword(self) -> &'static str {
        match self {
            Self::Bevel => "bevel",
            Self::Miter => "miter",
            Self::Round => "round",
        }
    }
}

/// Parses a CSS `<color>`. A value that is not a colour parses to `None`, and
/// the caller leaves the attribute unchanged, as the specification requires.
#[must_use]
pub fn parse_paint(text: &str) -> Option<Paint> {
    match parse_typed_property("color", text) {
        Some(Ok(TypedPropertyValue::Color(CssColor::Srgb {
            red,
            green,
            blue,
            alpha,
        }))) => Some(Paint {
            red,
            green,
            blue,
            alpha: alpha.clamp(0.0, 1.0),
        }),
        _ => None,
    }
}

/// The serialization of a colour the `fillStyle` and `strokeStyle` getters
/// return: `#rrggbb` when opaque, otherwise `rgba(r, g, b, a)`.
#[must_use]
pub fn serialize_paint(paint: Paint) -> String {
    if paint.alpha >= 1.0 {
        return format!("#{:02x}{:02x}{:02x}", paint.red, paint.green, paint.blue);
    }
    let rounded = (paint.alpha * 1000.0).round() / 1000.0;
    format!(
        "rgba({}, {}, {}, {})",
        paint.red, paint.green, paint.blue, rounded
    )
}

/// The colour a gradient shows at one user-space point (HTML §4.12.5.1.3).
///
/// A linear gradient projects the point onto the segment between the two
/// ends; a radial one solves for the circle of the start-to-end circle family
/// the point lies on, taking the larger root, which is what the standard's
/// "the value of t" names. Both clamp to the end stops, and no stop at all
/// means the caller never asked.
pub(super) fn gradient_at(
    kind: &GradientKind,
    stops: &[(f64, Paint)],
    x: f64,
    y: f64,
) -> (u8, u8, u8, f32) {
    let t = match *kind {
        GradientKind::Linear { x0, y0, x1, y1 } => {
            let dx = x1 - x0;
            let dy = y1 - y0;
            let length_squared = dx * dx + dy * dy;
            if length_squared <= 0.0 {
                1.0
            } else {
                ((x - x0) * dx + (y - y0) * dy) / length_squared
            }
        }
        GradientKind::Radial {
            x0,
            y0,
            r0,
            x1,
            y1,
            r1,
        } => {
            let dx = x1 - x0;
            let dy = y1 - y0;
            let dr = r1 - r0;
            let length_squared = dx * dx + dy * dy;
            // |c0 + t*(c1-c0) - p| = r0 + t*dr, squared: a quadratic in t.
            let ox = x0 - x;
            let oy = y0 - y;
            let b: f64 = 2.0 * (ox * dx + oy * dy + r0 * dr);
            let c: f64 = ox * ox + oy * oy - r0 * r0;
            let a: f64 = length_squared - dr * dr;
            if a.abs() < f64::EPSILON {
                if b.abs() < f64::EPSILON { 1.0 } else { -c / b }
            } else {
                let discriminant = b * b - 4.0 * a * c;
                if discriminant < 0.0 {
                    // Off every circle of the family: the standard clamps to
                    // the nearest end of the parameter range.
                    if c > 0.0 { 0.0 } else { 1.0 }
                } else {
                    let root = discriminant.sqrt();
                    let t1 = (-b + root) / (2.0 * a);
                    let t2 = (-b - root) / (2.0 * a);
                    t1.max(t2).max(0.0)
                }
            }
        }
    };
    gradient_colour(stops, t.clamp(0.0, 1.0))
}

/// The straight-alpha interpolation between the stops a parameter falls
/// between, clamped to the end stops outside them.
fn gradient_colour(stops: &[(f64, Paint)], t: f64) -> (u8, u8, u8, f32) {
    let mut stops = stops.to_vec();
    stops.sort_by(|left, right| left.0.total_cmp(&right.0));
    let Some((first_offset, first)) = stops.first() else {
        return (0, 0, 0, 0.0);
    };
    if t <= *first_offset {
        return (first.red, first.green, first.blue, first.alpha);
    }
    let Some((last_offset, last)) = stops.last() else {
        return (0, 0, 0, 0.0);
    };
    if t >= *last_offset {
        return (last.red, last.green, last.blue, last.alpha);
    }
    for pair in stops.windows(2) {
        let (start_offset, start) = pair[0];
        let (end_offset, end) = pair[1];
        if t >= start_offset && t <= end_offset {
            let span = end_offset - start_offset;
            let local = if span > 0.0 {
                (t - start_offset) / span
            } else {
                1.0
            };
            let mix = |a: u8, b: u8| -> u8 {
                round_byte(f32::from(a) + (f32::from(b) - f32::from(a)) * local as f32)
            };
            return (
                mix(start.red, end.red),
                mix(start.green, end.green),
                mix(start.blue, end.blue),
                start.alpha + (end.alpha - start.alpha) * local as f32,
            );
        }
    }
    (last.red, last.green, last.blue, last.alpha)
}

#[cfg(test)]
mod tests {
    use super::{Paint, parse_paint, serialize_paint};

    #[test]
    fn opaque_colours_serialize_as_hex_and_translucent_ones_as_rgba() {
        let red = parse_paint("red").expect("a named colour parses");
        assert_eq!(serialize_paint(red), "#ff0000");
        let half = parse_paint("rgba(0, 255, 0, 0.5)").expect("an rgba colour parses");
        assert_eq!(serialize_paint(half), "rgba(0, 255, 0, 0.5)");
    }

    #[test]
    fn a_non_colour_is_refused() {
        assert_eq!(parse_paint("not a colour"), None);
        assert_eq!(parse_paint(""), None);
        assert_eq!(Paint::BLACK.alpha, 1.0);
    }
}
