//! Byte-to-Unicode decoding for HTML resources.
//!
//! The tokenizer consumes Unicode. This module implements the preceding HTML
//! encoding-sniffing stage so network and embedded callers do not make their
//! own incompatible charset guesses.

use std::error::Error;
use std::fmt;

use encoding_rs::{CoderResult, Encoding, UTF_8, UTF_16BE, UTF_16LE, WINDOWS_1252};

const META_PRESCAN_BYTES: usize = 1_024;
const DECODE_BUFFER_BYTES: usize = 8 * 1_024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HtmlDecodeLimits {
    pub max_input_bytes: usize,
    pub max_output_bytes: usize,
}

impl Default for HtmlDecodeLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 64 * 1024 * 1024,
            max_output_bytes: 256 * 1024 * 1024,
        }
    }
}

/// Inputs to the HTML encoding-sniffing algorithm.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HtmlDecodeOptions {
    /// Charset label supplied by HTTP or another transport layer.
    pub transport_encoding_label: Option<String>,
    /// The locale/application fallback used when no authoritative declaration
    /// exists. HTML's interoperable Western default is `windows-1252`.
    pub fallback_encoding_label: String,
    pub limits: HtmlDecodeLimits,
}

impl Default for HtmlDecodeOptions {
    fn default() -> Self {
        Self {
            transport_encoding_label: None,
            fallback_encoding_label: "windows-1252".to_owned(),
            limits: HtmlDecodeLimits::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HtmlEncodingSource {
    Bom,
    Transport,
    Meta,
    Fallback,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncodingDeclarationSource {
    Transport,
    Meta,
    Fallback,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HtmlDecodeDiagnosticCode {
    BomOverridesTransport,
    UnsupportedEncodingLabel,
    DisallowedEncodingLabel,
    EncodingAdjustedForHtml,
    MalformedMetaDeclaration,
    DecodingErrorReplaced,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HtmlDecodeDiagnostic {
    pub code: HtmlDecodeDiagnosticCode,
    pub declaration_source: Option<EncodingDeclarationSource>,
    pub byte_offset: Option<usize>,
    pub label: Option<String>,
}

/// Unicode HTML plus the observable encoding decision that produced it.
#[derive(Clone, Debug)]
pub struct DecodedHtml {
    pub text: String,
    pub encoding: &'static Encoding,
    pub source: HtmlEncodingSource,
    pub diagnostics: Vec<HtmlDecodeDiagnostic>,
}

impl DecodedHtml {
    #[must_use]
    pub fn encoding_name(&self) -> &'static str {
        self.encoding.name()
    }
}

/// Sniffs and decodes an HTML byte stream according to HTML and Encoding
/// Standard precedence: BOM, transport, the first 1024 bytes of meta markup,
/// then the configured fallback.
///
/// # Errors
///
/// Returns an error before parsing for input/output resource-limit violations,
/// or when the mandatory fallback label is unsupported or disallowed.
pub fn decode_html_bytes(
    input: &[u8],
    options: &HtmlDecodeOptions,
) -> Result<DecodedHtml, HtmlDecodeError> {
    enforce_input_limit(input, options.limits)?;
    let mut diagnostics = Vec::new();

    let decision = if let Some((encoding, bom_length)) = sniff_bom(input) {
        if options.transport_encoding_label.is_some() {
            diagnostics.push(HtmlDecodeDiagnostic {
                code: HtmlDecodeDiagnosticCode::BomOverridesTransport,
                declaration_source: Some(EncodingDeclarationSource::Transport),
                byte_offset: Some(0),
                label: options.transport_encoding_label.clone(),
            });
        }
        EncodingDecision {
            encoding,
            source: HtmlEncodingSource::Bom,
            byte_start: bom_length,
        }
    } else {
        sniff_without_bom(input, options, &mut diagnostics)?
    };

    let (text, had_errors) = decode_with_limit(
        decision.encoding,
        &input[decision.byte_start..],
        options.limits.max_output_bytes,
    )?;
    if had_errors {
        diagnostics.push(HtmlDecodeDiagnostic {
            code: HtmlDecodeDiagnosticCode::DecodingErrorReplaced,
            declaration_source: None,
            byte_offset: None,
            label: Some(decision.encoding.name().to_owned()),
        });
    }
    Ok(DecodedHtml {
        text,
        encoding: decision.encoding,
        source: decision.source,
        diagnostics,
    })
}

#[derive(Clone, Copy)]
struct EncodingDecision {
    encoding: &'static Encoding,
    source: HtmlEncodingSource,
    byte_start: usize,
}

fn sniff_without_bom(
    input: &[u8],
    options: &HtmlDecodeOptions,
    diagnostics: &mut Vec<HtmlDecodeDiagnostic>,
) -> Result<EncodingDecision, HtmlDecodeError> {
    if let Some(label) = &options.transport_encoding_label
        && let Some(encoding) = resolve_declared_encoding(
            label,
            EncodingDeclarationSource::Transport,
            None,
            false,
            diagnostics,
        )
    {
        return Ok(EncodingDecision {
            encoding,
            source: HtmlEncodingSource::Transport,
            byte_start: 0,
        });
    }

    for declaration in prescan_meta_declarations(input, diagnostics) {
        if let Some(encoding) = resolve_declared_encoding(
            &declaration.label,
            EncodingDeclarationSource::Meta,
            Some(declaration.offset),
            true,
            diagnostics,
        ) {
            return Ok(EncodingDecision {
                encoding,
                source: HtmlEncodingSource::Meta,
                byte_start: 0,
            });
        }
    }

    let fallback = resolve_fallback_encoding(&options.fallback_encoding_label, diagnostics)?;
    Ok(EncodingDecision {
        encoding: fallback,
        source: HtmlEncodingSource::Fallback,
        byte_start: 0,
    })
}

fn sniff_bom(input: &[u8]) -> Option<(&'static Encoding, usize)> {
    if input.starts_with(&[0xEF, 0xBB, 0xBF]) {
        Some((UTF_8, 3))
    } else if input.starts_with(&[0xFF, 0xFE]) {
        Some((UTF_16LE, 2))
    } else if input.starts_with(&[0xFE, 0xFF]) {
        Some((UTF_16BE, 2))
    } else {
        None
    }
}

fn resolve_declared_encoding(
    label: &str,
    source: EncodingDeclarationSource,
    offset: Option<usize>,
    adjust_for_meta: bool,
    diagnostics: &mut Vec<HtmlDecodeDiagnostic>,
) -> Option<&'static Encoding> {
    let normalized = trim_ascii_whitespace(label);
    if is_disallowed_label(normalized) {
        push_label_diagnostic(
            diagnostics,
            HtmlDecodeDiagnosticCode::DisallowedEncodingLabel,
            source,
            offset,
            label,
        );
        return None;
    }
    let Some(mut encoding) = Encoding::for_label(normalized.as_bytes()) else {
        push_label_diagnostic(
            diagnostics,
            HtmlDecodeDiagnosticCode::UnsupportedEncodingLabel,
            source,
            offset,
            label,
        );
        return None;
    };
    if encoding.name().eq_ignore_ascii_case("replacement") {
        push_label_diagnostic(
            diagnostics,
            HtmlDecodeDiagnosticCode::DisallowedEncodingLabel,
            source,
            offset,
            label,
        );
        return None;
    }
    if adjust_for_meta {
        let adjusted = adjust_meta_encoding(encoding);
        if !std::ptr::eq(adjusted, encoding) {
            push_label_diagnostic(
                diagnostics,
                HtmlDecodeDiagnosticCode::EncodingAdjustedForHtml,
                source,
                offset,
                label,
            );
            encoding = adjusted;
        }
    }
    Some(encoding)
}

fn resolve_fallback_encoding(
    label: &str,
    diagnostics: &mut Vec<HtmlDecodeDiagnostic>,
) -> Result<&'static Encoding, HtmlDecodeError> {
    let normalized = trim_ascii_whitespace(label);
    if is_disallowed_label(normalized) {
        return Err(HtmlDecodeError::InvalidFallbackEncoding {
            label: label.to_owned(),
            disallowed: true,
        });
    }
    let encoding = Encoding::for_label(normalized.as_bytes()).ok_or_else(|| {
        HtmlDecodeError::InvalidFallbackEncoding {
            label: label.to_owned(),
            disallowed: false,
        }
    })?;
    if encoding.name().eq_ignore_ascii_case("replacement") {
        return Err(HtmlDecodeError::InvalidFallbackEncoding {
            label: label.to_owned(),
            disallowed: true,
        });
    }
    let adjusted = adjust_meta_encoding(encoding);
    if !std::ptr::eq(adjusted, encoding) {
        push_label_diagnostic(
            diagnostics,
            HtmlDecodeDiagnosticCode::EncodingAdjustedForHtml,
            EncodingDeclarationSource::Fallback,
            None,
            label,
        );
    }
    Ok(adjusted)
}

fn adjust_meta_encoding(encoding: &'static Encoding) -> &'static Encoding {
    if std::ptr::eq(encoding, UTF_16LE) || std::ptr::eq(encoding, UTF_16BE) {
        UTF_8
    } else if encoding.name().eq_ignore_ascii_case("x-user-defined") {
        WINDOWS_1252
    } else {
        encoding
    }
}

fn is_disallowed_label(label: &str) -> bool {
    matches!(
        label.to_ascii_lowercase().as_str(),
        "utf-7"
            | "unicode-1-1-utf-7"
            | "utf-32"
            | "utf-32le"
            | "utf-32be"
            | "bocu-1"
            | "cesu-8"
            | "scsu"
            | "utf-ebcdic"
    )
}

fn push_label_diagnostic(
    diagnostics: &mut Vec<HtmlDecodeDiagnostic>,
    code: HtmlDecodeDiagnosticCode,
    source: EncodingDeclarationSource,
    offset: Option<usize>,
    label: &str,
) {
    diagnostics.push(HtmlDecodeDiagnostic {
        code,
        declaration_source: Some(source),
        byte_offset: offset,
        label: Some(label.to_owned()),
    });
}

#[derive(Clone, Debug)]
struct MetaDeclaration {
    label: String,
    offset: usize,
}

#[derive(Clone, Debug)]
struct PrescanAttribute {
    name: String,
    value: String,
}

fn prescan_meta_declarations(
    input: &[u8],
    diagnostics: &mut Vec<HtmlDecodeDiagnostic>,
) -> Vec<MetaDeclaration> {
    let bytes = &input[..input.len().min(META_PRESCAN_BYTES)];
    let mut declarations = Vec::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        let Some(relative) = bytes[cursor..].iter().position(|byte| *byte == b'<') else {
            break;
        };
        cursor += relative;
        if bytes[cursor..].starts_with(b"<!--") {
            cursor = find_after(bytes, cursor + 4, b"-->").unwrap_or(bytes.len());
            continue;
        }
        if is_meta_tag_start(bytes, cursor) {
            let tag_offset = cursor;
            let (attributes, next) = parse_prescan_attributes(bytes, cursor + 5);
            collect_meta_declaration(&attributes, tag_offset, diagnostics, &mut declarations);
            cursor = next.max(cursor + 1);
            continue;
        }
        cursor = skip_non_meta_markup(bytes, cursor).max(cursor + 1);
    }
    declarations
}

fn collect_meta_declaration(
    attributes: &[PrescanAttribute],
    offset: usize,
    diagnostics: &mut Vec<HtmlDecodeDiagnostic>,
    declarations: &mut Vec<MetaDeclaration>,
) {
    if let Some(charset) = first_attribute(attributes, "charset") {
        if charset.is_empty() {
            push_malformed_meta(diagnostics, offset);
        } else {
            declarations.push(MetaDeclaration {
                label: charset.to_owned(),
                offset,
            });
        }
        return;
    }

    let pragma = first_attribute(attributes, "http-equiv")
        .is_some_and(|value| value.eq_ignore_ascii_case("content-type"));
    let Some(content) = first_attribute(attributes, "content") else {
        return;
    };
    if !pragma {
        return;
    }
    match extract_charset_from_content(content) {
        ContentCharset::Label(label) => declarations.push(MetaDeclaration { label, offset }),
        ContentCharset::Malformed => push_malformed_meta(diagnostics, offset),
        ContentCharset::Missing => {}
    }
}

fn push_malformed_meta(diagnostics: &mut Vec<HtmlDecodeDiagnostic>, offset: usize) {
    diagnostics.push(HtmlDecodeDiagnostic {
        code: HtmlDecodeDiagnosticCode::MalformedMetaDeclaration,
        declaration_source: Some(EncodingDeclarationSource::Meta),
        byte_offset: Some(offset),
        label: None,
    });
}

fn is_meta_tag_start(bytes: &[u8], offset: usize) -> bool {
    bytes
        .get(offset + 1..offset + 5)
        .is_some_and(|name| name.eq_ignore_ascii_case(b"meta"))
        && bytes
            .get(offset + 5)
            .is_none_or(|byte| byte.is_ascii_whitespace() || matches!(*byte, b'/' | b'>'))
}

fn parse_prescan_attributes(bytes: &[u8], mut cursor: usize) -> (Vec<PrescanAttribute>, usize) {
    let mut attributes = Vec::new();
    while cursor < bytes.len() {
        skip_ascii_whitespace(bytes, &mut cursor);
        while matches!(bytes.get(cursor), Some(b'/')) {
            cursor += 1;
            skip_ascii_whitespace(bytes, &mut cursor);
        }
        match bytes.get(cursor) {
            None => break,
            Some(b'>') => return (attributes, cursor + 1),
            Some(b'<') => return (attributes, cursor),
            Some(_) => {}
        }
        let name_start = cursor;
        while bytes.get(cursor).is_some_and(|byte| {
            !byte.is_ascii_whitespace() && !matches!(*byte, b'=' | b'/' | b'>' | b'<')
        }) {
            cursor += 1;
        }
        if name_start == cursor {
            cursor += 1;
            continue;
        }
        let name = ascii_lowercase(&bytes[name_start..cursor]);
        skip_ascii_whitespace(bytes, &mut cursor);
        let value = if bytes.get(cursor) == Some(&b'=') {
            cursor += 1;
            skip_ascii_whitespace(bytes, &mut cursor);
            parse_attribute_value(bytes, &mut cursor)
        } else {
            String::new()
        };
        if !attributes
            .iter()
            .any(|attribute: &PrescanAttribute| attribute.name == name)
        {
            attributes.push(PrescanAttribute { name, value });
        }
    }
    (attributes, cursor)
}

fn parse_attribute_value(bytes: &[u8], cursor: &mut usize) -> String {
    let Some(first) = bytes.get(*cursor).copied() else {
        return String::new();
    };
    if matches!(first, b'\'' | b'"') {
        *cursor += 1;
        let start = *cursor;
        while bytes.get(*cursor).is_some_and(|byte| *byte != first) {
            *cursor += 1;
        }
        let value = String::from_utf8_lossy(&bytes[start..*cursor]).into_owned();
        if bytes.get(*cursor) == Some(&first) {
            *cursor += 1;
        }
        value
    } else {
        let start = *cursor;
        while bytes
            .get(*cursor)
            .is_some_and(|byte| !byte.is_ascii_whitespace() && *byte != b'>')
        {
            *cursor += 1;
        }
        String::from_utf8_lossy(&bytes[start..*cursor]).into_owned()
    }
}

fn skip_non_meta_markup(bytes: &[u8], offset: usize) -> usize {
    let Some(next) = bytes.get(offset + 1).copied() else {
        return bytes.len();
    };
    if !next.is_ascii_alphabetic() && !matches!(next, b'/' | b'!' | b'?') {
        return offset + 1;
    }
    let mut cursor = offset + 1;
    let mut quote = None;
    while let Some(byte) = bytes.get(cursor).copied() {
        if let Some(expected) = quote {
            if byte == expected {
                quote = None;
            }
        } else if matches!(byte, b'\'' | b'"') {
            quote = Some(byte);
        } else if byte == b'>' {
            return cursor + 1;
        }
        cursor += 1;
    }
    bytes.len()
}

fn find_after(bytes: &[u8], start: usize, needle: &[u8]) -> Option<usize> {
    bytes[start..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|relative| start + relative + needle.len())
}

fn first_attribute<'a>(attributes: &'a [PrescanAttribute], name: &str) -> Option<&'a str> {
    attributes
        .iter()
        .find(|attribute| attribute.name == name)
        .map(|attribute| attribute.value.as_str())
}

enum ContentCharset {
    Missing,
    Malformed,
    Label(String),
}

fn extract_charset_from_content(content: &str) -> ContentCharset {
    let bytes = content.as_bytes();
    let mut cursor = 0;
    while cursor + 7 <= bytes.len() {
        if !bytes[cursor..cursor + 7].eq_ignore_ascii_case(b"charset") {
            cursor += 1;
            continue;
        }
        cursor += 7;
        skip_ascii_whitespace(bytes, &mut cursor);
        if bytes.get(cursor) != Some(&b'=') {
            continue;
        }
        cursor += 1;
        skip_ascii_whitespace(bytes, &mut cursor);
        let Some(first) = bytes.get(cursor).copied() else {
            return ContentCharset::Malformed;
        };
        let (start, end) = if matches!(first, b'\'' | b'"') {
            cursor += 1;
            let start = cursor;
            while bytes.get(cursor).is_some_and(|byte| *byte != first) {
                cursor += 1;
            }
            (start, cursor)
        } else {
            let start = cursor;
            while bytes.get(cursor).is_some_and(|byte| {
                !byte.is_ascii_whitespace() && !matches!(*byte, b';' | b'\'' | b'"')
            }) {
                cursor += 1;
            }
            (start, cursor)
        };
        if start == end {
            return ContentCharset::Malformed;
        }
        return ContentCharset::Label(content[start..end].to_owned());
    }
    ContentCharset::Missing
}

fn skip_ascii_whitespace(bytes: &[u8], cursor: &mut usize) {
    while bytes.get(*cursor).is_some_and(u8::is_ascii_whitespace) {
        *cursor += 1;
    }
}

fn ascii_lowercase(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| byte.to_ascii_lowercase() as char)
        .collect()
}

