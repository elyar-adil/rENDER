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
//! - same-document `<use>` references through `href`/`xlink:href`, including
//!   `x`/`y`, inherited paint, and `<symbol>` viewBox scaling by the use
//!   element's `width`/`height`;
//! - `mask` references on shapes and groups, with offscreen alpha or
//!   luminance compositing, `maskUnits` and `maskContentUnits`;
//! - `filter` references with `feGaussianBlur`, `feColorMatrix`, `feOffset`,
//!   `feBlend`, `feComposite`, `feFlood` and `feMerge`, composited through
//!   offscreen buffers;
//! - root sizing by `width`/`height` (percentages resolve against the
//!   `viewBox`), falling back to `viewBox`, then the spec default 300×150.
//!
//! Everything outside the subset (text, filter primitives other than the
//! five above, embedded images, scripting, external-document `<use>`
//! references, gradient `xlink:href` inheritance and spread modes other than
//! `pad`) contributes nothing, matching how an `<img>`-loaded SVG must
//! render: static and script-free.
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

/// Decode an SVG document into an RGBA image.
///
/// # Errors
///
/// Returns [`ImageDecodeError`] when the bytes are not valid UTF-8, no root
/// `<svg>` element exists, or the target size violates `limits`.
pub fn decode_svg(bytes: &[u8], limits: ImageLimits) -> Result<DecodedImage, ImageDecodeError> {
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
) -> Result<DecodedImage, ImageDecodeError> {
    let width = width_f.round().max(1.0) as u32;
    let height = height_f.round().max(1.0) as u32;
    enforce_dimensions(width, height, limits)?;

    let mut raster = Raster {
        width,
        height,
        pixels: vec![Color::rgba(0, 0, 0, 0); width as usize * height as usize],
        clip: ClipStack::default(),
        use_depth: 0,
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
    let fill = root_paint(
        root,
        "fill",
        current_color,
        Paint::Color(Color::BLACK),
        &gradient_index,
    );
    let stroke = root_paint(root, "stroke", current_color, Paint::None, &gradient_index);
    walk(
        root,
        root_transform,
        fill,
        stroke,
        current_color,
        &gradients,
        &gradient_index,
        &clip_paths,
        &mask_paths,
        &filter_paths,
        &ids,
        &mut raster,
    );
    DecodedImage::from_pixels(width, height, raster.pixels)
}

struct Raster {
    width: u32,
    height: u32,
    pixels: Vec<Color>,
    clip: ClipStack,
    use_depth: usize,
    mask_depth: usize,
    filter_depth: usize,
    opacity_depth: usize,
}

impl Raster {
    fn fill_polygon(&mut self, points: &[(f32, f32)], color_at: impl Fn(f32, f32) -> Color) {
        if points.len() < 2 {
            return;
        }
        let min_y = points.iter().fold(f32::MAX, |m, p| m.min(p.1));
        let max_y = points.iter().fold(f32::MIN, |m, p| m.max(p.1));
        let first_row = (min_y.floor().max(0.0)) as i64;
        let last_row = (max_y.ceil().min(f32::from(u16::MAX))) as i64;
        for row in first_row..=last_row {
            let sample = row as f32 + 0.5;
            let mut crossings: Vec<f32> = Vec::new();
            for index in 0..points.len() {
                let (x1, y1) = points[index];
                let (x2, y2) = points[(index + 1) % points.len()];
                if (y1 <= sample && y2 > sample) || (y2 <= sample && y1 > sample) {
                    let t = (sample - y1) / (y2 - y1);
                    crossings.push(x1 + t * (x2 - x1));
                }
            }
            crossings.sort_by(|left, right| {
                left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal)
            });
            for pair in crossings.chunks(2) {
                if pair.len() < 2 {
                    break;
                }
                // Pixel x is covered when its center (x + 0.5) lies in
                // [x0, x1): start = ceil(x0 - 0.5), end = ceil(x1 - 0.5)-1.
                let start = (pair[0] - 0.5).ceil().max(0.0) as i64;
                let end = ((pair[1] - 0.5).ceil() as i64)
                    .saturating_sub(1)
                    .min(i64::from(self.width.saturating_sub(1)));
                for x in start..=end {
                    self.put(x, row, color_at(x as f32 + 0.5, row as f32 + 0.5));
                }
            }
        }
    }

    fn stroke_segments(&mut self, segments: &[Segment], color_at: impl Fn(f32, f32) -> Color) {
        for ((x1, y1), (x2, y2)) in segments {
            let steps = ((x2 - x1).abs().max((y2 - y1).abs()).ceil() as i64).clamp(1, 4096);
            for step in 0..=steps {
                let t = step as f32 / steps as f32;
                let x = (x1 + t * (x2 - x1)).round() as i64;
                let y = (y1 + t * (y2 - y1)).round() as i64;
                self.put(x, y, color_at(x as f32 + 0.5, y as f32 + 0.5));
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
    match attribute(root, name).map(|raw| parse_paint(raw.trim(), current_color, gradient_index)) {
        None | Some(Paint::Inherit) => initial,
        Some(paint) => paint,
    }
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
    gradients: &[Gradient],
    gradient_index: &BTreeMap<String, usize>,
    clip_paths: &BTreeMap<String, &XmlNode>,
    mask_paths: &BTreeMap<String, &XmlNode>,
    filter_paths: &BTreeMap<String, &XmlNode>,
    ids: &BTreeMap<String, &XmlNode>,
    raster: &mut Raster,
) {
    let clipped = push_element_clip(raster, element, transform, None, clip_paths);
    for child_node in &element.children {
        let attribute = |name: &str| {
            attribute(child_node, name)
                .map(str::trim)
                .filter(|value| !value.is_empty())
        };
        let child_fill =
            attribute("fill").map_or(fill, |raw| parse_paint(raw, current_color, gradient_index));
        let child_stroke = attribute("stroke").map_or(stroke, |raw| {
            parse_paint(raw, current_color, gradient_index)
        });
        let child_transform = match attribute("transform") {
            Some(specification) => transform.then(parse_transform(specification)),
            None => transform,
        };
        if attribute("display").is_some_and(|value| value.eq_ignore_ascii_case("none")) {
            continue;
        }
        if paint_opacity_group(
            raster,
            child_node,
            transform,
            child_fill,
            child_stroke,
            current_color,
            gradients,
            gradient_index,
            clip_paths,
            mask_paths,
            filter_paths,
            ids,
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
            current_color,
            gradients,
            gradient_index,
            clip_paths,
            mask_paths,
            filter_paths,
            ids,
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
            current_color,
            gradients,
            gradient_index,
            clip_paths,
            mask_paths,
            filter_paths,
            ids,
        ) {
            continue;
        }
        match child_node.name.as_str() {
            "defs" | "text" | "image" | "style" | "title" | "desc" | "metadata" => {}
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
                    gradients,
                );
                stroke_outline(
                    raster,
                    &points,
                    &device,
                    child_transform,
                    child_stroke.resolve(None),
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
                    gradients,
                );
                stroke_outline(
                    raster,
                    &points,
                    &device,
                    child_transform,
                    child_stroke.resolve(None),
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
                    gradients,
                );
                let segments = polyline_segments(&device, child_node.name == "polygon");
                stroke_outline_segments(
                    raster,
                    &points,
                    &segments,
                    child_transform,
                    child_stroke.resolve(None),
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
                for (subpath, closed) in &subpaths {
                    if subpath.len() < 2 {
                        continue;
                    }
                    let device = transform_points(subpath, child_transform);
                    paint_shape(
                        raster,
                        subpath,
                        &device,
                        child_transform,
                        child_fill.resolve(None),
                        gradients,
                    );
                    let segments = polyline_segments(&device, *closed);
                    stroke_outline_segments(
                        raster,
                        subpath,
                        &segments,
                        child_transform,
                        child_stroke.resolve(None),
                        gradients,
                    );
                }
                if clipped {
                    raster.clip.pop();
                }
            }
            "use" => {
                let Some(reference) = attribute("href").or_else(|| attribute("xlink:href")) else {
                    continue;
                };
                let Some(id) = fragment_id(reference) else {
                    continue;
                };
                let Some(target) = ids.get(id) else {
                    continue;
                };
                if raster.use_depth >= 16 {
                    continue;
                }
                let x = attribute("x").and_then(parse_length).unwrap_or(0.0);
                let y = attribute("y").and_then(parse_length).unwrap_or(0.0);
                let mut use_transform = child_transform.then(Affine {
                    a: 1.0,
                    b: 0.0,
                    c: 0.0,
                    d: 1.0,
                    e: x,
                    f: y,
                });
                if matches!(target.name.as_str(), "symbol" | "svg")
                    && let Some((min_x, min_y, view_width, view_height)) = root_view_box(target)
                    && view_width > 0.0
                    && view_height > 0.0
                {
                    let width = attribute("width")
                        .and_then(parse_length)
                        .or_else(|| {
                            target
                                .attributes
                                .iter()
                                .find(|(name, _)| name == "width")
                                .and_then(|(_, value)| parse_length(value))
                        })
                        .unwrap_or(view_width);
                    let height = attribute("height")
                        .and_then(parse_length)
                        .or_else(|| {
                            target
                                .attributes
                                .iter()
                                .find(|(name, _)| name == "height")
                                .and_then(|(_, value)| parse_length(value))
                        })
                        .unwrap_or(view_height);
                    let scale_x = width / view_width;
                    let scale_y = height / view_height;
                    use_transform = use_transform.then(
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
                            }),
                    );
                }
                let mut attributes = Vec::new();
                if let Some(clip) = attribute("clip-path") {
                    attributes.push(("clip-path".to_owned(), clip.to_owned()));
                }
                let shadow = XmlNode {
                    name: "use-shadow".to_owned(),
                    attributes,
                    children: vec![(*target).clone()],
                };
                raster.use_depth += 1;
                walk(
                    &shadow,
                    use_transform,
                    child_fill,
                    child_stroke,
                    current_color,
                    gradients,
                    gradient_index,
                    clip_paths,
                    mask_paths,
                    filter_paths,
                    ids,
                    raster,
                );
                raster.use_depth -= 1;
            }
            "svg" | "g" | "a" | "switch" => {
                if attribute("display").is_some_and(|value| value.eq_ignore_ascii_case("none")) {
                    continue;
                }
                walk(
                    child_node,
                    child_transform,
                    child_fill,
                    child_stroke,
                    current_color,
                    gradients,
                    gradient_index,
                    clip_paths,
                    mask_paths,
                    filter_paths,
                    ids,
                    raster,
                );
            }
            _ => {
                // Unknown container: descend, it may hold shapes.
                walk(
                    child_node,
                    child_transform,
                    child_fill,
                    child_stroke,
                    current_color,
                    gradients,
                    gradient_index,
                    clip_paths,
                    mask_paths,
                    filter_paths,
                    ids,
                    raster,
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
    gradients: &[Gradient],
    gradient_index: &BTreeMap<String, usize>,
    clip_paths: &BTreeMap<String, &XmlNode>,
    mask_paths: &BTreeMap<String, &XmlNode>,
    filter_paths: &BTreeMap<String, &XmlNode>,
    ids: &BTreeMap<String, &XmlNode>,
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
        pixels: vec![Color::TRANSPARENT; raster.width as usize * raster.height as usize],
        clip: raster.clip.clone(),
        use_depth: raster.use_depth,
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
        gradients,
        gradient_index,
        clip_paths,
        mask_paths,
        filter_paths,
        ids,
        &mut content,
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
    gradients: &[Gradient],
    gradient_index: &BTreeMap<String, usize>,
    clip_paths: &BTreeMap<String, &XmlNode>,
    mask_paths: &BTreeMap<String, &XmlNode>,
    filter_paths: &BTreeMap<String, &XmlNode>,
    ids: &BTreeMap<String, &XmlNode>,
) -> bool {
    let Some(reference) = attribute(element, "mask") else {
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
        pixels: vec![Color::TRANSPARENT; raster.width as usize * raster.height as usize],
        clip: raster.clip.clone(),
        use_depth: raster.use_depth,
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
        gradients,
        gradient_index,
        clip_paths,
        mask_paths,
        filter_paths,
        ids,
        &mut content,
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
        pixels: vec![Color::TRANSPARENT; raster.width as usize * raster.height as usize],
        clip: ClipStack::default(),
        use_depth: 0,
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
        gradients,
        gradient_index,
        clip_paths,
        mask_paths,
        filter_paths,
        ids,
        &mut mask_raster,
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

/// Render one element through an SVG filter.
///
/// Like masks, the element is painted into an offscreen buffer with its
/// `filter` attribute removed. The filter primitives then transform that
/// buffer, and the result is composited through the filter region. The first
/// Supported primitives are `feGaussianBlur`, `feColorMatrix`, `feOffset`,
/// `feBlend`, `feComposite`, `feFlood` and `feMerge`; a chain is processed in
/// document order, with `result` names and `in`/`in2` references.
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
    gradients: &[Gradient],
    gradient_index: &BTreeMap<String, usize>,
    clip_paths: &BTreeMap<String, &XmlNode>,
    mask_paths: &BTreeMap<String, &XmlNode>,
    filter_paths: &BTreeMap<String, &XmlNode>,
    ids: &BTreeMap<String, &XmlNode>,
) -> bool {
    let Some(reference) = attribute(element, "filter") else {
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
        pixels: vec![Color::TRANSPARENT; raster.width as usize * raster.height as usize],
        clip: raster.clip.clone(),
        use_depth: raster.use_depth,
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
        gradients,
        gradient_index,
        clip_paths,
        mask_paths,
        filter_paths,
        ids,
        &mut content,
    );

    let source = content.pixels;
    let source_alpha: Vec<Color> = source
        .iter()
        .map(|pixel| Color::rgba(0, 0, 0, pixel.alpha))
        .collect();
    let mut results: BTreeMap<String, Vec<Color>> = BTreeMap::new();
    let mut pixels = source.clone();
    let object_primitives = attribute(filter, "primitiveUnits")
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("objectBoundingBox"));
    let transform_scale = affine_scale(element_transform);
    for primitive in &filter.children {
        let input = filter_input(primitive, "in", &source, &source_alpha, &results, &pixels);
        let input2 = filter_input(primitive, "in2", &source, &source_alpha, &results, &pixels);
        let output = match primitive.name.as_str() {
            "feGaussianBlur" => {
                let (mut sigma_x, mut sigma_y) =
                    parse_std_deviation(attribute(primitive, "stdDeviation").unwrap_or("0"));
                if object_primitives {
                    sigma_x *= bounds.width;
                    sigma_y *= bounds.height;
                }
                blur_pixels(
                    &input,
                    raster.width,
                    raster.height,
                    sigma_x * transform_scale,
                    sigma_y * transform_scale,
                )
            }
            "feColorMatrix" => {
                let matrix = parse_color_matrix(primitive);
                let mut output = input;
                for pixel in &mut output {
                    *pixel = apply_color_matrix(*pixel, &matrix);
                }
                output
            }
            "feOffset" => {
                let mut dx = attribute(primitive, "dx")
                    .and_then(parse_length)
                    .unwrap_or(0.0);
                let mut dy = attribute(primitive, "dy")
                    .and_then(parse_length)
                    .unwrap_or(0.0);
                if object_primitives {
                    dx *= bounds.width;
                    dy *= bounds.height;
                }
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
            "feBlend" => blend_pixels(
                &input2,
                &input,
                attribute(primitive, "mode").unwrap_or("normal"),
            ),
            "feComposite" => composite_pixels(
                &input2,
                &input,
                attribute(primitive, "operator").unwrap_or("over"),
            ),
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
                    let layer = filter_input(node, "in", &source, &source_alpha, &results, &merged);
                    merged = composite_pixels(&merged, &layer, "over");
                }
                merged
            }
            _ => input,
        };
        if let Some(result) = attribute(primitive, "result") {
            results.insert(result.trim().to_owned(), output.clone());
        }
        pixels = output;
    }

    let object_units = !attribute(filter, "filterUnits")
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("userSpaceOnUse"));
    let (x, y, width, height) = if object_units {
        (
            attribute(filter, "x")
                .and_then(parse_fraction)
                .unwrap_or(-0.1),
            attribute(filter, "y")
                .and_then(parse_fraction)
                .unwrap_or(-0.1),
            attribute(filter, "width")
                .and_then(parse_fraction)
                .unwrap_or(1.2),
            attribute(filter, "height")
                .and_then(parse_fraction)
                .unwrap_or(1.2),
        )
    } else {
        (
            attribute(filter, "x")
                .and_then(parse_length)
                .unwrap_or(bounds.x - 0.1 * bounds.width),
            attribute(filter, "y")
                .and_then(parse_length)
                .unwrap_or(bounds.y - 0.1 * bounds.height),
            attribute(filter, "width")
                .and_then(parse_length)
                .unwrap_or(1.2 * bounds.width),
            attribute(filter, "height")
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
            let pixel = pixels[index];
            if pixel.alpha > 0 {
                raster.put(i64::from(x), i64::from(y), pixel);
            }
        }
    }
    true
}

fn filter_input(
    primitive: &XmlNode,
    name: &str,
    source: &[Color],
    source_alpha: &[Color],
    results: &BTreeMap<String, Vec<Color>>,
    previous: &[Color],
) -> Vec<Color> {
    match attribute(primitive, name).map(str::trim) {
        Some("SourceGraphic") => source.to_vec(),
        Some("SourceAlpha") => source_alpha.to_vec(),
        Some(result) => results
            .get(result)
            .cloned()
            .unwrap_or_else(|| previous.to_vec()),
        None => previous.to_vec(),
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

fn blend_pixels(backdrop: &[Color], source: &[Color], mode: &str) -> Vec<Color> {
    let mode = mode.trim().to_ascii_lowercase();
    backdrop
        .iter()
        .zip(source)
        .map(|(backdrop, source)| {
            if mode == "normal" {
                return source_over(*source, *backdrop);
            }
            let sa = f32::from(source.alpha) / 255.0;
            let da = f32::from(backdrop.alpha) / 255.0;
            let output_alpha = sa + da * (1.0 - sa);
            if output_alpha <= 0.0001 {
                return Color::TRANSPARENT;
            }
            let channel = |source: u8, backdrop: u8| -> u8 {
                let source = f32::from(source) / 255.0;
                let backdrop = f32::from(backdrop) / 255.0;
                let blended = match mode.as_str() {
                    "multiply" => source * backdrop,
                    "screen" => 1.0 - (1.0 - source) * (1.0 - backdrop),
                    "darken" => source.min(backdrop),
                    "lighten" => source.max(backdrop),
                    "difference" => (source - backdrop).abs(),
                    "exclusion" => source + backdrop - 2.0 * source * backdrop,
                    _ => source,
                };
                let value = (blended * sa + backdrop * da * (1.0 - sa)) / output_alpha;
                (value.clamp(0.0, 1.0) * 255.0).round() as u8
            };
            Color::rgba(
                channel(source.red, backdrop.red),
                channel(source.green, backdrop.green),
                channel(source.blue, backdrop.blue),
                (output_alpha * 255.0).round().clamp(0.0, 255.0) as u8,
            )
        })
        .collect()
}

fn composite_pixels(backdrop: &[Color], source: &[Color], operator: &str) -> Vec<Color> {
    let operator = operator.trim().to_ascii_lowercase();
    backdrop
        .iter()
        .zip(source)
        .map(|(backdrop, source)| {
            let sa = f32::from(source.alpha) / 255.0;
            let da = f32::from(backdrop.alpha) / 255.0;
            let (out_alpha, source_weight, backdrop_weight) = match operator.as_str() {
                "in" => (sa * da, 1.0, 0.0),
                "out" => (sa * (1.0 - da), 1.0, 0.0),
                "atop" => (da, da, 1.0 - sa),
                "xor" => (sa + da - 2.0 * sa * da, 1.0 - da, 1.0 - sa),
                "arithmetic" => (sa + da - sa * da, 1.0, 1.0),
                _ => (sa + da * (1.0 - sa), 1.0, 1.0 - sa),
            };
            if out_alpha <= 0.0001 {
                return Color::TRANSPARENT;
            }
            let channel = |source: u8, backdrop: u8| -> u8 {
                let value = (f32::from(source) * sa * source_weight
                    + f32::from(backdrop) * da * backdrop_weight)
                    / (out_alpha * 255.0);
                (value.clamp(0.0, 1.0) * 255.0).round() as u8
            };
            Color::rgba(
                channel(source.red, backdrop.red),
                channel(source.green, backdrop.green),
                channel(source.blue, backdrop.blue),
                (out_alpha * 255.0).round().clamp(0.0, 255.0) as u8,
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

fn parse_std_deviation(raw: &str) -> (f32, f32) {
    let values: Vec<f32> = raw
        .split(|character: char| character == ',' || character.is_whitespace())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse::<f32>().ok())
        .collect();
    let first = values.first().copied().unwrap_or(0.0);
    (first, values.get(1).copied().unwrap_or(first))
}

fn affine_scale(transform: Affine) -> f32 {
    let scale_x = transform.a.hypot(transform.b);
    let scale_y = transform.c.hypot(transform.d);
    scale_x.midpoint(scale_y).max(0.0001)
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
                if sample_x < 0
                    || sample_y < 0
                    || sample_x >= width as i32
                    || sample_y >= height as i32
                {
                    continue;
                }
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

fn parse_color_matrix(element: &XmlNode) -> [f32; 20] {
    let values: Vec<f32> = attribute(element, "values")
        .unwrap_or("")
        .split(|character: char| character == ',' || character.is_whitespace())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse::<f32>().ok())
        .collect();
    let kind = attribute(element, "type")
        .unwrap_or("matrix")
        .trim()
        .to_ascii_lowercase();
    match kind.as_str() {
        "matrix" if values.len() >= 20 => {
            let mut matrix = [0.0; 20];
            matrix.copy_from_slice(&values[..20]);
            matrix
        }
        "saturate" => {
            let saturation = values.first().copied().unwrap_or(1.0);
            [
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
            ]
        }
        "huerotate" => {
            let radians = values.first().copied().unwrap_or(0.0).to_radians();
            let (sin, cos) = radians.sin_cos();
            [
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
            ]
        }
        "luminancetoalpha" => [
            0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.2126,
            0.7152, 0.0722, 0.0, 0.0,
        ],
        _ => [
            1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0,
            0.0, 1.0, 0.0,
        ],
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
    gradients: &[Gradient],
) {
    let Some(paint) = paint else {
        return;
    };
    match paint {
        Paint::Color(color) => raster.fill_polygon(device_points, |_, _| color),
        Paint::Gradient(index) => {
            let Some(gradient) = gradients.get(index) else {
                return;
            };
            let Some(sampler) = GradientSampler::new(local_points, transform, gradient) else {
                return;
            };
            raster.fill_polygon(device_points, |x, y| sampler.color_at(x, y));
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
fn paint_stroke(
    raster: &mut Raster,
    local_points: &[(f32, f32)],
    segments: &[Segment],
    transform: Affine,
    paint: Option<Paint>,
    gradients: &[Gradient],
) {
    let Some(paint) = paint else {
        return;
    };
    match paint {
        Paint::Color(color) => raster.stroke_segments(segments, |_, _| color),
        Paint::Gradient(index) => {
            let Some(gradient) = gradients.get(index) else {
                return;
            };
            let Some(sampler) = GradientSampler::new(local_points, transform, gradient) else {
                return;
            };
            raster.stroke_segments(segments, |x, y| sampler.color_at(x, y));
        }
        Paint::Inherit | Paint::None => {}
    }
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

fn stroke_outline(
    raster: &mut Raster,
    local_points: &[(f32, f32)],
    device_points: &[(f32, f32)],
    transform: Affine,
    paint: Option<Paint>,
    gradients: &[Gradient],
) {
    let segments = polyline_segments(device_points, true);
    paint_stroke(raster, local_points, &segments, transform, paint, gradients);
}

fn stroke_outline_segments(
    raster: &mut Raster,
    local_points: &[(f32, f32)],
    segments: &[Segment],
    transform: Affine,
    paint: Option<Paint>,
    gradients: &[Gradient],
) {
    paint_stroke(raster, local_points, segments, transform, paint, gradients);
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

/// The id in an `href="#id"` or `xlink:href="#id"` reference.
fn fragment_id(raw: &str) -> Option<&str> {
    let trimmed = raw.trim();
    if let Some(id) = reference_id(trimmed) {
        return Some(id);
    }
    let id = trimmed.strip_prefix('#')?;
    (!id.is_empty()).then_some(id)
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
    let Some(reference) = attribute(element, "clip-path") else {
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
                // Skip rx ry rotation large-arc sweep; land on the endpoint.
                for _ in 0..5 {
                    let _ = path_number(tokens, &mut index);
                }
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
    use super::{Color, ImageLimits, decode_svg, decode_svg_viewport, parse_path};

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
        let svg = br#"<svg width="10" height="10" fill="none" stroke="red"><rect width="4" height="4"/><line x1="6" y1="1" x2="8" y2="1"/></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(2, 2), Some(Color::rgba(0, 0, 0, 0)));
        assert_eq!(image.pixel(7, 1), Some(Color::rgb(255, 0, 0)));
    }

    /// `currentColor` takes the colour the caller supplies, for fill and for
    /// stroke alike, and not a fixed black.
    #[test]
    fn current_color_resolves_to_the_callers_colour() {
        let svg = br#"<svg width="8" height="8"><rect width="4" height="8" fill="currentColor"/><line x1="5" y1="1" x2="7" y2="1" stroke="CurrentColor" stroke-width="1"/></svg>"#;
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
            <line x1="0" y1="5" x2="10" y2="5" stroke="blue"/></g></svg>""#;
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

    #[test]
    fn a_self_referential_use_stops_at_the_depth_limit() {
        let svg = br##"<svg width="10" height="10"><defs>
            <g id="loop"><use href="#loop"/></g></defs>
            <use href="#loop"/></svg>"##;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(5, 5), Some(Color::TRANSPARENT));
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
    fn stroke_only_line_draws_its_pixels() {
        let svg = br#"<svg width="6" height="6"><line x1="1" y1="1" x2="5" y2="1" stroke="black" stroke-width="1"/></svg>"#;
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
}
