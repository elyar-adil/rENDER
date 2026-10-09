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
use crate::runtime::coroutine_run::Resume;
use crate::runtime::builtins::promise::PromiseState;
use crate::runtime::types::EnvironmentRecord;
use crate::runtime::types::JsMicrotask;
use crate::value::ObjectHost;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::rc::Rc;

pub(super) fn gc_trace_enabled() -> bool {
    pub(super) static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("RENDER_JS_GC").is_ok())
}

pub(super) fn listener_values(
    entry: &BTreeMap<String, Vec<super::types::Listener>>,
) -> impl Iterator<Item = &ObjectId> {
    entry.values().flatten().map(|listener| &listener.callback)
}

pub(super) fn mark_value(
    runtime: &JsRuntime,
    marked: &mut [bool],
    work: &mut Vec<ObjectId>,
    marked_environments: &mut BTreeSet<usize>,
    value: &JsValue,
) {
    if let JsValue::Object(object) = value {
        mark_object(runtime, marked, work, marked_environments, *object);
    }
}

pub(super) fn mark_object(
    runtime: &JsRuntime,
    marked: &mut [bool],
    work: &mut Vec<ObjectId>,
    marked_environments: &mut BTreeSet<usize>,
    object: ObjectId,
) {
    let index = object.as_usize();
    if index >= marked.len() || marked[index] {
        return;
    }
    marked[index] = true;
    let Some(current) = runtime.realm.objects().get(index) else {
        return;
    };
    mark_host(runtime, marked, work, marked_environments, &current.host);
    work.push(object);
}

pub(super) fn mark_environment(
    runtime: &JsRuntime,
    marked: &mut [bool],
    work: &mut Vec<ObjectId>,
    marked_environments: &mut BTreeSet<usize>,
    environment: &Rc<RefCell<EnvironmentRecord>>,
) {
    let identity = std::rc::Rc::as_ptr(environment) as usize;
    if !marked_environments.insert(identity) {
        return;
    }
    for binding in environment.borrow().bindings.values() {
        mark_value(runtime, marked, work, marked_environments, &binding.value);
    }
}

pub(super) fn mark_function(
    runtime: &JsRuntime,
    marked: &mut [bool],
    work: &mut Vec<ObjectId>,
    marked_environments: &mut BTreeSet<usize>,
    function_index: usize,
) {
    let Some(function) = runtime.functions.get(function_index) else {
        return;
    };
    for environment in &function.captured_environment {
        mark_environment(runtime, marked, work, marked_environments, environment);
    }
    // Class metadata keeps the home object and parent constructor alive, and
    // the class body environment needed by field initializers.
    if let Some(class) = &function.class {
        if let Some(home) = class.home_object {
            mark_object(runtime, marked, work, marked_environments, home);
        }
        if let Some(super_constructor) = class.super_constructor {
            mark_object(
                runtime,
                marked,
                work,
                marked_environments,
                super_constructor,
            );
        }
        for environment in &class.environment {
            mark_environment(runtime, marked, work, marked_environments, environment);
        }
    }
}

pub(super) fn mark_promise(
    runtime: &JsRuntime,
    marked: &mut [bool],
    work: &mut Vec<ObjectId>,
    marked_environments: &mut BTreeSet<usize>,
    promise_index: usize,
) {
    let Some(promise) = runtime.promises.get(promise_index) else {
        return;
    };
    match &promise.state {
        PromiseState::Fulfilled(value) | PromiseState::Rejected(value) => {
            mark_value(runtime, marked, work, marked_environments, value);
        }
        PromiseState::Pending => {}
    }
    for reaction in &promise.reactions {
        if let Some(handler) = reaction.on_fulfilled {
            mark_object(runtime, marked, work, marked_environments, handler);
        }
        if let Some(handler) = reaction.on_rejected {
            mark_object(runtime, marked, work, marked_environments, handler);
        }
    }
}

