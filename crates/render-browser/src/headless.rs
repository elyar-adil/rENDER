//! Offscreen page runner for auditing real pages without a display server.
//!
//! The session owns one [`Page`] plus the browser resource loaders: external
//! stylesheets, classic and module scripts, images, webfonts, `fetch()` and
//! dynamic `import()`. Every transfer is synchronous on the calling thread, so
//! the same code path works on a CI machine with no compositor, no graphics
//! API and no native window.
//!
//! Rendering happens through [`render_core::document::Document::render_with_interaction_and_transitions_and_frames`]
//! on a snapshot of the page DOM. The returned raster surface is already in
//! device pixels: layout and script geometry stay in CSS pixels and
//! `DocumentRenderOptions::device_scale` converts only the paint pass.
//!
//! This module deliberately contains no window, graphics-surface or input
//! dependency. The only I/O is HTTP(S)/file reads and the caller's PNG write.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::struct_excessive_bools,
    reason = "viewport/DPR values are small positive finite numbers, and the bools are independent pipeline stage flags"
)]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use render_core::css::computed::ComputedStyle;
use render_core::css::selector::InteractionState;
use render_core::css::transition::TransitionState;
use render_core::document::{
    DocumentBackends, DocumentLimits, DocumentRenderOptions, DocumentRenderOutput,
    ExternalStyleSheetKey, ExternalStyleSheets,
};
use render_core::dom::{NodeId, NodeKind};
use render_core::event_loop::TaskId;
use render_core::html::{HtmlDecodeOptions, decode_html_bytes};
use render_core::image::{
    ImageLimits, ImageResourceKey, ImageResources, ImageSelectionContext, ImageSource,
};
use render_core::import_map::ImportMap;
use render_core::js::{
    DocumentReadyState, DynamicModuleUnit, ElementRect, FetchCredentials, FetchOutcome,
};
use render_core::layout::{LayoutOptions, PhysicalSize};
use render_core::module_graph::{ModuleGraph, module_key};
use render_core::page::{Page, PageDomEvent, PageJob, PageOptions};
use render_core::paint::Surface;
use render_core::script::ScriptDiscoveryLimits;
use render_net::{
    BatchOptions, CancelToken, CookieJar, FetchConfig, FetchRequest, FetchResult, Header,
    HttpMethod, HttpProxyConfig, HttpTransport, Url,
};

use crate::font_backend::SystemFontBackend;
use crate::font_faces::{
    DocumentFontLoader, FontFetchPlan, SystemFontFaceInstaller, apply_font_batch,
};
use crate::images::{
    ImageFetchPlan, ImageResourceDiagnosticCode, apply_image_batch,
    plan_images_with_styles_and_context,
};
use crate::resources::{
    StylesheetFetchPlan, StylesheetResourceDiagnostic, apply_stylesheet_batch,
    plan_external_style_sheets,
};
use crate::scripts::{
    SCRIPT_ACCEPT, ScriptBatchPreparation, ScriptResourceDiagnostic, ScriptResourceSeverity,
    plan_classic_scripts, plan_unstarted_classic_scripts, prepare_script_batch,
};

/// Maximum outer resource/pump cycles before the runner reports a timeout.
const MAX_SETTLE_CYCLES: usize = 4_096;
/// Turns drained by one ordinary page pump.
const PAGE_TURN_BUDGET: usize = 64;
/// Turns drained while executing one prepared classic-script batch.
const SCRIPT_BATCH_TURN_BUDGET: usize = 1_024;
/// Wall-clock slice for draining one prepared classic-script batch.
const SCRIPT_BATCH_TIME_BUDGET: Duration = Duration::from_millis(500);
/// A pending timer within this horizon is allowed to fire before the final
/// frame. Longer timers do not keep a settled page alive: the screenshot is a
/// snapshot, not a recording session.
const TIMER_HORIZON: Duration = Duration::from_secs(1);
/// Upper bound on source bytes accepted for one page transfer.
const MAX_PAGE_BYTES: usize = 32 * 1024 * 1024;

/// Inputs for one offscreen page load.
#[derive(Clone, Debug)]
pub struct HeadlessOptions {
    /// Page URL or local `file:` URL.
    pub url: Url,
    /// Layout viewport width in CSS pixels.
    pub viewport_width: u32,
    /// Layout viewport height in CSS pixels.
    pub viewport_height: u32,
    /// Device pixel ratio used by the paint pass.
    pub device_scale: f32,
    /// Maximum wall-clock time spent loading and settling.
    pub wait: Duration,
    /// Explicit HTTP proxy (`host:port` or `http://host:port`), or `direct`.
    pub proxy: Option<String>,
    /// Retain the serialized DOM in the report.
    pub dump_dom: bool,
}

impl Default for HeadlessOptions {
    fn default() -> Self {
        Self {
            url: Url::parse("about:blank").expect("about:blank is a valid URL"),
            viewport_width: 1_180,
            viewport_height: 780,
            device_scale: 1.0,
            wait: Duration::from_secs(10),
            proxy: None,
            dump_dom: false,
        }
    }
}

