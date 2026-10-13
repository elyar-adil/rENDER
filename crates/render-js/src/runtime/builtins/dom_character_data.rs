//! `CharacterData` (DOM §4.10): the data of Text, Comment and
//! `ProcessingInstruction` nodes. Offsets and counts are UTF-16 code units, as
//! the specification requires, and an offset past the end is an
//! `IndexSizeError`.
//!
//! The tree stores data as UTF-8 text, so a replacement that splits a
//! surrogate pair cannot be stored exactly: the unpaired half becomes U+FFFD
//! when the new data is written. `substringData` still returns the exact code
//! units it was asked for.

use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::runtime::builtins::dom_collections::unsigned_long;
use crate::runtime::builtins::dom_exception::DomExceptionName;
use crate::runtime::convert::required_argument;
use crate::utf16::JsString;
use crate::value::NativeFunction;
use render_dom::Dom;
use render_dom::NodeId;
use render_dom::NodeKind;

/// The data of `node` when it is a `CharacterData` node.
pub(crate) fn character_data(dom: &Dom, node: NodeId) -> Option<&str> {
    match dom.node(node).map(render_dom::Node::kind)? {
        NodeKind::Text(data)
        | NodeKind::Comment(data)
        | NodeKind::ProcessingInstruction { data, .. } => Some(data.as_str()),
        _ => None,
    }
}

/// The length of `node`'s data in UTF-16 code units (DOM §4.10 `length`).
pub(crate) fn character_data_length(dom: &Dom, node: NodeId) -> Option<usize> {
    character_data(dom, node).map(|data| data.encode_utf16().count())
}

impl JsRuntime {
    /// The five `CharacterData` methods. The receiver must be a `CharacterData`
    /// node; anything else is an incompatible-receiver `TypeError`.
    pub(in crate::runtime) fn character_data_native(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let node = self.require_node(receiver)?;
        let Some(length) = character_data_length(dom, node) else {
            return Err(JsError::type_error(
                "incompatible CharacterData method receiver",
            ));
        };
        match function {
            NativeFunction::CharacterDataSubstringData => {
                let offset = self.offset_argument(dom, arguments, 0, "substringData")?;
                let count = self.offset_argument(dom, arguments, 1, "substringData")?;
                if offset > length {
                    return Err(self.index_size_error("substringData"));
                }
                let units = character_units(dom, node);
                let end = offset.saturating_add(count).min(length);
                Ok(JsValue::String(JsString::from_units(&units[offset..end])))
            }
            NativeFunction::CharacterDataAppendData => {
                let data = Self::data_argument(arguments, 0, "appendData")?;
                self.replace_character_data(dom, node, length, 0, &data, "appendData")
            }
            NativeFunction::CharacterDataInsertData => {
                let offset = self.offset_argument(dom, arguments, 0, "insertData")?;
                let data = Self::data_argument(arguments, 1, "insertData")?;
                self.replace_character_data(dom, node, offset, 0, &data, "insertData")
            }
            NativeFunction::CharacterDataDeleteData => {
                let offset = self.offset_argument(dom, arguments, 0, "deleteData")?;
                let count = self.offset_argument(dom, arguments, 1, "deleteData")?;
                self.replace_character_data(dom, node, offset, count, "", "deleteData")
            }
            NativeFunction::CharacterDataReplaceData => {
                let offset = self.offset_argument(dom, arguments, 0, "replaceData")?;
                let count = self.offset_argument(dom, arguments, 1, "replaceData")?;
                let data = Self::data_argument(arguments, 2, "replaceData")?;
                self.replace_character_data(dom, node, offset, count, &data, "replaceData")
            }
            other => unreachable!("not a CharacterData method: {other:?}"),
        }
    }

    /// DOM §4.10 "replace data": splice `data` in place of `count` code units
    /// at `offset`. An offset past the end is an `IndexSizeError`, and `count`
    /// is clamped to what remains.
    fn replace_character_data(
        &mut self,
        dom: &mut Dom,
        node: NodeId,
        offset: usize,
        count: usize,
        data: &str,
        method: &str,
    ) -> Result<JsValue, JsError> {
        let units = character_units(dom, node);
        let length = units.len();
        if offset > length {
            return Err(self.index_size_error(method));
        }
        let count = count.min(length - offset);
        let mut next = Vec::with_capacity(length - count + data.len());
        next.extend_from_slice(&units[..offset]);
        next.extend(data.encode_utf16());
        next.extend_from_slice(&units[offset + count..]);
        dom.set_character_data(node, String::from_utf16_lossy(&next))?;
        Ok(JsValue::Undefined)
    }

    /// An `unsigned long` argument (`WebIDL` §3.2.4.7).
    fn offset_argument(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
        index: usize,
        method: &str,
    ) -> Result<usize, JsError> {
        let value = required_argument(arguments, index, method)?;
        let number = self.to_number_value(dom, value)?;
        Ok(unsigned_long(number) as usize)
    }

    /// A `DOMString` data argument.
    fn data_argument(arguments: &[JsValue], index: usize, method: &str) -> Result<String, JsError> {
        Ok(required_argument(arguments, index, method)?
            .to_js_string()
            .to_lossy())
    }

    fn index_size_error(&mut self, method: &str) -> JsError {
        self.dom_exception(
            DomExceptionName::IndexSize,
            format!("{method}: offset is greater than the length of the data"),
        )
    }

    /// `Node.prototype.normalize()` (DOM §4.4).
    pub(in crate::runtime) fn node_normalize(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
    ) -> Result<JsValue, JsError> {
        let node = self.require_node(receiver)?;
        dom.normalize(node)?;
        Ok(JsValue::Undefined)
    }

    /// `Text.prototype.splitText(offset)` (DOM §4.11.1). The receiver must be a
    /// Text node, and the new node holding the rest of the data is returned.
    pub(in crate::runtime) fn text_split_text(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let node = self.require_node(receiver)?;
        if !matches!(
            dom.node(node).map(render_dom::Node::kind),
            Some(NodeKind::Text(_))
        ) {
            return Err(JsError::type_error("incompatible Text method receiver"));
        }
        let offset = self.offset_argument(dom, arguments, 0, "splitText")?;
        let rest = dom.split_text(node, offset)?;
        self.wrap_node(dom, rest)
    }
}

/// The data of `node` as UTF-16 code units.
fn character_units(dom: &Dom, node: NodeId) -> Vec<u16> {
    character_data(dom, node)
        .map(|data| data.encode_utf16().collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::character_data_length;
    use render_dom::Dom;

    #[test]
    fn length_counts_utf16_code_units_not_bytes_or_chars() {
        let mut dom = Dom::new();
        // U+1F600 is one code point, two UTF-16 code units, and four UTF-8 bytes.
        let text = dom.create_text("a\u{1F600}b");
        assert_eq!(character_data_length(&dom, text), Some(4));
        let element = dom.create_element("p");
        assert_eq!(character_data_length(&dom, element), None);
    }
}
