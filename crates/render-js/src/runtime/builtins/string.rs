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

use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::regex::MatchRanges;
use crate::runtime::JsRuntime;
use crate::runtime::builtins::array::MAX_MATERIALIZED_ELEMENTS;
use crate::runtime::convert::required_argument;
use crate::runtime::convert::slice_range;
use crate::runtime::convert::uint32_of_number;
use crate::utf16;
use crate::value::NativeFunction;
use crate::value::ObjectHost;
use crate::value::number_to_string;
use render_dom::Dom;

impl JsRuntime {
    pub(in crate::runtime) fn dispatch_string_native(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match function {
            NativeFunction::StringFromCharCode => self.string_from_char_code(dom, arguments),
            NativeFunction::StringFromCodePoint => self.string_from_code_point(dom, arguments),
            NativeFunction::StringRaw => self.string_raw(dom, arguments),
            // ECMA-262 22.1.3.4 `String.prototype.codePointAt`, which is
            // §11.1.4 `CodePointAt` over the code-unit list. This is the one
            // method where both notions are visible at once: the *position* is
            // a code-unit offset into the string, and the *answer* is a code
            // point, so `'😀'.codePointAt(0)` is `0x1F600` and
            // `'😀a'.codePointAt(2)` is `97`.
            //
            // A position that does not begin a valid pair reports that single
            // code unit's own value rather than `undefined`: the trailing half
            // of a pair, a leading surrogate with no partner, and a leading
            // surrogate followed by an ordinary character all answer
            // themselves. Only a position outside the string is `undefined`,
            // and a negative one is outside too, because step 5 is a plain
            // bounds check - unlike `at`, which counts from the end.
            NativeFunction::StrCodePointAt => {
                let text = self.require_string_receiver(receiver)?;
                let position = self.optional_integer_value(dom, arguments.first())?;
                if !position.is_finite() || position < 0.0 {
                    return Ok(JsValue::Undefined);
                }
                let units = utf16::utf16_units(&text);
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "the bounds check below bounds the index by the code-unit length"
                )]
                let index = position as usize;
                if index >= units.len() {
                    return Ok(JsValue::Undefined);
                }
                Ok(JsValue::Number(f64::from(utf16::code_point_at(
                    &units, index,
                ))))
            }
            // ECMA-262 22.1.3.1 `String.prototype.at`, over code units: step 5
            // is `ToAbsoluteIndex(index, length)` and step 7 is "the substring
            // of string from k to k + 1". One code unit, so a position inside a
            // surrogate pair returns that half - the same answer `charAt` gives,
            // which is exactly why the two agree on `codePointAt`'s subject.
            NativeFunction::StrAt => {
                let text = self.require_string_receiver(receiver)?;
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "a code-unit length is a usize and f64 represents every usize on this target"
                )]
                let length = utf16::utf16_length(&text) as f64;
                let relative = self.optional_integer_value(dom, arguments.first())?;
                let index = if relative >= 0.0 {
                    relative
                } else {
                    length + relative
                };
                if index < 0.0 || index >= length {
                    return Ok(JsValue::Undefined);
                }
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "the bounds check above bounds the index by the code-unit length"
                )]
                let index = index as usize;
                Ok(JsValue::String(utf16::string_from_unit(
                    utf16::utf16_units(&text)[index],
                )))
            }
            // ECMA-262 22.1.3.17 `padStart` and 22.1.3.18 `padEnd`, which is
            // §22.1.3.17.2 `StringPad` over code units.
            //
            // Step 4 of `StringPad` is "the String value consisting of repeated
            // concatenations of fillString **truncated to length fillLength**",
            // and `fillLength` is `maxLength - stringLength` measured in code
            // units. Two consequences an implementation that repeats the
            // filler's *first code point* gets wrong:
            //
            //  - a multi-unit filler alternates rather than repeating one
            //    character: `'abc'.padStart(6, 'xy')` is `'xyxabc'`, not
            //    `'xxxabc'`; and
            //  - the truncation boundary is a code-unit boundary, so an astral
            //    filler *can* be cut in half. `''.padStart(3, '\u{1F600}')` is
            //    `'\uD83D\uDE00\uD83D'` - three code units whose last is a
            //    lone high surrogate. That is the specified answer, and it is
            //    the same rule that lets `slice` cut a pair in half.
            NativeFunction::StrPadStart | NativeFunction::StrPadEnd => {
                let text = self.require_string_receiver(receiver)?;
                // Step 2 is `ToLength(maxLength)`, which truncates toward zero
                // and clamps a negative or non-finite argument to 0.
                let target =
                    self.to_length_value(dom, arguments.first().unwrap_or(&JsValue::Number(0.0)))?;
                // The engine's materialization cap is the bound here for the same
                // reason it bounds every other array/string materialization: a
                // `padStart(1e9)` must not allocate a gigabyte.
                if target > MAX_MATERIALIZED_ELEMENTS as f64 {
                    return Err(
                        self.range_error("String.prototype pad length exceeds the engine limit")
                    );
                }
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "the cap above bounds the target to the materialization limit"
                )]
                let target = target as usize;
                let current = utf16::utf16_length(&text);
                if current >= target {
                    return Ok(JsValue::String(text));
                }
                let filler = match arguments.get(1) {
                    Some(JsValue::Undefined) | None => vec![0x20],
                    Some(value) => utf16::utf16_units(&value.to_js_string()),
                };
                // Step 3 of `StringPad`: an empty filler pads by nothing at all.
                if filler.is_empty() {
                    return Ok(JsValue::String(text));
                }
                let fill_length = target - current;
                let mut padding: Vec<u16> = Vec::with_capacity(fill_length);
                while padding.len() < fill_length {
                    // `filler` is non-empty, so the inner loop always appends at
                    // least one unit and this terminates.
                    for unit in &filler {
                        if padding.len() == fill_length {
                            break;
                        }
                        padding.push(*unit);
                    }
                }
                let padding = utf16::string_from_utf16(&padding);
                let mut units = utf16::utf16_units(&text);
                if function == NativeFunction::StrPadStart {
                    let mut padded = utf16::utf16_units(&padding);
                    padded.append(&mut units);
                    units = padded;
                } else {
                    units.extend(utf16::utf16_units(&padding));
                }
                Ok(JsValue::String(utf16::string_from_utf16(&units)))
            }
            NativeFunction::StrTrimStart => self.string_trim_end(receiver, true),
            NativeFunction::StrTrimEnd => self.string_trim_end(receiver, false),
            // ECMA-262 22.1.3.19 `String.prototype.repeat`. A negative or
            // infinite count is a `RangeError` from `StringRepeat`, and a `NaN`
            // count is `0` because `ToIntegerOrInfinity(NaN)` is 0. The copies
            // are of the *code-unit sequence*, so `'\uD83D'.repeat(2)` is two
            // lone surrogates rather than one pair: `repeat` neither splits a
            // pair nor joins two halves that were not one.
            NativeFunction::StrRepeat => {
                let text = self.require_string_receiver(receiver)?;
                let count = self.optional_integer_value(dom, arguments.first())?;
                if count < 0.0 || !count.is_finite() {
                    return Err(self.range_error("String.prototype.repeat count is out of range"));
                }
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "the range check above bounds the count to a non-negative integer"
                )]
                let count = count as usize;
                let units = utf16::utf16_units(&text);
                let total = units
                    .len()
                    .checked_mul(count)
                    .filter(|total| *total <= MAX_MATERIALIZED_ELEMENTS);
                let Some(total) = total else {
                    return Err(
                        self.range_error("String.prototype.repeat result exceeds the engine limit")
                    );
                };
                let mut repeated: Vec<u16> = Vec::with_capacity(total);
                for _ in 0..count {
                    repeated.extend_from_slice(&units);
                }
                Ok(JsValue::String(utf16::string_from_utf16(&repeated)))
            }
            // ECMA-262 22.1.3.17 `String.prototype.localeCompare`. The
            // specification makes the collation implementation-defined ("an
            // implementation-defined string ordering"), and this engine has one
            // locale and no collation table, so the honest answer is a comparison
            // of the strings themselves: a negative, zero or positive number
            // saying which sorts first, which is the part every caller reads. A
            // hardcoded `0` would be the "exists, looks right, silently wrong"
            // answer: `sort` would treat every pair as equal and leave the input
            // order in place.
            NativeFunction::StrLocaleCompare => {
                let text = self.require_string_receiver(receiver)?;
                let other = required_argument(arguments, 0, "localeCompare")?.to_js_string();
                Ok(JsValue::Number(match text.as_str().cmp(other.as_str()) {
                    std::cmp::Ordering::Less => -1.0,
                    std::cmp::Ordering::Equal => 0.0,
                    std::cmp::Ordering::Greater => 1.0,
                }))
            }
            // ECMA-262 22.1.3.19 `String.prototype.replaceAll`. The two entry
            // points share `GetSubstitution`; the only difference is `global`.
            // For a regular expression that is a *check* - a non-global `RegExp`
            // is a `TypeError`, because silently treating it as `replace` would
            // give a different answer than the same expression with `g` - and for
            // a string pattern it is the search loop, which runs to the end of
            // the input rather than stopping at the first hit.
            NativeFunction::StrReplaceAll => {
                let search = required_argument(arguments, 0, "replaceAll")?.clone();
                let replacement = arguments.get(1).cloned().unwrap_or(JsValue::Undefined);
                if matches!(search, JsValue::Object(_))
                    && let (_, global) = self.coerce_pattern_argument(&search)?
                    && !global
                {
                    return Err(JsError::type_error(
                        "String.prototype.replaceAll must be called with a global RegExp",
                    ));
                }
                // `coerce_pattern_argument` turns a string search value into a
                // non-global pattern, so the literal case is handled here rather
                // than by delegating.
                if matches!(search, JsValue::Object(_)) {
                    return self.string_replace(dom, receiver, arguments);
                }
                self.string_replace_all_literal(dom, receiver, &search.to_js_string(), &replacement)
            }
            // ECMA-262 B.2.2.1 `String.prototype.substr`, over code units.
            // `intStart` is `ToClampedIndex(start, size)` - a negative start
            // counts from the end - and the run length is clamped to `[0, size]`
            // rather than resolved from the end, so `substr(0, -1)` is empty
            // and `substr(-1)` is the last code unit.
            NativeFunction::StringSubstr => {
                let text = self.require_string_receiver(receiver)?;
                let units = utf16::utf16_units(&text);
                let size = units.len();
                let start = slice_range(
                    size,
                    self.optional_integer_value(dom, arguments.first())?,
                    None,
                    true,
                )
                .start;
                let length = match arguments.get(1) {
                    None | Some(JsValue::Undefined) => size - start,
                    Some(value) => {
                        let requested = self.to_integer_value(dom, value)?;
                        #[allow(
                            clippy::cast_possible_truncation,
                            clippy::cast_sign_loss,
                            reason = "the length is clamped to the string size first"
                        )]
                        {
                            requested.max(0.0).min(size as f64) as usize
                        }
                    }
                };
                let end = start.saturating_add(length).min(size);
                Ok(JsValue::String(utf16::string_from_utf16(
                    &units[start..end],
                )))
            }
            other => self.dispatch_regexp_native(dom, other, receiver, arguments),
        }
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "positions are validated against the string length first"
)]
pub(in crate::runtime) fn valid_position(position: f64, length: usize) -> Option<usize> {
    if !position.is_finite() {
        return None;
    }
    let position = position.floor();
    if position < 0.0 {
        return None;
    }
    #[allow(clippy::cast_precision_loss, reason = "length fits exactly")]
    let length = length as f64;
    if position >= length {
        return None;
    }
    Some(position as usize)
}

