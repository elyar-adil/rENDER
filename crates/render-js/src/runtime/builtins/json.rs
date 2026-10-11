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

use std::ops::Range;

use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::runtime::builtins::object::{PartialDescriptor, PropertyName};
use crate::utf16::JsString;
use crate::value::NativeFunction;
use crate::value::ObjectHost;
use crate::value::number_to_string;
use crate::value::same_value;
use render_dom::Dom;

impl JsRuntime {
    pub(in crate::runtime) fn dispatch_json_native(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match function {
            NativeFunction::JsonParse => self.json_parse(dom, arguments),
            NativeFunction::JsonStringify => self.json_stringify(dom, arguments),
            NativeFunction::JsonRawJson => self.json_raw_json(dom, arguments),
            // ECMA-262 JSON.isRawJSON: true exactly for the objects JSON.rawJSON made.
            NativeFunction::JsonIsRawJson => Ok(JsValue::Boolean(matches!(
                arguments.first(),
                Some(JsValue::Object(object))
                    if matches!(self.realm.host(*object), Some(ObjectHost::RawJson))
            ))),
            other => self.dispatch_date_native(dom, other, receiver, arguments),
        }
    }
}

/// One parsed JSON value. A primitive keeps the code-unit range of its source
/// text, which the reviver's `context.source` reports.
#[derive(Debug)]
pub(in crate::runtime) enum JsonNode {
    Primitive(JsValue, Range<usize>),
    Array(Vec<JsonNode>),
    /// The members in source order. A name is code units, so a lone surrogate
    /// in a name survives into the property key.
    Object(Vec<(JsString, JsonNode)>),
}

/// The JSON Parse Record of json-parse-with-source: the value a parse created,
/// the source of a primitive, and the records of the elements or members beneath
/// it. The reviver walk compares `value` with the holder's current value, so a
/// record only describes a value the reviver has not changed.
pub(in crate::runtime) struct JsonRecord {
    value: JsValue,
    source: Option<Range<usize>>,
    elements: Vec<JsonRecord>,
    entries: Vec<(String, JsonRecord)>,
}

/// What a non-ASCII code unit reads as in the byte checks below. It is not a
/// JSON structural byte, so every structural test treats it as "something else".
const NOT_ASCII: u8 = 0xFF;

/// The value of one hexadecimal digit code unit.
fn hex_digit(unit: u16) -> Option<u16> {
    u8::try_from(unit)
        .ok()
        .and_then(|byte| char::from(byte).to_digit(16))
        .and_then(|digit| u16::try_from(digit).ok())
}

/// The JSON text parser (ECMA-262 25.5.1.1). It reads code units: the text is a
/// string value, and a `\uD800` escape or a lone surrogate in a string is one
/// code unit that must reach the value unchanged.
pub(in crate::runtime) struct JsonParser<'a> {
    pub(in crate::runtime) input: &'a [u16],
    pub(in crate::runtime) position: usize,
}

impl<'a> JsonParser<'a> {
    pub(in crate::runtime) fn new(input: &'a [u16]) -> Self {
        Self { input, position: 0 }
    }

    pub(in crate::runtime) fn parse(mut self) -> Result<JsonNode, String> {
        let value = self.value(0)?;
        self.whitespace();
        if self.position != self.input.len() {
            return Err("unexpected trailing JSON input".to_owned());
        }
        Ok(value)
    }

    pub(in crate::runtime) fn value(&mut self, depth: usize) -> Result<JsonNode, String> {
        if depth > 128 {
            return Err("JSON nesting depth exceeded".to_owned());
        }
        self.whitespace();
        let start = self.position;
        let primitive = |value: JsValue, end: usize| JsonNode::Primitive(value, start..end);
        match self.peek() {
            Some(b'n') => {
                self.literal(b"null")?;
                Ok(primitive(JsValue::Null, self.position))
            }
            Some(b't') => {
                self.literal(b"true")?;
                Ok(primitive(JsValue::Boolean(true), self.position))
            }
            Some(b'f') => {
                self.literal(b"false")?;
                Ok(primitive(JsValue::Boolean(false), self.position))
            }
            Some(b'"') => {
                let text = self.string()?;
                Ok(primitive(JsValue::String(text), self.position))
            }
            Some(b'[') => self.array(depth),
            Some(b'{') => self.object(depth),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => Err(self.error("expected a JSON value")),
        }
    }

