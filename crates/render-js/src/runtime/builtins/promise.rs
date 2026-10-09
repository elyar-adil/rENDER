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
            NativeFunction::PromiseAll => self.promise_combinator(dom, Combinator::All, arguments),
            NativeFunction::PromiseAllSettled => {
                self.promise_combinator(dom, Combinator::AllSettled, arguments)
            }
            NativeFunction::PromiseAny => self.promise_combinator(dom, Combinator::Any, arguments),
            NativeFunction::PromiseRace => {
                self.promise_combinator(dom, Combinator::Race, arguments)
            }
            NativeFunction::PromiseCombinatorFulfilled => {
                Ok(self.combinator_element_fulfilled(dom, arguments))
            }
            NativeFunction::PromiseCombinatorRejected => {
                Ok(self.combinator_element_rejected(dom, arguments))
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

/// Which of the four static combinators is running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Combinator {
    All,
    AllSettled,
    Any,
    Race,
}

/// Property names of the settlement store a combinator shares between its
/// per-element handlers.
///
/// The store is a plain object rather than a Rust-side record for two reasons.
/// It is traced by the collector as an ordinary object, so no lifetime
/// bookkeeping can leak it; and each element's handler receives it through a
/// bound callable's arguments, which are traced too, so a store stays alive for
/// exactly as long as some handler still needs it.
mod store {
    pub(super) const CAPABILITY: &str = "c";
    pub(super) const RESULTS: &str = "r";
    pub(super) const ERRORS: &str = "e";
    pub(super) const REMAINING: &str = "n";
    pub(super) const SETTLED: &str = "s";
    pub(super) const KIND: &str = "k";
    pub(super) const COUNT: &str = "t";
}

impl JsRuntime {
    /// `Promise.all`, `Promise.allSettled`, `Promise.any`, and `Promise.race`.
    ///
    /// All four share one shape: drain the iterable, then attach a settlement
    /// handler to each element and settle the capability according to a rule
    /// the *number* of settled elements decides. The four empty cases are four
    /// different answers, which is why the rules are not collapsed:
    ///
    /// - `all([])` fulfils with an empty array, because nothing failed.
    /// - `allSettled([])` fulfils with an empty array, because it never rejects.
    /// - `race([])` stays pending forever, because no element can win.
    /// - `any([])` rejects with an `AggregateError` over an empty `errors`,
    ///   because nothing succeeded.
    fn promise_combinator(
        &mut self,
        dom: &mut Dom,
        combinator: Combinator,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let iterable = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let (capability, result) = self.create_promise()?;
        // A source that cannot be iterated, or an iterator that throws, rejects
        // the capability rather than throwing out of the call. The caller still
        // receives a promise, which is what makes `Promise.all(5).catch(..)`
        // work. This is a distinct path from "no element settled": `any` over a
        // throwing iterator rejects with *that* error, not an AggregateError.
        let values = match self.iterate_combinator_source(dom, &iterable) {
            Ok(values) => values,
            Err(error) => {
                let reason = self.rejection_reason(&error)?;
                self.reject_promise(capability, &reason);
                return Ok(result);
            }
        };
        if values.is_empty() {
            match combinator {
                // Nothing can win, so the promise is never settled.
                Combinator::Race => return Ok(result),
                Combinator::Any => {
                    let reason =
                        self.construct_aggregate_error(dom, &[], "All promises were rejected")?;
                    self.reject_promise(capability, &reason);
                }
                Combinator::All | Combinator::AllSettled => {
                    let empty = self.create_array_from_values(&[])?;
                    self.resolve_promise(capability, &JsValue::Object(empty));
                }
            }
            return Ok(result);
        }
        let count = values.len();
        let results = self.create_array_from_values(&vec![JsValue::Undefined; count])?;
        let errors = match combinator {
            Combinator::Any => {
                Some(self.create_array_from_values(&vec![JsValue::Undefined; count])?)
            }
            _ => None,
        };
        let store = self.new_combinator_store(combinator, capability, results, errors, count);
        for (index, element) in values.into_iter().enumerate() {
            let on_fulfilled = self.combinator_handler(store, index, true);
            let on_rejected = self.combinator_handler(store, index, false);
            // The specification calls `then` on the element directly rather than
            // wrapping it in a promise first, so a thenable that is not a
            // promise is adopted by calling its own `then`. An element with no
            // callable `then` is already fulfilled with its own value.
            match self.callable_member(dom, &element, "then")? {
                Some(then) => {
                    self.call_with_this(dom, then, &[on_fulfilled, on_rejected], element.clone())?;
                }
                None => {
                    self.combinator_element_fulfilled(
                        dom,
                        &[
                            JsValue::Object(store),
                            JsValue::Number(index as f64),
                            element,
                        ],
                    );
                }
            }
        }
        Ok(result)
    }