/// ECMA-262 22.1.3.2 `String.prototype.charAt`: "the substring of string from
/// position to position + 1" over code units.
///
/// A position inside a surrogate pair returns that single code unit, so
/// `'\u{1F600}'.charAt(0)` is the lone high surrogate `'\uD83D'`. That is the
/// specified answer and it is the reason `codePointAt` exists; an
/// implementation that returned the whole character here could not reach the
/// trailing half at all, and `substring(0, 1)` would disagree with `charAt(0)`
/// even though the specification defines the first *in terms of* the second.
pub(in crate::runtime) fn char_at_value(units: &[u16], position: f64) -> JsValue {
    match valid_position(position, units.len()) {
        Some(position) => JsValue::String(utf16::string_from_unit(units[position])),
        None => JsValue::String(String::new()),
    }
}

/// Collect spans of every non-overlapping match honouring empty-match
/// advancement; used by global matching.
pub(in crate::runtime) fn collect_global_matches(
    compiled: &crate::regex::Compiled,
    input: &[char],
) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut cursor = 0usize;
    while cursor <= input.len() {
        let Some(found) = compiled.find(input, cursor) else {
            break;
        };
        spans.push((found.start, found.end));
        cursor = if found.end == found.start {
            found.end + 1
        } else {
            found.end
        };
    }
    spans
}

/// Split `input` around each match of `compiled`, returning piece spans.
pub(in crate::runtime) fn split_by_regex(
    compiled: &crate::regex::Compiled,
    input: &[char],
    limit: usize,
) -> Vec<(usize, usize)> {
    let mut pieces = Vec::new();
    let mut cursor = 0usize;
    while pieces.len() < limit && cursor <= input.len() {
        match compiled.find(input, cursor) {
            Some(found) => {
                pieces.push((cursor, found.start));
                cursor = if found.end == found.start {
                    found.end + 1
                } else {
                    found.end
                };
                if pieces.len() >= limit {
                    break;
                }
            }
            None => break,
        }
    }
    if pieces.len() < limit {
        pieces.push((cursor.min(input.len()), input.len()));
    }
    pieces
}

/// Expand `$&`, `` $` ``, `$'`, `$$`, and `$1`–`$9` in a replacement string.
pub(in crate::runtime) fn expand_replacement(
    replacement: &str,
    input: &[char],
    found: &crate::regex::MatchRanges,
) -> String {
    let characters: Vec<char> = replacement.chars().collect();
    let mut output = String::new();
    let mut index = 0usize;
    while index < characters.len() {
        let character = characters[index];
        if character != '$' || index + 1 >= characters.len() {
            output.push(character);
            index += 1;
            continue;
        }
        let next = characters[index + 1];
        match next {
            '$' => {
                output.push('$');
                index += 2;
            }
            '&' => {
                output.extend(&input[found.start..found.end]);
                index += 2;
            }
            '`' => {
                output.extend(&input[..found.start]);
                index += 2;
            }
            '\'' => {
                output.extend(&input[found.end.min(input.len())..]);
                index += 2;
            }
            digit @ '1'..='9' => {
                // `$nn` names group `nn` when the pattern has that many groups;
                // otherwise it is `$n` followed by a literal digit.
                let mut group = digit as usize - '1' as usize;
                index += 2;
                if let Some(second) = characters.get(index).and_then(|c| c.to_digit(10)) {
                    let two_digit = (group + 1) * 10 + second as usize;
                    if (1..=found.groups.len()).contains(&two_digit) {
                        group = two_digit - 1;
                        index += 1;
                    }
                }
                if let Some(Some((start, end))) = found.groups.get(group) {
                    output.extend(&input[*start..*end]);
                }
            }
            '<' if !found.names.is_empty() => {
                if let Some(close) = characters[index + 2..].iter().position(|c| *c == '>') {
                    let name: String = characters[index + 2..index + 2 + close].iter().collect();
                    if let Some((_, group)) =
                        found.names.iter().find(|(candidate, _)| *candidate == name)
                        && let Some(Some((start, end))) = found.groups.get(group - 1)
                    {
                        output.extend(&input[*start..*end]);
                    }
                    index += close + 3;
                } else {
                    output.push('$');
                    output.push('<');
                    index += 2;
                }
            }
            other => {
                output.push('$');
                output.push(other);
                index += 2;
            }
        }
    }
    output
}

/// `String.prototype.split` with a **non-empty** literal separator, over code
/// units.
///
/// The search is a substring search, so it is a code-unit search: splitting
/// `'\u{1F600}a'` on `'\uDE00'` yields `['\u{1F600}', 'a']` because unit 1 *is*
/// `0xDE00`. `str::split` on the engine's own `String` cannot express that,
/// because the trailing half of a pair is not a separate value there - the
/// engine represents the whole pair as one scalar.
pub(in crate::runtime) fn split_by_units(text: &str, separator: &str, limit: usize) -> Vec<String> {
    let units = utf16::utf16_units(text);
    let needle = utf16::utf16_units(separator);
    let mut pieces = Vec::new();
    let mut cursor = 0usize;
    while pieces.len() < limit {
        let found = (cursor..=units.len().saturating_sub(needle.len()))
            .find(|start| units[*start..].starts_with(&needle));
        let Some(found) = found else {
            pieces.push(utf16::string_from_utf16(&units[cursor..]));
            break;
        };
        pieces.push(utf16::string_from_utf16(&units[cursor..found]));
        cursor = found + needle.len();
    }
    pieces
}

/// Expand `$&`, `` $` ``, `$'`, `$$`, and `$1`–`$9` in a replacement string
/// against a **code-unit** input.
///
/// `GetSubstitution` (§22.1.3.19.1) slices the search string, so the spans it
/// indexes are code-unit offsets. `$&` is the matched substring, and the
/// backtick and apostrophe forms are the text before and after it - all of
/// which a surrogate pair can split, so replacing the trailing half of a pair
/// with a backtick expansion yields the lone high surrogate.
pub(in crate::runtime) fn expand_units_replacement(
    replacement: &str,
    input: &[u16],
    found: &crate::regex::MatchRanges,
) -> String {
    let mut output = String::new();
    let units: Vec<u16> = replacement.encode_utf16().collect();
    let mut index = 0usize;
    while index < units.len() {
        let unit = units[index];
        if unit != u16::from(b'$') || index + 1 >= units.len() {
            output.push_str(&utf16::string_from_unit(unit));
            index += 1;
            continue;
        }
        let next = units[index + 1];
        match next {
            unit if unit == u16::from(b'$') => {
                output.push('$');
                index += 2;
            }
            unit if unit == u16::from(b'&') => {
                output.push_str(&utf16::string_from_utf16(&input[found.start..found.end]));
                index += 2;
            }
            unit if unit == u16::from(b'`') => {
                output.push_str(&utf16::string_from_utf16(&input[..found.start]));
                index += 2;
            }
            unit if unit == u16::from(b'\'') => {
                output.push_str(&utf16::string_from_utf16(
                    &input[found.end.min(input.len())..],
                ));
                index += 2;
            }
            digit if (u16::from(b'1')..=u16::from(b'9')).contains(&digit) => {
                let mut group = usize::from(digit - u16::from(b'1'));
                index += 2;
                if let Some(second) = units
                    .get(index)
                    .filter(|unit| (u16::from(b'0')..=u16::from(b'9')).contains(unit))
                {
                    let two_digit = (group + 1) * 10 + usize::from(second - u16::from(b'0'));
                    if (1..=found.groups.len()).contains(&two_digit) {
                        group = two_digit - 1;
                        index += 1;
                    }
                }
                if let Some(Some((start, end))) = found.groups.get(group) {
                    output.push_str(&utf16::string_from_utf16(&input[*start..*end]));
                }
            }
            unit if unit == u16::from(b'<') && !found.names.is_empty() => {
                let rest = &units[index + 2..];
                if let Some(close) = rest.iter().position(|unit| *unit == u16::from(b'>')) {
                    let name = utf16::string_from_utf16(&rest[..close]);
                    if let Some((_, group)) =
                        found.names.iter().find(|(candidate, _)| *candidate == name)
                        && let Some(Some((start, end))) = found.groups.get(group - 1)
                    {
                        output.push_str(&utf16::string_from_utf16(&input[*start..*end]));
                    }
                    index += close + 3;
                } else {
                    output.push_str("$<");
                    index += 2;
                }
            }
            other => {
                output.push('$');
                output.push_str(&utf16::string_from_unit(other));
                index += 2;
            }
        }
    }
    output
}

