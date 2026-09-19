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
    clippy::needless_pass_by_value,
    clippy::too_many_lines,
    clippy::trivially_copy_pass_by_ref,
    clippy::unused_self,
    clippy::wrong_self_convention
)]

//! Test-only MP4 construction and a deterministic decoder backend.
//!
//! No checked-in binary fixture and no ffmpeg dependency: tests build a
//! minimal progressive MP4 (H.264 video track only) byte by byte, which
//! keeps every offset, size, and timestamp visible in the test itself. The
//! `ColorTestDecoder` backend stands in for the not-yet-wired H.264 pixel
//! decoder and emits one solid-color RGBA frame per access unit, enough to
//! exercise pipeline ordering, timestamps, and queue behavior.

use crate::video::FrameData;
use crate::video::VideoDecoder;
use crate::video::VideoError;
use crate::video::avc::ANNEX_B_START_CODE;
use crate::video::avc::AvcDecoderConfig;
use crate::video::avc::test_support::BitWriter;
use crate::video::decoder::DecodedPicture;

/// Builder for a minimal progressive MP4 with one H.264 video track.
#[derive(Clone, Debug)]
pub(crate) struct TestMp4Builder {
    /// Byte length of every sample, in decode order.
    sample_sizes: Vec<u32>,
    /// Constant `stts` sample delta in timescale ticks.
    sample_delta: u32,
    /// Media timescale (ticks per second).
    timescale: u32,
    /// One-based sync sample numbers; an empty vec omits `stss`.
    sync_samples: Vec<u32>,
    /// `ctts` runs as (`sample_count`, unsigned offset).
    composition_offsets: Vec<(u32, u32)>,
    /// `stsc` entries as (`first_chunk`, `samples_per_chunk`).
    sample_to_chunks: Vec<(u32, u32)>,
    /// Padding bytes inserted between chunks so chunk-boundary handling is
    /// observable through sample offsets.
    chunk_gap: usize,
    /// Extra bytes the `stsz` table claims for the last sample without the
    /// payload carrying them, to exercise sample-overrun rejection.
    last_sample_inflation: usize,
}

impl TestMp4Builder {
    /// Four 64x48 samples, 0.5 s apart on a 1000 Hz timescale, first sample
    /// sync, two chunks of two samples.
    pub(crate) fn new() -> Self {
        Self {
            sample_sizes: vec![40, 44, 36, 48],
            sample_delta: 500,
            timescale: 1_000,
            sync_samples: vec![1],
            composition_offsets: Vec::new(),
            sample_to_chunks: vec![(1, 2)],
            chunk_gap: 4,
            last_sample_inflation: 0,
        }
    }

    /// Make the `stsz` table declare `extra` more bytes for the last sample
    /// than the mdat payload carries.
    pub(crate) fn with_last_sample_inflation(mut self, extra: usize) -> Self {
        self.last_sample_inflation = extra;
        self
    }

    /// Override the `ctts` runs.
    pub(crate) fn with_composition_offsets(mut self, runs: Vec<(u32, u32)>) -> Self {
        self.composition_offsets = runs;
        self
    }

    /// Override the `stsc` entries.
    pub(crate) fn with_sample_to_chunks(mut self, entries: Vec<(u32, u32)>) -> Self {
        self.sample_to_chunks = entries;
        self
    }

    /// Serialize the complete MP4: `ftyp` + `moov` + `mdat`. Chunk offsets
    /// are laid out relative to the `mdat` payload first, then rewritten as
    /// absolute file offsets once the `ftyp`+`moov` prefix size is known
    /// (the `stco` length only depends on the chunk count, so a second pass
    /// is stable).
    pub(crate) fn build(&self) -> Vec<u8> {
        let layout = self.chunk_layout();
        let ftyp = self.build_ftyp();
        let moov_relative = self.build_moov(&layout.chunk_offsets);
        let mdat_data_start = (ftyp.len() + moov_relative.len() + 8) as u64;
        let absolute_offsets: Vec<u64> = layout
            .chunk_offsets
            .iter()
            .map(|offset| offset + mdat_data_start)
            .collect();
        let mut bytes = Vec::with_capacity(mdat_data_start as usize + layout.total_bytes);
        bytes.extend_from_slice(&ftyp);
        bytes.extend_from_slice(&self.build_moov(&absolute_offsets));
        bytes.extend_from_slice(&((layout.total_bytes as u32) + 8).to_be_bytes());
        bytes.extend_from_slice(b"mdat");
        bytes.extend_from_slice(&layout.payload);
        bytes
    }

