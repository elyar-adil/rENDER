//! Minimal static-SVG rasterizer for `<img>` content.
//!
//! Real-world icon SVGs draw with a compact vector subset; this module
//! implements that subset by the SVG spec so ordinary icons render
//! correctly everywhere, without site-specific handling:
//!
//! - shapes: `rect`, `circle`, `ellipse`, `polygon`, `polyline`, `line`,
//!   and `path` segments `M m L l H h V v C c S s Q q T t Z z` (arc `A`
//!   degrades to a line to its endpoint);
//! - nested `<g>` transforms: `translate`, `scale`, `matrix`, `rotate`,
//!   `skewX`, `skewY`;
//! - `fill`/`stroke` paints inherited through `<g>`, with `none`;
//! - linear and radial gradients, including `gradientUnits`,
//!   `gradientTransform`, `stop-color` and `stop-opacity`, for fills and
//!   strokes;
//! - `clipPath` references on shapes and groups, with `clipPathUnits` and
//!   transforms;
//! - same-document `<use>` references through `href`/`xlink:href`, rendering a
//!   shadow-tree clone of the target: `x`/`y`, the use element's transform and
//!   inherited paint, nested `<use>` with cycle detection and a depth cap, and
//!   `<symbol>`/`<svg>` targets whose `viewBox` and `preserveAspectRatio` map
//!   into a viewport established by the use element's `width`/`height`; that
//!   viewport clips the symbol, and `<symbol>` is never rendered directly;
//! - `mask` references on shapes and groups, with offscreen alpha or
//!   luminance compositing, `maskUnits` and `maskContentUnits`;
//! - `filter` references with `feGaussianBlur`, `feColorMatrix`, `feOffset`,
//!   `feBlend`, `feComposite`, `feFlood`, `feMerge`, `feDropShadow` and
//!   `feMorphology`, composited through offscreen buffers, with
//!   `filterUnits`, `primitiveUnits`, filter-region percentages/primitive
//!   subregions, CSS blend modes, and Porter-Duff plus arithmetic
//!   compositing. Unsupported primitives and units use a documented visible
//!   fallback and are reported by [`decode_svg_with_diagnostics`];
//! - root sizing by `width`/`height` (percentages resolve against the
//!   `viewBox`), falling back to `viewBox`, then the spec default 300×150.
//!
//! Everything else outside the subset (text, embedded images, scripting,
//! external-document `<use>` references, gradient `xlink:href` inheritance
//! and spread modes other than `pad`) contributes nothing, matching how an
//! `<img>`-loaded SVG must render: static and script-free. An external
//! `<use href="file.svg#id">` or an unresolved local fragment is a defined
//! fallback - that use paints nothing - and is reported by
//! [`decode_svg_with_diagnostics`] with
//! [`SvgRenderDiagnosticCode::UnsupportedReference`], never silently treated
//! as a local id. Unknown filter primitives are the exception: their input is
//! passed through and reported, so the rest of the filter chain stays visible.
//!
//! Two things about the document are easy to get wrong and are pinned by
//! tests: it is XML, so an attribute name is case-sensitive and `viewBox` is
//! spelled with its capital `B`; and [`decode_svg_viewport`] exists because a
//! caller that already knows the device-pixel size its box will occupy must
//! be able to rasterise at that size rather than at the root's own attributes.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "scanline rasterization clamps coordinates before every cast"
)]

use std::collections::BTreeMap;
use std::str;

use super::{Color, DecodedImage, ImageDecodeError, ImageLimits, enforce_dimensions};

/// One polyline segment in device coordinates.
type Segment = ((f32, f32), (f32, f32));

/// Affine transform `[a c e; b d f]`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Affine {
    a: f32,
    b: f32,
    c: f32,
    d: f32,
    e: f32,
    f: f32,
}

impl Affine {
    const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    fn apply(self, x: f32, y: f32) -> (f32, f32) {
        (
            self.a.mul_add(x, self.c.mul_add(y, self.e)),
            self.b.mul_add(x, self.d.mul_add(y, self.f)),
        )
    }

    /// Compose two transforms: `parent(child(point))`.
    fn then(self, child: Self) -> Self {
        Self {
            a: self.a * child.a + self.c * child.b,
            b: self.b * child.a + self.d * child.b,
            c: self.a * child.c + self.c * child.d,
            d: self.b * child.c + self.d * child.d,
            e: self.a * child.e + self.c * child.f + self.e,
            f: self.b * child.e + self.d * child.f + self.f,
        }
    }

    /// The inverse affine, or `None` when the transform is singular.
    ///
    /// Gradient painting maps each device pixel back into user space to sample
    /// its colour, so a degenerate transform has no colour to sample and the
    /// paint contributes nothing rather than dividing by zero.
    fn inverse(self) -> Option<Self> {
        let determinant = self.a.mul_add(self.d, -(self.b * self.c));
        if !determinant.is_finite() || determinant.abs() <= f32::EPSILON {
            return None;
        }
        let inverse = determinant.recip();
        Some(Self {
            a: self.d * inverse,
            b: -self.b * inverse,
            c: -self.c * inverse,
            d: self.a * inverse,
            e: (self.c * self.f - self.d * self.e) * inverse,
            f: (self.b * self.e - self.a * self.f) * inverse,
        })
    }
}

/// Paint for one element: explicit color, explicit `none`, an indexed
/// gradient, or inherited.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Paint {
    Inherit,
    None,
    Color(Color),
    Gradient(usize),
}

impl Paint {
    fn resolve(self, inherited: Option<Paint>) -> Option<Paint> {
        match self {
            Self::Inherit => inherited,
            Self::None => None,
            Self::Color(color) => Some(Self::Color(color)),
            Self::Gradient(index) => Some(Self::Gradient(index)),
        }
    }
}

/// The coordinate system a gradient's own geometry is expressed in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GradientUnits {
    ObjectBoundingBox,
    UserSpaceOnUse,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClipPathUnits {
    ObjectBoundingBox,
    UserSpaceOnUse,
}

/// The `align` half of an SVG `preserveAspectRatio` value. `None` is the
/// `none` keyword, which stretches the viewBox independently on both axes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ViewBoxAlign {
    None,
    Min,
    Mid,
    Max,
}

impl ViewBoxAlign {
    /// The fraction of the leftover viewport axis used as the offset before
    /// the scaled viewBox.
    fn factor(self) -> f32 {
        match self {
            Self::None | Self::Min => 0.0,
            Self::Mid => 0.5,
            Self::Max => 1.0,
        }
    }
}

/// `preserveAspectRatio`, as the viewBox-to-viewport mapping needs it.
///
/// The SVG default is `xMidYMid meet`: scale uniformly by the smaller axis and
/// centre the result. `slice` scales by the larger axis instead and overflows
/// the viewport, which is why a `<use>` of a `<symbol>` clips to its viewport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PreserveAspectRatio {
    align_x: ViewBoxAlign,
    align_y: ViewBoxAlign,
    slice: bool,
}

