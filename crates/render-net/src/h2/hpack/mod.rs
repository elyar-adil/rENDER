//! HPACK header compression (RFC 7541): static and dynamic tables, integer and
//! string primitives, Huffman coding, and all four header field
//! representations.

pub mod huffman;
mod static_table;

use std::collections::VecDeque;

use crate::h2::error::HpackError;

use static_table::STATIC_TABLE;

/// One HPACK header field. Names are compared as opaque octets here; HTTP/2
/// semantics (lowercase names, pseudo-header ordering) are enforced by the
/// connection layer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeaderField {
    pub name: Vec<u8>,
    pub value: Vec<u8>,
}

impl HeaderField {
    #[must_use]
    pub fn new(name: impl AsRef<[u8]>, value: impl AsRef<[u8]>) -> Self {
        Self {
            name: name.as_ref().to_vec(),
            value: value.as_ref().to_vec(),
        }
    }

    /// Returns the name when it is valid UTF-8.
    #[must_use]
    pub fn name_str(&self) -> Option<&str> {
        std::str::from_utf8(&self.name).ok()
    }

    /// Returns the value when it is valid UTF-8.
    #[must_use]
    pub fn value_str(&self) -> Option<&str> {
        std::str::from_utf8(&self.value).ok()
    }

    /// RFC 7541 section 4.1 entry size: name plus value plus 32 octets of
    /// accounting overhead.
    #[must_use]
    pub fn size(&self) -> usize {
        self.name.len() + self.value.len() + 32
    }
}

impl From<(&str, &str)> for HeaderField {
    fn from((name, value): (&str, &str)) -> Self {
        Self::new(name, value)
    }
}

/// How an encoder represents one literal header field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Indexing {
    /// Add the field to the dynamic table (`01` prefix).
    Incremental,
    /// Do not add the field to the dynamic table (`0000` prefix).
    WithoutIndexing,
    /// Do not add and mark as sensitive for intermediaries (`0001` prefix).
    NeverIndexed,
}

struct DynamicEntry {
    name: Vec<u8>,
    value: Vec<u8>,
}

impl DynamicEntry {
    fn size(&self) -> usize {
        self.name.len() + self.value.len() + 32
    }
}

#[derive(Default)]
struct DynamicTable {
    entries: VecDeque<DynamicEntry>,
    size: usize,
    max_size: usize,
}

impl DynamicTable {
    fn new(max_size: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            size: 0,
            max_size,
        }
    }

    fn set_max_size(&mut self, max_size: usize) {
        self.max_size = max_size;
        self.evict_to_fit(0);
    }

    fn evict_to_fit(&mut self, upcoming: usize) {
        while self.size + upcoming > self.max_size {
            let Some(entry) = self.entries.pop_back() else {
                break;
            };
            self.size -= entry.size();
        }
    }

    fn insert(&mut self, name: Vec<u8>, value: Vec<u8>) {
        let entry = DynamicEntry { name, value };
        let entry_size = entry.size();
        if entry_size > self.max_size {
            self.entries.clear();
            self.size = 0;
            return;
        }
        self.evict_to_fit(entry_size);
        self.size += entry_size;
        self.entries.push_front(entry);
    }

    fn get(&self, index: usize) -> Option<&DynamicEntry> {
        self.entries.get(index)
    }

    fn find_exact(&self, name: &[u8], value: &[u8]) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| entry.name == name && entry.value == value)
    }

    fn find_name(&self, name: &[u8]) -> Option<usize> {
        self.entries.iter().position(|entry| entry.name == name)
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn size(&self) -> usize {
        self.size
    }
}

/// HPACK encoder with its own dynamic table.
pub struct HpackEncoder {
    table: DynamicTable,
    /// Smallest table size requested since the last emitted block.
    pending_min: Option<usize>,
    /// Final table size requested since the last emitted block.
    pending_last: Option<usize>,
}

impl HpackEncoder {
    /// Creates an encoder whose dynamic table starts at `max_table_size`.
    #[must_use]
    pub fn new(max_table_size: usize) -> Self {
        Self {
            table: DynamicTable::new(max_table_size),
            pending_min: None,
            pending_last: None,
        }
    }

