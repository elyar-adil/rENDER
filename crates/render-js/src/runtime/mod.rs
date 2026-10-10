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
use crate::JsErrorKind;
use crate::JsSymbol;
use crate::JsValue;
use crate::ObjectId;
use crate::Realm;
use crate::RuntimeLimits;
use crate::ScriptOutcome;
use crate::parser::Statement;
use crate::runtime::convert::required_argument;
use crate::runtime::eval::Completion;
use crate::runtime::types::CallFrame;
use crate::runtime::types::ClassFrame;
use crate::runtime::types::Environment;
use crate::runtime::types::GlobalBinding;
use crate::runtime::types::PromiseRecord;
use crate::runtime::types::RegexRecord;
use crate::runtime::types::UserFunction;
use crate::value::ErrorKind;
use crate::value::NativeFunction;
use crate::value::ObjectHost;
use render_dom::Dom;
use render_dom::DomError;
use render_dom::DomRevision;
use render_dom::NodeId;
use std::collections::BTreeMap;
use url::Url;

mod async_iteration;
#[cfg(test)]
mod bigint_tests;
mod builtins;
mod class;
mod convert;
mod coroutine;
mod coroutine_run;
#[cfg(test)]
mod coroutine_tests;
#[cfg(test)]
mod dom_tests;
mod eval;
#[cfg(test)]
mod event_tests;
mod gc;
#[cfg(test)]
mod language_tests;
mod module;
#[cfg(test)]
mod module_tests;
#[cfg(test)]
mod prelude_tests;
mod types;

#[cfg(test)]
mod tests;

pub use types::{
    ConsoleLevel, ConsoleMessage, DocumentReadyState, ElementRect, FetchOutcome, HistoryRequest,
    JsMicrotask, NavigationRequest, PendingFetch, TimerEntry, TimerKind, TimerRequest,
};

/// Whether a history change may point `new` at the document URL `current`.
///
/// Tuple origins compare exactly. A `file:` document has an opaque origin in
/// the URL model, yet browsers let its script move between files, so every
/// `file:` URL shares one origin here. Other opaque origins match nothing.
fn same_history_origin(new: &Url, current: &Url) -> bool {
    if new.scheme() != current.scheme() {
        return false;
    }
    match (new.origin(), current.origin()) {
        (url::Origin::Tuple(..), url::Origin::Tuple(..)) => new.origin() == current.origin(),
        _ => new.scheme() == "file",
    }
}

/// A realm-owning interpreter instance. DOM wrappers retain stable `NodeId`
/// identities, never Rust references, across calls to [`Self::execute`].
#[derive(Debug)]
pub struct JsRuntime {
    realm: Realm,
    /// Functions embedders defined with `define_host_function`, indexed by the
    /// `NativeFunction::Host` that calls them.
    host_functions: Vec<HostFunction>,
    limits: RuntimeLimits,
    steps_remaining: usize,
    calls_active: usize,
    dom_nodes_created: usize,
    environment: Vec<Environment>,
    /// Scopes of callers whose frames are suspended while a callee runs. They
    /// are held here, not only in the caller's Rust locals, so a collection
    /// during the call still sees them.
    suspended_environments: Vec<Environment>,
    /// Declared modules by key (the module's absolute URL).
    modules: BTreeMap<String, module::ModuleRecord>,
    /// Whether the self-hosted built-ins in `prelude.js` have been installed.
    prelude_installed: bool,
    /// Labels written directly in front of the loop about to run.
    pending_loop_labels: Vec<String>,
    /// Generator and async activations, by id; `None` while one is running.
    coroutines: Vec<Option<coroutine_run::Coroutine>>,
    /// Compiled coroutine bodies, by function index.
    coroutine_code: BTreeMap<usize, std::rc::Rc<coroutine::CoroutineCode>>,
    /// Request queues and states of the async generators, by coroutine index.
    async_generators: async_iteration::AsyncGenerators,
    /// The error the prelude failed with, if it did; a test pins this to `None`.
    prelude_error: Option<JsError>,
    functions: Vec<UserFunction>,
    /// Class context of each active user-function call, for `super`,
    /// `new.target`, and field initialization.
    class_frames: Vec<ClassFrame>,
    /// `new.target` of each active construction, `Undefined` for plain calls.
    new_target_stack: Vec<JsValue>,
    /// Set across the `construct` of a `super()` call; see `dispatch_new_target`.
    super_call_pending: bool,
    /// Arrays being stringified right now, so a cycle ends instead of recursing.
    arrays_joining: Vec<ObjectId>,
    /// Counter allocating class-unique private-name ids.
    next_private_id: u64,
    promises: Vec<PromiseRecord>,
    /// The `alreadyResolved` flag of each resolving-function pair, indexed by
    /// the pair number a settlement function carries.
    promise_pairs_resolved: Vec<bool>,
    pending_microtasks: Vec<JsMicrotask>,
    event_listeners: BTreeMap<NodeId, BTreeMap<String, Vec<types::Listener>>>,
    /// Per-event dispatch state, present only while the event is dispatched.
    event_flags: BTreeMap<ObjectId, types::EventFlags>,
    event_handlers: BTreeMap<NodeId, BTreeMap<String, ObjectId>>,
    global_bindings: BTreeMap<String, GlobalBinding>,
    timers: BTreeMap<u64, TimerEntry>,
    next_timer_id: u64,
    pending_timer_requests: Vec<TimerRequest>,
    ready_state: DocumentReadyState,
    pending_history_requests: Vec<HistoryRequest>,
    pending_navigations: Vec<NavigationRequest>,
    /// Network transfers queued by `fetch()`/`XMLHttpRequest`, drained by the
    /// embedding through [`Self::take_pending_fetch_requests`].
    pending_fetch_requests: Vec<PendingFetch>,
    /// Fetch id -> promise record index for promise-returning transfers.
    /// Removed on settle so re-fetching ids cannot leak entries.
    pending_fetch_promises: BTreeMap<u64, usize>,
    /// Fetch id -> object the GC must keep alive until settle (the promise
    /// object for `fetch()`, the XHR instance for `XMLHttpRequest`).
    pending_fetch_targets: BTreeMap<u64, ObjectId>,
    next_fetch_id: u64,
    regexes: Vec<RegexRecord>,
    /// Compiled patterns by `(source, flags)`, so a regex literal evaluated
    /// again (in a loop, or a function called repeatedly) is not compiled again.
    regex_cache: std::collections::HashMap<(String, String), std::rc::Rc<crate::regex::Compiled>>,
    console_messages: Vec<ConsoleMessage>,
    window_event_handlers: BTreeMap<String, Vec<types::Listener>>,
    next_symbol_id: u64,
    /// `Symbol.for` registry: registry key -> symbol id.
    global_symbol_registry: BTreeMap<String, u64>,
    /// Active JavaScript call frames for stack traces and diagnostics.
    call_stack: Vec<CallFrame>,
    /// Objects a `ToObject` call boxed that are still referenced only from the
    /// interpreter's own Rust frames, not from any script-visible slot. The
    /// collector treats them as roots so a collection triggered by a later
    /// allocation in the same builtin cannot tombstone a live wrapper.
    transient_roots: Vec<ObjectId>,
    /// Byte offsets where each source line starts, for error positioning.
    source_line_starts: Vec<usize>,
    /// JS-visible cookies (`document.cookie`), keyed by name.
    js_cookie_jar: BTreeMap<String, String>,
    random_state: u64,
    element_geometry: BTreeMap<u64, ElementRect>,
    viewport: ElementRect,
    intersection_observers: Vec<ObjectId>,
    mutation_observers: Vec<ObjectId>,
    /// Every `MediaQueryList` this realm has produced, kept so a viewport change
    /// can re-evaluate them. The list is a *registry*, not a root: a list script
    /// has dropped is still evaluated (CSSOM says `matches` tracks the viewport
    /// for as long as the object exists) but collects like any other object, and
    /// `gc_identity_roots` does not include it.
    media_query_lists: Vec<ObjectId>,
    /// Highest DOM revision already copied into observer queues.
    mutation_seen_revision: DomRevision,
}

