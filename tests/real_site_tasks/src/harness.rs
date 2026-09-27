//! Deterministic resource substitution and one offline render of a fixture.
//!
//! The engine's normal load path is the only path this crate uses: parse HTML,
//! discover author stylesheet slots, discover images, discover scripts, supply
//! deterministic responses for every discovered request, then render. There is
//! no second renderer, no site-specific adapter, and no network call anywhere
//! below.
//!
//! Determinism comes from the same reference backends the engine's own
//! conformance tests use ([`SimpleTextMeasurer`], [`ReferenceTextShaper`],
//! [`NoGlyphMasks`]) rather than from system fonts, so the same fixture renders
//! the same pixels on any machine.

use url::Url;

use render_core::document::{
    AuthorStyleSlot, AuthorStyleSource, Document, DocumentBackends, DocumentDiagnostic,
    DocumentLimits, DocumentRenderOptions, DocumentRenderOutput, ExternalStyleSheetKey,
    ExternalStyleSheets,
};
use render_core::dom::{Dom, Node, NodeId, NodeKind};
use render_core::html::{DecodedHtml, HtmlDecodeOptions, decode_html_bytes};
use render_core::image::{
    DecodedImage, DiscoveredImage, ImageDiscovery, ImageDiscoveryDiagnosticCode, ImageLimits,
    ImageResourceKey, ImageResources, ImageSelectionContext, ImageSource,
    discover_images_with_context,
};
use render_core::layout::{FragmentTree, LayoutOptions, PhysicalSize, SimpleTextMeasurer};
use render_core::paint::{Color, DisplayList, NoGlyphMasks, ReferenceTextShaper};
use render_core::script::{ScriptDiscovery, ScriptDiscoveryLimits, discover_scripts};

use crate::fixture::{
    CONTRACT_VIEWPORT_HEIGHT, CONTRACT_VIEWPORT_WIDTH, FixtureSource, RealSiteFixture,
};

/// The viewport the contract is measured at: 1280 x 600 CSS pixels.
#[must_use]
pub const fn contract_viewport() -> PhysicalSize {
    PhysicalSize {
        width: CONTRACT_VIEWPORT_WIDTH,
        height: CONTRACT_VIEWPORT_HEIGHT,
    }
}

/// The device pixel ratio the contract is measured at. Fixing it is what makes
/// `x`-descriptor `srcset` selection reproducible.
pub const CONTRACT_DEVICE_PIXEL_RATIO_MILLI: u32 = 1_000;

/// Reference backends: deterministic, font-independent, system-font-free.
#[must_use]
pub fn reference_backends() -> DocumentBackends<'static> {
    DocumentBackends {
        text_measurer: &SimpleTextMeasurer,
        text_shaper: &ReferenceTextShaper,
        glyph_masks: &NoGlyphMasks,
    }
}

/// Render options pinned to the contract viewport.
#[must_use]
pub fn contract_render_options() -> DocumentRenderOptions {
    DocumentRenderOptions {
        layout: LayoutOptions {
            viewport: contract_viewport(),
            ..LayoutOptions::default()
        },
        ..DocumentRenderOptions::default()
    }
}

/// The image-selection context that matches the contract viewport.
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the contract viewport is a whole number of CSS pixels between 1 and 4096"
)]
pub const fn contract_selection_context() -> ImageSelectionContext {
    ImageSelectionContext {
        viewport_width: CONTRACT_VIEWPORT_WIDTH as u32,
        viewport_height: CONTRACT_VIEWPORT_HEIGHT as u32,
        device_pixel_ratio_milli: CONTRACT_DEVICE_PIXEL_RATIO_MILLI,
    }
}

/// One fixture, decoded, discovered, answered locally, and rendered.
pub struct Session {
    pub fixture: &'static RealSiteFixture,
    /// The result of running the HTML encoding-sniffing algorithm on the raw
    /// fixture bytes.
    pub decoded: DecodedHtml,
    pub document: Document,
    pub base_url: Url,
    /// The local bytes the harness serves for every external stylesheet.
    css: String,
    /// Author stylesheet slots in DOM tree order, as the engine found them.
    pub style_slots: Vec<AuthorStyleSlot>,
    /// Diagnostics from stylesheet discovery and collection.
    pub style_diagnostics: Vec<DocumentDiagnostic>,
    /// Image discovery output, kept so the assertions can inspect the plan
    /// rather than only its effect.
    pub image_discovery: ImageDiscovery,
    /// The deterministic decoded images installed for every discovered request.
    pub images: ImageResources,
    pub script_discovery: ScriptDiscovery,
    pub output: DocumentRenderOutput,
}

