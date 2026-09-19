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

//! H.264 (AVC) bitstream support: decoder-record parsing, NAL classification,
//! and sequence-parameter-set dimension extraction.
//!
//! This is the codec layer the demuxer feeds: an `avcC` record from the MP4
//! sample description becomes an [`AvcDecoderConfig`], whose SPS/PPS NAL
//! units are parsed far enough to recover the coded picture size used by
//! `videoWidth`/`videoHeight`. Pixel decoding itself lives behind the
//! [`crate::video::VideoDecoder`] trait (see the module documentation for the
//! phase split).

use crate::video::VideoError;

/// NAL unit type carried in the first byte of an H.264 NAL unit
/// (ISO 14496-10 §7.4.1): coded slice of a non-IDR picture.
pub const NAL_TYPE_SLICE_NON_IDR: u8 = 1;

/// NAL unit type: coded slice of an IDR (instant decoding refresh) picture.
pub const NAL_TYPE_SLICE_IDR: u8 = 5;

/// NAL unit type: sequence parameter set.
pub const NAL_TYPE_SPS: u8 = 7;

/// NAL unit type: picture parameter set.
pub const NAL_TYPE_PPS: u8 = 8;

/// The four-byte Annex-B start code prefix (`00 00 00 01`).
pub const ANNEX_B_START_CODE: [u8; 4] = [0x00, 0x00, 0x00, 0x01];

/// Profile idcs (ISO 14496-10 §7.3.2.1.1) whose SPS carries explicit
/// chroma/depth syntax instead of the baseline defaults.
const HIGH_PROFILE_IDCS: [u8; 13] = [100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135];

/// AVC decoder configuration record parsed from an `avcC` box
/// (ISO 14496-15 §5.3.3.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AvcDecoderConfig {
    /// `configurationVersion`; conforming records are `1`.
    pub configuration_version: u8,
    /// `AVCProfileIndication` from the record header.
    pub profile_idc: u8,
    /// `profile_compatibility` from the record header.
    pub profile_compatibility: u8,
    /// `AVCLevelIndication` from the record header.
    pub level_idc: u8,
    /// Length in bytes of the NAL unit length fields used by samples
    /// (`lengthSizeMinusOne + 1`); always 1..=4.
    pub nal_length_size: usize,
    /// Raw sequence parameter set NAL units, NAL header byte included.
    pub sequence_parameters: Vec<Vec<u8>>,
    /// Raw picture parameter set NAL units, NAL header byte included.
    pub picture_parameters: Vec<Vec<u8>>,
}

/// Coded picture geometry recovered from a sequence parameter set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeqParameterSet {
    /// `profile_idc` from the SPS header bytes.
    pub profile_idc: u8,
    /// `level_idc` from the SPS header bytes.
    pub level_idc: u8,
    /// `chroma_format_idc` (defaults to 1, 4:2:0, for baseline profiles).
    pub chroma_format_idc: u8,
    /// Coded width in pixels (`pic_width_in_mbs_minus1`-derived, cropped).
    pub width: u32,
    /// Coded height in pixels (`pic_height_in_map_units`-derived, cropped).
    pub height: u32,
    /// `frame_mbs_only_flag`: `false` means the stream may carry fields.
    pub frame_mbs_only: bool,
}

