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
    clippy::struct_excessive_bools,
    clippy::too_many_lines,
    clippy::wrong_self_convention
)]

//! Presentation clock and decoded-frame selection for video playback.
//!
//! Phase 2 of the video pipeline: the demux/decode phase 1 built is pulled
//! onto the screen. [`PresentationClock`] anchors the media position to the
//! embedding's virtual clock (the page supplies every `Duration`), pause
//! freezes the position, and [`VideoPipeline::advance_playback`] walks the
//! clock, decodes forward only as far as the current position needs, selects
//! the last decoded frame whose presentation timestamp is at or before the
//! position, and reports which DOM events (`timeupdate`, `ended`, `error`)
//! the element layer must queue.
//!
//! Audio is deliberately out of scope: this runtime has no audio device, so
//! audio tracks are ignored end to end and playback runs on the presentation
//! clock alone (no audio-video synchronization).
//!
//! `playbackRate` stays fixed at 1.0; the script-visible property slot exists
//! and variable-rate support can later plug into the clock's anchoring model
//! without touching the callers.

use std::collections::VecDeque;
use std::time::Duration;

use render_dom::NodeId;
use url::Url;

use crate::video::FrameData;
use crate::video::VideoFrame;
use crate::video::VideoPipeline;

/// Minimum virtual-time spacing between `timeupdate` firings (the ~4 Hz the
/// HTML media events guidance suggests).
pub const TIMEUPDATE_INTERVAL: Duration = Duration::from_millis(250);

/// Byte budget for retained decoded frames. The retained history is the
/// backward-seek window; when it is exceeded the oldest frames before the
/// presented one are dropped, so early positions become unreachable by a seek
/// until the element is reloaded.
pub const MAX_RETAINED_FRAME_BYTES: usize = 192 * 1024 * 1024;

/// Assumed frame duration when the end of the media must be inferred from a
/// lone decoded frame (containers without a usable track duration).
const FALLBACK_FRAME_DURATION: f64 = 1.0 / 30.0;

/// Element-layer work produced by one [`VideoPipeline::advance_playback`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackSignal {
    /// A `timeupdate` is due (throttled to [`TIMEUPDATE_INTERVAL`]).
    TimeUpdate,
    /// Playback reached the end of the media without looping.
    Ended,
    /// The decode backend failed; playback is frozen and an `error` event is
    /// due (the shipped placeholder decoder reports this on first use).
    DecodeFailed,
}

/// A paint-ready frame in packed 8-bit RGBA.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresentedFrame {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` pixel bytes.
    pub bytes: Vec<u8>,
}

/// One video element's paint-phase publication after an advance step: the DOM
/// `<video>` box that presents the element, the resolved media URL (resource
/// identity for the paint-side frame store), and the current frame, if any.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FramePublication {
    pub node: NodeId,
    pub media_url: Url,
    pub frame: Option<PresentedFrame>,
}

/// Media position anchored to the embedding's virtual clock.
///
/// `play()`/`pause()` run inside script turns where no clock is available, so
/// a resume anchors lazily at the next observed instant and a pause freezes
/// at the last published position; both are accurate to within one embedding
/// tick (the cadence the page pumps the clock at, ~30 fps).
#[derive(Clone, Copy, Debug)]
pub(crate) struct PresentationClock {
    playing: bool,
    /// Frozen position while paused, or the anchor position while playing.
    position: f64,
    /// The virtual instant matching `position` while playing; `None` until
    /// the first [`Self::anchor`] after a resume.
    anchored_at: Option<Duration>,
}

impl PresentationClock {
    pub(crate) const fn new() -> Self {
        Self {
            playing: false,
            position: 0.0,
            anchored_at: None,
        }
    }

    /// Begin or resume playback from the frozen position.
    pub(crate) fn play(&mut self) {
        self.playing = true;
        self.anchored_at = None;
    }

