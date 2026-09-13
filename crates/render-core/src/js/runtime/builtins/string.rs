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

use crate::dom::Dom;
use crate::js::JsError;
use crate::js::JsValue;
use crate::js::ObjectId;
use crate::js::lexer::surrogate_placeholder;
use crate::js::runtime::JsRuntime;
use crate::js::runtime::convert::optional_index;
use crate::js::runtime::convert::required_argument;
use crate::js::runtime::convert::slice_range;
use crate::js::runtime::convert::to_number;
use crate::js::value::NativeFunction;
use crate::js::value::ObjectHost;
use crate::js::value::number_to_string;

impl JsRuntime {
    pub(in crate::js::runtime) fn dispatch_string_native(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match function {
            NativeFunction::StringFromCharCode => Self::string_from_char_code(arguments),
            NativeFunction::StringFromCodePoint => Self::string_from_code_point(arguments),
            NativeFunction::StringRaw => Ok(JsValue::String(
                arguments
                    .first()
                    .map_or_else(String::new, JsValue::to_js_string),
            )),
            NativeFunction::StringSubstr => {
                let text = self.require_string_receiver(receiver)?;
                let characters: Vec<char> = text.chars().collect();
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "substr indices are validated small integers"
                )]
                let start = match optional_index(arguments.first()) {
                    Ok(value) => {
                        let raw = value as i64;
                        if raw < 0 {
                            characters.len().saturating_sub(raw.unsigned_abs() as usize)
                        } else {
                            (raw as usize).min(characters.len())
                        }
                    }
                    Err(_) => 0,
                };
                let length = match arguments.get(1) {
                    None | Some(JsValue::Undefined) => characters.len() - start,
                    Some(value) => {
                        #[allow(
                            clippy::cast_possible_truncation,
                            clippy::cast_sign_loss,
                            reason = "substr lengths are validated small integers"
                        )]
                        {
                            to_number(value)?.max(0.0) as usize
                        }
                    }
                };
                let end = (start + length).min(characters.len());
                Ok(JsValue::String(
                    characters[start.min(end)..end].iter().collect(),
                ))
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
pub(in crate::js::runtime) fn valid_position(position: f64, length: usize) -> Option<usize> {
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

pub(in crate::js::runtime) fn char_at_value(characters: &[char], position: f64) -> JsValue {
    match valid_position(position, characters.len()) {
        Some(position) => JsValue::String(characters[position].to_string()),
        None => JsValue::String(String::new()),
    }
}

/// Collect spans of every non-overlapping match honouring empty-match
/// advancement; used by global matching.
pub(in crate::js::runtime) fn collect_global_matches(
    compiled: &crate::js::regex::Compiled,
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
pub(in crate::js::runtime) fn split_by_regex(
    compiled: &crate::js::regex::Compiled,
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
pub(in crate::js::runtime) fn expand_replacement(
    replacement: &str,
    input: &[char],
    found: &crate::js::regex::MatchRanges,
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
                let group = digit as usize - '1' as usize;
                index += 2;
                if let Some(Some((start, end))) = found.groups.get(group) {
                    output.extend(&input[*start..*end]);
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

/// Map a `String.prototype` method name to its native implementation.
pub(in crate::js::runtime) fn string_method_native(name: &str) -> Option<NativeFunction> {
    match name {
        "charAt" => Some(NativeFunction::StrCharAt),
        "charCodeAt" => Some(NativeFunction::StrCharCodeAt),
        "indexOf" => Some(NativeFunction::StrIndexOf),
        "lastIndexOf" => Some(NativeFunction::StrLastIndexOf),
        "includes" => Some(NativeFunction::StrIncludes),
        "startsWith" => Some(NativeFunction::StrStartsWith),
        "endsWith" => Some(NativeFunction::StrEndsWith),
        "slice" => Some(NativeFunction::StrSlice),
        "substring" => Some(NativeFunction::StrSubstring),
        "toLowerCase" => Some(NativeFunction::StrToLowerCase),
        "toUpperCase" => Some(NativeFunction::StrToUpperCase),
        "trim" => Some(NativeFunction::StrTrim),
        "split" => Some(NativeFunction::StrSplit),
        "replace" => Some(NativeFunction::StrReplace),
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
pub(in crate::js::runtime) fn is_string_native(function: NativeFunction) -> bool {
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
    pub(in crate::js::runtime) fn string_wrapper(&mut self, value: String) -> ObjectId {
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
    pub(in crate::js::runtime) fn require_string_receiver(
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
    pub(in crate::js::runtime) fn require_string_object(
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

    pub(in crate::js::runtime) fn string_char_at(
        &self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let text = self.require_string_receiver(receiver)?;
        let characters: Vec<char> = text.chars().collect();
        let position = optional_index(arguments.first()).unwrap_or(0.0);
        Ok(char_at_value(&characters, position))
    }

    pub(in crate::js::runtime) fn string_char_code_at(
        &self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let text = self.require_string_receiver(receiver)?;
        let characters: Vec<char> = text.chars().collect();
        let position = optional_index(arguments.first()).unwrap_or(0.0);
        let Some(position) = valid_position(position, characters.len()) else {
            return Ok(JsValue::Number(f64::NAN));
        };
        #[allow(
            clippy::cast_precision_loss,
            reason = "code points fit exactly in binary64"
        )]
        let code = f64::from(characters[position] as u32);
        Ok(JsValue::Number(code))
    }

    pub(in crate::js::runtime) fn string_from_char_code(
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let mut units = Vec::with_capacity(arguments.len());
        for value in arguments {
            let number = to_number(value)?;
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
        let text = char::decode_utf16(units)
            .map(|result| {
                result.unwrap_or_else(|error| {
                    surrogate_placeholder(u32::from(error.unpaired_surrogate()))
                })
            })
            .collect();
        Ok(JsValue::String(text))
    }

    pub(in crate::js::runtime) fn string_from_code_point(
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let mut text = String::with_capacity(arguments.len());
        for value in arguments {
            let number = to_number(value)?;
            if !number.is_finite()
                || number.fract() != 0.0
                || !(0.0..=1_114_111.0).contains(&number)
            {
                return Err(JsError::type_error("invalid code point"));
            }
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "the preceding range and integer checks guarantee a Unicode scalar input"
            )]
            let code = number as u32;
            let Some(character) = char::from_u32(code) else {
                return Err(JsError::type_error("invalid code point"));
            };
            text.push(character);
        }
        Ok(JsValue::String(text))
    }

    pub(in crate::js::runtime) fn string_index_of(
        &self,
        receiver: ObjectId,
        arguments: &[JsValue],
        from_end: bool,
    ) -> Result<JsValue, JsError> {
        let text = self.require_string_receiver(receiver)?;
        let needle = required_argument(arguments, 0, "indexOf")?.to_js_string();
        let characters: Vec<char> = text.chars().collect();
        let needle_characters: Vec<char> = needle.chars().collect();
        if needle_characters.is_empty() {
            #[allow(
                clippy::cast_precision_loss,
                reason = "string lengths stay far below any precision boundary"
            )]
            let position = if from_end {
                characters.len() as f64
            } else {
                0.0
            };
            return Ok(JsValue::Number(position));
        }
        let positions: Vec<usize> = (0..=characters.len().saturating_sub(needle_characters.len()))
            .filter(|start| {
                characters[*start..]
                    .iter()
                    .zip(&needle_characters)
                    .all(|(left, right)| left == right)
            })
            .collect();
        let found = if from_end {
            positions.into_iter().next_back()
        } else {
            positions.into_iter().next()
        };
        Ok(match found {
            Some(position) => {
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "string lengths stay far below any precision boundary"
                )]
                let position = position as f64;
                JsValue::Number(position)
            }
            None => JsValue::Number(-1.0),
        })
    }

    pub(in crate::js::runtime) fn string_includes(
        &self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let text = self.require_string_receiver(receiver)?;
        let needle = required_argument(arguments, 0, "includes")?.to_js_string();
        Ok(JsValue::Boolean(text.contains(&needle)))
    }

    pub(in crate::js::runtime) fn string_starts_or_ends_with(
        &self,
        receiver: ObjectId,
        arguments: &[JsValue],
        starts: bool,
    ) -> Result<JsValue, JsError> {
        let text = self.require_string_receiver(receiver)?;
        let needle = required_argument(arguments, 0, "startsWith")?.to_js_string();
        Ok(JsValue::Boolean(if starts {
            text.starts_with(&needle)
        } else {
            text.ends_with(&needle)
        }))
    }

    pub(in crate::js::runtime) fn string_slice(
        &self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let text = self.require_string_receiver(receiver)?;
        let characters: Vec<char> = text.chars().collect();
        let start = optional_index(arguments.first())?;
        let end = match arguments.get(1) {
            None | Some(JsValue::Undefined) => None,
            Some(value) => Some(to_number(value)?),
        };
        let range = slice_range(&characters, start, end, true);
        Ok(JsValue::String(
            characters[range.start..range.end].iter().collect(),
        ))
    }

    pub(in crate::js::runtime) fn string_substring(
        &self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let text = self.require_string_receiver(receiver)?;
        let characters: Vec<char> = text.chars().collect();
        let start = optional_index(arguments.first())?;
        let end = match arguments.get(1) {
            None | Some(JsValue::Undefined) => None,
            Some(value) => Some(to_number(value)?),
        };
        let range = slice_range(&characters, start, end, false);
        Ok(JsValue::String(
            characters[range.start..range.end].iter().collect(),
        ))
    }

    pub(in crate::js::runtime) fn string_to_case(
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

    pub(in crate::js::runtime) fn string_trim(
        &self,
        receiver: ObjectId,
    ) -> Result<JsValue, JsError> {
        let text = self.require_string_receiver(receiver)?;
        Ok(JsValue::String(text.trim().to_owned()))
    }

    pub(in crate::js::runtime) fn string_concat(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let mut text = self.require_string_receiver(receiver)?;
        for argument in arguments {
            text.push_str(&argument.to_js_string());
        }
        Ok(JsValue::String(text))
    }

    /// Split by a literal separator or a regular expression.
    pub(in crate::js::runtime) fn string_split(
        &mut self,
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
            Some(value) => to_number(value)?.max(0.0).min(f64::from(u32::MAX)) as usize,
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
                        characters
                            .iter()
                            .take(limit)
                            .map(std::string::ToString::to_string)
                            .collect()
                    } else if limit == 0 {
                        Vec::new()
                    } else {
                        text.split(&separator)
                            .take(limit)
                            .map(str::to_owned)
                            .collect()
                    }
                }
            }
        };
        let values = pieces.into_iter().map(JsValue::String).collect::<Vec<_>>();
        Ok(JsValue::Object(self.create_array_from_values(&values)?))
    }

    /// `String.prototype.match`: one exec-style result unless the regex is
    /// global, in which case every full match is collected.
    pub(in crate::js::runtime) fn string_match(
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
            return match self.regex_exec_value(index, &characters, 0)? {
                Some(value) => Ok(value),
                None => Ok(JsValue::Null),
            };
        };
        let (index, global) = self.coerce_pattern_argument(argument)?;
        if !global {
            return match self.regex_exec_value(index, &characters, 0)? {
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

    pub(in crate::js::runtime) fn string_search(
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
                let start = found.start as f64;
                JsValue::Number(start)
            }
            None => JsValue::Number(-1.0),
        })
    }

    /// `String.prototype.replace` with `$&`, `$1`–`$9`, `` $` ``, `$'`, `$$`
    /// expansion or a replacement function.
    pub(in crate::js::runtime) fn string_replace(
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

        let mut output = String::new();
        let mut cursor = 0usize;
        let mut last_end = 0usize;
        while let Some(found) = compiled.find(&characters, cursor) {
            let replaced = match &replacement {
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
                    let position = found.start as f64;
                    call_arguments.push(JsValue::Number(position));
                    call_arguments.push(JsValue::String(text.clone()));
                    let produced = self.call(dom, *callable, &call_arguments)?;
                    produced.to_js_string()
                }
                other => expand_replacement(&other.to_js_string(), &characters, &found),
            };
            output.push_str(&characters[last_end..found.start].iter().collect::<String>());
            output.push_str(&replaced);
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
        output.push_str(
            &characters[last_end.min(characters.len())..]
                .iter()
                .collect::<String>(),
        );
        Ok(JsValue::String(output))
    }
}
