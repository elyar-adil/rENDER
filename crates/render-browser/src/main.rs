//! Native browser shell for the self-owned Rust rendering pipeline.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use render_browser::chrome::{
    AddressClickTracker, AddressContextMenu, Canvas, ChromeLayout, ChromeTheme, HitTarget, Point,
    TabDrag, TitleBarClickTracker, TitleBarGesture, WindowAction, address_index_at_x,
    paint_address_context_menu, paint_chrome,
};
use render_browser::editor::{AddressCommand, AddressEditor, Clipboard, NativeClipboard};
use render_browser::font_backend::SystemFontBackend;
use render_browser::home::{HOME_HTML, HOME_TITLE};
use render_browser::images::{ImageFetchPlan, apply_image_batch, plan_images_with_styles};
use render_browser::model::{PageScrollState, TabId, TabIntent, TabModel};
use render_browser::navigation::{NavigationIntent, NavigationTarget, intent_from_address};
use render_browser::resources::{
    StylesheetFetchPlan, StylesheetResourceDiagnostic, apply_stylesheet_batch,
    plan_external_style_sheets,
};
use render_browser::scripts::{
    ScriptBatchPreparation, ScriptFetchPlan, ScriptResourceDiagnostic,
    plan_unstarted_classic_scripts, prepare_script_batch,
};
use render_browser::worker::{
    CompletedRender, RenderCancellation, RenderFailure, RenderIdentity, RenderJob, RenderOffset,
    RenderViewport, RenderWorker, RenderWorkerOptions,
};
use render_core::css::computed::ComputedStyle;
use render_core::document::{
    Document, DocumentBackends, DocumentRenderOptions, ExternalStyleSheets,
};
use render_core::html::{HtmlDecodeOptions, decode_html_bytes};
use render_core::image::{ImageLimits, ImageResources};
use render_core::interaction::{
    ButtonBehavior, DefaultActionKind, FormMethod, activation_plan, plan_form_submission,
};
use render_core::js::{JsValue, RuntimeLimits};
use render_core::layout::{PhysicalPoint, PhysicalRect, PhysicalSize};
use render_core::navigation::{HistoryEntry, NavigationLimits, SessionHistory};
use render_core::page::{Page, PageJob};
use render_core::paint::{Color, CpuRasterizer, DisplayList, PaintCoordinateSpace, Surface};
use render_core::script::{ScriptDiagnostic, ScriptDiscoveryLimits, ScriptScheduling};
use render_net::{
    BatchOptions, CookieJar, FetchConfig, FetchError, FetchRequest, FetchResponse, FetchResult,
    HttpTransport, NetworkWorker, RequestHandle, Url,
};
use softbuffer::{Context, Surface as WindowSurface};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize as WindowSize};
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{CursorIcon, Theme, Window, WindowId};

const INITIAL_WIDTH: u32 = 1_180;
const INITIAL_HEIGHT: u32 = 780;
const SCROLL_LINE_PIXELS: f32 = 40.0;

#[derive(Clone, Copy)]
struct ContentHitRegion {
    bounds: PhysicalRect,
    source: Option<render_core::dom::NodeId>,
    coordinate_space: PaintCoordinateSpace,
}

#[allow(
    clippy::cast_precision_loss,
    reason = "native window dimensions are far below the exact f32 integer range"
)]
fn hit_test_content_regions(
    regions: impl DoubleEndedIterator<Item = ContentHitRegion>,
    window_point: Point,
    chrome_height: u32,
    scroll_offset: PhysicalPoint,
) -> Option<render_core::dom::NodeId> {
    let viewport_point = PhysicalPoint {
        x: window_point.x,
        y: window_point.y - chrome_height as f32,
    };
    if viewport_point.x < 0.0
        || viewport_point.y < 0.0
        || !viewport_point.x.is_finite()
        || !viewport_point.y.is_finite()
    {
        return None;
    }
    regions.rev().find_map(|region| {
        let source = region.source?;
        let point = match region.coordinate_space {
            PaintCoordinateSpace::Document => PhysicalPoint {
                x: viewport_point.x + scroll_offset.x,
                y: viewport_point.y + scroll_offset.y,
            },
            PaintCoordinateSpace::Viewport => viewport_point,
        };
        (point.x >= region.bounds.origin.x
            && point.x < region.bounds.right()
            && point.y >= region.bounds.origin.y
            && point.y < region.bounds.bottom())
        .then_some(source)
    })
}

fn get_content_navigation_target(
    dom: &render_core::dom::Dom,
    hit_node: render_core::dom::NodeId,
    document_url: &Url,
) -> Option<Url> {
    let mut candidate = Some(hit_node);
    while let Some(node) = candidate {
        match activation_plan(dom, node).map(|plan| plan.default_action) {
            Some(DefaultActionKind::FollowHyperlink { href }) => {
                return document_url.join(&href).ok();
            }
            Some(DefaultActionKind::InvokeButton(ButtonBehavior::Submit))
                if dom.attribute(node, "disabled").ok().flatten().is_none() =>
            {
                let submission = plan_form_submission(dom, node, document_url).ok()?;
                return (submission.method == FormMethod::Get).then_some(submission.target);
            }
            _ => {}
        }
        candidate = dom.parent(node);
    }
    None
}

type NativeSurface = WindowSurface<Arc<Window>, Arc<Window>>;

fn main() {
    // The interpreter and parser recurse deeply on minified real-world
    // scripts; the default main-thread stack overflows. Run the entire
    // event loop on a dedicated thread with a generous stack.
    let child = std::thread::Builder::new()
        .stack_size(512 * 1024 * 1024)
        .spawn(browser_main)
        .expect("spawn browser main thread");
    match child.join() {
        Ok(Ok(())) => {}
        Ok(Err(message)) => {
            eprintln!("error: {message}");
            std::process::exit(1);
        }
        Err(panic_payload) => std::panic::resume_unwind(panic_payload),
    }
}

fn browser_main() -> Result<(), String> {
    use winit::platform::windows::EventLoopBuilderExtWindows as _;
    let Some(initial) = load_initial_page().map_err(|error| error.to_string())? else {
        return Ok(());
    };
    let fonts = Arc::new(SystemFontBackend::load().map_err(|error| error.to_string())?);
    let event_loop = {
        let mut builder = EventLoop::<UserEvent>::with_user_event();
        // The event loop lives on our dedicated big-stack thread for the
        // whole program lifetime; no other thread touches it.
        builder.with_any_thread(true);
        builder.build().map_err(|error| error.to_string())?
    };
    event_loop.set_control_flow(ControlFlow::Wait);
    let network = NetworkWorker::start(HttpTransport::new(FetchConfig::default()))
        .map_err(|error| error.to_string())?;
    let render_worker = start_render_worker(Arc::clone(&fonts), event_loop.create_proxy())
        .map_err(|error| error.to_string())?;
    let mut app = BrowserApp::new(initial, fonts, network, render_worker);
    event_loop
        .run_app(&mut app)
        .map_err(|error| error.to_string())?;
    Ok(())
}

#[derive(Clone, Copy, Debug)]
enum UserEvent {
    RenderReady,
}

#[derive(Clone, Debug)]
struct PageSource {
    html: String,
    title: String,
    target: NavigationTarget,
}

fn load_initial_page() -> Result<Option<PageSource>, Box<dyn Error>> {
    let mut arguments = env::args_os().skip(1);
    let Some(argument) = arguments.next() else {
        return Ok(Some(home_source()));
    };
    if argument == "-h" || argument == "--help" {
        println!(
            "Usage: render-browser [URL_OR_LOCAL_HTML_PATH]\n\nNo argument opens the built-in home page. HTTP and HTTPS URLs use the browser's normal navigation pipeline."
        );
        return Ok(None);
    }
    if arguments.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected at most one URL or local HTML path",
        )
        .into());
    }
    if let Some(value) = argument.to_str()
        && let Ok(url) = Url::parse(value)
        && matches!(url.scheme(), "http" | "https")
    {
        return Ok(Some(network_start_source(url)));
    }
    let path = PathBuf::from(argument);
    if path.to_string_lossy().contains("://") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the command-line URL must use HTTP or HTTPS",
        )
        .into());
    }
    source_from_local_file(path).map(Some)
}

fn home_source() -> PageSource {
    PageSource {
        html: HOME_HTML.to_owned(),
        title: HOME_TITLE.to_owned(),
        target: NavigationTarget::Home,
    }
}

fn network_start_source(url: Url) -> PageSource {
    PageSource {
        html: String::new(),
        title: "Loading".to_owned(),
        target: NavigationTarget::Url(url),
    }
}

fn source_from_local_file(path: PathBuf) -> Result<PageSource, Box<dyn Error>> {
    if !path.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("local HTML file does not exist: {}", path.display()),
        )
        .into());
    }
    let path = fs::canonicalize(path)?;
    let bytes = fs::read(&path)?;
    let html = decode_html_bytes(&bytes, &HtmlDecodeOptions::default())?.text;
    let title = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Local page")
        .to_owned();
    Ok(PageSource {
        html,
        title,
        target: NavigationTarget::Url(Url::from_file_path(&path).map_err(|()| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "local path cannot be represented as a file URL: {}",
                    path.display()
                ),
            )
        })?),
    })
}