/// Parse an `avcC` decoder configuration record.
///
/// The record carries one byte-aligned SPS/PPS table exactly as it appears
/// in the MP4 sample description; NAL units keep their header bytes.
///
/// # Errors
///
/// [`VideoError::Bitstream`] when the record is truncated, its reserved
/// fields are not all-ones, or it declares NAL lengths outside 1..=4.
pub fn parse_avc_decoder_config(bytes: &[u8]) -> Result<AvcDecoderConfig, VideoError> {
    if bytes.len() < 7 {
        return Err(VideoError::Bitstream(format!(
            "avcC record is {} bytes; at least 7 required",
            bytes.len()
        )));
    }
    let configuration_version = bytes[0];
    if configuration_version != 1 {
        return Err(VideoError::Bitstream(format!(
            "unsupported avcC configurationVersion {configuration_version}"
        )));
    }
    let length_size_minus_one = bytes[4] & 0x03;
    let nal_length_size = usize::from(length_size_minus_one) + 1;
    if bytes[4] & !0x03 != 0xFC {
        return Err(VideoError::Bitstream(
            "avcC reserved length bits are not all ones".to_owned(),
        ));
    }
    let sps_count = usize::from(bytes[5] & 0x1F);
    if bytes[5] & !0x1F != 0xE0 {
        return Err(VideoError::Bitstream(
            "avcC reserved sequence bits are not all ones".to_owned(),
        ));
    }
    let mut cursor = 6usize;
    let mut sequence_parameters = Vec::new();
    for _ in 0..sps_count {
        let (nal, next) = read_length_prefixed(bytes, cursor)?;
        sequence_parameters.push(nal);
        cursor = next;
    }
    if cursor >= bytes.len() {
        return Err(VideoError::Bitstream(
            "avcC record ends before the picture parameter set count".to_owned(),
        ));
    }
    let pps_count = usize::from(bytes[cursor]);
    cursor += 1;
    let mut picture_parameters = Vec::new();
    for _ in 0..pps_count {
        let (nal, next) = read_length_prefixed(bytes, cursor)?;
        picture_parameters.push(nal);
        cursor = next;
    }
    Ok(AvcDecoderConfig {
        configuration_version,
        profile_idc: bytes[1],
        profile_compatibility: bytes[2],
        level_idc: bytes[3],
        nal_length_size,
        sequence_parameters,
        picture_parameters,
    })
}

fn read_length_prefixed(bytes: &[u8], start: usize) -> Result<(Vec<u8>, usize), VideoError> {
    if start + 2 > bytes.len() {
        return Err(VideoError::Bitstream(
            "avcC NAL length prefix is truncated".to_owned(),
        ));
    }
    let length = (u16::from(bytes[start]) << 8) | u16::from(bytes[start + 1]);
    let start = start + 2;
    let end = start + usize::from(length);
    if end > bytes.len() {
        return Err(VideoError::Bitstream(
            "avcC NAL unit is truncated".to_owned(),
        ));
    }
    Ok((bytes[start..end].to_vec(), end))
}

/// The `nal_unit_type` carried in the first byte of a NAL unit.
#[must_use]
pub fn nal_unit_type(nal: &[u8]) -> Option<u8> {
    nal.first().map(|header| header & 0x1F)
}

/// Remove `emulation prevention` bytes (`00 00 03`) from a NAL unit payload
/// so the underlying RBSP can be read bit by bit (ISO 14496-10 §7.3.1).
/// A `03` is only an emulation byte when it follows two zero bytes and is
/// itself followed by a byte value of at most `03` (or the unit's end).
#[must_use]
pub fn strip_emulation_prevention(bytes: &[u8]) -> Vec<u8> {
    let mut rbsp = Vec::with_capacity(bytes.len());
    let mut consecutive_zeroes = 0usize;
    let mut index = 0usize;
    while index < bytes.len() {
        let byte = bytes[index];
        let emulation = consecutive_zeroes >= 2
            && byte == 0x03
            && bytes.get(index + 1).is_none_or(|next| *next <= 0x03);
        if emulation {
            consecutive_zeroes = 0;
            index += 1;
            continue;
        }
        consecutive_zeroes = if byte == 0 {
            (consecutive_zeroes + 1).min(2)
        } else {
            0
        };
        rbsp.push(byte);
        index += 1;
    }
    rbsp
}

