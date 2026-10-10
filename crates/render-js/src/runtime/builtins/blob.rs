#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::too_many_lines
)]

//! Bounded File API `Blob` support used by media players and application
//! bootstrap code. The browser embedding still owns I/O; this module only
//! materializes bytes already present in script values.

use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::runtime::convert::to_number;
use crate::value::{NativeFunction, ObjectHost, TypedArrayKind};
use render_dom::Dom;

const MAX_BLOB_BYTES: usize = 16 * 1024 * 1024;

impl JsRuntime {
    pub(in crate::runtime) fn dispatch_blob_native(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match function {
            NativeFunction::BlobText => self.blob_text(receiver),
            NativeFunction::BlobArrayBuffer => self.blob_array_buffer(receiver),
            NativeFunction::BlobSlice => self.blob_slice(receiver, arguments),
            other => self.dispatch_url_native(dom, other, receiver, arguments),
        }
    }

    pub(in crate::runtime) fn blob_constructor(
        &mut self,
        dom: &mut Dom,
        constructor: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let mut bytes = Vec::new();
        if let Some(JsValue::Object(parts)) = arguments.first() {
            let length = self.array_like_length(dom, *parts)?;
            for index in 0..length {
                let part = self.get_member(dom, *parts, &index.to_string())?;
                self.append_blob_part(dom, &part, &mut bytes)?;
                if bytes.len() > MAX_BLOB_BYTES {
                    return Err(JsError::resource("Blob exceeds the engine byte limit"));
                }
            }
        } else if let Some(value) = arguments.first() {
            self.append_blob_part(dom, value, &mut bytes)?;
        }
        let content_type = arguments
            .get(1)
            .and_then(|value| match value {
                JsValue::Object(options) => self.realm.get_property(*options, "type"),
                _ => None,
            })
            .filter(|value| !matches!(value, JsValue::Undefined))
            .map_or_else(String::new, |value| value.to_js_string())
            .to_ascii_lowercase();
        let prototype = self
            .realm
            .get_property(constructor, "prototype")
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            });
        self.ensure_heap_capacity(1)?;
        let object = self.realm.create_object(prototype);
        if let Some(host) = self.realm.host_mut(object) {
            *host = ObjectHost::Blob {
                bytes,
                content_type,
            };
        }
        Ok(JsValue::Object(object))
    }

    fn append_blob_part(
        &mut self,
        dom: &mut Dom,
        value: &JsValue,
        bytes: &mut Vec<u8>,
    ) -> Result<(), JsError> {
        match value {
            JsValue::String(text) => bytes.extend_from_slice(text.as_bytes()),
            JsValue::Object(object) => match self.realm.host(*object) {
                Some(ObjectHost::Blob { bytes: part, .. }) => bytes.extend_from_slice(&part),
                Some(ObjectHost::TypedArray {
                    kind,
                    buffer,
                    start,
                    length,
                }) => bytes.extend(buffer.view_bytes(kind.element_size(), start, length)?),
                _ => {
                    let length = self.array_like_length(dom, *object)?;
                    if length > 0 {
                        for index in 0..length {
                            let item = self.get_member(dom, *object, &index.to_string())?;
                            let number = to_number(&item)?;
                            bytes.push(if number.is_finite() {
                                number.clamp(0.0, 255.0) as u8
                            } else {
                                0
                            });
                        }
                    } else {
                        bytes.extend_from_slice(value.to_js_string().as_bytes());
                    }
                }
            },
            _ => bytes.extend_from_slice(value.to_js_string().as_bytes()),
        }
        Ok(())
    }

    fn blob_bytes(&self, receiver: ObjectId) -> Result<(Vec<u8>, String), JsError> {
        match self.realm.host(receiver) {
            Some(ObjectHost::Blob {
                bytes,
                content_type,
            }) => Ok((bytes.clone(), content_type.clone())),
            _ => Err(JsError::type_error(
                "Blob method called on an incompatible receiver",
            )),
        }
    }

    fn blob_text(&mut self, receiver: ObjectId) -> Result<JsValue, JsError> {
        let (bytes, _) = self.blob_bytes(receiver)?;
        let (promise, value) = self.create_promise()?;
        self.resolve_promise(
            promise,
            &JsValue::String(String::from_utf8_lossy(&bytes).into_owned()),
        );
        Ok(value)
    }

    fn blob_array_buffer(&mut self, receiver: ObjectId) -> Result<JsValue, JsError> {
        let (bytes, _) = self.blob_bytes(receiver)?;
        let constructor = self
            .realm
            .global("Uint8Array")
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            })
            .ok_or_else(|| JsError::type_error("Uint8Array is unavailable"))?;
        let prototype = self
            .realm
            .get_property(constructor, "prototype")
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            });
        let values = bytes
            .iter()
            .map(|byte| f64::from(*byte))
            .collect::<Vec<_>>();
        let array =
            self.create_typed_array_from_values(TypedArrayKind::Uint8, &values, prototype)?;
        let (promise, value) = self.create_promise()?;
        self.resolve_promise(promise, &array);
        Ok(value)
    }

    fn blob_slice(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let (bytes, _) = self.blob_bytes(receiver)?;
        let len = bytes.len() as i64;
        let start = blob_index(arguments.first(), len, 0);
        let end = blob_index(arguments.get(1), len, len);
        let start = start as usize;
        let end = end.max(start as i64) as usize;
        let content_type = arguments
            .get(2)
            .map(JsValue::to_js_string)
            .unwrap_or_default()
            .to_ascii_lowercase();
        self.ensure_heap_capacity(1)?;
        let prototype = self.realm.get_prototype(receiver);
        let object = self.realm.create_object(prototype);
        if let Some(host) = self.realm.host_mut(object) {
            *host = ObjectHost::Blob {
                bytes: bytes[start..end.min(bytes.len())].to_vec(),
                content_type,
            };
        }
        Ok(JsValue::Object(object))
    }
}

fn blob_index(value: Option<&JsValue>, len: i64, default: i64) -> i64 {
    let Some(value) = value else { return default };
    let number = match value {
        JsValue::Number(number) => *number,
        _ => value.to_js_string().parse::<f64>().unwrap_or(0.0),
    };
    if !number.is_finite() {
        return if number.is_sign_negative() { 0 } else { len };
    }
    let integer = number.trunc() as i64;
    if integer < 0 {
        (len + integer).max(0)
    } else {
        integer.min(len)
    }
}

#[cfg(test)]
mod tests {
    use super::blob_index;

    #[test]
    fn blob_indices_follow_slice_defaults_and_negative_offsets() {
        assert_eq!(blob_index(None, 10, 0), 0);
        assert_eq!(blob_index(None, 10, 10), 10);
        assert_eq!(blob_index(Some(&crate::JsValue::Number(-3.0)), 10, 0), 7);
        assert_eq!(blob_index(Some(&crate::JsValue::Number(99.0)), 10, 0), 10);
    }
}
