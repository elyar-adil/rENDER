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
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::runtime::types::JsMicrotask;
use crate::runtime::types::PromiseCapability;
use crate::runtime::types::PromiseReaction;
use crate::runtime::types::PromiseRecord;
use crate::value::JsSymbol;
use crate::value::NativeFunction;
use crate::value::ObjectHost;
use crate::value::PropertyDescriptor;
use render_dom::Dom;

impl JsRuntime {
    pub(in crate::runtime) fn dispatch_promise_native(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let first = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        match function {
            NativeFunction::PromiseResolve => {
                self.promise_resolve_with_constructor(dom, receiver, &first)
            }
            NativeFunction::PromiseReject => {
                let (promise, capability) = self.new_promise_capability(dom, receiver)?;
                self.capability_reject(dom, capability, &first)?;
                Ok(promise)
            }
            NativeFunction::PromiseWithResolvers => self.promise_with_resolvers(dom, receiver),
            NativeFunction::PromiseTry => self.promise_try(dom, receiver, arguments),
            NativeFunction::PromiseSpecies => Ok(JsValue::Object(receiver)),
            NativeFunction::PromiseThen => self.promise_then(dom, receiver, arguments),
            NativeFunction::PromiseCatch => {
                self.invoke_then(dom, receiver, &[JsValue::Undefined, first])
            }
            NativeFunction::PromiseFinally => self.promise_finally(dom, receiver, arguments),
            NativeFunction::PromiseFinallyPass => {
                self.promise_finally_handler(dom, arguments, false)
            }
            NativeFunction::PromiseFinallyReject => {
                self.promise_finally_handler(dom, arguments, true)
            }
            NativeFunction::PromiseValueThunk => Ok(first),
            NativeFunction::PromiseThrower => Err(JsError::thrown(first)),
            NativeFunction::PromiseAll => {
                self.promise_combinator(dom, Combinator::All, receiver, arguments)
            }
            NativeFunction::PromiseAllSettled => {
                self.promise_combinator(dom, Combinator::AllSettled, receiver, arguments)
            }
            NativeFunction::PromiseAny => {
                self.promise_combinator(dom, Combinator::Any, receiver, arguments)
            }
            NativeFunction::PromiseRace => {
                self.promise_combinator(dom, Combinator::Race, receiver, arguments)
            }
            NativeFunction::PromiseAllKeyed => {
                self.promise_keyed_combinator(dom, Combinator::All, receiver, arguments)
            }
            NativeFunction::PromiseAllSettledKeyed => {
                self.promise_keyed_combinator(dom, Combinator::AllSettled, receiver, arguments)
            }
            NativeFunction::PromiseCombinatorElement => {
                self.combinator_element_settled(dom, arguments)
            }
            NativeFunction::PromiseCapabilityExecutor => self.capability_executor(arguments),
            NativeFunction::PromiseResolveThenableJob => self.resolve_thenable_job(dom, arguments),
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

/// Which of the four combinators is running. The keyed forms reuse `All` and
/// `AllSettled`, since their element rules are the same.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Combinator {
    All,
    AllSettled,
    Any,
    Race,
}

impl Combinator {
    const fn code(self) -> f64 {
        match self {
            Self::All => 0.0,
            Self::AllSettled => 1.0,
            Self::Any => 2.0,
            Self::Race => 3.0,
        }
    }

    fn from_code(code: f64) -> Self {
        match code as i32 {
            0 => Self::All,
            1 => Self::AllSettled,
            2 => Self::Any,
            _ => Self::Race,
        }
    }
}

/// Element roles an element settlement function plays, passed as its fourth
/// bound argument after the store, the element's index and its pair.
const ROLE_FULFILLED: f64 = 0.0;
const ROLE_SETTLED_FULFILLED: f64 = 1.0;
const ROLE_SETTLED_REJECTED: f64 = 2.0;
const ROLE_ANY_REJECTED: f64 = 3.0;

/// Property names of the settlement store a combinator shares between its
/// element functions.
///
/// The store is a plain object rather than a Rust-side record: the collector
/// traces it as an ordinary object, and each element function reaches it
/// through its bound arguments, so it lives exactly as long as an element can
/// still settle.
mod store {
    pub(super) const KIND: &str = "k";
    pub(super) const RESOLVE: &str = "r";
    pub(super) const REJECT: &str = "j";
    pub(super) const VALUES: &str = "v";
    pub(super) const ERRORS: &str = "e";
    /// Keyed forms only: maps each element index to the input key it settles.
    pub(super) const KEYS: &str = "y";
    pub(super) const REMAINING: &str = "n";
    pub(super) const COUNT: &str = "c";
    /// True for the array-valued forms, false for the keyed object forms.
    pub(super) const ARRAY: &str = "a";
}

/// Bound on the elements one combinator will attach, so a runaway iterable
/// cannot grow the store without limit.
const MAX_COMBINATOR_ELEMENTS: usize = 1 << 20;

impl JsRuntime {
    // ------------------------------------------------------------ promise records

    pub(in crate::runtime) fn create_promise(&mut self) -> Result<(usize, JsValue), JsError> {
        let (index, object) = self.create_promise_record()?;
        Ok((index, JsValue::Object(object)))
    }

    /// A pending promise record and the promise object that wraps it, for
    /// built-ins that keep the promise and its object themselves.
    pub(in crate::runtime) fn create_promise_record(
        &mut self,
    ) -> Result<(usize, ObjectId), JsError> {
        self.ensure_heap_capacity(1)?;
        let index = self.promises.len();
        let object = self.realm.promise(index);
        self.promises.push(PromiseRecord {
            state: PromiseState::Pending,
            reactions: Vec::new(),
        });
        Ok((index, object))
    }

    /// Fulfil `promise` with `value` without inspecting it for a `then`. Only
    /// for values the engine created itself, which can never be thenables.
    pub(in crate::runtime) fn resolve_promise(&mut self, promise: usize, value: &JsValue) {
        self.settle_promise(promise, value, true);
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
            capability: reaction.capability,
        });
    }