/// ECMA-262 22.1.1's `WhiteSpace` and `LineTerminator` productions, which is
/// the set `trim`/`trimStart`/`trimEnd` remove and the set
/// `String.prototype.split` splits on.
///
/// `str::trim` is not this set: it also removes U+0085 (NEL), which
/// JavaScript does not treat as whitespace. Keeping the predicate explicit is
/// the difference between `" \u{85}x ".trimStart()` answering `"x"` and
/// answering `"\u{85}x "`.
pub(in crate::runtime) fn is_js_whitespace(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'
            | '\u{000A}'
            | '\u{000B}'
            | '\u{000C}'
            | '\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

/// Map a `String.prototype` method name to its native implementation. This is
/// the primitive-receiver fast path: an installed prototype method is found by
/// ordinary lookup, and this is the same answer for a receiver the engine holds
/// as a `StringPrimitive` host.
pub(in crate::runtime) fn string_method_native(name: &str) -> Option<NativeFunction> {
    match name {
        "charAt" => Some(NativeFunction::StrCharAt),
        "charCodeAt" => Some(NativeFunction::StrCharCodeAt),
        "codePointAt" => Some(NativeFunction::StrCodePointAt),
        "at" => Some(NativeFunction::StrAt),
        "indexOf" => Some(NativeFunction::StrIndexOf),
        "lastIndexOf" => Some(NativeFunction::StrLastIndexOf),
        "includes" => Some(NativeFunction::StrIncludes),
        "startsWith" => Some(NativeFunction::StrStartsWith),
        "endsWith" => Some(NativeFunction::StrEndsWith),
        "slice" => Some(NativeFunction::StrSlice),
        "substring" => Some(NativeFunction::StrSubstring),
        "padStart" => Some(NativeFunction::StrPadStart),
        "padEnd" => Some(NativeFunction::StrPadEnd),
        "trim" => Some(NativeFunction::StrTrim),
        "trimStart" => Some(NativeFunction::StrTrimStart),
        "trimEnd" => Some(NativeFunction::StrTrimEnd),
        "repeat" => Some(NativeFunction::StrRepeat),
        "localeCompare" => Some(NativeFunction::StrLocaleCompare),
        "split" => Some(NativeFunction::StrSplit),
        "replace" => Some(NativeFunction::StrReplace),
        "replaceAll" => Some(NativeFunction::StrReplaceAll),
        "match" => Some(NativeFunction::StrMatch),
        "search" => Some(NativeFunction::StrSearch),
        "concat" => Some(NativeFunction::StrConcat),
        "toString" | "valueOf" => Some(NativeFunction::StrToString),
        _ => None,
    }
}

/// Whether the native is a `String.prototype` method whose receiver may be a
/// primitive string that needs a transient wrapper.
#[allow(dead_code, reason = "retained for potential future use")]
pub(in crate::runtime) fn is_string_native(function: NativeFunction) -> bool {
    matches!(
        function,
        NativeFunction::StrCharAt
            | NativeFunction::StrCharCodeAt
            | NativeFunction::StrIndexOf
            | NativeFunction::StrLastIndexOf
            | NativeFunction::StrIncludes
            | NativeFunction::StrStartsWith
            | NativeFunction::StrEndsWith
            | NativeFunction::StrSlice
            | NativeFunction::StrSubstring
            | NativeFunction::StrToLowerCase
            | NativeFunction::StrToUpperCase
            | NativeFunction::StrTrim
            | NativeFunction::StrSplit
            | NativeFunction::StrReplace
            | NativeFunction::StrMatch
            | NativeFunction::StrSearch
            | NativeFunction::StrConcat
            | NativeFunction::StrToString
    )
}

impl JsRuntime {
    /// Create a transient wrapper object exposing string prototype members.
    pub(in crate::runtime) fn string_wrapper(&mut self, value: String) -> ObjectId {
        self.realm.string_wrapper(value)
    }

    /// Resolve the string a `%String.prototype%` method operates on.
    ///
    /// Per spec, general string methods coerce any non-null receiver through
    /// the `ToString` abstract operation instead of requiring an actual string
    /// wrapper. Real-world bundles routinely call `String.prototype.indexOf`,
    /// `slice`, `match` and friends with `.call(anyObject)` as a coercion and
    /// feature probe, so plain objects must not throw here. The couple of
    /// brand-checking methods (`toString`, `valueOf`) use
    /// [`Self::require_string_object`] instead.
    #[allow(
        clippy::unnecessary_wraps,
        reason = "callers share the fallible native-string method path"
    )]
    pub(in crate::runtime) fn require_string_receiver(
        &self,
        receiver: ObjectId,
    ) -> Result<String, JsError> {
        match self.realm.host(receiver) {
            Some(ObjectHost::StringPrimitive(text)) => Ok(text.clone()),
            Some(ObjectHost::NumberPrimitive(number)) => Ok(number_to_string(number)),
            Some(ObjectHost::BooleanPrimitive(value)) => {
                Ok(if value { "true" } else { "false" }.to_owned())
            }
            Some(ObjectHost::Array) => {
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "array length is a validated finite non-negative integer"
                )]
                let length = self
                    .realm
                    .get_property(receiver, "length")
                    .and_then(|value| match &value {
                        JsValue::Number(number)
                            if number.is_finite() && number.is_sign_positive() =>
                        {
                            Some(*number as usize)
                        }
                        _ => None,
                    })
                    .unwrap_or(0);
                let joined = (0..length)
                    .map(|index| self.realm.get_property(receiver, &index.to_string()))
                    .map(|value| value.unwrap_or(JsValue::Undefined).to_js_string())
                    .collect::<Vec<_>>()
                    .join(",");
                Ok(joined)
            }
            // `String.prototype.toString`/`valueOf` brand-check below, so every
            // other host (ordinary objects, DOM nodes, collections, helpers)
            // falls back to the ordinary object string form this runtime
            // already produces for concatenation and logging.
            _ => Ok("[object Object]".to_owned()),
        }
    }

    /// Brand-check the receiver required by `String.prototype.toString` and
    /// `String.prototype.valueOf`, which throw on non-string objects.
    pub(in crate::runtime) fn require_string_object(
        &self,
        receiver: ObjectId,
    ) -> Result<String, JsError> {
        match self.realm.host(receiver) {
            Some(ObjectHost::StringPrimitive(text)) => Ok(text.clone()),
            other => Err(JsError::type_error(format!(
                "incompatible String method receiver (host {other:?})"
            ))),
        }
    }

    /// ECMA-262 22.1.2.4 `String.raw(template, ...substitutions)`.
    ///
    /// The literals are the template object's `raw` property, not the template
    /// itself, so a template that omits `raw` is a `TypeError` rather than an
    /// empty answer. A substitution is spliced only *between* two literals, so
    /// a call that supplies fewer substitutions than literals appends nothing
    /// instead of stringifying a hole.
    pub(in crate::runtime) fn string_raw(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let Some(template) = arguments.first() else {
            return Err(JsError::type_error("String.raw requires a template object"));
        };
        // Step 2/3: `ToObject(template)` then `ToObject(Get(template, "raw"))`.
        // Both throw for `null`/`undefined` and for a missing `raw`, which is
        // what an object that only looks array-like answers.
        let cooked = self.to_object(template)?;
        let raw = self
            .realm
            .get_property(cooked, "raw")
            .ok_or_else(|| JsError::type_error("String.raw template has no 'raw' property"))?;
        let literals = self.to_object(&raw)?;
        // Step 4: `LengthOfArrayLike`.
        let length = match self.realm.get_property(literals, "length") {
            Some(value) => self.to_length_value(dom, &value)?,
            None => 0.0,
        };
        if length > MAX_MATERIALIZED_ELEMENTS as f64 {
            return Err(
                self.range_error("String.raw template literal count exceeds the engine limit")
            );
        }
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "to_length is a non-negative integer already bounded by the materialization cap"
        )]
        let count = length as usize;
        // Step 5: an empty `raw` contributes nothing at all, not one `undefined`.
        let mut output = String::new();
        for index in 0..count {
            output.push_str(
                &self
                    .get_member(dom, literals, &index.to_string())?
                    .to_js_string(),
            );
            // Step 8.d: the final literal ends the string; step 8.e splices a
            // substitution only when a later literal still follows.
            if index + 1 == count {
                break;
            }
            if let Some(substitution) = arguments.get(index + 1) {
                output.push_str(&substitution.to_js_string());
            }
        }
        Ok(JsValue::String(output))
    }

    /// ECMA-262 22.1.3.2 `String.prototype.charAt`, over code units.
    pub(in crate::runtime) fn string_char_at(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let text = self.require_string_receiver(receiver)?;
        let position = self.optional_integer_value(dom, arguments.first())?;
        Ok(char_at_value(&utf16::utf16_units(&text), position))
    }

    /// ECMA-262 22.1.3.3 `String.prototype.charCodeAt`: "the numeric value of
    /// the code unit at index position", so a surrogate half reports its own
    /// 16-bit value - `'\u{1F600}'.charCodeAt(0)` is `0xD83D` and
    /// `charCodeAt(1)` is `0xDE00`. It is the method whose index is a code-unit
    /// offset and whose answer is a code unit, so it is the direct evidence
    /// that this engine addresses strings in code units.
    pub(in crate::runtime) fn string_char_code_at(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let text = self.require_string_receiver(receiver)?;
        let position = self.optional_integer_value(dom, arguments.first())?;
        let units = utf16::utf16_units(&text);
        let Some(position) = valid_position(position, units.len()) else {
            return Ok(JsValue::Number(f64::NAN));
        };
        Ok(JsValue::Number(f64::from(units[position])))
    }

    pub(in crate::runtime) fn string_from_char_code(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let mut units = Vec::with_capacity(arguments.len());
        for value in arguments {
            let number = self.to_number_value(dom, value)?;
            let integer = if number.is_finite() {
                number.trunc()
            } else {
                0.0
            };
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "ECMAScript fromCharCode wraps every numeric argument modulo 2^16"
            )]
            let unit = integer.rem_euclid(65_536.0) as u16;
            units.push(unit);
        }
        // This is the *same* code-unit-to-engine-string conversion every
        // substring result goes through, which is what makes
        // `String.fromCharCode(0xD83D, 0xDE00).length === 2` agree with
        // `'\u{1F600}'.length`: both are the length of the code-unit sequence.
        Ok(JsValue::String(utf16::string_from_utf16(&units)))
    }

    pub(in crate::runtime) fn string_from_code_point(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let mut text = String::with_capacity(arguments.len());
        for value in arguments {
            let number = self.to_number_value(dom, value)?;
            if !number.is_finite()
                || number.fract() != 0.0
                || !(0.0..=1_114_111.0).contains(&number)
            {
                return Err(self.range_error("invalid code point"));
            }
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "the preceding range and integer checks guarantee a Unicode scalar input"
            )]
            let code = number as u32;
            let Some(character) = char::from_u32(code) else {
                return Err(self.range_error("invalid code point"));
            };
            text.push(character);
        }
        Ok(JsValue::String(text))
    }

    /// ECMA-262 22.1.3.9 `indexOf` and 22.1.3.11 `lastIndexOf`, which are
    /// §6.1.4.1 `StringIndexOf` and §6.1.4.2 `StringLastIndexOf` over code
    /// units.
    ///
    /// Comparing code units rather than scalars is what makes a lone-surrogate
    /// needle findable: `'\u{1F600}'.indexOf('\uDE00')` is `1`, because unit 1
    /// *is* `0xDE00` even though it is the trailing half of a pair. An
    /// implementation that searches by code point cannot answer that at all.
    pub(in crate::runtime) fn string_index_of(
        &self,
        receiver: ObjectId,
        arguments: &[JsValue],
        from_end: bool,
    ) -> Result<JsValue, JsError> {
        let text = self.require_string_receiver(receiver)?;
        let needle = required_argument(arguments, 0, "indexOf")?.to_js_string();
        let units = utf16::utf16_units(&text);
        let needle = utf16::utf16_units(&needle);
        // §6.1.4.1 step 3: the empty String is found at `fromIndex`, and
        // `StringLastIndexOf` step 3 asserts the same for the end position, so
        // `indexOf('')` is 0 and `lastIndexOf('')` is the length.
        if needle.is_empty() {
            #[allow(
                clippy::cast_precision_loss,
                reason = "string lengths stay far below any precision boundary"
            )]
            let position = if from_end { units.len() as f64 } else { 0.0 };
            return Ok(JsValue::Number(position));
        }
        let found = if from_end {
            (0..=units.len().saturating_sub(needle.len()))
                .rev()
                .find(|start| units[*start..].starts_with(&needle))
        } else {
            (0..=units.len().saturating_sub(needle.len()))
                .find(|start| units[*start..].starts_with(&needle))
        };
        Ok(match found {
            #[allow(
                clippy::cast_precision_loss,
                reason = "string lengths stay far below any precision boundary"
            )]
            Some(position) => JsValue::Number(position as f64),
            None => JsValue::Number(-1.0),
        })
    }

    /// ECMA-262 22.1.3.8 `String.prototype.includes`, which is `StringIndexOf`
    /// over code units for its truthiness. The engine previously answered this
    /// with `str::contains`, which cannot find a lone surrogate that is the
    /// trailing half of a pair.
    pub(in crate::runtime) fn string_includes(
        &self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let text = self.require_string_receiver(receiver)?;
        let needle = required_argument(arguments, 0, "includes")?.to_js_string();
        let units = utf16::utf16_units(&text);
        let needle = utf16::utf16_units(&needle);
        Ok(JsValue::Boolean(
            (0..=units.len().saturating_sub(needle.len()))
                .any(|start| units[start..].starts_with(&needle)),
        ))
    }

    /// ECMA-262 22.1.3.24 `startsWith` and 22.1.3.7 `endsWith`, which compare
    /// the code-unit substring at the clamped end of the string against the
    /// search value. Comparing Rust `&str` prefixes/suffixes would be wrong for
    /// a lone-surrogate needle against a pair, for the same reason `includes`
    /// needed the code-unit view.
    pub(in crate::runtime) fn string_starts_or_ends_with(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
        starts: bool,
    ) -> Result<JsValue, JsError> {
        let text = self.require_string_receiver(receiver)?;
        // `ToString(searchString)`, so an absent argument searches for "undefined".
        let needle = self.to_string_value(dom, arguments.first().unwrap_or(&JsValue::Undefined))?;
        let units = utf16::utf16_units(&text);
        let needle = utf16::utf16_units(&needle);
        let length = units.len();
        // The position is converted even when the search string is empty, and
        // an absent position is 0 for `startsWith` and the length for `endsWith`.
        let position = match arguments.get(1) {
            None | Some(JsValue::Undefined) => {
                if starts {
                    0.0
                } else {
                    length as f64
                }
            }
            Some(value) => self.to_integer_value(dom, value)?,
        };
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss,
            reason = "the position is clamped to the code-unit length first"
        )]
        let clamped = position.max(0.0).min(length as f64) as usize;
        let matched = if starts {
            clamped + needle.len() <= length && units[clamped..clamped + needle.len()] == needle
        } else {
            clamped >= needle.len() && units[clamped - needle.len()..clamped] == needle
        };
        Ok(JsValue::Boolean(matched))
    }

    /// ECMA-262 22.1.3.22 `String.prototype.slice`, over code units: both
    /// bounds are `ToClampedIndex` and step 6 returns empty when `from ≥ to`.
    pub(in crate::runtime) fn string_slice(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let text = self.require_string_receiver(receiver)?;
        let units = utf16::utf16_units(&text);
        let start = self.optional_integer_value(dom, arguments.first())?;
        let end = match arguments.get(1) {
            None | Some(JsValue::Undefined) => None,
            Some(value) => Some(self.to_integer_value(dom, value)?),
        };
        let range = slice_range(units.len(), start, end, true);
        Ok(JsValue::String(utf16::string_from_utf16(&units[range])))
    }

    /// ECMA-262 22.1.3.25 `String.prototype.substring`, over code units: both
    /// bounds are clamped into `[0, length]` and step 6 takes
    /// `from = min(finalStart, finalEnd)`, so reversed bounds are swapped
    /// rather than yielding the empty String.
    pub(in crate::runtime) fn string_substring(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let text = self.require_string_receiver(receiver)?;
        let units = utf16::utf16_units(&text);
        let start = self.optional_integer_value(dom, arguments.first())?;
        let end = match arguments.get(1) {
            None | Some(JsValue::Undefined) => None,
            Some(value) => Some(self.to_integer_value(dom, value)?),
        };
        let range = slice_range(units.len(), start, end, false);
        Ok(JsValue::String(utf16::string_from_utf16(&units[range])))
    }

    pub(in crate::runtime) fn string_to_case(
        &self,
        receiver: ObjectId,
        _arguments: &[JsValue],
        upper: bool,
    ) -> Result<JsValue, JsError> {
        let text = self.require_string_receiver(receiver)?;
        Ok(JsValue::String(if upper {
            text.to_uppercase()
        } else {
            text.to_lowercase()
        }))
    }

    pub(in crate::runtime) fn string_trim(&self, receiver: ObjectId) -> Result<JsValue, JsError> {
        let text = self.require_string_receiver(receiver)?;
        Ok(JsValue::String(text.trim().to_owned()))
    }

    /// ECMA-262 22.1.3.17 `String.prototype.trimStart` and 22.1.3.18
    /// `trimEnd`, which are `trim` restricted to one end.
    pub(in crate::runtime) fn string_trim_end(
        &self,
        receiver: ObjectId,
        start: bool,
    ) -> Result<JsValue, JsError> {
        let text = self.require_string_receiver(receiver)?;
        Ok(JsValue::String(if start {
            text.trim_start_matches(is_js_whitespace).to_owned()
        } else {
            text.trim_end_matches(is_js_whitespace).to_owned()
        }))
    }

    /// `replaceAll` with a **string** search value: every non-overlapping
    /// occurrence, with the same substitution rules `replace` uses, all over
    /// the code-unit sequence. The search and the reported offset are both in
    /// code units, so `'\u{1F600}a'.replaceAll('\uDE00', fn)` matches the
    /// trailing half of the pair at offset 1.
    ///
    /// The empty search value is the interesting case. ECMA-262's search loop
    /// advances by one code unit after a zero-length match, so
    /// `"ab".replaceAll("", "-")` inserts a separator at every boundary
    /// *including* both ends: `"-a-b-"`. The end insertions are what a naive
    /// `split`/`join` gets wrong, and they are visible in the answer.
    pub(in crate::runtime) fn string_replace_all_literal(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        needle: &str,
        replacement: &JsValue,
    ) -> Result<JsValue, JsError> {
        let text = self.require_string_receiver(receiver)?;
        let units = utf16::utf16_units(&text);
        let pattern = utf16::utf16_units(needle);
        // The output is accumulated as code units rather than as text, because a
        // substitution can put a lone surrogate next to one the copy left
        // behind, and only re-decoding at the end knows whether the two halves
        // form a pair.
        let mut output: Vec<u16> = Vec::with_capacity(units.len());
        let replacement_template = replacement.to_js_string();
        let mut cursor = 0usize;
        while cursor <= units.len() {
            let found = MatchRanges {
                start: cursor,
                end: cursor + pattern.len(),
                groups: Vec::new(),
                names: std::sync::Arc::from(Vec::new()),
            };
            let matched = !pattern.is_empty() && units[cursor..].starts_with(pattern.as_slice());
            if matched {
                if let JsValue::Object(callable) = replacement
                    && Self::is_callable_object(*callable, &self.realm)
                {
                    let matched_text =
                        utf16::string_from_utf16(&units[cursor..cursor + pattern.len()]);
                    #[allow(
                        clippy::cast_precision_loss,
                        reason = "string offsets stay far below any precision boundary"
                    )]
                    let produced = self.call(
                        dom,
                        *callable,
                        &[
                            JsValue::String(matched_text),
                            JsValue::Number(cursor as f64),
                            JsValue::String(text.clone()),
                        ],
                    )?;
                    output.extend(utf16::utf16_units(&produced.to_js_string()));
                } else {
                    output.extend(utf16::utf16_units(&expand_units_replacement(
                        &replacement_template,
                        &units,
                        &found,
                    )));
                }
                cursor += pattern.len().max(1);
                continue;
            }
            if pattern.is_empty() {
                // Zero-length match at `cursor`: the substitution goes *before*
                // the code unit, and one more goes after the last one. The
                // order is the whole of the spec's answer here -
                // `"ab".replaceAll("", "-")` is `"-a-b-"`, and an implementation
                // that emits the unit first gets `"a-b--"` or `"-ab"`, which are
                // the two wrong answers a `split`/`join` produces.
                output.extend(utf16::utf16_units(&expand_units_replacement(
                    &replacement_template,
                    &units,
                    &found,
                )));
                if cursor < units.len() {
                    output.push(units[cursor]);
                }
                cursor += 1;
                continue;
            }
            if cursor < units.len() {
                output.push(units[cursor]);
            }
            cursor += 1;
        }
        Ok(JsValue::String(utf16::string_from_utf16(&output)))
    }

    /// §22.1.3.3 `String.prototype.concat`, which is a code-unit
    /// concatenation like `+`: two lone surrogates either side of a join become
    /// one pair, because the result is read back as a code-unit sequence.
    pub(in crate::runtime) fn string_concat(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let mut units = utf16::utf16_units(&self.require_string_receiver(receiver)?);
        for argument in arguments {
            units.extend(utf16::utf16_units(&argument.to_js_string()));
        }
        Ok(JsValue::String(utf16::string_from_utf16(&units)))
    }

    /// Split by a literal separator or a regular expression.
    pub(in crate::runtime) fn string_split(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let text = self.require_string_receiver(receiver)?;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "split limits are clamped to the u32 range first"
        )]
        let limit = match arguments.get(1) {
            None | Some(JsValue::Undefined) => usize::MAX,
            Some(value) => {
                let number = self.to_number_value(dom, value)?;
                uint32_of_number(number) as usize
            }
        };
        let pieces = match arguments.first() {
            None | Some(JsValue::Undefined) => vec![text],
            Some(separator) => {
                let characters: Vec<char> = text.chars().collect();
                if let JsValue::Object(_) = separator {
                    let (index, _) = self.coerce_pattern_argument(separator)?;
                    split_by_regex(&self.regexes[index].compiled, &characters, limit)
                        .into_iter()
                        .map(|span| characters[span.0..span.1].iter().collect())
                        .collect()
                } else {
                    let separator = separator.to_js_string();
                    if separator.is_empty() {
                        // §22.1.3.32: an empty separator "returns the List
                        // containing the String values for each code unit", so
                        // `'\u{1F600}'.split('')` has two elements. This is the
                        // same code-unit notion `length` reports, and answering
                        // with code points here would make `split('')`, `length`
                        // and `charAt` disagree about the same string.
                        utf16::utf16_units(&text)
                            .iter()
                            .take(limit)
                            .map(|unit| utf16::string_from_unit(*unit))
                            .collect()
                    } else if limit == 0 {
                        Vec::new()
                    } else {
                        // A non-empty literal separator is matched as a
                        // substring, so it is found in the code-unit sequence
                        // too: `'\u{1F600}a'.split('\uDE00')` splits inside the
                        // pair, which a `&str` search could not do.
                        split_by_units(&text, &separator, limit)
                    }
                }
            }
        };
        let values = pieces.into_iter().map(JsValue::String).collect::<Vec<_>>();
        Ok(JsValue::Object(self.create_array_from_values(&values)?))
    }

    /// `String.prototype.match`: one exec-style result unless the regex is
    /// global, in which case every full match is collected.
    pub(in crate::runtime) fn string_match(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let text = self.require_string_receiver(receiver)?;
        let characters: Vec<char> = text.chars().collect();
        let Some(argument) = arguments.first() else {
            let object = self.construct_regex("", "")?;
            let Some(ObjectHost::RegExp(index)) = self.realm.host(object) else {
                return Err(JsError::type_error("regexp construction failed"));
            };
            return match self.regex_exec_value(index, &characters, &text, 0)? {
                Some(value) => Ok(value),
                None => Ok(JsValue::Null),
            };
        };
        let (index, global) = self.coerce_pattern_argument(argument)?;
        if !global {
            return match self.regex_exec_value(index, &characters, &text, 0)? {
                Some(value) => Ok(value),
                None => Ok(JsValue::Null),
            };
        }
        let spans = collect_global_matches(&self.regexes[index].compiled, &characters);
        let values = spans
            .into_iter()
            .map(|(start, end)| JsValue::String(characters[start..end].iter().collect()))
            .collect::<Vec<_>>();
        if values.is_empty() {
            return Ok(JsValue::Null);
        }
        Ok(JsValue::Object(self.create_array_from_values(&values)?))
    }

    /// ECMA-262 22.1.3.21 `String.prototype.search`.
    ///
    /// The matcher walks code points but the answer is a String index, so the
    /// offset is converted: `'\u{1F600}b'.search(/b/)` is 2, the position a
    /// caller would pass to `slice`.
    pub(in crate::runtime) fn string_search(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let text = self.require_string_receiver(receiver)?;
        let characters: Vec<char> = text.chars().collect();
        let argument = required_argument(arguments, 0, "search")?;
        let (index, _) = self.coerce_pattern_argument(argument)?;
        Ok(match self.regexes[index].compiled.find(&characters, 0) {
            Some(found) => {
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "string lengths stay far below any precision boundary"
                )]
                let start = utf16::utf16_offset_of_char(&text, found.start) as f64;
                JsValue::Number(start)
            }
            None => JsValue::Number(-1.0),
        })
    }

    /// `String.prototype[Symbol.iterator]` (and its `values` alias): a String
    /// Iterator over the receiver's **code points**, sharing
    /// `%IteratorPrototype%` with every other engine iterator so
    /// `getProto(getProto(it))` walks the same chain in polyfills that snapshot
    /// the intrinsic.
    ///
    /// This is the half of the model that walks rather than addresses:
    /// `for...of`, spread and destructuring see one element for one code point,
    /// so `'😀'` iterates once with a value of `length === 2`. Only the
    /// *values* are code points - `length` and every index remain code units.
    pub(in crate::runtime) fn string_iterator(
        &mut self,
        receiver: ObjectId,
    ) -> Result<JsValue, JsError> {
        let text = self.require_string_receiver(receiver)?;
        self.ensure_heap_capacity(1)?;
        let values = text
            .chars()
            .map(|character| JsValue::String(character.to_string()))
            .collect::<Vec<_>>();
        Ok(JsValue::Object(self.realm.collection_iterator(values)))
    }

    /// `String.prototype.replace` with `$&`, `$1`–`$9`, `` $` ``, `$'`, `$$`
    /// expansion or a replacement function.
    pub(in crate::runtime) fn string_replace(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let text = self.require_string_receiver(receiver)?;
        let search = required_argument(arguments, 0, "replace")?;
        let replacement = required_argument(arguments, 1, "replace")?.clone();
        let characters: Vec<char> = text.chars().collect();
        let (index, global) = self.coerce_pattern_argument(search)?;
        let compiled = self.regexes[index].compiled.clone();

        let mut cursor = 0usize;
        let mut last_end = 0usize;
        // Accumulated as code units and decoded once at the end, so a
        // replacement that supplies one half of a pair next to a copy of the
        // other half is re-joined. The pattern is matched over code points (a
        // pattern is written in those terms) but the reported `position` and the
        // assembled result are String values, so both are in code units.
        let mut output: Vec<u16> = Vec::new();
        while let Some(found) = compiled.find(&characters, cursor) {
            let replaced: Vec<u16> = match &replacement {
                JsValue::Object(callable) if Self::is_callable_object(*callable, &self.realm) => {
                    let mut call_arguments = vec![JsValue::String(
                        characters[found.start..found.end].iter().collect(),
                    )];
                    for group in &found.groups {
                        call_arguments.push(match group {
                            Some((start, end)) => {
                                JsValue::String(characters[*start..*end].iter().collect())
                            }
                            None => JsValue::Undefined,
                        });
                    }
                    #[allow(
                        clippy::cast_precision_loss,
                        reason = "string lengths stay far below any precision boundary"
                    )]
                    let position = utf16::utf16_offset_of_char(&text, found.start) as f64;
                    call_arguments.push(JsValue::Number(position));
                    call_arguments.push(JsValue::String(text.clone()));
                    if !found.names.is_empty() {
                        let groups = self.named_groups_object(&found, &characters)?;
                        call_arguments.push(groups);
                    }
                    let produced = self.call(dom, *callable, &call_arguments)?;
                    utf16::utf16_units(&produced.to_js_string())
                }
                other => utf16::utf16_units(&expand_replacement(
                    &other.to_js_string(),
                    &characters,
                    &found,
                )),
            };
            for character in &characters[last_end..found.start] {
                output.extend_from_slice(character.encode_utf16(&mut [0u16; 2]));
            }
            output.extend(replaced);
            last_end = found.end;
            if found.end == found.start {
                // Empty match: step past the position to guarantee progress.
                if found.end >= characters.len() {
                    break;
                }
                cursor = found.end + 1;
            } else {
                cursor = found.end;
            }
            if !global {
                break;
            }
        }
        for character in &characters[last_end.min(characters.len())..] {
            output.extend_from_slice(character.encode_utf16(&mut [0u16; 2]));
        }
        Ok(JsValue::String(utf16::string_from_utf16(&output)))
    }
}

