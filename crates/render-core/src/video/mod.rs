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

//! Video playback pipeline: MP4 demuxing, H.264 bitstream support, a decoder
//! backend boundary, and the timestamped frame queue the future paint phase
//! will consume.
//!
//! Phase scope (video playback phase 1):
//! - `demux_h264_track` locates the H.264 video track in a progressive MP4
//!   (`stsz`/`stco`/`stsc`/`stts`/`stss`/`ctts` plus the `avcC` record) via
//!   Mozilla's pure-Rust `mp4parse` crate.
//! - `avc` parses the decoder configuration and sequence parameter sets far
//!   enough to recover coded dimensions and NAL parameter sets, and converts
//!   AVCC-formatted samples to Annex-B access units.
//! - `VideoPipeline` is the lazy, pull-based entry point: `decode_next_frame`
//!   decodes one more sample whenever the caller asks for a frame, feeding a
//!   bounded [`FrameQueue`]. The `HTMLVideoElement` bindings hold one
//!   pipeline per element once its media loads; a presentation clock and
//!   frame hand-off into paint arrive in a later phase.
//! - Pixel decoding sits behind the [`VideoDecoder`] trait. The shipped
//!   [`PlaceholderDecoder`] reports [`VideoError::DecoderUnavailable`]: the
//!   suggested `openh264` crate compiles vendored C and was rejected under
//!   this workspace's pure-Rust dependency rule, so a real backend is
//!   integration work for a later phase.

pub mod avc;
pub mod decoder;
pub mod demuxer;

#[cfg(test)]
pub(crate) mod test_mp4;

use std::collections::VecDeque;
use std::fmt;

pub use decoder::DecodedPicture;
pub use decoder::PlaceholderDecoder;
pub use decoder::VideoDecoder;
pub use demuxer::DemuxedTrack;
pub use demuxer::VideoCodec;
pub use demuxer::VideoSample;
pub use demuxer::VideoTrackInfo;
pub use demuxer::demux_h264_track;

/// Default bound on frames retained by a [`FrameQueue`]. At typical frame
/// rates this is several seconds of buffered video, and it keeps a decoder
/// faster than its consumer from growing memory without bound.
pub const DEFAULT_FRAME_QUEUE_CAPACITY: usize = 120;

/// Failure modes of the video pipeline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VideoError {
    /// The container bytes are not a usable MP4 or carry no usable track.
    Container(String),
    /// The track's codec is recognized but not playable here.
    UnsupportedCodec(String),
    /// H.264 bitstream syntax outside what this pipeline parses.
    Bitstream(String),
    /// No pixel-decode backend is wired (phase boundary; see module docs).
    DecoderUnavailable,
    /// A wired decoder backend failed on an access unit.
    Decode(String),
}

impl fmt::Display for VideoError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Container(message) => write!(formatter, "media container error: {message}"),
            Self::UnsupportedCodec(message) => {
                write!(formatter, "unsupported media codec: {message}")
            }
            Self::Bitstream(message) => write!(formatter, "H.264 bitstream error: {message}"),
            Self::DecoderUnavailable => write!(
                formatter,
                "H.264 pixel decoding is not wired in this build (phase 2)"
            ),
            Self::Decode(message) => write!(formatter, "decoder error: {message}"),
        }
    }
}

impl std::error::Error for VideoError {}

/// Pixel payload of a decoded frame. RGBA is the paint-ready layout the
/// frame queue hands to the render phase; NV12 is the layout hardware and
/// software H.264 decoders natively produce, so a backend can return it
/// without an intermediate conversion pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameData {
    /// Packed 8-bit RGBA, row-major, tightly packed.
    Rgba {
        width: u32,
        height: u32,
        /// `width * height * 4` pixel bytes.
        bytes: Vec<u8>,
    },
    /// NV12: full-resolution luma plane followed by interleaved chroma.
    Nv12 {
        width: u32,
        height: u32,
        /// `width * height` luma bytes.
        luma: Vec<u8>,
        /// `width * height / 2` interleaved UV bytes.
        chroma_uv: Vec<u8>,
    },
}

impl FrameData {
    /// Coded dimensions of the payload.
    #[must_use]
    pub const fn dimensions(&self) -> (u32, u32) {
        match self {
            Self::Rgba { width, height, .. } | Self::Nv12 { width, height, .. } => {
                (*width, *height)
            }
        }
    }
}