    /// Expand the `stsc` entries into per-chunk sample index lists, then lay
    /// chunks out back to back with [`Self::chunk_gap`] padding between them.
    fn chunk_layout(&self) -> ChunkLayout {
        let total = self.sample_sizes.len();
        let mut chunks: Vec<Vec<usize>> = Vec::new();
        let mut sample_index = 0usize;
        for (position, &(first_chunk, samples_per_chunk)) in
            self.sample_to_chunks.iter().enumerate()
        {
            let first_chunk = usize::try_from(first_chunk).expect("chunk number fits");
            let samples_per_chunk = usize::try_from(samples_per_chunk).expect("samples fit");
            let run_end_exclusive = self
                .sample_to_chunks
                .get(position + 1)
                .map_or(usize::MAX, |&(next, _)| {
                    usize::try_from(next).expect("chunk number fits") - 1
                });
            let mut chunk_index = chunks.len().max(first_chunk - 1);
            while chunk_index < run_end_exclusive && sample_index < total {
                let mut samples_in_chunk = Vec::new();
                for _ in 0..samples_per_chunk {
                    if sample_index >= total {
                        break;
                    }
                    samples_in_chunk.push(sample_index);
                    sample_index += 1;
                }
                chunks.push(samples_in_chunk);
                chunk_index += 1;
            }
            if sample_index >= total {
                break;
            }
        }

        let mut payload = Vec::new();
        let mut chunk_offsets = Vec::with_capacity(chunks.len());
        let mut cursor = 0usize;
        for (number, samples) in chunks.iter().enumerate() {
            if number > 0 {
                cursor += self.chunk_gap;
                payload.resize(payload.len() + self.chunk_gap, 0);
            }
            chunk_offsets.push(cursor as u64);
            for &sample in samples {
                payload.extend_from_slice(&self.sample_bytes(sample));
                cursor += self.sample_sizes[sample] as usize;
            }
        }
        ChunkLayout {
            chunk_offsets,
            payload,
            total_bytes: cursor,
        }
    }

    /// Serialized bytes of one sample: a single AVCC-length-prefixed NAL
    /// unit (IDR slice for sample 0, non-IDR otherwise) with deterministic
    /// filler bytes.
    fn sample_bytes(&self, index: usize) -> Vec<u8> {
        let size = self.sample_sizes[index] as usize;
        let nal_length = size - 4;
        let mut sample = Vec::with_capacity(size);
        sample.extend_from_slice(&(nal_length as u32).to_be_bytes());
        sample.push(if index == 0 { 0x65 } else { 0x41 });
        while sample.len() < size {
            let position = sample.len();
            sample.push((index * 7 + position) as u8);
        }
        sample
    }

    fn build_ftyp(&self) -> Vec<u8> {
        box_bytes(
            b"ftyp",
            [
                b"isom".as_slice(),
                &0u32.to_be_bytes(),
                b"isom".as_slice(),
                b"iso2".as_slice(),
                b"avc1".as_slice(),
                b"mp41".as_slice(),
            ]
            .concat(),
        )
    }

