//! CSS Color 4 color spaces, missing components and CSS Color 5 `color-mix()`.
//!
//! The paint contract is sRGB, so every color space here exists to convert
//! into and out of sRGB. The conversion matrices and transfer functions are
//! the sample code in CSS Color 4 §19, evaluated in `f32` because the engine's
//! device colors are eight-bit sRGB.
//!
//! Colors that contain `none` components (CSS Color 4 §4.4) keep their
//! authored color space in [`SpaceColor`] so interpolation can carry missing
//! components forward to analogous components (§13.3). Every other use treats
//! a missing component as zero. Out-of-gamut colors are mapped into sRGB with
//! the OKLCH chroma-reduction algorithm of §14.2 rather than clipped.

use crate::properties::{CssColor, format_number};

/// A floating-point sRGB color with gamma-encoded components.
///
/// Components may be outside `0.0..=1.0` until the color reaches the device
/// boundary; [`Self::clamped`] applies the used-value clamp.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba {
    pub red: f32,
    pub green: f32,
    pub blue: f32,
    pub alpha: f32,
}

impl Rgba {
    pub const TRANSPARENT: Self = Self::new(0.0, 0.0, 0.0, 0.0);

    #[must_use]
    pub const fn new(red: f32, green: f32, blue: f32, alpha: f32) -> Self {
        Self {
            red,
            green,
            blue,
            alpha,
        }
    }

    #[must_use]
    pub fn from_srgb8(red: u8, green: u8, blue: u8, alpha: f32) -> Self {
        Self {
            red: f32::from(red) / 255.0,
            green: f32::from(green) / 255.0,
            blue: f32::from(blue) / 255.0,
            alpha,
        }
    }

    /// Clamp every channel, including alpha, to its reference range without
    /// any perceptual gamut mapping. Used for the clipping stage inside the
    /// gamut-mapping algorithm and for already-sRGB interpolation inputs.
    #[must_use]
    pub fn clamped(self) -> Self {
        Self {
            red: self.red.clamp(0.0, 1.0),
            green: self.green.clamp(0.0, 1.0),
            blue: self.blue.clamp(0.0, 1.0),
            alpha: self.alpha.clamp(0.0, 1.0),
        }
    }

    /// CSS Color 4 §14.2: map an out-of-gamut sRGB color into the sRGB
    /// gamut by reducing OKLCH chroma while preserving lightness and hue.
    /// In-gamut colors are returned unchanged.
    #[must_use]
    pub fn gamut_mapped(self) -> Self {
        gamut_map_srgb(self)
    }

    /// The eight-bit sRGB color a device boundary accepts. Out-of-gamut
    /// colors are perceptually mapped with [`Self::gamut_mapped`], not
    /// clipped channel by channel.
    #[must_use]
    pub fn to_srgb8(self) -> (u8, u8, u8) {
        let value = self.gamut_mapped();
        (
            rounded_channel(value.red * 255.0),
            rounded_channel(value.green * 255.0),
            rounded_channel(value.blue * 255.0),
        )
    }
}

/// A color space named by `color-mix(in ...)` or `color()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorSpace {
    Srgb,
    SrgbLinear,
    DisplayP3,
    A98Rgb,
    ProphotoRgb,
    Rec2020,
    Xyz,
    XyzD50,
    XyzD65,
    Lab,
    Oklab,
    Lch,
    Oklch,
    Hsl,
    Hwb,
}

impl ColorSpace {
    #[must_use]
    pub fn from_ident(name: &str) -> Option<Self> {
        Some(match name.to_ascii_lowercase().as_str() {
            "srgb" => Self::Srgb,
            "srgb-linear" => Self::SrgbLinear,
            "display-p3" => Self::DisplayP3,
            "a98-rgb" => Self::A98Rgb,
            "prophoto-rgb" => Self::ProphotoRgb,
            "rec2020" => Self::Rec2020,
            "xyz" | "xyz-d65" => Self::XyzD65,
            "xyz-d50" => Self::XyzD50,
            "lab" => Self::Lab,
            "oklab" => Self::Oklab,
            "lch" => Self::Lch,
            "oklch" => Self::Oklch,
            "hsl" => Self::Hsl,
            "hwb" => Self::Hwb,
            _ => return None,
        })
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Srgb => "srgb",
            Self::SrgbLinear => "srgb-linear",
            Self::DisplayP3 => "display-p3",
            Self::A98Rgb => "a98-rgb",
            Self::ProphotoRgb => "prophoto-rgb",
            Self::Rec2020 => "rec2020",
            Self::Xyz => "xyz",
            Self::XyzD50 => "xyz-d50",
            Self::XyzD65 => "xyz-d65",
            Self::Lab => "lab",
            Self::Oklab => "oklab",
            Self::Lch => "lch",
            Self::Oklch => "oklch",
            Self::Hsl => "hsl",
            Self::Hwb => "hwb",
        }
    }

    #[must_use]
    pub const fn is_polar(self) -> bool {
        matches!(self, Self::Hsl | Self::Hwb | Self::Lch | Self::Oklch)
    }

    /// The index of the hue component for polar spaces. Hue angles are not
    /// premultiplied by alpha; every other component is.
    #[must_use]
    const fn hue_axis(self) -> Option<usize> {
        match self {
            Self::Hsl | Self::Hwb => Some(0),
            Self::Lch | Self::Oklch => Some(2),
            _ => None,
        }
    }

    /// Whether `color()` may take this space. The polar color spaces and the
    /// polar-only `lab`/`oklab` spellings are not `<predefined-rgb>` spaces.
    #[must_use]
    pub const fn is_predefined_space(self) -> bool {
        matches!(
            self,
            Self::Srgb
                | Self::SrgbLinear
                | Self::DisplayP3
                | Self::A98Rgb
                | Self::ProphotoRgb
                | Self::Rec2020
                | Self::Xyz
                | Self::XyzD50
                | Self::XyzD65
        )
    }
}

/// A color retained in its authored color space because at least one
/// component is missing (CSS Color 4 §4.4).
///
/// Components use the same units as the corresponding color function, except
/// that sRGB components are normalized to `0.0..=1.0`; `lab()`/`lch()`
/// lightness is `0.0..=100.0` and `oklab()`/`oklch()` lightness is
/// `0.0..=1.0`, matching [`ColorSpace`] conversions. Keeping the source space
/// lets interpolation carry a missing component forward to its analogous
/// component before the color is converted (§13.3).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpaceColor {
    pub space: ColorSpace,
    pub components: [Option<f32>; 3],
    pub alpha: Option<f32>,
}

impl SpaceColor {
    #[must_use]
    pub const fn new(space: ColorSpace, components: [Option<f32>; 3], alpha: Option<f32>) -> Self {
        Self {
            space,
            components,
            alpha,
        }
    }

    #[must_use]
    pub fn from_rgba(color: Rgba) -> Self {
        Self {
            space: ColorSpace::Srgb,
            components: [Some(color.red), Some(color.green), Some(color.blue)],
            alpha: Some(color.alpha),
        }
    }

    #[must_use]
    pub fn from_srgb8(red: u8, green: u8, blue: u8, alpha: f32) -> Self {
        Self::from_rgba(Rgba::from_srgb8(red, green, blue, alpha))
    }

    /// Whether any component or the alpha is missing.
    #[must_use]
    pub fn has_missing(self) -> bool {
        self.alpha.is_none() || self.components.iter().any(Option::is_none)
    }

    /// Convert to sRGB, treating missing components as zero (§4.4). The
    /// resulting components may be outside `0.0..=1.0`; the device boundary
    /// applies [`Rgba::gamut_mapped`].
    #[must_use]
    pub fn to_rgba(self) -> Rgba {
        let components = self.components.map(|component| component.unwrap_or(0.0));
        let mut color = space_to_srgb(self.space, components);
        color.alpha = self.alpha.unwrap_or(0.0);
        color
    }

    /// Serialize the color with missing components spelled `none`, preserving
    /// the function shape of its color space (§4.4, §16).
    #[must_use]
    pub fn to_css(&self) -> String {
        let component = |value: Option<f32>| match value {
            None => "none".to_owned(),
            Some(value) => format_number(value),
        };
        let percentage = |value: Option<f32>| match value {
            None => "none".to_owned(),
            Some(value) => format!("{}%", format_number(value * 100.0)),
        };
        let alpha = match self.alpha {
            None => " / none".to_owned(),
            Some(value) if value < 1.0 => format!(" / {}", format_number(value)),
            Some(_) => String::new(),
        };
        let [first, second, third] = self.components;
        let lightness = |value: Option<f32>| match value {
            None => "none".to_owned(),
            Some(value) => format!("{}%", format_number(value)),
        };
        match self.space {
            ColorSpace::Hsl => format!(
                "hsl({} {} {}{alpha})",
                component(first),
                percentage(second),
                percentage(third)
            ),
            ColorSpace::Hwb => format!(
                "hwb({} {} {}{alpha})",
                component(first),
                percentage(second),
                percentage(third)
            ),
            ColorSpace::Lab => format!(
                "lab({} {} {}{alpha})",
                lightness(first),
                component(second),
                component(third)
            ),
            ColorSpace::Lch => format!(
                "lch({} {} {}{alpha})",
                lightness(first),
                component(second),
                component(third)
            ),
            ColorSpace::Oklab => format!(
                "oklab({} {} {}{alpha})",
                component(first),
                component(second),
                component(third)
            ),
            ColorSpace::Oklch => format!(
                "oklch({} {} {}{alpha})",
                component(first),
                component(second),
                component(third)
            ),
            _ => format!(
                "color({} {} {} {}{alpha})",
                self.space.name(),
                component(first),
                component(second),
                component(third)
            ),
        }
    }
}

