use std::borrow::Cow;
use std::io::Cursor;

use image_codec::{DynamicImage, ImageFormat as CodecImageFormat, Rgba, RgbaImage};
use url::Url;

use crate::html::parse_document;
use crate::image::{
    DecodedImage, ImageDecodeError, ImageFormat, ImageLimits, ImageResourceKey, ImageResources,
    ImageSelectionContext, ImageSource, decode_image,
};
use crate::paint::Color;

use super::{
    AnimatedImage, AnimationLimits, AnimationLoop, decode_animation, decode_animation_with_limits,
    supports_animation,
};

const GIF_PALETTE: [u8; 12] = [
    255, 0, 0, // red
    0, 255, 0, // green
    0, 0, 255, // blue
    0, 0, 0, // transparent / black
];

fn gif_frame(
    left: u16,
    top: u16,
    width: u16,
    height: u16,
    indices: Vec<u8>,
) -> gif::Frame<'static> {
    gif::Frame {
        delay: 0,
        dispose: gif::DisposalMethod::Keep,
        transparent: None,
        needs_user_input: false,
        top,
        left,
        width,
        height,
        interlaced: false,
        palette: None,
        buffer: Cow::Owned(indices),
    }
}

fn encode_gif(
    width: u16,
    height: u16,
    palette: &[u8],
    repeat: Option<gif::Repeat>,
    frames: Vec<gif::Frame<'static>>,
) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder = gif::Encoder::new(&mut bytes, width, height, palette).unwrap();
        if let Some(repeat) = repeat {
            encoder.set_repeat(repeat).unwrap();
        }
        for frame in frames {
            encoder.write_frame(&frame).unwrap();
        }
    }
    bytes
}

fn three_frame_gif(repeat: Option<gif::Repeat>) -> Vec<u8> {
    let mut first = gif_frame(0, 0, 2, 1, vec![0, 0]);
    first.delay = 1;
    first.interlaced = true;
    let mut second = gif_frame(0, 0, 2, 1, vec![1, 1]);
    second.delay = 1;
    let mut third = gif_frame(0, 0, 2, 1, vec![2, 2]);
    third.delay = 1;
    encode_gif(2, 1, &GIF_PALETTE, repeat, vec![first, second, third])
}

fn decoded(animation: Option<AnimatedImage>) -> AnimatedImage {
    animation.expect("animated format decodes a sequence")
}

fn resource_key() -> ImageResourceKey {
    let parsed = parse_document("<img src='x.gif'>");
    ImageResourceKey {
        owner: parsed.dom.document(),
        requested_url: Url::parse("https://example.test/x.gif").unwrap(),
        source_snapshot: "x.gif".to_owned(),
        source: ImageSource::Element,
        selection_context: ImageSelectionContext::default(),
    }
}

#[test]
fn gif_frames_are_distinct_and_wrap_on_the_frame_delay_boundary() {
    let bytes = three_frame_gif(Some(gif::Repeat::Infinite));
    let animation =
        decoded(decode_animation(&bytes, ImageFormat::Gif, ImageLimits::default()).unwrap());

    assert_eq!(animation.canvas_size(), (2, 1));
    assert_eq!(animation.frame_count(), 3);
    assert!(animation.is_animated());
    assert_eq!(animation.loop_count(), AnimationLoop::Infinite);
    assert_eq!(animation.duration(), 30);
    assert_eq!(
        animation
            .frames()
            .iter()
            .map(super::Frame::delay_ms)
            .collect::<Vec<_>>(),
        [10, 10, 10]
    );

    assert_eq!(animation.frame_at(0).pixels()[0], Color::rgb(255, 0, 0));
    assert_eq!(animation.frame_at(9).pixels()[0], Color::rgb(255, 0, 0));
    assert_eq!(
        animation.frame_at(10).pixels()[0],
        Color::rgb(0, 255, 0),
        "the exact boundary switches to frame 1"
    );
    assert_eq!(animation.frame_at(20).pixels()[0], Color::rgb(0, 0, 255));
    assert_eq!(animation.frame_at(29).pixels()[0], Color::rgb(0, 0, 255));
    assert_eq!(
        animation.frame_at(30).pixels()[0],
        Color::rgb(255, 0, 0),
        "frame 0 returns at the loop boundary"
    );
    assert_eq!(animation.frame_index_at(u64::MAX), 1);
    assert_eq!(
        animation.frame_at(3_000_040).pixels()[0],
        Color::rgb(0, 255, 0)
    );
}

