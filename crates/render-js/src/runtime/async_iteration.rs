//! Async generators (ECMA-262 27.6) and the async-from-sync iterator
//! (ECMA-262 27.1.4) that `for await` uses over sync iterables.
//!
//! An async generator body runs as a coroutine ([`super::coroutine_run`]). What
//! sits on top of it is the request queue: every `next`, `return` and `throw`
//! call appends a request and gets a promise back, and the body is resumed for
//! the request at the head of the queue. A body step that completes a request
//! (a `yield`, a `return`, or a throw out of the body) settles that request's
//! promise and moves on to the next one.

use super::JsRuntime;
use super::coroutine_run::{CoStep, Resume};
use crate::value::ObjectHost;
use crate::{JsError, JsErrorKind, JsSymbol, JsValue, ObjectId};
use render_dom::Dom;
use std::collections::{BTreeMap, VecDeque};

/// ECMA-262 27.6.3 `[[AsyncGeneratorState]]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AsyncGeneratorState {
    /// Created; the body has not started.
    SuspendedStart,
    /// Stopped at a `yield` with no request waiting.
    SuspendedYield,
    /// The body is running or awaiting inside a resumption.
    Executing,
    /// Settling a `return` request, which awaits its operand first.
    AwaitingReturn,
    /// The body has finished; requests are answered without running it.
    Completed,
}

/// One pending `next`/`return`/`throw` call.
#[derive(Debug)]
pub(super) struct AsyncRequest {
    /// The resumption the call makes, which is what the body receives.
    pub(super) completion: Resume,
    /// The promise the call returned, settled when the body reaches the request.
    pub(super) promise: usize,
}

/// The state of one async generator object, keyed by its coroutine index.
#[derive(Debug)]
pub(super) struct AsyncGenerator {
    pub(super) state: AsyncGeneratorState,
    pub(super) queue: VecDeque<AsyncRequest>,
}

/// The three methods of `%AsyncFromSyncIteratorPrototype%`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AsyncFromSyncMethod {
    Next,
    Return,
    Throw,
}

/// Async generator records, keyed by the coroutine index of each generator.
pub(super) type AsyncGenerators = BTreeMap<usize, AsyncGenerator>;

impl JsRuntime {
    /// A new async generator starts suspended before its body (ECMA-262
    /// 27.6.3.2 AsyncGeneratorStart).
    pub(super) fn start_async_generator(&mut self, id: usize) {
        self.async_generators.insert(
            id,
            AsyncGenerator {
                state: AsyncGeneratorState::SuspendedStart,
                queue: VecDeque::new(),
            },
        );
    }

    pub(super) fn async_generator_state(&self, id: usize) -> Option<AsyncGeneratorState> {
        self.async_generators.get(&id).map(|generator| generator.state)
    }

    fn set_async_generator_state(&mut self, id: usize, state: AsyncGeneratorState) {
        if let Some(generator) = self.async_generators.get_mut(&id) {
            generator.state = state;
        }
    }

    fn enqueue_async_request(&mut self, id: usize, completion: Resume, promise: usize) {
        if let Some(generator) = self.async_generators.get_mut(&id) {
            generator.queue.push_back(AsyncRequest {
                completion,
                promise,
            });
        }
    }

    /// The completion of the request at the head of the queue, if any.
    fn front_completion(&self, id: usize) -> Option<Resume> {
        self.async_generators
            .get(&id)
            .and_then(|generator| generator.queue.front())
            .map(|request| request.completion.clone())
    }

