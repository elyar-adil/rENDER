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
use crate::value::ObjectHost;
use crate::value::{NativeFunction, RegExpAccessor};
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
            NativeFunction::RegExpAccessor(accessor) => {
                self.regexp_accessor(dom, receiver, accessor)
            }
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
        let index = self.regexes.len();
        self.regexes.push(RegexRecord {
            compiled,
            last_index: 0,
        });
        let object = self.realm.regexp_wrapper(index);
        // The flags and source are prototype accessors, not own properties; only
        // `lastIndex` is an own property of an instance.
        self.realm
            .define_hidden_data(object, "lastIndex", JsValue::Number(0.0));
        Ok(object)
    }

    /// The accessors ECMA-262 22.2.6 defines on %RegExp.prototype%. An instance
    /// reads its record. The prototype itself answers the source as `(?:)` and
    /// every flag as undefined, and `flags` reads each flag through `Get`.
    pub(in crate::runtime) fn regexp_accessor(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        accessor: RegExpAccessor,
    ) -> Result<JsValue, JsError> {
        // `flags` is generic (ECMA-262 22.2.6.4), so it reads every flag through
        // `Get` even for a real RegExp, and a patched flag getter shows in it.
        if accessor == RegExpAccessor::Flags {
            return self.regexp_flags_from_getters(dom, receiver);
        }
        if let Some(ObjectHost::RegExp(index)) = self.realm.host(receiver) {
            let flags = self.regexes[index].compiled.flags();
            let value = match accessor {
                RegExpAccessor::Source => {
                    return Ok(JsValue::String(
                        self.regexes[index].compiled.source().to_owned(),
                    ));
                }
                RegExpAccessor::Flags => unreachable!("handled above"),
                RegExpAccessor::Global => flags.global,
                RegExpAccessor::IgnoreCase => flags.ignore_case,
                RegExpAccessor::Multiline => flags.multiline,
                RegExpAccessor::DotAll => flags.dot_all,
                RegExpAccessor::Sticky => flags.sticky,
                // `v` reads the pattern with `u` semantics internally, but the
                // `unicode` getter reports only an explicit `u` (ECMA-262 22.2.6).
                RegExpAccessor::Unicode => flags.unicode && !flags.unicode_sets,
                RegExpAccessor::UnicodeSets => flags.unicode_sets,
                RegExpAccessor::HasIndices => flags.has_indices,
            };
            return Ok(JsValue::Boolean(value));
        }
        if receiver != self.realm.regexp_prototype() {
            return Err(JsError::type_error(
                "RegExp accessor called on an incompatible receiver",
            ));
        }
        Ok(match accessor {
            RegExpAccessor::Source => JsValue::String("(?:)".to_owned()),
            _ => JsValue::Undefined,
        })
    }

    /// ECMA-262 22.2.6.4 `RegExp.prototype.flags`: each flag's getter is read
    /// with `Get`, in the spec's order, so a subclass or a patched getter shows.
    fn regexp_flags_from_getters(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
    ) -> Result<JsValue, JsError> {
        let mut text = String::new();
        for (name, letter) in [
            ("hasIndices", 'd'),
            ("global", 'g'),
            ("ignoreCase", 'i'),
            ("multiline", 'm'),
            ("dotAll", 's'),
            ("unicode", 'u'),
            ("unicodeSets", 'v'),
            ("sticky", 'y'),
        ] {
            if self.get_member(dom, receiver, name)?.is_truthy() {
                text.push(letter);
            }
        }
        Ok(JsValue::String(text))
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
        has_indices: bool,
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
        if has_indices {
            let indices = self.match_indices(found)?;
            self.realm
                .set_property(array, "indices".to_owned(), JsValue::Object(indices));
        }
        Ok(JsValue::Object(array))
    }

    /// The `[start, end]` pair of a span, or `undefined` for a group that did
    /// not participate.
    fn index_pair(&mut self, span: Option<(usize, usize)>) -> Result<JsValue, JsError> {
        Ok(match span {
            Some((start, end)) => JsValue::Object(self.create_array_from_values(&[
                JsValue::Number(start as f64),
                JsValue::Number(end as f64),
            ])?),
            None => JsValue::Undefined,
        })
    }

    /// The `indices` array of a `d`-flag match: a pair for the whole match and
    /// for each group, plus a `groups` object for named groups (ECMA-262
    /// `MakeMatchIndicesIndexPairArray`). A repeated name takes the group that
    /// participated, as `groups` does.
    fn match_indices(&mut self, found: &MatchRanges) -> Result<ObjectId, JsError> {
        let mut values = Vec::with_capacity(found.groups.len() + 1);
        values.push(self.index_pair(Some((found.start, found.end)))?);
        for span in &found.groups {
            values.push(self.index_pair(*span)?);
        }
        let indices = self.create_array_from_values(&values)?;
        let groups = if found.names.is_empty() {
            JsValue::Undefined
        } else {
            self.ensure_heap_capacity(1)?;
            let mut entries: Vec<(&String, Option<(usize, usize)>)> = Vec::new();
            for (name, index) in found.names.iter() {
                let span = found.groups.get(index - 1).copied().flatten();
                match entries.iter_mut().find(|(existing, _)| *existing == name) {
                    Some((_, current)) => {
                        if current.is_none() {
                            *current = span;
                        }
                    }
                    None => entries.push((name, span)),
                }
            }
            let object = self.realm.create_object(None);
            for (name, span) in entries {
                let value = self.index_pair(span)?;
                self.realm.set_property(object, name.clone(), value);
            }
            JsValue::Object(object)
        };
        self.realm
            .set_property(indices, "groups".to_owned(), groups);
        Ok(indices)
    }

    /// The first match at or after `start` as a result array, or `None`.
    pub(in crate::runtime) fn regex_exec_value(
        &mut self,
        index: usize,
        input: &[u16],
        text: &str,
        start: usize,
    ) -> Result<Option<JsValue>, JsError> {
        let has_indices = self.regexes[index].compiled.flags().has_indices;
        match self.regexes[index].compiled.find(input, start) {
            Some(found) => self
                .regex_result(input, text, &found, has_indices)
                .map(Some),
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
        let has_indices = self.regexes[index].compiled.flags().has_indices;
        let value = self.regex_result(&input, &text, &found, has_indices)?;
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