#[test]
fn gif_interlaced_frame_is_deinterlaced() {
    let mut frame = gif_frame(0, 0, 1, 4, vec![0, 2, 1, 3]);
    frame.delay = 1;
    frame.interlaced = true;
    // For a four-row image the interlace pass order is row 0, row 2, then
    // rows 1 and 3. The palette maps these rows to red, green, blue, black.
    let bytes = encode_gif(1, 4, &GIF_PALETTE, None, vec![frame]);
    let animation =
        decoded(decode_animation(&bytes, ImageFormat::Gif, ImageLimits::default()).unwrap());

    let pixels = animation.frame_at(0);
    assert_eq!(pixels.pixels()[0], Color::rgb(255, 0, 0));
    assert_eq!(pixels.pixels()[1], Color::rgb(0, 255, 0));
    assert_eq!(pixels.pixels()[2], Color::rgb(0, 0, 255));
    assert_eq!(pixels.pixels()[3], Color::rgb(0, 0, 0));
}

#[test]
fn finite_gif_loop_stops_on_its_final_frame() {
    let bytes = three_frame_gif(Some(gif::Repeat::Finite(1)));
    let animation =
        decoded(decode_animation(&bytes, ImageFormat::Gif, ImageLimits::default()).unwrap());

    assert_eq!(animation.loop_count(), AnimationLoop::Finite(2));
    assert_eq!(animation.loop_count().finite_plays(), Some(2));
    assert_eq!(animation.frame_index_at(0), 0);
    assert_eq!(animation.frame_index_at(29), 2);
    assert_eq!(
        animation.frame_index_at(30),
        0,
        "the second and final play starts at one cycle"
    );
    assert_eq!(animation.frame_index_at(59), 2);
    assert_eq!(
        animation.frame_index_at(60),
        2,
        "finite playback clamps to the last frame"
    );
    assert_eq!(animation.frame_index_at(u64::MAX), 2);

    let once = decoded(
        decode_animation(
            &three_frame_gif(None),
            ImageFormat::Gif,
            ImageLimits::default(),
        )
        .unwrap(),
    );
    assert_eq!(once.loop_count(), AnimationLoop::Finite(1));
    assert_eq!(
        once.frame_index_at(30),
        2,
        "a GIF without a Netscape extension plays once and stops"
    );
}

#[test]
fn gif_local_palette_and_transparency_blend_onto_the_previous_frame() {
    let mut first = gif_frame(0, 0, 2, 2, vec![0, 0, 0, 0]);
    first.delay = 1;
    let mut second = gif_frame(0, 0, 2, 2, vec![0, 1, 1, 1]);
    second.delay = 1;
    second.transparent = Some(1);
    second.palette = Some(vec![
        0, 255, 0, // local index 0: green
        0, 0, 0, // local transparent index 1
        0, 0, 0, 0, 0, 0, 0, 0, // pad to four entries
    ]);
    let bytes = encode_gif(
        2,
        2,
        &GIF_PALETTE,
        Some(gif::Repeat::Finite(0)),
        vec![first, second],
    );
    let animation =
        decoded(decode_animation(&bytes, ImageFormat::Gif, ImageLimits::default()).unwrap());

    let second = animation.frame_at(10);
    assert_eq!(second.pixels()[0], Color::rgb(0, 255, 0));
    assert_eq!(
        second.pixels()[1],
        Color::rgb(255, 0, 0),
        "transparent pixels keep the previous frame"
    );
    assert_eq!(second.pixels()[2], Color::rgb(255, 0, 0));
    assert_eq!(second.pixels()[3], Color::rgb(255, 0, 0));
}