/// One of the color functions that supports relative color syntax
/// (CSS Color 5 §4). The processing space is fixed by the function, except for
/// `color()`, whose space is written explicitly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelativeFunction {
    Rgb,
    Hsl,
    Hwb,
    Lab,
    Lch,
    Oklab,
    Oklch,
    Color(ColorSpace),
}

/// A component keyword in a relative color expression.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelativeKeyword {
    /// One of the three channel keywords of the processing space.
    Component(u8),
    /// The origin color's alpha, a number in `0.0..=1.0`.
    Alpha,
}

/// One argument of a relative color function.
#[derive(Clone, Debug, PartialEq)]
pub enum RelativeChannel {
    /// The `none` keyword: a missing component.
    Missing,
    Number(f32),
    /// A percentage stored as a unit value (`100%` is `1.0`).
    Percentage(f32),
    Keyword(RelativeKeyword),
    Calc(Box<CalcExpr>),
}

/// The expression tree accepted inside `calc()` in a relative color.
#[derive(Clone, Debug, PartialEq)]
pub enum CalcExpr {
    Number(f32),
    /// A percentage stored as a unit value (`100%` is `1.0`).
    Percentage(f32),
    Keyword(RelativeKeyword),
    Add(Box<Self>, Box<Self>),
    Sub(Box<Self>, Box<Self>),
    Mul(Box<Self>, Box<Self>),
    Div(Box<Self>, Box<Self>),
    Negate(Box<Self>),
}

/// The alpha argument of a relative color. `Inherit` is the omitted form,
/// which copies the origin's alpha rather than defaulting to one.
#[derive(Clone, Debug, PartialEq)]
pub enum RelativeAlpha {
    Inherit,
    Channel(RelativeChannel),
}

/// An unresolved relative color (CSS Color 5 §4).
///
/// A relative color whose origin is absolute is evaluated while parsing and
/// never reaches this type. The type exists for origins that depend on context:
/// `currentcolor`, a system color, or `light-dark()`.
#[derive(Clone, Debug, PartialEq)]
pub struct RelativeColor {
    pub function: RelativeFunction,
    pub origin: CssColor,
    pub components: [RelativeChannel; 3],
    pub alpha: RelativeAlpha,
}

/// `alpha(from <color> / <alpha-value>)` (CSS Color 5 §4.10): the origin's
/// color components are unchanged and only its alpha is replaced or modified.
#[derive(Clone, Debug, PartialEq)]
pub struct AlphaColor {
    pub origin: CssColor,
    pub alpha: RelativeChannel,
}

impl AlphaColor {
    #[must_use]
    pub fn to_css(&self) -> String {
        let units = RelativeUnits {
            names: ["", "", ""],
            number_scale: [1.0; 3],
            percentage_scale: [0.0; 3],
        };
        format!(
            "alpha(from {} / {})",
            self.origin.to_css(),
            relative_channel_to_css(&self.alpha, &units)
        )
    }
}

/// Evaluate an `alpha()` relative color against its resolved origin. The
/// origin's components are retained exactly; only alpha changes.
#[must_use]
pub fn resolve_alpha_space(value: &AlphaColor, origin: SpaceColor) -> SpaceColor {
    let alpha = eval_relative_alpha(&value.alpha, &[None; 3], origin.alpha);
    SpaceColor::new(origin.space, origin.components, alpha)
}

/// `light-dark(<color>, <color>)` (CSS Color 5 §7). The used color scheme
/// selects one arm at paint time.
#[derive(Clone, Debug, PartialEq)]
pub struct LightDarkColor {
    pub light: CssColor,
    pub dark: CssColor,
}

impl LightDarkColor {
    #[must_use]
    pub fn to_css(&self) -> String {
        format!(
            "light-dark({}, {})",
            self.light.to_css(),
            self.dark.to_css()
        )
    }
}

/// The units one relative color function uses for its three components.
///
/// `number_scale` converts an authored number (the form channel keywords and
/// `calc()` use) into the engine's internal units. `percentage_scale` is the
/// authored number that `100%` means in that position; a zero entry means the
/// position does not accept a percentage (a hue).
#[derive(Clone, Copy)]
struct RelativeUnits {
    names: [&'static str; 3],
    number_scale: [f32; 3],
    percentage_scale: [f32; 3],
}

impl RelativeFunction {
    #[must_use]
    pub const fn space(self) -> ColorSpace {
        match self {
            Self::Rgb => ColorSpace::Srgb,
            Self::Hsl => ColorSpace::Hsl,
            Self::Hwb => ColorSpace::Hwb,
            Self::Lab => ColorSpace::Lab,
            Self::Lch => ColorSpace::Lch,
            Self::Oklab => ColorSpace::Oklab,
            Self::Oklch => ColorSpace::Oklch,
            Self::Color(space) => space,
        }
    }

    #[must_use]
    pub const fn component_names(self) -> [&'static str; 3] {
        match self.space() {
            ColorSpace::Srgb
            | ColorSpace::SrgbLinear
            | ColorSpace::DisplayP3
            | ColorSpace::A98Rgb
            | ColorSpace::ProphotoRgb
            | ColorSpace::Rec2020 => ["r", "g", "b"],
            ColorSpace::Xyz | ColorSpace::XyzD50 | ColorSpace::XyzD65 => ["x", "y", "z"],
            ColorSpace::Lab | ColorSpace::Oklab => ["l", "a", "b"],
            ColorSpace::Lch | ColorSpace::Oklch => ["l", "c", "h"],
            ColorSpace::Hsl => ["h", "s", "l"],
            ColorSpace::Hwb => ["h", "w", "b"],
        }
    }

    fn units(self) -> RelativeUnits {
        let names = self.component_names();
        match self {
            Self::Rgb => RelativeUnits {
                names,
                number_scale: [1.0 / 255.0; 3],
                percentage_scale: [255.0; 3],
            },
            Self::Hsl | Self::Hwb => RelativeUnits {
                names,
                number_scale: [1.0, 0.01, 0.01],
                percentage_scale: [0.0, 100.0, 100.0],
            },
            Self::Lab => RelativeUnits {
                names,
                number_scale: [1.0; 3],
                percentage_scale: [100.0, 125.0, 125.0],
            },
            Self::Lch => RelativeUnits {
                names,
                number_scale: [1.0; 3],
                percentage_scale: [100.0, 150.0, 0.0],
            },
            Self::Oklab => RelativeUnits {
                names,
                number_scale: [1.0; 3],
                percentage_scale: [1.0, 0.4, 0.4],
            },
            Self::Oklch => RelativeUnits {
                names,
                number_scale: [1.0; 3],
                percentage_scale: [1.0, 0.4, 0.0],
            },
            Self::Color(_) => RelativeUnits {
                names,
                number_scale: [1.0; 3],
                percentage_scale: [1.0; 3],
            },
        }
    }