impl From<DomError> for JsError {
    fn from(error: DomError) -> Self {
        Self::new(JsErrorKind::Dom, error.to_string(), None)
    }
}

/// The self-hosted standard library; see `JsRuntime::ensure_prelude`.
const PRELUDE_SOURCE: &str = include_str!("../prelude.js");

/// A Rust function an embedder installs in the realm with
/// [`JsRuntime::define_host_function`]. It receives the arguments of each call.
pub type HostFunction = fn(&mut JsRuntime, &mut Dom, &[JsValue]) -> Result<JsValue, JsError>;

impl JsRuntime {
    #[must_use]
    ///
    /// # Panics
    ///
    /// The built-in `about:blank` URL is a constant and always parses.
    pub fn new(dom: &Dom) -> Self {
        Self::with_url(
            dom,
            &Url::parse("about:blank").expect("about:blank is a valid URL"),
        )
    }

    #[must_use]
    pub fn with_url(dom: &Dom, document_url: &Url) -> Self {
        Self::with_limits_and_url(dom, RuntimeLimits::default(), document_url)
    }

    #[must_use]
    ///
    /// # Panics
    ///
    /// The built-in `about:blank` URL is a constant and always parses.
    pub fn with_limits(dom: &Dom, limits: RuntimeLimits) -> Self {
        Self::with_limits_and_url(
            dom,
            limits,
            &Url::parse("about:blank").expect("about:blank is a valid URL"),
        )
    }

