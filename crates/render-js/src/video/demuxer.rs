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

//! MP4 (ISO base media file format) demuxing down to one H.264 video track.
//!
//! The container walk is delegated to Mozilla's audited `mp4parse` crate;
//! this module selects the first decodable H.264 track and turns its sample
//! tables (`stsz`/`stco`/`stsc`/`stts`/`stss`/`ctts`) into absolute sample
//! locations and timestamps inside the original byte buffer. Progressive
//! files are handled by the same path: a fully-buffered byte range is the
//! phase-1 input contract (range-request streaming is a later phase).

use std::collections::BTreeSet;
use std::io::Cursor;

use crate::video::VideoError;
use crate::video::avc::AvcDecoderConfig;
use crate::video::avc::parse_avc_decoder_config;

/// Codec identity of the demuxed track. Phase 1 only recognizes H.264.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoCodec {
    /// ITU H.264 / MPEG-4 AVC (`avc1` sample entries).
    H264,
}

/// Static description of the selected video track.
#[derive(Clone, Debug, PartialEq)]
pub struct VideoTrackInfo {
    /// Codec of the selected track.
    pub codec: VideoCodec,
    /// Media timescale in ticks per second (from the track media header).
    pub timescale: u64,
    /// Total media duration in seconds when the container reports one.
    pub duration_seconds: Option<f64>,
    /// Coded width in pixels (sample description, then track header).
    pub width: u32,
    /// Coded height in pixels (sample description, then track header).
    pub height: u32,
    /// Number of samples (frames) in the track.
    pub sample_count: usize,
}

/// One decoded-input sample: its byte range in the container plus timing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VideoSample {
    /// Byte offset of the sample payload inside the container buffer.
    pub offset: u64,
    /// Payload length in bytes.
    pub size: u32,
    /// Decode timestamp in seconds (cumulative `stts` deltas).
    pub decode_timestamp: f64,
    /// Presentation timestamp in seconds (decode timestamp plus `ctts`
    /// composition offset).
    pub composition_timestamp: f64,
    /// Whether `stss` marks this sample a sync (key) sample; without `stss`
    /// every sample is sync.
    pub keyframe: bool,
}

/// A demuxed H.264 video track: metadata, decoder configuration, and the
/// sample table resolved to absolute offsets in the container buffer.
#[derive(Clone, Debug, PartialEq)]
pub struct DemuxedTrack {
    /// Track-level metadata surfaced to script and the pipeline.
    pub info: VideoTrackInfo,
    /// Parsed `avcC` decoder configuration (SPS/PPS, NAL length size).
    pub avc_config: AvcDecoderConfig,
    /// Samples in decode order with absolute offsets and timestamps.
    pub samples: Vec<VideoSample>,
}

/// Demux `bytes` and select its first playable H.264 video track.
///
/// # Errors
///
/// [`VideoError::Container`] when the bytes are not a parsable MP4 or carry
/// no video track, and [`VideoError::UnsupportedCodec`] when the first video
/// track is not H.264, and [`VideoError::Bitstream`] when its `avcC` record
/// cannot be parsed.
pub fn demux_h264_track(bytes: &[u8]) -> Result<DemuxedTrack, VideoError> {
    let mut cursor = Cursor::new(bytes);
    let context = mp4parse::read_mp4(&mut cursor)
        .map_err(|error| VideoError::Container(format!("MP4 parse failed: {error}")))?;
    let track = context
        .tracks
        .iter()
        .find(|track| track.track_type == mp4parse::TrackType::Video)
        .ok_or_else(|| VideoError::Container("MP4 contains no video track".to_owned()))?;

    let sample_description = track.stsd.as_ref().ok_or_else(|| {
        VideoError::Container("video track has no sample description box".to_owned())
    })?;
    let mut avc_record = None;
    let mut entry_size = (0_u32, 0_u32);
    for description in &sample_description.descriptions {
        if let mp4parse::SampleEntry::Video(entry) = description {
            if let mp4parse::VideoCodecSpecific::AVCConfig(record) = &entry.codec_specific {
                avc_record = Some(record);
                entry_size = (u32::from(entry.width), u32::from(entry.height));
                break;
            }
            return Err(VideoError::UnsupportedCodec(format!(
                "video track codec {codec:?} is not H.264",
                codec = entry.codec_type
            )));
        }
    }
    let avc_record = avc_record.ok_or_else(|| {
        VideoError::Container("video sample description carries no avcC record".to_owned())
    })?;
    let avc_config = parse_avc_decoder_config(avc_record)?;

    let timescale = track.timescale.map_or(0, |scale| scale.0);
    if timescale == 0 {
        return Err(VideoError::Container(
            "video track has no media timescale".to_owned(),
        ));
    }
    let samples = build_sample_table(track, timescale)?;
    if samples.is_empty() {
        return Err(VideoError::Container(
            "video track sample table lists no samples".to_owned(),
        ));
    }

    // Coded dimensions come from the sample description first, then the
    // track header (fixed-point 16.16), then the sequence parameter set.
    let sps = avc_config
        .sequence_parameters
        .first()
        .and_then(|nal| crate::video::avc::parse_seq_parameter_set(nal).ok());
    let (tkhd_width, tkhd_height) = track.tkhd.as_ref().map_or((0_u32, 0_u32), |header| {
        (header.width >> 16, header.height >> 16)
    });
    let width = pick_dimension(entry_size.0, tkhd_width, sps.map_or(0, |sps| sps.width));
    let height = pick_dimension(entry_size.1, tkhd_height, sps.map_or(0, |sps| sps.height));

    let duration_seconds = track
        .duration
        .map(|duration| duration.0 as f64 / timescale as f64);
    let info = VideoTrackInfo {
        codec: VideoCodec::H264,
        timescale,
        duration_seconds,
        width,
        height,
        sample_count: samples.len(),
    };
    Ok(DemuxedTrack {
        info,
        avc_config,
        samples,
    })
}