    /// Whether component `index` is a hue, whose percentage form is invalid.
    #[must_use]
    pub const fn is_hue(self, index: usize) -> bool {
        matches!(
            (self, index),
            (Self::Hsl | Self::Hwb, 0) | (Self::Lch | Self::Oklch, 2)
        )
    }
}

/// Evaluate a relative color whose origin has already been resolved.
#[must_use]
pub fn resolve_relative_space(relative: &RelativeColor, origin: SpaceColor) -> SpaceColor {
    let function = relative.function;
    let space = function.space();
    let units = function.units();
    let converted = if origin.space == space {
        origin.components
    } else {
        let carried = carried_missing(origin.space, space, &origin.components);
        let mut converted = convert_components(origin.space, space, origin.components).map(Some);
        powerless_to_missing(space, &mut converted);
        for index in 0..3 {
            if carried[index] {
                converted[index] = None;
            }
        }
        converted
    };
    let spec = [0, 1, 2].map(|index| {
        converted[index].map(|value| {
            let scale = units.number_scale[index];
            if scale == 0.0 { value } else { value / scale }
        })
    });
    let origin_alpha = origin.alpha;
    let components = [0, 1, 2].map(|index| {
        eval_relative_channel(
            &relative.components[index],
            index,
            &spec,
            origin_alpha,
            &units,
        )
    });
    let alpha = match &relative.alpha {
        RelativeAlpha::Inherit => origin_alpha,
        RelativeAlpha::Channel(channel) => eval_relative_alpha(channel, &spec, origin_alpha),
    };
    SpaceColor::new(space, components, alpha)
}

fn eval_relative_channel(
    channel: &RelativeChannel,
    slot: usize,
    spec: &[Option<f32>; 3],
    origin_alpha: Option<f32>,
    units: &RelativeUnits,
) -> Option<f32> {
    match channel {
        RelativeChannel::Missing => None,
        RelativeChannel::Number(value) => Some(value * units.number_scale[slot]),
        RelativeChannel::Percentage(value) => {
            let scale = units.percentage_scale[slot];
            (scale != 0.0).then_some(value * scale * units.number_scale[slot])
        }
        RelativeChannel::Keyword(RelativeKeyword::Component(index)) => spec
            .get(usize::from(*index))
            .copied()
            .flatten()
            .map(|value| value * units.number_scale[slot]),
        RelativeChannel::Keyword(RelativeKeyword::Alpha) => origin_alpha,
        RelativeChannel::Calc(expression) => {
            Some(eval_calc(expression, slot, spec, origin_alpha, units))
        }
    }
}

fn eval_relative_alpha(
    channel: &RelativeChannel,
    spec: &[Option<f32>; 3],
    origin_alpha: Option<f32>,
) -> Option<f32> {
    let value = match channel {
        RelativeChannel::Missing => None,
        RelativeChannel::Number(value) => Some(*value),
        RelativeChannel::Percentage(value) => Some(*value),
        RelativeChannel::Keyword(RelativeKeyword::Component(index)) => {
            spec.get(usize::from(*index)).copied().flatten()
        }
        RelativeChannel::Keyword(RelativeKeyword::Alpha) => origin_alpha,
        RelativeChannel::Calc(expression) => Some(eval_alpha_calc(expression, spec, origin_alpha)),
    };
    value.map(|value| value.clamp(0.0, 1.0))
}

fn eval_calc(
    expression: &CalcExpr,
    slot: usize,
    spec: &[Option<f32>; 3],
    origin_alpha: Option<f32>,
    units: &RelativeUnits,
) -> f32 {
    eval_calc_spec(expression, slot, spec, origin_alpha, units) * units.number_scale[slot]
}

/// Evaluate a `calc()` tree in the function's authored number units. The
/// channel keywords are the origin's numbers (`r` is 0..255 in `rgb()`), a
/// percentage uses the slot's reference range, and the caller converts the
/// final number into the engine's internal units once.
fn eval_calc_spec(
    expression: &CalcExpr,
    slot: usize,
    spec: &[Option<f32>; 3],
    origin_alpha: Option<f32>,
    units: &RelativeUnits,
) -> f32 {
    match expression {
        CalcExpr::Number(value) => *value,
        CalcExpr::Percentage(value) => *value * units.percentage_scale[slot],
        CalcExpr::Keyword(RelativeKeyword::Component(index)) => spec
            .get(usize::from(*index))
            .copied()
            .flatten()
            .unwrap_or(0.0),
        CalcExpr::Keyword(RelativeKeyword::Alpha) => origin_alpha.unwrap_or(0.0),
        CalcExpr::Add(left, right) => {
            eval_calc_spec(left, slot, spec, origin_alpha, units)
                + eval_calc_spec(right, slot, spec, origin_alpha, units)
        }
        CalcExpr::Sub(left, right) => {
            eval_calc_spec(left, slot, spec, origin_alpha, units)
                - eval_calc_spec(right, slot, spec, origin_alpha, units)
        }
        CalcExpr::Mul(left, right) => {
            eval_calc_spec(left, slot, spec, origin_alpha, units)
                * eval_calc_spec(right, slot, spec, origin_alpha, units)
        }
        CalcExpr::Div(left, right) => {
            let divisor = eval_calc_spec(right, slot, spec, origin_alpha, units);
            if divisor == 0.0 {
                0.0
            } else {
                eval_calc_spec(left, slot, spec, origin_alpha, units) / divisor
            }
        }
        CalcExpr::Negate(value) => -eval_calc_spec(value, slot, spec, origin_alpha, units),
    }
}

fn eval_alpha_calc(
    expression: &CalcExpr,
    spec: &[Option<f32>; 3],
    origin_alpha: Option<f32>,
) -> f32 {
    match expression {
        CalcExpr::Number(value) | CalcExpr::Percentage(value) => *value,
        CalcExpr::Keyword(RelativeKeyword::Component(index)) => spec
            .get(usize::from(*index))
            .copied()
            .flatten()
            .unwrap_or(0.0),
        CalcExpr::Keyword(RelativeKeyword::Alpha) => origin_alpha.unwrap_or(0.0),
        CalcExpr::Add(left, right) => {
            eval_alpha_calc(left, spec, origin_alpha) + eval_alpha_calc(right, spec, origin_alpha)
        }
        CalcExpr::Sub(left, right) => {
            eval_alpha_calc(left, spec, origin_alpha) - eval_alpha_calc(right, spec, origin_alpha)
        }
        CalcExpr::Mul(left, right) => {
            eval_alpha_calc(left, spec, origin_alpha) * eval_alpha_calc(right, spec, origin_alpha)
        }
        CalcExpr::Div(left, right) => {
            let divisor = eval_alpha_calc(right, spec, origin_alpha);
            if divisor == 0.0 {
                0.0
            } else {
                eval_alpha_calc(left, spec, origin_alpha) / divisor
            }
        }
        CalcExpr::Negate(value) => -eval_alpha_calc(value, spec, origin_alpha),
    }
}

impl RelativeColor {
    #[must_use]
    pub fn to_css(&self) -> String {
        let function = self.function;
        let units = function.units();
        let channels =
            [0, 1, 2].map(|index| relative_channel_to_css(&self.components[index], &units));
        let alpha = match &self.alpha {
            RelativeAlpha::Inherit => String::new(),
            RelativeAlpha::Channel(channel) => {
                format!(" / {}", relative_channel_to_css(channel, &units))
            }
        };
        match function {
            RelativeFunction::Color(space) => format!(
                "color(from {} {} {} {} {}{alpha})",
                self.origin.to_css(),
                space.name(),
                channels[0],
                channels[1],
                channels[2]
            ),
            _ => format!(
                "{}(from {} {} {} {}{alpha})",
                function_name(function),
                self.origin.to_css(),
                channels[0],
                channels[1],
                channels[2]
            ),
        }
    }
}

const fn function_name(function: RelativeFunction) -> &'static str {
    match function {
        RelativeFunction::Rgb => "rgb",
        RelativeFunction::Hsl => "hsl",
        RelativeFunction::Hwb => "hwb",
        RelativeFunction::Lab => "lab",
        RelativeFunction::Lch => "lch",
        RelativeFunction::Oklab => "oklab",
        RelativeFunction::Oklch => "oklch",
        RelativeFunction::Color(_) => "color",
    }
}

fn relative_channel_to_css(channel: &RelativeChannel, units: &RelativeUnits) -> String {
    match channel {
        RelativeChannel::Missing => "none".to_owned(),
        RelativeChannel::Number(value) => format_number(*value),
        RelativeChannel::Percentage(value) => format!("{}%", format_number(*value * 100.0)),
        RelativeChannel::Keyword(RelativeKeyword::Component(index)) => units
            .names
            .get(usize::from(*index))
            .copied()
            .unwrap_or("none")
            .to_owned(),
        RelativeChannel::Keyword(RelativeKeyword::Alpha) => "alpha".to_owned(),
        RelativeChannel::Calc(expression) => {
            format!("calc({})", calc_to_css(expression, units))
        }
    }
}

fn calc_to_css(expression: &CalcExpr, units: &RelativeUnits) -> String {
    match expression {
        CalcExpr::Number(value) => format_number(*value),
        CalcExpr::Percentage(value) => format!("{}%", format_number(*value * 100.0)),
        CalcExpr::Keyword(RelativeKeyword::Component(index)) => units
            .names
            .get(usize::from(*index))
            .copied()
            .unwrap_or("none")
            .to_owned(),
        CalcExpr::Keyword(RelativeKeyword::Alpha) => "alpha".to_owned(),
        CalcExpr::Add(left, right) => format!(
            "{} + {}",
            calc_to_css(left, units),
            calc_to_css(right, units)
        ),
        CalcExpr::Sub(left, right) => format!(
            "{} - {}",
            calc_to_css(left, units),
            calc_to_css(right, units)
        ),
        CalcExpr::Mul(left, right) => format!(
            "{} * {}",
            calc_to_css(left, units),
            calc_to_css(right, units)
        ),
        CalcExpr::Div(left, right) => format!(
            "{} / {}",
            calc_to_css(left, units),
            calc_to_css(right, units)
        ),
        CalcExpr::Negate(value) => format!("-({})", calc_to_css(value, units)),
    }
}

/// CSS Color 4 §12.4 hue interpolation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HueMethod {
    #[default]
    Shorter,
    Longer,
    Increasing,
    Decreasing,
}

impl HueMethod {
    #[must_use]
    pub fn from_ident(name: &str) -> Option<Self> {
        Some(match name.to_ascii_lowercase().as_str() {
            "shorter" => Self::Shorter,
            "longer" => Self::Longer,
            "increasing" => Self::Increasing,
            "decreasing" => Self::Decreasing,
            _ => return None,
        })
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Shorter => "shorter",
            Self::Longer => "longer",
            Self::Increasing => "increasing",
            Self::Decreasing => "decreasing",
        }
    }
}

/// The parsed, possibly unresolved value of `color-mix()`.
///
/// When both operands are absolute colors the parser resolves the function to
/// an sRGB [`CssColor::Srgb`] immediately. A mix survives only when it names
/// `currentcolor` or a system color, which cannot be resolved until paint.
#[derive(Clone, Debug, PartialEq)]
pub struct ColorMix {
    pub space: ColorSpace,
    pub hue: HueMethod,
    pub left: CssColor,
    pub right: CssColor,
    pub left_percent: Option<f32>,
    pub right_percent: Option<f32>,
    pub left_weight: f32,
    pub right_weight: f32,
    pub alpha_multiplier: f32,
}

impl ColorMix {
    #[must_use]
    pub fn to_css(&self) -> String {
        let hue = if self.space.is_polar() && self.hue != HueMethod::Shorter {
            format!(" {}", self.hue.name())
        } else {
            String::new()
        };
        let operand = |color: &CssColor, percent: Option<f32>| match percent {
            Some(percent) => format!("{} {}%", color.to_css(), format_number(percent)),
            None => color.to_css(),
        };
        format!(
            "color-mix(in {}{hue}, {}, {})",
            self.space.name(),
            operand(&self.left, self.left_percent),
            operand(&self.right, self.right_percent)
        )
    }
}