#[cfg(test)]
mod tests {
    use crate::runtime::JsRuntime;
    use render_html::parse_document;

    fn run(source: &str) -> String {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        match runtime.execute(&mut parsed.dom, source) {
            Ok(outcome) => outcome.value.to_js_string(),
            Err(error) => format!("<threw {}>", error.message()),
        }
    }

    /// The answer for a call that is *expected* to throw, read the way a caller
    /// reads it. `JsError::message` stringifies the thrown value, and a thrown
    /// `RangeError` stringifies as `[object Object]`, so the engine's own error
    /// report is not the observable a script sees - `e.name` and `e.message`
    /// are.
    fn caught(expression: &str) -> String {
        run(&format!(
            "var out = 'no throw'; try {{ {expression} }} \
             catch (e) {{ out = e.name + ': ' + e.message; }} out"
        ))
    }

    /// The code-unit sequence of a string-valued expression, as lowercase hex
    /// separated by spaces.
    ///
    /// A lone surrogate is invisible in a Rust `String` - the engine holds it as
    /// a private-use placeholder - so comparing a returned string against a Rust
    /// literal would silently pass or fail on the wrong thing. Reading the units
    /// back through the engine's own `charCodeAt` is what makes a
    /// half-a-pair result observable at all, and it is also the only way to
    /// state the expectation without depending on the internal placeholder.
    fn units_of(expression: &str) -> String {
        run(&format!(
            "var s = {expression}; var out = []; \
             for (var i = 0; i < s.length; i++) {{ \
               out.push(('0000' + s.charCodeAt(i).toString(16)).slice(-4)); \
             }} out.join(' ')"
        ))
    }