fn trim_ascii_whitespace(value: &str) -> &str {
    value.trim_matches(|character: char| character.is_ascii_whitespace())
}

fn enforce_input_limit(input: &[u8], limits: HtmlDecodeLimits) -> Result<(), HtmlDecodeError> {
    if input.len() > limits.max_input_bytes {
        return Err(HtmlDecodeError::InputLimitExceeded {
            limit: limits.max_input_bytes,
            actual: input.len(),
        });
    }
    Ok(())
}

fn decode_with_limit(
    encoding: &'static Encoding,
    input: &[u8],
    max_output_bytes: usize,
) -> Result<(String, bool), HtmlDecodeError> {
    let mut decoder = encoding.new_decoder_without_bom_handling();
    let mut output = String::with_capacity(input.len().min(max_output_bytes));
    let mut input_offset = 0;
    let mut had_errors = false;
    let mut buffer = [0_u8; DECODE_BUFFER_BYTES];
    loop {
        let (result, read, written, errors) =
            decoder.decode_to_utf8(&input[input_offset..], &mut buffer, true);
        input_offset += read;
        had_errors |= errors;
        let resulting_length = output.len().saturating_add(written);
        if resulting_length > max_output_bytes {
            return Err(HtmlDecodeError::OutputLimitExceeded {
                limit: max_output_bytes,
                actual_at_least: resulting_length,
            });
        }
        let output_chunk = std::str::from_utf8(&buffer[..written])
            .expect("encoding_rs must emit structurally valid UTF-8");
        output.push_str(output_chunk);
        match result {
            CoderResult::InputEmpty => return Ok((output, had_errors)),
            CoderResult::OutputFull => {}
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HtmlDecodeError {
    InputLimitExceeded {
        limit: usize,
        actual: usize,
    },
    OutputLimitExceeded {
        limit: usize,
        actual_at_least: usize,
    },
    InvalidFallbackEncoding {
        label: String,
        disallowed: bool,
    },
}

impl fmt::Display for HtmlDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InputLimitExceeded { limit, actual } => {
                write!(formatter, "HTML input is {actual} bytes; limit is {limit}")
            }
            Self::OutputLimitExceeded {
                limit,
                actual_at_least,
            } => write!(
                formatter,
                "decoded HTML is at least {actual_at_least} bytes; limit is {limit}"
            ),
            Self::InvalidFallbackEncoding { label, disallowed } => {
                let reason = if *disallowed {
                    "is disallowed for HTML"
                } else {
                    "is not a supported Encoding Standard label"
                };
                write!(formatter, "fallback encoding '{label}' {reason}")
            }
        }
    }
}

