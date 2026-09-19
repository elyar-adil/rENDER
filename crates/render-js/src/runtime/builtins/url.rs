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
use crate::value::NativeFunction;
use crate::value::ObjectHost;
use std::fmt::Write as _;
use url::Url;

impl JsRuntime {
    pub(in crate::runtime) fn dispatch_url_native(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match function {
            NativeFunction::UrlToString => Ok(self.url_to_string(&JsValue::Object(receiver))),
            NativeFunction::UrlSearchParamsGet
            | NativeFunction::UrlSearchParamsHas
            | NativeFunction::UrlSearchParamsSet
            | NativeFunction::UrlSearchParamsAppend
            | NativeFunction::UrlSearchParamsToString
            | NativeFunction::UrlSearchParamsForEach => Ok(self.url_search_params_method(
                &JsValue::Object(receiver),
                function,
                arguments,
                dom,
            )),
            other => self.dispatch_style_native(dom, other, receiver, arguments),
        }
    }
}

impl JsRuntime {
    pub(in crate::runtime) fn url_constructor(
        &mut self,
        constructor: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let input = required_argument(arguments, 0, "URL")?.to_js_string();
        let base = self.realm.global("location").and_then(|value| match value {
            JsValue::Object(object) => match self.realm.host(object) {
                Some(ObjectHost::Location(url)) => Some(url),
                _ => None,
            },
            _ => None,
        });
        let parsed = Url::parse(&input)
            .or_else(|_| {
                base.as_ref()
                    .ok_or(url::ParseError::EmptyHost)
                    .and_then(|base| base.join(&input))
            })
            .map_err(|_| JsError::type_error("Invalid URL"))?;
        let prototype = self
            .realm
            .get_property(constructor, "prototype")
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            });
        self.ensure_heap_capacity(2)?;
        let instance = self.realm.create_object(prototype);
        let params = self.realm.create_object(None);
        if let Some(object) = self.realm.object_mut(params) {
            object.host = ObjectHost::UrlSearchParams {
                pairs: parsed
                    .query()
                    .unwrap_or_default()
                    .split('&')
                    .filter(|part| !part.is_empty())
                    .map(|part| {
                        let mut pieces = part.splitn(2, '=');
                        (
                            pieces.next().unwrap_or_default().to_owned(),
                            pieces.next().unwrap_or_default().to_owned(),
                        )
                    })
                    .collect(),
                owner: Some(instance),
            };
        }
        if let Some(object) = self.realm.object_mut(instance) {
            object.host = ObjectHost::UrlInstance(parsed.clone());
        }
        for (name, value) in crate::value::location_components(&parsed) {
            self.realm
                .set_property(instance, name.to_owned(), JsValue::String(value));
        }
        self.realm
            .set_property(instance, "searchParams".to_owned(), JsValue::Object(params));
        Ok(JsValue::Object(instance))
    }

    pub(in crate::runtime) fn url_search_params_constructor(
        &mut self,
        constructor: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let input = arguments
            .first()
            .filter(|value| !matches!(value, JsValue::Undefined | JsValue::Null))
            .map(JsValue::to_js_string)
            .unwrap_or_default();
        let pairs = input
            .trim_start_matches('?')
            .split('&')
            .filter(|part| !part.is_empty())
            .map(|part| {
                let mut pieces = part.splitn(2, '=');
                (
                    pieces.next().unwrap_or_default().to_owned(),
                    pieces.next().unwrap_or_default().to_owned(),
                )
            })
            .collect();
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
            *host = ObjectHost::UrlSearchParams { pairs, owner: None };
        }
        Ok(JsValue::Object(object))
    }

    pub(in crate::runtime) fn url_to_string(&self, receiver: &JsValue) -> JsValue {
        match receiver {
            JsValue::Object(object) => match self.realm.host(*object) {
                Some(ObjectHost::UrlInstance(url)) => JsValue::String(url.to_string()),
                _ => JsValue::String(receiver.to_js_string()),
            },
            _ => JsValue::String(receiver.to_js_string()),
        }
    }

    pub(in crate::runtime) fn url_search_params_method(
        &mut self,
        receiver: &JsValue,
        function: NativeFunction,
        arguments: &[JsValue],
        dom: &mut Dom,
    ) -> JsValue {
        let JsValue::Object(object) = *receiver else {
            return JsValue::Undefined;
        };
        let Some(ObjectHost::UrlSearchParams { mut pairs, owner }) = self.realm.host(object) else {
            return JsValue::Undefined;
        };
        let key = arguments
            .first()
            .map(JsValue::to_js_string)
            .unwrap_or_default();
        let value = arguments
            .get(1)
            .map(JsValue::to_js_string)
            .unwrap_or_default();
        let result = match function {
            NativeFunction::UrlSearchParamsGet => pairs
                .iter()
                .find(|(name, _)| name == &key)
                .map_or(JsValue::Null, |(_, value)| JsValue::String(value.clone())),
            NativeFunction::UrlSearchParamsHas => {
                JsValue::Boolean(pairs.iter().any(|(name, _)| name == &key))
            }
            NativeFunction::UrlSearchParamsToString => JsValue::String(
                pairs
                    .iter()
                    .map(|(name, value)| format!("{name}={value}"))
                    .collect::<Vec<_>>()
                    .join("&"),
            ),
            NativeFunction::UrlSearchParamsSet => {
                if let Some((_, existing)) = pairs.iter_mut().find(|(name, _)| name == &key) {
                    *existing = value;
                } else {
                    pairs.push((key, value));
                }
                JsValue::Object(object)
            }
            NativeFunction::UrlSearchParamsAppend => {
                pairs.push((key, value));
                JsValue::Object(object)
            }
            NativeFunction::UrlSearchParamsForEach => {
                if let Some(JsValue::Object(callback)) = arguments.first() {
                    for (name, value) in &pairs {
                        let _ = self.call_with_this(
                            dom,
                            *callback,
                            &[
                                JsValue::String(value.clone()),
                                JsValue::String(name.clone()),
                            ],
                            JsValue::Object(object),
                        );
                    }
                }
                JsValue::Undefined
            }
            _ => JsValue::Undefined,
        };
        if matches!(
            function,
            NativeFunction::UrlSearchParamsSet | NativeFunction::UrlSearchParamsAppend
        ) {
            if let Some(host) = self.realm.host_mut(object) {
                *host = ObjectHost::UrlSearchParams {
                    pairs: pairs.clone(),
                    owner,
                };
            }
            if let Some(owner) = owner {
                if let Some(ObjectHost::UrlInstance(mut url)) = self.realm.host(owner) {
                    let query = pairs
                        .iter()
                        .map(|(name, value)| format!("{name}={value}"))
                        .collect::<Vec<_>>()
                        .join("&");
                    url.set_query(Some(&query));
                    if let Some(host) = self.realm.host_mut(owner) {
                        *host = ObjectHost::UrlInstance(url.clone());
                    }
                    self.realm.set_property(
                        owner,
                        "href".to_owned(),
                        JsValue::String(url.to_string()),
                    );
                    self.realm.set_property(
                        owner,
                        "search".to_owned(),
                        JsValue::String(format!("?{query}")),
                    );
                }
            }
        }
        result
    }

    /// Percent-encode per RFC 3986; `keep_uri` preserves reserved characters.
    pub(in crate::runtime) fn percent_encode(text: &str, keep_uri: bool) -> String {
        let mut output = String::with_capacity(text.len());
        for byte in text.bytes() {
            let keep = byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')'
                )
                || (keep_uri
                    && matches!(
                        byte,
                        b';' | b'/' | b'?' | b':' | b'@' | b'&' | b'=' | b'+' | b'$' | b',' | b'#'
                    ));
            if keep {
                output.push(byte as char);
            } else {
                let _ = write!(output, "%{byte:02X}");
            }
        }
        output
    }

    /// Decode `%XX` sequences; returns `None` on malformed input.
    pub(in crate::runtime) fn percent_decode(text: &str) -> Option<String> {
        let bytes: Vec<u8> = text.bytes().collect();
        let mut decoded = Vec::with_capacity(bytes.len());
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] == b'%' && index + 2 < bytes.len() {
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok()?;
                decoded.push(u8::from_str_radix(hex, 16).ok()?);
                index += 3;
            } else {
                decoded.push(bytes[index]);
                index += 1;
            }
        }
        String::from_utf8(decoded).ok()
    }
}