impl Default for PreserveAspectRatio {
    fn default() -> Self {
        Self {
            align_x: ViewBoxAlign::Mid,
            align_y: ViewBoxAlign::Mid,
            slice: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum GradientKind {
    Linear { x1: f32, y1: f32, x2: f32, y2: f32 },
    Radial { cx: f32, cy: f32, r: f32 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct GradientStop {
    offset: f32,
    color: Color,
}

/// One `linearGradient` or `radialGradient` definition.
///
/// `gradientTransform` is applied to the gradient's coordinate system, not to
/// the shape, so sampling maps the point through its inverse first. The stop
/// list is sorted by offset; sampling clamps outside the first and last stop,
/// which is SVG's default `spreadMethod="pad"`.
#[derive(Clone, Debug, PartialEq)]
struct Gradient {
    kind: GradientKind,
    units: GradientUnits,
    transform: Affine,
    stops: Vec<GradientStop>,
}

impl Gradient {
    fn sample(&self, x: f32, y: f32) -> Color {
        let progress = match self.kind {
            GradientKind::Linear { x1, y1, x2, y2 } => {
                let dx = x2 - x1;
                let dy = y2 - y1;
                let denominator = dx.mul_add(dx, dy * dy);
                if denominator <= f32::EPSILON {
                    0.0
                } else {
                    ((x - x1).mul_add(dx, (y - y1) * dy) / denominator).clamp(0.0, 1.0)
                }
            }
            GradientKind::Radial { cx, cy, r } => {
                if r <= 0.0 {
                    0.0
                } else {
                    ((x - cx).hypot(y - cy) / r).clamp(0.0, 1.0)
                }
            }
        };
        self.color_at(progress)
    }

    fn color_at(&self, progress: f32) -> Color {
        let Some(first) = self.stops.first() else {
            return Color::TRANSPARENT;
        };
        if progress <= first.offset {
            return first.color;
        }
        let Some(last) = self.stops.last() else {
            return first.color;
        };
        if progress >= last.offset {
            return last.color;
        }
        for pair in self.stops.windows(2) {
            let (start, end) = (pair[0], pair[1]);
            if progress < start.offset || progress > end.offset {
                continue;
            }
            let span = end.offset - start.offset;
            if span <= f32::EPSILON {
                return end.color;
            }
            return lerp_color(start.color, end.color, (progress - start.offset) / span);
        }
        last.color
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "channels are clamped to 0..=255 before the cast"
)]
fn lerp_color(from: Color, to: Color, progress: f32) -> Color {
    let progress = progress.clamp(0.0, 1.0);
    let channel = |start: u8, end: u8| -> u8 {
        (f32::from(start) + (f32::from(end) - f32::from(start)) * progress)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Color::rgba(
        channel(from.red, to.red),
        channel(from.green, to.green),
        channel(from.blue, to.blue),
        channel(from.alpha, to.alpha),
    )
}

/// A stack of clip layers, each layer the union of its polygons.
///
/// Nested clips intersect: a pixel must be inside every layer. The clips are
/// kept in device space because each shape paints immediately; a group clip
/// applies to every descendant drawn while it is on the stack.
#[derive(Clone, Debug, Default)]
struct ClipStack {
    layers: Vec<ClipLayer>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClipFillRule {
    NonZero,
    EvenOdd,
}

#[derive(Clone, Debug)]
struct ClipPolygon {
    contours: Vec<Vec<(f32, f32)>>,
    fill_rule: ClipFillRule,
}

#[derive(Clone, Debug)]
struct ClipLayer {
    /// Polygons in device space. An empty layer clips everything, which is
    /// what an empty `<clipPath>` means.
    polygons: Vec<ClipPolygon>,
}

impl ClipStack {
    fn push(&mut self, layer: ClipLayer) {
        self.layers.push(layer);
    }

    fn pop(&mut self) {
        self.layers.pop();
    }

    fn contains(&self, x: f32, y: f32) -> bool {
        self.layers.iter().all(|layer| {
            layer
                .polygons
                .iter()
                .any(|polygon| point_in_contours(&polygon.contours, polygon.fill_rule, x, y))
        })
    }
}

/// Point-in-polygon for SVG clip paths. A `path` contributes all of its
/// subpaths to one geometry, so a nested counter-clockwise contour is a hole
/// under `evenodd` and a second winding under `nonzero`.
fn point_in_contours(
    contours: &[Vec<(f32, f32)>],
    fill_rule: ClipFillRule,
    x: f32,
    y: f32,
) -> bool {
    let mut inside = false;
    let mut winding = 0_i32;
    for points in contours {
        if points.len() < 3 {
            continue;
        }
        for index in 0..points.len() {
            let (x1, y1) = points[index];
            let (x2, y2) = points[(index + 1) % points.len()];
            if (y1 > y) != (y2 > y) {
                let crossing = (x2 - x1).mul_add((y - y1) / (y2 - y1), x1);
                if x < crossing {
                    inside = !inside;
                }
            }
            let orientation = (x2 - x1) * (y - y1) - (x - x1) * (y2 - y1);
            if y2 > y1 && orientation > 0.0 {
                winding += 1;
            } else if y2 < y1 && orientation < 0.0 {
                winding -= 1;
            }
        }
    }
    match fill_rule {
        ClipFillRule::NonZero => winding != 0,
        ClipFillRule::EvenOdd => inside,
    }
}

/// Which part of the rasteriser reported a [`SvgRenderDiagnostic`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SvgRenderDiagnosticCode {
    /// A filter primitive or unit outside the supported subset; its input is
    /// passed through or the SVG default is used.
    UnsupportedFilter,
    /// A `<use>` reference that cannot be resolved in this document, or a
    /// reference form outside the supported subset. Nothing is rendered for
    /// that use element.
    UnsupportedReference,
    /// A presentation or geometry attribute whose value is outside the
    /// supported subset; the documented default was used instead.
    UnsupportedAttribute,
}

/// One rendering feature the rasteriser could not honour.
///
/// The rasteriser keeps rendering the document around the unsupported
/// feature, and reports that feature here instead of silently claiming to
/// have applied it. The returned pixels are therefore a defined fallback:
/// an unknown filter primitive passes its input through, an unknown blend
/// mode uses `normal`, an unknown unit falls back to the SVG default, and an
/// unresolved `<use>` renders nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SvgRenderDiagnostic {
    /// The element the diagnostic belongs to: the filter primitive, or
    /// `use` for a reference diagnostic.
    pub element: String,
    /// The reported category, so callers can route it without parsing text.
    pub code: SvgRenderDiagnosticCode,
    /// What could not be honoured and the fallback that was used.
    pub message: String,
}

/// Decode an SVG document into an RGBA image.
///
/// # Errors
///
/// Returns [`ImageDecodeError`] when the bytes are not valid UTF-8, no root
/// `<svg>` element exists, or the target size violates `limits`.
pub fn decode_svg(bytes: &[u8], limits: ImageLimits) -> Result<DecodedImage, ImageDecodeError> {
    decode_svg_with_diagnostics(bytes, limits).map(|(image, _)| image)
}

/// Decode an SVG document and report features outside the supported subset.
///
/// Diagnostics cover unsupported filter primitives/units/attributes and
/// `<use>` reference failures. Every diagnostic has a defined fallback: an
/// unsupported filter passes its input through, and a failed `<use>` paints
/// nothing while the rest of the document renders.
///
/// # Errors
///
/// Returns [`ImageDecodeError`] under the same conditions as [`decode_svg`].
pub fn decode_svg_with_diagnostics(
    bytes: &[u8],
    limits: ImageLimits,
) -> Result<(DecodedImage, Vec<SvgRenderDiagnostic>), ImageDecodeError> {
    let text = str::from_utf8(bytes)
        .map_err(|_| ImageDecodeError::Codec("svg document is not valid UTF-8".to_owned()))?;
    // image/svg+xml is an XML document type, so the tree is parsed as XML:
    // self-closing elements close (an HTML tree builder would keep them
    // open and nest every following sibling inside).
    let root = parse_xml(text).ok_or_else(|| {
        ImageDecodeError::Codec("svg document has no <svg> root element".to_owned())
    })?;
    let view_box = root_view_box(&root);
    let (width_f, height_f) = root_size(
        attribute(&root, "width").and_then(parse_length),
        attribute(&root, "height").and_then(parse_length),
        view_box,
    );
    // A standalone image has no surrounding `color` to inherit, so
    // `currentColor` takes the initial value of `color`.
    decode_viewport(&root, view_box, width_f, height_f, Color::BLACK, limits)
}

/// Decode an SVG document into an RGBA image of exactly `width` x `height`
/// device pixels, resolving `currentColor` to `current_color`.
///
/// The `viewBox`, when the root has one, still supplies the user-space
/// mapping: the geometry is scaled to fill the given viewport rather than to
/// the root's own `width`/`height` attributes. Callers that already know the
/// device-pixel size their page box will occupy - the inline-SVG path in
/// [`crate::image::inline_svg`] resolves it from the element's geometry
/// attributes - use this so the raster is produced at the size it is painted.
///
/// `current_color` is the value `currentColor` takes. An inline `<svg>`
/// passes its element's computed `color`; a standalone image passes the
/// initial value, which is what [`decode_svg`] does.
///
/// # Errors
///
/// Returns [`ImageDecodeError`] when the bytes are not valid UTF-8, no root
/// `<svg>` element exists, or the target size violates `limits`.
pub fn decode_svg_viewport(
    bytes: &[u8],
    width: u32,
    height: u32,
    current_color: Color,
    limits: ImageLimits,
) -> Result<DecodedImage, ImageDecodeError> {
    decode_svg_viewport_with_diagnostics(bytes, width, height, current_color, limits)
        .map(|(image, _)| image)
}

/// [`decode_svg_viewport`] with the filter diagnostics from the raster.
///
/// # Errors
///
/// Returns [`ImageDecodeError`] under the same conditions as
/// [`decode_svg_viewport`].
pub fn decode_svg_viewport_with_diagnostics(
    bytes: &[u8],
    width: u32,
    height: u32,
    current_color: Color,
    limits: ImageLimits,
) -> Result<(DecodedImage, Vec<SvgRenderDiagnostic>), ImageDecodeError> {
    let text = str::from_utf8(bytes)
        .map_err(|_| ImageDecodeError::Codec("svg document is not valid UTF-8".to_owned()))?;
    let root = parse_xml(text).ok_or_else(|| {
        ImageDecodeError::Codec("svg document has no <svg> root element".to_owned())
    })?;
    let view_box = root_view_box(&root);
    decode_viewport(
        &root,
        view_box,
        width.max(1) as f32,
        height.max(1) as f32,
        current_color,
        limits,
    )
}

/// The root element's `viewBox`, as `(min-x, min-y, width, height)`.
///
/// `viewBox` is one of the two camelCase names SVG uses, and the document is
/// XML, so the attribute name is matched case-sensitively. Reading it as
/// `viewbox` silently found nothing, which left the root viewport unscaled
/// while still reporting a plausible size.
fn root_view_box(root: &XmlNode) -> Option<(f32, f32, f32, f32)> {
    attribute(root, "viewBox").and_then(parse_view_box)
}

fn decode_viewport(
    root: &XmlNode,
    view_box: Option<(f32, f32, f32, f32)>,
    width_f: f32,
    height_f: f32,
    current_color: Color,
    limits: ImageLimits,
) -> Result<(DecodedImage, Vec<SvgRenderDiagnostic>), ImageDecodeError> {
    let width = width_f.round().max(1.0) as u32;
    let height = height_f.round().max(1.0) as u32;
    enforce_dimensions(width, height, limits)?;

    // Filter lengths in `filterUnits="userSpaceOnUse"` resolve percentages
    // against the viewport in user units. A viewBox supplies it directly;
    // otherwise the requested device size is the user-space size because no
    // root transform exists.
    let viewport = view_box.map_or((width_f, height_f), |(_, _, view_width, view_height)| {
        (view_width, view_height)
    });
    let mut diagnostics = Vec::new();
    let mut raster = Raster {
        width,
        height,
        viewport,
        pixels: vec![Color::rgba(0, 0, 0, 0); width as usize * height as usize],
        clip: ClipStack::default(),
        use_depth: 0,
        use_stack: Vec::new(),
        mask_depth: 0,
        filter_depth: 0,
        opacity_depth: 0,
    };

    // Root viewBox-to-viewport mapping, composed ahead of every shape.
    let root_transform = if let Some((min_x, min_y, view_width, view_height)) = view_box
        && view_width > 0.0
        && view_height > 0.0
    {
        let scale_x = width as f32 / view_width;
        let scale_y = height as f32 / view_height;
        Affine::IDENTITY
            .then(Affine {
                a: scale_x,
                b: 0.0,
                c: 0.0,
                d: scale_y,
                e: 0.0,
                f: 0.0,
            })
            .then(Affine {
                a: 1.0,
                b: 0.0,
                c: 0.0,
                d: 1.0,
                e: -min_x * scale_x,
                f: -min_y * scale_y,
            })
    } else {
        Affine::IDENTITY
    };

    // The root's own `fill` and `stroke` are the inherited starting values for
    // every shape below it, so they are read here: `walk` only reads the
    // attributes of the children it visits. Their initial values are black
    // fill and no stroke. An unparseable value is ignored, which leaves the
    // initial value in force.
    let (gradients, gradient_index) = collect_gradients(root, current_color);
    let clip_paths = collect_clip_paths(root);
    let mask_paths = collect_masks(root);
    let filter_paths = collect_filters(root);
    let ids = collect_element_ids(root);
    let root_color = property(root, "color")
        .and_then(parse_color)
        .unwrap_or(current_color);
    let fill = root_paint(
        root,
        "fill",
        root_color,
        Paint::Color(Color::BLACK),
        &gradient_index,
    );
    let stroke = root_paint(root, "stroke", root_color, Paint::None, &gradient_index);
    let root_fill_opacity = property(root, "fill-opacity")
        .and_then(parse_opacity)
        .unwrap_or(1.0);
    let root_stroke_opacity = property(root, "stroke-opacity")
        .and_then(parse_opacity)
        .unwrap_or(1.0);
    let root_fill_rule = match property(root, "fill-rule") {
        Some(value) if value.trim().eq_ignore_ascii_case("evenodd") => FillRule::EvenOdd,
        _ => FillRule::NonZero,
    };
    let root_stroke_width = property(root, "stroke-width")
        .and_then(parse_length)
        .unwrap_or(1.0);
    walk(
        root,
        root_transform,
        fill,
        stroke,
        root_color,
        root_fill_opacity,
        root_stroke_opacity,
        root_fill_rule,
        root_stroke_width,
        &gradients,
        &gradient_index,
        &clip_paths,
        &mask_paths,
        &filter_paths,
        &ids,
        &mut raster,
        &mut diagnostics,
    );
    let image = DecodedImage::from_pixels(width, height, raster.pixels)?;
    Ok((image, diagnostics))
}

struct Raster {
    width: u32,
    height: u32,
    /// Viewport size in user units, used by filter percentage lengths.
    viewport: (f32, f32),
    pixels: Vec<Color>,
    clip: ClipStack,
    use_depth: usize,
    /// The ids of the `<use>` targets on the current expansion path. A repeat
    /// is a reference cycle and is skipped instead of recursing forever.
    use_stack: Vec<String>,
    mask_depth: usize,
    filter_depth: usize,
    opacity_depth: usize,
}

/// SVG's `fill-rule` values (SVG 2 §13.3). `nonzero` is the initial value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FillRule {
    NonZero,
    EvenOdd,
}

/// Add the horizontal overlap of `[left, right)` with each pixel cell to a
/// row's coverage buffer, scaled by one sub-scanline's weight.
fn accumulate_span(coverage: &mut [f32], left: f32, right: f32, weight: f32) {
    if !left.is_finite() || !right.is_finite() || coverage.is_empty() {
        return;
    }
    let width = coverage.len() as f32;
    let left = left.max(0.0);
    let right = right.min(width);
    if right <= left {
        return;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let first = left.floor() as usize;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let end = (right.ceil() as usize).min(coverage.len());
    for (offset, covered) in coverage[first..end].iter_mut().enumerate() {
        let pixel = (first + offset) as f32;
        let overlap = right.min(pixel + 1.0) - left.max(pixel);
        if overlap > 0.0 {
            *covered += overlap * weight;
        }
    }
}

/// Shortest distance from a point to a line segment.
fn segment_distance(x: f32, y: f32, from: (f32, f32), to: (f32, f32)) -> f32 {
    let (x1, y1) = from;
    let (x2, y2) = to;
    let dx = x2 - x1;
    let dy = y2 - y1;
    let length_squared = dx.mul_add(dx, dy * dy);
    if length_squared <= f32::EPSILON {
        return (x - x1).hypot(y - y1);
    }
    let t = ((x - x1).mul_add(dx, (y - y1) * dy) / length_squared).clamp(0.0, 1.0);
    (x - (x1 + t * dx)).hypot(y - (y1 + t * dy))
}

/// Vertical samples per device pixel row used by the scanline filler.
///
/// Four sub-scanlines plus exact horizontal span overlap give the 24-48px
/// icon sizes intermediate edge coverage without turning every SVG into a
/// supersampled bitmap. Larger SVG artwork is scaled smoothly again when the
/// raster is drawn, so this is a quality/cost point rather than a resolution
/// claim.
const SUBSCANLINES: usize = 4;

impl Raster {
    fn fill_polygon(
        &mut self,
        points: &[(f32, f32)],
        opacity: f32,
        color_at: impl Fn(f32, f32) -> Color,
    ) {
        self.fill_subpaths(&[points.to_vec()], FillRule::NonZero, opacity, color_at);
    }

    /// Fill every subpath of one SVG path with one fill rule.
    ///
    /// SVG paints all of a `<path>`'s subpaths as one shape: under `evenodd`
    /// an inner subpath cuts a hole, and under `nonzero` overlapping windings
    /// fill. Filling each subpath independently instead paints the hole with
    /// the fill colour, which is the visible defect on multi-subpath icons
    /// (Zhihu's WeChat/QQ/Weibo marks).
    fn fill_subpaths(
        &mut self,
        subpaths: &[Vec<(f32, f32)>],
        fill_rule: FillRule,
        opacity: f32,
        color_at: impl Fn(f32, f32) -> Color,
    ) {
        let opacity = opacity.clamp(0.0, 1.0);
        if opacity <= 0.0 {
            return;
        }
        let edges: Vec<&[(f32, f32)]> = subpaths
            .iter()
            .map(Vec::as_slice)
            .filter(|points| points.len() >= 2)
            .collect();
        if edges.is_empty() {
            return;
        }
        let min_y = edges
            .iter()
            .flat_map(|points| points.iter().map(|point| point.1))
            .fold(f32::MAX, f32::min);
        let max_y = edges
            .iter()
            .flat_map(|points| points.iter().map(|point| point.1))
            .fold(f32::MIN, f32::max);
        let first_row = (min_y.floor().max(0.0)) as i64;
        let last_row = (max_y.ceil().min(f32::from(u16::MAX))) as i64;
        let subscanline_weight = 1.0 / SUBSCANLINES as f32;
        let mut coverage = vec![0.0_f32; self.width as usize];
        for row in first_row..=last_row {
            coverage.fill(0.0);
            for subscanline in 0..SUBSCANLINES {
                let sample = row as f32 + (subscanline as f32 + 0.5) * subscanline_weight;
                let mut crossings: Vec<(f32, i32)> = Vec::new();
                for points in &edges {
                    for index in 0..points.len() {
                        let (x1, y1) = points[index];
                        let (x2, y2) = points[(index + 1) % points.len()];
                        if (y1 <= sample && y2 > sample) || (y2 <= sample && y1 > sample) {
                            let t = (sample - y1) / (y2 - y1);
                            let direction = if y1 <= sample && y2 > sample { 1 } else { -1 };
                            crossings.push((x1 + t * (x2 - x1), direction));
                        }
                    }
                }
                crossings.sort_by(|left, right| {
                    left.0
                        .partial_cmp(&right.0)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                match fill_rule {
                    FillRule::EvenOdd => {
                        for pair in crossings.chunks(2) {
                            if let [(left, _), (right, _)] = pair {
                                accumulate_span(&mut coverage, *left, *right, subscanline_weight);
                            }
                        }
                    }
                    FillRule::NonZero => {
                        let mut winding = 0_i32;
                        let mut span_start = None;
                        for (x, direction) in crossings {
                            let previous = winding;
                            winding += direction;
                            if previous == 0 && winding != 0 {
                                span_start = Some(x);
                            } else if previous != 0
                                && winding == 0
                                && let Some(start) = span_start.take()
                            {
                                accumulate_span(&mut coverage, start, x, subscanline_weight);
                            }
                        }
                    }
                }
            }
            let sample_y = row as f32 + 0.5;
            for (x, covered) in coverage.iter().enumerate() {
                if *covered <= 0.0 {
                    continue;
                }
                let color = color_at(x as f32 + 0.5, sample_y);
                self.put(
                    i64::try_from(x).unwrap_or(i64::MAX),
                    row,
                    color.with_opacity((covered * opacity).min(1.0)),
                );
            }
        }
    }

    /// Paint a stroke as the union of capsules around its segments.
    ///
    /// Distance-to-segment coverage gives the same intermediate edge pixels a
    /// filled shape gets. Looking at all segments before writing avoids the
    /// double-darkened joint a per-segment draw would produce for a
    /// translucent stroke.
    fn stroke_segments(
        &mut self,
        segments: &[Segment],
        radius: f32,
        opacity: f32,
        color_at: impl Fn(f32, f32) -> Color,
    ) {
        let opacity = opacity.clamp(0.0, 1.0);
        if segments.is_empty() || !radius.is_finite() || radius <= 0.0 || opacity <= 0.0 {
            return;
        }
        let radius = radius.min(self.width as f32 + self.height as f32);
        let mut min_x = f32::MAX;
        let mut min_y = f32::MAX;
        let mut max_x = f32::MIN;
        let mut max_y = f32::MIN;
        for ((x1, y1), (x2, y2)) in segments {
            min_x = min_x.min(*x1).min(*x2);
            min_y = min_y.min(*y1).min(*y2);
            max_x = max_x.max(*x1).max(*x2);
            max_y = max_y.max(*y1).max(*y2);
        }
        if !min_x.is_finite() || !min_y.is_finite() || !max_x.is_finite() || !max_y.is_finite() {
            return;
        }
        let first_x = (min_x - radius - 1.0).floor().max(0.0) as i64;
        let first_y = (min_y - radius - 1.0).floor().max(0.0) as i64;
        let last_x =
            ((max_x + radius + 1.0).ceil() as i64).min(i64::from(self.width.saturating_sub(1)));
        let last_y =
            ((max_y + radius + 1.0).ceil() as i64).min(i64::from(self.height.saturating_sub(1)));
        for y in first_y..=last_y {
            for x in first_x..=last_x {
                let sample_x = x as f32 + 0.5;
                let sample_y = y as f32 + 0.5;
                let distance = segments
                    .iter()
                    .map(|(from, to)| segment_distance(sample_x, sample_y, *from, *to))
                    .fold(f32::MAX, f32::min);
                let coverage = (radius + 0.5 - distance).clamp(0.0, 1.0);
                if coverage <= 0.0 {
                    continue;
                }
                self.put(
                    x,
                    y,
                    color_at(sample_x, sample_y).with_opacity(coverage * opacity),
                );
            }
        }
    }

    fn put(&mut self, x: i64, y: i64, color: Color) {
        if color.alpha == 0
            || x < 0
            || y < 0
            || x >= self.width.into()
            || y >= self.height.into()
            || !self.clip.contains(x as f32 + 0.5, y as f32 + 0.5)
        {
            return;
        }
        let index =
            usize::try_from(y).unwrap_or(0) * self.width as usize + usize::try_from(x).unwrap_or(0);
        let destination = self.pixels[index];
        if color.alpha == 255 {
            self.pixels[index] = color;
            return;
        }
        // Straight-alpha source-over. Gradients use stop-opacity for soft
        // edges and glows, so replacing the destination pixel would make the
        // translucent part of an icon opaque instead of blending it.
        let source_alpha = f32::from(color.alpha) / 255.0;
        let destination_alpha = f32::from(destination.alpha) / 255.0;
        let output_alpha = source_alpha + destination_alpha * (1.0 - source_alpha);
        if output_alpha <= f32::EPSILON {
            self.pixels[index] = Color::TRANSPARENT;
            return;
        }
        let blend = |source: u8, destination: u8| -> u8 {
            let source = f32::from(source);
            let destination = f32::from(destination);
            ((source * source_alpha + destination * destination_alpha * (1.0 - source_alpha))
                / output_alpha)
                .round()
                .clamp(0.0, 255.0) as u8
        };
        self.pixels[index] = Color::rgba(
            blend(color.red, destination.red),
            blend(color.green, destination.green),
            blend(color.blue, destination.blue),
            (output_alpha * 255.0).round().clamp(0.0, 255.0) as u8,
        );
    }
}

/// The paint a root `svg`'s own `fill` or `stroke` attribute gives the shapes
/// below it. An absent or unparseable value leaves `initial` in force.
fn root_paint(
    root: &XmlNode,
    name: &str,
    current_color: Color,
    initial: Paint,
    gradient_index: &BTreeMap<String, usize>,
) -> Paint {
    match property(root, name).map(|raw| parse_paint(raw.trim(), current_color, gradient_index)) {
        None | Some(Paint::Inherit) => initial,
        Some(paint) => paint,
    }
}

/// Resolve a `fill`/`stroke` value, keeping the inherited paint when the value
/// is explicitly or implicitly `inherit`. Treating a parsed `Inherit` as an
/// unresolved paint would make `fill="inherit"` paint nothing.
fn inherited_paint(
    raw: &str,
    inherited: Paint,
    current_color: Color,
    gradient_index: &BTreeMap<String, usize>,
) -> Paint {
    match parse_paint(raw, current_color, gradient_index) {
        Paint::Inherit => inherited,
        paint => paint,
    }
}

/// The maximum number of nested `<use>` expansions. A document may chain
/// unique references deeper than this, and a reference cycle never ends, so
/// the limit is the bounded fallback for both.
const USE_DEPTH_LIMIT: usize = 16;

fn push_diagnostic(
    diagnostics: &mut Vec<SvgRenderDiagnostic>,
    element: &str,
    code: SvgRenderDiagnosticCode,
    message: impl Into<String>,
) {
    diagnostics.push(SvgRenderDiagnostic {
        element: element.to_owned(),
        code,
        message: message.into(),
    });
}

/// Report a `<use>` problem. Nothing is painted for the use element, which is
/// the same fallback a browser gives an unresolved reference link; the rest of
/// the document still renders.
fn use_diagnostic(diagnostics: &mut Vec<SvgRenderDiagnostic>, message: impl Into<String>) {
    push_diagnostic(
        diagnostics,
        "use",
        SvgRenderDiagnosticCode::UnsupportedReference,
        message,
    );
}

fn unsupported_attribute_diagnostic(
    diagnostics: &mut Vec<SvgRenderDiagnostic>,
    element: &XmlNode,
    message: impl Into<String>,
) {
    push_diagnostic(
        diagnostics,
        &element.name,
        SvgRenderDiagnosticCode::UnsupportedAttribute,
        message,
    );
}

impl PreserveAspectRatio {
    /// Parse the two-token `preserveAspectRatio` grammar. An unparseable value
    /// is `None`; the caller reports it and uses the SVG default.
    fn parse(raw: &str) -> Option<Self> {
        let mut tokens = raw.split_whitespace();
        let mut align = tokens.next()?;
        if align.eq_ignore_ascii_case("defer") {
            align = tokens.next()?;
        }
        let (align_x, align_y) = match align {
            "none" => (ViewBoxAlign::None, ViewBoxAlign::None),
            "xMinYMin" => (ViewBoxAlign::Min, ViewBoxAlign::Min),
            "xMinYMid" => (ViewBoxAlign::Min, ViewBoxAlign::Mid),
            "xMinYMax" => (ViewBoxAlign::Min, ViewBoxAlign::Max),
            "xMidYMin" => (ViewBoxAlign::Mid, ViewBoxAlign::Min),
            "xMidYMid" => (ViewBoxAlign::Mid, ViewBoxAlign::Mid),
            "xMidYMax" => (ViewBoxAlign::Mid, ViewBoxAlign::Max),
            "xMaxYMin" => (ViewBoxAlign::Max, ViewBoxAlign::Min),
            "xMaxYMid" => (ViewBoxAlign::Max, ViewBoxAlign::Mid),
            "xMaxYMax" => (ViewBoxAlign::Max, ViewBoxAlign::Max),
            _ => return None,
        };
        let slice = match tokens.next() {
            None | Some("meet") => false,
            Some("slice") => true,
            Some(_) => return None,
        };
        if tokens.next().is_some() {
            return None;
        }
        Some(Self {
            align_x,
            align_y,
            slice,
        })
    }
}

/// The mapping from a viewBox's user space into a `width` x `height`
/// viewport, honouring `preserveAspectRatio`.
fn view_box_transform(
    view_box: (f32, f32, f32, f32),
    width: f32,
    height: f32,
    preserve: PreserveAspectRatio,
) -> Affine {
    let (min_x, min_y, view_width, view_height) = view_box;
    if view_width <= 0.0 || view_height <= 0.0 || width <= 0.0 || height <= 0.0 {
        return Affine::IDENTITY;
    }
    let scale_x = width / view_width;
    let scale_y = height / view_height;
    if preserve.align_x == ViewBoxAlign::None && preserve.align_y == ViewBoxAlign::None {
        return Affine {
            a: scale_x,
            b: 0.0,
            c: 0.0,
            d: scale_y,
            e: -min_x * scale_x,
            f: -min_y * scale_y,
        };
    }
    let scale = if preserve.slice {
        scale_x.max(scale_y)
    } else {
        scale_x.min(scale_y)
    };
    let offset_x = preserve.align_x.factor() * (width - view_width * scale);
    let offset_y = preserve.align_y.factor() * (height - view_height * scale);
    Affine {
        a: scale,
        b: 0.0,
        c: 0.0,
        d: scale,
        e: offset_x - min_x * scale,
        f: offset_y - min_y * scale,
    }
}

/// A `<use>` width/height/x/y length. Percentages resolve against the current
/// viewport axis; an unparseable value is reported and the caller falls back.
fn use_length(
    element: &XmlNode,
    name: &str,
    percentage_basis: f32,
    diagnostics: &mut Vec<SvgRenderDiagnostic>,
) -> Option<f32> {
    let raw = attribute(element, name)?;
    let value = parse_length_with_basis(raw, percentage_basis);
    if value.is_none() {
        unsupported_attribute_diagnostic(
            diagnostics,
            element,
            format!(
                "unsupported `<use>` {name} length `{}`; the attribute was ignored",
                raw.trim()
            ),
        );
    }
    value
}

/// The size the generated `<use>` viewport takes. The use element's own
/// `width`/`height` win; otherwise a `<symbol>`/`<svg>` target's own attributes
/// apply, and finally the viewBox extent, which is the useful size for an
/// unsized icon sprite.
fn use_viewport_size(
    use_element: &XmlNode,
    target: &XmlNode,
    name: &str,
    basis: f32,
    fallback: Option<f32>,
    diagnostics: &mut Vec<SvgRenderDiagnostic>,
) -> Option<f32> {
    use_length(use_element, name, basis, diagnostics)
        .or_else(|| attribute(target, name).and_then(|raw| parse_length_with_basis(raw, basis)))
        .or(fallback)
}

/// The transform and clip rectangle a `<use>` of a `<symbol>` or nested `<svg>`
/// establishes.
struct UseViewport {
    content: Affine,
    /// The viewport quad in device space, or `None` when the target has no
    /// resolvable size and therefore no viewport to clip to.
    clip: Option<[(f32, f32); 4]>,
}

/// Resolve a use element's generated viewport.
///
/// SVG 2 §"Use element": a `<use>` of `<symbol>` or `<svg>` generates a shadow
/// `<svg>` whose `x`/`y`/`width`/`height` come from the use element and whose
/// viewBox comes from the target. `preserveAspectRatio` maps the target's user
/// space into that viewport, and the viewport clips the result.
fn use_viewport(
    use_element: &XmlNode,
    target: &XmlNode,
    viewport_transform: Affine,
    basis: (f32, f32),
    diagnostics: &mut Vec<SvgRenderDiagnostic>,
) -> UseViewport {
    let view_box = root_view_box(target);
    let fallback = view_box.map(|(_, _, width, height)| (width, height));
    let width = use_viewport_size(
        use_element,
        target,
        "width",
        basis.0,
        fallback.map(|size| size.0),
        diagnostics,
    );
    let height = use_viewport_size(
        use_element,
        target,
        "height",
        basis.1,
        fallback.map(|size| size.1),
        diagnostics,
    );
    let (Some(width), Some(height)) = (width, height) else {
        return UseViewport {
            content: viewport_transform,
            clip: None,
        };
    };
    if width <= 0.0 || height <= 0.0 {
        return UseViewport {
            content: viewport_transform,
            clip: Some([(0.0, 0.0); 4]),
        };
    }
    let viewport = [
        viewport_transform.apply(0.0, 0.0),
        viewport_transform.apply(width, 0.0),
        viewport_transform.apply(width, height),
        viewport_transform.apply(0.0, height),
    ];
    let Some(view_box) = view_box else {
        return UseViewport {
            content: viewport_transform,
            clip: Some(viewport),
        };
    };
    let preserve = match attribute(target, "preserveAspectRatio") {
        None => PreserveAspectRatio::default(),
        Some(raw) => PreserveAspectRatio::parse(raw.trim()).unwrap_or_else(|| {
            unsupported_attribute_diagnostic(
                diagnostics,
                target,
                format!(
                    "unsupported preserveAspectRatio `{}`; used xMidYMid meet",
                    raw.trim()
                ),
            );
            PreserveAspectRatio::default()
        }),
    };
    UseViewport {
        content: viewport_transform.then(view_box_transform(view_box, width, height, preserve)),
        clip: Some(viewport),
    }
}

/// Clone the referenced node into the use shadow tree.
///
/// IDs in the clone are deliberately not added to the document id map: the
/// clone is a rendering tree, not a second DOM, and a nested `<use>` must keep
/// resolving against the original definitions.
fn use_shadow_content(target: &XmlNode) -> XmlNode {
    let mut content = target.clone();
    if matches!(target.name.as_str(), "symbol" | "svg") {
        // The generated shadow `<svg>` supplies the viewport and viewBox
        // mapping; the target keeps only the presentation and transform
        // attributes that apply inside that generated viewport.
        "g".clone_into(&mut content.name);
        content.attributes.retain(|(name, _)| {
            !matches!(
                name.as_str(),
                "viewBox" | "preserveAspectRatio" | "x" | "y" | "width" | "height"
            )
        });
    }
    content
}

/// Push an axis-aligned (or affine-mapped) viewport rectangle as a clip layer.
fn push_device_rect_clip(raster: &mut Raster, corners: [(f32, f32); 4]) {
    raster.clip.push(ClipLayer {
        polygons: vec![ClipPolygon {
            contours: vec![corners.to_vec()],
            fill_rule: ClipFillRule::NonZero,
        }],
    });
}

#[allow(
    clippy::too_many_lines,
    clippy::too_many_arguments,
    reason = "one shape-arm per SVG element reads clearest as a flat match; the gradient table is shared state"
)]
fn walk(
    element: &XmlNode,
    transform: Affine,
    fill: Paint,
    stroke: Paint,
    current_color: Color,
    fill_opacity: f32,
    stroke_opacity: f32,
    fill_rule: FillRule,
    stroke_width: f32,
    gradients: &[Gradient],
    gradient_index: &BTreeMap<String, usize>,
    clip_paths: &BTreeMap<String, &XmlNode>,
    mask_paths: &BTreeMap<String, &XmlNode>,
    filter_paths: &BTreeMap<String, &XmlNode>,
    ids: &BTreeMap<String, &XmlNode>,
    raster: &mut Raster,
    diagnostics: &mut Vec<SvgRenderDiagnostic>,
) {
    let clipped = push_element_clip(raster, element, transform, None, clip_paths);
    for child_node in &element.children {
        let presentation = |name: &str| {
            property(child_node, name)
                .map(str::trim)
                .filter(|value| !value.is_empty())
        };
        let attribute = |name: &str| {
            attribute(child_node, name)
                .map(str::trim)
                .filter(|value| !value.is_empty())
        };
        // `color` is inherited and is what `currentColor` resolves to for this
        // element and every descendant that does not set its own.
        let child_color = presentation("color")
            .and_then(parse_color)
            .unwrap_or(current_color);
        let child_fill = presentation("fill").map_or(fill, |raw| {
            inherited_paint(raw, fill, child_color, gradient_index)
        });
        let child_stroke = presentation("stroke").map_or(stroke, |raw| {
            inherited_paint(raw, stroke, child_color, gradient_index)
        });
        let child_fill_opacity = presentation("fill-opacity")
            .and_then(parse_opacity)
            .unwrap_or(fill_opacity);
        let child_stroke_opacity = presentation("stroke-opacity")
            .and_then(parse_opacity)
            .unwrap_or(stroke_opacity);
        let child_fill_rule = match presentation("fill-rule") {
            Some(value) if value.eq_ignore_ascii_case("evenodd") => FillRule::EvenOdd,
            Some(value) if value.eq_ignore_ascii_case("nonzero") => FillRule::NonZero,
            _ => fill_rule,
        };
        let child_stroke_width = presentation("stroke-width")
            .and_then(parse_length)
            .filter(|width| *width >= 0.0)
            .unwrap_or(stroke_width);
        let child_transform = match presentation("transform") {
            Some(specification) => transform.then(parse_transform(specification)),
            None => transform,
        };
        if presentation("display").is_some_and(|value| value.eq_ignore_ascii_case("none")) {
            continue;
        }
        if paint_opacity_group(
            raster,
            child_node,
            transform,
            child_fill,
            child_stroke,
            child_color,
            child_fill_opacity,
            child_stroke_opacity,
            child_fill_rule,
            child_stroke_width,
            gradients,
            gradient_index,
            clip_paths,
            mask_paths,
            filter_paths,
            ids,
            diagnostics,
        ) {
            continue;
        }
        if paint_masked_element(
            raster,
            child_node,
            transform,
            child_transform,
            child_fill,
            child_stroke,
            child_color,
            child_fill_opacity,
            child_stroke_opacity,
            child_fill_rule,
            child_stroke_width,
            gradients,
            gradient_index,
            clip_paths,
            mask_paths,
            filter_paths,
            ids,
            diagnostics,
        ) {
            continue;
        }
        if paint_filtered_element(
            raster,
            child_node,
            transform,
            child_transform,
            child_fill,
            child_stroke,
            child_color,
            child_fill_opacity,
            child_stroke_opacity,
            child_fill_rule,
            child_stroke_width,
            gradients,
            gradient_index,
            clip_paths,
            mask_paths,
            filter_paths,
            ids,
            diagnostics,
        ) {
            continue;
        }
        match child_node.name.as_str() {
            "defs" | "symbol" | "text" | "image" | "style" | "title" | "desc" | "metadata" => {}
            "rect" => {
                let Some(points) = rect_polygon(
                    attribute("x").and_then(parse_length),
                    attribute("y").and_then(parse_length),
                    attribute("width").and_then(parse_length),
                    attribute("height").and_then(parse_length),
                ) else {
                    continue;
                };
                let device = transform_points(&points, child_transform);
                let clipped = push_element_clip(
                    raster,
                    child_node,
                    child_transform,
                    points_bounds(&points),
                    clip_paths,
                );
                paint_shape(
                    raster,
                    &points,
                    &device,
                    child_transform,
                    child_fill.resolve(None),
                    child_fill_opacity,
                    gradients,
                );
                stroke_outline(
                    raster,
                    &points,
                    &device,
                    child_transform,
                    child_stroke.resolve(None),
                    child_stroke_width,
                    child_stroke_opacity,
                    gradients,
                );
                if clipped {
                    raster.clip.pop();
                }
            }
            "circle" | "ellipse" => {
                let cx = attribute("cx").and_then(parse_length).unwrap_or(0.0);
                let cy = attribute("cy").and_then(parse_length).unwrap_or(0.0);
                let (rx, ry) = if child_node.name == "circle" {
                    let r = attribute("r").and_then(parse_length).unwrap_or(0.0);
                    (r, r)
                } else {
                    (
                        attribute("rx").and_then(parse_length).unwrap_or(0.0),
                        attribute("ry").and_then(parse_length).unwrap_or(0.0),
                    )
                };
                if rx <= 0.0 || ry <= 0.0 {
                    continue;
                }
                let mut points = Vec::with_capacity(48);
                for step in 0..48 {
                    let angle = std::f32::consts::TAU * step as f32 / 48.0;
                    points.push((cx + rx * angle.cos(), cy + ry * angle.sin()));
                }
                let device = transform_points(&points, child_transform);
                let clipped = push_element_clip(
                    raster,
                    child_node,
                    child_transform,
                    points_bounds(&points),
                    clip_paths,
                );
                paint_shape(
                    raster,
                    &points,
                    &device,
                    child_transform,
                    child_fill.resolve(None),
                    child_fill_opacity,
                    gradients,
                );
                stroke_outline(
                    raster,
                    &points,
                    &device,
                    child_transform,
                    child_stroke.resolve(None),
                    child_stroke_width,
                    child_stroke_opacity,
                    gradients,
                );
                if clipped {
                    raster.clip.pop();
                }
            }
            "polygon" | "polyline" => {
                let Some(points) = attribute("points").and_then(parse_points) else {
                    continue;
                };
                if points.len() < 2 {
                    continue;
                }
                let device = transform_points(&points, child_transform);
                let clipped = push_element_clip(
                    raster,
                    child_node,
                    child_transform,
                    points_bounds(&points),
                    clip_paths,
                );
                paint_shape(
                    raster,
                    &points,
                    &device,
                    child_transform,
                    child_fill.resolve(None),
                    child_fill_opacity,
                    gradients,
                );
                let segments = polyline_segments(&device, child_node.name == "polygon");
                stroke_outline_segments(
                    raster,
                    &points,
                    &segments,
                    child_transform,
                    child_stroke.resolve(None),
                    child_stroke_width,
                    child_stroke_opacity,
                    gradients,
                );
                if clipped {
                    raster.clip.pop();
                }
            }
            "line" => {
                let (Some(x1), Some(y1), Some(x2), Some(y2)) = (
                    attribute("x1").and_then(parse_length),
                    attribute("y1").and_then(parse_length),
                    attribute("x2").and_then(parse_length),
                    attribute("y2").and_then(parse_length),
                ) else {
                    continue;
                };
                let from = child_transform.apply(x1, y1);
                let to = child_transform.apply(x2, y2);
                let local = [(x1, y1), (x2, y2)];
                let clipped = push_element_clip(
                    raster,
                    child_node,
                    child_transform,
                    points_bounds(&local),
                    clip_paths,
                );
                stroke_outline_segments(
                    raster,
                    &local,
                    &[(from, to)],
                    child_transform,
                    child_stroke.resolve(None),
                    child_stroke_width,
                    child_stroke_opacity,
                    gradients,
                );
                if clipped {
                    raster.clip.pop();
                }
            }
            "path" => {
                let Some(specification) = attribute("d") else {
                    continue;
                };
                let subpaths = parse_path(specification);
                let all_points: Vec<(f32, f32)> = subpaths
                    .iter()
                    .flat_map(|(subpath, _)| subpath.iter().copied())
                    .collect();
                let clipped = push_element_clip(
                    raster,
                    child_node,
                    child_transform,
                    points_bounds(&all_points),
                    clip_paths,
                );
                // All subpaths of one `<path>` are one shape. Painting them
                // independently fills holes such as the counters in an icon.
                let fill_rule = child_fill_rule;
                let local_subpaths: Vec<Vec<(f32, f32)>> = subpaths
                    .iter()
                    .filter(|(subpath, _)| subpath.len() >= 2)
                    .map(|(subpath, _)| subpath.clone())
                    .collect();
                let device_subpaths: Vec<Vec<(f32, f32)>> = local_subpaths
                    .iter()
                    .map(|subpath| transform_points(subpath, child_transform))
                    .collect();
                paint_subpaths(
                    raster,
                    &local_subpaths,
                    &device_subpaths,
                    child_transform,
                    child_fill.resolve(None),
                    fill_rule,
                    child_fill_opacity,
                    gradients,
                );
                for (subpath, closed) in &subpaths {
                    if subpath.len() < 2 {
                        continue;
                    }
                    let device = transform_points(subpath, child_transform);
                    let segments = polyline_segments(&device, *closed);
                    stroke_outline_segments(
                        raster,
                        subpath,
                        &segments,
                        child_transform,
                        child_stroke.resolve(None),
                        child_stroke_width,
                        child_stroke_opacity,
                        gradients,
                    );
                }
                if clipped {
                    raster.clip.pop();
                }
            }
            "use" => {
                let Some(reference) = attribute("href").or_else(|| attribute("xlink:href")) else {
                    use_diagnostic(
                        diagnostics,
                        "`<use>` has no href or xlink:href; nothing is rendered",
                    );
                    continue;
                };
                let reference = reference.trim();
                let Some(id) = reference.strip_prefix('#') else {
                    use_diagnostic(
                        diagnostics,
                        format!(
                            "external-document `<use>` reference `{reference}` is not supported; nothing is rendered"
                        ),
                    );
                    continue;
                };
                let id = id.trim();
                if id.is_empty() {
                    use_diagnostic(
                        diagnostics,
                        "`<use>` has an empty fragment reference; nothing is rendered",
                    );
                    continue;
                }
                let Some(target) = ids.get(id) else {
                    use_diagnostic(
                        diagnostics,
                        format!(
                            "`<use>` reference `#{id}` does not resolve to an element in this document; nothing is rendered"
                        ),
                    );
                    continue;
                };
                if raster.use_stack.iter().any(|ancestor| ancestor == id) {
                    use_diagnostic(
                        diagnostics,
                        format!(
                            "`<use>` reference cycle through `#{id}`; the recursive expansion was stopped"
                        ),
                    );
                    continue;
                }
                if raster.use_depth >= USE_DEPTH_LIMIT {
                    use_diagnostic(
                        diagnostics,
                        format!(
                            "`<use>` nesting is deeper than the {USE_DEPTH_LIMIT}-level limit at `#{id}`; the rest of the expansion was stopped"
                        ),
                    );
                    continue;
                }
                let x = use_length(child_node, "x", raster.viewport.0, diagnostics).unwrap_or(0.0);
                let y = use_length(child_node, "y", raster.viewport.1, diagnostics).unwrap_or(0.0);
                let viewport_transform = child_transform.then(Affine {
                    a: 1.0,
                    b: 0.0,
                    c: 0.0,
                    d: 1.0,
                    e: x,
                    f: y,
                });
                let (content_transform, viewport_clip) =
                    if matches!(target.name.as_str(), "symbol" | "svg") {
                        let viewport = use_viewport(
                            child_node,
                            target,
                            viewport_transform,
                            raster.viewport,
                            diagnostics,
                        );
                        (viewport.content, viewport.clip)
                    } else {
                        (viewport_transform, None)
                    };

                let mut clip_layers = 0;
                if push_element_clip(raster, child_node, viewport_transform, None, clip_paths) {
                    clip_layers += 1;
                }
                if let Some(corners) = viewport_clip {
                    push_device_rect_clip(raster, corners);
                    clip_layers += 1;
                }

                // Rendering goes through a synthetic wrapper so the target is
                // processed as a child: its own fill/stroke, transform,
                // opacity, clip-path, mask and filter then apply exactly as
                // they would if the target had been written at this position.
                let shadow = XmlNode {
                    name: "use-shadow".to_owned(),
                    attributes: Vec::new(),
                    children: vec![use_shadow_content(target)],
                };
                raster.use_depth += 1;
                raster.use_stack.push(id.to_owned());
                walk(
                    &shadow,
                    content_transform,
                    child_fill,
                    child_stroke,
                    child_color,
                    child_fill_opacity,
                    child_stroke_opacity,
                    child_fill_rule,
                    child_stroke_width,
                    gradients,
                    gradient_index,
                    clip_paths,
                    mask_paths,
                    filter_paths,
                    ids,
                    raster,
                    diagnostics,
                );
                raster.use_stack.pop();
                raster.use_depth -= 1;
                for _ in 0..clip_layers {
                    raster.clip.pop();
                }
            }
            "svg" | "g" | "a" | "switch" => {
                if presentation("display").is_some_and(|value| value.eq_ignore_ascii_case("none")) {
                    continue;
                }
                walk(
                    child_node,
                    child_transform,
                    child_fill,
                    child_stroke,
                    child_color,
                    child_fill_opacity,
                    child_stroke_opacity,
                    child_fill_rule,
                    child_stroke_width,
                    gradients,
                    gradient_index,
                    clip_paths,
                    mask_paths,
                    filter_paths,
                    ids,
                    raster,
                    diagnostics,
                );
            }
            _ => {
                // Unknown container: descend, it may hold shapes.
                walk(
                    child_node,
                    child_transform,
                    child_fill,
                    child_stroke,
                    child_color,
                    child_fill_opacity,
                    child_stroke_opacity,
                    child_fill_rule,
                    child_stroke_width,
                    gradients,
                    gradient_index,
                    clip_paths,
                    mask_paths,
                    filter_paths,
                    ids,
                    raster,
                    diagnostics,
                );
            }
        }
    }
    if clipped {
        raster.clip.pop();
    }
}

/// Render one element with `opacity < 1` into an offscreen buffer and
/// composite the result once, so overlapping children inside a group do not
/// reveal each other through double alpha.
#[allow(clippy::too_many_arguments)]
fn paint_opacity_group(
    raster: &mut Raster,
    element: &XmlNode,
    parent_transform: Affine,
    fill: Paint,
    stroke: Paint,
    current_color: Color,
    fill_opacity: f32,
    stroke_opacity: f32,
    fill_rule: FillRule,
    stroke_width: f32,
    gradients: &[Gradient],
    gradient_index: &BTreeMap<String, usize>,
    clip_paths: &BTreeMap<String, &XmlNode>,
    mask_paths: &BTreeMap<String, &XmlNode>,
    filter_paths: &BTreeMap<String, &XmlNode>,
    ids: &BTreeMap<String, &XmlNode>,
    diagnostics: &mut Vec<SvgRenderDiagnostic>,
) -> bool {
    let Some(opacity) = element_opacity(element) else {
        return false;
    };
    if opacity >= 1.0 {
        return false;
    }
    if raster.opacity_depth >= 8 {
        return false;
    }
    if opacity <= 0.0 {
        return true;
    }
    let mut content = Raster {
        width: raster.width,
        height: raster.height,
        viewport: raster.viewport,
        pixels: vec![Color::TRANSPARENT; raster.width as usize * raster.height as usize],
        clip: raster.clip.clone(),
        use_depth: raster.use_depth,
        use_stack: raster.use_stack.clone(),
        mask_depth: raster.mask_depth,
        filter_depth: raster.filter_depth,
        opacity_depth: raster.opacity_depth + 1,
    };
    let mut clone = element.clone();
    clone.attributes.retain(|(name, _)| name != "opacity");
    if let Some((_, style)) = clone
        .attributes
        .iter_mut()
        .find(|(name, _)| name == "style")
    {
        *style = strip_opacity_from_style(style);
    }
    let shadow = XmlNode {
        name: "opacity-content".to_owned(),
        attributes: Vec::new(),
        children: vec![clone],
    };
    walk(
        &shadow,
        parent_transform,
        fill,
        stroke,
        current_color,
        fill_opacity,
        stroke_opacity,
        fill_rule,
        stroke_width,
        gradients,
        gradient_index,
        clip_paths,
        mask_paths,
        filter_paths,
        ids,
        &mut content,
        diagnostics,
    );
    for y in 0..raster.height {
        for x in 0..raster.width {
            let index = y as usize * raster.width as usize + x as usize;
            let pixel = content.pixels[index];
            if pixel.alpha > 0 {
                raster.put(i64::from(x), i64::from(y), pixel.with_opacity(opacity));
            }
        }
    }
    true
}

fn element_opacity(element: &XmlNode) -> Option<f32> {
    if let Some(style) = attribute(element, "style") {
        for declaration in style.split(';') {
            let Some((name, value)) = declaration.split_once(':') else {
                continue;
            };
            if name.trim().eq_ignore_ascii_case("opacity") {
                return parse_group_opacity(value);
            }
        }
    }
    attribute(element, "opacity").and_then(parse_group_opacity)
}

fn parse_group_opacity(raw: &str) -> Option<f32> {
    let value = raw.trim();
    let parsed = value.strip_suffix('%').map_or_else(
        || value.parse::<f32>().ok(),
        |number| number.parse::<f32>().ok().map(|value| value / 100.0),
    )?;
    parsed.is_finite().then_some(parsed.clamp(0.0, 1.0))
}

fn strip_opacity_from_style(style: &str) -> String {
    style
        .split(';')
        .filter(|declaration| {
            declaration
                .split_once(':')
                .is_none_or(|(name, _)| !name.trim().eq_ignore_ascii_case("opacity"))
        })
        .collect::<Vec<_>>()
        .join(";")
}

/// Render one element through an SVG mask.
///
/// The element is painted into an offscreen buffer with its `mask` attribute
/// removed, then the mask definition is painted into a second buffer. The
/// resulting coverage is applied per pixel when the offscreen content is
/// composited into `raster`, so masks work for shapes, groups and `<use>`.
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "one masked element needs content, mask and region passes in one place"
)]
fn paint_masked_element(
    raster: &mut Raster,
    element: &XmlNode,
    parent_transform: Affine,
    element_transform: Affine,
    fill: Paint,
    stroke: Paint,
    current_color: Color,
    fill_opacity: f32,
    stroke_opacity: f32,
    fill_rule: FillRule,
    stroke_width: f32,
    gradients: &[Gradient],
    gradient_index: &BTreeMap<String, usize>,
    clip_paths: &BTreeMap<String, &XmlNode>,
    mask_paths: &BTreeMap<String, &XmlNode>,
    filter_paths: &BTreeMap<String, &XmlNode>,
    ids: &BTreeMap<String, &XmlNode>,
    diagnostics: &mut Vec<SvgRenderDiagnostic>,
) -> bool {
    let Some(reference) = property(element, "mask") else {
        return false;
    };
    let Some(id) = reference_id(reference) else {
        return false;
    };
    let Some(mask) = mask_paths.get(id) else {
        return false;
    };
    if raster.mask_depth >= 8 {
        return false;
    }
    let Some(bounds) = element_bounds(element) else {
        return true;
    };

    let mut content = Raster {
        width: raster.width,
        height: raster.height,
        viewport: raster.viewport,
        pixels: vec![Color::TRANSPARENT; raster.width as usize * raster.height as usize],
        clip: raster.clip.clone(),
        use_depth: raster.use_depth,
        use_stack: raster.use_stack.clone(),
        mask_depth: raster.mask_depth,
        filter_depth: raster.filter_depth,
        opacity_depth: raster.opacity_depth,
    };
    let mut clone = element.clone();
    clone.attributes.retain(|(name, _)| name != "mask");
    let shadow = XmlNode {
        name: "masked-content".to_owned(),
        attributes: Vec::new(),
        children: vec![clone],
    };
    walk(
        &shadow,
        parent_transform,
        fill,
        stroke,
        current_color,
        fill_opacity,
        stroke_opacity,
        fill_rule,
        stroke_width,
        gradients,
        gradient_index,
        clip_paths,
        mask_paths,
        filter_paths,
        ids,
        &mut content,
        diagnostics,
    );

    let object_content = attribute(mask, "maskContentUnits")
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("objectBoundingBox"));
    let mut mask_transform = element_transform;
    if object_content {
        mask_transform = mask_transform.then(Affine {
            a: bounds.width,
            b: 0.0,
            c: 0.0,
            d: bounds.height,
            e: bounds.x,
            f: bounds.y,
        });
    }
    if let Some(specification) = attribute(mask, "transform") {
        mask_transform = mask_transform.then(parse_transform(specification));
    }
    let mut mask_raster = Raster {
        width: raster.width,
        height: raster.height,
        viewport: raster.viewport,
        pixels: vec![Color::TRANSPARENT; raster.width as usize * raster.height as usize],
        clip: ClipStack::default(),
        use_depth: 0,
        use_stack: raster.use_stack.clone(),
        mask_depth: raster.mask_depth + 1,
        filter_depth: raster.filter_depth,
        opacity_depth: raster.opacity_depth,
    };
    walk(
        mask,
        mask_transform,
        Paint::Color(Color::BLACK),
        Paint::None,
        current_color,
        1.0,
        1.0,
        FillRule::NonZero,
        1.0,
        gradients,
        gradient_index,
        clip_paths,
        mask_paths,
        filter_paths,
        ids,
        &mut mask_raster,
        diagnostics,
    );

    let object_units = !attribute(mask, "maskUnits")
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("userSpaceOnUse"));
    let (x, y, width, height) = if object_units {
        (
            attribute(mask, "x")
                .and_then(parse_fraction)
                .unwrap_or(-0.1),
            attribute(mask, "y")
                .and_then(parse_fraction)
                .unwrap_or(-0.1),
            attribute(mask, "width")
                .and_then(parse_fraction)
                .unwrap_or(1.2),
            attribute(mask, "height")
                .and_then(parse_fraction)
                .unwrap_or(1.2),
        )
    } else {
        (
            attribute(mask, "x")
                .and_then(parse_length)
                .unwrap_or(bounds.x - 0.1 * bounds.width),
            attribute(mask, "y")
                .and_then(parse_length)
                .unwrap_or(bounds.y - 0.1 * bounds.height),
            attribute(mask, "width")
                .and_then(parse_length)
                .unwrap_or(1.2 * bounds.width),
            attribute(mask, "height")
                .and_then(parse_length)
                .unwrap_or(1.2 * bounds.height),
        )
    };
    if width <= 0.0 || height <= 0.0 {
        return true;
    }
    let (region_x, region_y, region_width, region_height) = if object_units {
        (
            bounds.x + x * bounds.width,
            bounds.y + y * bounds.height,
            width * bounds.width,
            height * bounds.height,
        )
    } else {
        (x, y, width, height)
    };
    let region = [
        (region_x, region_y),
        (region_x + region_width, region_y),
        (region_x + region_width, region_y + region_height),
        (region_x, region_y + region_height),
    ]
    .map(|(x, y)| element_transform.apply(x, y));
    let region_contours = [region.to_vec()];
    let mask_is_alpha = attribute(mask, "mask-type")
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("alpha"))
        || attribute(mask, "style").is_some_and(|style| {
            style
                .to_ascii_lowercase()
                .replace(' ', "")
                .contains("mask-type:alpha")
        });