    /// The same, for an array of strings: one line of code units per element.
    fn units_of_each(expression: &str) -> String {
        run(&format!(
            "var list = {expression}; var out = []; \
             for (var k = 0; k < list.length; k++) {{ \
               var s = list[k]; var parts = []; \
               for (var i = 0; i < s.length; i++) {{ \
                 parts.push(('0000' + s.charCodeAt(i).toString(16)).slice(-4)); \
               }} out.push(parts.join(' ')); \
             }} out.join('|')"
        ))
    }

    /// `String.prototype.length` of a string-valued expression.
    fn length_of(expression: &str) -> String {
        run(&format!("String(({expression}).length)"))
    }

    /// `codePointAt` is the one method where both notions are visible at once:
    /// the *position* is a UTF-16 code-unit offset and the *answer* is a code
    /// point. So `'\u{1F600}'.codePointAt(0)` is the whole scalar while
    /// `codePointAt(1)` is the trailing surrogate's own value, and
    /// `'\u{1F600}a'.codePointAt(2)` is the `a` that sits two units along.
    ///
    /// The position that does *not* begin a pair is the case worth pinning: §11.1.4
    /// `CodePointAt` says a trailing surrogate, a leading surrogate with no
    /// partner, and a leading surrogate followed by an ordinary character all
    /// report themselves, rather than `undefined` or a wrong pairing.
    #[test]
    fn code_point_at_indexes_code_units_and_answers_code_points() {
        assert_eq!(run("'a'.codePointAt(0)"), "97");
        // The pair: one code point spanning two code-unit positions.
        assert_eq!(run("'\\u{1f600}'.codePointAt(0)"), "128512");
        assert_eq!(run("'\\u{1f600}'.codePointAt(0) === 0x1f600"), "true");
        // Position 1 holds the trailing surrogate, so it answers itself.
        assert_eq!(run("'\\u{1f600}'.codePointAt(1)"), "56832");
        assert_eq!(run("'\\u{1f600}'.codePointAt(1) === 0xDE00"), "true");
        // Position 2 is past both units.
        assert_eq!(run("String('\\u{1f600}'.codePointAt(2))"), "undefined");
        // A leading surrogate followed by an ordinary character is not a pair.
        assert_eq!(run("'a\\u{1f600}'.codePointAt(1)"), "128512");
        assert_eq!(run("'\\u{1f600}a'.codePointAt(2)"), "97");
        // A lone surrogate is its own code point.
        assert_eq!(run("'\\uD83D'.codePointAt(0)"), "55357");
        assert_eq!(run("'\\uD83D'.codePointAt(0) === 0xD83D"), "true");
        assert_eq!(run("'\\uDE00'.codePointAt(0)"), "56832");
        // Out of range in either direction is `undefined`, because step 5 is a
        // plain bounds check rather than a count-from-the-end.
        assert_eq!(
            run("String('a'.codePointAt(1)) + '|' + String('a'.codePointAt(-1))"),
            "undefined|undefined"
        );
        // A fractional position truncates toward zero.
        assert_eq!(run("String('a'.codePointAt(1.5))"), "undefined");
        assert_eq!(run("String('a'.codePointAt())"), "97");
        assert_eq!(run("String('ab'.codePointAt(0.9))"), "97");
        assert_eq!(run("String('\\u{1f600}a'.codePointAt(1.9))"), "56832");
    }

    /// `length` counts UTF-16 code units, so an astral character is two and a
    /// lone surrogate is one. This is the assertion that was declined before the
    /// model was split, because writing it would have pinned a defect.
    #[test]
    fn length_counts_utf16_code_units() {
        assert_eq!(length_of("'abc'"), "3");
        assert_eq!(length_of("''"), "0");
        // The two the engine used to get wrong, in both directions.
        assert_eq!(length_of("'\\u{1f600}'"), "2");
        assert_eq!(length_of("'\\uD83D'"), "1");
        assert_eq!(length_of("'\\uDE00'"), "1");
        assert_eq!(length_of("'a\\u{1f600}b'"), "4");
        // Two lone high surrogates are two units. In source they are two
        // separate escapes that are *not* a `SurrogatePair`, so the lexer keeps
        // them apart rather than replacing the pair with U+FFFD.
        assert_eq!(length_of("'\\uD83D\\uD83D'"), "2");
        assert_eq!(run("'\\uD83D\\uD83D'.charCodeAt(0)"), "55357");
        assert_eq!(run("'\\uD83D\\uD83D'.codePointAt(0)"), "55357");
        // A string built from code units measures the same way.
        assert_eq!(length_of("String.fromCharCode(0xD83D, 0xDE00)"), "2");
        assert_eq!(length_of("String.fromCharCode(0xD83D)"), "1");
        assert_eq!(
            run("String.fromCharCode(0xD83D, 0xDE00) === '\\u{1f600}'"),
            "true"
        );
        // `length` and the code-point walk disagree, and both are correct: the
        // string has three code units and two code points.
        assert_eq!(run("'a\\u{1f600}'.length"), "3");
        assert_eq!(run("[...'a\\u{1f600}'].length"), "2");
        assert_eq!(run("Array.from('a\\u{1f600}').length"), "2");
        assert_eq!(run("[...'a\\u{1f600}'][1].length"), "2");
    }

    /// `at` addresses code units: §22.1.3.1 step 7 is "the substring of string
    /// from k to k + 1", the same one-code-unit answer `charAt` gives, which is
    /// why the two agree.
    #[test]
    fn at_counts_negative_indices_from_the_end_in_code_units() {
        assert_eq!(
            run("'abc'.at(0) + '|' + 'abc'.at(-1) + '|' + String('abc'.at(3))"),
            "a|c|undefined"
        );
        assert_eq!(
            run("[1,2,3].at(0) + ':' + [1,2,3].at(-1) + ':' + String([1,2,3].at(-4))"),
            "1:3:undefined"
        );
        // `'\u{1F600}a'` is three code units: d83d de00 61. So `at(-1)` is the
        // `a` and `at(-2)` is the trailing surrogate, one step apart.
        assert_eq!(length_of("'\\u{1f600}a'"), "3");
        assert_eq!(units_of("'\\u{1f600}a'.at(-1)"), "0061");
        assert_eq!(units_of("'\\u{1f600}a'.at(-2)"), "de00");
        assert_eq!(units_of("'\\u{1f600}a'.at(0)"), "d83d");
        assert_eq!(units_of("'\\u{1f600}a'.at(1)"), "de00");
        assert_eq!(run("String('\\u{1f600}a'.at(3))"), "undefined");
        assert_eq!(run("String('\\u{1f600}a'.at(-4))"), "undefined");
        // A fractional index truncates toward zero, so -2.5 is -2 and 1.9 is 1.
        assert_eq!(units_of("'\\u{1f600}a'.at(-2.5)"), "de00");
        assert_eq!(units_of("'\\u{1f600}a'.at(1.9)"), "de00");
        // On a two-unit string, -1.5 truncates to -1, which is unit 1.
        assert_eq!(units_of("'\\u{1f600}'.at(-1.5)"), "de00");
        // An empty astral string still has two addressable units.
        assert_eq!(units_of("'\\u{1f600}'.at(0)"), "d83d");
        assert_eq!(units_of("'\\u{1f600}'.at(1)"), "de00");
    }

