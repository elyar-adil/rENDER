//! Explicit resource management built-ins (ECMA-262): `DisposableStack`,
//! `AsyncDisposableStack` and `SuppressedError`.
//!
//! A stack's `[[DisposeCapability]]` is kept in its host as `DisposeResource`
//! records, so a stack is an ordinary object whose methods are native
//! functions. Disposal runs user code, and a collection can run during that
//! code, so every value the Rust side holds across a call is pinned in
//! `transient_roots` until the call returns.

use crate::JsError;
use crate::JsErrorKind;
use crate::JsSymbol;
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::runtime::eval::PrimitiveHint;
use crate::value::DisposeResource;
use crate::value::DisposeStackKind;
use crate::value::DisposeStackOp;
use crate::value::DisposeStackState;
use crate::value::ObjectHost;
use crate::value::PendingDisposal;
use crate::value::PropertyDescriptor;
use render_dom::Dom;

/// The object a value names, so it can be pinned as a transient root.
fn object_of(value: &JsValue) -> Option<ObjectId> {
    match value {
        JsValue::Object(object) => Some(*object),
        _ => None,
    }
}

/// The property `CreateNonEnumerableDataPropertyOrThrow` defines.
fn data_property(value: JsValue) -> PropertyDescriptor {
    PropertyDescriptor {
        getter: None,
        setter: None,
        value,
        writable: true,
        enumerable: false,
        configurable: true,
    }
}

fn brand_error(kind: DisposeStackKind) -> JsError {
    JsError::type_error(match kind {
        DisposeStackKind::Sync => "receiver is not a DisposableStack",
        DisposeStackKind::Async => "receiver is not an AsyncDisposableStack",
    })
}

impl JsRuntime {
    /// Run `body` with `roots` pinned as transient roots until it returns.
    fn with_transient_roots<R>(
        &mut self,
        roots: &[ObjectId],
        body: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let mark = self.transient_roots.len();
        self.transient_roots.extend_from_slice(roots);
        let result = body(self);
        self.transient_roots.truncate(mark);
        result
    }

    /// The methods of `DisposableStack` (`kind` is `Sync`) and
    /// `AsyncDisposableStack` (`Async`). The receiver is pinned for the whole
    /// operation, because a method can run user code.
    pub(in crate::runtime) fn dispose_stack_native(
        &mut self,
        dom: &mut Dom,
        operation: DisposeStackOp,
        kind: DisposeStackKind,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        self.with_transient_roots(&[receiver], |runtime| {
            runtime.stack_operation(dom, operation, kind, receiver, arguments)
        })
    }

    fn stack_operation(
        &mut self,
        dom: &mut Dom,
        operation: DisposeStackOp,
        kind: DisposeStackKind,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let first = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let second = arguments.get(1).cloned().unwrap_or(JsValue::Undefined);
        match operation {
            DisposeStackOp::Adopt => self.stack_adopt(receiver, kind, first, &second),
            DisposeStackOp::Defer => self.stack_defer(receiver, kind, &first),
            DisposeStackOp::Use => self.stack_use(dom, receiver, kind, first),
            DisposeStackOp::Move => self.stack_move(receiver, kind),
            DisposeStackOp::Disposed => {
                self.with_stack(receiver, kind, |state| JsValue::Boolean(state.disposed))
            }
            DisposeStackOp::Dispose => match kind {
                DisposeStackKind::Sync => self.stack_dispose(dom, receiver),
                DisposeStackKind::Async => self.stack_dispose_async(dom, receiver),
            },
        }
    }

    /// Run `update` on the state of `receiver`, which must be a stack of `kind`.
    fn with_stack<R>(
        &mut self,
        receiver: ObjectId,
        kind: DisposeStackKind,
        update: impl FnOnce(&mut DisposeStackState) -> R,
    ) -> Result<R, JsError> {
        match self.realm.host_mut(receiver) {
            Some(ObjectHost::DisposableStack(state)) if state.kind == kind => Ok(update(state)),
            _ => Err(brand_error(kind)),
        }
    }

    /// The brand check and the `disposed` check that `adopt`, `defer`, `use`
    /// and `move` make before they change anything.
    fn require_pending(
        &mut self,
        receiver: ObjectId,
        kind: DisposeStackKind,
    ) -> Result<(), JsError> {
        if self.with_stack(receiver, kind, |state| state.disposed)? {
            return Err(JsError::reference("the stack has already been disposed"));
        }
        Ok(())
    }