/// Resolve a color when it contains no context-dependent component.
///
/// `currentcolor`, `canvas` and `canvastext` depend on the element or the
/// system palette and stay unresolved.
#[must_use]
pub fn resolve_absolute(value: &CssColor) -> Option<Rgba> {
    Some(resolve_absolute_space(value)?.to_rgba())
}

/// Like [`resolve_absolute`], but retains missing components (§4.4) so a
/// nested `color-mix()` can carry them into its own interpolation.
#[must_use]
pub fn resolve_absolute_space(value: &CssColor) -> Option<SpaceColor> {
    match value {
        CssColor::Srgb {
            red,
            green,
            blue,
            alpha,
        } => Some(SpaceColor::from_srgb8(*red, *green, *blue, *alpha)),
        CssColor::Space(color) => Some(**color),
        CssColor::Mix(mix) => {
            let left = resolve_absolute_space(&mix.left)?;
            let right = resolve_absolute_space(&mix.right)?;
            Some(mix_space_colors(
                mix.space,
                mix.hue,
                left,
                right,
                mix.left_weight,
                mix.right_weight,
                mix.alpha_multiplier,
            ))
        }
        CssColor::CurrentColor | CssColor::Canvas | CssColor::CanvasText => None,
        CssColor::LightDark(_) => None,
        CssColor::Alpha(alpha) => {
            let origin = resolve_absolute_space(&alpha.origin)?;
            Some(resolve_alpha_space(alpha, origin))
        }
        CssColor::Relative(relative) => {
            let origin = resolve_absolute_space(&relative.origin)?;
            Some(resolve_relative_space(relative, origin))
        }
    }
}

#[must_use]
pub fn rgba_to_css(value: Rgba) -> CssColor {
    let (red, green, blue) = value.to_srgb8();
    CssColor::Srgb {
        red,
        green,
        blue,
        alpha: value.alpha.clamp(0.0, 1.0),
    }
}

/// CSS Color 5 §3: interpolate two absolute colors in `space`.
///
/// The percentage weights are normalized to sum to one. `alpha_multiplier`
/// carries the percentage-sum reduction for a mix whose declared percentages
/// sum below 100%.
#[must_use]
pub fn mix_colors(
    space: ColorSpace,
    hue_method: HueMethod,
    left: Rgba,
    right: Rgba,
    left_weight: f32,
    right_weight: f32,
    alpha_multiplier: f32,
) -> Rgba {
    mix_space_colors(
        space,
        hue_method,
        SpaceColor::from_rgba(left.clamped()),
        SpaceColor::from_rgba(right.clamped()),
        left_weight,
        right_weight,
        alpha_multiplier,
    )
    .to_rgba()
}

/// CSS Color 4 §13: interpolate two colors that may have missing components.
///
/// Carry-forward is decided in the source color spaces, before conversion, by
/// matching analogous components and analogous sets (§13.3). Missing
/// components are then filled from the other color; only components missing
/// on both sides stay missing in the result. Hue interpolation follows
/// §13.5 and premultiplication follows §13.4.
#[must_use]
pub fn mix_space_colors(
    space: ColorSpace,
    hue_method: HueMethod,
    left: SpaceColor,
    right: SpaceColor,
    left_weight: f32,
    right_weight: f32,
    alpha_multiplier: f32,
) -> SpaceColor {
    let left_carried = carried_missing(left.space, space, &left.components);
    let right_carried = carried_missing(right.space, space, &right.components);
    let mut left_components = convert_components(left.space, space, left.components).map(Some);
    let mut right_components = convert_components(right.space, space, right.components).map(Some);
    // A converted color's powerless components become missing (§4.4.1).
    // Authored components in the interpolation space itself stay explicit.
    if left.space != space {
        powerless_to_missing(space, &mut left_components);
    }
    if right.space != space {
        powerless_to_missing(space, &mut right_components);
    }
    for index in 0..3 {
        if left_carried[index] {
            left_components[index] = None;
        }
        if right_carried[index] {
            right_components[index] = None;
        }
    }

    // Fill a component missing on one side with the other color's value; only
    // components missing on both sides stay missing in the result.
    let mut both_missing = [false; 3];
    for index in 0..3 {
        match (left_components[index], right_components[index]) {
            (None, Some(value)) => left_components[index] = Some(value),
            (Some(value), None) => right_components[index] = Some(value),
            (None, None) => both_missing[index] = true,
            (Some(_), Some(_)) => {}
        }
    }
    let mut left_alpha = left.alpha;
    let mut right_alpha = right.alpha;
    let alpha_missing = match (left_alpha, right_alpha) {
        (None, Some(value)) => {
            left_alpha = Some(value);
            false
        }
        (Some(value), None) => {
            right_alpha = Some(value);
            false
        }
        (None, None) => true,
        (Some(_), Some(_)) => false,
    };
    // A missing alpha is not premultiplied (§13.4); it also cannot divide the
    // components, so it acts as an opaque factor for this step.
    let left_alpha = left_alpha.unwrap_or(1.0);
    let right_alpha = right_alpha.unwrap_or(1.0);

    let hue_axis = space.hue_axis();
    if let Some(axis) = hue_axis
        && !both_missing[axis]
    {
        let (left_hue, right_hue) = adjust_hues(
            hue_method,
            left_components[axis].unwrap_or(0.0),
            right_components[axis].unwrap_or(0.0),
        );
        left_components[axis] = Some(left_hue);
        right_components[axis] = Some(right_hue);
    }

    let alpha = left_alpha * left_weight + right_alpha * right_weight;
    let mut mixed = [None; 3];
    for (index, slot) in mixed.iter_mut().enumerate() {
        if both_missing[index] {
            continue;
        }
        let left_value = left_components[index].unwrap_or(0.0);
        let right_value = right_components[index].unwrap_or(0.0);
        *slot = if Some(index) == hue_axis {
            // Hue is not premultiplied; interpolate the adjusted angles.
            Some((left_value * left_weight + right_value * right_weight).rem_euclid(360.0))
        } else {
            let premultiplied =
                left_value * left_alpha * left_weight + right_value * right_alpha * right_weight;
            Some(if alpha > f32::EPSILON {
                premultiplied / alpha
            } else {
                0.0
            })
        };
    }
    let alpha = if alpha_missing {
        None
    } else {
        Some((alpha * alpha_multiplier).clamp(0.0, 1.0))
    };
    SpaceColor::new(space, mixed, alpha)
}

/// The analogous-component categories of CSS Color 4 §13.3.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Analogous {
    Red,
    Green,
    Blue,
    Lightness,
    Colorfulness,
    Hue,
    OpponentA,
    OpponentB,
}

fn components_of(space: ColorSpace) -> [Option<Analogous>; 3] {
    use Analogous::{Blue, Colorfulness, Green, Hue, Lightness, OpponentA, OpponentB, Red};
    match space {
        ColorSpace::Srgb
        | ColorSpace::SrgbLinear
        | ColorSpace::DisplayP3
        | ColorSpace::A98Rgb
        | ColorSpace::ProphotoRgb
        | ColorSpace::Rec2020
        | ColorSpace::Xyz
        | ColorSpace::XyzD50
        | ColorSpace::XyzD65 => [Some(Red), Some(Green), Some(Blue)],
        ColorSpace::Lab | ColorSpace::Oklab => [Some(Lightness), Some(OpponentA), Some(OpponentB)],
        ColorSpace::Lch | ColorSpace::Oklch => [Some(Lightness), Some(Colorfulness), Some(Hue)],
        ColorSpace::Hsl => [Some(Hue), Some(Colorfulness), Some(Lightness)],
        ColorSpace::Hwb => [Some(Hue), None, None],
    }
}

/// The missing components of `source` that carry forward to `target` (§13.3).
///
/// Individual analogous components carry their missing state. The components
/// that remain after removing analogous ones form an analogous set, which
/// carries only when every component in the source set is missing.
fn carried_missing(
    source: ColorSpace,
    target: ColorSpace,
    components: &[Option<f32>; 3],
) -> [bool; 3] {
    let source_components = components_of(source);
    let target_components = components_of(target);
    let mut carried = [false; 3];
    let mut source_matched = [false; 3];
    let mut target_matched = [false; 3];
    for (index, category) in source_components.iter().enumerate() {
        let Some(category) = category else {
            continue;
        };
        for (target_index, target_category) in target_components.iter().enumerate() {
            if target_category == &Some(*category) {
                source_matched[index] = true;
                target_matched[target_index] = true;
                if components[index].is_none() {
                    carried[target_index] = true;
                }
            }
        }
    }
    let remaining_source: Vec<usize> = (0..3).filter(|&index| !source_matched[index]).collect();
    let remaining_target: Vec<usize> = (0..3).filter(|&index| !target_matched[index]).collect();
    if !remaining_source.is_empty()
        && remaining_source
            .iter()
            .all(|&index| components[index].is_none())
    {
        for index in remaining_target {
            carried[index] = true;
        }
    }
    carried
}

fn convert_components(from: ColorSpace, to: ColorSpace, components: [Option<f32>; 3]) -> [f32; 3] {
    let values = components.map(|component| component.unwrap_or(0.0));
    if from == to {
        values
    } else {
        srgb_to_space(space_to_srgb(from, values), to)
    }
}