    for y in 0..raster.height {
        for x in 0..raster.width {
            if !point_in_contours(
                &region_contours,
                ClipFillRule::NonZero,
                x as f32 + 0.5,
                y as f32 + 0.5,
            ) {
                continue;
            }
            let index = y as usize * raster.width as usize + x as usize;
            let source = content.pixels[index];
            if source.alpha == 0 {
                continue;
            }
            let mask_pixel = mask_raster.pixels[index];
            let mask_alpha = f32::from(mask_pixel.alpha) / 255.0;
            let coverage = if mask_is_alpha {
                mask_alpha
            } else {
                let luminance = 0.2126 * f32::from(mask_pixel.red)
                    + 0.7152 * f32::from(mask_pixel.green)
                    + 0.0722 * f32::from(mask_pixel.blue);
                (luminance / 255.0) * mask_alpha
            };
            if coverage <= 0.0 {
                continue;
            }
            raster.put(i64::from(x), i64::from(y), source.with_opacity(coverage));
        }
    }
    true
}

/// A filter region in the element's user space and in device pixels.
struct FilterRegion {
    local: Bounds,
    polygon: Vec<(f32, f32)>,
}

/// Render one element through an SVG filter.
///
/// Like masks, the element is painted into an offscreen buffer with its
/// `filter` attribute removed. The filter primitives then transform that
/// buffer, and the result is composited through the filter region.
///
/// Supported primitives are `feGaussianBlur`, `feColorMatrix`, `feOffset`,
/// `feBlend`, `feComposite`, `feFlood`, `feMerge`, `feDropShadow` and
/// `feMorphology`; a chain is processed in document order, with `result`
/// names and `in`/`in2` references. `filterUnits`, `primitiveUnits` and
/// percentage lengths are resolved against the object bounding box or the
/// viewport; each primitive's `x`/`y`/`width`/`height` subregion clips its
/// output. Features outside that subset are reported as
/// [`SvgRenderDiagnostic`]s and use a visible fallback instead of being
/// silently dropped.
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "one filtered element needs content, primitive chain and region passes"
)]
fn paint_filtered_element(
    raster: &mut Raster,
    element: &XmlNode,
    parent_transform: Affine,
    element_transform: Affine,
    fill: Paint,
    stroke: Paint,
    current_color: Color,
    fill_opacity: f32,
    stroke_opacity: f32,
    fill_rule: FillRule,
    stroke_width: f32,
    gradients: &[Gradient],
    gradient_index: &BTreeMap<String, usize>,
    clip_paths: &BTreeMap<String, &XmlNode>,
    mask_paths: &BTreeMap<String, &XmlNode>,
    filter_paths: &BTreeMap<String, &XmlNode>,
    ids: &BTreeMap<String, &XmlNode>,
    diagnostics: &mut Vec<SvgRenderDiagnostic>,
) -> bool {
    let Some(reference) = property(element, "filter") else {
        return false;
    };
    let Some(id) = reference_id(reference) else {
        return false;
    };
    let Some(filter) = filter_paths.get(id) else {
        return false;
    };
    if raster.filter_depth >= 8 {
        return false;
    }
    let Some(bounds) = element_bounds(element) else {
        return true;
    };

    let mut content = Raster {
        width: raster.width,
        height: raster.height,
        viewport: raster.viewport,
        pixels: vec![Color::TRANSPARENT; raster.width as usize * raster.height as usize],
        clip: raster.clip.clone(),
        use_depth: raster.use_depth,
        use_stack: raster.use_stack.clone(),
        mask_depth: raster.mask_depth,
        filter_depth: raster.filter_depth + 1,
        opacity_depth: raster.opacity_depth,
    };
    let mut clone = element.clone();
    clone.attributes.retain(|(name, _)| name != "filter");
    let shadow = XmlNode {
        name: "filtered-content".to_owned(),
        attributes: Vec::new(),
        children: vec![clone],
    };
    walk(
        &shadow,
        parent_transform,
        fill,
        stroke,
        current_color,
        fill_opacity,
        stroke_opacity,
        fill_rule,
        stroke_width,
        gradients,
        gradient_index,
        clip_paths,
        mask_paths,
        filter_paths,
        ids,
        &mut content,
        diagnostics,
    );

    let Some(region) = filter_region(
        filter,
        bounds,
        raster.viewport,
        element_transform,
        diagnostics,
    ) else {
        return true;
    };
    let source = content.pixels;
    let source_alpha: Vec<Color> = source
        .iter()
        .map(|pixel| Color::rgba(0, 0, 0, pixel.alpha))
        .collect();
    let mut results: BTreeMap<String, Vec<Color>> = BTreeMap::new();
    let mut pixels = source.clone();
    let object_primitives = filter_units(filter, "primitiveUnits", false, diagnostics);
    let scale_x = element_transform.a.hypot(element_transform.b).max(0.0001);
    let scale_y = element_transform.c.hypot(element_transform.d).max(0.0001);
    for primitive in &filter.children {
        if matches!(primitive.name.as_str(), "title" | "desc" | "metadata") {
            continue;
        }
        let input = filter_input(
            primitive,
            "in",
            &source,
            &source_alpha,
            &results,
            &pixels,
            diagnostics,
        );
        let input2 = filter_input(
            primitive,
            "in2",
            &source,
            &source_alpha,
            &results,
            &pixels,
            diagnostics,
        );
        let mut output = match primitive.name.as_str() {
            "feGaussianBlur" => {
                let (sigma_x, sigma_y) =
                    parse_number_pair(attribute(primitive, "stdDeviation").unwrap_or("0"));
                if sigma_x < 0.0 || sigma_y < 0.0 {
                    filter_diagnostic(
                        diagnostics,
                        primitive,
                        "negative stdDeviation is invalid; the primitive was disabled",
                    );
                }
                let sigma_x = sigma_x.max(0.0);
                let sigma_y = sigma_y.max(0.0);
                let (sigma_x, sigma_y) = if object_primitives {
                    (sigma_x * bounds.width, sigma_y * bounds.height)
                } else {
                    (sigma_x, sigma_y)
                };
                blur_pixels(
                    &input,
                    raster.width,
                    raster.height,
                    sigma_x * scale_x,
                    sigma_y * scale_y,
                )
            }
            "feColorMatrix" => {
                let matrix = parse_color_matrix(primitive).unwrap_or_else(|message| {
                    filter_diagnostic(diagnostics, primitive, message);
                    [
                        1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0,
                        0.0, 0.0, 0.0, 1.0, 0.0,
                    ]
                });
                let mut output = input;
                for pixel in &mut output {
                    *pixel = apply_color_matrix(*pixel, &matrix);
                }
                output
            }
            "feOffset" => {
                let dx = primitive_length(
                    primitive,
                    "dx",
                    object_primitives,
                    bounds,
                    raster.viewport.0,
                    0.0,
                    diagnostics,
                );
                let dy = primitive_length(
                    primitive,
                    "dy",
                    object_primitives,
                    bounds,
                    raster.viewport.1,
                    0.0,
                    diagnostics,
                );
                let translated_x = element_transform.a * dx + element_transform.c * dy;
                let translated_y = element_transform.b * dx + element_transform.d * dy;
                offset_pixels(
                    &input,
                    raster.width,
                    raster.height,
                    translated_x,
                    translated_y,
                )
            }
            "feBlend" => {
                let mode = attribute(primitive, "mode").unwrap_or("normal").trim();
                if !supported_blend_mode(mode) {
                    filter_diagnostic(
                        diagnostics,
                        primitive,
                        format!("unsupported blend mode `{mode}`; used `normal`"),
                    );
                }
                blend_pixels(&input2, &input, mode)
            }
            "feComposite" => {
                let operator = attribute(primitive, "operator")
                    .unwrap_or("over")
                    .trim()
                    .to_ascii_lowercase();
                if !matches!(
                    operator.as_str(),
                    "over" | "in" | "out" | "atop" | "xor" | "arithmetic"
                ) {
                    filter_diagnostic(
                        diagnostics,
                        primitive,
                        format!("unsupported composite operator `{operator}`; used `over`"),
                    );
                }
                let coefficients = ["k1", "k2", "k3", "k4"].map(|name| {
                    attribute(primitive, name)
                        .and_then(|raw| raw.trim().parse::<f32>().ok())
                        .unwrap_or(0.0)
                });
                composite_pixels(&input, &input2, &operator, coefficients)
            }
            "feDropShadow" => {
                let dx = primitive_length(
                    primitive,
                    "dx",
                    object_primitives,
                    bounds,
                    raster.viewport.0,
                    2.0,
                    diagnostics,
                );
                let dy = primitive_length(
                    primitive,
                    "dy",
                    object_primitives,
                    bounds,
                    raster.viewport.1,
                    2.0,
                    diagnostics,
                );
                let (sigma_x, sigma_y) =
                    parse_number_pair(attribute(primitive, "stdDeviation").unwrap_or("2"));
                if sigma_x < 0.0 || sigma_y < 0.0 {
                    filter_diagnostic(
                        diagnostics,
                        primitive,
                        "negative stdDeviation is invalid; the primitive was disabled",
                    );
                }
                let sigma_x = sigma_x.max(0.0);
                let sigma_y = sigma_y.max(0.0);
                let (sigma_x, sigma_y) = if object_primitives {
                    (sigma_x * bounds.width, sigma_y * bounds.height)
                } else {
                    (sigma_x, sigma_y)
                };
                let color = attribute(primitive, "flood-color")
                    .and_then(parse_color)
                    .unwrap_or(Color::BLACK);
                let opacity = attribute(primitive, "flood-opacity")
                    .and_then(parse_opacity)
                    .unwrap_or(1.0);
                drop_shadow_pixels(
                    &input,
                    (raster.width, raster.height),
                    (
                        element_transform.a * dx + element_transform.c * dy,
                        element_transform.b * dx + element_transform.d * dy,
                    ),
                    (sigma_x * scale_x, sigma_y * scale_y),
                    color.with_opacity(opacity),
                )
            }
            "feFlood" => {
                let color = attribute(primitive, "flood-color")
                    .and_then(parse_color)
                    .unwrap_or(Color::BLACK);
                let opacity = attribute(primitive, "flood-opacity")
                    .and_then(parse_opacity)
                    .unwrap_or(1.0);
                vec![color.with_opacity(opacity); input.len()]
            }
            "feMerge" => {
                let mut merged = vec![Color::TRANSPARENT; input.len()];
                for node in &primitive.children {
                    if node.name != "feMergeNode" {
                        continue;
                    }
                    let layer = filter_input(
                        node,
                        "in",
                        &source,
                        &source_alpha,
                        &results,
                        &pixels,
                        diagnostics,
                    );
                    merged = composite_pixels(&layer, &merged, "over", [0.0; 4]);
                }
                merged
            }
            "feMorphology" => {
                let (radius_x, radius_y) =
                    parse_number_pair(attribute(primitive, "radius").unwrap_or("0"));
                if radius_x < 0.0 || radius_y < 0.0 {
                    filter_diagnostic(
                        diagnostics,
                        primitive,
                        "negative morphology radius is invalid; the primitive was disabled",
                    );
                }
                let radius_x = radius_x.max(0.0);
                let radius_y = radius_y.max(0.0);
                let (radius_x, radius_y) = if object_primitives {
                    (radius_x * bounds.width, radius_y * bounds.height)
                } else {
                    (radius_x, radius_y)
                };
                let operator = attribute(primitive, "operator").unwrap_or("erode").trim();
                if !operator.eq_ignore_ascii_case("erode")
                    && !operator.eq_ignore_ascii_case("dilate")
                {
                    filter_diagnostic(
                        diagnostics,
                        primitive,
                        format!("unsupported morphology operator `{operator}`; used `erode`"),
                    );
                }
                morphology_pixels(
                    &input,
                    raster.width,
                    raster.height,
                    radius_x * scale_x,
                    radius_y * scale_y,
                    operator.eq_ignore_ascii_case("dilate"),
                )
            }
            name => {
                filter_diagnostic(
                    diagnostics,
                    primitive,
                    format!("unsupported filter primitive `{name}`; its input passed through"),
                );
                input
            }
        };
        let subregion = primitive_region(
            primitive,
            &region,
            bounds,
            raster.viewport,
            object_primitives,
            element_transform,
            diagnostics,
        );
        if let Some(subregion) = subregion {
            clip_to_polygon(&mut output, &subregion, raster.width, raster.height);
        }
        if let Some(result) = attribute(primitive, "result") {
            results.insert(result.trim().to_owned(), output.clone());
        }
        pixels = output;
    }

    clip_to_polygon(&mut pixels, &region.polygon, raster.width, raster.height);
    for y in 0..raster.height {
        for x in 0..raster.width {
            let index = y as usize * raster.width as usize + x as usize;
            let pixel = pixels[index];
            if pixel.alpha > 0 {
                raster.put(i64::from(x), i64::from(y), pixel);
            }
        }
    }
    true
}

fn filter_diagnostic(
    diagnostics: &mut Vec<SvgRenderDiagnostic>,
    element: &XmlNode,
    message: impl Into<String>,
) {
    diagnostics.push(SvgRenderDiagnostic {
        element: element.name.clone(),
        code: SvgRenderDiagnosticCode::UnsupportedFilter,
        message: message.into(),
    });
}

/// Parse `filterUnits`/`primitiveUnits`, whose only valid values are
/// `objectBoundingBox` and `userSpaceOnUse`.
fn filter_units(
    filter: &XmlNode,
    name: &str,
    object_bounding_box_by_default: bool,
    diagnostics: &mut Vec<SvgRenderDiagnostic>,
) -> bool {
    let Some(raw) = attribute(filter, name) else {
        return object_bounding_box_by_default;
    };
    let value = raw.trim();
    if value.eq_ignore_ascii_case("objectBoundingBox") {
        true
    } else if value.eq_ignore_ascii_case("userSpaceOnUse") {
        false
    } else {
        filter_diagnostic(
            diagnostics,
            filter,
            format!(
                "unsupported {name} `{value}`; used {}",
                if object_bounding_box_by_default {
                    "objectBoundingBox"
                } else {
                    "userSpaceOnUse"
                }
            ),
        );
        object_bounding_box_by_default
    }
}

fn parse_length_with_basis(raw: &str, percentage_basis: f32) -> Option<f32> {
    let trimmed = raw.trim();
    let value = parse_length(trimmed)?;
    Some(if trimmed.ends_with('%') {
        value * 0.01 * percentage_basis
    } else {
        value
    })
}

fn length_attribute(
    element: &XmlNode,
    name: &str,
    percentage_basis: f32,
    default: f32,
    diagnostics: &mut Vec<SvgRenderDiagnostic>,
) -> f32 {
    let Some(raw) = attribute(element, name) else {
        return default;
    };
    parse_length_with_basis(raw, percentage_basis).unwrap_or_else(|| {
        filter_diagnostic(
            diagnostics,
            element,
            format!(
                "unsupported length `{}` for `{name}`; used {default}",
                raw.trim()
            ),
        );
        default
    })
}