/// One element's border box in CSS pixels, for geometry JSON.
#[derive(Clone, Debug)]
pub struct HeadlessGeometry {
    pub node: u64,
    pub tag: String,
    pub id: Option<String>,
    pub class: Option<String>,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Counters and dimensions for one completed run.
#[derive(Clone, Debug)]
pub struct HeadlessHealth {
    pub url: Url,
    pub final_url: Url,
    pub ready_state: String,
    pub viewport_width: u32,
    pub viewport_height: u32,
    pub device_scale: f32,
    pub surface_width: u32,
    pub surface_height: u32,
    pub dom_elements: usize,
    pub boxes: usize,
    pub display_items: usize,
    pub stylesheets_loaded: usize,
    pub css_errors: usize,
    pub css_warnings: usize,
    pub scripts_queued: usize,
    pub scripts_executed: usize,
    pub scripts_failed: usize,
    pub scripts_threw: usize,
    pub images_loaded: usize,
    pub images_failed: usize,
    pub fonts_loaded: usize,
    pub font_failures: usize,
    pub page_fetches: usize,
    pub elapsed_ms: u64,
    pub timed_out: bool,
}

/// A completed offscreen run.
pub struct HeadlessReport {
    pub health: HeadlessHealth,
    /// Final composited raster, already scaled by the device pixel ratio.
    pub surface: Surface,
    pub geometry: Vec<HeadlessGeometry>,
    /// Serialized DOM when [`HeadlessOptions::dump_dom`] was set.
    pub dom_html: Option<String>,
}

impl HeadlessReport {
    /// Renders the health and geometry counters as a single JSON object.
    #[must_use]
    pub fn health_json(&self) -> String {
        use std::fmt::Write as _;
        let health = &self.health;
        let mut json = String::new();
        let _ = write!(
            json,
            "{{\"url\":{},\"final_url\":{},\"ready_state\":{},\
             \"viewport\":{{\"width\":{},\"height\":{},\"dpr\":{}}},\
             \"surface\":{{\"width\":{},\"height\":{}}},\
             \"dom_elements\":{},\"boxes\":{},\"display_items\":{},\
             \"stylesheets_loaded\":{},\"css_errors\":{},\"css_warnings\":{},\
             \"scripts_queued\":{},\"scripts_executed\":{},\"scripts_failed\":{},\"scripts_threw\":{},\
             \"images_loaded\":{},\"images_failed\":{},\
             \"fonts_loaded\":{},\"font_failures\":{},\"page_fetches\":{},\
             \"elapsed_ms\":{},\"timed_out\":{},\"geometry\":[",
            json_string(health.url.as_str()),
            json_string(health.final_url.as_str()),
            json_string(&health.ready_state),
            health.viewport_width,
            health.viewport_height,
            health.device_scale,
            health.surface_width,
            health.surface_height,
            health.dom_elements,
            health.boxes,
            health.display_items,
            health.stylesheets_loaded,
            health.css_errors,
            health.css_warnings,
            health.scripts_queued,
            health.scripts_executed,
            health.scripts_failed,
            health.scripts_threw,
            health.images_loaded,
            health.images_failed,
            health.fonts_loaded,
            health.font_failures,
            health.page_fetches,
            health.elapsed_ms,
            health.timed_out,
        );
        for (index, item) in self.geometry.iter().enumerate() {
            if index > 0 {
                json.push(',');
            }
            let _ = write!(
                json,
                "{{\"node\":{},\"tag\":{},\"id\":{},\"class\":{},\
                 \"x\":{:.3},\"y\":{:.3},\"width\":{:.3},\"height\":{:.3}}}",
                item.node,
                json_string(&item.tag),
                item.id
                    .as_deref()
                    .map_or_else(|| "null".to_owned(), json_string),
                item.class
                    .as_deref()
                    .map_or_else(|| "null".to_owned(), json_string),
                item.x,
                item.y,
                item.width,
                item.height,
            );
        }
        json.push_str("]}");
        json
    }
}

/// Runs one page to settlement and returns its offscreen raster.
///
/// # Errors
///
/// Returns a human-readable error for an unsupported URL scheme, a navigation
/// transfer failure, or a document that cannot be decoded as HTML.
pub fn run_headless(options: HeadlessOptions) -> Result<HeadlessReport, String> {
    HeadlessSession::new(options)?.run()
}

/// Encodes a raster surface as an RGBA PNG.
///
/// # Errors
///
/// Returns the I/O or encoder error.
pub fn write_surface_png(surface: &Surface, path: &Path) -> io::Result<()> {
    use image_codec::ImageEncoder as _;
    let mut rgba = Vec::with_capacity(surface.pixels().len() * 4);
    for pixel in surface.pixels() {
        rgba.extend_from_slice(&[pixel.red, pixel.green, pixel.blue, pixel.alpha]);
    }
    image_codec::codecs::png::PngEncoder::new(fs::File::create(path)?)
        .write_image(
            &rgba,
            surface.width(),
            surface.height(),
            image_codec::ExtendedColorType::Rgba8,
        )
        .map_err(|error| io::Error::other(error.to_string()))
}

struct HeadlessSession {
    options: HeadlessOptions,
    page: Page,
    base_url: Url,
    transport: HttpTransport,
    cookies: CookieJar,
    style_sheets: ExternalStyleSheets,
    attempted_styles: HashSet<ExternalStyleSheetKey>,
    images: ImageResources,
    attempted_images: HashSet<ImageResourceKey>,
    started_scripts: HashSet<NodeId>,
    import_map: ImportMap,
    fonts: Arc<SystemFontBackend>,
    font_loader: DocumentFontLoader,
    attempted_fonts: HashSet<Url>,
    transitions: TransitionState,
    last_render: Option<DocumentRenderOutput>,
    computed_styles: BTreeMap<NodeId, ComputedStyle>,
    last_geometry: BTreeMap<u64, ElementRect>,
    rendered_revision: Option<u64>,
    needs_render: bool,
    initial_script_scan_done: bool,
    dom_content_loaded: bool,
    load_fired: bool,
    styles_pending: bool,
    scripts_pending: bool,
    images_pending: bool,
    fonts_pending: bool,
    stylesheets_loaded: usize,
    css_errors: usize,
    css_warnings: usize,
    scripts_queued: usize,
    scripts_executed: usize,
    script_load_errors: usize,
    script_runtime_errors: usize,
    images_loaded: usize,
    image_errors: usize,
    fonts_loaded: usize,
    font_failures: usize,
    page_fetches: usize,
    created_at: Instant,
    deadline: Instant,
}

impl HeadlessSession {
    fn new(options: HeadlessOptions) -> Result<Self, String> {
        if options.viewport_width == 0 || options.viewport_height == 0 {
            return Err("viewport dimensions must be non-zero".to_owned());
        }
        if !options.device_scale.is_finite() || options.device_scale <= 0.0 {
            return Err("device scale must be a positive finite number".to_owned());
        }
        let transport = build_transport(options.proxy.as_deref())?;
        let mut cookies = CookieJar::default();
        let (html, final_url) = load_initial_document(&transport, &mut cookies, &options.url)?;
        let fonts = Arc::new(
            SystemFontBackend::load()
                .map_err(|error| format!("could not load the system font backend: {error}"))?,
        );
        let render_options = DocumentRenderOptions {
            layout: LayoutOptions {
                viewport: PhysicalSize {
                    width: options.viewport_width as f32,
                    height: options.viewport_height as f32,
                },
                ..LayoutOptions::default()
            },
            device_scale: options.device_scale,
            ..DocumentRenderOptions::default()
        };
        let mut page = Page::with_options_and_url_unrendered(
            &html,
            PageOptions {
                runtime_limits: runtime_limits(),
                render: render_options,
                ..PageOptions::default()
            },
            &final_url,
        );
        // Scripts must see the same geometry the renderer uses. Without this
        // the runtime defaults to 1024x768/DPR 1, so responsive pages sniff a
        // tablet viewport and choose the wrong markup and image variants.
        page.runtime_mut().install_viewport(
            options.viewport_width as f32,
            options.viewport_height as f32,
            0.0,
            0.0,
        );
        page.runtime_mut()
            .install_device_pixel_ratio(f64::from(options.device_scale));
        let now = Instant::now();
        let wait = options.wait;
        Ok(Self {
            options,
            page,
            base_url: final_url,
            transport,
            cookies,
            style_sheets: ExternalStyleSheets::default(),
            attempted_styles: HashSet::new(),
            images: ImageResources::default(),
            attempted_images: HashSet::new(),
            started_scripts: HashSet::new(),
            import_map: ImportMap::default(),
            fonts,
            font_loader: DocumentFontLoader::new(),
            attempted_fonts: HashSet::new(),
            transitions: TransitionState::default(),
            last_render: None,
            computed_styles: BTreeMap::new(),
            last_geometry: BTreeMap::new(),
            rendered_revision: None,
            needs_render: true,
            initial_script_scan_done: false,
            dom_content_loaded: false,
            load_fired: false,
            styles_pending: true,
            scripts_pending: true,
            images_pending: true,
            fonts_pending: true,
            stylesheets_loaded: 0,
            css_errors: 0,
            css_warnings: 0,
            scripts_queued: 0,
            scripts_executed: 0,
            script_load_errors: 0,
            script_runtime_errors: 0,
            images_loaded: 0,
            image_errors: 0,
            fonts_loaded: 0,
            font_failures: 0,
            page_fetches: 0,
            created_at: now,
            deadline: now + wait,
        })
    }

