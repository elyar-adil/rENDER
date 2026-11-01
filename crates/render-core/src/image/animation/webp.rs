use std::io::Cursor;
use std::sync::Arc;

use image_codec::AnimationDecoder;
use image_codec::ImageDecoder;
use image_codec::codecs::webp::WebPDecoder;

use super::{
    AnimatedImage, AnimationLimits, AnimationLoop, Frame, collect_frames, enforce_canvas,
    loop_from_webp_count,
};
use crate::image::{ImageDecodeError, ImageFormat, ImageLimits};

pub(super) fn decode(
    bytes: &[u8],
    limits: ImageLimits,
    animation_limits: AnimationLimits,
) -> Result<AnimatedImage, ImageDecodeError> {
    let anim = riff_chunk(bytes, *b"ANIM");
    let decoder = WebPDecoder::new(Cursor::new(bytes))
        .map_err(|error| ImageDecodeError::Codec(error.to_string()))?;
    if !decoder.has_animation() {
        if anim.is_some() {
            // Never silently fall back to frame zero when the container claims
            // an animation that the linked decoder cannot expose.
            return Err(ImageDecodeError::UnsupportedAnimation(
                "animated WebP".to_owned(),
            ));
        }
        // WebPDecoder's AnimationDecoder path assumes an animated stream, so
        // static WebP keeps using the ordinary still decoder and becomes a
        // one-frame sequence.
        let image = super::super::decode_image(bytes, ImageFormat::WebP, limits)?;
        let (width, height) = image.intrinsic_size();
        enforce_canvas(width, height, limits, animation_limits)?;
        let delay = animation_limits.min_frame_delay_ms.max(1);
        return AnimatedImage::build(
            width,
            height,
            vec![Frame::new(Arc::new(image), delay)],
            AnimationLoop::Finite(1),
        );
    }
    let loop_count = anim
        .and_then(|payload| {
            (payload.len() >= 6).then(|| u16::from_le_bytes([payload[4], payload[5]]))
        })
        .map_or(AnimationLoop::Finite(1), loop_from_webp_count);
    let (width, height) = decoder.dimensions();
    enforce_canvas(width, height, limits, animation_limits)?;
    collect_frames(
        decoder.into_frames(),
        width,
        height,
        loop_count,
        animation_limits,
    )
}

pub(super) fn riff_chunk(bytes: &[u8], wanted: [u8; 4]) -> Option<&[u8]> {
    if bytes.get(..4)? != b"RIFF" || bytes.get(8..12)? != b"WEBP" {
        return None;
    }
    let mut position = 12_usize;
    while position.checked_add(8)? <= bytes.len() {
        let id = bytes.get(position..position.checked_add(4)?)?;
        let size = usize::try_from(u32::from_le_bytes(
            bytes
                .get(position.checked_add(4)?..position.checked_add(8)?)?
                .try_into()
                .ok()?,
        ))
        .ok()?;
        let payload_start = position.checked_add(8)?;
        let payload_end = payload_start.checked_add(size)?;
        let payload = bytes.get(payload_start..payload_end)?;
        if id == wanted.as_slice() {
            return Some(payload);
        }
        position = payload_end.checked_add(size & 1)?;
    }
    None
}
