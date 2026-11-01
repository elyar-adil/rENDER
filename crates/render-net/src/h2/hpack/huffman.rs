//! The static HPACK Huffman code from RFC 7541 Appendix B.
//!
//! The table is a canonical prefix code over 257 symbols: the 256 octet
//! values plus EOS. EOS is used only for padding validation and must never
//! appear in a decoded string.

#![allow(
    clippy::unreadable_literal,
    reason = "the code and length arrays are generated verbatim from RFC 7541 Appendix B and stay aligned by row"
)]

use std::sync::OnceLock;

use crate::h2::error::HpackError;

/// Bit lengths for symbols 0..=255 and EOS at index 256.
const CODE_LENGTHS: [u8; 257] = [
    13, 23, 28, 28, 28, 28, 28, 28, 28, 24, 30, 28, 28, 30, 28, 28, 28, 28, 28, 28, 28, 28, 30, 28,
    28, 28, 28, 28, 28, 28, 28, 28, 6, 10, 10, 12, 13, 6, 8, 11, 10, 10, 8, 11, 8, 6, 6, 6, 5, 5,
    5, 6, 6, 6, 6, 6, 6, 6, 7, 8, 15, 6, 12, 10, 13, 6, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7,
    7, 7, 7, 7, 7, 7, 7, 7, 8, 7, 8, 13, 19, 13, 14, 6, 15, 5, 6, 5, 6, 5, 6, 6, 6, 5, 7, 7, 6, 6,
    6, 5, 6, 7, 6, 5, 5, 6, 7, 7, 7, 7, 7, 15, 11, 14, 13, 28, 20, 22, 20, 20, 22, 22, 22, 23, 22,
    23, 23, 23, 23, 23, 24, 23, 24, 24, 22, 23, 24, 23, 23, 23, 23, 21, 22, 23, 22, 23, 23, 24, 22,
    21, 20, 22, 22, 23, 23, 21, 23, 22, 22, 24, 21, 22, 23, 23, 21, 21, 22, 21, 23, 22, 23, 23, 20,
    22, 22, 22, 23, 22, 22, 23, 26, 26, 20, 19, 22, 23, 22, 25, 26, 26, 26, 27, 27, 26, 24, 25, 19,
    21, 26, 27, 27, 26, 27, 24, 21, 21, 26, 26, 28, 27, 27, 27, 20, 24, 20, 21, 22, 21, 21, 23, 22,
    22, 25, 25, 24, 24, 26, 23, 26, 27, 26, 26, 27, 27, 27, 27, 27, 28, 27, 27, 27, 27, 27, 26, 30,
];

