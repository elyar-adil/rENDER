//! Rasterise inline `<svg>` subtrees, so an icon in the document draws.
//!
//! The parser already builds a faithful SVG DOM: namespaced elements, the
//! case-adjusted local names of the "adjust SVG tag names" table
//! (`clipPath`, `foreignObject`, `viewBox`), the foreign-attribute tables, and
//! `xlink:`/`xml:`/`xmlns` attributes kept in their own namespaces. What was
//! missing is the *rendering* half: `render-layout` and the display list give
//! foreign content no geometry, so every inline `<svg>` on every page rendered
//! as nothing - and inline SVG is essentially all of a modern page's
//! iconography.
//!
//! The path from the DOM to pixels is short and adds no new rendering code:
//!
//! 1. find the SVG-namespace `svg` element;
//! 2. serialise its subtree with `render_html::serialize_html_node`, which
//!    restores the `xlink:`, `xml:` and `xmlns` prefixes;
//! 3. feed that text to [`super::svg`], the same rasteriser that draws an
//!    `<img src=icon.svg>`;
//! 4. register the result as an image resource keyed to the `svg` element, so
//!    the display list paints it through the ordinary `Image` command.
//!
//! # Sizing
//!
//! An `svg` element's size comes from its own `width`/`height` geometry
//! attributes and its `viewBox`, and never from layout, which cannot measure
//! foreign content. Those attributes are presentation attributes for the CSS
//! properties of the same name (SVG 2 §4.2: "all presentation attributes,
//! since they are defined by reference to their corresponding CSS
//! properties"), so [`crate::document`] turns them into user-agent-origin
//! `width`/`height` declarations through the presentational-hint path, and
//! [`svg_geometry`] resolves the same three values for the raster. One
//! resolution, two consumers.
//!
//! # What is deliberately not done
//!
//! * `use` / `symbol` / `defs` indirection. SVG 2 §3.2.4 renders a `use` as a
//!   shadow tree cloned from its target, which needs the clone to resolve its
//!   own styles and its own geometry. This rasteriser has no shadow tree, so
//!   a `<use>` contributes nothing. An honest absence is better than a global
//!   that silently returns an empty image.
//! * Gradients, `text`, `clipPath`, `mask`, `filter` and `<image>`: outside
//!   the rasteriser's documented subset, so they contribute nothing there
//!   either.
//! * `foreignObject`: its HTML subtree is laid out by render-layout as
//!   ordinary in-flow HTML rather than in the SVG viewport coordinate system,
//!   so it is positioned correctly only for `x=0 y=0` with no viewBox
//!   scaling. It is not rasterised, and an `svg` inside one is not descended
//!   into for rasterisation either - see [`discover_inline_svgs`].

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the raster size is clamped to a finite positive range immediately before each cast"
)]

use render_dom::{Dom, DomRevision, ElementData, Namespace, NodeId, NodeKind};
use render_html::serialize_html_node;
use url::Url;

use super::{
    DecodedImage, ImageLimits, ImageResourceId, ImageResourceKey, ImageResources,
    ImageSelectionContext, ImageSource, svg,
};

/// The `xlink` namespace URI, for the one attribute this module reads by
/// namespace.
const XLINK_NAMESPACE: Option<&str> = Some("http://www.w3.org/1999/xlink");

/// SVG 2 §3.2.1's "never-rendered element" types: elements with no direct
/// representation in the rendering tree whatever their `display` value.
///
/// [`crate::document`]'s user-agent sheet gives each of these `display: none`,
/// which is also what keeps their contents - a `<title>`'s text, a `<style>`'s
/// CSS - out of the page's text layout. The list is asserted against the
/// specification text in this crate's tests so the rule and the list cannot
/// drift apart.
pub const NEVER_RENDERED_SVG_ELEMENTS: [&str; 12] = [
    "clipPath",
    "defs",
    "desc",
    "linearGradient",
    "marker",
    "mask",
    "metadata",
    "pattern",
    "radialGradient",
    "script",
    "style",
    "title",
];

