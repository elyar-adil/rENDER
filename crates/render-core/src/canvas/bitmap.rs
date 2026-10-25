//! The pixel store of a canvas: premultiplied RGBA8, and the per-pixel
//! operations the 2D context needs on it.
//!
//! The canvas bitmap is stored premultiplied, as the specification's bitmap
//! is, so that source-over compositing and clearing are exact per channel.
//! Script reads and writes (`getImageData`, `putImageData`) convert to and
//! from straight alpha with rounding.

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

use crate::paint::coverage::Coverage;

/// A `width` by `height` canvas bitmap, row-major, premultiplied RGBA8.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bitmap {
    width: u32,
    height: u32,
    pixels: Vec<[u8; 4]>,
}

/// A clip region: one byte of coverage per canvas pixel, row-major.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipMask {
    width: u32,
    height: u32,
    values: Vec<u8>,
}

/// A solid source colour for a fill: straight RGB and an effective alpha in
/// `0.0..=1.0` that already includes `globalAlpha`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Source {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub alpha: f32,
}

impl Bitmap {
    /// A transparent black bitmap.
    #[must_use]
    pub fn new(width: u32, height: u32) -> Self {
        let count = pixel_count(width, height);
        Self {
            width,
            height,
            pixels: vec![[0; 4]; count],
        }
    }

    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Source-over composites `source` through `coverage` and `clip`.
    pub fn fill(&mut self, coverage: &Coverage, clip: Option<&ClipMask>, source: Source) {
        let (x0, y0) = coverage.origin();
        let (width, height) = coverage.size();
        for y in y0..y0 + height {
            for x in x0..x0 + width {
                let amount = coverage.at(x, y) * clip.map_or(1.0, |mask| mask.at(x, y));
                if amount <= 0.0 {
                    continue;
                }
                let alpha = (source.alpha * amount).clamp(0.0, 1.0);
                if alpha <= 0.0 {
                    continue;
                }
                let index = self.index(x, y);
                let destination = self.pixels[index];
                self.pixels[index] = composite_over(destination, source, alpha);
            }
        }
    }

    /// Fills the pixels of `coverage` that `clip` admits, sampling the source
    /// once per covered pixel at its centre. A gradient is the reason this
    /// exists: its colour varies over the bitmap, so one `Source` cannot
    /// describe the fill.
    pub fn fill_where(
        &mut self,
        coverage: &Coverage,
        clip: Option<&ClipMask>,
        sample: impl Fn(u32, u32) -> Source,
    ) {
        let (x0, y0) = coverage.origin();
        let (width, height) = coverage.size();
        for y in y0..y0 + height {
            for x in x0..x0 + width {
                let amount = coverage.at(x, y) * clip.map_or(1.0, |mask| mask.at(x, y));
                if amount <= 0.0 {
                    continue;
                }
                let source = sample(x, y);
                let alpha = (source.alpha * amount).clamp(0.0, 1.0);
                if alpha <= 0.0 {
                    continue;
                }
                let index = self.index(x, y);
                let destination = self.pixels[index];
                self.pixels[index] = composite_over(destination, source, alpha);
            }
        }
    }

    /// Composites the source over the whole bitmap in `mode` (HTML
    /// §4.12.5.1.3): unlike `source-over`, the modes are defined for every
    /// destination pixel - outside the fill's coverage the source alpha is
    /// zero, which is what makes `copy` and `source-in` clear what they do
    /// not cover. The sampler runs once per bitmap pixel.
    pub fn fill_composited(
        &mut self,
        coverage: Option<&Coverage>,
        clip: Option<&ClipMask>,
        mode: CompositeMode,
        sample: impl Fn(u32, u32) -> Source,
    ) {
        for y in 0..self.height {
            for x in 0..self.width {
                let clip_amount = clip.map_or(1.0, |mask| mask.at(x, y));
                if clip_amount <= 0.0 {
                    continue;
                }
                let mut source = sample(x, y);
                let covered = coverage.map_or(0.0, |coverage| coverage.at(x, y));
                source.alpha = (source.alpha * covered).clamp(0.0, 1.0);
                let index = self.index(x, y);
                let destination = self.pixels[index];
                self.pixels[index] = composite_pixel(destination, source, mode);
            }
        }
    }

