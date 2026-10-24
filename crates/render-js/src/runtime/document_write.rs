//! `document.write()`, `document.writeln()`, `document.open()` and
//! `document.close()` (HTML §8.4), plus `document.currentScript`.
//!
//! # The two shapes a write can take
//!
//! A write from a parser-created script that is still executing inserts at the
//! parser's insertion point — in this engine, right after that script element
//! (HTML §8.4.3: "the insertion point is just after the script element"). The
//! written text is parsed in the context of the script's parent, so a write
//! that spans several calls (`"<i"`, then `">Filler Text"`) lands as one
//! element, exactly as a tokenizer fed incrementally would produce.
//!
//! A write from anywhere else — an event handler, a timer, a queued test — has
//! no insertion point, so the document open steps run first: the document's
//! children are removed and the written text becomes the document's new
//! content, parsed as a whole document (HTML §8.4.2, the implicit `open()`).
//! Every `document.write` test in WPT's `document-write/` directory that
//! asserts `document.body.firstChild` is this shape, because testharness runs
//! test functions from its own queue, after the page loaded.
//!
//! # How the incremental stream is modelled
//!
//! This engine has no live parser: a page is parsed once, and scripts run
//! against the finished tree. A write session therefore keeps the text
//! written so far and re-parses it after every write, replacing the nodes the
//! previous parse had contributed. The observable DOM after each write is the
//! one an incremental parser would hold; the cost is a re-parse per write.
//!
//! A script element inside written content executes when its end tag has been
//! seen. "Seen" is not directly observable on a tree that ends wherever the
//! stream ends, so each parse is paired with a parse of the stream plus one
//! trailing character: a complete script's source is unchanged by that
//! character, while a script truncated by the end of the stream absorbs it.
//! Only scripts whose source survives the sentinel parse execute. At
//! `document.close()` — the end of the stream — the same rule holds, which is
//! also what a real parser does: an unterminated script never executes.
//!
//! # What is deliberately not here yet
//!
//! * Written `async`/`defer` scripts and written module scripts are not run:
//!   their scheduling belongs to the embedding resource pipeline. They are
//!   discovered and then skipped, which is a stated gap, not a silent one.
//! * `document.open()` removes event listeners on the wiped nodes, but the
//!   standard's session history (`ignore-destructive-writes` counters,
//!   unload-event bailouts) is not modelled.
//! * A write while the document loads but from outside any parser-created
//!   script (a timer during load) appends at the end of the body. A real
//!   parser feeds its stream at its own position, which this engine cannot
//!   observe; the end of the body is where such text lands in practice.

use render_dom::{Dom, Namespace, Node, NodeId, NodeKind};
use render_html::FragmentContext;
use url::Url;

use crate::DocumentReadyState;
use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::runtime::builtins::dom::find_body_node;
use crate::runtime::builtins::dom_exception::DomExceptionName;

/// Where the nodes a session's re-parse produces go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::runtime) enum WriteStream {
    /// Right after the parser-created script that is executing; the `NodeId`
    /// is that script element.
    AfterScript(NodeId),
    /// Appended to the end of the body while the document loads.
    BodyTail,
    /// The stream *is* the document (an implicit or explicit `open()` ran).
    WholeDocument,
}

/// One classic script found in a session's last parse, waiting for its end
/// tag to be seen.
#[derive(Clone, Debug, PartialEq)]
struct PendingWriteScript {
    owner: NodeId,
    /// The script's inline text, which for an external script is empty in a
    /// complete element and carries stray text while the end tag is missing.
    text: String,
    /// The `src` attribute of an external script.
    external: Option<String>,
    /// `type="module"`: discovered, not run (see the module documentation).
    module: bool,
}

