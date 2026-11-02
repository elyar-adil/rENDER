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
use crate::value::NativeFunction;
use crate::value::ObjectHost;
use render_dom::Dom;
use url::Url;
use url::form_urlencoded;

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
            NativeFunction::UrlParse => self.url_parse(dom, arguments),
            NativeFunction::UrlCanParse => self.url_can_parse(dom, arguments),
            NativeFunction::UrlCreateObjectUrl => self.url_create_object_url(arguments),
            NativeFunction::UrlRevokeObjectUrl => Ok(JsValue::Undefined),
            NativeFunction::UrlSearchParamsGet
            | NativeFunction::UrlSearchParamsGetAll
            | NativeFunction::UrlSearchParamsHas
            | NativeFunction::UrlSearchParamsSet
            | NativeFunction::UrlSearchParamsAppend
            | NativeFunction::UrlSearchParamsDelete
            | NativeFunction::UrlSearchParamsSort
            | NativeFunction::UrlSearchParamsToString
            | NativeFunction::UrlSearchParamsForEach
            | NativeFunction::UrlSearchParamsKeys
            | NativeFunction::UrlSearchParamsValues
            | NativeFunction::UrlSearchParamsEntries
            | NativeFunction::UrlSearchParamsSize => {
                self.url_search_params_method(&JsValue::Object(receiver), function, arguments, dom)
            }
            other => self.dispatch_style_native(dom, other, receiver, arguments),
        }
    }
}

impl JsRuntime {
    /// Return a data URL for Blob values. The embedding treats data URLs as
    /// ordinary resources, so this keeps `URL.createObjectURL(blob)` useful
    /// for image/video elements without adding a second transport registry.
    fn url_create_object_url(&mut self, arguments: &[JsValue]) -> Result<JsValue, JsError> {
        let object = match arguments.first() {
            Some(JsValue::Object(object)) => *object,
            _ => return Err(JsError::type_error("createObjectURL requires a Blob")),
        };
        let (bytes, content_type) = match self.realm.host(object) {
            Some(ObjectHost::Blob {
                bytes,
                content_type,
            }) => (bytes.clone(), content_type.clone()),
            _ => return Err(JsError::type_error("createObjectURL requires a Blob")),
        };
        let mime = if content_type.is_empty() {
            "application/octet-stream"
        } else {
            &content_type
        };
        Ok(JsValue::String(
            (format!("data:{mime};base64,{}", base64_encode(&bytes))).into(),
        ))
    }