    /// Changes the dynamic table capacity and schedules a size update at the
    /// start of the next encoded block.
    ///
    /// If the size changes more than once before the next block, the smallest
    /// and final values are both signalled, as RFC 7541 section 4.2 requires.
    pub fn set_max_table_size(&mut self, max_table_size: usize) {
        self.pending_min = Some(
            self.pending_min
                .map_or(max_table_size, |smallest| smallest.min(max_table_size)),
        );
        self.pending_last = Some(max_table_size);
        self.table.set_max_size(max_table_size);
    }

    /// Encodes a header list with the default policy: sensitive fields are
    /// never indexed and all other fields use incremental indexing.
    #[must_use]
    pub fn encode(&mut self, headers: &[HeaderField]) -> Vec<u8> {
        let fields: Vec<(HeaderField, Indexing)> = headers
            .iter()
            .cloned()
            .map(|field| {
                let policy = if is_sensitive(&field.name) {
                    Indexing::NeverIndexed
                } else {
                    Indexing::Incremental
                };
                (field, policy)
            })
            .collect();
        self.encode_with(&fields, true)
    }

    /// Encodes a header list with explicit per-field representations.
    ///
    /// `huffman` selects Huffman string literals when they are shorter than
    /// the raw form; pass `false` to force raw literals for interoperability
    /// testing.
    #[must_use]
    pub fn encode_with(&mut self, headers: &[(HeaderField, Indexing)], huffman: bool) -> Vec<u8> {
        let mut output = Vec::new();
        self.emit_size_updates(&mut output);
        for (field, indexing) in headers {
            self.encode_field(field, *indexing, huffman, &mut output);
        }
        output
    }

    /// Dynamic table entry count (useful for diagnostics and tests).
    #[must_use]
    pub fn dynamic_table_len(&self) -> usize {
        self.table.len()
    }

    /// Dynamic table size in HPACK accounting octets.
    #[must_use]
    pub fn dynamic_table_size(&self) -> usize {
        self.table.size()
    }

    fn emit_size_updates(&mut self, output: &mut Vec<u8>) {
        if let (Some(smallest), Some(final_size)) = (self.pending_min, self.pending_last) {
            if smallest != final_size {
                encode_integer(smallest, 5, 0x20, output);
            }
            encode_integer(final_size, 5, 0x20, output);
        }
        self.pending_min = None;
        self.pending_last = None;
    }

    fn encode_field(
        &mut self,
        field: &HeaderField,
        indexing: Indexing,
        huffman: bool,
        output: &mut Vec<u8>,
    ) {
        if indexing == Indexing::Incremental
            && let Some(index) = self.find_exact(&field.name, &field.value)
        {
            encode_integer(index, 7, 0x80, output);
            return;
        }

        let name_index = self.find_name(&field.name);
        let (pattern, prefix_bits) = match indexing {
            Indexing::Incremental => (0x40, 6),
            Indexing::WithoutIndexing => (0x00, 4),
            Indexing::NeverIndexed => (0x10, 4),
        };
        encode_integer(name_index.unwrap_or(0), prefix_bits, pattern, output);
        if name_index.is_none() {
            encode_string(&field.name, huffman, output);
        }
        encode_string(&field.value, huffman, output);

        if indexing == Indexing::Incremental {
            self.table.insert(field.name.clone(), field.value.clone());
        }
    }

    fn find_exact(&self, name: &[u8], value: &[u8]) -> Option<usize> {
        if let Some(position) = STATIC_TABLE
            .iter()
            .position(|(entry_name, entry_value)| *entry_name == name && *entry_value == value)
        {
            return Some(position + 1);
        }
        self.table
            .find_exact(name, value)
            .map(|position| STATIC_TABLE.len() + position + 1)
    }

    fn find_name(&self, name: &[u8]) -> Option<usize> {
        if let Some(position) = STATIC_TABLE
            .iter()
            .position(|(entry_name, _)| *entry_name == name)
        {
            return Some(position + 1);
        }
        self.table
            .find_name(name)
            .map(|position| STATIC_TABLE.len() + position + 1)
    }
}

