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

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::rc::Rc;

use super::lexer::surrogate_placeholder;
use super::parser::{
    BinaryOp, CatchClause, Expr, ObjectProperty, PropertyKey, Statement, UnaryOp, VariableKind,
};
use super::value::{CollectionKind, ErrorKind, NativeFunction, ObjectHost};
use super::{
    JsError, JsErrorKind, JsObject, JsValue, ObjectId, PropertyDescriptor, Realm, RuntimeLimits,
    ScriptOutcome,
};
use crate::css::selector::{MatchContext, matches_selector_list, parse_selector_list, select_all};
use crate::css::stylesheet::parse_declaration_list;
use crate::dom::{Dom, DomError, NodeId, NodeKind};
use crate::html::{serialize_html_fragment, serialize_html_node};
use url::Url;

#[derive(Clone, Debug)]
struct Binding {
    value: JsValue,
    mutable: bool,
    initialized: bool,
    kind: VariableKind,
}

#[derive(Clone, Copy, Debug)]
struct GlobalBinding {
    mutable: bool,
    initialized: bool,
    kind: VariableKind,
}

#[derive(Clone, Copy)]
enum ObjectEntryKind {
    Keys,
    Values,
    Entries,
}

#[derive(Clone, Copy)]
enum CollectionView {
    Keys,
    Values,
    Entries,
}

impl ObjectEntryKind {
    fn function_name(self) -> &'static str {
        match self {
            Self::Keys => "Object.keys",
            Self::Values => "Object.values",
            Self::Entries => "Object.entries",
        }
    }
}

#[derive(Debug, Default)]
struct EnvironmentRecord {
    bindings: BTreeMap<String, Binding>,
    function_scope: bool,
}

type Environment = Rc<RefCell<EnvironmentRecord>>;

/// Upper bound on buffered `console.*` messages; the oldest entry is dropped
/// when script logs past it so a chatty page cannot exhaust memory.
const MAX_BUFFERED_CONSOLE_MESSAGES: usize = 4096;

#[derive(Clone, Debug)]
struct UserFunction {
    parameters: Vec<String>,
    body: Vec<Statement>,
    captured_environment: Vec<Environment>,
    lexical_this: Option<JsValue>,
}

#[derive(Clone, Debug)]
enum PromiseState {
    Pending,
    Fulfilled(JsValue),
    Rejected(JsValue),
}

#[derive(Clone, Debug)]
struct PromiseReaction {
    on_fulfilled: Option<ObjectId>,
    on_rejected: Option<ObjectId>,
    result_promise: usize,
}

#[derive(Clone, Debug)]
struct PromiseRecord {
    state: PromiseState,
    reactions: Vec<PromiseReaction>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum JsMicrotask {
    Callback(ObjectId),
    IntersectionObserver(ObjectId),
    PromiseReaction {
        handler: Option<ObjectId>,
        argument: JsValue,
        fulfilled: bool,
        result_promise: usize,
    },
}

/// The scheduling flavor of a timer registered from script.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimerKind {
    /// `setTimeout`: fires once.
    Timeout,
    /// `setInterval`: the embedding re-arms it after each fire.
    Interval,
    /// `requestAnimationFrame`: fires once per frame the embedding drives.
    AnimationFrame,
}

/// A callback registered through the global timer functions. The runtime
/// retains only callable identities; actual scheduling belongs to the
/// embedding page, which drains [`JsRuntime::take_pending_timer_requests`].
#[derive(Clone, Debug)]
pub struct TimerEntry {
    pub kind: TimerKind,
    pub callback: ObjectId,
    pub delay_ms: f64,
}

/// A scheduling request emitted while script executed. `Schedule` entries must
/// become event-loop tasks after the current execution; `Cancel` entries drop
/// previously scheduled tasks that have not fired yet.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TimerRequest {
    Schedule { id: u64, delay_ms: f64 },
    Cancel { id: u64 },
}

/// A navigation a script requested through the `Location` interface
/// (`location.assign`, `location.replace`, or a `location.href` write).
///
/// The runtime never loads URLs itself; the embedding drains these requests
/// and performs the actual navigation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NavigationRequest {
    pub url: String,
    /// `true` for `location.replace()`-style navigation that must not add a
    /// history entry.
    pub replace: bool,
}

/// A compiled regular expression plus its mutable `lastIndex` state.
#[derive(Debug)]
struct RegexRecord {
    compiled: super::regex::Compiled,
    last_index: usize,
}

/// Severity of a buffered `console.*` message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConsoleLevel {
    Debug,
    Error,
    Info,
    Log,
    Warn,
}

impl ConsoleLevel {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Error => "error",
            Self::Info => "info",
            Self::Log => "log",
            Self::Warn => "warn",
        }
    }
}

/// One buffered console message drained by the embedding.
#[derive(Clone, Debug)]
pub struct ConsoleMessage {
    pub level: ConsoleLevel,
    pub text: String,
}

/// Border-box geometry of one element in CSS pixels, captured from the latest
/// layout pass and installed into the runtime by the embedding. Values are
/// stale by at most one render turn; elements without boxes read as zero.
#[derive(Clone, Copy, Debug)]
pub struct ElementRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Clone, Debug)]
enum Completion {
    Normal(JsValue),
    Return(JsValue),
    Break(Option<String>),
    Continue(Option<String>),
}

