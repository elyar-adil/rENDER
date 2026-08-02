//! A revision-preserving HTML-to-pixels pipeline.
//!
//! Parsing and rendering are deliberately separate. Script bindings mutate the
//! [`Dom`] owned by [`Document`], then render that same tree at its new
//! revision; ordinary DOM updates never require reparsing the HTML source.

use std::collections::{BTreeMap, HashMap};

use url::Url;

use crate::css::cascade::{CascadeInput, CascadeOrigin};
use crate::css::computed::{
    ComputationDiagnostic, ComputationLimits, ComputedStyle, PropertyRegistry,
    compute_document_styles,
};
use crate::css::selector::MatchContext;
use crate::css::stylesheet::{StyleSheet, StyleSheetDiagnostic, parse_stylesheet};
use crate::dom::{Dom, DomRevision, ElementData, Node, NodeId, NodeKind};
use crate::html::{HtmlParseError, QuirksMode, parse_document};
use crate::image::ImageResources;
use crate::layout::{
    FormattingDiagnostic, FormattingLimits, FormattingTree, LayoutDiagnostic, LayoutOptions,
    LayoutOutput, PhysicalPoint, SimpleTextMeasurer, TextMeasurer, build_formatting_tree,
    layout_formatting_tree_with_images,
};
use crate::paint::{
    Color, CpuRasterOutput, CpuRasterizer, DisplayListBuildOutput, DisplayListBuilderOptions,
    DisplayListDiagnostic, GlyphMaskProvider, NoGlyphMasks, RasterDiagnostic, ReferenceTextShaper,
    TextShaper, build_display_list_with_images,
};

/// Minimal interoperable defaults used before a generated HTML UA sheet lands.
/// Longhands are intentional: shorthand expansion is a separate CSS feature.
const UA_STYLE_SHEET: &str = r#"
html, body, address, article, aside, blockquote, center, details, dialog, div,
dd, dl, dt, fieldset, figcaption, figure, footer, form, header, hgroup, hr,
legend, listing, main, menu, nav, ol, p, plaintext, pre, search, section, ul,
xmp, h1, h2, h3, h4, h5, h6 { display: block; }
li, summary { display: list-item; }
table { display: table; }
thead { display: table-header-group; }
tbody { display: table-row-group; }
tfoot { display: table-footer-group; }
tr { display: table-row; }
td, th { display: table-cell; }
caption { display: table-caption; }
colgroup { display: table-column-group; }
col { display: table-column; }
button, input, select, textarea { display: inline-block; }
input:not([type="hidden" i]) { width: 180px; min-height: 22px; padding-left: 4px; padding-right: 4px; border: 1px solid #888; }
input[type="hidden" i] { display: none; }
ruby { display: ruby; }
rb { display: ruby-base; }
rt { display: ruby-text; }
rtc { display: ruby-text-container; }
head, area, base, basefont, datalist, link, meta, noembed, noframes, param,
rp, script, source, style, template, title, track, [hidden] { display: none; }
body { margin-top: 8px; margin-right: 8px; margin-bottom: 8px; margin-left: 8px; }
"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DocumentLimits {
    /// Bound the DOM walk performed solely to discover author style sources.
    pub max_style_discovery_nodes: usize,
    pub max_embedded_style_sheets: usize,
    pub max_embedded_style_bytes: usize,
    /// Bounds all `<style>` and stylesheet `<link>` slots discovered in DOM
    /// order, including currently ineligible slots.
    pub max_author_style_slots: usize,
    pub max_external_style_sheets: usize,
    pub max_external_style_url_bytes: usize,
}

impl Default for DocumentLimits {
    fn default() -> Self {
        Self {
            max_style_discovery_nodes: 1_000_000,
            max_embedded_style_sheets: 4_096,
            max_embedded_style_bytes: 16 * 1_024 * 1_024,
            max_author_style_slots: 8_192,
            max_external_style_sheets: 4_096,
            max_external_style_url_bytes: 1024 * 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DocumentRenderOptions {
    pub document_limits: DocumentLimits,
    pub computation_limits: ComputationLimits,
    pub formatting_limits: FormattingLimits,
    pub layout: LayoutOptions,
    /// Document-space origin painted into the layout viewport. Layout itself
    /// always uses `layout.viewport`; this offset only affects compositing.
    pub scroll_offset: PhysicalPoint,
    pub display_list: DisplayListBuilderOptions,
    pub raster_background: Color,
}

impl Default for DocumentRenderOptions {
    fn default() -> Self {
        let display_list = DisplayListBuilderOptions::default();
        Self {
            document_limits: DocumentLimits::default(),
            computation_limits: ComputationLimits::default(),
            formatting_limits: FormattingLimits::default(),
            layout: LayoutOptions::default(),
            scroll_offset: PhysicalPoint::default(),
            raster_background: display_list.palette.canvas,
            display_list,
        }
    }
}

#[derive(Clone, Copy)]
pub struct DocumentBackends<'a> {
    pub text_measurer: &'a dyn TextMeasurer,
    pub text_shaper: &'a dyn TextShaper,
    pub glyph_masks: &'a dyn GlyphMaskProvider,
}

impl std::fmt::Debug for DocumentBackends<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("DocumentBackends { .. }")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentDiagnosticCode {
    ExternalStyleSheetUnsupported,
    ExternalStyleSheetUnresolved,
    InlineStyleUnsupported,
    MediaQueryUnsupported,
    NonCssStyleType,
    QuirksModeUnsupported,
    StyleDiscoveryNodeLimit,
    EmbeddedStyleLimit,
    EmbeddedStyleBytesLimit,
    AuthorStyleSlotLimit,
    ExternalStyleSheetLimit,
    ExternalStyleSheetUrlBytesLimit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentDiagnostic {
    pub node: Option<NodeId>,
    pub code: DocumentDiagnosticCode,
    pub message: String,
}

/// Why an author stylesheet slot is or is not applicable to the current
/// screen rendering environment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthorStyleEligibility {
    pub type_is_css: bool,
    pub media_matches: bool,
    /// False means some media query syntax could not yet be evaluated. Other
    /// valid entries in the comma-separated media list may still match.
    pub media_fully_supported: bool,
}

impl AuthorStyleEligibility {
    #[must_use]
    pub const fn is_eligible(self) -> bool {
        self.type_is_css && self.media_matches
    }
}

/// The source represented by an author style slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthorStyleSource {
    Embedded,
    External {
        href: String,
        /// URL resolved against the caller-supplied document base URL.
        resolved_url: Option<Url>,
    },
}

/// One `<style>` or `<link rel=stylesheet href>` in DOM tree order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorStyleSlot {
    pub owner: NodeId,
    pub source_order: usize,
    pub source: AuthorStyleSource,
    pub eligibility: AuthorStyleEligibility,
}

/// Revision-bound stylesheet discovery result. Callers can fetch eligible
/// external slots in parallel, then inject their parsed sheets by key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorStyleDiscovery {
    pub revision: DomRevision,
    pub slots: Vec<AuthorStyleSlot>,
    pub diagnostics: Vec<DocumentDiagnostic>,
}