/// HPACK decoder with its own dynamic table.
pub struct HpackDecoder {
    table: DynamicTable,
    max_allowed_table_size: usize,
    header_list_limit: usize,
    max_string_size: usize,
}

impl HpackDecoder {
    /// Creates a decoder that will accept table size updates up to
    /// `max_table_size` and starts its dynamic table at that size.
    #[must_use]
    pub fn new(max_table_size: usize) -> Self {
        Self {
            table: DynamicTable::new(max_table_size),
            max_allowed_table_size: max_table_size,
            header_list_limit: usize::MAX,
            max_string_size: 1 << 20,
        }
    }

    /// Sets the maximum decoded header list size (RFC 7541 entry sizes).
    #[must_use]
    pub fn with_header_list_limit(mut self, limit: usize) -> Self {
        self.header_list_limit = limit;
        self
    }

    /// Sets the maximum size of one Huffman-decoded or raw string literal.
    #[must_use]
    pub fn with_max_string_size(mut self, limit: usize) -> Self {
        self.max_string_size = limit;
        self
    }

    /// Decodes one complete header block.
    ///
    /// # Errors
    ///
    /// Returns [`HpackError`] for invalid indexes, truncated or oversized
    /// integers and strings, invalid Huffman codes or padding, table size
    /// updates in the wrong position or above the advertised limit, and
    /// decoded header lists above the configured limit.
    pub fn decode(&mut self, block: &[u8]) -> Result<Vec<HeaderField>, HpackError> {
        let mut headers = Vec::new();
        let mut header_list_size = 0_usize;
        let mut position = 0;
        let mut saw_header = false;

        while position < block.len() {
            let first = block[position];
            if first & 0x80 != 0 {
                let index = decode_integer(block, &mut position, 7)?;
                if index == 0 {
                    return Err(HpackError::IndexZero);
                }
                let field = self.lookup(index)?;
                header_list_size = self.account(&field, header_list_size, &mut headers)?;
                saw_header = true;
            } else if first & 0x40 != 0 {
                let field = self.decode_literal(block, &mut position, 6)?;
                header_list_size = self.account(&field, header_list_size, &mut headers)?;
                self.table.insert(field.name.clone(), field.value.clone());
                saw_header = true;
            } else if first & 0x20 != 0 {
                if saw_header {
                    return Err(HpackError::SizeUpdateNotAtStart);
                }
                let size = decode_integer(block, &mut position, 5)?;
                if size > self.max_allowed_table_size {
                    return Err(HpackError::SizeUpdateTooLarge {
                        requested: size,
                        limit: self.max_allowed_table_size,
                    });
                }
                self.table.set_max_size(size);
            } else {
                let field = self.decode_literal(block, &mut position, 4)?;
                header_list_size = self.account(&field, header_list_size, &mut headers)?;
                saw_header = true;
            }
        }
        Ok(headers)
    }

    /// Dynamic table entry count (useful for diagnostics and tests).
    #[must_use]
    pub fn dynamic_table_len(&self) -> usize {
        self.table.len()
    }

    /// Dynamic table size in HPACK accounting octets.
    #[must_use]
    pub fn dynamic_table_size(&self) -> usize {
        self.table.size()
    }

    fn account(
        &self,
        field: &HeaderField,
        current: usize,
        headers: &mut Vec<HeaderField>,
    ) -> Result<usize, HpackError> {
        let next = current
            .checked_add(field.size())
            .ok_or(HpackError::HeaderListTooLarge {
                limit: self.header_list_limit,
            })?;
        if next > self.header_list_limit {
            return Err(HpackError::HeaderListTooLarge {
                limit: self.header_list_limit,
            });
        }
        headers.push(field.clone());
        Ok(next)
    }

    fn decode_literal(
        &self,
        block: &[u8],
        position: &mut usize,
        prefix_bits: u8,
    ) -> Result<HeaderField, HpackError> {
        let name_index = decode_integer(block, position, prefix_bits)?;
        let name = if name_index == 0 {
            self.decode_string(block, position)?
        } else {
            self.lookup(name_index)?.name
        };
        let value = self.decode_string(block, position)?;
        Ok(HeaderField { name, value })
    }

