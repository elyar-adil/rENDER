use std::io::Cursor;

use image_codec::AnimationDecoder;
use image_codec::ImageDecoder;
use image_codec::codecs::gif::GifDecoder;

use super::{AnimatedImage, AnimationLimits, collect_frames, enforce_canvas, loop_from_gif_repeat};
use crate::image::{ImageDecodeError, ImageLimits};

pub(super) fn decode(
    bytes: &[u8],
    limits: ImageLimits,
    animation_limits: AnimationLimits,
) -> Result<AnimatedImage, ImageDecodeError> {
    let decoder = GifDecoder::new(Cursor::new(bytes))
        .map_err(|error| ImageDecodeError::Codec(error.to_string()))?;
    let (width, height) = decoder.dimensions();
    enforce_canvas(width, height, limits, animation_limits)?;
    let loop_count = loop_from_gif_repeat(parse_loop_count(bytes));
    collect_frames(
        decoder.into_frames(),
        width,
        height,
        loop_count,
        animation_limits,
    )
}

fn skip_sub_blocks(bytes: &[u8], position: &mut usize) -> Option<()> {
    loop {
        let size = usize::from(*bytes.get(*position)?);
        *position = position.checked_add(1)?;
        if size == 0 {
            return Some(());
        }
        *position = position.checked_add(size)?;
        bytes.get(..*position)?;
    }
}

/// Read the NETSCAPE2.0/ANIMEXTS1.0 loop count without decoding pixels.
/// Malformed extension data yields `None`; the pixel decoder still owns the
/// authoritative validity check.
fn parse_loop_count(bytes: &[u8]) -> Option<u16> {
    let header = bytes.get(..6)?;
    if header != b"GIF87a" && header != b"GIF89a" {
        return None;
    }
    let flags = *bytes.get(10)?;
    let mut position = 13_usize;
    if flags & 0x80 != 0 {
        let entries = 1_usize << (usize::from(flags & 0x07) + 1);
        position = position.checked_add(entries.checked_mul(3)?)?;
    }
    loop {
        let block = *bytes.get(position)?;
        position = position.checked_add(1)?;
        match block {
            0x21 => {
                let label = *bytes.get(position)?;
                position = position.checked_add(1)?;
                if label == 0xFF {
                    let size = usize::from(*bytes.get(position)?);
                    position = position.checked_add(1)?;
                    let identifier = bytes.get(position..position.checked_add(size)?)?;
                    position = position.checked_add(size)?;
                    let repeats = identifier == b"NETSCAPE2.0" || identifier == b"ANIMEXTS1.0";
                    loop {
                        let sub_size = usize::from(*bytes.get(position)?);
                        position = position.checked_add(1)?;
                        if sub_size == 0 {
                            break;
                        }
                        let data = bytes.get(position..position.checked_add(sub_size)?)?;
                        position = position.checked_add(sub_size)?;
                        if repeats && sub_size >= 3 && data[0] == 1 {
                            return Some(u16::from_le_bytes([data[1], data[2]]));
                        }
                    }
                } else {
                    skip_sub_blocks(bytes, &mut position)?;
                }
            }
            0x2C => {
                let descriptor_end = position.checked_add(9)?;
                let packed = *bytes.get(descriptor_end.checked_sub(1)?)?;
                position = descriptor_end;
                if packed & 0x80 != 0 {
                    let entries = 1_usize << (usize::from(packed & 0x07) + 1);
                    position = position.checked_add(entries.checked_mul(3)?)?;
                }
                bytes.get(position)?;
                position = position.checked_add(1)?;
                skip_sub_blocks(bytes, &mut position)?;
            }
            _ => return None,
        }
    }
}
