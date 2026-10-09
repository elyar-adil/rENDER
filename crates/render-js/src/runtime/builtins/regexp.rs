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
use crate::runtime::convert::required_argument;
use crate::runtime::types::RegexRecord;
use crate::utf16;
use crate::value::NativeFunction;
use crate::value::ObjectHost;
use render_dom::Dom;

impl JsRuntime {
    pub(in crate::runtime) fn dispatch_regexp_native(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match function {
            NativeFunction::RegExpExec => self.regexp_exec(receiver, arguments),
            NativeFunction::RegExpTest => self.regexp_test(receiver, arguments),
            NativeFunction::RegExpToString => self.regexp_to_string(receiver),
            other => self.dispatch_object_native(dom, other, receiver, arguments),
        }
    }
}

impl JsRuntime {
    /// Compile `pattern` with `flags` and allocate a `RegExp` instance.
    pub(in crate::runtime) fn construct_regex(
        &mut self,
        pattern: &str,
        flags: &str,
    ) -> Result<ObjectId, JsError> {
        let compiled = crate::regex::compile(pattern, flags).map_err(|error| {
            JsError::syntax(
                format!("invalid regular expression /{pattern}/{flags}: {error}"),
                0,
            )
        })?;
        let flags_text = compiled.flags().describe();
        let global = compiled.flags().global;
        let ignore_case = compiled.flags().ignore_case;
        let multiline = compiled.flags().multiline;
        let dot_all = compiled.flags().dot_all;
        let sticky = compiled.flags().sticky;
        let index = self.regexes.len();
        self.regexes.push(RegexRecord {
            compiled,
            last_index: 0,
        });
        let object = self.realm.regexp_wrapper(index);
        for (name, value) in [
            ("source", JsValue::String(pattern.to_owned())),
            ("flags", JsValue::String(flags_text)),
            ("global", JsValue::Boolean(global)),
            ("ignoreCase", JsValue::Boolean(ignore_case)),
            ("multiline", JsValue::Boolean(multiline)),
            ("dotAll", JsValue::Boolean(dot_all)),
            ("sticky", JsValue::Boolean(sticky)),
            ("lastIndex", JsValue::Number(0.0)),
        ] {
            self.realm.set_property(object, name.to_owned(), value);
        }
        Ok(object)
    }

    pub(in crate::runtime) fn regex_index(&self, object: ObjectId) -> Result<usize, JsError> {
        match self.realm.host(object) {
            Some(ObjectHost::RegExp(index)) => Ok(index),
            _ => Err(JsError::type_error("incompatible RegExp method receiver")),
        }
    }

    /// Interpret a `String.prototype` regex-or-string argument. Returns the
    /// regex record index plus whether iteration must honor `g`.
    pub(in crate::runtime) fn coerce_pattern_argument(
        &mut self,
        value: &JsValue,
    ) -> Result<(usize, bool), JsError> {
        if let JsValue::Object(object) = value
            && let Some(ObjectHost::RegExp(index)) = self.realm.host(*object)
        {
            return Ok((index, self.regexes[index].compiled.flags().global));
        }
        let pattern = value.to_js_string();
        let object = self.construct_regex(&pattern, "")?;
        let Some(ObjectHost::RegExp(index)) = self.realm.host(object) else {
            return Err(JsError::type_error("regexp construction failed"));
        };
        Ok((index, false))
    }

    /// `RegExpBuiltinExec`'s result array for a match found in `input`, the code
    /// units of `text`. The matcher works in code units, so `index` is reported as
    /// matched: `'\u{1F600}a'.match(/a/).index` is 2.
    pub(in crate::runtime) fn regex_result(
        &mut self,
        input: &[u16],
        text: &str,
        found: &MatchRanges,
    ) -> Result<JsValue, JsError> {
        let mut values = Vec::with_capacity(found.groups.len() + 1);
        values.push(JsValue::String(utf16::string_from_utf16(
            &input[found.start..found.end],
        )));
        for group in &found.groups {
            values.push(match group {
                Some((start, end)) => {
                    JsValue::String(utf16::string_from_utf16(&input[*start..*end]))
                }
                None => JsValue::Undefined,
            });
        }
        let array = self.create_array_from_values(&values)?;
        self.realm.set_property(
            array,
            "index".to_owned(),
            JsValue::Number(found.start as f64),
        );
        self.realm
            .set_property(array, "input".to_owned(), JsValue::String(text.to_owned()));
        let groups = if found.names.is_empty() {
            JsValue::Undefined
        } else {
            self.named_groups_object(found, input)?
        };
        self.realm.set_property(array, "groups".to_owned(), groups);
        Ok(JsValue::Object(array))
    }