fn fraction_attribute(
    element: &XmlNode,
    name: &str,
    default: f32,
    diagnostics: &mut Vec<SvgRenderDiagnostic>,
) -> f32 {
    let Some(raw) = attribute(element, name) else {
        return default;
    };
    parse_fraction(raw).unwrap_or_else(|| {
        filter_diagnostic(
            diagnostics,
            element,
            format!(
                "invalid fraction `{}` for `{name}`; used {default}",
                raw.trim()
            ),
        );
        default
    })
}

/// Resolve one primitive distance in the filter's primitive units, then in
/// the element's user space. `objectBoundingBox` distances are fractions of
/// the corresponding bounding-box axis; user-space percentages use the
/// viewport axis.
fn primitive_length(
    primitive: &XmlNode,
    name: &str,
    object_bounding_box: bool,
    bounds: Bounds,
    viewport_basis: f32,
    default: f32,
    diagnostics: &mut Vec<SvgRenderDiagnostic>,
) -> f32 {
    if object_bounding_box {
        let fraction = fraction_attribute(primitive, name, default, diagnostics);
        fraction
            * if name == "dy" || name == "y" || name == "height" {
                bounds.height
            } else {
                bounds.width
            }
    } else {
        length_attribute(primitive, name, viewport_basis, default, diagnostics)
    }
}

fn filter_region(
    filter: &XmlNode,
    bounds: Bounds,
    viewport: (f32, f32),
    transform: Affine,
    diagnostics: &mut Vec<SvgRenderDiagnostic>,
) -> Option<FilterRegion> {
    let object_units = filter_units(filter, "filterUnits", true, diagnostics);
    let (x, y, width, height) = if object_units {
        (
            fraction_attribute(filter, "x", -0.1, diagnostics),
            fraction_attribute(filter, "y", -0.1, diagnostics),
            fraction_attribute(filter, "width", 1.2, diagnostics),
            fraction_attribute(filter, "height", 1.2, diagnostics),
        )
    } else {
        (
            length_attribute(filter, "x", viewport.0, -0.1 * viewport.0, diagnostics),
            length_attribute(filter, "y", viewport.1, -0.1 * viewport.1, diagnostics),
            length_attribute(filter, "width", viewport.0, 1.2 * viewport.0, diagnostics),
            length_attribute(filter, "height", viewport.1, 1.2 * viewport.1, diagnostics),
        )
    };
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    let local = if object_units {
        Bounds {
            x: x.mul_add(bounds.width, bounds.x),
            y: y.mul_add(bounds.height, bounds.y),
            width: width * bounds.width,
            height: height * bounds.height,
        }
    } else {
        Bounds {
            x,
            y,
            width,
            height,
        }
    };
    let polygon = [
        (local.x, local.y),
        (local.x + local.width, local.y),
        (local.x + local.width, local.y + local.height),
        (local.x, local.y + local.height),
    ]
    .map(|(x, y)| transform.apply(x, y))
    .to_vec();
    Some(FilterRegion { local, polygon })
}

/// Resolve a primitive's `x`/`y`/`width`/`height` subregion. Missing values
/// inherit the filter region, and an empty subregion produces transparent
/// output, as the specification requires.
fn primitive_region(
    primitive: &XmlNode,
    filter_region: &FilterRegion,
    bounds: Bounds,
    viewport: (f32, f32),
    object_units: bool,
    transform: Affine,
    diagnostics: &mut Vec<SvgRenderDiagnostic>,
) -> Option<Vec<(f32, f32)>> {
    let has_subregion = ["x", "y", "width", "height"]
        .iter()
        .any(|name| attribute(primitive, name).is_some());
    if !has_subregion {
        return None;
    }
    let (x, y, width, height) = if object_units {
        // In objectBoundingBox units every component is a fraction of the
        // element's box, including defaults inherited from a filter region
        // that may itself be expressed in userSpaceOnUse units.
        let default_x = (filter_region.local.x - bounds.x) / bounds.width;
        let default_y = (filter_region.local.y - bounds.y) / bounds.height;
        let default_width = filter_region.local.width / bounds.width;
        let default_height = filter_region.local.height / bounds.height;
        (
            fraction_attribute(primitive, "x", default_x, diagnostics),
            fraction_attribute(primitive, "y", default_y, diagnostics),
            fraction_attribute(primitive, "width", default_width, diagnostics),
            fraction_attribute(primitive, "height", default_height, diagnostics),
        )
    } else {
        (
            length_attribute(
                primitive,
                "x",
                viewport.0,
                filter_region.local.x,
                diagnostics,
            ),
            length_attribute(
                primitive,
                "y",
                viewport.1,
                filter_region.local.y,
                diagnostics,
            ),
            length_attribute(
                primitive,
                "width",
                viewport.0,
                filter_region.local.width,
                diagnostics,
            ),
            length_attribute(
                primitive,
                "height",
                viewport.1,
                filter_region.local.height,
                diagnostics,
            ),
        )
    };
    let local = if object_units {
        Bounds {
            x: x.mul_add(bounds.width, bounds.x),
            y: y.mul_add(bounds.height, bounds.y),
            width: width * bounds.width,
            height: height * bounds.height,
        }
    } else {
        Bounds {
            x,
            y,
            width,
            height,
        }
    };
    if local.width <= 0.0 || local.height <= 0.0 {
        return Some(Vec::new());
    }
    Some(
        [
            (local.x, local.y),
            (local.x + local.width, local.y),
            (local.x + local.width, local.y + local.height),
            (local.x, local.y + local.height),
        ]
        .map(|(x, y)| transform.apply(x, y))
        .to_vec(),
    )
}

fn clip_to_polygon(pixels: &mut [Color], polygon: &[(f32, f32)], width: u32, height: u32) {
    if polygon.is_empty() {
        pixels.fill(Color::TRANSPARENT);
        return;
    }
    let contours = [polygon.to_vec()];
    for y in 0..height {
        for x in 0..width {
            if !point_in_contours(
                &contours,
                ClipFillRule::NonZero,
                x as f32 + 0.5,
                y as f32 + 0.5,
            ) {
                pixels[y as usize * width as usize + x as usize] = Color::TRANSPARENT;
            }
        }
    }
}

fn filter_input(
    primitive: &XmlNode,
    name: &str,
    source: &[Color],
    source_alpha: &[Color],
    results: &BTreeMap<String, Vec<Color>>,
    previous: &[Color],
    diagnostics: &mut Vec<SvgRenderDiagnostic>,
) -> Vec<Color> {
    let Some(raw) = attribute(primitive, name) else {
        return previous.to_vec();
    };
    match raw.trim() {
        "SourceGraphic" => source.to_vec(),
        "SourceAlpha" => source_alpha.to_vec(),
        reference => {
            if let Some(result) = results.get(reference) {
                result.clone()
            } else {
                filter_diagnostic(
                    diagnostics,
                    primitive,
                    format!(
                        "unknown `{name}` reference `{reference}`; its previous-input fallback was used"
                    ),
                );
                previous.to_vec()
            }
        }
    }
}

#[allow(
    clippy::cast_possible_wrap,
    reason = "image dimensions are bounded by ImageLimits"
)]
fn offset_pixels(source: &[Color], width: u32, height: u32, dx: f32, dy: f32) -> Vec<Color> {
    let mut output = vec![Color::TRANSPARENT; source.len()];
    let shift_x = dx.round() as i32;
    let shift_y = dy.round() as i32;
    for y in 0..height as i32 {
        for x in 0..width as i32 {
            let source_x = x - shift_x;
            let source_y = y - shift_y;
            if source_x < 0 || source_y < 0 || source_x >= width as i32 || source_y >= height as i32
            {
                continue;
            }
            output[y as usize * width as usize + x as usize] =
                source[source_y as usize * width as usize + source_x as usize];
        }
    }
    output
}

/// Build a drop shadow from the input's alpha and paint it behind the input.
///
/// This is the spec's `feFlood` + `feComposite operator="in"` construction
/// over the input's alpha, offset and blurred with the caller-resolved pixel
/// distances, then merged under the input graphic. `size` is the raster's
/// `(width, height)`; `offset` and `sigma` are already in device pixels.
fn drop_shadow_pixels(
    source: &[Color],
    size: (u32, u32),
    offset: (f32, f32),
    sigma: (f32, f32),
    color: Color,
) -> Vec<Color> {
    let (width, height) = size;
    let alpha: Vec<Color> = source
        .iter()
        .map(|pixel| Color::rgba(0, 0, 0, pixel.alpha))
        .collect();
    let blurred = blur_pixels(&alpha, width, height, sigma.0, sigma.1);
    let shifted = offset_pixels(&blurred, width, height, offset.0, offset.1);
    let flood = vec![color; shifted.len()];
    let shadow = composite_pixels(&flood, &shifted, "in", [0.0; 4]);
    blend_pixels(&shadow, source, "normal")
}

fn supported_blend_mode(mode: &str) -> bool {
    matches!(
        mode.trim().to_ascii_lowercase().as_str(),
        "normal"
            | "multiply"
            | "screen"
            | "overlay"
            | "darken"
            | "lighten"
            | "color-dodge"
            | "color-burn"
            | "hard-light"
            | "soft-light"
            | "difference"
            | "exclusion"
            | "hue"
            | "saturation"
            | "color"
            | "luminosity"
    )
}

/// Composite `source` over `backdrop` with a CSS/SVG blend mode.
///
/// The separable modes use the W3C compositing formula
/// `co = Cs·as·(1-ab) + B(Cb,Cs)·as·ab + Cb·ab·(1-as)`, which keeps the
/// source visible where the backdrop is transparent. The four non-separable
/// modes use the W3C `SetLum`/`SetSat` construction on the whole colour
/// triple.
fn blend_pixels(backdrop: &[Color], source: &[Color], mode: &str) -> Vec<Color> {
    let mode = mode.trim().to_ascii_lowercase();
    let non_separable = matches!(mode.as_str(), "hue" | "saturation" | "color" | "luminosity");
    backdrop
        .iter()
        .zip(source)
        .map(|(backdrop, source)| {
            if !supported_blend_mode(&mode) || mode == "normal" {
                return source_over(*source, *backdrop);
            }
            let sa = f32::from(source.alpha) / 255.0;
            let da = f32::from(backdrop.alpha) / 255.0;
            let output_alpha = sa + da * (1.0 - sa);
            if output_alpha <= 0.0001 {
                return Color::TRANSPARENT;
            }
            let source_rgb = [
                f32::from(source.red) / 255.0,
                f32::from(source.green) / 255.0,
                f32::from(source.blue) / 255.0,
            ];
            let backdrop_rgb = [
                f32::from(backdrop.red) / 255.0,
                f32::from(backdrop.green) / 255.0,
                f32::from(backdrop.blue) / 255.0,
            ];
            let blended = if non_separable {
                non_separable_blend(&mode, backdrop_rgb, source_rgb)
            } else {
                std::array::from_fn(|index| {
                    separable_blend(&mode, backdrop_rgb[index], source_rgb[index])
                })
            };
            let channel = |index: usize| -> u8 {
                let value = (source_rgb[index] * sa * (1.0 - da)
                    + blended[index] * sa * da
                    + backdrop_rgb[index] * da * (1.0 - sa))
                    / output_alpha;
                (value.clamp(0.0, 1.0) * 255.0).round() as u8
            };
            Color::rgba(
                channel(0),
                channel(1),
                channel(2),
                (output_alpha * 255.0).round().clamp(0.0, 255.0) as u8,
            )
        })
        .collect()
}

fn separable_blend(mode: &str, backdrop: f32, source: f32) -> f32 {
    match mode {
        "multiply" => source * backdrop,
        "screen" => 1.0 - (1.0 - source) * (1.0 - backdrop),
        "overlay" => hard_light(source, backdrop),
        "darken" => source.min(backdrop),
        "lighten" => source.max(backdrop),
        "color-dodge" => {
            if backdrop <= 0.0 {
                0.0
            } else if source >= 1.0 {
                1.0
            } else {
                (backdrop / (1.0 - source)).min(1.0)
            }
        }
        "color-burn" => {
            if backdrop >= 1.0 {
                1.0
            } else if source <= 0.0 {
                0.0
            } else {
                1.0 - ((1.0 - backdrop) / source).min(1.0)
            }
        }
        "hard-light" => hard_light(backdrop, source),
        "soft-light" => {
            if source <= 0.5 {
                backdrop - (1.0 - 2.0 * source) * backdrop * (1.0 - backdrop)
            } else {
                let d = if backdrop <= 0.25 {
                    ((16.0 * backdrop - 12.0) * backdrop + 4.0) * backdrop
                } else {
                    backdrop.sqrt()
                };
                backdrop + (2.0 * source - 1.0) * (d - backdrop)
            }
        }
        "difference" => (source - backdrop).abs(),
        "exclusion" => source + backdrop - 2.0 * source * backdrop,
        _ => source,
    }
}

fn hard_light(backdrop: f32, source: f32) -> f32 {
    if source <= 0.5 {
        2.0 * source * backdrop
    } else {
        1.0 - 2.0 * (1.0 - source) * (1.0 - backdrop)
    }
}

fn non_separable_blend(mode: &str, backdrop: [f32; 3], source: [f32; 3]) -> [f32; 3] {
    let backdrop_luminosity = luminosity(backdrop);
    let source_luminosity = luminosity(source);
    match mode {
        "hue" => set_luminosity(
            set_saturation(source, saturation(backdrop)),
            backdrop_luminosity,
        ),
        "saturation" => set_luminosity(
            set_saturation(backdrop, saturation(source)),
            backdrop_luminosity,
        ),
        "color" => set_luminosity(source, backdrop_luminosity),
        "luminosity" => set_luminosity(backdrop, source_luminosity),
        _ => source,
    }
}

fn luminosity(color: [f32; 3]) -> f32 {
    0.3 * color[0] + 0.59 * color[1] + 0.11 * color[2]
}

fn saturation(color: [f32; 3]) -> f32 {
    color[0].max(color[1]).max(color[2]) - color[0].min(color[1]).min(color[2])
}

fn set_luminosity(mut color: [f32; 3], target: f32) -> [f32; 3] {
    let delta = target - luminosity(color);
    for channel in &mut color {
        *channel += delta;
    }
    let luminosity = luminosity(color);
    let minimum = color[0].min(color[1]).min(color[2]);
    let maximum = color[0].max(color[1]).max(color[2]);
    if minimum < 0.0 && luminosity - minimum > 0.0001 {
        for channel in &mut color {
            *channel = luminosity + (*channel - luminosity) * luminosity / (luminosity - minimum);
        }
    }
    if maximum > 1.0 && maximum - luminosity > 0.0001 {
        for channel in &mut color {
            *channel =
                luminosity + (*channel - luminosity) * (1.0 - luminosity) / (maximum - luminosity);
        }
    }
    color.map(|channel| channel.clamp(0.0, 1.0))
}

fn set_saturation(color: [f32; 3], target: f32) -> [f32; 3] {
    let mut order = [0, 1, 2];
    order.sort_by(|left, right| {
        color[*left]
            .partial_cmp(&color[*right])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let [minimum, middle, maximum] = order;
    let span = color[maximum] - color[minimum];
    if span <= 0.0001 {
        return [0.0; 3];
    }
    let mut result = [0.0; 3];
    result[middle] = (color[middle] - color[minimum]) * target / span;
    result[maximum] = target;
    result
}

/// Apply an SVG compositing operator.
///
/// `first` is the `in` input and `second` is the `in2` input. The Porter-Duff
/// operators are evaluated on premultiplied colours, so `in`/`out`/`atop`
/// keep the first input's straight colour when coverage is partial.
/// `arithmetic` uses the specification's
/// `k1·i1·i2 + k2·i1 + k3·i2 + k4` on every channel, including alpha.
fn composite_pixels(
    first: &[Color],
    second: &[Color],
    operator: &str,
    coefficients: [f32; 4],
) -> Vec<Color> {
    let operator = operator.trim().to_ascii_lowercase();
    first
        .iter()
        .zip(second)
        .map(|(first, second)| {
            let first_rgb = [
                f32::from(first.red) / 255.0,
                f32::from(first.green) / 255.0,
                f32::from(first.blue) / 255.0,
            ];
            let second_rgb = [
                f32::from(second.red) / 255.0,
                f32::from(second.green) / 255.0,
                f32::from(second.blue) / 255.0,
            ];
            let first_alpha = f32::from(first.alpha) / 255.0;
            let second_alpha = f32::from(second.alpha) / 255.0;
            if operator == "arithmetic" {
                let [k1, k2, k3, k4] = coefficients;
                let channel = |first: f32, second: f32| -> u8 {
                    ((k1 * first * second + k2 * first + k3 * second + k4).clamp(0.0, 1.0) * 255.0)
                        .round() as u8
                };
                return Color::rgba(
                    channel(first_rgb[0], second_rgb[0]),
                    channel(first_rgb[1], second_rgb[1]),
                    channel(first_rgb[2], second_rgb[2]),
                    channel(first_alpha, second_alpha),
                );
            }
            let (output_alpha, first_weight, second_weight) = match operator.as_str() {
                "in" => (first_alpha * second_alpha, 1.0, 0.0),
                "out" => (first_alpha * (1.0 - second_alpha), 1.0, 0.0),
                "atop" => (second_alpha, 1.0, 1.0 - first_alpha),
                "xor" => (
                    first_alpha + second_alpha - 2.0 * first_alpha * second_alpha,
                    1.0 - second_alpha,
                    1.0 - first_alpha,
                ),
                _ => (
                    first_alpha + second_alpha * (1.0 - first_alpha),
                    1.0,
                    1.0 - first_alpha,
                ),
            };
            if output_alpha <= 0.0001 {
                return Color::TRANSPARENT;
            }
            let channel = |first: f32, second: f32| -> u8 {
                let value = (first * first_alpha * first_weight
                    + second * second_alpha * second_weight)
                    / output_alpha;
                (value.clamp(0.0, 1.0) * 255.0).round() as u8
            };
            Color::rgba(
                channel(first_rgb[0], second_rgb[0]),
                channel(first_rgb[1], second_rgb[1]),
                channel(first_rgb[2], second_rgb[2]),
                (output_alpha * 255.0).round().clamp(0.0, 255.0) as u8,
            )
        })
        .collect()
}

fn source_over(source: Color, backdrop: Color) -> Color {
    let sa = f32::from(source.alpha) / 255.0;
    let da = f32::from(backdrop.alpha) / 255.0;
    let output_alpha = sa + da * (1.0 - sa);
    if output_alpha <= 0.0001 {
        return Color::TRANSPARENT;
    }
    let channel = |source: u8, backdrop: u8| -> u8 {
        let value = (f32::from(source) * sa + f32::from(backdrop) * da * (1.0 - sa))
            / (output_alpha * 255.0);
        (value.clamp(0.0, 1.0) * 255.0).round() as u8
    };
    Color::rgba(
        channel(source.red, backdrop.red),
        channel(source.green, backdrop.green),
        channel(source.blue, backdrop.blue),
        (output_alpha * 255.0).round().clamp(0.0, 255.0) as u8,
    )
}

/// Parse a `<number-optional-number>` value: the first number applies to both
/// axes when the second is absent.
fn parse_number_pair(raw: &str) -> (f32, f32) {
    let values: Vec<f32> = raw
        .split(|character: char| character == ',' || character.is_whitespace())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse::<f32>().ok())
        .collect();
    let first = values.first().copied().unwrap_or(0.0);
    (first, values.get(1).copied().unwrap_or(first))
}

fn blur_pixels(
    source: &[Color],
    width: u32,
    height: u32,
    sigma_x: f32,
    sigma_y: f32,
) -> Vec<Color> {
    if source.is_empty() || width == 0 || height == 0 {
        return source.to_vec();
    }
    let sigma_x = sigma_x.clamp(0.0, 64.0);
    let sigma_y = sigma_y.clamp(0.0, 64.0);
    if sigma_x < 0.05 && sigma_y < 0.05 {
        return source.to_vec();
    }
    let mut premultiplied: Vec<[f32; 4]> = source
        .iter()
        .map(|pixel| {
            let alpha = f32::from(pixel.alpha) / 255.0;
            [
                f32::from(pixel.red) / 255.0 * alpha,
                f32::from(pixel.green) / 255.0 * alpha,
                f32::from(pixel.blue) / 255.0 * alpha,
                alpha,
            ]
        })
        .collect();
    premultiplied = blur_axis(&premultiplied, width, height, sigma_x, true);
    premultiplied = blur_axis(&premultiplied, width, height, sigma_y, false);
    premultiplied
        .into_iter()
        .map(|pixel| {
            let alpha = pixel[3].clamp(0.0, 1.0);
            if alpha <= 0.0001 {
                return Color::TRANSPARENT;
            }
            Color::rgba(
                u8::try_from((pixel[0] / alpha * 255.0).round().clamp(0.0, 255.0) as i32)
                    .unwrap_or(255),
                u8::try_from((pixel[1] / alpha * 255.0).round().clamp(0.0, 255.0) as i32)
                    .unwrap_or(255),
                u8::try_from((pixel[2] / alpha * 255.0).round().clamp(0.0, 255.0) as i32)
                    .unwrap_or(255),
                u8::try_from((alpha * 255.0).round().clamp(0.0, 255.0) as i32).unwrap_or(255),
            )
        })
        .collect()
}

#[allow(
    clippy::cast_possible_wrap,
    reason = "image dimensions are bounded by ImageLimits"
)]
fn blur_axis(
    source: &[[f32; 4]],
    width: u32,
    height: u32,
    sigma: f32,
    horizontal: bool,
) -> Vec<[f32; 4]> {
    if sigma < 0.05 {
        return source.to_vec();
    }
    let radius = (sigma * 3.0).ceil().clamp(1.0, 64.0) as i32;
    let mut weights = Vec::with_capacity((radius * 2 + 1) as usize);
    let mut total = 0.0_f32;
    for offset in -radius..=radius {
        let weight = (-(offset as f32).powi(2) / (2.0 * sigma * sigma)).exp();
        weights.push(weight);
        total += weight;
    }
    if total > 0.0 {
        for weight in &mut weights {
            *weight /= total;
        }
    }
    let mut output = vec![[0.0_f32; 4]; source.len()];
    for y in 0..height as i32 {
        for x in 0..width as i32 {
            let mut sum = [0.0_f32; 4];
            for (index, offset) in (-radius..=radius).enumerate() {
                let (sample_x, sample_y) = if horizontal {
                    (x + offset, y)
                } else {
                    (x, y + offset)
                };
                // SVG filters extend the input's edge pixels indefinitely,
                // rather than treating the outside as transparent. Clamping
                // keeps a blur of a full-region shape opaque at that edge.
                let sample_x = sample_x.clamp(0, width as i32 - 1);
                let sample_y = sample_y.clamp(0, height as i32 - 1);
                let pixel = source[sample_y as usize * width as usize + sample_x as usize];
                let weight = weights[index];
                for channel in 0..4 {
                    sum[channel] += pixel[channel] * weight;
                }
            }
            output[y as usize * width as usize + x as usize] = sum;
        }
    }
    output
}

/// Apply a rectangular erode or dilate to the input's alpha channel.
///
/// The kernel spans `2 * radius + 1` pixels on each axis, and a radius of
/// zero (or a negative one, which the spec treats as an error) disables the
/// primitive. The kernel is separable, so the two axes run one after the
/// other; that keeps the work proportional to `radius_x + radius_y` per pixel
/// instead of their product. Radii are capped so a hostile document cannot
/// turn the passes into unbounded work.
#[allow(
    clippy::cast_possible_wrap,
    reason = "image dimensions are bounded by ImageLimits"
)]
fn morphology_pixels(
    source: &[Color],
    width: u32,
    height: u32,
    radius_x: f32,
    radius_y: f32,
    dilate: bool,
) -> Vec<Color> {
    if source.is_empty() || width == 0 || height == 0 {
        return source.to_vec();
    }
    let radius_x = radius_x.clamp(0.0, 64.0).round() as i32;
    let radius_y = radius_y.clamp(0.0, 64.0).round() as i32;
    if radius_x == 0 && radius_y == 0 {
        return source.to_vec();
    }
    let horizontal = morphology_axis(source, width, height, radius_x, true, dilate);
    morphology_axis(&horizontal, width, height, radius_y, false, dilate)
}

#[allow(
    clippy::cast_possible_wrap,
    reason = "image dimensions are bounded by ImageLimits"
)]
fn morphology_axis(
    source: &[Color],
    width: u32,
    height: u32,
    radius: i32,
    horizontal: bool,
    dilate: bool,
) -> Vec<Color> {
    if radius == 0 {
        return source.to_vec();
    }
    let width = width as i32;
    let height = height as i32;
    let mut output = vec![Color::TRANSPARENT; source.len()];
    for y in 0..height {
        for x in 0..width {
            let mut best: Option<Color> = None;
            for offset in -radius..=radius {
                let (sample_x, sample_y) = if horizontal {
                    (x + offset, y)
                } else {
                    (x, y + offset)
                };
                // Outside the buffer counts as transparent black, so an
                // erosion also thins shapes that touch the image edge.
                let sample =
                    if sample_x < 0 || sample_y < 0 || sample_x >= width || sample_y >= height {
                        Color::TRANSPARENT
                    } else {
                        source[sample_y as usize * width as usize + sample_x as usize]
                    };
                let replace = best.is_none_or(|current| {
                    if dilate {
                        sample.alpha > current.alpha
                    } else {
                        sample.alpha < current.alpha
                    }
                });
                if replace {
                    best = Some(sample);
                }
            }
            // The colour travels with the extremal-alpha sample: the
            // primitive changes coverage, and taking only the alpha would
            // paint the new pixels black.
            if let Some(pixel) = best {
                output[y as usize * width as usize + x as usize] = pixel;
            }
        }
    }
    output
}