    fn run(mut self) -> Result<HeadlessReport, String> {
        let mut timed_out = false;
        for _ in 0..MAX_SETTLE_CYCLES {
            if self.expired() {
                timed_out = true;
                break;
            }
            let mut activity = false;
            activity |= self.apply_stylesheets();
            if self.expired() {
                timed_out = true;
                break;
            }
            activity |= self.apply_scripts()?;
            if self.expired() {
                timed_out = true;
                break;
            }
            activity |= self.service_page_fetches();
            activity |= self.service_dynamic_imports();
            activity |= self.run_page_turns(PAGE_TURN_BUDGET);
            // A fetch resolved by `settle_fetch` can itself queue another one.
            activity |= self.service_page_fetches();
            if self.expired() {
                timed_out = true;
                break;
            }

            if self.needs_render || self.last_render.is_none() {
                self.render()?;
                activity = true;
            }
            let output = self.last_render.take();
            if let Some(output) = output.as_ref() {
                activity |= self.apply_fonts(output);
                activity |= self.apply_images(Some(output));
            }
            self.last_render = output;
            activity |= self.advance_ready_state();
            if activity {
                continue;
            }

            let now = Instant::now();
            if now >= self.deadline {
                timed_out = true;
                break;
            }
            // A timer due shortly is part of the page's settle path (a
            // deferred bootstrap, an image swap). A timer beyond the horizon
            // belongs to a recording, not to this snapshot.
            let elapsed = self.created_at.elapsed();
            if let Some(next) = self.page.next_wake_deadline()
                && next >= elapsed
            {
                let delay = next.saturating_sub(elapsed);
                let remaining = self.deadline.saturating_duration_since(now);
                if delay <= TIMER_HORIZON.min(remaining) {
                    std::thread::sleep(delay);
                    continue;
                }
            }
            break;
        }

        // Drain work the final render itself queued (geometry-driven
        // microtasks, image load events) without starting a new page timer.
        if !timed_out {
            for _ in 0..8 {
                if !self.run_page_turns(8) {
                    break;
                }
            }
        }
        let current_revision = self.page.document().dom().revision().as_u64();
        if self.needs_render || self.rendered_revision != Some(current_revision) {
            self.render()?;
        }
        self.advance_ready_state();
        if !timed_out {
            for _ in 0..4 {
                if !self.run_page_turns(8) {
                    break;
                }
            }
        }
        if self.rendered_revision != Some(self.page.document().dom().revision().as_u64()) {
            self.render()?;
        }

        let output = self
            .last_render
            .take()
            .ok_or_else(|| "the page produced no render output".to_owned())?;
        let elapsed = self.created_at.elapsed();
        let health = self.health(&output, elapsed, timed_out);
        let dom_html = self.options.dump_dom.then(|| {
            let dom = self.page.document().dom();
            render_core::html::serialize_html_node(dom, dom.document())
        });
        Ok(HeadlessReport {
            health,
            surface: output.raster.surface,
            geometry: self.geometry_json(),
            dom_html,
        })
    }

    fn expired(&self) -> bool {
        Instant::now() >= self.deadline
    }

    fn apply_stylesheets(&mut self) -> bool {
        let plan = plan_external_style_sheets(
            self.page.document(),
            &self.base_url,
            DocumentLimits::default(),
        );
        let current: HashSet<ExternalStyleSheetKey> =
            plan.resources.iter().map(|r| r.key.clone()).collect();
        self.attempted_styles.retain(|key| current.contains(key));

        let mut fresh = Vec::new();
        for resource in &plan.resources {
            if !self.attempted_styles.contains(&resource.key) {
                fresh.push(resource.clone());
                self.attempted_styles.insert(resource.key.clone());
            }
        }
        let mut activity = false;
        if !fresh.is_empty() {
            let requests = fresh.iter().map(|r| r.request.clone()).collect();
            let results = self.fetch_batch(requests);
            let subset = StylesheetFetchPlan {
                revision: plan.revision,
                resources: fresh,
                diagnostics: Vec::new(),
            };
            let application = apply_stylesheet_batch(self.page.document(), &subset, results);
            self.count_stylesheet_diagnostics(&application.diagnostics);
            self.stylesheets_loaded += application.loaded.len();
            for loaded in &application.loaded {
                let _ = self
                    .page
                    .queue_dom_event(PageDomEvent::new(loaded.key.owner, "load"));
            }
            for key in &application.failed {
                let _ = self
                    .page
                    .queue_dom_event(PageDomEvent::new(key.owner, "error"));
            }
            self.merge_stylesheets(&plan, &application.style_sheets);
            self.needs_render = true;
            activity = true;
        } else if !self.style_sheets.is_empty() {
            // A removed or retargeted <link> must also leave the external map.
            self.merge_stylesheets(&plan, &ExternalStyleSheets::default());
        }
        self.styles_pending = false;
        activity
    }

