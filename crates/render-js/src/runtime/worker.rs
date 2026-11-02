//! Dedicated Web Workers (HTML §10.2).
//!
//! A worker is an isolated global scope in its own realm: it has no `window`,
//! no `document`, and none of the page's bindings. It runs on the page's event
//! loop, interleaved at task boundaries, because the interpreter is
//! single-threaded. That is a same-thread worker, not a second OS thread: two
//! workers do not run in parallel, but each has its own global, its own
//! microtask/timer realm, and messages cross only as structured clones.
//!
//! Script loading resolves the URL against the page base and goes through the
//! embedding's ordinary network path: a `data:` URL is decoded here, an
//! embedding with a [`DocumentLoader`](super::DocumentLoader) supplies the text
//! synchronously, and otherwise the script is queued as a `PendingFetch` and
//! starts when `settle_fetch` completes it. A failed load fires `error` at the
//! `Worker` object; it never throws from the constructor.
//!
//! The `Worker` object and the worker global are both `EventTarget`s.
//! `postMessage` structured-clones at post time and again when the message is
//! delivered into the other realm, so the receiver gets objects whose
//! prototypes belong to its own realm. A transfer list moves `ArrayBuffer`
//! bytes and detaches the source, as the standard requires.

use std::collections::BTreeMap;

use render_dom::Dom;
use url::Url;

use super::builtins::dom_exception::DomExceptionName;
use super::builtins::structured_clone::TransferMap;
use super::nested::{BrowsingContextRecord, InternalTask};
use super::types::{EventFlags, Listener};
use super::{INTRINSICS_PRELUDE_SOURCE, JsRuntime};
use crate::CompiledScript;
use crate::runtime::build_line_starts;
use crate::value::ObjectHost;
use crate::value::TypedBuffer;
use crate::{JsError, JsValue, ObjectId, RealmId};

/// A dedicated worker's id in `JsRuntime::workers`.
pub(super) type WorkerId = u64;

/// Where a worker is in its lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WorkerState {
    /// The classic script has not finished loading.
    Loading,
    /// The script ran; messages are delivered as tasks.
    Running,
    /// `terminate()` or `close()` stopped the worker.
    Terminated,
}

/// One dedicated worker: its page-visible object and its isolated realm.
#[derive(Debug)]
pub(super) struct WorkerRecord {
    /// The realm that created the worker and receives its `message`/`error`.
    pub(super) owner: RealmId,
    /// The worker's own realm.
    pub(super) realm: RealmId,
    /// The `Worker` object the page holds.
    pub(super) object: ObjectId,
    pub(super) url: Url,
    pub(super) state: WorkerState,
    /// Messages posted before the script finished loading.
    pub(super) queue: Vec<JsValue>,
}

/// A transfer list resolved before serialization: the buffers the clone takes
/// and the source buffers to detach once the whole clone succeeds.
struct Transfers {
    moved: TransferMap,
    sources: Vec<TypedBuffer>,
}

/// Objects a worker realm's built-ins need, with headroom. Measured at the
/// current worker global set.
const WORKER_REALM_HEAP_OBJECTS: usize = 2_048;

impl JsRuntime {
    pub(super) fn dispatch_worker_native(
        &mut self,
        dom: &mut Dom,
        function: crate::value::NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        use crate::value::NativeFunction;
        match function {
            NativeFunction::WorkerPostMessage => self.worker_post_message(dom, receiver, arguments),
            NativeFunction::WorkerTerminate => self.worker_terminate(receiver),
            NativeFunction::WorkerAddEventListener => {
                self.worker_add_event_listener(receiver, arguments)
            }
            NativeFunction::WorkerRemoveEventListener => {
                self.worker_remove_event_listener(receiver, arguments)
            }
            NativeFunction::WorkerScopePostMessage => {
                self.worker_scope_post_message(dom, receiver, arguments)
            }
            NativeFunction::WorkerScopeImportScripts => {
                self.worker_scope_import_scripts(dom, receiver, arguments)
            }
            NativeFunction::WorkerScopeClose => self.worker_scope_close(receiver),
            other => self.dispatch_fetch_native(dom, other, receiver, arguments),
        }
    }