fn pick_dimension(sample_description: u32, track_header: u32, sps: u32) -> u32 {
    [sample_description, track_header, sps]
        .into_iter()
        .find(|dimension| *dimension > 0)
        .unwrap_or(0)
}

/// Resolve `stsz`/`stco`/`stsc`/`stts`/`stss`/`ctts` into absolute sample
/// locations with decode and composition timestamps.
fn build_sample_table(
    track: &mp4parse::Track,
    timescale: u64,
) -> Result<Vec<VideoSample>, VideoError> {
    let sizes = track
        .stsz
        .as_ref()
        .ok_or_else(|| VideoError::Container("video track has no sample size box".to_owned()))?;
    let offsets = track
        .stco
        .as_ref()
        .ok_or_else(|| VideoError::Container("video track has no chunk offset box".to_owned()))?;
    let sample_table = track.stsc.as_ref().ok_or_else(|| {
        VideoError::Container("video track has no sample-to-chunk box".to_owned())
    })?;
    let deltas = track
        .stts
        .as_ref()
        .ok_or_else(|| VideoError::Container("video track has no time-to-sample box".to_owned()))?;

    let total_samples: usize = deltas
        .samples
        .iter()
        .map(|run| usize::try_from(run.sample_count).unwrap_or(usize::MAX))
        .sum();
    let sample_sizes: Vec<u32> = if sizes.sample_size != 0 {
        vec![sizes.sample_size; total_samples]
    } else {
        sizes
            .sample_sizes
            .iter()
            .take(total_samples)
            .copied()
            .collect()
    };
    if sample_sizes.len() < total_samples {
        return Err(VideoError::Container(
            "video track sample size table is shorter than its timing table".to_owned(),
        ));
    }

    // Sample-to-chunk: each entry covers chunks from its (one-based)
    // `first_chunk` up to the next entry's `first_chunk - 1`.
    let chunk_offsets: &[u64] = &offsets.offsets;
    let mut samples = Vec::with_capacity(total_samples);
    let mut sample_index = 0_usize;
    for entry_position in 0..sample_table.samples.len() {
        let entry = &sample_table.samples[entry_position];
        let first_chunk = usize::try_from(entry.first_chunk)
            .ok()
            .and_then(|first| first.checked_sub(1))
            .ok_or_else(|| {
                VideoError::Container("sample-to-chunk first_chunk is zero".to_owned())
            })?;
        let run_end =
            sample_table
                .samples
                .get(entry_position + 1)
                .map_or(chunk_offsets.len(), |next| {
                    usize::try_from(next.first_chunk)
                        .unwrap_or(chunk_offsets.len() + 1)
                        .saturating_sub(1)
                        .min(chunk_offsets.len())
                });
        let samples_per_chunk = usize::try_from(entry.samples_per_chunk).unwrap_or(usize::MAX);
        let mut chunk_index = first_chunk;
        while chunk_index < run_end && sample_index < total_samples {
            let Some(chunk_offset) = chunk_offsets.get(chunk_index).copied() else {
                return Err(VideoError::Container(
                    "video track declares more chunks than the chunk offset box holds".to_owned(),
                ));
            };
            let mut payload_offset = chunk_offset;
            for _ in 0..samples_per_chunk {
                if sample_index >= total_samples {
                    break;
                }
                let size = sample_sizes[sample_index];
                samples.push(VideoSample {
                    offset: payload_offset,
                    size,
                    decode_timestamp: 0.0,
                    composition_timestamp: 0.0,
                    keyframe: false,
                });
                payload_offset += u64::from(size);
                sample_index += 1;
            }
            chunk_index += 1;
        }
        if sample_index >= total_samples {
            break;
        }
    }
    if samples.len() != total_samples {
        return Err(VideoError::Container(
            "video track chunk table does not cover every sample".to_owned(),
        ));
    }

    // Decode timestamps: cumulative `stts` deltas.
    let mut tick = 0_u64;
    let mut sample_position = 0_usize;
    for run in &deltas.samples {
        for _ in 0..run.sample_count {
            let Some(sample) = samples.get_mut(sample_position) else {
                break;
            };
            sample.decode_timestamp = tick as f64 / timescale as f64;
            tick += u64::from(run.sample_delta);
            sample_position += 1;
        }
    }

    // Composition offsets (`ctts`): version 0 stores unsigned offsets,
    // version 1 signed ones.
    if let Some(composition) = &track.ctts {
        let mut sample_position = 0_usize;
        for run in &composition.samples {
            for _ in 0..run.sample_count {
                let Some(sample) = samples.get_mut(sample_position) else {
                    break;
                };
                let offset_ticks = match run.time_offset {
                    mp4parse::TimeOffsetVersion::Version0(offset) => f64::from(offset),
                    mp4parse::TimeOffsetVersion::Version1(offset) => f64::from(offset),
                };
                sample.composition_timestamp =
                    sample.decode_timestamp + offset_ticks / timescale as f64;
                sample_position += 1;
            }
        }
    } else {
        for sample in &mut samples {
            sample.composition_timestamp = sample.decode_timestamp;
        }
    }

    // Sync samples (`stss`, one-based): absent means every sample is sync.
    let sync_samples: BTreeSet<u64> = track.stss.as_ref().map_or_else(BTreeSet::new, |sync| {
        sync.samples
            .iter()
            .map(|number| u64::from(*number))
            .collect()
    });
    if sync_samples.is_empty() {
        for sample in &mut samples {
            sample.keyframe = true;
        }
    } else {
        for (position, sample) in samples.iter_mut().enumerate() {
            let number = u64::try_from(position).unwrap_or(u64::MAX) + 1;
            sample.keyframe = sync_samples.contains(&number);
        }
    }
    Ok(samples)
}