fn status_source(
    target: NavigationTarget,
    title: &str,
    heading: &str,
    message: &str,
) -> PageSource {
    let address = escape_html(&target.display_address());
    let title_html = escape_html(title);
    let heading = escape_html(heading);
    let message = escape_html(message);
    let html = format!(
        r#"<!doctype html><html><head><title>{title_html}</title><style>
html {{ background-color: #f5f7fb; color: #172033; }} body {{ display: block; margin-top: 0px; margin-right: 0px; margin-bottom: 0px; margin-left: 0px; }}
main {{ display: block; width: 720px; margin-top: 72px; margin-right: auto; margin-bottom: 40px; margin-left: auto; background-color: white; padding-top: 36px; padding-right: 40px; padding-bottom: 36px; padding-left: 40px; }}
h1, p {{ display: block; }} h1 {{ color: #1859a9; margin-top: 0px; }} .address {{ display: block; background-color: #eef3f9; padding-top: 14px; padding-right: 16px; padding-bottom: 14px; padding-left: 16px; margin-top: 22px; }}
</style></head><body><main><h1>{heading}</h1><p>{message}</p><div class="address">{address}</div></main></body></html>"#
    );
    PageSource {
        html,
        title: title.to_owned(),
        target,
    }
}

fn error_source(target: NavigationTarget, message: &str) -> PageSource {
    status_source(
        target,
        "Load error",
        "This page could not be loaded",
        message,
    )
}

fn source_from_network_response(response: &FetchResponse) -> Result<PageSource, String> {
    let target = NavigationTarget::Url(response.final_url.clone());
    if !response.status.is_success() {
        return Err(format!(
            "The server returned HTTP status {}.",
            response.status.as_u16()
        ));
    }

    let media_type = response
        .content_type
        .as_ref()
        .map(|content_type| content_type.media_type.as_str());
    if let Some(media_type) = media_type
        && !matches!(media_type, "text/html" | "text/plain")
    {
        return Err(format!(
            "The response content type '{media_type}' is not renderable as a document yet."
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
    let title = response
        .final_url
        .host_str()
        .unwrap_or(response.final_url.as_str())
        .to_owned();
    Ok(PageSource {
        html,
        title,
        target,
    })
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[derive(Clone, Debug)]
enum PageRenderPayload {
    Full(Box<FullPageRenderPayload>),
    RetainedRaster {
        display_list: Arc<DisplayList>,
        images: ImageResources,
        raster_background: Color,
        content_height: f32,
        viewport_height: f32,
    },
}

#[derive(Clone, Debug)]
struct FullPageRenderPayload {
    document: Document,
    base_url: Url,
    external_style_sheets: ExternalStyleSheets,
    style_batch: Option<(StylesheetFetchPlan, Vec<FetchResult>)>,
    discover_external_styles: bool,
    images: ImageResources,
}

#[derive(Debug)]
struct PageRenderFrame {
    frame: Vec<u32>,
    viewport: WindowSize<u32>,
    display_list: Option<Arc<DisplayList>>,
    raster_background: Color,
    content_height: f32,
    viewport_height: f32,
    applied_style_sheets: Option<ExternalStyleSheets>,
    style_plan: Option<StylesheetFetchPlan>,
    style_diagnostics: Vec<StylesheetResourceDiagnostic>,
    computed_styles: Option<BTreeMap<render_core::dom::NodeId, ComputedStyle>>,
}

type PageRenderWorker = RenderWorker<PageRenderPayload, PageRenderFrame>;

fn start_render_worker(
    fonts: Arc<SystemFontBackend>,
    proxy: EventLoopProxy<UserEvent>,
) -> Result<PageRenderWorker, render_browser::worker::RenderWorkerStartError> {
    RenderWorker::start(
        RenderWorkerOptions::default(),
        move |job, cancellation| process_page_render(job, cancellation, &fonts),
        move || {
            let _event_loop_closed = proxy.send_event(UserEvent::RenderReady);
        },
    )
}

#[allow(clippy::too_many_lines)]
fn process_page_render(
    job: RenderJob<PageRenderPayload>,
    cancellation: &RenderCancellation,
    fonts: &SystemFontBackend,
) -> Result<PageRenderFrame, RenderFailure> {
    cancellation.check()?;
    match job.payload {
        PageRenderPayload::Full(full) => {
            let FullPageRenderPayload {
                document,
                base_url,
                external_style_sheets,
                style_batch,
                discover_external_styles,
                images,
            } = *full;
            cancellation.check()?;
            let (style_sheets, applied_style_sheets, style_diagnostics) =
                if let Some((plan, results)) = style_batch {
                    let application = apply_stylesheet_batch(&document, &plan, results);
                    (
                        application.style_sheets.clone(),
                        Some(application.style_sheets),
                        application.diagnostics,
                    )
                } else {
                    (external_style_sheets, None, Vec::new())
                };
            cancellation.check()?;
            let style_plan = discover_external_styles.then(|| {
                plan_external_style_sheets(
                    &document,
                    &base_url,
                    DocumentRenderOptions::default().document_limits,
                )
            });
            cancellation.check()?;
            let mut options = DocumentRenderOptions::default();
            options.layout.viewport = PhysicalSize {
                width: viewport_dimension(job.identity.viewport.width),
                height: viewport_dimension(job.identity.viewport.height),
            };
            options.scroll_offset = PhysicalPoint {
                x: job.identity.scroll_offset.x,
                y: job.identity.scroll_offset.y,
            };
            let raster_background = options.raster_background;
            let output = document.render_with_external_style_sheets_and_images(
                options,
                DocumentBackends {
                    text_measurer: fonts,
                    text_shaper: fonts,
                    glyph_masks: fonts,
                },
                &base_url,
                &style_sheets,
                &images,
            );
            cancellation.check()?;
            let viewport = WindowSize::new(
                output.raster.surface.width(),
                output.raster.surface.height(),
            );
            let content_height = output.layout.fragments.scrollable_content_size.height;
            let viewport_height = output.layout.fragments.viewport.height;
            let frame = surface_to_softbuffer(&output.raster.surface);
            Ok(PageRenderFrame {
                frame,
                viewport,
                display_list: Some(Arc::new(output.display.list)),
                raster_background,
                content_height,
                viewport_height,
                applied_style_sheets,
                style_plan,
                style_diagnostics,
                computed_styles: Some(output.styles),
            })
        }
        PageRenderPayload::RetainedRaster {
            display_list,
            images,
            raster_background,
            content_height,
            viewport_height,
        } => {
            let raster = CpuRasterizer.rasterize_viewport_with_images(
                &display_list,
                raster_background,
                fonts,
                PhysicalPoint {
                    x: job.identity.scroll_offset.x,
                    y: job.identity.scroll_offset.y,
                },
                Some(&images),
            );
            cancellation.check()?;
            Ok(PageRenderFrame {
                viewport: WindowSize::new(raster.surface.width(), raster.surface.height()),
                frame: surface_to_softbuffer(&raster.surface),
                display_list: None,
                raster_background,
                content_height,
                viewport_height,
                applied_style_sheets: None,
                style_plan: None,
                style_diagnostics: Vec::new(),
                computed_styles: None,
            })
        }
    }
}

struct PageState {
    navigation: PageNavigation<RequestHandle<FetchResult>>,
    page: Page,
    cookies: CookieJar,
    style_sheets: ExternalStyleSheets,
    style_batch: Option<(StylesheetFetchPlan, Vec<FetchResult>)>,
    images: ImageResources,
    computed_styles: BTreeMap<render_core::dom::NodeId, ComputedStyle>,
    pending_images: Option<PendingImages>,
    styles_resolved: bool,
    pending_style_sheets: Option<PendingStyleSheets>,
    scripts_resolved: bool,
    pending_scripts: Option<PendingScripts>,
    started_scripts: HashSet<render_core::dom::NodeId>,
    initial_script_scan_completed: bool,
    frame: Vec<u32>,
    viewport: WindowSize<u32>,
    display_list: Option<Arc<DisplayList>>,
    raster_background: Color,
    scroll: PageScrollState,
    history: SessionHistory,
    dom_revision: u64,
    external_styles_generation: u64,
    render_generation: u64,
    expected_render: Option<RenderIdentity>,
    /// Wall-clock anchor for the page's virtual event-loop clock.
    created_at: Instant,
}

struct PageNavigation<H> {
    committed: PageSource,
    pending: Option<PendingNavigation<H>>,
}

struct PendingNavigation<H> {
    requested_url: Url,
    handle: H,
}

struct PendingStyleSheets {
    plan: StylesheetFetchPlan,
    handle: RequestHandle<Vec<FetchResult>>,
}

struct PendingScripts {
    plan: ScriptFetchPlan,
    handle: RequestHandle<Vec<FetchResult>>,
}

struct PendingImages {
    plan: ImageFetchPlan,
    handle: RequestHandle<Vec<FetchResult>>,
}

impl<H> PageNavigation<H> {
    fn new(committed: PageSource) -> Self {
        Self {
            committed,
            pending: None,
        }
    }

    fn begin(&mut self, requested_url: Url, handle: H) {
        self.pending = Some(PendingNavigation {
            requested_url,
            handle,
        });
    }

    fn commit(&mut self, source: PageSource) {
        self.committed = source;
        self.pending = None;
    }

    fn take_pending(&mut self) -> Option<PendingNavigation<H>> {
        self.pending.take()
    }

    fn pending_url(&self) -> Option<&Url> {
        self.pending.as_ref().map(|pending| &pending.requested_url)
    }

    const fn committed(&self) -> &PageSource {
        &self.committed
    }
}

impl PageState {
    fn new(source: PageSource) -> Self {
        let history = SessionHistory::new(
            HistoryEntry::new(source.target.history_url()),
            NavigationLimits::default(),
        )
        .expect("browser-created page URLs fit the session-history limits");
        let page = Page::with_url(&source.html, &source.target.history_url());
        Self {
            navigation: PageNavigation::new(source),
            page,
            cookies: CookieJar::default(),
            style_sheets: ExternalStyleSheets::default(),
            style_batch: None,
            images: ImageResources::default(),
            computed_styles: BTreeMap::new(),
            pending_images: None,
            styles_resolved: false,
            pending_style_sheets: None,
            scripts_resolved: false,
            pending_scripts: None,
            started_scripts: HashSet::new(),
            initial_script_scan_completed: false,
            frame: Vec::new(),
            viewport: WindowSize::new(0, 0),
            display_list: None,
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

    fn set_source(&mut self, source: PageSource) {
        self.cancel_style_sheets();
        self.cancel_scripts();
        self.page = Page::with_url(&source.html, &source.target.history_url());
        self.navigation.commit(source);
        self.style_sheets = ExternalStyleSheets::default();
        self.style_batch = None;
        self.images = ImageResources::default();
        self.computed_styles.clear();
        self.styles_resolved = false;
        self.scripts_resolved = false;
        self.started_scripts.clear();
        self.initial_script_scan_completed = false;
        self.frame.clear();
        self.viewport = WindowSize::new(0, 0);
        self.display_list = None;
        self.scroll.reset();
        self.dom_revision = self.page.document().dom().revision().as_u64();
        self.external_styles_generation = 0;
        self.expected_render = None;
        self.created_at = Instant::now();
    }

    fn cancel_pending(&mut self) {
        if let Some(pending) = self.navigation.take_pending() {
            pending.handle.cancel();
        }
        self.cancel_style_sheets();
        self.cancel_scripts();
        self.cancel_images();
    }

    fn cancel_style_sheets(&mut self) {
        if let Some(pending) = self.pending_style_sheets.take() {
            pending.handle.cancel();
        }
    }

    fn cancel_scripts(&mut self) {
        if let Some(pending) = self.pending_scripts.take() {
            pending.handle.cancel();
        }
    }

    fn cancel_images(&mut self) {
        if let Some(pending) = self.pending_images.take() {
            pending.handle.cancel();
        }
    }

    fn execute_script_batch(&mut self, preparation: ScriptBatchPreparation) -> bool {
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
        loop {
            match self.page.run_one_turn_reference() {
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

    /// Print buffered `console.*` output from the page's script runtime.
    fn drain_console(&mut self) {
        for message in self.page.runtime_mut().take_console_messages() {
            eprintln!("[console.{}] {}", message.level.label(), message.text);
        }
    }

    /// Advance the page's virtual clock to real elapsed time and run every
    /// ready turn (timers, queued events, scripts).
    ///
    /// Returns whether any turn rendered plus per-task default-action results
    /// (`true` when the task's event was not `preventDefault()`-ed).
    fn run_page_turns(&mut self) -> (bool, HashMap<render_core::event_loop::TaskId, bool>) {
        let now = self.created_at.elapsed();
        let mut rendered = false;
        let mut defaults = HashMap::new();
        match self.page.pump_until_idle_reference(now) {
            Ok(outcome) => {
                rendered = outcome.rendered_turns > 0;
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
        (rendered, defaults)
    }

    /// Earliest wall-clock instant at which this page needs a wake-up.
    fn next_wake_instant(&self) -> Option<Instant> {
        self.page
            .next_wake_deadline()
            .map(|deadline| self.created_at + deadline)
    }

    /// Whether the page still has script work that requires future turns.
    fn has_pending_script_work(&self) -> bool {
        self.page.has_pending_work()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HistoryMode {
    Push,
    Current,
}
struct BrowserApp {
    tabs: TabModel,
    pages: HashMap<TabId, PageState>,
    fonts: Arc<SystemFontBackend>,
    render_worker: PageRenderWorker,
    network: NetworkWorker,
    editor: AddressEditor,
    content_editor: Option<ContentTextEditor>,
    clipboard: NativeClipboard,
    window: Option<Arc<Window>>,
    context: Option<Context<Arc<Window>>>,
    surface: Option<NativeSurface>,
    layout: Option<ChromeLayout>,
    frame: Vec<u32>,
    frame_size: WindowSize<u32>,
    theme: ChromeTheme,
    cursor: Point,
    hot: HitTarget,
    drag: Option<TabDrag>,
    address_selecting: bool,
    address_menu: Option<AddressContextMenu>,
    modifiers: ModifiersState,
    title_bar_clicks: TitleBarClickTracker,
    address_clicks: AddressClickTracker,
    started_at: Instant,
}

impl BrowserApp {
    fn new(
        initial: PageSource,
        fonts: Arc<SystemFontBackend>,
        network: NetworkWorker,
        render_worker: PageRenderWorker,
    ) -> Self {
        let tabs = TabModel::new(initial.title.clone(), initial.target.display_address());
        let active = tabs.active_id();
        let editor = AddressEditor::new(initial.target.display_address());
        Self {
            tabs,
            pages: HashMap::from([(active, PageState::new(initial))]),
            fonts,
            render_worker,
            network,
            editor,
            content_editor: None,
            clipboard: NativeClipboard::default(),
            window: None,
            context: None,
            surface: None,
            layout: None,
            frame: Vec::new(),
            frame_size: WindowSize::new(0, 0),
            theme: ChromeTheme::Light,
            cursor: Point::default(),
            hot: HitTarget::Chrome,
            drag: None,
            address_selecting: false,
            address_menu: None,
            modifiers: ModifiersState::default(),
            title_bar_clicks: TitleBarClickTracker::default(),
            address_clicks: AddressClickTracker::default(),
            started_at: Instant::now(),
        }
    }

    fn initialize(&mut self, event_loop: &ActiveEventLoop) -> Result<(), Box<dyn Error>> {
        let attributes = Window::default_attributes()
            .with_title("rENDER")
            .with_decorations(false)
            .with_inner_size(LogicalSize::new(
                f64::from(INITIAL_WIDTH),
                f64::from(INITIAL_HEIGHT),
            ))
            .with_min_inner_size(LogicalSize::new(560.0, 360.0));
        let window = Arc::new(event_loop.create_window(attributes)?);
        self.theme = window.theme().map_or(ChromeTheme::Light, theme_from_winit);
        let context = Context::new(window.clone())?;
        let surface = NativeSurface::new(&context, window.clone())?;
        self.window = Some(window);
        self.context = Some(context);
        self.surface = Some(surface);
        self.relayout_and_render(true);
        let initial_network_url = self
            .pages
            .get(&self.tabs.active_id())
            .map(|page| page.navigation.committed().target.clone())
            .and_then(|target| match target {
                NavigationTarget::Url(url) if matches!(url.scheme(), "http" | "https") => Some(url),
                _ => None,
            });
        if let Some(url) = initial_network_url {
            self.start_network_navigation(self.tabs.active_id(), url);
        }
        self.request_redraw();
        Ok(())
    }

    fn relayout_and_render(&mut self, render_page: bool) {
        let Some((size, scale)) = self
            .window
            .as_ref()
            .map(|window| (window.inner_size(), finite_f32(window.scale_factor())))
        else {
            return;
        };
        if size.width == 0 || size.height == 0 {
            return;
        }
        let layout = ChromeLayout::new(size.width, size.height, scale, self.tabs.tabs());
        if render_page {
            let viewport =
                WindowSize::new(size.width, size.height.saturating_sub(layout.chrome_height));
            self.schedule_page_render(self.tabs.active_id(), viewport, false);
        }
        self.layout = Some(layout);
        self.compose_frame(size);
        self.update_window_title();
    }

    fn schedule_page_render(
        &mut self,
        id: TabId,
        viewport: WindowSize<u32>,
        prefer_retained_raster: bool,
    ) {
        let Some(page) = self.pages.get_mut(&id) else {
            return;
        };
        page.render_generation = page.render_generation.saturating_add(1);
        if viewport.width == 0 || viewport.height == 0 {
            page.frame.clear();
            page.viewport = viewport;
            page.expected_render = None;
            self.render_worker.cancel_tab(id.as_u64());
            return;
        }
        let identity = RenderIdentity {
            tab_id: id.as_u64(),
            generation: page.render_generation,
            dom_revision: page.dom_revision,
            viewport: RenderViewport {
                width: viewport.width,
                height: viewport.height,
            },
            scroll_offset: RenderOffset {
                x: 0.0,
                y: page.scroll.offset_y(),
            },
            external_styles_generation: page.external_styles_generation,
        };
        let can_raster_retained = prefer_retained_raster
            && page.viewport == viewport
            && page.style_batch.is_none()
            && page.display_list.is_some();
        let payload = if can_raster_retained {
            PageRenderPayload::RetainedRaster {
                display_list: Arc::clone(
                    page.display_list
                        .as_ref()
                        .expect("retained display list was checked"),
                ),
                raster_background: page.raster_background,
                images: page.images.clone(),
                content_height: page.scroll.content_height(),
                viewport_height: page.scroll.viewport_height(),
            }
        } else {
            PageRenderPayload::Full(Box::new(FullPageRenderPayload {
                document: page.page.document().clone(),
                base_url: page.navigation.committed().target.history_url(),
                external_style_sheets: page.style_sheets.clone(),
                style_batch: page.style_batch.clone(),
                discover_external_styles: !page.styles_resolved
                    && page.pending_style_sheets.is_none()
                    && page.style_batch.is_none(),
                images: page.images.clone(),
            }))
        };
        let source_snapshot = Arc::from(page.navigation.committed().html.as_str());
        page.expected_render = Some(identity);
        if let Err(error) = self.render_worker.submit(RenderJob {
            identity,
            source_snapshot,
            payload,
        }) {
            page.expected_render = None;
            eprintln!("render-browser could not submit a render job: {error}");
        }
    }

    fn poll_render_worker(&mut self) {
        for completed in self.render_worker.drain_latest() {
            self.commit_render(completed);
        }
    }

    fn commit_render(&mut self, completed: CompletedRender<PageRenderFrame>) {
        let id = self
            .pages
            .keys()
            .copied()
            .find(|id| id.as_u64() == completed.identity.tab_id);
        let Some(id) = id else {
            return;
        };
        let frame = match completed.result {
            Ok(frame) => frame,
            Err(error) => {
                if self
                    .pages
                    .get(&id)
                    .is_some_and(|page| page.expected_render == Some(completed.identity))
                {
                    eprintln!("render-browser background render failed: {error}");
                }
                return;
            }
        };
        let style_plan = {
            let Some(page) = self.pages.get_mut(&id) else {
                return;
            };
            if page.expected_render != Some(completed.identity) {
                return;
            }
            page.expected_render = None;
            page.frame = frame.frame;
            page.viewport = frame.viewport;
            if let Some(display_list) = frame.display_list {
                page.display_list = Some(display_list);
            }
            page.raster_background = frame.raster_background;
            if let Some(styles) = frame.computed_styles {
                page.computed_styles = styles;
            }
            page.scroll
                .update_metrics(frame.content_height, frame.viewport_height);
            if let Some(style_sheets) = frame.applied_style_sheets {
                page.style_sheets = style_sheets;
                page.style_batch = None;
                page.styles_resolved = true;
                self.tabs.set_loading(id, false);
            }
            frame.style_plan
        };
        report_stylesheet_diagnostics(&frame.style_diagnostics);

        if let Some(plan) = style_plan {
            self.start_external_style_sheets(id, plan);
        } else {
            self.start_images(id);
            self.start_classic_scripts(id);
        }
        if id == self.tabs.active_id() {
            if let Some(window) = &self.window {
                self.compose_frame(window.inner_size());
            }
            self.request_redraw();
        } else {
            self.repaint_chrome();
        }
    }

    fn compose_frame(&mut self, size: WindowSize<u32>) {
        let Some(layout) = &self.layout else {
            return;
        };
        let pixel_count = size.width as usize * size.height as usize;
        self.frame.resize(pixel_count, 0x00ff_ffff);
        self.frame.fill(if self.theme == ChromeTheme::Dark {
            0x001e_2026
        } else {
            0x00ff_ffff
        });
        if let Some(page) = self.pages.get(&self.tabs.active_id()) {
            blit_page(
                &mut self.frame,
                size,
                &page.frame,
                page.viewport,
                layout.chrome_height,
            );
        }
        let mut canvas = Canvas::new(&mut self.frame, size.width, size.height);
        let maximized = self
            .window
            .as_ref()
            .is_some_and(|window| window.is_maximized());
        paint_chrome(
            &mut canvas,
            layout,
            self.tabs.tabs(),
            self.tabs.active_id(),
            &self.editor,
            self.theme,
            self.hot,
            maximized,
            self.fonts.as_ref(),
        );
        if let Some(menu) = &self.address_menu {
            paint_address_context_menu(
                &mut canvas,
                menu,
                self.theme,
                self.cursor,
                self.fonts.as_ref(),
            );
        }
        self.frame_size = size;
    }

    fn request_redraw(&self) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn repaint_chrome(&mut self) {
        let Some(window) = &self.window else {
            return;
        };
        self.compose_frame(window.inner_size());
        self.request_redraw();
    }

    fn present(&mut self) -> Result<(), Box<dyn Error>> {
        let width = NonZeroU32::new(self.frame_size.width)
            .ok_or_else(|| io::Error::other("cannot present a zero-width frame"))?;
        let height = NonZeroU32::new(self.frame_size.height)
            .ok_or_else(|| io::Error::other("cannot present a zero-height frame"))?;
        let surface = self
            .surface
            .as_mut()
            .ok_or_else(|| io::Error::other("native surface is not initialized"))?;
        surface.resize(width, height)?;
        let mut buffer = surface.buffer_mut()?;
        if buffer.len() != self.frame.len() {
            return Err(io::Error::other("CPU and native surface sizes differ").into());
        }
        buffer.copy_from_slice(&self.frame);
        buffer.present()?;
        Ok(())
    }

    fn handle_tab_intent(&mut self, intent: TabIntent) {
        match intent {
            TabIntent::New => {
                let id = self.tabs.apply(intent).expect("new tab creates an id");
                self.pages.insert(id, PageState::new(home_source()));
                self.sync_active_address();
                self.relayout_and_render(true);
            }
            TabIntent::Close(id) => {
                self.render_worker.cancel_tab(id.as_u64());
                if let Some(mut page) = self.pages.remove(&id) {
                    page.cancel_pending();
                }
                if let Some(created) = self.tabs.apply(intent) {
                    self.pages.insert(created, PageState::new(home_source()));
                }
                self.sync_active_address();
                self.relayout_and_render(true);
            }
            TabIntent::Activate(_) => {
                self.tabs.apply(intent);
                self.sync_active_address();
                self.relayout_and_render(true);
            }
            TabIntent::Move { .. } => {
                self.tabs.apply(intent);
                self.relayout_and_render(false);
            }
        }
        self.request_redraw();
    }

    fn sync_active_address(&mut self) {
        self.content_editor = None;
        self.editor.set_text(self.tabs.active().address.clone());
        self.editor.set_focused(false);
        if let Some(window) = &self.window {
            window.set_ime_allowed(false);
        }
    }

    fn emit_navigation(&mut self, intent: NavigationIntent) {
        let tab = self.tabs.active_id();
        match intent {
            NavigationIntent::Navigate(input) => self.navigate_target(
                tab,
                NavigationTarget::from_address_input(input),
                HistoryMode::Push,
            ),
            NavigationIntent::Home => {
                self.navigate_target(tab, NavigationTarget::Home, HistoryMode::Push);
            }
            NavigationIntent::Reload => self.reload_active(),
            NavigationIntent::Back => self.traverse_active(false),
            NavigationIntent::Forward => self.traverse_active(true),
        }
    }

    /// Perform any navigations the page's script requested since the last
    /// pump (`location.assign`/`replace`/`href`). Only the newest request is
    /// honored; scripts that redirect repeatedly cannot loop the browser.
    fn drain_script_navigations(&mut self, id: TabId) {
        let Some(page) = self.pages.get_mut(&id) else {
            return;
        };
        let base = page.navigation.committed().target.history_url();
        let requests = page.page.runtime_mut().take_pending_navigations();
        let Some(request) = requests.into_iter().next_back() else {
            return;
        };
        let Ok(url) = Url::options().base_url(Some(&base)).parse(&request.url) else {
            eprintln!("render-browser ignoring invalid script navigation URL");
            return;
        };
        let mode = if request.replace {
            HistoryMode::Current
        } else {
            HistoryMode::Push
        };
        self.navigate_target(id, NavigationTarget::from_url(url), mode);
    }

    fn navigate_target(&mut self, id: TabId, target: NavigationTarget, mode: HistoryMode) {
        if self
            .content_editor
            .as_ref()
            .is_some_and(|editor| editor.tab == id)
        {
            self.content_editor = None;
        }
        let target_url = target.history_url();
        let Some(page) = self.pages.get_mut(&id) else {
            return;
        };
        page.cancel_pending();
        if mode == HistoryMode::Push
            && let Err(error) = page.history.push(HistoryEntry::new(target_url.clone()))
        {
            self.install_source(id, error_source(target, &error.to_string()), false);
            return;
        }

        match target {
            NavigationTarget::Home => self.install_source(id, home_source(), false),
            NavigationTarget::Url(url) if matches!(url.scheme(), "http" | "https") => {
                self.start_network_navigation(id, url);
            }
            NavigationTarget::Url(url) if url.scheme() == "file" => {
                let target = NavigationTarget::Url(url.clone());
                let source = url.to_file_path().map_or_else(
                    |()| {
                        error_source(
                            target.clone(),
                            "The file URL cannot be converted to a local absolute path.",
                        )
                    },
                    |path| {
                        source_from_local_file(path).unwrap_or_else(|error| {
                            error_source(target.clone(), &error.to_string())
                        })
                    },
                );
                self.install_source(id, source, false);
            }
            NavigationTarget::Url(url) => {
                let scheme = url.scheme().to_owned();
                self.install_source(
                    id,
                    error_source(
                        NavigationTarget::Url(url),
                        &format!("The {scheme} URL scheme is not supported by this build."),
                    ),
                    false,
                );
            }
        }
    }

    fn reload_active(&mut self) {
        let id = self.tabs.active_id();
        let Some(url) = self
            .pages
            .get(&id)
            .map(|page| page.history.reload().url.clone())
        else {
            return;
        };
        self.navigate_target(id, NavigationTarget::from_url(url), HistoryMode::Current);
    }

    fn traverse_active(&mut self, forward: bool) {
        let id = self.tabs.active_id();
        let Some(page) = self.pages.get_mut(&id) else {
            return;
        };
        page.cancel_pending();
        let entry = if forward {
            page.history.forward()
        } else {
            page.history.back()
        };
        let Some(url) = entry.map(|entry| entry.url.clone()) else {
            self.repaint_chrome();
            return;
        };
        self.navigate_target(id, NavigationTarget::from_url(url), HistoryMode::Current);
    }

    fn start_network_navigation(&mut self, id: TabId, url: Url) {
        let request =
            FetchRequest::get(url.clone()).with_accept("text/html,text/plain;q=0.8,*/*;q=0.1");
        let request = self.pages.get(&id).map_or(request.clone(), |page| {
            page.cookies.decorate_request(request)
        });
        let handle = self.network.submit(request);
        let Some(page) = self.pages.get_mut(&id) else {
            handle.cancel();
            return;
        };
        let committed_title = page.navigation.committed().title.clone();
        page.navigation.begin(url, handle);
        let pending_address = page
            .navigation
            .pending_url()
            .expect("a just-started navigation has a pending URL")
            .as_str()
            .to_owned();
        self.tabs.update(id, committed_title, pending_address);
        self.tabs.set_loading(id, true);
        if id == self.tabs.active_id() {
            self.sync_active_address();
        }
        self.repaint_chrome();
        self.request_redraw();
    }

    fn install_source(&mut self, id: TabId, source: PageSource, loading: bool) {
        self.tabs
            .update(id, source.title.clone(), source.target.display_address());
        self.tabs.set_loading(id, loading);
        if let Some(page) = self.pages.get_mut(&id) {
            page.set_source(source);
        }
        if id == self.tabs.active_id() {
            self.sync_active_address();
            self.relayout_and_render(true);
        } else {
            self.repaint_chrome();
        }
        self.request_redraw();
    }

    fn start_external_style_sheets(&mut self, id: TabId, plan: StylesheetFetchPlan) {
        let Some(page) = self.pages.get_mut(&id) else {
            return;
        };
        page.cancel_style_sheets();
        if plan.is_empty() {
            report_stylesheet_diagnostics(&plan.diagnostics);
            page.styles_resolved = true;
            self.tabs.set_loading(id, false);
            self.start_classic_scripts(id);
            self.repaint_chrome();
            return;
        }

        let requests = plan
            .requests()
            .into_iter()
            .map(|request| page.cookies.decorate_request(request))
            .collect();
        let handle = self.network.submit_batch(requests, BatchOptions::default());
        page.pending_style_sheets = Some(PendingStyleSheets { plan, handle });
        self.tabs.set_loading(id, true);
        self.repaint_chrome();
    }

    fn start_classic_scripts(&mut self, id: TabId) {
        let mut rerender = false;
        let mut loading_complete = false;
        loop {
            let Some(page) = self.pages.get_mut(&id) else {
                return;
            };
            if !page.styles_resolved || page.scripts_resolved || page.pending_scripts.is_some() {
                break;
            }

            let base_url = page.navigation.committed().target.history_url();
            let limits = ScriptDiscoveryLimits::default();
            if page.started_scripts.len() >= limits.max_script_elements {
                eprintln!(
                    "render-browser stopped dynamic script discovery after {} started scripts",
                    limits.max_script_elements
                );
                page.scripts_resolved = true;
                loading_complete = true;
                break;
            }
            let follow_up_scan = page.initial_script_scan_completed;
            let plan = plan_unstarted_classic_scripts(
                page.page.document(),
                &base_url,
                limits,
                &page.started_scripts,
                follow_up_scan,
            );
            if !follow_up_scan {
                report_script_discovery_diagnostics(&plan.discovery_diagnostics);
            }
            page.initial_script_scan_completed = true;
            page.started_scripts.extend(plan.owners());
            if plan.is_empty() {
                page.scripts_resolved = true;
                loading_complete = true;
                break;
            }
            if plan.resources.is_empty() {
                let preparation = prepare_script_batch(
                    page.page.document(),
                    &plan,
                    Vec::new(),
                    &RuntimeLimits::default(),
                );
                report_script_diagnostics(&preparation.diagnostics);
                rerender |= page.execute_script_batch(preparation);
            } else {
                let requests = plan
                    .requests()
                    .into_iter()
                    .map(|request| page.cookies.decorate_request(request))
                    .collect();
                let handle = self.network.submit_batch(requests, BatchOptions::default());
                page.pending_scripts = Some(PendingScripts { plan, handle });
                self.tabs.set_loading(id, true);
                break;
            }
        }

        if loading_complete {
            self.tabs.set_loading(id, false);
        }
        if rerender {
            self.schedule_page_render_for_tab(id);
        }
        self.repaint_chrome();
        self.request_redraw();
    }

    fn start_images(&mut self, id: TabId) {
        let Some(page) = self.pages.get_mut(&id) else {
            return;
        };
        if page.pending_images.is_some() {
            return;
        }
        let plan = plan_images_with_styles(
            page.page.document(),
            &page.computed_styles,
            &page.navigation.committed().target.history_url(),
            &page.images,
            ImageLimits::default(),
        );
        report_image_diagnostics(&plan.diagnostics);
        if plan.is_empty() {
            return;
        }
        let requests = plan
            .requests()
            .into_iter()
            .map(|request| page.cookies.decorate_request(request))
            .collect();
        let handle = self.network.submit_batch(requests, BatchOptions::default());
        page.pending_images = Some(PendingImages { plan, handle });
    }

    fn poll_network(&mut self) {
        let mut completed_documents = Vec::new();
        let mut completed_style_sheets = Vec::new();
        let mut completed_scripts = Vec::new();
        let mut completed_images = Vec::new();
        for (id, page) in &mut self.pages {
            if let Some(pending) = page.navigation.pending.as_ref() {
                match pending.handle.try_recv() {
                    Ok(result) => completed_documents.push((*id, result)),
                    Err(TryRecvError::Empty) => {}
                    Err(TryRecvError::Disconnected) => {
                        completed_documents.push((*id, Err(FetchError::WorkerStopped)));
                    }
                }
            }
            if let Some(pending) = page.pending_style_sheets.as_ref() {
                match pending.handle.try_recv() {
                    Ok(results) => completed_style_sheets.push((*id, results)),
                    Err(TryRecvError::Empty) => {}
                    Err(TryRecvError::Disconnected) => {
                        completed_style_sheets.push((
                            *id,
                            pending
                                .plan
                                .resources
                                .iter()
                                .map(|_| Err(FetchError::WorkerStopped))
                                .collect(),
                        ));
                    }
                }
            }
            if let Some(pending) = page.pending_scripts.as_ref() {
                match pending.handle.try_recv() {
                    Ok(results) => completed_scripts.push((*id, results)),
                    Err(TryRecvError::Empty) => {}
                    Err(TryRecvError::Disconnected) => {
                        completed_scripts.push((
                            *id,
                            pending
                                .plan
                                .resources
                                .iter()
                                .map(|_| Err(FetchError::WorkerStopped))
                                .collect(),
                        ));
                    }
                }
            }
            if let Some(pending) = page.pending_images.as_ref() {
                match pending.handle.try_recv() {
                    Ok(results) => completed_images.push((*id, results)),
                    Err(TryRecvError::Empty) => {}
                    Err(TryRecvError::Disconnected) => completed_images.push((
                        *id,
                        pending
                            .plan
                            .resources
                            .iter()
                            .map(|_| Err(FetchError::WorkerStopped))
                            .collect(),
                    )),
                }
            }
        }
        for (id, result) in completed_documents {
            let requested_url = self
                .pages
                .get_mut(&id)
                .and_then(|page| page.navigation.take_pending())
                .map(|pending| pending.requested_url);
            if let Some(requested_url) = requested_url {
                self.finish_network_navigation(id, requested_url, result);
            }
        }
        for (id, results) in completed_style_sheets {
            self.finish_external_style_sheets(id, results);
        }
        for (id, results) in completed_scripts {
            self.finish_classic_scripts(id, results);
        }
        for (id, results) in completed_images {
            self.finish_images(id, results);
        }
    }

    fn finish_network_navigation(&mut self, id: TabId, requested_url: Url, result: FetchResult) {
        let response = match result {
            Ok(response) => response,
            Err(error) => {
                self.install_source(
                    id,
                    error_source(
                        NavigationTarget::Url(requested_url),
                        &format!("Network request failed: {error}"),
                    ),
                    false,
                );
                return;
            }
        };
        let final_url = response.final_url.clone();
        if let Some(page) = self.pages.get_mut(&id) {
            for issue in page.cookies.absorb_response(&response) {
                eprintln!("browser cookie rejected: {}", issue.message);
            }
            let _history_result = page.history.replace(HistoryEntry::new(final_url.clone()));
        }
        match source_from_network_response(&response) {
            Ok(source) => {
                self.install_source(id, source, true);
            }
            Err(message) => self.install_source(
                id,
                error_source(NavigationTarget::Url(final_url), &message),
                false,
            ),
        }
    }

    fn finish_external_style_sheets(&mut self, id: TabId, results: Vec<FetchResult>) {
        let Some(page) = self.pages.get_mut(&id) else {
            return;
        };
        let Some(pending) = page.pending_style_sheets.take() else {
            return;
        };
        for response in results.iter().flatten() {
            for issue in page.cookies.absorb_response(response) {
                eprintln!("browser cookie rejected: {}", issue.message);
            }
        }
        page.style_batch = Some((pending.plan, results));
        page.external_styles_generation = page.external_styles_generation.saturating_add(1);

        if id == self.tabs.active_id() {
            let viewport = self.layout.as_ref().map(|layout| {
                WindowSize::new(
                    self.frame_size.width,
                    self.frame_size.height.saturating_sub(layout.chrome_height),
                )
            });
            if let Some(viewport) = viewport {
                self.schedule_page_render(id, viewport, false);
            }
        } else {
            // Background tabs render when activated; the decoded batch remains
            // attached to the tab snapshot until then.
        }
        self.repaint_chrome();
        self.request_redraw();
    }

    fn finish_classic_scripts(&mut self, id: TabId, results: Vec<FetchResult>) {
        let rerender = {
            let Some(page) = self.pages.get_mut(&id) else {
                return;
            };
            let Some(pending) = page.pending_scripts.take() else {
                return;
            };
            for response in results.iter().flatten() {
                for issue in page.cookies.absorb_response(response) {
                    eprintln!("browser cookie rejected: {}", issue.message);
                }
            }
            let preparation = prepare_script_batch(
                page.page.document(),
                &pending.plan,
                results,
                &RuntimeLimits::default(),
            );
            report_script_diagnostics(&preparation.diagnostics);
            page.execute_script_batch(preparation)
        };

        if rerender {
            self.schedule_page_render_for_tab(id);
        }
        self.start_images(id);
        self.start_classic_scripts(id);
        self.repaint_chrome();
        self.request_redraw();
    }

    fn finish_images(&mut self, id: TabId, results: Vec<FetchResult>) {
        let Some(page) = self.pages.get_mut(&id) else {
            return;
        };
        let Some(pending) = page.pending_images.take() else {
            return;
        };
        let application = apply_image_batch(
            page.page.document(),
            &pending.plan,
            results,
            &mut page.images,
            ImageLimits::default(),
        );
        report_image_diagnostics(&application.diagnostics);
        if !application.loaded.is_empty() {
            page.external_styles_generation = page.external_styles_generation.saturating_add(1);
            self.schedule_page_render_for_tab(id);
        }
    }

    fn schedule_page_render_for_tab(&mut self, id: TabId) {
        if id != self.tabs.active_id() {
            return;
        }
        let viewport = self.layout.as_ref().map(|layout| {
            WindowSize::new(
                self.frame_size.width,
                self.frame_size.height.saturating_sub(layout.chrome_height),
            )
        });
        if let Some(viewport) = viewport {
            self.schedule_page_render(id, viewport, false);
        }
    }

    fn has_pending_network(&self) -> bool {
        self.pages.values().any(|page| {
            page.navigation.pending.is_some()
                || page.pending_style_sheets.is_some()
                || page.pending_scripts.is_some()
                || page.pending_images.is_some()
        })
    }

    fn has_pending_script_work(&self) -> bool {
        self.pages.values().any(PageState::has_pending_script_work)
    }

    fn handle_pointer_press(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(menu) = self.address_menu.take() {
            if let Some(item) = menu.item_at(self.cursor)
                && item.enabled
            {
                self.editor.execute(item.command, &mut self.clipboard);
            }
            self.repaint_chrome();
            return;
        }
        let Some((target, scale)) = self
            .layout
            .as_ref()
            .map(|layout| (layout.hit_test(self.cursor), layout.scale))
        else {
            return;
        };
        if target != HitTarget::TitleBar {
            self.title_bar_clicks.reset();
        }
        if target != HitTarget::AddressBar {
            self.address_clicks.reset();
            self.address_selecting = false;
        }
        match target {
            HitTarget::Tab(id) => {
                self.drag = Some(TabDrag::new(id, self.cursor.x));
                self.handle_tab_intent(TabIntent::Activate(id));
            }
            HitTarget::CloseTab(id) => self.handle_tab_intent(TabIntent::Close(id)),
            HitTarget::NewTab => self.handle_tab_intent(TabIntent::New),
            HitTarget::Toolbar(button) => self.emit_navigation(button.navigation_intent()),
            HitTarget::WindowControl(control) => {
                self.handle_window_action(event_loop, control.action());
            }
            HitTarget::TitleBar => {
                let gesture = self.title_bar_clicks.register(
                    Instant::now().duration_since(self.started_at),
                    self.cursor,
                    scale,
                );
                self.editor.set_focused(false);
                if let Some(window) = &self.window {
                    window.set_ime_allowed(false);
                }
                self.repaint_chrome();
                match gesture {
                    TitleBarGesture::BeginDrag => {
                        if let Some(window) = &self.window
                            && let Err(error) = window.drag_window()
                        {
                            eprintln!("render-browser could not begin a window drag: {error}");
                        }
                    }
                    TitleBarGesture::ToggleMaximize => {
                        self.handle_window_action(event_loop, WindowAction::ToggleMaximize);
                    }
                }
            }
            HitTarget::AddressBar => {
                self.content_editor = None;
                let index = self.layout.as_ref().map_or(0, |layout| {
                    address_index_at_x(layout, &self.editor, self.cursor.x, self.fonts.as_ref())
                });
                let is_double_click = self.address_clicks.register(
                    Instant::now().duration_since(self.started_at),
                    self.cursor,
                    scale,
                );
                self.editor.set_focused(true);
                if is_double_click && !self.modifiers.shift_key() {
                    self.editor.select_word_at(index);
                    self.address_selecting = false;
                } else {
                    self.editor
                        .begin_pointer_selection(index, self.modifiers.shift_key());
                    self.address_selecting = true;
                }
                if let Some(window) = &self.window {
                    window.set_ime_allowed(true);
                }
                self.repaint_chrome();
            }
            HitTarget::Content => self.handle_content_press(),
            HitTarget::Chrome => {
                self.content_editor = None;
                self.editor.set_focused(false);
                if let Some(window) = &self.window {
                    window.set_ime_allowed(false);
                }
                self.repaint_chrome();
            }
        }
    }

    fn handle_content_press(&mut self) {
        self.editor.set_focused(false);
        let id = self.tabs.active_id();
        let hit_node = self.content_node_at_cursor();
        if let Some(node) = hit_node
            && let Some(value) = self.content_text_input_value(id, node)
        {
            let mut editor = AddressEditor::new(value);
            editor.set_focused(true);
            self.content_editor = Some(ContentTextEditor {
                tab: id,
                node,
                editor,
            });
            if let Some(window) = &self.window {
                window.set_ime_allowed(true);
            }
            self.repaint_chrome();
            return;
        }
        self.content_editor = None;
        if let Some(window) = &self.window {
            window.set_ime_allowed(false);
        }
        // Give page scripts a chance to observe or cancel the click before
        // any default action (navigation) runs.
        let mut default_allowed = true;
        let click_task = hit_node.and_then(|node| {
            let page = self.pages.get_mut(&id)?;
            page.page.queue_click(node).ok()
        });
        if let Some(task) = click_task
            && let Some(page) = self.pages.get_mut(&id)
        {
            let (_, defaults) = page.run_page_turns();
            default_allowed = defaults.get(&task).copied().unwrap_or(true);
            self.drain_script_navigations(id);
        }
        let navigation = hit_node.and_then(|hit_node| {
            let page = self.pages.get(&id)?;
            get_content_navigation_target(
                page.page.document().dom(),
                hit_node,
                &page.navigation.committed().target.history_url(),
            )
        });
        if let Some(url) = navigation.filter(|_| default_allowed) {
            self.navigate_target(id, NavigationTarget::from_url(url), HistoryMode::Push);
        }
        self.repaint_chrome();
    }

    fn content_text_input_value(
        &self,
        tab: TabId,
        node: render_core::dom::NodeId,
    ) -> Option<String> {
        let dom = self.pages.get(&tab)?.page.document().dom();
        let render_core::dom::NodeKind::Element(element) = dom.node(node)?.kind() else {
            return None;
        };
        if element.local_name != "input" {
            return None;
        }
        let input_type = dom.attribute(node, "type").ok().flatten().unwrap_or("text");
        if !matches!(input_type.to_ascii_lowercase().as_str(), "text" | "search") {
            return None;
        }
        Some(
            dom.attribute(node, "value")
                .ok()
                .flatten()
                .unwrap_or("")
                .to_owned(),
        )
    }

    fn sync_content_editor(&mut self) {
        let Some(content) = self.content_editor.as_ref() else {
            return;
        };
        let tab = content.tab;
        let node = content.node;
        let value = content.editor.text().to_owned();
        if let Some(page) = self.pages.get_mut(&tab)
            && page
                .page
                .document_mut()
                .dom_mut()
                .set_attribute(node, "value", value)
                .is_ok()
            && page.page.queue_input_event(node).is_ok()
        {
            let (rendered, _) = page.run_page_turns();
            if rendered {
                self.schedule_page_render_for_tab(tab);
            }
            self.drain_script_navigations(tab);
            return;
        }
        self.schedule_page_render_for_tab(tab);
    }

    fn content_node_at_cursor(&self) -> Option<render_core::dom::NodeId> {
        let page = self.pages.get(&self.tabs.active_id())?;
        let display_list = page.display_list.as_ref()?;
        hit_test_content_regions(
            display_list.items().iter().map(|item| ContentHitRegion {
                bounds: item.bounds,
                source: item.source,
                coordinate_space: item.coordinate_space,
            }),
            self.cursor,
            self.layout.as_ref()?.chrome_height,
            PhysicalPoint {
                x: 0.0,
                y: page.scroll.offset_y(),
            },
        )
    }

    fn handle_context_menu_press(&mut self) {
        let Some((target, scale)) = self
            .layout
            .as_ref()
            .map(|layout| (layout.hit_test(self.cursor), layout.scale))
        else {
            return;
        };
        self.address_selecting = false;
        if target == HitTarget::AddressBar {
            self.editor.set_focused(true);
            if let Some(window) = &self.window {
                window.set_ime_allowed(true);
            }
            let paste_available = self.clipboard.read_text().is_some();
            self.address_menu = Some(AddressContextMenu::new(
                self.cursor,
                self.frame_size.width,
                self.frame_size.height,
                scale,
                &self.editor,
                paste_available,
            ));
        } else {
            self.address_menu = None;
        }
        self.repaint_chrome();
    }

    fn handle_window_action(&mut self, event_loop: &ActiveEventLoop, action: WindowAction) {
        match action {
            WindowAction::Minimize => {
                if let Some(window) = &self.window {
                    window.set_minimized(true);
                }
            }
            WindowAction::ToggleMaximize => {
                if let Some(window) = &self.window {
                    window.set_maximized(!window.is_maximized());
                }
                self.repaint_chrome();
            }
            WindowAction::Close => event_loop.exit(),
        }
    }

    fn handle_cursor_move(&mut self, position: PhysicalPosition<f64>) {
        self.cursor = Point {
            x: finite_f32(position.x),
            y: finite_f32(position.y),
        };
        let Some(layout) = &self.layout else {
            return;
        };
        self.hot = layout.hit_test(self.cursor);
        let cursor_icon = match self.hot {
            HitTarget::AddressBar => CursorIcon::Text,
            HitTarget::Content => self
                .content_node_at_cursor()
                .and_then(|node| {
                    let page = self.pages.get(&self.tabs.active_id())?;
                    get_content_navigation_target(
                        page.page.document().dom(),
                        node,
                        &page.navigation.committed().target.history_url(),
                    )
                })
                .map_or(CursorIcon::Default, |_| CursorIcon::Pointer),
            HitTarget::Tab(_)
            | HitTarget::CloseTab(_)
            | HitTarget::NewTab
            | HitTarget::Toolbar(_)
            | HitTarget::WindowControl(_) => CursorIcon::Pointer,
            HitTarget::TitleBar | HitTarget::Chrome => CursorIcon::Default,
        };
        if let Some(window) = &self.window {
            window.set_cursor(cursor_icon);
        }
        if self.address_selecting {
            let index =
                address_index_at_x(layout, &self.editor, self.cursor.x, self.fonts.as_ref());
            self.editor.extend_pointer_selection(index);
            self.repaint_chrome();
            return;
        }
        let move_intent = self
            .drag
            .as_mut()
            .and_then(|drag| drag.update(self.cursor.x, layout));
        if let Some(intent) = move_intent {
            self.handle_tab_intent(intent);
        } else {
            self.repaint_chrome();
        }
    }

    fn handle_pointer_release(&mut self) {
        self.drag = None;
        if self.address_selecting {
            self.address_selecting = false;
            self.editor.finish_pointer_selection();
            self.repaint_chrome();
        }
    }

    fn handle_mouse_wheel(&mut self, delta: MouseScrollDelta) {
        if !self
            .layout
            .as_ref()
            .is_some_and(|layout| layout.hit_test(self.cursor) == HitTarget::Content)
        {
            return;
        }
        let delta_y = wheel_document_delta_y(delta);
        let id = self.tabs.active_id();
        let changed = self
            .pages
            .get_mut(&id)
            .is_some_and(|page| page.scroll.scroll_by(delta_y));
        if changed {
            let viewport = self.layout.as_ref().map(|layout| {
                WindowSize::new(
                    self.frame_size.width,
                    self.frame_size.height.saturating_sub(layout.chrome_height),
                )
            });
            if let Some(viewport) = viewport {
                self.schedule_page_render(id, viewport, true);
            }
            self.repaint_chrome();
        }
    }

    fn handle_keyboard(&mut self, event: &winit::event::KeyEvent) {
        if event.state != ElementState::Pressed {
            return;
        }
        if matches!(event.logical_key, Key::Named(NamedKey::Escape))
            && self.address_menu.take().is_some()
        {
            self.repaint_chrome();
            return;
        }
        let menu_was_open = self.address_menu.take().is_some();
        if self.content_editor.is_some() && self.handle_content_keyboard(event) {
            return;
        }
        let control = self.modifiers.control_key();
        let shift = self.modifiers.shift_key();
        if control {
            if key_character_is(&event.logical_key, "l") {
                self.editor.set_focused(true);
                self.editor.select_all();
                if let Some(window) = &self.window {
                    window.set_ime_allowed(true);
                }
                self.repaint_chrome();
                return;
            }
            if key_character_is(&event.logical_key, "t") {
                self.handle_tab_intent(TabIntent::New);
                return;
            }
            if key_character_is(&event.logical_key, "w") {
                self.handle_tab_intent(TabIntent::Close(self.tabs.active_id()));
                return;
            }
            if let Some(command) = self
                .editor
                .is_focused()
                .then(|| address_shortcut(&event.logical_key, shift))
                .flatten()
            {
                self.editor.execute(command, &mut self.clipboard);
                self.repaint_chrome();
                return;
            }
        }
        if !self.editor.is_focused() {
            if menu_was_open {
                self.repaint_chrome();
            }
            self.forward_keydown_to_page(event);
            return;
        }
        match &event.logical_key {
            Key::Named(NamedKey::Enter) => {
                match intent_from_address(self.editor.text()) {
                    Ok(intent) => self.emit_navigation(intent),
                    Err(error) => {
                        let id = self.tabs.active_id();
                        let target = self.pages.get(&id).map_or(NavigationTarget::Home, |page| {
                            page.navigation.committed().target.clone()
                        });
                        if let Some(page) = self.pages.get_mut(&id) {
                            page.cancel_pending();
                        }
                        self.install_source(
                            id,
                            error_source(target, &format!("Invalid address: {error}")),
                            false,
                        );
                    }
                }
                return;
            }
            Key::Named(NamedKey::Escape) => {
                self.sync_active_address();
                self.repaint_chrome();
                return;
            }
            Key::Named(NamedKey::Backspace) => self.editor.backspace(),
            Key::Named(NamedKey::Delete) => self.editor.delete(),
            Key::Named(NamedKey::ArrowLeft) => self.editor.move_left(shift),
            Key::Named(NamedKey::ArrowRight) => self.editor.move_right(shift),
            Key::Named(NamedKey::Home) => self.editor.move_home(shift),
            Key::Named(NamedKey::End) => self.editor.move_end(shift),
            Key::Character(_)
                if !control && !self.modifiers.alt_key() && !self.modifiers.super_key() =>
            {
                if let Some(value) = &event.text {
                    self.editor.insert(value);
                }
            }
            _ => return,
        }
        self.repaint_chrome();
    }

    /// Forward a printable or named key to the page as a trusted `keydown`
    /// event so script can react to the keyboard.
    fn forward_keydown_to_page(&mut self, event: &winit::event::KeyEvent) {
        let Some(key) = page_key_name(event) else {
            return;
        };
        let id = self.tabs.active_id();
        let queued = self
            .pages
            .get_mut(&id)
            .and_then(|page| page.page.queue_keydown(&key).ok());
        if queued.is_some()
            && let Some(page) = self.pages.get_mut(&id)
        {
            let (rendered, _) = page.run_page_turns();
            if rendered {
                self.schedule_page_render_for_tab(id);
            }
            self.drain_script_navigations(id);
        }
    }

    fn handle_content_keyboard(&mut self, event: &winit::event::KeyEvent) -> bool {
        if event.state != ElementState::Pressed {
            return true;
        }
        let shift = self.modifiers.shift_key();
        let control = self.modifiers.control_key();
        let Some(content) = self.content_editor.as_mut() else {
            return false;
        };
        match &event.logical_key {
            Key::Named(NamedKey::Enter) => {
                let tab = content.tab;
                let node = content.node;
                let target = self
                    .pages
                    .get(&tab)
                    .and_then(|page| {
                        plan_form_submission(
                            page.page.document().dom(),
                            node,
                            &page.navigation.committed().target.history_url(),
                        )
                        .ok()
                    })
                    .filter(|submission| submission.method == FormMethod::Get)
                    .map(|submission| submission.target);
                self.content_editor = None;
                if let Some(url) = target {
                    self.navigate_target(tab, NavigationTarget::from_url(url), HistoryMode::Push);
                }
                true
            }
            Key::Named(NamedKey::Backspace) => {
                content.editor.backspace();
                self.sync_content_editor();
                true
            }
            Key::Named(NamedKey::Delete) => {
                content.editor.delete();
                self.sync_content_editor();
                true
            }
            Key::Named(NamedKey::ArrowLeft) => {
                content.editor.move_left(shift);
                true
            }
            Key::Named(NamedKey::ArrowRight) => {
                content.editor.move_right(shift);
                true
            }
            Key::Named(NamedKey::Home) => {
                content.editor.move_home(shift);
                true
            }
            Key::Named(NamedKey::End) => {
                content.editor.move_end(shift);
                true
            }
            Key::Character(_)
                if !control && !self.modifiers.alt_key() && !self.modifiers.super_key() =>
            {
                if let Some(value) = &event.text {
                    content.editor.insert(value);
                    self.sync_content_editor();
                }
                true
            }
            _ => true,
        }
    }

    fn update_window_title(&self) {
        let Some(window) = &self.window else {
            return;
        };
        window.set_title(&format!("{} - rENDER", self.tabs.active().title));
    }

    fn report_and_exit(event_loop: &ActiveEventLoop, operation: &str, error: &dyn fmt::Display) {
        eprintln!("render-browser could not {operation}: {error}");
        event_loop.exit();
    }
}

struct ContentTextEditor {
    tab: TabId,
    node: render_core::dom::NodeId,
    editor: AddressEditor,
}

impl ApplicationHandler<UserEvent> for BrowserApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none()
            && let Err(error) = self.initialize(event_loop)
        {
            Self::report_and_exit(event_loop, "create its native window", error.as_ref());
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if self.window.as_ref().map(|window| window.id()) != Some(window_id) {
            return;
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                self.address_menu = None;
                self.address_selecting = false;
                self.relayout_and_render(true);
                self.request_redraw();
            }
            WindowEvent::ThemeChanged(theme) => {
                self.theme = theme_from_winit(theme);
                self.repaint_chrome();
            }
            WindowEvent::CursorMoved { position, .. } => self.handle_cursor_move(position),
            WindowEvent::MouseWheel { delta, .. } => self.handle_mouse_wheel(delta),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.handle_pointer_press(event_loop),
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => self.handle_pointer_release(),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => self.handle_context_menu_press(),
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::Focused(false) => {
                self.title_bar_clicks.reset();
                self.address_clicks.reset();
                self.address_selecting = false;
                self.address_menu = None;
                self.content_editor = None;
                if let Some(window) = &self.window {
                    window.set_ime_allowed(false);
                }
            }
            WindowEvent::KeyboardInput { event, .. } => self.handle_keyboard(&event),
            WindowEvent::Ime(Ime::Preedit(value, _)) if self.editor.is_focused() => {
                self.editor.set_preedit(value);
                self.repaint_chrome();
            }
            WindowEvent::Ime(Ime::Commit(value)) if self.content_editor.is_some() => {
                if let Some(content) = self.content_editor.as_mut() {
                    content.editor.insert(&value);
                }
                self.sync_content_editor();
                self.repaint_chrome();
            }
            WindowEvent::Ime(Ime::Commit(value)) if self.editor.is_focused() => {
                self.editor.insert(&value);
                self.repaint_chrome();
            }
            WindowEvent::RedrawRequested => {
                if let Err(error) = self.present() {
                    Self::report_and_exit(event_loop, "present the CPU surface", error.as_ref());
                }
            }
            _ => {}
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::RenderReady => self.poll_render_worker(),
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.poll_network();
        let active = self.tabs.active_id();
        let mut rendered_active = false;
        let mut navigation_candidates = Vec::new();
        for (id, page) in &mut self.pages {
            let now = page.created_at.elapsed();
            match page.page.pump_until_idle_reference(now) {
                Ok(outcome) => rendered_active |= *id == active && outcome.rendered_turns > 0,
                Err(error) => eprintln!("render-browser page pump failed: {error}"),
            }
            page.drain_console();
            if !page
                .page
                .runtime_mut()
                .take_pending_navigations()
                .is_empty()
            {
                navigation_candidates.push(*id);
            }
        }
        if rendered_active {
            self.schedule_page_render_for_tab(active);
        }
        for id in navigation_candidates {
            self.drain_script_navigations(id);
        }
        if self.has_pending_network() || self.has_pending_script_work() {
            event_loop.set_control_flow(ControlFlow::WaitUntil(
                Instant::now() + Duration::from_millis(16),
            ));
            return;
        }
        if let Some(wake) = self
            .pages
            .values()
            .filter_map(PageState::next_wake_instant)
            .min()
        {
            // Cap the sleep so far-future timers still poll at a sane rate.
            event_loop.set_control_flow(ControlFlow::WaitUntil(
                wake.min(Instant::now() + Duration::from_millis(250)),
            ));
            return;
        }
        event_loop.set_control_flow(ControlFlow::Wait);
    }
}

fn key_character_is(key: &Key, expected: &str) -> bool {
    matches!(key, Key::Character(value) if value.eq_ignore_ascii_case(expected))
}

/// Map a winit key event to the DOM `KeyboardEvent.key` string it represents.
fn page_key_name(event: &winit::event::KeyEvent) -> Option<String> {
    if let Some(text) = &event.text {
        return Some(text.to_string());
    }
    let Key::Named(named) = &event.logical_key else {
        return None;
    };
    let name = match named {
        NamedKey::Enter => "Enter",
        NamedKey::Backspace => "Backspace",
        NamedKey::Delete => "Delete",
        NamedKey::Escape => "Escape",
        NamedKey::ArrowLeft => "ArrowLeft",
        NamedKey::ArrowRight => "ArrowRight",
        NamedKey::ArrowUp => "ArrowUp",
        NamedKey::ArrowDown => "ArrowDown",
        NamedKey::Home => "Home",
        NamedKey::End => "End",
        NamedKey::Tab => "Tab",
        NamedKey::Space => " ",
        _ => return None,
    };
    Some(name.to_owned())
}

fn wheel_document_delta_y(delta: MouseScrollDelta) -> f32 {
    match delta {
        MouseScrollDelta::LineDelta(_, y) => -y * SCROLL_LINE_PIXELS,
        MouseScrollDelta::PixelDelta(position) => -finite_f32(position.y),
    }
}

fn address_shortcut(key: &Key, shift: bool) -> Option<AddressCommand> {
    if key_character_is(key, "z") {
        Some(if shift {
            AddressCommand::Redo
        } else {
            AddressCommand::Undo
        })
    } else if key_character_is(key, "y") {
        Some(AddressCommand::Redo)
    } else if key_character_is(key, "x") {
        Some(AddressCommand::Cut)
    } else if key_character_is(key, "c") {
        Some(AddressCommand::Copy)
    } else if key_character_is(key, "v") {
        Some(AddressCommand::Paste)
    } else if key_character_is(key, "a") {
        Some(AddressCommand::SelectAll)
    } else {
        None
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "winit coordinates and scale factors are finite and small"
)]
fn finite_f32(value: f64) -> f32 {
    debug_assert!(value.is_finite());
    value as f32
}

const fn theme_from_winit(theme: Theme) -> ChromeTheme {
    match theme {
        Theme::Light => ChromeTheme::Light,
        Theme::Dark => ChromeTheme::Dark,
    }
}

fn blit_page(
    destination: &mut [u32],
    destination_size: WindowSize<u32>,
    source: &[u32],
    source_size: WindowSize<u32>,
    destination_y: u32,
) {
    let copy_width = source_size.width.min(destination_size.width) as usize;
    let copy_height = source_size
        .height
        .min(destination_size.height.saturating_sub(destination_y));
    for row in 0..copy_height {
        let source_start = row as usize * source_size.width as usize;
        let destination_start = (row + destination_y) as usize * destination_size.width as usize;
        destination[destination_start..destination_start + copy_width]
            .copy_from_slice(&source[source_start..source_start + copy_width]);
    }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "native dimensions are bounded far below f32's exact integer range"
)]
fn viewport_dimension(value: u32) -> f32 {
    value as f32
}

fn surface_to_softbuffer(surface: &Surface) -> Vec<u32> {
    surface
        .pixels()
        .iter()
        .map(|color| {
            (u32::from(color.red) << 16) | (u32::from(color.green) << 8) | u32::from(color.blue)
        })
        .collect()
}

fn report_stylesheet_diagnostics(diagnostics: &[StylesheetResourceDiagnostic]) {
    let mut summaries = BTreeMap::<(String, String, String, String), (usize, &str)>::new();
    for diagnostic in diagnostics {
        let url = diagnostic.requested_url.as_ref().map_or("", Url::as_str);
        let feature = diagnostic
            .message
            .rsplit_once(": ")
            .map_or(diagnostic.message.as_str(), |(_, detail)| detail);
        let key = (
            format!("{:?}", diagnostic.severity),
            format!("{:?}", diagnostic.code),
            url.to_owned(),
            feature.to_owned(),
        );
        let summary = summaries.entry(key).or_insert((0, &diagnostic.message));
        summary.0 = summary.0.saturating_add(1);
    }
    for ((severity, code, url, feature), (count, first_message)) in summaries {
        let occurrences = if count == 1 {
            String::new()
        } else {
            format!(" ({count} occurrences)")
        };
        eprintln!(
            "render-browser stylesheet {severity} {code} {url}: {feature}{occurrences}; first: {first_message}"
        );
    }
}

fn report_script_diagnostics(diagnostics: &[ScriptResourceDiagnostic]) {
    for diagnostic in diagnostics {
        let owner = diagnostic
            .owner
            .map_or_else(|| "document".to_owned(), |owner| format!("node {owner:?}"));
        let url = diagnostic
            .requested_url
            .as_ref()
            .map_or("inline", Url::as_str);
        eprintln!(
            "render-browser classic script {:?} {:?} {owner} {url}: {}",
            diagnostic.severity, diagnostic.code, diagnostic.message
        );
    }
}

fn report_script_discovery_diagnostics(diagnostics: &[ScriptDiagnostic]) {
    for diagnostic in diagnostics {
        let owner = diagnostic
            .owner
            .map_or_else(|| "document".to_owned(), |owner| format!("node {owner:?}"));
        let source_order = diagnostic
            .source_order
            .map_or_else(|| "unknown".to_owned(), |order| order.to_string());
        eprintln!(
            "render-browser classic script discovery {:?} {owner} source {source_order}: {}",
            diagnostic.code, diagnostic.message
        );
    }
}

fn report_image_diagnostics(diagnostics: &[render_browser::images::ImageResourceDiagnostic]) {
    for diagnostic in diagnostics {
        let owner = diagnostic
            .owner
            .map_or_else(|| "document".to_owned(), |owner| format!("node {owner:?}"));
        let url = diagnostic
            .requested_url
            .as_ref()
            .map_or("unknown", Url::as_str);
        eprintln!(
            "render-browser image {:?} {:?} {owner} {url}: {}",
            diagnostic.severity, diagnostic.code, diagnostic.message
        );
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use render_core::dom::NodeKind;
    use render_core::html::parse_document;
    use render_core::js::RuntimeLimits;
    use render_core::layout::{PhysicalPoint, PhysicalRect};
    use render_core::navigation::HistoryEntry;
    use render_core::paint::PaintCoordinateSpace;
    use render_core::paint::{Color, Surface};
    use render_core::script::ScriptDiscoveryLimits;
    use render_net::Url;
    use winit::dpi::{PhysicalPosition, PhysicalSize as WindowSize};
    use winit::event::MouseScrollDelta;
    use winit::keyboard::Key;

    use super::{
        ContentHitRegion, HOME_TITLE, PageNavigation, PageSource, PageState, address_shortcut,
        blit_page, get_content_navigation_target, hit_test_content_regions, home_source,
        network_start_source, surface_to_softbuffer, wheel_document_delta_y,
    };
    use render_browser::chrome::Point;
    use render_browser::editor::AddressCommand;
    use render_browser::scripts::{
        plan_classic_scripts, plan_unstarted_classic_scripts, prepare_script_batch,
    };

    #[test]
    fn converts_core_surface_to_softbuffer_rgb_words() {
        let surface = Surface::new(1, 1, Color::rgb(0x12, 0x34, 0x56));
        assert_eq!(surface_to_softbuffer(&surface), [0x0012_3456]);
    }

    #[test]
    fn network_start_source_preserves_the_requested_url() {
        let url = Url::parse("https://www.baidu.com/").expect("valid URL");
        let source = network_start_source(url.clone());

        assert_eq!(
            source.target,
            render_browser::navigation::NavigationTarget::Url(url)
        );
        assert!(source.html.is_empty());
    }

    #[test]
    fn address_control_shortcuts_map_to_shared_edit_commands() {
        assert_eq!(
            address_shortcut(&Key::Character("z".into()), false),
            Some(AddressCommand::Undo)
        );
        assert_eq!(
            address_shortcut(&Key::Character("Z".into()), true),
            Some(AddressCommand::Redo)
        );
        for (key, command) in [
            ("c", AddressCommand::Copy),
            ("x", AddressCommand::Cut),
            ("v", AddressCommand::Paste),
            ("a", AddressCommand::SelectAll),
        ] {
            assert_eq!(
                address_shortcut(&Key::Character(key.into()), false),
                Some(command)
            );
        }
    }

    #[test]
    fn page_blit_starts_below_chrome_and_clips() {
        let mut destination = vec![0; 4 * 4];
        let source = vec![7; 4 * 4];
        blit_page(
            &mut destination,
            WindowSize::new(4, 4),
            &source,
            WindowSize::new(4, 4),
            2,
        );
        assert_eq!(&destination[..8], &[0; 8]);
        assert_eq!(&destination[8..], &[7; 8]);
    }

    #[test]
    fn content_hit_test_accounts_for_chrome_scroll_and_paint_order() {
        let mut dom = render_core::dom::Dom::new();
        let document_source = dom.create_element("div");
        let top_source = dom.create_element("button");
        let regions = [
            ContentHitRegion {
                bounds: PhysicalRect::new(10.0, 140.0, 100.0, 30.0),
                source: Some(document_source),
                coordinate_space: PaintCoordinateSpace::Document,
            },
            ContentHitRegion {
                bounds: PhysicalRect::new(10.0, 140.0, 100.0, 30.0),
                source: Some(top_source),
                coordinate_space: PaintCoordinateSpace::Document,
            },
        ];

        assert_eq!(
            hit_test_content_regions(
                regions.into_iter(),
                Point { x: 20.0, y: 90.0 },
                60,
                PhysicalPoint { x: 0.0, y: 120.0 },
            ),
            Some(top_source)
        );
        assert_eq!(
            hit_test_content_regions(
                regions.into_iter(),
                Point { x: 20.0, y: 59.0 },
                60,
                PhysicalPoint { x: 0.0, y: 120.0 },
            ),
            None
        );
    }

    #[test]
    fn submit_descendant_builds_get_navigation_target() {
        let document = parse_document(
            "<form action='/s'><input name='wd' value='small browser'><button id='go' name='from' value='render'><span>Search</span></button></form>",
        );
        let dom = &document.dom;
        let mut pending = vec![dom.document()];
        let hit_node = loop {
            let node = pending.pop().expect("submit text exists");
            if matches!(dom.node(node).map(render_core::dom::Node::kind), Some(NodeKind::Text(text)) if text == "Search")
            {
                break node;
            }
            pending.extend(dom.children(node).unwrap_or_default().iter().rev());
        };
        let target = get_content_navigation_target(
            dom,
            hit_node,
            &Url::parse("https://www.baidu.com/").expect("valid base URL"),
        )
        .expect("GET submit navigation");

        assert_eq!(
            target.as_str(),
            "https://www.baidu.com/s?wd=small+browser&from=render"
        );
    }

    #[test]
    fn get_submission_reads_the_live_text_input_value() {
        let mut document = parse_document(
            "<form action='/s'><input id=query type=search name=wd value=old><button id=go>Search</button></form>",
        );
        let mut pending = vec![document.dom.document()];
        let mut query = None;
        let mut submit = None;
        while let Some(node) = pending.pop() {
            if document.dom.attribute(node, "id").ok().flatten() == Some("query") {
                query = Some(node);
            }
            if document.dom.attribute(node, "id").ok().flatten() == Some("go") {
                submit = Some(node);
            }
            pending.extend(document.dom.children(node).unwrap_or_default().iter().rev());
        }
        document
            .dom
            .set_attribute(query.expect("query input"), "value", "实时 搜索")
            .expect("live value mutation");

        let target = get_content_navigation_target(
            &document.dom,
            submit.expect("submit button"),
            &Url::parse("https://www.baidu.com/").expect("valid base URL"),
        )
        .expect("GET submit navigation");

        assert_eq!(
            target.as_str(),
            "https://www.baidu.com/s?wd=%E5%AE%9E%E6%97%B6+%E6%90%9C%E7%B4%A2"
        );
    }

    #[test]
    fn link_descendant_resolves_against_document_url() {
        let document = parse_document("<a href='/video/next'><span>Next</span></a>");
        let dom = &document.dom;
        let mut pending = vec![dom.document()];
        let hit_node = loop {
            let node = pending.pop().expect("link text exists");
            if matches!(dom.node(node).map(render_core::dom::Node::kind), Some(NodeKind::Text(text)) if text == "Next")
            {
                break node;
            }
            pending.extend(dom.children(node).unwrap_or_default().iter().rev());
        };

        let target = get_content_navigation_target(
            dom,
            hit_node,
            &Url::parse("https://example.test/current/page").expect("valid base URL"),
        )
        .expect("link navigation");

        assert_eq!(target.as_str(), "https://example.test/video/next");
    }

    #[test]
    fn page_states_own_independent_session_histories() {
        let mut first = PageState::new(home_source());
        let second = PageState::new(home_source());
        first
            .history
            .push(HistoryEntry::new(
                Url::parse("https://example.test/").expect("valid test URL"),
            ))
            .expect("history push");

        assert_eq!(first.history.len(), 2);
        assert_eq!(second.history.len(), 1);
        assert_eq!(
            first.history.current().url.as_str(),
            "https://example.test/"
        );
        assert_eq!(second.history.current().url.as_str(), "render://home");
    }

    #[test]
    fn wheel_deltas_map_to_document_scroll_direction() {
        assert!(
            (wheel_document_delta_y(MouseScrollDelta::LineDelta(0.0, -2.0)) - 80.0).abs()
                < f32::EPSILON
        );
        assert!(
            (wheel_document_delta_y(MouseScrollDelta::PixelDelta(PhysicalPosition::new(
                0.0, 12.5
            ))) + 12.5)
                .abs()
                < f32::EPSILON
        );
    }

    #[test]
    fn committed_navigation_resets_only_that_tabs_scroll_state() {
        let mut first = PageState::new(home_source());
        let mut second = PageState::new(home_source());
        first.scroll.update_metrics(1_000.0, 300.0);
        second.scroll.update_metrics(1_000.0, 300.0);
        assert!(first.scroll.scroll_by(240.0));
        assert!(second.scroll.scroll_by(120.0));

        first.set_source(PageSource {
            html: "<!doctype html><title>Next</title>".into(),
            title: "Next".into(),
            target: render_browser::navigation::NavigationTarget::Home,
        });

        assert!(first.scroll.offset_y().abs() < f32::EPSILON);
        assert!((second.scroll.offset_y() - 120.0).abs() < f32::EPSILON);
    }

    #[test]
    fn prepared_scripts_mutate_the_persistent_page_document() {
        let mut page = PageState::new(PageSource {
            html: "<p id=message>before</p><script>var prefix = 'after';</script><script>document.getElementById('message').textContent = prefix;</script>".into(),
            title: "Scripts".into(),
            target: render_browser::navigation::NavigationTarget::Home,
        });
        let base_url = page.navigation.committed().target.history_url();
        let plan = plan_classic_scripts(
            page.page.document(),
            &base_url,
            ScriptDiscoveryLimits::default(),
        );
        let preparation = prepare_script_batch(
            page.page.document(),
            &plan,
            Vec::new(),
            &RuntimeLimits::default(),
        );

        assert!(page.execute_script_batch(preparation));
        let dom = page.page.document().dom();
        let mut pending = vec![dom.document()];
        let message = loop {
            let node = pending.pop().expect("message element exists");
            if matches!(
                dom.node(node).map(render_core::dom::Node::kind),
                Some(NodeKind::Element(_))
            ) && dom.attribute(node, "id").expect("element lookup succeeds") == Some("message")
            {
                break node;
            }
            pending.extend(dom.children(node).unwrap_or_default().iter().rev());
        };
        let text = page
            .page
            .document()
            .dom()
            .children(message)
            .expect("children")[0];
        assert!(matches!(
            page.page
                .document()
                .dom()
                .node(text)
                .map(render_core::dom::Node::kind),
            Some(NodeKind::Text(value)) if value == "after"
        ));
    }

    #[test]
    fn committed_page_target_is_visible_as_script_location() {
        let mut page = PageState::new(PageSource {
            html: "<!doctype html><p></p>".into(),
            title: "Location".into(),
            target: render_browser::navigation::NavigationTarget::Url(
                Url::parse("https://example.test/app/index.html?q=1#view").expect("page URL"),
            ),
        });
        page.page
            .queue_script("location.href;")
            .expect("script should queue");
        let turn = page
            .page
            .run_one_turn_reference()
            .expect("turn should run")
            .expect("script turn");
        assert_eq!(
            turn.executions[0]
                .result
                .as_ref()
                .expect("script should execute")
                .value,
            render_core::js::JsValue::String(
                "https://example.test/app/index.html?q=1#view".to_owned()
            )
        );
    }

    #[test]
    fn executed_bootstrap_script_exposes_inserted_external_script_to_follow_up_scan() {
        let mut page = PageState::new(PageSource {
            html: "<main id=host></main><script>const chunk = document.createElement('script'); chunk.setAttribute('src', 'assets/chunk.js'); document.getElementById('host').appendChild(chunk);</script>".into(),
            title: "Dynamic scripts".into(),
            target: render_browser::navigation::NavigationTarget::Url(
                Url::parse("https://example.test/app/index.html").expect("page URL"),
            ),
        });
        let base_url = page.navigation.committed().target.history_url();
        let initial = plan_classic_scripts(
            page.page.document(),
            &base_url,
            ScriptDiscoveryLimits::default(),
        );
        let started = initial.owners().collect::<HashSet<_>>();
        let preparation = prepare_script_batch(
            page.page.document(),
            &initial,
            Vec::new(),
            &RuntimeLimits::default(),
        );

        assert!(page.execute_script_batch(preparation));
        let follow_up = plan_unstarted_classic_scripts(
            page.page.document(),
            &base_url,
            ScriptDiscoveryLimits::default(),
            &started,
            true,
        );

        assert_eq!(follow_up.resources.len(), 1);
        assert_eq!(
            follow_up.resources[0].request.url.as_str(),
            "https://example.test/app/assets/chunk.js"
        );
    }

    #[test]
    fn pending_navigation_keeps_committed_page_until_commit() {
        let mut navigation = PageNavigation::<()>::new(home_source());
        let pending_url = Url::parse("https://example.test/pending").expect("valid URL");
        navigation.begin(pending_url.clone(), ());

        assert_eq!(navigation.committed().title, HOME_TITLE);
        assert_eq!(
            navigation.committed().target,
            render_browser::navigation::NavigationTarget::Home
        );
        assert_eq!(navigation.pending_url(), Some(&pending_url));

        let committed_url = Url::parse("https://example.test/committed").expect("valid URL");
        navigation.commit(PageSource {
            html: "<title>Committed</title>".into(),
            title: "Committed".into(),
            target: render_browser::navigation::NavigationTarget::Url(committed_url.clone()),
        });

        assert_eq!(navigation.pending_url(), None);
        assert_eq!(navigation.committed().title, "Committed");
        assert_eq!(
            navigation.committed().target,
            render_browser::navigation::NavigationTarget::Url(committed_url)
        );
    }
}
