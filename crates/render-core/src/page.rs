//! A single-page coordinator joining script execution and rendering revisions.
//!
//! [`Page`] owns one parsed [`Document`], one JavaScript realm, one event loop,
//! and one rendering invalidation cursor. Script tasks always mutate that same
//! DOM arena. A completed event-loop turn derives a phase plan from the DOM
//! mutation journal, renders only when the plan requires work, and diffs the
//! resulting display list against the preceding snapshot.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

use url::Url;

use crate::document::{Document, DocumentBackends, DocumentRenderOptions, DocumentRenderOutput};
use crate::dom::DomRevision;
use crate::event_loop::{
    EventLoop, EventLoopLimits, MicrotaskCheckpoint, MicrotaskId, QueueError, Runnable, TaskId,
    TaskSource, TurnOutcome,
};
use crate::invalidation::{InvalidationCursor, InvalidationError, RenderingInvalidationPlan};
use crate::js::{CompiledScript, JsError, JsMicrotask, JsRuntime, RuntimeLimits, ScriptOutcome};
use crate::layout::SimpleTextMeasurer;
use crate::paint::{DisplayListDiff, NoGlyphMasks, ReferenceTextShaper};
use crate::script::{
    ClassicScript, ScriptDiagnostic, ScriptDiscoveryLimits, ScriptScheduling, ScriptSource,
    discover_scripts,
};

/// Page-level memory bounds in addition to per-script and scheduler limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageLimits {
    /// Total UTF-8 bytes retained by queued script tasks and microtasks.
    pub max_queued_script_bytes: usize,
}

impl Default for PageLimits {
    fn default() -> Self {
        Self {
            max_queued_script_bytes: 16 * 1_024 * 1_024,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PageOptions {
    pub page_limits: PageLimits,
    pub runtime_limits: RuntimeLimits,
    pub event_loop_limits: EventLoopLimits,
    pub render: DocumentRenderOptions,
}

/// Opaque work carried by the page event loop.
///
/// The enum is the typed extension point for future promise jobs, event
/// dispatch, parser callbacks, and networking completion tasks. Unsupported
/// work is not represented as a stringly typed callback.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum PageTask {
    Script(String),
    CompiledScript(CompiledScript),
    JsMicrotask(JsMicrotask),
}

impl PageTask {
    #[must_use]
    pub fn script(source: impl Into<String>) -> Self {
        Self::Script(source.into())
    }

    fn source_bytes(&self) -> usize {
        match self {
            Self::Script(source) => source.len(),
            Self::CompiledScript(_) | Self::JsMicrotask(_) => 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PageQueueError {
    ScriptSourceLimit {
        bytes: usize,
        limit: usize,
    },
    QueuedSourceBytesLimit {
        current: usize,
        additional: usize,
        limit: usize,
    },
    Scheduler(QueueError),
}

impl fmt::Display for PageQueueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ScriptSourceLimit { bytes, limit } => {
                write!(
                    formatter,
                    "script source uses {bytes} bytes; limit is {limit}"
                )
            }
            Self::QueuedSourceBytesLimit {
                current,
                additional,
                limit,
            } => write!(
                formatter,
                "queued scripts use {current} bytes and cannot retain {additional} more; limit is {limit}"
            ),
            Self::Scheduler(error) => error.fmt(formatter),
        }
    }
}

impl Error for PageQueueError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Scheduler(error) => Some(error),
            Self::ScriptSourceLimit { .. } | Self::QueuedSourceBytesLimit { .. } => None,
        }
    }
}

impl From<QueueError> for PageQueueError {
    fn from(error: QueueError) -> Self {
        Self::Scheduler(error)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocumentScriptQueueError {
    StaleDiscovery {
        discovered_revision: DomRevision,
        current_revision: DomRevision,
    },
    ExternalScriptPending {
        owner: crate::dom::NodeId,
        source_order: usize,
        resolved_url: Url,
    },
    Queue {
        owner: crate::dom::NodeId,
        source_order: usize,
        error: PageQueueError,
    },
}

impl fmt::Display for DocumentScriptQueueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StaleDiscovery {
                discovered_revision,
                current_revision,
            } => write!(
                formatter,
                "script discovery at DOM revision {discovered_revision:?} is stale; current revision is {current_revision:?}"
            ),
            Self::ExternalScriptPending { resolved_url, .. } => write!(
                formatter,
                "external script {resolved_url} requires the browser resource loader"
            ),
            Self::Queue { error, .. } => error.fmt(formatter),
        }
    }
}