/// The state of one `document.write` session.
#[derive(Debug)]
pub(in crate::runtime) struct WriteSession {
    stream: WriteStream,
    /// Every written string, concatenated in call order.
    text: String,
    /// The live nodes the previous parse of `text` contributed, which the
    /// next parse replaces.
    inserted: Vec<NodeId>,
    /// The scripts of the last parse, in tree order.
    scripts: Vec<PendingWriteScript>,
    /// How many of `scripts` have run. A session's stream only grows, and a
    /// deterministic parser keeps the discovered prefix stable, so an index
    /// counts as a cursor.
    executed: usize,
}

/// A placeholder that keeps `write_sessions` occupied while a session is
/// taken out to be worked on. Never observable: the index is only held across
/// statements that cannot re-enter.
impl Default for WriteSession {
    fn default() -> Self {
        Self {
            stream: WriteStream::BodyTail,
            text: String::new(),
            inserted: Vec::new(),
            scripts: Vec::new(),
            executed: 0,
        }
    }
}

impl JsRuntime {
    /// `document.write(...text)` (HTML §8.4.3).
    pub(in crate::runtime) fn document_write(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
        newline: bool,
    ) -> Result<JsValue, JsError> {
        self.write_or_writeln(dom, receiver, arguments, newline, "write")
    }

    /// `document.writeln(...text)`: `write` plus a trailing newline.
    pub(in crate::runtime) fn document_writeln(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        self.write_or_writeln(dom, receiver, arguments, true, "writeln")
    }

    fn write_or_writeln(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
        newline: bool,
        name: &str,
    ) -> Result<JsValue, JsError> {
        // An XML document cannot take a write (HTML §8.4.3; WPT's
        // `document.write-01.xhtml` expects `InvalidStateError`), and a
        // document without a browsing context — one `DOMImplementation` made
        // — has no parser to feed, so its write is silently dropped.
        let document = self.require_document(receiver)?;
        if !self.document_has_browsing_context(document) {
            return Ok(JsValue::Undefined);
        }
        if !dom.is_html_document(document) {
            return Err(self.dom_exception(
                DomExceptionName::InvalidState,
                format!(
                    "Failed to execute '{name}' on 'Document': \
                     Only HTML documents support {name}()"
                ),
            ));
        }
        // A module script's write is silently ignored, not an error: WPT's
        // module tests assert the iframe stays intact and fires no error
        // event.
        if self.active_script_is_module() {
            return Ok(JsValue::Undefined);
        }
        let mut text = String::new();
        for argument in arguments {
            text.push_str(&argument.to_js_string().to_lossy());
        }
        if newline {
            text.push('\n');
        }
        self.write_stream(dom, &text)?;
        Ok(JsValue::Undefined)
    }

    /// `document.open(url, name, features)` (HTML §8.4.2). The three-argument
    /// form is `window.open`, and with no popup UI the answer is the same one
    /// a blocked popup gets: `null`. Every other form runs the document open
    /// steps.
    pub(in crate::runtime) fn document_open(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        if arguments.len() >= 3 {
            return Ok(JsValue::Null);
        }
        if self.active_script_is_module() {
            return Ok(JsValue::Undefined);
        }
        let document = self.require_document(receiver)?;
        if !self.document_has_browsing_context(document) {
            return Ok(JsValue::Undefined);
        }
        if !dom.is_html_document(document) {
            return Err(self.dom_exception(
                DomExceptionName::InvalidState,
                "Failed to execute 'open' on 'Document': Only HTML documents support open()",
            ));
        }
        self.open_document_stream(dom, document)?;
        Ok(JsValue::Undefined)
    }