    fn merge_stylesheets(&mut self, plan: &StylesheetFetchPlan, incoming: &ExternalStyleSheets) {
        let mut merged = ExternalStyleSheets::default();
        for resource in &plan.resources {
            if !merged.insert_cloned_from(resource.key.clone(), incoming) {
                let _ = merged.insert_cloned_from(resource.key.clone(), &self.style_sheets);
            }
        }
        self.style_sheets = merged;
    }

    fn apply_scripts(&mut self) -> Result<bool, String> {
        let plan = if self.initial_script_scan_done {
            plan_unstarted_classic_scripts(
                self.page.document(),
                &self.base_url,
                ScriptDiscoveryLimits::default(),
                &self.started_scripts,
                true,
            )
        } else {
            self.initial_script_scan_done = true;
            plan_classic_scripts(
                self.page.document(),
                &self.base_url,
                ScriptDiscoveryLimits::default(),
            )
        };
        self.import_map = plan.import_map().clone();
        let owners: Vec<NodeId> = plan.owners().collect();
        if owners.is_empty() && plan.resources.is_empty() {
            self.scripts_pending = false;
            return Ok(false);
        }
        for owner in &owners {
            self.started_scripts.insert(*owner);
        }
        let requests = plan.requests();
        let results = self.fetch_batch(requests);
        let mut preparation =
            prepare_script_batch(self.page.document(), &plan, results, &runtime_limits());
        self.fetch_module_dependencies(&mut preparation)?;
        preparation.finish_modules();
        self.count_script_diagnostics(&preparation.diagnostics);
        let origins = sorted_script_origins(&preparation);
        self.scripts_pending = false;
        if preparation.scripts.is_empty() {
            return Ok(!owners.is_empty());
        }
        self.scripts_queued += preparation.scripts.len();
        let queue = self.page.queue_prepared_scripts(
            preparation.revision,
            preparation.scripts.into_iter().map(Into::into).collect(),
        );
        for error in &queue.errors {
            self.script_load_errors = self.script_load_errors.saturating_add(1);
            eprintln!("render-headless: could not queue script: {error}");
        }
        let failed: HashSet<(NodeId, usize)> = queue
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
            .collect();
        let task_origins: HashMap<_, _> = queue
            .queued
            .iter()
            .copied()
            .zip(
                origins
                    .into_iter()
                    .filter(|(source_order, owner, _)| !failed.contains(&(*owner, *source_order))),
            )
            .map(|(task, (source_order, owner, final_url))| {
                (task, (owner, source_order, final_url))
            })
            .collect();
        self.run_script_batch(&task_origins);
        self.needs_render = true;
        Ok(true)
    }

    fn fetch_module_dependencies(
        &mut self,
        preparation: &mut ScriptBatchPreparation,
    ) -> Result<(), String> {
        loop {
            let pending = preparation.pending_module_requests();
            if pending.is_empty() {
                return Ok(());
            }
            let results = self.fetch_batch(pending.clone());
            preparation.absorb_module_results(&pending, results, &runtime_limits());
        }
    }