/// The SVG "HTML integration points" of HTML 13.2.6.5: the SVG-namespace
/// elements whose children are parsed as HTML rather than as foreign content.
///
/// Everything else below an `svg` is foreign content drawn by the raster, so
/// only these are descended into when looking for a *second*, independent
/// `svg` that needs its own raster.
pub const SVG_HTML_INTEGRATION_POINTS: [&str; 3] = ["foreignObject", "desc", "title"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InlineSvgDiagnosticCode {
    /// The node budget for the DOM walk was reached, so the remaining `svg`
    /// elements were never looked at.
    NodeLimit,
    /// The serialised subtree is larger than the encoded-byte budget.
    EncodedBytesLimit,
    /// The rasteriser could not produce an image from the serialised subtree.
    DecodeFailed,
    /// The image store refused the decoded raster.
    StoreLimit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineSvgDiagnostic {
    pub node: Option<NodeId>,
    pub code: InlineSvgDiagnosticCode,
    pub message: String,
}

// Deliberately not folded into `ImageDiscoveryDiagnostic`: none of that enum's
// variants names what can go wrong here, and coercing a decode failure into
// `UnsupportedScheme` would make the diagnostic describe something that did
// not happen. The two types carry the same shape, so a caller can render them
// the same way.

/// One decoded inline `svg`, ready to be installed.
#[derive(Clone, Debug, PartialEq)]
pub struct InlineSvgRaster {
    /// The `svg` element this raster belongs to.
    pub owner: NodeId,
    /// The identity the image store records the raster under.
    pub key: ImageResourceKey,
    /// The serialised subtree, which is also the raster's content identity:
    /// the same markup rasterises to the same image.
    pub source: String,
    pub image: DecodedImage,
}

/// A revision-bound pass over the document's inline `svg` elements.
#[derive(Clone, Debug, PartialEq)]
pub struct InlineSvgDiscovery {
    pub revision: DomRevision,
    pub resources: Vec<InlineSvgRaster>,
    pub diagnostics: Vec<InlineSvgDiagnostic>,
}

impl InlineSvgDiscovery {
    /// Install every raster into `images`, returning the resource ids the
    /// display list will read.
    ///
    /// A raster already installed with the same markup is left alone and its
    /// existing id returned, so a caller may run discovery on every frame
    /// without reallocating every icon. A raster the store refuses is reported
    /// and skipped, so one oversized icon cannot stop the rest of the page's
    /// icons from being installed.
    #[must_use]
    pub fn install(
        &self,
        images: &mut ImageResources,
        limits: ImageLimits,
    ) -> (Vec<ImageResourceId>, Vec<InlineSvgDiagnostic>) {
        let mut installed = Vec::with_capacity(self.resources.len());
        let mut diagnostics = Vec::new();
        for raster in &self.resources {
            if let Some(loaded) = images.get_for_node_url(raster.owner, &raster.key.requested_url)
                && loaded.key.source_snapshot == raster.key.source_snapshot
            {
                installed.push(loaded.id);
                continue;
            }
            // The markup changed, so whatever this owner held before is stale.
            // Leaving it would keep the old pixels alive and let
            // `get_for_node` answer with either of two entries.
            if let Some(previous) = images.get_for_node(raster.owner)
                && previous.key.source == ImageSource::Element
            {
                images.remove_node(raster.owner);
            }
            match images.insert(raster.key.clone(), raster.image.clone(), limits) {
                Ok(id) => installed.push(id),
                Err(error) => diagnostics.push(InlineSvgDiagnostic {
                    node: Some(raster.owner),
                    code: InlineSvgDiagnosticCode::StoreLimit,
                    message: format!("inline SVG raster was not stored: {error}"),
                }),
            }
        }
        (installed, diagnostics)
    }
}

/// Find and rasterise every inline `<svg>` element in the document.
///
/// The walk stops at `limits.max_discovery_nodes`, the same node budget image
/// discovery uses, so a pathological document cannot make this pass unbounded.
///
/// An `svg` element found *below* another one is not rasterised separately:
/// its content is already inside the outer element's raster, and painting it
/// again would draw the same geometry twice. The exception is an `svg` reached
/// through an SVG HTML integration point, where the content between the two is
/// HTML and the inner icon is not in the outer raster at all.
#[must_use]
pub fn discover_inline_svgs(dom: &Dom, limits: ImageLimits) -> InlineSvgDiscovery {
    let selection_context = ImageSelectionContext::default();
    let mut resources = Vec::new();
    let mut diagnostics = Vec::new();
    let mut stack = vec![(dom.document(), false)];
    let mut visited = 0_usize;

    while let Some((node_id, inside_an_svg)) = stack.pop() {
        if visited >= limits.max_discovery_nodes {
            diagnostics.push(InlineSvgDiagnostic {
                node: None,
                code: InlineSvgDiagnosticCode::NodeLimit,
                message: "inline SVG discovery stopped at its DOM node limit".to_owned(),
            });
            break;
        }
        visited += 1;
        let Some(node) = dom.node(node_id) else {
            continue;
        };
        let Some(element) = node_element(node.kind()) else {
            // A document, fragment or character-data node can still hold
            // children, so the walk continues through it rather than stopping.
            for child in node.children().iter().rev().copied() {
                stack.push((child, inside_an_svg));
            }
            continue;
        };

        if is_svg_root(element) {
            match rasterise(dom, node_id, element, limits, selection_context) {
                Ok(raster) => resources.push(raster),
                Err(diagnostic) => diagnostics.push(diagnostic),
            }
        }

        // A `template`'s contents are parentless and unreachable, so nothing
        // in one is on the page; skipping the subtree mirrors the invariant
        // that makes `<template>` contents inert.
        let skip_children = is_template(element)
            || (inside_an_svg && !is_svg_root(element) && !is_html_integration_point(element));
        if skip_children {
            continue;
        }
        let nested = inside_an_svg || is_svg_root(element);
        for child in node.children().iter().rev().copied() {
            stack.push((child, nested));
        }
    }

    InlineSvgDiscovery {
        revision: dom.revision(),
        resources,
        diagnostics,
    }
}

fn node_element(kind: &NodeKind) -> Option<&ElementData> {
    match kind {
        NodeKind::Element(element) => Some(element),
        _ => None,
    }
}

fn is_template(element: &ElementData) -> bool {
    element.namespace == Namespace::Html && element.local_name == "template"
}

/// Whether the element is an `svg` element in the SVG namespace.
///
/// The namespace test is not decoration: an HTML element with the local name
/// `svg` is unknown markup that is laid out as ordinary flow content, and
/// rasterising it would replace real page content with an empty image.
fn is_svg_root(element: &ElementData) -> bool {
    element.namespace == Namespace::Svg && element.local_name == "svg"
}

fn is_html_integration_point(element: &ElementData) -> bool {
    element.namespace == Namespace::Svg
        && SVG_HTML_INTEGRATION_POINTS.contains(&element.local_name.as_str())
}

fn rasterise(
    dom: &Dom,
    owner: NodeId,
    element: &ElementData,
    limits: ImageLimits,
    selection_context: ImageSelectionContext,
) -> Result<InlineSvgRaster, InlineSvgDiagnostic> {
    let source = serialize_html_node(dom, owner);
    if source.len() > limits.max_encoded_bytes {
        return Err(InlineSvgDiagnostic {
            node: Some(owner),
            code: InlineSvgDiagnosticCode::EncodedBytesLimit,
            message: format!(
                "inline SVG markup uses {} bytes; the encoded-byte limit is {}",
                source.len(),
                limits.max_encoded_bytes
            ),
        });
    }
    let (width, height) = svg_geometry(element).raster_size();
    let image =
        svg::decode_svg_viewport(source.as_bytes(), width, height, limits).map_err(|error| {
            InlineSvgDiagnostic {
                node: Some(owner),
                code: InlineSvgDiagnosticCode::DecodeFailed,
                message: format!("inline SVG did not rasterise: {error}"),
            }
        })?;
    Ok(InlineSvgRaster {
        owner,
        key: ImageResourceKey {
            owner,
            requested_url: inline_svg_url(owner, &source),
            // The serialised markup is the source snapshot, so editing the
            // subtree produces a different key and a fresh resource id, which
            // is what makes the display-list diff see the change.
            source_snapshot: source.clone(),
            source: ImageSource::Element,
            selection_context,
        },
        source,
        image,
    })
}

/// A stable, non-fetchable URL standing in for the markup an inline `svg`
/// carries.
///
/// [`ImageResourceKey`] is keyed by `(owner, requested_url)`, and an inline
/// `svg` has no URL to fetch. The `inline-svg:` scheme is not one the loader
/// supports, so such a key can never be mistaken for a network request, and
/// the content digest keeps two different markups for one owner from sharing a
/// slot.
fn inline_svg_url(owner: NodeId, source: &str) -> Url {
    Url::parse(&format!(
        "inline-svg://element/{}/{:016x}",
        owner.as_u64(),
        content_digest(source)
    ))
    .expect("the inline SVG URL scheme and its digits are a well-formed URL")
}

/// FNV-1a over the markup, for key identity only.
///
/// A collision would mean one icon's raster replacing another's for the same
/// owner and the display list then showing the wrong pixels, so the digest is
/// over the whole serialised subtree rather than its length.
fn content_digest(bytes: &str) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    bytes.as_bytes().iter().fold(OFFSET, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(PRIME)
    })
}

