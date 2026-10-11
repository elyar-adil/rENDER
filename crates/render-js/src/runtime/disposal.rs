//! Disposal of `using` and `await using` bindings (ECMA-262 14.3.1, 14.3.3 and
//! 9.4.3 `DisposeResources`). A scope holds the resources its bindings declared,
//! and leaving the scope, normally or abruptly, disposes them in reverse
//! declaration order. A disposal that throws while the scope is left by a
//! throw wraps that throw in a `SuppressedError`; otherwise it replaces the
//! completion.
//!
//! This is the synchronous half, used by the tree-walking evaluator. A scope
//! that awaits its disposal runs in a coroutine, which drives the same steps
//! through [`super::coroutine_run`].

use super::JsRuntime;
use super::eval::Completion;
use super::types::{Environment, ScopeResource};
use crate::JsError;
use crate::JsErrorKind;
use crate::JsValue;
use crate::ObjectId;
use crate::parser::VariableKind;
use crate::value::DisposeResource;
use crate::value::DisposeStackKind;
use render_dom::Dom;

impl JsRuntime {
    /// `AddDisposableResource` for the value of a `using` or `await using`
    /// binding that `environment` is initializing (ECMA-262 14.3.1.1). For
    /// `using`, `null` and `undefined` add nothing. For `await using` they add a
    /// resource with no method, which still awaits when the scope is left. Any
    /// other value must have a dispose method, read once here: `@@asyncDispose`,
    /// falling back to `@@dispose`, for `await using`, and `@@dispose` otherwise.
    pub(super) fn add_disposable_resource(
        &mut self,
        dom: &mut Dom,
        environment: &Environment,
        value: JsValue,
        kind: VariableKind,
    ) -> Result<(), JsError> {
        let is_async = kind == VariableKind::AwaitUsing;
        let resource = if matches!(value, JsValue::Undefined | JsValue::Null) {
            if !is_async {
                return Ok(());
            }
            DisposeResource {
                value: JsValue::Undefined,
                method: None,
            }
        } else {
            let stack_kind = if is_async {
                DisposeStackKind::Async
            } else {
                DisposeStackKind::Sync
            };
            // The value must stay reachable while a getter for the method runs.
            let roots: Vec<ObjectId> = match &value {
                JsValue::Object(object) => vec![*object],
                _ => Vec::new(),
            };
            let method = self.with_transient_roots(&roots, |runtime| {
                runtime.dispose_method_of(dom, &value, stack_kind)
            })?;
            DisposeResource { value, method }
        };
        environment
            .borrow_mut()
            .disposables
            .push(ScopeResource { resource, is_async });
        Ok(())
    }

    /// `DisposeResources(scope, completion)` for a scope whose resources are the
    /// ones `environment` holds, run as the scope is left with `result`. The
    /// resources are disposed last declared first, and the scope is left with
    /// the completion it returns.
    pub(super) fn dispose_scope(
        &mut self,
        dom: &mut Dom,
        environment: &Environment,
        result: Result<Completion, JsError>,
    ) -> Result<Completion, JsError> {
        let mut result = result;
        loop {
            // A resource-limit stop is not a JavaScript completion, so disposal
            // does not run for it.
            if matches!(&result, Err(error) if error.kind() == JsErrorKind::ResourceLimit) {
                return result;
            }
            let next = environment.borrow_mut().disposables.pop();
            let Some(ScopeResource { resource, is_async }) = next else {
                return result;
            };
            // Only a throw completion is wrapped; a normal or jump completion's
            // value stays reachable while the disposer runs, as the throw does.
            let held = match &result {
                Ok(Completion::Normal(value) | Completion::Return(value)) => Some(value.clone()),
                Ok(_) => None,
                Err(error) => Some(self.error_value(error)),
            };
            let pending_throw = if result.is_err() { held.clone() } else { None };
            let outcome = if is_async {
                Err(JsError::syntax(
                    "`await using` outside an async function is not supported",
                    0,
                ))
            } else {
                self.call_disposer(dom, &resource, held.as_ref())
            };
            match outcome {
                Ok(_) => {}
                Err(error) if error.kind() == JsErrorKind::ResourceLimit => return Err(error),
                Err(error) => {
                    let thrown = self.thrown_value_while_holding(&error, held.as_ref());
                    let completion = self.fold_disposal_throw(pending_throw, thrown)?;
                    result = Err(JsError::thrown(completion));
                }
            }
        }
    }

    /// The completion a disposal that threw leaves a scope with: the thrown
    /// value, or, over a pending throw, a `SuppressedError` whose `error` is the
    /// disposal's throw and whose `suppressed` is the pending one.
    pub(super) fn fold_disposal_throw(
        &mut self,
        pending_throw: Option<JsValue>,
        thrown: JsValue,
    ) -> Result<JsValue, JsError> {
        let mut completion = pending_throw;
        self.record_disposal_error(&mut completion, thrown)?;
        // `record_disposal_error` always leaves a completion behind.
        Ok(completion.unwrap_or(JsValue::Undefined))
    }
}