    /// A member of `value`, but only when it is callable.
    fn callable_member(
        &mut self,
        dom: &mut Dom,
        value: &JsValue,
        name: &str,
    ) -> Result<Option<ObjectId>, JsError> {
        let JsValue::Object(object) = value else {
            return Ok(None);
        };
        let member = self.get_member(dom, *object, name)?;
        match member {
            JsValue::Object(function) if Self::is_callable_object(function, &self.realm) => {
                Ok(Some(function))
            }
            _ => Ok(None),
        }
    }

    /// Drain the combinator's iterable, requiring a real `@@iterator`.
    ///
    /// `iterate_values` deliberately falls back to an array-like `length`, which
    /// is right for `for...of` in this engine but wrong here: the specification
    /// rejects a plain object for a combinator, and silently accepting one would
    /// let a bundle's `Promise.all(responseLikeObject)` appear to work.
    fn iterate_combinator_source(
        &mut self,
        dom: &mut Dom,
        value: &JsValue,
    ) -> Result<Vec<JsValue>, JsError> {
        match value {
            // A string is iterable and walks code points, so `Promise.all("a\u{1F600}")`
            // sees two elements.
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

    /// One element of a combinator fulfilled. Bound arguments are
    /// `(store, index)`, so the settlement value arrives last.
    ///
    /// Returns `undefined` because a settlement handler's own value is
    /// discarded: the outcome goes to the capability, never to the chain that
    /// happened to be reading this element.
    fn combinator_element_fulfilled(&mut self, _dom: &mut Dom, arguments: &[JsValue]) -> JsValue {
        let value = arguments.get(2).cloned().unwrap_or(JsValue::Undefined);
        let Some(store) = Self::combinator_store_of(arguments) else {
            return JsValue::Undefined;
        };
        let index = Self::combinator_index_of(arguments);
        let kind = self.combinator_kind_of(store);
        match kind {
            Combinator::All => {
                self.record_combinator_result(store, index, value.clone());
            }
            // `race` settles on whichever element settles *first*, so it does
            // not wait for the rest. The settle flag keeps the second and later
            // settlements from overwriting it.
            Combinator::Race => {
                self.record_combinator_result(store, index, value.clone());
                if self.combinator_settle(store) {
                    self.resolve_combinator(store, &value);
                }
                return JsValue::Undefined;
            }
            Combinator::AllSettled => {
                let record = self.settlement_record("fulfilled", Some(value.clone()), None);
                self.record_combinator_result(store, index, JsValue::Object(record));
            }
            // `any` resolves on the first fulfilment and ignores it afterwards.
            Combinator::Any => {
                if self.combinator_settle(store) {
                    self.resolve_combinator(store, &value);
                }
                return JsValue::Undefined;
            }
        }
        // `all` and `allSettled` fulfil once every element has.
        if self.combinator_remaining(store) == 0
            && let Some(results) = self.combinator_results(store)
        {
            self.resolve_combinator(store, &JsValue::Object(results));
        }
        JsValue::Undefined
    }

    /// One `{status, value}` / `{status, reason}` record for `allSettled`.
    fn settlement_record(
        &mut self,
        status: &str,
        value: Option<JsValue>,
        reason: Option<JsValue>,
    ) -> ObjectId {
        let record = self.realm.create_ordinary_object();
        self.realm.set_property(
            record,
            "status".to_owned(),
            JsValue::String(status.to_owned()),
        );
        if let Some(value) = value {
            self.realm.set_property(record, "value".to_owned(), value);
        }
        if let Some(reason) = reason {
            self.realm.set_property(record, "reason".to_owned(), reason);
        }
        record
    }

    /// One element of a combinator rejected.
    fn combinator_element_rejected(&mut self, dom: &mut Dom, arguments: &[JsValue]) -> JsValue {
        let reason = arguments.get(2).cloned().unwrap_or(JsValue::Undefined);
        let Some(store) = Self::combinator_store_of(arguments) else {
            return JsValue::Undefined;
        };
        let index = Self::combinator_index_of(arguments);
        let kind = self.combinator_kind_of(store);
        if kind == Combinator::AllSettled {
            let record = self.settlement_record("rejected", None, Some(reason.clone()));
            self.record_combinator_result(store, index, JsValue::Object(record));
            if self.combinator_remaining(store) == 0
                && let Some(results) = self.combinator_results(store)
            {
                self.resolve_combinator(store, &JsValue::Object(results));
            }
            return JsValue::Undefined;
        }
        match kind {
            // `all` rejects on the *first* rejection and then ignores the rest,
            // but every element keeps its handler attached, so none is left
            // pending and none reports an unhandled rejection later.
            Combinator::All | Combinator::Race => {
                if self.combinator_settle(store) {
                    self.reject_combinator(store, &reason);
                }
            }
            // `allSettled` returned above: it fulfils rather than rejecting.
            Combinator::AllSettled => {}
            // `any` records every reason and only reports once nothing is left.
            Combinator::Any => {
                let count = self.combinator_count(store);
                self.record_combinator_error(store, index, reason);
                if self.combinator_remaining(store) == 0 && self.combinator_settle(store) {
                    let errors = self.combinator_error_values(store, count);
                    // Building the aggregate error is the one place a failure
                    // could escape; fall back to a plain rejection reason so a
                    // rejected capability is never left pending.
                    let reason = self
                        .construct_aggregate_error(dom, &errors, "All promises were rejected")
                        .unwrap_or_else(|_| {
                            JsValue::String("All promises were rejected".to_owned())
                        });
                    self.reject_combinator(store, &reason);
                }
            }
        }
        JsValue::Undefined
    }

    fn combinator_store_of(arguments: &[JsValue]) -> Option<ObjectId> {
        match arguments.first() {
            Some(JsValue::Object(store)) => Some(*store),
            _ => None,
        }
    }

    fn combinator_index_of(arguments: &[JsValue]) -> usize {
        match arguments.get(1) {
            Some(JsValue::Number(index)) => *index as usize,
            _ => 0,
        }
    }

    /// The combinator that owns `store`, recovered from the store itself so the
    /// handler needs no second piece of state.
    fn combinator_kind_of(&self, store: ObjectId) -> Combinator {
        match self.realm.get_property(store, store::KIND) {
            Some(JsValue::Number(kind)) => match kind as i32 {
                0 => Combinator::All,
                1 => Combinator::AllSettled,
                2 => Combinator::Any,
                _ => Combinator::Race,
            },
            _ => Combinator::Race,
        }
    }

    fn combinator_results(&self, store: ObjectId) -> Option<ObjectId> {
        store_member(&self.realm, store, store::RESULTS)
    }

    /// Record one settled element and decrement the outstanding count. The
    /// count is what decides when the capability may settle, and it is why the
    /// four combinators differ only in the rule they apply to it.
    fn record_combinator_result(&mut self, store: ObjectId, index: usize, value: JsValue) {
        if let Some(results) = self.combinator_results(store) {
            self.realm.set_property(results, index.to_string(), value);
        }
        self.decrement_combinator(store);
    }

    fn record_combinator_error(&mut self, store: ObjectId, index: usize, reason: JsValue) {
        if let Some(errors) = store_member(&self.realm, store, store::ERRORS) {
            self.realm.set_property(errors, index.to_string(), reason);
        }
        self.decrement_combinator(store);
    }

    fn decrement_combinator(&mut self, store: ObjectId) {
        let remaining = self.combinator_remaining(store);
        self.realm.set_property(
            store,
            store::REMAINING.to_owned(),
            JsValue::Number(remaining.saturating_sub(1) as f64),
        );
    }

    /// How many elements the combinator started with, which is what the
    /// `errors` array is sized by.
    fn combinator_count(&self, store: ObjectId) -> usize {
        let Some(JsValue::Number(count)) = self.realm.get_property(store, store::COUNT) else {
            return 0;
        };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let count = count.max(0.0);
        count as usize
    }

    /// How many elements have not settled yet.
    fn combinator_remaining(&self, store: ObjectId) -> usize {
        let Some(JsValue::Number(remaining)) = self.realm.get_property(store, store::REMAINING)
        else {
            return 0;
        };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let remaining = remaining.max(0.0);
        remaining as usize
    }

    /// Claim the right to settle the capability. Returns false when it has
    /// already been settled, which is what makes `race` settle once and stops a
    /// late rejection from overwriting `all`'s first rejection.
    fn combinator_settle(&mut self, store: ObjectId) -> bool {
        if self
            .realm
            .get_property(store, store::SETTLED)
            .as_ref()
            .is_some_and(JsValue::is_truthy)
        {
            return false;
        }
        self.realm
            .set_property(store, store::SETTLED.to_owned(), JsValue::Boolean(true));
        true
    }

    fn resolve_combinator(&mut self, store: ObjectId, value: &JsValue) {
        let Some(capability) = self.combinator_capability(store) else {
            return;
        };
        // The capability cannot resolve to itself, so a failure here would mean
        // the store's promise object was dropped; there is no handler to report
        // it to and no way to recover the capability.
        let _ = self.resolve_promise_value(capability, value);
    }

    fn reject_combinator(&mut self, store: ObjectId, reason: &JsValue) {
        let Some(capability) = self.combinator_capability(store) else {
            return;
        };
        self.reject_promise(capability, reason);
    }

    fn combinator_capability(&self, store: ObjectId) -> Option<usize> {
        match self.realm.get_property(store, store::CAPABILITY) {
            Some(JsValue::Object(promise)) => match self.realm.host(promise) {
                Some(ObjectHost::Promise(index)) => Some(index),
                _ => None,
            },
            _ => None,
        }
    }

    /// Build the `AggregateError` a combinator rejects with.
    ///
    /// `errors` is read back out of the store's error array by index, so the
    /// reasons appear in the iterable's order rather than in the order the
    /// rejections happened to arrive.
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

    /// The rejection reasons recorded so far, in index order.
    fn combinator_error_values(&mut self, store: ObjectId, count: usize) -> Vec<JsValue> {
        let Some(errors) = store_member(&self.realm, store, store::ERRORS) else {
            return Vec::new();
        };
        (0..count)
            .map(|index| {
                self.realm
                    .get_property(errors, &index.to_string())
                    .unwrap_or(JsValue::Undefined)
            })
            .collect()
    }
}

/// One object-valued member of the settlement store.
fn store_member(realm: &crate::value::Realm, store: ObjectId, key: &str) -> Option<ObjectId> {
    match realm.get_property(store, key) {
        Some(JsValue::Object(member)) => Some(member),
        _ => None,
    }
}

impl JsRuntime {
    /// One per-element settlement handler for a combinator.
    ///
    /// The handler is a bound callable carrying `(store, index)`, which is what
    /// makes the store reachable from the collector's point of view for exactly
    /// as long as any element can still call it.
    fn combinator_handler(&mut self, store: ObjectId, index: usize, fulfilled: bool) -> JsValue {
        let target = self.realm.native_object(if fulfilled {
            NativeFunction::PromiseCombinatorFulfilled
        } else {
            NativeFunction::PromiseCombinatorRejected
        });
        let handler = self.realm.bound_callable(
            target,
            JsValue::Undefined,
            vec![JsValue::Object(store), JsValue::Number(index as f64)],
        );
        JsValue::Object(handler)
    }

    fn new_combinator_store(
        &mut self,
        kind: Combinator,
        capability: usize,
        results: ObjectId,
        errors: Option<ObjectId>,
        count: usize,
    ) -> ObjectId {
        self.ensure_heap_capacity(1)
            .expect("a combinator store is one object against a checked budget");
        let store = self.realm.create_ordinary_object();
        self.realm.set_property(
            store,
            store::KIND.to_owned(),
            JsValue::Number(match kind {
                Combinator::All => 0.0,
                Combinator::AllSettled => 1.0,
                Combinator::Any => 2.0,
                Combinator::Race => 3.0,
            }),
        );
        let capability_object = self.realm.promise(capability);
        self.realm.set_property(
            store,
            store::CAPABILITY.to_owned(),
            JsValue::Object(capability_object),
        );
        self.realm
            .set_property(store, store::RESULTS.to_owned(), JsValue::Object(results));
        if let Some(errors) = errors {
            self.realm
                .set_property(store, store::ERRORS.to_owned(), JsValue::Object(errors));
        }
        self.realm.set_property(
            store,
            store::REMAINING.to_owned(),
            JsValue::Number(count as f64),
        );
        self.realm.set_property(
            store,
            store::COUNT.to_owned(),
            JsValue::Number(count as f64),
        );
        self.realm
            .set_property(store, store::SETTLED.to_owned(), JsValue::Boolean(false));
        store
    }
}

impl JsRuntime {
    /// The value a caught `JsError` arrives as, used as a rejection reason.
    ///
    /// A native engine error has to materialise as a standard error instance so
    /// `instanceof TypeError` and `error.stack` behave inside a `.catch`
    /// handler exactly as they do inside a `catch` block.
    fn rejection_reason(&mut self, error: &JsError) -> Result<JsValue, JsError> {
        if let Some(value) = error.thrown_value().cloned() {
            return Ok(value);
        }
        let kind = match error.kind() {
            crate::JsErrorKind::Syntax => crate::value::ErrorKind::SyntaxError,
            crate::JsErrorKind::Reference => crate::value::ErrorKind::ReferenceError,
            crate::JsErrorKind::Type => crate::value::ErrorKind::TypeError,
            crate::JsErrorKind::ResourceLimit => crate::value::ErrorKind::RangeError,
            _ => crate::value::ErrorKind::Error,
        };
        self.construct_standard_error(kind, error.message())
    }
}

/// Bound on the elements one combinator will materialise, so a runaway or
/// adversarial iterable cannot exhaust the heap before any handler is attached.
const MAX_COMBINATOR_ELEMENTS: usize = 1 << 20;

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

impl JsRuntime {
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