/// Parse a sequence parameter set NAL unit down to the coded picture size.
///
/// Only the prefix of the SPS up to `frame_mbs_only_flag` plus cropping is
/// interpreted; VUI and later syntax is skipped without validation, which is
/// enough to recover `videoWidth`/`videoHeight` for progressive H.264.
///
/// # Errors
///
/// [`VideoError::Bitstream`] when the NAL is not an SPS or its syntax is
/// truncated or unsupported.
pub fn parse_seq_parameter_set(nal: &[u8]) -> Result<SeqParameterSet, VideoError> {
    if nal_unit_type(nal) != Some(NAL_TYPE_SPS) {
        return Err(VideoError::Bitstream(format!(
            "NAL unit type {:?} is not a sequence parameter set",
            nal_unit_type(nal)
        )));
    }
    // The NAL header byte is not part of the RBSP.
    let rbsp = strip_emulation_prevention(nal.get(1..).unwrap_or(&[]));
    let mut reader = BitReader::new(&rbsp);
    let profile_idc = reader.read_u8().ok_or_else(|| {
        VideoError::Bitstream("sequence parameter set is truncated at profile_idc".to_owned())
    })?;
    let _constraint_flags = reader.read_u8().ok_or_else(|| {
        VideoError::Bitstream("sequence parameter set is truncated at constraint flags".to_owned())
    })?;
    let level_idc = reader.read_u8().ok_or_else(|| {
        VideoError::Bitstream("sequence parameter set is truncated at level_idc".to_owned())
    })?;
    let _seq_parameter_set_id = reader.read_ue()?;
    let mut chroma_format_idc = 1_u8;
    if HIGH_PROFILE_IDCS.contains(&profile_idc) {
        chroma_format_idc = u8::try_from(reader.read_ue()?.min(3)).unwrap_or(1);
        if chroma_format_idc == 3 {
            let _separate_colour_plane = reader.read_bit()?;
        }
        let _bit_depth_luma_minus8 = reader.read_ue()?;
        let _bit_depth_chroma_minus8 = reader.read_ue()?;
        let _qpprime_y_zero_transform_bypass = reader.read_bit()?;
        let seq_scaling_matrix_present = reader.read_bit()? != 0;
        if seq_scaling_matrix_present {
            return Err(VideoError::Bitstream(
                "sequence parameter set scaling lists are not supported".to_owned(),
            ));
        }
    }
    let _log2_max_frame_num_minus4 = reader.read_ue()?;
    let pic_order_cnt_type = reader.read_ue()?;
    match pic_order_cnt_type {
        0 => {
            let _log2_max_pic_order_cnt_lsb_minus4 = reader.read_ue()?;
        }
        1 => {
            let _delta_pic_order_always_zero = reader.read_bit()?;
            let _offset_for_non_ref_pic = reader.read_se()?;
            let _offset_for_top_to_bottom_field = reader.read_se()?;
            let cycle_count = reader.read_ue()?;
            if cycle_count > 255 {
                return Err(VideoError::Bitstream(
                    "sequence parameter set POC cycle is unreasonably large".to_owned(),
                ));
            }
            for _ in 0..cycle_count {
                let _offset_for_ref_frame = reader.read_se()?;
            }
        }
        2 => {}
        other => {
            return Err(VideoError::Bitstream(format!(
                "sequence parameter set pic_order_cnt_type {other} is invalid"
            )));
        }
    }
    let _max_num_ref_frames = reader.read_ue()?;
    let _gaps_in_frame_num_allowed = reader.read_bit()?;
    let pic_width_in_mbs_minus1 = reader.read_ue()?;
    let pic_height_in_map_units_minus1 = reader.read_ue()?;
    let frame_mbs_only = reader.read_bit()? != 0;
    if !frame_mbs_only {
        let _mb_adaptive_frame_field = reader.read_bit()?;
    }
    let _direct_8x8_inference = reader.read_bit()?;
    let frame_cropping = reader.read_bit()? != 0;
    let mut crop_left = 0_u32;
    let mut crop_right = 0_u32;
    let mut crop_top = 0_u32;
    let mut crop_bottom = 0_u32;
    if frame_cropping {
        crop_left = reader.read_ue()?;
        crop_right = reader.read_ue()?;
        crop_top = reader.read_ue()?;
        crop_bottom = reader.read_ue()?;
    }
    // Crop units follow ISO 14496-10 §7.4.2.2; the plane layout only depends
    // on the chroma format here because separate colour planes keep
    // ChromaArrayType at 0 for our purposes.
    let (crop_unit_x, crop_unit_y) = match chroma_format_idc {
        0 | 3 => (1_u32, 1_u32),
        1 => (2, 2),
        _ => (2, 1),
    };
    let width =
        (pic_width_in_mbs_minus1 + 1) * 16 - crop_left * crop_unit_x - crop_right * crop_unit_x;
    let height = ((2 - u32::from(frame_mbs_only)) * (pic_height_in_map_units_minus1 + 1) * 16)
        - crop_top * crop_unit_y
        - crop_bottom * crop_unit_y;
    Ok(SeqParameterSet {
        profile_idc,
        level_idc,
        chroma_format_idc,
        width,
        height,
        frame_mbs_only,
    })
}