/// CSS Color 4 §4.4.1: converted powerless components become missing.
///
/// The epsilon values are the ones in §19's sample code: 0.1% saturation for
/// HSL, 0.0015 chroma for LCH, 0.000004 chroma for OKLCH, and a 99.999%
/// white-plus-black sum for HWB.
fn powerless_to_missing(space: ColorSpace, components: &mut [Option<f32>; 3]) {
    match space {
        ColorSpace::Hsl => {
            let saturation = components[1].unwrap_or(0.0);
            if saturation <= 0.001 {
                components[0] = None;
                if saturation > 0.0 {
                    components[1] = Some(0.0);
                }
            }
        }
        ColorSpace::Hwb => {
            let white = components[1].unwrap_or(0.0);
            let black = components[2].unwrap_or(0.0);
            if white + black >= 1.0 - 1e-5 {
                components[0] = None;
                if white + black < 1.0 {
                    match (components[1], components[2]) {
                        (None, Some(_)) => components[2] = Some(1.0),
                        (Some(_), None) => components[1] = Some(1.0),
                        (Some(_), Some(_)) => components[2] = Some(1.0 - white),
                        (None, None) => {}
                    }
                }
            }
        }
        ColorSpace::Lch => {
            let chroma = components[1].unwrap_or(0.0);
            if chroma <= 0.0015 {
                components[2] = None;
                if chroma > 0.0 {
                    components[1] = Some(0.0);
                }
            }
        }
        ColorSpace::Oklch => {
            let chroma = components[1].unwrap_or(0.0);
            if chroma <= 0.000_004 {
                components[2] = None;
                if chroma > 0.0 {
                    components[1] = Some(0.0);
                }
            }
        }
        _ => {}
    }
}

/// CSS Color 4 §12.4: adjust the two hue angles for the requested path.
fn adjust_hues(method: HueMethod, left: f32, right: f32) -> (f32, f32) {
    let mut left = left.rem_euclid(360.0);
    let mut right = right.rem_euclid(360.0);
    let difference = right - left;
    match method {
        HueMethod::Shorter => {
            if difference > 180.0 {
                left += 360.0;
            } else if difference < -180.0 {
                right += 360.0;
            }
        }
        HueMethod::Longer => {
            if 0.0 < difference && difference < 180.0 {
                left += 360.0;
            } else if -180.0 < difference && difference <= 0.0 {
                right += 360.0;
            }
        }
        HueMethod::Increasing => {
            if right < left {
                right += 360.0;
            }
        }
        HueMethod::Decreasing => {
            if left < right {
                left += 360.0;
            }
        }
    }
    (left, right)
}

/// Convert an sRGB color into the coordinates of `space`.
#[must_use]
pub fn srgb_to_space(color: Rgba, space: ColorSpace) -> [f32; 3] {
    let linear = [
        srgb_decode(color.red),
        srgb_decode(color.green),
        srgb_decode(color.blue),
    ];
    let xyz = linear_srgb_to_xyz(linear);
    let xyz_d50 = adapt_d65_to_d50(xyz);
    match space {
        ColorSpace::Srgb => [color.red, color.green, color.blue],
        ColorSpace::SrgbLinear => linear,
        ColorSpace::DisplayP3 => xyz_to_p3(xyz),
        ColorSpace::A98Rgb => xyz_to_a98(xyz),
        ColorSpace::ProphotoRgb => xyz_to_prophoto(xyz_d50),
        ColorSpace::Rec2020 => xyz_to_rec2020(xyz),
        ColorSpace::Xyz | ColorSpace::XyzD65 => xyz,
        ColorSpace::XyzD50 => xyz_d50,
        ColorSpace::Lab => xyz_d50_to_lab(xyz_d50),
        ColorSpace::Oklab => linear_srgb_to_oklab(linear),
        ColorSpace::Lch => lab_to_lch(xyz_d50_to_lab(xyz_d50)),
        ColorSpace::Oklch => oklab_to_oklch(linear_srgb_to_oklab(linear)),
        ColorSpace::Hsl => srgb_to_hsl([color.red, color.green, color.blue]),
        ColorSpace::Hwb => srgb_to_hwb([color.red, color.green, color.blue]),
    }
}

/// Convert coordinates of `space` into sRGB.
#[must_use]
pub fn space_to_srgb(space: ColorSpace, components: [f32; 3]) -> Rgba {
    let linear = match space {
        ColorSpace::Srgb => [components[0], components[1], components[2]],
        ColorSpace::SrgbLinear => components,
        ColorSpace::DisplayP3 => xyz_to_linear_srgb(p3_to_xyz(components)),
        ColorSpace::A98Rgb => xyz_to_linear_srgb(a98_to_xyz(components)),
        ColorSpace::ProphotoRgb => {
            xyz_to_linear_srgb(adapt_d50_to_d65(prophoto_to_xyz(components)))
        }
        ColorSpace::Rec2020 => xyz_to_linear_srgb(rec2020_to_xyz(components)),
        ColorSpace::Xyz | ColorSpace::XyzD65 => xyz_to_linear_srgb(components),
        ColorSpace::XyzD50 => xyz_to_linear_srgb(adapt_d50_to_d65(components)),
        ColorSpace::Lab => xyz_to_linear_srgb(adapt_d50_to_d65(lab_to_xyz_d50(components))),
        ColorSpace::Oklab => oklab_to_linear_srgb(components),
        ColorSpace::Lch => {
            xyz_to_linear_srgb(adapt_d50_to_d65(lab_to_xyz_d50(lch_to_lab(components))))
        }
        ColorSpace::Oklch => oklab_to_linear_srgb(oklch_to_oklab(components)),
        ColorSpace::Hsl => hsx_to_srgb(components),
        ColorSpace::Hwb => hwb_to_srgb(components),
    };
    // `Srgb`, `Hsl` and `Hwb` already produce gamma-encoded components;
    // every other branch above ends in linear-light sRGB.
    let encoded = if matches!(space, ColorSpace::Srgb | ColorSpace::Hsl | ColorSpace::Hwb) {
        linear
    } else {
        [
            srgb_encode(linear[0]),
            srgb_encode(linear[1]),
            srgb_encode(linear[2]),
        ]
    };
    Rgba::new(encoded[0], encoded[1], encoded[2], 1.0)
}

/// CSS Color 4 §14.2.1: one just-noticeable difference in OKLCH is 0.02.
const GAMUT_JND: f32 = 0.02;

/// The binary search terminates once the OKLCH chroma interval is this small
/// (§14.2.2).
const GAMUT_EPSILON: f32 = 0.0001;

fn in_srgb_gamut(color: Rgba) -> bool {
    [color.red, color.green, color.blue]
        .iter()
        .all(|value| (0.0..=1.0).contains(value))
}

/// ΔEOK: Euclidean distance in Oklab (CSS Color 4 §20.3).
fn delta_e_ok(left: [f32; 3], right: [f32; 3]) -> f32 {
    let difference = [left[0] - right[0], left[1] - right[1], left[2] - right[2]];
    (difference[0] * difference[0] + difference[1] * difference[1] + difference[2] * difference[2])
        .sqrt()
}

/// CSS Color 4 §14.2.2, "Binary Search Gamut Mapping with Local MINDE".
///
/// The color is represented in OKLCH; lightness and hue stay fixed while
/// chroma is binary-searched towards the sRGB boundary. At each step the
/// current OKLCH color and its channel-clipped sRGB form are compared with
/// ΔEOK, and a clipped result within one JND is accepted. This is a
/// relative-colorimetric map: colors already inside sRGB are returned
/// unchanged.
fn gamut_map_srgb(color: Rgba) -> Rgba {
    if in_srgb_gamut(color) {
        return color;
    }
    let origin = srgb_to_space(color, ColorSpace::Oklch);
    if origin[0] >= 1.0 {
        return Rgba::new(1.0, 1.0, 1.0, color.alpha);
    }
    if origin[0] <= 0.0 {
        return Rgba::new(0.0, 0.0, 0.0, color.alpha);
    }
    let mut current = origin;
    let mut clipped = space_to_srgb(ColorSpace::Oklch, current).clamped();
    let mut difference = delta_e_ok(
        srgb_to_space(clipped, ColorSpace::Oklab),
        oklch_to_oklab(current),
    );
    if difference < GAMUT_JND {
        return Rgba::new(clipped.red, clipped.green, clipped.blue, color.alpha);
    }
    let mut min = 0.0;
    let mut max = origin[1];
    let mut min_in_gamut = true;
    while max - min > GAMUT_EPSILON {
        let chroma = (min + max) / 2.0;
        current = [origin[0], chroma, origin[2]];
        let srgb = space_to_srgb(ColorSpace::Oklch, current);
        if min_in_gamut && in_srgb_gamut(srgb) {
            min = chroma;
            continue;
        }
        clipped = srgb.clamped();
        difference = delta_e_ok(
            srgb_to_space(clipped, ColorSpace::Oklab),
            oklch_to_oklab(current),
        );
        if difference < GAMUT_JND {
            if GAMUT_JND - difference < GAMUT_EPSILON {
                return Rgba::new(clipped.red, clipped.green, clipped.blue, color.alpha);
            }
            min_in_gamut = false;
            min = chroma;
        } else {
            max = chroma;
        }
    }
    Rgba::new(clipped.red, clipped.green, clipped.blue, color.alpha)
}

fn rounded_channel(value: f32) -> u8 {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the value is rounded and clamped to the complete u8 range"
    )]
    {
        value.round().clamp(0.0, 255.0) as u8
    }
}

fn srgb_decode(value: f32) -> f32 {
    let sign = if value < 0.0 { -1.0 } else { 1.0 };
    let magnitude = value.abs();
    if magnitude <= 0.040_45 {
        value / 12.92
    } else {
        sign * ((magnitude + 0.055) / 1.055).powf(2.4)
    }
}

