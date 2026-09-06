//! Native browser shell for the self-owned Rust rendering pipeline.

#![allow(clippy::cast_precision_loss)]

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

use render_browser::cache::disk::{DiskCacheEvent, DiskCacheOperationId, DiskCacheWorker};
use render_browser::cache::{CacheEpoch, CacheLookup, HttpCache};
use render_browser::chrome::{
    AddressClickTracker, AddressContextMenu, Canvas, ChromeLayout, ChromeTheme, HitTarget, Point,
    TabDrag, TitleBarClickTracker, TitleBarGesture, WindowAction, address_index_at_x,
    paint_address_context_menu, paint_chrome,
};
use render_browser::editor::{AddressCommand, AddressEditor, Clipboard, NativeClipboard};
use render_browser::font_backend::SystemFontBackend;
use render_browser::home::{HOME_HTML, HOME_TITLE};
use render_browser::images::{
    ImageFetchPlan, apply_image_batch, plan_images_with_styles_and_context,
};
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
use render_browser::settings::{
    CacheClearUiState, SETTINGS_TITLE, is_trusted_clear_http_cache_action, settings_html,
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
use render_core::image::{ImageLimits, ImageResources, ImageSelectionContext, ImageSource};
use render_core::interaction::{
    ButtonBehavior, DefaultActionKind, FormMethod, activation_plan, plan_form_submission,
};
use render_core::js::{ElementRect, JsValue, RuntimeLimits};
use render_core::layout::{FragmentKind, PhysicalPoint, PhysicalRect, PhysicalSize};
use render_core::navigation::{HistoryEntry, NavigationLimits, SessionHistory};
use render_core::page::{Page, PageDomEvent, PageJob};
use render_core::paint::{
    Color, CpuRasterizer, DisplayCommand, DisplayList, PaintCoordinateSpace, PaintScene,
    RasterControl, RasterRequest, Surface,
};
use render_core::script::{ScriptDiagnostic, ScriptDiscoveryLimits, ScriptScheduling};
use render_net::{
    CookieJar, FetchConfig, FetchError, FetchRequest, FetchResponse, FetchResult, HttpTransport,
    NetworkWorker, RequestHandle, Url,
};
use softbuffer::{Context, Rect as SoftBufferRect, Surface as WindowSurface};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize as WindowSize};
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{CursorIcon, Theme, Window, WindowId};

const INITIAL_WIDTH: u32 = 1_180;
const INITIAL_HEIGHT: u32 = 780;
const SCROLL_LINE_PIXELS: f32 = 40.0;
const ACTIVE_PAGE_TURN_BUDGET: usize = 8;
const BACKGROUND_PAGE_TURN_BUDGET: usize = 2;
const MAX_DAMAGE_RECTS: usize = 16;
const DAMAGE_FULL_THRESHOLD_PERCENT: u64 = 75;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FrameRect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

impl FrameRect {
    fn right(self) -> u32 {
        self.x.saturating_add(self.width)
    }

    fn bottom(self) -> u32 {
        self.y.saturating_add(self.height)
    }

    fn area(self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }

    fn touches_or_overlaps(self, other: Self) -> bool {
        self.x <= other.right()
            && other.x <= self.right()
            && self.y <= other.bottom()
            && other.y <= self.bottom()
    }

    fn union(self, other: Self) -> Self {
        let right = self.right().max(other.right());
        let bottom = self.bottom().max(other.bottom());
        Self {
            x: self.x.min(other.x),
            y: self.y.min(other.y),
            width: right.saturating_sub(self.x.min(other.x)),
            height: bottom.saturating_sub(self.y.min(other.y)),
        }
    }

    fn clip(self, width: u32, height: u32) -> Option<Self> {
        let right = self.right().min(width);
        let bottom = self.bottom().min(height);
        (self.x < right && self.y < bottom).then_some(Self {
            x: self.x,
            y: self.y,
            width: right.saturating_sub(self.x),
            height: bottom.saturating_sub(self.y),
        })
    }
}

#[derive(Clone, Debug, Default)]
struct FrameDamage {
    full: bool,
    rects: Vec<FrameRect>,
}

impl FrameDamage {
    fn mark_full(&mut self) {
        self.full = true;
        self.rects.clear();
    }

    fn mark_rect(&mut self, rect: FrameRect, frame_width: u32, frame_height: u32) {
        if self.full || frame_width == 0 || frame_height == 0 {
            return;
        }
        let Some(mut merged) = rect.clip(frame_width, frame_height) else {
            return;
        };
        let mut index = 0;
        while index < self.rects.len() {
            if self.rects[index].touches_or_overlaps(merged) {
                merged = self.rects[index].union(merged);
                self.rects.swap_remove(index);
            } else {
                index += 1;
            }
        }
        self.rects.push(merged);
        let damaged_area = self.rects.iter().map(|item| item.area()).sum::<u64>();
        let frame_area = u64::from(frame_width) * u64::from(frame_height);
        if self.rects.len() > MAX_DAMAGE_RECTS
            || damaged_area.saturating_mul(100)
                >= frame_area.saturating_mul(DAMAGE_FULL_THRESHOLD_PERCENT)
        {
            self.mark_full();
        }
    }

    fn take_for_present(&mut self, frame_width: u32, frame_height: u32) -> Vec<SoftBufferRect> {
        if frame_width == 0 || frame_height == 0 {
            self.full = false;
            self.rects.clear();
            return Vec::new();
        }
        if !self.full && self.rects.is_empty() {
            return Vec::new();
        }
        let rects = if self.full || self.rects.is_empty() {
            vec![SoftBufferRect {
                x: 0,
                y: 0,
                width: NonZeroU32::new(frame_width).expect("frame width is non-zero"),
                height: NonZeroU32::new(frame_height).expect("frame height is non-zero"),
            }]
        } else {
            self.rects
                .iter()
                .filter_map(|rect| {
                    Some(SoftBufferRect {
                        x: rect.x,
                        y: rect.y,
                        width: NonZeroU32::new(rect.width)?,
                        height: NonZeroU32::new(rect.height)?,
                    })
                })
                .collect()
        };
        self.full = false;
        self.rects.clear();
        rects
    }
}