impl Error for HtmlDecodeError {}

#[cfg(test)]
mod tests {
    use super::{
        EncodingDeclarationSource, HtmlDecodeDiagnosticCode, HtmlDecodeError, HtmlDecodeLimits,
        HtmlDecodeOptions, HtmlEncodingSource, decode_html_bytes,
    };

    #[test]
    fn utf8_and_utf16_boms_have_highest_priority_and_are_removed() {
        let options = HtmlDecodeOptions {
            transport_encoding_label: Some("windows-1252".to_owned()),
            ..HtmlDecodeOptions::default()
        };
        let utf8 = decode_html_bytes(b"\xEF\xBB\xBF<p>\xE4\xB8\xAD</p>", &options).unwrap();
        assert_eq!(utf8.text, "<p>中</p>");
        assert_eq!(utf8.encoding_name(), "UTF-8");
        assert_eq!(utf8.source, HtmlEncodingSource::Bom);
        assert_eq!(
            utf8.diagnostics[0].code,
            HtmlDecodeDiagnosticCode::BomOverridesTransport
        );

        let utf16 = decode_html_bytes(
            &[0xFF, 0xFE, b'<', 0, b'p', 0, b'>', 0, 0x2D, 0x4E],
            &HtmlDecodeOptions::default(),
        )
        .unwrap();
        assert_eq!(utf16.text, "<p>中");
        assert_eq!(utf16.encoding_name(), "UTF-16LE");
    }