/// One timestamped decoded frame as consumed by the future paint phase.
#[derive(Clone, Debug, PartialEq)]
pub struct VideoFrame {
    /// Presentation timestamp in seconds (`ctts`-corrected).
    pub timestamp: f64,
    /// Decode timestamp in seconds.
    pub decode_timestamp: f64,
    /// Whether the frame decodes from a sync sample.
    pub keyframe: bool,
    /// Pixel payload.
    pub data: FrameData,
}

/// Bounded FIFO of decoded frames in presentation order.
///
/// When a push would exceed the capacity the oldest frame is evicted, which
/// keeps a fast decoder from outgrowing memory while a slow consumer drains.
#[derive(Clone, Debug)]
pub struct FrameQueue {
    frames: VecDeque<VideoFrame>,
    capacity: usize,
}

impl FrameQueue {
    /// An empty queue holding at most `capacity` frames.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            frames: VecDeque::new(),
            capacity: capacity.max(1),
        }
    }

    /// Push a frame; returns the evicted oldest frame when the queue was
    /// full.
    pub fn push(&mut self, frame: VideoFrame) -> Option<VideoFrame> {
        let evicted = if self.frames.len() >= self.capacity {
            self.frames.pop_front()
        } else {
            None
        };
        self.frames.push_back(frame);
        evicted
    }

    /// Pop the next frame in presentation order.
    pub fn pop(&mut self) -> Option<VideoFrame> {
        self.frames.pop_front()
    }

    /// Drain every queued frame, oldest first.
    pub fn drain(&mut self) -> Vec<VideoFrame> {
        self.frames.drain(..).collect()
    }

    /// Number of queued frames.
    #[must_use]
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// Whether no frame is queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }
}

/// Lazy demux/decode pipeline over one buffered MP4.
///
/// The pipeline owns the container bytes, the demuxed sample table, and a
/// decoder backend. Decoding is pull-based: every [`Self::decode_next_frame`]
/// call decodes samples until a frame is available or the stream ends. This
/// is the object the `HTMLVideoElement` bindings retain; the future paint
/// phase will consume frames from it on the presentation clock.
pub struct VideoPipeline {
    track: DemuxedTrack,
    container: Vec<u8>,
    next_sample: usize,
    flushed: bool,
    decoder: Box<dyn VideoDecoder>,
    decoder_ready: bool,
    queue: FrameQueue,
}

impl VideoPipeline {
    /// Open a pipeline over a fully buffered progressive MP4 with the
    /// placeholder decoder.
    ///
    /// # Errors
    ///
    /// [`VideoError::Container`] when no H.264 video track can be demuxed,
    /// plus whatever [`Self::with_decoder`] propagates.
    pub fn open(container: &[u8]) -> Result<Self, VideoError> {
        Self::with_decoder(container, Box::new(PlaceholderDecoder::new()))
    }

    /// Open a pipeline with an explicit decoder backend (tests now, the real
    /// H.264 backend later).
    ///
    /// # Errors
    ///
    /// [`VideoError::Container`] when no H.264 video track can be demuxed
    /// from `container`.
    pub fn with_decoder(
        container: &[u8],
        decoder: Box<dyn VideoDecoder>,
    ) -> Result<Self, VideoError> {
        let track = demux_h264_track(container)?;
        Ok(Self {
            track,
            container: container.to_vec(),
            next_sample: 0,
            flushed: false,
            decoder,
            decoder_ready: false,
            queue: FrameQueue::new(DEFAULT_FRAME_QUEUE_CAPACITY),
        })
    }

    /// Metadata of the demuxed track.
    #[must_use]
    pub const fn track(&self) -> &DemuxedTrack {
        &self.track
    }

    /// Samples still waiting to be fed to the decoder.
    #[must_use]
    pub fn samples_remaining(&self) -> usize {
        self.track.samples.len() - self.next_sample
    }

    /// Frames currently queued for presentation.
    #[must_use]
    pub fn queued_frames(&self) -> usize {
        self.queue.len()
    }

    /// Drain every queued frame, presentation order.
    pub fn take_ready_frames(&mut self) -> Vec<VideoFrame> {
        self.queue.drain()
    }