impl Session {
    /// Run the whole offline load for one fixture.
    ///
    /// # Panics
    ///
    /// Panics when the engine cannot decode the fixture, or when a deterministic
    /// response cannot be installed. Both are harness authoring errors rather
    /// than page-shape outcomes, so they must not surface as a contract
    /// failure.
    #[must_use]
    pub fn load(fixture: &'static RealSiteFixture) -> Self {
        let source = FixtureSource::read(fixture);
        let decoded = decode_html_bytes(&source.html_bytes, &HtmlDecodeOptions::default())
            .unwrap_or_else(|error| {
                panic!("{}: fixture bytes do not decode: {error}", fixture.label)
            });
        let document = Document::parse(&decoded.text);
        let base_url = source.base_url.clone();

        // Author stylesheets: discover the slots, then answer every eligible
        // external slot with the fixture's local deterministic bytes. A
        // separate key per slot is what proves the engine keys external sheets
        // by resolving URL, and that two sheets on one origin both apply.
        let style_discovery =
            document.discover_author_style_slots(&base_url, DocumentLimits::default());
        let style_sheets = supply_style_sheets(&style_discovery.slots, &source.css);
        let style_diagnostics = style_discovery.diagnostics.clone();

        // Images: discover, then answer every discovered request with a
        // deterministic decoded bitmap whose intrinsic size comes from the
        // requesting element's own declared box.
        let image_discovery = discover_images_with_context(
            document.dom(),
            &base_url,
            ImageLimits::default(),
            contract_selection_context(),
        );
        let mut images = ImageResources::default();
        for discovered in &image_discovery.resources {
            let image = deterministic_image(document.dom(), &discovered.key);
            images
                .insert(discovered.key.clone(), image, ImageLimits::default())
                .unwrap_or_else(|error| {
                    panic!("{}: cannot install image resource: {error}", fixture.label)
                });
        }

        let script_discovery =
            discover_scripts(&document, &base_url, ScriptDiscoveryLimits::default());

        let output = document.render_with_external_style_sheets_and_images(
            contract_render_options(),
            reference_backends(),
            &base_url,
            &style_sheets,
            &images,
        );

        Self {
            fixture,
            decoded,
            document,
            base_url,
            css: source.css,
            style_slots: style_discovery.slots,
            style_diagnostics,
            image_discovery,
            images,
            script_discovery,
            output,
        }
    }

    /// The fragments the reference layout produced.
    #[must_use]
    pub const fn fragments(&self) -> &FragmentTree {
        &self.output.layout.fragments
    }

    /// The display list the reference paint stage built.
    #[must_use]
    pub const fn display_list(&self) -> &DisplayList {
        &self.output.display.list
    }

    /// The local CSS the harness serves for every external stylesheet.
    #[must_use]
    pub fn css(&self) -> &str {
        &self.css
    }

    /// Eligible external stylesheet slots, in DOM tree order.
    #[must_use]
    pub fn external_style_slots(&self) -> Vec<&AuthorStyleSlot> {
        self.style_slots
            .iter()
            .filter(|slot| {
                slot.eligibility.is_eligible()
                    && matches!(
                        slot.source,
                        AuthorStyleSource::External {
                            resolved_url: Some(_),
                            ..
                        }
                    )
            })
            .collect()
    }