    /// Clears the pixels of `coverage` that `clip` admits, by the coverage.
    pub fn clear(&mut self, coverage: &Coverage, clip: Option<&ClipMask>) {
        let (x0, y0) = coverage.origin();
        let (width, height) = coverage.size();
        for y in y0..y0 + height {
            for x in x0..x0 + width {
                let amount = coverage.at(x, y) * clip.map_or(1.0, |mask| mask.at(x, y));
                if amount <= 0.0 {
                    continue;
                }
                let index = self.index(x, y);
                let keep = 1.0 - amount.clamp(0.0, 1.0);
                let pixel = self.pixels[index];
                self.pixels[index] = pixel.map(|channel| round_byte(f32::from(channel) * keep));
            }
        }
    }

    /// The straight-alpha RGBA8 pixels of the rectangle at `(x, y)` of size
    /// `width` by `height`. Pixels outside the bitmap read as transparent black.
    #[must_use]
    pub fn read_straight(&self, x: i64, y: i64, width: u32, height: u32) -> Vec<u8> {
        let mut out = Vec::with_capacity(pixel_count(width, height) * 4);
        for row in 0..i64::from(height) {
            for column in 0..i64::from(width) {
                let pixel = self.get(x + column, y + row).unwrap_or([0; 4]);
                out.extend_from_slice(&unpremultiply(pixel));
            }
        }
        out
    }

    /// The whole bitmap as straight-alpha RGBA8.
    #[must_use]
    pub fn straight_rgba(&self) -> Vec<u8> {
        self.read_straight(0, 0, self.width, self.height)
    }

    /// The whole bitmap as premultiplied RGB8: each pixel's colour
    /// composited onto opaque black. This is the colour a format without an
    /// alpha channel, such as JPEG, encodes.
    #[must_use]
    pub fn premultiplied_rgb(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(pixel_count(self.width, self.height) * 3);
        for pixel in &self.pixels {
            out.extend_from_slice(&pixel[..3]);
        }
        out
    }

    /// Writes straight-alpha RGBA8 `source` (`source_width` by
    /// `source_height`) so that its pixel `(sx, sy)` lands at `(dx + sx, dy + sy)`,
    /// replacing the destination pixels without compositing.
    pub fn write_straight(
        &mut self,
        source: &[u8],
        source_width: u32,
        source_height: u32,
        dx: i64,
        dy: i64,
        dirty: (u32, u32, u32, u32),
    ) {
        let (dirty_x, dirty_y, dirty_width, dirty_height) = dirty;
        for row in dirty_y..dirty_y.saturating_add(dirty_height).min(source_height) {
            for column in dirty_x..dirty_x.saturating_add(dirty_width).min(source_width) {
                let offset = (row as usize * source_width as usize + column as usize) * 4;
                let Some(bytes) = source.get(offset..offset + 4) else {
                    continue;
                };
                let target_x = dx + i64::from(column);
                let target_y = dy + i64::from(row);
                if target_x < 0 || target_y < 0 {
                    continue;
                }
                let (Ok(tx), Ok(ty)) = (u32::try_from(target_x), u32::try_from(target_y)) else {
                    continue;
                };
                if tx >= self.width || ty >= self.height {
                    continue;
                }
                let index = self.index(tx, ty);
                self.pixels[index] = premultiply([bytes[0], bytes[1], bytes[2], bytes[3]]);
            }
        }
    }

    fn get(&self, x: i64, y: i64) -> Option<[u8; 4]> {
        let (Ok(x), Ok(y)) = (u32::try_from(x), u32::try_from(y)) else {
            return None;
        };
        if x >= self.width || y >= self.height {
            return None;
        }
        Some(self.pixels[self.index(x, y)])
    }

    fn index(&self, x: u32, y: u32) -> usize {
        y as usize * self.width as usize + x as usize
    }
}

