//! Native browser shell for the self-owned Rust rendering pipeline.
#![allow(clippy::cast_precision_loss)]
use crate::ACTIVE_PAGE_TURN_BUDGET;
use crate::BACKGROUND_PAGE_TURN_BUDGET;
use crate::INITIAL_HEIGHT;
use crate::INITIAL_WIDTH;
use crate::NativeSurface;
use crate::SCROLL_LINE_PIXELS;
use crate::UserEvent;
use crate::content_interaction;
use crate::content_interaction::content_text_input_value;
use crate::content_interaction::content_wrapper_control;
use crate::diagnostics::dump_debug_frame;
use crate::diagnostics::log_completed_frame_debug;
use crate::diagnostics::report_image_diagnostics;
use crate::diagnostics::report_script_diagnostics;
use crate::diagnostics::report_script_discovery_diagnostics;
use crate::diagnostics::report_stylesheet_diagnostics;
use crate::fetch_handles::CachedBatchHandle;
use crate::fetch_handles::CachedFetchResult;
use crate::fetch_handles::CachedRequestHandle;
use crate::fetch_handles::CachedRequestState;
use crate::frame::FrameDamage;
use crate::frame::FrameRect;
use crate::frame::blit_page;
use crate::frame::copy_frame_regions;
use crate::page_source::PageSource;
use crate::page_source::error_source;
use crate::page_source::home_source;
use crate::page_source::settings_source;
use crate::page_source::source_from_local_file;
use crate::page_source::source_from_network_response;
use crate::page_state::PageState;
use crate::page_state::PendingImages;
use crate::page_state::PendingScripts;
use crate::page_state::PendingStyleSheets;
use crate::render_worker::FullPageRenderPayload;
use crate::render_worker::PageRenderFrame;
use crate::render_worker::PageRenderPayload;
use crate::render_worker::PageRenderWorker;
use render_browser::cache::CacheEpoch;
use render_browser::cache::CacheLookup;
use render_browser::cache::HttpCache;
use render_browser::cache::disk::DiskCacheEvent;
use render_browser::cache::disk::DiskCacheOperationId;
use render_browser::cache::disk::DiskCacheWorker;
use render_browser::chrome::AddressClickTracker;
use render_browser::chrome::AddressContextMenu;
use render_browser::chrome::Canvas;
use render_browser::chrome::ChromeLayout;
use render_browser::chrome::ChromeTheme;
use render_browser::chrome::HitTarget;
use render_browser::chrome::Point;
use render_browser::chrome::TabDrag;
use render_browser::chrome::TitleBarClickTracker;
use render_browser::chrome::TitleBarGesture;
use render_browser::chrome::WindowAction;
use render_browser::chrome::address_index_at_x;
use render_browser::chrome::paint_address_context_menu;
use render_browser::chrome::paint_chrome;
use render_browser::editor::AddressCommand;
use render_browser::editor::AddressEditor;
use render_browser::editor::Clipboard;
use render_browser::editor::NativeClipboard;
use render_browser::font_backend::SystemFontBackend;
use render_browser::images::apply_image_batch;
use render_browser::images::plan_images_with_styles_and_context;
use render_browser::model::TabId;
use render_browser::model::TabIntent;
use render_browser::model::TabModel;
use render_browser::navigation::NavigationIntent;
use render_browser::navigation::NavigationTarget;
use render_browser::navigation::intent_from_address;
use render_browser::resources::StylesheetFetchPlan;
use render_browser::scripts::plan_unstarted_classic_scripts;
use render_browser::scripts::prepare_script_batch;
use render_browser::settings::CacheClearUiState;
use render_browser::settings::is_trusted_clear_http_cache_action;
use render_browser::worker::CompletedRender;
use render_browser::worker::RenderIdentity;
use render_browser::worker::RenderJob;
use render_browser::worker::RenderOffset;
use render_browser::worker::RenderViewport;
use render_core::image::ImageLimits;
use render_core::image::ImageSelectionContext;
use render_core::image::ImageSource;
use render_core::interaction::FormMethod;
use render_core::js::RuntimeLimits;
use render_core::layout::PhysicalPoint;
use render_core::navigation::HistoryEntry;
use render_core::page::PageDomEvent;
use render_core::script::ScriptDiscoveryLimits;
use render_net::FetchError;
use render_net::FetchRequest;
use render_net::FetchResult;
use render_net::NetworkWorker;
use render_net::Url;
use softbuffer::Context;
use std::collections::HashMap;
use std::env;
use std::error::Error;
use std::fmt;
use std::io;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::mpsc::TryRecvError;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::dpi::PhysicalPosition;
use winit::dpi::PhysicalSize as WindowSize;
use winit::event::ElementState;
use winit::event::Ime;
use winit::event::MouseButton;
use winit::event::MouseScrollDelta;
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::event_loop::ControlFlow;
use winit::keyboard::Key;
use winit::keyboard::ModifiersState;
use winit::keyboard::NamedKey;
use winit::window::CursorIcon;
use winit::window::Theme;
use winit::window::Window;
use winit::window::WindowId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HistoryMode {
    Push,
    Current,
}

