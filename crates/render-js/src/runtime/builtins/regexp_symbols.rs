//! The `RegExp` Symbol protocol: `RegExp.prototype[@@match]`, `[@@matchAll]`,
//! `[@@replace]`, `[@@search]` and `[@@split]` (ECMA-262 §22.2.6.8-12), the
//! `RegExpExec` and `RegExpBuiltinExec` operations they share (§22.2.7.1-2),
//! `GetSubstitution` (§22.1.3.19.1) and `%RegExpStringIteratorPrototype%`
//! (§22.2.9). The `String.prototype` methods reach these through
//! `GetMethod(regexp, @@symbol)`, so an object that is not a `RegExp` takes part
//! in the same protocol.
//!
//! The protocol is generic: `exec`, `lastIndex` and `flags` are read through
//! `Get` and written through `Set`, so a subclass or a patched method is seen.
//! A `RegExp` whose `exec` is the built-in one is matched without building the
//! result array the specification creates for each match: that array is not
//! observable from the built-in path, and building one per match would exhaust
//! the heap budget on a global replace over a large input.

use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::regex::MatchRanges;
use crate::runtime::JsRuntime;
use crate::utf16;
use crate::value::{JsSymbol, NativeFunction, ObjectHost, RegExpAccessor};
use render_dom::Dom;

/// What one `RegExpExec` produced.
pub(in crate::runtime) enum Exec {
    /// `null`: no match.
    None,
    /// The built-in matcher's span for a match, read without a result array.
    Native(MatchRanges),
    /// The object a user-defined `exec` returned.
    Object(ObjectId),
}

/// The fields of one match that `@@replace` reads from its result.
struct MatchParts {
    matched: Vec<u16>,
    position: usize,
    captures: Vec<JsValue>,
    groups: JsValue,
}

const DOLLAR: u16 = b'$' as u16;

/// `AdvanceStringIndex` (§22.2.7.3) over the code units `input`.
pub(in crate::runtime) fn advance_string_index(input: &[u16], index: f64, unicode: bool) -> f64 {
    if !unicode || index + 1.0 >= input.len() as f64 {
        return index + 1.0;
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the index is below the input length here"
    )]
    let position = index as usize;
    crate::regex::advance_index(input, position, true) as f64
}

/// The `g` and "full unicode" (`u` or `v`) reads of a flags string.
fn global_and_unicode(flags: &str) -> (bool, bool) {
    (
        flags.contains('g'),
        flags.contains('u') || flags.contains('v'),
    )
}

impl JsRuntime {
    /// Entry point for the five `RegExp.prototype[@@x]` natives. They take their
    /// receiver unconverted: `this` must be an Object (§22.2.6.8 step 2), so a
    /// primitive is a `TypeError` rather than a boxed receiver.
    pub(in crate::runtime) fn regexp_symbol_method(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: &JsValue,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let JsValue::Object(rx) = receiver else {
            return Err(JsError::type_error(
                "RegExp.prototype method called on a non-object receiver",
            ));
        };
        let rx = *rx;
        let argument = |index: usize| arguments.get(index).cloned().unwrap_or(JsValue::Undefined);
        match function {
            NativeFunction::RegExpSymbolMatch => {
                let text = self.to_string_argument(dom, &argument(0))?;
                self.symbol_match(dom, rx, &text)
            }
            NativeFunction::RegExpSymbolMatchAll => {
                let text = self.to_string_argument(dom, &argument(0))?;
                self.symbol_match_all(dom, rx, &text)
            }
            NativeFunction::RegExpSymbolReplace => {
                let text = self.to_string_argument(dom, &argument(0))?;
                self.symbol_replace(dom, rx, &text, &argument(1))
            }
            NativeFunction::RegExpSymbolSearch => {
                let text = self.to_string_argument(dom, &argument(0))?;
                self.symbol_search(dom, rx, &text)
            }
            NativeFunction::RegExpSymbolSplit => {
                let text = self.to_string_argument(dom, &argument(0))?;
                self.symbol_split(dom, rx, &text, &argument(1))
            }
            other => Err(JsError::type_error(format!(
                "{other:?} is not a RegExp symbol method"
            ))),
        }
    }

    /// `ToString(value)` for an argument the specification converts. A Symbol,
    /// or a Symbol wrapper, is a `TypeError` rather than a string.
    pub(in crate::runtime) fn to_string_argument(
        &mut self,
        dom: &mut Dom,
        value: &JsValue,
    ) -> Result<String, JsError> {
        let is_symbol = match value {
            JsValue::Symbol(_) => true,
            JsValue::Object(object) => {
                matches!(
                    self.realm.host(*object),
                    Some(ObjectHost::SymbolInstance(_))
                )
            }
            _ => false,
        };
        if is_symbol {
            return Err(JsError::type_error(
                "Cannot convert a Symbol value to a string",
            ));
        }
        self.to_string_value(dom, value)
    }

