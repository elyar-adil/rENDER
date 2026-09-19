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

use render_dom::Dom;
use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::runtime::convert::required_argument;
use crate::runtime::types::RegexRecord;
use crate::value::NativeFunction;
use crate::value::ObjectHost;

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

    pub(in crate::runtime) fn regex_exec_value(
        &mut self,
        index: usize,
        input: &[char],
        start: usize,
    ) -> Result<Option<JsValue>, JsError> {
        let Some(found) = self.regexes[index].compiled.find(input, start) else {
            return Ok(None);
        };
        let mut values = Vec::with_capacity(found.groups.len() + 1);
        values.push(JsValue::String(
            input[found.start..found.end].iter().collect(),
        ));
        for group in &found.groups {
            values.push(match group {
                Some((start, end)) => JsValue::String(input[*start..*end].iter().collect()),
                None => JsValue::Undefined,
            });
        }
        let array = self.create_array_from_values(&values)?;
        #[allow(
            clippy::cast_precision_loss,
            reason = "string lengths stay far below any precision boundary"
        )]
        let index_number = found.start as f64;
        self.realm
            .set_property(array, "index".to_owned(), JsValue::Number(index_number));
        self.realm.set_property(
            array,
            "input".to_owned(),
            JsValue::String(input.iter().collect()),
        );
        Ok(Some(JsValue::Object(array)))
    }

    pub(in crate::runtime) fn regexp_exec(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let index = self.regex_index(receiver)?;
        let input: Vec<char> = required_argument(arguments, 0, "exec")?
            .to_js_string()
            .chars()
            .collect();
        let flags = self.regexes[index].compiled.flags();
        let track_last_index = flags.global || flags.sticky;
        let from = if track_last_index {
            self.regexes[index].last_index.min(input.len())
        } else {
            0
        };
        let found = self.regexes[index].compiled.find(&input, from);
        if let Some(value) = self.regex_exec_value(index, &input, from)? {
            if track_last_index && let Some(found) = found {
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "string lengths stay far below any precision boundary"
                )]
                let last = found.end as f64;
                self.regexes[index].last_index = found.end;
                self.realm
                    .set_property(receiver, "lastIndex".to_owned(), JsValue::Number(last));
            }
            Ok(value)
        } else {
            if track_last_index {
                self.regexes[index].last_index = 0;
                self.realm
                    .set_property(receiver, "lastIndex".to_owned(), JsValue::Number(0.0));
            }
            Ok(JsValue::Null)
        }
    }

    pub(in crate::runtime) fn regexp_test(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let index = self.regex_index(receiver)?;
        let input: Vec<char> = required_argument(arguments, 0, "test")?
            .to_js_string()
            .chars()
            .collect();
        let flags = self.regexes[index].compiled.flags();
        let track_last_index = flags.global || flags.sticky;
        let from = if track_last_index {
            self.regexes[index].last_index.min(input.len())
        } else {
            0
        };
        Ok(
            if let Some(found) = self.regexes[index].compiled.find(&input, from) {
                if track_last_index {
                    self.regexes[index].last_index = found.end;
                    #[allow(
                        clippy::cast_precision_loss,
                        reason = "string lengths stay far below any precision boundary"
                    )]
                    let last = found.end as f64;
                    self.realm.set_property(
                        receiver,
                        "lastIndex".to_owned(),
                        JsValue::Number(last),
                    );
                }
                JsValue::Boolean(true)
            } else {
                if track_last_index {
                    self.regexes[index].last_index = 0;
                    self.realm
                        .set_property(receiver, "lastIndex".to_owned(), JsValue::Number(0.0));
                }
                JsValue::Boolean(false)
            },
        )
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
