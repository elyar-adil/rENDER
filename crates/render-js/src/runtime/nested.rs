//! Nested browsing contexts: an `<iframe>` displays its own document, in its
//! own realm, with its own classic scripts, and the windows of a page can post
//! messages to each other (HTML 4.8.5, 9.4.3).
//!
//! One heap holds every realm of a page, and one DOM arena holds every
//! document, so a node id names one node for the whole page. A frame's document
//! is parsed into the arena, bound to a new realm, and its scripts run in that
//! realm. Frame work is scheduled as internal tasks on the timer requests that
//! the embedding already drains, so no new entry point is needed.
//!
//! When an iframe is connected it gets an `about:blank` realm at once, so
//! `contentWindow` is usable straight after insertion. Its document is loaded
//! in a later task, and its `load` event is a task after that.
//!
//! Not implemented: painting a nested document (an iframe still renders as
//! nothing), `window.open`, the cross-origin restrictions on frames (every
//! frame may script every other frame), navigating an existing frame to a new
//! URL, and the `onmessage` attribute handler.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;

use render_dom::{Dom, Namespace, NodeId, NodeKind};
use url::Url;

use super::builtins::dom_exception::DomExceptionName;
use super::{INTRINSICS_PRELUDE_SOURCE, JsRuntime, PRELUDE_SOURCE};
use crate::{JsError, JsValue, ObjectId, RealmId, TimerRequest};

/// The host's source of the documents and scripts that nested browsing
/// contexts load by URL.
pub trait DocumentLoader: Debug {
    /// The text of the resource at `url`, or `None` when it cannot be loaded.
    fn load_text(&mut self, url: &Url) -> Option<String>;
}

/// A browsing context the runtime hosts: the realm and the document it displays.
#[derive(Clone, Debug)]
pub(super) struct BrowsingContextRecord {
    /// The displayed document. `None` for a realm made by `CreateRealm`, which
    /// displays no document.
    pub(super) document: Option<NodeId>,
    /// The base URL that relative URLs in the document resolve against. It is
    /// the document's own URL, except for an `about:blank` or `about:srcdoc`
    /// document, which takes the base URL of the document that created its
    /// iframe (HTML 7.4.2.3). `location` reports the document's own URL, which
    /// the realm's host globals hold.
    pub(super) base: Url,
    /// The serialised origin of the document. An `about:blank` or `about:srcdoc`
    /// document takes its creator's origin, so it is not opaque (HTML 7.4.2.3).
    pub(super) origin: String,
    /// The iframe element that embeds the context, `None` for the page.
    pub(super) frame: Option<NodeId>,
}

impl BrowsingContextRecord {
    /// A context whose base URL and origin are its own URL's.
    pub(super) fn own(document: Option<NodeId>, url: Url, frame: Option<NodeId>) -> Self {
        let origin = url.origin().ascii_serialization();
        Self {
            document,
            base: url,
            origin,
            frame,
        }
    }
}

/// Work the runtime scheduled for itself. See the module comment.
#[derive(Clone, Debug)]
pub(super) enum InternalTask {
    /// Load the document of an iframe that was given an `about:blank` realm.
    LoadFrame { frame: NodeId },
    /// Fire `load` at an iframe once its document has loaded.
    FrameLoad { frame: NodeId },
    /// Deliver a `message` event to the window of `target`. The data is already
    /// a structured clone of what the sender posted.
    PostMessage {
        target: RealmId,
        source: RealmId,
        data: JsValue,
        origin: String,
    },
}

/// Where a connected iframe stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FrameState {
    /// Its document load is scheduled and it has no realm yet.
    Pending,
    /// The realm it displays.
    Realm(RealmId),
    /// No realm could be made for it, so it has no window and never loads.
    Refused,
}

const ABOUT_BLANK: &str = "about:blank";
const ABOUT_SRCDOC: &str = "about:srcdoc";

/// Objects one browsing context's built-ins take, measured at 1,843 on the
/// current intrinsics. Checked before a realm is built, with some headroom.
const REALM_HEAP_OBJECTS: usize = 2_048;

impl JsRuntime {
    /// Installs the host's loader for the documents and scripts of nested
    /// browsing contexts. Without one, a frame's `src` cannot load.
    pub fn set_document_loader(&mut self, loader: Box<dyn DocumentLoader>) {
        self.document_loader = Some(loader);
    }

