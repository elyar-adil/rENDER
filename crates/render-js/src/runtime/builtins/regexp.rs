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

use std::fmt::Write as _;

use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::regex::MatchRanges;
use crate::runtime::JsRuntime;
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
            NativeFunction::RegExpExec => self.regexp_exec(dom, receiver, arguments),
            NativeFunction::RegExpTest => self.regexp_test(dom, receiver, arguments),
            NativeFunction::RegExpToString => self.regexp_to_string(receiver),
            NativeFunction::RegExpAccessor(accessor) => {
                self.regexp_accessor(dom, receiver, accessor)
            }
            NativeFunction::RegExpSymbolMatch
            | NativeFunction::RegExpSymbolMatchAll
            | NativeFunction::RegExpSymbolReplace
            | NativeFunction::RegExpSymbolSearch
            | NativeFunction::RegExpSymbolSplit => {
                self.regexp_symbol_method(dom, function, &JsValue::Object(receiver), arguments)
            }
            NativeFunction::RegExpStringIteratorNext => {
                self.regexp_string_iterator_next(dom, receiver)
            }
            NativeFunction::RegExpEscape => Self::regexp_escape(arguments),
            NativeFunction::RegExpSpecies => Ok(JsValue::Object(receiver)),
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
        let key = (pattern.to_owned(), flags.to_owned());
        let compiled = if let Some(compiled) = self.regex_cache.get(&key) {
            std::rc::Rc::clone(compiled)
        } else {
            let compiled =
                std::rc::Rc::new(crate::regex::compile(pattern, flags).map_err(|error| {
                    JsError::syntax(
                        format!("invalid regular expression /{pattern}/{flags}: {error}"),
                        0,
                    )
                })?);
            // A page that builds patterns from data must not grow the cache
            // without bound, so it starts over once it is full.
            if self.regex_cache.len() >= 256 {
                self.regex_cache.clear();
            }
            self.regex_cache.insert(key, std::rc::Rc::clone(&compiled));
            compiled
        };
        let index = self.regexes.len();
        self.regexes.push(RegexRecord { compiled });
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
                    return Ok(JsValue::String(escaped_source(
                        self.regexes[index].compiled.source(),
                    )));
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
    pub(in crate::runtime) fn regexp_flags_from_getters(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
    ) -> Result<JsValue, JsError> {
        let mut text = String::new();
        for (name, letter, accessor) in [
            ("hasIndices", 'd', RegExpAccessor::HasIndices),
            ("global", 'g', RegExpAccessor::Global),
            ("ignoreCase", 'i', RegExpAccessor::IgnoreCase),
            ("multiline", 'm', RegExpAccessor::Multiline),
            ("dotAll", 's', RegExpAccessor::DotAll),
            ("unicode", 'u', RegExpAccessor::Unicode),
            ("unicodeSets", 'v', RegExpAccessor::UnicodeSets),
            ("sticky", 'y', RegExpAccessor::Sticky),
        ] {
            // The built-in getter answers exactly what `Get` would, so it is
            // read directly; a patched or inherited getter is called.
            let set = if self.is_intrinsic_regexp_getter(receiver, name, accessor) {
                self.regexp_accessor(dom, receiver, accessor)?.is_truthy()
            } else {
                self.get_member(dom, receiver, name)?.is_truthy()
            };
            if set {
                text.push(letter);
            }
        }
        Ok(JsValue::String(text))
    }

    /// Whether `Get(receiver, name)` would run the built-in `accessor` getter:
    /// `receiver` is a `RegExp` whose prototype is the intrinsic one, and that
    /// prototype still holds the built-in getter under `name`.
    pub(in crate::runtime) fn is_intrinsic_regexp_getter(
        &self,
        receiver: ObjectId,
        name: &str,
        accessor: RegExpAccessor,
    ) -> bool {
        if !matches!(self.realm.host(receiver), Some(ObjectHost::RegExp(_)))
            || self.realm.own_property(receiver, name).is_some()
        {
            return false;
        }
        let prototype = self.realm.regexp_prototype();
        if self.realm.get_prototype(receiver) != Some(prototype) {
            return false;
        }
        match self.realm.own_property(prototype, name) {
            Some(descriptor) => descriptor.getter.is_some_and(|getter| {
                matches!(
                    self.realm.host(getter),
                    Some(ObjectHost::NativeFunction(NativeFunction::RegExpAccessor(found)))
                        if found == accessor
                )
            }),
            None => false,
        }
    }

    pub(in crate::runtime) fn regex_index(&self, object: ObjectId) -> Result<usize, JsError> {
        match self.realm.host(object) {
            Some(ObjectHost::RegExp(index)) => Ok(index),
            _ => Err(JsError::type_error("incompatible RegExp method receiver")),
        }
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

    /// ECMA-262 22.2.6.2 `RegExp.prototype.exec`: `RegExpBuiltinExec` on the
    /// receiver, which must be a `RegExp`.
    pub(in crate::runtime) fn regexp_exec(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let argument = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let text = self.to_string_value(dom, &argument)?;
        self.regexp_builtin_exec(dom, receiver, &text)
    }

    /// ECMA-262 22.2.6.16 `RegExp.prototype.test`: `RegExpExec` on any object,
    /// so a user-defined `exec` decides the answer.
    pub(in crate::runtime) fn regexp_test(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let argument = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let text = self.to_string_value(dom, &argument)?;
        let input = utf16::utf16_units(&text);
        Ok(JsValue::Boolean(!matches!(
            self.regexp_exec_step(dom, receiver, &text, &input)?,
            crate::runtime::builtins::regexp_symbols::Exec::None
        )))
    }

    pub(in crate::runtime) fn regexp_to_string(
        &mut self,
        receiver: ObjectId,
    ) -> Result<JsValue, JsError> {
        let index = self.regex_index(receiver)?;
        let source = escaped_source(self.regexes[index].compiled.source());
        let flags = self.regexes[index].compiled.flags().describe();
        Ok(JsValue::String(format!("/{source}/{flags}")))
    }

    /// `RegExp.escape(string)`: the source text that matches `string` literally.
    /// Each code point is encoded by `EncodeForRegExpEscape`: a leading ASCII
    /// letter or digit as `\xHH`, syntax characters and `/` with a backslash,
    /// the control escapes as `\t \n \v \f \r`, the other punctuators, white
    /// space and line terminators as `\xHH` or `\uHHHH`, and a lone surrogate
    /// as `\uHHHH`. Everything else stands for itself.
    pub(in crate::runtime) fn regexp_escape(arguments: &[JsValue]) -> Result<JsValue, JsError> {
        let JsValue::String(text) = arguments.first().cloned().unwrap_or(JsValue::Undefined) else {
            return Err(JsError::type_error("RegExp.escape requires a string"));
        };
        let units = utf16::utf16_units(&text);
        let mut output = String::with_capacity(text.len() * 2);
        let mut index = 0usize;
        while index < units.len() {
            let lead = units[index];
            let pair = units.get(index + 1).copied().filter(|trail| {
                (0xDC00..=0xDFFF).contains(trail) && (0xD800..=0xDBFF).contains(&lead)
            });
            let (code, length) = match pair {
                Some(trail) => (
                    0x1_0000 + ((u32::from(lead) - 0xD800) << 10) + (u32::from(trail) - 0xDC00),
                    2,
                ),
                None => (u32::from(lead), 1),
            };
            let first = output.is_empty();
            escape_regexp_code_point(code, first, &mut output);
            index += length;
        }
        Ok(JsValue::String(output))
    }
}

