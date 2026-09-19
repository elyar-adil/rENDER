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

//! Network surface: `fetch()`, `Response`, and the classic `XMLHttpRequest`
//! subset.
//!
//! The runtime never performs I/O. Every transfer is queued as a
//! [`PendingFetch`](crate::runtime::types::PendingFetch); the embedding
//! drains them through `JsRuntime::take_pending_fetch_requests`, executes
//! them on its own transport, and completes each id through
//! `JsRuntime::settle_fetch`, which resolves the `fetch()` promise or
//! completes the XHR instance.

use render_dom::Dom;
use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::runtime::builtins::json::JsonParser;
use crate::runtime::convert::required_argument;
use crate::runtime::types::FetchOutcome;
use crate::runtime::types::JsMicrotask;
use crate::runtime::types::PendingFetch;
use crate::value::ErrorKind;
use crate::value::NativeFunction;
use crate::value::ObjectHost;
use crate::value::XhrResponse;
use crate::value::XmlHttpRequestState;
use url::Url;

impl JsRuntime {
    pub(in crate::runtime) fn dispatch_fetch_native(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match function {
            NativeFunction::GlobalFetch => self.perform_fetch(arguments),
            NativeFunction::ResponseText => self.response_text(receiver),
            NativeFunction::ResponseJson => self.response_json(receiver),
            NativeFunction::ResponseHeadersGet => self.response_headers_get(receiver, arguments),
            NativeFunction::XhrOpen => self.xhr_open(receiver, arguments),
            NativeFunction::XhrSetRequestHeader => self.xhr_set_request_header(receiver, arguments),
            NativeFunction::XhrSend => self.xhr_send(receiver, arguments),
            NativeFunction::XhrGetResponseHeader => {
                self.xhr_get_response_header(receiver, arguments)
            }
            other => self.dispatch_dom_native(dom, other, receiver, arguments),
        }
    }

    /// `fetch(input, init)`: queue one transfer and return the promise that
    /// `settle_fetch` resolves. String URLs and Request-like objects with a
    /// `url` property are accepted; relative URLs resolve against the
    /// document base.
    fn perform_fetch(&mut self, arguments: &[JsValue]) -> Result<JsValue, JsError> {
        let input = required_argument(arguments, 0, "fetch")?;
        let requested = match input {
            JsValue::Object(object) => self
                .realm
                .get_property(*object, "url")
                .map(|value| value.to_js_string())
                .filter(|value| !value.is_empty()),
            other => Some(other.to_js_string()),
        };
        let mut method = "GET".to_owned();
        let mut headers = Vec::new();
        let mut body = None;
        if let Some(JsValue::Object(init)) = arguments.get(1) {
            if let Some(value) = self.realm.get_property(*init, "method")
                && !matches!(value, JsValue::Undefined)
            {
                method = value.to_js_string().to_ascii_uppercase();
            }
            if method.is_empty() || !method.bytes().all(is_http_token_byte) {
                return Err(JsError::type_error(format!(
                    "fetch requires a valid HTTP method, got {method:?}"
                )));
            }
            if let Some(JsValue::Object(header_object)) = self.realm.get_property(*init, "headers")
            {
                for (name, value) in self
                    .realm
                    .enumerable_own_properties(header_object)
                    .unwrap_or_default()
                {
                    if matches!(value, JsValue::Undefined | JsValue::Null) {
                        continue;
                    }
                    headers.push((name, value.to_js_string()));
                }
            }
            if let Some(value) = self.realm.get_property(*init, "body")
                && !matches!(value, JsValue::Undefined | JsValue::Null)
            {
                body = Some(value.to_js_string());
            }
        }
        let (promise, value) = self.create_promise()?;
        let Some(url) = requested else {
            let reason = JsValue::String("fetch requires a string URL".to_owned());
            self.reject_promise(promise, &reason);
            return Ok(value);
        };
        match self.resolve_fetch_url(&url) {
            Ok(resolved) => {
                let id = self.next_fetch_id;
                self.next_fetch_id += 1;
                self.pending_fetch_requests.push(PendingFetch {
                    id,
                    url: resolved.to_string(),
                    method,
                    headers,
                    body,
                });
                self.pending_fetch_promises.insert(id, promise);
                if let JsValue::Object(promise_object) = value {
                    self.pending_fetch_targets.insert(id, promise_object);
                }
            }
            Err(message) => {
                let reason = self
                    .construct_standard_error(
                        ErrorKind::TypeError,
                        &format!("fetch URL {url:?} is invalid: {message}"),
                    )
                    .unwrap_or(JsValue::String(message));
                self.reject_promise(promise, &reason);
            }
        }
        Ok(value)
    }

