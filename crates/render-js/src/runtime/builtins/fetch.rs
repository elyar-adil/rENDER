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

use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::runtime::builtins::dom_exception::DomExceptionName;
use crate::runtime::builtins::json::JsonParser;
use crate::runtime::convert::required_argument;
use crate::runtime::types::FetchOutcome;
use crate::runtime::types::JsMicrotask;
use crate::runtime::types::PendingFetch;
use crate::value::ErrorKind;
use crate::value::NativeFunction;
use crate::value::ObjectHost;
use crate::value::PropertyDescriptor;
use crate::value::XhrResponse;
use crate::value::XmlHttpRequestState;
use render_dom::Dom;
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
            NativeFunction::AbortControllerAbort => {
                self.abort_controller_abort(dom, receiver, arguments)
            }
            NativeFunction::FormDataAppend
            | NativeFunction::FormDataGet
            | NativeFunction::FormDataSet
            | NativeFunction::FormDataHas
            | NativeFunction::FormDataDelete
            | NativeFunction::FormDataEntries => {
                self.form_data_method(dom, function, receiver, arguments)
            }
            NativeFunction::ResponseText => self.response_text(receiver),
            NativeFunction::ResponseJson => self.response_json(receiver),
            NativeFunction::ResponseHeadersGet => self.response_headers_get(receiver, arguments),
            NativeFunction::BlobText
            | NativeFunction::BlobArrayBuffer
            | NativeFunction::BlobSlice => {
                self.dispatch_blob_native(dom, function, receiver, arguments)
            }
            NativeFunction::XhrOpen => self.xhr_open(receiver, arguments),
            NativeFunction::XhrSetRequestHeader => self.xhr_set_request_header(receiver, arguments),
            NativeFunction::XhrSend => self.xhr_send(receiver, arguments),
            NativeFunction::XhrGetResponseHeader => {
                self.xhr_get_response_header(receiver, arguments)
            }
            NativeFunction::XhrGetAllResponseHeaders => self.xhr_get_all_response_headers(receiver),
            NativeFunction::XhrAddEventListener => self.xhr_add_event_listener(receiver, arguments),
            NativeFunction::XhrRemoveEventListener => {
                self.xhr_remove_event_listener(receiver, arguments)
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
        let mut signal = None;
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
                body = Some(self.request_body(&value, &mut headers));
            }
            if let Some(JsValue::Object(candidate)) = self.realm.get_property(*init, "signal")
                && matches!(self.realm.host(candidate), Some(ObjectHost::AbortSignal))
            {
                signal = Some(candidate);
            }
        }
        let (promise, value) = self.create_promise()?;
        if let Some(signal) = signal {
            if self
                .realm
                .get_property(signal, "aborted")
                .is_some_and(|value| value.is_truthy())
            {
                let reason = self
                    .realm
                    .get_property(signal, "reason")
                    .filter(|value| !matches!(value, JsValue::Undefined))
                    .unwrap_or_else(|| self.abort_reason());
                self.reject_promise(promise, &reason);
                return Ok(value);
            }
            if let JsValue::Object(promise_object) = value {
                self.realm.define_property(
                    promise_object,
                    "__renderAbortSignal",
                    PropertyDescriptor {
                        value: JsValue::Object(signal),
                        writable: false,
                        enumerable: false,
                        configurable: false,
                        getter: None,
                        setter: None,
                    },
                );
            }
        }
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

    pub(in crate::runtime) fn abort_controller_constructor(
        &mut self,
        constructor: ObjectId,
    ) -> Result<JsValue, JsError> {
        let prototype = self.constructor_prototype(constructor)?;
        self.ensure_heap_capacity(2)?;
        let controller = self.realm.create_object(Some(prototype));
        let signal = self.realm.create_ordinary_object();
        *self
            .realm
            .host_mut(controller)
            .expect("new controller has host") = ObjectHost::AbortController;
        *self.realm.host_mut(signal).expect("new signal has host") = ObjectHost::AbortSignal;
        self.realm
            .set_property(signal, "aborted".to_owned(), JsValue::Boolean(false));
        self.realm
            .set_property(signal, "reason".to_owned(), JsValue::Undefined);
        self.realm
            .set_property(controller, "signal".to_owned(), JsValue::Object(signal));
        Ok(JsValue::Object(controller))
    }

    fn abort_controller_abort(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        if !matches!(self.realm.host(receiver), Some(ObjectHost::AbortController)) {
            return Err(JsError::type_error("incompatible AbortController receiver"));
        }
        let Some(JsValue::Object(signal)) = self.realm.get_property(receiver, "signal") else {
            return Err(JsError::type_error("AbortController signal is missing"));
        };
        if self
            .realm
            .get_property(signal, "aborted")
            .is_some_and(|value| value.is_truthy())
        {
            return Ok(JsValue::Undefined);
        }
        let reason = arguments
            .first()
            .filter(|value| !matches!(value, JsValue::Undefined))
            .cloned()
            .unwrap_or_else(|| self.abort_reason());
        self.realm
            .set_property(signal, "aborted".to_owned(), JsValue::Boolean(true));
        self.realm
            .set_property(signal, "reason".to_owned(), reason.clone());
        let aborted: Vec<_> = self
            .pending_fetch_targets
            .iter()
            .filter_map(|(id, target)| {
                (self.realm.get_property(*target, "__renderAbortSignal")
                    == Some(JsValue::Object(signal)))
                .then_some(*id)
            })
            .collect();
        for id in aborted {
            self.pending_fetch_requests
                .retain(|request| request.id != id);
            self.pending_fetch_targets.remove(&id);
            if let Some(promise) = self.pending_fetch_promises.remove(&id) {
                self.reject_promise(promise, &reason);
            }
        }
        if let Some(JsValue::Object(callback)) = self.realm.get_property(signal, "onabort")
            && Self::is_callable_object(callback, &self.realm)
        {
            self.call_with_this(dom, callback, &[], JsValue::Object(signal))?;
        }
        Ok(JsValue::Undefined)
    }

    /// The rejection reason for an aborted request. Fetch Standard §"abort a
    /// fetch": "reject promise with an `AbortError` `DOMException`", so this is
    /// now a real one: `e.name`, `e.code` (20) and `e instanceof DOMException`
    /// all answer, and `AbortSignal`'s `onabort` sees the same object a
    /// `fetch` catch block does.
    fn abort_reason(&mut self) -> JsValue {
        self.construct_dom_exception(DomExceptionName::Abort, "The user aborted a request.")
            .unwrap_or_else(|_| {
                JsValue::String("AbortError: The user aborted a request.".to_owned())
            })
    }

    pub(in crate::runtime) fn form_data_constructor(
        &mut self,
        constructor: ObjectId,
    ) -> Result<JsValue, JsError> {
        let prototype = self.constructor_prototype(constructor)?;
        self.ensure_heap_capacity(1)?;
        let object = self.realm.create_object(Some(prototype));
        *self.realm.host_mut(object).expect("new form has host") = ObjectHost::FormData {
            entries: Vec::new(),
        };
        Ok(JsValue::Object(object))
    }

    fn form_data_method(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let name = if function == NativeFunction::FormDataEntries {
            String::new()
        } else {
            required_argument(arguments, 0, "FormData method")?.to_js_string()
        };
        let value = if matches!(
            function,
            NativeFunction::FormDataAppend | NativeFunction::FormDataSet
        ) {
            Some(required_argument(arguments, 1, "FormData method")?.to_js_string())
        } else {
            None
        };
        let Some(ObjectHost::FormData { entries }) = self.realm.host_mut(receiver) else {
            return Err(JsError::type_error("incompatible FormData receiver"));
        };
        match function {
            NativeFunction::FormDataAppend => {
                entries.push((name, value.expect("append value")));
                Ok(JsValue::Undefined)
            }
            NativeFunction::FormDataSet => {
                let first = entries.iter().position(|(key, _)| key == &name);
                entries.retain(|(key, _)| key != &name);
                let index = first.unwrap_or(entries.len()).min(entries.len());
                entries.insert(index, (name, value.expect("set value")));
                Ok(JsValue::Undefined)
            }
            NativeFunction::FormDataGet => Ok(entries
                .iter()
                .find(|(key, _)| key == &name)
                .map_or(JsValue::Null, |(_, value)| JsValue::String(value.clone()))),
            NativeFunction::FormDataHas => Ok(JsValue::Boolean(
                entries.iter().any(|(key, _)| key == &name),
            )),
            NativeFunction::FormDataDelete => {
                entries.retain(|(key, _)| key != &name);
                Ok(JsValue::Undefined)
            }
            NativeFunction::FormDataEntries => {
                let pairs = entries.clone();
                let mut rows = Vec::with_capacity(pairs.len());
                for (key, value) in pairs {
                    let pair = self.create_array_from_values(&[
                        JsValue::String(key),
                        JsValue::String(value),
                    ])?;
                    rows.push(JsValue::Object(pair));
                }
                let list = self.create_array_from_values(&rows)?;
                let values = self
                    .realm
                    .get_property(list, "values")
                    .ok_or_else(|| JsError::type_error("Array iterator is unavailable"))?;
                let method = Self::require_callable_object(&values, &self.realm)?;
                self.call_with_this(dom, method, &[], JsValue::Object(list))
            }
            _ => unreachable!("FormData native dispatch"),
        }
    }

    fn request_body(&self, value: &JsValue, headers: &mut Vec<(String, String)>) -> String {
        let JsValue::Object(object) = value else {
            return value.to_js_string();
        };
        let entries = match self.realm.host(*object) {
            Some(ObjectHost::FormData { entries }) => entries,
            Some(ObjectHost::UrlSearchParams { pairs, .. }) => {
                if !headers
                    .iter()
                    .any(|(name, _)| name.eq_ignore_ascii_case("content-type"))
                {
                    headers.push((
                        "Content-Type".to_owned(),
                        "application/x-www-form-urlencoded;charset=UTF-8".to_owned(),
                    ));
                }
                return url::form_urlencoded::Serializer::new(String::new())
                    .extend_pairs(
                        pairs
                            .iter()
                            .map(|(name, value)| (name.as_str(), value.as_str())),
                    )
                    .finish();
            }
            _ => return value.to_js_string(),
        };
        let boundary = "----render-browser-form-boundary";
        if !headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        {
            headers.push((
                "Content-Type".to_owned(),
                format!("multipart/form-data; boundary={boundary}"),
            ));
        }
        let mut body = String::new();
        for (name, value) in entries {
            body.push_str("--");
            body.push_str(boundary);
            body.push_str("\r\nContent-Disposition: form-data; name=\"");
            body.push_str(
                &name
                    .replace('"', "%22")
                    .replace('\r', "%0D")
                    .replace('\n', "%0A"),
            );
            body.push_str("\"\r\n\r\n");
            body.push_str(&value);
            body.push_str("\r\n");
        }
        body.push_str("--");
        body.push_str(boundary);
        body.push_str("--\r\n");
        body
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
            ("responseURL", JsValue::String(String::new())),
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
        for (name, value) in [
            ("readyState", JsValue::Number(1.0)),
            ("status", JsValue::Number(0.0)),
            ("statusText", JsValue::String(String::new())),
            ("response", JsValue::String(String::new())),
            ("responseText", JsValue::String(String::new())),
            ("responseURL", JsValue::String(String::new())),
        ] {
            self.realm.set_property(receiver, name.to_owned(), value);
        }
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
        let mut headers = state.headers;
        let body = match arguments.first() {
            None | Some(JsValue::Undefined | JsValue::Null) => None,
            Some(value) => Some(self.request_body(value, &mut headers)),
        };
        let id = self.next_fetch_id;
        self.next_fetch_id += 1;
        self.pending_fetch_requests.push(PendingFetch {
            id,
            url: state.url,
            method: state.method,
            headers,
            body,
        });
        if let Some(ObjectHost::XmlHttpRequest(state)) = self.realm.host_mut(receiver) {
            state.sent = true;
        }
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

    fn xhr_get_all_response_headers(&self, receiver: ObjectId) -> Result<JsValue, JsError> {
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
        let mut result = String::new();
        for (name, value) in headers {
            result.push_str(&name);
            result.push_str(": ");
            result.push_str(&value);
            result.push_str("\r\n");
        }
        Ok(JsValue::String(result))
    }

    fn xhr_listeners(&mut self, receiver: ObjectId) -> Result<ObjectId, JsError> {
        if !matches!(
            self.realm.host(receiver),
            Some(ObjectHost::XmlHttpRequest(_))
        ) {
            return Err(JsError::type_error(
                "incompatible XMLHttpRequest method receiver",
            ));
        }
        if let Some(JsValue::Object(listeners)) =
            self.realm.get_property(receiver, "__renderXhrListeners")
        {
            return Ok(listeners);
        }
        self.ensure_heap_capacity(1)?;
        let listeners = self.realm.create_ordinary_object();
        self.realm.define_property(
            receiver,
            "__renderXhrListeners",
            PropertyDescriptor {
                value: JsValue::Object(listeners),
                writable: false,
                enumerable: false,
                configurable: false,
                getter: None,
                setter: None,
            },
        );
        Ok(listeners)
    }

    fn xhr_event_callbacks(
        &mut self,
        listeners: ObjectId,
        event_type: &str,
    ) -> Result<Vec<ObjectId>, JsError> {
        let Some(JsValue::Object(array)) = self.realm.get_property(listeners, event_type) else {
            return Ok(Vec::new());
        };
        Ok(self
            .array_elements_for(array)?
            .into_iter()
            .filter_map(|value| match value {
                JsValue::Object(callback) => Some(callback),
                _ => None,
            })
            .collect())
    }

    fn xhr_add_event_listener(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let event_type = required_argument(arguments, 0, "addEventListener")?.to_js_string();
        let listeners = self.xhr_listeners(receiver)?;
        let Some(callback_value) = arguments.get(1) else {
            return Ok(JsValue::Undefined);
        };
        if matches!(callback_value, JsValue::Null | JsValue::Undefined) {
            return Ok(JsValue::Undefined);
        }
        let callback = Self::require_callable_object(callback_value, &self.realm)?;
        let mut callbacks = self.xhr_event_callbacks(listeners, &event_type)?;
        if !callbacks.contains(&callback) {
            callbacks.push(callback);
            let values: Vec<_> = callbacks.into_iter().map(JsValue::Object).collect();
            let array = self.create_array_from_values(&values)?;
            self.realm
                .set_property(listeners, event_type, JsValue::Object(array));
        }
        Ok(JsValue::Undefined)
    }

    fn xhr_remove_event_listener(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let event_type = required_argument(arguments, 0, "removeEventListener")?.to_js_string();
        let listeners = self.xhr_listeners(receiver)?;
        let Some(JsValue::Object(callback)) = arguments.get(1) else {
            return Ok(JsValue::Undefined);
        };
        let mut callbacks = self.xhr_event_callbacks(listeners, &event_type)?;
        callbacks.retain(|candidate| *candidate != *callback);
        let values: Vec<_> = callbacks.into_iter().map(JsValue::Object).collect();
        let array = self.create_array_from_values(&values)?;
        self.realm
            .set_property(listeners, event_type, JsValue::Object(array));
        Ok(JsValue::Undefined)
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
        let request_url =
            if let Some(ObjectHost::XmlHttpRequest(state)) = self.realm.host_mut(receiver) {
                state.response = Some(XhrResponse {
                    status,
                    status_text: status_text.clone(),
                    headers: headers.clone(),
                    body: body.clone(),
                });
                state.sent = false;
                state.url.clone()
            } else {
                String::new()
            };
        let response_type = self
            .realm
            .get_property(receiver, "responseType")
            .map(|value| value.to_js_string())
            .unwrap_or_default();
        let response_value = if status != 0 && response_type == "json" {
            match JsonParser::new(&body).parse() {
                Ok(node) => self.json_node_to_value(node).unwrap_or(JsValue::Null),
                Err(_) => JsValue::Null,
            }
        } else {
            JsValue::String(body.clone())
        };
        for (name, value) in [
            ("readyState", JsValue::Number(4.0)),
            ("status", JsValue::Number(f64::from(status))),
            ("statusText", JsValue::String(status_text)),
            ("responseText", JsValue::String(body.clone())),
            ("response", response_value),
            ("responseURL", JsValue::String(request_url)),
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
        let listeners = self
            .realm
            .get_property(receiver, "__renderXhrListeners")
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            });
        let mut callbacks = listeners
            .and_then(|list| self.xhr_event_callbacks(list, event_type).ok())
            .unwrap_or_default();
        if let Some(JsValue::Object(handler)) = self
            .realm
            .get_property(receiver, &format!("on{event_type}"))
            && Self::is_callable_object(handler, &self.realm)
        {
            callbacks.push(handler);
        }
        if callbacks.is_empty() {
            return;
        }
        if self
            .ensure_heap_capacity(callbacks.len().saturating_add(1))
            .is_err()
        {
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
        for callback in callbacks {
            let bound = self.realm.bound_callable(
                callback,
                JsValue::Object(receiver),
                vec![JsValue::Object(event)],
            );
            self.pending_microtasks.push(JsMicrotask::Callback(bound));
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use render_html::parse_document;

    fn drain(runtime: &mut JsRuntime, dom: &mut Dom) {
        for _ in 0..16 {
            let tasks = runtime.take_pending_microtasks();
            if tasks.is_empty() {
                break;
            }
            for task in tasks {
                runtime
                    .invoke_microtask(dom, task)
                    .expect("microtask should execute");
            }
        }
    }

    #[test]
    fn real_site_xhr_json_response_headers_and_load_listener() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        runtime.execute(&mut parsed.dom, r"
            var seen = [];
            var xhr = new XMLHttpRequest();
            function removed() { seen.push('removed'); }
            xhr.addEventListener('load', removed);
            xhr.removeEventListener('load', removed);
            xhr.addEventListener('load', function (event) {
                seen.push(event.target === xhr);
                seen.push(xhr.response.data.id);
                seen.push(xhr.getAllResponseHeaders().indexOf('Content-Type: application/json') >= 0);
                seen.push(xhr.responseURL);
            });
            xhr.open('GET', 'https://example.test/data');
            xhr.responseType = 'json';
            xhr.send();
            try { xhr.send(); } catch (error) { seen.push('duplicate-blocked'); }
        ").expect("XHR setup should execute");
        let requests = runtime.take_pending_fetch_requests();
        assert_eq!(requests.len(), 1);
        runtime.settle_fetch(
            &mut parsed.dom,
            requests[0].id,
            Ok(FetchOutcome {
                status: 200,
                status_text: "OK".to_owned(),
                headers: vec![("Content-Type".to_owned(), "application/json".to_owned())],
                body: br#"{"data":{"id":17}}"#.to_vec(),
            }),
        );
        drain(&mut runtime, &mut parsed.dom);
        let observed = runtime
            .execute(&mut parsed.dom, "seen.join('|')")
            .expect("result read should execute")
            .value;
        assert_eq!(
            observed,
            JsValue::String("duplicate-blocked|true|17|true|https://example.test/data".to_owned())
        );
    }

    #[test]
    fn form_data_serializes_a_post_body() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let result = runtime
            .execute(
                &mut parsed.dom,
                r"
            var form = new FormData();
            form.append('aid', '123');
            form.append('csrf', 'token');
            form.set('aid', '456');
            fetch('https://example.test/save', {method:'POST', body:form});
            form.get('aid') + ':' + form.has('csrf') + ':' + form.entries().next().value[0];
        ",
            )
            .expect("FormData request should execute");
        assert_eq!(result.value, JsValue::String("456:true:aid".to_owned()));
        let requests = runtime.take_pending_fetch_requests();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert!(
            request
                .headers
                .iter()
                .any(|(name, value)| name.eq_ignore_ascii_case("content-type")
                    && value.starts_with("multipart/form-data; boundary="))
        );
        let body = request.body.as_deref().expect("multipart body");
        assert!(body.contains("name=\"aid\"\r\n\r\n456\r\n"));
        assert!(body.contains("name=\"csrf\"\r\n\r\ntoken\r\n"));
    }

    #[test]
    fn abort_controller_rejects_and_cancels_queued_fetch() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        runtime
            .execute(
                &mut parsed.dom,
                r"
            var reason = '';
            var controller = new AbortController();
            fetch('https://example.test/slow', {signal:controller.signal})
                .catch(function (error) { reason = error.name; });
            controller.abort();
            var customReason = '';
            var alreadyAborted = new AbortController();
            alreadyAborted.abort('cancelled by caller');
            fetch('https://example.test/never', {signal:alreadyAborted.signal})
                .catch(function (error) { customReason = error; });
        ",
            )
            .expect("abort should execute");
        assert!(runtime.take_pending_fetch_requests().is_empty());
        drain(&mut runtime, &mut parsed.dom);
        let result = runtime
            .execute(
                &mut parsed.dom,
                "reason + ':' + controller.signal.aborted + ':' + customReason",
            )
            .expect("abort result should be readable");
        assert_eq!(
            result.value,
            JsValue::String("AbortError:true:cancelled by caller".to_owned())
        );
    }
}