/// Stable identity for fetched CSS. The URL is the resolved link request URL,
/// not the post-redirect response URL; redirect bookkeeping remains a caller
/// responsibility.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ExternalStyleSheetKey {
    pub owner: NodeId,
    pub requested_url: Url,
}

impl ExternalStyleSheetKey {
    #[must_use]
    pub const fn new(owner: NodeId, requested_url: Url) -> Self {
        Self {
            owner,
            requested_url,
        }
    }
}

/// Parsed external stylesheets supplied by a network/document coordinator.
#[derive(Clone, Debug, Default)]
pub struct ExternalStyleSheets {
    entries: HashMap<ExternalStyleSheetKey, StyleSheet>,
}

impl ExternalStyleSheets {
    pub fn insert(&mut self, key: ExternalStyleSheetKey, sheet: StyleSheet) -> Option<StyleSheet> {
        self.entries.insert(key, sheet)
    }

    pub fn insert_css(&mut self, key: ExternalStyleSheetKey, source: &str) -> Option<StyleSheet> {
        self.insert(key, parse_stylesheet(source))
    }

    #[must_use]
    pub fn get(&self, key: &ExternalStyleSheetKey) -> Option<&StyleSheet> {
        self.entries.get(key)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeStyleSheetDiagnostic {
    pub node: Option<NodeId>,
    pub diagnostic: StyleSheetDiagnostic,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeComputationDiagnostic {
    pub node: NodeId,
    pub diagnostic: ComputationDiagnostic,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DocumentRenderDiagnostics {
    pub document: Vec<DocumentDiagnostic>,
    pub style_sheets: Vec<NodeStyleSheetDiagnostic>,
    pub computed_styles: Vec<NodeComputationDiagnostic>,
    pub formatting: Vec<FormattingDiagnostic>,
    pub layout: Vec<LayoutDiagnostic>,
    pub display_list: Vec<DisplayListDiagnostic>,
    pub raster: Vec<RasterDiagnostic>,
}

#[derive(Clone, Debug)]
pub struct Document {
    dom: Dom,
    html_errors: Vec<HtmlParseError>,
    quirks_mode: QuirksMode,
}

impl Document {
    /// Parse an HTML document once. Subsequent script-driven updates should use
    /// [`Self::dom_mut`] and render the resulting DOM revision directly.
    #[must_use]
    pub fn parse(html: &str) -> Self {
        let parsed = parse_document(html);
        Self {
            dom: parsed.dom,
            html_errors: parsed.errors,
            quirks_mode: parsed.quirks_mode,
        }
    }

    #[must_use]
    pub const fn dom(&self) -> &Dom {
        &self.dom
    }

    pub const fn dom_mut(&mut self) -> &mut Dom {
        &mut self.dom
    }

    #[must_use]
    pub fn html_errors(&self) -> &[HtmlParseError] {
        &self.html_errors
    }

    #[must_use]
    pub const fn quirks_mode(&self) -> QuirksMode {
        self.quirks_mode
    }

    /// Execute style, layout, display-list construction, and CPU painting for
    /// the current DOM revision.
    #[must_use]
    pub fn render(
        &self,
        options: DocumentRenderOptions,
        backends: DocumentBackends<'_>,
    ) -> DocumentRenderOutput {
        render_dom(
            &self.dom,
            self.quirks_mode,
            options,
            backends,
            None,
            &ExternalStyleSheets::default(),
            None,
        )
    }

    #[must_use]
    pub fn render_with_images(
        &self,
        options: DocumentRenderOptions,
        backends: DocumentBackends<'_>,
        images: &ImageResources,
    ) -> DocumentRenderOutput {
        render_dom(
            &self.dom,
            self.quirks_mode,
            options,
            backends,
            None,
            &ExternalStyleSheets::default(),
            Some(images),
        )
    }

    /// Discovers embedded and external author stylesheet slots for the current
    /// DOM revision. This is a pure discovery step and performs no I/O.
    #[must_use]
    pub fn discover_author_style_slots(
        &self,
        base_url: &Url,
        limits: DocumentLimits,
    ) -> AuthorStyleDiscovery {
        discover_author_style_slots(&self.dom, Some(base_url), limits)
    }

    /// Renders with parsed external stylesheets supplied by the caller. Slots
    /// are rediscovered for the current DOM revision and cascaded in DOM order.
    #[must_use]
    pub fn render_with_external_style_sheets(
        &self,
        options: DocumentRenderOptions,
        backends: DocumentBackends<'_>,
        base_url: &Url,
        external: &ExternalStyleSheets,
    ) -> DocumentRenderOutput {
        render_dom(
            &self.dom,
            self.quirks_mode,
            options,
            backends,
            Some(base_url),
            external,
            None,
        )
    }

    #[must_use]
    pub fn render_with_external_style_sheets_and_images(
        &self,
        options: DocumentRenderOptions,
        backends: DocumentBackends<'_>,
        base_url: &Url,
        external: &ExternalStyleSheets,
        images: &ImageResources,
    ) -> DocumentRenderOutput {
        render_dom(
            &self.dom,
            self.quirks_mode,
            options,
            backends,
            Some(base_url),
            external,
            Some(images),
        )
    }

    /// Deterministic reference path suitable for conformance tests.
    #[must_use]
    pub fn render_reference(&self, options: DocumentRenderOptions) -> DocumentRenderOutput {
        self.render(
            options,
            DocumentBackends {
                text_measurer: &SimpleTextMeasurer,
                text_shaper: &ReferenceTextShaper,
                glyph_masks: &NoGlyphMasks,
            },
        )
    }

    /// Deterministic reference render with caller-supplied external CSS.
    #[must_use]
    pub fn render_reference_with_external_style_sheets(
        &self,
        options: DocumentRenderOptions,
        base_url: &Url,
        external: &ExternalStyleSheets,
    ) -> DocumentRenderOutput {
        self.render_with_external_style_sheets(
            options,
            DocumentBackends {
                text_measurer: &SimpleTextMeasurer,
                text_shaper: &ReferenceTextShaper,
                glyph_masks: &NoGlyphMasks,
            },
            base_url,
            external,
        )
    }
}

#[derive(Clone, Debug)]
pub struct DocumentRenderOutput {
    pub revision: DomRevision,
    pub styles: BTreeMap<NodeId, ComputedStyle>,
    pub formatting: FormattingTree,
    pub layout: LayoutOutput,
    /// Effective, clamped document-space origin used for this raster.
    pub paint_viewport_origin: PhysicalPoint,
    pub display: DisplayListBuildOutput,
    pub raster: CpuRasterOutput,
    pub diagnostics: DocumentRenderDiagnostics,
}

fn render_dom(
    dom: &Dom,
    quirks_mode: QuirksMode,
    options: DocumentRenderOptions,
    backends: DocumentBackends<'_>,
    base_url: Option<&Url>,
    external: &ExternalStyleSheets,
    images: Option<&ImageResources>,
) -> DocumentRenderOutput {
    let ua_sheet = parse_stylesheet(UA_STYLE_SHEET);
    let collected = collect_author_style_sheets(dom, base_url, external, options.document_limits);
    let mut cascade_inputs = Vec::with_capacity(collected.sheets.len().saturating_add(1));
    cascade_inputs.push(CascadeInput {
        sheet: &ua_sheet,
        origin: CascadeOrigin::UserAgent,
    });
    cascade_inputs.extend(collected.sheets.iter().map(|(_, sheet)| CascadeInput {
        sheet,
        origin: CascadeOrigin::Author,
    }));

    let styles = compute_document_styles(
        dom,
        &cascade_inputs,
        &PropertyRegistry::standard_baseline(),
        &options.computation_limits,
        &MatchContext::default(),
    );
    let formatting = build_formatting_tree(dom, &styles, &options.formatting_limits);
    let layout = layout_formatting_tree_with_images(
        dom,
        &formatting,
        &styles,
        options.layout,
        backends.text_measurer,
        images,
    );
    let display = build_display_list_with_images(
        &layout.fragments,
        &formatting,
        &styles,
        options.display_list,
        backends.text_shaper,
        images,
    );
    let paint_viewport_origin = layout.fragments.clamp_scroll_offset(options.scroll_offset);
    let raster = CpuRasterizer.rasterize_viewport_with_images(
        &display.list,
        options.raster_background,
        backends.glyph_masks,
        paint_viewport_origin,
        images,
    );

    let mut document_diagnostics = collected.diagnostics;
    if quirks_mode != QuirksMode::NoQuirks {
        document_diagnostics.insert(
            0,
            DocumentDiagnostic {
                node: None,
                code: DocumentDiagnosticCode::QuirksModeUnsupported,
                message: format!(
                    "{} CSS quirks are not implemented; standards-mode CSS semantics were used",
                    quirks_mode.as_str()
                ),
            },
        );
    }
    let diagnostics = DocumentRenderDiagnostics {
        document: document_diagnostics,
        style_sheets: std::iter::once((None, &ua_sheet))
            .chain(
                collected
                    .sheets
                    .iter()
                    .map(|(node, sheet)| (Some(*node), sheet)),
            )
            .flat_map(|(node, sheet)| {
                sheet
                    .diagnostics
                    .iter()
                    .cloned()
                    .map(move |diagnostic| NodeStyleSheetDiagnostic { node, diagnostic })
            })
            .collect(),
        computed_styles: styles
            .iter()
            .flat_map(|(node, style)| {
                style.diagnostics().iter().cloned().map(move |diagnostic| {
                    NodeComputationDiagnostic {
                        node: *node,
                        diagnostic,
                    }
                })
            })
            .collect(),
        formatting: formatting.diagnostics().to_vec(),
        layout: layout.diagnostics.clone(),
        display_list: display.diagnostics.clone(),
        raster: raster.diagnostics.clone(),
    };

    DocumentRenderOutput {
        revision: dom.revision(),
        styles,
        formatting,
        layout,
        paint_viewport_origin,
        display,
        raster,
        diagnostics,
    }
}

struct CollectedStyleSheets {
    sheets: Vec<(NodeId, StyleSheet)>,
    diagnostics: Vec<DocumentDiagnostic>,
}

fn collect_author_style_sheets(
    dom: &Dom,
    base_url: Option<&Url>,
    external: &ExternalStyleSheets,
    limits: DocumentLimits,
) -> CollectedStyleSheets {
    let discovery = discover_author_style_slots(dom, base_url, limits);
    let mut sheets = Vec::new();
    let mut diagnostics = discovery.diagnostics;
    let mut style_bytes = 0_usize;
    let mut embedded_sheet_count = 0_usize;

    for slot in discovery.slots {
        if !slot.eligibility.is_eligible() {
            continue;
        }
        match slot.source {
            AuthorStyleSource::Embedded => collect_style_element(
                dom,
                slot.owner,
                limits,
                &mut embedded_sheet_count,
                &mut style_bytes,
                &mut sheets,
                &mut diagnostics,
            ),
            AuthorStyleSource::External {
                resolved_url: Some(requested_url),
                ..
            } => {
                let key = ExternalStyleSheetKey::new(slot.owner, requested_url.clone());
                if let Some(sheet) = external.get(&key) {
                    sheets.push((slot.owner, sheet.clone()));
                } else {
                    diagnostics.push(DocumentDiagnostic {
                        node: Some(slot.owner),
                        code: DocumentDiagnosticCode::ExternalStyleSheetUnsupported,
                        message: format!(
                            "external stylesheet bytes were not supplied for {requested_url}"
                        ),
                    });
                }
            }
            AuthorStyleSource::External {
                resolved_url: None, ..
            } if base_url.is_none() => diagnostics.push(DocumentDiagnostic {
                node: Some(slot.owner),
                code: DocumentDiagnosticCode::ExternalStyleSheetUnsupported,
                message: "external stylesheet requires a document base URL and supplied bytes"
                    .to_owned(),
            }),
            AuthorStyleSource::External { .. } => {}
        }
    }
    CollectedStyleSheets {
        sheets,
        diagnostics,
    }
}

fn discover_author_style_slots(
    dom: &Dom,
    base_url: Option<&Url>,
    limits: DocumentLimits,
) -> AuthorStyleDiscovery {
    let mut discovery = StyleDiscoveryState::default();
    let mut stack = vec![dom.document()];
    let mut visited_nodes = 0_usize;

    while let Some(node_id) = stack.pop() {
        if visited_nodes >= limits.max_style_discovery_nodes {
            discovery.diagnostics.push(DocumentDiagnostic {
                node: None,
                code: DocumentDiagnosticCode::StyleDiscoveryNodeLimit,
                message: "style-source discovery stopped at its DOM node limit".to_owned(),
            });
            break;
        }
        visited_nodes += 1;
        let Some(node) = dom.node(node_id) else {
            continue;
        };
        if let NodeKind::Element(element) = node.kind() {
            discovery.inspect_element(node_id, element, base_url, limits);
        }
        if !matches!(node.kind(), NodeKind::Element(element) if element.local_name == "template") {
            stack.extend(node.children().iter().rev().copied());
        }
    }
    discovery.finish(dom.revision())
}

#[derive(Default)]
struct StyleDiscoveryState {
    slots: Vec<AuthorStyleSlot>,
    diagnostics: Vec<DocumentDiagnostic>,
    source_order: usize,
    external_count: usize,
    external_url_bytes: usize,
    slot_limit_reported: bool,
    external_limit_reported: bool,
    external_bytes_limit_reported: bool,
}

impl StyleDiscoveryState {
    fn inspect_element(
        &mut self,
        node: NodeId,
        element: &ElementData,
        base_url: Option<&Url>,
        limits: DocumentLimits,
    ) {
        if element.local_name == "style" {
            let eligibility = style_eligibility(node, element, &mut self.diagnostics);
            self.push_slot(
                AuthorStyleSlot {
                    owner: node,
                    source_order: self.source_order,
                    source: AuthorStyleSource::Embedded,
                    eligibility,
                },
                limits,
            );
            self.source_order = self.source_order.saturating_add(1);
        } else if element.local_name == "link"
            && is_stylesheet_link(element)
            && let Some(href) = attribute(element, "href")
        {
            self.discover_external(node, element, href, base_url, limits);
        }
    }

    fn discover_external(
        &mut self,
        node: NodeId,
        element: &ElementData,
        href: &str,
        base_url: Option<&Url>,
        limits: DocumentLimits,
    ) {
        let source_order = self.source_order;
        self.source_order = self.source_order.saturating_add(1);
        let eligibility = style_eligibility(node, element, &mut self.diagnostics);
        if self.external_count >= limits.max_external_style_sheets {
            self.report_external_count_limit(node);
            return;
        }
        let Some(next_url_bytes) = self.external_url_bytes.checked_add(href.len()) else {
            self.report_external_bytes_limit(node);
            return;
        };
        if next_url_bytes > limits.max_external_style_url_bytes {
            self.report_external_bytes_limit(node);
            return;
        }

        self.external_count += 1;
        self.external_url_bytes = next_url_bytes;
        let resolved_url = resolve_style_url(base_url, href);
        if base_url.is_some() && resolved_url.is_none() && eligibility.is_eligible() {
            self.diagnostics.push(DocumentDiagnostic {
                node: Some(node),
                code: DocumentDiagnosticCode::ExternalStyleSheetUnresolved,
                message: format!("could not resolve external stylesheet URL {href:?}"),
            });
        }
        self.push_slot(
            AuthorStyleSlot {
                owner: node,
                source_order,
                source: AuthorStyleSource::External {
                    href: href.to_owned(),
                    resolved_url,
                },
                eligibility,
            },
            limits,
        );
    }

    fn push_slot(&mut self, slot: AuthorStyleSlot, limits: DocumentLimits) {
        if self.slots.len() < limits.max_author_style_slots {
            self.slots.push(slot);
        } else if !self.slot_limit_reported {
            self.diagnostics.push(DocumentDiagnostic {
                node: Some(slot.owner),
                code: DocumentDiagnosticCode::AuthorStyleSlotLimit,
                message: "author stylesheet slot limit exceeded".to_owned(),
            });
            self.slot_limit_reported = true;
        }
    }

    fn report_external_count_limit(&mut self, node: NodeId) {
        if !self.external_limit_reported {
            self.diagnostics.push(DocumentDiagnostic {
                node: Some(node),
                code: DocumentDiagnosticCode::ExternalStyleSheetLimit,
                message: "external stylesheet discovery count limit exceeded".to_owned(),
            });
            self.external_limit_reported = true;
        }
    }

    fn report_external_bytes_limit(&mut self, node: NodeId) {
        if !self.external_bytes_limit_reported {
            self.diagnostics.push(DocumentDiagnostic {
                node: Some(node),
                code: DocumentDiagnosticCode::ExternalStyleSheetUrlBytesLimit,
                message: "external stylesheet URL byte limit exceeded".to_owned(),
            });
            self.external_bytes_limit_reported = true;
        }
    }

    fn finish(self, revision: DomRevision) -> AuthorStyleDiscovery {
        AuthorStyleDiscovery {
            revision,
            slots: self.slots,
            diagnostics: self.diagnostics,
        }
    }
}

fn is_stylesheet_link(element: &ElementData) -> bool {
    attribute(element, "rel").is_some_and(|rel| {
        rel.split_ascii_whitespace()
            .any(|token| token.eq_ignore_ascii_case("stylesheet"))
    })
}

fn style_eligibility(
    node: NodeId,
    element: &ElementData,
    diagnostics: &mut Vec<DocumentDiagnostic>,
) -> AuthorStyleEligibility {
    let type_is_css = !attribute(element, "type").is_some_and(|kind| {
        !kind.trim().is_empty() && !kind.trim().eq_ignore_ascii_case("text/css")
    });
    if !type_is_css {
        diagnostics.push(DocumentDiagnostic {
            node: Some(node),
            code: DocumentDiagnosticCode::NonCssStyleType,
            message: "an author stylesheet slot with a non-CSS type was not applied".to_owned(),
        });
    }
    let media = evaluate_screen_media(attribute(element, "media"));
    if media.has_unsupported_query {
        diagnostics.push(DocumentDiagnostic {
            node: Some(node),
            code: DocumentDiagnosticCode::MediaQueryUnsupported,
            message:
                "the media list contains query syntax beyond simple screen/all/print media types"
                    .to_owned(),
        });
    }
    AuthorStyleEligibility {
        type_is_css,
        media_matches: media.matches,
        media_fully_supported: !media.has_unsupported_query,
    }
}

fn resolve_style_url(base_url: Option<&Url>, href: &str) -> Option<Url> {
    let href = href.trim();
    match base_url {
        Some(base_url) => base_url.join(href).ok(),
        None => Url::parse(href).ok(),
    }
}

fn collect_style_element(
    dom: &Dom,
    node_id: NodeId,
    limits: DocumentLimits,
    embedded_sheet_count: &mut usize,
    style_bytes: &mut usize,
    sheets: &mut Vec<(NodeId, StyleSheet)>,
    diagnostics: &mut Vec<DocumentDiagnostic>,
) {
    let Some(node) = dom.node(node_id) else {
        return;
    };
    if *embedded_sheet_count >= limits.max_embedded_style_sheets {
        diagnostics.push(DocumentDiagnostic {
            node: Some(node_id),
            code: DocumentDiagnosticCode::EmbeddedStyleLimit,
            message: "embedded stylesheet count limit exceeded".to_owned(),
        });
        return;
    }

    let remaining_bytes = limits.max_embedded_style_bytes.saturating_sub(*style_bytes);
    let Some(css) = descendant_text_with_limit(dom, node, remaining_bytes) else {
        diagnostics.push(style_bytes_diagnostic(node_id));
        return;
    };
    let Some(next_style_bytes) = style_bytes.checked_add(css.len()) else {
        diagnostics.push(style_bytes_diagnostic(node_id));
        return;
    };
    *style_bytes = next_style_bytes;
    *embedded_sheet_count += 1;
    sheets.push((node_id, parse_stylesheet(&css)));
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct MediaEvaluation {
    matches: bool,
    has_unsupported_query: bool,
}

fn evaluate_screen_media(media: Option<&str>) -> MediaEvaluation {
    let Some(media) = media.map(str::trim).filter(|media| !media.is_empty()) else {
        return MediaEvaluation {
            matches: true,
            has_unsupported_query: false,
        };
    };
    let mut evaluation = MediaEvaluation::default();
    for query in media.split(',').map(str::trim) {
        let mut words = query.split_ascii_whitespace();
        let Some(first) = words.next() else {
            evaluation.has_unsupported_query = true;
            continue;
        };
        let (negated, media_type) = if first.eq_ignore_ascii_case("not") {
            (true, words.next())
        } else if first.eq_ignore_ascii_case("only") {
            (false, words.next())
        } else {
            (false, Some(first))
        };
        let Some(media_type) = media_type.filter(|_| words.next().is_none()) else {
            evaluation.has_unsupported_query = true;
            continue;
        };
        let type_matches = if media_type.eq_ignore_ascii_case("all")
            || media_type.eq_ignore_ascii_case("screen")
        {
            true
        } else if is_media_type_identifier(media_type) {
            false
        } else {
            evaluation.has_unsupported_query = true;
            continue;
        };
        evaluation.matches |= if negated { !type_matches } else { type_matches };
    }
    evaluation
}

fn is_media_type_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    if first == '-' {
        let Some(second) = characters.next() else {
            return false;
        };
        if second != '-' && !is_identifier_start(second) {
            return false;
        }
    } else if !is_identifier_start(first) {
        return false;
    }
    characters.all(|character| {
        is_identifier_start(character) || character.is_ascii_digit() || character == '-'
    })
}

const fn is_identifier_start(character: char) -> bool {
    character == '_' || character.is_ascii_alphabetic() || !character.is_ascii()
}

fn descendant_text_with_limit(dom: &Dom, root: &Node, max_bytes: usize) -> Option<String> {
    let mut text = String::with_capacity(max_bytes.min(4_096));
    let mut stack = root.children().iter().rev().copied().collect::<Vec<_>>();
    while let Some(node) = stack.pop() {
        let Some(node) = dom.node(node) else {
            continue;
        };
        if let NodeKind::Text(data) = node.kind() {
            if text
                .len()
                .checked_add(data.len())
                .is_none_or(|length| length > max_bytes)
            {
                return None;
            }
            text.push_str(data);
        }
        stack.extend(node.children().iter().rev().copied());
    }
    Some(text)
}

fn attribute<'a>(element: &'a ElementData, name: &str) -> Option<&'a str> {
    element
        .attributes
        .iter()
        .find(|attribute| attribute.namespace.is_none() && attribute.local_name == name)
        .map(|attribute| attribute.value.as_str())
}

fn style_bytes_diagnostic(node: NodeId) -> DocumentDiagnostic {
    DocumentDiagnostic {
        node: Some(node),
        code: DocumentDiagnosticCode::EmbeddedStyleBytesLimit,
        message: "embedded stylesheet byte limit exceeded".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AuthorStyleSlot, AuthorStyleSource, Document, DocumentDiagnosticCode, DocumentLimits,
        DocumentRenderOptions, ExternalStyleSheetKey, ExternalStyleSheets,
    };
    use crate::css::selector::{MatchContext, parse_selector_list, select_all};
    use crate::dom::{Dom, NodeId, NodeKind};
    use crate::image::{DecodedImage, ImageLimits, ImageResources, discover_images};
    use crate::layout::{FragmentKind, PhysicalPoint, PhysicalSize};
    use crate::paint::{Color, DisplayCommand};
    use url::Url;

    fn target_id(dom: &Dom, selector: &str) -> NodeId {
        let selectors = parse_selector_list(selector).expect("test selector must parse");
        select_all(dom, dom.document(), &selectors, &MatchContext::default())[0]
    }

    fn typed_css(
        document: &Document,
        render: &super::DocumentRenderOutput,
        selector: &str,
        property: &str,
    ) -> String {
        let node = target_id(document.dom(), selector);
        render.styles[&node]
            .typed(property)
            .unwrap_or_else(|| panic!("{property} must have a typed computed value"))
            .to_css()
    }

    fn external_key(slot: &AuthorStyleSlot) -> ExternalStyleSheetKey {
        let AuthorStyleSource::External {
            resolved_url: Some(url),
            ..
        } = &slot.source
        else {
            panic!("expected a resolved external stylesheet slot");
        };
        ExternalStyleSheetKey::new(slot.owner, url.clone())
    }

    #[test]
    fn embedded_author_css_flows_through_layout_and_paint() {
        let document = Document::parse(
            "<!doctype html><html><head><style>\
             #card { display:block; width:120px; height:40px; background-color:#2468ac }\
             </style></head><body><div id=card>hello</div></body></html>",
        );
        let render = document.render_reference(DocumentRenderOptions::default());

        assert_eq!(render.revision, document.dom().revision());
        assert_eq!(render.display.list.dom_revision, render.revision);
        assert_eq!(render.raster.surface.width(), 1_280);
        assert!(render.display.list.items().iter().any(|item| {
            matches!(
                item.command,
                DisplayCommand::SolidRect { color, .. }
                    if color.red == 0x24 && color.green == 0x68 && color.blue == 0xac
            )
        }));
        assert!(render.diagnostics.document.is_empty());
    }

    #[test]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "test image coordinates are small, finite, non-negative CSS pixel positions"
    )]
    fn decoded_image_preserves_ratio_and_paints_pixels() {
        let document = Document::parse(
            "<!doctype html><style>body{margin:0}img{width:8px}</style><img src=hero.png>",
        );
        let url = Url::parse("https://example.test/").unwrap();
        let key = discover_images(document.dom(), &url, ImageLimits::default()).resources[0]
            .key
            .clone();
        let image =
            DecodedImage::from_pixels(2, 1, vec![Color::rgb(255, 0, 0), Color::rgb(0, 0, 255)])
                .unwrap();
        let mut images = ImageResources::default();
        images.insert(key, image, ImageLimits::default()).unwrap();
        let render = document.render_with_images(
            DocumentRenderOptions::default(),
            super::DocumentBackends {
                text_measurer: &crate::layout::SimpleTextMeasurer,
                text_shaper: &crate::paint::ReferenceTextShaper,
                glyph_masks: &crate::paint::NoGlyphMasks,
            },
            &images,
        );
        let node = target_id(document.dom(), "img");
        let size = render
            .layout
            .fragments
            .iter()
            .find_map(
                |fragment| match (&fragment.kind, fragment.source == Some(node)) {
                    (FragmentKind::Box(geometry), true) => Some(geometry.content_rect.size),
                    _ => None,
                },
            )
            .unwrap();
        assert_eq!(
            size,
            PhysicalSize {
                width: 8.0,
                height: 4.0
            }
        );
        let destination = render
            .display
            .list
            .items()
            .iter()
            .find_map(|item| match item.command {
                DisplayCommand::Image(image) => Some(image.destination),
                _ => None,
            })
            .unwrap();
        let sample_y = destination.origin.y as u32 + 1;
        assert_eq!(
            render
                .raster
                .surface
                .pixel(destination.origin.x as u32 + 1, sample_y),
            Some(Color::rgb(255, 0, 0))
        );
        assert_eq!(
            render
                .raster
                .surface
                .pixel(destination.origin.x as u32 + 6, sample_y),
            Some(Color::rgb(0, 0, 255))
        );
    }

