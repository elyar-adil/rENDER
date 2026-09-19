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
use crate::runtime::types::JsMicrotask;
use crate::runtime::types::PromiseReaction;
use crate::runtime::types::PromiseRecord;
use crate::value::NativeFunction;
use crate::value::ObjectHost;
use render_dom::Dom;

impl JsRuntime {
    pub(in crate::runtime) fn dispatch_promise_native(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match function {
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
            NativeFunction::PromiseFinally => {
                let Some(callback) = arguments.first().and_then(|value| match value {
                    JsValue::Object(function)
                        if JsRuntime::is_callable_object(*function, &self.realm) =>
                    {
                        Some(*function)
                    }
                    _ => None,
                }) else {
                    return self.perform_promise_then(receiver, arguments);
                };
                // `p.finally(f)` behaves like
                // `p.then(v => { f(); return v; }, e => { f(); throw e; })`;
                // BoundCallable captures `f` ahead of the settlement value.
                let pass = self.realm.native_object(NativeFunction::PromiseFinallyPass);
                let rethrow = self
                    .realm
                    .native_object(NativeFunction::PromiseFinallyReject);
                let pass_handler = self.realm.bound_callable(
                    pass,
                    JsValue::Undefined,
                    vec![JsValue::Object(callback)],
                );
                let reject_handler = self.realm.bound_callable(
                    rethrow,
                    JsValue::Undefined,
                    vec![JsValue::Object(callback)],
                );
                self.perform_promise_then(
                    receiver,
                    &[
                        JsValue::Object(pass_handler),
                        JsValue::Object(reject_handler),
                    ],
                )
            }
            NativeFunction::PromiseFinallyPass => {
                let callback = required_argument(arguments, 0, "finally")?;
                let value = arguments.get(1).cloned().unwrap_or(JsValue::Undefined);
                if let JsValue::Object(function) = callback {
                    self.call_with_this(dom, *function, &[], JsValue::Undefined)?;
                }
                Ok(value)
            }
            NativeFunction::PromiseFinallyReject => {
                let callback = required_argument(arguments, 0, "finally")?;
                let reason = arguments.get(1).cloned().unwrap_or(JsValue::Undefined);
                if let JsValue::Object(function) = callback {
                    self.call_with_this(dom, *function, &[], JsValue::Undefined)?;
                }
                Err(JsError::thrown(reason))
            }
            other => self.dispatch_url_native(dom, other, receiver, arguments),
        }
    }
}

#[derive(Clone, Debug)]
pub(in crate::runtime) enum PromiseState {
    Pending,
    Fulfilled(JsValue),
    Rejected(JsValue),
}

impl JsRuntime {
    pub(in crate::runtime) fn create_promise(&mut self) -> Result<(usize, JsValue), JsError> {
        self.ensure_heap_capacity(1)?;
        let index = self.promises.len();
        let object = self.realm.promise(index);
        self.promises.push(PromiseRecord {
            state: PromiseState::Pending,
            reactions: Vec::new(),
        });
        Ok((index, JsValue::Object(object)))
    }

    pub(in crate::runtime) fn resolve_promise(&mut self, promise: usize, value: &JsValue) {
        self.settle_promise(promise, value, true);
    }

    pub(in crate::runtime) fn resolve_promise_value(
        &mut self,
        promise: usize,
        value: &JsValue,
    ) -> Result<(), JsError> {
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

    pub(in crate::runtime) fn reject_promise(&mut self, promise: usize, reason: &JsValue) {
        self.settle_promise(promise, reason, false);
    }

    pub(in crate::runtime) fn settle_promise(
        &mut self,
        promise: usize,
        value: &JsValue,
        fulfilled: bool,
    ) {
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

    pub(in crate::runtime) fn enqueue_reaction(
        &mut self,
        reaction: &PromiseReaction,
        argument: JsValue,
        fulfilled: bool,
    ) {
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

    pub(in crate::runtime) fn perform_promise_then(
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
}