    fn build_moov(&self, chunk_offsets: &[u64]) -> Vec<u8> {
        let total_duration: u32 = self.sample_delta * self.sample_sizes.len() as u32;
        let sps = synthetic_sps();
        let pps = synthetic_pps();
        let mut avcc = vec![1_u8, 66, 0xC0, 30, 0xFF, 0xE1];
        avcc.extend_from_slice(&(sps.len() as u16).to_be_bytes());
        avcc.extend_from_slice(&sps);
        avcc.push(1);
        avcc.extend_from_slice(&(pps.len() as u16).to_be_bytes());
        avcc.extend_from_slice(&pps);

        let mut chunk_offsets_payload = Vec::new();
        chunk_offsets_payload.extend_from_slice(&(chunk_offsets.len() as u32).to_be_bytes());
        for &offset in chunk_offsets {
            // `stco` carries 32-bit offsets; `co64` would be the 64-bit form.
            let offset = u32::try_from(offset).expect("test fixture offsets fit u32");
            chunk_offsets_payload.extend_from_slice(&offset.to_be_bytes());
        }

        let stsd_avc1 = box_bytes(
            b"avc1",
            [
                &[0_u8; 6][..],
                &1_u16.to_be_bytes(),
                &0_u16.to_be_bytes(),
                &[0_u8; 2],
                &[0_u8; 12],
                &64_u16.to_be_bytes(),
                &48_u16.to_be_bytes(),
                &0x0048_0000_u32.to_be_bytes(),
                &0x0048_0000_u32.to_be_bytes(),
                &0_u32.to_be_bytes(),
                &1_u16.to_be_bytes(),
                &[0_u8; 32],
                &0x0018_u16.to_be_bytes(),
                &0xFFFF_u16.to_be_bytes(),
                &box_bytes(b"avcC", avcc),
            ]
            .concat(),
        );

        let mut sample_to_chunks_payload = Vec::new();
        sample_to_chunks_payload
            .extend_from_slice(&(self.sample_to_chunks.len() as u32).to_be_bytes());
        for &(first_chunk, samples_per_chunk) in &self.sample_to_chunks {
            sample_to_chunks_payload.extend_from_slice(&first_chunk.to_be_bytes());
            sample_to_chunks_payload.extend_from_slice(&samples_per_chunk.to_be_bytes());
            sample_to_chunks_payload.extend_from_slice(&1_u32.to_be_bytes());
        }

        let mut declared_sizes = self.sample_sizes.clone();
        if let Some(last) = declared_sizes.last_mut() {
            *last += self.last_sample_inflation as u32;
        }
        let mut sample_sizes_payload = Vec::new();
        sample_sizes_payload.extend_from_slice(&0_u32.to_be_bytes());
        sample_sizes_payload.extend_from_slice(&(self.sample_sizes.len() as u32).to_be_bytes());
        for &size in &declared_sizes {
            sample_sizes_payload.extend_from_slice(&size.to_be_bytes());
        }

        let mut sync_samples_payload = Vec::new();
        sync_samples_payload.extend_from_slice(&(self.sync_samples.len() as u32).to_be_bytes());
        for &number in &self.sync_samples {
            sync_samples_payload.extend_from_slice(&number.to_be_bytes());
        }

        let stbl_children = [
            full_box_bytes(
                b"stsd",
                0,
                0,
                [&1_u32.to_be_bytes()[..], &stsd_avc1].concat(),
            ),
            full_box_bytes(
                b"stts",
                0,
                0,
                [
                    &1_u32.to_be_bytes()[..],
                    &(self.sample_sizes.len() as u32).to_be_bytes(),
                    &self.sample_delta.to_be_bytes(),
                ]
                .concat(),
            ),
            full_box_bytes(b"stss", 0, 0, sync_samples_payload),
            full_box_bytes(b"stsc", 0, 0, sample_to_chunks_payload),
            full_box_bytes(b"stsz", 0, 0, sample_sizes_payload),
            full_box_bytes(b"stco", 0, 0, chunk_offsets_payload),
        ];
        let mut stbl_children = stbl_children.to_vec();
        if !self.composition_offsets.is_empty() {
            let mut ctts_payload = Vec::new();
            ctts_payload.extend_from_slice(&(self.composition_offsets.len() as u32).to_be_bytes());
            for &(count, offset) in &self.composition_offsets {
                ctts_payload.extend_from_slice(&count.to_be_bytes());
                ctts_payload.extend_from_slice(&offset.to_be_bytes());
            }
            stbl_children.push(full_box_bytes(b"ctts", 0, 0, ctts_payload));
        }

        let dinf = box_bytes(
            b"dinf",
            [full_box_bytes(
                b"dref",
                0,
                0,
                [
                    &1_u32.to_be_bytes()[..],
                    &full_box_bytes(b"url ", 0, 1, Vec::new()),
                ]
                .concat(),
            )]
            .concat(),
        );
        let vmhd = full_box_bytes(
            b"vmhd",
            0,
            1,
            [
                &0_u16.to_be_bytes()[..],
                &0_u16.to_be_bytes(),
                &0_u16.to_be_bytes(),
                &0_u16.to_be_bytes(),
            ]
            .concat(),
        );
        let stbl = box_bytes(b"stbl", stbl_children.concat());
        let minf = box_bytes(b"minf", [vmhd, dinf, stbl].concat());
        let media_header = full_box_bytes(
            b"mdhd",
            0,
            0,
            [
                &0_u32.to_be_bytes()[..],
                &0_u32.to_be_bytes(),
                &self.timescale.to_be_bytes(),
                &total_duration.to_be_bytes(),
                &0x55C4_u16.to_be_bytes(),
                &0_u16.to_be_bytes(),
            ]
            .concat(),
        );
        let hdlr = full_box_bytes(
            b"hdlr",
            0,
            0,
            [
                &0_u32.to_be_bytes()[..],
                b"vide".as_slice(),
                &[0_u8; 12],
                b"VideoHandler\0".as_slice(),
            ]
            .concat(),
        );
        let mdia = box_bytes(b"mdia", [media_header, hdlr, minf].concat());
        let track_header = full_box_bytes(
            b"tkhd",
            0,
            3,
            [
                &0_u32.to_be_bytes()[..],
                &0_u32.to_be_bytes(),
                &1_u32.to_be_bytes(),
                &0_u32.to_be_bytes(),
                &total_duration.to_be_bytes(),
                &0_u64.to_be_bytes(),
                &0_u16.to_be_bytes(),
                &0_u16.to_be_bytes(),
                &0x0100_u16.to_be_bytes(),
                &0_u16.to_be_bytes(),
                &IDENTITY_MATRIX,
                &(64_u32 << 16).to_be_bytes(),
                &(48_u32 << 16).to_be_bytes(),
            ]
            .concat(),
        );
        let trak = box_bytes(b"trak", [track_header, mdia].concat());
        let movie_header = full_box_bytes(
            b"mvhd",
            0,
            0,
            [
                &0_u32.to_be_bytes()[..],
                &0_u32.to_be_bytes(),
                &self.timescale.to_be_bytes(),
                &total_duration.to_be_bytes(),
                &0x0001_0000_u32.to_be_bytes(),
                &0x0100_u16.to_be_bytes(),
                &0_u16.to_be_bytes(),
                &0_u64.to_be_bytes(),
                &IDENTITY_MATRIX,
                &[0_u8; 24],
                &2_u32.to_be_bytes(),
            ]
            .concat(),
        );
        box_bytes(b"moov", [movie_header, trak].concat())
    }
}