    #[test]
    fn unloaded_image_uses_html_or_default_dimensions() {
        let document = Document::parse(
            "<!doctype html><style>body{margin:0}</style><img id=a src=a width=40 height=20><img id=b src=b>",
        );
        let render = document.render_reference(DocumentRenderOptions::default());
        let size = |selector| {
            let node = target_id(document.dom(), selector);
            render
                .layout
                .fragments
                .iter()
                .find_map(
                    |fragment| match (&fragment.kind, fragment.source == Some(node)) {
                        (FragmentKind::Box(geometry), true) => Some(geometry.content_rect.size),
                        _ => None,
                    },
                )
                .unwrap()
        };
        assert_eq!(
            size("#a"),
            PhysicalSize {
                width: 40.0,
                height: 20.0
            }
        );
        assert_eq!(
            size("#b"),
            PhysicalSize {
                width: 300.0,
                height: 150.0
            }
        );
    }

    #[test]
    fn unsupported_css_sources_are_never_silently_ignored() {
        let document = Document::parse(
            "<!doctype html><link rel=stylesheet href=theme.css>\
             <link rel=stylesheet>\
             <style media='screen and (min-width: 1px)'>body { color:red }</style>\
             <body style='color:blue'></body>",
        );
        let render = document.render_reference(DocumentRenderOptions::default());
        let codes = render
            .diagnostics
            .document
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect::<Vec<_>>();

        assert!(codes.contains(&DocumentDiagnosticCode::ExternalStyleSheetUnsupported));
        assert!(codes.contains(&DocumentDiagnosticCode::MediaQueryUnsupported));
        assert_eq!(
            typed_css(&document, &render, "body", "color"),
            "rgb(0, 0, 255)"
        );
        assert_eq!(
            codes
                .iter()
                .filter(|code| **code == DocumentDiagnosticCode::ExternalStyleSheetUnsupported)
                .count(),
            1
        );
    }