    /// `%AsyncGeneratorPrototype%.next`, `.return` and `.throw` (ECMA-262
    /// 27.6.1.2-4): queue the request, run the body if it is waiting, and
    /// return the request's promise. Argument and receiver failures reject the
    /// promise instead of throwing.
    pub(super) fn async_generator_request(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        resume: Resume,
    ) -> Result<JsValue, JsError> {
        let (promise, result) = self.create_promise()?;
        let id = match self.realm.host(receiver) {
            Some(ObjectHost::AsyncGenerator(id)) if self.async_generators.contains_key(&id) => id,
            _ => {
                let error = JsError::type_error("incompatible AsyncGenerator receiver");
                let reason = self.error_value(&error);
                self.reject_promise(promise, &reason);
                return Ok(result);
            }
        };
        let state = self
            .async_generator_state(id)
            .unwrap_or(AsyncGeneratorState::Completed);
        match resume {
            Resume::Next(value) => {
                if state == AsyncGeneratorState::Completed {
                    self.settle_iterator_result(promise, JsValue::Undefined, true);
                    return Ok(result);
                }
                self.enqueue_async_request(id, Resume::Next(value.clone()), promise);
                if matches!(
                    state,
                    AsyncGeneratorState::SuspendedStart | AsyncGeneratorState::SuspendedYield
                ) {
                    self.async_generator_resume(dom, id, Resume::Next(value));
                }
            }
            Resume::Throw(reason) => {
                // A generator that never started completes without running.
                if state == AsyncGeneratorState::SuspendedStart {
                    self.set_async_generator_state(id, AsyncGeneratorState::Completed);
                    self.finish_coroutine(id);
                }
                if self.async_generator_state(id) == Some(AsyncGeneratorState::Completed) {
                    self.reject_promise(promise, &reason);
                    return Ok(result);
                }
                self.enqueue_async_request(id, Resume::Throw(reason.clone()), promise);
                if state == AsyncGeneratorState::SuspendedYield {
                    self.async_generator_resume(dom, id, Resume::Throw(reason));
                }
            }
            Resume::Return(value) => {
                self.enqueue_async_request(id, Resume::Return(value.clone()), promise);
                match state {
                    AsyncGeneratorState::SuspendedStart | AsyncGeneratorState::Completed => {
                        if state == AsyncGeneratorState::SuspendedStart {
                            self.finish_coroutine(id);
                        }
                        self.set_async_generator_state(id, AsyncGeneratorState::AwaitingReturn);
                        self.async_generator_await_return(dom, id);
                    }
                    AsyncGeneratorState::SuspendedYield => {
                        self.async_generator_resume(dom, id, Resume::Return(value));
                    }
                    AsyncGeneratorState::Executing | AsyncGeneratorState::AwaitingReturn => {}
                }
            }
        }
        Ok(result)
    }

    /// AsyncGeneratorResume: run the body for `completion`.
    fn async_generator_resume(&mut self, dom: &mut Dom, id: usize, completion: Resume) {
        self.set_async_generator_state(id, AsyncGeneratorState::Executing);
        self.async_generator_drive(dom, id, completion);
    }

    /// Run the body until it yields, returns, throws, or awaits. A yield with
    /// more requests queued resumes straight away with the next one.
    fn async_generator_drive(&mut self, dom: &mut Dom, id: usize, mut resume: Resume) {
        loop {
            match self.drive(dom, id, resume) {
                Ok(CoStep::Await(value)) => match self.await_value(dom, id, &value) {
                    Ok(()) => return,
                    // A failing `PromiseResolve` throws at the `await`.
                    Err(error) => resume = Resume::Throw(self.error_value(&error)),
                },
                Ok(CoStep::Yield(value)) => {
                    // AsyncGeneratorYield: settle the request that asked for it.
                    self.async_generator_complete_step(id, Ok(value), false);
                    match self.front_completion(id) {
                        Some(next) => resume = next,
                        None => {
                            self.set_async_generator_state(
                                id,
                                AsyncGeneratorState::SuspendedYield,
                            );
                            return;
                        }
                    }
                }
                Ok(CoStep::Complete(value)) => {
                    self.async_generator_finish(dom, id, Ok(value));
                    return;
                }
                Err(error) => {
                    let reason = self.error_value(&error);
                    self.async_generator_finish(dom, id, Err(reason));
                    return;
                }
            }
        }
    }

    /// Continue a body that was suspended on an `await`, or finish an
    /// `AwaitingReturn` settlement. Both reach here through `AsyncResume`.
    pub(super) fn async_generator_continue(&mut self, dom: &mut Dom, id: usize, resume: Resume) {
        if self.async_generator_state(id) == Some(AsyncGeneratorState::AwaitingReturn) {
            // AsyncGeneratorAwaitReturn's fulfilled and rejected closures.
            self.set_async_generator_state(id, AsyncGeneratorState::Completed);
            match resume {
                Resume::Throw(reason) => self.async_generator_complete_step(id, Err(reason), true),
                Resume::Next(value) | Resume::Return(value) => {
                    self.async_generator_complete_step(id, Ok(value), true);
                }
            }
            self.async_generator_drain(dom, id);
            return;
        }
        self.async_generator_drive(dom, id, resume);
    }