#[derive(Clone, Copy)]
struct ContentHitRegion {
    bounds: PhysicalRect,
    source: Option<render_core::dom::NodeId>,
    coordinate_space: PaintCoordinateSpace,
    hit_testable: bool,
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
        if !region.hit_testable {
            return None;
        }
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

/// Structural paint commands (clips, transforms, stacking contexts) carry the
/// full bounds of the subtree they open, so they would otherwise shadow the
/// content items painted inside them during a reverse paint-order scan.
fn is_content_hit_command(command: &DisplayCommand) -> bool {
    !matches!(
        command,
        DisplayCommand::PushClip(_)
            | DisplayCommand::PopClip
            | DisplayCommand::PushTransform(_)
            | DisplayCommand::PopTransform
            | DisplayCommand::PushStackingContext(_)
            | DisplayCommand::PopStackingContext
    )
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
    #[cfg(target_os = "macos")]
    {
        if let Err(message) = browser_main() {
            eprintln!("error: {message}");
            std::process::exit(1);
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
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
}

fn browser_main() -> Result<(), String> {
    #[cfg(target_os = "windows")]
    use winit::platform::windows::EventLoopBuilderExtWindows as _;
    let Some(initial) = load_initial_page().map_err(|error| error.to_string())? else {
        return Ok(());
    };
    let fonts = Arc::new(SystemFontBackend::load().map_err(|error| error.to_string())?);
    let event_loop = {
        let mut builder = EventLoop::<UserEvent>::with_user_event();
        // The event loop lives on our dedicated big-stack thread for the
        // whole program lifetime; no other thread touches it.
        #[cfg(target_os = "windows")]
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

/// A request handle that can resolve immediately from the private browser
/// cache or asynchronously from the bounded transport worker.
#[derive(Debug)]
enum CachedRequestState {
    Ready(Box<Option<FetchResult>>),
    Pending(RequestHandle<FetchResult>),
}

/// Request metadata travels with its response so a late completion can only
/// update the cache generation that originally submitted it.
#[derive(Debug)]
struct CachedRequestHandle {
    request: FetchRequest,
    epoch: CacheEpoch,
    state: CachedRequestState,
}

impl CachedRequestHandle {
    fn ready(request: FetchRequest, epoch: CacheEpoch, response: FetchResponse) -> Self {
        Self {
            request,
            epoch,
            state: CachedRequestState::Ready(Box::new(Some(Ok(response)))),
        }
    }

    fn pending(
        request: FetchRequest,
        epoch: CacheEpoch,
        handle: RequestHandle<FetchResult>,
    ) -> Self {
        Self {
            request,
            epoch,
            state: CachedRequestState::Pending(handle),
        }
    }

    fn cancel(&self) {
        if let CachedRequestState::Pending(handle) = &self.state {
            handle.cancel();
        }
    }

    fn try_recv(&mut self) -> Result<CachedFetchResult, TryRecvError> {
        let result = match &mut self.state {
            CachedRequestState::Ready(value) => value.take().ok_or(TryRecvError::Disconnected)?,
            CachedRequestState::Pending(handle) => match handle.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Empty) => return Err(TryRecvError::Empty),
                Err(TryRecvError::Disconnected) => Err(FetchError::WorkerStopped),
            },
        };
        Ok(CachedFetchResult {
            request: self.request.clone(),
            epoch: self.epoch,
            from_cache: matches!(self.state, CachedRequestState::Ready(_)),
            result,
        })
    }
}

#[derive(Debug)]
struct CachedBatchHandle {
    handles: Vec<Option<CachedRequestHandle>>,
    results: Vec<Option<CachedFetchResult>>,
}

impl CachedBatchHandle {
    fn new(handles: Vec<CachedRequestHandle>) -> Self {
        let len = handles.len();
        Self {
            handles: handles.into_iter().map(Some).collect(),
            results: std::iter::repeat_with(|| None).take(len).collect(),
        }
    }

    fn cancel(&self) {
        for handle in self.handles.iter().flatten() {
            handle.cancel();
        }
    }

    fn try_recv(&mut self) -> Result<Vec<CachedFetchResult>, TryRecvError> {
        let mut pending = false;
        for index in 0..self.handles.len() {
            let Some(handle) = self.handles[index].as_mut() else {
                continue;
            };
            match handle.try_recv() {
                Ok(result) => {
                    self.results[index] = Some(result);
                    self.handles[index] = None;
                }
                Err(TryRecvError::Empty) => pending = true,
                Err(TryRecvError::Disconnected) => unreachable!("cache handle maps disconnects"),
            }
        }
        if pending {
            return Err(TryRecvError::Empty);
        }
        Ok(self
            .results
            .iter_mut()
            .map(|result| result.take().expect("completed batch result"))
            .collect())
    }
}

fn load_initial_page() -> Result<Option<PageSource>, Box<dyn Error>> {
    let mut arguments = env::args_os().skip(1);
    let Some(argument) = arguments.next() else {
        return Ok(Some(home_source()));
    };
    if argument == "-h" || argument == "--help" {
        println!(
            "Usage: render-browser [URL_OR_LOCAL_HTML_PATH]\n\nNo argument opens the built-in home page. HTTP, HTTPS, and data: URLs use the browser's normal navigation pipeline."
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
        && matches!(url.scheme(), "http" | "https" | "data")
    {
        return Ok(Some(network_start_source(url)));
    }
    let path = PathBuf::from(argument);
    if path.to_string_lossy().contains("://") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the command-line URL must use HTTP, HTTPS, or data",
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

fn settings_source(state: CacheClearUiState) -> PageSource {
    PageSource {
        html: settings_html(state),
        title: SETTINGS_TITLE.to_owned(),
        target: NavigationTarget::Settings,
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
        scene: Arc<PaintScene>,
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
    paint_scene: Option<Arc<PaintScene>>,
    raster_background: Color,
    content_height: f32,
    viewport_height: f32,
    applied_style_sheets: Option<ExternalStyleSheets>,
    style_plan: Option<StylesheetFetchPlan>,
    style_diagnostics: Vec<StylesheetResourceDiagnostic>,
    computed_styles: Option<BTreeMap<render_core::dom::NodeId, ComputedStyle>>,
    geometry: Option<BTreeMap<u64, ElementRect>>,
    document_revision: u64,
}

type PageRenderWorker = RenderWorker<PageRenderPayload, PageRenderFrame>;

struct BrowserRasterControl<'a> {
    cancellation: &'a RenderCancellation,
}

impl RasterControl for BrowserRasterControl<'_> {
    fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }
}

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
            let geometry = geometry_from_layout(&output.layout.fragments);
            if env::var_os("RENDER_DEBUG_FRAME").is_some() {
                eprintln!(
                    "render-browser render stylesheets={} computed_styles={} fragments={} diagnostics={{document:{}, style:{}, layout:{}, display:{}, raster:{}}}",
                    style_sheets.len(),
                    output.styles.len(),
                    output.layout.fragments.iter().count(),
                    output.diagnostics.document.len(),
                    output.diagnostics.style_sheets.len(),
                    output.diagnostics.layout.len(),
                    output.diagnostics.display_list.len(),
                    output.diagnostics.raster.len(),
                );
                for (node, style) in output.styles.iter().take(16) {
                    eprintln!(
                        "render-browser style node={:?} display={:?} width={:?} height={:?} properties={}",
                        node,
                        style
                            .get("display")
                            .map(render_core::css::computed::ComputedValue::css_text),
                        style
                            .get("width")
                            .map(render_core::css::computed::ComputedValue::css_text),
                        style
                            .get("height")
                            .map(render_core::css::computed::ComputedValue::css_text),
                        style.properties().len(),
                    );
                }
            }
            let display_list = Arc::new(output.display.list);
            let paint_scene = Arc::new(PaintScene::from_shared_display_list(Arc::clone(
                &display_list,
            )));
            let frame = surface_to_softbuffer(&output.raster.surface);
            Ok(PageRenderFrame {
                frame,
                viewport,
                display_list: Some(display_list),
                paint_scene: Some(paint_scene),
                raster_background,
                content_height,
                viewport_height,
                applied_style_sheets,
                style_plan,
                style_diagnostics,
                computed_styles: Some(output.styles),
                geometry: Some(geometry),
                document_revision: output.revision.as_u64(),
            })
        }
        PageRenderPayload::RetainedRaster {
            scene,
            images,
            raster_background,
            content_height,
            viewport_height,
        } => {
            let request = RasterRequest::new(&scene, raster_background, fonts)
                .with_images(&images)
                .with_viewport_origin(PhysicalPoint {
                    x: job.identity.scroll_offset.x,
                    y: job.identity.scroll_offset.y,
                });
            let raster = CpuRasterizer
                .rasterize_request(request, &BrowserRasterControl { cancellation })
                .map_err(|_| RenderFailure::Cancelled)?;
            Ok(PageRenderFrame {
                viewport: WindowSize::new(raster.surface.width(), raster.surface.height()),
                frame: surface_to_softbuffer(&raster.surface),
                display_list: None,
                paint_scene: None,
                raster_background,
                content_height,
                viewport_height,
                applied_style_sheets: None,
                style_plan: None,
                style_diagnostics: Vec::new(),
                computed_styles: None,
                geometry: None,
                document_revision: job.identity.dom_revision,
            })
        }
    }
}

struct PageState {
    navigation: PageNavigation<CachedRequestHandle>,
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
    paint_scene: Option<Arc<PaintScene>>,
    geometry: BTreeMap<u64, ElementRect>,
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
    handle: CachedBatchHandle,
}

struct PendingScripts {
    plan: ScriptFetchPlan,
    handle: CachedBatchHandle,
}

struct PendingImages {
    plan: ImageFetchPlan,
    handle: CachedBatchHandle,
}

#[derive(Debug)]
struct CachedFetchResult {
    request: FetchRequest,
    epoch: CacheEpoch,
    from_cache: bool,
    result: FetchResult,
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
        let page = Page::with_url_unrendered(&source.html, &source.target.history_url());
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