pub(super) fn mark_host(
    runtime: &JsRuntime,
    marked: &mut [bool],
    work: &mut Vec<ObjectId>,
    marked_environments: &mut BTreeSet<usize>,
    host: &ObjectHost,
) {
    match host {
        ObjectHost::SymbolInstance(_)
        | ObjectHost::Ordinary
        | ObjectHost::Array
        | ObjectHost::Document(_)
        | ObjectHost::Node(_)
        | ObjectHost::ClassList(_)
        | ObjectHost::DataSet(_)
        | ObjectHost::CssStyleDeclaration(_)
        | ObjectHost::NativeFunction(_)
        | ObjectHost::Generator(_)
        | ObjectHost::AsyncResume { .. }
        | ObjectHost::AsyncGenerator(_)
        | ObjectHost::AsyncFromSyncValue { .. }
        | ObjectHost::PromiseConstructor
        | ObjectHost::ObjectConstructor
        | ObjectHost::FunctionConstructor
        | ObjectHost::StringConstructor
        | ObjectHost::NumberConstructor
        | ObjectHost::BooleanConstructor
        | ObjectHost::DateConstructor
        | ObjectHost::SymbolConstructor
        | ObjectHost::ArrayConstructor
        | ObjectHost::StringPrimitive(_)
        | ObjectHost::NumberPrimitive(_)
        | ObjectHost::BooleanPrimitive(_)
        | ObjectHost::DateInstance(_)
        | ObjectHost::NamedNodeMap(_)
        | ObjectHost::Attr { .. }
        | ObjectHost::RegExp(_)
        | ObjectHost::RegExpConstructor
        | ObjectHost::EventConstructor
        | ObjectHost::DomConstructor
        | ObjectHost::DomNodeConstructor(_)
        | ObjectHost::ImageConstructor
        | ObjectHost::IntersectionObserverConstructor
        | ObjectHost::MutationObserverConstructor
        | ObjectHost::Location(_)
        | ObjectHost::ErrorConstructor(_)
        | ObjectHost::ErrorInstance
        | ObjectHost::DomExceptionConstructor
        | ObjectHost::DomException { .. }
        | ObjectHost::AggregateErrorConstructor
        | ObjectHost::CollectionConstructor(_)
        | ObjectHost::TypedArrayConstructor(_)
        | ObjectHost::TypedArray { .. }
        | ObjectHost::DataView { .. }
        | ObjectHost::DataViewConstructor
        | ObjectHost::ArrayBufferHost(_)
        | ObjectHost::ArrayBufferConstructor
        | ObjectHost::TextDecoder { .. }
        | ObjectHost::TextEncoderConstructor
        | ObjectHost::TextDecoderConstructor
        | ObjectHost::UrlConstructor
        | ObjectHost::UrlSearchParamsConstructor
        | ObjectHost::UrlInstance(_)
        | ObjectHost::VideoConstructor
        | ObjectHost::XmlHttpRequestConstructor
        | ObjectHost::XmlHttpRequest(_)
        | ObjectHost::AbortControllerConstructor
        | ObjectHost::AbortController
        | ObjectHost::AbortSignal
        | ObjectHost::Storage
        | ObjectHost::FormDataConstructor
        | ObjectHost::FormData { .. }
        | ObjectHost::ResponseConstructor
        | ObjectHost::Response { .. } => {}
        ObjectHost::Blob { .. } | ObjectHost::BlobConstructor | ObjectHost::ProxyConstructor => {}
        ObjectHost::Proxy { target, handler } => {
            mark_object(runtime, marked, work, marked_environments, *target);
            mark_object(runtime, marked, work, marked_environments, *handler);
        }
        ObjectHost::VideoElement(state) => {
            // Pending `play()` promises stay reachable until the media load
            // settles, mirroring the fetch-transfer target roots below.
            for play_promise in &state.pending_play_promises {
                mark_object(
                    runtime,
                    marked,
                    work,
                    marked_environments,
                    play_promise.object,
                );
            }
        }
        ObjectHost::ResponseHeaders { owner } => {
            mark_object(runtime, marked, work, marked_environments, *owner);
        }
        ObjectHost::BoundFunction { receiver, .. } => {
            mark_object(runtime, marked, work, marked_environments, *receiver);
        }
        ObjectHost::BoundCallable {
            target,
            receiver,
            arguments,
        } => {
            mark_object(runtime, marked, work, marked_environments, *target);
            mark_value(runtime, marked, work, marked_environments, receiver);
            for argument in arguments {
                mark_value(runtime, marked, work, marked_environments, argument);
            }
        }
        ObjectHost::UserFunction(index) | ObjectHost::ArrowFunction(index) => {
            mark_function(runtime, marked, work, marked_environments, *index);
        }
        ObjectHost::IntersectionObserver { callback, .. }
        | ObjectHost::MutationObserver { callback, .. } => {
            mark_object(runtime, marked, work, marked_environments, *callback);
        }
        ObjectHost::MediaQueryList { listeners, .. } => {
            for listener in listeners {
                mark_object(runtime, marked, work, marked_environments, *listener);
            }
        }
        ObjectHost::Promise(index) | ObjectHost::PromiseSettler { promise: index, .. } => {
            mark_promise(runtime, marked, work, marked_environments, *index);
        }
        ObjectHost::AsyncFromSyncIterator { iterator, next } => {
            mark_object(runtime, marked, work, marked_environments, *iterator);
            mark_object(runtime, marked, work, marked_environments, *next);
        }
        ObjectHost::AsyncFromSyncClose { iterator } => {
            mark_object(runtime, marked, work, marked_environments, *iterator);
        }
        ObjectHost::Collection { entries, .. } => {
            for (key, value) in entries {
                mark_value(runtime, marked, work, marked_environments, key);
                mark_value(runtime, marked, work, marked_environments, value);
            }
        }
        ObjectHost::CollectionIterator { values, .. } => {
            for value in values {
                mark_value(runtime, marked, work, marked_environments, value);
            }
        }
        ObjectHost::IteratorHelper {
            source,
            source_next,
            callback,
            inner,
            inner_next,
            buffer,
            ..
        } => {
            for object in [source, source_next, callback, inner, inner_next]
                .into_iter()
                .flatten()
            {
                mark_object(runtime, marked, work, marked_environments, *object);
            }
            for value in buffer {
                mark_value(runtime, marked, work, marked_environments, value);
            }
        }
        ObjectHost::UrlSearchParams { owner, .. } => {
            if let Some(owner) = owner {
                mark_object(runtime, marked, work, marked_environments, *owner);
            }
        }
    }
}