/// An `svg` element's own size, as its geometry attributes define it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SvgGeometry {
    /// The `width` attribute, when it names an absolute length.
    pub width: Option<f32>,
    /// The `height` attribute, when it names an absolute length.
    pub height: Option<f32>,
    /// The `viewBox`, as `(min-x, min-y, width, height)`.
    pub view_box: Option<(f32, f32, f32, f32)>,
}

impl SvgGeometry {
    /// The width the element's box takes, in CSS pixels.
    ///
    /// SVG 2 leaves the initial value of `width` at the property's initial
    /// value, which a browser resolves against the containing block. This
    /// engine resolves a missing or percentage `width` to the `viewBox` extent
    /// instead. That is a deliberate difference, and it is the safe direction:
    /// an unsized icon under the browser rule becomes a block as wide as its
    /// container, and no real page writes one.
    #[must_use]
    pub fn used_width(self) -> Option<f32> {
        self.width
            .or_else(|| self.view_box.and_then(|view| positive(view.2)))
    }

    /// The height the element's box takes, in CSS pixels. See
    /// [`Self::used_width`] for the `auto` difference.
    #[must_use]
    pub fn used_height(self) -> Option<f32> {
        self.height
            .or_else(|| self.view_box.and_then(|view| positive(view.3)))
    }