/// Total submission attempts, including the first, before a request parked
/// behind a full network worker queue is allowed to surface the queue-full
/// error through the normal polling path.
const QUEUE_FULL_SUBMIT_ATTEMPTS: usize = 8;
/// First backoff delay for queue-full resubmission; it doubles per attempt.
const QUEUE_FULL_RETRY_INITIAL_DELAY: Duration = Duration::from_millis(5);
/// Ceiling for the queue-full backoff so the polling thread never stalls long.
const QUEUE_FULL_RETRY_MAX_DELAY: Duration = Duration::from_millis(200);
/// Rejection message produced by the render-net worker when its bounded
/// command queue is full. The two crates share the wording by contract.
const NETWORK_QUEUE_FULL_MESSAGE: &str = "network worker queue is full";

pub(super) struct BrowserApp {
    pub(super) tabs: TabModel,
    pub(super) pages: HashMap<TabId, PageState>,
    pub(super) fonts: Arc<SystemFontBackend>,
    pub(super) render_worker: PageRenderWorker,
    pub(super) network: NetworkWorker,
    pub(super) http_cache: HttpCache,
    pub(super) disk_cache: Option<DiskCacheWorker>,
    pub(super) pending_disk_clear: Option<DiskCacheOperationId>,
    pub(super) cache_clear_state: CacheClearUiState,
    pub(super) editor: AddressEditor,
    pub(super) content_editor: Option<ContentTextEditor>,
    pub(super) clipboard: NativeClipboard,
    pub(super) window: Option<Arc<Window>>,
    pub(super) context: Option<Context<Arc<Window>>>,
    pub(super) surface: Option<NativeSurface>,
    pub(super) layout: Option<ChromeLayout>,
    pub(super) frame: Vec<u32>,
    pub(super) frame_size: WindowSize<u32>,
    pub(super) frame_damage: FrameDamage,
    pub(super) theme: ChromeTheme,
    pub(super) cursor: Point,
    pub(super) hot: HitTarget,
    pub(super) cursor_icon: CursorIcon,
    pub(super) drag: Option<TabDrag>,
    pub(super) address_selecting: bool,
    pub(super) address_menu: Option<AddressContextMenu>,
    pub(super) modifiers: ModifiersState,
    pub(super) title_bar_clicks: TitleBarClickTracker,
    pub(super) address_clicks: AddressClickTracker,
    pub(super) left_pointer_down: bool,
    pub(super) started_at: Instant,
}

