//! Device-independent sRGB paint colors.

use crate::css::properties::CssColor;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Color {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub alpha: u8,
}

impl Color {
    pub const TRANSPARENT: Self = Self::rgba(0, 0, 0, 0);
    pub const BLACK: Self = Self::rgb(0, 0, 0);
    pub const WHITE: Self = Self::rgb(255, 255, 255);

    #[must_use]
    pub const fn rgb(red: u8, green: u8, blue: u8) -> Self {
        Self::rgba(red, green, blue, 255)
    }

    #[must_use]
    pub const fn rgba(red: u8, green: u8, blue: u8, alpha: u8) -> Self {
        Self {
            red,
            green,
            blue,
            alpha,
        }
    }

    #[must_use]
    pub fn with_opacity(self, opacity: f32) -> Self {
        Self {
            alpha: clamped_rounded_u8(f32::from(self.alpha) * opacity.clamp(0.0, 1.0)),
            ..self
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SystemPalette {
    pub canvas: Color,
    pub canvas_text: Color,
}

impl Default for SystemPalette {
    fn default() -> Self {
        Self {
            canvas: Color::WHITE,
            canvas_text: Color::BLACK,
        }
    }
}

impl SystemPalette {
    #[must_use]
    pub fn resolve(self, value: CssColor, current_color: Color) -> Color {
        match value {
            CssColor::Srgb {
                red,
                green,
                blue,
                alpha,
            } => Color::rgba(
                red,
                green,
                blue,
                clamped_rounded_u8(alpha.clamp(0.0, 1.0) * 255.0),
            ),
            CssColor::CurrentColor => current_color,
            CssColor::Canvas => self.canvas,
            CssColor::CanvasText => self.canvas_text,
        }
    }
}

/// Converts a floating-point color channel to its 8-bit device representation.
///
/// The cast is isolated here because Rust's saturating float-to-integer semantics
/// are exactly the behavior required at this device-color boundary.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is rounded and clamped to the complete u8 range"
)]
pub(crate) fn clamped_rounded_u8(value: f32) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}