    /// `PerformPromiseThen` with an intrinsic result promise, returned to the
    /// caller. Used by `await` and the async-iteration plumbing.
    pub(in crate::runtime) fn perform_promise_then(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let Some(ObjectHost::Promise(promise)) = self.realm.host(receiver) else {
            return Err(JsError::type_error("incompatible Promise method receiver"));
        };
        let on_fulfilled = callable_argument(arguments.first(), &self.realm);
        let on_rejected = callable_argument(arguments.get(1), &self.realm);
        let (index, object) = self.create_promise_record()?;
        self.perform_then(
            promise,
            on_fulfilled,
            on_rejected,
            Some(PromiseCapability::Record {
                promise: index,
                object,
            }),
        )?;
        Ok(JsValue::Object(object))
    }

    /// `PerformPromiseThen(promise, onFulfilled, onRejected, resultCapability)`.
    fn perform_then(
        &mut self,
        promise: usize,
        on_fulfilled: Option<ObjectId>,
        on_rejected: Option<ObjectId>,
        capability: Option<PromiseCapability>,
    ) -> Result<(), JsError> {
        let reaction = PromiseReaction {
            on_fulfilled,
            on_rejected,
            capability,
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
        Ok(())
    }

    /// `PromiseReactionJob`: run one handler and deliver its outcome to the
    /// reaction's capability. An abrupt handler rejects the capability; an
    /// abrupt capability function is the job's own error.
    pub(in crate::runtime) fn run_promise_reaction(
        &mut self,
        dom: &mut Dom,
        handler: Option<ObjectId>,
        argument: JsValue,
        fulfilled: bool,
        capability: Option<PromiseCapability>,
    ) -> Result<JsValue, JsError> {
        let outcome = match handler {
            Some(handler) => self.call(dom, handler, std::slice::from_ref(&argument)),
            None if fulfilled => Ok(argument),
            None => Err(JsError::thrown(argument)),
        };
        let Some(capability) = capability else {
            return match outcome {
                Err(error) if error.kind() == JsErrorKind::ResourceLimit => Err(error),
                _ => Ok(JsValue::Undefined),
            };
        };
        match outcome {
            Ok(value) => self.capability_resolve(dom, capability, &value)?,
            Err(error) if error.kind() != JsErrorKind::ResourceLimit => {
                let reason = self.error_value(&error);
                self.capability_reject(dom, capability, &reason)?;
            }
            Err(error) => return Err(error),
        }
        Ok(JsValue::Undefined)
    }

    /// The Promise Resolve Functions step (ECMA-262 27.2.1.3.2): a value that
    /// is a thenable is adopted by a job that calls its `then`, rather than
    /// fulfilling `promise` with the thenable itself.
    pub(in crate::runtime) fn resolve_promise_with(
        &mut self,
        dom: &mut Dom,
        promise: usize,
        value: &JsValue,
    ) -> Result<(), JsError> {
        let JsValue::Object(object) = value else {
            self.resolve_promise(promise, value);
            return Ok(());
        };
        let object = *object;
        if matches!(self.realm.host(object), Some(ObjectHost::Promise(source)) if source == promise)
        {
            let reason = self.construct_standard_error(
                crate::value::ErrorKind::TypeError,
                "Chaining cycle detected for promise",
            )?;
            self.reject_promise(promise, &reason);
            return Ok(());
        }
        let then = match self.get_member(dom, object, "then") {
            Ok(then) => then,
            Err(error) if error.kind() == JsErrorKind::ResourceLimit => return Err(error),
            Err(error) => {
                let reason = self.error_value(&error);
                self.reject_promise(promise, &reason);
                return Ok(());
            }
        };
        match then {
            JsValue::Object(then) if Self::is_callable_object(then, &self.realm) => {
                self.enqueue_thenable_job(promise, object, then)
            }
            _ => {
                self.resolve_promise(promise, value);
                Ok(())
            }
        }
    }

    /// `NewPromiseResolveThenableJob`: a fresh resolving pair for `promise`,
    /// handed to `then` when the job runs.
    fn enqueue_thenable_job(
        &mut self,
        promise: usize,
        thenable: ObjectId,
        then: ObjectId,
    ) -> Result<(), JsError> {
        self.ensure_heap_capacity(4)?;
        let (resolve, reject) = self.allocate_resolving_functions(promise);
        let target = self
            .realm
            .native_object(NativeFunction::PromiseResolveThenableJob);
        let job = self.realm.bound_callable(
            target,
            JsValue::Undefined,
            vec![
                JsValue::Object(resolve),
                JsValue::Object(reject),
                JsValue::Object(thenable),
                JsValue::Object(then),
            ],
        );
        self.pending_microtasks.push(JsMicrotask::Callback(job));
        Ok(())
    }

    fn resolve_thenable_job(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let (Some(resolve), Some(reject), Some(thenable), Some(then)) = (
            object_argument(arguments, 0),
            object_argument(arguments, 1),
            object_argument(arguments, 2),
            object_argument(arguments, 3),
        ) else {
            return Ok(JsValue::Undefined);
        };
        let outcome = self.call_with_this(
            dom,
            then,
            &[JsValue::Object(resolve), JsValue::Object(reject)],
            JsValue::Object(thenable),
        );
        match outcome {
            Ok(_) => Ok(JsValue::Undefined),
            Err(error) if error.kind() == JsErrorKind::ResourceLimit => Err(error),
            Err(error) => {
                let reason = self.error_value(&error);
                self.call_with_this(dom, reject, &[reason], JsValue::Undefined)?;
                Ok(JsValue::Undefined)
            }
        }
    }

    // ------------------------------------------------------ resolving functions

    /// Claim the `alreadyResolved` record of a pair. Returns false when the pair
    /// has already been used.
    fn claim_promise_pair(&mut self, pair: usize) -> bool {
        match self.promise_pairs_resolved.get_mut(pair) {
            Some(resolved) if !*resolved => {
                *resolved = true;
                true
            }
            _ => false,
        }
    }

    fn new_promise_pair(&mut self) -> usize {
        self.promise_pairs_resolved.push(false);
        self.promise_pairs_resolved.len() - 1
    }

    /// `CreateResolvingFunctions(promise)`: a resolve and a reject function that
    /// share one `alreadyResolved` record.
    fn create_resolving_functions(
        &mut self,
        promise: usize,
    ) -> Result<(ObjectId, ObjectId), JsError> {
        self.ensure_heap_capacity(2)?;
        Ok(self.allocate_resolving_functions(promise))
    }

    fn allocate_resolving_functions(&mut self, promise: usize) -> (ObjectId, ObjectId) {
        let pair = self.new_promise_pair();
        (
            self.realm.promise_settler(promise, true, pair),
            self.realm.promise_settler(promise, false, pair),
        )
    }

    /// Run a resolving function that a script called.
    pub(in crate::runtime) fn call_promise_settler(
        &mut self,
        dom: &mut Dom,
        promise: usize,
        fulfilled: bool,
        pair: usize,
        value: &JsValue,
    ) -> Result<(), JsError> {
        if !self.claim_promise_pair(pair) {
            return Ok(());
        }
        if fulfilled {
            self.resolve_promise_with(dom, promise, value)
        } else {
            self.reject_promise(promise, value);
            Ok(())
        }
    }

    /// `new Promise(executor)`.
    pub(in crate::runtime) fn construct_promise(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let executor = JsRuntime::require_callable_object(
            &arguments.first().cloned().unwrap_or(JsValue::Undefined),
            &self.realm,
        )?;
        let (index, object) = self.create_promise_record()?;
        let (resolve, reject) = self.create_resolving_functions(index)?;
        if let Err(error) = self.call(
            dom,
            executor,
            &[JsValue::Object(resolve), JsValue::Object(reject)],
        ) {
            if error.kind() == JsErrorKind::ResourceLimit {
                return Err(error);
            }
            let reason = self.error_value(&error);
            self.call_with_this(dom, reject, &[reason], JsValue::Undefined)?;
        }
        Ok(JsValue::Object(object))
    }

    // ------------------------------------------------------------ capabilities

    /// `NewPromiseCapability(C)` (ECMA-262 27.2.1.5). The intrinsic constructor
    /// gets a record directly, since no script can observe its executor.
    /// Any other constructor is constructed with an executor that captures the
    /// resolve and reject functions it is given.
    pub(in crate::runtime) fn new_promise_capability(
        &mut self,
        dom: &mut Dom,
        constructor: ObjectId,
    ) -> Result<(JsValue, PromiseCapability), JsError> {
        if matches!(
            self.realm.host(constructor),
            Some(ObjectHost::PromiseConstructor)
        ) {
            let (index, object) = self.create_promise_record()?;
            return Ok((
                JsValue::Object(object),
                PromiseCapability::Record {
                    promise: index,
                    object,
                },
            ));
        }
        if !self.is_constructor(constructor) {
            return Err(JsError::type_error(
                "Promise capability constructor is not a constructor",
            ));
        }
        self.ensure_heap_capacity(3)?;
        let slots = self.realm.create_ordinary_object();
        let executor = self.bound_function(
            NativeFunction::PromiseCapabilityExecutor,
            vec![JsValue::Object(slots)],
            2.0,
        );
        let promise = self.construct_dispatch(dom, constructor, &[JsValue::Object(executor)])?;
        let resolve = self.capability_slot(slots, "resolve")?;
        let reject = self.capability_slot(slots, "reject")?;
        Ok((promise, PromiseCapability::Functions { resolve, reject }))
    }

    fn capability_slot(&self, slots: ObjectId, name: &str) -> Result<ObjectId, JsError> {
        match self.realm.get_property(slots, name) {
            Some(JsValue::Object(function)) if Self::is_callable_object(function, &self.realm) => {
                Ok(function)
            }
            _ => Err(JsError::type_error(
                "Promise capability function is not callable",
            )),
        }
    }

    /// The `GetCapabilitiesExecutor` function: records the resolve and reject
    /// arguments once. The slot store is its first bound argument.
    fn capability_executor(&mut self, arguments: &[JsValue]) -> Result<JsValue, JsError> {
        let Some(slots) = object_argument(arguments, 0) else {
            return Ok(JsValue::Undefined);
        };
        for name in ["resolve", "reject"] {
            if matches!(self.realm.get_property(slots, name), Some(value) if value != JsValue::Undefined)
            {
                return Err(JsError::type_error(
                    "Promise capability executor was already invoked",
                ));
            }
        }
        self.realm.set_property(
            slots,
            "resolve".to_owned(),
            arguments.get(1).cloned().unwrap_or(JsValue::Undefined),
        );
        self.realm.set_property(
            slots,
            "reject".to_owned(),
            arguments.get(2).cloned().unwrap_or(JsValue::Undefined),
        );
        Ok(JsValue::Undefined)
    }

    /// `capability.[[Resolve]](value)`.
    fn capability_resolve(
        &mut self,
        dom: &mut Dom,
        capability: PromiseCapability,
        value: &JsValue,
    ) -> Result<(), JsError> {
        match capability {
            PromiseCapability::Record { promise, .. } => {
                self.resolve_promise_with(dom, promise, value)
            }
            PromiseCapability::Functions { resolve, .. } => self
                .call_with_this(
                    dom,
                    resolve,
                    std::slice::from_ref(value),
                    JsValue::Undefined,
                )
                .map(|_| ()),
        }
    }

    /// `capability.[[Reject]](reason)`.
    fn capability_reject(
        &mut self,
        dom: &mut Dom,
        capability: PromiseCapability,
        reason: &JsValue,
    ) -> Result<(), JsError> {
        match capability {
            PromiseCapability::Record { promise, .. } => {
                self.reject_promise(promise, reason);
                Ok(())
            }
            PromiseCapability::Functions { reject, .. } => self
                .call_with_this(
                    dom,
                    reject,
                    std::slice::from_ref(reason),
                    JsValue::Undefined,
                )
                .map(|_| ()),
        }
    }

    /// The capability's resolve and reject as callable objects, for handing to
    /// `then`. A record gets a fresh resolving pair.
    fn capability_functions(
        &mut self,
        capability: PromiseCapability,
    ) -> Result<(ObjectId, ObjectId), JsError> {
        match capability {
            PromiseCapability::Record { promise, .. } => self.create_resolving_functions(promise),
            PromiseCapability::Functions { resolve, reject } => Ok((resolve, reject)),
        }
    }

    // --------------------------------------------------------------- prototype

    /// `SpeciesConstructor(object, %Promise%)`.
    fn promise_species_constructor(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
    ) -> Result<ObjectId, JsError> {
        let default = self.realm.promise_constructor();
        let constructor = match self.get_member(dom, object, "constructor")? {
            JsValue::Undefined => return Ok(default),
            JsValue::Object(constructor) => constructor,
            _ => {
                return Err(JsError::type_error("Promise constructor is not an object"));
            }
        };
        let species =
            self.get_symbol_value(dom, constructor, &JsSymbol::well_known("@@species"))?;
        match species {
            JsValue::Undefined | JsValue::Null => Ok(default),
            JsValue::Object(species) if self.is_constructor(species) => Ok(species),
            _ => Err(JsError::type_error("Promise species is not a constructor")),
        }
    }

    /// `Promise.prototype.then` (ECMA-262 27.2.5.4.1).
    fn promise_then(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let Some(ObjectHost::Promise(promise)) = self.realm.host(receiver) else {
            return Err(JsError::type_error("incompatible Promise method receiver"));
        };
        let constructor = self.promise_species_constructor(dom, receiver)?;
        let (result, capability) = self.new_promise_capability(dom, constructor)?;
        let on_fulfilled = callable_argument(arguments.first(), &self.realm);
        let on_rejected = callable_argument(arguments.get(1), &self.realm);
        self.perform_then(promise, on_fulfilled, on_rejected, Some(capability))?;
        Ok(result)
    }

    /// `Invoke(receiver, "then", arguments)`.
    fn invoke_then(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match self.get_member(dom, receiver, "then")? {
            JsValue::Object(then) if Self::is_callable_object(then, &self.realm) => {
                self.call_with_this(dom, then, arguments, JsValue::Object(receiver))
            }
            _ => Err(JsError::type_error("then is not a function")),
        }
    }

    /// `Promise.prototype.finally` (ECMA-262 27.2.5.3).
    fn promise_finally(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let constructor = self.promise_species_constructor(dom, receiver)?;
        let on_finally = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let (then_finally, catch_finally) = match callable_argument(Some(&on_finally), &self.realm)
        {
            Some(callback) => {
                self.ensure_heap_capacity(4)?;
                let bound = vec![JsValue::Object(callback), JsValue::Object(constructor)];
                (
                    JsValue::Object(self.bound_function(
                        NativeFunction::PromiseFinallyPass,
                        bound.clone(),
                        1.0,
                    )),
                    JsValue::Object(self.bound_function(
                        NativeFunction::PromiseFinallyReject,
                        bound,
                        1.0,
                    )),
                )
            }
            None => (on_finally.clone(), on_finally),
        };
        self.invoke_then(dom, receiver, &[then_finally, catch_finally])
    }

    /// `thenFinally` (`rejected` false) and `catchFinally` (`rejected` true).
    /// Bound arguments are `(onFinally, C)`, then the settlement value.
    fn promise_finally_handler(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
        rejected: bool,
    ) -> Result<JsValue, JsError> {
        let (Some(callback), Some(constructor)) =
            (object_argument(arguments, 0), object_argument(arguments, 1))
        else {
            return Ok(JsValue::Undefined);
        };
        let value = arguments.get(2).cloned().unwrap_or(JsValue::Undefined);
        let result = self.call_with_this(dom, callback, &[], JsValue::Undefined)?;
        let promise = self.promise_resolve_with_constructor(dom, constructor, &result)?;
        let JsValue::Object(promise) = promise else {
            return Ok(JsValue::Undefined);
        };
        self.ensure_heap_capacity(1)?;
        let thunk = self.bound_function(
            if rejected {
                NativeFunction::PromiseThrower
            } else {
                NativeFunction::PromiseValueThunk
            },
            vec![value],
            0.0,
        );
        self.invoke_then(dom, promise, &[JsValue::Object(thunk)])
    }

    /// `PromiseResolve(C, value)` (ECMA-262 27.2.4.7.1).
    fn promise_resolve_with_constructor(
        &mut self,
        dom: &mut Dom,
        constructor: ObjectId,
        value: &JsValue,
    ) -> Result<JsValue, JsError> {
        if let JsValue::Object(object) = value
            && matches!(self.realm.host(*object), Some(ObjectHost::Promise(_)))
            && self.get_member(dom, *object, "constructor")? == JsValue::Object(constructor)
        {
            return Ok(value.clone());
        }
        let (promise, capability) = self.new_promise_capability(dom, constructor)?;
        self.capability_resolve(dom, capability, value)?;
        Ok(promise)
    }

    /// `Promise.withResolvers()`.
    fn promise_with_resolvers(
        &mut self,
        dom: &mut Dom,
        constructor: ObjectId,
    ) -> Result<JsValue, JsError> {
        let (promise, capability) = self.new_promise_capability(dom, constructor)?;
        let (resolve, reject) = self.capability_functions(capability)?;
        self.ensure_heap_capacity(1)?;
        let result = self.realm.create_ordinary_object();
        self.realm
            .set_property(result, "promise".to_owned(), promise);
        self.realm
            .set_property(result, "resolve".to_owned(), JsValue::Object(resolve));
        self.realm
            .set_property(result, "reject".to_owned(), JsValue::Object(reject));
        Ok(JsValue::Object(result))
    }

    /// `Promise.try(callback, ...args)`.
    fn promise_try(
        &mut self,
        dom: &mut Dom,
        constructor: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let (promise, capability) = self.new_promise_capability(dom, constructor)?;
        let callback = callable_argument(arguments.first(), &self.realm);
        let rest = arguments.get(1..).unwrap_or(&[]);
        let outcome = match callback {
            Some(callback) => self.call_with_this(dom, callback, rest, JsValue::Undefined),
            None => Err(JsError::type_error(
                "Promise.try callback is not a function",
            )),
        };
        match outcome {
            Ok(value) => self.capability_resolve(dom, capability, &value)?,
            Err(error) if error.kind() == JsErrorKind::ResourceLimit => return Err(error),
            Err(error) => {
                let reason = self.error_value(&error);
                self.capability_reject(dom, capability, &reason)?;
            }
        }
        Ok(promise)
    }

    // ---------------------------------------------------------------- statics

    /// `GetPromiseResolve(C)`.
    fn get_promise_resolve(
        &mut self,
        dom: &mut Dom,
        constructor: ObjectId,
    ) -> Result<ObjectId, JsError> {
        match self.get_member(dom, constructor, "resolve")? {
            JsValue::Object(function) if Self::is_callable_object(function, &self.realm) => {
                Ok(function)
            }
            _ => Err(JsError::type_error("Promise resolve is not a function")),
        }
    }

    /// The combinator entry shared by the two iterable forms' four variants
    /// and the two keyed forms: a non-constructor receiver throws, and every
    /// later failure rejects the returned promise.
    fn promise_combinator(
        &mut self,
        dom: &mut Dom,
        combinator: Combinator,
        constructor: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let (promise, capability) = self.new_promise_capability(dom, constructor)?;
        let iterable = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let pinned = self.transient_roots.len();
        self.pin_value(&promise);
        let outcome = self.drive_combinator(dom, combinator, constructor, capability, &iterable);
        self.transient_roots.truncate(pinned);
        self.settle_combinator_outcome(dom, capability, outcome, promise)
    }

    fn promise_keyed_combinator(
        &mut self,
        dom: &mut Dom,
        combinator: Combinator,
        constructor: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let (promise, capability) = self.new_promise_capability(dom, constructor)?;
        let input = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let pinned = self.transient_roots.len();
        self.pin_value(&promise);
        let outcome = self.drive_keyed_combinator(dom, combinator, constructor, capability, &input);
        self.transient_roots.truncate(pinned);
        self.settle_combinator_outcome(dom, capability, outcome, promise)
    }

    fn settle_combinator_outcome(
        &mut self,
        dom: &mut Dom,
        capability: PromiseCapability,
        outcome: Result<(), JsError>,
        promise: JsValue,
    ) -> Result<JsValue, JsError> {
        if let Err(error) = outcome {
            if error.kind() == JsErrorKind::ResourceLimit {
                return Err(error);
            }
            let reason = self.error_value(&error);
            self.capability_reject(dom, capability, &reason)?;
        }
        Ok(promise)
    }

    fn pin_value(&mut self, value: &JsValue) {
        if let JsValue::Object(object) = value {
            self.transient_roots.push(*object);
        }
    }

    /// `PerformPromiseAll` and its siblings over an iterable. The iterator is
    /// consumed lazily, one element at a time, so each element's `resolve` and
    /// `then` run before the next `next()`. An error while attaching an element
    /// closes the iterator; an error from `next()` itself does not.
    fn drive_combinator(
        &mut self,
        dom: &mut Dom,
        combinator: Combinator,
        constructor: ObjectId,
        capability: PromiseCapability,
        iterable: &JsValue,
    ) -> Result<(), JsError> {
        let promise_resolve = self.get_promise_resolve(dom, constructor)?;
        let (iterator, next) = self.combinator_iterator(dom, iterable)?;
        self.transient_roots
            .extend([iterator, next, promise_resolve, constructor]);
        let store = self.new_combinator_store(combinator, capability, false)?;
        self.transient_roots.push(store);
        let mut index = 0;
        while let Some(value) = self.iterator_next(dom, iterator, next)? {
            if let Err(error) = self.combinator_add_element(
                dom,
                store,
                (promise_resolve, constructor),
                index,
                None,
                value,
            ) {
                if error.kind() != JsErrorKind::ResourceLimit {
                    let _ = self.close_iterator_object(dom, iterator);
                }
                return Err(error);
            }
            index += 1;
        }
        self.finish_combinator_pass(dom, store)
    }

    /// The keyed walk: own enumerable keys, strings then symbols, read once in
    /// order. A key whose descriptor is not enumerable is skipped without its
    /// value being read.
    fn drive_keyed_combinator(
        &mut self,
        dom: &mut Dom,
        combinator: Combinator,
        constructor: ObjectId,
        capability: PromiseCapability,
        input: &JsValue,
    ) -> Result<(), JsError> {
        let promise_resolve = self.get_promise_resolve(dom, constructor)?;
        let JsValue::Object(input) = input else {
            return Err(JsError::type_error(
                "Promise keyed combinator input must be an object",
            ));
        };
        let input = *input;
        self.transient_roots
            .extend([input, promise_resolve, constructor]);
        let keys = self.own_keys_for_combinator(dom, input)?;
        let store = self.new_combinator_store(combinator, capability, true)?;
        self.transient_roots.push(store);
        let mut index = 0;
        for key in keys {
            let enumerable = match &key {
                JsValue::String(name) => self
                    .proxy_get_own_property_descriptor(dom, input, name)?
                    .is_some_and(|descriptor| descriptor.enumerable),
                JsValue::Symbol(symbol) => self
                    .realm
                    .own_symbol_property(input, symbol)
                    .is_some_and(|descriptor| descriptor.enumerable),
                _ => false,
            };
            if !enumerable {
                continue;
            }
            let value = match &key {
                JsValue::String(name) => self.get_member(dom, input, name)?,
                JsValue::Symbol(symbol) => self.get_symbol_value(dom, input, symbol)?,
                _ => JsValue::Undefined,
            };
            self.combinator_add_element(
                dom,
                store,
                (promise_resolve, constructor),
                index,
                Some(key),
                value,
            )?;
            index += 1;
        }
        self.finish_combinator_pass(dom, store)
    }

    /// The own keys a keyed combinator walks: `OwnPropertyKeys` order, strings
    /// first and then symbols.
    fn own_keys_for_combinator(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
    ) -> Result<Vec<JsValue>, JsError> {
        let names = self.proxy_own_keys(dom, object)?;
        let mut keys: Vec<JsValue> = names.into_iter().map(JsValue::String).collect();
        if !matches!(self.realm.host(object), Some(ObjectHost::Proxy { .. })) {
            keys.extend(
                self.realm
                    .own_symbols(object)
                    .unwrap_or_default()
                    .into_iter()
                    .map(JsValue::Symbol),
            );
        }
        Ok(keys)
    }

    fn combinator_iterator(
        &mut self,
        dom: &mut Dom,
        iterable: &JsValue,
    ) -> Result<(ObjectId, ObjectId), JsError> {
        let object = match iterable {
            JsValue::Object(object) => *object,
            JsValue::String(_) => self.to_object(iterable)?,
            _ => {
                return Err(JsError::type_error(format!(
                    "{} is not iterable",
                    describe_iterable(iterable)
                )));
            }
        };
        self.get_iterator(dom, &JsValue::Object(object))?
            .ok_or_else(|| {
                JsError::type_error(format!("{} is not iterable", describe_iterable(iterable)))
            })
    }

    /// Attach one element: call `promiseResolve` on it, count it, and invoke
    /// the `then` of the result with the handlers its combinator needs.
    fn combinator_add_element(
        &mut self,
        dom: &mut Dom,
        store: ObjectId,
        (promise_resolve, constructor): (ObjectId, ObjectId),
        index: usize,
        key: Option<JsValue>,
        value: JsValue,
    ) -> Result<(), JsError> {
        if index >= MAX_COMBINATOR_ELEMENTS {
            return Err(JsError::resource(
                "combinator iterable exceeds the materialization bound",
            ));
        }
        self.ensure_heap_capacity(4)?;
        let next =
            self.call_with_this(dom, promise_resolve, &[value], JsValue::Object(constructor))?;
        let JsValue::Object(next) = next else {
            return Err(JsError::type_error("then is not a function"));
        };
        let combinator = Combinator::from_code(self.store_number(store, store::KIND));
        let slot = match key {
            Some(key) => {
                if let Some(slots) = self.store_object(store, store::KEYS) {
                    self.realm
                        .set_property(slots, index.to_string(), key.clone());
                }
                key
            }
            None => JsValue::String(index.to_string()),
        };
        self.realm.set_property(
            store,
            store::COUNT.to_owned(),
            JsValue::Number((index + 1) as f64),
        );
        if matches!(combinator, Combinator::All | Combinator::AllSettled)
            && let Some(values) = self.store_object(store, store::VALUES)
        {
            self.set_combinator_entry(values, &slot, JsValue::Undefined);
        }
        self.adjust_remaining(store, 1.0);
        let pair = self.new_promise_pair();
        let (on_fulfilled, on_rejected) = match combinator {
            Combinator::All => (
                self.element_function(store, index, pair, ROLE_FULFILLED),
                JsValue::Object(self.store_function(store, store::REJECT)),
            ),
            Combinator::AllSettled => (
                self.element_function(store, index, pair, ROLE_SETTLED_FULFILLED),
                self.element_function(store, index, pair, ROLE_SETTLED_REJECTED),
            ),
            Combinator::Any => (
                JsValue::Object(self.store_function(store, store::RESOLVE)),
                self.element_function(store, index, pair, ROLE_ANY_REJECTED),
            ),
            Combinator::Race => (
                JsValue::Object(self.store_function(store, store::RESOLVE)),
                JsValue::Object(self.store_function(store, store::REJECT)),
            ),
        };
        self.invoke_then(dom, next, &[on_fulfilled, on_rejected])?;
        Ok(())
    }

    /// The sentinel count taken before the walk is released once the walk is
    /// over; a combinator whose elements already settled settles here.
    fn finish_combinator_pass(&mut self, dom: &mut Dom, store: ObjectId) -> Result<(), JsError> {
        if self.adjust_remaining(store, -1.0) == 0.0 {
            self.finish_combinator(dom, store)?;
        }
        Ok(())
    }

    fn finish_combinator(&mut self, dom: &mut Dom, store: ObjectId) -> Result<(), JsError> {
        let combinator = Combinator::from_code(self.store_number(store, store::KIND));
        match combinator {
            // An empty race stays pending, and a non-empty one settles through
            // its own handlers rather than here.
            Combinator::Race => Ok(()),
            Combinator::Any => {
                let errors = self.combinator_values(store, store::ERRORS);
                let reason =
                    self.construct_aggregate_error(dom, &errors, "All promises were rejected")?;
                let reject = self.store_function(store, store::REJECT);
                self.call_with_this(dom, reject, &[reason], JsValue::Undefined)?;
                Ok(())
            }
            Combinator::All | Combinator::AllSettled => {
                let result = if self.is_array_store(store) {
                    let values = self.combinator_values(store, store::VALUES);
                    JsValue::Object(self.create_array_from_values(&values)?)
                } else {
                    match self.store_object(store, store::VALUES) {
                        Some(values) => JsValue::Object(values),
                        None => JsValue::Undefined,
                    }
                };
                let resolve = self.store_function(store, store::RESOLVE);
                self.call_with_this(dom, resolve, &[result], JsValue::Undefined)?;
                Ok(())
            }
        }
    }

    /// One element's settlement function (`resolveElement`, `rejectElement`,
    /// and the `allSettled` pair). Its pair flag stops it running twice.
    fn combinator_element_settled(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let Some(store) = object_argument(arguments, 0) else {
            return Ok(JsValue::Undefined);
        };
        let (Some(index), Some(pair), Some(role)) = (
            number_argument(arguments, 1),
            number_argument(arguments, 2),
            number_argument(arguments, 3),
        ) else {
            return Ok(JsValue::Undefined);
        };
        if !self.claim_promise_pair(pair as usize) {
            return Ok(JsValue::Undefined);
        }
        let value = arguments.get(4).cloned().unwrap_or(JsValue::Undefined);
        self.ensure_heap_capacity(1)?;
        let key = self.combinator_slot_key(store, index as usize);
        if role == ROLE_ANY_REJECTED {
            if let Some(errors) = self.store_object(store, store::ERRORS) {
                self.set_combinator_entry(errors, &key, value);
            }
        } else if let Some(values) = self.store_object(store, store::VALUES) {
            let stored = if role == ROLE_SETTLED_REJECTED {
                JsValue::Object(self.settlement_record(false, value))
            } else if role == ROLE_SETTLED_FULFILLED {
                JsValue::Object(self.settlement_record(true, value))
            } else {
                value
            };
            self.set_combinator_entry(values, &key, stored);
        }
        if self.adjust_remaining(store, -1.0) == 0.0 {
            self.finish_combinator(dom, store)?;
        }
        Ok(JsValue::Undefined)
    }

    fn new_combinator_store(
        &mut self,
        combinator: Combinator,
        capability: PromiseCapability,
        keyed: bool,
    ) -> Result<ObjectId, JsError> {
        let (resolve, reject) = self.capability_functions(capability)?;
        self.ensure_heap_capacity(4)?;
        let store = self.realm.create_ordinary_object();
        let values = self.realm.create_object(None);
        self.realm.set_property(
            store,
            store::KIND.to_owned(),
            JsValue::Number(combinator.code()),
        );
        self.realm
            .set_property(store, store::RESOLVE.to_owned(), JsValue::Object(resolve));
        self.realm
            .set_property(store, store::REJECT.to_owned(), JsValue::Object(reject));
        self.realm
            .set_property(store, store::VALUES.to_owned(), JsValue::Object(values));
        if combinator == Combinator::Any {
            let errors = self.realm.create_object(None);
            self.realm
                .set_property(store, store::ERRORS.to_owned(), JsValue::Object(errors));
        }
        if keyed {
            let slots = self.realm.create_object(None);
            self.realm
                .set_property(store, store::KEYS.to_owned(), JsValue::Object(slots));
        }
        self.realm
            .set_property(store, store::ARRAY.to_owned(), JsValue::Boolean(!keyed));
        // The sentinel holds the walk open until every element is attached.
        self.realm
            .set_property(store, store::REMAINING.to_owned(), JsValue::Number(1.0));
        self.realm
            .set_property(store, store::COUNT.to_owned(), JsValue::Number(0.0));
        Ok(store)
    }

    fn element_function(
        &mut self,
        store: ObjectId,
        index: usize,
        pair: usize,
        role: f64,
    ) -> JsValue {
        JsValue::Object(self.bound_function(
            NativeFunction::PromiseCombinatorElement,
            vec![
                JsValue::Object(store),
                JsValue::Number(index as f64),
                JsValue::Number(pair as f64),
                JsValue::Number(role),
            ],
            1.0,
        ))
    }

    /// A script-visible function that runs `function` with `arguments` bound
    /// ahead of its own. It carries the `length` and the empty `name` that an
    /// anonymous built-in of that arity has.
    fn bound_function(
        &mut self,
        function: NativeFunction,
        arguments: Vec<JsValue>,
        length: f64,
    ) -> ObjectId {
        let target = self.realm.native_object(function);
        let bound = self
            .realm
            .bound_callable(target, JsValue::Undefined, arguments);
        for (key, descriptor) in crate::value::Realm::function_metadata("", length) {
            self.realm.define_property(bound, key, descriptor);
        }
        bound
    }

    fn store_function(&self, store: ObjectId, key: &str) -> ObjectId {
        match self.realm.get_property(store, key) {
            Some(JsValue::Object(function)) => function,
            _ => store,
        }
    }

    fn store_object(&self, store: ObjectId, key: &str) -> Option<ObjectId> {
        match self.realm.get_property(store, key) {
            Some(JsValue::Object(object)) => Some(object),
            _ => None,
        }
    }

    fn store_number(&self, object: ObjectId, key: &str) -> f64 {
        match self.realm.get_property(object, key) {
            Some(JsValue::Number(number)) => number,
            _ => 0.0,
        }
    }

    fn is_array_store(&self, store: ObjectId) -> bool {
        matches!(
            self.realm.get_property(store, store::ARRAY),
            Some(JsValue::Boolean(true))
        )
    }

    /// Move the outstanding-element count by `delta` and return the result.
    fn adjust_remaining(&mut self, store: ObjectId, delta: f64) -> f64 {
        let remaining = self.store_number(store, store::REMAINING) + delta;
        self.realm.set_property(
            store,
            store::REMAINING.to_owned(),
            JsValue::Number(remaining),
        );
        remaining
    }

    /// The property an element settles under: its input key for the keyed
    /// forms, its index otherwise.
    fn combinator_slot_key(&self, store: ObjectId, index: usize) -> JsValue {
        if let Some(slots) = self.store_object(store, store::KEYS)
            && let Some(key) = self.realm.get_property(slots, &index.to_string())
        {
            return key;
        }
        JsValue::String(index.to_string())
    }

    fn set_combinator_entry(&mut self, object: ObjectId, key: &JsValue, value: JsValue) {
        match key {
            JsValue::Symbol(symbol) => {
                self.realm.define_symbol_property(
                    object,
                    symbol,
                    PropertyDescriptor {
                        getter: None,
                        setter: None,
                        value,
                        writable: true,
                        enumerable: true,
                        configurable: true,
                    },
                );
            }
            JsValue::String(name) => {
                self.realm.set_property(object, name.clone(), value);
            }
            _ => {}
        }
    }

    /// The entries `0..count` of a combinator's value or error record, in index
    /// order.
    fn combinator_values(&self, store: ObjectId, key: &str) -> Vec<JsValue> {
        let Some(object) = self.store_object(store, key) else {
            return Vec::new();
        };
        let count = self.store_number(store, store::COUNT) as usize;
        (0..count)
            .map(|index| {
                self.realm
                    .get_property(object, &index.to_string())
                    .unwrap_or(JsValue::Undefined)
            })
            .collect()
    }

    /// One `{status, value}` / `{status, reason}` record for `allSettled`.
    fn settlement_record(&mut self, fulfilled: bool, value: JsValue) -> ObjectId {
        let record = self.realm.create_ordinary_object();
        let (status, key) = if fulfilled {
            ("fulfilled", "value")
        } else {
            ("rejected", "reason")
        };
        self.realm.set_property(
            record,
            "status".to_owned(),
            JsValue::String(status.to_owned()),
        );
        self.realm.set_property(record, key.to_owned(), value);
        record
    }

    /// Build the `AggregateError` a combinator rejects with.
    fn construct_aggregate_error(
        &mut self,
        dom: &mut Dom,
        errors: &[JsValue],
        message: &str,
    ) -> Result<JsValue, JsError> {
        let constructor = self
            .realm
            .global("AggregateError")
            .and_then(|value| match value {
                JsValue::Object(constructor) => Some(constructor),
                _ => None,
            })
            .ok_or_else(|| JsError::type_error("AggregateError is not installed"))?;
        let errors = JsValue::Object(self.create_array_from_values(errors)?);
        self.aggregate_error_constructor(
            dom,
            constructor,
            &[errors, JsValue::String(message.to_owned())],
        )
    }

    /// Drain an iterable into a list. Used by `new AggregateError(errors)`,
    /// which accepts any iterable.
    fn iterate_combinator_source(
        &mut self,
        dom: &mut Dom,
        value: &JsValue,
    ) -> Result<Vec<JsValue>, JsError> {
        match value {
            JsValue::String(text) => {
                return Ok(text
                    .chars()
                    .map(|character| JsValue::String(character.to_string()))
                    .collect());
            }
            JsValue::Object(object)
                if matches!(self.realm.host(*object), Some(ObjectHost::Array)) =>
            {
                return self.array_elements_for(*object);
            }
            _ => {}
        }
        let Some((iterator, next)) = self.get_iterator(dom, value)? else {
            return Err(JsError::type_error(format!(
                "{} is not iterable",
                describe_iterable(value)
            )));
        };
        let mut values = Vec::new();
        loop {
            if values.len() >= MAX_COMBINATOR_ELEMENTS {
                return Err(JsError::resource(
                    "combinator iterable exceeds the materialization bound",
                ));
            }
            match self.iterator_next(dom, iterator, next)? {
                Some(value) => values.push(value),
                None => return Ok(values),
            }
        }
    }

    /// `AggregateError(errors, message)`.
    ///
    /// `errors` is stored as an own property because that is what the
    /// specification does — there is no internal slot and no prototype accessor
    /// — which also means a caller can overwrite it, and `Object.keys` on the
    /// error stays empty because the property is not enumerable.
    pub(in crate::runtime) fn aggregate_error_constructor(
        &mut self,
        dom: &mut Dom,
        constructor: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let prototype = self
            .realm
            .get_property(constructor, "prototype")
            .and_then(|value| match value {
                JsValue::Object(prototype) => Some(prototype),
                _ => None,
            })
            .ok_or_else(|| {
                JsError::type_error("AggregateError constructor prototype is not an object")
            })?;
        // `errors` is required to be iterable: `new AggregateError()` is a
        // TypeError, not an error with no `errors`.
        let iterable = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let errors = self.iterate_combinator_source(dom, &iterable)?;
        let errors = self.create_array_from_values(&errors)?;
        let message = arguments
            .get(1)
            .filter(|value| !matches!(value, JsValue::Undefined))
            .map(JsValue::to_js_string);
        self.ensure_heap_capacity(1)?;
        let object = self.realm.create_error(prototype, message);
        self.realm.define_property(
            object,
            "errors",
            PropertyDescriptor {
                getter: None,
                setter: None,
                value: JsValue::Object(errors),
                writable: true,
                enumerable: false,
                configurable: true,
            },
        );
        let stack = {
            let name = self
                .realm
                .get_property(object, "name")
                .unwrap_or_else(|| JsValue::String("AggregateError".to_owned()))
                .to_js_string();
            let header = match self.realm.get_property(object, "message") {
                Some(JsValue::String(text)) if !text.is_empty() => format!("{name}: {text}"),
                _ => name,
            };
            format!("{header}{}", self.stack_frame_lines())
        };
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
        Ok(JsValue::Object(object))
    }
}

/// The callable object an optional argument names, if it is one. A
/// non-callable handler is ignored, as `then` requires, rather than thrown.
fn callable_argument(value: Option<&JsValue>, realm: &crate::value::Realm) -> Option<ObjectId> {
    match value {
        Some(JsValue::Object(object)) if JsRuntime::is_callable_object(*object, realm) => {
            Some(*object)
        }
        _ => None,
    }
}

fn object_argument(arguments: &[JsValue], index: usize) -> Option<ObjectId> {
    match arguments.get(index) {
        Some(JsValue::Object(object)) => Some(*object),
        _ => None,
    }
}

fn number_argument(arguments: &[JsValue], index: usize) -> Option<f64> {
    match arguments.get(index) {
        Some(JsValue::Number(number)) => Some(*number),
        _ => None,
    }
}

fn describe_iterable(value: &JsValue) -> String {
    match value {
        JsValue::Null => "null".to_owned(),
        JsValue::Undefined => "undefined".to_owned(),
        JsValue::Number(number) => format!("number {number}"),
        JsValue::BigInt(value) => format!("bigint {}", value.to_string_radix(10)),
        JsValue::Boolean(boolean) => format!("boolean {boolean}"),
        JsValue::Symbol(_) => "symbol".to_owned(),
        JsValue::Object(_) => "object".to_owned(),
        JsValue::String(text) => text.clone(),
    }
}
