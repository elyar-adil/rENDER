//! Animated GIF and animated WebP frame sequences.
//!
//! The still-image path ([`super::decode_image`]) remains the source of the
//! first bitmap. This module adds a self-contained frame sequence that
//! [`super::ImageResources`] can retain next to it without changing how
//! layout or paint consume still images.
//!
//! Frames are normalised to full-canvas straight-alpha RGBA. GIF disposal,
//! transparency, local/global palettes and interlacing are applied by the
//! `image` crate's `GifDecoder` (over the `gif` crate); its `Background`
//! disposal clears pixels to transparent, matching web viewers rather than the
//! rarely-used logical-screen background colour. Animated WebP uses the
//! `WebPDecoder`'s `AnimationDecoder` implementation, which also composites
//! each frame onto the canvas.
//!
//! The document clock is deliberately not wired in this layer. A coordinator
//! that has the page's monotonic time should call [`AnimatedImage::frame_at`]
//! with elapsed milliseconds before painting, then publish the returned frame
//! through the ordinary image-resource seam. See
//! [`super::ImageResources::insert_animated`] for the intended integration
//! point.
//!
//! The remaining wiring is outside this layer: `render-browser`'s image batch
//! currently calls `decode_image` and `ImageResources::insert`, while its
//! render jobs already carry `animation_time_millis` from the page clock. A
//! future turn should decode GIF/WebP with [`decode_animation`], install with
//! `insert_animated`, and before each render publish
//! [`AnimatedImage::frame_at`] at that time; paint still resolves every
//! `ImageResourceId` to the one still bitmap in [`super::ImageResources`].

use std::sync::Arc;

use image_codec::Delay;

use super::{DecodedImage, ImageDecodeError, ImageFormat, ImageLimits};
use crate::paint::Color;

mod gif;
mod webp;

/// Safety limits for an animated-image decode.
///
/// These are separate from [`ImageLimits`] because a frame sequence retains
/// many full canvases. The canvas itself is still checked against the
/// still-image limits first, then against these animation limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnimationLimits {
    /// Maximum number of frames retained from one animation.
    pub max_frames: usize,
    /// Maximum canvas width times height accepted for animation.
    pub max_canvas_pixels: u64,
    /// Maximum total RGBA storage retained across every decoded frame.
    pub max_decoded_bytes: usize,
    /// Minimum effective frame delay. Zero-delay frames are clamped up to this
    /// value so the timeline always advances. Values below 1 are treated as 1.
    pub min_frame_delay_ms: u32,
    /// Maximum effective frame delay. Very long delays are clamped down.
    pub max_frame_delay_ms: u32,
}

impl Default for AnimationLimits {
    fn default() -> Self {
        Self {
            max_frames: 2_048,
            max_canvas_pixels: 64 * 1_024 * 1_024,
            max_decoded_bytes: 512 * 1_024 * 1_024,
            min_frame_delay_ms: 10,
            max_frame_delay_ms: 60_000,
        }
    }
}

/// How many times a frame sequence plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnimationLoop {
    /// Keep wrapping forever (a GIF/WebP loop count of zero).
    Infinite,
    /// Total complete plays, including the first. Always at least one.
    Finite(u32),
}

impl AnimationLoop {
    /// Whether the sequence never stops wrapping.
    #[must_use]
    pub const fn is_infinite(self) -> bool {
        matches!(self, Self::Infinite)
    }

    /// The finite number of complete plays, or `None` for an infinite loop.
    #[must_use]
    pub const fn finite_plays(self) -> Option<u32> {
        match self {
            Self::Infinite => None,
            Self::Finite(plays) => Some(plays),
        }
    }
}

/// One fully composited animation frame.
#[derive(Clone, Debug)]
pub struct Frame {
    image: Arc<DecodedImage>,
    delay_ms: u32,
}

impl Frame {
    fn new(image: Arc<DecodedImage>, delay_ms: u32) -> Self {
        Self { image, delay_ms }
    }

    /// The full-canvas RGBA pixels for this frame.
    #[must_use]
    pub fn image(&self) -> &DecodedImage {
        &self.image
    }

    /// The full-canvas RGBA pixels for this frame.
    #[must_use]
    pub fn pixels(&self) -> &[Color] {
        self.image.pixels()
    }

    /// This frame's effective display delay in milliseconds.
    #[must_use]
    pub const fn delay_ms(&self) -> u32 {
        self.delay_ms
    }
}

/// A decoded GIF or animated-WebP sequence.
///
/// Frames are stored fully composited at canvas size, so [`Self::frame_at`] is
/// a deterministic lookup rather than a replay of disposal state.
#[derive(Clone, Debug)]
pub struct AnimatedImage {
    width: u32,
    height: u32,
    frames: Vec<Frame>,
    loop_count: AnimationLoop,
    total_duration_ms: u64,
}