    /// `Worker` construction: create the object and realm, then queue the
    /// classic script load. URL failures throw synchronously (the `WebIDL`
    /// argument conversion), but loading and execution failures are events.
    pub(super) fn worker_constructor(
        &mut self,
        dom: &mut Dom,
        constructor: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let url_value = crate::runtime::convert::required_argument(arguments, 0, "Worker")?;
        let requested = self.to_string_value(dom, url_value)?.to_lossy();
        let url = self.resolve_fetch_url(&requested).map_err(|message| {
            JsError::type_error(format!("Worker URL {requested:?}: {message}"))
        })?;
        let name = match arguments.get(1) {
            Some(JsValue::Object(options)) => match self.realm.get_property(*options, "name") {
                Some(JsValue::Undefined) | None => String::new(),
                Some(value) => self.to_string_value(dom, &value)?.to_lossy(),
            },
            _ => String::new(),
        };
        let owner = self.realm.current_realm();
        let prototype = self
            .realm
            .get_property(constructor, "prototype")
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            });
        self.ensure_heap_capacity(1)?;
        let object = self.realm.create_object(prototype);
        let realm = self.create_worker_realm(dom, owner, &url, &name)?;
        let id = self.next_worker_id;
        self.next_worker_id += 1;
        if let Some(host) = self.realm.host_mut(object) {
            *host = ObjectHost::Worker(id);
        }
        self.workers.insert(
            id,
            WorkerRecord {
                owner,
                realm,
                object,
                url,
                state: WorkerState::Loading,
                queue: Vec::new(),
            },
        );
        self.schedule(InternalTask::WorkerLoad { worker: id });
        Ok(JsValue::Object(object))
    }

    /// The realm of a worker, with the host globals and the ECMAScript prelude.
    fn create_worker_realm(
        &mut self,
        dom: &mut Dom,
        owner: RealmId,
        url: &Url,
        name: &str,
    ) -> Result<RealmId, JsError> {
        self.ensure_heap_capacity(WORKER_REALM_HEAP_OBJECTS)?;
        let realm = self.realm.create_worker_realm(url, name);
        let origin = self.contexts[owner.0].origin.clone();
        self.push_realm_state(BrowsingContextRecord {
            document: None,
            base: url.clone(),
            origin,
            frame: None,
        });
        self.with_realm(realm, |runtime| {
            runtime.run_self_hosted(dom, INTRINSICS_PRELUDE_SOURCE)
        })?;
        Ok(realm)
    }

    /// `Worker.prototype.postMessage(message, transfer)` (HTML §10.2.4).
    pub(super) fn worker_post_message(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let Some(ObjectHost::Worker(worker)) = self.realm.host(receiver) else {
            return Err(JsError::type_error(
                "Worker.postMessage called on an incompatible receiver",
            ));
        };
        if self
            .workers
            .get(&worker)
            .is_none_or(|record| record.state == WorkerState::Terminated)
        {
            return Ok(JsValue::Undefined);
        }
        let value = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let transfers = self.worker_transfers(dom, arguments.get(1))?;
        let data = self.structured_clone_value_with_transfers(
            &value,
            &mut BTreeMap::new(),
            Some(&transfers.moved),
        )?;
        for source in &transfers.sources {
            source.detach();
        }
        let Some(record) = self.workers.get_mut(&worker) else {
            return Ok(JsValue::Undefined);
        };
        match record.state {
            WorkerState::Loading => record.queue.push(data),
            WorkerState::Running => self.schedule(InternalTask::WorkerMessage { worker, data }),
            WorkerState::Terminated => {}
        }
        Ok(JsValue::Undefined)
    }

    /// `Worker.prototype.terminate()`.
    pub(super) fn worker_terminate(&mut self, receiver: ObjectId) -> Result<JsValue, JsError> {
        let Some(ObjectHost::Worker(worker)) = self.realm.host(receiver) else {
            return Err(JsError::type_error(
                "Worker.terminate called on an incompatible receiver",
            ));
        };
        self.terminate_worker(worker);
        Ok(JsValue::Undefined)
    }

    /// `Worker.prototype.addEventListener` / `removeEventListener`. A worker
    /// is an `EventTarget` but not a DOM node, so its registrations are kept
    /// outside the DOM event table.
    pub(super) fn worker_add_event_listener(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        self.require_worker_target(receiver, "addEventListener")?;
        let event_type =
            crate::runtime::convert::required_argument(arguments, 0, "addEventListener")?
                .to_js_string();
        let Some(listener) = self.listener_from_arguments(arguments, "addEventListener", false)?
        else {
            return Ok(JsValue::Undefined);
        };
        Self::insert_listener(
            self.worker_event_listeners
                .entry(receiver)
                .or_default()
                .entry(event_type.to_key())
                .or_default(),
            listener,
        );
        Ok(JsValue::Undefined)
    }

    pub(super) fn worker_remove_event_listener(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        self.require_worker_target(receiver, "removeEventListener")?;
        let event_type =
            crate::runtime::convert::required_argument(arguments, 0, "removeEventListener")?
                .to_js_string();
        let Some(listener) =
            self.listener_from_arguments(arguments, "removeEventListener", false)?
        else {
            return Ok(JsValue::Undefined);
        };
        if let Some(listeners) = self
            .worker_event_listeners
            .get_mut(&receiver)
            .and_then(|listeners| listeners.get_mut(&event_type.to_key()))
        {
            listeners.retain(|candidate| {
                !(candidate.callback == listener.callback && candidate.capture == listener.capture)
            });
        }
        Ok(JsValue::Undefined)
    }

    fn require_worker_target(&self, receiver: ObjectId, method: &str) -> Result<(), JsError> {
        if matches!(self.realm.host(receiver), Some(ObjectHost::Worker(_))) {
            Ok(())
        } else {
            Err(JsError::type_error(format!(
                "Worker.{method} called on an incompatible receiver"
            )))
        }
    }

    /// `DedicatedWorkerGlobalScope.postMessage(message, transfer)`.
    pub(super) fn worker_scope_post_message(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let worker = self.worker_for_global(receiver)?;
        if self
            .workers
            .get(&worker)
            .is_none_or(|record| record.state != WorkerState::Running)
        {
            return Ok(JsValue::Undefined);
        }
        let value = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let transfers = self.worker_transfers(dom, arguments.get(1))?;
        let data = self.structured_clone_value_with_transfers(
            &value,
            &mut BTreeMap::new(),
            Some(&transfers.moved),
        )?;
        for source in &transfers.sources {
            source.detach();
        }
        self.schedule(InternalTask::WorkerPageMessage { worker, data });
        Ok(JsValue::Undefined)
    }

    /// `DedicatedWorkerGlobalScope.importScripts(...urls)` (HTML §10.2.4): each
    /// URL is resolved against the worker's own base and executed
    /// synchronously in order, before the next one runs.
    pub(super) fn worker_scope_import_scripts(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let worker = self.worker_for_global(receiver)?;
        let Some(record) = self.workers.get(&worker) else {
            return Ok(JsValue::Undefined);
        };
        if record.state != WorkerState::Running {
            return Ok(JsValue::Undefined);
        }
        let base = record.url.clone();
        let base = base.join(".").unwrap_or(base);
        let mut sources = Vec::with_capacity(arguments.len());
        for argument in arguments {
            let requested = self.to_string_value(dom, argument)?.to_lossy();
            let url = base.join(&requested).map_err(|_| {
                JsError::dom(format!(
                    "NetworkError: importScripts URL {requested:?} is invalid"
                ))
            })?;
            let source = self.load_worker_script_source(&url).ok_or_else(|| {
                JsError::dom(format!("NetworkError: importScripts could not load {url}"))
            })?;
            sources.push((url, source));
        }
        for (url, source) in sources {
            let realm = self.workers[&worker].realm;
            self.with_realm(realm, |runtime| {
                runtime.execute_worker_source(dom, &source, &url)
            })?;
        }
        Ok(JsValue::Undefined)
    }

    /// `DedicatedWorkerGlobalScope.close()`: stop receiving any further work.
    pub(super) fn worker_scope_close(&mut self, receiver: ObjectId) -> Result<JsValue, JsError> {
        let worker = self.worker_for_global(receiver)?;
        self.terminate_worker(worker);
        Ok(JsValue::Undefined)
    }

    /// The worker whose global is `receiver`.
    fn worker_for_global(&self, receiver: ObjectId) -> Result<WorkerId, JsError> {
        let realm = self
            .realm_of_global(receiver)
            .ok_or_else(|| JsError::type_error("worker scope call on a non-worker global"))?;
        self.workers
            .iter()
            .find(|(_, record)| record.realm == realm)
            .map(|(id, _)| *id)
            .ok_or_else(|| JsError::type_error("worker scope call outside a worker"))
    }

    /// The worker realm that is current, if any. Used by error reporting to
    /// route an uncaught worker exception to the page-side `Worker` object.
    pub(super) fn current_worker(&self) -> Option<WorkerId> {
        let realm = self.realm.current_realm();
        self.workers
            .iter()
            .find(|(_, record)| record.realm == realm)
            .map(|(id, _)| *id)
    }

    /// Loads one worker's classic script. Called as an internal task so the
    /// constructor itself never touches the network.
    pub(super) fn run_worker_load(&mut self, dom: &mut Dom, worker: WorkerId) {
        let Some((state, url, object)) = self
            .workers
            .get(&worker)
            .map(|record| (record.state, record.url.clone(), record.object))
        else {
            return;
        };
        if state != WorkerState::Loading {
            return;
        }
        if url.scheme() == "data" {
            return match decode_data_url(&url) {
                Ok(source) => {
                    self.start_worker(dom, worker, &source, &url);
                }
                Err(message) => {
                    self.fail_worker_load(dom, worker, &message);
                }
            };
        }
        if let Some(source) = self.load_worker_script_source(&url) {
            self.start_worker(dom, worker, &source, &url);
            return;
        }
        if self.document_loader.is_some() {
            self.fail_worker_load(dom, worker, &format!("could not load worker script {url}"));
            return;
        }
        // No synchronous loader: queue the script through the same network
        // seam as `fetch()`. The embedding settles it by fetch id.
        let id = self.next_fetch_id;
        self.next_fetch_id += 1;
        self.pending_fetch_requests.push(crate::PendingFetch {
            id,
            url: url.to_string(),
            method: "GET".to_owned(),
            headers: Vec::new(),
            body: None,
            credentials: crate::FetchCredentials::SameOrigin,
        });
        self.pending_fetch_targets.insert(id, object);
        self.pending_worker_scripts.insert(id, worker);
    }

    /// The text of a worker script this runtime can load without a transfer.
    fn load_worker_script_source(&mut self, url: &Url) -> Option<String> {
        if url.scheme() == "data" {
            return decode_data_url(url).ok();
        }
        self.document_loader
            .as_mut()
            .and_then(|loader| loader.load_text(url))
    }

    /// Completes a worker-script transfer queued by `run_worker_load`. Returns
    /// whether `id` named a worker script rather than a page fetch.
    pub(super) fn settle_worker_script_fetch(
        &mut self,
        dom: &mut Dom,
        id: u64,
        outcome: Result<crate::FetchOutcome, String>,
    ) -> bool {
        let Some(worker) = self.pending_worker_scripts.remove(&id) else {
            return false;
        };
        self.pending_fetch_targets.remove(&id);
        let Some(record) = self.workers.get(&worker) else {
            return true;
        };
        if record.state != WorkerState::Loading {
            return true;
        }
        let url = record.url.clone();
        match outcome {
            Ok(response) if (200..300).contains(&response.status) => {
                let source = String::from_utf8_lossy(&response.body).into_owned();
                self.start_worker(dom, worker, &source, &url);
            }
            Ok(response) => {
                self.fail_worker_load(
                    dom,
                    worker,
                    &format!(
                        "worker script {} returned HTTP status {}",
                        url, response.status
                    ),
                );
            }
            Err(message) => {
                self.fail_worker_load(dom, worker, &format!("worker script {url}: {message}"));
            }
        }
        true
    }

    /// Runs the loaded classic script in the worker's own realm, then flushes
    /// messages that were posted while it was loading.
    fn start_worker(&mut self, dom: &mut Dom, worker: WorkerId, source: &str, url: &Url) {
        let Some(record) = self.workers.get_mut(&worker) else {
            return;
        };
        if record.state != WorkerState::Loading {
            return;
        }
        record.state = WorkerState::Running;
        let realm = record.realm;
        let _ = self.with_realm(realm, |runtime| {
            runtime.execute_worker_source(dom, source, url)
        });
        let Some(record) = self.workers.get_mut(&worker) else {
            return;
        };
        if record.state != WorkerState::Running {
            return;
        }
        for data in std::mem::take(&mut record.queue) {
            self.schedule(InternalTask::WorkerMessage { worker, data });
        }
    }

    /// Executes one classic script in the current worker realm. Unlike
    /// `execute`, this does not install the page prelude and does not report to
    /// the page's window; an escaping error goes to the `Worker` object.
    fn execute_worker_source(
        &mut self,
        dom: &mut Dom,
        source: &str,
        url: &Url,
    ) -> Result<(), JsError> {
        self.source_line_starts = build_line_starts(source);
        let script = match CompiledScript::compile(source, &self.limits) {
            Ok(script) => script,
            Err(error) => {
                let error = self.position_error(error);
                self.report_uncaught_error(dom, &error);
                return Err(error);
            }
        };
        self.transient_roots.clear();
        self.steps_remaining = self.limits.max_execution_steps;
        self.calls_active = 0;
        self.dom_nodes_created = 0;
        self.environment.clear();
        let previous_filename = self.script_filename.clone();
        self.script_filename = Some(url.to_string());
        let from_revision = dom.revision();
        let outcome =
            self.run_compiled_script(dom, &script.statements, script.is_strict(), from_revision);
        if let Err(error) = &outcome {
            self.report_uncaught_error(dom, error);
        }
        self.script_filename = previous_filename;
        self.queue_mutation_deliveries(dom);
        outcome.map(|_| ())
    }

    /// Delivers one page-to-worker message as a task in the worker realm.
    pub(super) fn deliver_worker_message(
        &mut self,
        dom: &mut Dom,
        worker: WorkerId,
        data: &JsValue,
    ) -> Result<(), JsError> {
        let Some(record) = self.workers.get(&worker) else {
            return Ok(());
        };
        if record.state != WorkerState::Running {
            return Ok(());
        }
        let realm = record.realm;
        let data = self.with_realm(realm, |runtime| {
            runtime.structured_clone_value(data, &mut BTreeMap::new())
        })?;
        self.with_realm(realm, |runtime| {
            let event = runtime.create_message_event("message")?;
            runtime.realm.set_property(event, "data".to_owned(), data);
            runtime.dispatch_worker_global_event(dom, realm, event, "message")
        })
    }

    /// Delivers one worker-to-page message at the `Worker` object.
    pub(super) fn deliver_worker_page_message(
        &mut self,
        dom: &mut Dom,
        worker: WorkerId,
        data: &JsValue,
    ) -> Result<(), JsError> {
        let Some(record) = self.workers.get(&worker) else {
            return Ok(());
        };
        if record.state == WorkerState::Terminated {
            return Ok(());
        }
        let owner = record.owner;
        let object = record.object;
        let origin = record.url.origin().ascii_serialization();
        let data = self.with_realm(owner, |runtime| {
            runtime.structured_clone_value(data, &mut BTreeMap::new())
        })?;
        self.with_realm(owner, |runtime| {
            let event = runtime.create_message_event("message")?;
            runtime.realm.set_property(event, "data".to_owned(), data);
            runtime
                .realm
                .set_property(event, "origin".to_owned(), JsValue::String(origin.into()));
            runtime.dispatch_worker_object_event(dom, object, event, "message")
        })
    }

    fn create_message_event(&mut self, event_type: &str) -> Result<ObjectId, JsError> {
        if self.realm.global("MessageEvent").is_some() {
            self.create_event_object_of("MessageEvent", event_type, false, false, false, true)
        } else {
            self.create_event_object(event_type, false, false, false, true)
        }
    }

    /// Direct dispatch at a worker's global object: its `addEventListener`
    /// registrations followed by its `on<type>` handler.
    fn dispatch_worker_global_event(
        &mut self,
        dom: &mut Dom,
        realm: RealmId,
        event: ObjectId,
        event_type: &str,
    ) -> Result<(), JsError> {
        let global = self.realm.global_of(realm);
        self.realm
            .set_property(event, "target".to_owned(), JsValue::Object(global));
        self.realm
            .set_property(event, "currentTarget".to_owned(), JsValue::Object(global));
        self.event_flags.insert(event, EventFlags::default());
        let listeners = self.window_event_handlers[realm.0]
            .get(event_type)
            .cloned()
            .unwrap_or_default();
        for listener in listeners {
            if listener.once
                && let Some(listeners) = self.window_event_handlers[realm.0].get_mut(event_type)
            {
                listeners.retain(|candidate| {
                    !(candidate.callback == listener.callback
                        && candidate.capture == listener.capture)
                });
            }
            self.call_listener(dom, &listener, event, global, event_type)?;
        }
        self.invoke_worker_handler(dom, global, event, event_type)
    }

    /// Direct dispatch at the page-side `Worker` object.
    fn dispatch_worker_object_event(
        &mut self,
        dom: &mut Dom,
        target: ObjectId,
        event: ObjectId,
        event_type: &str,
    ) -> Result<(), JsError> {
        self.realm
            .set_property(event, "target".to_owned(), JsValue::Object(target));
        self.realm
            .set_property(event, "currentTarget".to_owned(), JsValue::Object(target));
        self.event_flags.insert(event, EventFlags::default());
        let listeners = self
            .worker_event_listeners
            .get(&target)
            .and_then(|listeners| listeners.get(event_type))
            .cloned()
            .unwrap_or_default();
        for listener in listeners {
            if listener.once
                && let Some(listeners) = self
                    .worker_event_listeners
                    .get_mut(&target)
                    .and_then(|listeners| listeners.get_mut(event_type))
            {
                listeners.retain(|candidate| {
                    !(candidate.callback == listener.callback
                        && candidate.capture == listener.capture)
                });
            }
            self.call_listener(dom, &listener, event, target, event_type)?;
        }
        self.invoke_worker_handler(dom, target, event, event_type)
    }

    fn invoke_worker_handler(
        &mut self,
        dom: &mut Dom,
        target: ObjectId,
        event: ObjectId,
        event_type: &str,
    ) -> Result<(), JsError> {
        let name = format!("on{event_type}");
        let Some(JsValue::Object(callback)) = self.realm.get_property(target, &name) else {
            return Ok(());
        };
        if !Self::is_callable_object(callback, &self.realm) {
            return Ok(());
        }
        let listener = Listener {
            callback,
            capture: false,
            once: false,
            passive: false,
        };
        self.call_listener(dom, &listener, event, target, event_type)
    }

    /// Stops a worker: no future messages, timers, or script tasks run. The
    /// record stays so an existing `Worker` object keeps answering.
    fn terminate_worker(&mut self, worker: WorkerId) {
        let Some(record) = self.workers.get_mut(&worker) else {
            return;
        };
        if record.state == WorkerState::Terminated {
            return;
        }
        record.state = WorkerState::Terminated;
        record.queue.clear();
        let realm = record.realm;
        let object = record.object;
        let mut cancelled = Vec::new();
        self.timers.retain(|id, entry| {
            if entry.realm == realm {
                cancelled.push(*id);
                false
            } else {
                true
            }
        });
        for id in cancelled {
            self.pending_timer_requests
                .push(crate::TimerRequest::Cancel { id });
        }
        self.internal_tasks.retain(|_, task| match task {
            InternalTask::WorkerLoad { worker: pending }
            | InternalTask::WorkerMessage {
                worker: pending, ..
            }
            | InternalTask::WorkerPageMessage {
                worker: pending, ..
            } => *pending != worker,
            _ => true,
        });
        let stale_fetches: Vec<u64> = self
            .pending_worker_scripts
            .iter()
            .filter_map(|(id, pending)| (*pending == worker).then_some(*id))
            .collect();
        for id in stale_fetches {
            self.pending_worker_scripts.remove(&id);
            self.pending_fetch_targets.remove(&id);
        }
        self.worker_event_listeners.remove(&object);
    }

    /// Reports a load failure at the `Worker` object and stops the worker.
    fn fail_worker_load(&mut self, dom: &mut Dom, worker: WorkerId, message: &str) {
        let Some(record) = self.workers.get_mut(&worker) else {
            return;
        };
        record.state = WorkerState::Terminated;
        record.queue.clear();
        let owner = record.owner;
        let object = record.object;
        let url = record.url.clone();
        let _ = self.with_realm(owner, |runtime| {
            runtime.dispatch_worker_error_message(dom, object, message, url.as_str())
        });
    }

    /// Dispatches an uncaught worker exception at the page-side `Worker` object.
    /// Returns `true` when the current realm is a worker and the error was
    /// handled here, so the page's window `error` dispatch must not run.
    pub(super) fn report_worker_uncaught_error(&mut self, dom: &mut Dom, error: &JsError) -> bool {
        let Some(worker) = self.current_worker() else {
            return false;
        };
        let Some(record) = self.workers.get(&worker) else {
            return true;
        };
        let owner = record.owner;
        let object = record.object;
        let url = record.url.clone();
        let filename = self
            .script_filename
            .clone()
            .unwrap_or_else(|| url.to_string());
        let _ = self.with_realm(owner, |runtime| {
            runtime.dispatch_worker_error_event(dom, object, error, &filename)
        });
        true
    }

    fn dispatch_worker_error_event(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        error: &JsError,
        filename: &str,
    ) -> Result<(), JsError> {
        let error_value = match error.thrown_value() {
            Some(value) => value.clone(),
            None => self
                .construct_standard_error(super::standard_error_kind(error.kind()), error.message())
                .unwrap_or(JsValue::Undefined),
        };
        let message = match &error_value {
            JsValue::Object(instance) => self
                .realm
                .get_property(*instance, "message")
                .map(|value| value.to_js_string())
                .filter(|message| !message.is_empty())
                .unwrap_or_else(|| (error.message().to_owned()).into()),
            _ => crate::utf16::JsString::from(error.message()),
        };
        let event = self.create_worker_error_event(filename, message, error, error_value)?;
        self.dispatch_worker_object_event(dom, object, event, "error")
    }

    fn dispatch_worker_error_message(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        message: &str,
        filename: &str,
    ) -> Result<(), JsError> {
        let error_value = self
            .construct_standard_error(crate::value::ErrorKind::Error, message)
            .unwrap_or(JsValue::Null);
        let event = self.create_worker_error_event(
            filename,
            crate::utf16::JsString::from(message),
            &JsError::dom(message.to_owned()),
            error_value,
        )?;
        self.dispatch_worker_object_event(dom, object, event, "error")
    }

    fn create_worker_error_event(
        &mut self,
        filename: &str,
        message: crate::utf16::JsString,
        error: &JsError,
        error_value: JsValue,
    ) -> Result<ObjectId, JsError> {
        let event = if self.realm.global("ErrorEvent").is_some() {
            self.create_event_object_of("ErrorEvent", "error", false, true, false, true)?
        } else {
            self.create_event_object("error", false, true, false, true)?
        };
        for (name, value) in [
            ("message", JsValue::String(message)),
            ("filename", JsValue::String((filename.to_owned()).into())),
            (
                "lineno",
                JsValue::Number(error.position().map_or(0.0, |(line, _)| line as f64)),
            ),
            (
                "colno",
                JsValue::Number(error.position().map_or(0.0, |(_, column)| column as f64)),
            ),
            ("error", error_value),
        ] {
            self.realm.set_property(event, name.to_owned(), value);
        }
        Ok(event)
    }

    /// Turns the second `postMessage` argument into a transfer map. A
    /// duplicate, detached, immutable, shared, or non-ArrayBuffer entry is a
    /// `DataCloneError`, as the structured-clone transfer steps require.
    fn worker_transfers(
        &mut self,
        dom: &mut Dom,
        transfer: Option<&JsValue>,
    ) -> Result<Transfers, JsError> {
        let mut transfers = Transfers {
            moved: BTreeMap::new(),
            sources: Vec::new(),
        };
        let Some(transfer) = transfer else {
            return Ok(transfers);
        };
        if matches!(transfer, JsValue::Undefined | JsValue::Null) {
            return Ok(transfers);
        }
        let JsValue::Object(list) = transfer else {
            return Err(self.worker_data_clone_error(transfer));
        };
        let length = self.array_like_length(dom, *list)?;
        let mut seen = std::collections::BTreeSet::new();
        for index in 0..length {
            let item = self.get_member(dom, *list, &index.to_string())?;
            let JsValue::Object(object) = item else {
                return Err(self.worker_data_clone_error(&item));
            };
            let Some(ObjectHost::ArrayBufferHost(buffer)) = self.realm.host(object) else {
                return Err(self.worker_data_clone_error(&JsValue::Object(object)));
            };
            if buffer.is_detached() || buffer.is_immutable() || buffer.is_shared() {
                return Err(self.dom_exception(
                    DomExceptionName::DataClone,
                    "ArrayBuffer is not transferable",
                ));
            }
            if !seen.insert(object) {
                return Err(self.dom_exception(
                    DomExceptionName::DataClone,
                    "ArrayBuffer appears twice in the transfer list",
                ));
            }
            let bytes = buffer.bytes();
            transfers.moved.insert(object, TypedBuffer::new(bytes));
            transfers.sources.push(buffer.clone());
        }
        Ok(transfers)
    }

    fn worker_data_clone_error(&mut self, value: &JsValue) -> JsError {
        let description = match value {
            JsValue::Symbol(symbol) => symbol.to_display().to_lossy(),
            JsValue::Object(_) => "[object Object]".to_owned(),
            other => other.to_js_string().to_lossy(),
        };
        self.dom_exception(
            DomExceptionName::DataClone,
            format!("{description} could not be cloned."),
        )
    }
}