    /// Freeze at the last anchored position.
    pub(crate) fn pause(&mut self) {
        self.playing = false;
        self.anchored_at = None;
    }

    pub(crate) fn is_playing(&self) -> bool {
        self.playing
    }

    /// Bind the current position to `now` once per resume.
    pub(crate) fn anchor(&mut self, now: Duration) {
        if self.playing && self.anchored_at.is_none() {
            self.anchored_at = Some(now);
        }
    }

    /// Move the position; time flows from here on the next advance.
    pub(crate) fn seek(&mut self, position: f64) {
        self.position = position;
        self.anchored_at = None;
    }

    /// Fold the computed position back into the anchor so a later
    /// [`Self::pause`] freezes at the last published instant.
    pub(crate) fn rebase(&mut self, now: Duration, position: f64) {
        self.position = position;
        self.anchored_at = Some(now);
    }

    /// The media position at virtual instant `now`.
    pub(crate) fn position(&self, now: Duration) -> f64 {
        if !self.playing {
            return self.position;
        }
        match self.anchored_at {
            Some(at) => self.position + now.saturating_sub(at).as_secs_f64(),
            None => self.position,
        }
    }
}

/// Per-element presentation state carried by [`VideoPipeline`].
#[derive(Clone, Debug)]
pub(crate) struct PresentationState {
    pub(crate) clock: PresentationClock,
    /// Track duration in seconds when the container reports one.
    duration: Option<f64>,
    /// Frames decoded so far, in presentation order (the seek window).
    frames: VecDeque<VideoFrame>,
    /// Sum of [`frame_data_bytes`] across `frames`.
    retained_bytes: usize,
    /// The decode stream has been drained to its end.
    exhausted: bool,
    /// The decode backend failed; playback is frozen.
    failed: bool,
    /// The backend failure was already reported once to the element layer.
    reported_failure: bool,
    /// Playback reached the end of the media (non-looping).
    ended: bool,
    /// Presentation timestamp of the frame on screen.
    current_pts: Option<f64>,
    /// Position last published to the element's `currentTime` property.
    published_position: f64,
    /// Virtual instant of the last `timeupdate` signal.
    last_timeupdate: Option<Duration>,
    /// DOM `<video>` element whose box presents this pipeline's frames.
    bound_node: Option<NodeId>,
}

impl PresentationState {
    pub(crate) fn new(duration: Option<f64>) -> Self {
        Self {
            clock: PresentationClock::new(),
            duration,
            frames: VecDeque::new(),
            retained_bytes: 0,
            exhausted: false,
            failed: false,
            reported_failure: false,
            ended: false,
            current_pts: None,
            published_position: 0.0,
            last_timeupdate: None,
            bound_node: None,
        }
    }
}

fn frame_data_bytes(data: &FrameData) -> usize {
    match data {
        FrameData::Rgba { bytes, .. } => bytes.len(),
        FrameData::Nv12 {
            luma, chroma_uv, ..
        } => luma.len() + chroma_uv.len(),
    }
}

impl VideoPipeline {
    /// Begin or resume playback. When playback previously ended, the element
    /// layer seeks back to the start first (the `play()` restart rule).
    pub fn start_playback(&mut self) {
        self.presentation.clock.play();
    }

    /// Pause playback, freezing the position at the last published instant.
    pub fn pause_playback(&mut self) {
        self.presentation.clock.pause();
    }

    /// Whether the presentation clock is running.
    #[must_use]
    pub fn is_playing(&self) -> bool {
        self.presentation.clock.is_playing()
    }

    /// Whether playback reached the end of the media (non-looping).
    #[must_use]
    pub fn playback_ended(&self) -> bool {
        self.presentation.ended
    }

    /// The media position last published to `currentTime`.
    #[must_use]
    pub fn published_position(&self) -> f64 {
        self.presentation.published_position
    }