    /// The realm of the browsing context that displays `document`, if any.
    pub(super) fn realm_of_document(&self, document: NodeId) -> Option<RealmId> {
        self.contexts
            .iter()
            .position(|context| context.document == Some(document))
            .map(RealmId)
    }

    /// The realm that owns `node`: the context whose document is the root of
    /// the tree `node` is in. A node in no context's tree, such as one that was
    /// created and not yet inserted, belongs to the running realm.
    pub(super) fn realm_of_node(&self, dom: &Dom, node: NodeId) -> RealmId {
        self.realm_of_document(root_of(dom, node))
            .unwrap_or_else(|| self.realm.current_realm())
    }

    /// The realm whose global object is `global`, if it is a window.
    pub(super) fn realm_of_global(&self, global: ObjectId) -> Option<RealmId> {
        (0..self.contexts.len())
            .map(RealmId)
            .find(|realm| self.realm.global_of(*realm) == global)
    }

    /// The `contentWindow` (or, with `document`, the `contentDocument`) of the
    /// iframe `frame`, or `null` when the frame has no browsing context.
    pub(super) fn frame_content(
        &mut self,
        dom: &mut Dom,
        frame: NodeId,
        document: bool,
    ) -> JsValue {
        self.sync_nested_contexts(dom);
        if self.frames.get(&frame) == Some(&FrameState::Pending) {
            // A frame that has not loaded yet shows its about:blank window, which is
            // made now, on first use, so a frame nobody inspects costs one realm.
            self.create_blank_context(dom, frame);
        }
        match self.frames.get(&frame) {
            Some(FrameState::Realm(realm)) => JsValue::Object(if document {
                self.realm.document_of(*realm)
            } else {
                self.realm.global_of(*realm)
            }),
            _ => JsValue::Null,
        }
    }

    /// Brings every connected iframe up to date. A connected frame that is not
    /// yet known is scheduled to load; a frame that is no longer connected loses
    /// its state. Only a tree that changed since the last scan is searched.
    pub(super) fn sync_nested_contexts(&mut self, dom: &mut Dom) {
        if self.nested_scan_revision == Some(dom.revision()) {
            return;
        }
        self.nested_scan_revision = Some(dom.revision());
        let documents: Vec<NodeId> = self
            .contexts
            .iter()
            .filter_map(|context| context.document)
            .collect();
        let mut frames = Vec::new();
        for document in documents {
            collect_html_elements(dom, document, "iframe", &mut frames);
        }
        let connected: BTreeSet<NodeId> = frames.iter().copied().collect();
        self.frames.retain(|frame, _| connected.contains(frame));
        for frame in frames {
            if let std::collections::btree_map::Entry::Vacant(slot) = self.frames.entry(frame) {
                slot.insert(FrameState::Pending);
                self.schedule(InternalTask::LoadFrame { frame });
            }
        }
    }

    /// Gives a pending iframe its `about:blank` realm. A frame that cannot get
    /// one, because the heap has no room, is refused and has no window.
    fn create_blank_context(&mut self, dom: &mut Dom, frame: NodeId) {
        let document = dom.graft_document(Dom::new());
        match self.create_browsing_context(dom, document, &blank_url(), Some(frame)) {
            Ok(realm) => {
                self.link_context(dom, realm);
                self.frames.insert(frame, FrameState::Realm(realm));
            }
            Err(_) => {
                self.frames.insert(frame, FrameState::Refused);
            }
        }
    }

    /// Creates the realm of a browsing context that displays `document`, with
    /// the built-ins that every realm gets, and records the context. Fails
    /// without allocating when the heap cannot hold a realm, so a page that has
    /// used its heap keeps running.
    fn create_browsing_context(
        &mut self,
        dom: &mut Dom,
        document: NodeId,
        url: &Url,
        frame: Option<NodeId>,
    ) -> Result<RealmId, JsError> {
        self.ensure_heap_capacity(REALM_HEAP_OBJECTS)?;
        let context = self.new_context(dom, Some(document), url, frame);
        let realm = self.realm.create_document_realm(document, url);
        self.push_realm_state(context);
        self.with_realm(realm, |runtime| {
            runtime.run_self_hosted(dom, INTRINSICS_PRELUDE_SOURCE)?;
            runtime.run_self_hosted(dom, PRELUDE_SOURCE)
        })?;
        Ok(realm)
    }

