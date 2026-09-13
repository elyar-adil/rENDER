//! Native browser shell for the self-owned Rust rendering pipeline.
#![allow(clippy::cast_precision_loss)]
use crate::UserEvent;
use crate::frame::geometry_from_layout;
use crate::frame::surface_to_softbuffer;
use crate::frame::viewport_dimension;
use render_browser::font_backend::SystemFontBackend;
use render_browser::resources::StylesheetFetchPlan;
use render_browser::resources::StylesheetResourceDiagnostic;
use render_browser::resources::apply_stylesheet_batch;
use render_browser::resources::plan_external_style_sheets;
use render_browser::worker::RenderCancellation;
use render_browser::worker::RenderFailure;
use render_browser::worker::RenderJob;
use render_browser::worker::RenderWorker;
use render_browser::worker::RenderWorkerOptions;
use render_core::css::computed::ComputedStyle;
use render_core::document::Document;
use render_core::document::DocumentBackends;
use render_core::document::DocumentRenderOptions;
use render_core::document::ExternalStyleSheets;
use render_core::image::ImageResources;
use render_core::js::ElementRect;
use render_core::layout::PhysicalPoint;
use render_core::layout::PhysicalSize;
use render_core::paint::Color;
use render_core::paint::CpuRasterizer;
use render_core::paint::DisplayList;
use render_core::paint::PaintScene;
use render_core::paint::RasterControl;
use render_core::paint::RasterRequest;
use render_net::FetchResult;
use render_net::Url;
use std::collections::BTreeMap;
use std::env;
use std::sync::Arc;
use winit::dpi::PhysicalSize as WindowSize;
use winit::event_loop::EventLoopProxy;

#[derive(Clone, Debug)]
pub(super) enum PageRenderPayload {
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
pub(super) struct FullPageRenderPayload {
    pub(super) document: Document,
    pub(super) base_url: Url,
    pub(super) external_style_sheets: ExternalStyleSheets,
    pub(super) style_batch: Option<(StylesheetFetchPlan, Vec<FetchResult>)>,
    pub(super) discover_external_styles: bool,
    pub(super) images: ImageResources,
}

#[derive(Debug)]
pub(super) struct PageRenderFrame {
    pub(super) frame: Vec<u32>,
    pub(super) viewport: WindowSize<u32>,
    pub(super) display_list: Option<Arc<DisplayList>>,
    pub(super) paint_scene: Option<Arc<PaintScene>>,
    pub(super) raster_background: Color,
    pub(super) content_height: f32,
    pub(super) viewport_height: f32,
    pub(super) applied_style_sheets: Option<ExternalStyleSheets>,
    pub(super) style_plan: Option<StylesheetFetchPlan>,
    pub(super) style_diagnostics: Vec<StylesheetResourceDiagnostic>,
    pub(super) computed_styles: Option<BTreeMap<render_core::dom::NodeId, ComputedStyle>>,
    pub(super) geometry: Option<BTreeMap<u64, ElementRect>>,
    pub(super) document_revision: u64,
}

pub(super) type PageRenderWorker = RenderWorker<PageRenderPayload, PageRenderFrame>;

pub(super) struct BrowserRasterControl<'a> {
    pub(super) cancellation: &'a RenderCancellation,
}

impl RasterControl for BrowserRasterControl<'_> {
    fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }
}

pub(super) fn start_render_worker(
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
pub(super) fn process_page_render(
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
