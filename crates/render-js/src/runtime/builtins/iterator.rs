#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::match_same_arms,
    clippy::single_match_else,
    clippy::too_many_lines
)]

//! Iterator helpers (ES2025 `%IteratorPrototype%` methods), the abstract
//! `Iterator` constructor, `Iterator.from`, `Iterator.concat`, and the
//! `chunks`, `windows`, `includes` and `join` methods.
//!
//! A helper object is a generator-like state machine. Its slots live in the
//! host object; `next` and `return` copy them out as a [`HelperState`], run one
//! resumption, and write them back. `running` is the generator "executing"
//! state (re-entry throws a `TypeError`) and `done` is "completed".

use crate::JsError;
use crate::JsSymbol;
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::runtime::convert::same_value_zero;
use crate::value::IteratorHelperKind;
use crate::value::NativeFunction;
use crate::value::ObjectHost;
use render_dom::Dom;

/// `2**53 - 1`: the largest count a `take`/`drop`/`includes` argument may name.
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
/// `2**32 - 1`: the largest `chunks`/`windows` size.
const MAX_CHUNK_SIZE: f64 = 4_294_967_295.0;
/// Stands for `+∞` in a remaining count, which is never decremented.
const UNBOUNDED: u64 = u64::MAX;

/// An Iterator Record: the iterator object and its `next` method. `next` is
/// `None` when the property was not callable. That is an error only when the
/// record is stepped (`GetIteratorDirect` reads `next` but does not check it).
#[derive(Clone, Copy)]
struct Record {
    iterator: ObjectId,
    next: Option<ObjectId>,
}

/// A helper's slots, copied out of its host object while it runs.
#[derive(Clone)]
struct HelperState {
    kind: IteratorHelperKind,
    source: Option<ObjectId>,
    source_next: Option<ObjectId>,
    callback: Option<ObjectId>,
    inner: Option<ObjectId>,
    inner_next: Option<ObjectId>,
    counter: u64,
    buffer: Vec<JsValue>,
    done: bool,
    running: bool,
}

impl HelperState {
    fn source_record(&self) -> Option<Record> {
        self.source.map(|iterator| Record {
            iterator,
            next: self.source_next,
        })
    }

    /// Drop every iterator reference once the helper can no longer step.
    fn finish(&mut self) {
        self.done = true;
        self.source = None;
        self.source_next = None;
        self.inner = None;
        self.inner_next = None;
        self.buffer = Vec::new();
    }
}

/// The argument at `index`, or `undefined` when it is absent.
fn argument(arguments: &[JsValue], index: usize) -> &JsValue {
    arguments.get(index).unwrap_or(&JsValue::Undefined)
}

/// Whether `function` is an Iterator method that reads `this` unchanged. Such a
/// method throws a `TypeError` for a primitive or `undefined` receiver instead of
/// receiving a wrapper object.
pub(in crate::runtime) fn reads_receiver_unchanged(function: NativeFunction) -> bool {
    use NativeFunction as N;
    matches!(
        function,
        N::IteratorMap
            | N::IteratorFilter
            | N::IteratorTake
            | N::IteratorDrop
            | N::IteratorFlatMap
            | N::IteratorReduce
            | N::IteratorToArray
            | N::IteratorForEach
            | N::IteratorSome
            | N::IteratorEvery
            | N::IteratorFind
            | N::IteratorIncludes
            | N::IteratorJoin
            | N::IteratorChunks
            | N::IteratorWindows
            | N::IteratorHelperNext
            | N::IteratorHelperReturn
            | N::IteratorWrapNext
            | N::IteratorWrapReturn
    )
}

/// Which short-circuiting predicate `some`/`every`/`find` evaluates.
#[derive(Clone, Copy)]
enum Search {
    Some,
    Every,
    Find,
}