fn srgb_encode(value: f32) -> f32 {
    let sign = if value < 0.0 { -1.0 } else { 1.0 };
    let magnitude = value.abs();
    if magnitude > 0.003_130_8 {
        sign * (1.055 * magnitude.powf(1.0 / 2.4) - 0.055)
    } else {
        12.92 * value
    }
}

fn multiply(matrix: [[f32; 3]; 3], vector: [f32; 3]) -> [f32; 3] {
    [
        matrix[0][0] * vector[0] + matrix[0][1] * vector[1] + matrix[0][2] * vector[2],
        matrix[1][0] * vector[0] + matrix[1][1] * vector[1] + matrix[1][2] * vector[2],
        matrix[2][0] * vector[0] + matrix[2][1] * vector[1] + matrix[2][2] * vector[2],
    ]
}

const LIN_SRGB_TO_XYZ: [[f32; 3]; 3] = [
    [
        506_752.0 / 1_228_815.0,
        87_881.0 / 245_763.0,
        12_673.0 / 70_218.0,
    ],
    [
        87_098.0 / 409_605.0,
        175_762.0 / 245_763.0,
        12_673.0 / 175_545.0,
    ],
    [
        7_918.0 / 409_605.0,
        87_881.0 / 737_289.0,
        1_001_167.0 / 1_053_270.0,
    ],
];

const XYZ_TO_LIN_SRGB: [[f32; 3]; 3] = [
    [12_831.0 / 3_959.0, -329.0 / 214.0, -1_974.0 / 3_959.0],
    [
        -851_781.0 / 878_810.0,
        1_648_619.0 / 878_810.0,
        36_519.0 / 878_810.0,
    ],
    [705.0 / 12_673.0, -2_585.0 / 12_673.0, 705.0 / 667.0],
];

const LIN_P3_TO_XYZ: [[f32; 3]; 3] = [
    [
        608_311.0 / 1_250_200.0,
        189_793.0 / 714_400.0,
        198_249.0 / 1_000_160.0,
    ],
    [
        35_783.0 / 156_275.0,
        247_089.0 / 357_200.0,
        198_249.0 / 2_500_400.0,
    ],
    [0.0, 32_229.0 / 714_400.0, 5_220_557.0 / 5_000_800.0],
];

const XYZ_TO_LIN_P3: [[f32; 3]; 3] = [
    [
        446_124.0 / 178_915.0,
        -333_277.0 / 357_830.0,
        -72_051.0 / 178_915.0,
    ],
    [-14_852.0 / 17_905.0, 63_121.0 / 35_810.0, 423.0 / 17_905.0],
    [
        11_844.0 / 330_415.0,
        -50_337.0 / 660_830.0,
        316_169.0 / 330_415.0,
    ],
];

const LIN_A98_TO_XYZ: [[f32; 3]; 3] = [
    [
        573_536.0 / 994_567.0,
        263_643.0 / 1_420_810.0,
        187_206.0 / 994_567.0,
    ],
    [
        591_459.0 / 1_989_134.0,
        6_239_551.0 / 9_945_670.0,
        374_145.0 / 4_972_835.0,
    ],
    [
        53_769.0 / 1_989_134.0,
        351_524.0 / 4_972_835.0,
        4_929_758.0 / 4_972_835.0,
    ],
];

const XYZ_TO_LIN_A98: [[f32; 3]; 3] = [
    [
        1_829_569.0 / 896_150.0,
        -506_331.0 / 896_150.0,
        -308_931.0 / 896_150.0,
    ],
    [
        -851_781.0 / 878_810.0,
        1_648_619.0 / 878_810.0,
        36_519.0 / 878_810.0,
    ],
    [
        16_779.0 / 1_248_040.0,
        -147_721.0 / 1_248_040.0,
        1_266_979.0 / 1_248_040.0,
    ],
];

const LIN_PROPHOTO_TO_XYZ_D50: [[f32; 3]; 3] = [
    [0.797_766_6, 0.135_181_3, 0.031_347_734],
    [0.288_074_83, 0.711_835_2, 0.000_089_936_94],
    [0.0, 0.0, 0.825_104_6],
];

const XYZ_D50_TO_LIN_PROPHOTO: [[f32; 3]; 3] = [
    [1.345_786_9, -0.255_572_1, -0.051_101_865],
    [-0.544_630_7, 1.508_247_7, 0.020_527_447],
    [0.0, 0.0, 1.211_967_5],
];

const LIN_2020_TO_XYZ: [[f32; 3]; 3] = [
    [
        63_426_534.0 / 99_577_255.0,
        20_160_776.0 / 139_408_157.0,
        47_086_771.0 / 278_816_314.0,
    ],
    [
        26_158_966.0 / 99_577_255.0,
        472_592_308.0 / 697_040_785.0,
        8_267_143.0 / 139_408_157.0,
    ],
    [
        0.0,
        19_567_812.0 / 697_040_785.0,
        295_819_943.0 / 278_816_314.0,
    ],
];

const XYZ_TO_LIN_2020: [[f32; 3]; 3] = [
    [
        30_757_411.0 / 17_917_100.0,
        -6_372_589.0 / 17_917_100.0,
        -4_539_589.0 / 17_917_100.0,
    ],
    [
        -19_765_991.0 / 29_648_200.0,
        47_925_759.0 / 29_648_200.0,
        467_509.0 / 29_648_200.0,
    ],
    [
        792_561.0 / 44_930_125.0,
        -1_921_689.0 / 44_930_125.0,
        42_328_811.0 / 44_930_125.0,
    ],
];

const D65_TO_D50: [[f32; 3]; 3] = [
    [1.047_929_8, 0.022_946_794, -0.050_192_23],
    [0.029_627_816, 0.990_434_5, -0.017_073_825],
    [-0.009_243_058, 0.015_055_145, 0.751_874_3],
];

const D50_TO_D65: [[f32; 3]; 3] = [
    [0.955_473_4, -0.023_098_537, 0.063_259_31],
    [-0.028_369_707, 1.009_995_5, 0.021_041_4],
    [0.012_314_002, -0.020_507_696, 1.330_365_9],
];

const LIN_SRGB_TO_LMS: [[f32; 3]; 3] = [
    [0.412_221_47, 0.536_332_55, 0.051_445_995],
    [0.211_903_5, 0.680_699_5, 0.107_396_96],
    [0.088_302_46, 0.281_718_85, 0.629_978_7],
];

const LMS_TO_OKLAB: [[f32; 3]; 3] = [
    [0.210_454_26, 0.793_617_8, -0.004_072_047],
    [1.977_998_5, -2.428_592_2, 0.450_593_7],
    [0.025_904_037, 0.782_771_77, -0.808_675_77],
];

const OKLAB_TO_LMS: [[f32; 3]; 3] = [
    [1.0, 0.396_337_78, 0.215_803_76],
    [1.0, -0.105_561_346, -0.063_854_17],
    [1.0, -0.089_484_18, -1.291_485_5],
];

const LMS_TO_LIN_SRGB: [[f32; 3]; 3] = [
    [4.076_741_7, -3.307_711_6, 0.230_969_94],
    [-1.268_438, 2.609_757_4, -0.341_319_38],
    [-0.004_196_086_3, -0.703_418_6, 1.707_614_7],
];

fn linear_srgb_to_xyz(linear: [f32; 3]) -> [f32; 3] {
    multiply(LIN_SRGB_TO_XYZ, linear)
}

fn xyz_to_linear_srgb(xyz: [f32; 3]) -> [f32; 3] {
    multiply(XYZ_TO_LIN_SRGB, xyz)
}

fn adapt_d65_to_d50(xyz: [f32; 3]) -> [f32; 3] {
    multiply(D65_TO_D50, xyz)
}

fn adapt_d50_to_d65(xyz: [f32; 3]) -> [f32; 3] {
    multiply(D50_TO_D65, xyz)
}

fn p3_to_xyz(components: [f32; 3]) -> [f32; 3] {
    let linear = components.map(srgb_decode);
    multiply(LIN_P3_TO_XYZ, linear)
}

fn xyz_to_p3(xyz: [f32; 3]) -> [f32; 3] {
    multiply(XYZ_TO_LIN_P3, xyz).map(srgb_encode)
}

fn a98_to_xyz(components: [f32; 3]) -> [f32; 3] {
    let linear = components.map(a98_decode);
    multiply(LIN_A98_TO_XYZ, linear)
}

fn xyz_to_a98(xyz: [f32; 3]) -> [f32; 3] {
    multiply(XYZ_TO_LIN_A98, xyz).map(a98_encode)
}

fn prophoto_to_xyz(components: [f32; 3]) -> [f32; 3] {
    let linear = components.map(prophoto_decode);
    multiply(LIN_PROPHOTO_TO_XYZ_D50, linear)
}

fn xyz_to_prophoto(xyz_d50: [f32; 3]) -> [f32; 3] {
    multiply(XYZ_D50_TO_LIN_PROPHOTO, xyz_d50).map(prophoto_encode)
}

fn rec2020_to_xyz(components: [f32; 3]) -> [f32; 3] {
    let linear = components.map(rec2020_decode);
    multiply(LIN_2020_TO_XYZ, linear)
}

fn xyz_to_rec2020(xyz: [f32; 3]) -> [f32; 3] {
    multiply(XYZ_TO_LIN_2020, xyz).map(rec2020_encode)
}

fn a98_decode(value: f32) -> f32 {
    let sign = if value < 0.0 { -1.0 } else { 1.0 };
    sign * value.abs().powf(563.0 / 256.0)
}