    /// AsyncGeneratorCompleteStep: settle the request at the head of the queue.
    fn async_generator_complete_step(
        &mut self,
        id: usize,
        completion: Result<JsValue, JsValue>,
        done: bool,
    ) {
        let Some(request) = self
            .async_generators
            .get_mut(&id)
            .and_then(|generator| generator.queue.pop_front())
        else {
            return;
        };
        match completion {
            Ok(value) => self.settle_iterator_result(request.promise, value, done),
            Err(reason) => self.reject_promise(request.promise, &reason),
        }
    }

    /// AsyncGeneratorStart's ending: the body is done, so it completes its
    /// request and answers the rest of the queue (ECMA-262 27.6.3.2 steps 7-11).
    fn async_generator_finish(
        &mut self,
        dom: &mut Dom,
        id: usize,
        completion: Result<JsValue, JsValue>,
    ) {
        self.set_async_generator_state(id, AsyncGeneratorState::Completed);
        self.async_generator_complete_step(id, completion, true);
        self.async_generator_drain(dom, id);
    }

    /// AsyncGeneratorDrainQueue: a completed generator answers its queued
    /// requests in order; a queued `return` starts awaiting its operand.
    fn async_generator_drain(&mut self, dom: &mut Dom, id: usize) {
        loop {
            match self.front_completion(id) {
                None => return,
                Some(Resume::Return(_)) => {
                    self.set_async_generator_state(id, AsyncGeneratorState::AwaitingReturn);
                    self.async_generator_await_return(dom, id);
                    return;
                }
                Some(Resume::Throw(reason)) => {
                    self.async_generator_complete_step(id, Err(reason), true);
                }
                Some(Resume::Next(_)) => {
                    self.async_generator_complete_step(id, Ok(JsValue::Undefined), true);
                }
            }
        }
    }

    /// AsyncGeneratorAwaitReturn: await the operand of the `return` request at
    /// the head of the queue. Its settlement comes back through
    /// [`Self::async_generator_continue`].
    fn async_generator_await_return(&mut self, dom: &mut Dom, id: usize) {
        let Some(Resume::Return(value)) = self.front_completion(id) else {
            return;
        };
        if let Err(error) = self.await_value(dom, id, &value) {
            let reason = self.error_value(&error);
            self.set_async_generator_state(id, AsyncGeneratorState::Completed);
            self.async_generator_complete_step(id, Err(reason), true);
            self.async_generator_drain(dom, id);
        }
    }

    /// Resolve `promise` with `{ value, done }`.
    fn settle_iterator_result(&mut self, promise: usize, value: JsValue, done: bool) {
        match self.iteration_result(value, done) {
            Ok(result) => self.resolve_promise(promise, &result),
            Err(error) => {
                let reason = self.error_value(&error);
                self.reject_promise(promise, &reason);
            }
        }
    }

    // ------------------------------------------------------ async-from-sync

    /// GetIterator(value, async) (ECMA-262 7.4.3): `@@asyncIterator` when the
    /// value has one, otherwise the sync iterator wrapped by
    /// CreateAsyncFromSyncIterator (27.1.4.1).
    pub(super) fn async_delegate_iterator(
        &mut self,
        dom: &mut Dom,
        value: &JsValue,
    ) -> Result<(ObjectId, ObjectId), JsError> {
        if let JsValue::Object(target) = value {
            let symbol = JsSymbol::well_known("@@asyncIterator");
            match self.get_symbol_value(dom, *target, &symbol)? {
                JsValue::Undefined | JsValue::Null => {}
                JsValue::Object(method) if Self::is_callable_object(method, &self.realm) => {
                    let JsValue::Object(iterator) =
                        self.call_with_this(dom, method, &[], value.clone())?
                    else {
                        return Err(JsError::type_error("async iterator is not an object"));
                    };
                    let JsValue::Object(next) = self.get_member(dom, iterator, "next")? else {
                        return Err(JsError::type_error("iterator has no callable 'next'"));
                    };
                    if !Self::is_callable_object(next, &self.realm) {
                        return Err(JsError::type_error("iterator has no callable 'next'"));
                    }
                    return Ok((iterator, next));
                }
                _ => return Err(JsError::type_error("@@asyncIterator is not callable")),
            }
        }
        let (iterator, next) = self.delegate_iterator(dom, value)?;
        self.ensure_heap_capacity(1)?;
        let wrapper = self.realm.async_from_sync_iterator(iterator, next);
        match self.get_member(dom, wrapper, "next")? {
            JsValue::Object(wrapper_next) => Ok((wrapper, wrapper_next)),
            _ => Err(JsError::type_error("iterator has no callable 'next'")),
        }
    }

