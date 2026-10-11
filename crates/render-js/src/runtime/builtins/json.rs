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
use crate::runtime::JsRuntime;
use crate::runtime::convert::required_argument;
use crate::utf16::JsString;
use crate::value::NativeFunction;
use crate::value::ObjectHost;
use render_dom::Dom;
use std::collections::BTreeSet;
use std::fmt::Write as _;

impl JsRuntime {
    pub(in crate::runtime) fn dispatch_json_native(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match function {
            NativeFunction::JsonParse => {
                let input = required_argument(arguments, 0, "JSON.parse")?.to_js_string();
                let node = JsonParser::new(input.units())
                    .parse()
                    .map_err(|message| JsError::syntax(message, 0))?;
                self.json_node_to_value(node)
            }
            NativeFunction::JsonStringify => {
                let value = arguments.first().cloned().unwrap_or(JsValue::Undefined);
                let mut stack = BTreeSet::new();
                match self.json_stringify_value(dom, &value, &mut stack, 0)? {
                    Some(value) => Ok(JsValue::String((value).into())),
                    None => Ok(JsValue::Undefined),
                }
            }
            other => self.dispatch_date_native(dom, other, receiver, arguments),
        }
    }
}

#[derive(Debug)]
pub(in crate::runtime) enum JsonNode {
    Null,
    Bool(bool),
    Number(f64),
    String(JsString),
    Array(Vec<JsonNode>),
    /// The members in source order. A name is code units, so a lone surrogate
    /// in a name survives into the property key.
    Object(Vec<(JsString, JsonNode)>),
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
        match self.peek() {
            Some(b'n') => {
                self.literal(b"null")?;
                Ok(JsonNode::Null)
            }
            Some(b't') => {
                self.literal(b"true")?;
                Ok(JsonNode::Bool(true))
            }
            Some(b'f') => {
                self.literal(b"false")?;
                Ok(JsonNode::Bool(false))
            }
            Some(b'"') => Ok(JsonNode::String(self.string()?)),
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
        if self.consume(b'-') {}
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
        let text: String = self.input[start..self.position]
            .iter()
            .map(|&unit| u8::try_from(unit).map_or('?', char::from))
            .collect();
        let value = text
            .parse::<f64>()
            .map_err(|_| "invalid JSON number".to_owned())?;
        if !value.is_finite() {
            return Err(self.error("JSON number is not finite"));
        }
        Ok(JsonNode::Number(value))
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

/// `QuoteJSONString` (ECMA-262 25.5.2.3 in ES2019 and later). Every code unit is
/// read in order, so a lone surrogate is written as its `\uXXXX` escape rather
/// than as the replacement character.
pub(in crate::runtime) fn json_quote(value: &JsString) -> String {
    let mut output = String::with_capacity(value.len() + 2);
    output.push('"');
    for unit in char::decode_utf16(value.units().iter().copied()) {
        match unit {
            Ok(character) => match character {
                '"' => output.push_str("\\\""),
                '\\' => output.push_str("\\\\"),
                '\u{0008}' => output.push_str("\\b"),
                '\u{000c}' => output.push_str("\\f"),
                '\n' => output.push_str("\\n"),
                '\r' => output.push_str("\\r"),
                '\t' => output.push_str("\\t"),
                character if character < '\u{0020}' => {
                    let _ = write!(output, "\\u{:04x}", character as u32);
                }
                character => output.push(character),
            },
            Err(error) => {
                let _ = write!(output, "\\u{:04x}", error.unpaired_surrogate());
            }
        }
    }
    output.push('"');
    output
}

impl JsRuntime {
    pub(in crate::runtime) fn json_node_to_value(
        &mut self,
        node: JsonNode,
    ) -> Result<JsValue, JsError> {
        match node {
            JsonNode::Null => Ok(JsValue::Null),
            JsonNode::Bool(value) => Ok(JsValue::Boolean(value)),
            JsonNode::Number(value) => Ok(JsValue::Number(value)),
            JsonNode::String(value) => Ok(JsValue::String(value)),
            JsonNode::Array(values) => {
                self.ensure_heap_capacity(1)?;
                let array = self.realm.create_array();
                for (index, value) in values.into_iter().enumerate() {
                    let value = self.json_node_to_value(value)?;
                    self.realm.set_property(array, index.to_string(), value);
                }
                let length = self.realm.own_property_names(array).map_or(0, |names| {
                    names
                        .iter()
                        .filter_map(|name| name.parse::<usize>().ok())
                        .max()
                        .map_or(0, |n| n + 1)
                });
                self.realm
                    .set_property(array, "length".to_owned(), JsValue::Number(length as f64));
                Ok(JsValue::Object(array))
            }
            JsonNode::Object(properties) => {
                self.ensure_heap_capacity(1)?;
                let object = self.realm.create_ordinary_object();
                for (name, value) in properties {
                    let value = self.json_node_to_value(value)?;
                    self.realm.set_property(object, name.to_key(), value);
                }
                Ok(JsValue::Object(object))
            }
        }
    }

    /// `JSON.stringify` of one value as text, or `None` when it has no JSON
    /// form (`undefined`, a symbol, or a function).
    pub(in crate::runtime) fn json_text_of(
        &mut self,
        dom: &mut Dom,
        value: &JsValue,
    ) -> Result<Option<String>, JsError> {
        let mut stack = BTreeSet::new();
        self.json_stringify_value(dom, value, &mut stack, 0)
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

    pub(in crate::runtime) fn json_stringify_value(
        &mut self,
        dom: &mut Dom,
        value: &JsValue,
        stack: &mut BTreeSet<ObjectId>,
        depth: usize,
    ) -> Result<Option<String>, JsError> {
        if depth > 128 {
            return Err(JsError::resource("JSON nesting depth exceeded"));
        }
        Ok(match value {
            JsValue::Undefined | JsValue::Symbol(_) => None,
            JsValue::Null => Some("null".to_owned()),
            JsValue::Boolean(value) => Some(value.to_string()),
            JsValue::Number(value) if value.is_finite() => {
                Some(crate::value::number_to_string(*value))
            }
            JsValue::Number(_) => Some("null".to_owned()),
            // ECMA-262 25.5.2.2 SerializeJSONProperty: a BigInt has no JSON form.
            JsValue::BigInt(_) => {
                return Err(JsError::type_error("Do not know how to serialize a BigInt"));
            }
            JsValue::String(value) => Some(json_quote(value)),
            JsValue::Object(object) => {
                if !stack.insert(*object) {
                    return Err(JsError::type_error("Converting circular structure to JSON"));
                }
                let result = if matches!(self.realm.host(*object), Some(ObjectHost::Array)) {
                    let values = self.array_elements_for(dom, *object)?;
                    let mut output = String::from("[");
                    for (index, value) in values.iter().enumerate() {
                        if index > 0 {
                            output.push(',');
                        }
                        output.push_str(
                            self.json_stringify_value(dom, value, stack, depth + 1)?
                                .as_deref()
                                .unwrap_or("null"),
                        );
                    }
                    output.push(']');
                    output
                } else {
                    let mut output = String::from("{");
                    let mut first = true;
                    for (name, value) in self
                        .realm
                        .enumerable_own_properties(*object)
                        .unwrap_or_default()
                    {
                        let Some(value) =
                            self.json_stringify_value(dom, &value, stack, depth + 1)?
                        else {
                            continue;
                        };
                        if !first {
                            output.push(',');
                        }
                        first = false;
                        output.push_str(&json_quote(&JsString::from_key(&name)));
                        output.push(':');
                        output.push_str(&value);
                    }
                    output.push('}');
                    output
                };
                stack.remove(object);
                Some(result)
            }
        })
    }
}
