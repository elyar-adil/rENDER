//! Native browser shell for the self-owned Rust rendering pipeline.
#![allow(clippy::cast_precision_loss)]
use crate::ACTIVE_PAGE_TURN_BUDGET;
use crate::fetch_handles::CachedBatchHandle;
use crate::fetch_handles::CachedRequestHandle;
use crate::page_source::PageSource;
use render_browser::images::ImageFetchPlan;
use render_browser::model::PageScrollState;
use render_browser::resources::StylesheetFetchPlan;
use render_browser::scripts::ScriptBatchPreparation;
use render_browser::scripts::ScriptFetchPlan;
use render_browser::worker::RenderIdentity;
use render_core::css::computed::ComputedStyle;
use render_core::document::DocumentRenderOptions;
use render_core::document::ExternalStyleSheetKey;
use render_core::document::ExternalStyleSheets;
use render_core::image::ImageResourceKey;
use render_core::image::ImageResources;
use render_core::js::ElementRect;
use render_core::js::JsValue;
use render_core::navigation::HistoryEntry;
use render_core::navigation::NavigationLimits;
use render_core::navigation::SessionHistory;
use render_core::page::Page;
use render_core::page::PageJob;
use render_core::paint::Color;
use render_core::paint::DisplayList;
use render_core::paint::PaintScene;
use render_core::script::ScriptScheduling;
use render_net::CookieJar;
use render_net::FetchResult;
use render_net::Url;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;
use winit::dpi::PhysicalSize as WindowSize;

/// Upper bound on event-loop turns drained while executing one prepared
/// classic-script batch.
///
/// The drain normally finishes once the queued scripts and their load/error
/// events have run (a handful of turns). A page whose scripts re-queue ready
/// tasks from within those tasks could otherwise keep the UI thread in this
/// loop forever, which blocks every other stage — network polling, render
/// commits, input — indefinitely. When the budget is hit the remaining work
/// stays in the page's event loop and drains through the normal bounded page
/// pump instead.
const SCRIPT_BATCH_TURN_BUDGET: usize = 1_024;

#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag tracks an independent pipeline stage: styles, scripts, images, pending rerender"
)]
pub(super) struct PageState {
    pub(super) navigation: PageNavigation<CachedRequestHandle>,
    pub(super) page: Page,
    pub(super) cookies: CookieJar,
    pub(super) style_sheets: ExternalStyleSheets,
    /// Keys already submitted for this document. A failed stylesheet is not
    /// resubmitted on every paint; changing its URL creates a new key.
    pub(super) started_style_sheets: HashSet<ExternalStyleSheetKey>,
    pub(super) style_batch: Option<(StylesheetFetchPlan, Vec<FetchResult>)>,
    pub(super) images: ImageResources,
    /// Failed image loads are retried when their source changes, but not on
    /// every paint of the same broken URL.
    pub(super) attempted_images: HashSet<ImageResourceKey>,
    pub(super) computed_styles: BTreeMap<render_core::dom::NodeId, ComputedStyle>,
    pub(super) pending_images: Option<PendingImages>,
    pub(super) styles_resolved: bool,
    pub(super) pending_style_sheets: Option<PendingStyleSheets>,
    pub(super) scripts_resolved: bool,
    pub(super) pending_scripts: Option<PendingScripts>,
    /// A prepared script batch that finished while the page's first
    /// stylesheet batch was still loading. Stylesheets block script
    /// execution, not fetching, so the batch waits here in document order
    /// until the stylesheets resolve.
    pub(super) held_scripts: Option<ScriptBatchPreparation>,
    pub(super) started_scripts: HashSet<render_core::dom::NodeId>,
    pub(super) initial_script_scan_completed: bool,
    /// A DOM mutation (or style/image generation change) arrived while a
    /// render for this tab was already running. The running render stays
    /// committable, so instead of superseding it — which starves commits
    /// whenever mutations arrive faster than renders finish — the mutation is
    /// recorded here and the coordinator resubmits right after the commit.
    pub(super) render_dirty: bool,
    /// The viewport the coalesced request asked for. The committed
    /// `viewport` cannot stand in for it: a navigation resets that field to
    /// zero, and resubmitting a zero-sized viewport would cancel the tab
    /// instead of rendering the document the request was made for.
    pub(super) render_dirty_viewport: Option<WindowSize<u32>>,
    pub(super) frame: Vec<u32>,
    pub(super) viewport: WindowSize<u32>,
    pub(super) display_list: Option<Arc<DisplayList>>,
    pub(super) paint_scene: Option<Arc<PaintScene>>,
    pub(super) geometry: BTreeMap<u64, ElementRect>,
    pub(super) raster_background: Color,
    pub(super) scroll: PageScrollState,
    pub(super) history: SessionHistory,
    pub(super) dom_revision: u64,
    pub(super) external_styles_generation: u64,
    pub(super) render_generation: u64,
    pub(super) expected_render: Option<RenderIdentity>,
    /// Wall-clock anchor for the page's virtual event-loop clock.
    pub(super) created_at: Instant,
}