impl Error for DocumentScriptQueueError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Queue { error, .. } => Some(error),
            Self::StaleDiscovery { .. } | Self::ExternalScriptPending { .. } => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentScriptQueue {
    pub revision: DomRevision,
    pub queued: Vec<TaskId>,
    pub diagnostics: Vec<ScriptDiagnostic>,
    pub errors: Vec<DocumentScriptQueueError>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageJob {
    Task { id: TaskId, source: TaskSource },
    Microtask { id: MicrotaskId },
}

/// A script result remains observable even when execution throws after making
/// earlier DOM changes. Such partial mutations still participate in rendering.
#[derive(Clone, Debug, PartialEq)]
pub struct PageExecution {
    pub job: PageJob,
    pub result: Result<ScriptOutcome, JsError>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PageRenderUpdate {
    pub previous_revision: Option<DomRevision>,
    pub revision: DomRevision,
    pub display_list_diff: Option<DisplayListDiff>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PageTurnOutcome {
    pub event_loop: TurnOutcome,
    pub executions: Vec<PageExecution>,
    pub invalidation: RenderingInvalidationPlan,
    pub render: Option<PageRenderUpdate>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageError {
    Invalidation(InvalidationError),
}

impl fmt::Display for PageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalidation(error) => error.fmt(formatter),
        }
    }
}

impl Error for PageError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Invalidation(error) => Some(error),
        }
    }
}

impl From<InvalidationError> for PageError {
    fn from(error: InvalidationError) -> Self {
        Self::Invalidation(error)
    }
}

/// Coordinator for one document and its persistent JavaScript realm.
pub struct Page {
    document: Document,
    runtime: JsRuntime,
    event_loop: EventLoop<PageTask>,
    invalidation: InvalidationCursor,
    snapshot: DocumentRenderOutput,
    render_options: DocumentRenderOptions,
    limits: PageLimits,
    max_script_source_bytes: usize,
    queued_source_bytes: usize,
    task_source_bytes: BTreeMap<TaskId, usize>,
    microtask_source_bytes: BTreeMap<MicrotaskId, usize>,
}

impl Page {
    /// Parse and render an initial snapshot with deterministic reference
    /// backends.
    #[must_use]
    pub fn new(html: &str) -> Self {
        Self::with_options(html, PageOptions::default())
    }

    /// Parse and render an initial snapshot with deterministic reference
    /// backends and explicit resource options.
    #[must_use]
    pub fn with_options(html: &str, options: PageOptions) -> Self {
        Self::with_backends(
            html,
            options,
            DocumentBackends {
                text_measurer: &SimpleTextMeasurer,
                text_shaper: &ReferenceTextShaper,
                glyph_masks: &NoGlyphMasks,
            },
        )
    }

    /// Parse exactly once and create the initial render snapshot.
    #[must_use]
    pub fn with_backends(html: &str, options: PageOptions, backends: DocumentBackends<'_>) -> Self {
        let document = Document::parse(html);
        let snapshot = document.render(options.render, backends);
        let runtime = JsRuntime::with_limits(document.dom(), options.runtime_limits);
        let event_loop = EventLoop::with_limits(document.dom(), options.event_loop_limits);
        let invalidation = InvalidationCursor::at_current(document.dom());
        Self {
            document,
            runtime,
            event_loop,
            invalidation,
            snapshot,
            render_options: options.render,
            limits: options.page_limits,
            max_script_source_bytes: options.runtime_limits.max_source_bytes,
            queued_source_bytes: 0,
            task_source_bytes: BTreeMap::new(),
            microtask_source_bytes: BTreeMap::new(),
        }
    }

    #[must_use]
    pub const fn document(&self) -> &Document {
        &self.document
    }

    #[must_use]
    pub const fn runtime(&self) -> &JsRuntime {
        &self.runtime
    }

    #[must_use]
    pub const fn event_loop(&self) -> &EventLoop<PageTask> {
        &self.event_loop
    }

    #[must_use]
    pub const fn snapshot(&self) -> &DocumentRenderOutput {
        &self.snapshot
    }

    #[must_use]
    pub const fn queued_script_source_bytes(&self) -> usize {
        self.queued_source_bytes
    }

    /// Queue a script on the DOM-manipulation task source.
    ///
    /// # Errors
    ///
    /// Returns a typed source-byte or scheduler resource-limit error.
    pub fn queue_script(&mut self, source: impl Into<String>) -> Result<TaskId, PageQueueError> {
        self.queue_task(TaskSource::DomManipulation, PageTask::script(source))
    }

    /// Queue an already tokenized and parsed classic script without repeating
    /// compilation in the event-loop task.
    ///
    /// # Errors
    ///
    /// Returns a typed scheduler resource-limit error.
    pub fn queue_compiled_script(
        &mut self,
        script: CompiledScript,
    ) -> Result<TaskId, PageQueueError> {
        self.queue_task(
            TaskSource::DomManipulation,
            PageTask::CompiledScript(script),
        )
    }