    /// The record of a context that displays `url`. An `about:blank` or
    /// `about:srcdoc` document in an iframe takes the base URL and origin of the
    /// iframe's own document, so a window it posts to sees a same-origin sender.
    fn new_context(
        &self,
        dom: &Dom,
        document: Option<NodeId>,
        url: &Url,
        frame: Option<NodeId>,
    ) -> BrowsingContextRecord {
        let creator = frame.map(|frame| self.realm_of_node(dom, frame));
        match creator {
            Some(creator) if inherits_creator_origin(url) => BrowsingContextRecord {
                document,
                base: self.contexts[creator.0].base.clone(),
                origin: self.contexts[creator.0].origin.clone(),
                frame,
            },
            _ => BrowsingContextRecord::own(document, url.clone(), frame),
        }
    }

    /// Points the `parent`, `top` and `frameElement` of a context at its
    /// embedding.
    fn link_context(&mut self, dom: &Dom, realm: RealmId) {
        let global = self.realm.global_of(realm);
        let Some(frame) = self.contexts[realm.0].frame else {
            self.realm
                .set_frame_relations(realm, global, global, JsValue::Null);
            return;
        };
        let owner = self.realm_of_node(dom, frame);
        let owner_global = self.realm.global_of(owner);
        let top = self.top_realm_of(dom, owner);
        let top_global = self.realm.global_of(top);
        let frame_element = self
            .with_realm(owner, |runtime| runtime.wrap_node(dom, frame))
            .unwrap_or(JsValue::Null);
        self.realm
            .set_frame_relations(realm, owner_global, top_global, frame_element);
    }

    /// The top-level context above `realm`.
    fn top_realm_of(&self, dom: &Dom, realm: RealmId) -> RealmId {
        let mut current = realm;
        while let Some(frame) = self.contexts[current.0].frame {
            current = self.realm_of_node(dom, frame);
        }
        current
    }

    /// Schedules `task` as an event-loop task. The embedding runs it when it
    /// fires the timer, which is the same path a `setTimeout(0)` takes.
    fn schedule(&mut self, task: InternalTask) {
        let id = self.next_timer_id;
        self.next_timer_id += 1;
        self.internal_tasks.insert(id, task);
        self.pending_timer_requests
            .push(TimerRequest::Schedule { id, delay_ms: 0.0 });
    }

    /// Runs one scheduled task. The caller has reset the execution budget.
    pub(super) fn run_internal_task(
        &mut self,
        dom: &mut Dom,
        task: InternalTask,
    ) -> Result<(), JsError> {
        let outcome = match task {
            InternalTask::LoadFrame { frame } => self.load_frame(dom, frame),
            InternalTask::FrameLoad { frame } => self.fire_frame_load(dom, frame),
            InternalTask::PostMessage {
                target,
                source,
                data,
                origin,
            } => self.deliver_message(dom, target, source, data, origin),
        };
        self.queue_mutation_deliveries(dom);
        self.sync_nested_contexts(dom);
        outcome
    }

    /// Loads the document of an iframe: parses it, gives it its own realm, runs
    /// its scripts, and schedules the iframe's `load` event.
    fn load_frame(&mut self, dom: &mut Dom, frame: NodeId) -> Result<(), JsError> {
        // A frame removed since it was scheduled, or refused, loads nothing.
        match self.frames.get(&frame) {
            None | Some(FrameState::Refused) => return Ok(()),
            Some(FrameState::Pending | FrameState::Realm(_)) => {}
        }
        if self.realm_of_document(root_of(dom, frame)).is_none() {
            self.frames.remove(&frame);
            return Ok(());
        }
        let owner = self.realm_of_node(dom, frame);
        let base = self.contexts[owner.0].base.clone();
        let (url, source) = self.frame_document_source(dom, frame, &base);
        // An iframe with neither `src` nor `srcdoc` keeps the initial `about:blank`
        // window it already has. That document is not replaced, so what script
        // set up on it before this task ran is still there.
        if url.as_str() == ABOUT_BLANK
            && let Some(FrameState::Realm(_)) = self.frames.get(&frame)
        {
            self.schedule(InternalTask::FrameLoad { frame });
            return Ok(());
        }
        let document = dom.graft_document(render_html::parse_document(&source).dom);
        let Ok(realm) = self.create_browsing_context(dom, document, &url, Some(frame)) else {
            // The frame keeps the window it already has, if it has one, and its
            // `load` still fires, as a failed navigation's does.
            if self.frames.get(&frame) == Some(&FrameState::Pending) {
                self.frames.insert(frame, FrameState::Refused);
            }
            self.schedule(InternalTask::FrameLoad { frame });
            return Ok(());
        };
        self.link_context(dom, realm);
        self.frames.insert(frame, FrameState::Realm(realm));
        // The new document may hold iframes of its own, which the next scan must see
        // even though grafting does not record a mutation.
        self.nested_scan_revision = None;
        let document_base = self.contexts[realm.0].base.clone();
        self.with_realm(realm, |runtime| {
            runtime.run_document_scripts(dom, document, &document_base);
        });
        // The frame's `load` follows its window's, even when a `load` listener
        // reports a failure, so the embedding still hears about the frame.
        let window_load = self.dispatch_window_load(dom, realm);
        self.schedule(InternalTask::FrameLoad { frame });
        window_load
    }