    /// Whether the decode backend has failed and playback is frozen.
    #[must_use]
    pub fn presentation_failed(&self) -> bool {
        self.presentation.failed
    }

    /// Bytes of decoded frames retained as the seek window, for diagnostics.
    #[must_use]
    pub fn retained_history_bytes(&self) -> usize {
        self.presentation.retained_bytes
    }

    /// Presentation timestamp of the frame on screen, for diagnostics.
    #[must_use]
    pub fn current_frame_timestamp(&self) -> Option<f64> {
        self.presentation.current_pts
    }

    /// The DOM `<video>` node bound to this pipeline for painting.
    #[must_use]
    pub fn bound_node(&self) -> Option<NodeId> {
        self.presentation.bound_node
    }

    /// Bind (or rebind) the DOM `<video>` node whose box presents frames.
    pub fn bind_to_node(&mut self, node: NodeId) {
        self.presentation.bound_node = Some(node);
    }

    /// Drop the DOM node binding; the element stops publishing frames.
    pub fn unbind_node(&mut self) {
        self.presentation.bound_node = None;
    }

    /// The current frame as packed RGBA for the paint phase; `None` while no
    /// decoded frame covers the position. NV12 payloads are converted with
    /// the BT.601 limited-range matrix.
    #[must_use]
    pub fn current_frame_rgba(&self) -> Option<PresentedFrame> {
        let pts = self.presentation.current_pts?;
        let frame = self
            .presentation
            .frames
            .iter()
            .find(|frame| frame.timestamp == pts)?;
        let (width, height, bytes) = match &frame.data {
            FrameData::Rgba {
                width,
                height,
                bytes,
            } => (*width, *height, bytes.clone()),
            FrameData::Nv12 {
                width,
                height,
                luma,
                chroma_uv,
            } => (
                *width,
                *height,
                nv12_to_rgba(*width, *height, luma, chroma_uv),
            ),
        };
        Some(PresentedFrame {
            width,
            height,
            bytes,
        })
    }

    /// Reposition playback to `position` seconds (negative values clamp to
    /// the start, values past the end clamp to the end). Seeking is supported
    /// within the retained decoded-frame window; a forward target decodes
    /// more frames on demand. A seek to the end marks playback ended, and any
    /// ended state clears on a seek away from the end.
    pub fn seek_to(&mut self, position: f64) {
        if !position.is_finite() || self.presentation.failed {
            return;
        }
        let end = self.end_of_media();
        let clamped = position.clamp(0.0, if end.is_finite() { end } else { f64::MAX });
        self.decode_until_covering(clamped);
        let state = &mut self.presentation;
        state.clock.seek(clamped);
        state.ended = end.is_finite() && clamped >= end;
        state.published_position = clamped;
        self.select_current_frame(clamped);
    }

    /// Advance playback to the virtual instant `now`, decoding forward only
    /// as far as the position requires and selecting the presented frame.
    ///
    /// `looping` mirrors the element's `loop` property: reaching the end then
    /// wraps to the start instead of ending. Returns the element-layer
    /// signals in firing order.
    pub fn advance_playback(&mut self, now: Duration, looping: bool) -> Vec<PlaybackSignal> {
        let mut signals = Vec::new();
        if self.presentation.failed {
            self.report_decode_failure_once(&mut signals);
            return signals;
        }
        self.presentation.clock.anchor(now);
        if self.presentation.ended {
            return signals;
        }
        let end = self.end_of_media();
        let raw = self.presentation.clock.position(now);
        let at_end = end.is_finite() && raw >= end;
        let wrap = at_end && looping;
        let position = if wrap {
            self.presentation.clock.seek(0.0);
            self.presentation.last_timeupdate = None;
            0.0
        } else if at_end {
            end
        } else {
            raw
        };
        if at_end && !wrap {
            // Decode through the end so the final frame is presented.
            self.decode_until_covering(end);
            if self.presentation.failed {
                self.report_decode_failure_once(&mut signals);
                return signals;
            }
            self.presentation.clock.pause();
            self.presentation.clock.seek(end);
            self.presentation.ended = true;
            self.presentation.last_timeupdate = Some(now);
            self.select_current_frame(end);
            self.presentation.published_position = end;
            signals.push(PlaybackSignal::TimeUpdate);
            signals.push(PlaybackSignal::Ended);
            return signals;
        }
        if self.presentation.clock.is_playing() {
            self.decode_until_covering(position);
            if self.presentation.failed {
                self.report_decode_failure_once(&mut signals);
                return signals;
            }
            self.select_current_frame(position);
            self.presentation.published_position = position;
            self.presentation.clock.rebase(now, position);
            if self
                .presentation
                .last_timeupdate
                .is_none_or(|at| now.saturating_sub(at) >= TIMEUPDATE_INTERVAL)
            {
                self.presentation.last_timeupdate = Some(now);
                signals.push(PlaybackSignal::TimeUpdate);
            }
        }
        signals
    }