fn a98_encode(value: f32) -> f32 {
    let sign = if value < 0.0 { -1.0 } else { 1.0 };
    sign * value.abs().powf(256.0 / 563.0)
}

fn prophoto_decode(value: f32) -> f32 {
    let sign = if value < 0.0 { -1.0 } else { 1.0 };
    let magnitude = value.abs();
    if magnitude <= 16.0 / 512.0 {
        value / 16.0
    } else {
        sign * magnitude.powf(1.8)
    }
}

fn prophoto_encode(value: f32) -> f32 {
    let sign = if value < 0.0 { -1.0 } else { 1.0 };
    let magnitude = value.abs();
    if magnitude >= 1.0 / 512.0 {
        sign * magnitude.powf(1.0 / 1.8)
    } else {
        16.0 * value
    }
}

fn rec2020_decode(value: f32) -> f32 {
    const ALPHA: f32 = 1.099_296_8;
    const BETA: f32 = 0.018_053_97;
    let sign = if value < 0.0 { -1.0 } else { 1.0 };
    let magnitude = value.abs();
    if magnitude < BETA * 4.5 {
        value / 4.5
    } else {
        sign * ((magnitude + ALPHA - 1.0) / ALPHA).powf(1.0 / 0.45)
    }
}

fn rec2020_encode(value: f32) -> f32 {
    const ALPHA: f32 = 1.099_296_8;
    const BETA: f32 = 0.018_053_97;
    let sign = if value < 0.0 { -1.0 } else { 1.0 };
    let magnitude = value.abs();
    if magnitude > BETA {
        sign * (ALPHA * magnitude.powf(0.45) - (ALPHA - 1.0))
    } else {
        4.5 * value
    }
}

const D50_WHITE: [f32; 3] = [0.964_295_7, 1.0, 0.825_104_6];
const LAB_EPSILON: f32 = 216.0 / 24_389.0;
const LAB_KAPPA: f32 = 24_389.0 / 27.0;

fn lab_f(value: f32) -> f32 {
    if value > LAB_EPSILON {
        value.cbrt()
    } else {
        (LAB_KAPPA * value + 16.0) / 116.0
    }
}

fn lab_f_inverse(value: f32) -> f32 {
    let cube = value * value * value;
    if cube > LAB_EPSILON {
        cube
    } else {
        (116.0 * value - 16.0) / LAB_KAPPA
    }
}

