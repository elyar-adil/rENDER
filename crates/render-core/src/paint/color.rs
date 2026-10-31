//! Device-independent sRGB paint colors.

use crate::css::color::{Rgba, SpaceColor, mix_space_colors};
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
        rgba_to_color(self.resolve_space(value, current_color).to_rgba())
    }

    /// Resolve context-dependent colors while retaining missing components so
    /// a `color-mix()` can carry them forward (§13.3).
    fn resolve_space(self, value: CssColor, current_color: Color) -> SpaceColor {
        match value {
            CssColor::Srgb {
                red,
                green,
                blue,
                alpha,
            } => SpaceColor::from_srgb8(red, green, blue, alpha),
            CssColor::Space(color) => *color,
            CssColor::CurrentColor => SpaceColor::from_rgba(rgba_from_color(current_color)),
            CssColor::Canvas => SpaceColor::from_rgba(rgba_from_color(self.canvas)),
            CssColor::CanvasText => SpaceColor::from_rgba(rgba_from_color(self.canvas_text)),
            CssColor::Mix(mix) => {
                let left = self.resolve_space(mix.left, current_color);
                let right = self.resolve_space(mix.right, current_color);
                mix_space_colors(
                    mix.space,
                    mix.hue,
                    left,
                    right,
                    mix.left_weight,
                    mix.right_weight,
                    mix.alpha_multiplier,
                )
            }
        }
    }
}

fn rgba_from_color(color: Color) -> Rgba {
    Rgba::from_srgb8(
        color.red,
        color.green,
        color.blue,
        f32::from(color.alpha) / 255.0,
    )
}

fn rgba_to_color(value: Rgba) -> Color {
    let (red, green, blue) = value.to_srgb8();
    Color::rgba(
        red,
        green,
        blue,
        clamped_rounded_u8(value.alpha.clamp(0.0, 1.0) * 255.0),
    )
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

#[cfg(test)]
mod tests {
    use crate::css::properties::{CssColor, TypedPropertyValue, parse_typed_property};

    use super::{Color, SystemPalette};

    fn parsed(value: &str) -> CssColor {
        match parse_typed_property("color", value) {
            Some(Ok(TypedPropertyValue::Color(color))) => color,
            other => panic!("expected a color, got {other:?}"),
        }
    }

    #[test]
    fn color_mix_with_currentcolor_resolves_at_paint_time() {
        let color = parsed("color-mix(in srgb, currentcolor 20%, transparent)");
        assert!(matches!(color, CssColor::Mix(_)));
        // Premultiplied mixing of 20% opaque blue with 80% transparent leaves
        // a blue at 20% alpha; the hue must not be diluted towards black.
        assert_eq!(
            SystemPalette::default().resolve(color, Color::rgb(0, 0, 255)),
            Color::rgba(0, 0, 255, 51)
        );
    }

    #[test]
    fn absolute_color_mix_is_already_an_srgb_color() {
        assert!(matches!(
            parsed("color-mix(in oklab, red, blue)"),
            CssColor::Srgb { .. }
        ));
    }

    #[test]
    fn missing_components_render_as_zero_at_paint_time() {
        assert_eq!(
            SystemPalette::default().resolve(parsed("rgb(none 128 0 / none)"), Color::BLACK),
            Color::rgba(0, 128, 0, 0)
        );
        // All components missing in one operand are carried from currentcolor,
        // so the mix resolves to the current color itself.
        assert_eq!(
            SystemPalette::default().resolve(
                parsed("color-mix(in oklch, currentcolor, oklch(none none none))"),
                Color::rgb(0, 0, 255),
            ),
            Color::rgb(0, 0, 255)
        );
    }

    #[test]
    fn out_of_gamut_colors_use_perceptual_gamut_mapping_for_paint() {
        let palette = SystemPalette::default();
        assert_eq!(
            palette.resolve(parsed("color(display-p3 1 0 0)"), Color::BLACK),
            Color::rgb(255, 11, 12)
        );
        assert_eq!(
            palette.resolve(parsed("oklch(0.7 0.4 150)"), Color::BLACK),
            Color::rgb(0, 194, 72)
        );
    }
}