pub(super) struct PageNavigation<H> {
    pub(super) committed: PageSource,
    pub(super) pending: Option<PendingNavigation<H>>,
}

pub(super) struct PendingNavigation<H> {
    pub(super) requested_url: Url,
    pub(super) handle: H,
    /// Wall-clock anchor for stall reporting; a navigation or resource batch
    /// that stays in flight this long is surfaced on stderr once.
    pub(super) since: Instant,
    pub(super) stall_reported: bool,
}

/// A resource batch in flight, with the wall-clock anchor for stall
/// reporting (`since`/`stall_reported`).
pub(super) struct PendingStyleSheets {
    pub(super) plan: StylesheetFetchPlan,
    pub(super) handle: CachedBatchHandle,
    pub(super) since: Instant,
    pub(super) stall_reported: bool,
}

pub(super) struct PendingScripts {
    pub(super) plan: ScriptFetchPlan,
    pub(super) handle: CachedBatchHandle,
    pub(super) since: Instant,
    pub(super) stall_reported: bool,
    /// Set while the batch is fetching module dependencies rather than the
    /// scripts themselves: the batch already prepared, and the requests whose
    /// results are in flight.
    pub(super) module_phase: Option<ModulePhase>,
}

pub(super) struct ModulePhase {
    pub(super) preparation: ScriptBatchPreparation,
    pub(super) requests: Vec<render_net::FetchRequest>,
}

pub(super) struct PendingImages {
    pub(super) plan: ImageFetchPlan,
    pub(super) handle: CachedBatchHandle,
    pub(super) since: Instant,
    pub(super) stall_reported: bool,
}

impl<H> PageNavigation<H> {
    pub(super) fn new(committed: PageSource) -> Self {
        Self {
            committed,
            pending: None,
        }
    }

    pub(super) fn begin(&mut self, requested_url: Url, handle: H) {
        self.pending = Some(PendingNavigation {
            requested_url,
            handle,
            since: Instant::now(),
            stall_reported: false,
        });
    }

    pub(super) fn commit(&mut self, source: PageSource) {
        self.committed = source;
        self.pending = None;
    }

    pub(super) fn take_pending(&mut self) -> Option<PendingNavigation<H>> {
        self.pending.take()
    }

    pub(super) fn pending_url(&self) -> Option<&Url> {
        self.pending.as_ref().map(|pending| &pending.requested_url)
    }

    pub(super) const fn committed(&self) -> &PageSource {
        &self.committed
    }
}

impl PageState {
    pub(super) fn new(source: PageSource) -> Self {
        let history = SessionHistory::new(
            HistoryEntry::new(source.target.history_url()),
            NavigationLimits::default(),
        )
        .expect("browser-created page URLs fit the session-history limits");
        let page = Page::with_url_unrendered(&source.html, &source.target.history_url());
        Self {
            navigation: PageNavigation::new(source),
            page,
            cookies: CookieJar::default(),
            style_sheets: ExternalStyleSheets::default(),
            started_style_sheets: HashSet::new(),
            style_batch: None,
            images: ImageResources::default(),
            attempted_images: HashSet::new(),
            computed_styles: BTreeMap::new(),
            pending_images: None,
            styles_resolved: false,
            pending_style_sheets: None,
            scripts_resolved: false,
            pending_scripts: None,
            held_scripts: None,
            started_scripts: HashSet::new(),
            initial_script_scan_completed: false,
            render_dirty: false,
            render_dirty_viewport: None,
            frame: Vec::new(),
            viewport: WindowSize::new(0, 0),
            display_list: None,
            paint_scene: None,
            geometry: BTreeMap::new(),
            raster_background: DocumentRenderOptions::default().raster_background,
            scroll: PageScrollState::default(),
            history,
            dom_revision: 1,
            external_styles_generation: 0,
            render_generation: 0,
            expected_render: None,
            created_at: Instant::now(),
        }
    }

