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
use render_browser::resources::apply_stylesheet_batch_rematched;
use render_browser::resources::plan_external_style_sheets;
use render_browser::worker::RenderCancellation;
use render_browser::worker::RenderFailure;
use render_browser::worker::RenderJob;
use render_browser::worker::RenderWorker;
use render_browser::worker::RenderWorkerOptions;
use render_core::css::cascade::media_query_list_matches;
use render_core::css::computed::ComputedStyle;
use render_core::css::computed::ComputedValue;
use render_core::css::selector::MatchContext;
use render_core::css::stylesheet::StyleSheet;
use render_core::document::Document;
use render_core::document::DocumentBackends;
use render_core::document::DocumentRenderOptions;
use render_core::document::DocumentRenderOutput;
use render_core::document::ExternalStyleSheets;
use render_core::dom::Dom;
use render_core::dom::NodeId;
use render_core::image::ImageResources;
use render_core::js::ElementRect;
use render_core::layout::FragmentKind;
use render_core::layout::PhysicalPoint;
use render_core::layout::PhysicalSize;
use render_core::paint::Color;
use render_core::paint::CpuRasterizer;
use render_core::paint::DisplayCommand;
use render_core::paint::DisplayList;
use render_core::paint::PaintScene;
use render_core::paint::RasterControl;
use render_core::paint::RasterRequest;
use render_net::FetchResult;
use render_net::Url;
use std::collections::BTreeMap;
use std::collections::HashSet;
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
                    // Page scripts mutate the DOM while a large stylesheet is
                    // in flight, so the plan's revision routinely lags the
                    // snapshot. Re-plan against the snapshot (which already
                    // contains those mutations) and re-match the fetched CSS
                    // by URL instead of discarding it: this frame then renders
                    // styled, and the coordinator's next cycle covers any
                    // brand-new links.
                    let application = if plan.revision == document.dom().revision() {
                        apply_stylesheet_batch(&document, &plan, results)
                    } else {
                        let fresh = plan_external_style_sheets(
                            &document,
                            &base_url,
                            DocumentRenderOptions::default().document_limits,
                        );
                        apply_stylesheet_batch_rematched(&fresh, &plan, results)
                    };
                    // A follow-up batch contains only newly discovered links.
                    // Keep earlier sheets whose owner and URL still exist in
                    // this snapshot, while dropping detached or retargeted
                    // links. Replacing the whole map here made a dynamic CSS
                    // load strip every stylesheet fetched at startup.
                    let merged = merge_current_style_sheets(
                        &document,
                        &base_url,
                        &external_style_sheets,
                        &application.style_sheets,
                    );
                    (merged.clone(), Some(merged), application.diagnostics)
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
                let report = frame_style_report(
                    &document,
                    &output,
                    &style_sheets,
                    &base_url,
                    output.layout.fragments.viewport,
                );
                eprintln!(
                    "render-browser frame breakdown display_items={} content_height={} kinds={:?} display_none={} vanished={} zero_size_boxes={}",
                    report.display_items,
                    report.content_height,
                    report.item_kinds,
                    report.display_none,
                    report.vanished,
                    report.zero_size_boxes,
                );
                for line in &report.display_none_causes {
                    eprintln!("render-browser {line}");
                }
                for line in &report.vanished_causes {
                    eprintln!("render-browser {line}");
                }
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
                let dom = document.dom();
                let mut pending = vec![dom.document()];
                while let Some(node) = pending.pop() {
                    if let Some(render_core::dom::NodeKind::Element(element)) =
                        dom.node(node).map(render_core::dom::Node::kind)
                        && element.local_name == "img"
                        && let Some(loaded) = images.get_for_node(node)
                    {
                        let fragments = output
                            .layout
                            .fragments
                            .iter()
                            .filter(|fragment| fragment.source == Some(node))
                            .map(|fragment| fragment.rect)
                            .collect::<Vec<_>>();
                        let opacity = output.styles.get(&node).and_then(|style| {
                            style
                                .get("opacity")
                                .map(render_core::css::computed::ComputedValue::css_text)
                        });
                        eprintln!(
                            "render-browser loaded img node={node:?} resource={:?} fragments={fragments:?} opacity={opacity:?}",
                            loaded.id
                        );
                    }
                    pending.extend(dom.children(node).unwrap_or_default().iter().copied());
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

/// Retains only sheets linked by the current DOM, preferring the newest
/// parsed response when a link was fetched again.
pub(super) fn merge_current_style_sheets(
    document: &Document,
    base_url: &Url,
    existing: &ExternalStyleSheets,
    incoming: &ExternalStyleSheets,
) -> ExternalStyleSheets {
    let current = plan_external_style_sheets(
        document,
        base_url,
        DocumentRenderOptions::default().document_limits,
    );
    let mut merged = ExternalStyleSheets::default();
    for resource in &current.resources {
        let key = &resource.key;
        if let Some(sheet) = incoming.get(key).or_else(|| existing.get(key)) {
            merged.insert(key.clone(), sheet.clone());
        }
    }
    merged
}

/// How many elements a frame painted, and where the rest went.
pub(super) struct FrameStyleReport {
    pub(super) display_items: usize,
    pub(super) content_height: f32,
    pub(super) item_kinds: BTreeMap<&'static str, usize>,
    pub(super) display_none: usize,
    /// Elements whose computed `display` is not `none` yet which produced no
    /// layout box at all.
    pub(super) vanished: usize,
    pub(super) zero_size_boxes: usize,
    pub(super) display_none_causes: Vec<String>,
    pub(super) vanished_causes: Vec<String>,
}

/// Number of `display:none` nodes and vanished elements named per frame.
const CAUSE_SAMPLE_LIMIT: usize = 12;

/// Explains, for one frame, why elements are missing from the display list.
///
/// A styled frame must never paint *fewer* items than the unstyled frame that
/// preceded it, so a drop has to be attributable. Counting `display: none` is
/// not enough on its own: the useful question is which declaration decided it
/// and at which cascade origin, because a sheet that landed on the wrong owner
/// or a stale revision produces a broad, silent collapse. `vanished` covers the
/// complementary failure where an element kept its `display` value but never
/// produced a box.
fn frame_style_report(
    document: &Document,
    output: &DocumentRenderOutput,
    style_sheets: &ExternalStyleSheets,
    base_url: &Url,
    viewport: PhysicalSize,
) -> FrameStyleReport {
    let dom = document.dom();
    let mut item_kinds = BTreeMap::new();
    for item in output.display.list.items() {
        *item_kinds
            .entry(display_command_name(&item.command))
            .or_insert(0) += 1;
    }
    let mut box_sources = HashSet::new();
    let mut zero_size_boxes = 0_usize;
    for fragment in output.layout.fragments.iter() {
        if let Some(source) = fragment.source {
            box_sources.insert(source);
        }
        if matches!(&fragment.kind, FragmentKind::Box(_))
            && fragment.rect.size.width <= 0.0
            && fragment.rect.size.height <= 0.0
        {
            zero_size_boxes += 1;
        }
    }
    let mut display_none_nodes = Vec::new();
    let mut vanished_nodes = Vec::new();
    for (node, style) in &output.styles {
        let display = style
            .get("display")
            .map_or("block", ComputedValue::css_text);
        if display == "none" {
            display_none_nodes.push(*node);
        } else if !matches!(
            display,
            "inline" | "contents" | "table-row-group" | "table-row"
        ) && !box_sources.contains(node)
        {
            // An inline box has no fragment of its own, so only block-level and
            // atomic boxes are required to appear here. One of these missing
            // is a genuine collapse.
            vanished_nodes.push(*node);
        }
    }
    let sheets = ordered_applied_sheets(document, style_sheets, base_url);
    let context = match_context(document, viewport);
    let describe = |label: &str, nodes: &[NodeId]| -> Vec<String> {
        nodes
            .iter()
            .take(CAUSE_SAMPLE_LIMIT)
            .map(|node| {
                format!(
                    "{label} node {node:?} {} display-declaration={}",
                    describe_node(dom, *node),
                    display_declaration(dom, *node, &sheets, &context)
                )
            })
            .collect()
    };
    FrameStyleReport {
        display_items: output.display.list.items().len(),
        content_height: output.layout.fragments.scrollable_content_size.height,
        item_kinds,
        display_none: display_none_nodes.len(),
        vanished: vanished_nodes.len(),
        zero_size_boxes,
        display_none_causes: describe("display-none", &display_none_nodes),
        vanished_causes: describe("vanished", &vanished_nodes),
    }
}

const fn display_command_name(command: &DisplayCommand) -> &'static str {
    match command {
        DisplayCommand::SolidRect { .. } => "solid",
        DisplayCommand::Border(_) => "border",
        DisplayCommand::BoxShadow(_) => "shadow",
        DisplayCommand::PushClip(_) => "push-clip",
        DisplayCommand::PopClip => "pop-clip",
        DisplayCommand::PushTransform(_) => "push-transform",
        DisplayCommand::PopTransform => "pop-transform",
        DisplayCommand::GlyphRun(_) => "glyph",
        DisplayCommand::TextDecoration(_) => "decoration",
        DisplayCommand::TextShadow(_) => "text-shadow",
        DisplayCommand::ListMarker(_) => "list-marker",
        DisplayCommand::Image(_) => "image",
        DisplayCommand::LinearGradient(_) => "linear-gradient",
        DisplayCommand::RadialGradient(_) => "radial-gradient",
        DisplayCommand::Canvas { .. } => "canvas",
        DisplayCommand::PushStackingContext(_) => "push-stack",
        DisplayCommand::PopStackingContext => "pop-stack",
    }
}