impl JsRuntime {
    pub(super) fn ensure_heap_capacity(&mut self, additional: usize) -> Result<(), JsError> {
        if self.realm.object_count().saturating_add(additional) > self.limits.max_heap_objects {
            // Reclaim garbage before failing: script batches load many bundles
            // into one shared realm, and every earlier evaluation's dead
            // objects would otherwise exhaust the heap permanently.
            self.collect_garbage();
            if self.realm.object_count().saturating_add(additional) > self.limits.max_heap_objects {
                return Err(JsError::resource("JavaScript object heap limit exceeded"));
            }
        }
        Ok(())
    }

    /// Mark-reclaim dead object slots, keeping every live identity stable.
    ///
    /// The interpreter stores objects in an append-only arena, so a sweep must
    /// never move or reuse identities. Unmarked slots are rewritten to empty
    /// ordinary objects; any stale bookkeeping that still references one reads
    /// an inert object instead of corrupted state. Roots cover the realm, DOM
    /// wrapper identities, event listeners/handlers, timers, observers, queued
    /// microtasks, and the active variable/this stacks, and the mark walks
    /// property values, prototypes, closures' captured environments, promise
    /// settlements, and bound-callable receivers/arguments.
    pub fn collect_garbage(&mut self) -> usize {
        let total = self.realm.objects().len();
        if total == 0 {
            return 0;
        }
        let mut marked = vec![false; total];
        let mut work: Vec<ObjectId> = Vec::new();
        let mut marked_environments: BTreeSet<usize> = BTreeSet::new();
        for root in self.realm.gc_identity_roots() {
            mark_object(self, &mut marked, &mut work, &mut marked_environments, root);
        }
        for root in &self.transient_roots {
            mark_object(
                self,
                &mut marked,
                &mut work,
                &mut marked_environments,
                *root,
            );
        }
        for listeners in self.event_listeners.values().flat_map(listener_values) {
            mark_object(
                self,
                &mut marked,
                &mut work,
                &mut marked_environments,
                *listeners,
            );
        }
        for handler in self.event_handlers.values().flat_map(BTreeMap::values) {
            mark_object(
                self,
                &mut marked,
                &mut work,
                &mut marked_environments,
                *handler,
            );
        }
        for handler in self
            .window_event_handlers
            .values()
            .flatten()
            .map(|listener| &listener.callback)
        {
            mark_object(
                self,
                &mut marked,
                &mut work,
                &mut marked_environments,
                *handler,
            );
        }
        for timer in self.timers.values() {
            mark_object(
                self,
                &mut marked,
                &mut work,
                &mut marked_environments,
                timer.callback,
            );
        }
        for observer in &self.intersection_observers {
            mark_object(
                self,
                &mut marked,
                &mut work,
                &mut marked_environments,
                *observer,
            );
        }
        for observer in &self.mutation_observers {
            mark_object(
                self,
                &mut marked,
                &mut work,
                &mut marked_environments,
                *observer,
            );
        }
        // Pending network transfers keep their promise or XHR object alive so
        // a late settle still reaches registered reactions and callbacks.
        for target in self.pending_fetch_targets.values() {
            mark_object(
                self,
                &mut marked,
                &mut work,
                &mut marked_environments,
                *target,
            );
        }
        for microtask in &self.pending_microtasks {
            match microtask {
                JsMicrotask::Callback(callback) => {
                    mark_object(
                        self,
                        &mut marked,
                        &mut work,
                        &mut marked_environments,
                        *callback,
                    );
                }
                JsMicrotask::IntersectionObserver(observer)
                | JsMicrotask::MutationObserver(observer)
                | JsMicrotask::MediaQueryListChange(observer) => {
                    mark_object(
                        self,
                        &mut marked,
                        &mut work,
                        &mut marked_environments,
                        *observer,
                    );
                }
                JsMicrotask::PromiseReaction {
                    handler,
                    argument,
                    fulfilled: _,
                    result_promise,
                } => {
                    if let Some(callback) = handler {
                        mark_object(
                            self,
                            &mut marked,
                            &mut work,
                            &mut marked_environments,
                            *callback,
                        );
                    }
                    mark_value(
                        self,
                        &mut marked,
                        &mut work,
                        &mut marked_environments,
                        argument,
                    );
                    mark_promise(
                        self,
                        &mut marked,
                        &mut work,
                        &mut marked_environments,
                        *result_promise,
                    );
                }
            }
        }
        for record in self.modules.values() {
            mark_environment(
                self,
                &mut marked,
                &mut work,
                &mut marked_environments,
                &record.environment,
            );
        }
        // A suspended generator or async function is kept alive by its saved
        // scope chain, which also holds the hidden bindings that stand in for
        // its registers. (A running one is covered by `self.environment`.)
        for coroutine in self.coroutines.iter().flatten() {
            for environment in &coroutine.environment {
                mark_environment(
                    self,
                    &mut marked,
                    &mut work,
                    &mut marked_environments,
                    environment,
                );
            }
        }
        // Queued async generator requests keep their promises and resumption
        // values alive until they are settled.
        for generator in self.async_generators.values() {
            for request in &generator.queue {
                let (Resume::Next(value) | Resume::Throw(value) | Resume::Return(value)) =
                    &request.completion;
                mark_value(self, &mut marked, &mut work, &mut marked_environments, value);
                mark_promise(
                    self,
                    &mut marked,
                    &mut work,
                    &mut marked_environments,
                    request.promise,
                );
            }
        }
        // Collecting mid-execution must also treat the active scopes as
        // roots; between scripts these are empty.
        for scope in &self.environment {
            for binding in scope.borrow().bindings.values() {
                mark_value(
                    self,
                    &mut marked,
                    &mut work,
                    &mut marked_environments,
                    &binding.value,
                );
            }
        }
        while let Some(object) = work.pop() {
            let index = object.as_usize();
            let Some(current) = self.realm.objects().get(index) else {
                continue;
            };
            if let Some(prototype) = current.prototype() {
                mark_object(
                    self,
                    &mut marked,
                    &mut work,
                    &mut marked_environments,
                    prototype,
                );
            }
            for id in current.property_object_references() {
                mark_object(self, &mut marked, &mut work, &mut marked_environments, id);
            }
        }

        let before = self.realm.object_count();
        let reclaimed = self.realm.sweep_unmarked(&marked);
        if gc_trace_enabled() {
            eprintln!(
                "render-core js gc: reclaimed {reclaimed} of {before} live objects (heap now {})",
                self.realm.object_count()
            );
        }
        reclaimed
    }
}