    #[test]
    fn ua_display_defaults_cover_html_structures_without_hiding_inline_content() {
        let document = Document::parse(
            "<!doctype html><html><head><title>x</title></head><body>\
             <main id=main><span id=inline></span></main>\
             <details><summary id=summary>summary</summary></details>\
             <table><colgroup><col id=column></colgroup></table>\
             <input id=control><input id=hidden-control type=hidden>\
             <div id=hidden hidden>hidden</div></body></html>",
        );
        let render = document.render_reference(DocumentRenderOptions::default());

        assert_eq!(typed_css(&document, &render, "#main", "display"), "block");
        assert_eq!(
            typed_css(&document, &render, "#inline", "display"),
            "inline"
        );
        assert_eq!(
            typed_css(&document, &render, "#summary", "display"),
            "block flow list-item"
        );
        assert_eq!(
            typed_css(&document, &render, "#column", "display"),
            "table-column"
        );
        assert_eq!(
            typed_css(&document, &render, "#control", "display"),
            "inline-block"
        );
        assert_eq!(
            typed_css(&document, &render, "#hidden-control", "display"),
            "none"
        );
        assert_eq!(typed_css(&document, &render, "#hidden", "display"), "none");
    }

    #[test]
    fn inline_style_hides_template_text_and_wins_author_specificity() {
        let document = Document::parse(
            "<!doctype html><style>#template { display:block !important; color:red !important }</style>\
             <textarea id=template style='display:none !important; color:blue !important'><div>raw template</div></textarea>",
        );
        let render = document.render_reference(DocumentRenderOptions::default());

        assert_eq!(
            typed_css(&document, &render, "#template", "display"),
            "none"
        );
        assert_eq!(
            typed_css(&document, &render, "#template", "color"),
            "rgb(0, 0, 255)"
        );
        let template = target_id(document.dom(), "#template");
        assert!(
            render
                .layout
                .fragments
                .iter()
                .all(|fragment| fragment.source != Some(template))
        );
    }

