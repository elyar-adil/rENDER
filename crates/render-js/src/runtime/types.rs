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

#[derive(Clone, Copy)]
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
    pub(super) body: Vec<Statement>,
    pub(super) captured_environment: Vec<Environment>,
    pub(super) lexical_this: Option<JsValue>,
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