    /// `document.close()` (HTML §8.4.4): the end of the input stream. With no
    /// stream open it is a no-op, as the standard says.
    pub(in crate::runtime) fn document_close(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
    ) -> Result<JsValue, JsError> {
        let document = self.require_document(receiver)?;
        if !self.document_has_browsing_context(document) {
            return Ok(JsValue::Undefined);
        }
        let Some(index) = self
            .write_sessions
            .iter()
            .position(|session| session.stream == WriteStream::WholeDocument)
        else {
            return Ok(JsValue::Undefined);
        };
        let mut session = std::mem::take(&mut self.write_sessions[index]);
        self.run_pending_session_scripts(dom, &mut session)?;
        self.write_sessions.remove(index);
        self.set_document_ready_state(DocumentReadyState::Complete);
        self.dispatch_dom_event(dom, document, "readystatechange", false, false, false, &[])?;
        self.dispatch_page_load(dom)?;
        Ok(JsValue::Undefined)
    }

    /// `document.currentScript` (HTML §4.12.4.1): the script element whose
    /// classic script is currently running, or `null`.
    pub(in crate::runtime) fn document_current_script(
        &mut self,
        dom: &mut Dom,
    ) -> Result<JsValue, JsError> {
        match self.parser_script_owners.last() {
            Some(owner) => self.wrap_node(dom, *owner),
            None => Ok(JsValue::Null),
        }
    }

    /// Whether `document` is the realm's own document, the only kind that has
    /// a browsing context here; the documents `DOMImplementation` creates do
    /// not, and a write to them has no parser to feed.
    fn document_has_browsing_context(&self, document: NodeId) -> bool {
        self.contexts
            .iter()
            .any(|context| context.document == Some(document))
    }

    /// The document node of the realm the running code belongs to.
    fn active_document_node(&self) -> Result<NodeId, JsError> {
        let document_object = self.realm.document_object();
        self.require_document(document_object)
    }

    /// Whether the innermost running script is a module.
    fn active_script_is_module(&self) -> bool {
        self.active_script_kinds.last().copied().unwrap_or(false)
    }

    /// Run one parser-created script element's code, remembering it as the
    /// insertion point any `document.write` inside it uses.
    ///
    /// This is the embedding entry point for a script discovered in the
    /// document: the caller knows the element, and the element is what
    /// `document.currentScript` and the write insertion point name.
    ///
    /// # Errors
    ///
    /// The same as [`Self::execute`] and [`Self::execute_with_filename`].
    pub fn execute_parser_script(
        &mut self,
        dom: &mut Dom,
        source: &str,
        filename: Option<&str>,
        owner: NodeId,
    ) -> Result<crate::ScriptOutcome, JsError> {
        self.push_parser_script(owner);
        let outcome = match filename {
            Some(filename) => self.execute_with_filename(dom, source, filename),
            None => self.execute(dom, source),
        };
        self.pop_parser_script(owner);
        outcome
    }

    /// [`Self::execute_parser_script`] for an already compiled script.
    ///
    /// # Errors
    ///
    /// The same as [`Self::execute_compiled`].
    pub fn execute_parser_compiled_script(
        &mut self,
        dom: &mut Dom,
        script: &crate::CompiledScript,
        owner: NodeId,
    ) -> Result<crate::ScriptOutcome, JsError> {
        self.push_parser_script(owner);
        let outcome = self.execute_compiled(dom, script);
        self.pop_parser_script(owner);
        outcome
    }

    fn push_parser_script(&mut self, owner: NodeId) {
        self.parser_script_owners.push(owner);
    }

    /// End one parser script's run: its write stream closes with it, which is
    /// the parser moving its insertion point on.
    fn pop_parser_script(&mut self, owner: NodeId) {
        self.parser_script_owners.pop();
        self.write_sessions
            .retain(|session| session.stream != WriteStream::AfterScript(owner));
    }