    #[test]
    fn simple_screen_media_is_evaluated_and_unsupported_queries_are_diagnosed() {
        let document = Document::parse(
            "<!doctype html><style media='only screen'>#screen { color: red }</style>\
             <style media=print>#print { color: red }</style>\
             <style media='not print'>#not-print { color: red }</style>\
             <style media=speech>#speech { color: red }</style>\
             <style media='screen and (min-width: 1px)'>#query { color: red }</style>\
             <style type=text/plain>#wrong-type { color: red }</style>\
             <p id=screen></p><p id=print></p><p id=not-print></p><p id=speech></p>\
             <p id=query></p><p id=wrong-type></p>",
        );
        let render = document.render_reference(DocumentRenderOptions::default());

        assert_eq!(
            typed_css(&document, &render, "#screen", "color"),
            "rgb(255, 0, 0)"
        );
        assert_eq!(
            typed_css(&document, &render, "#print", "color"),
            "canvastext"
        );
        assert_eq!(
            typed_css(&document, &render, "#not-print", "color"),
            "rgb(255, 0, 0)"
        );
        assert_eq!(
            typed_css(&document, &render, "#speech", "color"),
            "canvastext"
        );
        assert_eq!(
            typed_css(&document, &render, "#query", "color"),
            "canvastext"
        );
        assert_eq!(
            typed_css(&document, &render, "#wrong-type", "color"),
            "canvastext"
        );
        assert!(render.diagnostics.document.iter().any(|diagnostic| {
            diagnostic.code == DocumentDiagnosticCode::MediaQueryUnsupported
        }));
        assert!(
            render
                .diagnostics
                .document
                .iter()
                .any(|diagnostic| { diagnostic.code == DocumentDiagnosticCode::NonCssStyleType })
        );
    }