    /// Decode forward until one frame is available; `None` means the stream
    /// is exhausted (and flushed).
    ///
    /// Frames produced by one access unit inherit that access unit's
    /// container timestamps; decoders that reorder output emit their
    /// pictures on a later call, so presentation order can lag decode order
    /// by design.
    ///
    /// # Errors
    ///
    /// Propagates backend [`VideoError`]s; a sample whose byte range falls
    /// outside the buffered container raises [`VideoError::Container`].
    pub fn decode_next_frame(&mut self) -> Result<Option<VideoFrame>, VideoError> {
        if let Some(frame) = self.queue.pop() {
            return Ok(Some(frame));
        }
        while self.next_sample < self.track.samples.len() {
            let index = self.next_sample;
            let sample = self.track.samples[index];
            let start = usize::try_from(sample.offset).map_err(|_| {
                VideoError::Container("sample offset exceeds addressable bytes".to_owned())
            })?;
            let end = start
                .checked_add(usize::try_from(sample.size).unwrap_or(usize::MAX))
                .filter(|end| *end <= self.container.len())
                .ok_or_else(|| {
                    VideoError::Container(format!(
                        "sample {index} range {start}..+{} overruns the container",
                        sample.size
                    ))
                })?;
            let payload = self.container[start..end].to_vec();
            if !self.decoder_ready {
                self.decoder.configure(&self.track.avc_config)?;
                self.decoder_ready = true;
            }
            let parameter_sets: Vec<&[u8]> = self
                .track
                .avc_config
                .sequence_parameters
                .iter()
                .chain(self.track.avc_config.picture_parameters.iter())
                .map(Vec::as_slice)
                .collect();
            let access_unit = avc::avcc_sample_to_annex_b(
                &payload,
                self.track.avc_config.nal_length_size,
                &parameter_sets,
            )?;
            let pictures = self.decoder.decode(&access_unit, sample.keyframe)?;
            for picture in pictures {
                self.queue.push(VideoFrame {
                    timestamp: sample.composition_timestamp,
                    decode_timestamp: sample.decode_timestamp,
                    keyframe: sample.keyframe,
                    data: picture.data,
                });
            }
            self.next_sample += 1;
            if let Some(frame) = self.queue.pop() {
                return Ok(Some(frame));
            }
        }
        if !self.flushed {
            self.flushed = true;
            for picture in self.decoder.flush()? {
                self.queue.push(VideoFrame {
                    timestamp: self.track.info.duration_seconds.unwrap_or(0.0),
                    decode_timestamp: self.track.info.duration_seconds.unwrap_or(0.0),
                    keyframe: false,
                    data: picture.data,
                });
            }
            if let Some(frame) = self.queue.pop() {
                return Ok(Some(frame));
            }
        }
        Ok(None)
    }
}