    /// The URL and source text of the document an iframe displays. `srcdoc`
    /// wins over `src`. A frame with neither, or whose `src` cannot be loaded,
    /// displays an empty document.
    fn frame_document_source(&mut self, dom: &Dom, frame: NodeId, base: &Url) -> (Url, String) {
        if let Ok(Some(srcdoc)) = dom.attribute(frame, "srcdoc") {
            return (
                Url::parse(ABOUT_SRCDOC).expect("constant URL"),
                srcdoc.to_owned(),
            );
        }
        if let Ok(Some(src)) = dom.attribute(frame, "src")
            && let Ok(url) = base.join(src.trim())
        {
            let source = self
                .document_loader
                .as_mut()
                .and_then(|loader| loader.load_text(&url))
                .unwrap_or_default();
            return (url, source);
        }
        (blank_url(), String::new())
    }

    /// Runs the classic scripts of a document, in document order, in the
    /// running realm. A script that fails reports to the window's error
    /// handlers, as it does on the page, and the next one still runs.
    fn run_document_scripts(&mut self, dom: &mut Dom, document: NodeId, base: &Url) {
        let mut scripts = Vec::new();
        collect_html_elements(dom, document, "script", &mut scripts);
        for script in scripts {
            if !is_classic_script(dom, script) {
                continue;
            }
            let source = match dom.attribute(script, "src") {
                Ok(Some(src)) if src.trim().is_empty() => None,
                Ok(Some(src)) => base
                    .join(src.trim())
                    .ok()
                    .and_then(|url| self.document_loader.as_mut()?.load_text(&url)),
                _ => Some(text_content(dom, script)),
            };
            if let Some(source) = source {
                let _ = self.execute(dom, &source);
            }
        }
    }

    /// Fires `load` at an iframe, in the realm of the document that embeds it.
    fn fire_frame_load(&mut self, dom: &mut Dom, frame: NodeId) -> Result<(), JsError> {
        if !self.frames.contains_key(&frame) {
            return Ok(());
        }
        let owner = self.realm_of_node(dom, frame);
        self.with_realm(owner, |runtime| {
            let event = runtime.create_event_object("load", false, false, false, true)?;
            runtime.dispatch_prepared_event(dom, frame, event, "load", false)
        })?;
        Ok(())
    }

    /// Fires `load` at the window of `realm`, after its document's scripts ran.
    /// The event is aimed at the document, which is how every window event here
    /// reaches the window.
    fn dispatch_window_load(&mut self, dom: &mut Dom, realm: RealmId) -> Result<(), JsError> {
        let Some(document) = self.contexts[realm.0].document else {
            return Ok(());
        };
        self.with_realm(realm, |runtime| {
            let event = runtime.create_event_object("load", false, false, false, true)?;
            runtime.dispatch_prepared_event(dom, document, event, "load", false)
        })?;
        Ok(())
    }

    /// Delivers a `message` event to the window of `target`.
    fn deliver_message(
        &mut self,
        dom: &mut Dom,
        target: RealmId,
        source: RealmId,
        data: JsValue,
        origin: String,
    ) -> Result<(), JsError> {
        let Some(document) = self.contexts[target.0].document else {
            return Ok(());
        };
        let source_window = self.realm.global_of(source);
        self.with_realm(target, |runtime| {
            let event = runtime.create_event_object("message", false, false, false, true)?;
            runtime.realm.set_property(event, "data".to_owned(), data);
            runtime
                .realm
                .set_property(event, "origin".to_owned(), JsValue::String(origin.into()));
            runtime
                .realm
                .set_property(event, "source".to_owned(), JsValue::Object(source_window));
            runtime.dispatch_prepared_event(dom, document, event, "message", false)
        })?;
        Ok(())
    }