    #[test]
    fn style_text_is_collected_again_after_dynamic_dom_mutation() {
        let mut document = Document::parse(
            "<!doctype html><style id=theme>#card { background-color: red }</style>\
             <div id=card>card</div>",
        );
        let first = document.render_reference(DocumentRenderOptions::default());
        assert_eq!(
            typed_css(&document, &first, "#card", "background-color"),
            "rgb(255, 0, 0)"
        );

        let style = target_id(document.dom(), "#theme");
        let text = document.dom().children(style).expect("style children")[0];
        document
            .dom_mut()
            .set_character_data(text, "#card { background-color: blue }")
            .expect("style text mutation succeeds");
        let second = document.render_reference(DocumentRenderOptions::default());

        assert!(second.revision > first.revision);
        assert_eq!(
            typed_css(&document, &second, "#card", "background-color"),
            "rgb(0, 0, 255)"
        );
    }

    #[test]
    fn external_and_embedded_sheets_cascade_in_dom_slot_order() {
        let document = Document::parse(
            "<!doctype html><html><head>\
             <style>#target { color: #ff0000 }</style>\
             <link rel=stylesheet href=css/a.css>\
             <style>#target { color: #00ff00 }</style>\
             <link rel=stylesheet href=css/b.css>\
             </head><body><p id=target>target</p></body></html>",
        );
        let base = Url::parse("https://example.test/pages/index.html").expect("base URL");
        let discovery = document.discover_author_style_slots(&base, DocumentLimits::default());

        assert_eq!(discovery.revision, document.dom().revision());
        assert_eq!(discovery.slots.len(), 4);
        assert_eq!(
            discovery
                .slots
                .iter()
                .map(|slot| slot.source_order)
                .collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
        let AuthorStyleSource::External {
            resolved_url: Some(first_url),
            ..
        } = &discovery.slots[1].source
        else {
            panic!("second slot must be external");
        };
        assert_eq!(first_url.as_str(), "https://example.test/pages/css/a.css");

        let mut external = ExternalStyleSheets::default();
        external.insert_css(
            external_key(&discovery.slots[1]),
            "#target { color: #0000ff }",
        );
        let without_last = document.render_reference_with_external_style_sheets(
            DocumentRenderOptions::default(),
            &base,
            &external,
        );
        assert_eq!(
            typed_css(&document, &without_last, "#target", "color"),
            "rgb(0, 255, 0)"
        );
        assert!(without_last.diagnostics.document.iter().any(|diagnostic| {
            diagnostic.node == Some(discovery.slots[3].owner)
                && diagnostic.code == DocumentDiagnosticCode::ExternalStyleSheetUnsupported
        }));

        external.insert_css(
            external_key(&discovery.slots[3]),
            "#target { color: #000000 }",
        );
        let complete = document.render_reference_with_external_style_sheets(
            DocumentRenderOptions::default(),
            &base,
            &external,
        );
        assert_eq!(
            typed_css(&document, &complete, "#target", "color"),
            "rgb(0, 0, 0)"
        );
        assert!(!complete.diagnostics.document.iter().any(|diagnostic| {
            diagnostic.code == DocumentDiagnosticCode::ExternalStyleSheetUnsupported
                || diagnostic.code == DocumentDiagnosticCode::ExternalStyleSheetUnresolved
        }));
    }

    #[test]
    fn external_discovery_models_media_type_and_resolution_failures() {
        let document = Document::parse(
            "<!doctype html><head>\
             <link rel=stylesheet href=print.css media=print>\
             <link rel=stylesheet href=plain.css type=text/plain>\
             <link rel=stylesheet href=query.css media='screen and (min-width: 1px)'>\
             <link rel=stylesheet href=screen.css media='only screen'>\
             <link rel=stylesheet href='http://['>\
             </head><body><p id=target></p></body>",
        );
        let base = Url::parse("https://example.test/base/page.html").expect("base URL");
        let discovery = document.discover_author_style_slots(&base, DocumentLimits::default());

        assert_eq!(discovery.slots.len(), 5);
        assert!(!discovery.slots[0].eligibility.media_matches);
        assert!(!discovery.slots[1].eligibility.type_is_css);
        assert!(!discovery.slots[2].eligibility.media_fully_supported);
        assert!(discovery.slots[3].eligibility.is_eligible());
        assert!(matches!(
            discovery.slots[4].source,
            AuthorStyleSource::External {
                resolved_url: None,
                ..
            }
        ));
        assert!(discovery.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == DocumentDiagnosticCode::ExternalStyleSheetUnresolved
        }));