#[test]
fn gif_previous_disposal_restores_the_frame_before_it() {
    let mut first = gif_frame(0, 0, 2, 2, vec![0, 0, 0, 0]);
    first.delay = 1;
    let mut second = gif_frame(0, 0, 2, 2, vec![1, 1, 1, 1]);
    second.delay = 1;
    second.dispose = gif::DisposalMethod::Previous;
    let mut third = gif_frame(0, 0, 1, 1, vec![2]);
    third.delay = 1;
    let bytes = encode_gif(
        2,
        2,
        &GIF_PALETTE,
        Some(gif::Repeat::Finite(0)),
        vec![first, second, third],
    );
    let animation =
        decoded(decode_animation(&bytes, ImageFormat::Gif, ImageLimits::default()).unwrap());

    let third = animation.frame_at(20);
    assert_eq!(third.pixels()[0], Color::rgb(0, 0, 255));
    assert_eq!(
        third.pixels()[1],
        Color::rgb(255, 0, 0),
        "Previous disposal removes the green frame"
    );
}

#[test]
fn gif_background_disposal_clears_to_transparent() {
    let mut first = gif_frame(0, 0, 2, 2, vec![0, 0, 0, 0]);
    first.delay = 1;
    let mut second = gif_frame(0, 0, 2, 2, vec![1, 1, 1, 1]);
    second.delay = 1;
    second.dispose = gif::DisposalMethod::Background;
    let mut third = gif_frame(0, 0, 1, 1, vec![2]);
    third.delay = 1;
    let bytes = encode_gif(
        2,
        2,
        &GIF_PALETTE,
        Some(gif::Repeat::Finite(0)),
        vec![first, second, third],
    );
    let animation =
        decoded(decode_animation(&bytes, ImageFormat::Gif, ImageLimits::default()).unwrap());

    let third = animation.frame_at(20);
    assert_eq!(third.pixels()[0], Color::rgb(0, 0, 255));
    assert_eq!(third.pixels()[1].alpha, 0);
    assert_eq!(third.pixels()[2].alpha, 0);
    assert_eq!(third.pixels()[3].alpha, 0);
}

#[test]
fn zero_and_oversized_gif_delays_are_clamped() {
    let mut first = gif_frame(0, 0, 2, 1, vec![0, 0]);
    first.delay = 0;
    let mut second = gif_frame(0, 0, 2, 1, vec![1, 1]);
    second.delay = 6_001;
    let bytes = encode_gif(
        2,
        1,
        &GIF_PALETTE,
        Some(gif::Repeat::Infinite),
        vec![first, second],
    );
    let animation =
        decoded(decode_animation(&bytes, ImageFormat::Gif, ImageLimits::default()).unwrap());

    assert_eq!(animation.frames()[0].delay_ms(), 10);
    assert_eq!(animation.frames()[1].delay_ms(), 60_000);
    assert_eq!(animation.duration(), 60_010);
    assert_eq!(animation.frame_index_at(0), 0);
    assert_eq!(animation.frame_index_at(10), 1);
    assert_eq!(animation.frame_index_at(60_010), 0);
}

#[test]
fn animation_limits_reject_frames_canvas_and_decoded_bytes() {
    let bytes = three_frame_gif(Some(gif::Repeat::Infinite));
    let still_limits = ImageLimits::default();

    let frame_error = decode_animation_with_limits(
        &bytes,
        ImageFormat::Gif,
        still_limits,
        AnimationLimits {
            max_frames: 2,
            ..AnimationLimits::default()
        },
    )
    .unwrap_err();
    assert_eq!(
        frame_error,
        ImageDecodeError::FrameLimit {
            actual: 3,
            limit: 2
        }
    );

    let canvas_error = decode_animation_with_limits(
        &bytes,
        ImageFormat::Gif,
        still_limits,
        AnimationLimits {
            max_canvas_pixels: 1,
            ..AnimationLimits::default()
        },
    )
    .unwrap_err();
    assert_eq!(
        canvas_error,
        ImageDecodeError::PixelLimit {
            actual: 2,
            limit: 1
        }
    );

    let byte_error = decode_animation_with_limits(
        &bytes,
        ImageFormat::Gif,
        still_limits,
        AnimationLimits {
            max_decoded_bytes: 15,
            ..AnimationLimits::default()
        },
    )
    .unwrap_err();
    assert_eq!(
        byte_error,
        ImageDecodeError::DecodedBytesLimit {
            actual: 16,
            limit: 15
        }
    );

    let encoded_error = decode_animation(
        &bytes,
        ImageFormat::Gif,
        ImageLimits {
            max_encoded_bytes: bytes.len() - 1,
            ..ImageLimits::default()
        },
    )
    .unwrap_err();
    assert_eq!(
        encoded_error,
        ImageDecodeError::EncodedBytesLimit {
            actual: bytes.len(),
            limit: bytes.len() - 1
        }
    );
}