    fn require_callable(&self, value: &JsValue) -> Result<ObjectId, JsError> {
        match value {
            JsValue::Object(object) if Self::is_callable_object(*object, &self.realm) => {
                Ok(*object)
            }
            _ => Err(JsError::type_error("onDispose is not a function")),
        }
    }

    /// `adopt(value, onDispose)`: the record calls `onDispose(value)`.
    fn stack_adopt(
        &mut self,
        receiver: ObjectId,
        kind: DisposeStackKind,
        value: JsValue,
        on_dispose: &JsValue,
    ) -> Result<JsValue, JsError> {
        self.require_pending(receiver, kind)?;
        let on_dispose = self.require_callable(on_dispose)?;
        self.ensure_heap_capacity(1)?;
        let method = self
            .realm
            .bound_callable(on_dispose, JsValue::Undefined, vec![value.clone()]);
        self.with_stack(receiver, kind, |state| {
            state.resources.push(DisposeResource {
                value: JsValue::Undefined,
                method: Some(method),
            });
        })?;
        Ok(value)
    }

    /// `defer(onDispose)`: the record calls `onDispose()` with no `this`.
    fn stack_defer(
        &mut self,
        receiver: ObjectId,
        kind: DisposeStackKind,
        on_dispose: &JsValue,
    ) -> Result<JsValue, JsError> {
        self.require_pending(receiver, kind)?;
        let method = self.require_callable(on_dispose)?;
        self.with_stack(receiver, kind, |state| {
            state.resources.push(DisposeResource {
                value: JsValue::Undefined,
                method: Some(method),
            });
        })?;
        Ok(JsValue::Undefined)
    }

    /// `use(value)`: the record keeps `value` and its dispose method, which is
    /// read exactly once here. `null` and `undefined` are recorded with no method.
    fn stack_use(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        kind: DisposeStackKind,
        value: JsValue,
    ) -> Result<JsValue, JsError> {
        self.require_pending(receiver, kind)?;
        let method = self.with_transient_roots(&[receiver], |runtime| {
            runtime.dispose_method_of(dom, &value, kind)
        })?;
        self.with_stack(receiver, kind, |state| {
            state.resources.push(DisposeResource {
                value: value.clone(),
                method,
            });
        })?;
        Ok(value)
    }

    /// `GetDisposeMethod(value, hint)`. An async stack prefers
    /// `@@asyncDispose` and falls back to `@@dispose`; the first method that
    /// is not `undefined` or `null` is taken, and anything else that is not
    /// callable throws.
    fn dispose_method_of(
        &mut self,
        dom: &mut Dom,
        value: &JsValue,
        kind: DisposeStackKind,
    ) -> Result<Option<ObjectId>, JsError> {
        if matches!(value, JsValue::Undefined | JsValue::Null) {
            return Ok(None);
        }
        let object = self.to_object(value)?;
        let method = match kind {
            DisposeStackKind::Async => {
                match self.dispose_method_named(dom, object, "@@asyncDispose")? {
                    Some(method) => Some(method),
                    None => self.dispose_method_named(dom, object, "@@dispose")?,
                }
            }
            DisposeStackKind::Sync => self.dispose_method_named(dom, object, "@@dispose")?,
        };
        method
            .ok_or_else(|| JsError::type_error("resource has no callable dispose method"))
            .map(Some)
    }

    /// `GetMethod(object, @@key)`: `None` for `undefined` and `null`, the
    /// method when it is callable, and a `TypeError` for anything else.
    fn dispose_method_named(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        key: &str,
    ) -> Result<Option<ObjectId>, JsError> {
        let symbol = JsSymbol::well_known(key);
        match self.get_symbol_value(dom, object, &symbol)? {
            JsValue::Undefined | JsValue::Null => Ok(None),
            JsValue::Object(method) if Self::is_callable_object(method, &self.realm) => {
                Ok(Some(method))
            }
            _ => Err(JsError::type_error("dispose method is not callable")),
        }
    }

    /// `move()`: a new stack of the intrinsic class takes every record, and
    /// this stack becomes disposed without disposing anything.
    fn stack_move(
        &mut self,
        receiver: ObjectId,
        kind: DisposeStackKind,
    ) -> Result<JsValue, JsError> {
        self.require_pending(receiver, kind)?;
        self.ensure_heap_capacity(1)?;
        let resources = self.with_stack(receiver, kind, |state| {
            state.disposed = true;
            std::mem::take(&mut state.resources)
        })?;
        let prototype = self.realm.intrinsic_disposable_stack_prototype(kind);
        let moved = self.realm.disposable_stack(
            prototype,
            DisposeStackState {
                kind,
                disposed: false,
                resources,
                disposal: None,
            },
        );
        Ok(JsValue::Object(moved))
    }