/// Convert one AVCC-formatted access unit (4-byte or shorter big-endian
/// length prefixes per NAL unit) into an Annex-B byte stream.
///
/// `parameter_sets` are emitted (SPS then PPS) ahead of the sample's own
/// NAL units; decoders that did not see out-of-band configuration rely on
/// this in-band repetition, and repeating it on every sample is what real
/// demuxers do for `avcC`-format streams.
///
/// # Errors
///
/// [`VideoError::Bitstream`] when a NAL length prefix overruns the sample.
pub fn avcc_sample_to_annex_b(
    sample: &[u8],
    nal_length_size: usize,
    parameter_sets: &[&[u8]],
) -> Result<Vec<u8>, VideoError> {
    let mut annex_b = Vec::with_capacity(sample.len() + 8 * parameter_sets.len());
    for parameter_set in parameter_sets {
        annex_b.extend_from_slice(&ANNEX_B_START_CODE);
        annex_b.extend_from_slice(parameter_set);
    }
    let mut cursor = 0usize;
    while cursor < sample.len() {
        if cursor + nal_length_size > sample.len() {
            return Err(VideoError::Bitstream(
                "AVCC sample ends inside a NAL length prefix".to_owned(),
            ));
        }
        let mut length = 0usize;
        for byte in &sample[cursor..cursor + nal_length_size] {
            length = (length << 8) | usize::from(*byte);
        }
        cursor += nal_length_size;
        let end = cursor
            .checked_add(length)
            .filter(|end| *end <= sample.len())
            .ok_or_else(|| {
                VideoError::Bitstream("AVCC sample NAL unit overruns the sample".to_owned())
            })?;
        annex_b.extend_from_slice(&ANNEX_B_START_CODE);
        annex_b.extend_from_slice(&sample[cursor..end]);
        cursor = end;
    }
    Ok(annex_b)
}

/// MSB-first bit reader over an H.264 RBSP, including the Exp-Golomb
/// (`ue`/`se`) encodings of ISO 14496-10 §9.1.
struct BitReader<'a> {
    bytes: &'a [u8],
    /// Next bit to read, counted from the most significant bit.
    bit: usize,
}

