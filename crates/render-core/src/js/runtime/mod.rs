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
use crate::dom::DomError;
use crate::dom::DomRevision;
use crate::dom::NodeId;
use crate::js::JsError;
use crate::js::JsErrorKind;
use crate::js::JsValue;
use crate::js::ObjectId;
use crate::js::Realm;
use crate::js::RuntimeLimits;
use crate::js::ScriptOutcome;
use crate::js::runtime::convert::required_argument;
use crate::js::runtime::eval::Completion;
use crate::js::runtime::types::Environment;
use crate::js::runtime::types::GlobalBinding;
use crate::js::runtime::types::PromiseRecord;
use crate::js::runtime::types::RegexRecord;
use crate::js::runtime::types::UserFunction;
use crate::js::value::ErrorKind;
use crate::js::value::NativeFunction;
use crate::js::value::ObjectHost;
use std::collections::BTreeMap;
use url::Url;

mod builtins;
mod convert;
mod eval;
mod gc;
mod types;

#[cfg(test)]
mod tests;

pub use types::{
    ConsoleLevel, ConsoleMessage, ElementRect, JsMicrotask, NavigationRequest, TimerEntry,
    TimerKind, TimerRequest,
};

/// A realm-owning interpreter instance. DOM wrappers retain stable `NodeId`
/// identities, never Rust references, across calls to [`Self::execute`].
#[derive(Debug)]
pub struct JsRuntime {
    realm: Realm,
    limits: RuntimeLimits,
    steps_remaining: usize,
    calls_active: usize,
    dom_nodes_created: usize,
    this_stack: Vec<JsValue>,
    environment: Vec<Environment>,
    functions: Vec<UserFunction>,
    promises: Vec<PromiseRecord>,
    pending_microtasks: Vec<JsMicrotask>,
    event_listeners: BTreeMap<NodeId, BTreeMap<String, Vec<ObjectId>>>,
    event_handlers: BTreeMap<NodeId, BTreeMap<String, ObjectId>>,
    global_bindings: BTreeMap<String, GlobalBinding>,
    timers: BTreeMap<u64, TimerEntry>,
    next_timer_id: u64,
    pending_timer_requests: Vec<TimerRequest>,
    pending_navigations: Vec<NavigationRequest>,
    regexes: Vec<RegexRecord>,
    console_messages: Vec<ConsoleMessage>,
    window_event_handlers: BTreeMap<String, Vec<ObjectId>>,
    next_symbol_id: u64,
    /// Temporary diagnostics ring: active user/native call names.
    call_stack: Vec<String>,
    random_state: u64,
    element_geometry: BTreeMap<u64, ElementRect>,
    viewport: ElementRect,
    intersection_observers: Vec<ObjectId>,
    mutation_observers: Vec<ObjectId>,
    /// Highest DOM revision already copied into observer queues.
    mutation_seen_revision: DomRevision,
}

