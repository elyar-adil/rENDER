//! Headless raster capture and pixel comparison against a human-approved
//! baseline.
//!
//! This capability is **report-only by default**. It never fails a test on a
//! pixel difference, and it does not generate a baseline from current output:
//! today's renderer is known-wrong in the ways `docs/visual_fidelity_gaps.md`
//! records, so an auto-generated baseline would encode that wrongness as truth.
//! A baseline is promoted by a human, through the `real-site-shots` binary,
//! only after they have looked at the rendered page and agreed it is correct.
//!
//! The raster comes from the reference backends in [`crate::harness`], never
//! from system fonts, so two machines produce the same bytes for the same
//! fixture.

use std::io::Cursor;
use std::path::Path;

use image_codec::codecs::png::PngEncoder;
use image_codec::{ExtendedColorType, ImageEncoder, ImageFormat, load_from_memory_with_format};
use render_core::paint::{Color, Surface};

use crate::fixture::RealSiteFixture;
use crate::harness::Session;

/// A captured viewport raster in straight RGBA8.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shot {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, row-major, no padding.
    pub rgba: Vec<u8>,
}

impl Shot {
    /// A solid-colour shot, for exercising the comparison maths.
    #[must_use]
    pub fn solid(width: u32, height: u32, color: Color) -> Self {
        let rgba = (0..(width as usize) * (height as usize))
            .flat_map(|_| [color.red, color.green, color.blue, color.alpha])
            .collect();
        Self {
            width,
            height,
            rgba,
        }
    }

    /// Number of pixels in the shot.
    #[must_use]
    pub const fn pixel_count(&self) -> usize {
        (self.width * self.height) as usize
    }

    /// A short, stable digest of the raster, for eyeballing two runs.
    #[must_use]
    pub fn digest(&self) -> String {
        format!("{:08x}", fnv1a32(&self.rgba))
    }
}

/// What the comparison found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BaselineStatus {
    /// No baseline is checked in for this fixture yet.
    Absent,
    /// The baseline has the wrong dimensions for the current viewport.
    MismatchedShape,
    /// Every pixel matches.
    Unchanged,
    /// At least one pixel differs.
    Differs,
}

/// The result of comparing one shot with a baseline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Comparison {
    pub status: BaselineStatus,
    pub differing_pixels: usize,
    pub total_pixels: usize,
    pub max_channel_delta: u8,
}

impl Comparison {
    /// Share of pixels that differ, in the range 0.0 ..= 1.0.
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        reason = "a ratio over at most a few million pixels needs no f64 mantissa"
    )]
    pub fn difference_ratio(&self) -> f32 {
        if self.total_pixels == 0 {
            return 0.0;
        }
        self.differing_pixels as f32 / self.total_pixels as f32
    }
}

/// Capture the reference render of a fixture as RGBA8.
#[must_use]
pub fn capture(session: &Session) -> Shot {
    shot_from_surface(&session.output.raster.surface)
}

/// Convert an engine surface into a shot.
#[must_use]
pub fn shot_from_surface(surface: &Surface) -> Shot {
    let mut rgba = Vec::with_capacity((surface.width() as usize) * (surface.height() as usize) * 4);
    for pixel in surface.pixels() {
        rgba.extend_from_slice(&[pixel.red, pixel.green, pixel.blue, pixel.alpha]);
    }
    Shot {
        width: surface.width(),
        height: surface.height(),
        rgba,
    }
}

/// Compare a shot with an already-decoded baseline.
#[must_use]
pub fn compare(shot: &Shot, baseline: Option<&Shot>) -> Comparison {
    let Some(baseline) = baseline else {
        return Comparison {
            status: BaselineStatus::Absent,
            differing_pixels: 0,
            total_pixels: shot.pixel_count(),
            max_channel_delta: 0,
        };
    };
    if baseline.width != shot.width || baseline.height != shot.height {
        return Comparison {
            status: BaselineStatus::MismatchedShape,
            differing_pixels: shot.pixel_count(),
            total_pixels: shot.pixel_count(),
            max_channel_delta: u8::MAX,
        };
    }
    let mut differing = 0_usize;
    let mut max_delta = 0_u8;
    for (left, right) in shot.rgba.chunks_exact(4).zip(baseline.rgba.chunks_exact(4)) {
        let mut pixel_differs = false;
        for channel in 0..4 {
            let delta = left[channel].abs_diff(right[channel]);
            if delta > 0 {
                pixel_differs = true;
            }
            max_delta = max_delta.max(delta);
        }
        if pixel_differs {
            differing += 1;
        }
    }
    Comparison {
        status: if differing == 0 {
            BaselineStatus::Unchanged
        } else {
            BaselineStatus::Differs
        },
        differing_pixels: differing,
        total_pixels: shot.pixel_count(),
        max_channel_delta: max_delta,
    }
}

/// Read a baseline PNG from disk, if one is there.
#[must_use]
pub fn read_baseline(path: &Path) -> Option<Shot> {
    let bytes = std::fs::read(path).ok()?;
    decode_png(&bytes)
}

/// Decode a baseline PNG.
#[must_use]
pub fn decode_png(bytes: &[u8]) -> Option<Shot> {
    let decoded = load_from_memory_with_format(bytes, ImageFormat::Png).ok()?;
    let rgba = decoded.to_rgba8();
    let (width, height) = rgba.dimensions();
    Some(Shot {
        width,
        height,
        rgba: rgba.into_raw(),
    })
}