impl<'a> BitReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, bit: 0 }
    }

    fn read_bit(&mut self) -> Result<u32, VideoError> {
        let byte = *self.bytes.get(self.bit / 8).ok_or_else(|| {
            VideoError::Bitstream("sequence parameter set ends mid-syntax".to_owned())
        })?;
        let bit = (byte >> (7 - (self.bit % 8))) & 1;
        self.bit += 1;
        Ok(u32::from(bit))
    }

    fn read_bits(&mut self, count: u32) -> Result<u32, VideoError> {
        let mut value = 0_u32;
        for _ in 0..count {
            value = (value << 1) | self.read_bit()?;
        }
        Ok(value)
    }

    fn read_u8(&mut self) -> Option<u8> {
        if self.bit % 8 != 0 {
            return None;
        }
        let byte = *self.bytes.get(self.bit / 8)?;
        self.bit += 8;
        Some(byte)
    }

    /// Unsigned Exp-Golomb: a zero prefix, a terminating one, then the
    /// suffix bits.
    fn read_ue(&mut self) -> Result<u32, VideoError> {
        let mut zeroes = 0_u32;
        while self.read_bit()? == 0 {
            zeroes += 1;
            if zeroes > 32 {
                return Err(VideoError::Bitstream(
                    "sequence parameter set Exp-Golomb prefix is unbounded".to_owned(),
                ));
            }
        }
        if zeroes == 0 {
            return Ok(0);
        }
        let suffix = self.read_bits(zeroes)?;
        Ok((1_u32 << zeroes) - 1 + suffix)
    }

    /// Signed Exp-Golomb: `ue` values map to signed by alternating signs.
    fn read_se(&mut self) -> Result<i64, VideoError> {
        let value = self.read_ue()?;
        Ok(if value % 2 == 0 {
            -i64::from(value + 1) / 2
        } else {
            i64::from(value + 1) / 2
        })
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    //! MSB-first bit writer used to synthesize H.264 SPS/PPS test vectors.

    /// Counterpart to the parser's [`super::BitReader`].
    pub(crate) struct BitWriter {
        bytes: Vec<u8>,
        bit: usize,
    }

    impl BitWriter {
        pub(crate) fn new() -> Self {
            Self {
                bytes: Vec::new(),
                bit: 0,
            }
        }

        pub(crate) fn push_bit(&mut self, bit: u32) {
            if self.bit % 8 == 0 {
                self.bytes.push(0);
            }
            if bit != 0 {
                let last = self.bytes.len() - 1;
                self.bytes[last] |= 1 << (7 - (self.bit % 8));
            }
            self.bit += 1;
        }

        pub(crate) fn push_bits(&mut self, count: u32, value: u32) {
            for offset in (0..count).rev() {
                self.push_bit((value >> offset) & 1);
            }
        }

        pub(crate) fn push_bytes(&mut self, bytes: &[u8]) {
            debug_assert_eq!(self.bit % 8, 0, "byte writes must stay byte-aligned");
            self.bytes.extend_from_slice(bytes);
            self.bit += bytes.len() * 8;
        }

        /// Unsigned Exp-Golomb.
        pub(crate) fn push_ue(&mut self, value: u32) {
            let value = value + 1;
            let bits = 32 - value.leading_zeros();
            for _ in 1..bits {
                self.push_bit(0);
            }
            self.push_bits(bits, value);
        }

        /// Signed Exp-Golomb.
        pub(crate) fn push_se(&mut self, value: i32) {
            let encoded = if value <= 0 {
                u32::try_from(-2 * value).expect("signed Exp-Golomb range")
            } else {
                u32::try_from(2 * value - 1).expect("signed Exp-Golomb range")
            };
            self.push_ue(encoded);
        }

        /// RBSP trailing bit: a one followed by alignment zeroes.
        pub(crate) fn push_trailing(&mut self) {
            self.push_bit(1);
            while self.bit % 8 != 0 {
                self.push_bit(0);
            }
        }

        pub(crate) fn into_bytes(self) -> Vec<u8> {
            self.bytes
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AvcDecoderConfig;
    use super::NAL_TYPE_SPS;
    use super::SeqParameterSet;
    use super::avcc_sample_to_annex_b;
    use super::nal_unit_type;
    use super::parse_avc_decoder_config;
    use super::parse_seq_parameter_set;
    use super::strip_emulation_prevention;
    use super::test_support::BitWriter;
    use crate::video::VideoError;

    /// A baseline SPS for `width x height`, bit-exact for the parser.
    pub(crate) fn synthetic_sps(width: u32, height: u32) -> Vec<u8> {
        let mut writer = BitWriter::new();
        writer.push_bytes(&[0x67, 66, 0x00, 30]);
        writer.push_ue(0); // seq_parameter_set_id
        writer.push_ue(0); // log2_max_frame_num_minus4
        writer.push_ue(0); // pic_order_cnt_type
        writer.push_ue(0); // log2_max_pic_order_cnt_lsb_minus4
        writer.push_ue(1); // max_num_ref_frames
        writer.push_bit(0); // gaps_in_frame_num_value_allowed_flag
        writer.push_ue(width / 16 - 1); // pic_width_in_mbs_minus1
        writer.push_ue(height / 16 - 1); // pic_height_in_map_units_minus1
        writer.push_bit(1); // frame_mbs_only_flag
        writer.push_bit(0); // direct_8x8_inference_flag
        writer.push_bit(0); // frame_cropping_flag
        writer.push_bit(0); // vui_parameters_present_flag
        writer.push_trailing();
        writer.into_bytes()
    }

    pub(crate) fn synthetic_pps() -> Vec<u8> {
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

    pub(crate) fn synthetic_avcc(sps: &[u8], pps: &[u8]) -> Vec<u8> {
        let mut record = vec![1_u8, 66, 0x00, 30, 0xFF, 0xE1];
        record.extend_from_slice(&(sps.len() as u16).to_be_bytes());
        record.extend_from_slice(sps);
        record.push(1);
        record.extend_from_slice(&(pps.len() as u16).to_be_bytes());
        record.extend_from_slice(pps);
        record
    }

    #[test]
    fn parses_synthetic_avcc_and_sps_dimensions() {
        let sps = synthetic_sps(64, 48);
        let pps = synthetic_pps();
        let record = synthetic_avcc(&sps, &pps);
        let config = parse_avc_decoder_config(&record).expect("avcC parses");
        assert_eq!(
            config,
            AvcDecoderConfig {
                configuration_version: 1,
                profile_idc: 66,
                profile_compatibility: 0x00,
                level_idc: 30,
                nal_length_size: 4,
                sequence_parameters: vec![sps.clone()],
                picture_parameters: vec![pps],
            }
        );
        let parsed = parse_seq_parameter_set(&sps).expect("SPS parses");
        assert_eq!(
            parsed,
            SeqParameterSet {
                profile_idc: 66,
                level_idc: 30,
                chroma_format_idc: 1,
                width: 64,
                height: 48,
                frame_mbs_only: true,
            }
        );
        assert_eq!(nal_unit_type(&sps), Some(NAL_TYPE_SPS));
    }

    #[test]
    fn strips_emulation_prevention_bytes() {
        // `00 00 03` followed by a low byte (or the end) is removed.
        assert_eq!(strip_emulation_prevention(&[0, 0, 3, 2]), vec![0, 0, 2]);
        assert_eq!(strip_emulation_prevention(&[0, 0, 3]), vec![0, 0]);
        // A `03` followed by a high byte is real data and must survive.
        assert_eq!(
            strip_emulation_prevention(&[0, 0, 3, 5, 1]),
            vec![0, 0, 3, 5, 1]
        );
        // A `00 00 03` that itself ends the third zero run: the scan sees the
        // second and third zeroes, removes the `03`, and keeps all zeroes.
        assert_eq!(
            strip_emulation_prevention(&[0, 0, 0, 3, 0]),
            vec![0, 0, 0, 0]
        );
        assert_eq!(strip_emulation_prevention(&[0, 0, 3]), vec![0, 0]);
    }

    #[test]
    fn rejects_truncated_and_malformed_records() {
        assert!(matches!(
            parse_avc_decoder_config(&[1, 66, 0, 30]),
            Err(VideoError::Bitstream(_))
        ));
        let sps = synthetic_sps(64, 48);
        let mut record = synthetic_avcc(&sps, &synthetic_pps());
        record.truncate(record.len() - 1);
        assert!(matches!(
            parse_avc_decoder_config(&record),
            Err(VideoError::Bitstream(_))
        ));
        record[0] = 2;
        assert!(matches!(
            parse_avc_decoder_config(&record),
            Err(VideoError::Bitstream(_))
        ));
    }

    #[test]
    fn converts_avcc_samples_to_annex_b_with_parameter_sets() {
        let sps = synthetic_sps(64, 48);
        let pps = synthetic_pps();
        let sample: [u8; 12] = [0, 0, 0, 3, 0x41, 0xAA, 0xBB, 0, 0, 0, 1, 0x65];
        let annex_b = avcc_sample_to_annex_b(&sample, 4, &[&sps, &pps]).expect("sample converts");
        assert!(annex_b.starts_with(&[0, 0, 0, 1, 0x67]));
        let joined: Vec<u8> = [
            &[0, 0, 0, 1][..],
            &sps,
            &[0, 0, 0, 1],
            &pps,
            &[0, 0, 0, 1, 0x41, 0xAA, 0xBB],
            &[0, 0, 0, 1, 0x65],
        ]
        .concat();
        assert_eq!(annex_b, joined);
        // A length prefix that overruns the sample is rejected.
        assert!(matches!(
            avcc_sample_to_annex_b(&[0, 0, 0, 40, 0x41], 4, &[]),
            Err(VideoError::Bitstream(_))
        ));
    }
}