    pub(in crate::runtime) fn array(&mut self, depth: usize) -> Result<JsonNode, String> {
        self.position += 1;
        self.whitespace();
        let mut values = Vec::new();
        if self.consume(b']') {
            return Ok(JsonNode::Array(values));
        }
        loop {
            values.push(self.value(depth + 1)?);
            self.whitespace();
            if self.consume(b']') {
                break;
            }
            if !self.consume(b',') {
                return Err(self.error("expected ',' or ']'"));
            }
        }
        Ok(JsonNode::Array(values))
    }

    pub(in crate::runtime) fn object(&mut self, depth: usize) -> Result<JsonNode, String> {
        self.position += 1;
        self.whitespace();
        let mut properties = Vec::new();
        if self.consume(b'}') {
            return Ok(JsonNode::Object(properties));
        }
        loop {
            self.whitespace();
            if self.peek() != Some(b'"') {
                return Err(self.error("object keys must be strings"));
            }
            let name = self.string()?;
            self.whitespace();
            if !self.consume(b':') {
                return Err(self.error("expected ':' after object key"));
            }
            properties.push((name, self.value(depth + 1)?));
            self.whitespace();
            if self.consume(b'}') {
                break;
            }
            if !self.consume(b',') {
                return Err(self.error("expected ',' or '}'"));
            }
        }
        Ok(JsonNode::Object(properties))
    }

    pub(in crate::runtime) fn number(&mut self) -> Result<JsonNode, String> {
        let start = self.position;
        self.consume(b'-');
        match self.peek() {
            Some(b'0') => {
                self.position += 1;
            }
            Some(b'1'..=b'9') => {
                while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                    self.position += 1;
                }
            }
            _ => return Err(self.error("invalid JSON number")),
        }
        if self.consume(b'.') {
            let fraction_start = self.position;
            while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                self.position += 1;
            }
            if self.position == fraction_start {
                return Err(self.error("invalid JSON fraction"));
            }
        }
        if self.peek().is_some_and(|byte| byte == b'e' || byte == b'E') {
            self.position += 1;
            if self.peek().is_some_and(|byte| byte == b'+' || byte == b'-') {
                self.position += 1;
            }
            let exponent_start = self.position;
            while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                self.position += 1;
            }
            if self.position == exponent_start {
                return Err(self.error("invalid JSON exponent"));
            }
        }
        // Every unit of a number is ASCII, so each one reads as its character.
        // An out-of-range literal such as `1e400` is an ordinary Number value
        // (ECMA-262 7.1.4.1.1 rounds it to infinity), not a parse error.
        let text: String = self.input[start..self.position]
            .iter()
            .map(|&unit| u8::try_from(unit).map_or('?', char::from))
            .collect();
        let value = text
            .parse::<f64>()
            .map_err(|_| "invalid JSON number".to_owned())?;
        Ok(JsonNode::Primitive(
            JsValue::Number(value),
            start..self.position,
        ))
    }

    pub(in crate::runtime) fn string(&mut self) -> Result<JsString, String> {
        if !self.consume(b'"') {
            return Err(self.error("expected JSON string"));
        }
        let mut output: Vec<u16> = Vec::new();
        while let Some(unit) = self.input.get(self.position).copied() {
            self.position += 1;
            match unit {
                // The closing quote.
                0x22 => return Ok(JsString::from_unit_vec(output)),
                0x5C => {
                    let escape = self
                        .input
                        .get(self.position)
                        .copied()
                        .ok_or_else(|| self.error("unterminated JSON escape"))?;
                    self.position += 1;
                    output.push(match escape {
                        // `"`, `\`, `/`
                        0x22 | 0x5C | 0x2F => escape,
                        // `b`, `f`, `n`, `r`, `t`
                        0x62 => 0x08,
                        0x66 => 0x0C,
                        0x6E => 0x0A,
                        0x72 => 0x0D,
                        0x74 => 0x09,
                        // `u`: exactly four hexadecimal digits, one code unit.
                        0x75 => self.hex_escape()?,
                        _ => return Err(self.error("invalid JSON escape")),
                    });
                }
                0x00..=0x1F => return Err(self.error("control character in JSON string")),
                other => output.push(other),
            }
        }
        Err(self.error("unterminated JSON string"))
    }

    /// The four hexadecimal digits after `\u`, as one code unit.
    fn hex_escape(&mut self) -> Result<u16, String> {
        let mut value: u16 = 0;
        for _ in 0..4 {
            let digit = self
                .input
                .get(self.position)
                .copied()
                .and_then(hex_digit)
                .ok_or_else(|| self.error("invalid unicode escape"))?;
            value = value * 16 + digit;
            self.position += 1;
        }
        Ok(value)
    }

    pub(in crate::runtime) fn literal(&mut self, literal: &[u8]) -> Result<(), String> {
        let end = self.position + literal.len();
        let found = self.input.get(self.position..end).is_some_and(|units| {
            units
                .iter()
                .copied()
                .eq(literal.iter().map(|&byte| u16::from(byte)))
        });
        if found {
            self.position = end;
            Ok(())
        } else {
            Err(self.error("invalid JSON literal"))
        }
    }
    pub(in crate::runtime) fn whitespace(&mut self) {
        while self
            .peek()
            .is_some_and(|byte| matches!(byte, b' ' | b'\n' | b'\r' | b'\t'))
        {
            self.position += 1;
        }
    }
    pub(in crate::runtime) fn consume(&mut self, expected: u8) -> bool {
        if self.peek() == Some(expected) {
            self.position += 1;
            true
        } else {
            false
        }
    }
    pub(in crate::runtime) fn peek(&self) -> Option<u8> {
        self.input
            .get(self.position)
            .map(|&unit| u8::try_from(unit).unwrap_or(NOT_ASCII))
    }
    pub(in crate::runtime) fn error(&self, message: &str) -> String {
        format!("{message} at code unit {}", self.position)
    }
}