#[cfg(test)]
mod tests {
    use crate::video::VideoError;
    use crate::video::demuxer::demux_h264_track;
    use crate::video::test_mp4::TestMp4Builder;

    #[test]
    fn demuxes_hand_built_progressive_mp4() {
        let fixture = TestMp4Builder::new().build();
        let track = demux_h264_track(&fixture).expect("fixture demuxes");
        assert_eq!(track.info.timescale, 1_000);
        assert_eq!(track.info.duration_seconds, Some(2.0));
        assert_eq!(track.info.width, 64);
        assert_eq!(track.info.height, 48);
        assert_eq!(track.info.sample_count, 4);
        assert_eq!(track.avc_config.nal_length_size, 4);
        assert_eq!(track.samples.len(), 4);
        let timestamps: Vec<f64> = track
            .samples
            .iter()
            .map(|sample| sample.decode_timestamp)
            .collect();
        assert_eq!(timestamps, vec![0.0, 0.5, 1.0, 1.5]);
        assert_eq!(
            track
                .samples
                .iter()
                .map(|sample| sample.composition_timestamp)
                .collect::<Vec<_>>(),
            timestamps
        );
        assert_eq!(
            track
                .samples
                .iter()
                .map(|sample| sample.keyframe)
                .collect::<Vec<_>>(),
            vec![true, false, false, false]
        );
        // Sizes and absolute offsets line up with the built mdat layout:
        // chunk 1 holds samples 1-2 back to back, then a 4-byte gap, then
        // chunk 2 holds samples 3-4.
        assert_eq!(
            track
                .samples
                .iter()
                .map(|sample| sample.size)
                .collect::<Vec<_>>(),
            vec![40, 44, 36, 48]
        );
        let mdat_start = track.samples[0].offset;
        assert_eq!(track.samples[1].offset, mdat_start + 40);
        assert_eq!(track.samples[2].offset, mdat_start + 40 + 44 + 4);
        assert_eq!(track.samples[3].offset, mdat_start + 40 + 44 + 4 + 36);
    }

    #[test]
    fn applies_composition_offsets_from_ctts() {
        let fixture = TestMp4Builder::new()
            .with_composition_offsets(vec![(2, 125), (2, 0)])
            .build();
        let track = demux_h264_track(&fixture).expect("fixture demuxes");
        assert_eq!(
            track
                .samples
                .iter()
                .map(|sample| sample.composition_timestamp)
                .collect::<Vec<_>>(),
            vec![0.125, 0.625, 1.0, 1.5]
        );
    }

    #[test]
    fn honors_explicit_chunk_boundaries() {
        // Two entries: chunk 1 carries one sample, chunk 2 carries two, and
        // the remaining sample lands in chunk 3. The inter-chunk gap makes
        // the chunk boundaries visible in the resolved offsets.
        let fixture = TestMp4Builder::new()
            .with_sample_to_chunks(vec![(1, 1), (2, 2)])
            .build();
        let track = demux_h264_track(&fixture).expect("fixture demuxes");
        assert_eq!(track.samples.len(), 4);
        let start = track.samples[0].offset;
        assert_eq!(track.samples[1].offset, start + 40 + 4);
        assert_eq!(track.samples[2].offset, start + 40 + 4 + 44);
        assert_eq!(track.samples[3].offset, start + 40 + 4 + 44 + 36 + 4);
    }

    #[test]
    fn rejects_non_media_and_truncated_bytes() {
        assert!(matches!(
            demux_h264_track(b"not an mp4 at all"),
            Err(VideoError::Container(_))
        ));
        assert!(matches!(
            demux_h264_track(&[]),
            Err(VideoError::Container(_))
        ));
    }
}