fn parse_color_matrix(element: &XmlNode) -> Result<[f32; 20], String> {
    let raw_values: Vec<&str> = attribute(element, "values")
        .unwrap_or("")
        .split(|character: char| character == ',' || character.is_whitespace())
        .filter(|part| !part.is_empty())
        .collect();
    let values = raw_values
        .iter()
        .map(|part| {
            part.parse::<f32>()
                .map_err(|_| format!("invalid feColorMatrix value `{part}`; used identity"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let kind = attribute(element, "type")
        .unwrap_or("matrix")
        .trim()
        .to_ascii_lowercase();
    match kind.as_str() {
        "matrix" => {
            if values.len() != 20 {
                return Err(format!(
                    "feColorMatrix type=\"matrix\" needs 20 values, found {}; used identity",
                    values.len()
                ));
            }
            let mut matrix = [0.0; 20];
            matrix.copy_from_slice(&values);
            Ok(matrix)
        }
        "saturate" => {
            let saturation = values.first().copied().unwrap_or(1.0);
            Ok([
                0.213 + 0.787 * saturation,
                0.715 - 0.715 * saturation,
                0.072 - 0.072 * saturation,
                0.0,
                0.0,
                0.213 - 0.213 * saturation,
                0.715 + 0.285 * saturation,
                0.072 - 0.072 * saturation,
                0.0,
                0.0,
                0.213 - 0.213 * saturation,
                0.715 - 0.715 * saturation,
                0.072 + 0.928 * saturation,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                1.0,
                0.0,
            ])
        }
        "huerotate" => {
            let radians = values.first().copied().unwrap_or(0.0).to_radians();
            let (sin, cos) = radians.sin_cos();
            Ok([
                0.213 + cos * 0.787 - sin * 0.213,
                0.715 - cos * 0.715 - sin * 0.715,
                0.072 - cos * 0.072 + sin * 0.928,
                0.0,
                0.0,
                0.213 - cos * 0.213 + sin * 0.143,
                0.715 + cos * 0.285 + sin * 0.140,
                0.072 - cos * 0.072 - sin * 0.283,
                0.0,
                0.0,
                0.213 - cos * 0.213 - sin * 0.787,
                0.715 - cos * 0.715 + sin * 0.715,
                0.072 + cos * 0.928 + sin * 0.072,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                1.0,
                0.0,
            ])
        }
        "luminancetoalpha" => Ok([
            0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.2126,
            0.7152, 0.0722, 0.0, 0.0,
        ]),
        other => Err(format!(
            "unsupported feColorMatrix type `{other}`; used identity"
        )),
    }
}

fn apply_color_matrix(pixel: Color, matrix: &[f32; 20]) -> Color {
    let red = f32::from(pixel.red) / 255.0;
    let green = f32::from(pixel.green) / 255.0;
    let blue = f32::from(pixel.blue) / 255.0;
    let alpha = f32::from(pixel.alpha) / 255.0;
    let channel = |row: usize| -> u8 {
        let offset = row * 5;
        let value = matrix[offset] * red
            + matrix[offset + 1] * green
            + matrix[offset + 2] * blue
            + matrix[offset + 3] * alpha
            + matrix[offset + 4];
        (value.clamp(0.0, 1.0) * 255.0).round() as u8
    };
    Color::rgba(channel(0), channel(1), channel(2), channel(3))
}

/// Fill one shape with a solid colour or a gradient.
///
/// Fill every subpath of one `<path>` as one shape, with the element's
/// `fill-rule`. Strokes stay per-subpath because a stroke follows each contour.
#[allow(clippy::too_many_arguments)]
fn paint_subpaths(
    raster: &mut Raster,
    local_subpaths: &[Vec<(f32, f32)>],
    device_subpaths: &[Vec<(f32, f32)>],
    transform: Affine,
    paint: Option<Paint>,
    fill_rule: FillRule,
    fill_opacity: f32,
    gradients: &[Gradient],
) {
    let Some(paint) = paint else {
        return;
    };
    match paint {
        Paint::Color(color) => {
            raster.fill_subpaths(device_subpaths, fill_rule, fill_opacity, |_, _| color);
        }
        Paint::Gradient(index) => {
            let Some(gradient) = gradients.get(index) else {
                return;
            };
            let local_points: Vec<(f32, f32)> = local_subpaths
                .iter()
                .flat_map(|subpath| subpath.iter().copied())
                .collect();
            let Some(sampler) = GradientSampler::new(&local_points, transform, gradient) else {
                return;
            };
            raster.fill_subpaths(device_subpaths, fill_rule, fill_opacity, |x, y| {
                sampler.color_at(x, y)
            });
        }
        Paint::Inherit | Paint::None => {}
    }
}

/// The shape's points arrive both in its own user space (needed for
/// `objectBoundingBox` gradients) and in device space (the scanline fill).
/// Gradient painting maps each device pixel back through the element
/// transform, then through the gradient transform, so a rotated or scaled
/// shape samples the gradient in the coordinate system the author wrote.
fn paint_shape(
    raster: &mut Raster,
    local_points: &[(f32, f32)],
    device_points: &[(f32, f32)],
    transform: Affine,
    paint: Option<Paint>,
    opacity: f32,
    gradients: &[Gradient],
) {
    let Some(paint) = paint else {
        return;
    };
    match paint {
        Paint::Color(color) => raster.fill_polygon(device_points, opacity, |_, _| color),
        Paint::Gradient(index) => {
            let Some(gradient) = gradients.get(index) else {
                return;
            };
            let Some(sampler) = GradientSampler::new(local_points, transform, gradient) else {
                return;
            };
            raster.fill_polygon(device_points, opacity, |x, y| sampler.color_at(x, y));
        }
        Paint::Inherit | Paint::None => {}
    }
}

/// The bounds of a shape in its own user space, for `objectBoundingBox`
/// gradients. Degenerate shapes have no coordinate system to sample.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Bounds {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

fn points_bounds(points: &[(f32, f32)]) -> Option<Bounds> {
    let mut minimum = (f32::MAX, f32::MAX);
    let mut maximum = (f32::MIN, f32::MIN);
    for (x, y) in points {
        minimum.0 = minimum.0.min(*x);
        minimum.1 = minimum.1.min(*y);
        maximum.0 = maximum.0.max(*x);
        maximum.1 = maximum.1.max(*y);
    }
    let width = maximum.0 - minimum.0;
    let height = maximum.1 - minimum.1;
    (width > f32::EPSILON && height > f32::EPSILON).then_some(Bounds {
        x: minimum.0,
        y: minimum.1,
        width,
        height,
    })
}

/// Paint one shape's stroke with a solid colour or a gradient.
#[allow(clippy::too_many_arguments)]
fn paint_stroke(
    raster: &mut Raster,
    local_points: &[(f32, f32)],
    segments: &[Segment],
    transform: Affine,
    paint: Option<Paint>,
    stroke_width: f32,
    opacity: f32,
    gradients: &[Gradient],
) {
    let Some(paint) = paint else {
        return;
    };
    // A uniform transform scales the user-space width exactly; for a
    // non-uniform transform an average axis scale is the closest a round
    // capsule can get without tessellating an elliptical pen.
    let radius = stroke_width * transform_scale(transform) * 0.5;
    match paint {
        Paint::Color(color) => raster.stroke_segments(segments, radius, opacity, |_, _| color),
        Paint::Gradient(index) => {
            let Some(gradient) = gradients.get(index) else {
                return;
            };
            let Some(sampler) = GradientSampler::new(local_points, transform, gradient) else {
                return;
            };
            raster.stroke_segments(segments, radius, opacity, |x, y| sampler.color_at(x, y));
        }
        Paint::Inherit | Paint::None => {}
    }
}

/// The average scale a transform applies to a unit vector in each axis.
fn transform_scale(transform: Affine) -> f32 {
    let horizontal = transform.a.hypot(transform.b);
    let vertical = transform.c.hypot(transform.d);
    ((horizontal + vertical) * 0.5).max(f32::EPSILON)
}

/// A gradient resolved against one shape: the maps from device pixels back to
/// user space and then to the gradient's own coordinate system.
struct GradientSampler<'a> {
    gradient: &'a Gradient,
    to_local: Affine,
    to_gradient: Affine,
    space: GradientSpace,
}

enum GradientSpace {
    UserSpace,
    ObjectBounds(Bounds),
}

impl<'a> GradientSampler<'a> {
    fn new(local_points: &[(f32, f32)], transform: Affine, gradient: &'a Gradient) -> Option<Self> {
        let space = match gradient.units {
            GradientUnits::UserSpaceOnUse => GradientSpace::UserSpace,
            GradientUnits::ObjectBoundingBox => {
                GradientSpace::ObjectBounds(points_bounds(local_points)?)
            }
        };
        Some(Self {
            gradient,
            to_local: transform.inverse()?,
            to_gradient: gradient.transform.inverse()?,
            space,
        })
    }

    fn color_at(&self, x: f32, y: f32) -> Color {
        let (user_x, user_y) = self.to_local.apply(x, y);
        let (gradient_x, gradient_y) = match &self.space {
            GradientSpace::UserSpace => (user_x, user_y),
            GradientSpace::ObjectBounds(bounds) => (
                (user_x - bounds.x) / bounds.width,
                (user_y - bounds.y) / bounds.height,
            ),
        };
        let (gradient_x, gradient_y) = self.to_gradient.apply(gradient_x, gradient_y);
        self.gradient.sample(gradient_x, gradient_y)
    }
}

fn transform_points(points: &[(f32, f32)], transform: Affine) -> Vec<(f32, f32)> {
    points
        .iter()
        .map(|(x, y)| transform.apply(*x, *y))
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn stroke_outline(
    raster: &mut Raster,
    local_points: &[(f32, f32)],
    device_points: &[(f32, f32)],
    transform: Affine,
    paint: Option<Paint>,
    stroke_width: f32,
    opacity: f32,
    gradients: &[Gradient],
) {
    let segments = polyline_segments(device_points, true);
    paint_stroke(
        raster,
        local_points,
        &segments,
        transform,
        paint,
        stroke_width,
        opacity,
        gradients,
    );
}

#[allow(clippy::too_many_arguments)]
fn stroke_outline_segments(
    raster: &mut Raster,
    local_points: &[(f32, f32)],
    segments: &[Segment],
    transform: Affine,
    paint: Option<Paint>,
    stroke_width: f32,
    opacity: f32,
    gradients: &[Gradient],
) {
    paint_stroke(
        raster,
        local_points,
        segments,
        transform,
        paint,
        stroke_width,
        opacity,
        gradients,
    );
}

fn polyline_segments(points: &[(f32, f32)], closed: bool) -> Vec<Segment> {
    let mut segments = Vec::new();
    for pair in points.windows(2) {
        segments.push((pair[0], pair[1]));
    }
    if closed && points.len() > 2 {
        segments.push((*points.last().expect("non-empty"), points[0]));
    }
    segments
}

/// A minimal XML element tree: SVG loaded through `<img>` is an XML
/// document, so the tree honors self-closing elements, quoted attributes,
/// comments, processing instructions, and CDATA.
#[derive(Clone, Debug, PartialEq)]
pub struct XmlNode {
    pub name: String,
    pub attributes: Vec<(String, String)>,
    pub children: Vec<XmlNode>,
}

fn attribute<'a>(element: &'a XmlNode, name: &str) -> Option<&'a str> {
    element
        .attributes
        .iter()
        .find(|(attribute_name, _)| attribute_name == name)
        .map(|(_, value)| value.as_str())
}

/// The value of a CSS property in an element's `style` attribute.
///
/// SVG presentation attributes are CSS properties with a lower priority than
/// an inline `style`. This reads the last declaration of `name`, which is the
/// one CSS winner in a single declaration block, so `style="fill:red;fill:blue"`
/// resolves to blue.
fn style_declaration<'a>(element: &'a XmlNode, name: &str) -> Option<&'a str> {
    let style = attribute(element, "style")?;
    style
        .split(';')
        .filter_map(|declaration| declaration.split_once(':'))
        .filter(|(property, _)| property.trim().eq_ignore_ascii_case(name))
        .map(|(_, value)| value.trim())
        .rfind(|value| !value.is_empty())
}

/// A presentation attribute with its `style` declaration taking precedence.
fn property<'a>(element: &'a XmlNode, name: &str) -> Option<&'a str> {
    style_declaration(element, name).or_else(|| attribute(element, name))
}

fn parse_xml(text: &str) -> Option<XmlNode> {
    let mut stack: Vec<XmlNode> = Vec::new();
    let mut root: Option<XmlNode> = None;
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'<' {
            if text[index..].starts_with("<!--") {
                index = text[index..]
                    .find("-->")
                    .map_or(text.len(), |found| index + found + 3);
                continue;
            }
            if text[index..].starts_with("<![CDATA[") {
                index = text[index..]
                    .find("]]>")
                    .map_or(text.len(), |found| index + found + 3);
                continue;
            }
            if text[index..].starts_with("<?") || text[index..].starts_with("<!") {
                index = text[index..]
                    .find('>')
                    .map_or(text.len(), |found| index + found + 1);
                continue;
            }
            let Some(tag_end) = text[index..].find('>') else {
                break;
            };
            let tag = &text[index + 1..index + tag_end];
            index += tag_end + 1;
            if let Some(name) = tag.strip_prefix('/') {
                let name = name.trim();
                let closed = stack.pop()?;
                if !name.is_empty() && closed.name != name {
                    return None;
                }
                match stack.pop() {
                    Some(mut parent) => {
                        parent.children.push(closed);
                        stack.push(parent);
                    }
                    None => root = Some(closed),
                }
            } else {
                let self_closing = tag.ends_with('/');
                let tag = tag.strip_suffix('/').unwrap_or(tag);
                let element = parse_xml_tag(tag);
                if self_closing {
                    match stack.pop() {
                        Some(mut parent) => {
                            parent.children.push(element);
                            stack.push(parent);
                        }
                        None => root = Some(element),
                    }
                } else {
                    stack.push(element);
                }
            }
            continue;
        }
        index += 1;
    }
    root
}

fn parse_xml_tag(tag: &str) -> XmlNode {
    let mut name = String::new();
    let mut attributes = Vec::new();
    let mut characters = tag.chars().peekable();
    while let Some(character) = characters.peek() {
        if character.is_whitespace() {
            break;
        }
        name.push(*character);
        characters.next();
    }
    loop {
        while characters.peek().is_some_and(|c: &char| c.is_whitespace()) {
            characters.next();
        }
        if characters.peek().is_none() {
            break;
        }
        let mut attribute_name = String::new();
        while let Some(character) = characters.peek()
            && !character.is_whitespace()
            && *character != '='
        {
            attribute_name.push(*character);
            characters.next();
        }
        while characters.peek().is_some_and(|c: &char| c.is_whitespace()) {
            characters.next();
        }
        if characters.peek() == Some(&'=') {
            characters.next();
            while characters.peek().is_some_and(|c: &char| c.is_whitespace()) {
                characters.next();
            }
            let quote = characters.next().unwrap_or('"');
            let mut value = String::new();
            while let Some(character) = characters.peek()
                && *character != quote
            {
                value.push(*character);
                characters.next();
            }
            characters.next();
            attributes.push((attribute_name, decode_entities(&value)));
        } else {
            attributes.push((attribute_name, String::new()));
        }
    }
    XmlNode {
        name,
        attributes,
        children: Vec::new(),
    }
}

fn decode_entities(raw: &str) -> String {
    if !raw.contains('&') {
        return raw.to_owned();
    }
    raw.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// Root size resolution: explicit width/height (percentages resolve against
/// the viewBox, unparseable values fall to its smaller side), then the
/// viewBox, then the spec default 300×150.
fn root_size(
    width: Option<f32>,
    height: Option<f32>,
    view_box: Option<(f32, f32, f32, f32)>,
) -> (f32, f32) {
    let view_size = view_box.map_or((300.0, 150.0), |(_, _, w, h)| (w, h));
    let resolve = |value: Option<f32>, fallback: f32| value.unwrap_or(fallback);
    (resolve(width, view_size.0), resolve(height, view_size.1))
}

fn parse_length(raw: &str) -> Option<f32> {
    let trimmed = raw.trim();
    let number: String = trimmed
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-' || *c == '+')
        .collect();
    number
        .parse::<f32>()
        .ok()
        .filter(|value| value.is_finite())
        .filter(|_| {
            // Percentages are handled by the caller when a viewBox exists;
            // the raw scale-less number is the best available basis here.
            let suffix = trimmed[number.len()..].trim_start();
            suffix.is_empty() || suffix.eq_ignore_ascii_case("px") || suffix.ends_with('%')
        })
}

fn parse_view_box(raw: &str) -> Option<(f32, f32, f32, f32)> {
    let numbers: Vec<f32> = raw
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|part| !part.is_empty())
        .filter_map(parse_length)
        .collect();
    if numbers.len() == 4 {
        Some((numbers[0], numbers[1], numbers[2], numbers[3]))
    } else {
        None
    }
}

fn parse_points(raw: &str) -> Option<Vec<(f32, f32)>> {
    let numbers: Vec<f32> = raw
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|part| !part.is_empty())
        .filter_map(parse_length)
        .collect();
    if numbers.is_empty() || !numbers.len().is_multiple_of(2) {
        return None;
    }
    Some(
        numbers
            .chunks(2)
            .map(|pair| (pair[0], pair[1]))
            .collect::<Vec<_>>(),
    )
}

fn rect_polygon(
    x: Option<f32>,
    y: Option<f32>,
    width: Option<f32>,
    height: Option<f32>,
) -> Option<Vec<(f32, f32)>> {
    let (width, height) = (width?, height?);
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    let x = x.unwrap_or(0.0);
    let y = y.unwrap_or(0.0);
    Some(vec![
        (x, y),
        (x + width, y),
        (x + width, y + height),
        (x, y + height),
    ])
}

/// The id in a `url(#id)` paint or `clip-path` reference.
fn reference_id(raw: &str) -> Option<&str> {
    let trimmed = raw.trim();
    let lower = trimmed.to_ascii_lowercase();
    if !lower.starts_with("url(") || !trimmed.ends_with(')') {
        return None;
    }
    let inner = trimmed[4..trimmed.len() - 1].trim();
    let id = inner
        .trim_matches(|character| character == '\"' || character == '\'')
        .trim_start_matches('#');
    (!id.is_empty()).then_some(id)
}

fn parse_paint(raw: &str, current_color: Color, gradient_index: &BTreeMap<String, usize>) -> Paint {
    let trimmed = raw.trim();
    if let Some(id) = reference_id(trimmed)
        && let Some(index) = gradient_index.get(id)
    {
        return Paint::Gradient(*index);
    }
    match trimmed.to_ascii_lowercase().as_str() {
        "none" | "transparent" => Paint::None,
        "currentcolor" => Paint::Color(current_color),
        _ => parse_color(trimmed).map_or(Paint::Inherit, Paint::Color),
    }
}

/// Collect every `linearGradient` and `radialGradient` in the document.
///
/// The definitions are looked up by `id` because a shape may reference a
/// gradient that appears after it in a `<defs>` block. Duplicate ids keep the
/// first definition, which is the order a browser resolves them in.
fn collect_gradients(
    root: &XmlNode,
    current_color: Color,
) -> (Vec<Gradient>, BTreeMap<String, usize>) {
    let mut gradients = Vec::new();
    let mut index = BTreeMap::new();
    collect_gradients_into(root, current_color, &mut gradients, &mut index);
    (gradients, index)
}

fn collect_gradients_into(
    element: &XmlNode,
    current_color: Color,
    gradients: &mut Vec<Gradient>,
    index: &mut BTreeMap<String, usize>,
) {
    for child in &element.children {
        if matches!(child.name.as_str(), "linearGradient" | "radialGradient") {
            let id = attribute(child, "id")
                .map(str::trim)
                .filter(|id| !id.is_empty());
            if let Some(id) = id
                && let Some(gradient) = parse_gradient(child, current_color)
                && !index.contains_key(id)
            {
                index.insert(id.to_owned(), gradients.len());
                gradients.push(gradient);
            }
        }
        collect_gradients_into(child, current_color, gradients, index);
    }
}

/// Index every `clipPath` definition by id, including forward references.
fn collect_clip_paths(root: &XmlNode) -> BTreeMap<String, &XmlNode> {
    let mut paths = BTreeMap::new();
    collect_clip_paths_into(root, &mut paths);
    paths
}

fn collect_clip_paths_into<'a>(element: &'a XmlNode, paths: &mut BTreeMap<String, &'a XmlNode>) {
    for child in &element.children {
        if child.name == "clipPath"
            && let Some(id) = attribute(child, "id")
                .map(str::trim)
                .filter(|id| !id.is_empty())
        {
            paths.entry(id.to_owned()).or_insert(child);
        }
        collect_clip_paths_into(child, paths);
    }
}

/// Index every element with an id, for `<use>` references.
fn collect_element_ids(root: &XmlNode) -> BTreeMap<String, &XmlNode> {
    let mut ids = BTreeMap::new();
    collect_element_ids_into(root, &mut ids);
    ids
}

fn collect_element_ids_into<'a>(element: &'a XmlNode, ids: &mut BTreeMap<String, &'a XmlNode>) {
    for child in &element.children {
        if let Some(id) = attribute(child, "id")
            .map(str::trim)
            .filter(|id| !id.is_empty())
        {
            ids.entry(id.to_owned()).or_insert(child);
        }
        collect_element_ids_into(child, ids);
    }
}

/// Index every `mask` definition by id, including forward references.
fn collect_masks(root: &XmlNode) -> BTreeMap<String, &XmlNode> {
    let mut masks = BTreeMap::new();
    collect_masks_into(root, &mut masks);
    masks
}

fn collect_masks_into<'a>(element: &'a XmlNode, masks: &mut BTreeMap<String, &'a XmlNode>) {
    for child in &element.children {
        if child.name == "mask"
            && let Some(id) = attribute(child, "id")
                .map(str::trim)
                .filter(|id| !id.is_empty())
        {
            masks.entry(id.to_owned()).or_insert(child);
        }
        collect_masks_into(child, masks);
    }
}

/// Index every `filter` definition by id, including forward references.
fn collect_filters(root: &XmlNode) -> BTreeMap<String, &XmlNode> {
    let mut filters = BTreeMap::new();
    collect_filters_into(root, &mut filters);
    filters
}

fn collect_filters_into<'a>(element: &'a XmlNode, filters: &mut BTreeMap<String, &'a XmlNode>) {
    for child in &element.children {
        if child.name == "filter"
            && let Some(id) = attribute(child, "id")
                .map(str::trim)
                .filter(|id| !id.is_empty())
        {
            filters.entry(id.to_owned()).or_insert(child);
        }
        collect_filters_into(child, filters);
    }
}

fn clip_path_units(node: &XmlNode) -> ClipPathUnits {
    if attribute(node, "clipPathUnits")
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("objectBoundingBox"))
    {
        ClipPathUnits::ObjectBoundingBox
    } else {
        ClipPathUnits::UserSpaceOnUse
    }
}

/// Push a clip for `element`, if it has a resolvable `clip-path`.
///
/// `local_bounds` is the geometry bounds for a shape; containers pass `None`
/// and their descendant geometry is measured when `objectBoundingBox` units
/// need it. Returns whether a layer was pushed so the caller can pop it.
fn push_element_clip(
    raster: &mut Raster,
    element: &XmlNode,
    transform: Affine,
    local_bounds: Option<Bounds>,
    clip_paths: &BTreeMap<String, &XmlNode>,
) -> bool {
    let Some(reference) = property(element, "clip-path") else {
        return false;
    };
    let Some(id) = reference_id(reference) else {
        return false;
    };
    let Some(clip) = clip_paths.get(id) else {
        return false;
    };
    let units = clip_path_units(clip);
    let bounds = match units {
        ClipPathUnits::UserSpaceOnUse => None,
        ClipPathUnits::ObjectBoundingBox => {
            let Some(bounds) = local_bounds.or_else(|| element_bounds(element)) else {
                return false;
            };
            Some(bounds)
        }
    };
    let clip_transform = attribute(clip, "transform").map_or(Affine::IDENTITY, parse_transform);
    let fill_rule = clip_fill_rule(clip, ClipFillRule::NonZero);
    let mut polygons = Vec::new();
    collect_clip_geometry(
        clip,
        clip_transform,
        transform,
        units,
        bounds,
        fill_rule,
        &mut polygons,
    );
    raster.clip.push(ClipLayer { polygons });
    true
}

fn collect_clip_geometry(
    container: &XmlNode,
    clip_transform: Affine,
    element_transform: Affine,
    units: ClipPathUnits,
    bounds: Option<Bounds>,
    fill_rule: ClipFillRule,
    polygons: &mut Vec<ClipPolygon>,
) {
    for child in &container.children {
        let child_transform = match attribute(child, "transform") {
            Some(specification) => clip_transform.then(parse_transform(specification)),
            None => clip_transform,
        };
        let child_fill_rule = clip_fill_rule(child, fill_rule);
        let mut push = |points: &[(f32, f32)]| {
            if let Some(points) =
                clip_device_polygon(points, child_transform, element_transform, units, bounds)
            {
                polygons.push(ClipPolygon {
                    contours: vec![points],
                    fill_rule: child_fill_rule,
                });
            }
        };
        match child.name.as_str() {
            "rect" => {
                if let Some(points) = rect_polygon(
                    attribute(child, "x").and_then(parse_length),
                    attribute(child, "y").and_then(parse_length),
                    attribute(child, "width").and_then(parse_length),
                    attribute(child, "height").and_then(parse_length),
                ) {
                    push(&points);
                }
            }
            "circle" | "ellipse" => {
                let cx = attribute(child, "cx").and_then(parse_length).unwrap_or(0.0);
                let cy = attribute(child, "cy").and_then(parse_length).unwrap_or(0.0);
                let (rx, ry) = if child.name == "circle" {
                    let r = attribute(child, "r").and_then(parse_length).unwrap_or(0.0);
                    (r, r)
                } else {
                    (
                        attribute(child, "rx").and_then(parse_length).unwrap_or(0.0),
                        attribute(child, "ry").and_then(parse_length).unwrap_or(0.0),
                    )
                };
                if rx > 0.0 && ry > 0.0 {
                    let points: Vec<_> = (0..48)
                        .map(|step| {
                            let angle = std::f32::consts::TAU * step as f32 / 48.0;
                            (cx + rx * angle.cos(), cy + ry * angle.sin())
                        })
                        .collect();
                    push(&points);
                }
            }
            "polygon" | "polyline" => {
                if let Some(points) = attribute(child, "points").and_then(parse_points)
                    && points.len() >= 3
                {
                    push(&points);
                }
            }
            "path" => {
                if let Some(specification) = attribute(child, "d") {
                    let mut contours = Vec::new();
                    for (subpath, _) in parse_path(specification) {
                        if subpath.len() >= 3
                            && let Some(points) = clip_device_polygon(
                                &subpath,
                                child_transform,
                                element_transform,
                                units,
                                bounds,
                            )
                        {
                            contours.push(points);
                        }
                    }
                    if !contours.is_empty() {
                        polygons.push(ClipPolygon {
                            contours,
                            fill_rule: child_fill_rule,
                        });
                    }
                }
            }
            "g" | "svg" | "a" | "switch" => {
                collect_clip_geometry(
                    child,
                    child_transform,
                    element_transform,
                    units,
                    bounds,
                    child_fill_rule,
                    polygons,
                );
            }
            _ => {}
        }
    }
}

fn clip_fill_rule(element: &XmlNode, inherited: ClipFillRule) -> ClipFillRule {
    if let Some(value) = attribute(element, "clip-rule") {
        return if value.trim().eq_ignore_ascii_case("evenodd") {
            ClipFillRule::EvenOdd
        } else {
            ClipFillRule::NonZero
        };
    }
    if let Some(style) = attribute(element, "style") {
        let style = style.to_ascii_lowercase().replace(' ', "");
        if style.contains("clip-rule:evenodd") {
            return ClipFillRule::EvenOdd;
        }
        if style.contains("clip-rule:nonzero") {
            return ClipFillRule::NonZero;
        }
    }
    inherited
}