struct ChunkLayout {
    chunk_offsets: Vec<u64>,
    payload: Vec<u8>,
    total_bytes: usize,
}

/// Identity matrix for `tkhd`/`mvhd` (36 bytes, 16.16 fixed point).
const IDENTITY_MATRIX: [u8; 36] = {
    let mut matrix = [0_u8; 36];
    matrix[0] = 0x00;
    matrix[1] = 0x01;
    matrix[2] = 0x00;
    matrix[3] = 0x00;
    matrix[16] = 0x00;
    matrix[17] = 0x01;
    matrix[18] = 0x00;
    matrix[19] = 0x00;
    matrix[32] = 0x40;
    matrix[33] = 0x00;
    matrix[34] = 0x00;
    matrix[35] = 0x00;
    matrix
};

fn box_bytes(kind: &[u8; 4], payload: Vec<u8>) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(payload.len() + 8);
    bytes.extend_from_slice(&((payload.len() as u32) + 8).to_be_bytes());
    bytes.extend_from_slice(kind);
    bytes.extend_from_slice(&payload);
    bytes
}

fn full_box_bytes(kind: &[u8; 4], version: u8, flags: u32, payload: Vec<u8>) -> Vec<u8> {
    let mut payload_with_header = Vec::with_capacity(payload.len() + 4);
    payload_with_header.push(version);
    // Flags are a 24-bit field.
    payload_with_header.extend_from_slice(&flags.to_be_bytes()[1..4]);
    payload_with_header.extend_from_slice(&payload);
    box_bytes(kind, payload_with_header)
}