/// Decode a `data:` URL into script text. Both `;base64` and percent-encoded
/// forms are supported because `URL.createObjectURL(blob)` produces the former
/// and hand-written worker fixtures often use the latter.
fn decode_data_url(url: &Url) -> Result<String, String> {
    let text = url.as_str();
    let rest = text
        .strip_prefix("data:")
        .ok_or_else(|| "not a data URL".to_owned())?;
    let (metadata, payload) = rest
        .split_once(',')
        .ok_or_else(|| "data URL has no comma".to_owned())?;
    let bytes = if metadata
        .rsplit(';')
        .next()
        .is_some_and(|last| last.eq_ignore_ascii_case("base64"))
    {
        decode_base64(payload)?
    } else {
        decode_percent(payload)
    };
    String::from_utf8(bytes).map_err(|_| "worker script is not UTF-8".to_owned())
}

fn decode_base64(text: &str) -> Result<Vec<u8>, String> {
    fn value(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let compact: Vec<u8> = text
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    if compact.is_empty() {
        return Ok(Vec::new());
    }
    if !compact.len().is_multiple_of(4) {
        return Err("base64 payload length is not a multiple of four".to_owned());
    }
    let mut decoded = Vec::with_capacity(compact.len() / 4 * 3);
    for chunk in compact.chunks_exact(4) {
        let first = value(chunk[0]).ok_or_else(|| "invalid base64".to_owned())?;
        let second = value(chunk[1]).ok_or_else(|| "invalid base64".to_owned())?;
        decoded.push((first << 2) | (second >> 4));
        if chunk[2] != b'=' {
            let third = value(chunk[2]).ok_or_else(|| "invalid base64".to_owned())?;
            decoded.push((second << 4) | (third >> 2));
            if chunk[3] != b'=' {
                let fourth = value(chunk[3]).ok_or_else(|| "invalid base64".to_owned())?;
                decoded.push((third << 6) | fourth);
            }
        }
    }
    Ok(decoded)
}

fn decode_percent(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2]))
        {
            decoded.push(high << 4 | low);
            index += 3;
            continue;
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    decoded
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