    /// The last record of a stack, copied out without removing it. Disposal
    /// removes it only after its method returns, so the method stays rooted
    /// while it runs.
    fn peek_resource(&mut self, stack: ObjectId) -> Option<DisposeResource> {
        match self.realm.host_mut(stack) {
            Some(ObjectHost::DisposableStack(state)) => state.resources.last().cloned(),
            _ => None,
        }
    }

    fn pop_resource(&mut self, stack: ObjectId) {
        if let Some(ObjectHost::DisposableStack(state)) = self.realm.host_mut(stack) {
            state.resources.pop();
        }
    }

    /// Brand-checks `receiver`, marks its stack disposed and reports whether it
    /// already was. The first call is the one that disposes.
    fn begin_disposal(
        &mut self,
        receiver: ObjectId,
        kind: DisposeStackKind,
    ) -> Result<bool, JsError> {
        self.with_stack(receiver, kind, |state| {
            std::mem::replace(&mut state.disposed, true)
        })
    }

    /// `DisposableStack.prototype.dispose()`: call the records in reverse
    /// order. A throw is kept as is if it is the first; a later throw wraps the
    /// one already pending in a `SuppressedError`, so the outermost error is the
    /// last one thrown.
    fn stack_dispose(&mut self, dom: &mut Dom, receiver: ObjectId) -> Result<JsValue, JsError> {
        if self.begin_disposal(receiver, DisposeStackKind::Sync)? {
            return Ok(JsValue::Undefined);
        }
        let mut completion: Option<JsValue> = None;
        while let Some(resource) = self.peek_resource(receiver) {
            let outcome = self.call_disposer(dom, &resource, completion.as_ref());
            self.pop_resource(receiver);
            match outcome {
                Ok(_) => {}
                Err(error) if error.kind() == JsErrorKind::ResourceLimit => return Err(error),
                Err(error) => {
                    let thrown = self.thrown_value_while_holding(&error, completion.as_ref());
                    self.record_disposal_error(&mut completion, thrown)?;
                }
            }
        }
        match completion {
            Some(thrown) => Err(JsError::thrown(thrown)),
            None => Ok(JsValue::Undefined),
        }
    }

    /// `Call(method, value)` for one record. Its objects and the pending
    /// completion stay pinned while the method runs.
    fn call_disposer(
        &mut self,
        dom: &mut Dom,
        resource: &DisposeResource,
        completion: Option<&JsValue>,
    ) -> Result<JsValue, JsError> {
        let mut roots: Vec<ObjectId> = resource
            .method
            .into_iter()
            .chain(object_of(&resource.value))
            .collect();
        roots.extend(completion.and_then(object_of));
        self.with_transient_roots(&roots, |runtime| match resource.method {
            Some(method) => runtime.call_with_this(dom, method, &[], resource.value.clone()),
            None => Ok(JsValue::Undefined),
        })
    }

    /// The thrown value of `error`, converted while `held` stays reachable.
    fn thrown_value_while_holding(&mut self, error: &JsError, held: Option<&JsValue>) -> JsValue {
        let roots: Vec<ObjectId> = held.and_then(object_of).into_iter().collect();
        self.with_transient_roots(&roots, |runtime| runtime.error_value(error))
    }

    /// Fold a disposer's throw into the completion, the way `DisposeResources`
    /// does: the first throw is kept as is, and a later one becomes a
    /// `SuppressedError` whose `error` is the new throw.
    fn record_disposal_error(
        &mut self,
        completion: &mut Option<JsValue>,
        thrown: JsValue,
    ) -> Result<(), JsError> {
        *completion = Some(match completion.take() {
            Some(suppressed) => {
                let prototype = self.realm.intrinsic_suppressed_error_prototype();
                JsValue::Object(self.new_suppressed_error(prototype, thrown, suppressed, None)?)
            }
            None => thrown,
        });
        Ok(())
    }

    /// `disposeAsync()`. The call returns a promise, and a bad receiver rejects
    /// it rather than throwing. A stack that is already disposed resolves at once.
    fn stack_dispose_async(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
    ) -> Result<JsValue, JsError> {
        let (index, promise) = self.create_promise()?;
        match self.begin_disposal(receiver, DisposeStackKind::Async) {
            Ok(true) => self.resolve_promise(index, &JsValue::Undefined),
            Ok(false) => self.continue_async_disposal(dom, receiver, None, index)?,
            Err(error) => {
                let reason = self.thrown_value_while_holding(&error, Some(&promise));
                self.reject_promise(index, &reason);
            }
        }
        Ok(promise)
    }