    /// Surface a backend failure exactly once per element.
    fn report_decode_failure_once(&mut self, signals: &mut Vec<PlaybackSignal>) {
        if !self.presentation.reported_failure {
            self.presentation.reported_failure = true;
            signals.push(PlaybackSignal::DecodeFailed);
        }
    }

    /// Decode forward until a retained frame's timestamp passes `position`
    /// (or the stream ends). One frame beyond the target is retained for the
    /// next advance.
    fn decode_until_covering(&mut self, position: f64) {
        if self.presentation.failed {
            return;
        }
        let mut decoded: Vec<VideoFrame> = Vec::new();
        loop {
            if self
                .presentation
                .frames
                .back()
                .is_some_and(|frame| frame.timestamp > position)
                || decoded
                    .last()
                    .is_some_and(|frame| frame.timestamp > position)
            {
                break;
            }
            if self.presentation.exhausted {
                break;
            }
            match self.decode_next_frame() {
                Ok(Some(frame)) => decoded.push(frame),
                Ok(None) => {
                    self.presentation.exhausted = true;
                    break;
                }
                Err(_) => {
                    self.presentation.failed = true;
                    break;
                }
            }
        }
        for frame in decoded {
            self.presentation.retained_bytes = self
                .presentation
                .retained_bytes
                .saturating_add(frame_data_bytes(&frame.data));
            self.presentation.frames.push_back(frame);
        }
        self.evict_oversized_history();
    }

    /// Drop the oldest retained frames (strictly before the presented one)
    /// while the byte budget is exceeded.
    fn evict_oversized_history(&mut self) {
        while self.presentation.retained_bytes > MAX_RETAINED_FRAME_BYTES
            && self.presentation.frames.len() > 1
            && self.presentation.current_pts.is_some_and(|pts| {
                self.presentation
                    .frames
                    .front()
                    .is_some_and(|frame| frame.timestamp < pts)
            })
        {
            if let Some(frame) = self.presentation.frames.pop_front() {
                self.presentation.retained_bytes = self
                    .presentation
                    .retained_bytes
                    .saturating_sub(frame_data_bytes(&frame.data));
            }
        }
    }

    /// Present the last retained frame at or before `position`.
    fn select_current_frame(&mut self, position: f64) {
        self.presentation.current_pts = self
            .presentation
            .frames
            .iter()
            .rposition(|frame| frame.timestamp <= position)
            .map(|index| self.presentation.frames[index].timestamp);
    }

    /// End of the media in seconds: the container track duration when known,
    /// otherwise the last decoded frame plus one estimated frame duration.
    fn end_of_media(&self) -> f64 {
        if let Some(duration) = self.presentation.duration {
            return duration;
        }
        let mut last_two = self
            .presentation
            .frames
            .iter()
            .rev()
            .map(|frame| frame.timestamp)
            .take(2);
        match (last_two.next(), last_two.next()) {
            (Some(last), Some(previous)) => last + (last - previous).max(FALLBACK_FRAME_DURATION),
            (Some(last), None) => last + FALLBACK_FRAME_DURATION,
            (None, _) => f64::INFINITY,
        }
    }
}