#[derive(Clone, Debug)]
enum AssignmentReference {
    Binding(String),
    Property { object: ObjectId, property: String },
}

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
        self.dispatch_prepared_event(dom, target, event, event_type, bubbles)
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
        self.steps_remaining = self.limits.max_execution_steps;
        self.calls_active = 0;
        self.dom_nodes_created = 0;
        self.this_stack.clear();
        self.environment.clear();
        self.instantiate_statements(&script.statements)?;
        let completion = self.evaluate_statements(dom, &script.statements)?;
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

    fn evaluate_statements(
        &mut self,
        dom: &mut Dom,
        statements: &[Statement],
    ) -> Result<Completion, JsError> {
        let mut value = JsValue::Undefined;
        for statement in statements {
            match self.evaluate_statement(dom, statement)? {
                Completion::Normal(next) => value = next,
                abrupt @ (Completion::Return(_)
                | Completion::Break(_)
                | Completion::Continue(_)) => {
                    return Ok(abrupt);
                }
            }
        }
        Ok(Completion::Normal(value))
    }

    fn instantiate_statements(&mut self, statements: &[Statement]) -> Result<(), JsError> {
        let mut lexical_declarations = BTreeMap::new();
        let mut var_names = BTreeSet::new();
        let mut functions = Vec::new();
        for statement in statements {
            match statement {
                Statement::Variable {
                    kind: kind @ (VariableKind::Let | VariableKind::Const),
                    name,
                    ..
                } => {
                    if lexical_declarations.insert(name.clone(), *kind).is_some() {
                        return Err(JsError::syntax(
                            format!("binding {name:?} is declared more than once"),
                            0,
                        ));
                    }
                }
                Statement::Variable {
                    kind: VariableKind::Var,
                    name,
                    ..
                } => {
                    var_names.insert(name.clone());
                }
                Statement::VariableList { kind, declarations } => {
                    for (name, _) in declarations {
                        if *kind == VariableKind::Var {
                            var_names.insert(name.clone());
                        } else if lexical_declarations.insert(name.clone(), *kind).is_some() {
                            return Err(JsError::syntax(
                                format!("binding {name:?} is declared more than once"),
                                0,
                            ));
                        }
                    }
                }
                Statement::Function {
                    name,
                    parameters,
                    body,
                } => {
                    var_names.insert(name.clone());
                    functions.push((name, parameters, body));
                }
                _ => collect_var_names(statement, &mut var_names),
            }
        }
        if let Some(name) = lexical_declarations
            .keys()
            .find(|name| var_names.contains(*name))
        {
            return Err(JsError::syntax(
                format!("binding {name:?} conflicts with a var declaration"),
                0,
            ));
        }

        for (name, kind) in lexical_declarations {
            self.create_binding(&name, kind, false, JsValue::Undefined)?;
        }
        for name in var_names {
            self.create_binding(&name, VariableKind::Var, true, JsValue::Undefined)?;
        }
        for (name, parameters, body) in functions {
            let value = self.create_user_function(parameters, body)?;
            self.initialize_binding(name, value, VariableKind::Var)?;
        }
        Ok(())
    }

    fn instantiate_block_lexicals(&mut self, statements: &[Statement]) -> Result<(), JsError> {
        let mut declarations = BTreeMap::new();
        let mut functions = Vec::new();
        for statement in statements {
            match statement {
                Statement::VariableList {
                    kind,
                    declarations: variables,
                } => {
                    for (name, _) in variables {
                        if *kind != VariableKind::Var
                            && declarations.insert(name.clone(), *kind).is_some()
                        {
                            return Err(JsError::syntax(
                                format!("binding {name:?} is declared more than once"),
                                0,
                            ));
                        }
                    }
                }
                Statement::Variable {
                    kind: kind @ (VariableKind::Let | VariableKind::Const),
                    name,
                    ..
                } if declarations.insert(name.clone(), *kind).is_some() => {
                    return Err(JsError::syntax(
                        format!("binding {name:?} is declared more than once"),
                        0,
                    ));
                }
                Statement::Function {
                    name,
                    parameters,
                    body,
                } => {
                    if declarations
                        .insert(name.clone(), VariableKind::Const)
                        .is_some()
                    {
                        return Err(JsError::syntax(
                            format!("binding {name:?} is declared more than once"),
                            0,
                        ));
                    }
                    functions.push((name, parameters, body));
                }
                _ => {}
            }
        }
        for (name, kind) in declarations {
            self.create_binding(&name, kind, false, JsValue::Undefined)?;
        }
        for (name, parameters, body) in functions {
            let value = self.create_user_function(parameters, body)?;
            self.initialize_binding(name, value, VariableKind::Const)?;
        }
        Ok(())
    }

    fn create_user_function(
        &mut self,
        parameters: &[String],
        body: &[Statement],
    ) -> Result<JsValue, JsError> {
        self.create_function(parameters, body, None)
    }

    fn create_arrow_function(
        &mut self,
        parameters: &[String],
        body: &[Statement],
    ) -> Result<JsValue, JsError> {
        let lexical_this = self
            .this_stack
            .last()
            .cloned()
            .unwrap_or(JsValue::Undefined);
        self.create_function(parameters, body, Some(lexical_this))
    }

    fn create_function(
        &mut self,
        parameters: &[String],
        body: &[Statement],
        lexical_this: Option<JsValue>,
    ) -> Result<JsValue, JsError> {
        let is_arrow = lexical_this.is_some();
        self.ensure_heap_capacity(if is_arrow { 1 } else { 2 })?;
        let function_index = self.functions.len();
        self.functions.push(UserFunction {
            parameters: parameters.to_vec(),
            body: body.to_vec(),
            captured_environment: self.environment.clone(),
            lexical_this,
        });
        let function = if is_arrow {
            self.realm.arrow_function(function_index)
        } else {
            self.realm.user_function(function_index)
        };
        Ok(JsValue::Object(function))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "statement dispatch mirrors the AST one-to-one"
    )]
    fn evaluate_statement(
        &mut self,
        dom: &mut Dom,
        statement: &Statement,
    ) -> Result<Completion, JsError> {
        self.consume_step()?;
        if Self::statement_trace_enabled() {
            let rendered = format!("{statement:?}");
            let truncated: String = rendered.chars().take(1400).collect();
            eprintln!("[stmt depth={}] {truncated}", self.calls_active);
        }
        // Cheap depth probe independent of Debug formatting.
        if Self::depth_trace_enabled() {
            eprintln!("[d={}]", self.calls_active);
        }
        if self.calls_active == 30 && Self::depth_trace_enabled() {
            eprintln!("{}", std::backtrace::Backtrace::force_capture());
        }
        match statement {
            Statement::Variable { kind, name, value } => {
                if *kind == VariableKind::Var && value.is_none() {
                    return Ok(Completion::Normal(JsValue::Undefined));
                }
                let value = match value {
                    Some(expression) => self.evaluate(dom, expression)?,
                    None => JsValue::Undefined,
                };
                self.initialize_binding(name, value.clone(), *kind)?;
                Ok(Completion::Normal(value))
            }
            Statement::VariableList { kind, declarations } => {
                let mut value = JsValue::Undefined;
                for (name, expression) in declarations {
                    if let Some(expression) = expression {
                        value = self.evaluate(dom, expression)?;
                        self.initialize_binding(name, value.clone(), *kind)?;
                    } else if *kind == VariableKind::Let {
                        self.initialize_binding(name, JsValue::Undefined, *kind)?;
                    }
                }
                Ok(Completion::Normal(value))
            }
            Statement::Function { name, .. } => self.lookup_binding(name).map(Completion::Normal),
            Statement::Return(value) => {
                let value = match value {
                    Some(expression) => self.evaluate(dom, expression)?,
                    None => JsValue::Undefined,
                };
                Ok(Completion::Return(value))
            }
            Statement::Throw(expression) => {
                let value = self.evaluate(dom, expression)?;
                Err(JsError::thrown(value))
            }
            Statement::Try {
                body,
                catch,
                finally,
            } => self.evaluate_try_statement(dom, body, catch.as_ref(), finally.as_deref()),
            Statement::If {
                condition,
                consequent,
                alternate,
            } => {
                if self.evaluate(dom, condition)?.is_truthy() {
                    self.evaluate_statement(dom, consequent)
                } else if let Some(alternate) = alternate {
                    self.evaluate_statement(dom, alternate)
                } else {
                    Ok(Completion::Normal(JsValue::Undefined))
                }
            }
            Statement::Switch { expression, cases } => {
                self.evaluate_switch_statement(dom, expression, cases)
            }
            Statement::While { condition, body } => {
                let mut value = JsValue::Undefined;
                loop {
                    self.consume_step()?;
                    if !self.evaluate(dom, condition)?.is_truthy() {
                        break;
                    }
                    match self.evaluate_statement(dom, body)? {
                        Completion::Normal(next) => value = next,
                        Completion::Continue(None) => {}
                        Completion::Break(None) => break,
                        returned @ Completion::Return(_) => return Ok(returned),
                        labeled @ (Completion::Break(Some(_)) | Completion::Continue(Some(_))) => {
                            return Ok(labeled);
                        }
                    }
                }
                Ok(Completion::Normal(value))
            }
            Statement::DoWhile { condition, body } => {
                let mut value = JsValue::Undefined;
                loop {
                    self.consume_step()?;
                    match self.evaluate_statement(dom, body)? {
                        Completion::Normal(next) => value = next,
                        Completion::Continue(None) => {}
                        Completion::Break(None) => break,
                        returned @ Completion::Return(_) => return Ok(returned),
                        labeled @ (Completion::Break(Some(_)) | Completion::Continue(Some(_))) => {
                            return Ok(labeled);
                        }
                    }
                    if !self.evaluate(dom, condition)?.is_truthy() {
                        break;
                    }
                }
                Ok(Completion::Normal(value))
            }
            Statement::For {
                initializer,
                condition,
                update,
                body,
            } => self.evaluate_for_statement(
                dom,
                initializer.as_deref(),
                condition.as_ref(),
                update.as_ref(),
                body,
            ),
            Statement::ForIn {
                kind,
                name,
                iterable,
                body,
            } => self.evaluate_for_in_statement(dom, *kind, name, iterable, body),
            Statement::ForOf {
                kind,
                name,
                iterable,
                body,
            } => self.evaluate_for_of_statement(dom, *kind, name, iterable, body),
            Statement::ForInExpr {
                target,
                iterable,
                body,
            } => self.evaluate_for_in_expr_statement(dom, target, iterable, body),
            // Labels bind `break label` / `continue label` to this statement;
            // unlabeled control flow binds to the nearest enclosing loop.
            Statement::Labeled { label, body } => match self.evaluate_statement(dom, body)? {
                Completion::Break(Some(target)) | Completion::Continue(Some(target))
                    if *target == *label =>
                {
                    Ok(Completion::Normal(JsValue::Undefined))
                }
                other => Ok(other),
            },
            Statement::Break(label) => Ok(Completion::Break(label.clone())),
            Statement::Continue(label) => Ok(Completion::Continue(label.clone())),
            Statement::Block(statements) => self.evaluate_scoped_statements(dom, statements),
            Statement::Expression(expression) => {
                self.evaluate(dom, expression).map(Completion::Normal)
            }
        }
    }

    fn evaluate_switch_statement(
        &mut self,
        dom: &mut Dom,
        expression: &Expr,
        cases: &[(Option<Expr>, Vec<Statement>)],
    ) -> Result<Completion, JsError> {
        let discriminant = self.evaluate(dom, expression)?;
        let mut active = false;
        let mut value = JsValue::Undefined;
        for (test, statements) in cases {
            if !active {
                active = match test {
                    Some(test) => strict_equal(&discriminant, &self.evaluate(dom, test)?),
                    None => true,
                };
            }
            if !active {
                continue;
            }
            match self.evaluate_statements(dom, statements)? {
                Completion::Normal(next) => value = next,
                Completion::Break(None) => break,
                abrupt => return Ok(abrupt),
            }
        }
        Ok(Completion::Normal(value))
    }

    fn evaluate_for_statement(
        &mut self,
        dom: &mut Dom,
        initializer: Option<&Statement>,
        condition: Option<&Expr>,
        update: Option<&Expr>,
        body: &Statement,
    ) -> Result<Completion, JsError> {
        self.environment
            .push(Rc::new(RefCell::new(EnvironmentRecord::default())));
        let result = (|| {
            if let Some(initializer) = initializer {
                if let Statement::Variable { kind, name, .. } = initializer {
                    self.create_binding(
                        name,
                        *kind,
                        *kind == VariableKind::Var,
                        JsValue::Undefined,
                    )?;
                }
                match self.evaluate_statement(dom, initializer)? {
                    Completion::Normal(_) => {}
                    abrupt => return Ok(abrupt),
                }
            }
            let mut value = JsValue::Undefined;
            loop {
                self.consume_step()?;
                if let Some(condition) = condition
                    && !self.evaluate(dom, condition)?.is_truthy()
                {
                    break;
                }
                match self.evaluate_statement(dom, body)? {
                    Completion::Normal(next) => value = next,
                    Completion::Continue(None) => {}
                    Completion::Break(None) => break,
                    returned @ Completion::Return(_) => return Ok(returned),
                    labeled @ (Completion::Break(Some(_)) | Completion::Continue(Some(_))) => {
                        return Ok(labeled);
                    }
                }
                if let Some(update) = update {
                    self.evaluate(dom, update)?;
                }
            }
            Ok(Completion::Normal(value))
        })();
        self.environment.pop();
        result
    }

    fn evaluate_for_in_statement(
        &mut self,
        dom: &mut Dom,
        kind: VariableKind,
        name: &str,
        iterable: &Expr,
        body: &Statement,
    ) -> Result<Completion, JsError> {
        let iterable = self.evaluate(dom, iterable)?;
        let names = match iterable {
            JsValue::Object(object) => self
                .realm
                .enumerable_property_names(object)
                .ok_or_else(|| JsError::type_error("could not enumerate object properties"))?,
            _ => Vec::new(),
        };
        let mut value = JsValue::Undefined;
        if kind == VariableKind::Var {
            self.create_binding(name, kind, true, JsValue::Undefined)?;
        }
        for property in names {
            self.consume_step()?;
            let iteration_environment = if kind == VariableKind::Var {
                None
            } else {
                let environment = Rc::new(RefCell::new(EnvironmentRecord::default()));
                environment.borrow_mut().bindings.insert(
                    name.to_owned(),
                    Binding {
                        value: JsValue::String(property.clone()),
                        mutable: kind != VariableKind::Const,
                        initialized: true,
                        kind,
                    },
                );
                self.environment.push(Rc::clone(&environment));
                Some(environment)
            };
            if kind == VariableKind::Var {
                self.assign_binding(name, JsValue::String(property))?;
            }
            let completion = self.evaluate_statement(dom, body);
            if iteration_environment.is_some() {
                self.environment.pop();
            }
            match completion? {
                Completion::Normal(next) => value = next,
                Completion::Continue(None) => {}
                Completion::Break(None) => break,
                returned @ Completion::Return(_) => return Ok(returned),
                labeled @ (Completion::Break(Some(_)) | Completion::Continue(Some(_))) => {
                    return Ok(labeled);
                }
            }
        }
        Ok(Completion::Normal(value))
    }

    /// `for (target in iterable)` with an assignment target instead of a
    /// declared binding.
    fn evaluate_for_in_expr_statement(
        &mut self,
        dom: &mut Dom,
        target: &Expr,
        iterable: &Expr,
        body: &Statement,
    ) -> Result<Completion, JsError> {
        let iterable = self.evaluate(dom, iterable)?;
        let names = match iterable {
            JsValue::Object(object) => self
                .realm
                .enumerable_property_names(object)
                .ok_or_else(|| JsError::type_error("could not enumerate object properties"))?,
            _ => Vec::new(),
        };
        let mut value = JsValue::Undefined;
        for property in names {
            self.consume_step()?;
            let reference = self.resolve_assignment_reference(dom, target)?;
            self.write_assignment_reference(dom, &reference, JsValue::String(property.clone()))?;
            match self.evaluate_statement(dom, body)? {
                Completion::Normal(next) => value = next,
                Completion::Continue(None) => {}
                Completion::Break(None) => break,
                returned @ Completion::Return(_) => return Ok(returned),
                labeled @ (Completion::Break(Some(_)) | Completion::Continue(Some(_))) => {
                    return Ok(labeled);
                }
            }
        }
        Ok(Completion::Normal(value))
    }

    fn evaluate_for_of_statement(
        &mut self,
        dom: &mut Dom,
        kind: VariableKind,
        name: &str,
        iterable: &Expr,
        body: &Statement,
    ) -> Result<Completion, JsError> {
        let iterable = self.evaluate(dom, iterable)?;
        let values = match iterable {
            JsValue::Object(object)
                if matches!(self.realm.host(object), Some(ObjectHost::Array)) =>
            {
                self.array_elements_for(object)
            }
            JsValue::String(text) => text
                .chars()
                .map(|character| JsValue::String(character.to_string()))
                .collect(),
            JsValue::Object(object) => {
                let length = self
                    .realm
                    .get_property(object, "length")
                    .map(|value| to_number(&value).unwrap_or(0.0).max(0.0) as usize)
                    .unwrap_or(0);
                (0..length)
                    .map(|index| self.get_member(dom, object, &index.to_string()))
                    .collect::<Result<Vec<_>, _>>()?
            }
            _ => Vec::new(),
        };
        if kind == VariableKind::Var {
            self.create_binding(name, kind, true, JsValue::Undefined)?;
        }
        let mut value = JsValue::Undefined;
        for item in values {
            self.consume_step()?;
            let iteration_environment = if kind == VariableKind::Var {
                None
            } else {
                let environment = Rc::new(RefCell::new(EnvironmentRecord::default()));
                environment.borrow_mut().bindings.insert(
                    name.to_owned(),
                    Binding {
                        value: item.clone(),
                        mutable: kind != VariableKind::Const,
                        initialized: true,
                        kind,
                    },
                );
                self.environment.push(Rc::clone(&environment));
                Some(environment)
            };
            if kind == VariableKind::Var {
                self.assign_binding(name, item)?;
            }
            let completion = self.evaluate_statement(dom, body);
            if iteration_environment.is_some() {
                self.environment.pop();
            }
            match completion? {
                Completion::Normal(next) => value = next,
                Completion::Continue(None) => {}
                Completion::Break(None) => break,
                returned @ Completion::Return(_) => return Ok(returned),
                labeled @ (Completion::Break(Some(_)) | Completion::Continue(Some(_))) => {
                    return Ok(labeled);
                }
            }
        }
        Ok(Completion::Normal(value))
    }

    fn evaluate_try_statement(
        &mut self,
        dom: &mut Dom,
        body: &[Statement],
        catch: Option<&CatchClause>,
        finally: Option<&[Statement]>,
    ) -> Result<Completion, JsError> {
        let mut result = self.evaluate_scoped_statements(dom, body);
        if let Err(error) = &result
            && error.kind() != JsErrorKind::ResourceLimit
            && let Some(catch) = catch
        {
            let value = error
                .thrown_value()
                .cloned()
                .unwrap_or_else(|| JsValue::String(error.to_string()));
            let catch_environment = Rc::new(RefCell::new(EnvironmentRecord::default()));
            catch_environment.borrow_mut().bindings.insert(
                catch.parameter.clone(),
                Binding {
                    value,
                    mutable: true,
                    initialized: true,
                    kind: VariableKind::Let,
                },
            );
            self.environment.push(catch_environment);
            result = self
                .instantiate_block_lexicals(&catch.body)
                .and_then(|()| self.evaluate_statements(dom, &catch.body));
            self.environment.pop();
        }
        if let Some(finally) = finally {
            match self.evaluate_scoped_statements(dom, finally) {
                Ok(Completion::Normal(_)) => {}
                abrupt => return abrupt,
            }
        }
        result
    }

    fn evaluate_scoped_statements(
        &mut self,
        dom: &mut Dom,
        statements: &[Statement],
    ) -> Result<Completion, JsError> {
        self.environment
            .push(Rc::new(RefCell::new(EnvironmentRecord::default())));
        let result = self
            .instantiate_block_lexicals(statements)
            .and_then(|()| self.evaluate_statements(dom, statements));
        self.environment.pop();
        result
    }

    #[allow(
        clippy::too_many_lines,
        reason = "expression dispatch mirrors the AST one-to-one"
    )]
    fn evaluate(&mut self, dom: &mut Dom, expression: &Expr) -> Result<JsValue, JsError> {
        self.consume_step()?;
        match expression {
            Expr::Literal(value) => Ok(value.clone()),
            Expr::RegexLiteral { pattern, flags } => {
                let object = self.construct_regex(pattern, flags)?;
                Ok(JsValue::Object(object))
            }
            Expr::This => Ok(self
                .this_stack
                .last()
                .cloned()
                // Bare `this` outside any function refers to the global object.
                .unwrap_or_else(|| JsValue::Object(self.realm.global_object()))),
            Expr::Identifier(name) => self.lookup_binding(name),
            Expr::Function {
                name,
                parameters,
                body,
            } => self.evaluate_function_expression(name.as_deref(), parameters, body),
            Expr::Arrow { parameters, body } => self.create_arrow_function(parameters, body),
            Expr::Object(properties) => self.evaluate_object_literal(dom, properties),
            Expr::Array(elements) => self.evaluate_array_literal(dom, elements),
            Expr::Spread(expression) => self.evaluate(dom, expression),
            Expr::ObjectRest { object, excluded } => {
                let value = self.evaluate(dom, object)?;
                self.create_object_rest(&value, excluded)
                    .map(JsValue::Object)
            }
            Expr::Unary {
                operator: UnaryOp::Delete,
                operand,
            } => self.evaluate_delete(dom, operand),
            // `typeof identifier` never throws for undeclared bindings.
            Expr::Unary {
                operator: UnaryOp::Typeof,
                operand,
            } if matches!(operand.as_ref(), Expr::Identifier(name) if !self.binding_exists(name)) => {
                Ok(JsValue::String("undefined".to_owned()))
            }
            Expr::Unary { operator, operand } => {
                let value = self.evaluate(dom, operand)?;
                self.evaluate_unary(*operator, &value)
            }
            Expr::Binary {
                operator,
                left,
                right,
            } => self.evaluate_binary(dom, *operator, left, right),
            Expr::Conditional {
                condition,
                consequent,
                alternate,
            } => {
                if self.evaluate(dom, condition)?.is_truthy() {
                    self.evaluate(dom, consequent)
                } else {
                    self.evaluate(dom, alternate)
                }
            }
            Expr::Update {
                target,
                operator,
                prefix,
            } => {
                let reference = self.resolve_assignment_reference(dom, target)?;
                let previous = self.read_assignment_reference(dom, &reference)?;
                let next =
                    Self::evaluate_binary_values(*operator, &previous, &JsValue::Number(1.0))?;
                self.write_assignment_reference(dom, &reference, next.clone())?;
                Ok(if *prefix { next } else { previous })
            }
            Expr::Member { object, property } => {
                let evaluated = self.evaluate(dom, object)?;
                if matches!(evaluated, JsValue::Null | JsValue::Undefined) {
                    return Ok(JsValue::Undefined);
                }
                let object = self.coerce_member_base(&evaluated, property)?;
                self.get_member(dom, object, property)
            }
            Expr::ComputedMember { object, property } => {
                let evaluated = self.evaluate(dom, object)?;
                if matches!(evaluated, JsValue::Null | JsValue::Undefined) {
                    return Ok(JsValue::Undefined);
                }
                let key = self.evaluate(dom, property)?.to_js_string();
                let object = self.coerce_member_base(&evaluated, &key)?;
                self.get_member(dom, object, &key)
            }
            Expr::New {
                constructor,
                arguments,
            } => {
                let evaluated = self.evaluate(dom, constructor)?;
                if matches!(evaluated, JsValue::Null | JsValue::Undefined) {
                    return Ok(JsValue::Undefined);
                }
                let JsValue::Object(constructor) = evaluated else {
                    // An unavailable feature constructor evaluates to a
                    // primitive in some compatibility branches.  Treat that
                    // branch as an inert construction result so the rest of
                    // the page can continue initializing.
                    return Ok(JsValue::Undefined);
                };
                let mut values = Vec::with_capacity(arguments.len());
                for argument in arguments {
                    self.evaluate_argument(dom, argument, &mut values)?;
                }
                self.construct(dom, constructor, &values)
            }
            Expr::Call { callee, arguments } => self.evaluate_call(dom, callee, arguments),
            Expr::Sequence(expressions) => {
                let mut value = JsValue::Undefined;
                for expression in expressions {
                    value = self.evaluate(dom, expression)?;
                }
                Ok(value)
            }
            Expr::CompoundAssignment {
                target,
                operator,
                value,
            } => {
                let reference = self.resolve_assignment_reference(dom, target)?;
                let current = self.read_assignment_reference(dom, &reference)?;
                let right = self.evaluate(dom, value)?;
                let combined = Self::evaluate_binary_values(*operator, &current, &right)?;
                self.write_assignment_reference(dom, &reference, combined.clone())?;
                Ok(combined)
            }
            Expr::Assignment { target, value } => {
                if matches!(target.as_ref(), Expr::Array(_) | Expr::Object(_)) {
                    let value = self.evaluate(dom, value)?;
                    self.assign_destructuring_target(dom, target, value.clone())?;
                    return Ok(value);
                }
                let reference = self.resolve_assignment_reference(dom, target)?;
                let value = self.evaluate(dom, value)?;
                self.write_assignment_reference(dom, &reference, value.clone())?;
                Ok(value)
            }
        }
    }

    fn resolve_assignment_reference(
        &mut self,
        dom: &mut Dom,
        target: &Expr,
    ) -> Result<AssignmentReference, JsError> {
        match target {
            Expr::Identifier(name) => Ok(AssignmentReference::Binding(name.clone())),
            Expr::Member { object, property } => {
                let evaluated = self.evaluate(dom, object)?;
                let object = if matches!(evaluated, JsValue::Null | JsValue::Undefined) {
                    // Keep optional polyfill assignments harmless when their
                    // feature-detected receiver is absent.
                    self.realm.global_object()
                } else {
                    self.coerce_member_base(&evaluated, property)?
                };
                Ok(AssignmentReference::Property {
                    object,
                    property: property.clone(),
                })
            }
            Expr::ComputedMember { object, property } => {
                let evaluated = self.evaluate(dom, object)?;
                let key = self.evaluate(dom, property)?.to_js_string();
                let object = if matches!(evaluated, JsValue::Null | JsValue::Undefined) {
                    self.realm.global_object()
                } else {
                    self.coerce_member_base(&evaluated, &key)?
                };
                Ok(AssignmentReference::Property {
                    object,
                    property: key,
                })
            }
            _ => Err(JsError::syntax("invalid assignment target", 0)),
        }
    }

    fn assign_destructuring_target(
        &mut self,
        dom: &mut Dom,
        target: &Expr,
        value: JsValue,
    ) -> Result<(), JsError> {
        match target {
            Expr::Identifier(_) | Expr::Member { .. } | Expr::ComputedMember { .. } => {
                let reference = self.resolve_assignment_reference(dom, target)?;
                self.write_assignment_reference(dom, &reference, value)
            }
            Expr::Assignment {
                target,
                value: default,
            } => {
                let value = if matches!(value, JsValue::Undefined) {
                    self.evaluate(dom, default)?
                } else {
                    value
                };
                self.assign_destructuring_target(dom, target, value)
            }
            Expr::Array(targets) => {
                let values = match value {
                    JsValue::Object(object) => self.array_elements_for(object),
                    JsValue::String(text) => text
                        .chars()
                        .map(|character| JsValue::String(character.to_string()))
                        .collect(),
                    _ => return Err(JsError::type_error("value is not iterable")),
                };
                for (index, target) in targets.iter().enumerate() {
                    if matches!(target, Expr::Literal(JsValue::Undefined)) {
                        continue;
                    }
                    if let Expr::Spread(target) = target {
                        let rest =
                            self.create_array_from_values(values.get(index..).unwrap_or_default())?;
                        self.assign_destructuring_target(dom, target, JsValue::Object(rest))?;
                        break;
                    }
                    self.assign_destructuring_target(
                        dom,
                        target,
                        values.get(index).cloned().unwrap_or(JsValue::Undefined),
                    )?;
                }
                Ok(())
            }
            Expr::Object(properties) => {
                let object = match value {
                    JsValue::Null | JsValue::Undefined => self.realm.create_ordinary_object(),
                    JsValue::Object(object) => object,
                    _ => Self::require_object(&value)?,
                };
                let mut excluded = Vec::new();
                for property in properties {
                    if matches!(&property.key, PropertyKey::Spread) {
                        let rest = self.create_object_rest(&value, &excluded)?;
                        self.assign_destructuring_target(
                            dom,
                            &property.value,
                            JsValue::Object(rest),
                        )?;
                        continue;
                    }
                    let key = match &property.key {
                        PropertyKey::Static(key) => key.clone(),
                        PropertyKey::Computed(expression) => {
                            self.evaluate(dom, expression)?.to_js_string()
                        }
                        PropertyKey::Spread => unreachable!("spread handled above"),
                    };
                    let property_value = self.get_member(dom, object, &key)?;
                    excluded.push(key);
                    self.assign_destructuring_target(dom, &property.value, property_value)?;
                }
                Ok(())
            }
            _ => Err(JsError::syntax(
                "invalid destructuring assignment target",
                0,
            )),
        }
    }

    fn read_assignment_reference(
        &mut self,
        dom: &Dom,
        reference: &AssignmentReference,
    ) -> Result<JsValue, JsError> {
        match reference {
            AssignmentReference::Binding(name) => self.lookup_binding(name),
            AssignmentReference::Property { object, property } => {
                self.get_member(dom, *object, property)
            }
        }
    }

    fn write_assignment_reference(
        &mut self,
        dom: &mut Dom,
        reference: &AssignmentReference,
        value: JsValue,
    ) -> Result<(), JsError> {
        match reference {
            AssignmentReference::Binding(name) => self.assign_binding(name, value),
            AssignmentReference::Property { object, property } => {
                self.set_member(dom, *object, property, value)
            }
        }
    }

    fn evaluate_delete(&mut self, dom: &mut Dom, operand: &Expr) -> Result<JsValue, JsError> {
        match operand {
            Expr::Member { .. } | Expr::ComputedMember { .. } => {
                let reference = self.resolve_assignment_reference(dom, operand)?;
                let AssignmentReference::Property { object, property } = reference else {
                    unreachable!("member expressions resolve to property references");
                };
                Ok(JsValue::Boolean(
                    self.realm.delete_property(object, &property),
                ))
            }
            Expr::Identifier(_) => Ok(JsValue::Boolean(false)),
            _ => {
                self.evaluate(dom, operand)?;
                Ok(JsValue::Boolean(true))
            }
        }
    }

    /// Numeric-hint `ToPrimitive` for the hosts we can convert directly.
    fn numeric_primitive(&self, value: &JsValue) -> JsValue {
        if let JsValue::Object(object) = value {
            return match self.realm.host(*object) {
                Some(ObjectHost::DateInstance(ms)) => JsValue::Number(ms),
                Some(ObjectHost::StringPrimitive(text)) => JsValue::String(text.clone()),
                Some(ObjectHost::NumberPrimitive(number)) => JsValue::Number(number),
                Some(ObjectHost::BooleanPrimitive(value)) => JsValue::Boolean(value),
                _ => value.clone(),
            };
        }
        value.clone()
    }

    fn to_numeric_primitive(&mut self, dom: &mut Dom, value: JsValue) -> Result<JsValue, JsError> {
        let JsValue::Object(object) = value else {
            return Ok(value);
        };
        match self.realm.host(object) {
            Some(ObjectHost::DateInstance(ms)) => return Ok(JsValue::Number(ms)),
            Some(ObjectHost::StringPrimitive(text)) => return Ok(JsValue::String(text)),
            Some(ObjectHost::NumberPrimitive(number)) => return Ok(JsValue::Number(number)),
            Some(ObjectHost::BooleanPrimitive(value)) => return Ok(JsValue::Boolean(value)),
            Some(ObjectHost::Array) => {
                let text = self
                    .array_elements_for(object)
                    .iter()
                    .map(|value| match value {
                        JsValue::Null | JsValue::Undefined => String::new(),
                        value => value.to_js_string(),
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                return Ok(JsValue::String(text));
            }
            _ => {}
        }
        for method in ["valueOf", "toString"] {
            let Some(JsValue::Object(callable)) = self.realm.get_property(object, method) else {
                continue;
            };
            if !Self::is_callable_object(callable, &self.realm) {
                continue;
            }
            let result = self.call_with_this(dom, callable, &[], JsValue::Object(object))?;
            if !matches!(result, JsValue::Object(_)) {
                return Ok(result);
            }
        }
        // Host objects without a usable `valueOf`/`toString` still have the
        // ordinary object string representation.  Returning it keeps string
        // concatenation and URL/logging code from aborting on an incomplete
        // platform object.
        Ok(JsValue::String("[object Object]".to_owned()))
    }

    /// Whether `name` resolves in any scope, the global bindings, or the
    /// global object itself.
    fn binding_exists(&self, name: &str) -> bool {
        if self
            .environment
            .iter()
            .rev()
            .any(|scope| scope.borrow().bindings.contains_key(name))
        {
            return true;
        }
        self.global_bindings.contains_key(name) || self.realm.global(name).is_some()
    }

    fn evaluate_binary_values(
        operator: BinaryOp,
        left: &JsValue,
        right: &JsValue,
    ) -> Result<JsValue, JsError> {
        match operator {
            BinaryOp::Add => {
                if matches!(left, JsValue::String(_)) || matches!(right, JsValue::String(_)) {
                    Ok(JsValue::String(format!(
                        "{}{}",
                        left.to_js_string(),
                        right.to_js_string()
                    )))
                } else {
                    Ok(JsValue::Number(to_number(left)? + to_number(right)?))
                }
            }
            BinaryOp::Subtract => Ok(JsValue::Number(to_number(left)? - to_number(right)?)),
            BinaryOp::BitwiseAnd => bitwise_binary(left, right, |left, right| left & right),
            BinaryOp::BitwiseXor => bitwise_binary(left, right, |left, right| left ^ right),
            BinaryOp::BitwiseOr => bitwise_binary(left, right, |left, right| left | right),
            BinaryOp::LeftShift => shift_left(left, right),
            BinaryOp::RightShift => shift_right(left, right),
            BinaryOp::UnsignedRightShift => unsigned_shift_right(left, right),
            _ => Err(JsError::type_error(
                "unsupported compound assignment operator",
            )),
        }
    }

    fn evaluate_call(
        &mut self,
        dom: &mut Dom,
        callee: &Expr,
        arguments: &[Expr],
    ) -> Result<JsValue, JsError> {
        let callee_label = match callee {
            Expr::Member { property, .. } => format!(".{property}"),
            Expr::ComputedMember { .. } => "[]".to_owned(),
            _ => String::new(),
        };
        let (callee_value, receiver) = match callee {
            Expr::Member { object, property } => {
                let receiver = self.evaluate(dom, object)?;
                if matches!(receiver, JsValue::Null | JsValue::Undefined) {
                    return Ok(JsValue::Undefined);
                }
                let object = self.coerce_member_base(&receiver, property)?;
                (self.get_member(dom, object, property)?, receiver)
            }
            Expr::ComputedMember { object, property } => {
                let receiver = self.evaluate(dom, object)?;
                if matches!(receiver, JsValue::Null | JsValue::Undefined) {
                    return Ok(JsValue::Undefined);
                }
                let key = self.evaluate(dom, property)?.to_js_string();
                let object = self.coerce_member_base(&receiver, &key)?;
                (self.get_member(dom, object, &key)?, receiver)
            }
            _ => (self.evaluate(dom, callee)?, JsValue::Undefined),
        };
        let callee = match callee_value {
            JsValue::Undefined | JsValue::Null => {
                // Web pages routinely feature-detect optional host methods
                // through a call guarded by a surrounding branch. Treat a
                // missing host hook as an inert call so one telemetry shim
                // cannot abort the entire application bootstrap.
                return Ok(JsValue::Undefined);
            }
            JsValue::String(_) | JsValue::Number(_) | JsValue::Boolean(_) => {
                return Ok(JsValue::Undefined);
            }
            value => Self::require_object(&value).map_err(|_| {
                JsError::type_error(format!(
                    "value of callee{callee_label} is undefined or not callable"
                ))
            })?,
        };
        let mut values = Vec::with_capacity(arguments.len());
        for argument in arguments {
            self.evaluate_argument(dom, argument, &mut values)?;
        }
        self.call_with_this(dom, callee, &values, receiver)
    }

    fn evaluate_function_expression(
        &mut self,
        name: Option<&str>,
        parameters: &[String],
        body: &[Statement],
    ) -> Result<JsValue, JsError> {
        let Some(name) = name else {
            return self.create_user_function(parameters, body);
        };
        self.environment
            .push(Rc::new(RefCell::new(EnvironmentRecord::default())));
        let result = (|| {
            self.create_binding(name, VariableKind::Const, false, JsValue::Undefined)?;
            let value = self.create_user_function(parameters, body)?;
            self.initialize_binding(name, value.clone(), VariableKind::Const)?;
            Ok(value)
        })();
        self.environment.pop();
        result
    }

    fn evaluate_object_literal(
        &mut self,
        dom: &mut Dom,
        properties: &[ObjectProperty],
    ) -> Result<JsValue, JsError> {
        self.ensure_heap_capacity(1)?;
        let object = self.realm.create_ordinary_object();
        for property in properties {
            if matches!(property.key, PropertyKey::Spread) {
                match self.evaluate(dom, &property.value)? {
                    JsValue::Null | JsValue::Undefined => {}
                    JsValue::Object(source) => {
                        for (key, value) in self
                            .realm
                            .enumerable_own_properties(source)
                            .unwrap_or_default()
                        {
                            if !self.realm.set_property(object, key, value) {
                                return Err(JsError::type_error(
                                    "could not define spread object property",
                                ));
                            }
                        }
                    }
                    JsValue::String(text) => {
                        for (index, character) in text.chars().enumerate() {
                            if !self.realm.set_property(
                                object,
                                index.to_string(),
                                JsValue::String(character.to_string()),
                            ) {
                                return Err(JsError::type_error(
                                    "could not define spread string property",
                                ));
                            }
                        }
                    }
                    JsValue::Boolean(_) | JsValue::Number(_) => {}
                }
                continue;
            }
            let key = match &property.key {
                PropertyKey::Static(key) => key.clone(),
                PropertyKey::Computed(expression) => self.evaluate(dom, expression)?.to_js_string(),
                PropertyKey::Spread => unreachable!("spread property handled above"),
            };
            let value = self.evaluate(dom, &property.value)?;
            if !self.realm.set_property(object, key, value) {
                return Err(JsError::type_error("could not define object property"));
            }
        }
        Ok(JsValue::Object(object))
    }

    fn create_object_rest(
        &mut self,
        value: &JsValue,
        excluded: &[String],
    ) -> Result<ObjectId, JsError> {
        if matches!(value, JsValue::Null | JsValue::Undefined) {
            return Ok(self.realm.create_ordinary_object());
        }
        let source = Self::require_object(value)?;
        self.ensure_heap_capacity(1)?;
        let result = self.realm.create_ordinary_object();
        for (key, value) in self
            .realm
            .enumerable_own_properties(source)
            .unwrap_or_default()
        {
            if !excluded.contains(&key) && !self.realm.set_property(result, key, value) {
                return Err(JsError::type_error("could not define object rest property"));
            }
        }
        Ok(result)
    }

    fn image_constructor(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        if self.dom_nodes_created >= self.limits.max_dom_nodes_created {
            return Err(JsError::resource("DOM node creation limit exceeded"));
        }
        self.ensure_heap_capacity(1)?;
        self.dom_nodes_created = self.dom_nodes_created.saturating_add(1);
        let image = dom.create_element("img");
        for (index, name) in [(0, "width"), (1, "height")] {
            if let Some(value) = arguments.get(index)
                && !matches!(value, JsValue::Undefined)
            {
                let number = to_number(value)?.max(0.0).floor();
                dom.set_attribute(image, name, number.to_string())?;
            }
        }
        self.wrap_node(image)
    }

    fn evaluate_array_literal(
        &mut self,
        dom: &mut Dom,
        elements: &[Expr],
    ) -> Result<JsValue, JsError> {
        self.ensure_heap_capacity(1)?;
        let object = self.realm.create_array();
        let mut values = Vec::with_capacity(elements.len());
        for expression in elements {
            self.evaluate_argument(dom, expression, &mut values)?;
        }
        for (index, value) in values.iter().cloned().enumerate() {
            if !self.realm.set_property(object, index.to_string(), value) {
                return Err(JsError::type_error("could not define array element"));
            }
        }
        let length = u32::try_from(values.len())
            .map_err(|_| JsError::resource("array literal exceeds the supported u32 range"))?;
        if !self.realm.set_property(
            object,
            "length".to_owned(),
            JsValue::Number(f64::from(length)),
        ) {
            return Err(JsError::type_error("could not define array length"));
        }
        Ok(JsValue::Object(object))
    }

    fn evaluate_argument(
        &mut self,
        dom: &mut Dom,
        expression: &Expr,
        output: &mut Vec<JsValue>,
    ) -> Result<(), JsError> {
        let Expr::Spread(iterable) = expression else {
            output.push(self.evaluate(dom, expression)?);
            return Ok(());
        };
        match self.evaluate(dom, iterable)? {
            JsValue::Object(object) => output.extend(self.array_elements_for(object)),
            JsValue::String(text) => {
                output.extend(
                    text.chars()
                        .map(|character| JsValue::String(character.to_string())),
                );
            }
            _ => return Err(JsError::type_error("spread value is not iterable")),
        }
        Ok(())
    }

    fn evaluate_unary(&self, operator: UnaryOp, value: &JsValue) -> Result<JsValue, JsError> {
        match operator {
            UnaryOp::Not => Ok(JsValue::Boolean(!value.is_truthy())),
            UnaryOp::Typeof => Ok(JsValue::String(
                match value {
                    JsValue::Undefined => "undefined",
                    JsValue::Object(object) if Self::is_callable_object(*object, &self.realm) => {
                        "function"
                    }
                    JsValue::Null | JsValue::Object(_) => "object",
                    JsValue::Boolean(_) => "boolean",
                    JsValue::Number(_) => "number",
                    JsValue::String(_) => "string",
                }
                .to_owned(),
            )),
            UnaryOp::Plus => Ok(JsValue::Number(to_number(&self.numeric_primitive(value))?)),
            UnaryOp::Minus => Ok(JsValue::Number(-to_number(&self.numeric_primitive(value))?)),
            UnaryOp::BitwiseNot => Ok(JsValue::Number(f64::from(!to_int32(value)?))),
            UnaryOp::Void => Ok(JsValue::Undefined),
            UnaryOp::Delete => unreachable!("delete evaluates an assignment reference"),
        }
    }

    fn evaluate_binary(
        &mut self,
        dom: &mut Dom,
        operator: BinaryOp,
        left: &Expr,
        right: &Expr,
    ) -> Result<JsValue, JsError> {
        let left = self.evaluate(dom, left)?;
        if operator == BinaryOp::LogicalAnd && !left.is_truthy() {
            return Ok(left);
        }
        if operator == BinaryOp::LogicalOr && left.is_truthy() {
            return Ok(left);
        }
        let right = self.evaluate(dom, right)?;
        if operator == BinaryOp::Instanceof {
            return self.instanceof(&left, &right).map(JsValue::Boolean);
        }
        if operator == BinaryOp::In {
            return self.property_in(&left, &right).map(JsValue::Boolean);
        }
        let left = self.to_numeric_primitive(dom, left)?;
        let right = self.to_numeric_primitive(dom, right)?;
        match operator {
            BinaryOp::LogicalAnd | BinaryOp::LogicalOr => Ok(right),
            BinaryOp::Add => {
                if matches!(left, JsValue::String(_)) || matches!(right, JsValue::String(_)) {
                    Ok(JsValue::String(format!(
                        "{}{}",
                        left.to_js_string(),
                        right.to_js_string()
                    )))
                } else {
                    Ok(JsValue::Number(to_number(&left)? + to_number(&right)?))
                }
            }
            BinaryOp::Subtract => Ok(JsValue::Number(to_number(&left)? - to_number(&right)?)),
            BinaryOp::Multiply => Ok(JsValue::Number(to_number(&left)? * to_number(&right)?)),
            BinaryOp::Exponentiate => {
                Ok(JsValue::Number(to_number(&left)?.powf(to_number(&right)?)))
            }
            BinaryOp::Divide => Ok(JsValue::Number(to_number(&left)? / to_number(&right)?)),
            BinaryOp::Remainder => Ok(JsValue::Number(to_number(&left)? % to_number(&right)?)),
            BinaryOp::BitwiseAnd => bitwise_binary(&left, &right, |left, right| left & right),
            BinaryOp::BitwiseXor => bitwise_binary(&left, &right, |left, right| left ^ right),
            BinaryOp::BitwiseOr => bitwise_binary(&left, &right, |left, right| left | right),
            BinaryOp::LeftShift => shift_left(&left, &right),
            BinaryOp::RightShift => shift_right(&left, &right),
            BinaryOp::UnsignedRightShift => unsigned_shift_right(&left, &right),
            BinaryOp::Less => compare(&left, &right, |a, b| a < b, |a, b| a < b),
            BinaryOp::LessEqual => compare(&left, &right, |a, b| a <= b, |a, b| a <= b),
            BinaryOp::Greater => compare(&left, &right, |a, b| a > b, |a, b| a > b),
            BinaryOp::GreaterEqual => compare(&left, &right, |a, b| a >= b, |a, b| a >= b),
            BinaryOp::StrictEqual => Ok(JsValue::Boolean(strict_equal(&left, &right))),
            BinaryOp::StrictNotEqual => Ok(JsValue::Boolean(!strict_equal(&left, &right))),
            BinaryOp::Equal => Ok(JsValue::Boolean(abstract_equal(&left, &right)?)),
            BinaryOp::NotEqual => Ok(JsValue::Boolean(!abstract_equal(&left, &right)?)),
            BinaryOp::Instanceof => unreachable!("instanceof is handled before numeric operators"),
            BinaryOp::In => unreachable!("in is handled before numeric operators"),
        }
    }

    /// The `in` operator: property existence on objects, index bounds on
    /// strings.
    fn property_in(&self, key: &JsValue, container: &JsValue) -> Result<bool, JsError> {
        let name = key.to_js_string();
        match container {
            JsValue::Object(object) => {
                if self.realm.get_property(*object, &name).is_some() {
                    return Ok(true);
                }
                if let Some(ObjectHost::StringPrimitive(text)) = self.realm.host(*object) {
                    let characters: Vec<char> = text.chars().collect();
                    if let Ok(index) = name.parse::<usize>() {
                        return Ok(index < characters.len());
                    }
                    return Ok(name == "length");
                }
                Ok(false)
            }
            _ => Err(JsError::type_error(
                "right-hand side of 'in' must be an object",
            )),
        }
    }

    fn instanceof(&self, value: &JsValue, constructor: &JsValue) -> Result<bool, JsError> {
        let constructor = Self::require_object(constructor)?;
        if !Self::is_callable_object(constructor, &self.realm) {
            return Err(JsError::type_error(format!(
                "right-hand side of instanceof is not callable: host={:?}",
                self.realm.host(constructor)
            )));
        }
        let prototype = self
            .realm
            .get_property(constructor, "prototype")
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            });
        let Some(prototype) = prototype else {
            // Transpiled feature probes occasionally use a callable shim with
            // a primitive prototype.  It cannot match any object, so the
            // observable result is simply false.
            return Ok(false);
        };
        let JsValue::Object(object) = value else {
            return Ok(false);
        };
        let mut candidate = self.realm.object(*object).and_then(JsObject::prototype);
        let mut visited = 0_usize;
        while let Some(current) = candidate {
            if current == prototype {
                return Ok(true);
            }
            if visited >= self.limits.max_heap_objects {
                return Err(JsError::resource(
                    "prototype chain exceeds the heap object limit",
                ));
            }
            visited = visited.saturating_add(1);
            candidate = self.realm.object(current).and_then(JsObject::prototype);
        }
        Ok(false)
    }

    fn create_binding(
        &mut self,
        name: &str,
        kind: VariableKind,
        initialized: bool,
        value: JsValue,
    ) -> Result<(), JsError> {
        let mutable = kind != VariableKind::Const;
        if self.environment.is_empty() {
            if let Some(existing) = self.global_bindings.get(name) {
                if kind == VariableKind::Var && existing.kind == VariableKind::Var {
                    return Ok(());
                }
                return Err(JsError::syntax(
                    format!("global binding {name:?} is already declared"),
                    0,
                ));
            }
            if !self.realm.set_global(name.to_owned(), value) {
                return Err(JsError::type_error(format!(
                    "global property {name:?} is not writable"
                )));
            }
            self.global_bindings.insert(
                name.to_owned(),
                GlobalBinding {
                    mutable,
                    initialized,
                    kind,
                },
            );
            return Ok(());
        }
        let target_index = if kind == VariableKind::Var {
            self.environment
                .iter()
                .rposition(|scope| scope.borrow().function_scope)
                .unwrap_or_else(|| self.environment.len().saturating_sub(1))
        } else {
            self.environment.len().saturating_sub(1)
        };
        let target = self
            .environment
            .get(target_index)
            .expect("non-empty environment chain has a declaration target");
        let mut target = target.borrow_mut();
        if let Some(existing) = target.bindings.get(name) {
            if kind == VariableKind::Var && existing.kind == VariableKind::Var {
                return Ok(());
            }
            return Err(JsError::syntax(
                format!("binding {name:?} is already declared in this scope"),
                0,
            ));
        }
        target.bindings.insert(
            name.to_owned(),
            Binding {
                value,
                mutable,
                initialized,
                kind,
            },
        );
        if Self::binding_trace_enabled() && name == "document" {
            eprintln!(
                "[bind create document depth={} scopes={} initialized={initialized} | stack {:?}]",
                self.calls_active,
                self.environment.len(),
                self.call_stack
            );
        }
        Ok(())
    }

    fn initialize_binding(
        &mut self,
        name: &str,
        value: JsValue,
        kind: VariableKind,
    ) -> Result<(), JsError> {
        for scope in self.environment.iter().rev() {
            let mut scope = scope.borrow_mut();
            if let Some(binding) = scope.bindings.get_mut(name) {
                if binding.initialized && kind != VariableKind::Var {
                    return Err(JsError::syntax(
                        format!("binding {name:?} is already initialized"),
                        0,
                    ));
                }
                binding.value = value;
                binding.initialized = true;
                if Self::binding_trace_enabled() && name == "document" {
                    let assigned_object = matches!(binding.value, JsValue::Object(_));
                    eprintln!(
                        "[bind assign document depth={} scopes={} is_object={assigned_object}]",
                        self.calls_active,
                        self.environment.len()
                    );
                }
                return Ok(());
            }
        }
        if let Some(binding) = self.global_bindings.get_mut(name) {
            if binding.initialized && kind != VariableKind::Var {
                return Err(JsError::syntax(
                    format!("global binding {name:?} is already initialized"),
                    0,
                ));
            }
            binding.initialized = true;
            if self.realm.set_global(name.to_owned(), value) {
                return Ok(());
            }
            return Err(JsError::type_error(format!(
                "global property {name:?} is not writable"
            )));
        }
        Err(JsError::reference(format!("{name} is not defined")))
    }

    fn lookup_binding(&self, name: &str) -> Result<JsValue, JsError> {
        for scope in self.environment.iter().rev() {
            if let Some(binding) = scope.borrow().bindings.get(name) {
                if !binding.initialized {
                    return Err(JsError::reference(format!(
                        "cannot access {name} before initialization"
                    )));
                }
                return Ok(binding.value.clone());
            }
        }
        if let Some(binding) = self.global_bindings.get(name)
            && !binding.initialized
        {
            return Err(JsError::reference(format!(
                "cannot access {name} before initialization"
            )));
        }
        self.realm
            .global(name)
            .ok_or_else(|| JsError::reference(format!("{name} is not defined")))
    }

    fn assign_binding(&mut self, name: &str, value: JsValue) -> Result<(), JsError> {
        for scope in self.environment.iter().rev() {
            let mut scope = scope.borrow_mut();
            if let Some(binding) = scope.bindings.get_mut(name) {
                if !binding.initialized {
                    return Err(JsError::reference(format!(
                        "cannot access {name} before initialization"
                    )));
                }
                if !binding.mutable {
                    return Err(JsError::type_error(format!(
                        "assignment to constant binding {name:?}"
                    )));
                }
                binding.value = value;
                return Ok(());
            }
        }
        if let Some(binding) = self.global_bindings.get(name) {
            if !binding.initialized {
                return Err(JsError::reference(format!(
                    "cannot access {name} before initialization"
                )));
            }
            if !binding.mutable {
                return Err(JsError::type_error(format!(
                    "assignment to constant binding {name:?}"
                )));
            }
        }
        // Implicit global creation (sloppy-mode assignment to undeclared).
        // Per spec, strict mode would throw here; we treat all code as sloppy.
        if self.realm.set_global(name.to_owned(), value) {
            Ok(())
        } else {
            Err(JsError::type_error(format!(
                "global property {name:?} is not writable"
            )))
        }
    }

    #[allow(clippy::too_many_lines)]
    fn get_member(
        &mut self,
        dom: &Dom,
        object: ObjectId,
        property: &str,
    ) -> Result<JsValue, JsError> {
        self.consume_step()?;
        // Own data properties win outright (instance fields such as a
        // RegExp's `source`).
        if let Some(descriptor) = self.realm.own_property(object, property) {
            return Ok(descriptor.value);
        }
        let inherited = self.realm.get_property(object, property);
        if object == self.realm.global_object() {
            let value = match property {
                "innerWidth" | "outerWidth" => Some(self.viewport.width),
                "innerHeight" | "outerHeight" => Some(self.viewport.height),
                "scrollX" | "pageXOffset" => Some(self.viewport.x),
                "scrollY" | "pageYOffset" => Some(self.viewport.y),
                _ => None,
            };
            if let Some(value) = value {
                return Ok(JsValue::Number(f64::from(value)));
            }
        }
        // Node identity properties apply to every node wrapper, including
        // the Document host.
        if matches!(
            self.realm.host(object),
            Some(ObjectHost::Document(_) | ObjectHost::Node(_))
        ) {
            let Some(ObjectHost::Document(node_id) | ObjectHost::Node(node_id)) =
                self.realm.host(object)
            else {
                unreachable!("checked above")
            };
            match property {
                "nodeType" => {
                    return Ok(JsValue::Number(
                        match dom.node(node_id).map(crate::dom::Node::kind) {
                            Some(NodeKind::Element(_)) => 1.0,
                            Some(NodeKind::Text(_)) => 3.0,
                            Some(NodeKind::Comment(_)) => 8.0,
                            Some(NodeKind::Document) => 9.0,
                            Some(NodeKind::DocumentType(_)) => 10.0,
                            Some(NodeKind::DocumentFragment) => 11.0,
                            _ => 0.0,
                        },
                    ));
                }
                "nodeName" | "tagName" => {
                    let name = match dom.node(node_id).map(crate::dom::Node::kind) {
                        Some(NodeKind::Element(element)) => element.local_name.to_ascii_uppercase(),
                        Some(other) => other.name().to_owned(),
                        None => String::new(),
                    };
                    return Ok(JsValue::String(name));
                }
                _ => {}
            }
        }
        match self.realm.host(object) {
            Some(ObjectHost::Document(document)) => match property {
                "documentElement" | "body" | "head" => {
                    let tag = match property {
                        "documentElement" => "html",
                        "body" => "body",
                        _ => "head",
                    };
                    return match self.find_element_by_tag(dom, document, tag)? {
                        Some(node) => self.wrap_node(node),
                        None => Ok(JsValue::Null),
                    };
                }
                "readyState" => return Ok(JsValue::String("complete".to_owned())),
                // The embedding window is the realm's global object.
                "defaultView" | "parentWindow" => {
                    return Ok(JsValue::Object(self.realm.global_object()));
                }
                "activeElement" => {
                    return match self.find_element_by_tag(dom, document, "body")? {
                        Some(node) => self.wrap_node(node),
                        None => Ok(JsValue::Null),
                    };
                }
                _ => {}
            },
            Some(ObjectHost::Node(node)) => match property {
                "textContent" => return self.text_content(dom, node).map(JsValue::String),
                "clientWidth" | "offsetWidth" | "scrollWidth" => {
                    let width = self
                        .element_geometry
                        .get(&node.as_u64())
                        .map_or(0.0, |rect| rect.width);
                    return Ok(JsValue::Number(f64::from(width)));
                }
                "clientHeight" | "offsetHeight" | "scrollHeight" => {
                    let height = self
                        .element_geometry
                        .get(&node.as_u64())
                        .map_or(0.0, |rect| rect.height);
                    return Ok(JsValue::Number(f64::from(height)));
                }
                "attributes" => {
                    self.ensure_heap_capacity(1)?;
                    return Ok(JsValue::Object(self.realm.named_node_map_wrapper(node)));
                }
                "classList" => {
                    self.ensure_heap_capacity(1)?;
                    return Ok(JsValue::Object(self.realm.class_list_wrapper(node)));
                }
                "style" => {
                    self.ensure_heap_capacity(1)?;
                    return Ok(JsValue::Object(self.realm.style_declaration_wrapper(node)));
                }
                "innerHTML" | "outerHTML" => {
                    let html = if property == "innerHTML" {
                        serialize_html_fragment(dom, node)
                    } else {
                        serialize_html_node(dom, node)
                    };
                    return Ok(JsValue::String(html));
                }
                "className" => {
                    return Ok(JsValue::String(
                        dom.attribute(node, "class")?.unwrap_or_default().to_owned(),
                    ));
                }
                "parentNode" | "parentElement" => {
                    let parent = dom.parent(node).filter(|parent| {
                        property == "parentNode"
                            || matches!(
                                dom.node(*parent).map(crate::dom::Node::kind),
                                Some(NodeKind::Element(_))
                            )
                    });
                    return match parent {
                        Some(parent) => self.wrap_node(parent),
                        None => Ok(JsValue::Null),
                    };
                }
                "firstChild" | "lastChild" | "nextSibling" | "previousSibling" => {
                    let related = match property {
                        "firstChild" => dom
                            .children(node)
                            .and_then(|children| children.first())
                            .copied(),
                        "lastChild" => dom
                            .children(node)
                            .and_then(|children| children.last())
                            .copied(),
                        "nextSibling" => dom.next_sibling(node),
                        _ => dom.previous_sibling(node),
                    };
                    return match related {
                        Some(related) => self.wrap_node(related),
                        None => Ok(JsValue::Null),
                    };
                }
                "ownerDocument" => return Ok(JsValue::Object(self.realm.document_object())),
                "children" | "childNodes" => {
                    let elements_only = property == "children";
                    let values = dom
                        .children(node)
                        .unwrap_or_default()
                        .iter()
                        .copied()
                        .filter(|child| {
                            !elements_only
                                || matches!(
                                    dom.node(*child).map(crate::dom::Node::kind),
                                    Some(NodeKind::Element(_))
                                )
                        })
                        .map(|child| self.wrap_node(child))
                        .collect::<Result<Vec<_>, _>>()?;
                    return Ok(JsValue::Object(self.create_array_from_values(&values)?));
                }
                property if property.starts_with("on") && property.len() > 2 => {
                    let event_type = property[2..].to_ascii_lowercase();
                    return Ok(self
                        .event_handlers
                        .get(&node)
                        .and_then(|handlers| handlers.get(&event_type))
                        .copied()
                        .map_or(JsValue::Null, JsValue::Object));
                }
                property if node_attribute_property(property).is_some() => {
                    let attribute = node_attribute_property(property).expect("checked above");
                    if node_boolean_property(property) {
                        return Ok(JsValue::Boolean(dom.attribute(node, attribute)?.is_some()));
                    }
                    return Ok(JsValue::String(
                        dom.attribute(node, attribute)?
                            .unwrap_or_default()
                            .to_owned(),
                    ));
                }
                _ => {}
            },
            Some(ObjectHost::NamedNodeMap(node)) => {
                let attributes = match dom.node(node).map(crate::dom::Node::kind) {
                    Some(NodeKind::Element(element)) => &element.attributes,
                    _ => &Vec::new(),
                };
                if property == "length" {
                    #[allow(
                        clippy::cast_precision_loss,
                        reason = "attribute counts stay far below any precision boundary"
                    )]
                    return Ok(JsValue::Number(attributes.len() as f64));
                }
                if let Ok(index) = property.parse::<usize>() {
                    return match attributes.get(index) {
                        Some(attribute) => {
                            self.ensure_heap_capacity(1)?;
                            Ok(JsValue::Object(self.realm.attr_wrapper(
                                node,
                                attribute.local_name.clone(),
                            )))
                        }
                        None => Ok(JsValue::Undefined),
                    };
                }
                // Named access: the attribute node, or `null` per spec.
                if let Some(attribute) = attributes
                    .iter()
                    .find(|candidate| candidate.local_name == property)
                {
                    self.ensure_heap_capacity(1)?;
                    let name = attribute.local_name.clone();
                    return Ok(JsValue::Object(self.realm.attr_wrapper(node, name)));
                }
                return Ok(JsValue::Null);
            }
            Some(ObjectHost::Attr { owner, name }) => {
                let value = match dom.attribute(owner, &name.clone())? {
                    Some(value) => value.to_owned(),
                    None => String::new(),
                };
                match property {
                    "name" | "nodeName" => return Ok(JsValue::String(name.clone())),
                    "value" | "nodeValue" | "textContent" => {
                        return Ok(JsValue::String(value));
                    }
                    "specified" => return Ok(JsValue::Boolean(true)),
                    _ => {}
                }
            }
            Some(ObjectHost::ClassList(node)) => match property {
                "length" => {
                    #[allow(
                        clippy::cast_precision_loss,
                        reason = "class-list sizes are far below any precision boundary"
                    )]
                    let length = Self::class_list_tokens(dom, node)?.len() as f64;
                    return Ok(JsValue::Number(length));
                }
                "value" => {
                    return Ok(JsValue::String(
                        Self::class_list_tokens(dom, node)?.join(" "),
                    ));
                }
                _ => {}
            },
            Some(ObjectHost::CssStyleDeclaration(node)) => {
                if property == "cssText" {
                    let declarations = Self::inline_declarations(dom, node);
                    let text = declarations
                        .iter()
                        .map(|(name, value, important)| {
                            if *important {
                                format!("{name}: {value} !important;")
                            } else {
                                format!("{name}: {value};")
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    return Ok(JsValue::String(text));
                }
                if property == "length" || property.parse::<usize>().is_ok() {
                    let declarations = Self::inline_declarations(dom, node);
                    if property == "length" {
                        #[allow(
                            clippy::cast_precision_loss,
                            reason = "declaration counts are far below any precision boundary"
                        )]
                        let length = declarations.len() as f64;
                        return Ok(JsValue::Number(length));
                    }
                    if let Ok(index) = property.parse::<usize>() {
                        return Ok(match declarations.get(index) {
                            Some((name, _, _)) => JsValue::String(name.clone()),
                            None => JsValue::Undefined,
                        });
                    }
                }
                // camelCase member access maps to kebab-case properties, but
                // the interface's methods always take precedence.
                if !STYLE_METHOD_PROPERTIES.contains(&property) {
                    let css_name = css_prop_from_member(property);
                    if is_valid_property_name(&css_name) {
                        return Ok(JsValue::String(
                            Self::inline_declarations(dom, node)
                                .into_iter()
                                .find(|(name, _, _)| *name == css_name)
                                .map_or_else(String::new, |(_, value, _)| value),
                        ));
                    }
                }
            }
            Some(ObjectHost::RegExp(index))
                // `lastIndex` lives in the record so exec/test updates stay
                // visible even when the cached property lags behind.
                if property == "lastIndex" => {
                    #[allow(
                        clippy::cast_precision_loss,
                        reason = "string lengths stay far below any precision boundary"
                    )]
                    let last_index = self.regexes[index].last_index as f64;
                    return Ok(JsValue::Number(last_index));
                }
            Some(ObjectHost::Collection { kind, entries })
                if property == "size" && !kind.is_weak() =>
            {
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "collection sizes are bounded by the JavaScript heap limit"
                )]
                return Ok(JsValue::Number(entries.len() as f64));
            }
            Some(ObjectHost::StringPrimitive(text)) => {
                let characters: Vec<char> = text.chars().collect();
                match property {
                    "length" => {
                        #[allow(
                            clippy::cast_precision_loss,
                            reason = "string lengths stay far below any precision boundary"
                        )]
                        let length = characters.len() as f64;
                        return Ok(JsValue::Number(length));
                    }
                    _ => {
                        if let Ok(index) = property.parse::<usize>() {
                            return Ok(characters
                                .get(index)
                                .map_or(JsValue::Undefined, |character| {
                                    JsValue::String(character.to_string())
                                }));
                        }
                    }
                }
                // Method access falls through to the table below and binds
                // to this wrapper instance.
            }
            _ => {}
        }
        // Element wrappers have a real shared prototype. Respect overrides
        // installed on `Element.prototype` before falling back to synthesized
        // legacy host methods, and preserve method identity across reads.
        if matches!(self.realm.host(object), Some(ObjectHost::Node(_)))
            && let Some(value) = inherited.clone()
        {
            return Ok(value);
        }
        // Arrays expose their standard prototype methods through the shared
        // prototype object. Keep those methods visible before the synthesized
        // host dispatch table is consulted (forEach/map/filter are ordinary
        // inherited functions, not array-specific host properties).
        if matches!(
            self.realm.host(object),
            Some(ObjectHost::Array | ObjectHost::Collection { .. })
        ) && let Some(value) = inherited.clone()
        {
            return Ok(value);
        }
        let function = match (self.realm.host(object), property) {
            (Some(ObjectHost::Promise(_)), "then") => Some(NativeFunction::PromiseThen),
            (Some(ObjectHost::Promise(_)), "catch") => Some(NativeFunction::PromiseCatch),
            (_, "addEventListener") if object == self.realm.global_object() => {
                Some(NativeFunction::WindowAddEventListener)
            }
            (_, "removeEventListener") if object == self.realm.global_object() => {
                Some(NativeFunction::WindowRemoveEventListener)
            }
            (Some(ObjectHost::Document(_)), "getElementById") => {
                Some(NativeFunction::GetElementById)
            }
            (Some(ObjectHost::Document(_) | ObjectHost::Node(_)), "querySelector") => {
                Some(NativeFunction::QuerySelector)
            }
            (Some(ObjectHost::Document(_) | ObjectHost::Node(_)), "querySelectorAll") => {
                Some(NativeFunction::QuerySelectorAll)
            }
            (Some(ObjectHost::Document(_) | ObjectHost::Node(_)), "getElementsByTagName") => {
                Some(NativeFunction::GetElementsByTagName)
            }
            (Some(ObjectHost::Document(_) | ObjectHost::Node(_)), "getElementsByClassName") => {
                Some(NativeFunction::GetElementsByClassName)
            }
            (Some(ObjectHost::Document(_) | ObjectHost::Node(_)), "cloneNode") => {
                Some(NativeFunction::CloneNode)
            }
            (Some(ObjectHost::Document(_)), "createTextNode") => {
                Some(NativeFunction::CreateTextNode)
            }
            (Some(ObjectHost::Document(_)), "createDocumentFragment") => {
                Some(NativeFunction::CreateDocumentFragment)
            }
            (Some(ObjectHost::Node(_)), "compareDocumentPosition") => {
                Some(NativeFunction::CompareDocumentPosition)
            }
            (Some(ObjectHost::NamedNodeMap(_)), "item") => Some(NativeFunction::NamedMapItem),
            (Some(ObjectHost::NamedNodeMap(_)), "getNamedItem") => {
                Some(NativeFunction::NamedMapGetNamedItem)
            }
            (Some(ObjectHost::Attr { .. }), "getName") => Some(NativeFunction::AttrGetName),
            (Some(ObjectHost::Attr { .. }), "getValue") => Some(NativeFunction::AttrGetValue),
            (Some(ObjectHost::Document(_)), "createElement") => Some(NativeFunction::CreateElement),
            (Some(ObjectHost::Document(_) | ObjectHost::Node(_)), "addEventListener") => {
                Some(NativeFunction::AddEventListener)
            }
            (Some(ObjectHost::Document(_) | ObjectHost::Node(_)), "removeEventListener") => {
                Some(NativeFunction::RemoveEventListener)
            }
            (Some(ObjectHost::Document(_) | ObjectHost::Node(_)), "dispatchEvent") => {
                Some(NativeFunction::DispatchEvent)
            }
            (Some(ObjectHost::Node(_)), "setAttribute") => Some(NativeFunction::SetAttribute),
            (Some(ObjectHost::Node(_)), "getAttribute") => Some(NativeFunction::GetAttribute),
            (Some(ObjectHost::Node(_)), "hasAttribute") => Some(NativeFunction::HasAttribute),
            (Some(ObjectHost::Node(_)), "removeAttribute") => Some(NativeFunction::RemoveAttribute),
            (Some(ObjectHost::Node(_)), "appendChild") => Some(NativeFunction::AppendChild),
            (Some(ObjectHost::Node(_)), "removeChild") => Some(NativeFunction::RemoveChild),
            (Some(ObjectHost::Node(_)), "insertBefore") => Some(NativeFunction::InsertBefore),
            (Some(ObjectHost::Node(_)), "remove") => Some(NativeFunction::RemoveNode),
            (Some(ObjectHost::Node(_)), "contains") => Some(NativeFunction::Contains),
            (Some(ObjectHost::Node(_)), "matches") => Some(NativeFunction::Matches),
            (Some(ObjectHost::Node(_)), "click") => Some(NativeFunction::Click),
            (Some(ObjectHost::Node(_)), "getBoundingClientRect") => {
                Some(NativeFunction::GetBoundingClientRect)
            }
            (Some(ObjectHost::CssStyleDeclaration(_)), "getPropertyValue") => {
                Some(NativeFunction::StyleGetProperty)
            }
            (Some(ObjectHost::CssStyleDeclaration(_)), "setProperty") => {
                Some(NativeFunction::StyleSetProperty)
            }
            (Some(ObjectHost::CssStyleDeclaration(_)), "removeProperty") => {
                Some(NativeFunction::StyleRemoveProperty)
            }
            (Some(ObjectHost::CssStyleDeclaration(_)), "item") => Some(NativeFunction::StyleItem),
            (Some(ObjectHost::ClassList(_)), "add") => Some(NativeFunction::ClassListAdd),
            (Some(ObjectHost::ClassList(_)), "remove") => Some(NativeFunction::ClassListRemove),
            (Some(ObjectHost::ClassList(_)), "toggle") => Some(NativeFunction::ClassListToggle),
            (Some(ObjectHost::ClassList(_)), "contains") => Some(NativeFunction::ClassListContains),
            (Some(ObjectHost::ClassList(_)), "item") => Some(NativeFunction::ClassListItem),
            (Some(ObjectHost::ClassList(_)), "toString") => Some(NativeFunction::ClassListToString),
            (Some(ObjectHost::RegExp(_)), "exec") => Some(NativeFunction::RegExpExec),
            (Some(ObjectHost::RegExp(_)), "test") => Some(NativeFunction::RegExpTest),
            (Some(ObjectHost::RegExp(_)), "toString") => Some(NativeFunction::RegExpToString),
            (Some(ObjectHost::CollectionIterator { .. }), "next") => {
                Some(NativeFunction::CollectionIteratorNext)
            }
            (Some(ObjectHost::StringPrimitive(_)), name) => string_method_native(name),
            _ => None,
        };
        if let Some(function) = function {
            self.ensure_heap_capacity(1)?;
            return Ok(JsValue::Object(self.realm.bound_function(function, object)));
        }
        // Inherited prototype members come last so host interfaces keep
        // precedence over `Object.prototype` fallbacks.
        if let Some(value) = inherited {
            return Ok(value);
        }
        Ok(JsValue::Undefined)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "host-object write paths each need their own arm"
    )]
    fn set_member(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        property: &str,
        value: JsValue,
    ) -> Result<(), JsError> {
        self.consume_step()?;
        let location_base = match self.realm.host(object) {
            Some(ObjectHost::Location(url)) => Some(url.clone()),
            _ => None,
        };
        if let Some(base) = location_base {
            if property == "href" {
                let target = value.to_js_string();
                let resolved = base.join(&target).map_err(|error| {
                    JsError::dom(format!("invalid navigation URL {target:?}: {error}"))
                })?;
                self.pending_navigations.push(NavigationRequest {
                    url: resolved.to_string(),
                    replace: false,
                });
                return Ok(());
            }
            return Err(JsError::type_error(format!(
                "Location property {property:?} is read-only; navigation is owned by the embedding browser"
            )));
        }
        if let (Some(ObjectHost::Node(node)), "textContent") = (self.realm.host(object), property) {
            return self.set_text_content(dom, node, value.to_js_string());
        }
        if let (Some(ObjectHost::Node(node)), "innerHTML" | "outerHTML") =
            (self.realm.host(object), property)
        {
            let source = match &value {
                JsValue::Null | JsValue::Undefined => String::new(),
                other => other.to_js_string(),
            };
            if property == "innerHTML" {
                return self.set_inner_html(dom, node, &source);
            }
            return self.set_outer_html(dom, node, &source);
        }
        if let Some(ObjectHost::CssStyleDeclaration(node)) = self.realm.host(object) {
            if property == "cssText" {
                let source = value.to_js_string();
                let declarations = parse_declaration_list(&source)
                    .0
                    .into_iter()
                    .map(|declaration| {
                        (
                            declaration.name.to_ascii_lowercase(),
                            declaration.value,
                            declaration.important,
                        )
                    })
                    .collect::<Vec<_>>();
                Self::write_inline_declarations(dom, node, &declarations)?;
                return Ok(());
            }
            // camelCase member assignment maps to kebab-case properties, but
            // the interface's methods always take precedence.
            if !STYLE_METHOD_PROPERTIES.contains(&property) {
                let css_name = css_prop_from_member(property);
                if is_valid_property_name(&css_name) {
                    let mut declarations: Vec<(String, String, bool)> =
                        Self::inline_declarations(dom, node)
                            .into_iter()
                            .filter(|(name, _, _)| *name != css_name)
                            .collect();
                    let css_value = value.to_js_string();
                    let css_value = css_value.trim().to_owned();
                    if !css_value.is_empty() {
                        declarations.push((css_name.clone(), css_value, false));
                    }
                    Self::write_inline_declarations(dom, node, &declarations)?;
                    return Ok(());
                }
            }
        }
        match self.realm.host(object) {
            Some(ObjectHost::Array) => {
                if property == "length" {
                    return self.set_array_length_value(object, &value);
                }
                if let Some(index) = array_index(property) {
                    if !self.realm.set_property(object, property.to_owned(), value) {
                        return Err(JsError::type_error(format!(
                            "property {property:?} is not writable"
                        )));
                    }
                    let length = self.array_length(object)?;
                    if index >= length {
                        let next_length = index.checked_add(1).ok_or_else(|| {
                            JsError::type_error("array length exceeds the supported range")
                        })?;
                        self.set_array_length(object, next_length)?;
                    }
                    return Ok(());
                }
            }
            Some(ObjectHost::Node(node)) => {
                if property == "className" {
                    return Ok(dom.set_attribute(node, "class", value.to_js_string())?);
                }
                if property.starts_with("on") && property.len() > 2 {
                    let event_type = property[2..].to_ascii_lowercase();
                    match value {
                        JsValue::Null | JsValue::Undefined => {
                            if let Some(handlers) = self.event_handlers.get_mut(&node) {
                                handlers.remove(&event_type);
                            }
                        }
                        value => {
                            let callback = Self::require_callable_object(&value, &self.realm)?;
                            self.event_handlers
                                .entry(node)
                                .or_default()
                                .insert(event_type, callback);
                        }
                    }
                    return Ok(());
                }
                if let Some(attribute) = node_attribute_property(property) {
                    if node_boolean_property(property) && !value.is_truthy() {
                        return Ok(dom.remove_attribute(node, attribute)?);
                    }
                    return Ok(dom.set_attribute(node, attribute, value.to_js_string())?);
                }
            }
            Some(ObjectHost::ClassList(node)) if property == "value" => {
                return Ok(dom.set_attribute(node, "class", value.to_js_string())?);
            }
            // Assignments to primitive string wrappers are silently ignored,
            // mirroring how non-strict engines drop them.
            Some(ObjectHost::StringPrimitive(_)) => return Ok(()),
            Some(ObjectHost::RegExp(index)) if property == "lastIndex" => {
                let number = to_number(&value)?;
                #[allow(
                    clippy::cast_sign_loss,
                    clippy::cast_possible_truncation,
                    reason = "negative and fractional indices floor toward zero"
                )]
                let last_index = number.floor().max(0.0) as usize;
                self.regexes[index].last_index = last_index;
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "string lengths stay far below any precision boundary"
                )]
                let stored = last_index as f64;
                self.realm
                    .set_property(object, "lastIndex".to_owned(), JsValue::Number(stored));
                return Ok(());
            }
            _ => {}
        }
        if !self.realm.set_property(object, property.to_owned(), value) {
            return Err(JsError::type_error(format!(
                "property {property:?} is not writable"
            )));
        }
        Ok(())
    }

    fn construct(
        &mut self,
        dom: &mut Dom,
        constructor: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        // Constructors recurse through user code just like calls; keep the
        // same depth accounting so runaway `new` chains stay bounded.
        self.consume_step()?;
        if self.calls_active >= self.limits.max_call_depth {
            return Err(JsError::resource("maximum call depth exceeded"));
        }
        self.calls_active = self.calls_active.saturating_add(1);
        let result = self.construct_dispatch(dom, constructor, arguments);
        self.calls_active = self.calls_active.saturating_sub(1);
        result
    }

    fn construct_dispatch(
        &mut self,
        dom: &mut Dom,
        constructor: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match self.realm.host(constructor) {
            Some(ObjectHost::ObjectConstructor) => self.object_constructor(arguments),
            Some(ObjectHost::ArrayConstructor) => self.array_constructor(arguments),
            Some(ObjectHost::NumberConstructor) => Ok(JsValue::Number(match arguments.first() {
                None | Some(JsValue::Undefined) => 0.0,
                Some(value) => to_number(value)?,
            })),
            Some(ObjectHost::BooleanConstructor) => Ok(JsValue::Boolean(
                arguments.first().is_none_or(JsValue::is_truthy),
            )),
            Some(ObjectHost::DateConstructor) => {
                let ms = Self::date_from_constructor_arguments(arguments)?;
                self.ensure_heap_capacity(1)?;
                Ok(JsValue::Object(self.realm.date_wrapper(ms)))
            }
            Some(ObjectHost::ErrorConstructor(kind)) => {
                self.error_constructor(constructor, kind, arguments)
            }
            Some(ObjectHost::PromiseConstructor) => {
                let executor = Self::require_callable_object(
                    required_argument(arguments, 0, "Promise")?,
                    &self.realm,
                )?;
                let (promise, value) = self.create_promise()?;
                self.ensure_heap_capacity(2)?;
                let resolve = self.realm.promise_settler(promise, true);
                let reject = self.realm.promise_settler(promise, false);
                if let Err(error) = self.call(
                    dom,
                    executor,
                    &[JsValue::Object(resolve), JsValue::Object(reject)],
                ) {
                    if error.kind() == JsErrorKind::ResourceLimit {
                        return Err(error);
                    }
                    let reason = error
                        .thrown_value()
                        .cloned()
                        .unwrap_or_else(|| JsValue::String(error.to_string()));
                    self.reject_promise(promise, &reason);
                }
                Ok(value)
            }
            Some(ObjectHost::EventConstructor) => self.event_constructor(arguments),
            Some(ObjectHost::DomConstructor) => Err(JsError::type_error("Illegal constructor")),
            Some(ObjectHost::ImageConstructor) => self.image_constructor(dom, arguments),
            Some(ObjectHost::IntersectionObserverConstructor) => {
                self.intersection_observer_constructor(constructor, arguments)
            }
            Some(ObjectHost::CollectionConstructor(kind)) => {
                self.collection_constructor(dom, constructor, kind, arguments)
            }
            Some(ObjectHost::RegExpConstructor) => {
                let pattern = required_argument(arguments, 0, "RegExp")?.to_js_string();
                let flags = match arguments.get(1) {
                    None | Some(JsValue::Undefined) => String::new(),
                    Some(value) => value.to_js_string(),
                };
                let object = self.construct_regex(&pattern, &flags)?;
                Ok(JsValue::Object(object))
            }
            Some(ObjectHost::UrlConstructor) => self.url_constructor(constructor, arguments),
            Some(ObjectHost::UrlSearchParamsConstructor) => {
                self.url_search_params_constructor(constructor, arguments)
            }
            Some(ObjectHost::ArrowFunction(_)) => {
                Err(JsError::type_error("arrow function is not a constructor"))
            }
            Some(ObjectHost::UserFunction(index)) => {
                self.ensure_heap_capacity(1)?;
                let prototype = match self.realm.get_property(constructor, "prototype") {
                    Some(JsValue::Object(prototype)) => Some(prototype),
                    _ => None,
                };
                let instance = self.realm.create_object(prototype);
                let result =
                    self.call_user(dom, index, arguments, JsValue::Object(instance), true)?;
                if matches!(result, JsValue::Object(_)) {
                    Ok(result)
                } else {
                    Ok(JsValue::Object(instance))
                }
            }
            _ => Err(JsError::type_error("value is not a constructor")),
        }
    }

    fn call(
        &mut self,
        dom: &mut Dom,
        callee: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        self.call_with_this(dom, callee, arguments, JsValue::Undefined)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "call dispatch covers all callable host variants"
    )]
    fn call_with_this(
        &mut self,
        dom: &mut Dom,
        callee: ObjectId,
        arguments: &[JsValue],
        receiver: JsValue,
    ) -> Result<JsValue, JsError> {
        self.consume_step()?;
        if self.calls_active >= self.limits.max_call_depth {
            return Err(JsError::resource("maximum call depth exceeded"));
        }
        self.calls_active = self.calls_active.saturating_add(1);
        let result = match self.realm.host(callee) {
            Some(ObjectHost::ObjectConstructor) => self.object_constructor(arguments),
            Some(ObjectHost::ArrayConstructor) => self.array_constructor(arguments),
            Some(ObjectHost::FunctionConstructor) => Ok(JsValue::Undefined),
            Some(ObjectHost::StringConstructor) => Ok(JsValue::String(
                arguments
                    .first()
                    .map_or_else(String::new, JsValue::to_js_string),
            )),
            Some(ObjectHost::NumberConstructor) => Ok(JsValue::Number(match arguments.first() {
                None | Some(JsValue::Undefined) => 0.0,
                Some(value) => to_number(value)?,
            })),
            Some(ObjectHost::BooleanConstructor) => Ok(JsValue::Boolean(
                arguments.first().is_none_or(JsValue::is_truthy),
            )),
            // `Date()` called as a function yields the current time string.
            Some(ObjectHost::DateConstructor) => {
                Ok(JsValue::String(Self::format_date_utc(Self::now_ms())))
            }
            // `Symbol(desc)` creates a unique opaque object token.
            Some(ObjectHost::SymbolConstructor) => {
                let description = arguments
                    .first()
                    .filter(|value| !matches!(value, JsValue::Undefined))
                    .map(JsValue::to_js_string);
                self.next_symbol_id += 1;
                let symbol_id = self.next_symbol_id;
                self.ensure_heap_capacity(1)?;
                let prototype = self
                    .realm
                    .global("Symbol")
                    .and_then(|value| match value {
                        JsValue::Object(constructor) => {
                            self.realm.get_property(constructor, "prototype")
                        }
                        _ => None,
                    })
                    .and_then(|value| match value {
                        JsValue::Object(prototype) => Some(prototype),
                        _ => None,
                    });
                let instance = self.realm.create_object(prototype);
                if let Some(description) = description {
                    self.realm.set_property(
                        instance,
                        "description".to_owned(),
                        JsValue::String(description),
                    );
                }
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "symbol IDs stay far below any precision boundary"
                )]
                let id_value = symbol_id as f64;
                self.realm
                    .set_property(instance, "@@id".to_owned(), JsValue::Number(id_value));
                Ok(JsValue::Object(instance))
            }
            Some(ObjectHost::EventConstructor) => {
                Err(JsError::type_error("Event constructor requires 'new'"))
            }
            Some(ObjectHost::DomConstructor) => Err(JsError::type_error("Illegal constructor")),
            Some(ObjectHost::ImageConstructor) => self.image_constructor(dom, arguments),
            Some(ObjectHost::IntersectionObserverConstructor) => Err(JsError::type_error(
                "IntersectionObserver constructor requires 'new'",
            )),
            Some(ObjectHost::CollectionConstructor(_)) => {
                Err(JsError::type_error("collection constructors require 'new'"))
            }
            Some(ObjectHost::RegExpConstructor) => {
                // `RegExp(re)` called without `new` behaves like construction.
                let pattern = required_argument(arguments, 0, "RegExp")?.to_js_string();
                let flags = match arguments.get(1) {
                    None | Some(JsValue::Undefined) => String::new(),
                    Some(value) => value.to_js_string(),
                };
                let object = self.construct_regex(&pattern, &flags)?;
                Ok(JsValue::Object(object))
            }
            Some(ObjectHost::UrlConstructor) => self.url_constructor(callee, arguments),
            Some(ObjectHost::UrlSearchParamsConstructor) => {
                self.url_search_params_constructor(callee, arguments)
            }
            Some(ObjectHost::ErrorConstructor(kind)) => {
                self.error_constructor(callee, kind, arguments)
            }
            Some(ObjectHost::NativeFunction(NativeFunction::ObjectPrototypeToString)) => {
                // `toString.call(primitive)` never materializes an object.
                Ok(JsValue::String(self.object_to_string_tag(&receiver)))
            }
            Some(ObjectHost::NativeFunction(NativeFunction::ObjectPrototypeValueOf)) => {
                Ok(receiver.clone())
            }
            Some(ObjectHost::NativeFunction(NativeFunction::FunctionPrototype)) => {
                Ok(JsValue::Undefined)
            }
            Some(ObjectHost::NativeFunction(NativeFunction::FunctionCall)) => {
                self.function_call(dom, &receiver, arguments)
            }
            Some(ObjectHost::NativeFunction(NativeFunction::FunctionApply)) => {
                // apply(thisArg, [args...])
                let callable = Self::require_callable_object(&receiver, &self.realm)?;
                let this_argument = arguments.first().cloned().unwrap_or(JsValue::Undefined);
                let call_arguments = match arguments.get(1) {
                    Some(JsValue::Object(array)) => self.array_elements_for(*array),
                    _ => Vec::new(),
                };
                self.call_with_this(dom, callable, &call_arguments, this_argument)
            }
            Some(ObjectHost::NativeFunction(NativeFunction::FunctionBind)) => {
                self.function_bind(&receiver, arguments)
            }
            Some(ObjectHost::NativeFunction(NativeFunction::UrlToString)) => {
                Ok(self.url_to_string(&receiver))
            }
            Some(ObjectHost::NativeFunction(
                function @ (NativeFunction::UrlSearchParamsGet
                | NativeFunction::UrlSearchParamsHas
                | NativeFunction::UrlSearchParamsSet
                | NativeFunction::UrlSearchParamsAppend
                | NativeFunction::UrlSearchParamsToString
                | NativeFunction::UrlSearchParamsForEach),
            )) => self.url_search_params_method(receiver.clone(), function, arguments, dom),
            Some(ObjectHost::NativeFunction(function)) => {
                let receiver = match &receiver {
                    JsValue::Object(object) => *object,
                    JsValue::Null | JsValue::Undefined => self.realm.global_object(),
                    _ => self.coerce_member_base(&receiver, &format!("{function:?} receiver"))?,
                };
                self.call_native(dom, function, receiver, arguments)
            }
            Some(ObjectHost::BoundFunction { function, receiver }) => {
                self.call_native(dom, function, receiver, arguments)
            }
            Some(ObjectHost::BoundCallable {
                target,
                receiver,
                arguments: bound_arguments,
            }) => {
                let mut combined = bound_arguments;
                combined.extend_from_slice(arguments);
                self.call_with_this(dom, target, &combined, receiver)
            }
            Some(ObjectHost::UserFunction(index)) => {
                self.call_user(dom, index, arguments, receiver, true)
            }
            Some(ObjectHost::ArrowFunction(index)) => {
                self.call_user(dom, index, arguments, receiver, false)
            }
            Some(ObjectHost::PromiseSettler { promise, fulfilled }) => {
                let value = arguments.first().cloned().unwrap_or(JsValue::Undefined);
                if fulfilled {
                    self.resolve_promise_value(promise, &value)?;
                } else {
                    self.reject_promise(promise, &value);
                }
                Ok(JsValue::Undefined)
            }
            _ => Err(JsError::type_error("value is not callable")),
        };
        self.calls_active = self.calls_active.saturating_sub(1);
        result
    }

    fn url_constructor(
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
                    .ok_or_else(|| url::ParseError::EmptyHost)
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
        for (name, value) in super::value::location_components(&parsed) {
            self.realm
                .set_property(instance, name.to_owned(), JsValue::String(value));
        }
        self.realm
            .set_property(instance, "searchParams".to_owned(), JsValue::Object(params));
        Ok(JsValue::Object(instance))
    }

    fn url_search_params_constructor(
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

    fn url_to_string(&self, receiver: &JsValue) -> JsValue {
        match receiver {
            JsValue::Object(object) => match self.realm.host(*object) {
                Some(ObjectHost::UrlInstance(url)) => JsValue::String(url.to_string()),
                _ => JsValue::String(receiver.to_js_string()),
            },
            _ => JsValue::String(receiver.to_js_string()),
        }
    }

    fn url_search_params_method(
        &mut self,
        receiver: JsValue,
        function: NativeFunction,
        arguments: &[JsValue],
        dom: &mut Dom,
    ) -> Result<JsValue, JsError> {
        let JsValue::Object(object) = receiver else {
            return Ok(JsValue::Undefined);
        };
        let Some(ObjectHost::UrlSearchParams { mut pairs, owner }) = self.realm.host(object) else {
            return Ok(JsValue::Undefined);
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
                if let Some(JsValue::Object(callback)) = arguments.get(0) {
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
        Ok(result)
    }

    fn function_call(
        &mut self,
        dom: &mut Dom,
        receiver: &JsValue,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let callable = Self::require_callable_object(receiver, &self.realm)?;
        let this_argument = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        self.call_with_this(
            dom,
            callable,
            arguments.get(1..).unwrap_or_default(),
            this_argument,
        )
    }

    fn function_bind(
        &mut self,
        receiver: &JsValue,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let target = Self::require_callable_object(receiver, &self.realm)?;
        self.ensure_heap_capacity(1)?;
        let bound_receiver = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let bound_arguments = arguments.get(1..).unwrap_or_default().to_vec();
        Ok(JsValue::Object(self.realm.bound_callable(
            target,
            bound_receiver,
            bound_arguments,
        )))
    }

    fn array_constructor(&mut self, arguments: &[JsValue]) -> Result<JsValue, JsError> {
        self.ensure_heap_capacity(1)?;
        let array = self.realm.create_array();
        match arguments {
            [] => {}
            [JsValue::Number(length)] => {
                if !length.is_finite()
                    || *length < 0.0
                    || length.fract() != 0.0
                    || *length > f64::from(u32::MAX)
                {
                    return Err(JsError::type_error("invalid array length"));
                }
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                self.set_array_length(array, *length as u32)?;
            }
            [value] => {
                if !self
                    .realm
                    .set_property(array, "0".to_owned(), value.clone())
                {
                    return Err(JsError::type_error("could not define array element"));
                }
                self.set_array_length(array, 1)?;
            }
            values => {
                for (index, value) in values.iter().enumerate() {
                    self.realm
                        .set_property(array, index.to_string(), value.clone());
                }
                let length = u32::try_from(values.len()).map_err(|_| {
                    JsError::resource("array constructor exceeds the supported u32 range")
                })?;
                self.set_array_length(array, length)?;
            }
        }
        Ok(JsValue::Object(array))
    }

    fn collection_constructor(
        &mut self,
        dom: &mut Dom,
        constructor: ObjectId,
        kind: CollectionKind,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        self.ensure_heap_capacity(1)?;
        let prototype = self
            .realm
            .get_property(constructor, "prototype")
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            });
        let collection = self.realm.collection(kind, prototype);
        let Some(iterable) = arguments.first() else {
            return Ok(JsValue::Object(collection));
        };
        if matches!(iterable, JsValue::Null | JsValue::Undefined) {
            return Ok(JsValue::Object(collection));
        }
        let iterable = Self::require_object(iterable)?;
        if !matches!(self.realm.host(iterable), Some(ObjectHost::Array)) {
            return Err(JsError::type_error(
                "collection constructor currently requires an Array iterable",
            ));
        }
        for item in self.array_elements_for(iterable) {
            if kind.is_map() {
                let pair = Self::require_object(&item)?;
                let key = self.get_member(dom, pair, "0")?;
                let value = self.get_member(dom, pair, "1")?;
                self.collection_store(collection, key, value, true)?;
            } else {
                self.collection_store(collection, item.clone(), item, false)?;
            }
        }
        Ok(JsValue::Object(collection))
    }

    fn call_user(
        &mut self,
        dom: &mut Dom,
        index: usize,
        arguments: &[JsValue],
        receiver: JsValue,
        create_arguments_binding: bool,
    ) -> Result<JsValue, JsError> {
        // All user functions are treated as sloppy mode: a nullish `this`
        // falls back to the global object.
        let receiver = match receiver {
            JsValue::Undefined | JsValue::Null => JsValue::Object(self.realm.global_object()),
            other => other,
        };
        let function = self
            .functions
            .get(index)
            .cloned()
            .ok_or_else(|| JsError::type_error("function object refers to unknown code"))?;
        let function_label = format!("(user fn #{index})");
        if self
            .call_stack
            .iter()
            .filter(|label| label.as_str() == function_label)
            .count()
            >= 2
        {
            return Ok(JsValue::Undefined);
        }
        let previous_environment =
            std::mem::replace(&mut self.environment, function.captured_environment);
        let mut call_environment = EnvironmentRecord {
            function_scope: true,
            ..EnvironmentRecord::default()
        };
        if create_arguments_binding {
            let arguments_object = self.create_array_from_values(arguments)?;
            call_environment.bindings.insert(
                "arguments".to_owned(),
                Binding {
                    value: JsValue::Object(arguments_object),
                    mutable: true,
                    initialized: true,
                    kind: VariableKind::Var,
                },
            );
        }
        for (index, parameter) in function.parameters.iter().enumerate() {
            call_environment.bindings.insert(
                parameter.clone(),
                Binding {
                    value: arguments.get(index).cloned().unwrap_or(JsValue::Undefined),
                    mutable: true,
                    initialized: true,
                    kind: VariableKind::Var,
                },
            );
        }
        self.environment
            .push(Rc::new(RefCell::new(call_environment)));
        self.this_stack
            .push(function.lexical_this.clone().unwrap_or(receiver));
        let label = format!("(user fn #{index})");
        self.call_stack.push(label);
        let result = self
            .instantiate_statements(&function.body)
            .and_then(|()| self.evaluate_statements(dom, &function.body));
        self.call_stack.pop();
        self.this_stack.pop();
        self.environment = previous_environment;
        match result? {
            Completion::Normal(_) => Ok(JsValue::Undefined),
            Completion::Return(value) => Ok(value),
            Completion::Break(_) | Completion::Continue(_) => {
                Err(JsError::syntax("loop control escaped a function body", 0))
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn call_native(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        self.call_stack.push(format!("{function:?}"));
        let result = self.call_native_dispatch(dom, function, receiver, arguments);
        self.call_stack.pop();
        result
    }

    #[allow(
        clippy::too_many_lines,
        reason = "native dispatch mirrors the NativeFunction variants one-to-one"
    )]
    fn call_native_dispatch(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match function {
            NativeFunction::GetElementById => {
                self.require_document(receiver)?;
                let id = required_argument(arguments, 0, "getElementById")?.to_js_string();
                match self.find_element_by_id(dom, &id)? {
                    Some(node) => self.wrap_node(node),
                    None => Ok(JsValue::Null),
                }
            }
            NativeFunction::QuerySelector => {
                let root = self.query_root(receiver)?;
                let selector = required_argument(arguments, 0, "querySelector")?.to_js_string();
                let selectors = parse_selector_list(&selector)
                    .map_err(|error| JsError::dom(format!("invalid selector: {error}")))?;
                match select_all(dom, root, &selectors, &MatchContext::default())
                    .into_iter()
                    .next()
                {
                    Some(node) => self.wrap_node(node),
                    None => Ok(JsValue::Null),
                }
            }
            NativeFunction::QuerySelectorAll => {
                let root = self.query_root(receiver)?;
                let selector = required_argument(arguments, 0, "querySelectorAll")?.to_js_string();
                let selectors = parse_selector_list(&selector)
                    .map_err(|error| JsError::dom(format!("invalid selector: {error}")))?;
                let nodes = select_all(dom, root, &selectors, &MatchContext::default())
                    .into_iter()
                    .map(|node| self.wrap_node(node))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(JsValue::Object(self.create_array_from_values(&nodes)?))
            }
            NativeFunction::GetElementsByTagName => {
                let root = self.query_root(receiver)?;
                let tag = required_argument(arguments, 0, "getElementsByTagName")?.to_js_string();
                // Type selectors match case-insensitively for HTML elements,
                // which is exactly the legacy API contract.
                let selectors = parse_selector_list(&tag)
                    .map_err(|error| JsError::dom(format!("invalid selector: {error}")))?;
                let nodes = select_all(dom, root, &selectors, &MatchContext::default())
                    .into_iter()
                    .map(|node| self.wrap_node(node))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(JsValue::Object(self.create_array_from_values(&nodes)?))
            }
            NativeFunction::GetElementsByClassName => {
                let root = self.query_root(receiver)?;
                let names =
                    required_argument(arguments, 0, "getElementsByClassName")?.to_js_string();
                let selector: String = names
                    .split_ascii_whitespace()
                    .map(|name| format!(".{name}"))
                    .fold(String::new(), |mut acc, part| {
                        acc.push_str(&part);
                        acc
                    });
                if selector.is_empty() {
                    return Ok(JsValue::Object(self.create_array_from_values(&[])?));
                }
                let selectors = parse_selector_list(&selector)
                    .map_err(|error| JsError::dom(format!("invalid selector: {error}")))?;
                let nodes = select_all(dom, root, &selectors, &MatchContext::default())
                    .into_iter()
                    .map(|node| self.wrap_node(node))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(JsValue::Object(self.create_array_from_values(&nodes)?))
            }
            NativeFunction::CloneNode => {
                let node = self.require_node(receiver)?;
                let deep = arguments.first().is_some_and(JsValue::is_truthy);
                self.clone_node_value(dom, node, deep)
            }
            NativeFunction::CreateTextNode => {
                self.require_document(receiver)?;
                let data = required_argument(arguments, 0, "createTextNode")?.to_js_string();
                let node = dom.create_text(data);
                self.wrap_node(node)
            }
            NativeFunction::CreateDocumentFragment => {
                self.require_document(receiver)?;
                let node = dom.create_document_fragment();
                self.wrap_node(node)
            }
            NativeFunction::ArrayIndexOf => self.array_index_of(receiver, arguments),
            NativeFunction::ArraySlice => self.array_slice(receiver, arguments),
            NativeFunction::ArraySplice => self.array_splice(receiver, arguments),
            NativeFunction::ArrayReverse => self.array_reverse(receiver),
            NativeFunction::ArraySort => self.array_sort(dom, receiver, arguments),
            NativeFunction::ArrayConcat => self.array_concat(receiver, arguments),
            NativeFunction::ArrayShift => self.array_shift(receiver),
            NativeFunction::ArrayUnshift => self.array_unshift(receiver, arguments),
            NativeFunction::ArrayForEach => {
                let callback = Self::require_callable_object(
                    required_argument(arguments, 0, "forEach")?,
                    &self.realm,
                )?;
                self.array_iterate_with(dom, receiver, callback, false, false)
            }
            NativeFunction::ArrayMap => {
                let callback = Self::require_callable_object(
                    required_argument(arguments, 0, "map")?,
                    &self.realm,
                )?;
                self.array_iterate_with(dom, receiver, callback, true, false)
            }
            NativeFunction::ArrayFilter => {
                let callback = Self::require_callable_object(
                    required_argument(arguments, 0, "filter")?,
                    &self.realm,
                )?;
                self.array_iterate_with(dom, receiver, callback, false, true)
            }
            NativeFunction::ArraySome => {
                let callback = Self::require_callable_object(
                    required_argument(arguments, 0, "some")?,
                    &self.realm,
                )?;
                self.array_some(dom, receiver, callback)
            }
            NativeFunction::ArrayFind => self.array_find(dom, receiver, arguments, false),
            NativeFunction::ArrayFindIndex => self.array_find(dom, receiver, arguments, true),
            NativeFunction::ArrayEvery => self.array_every(dom, receiver, arguments),
            NativeFunction::ArrayIncludes => self.array_includes(receiver, arguments),
            NativeFunction::ArrayReduce => self.array_reduce(dom, receiver, arguments),
            NativeFunction::MathRandom => {
                let mut state = self.random_state;
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                self.random_state = state;
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "53-bit mantissa division yields a uniform f64 in [0, 1)"
                )]
                let value = (state >> 11) as f64 / (1u64 << 53) as f64;
                Ok(JsValue::Number(value))
            }
            NativeFunction::GetComputedStyle => {
                let argument = required_argument(arguments, 0, "getComputedStyle")?;
                let element = self.value_as_node(argument)?;
                self.ensure_heap_capacity(1)?;
                Ok(JsValue::Object(
                    self.realm.style_declaration_wrapper(element),
                ))
            }
            NativeFunction::CompareDocumentPosition => {
                let node = self.require_node(receiver)?;
                let other = self.value_as_node(required_argument(
                    arguments,
                    0,
                    "compareDocumentPosition",
                )?)?;
                if node == other {
                    return Ok(JsValue::Number(0.0));
                }
                if dom_contains(dom, node, other) {
                    // `other` is inside `node` and comes after it.
                    return Ok(JsValue::Number(
                        DOCUMENT_POSITION_CONTAINED_BY + DOCUMENT_POSITION_FOLLOWING,
                    ));
                }
                if dom_contains(dom, other, node) {
                    return Ok(JsValue::Number(
                        DOCUMENT_POSITION_CONTAINS + DOCUMENT_POSITION_PRECEDING,
                    ));
                }
                Ok(JsValue::Number(DOCUMENT_POSITION_DISCONNECTED))
            }
            NativeFunction::CreateElement => {
                self.require_document(receiver)?;
                let name = required_argument(arguments, 0, "createElement")?.to_js_string();
                if !valid_html_local_name(&name) {
                    return Err(JsError::dom(format!(
                        "{name:?} is not a supported HTML local name"
                    )));
                }
                if self.dom_nodes_created >= self.limits.max_dom_nodes_created {
                    return Err(JsError::resource("DOM node creation limit exceeded"));
                }
                self.ensure_heap_capacity(1)?;
                self.dom_nodes_created = self.dom_nodes_created.saturating_add(1);
                let node = dom.create_element(name);
                Ok(JsValue::Object(self.realm.node_wrapper(node)))
            }
            NativeFunction::SetAttribute => {
                let node = self.require_node(receiver)?;
                let name = required_argument(arguments, 0, "setAttribute")?.to_js_string();
                let value = required_argument(arguments, 1, "setAttribute")?.to_js_string();
                dom.set_attribute(node, name, value)?;
                Ok(JsValue::Undefined)
            }
            NativeFunction::GetAttribute => {
                let node = self.require_node(receiver)?;
                let name = required_argument(arguments, 0, "getAttribute")?.to_js_string();
                Ok(dom
                    .attribute(node, &name)?
                    .map_or(JsValue::Null, |value| JsValue::String(value.to_owned())))
            }
            NativeFunction::HasAttribute => {
                let node = self.require_node(receiver)?;
                let name = required_argument(arguments, 0, "hasAttribute")?.to_js_string();
                Ok(JsValue::Boolean(dom.attribute(node, &name)?.is_some()))
            }
            NativeFunction::RemoveAttribute => {
                let node = self.require_node(receiver)?;
                let name = required_argument(arguments, 0, "removeAttribute")?.to_js_string();
                dom.remove_attribute(node, &name)?;
                Ok(JsValue::Undefined)
            }
            NativeFunction::AppendChild => {
                let parent = self.require_node(receiver)?;
                let child = self.value_as_node(required_argument(arguments, 0, "appendChild")?)?;
                dom.append_child(parent, child)?;
                self.wrap_node(child)
            }
            NativeFunction::RemoveChild => {
                let parent = self.require_node(receiver)?;
                let child = self.value_as_node(required_argument(arguments, 0, "removeChild")?)?;
                dom.remove_child(parent, child)?;
                self.wrap_node(child)
            }
            NativeFunction::InsertBefore => {
                let parent = self.require_node(receiver)?;
                let child = self.value_as_node(required_argument(arguments, 0, "insertBefore")?)?;
                let reference = match arguments.get(1) {
                    None | Some(JsValue::Null | JsValue::Undefined) => None,
                    Some(value) => Some(self.value_as_node(value)?),
                };
                dom.insert_before(parent, child, reference)?;
                self.wrap_node(child)
            }
            NativeFunction::RemoveNode => {
                let node = self.require_node(receiver)?;
                if let Some(parent) = dom.parent(node) {
                    dom.remove_child(parent, node)?;
                }
                Ok(JsValue::Undefined)
            }
            NativeFunction::Contains => {
                let root = self.require_node(receiver)?;
                let candidate = self.value_as_node(required_argument(arguments, 0, "contains")?)?;
                Ok(JsValue::Boolean(dom_contains(dom, root, candidate)))
            }
            NativeFunction::Matches => {
                let node = self.require_node(receiver)?;
                let selector = required_argument(arguments, 0, "matches")?.to_js_string();
                let selectors = parse_selector_list(&selector)
                    .map_err(|error| JsError::dom(format!("invalid selector: {error}")))?;
                Ok(JsValue::Boolean(matches_selector_list(
                    dom,
                    node,
                    &selectors,
                    &MatchContext::default(),
                )))
            }
            NativeFunction::Click => {
                self.require_node(receiver)?;
                let options = self.realm.create_ordinary_object();
                self.realm
                    .set_property(options, "bubbles".to_owned(), JsValue::Boolean(true));
                self.realm
                    .set_property(options, "cancelable".to_owned(), JsValue::Boolean(true));
                let event = self.event_constructor(&[
                    JsValue::String("click".to_owned()),
                    JsValue::Object(options),
                ])?;
                let _ = self.dispatch_event(dom, receiver, &[event])?;
                Ok(JsValue::Undefined)
            }
            NativeFunction::AddEventListener => self.add_event_listener(receiver, arguments),
            NativeFunction::RemoveEventListener => self.remove_event_listener(receiver, arguments),
            NativeFunction::DispatchEvent => self.dispatch_event(dom, receiver, arguments),
            NativeFunction::EventPreventDefault => Ok(self.event_prevent_default(receiver)),
            NativeFunction::ClassListAdd => self.class_list_add(dom, receiver, arguments),
            NativeFunction::ClassListRemove => self.class_list_remove(dom, receiver, arguments),
            NativeFunction::ClassListToggle => self.class_list_toggle(dom, receiver, arguments),
            NativeFunction::ClassListContains => self.class_list_contains(dom, receiver, arguments),
            NativeFunction::ClassListItem => self.class_list_item(dom, receiver, arguments),
            NativeFunction::ClassListToString => self.class_list_to_string(dom, receiver),
            NativeFunction::LocationToString => match self.realm.host(receiver) {
                Some(ObjectHost::Location(url)) => Ok(JsValue::String(url.to_string())),
                _ => Err(JsError::type_error("incompatible Location method receiver")),
            },
            NativeFunction::LocationAssign | NativeFunction::LocationReplace => {
                self.request_location_navigation(receiver, arguments, function)
            }
            NativeFunction::ConsoleDebug
            | NativeFunction::ConsoleError
            | NativeFunction::ConsoleInfo
            | NativeFunction::ConsoleLog
            | NativeFunction::ConsoleWarn => self.console_write(function, arguments),
            NativeFunction::SetTimeout | NativeFunction::SetInterval => {
                self.register_timer(function, arguments)
            }
            NativeFunction::RequestAnimationFrame => {
                let callback = Self::require_callable_object(
                    required_argument(arguments, 0, "requestAnimationFrame")?,
                    &self.realm,
                )?;
                Ok(JsValue::Number(self.register_timer_entry(
                    callback,
                    0.0,
                    TimerKind::AnimationFrame,
                )))
            }
            NativeFunction::ClearTimeout | NativeFunction::ClearInterval => {
                Ok(self.cancel_timer(arguments))
            }
            NativeFunction::CancelAnimationFrame => Ok(self.cancel_timer(arguments)),
            NativeFunction::GetBoundingClientRect => {
                let node = self.require_node(receiver)?;
                Ok(self.element_rect_value(node))
            }
            NativeFunction::IntersectionObserve => self.intersection_observe(receiver, arguments),
            NativeFunction::IntersectionUnobserve => {
                self.intersection_unobserve(receiver, arguments)
            }
            NativeFunction::IntersectionDisconnect => self.intersection_disconnect(receiver),
            NativeFunction::IntersectionTakeRecords => self.intersection_take_records(receiver),
            NativeFunction::StyleGetProperty => self.style_get_property(dom, receiver, arguments),
            NativeFunction::StyleSetProperty => self.style_set_property(dom, receiver, arguments),
            NativeFunction::StyleRemoveProperty => {
                self.style_remove_property(dom, receiver, arguments)
            }
            NativeFunction::StyleItem => self.style_item(dom, receiver, arguments),
            NativeFunction::RegExpExec => self.regexp_exec(receiver, arguments),
            NativeFunction::RegExpTest => self.regexp_test(receiver, arguments),
            NativeFunction::RegExpToString => self.regexp_to_string(receiver),
            NativeFunction::StrCharAt => self.string_char_at(receiver, arguments),
            NativeFunction::StrCharCodeAt => self.string_char_code_at(receiver, arguments),
            NativeFunction::StringFromCharCode => Self::string_from_char_code(arguments),
            NativeFunction::StringFromCodePoint => Self::string_from_code_point(arguments),
            NativeFunction::StringRaw => Ok(JsValue::String(
                arguments
                    .first()
                    .map_or_else(String::new, JsValue::to_js_string),
            )),
            NativeFunction::StrIndexOf => self.string_index_of(receiver, arguments, false),
            NativeFunction::StrLastIndexOf => self.string_index_of(receiver, arguments, true),
            NativeFunction::StrIncludes => self.string_includes(receiver, arguments),
            NativeFunction::StrStartsWith => {
                self.string_starts_or_ends_with(receiver, arguments, true)
            }
            NativeFunction::StrEndsWith => {
                self.string_starts_or_ends_with(receiver, arguments, false)
            }
            NativeFunction::StrSlice => self.string_slice(receiver, arguments),
            NativeFunction::StrSubstring => self.string_substring(receiver, arguments),
            NativeFunction::StrToLowerCase => self.string_to_case(receiver, arguments, false),
            NativeFunction::StrToUpperCase => self.string_to_case(receiver, arguments, true),
            NativeFunction::StrTrim => self.string_trim(receiver),
            NativeFunction::StrSplit => self.string_split(receiver, arguments),
            NativeFunction::StrReplace => self.string_replace(dom, receiver, arguments),
            NativeFunction::StrMatch => self.string_match(receiver, arguments),
            NativeFunction::StrSearch => self.string_search(receiver, arguments),
            NativeFunction::StrConcat => self.string_concat(receiver, arguments),
            NativeFunction::StrToString => {
                Ok(JsValue::String(self.require_string_receiver(receiver)?))
            }
            NativeFunction::StrForEach => {
                let text = self.require_string_receiver(receiver)?;
                let callback = Self::require_callable_object(
                    required_argument(arguments, 0, "String.forEach")?,
                    &self.realm,
                )?;
                for (index, character) in text.chars().enumerate() {
                    #[allow(clippy::cast_precision_loss)]
                    let index = JsValue::Number(index as f64);
                    self.call(
                        dom,
                        callback,
                        &[
                            JsValue::String(character.to_string()),
                            index,
                            JsValue::Object(receiver),
                        ],
                    )?;
                }
                Ok(JsValue::Undefined)
            }
            NativeFunction::StrPush => Ok(JsValue::Number(
                self.require_string_receiver(receiver)?.chars().count() as f64,
            )),
            NativeFunction::QueueMicrotask => {
                let callback = Self::require_callable_object(
                    required_argument(arguments, 0, "queueMicrotask")?,
                    &self.realm,
                )?;
                self.pending_microtasks
                    .push(JsMicrotask::Callback(callback));
                Ok(JsValue::Undefined)
            }
            NativeFunction::PromiseResolve => {
                let value = arguments.first().cloned().unwrap_or(JsValue::Undefined);
                if let JsValue::Object(object) = value
                    && matches!(self.realm.host(object), Some(ObjectHost::Promise(_)))
                {
                    return Ok(JsValue::Object(object));
                }
                let (promise, result) = self.create_promise()?;
                self.resolve_promise(promise, &value);
                Ok(result)
            }
            NativeFunction::PromiseReject => {
                let reason = arguments.first().cloned().unwrap_or(JsValue::Undefined);
                let (promise, result) = self.create_promise()?;
                self.reject_promise(promise, &reason);
                Ok(result)
            }
            NativeFunction::PromiseThen => self.perform_promise_then(receiver, arguments),
            NativeFunction::PromiseCatch => {
                let handler = arguments.first().cloned().unwrap_or(JsValue::Undefined);
                self.perform_promise_then(receiver, &[JsValue::Undefined, handler])
            }
            NativeFunction::ArrayIsArray => Ok(JsValue::Boolean(matches!(
                arguments.first(),
                Some(JsValue::Object(object))
                    if matches!(self.realm.host(*object), Some(ObjectHost::Array))
            ))),
            NativeFunction::ArrayFrom => self.array_from(dom, arguments),
            NativeFunction::ArrayPush => self.array_push(receiver, arguments),
            NativeFunction::ArrayPop => self.array_pop(receiver),
            NativeFunction::ArrayJoin => self.array_join(receiver, arguments),
            NativeFunction::MathAbs => Self::math_unary(arguments, f64::abs),
            NativeFunction::MathCeil => Self::math_unary(arguments, f64::ceil),
            NativeFunction::MathFloor => Self::math_unary(arguments, f64::floor),
            NativeFunction::MathMax => Self::math_min_max(arguments, f64::NEG_INFINITY, f64::max),
            NativeFunction::MathMin => Self::math_min_max(arguments, f64::INFINITY, f64::min),
            NativeFunction::MathPow => Self::math_pow(arguments),
            NativeFunction::MathRound => Self::math_unary(arguments, js_math_round),
            NativeFunction::MathSqrt => Self::math_unary(arguments, f64::sqrt),
            NativeFunction::ObjectAssign => self.object_assign(arguments),
            NativeFunction::ObjectKeys => self.object_entries(arguments, ObjectEntryKind::Keys),
            NativeFunction::ObjectValues => self.object_entries(arguments, ObjectEntryKind::Values),
            NativeFunction::ObjectEntries => {
                self.object_entries(arguments, ObjectEntryKind::Entries)
            }
            NativeFunction::ObjectCreate => self.object_create(arguments),
            NativeFunction::ObjectDefineProperty => self.object_define_property(arguments),
            NativeFunction::ObjectDefineProperties => self.object_define_properties(arguments),
            NativeFunction::ObjectGetOwnPropertyDescriptor => {
                self.object_get_own_property_descriptor(arguments)
            }
            NativeFunction::ObjectGetOwnPropertyDescriptors => {
                self.object_get_own_property_descriptors(arguments)
            }
            NativeFunction::ObjectGetOwnPropertyNames => {
                self.object_get_own_property_names(arguments)
            }
            NativeFunction::ObjectGetPrototypeOf => self.object_get_prototype_of(arguments),
            NativeFunction::ObjectHasOwn => self.object_has_own(arguments),
            NativeFunction::ObjectPrototypeHasOwnProperty => {
                self.object_prototype_has_own_property(receiver, arguments)
            }
            NativeFunction::ObjectPrototypeIsPrototypeOf => {
                Ok(self.object_prototype_is_prototype_of(receiver, arguments))
            }
            NativeFunction::NamedMapItem | NativeFunction::NamedMapGetNamedItem => {
                let Some(ObjectHost::NamedNodeMap(map_node)) = self.realm.host(receiver) else {
                    return Err(JsError::type_error("incompatible attributes receiver"));
                };
                let key = required_argument(arguments, 0, "item")?.to_js_string();
                let attribute = if function == NativeFunction::NamedMapItem {
                    let index_result = key.parse::<usize>();
                    dom.node(map_node)
                        .and_then(|node| match node.kind() {
                            NodeKind::Element(element) => {
                                element.attributes.get(index_result.unwrap_or(usize::MAX))
                            }
                            _ => None,
                        })
                        .map(|attribute| attribute.local_name.clone())
                } else {
                    dom.attribute(map_node, &key)?.map(|_value| key.clone())
                };
                match attribute {
                    Some(name) => {
                        self.ensure_heap_capacity(1)?;
                        Ok(JsValue::Object(self.realm.attr_wrapper(map_node, name)))
                    }
                    None => Ok(JsValue::Null),
                }
            }
            NativeFunction::AttrGetName | NativeFunction::AttrGetValue => {
                let (owner, name) = match self.realm.host(receiver) {
                    Some(ObjectHost::Attr { owner, name }) => (owner, name.clone()),
                    _ => return Err(JsError::type_error("incompatible Attr receiver")),
                };
                if function == NativeFunction::AttrGetName {
                    Ok(JsValue::String(name))
                } else {
                    Ok(JsValue::String(
                        dom.attribute(owner, &name)?.unwrap_or_default().to_owned(),
                    ))
                }
            }
            NativeFunction::GlobalEvalStub => {
                // Indirect eval is not supported; return the argument
                // unchanged so JSONP-style `eval(data)` patterns don't crash.
                Ok(arguments.first().cloned().unwrap_or(JsValue::Undefined))
            }
            NativeFunction::GlobalImport => {
                // Module graph fetching belongs to the browser coordinator.
                // Keep dynamic import thenable so feature detection and
                // optional chunks do not turn into a fatal ReferenceError.
                let (promise, result) = self.create_promise()?;
                let namespace = self.realm.create_ordinary_object();
                self.resolve_promise(promise, &JsValue::Object(namespace));
                Ok(result)
            }
            NativeFunction::GlobalNoop => Ok(JsValue::Undefined),
            NativeFunction::CssSupports => Ok(JsValue::Boolean(true)),
            NativeFunction::UrlToString => Ok(self.url_to_string(&JsValue::Object(receiver))),
            NativeFunction::UrlSearchParamsGet
            | NativeFunction::UrlSearchParamsHas
            | NativeFunction::UrlSearchParamsSet
            | NativeFunction::UrlSearchParamsAppend
            | NativeFunction::UrlSearchParamsToString
            | NativeFunction::UrlSearchParamsForEach => {
                self.url_search_params_method(JsValue::Object(receiver), function, arguments, dom)
            }
            NativeFunction::GlobalEscape => {
                let text = required_argument(arguments, 0, "escape")?.to_js_string();
                let mut output = String::with_capacity(text.len());
                for character in text.chars() {
                    let code = character as u32;
                    if code < 0x80
                        && (character.is_ascii_alphanumeric()
                            || matches!(
                                character,
                                '@' | '*' | '_' | '+' | '-' | '.' | '/' | '(' | ')'
                            ))
                    {
                        output.push(character);
                    } else if code < 0x100 {
                        let _ = write!(output, "%{code:02X}");
                    } else {
                        let _ = write!(output, "%u{code:04X}");
                    }
                }
                Ok(JsValue::String(output))
            }
            NativeFunction::GlobalUnescape => {
                let text = required_argument(arguments, 0, "unescape")?.to_js_string();
                match Self::percent_decode(&text) {
                    Some(decoded) => Ok(JsValue::String(decoded)),
                    None => Ok(JsValue::String(text)),
                }
            }
            NativeFunction::GlobalParseInt => {
                let text = required_argument(arguments, 0, "parseInt")?.to_js_string();
                let trimmed = text.trim_start();
                let (radix, digits) = if let Some(rest) = trimmed.strip_prefix("0x") {
                    (16u32, rest)
                } else if let Some(rest) = trimmed.strip_prefix("0X") {
                    (16, rest)
                } else {
                    (10, trimmed)
                };
                let end = digits
                    .chars()
                    .position(|c| c.to_digit(radix).is_none())
                    .unwrap_or(digits.len());
                match i64::from_str_radix(&digits[..end], radix) {
                    #[allow(
                        clippy::cast_precision_loss,
                        reason = "parseInt results stay within binary64 precision"
                    )]
                    Ok(value) => Ok(JsValue::Number(value as f64)),
                    Err(_) => Ok(JsValue::Number(f64::NAN)),
                }
            }
            NativeFunction::GlobalParseFloat => {
                let text = required_argument(arguments, 0, "parseFloat")?.to_js_string();
                let trimmed = text.trim_start();
                let end = trimmed
                    .find(|c: char| {
                        !(c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-'))
                    })
                    .unwrap_or(trimmed.len());
                trimmed[..end].parse::<f64>().map_or_else(
                    |_| Ok(JsValue::Number(f64::NAN)),
                    |value| Ok(JsValue::Number(value)),
                )
            }
            NativeFunction::GlobalIsNaN => Ok(JsValue::Boolean(
                to_number(required_argument(arguments, 0, "isNaN")?)?.is_nan(),
            )),
            NativeFunction::GlobalIsFinite => Ok(JsValue::Boolean(
                to_number(required_argument(arguments, 0, "isFinite")?)?.is_finite(),
            )),
            NativeFunction::GlobalEncodeURI | NativeFunction::GlobalEncodeURIComponent => {
                let text = required_argument(arguments, 0, "encodeURI")?.to_js_string();
                let component = function == NativeFunction::GlobalEncodeURIComponent;
                Ok(JsValue::String(Self::percent_encode(&text, !component)))
            }
            NativeFunction::GlobalDecodeURI | NativeFunction::GlobalDecodeURIComponent => {
                let text = required_argument(arguments, 0, "decodeURI")?.to_js_string();
                match Self::percent_decode(&text) {
                    Some(decoded) => Ok(JsValue::String(decoded)),
                    None => Err(JsError::dom("malformed URI sequence")),
                }
            }
            NativeFunction::WindowAddEventListener => self.add_window_listener(receiver, arguments),
            NativeFunction::WindowRemoveEventListener => {
                self.remove_window_listener(receiver, arguments)
            }
            NativeFunction::SymbolToString => {
                let description = self
                    .realm
                    .get_property(receiver, "description")
                    .map(|value| value.to_js_string())
                    .unwrap_or_default();
                Ok(JsValue::String(format!("Symbol({description})")))
            }
            NativeFunction::SymbolValueOf | NativeFunction::NumValueOf => {
                Ok(JsValue::Object(receiver))
            }
            NativeFunction::DateSetTime => {
                if let Some(ObjectHost::DateInstance(_ms)) = self.realm.host(receiver) {
                    let new_ms = to_number(required_argument(arguments, 0, "setTime")?)?;
                    self.realm.set_host_data_date(receiver, new_ms);
                    Ok(JsValue::Number(new_ms))
                } else {
                    Err(JsError::type_error("incompatible Date method receiver"))
                }
            }
            NativeFunction::DateGetFullYear
            | NativeFunction::DateGetMonth
            | NativeFunction::DateGetDate
            | NativeFunction::DateGetDay
            | NativeFunction::DateGetHours
            | NativeFunction::DateGetMinutes
            | NativeFunction::DateGetSeconds
            | NativeFunction::DateGetMilliseconds
            | NativeFunction::DateGetTimezoneOffset
            | NativeFunction::DateGetUTCFullYear
            | NativeFunction::DateGetUTCMonth
            | NativeFunction::DateGetUTCDate
            | NativeFunction::DateGetUTCDay
            | NativeFunction::DateGetUTCHours
            | NativeFunction::DateGetUTCMinutes
            | NativeFunction::DateGetUTCSeconds
            | NativeFunction::DateGetUTCMilliseconds => {
                let ms = self.require_date_value(receiver)?;
                Ok(Self::date_getter(function, ms))
            }
            NativeFunction::DateToISOString => {
                let ms = self.require_date_value(receiver)?;
                if !ms.is_finite() {
                    return Err(JsError::type_error("Invalid time value"));
                }
                Ok(JsValue::String(Self::format_date_iso(ms)))
            }
            NativeFunction::DateToJSON => {
                let ms = self.require_date_value(receiver)?;
                if !ms.is_finite() {
                    Ok(JsValue::Null)
                } else {
                    Ok(JsValue::String(Self::format_date_iso(ms)))
                }
            }
            NativeFunction::DateToDateString => {
                let ms = self.require_date_value(receiver)?;
                if !ms.is_finite() {
                    return Ok(JsValue::String("Invalid Date".to_owned()));
                }
                let (year, month, day, _, _, _, _, weekday) = Self::date_components(ms);
                Ok(JsValue::String(format!(
                    "{} {} {day:02} {year}",
                    DATE_WEEKDAYS[weekday as usize], DATE_MONTHS[month as usize]
                )))
            }
            NativeFunction::DateParse => {
                let input = required_argument(arguments, 0, "Date.parse")?.to_js_string();
                Ok(JsValue::Number(
                    Self::parse_date_string(&input).unwrap_or(f64::NAN),
                ))
            }
            NativeFunction::DateUTC => {
                Ok(JsValue::Number(Self::date_from_utc_arguments(arguments)?))
            }
            NativeFunction::StringSubstr => {
                let text = self.require_string_receiver(receiver)?;
                let characters: Vec<char> = text.chars().collect();
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "substr indices are validated small integers"
                )]
                let start = match optional_index(arguments.first()) {
                    Ok(value) => {
                        let raw = value as i64;
                        if raw < 0 {
                            characters.len().saturating_sub(raw.unsigned_abs() as usize)
                        } else {
                            (raw as usize).min(characters.len())
                        }
                    }
                    Err(_) => 0,
                };
                let length = match arguments.get(1) {
                    None | Some(JsValue::Undefined) => characters.len() - start,
                    Some(value) => {
                        #[allow(
                            clippy::cast_possible_truncation,
                            clippy::cast_sign_loss,
                            reason = "substr lengths are validated small integers"
                        )]
                        {
                            to_number(value)?.max(0.0) as usize
                        }
                    }
                };
                let end = (start + length).min(characters.len());
                Ok(JsValue::String(
                    characters[start.min(end)..end].iter().collect(),
                ))
            }
            NativeFunction::NumToFixed => {
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "toFixed digits are validated small integers"
                )]
                let digits = match arguments.first() {
                    Some(JsValue::Number(n)) => *n as usize,
                    _ => 0,
                };
                match self.realm.host(receiver) {
                    Some(ObjectHost::NumberPrimitive(value)) => {
                        Ok(JsValue::String(format!("{value:.digits$}")))
                    }
                    _ => Err(JsError::type_error("incompatible Number method receiver")),
                }
            }
            NativeFunction::NumToPrecision => {
                let value = match self.realm.host(receiver) {
                    Some(ObjectHost::NumberPrimitive(value)) => value,
                    _ => {
                        return Err(JsError::type_error("incompatible Number method receiver"));
                    }
                };
                let Some(argument) = arguments.first() else {
                    return Ok(JsValue::String(super::value::number_to_string(value)));
                };
                if matches!(argument, JsValue::Undefined) {
                    return Ok(JsValue::String(super::value::number_to_string(value)));
                }
                let precision = to_number(argument)?;
                if !precision.is_finite()
                    || precision.fract() != 0.0
                    || !(1.0..=100.0).contains(&precision)
                {
                    return Err(JsError::type_error("invalid toPrecision precision"));
                }
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "toPrecision precision is validated in the 1..=100 range"
                )]
                let precision = precision as usize;
                Ok(JsValue::String(format_number_precision(value, precision)))
            }
            NativeFunction::NumToString => match self.realm.host(receiver) {
                Some(ObjectHost::NumberPrimitive(value)) => {
                    Ok(JsValue::String(super::value::number_to_string(value)))
                }
                _ => Err(JsError::type_error("incompatible Number method receiver")),
            },
            NativeFunction::BoolToString | NativeFunction::BoolValueOf => {
                match self.realm.host(receiver) {
                    Some(ObjectHost::BooleanPrimitive(value)) => Ok(JsValue::String(
                        if value { "true" } else { "false" }.to_owned(),
                    )),
                    _ => Err(JsError::type_error("incompatible Boolean method receiver")),
                }
            }
            NativeFunction::DateNow => Ok(JsValue::Number(Self::now_ms())),
            NativeFunction::DateGetValue | NativeFunction::DateValueOf => {
                match self.realm.host(receiver) {
                    Some(ObjectHost::DateInstance(ms)) => Ok(JsValue::Number(ms)),
                    _ => Err(JsError::type_error("incompatible Date method receiver")),
                }
            }
            NativeFunction::DateToString | NativeFunction::DateToGMTString => {
                match self.realm.host(receiver) {
                    Some(ObjectHost::DateInstance(ms)) => {
                        Ok(JsValue::String(Self::format_date_utc(ms)))
                    }
                    _ => Err(JsError::type_error("incompatible Date method receiver")),
                }
            }
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
            NativeFunction::PerformanceNow => Ok(JsValue::Number(Self::monotonic_now_ms())),
            NativeFunction::CollectionGet => self.collection_get(receiver, arguments),
            NativeFunction::CollectionSet => {
                let key = required_argument(arguments, 0, "Map.set")?.clone();
                let value = arguments.get(1).cloned().unwrap_or(JsValue::Undefined);
                self.collection_store(receiver, key, value, true)?;
                Ok(JsValue::Object(receiver))
            }
            NativeFunction::CollectionAdd => {
                let value = required_argument(arguments, 0, "Set.add")?.clone();
                self.collection_store(receiver, value.clone(), value, false)?;
                Ok(JsValue::Object(receiver))
            }
            NativeFunction::CollectionHas => self.collection_has(receiver, arguments),
            NativeFunction::CollectionDelete => self.collection_delete(receiver, arguments),
            NativeFunction::CollectionClear => self.collection_clear(receiver),
            NativeFunction::CollectionForEach => self.collection_for_each(dom, receiver, arguments),
            NativeFunction::CollectionKeys => {
                self.collection_iterator(receiver, CollectionView::Keys)
            }
            NativeFunction::CollectionValues => {
                self.collection_iterator(receiver, CollectionView::Values)
            }
            NativeFunction::CollectionEntries => {
                self.collection_iterator(receiver, CollectionView::Entries)
            }
            NativeFunction::CollectionIteratorNext => self.collection_iterator_next(receiver),
            NativeFunction::ObjectPrototypePropertyIsEnumerable => {
                self.object_prototype_property_is_enumerable(receiver, arguments)
            }
            NativeFunction::ObjectPrototypeToString => Ok(JsValue::String(
                self.object_to_string_tag_for_object(receiver),
            )),
            NativeFunction::ObjectPrototypeValueOf => Ok(JsValue::Object(receiver)),
            NativeFunction::ErrorPrototypeToString => Ok(self.error_to_string(receiver)),
            NativeFunction::FunctionPrototype
            | NativeFunction::FunctionCall
            | NativeFunction::FunctionApply
            | NativeFunction::FunctionBind => {
                unreachable!("Function prototype methods use arbitrary receivers")
            }
        }
    }

    fn object_constructor(&mut self, arguments: &[JsValue]) -> Result<JsValue, JsError> {
        if let Some(JsValue::Object(object)) = arguments.first() {
            return Ok(JsValue::Object(*object));
        }
        self.ensure_heap_capacity(1)?;
        Ok(JsValue::Object(self.realm.create_ordinary_object()))
    }

    fn collection_get(
        &self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let key = required_argument(arguments, 0, "Map.get")?;
        let Some(ObjectHost::Collection { kind, entries }) = self.realm.host(receiver) else {
            return Err(JsError::type_error("incompatible Map receiver"));
        };
        if !kind.is_map() {
            return Err(JsError::type_error("Map.get called on a Set"));
        }
        Ok(entries
            .iter()
            .find(|(candidate, _)| same_value_zero(candidate, key))
            .map_or(JsValue::Undefined, |(_, value)| value.clone()))
    }

    fn collection_store(
        &mut self,
        receiver: ObjectId,
        key: JsValue,
        value: JsValue,
        map_method: bool,
    ) -> Result<(), JsError> {
        let Some(ObjectHost::Collection { kind, .. }) = self.realm.host(receiver) else {
            return Err(JsError::type_error("incompatible collection receiver"));
        };
        if kind.is_map() != map_method {
            return Err(JsError::type_error(
                "collection method used with the wrong receiver",
            ));
        }
        if kind.is_weak() && !matches!(key, JsValue::Object(_)) {
            return Err(JsError::type_error("weak collection keys must be objects"));
        }
        let Some(ObjectHost::Collection { entries, .. }) = self.realm.host_mut(receiver) else {
            unreachable!("collection host checked above")
        };
        if let Some((_, stored)) = entries
            .iter_mut()
            .find(|(candidate, _)| same_value_zero(candidate, &key))
        {
            *stored = value;
        } else {
            entries.push((key, value));
        }
        Ok(())
    }

    fn collection_has(
        &self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let key = required_argument(arguments, 0, "collection.has")?;
        let Some(ObjectHost::Collection { entries, .. }) = self.realm.host(receiver) else {
            return Err(JsError::type_error("incompatible collection receiver"));
        };
        Ok(JsValue::Boolean(
            entries
                .iter()
                .any(|(candidate, _)| same_value_zero(candidate, key)),
        ))
    }

    fn collection_delete(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let key = required_argument(arguments, 0, "collection.delete")?;
        let Some(ObjectHost::Collection { entries, .. }) = self.realm.host_mut(receiver) else {
            return Err(JsError::type_error("incompatible collection receiver"));
        };
        let Some(index) = entries
            .iter()
            .position(|(candidate, _)| same_value_zero(candidate, key))
        else {
            return Ok(JsValue::Boolean(false));
        };
        entries.remove(index);
        Ok(JsValue::Boolean(true))
    }

    fn collection_clear(&mut self, receiver: ObjectId) -> Result<JsValue, JsError> {
        let Some(ObjectHost::Collection { kind, entries }) = self.realm.host_mut(receiver) else {
            return Err(JsError::type_error("incompatible collection receiver"));
        };
        if kind.is_weak() {
            return Err(JsError::type_error("weak collections cannot be cleared"));
        }
        entries.clear();
        Ok(JsValue::Undefined)
    }

    fn collection_for_each(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let callback = Self::require_callable_object(
            required_argument(arguments, 0, "collection.forEach")?,
            &self.realm,
        )?;
        let this_argument = arguments.get(1).cloned().unwrap_or(JsValue::Undefined);
        let Some(ObjectHost::Collection { kind, entries }) = self.realm.host(receiver) else {
            return Err(JsError::type_error("incompatible collection receiver"));
        };
        if kind.is_weak() {
            return Err(JsError::type_error("weak collections are not enumerable"));
        }
        for (key, value) in entries {
            let callback_arguments = if kind.is_map() {
                [value, key, JsValue::Object(receiver)]
            } else {
                [key.clone(), key, JsValue::Object(receiver)]
            };
            self.call_with_this(dom, callback, &callback_arguments, this_argument.clone())?;
        }
        Ok(JsValue::Undefined)
    }

    fn collection_iterator(
        &mut self,
        receiver: ObjectId,
        view: CollectionView,
    ) -> Result<JsValue, JsError> {
        let Some(ObjectHost::Collection { kind, entries }) = self.realm.host(receiver) else {
            return Err(JsError::type_error("incompatible collection receiver"));
        };
        if kind.is_weak() {
            return Err(JsError::type_error("weak collections are not enumerable"));
        }
        let mut values = Vec::with_capacity(entries.len());
        for (key, value) in entries {
            values.push(match view {
                CollectionView::Keys => key,
                CollectionView::Values if kind.is_map() => value,
                CollectionView::Values => key,
                CollectionView::Entries => {
                    JsValue::Object(self.create_array_from_values(&[key, value])?)
                }
            });
        }
        self.ensure_heap_capacity(1)?;
        Ok(JsValue::Object(self.realm.collection_iterator(values)))
    }

    fn collection_iterator_next(&mut self, receiver: ObjectId) -> Result<JsValue, JsError> {
        let value = {
            let Some(ObjectHost::CollectionIterator { values, index }) =
                self.realm.host_mut(receiver)
            else {
                return Err(JsError::type_error(
                    "incompatible collection iterator receiver",
                ));
            };
            let value = values.get(*index).cloned();
            *index = index.saturating_add(1);
            value
        };
        self.ensure_heap_capacity(1)?;
        let result = self.realm.create_ordinary_object();
        self.realm
            .set_property(result, "done".to_owned(), JsValue::Boolean(value.is_none()));
        self.realm.set_property(
            result,
            "value".to_owned(),
            value.unwrap_or(JsValue::Undefined),
        );
        Ok(JsValue::Object(result))
    }

    fn event_constructor(&mut self, arguments: &[JsValue]) -> Result<JsValue, JsError> {
        let event_type = required_argument(arguments, 0, "Event")?.to_js_string();
        if event_type.is_empty() {
            return Err(JsError::type_error("Event type must not be empty"));
        }
        let options = arguments.get(1).and_then(|value| match value {
            JsValue::Object(object) => Some(*object),
            _ => None,
        });
        let bubbles = options
            .and_then(|object| self.realm.get_property(object, "bubbles"))
            .is_some_and(|value| value.is_truthy());
        let cancelable = options
            .and_then(|object| self.realm.get_property(object, "cancelable"))
            .is_some_and(|value| value.is_truthy());
        let constructor = self
            .realm
            .global("Event")
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            })
            .ok_or_else(|| JsError::type_error("Event constructor is unavailable"))?;
        let prototype = self
            .realm
            .get_property(constructor, "prototype")
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            });
        self.ensure_heap_capacity(1)?;
        let event = self.realm.create_object(prototype);
        for (name, value) in [
            ("type", JsValue::String(event_type)),
            ("bubbles", JsValue::Boolean(bubbles)),
            ("cancelable", JsValue::Boolean(cancelable)),
            ("defaultPrevented", JsValue::Boolean(false)),
            ("target", JsValue::Null),
            ("currentTarget", JsValue::Null),
        ] {
            self.realm.set_property(event, name.to_owned(), value);
        }
        Ok(JsValue::Object(event))
    }

    fn event_target_node(&self, receiver: ObjectId) -> Result<NodeId, JsError> {
        match self.realm.host(receiver) {
            Some(ObjectHost::Document(node) | ObjectHost::Node(node)) => Ok(node),
            _ => Err(JsError::type_error(
                "incompatible EventTarget method receiver",
            )),
        }
    }

    fn add_event_listener(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let target = self.event_target_node(receiver)?;
        let event_type = required_argument(arguments, 0, "addEventListener")?.to_js_string();
        let callback = match arguments.get(1) {
            None | Some(JsValue::Null | JsValue::Undefined) => return Ok(JsValue::Undefined),
            Some(value) => Self::require_callable_object(value, &self.realm)?,
        };
        let callbacks = self
            .event_listeners
            .entry(target)
            .or_default()
            .entry(event_type)
            .or_default();
        if !callbacks.contains(&callback) {
            callbacks.push(callback);
        }
        Ok(JsValue::Undefined)
    }

    fn remove_event_listener(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let target = self.event_target_node(receiver)?;
        let event_type = required_argument(arguments, 0, "removeEventListener")?.to_js_string();
        let Some(JsValue::Object(callback)) = arguments.get(1) else {
            return Ok(JsValue::Undefined);
        };
        if let Some(callbacks) = self
            .event_listeners
            .get_mut(&target)
            .and_then(|listeners| listeners.get_mut(&event_type))
        {
            callbacks.retain(|candidate| candidate != callback);
        }
        Ok(JsValue::Undefined)
    }

    fn dispatch_event(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let target = self.event_target_node(receiver)?;
        let event = Self::require_object(required_argument(arguments, 0, "dispatchEvent")?)?;
        let event_type = self
            .realm
            .get_property(event, "type")
            .map(|value| value.to_js_string())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| JsError::type_error("dispatchEvent argument is not an Event"))?;
        let bubbles = self
            .realm
            .get_property(event, "bubbles")
            .is_some_and(|value| value.is_truthy());
        let not_canceled =
            self.dispatch_prepared_event(dom, target, event, &event_type, bubbles)?;
        Ok(JsValue::Boolean(not_canceled))
    }

    /// Walk the event path from `target` upward, invoking matching listeners
    /// and `on*` handlers. Returns whether no listener called
    /// `preventDefault()`.
    /// `window.addEventListener`: listeners live outside the node tree.
    /// Read indexed elements from an object that may or may not be an Array.
    fn array_elements_for(&mut self, object: ObjectId) -> Vec<JsValue> {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "length is validated as a finite non-negative integer"
        )]
        let length = self
            .realm
            .get_property(object, "length")
            .and_then(|value| match &value {
                JsValue::Number(n) if n.is_finite() && *n >= 0.0 => Some(*n as usize),
                _ => None,
            })
            .unwrap_or(0);
        (0..length)
            .map(|index| self.realm.get_property(object, &index.to_string()))
            .map(|value| value.unwrap_or(JsValue::Undefined))
            .collect()
    }

    fn add_window_listener(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        if receiver != self.realm.global_object() {
            return Err(JsError::type_error(
                "window listeners must be added on the global object",
            ));
        }
        let event_type = required_argument(arguments, 0, "addEventListener")?.to_js_string();
        let callback = Self::require_callable_object(
            required_argument(arguments, 1, "addEventListener")?,
            &self.realm,
        )?;
        self.window_event_handlers
            .entry(event_type)
            .or_default()
            .push(callback);
        Ok(JsValue::Undefined)
    }

    fn remove_window_listener(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        if receiver != self.realm.global_object() {
            return Err(JsError::type_error(
                "window listeners must be removed from the global object",
            ));
        }
        let event_type = required_argument(arguments, 0, "removeEventListener")?.to_js_string();
        let callback = Self::require_callable_object(
            required_argument(arguments, 1, "removeEventListener")?,
            &self.realm,
        )?;
        if let Some(listeners) = self.window_event_handlers.get_mut(&event_type) {
            listeners.retain(|candidate| *candidate != callback);
        }
        Ok(JsValue::Undefined)
    }

    fn dispatch_prepared_event(
        &mut self,
        dom: &mut Dom,
        target: NodeId,
        event: ObjectId,
        event_type: &str,
        bubbles: bool,
    ) -> Result<bool, JsError> {
        let receiver_wrapper = if target == dom.document() {
            self.realm.document_object()
        } else {
            self.ensure_heap_capacity(1)?;
            self.realm.node_wrapper(target)
        };
        self.realm.set_property(
            event,
            "target".to_owned(),
            JsValue::Object(receiver_wrapper),
        );

        let mut path = vec![target];
        if bubbles {
            let mut ancestor = dom.parent(target);
            while let Some(node) = ancestor {
                path.push(node);
                ancestor = dom.parent(node);
            }
        }
        for node in path {
            let current_target = if node == dom.document() {
                self.realm.document_object()
            } else {
                self.ensure_heap_capacity(1)?;
                self.realm.node_wrapper(node)
            };
            self.realm.set_property(
                event,
                "currentTarget".to_owned(),
                JsValue::Object(current_target),
            );
            let callbacks = self
                .event_listeners
                .get(&node)
                .and_then(|listeners| listeners.get(event_type))
                .cloned()
                .unwrap_or_default();
            for callback in callbacks {
                self.call_with_this(
                    dom,
                    callback,
                    &[JsValue::Object(event)],
                    JsValue::Object(current_target),
                )?;
            }
            if let Some(callback) = self
                .event_handlers
                .get(&node)
                .and_then(|handlers| handlers.get(event_type))
                .copied()
            {
                self.call_with_this(
                    dom,
                    callback,
                    &[JsValue::Object(event)],
                    JsValue::Object(current_target),
                )?;
            }
        }
        // Events bubble to the window object last.
        let window_callbacks = self
            .window_event_handlers
            .get(event_type)
            .cloned()
            .unwrap_or_default();
        for callback in window_callbacks {
            self.realm.set_property(
                event,
                "currentTarget".to_owned(),
                JsValue::Object(self.realm.global_object()),
            );
            self.call_with_this(
                dom,
                callback,
                &[JsValue::Object(event)],
                JsValue::Object(self.realm.global_object()),
            )?;
        }
        self.realm
            .set_property(event, "currentTarget".to_owned(), JsValue::Null);
        let canceled = self
            .realm
            .get_property(event, "defaultPrevented")
            .is_some_and(|value| value.is_truthy());
        Ok(!canceled)
    }

    fn register_timer_entry(&mut self, callback: ObjectId, delay_ms: f64, kind: TimerKind) -> f64 {
        let id = self.next_timer_id;
        self.next_timer_id += 1;
        self.timers.insert(
            id,
            TimerEntry {
                kind,
                callback,
                delay_ms,
            },
        );
        self.pending_timer_requests
            .push(TimerRequest::Schedule { id, delay_ms });
        #[allow(
            clippy::cast_precision_loss,
            reason = "timer ids stay far below the 2^53 exact-integer boundary"
        )]
        {
            id as f64
        }
    }

    fn register_timer(
        &mut self,
        function: NativeFunction,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let kind = match function {
            NativeFunction::SetInterval => TimerKind::Interval,
            _ => TimerKind::Timeout,
        };
        let name = if kind == TimerKind::Timeout {
            "setTimeout"
        } else {
            "setInterval"
        };
        let callback =
            Self::require_callable_object(required_argument(arguments, 0, name)?, &self.realm)?;
        let delay = match arguments.get(1) {
            None | Some(JsValue::Undefined) => 0.0,
            Some(value) => to_number(value)?,
        };
        let delay_ms = if delay.is_nan() { 0.0 } else { delay.max(0.0) };
        Ok(JsValue::Number(
            self.register_timer_entry(callback, delay_ms, kind),
        ))
    }

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

    fn cancel_timer(&mut self, arguments: &[JsValue]) -> JsValue {
        if let Some(id) = optional_timer_id(arguments.first().unwrap_or(&JsValue::Undefined))
            && self.timers.remove(&id).is_some()
        {
            self.pending_timer_requests
                .push(TimerRequest::Cancel { id });
        }
        JsValue::Undefined
    }

    /// Format `console.*` arguments the way engines join them: one space
    /// between arguments, objects through their string coercion.
    fn console_write(
        &mut self,
        function: NativeFunction,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let level = match function {
            NativeFunction::ConsoleDebug => ConsoleLevel::Debug,
            NativeFunction::ConsoleError => ConsoleLevel::Error,
            NativeFunction::ConsoleInfo => ConsoleLevel::Info,
            NativeFunction::ConsoleLog => ConsoleLevel::Log,
            NativeFunction::ConsoleWarn => ConsoleLevel::Warn,
            _ => return Err(JsError::type_error("incompatible console method receiver")),
        };
        let text = arguments
            .iter()
            .map(JsValue::to_js_string)
            .collect::<Vec<_>>()
            .join(" ");
        if self.console_messages.len() >= MAX_BUFFERED_CONSOLE_MESSAGES {
            self.console_messages.remove(0);
        }
        self.console_messages.push(ConsoleMessage { level, text });
        Ok(JsValue::Undefined)
    }

    fn element_rect_value(&mut self, node: NodeId) -> JsValue {
        let mut rect = self
            .element_geometry
            .get(&node.as_u64())
            .copied()
            .unwrap_or(ElementRect {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 0.0,
            });
        rect.x -= self.viewport.x;
        rect.y -= self.viewport.y;
        self.rect_value(rect)
    }

    fn rect_value(&mut self, rect: ElementRect) -> JsValue {
        let object = self.realm.create_ordinary_object();
        for (name, value) in [
            ("x", rect.x),
            ("y", rect.y),
            ("width", rect.width),
            ("height", rect.height),
            ("top", rect.y),
            ("right", rect.x + rect.width),
            ("bottom", rect.y + rect.height),
            ("left", rect.x),
        ] {
            self.realm
                .set_property(object, name.to_owned(), JsValue::Number(f64::from(value)));
        }
        JsValue::Object(object)
    }

    fn intersection_observer_constructor(
        &mut self,
        constructor: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let callback = Self::require_callable_object(
            required_argument(arguments, 0, "IntersectionObserver")?,
            &self.realm,
        )?;
        let prototype = self
            .realm
            .get_property(constructor, "prototype")
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            });
        self.ensure_heap_capacity(2)?;
        let observer = self.realm.create_object(prototype);
        *self
            .realm
            .host_mut(observer)
            .expect("newly created observer has host storage") = ObjectHost::IntersectionObserver {
            callback,
            targets: Vec::new(),
        };
        let options = arguments.get(1).and_then(|value| match value {
            JsValue::Object(object) => Some(*object),
            _ => None,
        });
        let root = options
            .and_then(|object| self.realm.get_property(object, "root"))
            .unwrap_or(JsValue::Null);
        let root_margin = options
            .and_then(|object| self.realm.get_property(object, "rootMargin"))
            .map_or_else(
                || "0px 0px 0px 0px".to_owned(),
                |value| value.to_js_string(),
            );
        let thresholds =
            match options.and_then(|object| self.realm.get_property(object, "threshold")) {
                Some(JsValue::Object(array)) => self.array_elements_for(array),
                Some(value) if !matches!(value, JsValue::Undefined) => vec![value],
                _ => vec![JsValue::Number(0.0)],
            };
        let thresholds = self.create_array_from_values(&thresholds)?;
        for (name, value) in [
            ("root", root),
            ("rootMargin", JsValue::String(root_margin)),
            ("thresholds", JsValue::Object(thresholds)),
        ] {
            self.realm.set_property(observer, name.to_owned(), value);
        }
        self.intersection_observers.push(observer);
        Ok(JsValue::Object(observer))
    }

    fn intersection_observe(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let target = self.value_as_node(required_argument(arguments, 0, "observe")?)?;
        let Some(ObjectHost::IntersectionObserver { targets, .. }) = self.realm.host_mut(receiver)
        else {
            return Err(JsError::type_error(
                "incompatible IntersectionObserver receiver",
            ));
        };
        if !targets.contains(&target) {
            targets.push(target);
            self.queue_intersection_observer(receiver);
        }
        Ok(JsValue::Undefined)
    }

    fn intersection_unobserve(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let target = self.value_as_node(required_argument(arguments, 0, "unobserve")?)?;
        let Some(ObjectHost::IntersectionObserver { targets, .. }) = self.realm.host_mut(receiver)
        else {
            return Err(JsError::type_error(
                "incompatible IntersectionObserver receiver",
            ));
        };
        targets.retain(|candidate| *candidate != target);
        Ok(JsValue::Undefined)
    }

    fn intersection_disconnect(&mut self, receiver: ObjectId) -> Result<JsValue, JsError> {
        let Some(ObjectHost::IntersectionObserver { targets, .. }) = self.realm.host_mut(receiver)
        else {
            return Err(JsError::type_error(
                "incompatible IntersectionObserver receiver",
            ));
        };
        targets.clear();
        self.pending_microtasks.retain(
            |task| !matches!(task, JsMicrotask::IntersectionObserver(observer) if *observer == receiver),
        );
        Ok(JsValue::Undefined)
    }

    fn intersection_take_records(&mut self, receiver: ObjectId) -> Result<JsValue, JsError> {
        if !matches!(
            self.realm.host(receiver),
            Some(ObjectHost::IntersectionObserver { .. })
        ) {
            return Err(JsError::type_error(
                "incompatible IntersectionObserver receiver",
            ));
        }
        Ok(JsValue::Object(self.create_array_from_values(&[])?))
    }

    fn queue_intersection_observer(&mut self, observer: ObjectId) {
        if !self
            .pending_microtasks
            .iter()
            .any(|task| matches!(task, JsMicrotask::IntersectionObserver(id) if *id == observer))
        {
            self.pending_microtasks
                .push(JsMicrotask::IntersectionObserver(observer));
        }
    }

    fn queue_intersection_observers(&mut self) {
        for observer in self.intersection_observers.clone() {
            if matches!(
                self.realm.host(observer),
                Some(ObjectHost::IntersectionObserver { targets, .. }) if !targets.is_empty()
            ) {
                self.queue_intersection_observer(observer);
            }
        }
    }

    fn notify_intersection_observer(
        &mut self,
        dom: &mut Dom,
        observer: ObjectId,
    ) -> Result<JsValue, JsError> {
        let Some(ObjectHost::IntersectionObserver { callback, targets }) =
            self.realm.host(observer)
        else {
            return Ok(JsValue::Undefined);
        };
        let mut entries = Vec::with_capacity(targets.len());
        for target in targets {
            if dom.node(target).is_none() {
                continue;
            }
            entries.push(self.intersection_entry(target)?);
        }
        if entries.is_empty() {
            return Ok(JsValue::Undefined);
        }
        let entries = self.create_array_from_values(&entries)?;
        self.call_with_this(
            dom,
            callback,
            &[JsValue::Object(entries), JsValue::Object(observer)],
            JsValue::Object(observer),
        )
    }

    fn intersection_entry(&mut self, target: NodeId) -> Result<JsValue, JsError> {
        let document_rect = self
            .element_geometry
            .get(&target.as_u64())
            .copied()
            .unwrap_or(ElementRect {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 0.0,
            });
        let left = document_rect.x.max(self.viewport.x);
        let top = document_rect.y.max(self.viewport.y);
        let right =
            (document_rect.x + document_rect.width).min(self.viewport.x + self.viewport.width);
        let bottom =
            (document_rect.y + document_rect.height).min(self.viewport.y + self.viewport.height);
        let intersection = ElementRect {
            x: left - self.viewport.x,
            y: top - self.viewport.y,
            width: (right - left).max(0.0),
            height: (bottom - top).max(0.0),
        };
        let area = document_rect.width.max(0.0) * document_rect.height.max(0.0);
        let intersection_area = intersection.width * intersection.height;
        let is_intersecting = intersection.width > 0.0 && intersection.height > 0.0;
        let ratio = if area > 0.0 {
            intersection_area / area
        } else {
            0.0
        };
        let constructor = self
            .realm
            .global("IntersectionObserverEntry")
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            });
        let prototype = constructor
            .and_then(|object| self.realm.get_property(object, "prototype"))
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            });
        self.ensure_heap_capacity(5)?;
        let entry = self.realm.create_object(prototype);
        let mut viewport_rect = document_rect;
        viewport_rect.x -= self.viewport.x;
        viewport_rect.y -= self.viewport.y;
        let root_bounds = ElementRect {
            x: 0.0,
            y: 0.0,
            width: self.viewport.width,
            height: self.viewport.height,
        };
        let bounding = self.rect_value(viewport_rect);
        let intersection_rect = self.rect_value(intersection);
        let root = self.rect_value(root_bounds);
        let target = self.wrap_node(target)?;
        for (name, value) in [
            ("time", JsValue::Number(Self::monotonic_now_ms())),
            ("target", target),
            ("rootBounds", root),
            ("boundingClientRect", bounding),
            ("intersectionRect", intersection_rect),
            ("isIntersecting", JsValue::Boolean(is_intersecting)),
            ("intersectionRatio", JsValue::Number(f64::from(ratio))),
        ] {
            self.realm.set_property(entry, name.to_owned(), value);
        }
        Ok(JsValue::Object(entry))
    }

    /// Declarations of the element's inline `style` attribute, in source order.
    fn inline_declarations(dom: &Dom, node: NodeId) -> Vec<(String, String, bool)> {
        let Ok(Some(source)) = dom.attribute(node, "style") else {
            return Vec::new();
        };
        parse_declaration_list(source)
            .0
            .into_iter()
            .map(|declaration| (declaration.name, declaration.value, declaration.important))
            .collect()
    }

    /// Serialize the element's inline declarations back into attribute text.
    fn write_inline_declarations(
        dom: &mut Dom,
        node: NodeId,
        declarations: &[(String, String, bool)],
    ) -> Result<(), DomError> {
        if declarations.is_empty() {
            dom.remove_attribute(node, "style")
        } else {
            let source = declarations
                .iter()
                .map(|(name, value, important)| {
                    if *important {
                        format!("{name}: {value} !important;")
                    } else {
                        format!("{name}: {value};")
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");
            dom.set_attribute(node, "style", &source)
        }
    }

    fn style_get_property(
        &mut self,
        dom: &Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let node = self.require_style_declaration(receiver)?;
        let requested = required_argument(arguments, 0, "getPropertyValue")?.to_js_string();
        let requested = requested.trim().to_ascii_lowercase();
        Ok(JsValue::String(
            Self::inline_declarations(dom, node)
                .into_iter()
                .find(|(name, _, _)| *name == requested)
                .map_or_else(String::new, |(_, value, _)| value),
        ))
    }

    fn style_set_property(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let node = self.require_style_declaration(receiver)?;
        let name = required_argument(arguments, 0, "setProperty")?
            .to_js_string()
            .trim()
            .to_ascii_lowercase();
        if !is_valid_property_name(&name) {
            return Err(JsError::dom(format!("invalid CSS property name {name:?}")));
        }
        let mut value = arguments
            .get(1)
            .unwrap_or(&JsValue::Undefined)
            .to_js_string();
        let important = arguments
            .get(2)
            .map(JsValue::to_js_string)
            .is_some_and(|priority| priority.eq_ignore_ascii_case("important"));
        value = value.trim().into();
        let mut declarations: Vec<(String, String, bool)> = Self::inline_declarations(dom, node)
            .into_iter()
            .filter(|(existing, _, _)| *existing != name)
            .collect();
        if !value.is_empty() {
            declarations.push((name, value, important));
        }
        Self::write_inline_declarations(dom, node, &declarations)?;
        Ok(JsValue::Undefined)
    }

    fn style_remove_property(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let node = self.require_style_declaration(receiver)?;
        let name = required_argument(arguments, 0, "removeProperty")?
            .to_js_string()
            .trim()
            .to_ascii_lowercase();
        let previous = Self::inline_declarations(dom, node)
            .into_iter()
            .find(|(existing, _, _)| *existing == name)
            .map_or_else(String::new, |(_, value, _)| value);
        let declarations: Vec<(String, String, bool)> = Self::inline_declarations(dom, node)
            .into_iter()
            .filter(|(existing, _, _)| *existing != name)
            .collect();
        Self::write_inline_declarations(dom, node, &declarations)?;
        Ok(JsValue::String(previous))
    }

    fn style_item(
        &mut self,
        dom: &Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let node = self.require_style_declaration(receiver)?;
        let index = match arguments.first() {
            Some(value) => to_number(value)?,
            None => return Ok(JsValue::String(String::new())),
        };
        let declarations = Self::inline_declarations(dom, node);
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "style indices are validated array positions"
        )]
        let position = index as usize;
        Ok(match declarations.get(position) {
            Some((name, _, _)) => JsValue::String(name.clone()),
            None => JsValue::String(String::new()),
        })
    }

    fn require_style_declaration(&self, object: ObjectId) -> Result<NodeId, JsError> {
        match self.realm.host(object) {
            Some(ObjectHost::CssStyleDeclaration(node)) => Ok(node),
            _ => Err(JsError::type_error(
                "incompatible CSSStyleDeclaration method receiver",
            )),
        }
    }

    /// Parse an `innerHTML` fragment and replace `target`'s children with it.
    ///
    /// The source is parsed through the ordinary HTML parser (body context is
    /// approximated), then imported node by node into a detached scratch
    /// parent so limits apply. Only once every node copied cleanly are the
    /// target's original children replaced; a failed import leaves the DOM
    /// untouched.
    ///
    /// # Errors
    ///
    /// Returns resource-limit errors when importing exceeds the configured
    /// DOM-node budget, and DOM errors when splicing fails.
    fn set_inner_html(
        &mut self,
        dom: &mut Dom,
        target: NodeId,
        source: &str,
    ) -> Result<(), JsError> {
        let scratch = crate::html::parse_document(source);
        let body = find_body_node(&scratch.dom, scratch.dom.document())
            .ok_or_else(|| JsError::dom("fragment parsing produced no body"))?;

        let staging = dom.create_element("fragment");
        for child in scratch.dom.children(body).unwrap_or_default().to_vec() {
            self.import_dom_subtree(dom, &scratch.dom, child, staging, true)?;
        }

        let old_children = dom.children(target).unwrap_or_default().to_vec();
        for child in old_children {
            dom.remove_child(target, child)?;
        }
        while let Some(child) = dom.children(staging).and_then(<[NodeId]>::first).copied() {
            dom.insert_before(target, child, None)?;
        }
        Ok(())
    }

    /// Parse an `outerHTML` fragment and replace `target` itself with it.
    ///
    /// Replacing a parentless node (or the document element) is rejected the
    /// way the platform rejects it, before any mutation happens.
    fn set_outer_html(
        &mut self,
        dom: &mut Dom,
        target: NodeId,
        source: &str,
    ) -> Result<(), JsError> {
        if target == dom.document() {
            return Err(JsError::dom("outerHTML cannot replace the document node"));
        }
        let Some(parent) = dom.parent(target) else {
            return Err(JsError::dom("outerHTML requires a parent to splice into"));
        };
        let scratch = crate::html::parse_document(source);
        let body = find_body_node(&scratch.dom, scratch.dom.document())
            .ok_or_else(|| JsError::dom("fragment parsing produced no body"))?;

        let staging = dom.create_element("fragment");
        for child in scratch.dom.children(body).unwrap_or_default().to_vec() {
            self.import_dom_subtree(dom, &scratch.dom, child, staging, true)?;
        }

        let next_sibling = dom.next_sibling(target);
        dom.remove_child(parent, target)?;
        while let Some(child) = dom.children(staging).and_then(<[NodeId]>::first).copied() {
            dom.insert_before(parent, child, next_sibling)?;
        }
        Ok(())
    }

    /// `node.cloneNode(deep)`: structural copy inside the same arena.
    fn clone_node_value(
        &mut self,
        dom: &mut Dom,
        node: NodeId,
        deep: bool,
    ) -> Result<JsValue, JsError> {
        let copy = self.clone_node_recursive(dom, node, deep)?;
        self.wrap_node(copy)
    }

    fn clone_node_recursive(
        &mut self,
        dom: &mut Dom,
        node: NodeId,
        deep: bool,
    ) -> Result<NodeId, JsError> {
        if self.dom_nodes_created >= self.limits.max_dom_nodes_created {
            return Err(JsError::resource("DOM node creation limit exceeded"));
        }
        let source = match dom.node(node).map(crate::dom::Node::kind) {
            Some(NodeKind::Element(element)) => CloneSource::Element {
                local_name: element.local_name.clone(),
                attributes: element
                    .attributes
                    .iter()
                    .map(|attribute| (attribute.local_name.clone(), attribute.value.clone()))
                    .collect(),
            },
            Some(NodeKind::Text(data)) => CloneSource::Text(data.clone()),
            Some(NodeKind::Comment(data)) => CloneSource::Comment(data.clone()),
            Some(NodeKind::DocumentFragment) => CloneSource::Fragment,
            _ => return Err(JsError::dom("this node type cannot be cloned here")),
        };
        let copy = match source {
            CloneSource::Element {
                local_name,
                attributes,
            } => {
                self.ensure_heap_capacity(1)?;
                self.dom_nodes_created = self.dom_nodes_created.saturating_add(1);
                let copy = dom.create_element(local_name);
                for (name, value) in attributes {
                    dom.set_attribute(copy, name, value)?;
                }
                copy
            }
            CloneSource::Text(data) => {
                self.ensure_heap_capacity(1)?;
                self.dom_nodes_created = self.dom_nodes_created.saturating_add(1);
                dom.create_text(data)
            }
            CloneSource::Comment(data) => {
                self.ensure_heap_capacity(1)?;
                self.dom_nodes_created = self.dom_nodes_created.saturating_add(1);
                dom.create_comment(data)
            }
            CloneSource::Fragment => {
                self.ensure_heap_capacity(1)?;
                self.dom_nodes_created = self.dom_nodes_created.saturating_add(1);
                dom.create_document_fragment()
            }
        };
        let children = if deep {
            dom.children(node).unwrap_or_default().to_vec()
        } else {
            Vec::new()
        };
        for child in children {
            let child_copy = self.clone_node_recursive(dom, child, true)?;
            dom.append_child(copy, child_copy)?;
        }
        Ok(copy)
    }

    fn import_dom_subtree(
        &mut self,
        target: &mut Dom,
        source: &Dom,
        node: NodeId,
        parent: NodeId,
        deep: bool,
    ) -> Result<(), JsError> {
        if self.dom_nodes_created >= self.limits.max_dom_nodes_created {
            return Err(JsError::resource("DOM node creation limit exceeded"));
        }
        let imported = match source.node(node).map(crate::dom::Node::kind) {
            Some(NodeKind::Element(element)) => {
                self.ensure_heap_capacity(1)?;
                self.dom_nodes_created = self.dom_nodes_created.saturating_add(1);
                let copy = target.create_element(element.local_name.clone());
                for attribute in &element.attributes {
                    target.set_attribute(
                        copy,
                        attribute.local_name.clone(),
                        attribute.value.clone(),
                    )?;
                }
                copy
            }
            Some(NodeKind::Text(data)) => {
                self.ensure_heap_capacity(1)?;
                self.dom_nodes_created = self.dom_nodes_created.saturating_add(1);
                target.create_text(data.clone())
            }
            Some(NodeKind::Comment(data)) => {
                self.ensure_heap_capacity(1)?;
                self.dom_nodes_created = self.dom_nodes_created.saturating_add(1);
                target.create_comment(data.clone())
            }
            // DocumentType / ProcessingInstruction nodes have no meaning
            // inside an element fragment.
            _ => return Ok(()),
        };
        target.append_child(parent, imported)?;
        if deep {
            for child in source.children(node).unwrap_or_default().to_vec() {
                self.import_dom_subtree(target, source, child, imported, true)?;
            }
        }
        Ok(())
    }

    fn event_prevent_default(&mut self, receiver: ObjectId) -> JsValue {
        let cancelable = self
            .realm
            .get_property(receiver, "cancelable")
            .is_some_and(|value| value.is_truthy());
        if cancelable {
            self.realm.set_property(
                receiver,
                "defaultPrevented".to_owned(),
                JsValue::Boolean(true),
            );
        }
        JsValue::Undefined
    }

    fn error_constructor(
        &mut self,
        constructor: ObjectId,
        _kind: ErrorKind,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        self.ensure_heap_capacity(1)?;
        let prototype = self
            .realm
            .get_property(constructor, "prototype")
            .and_then(|value| match value {
                JsValue::Object(prototype) => Some(prototype),
                _ => None,
            })
            .ok_or_else(|| JsError::type_error("Error constructor prototype is not an object"))?;
        let message = arguments
            .first()
            .and_then(|value| (!matches!(value, JsValue::Undefined)).then(|| value.to_js_string()));
        Ok(JsValue::Object(self.realm.create_error(prototype, message)))
    }

    fn error_to_string(&self, receiver: ObjectId) -> JsValue {
        let name = self
            .realm
            .get_property(receiver, "name")
            .unwrap_or_else(|| JsValue::String("Error".to_owned()))
            .to_js_string();
        let message = self
            .realm
            .get_property(receiver, "message")
            .unwrap_or_else(|| JsValue::String(String::new()))
            .to_js_string();
        let result = match (name.is_empty(), message.is_empty()) {
            (true, _) => message,
            (_, true) => name,
            (false, false) => format!("{name}: {message}"),
        };
        JsValue::String(result)
    }

    fn object_assign(&mut self, arguments: &[JsValue]) -> Result<JsValue, JsError> {
        let target_value = required_argument(arguments, 0, "Object.assign")?;
        let target = if matches!(target_value, JsValue::Null | JsValue::Undefined) {
            self.realm.create_ordinary_object()
        } else {
            Self::require_object(target_value)?
        };
        for source in &arguments[1..] {
            match source {
                JsValue::Object(source) => {
                    let properties = self
                        .realm
                        .enumerable_own_properties(*source)
                        .ok_or_else(|| JsError::type_error("Object.assign source is invalid"))?;
                    for (key, value) in properties {
                        if !self.realm.set_property(target, key, value) {
                            return Err(JsError::type_error(
                                "Object.assign could not write target property",
                            ));
                        }
                    }
                }
                JsValue::String(source) => {
                    for (index, character) in source.chars().enumerate() {
                        if !self.realm.set_property(
                            target,
                            index.to_string(),
                            JsValue::String(character.to_string()),
                        ) {
                            return Err(JsError::type_error(
                                "Object.assign could not write target property",
                            ));
                        }
                    }
                }
                JsValue::Undefined | JsValue::Null | JsValue::Boolean(_) | JsValue::Number(_) => {}
            }
        }
        Ok(JsValue::Object(target))
    }

    fn object_entries(
        &mut self,
        arguments: &[JsValue],
        kind: ObjectEntryKind,
    ) -> Result<JsValue, JsError> {
        let value = required_argument(arguments, 0, kind.function_name())?;
        let properties = match value {
            JsValue::Undefined | JsValue::Null => Vec::new(),
            JsValue::Object(object) => self
                .realm
                .enumerable_own_properties(*object)
                .ok_or_else(|| JsError::type_error("object is invalid"))?,
            JsValue::String(value) => value
                .chars()
                .enumerate()
                .map(|(index, character)| {
                    (index.to_string(), JsValue::String(character.to_string()))
                })
                .collect(),
            JsValue::Boolean(_) | JsValue::Number(_) => Vec::new(),
        };
        let mut output = Vec::with_capacity(properties.len());
        for (key, value) in properties {
            output.push(match kind {
                ObjectEntryKind::Keys => JsValue::String(key),
                ObjectEntryKind::Values => value,
                ObjectEntryKind::Entries => {
                    JsValue::Object(self.create_array_from_values(&[JsValue::String(key), value])?)
                }
            });
        }
        Ok(JsValue::Object(self.create_array_from_values(&output)?))
    }

    fn object_create(&mut self, arguments: &[JsValue]) -> Result<JsValue, JsError> {
        let prototype = match required_argument(arguments, 0, "Object.create")? {
            JsValue::Null => None,
            JsValue::Object(object) => Some(*object),
            // A missing optional base class is represented by `undefined` in
            // transpiled bundles.  Treat it as a null prototype so helper
            // setup can continue and the eventual subclass remains usable.
            _ => None,
        };
        self.ensure_heap_capacity(1)?;
        Ok(JsValue::Object(self.realm.create_object(prototype)))
    }

    fn object_define_property(&mut self, arguments: &[JsValue]) -> Result<JsValue, JsError> {
        let target = required_argument(arguments, 0, "Object.defineProperty")?;
        if matches!(target, JsValue::Null | JsValue::Undefined) {
            // Keep descriptor-heavy compatibility shims from aborting an
            // entire page when an optional host object is absent.
            return Ok(target.clone());
        }
        let object = Self::require_object(target)?;
        let key = required_argument(arguments, 1, "Object.defineProperty")?.to_js_string();
        let descriptor_value = required_argument(arguments, 2, "Object.defineProperty")?;
        if matches!(descriptor_value, JsValue::Null | JsValue::Undefined) {
            return Ok(JsValue::Object(object));
        }
        let descriptor = match descriptor_value {
            JsValue::Object(descriptor) => *descriptor,
            // Descriptor objects are often assembled by feature-detection
            // shims.  A primitive here has no descriptor fields; accepting it
            // as an empty descriptor keeps the target usable like browsers do
            // for permissive host objects.
            JsValue::String(_) | JsValue::Number(_) | JsValue::Boolean(_) => {
                return Ok(JsValue::Object(object));
            }
            JsValue::Null | JsValue::Undefined => unreachable!(),
        };
        let existing = self.realm.own_property(object, &key);
        let descriptor = PropertyDescriptor {
            value: self
                .realm
                .get_property(descriptor, "value")
                .or_else(|| existing.as_ref().map(|property| property.value.clone()))
                .unwrap_or(JsValue::Undefined),
            writable: self.realm.get_property(descriptor, "writable").map_or_else(
                || existing.as_ref().is_some_and(|property| property.writable),
                |value| value.is_truthy(),
            ),
            enumerable: self
                .realm
                .get_property(descriptor, "enumerable")
                .map_or_else(
                    || {
                        existing
                            .as_ref()
                            .is_some_and(|property| property.enumerable)
                    },
                    |value| value.is_truthy(),
                ),
            configurable: self
                .realm
                .get_property(descriptor, "configurable")
                .map_or_else(
                    || {
                        existing
                            .as_ref()
                            .is_some_and(|property| property.configurable)
                    },
                    |value| value.is_truthy(),
                ),
        };
        let descriptor_value = descriptor.value.clone();
        if !self.realm.define_property(object, key.clone(), descriptor) {
            // Browser polyfills frequently re-run their descriptor installer
            // after a partial initialization. If the existing property is
            // writable, applying its value is the observable part callers
            // rely on; keep the descriptor's non-configurable guard for
            // genuinely read-only properties.
            if !self.realm.set_property(object, key, descriptor_value) {
                return Err(JsError::type_error("cannot redefine object property"));
            }
        }
        Ok(JsValue::Object(object))
    }

    fn object_define_properties(&mut self, arguments: &[JsValue]) -> Result<JsValue, JsError> {
        let target_value = required_argument(arguments, 0, "Object.defineProperties")?;
        if matches!(target_value, JsValue::Null | JsValue::Undefined) {
            return Ok(target_value.clone());
        }
        let target = Self::require_object(target_value)?;
        let descriptors_value = required_argument(arguments, 1, "Object.defineProperties")?;
        if matches!(descriptors_value, JsValue::Null | JsValue::Undefined) {
            return Ok(JsValue::Object(target));
        }
        let descriptors = Self::require_object(descriptors_value)?;
        let properties = self
            .realm
            .enumerable_own_properties(descriptors)
            .unwrap_or_default();
        for (key, descriptor) in properties {
            self.object_define_property(&[
                JsValue::Object(target),
                JsValue::String(key),
                descriptor,
            ])?;
        }
        Ok(JsValue::Object(target))
    }

    fn object_get_own_property_descriptor(
        &mut self,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let object = Self::require_object(required_argument(
            arguments,
            0,
            "Object.getOwnPropertyDescriptor",
        )?)?;
        let key =
            required_argument(arguments, 1, "Object.getOwnPropertyDescriptor")?.to_js_string();
        let Some(descriptor) = self.realm.own_property(object, &key) else {
            return Ok(JsValue::Undefined);
        };
        self.ensure_heap_capacity(1)?;
        let result = self.realm.create_ordinary_object();
        for (key, value) in [
            ("value", descriptor.value),
            ("writable", JsValue::Boolean(descriptor.writable)),
            ("enumerable", JsValue::Boolean(descriptor.enumerable)),
            ("configurable", JsValue::Boolean(descriptor.configurable)),
        ] {
            self.realm.set_property(result, key.to_owned(), value);
        }
        if object == self.realm.element_prototype()
            && matches!(key.as_str(), "scrollTop" | "scrollLeft")
        {
            self.realm.set_property(
                result,
                "set".to_owned(),
                JsValue::Object(self.realm.function_prototype()),
            );
        }
        Ok(JsValue::Object(result))
    }

    fn object_get_own_property_descriptors(
        &mut self,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let value = required_argument(arguments, 0, "Object.getOwnPropertyDescriptors")?;
        if matches!(value, JsValue::Null | JsValue::Undefined) {
            return Ok(JsValue::Object(self.realm.create_ordinary_object()));
        }
        let object = Self::require_object(value)?;
        self.ensure_heap_capacity(1)?;
        let result = self.realm.create_ordinary_object();
        for key in self.realm.own_property_names(object).unwrap_or_default() {
            let Some(descriptor) = self.realm.own_property(object, &key) else {
                continue;
            };
            let descriptor_object = self.realm.create_ordinary_object();
            for (name, value) in [
                ("value", descriptor.value),
                ("writable", JsValue::Boolean(descriptor.writable)),
                ("enumerable", JsValue::Boolean(descriptor.enumerable)),
                ("configurable", JsValue::Boolean(descriptor.configurable)),
            ] {
                self.realm
                    .set_property(descriptor_object, name.to_owned(), value);
            }
            self.realm
                .set_property(result, key, JsValue::Object(descriptor_object));
        }
        Ok(JsValue::Object(result))
    }

    fn object_get_prototype_of(&self, arguments: &[JsValue]) -> Result<JsValue, JsError> {
        let value = required_argument(arguments, 0, "Object.getPrototypeOf")?;
        if matches!(value, JsValue::Null | JsValue::Undefined) {
            return Ok(JsValue::Null);
        }
        let object = Self::require_object(value)?;
        Ok(self
            .realm
            .object(object)
            .and_then(JsObject::prototype)
            .map_or(JsValue::Null, JsValue::Object))
    }

    fn object_get_own_property_names(&mut self, arguments: &[JsValue]) -> Result<JsValue, JsError> {
        let value = required_argument(arguments, 0, "Object.getOwnPropertyNames")?;
        if matches!(value, JsValue::Null | JsValue::Undefined) {
            return Ok(JsValue::Object(self.create_array_from_values(&[])?));
        }
        let object = Self::require_object(value)?;
        let names = self
            .realm
            .own_property_names(object)
            .ok_or_else(|| JsError::type_error("object is invalid"))?
            .into_iter()
            .map(JsValue::String)
            .collect::<Vec<_>>();
        Ok(JsValue::Object(self.create_array_from_values(&names)?))
    }

    fn object_has_own(&self, arguments: &[JsValue]) -> Result<JsValue, JsError> {
        let value = required_argument(arguments, 0, "Object.hasOwn")?;
        if matches!(value, JsValue::Null | JsValue::Undefined) {
            return Ok(JsValue::Boolean(false));
        }
        let object = Self::require_object(value)?;
        let key = required_argument(arguments, 1, "Object.hasOwn")?.to_js_string();
        Ok(JsValue::Boolean(
            self.realm.own_property(object, &key).is_some(),
        ))
    }

    fn object_prototype_has_own_property(
        &self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let key =
            required_argument(arguments, 0, "Object.prototype.hasOwnProperty")?.to_js_string();
        Ok(JsValue::Boolean(
            self.realm.own_property(receiver, &key).is_some(),
        ))
    }

    fn object_prototype_is_prototype_of(
        &self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> JsValue {
        let Some(&JsValue::Object(mut candidate)) = arguments.first() else {
            return JsValue::Boolean(false);
        };
        for _ in 0..self.realm.object_count() {
            let Some(object) = self.realm.object(candidate) else {
                return JsValue::Boolean(false);
            };
            let Some(prototype) = object.prototype() else {
                return JsValue::Boolean(false);
            };
            if prototype == receiver {
                return JsValue::Boolean(true);
            }
            candidate = prototype;
        }
        JsValue::Boolean(false)
    }

    fn object_prototype_property_is_enumerable(
        &self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let key = required_argument(arguments, 0, "Object.prototype.propertyIsEnumerable")?
            .to_js_string();
        Ok(JsValue::Boolean(
            self.realm
                .own_property(receiver, &key)
                .is_some_and(|descriptor| descriptor.enumerable),
        ))
    }

    fn create_array_from_values(&mut self, values: &[JsValue]) -> Result<ObjectId, JsError> {
        self.ensure_heap_capacity(1)?;
        let array = self.realm.create_array();
        for (index, value) in values.iter().enumerate() {
            self.realm
                .set_property(array, index.to_string(), value.clone());
        }
        let length = u32::try_from(values.len())
            .map_err(|_| JsError::resource("array result exceeds the supported u32 range"))?;
        self.set_array_length(array, length)?;
        Ok(array)
    }

    fn array_push(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let mut length = self.array_length(receiver)?;
        for value in arguments {
            if !self
                .realm
                .set_property(receiver, length.to_string(), value.clone())
            {
                return Err(JsError::type_error("could not append array element"));
            }
            length = length
                .checked_add(1)
                .ok_or_else(|| JsError::resource("Array.prototype.push exceeds u32 length"))?;
        }
        self.set_array_length(receiver, length)?;
        Ok(JsValue::Number(f64::from(length)))
    }

    fn array_pop(&mut self, receiver: ObjectId) -> Result<JsValue, JsError> {
        let length = self.array_length(receiver)?;
        if length == 0 {
            return Ok(JsValue::Undefined);
        }
        let index = length.saturating_sub(1);
        let value = self
            .realm
            .remove_property(receiver, &index.to_string())
            .unwrap_or(JsValue::Undefined);
        self.set_array_length(receiver, index)?;
        Ok(value)
    }

    fn array_join(&self, receiver: ObjectId, arguments: &[JsValue]) -> Result<JsValue, JsError> {
        let separator = match arguments.first() {
            None | Some(JsValue::Undefined) => ",".to_owned(),
            Some(value) => value.to_js_string(),
        };
        let length = self.array_length(receiver)?;
        let mut output = String::new();
        for index in 0..length {
            if index != 0 {
                output.push_str(&separator);
            }
            match self.realm.get_property(receiver, &index.to_string()) {
                None | Some(JsValue::Undefined | JsValue::Null) => {}
                Some(value) => output.push_str(&value.to_js_string()),
            }
        }
        Ok(JsValue::String(output))
    }

    /// Read all indexed elements (holes become `undefined`).
    fn array_elements(&self, receiver: ObjectId) -> Result<Vec<JsValue>, JsError> {
        let length = self.array_length(receiver)?;
        Ok((0..length)
            .map(|index| self.realm.get_property(receiver, &index.to_string()))
            .map(|value| value.unwrap_or(JsValue::Undefined))
            .collect())
    }

    /// Replace the indexed elements of `receiver`, updating its length.
    fn set_array_elements(
        &mut self,
        receiver: ObjectId,
        values: &[JsValue],
    ) -> Result<(), JsError> {
        let old_length = self.array_length(receiver)?;
        for index in 0..old_length {
            self.realm.remove_property(receiver, &index.to_string());
        }
        for (index, value) in values.iter().enumerate() {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "index comes from a bounded u32 loop"
            )]
            self.realm
                .set_property(receiver, index.to_string(), value.clone());
        }
        #[allow(
            clippy::cast_possible_truncation,
            reason = "array element counts stay far below the u32 boundary"
        )]
        #[allow(
            clippy::cast_possible_truncation,
            reason = "array element counts stay far below the u32 boundary"
        )]
        self.set_array_length(receiver, values.len() as u32)
    }

    fn array_index_of(
        &self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let needle = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let elements = self.array_elements(receiver)?;
        let start = match arguments.get(1) {
            None | Some(JsValue::Undefined) => 0usize,
            Some(value) => {
                let raw = to_number(value)?;
                if raw < 0.0 {
                    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                    let from_end = (-raw) as usize;
                    elements.len().saturating_sub(from_end)
                } else {
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    {
                        raw as usize
                    }
                }
            }
        };
        for (index, element) in elements.iter().enumerate().skip(start) {
            if strict_equal(element, &needle) {
                #[allow(clippy::cast_precision_loss)]
                return Ok(JsValue::Number(index as f64));
            }
        }
        Ok(JsValue::Number(-1.0))
    }

    fn array_slice(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let elements = self.array_elements(receiver)?;
        let resolve = |raw: f64, length: usize| -> usize {
            if raw < 0.0 {
                #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                let from_end = (-raw) as usize;
                length.saturating_sub(from_end)
            } else {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                {
                    (raw as usize).min(length)
                }
            }
        };
        let length = elements.len();
        let start = match arguments.first() {
            None | Some(JsValue::Undefined) => 0,
            Some(value) => resolve(to_number(value)?, length),
        };
        let end = match arguments.get(1) {
            None | Some(JsValue::Undefined) => length,
            Some(value) => resolve(to_number(value)?, length),
        };
        let clipped_start = start.min(end).min(length);
        let clipped_end = end.min(length);
        let picked = elements[clipped_start.min(clipped_end)..clipped_end].to_vec();
        Ok(JsValue::Object(self.create_array_from_values(&picked)?))
    }

    fn array_splice(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let elements = self.array_elements(receiver)?;
        let length = elements.len();
        let raw_start = match arguments.first() {
            None | Some(JsValue::Undefined) => 0.0,
            Some(value) => to_number(value)?,
        };
        let start = if raw_start < 0.0 {
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
            let from_end = (-raw_start) as usize;
            length.saturating_sub(from_end)
        } else {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            {
                (raw_start as usize).min(length)
            }
        };
        let delete_count = match arguments.get(1) {
            None | Some(JsValue::Undefined) => length - start,
            Some(value) => {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let raw = to_number(value)?.max(0.0) as usize;
                raw.min(length - start)
            }
        };
        let mut result = elements.clone();
        let removed: Vec<JsValue> = result.splice(start..start + delete_count, []).collect();
        for (offset, item) in arguments.iter().skip(2).enumerate() {
            result.insert(start + offset, item.clone());
        }
        self.set_array_elements(receiver, &result)?;
        Ok(JsValue::Object(self.create_array_from_values(&removed)?))
    }

    fn array_reverse(&mut self, receiver: ObjectId) -> Result<JsValue, JsError> {
        let mut elements = self.array_elements(receiver)?;
        elements.reverse();
        self.set_array_elements(receiver, &elements)?;
        Ok(JsValue::Object(self.create_array_from_values(&elements)?))
    }

    fn array_sort(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let mut elements = self.array_elements(receiver)?;
        let comparator = match arguments.first() {
            Some(JsValue::Object(object)) if Self::is_callable_object(*object, &self.realm) => {
                Some(*object)
            }
            _ => None,
        };
        // Insertion sort keeps comparator calls simple and stable enough.
        for index in 1..elements.len() {
            let mut position = index;
            while position > 0 {
                let keep = Self::sort_order(
                    self,
                    dom,
                    comparator,
                    &elements[position - 1],
                    &elements[position],
                )?;
                if keep <= 0 {
                    break;
                }
                elements.swap(position - 1, position);
                position -= 1;
            }
        }
        self.set_array_elements(receiver, &elements)?;
        Ok(JsValue::Object(receiver))
    }

    /// Comparator result: negative when `left` sorts before `right`.
    fn sort_order(
        &mut self,
        dom: &mut Dom,
        comparator: Option<ObjectId>,
        left: &JsValue,
        right: &JsValue,
    ) -> Result<i32, JsError> {
        if let Some(function) = comparator {
            let result = self.call(dom, function, &[left.clone(), right.clone()])?;
            let number = to_number(&result)?;
            return Ok(if number < 0.0 {
                -1
            } else {
                i32::from(number > 0.0)
            });
        }
        let left_text = match left {
            JsValue::Undefined => None,
            other => Some(other.to_js_string()),
        };
        let right_text = match right {
            JsValue::Undefined => None,
            other => Some(other.to_js_string()),
        };
        Ok(match (left_text, right_text) {
            (None, None) => 0,
            (None, Some(_)) => 1,
            (Some(_), None) => -1,
            (Some(a), Some(b)) => match a.cmp(&b) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            },
        })
    }

    fn array_concat(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let mut values = self.array_elements(receiver)?;
        for argument in arguments {
            if let JsValue::Object(object) = argument
                && matches!(self.realm.host(*object), Some(ObjectHost::Array))
            {
                values.extend(self.array_elements(*object)?);
                continue;
            }
            values.push(argument.clone());
        }
        Ok(JsValue::Object(self.create_array_from_values(&values)?))
    }

    fn array_shift(&mut self, receiver: ObjectId) -> Result<JsValue, JsError> {
        let mut elements = self.array_elements(receiver)?;
        if elements.is_empty() {
            self.set_array_length(receiver, 0)?;
            return Ok(JsValue::Undefined);
        }
        let first = elements.remove(0);
        self.set_array_elements(receiver, &elements)?;
        Ok(first)
    }

    fn array_unshift(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let mut elements = self.array_elements(receiver)?;
        elements.splice(0..0, arguments.iter().cloned());
        let new_length = elements.len();
        self.set_array_elements(receiver, &elements)?;
        #[allow(clippy::cast_precision_loss)]
        Ok(JsValue::Number(new_length as f64))
    }

    fn array_iterate_with(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        callback: ObjectId,
        map: bool,
        filter: bool,
    ) -> Result<JsValue, JsError> {
        let elements = self.array_elements(receiver)?;
        let mut mapped = Vec::with_capacity(elements.len());
        for (index, element) in elements.iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let keep = self.call(
                dom,
                callback,
                &[
                    element.clone(),
                    JsValue::Number(index as f64),
                    JsValue::Object(receiver),
                ],
            )?;
            if map {
                mapped.push(keep);
            } else if keep.is_truthy() {
                mapped.push(element.clone());
            }
        }
        if filter || map {
            Ok(JsValue::Object(self.create_array_from_values(&mapped)?))
        } else {
            Ok(JsValue::Undefined)
        }
    }

    fn array_some(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        callback: ObjectId,
    ) -> Result<JsValue, JsError> {
        let elements = self.array_elements(receiver)?;
        for (index, element) in elements.iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let matches = self.call(
                dom,
                callback,
                &[
                    element.clone(),
                    JsValue::Number(index as f64),
                    JsValue::Object(receiver),
                ],
            )?;
            if matches.is_truthy() {
                return Ok(JsValue::Boolean(true));
            }
        }
        Ok(JsValue::Boolean(false))
    }

    fn array_from(&mut self, dom: &mut Dom, arguments: &[JsValue]) -> Result<JsValue, JsError> {
        let source = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let mut values = match source {
            JsValue::Object(object)
                if matches!(self.realm.host(object), Some(ObjectHost::Array)) =>
            {
                self.array_elements_for(object)
            }
            JsValue::String(text) => text
                .chars()
                .map(|character| JsValue::String(character.to_string()))
                .collect(),
            JsValue::Object(object) => {
                let length = self
                    .realm
                    .get_property(object, "length")
                    .and_then(|value| to_number(&value).ok())
                    .unwrap_or(0.0)
                    .max(0.0) as usize;
                (0..length)
                    .map(|index| self.get_member(dom, object, &index.to_string()))
                    .collect::<Result<Vec<_>, _>>()?
            }
            JsValue::Null | JsValue::Undefined => Vec::new(),
            value => vec![value],
        };
        if let Some(mapper) = arguments.get(1)
            && let JsValue::Object(mapper) = mapper
        {
            let mapper = Self::require_callable_object(&JsValue::Object(*mapper), &self.realm)?;
            for (index, value) in values.iter_mut().enumerate() {
                #[allow(clippy::cast_precision_loss)]
                let index_value = JsValue::Number(index as f64);
                *value = self.call(dom, mapper, &[value.clone(), index_value])?;
            }
        }
        Ok(JsValue::Object(self.create_array_from_values(&values)?))
    }

    fn array_find(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
        index_result: bool,
    ) -> Result<JsValue, JsError> {
        let callback = Self::require_callable_object(
            required_argument(arguments, 0, "Array.find")?,
            &self.realm,
        )?;
        for (index, value) in self.array_elements(receiver)?.into_iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let matched = self.call(
                dom,
                callback,
                &[
                    value.clone(),
                    JsValue::Number(index as f64),
                    JsValue::Object(receiver),
                ],
            )?;
            if matched.is_truthy() {
                return if index_result {
                    #[allow(clippy::cast_precision_loss)]
                    Ok(JsValue::Number(index as f64))
                } else {
                    Ok(value)
                };
            }
        }
        if index_result {
            Ok(JsValue::Number(-1.0))
        } else {
            Ok(JsValue::Undefined)
        }
    }

    fn array_every(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let callback = Self::require_callable_object(
            required_argument(arguments, 0, "Array.every")?,
            &self.realm,
        )?;
        for (index, value) in self.array_elements(receiver)?.into_iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let matched = self.call(
                dom,
                callback,
                &[
                    value,
                    JsValue::Number(index as f64),
                    JsValue::Object(receiver),
                ],
            )?;
            if !matched.is_truthy() {
                return Ok(JsValue::Boolean(false));
            }
        }
        Ok(JsValue::Boolean(true))
    }

    fn array_includes(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let search = arguments.first().unwrap_or(&JsValue::Undefined);
        let start = arguments
            .get(1)
            .and_then(|value| to_number(value).ok())
            .unwrap_or(0.0)
            .max(0.0) as usize;
        Ok(JsValue::Boolean(
            self.array_elements(receiver)?
                .iter()
                .skip(start)
                .any(|value| same_value_zero(value, search)),
        ))
    }

    fn array_reduce(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let callback = Self::require_callable_object(
            required_argument(arguments, 0, "Array.reduce")?,
            &self.realm,
        )?;
        let values = self.array_elements(receiver)?;
        let mut index = 0_usize;
        let mut accumulator = if let Some(initial) = arguments.get(1) {
            initial.clone()
        } else {
            let Some(first) = values.first() else {
                return Err(JsError::type_error(
                    "Reduce of empty array with no initial value",
                ));
            };
            index = 1;
            first.clone()
        };
        while let Some(value) = values.get(index) {
            #[allow(clippy::cast_precision_loss)]
            let next = self.call(
                dom,
                callback,
                &[
                    accumulator,
                    value.clone(),
                    JsValue::Number(index as f64),
                    JsValue::Object(receiver),
                ],
            )?;
            accumulator = next;
            index = index.saturating_add(1);
        }
        Ok(accumulator)
    }

    fn math_unary(
        arguments: &[JsValue],
        operation: impl FnOnce(f64) -> f64,
    ) -> Result<JsValue, JsError> {
        Ok(JsValue::Number(operation(to_number(
            arguments.first().unwrap_or(&JsValue::Undefined),
        )?)))
    }

    fn math_min_max(
        arguments: &[JsValue],
        identity: f64,
        operation: impl Fn(f64, f64) -> f64,
    ) -> Result<JsValue, JsError> {
        let mut result = identity;
        for argument in arguments {
            let value = to_number(argument)?;
            if value.is_nan() {
                return Ok(JsValue::Number(f64::NAN));
            }
            result = operation(result, value);
        }
        Ok(JsValue::Number(result))
    }

    fn math_pow(arguments: &[JsValue]) -> Result<JsValue, JsError> {
        let base = arguments.first().unwrap_or(&JsValue::Undefined);
        let exponent = arguments.get(1).unwrap_or(&JsValue::Undefined);
        Ok(JsValue::Number(to_number(base)?.powf(to_number(exponent)?)))
    }

    fn array_length(&self, object: ObjectId) -> Result<u32, JsError> {
        let value = self
            .realm
            .get_property(object, "length")
            .unwrap_or(JsValue::Undefined);
        let number = to_number(&value)?;
        if number.is_nan() || number <= 0.0 {
            return Ok(0);
        }
        if number.is_infinite() {
            return Ok(u32::MAX);
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Ok(number.trunc().min(f64::from(u32::MAX)) as u32)
    }

    fn set_array_length_value(&mut self, object: ObjectId, value: &JsValue) -> Result<(), JsError> {
        let number = to_number(value)?;
        if !number.is_finite()
            || number < 0.0
            || number.fract() != 0.0
            || number > f64::from(u32::MAX)
        {
            return Err(JsError::type_error("invalid array length"));
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let length = number as u32;
        let old_length = self.array_length(object)?;
        if length < old_length {
            for name in self.realm.own_property_names(object).unwrap_or_default() {
                if let Some(index) = array_index(&name)
                    && index >= length
                    && !self.realm.delete_property(object, &name)
                {
                    return Err(JsError::type_error("could not shrink array length"));
                }
            }
        }
        self.set_array_length(object, length)
    }

    fn set_array_length(&mut self, object: ObjectId, length: u32) -> Result<(), JsError> {
        if self.realm.set_property(
            object,
            "length".to_owned(),
            JsValue::Number(f64::from(length)),
        ) {
            Ok(())
        } else {
            Err(JsError::type_error("array length is not writable"))
        }
    }

    fn create_promise(&mut self) -> Result<(usize, JsValue), JsError> {
        self.ensure_heap_capacity(1)?;
        let index = self.promises.len();
        let object = self.realm.promise(index);
        self.promises.push(PromiseRecord {
            state: PromiseState::Pending,
            reactions: Vec::new(),
        });
        Ok((index, JsValue::Object(object)))
    }

    fn resolve_promise(&mut self, promise: usize, value: &JsValue) {
        self.settle_promise(promise, value, true);
    }

    fn resolve_promise_value(&mut self, promise: usize, value: &JsValue) -> Result<(), JsError> {
        if let JsValue::Object(object) = value
            && let Some(ObjectHost::Promise(source)) = self.realm.host(*object)
        {
            if source == promise {
                self.reject_promise(
                    promise,
                    &JsValue::String("a promise cannot resolve to itself".to_owned()),
                );
                return Ok(());
            }
            let state = self
                .promises
                .get(source)
                .map(|record| record.state.clone())
                .ok_or_else(|| JsError::type_error("Promise object refers to unknown state"))?;
            match state {
                PromiseState::Pending => self.promises[source].reactions.push(PromiseReaction {
                    on_fulfilled: None,
                    on_rejected: None,
                    result_promise: promise,
                }),
                PromiseState::Fulfilled(value) => self.resolve_promise(promise, &value),
                PromiseState::Rejected(reason) => self.reject_promise(promise, &reason),
            }
            return Ok(());
        }
        self.resolve_promise(promise, value);
        Ok(())
    }

    fn reject_promise(&mut self, promise: usize, reason: &JsValue) {
        self.settle_promise(promise, reason, false);
    }

    fn settle_promise(&mut self, promise: usize, value: &JsValue, fulfilled: bool) {
        let Some(record) = self.promises.get_mut(promise) else {
            return;
        };
        if !matches!(record.state, PromiseState::Pending) {
            return;
        }
        record.state = if fulfilled {
            PromiseState::Fulfilled(value.clone())
        } else {
            PromiseState::Rejected(value.clone())
        };
        let reactions = std::mem::take(&mut record.reactions);
        for reaction in reactions {
            self.enqueue_reaction(&reaction, value.clone(), fulfilled);
        }
    }

    fn enqueue_reaction(&mut self, reaction: &PromiseReaction, argument: JsValue, fulfilled: bool) {
        let handler = if fulfilled {
            reaction.on_fulfilled
        } else {
            reaction.on_rejected
        };
        self.pending_microtasks.push(JsMicrotask::PromiseReaction {
            handler,
            argument,
            fulfilled,
            result_promise: reaction.result_promise,
        });
    }

    fn perform_promise_then(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let Some(ObjectHost::Promise(promise)) = self.realm.host(receiver) else {
            return Err(JsError::type_error("incompatible Promise method receiver"));
        };
        let on_fulfilled = self.optional_callable(arguments.first())?;
        let on_rejected = self.optional_callable(arguments.get(1))?;
        let (result_promise, result) = self.create_promise()?;
        let reaction = PromiseReaction {
            on_fulfilled,
            on_rejected,
            result_promise,
        };
        let state = self
            .promises
            .get(promise)
            .map(|record| record.state.clone())
            .ok_or_else(|| JsError::type_error("Promise object refers to unknown state"))?;
        match state {
            PromiseState::Pending => self.promises[promise].reactions.push(reaction),
            PromiseState::Fulfilled(value) => self.enqueue_reaction(&reaction, value, true),
            PromiseState::Rejected(reason) => self.enqueue_reaction(&reaction, reason, false),
        }
        Ok(result)
    }

    fn optional_callable(&self, value: Option<&JsValue>) -> Result<Option<ObjectId>, JsError> {
        let Some(value) = value else {
            return Ok(None);
        };
        if matches!(value, JsValue::Undefined | JsValue::Null) {
            return Ok(None);
        }
        Self::require_callable_object(value, &self.realm).map(Some)
    }

    fn require_callable_object(value: &JsValue, realm: &Realm) -> Result<ObjectId, JsError> {
        let object = Self::require_object(value)?;
        if matches!(
            realm.host(object),
            Some(
                ObjectHost::NativeFunction(_)
                    | ObjectHost::BoundFunction { .. }
                    | ObjectHost::BoundCallable { .. }
                    | ObjectHost::UserFunction(_)
                    | ObjectHost::ArrowFunction(_)
                    | ObjectHost::FunctionConstructor
                    | ObjectHost::StringConstructor
                    | ObjectHost::NumberConstructor
                    | ObjectHost::BooleanConstructor
                    | ObjectHost::DateConstructor
                    | ObjectHost::SymbolConstructor
                    | ObjectHost::ArrayConstructor
                    | ObjectHost::RegExpConstructor
                    | ObjectHost::EventConstructor
                    | ObjectHost::DomConstructor
                    | ObjectHost::ImageConstructor
                    | ObjectHost::IntersectionObserverConstructor
                    | ObjectHost::CollectionConstructor(_)
                    | ObjectHost::ErrorConstructor(_)
                    | ObjectHost::PromiseSettler { .. }
            )
        ) {
            Ok(object)
        } else {
            Err(JsError::type_error("value is not callable"))
        }
    }

    fn is_callable_object(object: ObjectId, realm: &Realm) -> bool {
        matches!(
            realm.host(object),
            Some(
                ObjectHost::NativeFunction(_)
                    | ObjectHost::BoundFunction { .. }
                    | ObjectHost::BoundCallable { .. }
                    | ObjectHost::UserFunction(_)
                    | ObjectHost::ArrowFunction(_)
                    | ObjectHost::FunctionConstructor
                    | ObjectHost::StringConstructor
                    | ObjectHost::NumberConstructor
                    | ObjectHost::BooleanConstructor
                    | ObjectHost::DateConstructor
                    | ObjectHost::SymbolConstructor
                    | ObjectHost::ArrayConstructor
                    | ObjectHost::RegExpConstructor
                    | ObjectHost::EventConstructor
                    | ObjectHost::DomConstructor
                    | ObjectHost::ImageConstructor
                    | ObjectHost::IntersectionObserverConstructor
                    | ObjectHost::CollectionConstructor(_)
                    | ObjectHost::ErrorConstructor(_)
                    | ObjectHost::PromiseSettler { .. }
            )
        )
    }

    fn query_root(&self, object: ObjectId) -> Result<NodeId, JsError> {
        match self.realm.host(object) {
            Some(ObjectHost::Document(document) | ObjectHost::Node(document)) => Ok(document),
            _ => Err(JsError::type_error(
                "querySelector method called on a non-Document/non-Element object",
            )),
        }
    }

    fn find_element_by_tag(
        &mut self,
        dom: &Dom,
        root: NodeId,
        tag: &str,
    ) -> Result<Option<NodeId>, JsError> {
        let mut pending = vec![root];
        while let Some(node) = pending.pop() {
            self.consume_step()?;
            if let Some(NodeKind::Element(data)) = dom.node(node).map(crate::dom::Node::kind)
                && data.local_name.eq_ignore_ascii_case(tag)
            {
                return Ok(Some(node));
            }
            pending.extend(dom.children(node).unwrap_or_default().iter().rev());
        }
        Ok(None)
    }

    fn class_list_tokens(dom: &Dom, node: NodeId) -> Result<Vec<String>, JsError> {
        let value = dom.attribute(node, "class")?.unwrap_or_default();
        Ok(value.split_ascii_whitespace().map(str::to_owned).collect())
    }

    fn require_class_list(&self, object: ObjectId) -> Result<NodeId, JsError> {
        match self.realm.host(object) {
            Some(ObjectHost::ClassList(node)) => Ok(node),
            _ => Err(JsError::type_error("incompatible DOMTokenList receiver")),
        }
    }

    fn class_list_token(
        arguments: &[JsValue],
        index: usize,
        function: &str,
    ) -> Result<String, JsError> {
        let token = required_argument(arguments, index, function)?.to_js_string();
        if token.is_empty()
            || token
                .chars()
                .any(|character| character.is_ascii_whitespace())
        {
            return Err(JsError::dom(format!(
                "{function} token must be non-empty and contain no ASCII whitespace"
            )));
        }
        Ok(token)
    }

    fn class_list_add(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let node = self.require_class_list(receiver)?;
        let mut tokens = Self::class_list_tokens(dom, node)?;
        let mut changed = false;
        for index in 0..arguments.len() {
            let token = Self::class_list_token(arguments, index, "classList.add")?;
            if !tokens.contains(&token) {
                tokens.push(token);
                changed = true;
            }
        }
        if changed {
            dom.set_attribute(node, "class", tokens.join(" "))?;
        }
        Ok(JsValue::Undefined)
    }

    fn class_list_remove(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let node = self.require_class_list(receiver)?;
        let mut tokens = Self::class_list_tokens(dom, node)?;
        let original_len = tokens.len();
        for index in 0..arguments.len() {
            let token = Self::class_list_token(arguments, index, "classList.remove")?;
            tokens.retain(|candidate| candidate != &token);
        }
        if tokens.len() != original_len {
            if tokens.is_empty() {
                dom.remove_attribute(node, "class")?;
            } else {
                dom.set_attribute(node, "class", tokens.join(" "))?;
            }
        }
        Ok(JsValue::Undefined)
    }

    fn class_list_toggle(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let node = self.require_class_list(receiver)?;
        let token = Self::class_list_token(arguments, 0, "classList.toggle")?;
        let mut tokens = Self::class_list_tokens(dom, node)?;
        let present = tokens.iter().any(|candidate| candidate == &token);
        let next = match arguments.get(1) {
            Some(force) => force.is_truthy(),
            None => !present,
        };
        if next && !present {
            tokens.push(token);
            dom.set_attribute(node, "class", tokens.join(" "))?;
        } else if !next && present {
            tokens.retain(|candidate| candidate != &token);
            if tokens.is_empty() {
                dom.remove_attribute(node, "class")?;
            } else {
                dom.set_attribute(node, "class", tokens.join(" "))?;
            }
        }
        Ok(JsValue::Boolean(next))
    }

    fn class_list_contains(
        &self,
        dom: &Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let node = self.require_class_list(receiver)?;
        let token = Self::class_list_token(arguments, 0, "classList.contains")?;
        Ok(JsValue::Boolean(
            Self::class_list_tokens(dom, node)?
                .iter()
                .any(|candidate| candidate == &token),
        ))
    }

    fn class_list_item(
        &self,
        dom: &Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let node = self.require_class_list(receiver)?;
        let index = to_number(required_argument(arguments, 0, "classList.item")?)?;
        if !index.is_finite() || index < 0.0 || index.fract() != 0.0 {
            return Ok(JsValue::Null);
        }
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "index was validated as a finite non-negative integer"
        )]
        let index = index as usize;
        Ok(Self::class_list_tokens(dom, node)?
            .get(index)
            .cloned()
            .map_or(JsValue::Null, JsValue::String))
    }

    fn class_list_to_string(&self, dom: &Dom, receiver: ObjectId) -> Result<JsValue, JsError> {
        let node = self.require_class_list(receiver)?;
        Ok(JsValue::String(
            Self::class_list_tokens(dom, node)?.join(" "),
        ))
    }

    fn find_element_by_id(&mut self, dom: &Dom, id: &str) -> Result<Option<NodeId>, JsError> {
        let mut pending = vec![dom.document()];
        while let Some(node) = pending.pop() {
            self.consume_step()?;
            if matches!(
                dom.node(node).map(crate::dom::Node::kind),
                Some(NodeKind::Element(_))
            ) && dom.attribute(node, "id")? == Some(id)
            {
                return Ok(Some(node));
            }
            if let Some(children) = dom.children(node) {
                pending.extend(children.iter().rev());
            }
        }
        Ok(None)
    }

    fn text_content(&mut self, dom: &Dom, node: NodeId) -> Result<String, JsError> {
        let Some(root) = dom.node(node) else {
            return Err(JsError::dom("DOM wrapper refers to an unknown node"));
        };
        if let NodeKind::Text(data) | NodeKind::Comment(data) = root.kind() {
            return Ok(data.clone());
        }
        let mut result = String::new();
        let mut pending = root.children().iter().rev().copied().collect::<Vec<_>>();
        while let Some(candidate) = pending.pop() {
            self.consume_step()?;
            let Some(current) = dom.node(candidate) else {
                continue;
            };
            if let NodeKind::Text(data) = current.kind() {
                result.push_str(data);
            }
            pending.extend(current.children().iter().rev());
        }
        Ok(result)
    }

    fn set_text_content(
        &mut self,
        dom: &mut Dom,
        node: NodeId,
        value: String,
    ) -> Result<(), JsError> {
        let kind = dom
            .node(node)
            .map(crate::dom::Node::kind)
            .ok_or_else(|| JsError::dom("DOM wrapper refers to an unknown node"))?;
        if matches!(
            kind,
            NodeKind::Text(_) | NodeKind::Comment(_) | NodeKind::ProcessingInstruction { .. }
        ) {
            dom.set_character_data(node, value)?;
            return Ok(());
        }
        if !matches!(kind, NodeKind::Element(_) | NodeKind::DocumentFragment) {
            return Ok(());
        }
        let children = dom.children(node).unwrap_or_default().to_vec();
        for child in children {
            self.consume_step()?;
            dom.remove_child(node, child)?;
        }
        if !value.is_empty() {
            if self.dom_nodes_created >= self.limits.max_dom_nodes_created {
                return Err(JsError::resource("DOM node creation limit exceeded"));
            }
            self.dom_nodes_created = self.dom_nodes_created.saturating_add(1);
            let text = dom.create_text(value);
            dom.append_child(node, text)?;
        }
        Ok(())
    }

    /// Accept an object receiver for member access, wrapping primitives that
    /// carry methods (strings) instead of rejecting them outright.
    fn coerce_member_base(&mut self, value: &JsValue, context: &str) -> Result<ObjectId, JsError> {
        match value {
            JsValue::Object(object) => Ok(*object),
            JsValue::Null | JsValue::Undefined => Err(JsError::type_error(format!(
                "cannot access .{context} of {} | stack {:?}",
                if matches!(value, JsValue::Null) {
                    "null"
                } else {
                    "undefined"
                },
                self.call_stack
            ))),
            JsValue::String(text) => Ok(self.string_wrapper(text.clone())),
            JsValue::Number(value) => Ok(self.realm.number_primitive_wrapper(*value)),
            JsValue::Boolean(value) => Ok(self.realm.boolean_primitive_wrapper(*value)),
        }
    }

    /// Create a transient wrapper object exposing string prototype members.
    fn string_wrapper(&mut self, value: String) -> ObjectId {
        self.realm.string_wrapper(value)
    }

    /// Compile `pattern` with `flags` and allocate a `RegExp` instance.
    fn construct_regex(&mut self, pattern: &str, flags: &str) -> Result<ObjectId, JsError> {
        let compiled = super::regex::compile(pattern, flags).map_err(|error| {
            JsError::syntax(
                format!("invalid regular expression /{pattern}/{flags}: {error}"),
                0,
            )
        })?;
        let flags_text = compiled.flags().describe();
        let global = compiled.flags().global;
        let ignore_case = compiled.flags().ignore_case;
        let multiline = compiled.flags().multiline;
        let dot_all = compiled.flags().dot_all;
        let sticky = compiled.flags().sticky;
        let index = self.regexes.len();
        self.regexes.push(RegexRecord {
            compiled,
            last_index: 0,
        });
        let object = self.realm.regexp_wrapper(index);
        for (name, value) in [
            ("source", JsValue::String(pattern.to_owned())),
            ("flags", JsValue::String(flags_text)),
            ("global", JsValue::Boolean(global)),
            ("ignoreCase", JsValue::Boolean(ignore_case)),
            ("multiline", JsValue::Boolean(multiline)),
            ("dotAll", JsValue::Boolean(dot_all)),
            ("sticky", JsValue::Boolean(sticky)),
            ("lastIndex", JsValue::Number(0.0)),
        ] {
            self.realm.set_property(object, name.to_owned(), value);
        }
        Ok(object)
    }

    fn regex_index(&self, object: ObjectId) -> Result<usize, JsError> {
        match self.realm.host(object) {
            Some(ObjectHost::RegExp(index)) => Ok(index),
            _ => Err(JsError::type_error("incompatible RegExp method receiver")),
        }
    }

    /// Interpret a `String.prototype` regex-or-string argument. Returns the
    /// regex record index plus whether iteration must honor `g`.
    fn coerce_pattern_argument(&mut self, value: &JsValue) -> Result<(usize, bool), JsError> {
        if let JsValue::Object(object) = value
            && let Some(ObjectHost::RegExp(index)) = self.realm.host(*object)
        {
            return Ok((index, self.regexes[index].compiled.flags().global));
        }
        let pattern = value.to_js_string();
        let object = self.construct_regex(&pattern, "")?;
        let Some(ObjectHost::RegExp(index)) = self.realm.host(object) else {
            return Err(JsError::type_error("regexp construction failed"));
        };
        Ok((index, false))
    }

    fn regex_exec_value(
        &mut self,
        index: usize,
        input: &[char],
        start: usize,
    ) -> Result<Option<JsValue>, JsError> {
        let Some(found) = self.regexes[index].compiled.find(input, start) else {
            return Ok(None);
        };
        let mut values = Vec::with_capacity(found.groups.len() + 1);
        values.push(JsValue::String(
            input[found.start..found.end].iter().collect(),
        ));
        for group in &found.groups {
            values.push(match group {
                Some((start, end)) => JsValue::String(input[*start..*end].iter().collect()),
                None => JsValue::Undefined,
            });
        }
        let array = self.create_array_from_values(&values)?;
        #[allow(
            clippy::cast_precision_loss,
            reason = "string lengths stay far below any precision boundary"
        )]
        let index_number = found.start as f64;
        self.realm
            .set_property(array, "index".to_owned(), JsValue::Number(index_number));
        self.realm.set_property(
            array,
            "input".to_owned(),
            JsValue::String(input.iter().collect()),
        );
        Ok(Some(JsValue::Object(array)))
    }

    fn regexp_exec(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let index = self.regex_index(receiver)?;
        let input: Vec<char> = required_argument(arguments, 0, "exec")?
            .to_js_string()
            .chars()
            .collect();
        let flags = self.regexes[index].compiled.flags();
        let track_last_index = flags.global || flags.sticky;
        let from = if track_last_index {
            self.regexes[index].last_index.min(input.len())
        } else {
            0
        };
        let found = self.regexes[index].compiled.find(&input, from);
        if let Some(value) = self.regex_exec_value(index, &input, from)? {
            if track_last_index && let Some(found) = found {
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "string lengths stay far below any precision boundary"
                )]
                let last = found.end as f64;
                self.regexes[index].last_index = found.end;
                self.realm
                    .set_property(receiver, "lastIndex".to_owned(), JsValue::Number(last));
            }
            Ok(value)
        } else {
            if track_last_index {
                self.regexes[index].last_index = 0;
                self.realm
                    .set_property(receiver, "lastIndex".to_owned(), JsValue::Number(0.0));
            }
            Ok(JsValue::Null)
        }
    }

    fn regexp_test(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let index = self.regex_index(receiver)?;
        let input: Vec<char> = required_argument(arguments, 0, "test")?
            .to_js_string()
            .chars()
            .collect();
        let flags = self.regexes[index].compiled.flags();
        let track_last_index = flags.global || flags.sticky;
        let from = if track_last_index {
            self.regexes[index].last_index.min(input.len())
        } else {
            0
        };
        Ok(
            if let Some(found) = self.regexes[index].compiled.find(&input, from) {
                if track_last_index {
                    self.regexes[index].last_index = found.end;
                    #[allow(
                        clippy::cast_precision_loss,
                        reason = "string lengths stay far below any precision boundary"
                    )]
                    let last = found.end as f64;
                    self.realm.set_property(
                        receiver,
                        "lastIndex".to_owned(),
                        JsValue::Number(last),
                    );
                }
                JsValue::Boolean(true)
            } else {
                if track_last_index {
                    self.regexes[index].last_index = 0;
                    self.realm
                        .set_property(receiver, "lastIndex".to_owned(), JsValue::Number(0.0));
                }
                JsValue::Boolean(false)
            },
        )
    }

    fn regexp_to_string(&mut self, receiver: ObjectId) -> Result<JsValue, JsError> {
        let index = self.regex_index(receiver)?;
        let source = self.regexes[index].compiled.source().to_owned();
        let flags = self.regexes[index].compiled.flags().describe();
        Ok(JsValue::String(format!("/{source}/{flags}")))
    }

    fn require_string_receiver(&self, receiver: ObjectId) -> Result<String, JsError> {
        match self.realm.host(receiver) {
            Some(ObjectHost::StringPrimitive(text)) => Ok(text.clone()),
            _ => Err(JsError::type_error("incompatible String method receiver")),
        }
    }

    fn string_char_at(
        &self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let text = self.require_string_receiver(receiver)?;
        let characters: Vec<char> = text.chars().collect();
        let position = optional_index(arguments.first()).unwrap_or(0.0);
        Ok(char_at_value(&characters, position))
    }

    fn string_char_code_at(
        &self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let text = self.require_string_receiver(receiver)?;
        let characters: Vec<char> = text.chars().collect();
        let position = optional_index(arguments.first()).unwrap_or(0.0);
        let Some(position) = valid_position(position, characters.len()) else {
            return Ok(JsValue::Number(f64::NAN));
        };
        #[allow(
            clippy::cast_precision_loss,
            reason = "code points fit exactly in binary64"
        )]
        let code = f64::from(characters[position] as u32);
        Ok(JsValue::Number(code))
    }

    fn string_from_char_code(arguments: &[JsValue]) -> Result<JsValue, JsError> {
        let mut units = Vec::with_capacity(arguments.len());
        for value in arguments {
            let number = to_number(value)?;
            let integer = if number.is_finite() {
                number.trunc()
            } else {
                0.0
            };
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "ECMAScript fromCharCode wraps every numeric argument modulo 2^16"
            )]
            let unit = integer.rem_euclid(65_536.0) as u16;
            units.push(unit);
        }
        let text = char::decode_utf16(units)
            .map(|result| {
                result.unwrap_or_else(|error| {
                    surrogate_placeholder(u32::from(error.unpaired_surrogate()))
                })
            })
            .collect();
        Ok(JsValue::String(text))
    }

    fn string_from_code_point(arguments: &[JsValue]) -> Result<JsValue, JsError> {
        let mut text = String::with_capacity(arguments.len());
        for value in arguments {
            let number = to_number(value)?;
            if !number.is_finite()
                || number.fract() != 0.0
                || !(0.0..=1_114_111.0).contains(&number)
            {
                return Err(JsError::type_error("invalid code point"));
            }
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "the preceding range and integer checks guarantee a Unicode scalar input"
            )]
            let code = number as u32;
            let Some(character) = char::from_u32(code) else {
                return Err(JsError::type_error("invalid code point"));
            };
            text.push(character);
        }
        Ok(JsValue::String(text))
    }

    fn string_index_of(
        &self,
        receiver: ObjectId,
        arguments: &[JsValue],
        from_end: bool,
    ) -> Result<JsValue, JsError> {
        let text = self.require_string_receiver(receiver)?;
        let needle = required_argument(arguments, 0, "indexOf")?.to_js_string();
        let characters: Vec<char> = text.chars().collect();
        let needle_characters: Vec<char> = needle.chars().collect();
        if needle_characters.is_empty() {
            #[allow(
                clippy::cast_precision_loss,
                reason = "string lengths stay far below any precision boundary"
            )]
            let position = if from_end {
                characters.len() as f64
            } else {
                0.0
            };
            return Ok(JsValue::Number(position));
        }
        let positions: Vec<usize> = (0..=characters.len().saturating_sub(needle_characters.len()))
            .filter(|start| {
                characters[*start..]
                    .iter()
                    .zip(&needle_characters)
                    .all(|(left, right)| left == right)
            })
            .collect();
        let found = if from_end {
            positions.into_iter().next_back()
        } else {
            positions.into_iter().next()
        };
        Ok(match found {
            Some(position) => {
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "string lengths stay far below any precision boundary"
                )]
                let position = position as f64;
                JsValue::Number(position)
            }
            None => JsValue::Number(-1.0),
        })
    }

    fn string_includes(
        &self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let text = self.require_string_receiver(receiver)?;
        let needle = required_argument(arguments, 0, "includes")?.to_js_string();
        Ok(JsValue::Boolean(text.contains(&needle)))
    }

    fn string_starts_or_ends_with(
        &self,
        receiver: ObjectId,
        arguments: &[JsValue],
        starts: bool,
    ) -> Result<JsValue, JsError> {
        let text = self.require_string_receiver(receiver)?;
        let needle = required_argument(arguments, 0, "startsWith")?.to_js_string();
        Ok(JsValue::Boolean(if starts {
            text.starts_with(&needle)
        } else {
            text.ends_with(&needle)
        }))
    }

    fn string_slice(&self, receiver: ObjectId, arguments: &[JsValue]) -> Result<JsValue, JsError> {
        let text = self.require_string_receiver(receiver)?;
        let characters: Vec<char> = text.chars().collect();
        let start = optional_index(arguments.first())?;
        let end = match arguments.get(1) {
            None | Some(JsValue::Undefined) => None,
            Some(value) => Some(to_number(value)?),
        };
        let range = slice_range(&characters, start, end, true);
        Ok(JsValue::String(
            characters[range.start..range.end].iter().collect(),
        ))
    }

    fn string_substring(
        &self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let text = self.require_string_receiver(receiver)?;
        let characters: Vec<char> = text.chars().collect();
        let start = optional_index(arguments.first())?;
        let end = match arguments.get(1) {
            None | Some(JsValue::Undefined) => None,
            Some(value) => Some(to_number(value)?),
        };
        let range = slice_range(&characters, start, end, false);
        Ok(JsValue::String(
            characters[range.start..range.end].iter().collect(),
        ))
    }

    fn string_to_case(
        &self,
        receiver: ObjectId,
        _arguments: &[JsValue],
        upper: bool,
    ) -> Result<JsValue, JsError> {
        let text = self.require_string_receiver(receiver)?;
        Ok(JsValue::String(if upper {
            text.to_uppercase()
        } else {
            text.to_lowercase()
        }))
    }

    fn string_trim(&self, receiver: ObjectId) -> Result<JsValue, JsError> {
        let text = self.require_string_receiver(receiver)?;
        Ok(JsValue::String(text.trim().to_owned()))
    }

    fn string_concat(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let mut text = self.require_string_receiver(receiver)?;
        for argument in arguments {
            text.push_str(&argument.to_js_string());
        }
        Ok(JsValue::String(text))
    }

    /// Split by a literal separator or a regular expression.
    fn string_split(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let text = self.require_string_receiver(receiver)?;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "split limits are clamped to the u32 range first"
        )]
        let limit = match arguments.get(1) {
            None | Some(JsValue::Undefined) => usize::MAX,
            Some(value) => to_number(value)?.max(0.0).min(f64::from(u32::MAX)) as usize,
        };
        let pieces = match arguments.first() {
            None | Some(JsValue::Undefined) => vec![text],
            Some(separator) => {
                let characters: Vec<char> = text.chars().collect();
                if let JsValue::Object(_) = separator {
                    let (index, _) = self.coerce_pattern_argument(separator)?;
                    split_by_regex(&self.regexes[index].compiled, &characters, limit)
                        .into_iter()
                        .map(|span| characters[span.0..span.1].iter().collect())
                        .collect()
                } else {
                    let separator = separator.to_js_string();
                    if separator.is_empty() {
                        characters
                            .iter()
                            .take(limit)
                            .map(std::string::ToString::to_string)
                            .collect()
                    } else if limit == 0 {
                        Vec::new()
                    } else {
                        text.split(&separator)
                            .take(limit)
                            .map(str::to_owned)
                            .collect()
                    }
                }
            }
        };
        let values = pieces.into_iter().map(JsValue::String).collect::<Vec<_>>();
        Ok(JsValue::Object(self.create_array_from_values(&values)?))
    }

    /// `String.prototype.match`: one exec-style result unless the regex is
    /// global, in which case every full match is collected.
    fn string_match(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let text = self.require_string_receiver(receiver)?;
        let characters: Vec<char> = text.chars().collect();
        let Some(argument) = arguments.first() else {
            let object = self.construct_regex("", "")?;
            let Some(ObjectHost::RegExp(index)) = self.realm.host(object) else {
                return Err(JsError::type_error("regexp construction failed"));
            };
            return match self.regex_exec_value(index, &characters, 0)? {
                Some(value) => Ok(value),
                None => Ok(JsValue::Null),
            };
        };
        let (index, global) = self.coerce_pattern_argument(argument)?;
        if !global {
            return match self.regex_exec_value(index, &characters, 0)? {
                Some(value) => Ok(value),
                None => Ok(JsValue::Null),
            };
        }
        let spans = collect_global_matches(&self.regexes[index].compiled, &characters);
        let values = spans
            .into_iter()
            .map(|(start, end)| JsValue::String(characters[start..end].iter().collect()))
            .collect::<Vec<_>>();
        if values.is_empty() {
            return Ok(JsValue::Null);
        }
        Ok(JsValue::Object(self.create_array_from_values(&values)?))
    }

    fn string_search(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let text = self.require_string_receiver(receiver)?;
        let characters: Vec<char> = text.chars().collect();
        let argument = required_argument(arguments, 0, "search")?;
        let (index, _) = self.coerce_pattern_argument(argument)?;
        Ok(match self.regexes[index].compiled.find(&characters, 0) {
            Some(found) => {
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "string lengths stay far below any precision boundary"
                )]
                let start = found.start as f64;
                JsValue::Number(start)
            }
            None => JsValue::Number(-1.0),
        })
    }

    /// `String.prototype.replace` with `$&`, `$1`–`$9`, `` $` ``, `$'`, `$$`
    /// expansion or a replacement function.
    fn string_replace(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let text = self.require_string_receiver(receiver)?;
        let search = required_argument(arguments, 0, "replace")?;
        let replacement = required_argument(arguments, 1, "replace")?.clone();
        let characters: Vec<char> = text.chars().collect();
        let (index, global) = self.coerce_pattern_argument(search)?;
        let compiled = self.regexes[index].compiled.clone();

        let mut output = String::new();
        let mut cursor = 0usize;
        let mut last_end = 0usize;
        while let Some(found) = compiled.find(&characters, cursor) {
            let replaced = match &replacement {
                JsValue::Object(callable) if Self::is_callable_object(*callable, &self.realm) => {
                    let mut call_arguments = vec![JsValue::String(
                        characters[found.start..found.end].iter().collect(),
                    )];
                    for group in &found.groups {
                        call_arguments.push(match group {
                            Some((start, end)) => {
                                JsValue::String(characters[*start..*end].iter().collect())
                            }
                            None => JsValue::Undefined,
                        });
                    }
                    #[allow(
                        clippy::cast_precision_loss,
                        reason = "string lengths stay far below any precision boundary"
                    )]
                    let position = found.start as f64;
                    call_arguments.push(JsValue::Number(position));
                    call_arguments.push(JsValue::String(text.clone()));
                    let produced = self.call(dom, *callable, &call_arguments)?;
                    produced.to_js_string()
                }
                other => expand_replacement(&other.to_js_string(), &characters, &found),
            };
            output.push_str(&characters[last_end..found.start].iter().collect::<String>());
            output.push_str(&replaced);
            last_end = found.end;
            if found.end == found.start {
                // Empty match: step past the position to guarantee progress.
                if found.end >= characters.len() {
                    break;
                }
                cursor = found.end + 1;
            } else {
                cursor = found.end;
            }
            if !global {
                break;
            }
        }
        output.push_str(
            &characters[last_end.min(characters.len())..]
                .iter()
                .collect::<String>(),
        );
        Ok(JsValue::String(output))
    }

    fn require_document(&self, object: ObjectId) -> Result<NodeId, JsError> {
        match self.realm.host(object) {
            Some(ObjectHost::Document(document)) => Ok(document),
            _ => Err(JsError::type_error("incompatible Document method receiver")),
        }
    }

    fn require_node(&self, object: ObjectId) -> Result<NodeId, JsError> {
        match self.realm.host(object) {
            Some(ObjectHost::Node(node)) => Ok(node),
            _ => Err(JsError::type_error("incompatible Node method receiver")),
        }
    }

    fn require_object(value: &JsValue) -> Result<ObjectId, JsError> {
        match value {
            JsValue::Object(object) => Ok(*object),
            JsValue::Null | JsValue::Undefined => Err(JsError::type_error(
                "cannot access a property of null or undefined",
            )),
            _ => Err(JsError::type_error(
                "primitive object coercion is not implemented in this runtime slice",
            )),
        }
    }

    fn value_as_node(&self, value: &JsValue) -> Result<NodeId, JsError> {
        let JsValue::Object(object) = value else {
            return Err(JsError::type_error("argument is not a Node"));
        };
        self.require_node(*object)
    }
    fn require_date_value(&self, receiver: ObjectId) -> Result<f64, JsError> {
        match self.realm.host(receiver) {
            Some(ObjectHost::DateInstance(ms)) => Ok(ms),
            _ => Err(JsError::type_error("incompatible Date method receiver")),
        }
    }

    fn date_getter(function: NativeFunction, ms: f64) -> JsValue {
        if !ms.is_finite() {
            return JsValue::Number(f64::NAN);
        }
        let (year, month, day, hour, minute, second, millis, weekday) = Self::date_components(ms);
        let value = match function {
            NativeFunction::DateGetFullYear | NativeFunction::DateGetUTCFullYear => year,
            NativeFunction::DateGetMonth | NativeFunction::DateGetUTCMonth => month,
            NativeFunction::DateGetDate | NativeFunction::DateGetUTCDate => day,
            NativeFunction::DateGetDay | NativeFunction::DateGetUTCDay => weekday,
            NativeFunction::DateGetHours | NativeFunction::DateGetUTCHours => hour,
            NativeFunction::DateGetMinutes | NativeFunction::DateGetUTCMinutes => minute,
            NativeFunction::DateGetSeconds | NativeFunction::DateGetUTCSeconds => second,
            NativeFunction::DateGetMilliseconds | NativeFunction::DateGetUTCMilliseconds => millis,
            NativeFunction::DateGetTimezoneOffset => 0,
            _ => 0,
        };
        JsValue::Number(f64::from(value))
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn date_components(ms: f64) -> (i32, i32, i32, i32, i32, i32, i32, i32) {
        let total_millis = ms.floor() as i64;
        let days = total_millis.div_euclid(86_400_000);
        let day_millis = total_millis.rem_euclid(86_400_000);
        let seconds = day_millis / 1000;
        let millis = day_millis % 1000;
        let z = days + 719_468;
        let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
        let day_of_era = z - era * 146_097;
        let year_of_era =
            (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let mut year = year_of_era + era * 400;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let month_prelude = (5 * day_of_year + 2) / 153;
        let month = if month_prelude < 10 {
            month_prelude + 3
        } else {
            month_prelude - 9
        };
        let day = day_of_year - (153 * month_prelude + 2) / 5 + 1;
        year += i64::from(month <= 2);
        let month = month - 1;
        let hour = seconds / 3600;
        let minute = (seconds % 3600) / 60;
        let second = seconds % 60;
        let weekday = (days + 4).rem_euclid(7);
        (
            year as i32,
            month as i32,
            day as i32,
            hour as i32,
            minute as i32,
            second as i32,
            millis as i32,
            weekday as i32,
        )
    }

    fn date_from_constructor_arguments(arguments: &[JsValue]) -> Result<f64, JsError> {
        match arguments {
            [] | [JsValue::Undefined] => Ok(Self::now_ms()),
            [JsValue::String(value)] => Ok(Self::parse_date_string(value).unwrap_or(f64::NAN)),
            [value] => to_number(value),
            _ => Self::date_from_utc_arguments(arguments),
        }
    }

    fn date_from_utc_arguments(arguments: &[JsValue]) -> Result<f64, JsError> {
        let number = |index: usize, default: f64| -> Result<f64, JsError> {
            Ok(match arguments.get(index) {
                None | Some(JsValue::Undefined) => default,
                Some(value) => to_number(value)?,
            })
        };
        let mut year = number(0, 0.0)? as i32;
        let month = number(1, 0.0)? as i32;
        let day = number(2, 1.0)? as i32;
        let hour = number(3, 0.0)? as i32;
        let minute = number(4, 0.0)? as i32;
        let second = number(5, 0.0)? as i32;
        let millis = number(6, 0.0)? as i32;
        if (0..=99).contains(&year) {
            year += 1900;
        }
        let days = days_from_civil(year, month + 1, 1) + i64::from(day - 1);
        Ok((days * 86_400_000
            + i64::from(hour) * 3_600_000
            + i64::from(minute) * 60_000
            + i64::from(second) * 1_000
            + i64::from(millis)) as f64)
    }

    fn parse_date_string(value: &str) -> Option<f64> {
        let value = value.trim();
        let (date, time) = value.split_once('T').or_else(|| value.split_once(' '))?;
        let mut date_parts = date.split('-');
        let year = date_parts.next()?.parse::<i32>().ok()?;
        let month = date_parts.next()?.parse::<i32>().ok()?;
        let day = date_parts.next()?.parse::<i32>().ok()?;
        let mut time_parts = time.trim_end_matches('Z').split(':');
        let hour = time_parts.next()?.parse::<i32>().ok()?;
        let minute = time_parts.next()?.parse::<i32>().ok()?;
        let second_part = time_parts.next().unwrap_or("0");
        let (second, millis) = if let Some((whole, fraction)) = second_part.split_once('.') {
            (
                whole.parse::<i32>().ok()?,
                format!("{fraction:0<3}")[..3].parse::<i32>().ok()?,
            )
        } else {
            (second_part.parse::<i32>().ok()?, 0)
        };
        let days = days_from_civil(year, month, day);
        Some(
            (days * 86_400_000
                + i64::from(hour) * 3_600_000
                + i64::from(minute) * 60_000
                + i64::from(second) * 1_000
                + i64::from(millis)) as f64,
        )
    }

    fn format_date_iso(ms: f64) -> String {
        let (year, month, day, hour, minute, second, millis, _) = Self::date_components(ms);
        format!(
            "{year:04}-{:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z",
            month + 1
        )
    }

    fn json_node_to_value(&mut self, node: JsonNode) -> Result<JsValue, JsError> {
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

    fn json_stringify_value(
        &mut self,
        value: &JsValue,
        stack: &mut BTreeSet<ObjectId>,
        depth: usize,
    ) -> Result<Option<String>, JsError> {
        if depth > 128 {
            return Err(JsError::resource("JSON nesting depth exceeded"));
        }
        Ok(match value {
            JsValue::Undefined => None,
            JsValue::Null => Some("null".to_owned()),
            JsValue::Boolean(value) => Some(value.to_string()),
            JsValue::Number(value) if value.is_finite() => {
                Some(super::value::number_to_string(*value))
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

    /// Milliseconds since the Unix epoch.
    fn now_ms() -> f64 {
        #[allow(
            clippy::cast_precision_loss,
            reason = "epoch milliseconds fit exactly in binary64 for millions of years"
        )]
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |duration| duration.as_millis() as f64)
    }

    fn monotonic_now_ms() -> f64 {
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        START
            .get_or_init(std::time::Instant::now)
            .elapsed()
            .as_secs_f64()
            * 1_000.0
    }

    /// Minimal UTC formatting: `Mon Jan 01 2026 00:00:00 GMT+0000`.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "epoch seconds fit i64 comfortably; indices are bounded by construction"
    )]
    fn format_date_utc(ms: f64) -> String {
        let total_seconds = (ms / 1000.0).floor();
        let days = (total_seconds / 86_400.0).floor() as i64;
        let seconds_of_day = total_seconds as i64 - days * 86_400;
        // Civil-from-days (Howard Hinnant's algorithm).
        let z = days + 719_468;
        let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
        let day_of_era = z - era * 146_097;
        let year_of_era =
            (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let mut year = year_of_era + era * 400;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let month_prelude = (5 * day_of_year + 2) / 153;
        let month = if month_prelude < 10 {
            month_prelude + 3
        } else {
            month_prelude - 9
        };
        year += i64::from(month <= 2);
        let month_index = (month - 1) as usize;
        let day_of_month = day_of_year - (153 * month_prelude + 2) / 5 + 1;
        let hour = seconds_of_day / 3600;
        let minute = (seconds_of_day % 3600) / 60;
        let second = seconds_of_day % 60;
        let weekday = (days + 4).rem_euclid(7) as usize;
        format!(
            "{:03} {} {:02} {} {:02}:{:02}:{:02} GMT+0000",
            DATE_WEEKDAYS[weekday],
            DATE_MONTHS[month_index.clamp(0, 11)],
            day_of_month,
            year,
            hour,
            minute,
            second,
        )
    }

    /// Percent-encode per RFC 3986; `keep_uri` preserves reserved characters.
    fn percent_encode(text: &str, keep_uri: bool) -> String {
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
    fn percent_decode(text: &str) -> Option<String> {
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

    /// `Object.prototype.toString` tag for any value (primitives included).
    fn object_to_string_tag(&self, value: &JsValue) -> String {
        let builtin = match value {
            JsValue::Undefined => return "[object Undefined]".to_owned(),
            JsValue::Null => return "[object Null]".to_owned(),
            JsValue::Boolean(_) => "Boolean",
            JsValue::Number(_) => "Number",
            JsValue::String(_) => "String",
            JsValue::Object(object) => {
                return self.object_to_string_tag_for_object(*object);
            }
        };
        format!("[object {builtin}]")
    }

    fn object_to_string_tag_for_object(&self, object: ObjectId) -> String {
        let host_tag = match self.realm.host(object) {
            Some(ObjectHost::Array) => "Array",
            Some(ObjectHost::RegExp(_)) => "RegExp",
            Some(ObjectHost::StringPrimitive(_)) => "String",
            Some(ObjectHost::Document(_)) => "HTMLDocument",
            _ if Self::is_callable_object(object, &self.realm) => "Function",
            _ => "Object",
        };
        format!("[object {host_tag}]")
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

    fn wrap_node(&mut self, node: NodeId) -> Result<JsValue, JsError> {
        self.ensure_heap_capacity(1)?;
        Ok(JsValue::Object(self.realm.node_wrapper(node)))
    }

    fn ensure_heap_capacity(&self, additional: usize) -> Result<(), JsError> {
        if self.realm.object_count().saturating_add(additional) > self.limits.max_heap_objects {
            Err(JsError::resource("JavaScript object heap limit exceeded"))
        } else {
            Ok(())
        }
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
}

fn collect_var_names(statement: &Statement, names: &mut BTreeSet<String>) {
    match statement {
        Statement::Variable {
            kind: VariableKind::Var,
            name,
            ..
        } => {
            names.insert(name.clone());
        }
        Statement::VariableList {
            kind: VariableKind::Var,
            declarations,
            ..
        } => {
            for (name, _) in declarations {
                names.insert(name.clone());
            }
        }
        Statement::If {
            consequent,
            alternate,
            ..
        } => {
            collect_var_names(consequent, names);
            if let Some(alternate) = alternate {
                collect_var_names(alternate, names);
            }
        }
        Statement::Switch { cases, .. } => {
            for (_, statements) in cases {
                for statement in statements {
                    collect_var_names(statement, names);
                }
            }
        }
        Statement::While { body, .. }
        | Statement::Labeled { body, .. }
        | Statement::ForInExpr { body, .. }
        | Statement::DoWhile { body, .. } => collect_var_names(body, names),
        Statement::For {
            initializer, body, ..
        } => {
            if let Some(initializer) = initializer {
                collect_var_names(initializer, names);
            }
            collect_var_names(body, names);
        }
        Statement::ForIn {
            kind, name, body, ..
        } => {
            if *kind == VariableKind::Var {
                names.insert(name.clone());
            }
            collect_var_names(body, names);
        }
        Statement::ForOf {
            kind, name, body, ..
        } => {
            if *kind == VariableKind::Var {
                names.insert(name.clone());
            }
            collect_var_names(body, names);
        }
        Statement::Block(statements) => {
            for statement in statements {
                collect_var_names(statement, names);
            }
        }
        Statement::Try {
            body,
            catch,
            finally,
        } => {
            for statement in body {
                collect_var_names(statement, names);
            }
            if let Some(catch) = catch {
                for statement in &catch.body {
                    collect_var_names(statement, names);
                }
            }
            if let Some(finally) = finally {
                for statement in finally {
                    collect_var_names(statement, names);
                }
            }
        }
        Statement::Function { .. }
        | Statement::Variable { .. }
        | Statement::VariableList { .. }
        | Statement::Return(_)
        | Statement::Throw(_)
        | Statement::Break(_)
        | Statement::Continue(_)
        | Statement::Expression(_) => {}
    }
}

#[derive(Debug)]
enum JsonNode {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<JsonNode>),
    Object(Vec<(String, JsonNode)>),
}

struct JsonParser<'a> {
    input: &'a [u8],
    position: usize,
}

impl<'a> JsonParser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input: input.as_bytes(),
            position: 0,
        }
    }

    fn parse(mut self) -> Result<JsonNode, String> {
        let value = self.value(0)?;
        self.whitespace();
        if self.position != self.input.len() {
            return Err("unexpected trailing JSON input".to_owned());
        }
        Ok(value)
    }

    fn value(&mut self, depth: usize) -> Result<JsonNode, String> {
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

    fn array(&mut self, depth: usize) -> Result<JsonNode, String> {
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

    fn object(&mut self, depth: usize) -> Result<JsonNode, String> {
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

    fn number(&mut self) -> Result<JsonNode, String> {
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

    fn string(&mut self) -> Result<String, String> {
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

    fn literal(&mut self, literal: &[u8]) -> Result<(), String> {
        if self.input.get(self.position..self.position + literal.len()) == Some(literal) {
            self.position += literal.len();
            Ok(())
        } else {
            Err(self.error("invalid JSON literal"))
        }
    }
    fn whitespace(&mut self) {
        while self
            .peek()
            .is_some_and(|byte| matches!(byte, b' ' | b'\n' | b'\r' | b'\t'))
        {
            self.position += 1;
        }
    }
    fn consume(&mut self, expected: u8) -> bool {
        if self.peek() == Some(expected) {
            self.position += 1;
            true
        } else {
            false
        }
    }
    fn peek(&self) -> Option<u8> {
        self.input.get(self.position).copied()
    }
    fn error(&self, message: &str) -> String {
        format!("{message} at byte {}", self.position)
    }
}

fn json_quote(value: &str) -> String {
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

#[allow(clippy::cast_possible_wrap)]
fn days_from_civil(year: i32, month: i32, day: i32) -> i64 {
    let year = i64::from(year) - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

impl JsValue {
    fn is_truthy(&self) -> bool {
        match self {
            Self::Undefined | Self::Null => false,
            Self::Boolean(value) => *value,
            Self::Number(value) => *value != 0.0 && !value.is_nan(),
            Self::String(value) => !value.is_empty(),
            Self::Object(_) => true,
        }
    }
}

/// Coerce an optional argument into a character index (negative counts from
/// the end, matching `String.prototype` slice semantics for the callers that
/// need it; `charAt`-style callers pass the raw value through).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "array indices stay far below any precision boundary"
)]
fn optional_index(value: Option<&JsValue>) -> Result<f64, JsError> {
    match value {
        None | Some(JsValue::Undefined) => Ok(0.0),
        Some(other) => to_number(other),
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "positions are validated against the string length first"
)]
fn valid_position(position: f64, length: usize) -> Option<usize> {
    if !position.is_finite() {
        return None;
    }
    let position = position.floor();
    if position < 0.0 {
        return None;
    }
    #[allow(clippy::cast_precision_loss, reason = "length fits exactly")]
    let length = length as f64;
    if position >= length {
        return None;
    }
    Some(position as usize)
}

fn char_at_value(characters: &[char], position: f64) -> JsValue {
    match valid_position(position, characters.len()) {
        Some(position) => JsValue::String(characters[position].to_string()),
        None => JsValue::String(String::new()),
    }
}

/// Resolve a slice/substring range: negative bounds count from the end and
/// are clamped. When `swap` is set (slice semantics) reversed bounds clamp to
/// an empty range; otherwise they are swapped (substring semantics).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "bounds are clamped before casting to usize"
)]
fn slice_range(
    characters: &[char],
    start: f64,
    end: Option<f64>,
    swap: bool,
) -> std::ops::Range<usize> {
    let length = characters.len();
    let resolve = |value: f64| -> usize {
        if !value.is_finite() {
            return usize::MAX;
        }
        let mut value = value.floor();
        #[allow(clippy::cast_precision_loss, reason = "length fits exactly")]
        let length = length as f64;
        if value < 0.0 {
            value += length;
        }
        value.max(0.0).min(length) as usize
    };
    let mut start = resolve(start);
    let mut end = resolve(end.unwrap_or(if swap { f64::INFINITY } else { f64::MAX }));
    if end == usize::MAX {
        end = length;
    }
    if swap && end < start {
        return start..start;
    }
    if !swap && end < start {
        std::mem::swap(&mut start, &mut end);
    }
    start..end.min(length)
}

/// Collect spans of every non-overlapping match honouring empty-match
/// advancement; used by global matching.
fn collect_global_matches(
    compiled: &super::regex::Compiled,
    input: &[char],
) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut cursor = 0usize;
    while cursor <= input.len() {
        let Some(found) = compiled.find(input, cursor) else {
            break;
        };
        spans.push((found.start, found.end));
        cursor = if found.end == found.start {
            found.end + 1
        } else {
            found.end
        };
    }
    spans
}

/// Split `input` around each match of `compiled`, returning piece spans.
fn split_by_regex(
    compiled: &super::regex::Compiled,
    input: &[char],
    limit: usize,
) -> Vec<(usize, usize)> {
    let mut pieces = Vec::new();
    let mut cursor = 0usize;
    while pieces.len() < limit && cursor <= input.len() {
        match compiled.find(input, cursor) {
            Some(found) => {
                pieces.push((cursor, found.start));
                cursor = if found.end == found.start {
                    found.end + 1
                } else {
                    found.end
                };
                if pieces.len() >= limit {
                    break;
                }
            }
            None => break,
        }
    }
    if pieces.len() < limit {
        pieces.push((cursor.min(input.len()), input.len()));
    }
    pieces
}

/// Expand `$&`, `` $` ``, `$'`, `$$`, and `$1`–`$9` in a replacement string.
fn expand_replacement(
    replacement: &str,
    input: &[char],
    found: &super::regex::MatchRanges,
) -> String {
    let characters: Vec<char> = replacement.chars().collect();
    let mut output = String::new();
    let mut index = 0usize;
    while index < characters.len() {
        let character = characters[index];
        if character != '$' || index + 1 >= characters.len() {
            output.push(character);
            index += 1;
            continue;
        }
        let next = characters[index + 1];
        match next {
            '$' => {
                output.push('$');
                index += 2;
            }
            '&' => {
                output.extend(&input[found.start..found.end]);
                index += 2;
            }
            '`' => {
                output.extend(&input[..found.start]);
                index += 2;
            }
            '\'' => {
                output.extend(&input[found.end.min(input.len())..]);
                index += 2;
            }
            digit @ '1'..='9' => {
                let group = digit as usize - '1' as usize;
                index += 2;
                if let Some(Some((start, end))) = found.groups.get(group) {
                    output.extend(&input[*start..*end]);
                }
            }
            other => {
                output.push('$');
                output.push(other);
                index += 2;
            }
        }
    }
    output
}

/// Map a `String.prototype` method name to its native implementation.
fn string_method_native(name: &str) -> Option<NativeFunction> {
    match name {
        "charAt" => Some(NativeFunction::StrCharAt),
        "charCodeAt" => Some(NativeFunction::StrCharCodeAt),
        "indexOf" => Some(NativeFunction::StrIndexOf),
        "lastIndexOf" => Some(NativeFunction::StrLastIndexOf),
        "includes" => Some(NativeFunction::StrIncludes),
        "startsWith" => Some(NativeFunction::StrStartsWith),
        "endsWith" => Some(NativeFunction::StrEndsWith),
        "slice" => Some(NativeFunction::StrSlice),
        "substring" => Some(NativeFunction::StrSubstring),
        "toLowerCase" => Some(NativeFunction::StrToLowerCase),
        "toUpperCase" => Some(NativeFunction::StrToUpperCase),
        "trim" => Some(NativeFunction::StrTrim),
        "split" => Some(NativeFunction::StrSplit),
        "replace" => Some(NativeFunction::StrReplace),
        "match" => Some(NativeFunction::StrMatch),
        "search" => Some(NativeFunction::StrSearch),
        "concat" => Some(NativeFunction::StrConcat),
        "toString" | "valueOf" => Some(NativeFunction::StrToString),
        _ => None,
    }
}

/// Whether the native is a `String.prototype` method whose receiver may be a
/// primitive string that needs a transient wrapper.
#[allow(dead_code, reason = "retained for potential future use")]
fn is_string_native(function: NativeFunction) -> bool {
    matches!(
        function,
        NativeFunction::StrCharAt
            | NativeFunction::StrCharCodeAt
            | NativeFunction::StrIndexOf
            | NativeFunction::StrLastIndexOf
            | NativeFunction::StrIncludes
            | NativeFunction::StrStartsWith
            | NativeFunction::StrEndsWith
            | NativeFunction::StrSlice
            | NativeFunction::StrSubstring
            | NativeFunction::StrToLowerCase
            | NativeFunction::StrToUpperCase
            | NativeFunction::StrTrim
            | NativeFunction::StrSplit
            | NativeFunction::StrReplace
            | NativeFunction::StrMatch
            | NativeFunction::StrSearch
            | NativeFunction::StrConcat
            | NativeFunction::StrToString
    )
}

fn js_math_round(value: f64) -> f64 {
    if value.is_nan() || value.is_infinite() || value == 0.0 {
        return value;
    }
    (value + 0.5).floor()
}

fn to_number(value: &JsValue) -> Result<f64, JsError> {
    match value {
        JsValue::Undefined => Ok(f64::NAN),
        JsValue::Null => Ok(0.0),
        JsValue::Boolean(value) => Ok(u8::from(*value).into()),
        JsValue::Number(value) => Ok(*value),
        JsValue::String(value) => {
            let value = value.trim();
            if value.is_empty() {
                Ok(0.0)
            } else {
                let radix_value = [
                    ("0x", 16),
                    ("0X", 16),
                    ("0b", 2),
                    ("0B", 2),
                    ("0o", 8),
                    ("0O", 8),
                ]
                .into_iter()
                .find_map(|(prefix, radix)| {
                    value.strip_prefix(prefix).map(|digits| {
                        u64::from_str_radix(digits, radix).map_or(f64::NAN, |number| number as f64)
                    })
                });
                Ok(radix_value.unwrap_or_else(|| value.parse().unwrap_or(f64::NAN)))
            }
        }
        JsValue::Object(_) => Err(JsError::type_error(
            "object-to-primitive numeric conversion is not implemented",
        )),
    }
}

fn format_number_precision(value: f64, precision: usize) -> String {
    if !value.is_finite() {
        return super::value::number_to_string(value);
    }
    if value == 0.0 {
        return if precision == 1 {
            "0".to_owned()
        } else {
            format!("0.{:0<width$}", "", width = precision - 1)
        };
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "finite binary64 base-10 exponents fit comfortably in i32"
    )]
    let exponent = value.abs().log10().floor() as i32;
    if exponent < -6 || exponent >= precision as i32 {
        let mut formatted = format!("{:.*e}", precision - 1, value);
        if let Some(marker) = formatted.find('e') {
            let raw_exponent = formatted[marker + 1..].parse::<i32>().unwrap_or(exponent);
            formatted.truncate(marker + 1);
            if raw_exponent >= 0 {
                formatted.push('+');
            }
            formatted.push_str(&raw_exponent.to_string());
        }
        formatted
    } else {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "precision is at most 100 and exponent selects fixed notation"
        )]
        let decimals = (precision as i32 - exponent - 1) as usize;
        format!("{value:.decimals$}")
    }
}