    /// The device-pixel size to raster at.
    ///
    /// A percentage or missing dimension has no device-pixel size before
    /// layout runs, so the `viewBox` extent is used, and the SVG default
    /// viewport (300x150) when there is no `viewBox` either. Paint then fits
    /// the raster to the element's content box through `object-fit`, so the
    /// box decides the on-screen size and this only decides the resolution.
    #[must_use]
    pub fn raster_size(self) -> (u32, u32) {
        let fallback_width = self.view_box.map_or(300.0, |view| view.2);
        let fallback_height = self.view_box.map_or(150.0, |view| view.3);
        let width = self.used_width().unwrap_or(fallback_width);
        let height = self.used_height().unwrap_or(fallback_height);
        (
            width.round().clamp(1.0, u32::MAX as f32) as u32,
            height.round().clamp(1.0, u32::MAX as f32) as u32,
        )
    }
}

fn positive(value: f32) -> Option<f32> {
    (value.is_finite() && value > 0.0).then_some(value)
}

/// Read an `svg` element's geometry attributes.
///
/// `width`, `height` and `viewBox` are in the null namespace for inline SVG,
/// so a null-namespace lookup finds them. `viewBox` is camelCase and the SVG
/// namespace is case-sensitive, so the name is spelled exactly - a lowercased
/// lookup finds nothing and silently leaves the viewport unscaled.
#[must_use]
pub fn svg_geometry(element: &ElementData) -> SvgGeometry {
    SvgGeometry {
        width: attribute_length(element, "width"),
        height: attribute_length(element, "height"),
        view_box: parse_view_box(element),
    }
}

fn attribute_value<'a>(element: &'a ElementData, name: &str) -> Option<&'a str> {
    element
        .attributes
        .iter()
        .find(|attribute| attribute.namespace.is_none() && attribute.local_name == name)
        .map(|attribute| attribute.value.trim())
        .filter(|value| !value.is_empty())
}

fn attribute_length(element: &ElementData, name: &str) -> Option<f32> {
    svg_length(attribute_value(element, name)?)
}