    /// `Window.prototype.postMessage(message, targetOrigin)` (HTML 9.4.3), with
    /// `receiver` the window it is called on. The message is cloned now, as the
    /// standard requires, and delivered as a task.
    pub(super) fn window_post_message(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let target = self
            .realm_of_global(receiver)
            .ok_or_else(|| JsError::type_error("postMessage must be called on a window"))?;
        let message = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let target_origin = match arguments.get(1) {
            Some(JsValue::Object(options)) => {
                match self.realm.get_property(*options, "targetOrigin") {
                    Some(JsValue::Undefined) | None => "/".to_owned(),
                    Some(value) => value.to_js_string().to_lossy(),
                }
            }
            Some(value) => value.to_js_string().to_lossy(),
            None => "/".to_owned(),
        };
        // The caller's realm, not the callee's: `postMessage` is installed in the
        // realm of the window it posts to, and that realm is current while it runs.
        let source = self
            .caller_realms
            .last()
            .copied()
            .unwrap_or_else(|| self.realm.current_realm());
        let source_origin = self.origin_of(source);
        let delivered = match target_origin.as_str() {
            "*" => true,
            "/" => source_origin == self.origin_of(target),
            other => {
                // Anything else must be a URL, or the call throws (HTML 9.4.3 step 4).
                let url = Url::parse(other).map_err(|_| {
                    self.dom_exception(
                        DomExceptionName::Syntax,
                        format!("postMessage: {other:?} is not a valid targetOrigin"),
                    )
                })?;
                url.origin().ascii_serialization() == self.origin_of(target)
            }
        };
        if !delivered {
            return Ok(JsValue::Undefined);
        }
        let data = self.structured_clone_value(&message, &mut BTreeMap::new())?;
        self.schedule(InternalTask::PostMessage {
            target,
            source,
            data,
            origin: source_origin,
        });
        Ok(JsValue::Undefined)
    }

    /// The serialised origin of the document of `realm`.
    fn origin_of(&self, realm: RealmId) -> String {
        self.contexts[realm.0].origin.clone()
    }
}

fn blank_url() -> Url {
    Url::parse(ABOUT_BLANK).expect("constant URL")
}

/// Whether a document at `url` takes its origin and base URL from the document
/// that created it, rather than from `url`.
fn inherits_creator_origin(url: &Url) -> bool {
    matches!(url.as_str(), ABOUT_BLANK | ABOUT_SRCDOC)
}

/// The root of the tree that `node` is in.
fn root_of(dom: &Dom, node: NodeId) -> NodeId {
    let mut root = node;
    while let Some(parent) = dom.parent(root) {
        root = parent;
    }
    root
}

/// Whether `node` is an HTML element with `local_name`.
pub(super) fn is_html_element(dom: &Dom, node: NodeId, local_name: &str) -> bool {
    matches!(
        dom.node(node).map(render_dom::Node::kind),
        Some(NodeKind::Element(element))
            if element.namespace == Namespace::Html && element.local_name == local_name
    )
}

/// Appends every HTML element named `local_name` under `root`, in document order.
fn collect_html_elements(dom: &Dom, root: NodeId, local_name: &str, out: &mut Vec<NodeId>) {
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        if is_html_element(dom, node, local_name) {
            out.push(node);
        }
        if let Some(children) = dom.children(node) {
            pending.extend(children.iter().rev().copied());
        }
    }
}

/// A script element runs when it has no `type`, or a JavaScript one. Module
/// scripts are not run here.
fn is_classic_script(dom: &Dom, script: NodeId) -> bool {
    match dom.attribute(script, "type") {
        Ok(None) => true,
        Ok(Some(kind)) => matches!(
            kind.trim().to_ascii_lowercase().as_str(),
            "" | "text/javascript" | "application/javascript"
        ),
        Err(_) => false,
    }
}

fn text_content(dom: &Dom, node: NodeId) -> String {
    let mut text = String::new();
    for child in dom.children(node).unwrap_or_default() {
        if let Some(NodeKind::Text(data)) = dom.node(*child).map(render_dom::Node::kind) {
            text.push_str(data);
        }
    }
    text
}