        let mut external = ExternalStyleSheets::default();
        external.insert_css(
            external_key(&discovery.slots[3]),
            "#target { color: #ff0000 }",
        );
        let render = document.render_reference_with_external_style_sheets(
            DocumentRenderOptions::default(),
            &base,
            &external,
        );
        assert_eq!(
            typed_css(&document, &render, "#target", "color"),
            "rgb(255, 0, 0)"
        );
        assert!(!render.diagnostics.document.iter().any(|diagnostic| {
            diagnostic.code == DocumentDiagnosticCode::ExternalStyleSheetUnsupported
        }));
        assert!(render.diagnostics.document.iter().any(|diagnostic| {
            diagnostic.code == DocumentDiagnosticCode::ExternalStyleSheetUnresolved
        }));
    }

    #[test]
    fn external_style_discovery_limits_fail_closed() {
        let document = Document::parse(
            "<!doctype html><link rel=stylesheet href=a.css>\
             <link rel=stylesheet href=b.css><style>p { color:red }</style>",
        );
        let base = Url::parse("https://example.test/index.html").expect("base URL");
        let discovery = document.discover_author_style_slots(
            &base,
            DocumentLimits {
                max_external_style_sheets: 1,
                max_author_style_slots: 1,
                ..DocumentLimits::default()
            },
        );

        assert_eq!(discovery.slots.len(), 1);
        assert!(discovery.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == DocumentDiagnosticCode::ExternalStyleSheetLimit
        }));
        assert!(
            discovery.diagnostics.iter().any(|diagnostic| {
                diagnostic.code == DocumentDiagnosticCode::AuthorStyleSlotLimit
            })
        );
    }

    #[test]
    fn dynamically_inserted_and_retargeted_link_is_rediscovered() {
        let mut document = Document::parse(
            "<!doctype html><html><head id=head></head>\
             <body><p id=target>target</p></body></html>",
        );
        let base = Url::parse("https://example.test/index.html").expect("base URL");
        let initial_revision = document.dom().revision();
        assert!(
            document
                .discover_author_style_slots(&base, DocumentLimits::default())
                .slots
                .is_empty()
        );

        let head = target_id(document.dom(), "#head");
        let link = document.dom_mut().create_element("link");
        document
            .dom_mut()
            .set_attribute(link, "rel", "stylesheet")
            .expect("set rel");
        document
            .dom_mut()
            .set_attribute(link, "href", "old.css")
            .expect("set href");
        document
            .dom_mut()
            .append_child(head, link)
            .expect("insert stylesheet link");
        let old_discovery = document.discover_author_style_slots(&base, DocumentLimits::default());
        assert!(old_discovery.revision > initial_revision);
        assert_eq!(old_discovery.slots.len(), 1);

        let mut external = ExternalStyleSheets::default();
        external.insert_css(
            external_key(&old_discovery.slots[0]),
            "#target { color: #ff0000 }",
        );
        let old_render = document.render_reference_with_external_style_sheets(
            DocumentRenderOptions::default(),
            &base,
            &external,
        );
        assert_eq!(
            typed_css(&document, &old_render, "#target", "color"),
            "rgb(255, 0, 0)"
        );

        document
            .dom_mut()
            .set_attribute(link, "href", "new.css")
            .expect("retarget href");
        let new_discovery = document.discover_author_style_slots(&base, DocumentLimits::default());
        assert!(new_discovery.revision > old_discovery.revision);
        let stale_render = document.render_reference_with_external_style_sheets(
            DocumentRenderOptions::default(),
            &base,
            &external,
        );
        assert_eq!(
            typed_css(&document, &stale_render, "#target", "color"),
            "canvastext"
        );
        assert!(stale_render.diagnostics.document.iter().any(|diagnostic| {
            diagnostic.node == Some(link)
                && diagnostic.code == DocumentDiagnosticCode::ExternalStyleSheetUnsupported
        }));

        external.insert_css(
            external_key(&new_discovery.slots[0]),
            "#target { color: #0000ff }",
        );
        let new_render = document.render_reference_with_external_style_sheets(
            DocumentRenderOptions::default(),
            &base,
            &external,
        );
        assert_eq!(
            typed_css(&document, &new_render, "#target", "color"),
            "rgb(0, 0, 255)"
        );
        assert!(!new_render.diagnostics.document.iter().any(|diagnostic| {
            diagnostic.code == DocumentDiagnosticCode::ExternalStyleSheetUnsupported
        }));
    }

    #[test]
    fn styles_in_template_contents_are_inert() {
        let mut document = Document::parse(
            "<!doctype html><template id=holder></template><p id=target>target</p>",
        );
        let template = target_id(document.dom(), "#holder");
        let style = document.dom_mut().create_element("style");
        let css = document.dom_mut().create_text("#target { color: red }");
        document
            .dom_mut()
            .append_child(style, css)
            .expect("style accepts text");
        document
            .dom_mut()
            .append_child(template, style)
            .expect("test DOM represents inert template contents");

        let render = document.render_reference(DocumentRenderOptions::default());
        assert_eq!(
            typed_css(&document, &render, "#target", "color"),
            "canvastext"
        );
    }

    #[test]
    fn style_resource_limits_fail_closed_without_preallocating_oversized_css() {
        let document = Document::parse(
            "<!doctype html><style>xxxxxxxxxxxxxxxxxxxx</style>\
             <style>p{color:red}</style><p id=target>target</p>",
        );
        let options = DocumentRenderOptions {
            document_limits: DocumentLimits {
                max_embedded_style_bytes: 12,
                ..DocumentLimits::default()
            },
            ..DocumentRenderOptions::default()
        };
        let render = document.render_reference(options);

        assert_eq!(
            typed_css(&document, &render, "#target", "color"),
            "rgb(255, 0, 0)"
        );
        assert_eq!(
            render
                .diagnostics
                .document
                .iter()
                .filter(|diagnostic| {
                    diagnostic.code == DocumentDiagnosticCode::EmbeddedStyleBytesLimit
                })
                .count(),
            1
        );
    }

    #[test]
    fn non_applicable_styles_do_not_consume_sheet_limit() {
        let document = Document::parse(
            "<!doctype html><style type=text/plain>p { color: blue }</style>\
             <style>p { color: red }</style><style>p { color: green }</style><p id=target></p>",
        );
        let options = DocumentRenderOptions {
            document_limits: DocumentLimits {
                max_embedded_style_sheets: 1,
                ..DocumentLimits::default()
            },
            ..DocumentRenderOptions::default()
        };
        let render = document.render_reference(options);

        assert_eq!(
            typed_css(&document, &render, "#target", "color"),
            "rgb(255, 0, 0)"
        );
        assert!(
            render
                .diagnostics
                .document
                .iter()
                .any(|diagnostic| { diagnostic.code == DocumentDiagnosticCode::NonCssStyleType })
        );
        assert!(
            render.diagnostics.document.iter().any(|diagnostic| {
                diagnostic.code == DocumentDiagnosticCode::EmbeddedStyleLimit
            })
        );
    }

    #[test]
    fn style_discovery_and_quirks_mode_have_explicit_diagnostics() {
        let document = Document::parse("<style>p { color: red }</style><p id=target></p>");
        let options = DocumentRenderOptions {
            document_limits: DocumentLimits {
                max_style_discovery_nodes: 1,
                ..DocumentLimits::default()
            },
            ..DocumentRenderOptions::default()
        };
        let render = document.render_reference(options);

        assert!(render.diagnostics.document.iter().any(|diagnostic| {
            diagnostic.code == DocumentDiagnosticCode::StyleDiscoveryNodeLimit
        }));
        assert!(render.diagnostics.document.iter().any(|diagnostic| {
            diagnostic.code == DocumentDiagnosticCode::QuirksModeUnsupported
        }));
        assert_eq!(
            typed_css(&document, &render, "#target", "color"),
            "canvastext"
        );
    }

    #[test]
    fn rerender_consumes_a_new_dom_revision_without_html_reparse() {
        let mut document = Document::parse(
            "<!doctype html><style>p { display:block }</style><p id=message>before</p>",
        );
        let first = document.render_reference(DocumentRenderOptions::default());
        let selector = parse_selector_list("#message").expect("selector must parse");
        let paragraph = select_all(
            document.dom(),
            document.dom().document(),
            &selector,
            &MatchContext::default(),
        )[0];
        let text = document
            .dom()
            .children(paragraph)
            .expect("paragraph children")
            .iter()
            .copied()
            .find(|node| {
                matches!(
                    document.dom().node(*node).map(crate::dom::Node::kind),
                    Some(NodeKind::Text(_))
                )
            })
            .expect("paragraph text");
        document
            .dom_mut()
            .set_character_data(text, "after mutation")
            .expect("text mutation must succeed");

        let second = document.render_reference(DocumentRenderOptions::default());
        let diff = second.display.list.diff(&first.display.list);
        assert!(second.revision > first.revision);
        assert_eq!(second.revision, document.dom().revision());
        assert!(!diff.full_repaint);
        assert!(!diff.changed.is_empty() || !diff.inserted.is_empty());
        assert!(!diff.dirty_rects.is_empty());
    }

    #[test]
    fn scroll_extent_and_offset_do_not_change_the_layout_viewport() {
        let document = Document::parse(
            "<!doctype html><style>\
             html, body, div { display:block; margin-top:0; margin-right:0; margin-bottom:0; margin-left:0 }\
             .first { height:10px; background-color:#ff0000 }\
             .second { height:10px; background-color:#0000ff }\
             .third { height:10px; background-color:#00ff00 }\
             </style><body><div class=first></div><div class=second></div><div class=third></div></body>",
        );
        let base = DocumentRenderOptions {
            layout: crate::layout::LayoutOptions {
                viewport: PhysicalSize {
                    width: 8.0,
                    height: 10.0,
                },
                ..crate::layout::LayoutOptions::default()
            },
            raster_background: Color::WHITE,
            ..DocumentRenderOptions::default()
        };
        let top = document.render_reference(base);
        let scrolled = document.render_reference(DocumentRenderOptions {
            scroll_offset: PhysicalPoint { x: 0.0, y: 10.0 },
            ..base
        });

        assert_eq!(top.layout, scrolled.layout);
        assert!((top.layout.fragments.viewport.height - 10.0).abs() < f32::EPSILON);
        assert!(top.layout.fragments.scrollable_content_size.height >= 30.0);
        assert!((scrolled.paint_viewport_origin.y - 10.0).abs() < f32::EPSILON);
        assert_eq!(top.raster.surface.pixel(0, 0), Some(Color::rgb(255, 0, 0)));
        assert_eq!(
            scrolled.raster.surface.pixel(0, 0),
            Some(Color::rgb(0, 0, 255))
        );
        assert_eq!(scrolled.raster.surface.height(), 10);
    }
}
