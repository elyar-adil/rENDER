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