    /// Feed one written string into the document's open stream, opening one
    /// first when there is none.
    fn write_stream(&mut self, dom: &mut Dom, text: &str) -> Result<(), JsError> {
        // Which stream the write belongs to: the innermost executing
        // parser-created script owns the insertion point; a write while the
        // document loads but outside any parser script appends to the body;
        // anything else opens the document first.
        let owner = self.parser_script_owners.last().copied();
        let whole_document_open = self
            .write_sessions
            .iter()
            .any(|session| session.stream == WriteStream::WholeDocument);
        let stream = match owner {
            Some(owner) => WriteStream::AfterScript(owner),
            // A stream `open()` started stays the destination until `close()`,
            // whatever the readiness says: the open parser owns the input.
            // `open()` leaves an empty whole-document session behind, so this
            // sees it before the readiness check can misroute the write.
            None if whole_document_open => WriteStream::WholeDocument,
            None if self.ready_state == DocumentReadyState::Loading => WriteStream::BodyTail,
            None => WriteStream::WholeDocument,
        };
        let index = match self
            .write_sessions
            .iter()
            .position(|session| session.stream == stream)
        {
            Some(index) => index,
            None if stream == WriteStream::WholeDocument => {
                let document = self.active_document_node()?;
                self.open_document_stream(dom, document)?;
                self.write_sessions.len() - 1
            }
            None => {
                self.write_sessions.push(WriteSession {
                    stream,
                    text: String::new(),
                    inserted: Vec::new(),
                    scripts: Vec::new(),
                    executed: 0,
                });
                self.write_sessions.len() - 1
            }
        };
        let mut session = std::mem::take(&mut self.write_sessions[index]);
        session.text.push_str(text);
        // An `open()` a written script calls resets every stream. The epoch
        // tells this one apart from an undisturbed return: a session that
        // outlived an `open()` is stale, and the next write starts fresh.
        let epoch = self.write_stream_epoch;
        let result = self.reparse_session(dom, &mut session);
        if self.write_stream_epoch == epoch {
            match self
                .write_sessions
                .iter()
                .position(|open| open.stream == session.stream)
            {
                Some(slot) => self.write_sessions[slot] = session,
                None => self.write_sessions.push(session),
            }
        }
        result
    }

    /// The document open steps (HTML §8.4.2): remove every child of the
    /// document, with their event listeners, and reset every open stream —
    /// each `open()` starts a new parser, so no earlier stream survives it.
    fn open_document_stream(&mut self, dom: &mut Dom, document: NodeId) -> Result<(), JsError> {
        self.write_sessions.clear();
        self.write_stream_epoch = self.write_stream_epoch.wrapping_add(1);
        let wiped = descendants_of_children(dom, document);
        for node in wiped {
            self.event_listeners.remove(&node);
            self.event_handlers.remove(&node);
        }
        let children = dom.children(document).unwrap_or_default().to_vec();
        for child in children {
            dom.remove_child(document, child)?;
        }
        self.set_document_ready_state(DocumentReadyState::Loading);
        self.dispatch_dom_event(dom, document, "readystatechange", false, false, false, &[])?;
        // The open parser exists from this moment: the next write continues
        // this stream rather than opening another one.
        self.write_sessions.push(WriteSession {
            stream: WriteStream::WholeDocument,
            text: String::new(),
            inserted: Vec::new(),
            scripts: Vec::new(),
            executed: 0,
        });
        Ok(())
    }