#[test]
fn malformed_animation_data_is_rejected_without_panicking() {
    assert!(decode_animation(b"", ImageFormat::Gif, ImageLimits::default()).is_err());
    assert!(decode_animation(b"GIF89a", ImageFormat::Gif, ImageLimits::default()).is_err());
    assert!(decode_animation(b"not a webp", ImageFormat::WebP, ImageLimits::default()).is_err());
    assert!(
        decode_animation(
            b"RIFF\x04\x00\x00\x00WEBP",
            ImageFormat::WebP,
            ImageLimits::default()
        )
        .is_err()
    );
}

#[test]
fn still_formats_and_still_gif_decode_to_one_frame_or_no_sequence() {
    assert!(!supports_animation(ImageFormat::Png));
    assert!(supports_animation(ImageFormat::Gif));
    assert!(supports_animation(ImageFormat::WebP));
    assert!(
        decode_animation(
            b"not really a png",
            ImageFormat::Png,
            ImageLimits::default()
        )
        .unwrap()
        .is_none()
    );
    assert!(
        decode_animation(
            b"not really a jpeg",
            ImageFormat::Jpeg,
            ImageLimits::default()
        )
        .unwrap()
        .is_none()
    );

    let mut frame = gif_frame(0, 0, 2, 1, vec![0, 0]);
    frame.delay = 1;
    let bytes = encode_gif(2, 1, &GIF_PALETTE, None, vec![frame]);
    let animation =
        decoded(decode_animation(&bytes, ImageFormat::Gif, ImageLimits::default()).unwrap());
    assert_eq!(animation.frame_count(), 1);
    assert!(!animation.is_animated());
    assert_eq!(animation.loop_count(), AnimationLoop::Finite(1));
    assert_eq!(animation.duration(), 10);
    assert_eq!(
        decode_image(&bytes, ImageFormat::Gif, ImageLimits::default())
            .unwrap()
            .intrinsic_size(),
        (2, 1)
    );
}

#[test]
fn still_webp_decodes_to_one_frame_through_the_animation_api() {
    let bytes = webp_still([0, 0, 255, 255]);
    let animation =
        decoded(decode_animation(&bytes, ImageFormat::WebP, ImageLimits::default()).unwrap());
    assert_eq!(animation.frame_count(), 1);
    assert!(!animation.is_animated());
    assert_eq!(animation.canvas_size(), (2, 2));
    assert_eq!(animation.duration(), 10);
    assert_eq!(animation.frame_at(0).pixels()[0], Color::rgb(0, 0, 255));
}

#[test]
fn animated_webp_decodes_every_frame_and_wraps() {
    let bytes = animated_webp(
        &[webp_still([255, 0, 0, 255]), webp_still([0, 255, 0, 255])],
        25,
        0,
    );
    let animation =
        decoded(decode_animation(&bytes, ImageFormat::WebP, ImageLimits::default()).unwrap());

    assert_eq!(animation.frame_count(), 2);
    assert_eq!(animation.loop_count(), AnimationLoop::Infinite);
    assert_eq!(animation.duration(), 50);
    assert_eq!(animation.frame_at(0).pixels()[0], Color::rgb(255, 0, 0));
    assert_eq!(animation.frame_at(25).pixels()[0], Color::rgb(0, 255, 0));
    assert_eq!(animation.frame_at(50).pixels()[0], Color::rgb(255, 0, 0));
    assert_eq!(animation.frame_index_at(1_000_050), 0);

    let finite_bytes = animated_webp(
        &[webp_still([255, 0, 0, 255]), webp_still([0, 255, 0, 255])],
        25,
        2,
    );
    let finite = decoded(
        decode_animation(&finite_bytes, ImageFormat::WebP, ImageLimits::default()).unwrap(),
    );
    assert_eq!(finite.loop_count(), AnimationLoop::Finite(2));
    assert_eq!(finite.frame_index_at(100), 1);
}