impl ClipMask {
    /// A mask that admits every pixel of a `width` by `height` canvas.
    #[must_use]
    pub fn full(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            values: vec![255; pixel_count(width, height)],
        }
    }

    /// The mask intersected with the coverage of a new clip path.
    #[must_use]
    pub fn intersect(&self, coverage: &Coverage) -> Self {
        let mut values = vec![0_u8; self.values.len()];
        let (x0, y0) = coverage.origin();
        let (width, height) = coverage.size();
        for y in y0..y0 + height {
            for x in x0..x0 + width {
                let index = y as usize * self.width as usize + x as usize;
                let amount = coverage.at(x, y);
                let existing = f32::from(self.values[index]) / 255.0;
                values[index] = round_byte(amount * existing * 255.0);
            }
        }
        Self {
            width: self.width,
            height: self.height,
            values,
        }
    }

    /// The mask that admits nothing.
    #[must_use]
    pub fn empty(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            values: vec![0; pixel_count(width, height)],
        }
    }

    #[must_use]
    pub fn at(&self, x: u32, y: u32) -> f32 {
        if x >= self.width || y >= self.height {
            return 0.0;
        }
        f32::from(self.values[y as usize * self.width as usize + x as usize]) / 255.0
    }
}

/// The number of pixels of a `width` by `height` grid.
fn pixel_count(width: u32, height: u32) -> usize {
    (width as usize).saturating_mul(height as usize)
}

/// Porter-Duff source-over of a straight colour at `alpha` onto a premultiplied
/// destination pixel.
fn composite_over(destination: [u8; 4], source: Source, alpha: f32) -> [u8; 4] {
    let premultiplied = [
        f32::from(source.red) * alpha,
        f32::from(source.green) * alpha,
        f32::from(source.blue) * alpha,
    ];
    let inverse = 1.0 - alpha;
    let mut out = [0_u8; 4];
    for channel in 0..3 {
        let value =
            premultiplied[channel] / 255.0 + f32::from(destination[channel]) / 255.0 * inverse;
        out[channel] = round_byte(value * 255.0);
    }
    let alpha_value = alpha + f32::from(destination[3]) / 255.0 * inverse;
    out[3] = round_byte(alpha_value * 255.0);
    out
}

/// The Porter-Duff compositing of one source pixel onto one destination
/// pixel (HTML §4.12.5.1.3 "compositing"): the source and destination
/// weights the mode names, applied to premultiplied colours and an alpha
/// that adds the same way. A blend mode composes its blend function with
/// source-over, as CSS Compositing and Blending §5.2 defines it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompositeMode {
    Clear,
    Copy,
    Destination,
    DestinationAtop,
    DestinationIn,
    DestinationOut,
    DestinationOver,
    Lighter,
    SourceAtop,
    SourceIn,
    SourceOut,
    SourceOver,
    Xor,
    Blend(BlendMode),
}

/// A separable or nonseparable colour blend function (CSS Compositing and
/// Blending Level 1 §5.3-5.4). Each takes straight, un-premultiplied
/// colours and returns the blended colour, which is then composited with
/// source-over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlendMode {
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
    Hue,
    Saturation,
    Color,
    Luminosity,
}

impl CompositeMode {
    /// The mode a `globalCompositeOperation` keyword names. The keywords are
    /// Web IDL enum values on the context's attribute and therefore
    /// case-sensitive (WPT `2d.composite.operation.casesensitive`); the
    /// selectors are the ones HTML §4.12.5.1.3 lists.
    #[must_use]
    pub fn parse(keyword: &str) -> Option<Self> {
        let mode = match keyword {
            "clear" => Self::Clear,
            "copy" => Self::Copy,
            "destination" => Self::Destination,
            "destination-atop" => Self::DestinationAtop,
            "destination-in" => Self::DestinationIn,
            "destination-out" => Self::DestinationOut,
            "destination-over" => Self::DestinationOver,
            "lighter" => Self::Lighter,
            "source-atop" => Self::SourceAtop,
            "source-in" => Self::SourceIn,
            "source-out" => Self::SourceOut,
            "source-over" => Self::SourceOver,
            "xor" => Self::Xor,
            "multiply" => Self::Blend(BlendMode::Multiply),
            "screen" => Self::Blend(BlendMode::Screen),
            "overlay" => Self::Blend(BlendMode::Overlay),
            "darken" => Self::Blend(BlendMode::Darken),
            "lighten" => Self::Blend(BlendMode::Lighten),
            "color-dodge" => Self::Blend(BlendMode::ColorDodge),
            "color-burn" => Self::Blend(BlendMode::ColorBurn),
            "hard-light" => Self::Blend(BlendMode::HardLight),
            "soft-light" => Self::Blend(BlendMode::SoftLight),
            "difference" => Self::Blend(BlendMode::Difference),
            "exclusion" => Self::Blend(BlendMode::Exclusion),
            "hue" => Self::Blend(BlendMode::Hue),
            "saturation" => Self::Blend(BlendMode::Saturation),
            "color" => Self::Blend(BlendMode::Color),
            "luminosity" => Self::Blend(BlendMode::Luminosity),
            _ => return None,
        };
        Some(mode)
    }