/// Encode a shot as a PNG, so a human can open it and judge it.
///
/// # Panics
///
/// Panics when the captured bytes cannot be encoded. A shot always comes from
/// an engine surface with exact dimensions, so this cannot fail in practice.
#[must_use]
pub fn encode_png(shot: &Shot) -> Vec<u8> {
    let mut bytes = Vec::new();
    PngEncoder::new(Cursor::new(&mut bytes))
        .write_image(
            &shot.rgba,
            shot.width,
            shot.height,
            ExtendedColorType::Rgba8,
        )
        .unwrap_or_else(|error| panic!("the captured shot does not encode as PNG: {error}"));
    bytes
}

/// Render a fixture and capture it.
#[must_use]
pub fn capture_fixture(fixture: &'static RealSiteFixture) -> (Session, Shot) {
    let session = Session::load(fixture);
    let shot = capture(&session);
    (session, shot)
}

/// A one-line human-readable report row.
#[must_use]
pub fn describe(fixture: &RealSiteFixture, comparison: &Comparison) -> String {
    format!(
        "{label:<18} {status:<16} {differing:>9} / {total} px differ ({ratio:.4}%), max channel delta {delta}",
        label = fixture.label,
        status = match comparison.status {
            BaselineStatus::Absent => "no-baseline",
            BaselineStatus::MismatchedShape => "shape-mismatch",
            BaselineStatus::Unchanged => "unchanged",
            BaselineStatus::Differs => "differs",
        },
        differing = comparison.differing_pixels,
        total = comparison.total_pixels,
        ratio = comparison.difference_ratio(),
        delta = comparison.max_channel_delta,
    )
}

fn fnv1a32(bytes: &[u8]) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in bytes {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::{
        BaselineStatus, Shot, capture, compare, decode_png, encode_png, shot_from_surface,
    };
    use crate::fixture::{BAIDU_HOME, NETEASE_163_HOME};
    use crate::harness::Session;
    use render_core::paint::{Color, Surface};

    #[test]
    fn a_missing_baseline_is_reported_not_scored() {
        let shot = Shot::solid(4, 4, Color::rgb(0, 0, 0));
        let comparison = compare(&shot, None);
        assert_eq!(comparison.status, BaselineStatus::Absent);
        assert_eq!(comparison.differing_pixels, 0);
    }

    #[test]
    fn comparison_counts_differing_pixels_and_the_worst_channel() {
        let baseline = Shot::solid(2, 2, Color::rgb(10, 10, 10));
        let mut shot = baseline.clone();
        shot.rgba[0] = 200;
        let comparison = compare(&shot, Some(&baseline));
        assert_eq!(comparison.status, BaselineStatus::Differs);
        assert_eq!(comparison.differing_pixels, 1);
        assert_eq!(comparison.total_pixels, 4);
        assert_eq!(comparison.max_channel_delta, 190);
    }

    #[test]
    fn an_identical_baseline_reports_no_difference() {
        let shot = Shot::solid(3, 3, Color::rgb(1, 2, 3));
        assert_eq!(
            compare(&shot, Some(&shot.clone())).status,
            BaselineStatus::Unchanged
        );
    }

    #[test]
    fn a_shape_mismatch_is_distinct_from_a_differs() {
        let shot = Shot::solid(2, 2, Color::rgb(0, 0, 0));
        let baseline = Shot::solid(4, 4, Color::rgb(0, 0, 0));
        assert_eq!(
            compare(&shot, Some(&baseline)).status,
            BaselineStatus::MismatchedShape
        );
    }

    #[test]
    fn png_round_trip_preserves_every_pixel() {
        let original = Shot::solid(7, 5, Color::rgb(12, 200, 90));
        let decoded = decode_png(&encode_png(&original)).expect("the shot round-trips");
        assert_eq!(decoded, original);
    }

    #[test]
    fn capture_reads_the_engine_surface() {
        let session = Session::load(&BAIDU_HOME);
        let surface = &session.output.raster.surface;
        let shot = shot_from_surface(surface);
        assert_eq!(shot.width, surface.width());
        assert_eq!(shot.height, surface.height());
        assert_eq!(
            shot.pixel_count(),
            (surface.width() * surface.height()) as usize
        );
    }

    #[test]
    fn the_reference_render_is_reproducible() {
        let first = Session::load(&BAIDU_HOME);
        let second = Session::load(&BAIDU_HOME);
        assert_eq!(
            first.output.layout.fragments,
            second.output.layout.fragments
        );
        assert_eq!(capture(&first), capture(&second));
    }

    #[test]
    fn the_portal_fixture_raster_is_not_a_single_flat_colour() {
        let session = Session::load(&NETEASE_163_HOME);
        let surface: &Surface = &session.output.raster.surface;
        let background = surface.pixel(0, 0).expect("the corner pixel exists");
        let painted = surface
            .pixels()
            .iter()
            .filter(|pixel| **pixel != background)
            .count();
        let total = (surface.width() * surface.height()) as usize;
        assert!(
            painted > total / 20,
            "only {painted} of {total} pixels differ from the canvas colour"
        );
    }
}