    /// The first match at or after `start` as a result array, or `None`.
    pub(in crate::runtime) fn regex_exec_value(
        &mut self,
        index: usize,
        input: &[u16],
        text: &str,
        start: usize,
    ) -> Result<Option<JsValue>, JsError> {
        match self.regexes[index].compiled.find(input, start) {
            Some(found) => self.regex_result(input, text, &found).map(Some),
            None => Ok(None),
        }
    }

    /// The `groups` object of a match: a prototype-less object mapping each
    /// group name to its capture, or `undefined` for a group that did not
    /// participate (ECMA-262 §22.2.7.2 `RegExpBuiltinExec`).
    pub(in crate::runtime) fn named_groups_object(
        &mut self,
        found: &MatchRanges,
        input: &[u16],
    ) -> Result<JsValue, JsError> {
        self.ensure_heap_capacity(1)?;
        let groups = self.realm.create_object(None);
        // Keys follow source order, and a name shared by several groups takes
        // the value of the one that participated (at most one can).
        let mut entries: Vec<(&String, JsValue)> = Vec::new();
        for (name, index) in found.names.iter() {
            let value = match found.groups.get(index - 1) {
                Some(Some((start, end))) => {
                    JsValue::String(utf16::string_from_utf16(&input[*start..*end]))
                }
                _ => JsValue::Undefined,
            };
            match entries.iter_mut().find(|(existing, _)| *existing == name) {
                Some((_, current)) => {
                    if matches!(current, JsValue::Undefined) {
                        *current = value;
                    }
                }
                None => entries.push((name, value)),
            }
        }
        for (name, value) in entries {
            self.realm.set_property(groups, name.clone(), value);
        }
        Ok(JsValue::Object(groups))
    }

    pub(in crate::runtime) fn regexp_exec(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let index = self.regex_index(receiver)?;
        let text = required_argument(arguments, 0, "exec")?.to_js_string();
        let input = utf16::utf16_units(&text);
        let track_last_index = {
            let flags = self.regexes[index].compiled.flags();
            flags.global || flags.sticky
        };
        // `lastIndex` is a code-unit index, which is how the matcher addresses
        // `input`. Past the end of the input there is no match.
        let from = if track_last_index {
            self.regexes[index].last_index
        } else {
            0
        };
        let found = if from > input.len() {
            None
        } else {
            self.regexes[index].compiled.find(&input, from)
        };
        let Some(found) = found else {
            if track_last_index {
                self.store_regex_last_index(receiver, index, 0);
            }
            return Ok(JsValue::Null);
        };
        let value = self.regex_result(&input, &text, &found)?;
        if track_last_index {
            self.store_regex_last_index(receiver, index, found.end);
        }
        Ok(value)
    }

    /// Record `last_index` (a code-unit index) as the regex's `lastIndex`, both
    /// in the record and in the property script reads back.
    fn store_regex_last_index(&mut self, receiver: ObjectId, index: usize, last_index: usize) {
        self.regexes[index].last_index = last_index;
        let stored = last_index as f64;
        self.realm
            .set_property(receiver, "lastIndex".to_owned(), JsValue::Number(stored));
    }

    pub(in crate::runtime) fn regexp_test(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let index = self.regex_index(receiver)?;
        let text = required_argument(arguments, 0, "test")?.to_js_string();
        let input = utf16::utf16_units(&text);
        let track_last_index = {
            let flags = self.regexes[index].compiled.flags();
            flags.global || flags.sticky
        };
        let from = if track_last_index {
            self.regexes[index].last_index
        } else {
            0
        };
        let found = if from > input.len() {
            None
        } else {
            self.regexes[index].compiled.find(&input, from)
        };
        if let Some(found) = found {
            if track_last_index {
                self.store_regex_last_index(receiver, index, found.end);
            }
            Ok(JsValue::Boolean(true))
        } else {
            if track_last_index {
                self.store_regex_last_index(receiver, index, 0);
            }
            Ok(JsValue::Boolean(false))
        }
    }

    pub(in crate::runtime) fn regexp_to_string(
        &mut self,
        receiver: ObjectId,
    ) -> Result<JsValue, JsError> {
        let index = self.regex_index(receiver)?;
        let source = self.regexes[index].compiled.source().to_owned();
        let flags = self.regexes[index].compiled.flags().describe();
        Ok(JsValue::String(format!("/{source}/{flags}")))
    }
}