    /// Discover document scripts once and queue supported parser-blocking inline
    /// classic scripts in DOM tree order.
    ///
    /// External scripts are deliberately reported rather than fetched here:
    /// network I/O belongs to the browser resource coordinator. Unsupported
    /// module and scheduling modes remain available as discovery diagnostics.
    #[must_use]
    pub fn queue_document_scripts(
        &mut self,
        base_url: &Url,
        limits: ScriptDiscoveryLimits,
    ) -> DocumentScriptQueue {
        let discovery = discover_scripts(&self.document, base_url, limits);
        let current_revision = self.document.dom().revision();
        if discovery.revision != current_revision {
            return DocumentScriptQueue {
                revision: discovery.revision,
                queued: Vec::new(),
                diagnostics: discovery.diagnostics,
                errors: vec![DocumentScriptQueueError::StaleDiscovery {
                    discovered_revision: discovery.revision,
                    current_revision,
                }],
            };
        }

        let external_errors = discovery
            .scripts
            .iter()
            .filter_map(|script| match &script.source {
                ScriptSource::External { resolved_url, .. } => {
                    Some(DocumentScriptQueueError::ExternalScriptPending {
                        owner: script.owner,
                        source_order: script.source_order,
                        resolved_url: resolved_url.clone(),
                    })
                }
                ScriptSource::Inline { .. } => None,
            })
            .collect::<Vec<_>>();
        if !external_errors.is_empty() {
            return DocumentScriptQueue {
                revision: discovery.revision,
                queued: Vec::new(),
                diagnostics: discovery.diagnostics,
                errors: external_errors,
            };
        }

        let mut queued = Vec::new();
        let mut errors = Vec::new();
        for script in discovery.scripts {
            queue_discovered_inline_script(self, script, &mut queued, &mut errors);
        }
        DocumentScriptQueue {
            revision: discovery.revision,
            queued,
            diagnostics: discovery.diagnostics,
            errors,
        }
    }

    /// Queue typed page work on an explicit task source.
    ///
    /// # Errors
    ///
    /// Returns a typed source-byte or scheduler resource-limit error.
    pub fn queue_task(
        &mut self,
        source: TaskSource,
        task: PageTask,
    ) -> Result<TaskId, PageQueueError> {
        let bytes = self.check_source_capacity(&task)?;
        let id = self.event_loop.queue_task(source, task)?;
        self.retain_source_bytes(bytes);
        self.task_source_bytes.insert(id, bytes);
        Ok(id)
    }

    /// Queue typed page work for the microtask checkpoint after the next task.
    ///
    /// This is the integration point for future promise jobs; the current JS
    /// slice does not create promise microtasks itself.
    ///
    /// # Errors
    ///
    /// Returns a typed source-byte or scheduler resource-limit error.
    pub fn queue_microtask(&mut self, task: PageTask) -> Result<MicrotaskId, PageQueueError> {
        let bytes = self.check_source_capacity(&task)?;
        let id = self.event_loop.queue_microtask(task)?;
        self.retain_source_bytes(bytes);
        self.microtask_source_bytes.insert(id, bytes);
        Ok(id)
    }

    /// Run one event-loop turn with deterministic reference rendering.
    ///
    /// # Errors
    ///
    /// Returns an invalidation lineage error. Individual JavaScript failures
    /// are retained in [`PageTurnOutcome::executions`] because a failed script
    /// may already have mutated the DOM.
    pub fn run_one_turn_reference(&mut self) -> Result<Option<PageTurnOutcome>, PageError> {
        self.run_one_turn(DocumentBackends {
            text_measurer: &SimpleTextMeasurer,
            text_shaper: &ReferenceTextShaper,
            glyph_masks: &NoGlyphMasks,
        })
    }