/// `EscapeRegExpPattern` (ECMA-262 22.2.6.13.1) behind the `source` getter: the
/// empty pattern reads `(?:)`, an unescaped `/` outside a class is escaped, and
/// a line terminator is written as its escape so the source stays one line.
fn escaped_source(source: &str) -> String {
    if source.is_empty() {
        return "(?:)".to_owned();
    }
    let mut output = String::with_capacity(source.len());
    let mut in_class = false;
    let mut characters = source.chars();
    while let Some(character) = characters.next() {
        match character {
            '\\' => {
                output.push('\\');
                // A backslash escapes the next character, which is copied
                // as written; a line terminator after it is written escaped.
                match characters.next() {
                    Some('\n') => output.push('n'),
                    Some('\r') => output.push('r'),
                    Some('\u{2028}') => output.push_str("u2028"),
                    Some('\u{2029}') => output.push_str("u2029"),
                    Some(next) => output.push(next),
                    None => {}
                }
            }
            '[' => {
                in_class = true;
                output.push(character);
            }
            ']' => {
                in_class = false;
                output.push(character);
            }
            '/' if !in_class => output.push_str("\\/"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\u{2028}' => output.push_str("\\u2028"),
            '\u{2029}' => output.push_str("\\u2029"),
            other => output.push(other),
        }
    }
    output
}

/// `EncodeForRegExpEscape` (RegExp.escape proposal) for one code point, appended
/// to `output`. `first` is true when nothing has been emitted yet.
fn escape_regexp_code_point(code: u32, first: bool, output: &mut String) {
    let Some(character) = char::from_u32(code) else {
        // A lone surrogate is a code point `char` cannot hold.
        let _ = write!(output, "\\u{code:04x}");
        return;
    };
    if first && character.is_ascii_alphanumeric() {
        let _ = write!(output, "\\x{code:02x}");
        return;
    }
    match character {
        '^' | '$' | '\\' | '.' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|'
        | '/' => {
            output.push('\\');
            output.push(character);
        }
        '\t' => output.push_str("\\t"),
        '\n' => output.push_str("\\n"),
        '\u{b}' => output.push_str("\\v"),
        '\u{c}' => output.push_str("\\f"),
        '\r' => output.push_str("\\r"),
        ',' | '-' | '=' | '<' | '>' | '#' | '&' | '!' | '%' | ':' | ';' | '@' | '~' | '\''
        | '`' | '"' => {
            let _ = write!(output, "\\x{code:02x}");
        }
        _ if is_regexp_escape_space(code) => {
            if code <= 0xFF {
                let _ = write!(output, "\\x{code:02x}");
            } else {
                let _ = write!(output, "\\u{code:04x}");
            }
        }
        _ => output.push(character),
    }
}

/// `WhiteSpace` and `LineTerminator` (ECMA-262 12.2-12.3) as `RegExp.escape`
/// names them: the Zs category plus tab, vertical tab, form feed, no-break
/// space, the byte-order mark and the line terminators.
fn is_regexp_escape_space(code: u32) -> bool {
    matches!(
        code,
        0x09..=0x0D
            | 0x20
            | 0xA0
            | 0x1680
            | 0x2000..=0x200A
            | 0x2028
            | 0x2029
            | 0x202F
            | 0x205F
            | 0x3000
            | 0xFEFF
    )
}