impl BrowserApp {
    pub(super) fn new(
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

    pub(super) fn initialize(
        &mut self,
        event_loop: &ActiveEventLoop,
    ) -> Result<(), Box<dyn Error>> {
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

    pub(super) fn relayout_and_render(&mut self, render_page: bool) {
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

    pub(super) fn schedule_page_render(
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

    pub(super) fn poll_render_worker(&mut self) {
        for completed in self.render_worker.drain_latest() {
            self.commit_render(completed);
        }
    }

    pub(super) fn commit_render(&mut self, completed: CompletedRender<PageRenderFrame>) {
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

    pub(super) fn compose_frame(&mut self, size: WindowSize<u32>) {
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

    pub(super) fn request_redraw(&self) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    pub(super) fn repaint_chrome(&mut self) {
        let Some(window) = &self.window else {
            return;
        };
        let size = window.inner_size();
        self.mark_chrome_damage(size);
        self.compose_frame(size);
        self.request_redraw();
    }

    pub(super) fn mark_page_damage(&mut self, size: WindowSize<u32>) {
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

    pub(super) fn mark_chrome_damage(&mut self, size: WindowSize<u32>) {
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

    pub(super) fn present(&mut self) -> Result<(), Box<dyn Error>> {
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

    pub(super) fn handle_tab_intent(&mut self, intent: TabIntent) {
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

    pub(super) fn sync_active_address(&mut self) {
        self.content_editor = None;
        self.editor.set_text(self.tabs.active().address.clone());
        self.editor.set_focused(false);
        if let Some(window) = &self.window {
            window.set_ime_allowed(false);
        }
    }

    pub(super) fn emit_navigation(&mut self, intent: NavigationIntent) {
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
    pub(super) fn drain_script_navigations(&mut self, id: TabId) {
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

    pub(super) fn navigate_target(
        &mut self,
        id: TabId,
        target: NavigationTarget,
        mode: HistoryMode,
    ) {
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

    pub(super) fn reload_active(&mut self) {
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

    pub(super) fn traverse_active(&mut self, forward: bool) {
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

    pub(super) fn start_network_navigation(&mut self, id: TabId, url: Url) {
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

    pub(super) fn submit_cached_fetch(&mut self, request: FetchRequest) -> CachedRequestHandle {
        let epoch = self.http_cache.epoch();
        let now = Instant::now();
        match self.http_cache.lookup(&request, now) {
            CacheLookup::Hit(response) => CachedRequestHandle::ready(request, epoch, *response),
            CacheLookup::Miss => {
                let submitted_request = self
                    .http_cache
                    .revalidation_request(&request, now)
                    .unwrap_or_else(|| request.clone());
                self.submit_with_queue_full_backoff(submitted_request, epoch)
            }
        }
    }

    /// Submits one request, retrying with bounded exponential backoff when the
    /// network worker rejects it because its command queue is momentarily full.
    ///
    /// Real pages fire dozens of concurrent subresource fetches; a burst like
    /// that must not turn into hard per-resource failures. Once the worker
    /// accepts the submission (or the retry budget is spent, leaving the last
    /// handle to settle through the normal polling path) the handle is returned
    /// unchanged. Every other immediate outcome, such as a stopped worker, is
    /// surfaced exactly as the worker produced it.
    fn submit_with_queue_full_backoff(
        &mut self,
        request: FetchRequest,
        epoch: CacheEpoch,
    ) -> CachedRequestHandle {
        let mut delay = QUEUE_FULL_RETRY_INITIAL_DELAY;
        let mut handle = self.network.submit(request.clone());
        for _ in 1..QUEUE_FULL_SUBMIT_ATTEMPTS {
            match handle.try_recv() {
                Err(TryRecvError::Empty) => {
                    return CachedRequestHandle::pending(request, epoch, handle);
                }
                Err(TryRecvError::Disconnected) => {
                    unreachable!("submit always answers its handle exactly once")
                }
                Ok(Err(FetchError::Transport(message)))
                    if message == NETWORK_QUEUE_FULL_MESSAGE => {}
                Ok(result) => {
                    return CachedRequestHandle {
                        request,
                        epoch,
                        state: CachedRequestState::Ready(Box::new(Some(result))),
                    };
                }
            }
            thread::sleep(delay);
            delay = delay.saturating_mul(2).min(QUEUE_FULL_RETRY_MAX_DELAY);
            handle = self.network.submit(request.clone());
        }
        CachedRequestHandle::pending(request, epoch, handle)
    }

    pub(super) fn submit_cached_batch(&mut self, requests: Vec<FetchRequest>) -> CachedBatchHandle {
        CachedBatchHandle::new(
            requests
                .into_iter()
                .map(|request| self.submit_cached_fetch(request))
                .collect(),
        )
    }

    pub(super) fn finish_cached_fetch(&mut self, completion: CachedFetchResult) -> FetchResult {
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

    pub(super) fn finish_cached_batch(
        &mut self,
        completions: Vec<CachedFetchResult>,
    ) -> Vec<FetchResult> {
        completions
            .into_iter()
            .map(|completion| self.finish_cached_fetch(completion))
            .collect()
    }

    pub(super) fn install_source(&mut self, id: TabId, source: PageSource, loading: bool) {
        let fallback_title = source.title.clone();
        let mut title = fallback_title;
        let mut address = source.target.display_address();
        if let Some(page) = self.pages.get_mut(&id) {
            page.set_source(source);
            page.sync_committed_title();
            title.clone_from(&page.navigation.committed().title);
            address = page.navigation.committed().target.display_address();
        }
        self.tabs.update(id, title, address);
        self.tabs.set_loading(id, loading);
        if id == self.tabs.active_id() {
            self.sync_active_address();
            self.relayout_and_render(true);
        } else {
            self.repaint_chrome();
        }
        self.request_redraw();
    }

    /// Propagate the document's `<title>` (including script-driven updates) to
    /// the tab label and window title.
    pub(super) fn sync_page_title(&mut self, id: TabId) {
        let Some(page) = self.pages.get_mut(&id) else {
            return;
        };
        if !page.sync_committed_title() {
            return;
        }
        if env::var_os("RENDER_DEBUG_FRAME").is_some() {
            eprintln!(
                "render-browser tab title update -> {:?}",
                page.navigation.committed().title
            );
        }
        let title = page.navigation.committed().title.clone();
        let address = page.navigation.committed().target.display_address();
        self.tabs.update(id, title, address);
        if id == self.tabs.active_id() {
            self.update_window_title();
        }
        self.repaint_chrome();
    }

    pub(super) fn start_external_style_sheets(&mut self, id: TabId, plan: StylesheetFetchPlan) {
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

    pub(super) fn start_classic_scripts(&mut self, id: TabId) {
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

        self.sync_page_title(id);
        if loading_complete {
            self.tabs.set_loading(id, false);
        }
        if rerender {
            self.schedule_page_render_for_tab(id);
        }
        self.repaint_chrome();
        self.request_redraw();
    }

    pub(super) fn start_images(&mut self, id: TabId) {
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

    pub(super) fn poll_network(&mut self) {
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

    pub(super) fn finish_network_navigation(
        &mut self,
        id: TabId,
        requested_url: Url,
        result: FetchResult,
    ) {
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

    pub(super) fn finish_external_style_sheets(&mut self, id: TabId, results: Vec<FetchResult>) {
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

    pub(super) fn finish_classic_scripts(&mut self, id: TabId, results: Vec<FetchResult>) {
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

        self.sync_page_title(id);
        if rerender {
            self.schedule_page_render_for_tab(id);
        }
        self.start_images(id);
        self.start_classic_scripts(id);
        self.repaint_chrome();
        self.request_redraw();
    }

    pub(super) fn finish_images(&mut self, id: TabId, results: Vec<FetchResult>) {
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

    pub(super) fn schedule_page_render_for_tab(&mut self, id: TabId) {
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

    pub(super) fn has_pending_network(&self) -> bool {
        self.pages.values().any(|page| {
            page.navigation.pending.is_some()
                || page.pending_style_sheets.is_some()
                || page.pending_scripts.is_some()
                || page.pending_images.is_some()
        })
    }

    pub(super) fn has_pending_script_work(&self) -> bool {
        self.pages.values().any(PageState::has_pending_script_work)
    }

    pub(super) fn handle_pointer_press(&mut self, event_loop: &ActiveEventLoop) {
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

    pub(super) fn handle_content_press(&mut self) {
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
            self.sync_page_title(id);
        }
        let submit_allowed = if default_allowed {
            let form = hit_node.and_then(|node| {
                let page = self.pages.get(&id)?;
                content_interaction::submit_form_for_node(page.page.document().dom(), node)
            });
            form.is_none_or(|form| {
                let task = self
                    .pages
                    .get_mut(&id)
                    .and_then(|page| page.page.queue_submit_event(form).ok());
                let allowed = task.is_none_or(|task| {
                    self.pages.get_mut(&id).is_some_and(|page| {
                        let (_, defaults) = page.run_page_turns();
                        defaults.get(&task).copied().unwrap_or(true)
                    })
                });
                self.drain_script_navigations(id);
                allowed
            })
        } else {
            false
        };
        // Recompute the target after click/submit listeners ran: handlers are
        // allowed to update the live input value or form action.
        let navigation = submit_allowed
            .then_some(hit_node)
            .flatten()
            .and_then(|hit_node| {
                let page = self.pages.get(&id)?;
                content_interaction::get_content_navigation_target(
                    page.page.document().dom(),
                    hit_node,
                    &page.navigation.committed().target.history_url(),
                )
            });
        if let Some(url) = navigation {
            self.navigate_target(id, NavigationTarget::from_url(url), HistoryMode::Push);
        }
        self.repaint_chrome();
    }

    pub(super) fn is_cache_clear_control(
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

    pub(super) fn clear_http_cache(&mut self) {
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

    pub(super) fn poll_disk_cache(&mut self) {
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

    pub(super) fn refresh_settings_pages(&mut self) {
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

    pub(super) fn content_text_input_value(
        &self,
        tab: TabId,
        node: render_core::dom::NodeId,
    ) -> Option<String> {
        let dom = self.pages.get(&tab)?.page.document().dom();
        content_text_input_value(dom, node)
    }

    pub(super) fn content_wrapper_control(
        &self,
        tab: TabId,
        node: render_core::dom::NodeId,
    ) -> Option<render_core::dom::NodeId> {
        let page = self.pages.get(&tab)?;
        content_wrapper_control(page.page.document().dom(), &page.geometry, node)
    }

    pub(super) fn content_editable_node(
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

    pub(super) fn sync_content_editor(&mut self) {
        let Some(content) = self.content_editor.as_ref() else {
            return;
        };
        let tab = content.tab;
        let node = content.node;
        let value = content.editor.text().to_owned();
        if let Some(page) = self.pages.get_mut(&tab)
            && content_interaction::set_content_text_value(
                page.page.document_mut().dom_mut(),
                node,
                &value,
            )
            .is_ok()
            && page.page.queue_input_event(node).is_ok()
        {
            let (rendered, _) = page.run_page_turns();
            if rendered {
                self.schedule_page_render_for_tab(tab);
            }
            self.drain_script_navigations(tab);
            self.sync_page_title(tab);
            return;
        }
        self.schedule_page_render_for_tab(tab);
    }

    pub(super) fn content_node_at_cursor(&self) -> Option<render_core::dom::NodeId> {
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
        // Controls can be visually empty (for example an input whose
        // background is supplied by an unsupported CSS image), so retain a
        // geometry-based hit target for all HTML interactive elements.
        let point = PhysicalPoint {
            x: self.cursor.x,
            y: self.cursor.y - self.layout.as_ref()?.chrome_height as f32 + page.scroll.offset_y(),
        };
        let geometry_hit = page
            .geometry
            .iter()
            .filter_map(|(raw_node, rect)| {
                let node = render_core::dom::NodeId::from_u64(*raw_node);
                let interactive =
                    render_core::interaction::activation_plan(page.page.document().dom(), node)
                        .is_some()
                        || content_interaction::is_content_editable(
                            page.page.document().dom(),
                            node,
                        )
                        || content_interaction::is_clickable_wrapper(
                            page.page.document().dom(),
                            node,
                        );
                let contains = point.x >= rect.x
                    && point.x < rect.x + rect.width
                    && point.y >= rect.y
                    && point.y < rect.y + rect.height;
                (interactive && contains && rect.width > 0.0 && rect.height > 0.0)
                    .then_some((rect.width * rect.height, node))
            })
            .min_by(|(left, _), (right, _)| left.total_cmp(right))
            .map(|(_, node)| node);

        let display_hit = page.display_list.as_ref().and_then(|display_list| {
            content_interaction::hit_test_content_regions(
                display_list
                    .items()
                    .iter()
                    .map(|item| content_interaction::ContentHitRegion {
                        bounds: item.bounds,
                        source: item.source,
                        coordinate_space: item.coordinate_space,
                        hit_testable: content_interaction::is_content_hit_command(&item.command),
                    }),
                self.cursor,
                self.layout.as_ref()?.chrome_height,
                PhysicalPoint {
                    x: 0.0,
                    y: page.scroll.offset_y(),
                },
            )
        });
        // Prefer the geometry target when paint only exposed an ancestor
        // background. Otherwise preserve paint order for links and scripted
        // containers whose event listener lives above the painted node.
        if let (Some(painted), Some(control)) = (display_hit, geometry_hit)
            && painted != control
            && render_core::interaction::activation_plan(page.page.document().dom(), painted)
                .is_none()
        {
            return Some(control);
        }
        display_hit.or(geometry_hit)
    }

    pub(super) fn handle_context_menu_press(&mut self) {
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

    pub(super) fn handle_window_action(
        &mut self,
        event_loop: &ActiveEventLoop,
        action: WindowAction,
    ) {
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

    pub(super) fn handle_cursor_move(&mut self, position: PhysicalPosition<f64>) {
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
                    content_interaction::get_content_navigation_target(
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

    pub(super) fn handle_pointer_release(&mut self) {
        self.left_pointer_down = false;
        self.drag = None;
        if self.address_selecting {
            self.address_selecting = false;
            self.editor.finish_pointer_selection();
            self.repaint_chrome();
        }
    }

    pub(super) fn handle_mouse_wheel(&mut self, delta: MouseScrollDelta) {
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

    pub(super) fn handle_keyboard(&mut self, event: &winit::event::KeyEvent) {
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
    pub(super) fn forward_keydown_to_page(&mut self, event: &winit::event::KeyEvent) {
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
            self.sync_page_title(id);
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "keyboard editing keeps each native control operation explicit"
    )]
    pub(super) fn handle_content_keyboard(&mut self, event: &winit::event::KeyEvent) -> bool {
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
                let key_task = self
                    .pages
                    .get_mut(&tab)
                    .and_then(|page| page.page.queue_keydown_at(node, "Enter").ok());
                let key_allowed = key_task.is_none_or(|task| {
                    self.pages.get_mut(&tab).is_some_and(|page| {
                        let (_, defaults) = page.run_page_turns();
                        defaults.get(&task).copied().unwrap_or(true)
                    })
                });
                self.drain_script_navigations(tab);
                self.content_editor = None;
                if key_allowed {
                    let form = self.pages.get(&tab).and_then(|page| {
                        content_interaction::associated_form_for_node(
                            page.page.document().dom(),
                            node,
                        )
                    });
                    let submit_allowed = form.is_none_or(|form| {
                        let task = self
                            .pages
                            .get_mut(&tab)
                            .and_then(|page| page.page.queue_submit_event(form).ok());
                        task.is_none_or(|task| {
                            self.pages.get_mut(&tab).is_some_and(|page| {
                                let (_, defaults) = page.run_page_turns();
                                defaults.get(&task).copied().unwrap_or(true)
                            })
                        })
                    });
                    self.drain_script_navigations(tab);
                    let target = submit_allowed
                        .then(|| {
                            self.pages.get(&tab).and_then(|page| {
                                render_core::interaction::plan_form_submission(
                                    page.page.document().dom(),
                                    node,
                                    &page.navigation.committed().target.history_url(),
                                )
                                .ok()
                            })
                        })
                        .flatten()
                        .filter(|submission| submission.method == FormMethod::Get)
                        .map(|submission| submission.target);
                    if let Some(url) = target {
                        self.navigate_target(
                            tab,
                            NavigationTarget::from_url(url),
                            HistoryMode::Push,
                        );
                    }
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

    pub(super) fn update_window_title(&self) {
        let Some(window) = &self.window else {
            return;
        };
        window.set_title(&format!("{} - rENDER", self.tabs.active().title));
    }

    pub(super) fn report_and_exit(
        event_loop: &ActiveEventLoop,
        operation: &str,
        error: &dyn fmt::Display,
    ) {
        eprintln!("render-browser could not {operation}: {error}");
        event_loop.exit();
    }
}

pub(super) struct ContentTextEditor {
    pub(super) tab: TabId,
    pub(super) node: render_core::dom::NodeId,
    pub(super) editor: AddressEditor,
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
        let mut title_candidates = Vec::new();
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
                    if revision_after != revision_before {
                        rendered_active |= *id == active;
                        title_candidates.push(*id);
                    }
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
        for id in title_candidates {
            self.sync_page_title(id);
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

pub(super) fn key_character_is(key: &Key, expected: &str) -> bool {
    matches!(key, Key::Character(value) if value.eq_ignore_ascii_case(expected))
}

/// Map a winit key event to the DOM `KeyboardEvent.key` string it represents.
pub(super) fn page_key_name(event: &winit::event::KeyEvent) -> Option<String> {
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
pub(super) enum HostPlatform {
    MacOs,
    Other,
}

pub(super) const fn primary_modifier_for(
    platform: HostPlatform,
    control: bool,
    command: bool,
) -> bool {
    match platform {
        HostPlatform::MacOs => command,
        HostPlatform::Other => control,
    }
}

pub(super) fn primary_modifier_active(modifiers: ModifiersState) -> bool {
    let platform = if cfg!(target_os = "macos") {
        HostPlatform::MacOs
    } else {
        HostPlatform::Other
    };
    primary_modifier_for(platform, modifiers.control_key(), modifiers.super_key())
}

pub(super) fn wheel_document_delta_y(delta: MouseScrollDelta) -> f32 {
    match delta {
        MouseScrollDelta::LineDelta(_, y) => -y * SCROLL_LINE_PIXELS,
        MouseScrollDelta::PixelDelta(position) => -finite_f32(position.y),
    }
}

pub(super) fn address_shortcut(key: &Key, shift: bool) -> Option<AddressCommand> {
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
pub(super) fn finite_f32(value: f64) -> f32 {
    debug_assert!(value.is_finite());
    value as f32
}

pub(super) const fn theme_from_winit(theme: Theme) -> ChromeTheme {
    match theme {
        Theme::Light => ChromeTheme::Light,
        Theme::Dark => ChromeTheme::Dark,
    }
}