    /// Run the remaining records of `stack` in reverse order until one of them
    /// suspends the disposal at an `await`, or none are left. Every record is
    /// awaited, even one with no method, because `Dispose` awaits the result of
    /// every async-hinted record. When the last record has run, `promise`
    /// rejects with the completion if one threw, and resolves otherwise.
    fn continue_async_disposal(
        &mut self,
        dom: &mut Dom,
        stack: ObjectId,
        completion: Option<JsValue>,
        promise: usize,
    ) -> Result<(), JsError> {
        let mut roots = vec![stack];
        roots.extend(completion.as_ref().and_then(object_of));
        let mut completion = completion;
        self.with_transient_roots(&roots, |runtime| {
            runtime.async_disposal_steps(dom, stack, &mut completion, promise)
        })
    }

    fn async_disposal_steps(
        &mut self,
        dom: &mut Dom,
        stack: ObjectId,
        completion: &mut Option<JsValue>,
        promise: usize,
    ) -> Result<(), JsError> {
        while let Some(resource) = self.peek_resource(stack) {
            self.transient_roots
                .extend(completion.as_ref().and_then(object_of));
            let called = self.call_disposer(dom, &resource, completion.as_ref());
            self.pop_resource(stack);
            let result = match called {
                Ok(result) => result,
                Err(error) if error.kind() == JsErrorKind::ResourceLimit => return Err(error),
                Err(error) => {
                    let thrown = self.thrown_value_while_holding(&error, completion.as_ref());
                    self.record_disposal_error(completion, thrown)?;
                    continue;
                }
            };
            // `Await(result)`: `PromiseResolve` can throw, which is the same as
            // the disposer throwing.
            self.transient_roots.extend(object_of(&result));
            let awaited = match self.promise_resolve(dom, &result) {
                Ok(awaited) => awaited,
                Err(error) if error.kind() == JsErrorKind::ResourceLimit => return Err(error),
                Err(error) => {
                    let thrown = self.thrown_value_while_holding(&error, completion.as_ref());
                    self.record_disposal_error(completion, thrown)?;
                    continue;
                }
            };
            self.transient_roots.push(awaited);
            self.ensure_heap_capacity(2)?;
            let on_fulfilled = self.realm.dispose_resume_function(stack, false);
            let on_rejected = self.realm.dispose_resume_function(stack, true);
            self.transient_roots.extend([on_fulfilled, on_rejected]);
            self.perform_promise_then(
                awaited,
                &[JsValue::Object(on_fulfilled), JsValue::Object(on_rejected)],
            )?;
            self.set_pending_disposal(
                stack,
                PendingDisposal {
                    completion: completion.take(),
                    promise,
                },
            );
            return Ok(());
        }
        match completion.take() {
            Some(thrown) => self.reject_promise(promise, &thrown),
            None => self.resolve_promise(promise, &JsValue::Undefined),
        }
        Ok(())
    }

    /// The `onFulfilled` or `onRejected` callback of an `await` in
    /// `disposeAsync`: it records a rejection as a throw and resumes the disposal.
    pub(in crate::runtime) fn resume_async_disposal(
        &mut self,
        dom: &mut Dom,
        stack: ObjectId,
        rejected: bool,
        reason: JsValue,
    ) -> Result<JsValue, JsError> {
        let Some(PendingDisposal {
            completion,
            promise,
        }) = self.take_pending_disposal(stack)
        else {
            return Ok(JsValue::Undefined);
        };
        let mut completion = completion;
        let mut roots = vec![stack];
        roots.extend(object_of(&reason));
        roots.extend(completion.as_ref().and_then(object_of));
        self.with_transient_roots(&roots, |runtime| {
            if rejected {
                runtime.record_disposal_error(&mut completion, reason)?;
            }
            runtime.continue_async_disposal(dom, stack, completion, promise)
        })?;
        Ok(JsValue::Undefined)
    }

    fn take_pending_disposal(&mut self, stack: ObjectId) -> Option<PendingDisposal> {
        match self.realm.host_mut(stack) {
            Some(ObjectHost::DisposableStack(state)) => state.disposal.take(),
            _ => None,
        }
    }

    fn set_pending_disposal(&mut self, stack: ObjectId, pending: PendingDisposal) {
        if let Some(ObjectHost::DisposableStack(state)) = self.realm.host_mut(stack) {
            state.disposal = Some(pending);
        }
    }