    fn run_script_batch(&mut self, task_origins: &HashMap<TaskId, ScriptOrigin>) {
        let started = Instant::now();
        for _ in 0..SCRIPT_BATCH_TURN_BUDGET {
            if started.elapsed() >= SCRIPT_BATCH_TIME_BUDGET || self.expired() {
                break;
            }
            match self.page.run_one_turn_without_render() {
                Ok(Some(turn)) => {
                    let mut origin = None;
                    for execution in turn.executions {
                        if let PageJob::Task { id, .. } = execution.job {
                            origin = task_origins.get(&id).cloned();
                        }
                        if let Some((owner, _, Some(_))) = &origin {
                            self.scripts_executed = self.scripts_executed.saturating_add(1);
                            let _ = self.page.queue_dom_event(PageDomEvent::new(*owner, "load"));
                        }
                        if let Err(error) = execution.result {
                            self.script_runtime_errors =
                                self.script_runtime_errors.saturating_add(1);
                            eprintln!("render-headless: script failed: {}", error.message());
                        }
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    eprintln!("render-headless: page turn failed: {error}");
                    break;
                }
            }
        }
        self.note_page_change();
    }

    fn service_page_fetches(&mut self) -> bool {
        let mut activity = false;
        loop {
            let pending = self.page.take_pending_fetch_requests();
            if pending.is_empty() {
                break;
            }
            activity = true;
            for request in pending {
                let outcome = self.execute_page_fetch(&request);
                if let Err(error) = self.page.settle_fetch(request.id, outcome) {
                    self.script_runtime_errors = self.script_runtime_errors.saturating_add(1);
                    eprintln!(
                        "render-headless: fetch settlement callback failed: {}",
                        error.message()
                    );
                }
                self.page_fetches = self.page_fetches.saturating_add(1);
            }
        }
        self.page.drain_pending_microtasks();
        self.note_page_change();
        activity
    }

    fn execute_page_fetch(
        &mut self,
        request: &render_core::js::PendingFetch,
    ) -> Result<FetchOutcome, String> {
        let target = Url::parse(&request.url)
            .map_err(|error| format!("fetch URL {:?} is invalid: {error}", request.url))?;
        let method = HttpMethod::from_wire(&request.method)
            .ok_or_else(|| format!("unsupported fetch method {:?}", request.method))?;
        let mut outgoing = FetchRequest::new(method, target.clone()).with_accept("*/*");
        for (name, value) in &request.headers {
            if !is_forbidden_request_header(name) {
                outgoing = outgoing.with_header(name.clone(), value.clone());
            }
        }
        if let Some(body) = &request.body {
            outgoing = outgoing.with_body(body.clone().into_bytes());
        }
        let same_origin = self.base_url.origin() == target.origin();
        let send_cookies = match request.credentials {
            FetchCredentials::Omit => false,
            FetchCredentials::SameOrigin => same_origin,
            FetchCredentials::Include => true,
        };
        if send_cookies {
            outgoing = self.cookies.decorate_request(outgoing);
        }
        let response = self
            .transport
            .fetch(&outgoing, &CancelToken::default())
            .map_err(|error| error.to_string())?;
        if send_cookies {
            let _ = self.cookies.absorb_response(&response);
        }
        let cross_origin = !same_origin;
        if cross_origin && !cors_response_allows(&response.headers, &self.base_url) {
            return Err(BLOCKED_BY_CORS.to_owned());
        }
        Ok(FetchOutcome {
            status: response.status.as_u16(),
            status_text: String::new(),
            headers: exposed_response_headers(&response.headers, cross_origin),
            body: response.body,
        })
    }

    fn service_dynamic_imports(&mut self) -> bool {
        let loads = self.page.take_pending_module_loads();
        if loads.is_empty() {
            return false;
        }
        for load in loads {
            let referrer = Url::parse(&load.referrer).unwrap_or_else(|_| self.base_url.clone());
            let Ok(target) = self.import_map.resolve(&referrer, &load.specifier) else {
                self.page.reject_dynamic_import(
                    load.id,
                    format!(
                        "import specifier {:?} cannot be resolved from {}",
                        load.specifier, load.referrer
                    ),
                );
                continue;
            };
            let entry_key = module_key(&target);
            match self.load_module_graph(target) {
                Ok(graph) => {
                    let units = graph.entry(&entry_key).map(|entry| {
                        entry
                            .units
                            .into_iter()
                            .map(|unit| DynamicModuleUnit {
                                key: unit.key,
                                compiled: unit.compiled,
                                resolutions: unit.resolutions,
                            })
                            .collect::<Vec<_>>()
                    });
                    match units {
                        Some(units) => {
                            if let Err(error) = self
                                .page
                                .resolve_dynamic_import(load.id, &entry_key, &units)
                            {
                                self.page.reject_dynamic_import(load.id, error.to_string());
                            }
                        }
                        None => self.page.reject_dynamic_import(
                            load.id,
                            "the imported module could not be compiled",
                        ),
                    }
                }
                Err(error) => {
                    self.page.reject_dynamic_import(load.id, error);
                }
            }
            self.page.drain_pending_microtasks();
        }
        self.note_page_change();
        true
    }

    fn load_module_graph(&mut self, root: Url) -> Result<ModuleGraph, String> {
        let mut graph = ModuleGraph::new();
        graph.set_import_map(self.import_map.clone());
        let mut requests = vec![FetchRequest::get(root).with_accept(SCRIPT_ACCEPT)];
        loop {
            let results = self.fetch_batch(requests.clone());
            for (request, result) in requests.iter().zip(results) {
                graph.complete(&request.url, module_outcome(result), &runtime_limits());
            }
            let pending = graph.take_pending();
            if pending.is_empty() {
                let diagnostics = graph.take_diagnostics();
                if let Some(message) = diagnostics.into_iter().next() {
                    return Err(message);
                }
                return Ok(graph);
            }
            requests = pending
                .into_iter()
                .map(|url| FetchRequest::get(url).with_accept(SCRIPT_ACCEPT))
                .collect();
        }
    }

    fn apply_fonts(&mut self, output: &DocumentRenderOutput) -> bool {
        self.fonts_pending = false;
        if !self.font_loader.has_pending_network() {
            return false;
        }
        let families = used_font_families(&output.styles);
        let characters = self
            .font_loader
            .document_characters((1, 1), self.page.document().dom());
        let urls = self
            .font_loader
            .plan_fetches(families.iter().map(String::as_str), &characters);
        let fresh: Vec<Url> = urls
            .into_iter()
            .filter(|url| !self.attempted_fonts.contains(url))
            .collect();
        if fresh.is_empty() {
            return false;
        }
        for url in &fresh {
            self.attempted_fonts.insert(url.clone());
        }
        let plan = FontFetchPlan::new(fresh);
        let results = self.fetch_batch(plan.requests());
        let batch = apply_font_batch(&plan, results);
        for failure in &batch.failures {
            self.font_failures = self.font_failures.saturating_add(1);
            eprintln!(
                "render-headless: webfont {} failed: {}",
                failure.url, failure.message
            );
        }
        self.fonts_loaded += batch.resources.len();
        self.font_loader.install_fetched(&batch.resources);
        self.needs_render = true;
        !batch.resources.is_empty() || !batch.failures.is_empty()
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one batch tracks planning, per-element load state, decode results and error events in order"
    )]
    fn apply_images(&mut self, output: Option<&DocumentRenderOutput>) -> bool {
        self.images_pending = false;
        let context = ImageSelectionContext {
            viewport_width: self.options.viewport_width.max(1),
            viewport_height: self.options.viewport_height.max(1),
            device_pixel_ratio_milli: (self.options.device_scale.max(0.001) * 1_000.0).round()
                as u32,
        };
        let styles = output.map_or(&self.computed_styles, |output| &output.styles);
        let plan = plan_images_with_styles_and_context(
            self.page.document(),
            styles,
            &self.base_url,
            &self.images,
            ImageLimits::default(),
            context,
        );
        self.attempted_images
            .retain(|key| plan.resources.iter().any(|resource| &resource.key == key));
        let fresh: Vec<_> = plan
            .resources
            .iter()
            .filter(|resource| !self.attempted_images.contains(&resource.key))
            .cloned()
            .collect();
        if fresh.is_empty() {
            return false;
        }
        for resource in &fresh {
            self.attempted_images.insert(resource.key.clone());
            if resource.key.source == ImageSource::Element {
                self.page.document_mut().dom_mut().set_image_state(
                    resource.key.owner,
                    render_core::dom::ImageLoadState::Pending,
                );
                self.page.remove_canvas_image(resource.key.owner);
            }
        }
        let requests = fresh
            .iter()
            .map(|resource| resource.request.clone())
            .collect();
        let results = self.fetch_batch(requests);
        let subset = ImageFetchPlan {
            revision: plan.revision,
            document_url: plan.document_url.clone(),
            resources: fresh,
            diagnostics: Vec::new(),
        };
        let application = apply_image_batch(
            self.page.document(),
            &subset,
            results,
            &mut self.images,
            ImageLimits::default(),
        );
        self.images_loaded += application.loaded.len();
        for loaded in &application.loaded {
            if loaded.source == ImageSource::Element {
                self.page.document_mut().dom_mut().set_image_state(
                    loaded.owner,
                    render_core::dom::ImageLoadState::Loaded {
                        width: loaded.width,
                        height: loaded.height,
                    },
                );
                if let Some(image) = self.images.get_for_node(loaded.owner) {
                    self.page.install_canvas_image(loaded.owner, &image.image);
                }
            }
            if matches!(
                loaded.source,
                ImageSource::Element | ImageSource::VideoPoster
            ) {
                let _ = self
                    .page
                    .queue_dom_event(PageDomEvent::new(loaded.owner, "load"));
            }
        }
        let element_owners: Vec<NodeId> = subset
            .resources
            .iter()
            .filter(|resource| resource.key.source == ImageSource::Element)
            .map(|resource| resource.key.owner)
            .collect();
        for diagnostic in &application.diagnostics {
            if diagnostic.severity != crate::images::ImageDiagnosticSeverity::Error
                || diagnostic.code == ImageResourceDiagnosticCode::StaleSource
            {
                continue;
            }
            self.image_errors = self.image_errors.saturating_add(1);
            let Some(owner) = diagnostic.owner else {
                continue;
            };
            if !element_owners.contains(&owner) {
                continue;
            }
            self.page
                .document_mut()
                .dom_mut()
                .set_image_state(owner, render_core::dom::ImageLoadState::Broken);
            self.page.remove_canvas_image(owner);
            let _ = self.page.queue_dom_event(PageDomEvent::new(owner, "error"));
        }
        self.needs_render = true;
        !subset.resources.is_empty()
    }