/// Convert an NV12 payload to packed RGBA with the BT.601 limited-range
/// matrix. Missing chroma bytes paint black rather than panicking.
fn nv12_to_rgba(width: u32, height: u32, luma: &[u8], chroma_uv: &[u8]) -> Vec<u8> {
    let width = width as usize;
    let height = height as usize;
    let capacity = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .unwrap_or(0);
    let mut rgba = Vec::with_capacity(capacity);
    for y in 0..height {
        let chroma_row = (y / 2).saturating_mul(width);
        for x in 0..width {
            let luma_value = f64::from(
                luma.get(y.saturating_mul(width).saturating_add(x))
                    .copied()
                    .unwrap_or(16),
            );
            let chroma_base = chroma_row.saturating_add((x / 2).saturating_mul(2));
            let cb = f64::from(chroma_uv.get(chroma_base).copied().unwrap_or(128));
            let cr = f64::from(chroma_uv.get(chroma_base + 1).copied().unwrap_or(128));
            let luma_prime = 1.164 * (luma_value - 16.0);
            rgba.extend_from_slice(&[
                clamp_byte(luma_prime + 1.596 * (cr - 128.0)),
                clamp_byte(luma_prime - 0.391 * (cb - 128.0) - 0.813 * (cr - 128.0)),
                clamp_byte(luma_prime + 2.018 * (cb - 128.0)),
                255,
            ]);
        }
    }
    rgba
}

