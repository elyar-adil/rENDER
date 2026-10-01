//! `TextEncoder` and `TextDecoder` (Encoding Standard).
//!
//! Bundled base64 and utf8 helpers call `new TextEncoder().encode(...)` and
//! `new TextDecoder().decode(...)` at the point of use, so a missing global
//! threw a `ReferenceError` and took the surrounding script with it. This
//! module implements the Encoding Standard's UTF-8 encoder and decoder
//! faithfully, including the cases a byte-loop shortcut gets wrong:
//!
//! - an unpaired surrogate on *encode* is U+FFFD, because ECMAScript strings
//!   are UTF-16 code-unit sequences and a lone half is not a scalar value;
//! - a *truncated* sequence on decode is one error for the whole sequence, not
//!   one per byte, and an *invalid continuation* is one error followed by a
//!   re-synchronisation that reprocesses the offending byte as a fresh lead;
//! - an overlong encoding, a surrogate half, and a code point above U+10FFFF are
//!   all errors, which is why `[0xC0,0xAF]` yields two U+FFFD and
//!   `[0xED,0xA0,0x80]` yields three;
//! - a stream carries a partial sequence across `decode` calls instead of
//!   mangling it, and only reports the error at end of stream.
//!
//! **Supported encodings.** UTF-8, windows-1252 and x-user-defined. The
//! `latin1`, `iso-8859-1`, `ascii` and `us-ascii` labels all name
//! **windows-1252**, which is what the Encoding Standard says and what a
//! browser does: `new TextDecoder('latin1').decode(new Uint8Array([0x80]))`
//! is U+20AC, not U+0080. A label the Encoding Standard knows but this engine
//! does not implement (`utf-16le`, `shift_jis`, and so on) is a
//! `DOMException` named `RangeError` from the constructor, which is a loud
//! failure rather than a silent mis-decode.
//!
//! **Errors.** The Encoding Standard throws a `TypeError` for a fatal decode
//! and a `RangeError` for an unknown label, and both are `DOMException`s whose
//! *names* are those strings. `e.name` therefore reads `"TypeError"` or
//! `"RangeError"`, `e instanceof DOMException` is true, `e instanceof
//! TypeError` is false, and `e.code` is 0 because neither name is a row of
//! `WebIDL` §2.8.1's table.

use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::runtime::builtins::dom_exception::DomExceptionName;
use crate::runtime::convert::to_number;
use crate::value::{NativeFunction, ObjectHost, TextEncoding};
use render_dom::Dom;

/// U+FFFD, the replacement character every decoding error produces.
const REPLACEMENT: char = '\u{fffd}';

/// The Encoding Standard's labels for the encodings this engine implements.
/// Matching is ASCII case-insensitive, as the standard requires.
fn get_encoding(label: &str) -> Option<TextEncoding> {
    let normalized = label.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "unicode-1-1-utf-8" | "unicode11utf8" | "unicode20utf8" | "utf-8" | "utf8"
        | "utf_8_2000" | "utf-8n" => Some(TextEncoding::Utf8),
        "ansi_x3.4-1968" | "ascii" | "cp1252" | "cp819" | "csisolatin1" | "ibm819"
        | "iso-8859-1" | "iso-ir-100" | "iso8859-1" | "iso88591" | "iso_8859-1"
        | "iso_8859-1:1987" | "l1" | "latin1" | "us-ascii" | "windows-1252" | "x-cp1252" => {
            Some(TextEncoding::Windows1252)
        }
        "x-user-defined" => Some(TextEncoding::XUserDefined),
        _ => None,
    }
}

/// windows-1252 differs from ISO-8859-1 only in `0x80` through `0x9F`, so this
/// table covers exactly that block. The five positions the standard leaves
/// unmapped
/// (`0x81`, `0x8D`, `0x8F`, `0x90`, `0x9D`) map to the C1 control of the same
/// value, which is what the standard's index pointer says and what a browser
/// produces.
const WINDOWS_1252_HIGH: [char; 32] = [
    '\u{20ac}', '\u{0081}', '\u{201a}', '\u{0192}', '\u{201e}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{02c6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{008d}', '\u{017d}', '\u{008f}',
    '\u{0090}', '\u{2018}', '\u{2019}', '\u{201c}', '\u{201d}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{02dc}', '\u{2122}', '\u{0161}', '\u{203a}', '\u{0153}', '\u{009d}', '\u{017e}', '\u{0178}',
];

/// x-user-defined maps `0x80` through `0x9F` to the U+F780 through U+F7FF
/// private block and is the identity elsewhere, which is how code moves opaque
/// bytes through a string.
fn x_user_defined(byte: u8) -> char {
    if (0x80..=0x9f).contains(&byte) {
        char::from_u32(0xf780 + u32::from(byte - 0x80)).unwrap_or(REPLACEMENT)
    } else {
        char::from_u32(u32::from(byte)).unwrap_or(REPLACEMENT)
    }
}