/// An SVG length in user units.
///
/// A user unit is a CSS pixel, so a unitless number and a `px` length are the
/// same value. Every other form is rejected rather than guessed: a percentage
/// needs a viewport the caller does not have, and a font-relative or physical
/// unit is not a count of user units, so reading `2em` as `2` would be a
/// silently wrong size.
fn svg_length(raw: &str) -> Option<f32> {
    let number: String = raw
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-' || *c == '+')
        .collect();
    let suffix = raw[number.len()..].trim();
    if !(suffix.is_empty() || suffix.eq_ignore_ascii_case("px")) {
        return None;
    }
    let value = number.parse::<f32>().ok()?;
    (value.is_finite()).then_some(value)
}

fn parse_view_box(element: &ElementData) -> Option<(f32, f32, f32, f32)> {
    let raw = attribute_value(element, "viewBox")?;
    let numbers: Vec<f32> = raw
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|part| !part.is_empty())
        .filter_map(svg_length)
        .collect();
    (numbers.len() == 4).then_some((numbers[0], numbers[1], numbers[2], numbers[3]))
}

/// The `href` of a namespaced reference such as `<use xlink:href="#icon">`.
///
/// A `xlink:href` is in the `xlink` namespace, and `Dom::attribute` deliberately
/// does not return it: that accessor matches the null namespace only, because
/// an attribute is identified by the `(namespace, local name)` pair. Reading a
/// namespaced attribute through the wrong accessor returns `None` and
/// silently drops every `xlink:href` reference, so the namespace is part of
/// this call and the tests assert both ways round.
#[must_use]
pub fn xlink_href(dom: &Dom, element: NodeId) -> Option<&str> {
    dom.attribute_ns(element, XLINK_NAMESPACE, "href")
        .ok()
        .flatten()
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "test surface coordinates are small, finite, non-negative device pixels"
    )]
    use super::{
        NEVER_RENDERED_SVG_ELEMENTS, SVG_HTML_INTEGRATION_POINTS, SvgGeometry,
        discover_inline_svgs, svg_geometry, xlink_href,
    };
    use crate::css::selector::{MatchContext, parse_selector_list, select_all};
    use crate::dom::{Dom, NodeId, NodeKind};
    use crate::html::parse_document_with_scripting;
    use crate::image::{Color, ImageLimits, ImageResources};
    use crate::layout::{FragmentKind, PhysicalSize};
    use crate::paint::Color as PaintColor;

    fn parse(html: &str) -> Dom {
        parse_document_with_scripting(html, true).dom
    }

    fn only_svg(dom: &Dom) -> NodeId {
        let selectors = parse_selector_list("svg").expect("valid selector");
        select_all(dom, dom.document(), &selectors, &MatchContext::default())[0]
    }

    fn element(dom: &Dom, node: NodeId) -> &crate::dom::ElementData {
        match dom.node(node).expect("node exists").kind() {
            NodeKind::Element(element) => element,
            other => panic!("expected an element, found {other:?}"),
        }
    }

    #[test]
    fn an_inline_svg_rasterises_its_shapes() {
        let dom = parse(
            "<!doctype html><body><svg width=10 height=10 viewBox='0 0 10 10'>\
             <rect x='2' y='2' width='6' height='6' fill='#ff0000'/></svg>",
        );
        let discovery = discover_inline_svgs(&dom, ImageLimits::default());

        assert_eq!(discovery.resources.len(), 1);
        assert!(discovery.diagnostics.is_empty());
        let raster = &discovery.resources[0];
        assert_eq!((raster.image.width(), raster.image.height()), (10, 10));
        assert_eq!(raster.image.pixel(4, 4), Some(Color::rgb(255, 0, 0)));
        assert_eq!(raster.image.pixel(9, 9), Some(Color::rgba(0, 0, 0, 0)));
        // The serialised subtree is what went to the rasteriser, with the
        // case-adjusted local names the parser produced.
        assert!(raster.source.contains("<rect"), "{}", raster.source);
    }

    #[test]
    fn the_raster_is_measured_from_the_geometry_attributes_and_painted() {
        let document = crate::document::Document::parse(
            "<!doctype html><style>body{margin:0}</style>\
             <svg id=icon width=20 height=10 viewBox='0 0 10 10'>\
             <rect width='10' height='10' fill='#0000ff'/></svg>",
        );
        let discovery = discover_inline_svgs(document.dom(), ImageLimits::default());
        let mut images = ImageResources::default();
        let (installed, diagnostics) = discovery.install(&mut images, ImageLimits::default());
        assert_eq!(installed.len(), 1);
        assert!(diagnostics.is_empty());

        let render = document.render_with_images(
            crate::document::DocumentRenderOptions::default(),
            crate::document::DocumentBackends {
                text_measurer: &crate::layout::SimpleTextMeasurer,
                text_shaper: &crate::paint::ReferenceTextShaper,
                glyph_masks: &crate::paint::NoGlyphMasks,
            },
            &images,
        );

        // The box comes from width/height, and the raster from the same two
        // attributes: a 2:1 viewBox stretched into a 20x10 box.
        let icon = only_svg(document.dom());
        let box_size = render
            .layout
            .fragments
            .iter()
            .find_map(
                |fragment| match (&fragment.kind, fragment.source == Some(icon)) {
                    (FragmentKind::Box(geometry), true) => Some(geometry.content_rect.size),
                    _ => None,
                },
            )
            .expect("the svg element is laid out as a box");
        assert_eq!(
            box_size,
            PhysicalSize {
                width: 20.0,
                height: 10.0
            }
        );
        let destination = render
            .display
            .list
            .items()
            .iter()
            .find_map(|item| match item.command {
                crate::paint::DisplayCommand::Image(image) => Some(image.destination),
                _ => None,
            })
            .expect("the raster is painted as an image");
        assert_eq!(
            destination.size,
            crate::layout::PhysicalSize {
                width: 20.0,
                height: 10.0
            }
        );
        assert_eq!(
            render.raster.surface.pixel(
                destination.origin.x as u32 + 1,
                destination.origin.y as u32 + 1
            ),
            Some(PaintColor::rgb(0, 0, 255))
        );
    }

    /// Installing the same markup twice keeps one resource, so a caller may
    /// run discovery every frame; changed markup gets a new resource id, which
    /// is what makes the display-list diff see the change.
    #[test]
    fn installing_unchanged_markup_reuses_the_resource_and_changed_markup_does_not() {
        let dom = parse(
            "<!doctype html><body><svg width=4 height=4><rect width=4 height=4 fill=black/></svg>",
        );
        let mut images = ImageResources::default();
        let limits = ImageLimits::default();
        let (first, diagnostics) = discover_inline_svgs(&dom, limits).install(&mut images, limits);
        assert!(diagnostics.is_empty());
        let (second, diagnostics) = discover_inline_svgs(&dom, limits).install(&mut images, limits);

        assert!(diagnostics.is_empty());
        assert_eq!(first, second);
        assert_eq!(images.len(), 1);

        // `span` would not do here: it is on the foreign-content breakout list,
        // so it closes the `svg` and leaves the serialised subtree identical.
        let changed = parse(
            "<!doctype html><body><svg width=4 height=4>\
             <g><rect width=4 height=4 fill=black/></g></svg>",
        );
        let (third, diagnostics) =
            discover_inline_svgs(&changed, limits).install(&mut images, limits);
        assert!(diagnostics.is_empty());
        assert_eq!(third.len(), 1);
        assert_ne!(
            third[0], first[0],
            "changed markup needs a new resource id so the display-list diff sees it"
        );
        assert_eq!(images.len(), 1, "the owner still holds exactly one raster");
    }

    /// A `xlink:href` is in the `xlink` namespace and a null-namespace `href`
    /// is a different attribute. Reading the namespaced one through the
    /// null-namespace accessor returns `None`, which would silently drop every
    /// `xlink:href` reference on a page, so both directions are asserted.
    #[test]
    fn xlink_href_is_read_through_its_own_namespace() {
        let dom = parse(
            "<!doctype html><body><svg>\
             <use id=both xlink:href='#xlink-target' href='#null-target'></use>\
             <use id=namespaced xlink:href='#only-xlink'></use>\
             <use id=null-only href='#only-null'></use>\
             </svg>",
        );
        let node = |selector: &str| {
            select_all(
                &dom,
                dom.document(),
                &parse_selector_list(selector).expect("valid selector"),
                &MatchContext::default(),
            )[0]
        };

        assert_eq!(xlink_href(&dom, node("#both")), Some("#xlink-target"));
        assert_eq!(
            dom.attribute(node("#both"), "href")
                .expect("attributes are readable"),
            Some("#null-target"),
            "the null-namespace accessor must not see a xlink:href"
        );
        assert_eq!(xlink_href(&dom, node("#namespaced")), Some("#only-xlink"));
        assert_eq!(
            dom.attribute(node("#namespaced"), "href")
                .expect("attributes are readable"),
            None,
            "an xlink:href alone must not be readable as a plain href"
        );
        assert_eq!(
            xlink_href(&dom, node("#null-only")),
            None,
            "a null-namespace href is not a xlink:href"
        );
    }

    /// The namespace test is load-bearing: an HTML element with the local name
    /// `svg` is unknown markup that lays out as ordinary flow content, and
    /// rasterising it would replace real page content with an empty image.
    ///
    /// The parser puts a `<svg>` start tag in the SVG namespace, so the guard
    /// can only be exercised against an element built through the DOM API -
    /// which is what `document.createElement("svg")` produces, and what a
    /// script gets.
    #[test]
    fn an_html_element_named_svg_is_not_rasterised() {
        let mut dom = parse("<!doctype html><body><p>text</p>");
        let body = select_all(
            &dom,
            dom.document(),
            &parse_selector_list("body").expect("valid selector"),
            &MatchContext::default(),
        )[0];
        let fake = dom.create_element("svg");
        dom.append_child(body, fake).expect("append the element");
        let rect = dom.create_element("rect");
        dom.set_attribute(rect, "width", "4").expect("set width");
        dom.set_attribute(rect, "height", "4").expect("set height");
        dom.append_child(fake, rect).expect("append the child");

        assert_eq!(element(&dom, fake).namespace, crate::dom::Namespace::Html);
        let discovery = discover_inline_svgs(&dom, ImageLimits::default());
        assert!(
            discovery.resources.is_empty(),
            "an HTML-namespace svg is not foreign content and must keep its own layout"
        );
        assert!(discovery.diagnostics.is_empty());

        // The parser's own namespace assignment is the other half, and it is
        // what the guard exists to respect.
        let parsed = parse("<!doctype html><body><svg><rect width=4 height=4/></svg>");
        assert_eq!(
            element(&parsed, only_svg(&parsed)).namespace,
            crate::dom::Namespace::Svg
        );
        assert_eq!(
            discover_inline_svgs(&parsed, ImageLimits::default())
                .resources
                .len(),
            1
        );
    }

    /// A nested `svg` is already inside the outer raster, so rasterising it
    /// again would draw the same geometry twice. An `svg` reached through an
    /// SVG HTML integration point is a different case: the content between them
    /// is HTML, so the inner icon is not in the outer raster.
    #[test]
    fn a_nested_svg_is_rasterised_once_unless_an_html_island_separates_them() {
        let nested = parse(
            "<!doctype html><body><svg width=8 height=8>\
             <g><svg width=4 height=4><rect width=4 height=4 fill=black/></svg></g></svg>",
        );
        let island = parse(
            "<!doctype html><body><svg width=8 height=8><foreignObject width=8 height=8>\
             <svg width=4 height=4><rect width=4 height=4 fill=black/></svg>\
             </foreignObject></svg>",
        );

        let nested_discovery = discover_inline_svgs(&nested, ImageLimits::default());
        assert_eq!(nested_discovery.resources.len(), 1);
        assert_eq!(nested_discovery.resources[0].owner, only_svg(&nested));

        let island_discovery = discover_inline_svgs(&island, ImageLimits::default());
        assert_eq!(
            island_discovery.resources.len(),
            2,
            "an svg inside a foreignObject is not part of the outer raster"
        );
    }

    /// SVG 2 §3.2.1's never-rendered element types, as the specification text
    /// lists them. The user-agent sheet in `document.rs` carries a rule over
    /// these names, and this is what stops the two copies drifting.
    #[test]
    fn the_never_rendered_element_list_matches_svg_2() {
        assert_eq!(
            NEVER_RENDERED_SVG_ELEMENTS,
            [
                "clipPath",
                "defs",
                "desc",
                "linearGradient",
                "marker",
                "mask",
                "metadata",
                "pattern",
                "radialGradient",
                "script",
                "style",
                "title",
            ]
        );
        assert_eq!(
            SVG_HTML_INTEGRATION_POINTS,
            ["foreignObject", "desc", "title"]
        );
    }

    #[test]
    fn never_rendered_svg_elements_take_no_part_in_layout() {
        let document = crate::document::Document::parse(
            "<!doctype html><svg id=icon width=8 height=8><title>Star</title>\
             <desc>A five pointed star</desc><defs><rect width=8 height=8 fill=black/></defs>\
             <rect width=8 height=8 fill=black/></svg>",
        );
        let render = document.render_reference(crate::document::DocumentRenderOptions::default());
        for selector in ["title", "desc", "defs"] {
            let node = select_all(
                document.dom(),
                document.dom().document(),
                &parse_selector_list(selector).expect("valid selector"),
                &MatchContext::default(),
            )[0];
            assert!(
                render
                    .layout
                    .fragments
                    .iter()
                    .all(|f| f.source != Some(node)),
                "a {selector} element is never rendered, so it must have no box"
            );
        }
        // The icon itself does get one, or the rule would have fixed the
        // problem by hiding everything.
        let icon = only_svg(document.dom());
        assert!(
            render
                .layout
                .fragments
                .iter()
                .any(|f| f.source == Some(icon)),
            "the svg element is laid out as a box"
        );
    }

    #[test]
    fn geometry_reads_width_height_and_the_camel_case_view_box() {
        let dom = parse(
            "<!doctype html><body>\
             <svg id=explicit width='24' height='16' viewBox='0 0 48 32'></svg>\
             <svg id=viewbox-only viewBox='0 0 10 4'></svg>\
             <svg id=percent width='100%' height='50%' viewBox='0 0 30 20'></svg>\
             <svg id=em width='2em' height='16px' viewBox='0 0 30 20'></svg>\
             <svg id=bare></svg>",
        );
        let geometry = |selector: &str| {
            let node = select_all(
                &dom,
                dom.document(),
                &parse_selector_list(selector).expect("valid selector"),
                &MatchContext::default(),
            )[0];
            svg_geometry(element(&dom, node))
        };

        assert_eq!(
            geometry("#explicit"),
            SvgGeometry {
                width: Some(24.0),
                height: Some(16.0),
                view_box: Some((0.0, 0.0, 48.0, 32.0)),
            }
        );
        // A missing or percentage dimension falls back to the viewBox extent.
        assert_eq!(geometry("#viewbox-only").used_width(), Some(10.0));
        assert_eq!(geometry("#viewbox-only").used_height(), Some(4.0));
        assert_eq!(geometry("#percent").used_width(), Some(30.0));
        assert_eq!(geometry("#percent").used_height(), Some(20.0));
        // A `px` length is a user unit; a font-relative one is not a count of
        // user units and is rejected rather than read as its number.
        assert_eq!(geometry("#em").used_height(), Some(16.0));
        assert_eq!(geometry("#em").used_width(), Some(30.0));
        // With neither, the raster falls back to the SVG default viewport.
        assert_eq!(geometry("#bare").raster_size(), (300, 150));
        // The box and the raster read the same two attributes.
        assert_eq!(geometry("#explicit").raster_size(), (24, 16));
        assert_eq!(geometry("#viewbox-only").raster_size(), (10, 4));
    }

    /// The node budget is reported, so a truncated pass never looks complete.
    #[test]
    fn the_node_budget_is_reported() {
        let dom =
            parse("<!doctype html><body><svg width=4 height=4><rect width=4 height=4/></svg>");
        let discovery = discover_inline_svgs(
            &dom,
            ImageLimits {
                max_discovery_nodes: 1,
                ..ImageLimits::default()
            },
        );

        assert_eq!(discovery.diagnostics.len(), 1);
        assert_eq!(
            discovery.diagnostics[0].code,
            super::InlineSvgDiagnosticCode::NodeLimit
        );
    }
}