    fn coefficients(self, source_alpha: f32, destination_alpha: f32) -> (f32, f32) {
        match self {
            Self::Clear => (0.0, 0.0),
            Self::Copy => (1.0, 0.0),
            Self::Destination => (0.0, 1.0),
            Self::DestinationAtop => (1.0 - destination_alpha, source_alpha),
            Self::DestinationIn => (0.0, source_alpha),
            Self::DestinationOut => (0.0, 1.0 - source_alpha),
            Self::DestinationOver => (1.0 - destination_alpha, 1.0),
            Self::Lighter => (1.0, 1.0),
            Self::SourceAtop => (destination_alpha, 1.0 - source_alpha),
            Self::SourceIn => (destination_alpha, 0.0),
            Self::SourceOut => (1.0 - destination_alpha, 0.0),
            Self::SourceOver | Self::Blend(_) => (1.0, 1.0 - source_alpha),
            Self::Xor => (1.0 - destination_alpha, 1.0 - source_alpha),
        }
    }
}

impl BlendMode {
    /// The blend function, on straight colours in `0.0..=1.0`.
    ///
    /// The nonseparable modes are the four CSS builds from `SetLum`,
    /// `SetSat` and `ClipColor`, so they share those helpers rather than
    /// repeating the formulas.
    fn apply(self, backdrop: [f32; 3], source: [f32; 3]) -> [f32; 3] {
        match self {
            Self::Multiply => [
                backdrop[0] * source[0],
                backdrop[1] * source[1],
                backdrop[2] * source[2],
            ],
            Self::Screen => [
                screen(backdrop[0], source[0]),
                screen(backdrop[1], source[1]),
                screen(backdrop[2], source[2]),
            ],
            Self::Overlay => [
                hard_light(source[0], backdrop[0]),
                hard_light(source[1], backdrop[1]),
                hard_light(source[2], backdrop[2]),
            ],
            Self::Darken => [
                backdrop[0].min(source[0]),
                backdrop[1].min(source[1]),
                backdrop[2].min(source[2]),
            ],
            Self::Lighten => [
                backdrop[0].max(source[0]),
                backdrop[1].max(source[1]),
                backdrop[2].max(source[2]),
            ],
            Self::ColorDodge => [
                color_dodge(backdrop[0], source[0]),
                color_dodge(backdrop[1], source[1]),
                color_dodge(backdrop[2], source[2]),
            ],
            Self::ColorBurn => [
                color_burn(backdrop[0], source[0]),
                color_burn(backdrop[1], source[1]),
                color_burn(backdrop[2], source[2]),
            ],
            Self::HardLight => [
                hard_light(backdrop[0], source[0]),
                hard_light(backdrop[1], source[1]),
                hard_light(backdrop[2], source[2]),
            ],
            Self::SoftLight => [
                soft_light(backdrop[0], source[0]),
                soft_light(backdrop[1], source[1]),
                soft_light(backdrop[2], source[2]),
            ],
            Self::Difference => [
                (backdrop[0] - source[0]).abs(),
                (backdrop[1] - source[1]).abs(),
                (backdrop[2] - source[2]).abs(),
            ],
            Self::Exclusion => [
                exclusion(backdrop[0], source[0]),
                exclusion(backdrop[1], source[1]),
                exclusion(backdrop[2], source[2]),
            ],
            Self::Hue => set_luminosity(
                set_saturation(source, saturation(backdrop)),
                luminosity(backdrop),
            ),
            Self::Saturation => set_luminosity(
                set_saturation(backdrop, saturation(source)),
                luminosity(backdrop),
            ),
            Self::Color => set_luminosity(source, luminosity(backdrop)),
            Self::Luminosity => set_luminosity(backdrop, luminosity(source)),
        }
    }
}