    fn advance_ready_state(&mut self) -> bool {
        let state = self.page.runtime().document_ready_state();
        if state == DocumentReadyState::Loading {
            if !self.initial_script_scan_done
                || self.styles_pending
                || self.scripts_pending
                || self.page.has_pending_immediate_work()
                || !self.page.pending_fetch_queue_empty()
                || !self.page.pending_module_load_queue_empty()
            {
                return false;
            }
            self.page
                .runtime_mut()
                .set_document_ready_state(DocumentReadyState::Interactive);
            let target = self.page.document().dom().document();
            let _ = self
                .page
                .queue_dom_event(PageDomEvent::new(target, "DOMContentLoaded").bubbles(true));
            self.dom_content_loaded = true;
            return true;
        }
        if state == DocumentReadyState::Interactive && !self.load_fired {
            if !self.dom_content_loaded
                || self.styles_pending
                || self.scripts_pending
                || self.images_pending
                || self.fonts_pending
                || self.page.has_pending_immediate_work()
                || !self.page.pending_fetch_queue_empty()
                || !self.page.pending_module_load_queue_empty()
            {
                return false;
            }
            self.page
                .runtime_mut()
                .set_document_ready_state(DocumentReadyState::Complete);
            let target = self.page.document().dom().document();
            let _ = self.page.queue_dom_event(PageDomEvent::new(target, "load"));
            self.load_fired = true;
            return true;
        }
        false
    }

    fn render(&mut self) -> Result<(), String> {
        let (_, canvas_images) = self.page.publish_render_resources();
        let mut render_images = self.images.clone();
        render_images
            .overlay(&canvas_images, ImageLimits::default())
            .map_err(|error| format!("could not publish canvas pixels: {error}"))?;
        let frame_documents = self.page.runtime().frame_documents();
        let options = DocumentRenderOptions {
            layout: LayoutOptions {
                viewport: PhysicalSize {
                    width: self.options.viewport_width as f32,
                    height: self.options.viewport_height as f32,
                },
                ..LayoutOptions::default()
            },
            device_scale: self.options.device_scale,
            animation_time: self.created_at.elapsed().as_secs_f32(),
            ..DocumentRenderOptions::default()
        };
        self.font_loader.begin_document((1, 1));
        let installer = SystemFontFaceInstaller {
            backend: &self.fonts,
            loader: &self.font_loader,
        };
        let output = {
            let backends = DocumentBackends {
                text_measurer: self.fonts.as_ref(),
                text_shaper: self.fonts.as_ref(),
                glyph_masks: self.fonts.as_ref(),
                font_face_installer: Some(&installer),
            };
            self.page
                .document()
                .render_with_interaction_and_transitions_and_frames(
                    options,
                    backends,
                    &self.base_url,
                    &self.style_sheets,
                    &render_images,
                    &InteractionState::default(),
                    &mut self.transitions,
                    &frame_documents,
                )
        };
        let revision = output.revision.as_u64();
        let geometry = geometry_from_layout(&output.layout.fragments);
        let _ = self
            .page
            .publish_render_geometry(revision, geometry.clone());
        let _ = self.page.publish_computed_styles(revision, &output.styles);
        self.page.queue_pending_runtime_microtasks();
        self.computed_styles = output.styles.clone();
        self.last_geometry = geometry;
        self.rendered_revision = Some(revision);
        self.last_render = Some(output);
        self.needs_render = false;
        Ok(())
    }