/// An ECMAScript string holds UTF-16 code units, and the engine represents an
/// unpaired surrogate as a private-use placeholder (see
/// `crate::lexer::surrogate_placeholder`). Encoding one is an error, so it
/// becomes U+FFFD exactly as a browser's encoder does.
fn is_unpaired_surrogate(character: char) -> bool {
    let value = u32::from(character);
    (0xf_0000..=0xf_07ff).contains(&value)
}

/// One code point's UTF-8 bytes. A scalar below U+80 is one byte, below U+800
/// two, below U+10000 three, and the astral planes four.
fn utf8_bytes(scalar: u32, out: &mut Vec<u8>) {
    match scalar {
        0..=0x7f => out.push(scalar as u8),
        0x80..=0x7ff => {
            out.push(0xc0 | (scalar >> 6) as u8);
            out.push(0x80 | (scalar & 0x3f) as u8);
        }
        0x800..=0xffff => {
            out.push(0xe0 | (scalar >> 12) as u8);
            out.push(0x80 | ((scalar >> 6) & 0x3f) as u8);
            out.push(0x80 | (scalar & 0x3f) as u8);
        }
        _ => {
            out.push(0xf0 | (scalar >> 18) as u8);
            out.push(0x80 | ((scalar >> 12) & 0x3f) as u8);
            out.push(0x80 | ((scalar >> 6) & 0x3f) as u8);
            out.push(0x80 | (scalar & 0x3f) as u8);
        }
    }
}

/// The UTF-8 encoding of a string, with an unpaired surrogate becoming U+FFFD.
fn encode_utf8(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    for character in text.chars() {
        if is_unpaired_surrogate(character) {
            utf8_bytes(u32::from(REPLACEMENT), &mut out);
        } else {
            utf8_bytes(u32::from(character), &mut out);
        }
    }
    out
}

/// The decoded bytes of one code point, or `None` when `byte` cannot start a
/// sequence. The returned width and range are what the continuation bytes must
/// satisfy: the range excludes the overlong forms and, for a three-byte lead,
/// the surrogate halves.
fn utf8_lead(byte: u8) -> Option<(usize, u8, u8)> {
    match byte {
        0x00..=0x7f => Some((1, 0, 0)),
        0xc2..=0xdf => Some((2, 0x80, 0xbf)),
        0xe0 => Some((3, 0xa0, 0xbf)),
        0xe1..=0xec => Some((3, 0x80, 0xbf)),
        0xed => Some((3, 0x80, 0x9f)),
        0xee..=0xef => Some((3, 0x80, 0xbf)),
        0xf0 => Some((4, 0x90, 0xbf)),
        0xf1..=0xf3 => Some((4, 0x80, 0xbf)),
        0xf4 => Some((4, 0x80, 0x8f)),
        // 0x80..=0xbf is a continuation with no lead, and 0xc0/0xc1 are always
        // overlong, 0xf5..=0xff always above U+10FFFF: both are errors here.
        _ => None,
    }
}

/// The outcome of running the UTF-8 decoder over a byte run.
struct Utf8Outcome {
    text: String,
    /// Bytes of a truncated trailing sequence, to be prepended to the next
    /// chunk when streaming.
    pending: Vec<u8>,
    /// Whether any error occurred, which is what `fatal` turns into a throw.
    errored: bool,
}

/// The UTF-8 decoder. On an invalid continuation it emits one U+FFFD and
/// re-synchronises by reprocessing the offending byte as a fresh lead, which is
/// what makes `[0xC3,0x28]` decode to `U+FFFD` followed by `(` rather than
/// swallowing the `(`.
fn decode_utf8(bytes: &[u8], stream: bool, bom_seen: bool) -> Utf8Outcome {
    let mut text = String::with_capacity(bytes.len());
    let mut errored = false;
    let mut index = 0usize;
    // §4.3 decode: a leading BOM is consumed once per stream, not per chunk.
    if !bom_seen && bytes.len() >= 3 && bytes[..3] == [0xef, 0xbb, 0xbf] {
        index = 3;
    }
    while index < bytes.len() {
        let Some((width, low, high)) = utf8_lead(bytes[index]) else {
            text.push(REPLACEMENT);
            errored = true;
            index += 1;
            continue;
        };
        if width == 1 {
            text.push(char::from(bytes[index]));
            index += 1;
            continue;
        }
        if index + width > bytes.len() {
            if stream {
                // Hold the whole partial sequence back for the next chunk.
                return Utf8Outcome {
                    text,
                    pending: bytes[index..].to_vec(),
                    errored,
                };
            }
            // End of stream with a truncated sequence: one error for all of it.
            text.push(REPLACEMENT);
            return Utf8Outcome {
                text,
                pending: Vec::new(),
                errored: true,
            };
        }
        // Accumulate the lead's payload and then one 6-bit group per
        // continuation byte, shifting left each time. The second byte carries a
        // narrower range than the rest, which is what rules out the overlong
        // forms and the surrogate halves.
        let mut scalar = match width {
            2 => u32::from(bytes[index]) & 0x1f,
            3 => u32::from(bytes[index]) & 0x0f,
            _ => u32::from(bytes[index]) & 0x07,
        };
        let mut valid = true;
        for offset in 1..width {
            let byte = bytes[index + offset];
            let in_range = if offset == 1 {
                (low..=high).contains(&byte)
            } else {
                (0x80..=0xbf).contains(&byte)
            };
            if !in_range {
                // Re-synchronise: the error is reported for the lead, and the
                // offending byte is reprocessed as the start of a new sequence.
                valid = false;
                index += offset;
                break;
            }
            scalar = (scalar << 6) | (u32::from(byte) & 0x3f);
        }
        if !valid {
            text.push(REPLACEMENT);
            errored = true;
            continue;
        }
        text.push(char::from_u32(scalar).unwrap_or(REPLACEMENT));
        index += width;
    }
    Utf8Outcome {
        text,
        pending: Vec::new(),
        errored,
    }
}