/// Applied sheets in DOM source order, so a reported declaration can name the
/// link it came from.
pub(super) fn ordered_applied_sheets<'a>(
    document: &Document,
    style_sheets: &'a ExternalStyleSheets,
    base_url: &Url,
) -> Vec<(String, &'a StyleSheet)> {
    let plan = plan_external_style_sheets(
        document,
        base_url,
        DocumentRenderOptions::default().document_limits,
    );
    plan.resources
        .iter()
        .filter_map(|resource| {
            style_sheets
                .get(&resource.key)
                .map(|sheet| (resource.key.requested_url.as_str().to_owned(), sheet))
        })
        .collect()
}

pub(super) fn match_context(document: &Document, viewport: PhysicalSize) -> MatchContext {
    MatchContext {
        scope: None,
        quirks_mode: document.quirks_mode() == render_core::html::QuirksMode::Quirks,
        pseudo_element: None,
        focused: None,
        target: None,
        hovered: HashSet::new(),
        active: HashSet::new(),
        visited_links: HashSet::new(),
        viewport_width: Some(viewport.width),
        viewport_height: Some(viewport.height),
    }
}

pub(super) fn describe_node(dom: &Dom, node: NodeId) -> String {
    let mut out = String::new();
    if let Some(render_core::dom::NodeKind::Element(element)) =
        dom.node(node).map(render_core::dom::Node::kind)
    {
        out.push('<');
        out.push_str(&element.local_name);
        out.push('>');
    } else {
        out.push_str("non-element");
    }
    for attribute in ["id", "class"] {
        if let Ok(Some(value)) = dom.attribute(node, attribute) {
            out.push(' ');
            out.push_str(attribute);
            out.push_str("=\"");
            out.push_str(value);
            out.push('"');
        }
    }
    out
}