    pub(in crate::runtime) fn url_constructor(
        &mut self,
        dom: &mut Dom,
        constructor: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let parsed = self
            .resolve_url(dom, arguments, true)?
            .ok_or_else(|| JsError::type_error("Invalid URL"))?;
        let prototype = self
            .realm
            .get_property(constructor, "prototype")
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            });
        self.create_url_instance(&parsed, prototype)
    }

    /// `URL.parse(input, base)` (the URL parser static method): `null` where
    /// the constructor would throw. It never installs a base URL of its own,
    /// so a relative input without a base is not parseable.
    fn url_parse(&mut self, dom: &mut Dom, arguments: &[JsValue]) -> Result<JsValue, JsError> {
        match self.resolve_url(dom, arguments, false)? {
            Some(url) => {
                let prototype = self.url_prototype();
                self.create_url_instance(&url, prototype)
            }
            None => Ok(JsValue::Null),
        }
    }

    /// `URL.canParse(input, base)`: whether `URL.parse` would return a URL.
    fn url_can_parse(&mut self, dom: &mut Dom, arguments: &[JsValue]) -> Result<JsValue, JsError> {
        Ok(JsValue::Boolean(
            self.resolve_url(dom, arguments, false)?.is_some(),
        ))
    }

    fn url_prototype(&mut self) -> Option<ObjectId> {
        self.realm
            .global("URL")
            .and_then(|value| match value {
                JsValue::Object(constructor) => self.realm.get_property(constructor, "prototype"),
                _ => None,
            })
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            })
    }

    /// The parsed URL for the constructor's `(input, base)` arguments. The
    /// constructor supplies the document URL as the implicit base when the
    /// input is relative and no base argument was given; `URL.parse` does not.
    fn resolve_url(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
        require_result: bool,
    ) -> Result<Option<Url>, JsError> {
        // WHATWG URL: both arguments are converted with ToString, so a `URL`
        // object (or any object with `toString`) supplies its string form.
        let input = self
            .to_string_value(dom, required_argument(arguments, 0, "URL")?)?
            .to_lossy();
        let explicit_base = match arguments.get(1) {
            None | Some(JsValue::Undefined) => None,
            Some(value) => Some(self.to_string_value(dom, value)?.to_lossy()),
        };
        if let Some(base) = explicit_base {
            // An explicit base is parsed before the input; an invalid one makes
            // the result unparseable even when the input is absolute.
            return Ok(Url::parse(&base)
                .ok()
                .and_then(|base| Url::parse(&input).ok().or_else(|| base.join(&input).ok())));
        }
        if require_result {
            return Ok(Url::parse(&input).ok().or_else(|| {
                self.document_url()
                    .and_then(|base| Url::parse(&base).ok())
                    .and_then(|base| base.join(&input).ok())
            }));
        }
        Ok(Url::parse(&input).ok())
    }

    fn document_url(&self) -> Option<String> {
        self.realm.global("location").and_then(|value| match value {
            JsValue::Object(object) => match self.realm.host(object) {
                Some(ObjectHost::Location(url)) => Some(url.to_string()),
                _ => None,
            },
            _ => None,
        })
    }

    fn create_url_instance(
        &mut self,
        parsed: &Url,
        prototype: Option<ObjectId>,
    ) -> Result<JsValue, JsError> {
        let search_prototype = self.url_search_params_prototype();
        self.ensure_heap_capacity(2)?;
        let instance = self.realm.create_object(prototype);
        let params = self.realm.create_object(search_prototype);
        if let Some(object) = self.realm.object_mut(params) {
            object.host = ObjectHost::UrlSearchParams {
                pairs: parse_form_pairs(parsed.query().unwrap_or_default()),
                owner: Some(instance),
            };
        }
        if let Some(object) = self.realm.object_mut(instance) {
            object.host = ObjectHost::UrlInstance(parsed.clone());
        }
        for (name, value) in crate::value::location_components(parsed) {
            self.realm
                .set_property(instance, name.to_owned(), JsValue::String((value).into()));
        }
        self.realm
            .set_property(instance, "searchParams".to_owned(), JsValue::Object(params));
        Ok(JsValue::Object(instance))
    }

    fn url_search_params_prototype(&mut self) -> Option<ObjectId> {
        self.realm
            .global("URLSearchParams")
            .and_then(|value| match value {
                JsValue::Object(constructor) => self.realm.get_property(constructor, "prototype"),
                _ => None,
            })
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            })
    }

    pub(in crate::runtime) fn url_search_params_constructor(
        &mut self,
        dom: &mut Dom,
        constructor: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let pairs = match arguments.first() {
            None | Some(JsValue::Undefined | JsValue::Null) => Vec::new(),
            Some(JsValue::String(input)) => parse_form_pairs(&input.to_lossy()),
            Some(JsValue::Object(object)) => self.url_search_params_init(dom, *object)?,
            Some(value) => parse_form_pairs(&value.to_js_string().to_lossy()),
        };
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

    /// The conversion steps of `new URLSearchParams(init)` for an object: an
    /// existing parameter list is copied, an iterable is read as pair
    /// sequences, and anything else is a record of name/value members (WHATWG
    /// URL §6.2).
    fn url_search_params_init(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
    ) -> Result<Vec<(String, String)>, JsError> {
        if let Some(ObjectHost::UrlSearchParams { pairs, .. }) = self.realm.host(object) {
            return Ok(pairs.clone());
        }
        if let Some((iterator, next)) = self.get_iterator(dom, &JsValue::Object(object))? {
            let pinned = self.transient_roots.len();
            self.transient_roots.extend([iterator, next]);
            let mut pairs = Vec::new();
            while let Some(step) = self.iterator_next(dom, iterator, next)? {
                let JsValue::Object(item) = step else {
                    self.transient_roots.truncate(pinned);
                    return Err(JsError::type_error(
                        "Each query pair must be an object with two elements",
                    ));
                };
                let name = self.get_member(dom, item, "0")?.to_js_string();
                let value = self.get_member(dom, item, "1")?.to_js_string();
                pairs.push((name.to_lossy(), value.to_lossy()));
            }
            self.transient_roots.truncate(pinned);
            return Ok(pairs);
        }
        let keys = self.own_string_keys(dom, object)?;
        let mut pairs = Vec::with_capacity(keys.len());
        for key in keys {
            let value = self.get_member(dom, object, &key)?.to_js_string();
            pairs.push((key, value.to_lossy()));
        }
        Ok(pairs)
    }

    pub(in crate::runtime) fn url_to_string(&self, receiver: &JsValue) -> JsValue {
        match receiver {
            JsValue::Object(object) => match self.realm.host(*object) {
                Some(ObjectHost::UrlInstance(url)) => JsValue::String(url.to_string().into()),
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
    ) -> Result<JsValue, JsError> {
        let JsValue::Object(object) = *receiver else {
            return Err(JsError::type_error(
                "URLSearchParams method called on an incompatible receiver",
            ));
        };
        let Some(ObjectHost::UrlSearchParams { mut pairs, owner }) = self.realm.host(object) else {
            return Err(JsError::type_error(
                "URLSearchParams method called on an incompatible receiver",
            ));
        };
        // WHATWG URL takes name and value as USVString, so a lone surrogate
        // becomes U+FFFD before it is stored.
        let key = arguments
            .first()
            .map(|value| value.to_js_string().to_lossy())
            .unwrap_or_default();
        let value = arguments
            .get(1)
            .map(|value| value.to_js_string().to_lossy());
        let mut mutated = false;
        let result = match function {
            NativeFunction::UrlSearchParamsGet => pairs
                .iter()
                .find(|(name, _)| name == &key)
                .map_or(JsValue::Null, |(_, value)| {
                    JsValue::String(value.as_str().into())
                }),
            NativeFunction::UrlSearchParamsGetAll => {
                let values: Vec<JsValue> = pairs
                    .iter()
                    .filter(|(name, _)| name == &key)
                    .map(|(_, value)| JsValue::String(value.as_str().into()))
                    .collect();
                JsValue::Object(self.create_array_from_values(&values)?)
            }
            NativeFunction::UrlSearchParamsHas => {
                JsValue::Boolean(pairs.iter().any(|(name, v)| {
                    name == &key && value.as_ref().is_none_or(|wanted| v == wanted)
                }))
            }
            NativeFunction::UrlSearchParamsToString => {
                JsValue::String(serialize_pairs(&pairs).into())
            }
            NativeFunction::UrlSearchParamsSize => JsValue::Number(pairs.len() as f64),
            NativeFunction::UrlSearchParamsSet => {
                let value = value.unwrap_or_default();
                if let Some(first) = pairs.iter().position(|(name, _)| name == &key) {
                    pairs[first].1 = value;
                    // `set` keeps the first occurrence and drops later ones
                    // with the same name (WHATWG URL §6.2).
                    let mut seen = false;
                    pairs.retain(|(name, _)| {
                        if name != &key {
                            return true;
                        }
                        if seen {
                            false
                        } else {
                            seen = true;
                            true
                        }
                    });
                } else {
                    pairs.push((key.clone(), value));
                }
                mutated = true;
                JsValue::Object(object)
            }
            NativeFunction::UrlSearchParamsAppend => {
                pairs.push((key.clone(), value.unwrap_or_default()));
                mutated = true;
                JsValue::Object(object)
            }
            NativeFunction::UrlSearchParamsDelete => {
                match value {
                    Some(wanted) => {
                        pairs.retain(|(name, value)| !(name == &key && value == &wanted));
                    }
                    None => pairs.retain(|(name, _)| name != &key),
                }
                mutated = true;
                JsValue::Undefined
            }
            NativeFunction::UrlSearchParamsSort => {
                pairs.sort_by(|left, right| left.0.encode_utf16().cmp(right.0.encode_utf16()));
                mutated = true;
                JsValue::Undefined
            }
            NativeFunction::UrlSearchParamsForEach => {
                let Some(JsValue::Object(callback)) = arguments.first() else {
                    return Err(JsError::type_error("forEach requires a callback function"));
                };
                if !Self::is_callable_object(*callback, &self.realm) {
                    return Err(JsError::type_error("forEach requires a callback function"));
                }
                let this_argument = arguments.get(1).cloned().unwrap_or(JsValue::Undefined);
                let snapshot = pairs.clone();
                for (name, value) in snapshot {
                    self.call_with_this(
                        dom,
                        *callback,
                        &[
                            JsValue::String(value.into()),
                            JsValue::String(name.into()),
                            JsValue::Object(object),
                        ],
                        this_argument.clone(),
                    )?;
                }
                JsValue::Undefined
            }
            NativeFunction::UrlSearchParamsKeys => {
                let values: Vec<JsValue> = pairs
                    .iter()
                    .map(|(name, _)| JsValue::String(name.as_str().into()))
                    .collect();
                JsValue::Object(self.realm.collection_iterator(values))
            }
            NativeFunction::UrlSearchParamsValues => {
                let values: Vec<JsValue> = pairs
                    .iter()
                    .map(|(_, value)| JsValue::String(value.as_str().into()))
                    .collect();
                JsValue::Object(self.realm.collection_iterator(values))
            }
            NativeFunction::UrlSearchParamsEntries => {
                let mut values = Vec::with_capacity(pairs.len());
                for (name, value) in &pairs {
                    let pair = self.create_array_from_values(&[
                        JsValue::String(name.as_str().into()),
                        JsValue::String(value.as_str().into()),
                    ])?;
                    values.push(JsValue::Object(pair));
                }
                JsValue::Object(self.realm.collection_iterator(values))
            }
            _ => JsValue::Undefined,
        };
        if mutated {
            if let Some(host) = self.realm.host_mut(object) {
                *host = ObjectHost::UrlSearchParams {
                    pairs: pairs.clone(),
                    owner,
                };
            }
            if let Some(owner) = owner {
                sync_url_from_pairs(&mut self.realm, owner, &pairs);
            }
        }
        Ok(result)
    }
}

fn parse_form_pairs(input: &str) -> Vec<(String, String)> {
    // The `URLSearchParams` constructor drops one leading `?`, while a URL's
    // query component never carries one; both reach this parser.
    let input = input.strip_prefix('?').unwrap_or(input);
    form_urlencoded::parse(input.as_bytes())
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect()
}

fn serialize_pairs(pairs: &[(String, String)]) -> String {
    let mut serializer = form_urlencoded::Serializer::new(String::new());
    for (name, value) in pairs {
        serializer.append_pair(name, value);
    }
    serializer.finish()
}

/// Keep a `URL`'s `href`, `search` and internal URL in step with its live
/// `searchParams` object after a mutating method (WHATWG URL §6.1).
fn sync_url_from_pairs(
    realm: &mut crate::value::Realm,
    owner: ObjectId,
    pairs: &[(String, String)],
) {
    let Some(ObjectHost::UrlInstance(url)) = realm.host(owner) else {
        return;
    };
    let mut url = url;
    let query = serialize_pairs(pairs);
    if query.is_empty() {
        url.set_query(None);
    } else {
        url.set_query(Some(&query));
    }
    let href = url.to_string();
    let search = url
        .query()
        .map_or_else(String::new, |query| format!("?{query}"));
    if let Some(host) = realm.host_mut(owner) {
        *host = ObjectHost::UrlInstance(url);
    }
    realm.set_property(owner, "href".to_owned(), JsValue::String(href.into()));
    realm.set_property(owner, "search".to_owned(), JsValue::String(search.into()));
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        output.push(TABLE[(first >> 2) as usize] as char);
        output.push(TABLE[((first & 3) << 4 | second >> 4) as usize] as char);
        output.push(if chunk.len() > 1 {
            TABLE[((second & 15) << 2 | third >> 6) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            TABLE[(third & 63) as usize] as char
        } else {
            '='
        });
    }
    output
}