impl AnimatedImage {
    fn build(
        width: u32,
        height: u32,
        frames: Vec<Frame>,
        loop_count: AnimationLoop,
    ) -> Result<Self, ImageDecodeError> {
        if frames.is_empty() {
            return Err(ImageDecodeError::EmptyAnimation);
        }
        let total_duration_ms = frames.iter().fold(0_u64, |total, frame| {
            total.saturating_add(u64::from(frame.delay_ms))
        });
        Ok(Self {
            width,
            height,
            frames,
            loop_count,
            total_duration_ms,
        })
    }

    /// Canvas width in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Canvas height in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Canvas size in pixels.
    #[must_use]
    pub const fn canvas_size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Every composited frame, in display order.
    #[must_use]
    pub fn frames(&self) -> &[Frame] {
        &self.frames
    }

    /// Number of decoded frames.
    #[must_use]
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Whether this sequence has more than one frame.
    #[must_use]
    pub fn is_animated(&self) -> bool {
        self.frames.len() > 1
    }

    /// The loop policy parsed from the container.
    #[must_use]
    pub const fn loop_count(&self) -> AnimationLoop {
        self.loop_count
    }

    /// Duration of one complete play, in milliseconds.
    ///
    /// Delays are clamped to at least 1 ms while decoding, so this is non-zero
    /// for every non-empty sequence.
    #[must_use]
    pub const fn duration(&self) -> u64 {
        self.total_duration_ms
    }

    /// Total RGBA storage retained by all frames, including their shared
    /// first-frame allocation.
    #[must_use]
    pub fn decoded_bytes(&self) -> usize {
        self.frames
            .iter()
            .map(|frame| frame.image.decoded_bytes())
            .fold(0_usize, usize::saturating_add)
    }

    /// The deterministic frame index shown at `elapsed_ms`.
    ///
    /// Exact frame boundaries select the next frame. Infinite animations wrap
    /// modulo [`Self::duration`]; finite animations stop on their final frame
    /// after every requested play has completed.
    #[must_use]
    pub fn frame_index_at(&self, elapsed_ms: u64) -> usize {
        let duration = self.total_duration_ms;
        if duration == 0 {
            return 0;
        }
        let elapsed = match self.loop_count {
            AnimationLoop::Infinite => elapsed_ms % duration,
            AnimationLoop::Finite(plays) => {
                let total = duration.saturating_mul(u64::from(plays.max(1)));
                if elapsed_ms >= total {
                    duration - 1
                } else {
                    elapsed_ms % duration
                }
            }
        };
        let mut end = 0_u64;
        for (index, frame) in self.frames.iter().enumerate() {
            end = end.saturating_add(u64::from(frame.delay_ms));
            if elapsed < end {
                return index;
            }
        }
        self.frames.len().saturating_sub(1)
    }

    /// The deterministic frame shown at `elapsed_ms`.
    ///
    /// The returned frame shares its immutable pixel buffer with this
    /// sequence, so repeated lookups are cheap.
    #[must_use]
    pub fn frame_at(&self, elapsed_ms: u64) -> Frame {
        self.frames[self.frame_index_at(elapsed_ms)].clone()
    }

    /// The first composited frame.
    #[must_use]
    pub fn first_frame(&self) -> &Frame {
        &self.frames[0]
    }

    pub(crate) fn first_frame_image_arc(&self) -> Arc<DecodedImage> {
        Arc::clone(&self.frames[0].image)
    }
}

/// Whether this format has a frame-sequence decoder.
#[must_use]
pub const fn supports_animation(format: ImageFormat) -> bool {
    matches!(format, ImageFormat::Gif | ImageFormat::WebP)
}

/// Decode every frame of a GIF or animated WebP.
///
/// A still GIF/WebP decodes to a one-frame sequence. Formats without an
/// animation model return `Ok(None)`, so callers can keep the existing still
/// decode path for them. An animated WebP whose animation the linked decoder
/// cannot expose returns [`ImageDecodeError::UnsupportedAnimation`] rather than
/// silently using frame zero.
///
/// # Errors
///
/// Returns encoded-byte, canvas, frame-count, decoded-byte, or codec errors.
pub fn decode_animation(
    bytes: &[u8],
    format: ImageFormat,
    limits: ImageLimits,
) -> Result<Option<AnimatedImage>, ImageDecodeError> {
    decode_animation_with_limits(bytes, format, limits, AnimationLimits::default())
}