/// `B(Cb, Cs) = Cb + Cs - Cb x Cs`.
fn screen(backdrop: f32, source: f32) -> f32 {
    backdrop + source - backdrop * source
}

/// The shared hard-light formula; `overlay` is hard-light with the two
/// colours exchanged (CSS Compositing and Blending §5.3).
fn hard_light(backdrop: f32, source: f32) -> f32 {
    if source <= 0.5 {
        2.0 * source * backdrop
    } else {
        1.0 - 2.0 * (1.0 - source) * (1.0 - backdrop)
    }
}

/// `B(Cb, Cs) = Cb / (1 - Cs)`, with the two degenerate ends at 0 and 1.
fn color_dodge(backdrop: f32, source: f32) -> f32 {
    if backdrop <= 0.0 {
        0.0
    } else if source >= 1.0 {
        1.0
    } else {
        (backdrop / (1.0 - source)).min(1.0)
    }
}

/// `B(Cb, Cs) = 1 - min(1, (1 - Cb) / Cs)`, with the degenerate ends at 1
/// and 0.
fn color_burn(backdrop: f32, source: f32) -> f32 {
    if backdrop >= 1.0 {
        1.0
    } else if source <= 0.0 {
        0.0
    } else {
        1.0 - ((1.0 - backdrop) / source).min(1.0)
    }
}

/// The piecewise soft-light curve of CSS Compositing and Blending §5.3.
fn soft_light(backdrop: f32, source: f32) -> f32 {
    if source <= 0.5 {
        backdrop - (1.0 - 2.0 * source) * backdrop * (1.0 - backdrop)
    } else {
        let d = if backdrop <= 0.25 {
            ((16.0 * backdrop - 12.0) * backdrop + 4.0) * backdrop
        } else {
            backdrop.sqrt()
        };
        backdrop + (2.0 * source - 1.0) * (d - backdrop)
    }
}

/// `B(Cb, Cs) = Cb + Cs - 2 x Cb x Cs`.
fn exclusion(backdrop: f32, source: f32) -> f32 {
    backdrop + source - 2.0 * backdrop * source
}

/// The luminance coefficients of CSS Compositing and Blending §5.4.
fn luminosity(colour: [f32; 3]) -> f32 {
    0.3 * colour[0] + 0.59 * colour[1] + 0.11 * colour[2]
}

/// `ClipColor(C)`: pull a colour back inside the unit cube without changing
/// its luminosity.
fn clip_color(mut colour: [f32; 3]) -> [f32; 3] {
    let lum = luminosity(colour);
    let lowest = colour.iter().copied().fold(f32::INFINITY, f32::min);
    let highest = colour.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    if lowest < 0.0 && lum != lowest {
        for channel in &mut colour {
            *channel = lum + (*channel - lum) * lum / (lum - lowest);
        }
    }
    if highest > 1.0 && highest != lum {
        for channel in &mut colour {
            *channel = lum + (*channel - lum) * (1.0 - lum) / (highest - lum);
        }
    }
    colour
}

/// `SetLum(C, l)`: change a colour's luminosity.
fn set_luminosity(colour: [f32; 3], target: f32) -> [f32; 3] {
    let difference = target - luminosity(colour);
    clip_color([
        colour[0] + difference,
        colour[1] + difference,
        colour[2] + difference,
    ])
}

/// `Sat(C)`: the saturation of a colour.
fn saturation(colour: [f32; 3]) -> f32 {
    let highest = colour.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let lowest = colour.iter().copied().fold(f32::INFINITY, f32::min);
    highest - lowest
}

/// `SetSat(C, s)`: change a colour's saturation.
fn set_saturation(colour: [f32; 3], target: f32) -> [f32; 3] {
    let highest = colour.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let lowest = colour.iter().copied().fold(f32::INFINITY, f32::min);
    if highest <= lowest {
        return [0.0; 3];
    }
    colour.map(|channel| {
        if channel == lowest {
            0.0
        } else if channel == highest {
            target
        } else {
            (channel - lowest) * target / (highest - lowest)
        }
    })
}