    /// `padStart`/`padEnd` are §22.1.3.17.2 `StringPad`: the *whole* `fillString`
    /// repeats and is then truncated to `maxLength - stringLength` code units.
    #[test]
    fn pad_start_and_pad_end_repeat_the_whole_filler_in_code_units() {
        assert_eq!(run("'5'.padStart(3, '0')"), "005");
        assert_eq!(run("'5'.padEnd(3, '0')"), "500");
        assert_eq!(run("'abc'.padStart(2, '0')"), "abc");
        // Step 4 is "repeated concatenations of fillString truncated to length
        // fillLength", so a two-unit filler alternates and is then cut. A
        // filler repeated by its *first* character gives `"xxxabc"`, which is
        // the answer a `repeat`-the-first-unit implementation produces.
        assert_eq!(run("'abc'.padStart(6, 'xy')"), "xyxabc");
        assert_eq!(run("'abc'.padEnd(6, 'xy')"), "abcxyx");
        assert_eq!(run("'a'.padStart(4, 'ab')"), "abaa");
        assert_eq!(run("'a'.padStart(5, 'ab')"), "ababa");
        assert_eq!(run("'abc'.padStart(7, 'xy')"), "xyxyabc");
        // An astral filler is truncated at a code-unit boundary, so the result
        // is exactly `maxLength` units long. This is the assertion that was
        // declined before the model was split, because writing it would have
        // pinned a defect: under the old code-point model this was length 5.
        assert_eq!(length_of("'a'.padStart(3, '\\u{1f600}')"), "3");
        assert_eq!(units_of("'a'.padStart(3, '\\u{1f600}')"), "d83d de00 0061");
        assert_eq!(length_of("'a'.padEnd(3, '\\u{1f600}')"), "3");
        assert_eq!(units_of("'a'.padEnd(3, '\\u{1f600}')"), "0061 d83d de00");
        // A target that is one short of a whole pair truncates the filler
        // mid-pair, which is the specified answer rather than a defect.
        assert_eq!(length_of("''.padStart(3, '\\u{1f600}')"), "3");
        assert_eq!(units_of("''.padStart(3, '\\u{1f600}')"), "d83d de00 d83d");
        assert_eq!(units_of("''.padStart(1, '\\u{1f600}')"), "d83d");
        // A string that already fills the target is returned unchanged.
        assert_eq!(units_of("'\\u{1f600}'.padStart(2, '0')"), "d83d de00");
        assert_eq!(units_of("'\\u{1f600}'.padStart(3, '0')"), "0030 d83d de00");
        // An empty filler pads by nothing; `undefined` means one space.
        assert_eq!(length_of("'a'.padStart(2, '')"), "1");
        assert_eq!(run("'a'.padStart(2, '')"), "a");
        assert_eq!(run("'a'.padStart(2)"), " a");
        assert_eq!(run("'a'.padStart(0, '0')"), "a");
        // Step 2 is `ToLength(maxLength)`, which truncates toward zero, so
        // `1.9` is a target of 1 and `2.9` is a target of 2.
        assert_eq!(run("'a'.padStart(1.9, '0')"), "a");
        assert_eq!(run("'a'.padStart(2.9, '0')"), "0a");
        assert_eq!(run("'a'.padStart(-1, '0')"), "a");
        assert_eq!(run("String('a'.padStart(NaN, '0'))"), "a");
    }

    /// `padStart`/`padEnd` never *join* two halves that were not already one
    /// pair, and never drop one either: the filler's own units are copied
    /// through unchanged.
    #[test]
    fn padding_copies_the_fillers_units_without_joining_them() {
        // A lone-surrogate filler keeps its single unit per copy, so padding an
        // empty string to two gives two lone surrogates - not one pair.
        assert_eq!(units_of("''.padStart(2, '\\uD83D')"), "d83d d83d");
        assert_eq!(length_of("''.padStart(2, '\\uD83D')"), "2");
        assert_eq!(run("''.padStart(2, '\\uD83D').codePointAt(1)"), "55357");
        // A pair filler copies whole pairs when the target allows it.
        assert_eq!(
            units_of("''.padStart(4, '\\u{1f600}')"),
            "d83d de00 d83d de00"
        );
        assert_eq!(length_of("''.padStart(4, '\\u{1f600}')"), "4");
        // A pair filler mixed with a trailing lone unit keeps both.
        assert_eq!(
            units_of("''.padStart(3, '\\u{1f600}\\uDE00')"),
            "d83d de00 de00"
        );
    }

    /// `repeat` copies the code-unit sequence: it never splits a pair and never
    /// joins two lone halves into one.
    #[test]
    fn repeat_copies_the_code_unit_sequence() {
        assert_eq!(run("'ab'.repeat(3)"), "ababab");
        assert_eq!(run("''.repeat(3) + '|'"), "|");
        assert_eq!(run("'a'.repeat(0) + '|'"), "|");
        assert_eq!(length_of("'\\u{1f600}'.repeat(2)"), "4");
        assert_eq!(units_of("'\\u{1f600}'.repeat(2)"), "d83d de00 d83d de00");
        // Two lone surrogates repeat as two lone surrogates, not as a pair.
        assert_eq!(length_of("'\\uD83D'.repeat(2)"), "2");
        assert_eq!(units_of("'\\uD83D'.repeat(2)"), "d83d d83d");
        assert_eq!(run("'\\uD83D'.repeat(2).codePointAt(0)"), "55357");
        assert_eq!(run("'\\uD83D'.repeat(2).codePointAt(1)"), "55357");
        // A high and a low surrogate in the same string stay in that order, so
        // the copy is a pair only where the original was one.
        assert_eq!(units_of("'\\uD83D\\uDE00'.repeat(1)"), "d83d de00");
        assert_eq!(units_of("'\\uDE00\\uD83D'.repeat(1)"), "de00 d83d");
        assert_eq!(run("'\\uDE00\\uD83D'.repeat(1).codePointAt(0)"), "56832");
    }

    /// `trimStart`/`trimEnd` are `trim` restricted to one end, and the whitespace
    /// set is JavaScript's rather than Rust's: U+0085 is *not* trimmed, because
    /// it is not in JavaScript's `WhiteSpace` production while `str::trim`
    /// removes it.
    #[test]
    fn trim_start_and_trim_end_trim_one_end_each() {
        assert_eq!(run("'  a  '.trimStart() + '|'"), "a  |");
        assert_eq!(run("'  a  '.trimEnd() + '|'"), "  a|");
        assert_eq!(run("'\\t\\n a \\r'.trimStart() + '|'"), "a \r|");
        assert_eq!(run("'\\u00a0a\\u00a0'.trimStart() + '|'"), "a\u{00a0}|");
        // U+0085: kept by both, because it is not `WhiteSpace` in JavaScript
        // even though `str::trim` removes it. Compared as content, for the same
        // reason as the astral filler above.
        assert_eq!(run("'\\u0085a'.trimStart() === '\\u0085a'"), "true");
        assert_eq!(run("'a\\u0085'.trimEnd() === 'a\\u0085'"), "true");
        // U+00A0 *is* `WhiteSpace`, so the same two calls do remove it.
        assert_eq!(run("'\\u00a0a\\u00a0'.trimStart() === 'a\\u00a0'"), "true");
    }

    /// `repeat` is a `RangeError` for a negative or infinite count, and `NaN` is
    /// `0` because `ToIntegerOrInfinity(NaN)` is `0`.
    #[test]
    fn repeat_rejects_an_impossible_count() {
        assert_eq!(run("'ab'.repeat(3)"), "ababab");
        assert_eq!(run("''.repeat(3) + '|'"), "|");
        assert_eq!(run("'a'.repeat(0) + '|'"), "|");
        assert_eq!(run("String('a'.repeat(NaN)) + '|'"), "|");
        // A missing count is `0` (`ToIntegerOrInfinity(undefined)` is 0), and a
        // negative or infinite one is a `RangeError` from `StringRepeat`.
        assert_eq!(run("'a'.repeat() + '|'"), "|");
        assert_eq!(
            caught("'a'.repeat(-1)"),
            "RangeError: String.prototype.repeat count is out of range"
        );
        assert_eq!(
            caught("'a'.repeat(Infinity)"),
            "RangeError: String.prototype.repeat count is out of range"
        );
        // Bounded by the engine's materialization cap rather than by the heap.
        assert_eq!(
            caught("'a'.repeat(1000000000000)"),
            "RangeError: String.prototype.repeat result exceeds the engine limit"
        );
    }

    /// `localeCompare` is implementation-defined by the specification, and the
    /// only part every caller reads is the sign. A hardcoded `0` would make
    /// `sort` treat every pair as equal and leave the input order in place, so
    /// the sort is the assertion.
    #[test]
    fn locale_compare_orders_strings_and_drives_a_sort() {
        assert_eq!(run("'a'.localeCompare('b')"), "-1");
        assert_eq!(run("'b'.localeCompare('a')"), "1");
        assert_eq!(run("'a'.localeCompare('a')"), "0");
        assert_eq!(
            run("['c','a','b'].sort(function (x, y) { return x.localeCompare(y); }).join('')"),
            "abc"
        );
        // A missing argument is a `TypeError`: `localeCompare(that)` is not an
        // optional argument, and the specification's own step 1 is "Let
        // comparator be ? Get(@@localeCompare, « this, »); perform ? Call(«
        // comparator, undefined, « to, this »»)" - a `Call` on a missing argument
        // throws.
        assert_eq!(
            caught("'a'.localeCompare()"),
            "TypeError: localeCompare requires at least 1 argument(s)"
        );
    }

    /// The two halves of `replaceAll`'s difference from `replace`: a string
    /// pattern replaces every occurrence, and a `RegExp` pattern must carry `g`.
    #[test]
    fn replace_all_replaces_every_occurrence_and_demands_a_global_regexp() {
        assert_eq!(run("'a-b-a'.replaceAll('-', '+')"), "a+b+a");
        assert_eq!(run("'a-b-a'.replace('-', '+')"), "a+b-a");
        // The empty pattern inserts at every boundary *including* both ends,
        // which is the case a `split`/`join` gets wrong.
        assert_eq!(run("'ab'.replaceAll('', '-')"), "-a-b-");
        // The `$` substitutions are the ones `replace` uses.
        assert_eq!(run("'ab'.replaceAll('b', '[$&]')"), "a[b]");
        assert_eq!(run("'ab'.replaceAll('b', '[$`]')"), "a[a]");
        assert_eq!(run("'aa'.replaceAll('a', '$$')"), "$$");
        assert_eq!(
            run("'a-a'.replaceAll('a', function (m, offset) { return offset; })"),
            "0-2"
        );
        // A global RegExp works, and a non-global one is a `TypeError`, because
        // silently treating it as `replace` would answer a different question
        // than the same expression with `g`. `replace` with `g` replaces every
        // match, which is the contrast the two entry points are about.
        assert_eq!(run("'a1b2'.replaceAll(/[0-9]/g, '#')"), "a#b#");
        assert_eq!(run("'a1b2'.replace(/[0-9]/g, '#')"), "a#b#");
        assert_eq!(run("'a1b2'.replace(/[0-9]/, '#')"), "a#b2");
        assert_eq!(
            caught("'a1'.replaceAll(/[0-9]/, '#')"),
            "TypeError: String.prototype.replaceAll must be called with a global RegExp"
        );
    }