    /// Re-parse one session's stream and make the document match it.
    ///
    /// The session is taken out of [`JsRuntime::write_sessions`] by the
    /// caller, so scripts it runs can write recursively without touching it.
    fn reparse_session(
        &mut self,
        dom: &mut Dom,
        session: &mut WriteSession,
    ) -> Result<(), JsError> {
        let document = self.active_document_node()?;
        // Remove the nodes the previous parse contributed, then import the
        // new ones at the same place. A node that moved elsewhere in the
        // meantime (the page adopted it) stays where its new parent put it.
        let expected_parent = match session.stream {
            WriteStream::AfterScript(owner) => dom.parent(owner),
            WriteStream::BodyTail => find_body_node(dom, document),
            WriteStream::WholeDocument => Some(document),
        };
        for node in std::mem::take(&mut session.inserted) {
            if dom.parent(node) == expected_parent
                && let Some(parent) = expected_parent
            {
                dom.remove_child(parent, node)?;
            }
        }
        session.inserted.clear();
        match session.stream {
            WriteStream::AfterScript(owner) => {
                let Some(parent) = dom.parent(owner) else {
                    // The executing script element was removed (an `open()`
                    // during load does this). The stream lands in the body
                    // when there is one, and is dropped otherwise.
                    let Some(body) = find_body_node(dom, document) else {
                        return Ok(());
                    };
                    let parsed = render_html::parse_html_fragment_with_scripting(
                        &session.text,
                        &FragmentContext::html("body"),
                        true,
                    );
                    return self.import_parse_roots(
                        dom,
                        session,
                        &parsed.dom,
                        parsed.dom.children(parsed.fragment).unwrap_or_default(),
                        body,
                        None,
                    );
                };
                let context = fragment_context_for(dom, owner);
                let parsed =
                    render_html::parse_html_fragment_with_scripting(&session.text, &context, true);
                self.import_parse_roots(
                    dom,
                    session,
                    &parsed.dom,
                    parsed.dom.children(parsed.fragment).unwrap_or_default(),
                    parent,
                    Some(owner),
                )?;
            }
            WriteStream::BodyTail => {
                let Some(body) = find_body_node(dom, document) else {
                    return Ok(());
                };
                let parsed = render_html::parse_html_fragment_with_scripting(
                    &session.text,
                    &FragmentContext::html("body"),
                    true,
                );
                self.import_parse_roots(
                    dom,
                    session,
                    &parsed.dom,
                    parsed.dom.children(parsed.fragment).unwrap_or_default(),
                    body,
                    None,
                )?;
            }
            WriteStream::WholeDocument => {
                let parsed = render_html::parse_document_with_scripting(&session.text, true);
                self.import_parse_roots(
                    dom,
                    session,
                    &parsed.dom,
                    parsed
                        .dom
                        .children(parsed.dom.document())
                        .unwrap_or_default(),
                    document,
                    None,
                )?;
            }
        }
        let mut scripts = Vec::new();
        for root in &session.inserted {
            collect_write_scripts(dom, *root, &mut scripts);
        }
        session.scripts = scripts;
        self.run_pending_session_scripts(dom, session)
    }

    /// Import `roots` of a parse output under `parent`, in order, right after
    /// `cursor` when given.
    fn import_parse_roots(
        &mut self,
        dom: &mut Dom,
        session: &mut WriteSession,
        source: &Dom,
        roots: &[NodeId],
        parent: NodeId,
        cursor: Option<NodeId>,
    ) -> Result<(), JsError> {
        let document = self.active_document_node()?;
        let mut cursor = cursor;
        for root in roots {
            let imported = self.import_write_node(dom, source, *root, document, parent, cursor)?;
            cursor = imported;
            if let Some(imported) = imported {
                session.inserted.push(imported);
            }
        }
        Ok(())
    }