    /// `new DisposableStack()` and `new AsyncDisposableStack()`: an empty
    /// pending stack. Its prototype comes from `new.target`.
    pub(in crate::runtime) fn disposable_stack_constructor(
        &mut self,
        dom: &mut Dom,
        new_target: &JsValue,
        kind: DisposeStackKind,
    ) -> Result<JsValue, JsError> {
        let fallback = self.realm.intrinsic_disposable_stack_prototype(kind);
        let prototype = self.prototype_from_new_target(dom, new_target, fallback)?;
        self.with_transient_roots(&[prototype], |runtime| runtime.ensure_heap_capacity(1))?;
        let state = DisposeStackState {
            kind,
            disposed: false,
            resources: Vec::new(),
            disposal: None,
        };
        Ok(JsValue::Object(
            self.realm.disposable_stack(prototype, state),
        ))
    }

    /// `SuppressedError(error, suppressed, message)`, with or without `new`.
    /// `new.target` supplies the prototype, and `message` is converted only when it is given.
    pub(in crate::runtime) fn suppressed_error_constructor(
        &mut self,
        dom: &mut Dom,
        new_target: &JsValue,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let fallback = self.realm.intrinsic_suppressed_error_prototype();
        let prototype = self.prototype_from_new_target(dom, new_target, fallback)?;
        let error = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let suppressed = arguments.get(1).cloned().unwrap_or(JsValue::Undefined);
        let message =
            self.with_transient_roots(&[prototype], |runtime| match arguments.get(2) {
                None | Some(JsValue::Undefined) => Ok(None),
                Some(message) => runtime.to_string_strict(dom, message).map(Some),
            })?;
        let object = self.new_suppressed_error(prototype, error, suppressed, message)?;
        Ok(JsValue::Object(object))
    }

    /// An error instance of `prototype` with own `message` (when given),
    /// `error` and `suppressed`, in that order, and a `stack` property. The
    /// stack comes last, because the property order is part of the contract.
    fn new_suppressed_error(
        &mut self,
        prototype: ObjectId,
        error: JsValue,
        suppressed: JsValue,
        message: Option<String>,
    ) -> Result<ObjectId, JsError> {
        let mut roots = vec![prototype];
        roots.extend(object_of(&error));
        roots.extend(object_of(&suppressed));
        self.with_transient_roots(&roots, |runtime| runtime.ensure_heap_capacity(1))?;
        let object = self.realm.create_error(prototype, None);
        if let Some(message) = message {
            self.realm
                .define_property(object, "message", data_property(JsValue::String(message)));
        }
        self.realm
            .define_property(object, "error", data_property(error));
        self.realm
            .define_property(object, "suppressed", data_property(suppressed));
        self.install_error_stack(object, "SuppressedError");
        Ok(object)
    }

    /// The `stack` own property engine-made errors carry: `Name: message`
    /// followed by the current frames.
    fn install_error_stack(&mut self, object: ObjectId, fallback_name: &str) {
        let name = self
            .realm
            .get_property(object, "name")
            .map_or_else(|| fallback_name.to_owned(), |value| value.to_js_string());
        let header = match self.realm.get_property(object, "message") {
            Some(JsValue::String(text)) if !text.is_empty() => format!("{name}: {text}"),
            _ => name,
        };
        let stack = format!("{header}{}", self.stack_frame_lines());
        self.realm.define_property(
            object,
            "stack",
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::String(stack),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
    }

    /// `ToString(value)` (ECMA-262 7.1.17). Unlike the `String` conversion, it
    /// throws for a Symbol, including one that `ToPrimitive` produced.
    fn to_string_strict(&mut self, dom: &mut Dom, value: &JsValue) -> Result<String, JsError> {
        match self.to_primitive_with_hint(dom, value.clone(), PrimitiveHint::String)? {
            JsValue::Symbol(_) => Err(JsError::type_error(
                "Cannot convert a Symbol value to a string",
            )),
            primitive => Ok(primitive.to_js_string()),
        }
    }

    /// `GetPrototypeFromConstructor(newTarget, fallback)`: `newTarget.prototype`
    /// when that is an object, and the intrinsic `fallback` otherwise.
    fn prototype_from_new_target(
        &mut self,
        dom: &mut Dom,
        new_target: &JsValue,
        fallback: ObjectId,
    ) -> Result<ObjectId, JsError> {
        if let JsValue::Object(target) = new_target
            && let JsValue::Object(prototype) = self.get_member(dom, *target, "prototype")?
        {
            return Ok(prototype);
        }
        Ok(fallback)
    }
}
