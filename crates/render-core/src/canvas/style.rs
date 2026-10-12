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

use crate::css::properties::{CssColor, TypedPropertyValue, parse_typed_property};

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