    /// Import one node from a parsed document under `parent`, right after
    /// `cursor` when given, and return the imported node.
    ///
    /// Unlike `import_dom_subtree`, this preserves the element's namespace
    /// (a written `<svg>` must not become an HTML element), imports
    /// document-level constructs a fragment import drops (a doctype, a
    /// processing instruction), and sets each imported node's node document.
    fn import_write_node(
        &mut self,
        dom: &mut Dom,
        source: &Dom,
        node: NodeId,
        document: NodeId,
        parent: NodeId,
        cursor: Option<NodeId>,
    ) -> Result<Option<NodeId>, JsError> {
        if self.dom_nodes_created >= self.limits.max_dom_nodes_created {
            return Err(JsError::resource("DOM node creation limit exceeded"));
        }
        let imported = match source.node(node).map(Node::kind) {
            Some(NodeKind::Element(element)) => {
                self.ensure_heap_capacity(1)?;
                self.dom_nodes_created = self.dom_nodes_created.saturating_add(1);
                let copy = if element.namespace == Namespace::Html {
                    dom.create_element(element.local_name.clone())
                } else {
                    dom.create_element_qualified(
                        element.namespace.clone(),
                        element.prefix.clone(),
                        element.local_name.clone(),
                    )
                };
                for attribute in &element.attributes {
                    if let Some(namespace) = &attribute.namespace {
                        dom.set_attribute_ns(
                            copy,
                            Some(namespace.as_str()),
                            attribute.prefix.as_deref(),
                            attribute.local_name.clone(),
                            attribute.value.clone(),
                        )?;
                    } else {
                        dom.set_attribute(
                            copy,
                            attribute.local_name.clone(),
                            attribute.value.clone(),
                        )?;
                    }
                }
                copy
            }
            Some(NodeKind::Text(data)) => {
                self.ensure_heap_capacity(1)?;
                self.dom_nodes_created = self.dom_nodes_created.saturating_add(1);
                dom.create_text(data)
            }
            Some(NodeKind::Comment(data)) => {
                self.ensure_heap_capacity(1)?;
                self.dom_nodes_created = self.dom_nodes_created.saturating_add(1);
                dom.create_comment(data)
            }
            Some(NodeKind::DocumentType(doctype)) => {
                self.ensure_heap_capacity(1)?;
                self.dom_nodes_created = self.dom_nodes_created.saturating_add(1);
                dom.create_document_type(
                    doctype.name.clone(),
                    doctype.public_id.clone(),
                    doctype.system_id.clone(),
                )
            }
            Some(NodeKind::ProcessingInstruction { target, data }) => {
                self.ensure_heap_capacity(1)?;
                self.dom_nodes_created = self.dom_nodes_created.saturating_add(1);
                dom.create_processing_instruction(target.clone(), data.clone())
            }
            _ => return Ok(None),
        };
        dom.set_node_document(imported, document);
        let before = cursor.and_then(|cursor| dom.next_sibling(cursor));
        dom.insert_before(parent, imported, before)?;
        if let Some(children) = source.children(node) {
            let children = children.to_vec();
            // The children import into `imported`, whose sibling order is
            // unrelated to the cursor's: they append, then chain after each
            // other.
            let mut child_cursor: Option<NodeId> = None;
            for child in children {
                if let Some(imported_child) =
                    self.import_write_node(dom, source, child, document, imported, child_cursor)?
                {
                    child_cursor = Some(imported_child);
                }
            }
        }
        Ok(Some(imported))
    }

    /// Run the session's scripts whose end tag has now been seen, in order.
    fn run_pending_session_scripts(
        &mut self,
        dom: &mut Dom,
        session: &mut WriteSession,
    ) -> Result<(), JsError> {
        while session.executed < session.scripts.len() {
            if !Self::write_script_is_complete(session, session.executed) {
                break;
            }
            let pending = session.scripts[session.executed].clone();
            session.executed += 1;
            self.run_written_script(dom, &pending)?;
        }
        Ok(())
    }

    /// Whether the script at `index` survived the sentinel parse: a script
    /// whose end tag has not arrived absorbs the sentinel character into its
    /// text, so its source differs from the plain parse.
    fn write_script_is_complete(session: &WriteSession, index: usize) -> bool {
        let mut with_sentinel = session.text.clone();
        with_sentinel.push('z');
        let sentinel = render_html::parse_document_with_scripting(&with_sentinel, true);
        let mut scripts = Vec::new();
        collect_write_scripts(&sentinel.dom, sentinel.dom.document(), &mut scripts);
        scripts.get(index).is_some_and(|sentinel_script| {
            // The owner node is the sentinel parse's own arena id, never the
            // live one; only the source decides completeness.
            sentinel_script.text == session.scripts[index].text
                && sentinel_script.external == session.scripts[index].external
                && sentinel_script.module == session.scripts[index].module
        })
    }