    /// Run task -> microtask checkpoint -> rendering opportunity.
    ///
    /// # Errors
    ///
    /// Returns an invalidation lineage error. Script failures remain explicit
    /// per execution in the successful turn result.
    pub fn run_one_turn(
        &mut self,
        backends: DocumentBackends<'_>,
    ) -> Result<Option<PageTurnOutcome>, PageError> {
        let mut executions = Vec::new();
        let event_loop = &mut self.event_loop;
        let runtime = &mut self.runtime;
        let document = &mut self.document;
        let task_source_bytes = &mut self.task_source_bytes;
        let microtask_source_bytes = &mut self.microtask_source_bytes;
        let queued_source_bytes = &mut self.queued_source_bytes;

        let Some(event_loop_outcome) =
            event_loop.run_one_turn(document.dom_mut(), |runnable, scheduler, dom| {
                let (job, task, bytes) = match runnable {
                    Runnable::Task {
                        id,
                        source,
                        payload,
                    } => (
                        PageJob::Task { id, source },
                        payload,
                        task_source_bytes.remove(&id).unwrap_or_default(),
                    ),
                    Runnable::Microtask { id, payload } => (
                        PageJob::Microtask { id },
                        payload,
                        microtask_source_bytes.remove(&id).unwrap_or_default(),
                    ),
                };
                *queued_source_bytes = queued_source_bytes.saturating_sub(bytes);
                let result = execute_task(runtime, dom, task);
                executions.push(PageExecution { job, result });
                for microtask in runtime.take_pending_microtasks() {
                    match scheduler.queue_microtask(PageTask::JsMicrotask(microtask)) {
                        Ok(id) => {
                            microtask_source_bytes.insert(id, 0);
                        }
                        Err(error) => {
                            if let Some(execution) = executions.last_mut()
                                && execution.result.is_ok()
                            {
                                execution.result = Err(JsError::resource(format!(
                                    "could not schedule JavaScript microtask: {error}"
                                )));
                            }
                            break;
                        }
                    }
                }
            })
        else {
            return Ok(None);
        };

        if matches!(
            event_loop_outcome.microtasks,
            MicrotaskCheckpoint::ResourceLimitReached { .. }
        ) {
            let discarded_bytes = microtask_source_bytes
                .values()
                .copied()
                .fold(0_usize, usize::saturating_add);
            *queued_source_bytes = queued_source_bytes.saturating_sub(discarded_bytes);
            microtask_source_bytes.clear();
        }

        let invalidation = self.invalidation.take(self.document.dom())?;
        let render = if invalidation.is_empty() {
            None
        } else {
            Some(self.render_update(backends))
        };
        Ok(Some(PageTurnOutcome {
            event_loop: event_loop_outcome,
            executions,
            invalidation,
            render,
        }))
    }

    fn check_source_capacity(&self, task: &PageTask) -> Result<usize, PageQueueError> {
        let bytes = task.source_bytes();
        if bytes > self.max_script_source_bytes {
            return Err(PageQueueError::ScriptSourceLimit {
                bytes,
                limit: self.max_script_source_bytes,
            });
        }
        if self
            .queued_source_bytes
            .checked_add(bytes)
            .is_none_or(|total| total > self.limits.max_queued_script_bytes)
        {
            return Err(PageQueueError::QueuedSourceBytesLimit {
                current: self.queued_source_bytes,
                additional: bytes,
                limit: self.limits.max_queued_script_bytes,
            });
        }
        Ok(bytes)
    }

    fn retain_source_bytes(&mut self, bytes: usize) {
        self.queued_source_bytes = self.queued_source_bytes.saturating_add(bytes);
    }

    fn render_update(&mut self, backends: DocumentBackends<'_>) -> PageRenderUpdate {
        let previous_revision = self.snapshot.revision;
        let next = self.document.render(self.render_options, backends);
        let display_list_diff = next.display.list.diff(&self.snapshot.display.list);
        let revision = next.revision;
        self.snapshot = next;
        PageRenderUpdate {
            previous_revision: Some(previous_revision),
            revision,
            display_list_diff: Some(display_list_diff),
        }
    }
}

fn queue_discovered_inline_script(
    page: &mut Page,
    script: ClassicScript,
    queued: &mut Vec<TaskId>,
    errors: &mut Vec<DocumentScriptQueueError>,
) {
    debug_assert_eq!(script.scheduling, ScriptScheduling::ParserBlocking);
    let ScriptSource::Inline { source } = script.source else {
        unreachable!("external scripts are rejected before queuing begins");
    };
    match page.queue_script(source) {
        Ok(id) => queued.push(id),
        Err(error) => errors.push(DocumentScriptQueueError::Queue {
            owner: script.owner,
            source_order: script.source_order,
            error,
        }),
    }
}

