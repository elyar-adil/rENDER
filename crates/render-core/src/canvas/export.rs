//! Serializing a canvas bitmap as a `data:` URL (HTML §4.12.5.3).

#![allow(
    // The quality is clamped to 0..=1 before it becomes a percentage, so the
    // conversion cannot truncate a meaningful value.
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use base64::Engine as _;
use image_codec::codecs::jpeg::JpegEncoder;
use image_codec::codecs::png::PngEncoder;
use image_codec::{ExtendedColorType, ImageEncoder};

use super::bitmap::Bitmap;

/// The quality a JPEG is encoded with when the caller does not pass a number
/// in `0..=1`.
const DEFAULT_JPEG_QUALITY: f64 = 0.92;

/// The formats `toDataURL` can encode. `image/webp` is not one: the encoder
/// backend has no WebP writer, and the specification sends an unencodable
/// type back to PNG.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Format {
    Png,
    Jpeg,
}

impl Format {
    /// The format a `toDataURL` type names, ASCII case-insensitively. A type
    /// this engine cannot encode, including a syntactically invalid one, is
    /// PNG.
    fn parse(mime_type: &str) -> Self {
        if mime_type.eq_ignore_ascii_case("image/jpeg") {
            Self::Jpeg
        } else {
            Self::Png
        }
    }

    const fn media_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
        }
    }
}

/// The `data:` URL for `bitmap`, encoded in the requested format.
///
/// A bitmap with a zero side serializes to `data:,` (HTML §4.12.5.3). An
/// encoding failure serializes to `data:`; the in-memory codecs do not fail
/// for a bitmap they produced, but the branch keeps the specification's shape.
pub(super) fn data_url(bitmap: &Bitmap, mime_type: &str, quality: Option<f64>) -> String {
    if bitmap.width() == 0 || bitmap.height() == 0 {
        return "data:,".to_owned();
    }
    let format = Format::parse(mime_type);
    let encoded = match format {
        Format::Png => encode_png(bitmap),
        Format::Jpeg => encode_jpeg(bitmap, quality),
    };
    let Some(encoded) = encoded else {
        return "data:".to_owned();
    };
    let mut url = String::from("data:");
    url.push_str(format.media_type());
    url.push_str(";base64,");
    base64::engine::general_purpose::STANDARD.encode_string(encoded, &mut url);
    url
}

fn encode_png(bitmap: &Bitmap) -> Option<Vec<u8>> {
    let pixels = bitmap.straight_rgba();
    let mut encoded = Vec::new();
    PngEncoder::new(&mut encoded)
        .write_image(
            &pixels,
            bitmap.width(),
            bitmap.height(),
            ExtendedColorType::Rgba8,
        )
        .ok()?;
    Some(encoded)
}

/// JPEG has no alpha channel: a pixel's premultiplied colour is its colour
/// composited onto black, which is exactly what the bitmap already stores.
fn encode_jpeg(bitmap: &Bitmap, quality: Option<f64>) -> Option<Vec<u8>> {
    let pixels = bitmap.premultiplied_rgb();
    let quality = quality
        .filter(|quality| quality.is_finite() && (0.0..=1.0).contains(quality))
        .unwrap_or(DEFAULT_JPEG_QUALITY);
    let quality = (quality * 100.0).round().clamp(1.0, 100.0) as u8;
    let mut encoded = Vec::new();
    JpegEncoder::new_with_quality(&mut encoded, quality)
        .write_image(
            &pixels,
            bitmap.width(),
            bitmap.height(),
            ExtendedColorType::Rgb8,
        )
        .ok()?;
    Some(encoded)
}