/// A `clipPath` definition extracted from serialized markup, for a CSS
/// `clip-path: url(#id)` reference.
pub(crate) struct CssClipPathGeometry {
    pub(crate) object_bounding_box: bool,
    pub(crate) even_odd: bool,
    pub(crate) contours: Vec<Vec<(f32, f32)>>,
}

pub(crate) fn css_clip_path_geometry(markup: &[u8]) -> Option<CssClipPathGeometry> {
    let text = str::from_utf8(markup).ok()?;
    let root = parse_xml(text)?;
    if root.name != "clipPath" {
        return None;
    }
    let object_bounding_box = clip_path_units(&root) == ClipPathUnits::ObjectBoundingBox;
    let transform = attribute(&root, "transform").map_or(Affine::IDENTITY, parse_transform);
    let fill_rule = clip_fill_rule(&root, ClipFillRule::NonZero);
    let mut polygons = Vec::new();
    collect_clip_geometry(
        &root,
        transform,
        Affine::IDENTITY,
        ClipPathUnits::UserSpaceOnUse,
        None,
        fill_rule,
        &mut polygons,
    );
    let mut contours = Vec::new();
    for polygon in &polygons {
        contours.extend(polygon.contours.iter().cloned());
    }
    Some(CssClipPathGeometry {
        object_bounding_box,
        even_odd: fill_rule == ClipFillRule::EvenOdd,
        contours,
    })
}

pub(crate) fn parse_css_path_contours(data: &str) -> Vec<Vec<(f32, f32)>> {
    parse_path(data)
        .into_iter()
        .map(|(points, _)| points)
        .filter(|points| points.len() >= 3)
        .collect()
}

fn clip_device_polygon(
    points: &[(f32, f32)],
    clip_transform: Affine,
    element_transform: Affine,
    units: ClipPathUnits,
    bounds: Option<Bounds>,
) -> Option<Vec<(f32, f32)>> {
    if points.len() < 3 {
        return None;
    }
    let mut polygon = Vec::with_capacity(points.len());
    for (x, y) in points {
        let (x, y) = clip_transform.apply(*x, *y);
        let (x, y) = match units {
            ClipPathUnits::UserSpaceOnUse => (x, y),
            ClipPathUnits::ObjectBoundingBox => {
                let bounds = bounds?;
                (bounds.x + x * bounds.width, bounds.y + y * bounds.height)
            }
        };
        polygon.push(element_transform.apply(x, y));
    }
    Some(polygon)
}

/// Bounds of all shape geometry below `element`, in the element's own user
/// coordinate system. Used for `clipPathUnits="objectBoundingBox"` on a group.
fn element_bounds(element: &XmlNode) -> Option<Bounds> {
    let mut points = Vec::new();
    collect_bounds(element, Affine::IDENTITY, &mut points);
    points_bounds(&points)
}

fn collect_bounds(element: &XmlNode, transform: Affine, points: &mut Vec<(f32, f32)>) {
    match element.name.as_str() {
        "rect" => {
            if let Some(local) = rect_polygon(
                attribute(element, "x").and_then(parse_length),
                attribute(element, "y").and_then(parse_length),
                attribute(element, "width").and_then(parse_length),
                attribute(element, "height").and_then(parse_length),
            ) {
                points.extend(local.iter().map(|(x, y)| transform.apply(*x, *y)));
            }
            return;
        }
        "circle" | "ellipse" => {
            let cx = attribute(element, "cx")
                .and_then(parse_length)
                .unwrap_or(0.0);
            let cy = attribute(element, "cy")
                .and_then(parse_length)
                .unwrap_or(0.0);
            let (rx, ry) = if element.name == "circle" {
                let r = attribute(element, "r")
                    .and_then(parse_length)
                    .unwrap_or(0.0);
                (r, r)
            } else {
                (
                    attribute(element, "rx")
                        .and_then(parse_length)
                        .unwrap_or(0.0),
                    attribute(element, "ry")
                        .and_then(parse_length)
                        .unwrap_or(0.0),
                )
            };
            if rx > 0.0 && ry > 0.0 {
                for step in 0..48 {
                    let angle = std::f32::consts::TAU * step as f32 / 48.0;
                    points.push(transform.apply(cx + rx * angle.cos(), cy + ry * angle.sin()));
                }
            }
            return;
        }
        "polygon" | "polyline" => {
            if let Some(local) = attribute(element, "points").and_then(parse_points) {
                points.extend(local.iter().map(|(x, y)| transform.apply(*x, *y)));
            }
            return;
        }
        "path" => {
            if let Some(specification) = attribute(element, "d") {
                for (subpath, _) in parse_path(specification) {
                    points.extend(subpath.iter().map(|(x, y)| transform.apply(*x, *y)));
                }
            }
            return;
        }
        "line" => {
            let (Some(x1), Some(y1), Some(x2), Some(y2)) = (
                attribute(element, "x1").and_then(parse_length),
                attribute(element, "y1").and_then(parse_length),
                attribute(element, "x2").and_then(parse_length),
                attribute(element, "y2").and_then(parse_length),
            ) else {
                return;
            };
            points.push(transform.apply(x1, y1));
            points.push(transform.apply(x2, y2));
            return;
        }
        _ => {}
    }
    for child in &element.children {
        let child_transform = match attribute(child, "transform") {
            Some(specification) => transform.then(parse_transform(specification)),
            None => transform,
        };
        collect_bounds(child, child_transform, points);
    }
}

fn parse_gradient(element: &XmlNode, current_color: Color) -> Option<Gradient> {
    let units = if attribute(element, "gradientUnits")
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("userSpaceOnUse"))
    {
        GradientUnits::UserSpaceOnUse
    } else {
        GradientUnits::ObjectBoundingBox
    };
    let transform =
        attribute(element, "gradientTransform").map_or(Affine::IDENTITY, parse_transform);
    let mut stops: Vec<GradientStop> = element
        .children
        .iter()
        .filter(|child| child.name == "stop")
        .map(|child| parse_stop(child, current_color))
        .collect();
    stops.sort_by(|left, right| {
        left.offset
            .partial_cmp(&right.offset)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let kind = match element.name.as_str() {
        "linearGradient" => GradientKind::Linear {
            x1: attribute(element, "x1")
                .and_then(parse_fraction)
                .unwrap_or(0.0),
            y1: attribute(element, "y1")
                .and_then(parse_fraction)
                .unwrap_or(0.0),
            x2: attribute(element, "x2")
                .and_then(parse_fraction)
                .unwrap_or(1.0),
            y2: attribute(element, "y2")
                .and_then(parse_fraction)
                .unwrap_or(0.0),
        },
        "radialGradient" => GradientKind::Radial {
            cx: attribute(element, "cx")
                .and_then(parse_fraction)
                .unwrap_or(0.5),
            cy: attribute(element, "cy")
                .and_then(parse_fraction)
                .unwrap_or(0.5),
            r: attribute(element, "r")
                .and_then(parse_fraction)
                .unwrap_or(0.5),
        },
        _ => return None,
    };
    Some(Gradient {
        kind,
        units,
        transform,
        stops,
    })
}

fn parse_stop(element: &XmlNode, current_color: Color) -> GradientStop {
    let offset = attribute(element, "offset")
        .and_then(parse_fraction)
        .unwrap_or(0.0)
        .clamp(0.0, 1.0);
    let mut color = match attribute(element, "stop-color").map(str::trim) {
        Some(value) if value.eq_ignore_ascii_case("currentcolor") => current_color,
        Some(value) => parse_color(value).unwrap_or(Color::BLACK),
        None => Color::BLACK,
    };
    if let Some(opacity) = attribute(element, "stop-opacity").and_then(parse_opacity) {
        color = color.with_opacity(opacity);
    }
    GradientStop { offset, color }
}

/// A gradient coordinate: a number, or a percentage of the relevant basis.
fn parse_fraction(raw: &str) -> Option<f32> {
    let trimmed = raw.trim();
    let value = parse_length(trimmed)?;
    Some(if trimmed.ends_with('%') {
        value / 100.0
    } else {
        value
    })
}

fn parse_opacity(raw: &str) -> Option<f32> {
    parse_fraction(raw).map(|value| value.clamp(0.0, 1.0))
}

fn parse_color(raw: &str) -> Option<Color> {
    let trimmed = raw.trim();
    if let Some(hex) = trimmed.strip_prefix('#') {
        let digits = hex.as_bytes();
        return match digits.len() {
            3 => {
                let expanded: Vec<u8> = digits.iter().flat_map(|byte| [*byte, *byte]).collect();
                six_digit_hex(&expanded, 255)
            }
            6 => six_digit_hex(digits, 255),
            8 => six_digit_hex(
                digits,
                six_digit_hex(&digits[4..8], 255).map_or(255, |c| c.alpha),
            )
            .map(|color| color.with_opacity(1.0))
            .and(six_digit_hex(
                &digits[..6],
                six_digit_hex(&digits[6..8], 255).map_or(255, |c| c.alpha),
            )),
            _ => None,
        };
    }
    match trimmed.to_ascii_lowercase().as_str() {
        "black" => Some(Color::rgb(0, 0, 0)),
        "white" => Some(Color::rgb(255, 255, 255)),
        "red" => Some(Color::rgb(255, 0, 0)),
        "green" => Some(Color::rgb(0, 128, 0)),
        "lime" => Some(Color::rgb(0, 255, 0)),
        "blue" => Some(Color::rgb(0, 0, 255)),
        "yellow" => Some(Color::rgb(255, 255, 0)),
        "orange" => Some(Color::rgb(255, 165, 0)),
        "purple" => Some(Color::rgb(128, 0, 128)),
        "gray" | "grey" => Some(Color::rgb(128, 128, 128)),
        "silver" => Some(Color::rgb(192, 192, 192)),
        "cyan" | "aqua" => Some(Color::rgb(0, 255, 255)),
        "magenta" | "fuchsia" => Some(Color::rgb(255, 0, 255)),
        _ => None,
    }
}

fn six_digit_hex(digits: &[u8], alpha: u8) -> Option<Color> {
    if digits.len() < 6 {
        return None;
    }
    let channel = |index: usize| -> Option<u8> {
        let high = hex_value(digits[index])?;
        let low = hex_value(digits[index + 1])?;
        Some(high * 16 + low)
    };
    Some(Color::rgba(channel(0)?, channel(2)?, channel(4)?, alpha))
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Parse a `transform` attribute into a composed [`Affine`].
fn parse_transform(raw: &str) -> Affine {
    transform_functions(raw)
        .into_iter()
        .fold(Affine::IDENTITY, |accumulated, (name, numbers)| {
            accumulated.then(transform_function(&name, &numbers))
        })
}

/// Split a `transform` attribute into its `(name, arguments)` pairs, in
/// source order. SVG 2 §7.6 lets a list of transform functions appear in one
/// attribute; an unparseable or unterminated entry ends the scan rather than
/// failing the document.
fn transform_functions(raw: &str) -> Vec<(String, Vec<f32>)> {
    let mut functions = Vec::new();
    let bytes = raw.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        while index < bytes.len() && !bytes[index].is_ascii_alphabetic() {
            index += 1;
        }
        let name_start = index;
        while index < bytes.len() && bytes[index].is_ascii_alphabetic() {
            index += 1;
        }
        let name = raw[name_start..index].to_owned();
        while index < bytes.len() && bytes[index] != b'(' {
            index += 1;
        }
        if index >= bytes.len() {
            break;
        }
        index += 1;
        let args_start = index;
        while index < bytes.len() && bytes[index] != b')' {
            index += 1;
        }
        let arguments = raw[args_start..index.min(raw.len())]
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|part| !part.is_empty())
            .filter_map(parse_length)
            .collect();
        index = index.min(raw.len()) + 1;
        functions.push((name, arguments));
    }
    functions
}

/// The affine for one transform function, per SVG 2 §7.6. An unknown name
/// contributes the identity, so a document using a function outside this
/// subset still renders the shapes it does support.
fn transform_function(name: &str, numbers: &[f32]) -> Affine {
    match name {
        "translate" => Affine {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: numbers.first().copied().unwrap_or(0.0),
            f: numbers.get(1).copied().unwrap_or(0.0),
        },
        "scale" => {
            let sx = numbers.first().copied().unwrap_or(1.0);
            let sy = numbers.get(1).copied().unwrap_or(sx);
            Affine {
                a: sx,
                b: 0.0,
                c: 0.0,
                d: sy,
                e: 0.0,
                f: 0.0,
            }
        }
        "matrix" if numbers.len() == 6 => Affine {
            a: numbers[0],
            b: numbers[1],
            c: numbers[2],
            d: numbers[3],
            e: numbers[4],
            f: numbers[5],
        },
        "rotate" => {
            let radians = numbers.first().copied().unwrap_or(0.0).to_radians();
            let (sin, cos) = radians.sin_cos();
            let rotation = Affine {
                a: cos,
                b: sin,
                c: -sin,
                d: cos,
                e: 0.0,
                f: 0.0,
            };
            match (numbers.get(1).copied(), numbers.get(2).copied()) {
                (Some(cx), Some(cy)) => Affine::IDENTITY
                    .then(Affine {
                        a: 1.0,
                        b: 0.0,
                        c: 0.0,
                        d: 1.0,
                        e: cx,
                        f: cy,
                    })
                    .then(rotation)
                    .then(Affine {
                        a: 1.0,
                        b: 0.0,
                        c: 0.0,
                        d: 1.0,
                        e: -cx,
                        f: -cy,
                    }),
                _ => rotation,
            }
        }
        "skewx" => Affine {
            a: 1.0,
            b: 0.0,
            c: numbers.first().copied().unwrap_or(0.0).to_radians().tan(),
            d: 1.0,
            e: 0.0,
            f: 0.0,
        },
        "skewy" => Affine {
            a: 1.0,
            b: numbers.first().copied().unwrap_or(0.0).to_radians().tan(),
            c: 0.0,
            d: 1.0,
            e: 0.0,
            f: 0.0,
        },
        _ => Affine::IDENTITY,
    }
}

fn path_number(tokens: &[PathToken], index: &mut usize) -> Option<f32> {
    match tokens.get(*index) {
        Some(PathToken::Number(value)) => {
            *index += 1;
            Some(*value)
        }
        _ => None,
    }
}

/// Flatten one SVG elliptical arc (`A`/`a`) into points, per SVG 2
/// implementation notes §B.2.4 (endpoint to center parameterisation).
///
/// A path `A` command used to fall through to its endpoint as a straight line;
/// icon sets use arcs for almost every curve, so that turned logos and social
/// marks into blobs. Radii are corrected as the spec requires when they are
/// too small to span the endpoints, and the flags choose the centre/angle.
fn arc_points(
    from: (f32, f32),
    to: (f32, f32),
    rx: f32,
    ry: f32,
    rotation_degrees: f32,
    large_arc: bool,
    sweep: bool,
) -> Vec<(f32, f32)> {
    let phi = rotation_degrees.to_radians();
    let (sin_phi, cos_phi) = phi.sin_cos();
    let (x1, y1) = from;
    let (x2, y2) = to;
    let dx = (x1 - x2) * 0.5;
    let dy = (y1 - y2) * 0.5;
    let x1p = cos_phi.mul_add(dx, sin_phi * dy);
    let y1p = (-sin_phi).mul_add(dx, cos_phi * dy);
    let mut rx = rx;
    let mut ry = ry;
    let lambda = (x1p * x1p) / (rx * rx) + (y1p * y1p) / (ry * ry);
    if lambda > 1.0 {
        let scale = lambda.sqrt();
        rx *= scale;
        ry *= scale;
    }
    let rx2 = rx * rx;
    let ry2 = ry * ry;
    let numerator = (rx2 * ry2) - (rx2 * y1p * y1p) - (ry2 * x1p * x1p);
    let denominator = (rx2 * y1p * y1p) + (ry2 * x1p * x1p);
    let mut coefficient = if denominator <= f32::EPSILON {
        0.0
    } else {
        (numerator.max(0.0) / denominator).sqrt()
    };
    if large_arc == sweep {
        coefficient = -coefficient;
    }
    let cxp = coefficient * rx * y1p / ry;
    let cyp = -coefficient * ry * x1p / rx;
    let cx = cos_phi.mul_add(cxp, -sin_phi * cyp) + (x1 + x2) * 0.5;
    let cy = sin_phi.mul_add(cxp, cos_phi * cyp) + (y1 + y2) * 0.5;
    let vector_angle =
        |ux: f32, uy: f32, vx: f32, vy: f32| (ux * vy - uy * vx).atan2(ux * vx + uy * vy);
    let start_vector = ((x1p - cxp) / rx, (y1p - cyp) / ry);
    let end_vector = ((-x1p - cxp) / rx, (-y1p - cyp) / ry);
    let theta1 = vector_angle(1.0, 0.0, start_vector.0, start_vector.1);
    let mut delta = vector_angle(start_vector.0, start_vector.1, end_vector.0, end_vector.1);
    if !sweep && delta > 0.0 {
        delta -= std::f32::consts::TAU;
    } else if sweep && delta < 0.0 {
        delta += std::f32::consts::TAU;
    }
    let steps = (delta.abs() / (std::f32::consts::PI / 24.0)).ceil() as usize;
    let steps = steps.clamp(4, 128);
    let mut points = Vec::with_capacity(steps);
    for step in 1..=steps {
        let angle = theta1 + delta * (step as f32 / steps as f32);
        let (sin_angle, cos_angle) = angle.sin_cos();
        let x = cx + cos_phi * rx * cos_angle - sin_phi * ry * sin_angle;
        let y = cy + sin_phi * rx * cos_angle + cos_phi * ry * sin_angle;
        points.push((x, y));
    }
    points
}

fn path_pair(tokens: &[PathToken], index: &mut usize) -> Option<(f32, f32)> {
    let x = path_number(tokens, index)?;
    let y = path_number(tokens, index)?;
    Some((x, y))
}

/// Parse a path `d` attribute into subpaths of points with a closed flag.
/// Curves are flattened by fixed-step sampling; arc segments degrade to a
/// straight line to their endpoint.
#[allow(clippy::too_many_lines)]
fn parse_path(raw: &str) -> Vec<(Vec<(f32, f32)>, bool)> {
    let tokens = &tokenize_path(raw);
    let mut subpaths: Vec<(Vec<(f32, f32)>, bool)> = Vec::new();
    let mut current: Vec<(f32, f32)> = Vec::new();
    let mut command = ' ';
    let mut index = 0;
    let mut cursor = (0.0_f32, 0.0_f32);
    let mut subpath_start = (0.0_f32, 0.0_f32);
    let mut last_cubic_control: Option<(f32, f32)> = None;
    let mut last_quad_control: Option<(f32, f32)> = None;

    while index < tokens.len() {
        let next = &tokens[index];
        if let PathToken::Command(letter) = next {
            command = *letter;
            index += 1;
            // After M/m, subsequent coordinate pairs are implicit line-tos.
            if matches!(command, 'M' | 'm') {
                command = if command == 'M' { 'M' } else { 'm' };
            }
        }
        let relative = command.is_ascii_lowercase();
        match command.to_ascii_uppercase() {
            'M' => {
                let Some((x, y)) = path_pair(tokens, &mut index) else {
                    break;
                };
                cursor = if relative {
                    (cursor.0 + x, cursor.1 + y)
                } else {
                    (x, y)
                };
                if !current.is_empty() {
                    subpaths.push((std::mem::take(&mut current), false));
                }
                current.push(cursor);
                subpath_start = cursor;
                last_cubic_control = None;
                last_quad_control = None;
                command = if relative { 'l' } else { 'L' };
            }
            'L' => {
                let Some((x, y)) = path_pair(tokens, &mut index) else {
                    break;
                };
                cursor = if relative {
                    (cursor.0 + x, cursor.1 + y)
                } else {
                    (x, y)
                };
                current.push(cursor);
            }
            'H' => {
                let Some(x) = path_number(tokens, &mut index) else {
                    break;
                };
                cursor.0 = if relative { cursor.0 + x } else { x };
                current.push(cursor);
            }
            'V' => {
                let Some(y) = path_number(tokens, &mut index) else {
                    break;
                };
                cursor.1 = if relative { cursor.1 + y } else { y };
                current.push(cursor);
            }
            'C' => {
                let Some((x1, y1)) = path_pair(tokens, &mut index) else {
                    break;
                };
                let Some((x2, y2)) = path_pair(tokens, &mut index) else {
                    break;
                };
                let Some((x, y)) = path_pair(tokens, &mut index) else {
                    break;
                };
                let (p0x, p0y) = cursor;
                let (x1, y1) = if relative {
                    (p0x + x1, p0y + y1)
                } else {
                    (x1, y1)
                };
                let (x2, y2) = if relative {
                    (p0x + x2, p0y + y2)
                } else {
                    (x2, y2)
                };
                let (x, y) = if relative { (p0x + x, p0y + y) } else { (x, y) };
                for step in 1..=12 {
                    let t = step as f32 / 12.0;
                    current.push(cubic_point((p0x, p0y), (x1, y1), (x2, y2), (x, y), t));
                }
                last_cubic_control = Some((x2, y2));
                cursor = (x, y);
            }
            'S' => {
                let Some((x2, y2)) = path_pair(tokens, &mut index) else {
                    break;
                };
                let Some((x, y)) = path_pair(tokens, &mut index) else {
                    break;
                };
                let (p0x, p0y) = cursor;
                let (x2, y2) = if relative {
                    (p0x + x2, p0y + y2)
                } else {
                    (x2, y2)
                };
                let (x, y) = if relative { (p0x + x, p0y + y) } else { (x, y) };
                let (x1, y1) = last_cubic_control
                    .map_or((p0x, p0y), |(cx, cy)| (2.0 * p0x - cx, 2.0 * p0y - cy));
                for step in 1..=12 {
                    let t = step as f32 / 12.0;
                    current.push(cubic_point((p0x, p0y), (x1, y1), (x2, y2), (x, y), t));
                }
                last_cubic_control = Some((x2, y2));
                cursor = (x, y);
            }
            'Q' => {
                let Some((x1, y1)) = path_pair(tokens, &mut index) else {
                    break;
                };
                let Some((x, y)) = path_pair(tokens, &mut index) else {
                    break;
                };
                let (p0x, p0y) = cursor;
                let (x1, y1) = if relative {
                    (p0x + x1, p0y + y1)
                } else {
                    (x1, y1)
                };
                let (x, y) = if relative { (p0x + x, p0y + y) } else { (x, y) };
                for step in 1..=10 {
                    let t = step as f32 / 10.0;
                    current.push(quad_point((p0x, p0y), (x1, y1), (x, y), t));
                }
                last_quad_control = Some((x1, y1));
                cursor = (x, y);
            }
            'T' => {
                let Some((x, y)) = path_pair(tokens, &mut index) else {
                    break;
                };
                let (p0x, p0y) = cursor;
                let (x, y) = if relative { (p0x + x, p0y + y) } else { (x, y) };
                let (x1, y1) = last_quad_control
                    .map_or((p0x, p0y), |(cx, cy)| (2.0 * p0x - cx, 2.0 * p0y - cy));
                for step in 1..=10 {
                    let t = step as f32 / 10.0;
                    current.push(quad_point((p0x, p0y), (x1, y1), (x, y), t));
                }
                last_quad_control = Some((x1, y1));
                cursor = (x, y);
            }
            'A' => {
                let Some(rx) = path_number(tokens, &mut index) else {
                    break;
                };
                let Some(ry) = path_number(tokens, &mut index) else {
                    break;
                };
                let Some(rotation) = path_number(tokens, &mut index) else {
                    break;
                };
                let Some(large_arc) = path_number(tokens, &mut index) else {
                    break;
                };
                let Some(sweep) = path_number(tokens, &mut index) else {
                    break;
                };
                let Some((x, y)) = path_pair(tokens, &mut index) else {
                    break;
                };
                let end = if relative {
                    (cursor.0 + x, cursor.1 + y)
                } else {
                    (x, y)
                };
                if rx.abs() <= f32::EPSILON || ry.abs() <= f32::EPSILON {
                    current.push(end);
                } else {
                    current.extend(arc_points(
                        cursor,
                        end,
                        rx.abs(),
                        ry.abs(),
                        rotation,
                        large_arc != 0.0,
                        sweep != 0.0,
                    ));
                }
                cursor = end;
                last_cubic_control = None;
                last_quad_control = None;
            }
            'Z' => {
                if !current.is_empty() {
                    current.push(subpath_start);
                    subpaths.push((std::mem::take(&mut current), true));
                }
                cursor = subpath_start;
            }
            _ => break,
        }
    }
    if !current.is_empty() {
        subpaths.push((current, false));
    }
    subpaths
}

/// A point on a Bézier control polygon, as `(x, y)`.
type ControlPoint = (f32, f32);

/// A point on a cubic Bézier at parameter `t` (SVG 2 §8.3.6).
fn cubic_point(
    start: ControlPoint,
    first_control: ControlPoint,
    second_control: ControlPoint,
    end: ControlPoint,
    t: f32,
) -> (f32, f32) {
    let inverse = 1.0 - t;
    // The degree-3 Bernstein basis weights.
    let w0 = inverse * inverse * inverse;
    let w1 = 3.0 * inverse * inverse * t;
    let w2 = 3.0 * inverse * t * t;
    let w3 = t * t * t;
    (
        w0 * start.0 + w1 * first_control.0 + w2 * second_control.0 + w3 * end.0,
        w0 * start.1 + w1 * first_control.1 + w2 * second_control.1 + w3 * end.1,
    )
}

/// A point on a quadratic Bézier at parameter `t` (SVG 2 §8.3.5).
fn quad_point(start: ControlPoint, control: ControlPoint, end: ControlPoint, t: f32) -> (f32, f32) {
    let inverse = 1.0 - t;
    // The degree-2 Bernstein basis weights.
    let w0 = inverse * inverse;
    let w1 = 2.0 * inverse * t;
    let w2 = t * t;
    (
        w0 * start.0 + w1 * control.0 + w2 * end.0,
        w0 * start.1 + w1 * control.1 + w2 * end.1,
    )
}

enum PathToken {
    Command(char),
    Number(f32),
}

fn tokenize_path(raw: &str) -> Vec<PathToken> {
    let mut tokens = Vec::new();
    let mut number = String::new();
    for character in raw.chars() {
        if character.is_ascii_alphabetic() {
            flush_number(&mut number, &mut tokens);
            tokens.push(PathToken::Command(character));
        } else if character.is_ascii_digit()
            || character == '.'
            || character == '-'
            || character == '+'
            || character == 'e'
            || character == 'E'
        {
            let ends_with_exponent = number.ends_with(['e', 'E']);
            let starts_new_number =
                (character == '-' || character == '+') && !ends_with_exponent && !number.is_empty();
            let starts_new_fraction = character == '.' && number.contains('.');
            if starts_new_number || starts_new_fraction {
                flush_number(&mut number, &mut tokens);
            }
            number.push(character);
        } else {
            flush_number(&mut number, &mut tokens);
        }
    }
    flush_number(&mut number, &mut tokens);
    tokens
}