    /// `%AsyncFromSyncIteratorPrototype%.next`, `.return` and `.throw`
    /// (ECMA-262 27.1.6). Failures reject the returned promise.
    pub(super) fn async_from_sync_method(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        method: AsyncFromSyncMethod,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match self.async_from_sync_attempt(dom, receiver, method, arguments) {
            Ok(promise) => Ok(promise),
            Err(error) if error.kind() == JsErrorKind::ResourceLimit => Err(error),
            // IfAbruptRejectPromise.
            Err(error) => {
                let reason = self.error_value(&error);
                let (promise, result) = self.create_promise()?;
                self.reject_promise(promise, &reason);
                Ok(result)
            }
        }
    }

    fn async_from_sync_attempt(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        method: AsyncFromSyncMethod,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let Some(ObjectHost::AsyncFromSyncIterator { iterator, next }) =
            self.realm.host(receiver)
        else {
            return Err(JsError::type_error(
                "incompatible AsyncFromSyncIterator receiver",
            ));
        };
        // A method's value argument is passed only when the caller gave one.
        let value: Vec<JsValue> = arguments.iter().take(1).cloned().collect();
        match method {
            AsyncFromSyncMethod::Next => {
                let result = self.call_with_this(dom, next, &value, JsValue::Object(iterator))?;
                let JsValue::Object(result) = result else {
                    return Err(JsError::type_error("iterator result is not an object"));
                };
                self.async_from_sync_continuation(dom, result, iterator, true)
            }
            AsyncFromSyncMethod::Return => match self.get_method(dom, iterator, "return")? {
                None => {
                    let value = arguments.first().cloned().unwrap_or(JsValue::Undefined);
                    let (promise, result) = self.create_promise()?;
                    self.settle_iterator_result(promise, value, true);
                    Ok(result)
                }
                Some(return_method) => {
                    let result =
                        self.call_with_this(dom, return_method, &value, JsValue::Object(iterator))?;
                    let JsValue::Object(result) = result else {
                        return Err(JsError::type_error("iterator result is not an object"));
                    };
                    self.async_from_sync_continuation(dom, result, iterator, false)
                }
            },
            AsyncFromSyncMethod::Throw => match self.get_method(dom, iterator, "throw")? {
                None => {
                    // Without `throw`, the sync iterator is closed and the
                    // promise rejects with a TypeError.
                    self.close_iterator_object(dom, iterator)?;
                    Err(JsError::type_error(
                        "the iterator does not provide a 'throw' method",
                    ))
                }
                Some(throw_method) => {
                    let result =
                        self.call_with_this(dom, throw_method, &value, JsValue::Object(iterator))?;
                    let JsValue::Object(result) = result else {
                        return Err(JsError::type_error("iterator result is not an object"));
                    };
                    self.async_from_sync_continuation(dom, result, iterator, true)
                }
            },
        }
    }

    /// AsyncFromSyncIteratorContinuation (ECMA-262 27.1.6.4): await the sync
    /// result's value and unwrap it into an iterator result.
    fn async_from_sync_continuation(
        &mut self,
        dom: &mut Dom,
        result: ObjectId,
        iterator: ObjectId,
        close_on_rejection: bool,
    ) -> Result<JsValue, JsError> {
        let done = self.get_member(dom, result, "done")?.is_truthy();
        let value = self.get_member(dom, result, "value")?;
        let wrapper = match self.promise_resolve(dom, &value) {
            Ok(wrapper) => wrapper,
            Err(error) => {
                // A value that cannot be awaited closes the sync iterator.
                if !done && close_on_rejection {
                    let _ = self.close_iterator_object(dom, iterator);
                }
                return Err(error);
            }
        };
        self.ensure_heap_capacity(2)?;
        let on_fulfilled = self.realm.async_from_sync_value(done);
        let on_rejected = if done || !close_on_rejection {
            JsValue::Undefined
        } else {
            JsValue::Object(self.realm.async_from_sync_close(iterator))
        };
        self.perform_promise_then(
            wrapper,
            &[JsValue::Object(on_fulfilled), on_rejected],
        )
    }

    /// The `[[Get]]` of an optional method (ECMA-262 7.3.11 GetMethod).
    fn get_method(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        name: &str,
    ) -> Result<Option<ObjectId>, JsError> {
        match self.get_member(dom, object, name)? {
            JsValue::Undefined | JsValue::Null => Ok(None),
            JsValue::Object(method) if Self::is_callable_object(method, &self.realm) => {
                Ok(Some(method))
            }
            _ => Err(JsError::type_error(format!("{name} is not a function"))),
        }
    }
}
