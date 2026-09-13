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
use crate::js::runtime::JsRuntime;
use crate::js::runtime::convert::required_argument;
use crate::js::value::NativeFunction;
use crate::js::value::ObjectHost;
use std::collections::BTreeSet;
use std::fmt::Write as _;

impl JsRuntime {
    pub(in crate::js::runtime) fn dispatch_json_native(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match function {
            NativeFunction::JsonParse => {
                let input = required_argument(arguments, 0, "JSON.parse")?.to_js_string();
                let node = JsonParser::new(&input)
                    .parse()
                    .map_err(|message| JsError::syntax(message, 0))?;
                self.json_node_to_value(node)
            }
            NativeFunction::JsonStringify => {
                let value = arguments.first().cloned().unwrap_or(JsValue::Undefined);
                let mut stack = BTreeSet::new();
                match self.json_stringify_value(&value, &mut stack, 0)? {
                    Some(value) => Ok(JsValue::String(value)),
                    None => Ok(JsValue::Undefined),
                }
            }
            other => self.dispatch_date_native(dom, other, receiver, arguments),
        }
    }
}

#[derive(Debug)]
pub(in crate::js::runtime) enum JsonNode {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<JsonNode>),
    Object(Vec<(String, JsonNode)>),
}

pub(in crate::js::runtime) struct JsonParser<'a> {
    pub(in crate::js::runtime) input: &'a [u8],
    pub(in crate::js::runtime) position: usize,
}

impl<'a> JsonParser<'a> {
    pub(in crate::js::runtime) fn new(input: &'a str) -> Self {
        Self {
            input: input.as_bytes(),
            position: 0,
        }
    }

    pub(in crate::js::runtime) fn parse(mut self) -> Result<JsonNode, String> {
        let value = self.value(0)?;
        self.whitespace();
        if self.position != self.input.len() {
            return Err("unexpected trailing JSON input".to_owned());
        }
        Ok(value)
    }

    pub(in crate::js::runtime) fn value(&mut self, depth: usize) -> Result<JsonNode, String> {
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

    pub(in crate::js::runtime) fn array(&mut self, depth: usize) -> Result<JsonNode, String> {
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

    pub(in crate::js::runtime) fn object(&mut self, depth: usize) -> Result<JsonNode, String> {
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

    pub(in crate::js::runtime) fn number(&mut self) -> Result<JsonNode, String> {
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
        let text = std::str::from_utf8(&self.input[start..self.position])
            .map_err(|_| "invalid JSON number".to_owned())?;
        let value = text
            .parse::<f64>()
            .map_err(|_| "invalid JSON number".to_owned())?;
        if !value.is_finite() {
            return Err(self.error("JSON number is not finite"));
        }
        Ok(JsonNode::Number(value))
    }

    pub(in crate::js::runtime) fn string(&mut self) -> Result<String, String> {
        if !self.consume(b'"') {
            return Err(self.error("expected JSON string"));
        }
        let mut output = String::new();
        while let Some(byte) = self.peek() {
            self.position += 1;
            match byte {
                b'"' => return Ok(output),
                b'\\' => {
                    let escape = self
                        .peek()
                        .ok_or_else(|| self.error("unterminated JSON escape"))?;
                    self.position += 1;
                    match escape {
                        b'"' | b'\\' | b'/' => output.push(escape as char),
                        b'b' => output.push('\u{0008}'),
                        b'f' => output.push('\u{000c}'),
                        b'n' => output.push('\n'),
                        b'r' => output.push('\r'),
                        b't' => output.push('\t'),
                        b'u' => {
                            let end = self
                                .position
                                .checked_add(4)
                                .ok_or_else(|| self.error("invalid unicode escape"))?;
                            let digits = self
                                .input
                                .get(self.position..end)
                                .ok_or_else(|| self.error("invalid unicode escape"))?;
                            let hex = std::str::from_utf8(digits)
                                .map_err(|_| self.error("invalid unicode escape"))?;
                            let value = u16::from_str_radix(hex, 16)
                                .map_err(|_| self.error("invalid unicode escape"))?;
                            output.push(char::from_u32(u32::from(value)).unwrap_or('\u{fffd}'));
                            self.position = end;
                        }
                        _ => return Err(self.error("invalid JSON escape")),
                    }
                }
                0..=0x1f => return Err(self.error("control character in JSON string")),
                _ => {
                    let start = self.position - 1;
                    while self
                        .peek()
                        .is_some_and(|next| !matches!(next, b'"' | b'\\' | 0..=0x1f))
                    {
                        self.position += 1;
                    }
                    let text = std::str::from_utf8(&self.input[start..self.position])
                        .map_err(|_| self.error("invalid UTF-8 in JSON string"))?;
                    output.push_str(text);
                }
            }
        }
        Err(self.error("unterminated JSON string"))
    }

    pub(in crate::js::runtime) fn literal(&mut self, literal: &[u8]) -> Result<(), String> {
        if self.input.get(self.position..self.position + literal.len()) == Some(literal) {
            self.position += literal.len();
            Ok(())
        } else {
            Err(self.error("invalid JSON literal"))
        }
    }
    pub(in crate::js::runtime) fn whitespace(&mut self) {
        while self
            .peek()
            .is_some_and(|byte| matches!(byte, b' ' | b'\n' | b'\r' | b'\t'))
        {
            self.position += 1;
        }
    }
    pub(in crate::js::runtime) fn consume(&mut self, expected: u8) -> bool {
        if self.peek() == Some(expected) {
            self.position += 1;
            true
        } else {
            false
        }
    }
    pub(in crate::js::runtime) fn peek(&self) -> Option<u8> {
        self.input.get(self.position).copied()
    }
    pub(in crate::js::runtime) fn error(&self, message: &str) -> String {
        format!("{message} at byte {}", self.position)
    }
}

pub(in crate::js::runtime) fn json_quote(value: &str) -> String {
    let mut output = String::with_capacity(value.len() + 2);
    output.push('"');
    for character in value.chars() {
        match character {
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
        }
    }
    output.push('"');
    output
}

impl JsRuntime {
    pub(in crate::js::runtime) fn json_node_to_value(
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
                    self.realm.set_property(object, name, value);
                }
                Ok(JsValue::Object(object))
            }
        }
    }

    pub(in crate::js::runtime) fn json_stringify_value(
        &mut self,
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
                Some(crate::js::value::number_to_string(*value))
            }
            JsValue::Number(_) => Some("null".to_owned()),
            JsValue::String(value) => Some(json_quote(value)),
            JsValue::Object(object) => {
                if !stack.insert(*object) {
                    return Err(JsError::type_error("Converting circular structure to JSON"));
                }
                let result = if matches!(self.realm.host(*object), Some(ObjectHost::Array)) {
                    let values = self.array_elements_for(*object);
                    let mut output = String::from("[");
                    for (index, value) in values.iter().enumerate() {
                        if index > 0 {
                            output.push(',');
                        }
                        output.push_str(
                            self.json_stringify_value(value, stack, depth + 1)?
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
                        let Some(value) = self.json_stringify_value(&value, stack, depth + 1)?
                        else {
                            continue;
                        };
                        if !first {
                            output.push(',');
                        }
                        first = false;
                        output.push_str(&json_quote(&name));
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