    pub(super) fn set_source(&mut self, source: PageSource) {
        self.cancel_style_sheets();
        self.cancel_scripts();
        self.page = Page::with_url_unrendered(&source.html, &source.target.history_url());
        self.navigation.commit(source);
        self.style_sheets = ExternalStyleSheets::default();
        self.started_style_sheets.clear();
        self.style_batch = None;
        self.images = ImageResources::default();
        self.attempted_images.clear();
        self.computed_styles.clear();
        self.styles_resolved = false;
        self.scripts_resolved = false;
        self.started_scripts.clear();
        self.initial_script_scan_completed = false;
        self.render_dirty = false;
        self.render_dirty_viewport = None;
        self.frame.clear();
        self.viewport = WindowSize::new(0, 0);
        self.display_list = None;
        self.paint_scene = None;
        self.geometry.clear();
        self.scroll.reset();
        self.dom_revision = self.page.document().dom().revision().as_u64();
        self.external_styles_generation = 0;
        self.expected_render = None;
        self.created_at = Instant::now();
    }

    pub(super) fn cancel_pending(&mut self) {
        if let Some(pending) = self.navigation.take_pending() {
            pending.handle.cancel();
        }
        self.cancel_style_sheets();
        self.cancel_scripts();
        self.cancel_images();
    }

    pub(super) fn cancel_style_sheets(&mut self) {
        if let Some(pending) = self.pending_style_sheets.take() {
            pending.handle.cancel();
        }
    }

    pub(super) fn cancel_scripts(&mut self) {
        if let Some(pending) = self.pending_scripts.take() {
            pending.handle.cancel();
        }
        self.held_scripts = None;
    }

    pub(super) fn cancel_images(&mut self) {
        if let Some(pending) = self.pending_images.take() {
            pending.handle.cancel();
        }
    }

    /// The next round of module-dependency requests for a prepared batch,
    /// carrying this page's cookies. Empty once the module graph is closed.
    pub(super) fn module_round_requests(
        &self,
        preparation: &mut ScriptBatchPreparation,
    ) -> Vec<render_net::FetchRequest> {
        preparation
            .pending_module_requests()
            .into_iter()
            .map(|request| self.cookies.decorate_request(request))
            .collect()
    }