impl From<DomError> for JsError {
    fn from(error: DomError) -> Self {
        Self::new(JsErrorKind::Dom, error.to_string(), None)
    }
}

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
            steps_remaining: limits.max_execution_steps,
            calls_active: 0,
            dom_nodes_created: 0,
            this_stack: Vec::new(),
            environment: Vec::new(),
            functions: Vec::new(),
            promises: Vec::new(),
            pending_microtasks: Vec::new(),
            event_listeners: BTreeMap::new(),
            event_handlers: BTreeMap::new(),
            global_bindings: BTreeMap::new(),
            timers: BTreeMap::new(),
            next_timer_id: 1,
            pending_timer_requests: Vec::new(),
            pending_navigations: Vec::new(),
            regexes: Vec::new(),
            console_messages: Vec::new(),
            window_event_handlers: BTreeMap::new(),
            next_symbol_id: 0,
            call_stack: Vec::new(),
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
            mutation_seen_revision: dom.revision(),
            limits,
        }
    }

    #[must_use]
    pub const fn realm(&self) -> &Realm {
        &self.realm
    }

    /// Drain callbacks registered through `queueMicrotask()` in FIFO order.
    /// The embedding page owns scheduling; the runtime only retains callable
    /// identities from this realm.
    /// Temporary diagnostics: current JS call stack labels.
    #[doc(hidden)]
    #[must_use]
    pub fn debug_call_stack(&self) -> Vec<String> {
        self.call_stack.clone()
    }

    pub fn take_pending_microtasks(&mut self) -> Vec<JsMicrotask> {
        std::mem::take(&mut self.pending_microtasks)
    }

    /// Drain timer scheduling requests emitted by script since the last call.
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
        let Some(entry) = self.timers.get(&id).cloned() else {
            return Ok(None);
        };
        if entry.kind != TimerKind::Interval {
            self.timers.remove(&id);
        }
        self.steps_remaining = self.limits.max_execution_steps;
        self.calls_active = 0;
        self.dom_nodes_created = 0;
        self.this_stack.clear();
        self.environment.clear();
        self.call(dom, entry.callback, &[])?;
        self.queue_mutation_deliveries(dom);
        Ok((entry.kind == TimerKind::Interval).then_some(entry.delay_ms))
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
        self.ensure_heap_capacity(1)?;
        let event = self.realm.create_object(prototype);
        for (name, value) in [
            ("type", JsValue::String(event_type.to_owned())),
            ("bubbles", JsValue::Boolean(bubbles)),
            ("cancelable", JsValue::Boolean(cancelable)),
            ("defaultPrevented", JsValue::Boolean(false)),
            ("target", JsValue::Null),
            ("currentTarget", JsValue::Null),
        ] {
            self.realm.set_property(event, name.to_owned(), value);
        }
        for (name, value) in extra_properties {
            self.realm
                .set_property(event, (*name).to_owned(), value.clone());
        }
        self.steps_remaining = self.limits.max_execution_steps;
        self.calls_active = 0;
        self.dom_nodes_created = 0;
        self.this_stack.clear();
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
        self.this_stack.clear();
        self.environment.clear();
        match microtask {
            JsMicrotask::Callback(callback) => self.call(dom, callback, &[]),
            JsMicrotask::IntersectionObserver(observer) => {
                self.notify_intersection_observer(dom, observer)
            }
            JsMicrotask::MutationObserver(observer) => self.notify_mutation_observer(dom, observer),
            JsMicrotask::PromiseReaction {
                handler,
                argument,
                fulfilled,
                result_promise,
            } => {
                let outcome = match handler {
                    Some(handler) => self.call(dom, handler, std::slice::from_ref(&argument)),
                    None if fulfilled => Ok(argument),
                    None => Err(JsError::thrown(argument)),
                };
                match outcome {
                    Ok(value) => self.resolve_promise_value(result_promise, &value)?,
                    Err(error) if error.kind() != JsErrorKind::ResourceLimit => {
                        let reason = error
                            .thrown_value()
                            .cloned()
                            .unwrap_or_else(|| JsValue::String(error.to_string()));
                        self.reject_promise(result_promise, &reason);
                    }
                    Err(error) => return Err(error),
                }
                Ok(JsValue::Undefined)
            }
        }
    }

    /// Parse and run one script against the existing DOM arena.
    ///
    /// # Errors
    ///
    /// Returns a typed syntax/runtime/DOM/resource-limit error. Unsupported
    /// syntax is never silently ignored.
    pub fn execute(&mut self, dom: &mut Dom, source: &str) -> Result<ScriptOutcome, JsError> {
        let script = super::CompiledScript::compile(source, &self.limits)?;
        self.execute_compiled(dom, &script)
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
        let from_revision = dom.revision();
        // Classic scripts share one realm, so reclaim the previous script's
        // garbage before allocating objects for this one.
        self.collect_garbage();
        self.steps_remaining = self.limits.max_execution_steps;
        self.calls_active = 0;
        self.dom_nodes_created = 0;
        self.this_stack.clear();
        self.environment.clear();
        self.instantiate_statements(&script.statements)?;
        let completion = self.evaluate_statements(dom, &script.statements)?;
        self.queue_mutation_deliveries(dom);
        let value = match completion {
            Completion::Normal(value) => value,
            Completion::Return(_) | Completion::Break(_) | Completion::Continue(_) => {
                return Err(JsError::syntax(
                    "abrupt completion escaped the script body",
                    0,
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
        let trail = self
            .call_stack
            .iter()
            .rev()
            .take(8)
            .rev()
            .cloned()
            .collect::<Vec<_>>()
            .join(" <- ");
        let message = format!("Maximum call stack size exceeded (near {trail})");
        let thrown = match self.realm.global("RangeError") {
            Some(JsValue::Object(constructor)) => {
                let argument = JsValue::String(message.clone());
                match self.error_constructor(constructor, ErrorKind::RangeError, &[argument]) {
                    Ok(JsValue::Object(instance)) => Some(JsError::thrown_with_message(
                        JsValue::Object(instance),
                        message.clone(),
                    )),
                    _ => None,
                }
            }
            _ => None,
        };
        thrown.unwrap_or_else(|| JsError::resource(message))
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

    fn call_native_dispatch(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        self.dispatch_dom_native(dom, function, receiver, arguments)
    }
}
