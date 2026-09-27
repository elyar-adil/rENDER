#![allow(
    clippy::cast_precision_loss,
    clippy::match_same_arms,
    clippy::never_loop,
    clippy::single_match,
    clippy::single_match_else,
    clippy::too_many_lines
)]

//! Iterator helpers (ES2025 `%IteratorPrototype%` methods) and the abstract
//! `Iterator` constructor.
//!
//! Helper objects are lazy state machines: every `next()` steps the
//! underlying iterator only as far as the helper semantics require, and the
//! spec's early-close behavior is preserved by forwarding `return` to the
//! source when a consumer stops early.

use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::runtime::builtins::array::to_length;
use crate::runtime::convert::to_number;
use crate::value::IteratorHelperKind;
use crate::value::ObjectHost;
use render_dom::Dom;

impl JsRuntime {
    /// Dispatch entry for the iterator-helper native functions; returns
    /// `None` when `function` is not one of them.
    #[allow(clippy::option_if_let_else)]
    pub(in crate::runtime) fn dispatch_iterator_native(
        &mut self,
        dom: &mut Dom,
        function: crate::value::NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Option<Result<JsValue, JsError>> {
        use crate::value::NativeFunction as N;
        let result = match function {
            N::IteratorConstructor => Err(JsError::type_error("Iterator is abstract")),
            N::IteratorFrom => self.iterator_from(dom, arguments),
            N::IteratorPrototypeIterator => Ok(JsValue::Object(receiver)),
            N::IteratorHelperNext => self.iterator_helper_next(dom, receiver),
            N::IteratorHelperReturn => self.iterator_helper_return(dom, receiver),
            N::IteratorMap => self.iterator_map(dom, receiver, arguments),
            N::IteratorFilter => self.iterator_filter(dom, receiver, arguments),
            N::IteratorTake => self.iterator_take(dom, receiver, arguments),
            N::IteratorDrop => self.iterator_drop(dom, receiver, arguments),
            N::IteratorFlatMap => self.iterator_flat_map(dom, receiver, arguments),
            N::IteratorReduce => self.iterator_reduce(dom, receiver, arguments),
            N::IteratorToArray => self.iterator_to_array(dom, receiver),
            N::IteratorForEach => self.iterator_for_each(dom, receiver, arguments),
            N::IteratorSome => {
                self.iterator_predicate(dom, receiver, arguments, PredicateKind::Some)
            }
            N::IteratorEvery => {
                self.iterator_predicate(dom, receiver, arguments, PredicateKind::Every)
            }
            N::IteratorFind => {
                self.iterator_predicate(dom, receiver, arguments, PredicateKind::Find)
            }
            N::IteratorConcat => self.iterator_concat(dom, receiver, arguments),
            N::IteratorChunks => self.iterator_chunks(dom, receiver, arguments, true),
            N::IteratorWindows => self.iterator_chunks(dom, receiver, arguments, false),
            _ => return None,
        };
        Some(result)
    }

    /// `GetIteratorDirect(this)`: the receiver must be an object whose `next`
    /// is callable.
    fn iterator_direct(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
    ) -> Result<(ObjectId, ObjectId), JsError> {
        let next = self.get_member(dom, receiver, "next")?;
        let next = Self::require_callable_object(&next, &self.realm)?;
        Ok((receiver, next))
    }

    /// Step one iterator: `None` when the source reports `done`.
    fn step_iterator(
        &mut self,
        dom: &mut Dom,
        iterator: ObjectId,
        next: ObjectId,
    ) -> Result<Option<JsValue>, JsError> {
        let result = self.call_with_this(dom, next, &[], JsValue::Object(iterator))?;
        let JsValue::Object(record) = result else {
            return Err(JsError::type_error("iterator result is not an object"));
        };
        let done = self.get_member(dom, record, "done")?.is_truthy();
        if done {
            return Ok(None);
        }
        Ok(Some(self.get_member(dom, record, "value")?))
    }

    fn create_iterator_helper(
        &mut self,
        kind: IteratorHelperKind,
        source: Option<ObjectId>,
        source_next: Option<ObjectId>,
        callback: Option<ObjectId>,
        counter: u64,
    ) -> Result<JsValue, JsError> {
        self.ensure_heap_capacity(1)?;
        Ok(JsValue::Object(self.realm.iterator_helper(
            kind,
            source,
            source_next,
            callback,
            counter,
        )))
    }

    fn iterator_map(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let callback = Self::require_callable_object(
            arguments.first().unwrap_or(&JsValue::Undefined),
            &self.realm,
        )?;
        let (source, next) = self.iterator_direct(dom, receiver)?;
        self.create_iterator_helper(
            IteratorHelperKind::Map,
            Some(source),
            Some(next),
            Some(callback),
            0,
        )
    }

    fn iterator_filter(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let callback = Self::require_callable_object(
            arguments.first().unwrap_or(&JsValue::Undefined),
            &self.realm,
        )?;
        let (source, next) = self.iterator_direct(dom, receiver)?;
        self.create_iterator_helper(
            IteratorHelperKind::Filter,
            Some(source),
            Some(next),
            Some(callback),
            0,
        )
    }

    fn iterator_take(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let limit = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let (source, next) = self.iterator_direct(dom, receiver)?;
        self.create_iterator_helper(
            IteratorHelperKind::Take(count_argument(&limit)?),
            Some(source),
            Some(next),
            None,
            0,
        )
    }

    fn iterator_drop(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let limit = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let (source, next) = self.iterator_direct(dom, receiver)?;
        self.create_iterator_helper(
            IteratorHelperKind::Drop(count_argument(&limit)?),
            Some(source),
            Some(next),
            None,
            0,
        )
    }

    fn iterator_flat_map(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let callback = Self::require_callable_object(
            arguments.first().unwrap_or(&JsValue::Undefined),
            &self.realm,
        )?;
        let (source, next) = self.iterator_direct(dom, receiver)?;
        self.create_iterator_helper(
            IteratorHelperKind::FlatMap,
            Some(source),
            Some(next),
            Some(callback),
            0,
        )
    }

    fn iterator_reduce(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let callback = Self::require_callable_object(
            arguments.first().unwrap_or(&JsValue::Undefined),
            &self.realm,
        )?;
        let (source, next) = self.iterator_direct(dom, receiver)?;
        let mut accumulator = arguments.get(1).cloned();
        let mut index = 0_u64;
        while let Some(value) = self.step_iterator(dom, source, next)? {
            accumulator = Some(match accumulator.take() {
                Some(accumulator) => self.call(
                    dom,
                    callback,
                    &[accumulator, value, JsValue::Number(index as f64)],
                )?,
                None => value,
            });
            index += 1;
        }
        accumulator
            .ok_or_else(|| JsError::type_error("Reduce of empty iterator with no initial value"))
    }

    fn iterator_to_array(&mut self, dom: &mut Dom, receiver: ObjectId) -> Result<JsValue, JsError> {
        let (source, next) = self.iterator_direct(dom, receiver)?;
        let mut values = Vec::new();
        while let Some(value) = self.step_iterator(dom, source, next)? {
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
        let callback = Self::require_callable_object(
            arguments.first().unwrap_or(&JsValue::Undefined),
            &self.realm,
        )?;
        let (source, next) = self.iterator_direct(dom, receiver)?;
        let mut index = 0_u64;
        while let Some(value) = self.step_iterator(dom, source, next)? {
            self.call(dom, callback, &[value, JsValue::Number(index as f64)])?;
            index += 1;
        }
        Ok(JsValue::Undefined)
    }

    fn iterator_predicate(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
        kind: PredicateKind,
    ) -> Result<JsValue, JsError> {
        let callback = Self::require_callable_object(
            arguments.first().unwrap_or(&JsValue::Undefined),
            &self.realm,
        )?;
        let (source, next) = self.iterator_direct(dom, receiver)?;
        let mut index = 0_u64;
        while let Some(value) = self.step_iterator(dom, source, next)? {
            let matched = self
                .call(
                    dom,
                    callback,
                    &[value.clone(), JsValue::Number(index as f64)],
                )?
                .is_truthy();
            index += 1;
            match kind {
                PredicateKind::Some if matched => return Ok(JsValue::Boolean(true)),
                PredicateKind::Every if !matched => return Ok(JsValue::Boolean(false)),
                PredicateKind::Find if matched => return Ok(value),
                _ => {}
            }
        }
        Ok(match kind {
            PredicateKind::Some => JsValue::Boolean(false),
            PredicateKind::Every => JsValue::Boolean(true),
            PredicateKind::Find => JsValue::Undefined,
        })
    }

    fn iterator_concat(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let (source, next) = self.iterator_direct(dom, receiver)?;
        let mut rest = Vec::new();
        for argument in arguments {
            let iterator = self
                .get_iterator(dom, argument)?
                .ok_or_else(|| JsError::type_error("Iterator.concat argument is not iterable"))?;
            let (iterator, _) = iterator;
            rest.push(iterator);
        }
        let helper = self.realm.iterator_helper(
            IteratorHelperKind::Concat(rest),
            Some(source),
            Some(next),
            None,
            0,
        );
        Ok(JsValue::Object(helper))
    }

    fn iterator_chunks(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
        chunks: bool,
    ) -> Result<JsValue, JsError> {
        let size = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let (source, next) = self.iterator_direct(dom, receiver)?;
        let size = self.iterator_chunk_size(&size)?;
        let kind = if chunks {
            IteratorHelperKind::Chunks(size)
        } else {
            IteratorHelperKind::Windows(size)
        };
        self.create_iterator_helper(kind, Some(source), Some(next), None, 0)
    }

    /// `Iterator.from(value)`: reuse the @@iterator result, or wrap an object
    /// that already exposes a callable `next`.
    fn iterator_from(&mut self, dom: &mut Dom, arguments: &[JsValue]) -> Result<JsValue, JsError> {
        let value = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        if matches!(value, JsValue::Null | JsValue::Undefined) {
            return Err(JsError::type_error(
                "Iterator.from requires an iterable object",
            ));
        }
        if let Some((iterator, next)) = self.get_iterator(dom, &value)? {
            return self.create_iterator_helper(
                IteratorHelperKind::Wrap,
                Some(iterator),
                Some(next),
                None,
                0,
            );
        }
        let JsValue::Object(object) = value else {
            return Err(JsError::type_error("Iterator.from requires an object"));
        };
        let next = self.get_member(dom, object, "next")?;
        let next = Self::require_callable_object(&next, &self.realm)?;
        self.create_iterator_helper(IteratorHelperKind::Wrap, Some(object), Some(next), None, 0)
    }

    /// Step one helper. Every branch reads its state, drops the borrow, runs
    /// user code, then writes the advanced state back.
    pub(in crate::runtime) fn iterator_helper_next(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
    ) -> Result<JsValue, JsError> {
        let Some(ObjectHost::IteratorHelper { kind, .. }) = self.realm.host(receiver) else {
            return Err(JsError::type_error("incompatible iterator helper receiver"));
        };
        let kind = kind.clone();
        let (source, source_next) = match self.realm.host(receiver) {
            Some(ObjectHost::IteratorHelper {
                source,
                source_next,
                ..
            }) => (source, source_next),
            _ => (None, None),
        };
        if source.is_none() || source_next.is_none() {
            self.mark_helper_done(receiver);
            return self.iterator_result(dom, JsValue::Undefined, true);
        }
        let source = source.expect("checked above");
        let next = source_next.expect("checked above");
        match kind {
            IteratorHelperKind::Wrap => match self.step_iterator(dom, source, next)? {
                None => {
                    self.mark_helper_done(receiver);
                    self.iterator_result(dom, JsValue::Undefined, true)
                }
                Some(value) => self.iterator_result(dom, value, false),
            },
            IteratorHelperKind::Map => match self.step_iterator(dom, source, next)? {
                None => {
                    self.mark_helper_done(receiver);
                    self.iterator_result(dom, JsValue::Undefined, true)
                }
                Some(value) => {
                    let (callback, counter) = self.helper_callback(receiver);
                    let mapped =
                        self.call(dom, callback, &[value, JsValue::Number(counter as f64)])?;
                    self.bump_helper_counter(receiver, counter + 1);
                    self.iterator_result(dom, mapped, false)
                }
            },
            IteratorHelperKind::Filter => {
                let (callback, _) = self.helper_callback(receiver);
                loop {
                    match self.step_iterator(dom, source, next)? {
                        None => {
                            self.mark_helper_done(receiver);
                            return self.iterator_result(dom, JsValue::Undefined, true);
                        }
                        Some(value) => {
                            let counter = self.helper_counter(receiver);
                            let keep = self
                                .call(
                                    dom,
                                    callback,
                                    &[value.clone(), JsValue::Number(counter as f64)],
                                )?
                                .is_truthy();
                            self.bump_helper_counter(receiver, counter + 1);
                            if keep {
                                return self.iterator_result(dom, value, false);
                            }
                        }
                    }
                }
            }
            IteratorHelperKind::Take(limit) => {
                let taken = self.helper_counter(receiver);
                if taken >= limit {
                    self.close_helper_source(dom, receiver);
                    return self.iterator_result(dom, JsValue::Undefined, true);
                }
                match self.step_iterator(dom, source, next)? {
                    None => {
                        self.mark_helper_done(receiver);
                        self.iterator_result(dom, JsValue::Undefined, true)
                    }
                    Some(value) => {
                        self.bump_helper_counter(receiver, taken.saturating_add(1));
                        self.iterator_result(dom, value, false)
                    }
                }
            }
            IteratorHelperKind::Drop(limit) => {
                let mut remaining = self.helper_counter(receiver);
                while remaining < limit {
                    match self.step_iterator(dom, source, next)? {
                        None => {
                            self.mark_helper_done(receiver);
                            return self.iterator_result(dom, JsValue::Undefined, true);
                        }
                        Some(_) => {
                            remaining += 1;
                            self.bump_helper_counter(receiver, remaining);
                        }
                    }
                }
                match self.step_iterator(dom, source, next)? {
                    None => {
                        self.mark_helper_done(receiver);
                        self.iterator_result(dom, JsValue::Undefined, true)
                    }
                    Some(value) => self.iterator_result(dom, value, false),
                }
            }
            IteratorHelperKind::FlatMap => {
                let (callback, _) = self.helper_callback(receiver);
                loop {
                    // Drain an active inner iterator first.
                    if let Some((inner, inner_next)) = self.helper_inner(receiver) {
                        match self.step_iterator(dom, inner, inner_next)? {
                            Some(value) => return self.iterator_result(dom, value, false),
                            None => self.clear_helper_inner(receiver),
                        }
                    }
                    match self.step_iterator(dom, source, next)? {
                        None => {
                            self.mark_helper_done(receiver);
                            return self.iterator_result(dom, JsValue::Undefined, true);
                        }
                        Some(value) => {
                            let counter = self.helper_counter(receiver);
                            let mapped = self.call(
                                dom,
                                callback,
                                &[value, JsValue::Number(counter as f64)],
                            )?;
                            self.bump_helper_counter(receiver, counter + 1);
                            let (iterator, iterator_next) =
                                self.get_iterator(dom, &mapped)?.ok_or_else(|| {
                                    JsError::type_error(
                                        "flatMap mapper must return an iterable object",
                                    )
                                })?;
                            self.set_helper_inner(receiver, iterator, iterator_next);
                        }
                    }
                }
            }
            IteratorHelperKind::Concat(rest) => loop {
                match self.step_iterator(dom, source, next)? {
                    Some(value) => return self.iterator_result(dom, value, false),
                    None => {
                        let Some(next_iterator) = self.shift_concat_source(receiver) else {
                            self.mark_helper_done(receiver);
                            return self.iterator_result(dom, JsValue::Undefined, true);
                        };
                        let _ = rest;
                        let (iterator, iterator_next) = self
                            .get_iterator(dom, &JsValue::Object(next_iterator))?
                            .unwrap_or((next_iterator, next));
                        self.set_helper_source(receiver, iterator, iterator_next);
                        return self.iterator_helper_next(dom, receiver);
                    }
                }
            },
            IteratorHelperKind::Chunks(size) => {
                let mut buffer = self.helper_buffer(receiver);
                if buffer.len() as u64 >= size {
                    let chunk = JsValue::Object(self.create_array_from_values(&buffer)?);
                    self.set_helper_buffer(receiver, Vec::new());
                    return self.iterator_result(dom, chunk, false);
                }
                while (buffer.len() as u64) < size {
                    match self.step_iterator(dom, source, next)? {
                        Some(value) => buffer.push(value),
                        None => {
                            self.mark_helper_done(receiver);
                            if buffer.is_empty() {
                                return self.iterator_result(dom, JsValue::Undefined, true);
                            }
                            self.set_helper_buffer(receiver, buffer.clone());
                            let chunk = JsValue::Object(self.create_array_from_values(&buffer)?);
                            self.set_helper_buffer(receiver, Vec::new());
                            return self.iterator_result(dom, chunk, false);
                        }
                    }
                }
                self.set_helper_buffer(receiver, buffer.clone());
                let chunk = JsValue::Object(self.create_array_from_values(&buffer)?);
                self.set_helper_buffer(receiver, Vec::new());
                self.iterator_result(dom, chunk, false)
            }
            IteratorHelperKind::Windows(size) => {
                let mut buffer = self.helper_buffer(receiver);
                if buffer.is_empty() {
                    while (buffer.len() as u64) < size {
                        match self.step_iterator(dom, source, next)? {
                            Some(value) => buffer.push(value),
                            None => {
                                self.mark_helper_done(receiver);
                                return self.iterator_result(dom, JsValue::Undefined, true);
                            }
                        }
                    }
                    self.set_helper_buffer(receiver, buffer.clone());
                    let window = JsValue::Object(self.create_array_from_values(&buffer)?);
                    return self.iterator_result(dom, window, false);
                }
                match self.step_iterator(dom, source, next)? {
                    Some(value) => {
                        buffer.remove(0);
                        buffer.push(value);
                        self.set_helper_buffer(receiver, buffer.clone());
                        let window = JsValue::Object(self.create_array_from_values(&buffer)?);
                        self.iterator_result(dom, window, false)
                    }
                    None => {
                        self.mark_helper_done(receiver);
                        self.iterator_result(dom, JsValue::Undefined, true)
                    }
                }
            }
        }
    }

    /// `%IteratorHelperPrototype%.return`: close the source if still open.
    pub(in crate::runtime) fn iterator_helper_return(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
    ) -> Result<JsValue, JsError> {
        if !matches!(
            self.realm.host(receiver),
            Some(ObjectHost::IteratorHelper { .. })
        ) {
            return Err(JsError::type_error("incompatible iterator helper receiver"));
        }
        let done = matches!(
            self.realm.host(receiver),
            Some(ObjectHost::IteratorHelper { done: true, .. })
        );
        if !done {
            self.close_helper_source(dom, receiver);
            self.mark_helper_done(receiver);
        }
        self.iterator_result(dom, JsValue::Undefined, true)
    }

    /// Call the source iterator's `return` method, if any, and mark the
    /// helper closed.
    fn close_helper_source(&mut self, dom: &mut Dom, receiver: ObjectId) {
        let (source, source_next, done) = match self.realm.host(receiver) {
            Some(ObjectHost::IteratorHelper {
                source,
                source_next,
                done,
                ..
            }) => (source, source_next, done),
            _ => (None, None, true),
        };
        let _ = source_next;
        if done {
            return;
        }
        let Some(source) = source else {
            return;
        };
        let return_method = self
            .get_member(dom, source, "return")
            .ok()
            .filter(|value| {
                matches!(value, JsValue::Object(object) if Self::is_callable_object(*object, &self.realm))
            });
        if let Some(JsValue::Object(return_method)) = return_method {
            let _ = self.call_with_this(dom, return_method, &[], JsValue::Object(source));
        }
    }

    /// Build one `{ value, done }` iterator result object.
    fn iterator_result(
        &mut self,
        _dom: &mut Dom,
        value: JsValue,
        done: bool,
    ) -> Result<JsValue, JsError> {
        self.ensure_heap_capacity(1)?;
        let result = self.realm.create_ordinary_object();
        self.realm.set_property(result, "value".to_owned(), value);
        self.realm
            .set_property(result, "done".to_owned(), JsValue::Boolean(done));
        Ok(JsValue::Object(result))
    }

    fn mark_helper_done(&mut self, receiver: ObjectId) {
        if let Some(ObjectHost::IteratorHelper {
            done,
            source,
            source_next,
            ..
        }) = self.realm.host_mut(receiver)
        {
            *done = true;
            *source = None;
            *source_next = None;
        }
    }

    fn helper_callback(&self, receiver: ObjectId) -> (ObjectId, u64) {
        match self.realm.host(receiver) {
            Some(ObjectHost::IteratorHelper {
                callback, counter, ..
            }) => (
                callback.unwrap_or_else(|| self.realm.global_object()),
                counter,
            ),
            _ => (self.realm.global_object(), 0),
        }
    }

    fn helper_counter(&self, receiver: ObjectId) -> u64 {
        match self.realm.host(receiver) {
            Some(ObjectHost::IteratorHelper { counter, .. }) => counter,
            _ => 0,
        }
    }

    fn bump_helper_counter(&mut self, receiver: ObjectId, value: u64) {
        if let Some(ObjectHost::IteratorHelper { counter, .. }) = self.realm.host_mut(receiver) {
            *counter = value;
        }
    }

    fn helper_inner(&self, receiver: ObjectId) -> Option<(ObjectId, ObjectId)> {
        match self.realm.host(receiver) {
            Some(ObjectHost::IteratorHelper {
                inner: Some(inner),
                inner_next: Some(inner_next),
                ..
            }) => Some((inner, inner_next)),
            _ => None,
        }
    }

    fn set_helper_inner(&mut self, receiver: ObjectId, inner: ObjectId, inner_next: ObjectId) {
        if let Some(ObjectHost::IteratorHelper {
            inner: slot,
            inner_next: next_slot,
            ..
        }) = self.realm.host_mut(receiver)
        {
            *slot = Some(inner);
            *next_slot = Some(inner_next);
        }
    }

    fn clear_helper_inner(&mut self, receiver: ObjectId) {
        if let Some(ObjectHost::IteratorHelper {
            inner, inner_next, ..
        }) = self.realm.host_mut(receiver)
        {
            *inner = None;
            *inner_next = None;
        }
    }

    fn set_helper_source(&mut self, receiver: ObjectId, source: ObjectId, source_next: ObjectId) {
        if let Some(ObjectHost::IteratorHelper {
            source: source_slot,
            source_next: next_slot,
            ..
        }) = self.realm.host_mut(receiver)
        {
            *source_slot = Some(source);
            *next_slot = Some(source_next);
        }
    }

    fn shift_concat_source(&mut self, receiver: ObjectId) -> Option<ObjectId> {
        match self.realm.host_mut(receiver) {
            Some(ObjectHost::IteratorHelper {
                kind: IteratorHelperKind::Concat(rest),
                ..
            }) if !rest.is_empty() => Some(rest.remove(0)),
            _ => None,
        }
    }

    fn helper_buffer(&self, receiver: ObjectId) -> Vec<JsValue> {
        match self.realm.host(receiver) {
            Some(ObjectHost::IteratorHelper { buffer, .. }) => buffer.clone(),
            _ => Vec::new(),
        }
    }

    fn set_helper_buffer(&mut self, receiver: ObjectId, buffer: Vec<JsValue>) {
        if let Some(ObjectHost::IteratorHelper { buffer: slot, .. }) = self.realm.host_mut(receiver)
        {
            *slot = buffer;
        }
    }

    /// `chunks`/`windows` sizes must be integers of at least one.
    fn iterator_chunk_size(&mut self, value: &JsValue) -> Result<u64, JsError> {
        let number = to_number(value)?;
        if !number.is_finite() || number.fract() != 0.0 || number < 1.0 {
            return Err(self.range_error("iterator helper size must be a positive integer"));
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Ok(number as u64)
    }
}

/// Which short-circuiting predicate `some`/`every`/`find` evaluates.
#[derive(Clone, Copy)]
enum PredicateKind {
    Some,
    Every,
    Find,
}

/// `take`/`drop` count arguments: `ToIntegerOrInfinity`, rejecting negatives.
fn count_argument(value: &JsValue) -> Result<u64, JsError> {
    let number = to_number(value)?;
    if number.is_nan() {
        return Ok(0);
    }
    if number < 0.0 {
        return Err(JsError::type_error(
            "iterator helper count must not be negative",
        ));
    }
    if number.is_infinite() {
        return Ok(u64::MAX);
    }
    Ok(to_length(&JsValue::Number(number.trunc()))? as u64)
}