fn array_index(property: &str) -> Option<u32> {
    let index = property.parse::<u32>().ok()?;
    (index.to_string() == property && index < u32::MAX).then_some(index)
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn to_uint32(value: &JsValue) -> Result<u32, JsError> {
    let number = to_number(value)?;
    if !number.is_finite() || number == 0.0 {
        return Ok(0);
    }
    let integer = number.trunc();
    let modulo = integer.rem_euclid(4_294_967_296.0);
    Ok(modulo as u32)
}

#[allow(clippy::cast_possible_wrap)]
fn to_int32(value: &JsValue) -> Result<i32, JsError> {
    Ok(to_uint32(value)? as i32)
}

fn bitwise_binary(
    left: &JsValue,
    right: &JsValue,
    operation: impl FnOnce(i32, i32) -> i32,
) -> Result<JsValue, JsError> {
    Ok(JsValue::Number(f64::from(operation(
        to_int32(left)?,
        to_int32(right)?,
    ))))
}

fn shift_count(value: &JsValue) -> Result<u32, JsError> {
    Ok(to_uint32(value)? & 0x1f)
}

fn shift_left(left: &JsValue, right: &JsValue) -> Result<JsValue, JsError> {
    Ok(JsValue::Number(f64::from(
        to_int32(left)?.wrapping_shl(shift_count(right)?),
    )))
}

fn shift_right(left: &JsValue, right: &JsValue) -> Result<JsValue, JsError> {
    Ok(JsValue::Number(f64::from(
        to_int32(left)? >> shift_count(right)?,
    )))
}

fn unsigned_shift_right(left: &JsValue, right: &JsValue) -> Result<JsValue, JsError> {
    Ok(JsValue::Number(f64::from(
        to_uint32(left)? >> shift_count(right)?,
    )))
}

fn compare(
    left: &JsValue,
    right: &JsValue,
    numeric: impl FnOnce(f64, f64) -> bool,
    string: impl FnOnce(&str, &str) -> bool,
) -> Result<JsValue, JsError> {
    if let (JsValue::String(left), JsValue::String(right)) = (left, right) {
        return Ok(JsValue::Boolean(string(left, right)));
    }
    Ok(JsValue::Boolean(numeric(
        to_number(left)?,
        to_number(right)?,
    )))
}

fn strict_equal(left: &JsValue, right: &JsValue) -> bool {
    match (left, right) {
        (JsValue::Undefined, JsValue::Undefined) | (JsValue::Null, JsValue::Null) => true,
        (JsValue::Boolean(left), JsValue::Boolean(right)) => left == right,
        (JsValue::Number(left), JsValue::Number(right)) => number_equal(*left, *right),
        (JsValue::String(left), JsValue::String(right)) => left == right,
        (JsValue::Object(left), JsValue::Object(right)) => left == right,
        _ => false,
    }
}

#[allow(clippy::float_cmp)]
fn number_equal(left: f64, right: f64) -> bool {
    // ECMAScript Number equality is exact IEEE-754 equality: NaN differs from
    // every value, while +0 and -0 compare equal. An epsilon comparison would
    // implement different language semantics.
    left == right
}

fn same_value_zero(left: &JsValue, right: &JsValue) -> bool {
    match (left, right) {
        (JsValue::Number(left), JsValue::Number(right)) => {
            (left.is_nan() && right.is_nan()) || left == right
        }
        _ => left == right,
    }
}

fn dom_contains(dom: &Dom, root: NodeId, candidate: NodeId) -> bool {
    let mut current = Some(candidate);
    while let Some(node) = current {
        if node == root {
            return true;
        }
        current = dom.parent(node);
    }
    false
}

fn find_body_node(dom: &Dom, root: NodeId) -> Option<NodeId> {
    for child in dom.children(root).unwrap_or_default() {
        if matches!(
            dom.node(*child).map(crate::dom::Node::kind),
            Some(NodeKind::Element(element))
                if element.namespace == crate::dom::Namespace::Html
                    && element.local_name == "body"
        ) {
            return Some(*child);
        }
        if let Some(found) = find_body_node(dom, *child) {
            return Some(found);
        }
    }
    None
}

/// Interpret a timer id argument; non-integral or out-of-range values match no
/// timer, mirroring the platform's lenient `clearTimeout` behavior.
fn optional_timer_id(value: &JsValue) -> Option<u64> {
    match value {
        JsValue::Number(number)
            if number.is_finite() && number.fract() == 0.0 && *number >= 1.0 =>
        {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "guarded by the finite integral range check above"
            )]
            {
                Some(*number as u64)
            }
        }
        _ => None,
    }
}