    /// Resolve a request URL string against the document base
    /// (`location.href`); without a base only absolute URLs are accepted.
    pub(in crate::runtime) fn resolve_fetch_url(&self, requested: &str) -> Result<Url, String> {
        match self.document_base_url() {
            Some(base) => base.join(requested).map_err(|error| error.to_string()),
            None => Url::parse(requested).map_err(|error| error.to_string()),
        }
    }

    /// The committed document URL behind the `location` global, if any.
    pub(in crate::runtime) fn document_base_url(&self) -> Option<Url> {
        let location = self
            .realm
            .get_property(self.realm.global_object(), "location")?;
        match location {
            JsValue::Object(object) => match self.realm.host(object) {
                Some(ObjectHost::Location(url)) => Some(url),
                _ => None,
            },
            _ => None,
        }
    }

    /// Materialize a `Response` instance for a successful transfer.
    pub(in crate::runtime) fn build_response_value(
        &mut self,
        outcome: &FetchOutcome,
    ) -> Result<JsValue, JsError> {
        let constructor = match self.realm.global("Response") {
            Some(JsValue::Object(constructor)) => constructor,
            _ => return Err(JsError::type_error("Response constructor is unavailable")),
        };
        let prototype = self.constructor_prototype(constructor)?;
        self.ensure_heap_capacity(3)?;
        let response = self.realm.create_object(Some(prototype));
        self.install_response_state(
            response,
            outcome.status,
            outcome.status_text.clone(),
            outcome.headers.clone(),
            String::from_utf8_lossy(&outcome.body).into_owned(),
        );
        Ok(JsValue::Object(response))
    }

    /// `new Response(body)`: a fulfilled response without a transfer.
    pub(in crate::runtime) fn response_constructor(
        &mut self,
        constructor: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let prototype = self.constructor_prototype(constructor)?;
        let body = match arguments.first() {
            None | Some(JsValue::Undefined | JsValue::Null) => String::new(),
            Some(value) => value.to_js_string(),
        };
        self.ensure_heap_capacity(3)?;
        let response = self.realm.create_object(Some(prototype));
        self.install_response_state(response, 200, "OK".to_owned(), Vec::new(), body);
        Ok(JsValue::Object(response))
    }

    /// Attach host state and script-visible properties to a fresh `Response`.
    fn install_response_state(
        &mut self,
        response: ObjectId,
        status: u16,
        status_text: String,
        headers: Vec<(String, String)>,
        body: String,
    ) {
        for (name, value) in [
            ("status", JsValue::Number(f64::from(status))),
            ("statusText", JsValue::String(status_text.clone())),
            ("ok", JsValue::Boolean((200..300).contains(&status))),
            ("type", JsValue::String("basic".to_owned())),
            ("bodyUsed", JsValue::Boolean(false)),
            ("url", JsValue::String(String::new())),
        ] {
            self.realm.set_property(response, name.to_owned(), value);
        }
        *self
            .realm
            .host_mut(response)
            .expect("newly created Response has host storage") = ObjectHost::Response {
            status,
            status_text,
            headers,
            body,
        };
        let headers_object = self.realm.create_ordinary_object();
        *self
            .realm
            .host_mut(headers_object)
            .expect("newly created Headers has host storage") =
            ObjectHost::ResponseHeaders { owner: response };
        let headers_get = self.realm.native_object(NativeFunction::ResponseHeadersGet);
        self.realm.set_property(
            headers_object,
            "get".to_owned(),
            JsValue::Object(headers_get),
        );
        self.realm.set_property(
            response,
            "headers".to_owned(),
            JsValue::Object(headers_object),
        );
    }

    fn response_text(&mut self, receiver: ObjectId) -> Result<JsValue, JsError> {
        let body = match self.realm.host(receiver) {
            Some(ObjectHost::Response { body, .. }) => body.clone(),
            _ => return Err(JsError::type_error("incompatible Response method receiver")),
        };
        // The body is already available, so the promise settles immediately;
        // reaction callbacks still run at the microtask checkpoint.
        let (promise, value) = self.create_promise()?;
        self.resolve_promise(promise, &JsValue::String(body));
        Ok(value)
    }

    fn response_json(&mut self, receiver: ObjectId) -> Result<JsValue, JsError> {
        let body = match self.realm.host(receiver) {
            Some(ObjectHost::Response { body, .. }) => body.clone(),
            _ => return Err(JsError::type_error("incompatible Response method receiver")),
        };
        let (promise, value) = self.create_promise()?;
        match JsonParser::new(&body).parse() {
            Ok(node) => match self.json_node_to_value(node) {
                Ok(parsed) => self.resolve_promise(promise, &parsed),
                Err(error) => {
                    let reason = JsValue::String(error.to_string());
                    self.reject_promise(promise, &reason);
                }
            },
            Err(message) => {
                let reason = self
                    .construct_standard_error(
                        ErrorKind::SyntaxError,
                        &format!("Failed to parse response body as JSON: {message}"),
                    )
                    .unwrap_or(JsValue::String(message));
                self.reject_promise(promise, &reason);
            }
        }
        Ok(value)
    }