    #[must_use]
    pub fn with_limits_and_url(dom: &Dom, limits: RuntimeLimits, document_url: &Url) -> Self {
        Self {
            realm: Realm::bootstrap(dom.document(), document_url),
            host_functions: Vec::new(),
            steps_remaining: limits.max_execution_steps,
            calls_active: 0,
            dom_nodes_created: 0,
            environment: Vec::new(),
            suspended_environments: Vec::new(),
            modules: BTreeMap::new(),
            prelude_installed: false,
            pending_loop_labels: Vec::new(),
            coroutines: Vec::new(),
            coroutine_code: BTreeMap::new(),
            async_generators: BTreeMap::new(),
            prelude_error: None,
            functions: Vec::new(),
            class_frames: Vec::new(),
            new_target_stack: Vec::new(),
            super_call_pending: false,
            arrays_joining: Vec::new(),
            next_private_id: 1,
            promises: Vec::new(),
            promise_pairs_resolved: Vec::new(),
            pending_microtasks: Vec::new(),
            event_listeners: BTreeMap::new(),
            event_flags: BTreeMap::new(),
            event_handlers: BTreeMap::new(),
            global_bindings: BTreeMap::new(),
            timers: BTreeMap::new(),
            next_timer_id: 1,
            pending_timer_requests: Vec::new(),
            ready_state: DocumentReadyState::Loading,
            pending_history_requests: Vec::new(),
            pending_navigations: Vec::new(),
            pending_fetch_requests: Vec::new(),
            pending_fetch_promises: BTreeMap::new(),
            pending_fetch_targets: BTreeMap::new(),
            next_fetch_id: 1,
            regexes: Vec::new(),
            regex_cache: std::collections::HashMap::new(),
            console_messages: Vec::new(),
            window_event_handlers: BTreeMap::new(),
            next_symbol_id: crate::value::FIRST_DYNAMIC_SYMBOL_ID,
            global_symbol_registry: BTreeMap::new(),
            call_stack: Vec::new(),
            transient_roots: Vec::new(),
            source_line_starts: Vec::new(),
            js_cookie_jar: BTreeMap::new(),
            random_state: {
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0x2545_F491_4F6C_DD1D, |duration| {
                        u64::from(duration.subsec_nanos())
                    })
                    ^ 0x9E37_79B9_7F4A_7C15;
                nanos | 1
            },
            element_geometry: BTreeMap::new(),
            viewport: ElementRect {
                x: 0.0,
                y: 0.0,
                width: 1_024.0,
                height: 768.0,
            },
            intersection_observers: Vec::new(),
            mutation_observers: Vec::new(),
            media_query_lists: Vec::new(),
            mutation_seen_revision: dom.revision(),
            limits,
        }
    }

    #[must_use]
    pub const fn realm(&self) -> &Realm {
        &self.realm
    }

    /// Interpreter steps consumed by the script currently running (or the last
    /// one that finished). Embedders use it as a progress signal for a long
    /// bundle, where the first-failure position alone says nothing about how
    /// much of the file actually executed.
    #[must_use]
    pub fn steps_consumed(&self) -> usize {
        self.limits
            .max_execution_steps
            .saturating_sub(self.steps_remaining)
    }

    /// Drain callbacks registered through `queueMicrotask()` in FIFO order.
    /// The embedding page owns scheduling; the runtime only retains callable
    /// identities from this realm.
    /// Diagnostics: active call frame labels, innermost last.
    #[doc(hidden)]
    #[must_use]
    pub fn debug_call_stack(&self) -> Vec<String> {
        self.call_stack
            .iter()
            .map(|frame| frame.name.clone())
            .collect()
    }

    /// Innermost-first `    at <frame>` lines for `Error.prototype.stack`.
    pub(super) fn stack_frame_lines(&self) -> String {
        let mut lines = String::new();
        for frame in self.call_stack.iter().rev() {
            lines.push_str("\n    at ");
            lines.push_str(&frame.name);
        }
        lines
    }

    /// Render the innermost `limit` frames for compact error trails.
    pub(super) fn call_frame_trail(&self, limit: usize) -> String {
        let names = self
            .call_stack
            .iter()
            .rev()
            .take(limit)
            .map(|frame| frame.name.clone())
            .collect::<Vec<_>>();
        names.iter().rev().cloned().collect::<Vec<_>>().join(" <- ")
    }

    /// Allocate a fresh unique symbol; runtime symbols start above the
    /// well-known id range so identity never collides with bootstrap.
    pub(super) fn create_symbol(&mut self, description: Option<String>) -> JsSymbol {
        self.next_symbol_id += 1;
        JsSymbol::new(self.next_symbol_id, description)
    }

    /// `Symbol.for(key)`: return the registered symbol or register a new one.
    pub(super) fn symbol_for(&mut self, key: String) -> JsSymbol {
        if let Some(id) = self.global_symbol_registry.get(&key) {
            return JsSymbol::new(*id, Some(key));
        }
        self.next_symbol_id += 1;
        let id = self.next_symbol_id;
        self.global_symbol_registry.insert(key.clone(), id);
        JsSymbol::new(id, Some(key))
    }

    /// Construct a standard error instance from a global constructor, the
    /// shared path for thrown-error synthesis and depth-limit `RangeError`s.
    pub(super) fn construct_standard_error(
        &mut self,
        kind: ErrorKind,
        message: &str,
    ) -> Result<JsValue, JsError> {
        let Some(JsValue::Object(constructor)) = self.realm.global(kind.name()) else {
            return Err(JsError::resource(message.to_owned()));
        };
        let argument = JsValue::String(message.to_owned());
        match self.error_constructor(constructor, kind, &[argument]) {
            Ok(instance) => Ok(instance),
            Err(_) => Err(JsError::resource(message.to_owned())),
        }
    }

    pub fn take_pending_microtasks(&mut self) -> Vec<JsMicrotask> {
        std::mem::take(&mut self.pending_microtasks)
    }

    /// Drain timer scheduling requests emitted by script since the last call.
    /// Session-history changes script requested since the last call, in order.
    pub fn take_pending_history_requests(&mut self) -> Vec<HistoryRequest> {
        std::mem::take(&mut self.pending_history_requests)
    }

    /// The URL the document reports as its location, if the location host is
    /// available.
    fn document_location_url(&self) -> Option<Url> {
        match self.realm.global("location") {
            Some(JsValue::Object(object)) => match self.realm.host(object) {
                Some(ObjectHost::Location(url)) => Some(url),
                _ => None,
            },
            _ => None,
        }
    }

    /// `history.pushState` and `history.replaceState`: same-origin only, as the
    /// HTML standard requires (a different origin is a `SecurityError`). The
    /// location and `history.state` change at once; the shell applies the entry.
    pub(crate) fn history_state_change(
        &mut self,
        arguments: &[JsValue],
        replace: bool,
    ) -> Result<JsValue, JsError> {
        let current = self
            .document_location_url()
            .ok_or_else(|| JsError::dom("the document has no location to change"))?;
        let url = match arguments.get(2) {
            None | Some(JsValue::Undefined) => current.clone(),
            Some(value) => current
                .join(&value.to_js_string())
                .map_err(|error| JsError::dom(format!("invalid history URL: {error}")))?,
        };
        if !same_history_origin(&url, &current) {
            return Err(JsError::dom(
                "SecurityError: history cannot change the document to another origin",
            ));
        }
        let state = arguments.first().cloned().unwrap_or(JsValue::Null);
        let serialized = self.json_text_of(&state)?;
        if let Some(JsValue::Object(history)) = self.realm.global("history") {
            self.realm.set_property(history, "state".to_owned(), state);
        }
        self.realm.set_location_url(url.clone());
        let url = url.to_string();
        self.pending_history_requests.push(if replace {
            HistoryRequest::Replace {
                url,
                state: serialized,
            }
        } else {
            HistoryRequest::Push {
                url,
                state: serialized,
            }
        });
        Ok(JsValue::Undefined)
    }

    /// Applies a same-document session-history traversal: the location becomes
    /// `url`, `history.state` becomes the parsed `state`, and `popstate` fires at
    /// the document. Nothing reloads, so script state survives. Returns whether
    /// the event was not canceled.
    ///
    /// # Errors
    ///
    /// Returns the typed error of a handler that throws, or of a `state` text
    /// that is not JSON.
    pub fn traverse_history(
        &mut self,
        dom: &mut Dom,
        url: &Url,
        state: Option<&str>,
    ) -> Result<bool, JsError> {
        let state = match state {
            Some(text) => self.json_value_of_text(text)?,
            None => JsValue::Null,
        };
        if let Some(JsValue::Object(history)) = self.realm.global("history") {
            self.realm
                .set_property(history, "state".to_owned(), state.clone());
        }
        self.realm.set_location_url(url.clone());
        let document = dom.document();
        self.dispatch_dom_event(dom, document, "popstate", false, false, &[("state", state)])
    }

    /// Sets `history.length` to the embedding's session-history length. The
    /// embedding calls it before each turn, so the value reflects the list as
    /// of the turn's start.
    pub fn set_history_length(&mut self, length: usize) {
        let length = f64::from(u32::try_from(length).unwrap_or(u32::MAX));
        if let Some(JsValue::Object(history)) = self.realm.global("history") {
            self.realm
                .set_property(history, "length".to_owned(), JsValue::Number(length));
        }
    }

    /// The state `document.readyState` reports.
    #[must_use]
    pub const fn document_ready_state(&self) -> DocumentReadyState {
        self.ready_state
    }

    /// Set the state `document.readyState` reports. The embedding calls this
    /// before it queues the matching lifecycle event, so a handler sees the
    /// state the event announces.
    pub fn set_document_ready_state(&mut self, state: DocumentReadyState) {
        self.ready_state = state;
    }

    pub fn take_pending_timer_requests(&mut self) -> Vec<TimerRequest> {
        std::mem::take(&mut self.pending_timer_requests)
    }

    /// Drain buffered `console.*` output since the last call, oldest first.
    pub fn take_console_messages(&mut self) -> Vec<ConsoleMessage> {
        std::mem::take(&mut self.console_messages)
    }

    /// Drain script-requested navigations (`location.assign`, `replace`, or
    /// `href` writes) since the last call. The embedding performs the load.
    pub fn take_pending_navigations(&mut self) -> Vec<NavigationRequest> {
        std::mem::take(&mut self.pending_navigations)
    }

    /// Drain network transfers queued by `fetch()`/`XMLHttpRequest` since the
    /// last call. The embedding executes each request on its transport and
    /// completes it by id through [`Self::settle_fetch`].
    /// Serialize the JS-visible cookie jar as `document.cookie` does.
    pub(super) fn js_cookie_jar_serialize(&self) -> String {
        self.js_cookie_jar
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// Store one cookie from a `document.cookie` assignment.
    pub(super) fn js_cookie_store(&mut self, assignment: &str) {
        let Some((name, value)) = assignment.split_once('=') else {
            return;
        };
        let name = name.trim().to_owned();
        // Attribute suffixes (`; Path=/`, `; Secure`, etc.) are stripped.
        let value = value.split(';').next().unwrap_or("").trim().to_owned();
        if value.is_empty() {
            self.js_cookie_jar.remove(&name);
        } else {
            self.js_cookie_jar.insert(name, value);
        }
    }

    /// Whether any `fetch()`/XHR transfer is still queued for the
    /// embedding, used to keep the event loop polling.
    #[must_use]
    pub fn pending_fetch_queue_empty(&self) -> bool {
        self.pending_fetch_requests.is_empty()
    }

    pub fn take_pending_fetch_requests(&mut self) -> Vec<PendingFetch> {
        std::mem::take(&mut self.pending_fetch_requests)
    }

    /// Complete one queued network transfer previously drained from
    /// [`Self::take_pending_fetch_requests`].
    ///
    /// Unknown ids are ignored silently: the page may have navigated between
    /// queueing and settling. A success resolves the `fetch()` promise with a
    /// `Response` or completes the `XMLHttpRequest` (readyState 4, status,
    /// `responseText`, then `readystatechange`/`load` callbacks at the next
    /// microtask checkpoint); a failure rejects the promise with a
    /// `TypeError` or fires the `readystatechange`/`error` callbacks.
    pub fn settle_fetch(&mut self, dom: &mut Dom, id: u64, outcome: Result<FetchOutcome, String>) {
        let promise_index = self.pending_fetch_promises.remove(&id);
        let Some(target) = self.pending_fetch_targets.remove(&id) else {
            return;
        };
        match self.realm.host(target) {
            Some(ObjectHost::Promise(_)) => {
                let Some(promise_index) = promise_index else {
                    return;
                };
                match outcome {
                    Ok(outcome) => match self.build_response_value(&outcome) {
                        Ok(value) => {
                            let _ = self.resolve_promise_with(dom, promise_index, &value);
                        }
                        Err(error) => {
                            let reason = error
                                .thrown_value()
                                .cloned()
                                .unwrap_or_else(|| JsValue::String(error.to_string()));
                            self.reject_promise(promise_index, &reason);
                        }
                    },
                    Err(message) => {
                        let reason = self
                            .construct_standard_error(ErrorKind::TypeError, &message)
                            .unwrap_or_else(|_| JsValue::String(message.clone()));
                        self.reject_promise(promise_index, &reason);
                    }
                }
            }
            Some(ObjectHost::XmlHttpRequest(_)) => self.complete_xml_http_request(target, outcome),
            _ => {}
        }
    }

    /// Install border-box geometry captured from the latest layout pass. Keys
    /// are DOM node ids as produced by `NodeId::as_u64`.
    pub fn install_element_geometry(&mut self, geometry: BTreeMap<u64, ElementRect>) {
        self.element_geometry = geometry;
        self.queue_intersection_observers();
    }

    /// Publish the current CSS viewport and document scroll position.
    pub fn install_viewport(&mut self, width: f32, height: f32, scroll_x: f32, scroll_y: f32) {
        self.viewport = ElementRect {
            x: scroll_x.max(0.0),
            y: scroll_y.max(0.0),
            width: width.max(0.0),
            height: height.max(0.0),
        };
        self.queue_intersection_observers();
        // A `MediaQueryList` is a *subscription* to the viewport, so the same
        // call that tells intersection observers where the viewport is has to
        // tell a `MediaQueryList` whether it still matches. Without this a
        // `matchMedia` would be correct once and then never again, which is the
        // failure this project treats as worst: a global that exists, looks
        // right, and is silently wrong for the case that matters.
        self.queue_media_query_list_changes();
    }

    /// Whether at least one timer is still registered from script. Intervals
    /// keep this true until cancelled; timeouts only until they fire.
    #[must_use]
    pub fn has_active_timers(&self) -> bool {
        !self.timers.is_empty()
    }

    /// Fire a scheduled timer callback.
    ///
    /// Returns `Some(delay)` when the caller must re-arm an interval with that
    /// period, and `None` otherwise (unknown id, timeout, or animation frame).
    ///
    /// # Errors
    ///
    /// Propagates errors thrown inside the callback, except promise rejections
    /// which stay contained like in ordinary microtask execution.
    pub fn fire_timer(&mut self, dom: &mut Dom, id: u64) -> Result<Option<f64>, JsError> {
        let (next_period, outcome) = self.fire_timer_reporting(dom, id);
        outcome.map(|()| next_period)
    }

    /// Runs one timer callback. Returns the period of the next run, if the timer
    /// repeats, together with the callback's outcome, and the two are
    /// independent: an interval keeps repeating when its callback throws
    /// (HTML 8.6), so the embedding must reschedule from the first value and
    /// report the second. A callback that cancelled its own interval gets no
    /// next run.
    pub fn fire_timer_reporting(
        &mut self,
        dom: &mut Dom,
        id: u64,
    ) -> (Option<f64>, Result<(), JsError>) {
        let Some(entry) = self.timers.get(&id).cloned() else {
            return (None, Ok(()));
        };
        if entry.kind != TimerKind::Interval {
            self.timers.remove(&id);
        }
        self.steps_remaining = self.limits.max_execution_steps;
        self.calls_active = 0;
        self.dom_nodes_created = 0;
        self.environment.clear();
        let outcome = self.call(dom, entry.callback, &entry.arguments).map(|_| ());
        self.queue_mutation_deliveries(dom);
        let repeats = entry.kind == TimerKind::Interval && self.timers.contains_key(&id);
        (repeats.then_some(entry.delay_ms), outcome)
    }

    /// Dispatch a trusted DOM event at `target` as if the user agent produced
    /// it, returning whether the default action remains enabled (`true` means
    /// no listener called `preventDefault()`).
    ///
    /// # Errors
    ///
    /// Propagates errors thrown inside listeners.
    pub fn dispatch_dom_event(
        &mut self,
        dom: &mut Dom,
        target: NodeId,
        event_type: &str,
        bubbles: bool,
        cancelable: bool,
        extra_properties: &[(&str, JsValue)],
    ) -> Result<bool, JsError> {
        let event = self.create_event_object(event_type, bubbles, cancelable, false, true)?;
        for (name, value) in extra_properties {
            self.realm
                .set_property(event, (*name).to_owned(), value.clone());
        }
        self.steps_remaining = self.limits.max_execution_steps;
        self.calls_active = 0;
        self.dom_nodes_created = 0;
        self.environment.clear();
        let default_enabled =
            self.dispatch_prepared_event(dom, target, event, event_type, bubbles)?;
        self.queue_mutation_deliveries(dom);
        Ok(default_enabled)
    }

    /// Invoke one callable retained by the embedding page.
    ///
    /// # Errors
    ///
    /// Returns the same typed runtime or resource-limit errors as an ordinary
    /// JavaScript call.
    pub fn invoke_microtask(
        &mut self,
        dom: &mut Dom,
        microtask: JsMicrotask,
    ) -> Result<JsValue, JsError> {
        self.steps_remaining = self.limits.max_execution_steps;
        self.calls_active = 0;
        self.dom_nodes_created = 0;
        self.environment.clear();
        let outcome = match microtask {
            JsMicrotask::Callback(callback) => self.call(dom, callback, &[]),
            JsMicrotask::IntersectionObserver(observer) => {
                self.notify_intersection_observer(dom, observer)
            }
            JsMicrotask::MutationObserver(observer) => self.notify_mutation_observer(dom, observer),
            JsMicrotask::MediaQueryListChange(list) => self.notify_media_query_list(dom, list),
            JsMicrotask::PromiseReaction {
                handler,
                argument,
                fulfilled,
                capability,
            } => self.run_promise_reaction(dom, handler, argument, fulfilled, capability),
        };
        match outcome {
            Ok(value) => Ok(value),
            Err(error) => {
                // An exception escaping a microtask callback is an uncaught
                // runtime failure, so it fires the same window `error` event
                // a failing classic script does.
                if error.kind() != JsErrorKind::ResourceLimit {
                    self.report_uncaught_error(dom, &error);
                }
                Err(error)
            }
        }
    }

    /// Dispatch the window `error` event for one uncaught script failure.
    ///
    /// The event reuses the ordinary window dispatch path, so listeners
    /// registered through `window.addEventListener("error", ...)` observe the
    /// `{message, filename, lineno, colno, error}` payload real engines
    /// provide: the thrown value is surfaced as a real Error instance when
    /// one was thrown (or synthesized for typed host errors), and
    /// line/column come from the source position when known. Failures inside
    /// error listeners are swallowed so an error handler cannot recurse this
    /// path; resource-limit aborts carry no script value and dispatch
    /// nothing.
    fn report_uncaught_error(&mut self, dom: &mut Dom, error: &JsError) {
        if error.kind() == JsErrorKind::ResourceLimit {
            return;
        }
        self.steps_remaining = self.limits.max_execution_steps;
        self.calls_active = 0;
        self.dom_nodes_created = 0;
        self.environment.clear();
        let prototype = self
            .realm
            .global("Event")
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            })
            .and_then(|constructor| {
                self.realm
                    .get_property(constructor, "prototype")
                    .and_then(|value| match value {
                        JsValue::Object(object) => Some(object),
                        _ => None,
                    })
            });
        if self.ensure_heap_capacity(1).is_err() {
            return;
        }
        let error_value = match error.thrown_value() {
            Some(value) => value.clone(),
            None => self
                .construct_standard_error(standard_error_kind(error.kind()), error.message())
                .unwrap_or(JsValue::Undefined),
        };
        let message = match &error_value {
            JsValue::Object(instance) => self
                .realm
                .get_property(*instance, "message")
                .map(|value| value.to_js_string())
                .filter(|message| !message.is_empty())
                .unwrap_or_else(|| error.message().to_owned()),
            _ => error.message().to_owned(),
        };
        let event = self.realm.create_object(prototype);
        for (name, value) in [
            ("type", JsValue::String("error".to_owned())),
            ("bubbles", JsValue::Boolean(false)),
            ("cancelable", JsValue::Boolean(true)),
            ("defaultPrevented", JsValue::Boolean(false)),
            ("target", JsValue::Null),
            ("currentTarget", JsValue::Null),
            ("message", JsValue::String(message)),
            ("filename", JsValue::String(self.document_url_string())),
            (
                "lineno",
                JsValue::Number(error.position().map_or(0.0, |(line, _)| line as f64)),
            ),
            (
                "colno",
                JsValue::Number(error.position().map_or(0.0, |(_, column)| column as f64)),
            ),
            ("error", error_value),
        ] {
            self.realm.set_property(event, name.to_owned(), value);
        }
        // Window-level dispatch: the document is the propagation target and
        // `window_event_handlers` listeners run last, matching every other
        // window event this runtime fires. Listener throws are ignored.
        let _ = self.dispatch_prepared_event(dom, dom.document(), event, "error", false);
    }

    /// The committed document URL, used as the `filename` field of window
    /// `error` events. Falls back to the empty string when the location host
    /// is unavailable.
    fn document_url_string(&self) -> String {
        match self.realm.global("location") {
            Some(JsValue::Object(object)) => match self.realm.host(object) {
                Some(ObjectHost::Location(url)) => url.to_string(),
                _ => String::new(),
            },
            _ => String::new(),
        }
    }

    /// Parse and run one script against the existing DOM arena.
    ///
    /// # Errors
    ///
    /// Returns a typed syntax/runtime/DOM/resource-limit error. Unsupported
    /// syntax is never silently ignored.
    pub fn execute(&mut self, dom: &mut Dom, source: &str) -> Result<ScriptOutcome, JsError> {
        self.ensure_prelude(dom);
        self.source_line_starts = build_line_starts(source);
        let script = match super::CompiledScript::compile(source, &self.limits) {
            Ok(script) => script,
            Err(error) => {
                let error = self.position_error(error);
                self.report_uncaught_error(dom, &error);
                return Err(error);
            }
        };
        self.execute_compiled(dom, &script)
    }

    /// Defines `name` on `target` as a function that runs `host` when called. An
    /// embedder uses this for host-defined functions, such as the test262 host's
    /// `$262.detachArrayBuffer`.
    pub fn define_host_function(&mut self, target: ObjectId, name: &str, host: HostFunction) {
        let index = self.host_functions.len();
        self.host_functions.push(host);
        let function = self.realm.native_object(NativeFunction::Host(index));
        self.realm
            .set_property(target, name.to_owned(), JsValue::Object(function));
    }

    /// Install the built-ins that are written in JavaScript (`prelude.js`).
    ///
    /// Engines commonly self-host the parts of the standard library that are
    /// simplest to state in the language itself; doing the same here keeps the
    /// Rust surface small. It runs once, lazily, before the first script, with
    /// its own execution budget, and never reports to `window.onerror`.
    pub(super) fn ensure_prelude(&mut self, dom: &mut Dom) {
        if self.prelude_installed {
            return;
        }
        self.prelude_installed = true;
        thread_local! {
            // Parsing the prelude is most of its cost, and a thread's runtimes
            // can all run the same tree.
            static COMPILED: std::cell::RefCell<Option<std::rc::Rc<super::CompiledScript>>> =
                const { std::cell::RefCell::new(None) };
        }
        let compiled = COMPILED.with(|cell| {
            if let Some(script) = cell.borrow().as_ref() {
                return Ok(script.clone());
            }
            let script = std::rc::Rc::new(super::CompiledScript::compile(
                PRELUDE_SOURCE,
                &self.limits,
            )?);
            *cell.borrow_mut() = Some(script.clone());
            Ok(script)
        });
        let outcome = compiled.and_then(|script| {
            self.source_line_starts = build_line_starts(PRELUDE_SOURCE);
            self.steps_remaining = self.limits.max_execution_steps;
            self.calls_active = 0;
            self.environment.clear();
            let from_revision = dom.revision();
            self.run_compiled_script(dom, &script.statements, from_revision)
        });
        if let Err(error) = outcome {
            if std::env::var_os("RENDER_JS_TRACE").is_some() {
                eprintln!("[render-js prelude failed: {error}]");
            }
            self.prelude_error = Some(error);
        }
    }

    /// The error the self-hosted prelude failed with, if any.
    #[must_use]
    pub fn prelude_error(&self) -> Option<&JsError> {
        self.prelude_error.as_ref()
    }

    /// Resolve an error's byte offset into a line/column pair using the
    /// source most recently executed in this runtime.
    fn position_error(&self, mut error: JsError) -> JsError {
        if error.position().is_none()
            && let Some(offset) = error.offset()
            && let Some(position) = resolve_position(&self.source_line_starts, offset)
        {
            error = error.at_position(position.0, position.1);
        }
        error
    }

    /// Run a previously compiled script in this runtime's Realm.
    ///
    /// # Errors
    ///
    /// Returns typed declaration-instantiation, runtime, DOM, or resource-limit
    /// errors. Every invocation receives a fresh execution budget while Realm
    /// globals and heap identity intentionally persist between classic scripts.
    pub fn execute_compiled(
        &mut self,
        dom: &mut Dom,
        script: &super::CompiledScript,
    ) -> Result<ScriptOutcome, JsError> {
        self.ensure_prelude(dom);
        self.source_line_starts = build_line_starts(script.source());
        let from_revision = dom.revision();
        // Nothing from the previous script can still be held by an interpreter
        // frame, so its boxed-wrapper pins go before the collection runs.
        self.transient_roots.clear();
        // Classic scripts share one realm, so reclaim the previous script's
        // garbage before allocating objects for this one.
        self.collect_garbage();
        self.steps_remaining = self.limits.max_execution_steps;
        self.calls_active = 0;
        self.dom_nodes_created = 0;
        self.environment.clear();
        let outcome = self.run_compiled_script(dom, &script.statements, from_revision);
        if let Err(error) = &outcome {
            self.report_uncaught_error(dom, error);
        }
        outcome
    }

    /// Interpret one compiled script body. Errors escaping this body are
    /// uncaught failures of the whole script; the caller reports them as
    /// window `error` events after they have been source-positioned.
    fn run_compiled_script(
        &mut self,
        dom: &mut Dom,
        statements: &[Statement],
        from_revision: DomRevision,
    ) -> Result<ScriptOutcome, JsError> {
        self.instantiate_statements(statements)
            .map_err(|error| self.position_error(error))?;
        let completion = self
            .evaluate_statements(dom, statements)
            .map_err(|error| self.position_error(error))?;
        self.queue_mutation_deliveries(dom);
        let value = match completion {
            Completion::Normal(value) => value,
            Completion::Return(_) | Completion::Break(_) | Completion::Continue(_) => {
                return Err(JsError::new(
                    JsErrorKind::Syntax,
                    "abrupt completion escaped the script body",
                    None,
                ));
            }
        };
        Ok(ScriptOutcome {
            value,
            from_revision,
            to_revision: dom.revision(),
        })
    }

    /// Upper bound on one typed array's element count. Real bundles use small
    /// pools; the bound keeps a hostile `new Float64Array(2**53-1)` from
    /// attempting a multi-gigabyte allocation.
    const MAX_TYPED_ARRAY_ELEMENTS: usize = 1 << 24;

    /// Resolve a `location.assign`/`replace` argument against the committed
    /// document URL and queue the navigation for the embedding.
    fn request_location_navigation(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
        function: NativeFunction,
    ) -> Result<JsValue, JsError> {
        let replace = function == NativeFunction::LocationReplace;
        let base = match self.realm.host(receiver) {
            Some(ObjectHost::Location(url)) => url.clone(),
            _ => return Err(JsError::type_error("incompatible Location method receiver")),
        };
        let target = required_argument(arguments, 0, if replace { "replace" } else { "assign" })?
            .to_js_string();
        let resolved = base
            .join(&target)
            .map_err(|error| JsError::dom(format!("invalid navigation URL {target:?}: {error}")))?;
        self.pending_navigations.push(NavigationRequest {
            url: resolved.to_string(),
            replace,
        });
        Ok(JsValue::Undefined)
    }

    /// Surface the call-depth limit as a throw-ready `RangeError` instance so
    /// script-level `try`/`catch` and `instanceof RangeError` checks behave
    /// like a real engine. Falls back to the resource-limit error when the
    /// heap cannot admit the error object. The diagnostic trail names the
    /// innermost active frames.
    fn call_depth_exceeded(&mut self) -> JsError {
        let trail = self.call_frame_trail(8);
        let message = format!("Maximum call stack size exceeded (near {trail})");
        match self.construct_standard_error(ErrorKind::RangeError, &message) {
            Ok(instance) => JsError::thrown_with_message(instance, message),
            Err(_) => JsError::resource(message),
        }
    }

    /// Surface a throw-ready `RangeError` instance with the given message.
    fn range_error(&mut self, message: &str) -> JsError {
        let value = JsValue::String(message.to_owned());
        match self.realm.global("RangeError") {
            Some(JsValue::Object(constructor)) => {
                match self.error_constructor(constructor, ErrorKind::RangeError, &[value]) {
                    Ok(JsValue::Object(instance)) => JsError::thrown(JsValue::Object(instance)),
                    _ => JsError::type_error(message),
                }
            }
            _ => JsError::type_error(message),
        }
    }

    /// Temporary: statement tracing gate for offline diagnostics.
    fn statement_trace_enabled() -> bool {
        static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ENABLED.get_or_init(|| std::env::var("RENDER_JS_TRACE").is_ok())
    }

    fn depth_trace_enabled() -> bool {
        static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ENABLED.get_or_init(|| std::env::var("RENDER_JS_DEPTH").is_ok())
    }

    fn binding_trace_enabled() -> bool {
        static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ENABLED.get_or_init(|| std::env::var("RENDER_JS_BINDINGS").is_ok())
    }

    fn consume_step(&mut self) -> Result<(), JsError> {
        if self.steps_remaining == 0 {
            return Err(JsError::resource(
                "JavaScript execution step limit exceeded",
            ));
        }
        self.steps_remaining = self.steps_remaining.saturating_sub(1);
        Ok(())
    }

    /// Keeps the current scope chain alive as a collector root before the
    /// runtime swaps it for another one. Returns the depth to hand to
    /// `resume_scopes` once the chain is back in place.
    fn suspend_scopes(&mut self) -> usize {
        let depth = self.suspended_environments.len();
        self.suspended_environments
            .extend(self.environment.iter().cloned());
        depth
    }

    /// Releases the roots that `suspend_scopes` returned `depth` for.
    fn resume_scopes(&mut self, depth: usize) {
        self.suspended_environments.truncate(depth);
    }

    fn call_native_dispatch(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        self.dispatch_fetch_native(dom, function, receiver, arguments)
    }
}

