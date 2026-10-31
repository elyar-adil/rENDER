//! CSS Color 4 color spaces and CSS Color 5 `color-mix()` interpolation.
//!
//! The paint contract is sRGB, so every color space here exists to convert
//! into and out of sRGB. The conversion matrices and transfer functions are
//! the sample code in CSS Color 4 §19, evaluated in `f32` because the engine's
//! device colors are eight-bit sRGB.

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

    /// The eight-bit sRGB color a device boundary accepts.
    #[must_use]
    pub fn clamped(self) -> Self {
        Self {
            red: self.red.clamp(0.0, 1.0),
            green: self.green.clamp(0.0, 1.0),
            blue: self.blue.clamp(0.0, 1.0),
            alpha: self.alpha.clamp(0.0, 1.0),
        }
    }

    #[must_use]
    pub fn to_srgb8(self) -> (u8, u8, u8) {
        let value = self.clamped();
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
    match value {
        CssColor::Srgb {
            red,
            green,
            blue,
            alpha,
        } => Some(Rgba::from_srgb8(*red, *green, *blue, *alpha)),
        CssColor::Mix(mix) => {
            let left = resolve_absolute(&mix.left)?;
            let right = resolve_absolute(&mix.right)?;
            Some(mix_colors(
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
    let left = left.clamped();
    let right = right.clamped();
    let mut left_components = srgb_to_space(left, space);
    let mut right_components = srgb_to_space(right, space);
    let hue_axis = space.hue_axis();
    if let Some(axis) = hue_axis {
        let left_powerless = is_powerless(space, left_components);
        let right_powerless = is_powerless(space, right_components);
        if left_powerless && right_powerless {
            left_components[axis] = 0.0;
            right_components[axis] = 0.0;
        } else if left_powerless {
            left_components[axis] = right_components[axis];
        } else if right_powerless {
            right_components[axis] = left_components[axis];
        }
        let (left_hue, right_hue) =
            adjust_hues(hue_method, left_components[axis], right_components[axis]);
        left_components[axis] = left_hue;
        right_components[axis] = right_hue;
    }
    let alpha = left.alpha * left_weight + right.alpha * right_weight;
    let mut mixed = [0.0; 3];
    for (index, slot) in mixed.iter_mut().enumerate() {
        if Some(index) == hue_axis {
            // Hue is not premultiplied; interpolate the adjusted angles.
            *slot = left_components[index] * left_weight + right_components[index] * right_weight;
            *slot = slot.rem_euclid(360.0);
        } else {
            let premultiplied = left_components[index] * left.alpha * left_weight
                + right_components[index] * right.alpha * right_weight;
            *slot = if alpha > f32::EPSILON {
                premultiplied / alpha
            } else {
                0.0
            };
        }
    }
    let mut result = space_to_srgb(space, mixed);
    result.alpha = (alpha * alpha_multiplier).clamp(0.0, 1.0);
    result
}

fn is_powerless(space: ColorSpace, components: [f32; 3]) -> bool {
    match space {
        ColorSpace::Hsl => components[1] <= 1e-6,
        ColorSpace::Hwb => components[1] + components[2] >= 1.0 - 1e-6,
        ColorSpace::Lch | ColorSpace::Oklch => components[1] <= 1e-6,
        _ => false,
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
    let red = rgb[0].clamp(0.0, 1.0);
    let green = rgb[1].clamp(0.0, 1.0);
    let blue = rgb[2].clamp(0.0, 1.0);
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
    [hue.rem_euclid(360.0), saturation.clamp(0.0, 1.0), lightness]
}

fn hsx_to_srgb(hsl: [f32; 3]) -> [f32; 3] {
    let hue = hsl[0];
    let saturation = hsl[1].clamp(0.0, 1.0);
    let lightness = hsl[2].clamp(0.0, 1.0);
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
    [
        (red + offset).clamp(0.0, 1.0),
        (green + offset).clamp(0.0, 1.0),
        (blue + offset).clamp(0.0, 1.0),
    ]
}

fn srgb_to_hwb(rgb: [f32; 3]) -> [f32; 3] {
    let hue = srgb_to_hsl(rgb)[0];
    let red = rgb[0].clamp(0.0, 1.0);
    let green = rgb[1].clamp(0.0, 1.0);
    let blue = rgb[2].clamp(0.0, 1.0);
    let white = red.min(green).min(blue);
    let black = 1.0 - red.max(green).max(blue);
    [hue, white, black]
}

fn hwb_to_srgb(hwb: [f32; 3]) -> [f32; 3] {
    let hue = hwb[0];
    let mut white = hwb[1].clamp(0.0, 1.0);
    let mut black = hwb[2].clamp(0.0, 1.0);
    let sum = white + black;
    if sum > 1.0 {
        white /= sum;
        black /= sum;
    }
    if sum >= 1.0 {
        let gray = white / sum;
        return [gray, gray, gray];
    }
    let pure = hsx_to_srgb([hue, 1.0, 0.5]);
    let factor = 1.0 - white - black;
    [
        (pure[0] * factor + white).clamp(0.0, 1.0),
        (pure[1] * factor + white).clamp(0.0, 1.0),
        (pure[2] * factor + white).clamp(0.0, 1.0),
    ]
}

#[cfg(test)]
mod tests {
    #![allow(clippy::float_cmp)]

    use super::{ColorSpace, HueMethod, Rgba, mix_colors, space_to_srgb, srgb_to_space};

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
}