/// One Porter-Duff composite of `source` (its alpha already carries the
/// coverage) onto `destination` in `mode`.
fn composite_pixel(destination: [u8; 4], source: Source, mode: CompositeMode) -> [u8; 4] {
    let source_alpha = source.alpha.clamp(0.0, 1.0);
    let destination_alpha = f32::from(destination[3]) / 255.0;
    if let CompositeMode::Blend(blend) = mode {
        return composite_blend(destination, source, source_alpha, destination_alpha, blend);
    }
    let (source_weight, destination_weight) = mode.coefficients(source_alpha, destination_alpha);
    let output_alpha =
        (source_alpha * source_weight + destination_alpha * destination_weight).min(1.0);
    if output_alpha <= 0.0 {
        return [0; 4];
    }
    // The bitmap stores premultiplied colours, the same convention
    // `composite_over` and `read_straight` follow.
    let mut out = [0_u8; 4];
    for channel in 0..3 {
        let source_colour = f32::from([source.red, source.green, source.blue][channel])
            * source_alpha
            * source_weight;
        // `destination` is already premultiplied, so its alpha is in the
        // colour channel; only the mode's weight applies.
        let destination_colour = f32::from(destination[channel]) * destination_weight;
        out[channel] = round_byte(source_colour + destination_colour);
    }
    out[3] = round_byte(output_alpha * 255.0);
    out
}

/// A blend mode's result: blend the two straight colours, mix the blend with
/// the source by the backdrop's alpha (CSS Compositing and Blending §5.2),
/// then composite that colour with source-over.
fn composite_blend(
    destination: [u8; 4],
    source: Source,
    source_alpha: f32,
    destination_alpha: f32,
    blend: BlendMode,
) -> [u8; 4] {
    let source_straight = [
        f32::from(source.red) / 255.0,
        f32::from(source.green) / 255.0,
        f32::from(source.blue) / 255.0,
    ];
    // The destination is stored premultiplied. Its straight colour only
    // enters the blend where the backdrop exists; where it does not, the
    // source reaches the result unblended, which the alpha-weighted mix below
    // gives for any backdrop colour.
    let backdrop_straight = if destination_alpha > 0.0 {
        let scale = 1.0 / (destination_alpha * 255.0);
        [
            f32::from(destination[0]) * scale,
            f32::from(destination[1]) * scale,
            f32::from(destination[2]) * scale,
        ]
    } else {
        [0.0; 3]
    };
    let blended = blend.apply(backdrop_straight, source_straight);
    let output_alpha = source_alpha + destination_alpha * (1.0 - source_alpha);
    let mut out = [0_u8; 4];
    for channel in 0..3 {
        let mixed = (1.0 - destination_alpha) * source_straight[channel]
            + destination_alpha * blended[channel];
        let premultiplied =
            source_alpha * mixed + f32::from(destination[channel]) / 255.0 * (1.0 - source_alpha);
        out[channel] = round_byte(premultiplied * 255.0);
    }
    out[3] = round_byte(output_alpha * 255.0);
    out
}

/// The straight-alpha form of a premultiplied pixel, rounded to bytes.
fn unpremultiply(pixel: [u8; 4]) -> [u8; 4] {
    let alpha = pixel[3];
    if alpha == 0 {
        return [0; 4];
    }
    let scale = 255.0 / f32::from(alpha);
    [
        round_byte(f32::from(pixel[0]) * scale),
        round_byte(f32::from(pixel[1]) * scale),
        round_byte(f32::from(pixel[2]) * scale),
        alpha,
    ]
}

/// The premultiplied form of a straight-alpha pixel, rounded to bytes.
fn premultiply(pixel: [u8; 4]) -> [u8; 4] {
    let alpha = f32::from(pixel[3]) / 255.0;
    [
        round_byte(f32::from(pixel[0]) * alpha),
        round_byte(f32::from(pixel[1]) * alpha),
        round_byte(f32::from(pixel[2]) * alpha),
        pixel[3],
    ]
}

/// Rounds a `0.0..=255.0` channel value to the nearest byte.
pub(crate) fn round_byte(value: f32) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}