/// Members of the `CSSStyleDeclaration` interface that are methods rather
/// than camelCase mirrors of CSS properties; they must never be treated as
/// inline declarations.
const STYLE_METHOD_PROPERTIES: [&str; 4] =
    ["getPropertyValue", "setProperty", "removeProperty", "item"];

fn is_valid_property_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
        && name
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == '-')
}

/// Map a camelCase style member (`backgroundColor`) to its CSS property name.
fn css_prop_from_member(property: &str) -> String {
    let mut mapped = String::with_capacity(property.len() + 4);
    for character in property.chars() {
        if character.is_ascii_uppercase() {
            mapped.push('-');
            mapped.push(character.to_ascii_lowercase());
        } else {
            mapped.push(character);
        }
    }
    mapped
}

fn node_attribute_property(property: &str) -> Option<&str> {
    match property {
        "id" => Some("id"),
        "className" => Some("class"),
        "value" => Some("value"),
        "name" => Some("name"),
        "title" => Some("title"),
        "href" => Some("href"),
        "src" => Some("src"),
        "srcset" | "srcSet" => Some("srcset"),
        "sizes" => Some("sizes"),
        "poster" => Some("poster"),
        "loading" => Some("loading"),
        "width" => Some("width"),
        "height" => Some("height"),
        "alt" => Some("alt"),
        "role" => Some("role"),
        "type" => Some("type"),
        "placeholder" => Some("placeholder"),
        "action" => Some("action"),
        "method" => Some("method"),
        "target" => Some("target"),
        "rel" => Some("rel"),
        "tabIndex" => Some("tabindex"),
        "disabled" => Some("disabled"),
        "checked" => Some("checked"),
        "selected" => Some("selected"),
        "hidden" => Some("hidden"),
        "readOnly" => Some("readonly"),
        "required" => Some("required"),
        "multiple" => Some("multiple"),
        "autofocus" | "autoFocus" => Some("autofocus"),
        _ => None,
    }
}