    /// `ToString(this)` for a String.prototype method whose receiver is the
    /// object `receiver`: a String, Number or Boolean wrapper is read directly,
    /// and any other object goes through `ToPrimitive`, which can throw.
    pub(in crate::runtime) fn this_string(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
    ) -> Result<String, JsError> {
        match self.realm.host(receiver) {
            Some(
                ObjectHost::StringPrimitive(_)
                | ObjectHost::NumberPrimitive(_)
                | ObjectHost::BooleanPrimitive(_),
            ) => self.require_string_receiver(receiver),
            _ => self.to_string_argument(dom, &JsValue::Object(receiver)),
        }
    }

    /// `GetMethod(value, @@name)` for a value that may be a RegExp-like object.
    /// A primitive has no such method on the built-in prototypes.
    pub(in crate::runtime) fn symbol_method_of(
        &mut self,
        dom: &mut Dom,
        value: &JsValue,
        name: &str,
    ) -> Result<Option<ObjectId>, JsError> {
        let JsValue::Object(object) = value else {
            return Ok(None);
        };
        let method = self.get_symbol_value(dom, *object, &JsSymbol::well_known(name))?;
        match method {
            JsValue::Undefined | JsValue::Null => Ok(None),
            JsValue::Object(callable) if Self::is_callable_object(callable, &self.realm) => {
                Ok(Some(callable))
            }
            _ => Err(JsError::type_error(format!("{name} is not a function"))),
        }
    }

    /// `Invoke(value, @@name, arguments)`.
    pub(in crate::runtime) fn invoke_symbol(
        &mut self,
        dom: &mut Dom,
        target: ObjectId,
        name: &str,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match self.symbol_method_of(dom, &JsValue::Object(target), name)? {
            Some(method) => self.call_with_this(dom, method, arguments, JsValue::Object(target)),
            None => Err(JsError::type_error(format!("{name} is not a function"))),
        }
    }

    /// `RegExpCreate(P, F)` (§22.2.3.1): `P` undefined is the empty pattern,
    /// anything else is converted with `ToString`.
    pub(in crate::runtime) fn regexp_create(
        &mut self,
        dom: &mut Dom,
        pattern: &JsValue,
        flags: &str,
    ) -> Result<ObjectId, JsError> {
        let source = match pattern {
            JsValue::Undefined => String::new(),
            value => self.to_string_value(dom, value)?,
        };
        self.construct_regex(&source, flags)
    }

    /// `IsRegExp` (§7.2.8).
    pub(in crate::runtime) fn is_regexp(
        &mut self,
        dom: &mut Dom,
        value: &JsValue,
    ) -> Result<bool, JsError> {
        let JsValue::Object(object) = value else {
            return Ok(false);
        };
        let matcher = self.get_symbol_value(dom, *object, &JsSymbol::well_known("@@match"))?;
        if !matches!(matcher, JsValue::Undefined) {
            return Ok(matcher.is_truthy());
        }
        Ok(matches!(
            self.realm.host(*object),
            Some(ObjectHost::RegExp(_))
        ))
    }

    /// `SpeciesConstructor(O, %RegExp%)` (§7.3.22).
    pub(in crate::runtime) fn regexp_species_constructor(
        &mut self,
        dom: &mut Dom,
        rx: ObjectId,
    ) -> Result<ObjectId, JsError> {
        let default = match self.realm.global("RegExp") {
            Some(JsValue::Object(constructor)) => constructor,
            _ => return Err(JsError::type_error("RegExp constructor is missing")),
        };
        let constructor = self.get_member(dom, rx, "constructor")?;
        let constructor = match constructor {
            JsValue::Undefined => return Ok(default),
            JsValue::Object(object) => object,
            _ => return Err(JsError::type_error("RegExp constructor is not an object")),
        };
        let species =
            self.get_symbol_value(dom, constructor, &JsSymbol::well_known("@@species"))?;
        match species {
            JsValue::Undefined | JsValue::Null => Ok(default),
            JsValue::Object(species) if self.is_constructor(species) => Ok(species),
            _ => Err(JsError::type_error("RegExp species is not a constructor")),
        }
    }

    /// `Set(O, P, V, true)`: a failed write is a `TypeError`.
    pub(in crate::runtime) fn set_strict(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        key: &str,
        value: JsValue,
    ) -> Result<(), JsError> {
        if let Some(own) = self.realm.own_property(object, key)
            && !own.is_accessor()
        {
            if !own.writable {
                return Err(JsError::type_error(format!(
                    "Cannot assign to read only property '{key}'"
                )));
            }
            // An own writable data property of a RegExp is written in place:
            // the host-specific `[[Set]]` arms do not apply to it.
            self.realm.set_property(object, key.to_owned(), value);
            return Ok(());
        }
        self.set_member(dom, object, key, value)
    }