    /// The single-index operations, and what each one does at a position that
    /// lands between the two halves of a pair.
    ///
    /// The expectation is *not* "never a half": §6.1.4 defines a String as a
    /// sequence of code units, so `charAt`, `at` and every substring operation
    /// return one code unit and a request inside a pair returns that half. An
    /// implementation that returned the whole character instead could not reach
    /// the trailing half at all, and `charAt(0)` would disagree with
    /// `substring(0, 1)` even though the specification defines the first *in
    /// terms of* the second. What must hold is the other half of the contract:
    /// a returned half is a real lone surrogate, readable back as its own
    /// code unit, and two halves still add back up to the pair.
    #[test]
    fn single_index_operations_return_one_code_unit() {
        // §22.1.3.2 `charAt`: "the substring of string from position to
        // position + 1". This is the assertion that was declined before the
        // model was split: under the old code-point model it answered the whole
        // emoji, so `charCodeAt(0)` was 0x1F600 and `'😀'.length` was 1.
        assert_eq!(run("'\\u{1f600}'.charCodeAt(0)"), "55357");
        assert_eq!(run("'\\u{1f600}'.charCodeAt(0) === 0xD83D"), "true");
        assert_eq!(run("'\\u{1f600}'.charCodeAt(1)"), "56832");
        assert_eq!(run("'\\u{1f600}'.charCodeAt(1) === 0xDE00"), "true");
        assert_eq!(run("String('\\u{1f600}'.charCodeAt(2))"), "NaN");
        assert_eq!(run("String('\\u{1f600}'.charCodeAt(-1))"), "NaN");
        assert_eq!(run("String('\\u{1f600}'.charCodeAt(1.5))"), "56832");
        // `charAt` returns the same single unit.
        assert_eq!(units_of("'\\u{1f600}'.charAt(0)"), "d83d");
        assert_eq!(units_of("'\\u{1f600}'.charAt(1)"), "de00");
        assert_eq!(length_of("'\\u{1f600}'.charAt(0)"), "1");
        assert_eq!(run("'\\u{1f600}'.charAt(2) === ''"), "true");
        assert_eq!(run("'\\u{1f600}'.charAt(-1) === ''"), "true");
        // Indexed access on a String exotic object is the same answer.
        assert_eq!(run("'\\u{1f600}'[0] === '\\u{1f600}'.charAt(0)"), "true");
        assert_eq!(run("'\\u{1f600}'[1] === '\\u{1f600}'.charAt(1)"), "true");
        assert_eq!(run("String('\\u{1f600}'[2])"), "undefined");
        // A lone surrogate is one addressable unit, and the two halves of a pair
        // still concatenate back into the pair they came from.
        assert_eq!(length_of("'\\uD83D'"), "1");
        assert_eq!(run("'\\uD83D'.charCodeAt(0)"), "55357");
        assert_eq!(run("'\\uD83D'.charAt(0) === '\\uD83D'"), "true");
        assert_eq!(
            run("'\\u{1f600}'.charAt(0) + '\\u{1f600}'.charAt(1) === '\\u{1f600}'"),
            "true"
        );
        assert_eq!(
            run("'\\u{1f600}'.slice(0, 1) + '\\u{1f600}'.slice(1) === '\\u{1f600}'"),
            "true"
        );
    }

    /// The range operations, over code units, with the clamping each one
    /// specifies: `slice` empties on reversed bounds, `substring` swaps them,
    /// and `substr` clamps its run length rather than resolving it from the end.
    #[test]
    fn range_operations_slice_in_code_units() {
        // §22.1.3.22 `slice`: `from ≥ to` is the empty String.
        assert_eq!(units_of("'\\u{1f600}'.slice(0, 1)"), "d83d");
        assert_eq!(units_of("'\\u{1f600}'.slice(1, 2)"), "de00");
        assert_eq!(units_of("'\\u{1f600}'.slice(1)"), "de00");
        assert_eq!(units_of("'\\u{1f600}'.slice(-2)"), "d83d de00");
        assert_eq!(units_of("'\\u{1f600}'.slice(-1)"), "de00");
        assert_eq!(length_of("'\\u{1f600}'.slice(0, 1)"), "1");
        assert_eq!(run("'\\u{1f600}'.slice(2, 9) === ''"), "true");
        assert_eq!(run("'\\u{1f600}'.slice(9, 5) === ''"), "true");
        assert_eq!(run("'\\u{1f600}'.slice(NaN, 1) === '\\uD83D'"), "true");
        assert_eq!(run("'\\u{1f600}'.slice(1.7, 0.2) === ''"), "true");
        // §22.1.3.25 `substring`: reversed bounds are swapped, not emptied.
        assert_eq!(units_of("'\\u{1f600}'.substring(0, 1)"), "d83d");
        assert_eq!(units_of("'\\u{1f600}'.substring(1, 0)"), "d83d");
        assert_eq!(units_of("'\\u{1f600}'.substring(1, 2)"), "de00");
        assert_eq!(units_of("'\\u{1f600}'.substring(5, 9)"), "");
        assert_eq!(units_of("'\\u{1f600}'.substring(-3, 9)"), "d83d de00");
        assert_eq!(run("'\\u{1f600}'.substring(NaN, 1) === '\\uD83D'"), "true");
        assert_eq!(
            run("'\\u{1f600}'.substring(1.7, 0.2) === '\\uD83D'"),
            "true"
        );
        // B.2.2.1 `substr`: the run length is clamped into `[0, size]`, so a
        // negative length is empty and an oversized one runs to the end.
        assert_eq!(units_of("'\\u{1f600}'.substr(0, 1)"), "d83d");
        assert_eq!(units_of("'\\u{1f600}'.substr(1, 1)"), "de00");
        assert_eq!(units_of("'\\u{1f600}'.substr(1)"), "de00");
        assert_eq!(units_of("'\\u{1f600}'.substr(-1, 1)"), "de00");
        assert_eq!(units_of("'\\u{1f600}'.substr()"), "d83d de00");
        assert_eq!(length_of("'\\u{1f600}'.substr()"), "2");
        assert_eq!(run("'\\u{1f600}'.substr(0, -1) === ''"), "true");
        assert_eq!(run("'\\u{1f600}'.substr(1.7, 0.2) === ''"), "true");
        assert_eq!(units_of("'\\u{1f600}'.substr(-5, 99)"), "d83d de00");
        // A lone surrogate is one unit to every one of them.
        assert_eq!(length_of("'\\uD83D'.slice(0, 1)"), "1");
        assert_eq!(length_of("'\\uD83D'.substring(0, 1)"), "1");
        assert_eq!(length_of("'\\uD83D'.substr(0, 1)"), "1");
    }

    /// The search operations compare *code units*, which is what makes a lone
    /// surrogate findable inside a pair. A search by code point cannot answer
    /// `'😀'.indexOf('\uDE00')` at all, because the trailing half is not a
    /// separate value in that model.
    #[test]
    fn search_operations_find_the_halves_of_a_pair() {
        // The `b` after a pair is at code-unit 3: the pair is two units wide, so
        // a code-point model would say 2 and hand back the wrong character for
        // `slice(index, index + 1)`.
        assert_eq!(run("'a\\u{1f600}b'.indexOf('b')"), "3");
        assert_eq!(run("'a\\u{1f600}b'.slice(3)"), "b");
        assert_eq!(run("'a\\u{1f600}b'.lastIndexOf('b')"), "3");
        assert_eq!(run("'\\u{1f600}'.indexOf('\\uD83D')"), "0");
        assert_eq!(run("'\\u{1f600}'.indexOf('\\uDE00')"), "1");
        assert_eq!(run("'\\u{1f600}'.lastIndexOf('\\uDE00')"), "1");
        assert_eq!(run("'\\u{1f600}'.indexOf('\\u{1f600}')"), "0");
        assert_eq!(run("'a\\u{1f600}'.lastIndexOf('\\uD83D')"), "1");
        // A needle that is only half a pair does not match the whole pair.
        assert_eq!(run("'a\\u{1f600}'.indexOf('\\uDE00')"), "2");
        // Two pairs are four units, and the trailing halves are at 1 and 3.
        assert_eq!(run("'\\u{1f600}\\u{1f600}'.lastIndexOf('\\uDE00')"), "3");
        assert_eq!(run("'\\u{1f600}\\u{1f600}'.indexOf('\\uDE00')"), "1");
        // `includes`, `startsWith` and `endsWith` agree.
        assert_eq!(run("'\\u{1f600}'.includes('\\uDE00')"), "true");
        assert_eq!(run("'\\u{1f600}'.includes('\\uD83D')"), "true");
        assert_eq!(run("'\\u{1f600}'.includes('\\uDE00\\uD83D')"), "false");
        assert_eq!(run("'\\u{1f600}'.startsWith('\\uD83D')"), "true");
        assert_eq!(run("'\\u{1f600}'.startsWith('\\uDE00')"), "false");
        assert_eq!(run("'\\u{1f600}'.endsWith('\\uDE00')"), "true");
        assert_eq!(run("'\\u{1f600}'.endsWith('\\uD83D')"), "false");
        // The empty needle is found at 0 by `indexOf` and at the length by
        // `lastIndexOf`, in code units.
        assert_eq!(run("'\\u{1f600}'.indexOf('')"), "0");
        assert_eq!(run("'\\u{1f600}'.lastIndexOf('')"), "2");
        assert_eq!(run("'\\u{1f600}'.startsWith('')"), "true");
        assert_eq!(run("'\\u{1f600}'.endsWith('')"), "true");
        assert_eq!(run("'\\u{1f600}'.includes('')"), "true");
        // A lone surrogate in the receiver is findable like any other unit.
        assert_eq!(run("'a\\uD83Db'.indexOf('\\uD83D')"), "1");
        assert_eq!(run("'a\\uD83Db'.indexOf('\\uDE00')"), "-1");
        // Plain ASCII is unchanged, which is the regression guard for the
        // substring search these operations now share.
        assert_eq!(run("'hello'.indexOf('ll')"), "2");
        assert_eq!(run("'hello'.lastIndexOf('l')"), "3");
        assert_eq!(run("'hello'.indexOf('z')"), "-1");
        assert_eq!(run("'hello'.includes('ell')"), "true");
        assert_eq!(run("'hello'.startsWith('he')"), "true");
        assert_eq!(run("'hello'.endsWith('lo')"), "true");
    }