impl fmt::Debug for VideoPipeline {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VideoPipeline")
            .field("info", &self.track.info)
            .field("samples_remaining", &self.samples_remaining())
            .field("queued_frames", &self.queue.len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use crate::video::FrameData;
    use crate::video::VideoDecoder;
    use crate::video::VideoError;
    use crate::video::VideoPipeline;
    use crate::video::avc::SeqParameterSet;
    use crate::video::decoder::DecodedPicture;
    use crate::video::test_mp4::ColorTestDecoder;
    use crate::video::test_mp4::TestMp4Builder;

    /// A decoder backend that fails on the first access unit, proving errors
    /// propagate instead of being swallowed by the queue.
    struct FailingDecoder;

    impl VideoDecoder for FailingDecoder {
        fn configure(
            &mut self,
            _config: &crate::video::avc::AvcDecoderConfig,
        ) -> Result<(), VideoError> {
            Ok(())
        }

        fn decode(
            &mut self,
            _access_unit: &[u8],
            _keyframe: bool,
        ) -> Result<Vec<DecodedPicture>, VideoError> {
            Err(VideoError::Decode("backend exploded".to_owned()))
        }
    }

    #[test]
    fn decodes_frames_in_order_with_timestamps() {
        let fixture = TestMp4Builder::new().build();
        let mut pipeline =
            VideoPipeline::with_decoder(&fixture, Box::new(ColorTestDecoder::new(64, 48)))
                .expect("fixture opens");
        assert_eq!(pipeline.track().info.sample_count, 4);
        assert_eq!(pipeline.samples_remaining(), 4);

        let mut timestamps = Vec::new();
        let mut keyframes = Vec::new();
        let mut colors = Vec::new();
        while let Some(frame) = pipeline.decode_next_frame().expect("decode proceeds") {
            timestamps.push(frame.timestamp);
            keyframes.push(frame.keyframe);
            let FrameData::Rgba { bytes, .. } = frame.data else {
                panic!("test decoder emits RGBA");
            };
            colors.push(bytes[0]);
        }
        assert_eq!(timestamps, vec![0.0, 0.5, 1.0, 1.5]);
        assert_eq!(keyframes, vec![true, false, false, false]);
        // One synthetic frame per sample, colored by its decode order.
        assert_eq!(colors, vec![1, 2, 3, 4]);
        assert_eq!(pipeline.samples_remaining(), 0);
        // Exhausted streams stay exhausted.
        assert!(pipeline.decode_next_frame().expect("idempotent").is_none());
    }

    #[test]
    fn queue_drains_oldest_first_and_bounded_push_evicts() {
        let fixture = TestMp4Builder::new().build();
        let mut pipeline =
            VideoPipeline::with_decoder(&fixture, Box::new(ColorTestDecoder::new(64, 48)))
                .expect("fixture opens");
        // Decode one frame at a time: `decode_next_frame` hands back the
        // earliest frame directly and leaves the queue empty behind it.
        let first = pipeline
            .decode_next_frame()
            .expect("decode proceeds")
            .expect("first frame");
        assert_eq!(first.timestamp, 0.0);
        assert!(pipeline.take_ready_frames().is_empty());
        let second = pipeline
            .decode_next_frame()
            .expect("decode proceeds")
            .expect("second frame");
        assert_eq!(second.timestamp, 0.5);
        assert!(pipeline.take_ready_frames().is_empty());

        let mut queue = crate::video::FrameQueue::new(2);
        for timestamp in [0.0, 1.0, 2.0, 3.0] {
            queue.push(crate::video::VideoFrame {
                timestamp,
                decode_timestamp: timestamp,
                keyframe: false,
                data: FrameData::Rgba {
                    width: 1,
                    height: 1,
                    bytes: vec![0, 0, 0, 255],
                },
            });
        }
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.pop().expect("oldest retained").timestamp, 2.0);
    }

    #[test]
    fn sample_overrun_and_backend_errors_propagate() {
        let fixture = TestMp4Builder::new().build();
        let mut pipeline =
            VideoPipeline::with_decoder(&fixture, Box::new(FailingDecoder)).expect("fixture opens");
        assert!(matches!(
            pipeline.decode_next_frame(),
            Err(VideoError::Decode(_))
        ));

        // A container whose sample table points past the buffer is rejected
        // at slice time rather than panicking: the builder claims more bytes
        // for the last sample than the mdat payload carries.
        let inflated = TestMp4Builder::new().with_last_sample_inflation(64).build();
        let mut pipeline =
            VideoPipeline::with_decoder(&inflated, Box::new(ColorTestDecoder::new(64, 48)))
                .expect("inflated container still demuxes");
        let mut decoded = 0;
        loop {
            match pipeline.decode_next_frame() {
                Ok(Some(_)) => decoded += 1,
                Ok(None) => panic!("the overrunning sample must error, not end the stream"),
                Err(VideoError::Container(_)) => break,
                Err(error) => panic!("unexpected error while decoding: {error}"),
            }
        }
        // The three in-bounds samples decoded before the overrun failed.
        assert_eq!(decoded, 3);
    }

    #[test]
    fn placeholder_decoder_reports_unavailable() {
        let fixture = TestMp4Builder::new().build();
        let mut pipeline = VideoPipeline::open(&fixture).expect("fixture opens");
        assert!(matches!(
            pipeline.decode_next_frame(),
            Err(VideoError::DecoderUnavailable)
        ));
        let error = pipeline.decode_next_frame().unwrap_err();
        assert_eq!(
            error.to_string(),
            "H.264 pixel decoding is not wired in this build (phase 2)"
        );
    }

    #[test]
    fn sequence_parameter_sets_round_trip_through_avcc() {
        let fixture = TestMp4Builder::new().build();
        let track = crate::video::demux_h264_track(&fixture).expect("fixture demuxes");
        let sps = track
            .avc_config
            .sequence_parameters
            .first()
            .expect("fixture carries an SPS");
        let parsed: SeqParameterSet =
            crate::video::avc::parse_seq_parameter_set(sps).expect("SPS parses");
        assert_eq!((parsed.width, parsed.height), (64, 48));
    }
}