    /// `RegExpBuiltinExec`'s matching half (§22.2.7.2 steps 4-14): reads and
    /// writes `lastIndex` with `Get`/`Set`, and returns the span of the match.
    pub(in crate::runtime) fn regexp_builtin_match(
        &mut self,
        dom: &mut Dom,
        rx: ObjectId,
        index: usize,
        input: &[u16],
    ) -> Result<Option<MatchRanges>, JsError> {
        let flags = self.regexes[index].compiled.flags();
        let tracks = flags.global || flags.sticky;
        let stored = self.get_member(dom, rx, "lastIndex")?;
        let last_index = if tracks {
            self.to_length_value(dom, &stored)?
        } else {
            self.to_length_value(dom, &stored)?;
            0.0
        };
        if last_index > input.len() as f64 {
            if tracks {
                self.set_strict(dom, rx, "lastIndex", JsValue::Number(0.0))?;
            }
            return Ok(None);
        }
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the index was checked against the input length above"
        )]
        let from = last_index as usize;
        let Some(found) = self.regexes[index].compiled.find(input, from) else {
            if tracks {
                self.set_strict(dom, rx, "lastIndex", JsValue::Number(0.0))?;
            }
            return Ok(None);
        };
        if tracks {
            #[allow(
                clippy::cast_precision_loss,
                reason = "string lengths stay far below any precision boundary"
            )]
            let end = found.end as f64;
            self.set_strict(dom, rx, "lastIndex", JsValue::Number(end))?;
        }
        Ok(Some(found))
    }

    /// `RegExpBuiltinExec` (§22.2.7.2): the match, as the `exec` result array.
    pub(in crate::runtime) fn regexp_builtin_exec(
        &mut self,
        dom: &mut Dom,
        rx: ObjectId,
        text: &str,
    ) -> Result<JsValue, JsError> {
        let index = self.regex_index(rx)?;
        let input = utf16::utf16_units(text);
        match self.regexp_builtin_match(dom, rx, index, &input)? {
            Some(found) => {
                let has_indices = self.regexes[index].compiled.flags().has_indices;
                self.regex_result(&input, text, &found, has_indices)
            }
            None => Ok(JsValue::Null),
        }
    }

    /// `RegExpExec(R, S)` (§22.2.7.1). `input` is `text` in code units. A
    /// callable `exec` is called with `Call`; the built-in one is matched
    /// directly, which is the same operation without the array.
    pub(in crate::runtime) fn regexp_exec_step(
        &mut self,
        dom: &mut Dom,
        rx: ObjectId,
        text: &str,
        input: &[u16],
    ) -> Result<Exec, JsError> {
        let exec = self.get_member(dom, rx, "exec")?;
        if let JsValue::Object(method) = exec
            && Self::is_callable_object(method, &self.realm)
        {
            if matches!(
                self.realm.host(method),
                Some(ObjectHost::NativeFunction(NativeFunction::RegExpExec))
            ) {
                let index = self.regex_index(rx)?;
                return Ok(match self.regexp_builtin_match(dom, rx, index, input)? {
                    Some(found) => Exec::Native(found),
                    None => Exec::None,
                });
            }
            let result = self.call_with_this(
                dom,
                method,
                &[JsValue::String(text.to_owned())],
                JsValue::Object(rx),
            )?;
            return match result {
                JsValue::Null => Ok(Exec::None),
                JsValue::Object(object) => {
                    self.transient_roots.push(object);
                    Ok(Exec::Object(object))
                }
                _ => Err(JsError::type_error(
                    "RegExp exec method returned something other than an Object or null",
                )),
            };
        }
        let index = self.regex_index(rx)?;
        Ok(match self.regexp_builtin_match(dom, rx, index, input)? {
            Some(found) => Exec::Native(found),
            None => Exec::None,
        })
    }

    /// The `exec` result array for a span the built-in matcher found.
    pub(in crate::runtime) fn exec_result_value(
        &mut self,
        rx: ObjectId,
        input: &[u16],
        text: &str,
        found: &MatchRanges,
    ) -> Result<JsValue, JsError> {
        let index = self.regex_index(rx)?;
        let has_indices = self.regexes[index].compiled.flags().has_indices;
        self.regex_result(input, text, found, has_indices)
    }

    /// `RegExp.prototype[@@match]` (§22.2.6.8).
    fn symbol_match(
        &mut self,
        dom: &mut Dom,
        rx: ObjectId,
        text: &str,
    ) -> Result<JsValue, JsError> {
        let flags = self.regexp_flags(dom, rx)?;
        let (global, full_unicode) = global_and_unicode(&flags);
        if !global {
            let input = utf16::utf16_units(text);
            return match self.regexp_exec_step(dom, rx, text, &input)? {
                Exec::None => Ok(JsValue::Null),
                Exec::Native(found) => self.exec_result_value(rx, &input, text, &found),
                Exec::Object(object) => Ok(JsValue::Object(object)),
            };
        }
        self.set_strict(dom, rx, "lastIndex", JsValue::Number(0.0))?;
        let input = utf16::utf16_units(text);
        let mut collected = Vec::new();
        loop {
            match self.regexp_exec_step(dom, rx, text, &input)? {
                Exec::None => break,
                Exec::Native(found) => {
                    let empty = found.start == found.end;
                    collected.push(JsValue::String(utf16::string_from_utf16(
                        &input[found.start..found.end],
                    )));
                    if empty {
                        self.advance_last_index(dom, rx, &input, full_unicode, found.end as f64)?;
                    }
                }
                Exec::Object(result) => {
                    let matched_value = self.get_member(dom, result, "0")?;
                    let matched = self.to_string_value(dom, &matched_value)?;
                    if matched.is_empty() {
                        let stored = self.get_member(dom, rx, "lastIndex")?;
                        let this_index = self.to_length_value(dom, &stored)?;
                        self.advance_last_index(dom, rx, &input, full_unicode, this_index)?;
                    }
                    collected.push(JsValue::String(matched));
                }
            }
        }
        if collected.is_empty() {
            return Ok(JsValue::Null);
        }
        Ok(JsValue::Object(self.create_array_from_values(&collected)?))
    }

    /// The `lastIndex` step an empty match takes so the loop makes progress.
    fn advance_last_index(
        &mut self,
        dom: &mut Dom,
        rx: ObjectId,
        input: &[u16],
        full_unicode: bool,
        this_index: f64,
    ) -> Result<(), JsError> {
        let next = advance_string_index(input, this_index, full_unicode);
        self.set_strict(dom, rx, "lastIndex", JsValue::Number(next))
    }

    /// `RegExp.prototype[@@matchAll]` (§22.2.6.9).
    fn symbol_match_all(
        &mut self,
        dom: &mut Dom,
        rx: ObjectId,
        text: &str,
    ) -> Result<JsValue, JsError> {
        let constructor = self.regexp_species_constructor(dom, rx)?;
        let flags = self.regexp_flags(dom, rx)?;
        let matcher = self.construct_dispatch(
            dom,
            constructor,
            &[JsValue::Object(rx), JsValue::String(flags.clone())],
        )?;
        let JsValue::Object(matcher) = matcher else {
            return Err(JsError::type_error(
                "RegExp species did not construct an object",
            ));
        };
        let stored = self.get_member(dom, rx, "lastIndex")?;
        let last_index = self.to_length_value(dom, &stored)?;
        self.set_strict(dom, matcher, "lastIndex", JsValue::Number(last_index))?;
        let (global, full_unicode) = global_and_unicode(&flags);
        let iterator =
            self.realm
                .regexp_string_iterator(matcher, text.to_owned(), global, full_unicode);
        Ok(JsValue::Object(iterator))
    }

    /// `%RegExpStringIteratorPrototype%.next()` (§22.2.9.2.1).
    pub(in crate::runtime) fn regexp_string_iterator_next(
        &mut self,
        dom: &mut Dom,
        iterator: ObjectId,
    ) -> Result<JsValue, JsError> {
        let Some(ObjectHost::RegExpStringIterator {
            matcher,
            input,
            global,
            unicode,
            done,
        }) = self.realm.host(iterator)
        else {
            return Err(JsError::type_error(
                "RegExp String Iterator next called on an incompatible receiver",
            ));
        };
        if done {
            return self.iterator_step_result(JsValue::Undefined, true);
        }
        let units = utf16::utf16_units(&input);
        let outcome = self.regexp_exec_step(dom, matcher, &input, &units)?;
        let value = match outcome {
            Exec::None => {
                self.set_iterator_done(iterator);
                return self.iterator_step_result(JsValue::Undefined, true);
            }
            Exec::Native(found) => {
                if !global {
                    self.set_iterator_done(iterator);
                } else if found.start == found.end {
                    self.advance_last_index(dom, matcher, &units, unicode, found.end as f64)?;
                }
                self.exec_result_value(matcher, &units, &input, &found)?
            }
            Exec::Object(result) => {
                if !global {
                    self.set_iterator_done(iterator);
                } else {
                    let matched_value = self.get_member(dom, result, "0")?;
                    let matched = self.to_string_value(dom, &matched_value)?;
                    if matched.is_empty() {
                        let stored = self.get_member(dom, matcher, "lastIndex")?;
                        let this_index = self.to_length_value(dom, &stored)?;
                        self.advance_last_index(dom, matcher, &units, unicode, this_index)?;
                    }
                }
                JsValue::Object(result)
            }
        };
        self.iterator_step_result(value, false)
    }

    fn set_iterator_done(&mut self, iterator: ObjectId) {
        if let Some(ObjectHost::RegExpStringIterator { done, .. }) = self.realm.host_mut(iterator) {
            *done = true;
        }
    }

    /// A `{ value, done }` iterator result.
    fn iterator_step_result(&mut self, value: JsValue, done: bool) -> Result<JsValue, JsError> {
        self.ensure_heap_capacity(1)?;
        let result = self.realm.create_ordinary_object();
        self.realm.set_property(result, "value".to_owned(), value);
        self.realm
            .set_property(result, "done".to_owned(), JsValue::Boolean(done));
        Ok(JsValue::Object(result))
    }

    /// `RegExp.prototype[@@replace]` (§22.2.6.10).
    fn symbol_replace(
        &mut self,
        dom: &mut Dom,
        rx: ObjectId,
        text: &str,
        replace_value: &JsValue,
    ) -> Result<JsValue, JsError> {
        let input = utf16::utf16_units(text);
        let replacer = match replace_value {
            JsValue::Object(callable) if Self::is_callable_object(*callable, &self.realm) => {
                Some(*callable)
            }
            _ => None,
        };
        let template = if replacer.is_some() {
            Vec::new()
        } else {
            let template = self.to_string_argument(dom, replace_value)?;
            utf16::utf16_units(&template)
        };
        let flags = self.regexp_flags(dom, rx)?;
        let (global, full_unicode) = global_and_unicode(&flags);
        if global {
            self.set_strict(dom, rx, "lastIndex", JsValue::Number(0.0))?;
        }
        let mut results = Vec::new();
        loop {
            match self.regexp_exec_step(dom, rx, text, &input)? {
                Exec::None => break,
                Exec::Native(found) => {
                    let empty = found.start == found.end;
                    results.push(Exec::Native(found));
                    if !global {
                        break;
                    }
                    if empty {
                        let end = match results.last() {
                            Some(Exec::Native(found)) => found.end as f64,
                            _ => 0.0,
                        };
                        self.advance_last_index(dom, rx, &input, full_unicode, end)?;
                    }
                }
                Exec::Object(result) => {
                    results.push(Exec::Object(result));
                    if !global {
                        break;
                    }
                    let matched_value = self.get_member(dom, result, "0")?;
                    let matched = self.to_string_value(dom, &matched_value)?;
                    if matched.is_empty() {
                        let stored = self.get_member(dom, rx, "lastIndex")?;
                        let this_index = self.to_length_value(dom, &stored)?;
                        self.advance_last_index(dom, rx, &input, full_unicode, this_index)?;
                    }
                }
            }
        }
        let mut accumulated: Vec<u16> = Vec::new();
        let mut next_source = 0usize;
        for result in results {
            let parts = match result {
                Exec::Native(found) => {
                    self.native_match_parts(&found, &input, replacer, &template)?
                }
                Exec::Object(object) => self.object_match_parts(dom, object, input.len())?,
                Exec::None => continue,
            };
            let replacement = if let Some(callable) = replacer {
                let mut arguments = Vec::with_capacity(parts.captures.len() + 4);
                arguments.push(JsValue::String(utf16::string_from_utf16(&parts.matched)));
                arguments.extend(parts.captures.iter().cloned());
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "string positions stay far below any precision boundary"
                )]
                arguments.push(JsValue::Number(parts.position as f64));
                arguments.push(JsValue::String(text.to_owned()));
                if !matches!(parts.groups, JsValue::Undefined) {
                    arguments.push(parts.groups.clone());
                }
                let produced = self.call(dom, callable, &arguments)?;
                let produced = self.to_string_value(dom, &produced)?;
                utf16::utf16_units(&produced)
            } else {
                let named = match &parts.groups {
                    JsValue::Undefined => JsValue::Undefined,
                    groups => {
                        let object = self.to_object(groups)?;
                        self.transient_roots.push(object);
                        JsValue::Object(object)
                    }
                };
                self.get_substitution(
                    dom,
                    &parts.matched,
                    &input,
                    parts.position,
                    &parts.captures,
                    &named,
                    &template,
                )?
            };
            if parts.position >= next_source {
                accumulated.extend_from_slice(&input[next_source..parts.position]);
                accumulated.extend(replacement);
                next_source = parts.position + parts.matched.len();
            }
        }
        if next_source < input.len() {
            accumulated.extend_from_slice(&input[next_source..]);
        }
        Ok(JsValue::String(utf16::string_from_utf16(&accumulated)))
    }

    /// The `@@replace` fields of a span the built-in matcher found.
    fn native_match_parts(
        &mut self,
        found: &MatchRanges,
        input: &[u16],
        replacer: Option<ObjectId>,
        template: &[u16],
    ) -> Result<MatchParts, JsError> {
        let captures = found
            .groups
            .iter()
            .map(|span| match span {
                Some((start, end)) => {
                    JsValue::String(utf16::string_from_utf16(&input[*start..*end]))
                }
                None => JsValue::Undefined,
            })
            .collect();
        let needs_groups = !found.names.is_empty()
            && (replacer.is_some()
                || template
                    .windows(2)
                    .any(|pair| pair == [DOLLAR, u16::from(b'<')]));
        let groups = if needs_groups {
            let groups = self.named_groups_object(found, input)?;
            if let JsValue::Object(object) = &groups {
                self.transient_roots.push(*object);
            }
            groups
        } else {
            JsValue::Undefined
        };
        Ok(MatchParts {
            matched: input[found.start..found.end].to_vec(),
            position: found.start,
            captures,
            groups,
        })
    }

    /// The `@@replace` fields of a user-defined `exec` result (§22.2.6.10 step
    /// 14), read in the specification's order.
    fn object_match_parts(
        &mut self,
        dom: &mut Dom,
        result: ObjectId,
        input_length: usize,
    ) -> Result<MatchParts, JsError> {
        let length_value = self.get_member(dom, result, "length")?;
        let length = self.to_length_value(dom, &length_value)?;
        let captures_count = if length >= 1.0 { length - 1.0 } else { 0.0 };
        let matched_value = self.get_member(dom, result, "0")?;
        let matched = self.to_string_value(dom, &matched_value)?;
        let index_value = self.get_member(dom, result, "index")?;
        let index = self.to_integer_value(dom, &index_value)?;
        let position = index.max(0.0).min(input_length as f64);
        let mut captures = Vec::new();
        let mut n = 1.0;
        while n <= captures_count {
            let capture = self.get_member(dom, result, &format_index(n))?;
            captures.push(match capture {
                JsValue::Undefined => JsValue::Undefined,
                value => JsValue::String(self.to_string_value(dom, &value)?),
            });
            n += 1.0;
        }
        let groups = self.get_member(dom, result, "groups")?;
        Ok(MatchParts {
            matched: utf16::utf16_units(&matched),
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "the position is clamped to the input length above"
            )]
            position: position as usize,
            captures,
            groups,
        })
    }

    /// `GetSubstitution` (§22.1.3.19.1). The template and the result are code
    /// units; `captures` holds `undefined` or a String for each group.
    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors the specification's operation"
    )]
    pub(in crate::runtime) fn get_substitution(
        &mut self,
        dom: &mut Dom,
        matched: &[u16],
        input: &[u16],
        position: usize,
        captures: &[JsValue],
        named: &JsValue,
        template: &[u16],
    ) -> Result<Vec<u16>, JsError> {
        let mut output = Vec::with_capacity(template.len());
        let capture_count = captures.len();
        let tail = (position + matched.len()).min(input.len());
        let mut index = 0usize;
        while index < template.len() {
            let character = template[index];
            if character != DOLLAR || index + 1 >= template.len() {
                output.push(character);
                index += 1;
                continue;
            }
            let next = template[index + 1];
            match next {
                0x24 => {
                    output.push(DOLLAR);
                    index += 2;
                }
                0x26 => {
                    output.extend_from_slice(matched);
                    index += 2;
                }
                0x60 => {
                    output.extend_from_slice(&input[..position.min(input.len())]);
                    index += 2;
                }
                0x27 => {
                    output.extend_from_slice(&input[tail..]);
                    index += 2;
                }
                0x30..=0x39 => {
                    let first = usize::from(next - 0x30);
                    let mut digit_count = 1usize;
                    let mut reference = first;
                    if let Some(&second) = template.get(index + 2)
                        && (0x30..=0x39).contains(&second)
                    {
                        digit_count = 2;
                        reference = first * 10 + usize::from(second - 0x30);
                    }
                    if reference > capture_count && digit_count == 2 {
                        digit_count = 1;
                        reference = first;
                    }
                    let length = 1 + digit_count;
                    if (1..=capture_count).contains(&reference) {
                        if let JsValue::String(capture) = &captures[reference - 1] {
                            output.extend(utf16::utf16_units(capture));
                        }
                    } else {
                        let end = (index + length).min(template.len());
                        output.extend_from_slice(&template[index..end]);
                    }
                    index += length;
                }
                0x3C => {
                    let close = template[index + 2..]
                        .iter()
                        .position(|unit| *unit == u16::from(b'>'));
                    if let (JsValue::Object(object), Some(close)) = (named, close) {
                        let name =
                            utf16::string_from_utf16(&template[index + 2..index + 2 + close]);
                        let capture = self.get_member(dom, *object, &name)?;
                        if !matches!(capture, JsValue::Undefined) {
                            let text = self.to_string_value(dom, &capture)?;
                            output.extend(utf16::utf16_units(&text));
                        }
                        index += close + 3;
                    } else {
                        output.push(DOLLAR);
                        output.push(u16::from(b'<'));
                        index += 2;
                    }
                }
                _ => {
                    output.push(DOLLAR);
                    index += 1;
                }
            }
        }
        Ok(output)
    }

    /// `ToString(Get(rx, "flags"))`. The built-in `flags` getter is read
    /// directly while it is intact, since it would only call the built-in flag
    /// getters.
    pub(in crate::runtime) fn regexp_flags(
        &mut self,
        dom: &mut Dom,
        rx: ObjectId,
    ) -> Result<String, JsError> {
        if self.is_intrinsic_regexp_getter(rx, "flags", RegExpAccessor::Flags)
            && let JsValue::String(text) = self.regexp_flags_from_getters(dom, rx)?
        {
            return Ok(text);
        }
        let value = self.get_member(dom, rx, "flags")?;
        self.to_string_value(dom, &value)
    }

    /// `RegExp.prototype[@@search]` (§22.2.6.11).
    fn symbol_search(
        &mut self,
        dom: &mut Dom,
        rx: ObjectId,
        text: &str,
    ) -> Result<JsValue, JsError> {
        let previous = self.get_member(dom, rx, "lastIndex")?;
        if !is_positive_zero(&previous) {
            self.set_strict(dom, rx, "lastIndex", JsValue::Number(0.0))?;
        }
        let input = utf16::utf16_units(text);
        let outcome = self.regexp_exec_step(dom, rx, text, &input)?;
        let current = self.get_member(dom, rx, "lastIndex")?;
        if !same_value(&current, &previous) {
            self.set_strict(dom, rx, "lastIndex", previous)?;
        }
        match outcome {
            Exec::None => Ok(JsValue::Number(-1.0)),
            Exec::Native(found) => {
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "string positions stay far below any precision boundary"
                )]
                let start = found.start as f64;
                Ok(JsValue::Number(start))
            }
            Exec::Object(result) => self.get_member(dom, result, "index"),
        }
    }

    /// `RegExp.prototype[@@split]` (§22.2.6.12).
    fn symbol_split(
        &mut self,
        dom: &mut Dom,
        rx: ObjectId,
        text: &str,
        limit: &JsValue,
    ) -> Result<JsValue, JsError> {
        let constructor = self.regexp_species_constructor(dom, rx)?;
        let flags = self.regexp_flags(dom, rx)?;
        let unicode_matching = flags.contains('u') || flags.contains('v');
        let new_flags = if flags.contains('y') {
            flags.clone()
        } else {
            format!("{flags}y")
        };
        let splitter = self.construct_dispatch(
            dom,
            constructor,
            &[JsValue::Object(rx), JsValue::String(new_flags)],
        )?;
        let JsValue::Object(splitter) = splitter else {
            return Err(JsError::type_error(
                "RegExp species did not construct an object",
            ));
        };
        let limit = match limit {
            JsValue::Undefined => u32::MAX,
            value => crate::runtime::convert::to_uint32(value)?,
        };
        let input = utf16::utf16_units(text);
        let mut pieces: Vec<JsValue> = Vec::new();
        if limit == 0 {
            return Ok(JsValue::Object(self.create_array_from_values(&pieces)?));
        }
        let size = input.len();
        if size == 0 {
            return Ok(match self.regexp_exec_step(dom, splitter, text, &input)? {
                Exec::None => JsValue::Object(
                    self.create_array_from_values(&[JsValue::String(text.to_owned())])?,
                ),
                _ => JsValue::Object(self.create_array_from_values(&pieces)?),
            });
        }
        let limit = limit as usize;
        let mut p = 0usize;
        let mut q = 0usize;
        while q < size {
            self.set_strict(dom, splitter, "lastIndex", JsValue::Number(q as f64))?;
            match self.regexp_exec_step(dom, splitter, text, &input)? {
                Exec::None => {
                    q = advance_string_index(&input, q as f64, unicode_matching) as usize;
                }
                outcome => {
                    let (end, captures) = match outcome {
                        Exec::Native(found) => {
                            let captures: Vec<JsValue> = found
                                .groups
                                .iter()
                                .map(|span| match span {
                                    Some((start, end)) => JsValue::String(
                                        utf16::string_from_utf16(&input[*start..*end]),
                                    ),
                                    None => JsValue::Undefined,
                                })
                                .collect();
                            (found.end, captures)
                        }
                        Exec::Object(result) => {
                            let stored = self.get_member(dom, splitter, "lastIndex")?;
                            let end = self.to_length_value(dom, &stored)? as usize;
                            let length_value = self.get_member(dom, result, "length")?;
                            let length = self.to_length_value(dom, &length_value)?;
                            let mut captures = Vec::new();
                            let mut n = 1.0;
                            while n < length {
                                let capture = self.get_member(dom, result, &format_index(n))?;
                                captures.push(capture);
                                n += 1.0;
                            }
                            (end, captures)
                        }
                        Exec::None => unreachable!("handled above"),
                    };
                    let e = end.min(size);
                    if e == p {
                        q = advance_string_index(&input, q as f64, unicode_matching) as usize;
                        continue;
                    }
                    pieces.push(JsValue::String(utf16::string_from_utf16(&input[p..q])));
                    if pieces.len() == limit {
                        return Ok(JsValue::Object(self.create_array_from_values(&pieces)?));
                    }
                    p = e;
                    for capture in captures {
                        pieces.push(capture);
                        if pieces.len() == limit {
                            return Ok(JsValue::Object(self.create_array_from_values(&pieces)?));
                        }
                    }
                    q = p;
                }
            }
        }
        pieces.push(JsValue::String(utf16::string_from_utf16(&input[p..size])));
        Ok(JsValue::Object(self.create_array_from_values(&pieces)?))
    }
}