#[test]
fn image_resources_keep_the_animation_next_to_the_first_frame() {
    let bytes = three_frame_gif(Some(gif::Repeat::Infinite));
    let animation =
        decoded(decode_animation(&bytes, ImageFormat::Gif, ImageLimits::default()).unwrap());
    let animation_bytes = animation.decoded_bytes();
    let key = resource_key();
    let owner = key.owner;
    let mut resources = ImageResources::default();
    let limits = ImageLimits::default();

    let id = resources
        .insert_animated(key.clone(), animation, limits)
        .unwrap();
    let loaded = resources.get_for_node(owner).unwrap();
    assert_eq!(loaded.id, id);
    assert_eq!(loaded.image.pixel(0, 0), Some(Color::rgb(255, 0, 0)));
    assert_eq!(loaded.animation.as_ref().unwrap().frame_count(), 3);
    assert_eq!(resources.get_animation(id).unwrap().frame_count(), 3);
    assert_eq!(
        resources.decoded_bytes(),
        animation_bytes,
        "the shared first frame is not counted twice"
    );

    let still = DecodedImage::from_pixels(1, 1, vec![Color::rgb(0, 255, 0)]).unwrap();
    let still_bytes = still.decoded_bytes();
    let replaced = resources.insert(key, still, limits).unwrap();
    assert_ne!(replaced, id);
    assert!(resources.get_animation(replaced).is_none());
    assert!(resources.get_animation(id).is_none());
    assert_eq!(resources.decoded_bytes(), still_bytes);
    assert!(resources.get_for_node(owner).unwrap().animation.is_none());
}

fn webp_still(pixel: [u8; 4]) -> Vec<u8> {
    let image = RgbaImage::from_pixel(2, 2, Rgba(pixel));
    let mut bytes = Vec::new();
    DynamicImage::ImageRgba8(image)
        .write_to(&mut Cursor::new(&mut bytes), CodecImageFormat::WebP)
        .expect("encode test WebP");
    bytes
}

fn push_3(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes()[..3]);
}

fn write_3(bytes: &mut [u8], value: u32) {
    bytes.copy_from_slice(&value.to_le_bytes()[..3]);
}

fn push_chunk(bytes: &mut Vec<u8>, id: [u8; 4], payload: &[u8]) {
    bytes.extend_from_slice(&id);
    bytes.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_le_bytes());
    bytes.extend_from_slice(payload);
    if payload.len() % 2 == 1 {
        bytes.push(0);
    }
}

fn animated_webp(stills: &[Vec<u8>], delay_ms: u32, loop_count: u16) -> Vec<u8> {
    let (width, height) = (2_u32, 2_u32);
    let mut body = Vec::new();

    let mut extended = [0_u8; 10];
    extended[0] = 0x12; // animation + alpha
    write_3(&mut extended[4..7], width - 1);
    write_3(&mut extended[7..10], height - 1);
    push_chunk(&mut body, *b"VP8X", &extended);

    let mut anim = vec![0_u8; 4];
    anim.extend_from_slice(&loop_count.to_le_bytes());
    push_chunk(&mut body, *b"ANIM", &anim);

    for still in stills {
        let frame_data = super::webp::riff_chunk(still, *b"VP8L").expect("lossless WebP frame");
        let mut anmf = Vec::new();
        push_3(&mut anmf, 0);
        push_3(&mut anmf, 0);
        push_3(&mut anmf, width - 1);
        push_3(&mut anmf, height - 1);
        push_3(&mut anmf, delay_ms);
        anmf.push(0b0000_0010); // no alpha blending, do not dispose
        push_chunk(&mut anmf, *b"VP8L", frame_data);
        push_chunk(&mut body, *b"ANMF", &anmf);
    }

    let mut bytes = b"RIFF".to_vec();
    bytes.extend_from_slice(&u32::try_from(4 + body.len()).unwrap().to_le_bytes());
    bytes.extend_from_slice(b"WEBP");
    bytes.extend_from_slice(&body);
    bytes
}