    fn set_source(&mut self, source: PageSource) {
        self.cancel_style_sheets();
        self.cancel_scripts();
        self.page = Page::with_url_unrendered(&source.html, &source.target.history_url());
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
        self.paint_scene = None;
        self.geometry.clear();
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

    /// Print buffered `console.*` output from the page's script runtime.
    fn drain_console(&mut self) {
        for message in self.page.runtime_mut().take_console_messages() {
            eprintln!("[console.{}] {}", message.level.label(), message.text);
        }
    }

    /// Advance the page's virtual clock to real elapsed time and run every
    /// ready turn (timers, queued events, scripts).
    ///
    /// Returns whether any turn mutated the DOM plus per-task default-action
    /// results (`true` when the task's event was not `preventDefault()`-ed).
    fn run_page_turns(&mut self) -> (bool, HashMap<render_core::event_loop::TaskId, bool>) {
        self.run_page_turns_with_budget(ACTIVE_PAGE_TURN_BUDGET)
    }

    fn run_page_turns_with_budget(
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
        (revision_after != revision_before, defaults)
    }

    /// Earliest wall-clock instant at which this page needs a wake-up.
    fn next_wake_instant(&self) -> Option<Instant> {
        self.page
            .next_wake_deadline()
            .map(|deadline| self.created_at + deadline)
    }

    /// Whether the page still has script work that requires future turns.
    fn has_pending_script_work(&self) -> bool {
        self.page.has_pending_immediate_work()
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
    http_cache: HttpCache,
    disk_cache: Option<DiskCacheWorker>,
    pending_disk_clear: Option<DiskCacheOperationId>,
    cache_clear_state: CacheClearUiState,
    editor: AddressEditor,
    content_editor: Option<ContentTextEditor>,
    clipboard: NativeClipboard,
    window: Option<Arc<Window>>,
    context: Option<Context<Arc<Window>>>,
    surface: Option<NativeSurface>,
    layout: Option<ChromeLayout>,
    frame: Vec<u32>,
    frame_size: WindowSize<u32>,
    frame_damage: FrameDamage,
    theme: ChromeTheme,
    cursor: Point,
    hot: HitTarget,
    cursor_icon: CursorIcon,
    drag: Option<TabDrag>,
    address_selecting: bool,
    address_menu: Option<AddressContextMenu>,
    modifiers: ModifiersState,
    title_bar_clicks: TitleBarClickTracker,
    address_clicks: AddressClickTracker,
    left_pointer_down: bool,
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
        let disk_cache = match render_browser::cache::disk::DiskCacheConfig::from_environment() {
            Ok(config) => match DiskCacheWorker::start(config) {
                Ok(worker) => Some(worker),
                Err(error) => {
                    eprintln!("render-browser disk cache disabled: {error}");
                    None
                }
            },
            Err(error) => {
                eprintln!("render-browser disk cache disabled: {error}");
                None
            }
        };
        Self {
            tabs,
            pages: HashMap::from([(active, PageState::new(initial))]),
            fonts,
            render_worker,
            network,
            http_cache: HttpCache::default(),
            disk_cache,
            pending_disk_clear: None,
            cache_clear_state: CacheClearUiState::Ready,
            editor,
            content_editor: None,
            clipboard: NativeClipboard::default(),
            window: None,
            context: None,
            surface: None,
            layout: None,
            frame: Vec::new(),
            frame_size: WindowSize::new(0, 0),
            frame_damage: FrameDamage {
                full: true,
                rects: Vec::new(),
            },
            theme: ChromeTheme::Light,
            cursor: Point::default(),
            hot: HitTarget::Chrome,
            cursor_icon: CursorIcon::Default,
            drag: None,
            address_selecting: false,
            address_menu: None,
            modifiers: ModifiersState::default(),
            title_bar_clicks: TitleBarClickTracker::default(),
            address_clicks: AddressClickTracker::default(),
            left_pointer_down: false,
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
                NavigationTarget::Url(url) if matches!(url.scheme(), "http" | "https" | "data") => {
                    Some(url)
                }
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
        self.frame_damage.mark_full();
        let layout = ChromeLayout::new(size.width, size.height, scale, self.tabs.tabs());
        if render_page {
            let viewport =
                WindowSize::new(size.width, size.height.saturating_sub(layout.chrome_height));
            self.schedule_page_render(self.tabs.active_id(), viewport, false);
        }
        self.layout = Some(layout);
        self.mark_chrome_damage(size);
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
            && page.paint_scene.is_some();
        let payload = if can_raster_retained {
            PageRenderPayload::RetainedRaster {
                scene: Arc::clone(
                    page.paint_scene
                        .as_ref()
                        .expect("retained paint scene was checked"),
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
        log_completed_frame_debug(&frame, completed.identity.tab_id);
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
            if let Some(paint_scene) = frame.paint_scene {
                page.paint_scene = Some(paint_scene);
            }
            if let Some(geometry) = frame.geometry.clone() {
                page.geometry = geometry;
            }
            page.raster_background = frame.raster_background;
            if let Some(styles) = frame.computed_styles {
                page.computed_styles = styles;
            }
            page.scroll
                .update_metrics(frame.content_height, frame.viewport_height);
            page.page.runtime_mut().install_viewport(
                page.viewport.width as f32,
                page.viewport.height as f32,
                0.0,
                page.scroll.offset_y(),
            );
            if let Some(geometry) = frame.geometry {
                let _published = page
                    .page
                    .publish_render_geometry(frame.document_revision, geometry);
            }
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
            if let Some(size) = self.window.as_ref().map(|window| window.inner_size()) {
                self.mark_page_damage(size);
                self.compose_frame(size);
            }
            self.request_redraw();
        } else {
            self.repaint_chrome();
        }
    }

    fn compose_frame(&mut self, size: WindowSize<u32>) {
        if self.frame_size != size {
            self.frame_damage.mark_full();
        }
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
        dump_debug_frame(&self.frame, size);
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
        let size = window.inner_size();
        self.mark_chrome_damage(size);
        self.compose_frame(size);
        self.request_redraw();
    }

    fn mark_page_damage(&mut self, size: WindowSize<u32>) {
        let Some(chrome_height) = self.layout.as_ref().map(|layout| layout.chrome_height) else {
            self.frame_damage.mark_full();
            return;
        };
        self.frame_damage.mark_rect(
            FrameRect {
                x: 0,
                y: chrome_height,
                width: size.width,
                height: size.height.saturating_sub(chrome_height),
            },
            size.width,
            size.height,
        );
    }

    fn mark_chrome_damage(&mut self, size: WindowSize<u32>) {
        let chrome_height = self
            .layout
            .as_ref()
            .map_or(size.height, |layout| layout.chrome_height);
        self.frame_damage.mark_rect(
            FrameRect {
                x: 0,
                y: 0,
                width: size.width,
                height: chrome_height.min(size.height),
            },
            size.width,
            size.height,
        );
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
        if buffer.age() != 1 {
            self.frame_damage.mark_full();
        }
        let damage = self
            .frame_damage
            .take_for_present(self.frame_size.width, self.frame_size.height);
        if damage.is_empty() {
            return Ok(());
        }
        let full_damage = damage.len() == 1
            && damage[0].x == 0
            && damage[0].y == 0
            && damage[0].width.get() == self.frame_size.width
            && damage[0].height.get() == self.frame_size.height;
        if full_damage {
            buffer.copy_from_slice(&self.frame);
            buffer.present()?;
        } else {
            copy_frame_regions(&mut buffer, &self.frame, self.frame_size, &damage);
            buffer.present_with_damage(&damage)?;
        }
        Ok(())
    }

    fn handle_tab_intent(&mut self, intent: TabIntent) {
        if matches!(intent, TabIntent::Close(_)) {
            self.drag = None;
        }
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
            NavigationIntent::Settings => {
                self.navigate_target(tab, NavigationTarget::Settings, HistoryMode::Push);
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
            NavigationTarget::Settings => {
                self.install_source(id, settings_source(self.cache_clear_state), false);
            }
            NavigationTarget::Url(url) if matches!(url.scheme(), "http" | "https" | "data") => {
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
        let handle = self.submit_cached_fetch(request);
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

    fn submit_cached_fetch(&mut self, request: FetchRequest) -> CachedRequestHandle {
        let epoch = self.http_cache.epoch();
        let now = Instant::now();
        match self.http_cache.lookup(&request, now) {
            CacheLookup::Hit(response) => CachedRequestHandle::ready(request, epoch, *response),
            CacheLookup::Miss => {
                let submitted_request = self
                    .http_cache
                    .revalidation_request(&request, now)
                    .unwrap_or_else(|| request.clone());
                let handle = self.network.submit(submitted_request.clone());
                CachedRequestHandle::pending(submitted_request, epoch, handle)
            }
        }
    }

    fn submit_cached_batch(&mut self, requests: Vec<FetchRequest>) -> CachedBatchHandle {
        CachedBatchHandle::new(
            requests
                .into_iter()
                .map(|request| self.submit_cached_fetch(request))
                .collect(),
        )
    }

    fn finish_cached_fetch(&mut self, completion: CachedFetchResult) -> FetchResult {
        let CachedFetchResult {
            request,
            epoch,
            from_cache,
            result,
        } = completion;
        match result {
            Ok(response) if response.status.as_u16() == 304 => self
                .http_cache
                .merge_not_modified(&request, &response, Instant::now(), epoch)
                .ok_or(FetchError::WorkerStopped),
            Ok(response) => {
                if !from_cache {
                    let _outcome =
                        self.http_cache
                            .store(&request, &response, Instant::now(), epoch);
                }
                Ok(response)
            }
            Err(error) => Err(error),
        }
    }

    fn finish_cached_batch(&mut self, completions: Vec<CachedFetchResult>) -> Vec<FetchResult> {
        completions
            .into_iter()
            .map(|completion| self.finish_cached_fetch(completion))
            .collect()
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
        if env::var_os("RENDER_DEBUG_FRAME").is_some() {
            eprintln!(
                "render-browser stylesheet plan resources={} diagnostics={}",
                plan.resources.len(),
                plan.diagnostics.len()
            );
            for resource in &plan.resources {
                eprintln!(
                    "render-browser stylesheet request owner={:?} url={}",
                    resource.key.owner, resource.key.requested_url
                );
            }
        }
        if plan.is_empty() {
            report_stylesheet_diagnostics(&plan.diagnostics);
            let Some(page) = self.pages.get_mut(&id) else {
                return;
            };
            page.cancel_style_sheets();
            page.styles_resolved = true;
            self.tabs.set_loading(id, false);
            self.start_classic_scripts(id);
            self.repaint_chrome();
            return;
        }
        let requests = {
            let Some(page) = self.pages.get_mut(&id) else {
                return;
            };
            page.cancel_style_sheets();
            plan.requests()
                .into_iter()
                .map(|request| page.cookies.decorate_request(request))
                .collect::<Vec<_>>()
        };
        let handle = self.submit_cached_batch(requests);
        if let Some(page) = self.pages.get_mut(&id) {
            page.pending_style_sheets = Some(PendingStyleSheets { plan, handle });
        } else {
            handle.cancel();
            return;
        }
        self.tabs.set_loading(id, true);
        self.repaint_chrome();
    }

    fn start_classic_scripts(&mut self, id: TabId) {
        let mut rerender = false;
        let mut loading_complete = false;
        let mut pending_request = None;
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
                    .collect::<Vec<_>>();
                pending_request = Some((plan, requests));
                break;
            }
        }

        if let Some((plan, requests)) = pending_request {
            let handle = self.submit_cached_batch(requests);
            let Some(page) = self.pages.get_mut(&id) else {
                handle.cancel();
                return;
            };
            page.pending_scripts = Some(PendingScripts { plan, handle });
            self.tabs.set_loading(id, true);
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
        let (plan, requests) = {
            let Some(page) = self.pages.get_mut(&id) else {
                return;
            };
            if page.pending_images.is_some() {
                return;
            }
            let plan = plan_images_with_styles_and_context(
                page.page.document(),
                &page.computed_styles,
                &page.navigation.committed().target.history_url(),
                &page.images,
                ImageLimits::default(),
                ImageSelectionContext {
                    viewport_width: page.viewport.width.max(1),
                    viewport_height: page.viewport.height.max(1),
                    device_pixel_ratio_milli: 1_000,
                },
            );
            let requests = plan
                .requests()
                .into_iter()
                .map(|request| page.cookies.decorate_request(request))
                .collect::<Vec<_>>();
            (plan, requests)
        };
        report_image_diagnostics(&plan.diagnostics);
        if env::var_os("RENDER_DEBUG_FRAME").is_some() {
            eprintln!(
                "render-browser image plan resources={}",
                plan.resources.len()
            );
            for resource in plan.resources.iter().take(12) {
                eprintln!(
                    "render-browser image request owner={:?} source={:?} url={}",
                    resource.key.owner, resource.key.source, resource.key.requested_url
                );
            }
        }
        if plan.is_empty() {
            return;
        }
        let handle = self.submit_cached_batch(requests);
        if let Some(page) = self.pages.get_mut(&id) {
            page.pending_images = Some(PendingImages { plan, handle });
        } else {
            handle.cancel();
        }
    }

    fn poll_network(&mut self) {
        let mut completed_documents = Vec::new();
        let mut completed_style_sheets = Vec::new();
        let mut completed_scripts = Vec::new();
        let mut completed_images = Vec::new();
        for (id, page) in &mut self.pages {
            if let Some(pending) = page.navigation.pending.as_mut() {
                match pending.handle.try_recv() {
                    Ok(result) => completed_documents.push((*id, result)),
                    Err(TryRecvError::Empty) => {}
                    Err(TryRecvError::Disconnected) => {
                        unreachable!("cache request handle maps disconnects")
                    }
                }
            }
            if let Some(pending) = page.pending_style_sheets.as_mut() {
                match pending.handle.try_recv() {
                    Ok(results) => completed_style_sheets.push((*id, results)),
                    Err(TryRecvError::Empty) => {}
                    Err(TryRecvError::Disconnected) => {
                        unreachable!("cache batch handle maps disconnects")
                    }
                }
            }
            if let Some(pending) = page.pending_scripts.as_mut() {
                match pending.handle.try_recv() {
                    Ok(results) => completed_scripts.push((*id, results)),
                    Err(TryRecvError::Empty) => {}
                    Err(TryRecvError::Disconnected) => {
                        unreachable!("cache batch handle maps disconnects")
                    }
                }
            }
            if let Some(pending) = page.pending_images.as_mut() {
                match pending.handle.try_recv() {
                    Ok(results) => completed_images.push((*id, results)),
                    Err(TryRecvError::Empty) => {}
                    Err(TryRecvError::Disconnected) => {
                        unreachable!("cache batch handle maps disconnects")
                    }
                }
            }
        }
        for (id, completion) in completed_documents {
            let requested_url = self
                .pages
                .get_mut(&id)
                .and_then(|page| page.navigation.take_pending())
                .map(|pending| pending.requested_url);
            if let Some(requested_url) = requested_url {
                let result = self.finish_cached_fetch(completion);
                self.finish_network_navigation(id, requested_url, result);
            }
        }
        for (id, completions) in completed_style_sheets {
            let results = self.finish_cached_batch(completions);
            self.finish_external_style_sheets(id, results);
        }
        for (id, completions) in completed_scripts {
            let results = self.finish_cached_batch(completions);
            self.finish_classic_scripts(id, results);
        }
        for (id, completions) in completed_images {
            let results = self.finish_cached_batch(completions);
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
        if env::var_os("RENDER_DEBUG_FRAME").is_some() {
            eprintln!("render-browser image loaded={}", application.loaded.len());
            for loaded in application.loaded.iter().take(12) {
                eprintln!(
                    "render-browser image result owner={:?} source={:?} size={}x{}",
                    loaded.owner, loaded.source, loaded.width, loaded.height
                );
            }
        }
        for loaded in &application.loaded {
            if matches!(
                loaded.source,
                ImageSource::Element | ImageSource::VideoPoster
            ) {
                let _ = page
                    .page
                    .queue_dom_event(PageDomEvent::new(loaded.owner, "load"));
            }
        }
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
        self.left_pointer_down = true;
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
        if self.is_cache_clear_control(id, hit_node) {
            self.clear_http_cache();
            self.repaint_chrome();
            return;
        }
        let editable = hit_node.and_then(|node| {
            self.content_editable_node(id, node)
                .or_else(|| self.content_wrapper_control(id, node))
        });
        if let Some(node) = editable
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

    fn is_cache_clear_control(
        &self,
        id: TabId,
        hit_node: Option<render_core::dom::NodeId>,
    ) -> bool {
        let Some(mut node) = hit_node else {
            return false;
        };
        let Some(page) = self.pages.get(&id) else {
            return false;
        };
        if page.navigation.committed().target != NavigationTarget::Settings {
            return false;
        }
        let dom = page.page.document().dom();
        loop {
            let element_id = dom.attribute(node, "id").ok().flatten();
            let action = dom.attribute(node, "data-render-action").ok().flatten();
            if is_trusted_clear_http_cache_action(true, element_id, action) {
                return true;
            }
            let Some(parent) = dom.parent(node) else {
                return false;
            };
            node = parent;
        }
    }

    fn clear_http_cache(&mut self) {
        if self.cache_clear_state.is_busy() {
            return;
        }
        let result = self.http_cache.clear();
        let memory_entries = result.memory_entries;
        let memory_bytes = result.memory_bytes;
        if let Some(worker) = self.disk_cache.as_ref() {
            match worker.clear() {
                Ok(operation) => {
                    self.pending_disk_clear = Some(operation);
                    self.cache_clear_state = CacheClearUiState::ClearingDisk {
                        memory_entries,
                        memory_bytes,
                    };
                }
                Err(error) => {
                    eprintln!("render-browser could not clear disk cache: {error}");
                    self.cache_clear_state = CacheClearUiState::DiskClearFailed {
                        memory_entries,
                        memory_bytes,
                    };
                }
            }
        } else {
            self.cache_clear_state = CacheClearUiState::Cleared {
                memory_entries,
                memory_bytes,
            };
        }
        self.refresh_settings_pages();
    }

    fn poll_disk_cache(&mut self) {
        let events = {
            let Some(worker) = self.disk_cache.as_ref() else {
                return;
            };
            let mut events = Vec::new();
            while let Ok(event) = worker.poll() {
                events.push(event);
            }
            events
        };
        let mut refresh_settings = false;
        for event in events {
            match event {
                DiskCacheEvent::Ready { result: Err(error) } => {
                    eprintln!("render-browser disk cache disabled: {error}");
                    self.disk_cache = None;
                    if self.pending_disk_clear.take().is_some() {
                        self.cache_clear_state = CacheClearUiState::DiskClearFailed {
                            memory_entries: 0,
                            memory_bytes: 0,
                        };
                        refresh_settings = true;
                    }
                }
                DiskCacheEvent::ClearFinished { id, result } => {
                    if self.pending_disk_clear != Some(id) {
                        continue;
                    }
                    self.pending_disk_clear = None;
                    let (memory_entries, memory_bytes) = match self.cache_clear_state {
                        CacheClearUiState::ClearingDisk {
                            memory_entries,
                            memory_bytes,
                        } => (memory_entries, memory_bytes),
                        _ => (0, 0),
                    };
                    self.cache_clear_state = match result {
                        Ok(_) => CacheClearUiState::Cleared {
                            memory_entries,
                            memory_bytes,
                        },
                        Err(error) => {
                            eprintln!("render-browser disk cache cleanup failed: {error}");
                            CacheClearUiState::DiskClearFailed {
                                memory_entries,
                                memory_bytes,
                            }
                        }
                    };
                    refresh_settings = true;
                }
                DiskCacheEvent::Ready { result: Ok(_) }
                | DiskCacheEvent::Read { .. }
                | DiskCacheEvent::Write { .. }
                | DiskCacheEvent::ClearStarted { .. } => {}
            }
        }
        if refresh_settings {
            self.refresh_settings_pages();
        }
    }

    fn refresh_settings_pages(&mut self) {
        let settings_tabs = self
            .pages
            .iter()
            .filter_map(|(id, page)| {
                (page.navigation.committed().target == NavigationTarget::Settings).then_some(*id)
            })
            .collect::<Vec<_>>();
        for id in settings_tabs {
            self.install_source(id, settings_source(self.cache_clear_state), false);
        }
    }

    fn content_text_input_value(
        &self,
        tab: TabId,
        node: render_core::dom::NodeId,
    ) -> Option<String> {
        let dom = self.pages.get(&tab)?.page.document().dom();
        content_text_input_value(dom, node)
    }

    fn content_wrapper_control(
        &self,
        tab: TabId,
        node: render_core::dom::NodeId,
    ) -> Option<render_core::dom::NodeId> {
        let page = self.pages.get(&tab)?;
        content_wrapper_control(page.page.document().dom(), &page.geometry, node)
    }

    fn content_editable_node(
        &self,
        tab: TabId,
        node: render_core::dom::NodeId,
    ) -> Option<render_core::dom::NodeId> {
        let page = self.pages.get(&tab)?;
        let dom = page.page.document().dom();
        let mut candidate = Some(node);
        while let Some(current) = candidate {
            if self.content_text_input_value(tab, current).is_some() {
                return Some(current);
            }
            candidate = dom.parent(current);
        }
        None
    }

    fn sync_content_editor(&mut self) {
        let Some(content) = self.content_editor.as_ref() else {
            return;
        };
        let tab = content.tab;
        let node = content.node;
        let value = content.editor.text().to_owned();
        if let Some(page) = self.pages.get_mut(&tab)
            && set_content_text_value(page.page.document_mut().dom_mut(), node, &value).is_ok()
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
        let id = self.tabs.active_id();
        let page = self.pages.get(&id)?;
        let editable = page
            .geometry
            .iter()
            .filter_map(|(raw_node, rect)| {
                let node = render_core::dom::NodeId::from_u64(*raw_node);
                self.content_text_input_value(id, node)?;
                let point = PhysicalPoint {
                    x: self.cursor.x,
                    y: self.cursor.y - self.layout.as_ref()?.chrome_height as f32
                        + page.scroll.offset_y(),
                };
                (point.x >= rect.x
                    && point.x < rect.x + rect.width
                    && point.y >= rect.y
                    && point.y < rect.y + rect.height)
                    .then_some((rect.width * rect.height, node))
            })
            .min_by(|(left, _), (right, _)| left.total_cmp(right))
            .map(|(_, node)| node);
        if editable.is_some() {
            return editable;
        }
        let display_list = page.display_list.as_ref()?;
        hit_test_content_regions(
            display_list.items().iter().map(|item| ContentHitRegion {
                bounds: item.bounds,
                source: item.source,
                coordinate_space: item.coordinate_space,
                hit_testable: is_content_hit_command(&item.command),
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
        let previous_hot = self.hot;
        self.hot = layout.hit_test(self.cursor);
        let hot_changed = self.hot != previous_hot;
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
            HitTarget::NewTab | HitTarget::Toolbar(_) | HitTarget::WindowControl(_) => {
                CursorIcon::Pointer
            }
            HitTarget::Tab(_)
            | HitTarget::CloseTab(_)
            | HitTarget::TitleBar
            | HitTarget::Chrome => CursorIcon::Default,
        };
        if cursor_icon != self.cursor_icon {
            if let Some(window) = &self.window {
                window.set_cursor(cursor_icon);
            }
            self.cursor_icon = cursor_icon;
        }
        if self.address_selecting {
            let index =
                address_index_at_x(layout, &self.editor, self.cursor.x, self.fonts.as_ref());
            self.editor.extend_pointer_selection(index);
            self.repaint_chrome();
            return;
        }
        let move_intent = self
            .left_pointer_down
            .then(|| {
                self.drag
                    .as_mut()
                    .and_then(|drag| drag.update(self.cursor.x, layout))
            })
            .flatten();
        if let Some(intent) = move_intent {
            self.handle_tab_intent(intent);
        } else if hot_changed || self.drag.is_some() {
            self.repaint_chrome();
        }
    }

    fn handle_pointer_release(&mut self) {
        self.left_pointer_down = false;
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
            if let Some(page) = self.pages.get_mut(&id) {
                page.page.runtime_mut().install_viewport(
                    page.viewport.width as f32,
                    page.viewport.height as f32,
                    0.0,
                    page.scroll.offset_y(),
                );
                page.page.queue_pending_runtime_microtasks();
                let document = page.page.document().dom().document();
                let _scroll_event = page
                    .page
                    .queue_dom_event(PageDomEvent::new(document, "scroll"));
            }
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
        let primary = primary_modifier_active(self.modifiers);
        let shift = self.modifiers.shift_key();
        if primary {
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
        if self.content_editor.is_some() && self.handle_content_keyboard(event) {
            return;
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
            Key::Character(_) if !primary && !self.modifiers.alt_key() => {
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
        let primary = primary_modifier_active(self.modifiers);
        if primary && let Some(command) = address_shortcut(&event.logical_key, shift) {
            let changed = self
                .content_editor
                .as_mut()
                .is_some_and(|content| content.editor.execute(command, &mut self.clipboard));
            if changed && !matches!(command, AddressCommand::Copy | AddressCommand::SelectAll) {
                self.sync_content_editor();
            }
            self.repaint_chrome();
            return true;
        }
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
            Key::Character(_) if !primary && !self.modifiers.alt_key() => {
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

fn is_content_editable(dom: &render_core::dom::Dom, node: render_core::dom::NodeId) -> bool {
    dom.attribute(node, "contenteditable")
        .ok()
        .flatten()
        .is_some_and(|value| {
            value.is_empty()
                || value.eq_ignore_ascii_case("true")
                || value.eq_ignore_ascii_case("plaintext-only")
        })
}

fn content_text_input_value(
    dom: &render_core::dom::Dom,
    node: render_core::dom::NodeId,
) -> Option<String> {
    let render_core::dom::NodeKind::Element(element) = dom.node(node)?.kind() else {
        return None;
    };
    match element.local_name.as_str() {
        "input" => {
            // A missing or empty type attribute defaults to "text".
            let input_type = dom
                .attribute(node, "type")
                .ok()
                .flatten()
                .filter(|value| !value.is_empty())
                .unwrap_or("text");
            matches!(input_type.to_ascii_lowercase().as_str(), "text" | "search").then(|| {
                dom.attribute(node, "value")
                    .ok()
                    .flatten()
                    .unwrap_or("")
                    .to_owned()
            })
        }
        "textarea" => Some(descendant_text(dom, node)),
        _ if is_content_editable(dom, node) => Some(descendant_text(dom, node)),
        _ => None,
    }
}

/// Route a click on a painted wrapper (for example a styled search box whose
/// embedded control paints no content of its own) to the embedded text
/// control. The control must be the dominant painted area of the wrapper so
/// page-sized containers never capture clicks meant for surrounding content.
fn content_wrapper_control(
    dom: &render_core::dom::Dom,
    geometry: &BTreeMap<u64, ElementRect>,
    node: render_core::dom::NodeId,
) -> Option<render_core::dom::NodeId> {
    let bounds = geometry.get(&node.as_u64())?;
    let wrapper_area = bounds.width * bounds.height;
    if wrapper_area <= 0.0 {
        return None;
    }
    let mut pending = dom.children(node).unwrap_or_default().to_vec();
    let mut best: Option<(f32, render_core::dom::NodeId)> = None;
    while let Some(current) = pending.pop() {
        pending.extend(dom.children(current).unwrap_or_default().iter().copied());
        if content_text_input_value(dom, current).is_none() {
            continue;
        }
        let Some(rect) = geometry.get(&current.as_u64()) else {
            continue;
        };
        let area = rect.width * rect.height;
        if best.is_none_or(|(smallest, _)| area < smallest) {
            best = Some((area, current));
        }
    }
    let (area, control) = best?;
    (area >= wrapper_area * 0.25).then_some(control)
}

fn descendant_text(dom: &render_core::dom::Dom, root: render_core::dom::NodeId) -> String {
    let mut output = String::new();
    let mut pending = dom
        .children(root)
        .unwrap_or_default()
        .iter()
        .rev()
        .copied()
        .collect::<Vec<_>>();
    while let Some(node) = pending.pop() {
        if let Some(render_core::dom::NodeKind::Text(text)) =
            dom.node(node).map(render_core::dom::Node::kind)
        {
            output.push_str(text);
        }
        pending.extend(dom.children(node).unwrap_or_default().iter().rev().copied());
    }
    output
}

fn set_content_text_value(
    dom: &mut render_core::dom::Dom,
    node: render_core::dom::NodeId,
    value: &str,
) -> Result<(), render_core::dom::DomError> {
    let kind = dom.node(node).map(render_core::dom::Node::kind);
    let Some(render_core::dom::NodeKind::Element(element)) = kind else {
        return Ok(());
    };
    if element.local_name == "input" {
        return dom.set_attribute(node, "value", value);
    }
    let children = dom.children(node).unwrap_or_default().to_vec();
    for child in children {
        dom.remove_child(node, child)?;
    }
    if !value.is_empty() {
        let text = dom.create_text(value);
        dom.append_child(node, text)?;
    }
    Ok(())
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
                self.left_pointer_down = false;
                self.drag = None;
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
        self.poll_disk_cache();
        let active = self.tabs.active_id();
        let mut rendered_active = false;
        let mut navigation_candidates = Vec::new();
        for (id, page) in &mut self.pages {
            let revision_before = page.dom_revision;
            let turn_budget = if *id == active {
                ACTIVE_PAGE_TURN_BUDGET
            } else {
                BACKGROUND_PAGE_TURN_BUDGET
            };
            match page
                .page
                .pump_at_most_without_render(page.created_at.elapsed(), turn_budget)
            {
                Ok(_) => {
                    let revision_after = page.page.document().dom().revision().as_u64();
                    page.dom_revision = revision_after;
                    rendered_active |= *id == active && revision_after != revision_before;
                }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HostPlatform {
    MacOs,
    Other,
}

const fn primary_modifier_for(platform: HostPlatform, control: bool, command: bool) -> bool {
    match platform {
        HostPlatform::MacOs => command,
        HostPlatform::Other => control,
    }
}

fn primary_modifier_active(modifiers: ModifiersState) -> bool {
    let platform = if cfg!(target_os = "macos") {
        HostPlatform::MacOs
    } else {
        HostPlatform::Other
    };
    primary_modifier_for(platform, modifiers.control_key(), modifiers.super_key())
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

fn copy_frame_regions(
    destination: &mut [u32],
    source: &[u32],
    frame_size: WindowSize<u32>,
    regions: &[SoftBufferRect],
) {
    let frame_width = frame_size.width as usize;
    for region in regions {
        let x = region.x as usize;
        let y = region.y as usize;
        let width = region.width.get() as usize;
        let height = region.height.get() as usize;
        for row in 0..height {
            let offset = (y + row) * frame_width + x;
            let end = offset + width;
            destination[offset..end].copy_from_slice(&source[offset..end]);
        }
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

fn log_completed_frame_debug(frame: &PageRenderFrame, tab_id: u64) {
    if env::var_os("RENDER_DEBUG_FRAME").is_none() {
        return;
    }
    eprintln!(
        "render-browser frame page={tab_id} pixels={} display_items={} geometry={} content_height={} viewport_height={}",
        frame.frame.len(),
        frame
            .display_list
            .as_ref()
            .map_or(0, |list| list.items().len()),
        frame.geometry.as_ref().map_or(0, BTreeMap::len),
        frame.content_height,
        frame.viewport_height,
    );
    let Some(display_list) = &frame.display_list else {
        return;
    };
    for item in display_list.items().iter().take(24) {
        let command = match &item.command {
            render_core::paint::DisplayCommand::SolidRect { .. } => "solid",
            render_core::paint::DisplayCommand::Border(_) => "border",
            render_core::paint::DisplayCommand::BoxShadow(_) => "shadow",
            render_core::paint::DisplayCommand::PushClip(_) => "push-clip",
            render_core::paint::DisplayCommand::PopClip => "pop-clip",
            render_core::paint::DisplayCommand::PushTransform(_) => "push-transform",
            render_core::paint::DisplayCommand::PopTransform => "pop-transform",
            render_core::paint::DisplayCommand::GlyphRun(_) => "glyph",
            render_core::paint::DisplayCommand::TextDecoration(_) => "decoration",
            render_core::paint::DisplayCommand::Image(_) => "image",
            render_core::paint::DisplayCommand::LinearGradient(_) => "linear-gradient",
            render_core::paint::DisplayCommand::RadialGradient(_) => "radial-gradient",
            render_core::paint::DisplayCommand::Canvas { .. } => "canvas",
            render_core::paint::DisplayCommand::PushStackingContext(_) => "push-stack",
            render_core::paint::DisplayCommand::PopStackingContext => "pop-stack",
        };
        eprintln!(
            "render-browser display command={} source={:?} bounds={:?}",
            command, item.source, item.bounds
        );
    }
}

fn dump_debug_frame(frame: &[u32], size: WindowSize<u32>) {
    let Some(path) = env::var_os("RENDER_DUMP_FRAME") else {
        return;
    };
    if size.width == 0
        || size.height == 0
        || frame.len() != size.width as usize * size.height as usize
    {
        return;
    }
    let mut ppm = format!("P6\n{} {}\n255\n", size.width, size.height).into_bytes();
    ppm.reserve(frame.len().saturating_mul(3));
    for pixel in frame {
        ppm.extend_from_slice(&[
            ((pixel >> 16) & 0xff) as u8,
            ((pixel >> 8) & 0xff) as u8,
            (pixel & 0xff) as u8,
        ]);
    }
    let _ = fs::write(path, ppm);
}

fn geometry_from_layout(
    fragments: &render_core::layout::FragmentTree,
) -> BTreeMap<u64, ElementRect> {
    let mut geometry = BTreeMap::new();
    for fragment in fragments.iter() {
        let FragmentKind::Box(box_geometry) = &fragment.kind else {
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
    use std::collections::{BTreeMap, HashSet};

    use render_core::dom::NodeKind;
    use render_core::html::parse_document;
    use render_core::js::RuntimeLimits;
    use render_core::layout::{PhysicalPoint, PhysicalRect};
    use render_core::navigation::HistoryEntry;
    use render_core::paint::PaintCoordinateSpace;
    use render_core::paint::{ClipShape, Color, DisplayCommand, Surface, Transform2D};
    use render_core::script::ScriptDiscoveryLimits;
    use render_net::{FetchConfig, FetchRequest, HttpTransport, Url};
    use winit::dpi::{PhysicalPosition, PhysicalSize as WindowSize};
    use winit::event::MouseScrollDelta;
    use winit::keyboard::Key;

    use super::{
        ContentHitRegion, ElementRect, FrameDamage, FrameRect, HOME_TITLE, HostPlatform,
        PageNavigation, PageSource, PageState, address_shortcut, blit_page,
        content_text_input_value, content_wrapper_control, get_content_navigation_target,
        hit_test_content_regions, home_source, is_content_hit_command, network_start_source,
        primary_modifier_for, source_from_network_response, surface_to_softbuffer,
        wheel_document_delta_y,
    };
    use render_browser::chrome::Point;
    use render_browser::editor::AddressCommand;
    use render_browser::navigation::NavigationTarget;
    use render_browser::scripts::{
        plan_classic_scripts, plan_unstarted_classic_scripts, prepare_script_batch,
    };

    #[test]
    fn converts_core_surface_to_softbuffer_rgb_words() {
        let surface = Surface::new(1, 1, Color::rgb(0x12, 0x34, 0x56));
        assert_eq!(surface_to_softbuffer(&surface), [0x0012_3456]);
    }

    #[test]
    fn frame_damage_clips_and_merges_touching_regions() {
        let mut damage = FrameDamage::default();
        damage.mark_rect(
            FrameRect {
                x: 8,
                y: 8,
                width: 10,
                height: 10,
            },
            100,
            100,
        );
        damage.mark_rect(
            FrameRect {
                x: 18,
                y: 8,
                width: 10,
                height: 10,
            },
            100,
            100,
        );
        assert_eq!(
            damage.rects,
            [FrameRect {
                x: 8,
                y: 8,
                width: 20,
                height: 10,
            }]
        );
        damage.mark_rect(
            FrameRect {
                x: 95,
                y: 95,
                width: 20,
                height: 20,
            },
            100,
            100,
        );
        assert!(damage.rects.contains(&FrameRect {
            x: 95,
            y: 95,
            width: 5,
            height: 5,
        }));
    }

    #[test]
    fn frame_damage_switches_to_full_for_large_updates() {
        let mut damage = FrameDamage::default();
        damage.mark_rect(
            FrameRect {
                x: 0,
                y: 0,
                width: 80,
                height: 100,
            },
            100,
            100,
        );
        assert!(damage.full);
        assert!(damage.rects.is_empty());
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
    fn data_document_response_becomes_a_renderable_html_page() {
        let url =
            Url::parse("data:text/html,%3Ctitle%3EData%3C%2Ftitle%3E%3Ch1%3EHello%3C%2Fh1%3E")
                .expect("valid data URL");
        let response = HttpTransport::new(FetchConfig::default())
            .fetch(
                &FetchRequest::get(url.clone()),
                &render_net::CancelToken::default(),
            )
            .expect("data response");
        let source = source_from_network_response(&response).expect("renderable data document");

        assert_eq!(source.target, NavigationTarget::Url(url));
        assert!(source.html.contains("<h1>Hello</h1>"));
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
    fn primary_modifier_matches_macos_and_windows_conventions() {
        assert!(primary_modifier_for(HostPlatform::MacOs, false, true));
        assert!(!primary_modifier_for(HostPlatform::MacOs, true, false));
        assert!(primary_modifier_for(HostPlatform::Other, true, false));
        assert!(!primary_modifier_for(HostPlatform::Other, false, true));
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
                hit_testable: true,
            },
            ContentHitRegion {
                bounds: PhysicalRect::new(10.0, 140.0, 100.0, 30.0),
                source: Some(top_source),
                coordinate_space: PaintCoordinateSpace::Document,
                hit_testable: true,
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
    fn structural_paint_commands_do_not_participate_in_content_hits() {
        let bounds = PhysicalRect::new(0.0, 0.0, 100.0, 50.0);
        assert!(is_content_hit_command(&DisplayCommand::SolidRect {
            rect: bounds,
            color: Color::rgb(0xff, 0xff, 0xff),
        }));
        assert!(!is_content_hit_command(&DisplayCommand::PushClip(
            ClipShape::Rect(bounds)
        )));
        assert!(!is_content_hit_command(&DisplayCommand::PopClip));
        assert!(!is_content_hit_command(&DisplayCommand::PushTransform(
            Transform2D::default()
        )));
        assert!(!is_content_hit_command(&DisplayCommand::PopTransform));
        assert!(!is_content_hit_command(&DisplayCommand::PopStackingContext));
    }

    #[test]
    fn clip_items_do_not_shadow_painted_content_during_hit_testing() {
        let mut dom = render_core::dom::Dom::new();
        let root = dom.create_element("div");
        let link = dom.create_element("a");
        // A root-sized clip pair surrounds every content item in paint order;
        // the reverse scan must land on the link content, not on the clips.
        let regions = [
            ContentHitRegion {
                bounds: PhysicalRect::new(0.0, 0.0, 1_770.0, 1_026.0),
                source: Some(root),
                coordinate_space: PaintCoordinateSpace::Document,
                hit_testable: false,
            },
            ContentHitRegion {
                bounds: PhysicalRect::new(36.0, 30.0, 24.0, 20.0),
                source: Some(link),
                coordinate_space: PaintCoordinateSpace::Document,
                hit_testable: true,
            },
            ContentHitRegion {
                bounds: PhysicalRect::new(0.0, 0.0, 1_770.0, 1_026.0),
                source: Some(root),
                coordinate_space: PaintCoordinateSpace::Document,
                hit_testable: false,
            },
        ];
        assert_eq!(
            hit_test_content_regions(
                regions.into_iter(),
                Point { x: 48.0, y: 100.0 },
                60,
                PhysicalPoint { x: 0.0, y: 0.0 },
            ),
            Some(link)
        );
    }

    #[test]
    fn text_input_value_defaults_missing_and_empty_type_to_text() {
        let document = parse_document(
            "<input id='kw' name='wd' value=''><input id='blank' type='' value='x'>\
             <input id='hidden' type='hidden' value='h'><input id='search' type='SEARCH' value='s'>\
             <textarea id='ta'>hi</textarea><div id='plain'>text</div>",
        );
        let dom = &document.dom;
        let find = |id: &str| {
            let mut pending = vec![dom.document()];
            while let Some(node) = pending.pop() {
                if dom.attribute(node, "id").ok().flatten() == Some(id) {
                    return node;
                }
                pending.extend(dom.children(node).unwrap_or_default().iter().copied());
            }
            panic!("element {id} should exist");
        };
        assert_eq!(
            content_text_input_value(dom, find("kw")),
            Some(String::new())
        );
        assert_eq!(
            content_text_input_value(dom, find("blank")),
            Some("x".to_owned())
        );
        assert_eq!(content_text_input_value(dom, find("hidden")), None);
        assert_eq!(
            content_text_input_value(dom, find("search")),
            Some("s".to_owned())
        );
        assert_eq!(
            content_text_input_value(dom, find("ta")),
            Some("hi".to_owned())
        );
        assert_eq!(content_text_input_value(dom, find("plain")), None);
    }

    #[test]
    fn wrapper_click_routes_to_dominant_embedded_text_control() {
        let document =
            parse_document("<div id='wrap'><textarea id='ta'></textarea><button>go</button></div>");
        let dom = &document.dom;
        let find = |id: &str| {
            let mut pending = vec![dom.document()];
            while let Some(node) = pending.pop() {
                if dom.attribute(node, "id").ok().flatten() == Some(id) {
                    return node;
                }
                pending.extend(dom.children(node).unwrap_or_default().iter().copied());
            }
            panic!("element {id} should exist");
        };
        let wrap = find("wrap");
        let ta = find("ta");
        let mut geometry = BTreeMap::new();
        geometry.insert(
            wrap.as_u64(),
            ElementRect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 100.0,
            },
        );
        geometry.insert(
            ta.as_u64(),
            ElementRect {
                x: 5.0,
                y: 5.0,
                width: 90.0,
                height: 28.0,
            },
        );
        assert_eq!(content_wrapper_control(dom, &geometry, wrap), Some(ta));

        // A control covering only a sliver of the wrapper does not capture it.
        geometry.insert(
            ta.as_u64(),
            ElementRect {
                x: 5.0,
                y: 5.0,
                width: 10.0,
                height: 10.0,
            },
        );
        assert_eq!(content_wrapper_control(dom, &geometry, wrap), None);
    }

    #[test]
    fn wrapper_without_geometry_or_control_stays_unrouted() {
        let document = parse_document(
            "<div id='bare'><p>nothing interactive</p></div><div id='hidden-wrap'><textarea id='ta'></textarea></div>",
        );
        let dom = &document.dom;
        let find = |id: &str| {
            let mut pending = vec![dom.document()];
            while let Some(node) = pending.pop() {
                if dom.attribute(node, "id").ok().flatten() == Some(id) {
                    return node;
                }
                pending.extend(dom.children(node).unwrap_or_default().iter().copied());
            }
            panic!("element {id} should exist");
        };
        let bare = find("bare");
        let hidden_wrap = find("hidden-wrap");
        let mut geometry = BTreeMap::new();
        geometry.insert(
            bare.as_u64(),
            ElementRect {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 0.0,
            },
        );
        assert_eq!(content_wrapper_control(dom, &geometry, bare), None);
        // No geometry entry for the wrapper at all.
        assert_eq!(content_wrapper_control(dom, &geometry, hidden_wrap), None);
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