/// The decimal form of a capture index, as the property key `Get` reads.
fn format_index(index: f64) -> String {
    format!("{index}")
}

/// `SameValue(value, +0)`.
fn is_positive_zero(value: &JsValue) -> bool {
    matches!(value, JsValue::Number(number) if number.to_bits() == 0.0_f64.to_bits())
}

/// `SameValue` for the values `lastIndex` can hold.
fn same_value(left: &JsValue, right: &JsValue) -> bool {
    match (left, right) {
        (JsValue::Number(a), JsValue::Number(b)) => {
            (a.is_nan() && b.is_nan()) || a.to_bits() == b.to_bits()
        }
        _ => left == right,
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

    #[test]
    fn string_methods_delegate_to_the_regexp_symbol_methods() {
        // §22.1.3.19: `replace` calls the search value's `@@replace` with the
        // receiver and the replace value.
        assert_eq!(
            run(
                "var log = []; var o = {}; o[Symbol.replace] = function (s, r) { log.push(s, r); return 'R'; }; 'abc'.replace(o, 'x') + log.join()"
            ),
            "Rabc,x"
        );
        assert_eq!(
            run("'abc'.replace(/(?<n>b)/, '[$<n>$&$`$\\']')"),
            "a[bbac]c"
        );
        assert_eq!(
            run("'a.b.c'.replace('.', '!') + ' ' + 'a.b.c'.replaceAll('.', '!')"),
            "a!b.c a!b!c"
        );
        assert_eq!(
            run("'a,b;c'.split(/[,;]/).join('|') + ' ' + 'aXbX'.split('X', 1).join('|')"),
            "a|b|c a"
        );
        assert_eq!(
            run("'x1y22'.search(/\\d\\d/) + ' ' + 'x'.search(/q/)"),
            "3 -1"
        );
        assert_eq!(
            run(
                "[...'a1b22'.matchAll(/\\d+/g)].map(function (m) { return m[0] + '@' + m.index; }).join('|')"
            ),
            "1@1|22@3"
        );
    }

    #[test]
    fn regexp_symbol_methods_are_named_and_sized_by_the_specification() {
        assert_eq!(
            run(
                "RegExp.prototype[Symbol.replace].name + ' ' + RegExp.prototype[Symbol.replace].length + ' ' + RegExp.prototype[Symbol.split].length"
            ),
            "[Symbol.replace] 2 2"
        );
        assert_eq!(
            run("typeof Object.getOwnPropertyDescriptor(RegExp, Symbol.species).get"),
            "function"
        );
    }

    #[test]
    fn symbol_methods_reject_a_primitive_receiver() {
        assert_eq!(
            run(
                "var out = 'no throw'; try { RegExp.prototype[Symbol.match].call(1, 'a'); } catch (e) { out = e.name; } out"
            ),
            "TypeError"
        );
    }

    #[test]
    fn last_index_is_an_ordinary_property_read_through_to_length() {
        // §22.2.7.2 step 4: `lastIndex` is `ToLength(Get(R, "lastIndex"))`, so a
        // string index is honored, and the value read back is the stored one.
        assert_eq!(
            run("var r = /a/g; r.lastIndex = '2'; r.exec('aaa').index + ':' + r.lastIndex"),
            "2:3"
        );
        assert_eq!(run("var r = /a/g; r.lastIndex = 'x'; r.lastIndex"), "x");
        // A failed global match writes `0` with `Set(..., true)`, so a read-only
        // `lastIndex` is a TypeError.
        assert_eq!(
            run(
                "var r = /a/g; Object.defineProperty(r, 'lastIndex', { value: 5, writable: false }); var out = 'no throw'; try { r.exec('b'); } catch (e) { out = e.name; } out"
            ),
            "TypeError"
        );
    }

    #[test]
    fn user_exec_results_drive_the_protocol() {
        // §22.2.7.1 RegExpExec calls a user `exec`, and `@@replace` reads the
        // result's `index`, `length` and captures.
        assert_eq!(
            run("var r = /x/g; r.exec = function () { return null; }; 'xxx'.replace(r, 'y')"),
            "xxx"
        );
        assert_eq!(
            run(
                "var r = /x/; r.exec = function () { return { 0: 'hi', 1: 'h', length: 2, index: 1, groups: undefined }; }; 'abcd'.replace(r, '[$1$&]')"
            ),
            "a[hhi]d"
        );
    }

    #[test]
    fn regexp_escape_encodes_each_code_point_for_a_literal_match() {
        assert_eq!(run("RegExp.escape('.a b')"), "\\.a\\x20b");
        assert_eq!(run("RegExp.escape('1-x')"), "\\x31\\x2dx");
        assert_eq!(run("RegExp.escape('\\n\\t')"), "\\n\\t");
        assert_eq!(run("RegExp.escape('\\uD800')"), "\\ud800");
        assert_eq!(run("RegExp.escape('_')"), "_");
        assert_eq!(
            run("var out = 'no throw'; try { RegExp.escape(1); } catch (e) { out = e.name; } out"),
            "TypeError"
        );
    }

    #[test]
    fn the_source_getter_escapes_slashes_and_line_terminators() {
        assert_eq!(run("new RegExp('').source"), "(?:)");
        assert_eq!(run("new RegExp('/').source"), "\\/");
        assert_eq!(run("new RegExp('a\\nb').source"), "a\\nb");
        assert_eq!(run("RegExp('[/]').toString()"), "/[/]/");
    }

    #[test]
    fn from_code_point_accepts_a_surrogate_as_one_code_unit() {
        assert_eq!(run("String.fromCodePoint(0xD800).length"), "1");
    }
}