/// Huffman codes aligned to the least significant bit (RFC 7541 Appendix B).
const CODES: [u32; 257] = [
    0x1ff8, 0x7fffd8, 0xfffffe2, 0xfffffe3, 0xfffffe4, 0xfffffe5, 0xfffffe6, 0xfffffe7, 0xfffffe8,
    0xffffea, 0x3ffffffc, 0xfffffe9, 0xfffffea, 0x3ffffffd, 0xfffffeb, 0xfffffec, 0xfffffed,
    0xfffffee, 0xfffffef, 0xffffff0, 0xffffff1, 0xffffff2, 0x3ffffffe, 0xffffff3, 0xffffff4,
    0xffffff5, 0xffffff6, 0xffffff7, 0xffffff8, 0xffffff9, 0xffffffa, 0xffffffb, 0x14, 0x3f8,
    0x3f9, 0xffa, 0x1ff9, 0x15, 0xf8, 0x7fa, 0x3fa, 0x3fb, 0xf9, 0x7fb, 0xfa, 0x16, 0x17, 0x18,
    0x0, 0x1, 0x2, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f, 0x5c, 0xfb, 0x7ffc, 0x20, 0xffb,
    0x3fc, 0x1ffa, 0x21, 0x5d, 0x5e, 0x5f, 0x60, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68,
    0x69, 0x6a, 0x6b, 0x6c, 0x6d, 0x6e, 0x6f, 0x70, 0x71, 0x72, 0xfc, 0x73, 0xfd, 0x1ffb, 0x7fff0,
    0x1ffc, 0x3ffc, 0x22, 0x7ffd, 0x3, 0x23, 0x4, 0x24, 0x5, 0x25, 0x26, 0x27, 0x6, 0x74, 0x75,
    0x28, 0x29, 0x2a, 0x7, 0x2b, 0x76, 0x2c, 0x8, 0x9, 0x2d, 0x77, 0x78, 0x79, 0x7a, 0x7b, 0x7ffe,
    0x7fc, 0x3ffd, 0x1ffd, 0xffffffc, 0xfffe6, 0x3fffd2, 0xfffe7, 0xfffe8, 0x3fffd3, 0x3fffd4,
    0x3fffd5, 0x7fffd9, 0x3fffd6, 0x7fffda, 0x7fffdb, 0x7fffdc, 0x7fffdd, 0x7fffde, 0xffffeb,
    0x7fffdf, 0xffffec, 0xffffed, 0x3fffd7, 0x7fffe0, 0xffffee, 0x7fffe1, 0x7fffe2, 0x7fffe3,
    0x7fffe4, 0x1fffdc, 0x3fffd8, 0x7fffe5, 0x3fffd9, 0x7fffe6, 0x7fffe7, 0xffffef, 0x3fffda,
    0x1fffdd, 0xfffe9, 0x3fffdb, 0x3fffdc, 0x7fffe8, 0x7fffe9, 0x1fffde, 0x7fffea, 0x3fffdd,
    0x3fffde, 0xfffff0, 0x1fffdf, 0x3fffdf, 0x7fffeb, 0x7fffec, 0x1fffe0, 0x1fffe1, 0x3fffe0,
    0x1fffe2, 0x7fffed, 0x3fffe1, 0x7fffee, 0x7fffef, 0xfffea, 0x3fffe2, 0x3fffe3, 0x3fffe4,
    0x7ffff0, 0x3fffe5, 0x3fffe6, 0x7ffff1, 0x3ffffe0, 0x3ffffe1, 0xfffeb, 0x7fff1, 0x3fffe7,
    0x7ffff2, 0x3fffe8, 0x1ffffec, 0x3ffffe2, 0x3ffffe3, 0x3ffffe4, 0x7ffffde, 0x7ffffdf,
    0x3ffffe5, 0xfffff1, 0x1ffffed, 0x7fff2, 0x1fffe3, 0x3ffffe6, 0x7ffffe0, 0x7ffffe1, 0x3ffffe7,
    0x7ffffe2, 0xfffff2, 0x1fffe4, 0x1fffe5, 0x3ffffe8, 0x3ffffe9, 0xffffffd, 0x7ffffe3, 0x7ffffe4,
    0x7ffffe5, 0xfffec, 0xfffff3, 0xfffed, 0x1fffe6, 0x3fffe9, 0x1fffe7, 0x1fffe8, 0x7ffff3,
    0x3fffea, 0x3fffeb, 0x1ffffee, 0x1ffffef, 0xfffff4, 0xfffff5, 0x3ffffea, 0x7ffff4, 0x3ffffeb,
    0x7ffffe6, 0x3ffffec, 0x3ffffed, 0x7ffffe7, 0x7ffffe8, 0x7ffffe9, 0x7ffffea, 0x7ffffeb,
    0xffffffe, 0x7ffffec, 0x7ffffed, 0x7ffffee, 0x7ffffef, 0x7fffff0, 0x3ffffee, 0x3fffffff,
];

#[derive(Clone, Copy, Default)]
struct Node {
    children: [Option<usize>; 2],
    symbol: Option<u16>,
}

struct Tree {
    nodes: Vec<Node>,
}

static TREE: OnceLock<Tree> = OnceLock::new();

fn tree() -> &'static Tree {
    TREE.get_or_init(|| {
        let mut nodes = vec![Node::default()];
        for symbol in 0..CODES.len() {
            let code = CODES[symbol];
            let length = usize::from(CODE_LENGTHS[symbol]);
            let mut node = 0;
            for shift in (0..length).rev() {
                let bit = usize::try_from((code >> shift) & 1).expect("one bit");
                let next = if let Some(next) = nodes[node].children[bit] {
                    next
                } else {
                    let next = nodes.len();
                    nodes.push(Node::default());
                    nodes[node].children[bit] = Some(next);
                    next
                };
                node = next;
            }
            debug_assert!(
                nodes[node].symbol.is_none(),
                "Huffman table is a prefix code"
            );
            nodes[node].symbol = Some(u16::try_from(symbol).expect("symbol fits u16"));
        }
        Tree { nodes }
    })
}