    /// Image-discovery diagnostics the harness does not tolerate.
    ///
    /// `MissingSource` is the code reserved for an image whose bytes have not
    /// been requested yet, which is the state every `data-src` deferred element
    /// is in. It is permitted, and it is currently never constructed, so a clean
    /// run reports nothing at all. Every other code is a real failure of
    /// resource classification: an unresolvable URL, an unsupported scheme, a
    /// limit, or a `srcset` that resolved to nothing.
    #[must_use]
    pub fn image_discovery_failures(&self) -> Vec<(ImageDiscoveryDiagnosticCode, String)> {
        self.image_discovery
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code != ImageDiscoveryDiagnosticCode::MissingSource)
            .map(|diagnostic| (diagnostic.code, diagnostic.message.clone()))
            .collect()
    }

    /// Nodes the engine reported as having no fetchable image source.
    #[must_use]
    pub fn deferred_image_nodes(&self) -> Vec<NodeId> {
        self.image_discovery
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == ImageDiscoveryDiagnosticCode::MissingSource)
            .filter_map(|diagnostic| diagnostic.node)
            .collect()
    }

    /// Discovered images grouped by how their source was selected.
    #[must_use]
    pub fn discovered_by_source(&self) -> Vec<(ImageSource, &DiscoveredImage)> {
        self.image_discovery
            .resources
            .iter()
            .map(|discovered| (discovered.key.source, discovered))
            .collect()
    }
}

/// Answer every eligible external stylesheet slot with the same local bytes.
fn supply_style_sheets(slots: &[AuthorStyleSlot], css: &str) -> ExternalStyleSheets {
    let mut sheets = ExternalStyleSheets::default();
    for slot in slots {
        if !slot.eligibility.is_eligible() {
            continue;
        }
        let AuthorStyleSource::External {
            resolved_url: Some(requested_url),
            ..
        } = &slot.source
        else {
            continue;
        };
        let key = ExternalStyleSheetKey::new(slot.owner, requested_url.clone());
        sheets.insert_css(key, css);
    }
    sheets
}

/// The deterministic bitmap installed for one discovered image.
///
/// Its intrinsic size comes from the requesting element's own `width` and
/// `height` attributes when it declares both, so layout exercises the same
/// replaced-element path a real page does. Its colour comes from an FNV-1a
/// digest of the requested URL, so two different images on a page differ and
/// the same URL always produces the same pixels on every machine and every run.
/// Nothing here inspects which site a URL belongs to.
fn deterministic_image(dom: &Dom, key: &ImageResourceKey) -> DecodedImage {
    const FALLBACK: u32 = 8;
    const MAX_DECLARED: u32 = 640;
    const CELL: u32 = 8;

    let (width, height) = declared_pixel_box(dom, key.owner).unwrap_or((FALLBACK, FALLBACK));
    let width = width.clamp(1, MAX_DECLARED);
    let height = height.clamp(1, MAX_DECLARED);

    let digest = fnv1a32(key.requested_url.as_str().as_bytes());
    let base = Color::rgb(
        0x40_u8.wrapping_add((digest & 0x3f) as u8),
        0x40_u8.wrapping_add(((digest >> 8) & 0x3f) as u8),
        0x40_u8.wrapping_add(((digest >> 16) & 0x3f) as u8),
    );
    let accent = Color::rgb(
        base.red.wrapping_add(0x40),
        base.green.wrapping_add(0x30),
        base.blue.wrapping_add(0x20),
    );

    // A checkerboard keeps midtones in the raster, so a positional regression
    // shows up as more than a flat fill change.
    let mut pixels = Vec::with_capacity((width as usize) * (height as usize));
    for y in 0..height {
        for x in 0..width {
            let shaded = (x / CELL + y / CELL) % 2 == 0;
            pixels.push(if shaded { base } else { accent });
        }
    }
    DecodedImage::from_pixels(width, height, pixels)
        .unwrap_or_else(|error| panic!("the deterministic image does not decode: {error}"))
}

/// The element's own declared pixel box, when it declares both dimensions.
fn declared_pixel_box(dom: &Dom, node: NodeId) -> Option<(u32, u32)> {
    let Some(NodeKind::Element(element)) = dom.node(node).map(Node::kind) else {
        return None;
    };
    let read = |name: &str| {
        element
            .attributes
            .iter()
            .find(|attribute| attribute.namespace.is_none() && attribute.local_name == name)
            .and_then(|attribute| attribute.value.trim().parse::<u32>().ok())
    };
    Some((read("width")?, read("height")?))
}

fn fnv1a32(bytes: &[u8]) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in bytes {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}
