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
//! - root sizing by `width`/`height` (percentages resolve against the
//!   `viewBox`), falling back to `viewBox`, then the spec default 300×150.
//!
//! Everything outside the subset (text, clips, masks, embedded images,
//! scripting, gradient `xlink:href` inheritance and spread modes other than
//! `pad`) contributes nothing, matching how an `<img>`-loaded SVG must
//! render: static and script-free.
//!
//! `use` and `symbol` indirection is outside the subset too, so a `<use>`
//! contributes nothing: SVG 2 §3.2.4 renders it as a shadow tree cloned from
//! its target, which needs the clone to resolve its own styles and geometry.
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
        &mut raster,
    );
    DecodedImage::from_pixels(width, height, raster.pixels)
}

struct Raster {
    width: u32,
    height: u32,
    pixels: Vec<Color>,
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
        if color.alpha == 0 || x < 0 || y < 0 || x >= self.width.into() || y >= self.height.into() {
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
    raster: &mut Raster,
) {
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
            }
            "polygon" | "polyline" => {
                let Some(points) = attribute("points").and_then(parse_points) else {
                    continue;
                };
                if points.len() < 2 {
                    continue;
                }
                let device = transform_points(&points, child_transform);
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
                stroke_outline_segments(
                    raster,
                    &[(x1, y1), (x2, y2)],
                    &[(from, to)],
                    child_transform,
                    child_stroke.resolve(None),
                    gradients,
                );
            }
            "path" => {
                let Some(specification) = attribute("d") else {
                    continue;
                };
                for (subpath, closed) in &parse_path(specification) {
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
                    raster,
                );
            }
        }
    }
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
#[derive(Debug, PartialEq)]
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

fn parse_paint(raw: &str, current_color: Color, gradient_index: &BTreeMap<String, usize>) -> Paint {
    let trimmed = raw.trim();
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("url(") && trimmed.ends_with(')') {
        let inner = trimmed[4..trimmed.len() - 1].trim();
        let id = inner
            .trim_matches(|character| character == '"' || character == '\'')
            .trim_start_matches('#');
        if let Some(index) = gradient_index.get(id) {
            return Paint::Gradient(*index);
        }
    }
    match lower.as_str() {
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
    fn stroke_only_line_draws_its_pixels() {
        let svg = br#"<svg width="6" height="6"><line x1="1" y1="1" x2="5" y2="1" stroke="black" stroke-width="1"/></svg>"#;
        let image = decode_svg(svg, ImageLimits::default()).expect("decodes");
        assert_eq!(image.pixel(3, 1), Some(Color::rgb(0, 0, 0)));
        assert_eq!(image.pixel(3, 3), Some(Color::rgba(0, 0, 0, 0)));
    }
}