    pub(super) fn execute_script_batch(&mut self, preparation: ScriptBatchPreparation) -> bool {
        let revision_before_execution = self.page.document().dom().revision();
        let mut origins = preparation
            .scripts
            .iter()
            .enumerate()
            .map(|(input_order, script)| {
                (
                    script.scheduling,
                    script.source_order,
                    input_order,
                    script.owner,
                    script.final_url.clone(),
                )
            })
            .collect::<Vec<_>>();
        origins.sort_by_key(
            |(scheduling, source_order, input_order, _, _)| match scheduling {
                ScriptScheduling::ParserBlocking => (0, *source_order),
                ScriptScheduling::Async => (1, *input_order),
                ScriptScheduling::Defer => (2, *source_order),
            },
        );
        let queue = self.page.queue_prepared_scripts(
            preparation.revision,
            preparation.scripts.into_iter().map(Into::into).collect(),
        );
        let failed = queue
            .errors
            .iter()
            .filter_map(|error| match error {
                render_core::page::DocumentScriptQueueError::Queue {
                    owner,
                    source_order,
                    ..
                } => Some((*owner, *source_order)),
                _ => None,
            })
            .collect::<HashSet<_>>();
        for error in &queue.errors {
            eprintln!("render-browser could not queue classic script: {error}");
        }
        let origins = queue
            .queued
            .iter()
            .copied()
            .zip(
                origins
                    .into_iter()
                    .filter(|(_, source_order, _, owner, _)| {
                        !failed.contains(&(*owner, *source_order))
                    }),
            )
            .map(|(task, (_, source_order, _, owner, final_url))| {
                (task, (owner, source_order, final_url))
            })
            .collect::<HashMap<_, _>>();
        let mut turns = 0_usize;
        loop {
            if turns >= SCRIPT_BATCH_TURN_BUDGET {
                eprintln!(
                    "render-browser script batch hit its {SCRIPT_BATCH_TURN_BUDGET}-turn budget; \
                     remaining page work drains through the bounded page pump"
                );
                break;
            }
            turns += 1;
            match self.page.run_one_turn_without_render() {
                Ok(Some(turn)) => {
                    let mut turn_origin = None;
                    for execution in turn.executions {
                        if let PageJob::Task { id, .. } = execution.job {
                            turn_origin = origins.get(&id);
                        }
                        if let Err(error) = execution.result {
                            if let Some((owner, source_order, final_url)) = turn_origin {
                                let url = final_url.as_ref().map_or("inline", Url::as_str);
                                eprintln!(
                                    "render-browser classic script node {owner:?} source {source_order} {url} failed: {error}"
                                );
                            } else {
                                eprintln!("render-browser classic script failed: {error}");
                            }
                        }
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    eprintln!("render-browser page turn failed: {error}");
                    break;
                }
            }
        }
        self.dom_revision = self.page.document().dom().revision().as_u64();
        self.page.document().dom().revision() != revision_before_execution
    }

    /// Derive the committed page title from the document's `<title>` element,
    /// keeping the URL-based fallback when the document has none.
    ///
    /// Returns whether the committed title changed.
    pub(super) fn sync_committed_title(&mut self) -> bool {
        let derived = self.page.document_title().and_then(|title| {
            let trimmed = title.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_owned())
            }
        });
        let Some(title) = derived else {
            return false;
        };
        if self.navigation.committed().title == title {
            return false;
        }
        self.navigation.committed.title = title;
        true
    }

    /// Print buffered `console.*` output from the page's script runtime.
    pub(super) fn drain_console(&mut self) {
        for message in self.page.runtime_mut().take_console_messages() {
            eprintln!("[console.{}] {}", message.level.label(), message.text);
        }
    }

    /// Advance the page's virtual clock to real elapsed time and run every
    /// ready turn (timers, queued events, scripts).
    ///
    /// Returns whether any turn mutated the DOM plus per-task default-action
    /// results (`true` when the task's event was not `preventDefault()`-ed).
    pub(super) fn run_page_turns(
        &mut self,
    ) -> (bool, HashMap<render_core::event_loop::TaskId, bool>) {
        self.run_page_turns_with_budget(ACTIVE_PAGE_TURN_BUDGET)
    }

    pub(super) fn run_page_turns_with_budget(
        &mut self,
        turn_budget: usize,
    ) -> (bool, HashMap<render_core::event_loop::TaskId, bool>) {
        let now = self.created_at.elapsed();
        let revision_before = self.page.document().dom().revision().as_u64();
        let mut defaults = HashMap::new();
        match self.page.pump_at_most_without_render(now, turn_budget) {
            Ok(outcome) => {
                for (id, result) in outcome.task_results {
                    let default_allowed = match &result {
                        Ok(script) => matches!(script.value, JsValue::Boolean(true)),
                        // A throwing listener never prevents the default action.
                        Err(_) => true,
                    };
                    defaults.insert(id, default_allowed);
                }
            }
            Err(error) => eprintln!("render-browser page pump failed: {error}"),
        }
        self.drain_console();
        let revision_after = self.page.document().dom().revision().as_u64();
        self.dom_revision = revision_after;
        let changed = revision_after != revision_before;
        if changed {
            // Timers, event handlers and fetch callbacks may append scripts
            // long after the initial document scan completed.
            self.scripts_resolved = false;
        }
        (changed, defaults)
    }

    /// Earliest wall-clock instant at which this page needs a wake-up.
    pub(super) fn next_wake_instant(&self) -> Option<Instant> {
        self.page
            .next_wake_deadline()
            .map(|deadline| self.created_at + deadline)
    }

    /// Whether the page still has script work that requires future turns.
    pub(super) fn has_pending_script_work(&self) -> bool {
        self.page.has_pending_immediate_work()
    }

    /// Whether a repaint was coalesced behind a render that has not reported a
    /// completion, so only the coordinator can still honour it.
    pub(super) const fn has_unresolved_render_request(&self) -> bool {
        self.render_dirty
    }
}
