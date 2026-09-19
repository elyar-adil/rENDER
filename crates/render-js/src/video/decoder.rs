#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::float_cmp,
    clippy::if_not_else,
    clippy::manual_let_else,
    clippy::map_unwrap_or,
    clippy::match_same_arms,
    clippy::needless_ifs,
    clippy::too_many_lines,
    clippy::wrong_self_convention
)]

//! Decoder backend boundary for the video pipeline.
//!
//! Phase 1 wires everything around pixel decoding: containers are demuxed,
//! samples are converted to Annex-B access units, and results flow into the
//! frame queue. The actual H.264 slice-to-pixel decode lives behind
//! [`VideoDecoder`]. The only shipped implementation is
//! [`PlaceholderDecoder`], which reports [`VideoError::DecoderUnavailable`]:
//! a real backend (Mozilla's `openh264` bindings compile vendored C and were
//! rejected under this workspace's pure-Rust dependency rule; a pure-Rust
//! H.264 decoder does not yet exist on crates.io) plugs in here in a later
//! phase without touching the demuxer, pipeline, or JS surface again.

use crate::video::FrameData;
use crate::video::VideoError;
use crate::video::avc::AvcDecoderConfig;

/// One decoded output picture in decode order. Timestamps are attached by
/// the pipeline (which owns the container's sample timing), not the decoder.
#[derive(Clone, Debug, PartialEq)]
pub struct DecodedPicture {
    /// Pixel payload of the picture.
    pub data: FrameData,
}

/// Pixel decode backend for one configured track.
pub trait VideoDecoder {
    /// Prepare the decoder for a track. Called once before the first
    /// `decode` of a stream.
    ///
    /// # Errors
    ///
    /// Backend-specific; the pipeline propagates the error to the element.
    fn configure(&mut self, config: &AvcDecoderConfig) -> Result<(), VideoError>;

    /// Decode one Annex-B access unit (SPS/PPS may be in-band).
    ///
    /// Returns pictures in decode order; decoders with output reordering may
    /// return an empty vec and emit pictures for later access units.
    ///
    /// # Errors
    ///
    /// Backend-specific decode failures.
    fn decode(
        &mut self,
        access_unit: &[u8],
        keyframe: bool,
    ) -> Result<Vec<DecodedPicture>, VideoError>;

    /// Drain buffered pictures at end of stream. The default returns
    /// nothing.
    ///
    /// # Errors
    ///
    /// Backend-specific flush failures.
    fn flush(&mut self) -> Result<Vec<DecodedPicture>, VideoError> {
        Ok(Vec::new())
    }
}

/// Phase-1 stand-in for a real H.264 pixel decoder.
///
/// Every method reports [`VideoError::DecoderUnavailable`] so a pipeline
/// built through [`crate::video::VideoPipeline::open`] surfaces a clear
/// diagnostic instead of silently producing no frames. Tests and the future
/// backend go through [`crate::video::VideoPipeline::with_decoder`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlaceholderDecoder;

impl PlaceholderDecoder {
    /// A placeholder decoder has no configuration state.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl VideoDecoder for PlaceholderDecoder {
    fn configure(&mut self, _config: &AvcDecoderConfig) -> Result<(), VideoError> {
        Err(VideoError::DecoderUnavailable)
    }

    fn decode(
        &mut self,
        _access_unit: &[u8],
        _keyframe: bool,
    ) -> Result<Vec<DecodedPicture>, VideoError> {
        Err(VideoError::DecoderUnavailable)
    }
}