fn xyz_d50_to_lab(xyz: [f32; 3]) -> [f32; 3] {
    let fx = lab_f(xyz[0] / D50_WHITE[0]);
    let fy = lab_f(xyz[1] / D50_WHITE[1]);
    let fz = lab_f(xyz[2] / D50_WHITE[2]);
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

fn lab_to_xyz_d50(lab: [f32; 3]) -> [f32; 3] {
    let fy = (lab[0] + 16.0) / 116.0;
    let fx = fy + lab[1] / 500.0;
    let fz = fy - lab[2] / 200.0;
    [
        D50_WHITE[0] * lab_f_inverse(fx),
        D50_WHITE[1] * lab_f_inverse(fy),
        D50_WHITE[2] * lab_f_inverse(fz),
    ]
}

fn linear_srgb_to_oklab(linear: [f32; 3]) -> [f32; 3] {
    let lms = multiply(LIN_SRGB_TO_LMS, linear).map(f32::cbrt);
    multiply(LMS_TO_OKLAB, lms)
}

fn oklab_to_linear_srgb(lab: [f32; 3]) -> [f32; 3] {
    let lms = multiply(OKLAB_TO_LMS, lab).map(|value| value * value * value);
    multiply(LMS_TO_LIN_SRGB, lms)
}

fn lab_to_lch(lab: [f32; 3]) -> [f32; 3] {
    let chroma = (lab[1] * lab[1] + lab[2] * lab[2]).sqrt();
    let hue = if chroma <= 1e-6 {
        0.0
    } else {
        lab[2].atan2(lab[1]).to_degrees().rem_euclid(360.0)
    };
    [lab[0], chroma, hue]
}

fn lch_to_lab(lch: [f32; 3]) -> [f32; 3] {
    let radians = lch[2].to_radians();
    [lch[0], lch[1] * radians.cos(), lch[1] * radians.sin()]
}

fn oklab_to_oklch(lab: [f32; 3]) -> [f32; 3] {
    lab_to_lch(lab)
}

fn oklch_to_oklab(lch: [f32; 3]) -> [f32; 3] {
    lch_to_lab(lch)
}

fn srgb_to_hsl(rgb: [f32; 3]) -> [f32; 3] {
    // CSS Color 4 §19's sample code does not clamp here: conversion to an
    // interpolation space must preserve out-of-gamut values (§13.2).
    let red = rgb[0];
    let green = rgb[1];
    let blue = rgb[2];
    let maximum = red.max(green).max(blue);
    let minimum = red.min(green).min(blue);
    let lightness = (maximum + minimum) / 2.0;
    let delta = maximum - minimum;
    if delta <= 1e-6 {
        return [0.0, 0.0, lightness];
    }
    let saturation = delta / (1.0 - (2.0 * lightness - 1.0).abs());
    let hue = if maximum == red {
        ((green - blue) / delta).rem_euclid(6.0)
    } else if maximum == green {
        (blue - red) / delta + 2.0
    } else {
        (red - green) / delta + 4.0
    } * 60.0;
    [hue.rem_euclid(360.0), saturation, lightness]
}

fn hsx_to_srgb(hsl: [f32; 3]) -> [f32; 3] {
    let hue = hsl[0];
    let saturation = hsl[1];
    let lightness = hsl[2];
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let sector = (hue.rem_euclid(360.0)) / 60.0;
    let second = chroma * (1.0 - ((sector % 2.0) - 1.0).abs());
    let (red, green, blue) = if sector < 1.0 {
        (chroma, second, 0.0)
    } else if sector < 2.0 {
        (second, chroma, 0.0)
    } else if sector < 3.0 {
        (0.0, chroma, second)
    } else if sector < 4.0 {
        (0.0, second, chroma)
    } else if sector < 5.0 {
        (second, 0.0, chroma)
    } else {
        (chroma, 0.0, second)
    };
    let offset = lightness - chroma / 2.0;
    [red + offset, green + offset, blue + offset]
}

fn srgb_to_hwb(rgb: [f32; 3]) -> [f32; 3] {
    let hue = srgb_to_hsl(rgb)[0];
    let white = rgb[0].min(rgb[1]).min(rgb[2]);
    let black = 1.0 - rgb[0].max(rgb[1]).max(rgb[2]);
    [hue, white, black]
}

fn hwb_to_srgb(hwb: [f32; 3]) -> [f32; 3] {
    let hue = hwb[0];
    let white = hwb[1];
    let black = hwb[2];
    let sum = white + black;
    if sum >= 1.0 {
        let gray = white / sum;
        return [gray, gray, gray];
    }
    let pure = hsx_to_srgb([hue, 1.0, 0.5]);
    let factor = 1.0 - white - black;
    [
        pure[0] * factor + white,
        pure[1] * factor + white,
        pure[2] * factor + white,
    ]
}

#[cfg(test)]
mod tests {
    #![allow(clippy::float_cmp)]

    use super::{
        ColorSpace, HueMethod, Rgba, SpaceColor, mix_colors, mix_space_colors, space_to_srgb,
        srgb_to_space,
    };

    fn assert_rgb(color: Rgba, expected: (u8, u8, u8)) {
        assert_eq!(color.to_srgb8(), expected, "{color:?}");
    }

    #[test]
    fn srgb_round_trips_through_every_space() {
        for space in [
            ColorSpace::Srgb,
            ColorSpace::SrgbLinear,
            ColorSpace::DisplayP3,
            ColorSpace::A98Rgb,
            ColorSpace::ProphotoRgb,
            ColorSpace::Rec2020,
            ColorSpace::Xyz,
            ColorSpace::XyzD50,
            ColorSpace::Lab,
            ColorSpace::Oklab,
            ColorSpace::Lch,
            ColorSpace::Oklch,
            ColorSpace::Hsl,
            ColorSpace::Hwb,
        ] {
            let original = Rgba::from_srgb8(40, 120, 200, 1.0);
            let round_trip = space_to_srgb(space, srgb_to_space(original, space));
            assert_eq!(
                round_trip.to_srgb8(),
                (40, 120, 200),
                "{space:?}: {round_trip:?}"
            );
            let gray = Rgba::from_srgb8(128, 128, 128, 1.0);
            let gray_round_trip = space_to_srgb(space, srgb_to_space(gray, space));
            assert_eq!(
                gray_round_trip.to_srgb8(),
                (128, 128, 128),
                "{space:?} gray: {gray_round_trip:?}"
            );
        }
    }

    #[test]
    fn mixing_srgb_interpolates_gamma_encoded_components() {
        let red = Rgba::from_srgb8(255, 0, 0, 1.0);
        let blue = Rgba::from_srgb8(0, 0, 255, 1.0);
        let mixed = mix_colors(
            ColorSpace::Srgb,
            HueMethod::Shorter,
            red,
            blue,
            0.5,
            0.5,
            1.0,
        );
        assert_rgb(mixed, (128, 0, 128));
        assert_eq!(mixed.alpha, 1.0);
    }

    #[test]
    fn mixing_transparent_keeps_the_opaque_hue() {
        let red = Rgba::from_srgb8(255, 0, 0, 1.0);
        let transparent = Rgba::TRANSPARENT;
        let mixed = mix_colors(
            ColorSpace::Srgb,
            HueMethod::Shorter,
            red,
            transparent,
            0.5,
            0.5,
            1.0,
        );
        assert_rgb(mixed, (255, 0, 0));
        assert_eq!(mixed.alpha, 0.5);
    }

    #[test]
    fn percentage_sum_below_one_hundred_multiplies_alpha() {
        let red = Rgba::from_srgb8(255, 0, 0, 1.0);
        let blue = Rgba::from_srgb8(0, 0, 255, 1.0);
        let mixed = mix_colors(
            ColorSpace::Srgb,
            HueMethod::Shorter,
            red,
            blue,
            0.5,
            0.5,
            0.5,
        );
        assert_rgb(mixed, (128, 0, 128));
        assert_eq!(mixed.alpha, 0.5);
    }

    #[test]
    fn hsl_hue_paths_follow_the_requested_direction() {
        let red = Rgba::from_srgb8(255, 0, 0, 1.0);
        let blue = Rgba::from_srgb8(0, 0, 255, 1.0);
        // Red is hue 0, blue is hue 240. Shorter goes backwards through
        // magenta (midpoint 300), longer through green/cyan (midpoint 120).
        let shorter = mix_colors(
            ColorSpace::Hsl,
            HueMethod::Shorter,
            red,
            blue,
            0.5,
            0.5,
            1.0,
        );
        assert_rgb(shorter, (255, 0, 255));
        let longer = mix_colors(ColorSpace::Hsl, HueMethod::Longer, red, blue, 0.5, 0.5, 1.0);
        assert_rgb(longer, (0, 255, 0));
        let increasing = mix_colors(
            ColorSpace::Hsl,
            HueMethod::Increasing,
            red,
            blue,
            0.5,
            0.5,
            1.0,
        );
        assert_rgb(increasing, (0, 255, 0));
        let decreasing = mix_colors(
            ColorSpace::Hsl,
            HueMethod::Decreasing,
            red,
            blue,
            0.5,
            0.5,
            1.0,
        );
        assert_rgb(decreasing, (255, 0, 255));
    }

    /// CSS Color 4 §13.3: a missing component is replaced with the other
    /// color's corresponding component before interpolation, so a missing
    /// OKLCH lightness takes the other lightness instead of acting as zero.
    #[test]
    fn missing_component_carries_forward_before_interpolation() {
        let left = SpaceColor::new(ColorSpace::Oklch, [None, Some(0.25), Some(30.0)], Some(1.0));
        let right = SpaceColor::new(
            ColorSpace::Oklch,
            [Some(0.8), Some(0.125), Some(60.0)],
            Some(1.0),
        );
        let mixed = mix_space_colors(
            ColorSpace::Oklch,
            HueMethod::Shorter,
            left,
            right,
            0.5,
            0.5,
            1.0,
        );
        assert_eq!(mixed.space, ColorSpace::Oklch);
        // L is carried from the right color (0.8), not averaged with 0.
        assert_eq!(mixed.components[0], Some(0.8));
        // C and H interpolate normally: (0.25 + 0.125) / 2 and (30 + 60) / 2.
        assert_eq!(mixed.components[1], Some(0.1875));
        assert_eq!(mixed.components[2], Some(45.0));
        assert_eq!(mixed.alpha, Some(1.0));
        assert_eq!(mixed.to_css(), "oklch(0.8 0.1875 45)");
    }

    /// When both operands miss a component, the result misses it too; a
    /// component missing on only one side is filled from the other side.
    #[test]
    fn components_missing_on_both_sides_stay_missing() {
        let left = SpaceColor::new(ColorSpace::Srgb, [None, Some(0.5), Some(0.0)], Some(1.0));
        let right = SpaceColor::new(ColorSpace::Srgb, [None, Some(0.0), Some(1.0)], Some(1.0));
        let mixed = mix_space_colors(
            ColorSpace::Srgb,
            HueMethod::Shorter,
            left,
            right,
            0.5,
            0.5,
            1.0,
        );
        assert_eq!(mixed.components[0], None);
        assert_eq!(mixed.components[1], Some(0.25));
        assert_eq!(mixed.components[2], Some(0.5));
        assert!(mixed.to_css().starts_with("color(srgb none 0.25 0.5"));
    }

    /// CSS Color 4 §13.3 analogous set: both CIE Lab opponents `a` and `b`
    /// missing carry to both LCH chroma and hue, rather than becoming zero.
    #[test]
    fn analogous_set_carries_lab_opponents_to_lch_chroma_and_hue() {
        let left = SpaceColor::new(ColorSpace::Lab, [Some(50.0), None, None], Some(1.0));
        let right = SpaceColor::new(
            ColorSpace::Lch,
            [Some(50.0), Some(40.0), Some(120.0)],
            Some(1.0),
        );
        let mixed = mix_space_colors(
            ColorSpace::Lch,
            HueMethod::Shorter,
            left,
            right,
            0.5,
            0.5,
            1.0,
        );
        // The carried-forward missing chroma and hue are filled from the
        // right color, so both operands land on lch(50 40 120).
        assert!((mixed.components[0].unwrap() - 50.0).abs() < 1e-3);
        assert!((mixed.components[1].unwrap() - 40.0).abs() < 1e-3);
        assert!((mixed.components[2].unwrap() - 120.0).abs() < 1e-3);
    }

    /// A missing alpha is carried forward before premultiplication; it must
    /// not be treated as fully transparent.
    #[test]
    fn missing_alpha_is_carried_forward() {
        let left = SpaceColor::new(ColorSpace::Oklch, [Some(0.5), Some(0.1), Some(30.0)], None);
        let right = SpaceColor::new(
            ColorSpace::Oklch,
            [Some(0.8), Some(0.1), Some(60.0)],
            Some(0.5),
        );
        let mixed = mix_space_colors(
            ColorSpace::Oklch,
            HueMethod::Shorter,
            left,
            right,
            0.5,
            0.5,
            1.0,
        );
        assert_eq!(mixed.alpha, Some(0.5));
        assert!((mixed.components[0].unwrap() - 0.65).abs() < 1e-6);
        assert!((mixed.components[1].unwrap() - 0.1).abs() < 1e-6);
        assert!((mixed.components[2].unwrap() - 45.0).abs() < 1e-4);
    }

    /// CSS Color 4 §14.2.2: out-of-gamut colors reduce OKLCH chroma and keep
    /// lightness/hue, which differs from per-channel clipping for the display
    /// P3 primaries.
    #[test]
    fn gamut_mapping_reduces_oklch_chroma_instead_of_clipping() {
        let p3_red = space_to_srgb(ColorSpace::DisplayP3, [1.0, 0.0, 0.0]);
        assert_eq!(p3_red.to_srgb8(), (255, 11, 12));
        // Channel clipping would give (0, 255, 0); perceptual mapping keeps
        // the sRGB green primary in gamut but lowers the blue spill.
        let p3_green = space_to_srgb(ColorSpace::DisplayP3, [0.0, 1.0, 0.0]);
        assert_eq!(p3_green.to_srgb8(), (0, 251, 41));
        let p3_blue = space_to_srgb(ColorSpace::DisplayP3, [0.0, 0.0, 1.0]);
        assert_eq!(p3_blue.to_srgb8(), (0, 0, 255));
        let p3_yellow = space_to_srgb(ColorSpace::DisplayP3, [1.0, 1.0, 0.0]);
        assert_eq!(p3_yellow.to_srgb8(), (254, 255, 0));
        // A strongly out-of-gamut OKLCH color is mapped through the binary
        // search: chroma falls from 0.4 while L=0.7 and H=150 stay fixed.
        let oklch = space_to_srgb(ColorSpace::Oklch, [0.7, 0.4, 150.0]);
        assert_eq!(oklch.to_srgb8(), (0, 194, 72));
        // Colors already inside sRGB pass through the relative-colorimetric
        // map unchanged.
        let in_gamut = Rgba::from_srgb8(40, 120, 200, 1.0);
        assert_eq!(in_gamut.gamut_mapped(), in_gamut);
    }

    /// Out-of-gamut values are not clipped when converting into an
    /// interpolation space. This pins the §3.4 CSS Color 5 example:
    /// `color-mix(in hsl, color(display-p3 0 1 0) 80%, yellow)` produces
    /// hsl(114.3032 261.5568% 30.2672%) and color(srgb -0.3387 1.0943 -0.48899).
    #[test]
    fn out_of_gamut_operands_keep_their_hsl_coordinates() {
        let left = SpaceColor::new(
            ColorSpace::DisplayP3,
            [Some(0.0), Some(1.0), Some(0.0)],
            Some(1.0),
        );
        let right = SpaceColor::from_srgb8(255, 255, 0, 1.0);
        let mixed = mix_space_colors(
            ColorSpace::Hsl,
            HueMethod::Shorter,
            left,
            right,
            0.8,
            0.2,
            1.0,
        );
        assert_eq!(mixed.space, ColorSpace::Hsl);
        assert!((mixed.components[0].unwrap() - 114.3032).abs() < 1e-2);
        assert!((mixed.components[1].unwrap() - 2.615_568).abs() < 1e-3);
        assert!((mixed.components[2].unwrap() - 0.302_672).abs() < 1e-3);
        let srgb = space_to_srgb(
            ColorSpace::Hsl,
            mixed.components.map(|component| component.unwrap_or(0.0)),
        );
        assert!((srgb.red + 0.3387).abs() < 1e-2);
        assert!((srgb.green - 1.0943).abs() < 1e-2);
        assert!((srgb.blue + 0.488_99).abs() < 1e-2);
    }
}