/// Map a runtime error kind onto the global error constructor used to
/// synthesize the `error` field of window `error` events when no script value
/// was thrown.
fn standard_error_kind(kind: JsErrorKind) -> ErrorKind {
    match kind {
        JsErrorKind::Syntax => ErrorKind::SyntaxError,
        JsErrorKind::Reference => ErrorKind::ReferenceError,
        JsErrorKind::Type => ErrorKind::TypeError,
        JsErrorKind::Dom | JsErrorKind::Throw | JsErrorKind::ResourceLimit => ErrorKind::Error,
    }
}

/// Byte offsets where each line of `source` starts (line 1 starts at 0).
fn build_line_starts(source: &str) -> Vec<usize> {
    let mut starts = vec![0_usize];
    for (index, byte) in source.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(index + 1);
        }
    }
    starts
}

/// Translate a byte offset into a 1-based (line, column) pair.
fn resolve_position(line_starts: &[usize], offset: usize) -> Option<(usize, usize)> {
    let mut line = line_starts
        .binary_search(&offset)
        .unwrap_or_else(|insertion| insertion.saturating_sub(1));
    if line >= line_starts.len() {
        line = line_starts.len().saturating_sub(1);
    }
    let line_start = line_starts.get(line)?;
    if offset < *line_start {
        return None;
    }
    Some((line + 1, offset - line_start + 1))
}