/// The windows-1252 and x-user-defined decoders have no error states: every
/// byte maps to exactly one code point.
fn decode_single_byte(bytes: &[u8], encoding: TextEncoding) -> String {
    bytes
        .iter()
        .map(|byte| match encoding {
            // windows-1252 is ISO-8859-1 with a remapped C1 block, so only
            // `0x80` through `0x9F` consults the table; `0xA0` through `0xFF`
            // are the identity, exactly as in the standard's index.
            TextEncoding::Windows1252 => match byte {
                0x00..=0x7f | 0xa0..=0xff => char::from(*byte),
                _ => WINDOWS_1252_HIGH[usize::from(byte - 0x80)],
            },
            TextEncoding::XUserDefined => x_user_defined(*byte),
            // `Utf8` never reaches this arm: it has its own decoder.
            TextEncoding::Utf8 => char::from(*byte),
        })
        .collect()
}

impl JsRuntime {
    /// Bytes of an `ArrayBufferView` argument: a typed array, a `DataView`, or
    /// an array-like, which is the set the spec's buffer-source conversion
    /// accepts here.
    fn view_bytes(&mut self, dom: &mut Dom, value: &JsValue) -> Result<Vec<u8>, JsError> {
        let JsValue::Object(object) = value else {
            return Ok(value.to_js_string().as_bytes().to_vec());
        };
        if let Some(ObjectHost::TypedArray {
            kind,
            buffer,
            start,
            length,
        }) = self.realm.host(*object)
        {
            let values = buffer.0.borrow();
            return Ok(values[start..start.saturating_add(length)]
                .iter()
                .map(|value| kind.encode(*value) as u8)
                .collect());
        }
        if let Some(ObjectHost::DataView {
            buffer,
            byte_offset,
            byte_length,
            ..
        }) = self.realm.host(*object)
        {
            let values = buffer.0.borrow();
            return Ok(values[byte_offset..byte_offset + byte_length]
                .iter()
                .map(|value| *value as i64 as u8)
                .collect());
        }
        let length = self.array_like_length(dom, *object)?;
        let mut bytes = Vec::new();
        for index in 0..length {
            let item = self.get_member(dom, *object, &index.to_string())?;
            let number = to_number(&item)?;
            bytes.push(if number.is_finite() {
                number.clamp(0.0, 255.0) as u8
            } else {
                0
            });
        }
        Ok(bytes)
    }