    fn response_headers_get(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let name = required_argument(arguments, 0, "get")?.to_js_string();
        let owner = match self.realm.host(receiver) {
            Some(ObjectHost::ResponseHeaders { owner }) => owner,
            _ => return Err(JsError::type_error("incompatible Headers method receiver")),
        };
        let headers = match self.realm.host(owner) {
            Some(ObjectHost::Response { headers, .. }) => headers.clone(),
            _ => return Ok(JsValue::Null),
        };
        Ok(find_header(&headers, &name).map_or(JsValue::Null, JsValue::String))
    }

    /// `new XMLHttpRequest()`: an instance in the UNSENT state.
    pub(in crate::runtime) fn xml_http_request_constructor(
        &mut self,
        constructor: ObjectId,
    ) -> Result<JsValue, JsError> {
        let prototype = self.constructor_prototype(constructor)?;
        self.ensure_heap_capacity(1)?;
        let instance = self.realm.create_object(Some(prototype));
        *self
            .realm
            .host_mut(instance)
            .expect("newly created XMLHttpRequest has host storage") =
            ObjectHost::XmlHttpRequest(XmlHttpRequestState::default());
        for (name, value) in [
            ("readyState", JsValue::Number(0.0)),
            ("status", JsValue::Number(0.0)),
            ("statusText", JsValue::String(String::new())),
            ("responseType", JsValue::String(String::new())),
            ("response", JsValue::String(String::new())),
            ("responseText", JsValue::String(String::new())),
            ("withCredentials", JsValue::Boolean(false)),
            ("onreadystatechange", JsValue::Undefined),
            ("onload", JsValue::Undefined),
            ("onerror", JsValue::Undefined),
            ("onloadend", JsValue::Undefined),
        ] {
            self.realm.set_property(instance, name.to_owned(), value);
        }
        Ok(JsValue::Object(instance))
    }

    fn xhr_open(&mut self, receiver: ObjectId, arguments: &[JsValue]) -> Result<JsValue, JsError> {
        let method = required_argument(arguments, 0, "open")?.to_js_string();
        let requested = required_argument(arguments, 1, "open")?.to_js_string();
        if method.is_empty() || !method.bytes().all(is_http_token_byte) {
            return Err(JsError::type_error(format!(
                "XMLHttpRequest.open requires a valid HTTP method, got {method:?}"
            )));
        }
        // `open(method, url)` and `open(method, url, true)` are async; only
        // an explicit falsy flag selects the unsupported synchronous path.
        let async_request = match arguments.get(2) {
            None | Some(JsValue::Undefined) => true,
            Some(value) => value.is_truthy(),
        };
        let resolved = self.resolve_fetch_url(&requested).map_err(|message| {
            JsError::type_error(format!(
                "XMLHttpRequest.open URL {requested:?} is invalid: {message}"
            ))
        })?;
        match self.realm.host_mut(receiver) {
            Some(ObjectHost::XmlHttpRequest(state)) => {
                *state = XmlHttpRequestState {
                    method: method.to_ascii_uppercase(),
                    url: resolved.to_string(),
                    headers: Vec::new(),
                    async_request,
                    sent: false,
                    response: None,
                };
            }
            _ => {
                return Err(JsError::type_error(
                    "incompatible XMLHttpRequest method receiver",
                ));
            }
        }
        self.realm
            .set_property(receiver, "readyState".to_owned(), JsValue::Number(1.0));
        Ok(JsValue::Undefined)
    }

    fn xhr_set_request_header(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let name = required_argument(arguments, 0, "setRequestHeader")?.to_js_string();
        let value = required_argument(arguments, 1, "setRequestHeader")?.to_js_string();
        if name.is_empty() || !name.bytes().all(is_http_token_byte) {
            return Err(JsError::type_error(format!(
                "setRequestHeader requires a valid header name, got {name:?}"
            )));
        }
        match self.realm.host_mut(receiver) {
            Some(ObjectHost::XmlHttpRequest(state)) if !state.method.is_empty() && !state.sent => {
                state.headers.push((name, value));
            }
            Some(ObjectHost::XmlHttpRequest(_)) => {
                return Err(JsError::type_error(
                    "setRequestHeader requires an opened, unsent XMLHttpRequest",
                ));
            }
            _ => {
                return Err(JsError::type_error(
                    "incompatible XMLHttpRequest method receiver",
                ));
            }
        }
        Ok(JsValue::Undefined)
    }