/// Baseline SPS declaring 64x48, bit-exact for the parser.
fn synthetic_sps() -> Vec<u8> {
    let mut writer = BitWriter::new();
    writer.push_bytes(&[0x67, 66, 0xC0, 30]);
    writer.push_ue(0); // seq_parameter_set_id
    writer.push_ue(0); // log2_max_frame_num_minus4
    writer.push_ue(0); // pic_order_cnt_type
    writer.push_ue(0); // log2_max_pic_order_cnt_lsb_minus4
    writer.push_ue(1); // max_num_ref_frames
    writer.push_bit(0); // gaps_in_frame_num_value_allowed_flag
    writer.push_ue(3); // pic_width_in_mbs_minus1 -> 64
    writer.push_ue(2); // pic_height_in_map_units_minus1 -> 48
    writer.push_bit(1); // frame_mbs_only_flag
    writer.push_bit(0); // direct_8x8_inference_flag
    writer.push_bit(0); // frame_cropping_flag
    writer.push_bit(0); // vui_parameters_present_flag
    writer.push_trailing();
    writer.into_bytes()
}

/// Minimal picture parameter set matching [`synthetic_sps`].
fn synthetic_pps() -> Vec<u8> {
    let mut writer = BitWriter::new();
    writer.push_bytes(&[0x68]);
    writer.push_ue(0); // pic_parameter_set_id
    writer.push_ue(0); // seq_parameter_set_id
    writer.push_bit(0); // entropy_coding_mode_flag (CAVLC)
    writer.push_bit(0); // bottom_field_pic_order_in_frame_present_flag
    writer.push_ue(0); // num_slice_groups_minus1
    writer.push_ue(0); // num_ref_idx_l0_default_active_minus1
    writer.push_ue(0); // num_ref_idx_l1_default_active_minus1
    writer.push_bit(0); // weighted_pred_flag
    writer.push_bits(2, 0); // weighted_bipred_idc
    writer.push_se(0); // pic_init_qp_minus26
    writer.push_se(0); // pic_init_qs_minus26
    writer.push_se(0); // chroma_qp_index_offset
    writer.push_bit(0); // deblocking_filter_control_present_flag
    writer.push_bit(0); // constrained_intra_pred_flag
    writer.push_bit(0); // redundant_pic_cnt_present_flag
    writer.push_trailing();
    writer.into_bytes()
}

/// Deterministic stand-in decoder: one solid-color RGBA frame per access
/// unit, colored by decode order (1, 2, 3, ...). Malformed Annex-B input
/// fails loudly so the pipeline's AVCC conversion is under test too.
#[derive(Clone, Debug)]
pub(crate) struct ColorTestDecoder {
    width: u32,
    height: u32,
    next_color: u8,
}

impl ColorTestDecoder {
    pub(crate) fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            next_color: 1,
        }
    }
}

impl VideoDecoder for ColorTestDecoder {
    fn configure(&mut self, config: &AvcDecoderConfig) -> Result<(), VideoError> {
        let sps = config
            .sequence_parameters
            .first()
            .ok_or_else(|| VideoError::Decode("test decoder requires an SPS".to_owned()))?;
        let parsed = crate::video::avc::parse_seq_parameter_set(sps)
            .map_err(|error| VideoError::Decode(error.to_string()))?;
        assert_eq!((parsed.width, parsed.height), (self.width, self.height));
        Ok(())
    }

    fn decode(
        &mut self,
        access_unit: &[u8],
        _keyframe: bool,
    ) -> Result<Vec<DecodedPicture>, VideoError> {
        // The pipeline must deliver Annex-B with parameter sets in-band, so
        // the first NAL is the SPS.
        assert!(access_unit.starts_with(&ANNEX_B_START_CODE));
        let first_nal_type = access_unit.get(4).map(|header| header & 0x1F).unwrap_or(0);
        assert_eq!(first_nal_type, 0x07, "SPS precedes each access unit");
        let color = self.next_color;
        self.next_color = self.next_color.wrapping_add(1).max(1);
        let pixel_count = (self.width * self.height) as usize;
        let mut bytes = vec![0_u8; pixel_count * 4];
        for pixel in bytes.chunks_exact_mut(4) {
            pixel[0] = color;
            pixel[1] = color.wrapping_add(64);
            pixel[2] = color.wrapping_add(128);
            pixel[3] = 255;
        }
        Ok(vec![DecodedPicture {
            data: FrameData::Rgba {
                width: self.width,
                height: self.height,
                bytes,
            },
        }])
    }
}