    fn run_page_turns(&mut self, budget: usize) -> bool {
        let mut activity = false;
        for _ in 0..budget {
            if self.expired() {
                break;
            }
            match self.page.run_one_turn_without_render() {
                Ok(Some(turn)) => {
                    activity = true;
                    for execution in turn.executions {
                        if let Err(error) = execution.result {
                            self.script_runtime_errors =
                                self.script_runtime_errors.saturating_add(1);
                            eprintln!("render-headless: script task failed: {}", error.message());
                        }
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    eprintln!("render-headless: page turn failed: {error}");
                    break;
                }
            }
        }
        if self.page.publish_canvas_changes() {
            activity = true;
        }
        self.page.drain_pending_microtasks();
        if self.note_page_change() {
            activity = true;
        }
        activity
    }

    /// Marks a render dirty when the DOM moved past the last committed frame.
    fn note_page_change(&mut self) -> bool {
        let revision = self.page.document().dom().revision().as_u64();
        if self
            .rendered_revision
            .is_some_and(|rendered| rendered != revision)
        {
            self.needs_render = true;
            return true;
        }
        false
    }

    fn fetch_batch(&mut self, requests: Vec<FetchRequest>) -> Vec<FetchResult> {
        if requests.is_empty() {
            return Vec::new();
        }
        let requests = requests
            .into_iter()
            .map(|request| self.cookies.decorate_request(request))
            .collect();
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        let timeout = remaining
            .max(Duration::from_millis(250))
            .min(Duration::from_secs(30));
        let results = self.transport.fetch_batch(
            requests,
            &BatchOptions {
                timeout,
                ..BatchOptions::default()
            },
            &CancelToken::default(),
        );
        for response in results.iter().flatten() {
            let _ = self.cookies.absorb_response(response);
        }
        results
    }

    fn count_stylesheet_diagnostics(&mut self, diagnostics: &[StylesheetResourceDiagnostic]) {
        for diagnostic in diagnostics {
            match diagnostic.severity {
                crate::resources::StylesheetDiagnosticSeverity::Error => {
                    self.css_errors = self.css_errors.saturating_add(1);
                }
                crate::resources::StylesheetDiagnosticSeverity::Warning => {
                    self.css_warnings = self.css_warnings.saturating_add(1);
                }
            }
        }
    }

    fn count_script_diagnostics(&mut self, diagnostics: &[ScriptResourceDiagnostic]) {
        for diagnostic in diagnostics {
            if diagnostic.severity != ScriptResourceSeverity::Error {
                continue;
            }
            self.script_load_errors = self.script_load_errors.saturating_add(1);
            if let Some(owner) = diagnostic.owner {
                let _ = self.page.queue_dom_event(PageDomEvent::new(owner, "error"));
            }
        }
    }

    fn health(
        &self,
        output: &DocumentRenderOutput,
        elapsed: Duration,
        timed_out: bool,
    ) -> HeadlessHealth {
        let dom = self.page.document().dom();
        let mut elements = 0usize;
        let mut stack = vec![dom.document()];
        while let Some(node) = stack.pop() {
            if matches!(
                dom.node(node).map(render_core::dom::Node::kind),
                Some(NodeKind::Element(_))
            ) {
                elements = elements.saturating_add(1);
            }
            if let Some(children) = dom.children(node) {
                stack.extend(children.iter().copied());
            }
        }
        HeadlessHealth {
            url: self.options.url.clone(),
            final_url: self.base_url.clone(),
            ready_state: self
                .page
                .runtime()
                .document_ready_state()
                .as_str()
                .to_owned(),
            viewport_width: self.options.viewport_width,
            viewport_height: self.options.viewport_height,
            device_scale: self.options.device_scale,
            surface_width: output.raster.surface.width(),
            surface_height: output.raster.surface.height(),
            dom_elements: elements,
            boxes: self.last_geometry.len(),
            display_items: output.display.list.items().len(),
            stylesheets_loaded: self.stylesheets_loaded,
            css_errors: self.css_errors,
            css_warnings: self.css_warnings,
            scripts_queued: self.scripts_queued,
            scripts_executed: self.scripts_executed,
            scripts_failed: self.script_load_errors,
            scripts_threw: self.script_runtime_errors,
            images_loaded: self.images_loaded,
            images_failed: self.image_errors,
            fonts_loaded: self.fonts_loaded,
            font_failures: self.font_failures,
            page_fetches: self.page_fetches,
            elapsed_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
            timed_out,
        }
    }

    fn geometry_json(&self) -> Vec<HeadlessGeometry> {
        const MAX_GEOMETRY_ENTRIES: usize = 2_000;
        let dom = self.page.document().dom();
        self.last_geometry
            .iter()
            .take(MAX_GEOMETRY_ENTRIES)
            .filter_map(|(node, rect)| {
                let node_id = NodeId::from_u64(*node);
                let candidate = dom.node(node_id)?;
                let NodeKind::Element(element) = candidate.kind() else {
                    return None;
                };
                Some(HeadlessGeometry {
                    node: *node,
                    tag: element.local_name.clone(),
                    id: dom
                        .attribute(node_id, "id")
                        .ok()
                        .flatten()
                        .map(str::to_owned),
                    class: dom
                        .attribute(node_id, "class")
                        .ok()
                        .flatten()
                        .map(str::to_owned),
                    x: rect.x,
                    y: rect.y,
                    width: rect.width,
                    height: rect.height,
                })
            })
            .collect()
    }
}

type ScriptOrigin = (NodeId, usize, Option<Url>);

fn sorted_script_origins(
    preparation: &ScriptBatchPreparation,
) -> Vec<(usize, NodeId, Option<Url>)> {
    use render_core::script::ScriptScheduling;
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
    origins
        .into_iter()
        .map(|(_, source_order, _, owner, final_url)| (source_order, owner, final_url))
        .collect()
}

fn used_font_families(styles: &BTreeMap<NodeId, ComputedStyle>) -> Vec<String> {
    styles
        .values()
        .filter_map(|style| {
            style
                .get("font-family")
                .map(|value| value.css_text().to_owned())
        })
        .collect()
}

fn geometry_from_layout(
    fragments: &render_core::layout::FragmentTree,
) -> BTreeMap<u64, ElementRect> {
    let mut geometry = BTreeMap::new();
    for fragment in fragments.iter() {
        let render_core::layout::FragmentKind::Box(box_geometry) = &fragment.kind else {
            continue;
        };
        let Some(source) = fragment.source else {
            continue;
        };
        let rect = box_geometry.border_rect();
        geometry.entry(source.as_u64()).or_insert(ElementRect {
            x: rect.origin.x,
            y: rect.origin.y,
            width: rect.size.width,
            height: rect.size.height,
        });
    }
    geometry
}

fn build_transport(proxy: Option<&str>) -> Result<HttpTransport, String> {
    match proxy {
        None => Ok(HttpTransport::new(FetchConfig::default())),
        Some(value) if value.eq_ignore_ascii_case("direct") => {
            Ok(HttpTransport::with_http_proxy(FetchConfig::default(), None))
        }
        Some(value) => {
            let proxy = HttpProxyConfig::parse(value)
                .map_err(|error| format!("invalid --proxy value: {error}"))?;
            Ok(HttpTransport::with_http_proxy(
                FetchConfig::default(),
                Some(proxy),
            ))
        }
    }
}

fn load_initial_document(
    transport: &HttpTransport,
    cookies: &mut CookieJar,
    url: &Url,
) -> Result<(String, Url), String> {
    match url.scheme() {
        "http" | "https" => {
            let request = cookies.decorate_request(
                FetchRequest::get(url.clone())
                    .with_accept("text/html,application/xhtml+xml,text/plain;q=0.9,*/*;q=0.1"),
            );
            let response = transport
                .fetch(&request, &CancelToken::default())
                .map_err(|error| format!("navigation to {url} failed: {error}"))?;
            let _ = cookies.absorb_response(&response);
            if response.body.len() > MAX_PAGE_BYTES {
                return Err(format!(
                    "navigation body is {} bytes; limit is {MAX_PAGE_BYTES}",
                    response.body.len()
                ));
            }
            let media_type = response
                .content_type
                .as_ref()
                .map(|content_type| content_type.media_type.as_str());
            if !matches!(media_type, None | Some("text/html" | "text/plain")) {
                return Err(format!(
                    "response content type {:?} is not renderable as a document",
                    media_type.unwrap_or_default()
                ));
            }
            if !response.status.is_success() && response.body.is_empty() {
                return Err(format!(
                    "server returned HTTP status {} with an empty body",
                    response.status.as_u16()
                ));
            }
            let decoded = decode_html_bytes(
                &response.body,
                &HtmlDecodeOptions {
                    transport_encoding_label: response
                        .content_type
                        .as_ref()
                        .and_then(|content_type| content_type.charset.clone()),
                    ..HtmlDecodeOptions::default()
                },
            )
            .map_err(|error| format!("HTML decoding failed: {error}"))?;
            let html = if media_type == Some("text/plain") {
                format!(
                    "<!doctype html><html><head><title>Plain text</title></head><body><pre>{}</pre></body></html>",
                    escape_html(&decoded.text)
                )
            } else {
                decoded.text
            };
            Ok((html, response.final_url))
        }
        "file" => {
            let path = url
                .to_file_path()
                .map_err(|()| format!("file URL has no local path: {url}"))?;
            let bytes = fs::read(&path)
                .map_err(|error| format!("could not read {}: {error}", path.display()))?;
            if bytes.len() > MAX_PAGE_BYTES {
                return Err(format!(
                    "document is {} bytes; limit is {MAX_PAGE_BYTES}",
                    bytes.len()
                ));
            }
            let decoded = decode_html_bytes(&bytes, &HtmlDecodeOptions::default())
                .map_err(|error| format!("HTML decoding failed: {error}"))?;
            Ok((decoded.text, url.clone()))
        }
        scheme => Err(format!(
            "unsupported URL scheme {scheme:?}; use http, https or file"
        )),
    }
}

fn module_outcome(result: FetchResult) -> Result<(Url, String), String> {
    match result {
        Ok(response) if !response.status.is_success() => Err(format!(
            "module server returned HTTP status {}",
            response.status.as_u16()
        )),
        Ok(response) => Ok((
            response.final_url,
            String::from_utf8_lossy(&response.body).into_owned(),
        )),
        Err(error) => Err(error.to_string()),
    }
}

fn is_forbidden_request_header(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.starts_with("proxy-")
        || name.starts_with("sec-")
        || matches!(
            name.as_str(),
            "accept-charset"
                | "accept-encoding"
                | "access-control-request-headers"
                | "access-control-request-method"
                | "connection"
                | "content-length"
                | "cookie"
                | "cookie2"
                | "date"
                | "dnt"
                | "expect"
                | "host"
                | "keep-alive"
                | "origin"
                | "referer"
                | "set-cookie"
                | "te"
                | "trailer"
                | "transfer-encoding"
                | "upgrade"
                | "via"
        )
}

const BLOCKED_BY_CORS: &str = "network: blocked by the CORS policy";

fn cors_response_allows(headers: &[Header], page_url: &Url) -> bool {
    let origin = page_url.origin().ascii_serialization();
    let mut values = headers
        .iter()
        .filter(|header| {
            header
                .name
                .eq_ignore_ascii_case("access-control-allow-origin")
        })
        .map(|header| String::from_utf8_lossy(&header.value).trim().to_owned());
    match (values.next(), values.next()) {
        (Some(value), None) => value == "*" || value == origin,
        _ => false,
    }
}

fn exposed_response_headers(headers: &[Header], cross_origin: bool) -> Vec<(String, String)> {
    headers
        .iter()
        .filter(|header| {
            !cross_origin
                || is_safelisted_response_header(&header.name)
                || list_allows(headers, "access-control-expose-headers", &header.name)
        })
        .map(|header| {
            (
                header.name.clone(),
                String::from_utf8_lossy(&header.value).into_owned(),
            )
        })
        .collect()
}

fn is_safelisted_response_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "cache-control"
            | "content-language"
            | "content-length"
            | "content-type"
            | "expires"
            | "last-modified"
            | "pragma"
    )
}

fn list_allows(headers: &[Header], list_name: &str, item: &str) -> bool {
    headers
        .iter()
        .filter(|header| header.name.eq_ignore_ascii_case(list_name))
        .flat_map(|header| {
            String::from_utf8_lossy(&header.value)
                .split(',')
                .map(str::trim)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .any(|candidate| candidate == "*" || candidate.eq_ignore_ascii_case(item))
}

fn runtime_limits() -> render_core::js::RuntimeLimits {
    let mut limits = render_core::js::RuntimeLimits::default();
    if let Ok(value) = std::env::var("RENDER_JS_STEPS")
        && let Ok(steps) = value.parse::<usize>()
    {
        limits.max_execution_steps = steps;
    }
    limits
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn json_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character < ' ' => {
                use std::fmt::Write as _;
                let _ = write!(escaped, "\\u{:04x}", character as u32);
            }
            character => escaped.push(character),
        }
    }
    escaped.push('"');
    escaped
}