    #[test]
    fn transport_encoding_wins_over_conflicting_meta() {
        let mut bytes = b"<meta charset=shift_jis><p>".to_vec();
        bytes.extend_from_slice(&[0xD6, 0xD0, 0xCE, 0xC4]);
        let decoded = decode_html_bytes(
            &bytes,
            &HtmlDecodeOptions {
                transport_encoding_label: Some("gb18030".to_owned()),
                ..HtmlDecodeOptions::default()
            },
        )
        .unwrap();
        assert_eq!(decoded.source, HtmlEncodingSource::Transport);
        assert_eq!(decoded.encoding_name(), "gb18030");
        assert!(decoded.text.ends_with("中文"));
    }

    #[test]
    fn gbk_meta_and_http_equiv_are_found_during_ascii_prescan() {
        let mut direct = b"<!doctype html><meta charset='GBK'><p>".to_vec();
        direct.extend_from_slice(&[0xD6, 0xD0, 0xCE, 0xC4]);
        let decoded = decode_html_bytes(&direct, &HtmlDecodeOptions::default()).unwrap();
        assert_eq!(decoded.source, HtmlEncodingSource::Meta);
        assert!(decoded.text.ends_with("中文"));

        let mut pragma =
            b"<META content='text/html; charset=gb18030' HTTP-EQUIV=Content-Type>".to_vec();
        pragma.extend_from_slice(&[0xD6, 0xD0]);
        let decoded = decode_html_bytes(&pragma, &HtmlDecodeOptions::default()).unwrap();
        assert_eq!(decoded.source, HtmlEncodingSource::Meta);
        assert!(decoded.text.ends_with('中'));
    }