    fn lookup(&self, index: usize) -> Result<HeaderField, HpackError> {
        if index == 0 {
            return Err(HpackError::IndexZero);
        }
        if let Some((name, value)) = STATIC_TABLE.get(index - 1) {
            return Ok(HeaderField::new(name, value));
        }
        let dynamic_index = index - STATIC_TABLE.len() - 1;
        self.table
            .get(dynamic_index)
            .map(|entry| HeaderField {
                name: entry.name.clone(),
                value: entry.value.clone(),
            })
            .ok_or(HpackError::InvalidIndex { index })
    }

    fn decode_string(&self, block: &[u8], position: &mut usize) -> Result<Vec<u8>, HpackError> {
        let Some(&first) = block.get(*position) else {
            return Err(HpackError::TruncatedString {
                expected: 1,
                remaining: 0,
            });
        };
        let huffman = first & 0x80 != 0;
        let length = decode_integer(block, position, 7)?;
        if length > self.max_string_size {
            return Err(HpackError::StringTooLong {
                length,
                limit: self.max_string_size,
            });
        }
        let remaining = block.len() - *position;
        if length > remaining {
            return Err(HpackError::TruncatedString {
                expected: length,
                remaining,
            });
        }
        let raw = &block[*position..*position + length];
        *position += length;
        if huffman {
            huffman::decode(raw)
        } else {
            Ok(raw.to_vec())
        }
    }
}

fn is_sensitive(name: &[u8]) -> bool {
    name == b"authorization"
        || name == b"cookie"
        || name == b"proxy-authorization"
        || name == b"set-cookie"
}

fn encode_integer(value: usize, prefix_bits: u8, pattern: u8, output: &mut Vec<u8>) {
    debug_assert!((1..=8).contains(&prefix_bits));
    debug_assert_eq!(
        pattern & u8::try_from((1_u16 << prefix_bits) - 1).unwrap_or(0),
        0
    );
    let mask = (1_usize << prefix_bits) - 1;
    if value < mask {
        output.push(pattern | u8::try_from(value).expect("value fits prefix"));
        return;
    }
    output.push(pattern | u8::try_from(mask).expect("mask fits octet"));
    let mut remaining = value - mask;
    while remaining >= 128 {
        output.push(u8::try_from(remaining & 0x7f).expect("seven bits") | 0x80);
        remaining >>= 7;
    }
    output.push(u8::try_from(remaining).expect("remaining fits octet"));
}

fn decode_integer(
    block: &[u8],
    position: &mut usize,
    prefix_bits: u8,
) -> Result<usize, HpackError> {
    debug_assert!((1..=8).contains(&prefix_bits));
    let first = *block.get(*position).ok_or(HpackError::TruncatedInteger)?;
    *position += 1;
    let mask = (1_u16 << prefix_bits) - 1;
    let mut value = usize::from(first) & usize::from(mask);
    if value < usize::from(mask) {
        return Ok(value);
    }

    let mut shift = 0_u32;
    loop {
        let byte = *block.get(*position).ok_or(HpackError::TruncatedInteger)?;
        *position += 1;
        let chunk = usize::from(byte & 0x7f);
        let add = chunk
            .checked_shl(shift)
            .ok_or(HpackError::IntegerOverflow)?;
        value = value.checked_add(add).ok_or(HpackError::IntegerOverflow)?;
        if value > u32::MAX as usize {
            return Err(HpackError::IntegerOverflow);
        }
        if byte & 0x80 == 0 {
            return Ok(value);
        }
        shift += 7;
        if shift > 28 {
            return Err(HpackError::IntegerOverflow);
        }
    }
}