fn node_boolean_property(property: &str) -> bool {
    matches!(
        property,
        "disabled"
            | "checked"
            | "selected"
            | "hidden"
            | "readOnly"
            | "required"
            | "multiple"
            | "autofocus"
            | "autoFocus"
    )
}

fn abstract_equal(left: &JsValue, right: &JsValue) -> Result<bool, JsError> {
    if strict_equal(left, right) {
        return Ok(true);
    }
    if matches!(
        (left, right),
        (JsValue::Null, JsValue::Undefined) | (JsValue::Undefined, JsValue::Null)
    ) {
        return Ok(true);
    }
    match (left, right) {
        (JsValue::Number(left), JsValue::String(_)) => Ok(number_equal(*left, to_number(right)?)),
        (JsValue::String(_), JsValue::Number(right)) => Ok(number_equal(to_number(left)?, *right)),
        (JsValue::Boolean(_), _) => abstract_equal(&JsValue::Number(to_number(left)?), right),
        (_, JsValue::Boolean(_)) => abstract_equal(left, &JsValue::Number(to_number(right)?)),
        _ => Ok(false),
    }
}

/// `DocumentPosition` bitmask values for `compareDocumentPosition`.
const DOCUMENT_POSITION_DISCONNECTED: f64 = 1.0;
const DOCUMENT_POSITION_PRECEDING: f64 = 2.0;
const DOCUMENT_POSITION_FOLLOWING: f64 = 4.0;
const DOCUMENT_POSITION_CONTAINS: f64 = 8.0;
const DOCUMENT_POSITION_CONTAINED_BY: f64 = 16.0;