fn flush_number(number: &mut String, tokens: &mut Vec<PathToken>) {
    if number.is_empty() {
        return;
    }
    if let Ok(value) = number.parse::<f32>() {
        tokens.push(PathToken::Number(value));
    }
    number.clear();
}

#[cfg(test)]
mod tests {
    use super::{
        Color, ImageLimits, SvgRenderDiagnosticCode, decode_svg, decode_svg_viewport,
        decode_svg_with_diagnostics, parse_path,
    };
    use std::fmt::Write as _;

    const T5_MEDAL: &str = r#"<svg class="medal-newlevel-circled" xmlns="http://www.w3.org/2000/svg" data-priority="2" width="32" height="32" viewBox="0 0 32 32" fill="none">
  <g clip-path="url(#clip0_5048_100697)">
    <path d="M16 0.75C24.4223 0.75 31.25 7.57766 31.25 16C31.25 24.4223 24.4223 31.25 16 31.25C7.57766 31.25 0.75 24.4223 0.75 16C0.75 7.57766 7.57766 0.75 16 0.75Z" stroke="white" stroke-width="1.5" stroke-miterlimit="10" />
    <path d="M15.9268 13.5645C19.0638 13.5646 21.6064 16.1081 21.6064 19.2451C21.6063 22.382 19.0637 24.9246 15.9268 24.9248C12.7897 24.9248 10.2463 22.3821 10.2461 19.2451C10.2461 16.108 12.7896 13.5645 15.9268 13.5645ZM15.7949 16.3682C15.0948 16.3734 14.394 16.6404 13.8594 17.168C12.7794 18.2338 12.7794 19.9615 13.8594 21.0273C14.0754 21.2405 14.4255 21.2405 14.6416 21.0273C14.8577 20.8142 14.8577 20.469 14.6416 20.2559C13.9936 19.6163 13.9936 18.579 14.6416 17.9395C14.9625 17.6228 15.3815 17.4632 15.8027 17.46C16.1082 17.4577 16.3547 17.2115 16.3525 16.9102C16.3502 16.6088 16.1003 16.3661 15.7949 16.3682Z" fill="white" />
    <path d="M15.9268 13.5889C17.2017 13.5889 18.3792 14.0058 19.3311 14.71L16.6689 16.0283C16.2017 16.2596 15.6518 16.2596 15.1846 16.0283L12.5215 14.71C13.4735 14.0054 14.6515 13.5889 15.9268 13.5889ZM22.0127 8.17969C22.9294 8.17972 23.6729 8.91569 23.6729 9.82324V11.5459C23.6729 12.1683 23.3171 12.7372 22.7549 13.0156L20.2305 14.2646C19.0714 13.2768 17.569 12.6797 15.9268 12.6797C14.2845 12.6797 12.7821 13.2768 11.623 14.2646L9.09863 13.0156C8.53631 12.7373 8.18066 12.1683 8.18066 11.5459V9.82324C8.18066 8.91567 8.92412 8.17969 9.84082 8.17969H22.0127Z" fill="white" />
  </g>
  <defs>
    <clipPath id="clip0_5048_100697">
      <rect width="32" height="32" fill="white" />
    </clipPath>
  </defs>
</svg>
"#;