fn encode_string(value: &[u8], huffman: bool, output: &mut Vec<u8>) {
    if huffman {
        let encoded = huffman::encode(value);
        if encoded.len() < value.len() {
            encode_integer(encoded.len(), 7, 0x80, output);
            output.extend_from_slice(&encoded);
            return;
        }
    }
    encode_integer(value.len(), 7, 0, output);
    output.extend_from_slice(value);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &str) -> Vec<u8> {
        let compact: String = bytes
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect();
        compact
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                u8::from_str_radix(std::str::from_utf8(pair).expect("hex pair"), 16)
                    .expect("valid hex")
            })
            .collect()
    }

    fn fields(decoded: &[HeaderField]) -> Vec<(&str, &[u8])> {
        decoded
            .iter()
            .map(|field| {
                (
                    field.name_str().expect("test header name is ASCII"),
                    field.value.as_slice(),
                )
            })
            .collect()
    }

    #[test]
    fn decodes_rfc_7541_huffman_request_examples() {
        // C.4.1: first request with Huffman coding.
        let mut decoder = HpackDecoder::new(4096);
        let headers = decoder
            .decode(&hex("8286 8441 8cf1 e3c2 e5f2 3a6b a0ab 90f4 ff"))
            .expect("C.4.1");
        assert_eq!(
            fields(&headers),
            vec![
                (":method", &b"GET"[..]),
                (":scheme", &b"http"[..]),
                (":path", &b"/"[..]),
                (":authority", &b"www.example.com"[..]),
            ]
        );

        // C.4.2: second request, reusing the dynamic table.
        let headers = decoder
            .decode(&hex("8286 84be 5886 a8eb 1064 9cbf"))
            .expect("C.4.2");
        assert_eq!(
            fields(&headers),
            vec![
                (":method", &b"GET"[..]),
                (":scheme", &b"http"[..]),
                (":path", &b"/"[..]),
                (":authority", &b"www.example.com"[..]),
                ("cache-control", &b"no-cache"[..]),
            ]
        );

        // C.4.3: third request.
        let headers = decoder
            .decode(&hex(
                "8287 85bf 4088 25a8 49e9 5ba9 7d7f 8925 a849 e95b b8e8 b4bf",
            ))
            .expect("C.4.3");
        assert_eq!(
            fields(&headers),
            vec![
                (":method", &b"GET"[..]),
                (":scheme", &b"https"[..]),
                (":path", &b"/index.html"[..]),
                (":authority", &b"www.example.com"[..]),
                ("custom-key", &b"custom-value"[..]),
            ]
        );
    }

    #[test]
    fn decodes_rfc_7541_response_example() {
        // C.6.1: first response with Huffman coding.
        let mut decoder = HpackDecoder::new(4096);
        let headers = decoder
            .decode(&hex(
                "4882 6402 5885 aec3 771a 4b61 96d0 7abe 9410 54d4 44a8 2005 \
                 9504 0b81 66e0 82a6 2d1b ff6e 919d 29ad 1718 63c7 8f0b 97c8 \
                 e9ae 82ae 43d3",
            ))
            .expect("C.6.1");
        assert_eq!(headers[0].name_str(), Some(":status"));
        assert_eq!(headers[0].value_str(), Some("302"));
        assert_eq!(headers[1].name_str(), Some("cache-control"));
        assert_eq!(headers[1].value_str(), Some("private"));
        assert_eq!(headers[2].name_str(), Some("date"));
        assert_eq!(headers[3].name_str(), Some("location"));
        assert_eq!(headers[3].value_str(), Some("https://www.example.com"));
    }

    #[test]
    fn encoder_and_decoder_share_dynamic_indexes() {
        let mut encoder = HpackEncoder::new(4096);
        let mut decoder = HpackDecoder::new(4096);

        let request = vec![
            HeaderField::new(":method", "GET"),
            HeaderField::new(":scheme", "http"),
            HeaderField::new(":path", "/"),
            HeaderField::new(":authority", "www.example.com"),
            HeaderField::new("x-custom", "some value"),
        ];
        let first = encoder.encode(&request);
        assert_eq!(decoder.decode(&first).expect("first block"), request);
        assert!(encoder.dynamic_table_len() > 0);

        let second = encoder.encode(&request);
        assert!(
            second.len() < first.len(),
            "second block should reuse indexes"
        );
        assert_eq!(decoder.decode(&second).expect("second block"), request);
        assert_eq!(
            encoder.dynamic_table_size(),
            decoder.dynamic_table_size(),
            "both sides account the dynamic table identically"
        );
    }

    #[test]
    fn emits_literal_without_indexing_and_never_indexed() {
        let mut encoder = HpackEncoder::new(4096);
        let without = vec![(
            HeaderField::new("x-plain", "value"),
            Indexing::WithoutIndexing,
        )];
        let block = encoder.encode_with(&without, false);
        assert_eq!(block[0] & 0xf0, 0x00);
        let mut decoder = HpackDecoder::new(4096);
        assert_eq!(
            decoder.decode(&block).expect("without indexing"),
            vec![HeaderField::new("x-plain", "value")]
        );
        assert_eq!(decoder.dynamic_table_len(), 0);
        assert_eq!(encoder.dynamic_table_len(), 0);

        let never = vec![(
            HeaderField::new("authorization", "secret"),
            Indexing::NeverIndexed,
        )];
        let block = encoder.encode_with(&never, false);
        assert_eq!(block[0] & 0xf0, 0x10);
        decoder.decode(&block).expect("never indexed");
        assert_eq!(decoder.dynamic_table_len(), 0);
    }

    #[test]
    fn emits_dynamic_table_size_updates() {
        let mut encoder = HpackEncoder::new(4096);
        encoder.set_max_table_size(0);
        let block = encoder.encode(&[HeaderField::new(":method", "GET")]);
        assert_eq!(block[0], 0x20);

        encoder.set_max_table_size(128);
        let block = encoder.encode(&[HeaderField::new(":method", "GET")]);
        let mut decoder = HpackDecoder::new(4096);
        decoder.decode(&block).expect("size update to 128");
        assert_eq!(decoder.dynamic_table_size(), 0);
    }

    #[test]
    fn rejects_malformed_blocks() {
        let mut decoder = HpackDecoder::new(4096);

        assert_eq!(
            decoder.decode(&[0x80]).expect_err("index zero"),
            HpackError::IndexZero
        );
        assert_eq!(
            decoder
                .decode(&[0xbe])
                .expect_err("dynamic index on empty table"),
            HpackError::InvalidIndex { index: 62 }
        );
        assert_eq!(
            decoder
                .decode(&[0x82, 0x20])
                .expect_err("size update after field"),
            HpackError::SizeUpdateNotAtStart
        );
        assert_eq!(
            decoder
                .decode(&[0x40, 0x0a, b'x'])
                .expect_err("truncated string"),
            HpackError::TruncatedString {
                expected: 10,
                remaining: 1
            }
        );
        assert_eq!(
            decoder
                .decode(&[0x3f, 0xe2, 0x1f])
                .expect_err("size update too large"),
            HpackError::SizeUpdateTooLarge {
                requested: 4097,
                limit: 4096
            }
        );
        assert_eq!(
            decoder.decode(&[0x3f]).expect_err("truncated integer"),
            HpackError::TruncatedInteger
        );
    }

    #[test]
    fn enforces_header_list_limit() {
        let mut decoder = HpackDecoder::new(4096).with_header_list_limit(40);
        let block = hex("40 03 78 2d 61 01 62"); // x-a: b is 36 octets with overhead
        decoder.decode(&block).expect("fits limit");
        let block = hex("40 0a 78 2d 6c 6f 6e 67 6e 61 6d 65 01 62");
        assert_eq!(
            decoder.decode(&block).expect_err("over limit"),
            HpackError::HeaderListTooLarge { limit: 40 }
        );
    }

    #[test]
    fn integer_primitives_match_rfc_examples() {
        let mut encoded = Vec::new();
        encode_integer(10, 5, 0, &mut encoded);
        assert_eq!(encoded, [0x0a]);
        encoded.clear();
        encode_integer(1337, 5, 0, &mut encoded);
        assert_eq!(encoded, [0x1f, 0x9a, 0x0a]);
        encoded.clear();
        encode_integer(42, 8, 0, &mut encoded);
        assert_eq!(encoded, [0x2a]);

        let mut position = 0;
        assert_eq!(
            decode_integer(&[0x1f, 0x9a, 0x0a], &mut position, 5),
            Ok(1337)
        );
        assert_eq!(position, 3);
    }
}