/// Weekday and month names for the minimal UTC date formatter.
const DATE_WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
const DATE_MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Source shape of cloned nodes in `clone_node_recursive`.
enum CloneSource {
    Element {
        local_name: String,
        attributes: Vec<(String, String)>,
    },
    Text(String),
    Comment(String),
    Fragment,
}

fn required_argument<'a>(
    arguments: &'a [JsValue],
    index: usize,
    function: &str,
) -> Result<&'a JsValue, JsError> {
    arguments.get(index).ok_or_else(|| {
        JsError::type_error(format!(
            "{function} requires at least {} argument(s)",
            index.saturating_add(1)
        ))
    })
}

fn valid_html_local_name(name: &str) -> bool {
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic() || matches!(character, '_' | ':'))
        && characters.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | ':' | '-' | '.')
        })
}

impl From<DomError> for JsError {
    fn from(error: DomError) -> Self {
        Self::new(JsErrorKind::Dom, error.to_string(), None)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{ElementRect, JsRuntime};
    use crate::html::parse_document;
    use crate::js::JsValue;
    use url::Url;

    #[test]
    fn location_exposes_normalized_committed_url_components() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let url =
            Url::parse("https://user:pass@example.test:8443/a/b?q=rust#part").expect("test URL");
        let mut runtime = JsRuntime::with_url(&parsed.dom, &url);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r##"
                    window === self && self === globalThis && globalThis === window &&
                    window.location === location && document.location === location &&
                    location.href === "https://user:pass@example.test:8443/a/b?q=rust#part" &&
                    location.origin === "https://example.test:8443" &&
                    location.protocol === "https:" && location.host === "example.test:8443" &&
                    location.hostname === "example.test" && location.port === "8443" &&
                    location.pathname === "/a/b" && location.search === "?q=rust" &&
                    location.hash === "#part" && location.toString() === location.href;
                "##,
            )
            .expect("Location reads should execute");
        assert_eq!(outcome.value, JsValue::Boolean(true));
    }

    #[test]
    fn array_some_short_circuits_on_the_first_matching_callback() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                "var calls = 0; var matched = [1, 2, 3].some(function(value) { calls += 1; return value === 2; }); matched && calls === 2;",
            )
            .expect("Array.prototype.some should execute");
        assert_eq!(outcome.value, JsValue::Boolean(true));
    }

    #[test]
    fn location_href_writes_and_assign_replace_queue_navigations() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let url = Url::parse("https://example.test/current").expect("test URL");
        let mut runtime = JsRuntime::with_url(&parsed.dom, &url);
        runtime
            .execute(
                &mut parsed.dom,
                r#"
                    location.href = "/next";
                    location.assign("https://other.test/a");
                    location.replace("/swap");
                "#,
            )
            .expect("Location navigation writes should execute");
        assert_eq!(
            runtime.take_pending_navigations(),
            vec![
                super::NavigationRequest {
                    url: "https://example.test/next".to_owned(),
                    replace: false
                },
                super::NavigationRequest {
                    url: "https://other.test/a".to_owned(),
                    replace: false
                },
                super::NavigationRequest {
                    url: "https://example.test/swap".to_owned(),
                    replace: true
                },
            ]
        );
        assert!(runtime.take_pending_navigations().is_empty());
    }

    #[test]
    fn location_non_href_writes_still_fail_instead_of_faking_navigation() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let url = Url::parse("https://example.test/current").expect("test URL");
        let mut runtime = JsRuntime::with_url(&parsed.dom, &url);
        let error = runtime
            .execute(&mut parsed.dom, "location.pathname = '/next';")
            .expect_err("unsupported navigation must be explicit");
        assert_eq!(error.kind(), crate::js::JsErrorKind::Type);
        assert!(error.message().contains("embedding browser"));
    }

    #[test]
    fn navigator_exposes_browser_bootstrap_properties() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"navigator.appName === "Netscape" &&
                    navigator.userAgent === "Mozilla/5.0 rENDER/0.1" &&
                    navigator.language === "zh-CN" &&
                    navigator.cookieEnabled && navigator.onLine;"#,
            )
            .expect("navigator feature detection should execute");

        assert_eq!(outcome.value, JsValue::Boolean(true));
    }

    #[test]
    fn event_target_registers_removes_and_cancels_listeners() {
        let mut parsed = parse_document("<!doctype html><button id='button'>go</button>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    var button = document.getElementById("button");
                    var calls = 0;
                    function listener(event) {
                        calls = calls + 1;
                        event.preventDefault();
                    }
                    button.addEventListener("activate", listener);
                    button.addEventListener("activate", listener);
                    var event = new Event("activate", { cancelable: true });
                    var accepted = button.dispatchEvent(event);
                    button.removeEventListener("activate", listener);
                    button.dispatchEvent(new Event("activate"));
                    calls + ":" + accepted + ":" + event.defaultPrevented;
                "#,
            )
            .expect("EventTarget methods should dispatch a cancelable event");
        assert_eq!(outcome.value, JsValue::String("1:false:true".to_owned()));
    }

    #[test]
    fn bubbling_event_exposes_target_current_target_and_listener_this() {
        let mut parsed =
            parse_document("<!doctype html><div id='parent'><button id='child'>go</button></div>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    var parent = document.getElementById("parent");
                    var child = document.getElementById("child");
                    var observed = false;
                    function listener(event) {
                        observed = event.target === child &&
                            event.currentTarget === parent && this === parent;
                    }
                    parent.addEventListener("activate", listener);
                    var event = new Event("activate", { bubbles: true });
                    child.dispatchEvent(event);
                    observed && event.currentTarget === null;
                "#,
            )
            .expect("bubbling should traverse DOM ancestors");
        assert_eq!(outcome.value, JsValue::Boolean(true));
    }

    #[test]
    fn object_reflection_and_assignment_cover_common_runtime_usage() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    var source = { b: 2, a: 1 };
                    var target = Object.assign({}, source);
                    var descriptor = Object.getOwnPropertyDescriptor(target, "a");
                    Object.defineProperty(target, "hidden", { value: 9, enumerable: false, writable: false, configurable: false });
                    Object.keys(target)[0] + Object.keys(target)[1] + Object.values(target)[0] + Object.values(target)[1] + Object.hasOwn(target, "hidden") + descriptor.value;
                "#,
            )
            .expect("Object builtins should execute");
        assert_eq!(outcome.value, JsValue::String("ab12true1".to_owned()));
    }

    #[test]
    fn object_create_and_constructor_preserve_prototypes() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r"
                    var prototype = { answer: 42 };
                    var child = Object.create(prototype);
                    var wrapped = { value: 7 };
                    Object.getPrototypeOf(child) === prototype && child.answer === 42 && Object(wrapped) === wrapped;
                ",
            )
            .expect("Object.create and Object() should execute");
        assert_eq!(outcome.value, JsValue::Boolean(true));
    }

    #[test]
    fn object_prototype_methods_cover_ordinary_arrays_and_null_prototypes() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    var ordinary = { visible: 1 };
                    var dictionary = Object.create(null);
                    Object.getPrototypeOf(ordinary) === Object.prototype &&
                        ordinary.hasOwnProperty("visible") &&
                        !ordinary.hasOwnProperty("toString") &&
                        ordinary.propertyIsEnumerable("visible") &&
                        !ordinary.propertyIsEnumerable("hasOwnProperty") &&
                        Object.prototype.isPrototypeOf(ordinary) &&
                        Object.prototype.isPrototypeOf([]) &&
                        Array.hasOwnProperty("prototype") &&
                        dictionary.hasOwnProperty === undefined;
                "#,
            )
            .expect("Object.prototype methods should execute");
        assert_eq!(outcome.value, JsValue::Boolean(true));
    }

    #[test]
    fn array_constructor_and_length_follow_common_ecmascript_semantics() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    var empty = new Array();
                    var sized = Array(3);
                    var one = Array("x");
                    var many = new Array(1, 2, 3);
                    sized[4] = 5;
                    sized.length = 2;
                    var unshifted = many.unshift(0);
                    empty.length + ":" + sized.length + ":" +
                        (sized[4] === undefined) + ":" + one[0] + ":" +
                        many.join(",") + ":" + unshifted + ":" + Array.isArray(many);
                "#,
            )
            .expect("Array constructor and length semantics should execute");
        assert_eq!(
            outcome.value,
            JsValue::String("0:2:true:x:0,1,2,3:4:true".to_owned())
        );
    }

    #[test]
    fn primitive_prototypes_and_native_function_call_are_wired() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    [
                        8e-5.toFixed(3),
                        1..toPrecision(void 0),
                        (1.2).toPrecision(3),
                        Number.prototype.constructor === Number,
                        Boolean.prototype.constructor === Boolean,
                        Array.prototype.slice.call({0: "ok", length: 1}, 0)[0],
                        (function(floor) { return floor(1.9); })(Math.floor)
                    ].join("|");
                "#,
            )
            .expect("primitive methods and native call inheritance should execute");
        assert_eq!(
            outcome.value,
            JsValue::String("0.000|1|1.20|true|true|ok|1".to_owned())
        );
    }

    #[test]
    fn dom_interfaces_and_keyed_collections_cover_site_bootstrap_usage() {
        let mut parsed = parse_document("<!doctype html><main id='app'></main>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    var key = {};
                    var map = new Map([[key, 1], ["x", 2]]);
                    map.set(NaN, 3);
                    var total = 0;
                    map.forEach(function(value) { total += value; });
                    var first = map.entries().next();
                    var set = new Set([1, 2, 2]);
                    var weak = new WeakMap([[key, "ok"]]);
                    var element = document.createElement("section");
                    [
                        typeof Element,
                        element instanceof Element,
                        Element.prototype.matches === element.matches,
                        map.size, map.get(key), map.has(NaN), total,
                        first.done, first.value[1], set.size, weak.get(key)
                    ].join("|");
                "#,
            )
            .expect("DOM constructors and keyed collections should execute");
        assert_eq!(
            outcome.value,
            JsValue::String("function|true|true|3|1|true|6|false|1|2|ok".to_owned())
        );
    }

    #[test]
    fn template_literals_and_surrogate_pairs_execute() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    var site = "QQ";
                    `${site}-${1 + 2}-\uD83D\uDE00`;
                "#,
            )
            .expect("template interpolation should execute");
        assert_eq!(outcome.value, JsValue::String("QQ-3-😀".to_owned()));
    }

    #[test]
    fn array_length_reads_use_to_length_for_generic_array_methods() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    var object = { 0: "a", length: "1.9" };
                    Array.prototype.join.call(object, "-");
                "#,
            )
            .expect("generic array methods should normalize length");
        assert_eq!(outcome.value, JsValue::String("a".to_owned()));
    }

    #[test]
    fn for_in_enumerates_prototypes_and_delete_honors_descriptors() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    var prototype = { inherited: 1 };
                    var object = Object.create(prototype);
                    object.own = 2;
                    Object.defineProperty(object, "hidden", { value: 3, enumerable: false });
                    Object.defineProperty(object, "fixed", { value: 4, enumerable: true, configurable: false });
                    var names = "";
                    for (var name in object) { names = names + name + ","; }
                    var removed = delete object.own;
                    var retained = delete object.fixed;
                    names + removed + "," + retained + "," + object.own + "," + object.fixed;
                "#,
            )
            .expect("for-in and delete should execute");
        assert_eq!(
            outcome.value,
            JsValue::String("fixed,own,inherited,true,false,undefined,4".to_owned())
        );
    }

    #[test]
    fn function_call_bind_and_typeof_share_callable_semantics() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    function add(left, right) { return this.base + left + right; }
                    var bound = add.bind({ base: 10 }, 2);
                    typeof Function === "function" &&
                        typeof Function.prototype === "function" &&
                        typeof bound === "function" &&
                        add.call({ base: 1 }, 3, 4) === 8 &&
                        bound(5) === 17;
                "#,
            )
            .expect("Function.prototype call and bind should execute");
        assert_eq!(outcome.value, JsValue::Boolean(true));
    }

    #[test]
    fn property_helper_primordials_can_be_uncurried() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    var join = Function.prototype.call.bind(Array.prototype.join);
                    var push = Function.prototype.call.bind(Array.prototype.push);
                    var values = ["a"];
                    push(values, "b", "c");
                    var target = { visible: 1 };
                    Object.defineProperty(target, "hidden", { value: 2, enumerable: false });
                    join(values, ";") + ":" +
                        join(Object.getOwnPropertyNames(target), ",") + ":" +
                        Math.pow(2, 5);
                "#,
            )
            .expect("propertyHelper primordial operations should execute");
        assert_eq!(
            outcome.value,
            JsValue::String("a;b;c:hidden,visible:32".to_owned())
        );
    }

    #[test]
    fn user_functions_expose_arguments_and_string_conversion() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    function inspect(value) {
                        var first = () => arguments[0];
                        return arguments.length + ":" + String(first()) + ":" + typeof String;
                    }
                    inspect(42, "extra");
                "#,
            )
            .expect("ordinary functions should expose arguments");
        assert_eq!(outcome.value, JsValue::String("2:42:function".to_owned()));
    }

    #[test]
    fn native_error_constructors_share_the_error_prototype_contract() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    var error = new TypeError("bad input");
                    var called = RangeError("out of range");
                    typeof Error + ":" +
                        (error instanceof TypeError) + ":" +
                        (error instanceof Error) + ":" +
                        (called instanceof RangeError) + ":" +
                        error.name + ":" + error.message + ":" + error.toString();
                "#,
            )
            .expect("native Error constructors should call and construct");

        assert_eq!(
            outcome.value,
            JsValue::String(
                "function:true:true:true:TypeError:bad input:TypeError: bad input".to_owned()
            )
        );
    }

    #[test]
    fn native_errors_have_distinct_prototypes_and_optional_messages() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    var empty = new Error();
                    var syntax = new SyntaxError("parse");
                    Object.getPrototypeOf(empty) === Error.prototype &&
                        Object.getPrototypeOf(syntax) === SyntaxError.prototype &&
                        Object.getPrototypeOf(SyntaxError.prototype) === Error.prototype &&
                        !empty.hasOwnProperty("message") &&
                        syntax.propertyIsEnumerable("message") === false &&
                        empty.toString() === "Error";
                "#,
            )
            .expect("native Error prototypes and descriptors should execute");

        assert_eq!(outcome.value, JsValue::Boolean(true));
    }

    #[test]
    fn bitwise_operators_follow_ecmascript_precedence_and_int32_semantics() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    var flags = 6;
                    var jqueryToggle = flags ^ 1;
                    var precedence = 1 | 2 ^ 3 & 1;
                    var shifts = (1 << 31) + ":" + (-1 >> 1) + ":" + (-1 >>> 0);
                    var coercion = (NaN | 0) + ":" + (Infinity & 7) + ":" + (~0);
                    jqueryToggle + ":" + precedence + ":" + shifts + ":" + coercion;
                "#,
            )
            .expect("site-style bitwise expressions should execute");

        assert_eq!(
            outcome.value,
            JsValue::String("7:3:-2147483648:-1:4294967295:0:0:-1".to_owned())
        );
    }

    #[test]
    fn bitwise_compound_assignment_evaluates_member_reference_once() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    var calls = 0;
                    var values = [6];
                    function index() { calls += 1; return 0; }
                    values[index()] ^= 3;
                    values[index()] <<= 2;
                    values[index()] >>>= 1;
                    values[0] + ":" + calls;
                "#,
            )
            .expect("compound bitwise assignment should preserve a single reference evaluation");

        assert_eq!(outcome.value, JsValue::String("10:3".to_owned()));
    }

    #[test]
    fn set_timeout_registers_timer_and_requests_scheduling() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(&mut parsed.dom, "setTimeout(function () {}, 250);")
            .expect("setTimeout should execute");
        let id = match outcome.value {
            JsValue::Number(id) => id,
            other => panic!("setTimeout should return a numeric id, got {other:?}"),
        };
        assert!((id - 1.0).abs() < f64::EPSILON, "unexpected timer id {id}");
        let requests = runtime.take_pending_timer_requests();
        assert_eq!(
            requests,
            vec![super::TimerRequest::Schedule {
                id: 1,
                delay_ms: 250.0
            }]
        );
    }

    #[test]
    fn fire_timer_invokes_callback_once_and_clear_removes_it() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        runtime
            .execute(
                &mut parsed.dom,
                r"
                    var hits = 0;
                    setTimeout(function () { hits += 1; }, 0);
                    var interval = setInterval(function () { hits += 10; }, 5);
                    clearInterval(interval);
                ",
            )
            .expect("timer registration should succeed");
        let _ = runtime.take_pending_timer_requests();

        // The timeout fires once; the cancelled interval stays silent.
        let rearm = runtime
            .fire_timer(&mut parsed.dom, 1)
            .expect("firing a registered timer succeeds");
        assert_eq!(rearm, None);
        assert!(runtime.take_pending_timer_requests().is_empty());

        runtime
            .execute(&mut parsed.dom, "hits")
            .map(|outcome| assert_eq!(outcome.value, JsValue::Number(1.0)))
            .expect("reading hits should work");
        assert!(runtime.fire_timer(&mut parsed.dom, 99).is_ok());
        assert_eq!(
            super::ConsoleLevel::Log.label(),
            "log",
            "sanity check the level labels stay importable"
        );
    }

    #[test]
    fn console_methods_buffer_messages_for_the_embedding() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        runtime
            .execute(
                &mut parsed.dom,
                r#"
                    console.log("hello", 42);
                    console.warn("careful");
                    console.error("boom");
                    console.info("info");
                    console.debug("debug");
                "#,
            )
            .expect("console calls should execute");
        let messages = runtime.take_console_messages();
        let rendered: Vec<_> = messages
            .iter()
            .map(|message| (message.level.label(), message.text.as_str()))
            .collect();
        assert_eq!(
            rendered,
            vec![
                ("log", "hello 42"),
                ("warn", "careful"),
                ("error", "boom"),
                ("info", "info"),
                ("debug", "debug"),
            ]
        );
        assert!(runtime.take_console_messages().is_empty());
    }

    #[test]
    fn request_animation_frame_registers_zero_delay_one_shot() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        runtime
            .execute(&mut parsed.dom, r"requestAnimationFrame(function () {});")
            .expect("rAF registration should succeed");
        let requests = runtime.take_pending_timer_requests();
        assert!(matches!(
            requests.as_slice(),
            [super::TimerRequest::Schedule { delay_ms: 0.0, .. }]
        ));
        let rearm = runtime
            .fire_timer(&mut parsed.dom, 1)
            .expect("animation frame callback fires");
        assert_eq!(rearm, None);
    }

    #[test]
    fn inner_html_round_trips_and_setter_replaces_children() {
        let mut parsed = parse_document("<!doctype html><div id='host'><p>old</p></div>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    var host = document.getElementById("host");
                    var before = host.innerHTML;
                    host.innerHTML = "<b>new</b> text &amp; more<!-- c -->";
                    var after = host.innerHTML;
                    before + "|" + after + "|" + host.children.length;
                "#,
            )
            .expect("innerHTML accessors should execute");
        assert_eq!(
            outcome.value,
            JsValue::String("<p>old</p>|<b>new</b> text &amp; more<!-- c -->|1".to_owned()),
            "children counts only the element child; text and comment are preserved"
        );
    }

    #[test]
    fn inner_html_import_respects_node_creation_limits() {
        let mut parsed = parse_document("<!doctype html><div id='host'></div>");
        let limits = crate::js::RuntimeLimits {
            max_dom_nodes_created: 2,
            ..crate::js::RuntimeLimits::default()
        };
        let mut runtime = JsRuntime::with_limits(&parsed.dom, limits);
        let error = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    var host = document.getElementById("host");
                    host.innerHTML = "<i>a</i><i>b</i><i>c</i>";
                "#,
            )
            .expect_err("importing past the node budget must fail");
        assert_eq!(error.kind(), crate::js::JsErrorKind::ResourceLimit);
    }

    #[test]
    fn style_declaration_reads_writes_and_clears_inline_declarations() {
        let mut parsed = parse_document("<!doctype html><p id='target'>x</p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    var p = document.getElementById("target");
                    p.style.color = "red";
                    p.style.setProperty("background-color", "blue", "important");
                    var color = p.style.color;
                    var viaGet = p.style.getPropertyValue("color");
                    var length = p.style.length;
                    var first = p.style.item(0);
                    var cssText = p.style.cssText;
                    p.style.removeProperty("color");
                    var afterRemove = p.style.getPropertyValue("color");
                    color + ":" + viaGet + ":" + length + ":" + first + ":" +
                        cssText + ":" + afterRemove + ":" +
                        p.getAttribute("style");
                "#,
            )
            .expect("CSSStyleDeclaration operations should execute");
        // Declaration order follows source order; removing `color` leaves only
        // the important background declaration on the element.
        assert_eq!(
            outcome.value,
            JsValue::String(
                "red:red:2:color:color: red; background-color: blue !important;::\
                 background-color: blue !important;"
                    .to_owned()
            ),
            "unexpected inline style state"
        );
    }

    #[test]
    fn get_rect_bounds_returns_zeroes_without_layout() {
        let mut parsed = parse_document("<!doctype html><p id='p'>x</p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    var rect = document.getElementById("p").getBoundingClientRect();
                    rect.width === 0 && rect.height === 0 && rect.top === 0 &&
                        rect.left === 0 && rect.right === 0 && rect.bottom === 0;
                "#,
            )
            .expect("getBoundingClientRect without geometry should return zeros");
        assert_eq!(outcome.value, JsValue::Boolean(true));
    }

    #[test]
    fn get_rect_bounds_reports_installed_geometry() {
        let mut parsed = parse_document("<!doctype html><p id='p'>x</p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let mut geometry = std::collections::BTreeMap::new();
        let body = super::find_body_node(&parsed.dom, parsed.dom.document()).expect("body exists");
        let paragraph = parsed.dom.children(body).unwrap_or_default()[0];
        geometry.insert(
            paragraph.as_u64(),
            super::ElementRect {
                x: 8.0,
                y: 16.0,
                width: 100.0,
                height: 20.0,
            },
        );
        runtime.install_element_geometry(geometry);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    var rect = document.getElementById("p").getBoundingClientRect();
                    rect.x + "," + rect.y + "," + rect.width + "," + rect.height + "," +
                        rect.right + "," + rect.bottom;
                "#,
            )
            .expect("geometry-backed rect should read back");
        assert_eq!(
            outcome.value,
            JsValue::String("8,16,100,20,108,36".to_owned())
        );
    }

    #[test]
    fn dispatch_dom_event_walks_ancestors_and_reports_prevention() {
        let mut parsed =
            parse_document("<!doctype html><div id='parent'><button id='child'>go</button></div>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        runtime
            .execute(
                &mut parsed.dom,
                r#"
                    document.getElementById("parent")
                        .addEventListener("custompress", function (event) {
                            event.preventDefault();
                        });
                "#,
            )
            .expect("listener registration should succeed");
        let child = {
            let body =
                super::find_body_node(&parsed.dom, parsed.dom.document()).expect("body exists");
            let div = parsed.dom.children(body).unwrap_or_default()[0];
            parsed.dom.children(div).unwrap_or_default()[0]
        };
        let allowed = runtime
            .dispatch_dom_event(&mut parsed.dom, child, "custompress", true, true, &[])
            .expect("trusted dispatch should run listeners");
        assert!(!allowed, "preventDefault must cancel the default action");
    }

    #[test]
    fn regex_literals_support_exec_groups_and_flags() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    var re = /(\w+)-(\d+)/i;
                    var first = re.exec("item-42 rest");
                    var flags_ok = re.global === false && re.ignoreCase === true &&
                        re.multiline === false && re.sticky === false;
                    [first[0], first[1], first[2], first.index, first.input, flags_ok].join("|");
                "#,
            )
            .expect("regex literal should execute");
        assert_eq!(
            outcome.value,
            JsValue::String("item-42|item|42|0|item-42 rest|true".to_owned())
        );
    }

    #[test]
    fn regex_global_test_and_sticky_track_last_index() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    var global = /a/g;
                    var sticky = /a/y;
                    sticky.lastIndex = 2;
                    [
                        global.test("banana"), global.test("banana"),
                        global.test("banana"), global.test("banana"), global.test("banana"),
                        sticky.test("banana"), sticky.lastIndex
                    ].join(",");
                "#,
            )
            .expect("regex flag tests should execute");
        assert_eq!(
            outcome.value,
            JsValue::String("true,true,true,false,true,false,0".to_owned())
        );
    }

    #[test]
    fn string_methods_cover_indexing_slicing_and_case() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    var text = "  Hello, World!  ";
                    [
                        text.trim().charAt(4), text.trim().charCodeAt(0),
                        text.indexOf("World") - 2, "abc".lastIndexOf("b"),
                        "hello".toUpperCase(), "WORLD".toLowerCase(),
                        "abcdef".slice(1, 3), "abcdef".slice(-3),
                        "abcdef".substring(4, 2), "abc".concat("def", "ghi")
                    ].join("|");
                "#,
            )
            .expect("string methods should execute");
        assert_eq!(
            outcome.value,
            JsValue::String("o|72|7|1|HELLO|world|bc|def|cd|abcdefghi".to_owned())
        );
    }

    #[test]
    fn string_replace_expands_groups_and_honors_global() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    [
                        "a-b-c".replace(/(\w)-(\w)/, "$2_$1"),
                        "a-b-c".replace(/-/g, "+"),
                        "2026-08-23".replace(/(\d{4})-(\d{2})-(\d{2})/, "$3/$2/$1")
                    ].join("|");
                "#,
            )
            .expect("string replace should execute");
        assert_eq!(
            outcome.value,
            JsValue::String("b_a-c|a+b+c|23/08/2026".to_owned())
        );
    }

    #[test]
    fn string_split_match_search_interoperate_with_regex() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    var matched = "ab12cd34".match(/\d+/g);
                    [
                        "a,b,,c".split(",").length,
                        "one two  three".split(/\s+/).join("/"),
                        matched.length + ":" + matched.join("."),
                        "find the needle".search(/needle/),
                        "nope".search(/zzz/)
                    ].join("|");
                "#,
            )
            .expect("regex-aware string methods should execute");
        assert_eq!(
            outcome.value,
            JsValue::String("4|one/two/three|2:12.34|9|-1".to_owned())
        );
    }

    #[test]
    fn json_and_date_builtins_cover_real_world_bootstrap_scripts() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    var value = JSON.parse('{"name":"rENDER","items":[1,true,null]}');
                    var encoded = JSON.stringify(value);
                    var date = new Date("2026-08-23T12:34:56.789Z");
                    [encoded, date.getTime(), date.getUTCFullYear(), date.getUTCMonth(), date.getUTCDate(),
                     date.getUTCHours(), date.toISOString(), Date.parse("2026-08-23T00:00:00Z")].join("|");
                "#,
            )
            .expect("JSON and Date builtins should execute");
        assert_eq!(
            outcome.value,
            JsValue::String(
                "{\"items\":[1,true,null],\"name\":\"rENDER\"}|1787488496789|2026|7|23|12|2026-08-23T12:34:56.789Z|1787443200000"
                    .to_owned()
            )
        );
    }

    #[test]
    fn modern_binding_spread_computed_and_string_builtins_execute() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    const key = "side";
                    const source = {head: 1, side: 7, tail: 9};
                    const {head: a, ...rest} = source;
                    const [[b], c] = [[2], 3];
                    let x, y; [x, y] = [4, 5];
                    const object = {[key]: 6, ...rest};
                    [String.fromCharCode(65, 66), String.fromCodePoint(128512),
                     a, b, c, x, y, object.side, object.tail, 2 ** 3].join("|");
                    let pairs = "";
                    for (const [name, value] of Object.entries(object)) {
                        pairs += name + value;
                    }
                    [String.fromCharCode(65, 66), String.fromCodePoint(128512),
                     a, b, c, x, y, object.side, object.tail, 2 ** 3, pairs].join("|");
                "#,
            )
            .expect("modern syntax and string constructors should execute");
        assert_eq!(
            outcome.value,
            JsValue::String("AB|😀|1|2|3|4|5|7|9|8|side7tail9".to_owned())
        );
    }

    #[test]
    fn intersection_observer_reports_viewport_entries_and_drives_src_mutation() {
        let mut parsed = parse_document("<!doctype html><img id=lazy data-src=loaded.png>");
        let image = {
            let body = super::find_body_node(&parsed.dom, parsed.dom.document()).unwrap();
            parsed.dom.children(body).unwrap_or_default()[0]
        };
        let mut runtime = JsRuntime::new(&parsed.dom);
        runtime.install_viewport(800.0, 600.0, 0.0, 0.0);
        let mut geometry = BTreeMap::new();
        geometry.insert(
            image.as_u64(),
            ElementRect {
                x: 20.0,
                y: 30.0,
                width: 200.0,
                height: 100.0,
            },
        );
        runtime.install_element_geometry(geometry);
        runtime
            .execute(
                &mut parsed.dom,
                r#"
                    var observed = "";
                    var target = document.getElementById("lazy");
                    var observer = new IntersectionObserver(function(entries) {
                        observed = entries[0].isIntersecting + ":" +
                            entries[0].intersectionRatio + ":" +
                            entries[0].boundingClientRect.top;
                        if (entries[0].isIntersecting) target.src = target.getAttribute("data-src");
                    });
                    observer.observe(target);
                "#,
            )
            .expect("observer registration should execute");
        let tasks = runtime.take_pending_microtasks();
        assert_eq!(tasks.len(), 1);
        runtime
            .invoke_microtask(&mut parsed.dom, tasks.into_iter().next().unwrap())
            .expect("observer delivery should execute");
        assert_eq!(
            parsed.dom.attribute(image, "src").unwrap(),
            Some("loaded.png")
        );
        let outcome = runtime.execute(&mut parsed.dom, "observed").unwrap();
        assert_eq!(outcome.value, JsValue::String("true:1:30".to_owned()));
    }
}