    /// Run one written script: inline executes directly; an external one
    /// loads through the document loader when the embedding supplies one.
    fn run_written_script(
        &mut self,
        dom: &mut Dom,
        pending: &PendingWriteScript,
    ) -> Result<(), JsError> {
        if pending.module {
            return Ok(());
        }
        let Some(source) = (match &pending.external {
            None => Some(pending.text.clone()),
            Some(src) => self.load_written_external_script(src),
        }) else {
            return Ok(());
        };
        self.execute_parser_script(dom, &source, None, pending.owner)?;
        Ok(())
    }

    /// Resolve and load one written external script through the document
    /// loader. A loader that cannot serve the URL yields `None`, and the
    /// script does not run, as a failed fetch does not run either.
    fn load_written_external_script(&mut self, src: &str) -> Option<String> {
        let base = Url::parse(&self.realm.document_base_url()).ok()?;
        let url = base.join(src).ok()?;
        self.document_loader.as_mut()?.load_text(&url)
    }
}

/// The script elements under `root`, in tree order.
fn collect_write_scripts(dom: &Dom, root: NodeId, scripts: &mut Vec<PendingWriteScript>) {
    let Some(tree_node) = dom.node(root) else {
        return;
    };
    if let NodeKind::Element(element) = tree_node.kind()
        && element.local_name == "script"
    {
        let attribute = |name: &str| {
            element
                .attributes
                .iter()
                .find(|attribute| attribute.local_name == name)
                .map(|attribute| attribute.value.clone())
        };
        scripts.push(PendingWriteScript {
            owner: root,
            text: text_of_element(dom, root),
            external: attribute("src"),
            module: attribute("type").is_some_and(|kind| kind == "module"),
        });
    }
    for child in tree_node.children() {
        collect_write_scripts(dom, *child, scripts);
    }
}

/// The text a script element holds: its descendant text concatenated.
fn text_of_element(dom: &Dom, node: NodeId) -> String {
    let mut output = String::new();
    let mut stack = dom
        .node(node)
        .map(Node::children)
        .map(<[NodeId]>::iter)
        .map(|children| children.rev().copied().collect::<Vec<_>>())
        .unwrap_or_default();
    while let Some(current) = stack.pop() {
        match dom.node(current).map(Node::kind) {
            Some(NodeKind::Text(text)) => output.push_str(text),
            Some(NodeKind::Element(_)) => {
                if let Some(children) = dom.node(current).map(Node::children) {
                    stack.extend(children.iter().rev());
                }
            }
            _ => {}
        }
    }
    output
}

/// A fragment context matching the parent of the script that is writing.
fn fragment_context_for(dom: &Dom, script: NodeId) -> FragmentContext {
    let parent = dom
        .parent(script)
        .and_then(|parent| dom.node(parent))
        .map(Node::kind);
    match parent {
        Some(NodeKind::Element(element)) if element.namespace == Namespace::Html => {
            FragmentContext::html(element.local_name.clone())
        }
        Some(NodeKind::Element(element)) => FragmentContext {
            namespace: element.namespace.clone(),
            local_name: element.local_name.clone(),
            attributes: Vec::new(),
            quirks_mode: render_html::QuirksMode::NoQuirks,
        },
        _ => FragmentContext::html("body"),
    }
}

/// Every descendant of `node`'s children, the children included.
fn descendants_of_children(dom: &Dom, node: NodeId) -> Vec<NodeId> {
    let mut output = Vec::new();
    let mut stack = dom
        .children(node)
        .unwrap_or_default()
        .iter()
        .rev()
        .copied()
        .collect::<Vec<_>>();
    while let Some(current) = stack.pop() {
        output.push(current);
        if let Some(children) = dom.children(current) {
            stack.extend(children.iter().rev());
        }
    }
    output
}