impl JsRuntime {
    /// Dispatch entry for the iterator native functions; returns `None` when
    /// `function` is not one of them.
    pub(in crate::runtime) fn dispatch_iterator_native(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Option<Result<JsValue, JsError>> {
        use NativeFunction as N;
        let result = match function {
            N::IteratorConstructor => Err(JsError::type_error("Iterator is abstract")),
            N::IteratorFrom => self.iterator_from(dom, argument(arguments, 0)),
            N::IteratorConcat => self.iterator_concat(dom, arguments),
            N::IteratorPrototypeIterator => Ok(JsValue::Object(receiver)),
            N::IteratorHelperNext => self.iterator_helper_next(dom, receiver),
            N::IteratorHelperReturn => self.iterator_helper_return(dom, receiver),
            N::IteratorWrapNext => self.iterator_wrap_next(dom, receiver),
            N::IteratorWrapReturn => self.iterator_wrap_return(dom, receiver),
            N::IteratorMap => self.iterator_map(dom, receiver, arguments),
            N::IteratorFilter => self.iterator_filter(dom, receiver, arguments),
            N::IteratorTake => self.iterator_take(dom, receiver, arguments),
            N::IteratorDrop => self.iterator_drop(dom, receiver, arguments),
            N::IteratorFlatMap => self.iterator_flat_map(dom, receiver, arguments),
            N::IteratorReduce => self.iterator_reduce(dom, receiver, arguments),
            N::IteratorToArray => self.iterator_to_array(dom, receiver),
            N::IteratorForEach => self.iterator_for_each(dom, receiver, arguments),
            N::IteratorSome => self.iterator_search(dom, receiver, arguments, Search::Some),
            N::IteratorEvery => self.iterator_search(dom, receiver, arguments, Search::Every),
            N::IteratorFind => self.iterator_search(dom, receiver, arguments, Search::Find),
            N::IteratorIncludes => self.iterator_includes(dom, receiver, arguments),
            N::IteratorJoin => self.iterator_join(dom, receiver, arguments),
            N::IteratorChunks => self.iterator_chunks(dom, receiver, arguments),
            N::IteratorWindows => self.iterator_windows(dom, receiver, arguments),
            _ => return None,
        };
        Some(result)
    }

    // ----- Iterator Records and closing -------------------------------------

    /// `GetIteratorDirect(iterator)`: `next` is read once and checked when called.
    fn iterator_direct(&mut self, dom: &mut Dom, iterator: ObjectId) -> Result<Record, JsError> {
        let next = self.get_member(dom, iterator, "next")?;
        Ok(Record {
            iterator,
            next: self.callable_object(&next),
        })
    }

    fn callable_object(&self, value: &JsValue) -> Option<ObjectId> {
        match value {
            JsValue::Object(object) if Self::is_callable_object(*object, &self.realm) => {
                Some(*object)
            }
            _ => None,
        }
    }

    /// `IteratorStep` (`want_value` false) and `IteratorStepValue`. `Ok(None)`
    /// means the iterator reported done.
    fn step_record(
        &mut self,
        dom: &mut Dom,
        record: Record,
        want_value: bool,
    ) -> Result<Option<JsValue>, JsError> {
        let next = record
            .next
            .ok_or_else(|| JsError::type_error("iterator next method is not callable"))?;
        let result = self.call_with_this(dom, next, &[], JsValue::Object(record.iterator))?;
        let JsValue::Object(result) = result else {
            return Err(JsError::type_error("iterator result is not an object"));
        };
        if self.get_member(dom, result, "done")?.is_truthy() {
            return Ok(None);
        }
        if want_value {
            self.get_member(dom, result, "value").map(Some)
        } else {
            Ok(Some(JsValue::Undefined))
        }
    }

    /// `GetMethod(iterator, "return")` followed by the call. `Ok(None)` when the
    /// iterator has no `return` method.
    fn call_return_method(
        &mut self,
        dom: &mut Dom,
        iterator: ObjectId,
    ) -> Result<Option<JsValue>, JsError> {
        let method = self.get_member(dom, iterator, "return")?;
        if matches!(method, JsValue::Undefined | JsValue::Null) {
            return Ok(None);
        }
        let method = self
            .callable_object(&method)
            .ok_or_else(|| JsError::type_error("iterator return method is not callable"))?;
        self.call_with_this(dom, method, &[], JsValue::Object(iterator))
            .map(Some)
    }

    /// `IteratorClose(record, completion)`. A throw completion is returned
    /// unchanged and the outcome of `return` is discarded. A normal completion
    /// propagates errors from `return` and requires an object result.
    fn iterator_close<T>(
        &mut self,
        dom: &mut Dom,
        iterator: ObjectId,
        completion: Result<T, JsError>,
    ) -> Result<T, JsError> {
        match completion {
            Err(error) => Err(self.close_on_throw(dom, iterator, error)),
            Ok(value) => match self.call_return_method(dom, iterator)? {
                Some(JsValue::Object(_)) | None => Ok(value),
                Some(_) => Err(JsError::type_error(
                    "iterator return result is not an object",
                )),
            },
        }
    }

    /// `IteratorClose` for a throw completion: run `return`, discard its
    /// outcome, and hand back the original error.
    fn close_on_throw(&mut self, dom: &mut Dom, iterator: ObjectId, error: JsError) -> JsError {
        let _ = self.call_return_method(dom, iterator);
        error
    }

    /// `IfAbruptCloseIterator`: close `iterator` when `result` is a throw.
    fn close_on_error<T>(
        &mut self,
        dom: &mut Dom,
        iterator: ObjectId,
        result: Result<T, JsError>,
    ) -> Result<T, JsError> {
        match result {
            Ok(value) => Ok(value),
            Err(error) => self.iterator_close(dom, iterator, Err(error)),
        }
    }

    /// The callback argument of a method. A non-callable value closes the
    /// receiver and throws a `TypeError`, before `next` is read.
    fn require_callback(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        value: &JsValue,
    ) -> Result<ObjectId, JsError> {
        if let Some(callback) = self.callable_object(value) {
            return Ok(callback);
        }
        let error = JsError::type_error("iterator helper callback is not callable");
        self.iterator_close(dom, receiver, Err(error))
    }

    /// `GetIteratorFlattenable(value, reject-primitives | iterate-string-primitives)`.
    fn get_iterator_flattenable(
        &mut self,
        dom: &mut Dom,
        value: &JsValue,
        allow_strings: bool,
    ) -> Result<Record, JsError> {
        let object = match value {
            JsValue::Object(object) => *object,
            JsValue::String(_) if allow_strings => {
                self.coerce_member_base(value, "Iterator.from")?
            }
            _ => {
                return Err(JsError::type_error(
                    "iterator helper value is not an object",
                ));
            }
        };
        let method = self.get_symbol_value(dom, object, &JsSymbol::well_known("@@iterator"))?;
        let iterator = match method {
            JsValue::Undefined | JsValue::Null => object,
            method => {
                let method = self
                    .callable_object(&method)
                    .ok_or_else(|| JsError::type_error("Symbol.iterator is not callable"))?;
                match self.call_with_this(dom, method, &[], JsValue::Object(object))? {
                    JsValue::Object(iterator) => iterator,
                    _ => {
                        return Err(JsError::type_error("iterator is not an object"));
                    }
                }
            }
        };
        self.iterator_direct(dom, iterator)
    }

    // ----- Argument validation ----------------------------------------------

    /// The `take`/`drop` limit: `ToNumber`, then NaN, a finite value above
    /// `2**53 - 1`, or a negative integer is a `RangeError` that closes the
    /// receiver. `+∞` is allowed and never runs out.
    fn limit_argument(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        value: &JsValue,
    ) -> Result<u64, JsError> {
        let number = self.to_number_value(dom, value);
        let number = self.close_on_error(dom, receiver, number)?;
        if number.is_nan()
            || (number.is_finite() && number > MAX_SAFE_INTEGER)
            || number.trunc() < 0.0
        {
            let error = self.range_error("iterator helper limit is out of range");
            return self.iterator_close(dom, receiver, Err(error));
        }
        Ok(if number.is_infinite() {
            UNBOUNDED
        } else {
            number.trunc() as u64
        })
    }

    /// The `chunks`/`windows` size: an integer Number in `[1, 2**32 - 1]`.
    /// Nothing is coerced. A non-Number, NaN or infinite or non-integral value is
    /// a `TypeError`, and an out-of-range integer is a `RangeError`. Either closes
    /// the receiver.
    fn size_argument(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        value: &JsValue,
    ) -> Result<u64, JsError> {
        let error = match value {
            JsValue::Number(number) if number.is_finite() && number.fract() == 0.0 => {
                if *number >= 1.0 && *number <= MAX_CHUNK_SIZE {
                    return Ok(*number as u64);
                }
                self.range_error("iterator helper size is out of range")
            }
            _ => JsError::type_error("iterator helper size must be an integer"),
        };
        self.iterator_close(dom, receiver, Err(error))
    }

    /// The `includes` skip count: `undefined` means 0. Nothing is coerced. A
    /// non-Number, NaN or non-integral value is a `TypeError`, and a negative or
    /// above-`2**53 - 1` (finite) value is a `RangeError`. Either closes the
    /// receiver. `+∞` is allowed.
    fn skip_argument(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        value: &JsValue,
    ) -> Result<f64, JsError> {
        match value {
            JsValue::Number(number)
                if !number.is_nan() && (number.is_infinite() || number.fract() == 0.0) =>
            {
                let number = *number;
                if number < 0.0 || (number.is_finite() && number > MAX_SAFE_INTEGER) {
                    let error = self.range_error("iterator helper skip count is out of range");
                    return self.iterator_close(dom, receiver, Err(error));
                }
                Ok(number)
            }
            _ => {
                let error = JsError::type_error("iterator helper skip count must be an integer");
                self.iterator_close(dom, receiver, Err(error))
            }
        }
    }

    /// The `windows` undersized mode: `undefined` is "only-full", and the strings
    /// "only-full" and "allow-partial" are accepted. Anything else closes the
    /// receiver and throws a `TypeError`. Returns `true` for "allow-partial".
    fn undersized_argument(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        value: &JsValue,
    ) -> Result<bool, JsError> {
        match value {
            JsValue::Undefined => Ok(false),
            JsValue::String(mode) if mode == "only-full" => Ok(false),
            JsValue::String(mode) if mode == "allow-partial" => Ok(true),
            _ => {
                let error = JsError::type_error("windows undersized must be a known mode");
                self.iterator_close(dom, receiver, Err(error))
            }
        }
    }

    // ----- Helper object storage --------------------------------------------

    fn create_helper(
        &mut self,
        kind: IteratorHelperKind,
        record: Record,
        callback: Option<ObjectId>,
        counter: u64,
    ) -> Result<JsValue, JsError> {
        self.ensure_heap_capacity(1)?;
        Ok(JsValue::Object(self.realm.iterator_helper(
            kind,
            Some(record.iterator),
            record.next,
            callback,
            counter,
        )))
    }

    fn helper_snapshot(&self, receiver: ObjectId) -> Option<HelperState> {
        match self.realm.host(receiver) {
            Some(ObjectHost::IteratorHelper {
                kind,
                source,
                source_next,
                callback,
                inner,
                inner_next,
                counter,
                buffer,
                done,
                running,
            }) => Some(HelperState {
                kind,
                source,
                source_next,
                callback,
                inner,
                inner_next,
                counter,
                buffer,
                done,
                running,
            }),
            _ => None,
        }
    }

    fn helper_store(&mut self, receiver: ObjectId, state: &HelperState) {
        if let Some(ObjectHost::IteratorHelper {
            source,
            source_next,
            callback,
            inner,
            inner_next,
            counter,
            buffer,
            done,
            running,
            ..
        }) = self.realm.host_mut(receiver)
        {
            *source = state.source;
            *source_next = state.source_next;
            *callback = state.callback;
            *inner = state.inner;
            *inner_next = state.inner_next;
            *counter = state.counter;
            buffer.clone_from(&state.buffer);
            *done = state.done;
            *running = state.running;
        }
    }

    // ----- Iterator.prototype methods ---------------------------------------

    fn iterator_map(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let callback = self.require_callback(dom, receiver, argument(arguments, 0))?;
        let record = self.iterator_direct(dom, receiver)?;
        self.create_helper(IteratorHelperKind::Map, record, Some(callback), 0)
    }

    fn iterator_filter(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let callback = self.require_callback(dom, receiver, argument(arguments, 0))?;
        let record = self.iterator_direct(dom, receiver)?;
        self.create_helper(IteratorHelperKind::Filter, record, Some(callback), 0)
    }

    fn iterator_flat_map(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let callback = self.require_callback(dom, receiver, argument(arguments, 0))?;
        let record = self.iterator_direct(dom, receiver)?;
        self.create_helper(IteratorHelperKind::FlatMap, record, Some(callback), 0)
    }

    fn iterator_take(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let remaining = self.limit_argument(dom, receiver, argument(arguments, 0))?;
        let record = self.iterator_direct(dom, receiver)?;
        self.create_helper(IteratorHelperKind::Take, record, None, remaining)
    }

    fn iterator_drop(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let skip = self.limit_argument(dom, receiver, argument(arguments, 0))?;
        let record = self.iterator_direct(dom, receiver)?;
        self.create_helper(IteratorHelperKind::Drop, record, None, skip)
    }

    fn iterator_reduce(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let callback = self.require_callback(dom, receiver, argument(arguments, 0))?;
        let record = self.iterator_direct(dom, receiver)?;
        let (mut accumulator, mut counter) = if arguments.len() >= 2 {
            (arguments[1].clone(), 0.0)
        } else {
            match self.step_record(dom, record, true)? {
                Some(value) => (value, 1.0),
                None => {
                    return Err(JsError::type_error(
                        "Reduce of empty iterator with no initial value",
                    ));
                }
            }
        };
        while let Some(value) = self.step_record(dom, record, true)? {
            let result = self.call(
                dom,
                callback,
                &[accumulator, value, JsValue::Number(counter)],
            );
            accumulator = self.close_on_error(dom, record.iterator, result)?;
            counter += 1.0;
        }
        Ok(accumulator)
    }

    fn iterator_to_array(&mut self, dom: &mut Dom, receiver: ObjectId) -> Result<JsValue, JsError> {
        let record = self.iterator_direct(dom, receiver)?;
        let mut values = Vec::new();
        while let Some(value) = self.step_record(dom, record, true)? {
            values.push(value);
        }
        self.create_array_from_values(&values).map(JsValue::Object)
    }

    fn iterator_for_each(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let callback = self.require_callback(dom, receiver, argument(arguments, 0))?;
        let record = self.iterator_direct(dom, receiver)?;
        let mut counter = 0.0;
        while let Some(value) = self.step_record(dom, record, true)? {
            let result = self.call(dom, callback, &[value, JsValue::Number(counter)]);
            self.close_on_error(dom, record.iterator, result)?;
            counter += 1.0;
        }
        Ok(JsValue::Undefined)
    }

    fn iterator_search(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
        kind: Search,
    ) -> Result<JsValue, JsError> {
        let predicate = self.require_callback(dom, receiver, argument(arguments, 0))?;
        let record = self.iterator_direct(dom, receiver)?;
        let mut counter = 0.0;
        while let Some(value) = self.step_record(dom, record, true)? {
            let result = self.call(dom, predicate, &[value.clone(), JsValue::Number(counter)]);
            let matched = self
                .close_on_error(dom, record.iterator, result)?
                .is_truthy();
            counter += 1.0;
            match kind {
                Search::Some if matched => {
                    return self.iterator_close(dom, record.iterator, Ok(JsValue::Boolean(true)));
                }
                Search::Every if !matched => {
                    return self.iterator_close(dom, record.iterator, Ok(JsValue::Boolean(false)));
                }
                Search::Find if matched => {
                    return self.iterator_close(dom, record.iterator, Ok(value));
                }
                _ => {}
            }
        }
        Ok(match kind {
            Search::Some => JsValue::Boolean(false),
            Search::Every => JsValue::Boolean(true),
            Search::Find => JsValue::Undefined,
        })
    }

    fn iterator_includes(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let search = argument(arguments, 0).clone();
        let mut skipped = match arguments.get(1) {
            None | Some(JsValue::Undefined) => 0.0,
            Some(value) => self.skip_argument(dom, receiver, value)?,
        };
        let record = self.iterator_direct(dom, receiver)?;
        while skipped > 0.0 {
            if self.step_record(dom, record, false)?.is_none() {
                return Ok(JsValue::Boolean(false));
            }
            skipped -= 1.0;
        }
        while let Some(value) = self.step_record(dom, record, true)? {
            if same_value_zero(&value, &search) {
                return self.iterator_close(dom, record.iterator, Ok(JsValue::Boolean(true)));
            }
        }
        Ok(JsValue::Boolean(false))
    }

    fn iterator_join(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let separator = match arguments.first() {
            None | Some(JsValue::Undefined) => ",".to_owned(),
            Some(value) => {
                let text = self.to_string_value(dom, value);
                self.close_on_error(dom, receiver, text)?
            }
        };
        let record = self.iterator_direct(dom, receiver)?;
        let mut joined = String::new();
        let mut first = true;
        while let Some(value) = self.step_record(dom, record, true)? {
            let element = match value {
                JsValue::Undefined | JsValue::Null => String::new(),
                value => {
                    let text = self.to_string_value(dom, &value);
                    self.close_on_error(dom, record.iterator, text)?
                }
            };
            if !first {
                joined.push_str(&separator);
            }
            first = false;
            joined.push_str(&element);
        }
        Ok(JsValue::String(joined))
    }

    fn iterator_chunks(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let size = self.size_argument(dom, receiver, argument(arguments, 0))?;
        let record = self.iterator_direct(dom, receiver)?;
        self.create_helper(IteratorHelperKind::Chunks(size), record, None, 0)
    }

    fn iterator_windows(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let size = self.size_argument(dom, receiver, argument(arguments, 0))?;
        let partial = self.undersized_argument(dom, receiver, argument(arguments, 1))?;
        let record = self.iterator_direct(dom, receiver)?;
        self.create_helper(IteratorHelperKind::Windows(size, partial), record, None, 0)
    }

    // ----- Iterator (constructor) statics -----------------------------------

    /// `Iterator.from(value)`: an iterator that already inherits from
    /// `%Iterator.prototype%` is returned as is; anything else is wrapped.
    fn iterator_from(&mut self, dom: &mut Dom, value: &JsValue) -> Result<JsValue, JsError> {
        let record = self.get_iterator_flattenable(dom, value, true)?;
        if self.inherits_from_iterator_prototype(record.iterator) {
            return Ok(JsValue::Object(record.iterator));
        }
        self.create_helper(IteratorHelperKind::Wrap, record, None, 0)
    }

    fn inherits_from_iterator_prototype(&self, object: ObjectId) -> bool {
        let target = self.realm.iterator_prototype_id();
        let mut current = self.realm.get_prototype(object);
        while let Some(prototype) = current {
            if prototype == target {
                return true;
            }
            current = self.realm.get_prototype(prototype);
        }
        false
    }

    /// `Iterator.concat(...items)`: each item must have a callable
    /// `@@iterator`, which is captured now. The iterators are opened lazily.
    fn iterator_concat(&mut self, dom: &mut Dom, items: &[JsValue]) -> Result<JsValue, JsError> {
        let mut pairs = Vec::with_capacity(items.len() * 2);
        for item in items {
            let JsValue::Object(iterable) = item else {
                return Err(JsError::type_error(
                    "Iterator.concat argument is not an object",
                ));
            };
            let method =
                self.get_symbol_value(dom, *iterable, &JsSymbol::well_known("@@iterator"))?;
            let method = self
                .callable_object(&method)
                .ok_or_else(|| JsError::type_error("Iterator.concat argument is not iterable"))?;
            pairs.push(item.clone());
            pairs.push(JsValue::Object(method));
        }
        self.ensure_heap_capacity(1)?;
        let helper = self
            .realm
            .iterator_helper(IteratorHelperKind::Concat, None, None, None, 0);
        if let Some(ObjectHost::IteratorHelper { buffer, .. }) = self.realm.host_mut(helper) {
            *buffer = pairs;
        }
        Ok(JsValue::Object(helper))
    }

    // ----- Helper and wrapper prototypes ------------------------------------

    /// `%IteratorHelperPrototype%.next()`.
    pub(in crate::runtime) fn iterator_helper_next(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
    ) -> Result<JsValue, JsError> {
        let Some(mut state) = self.helper_snapshot(receiver) else {
            return Err(JsError::type_error("incompatible iterator helper receiver"));
        };
        if state.kind == IteratorHelperKind::Wrap {
            return self.iterator_wrap_next(dom, receiver);
        }
        if state.running {
            return Err(JsError::type_error("iterator helper is already running"));
        }
        if state.done {
            return self.iterator_result(JsValue::Undefined, true);
        }
        state.running = true;
        self.helper_store(receiver, &state);
        let outcome = self.helper_resume(dom, &mut state);
        state.running = false;
        match outcome {
            Ok(Some(value)) => {
                self.helper_store(receiver, &state);
                self.iterator_result(value, false)
            }
            Ok(None) => {
                state.finish();
                self.helper_store(receiver, &state);
                self.iterator_result(JsValue::Undefined, true)
            }
            Err(error) => {
                state.finish();
                self.helper_store(receiver, &state);
                Err(error)
            }
        }
    }

    /// `%IteratorHelperPrototype%.return()`: close the helper's iterators, unless
    /// it has already completed.
    pub(in crate::runtime) fn iterator_helper_return(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
    ) -> Result<JsValue, JsError> {
        let Some(mut state) = self.helper_snapshot(receiver) else {
            return Err(JsError::type_error("incompatible iterator helper receiver"));
        };
        if state.kind == IteratorHelperKind::Wrap {
            return self.iterator_wrap_return(dom, receiver);
        }
        if state.running {
            return Err(JsError::type_error("iterator helper is already running"));
        }
        if state.done {
            return self.iterator_result(JsValue::Undefined, true);
        }
        state.running = true;
        self.helper_store(receiver, &state);
        let outcome = self.helper_close(dom, &state);
        state.running = false;
        state.finish();
        self.helper_store(receiver, &state);
        outcome?;
        self.iterator_result(JsValue::Undefined, true)
    }

    /// Close the helper's open iterators, innermost first. A throwing inner
    /// close closes the source with that error (`flatMap`).
    fn helper_close(&mut self, dom: &mut Dom, state: &HelperState) -> Result<(), JsError> {
        if let (IteratorHelperKind::FlatMap, Some(inner)) = (&state.kind, state.inner)
            && let Err(error) = self.iterator_close(dom, inner, Ok(()))
        {
            return match state.source {
                Some(source) => Err(self.close_on_throw(dom, source, error)),
                None => Err(error),
            };
        }
        match state.source {
            Some(source) => self.iterator_close(dom, source, Ok(())),
            None => Ok(()),
        }
    }

    /// `%WrapForValidIteratorPrototype%.next()`: forwards to the wrapped `next`.
    fn iterator_wrap_next(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
    ) -> Result<JsValue, JsError> {
        let state = self
            .helper_snapshot(receiver)
            .filter(|state| state.kind == IteratorHelperKind::Wrap)
            .ok_or_else(|| JsError::type_error("incompatible iterator wrapper receiver"))?;
        let (Some(iterator), Some(next)) = (state.source, state.source_next) else {
            return Err(JsError::type_error("iterator next method is not callable"));
        };
        self.call_with_this(dom, next, &[], JsValue::Object(iterator))
    }

    /// Build one `{ value, done }` iterator result object.
    fn iterator_result(&mut self, value: JsValue, done: bool) -> Result<JsValue, JsError> {
        self.ensure_heap_capacity(1)?;
        let result = self.realm.create_ordinary_object();
        self.realm.set_property(result, "value".to_owned(), value);
        self.realm
            .set_property(result, "done".to_owned(), JsValue::Boolean(done));
        Ok(JsValue::Object(result))
    }

    /// `%WrapForValidIteratorPrototype%.return()`: forwards to the wrapped
    /// `return`, or reports `{ value: undefined, done: true }` without one.
    fn iterator_wrap_return(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
    ) -> Result<JsValue, JsError> {
        let state = self
            .helper_snapshot(receiver)
            .filter(|state| state.kind == IteratorHelperKind::Wrap)
            .ok_or_else(|| JsError::type_error("incompatible iterator wrapper receiver"))?;
        let Some(iterator) = state.source else {
            return Err(JsError::type_error(
                "incompatible iterator wrapper receiver",
            ));
        };
        match self.call_return_method(dom, iterator)? {
            Some(result) => Ok(result),
            None => self.iterator_result(JsValue::Undefined, true),
        }
    }

    /// Run one resumption of a helper. `Ok(Some(value))` yields, and
    /// `Ok(None)` completes.
    fn helper_resume(
        &mut self,
        dom: &mut Dom,
        state: &mut HelperState,
    ) -> Result<Option<JsValue>, JsError> {
        let kind = state.kind.clone();
        match kind {
            IteratorHelperKind::Wrap => Ok(None),
            IteratorHelperKind::Map => self.map_step(dom, state),
            IteratorHelperKind::Filter => self.filter_step(dom, state),
            IteratorHelperKind::Take => self.take_step(dom, state),
            IteratorHelperKind::Drop => self.drop_step(dom, state),
            IteratorHelperKind::FlatMap => self.flat_map_step(dom, state),
            IteratorHelperKind::Concat => self.concat_step(dom, state),
            IteratorHelperKind::Chunks(size) => self.chunks_step(dom, state, size),
            IteratorHelperKind::Windows(size, partial) => {
                self.windows_step(dom, state, size, partial)
            }
        }
    }

    fn map_step(
        &mut self,
        dom: &mut Dom,
        state: &mut HelperState,
    ) -> Result<Option<JsValue>, JsError> {
        let (Some(record), Some(callback)) = (state.source_record(), state.callback) else {
            return Ok(None);
        };
        let Some(value) = self.step_record(dom, record, true)? else {
            return Ok(None);
        };
        let counter = state.counter;
        state.counter += 1;
        let result = self.call(dom, callback, &[value, JsValue::Number(counter as f64)]);
        self.close_on_error(dom, record.iterator, result).map(Some)
    }

    fn filter_step(
        &mut self,
        dom: &mut Dom,
        state: &mut HelperState,
    ) -> Result<Option<JsValue>, JsError> {
        let (Some(record), Some(callback)) = (state.source_record(), state.callback) else {
            return Ok(None);
        };
        loop {
            let Some(value) = self.step_record(dom, record, true)? else {
                return Ok(None);
            };
            let counter = state.counter;
            state.counter += 1;
            let result = self.call(
                dom,
                callback,
                &[value.clone(), JsValue::Number(counter as f64)],
            );
            if self
                .close_on_error(dom, record.iterator, result)?
                .is_truthy()
            {
                return Ok(Some(value));
            }
        }
    }

    /// `take`: `counter` is the remaining count. Once it reaches zero, the
    /// source is closed with a normal completion.
    fn take_step(
        &mut self,
        dom: &mut Dom,
        state: &mut HelperState,
    ) -> Result<Option<JsValue>, JsError> {
        let Some(record) = state.source_record() else {
            return Ok(None);
        };
        if state.counter == 0 {
            self.iterator_close(dom, record.iterator, Ok(()))?;
            return Ok(None);
        }
        if state.counter != UNBOUNDED {
            state.counter -= 1;
        }
        self.step_record(dom, record, true)
    }

    /// `drop`: `counter` is the number of values still to skip.
    fn drop_step(
        &mut self,
        dom: &mut Dom,
        state: &mut HelperState,
    ) -> Result<Option<JsValue>, JsError> {
        let Some(record) = state.source_record() else {
            return Ok(None);
        };
        while state.counter > 0 {
            if state.counter != UNBOUNDED {
                state.counter -= 1;
            }
            if self.step_record(dom, record, false)?.is_none() {
                return Ok(None);
            }
        }
        self.step_record(dom, record, true)
    }

    fn flat_map_step(
        &mut self,
        dom: &mut Dom,
        state: &mut HelperState,
    ) -> Result<Option<JsValue>, JsError> {
        let (Some(source), Some(callback)) = (state.source_record(), state.callback) else {
            return Ok(None);
        };
        loop {
            if let Some(inner) = state.inner {
                let inner_record = Record {
                    iterator: inner,
                    next: state.inner_next,
                };
                match self.step_record(dom, inner_record, true) {
                    Ok(Some(value)) => return Ok(Some(value)),
                    Ok(None) => {
                        state.inner = None;
                        state.inner_next = None;
                    }
                    Err(error) => return Err(self.close_on_throw(dom, source.iterator, error)),
                }
            }
            let Some(value) = self.step_record(dom, source, true)? else {
                return Ok(None);
            };
            let counter = state.counter;
            state.counter += 1;
            let mapped = self.call(dom, callback, &[value, JsValue::Number(counter as f64)]);
            let mapped = self.close_on_error(dom, source.iterator, mapped)?;
            let inner = self.get_iterator_flattenable(dom, &mapped, false);
            let inner = self.close_on_error(dom, source.iterator, inner)?;
            state.inner = Some(inner.iterator);
            state.inner_next = inner.next;
        }
    }

    /// `concat`: `buffer` holds (iterable, open method) pairs and `counter` is
    /// the index of the next pair to open.
    fn concat_step(
        &mut self,
        dom: &mut Dom,
        state: &mut HelperState,
    ) -> Result<Option<JsValue>, JsError> {
        loop {
            if let Some(record) = state.source_record() {
                if let Some(value) = self.step_record(dom, record, true)? {
                    return Ok(Some(value));
                }
                state.source = None;
                state.source_next = None;
            }
            let index = state.counter as usize * 2;
            let (Some(JsValue::Object(iterable)), Some(JsValue::Object(method))) = (
                state.buffer.get(index).cloned(),
                state.buffer.get(index + 1).cloned(),
            ) else {
                return Ok(None);
            };
            state.counter += 1;
            let iterator = match self.call_with_this(dom, method, &[], JsValue::Object(iterable))? {
                JsValue::Object(iterator) => iterator,
                _ => return Err(JsError::type_error("iterator is not an object")),
            };
            let record = self.iterator_direct(dom, iterator)?;
            state.source = Some(iterator);
            state.source_next = record.next;
        }
    }

    /// `chunks`: collects `size` values per step. A short final chunk is
    /// yielded once, and the helper is then complete.
    fn chunks_step(
        &mut self,
        dom: &mut Dom,
        state: &mut HelperState,
        size: u64,
    ) -> Result<Option<JsValue>, JsError> {
        let Some(record) = state.source_record() else {
            return Ok(None);
        };
        let mut chunk = Vec::new();
        while (chunk.len() as u64) < size {
            match self.step_record(dom, record, true)? {
                Some(value) => chunk.push(value),
                None => {
                    if chunk.is_empty() {
                        return Ok(None);
                    }
                    state.done = true;
                    break;
                }
            }
        }
        self.create_array_from_values(&chunk)
            .map(|array| Some(JsValue::Object(array)))
    }

    /// `windows`: a sliding window of `size` values. The first window is
    /// yielded once it is full, or, with `allow-partial`, when the source ends
    /// early.
    fn windows_step(
        &mut self,
        dom: &mut Dom,
        state: &mut HelperState,
        size: u64,
        partial: bool,
    ) -> Result<Option<JsValue>, JsError> {
        let Some(record) = state.source_record() else {
            return Ok(None);
        };
        let mut buffer = std::mem::take(&mut state.buffer);
        if (buffer.len() as u64) < size {
            while (buffer.len() as u64) < size {
                match self.step_record(dom, record, true)? {
                    Some(value) => buffer.push(value),
                    None => {
                        if !partial || buffer.is_empty() {
                            return Ok(None);
                        }
                        state.done = true;
                        break;
                    }
                }
            }
        } else {
            match self.step_record(dom, record, true)? {
                Some(value) => {
                    buffer.remove(0);
                    buffer.push(value);
                }
                None => return Ok(None),
            }
        }
        let window = self.create_array_from_values(&buffer)?;
        state.buffer = buffer;
        Ok(Some(JsValue::Object(window)))
    }
}