    /// A lone surrogate that is *not* part of a pair. The source literal
    /// `'\uD83D\uDE00'` would be a pair, so these are built with
    /// `String.fromCharCode`, which is how a script actually produces one.
    ///
    /// This is the case a code-point model cannot represent at all: it has no
    /// value for half a character, so it must either answer the whole emoji or
    /// lose the character. The engine's placeholder representation keeps it,
    /// which is what makes `charCodeAt`, `indexOf` and `codePointAt` agree that
    /// unit 0 is `0xD83D` and not a pair.
    #[test]
    fn a_lone_surrogate_is_one_addressable_unit() {
        let lone = "String.fromCharCode(0xD83D)";
        assert_eq!(length_of(lone), "1");
        assert_eq!(run(&format!("{lone}.charCodeAt(0)")), "55357");
        assert_eq!(run(&format!("{lone}.codePointAt(0)")), "55357");
        assert_eq!(run(&format!("String({lone}.charCodeAt(1))")), "NaN");
        assert_eq!(run(&format!("{lone}.charAt(0) === {lone}")), "true");
        assert_eq!(units_of(&format!("{lone}.at(-1)")), "d83d");
        assert_eq!(run(&format!("String({lone}.at(1))")), "undefined");
        // It is findable inside a longer string, and it is not the pair.
        assert_eq!(
            run("String(('a' + String.fromCharCode(0xD83D) + 'b').length)"),
            "3"
        );
        assert_eq!(
            run("('a' + String.fromCharCode(0xD83D)).indexOf('\\uD83D')"),
            "1"
        );
        assert_eq!(
            run("('a' + String.fromCharCode(0xD83D)).indexOf('\\uDE00')"),
            "-1"
        );
        // A trailing surrogate is the mirror case: also one unit, and it is
        // never the lead of a pair.
        let trailing = "String.fromCharCode(0xDE00)";
        assert_eq!(length_of(trailing), "1");
        assert_eq!(run(&format!("{trailing}.charCodeAt(0)")), "56832");
        assert_eq!(run(&format!("{trailing}.codePointAt(0)")), "56832");
        assert_eq!(
            run(&format!("String({trailing}.codePointAt(1))")),
            "undefined"
        );
        // Concatenating the two halves in order *does* form a pair, because
        // concatenation is a code-unit concatenation - so the result is
        // searchable as the pair, and `codePointAt` reports the scalar. This is
        // why the two must be built separately above to stay lone.
        assert_eq!(length_of(&format!("{lone} + {trailing}")), "2");
        assert_eq!(
            run(&format!("({lone} + {trailing}).codePointAt(0)")),
            "128512"
        );
        assert_eq!(
            run(&format!("({lone} + {trailing}).indexOf('\\u{{1f600}}')")),
            "0"
        );
        // In the other order they stay two lone surrogates, since a pair needs a
        // leading surrogate first.
        assert_eq!(
            run(&format!("({trailing} + {lone}).codePointAt(0)")),
            "56832"
        );
        assert_eq!(
            run(&format!("({trailing} + {lone}).indexOf('\\u{{1f600}}')")),
            "-1"
        );
    }

    /// Concatenation is a code-unit concatenation, so two halves either side of
    /// a join become one pair. The engine holds a lone surrogate as a
    /// private-use placeholder, so this is where a plain text concatenation
    /// would visibly disagree with the specified answer.
    #[test]
    fn concatenation_re_joins_a_pair_split_across_the_operator() {
        let lone = "String.fromCharCode(0xD83D)";
        let trailing = "String.fromCharCode(0xDE00)";
        // `charAt` hands back the halves; putting them back together is the
        // pair, and the specification's `substring(pos, pos+1)` note means the
        // two must round-trip.
        assert_eq!(
            run("'\\u{1f600}'.charAt(0) + '\\u{1f600}'.charAt(1) === '\\u{1f600}'"),
            "true"
        );
        assert_eq!(
            run("'\\u{1f600}'.slice(0,1) + '\\u{1f600}'.slice(1) === '\\u{1f600}'"),
            "true"
        );
        assert_eq!(
            run(&format!("{lone} + {trailing} === '\\u{{1f600}}'")),
            "true"
        );
        assert_eq!(
            run(&format!("String(({lone} + {trailing}).codePointAt(0))")),
            "128512"
        );
        // The other order is not a pair, and joining it must not invent one.
        assert_eq!(
            run(&format!("({trailing} + {lone}).codePointAt(0)")),
            "56832"
        );
        assert_eq!(length_of(&format!("{trailing} + {lone}")), "2");
        // `concat` and `+=` are the same operation.
        assert_eq!(
            run(&format!("''.concat({lone}, {trailing}) === '\\u{{1f600}}'")),
            "true"
        );
        assert_eq!(
            run(&format!(
                "var s = {lone}; s += {trailing}; s === '\\u{{1f600}}'"
            )),
            "true"
        );
        // A pair is only ever formed from a leading surrogate *followed* by a
        // trailing one, so prefixing a low surrogate with another character does
        // not complete one - the result is a lone low surrogate and a scalar.
        assert_eq!(
            run("'\\u{1f600}'.slice(1) + String.fromCharCode(0x1F601) === '\\u{1F600}\\u{1F601}'"),
            "false"
        );
        assert_eq!(
            length_of("'\\u{1f600}'.slice(1) + String.fromCharCode(0x1F601)"),
            "2"
        );
        assert_eq!(
            run("'\\u{1f600}'.slice(1).concat(String.fromCharCode(0x1F601)).codePointAt(0)"),
            "56832"
        );
    }

    /// `split` with an empty separator is §22.1.3.32's code-unit split, the
    /// same notion `length` and `charAt` use.
    #[test]
    fn split_with_an_empty_separator_splits_code_units() {
        assert_eq!(run("'abc'.split('').length"), "3");
        assert_eq!(run("'\\u{1f600}'.split('').length"), "2");
        assert_eq!(units_of_each("'\\u{1f600}'.split('')"), "d83d|de00");
        assert_eq!(units_of_each("'\\uD83D'.split('')"), "d83d");
        assert_eq!(units_of_each("'a\\u{1f600}'.split('')"), "0061|d83d|de00");
        // A non-empty separator splits on a substring, so a lone surrogate can
        // split a pair in half.
        assert_eq!(run("'\\u{1f600}'.split('\\uD83D').length"), "2");
        assert_eq!(units_of_each("'\\u{1f600}'.split('\\uD83D')"), "|de00");
        assert_eq!(
            units_of_each("'a\\u{1f600}'.split('\\uDE00')"),
            "0061 d83d|"
        );
        assert_eq!(units_of_each("'\\u{1f600}'.split('\\u{1f600}')"), "|");
        assert_eq!(run("'abc'.split('b').join('|')"), "a|c");
    }

    /// `replaceAll` with a literal search value, over code units: it finds a
    /// lone surrogate inside a pair and reports the offset in code units.
    #[test]
    fn replace_all_literal_searches_and_reports_in_code_units() {
        assert_eq!(run("'a\\uD83Db'.replaceAll('\\uD83D', 'X')"), "aXb");
        assert_eq!(run("'a\\uD83Db'.replaceAll('\\uD83D', 'X').length"), "3");
        // Cutting the trailing half out of a pair leaves a lone high surrogate,
        // read back as its own code unit.
        assert_eq!(
            units_of("'\\u{1f600}'.replaceAll('\\uDE00', 'X')"),
            "d83d 0058"
        );
        assert_eq!(length_of("'\\u{1f600}'.replaceAll('\\uDE00', 'X')"), "2");
        assert_eq!(
            run("'\\u{1f600}'.replaceAll('\\uDE00', 'X').codePointAt(0)"),
            "55357"
        );
        // Substituting a lone high surrogate into an empty string leaves one
        // unit, because the join that would have completed a pair is not there.
        assert_eq!(units_of("''.replaceAll('', '\\uD83D')"), "d83d");
        // Replacing a whole pair keeps the pair.
        assert_eq!(run("'\\u{1f600}'.replaceAll('\\u{1f600}', 'X')"), "X");
        assert_eq!(length_of("'\\u{1f600}'.replaceAll('\\u{1f600}', 'X')"), "1");
        // The reported offset is a code-unit offset: the trailing half of the
        // pair sits at unit 2 of `"a\u{1F600}"`, not unit 1.
        assert_eq!(
            run("'a\\u{1f600}'.replaceAll('\\uDE00', function (m, o) { return o; })"),
            "a\u{f003d}2"
        );
        assert_eq!(
            run(
                "String('a\\u{1f600}'.replaceAll('\\uDE00', function (m, o) { return o; }).length)"
            ),
            "3"
        );
        // A whole-pair needle is reported at the pair's own start.
        assert_eq!(
            run("'a\\u{1f600}'.replaceAll('\\u{1f600}', function (m, o) { return o; })"),
            "a1"
        );
        // The empty search value still inserts at every boundary, and the
        // boundaries are now code-unit boundaries, so a pair is interrupted
        // twice rather than once.
        assert_eq!(run("'ab'.replaceAll('', '-')"), "-a-b-");
        assert_eq!(
            units_of("'ab'.replaceAll('', '-')"),
            "002d 0061 002d 0062 002d"
        );
        assert_eq!(
            units_of("'\\u{1f600}'.replaceAll('', '-')"),
            "002d d83d 002d de00 002d"
        );
        assert_eq!(length_of("'\\u{1f600}'.replaceAll('', '-')"), "5");
    }

    /// `codePointAt` and `at` were implemented in the same round and address
    /// the same string. They must not disagree about what a position means: both
    /// number positions in code units, and they differ only in what they return
    /// from one. This is the consistency check between the two, and it is what
    /// would have caught the code-point indexing `at` had.
    #[test]
    fn code_point_at_and_at_agree_on_what_a_position_is() {
        // Same position, same unit: `at` hands back the unit, `codePointAt`
        // hands back the code point that starts there.
        assert_eq!(units_of("'\\u{1f600}'.at(0)"), "d83d");
        assert_eq!(run("'\\u{1f600}'.codePointAt(0)"), "128512");
        // The trailing half: `at` returns it, `codePointAt` reports it as its
        // own code point rather than pairing it backwards.
        assert_eq!(units_of("'\\u{1f600}'.at(1)"), "de00");
        assert_eq!(run("'\\u{1f600}'.codePointAt(1)"), "56832");
        // Reading the code point at the position `at` reported must give the
        // same unit back, for every position of an astral string.
        assert_eq!(
            run("var s = '\\u{1f600}'; var out = []; \
                 for (var i = 0; i < s.length; i++) { \
                   out.push(s.at(i).charCodeAt(0)); } out.join(',')"),
            "55357,56832"
        );
        // And the length both of them are bounded by.
        assert_eq!(length_of("'\\u{1f600}'"), "2");
        assert_eq!(run("'\\u{1f600}'.at(2) === undefined"), "true");
        assert_eq!(run("String('\\u{1f600}'.codePointAt(2))"), "undefined");
    }

    /// The walk side of the model: the string iterator yields code points, so
    /// `for...of`, spread, destructuring and `Array.from` see one element for
    /// an astral character - while `length` on that same element is 2.
    #[test]
    fn iteration_yields_code_points_while_length_counts_units() {
        assert_eq!(run("[...'\\u{1f600}'].length"), "1");
        assert_eq!(run("[...'\\u{1f600}'][0].length"), "2");
        assert_eq!(run("Array.from('\\u{1f600}').length"), "1");
        assert_eq!(run("Array.from('\\u{1f600}')[0].length"), "2");
        assert_eq!(run("Array.from('\\u{1f600}')[0] === '\\u{1f600}'"), "true");
        assert_eq!(
            run("var n = 0; for (var c of '\\u{1f600}') { n += c.length; } n"),
            "2"
        );
        assert_eq!(
            run("var n = 0; for (var c of 'a\\u{1f600}') { n += c.length; } n"),
            "3"
        );
        // A lone surrogate iterates as one element of one unit.
        assert_eq!(run("[...'\\uD83D'].length"), "1");
        assert_eq!(run("[...'\\uD83D'][0].length"), "1");
        assert_eq!(run("[...'\\uD83D'][0] === '\\uD83D'"), "true");
        // A high surrogate followed by a low one is a pair wherever it came from,
        // so the source literal and two separately built halves agree.
        assert_eq!(run("[...'\\uD83D\\uDE00'].length"), "1");
        assert_eq!(run("Array.from('\\uD83D\\uDE00').length"), "1");
        assert_eq!(run("Array.from('\\uD83D\\uDE00')[0].length"), "2");
        assert_eq!(
            run("[...(String.fromCharCode(0xD83D) + String.fromCharCode(0xDE00))].length"),
            "1"
        );
        // The reversed order is not a pair, so it iterates as two elements.
        assert_eq!(run("[...'\\uDE00\\uD83D'].length"), "2");
        // The same string, both answers, together.
        assert_eq!(
            run("var s = 'a\\u{1f600}'; s.length + ':' + [...s].length"),
            "3:2"
        );
    }
}