    pub(in crate::runtime) fn dispatch_encoding_native(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match function {
            NativeFunction::TextEncoderEncode => self.text_encoder_encode(arguments),
            NativeFunction::TextEncoderEncodeInto => self.text_encoder_encode_into(arguments),
            NativeFunction::TextDecoderDecode => self.text_decoder_decode(dom, receiver, arguments),
            other => self.dispatch_data_view_native(dom, other, receiver, arguments),
        }
    }
    /// `new TextEncoder()`: UTF-8 only, and stateless.
    pub(in crate::runtime) fn text_encoder_constructor(
        &mut self,
        constructor: ObjectId,
    ) -> Result<JsValue, JsError> {
        let prototype = self
            .realm
            .get_property(constructor, "prototype")
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            });
        self.ensure_heap_capacity(1)?;
        Ok(JsValue::Object(self.realm.create_object(prototype)))
    }

    /// `new TextDecoder(label, {fatal, ignoreBOM})`. An unrecognised label is a
    /// `RangeError`; the resolved encoding and the `fatal` flag become own
    /// read-only properties, because both answers depend on the label.
    pub(in crate::runtime) fn text_decoder_constructor(
        &mut self,
        constructor: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let label = match arguments.first() {
            None | Some(JsValue::Undefined) => "utf-8".to_owned(),
            Some(value) => value.to_js_string(),
        };
        // Encoding Standard §"TextDecoder": "If encoding is null, then throw a
        // `RangeError`." A Web API specification's `RangeError` is a
        // `DOMException` named `RangeError`, so `e.name` is unchanged by the
        // migration and `e.code` is 0 (no table entry), while
        // `e instanceof RangeError` becomes **false** and
        // `e instanceof DOMException` becomes true. That trade is the correct
        // one: `e instanceof RangeError` was true only because the engine had
        // nowhere else to put the name, and it was wrong for exactly the code
        // that checks `e instanceof DOMException` first.
        let Some(encoding) = get_encoding(&label) else {
            return Err(self.dom_exception(
                DomExceptionName::EcmascriptRangeError,
                format!("The encoding label provided ('{label}') is invalid"),
            ));
        };
        let options = match arguments.get(1) {
            Some(JsValue::Object(object)) => Some(*object),
            _ => None,
        };
        let flag = |name: &str| {
            options.is_some_and(|object| {
                self.realm
                    .get_property(object, name)
                    .is_some_and(|value| value.is_truthy())
            })
        };
        let fatal = flag("fatal");
        // `ignoreBOM` defaults to false, so a leading U+FEFF is stripped unless
        // the caller asks to keep it.
        let bom_seen = flag("ignoreBOM");
        let prototype = self
            .realm
            .get_property(constructor, "prototype")
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            });
        self.ensure_heap_capacity(1)?;
        let object = self.realm.create_object(prototype);
        if let Some(host) = self.realm.host_mut(object) {
            *host = ObjectHost::TextDecoder {
                encoding,
                fatal,
                pending: Vec::new(),
                bom_seen,
            };
        }
        for (name, value) in [
            ("encoding", JsValue::String(encoding.name().to_owned())),
            ("fatal", JsValue::Boolean(fatal)),
        ] {
            self.realm.define_property(
                object,
                name,
                crate::PropertyDescriptor {
                    value,
                    writable: false,
                    getter: None,
                    setter: None,
                    enumerable: true,
                    configurable: true,
                },
            );
        }
        Ok(JsValue::Object(object))
    }

    /// `TextEncoder.prototype.encode(input = "")`.
    fn text_encoder_encode(&mut self, arguments: &[JsValue]) -> Result<JsValue, JsError> {
        let text = arguments
            .first()
            .map_or_else(String::new, JsValue::to_js_string);
        let bytes = encode_utf8(&text);
        self.uint8_array_from_bytes(&bytes)
    }

    /// `TextEncoder.prototype.encodeInto(source, destination)`.
    ///
    /// Writes whole code points only: the first code point that would not fit is
    /// not partially written, and `read` then reports how much of `source` was
    /// consumed. `read` counts UTF-16 code units, so an astral code point
    /// counts as two.
    fn text_encoder_encode_into(&mut self, arguments: &[JsValue]) -> Result<JsValue, JsError> {
        let source = arguments
            .first()
            .map_or_else(String::new, JsValue::to_js_string);
        let destination = arguments
            .get(1)
            .ok_or_else(|| JsError::type_error("encodeInto requires a destination"))?;
        let JsValue::Object(target) = destination else {
            return Err(JsError::type_error(
                "encodeInto destination must be a Uint8Array",
            ));
        };
        let Some(ObjectHost::TypedArray {
            kind,
            buffer,
            start,
            length,
        }) = self.realm.host(*target)
        else {
            return Err(JsError::type_error(
                "encodeInto destination must be a Uint8Array",
            ));
        };
        if kind.element_size() != 1 {
            return Err(JsError::type_error(
                "encodeInto destination must be a byte-sized typed array",
            ));
        }
        let mut written = 0usize;
        let mut read = 0usize;
        {
            let mut slots = buffer.0.borrow_mut();
            let limit = start + length;
            for character in source.chars() {
                let scalar = if is_unpaired_surrogate(character) {
                    u32::from(REPLACEMENT)
                } else {
                    u32::from(character)
                };
                let mut encoded = Vec::with_capacity(4);
                utf8_bytes(scalar, &mut encoded);
                if start + written + encoded.len() > limit {
                    break;
                }
                for byte in encoded {
                    slots[start + written] = f64::from(byte);
                    written += 1;
                }
                // `read` is "the number of code units read from source", so it is
                // counted in the same units `String.prototype.length` reports.
                // A placeholder stands for exactly *one* surrogate, and
                // `char::len_utf16` would count the private-use scalar itself as
                // two - so `encodeInto('\uD83Dx')` reported 2 read for a string
                // of length 2 where the first code point is one unit wide, and
                // stopped a unit early. One unit per placeholder is the count
                // that makes `read` and `length` agree.
                read += if is_unpaired_surrogate(character) {
                    1
                } else {
                    character.len_utf16()
                };
            }
        }
        self.ensure_heap_capacity(1)?;
        let result = self.realm.create_ordinary_object();
        for (name, value) in [
            ("read", JsValue::Number(read as f64)),
            ("written", JsValue::Number(written as f64)),
        ] {
            self.realm.set_property(result, name.to_owned(), value);
        }
        Ok(JsValue::Object(result))
    }

    /// `TextDecoder.prototype.decode(input, {stream})`.
    fn text_decoder_decode(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        // Take the held-back bytes out first: they belong to the *previous*
        // streamed call, and prepending them is what carries a partial
        // multi-byte sequence across a chunk boundary instead of mangling it.
        let (mut bytes, encoding, fatal, mut bom_seen) = match self.realm.host_mut(receiver) {
            Some(ObjectHost::TextDecoder {
                encoding,
                fatal,
                pending,
                bom_seen,
            }) => (std::mem::take(pending), *encoding, *fatal, *bom_seen),
            _ => {
                return Err(JsError::type_error(
                    "TextDecoder.prototype.decode called on an incompatible receiver",
                ));
            }
        };
        let mut chunk = match arguments.first() {
            Some(JsValue::Undefined) | None => Vec::new(),
            Some(value) => self.view_bytes(dom, value)?,
        };
        if !bytes.is_empty() {
            bytes.append(&mut chunk);
            chunk = bytes;
        }
        bytes = chunk;
        let stream = match arguments.get(1) {
            Some(JsValue::Object(options)) => self
                .realm
                .get_property(*options, "stream")
                .is_some_and(|value| value.is_truthy()),
            _ => false,
        };
        let (text, next_pending, errored) = match encoding {
            TextEncoding::Utf8 => {
                let outcome = decode_utf8(&bytes, stream, bom_seen);
                (outcome.text, outcome.pending, outcome.errored)
            }
            other => (decode_single_byte(&bytes, other), Vec::new(), false),
        };
        if errored && fatal {
            // Encoding Standard §"decode": "If fatal is true and the decoder
            // encountered an error, then throw a `TypeError`." A `TypeError` here
            // is a `DOMException` whose *name* is `TypeError`, which is why it
            // is not the ECMAScript `TypeError`: `e instanceof TypeError` is
            // false, and `e instanceof DOMException` is true. §2.8.1 spells this
            // namespace collision out for `SyntaxError` and it holds for every
            // name the two error families share.
            return Err(self.dom_exception(
                DomExceptionName::EcmascriptTypeError,
                format!(
                    "The encoded data was not valid for encoding {}",
                    encoding.name()
                ),
            ));
        }
        // A BOM is skipped once per stream, so a chunk that does not start with
        // one must not reset the flag.
        bom_seen = bom_seen || bytes.starts_with(&[0xef, 0xbb, 0xbf]);
        if let Some(ObjectHost::TextDecoder {
            pending,
            bom_seen: seen,
            ..
        }) = self.realm.host_mut(receiver)
        {
            *pending = next_pending;
            *seen = bom_seen;
        }
        Ok(JsValue::String(text))
    }

    /// A `Uint8Array` over `bytes`, using the installed `%Uint8Array.prototype%`
    /// so `instanceof` and every prototype method work.
    pub(in crate::runtime) fn uint8_array_from_bytes(
        &mut self,
        bytes: &[u8],
    ) -> Result<JsValue, JsError> {
        let prototype = self
            .realm
            .global("Uint8Array")
            .and_then(|value| match value {
                JsValue::Object(constructor) => self.realm.get_property(constructor, "prototype"),
                _ => None,
            })
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            });
        let values = bytes
            .iter()
            .map(|byte| f64::from(*byte))
            .collect::<Vec<_>>();
        self.create_typed_array_from_values(crate::value::TypedArrayKind::Uint8, &values, prototype)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        REPLACEMENT, WINDOWS_1252_HIGH, decode_single_byte, decode_utf8, encode_utf8, get_encoding,
        is_unpaired_surrogate, utf8_lead,
    };
    use crate::lexer::surrogate_placeholder;
    use crate::runtime::JsRuntime;
    use crate::value::TextEncoding;
    use render_html::parse_document;

    /// Every expectation in this module was measured against Node v24.13.1
    /// running the same expression, so a divergence is an engine change rather
    /// than a hand-computed guess. The probe's completion value is compared as
    /// its JavaScript string form, so a probe may end in a number or a boolean.
    fn run(source: &str) -> String {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(&mut parsed.dom, source)
            .expect("encoding probe executes");
        outcome.value.to_js_string()
    }

    #[test]
    fn encode_covers_ascii_astral_and_the_replacement_of_a_lone_surrogate() {
        assert_eq!(
            run(r"Array.from(new TextEncoder().encode('abc')).join(',')"),
            "97,98,99"
        );
        assert_eq!(
            run(r"Array.from(new TextEncoder().encode('A\u{1F600}B')).join(',')"),
            "65,240,159,152,128,66"
        );
        // An unpaired surrogate is not a scalar value, so both halves encode
        // as U+FFFD rather than producing a CESU-8 pair.
        assert_eq!(
            run(r"Array.from(new TextEncoder().encode('\uD800')).join(',')"),
            "239,191,189"
        );
        assert_eq!(
            run(r"Array.from(new TextEncoder().encode('\uDC00')).join(',')"),
            "239,191,189"
        );
        assert_eq!(
            run(r"Array.from(new TextEncoder().encode('\uD83D\uDE00')).join(',')"),
            "240,159,152,128"
        );
        assert_eq!(
            run(r"Array.from(new TextEncoder().encode('\u{10FFFF}')).join(',')"),
            "244,143,191,191"
        );
        assert_eq!(run("new TextEncoder().encode().length"), "0");
        assert_eq!(run("new TextEncoder().encoding"), "utf-8");
    }

    #[test]
    fn decode_reports_one_error_per_sequence_and_resynchronises() {
        // `codePointAt(0)` reports the scalar, which is how an astral answer is
        // told from its UTF-16 halves.
        assert_eq!(
            run("new TextDecoder().decode(new Uint8Array([0x61,0x62,0x63]))"),
            "abc"
        );
        assert_eq!(
            run("new TextDecoder().decode(new Uint8Array([0xC3,0xA9])).charCodeAt(0)"),
            "233"
        );
        assert_eq!(
            run("new TextDecoder().decode(new Uint8Array([0xE2,0x82,0xAC])).charCodeAt(0)"),
            "8364"
        );
        assert_eq!(
            run("new TextDecoder().decode(new Uint8Array([0xF0,0x9F,0x98,0x80])) === '\u{1F600}'"),
            "true"
        );
        // A leading BOM is consumed, leaving "a".
        assert_eq!(
            run("new TextDecoder().decode(new Uint8Array([0xEF,0xBB,0xBF,0x61]))"),
            "a"
        );
        // A truncated sequence is ONE error for the whole sequence.
        assert_eq!(
            run("new TextDecoder().decode(new Uint8Array([0xC3]))"),
            "\u{fffd}"
        );
        assert_eq!(
            run("new TextDecoder().decode(new Uint8Array([0xE2,0x82]))"),
            "\u{fffd}"
        );
        // An invalid continuation is one error, then the offending byte is
        // reprocessed as a fresh lead, so the '(' survives.
        assert_eq!(
            run("new TextDecoder().decode(new Uint8Array([0xC3,0x28]))"),
            "\u{fffd}("
        );
        assert_eq!(
            run("new TextDecoder().decode(new Uint8Array([0x80]))"),
            "\u{fffd}"
        );
        // An overlong form, a surrogate half, and a code point above U+10FFFF
        // are each errors, and each yields one replacement per offending byte.
        assert_eq!(
            run("new TextDecoder().decode(new Uint8Array([0xC0,0xAF]))"),
            "\u{fffd}\u{fffd}"
        );
        assert_eq!(
            run("new TextDecoder().decode(new Uint8Array([0xED,0xA0,0x80]))"),
            "\u{fffd}\u{fffd}\u{fffd}"
        );
        assert_eq!(
            run("new TextDecoder().decode(new Uint8Array([0xF5,0x80,0x80,0x80]))"),
            "\u{fffd}\u{fffd}\u{fffd}\u{fffd}"
        );
    }

    #[test]
    fn fatal_turns_a_replacement_into_a_throw() {
        assert_eq!(
            run(r"
                function thrown(fn) { try { fn(); return 'no throw'; } catch (e) { return e.name; } }
                thrown(function () { return new TextDecoder('utf-8', {fatal: true}).decode(new Uint8Array([0xC3])); })
            "),
            "TypeError"
        );
        // A valid sequence never throws, however fatal the decoder is.
        assert_eq!(
            run(r"
                new TextDecoder('utf-8', {fatal: true}).decode(new Uint8Array([0xC3, 0xA9]))
            "),
            "\u{e9}"
        );
        assert_eq!(run("new TextDecoder('utf-8', {fatal: true}).fatal"), "true");
        assert_eq!(run("new TextDecoder().fatal"), "false");
    }

    #[test]
    fn a_stream_carries_a_partial_sequence_across_chunks() {
        // The three-byte EURO SIGN split after two bytes.
        assert_eq!(
            run(r"
                var d = new TextDecoder();
                var head = d.decode(new Uint8Array([0xE2, 0x82]), {stream: true});
                var tail = d.decode(new Uint8Array([0xAC]));
                head + tail
            "),
            "\u{20ac}"
        );
        // The four-byte emoji split after three bytes.
        assert_eq!(
            run(r"
                var d = new TextDecoder();
                d.decode(new Uint8Array([0xF0, 0x9F, 0x98]), {stream: true})
                    + d.decode(new Uint8Array([0x80]))
            "),
            "\u{1f600}"
        );
        // A partial sequence is held back, not mangled and not reported.
        assert_eq!(
            run("new TextDecoder().decode(new Uint8Array([0xE2, 0x82]), {stream: true})"),
            ""
        );
        assert_eq!(
            run(r"
                var d = new TextDecoder();
                d.decode(new Uint8Array([0xE2, 0x82]), {stream: true})
                    + d.decode(new Uint8Array([]), {stream: true})
            "),
            ""
        );
        // The error surfaces at end of stream, where fatal can see it.
        assert_eq!(
            run(r"
                function thrown(fn) { try { fn(); return 'no throw'; } catch (e) { return e.name; } }
                var d = new TextDecoder('utf-8', {fatal: true});
                thrown(function () {
                    d.decode(new Uint8Array([0xC3]), {stream: true});
                    return d.decode(new Uint8Array([]), {stream: false});
                })
            "),
            "TypeError"
        );
    }

    /// The test that pins the agreement between `encodeInto`'s `read` and
    /// `String.prototype.length`.
    ///
    /// This is the sharpest evidence that the string model was wrong rather
    /// than incomplete. `TextEncoder.prototype.encodeInto` reported `read` in
    /// UTF-16 code units - correctly, per the Encoding Standard, and checked
    /// against Node - while `String.prototype.length` counted code points in the
    /// same engine, and the suite asserted both. Two methods on the same string
    /// disagreed about its size.
    ///
    /// `read` is defined as "the number of code units read from `source`" and
    /// `length` as the number of code units in a String value, so when the
    /// destination is large enough to take everything, the two must be equal.
    /// The surrogate cases are the ones that would have caught it: a lone
    /// surrogate is *one* code unit that encodes to *three* bytes, and a pair is
    /// *two* code units that encodes to *four*, so a `written`-based reading
    /// cannot stand in for either.
    #[test]
    fn encode_into_read_agrees_with_string_length() {
        let cases = [
            ("''", "''"),
            ("'ascii'", "'plain ascii'"),
            ("'a\\u{1F600}'", "'ascii then a pair'"),
            ("'\\u{1F600}'", "'a pair'"),
            ("'\\u{1F600}a'", "'a pair then ascii'"),
            ("'\\uD83D'", "'a lone leading surrogate'"),
            ("'\\uDE00'", "'a lone trailing surrogate'"),
            (
                "String.fromCharCode(0xD83D) + String.fromCharCode(0xDE00)",
                "'the pair from its halves'",
            ),
            ("'\\uD83D\\uD83D'", "'two lone leading surrogates'"),
            ("'\\u{1F600}\\u{1F600}'", "'two pairs'"),
            ("'x\\uD83Dy'", "'a lone surrogate between ascii'"),
        ];
        for (expression, description) in cases {
            let observed = run(&format!(
                "
                var s = {expression};
                var d = new Uint8Array(256);
                var r = new TextEncoder().encodeInto(s, d);
                [s.length, r.read, r.written, r.read === s.length].join('|')
                "
            ));
            let fields: Vec<&str> = observed.split('|').collect();
            assert_eq!(
                fields[3], "true",
                "encodeInto read must equal length for {description} ({expression}), got {observed}"
            );
            assert_eq!(fields[0], fields[1], "read vs length for {description}");
        }
        // The numbers themselves, so a change in either is visible rather than
        // only the equality being re-established.
        assert_eq!(
            run(
                "var s = 'a\\u{1F600}'; var r = new TextEncoder().encodeInto(s, new Uint8Array(256)); [s.length, r.read, r.written].join('|')"
            ),
            "3|3|5"
        );
        // A lone surrogate: one code unit consumed, three bytes written. The
        // bytes are U+FFFD because an unpaired surrogate is not encodable.
        assert_eq!(
            run(
                "var s = '\\uD83D'; var d = new Uint8Array(256); var r = new TextEncoder().encodeInto(s, d); [s.length, r.read, r.written, Array.from(d.slice(0,3)).join(',')].join('|')"
            ),
            "1|1|3|239,191,189"
        );
    }

    /// `read` stops at a whole code point, and where it stops is counted in the
    /// same units `length` reports - so a truncated encode of a pair reports 0,
    /// not 1, even though a whole unit was available.
    #[test]
    fn encode_into_read_agrees_with_length_when_it_stops_early() {
        assert_eq!(
            run(r"
                var s = '\u{1F600}';
                var d = new Uint8Array(3);
                var r = new TextEncoder().encodeInto(s, d);
                [s.length, r.read, r.written, r.read < s.length].join('|')
            "),
            "2|0|0|true"
        );
        // Four bytes take the pair and stop before the next code point.
        assert_eq!(
            run(r"
                var s = '\u{1F600}\u{1F601}';
                var d = new Uint8Array(4);
                var r = new TextEncoder().encodeInto(s, d);
                [s.length, r.read, r.written].join('|')
            "),
            "4|2|4"
        );
        // Three bytes take a lone surrogate exactly, and stop.
        assert_eq!(
            run(r"
                var s = '\uD83Dx';
                var d = new Uint8Array(3);
                var r = new TextEncoder().encodeInto(s, d);
                [s.length, r.read, r.written].join('|')
            "),
            "2|1|3"
        );
    }

    #[test]
    fn encode_into_writes_whole_code_points_and_reports_utf16_units() {
        assert_eq!(
            run(r"
                var d = new Uint8Array(8);
                var r = new TextEncoder().encodeInto('hi', d);
                [r.read, r.written, Array.from(d.slice(0, 4)).join(',')].join('|')
            "),
            "2|2|104,105,0,0"
        );
        // An astral code point counts as two UTF-16 code units.
        assert_eq!(
            run(r"
                var d = new Uint8Array(5);
                var r = new TextEncoder().encodeInto('a\u{1F600}', d);
                [r.read, r.written].join('|')
            "),
            "3|5"
        );
        // A code point that does not fit is not partially written.
        assert_eq!(
            run(r"
                var d = new Uint8Array(3);
                var r = new TextEncoder().encodeInto('\u{1F600}', d);
                [r.read, r.written, Array.from(d).join(',')].join('|')
            "),
            "0|0|0,0,0"
        );
        assert_eq!(
            run(r"
                var d = new Uint8Array(3);
                var r = new TextEncoder().encodeInto('\u00E9', d);
                [r.read, r.written].join('|')
            "),
            "1|2"
        );
        assert_eq!(
            run(r"
                var d = new Uint8Array(2);
                var r = new TextEncoder().encodeInto('\uD800ab', d);
                [r.read, r.written].join('|')
            "),
            "0|0"
        );
    }

    #[test]
    fn labels_resolve_through_the_standard_table() {
        // `latin1` and `iso-8859-1` both name windows-1252, so 0x80 is EURO SIGN
        // and not U+0080. Identity comparison is used because this engine has
        // no `String.prototype.codePointAt`.
        for label in ["latin1", "iso-8859-1", "ascii", "us-ascii", "windows-1252"] {
            let observed = run(&format!(
                "new TextDecoder('{label}').encoding + ':' \
                 + (new TextDecoder('{label}').decode(new Uint8Array([0x80])) === '\\u20AC')"
            ));
            assert_eq!(observed, "windows-1252:true", "label {label}");
        }
        // Above 0x9F windows-1252 is the identity, which is what separates it
        // from a table lookup that wrongly runs to 0xFF.
        assert_eq!(
            run("new TextDecoder('latin1').decode(new Uint8Array([0x41,0xE9])) === 'A\u{e9}'"),
            "true"
        );
        assert_eq!(run("new TextDecoder('UTF8').encoding"), "utf-8");
        assert_eq!(
            run("new TextDecoder('x-user-defined').decode(new Uint8Array([0x80, 0x41])).length"),
            "2"
        );
        // A label the standard does not define is a RangeError, not a default.
        assert_eq!(
            run(r"
                function thrown(fn) { try { fn(); return 'no throw'; } catch (e) { return e.name; } }
                thrown(function () { return new TextDecoder('bogus-8'); })
            "),
            "RangeError"
        );
    }

    #[test]
    fn constructors_are_new_only() {
        assert_eq!(
            run(r"
                function thrown(fn) { try { fn(); return 'no throw'; } catch (e) { return e.name; } }
                [thrown(function () { TextEncoder(); }),
                 thrown(function () { TextDecoder(); }),
                 typeof new TextEncoder(),
                 typeof new TextDecoder()].join(',')
            "),
            "TypeError,TypeError,object,object"
        );
    }

    #[test]
    fn unpaired_surrogates_are_recognised_by_their_placeholder() {
        assert!(is_unpaired_surrogate(surrogate_placeholder(0xd800)));
        assert!(is_unpaired_surrogate(surrogate_placeholder(0xdfff)));
        assert!(!is_unpaired_surrogate('a'));
        assert!(!is_unpaired_surrogate(char::from_u32(0x1f600).unwrap()));
    }

    #[test]
    fn utf8_lead_rejects_the_forms_the_standard_rejects() {
        assert_eq!(utf8_lead(0xc0), None, "0xC0 is always overlong");
        assert_eq!(utf8_lead(0xc1), None, "0xC1 is always overlong");
        assert_eq!(utf8_lead(0xf5), None, "0xF5 is above U+10FFFF");
        assert_eq!(utf8_lead(0x80), None, "a continuation has no lead");
        assert_eq!(
            utf8_lead(0xed),
            Some((3, 0x80, 0x9f)),
            "no surrogate halves"
        );
        assert_eq!(utf8_lead(0xf4), Some((4, 0x80, 0x8f)), "caps at U+10FFFF");
    }

    #[test]
    fn windows_1252_high_block_matches_the_standard_index() {
        assert_eq!(WINDOWS_1252_HIGH[0], '\u{20ac}');
        assert_eq!(
            WINDOWS_1252_HIGH[1], '\u{0081}',
            "unmapped, so the C1 control"
        );
        // Byte 0x9E is U+017E, and byte 0x9D is the unmapped C1 control.
        assert_eq!(WINDOWS_1252_HIGH[0x1e], '\u{017e}');
        assert_eq!(WINDOWS_1252_HIGH[0x1d], '\u{009d}');
        assert_eq!(
            decode_single_byte(&[0x80], TextEncoding::Windows1252),
            "\u{20ac}"
        );
        assert_eq!(
            decode_single_byte(&[0x41, 0xe9], TextEncoding::Windows1252),
            "A\u{e9}"
        );
        assert_eq!(decode_single_byte(&[0x41], TextEncoding::XUserDefined), "A");
    }

    #[test]
    fn the_decoder_reports_its_pending_bytes_and_its_errors() {
        let held = decode_utf8(&[0xe2, 0x82], true, false);
        assert!(
            held.pending == vec![0xe2, 0x82],
            "a partial sequence is kept"
        );
        assert!(held.text.is_empty());
        assert!(!held.errored, "a partial sequence is not an error yet");
        let flushed = decode_utf8(&[0xe2, 0x82], false, false);
        assert!(flushed.pending.is_empty());
        assert_eq!(flushed.text, "\u{fffd}");
        assert!(flushed.errored);
        assert!(get_encoding("utf-16le").is_none());
        assert_eq!(get_encoding("utf-8"), Some(TextEncoding::Utf8));
    }

    #[test]
    fn replacement_is_the_only_error_output() {
        assert_eq!(REPLACEMENT, '\u{fffd}');
        assert_eq!(encode_utf8("\u{fffd}"), vec![0xef, 0xbf, 0xbd]);
    }
}