fn execute_task(
    runtime: &mut JsRuntime,
    dom: &mut crate::dom::Dom,
    task: PageTask,
) -> Result<ScriptOutcome, JsError> {
    match task {
        PageTask::Script(source) => runtime.execute(dom, &source),
        PageTask::CompiledScript(script) => runtime.execute_compiled(dom, &script),
        PageTask::JsMicrotask(microtask) => {
            let from_revision = dom.revision();
            runtime
                .invoke_microtask(dom, microtask)
                .map(|value| ScriptOutcome {
                    value,
                    from_revision,
                    to_revision: dom.revision(),
                })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DocumentScriptQueueError, Page, PageJob, PageLimits, PageOptions, PageQueueError, PageTask,
    };
    use crate::dom::{Dom, NodeId, NodeKind};
    use crate::event_loop::{EventLoopLimits, MicrotaskCheckpoint, RenderingDecision};
    use crate::js::RuntimeLimits;
    use crate::script::{ScriptDiagnosticCode, ScriptDiscoveryLimits};
    use url::Url;

    fn element_with_id(dom: &Dom, id: &str) -> NodeId {
        let mut pending = vec![dom.document()];
        while let Some(node) = pending.pop() {
            if matches!(
                dom.node(node).map(crate::dom::Node::kind),
                Some(NodeKind::Element(_))
            ) && dom.attribute(node, "id").expect("element lookup succeeds") == Some(id)
            {
                return node;
            }
            pending.extend(dom.children(node).unwrap_or_default().iter().rev());
        }
        panic!("test element #{id} must exist");
    }

    #[test]
    fn document_inline_scripts_share_one_realm_and_render_after_dom_mutation() {
        let mut page = Page::new(
            "<!doctype html><p id=message>before</p>\
             <script>var suffix = '-first'; document.getElementById('message').textContent = 'script' + suffix;</script>\
             <script>document.getElementById('message').setAttribute('data-order', suffix + '-second');</script>",
        );
        let initial_revision = page.snapshot().revision;
        let queue = page.queue_document_scripts(
            &Url::parse("https://example.test/page").expect("base URL"),
            ScriptDiscoveryLimits::default(),
        );

        assert_eq!(queue.queued.len(), 2);
        assert!(queue.diagnostics.is_empty());
        assert!(queue.errors.is_empty());

        let first = page
            .run_one_turn_reference()
            .expect("first invalidation succeeds")
            .expect("first script is queued");
        assert!(first.executions[0].result.is_ok());
        assert!(first.render.is_some());

        let second = page
            .run_one_turn_reference()
            .expect("second invalidation succeeds")
            .expect("second script is queued");
        assert!(second.executions[0].result.is_ok());
        assert!(second.render.is_some());

        let message = element_with_id(page.document().dom(), "message");
        assert_eq!(
            page.document().dom().attribute(message, "data-order"),
            Ok(Some("-first-second"))
        );
        let text = page.document().dom().children(message).unwrap()[0];
        assert!(matches!(
            page.document().dom().node(text).map(crate::dom::Node::kind),
            Some(NodeKind::Text(value)) if value == "script-first"
        ));
        assert!(page.snapshot().revision > initial_revision);
    }

    #[test]
    fn document_script_queue_reports_unsupported_and_external_work_atomically() {
        let mut page = Page::new(
            "<!doctype html>\
             <script>document.title = 'must-not-run-ahead';</script>\
             <script type=module>ignored()</script>\
             <script src=/app.js></script>\
             <script>document.title = 'also-blocked';</script>\
             <script async>ignored()</script>",
        );
        let queue = page.queue_document_scripts(
            &Url::parse("https://example.test/path/").expect("base URL"),
            ScriptDiscoveryLimits::default(),
        );

        assert!(queue.queued.is_empty());
        assert_eq!(page.event_loop().pending_task_count(), 0);
        assert_eq!(
            queue
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [
                ScriptDiagnosticCode::ModuleUnsupported,
                ScriptDiagnosticCode::AsyncUnsupported,
            ]
        );
        assert!(matches!(
            queue.errors.as_slice(),
            [DocumentScriptQueueError::ExternalScriptPending { resolved_url, .. }]
                if resolved_url.as_str() == "https://example.test/app.js"
        ));
    }

    #[test]
    fn compiled_script_task_executes_without_reparsing_in_the_page_realm() {
        let mut page = Page::new("<!doctype html><p id=message>before</p>");
        let script = crate::js::CompiledScript::compile(
            "document.getElementById('message').textContent = 'compiled';",
            &RuntimeLimits::default(),
        )
        .expect("script compiles before queuing");
        page.queue_compiled_script(script)
            .expect("compiled task fits scheduler");

        let turn = page
            .run_one_turn_reference()
            .expect("invalidation succeeds")
            .expect("compiled task is ready");

        assert!(turn.executions[0].result.is_ok());
        let message = element_with_id(page.document().dom(), "message");
        let text = page.document().dom().children(message).unwrap()[0];
        assert!(matches!(
            page.document().dom().node(text).map(crate::dom::Node::kind),
            Some(NodeKind::Text(value)) if value == "compiled"
        ));
    }

    #[test]
    fn script_turn_mutates_one_dom_and_produces_plan_snapshot_and_dirty_diff() {
        let mut page = Page::new(
            "<!doctype html><style>p { display:block; width:160px }</style>\
             <p id=message>before</p>",
        );
        let target_before = element_with_id(page.document().dom(), "message");
        let initial_revision = page.snapshot().revision;
        page.queue_script(
            r#"
                const target = document.getElementById("message");
                target.textContent = "updated";
                target.setAttribute("data-state", "live");
                const badge = document.createElement("span");
                badge.setAttribute("id", "badge");
                badge.textContent = "!";
                target.appendChild(badge);
            "#,
        )
        .expect("script task fits");

        let turn = page
            .run_one_turn_reference()
            .expect("invalidation lineage remains valid")
            .expect("script task is ready");

        assert_eq!(turn.executions.len(), 1);
        assert!(turn.executions[0].result.is_ok());
        assert!(matches!(turn.executions[0].job, PageJob::Task { .. }));
        assert!(matches!(
            turn.event_loop.rendering.decision,
            RenderingDecision::Incremental(_)
        ));
        assert_eq!(turn.invalidation.from_revision, initial_revision);
        assert_eq!(
            turn.invalidation.to_revision,
            page.document().dom().revision()
        );
        assert!(turn.invalidation.style.is_required());
        assert!(turn.invalidation.layout.is_required());
        assert!(turn.invalidation.paint.is_required());

        let target_after = element_with_id(page.document().dom(), "message");
        assert_eq!(target_after, target_before);
        assert_eq!(
            page.document().dom().attribute(target_after, "data-state"),
            Ok(Some("live"))
        );
        let badge = element_with_id(page.document().dom(), "badge");
        assert_eq!(page.document().dom().parent(badge), Some(target_after));

        let update = turn.render.expect("DOM mutations require rendering");
        assert_eq!(update.previous_revision, Some(initial_revision));
        assert_eq!(update.revision, page.snapshot().revision);
        let diff = update
            .display_list_diff
            .expect("previous snapshot yields a diff");
        assert_eq!(diff.from_revision, initial_revision);
        assert_eq!(diff.to_revision, page.snapshot().revision);
        assert!(!diff.inserted.is_empty() || !diff.changed.is_empty());
        assert!(!diff.dirty_rects.is_empty());
        assert_eq!(page.queued_script_source_bytes(), 0);
    }

    #[test]
    fn typed_microtask_runs_after_task_in_the_same_rendering_turn() {
        let mut page = Page::new("<!doctype html><p id=message>before</p>");
        page.queue_script(
            "const target = document.getElementById('message'); target.textContent = 'task';",
        )
        .expect("task fits");
        page.queue_microtask(PageTask::script("target.textContent = 'microtask';"))
            .expect("microtask fits");

        let turn = page
            .run_one_turn_reference()
            .expect("invalidation succeeds")
            .expect("task is ready");

        assert_eq!(turn.executions.len(), 2);
        assert!(matches!(turn.executions[0].job, PageJob::Task { .. }));
        assert!(matches!(turn.executions[1].job, PageJob::Microtask { .. }));
        assert!(
            turn.executions
                .iter()
                .all(|execution| execution.result.is_ok())
        );
        let message = element_with_id(page.document().dom(), "message");
        let text = page
            .document()
            .dom()
            .children(message)
            .expect("message children")[0];
        assert!(matches!(
            page.document().dom().node(text).map(crate::dom::Node::kind),
            Some(NodeKind::Text(value)) if value == "microtask"
        ));
    }

    #[test]
    fn script_queue_microtask_runs_before_the_same_turns_rendering_opportunity() {
        let mut page = Page::new("<!doctype html><p id=message>before</p>");
        page.queue_script(
            "function update() { const target = document.getElementById('message'); target.textContent = 'microtask'; } queueMicrotask(update);",
        )
        .expect("script task fits");

        let turn = page
            .run_one_turn_reference()
            .expect("invalidation succeeds")
            .expect("script task is ready");

        assert_eq!(turn.executions.len(), 2);
        assert!(matches!(turn.executions[0].job, PageJob::Task { .. }));
        assert!(matches!(turn.executions[1].job, PageJob::Microtask { .. }));
        assert!(
            turn.executions
                .iter()
                .all(|execution| execution.result.is_ok())
        );
        assert!(matches!(
            turn.event_loop.microtasks,
            MicrotaskCheckpoint::Complete { executed: 1 }
        ));
        assert!(turn.invalidation.layout.is_required());
        let message = element_with_id(page.document().dom(), "message");
        let text = page.document().dom().children(message).unwrap()[0];
        assert!(matches!(
            page.document().dom().node(text).map(crate::dom::Node::kind),
            Some(NodeKind::Text(value)) if value == "microtask"
        ));
    }

    #[test]
    fn promise_reactions_run_in_registration_order_before_rendering() {
        let mut page = Page::new("<!doctype html><p id=message>before</p>");
        page.queue_script(
            "const target = document.getElementById('message'); let order = ''; Promise.resolve('A').then(function(value) { order = order + value; target.textContent = order; return 'B'; }).then(function(value) { order = order + value; target.textContent = order; }); order = order + 'S';",
        )
        .expect("script task fits");

        let turn = page
            .run_one_turn_reference()
            .expect("invalidation succeeds")
            .expect("script task is ready");

        assert_eq!(turn.executions.len(), 3);
        assert!(matches!(turn.executions[0].job, PageJob::Task { .. }));
        assert!(
            turn.executions[1..]
                .iter()
                .all(|execution| matches!(execution.job, PageJob::Microtask { .. }))
        );
        assert!(
            turn.executions
                .iter()
                .all(|execution| execution.result.is_ok())
        );
        assert!(matches!(
            turn.event_loop.microtasks,
            MicrotaskCheckpoint::Complete { executed: 2 }
        ));
        let message = element_with_id(page.document().dom(), "message");
        let text = page.document().dom().children(message).unwrap()[0];
        assert!(matches!(
            page.document().dom().node(text).map(crate::dom::Node::kind),
            Some(NodeKind::Text(value)) if value == "SAB"
        ));
    }

    #[test]
    fn promise_rejection_is_recovered_by_catch_before_rendering() {
        let mut page = Page::new("<!doctype html><p id=message>before</p>");
        page.queue_script(
            "const target = document.getElementById('message'); Promise.reject('failure').catch(function(reason) { target.textContent = 'caught-' + reason; });",
        )
        .expect("script task fits");

        let turn = page
            .run_one_turn_reference()
            .expect("invalidation succeeds")
            .expect("script task is ready");

        assert_eq!(turn.executions.len(), 2, "{:#?}", turn.executions);
        assert!(
            turn.executions
                .iter()
                .all(|execution| execution.result.is_ok())
        );
        let message = element_with_id(page.document().dom(), "message");
        let text = page.document().dom().children(message).unwrap()[0];
        assert!(matches!(
            page.document().dom().node(text).map(crate::dom::Node::kind),
            Some(NodeKind::Text(value)) if value == "caught-failure"
        ));
    }

    #[test]
    fn queue_microtask_and_promise_reactions_share_fifo_ordering() {
        let mut page = Page::new("<!doctype html><p id=message>before</p>");
        page.queue_script(
            "const target = document.getElementById('message'); let order = ''; queueMicrotask(function() { order = order + 'Q'; target.textContent = order; }); Promise.resolve().then(function() { order = order + 'P'; target.textContent = order; });",
        )
        .expect("script task fits");

        let turn = page
            .run_one_turn_reference()
            .expect("invalidation succeeds")
            .expect("script task is ready");

        assert_eq!(turn.executions.len(), 3);
        let message = element_with_id(page.document().dom(), "message");
        let text = page.document().dom().children(message).unwrap()[0];
        assert!(matches!(
            page.document().dom().node(text).map(crate::dom::Node::kind),
            Some(NodeKind::Text(value)) if value == "QP"
        ));
    }

    #[test]
    fn thrown_reaction_rejects_the_chained_promise() {
        let mut page = Page::new("<!doctype html><p id=message>before</p>");
        page.queue_script(
            "const target = document.getElementById('message'); Promise.resolve().then(function() { throw 'reaction-failed'; }).catch(function(reason) { target.textContent = reason; });",
        )
        .expect("script task fits");

        let turn = page
            .run_one_turn_reference()
            .expect("invalidation succeeds")
            .expect("script task is ready");

        assert_eq!(turn.executions.len(), 3);
        assert!(
            turn.executions
                .iter()
                .all(|execution| execution.result.is_ok())
        );
        let message = element_with_id(page.document().dom(), "message");
        let text = page.document().dom().children(message).unwrap()[0];
        assert!(matches!(
            page.document().dom().node(text).map(crate::dom::Node::kind),
            Some(NodeKind::Text(value)) if value == "reaction-failed"
        ));
    }

    #[test]
    fn promise_chain_adopts_a_returned_promise() {
        let mut page = Page::new("<!doctype html><p id=message>before</p>");
        page.queue_script(
            "const target = document.getElementById('message'); Promise.resolve('outer').then(function() { return Promise.resolve('inner'); }).then(function(value) { target.textContent = value; });",
        )
        .expect("script task fits");

        let turn = page
            .run_one_turn_reference()
            .expect("invalidation succeeds")
            .expect("script task is ready");

        assert_eq!(turn.executions.len(), 3);
        assert!(
            turn.executions
                .iter()
                .all(|execution| execution.result.is_ok())
        );
        let message = element_with_id(page.document().dom(), "message");
        let text = page.document().dom().children(message).unwrap()[0];
        assert!(matches!(
            page.document().dom().node(text).map(crate::dom::Node::kind),
            Some(NodeKind::Text(value)) if value == "inner"
        ));
    }

    #[test]
    fn promise_constructor_resolve_adopts_another_promise() {
        let mut page = Page::new("<!doctype html><p id=message>before</p>");
        page.queue_script(
            "const target = document.getElementById('message'); const inner = Promise.resolve('adopted'); const outer = new Promise(function(resolve) { resolve(inner); }); outer.then(function(value) { target.textContent = value; });",
        )
        .expect("script task fits");

        let turn = page
            .run_one_turn_reference()
            .expect("invalidation succeeds")
            .expect("script task is ready");

        assert_eq!(turn.executions.len(), 2);
        assert!(
            turn.executions
                .iter()
                .all(|execution| execution.result.is_ok())
        );
        let message = element_with_id(page.document().dom(), "message");
        let text = page.document().dom().children(message).unwrap()[0];
        assert!(matches!(
            page.document().dom().node(text).map(crate::dom::Node::kind),
            Some(NodeKind::Text(value)) if value == "adopted"
        ));
    }

    #[test]
    fn promise_constructor_executor_settles_only_once() {
        let mut page = Page::new("<!doctype html><p id=message>before</p>");
        page.queue_script(
            "const target = document.getElementById('message'); const promise = new Promise(function(resolve, reject) { resolve('first'); reject('second'); }); promise.then(function(value) { target.textContent = value; });",
        )
        .expect("script task fits");

        let turn = page
            .run_one_turn_reference()
            .expect("invalidation succeeds")
            .expect("script task is ready");

        assert_eq!(turn.executions.len(), 2);
        assert!(
            turn.executions
                .iter()
                .all(|execution| execution.result.is_ok())
        );
        let message = element_with_id(page.document().dom(), "message");
        let text = page.document().dom().children(message).unwrap()[0];
        assert!(matches!(
            page.document().dom().node(text).map(crate::dom::Node::kind),
            Some(NodeKind::Text(value)) if value == "first"
        ));
    }

    #[test]
    fn each_javascript_microtask_gets_a_fresh_execution_budget() {
        let options = PageOptions {
            runtime_limits: RuntimeLimits {
                max_execution_steps: 200,
                ..RuntimeLimits::default()
            },
            ..PageOptions::default()
        };
        let mut page = Page::with_options("<!doctype html><p id=message>before</p>", options);
        page.queue_script(
            "let i = 0; while (i < 10) { i = i + 1; } queueMicrotask(function() { let j = 0; while (j < 10) { j = j + 1; } document.getElementById('message').textContent = 'fresh-budget'; });",
        )
        .expect("script task fits");

        let turn = page
            .run_one_turn_reference()
            .expect("invalidation succeeds")
            .expect("script task is ready");

        assert_eq!(turn.executions.len(), 2, "{:#?}", turn.executions);
        assert!(
            turn.executions
                .iter()
                .all(|execution| execution.result.is_ok()),
            "{:#?}",
            turn.executions
        );
        let message = element_with_id(page.document().dom(), "message");
        let text = page.document().dom().children(message).unwrap()[0];
        assert!(matches!(
            page.document().dom().node(text).map(crate::dom::Node::kind),
            Some(NodeKind::Text(value)) if value == "fresh-budget"
        ));
    }

    #[test]
    fn queued_closure_retains_its_shared_environment_until_the_checkpoint() {
        let mut page = Page::new("<!doctype html><p id=message>before</p>");
        page.queue_script(
            "function schedule() { let prefix = 'closed-'; function update() { const target = document.getElementById('message'); target.textContent = prefix + 'over'; } queueMicrotask(update); prefix = 'carried-'; } schedule();",
        )
        .expect("script task fits");

        let turn = page
            .run_one_turn_reference()
            .expect("invalidation succeeds")
            .expect("script task is ready");

        assert_eq!(turn.executions.len(), 2);
        assert!(
            turn.executions
                .iter()
                .all(|execution| execution.result.is_ok())
        );
        let message = element_with_id(page.document().dom(), "message");
        let text = page.document().dom().children(message).unwrap()[0];
        assert!(matches!(
            page.document().dom().node(text).map(crate::dom::Node::kind),
            Some(NodeKind::Text(value)) if value == "carried-over"
        ));
    }

    #[test]
    fn page_source_limits_and_discarded_microtasks_keep_accounting_bounded() {
        let options = PageOptions {
            page_limits: PageLimits {
                max_queued_script_bytes: 8,
            },
            runtime_limits: RuntimeLimits {
                max_source_bytes: 6,
                ..RuntimeLimits::default()
            },
            event_loop_limits: EventLoopLimits {
                max_microtasks_per_checkpoint: 0,
                ..EventLoopLimits::default()
            },
            ..PageOptions::default()
        };
        let mut page = Page::with_options("<!doctype html><p>page</p>", options);

        assert!(matches!(
            page.queue_script("1234567"),
            Err(PageQueueError::ScriptSourceLimit { .. })
        ));
        page.queue_script("").expect("empty task fits");
        page.queue_microtask(PageTask::script("123456"))
            .expect("bounded microtask fits");
        assert!(matches!(
            page.queue_microtask(PageTask::script("abc")),
            Err(PageQueueError::QueuedSourceBytesLimit { .. })
        ));
        assert_eq!(page.queued_script_source_bytes(), 6);

        let turn = page
            .run_one_turn_reference()
            .expect("invalidation succeeds")
            .expect("empty script task is ready");
        assert!(matches!(
            turn.event_loop.microtasks,
            MicrotaskCheckpoint::ResourceLimitReached {
                executed: 0,
                discarded: 1
            }
        ));
        assert_eq!(page.queued_script_source_bytes(), 0);
    }
}