    #[test]
    fn invalid_meta_recovers_to_a_later_valid_declaration() {
        let mut bytes =
            b"<meta charset=><meta charset=not-a-real-encoding><meta charset=gbk>".to_vec();
        bytes.extend_from_slice(&[0xD6, 0xD0]);
        let decoded = decode_html_bytes(&bytes, &HtmlDecodeOptions::default()).unwrap();
        assert_eq!(decoded.source, HtmlEncodingSource::Meta);
        assert!(decoded.text.ends_with('中'));
        assert!(decoded.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == HtmlDecodeDiagnosticCode::UnsupportedEncodingLabel
                && diagnostic.declaration_source == Some(EncodingDeclarationSource::Meta)
        }));
        assert!(decoded.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == HtmlDecodeDiagnosticCode::MalformedMetaDeclaration
        }));
    }

    #[test]
    fn fallback_is_explicit_and_windows_1252_is_interoperable_default() {
        let decoded =
            decode_html_bytes(b"<p>price \x80</p>", &HtmlDecodeOptions::default()).unwrap();
        assert_eq!(decoded.source, HtmlEncodingSource::Fallback);
        assert_eq!(decoded.encoding_name(), "windows-1252");
        assert_eq!(decoded.text, "<p>price €</p>");
    }

    #[test]
    fn shift_jis_transport_label_uses_encoding_standard_decoder() {
        let decoded = decode_html_bytes(
            &[0x82, 0xA0],
            &HtmlDecodeOptions {
                transport_encoding_label: Some("Shift_JIS".to_owned()),
                ..HtmlDecodeOptions::default()
            },
        )
        .unwrap();
        assert_eq!(decoded.text, "あ");
        assert_eq!(decoded.encoding_name(), "Shift_JIS");
    }

    #[test]
    fn disallowed_utf7_is_ignored_or_rejected_by_declaration_source() {
        let decoded = decode_html_bytes(
            b"<meta charset=utf-7><p>ascii</p>",
            &HtmlDecodeOptions::default(),
        )
        .unwrap();
        assert_eq!(decoded.source, HtmlEncodingSource::Fallback);
        assert!(decoded.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == HtmlDecodeDiagnosticCode::DisallowedEncodingLabel
        }));

        let error = decode_html_bytes(
            b"ascii",
            &HtmlDecodeOptions {
                fallback_encoding_label: "utf-7".to_owned(),
                ..HtmlDecodeOptions::default()
            },
        )
        .unwrap_err();
        assert!(matches!(
            error,
            HtmlDecodeError::InvalidFallbackEncoding {
                disallowed: true,
                ..
            }
        ));
    }

    #[test]
    fn meta_after_first_1024_bytes_does_not_change_fallback() {
        let mut bytes = vec![b' '; 1_025];
        bytes.extend_from_slice(b"<meta charset=utf-8>");
        let decoded = decode_html_bytes(&bytes, &HtmlDecodeOptions::default()).unwrap();
        assert_eq!(decoded.source, HtmlEncodingSource::Fallback);
    }

    #[test]
    fn input_and_decoded_output_limits_fail_explicitly() {
        let input_error = decode_html_bytes(
            b"1234",
            &HtmlDecodeOptions {
                limits: HtmlDecodeLimits {
                    max_input_bytes: 3,
                    max_output_bytes: 100,
                },
                ..HtmlDecodeOptions::default()
            },
        )
        .unwrap_err();
        assert!(matches!(
            input_error,
            HtmlDecodeError::InputLimitExceeded { .. }
        ));

        let output_error = decode_html_bytes(
            b"1234",
            &HtmlDecodeOptions {
                limits: HtmlDecodeLimits {
                    max_input_bytes: 100,
                    max_output_bytes: 3,
                },
                ..HtmlDecodeOptions::default()
            },
        )
        .unwrap_err();
        assert!(matches!(
            output_error,
            HtmlDecodeError::OutputLimitExceeded { .. }
        ));
    }

    #[test]
    fn malformed_sequences_are_replaced_and_diagnosed() {
        let decoded = decode_html_bytes(
            &[0xFF],
            &HtmlDecodeOptions {
                transport_encoding_label: Some("utf-8".to_owned()),
                ..HtmlDecodeOptions::default()
            },
        )
        .unwrap();
        assert_eq!(decoded.text, "�");
        assert!(decoded.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == HtmlDecodeDiagnosticCode::DecodingErrorReplaced
        }));
    }
}