    #[test]
    fn rect_fill_covers_exact_area() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect x="2" y="3" width="4" height="5" fill="#ff0000"/></svg>"##;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!((image.width(), image.height()), (10, 10));
        assert_eq!(image.pixel(2, 3), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(5, 7), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(6, 3), None.or(Some(Color::rgba(0, 0, 0, 0))));
        assert_eq!(image.pixel(1, 3), Some(Color::rgba(0, 0, 0, 0)));
    }

    #[test]
    fn polygon_and_circle_render() {
        let svg = br##"<svg width="20" height="20"><polygon points="0,0 10,0 0,10" fill="blue"/><circle cx="15" cy="15" r="3" fill="#00ff00"/></svg>"##;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(1, 1), Some(Color::rgb(0, 0, 255)));
        assert_eq!(image.pixel(15, 15), Some(Color::rgb(0, 255, 0)));
    }

    #[test]
    fn path_lineto_and_close_fill() {
        let svg = br#"<svg width="8" height="8"><path d="M1 1 L6 1 L1 6 Z" fill="red"/></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(2, 2), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(5, 5), Some(Color::rgba(0, 0, 0, 0)));
    }

    /// `viewBox` is camelCase and the document is XML, so a case-insensitive
    /// or lowercased lookup would find nothing. The rect covers user-space
    /// 2..6 of a 10-unit `viewBox` mapped onto a 20px viewport, so its edges
    /// land on device pixels 4 and 12.
    #[test]
    fn view_box_scales_geometry_to_the_viewport() {
        let svg = br#"<svg width="20" height="20" viewBox="0 0 10 10"><rect x="2" y="2" width="4" height="4" fill="black"/></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!((image.width(), image.height()), (20, 20));
        assert_eq!(image.pixel(4, 4), Some(Color::rgb(0, 0, 0)));
        assert_eq!(image.pixel(11, 11), Some(Color::rgb(0, 0, 0)));
        // The unscaled 2..6 rect would have covered pixels 2..5 instead.
        assert_eq!(image.pixel(2, 2), Some(Color::rgba(0, 0, 0, 0)));
        assert_eq!(image.pixel(12, 12), Some(Color::rgba(0, 0, 0, 0)));
    }

    /// The viewBox origin is part of the mapping: a non-zero `min-x`/`min-y`
    /// shifts the whole user space, not just the scale.
    #[test]
    fn view_box_origin_translates_user_space() {
        let svg = br#"<svg width="10" height="10" viewBox="5 5 10 10"><rect x="5" y="5" width="10" height="10" fill="black"/></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(0, 0), Some(Color::rgb(0, 0, 0)));
        assert_eq!(image.pixel(9, 9), Some(Color::rgb(0, 0, 0)));
    }

    /// A caller that already knows the device-pixel size of its page box
    /// rasterises at that size; the viewBox still supplies the user-space
    /// mapping, so the geometry scales to fill the given viewport.
    #[test]
    fn explicit_viewport_overrides_the_root_size() {
        let svg = br#"<svg width="20" height="20" viewBox="0 0 10 10"><rect width="10" height="10" fill="black"/></svg>"#;
        let image = decode_svg_viewport(svg, 40, 20, Color::BLACK, ImageLimits::default())
            .expect("decodes");
        assert_eq!((image.width(), image.height()), (40, 20));
        // 2:1 stretch of the 1:1 viewBox, so the whole viewport is covered.
        assert_eq!(image.pixel(0, 0), Some(Color::rgb(0, 0, 0)));
        assert_eq!(image.pixel(39, 19), Some(Color::rgb(0, 0, 0)));
    }

    #[test]
    fn fill_inherits_through_group_transform() {
        let svg = br##"<svg width="10" height="10"><g fill="#00f" transform="translate(2 2)"><rect width="3" height="3"/></g></svg>"##;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(3, 3), Some(Color::rgb(0, 0, 255)));
        assert_eq!(image.pixel(1, 1), Some(Color::rgba(0, 0, 0, 0)));
    }

    /// The initial value of `fill` is black, so a shape with no fill anywhere
    /// above it is filled. The rasteriser used to start the root with "no
    /// inherited paint", which left such a shape unpainted.
    #[test]
    fn a_shape_with_no_fill_anywhere_is_filled_black() {
        let svg = br#"<svg width="10" height="10"><rect width="4" height="4"/></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(1, 1), Some(Color::rgb(0, 0, 0)));
        assert_eq!(image.pixel(6, 6), Some(Color::rgba(0, 0, 0, 0)));
    }

    /// A root `fill="none"` is inherited by the shapes below it. Before the
    /// root's own attributes were read, this shape was filled black.
    #[test]
    fn a_root_fill_none_is_inherited_by_its_children() {
        let svg = br#"<svg width="10" height="10" fill="none" stroke="red"><rect width="4" height="4"/><line x1="6" y1="1.5" x2="8" y2="1.5"/></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(2, 2), Some(Color::rgba(0, 0, 0, 0)));
        assert_eq!(image.pixel(7, 1), Some(Color::rgb(255, 0, 0)));
    }

    /// `currentColor` takes the colour the caller supplies, for fill and for
    /// stroke alike, and not a fixed black.
    #[test]
    fn current_color_resolves_to_the_callers_colour() {
        let svg = br#"<svg width="8" height="8"><rect width="4" height="8" fill="currentColor"/><line x1="5" y1="1.5" x2="7" y2="1.5" stroke="CurrentColor" stroke-width="1"/></svg>"#;
        let green = Color::rgb(0, 128, 0);
        let image = decode_svg_viewport(svg, 8, 8, green, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(1, 1), Some(green));
        assert_eq!(image.pixel(6, 1), Some(green));
        assert_eq!(image.pixel(6, 5), Some(Color::rgba(0, 0, 0, 0)));
    }

    #[test]
    fn default_viewport_is_300x150_without_size_or_viewbox() {
        let svg = br#"<svg><rect width="10" height="10" fill="red"/></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!((image.width(), image.height()), (300, 150));
    }

    #[test]
    fn parse_path_handles_relative_and_mixed_numbers() {
        let subpaths = parse_path("M1 2L3 4m-1-1H0V5z");
        assert_eq!(subpaths.len(), 2);
        let (first, first_closed) = &subpaths[0];
        assert_eq!(first[0], (1.0, 2.0));
        assert_eq!(first[1], (3.0, 4.0));
        assert!(!*first_closed);
        let (second, second_closed) = &subpaths[1];
        assert_eq!(second[0], (2.0, 3.0));
        // z appends the subpath start point as the closing vertex.
        assert_eq!(second.last(), Some(&(2.0, 3.0)));
        assert_eq!(second[second.len() - 2], (0.0, 5.0));
        assert!(*second_closed);
    }

    #[test]
    fn parse_path_flattens_an_elliptical_arc_off_the_chord() {
        // A semicircle from (0,5) to (10,5): the old arc arm dropped every
        // point except the endpoint, so the shape became a straight chord.
        let subpaths = parse_path("M0 5 A5 5 0 0 1 10 5");
        assert_eq!(subpaths.len(), 1);
        let points = &subpaths[0].0;
        assert!(
            points.len() > 4,
            "the arc should be sampled, got {points:?}"
        );
        assert!(
            points.iter().any(|(_, y)| (*y - 5.0).abs() > 2.0),
            "the arc leaves the chord: {points:?}"
        );
        let (x, y) = *points.last().expect("endpoint");
        assert!((x - 10.0).abs() < 0.01 && (y - 5.0).abs() < 0.01);
    }

    #[test]
    fn linear_gradient_interpolates_along_its_axis() {
        let svg = br##"<svg width="8" height="1"><defs>
            <linearGradient id="g" x1="0" y1="0" x2="8" y2="0" gradientUnits="userSpaceOnUse">
              <stop stop-color="#ff0000"/><stop offset="1" stop-color="#0000ff"/>
            </linearGradient></defs>
            <rect width="8" height="1" fill="url(#g)"/></svg>"##;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        let left = image.pixel(0, 0).expect("left pixel");
        let right = image.pixel(7, 0).expect("right pixel");
        assert!(left.red > 200 && left.blue < 60, "{left:?}");
        assert!(right.blue > 200 && right.red < 60, "{right:?}");
        assert_eq!(left.alpha, 255);
        assert_eq!(right.alpha, 255);
    }

    #[test]
    fn a_gradient_transform_moves_the_gradient_coordinate_system() {
        let svg = br#"<svg width="4" height="1"><defs>
            <linearGradient id="g" x1="0" y1="0" x2="1" y2="0"
              gradientUnits="userSpaceOnUse" gradientTransform="translate(2 0)">
              <stop stop-color="red"/><stop offset="1" stop-color="blue"/>
            </linearGradient></defs>
            <rect width="4" height="1" fill="url(#g)"/></svg>""#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(0, 0), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(3, 0), Some(Color::rgb(0, 0, 255)));
    }

    #[test]
    fn a_gradient_stroke_uses_the_same_coordinate_system() {
        let svg = br#"<svg width="8" height="8"><defs>
            <linearGradient id="g" x1="0" y1="0" x2="8" y2="0" gradientUnits="userSpaceOnUse">
              <stop stop-color="red"/><stop offset="1" stop-color="blue"/>
            </linearGradient></defs>
            <line x1="0" y1="4" x2="8" y2="4" stroke="url(#g)" stroke-width="1"/>
            </svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        let left = image.pixel(0, 4).expect("left stroke pixel");
        let right = image.pixel(7, 4).expect("right stroke pixel");
        assert!(left.red > 200 && left.blue < 80, "{left:?}");
        assert!(right.blue > 200 && right.red < 80, "{right:?}");
    }

    #[test]
    fn radial_gradient_stop_opacity_blends_towards_the_edge() {
        let svg = br#"<svg width="9" height="9"><defs>
            <radialGradient id="g" cx="0.5" cy="0.5" r="0.5">
              <stop stop-color="red"/><stop offset="1" stop-color="red" stop-opacity="0"/>
            </radialGradient></defs>
            <rect width="9" height="9" fill="url(#g)"/></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(4, 4), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(0, 0), Some(Color::TRANSPARENT));
    }

    #[test]
    fn a_clip_path_limits_fill_and_stroke() {
        let svg = br#"<svg width="10" height="10"><defs>
            <clipPath id="c"><rect width="5" height="10"/></clipPath></defs>
            <g clip-path="url(#c)"><rect width="10" height="10" fill="red"/>
            <line x1="0" y1="5.5" x2="10" y2="5.5" stroke="blue"/></g></svg>""#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(2, 2), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(7, 2), Some(Color::TRANSPARENT));
        assert_eq!(image.pixel(2, 5), Some(Color::rgb(0, 0, 255)));
        assert_eq!(image.pixel(7, 5), Some(Color::TRANSPARENT));
    }

    #[test]
    fn object_bounding_box_clip_path_maps_to_the_element_bounds() {
        let svg = br#"<svg width="10" height="10"><defs>
            <clipPath id="c" clipPathUnits="objectBoundingBox">
              <rect width="0.5" height="1"/></clipPath></defs>
            <rect x="2" y="2" width="6" height="6" fill="red" clip-path="url(#c)"/></svg>""#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(2, 2), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(4, 4), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(6, 4), Some(Color::TRANSPARENT));
    }

    #[test]
    fn object_bounding_box_units_on_a_group_use_descendant_bounds() {
        let svg = br#"<svg width="10" height="10"><defs>
            <clipPath id="c" clipPathUnits="objectBoundingBox">
              <rect width="0.5" height="1"/></clipPath></defs>
            <g clip-path="url(#c)"><rect x="2" y="2" width="6" height="6" fill="red"/></g>
            </svg>""#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(2, 2), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(4, 4), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(6, 4), Some(Color::TRANSPARENT));
    }

    #[test]
    fn a_transform_on_the_clip_path_moves_the_clip_region() {
        let svg = br#"<svg width="10" height="10"><defs>
            <clipPath id="c" transform="translate(5 0)">
              <rect width="5" height="10"/></clipPath></defs>
            <rect width="10" height="10" fill="red" clip-path="url(#c)"/></svg>""#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(2, 2), Some(Color::TRANSPARENT));
        assert_eq!(image.pixel(7, 2), Some(Color::rgb(255, 0, 0)));
    }

    #[test]
    fn nested_clip_paths_intersect() {
        let svg = br#"<svg width="10" height="10"><defs>
            <clipPath id="left"><rect width="8" height="10"/></clipPath>
            <clipPath id="right"><rect x="3" width="7" height="10"/></clipPath></defs>
            <g clip-path="url(#left)"><g clip-path="url(#right)">
              <rect width="10" height="10" fill="red"/></g></g></svg>""#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(2, 2), Some(Color::TRANSPARENT));
        assert_eq!(image.pixel(4, 2), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(8, 2), Some(Color::TRANSPARENT));
    }

    #[test]
    fn the_t5_rewards_medal_clip_path_keeps_the_icon_visible() {
        // The template Bing's Rewards widget inserts for the T5 experiment.
        // Its whole drawing is inside `<g clip-path="url(#clip0_...)">`; a
        // clip that is parsed but empties the group would erase the medal.
        let image = decode_svg_viewport(
            T5_MEDAL.as_bytes(),
            32,
            32,
            Color::WHITE,
            ImageLimits::default(),
        )
        .expect("T5 medal decodes");
        let mut visible = 0;
        let mut white = 0;
        for y in 0..32 {
            for x in 0..32 {
                if let Some(pixel) = image.pixel(x, y)
                    && pixel.alpha > 0
                {
                    visible += 1;
                    if pixel.red > 200 && pixel.green > 200 && pixel.blue > 200 {
                        white += 1;
                    }
                }
            }
        }
        assert!(visible > 150, "only {visible} visible pixels");
        assert!(white > 50, "only {white} white pixels");
    }

    #[test]
    fn use_renders_a_referenced_shape_at_its_x_and_y() {
        let svg = br##"<svg width="10" height="10"><defs>
            <path id="p" d="M0 0h5v5H0z" fill="red"/></defs>
            <use href="#p" x="3" y="2"/></svg>"##;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(4, 3), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(1, 1), Some(Color::TRANSPARENT));
    }

    #[test]
    fn use_accepts_the_legacy_xlink_href_spelling() {
        let svg = br##"<svg width="10" height="10"><defs>
            <rect id="r" width="4" height="4" fill="blue"/></defs>
            <use xlink:href="#r" x="2" y="2"/></svg>"##;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(3, 3), Some(Color::rgb(0, 0, 255)));
    }

    #[test]
    fn use_passes_its_paint_to_the_referenced_shape() {
        let svg = br##"<svg width="10" height="10"><defs>
            <rect id="r" width="4" height="4"/></defs>
            <use href="#r" fill="#00ff00"/></svg>"##;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(1, 1), Some(Color::rgb(0, 255, 0)));
    }

    #[test]
    fn use_scales_a_symbol_view_box_to_its_width_and_height() {
        let svg = br##"<svg width="20" height="20"><defs>
            <symbol id="s" viewBox="0 0 10 10"><rect width="10" height="10" fill="red"/></symbol>
            </defs><use href="#s" width="20" height="20"/></svg>"##;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(15, 15), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(2, 2), Some(Color::rgb(255, 0, 0)));
    }

    /// A `<symbol>` is a definition, not a shape: it has no direct rendering,
    /// and only a `<use>` gives it a viewport and paints its contents.
    #[test]
    fn a_symbol_is_invisible_until_use_references_it() {
        let definition_only = br#"<svg width="20" height="20"><defs>
            <symbol id="s" viewBox="0 0 10 10"><rect width="10" height="10" fill="red"/></symbol>
            </defs></svg>"#;
        let image = decode_svg(definition_only, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(5, 5), Some(Color::TRANSPARENT));
        assert_eq!(image.pixel(15, 15), Some(Color::TRANSPARENT));

        let used = br##"<svg width="20" height="20"><defs>
            <symbol id="s" viewBox="0 0 10 10"><rect width="10" height="10" fill="red"/></symbol>
            </defs><use href="#s" x="10" y="5" width="10" height="10"/></svg>"##;
        let image = decode_svg(used, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(15, 10), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(5, 5), Some(Color::TRANSPARENT));
        assert_eq!(image.pixel(15, 18), Some(Color::TRANSPARENT));
    }

    /// The use element contributes the outer translation and the inherited
    /// paint; the referenced group keeps its own transform. A property the
    /// target sets itself is not overridden by the use wrapper.
    #[test]
    fn use_composes_the_target_transform_and_inherits_paint_from_the_use_wrapper() {
        let svg = br##"<svg width="20" height="20"><defs>
            <g id="g" transform="translate(2 3)"><rect width="4" height="4"/></g>
            </defs><use href="#g" x="5" y="1" fill="#00ff00"/></svg>"##;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(8, 5), Some(Color::rgb(0, 255, 0)));
        assert_eq!(image.pixel(6, 5), Some(Color::TRANSPARENT));
        assert_eq!(image.pixel(12, 5), Some(Color::TRANSPARENT));

        let target_paint = br##"<svg width="10" height="10"><defs>
            <rect id="r" width="4" height="4" style="fill:#0000ff"/></defs>
            <use href="#r" fill="#00ff00"/></svg>"##;
        let image = decode_svg(target_paint, ImageLimits::default()).expect("decodes");
        assert_eq!(
            image.pixel(1, 1),
            Some(Color::rgb(0, 0, 255)),
            "an explicitly filled target must keep its paint"
        );

        let explicit_inherit = br##"<svg width="10" height="10"><defs>
            <g id="g" fill="inherit"><rect width="4" height="4"/></g></defs>
            <use href="#g" fill="#00ff00"/></svg>"##;
        let image = decode_svg(explicit_inherit, ImageLimits::default()).expect("decodes");
        assert_eq!(
            image.pixel(1, 1),
            Some(Color::rgb(0, 255, 0)),
            "fill=inherit takes the use wrapper's paint"
        );
    }

    /// `<use width height>` establishes the generated viewport, the symbol's
    /// viewBox maps into it with the default xMidYMid meet alignment, and the
    /// viewport clips geometry the symbol draws outside the viewBox.
    #[test]
    fn use_clips_a_symbol_to_its_generated_viewport_and_centres_the_view_box() {
        let svg = br##"<svg width="30" height="10"><defs>
            <symbol id="s" viewBox="0 0 10 10"><rect width="20" height="10" fill="red"/></symbol>
            </defs><use href="#s" x="5" width="20" height="10"/></svg>"##;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        // 1:1 meet scaling inside a 20x10 viewport leaves 5 units on each
        // side; the symbol's oversized rect therefore starts at x=10.
        assert_eq!(image.pixel(7, 5), Some(Color::TRANSPARENT));
        assert_eq!(image.pixel(24, 5), Some(Color::rgb(255, 0, 0)));
        // The generated viewport ends at x=25; the rect would otherwise reach
        // x=30 because it is wider than the symbol's viewBox.
        assert_eq!(image.pixel(26, 5), Some(Color::TRANSPARENT));
    }

    #[test]
    fn preserve_aspect_ratio_none_stretches_the_symbol_view_box() {
        let svg = br##"<svg width="10" height="10"><defs>
            <symbol id="s" viewBox="0 0 10 5" preserveAspectRatio="none">
              <rect width="10" height="5" fill="red"/></symbol>
            </defs><use href="#s" width="10" height="10"/></svg>"##;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(5, 1), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(5, 9), Some(Color::rgb(255, 0, 0)));
    }

    #[test]
    fn nested_use_resolves_against_the_original_id_map_for_every_reference() {
        let svg = br##"<svg width="20" height="20"><defs>
            <rect id="r" width="2" height="2" fill="red"/>
            <g id="g"><use href="#r" x="4"/></g>
            </defs><use href="#g" x="2" y="3"/><use href="#g" x="10" y="10"/></svg>"##;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(6, 3), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(14, 10), Some(Color::rgb(255, 0, 0)));
    }

    /// A style declaration is a CSS property and wins over the presentation
    /// attribute; it applies to inherited paint and to `currentColor`.
    #[test]
    fn use_honours_style_presentation_and_current_color() {
        let style_on_use = br##"<svg width="10" height="10"><defs>
            <rect id="r" width="4" height="4"/></defs>
            <use href="#r" style="fill:#00ff00"/></svg>"##;
        let image = decode_svg(style_on_use, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(1, 1), Some(Color::rgb(0, 255, 0)));

        let current_color = br##"<svg width="10" height="10"><defs>
            <rect id="r" width="4" height="4"/></defs>
            <use href="#r" fill="currentColor" style="color:#0000ff"/></svg>"##;
        let image = decode_svg(current_color, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(1, 1), Some(Color::rgb(0, 0, 255)));

        let target_color = br##"<svg width="10" height="10"><defs>
            <g id="g" color="#0000ff"><rect width="4" height="4" fill="currentColor"/></g>
            </defs><use href="#g"/></svg>"##;
        let image = decode_svg(target_color, ImageLimits::default()).expect("decodes");
        assert_eq!(
            image.pixel(1, 1),
            Some(Color::rgb(0, 0, 255)),
            "currentColor follows the target's own inherited color"
        );
    }

    #[test]
    fn a_self_referential_use_is_stopped_with_a_cycle_diagnostic() {
        let svg = br##"<svg width="10" height="10"><defs>
            <g id="loop"><use href="#loop"/></g></defs>
            <use href="#loop"/></svg>"##;
        let (image, diagnostics) =
            decode_svg_with_diagnostics(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(5, 5), Some(Color::TRANSPARENT));
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].code,
            SvgRenderDiagnosticCode::UnsupportedReference
        );
        assert!(
            diagnostics[0].message.contains("cycle"),
            "{}",
            diagnostics[0].message
        );
    }

    #[test]
    fn deeply_nested_use_is_capped_with_a_diagnostic() {
        let mut svg = String::from("<svg width=\"10\" height=\"10\"><defs>");
        for level in 1..=20 {
            write!(
                svg,
                "<g id=\"g{level}\"><use href=\"#g{}\"/></g>",
                level + 1
            )
            .expect("writing to a String cannot fail");
        }
        svg.push_str("<rect id=\"g21\" width=\"10\" height=\"10\" fill=\"red\"/></defs>");
        svg.push_str("<use href=\"#g1\"/></svg>");
        let (image, diagnostics) =
            decode_svg_with_diagnostics(svg.as_bytes(), ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(5, 5), Some(Color::TRANSPARENT));
        assert!(!diagnostics.is_empty());
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == SvgRenderDiagnosticCode::UnsupportedReference)
        );
    }

    /// An external reference must not fall back to a same-named local id and
    /// must not paint anything; the diagnostic is the whole result for that
    /// use element.
    #[test]
    fn an_external_use_reference_is_reported_and_paints_nothing() {
        let svg = br#"<svg width="10" height="10"><defs>
            <rect id="icon" width="10" height="10" fill="red"/></defs>
            <use href="https://example.com/icons.svg#icon"/></svg>"#;
        let (image, diagnostics) =
            decode_svg_with_diagnostics(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(5, 5), Some(Color::TRANSPARENT));
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].code,
            SvgRenderDiagnosticCode::UnsupportedReference
        );
        assert!(
            diagnostics[0].message.contains("external-document"),
            "{}",
            diagnostics[0].message
        );
    }

    #[test]
    fn a_missing_use_target_is_reported_and_paints_nothing() {
        let svg = br##"<svg width="10" height="10">
            <rect width="2" height="2" fill="red"/><use href="#missing"/></svg>"##;
        let (image, diagnostics) =
            decode_svg_with_diagnostics(svg, ImageLimits::default()).expect("decodes");
        // The rest of the document is unaffected by the broken reference.
        assert_eq!(image.pixel(1, 1), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(5, 5), Some(Color::TRANSPARENT));
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].code,
            SvgRenderDiagnosticCode::UnsupportedReference
        );
    }

    #[test]
    fn an_alpha_mask_limits_the_masked_element() {
        let svg = br#"<svg width="10" height="10"><defs>
            <mask id="m" mask-type="alpha"><rect width="5" height="10" fill="white"/></mask>
            </defs><rect width="10" height="10" fill="red" mask="url(#m)"/></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(2, 2), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(7, 2), Some(Color::TRANSPARENT));
    }

    #[test]
    fn a_luminance_mask_uses_the_mask_content_brightness() {
        let svg = br#"<svg width="10" height="10"><defs>
            <mask id="m"><rect width="5" height="10" fill="white"/>
              <rect x="5" width="5" height="10" fill="black"/></mask>
            </defs><rect width="10" height="10" fill="red" mask="url(#m)"/></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(2, 2), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(7, 2), Some(Color::TRANSPARENT));
    }

    #[test]
    fn a_group_mask_composites_the_whole_group() {
        let svg = br#"<svg width="10" height="10"><defs>
            <mask id="m" mask-type="alpha"><rect width="5" height="10" fill="white"/></mask>
            </defs><g mask="url(#m)"><rect width="10" height="10" fill="red"/>
            <rect x="0" y="5" width="10" height="5" fill="blue"/></g></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(2, 2), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(2, 7), Some(Color::rgb(0, 0, 255)));
        assert_eq!(image.pixel(7, 2), Some(Color::TRANSPARENT));
    }

    #[test]
    fn mask_content_units_can_use_the_element_bounding_box() {
        let svg = br#"<svg width="10" height="10"><defs>
            <mask id="m" mask-type="alpha" maskContentUnits="objectBoundingBox">
              <rect width="0.5" height="1" fill="white"/></mask>
            </defs><rect x="2" y="2" width="6" height="6" fill="red" mask="url(#m)"/></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(2, 2), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(4, 4), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(6, 4), Some(Color::TRANSPARENT));
    }

    #[test]
    fn a_luminance_mask_preserves_partial_alpha() {
        let svg = br##"<svg width="4" height="4"><defs>
            <mask id="m"><rect width="4" height="4" fill="#808080"/></mask>
            </defs><rect width="4" height="4" fill="red" mask="url(#m)"/></svg>"##;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        let pixel = image.pixel(2, 2).expect("masked pixel");
        assert!(pixel.alpha > 100 && pixel.alpha < 200, "{pixel:?}");
        assert!(pixel.red > 200, "{pixel:?}");
    }

    #[test]
    fn a_gaussian_blur_spreads_alpha_beyond_the_source_shape() {
        let svg = br#"<svg width="20" height="20"><defs>
            <filter id="f"><feGaussianBlur stdDeviation="1"/></filter></defs>
            <rect x="8" y="8" width="4" height="4" fill="red" filter="url(#f)"/></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        let centre = image.pixel(10, 10).expect("centre pixel");
        let edge = image.pixel(8, 10).expect("blurred edge pixel");
        assert_eq!(centre.red, 255);
        assert!(centre.alpha > 180, "centre alpha={}", centre.alpha);
        assert!(edge.alpha > 0, "blur did not extend the shape edge");
        assert!(edge.alpha < 255, "edge should be partially transparent");
        assert_eq!(
            image.pixel(2, 2).map(|pixel| pixel.alpha),
            Some(0),
            "the filter region should clip the far edge"
        );
    }

    #[test]
    fn a_blur_duplicates_the_input_edge_instead_of_fading_to_transparent() {
        let svg = br#"<svg width="12" height="12"><defs>
            <filter id="f" filterUnits="userSpaceOnUse" x="0" y="0" width="12" height="12">
              <feGaussianBlur stdDeviation="2"/></filter></defs>
            <rect width="12" height="12" fill="red" filter="url(#f)"/></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(0, 0), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(6, 6), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(11, 11), Some(Color::rgb(255, 0, 0)));
    }

    #[test]
    fn a_saturate_color_matrix_desaturates_the_source() {
        let svg = br#"<svg width="10" height="10"><defs>
            <filter id="f"><feColorMatrix type="saturate" values="0"/></filter></defs>
            <rect x="2" y="2" width="6" height="6" fill="red" filter="url(#f)"/></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        let pixel = image.pixel(4, 4).expect("filtered pixel");
        assert_eq!(pixel.alpha, 255);
        assert!((i16::from(pixel.red) - i16::from(pixel.green)).abs() <= 1);
        assert!((i16::from(pixel.green) - i16::from(pixel.blue)).abs() <= 1);
        assert!(pixel.red < 100, "{pixel:?}");
    }

    #[test]
    fn a_color_matrix_can_swap_channels() {
        let svg = br#"<svg width="10" height="10"><defs>
            <filter id="f"><feColorMatrix type="matrix"
              values="0 1 0 0 0  1 0 0 0 0  0 0 1 0 0  0 0 0 1 0"/></filter></defs>
            <rect x="2" y="2" width="6" height="6" fill="red" filter="url(#f)"/></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(4, 4), Some(Color::rgb(0, 255, 0)));
    }

    #[test]
    fn a_grayscale_color_matrix_produces_exact_luminance_pixels() {
        let svg = br#"<svg width="10" height="10"><defs>
            <filter id="f"><feColorMatrix type="saturate" values="0"/></filter></defs>
            <rect x="2" y="2" width="6" height="6" fill="red" filter="url(#f)"/></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(4, 4), Some(Color::rgb(54, 54, 54)));
    }

    #[test]
    fn an_invert_color_matrix_produces_exact_known_pixels() {
        let svg = br#"<svg width="10" height="10"><defs>
            <filter id="f"><feColorMatrix type="matrix"
              values="-1 0 0 0 1  0 -1 0 0 1  0 0 -1 0 1  0 0 0 1 0"/></filter></defs>
            <rect x="2" y="2" width="6" height="6" fill="red" filter="url(#f)"/></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(4, 4), Some(Color::rgb(0, 255, 255)));
        assert_eq!(image.pixel(1, 1), Some(Color::TRANSPARENT));
    }

    #[test]
    fn fill_rule_evenodd_cuts_a_hole_in_a_multi_subpath_path() {
        let evenodd = br#"<svg width="8" height="8" xmlns="http://www.w3.org/2000/svg">
            <path fill="red" fill-rule="evenodd" d="M0 0H8V8H0Z M2 2H6V6H2Z"/></svg>"#;
        let nonzero = br#"<svg width="8" height="8" xmlns="http://www.w3.org/2000/svg">
            <path fill="red" d="M0 0H8V8H0Z M2 2H6V6H2Z"/></svg>"#;
        let evenodd = decode_svg(evenodd, ImageLimits::default()).expect("evenodd decodes");
        let nonzero = decode_svg(nonzero, ImageLimits::default()).expect("nonzero decodes");
        assert_eq!(evenodd.pixel(1, 1), Some(Color::rgb(255, 0, 0)));
        assert_eq!(
            evenodd.pixel(4, 4),
            Some(Color::TRANSPARENT),
            "the inner subpath is a hole under evenodd"
        );
        assert_eq!(
            nonzero.pixel(4, 4),
            Some(Color::rgb(255, 0, 0)),
            "the same-direction inner subpath stays filled under nonzero"
        );
    }

    #[test]
    fn fill_rule_is_inherited_from_a_group() {
        let svg = br#"<svg width="8" height="8"><g fill-rule="evenodd">
            <path fill="red" d="M0 0H8V8H0Z M2 2H6V6H2Z"/></g></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(1, 1), Some(Color::rgb(255, 0, 0)));
        assert_eq!(
            image.pixel(4, 4),
            Some(Color::TRANSPARENT),
            "the descendant path inherits the group's evenodd fill rule"
        );
    }

    #[test]
    fn small_shapes_have_antialiased_edge_coverage() {
        let svg = br##"<svg width="24" height="24">
            <circle cx="12" cy="12" r="10" fill="#1772f6"/></svg>"##;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        let mut partial = 0;
        let mut opaque = 0;
        for y in 0..24 {
            for x in 0..24 {
                let alpha = image.pixel(x, y).expect("inside image").alpha;
                if alpha == 255 {
                    opaque += 1;
                } else if alpha > 0 {
                    partial += 1;
                }
            }
        }
        assert!(opaque > 100, "the circle interior stays opaque");
        assert!(
            partial > 20,
            "a 24px circle must have antialiased edge pixels, found {partial}"
        );
    }

    #[test]
    fn fill_and_stroke_opacity_are_honoured() {
        let svg = br#"<svg width="8" height="12">
            <g fill-opacity="0.5"><rect width="8" height="6" fill="red"/></g>
            <line x1="0" y1="10.5" x2="8" y2="10.5" stroke="blue"
                  stroke-width="2" stroke-opacity="0.5"/></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(4, 3).expect("fill pixel").alpha, 128);
        assert_eq!(
            image.pixel(4, 10).expect("stroke pixel"),
            Color::rgba(0, 0, 255, 128)
        );
    }

    #[test]
    fn stroke_width_scales_with_the_transform() {
        let svg = br#"<svg width="16" height="16">
            <g transform="scale(2)"><line x1="1" y1="4" x2="7" y2="4"
                stroke="black" stroke-width="1"/></g></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        // A 1-unit stroke under scale(2) covers two device rows, not one.
        assert!(image.pixel(4, 7).expect("row above").alpha > 0);
        assert!(image.pixel(4, 8).expect("row below").alpha > 0);
        assert_eq!(image.pixel(4, 6).expect("outside").alpha, 0);
        assert_eq!(image.pixel(4, 9).expect("outside").alpha, 0);
    }

    #[test]
    fn clip_rule_evenodd_cuts_a_hole_while_nonzero_fills_it() {
        let evenodd = br#"<svg width="10" height="10"><defs>
            <clipPath id="c" clip-rule="evenodd"><path d="M0 0H10V10H0Z M3 3H7V7H3Z"/></clipPath>
            </defs><rect width="10" height="10" fill="red" clip-path="url(#c)"/></svg>"#;
        let nonzero = br#"<svg width="10" height="10"><defs>
            <clipPath id="c"><path d="M0 0H10V10H0Z M3 3H7V7H3Z"/></clipPath>
            </defs><rect width="10" height="10" fill="red" clip-path="url(#c)"/></svg>"#;
        let evenodd = decode_svg(evenodd, ImageLimits::default()).expect("evenodd decodes");
        let nonzero = decode_svg(nonzero, ImageLimits::default()).expect("nonzero decodes");
        assert_eq!(evenodd.pixel(1, 1), Some(Color::rgb(255, 0, 0)));
        assert_eq!(evenodd.pixel(5, 5), Some(Color::TRANSPARENT));
        assert_eq!(nonzero.pixel(5, 5), Some(Color::rgb(255, 0, 0)));
    }

    #[test]
    fn clip_rule_in_the_style_attribute_is_honoured() {
        let svg = br#"<svg width="10" height="10"><defs>
            <clipPath id="c" style="clip-rule: evenodd">
              <path d="M0 0H10V10H0Z M3 3H7V7H3Z"/></clipPath>
            </defs><rect width="10" height="10" fill="red" clip-path="url(#c)"/></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(5, 5), Some(Color::TRANSPARENT));
    }

    #[test]
    fn group_opacity_composites_overlapping_children_once() {
        let svg = br#"<svg width="20" height="10"><g opacity="0.5">
            <rect width="10" height="10" fill="red"/><rect x="5" width="10" height="10" fill="red"/>
            </g></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        let overlap = image.pixel(7, 5).expect("overlap pixel");
        assert_eq!(overlap.red, 255);
        assert!(
            (128..=128).contains(&overlap.alpha),
            "group opacity overlap alpha={}",
            overlap.alpha
        );
        assert_eq!(image.pixel(1, 5).expect("left pixel").alpha, 128);
    }

    #[test]
    fn nested_group_opacity_multiplies() {
        let svg = br#"<svg width="10" height="10"><g opacity="0.5"><g opacity="0.5">
            <rect width="10" height="10" fill="red"/></g></g></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        let pixel = image.pixel(5, 5).expect("pixel");
        assert!(
            (63..=65).contains(&pixel.alpha),
            "nested opacity alpha={}",
            pixel.alpha
        );
    }

    #[test]
    fn group_opacity_combines_with_a_mask() {
        let svg = br#"<svg width="20" height="10"><defs>
            <mask id="m" mask-type="alpha"><rect width="10" height="10" fill="white"/></mask>
            </defs><g opacity="0.5" mask="url(#m)"><rect width="20" height="10" fill="red"/></g></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert!(
            (127..=129).contains(&image.pixel(2, 5).expect("masked pixel").alpha),
            "masked alpha={}",
            image.pixel(2, 5).expect("pixel").alpha
        );
        assert_eq!(image.pixel(15, 5).expect("outside pixel").alpha, 0);
    }

    #[test]
    fn group_opacity_composes_with_a_filter() {
        let svg = br#"<svg width="20" height="20"><defs>
            <filter id="f"><feGaussianBlur stdDeviation="1"/></filter></defs>
            <g opacity="0.5" filter="url(#f)"><rect x="8" y="8" width="4" height="4" fill="red"/></g>
            </svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        let pixel = image.pixel(10, 10).expect("pixel");
        assert!(pixel.alpha > 80 && pixel.alpha < 200, "{pixel:?}");
    }

    #[test]
    fn group_opacity_in_the_style_attribute_is_honoured() {
        let svg = br#"<svg width="10" height="10"><g style="opacity: 0.5">
            <rect width="10" height="10" fill="red"/></g></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(5, 5).expect("pixel").alpha, 128);
    }

    #[test]
    fn fe_offset_moves_the_source() {
        let svg = br#"<svg width="20" height="20"><defs>
            <filter id="f" filterUnits="userSpaceOnUse" x="0" y="0" width="20" height="20">
              <feOffset dx="5" dy="2"/></filter></defs>
            <rect x="2" y="2" width="5" height="5" fill="red" filter="url(#f)"/></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(3, 3), Some(Color::TRANSPARENT));
        assert_eq!(image.pixel(8, 5), Some(Color::rgb(255, 0, 0)));
    }

    #[test]
    fn fe_offset_moves_a_shape_without_changing_its_coverage() {
        let plain = br#"<svg width="20" height="20">
            <rect x="2" y="2" width="5" height="5" fill="red"/></svg>"#;
        let filtered = br#"<svg width="20" height="20"><defs>
            <filter id="f" filterUnits="userSpaceOnUse" x="0" y="0" width="20" height="20">
              <feOffset dx="5" dy="2"/></filter></defs>
            <rect x="2" y="2" width="5" height="5" fill="red" filter="url(#f)"/></svg>"#;
        let plain = decode_svg(plain, ImageLimits::default()).expect("decodes");
        let filtered = decode_svg(filtered, ImageLimits::default()).expect("decodes");
        let covered = |image: &super::DecodedImage| {
            image
                .pixels()
                .iter()
                .filter(|pixel| pixel.alpha > 0)
                .count()
        };
        assert_eq!(covered(&plain), 25);
        assert_eq!(covered(&filtered), covered(&plain));
        assert_eq!(filtered.pixel(3, 3), Some(Color::TRANSPARENT));
        assert_eq!(filtered.pixel(8, 5), Some(Color::rgb(255, 0, 0)));
    }

    #[test]
    fn primitive_units_object_bounding_box_scales_offset_distances() {
        let svg = br#"<svg width="20" height="20"><defs>
            <filter id="f" filterUnits="userSpaceOnUse" x="0" y="0" width="20" height="20"
              primitiveUnits="objectBoundingBox">
              <feOffset dx="0.5" dy="0.25"/></filter></defs>
            <rect x="2" y="2" width="8" height="8" fill="red" filter="url(#f)"/></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(5, 4), Some(Color::TRANSPARENT));
        assert_eq!(image.pixel(6, 4), Some(Color::rgb(255, 0, 0)));
        assert_eq!(
            image
                .pixels()
                .iter()
                .filter(|pixel| pixel.alpha > 0)
                .count(),
            64
        );
    }

    #[test]
    fn fe_composite_over_combines_named_offset_and_source() {
        let svg = br#"<svg width="20" height="20"><defs>
            <filter id="f" filterUnits="userSpaceOnUse" x="0" y="0" width="20" height="20">
              <feOffset dx="5" result="shifted"/>
              <feComposite in="SourceGraphic" in2="shifted" operator="over"/>
            </filter></defs>
            <rect x="2" y="2" width="5" height="5" fill="red" filter="url(#f)"/></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(3, 3), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(8, 5), Some(Color::rgb(255, 0, 0)));
    }

    #[test]
    fn fe_blend_normal_uses_the_named_colored_result() {
        let svg = br#"<svg width="20" height="20"><defs>
            <filter id="f" filterUnits="userSpaceOnUse" x="0" y="0" width="20" height="20">
              <feOffset dx="5" result="shifted"/>
              <feColorMatrix in="shifted" type="matrix"
                values="0 0 1 0 0  0 1 0 0 0  1 0 0 0 0  0 0 0 1 0" result="blueShifted"/>
              <feBlend in="SourceGraphic" in2="blueShifted" mode="normal"/>
            </filter></defs>
            <rect x="2" y="2" width="5" height="5" fill="red" filter="url(#f)"/></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(3, 3), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(8, 5), Some(Color::rgb(0, 0, 255)));
    }

    #[test]
    fn fe_blend_multiply_uses_the_two_inputs_and_keeps_the_backdrop_elsewhere() {
        let svg = br##"<svg width="10" height="10"><defs>
            <filter id="f" filterUnits="userSpaceOnUse" x="0" y="0" width="10" height="10">
              <feFlood flood-color="#808080" result="backdrop"/>
              <feBlend in="SourceGraphic" in2="backdrop" mode="multiply"/>
            </filter></defs>
            <rect x="2" y="2" width="2" height="2" fill="red" filter="url(#f)"/></svg>"##;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(2, 2), Some(Color::rgb(128, 0, 0)));
        assert_eq!(image.pixel(0, 0), Some(Color::rgb(128, 128, 128)));
    }

    #[test]
    fn fe_composite_arithmetic_honours_all_four_coefficients() {
        let svg = br##"<svg width="8" height="8"><defs>
            <filter id="f" filterUnits="userSpaceOnUse" x="0" y="0" width="8" height="8">
              <feFlood flood-color="#ff0000" result="red"/>
              <feFlood flood-color="#0000ff" result="blue"/>
              <feComposite in="red" in2="blue" operator="arithmetic"
                k1="0" k2="0.5" k3="0.5" k4="0"/>
            </filter></defs>
            <rect width="8" height="8" fill="white" filter="url(#f)"/></svg>"##;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(4, 4), Some(Color::rgb(128, 0, 128)));
    }

    #[test]
    fn stroke_only_line_draws_its_pixels() {
        let svg = br#"<svg width="6" height="6"><line x1="1" y1="1.5" x2="5" y2="1.5" stroke="black" stroke-width="1"/></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(3, 1), Some(Color::rgb(0, 0, 0)));
        assert_eq!(image.pixel(3, 3), Some(Color::rgba(0, 0, 0, 0)));
    }
    #[test]
    fn fe_flood_paints_the_filter_region() {
        let svg = br##"<svg width="20" height="20"><defs>
            <filter id="f" filterUnits="userSpaceOnUse" x="0" y="0" width="20" height="20">
              <feFlood flood-color="#0000ff" flood-opacity="0.5"/>
            </filter></defs>
            <rect x="2" y="2" width="5" height="5" fill="red" filter="url(#f)"/></svg>"##;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(10, 10), Some(Color::rgba(0, 0, 255, 128)));
    }

    #[test]
    fn a_user_space_filter_region_resolves_percentage_geometry() {
        let svg = br##"<svg width="20" height="20" viewBox="0 0 20 20"><defs>
            <filter id="f" filterUnits="userSpaceOnUse"
              x="25%" y="25%" width="50%" height="50%">
              <feFlood flood-color="#ff0000"/>
            </filter></defs>
            <rect width="20" height="20" fill="#0000ff" filter="url(#f)"/></svg>"##;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(4, 4), Some(Color::TRANSPARENT));
        assert_eq!(image.pixel(5, 5), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(14, 14), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(15, 15), Some(Color::TRANSPARENT));
    }

    #[test]
    fn a_primitive_subregion_clips_that_primitive_to_its_own_rectangle() {
        let svg = br#"<svg width="12" height="12"><defs>
            <filter id="f" filterUnits="userSpaceOnUse" x="0" y="0" width="12" height="12">
              <feFlood x="4" y="4" width="4" height="4" flood-color="red"/>
            </filter></defs>
            <rect width="12" height="12" fill="white" filter="url(#f)"/></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(3, 3), Some(Color::TRANSPARENT));
        assert_eq!(image.pixel(4, 4), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(7, 7), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(8, 8), Some(Color::TRANSPARENT));
    }

    #[test]
    fn an_object_bounding_box_primitive_subregion_maps_to_the_element_bounds() {
        let svg = br#"<svg width="20" height="20"><defs>
            <filter id="f" filterUnits="userSpaceOnUse" x="0" y="0" width="20" height="20"
              primitiveUnits="objectBoundingBox">
              <feFlood x="0" y="0" width="0.5" height="0.5" flood-color="red"/>
            </filter></defs>
            <rect x="5" y="5" width="10" height="10" fill="white" filter="url(#f)"/></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(4, 4), Some(Color::TRANSPARENT));
        assert_eq!(image.pixel(5, 5), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(9, 9), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(10, 10), Some(Color::TRANSPARENT));
    }

    #[test]
    fn fe_merge_layers_named_results_in_order() {
        let svg = br#"<svg width="20" height="20"><defs>
            <filter id="f" filterUnits="userSpaceOnUse" x="0" y="0" width="20" height="20">
              <feOffset dx="5" result="shifted"/>
              <feMerge>
                <feMergeNode in="SourceGraphic"/>
                <feMergeNode in="shifted"/>
              </feMerge>
            </filter></defs>
            <rect x="2" y="2" width="5" height="5" fill="red" filter="url(#f)"/></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(3, 3), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(8, 5), Some(Color::rgb(255, 0, 0)));
    }

    #[test]
    fn fe_merge_can_layer_a_flood_under_an_offset_shape() {
        let svg = br##"<svg width="12" height="12"><defs>
            <filter id="f" filterUnits="userSpaceOnUse" x="0" y="0" width="12" height="12">
              <feFlood flood-color="#0000ff" result="background"/>
              <feOffset in="SourceGraphic" dx="4" result="moved"/>
              <feMerge>
                <feMergeNode in="background"/>
                <feMergeNode in="moved"/>
              </feMerge>
            </filter></defs>
            <rect x="1" y="1" width="2" height="2" fill="red" filter="url(#f)"/></svg>"##;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(0, 0), Some(Color::rgb(0, 0, 255)));
        assert_eq!(image.pixel(1, 1), Some(Color::rgb(0, 0, 255)));
        assert_eq!(image.pixel(5, 1), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(11, 11), Some(Color::rgb(0, 0, 255)));
    }

    #[test]
    fn unsupported_filter_primitives_keep_their_input_and_are_reported() {
        let svg = br#"<svg width="10" height="10"><defs>
            <filter id="f" filterUnits="userSpaceOnUse" x="0" y="0" width="10" height="10">
              <feTurbulence baseFrequency="0.5"/>
            </filter></defs>
            <rect x="2" y="2" width="6" height="6" fill="red" filter="url(#f)"/></svg>"#;
        let (image, diagnostics) =
            decode_svg_with_diagnostics(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(4, 4), Some(Color::rgb(255, 0, 0)));
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].element, "feTurbulence");
    }

    #[test]
    fn fe_drop_shadow_paints_an_offset_coloured_shadow_behind_the_source() {
        let svg = br##"<svg width="20" height="20"><defs>
            <filter id="f" filterUnits="userSpaceOnUse" x="0" y="0" width="20" height="20">
              <feDropShadow dx="2" dy="2" stdDeviation="0"
                flood-color="#0000ff" flood-opacity="0.5"/>
            </filter></defs>
            <rect x="5" y="5" width="6" height="6" fill="red" filter="url(#f)"/></svg>"##;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(10, 10), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(12, 12), Some(Color::rgba(0, 0, 255, 128)));
        assert_eq!(image.pixel(2, 2), Some(Color::TRANSPARENT));
    }

    #[test]
    fn fe_drop_shadow_shadows_a_named_result() {
        let svg = br##"<svg width="20" height="20"><defs>
            <filter id="f" filterUnits="userSpaceOnUse" x="0" y="0" width="20" height="20">
              <feOffset in="SourceGraphic" dx="4" result="moved"/>
              <feDropShadow in="moved" dx="1" dy="1" stdDeviation="0"
                flood-color="#00ff00"/>
            </filter></defs>
            <rect x="2" y="2" width="4" height="4" fill="red" filter="url(#f)"/></svg>"##;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(2, 2), Some(Color::TRANSPARENT));
        assert_eq!(image.pixel(7, 4), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(10, 6), Some(Color::rgb(0, 255, 0)));
        assert_eq!(image.pixel(10, 2), Some(Color::TRANSPARENT));
    }

    #[test]
    fn fe_morphology_dilate_uses_the_x_and_y_radii_independently() {
        let svg = br#"<svg width="12" height="12"><defs>
            <filter id="f" filterUnits="userSpaceOnUse" x="0" y="0" width="12" height="12">
              <feMorphology operator="dilate" radius="2 1"/>
            </filter></defs>
            <rect x="4" y="4" width="4" height="4" fill="red" filter="url(#f)"/></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(2, 5), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(1, 5), Some(Color::TRANSPARENT));
        assert_eq!(image.pixel(5, 3), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(5, 2), Some(Color::TRANSPARENT));
        assert_eq!(image.pixel(2, 2), Some(Color::TRANSPARENT));
    }

    #[test]
    fn fe_morphology_erode_thins_the_source_by_the_pixel_radius() {
        let svg = br#"<svg width="12" height="12"><defs>
            <filter id="f" filterUnits="userSpaceOnUse" x="0" y="0" width="12" height="12">
              <feMorphology operator="erode" radius="1"/>
            </filter></defs>
            <rect x="4" y="4" width="4" height="4" fill="red" filter="url(#f)"/></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(4, 4), Some(Color::TRANSPARENT));
        assert_eq!(image.pixel(4, 5), Some(Color::TRANSPARENT));
        assert_eq!(image.pixel(5, 5), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(6, 6), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(7, 7), Some(Color::TRANSPARENT));
    }

    #[test]
    fn fe_morphology_result_can_be_referenced_by_name() {
        let svg = br#"<svg width="12" height="12"><defs>
            <filter id="f" filterUnits="userSpaceOnUse" x="0" y="0" width="12" height="12">
              <feMorphology in="SourceGraphic" operator="dilate" radius="1" result="grown"/>
              <feOffset in="grown" dx="2"/>
            </filter></defs>
            <rect x="3" y="3" width="4" height="4" fill="red" filter="url(#f)"/></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(3, 5), Some(Color::TRANSPARENT));
        assert_eq!(image.pixel(4, 5), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(9, 5), Some(Color::rgb(255, 0, 0)));
        assert_eq!(image.pixel(10, 5), Some(Color::TRANSPARENT));
    }
}