    fn xhr_send(&mut self, receiver: ObjectId, arguments: &[JsValue]) -> Result<JsValue, JsError> {
        let state = match self.realm.host(receiver) {
            Some(ObjectHost::XmlHttpRequest(state)) => state.clone(),
            _ => {
                return Err(JsError::type_error(
                    "incompatible XMLHttpRequest method receiver",
                ));
            }
        };
        if state.method.is_empty() {
            return Err(JsError::type_error(
                "XMLHttpRequest.send requires open() to run first",
            ));
        }
        if state.sent {
            return Err(JsError::type_error(
                "XMLHttpRequest.send may run once per open()",
            ));
        }
        if !state.async_request {
            return Err(JsError::type_error(
                "synchronous XMLHttpRequest is not supported",
            ));
        }
        let body = match arguments.first() {
            None | Some(JsValue::Undefined | JsValue::Null) => None,
            Some(value) => Some(value.to_js_string()),
        };
        let id = self.next_fetch_id;
        self.next_fetch_id += 1;
        self.pending_fetch_requests.push(PendingFetch {
            id,
            url: state.url,
            method: state.method,
            headers: state.headers,
            body,
        });
        self.pending_fetch_targets.insert(id, receiver);
        Ok(JsValue::Undefined)
    }

    fn xhr_get_response_header(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let name = required_argument(arguments, 0, "getResponseHeader")?.to_js_string();
        let headers = match self.realm.host(receiver) {
            Some(ObjectHost::XmlHttpRequest(state)) => state
                .response
                .as_ref()
                .map(|response| response.headers.clone())
                .unwrap_or_default(),
            _ => {
                return Err(JsError::type_error(
                    "incompatible XMLHttpRequest method receiver",
                ));
            }
        };
        Ok(find_header(&headers, &name).map_or(JsValue::Null, JsValue::String))
    }

    /// Transition one XHR to readyState 4 and queue its completion callbacks
    /// as microtasks. Transport failures keep status 0 and fire `error`.
    pub(in crate::runtime) fn complete_xml_http_request(
        &mut self,
        receiver: ObjectId,
        outcome: Result<FetchOutcome, String>,
    ) {
        let (status, status_text, headers, body) = match outcome {
            Ok(outcome) => (
                outcome.status,
                outcome.status_text,
                outcome.headers,
                String::from_utf8_lossy(&outcome.body).into_owned(),
            ),
            Err(_) => (0, String::new(), Vec::new(), String::new()),
        };
        if let Some(ObjectHost::XmlHttpRequest(state)) = self.realm.host_mut(receiver) {
            state.response = Some(XhrResponse {
                status,
                status_text: status_text.clone(),
                headers: headers.clone(),
                body: body.clone(),
            });
        }
        for (name, value) in [
            ("readyState", JsValue::Number(4.0)),
            ("status", JsValue::Number(f64::from(status))),
            ("statusText", JsValue::String(status_text)),
            ("responseText", JsValue::String(body.clone())),
            ("response", JsValue::String(body)),
        ] {
            self.realm.set_property(receiver, name.to_owned(), value);
        }
        let failed = status == 0;
        let events: [&str; 3] = if failed {
            ["readystatechange", "error", "loadend"]
        } else {
            ["readystatechange", "load", "loadend"]
        };
        for event_type in events {
            self.queue_xhr_event(receiver, event_type);
        }
    }

    /// Queue one `on<type>` XHR callback as a microtask bound to the XHR
    /// instance, with a minimal event object as its argument.
    fn queue_xhr_event(&mut self, receiver: ObjectId, event_type: &str) {
        let Some(JsValue::Object(callback)) = self
            .realm
            .get_property(receiver, &format!("on{event_type}"))
        else {
            return;
        };
        if !Self::is_callable_object(callback, &self.realm) {
            return;
        }
        if self.ensure_heap_capacity(2).is_err() {
            return;
        }
        let event = self.realm.create_ordinary_object();
        for (name, value) in [
            ("type", JsValue::String(event_type.to_owned())),
            ("target", JsValue::Object(receiver)),
            ("currentTarget", JsValue::Object(receiver)),
        ] {
            self.realm.set_property(event, name.to_owned(), value);
        }
        let bound = self.realm.bound_callable(
            callback,
            JsValue::Object(receiver),
            vec![JsValue::Object(event)],
        );
        self.pending_microtasks.push(JsMicrotask::Callback(bound));
    }

    fn constructor_prototype(&self, constructor: ObjectId) -> Result<ObjectId, JsError> {
        match self.realm.get_property(constructor, "prototype") {
            Some(JsValue::Object(prototype)) => Ok(prototype),
            _ => Err(JsError::type_error(
                "constructor is missing its prototype object",
            )),
        }
    }
}

/// Case-insensitive response header lookup returning the first match.
fn find_header(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(header, _)| header.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone())
}

/// RFC 9110 token-character test used for methods and header names.
const fn is_http_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}