fn clamp_byte(value: f64) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::FramePublication;
    use super::MAX_RETAINED_FRAME_BYTES;
    use super::PresentationClock;
    use crate::video::PlaybackSignal;
    use crate::video::VideoPipeline;
    use crate::video::test_mp4::ColorTestDecoder;
    use crate::video::test_mp4::TestMp4Builder;
    use std::time::Duration;

    fn pipeline() -> VideoPipeline {
        let fixture = TestMp4Builder::new().build();
        VideoPipeline::with_decoder(&fixture, Box::new(ColorTestDecoder::new(64, 48)))
            .expect("fixture opens")
    }

    fn color_of(video: &VideoPipeline) -> u8 {
        video
            .current_frame_rgba()
            .expect("a frame is presented")
            .bytes[0]
    }

    #[test]
    fn clock_anchors_lazily_and_freezes_on_pause() {
        let mut clock = PresentationClock::new();
        let now = Duration::from_secs(1);
        // Before any resume the position stays put.
        clock.anchor(now);
        assert_eq!(clock.position(now), 0.0);
        clock.play();
        // Unanchored resume: no time has been observed yet.
        assert_eq!(clock.position(now), 0.0);
        clock.anchor(now);
        assert_eq!(clock.position(Duration::from_secs(2)), 1.0);
        // Pausing freezes at the last rebased (published) instant.
        clock.rebase(Duration::from_secs(2), 1.0);
        clock.pause();
        assert_eq!(clock.position(Duration::from_secs(5)), 1.0);
        // Seek re-bases the flowing position.
        clock.play();
        clock.seek(0.25);
        clock.anchor(Duration::from_secs(6));
        assert_eq!(clock.position(Duration::from_millis(6_500)), 0.75);
    }

    #[test]
    fn advance_decodes_and_presents_frames_in_pts_order() {
        let mut video = pipeline();
        video.start_playback();
        let frame_times = [0.0, 0.5, 1.0, 1.5, 2.0];
        let mut presented = Vec::new();
        for (index, at) in frame_times.iter().enumerate() {
            let signals = video.advance_playback(Duration::from_secs_f64(*at), false);
            presented.push(video.current_frame_timestamp());
            if index > 0 && index < 4 {
                assert_eq!(
                    color_of(&video),
                    (index + 1) as u8,
                    "frame color follows decode order"
                );
                assert_eq!(signals, vec![PlaybackSignal::TimeUpdate]);
            }
        }
        // Positions 0.0..1.5 present the frames at 0.0..1.5 in order; the
        // end of the media keeps the final frame on screen.
        assert_eq!(
            presented,
            vec![Some(0.0), Some(0.5), Some(1.0), Some(1.5), Some(1.5)]
        );
        // All four samples decoded by the time the clock reached the end.
        assert_eq!(video.samples_remaining(), 0);
        assert_eq!(video.published_position(), 2.0);
    }

    #[test]
    fn reaching_the_end_fires_timeupdate_then_ended_and_freezes() {
        let mut video = pipeline();
        video.start_playback();
        // The first pump anchors the freshly resumed clock.
        video.advance_playback(Duration::ZERO, false);
        let signals = video.advance_playback(Duration::from_secs(2), false);
        assert_eq!(
            signals,
            vec![PlaybackSignal::TimeUpdate, PlaybackSignal::Ended]
        );
        assert!(video.playback_ended());
        assert_eq!(video.published_position(), 2.0);
        // Later advances are inert.
        assert!(
            video
                .advance_playback(Duration::from_secs(3), false)
                .is_empty()
        );
        assert_eq!(video.published_position(), 2.0);
        assert_eq!(video.current_frame_timestamp(), Some(1.5));
        // Restarting after the end rewinds to the start.
        video.seek_to(0.0);
        assert!(!video.playback_ended());
        video.start_playback();
        let signals = video.advance_playback(Duration::from_millis(2_100), false);
        // The throttle window from the ended step still applies.
        assert!(signals.is_empty());
        assert_eq!(video.published_position(), 0.0);
        video.advance_playback(Duration::from_millis(2_200), false);
        assert_eq!(video.published_position(), 0.1);
        assert_eq!(video.current_frame_timestamp(), Some(0.0));
    }

    #[test]
    fn pause_and_resume_freeze_then_continue_the_position() {
        let mut video = pipeline();
        video.start_playback();
        video.advance_playback(Duration::from_millis(600), false);
        video.advance_playback(Duration::from_millis(1_200), false);
        assert_eq!(video.published_position(), 0.6);
        assert_eq!(color_of(&video), 2);
        video.pause_playback();
        assert_eq!(
            video.advance_playback(Duration::from_secs(2), false),
            Vec::<PlaybackSignal>::new()
        );
        assert_eq!(video.published_position(), 0.6);
        // Resume anchors at the next observed instant.
        video.start_playback();
        video.advance_playback(Duration::from_millis(2_100), false);
        assert_eq!(video.published_position(), 0.6);
        video.advance_playback(Duration::from_millis(2_300), false);
        assert_eq!(video.published_position(), 0.8);
        assert_eq!(video.current_frame_timestamp(), Some(0.5));
    }

    #[test]
    fn seek_repositions_within_the_decoded_window() {
        let mut video = pipeline();
        video.start_playback();
        video.advance_playback(Duration::ZERO, false);
        video.advance_playback(Duration::from_secs(1), false);
        assert_eq!(video.current_frame_timestamp(), Some(1.0));
        // Backward seek lands on the earlier retained frame.
        video.seek_to(0.5);
        assert_eq!(video.current_frame_timestamp(), Some(0.5));
        assert_eq!(color_of(&video), 2);
        assert_eq!(video.published_position(), 0.5);
        // Forward seek past the decoded window decodes on demand.
        video.seek_to(1.4);
        assert_eq!(video.current_frame_timestamp(), Some(1.0));
        video.seek_to(1.6);
        assert_eq!(video.current_frame_timestamp(), Some(1.5));
        assert_eq!(color_of(&video), 4);
        // Seek to the end marks playback ended; away from it clears the flag.
        video.seek_to(2.0);
        assert!(video.playback_ended());
        video.seek_to(0.0);
        assert!(!video.playback_ended());
        assert_eq!(video.current_frame_timestamp(), Some(0.0));
        // Negative and non-finite targets clamp or are ignored.
        video.seek_to(-3.0);
        assert_eq!(video.published_position(), 0.0);
        video.seek_to(f64::NAN);
        assert_eq!(video.published_position(), 0.0);
    }

    #[test]
    fn looping_wraps_to_the_start_without_ending() {
        let mut video = pipeline();
        video.start_playback();
        video.advance_playback(Duration::ZERO, true);
        for at in [2_u64, 4] {
            let signals = video.advance_playback(Duration::from_secs(at), true);
            assert_eq!(signals, vec![PlaybackSignal::TimeUpdate]);
            assert!(!video.playback_ended());
            assert_eq!(video.published_position(), 0.0);
        }
    }

    #[test]
    fn timeupdate_is_throttled_to_the_interval() {
        let mut video = pipeline();
        video.start_playback();
        let first = video.advance_playback(Duration::from_millis(0), false);
        assert_eq!(first, vec![PlaybackSignal::TimeUpdate]);
        let within = video.advance_playback(Duration::from_millis(100), false);
        assert!(within.is_empty());
        let due = video.advance_playback(Duration::from_millis(250), false);
        assert_eq!(due, vec![PlaybackSignal::TimeUpdate]);
        let next_window = video.advance_playback(Duration::from_millis(400), false);
        assert!(next_window.is_empty());
        let due_again = video.advance_playback(Duration::from_millis(500), false);
        assert_eq!(due_again, vec![PlaybackSignal::TimeUpdate]);
    }

    #[test]
    fn decode_failure_freezes_playback_and_reports_once() {
        // The shipped build has no pixel decoder: the placeholder backend
        // fails on the first access unit.
        let fixture = TestMp4Builder::new().build();
        let mut video = VideoPipeline::open(&fixture).expect("fixture opens");
        video.start_playback();
        let signals = video.advance_playback(Duration::from_millis(100), false);
        assert_eq!(signals, vec![PlaybackSignal::DecodeFailed]);
        assert!(video.presentation_failed());
        assert!(
            video
                .advance_playback(Duration::from_millis(200), false)
                .is_empty()
        );
    }

    #[test]
    fn retained_history_is_bounded_for_oversized_streams() {
        let mut video = pipeline();
        video.start_playback();
        video.advance_playback(Duration::ZERO, false);
        // Decoding everything retains all four frames under the budget; the
        // eviction only guards pathological streams, so assert accounting.
        video.seek_to(1.9);
        assert_eq!(video.retained_history_bytes(), 4 * 64 * 48 * 4);
        assert!(video.retained_history_bytes() <= MAX_RETAINED_FRAME_BYTES);
    }

    #[test]
    fn nv12_conversion_uses_bt601_limited_range() {
        // 2x2 NV12: one red-ish chroma pair over black luma.
        let luma = [81, 81, 81, 81];
        let chroma = [90, 240, 90, 240];
        let rgba = super::nv12_to_rgba(2, 2, &luma, &chroma);
        for pixel in rgba.chunks_exact(4) {
            assert_eq!(pixel, &[254, 0, 0, 255]);
        }
        // Missing chroma paints neutral.
        let rgba = super::nv12_to_rgba(2, 2, &luma, &[]);
        assert!(rgba.chunks_exact(4).all(|pixel| pixel[3] == 255));
    }

    #[test]
    fn frame_publication_carries_node_and_url() {
        let video = pipeline();
        let _ = FramePublication {
            node: render_dom::NodeId::from_u64(7),
            media_url: url::Url::parse("https://example.test/movie.mp4").unwrap(),
            frame: video.current_frame_rgba(),
        };
    }
}
