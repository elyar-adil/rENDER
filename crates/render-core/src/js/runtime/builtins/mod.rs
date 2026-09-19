//! ECMAScript and Web API built-ins, grouped by specification area.
//!
//! Each file owns one API domain: its `NativeFunction` dispatch arms, its
//! `impl JsRuntime` methods, and its private helpers. Adding a new Web API
//! means adding a variant to `crate::js::value::NativeFunction`, creating
//! the value where the host object is built, and handling it in this
//! domain's `dispatch_*_native` match. `global_fns.rs` holds the residual
//! global-object functions and stays exhaustive over the enum so that a
//! missed arm is a compile error, not a runtime gap.
//!
//! Native dispatch is a fallthrough chain rooted in
//! `JsRuntime::call_native_dispatch`; `fetch.rs` currently sits at the head
//! and hands unmatched functions to `dom.rs`.

pub(super) mod array;
pub(super) mod collections;
pub(super) mod date;
pub(super) mod dom;
pub(super) mod events;
pub(super) mod fetch;
pub(super) mod global_fns;
pub(super) mod json;
pub(super) mod math;
pub(super) mod object;
pub(super) mod observers;
pub(super) mod promise;
pub(super) mod regexp;
pub(super) mod string;
pub(super) mod style;
pub(super) mod timers;
pub(super) mod typed_array;
pub(super) mod url;
pub(super) mod video;
