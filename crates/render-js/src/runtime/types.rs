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

use crate::JsValue;
use crate::ObjectId;
use crate::parser::Expr;
use crate::parser::Statement;
use crate::parser::VariableKind;
use crate::runtime::builtins::promise::PromiseState;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

#[derive(Clone, Debug)]
pub(super) struct Binding {
    pub(super) value: JsValue,
    pub(super) mutable: bool,
    pub(super) initialized: bool,
    pub(super) kind: VariableKind,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct GlobalBinding {
    pub(super) mutable: bool,
    pub(super) initialized: bool,
    pub(super) kind: VariableKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ObjectEntryKind {
    Keys,
    Values,
    Entries,
}

impl ObjectEntryKind {
    pub(super) fn function_name(self) -> &'static str {
        match self {
            Self::Keys => "Object.keys",
            Self::Values => "Object.values",
            Self::Entries => "Object.entries",
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct EnvironmentRecord {
    pub(super) bindings: BTreeMap<String, Binding>,
    pub(super) function_scope: bool,
}

pub(super) type Environment = Rc<RefCell<EnvironmentRecord>>;

/// Upper bound on buffered `console.*` messages; the oldest entry is dropped
/// when script logs past it so a chatty page cannot exhaust memory.
pub(super) const MAX_BUFFERED_CONSOLE_MESSAGES: usize = 4096;

#[derive(Clone, Debug)]
pub(super) struct UserFunction {
    pub(super) name: Option<String>,
    pub(super) parameters: Vec<String>,
    /// Default initializer expressions parallel to `parameters`; `None` for
    /// parameters without one. A default evaluates left to right whenever
    /// its argument is `undefined` or absent, in the call environment built
    /// so far, so later defaults see earlier bindings.
    pub(super) defaults: Vec<Option<Expr>>,
    pub(super) body: Vec<Statement>,
    pub(super) captured_environment: Vec<Environment>,
    /// Arrow functions do not bind their own `this`; they resolve the
    /// enclosing function's `this` binding lexically.
    pub(super) arrow: bool,
    /// Class methods and constructors run in strict mode: a nullish `this`
    /// never falls back to the global object.
    pub(super) strict: bool,
    /// Class-specific metadata for methods and constructors; `None` for
    /// ordinary functions.
    pub(super) class: Option<Rc<ClassFunction>>,
    /// The final parameter collects the remaining arguments into an array.
    pub(super) rest: bool,
}

/// One class body's private-name registry, chained to the lexically
/// enclosing class so a nested class can access outer private names.
#[derive(Clone, Debug, Default)]
pub(super) struct PrivateScope {
    pub(super) names: BTreeMap<String, u64>,
    pub(super) outer: Option<Rc<PrivateScope>>,
}

impl PrivateScope {
    /// Resolve a written private name through the lexical class chain.
    pub(super) fn resolve(&self, name: &str) -> Option<u64> {
        let mut scope = self;
        loop {
            if let Some(id) = scope.names.get(name) {
                return Some(*id);
            }
            scope = scope.outer.as_deref()?;
        }
    }
}

/// Class-specific metadata carried by a method or constructor function.
#[derive(Clone, Debug, Default)]
pub(super) struct ClassFunction {
    /// The object `super.property` starts from (the class prototype for
    /// instance methods, the constructor for static methods).
    pub(super) home_object: Option<ObjectId>,
    /// The parent constructor for derived classes; `None` for base classes
    /// and non-constructors.
    pub(super) super_constructor: Option<ObjectId>,
    pub(super) derived: bool,
    /// Instance fields initialized by a constructor (base) or by its
    /// `super()` call (derived).
    pub(super) fields: Vec<ClassFieldDefinition>,
    /// Class-body private-name registry: written name -> unique id.
    pub(super) private_names: Rc<PrivateScope>,
    /// Whether this function is a constructor at all (base constructors are
    /// constructible; methods are not).
    pub(super) constructor: bool,
    /// Environment vector of the class body, used to evaluate field
    /// initializers when `super()` completes.
    pub(super) environment: Vec<Environment>,
}

/// One class field definition: its resolved key and optional initializer.
#[derive(Clone, Debug)]
pub(super) struct ClassFieldDefinition {
    pub(super) key: ClassFieldKey,
    pub(super) initializer: Option<crate::parser::Expr>,
}

#[derive(Clone, Debug)]
pub(super) enum ClassFieldKey {
    Named(String),
    Private(u64),
}

/// Per-call class context backing `super`, `new.target`, private names, and
/// instance-field initialization. Pushed by every user-function call; arrows
/// inherit the frame of the function they execute inside.
#[derive(Clone, Debug, Default)]
pub(super) struct ClassFrame {
    pub(super) function: Option<Rc<ClassFunction>>,
    /// Lexical private scope for `#name` resolution; set while class bodies,
    /// field initializers, and static blocks evaluate, even outside a call.
    pub(super) private_scope: Option<Rc<PrivateScope>>,
}

/// One active JavaScript call frame, retained for stack traces and
/// depth-limit diagnostics. User functions surface their source name;
/// native entry points surface the Rust variant name.
#[derive(Clone, Debug)]
pub(super) struct CallFrame {
    pub(super) name: String,
}

#[derive(Clone, Debug)]
pub(super) struct PromiseReaction {
    pub(super) on_fulfilled: Option<ObjectId>,
    pub(super) on_rejected: Option<ObjectId>,
    pub(super) result_promise: usize,
}

#[derive(Clone, Debug)]
pub(super) struct PromiseRecord {
    pub(super) state: PromiseState,
    pub(super) reactions: Vec<PromiseReaction>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum JsMicrotask {
    Callback(ObjectId),
    IntersectionObserver(ObjectId),
    MutationObserver(ObjectId),
    /// A `MediaQueryList` whose `matches` flipped, so its `change` listeners
    /// have to run. Not an observer and not a callback: the invocation is a
    /// dispatch to whichever of `onchange`, `change` listeners and the deprecated
    /// `addListener` callbacks are present, and the embedding owns the choice of
    /// when in the frame that happens.
    MediaQueryListChange(ObjectId),
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

/// One network transfer (`fetch()` or `XMLHttpRequest`) that script queued.
///
/// The runtime never performs I/O; the embedding drains these requests
/// through [`JsRuntime::take_pending_fetch_requests`], executes them on its
/// transport, and completes each one by id through
/// [`JsRuntime::settle_fetch`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingFetch {
    /// Correlation id echoed back to [`JsRuntime::settle_fetch`]. Ids are
    /// unique per runtime and never reused.
    pub id: u64,
    /// Absolute request URL, already resolved against the document base.
    pub url: String,
    /// Uppercased request method (`GET`, `POST`, ...).
    pub method: String,
    /// Caller-provided request headers in submission order.
    pub headers: Vec<(String, String)>,
    /// Request body for methods that carry one.
    pub body: Option<String>,
}

/// Completed transfer handed back through [`JsRuntime::settle_fetch`].
///
/// HTTP 4xx/5xx responses are successful transfers (matching the `fetch`
/// contract); the `Err` side of `settle_fetch` is reserved for transport
/// failures such as DNS, TLS, timeout, or cancellation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchOutcome {
    /// Numeric HTTP status code, including non-success statuses.
    pub status: u16,
    /// Reason phrase exactly as received (may be empty).
    pub status_text: String,
    /// Response headers in wire order.
    pub headers: Vec<(String, String)>,
    /// Raw response body bytes.
    pub body: Vec<u8>,
}

/// A compiled regular expression plus its mutable `lastIndex` state.
#[derive(Debug)]
pub(super) struct RegexRecord {
    pub(super) compiled: crate::regex::Compiled,
    pub(super) last_index: usize,
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