/// [`decode_animation`] with explicit animation-specific safety limits.
///
/// # Errors
///
/// Returns encoded-byte, canvas, frame-count, decoded-byte, or codec errors.
pub fn decode_animation_with_limits(
    bytes: &[u8],
    format: ImageFormat,
    limits: ImageLimits,
    animation_limits: AnimationLimits,
) -> Result<Option<AnimatedImage>, ImageDecodeError> {
    if bytes.len() > limits.max_encoded_bytes {
        return Err(ImageDecodeError::EncodedBytesLimit {
            actual: bytes.len(),
            limit: limits.max_encoded_bytes,
        });
    }
    match format {
        ImageFormat::Gif => gif::decode(bytes, limits, animation_limits).map(Some),
        ImageFormat::WebP => webp::decode(bytes, limits, animation_limits).map(Some),
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Avif | ImageFormat::Svg => Ok(None),
    }
}

fn enforce_canvas(
    width: u32,
    height: u32,
    limits: ImageLimits,
    animation_limits: AnimationLimits,
) -> Result<(), ImageDecodeError> {
    super::enforce_dimensions(width, height, limits)?;
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or(ImageDecodeError::DimensionOverflow)?;
    if pixels > animation_limits.max_canvas_pixels {
        return Err(ImageDecodeError::PixelLimit {
            actual: pixels,
            limit: animation_limits.max_canvas_pixels,
        });
    }
    let bytes = usize::try_from(pixels)
        .ok()
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or(ImageDecodeError::DimensionOverflow)?;
    if bytes > animation_limits.max_decoded_bytes {
        return Err(ImageDecodeError::DecodedBytesLimit {
            actual: bytes,
            limit: animation_limits.max_decoded_bytes,
        });
    }
    Ok(())
}

fn delay_ms(delay: Delay, limits: AnimationLimits) -> u32 {
    let (numerator, denominator) = delay.numer_denom_ms();
    let milliseconds = if denominator == 0 {
        0
    } else {
        u64::from(numerator) / u64::from(denominator)
    };
    let minimum = u64::from(limits.min_frame_delay_ms.max(1));
    let maximum = u64::from(
        limits
            .max_frame_delay_ms
            .max(limits.min_frame_delay_ms.max(1)),
    );
    u32::try_from(milliseconds.clamp(minimum, maximum)).unwrap_or(u32::MAX)
}

fn collect_frames(
    frames: impl Iterator<Item = image_codec::ImageResult<image_codec::Frame>>,
    width: u32,
    height: u32,
    loop_count: AnimationLoop,
    limits: AnimationLimits,
) -> Result<AnimatedImage, ImageDecodeError> {
    let mut decoded = Vec::new();
    let mut total_bytes = 0_usize;
    for frame in frames {
        if decoded.len() >= limits.max_frames {
            return Err(ImageDecodeError::FrameLimit {
                actual: limits.max_frames.saturating_add(1),
                limit: limits.max_frames,
            });
        }
        let frame = frame.map_err(|error| ImageDecodeError::Codec(error.to_string()))?;
        let delay = delay_ms(frame.delay(), limits);
        let buffer = frame.into_buffer();
        if buffer.dimensions() != (width, height) {
            return Err(ImageDecodeError::Codec(format!(
                "animation frame is {}x{}, canvas is {width}x{height}",
                buffer.width(),
                buffer.height()
            )));
        }
        let pixels = buffer
            .pixels()
            .map(|pixel| Color::rgba(pixel[0], pixel[1], pixel[2], pixel[3]))
            .collect();
        let image = DecodedImage::from_pixels(width, height, pixels)?;
        let frame_bytes = image.decoded_bytes();
        total_bytes =
            total_bytes
                .checked_add(frame_bytes)
                .ok_or(ImageDecodeError::DecodedBytesLimit {
                    actual: usize::MAX,
                    limit: limits.max_decoded_bytes,
                })?;
        if total_bytes > limits.max_decoded_bytes {
            return Err(ImageDecodeError::DecodedBytesLimit {
                actual: total_bytes,
                limit: limits.max_decoded_bytes,
            });
        }
        decoded.push(Frame::new(Arc::new(image), delay));
    }
    AnimatedImage::build(width, height, decoded, loop_count)
}

fn loop_from_gif_repeat(repeat: Option<u16>) -> AnimationLoop {
    match repeat {
        Some(0) => AnimationLoop::Infinite,
        // The Netscape extension counts repetitions after the first play.
        Some(repetitions) => AnimationLoop::Finite(u32::from(repetitions) + 1),
        None => AnimationLoop::Finite(1),
    }
}

fn loop_from_webp_count(count: u16) -> AnimationLoop {
    match count {
        0 => AnimationLoop::Infinite,
        // WebP's ANIM loop count is the number of complete plays.
        plays => AnimationLoop::Finite(u32::from(plays)),
    }
}

#[cfg(test)]
mod tests;