/// The code units of `text`, for output that is ASCII.
fn units_of(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

/// `QuoteJSONString` (ECMA-262 25.5.2.3). The text is read in code units, so
/// a matched surrogate pair is copied and a lone surrogate is written as its
/// `\uXXXX` escape, which keeps the output well-formed.
pub(in crate::runtime) fn json_quote(value: &JsString) -> Vec<u16> {
    let units = value.units();
    let mut output = Vec::with_capacity(units.len() + 2);
    output.push(u16::from(b'"'));
    let mut index = 0;
    while index < units.len() {
        let unit = units[index];
        let escape = match unit {
            0x22 => Some(r#"\""#),
            0x5C => Some(r"\\"),
            0x08 => Some(r"\b"),
            0x0C => Some(r"\f"),
            0x0A => Some(r"\n"),
            0x0D => Some(r"\r"),
            0x09 => Some(r"\t"),
            _ => None,
        };
        if let Some(escape) = escape {
            output.extend(units_of(escape));
        } else if (0xD800..=0xDBFF).contains(&unit)
            && units
                .get(index + 1)
                .is_some_and(|next| (0xDC00..=0xDFFF).contains(next))
        {
            output.push(unit);
            output.push(units[index + 1]);
            index += 1;
        } else if unit < 0x20 || (0xD800..=0xDFFF).contains(&unit) {
            output.extend(units_of(&format!("\\u{unit:04x}")));
        } else {
            output.push(unit);
        }
        index += 1;
    }
    output.push(u16::from(b'"'));
    output
}

/// The state of one `JSON.stringify` call (ECMA-262 25.5.2.1 steps 1 to 7).
#[derive(Default)]
struct JsonWriter {
    replacer: Option<ObjectId>,
    property_list: Option<Vec<String>>,
    gap: Vec<u16>,
    indent: Vec<u16>,
    stack: Vec<ObjectId>,
}

/// Joins serialized members between `open` and `close` (ECMA-262 25.5.2.5 and
/// 25.5.2.2 final steps). `indent` is the indentation of the members, and
/// `stepback` the indentation of the closing bracket.
fn json_wrap(
    members: Vec<Vec<u16>>,
    open: u8,
    close: u8,
    writer: &JsonWriter,
    stepback: &[u16],
) -> Vec<u16> {
    let (comma, newline) = (u16::from(b','), u16::from(b'\n'));
    let mut output = vec![u16::from(open)];
    if members.is_empty() {
        output.push(u16::from(close));
        return output;
    }
    if writer.gap.is_empty() {
        for (index, member) in members.into_iter().enumerate() {
            if index > 0 {
                output.push(comma);
            }
            output.extend(member);
        }
    } else {
        output.push(newline);
        for (index, member) in members.into_iter().enumerate() {
            if index > 0 {
                output.push(comma);
                output.push(newline);
            }
            output.extend_from_slice(&writer.indent);
            output.extend(member);
        }
        output.push(newline);
        output.extend_from_slice(stepback);
    }
    output.push(u16::from(close));
    output
}

impl JsRuntime {
    /// ECMA-262 25.5.1 `JSON.parse`: `ToString(text)`, the parse, and then the
    /// reviver walk (25.5.1.1 `InternalizeJSONProperty`) when a callable reviver
    /// is given. The reviver is looked up once, after the text parses.
    fn json_parse(&mut self, dom: &mut Dom, arguments: &[JsValue]) -> Result<JsValue, JsError> {
        let text_value = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let text = self.to_string_value(dom, &text_value)?;
        let node = JsonParser::new(text.units())
            .parse()
            .map_err(|message| JsError::syntax(message, 0))?;
        let reviver = match arguments.get(1) {
            Some(JsValue::Object(object))
                if JsRuntime::is_callable_object(*object, &self.realm) =>
            {
                Some(*object)
            }
            _ => None,
        };
        let Some(reviver) = reviver else {
            return self.json_node_to_value(node);
        };
        let (value, record) = self.json_build(node, true)?;
        self.ensure_heap_capacity(1)?;
        let root = self.realm.create_ordinary_object();
        self.realm.set_property(root, String::new(), value);
        self.json_internalize(dom, root, "", reviver, record.as_ref(), &text)
    }

    /// ECMA-262 25.5.1.1 `InternalizeJSONProperty`, with the `context` argument of
    /// json-parse-with-source. `context.source` is present only for a primitive
    /// the reviver has not replaced, and the records of an array or object
    /// apply only while the holder still holds that same object.
    fn json_internalize(
        &mut self,
        dom: &mut Dom,
        holder: ObjectId,
        name: &str,
        reviver: ObjectId,
        record: Option<&JsonRecord>,
        text: &JsString,
    ) -> Result<JsValue, JsError> {
        let value = self.get_member(dom, holder, name)?;
        self.ensure_heap_capacity(1)?;
        let context = self.realm.create_ordinary_object();
        let record = record.filter(|record| same_value(&record.value, &value));
        if let Some(range) = record.and_then(|record| record.source.clone()) {
            let source = text.units()[range].to_vec();
            self.realm.set_property(
                context,
                "source".to_owned(),
                JsValue::String(JsString::from_unit_vec(source)),
            );
        }
        if let JsValue::Object(object) = value {
            if self.is_array_value(&value)? {
                let length = self.get_member(dom, object, "length")?;
                let length = self.to_length_value(dom, &length)?;
                let mut index = 0.0;
                while index < length {
                    let key = number_to_string(index);
                    let child = record.and_then(|record| record.elements.get(index as usize));
                    self.json_revive_member(dom, object, key, reviver, child, text)?;
                    index += 1.0;
                }
            } else {
                for key in self.json_enumerable_keys(dom, object)? {
                    let child = record.and_then(|record| {
                        record
                            .entries
                            .iter()
                            .rev()
                            .find(|(name, _)| *name == key)
                            .map(|(_, child)| child)
                    });
                    self.json_revive_member(dom, object, key, reviver, child, text)?;
                }
            }
        }
        let arguments = [
            JsValue::String(JsString::from_key(name)),
            value,
            JsValue::Object(context),
        ];
        self.call_with_this(dom, reviver, &arguments, JsValue::Object(holder))
    }

    /// One member step of `InternalizeJSONProperty`: revive it, then delete it
    /// when the reviver returned `undefined`, and otherwise define the result
    /// as a data property (`CreateDataProperty`, whose result is not checked).
    fn json_revive_member(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        key: String,
        reviver: ObjectId,
        record: Option<&JsonRecord>,
        text: &JsString,
    ) -> Result<(), JsError> {
        let element = self.json_internalize(dom, object, &key, reviver, record, text)?;
        if matches!(element, JsValue::Undefined) {
            self.delete_property_on(dom, object, &PropertyName::String(key))?;
        } else {
            let partial = PartialDescriptor {
                value: Some(element),
                writable: Some(true),
                enumerable: Some(true),
                configurable: Some(true),
                ..PartialDescriptor::default()
            };
            self.define_own_property(dom, object, &PropertyName::String(key), partial)?;
        }
        Ok(())
    }

    /// The string keys that `EnumerableOwnProperties` (ECMA-262 7.3.23) reports:
    /// the enumerable own string keys, in `[[OwnPropertyKeys]]` order.
    fn json_enumerable_keys(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
    ) -> Result<Vec<String>, JsError> {
        let mut keys = Vec::new();
        for key in self.own_property_keys(dom, object)? {
            let PropertyName::String(name) = &key else {
                continue;
            };
            if self
                .own_descriptor(dom, object, &key)?
                .is_some_and(|descriptor| descriptor.enumerable)
            {
                keys.push(name.clone());
            }
        }
        Ok(keys)
    }

    /// ECMA-262 25.5.1 steps 3 to 4 for `JSON.parse` without a reviver: build
    /// the value of a parsed node.
    pub(in crate::runtime) fn json_node_to_value(
        &mut self,
        node: JsonNode,
    ) -> Result<JsValue, JsError> {
        Ok(self.json_build(node, false)?.0)
    }

    /// Builds the value of a parsed node, and its parse record when `keep` is
    /// set. A record is only needed by a reviver, so it is not built otherwise.
    fn json_build(
        &mut self,
        node: JsonNode,
        keep: bool,
    ) -> Result<(JsValue, Option<JsonRecord>), JsError> {
        match node {
            JsonNode::Primitive(value, source) => {
                let record = keep.then(|| JsonRecord {
                    value: value.clone(),
                    source: Some(source),
                    elements: Vec::new(),
                    entries: Vec::new(),
                });
                Ok((value, record))
            }
            JsonNode::Array(nodes) => {
                let mut values = Vec::with_capacity(nodes.len());
                let mut elements = Vec::new();
                for node in nodes {
                    let (value, record) = self.json_build(node, keep)?;
                    values.push(value);
                    elements.extend(record);
                }
                let value = JsValue::Object(self.create_array_from_values(&values)?);
                let record = keep.then(|| JsonRecord {
                    value: value.clone(),
                    source: None,
                    elements,
                    entries: Vec::new(),
                });
                Ok((value, record))
            }
            JsonNode::Object(members) => {
                self.ensure_heap_capacity(1)?;
                let object = self.realm.create_ordinary_object();
                let mut entries = Vec::new();
                for (name, node) in members {
                    let (value, record) = self.json_build(node, keep)?;
                    let key = name.to_key();
                    self.realm.set_property(object, key.clone(), value);
                    entries.extend(record.map(|record| (key, record)));
                }
                let value = JsValue::Object(object);
                let record = keep.then(|| JsonRecord {
                    value: value.clone(),
                    source: None,
                    elements: Vec::new(),
                    entries,
                });
                Ok((value, record))
            }
        }
    }

    /// ECMA-262 25.5.2 `JSON.stringify(value, replacer, space)`.
    fn json_stringify(&mut self, dom: &mut Dom, arguments: &[JsValue]) -> Result<JsValue, JsError> {
        let value = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let replacer = arguments.get(1).cloned().unwrap_or(JsValue::Undefined);
        let mut writer = JsonWriter::default();
        if let JsValue::Object(replacer_object) = replacer {
            if JsRuntime::is_callable_object(replacer_object, &self.realm) {
                writer.replacer = Some(replacer_object);
            } else if self.is_array_value(&replacer)? {
                writer.property_list = Some(self.json_property_list(dom, replacer_object)?);
            }
        }
        let mut space = arguments.get(2).cloned().unwrap_or(JsValue::Undefined);
        if let JsValue::Object(space_object) = space {
            match self.realm.host(space_object) {
                Some(ObjectHost::NumberPrimitive(_)) => {
                    space = JsValue::Number(self.to_number_value(dom, &space)?);
                }
                Some(ObjectHost::StringPrimitive(_)) => {
                    space = JsValue::String(self.to_string_value(dom, &space)?);
                }
                _ => {}
            }
        }
        writer.gap = match &space {
            JsValue::Number(_) => {
                let count = self.to_integer_value(dom, &space)?.clamp(0.0, 10.0) as usize;
                vec![u16::from(b' '); count]
            }
            JsValue::String(text) => text.units().iter().take(10).copied().collect(),
            _ => Vec::new(),
        };
        Ok(match self.json_serialize_root(dom, &mut writer, value)? {
            Some(text) => JsValue::String(JsString::from_unit_vec(text)),
            None => JsValue::Undefined,
        })
    }

    /// The `PropertyList` of a `JSON.stringify` array replacer (ECMA-262 25.5.2.1
    /// step 4.b): string and number elements, and String and Number wrapper
    /// elements, converted to keys with duplicates removed.
    fn json_property_list(
        &mut self,
        dom: &mut Dom,
        replacer: ObjectId,
    ) -> Result<Vec<String>, JsError> {
        let length = self.get_member(dom, replacer, "length")?;
        let length = self.to_length_value(dom, &length)?;
        let mut list: Vec<String> = Vec::new();
        let mut index = 0.0;
        while index < length {
            let element = self.get_member(dom, replacer, &number_to_string(index))?;
            let item = match &element {
                JsValue::String(text) => Some(text.clone()),
                JsValue::Number(_) => Some(self.to_string_value(dom, &element)?),
                JsValue::Object(object)
                    if matches!(
                        self.realm.host(*object),
                        Some(ObjectHost::StringPrimitive(_) | ObjectHost::NumberPrimitive(_))
                    ) =>
                {
                    Some(self.to_string_value(dom, &element)?)
                }
                _ => None,
            };
            if let Some(item) = item {
                let key = item.to_key();
                if !list.contains(&key) {
                    list.push(key);
                }
            }
            index += 1.0;
        }
        Ok(list)
    }

    /// `JSON.stringify` of one value as text, or `None` when it has no JSON
    /// form (`undefined`, a symbol, or a function).
    pub(in crate::runtime) fn json_text_of(
        &mut self,
        dom: &mut Dom,
        value: &JsValue,
    ) -> Result<Option<String>, JsError> {
        let mut writer = JsonWriter::default();
        Ok(self
            .json_serialize_root(dom, &mut writer, value.clone())?
            .map(|units| String::from_utf16_lossy(&units)))
    }

    /// `JSON.parse` of text into a value.
    pub(in crate::runtime) fn json_value_of_text(
        &mut self,
        text: &str,
    ) -> Result<JsValue, JsError> {
        let input = JsString::from(text);
        let node = JsonParser::new(input.units())
            .parse()
            .map_err(|message| JsError::syntax(message, 0))?;
        self.json_node_to_value(node)
    }

    /// ECMA-262 25.5.2.1 steps 5 to 8: the wrapper holding `value` under the
    /// empty key, then `SerializeJSONProperty` on it.
    fn json_serialize_root(
        &mut self,
        dom: &mut Dom,
        writer: &mut JsonWriter,
        value: JsValue,
    ) -> Result<Option<Vec<u16>>, JsError> {
        self.ensure_heap_capacity(1)?;
        let wrapper = self.realm.create_ordinary_object();
        self.realm.set_property(wrapper, String::new(), value);
        self.json_serialize_property(dom, writer, wrapper, "")
    }

    /// ECMA-262 25.5.2.2 `SerializeJSONProperty`.
    fn json_serialize_property(
        &mut self,
        dom: &mut Dom,
        writer: &mut JsonWriter,
        holder: ObjectId,
        key: &str,
    ) -> Result<Option<Vec<u16>>, JsError> {
        let mut value = self.get_member(dom, holder, key)?;
        if matches!(value, JsValue::Object(_) | JsValue::BigInt(_)) {
            let to_json = self.json_get_from_value(dom, &value, "toJSON")?;
            if let JsValue::Object(function) = to_json
                && JsRuntime::is_callable_object(function, &self.realm)
            {
                let arguments = [JsValue::String(JsString::from_key(key))];
                value = self.call_with_this(dom, function, &arguments, value.clone())?;
            }
        }
        if let Some(replacer) = writer.replacer {
            let arguments = [JsValue::String(JsString::from_key(key)), value];
            value = self.call_with_this(dom, replacer, &arguments, JsValue::Object(holder))?;
        }
        if let JsValue::Object(object) = value {
            // A `JSON.rawJSON` object is written as its own text.
            if matches!(self.realm.host(object), Some(ObjectHost::RawJson)) {
                let raw = self.get_member(dom, object, "rawJSON")?;
                return Ok(Some(self.to_string_value(dom, &raw)?.units().to_vec()));
            }
            value = match self.realm.host(object) {
                Some(ObjectHost::NumberPrimitive(_)) => {
                    JsValue::Number(self.to_number_value(dom, &value)?)
                }
                Some(ObjectHost::StringPrimitive(_)) => {
                    JsValue::String(self.to_string_value(dom, &value)?)
                }
                Some(ObjectHost::BooleanPrimitive(boolean)) => JsValue::Boolean(boolean),
                Some(ObjectHost::BigIntPrimitive(bigint)) => JsValue::BigInt(bigint),
                _ => value,
            };
        }
        match value {
            JsValue::Null => Ok(Some(units_of("null"))),
            JsValue::Boolean(true) => Ok(Some(units_of("true"))),
            JsValue::Boolean(false) => Ok(Some(units_of("false"))),
            JsValue::String(text) => Ok(Some(json_quote(&text))),
            JsValue::Number(number) if number.is_finite() => {
                Ok(Some(units_of(&number_to_string(number))))
            }
            JsValue::Number(_) => Ok(Some(units_of("null"))),
            // ECMA-262 25.5.2.2 step 10: a BigInt has no JSON form.
            JsValue::BigInt(_) => Err(JsError::type_error("Do not know how to serialize a BigInt")),
            JsValue::Object(object) if !JsRuntime::is_callable_object(object, &self.realm) => {
                if self.is_array_value(&JsValue::Object(object))? {
                    self.json_serialize_array(dom, writer, object).map(Some)
                } else {
                    self.json_serialize_object(dom, writer, object).map(Some)
                }
            }
            // `undefined`, a symbol, and a callable have no JSON form.
            _ => Ok(None),
        }
    }

    /// `GetV(value, key)` (ECMA-262 7.3.3): an object reads with [[Get]], and a
    /// primitive reads through its wrapper's prototype chain, with the primitive
    /// itself as the receiver of an accessor.
    fn json_get_from_value(
        &mut self,
        dom: &mut Dom,
        value: &JsValue,
        key: &str,
    ) -> Result<JsValue, JsError> {
        match value {
            JsValue::Object(object) => self.get_member(dom, *object, key),
            primitive => {
                let wrapper = self.to_object(primitive)?;
                match self.realm.get_descriptor(wrapper, key) {
                    Some(descriptor) if descriptor.is_accessor() => match descriptor.getter {
                        Some(getter) => self.call_with_this(dom, getter, &[], primitive.clone()),
                        None => Ok(JsValue::Undefined),
                    },
                    Some(descriptor) => Ok(descriptor.value),
                    None => Ok(JsValue::Undefined),
                }
            }
        }
    }

    /// ECMA-262 25.5.2.6 `SerializeJSONObject`.
    fn json_serialize_object(
        &mut self,
        dom: &mut Dom,
        writer: &mut JsonWriter,
        object: ObjectId,
    ) -> Result<Vec<u16>, JsError> {
        if writer.stack.contains(&object) {
            return Err(JsError::type_error("Converting circular structure to JSON"));
        }
        writer.stack.push(object);
        let stepback = writer.indent.clone();
        writer.indent.extend_from_slice(&writer.gap);
        let keys = match writer.property_list.clone() {
            Some(list) => list,
            None => self.json_enumerable_keys(dom, object)?,
        };
        let mut members = Vec::new();
        for key in keys {
            if let Some(text) = self.json_serialize_property(dom, writer, object, &key)? {
                let mut member = json_quote(&JsString::from_key(&key));
                member.push(u16::from(b':'));
                if !writer.gap.is_empty() {
                    member.push(u16::from(b' '));
                }
                member.extend(text);
                members.push(member);
            }
        }
        let output = json_wrap(members, b'{', b'}', writer, &stepback);
        writer.stack.pop();
        writer.indent = stepback;
        Ok(output)
    }

    /// ECMA-262 25.5.2.7 `SerializeJSONArray`.
    fn json_serialize_array(
        &mut self,
        dom: &mut Dom,
        writer: &mut JsonWriter,
        array: ObjectId,
    ) -> Result<Vec<u16>, JsError> {
        if writer.stack.contains(&array) {
            return Err(JsError::type_error("Converting circular structure to JSON"));
        }
        writer.stack.push(array);
        let stepback = writer.indent.clone();
        writer.indent.extend_from_slice(&writer.gap);
        let length = self.get_member(dom, array, "length")?;
        let length = self.to_length_value(dom, &length)?;
        let mut elements = Vec::new();
        let mut index = 0.0;
        while index < length {
            let key = number_to_string(index);
            let text = self
                .json_serialize_property(dom, writer, array, &key)?
                .unwrap_or_else(|| units_of("null"));
            elements.push(text);
            index += 1.0;
        }
        let output = json_wrap(elements, b'[', b']', writer, &stepback);
        writer.stack.pop();
        writer.indent = stepback;
        Ok(output)
    }

    /// `JSON.rawJSON(text)` (json-parse-with-source): the text must be one JSON
    /// primitive without surrounding white space. The result is a frozen object
    /// with a null prototype and a `rawJSON` property.
    fn json_raw_json(&mut self, dom: &mut Dom, arguments: &[JsValue]) -> Result<JsValue, JsError> {
        let text_value = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let text = self.to_string_value(dom, &text_value)?;
        let units = text.units();
        let is_white_space = |unit: Option<&u16>| matches!(unit, Some(0x09 | 0x0A | 0x0D | 0x20));
        if units.is_empty() || is_white_space(units.first()) || is_white_space(units.last()) {
            return Err(JsError::syntax(
                "JSON.rawJSON cannot be created from text with surrounding white space",
                0,
            ));
        }
        let node = JsonParser::new(units)
            .parse()
            .map_err(|message| JsError::syntax(message, 0))?;
        if matches!(node, JsonNode::Array(_) | JsonNode::Object(_)) {
            return Err(JsError::syntax(
                "JSON.rawJSON accepts only a JSON primitive",
                0,
            ));
        }
        self.ensure_heap_capacity(1)?;
        let object = self.realm.allocate_host(None, ObjectHost::RawJson);
        self.realm
            .set_property(object, "rawJSON".to_owned(), JsValue::String(text));
        self.set_integrity_level(dom, object, true)?;
        Ok(JsValue::Object(object))
    }
}