/// Cascade rank of one `display` declaration, with the report line it wins with.
type DisplayRank = (bool, (u32, u32, u32), u64, String);

pub(super) fn display_declaration(
    dom: &Dom,
    node: NodeId,
    sheets: &[(String, &StyleSheet)],
    context: &MatchContext,
) -> String {
    let mut best: Option<DisplayRank> = None;
    for (url, sheet) in sheets {
        for rule in &sheet.rules {
            if !rule
                .media
                .iter()
                .all(|query| media_query_list_matches(query, context))
            {
                continue;
            }
            if !render_core::css::selector::matches_selector_list(
                dom,
                node,
                &rule.selectors,
                context,
            ) {
                continue;
            }
            for declaration in &rule.declarations {
                if !declaration.name.eq_ignore_ascii_case("display") {
                    continue;
                }
                let specificity = rule.selectors.max_specificity();
                let rank = (
                    declaration.important,
                    (specificity.ids, specificity.classes, specificity.types),
                    rule.source_order,
                    format!(
                        "{url} rule#{} media={:?} !{} => display: {}",
                        rule.source_order,
                        rule.media,
                        if declaration.important {
                            "important"
                        } else {
                            "normal"
                        },
                        declaration.value.trim()
                    ),
                );
                if best.as_ref().is_none_or(|current| rank > *current) {
                    best = Some(rank);
                }
            }
        }
    }
    best.map_or_else(
        || "no matching author declaration (user-agent origin or inherited)".to_owned(),
        |(_, _, _, text)| text,
    )
}