/// Decodes one HPACK Huffman string.
///
/// # Errors
///
/// Returns [`HpackError::InvalidHuffmanCode`] for a bit sequence that is not a
/// code, [`HpackError::EosInHuffman`] if EOS appears, and
/// [`HpackError::InvalidHuffmanPadding`] if the trailing padding is longer
/// than seven bits or is not a prefix of EOS.
pub fn decode(input: &[u8]) -> Result<Vec<u8>, HpackError> {
    let tree = tree();
    let mut output = Vec::with_capacity(input.len());
    let mut node = 0;
    let mut bits_since_symbol = 0_u32;
    let mut all_ones = true;

    for &byte in input {
        for shift in (0..8).rev() {
            let bit = usize::from((byte >> shift) & 1);
            node = tree.nodes[node].children[bit].ok_or(HpackError::InvalidHuffmanCode)?;
            bits_since_symbol += 1;
            all_ones &= bit == 1;
            if let Some(symbol) = tree.nodes[node].symbol {
                if symbol == 256 {
                    return Err(HpackError::EosInHuffman);
                }
                output.push(u8::try_from(symbol).unwrap_or_default());
                node = 0;
                bits_since_symbol = 0;
                all_ones = true;
            }
        }
    }

    if node != 0 && (bits_since_symbol > 7 || !all_ones) {
        return Err(HpackError::InvalidHuffmanPadding);
    }
    Ok(output)
}

/// Encodes a byte string with the HPACK Huffman table, adding EOS-prefix
/// padding to the next octet boundary.
#[must_use]
pub fn encode(input: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(encoded_len(input));
    let mut accumulator = 0_u64;
    let mut bits = 0_u32;

    for &byte in input {
        let code = u64::from(CODES[usize::from(byte)]);
        let length = u32::from(CODE_LENGTHS[usize::from(byte)]);
        accumulator = (accumulator << length) | code;
        bits += length;
        while bits >= 8 {
            bits -= 8;
            output.push(u8::try_from((accumulator >> bits) & 0xff).unwrap_or_default());
        }
        accumulator &= (1_u64 << bits) - 1;
    }

    if bits > 0 {
        let padding = 8 - bits;
        let value = (accumulator << padding) | ((1_u64 << padding) - 1);
        output.push(u8::try_from(value & 0xff).unwrap_or_default());
    }
    output
}

/// Returns the Huffman-encoded length in octets without allocating.
#[must_use]
pub fn encoded_len(input: &[u8]) -> usize {
    let bits: usize = input
        .iter()
        .map(|byte| usize::from(CODE_LENGTHS[usize::from(*byte)]))
        .sum();
    bits.div_ceil(8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_and_decodes_rfc_example_strings() {
        for input in [
            &b"www.example.com"[..],
            &b"/sample/path"[..],
            &b"custom-key"[..],
            &b"no-cache"[..],
            &[0x00, 0x7f, 0x80, 0xff],
        ] {
            let encoded = encode(input);
            assert_eq!(encoded.len(), encoded_len(input));
            assert_eq!(decode(&encoded).expect("decode Huffman"), input);
        }
    }

    #[test]
    fn round_trips_every_octet_in_one_string() {
        let input: Vec<u8> = (0..=255).collect();
        let encoded = encode(&input);
        assert_eq!(encoded.len(), encoded_len(&input));
        assert_eq!(decode(&encoded).expect("decode all octets"), input);
    }

    #[test]
    fn rejects_eos_and_bad_padding() {
        // EOS is 30 one bits followed by two padding bits.
        assert_eq!(
            decode(&[0xff, 0xff, 0xff, 0xfc]).expect_err("EOS"),
            HpackError::EosInHuffman
        );
        // Eight one bits are more than seven bits of padding.
        assert_eq!(
            decode(&[0xff]).expect_err("padding too long"),
            HpackError::InvalidHuffmanPadding
        );
        // A symbol followed by zero padding is not an EOS prefix.
        assert_eq!(
            decode(&[0x00]).expect_err("zero padding"),
            HpackError::InvalidHuffmanPadding
        );
    }
}
