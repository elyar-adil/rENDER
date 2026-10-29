//! The 2D canvas engine: the bitmap, the drawing state and the path of every
//! `<canvas>` and `OffscreenCanvas`, behind the script seam in
//! `render_js::canvas`.
//!
//! Every drawing operation is rasterized by the CPU rasterizer's coverage
//! routine ([`crate::paint::coverage`]) and composited into the canvas bitmap
//! here. Canvas pixels reach the page through the same per-node image store
//! as video frames, so the paint path draws them like any replaced element.

#![allow(
    // Geometry and pixel code converts between float coordinates and pixel
    // indices. Every conversion is preceded by a clamp or a floor, and the
    // bounds are the canvas or path extent, so truncation is the intent.
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap,
    // Exact comparisons are intended: a degenerate segment is exactly zero
    // length, and an edge is exactly horizontal.
    clippy::float_cmp,
    // The specification's own names (x1, cp2y, ...) are kept.
    clippy::many_single_char_names,
    clippy::similar_names,
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::collapsible_if,
    clippy::single_match_else
)]

mod bitmap;
mod export;
mod path;
mod stroke;
mod style;

#[cfg(test)]
mod argument_tests;
#[cfg(test)]
mod limit_tests;
#[cfg(test)]
mod page_tests;
#[cfg(test)]
mod settings_tests;
#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::Arc;

use render_dom::{Dom, NodeId};
use render_js::canvas::{
    Attribute, CanvasError, CanvasErrorKind, CanvasHost, CanvasId, CanvasImageSource, CanvasOp,
    CanvasStyle, CanvasValue, FillRule, GradientKind, GradientOp, MAX_IMAGE_PIXELS, PixelBuffer,
};

use crate::paint::coverage::{contains, polygon_coverage};

use bitmap::{Bitmap, ClipMask, CompositeMode, Source};
use path::{
    IDENTITY, Matrix, Path, Point, apply, arc_points, arc_to_geometry, invert, multiply,
    sweep_points,
};
use stroke::{StrokeStyle, stroke_polygons};
use style::{LineCap, LineJoin, Paint, gradient_at, parse_paint, serialize_paint};

/// The width and height of a `<canvas>` with no valid `width` or `height`.
const DEFAULT_WIDTH: u32 = 300;
const DEFAULT_HEIGHT: u32 = 150;

/// The largest side a canvas bitmap is allocated with. A larger canvas keeps its
/// attribute values but has an empty bitmap, so nothing is drawn on it.
const MAX_DIMENSION: u32 = 32_767;

/// The largest pixel count a canvas bitmap is allocated with (16,777,216 pixels,
/// 64 MiB of premultiplied RGBA8).
const MAX_PIXELS: u64 = 1 << 24;

/// What the context fills and strokes with: a solid colour, or a gradient
/// of the host's registry, named by the id both sides agreed on (HTML
/// §4.12.5.1.3).
#[derive(Clone, Copy, Debug, PartialEq)]
enum SurfacePaint {
    Solid(Paint),
    Gradient(u64),
}

impl SurfacePaint {
    const BLACK: Self = Self::Solid(Paint::BLACK);
}

/// One registered gradient: its geometry in user space and its colour stops
/// in the order `addColorStop` added them.
#[derive(Clone, Debug)]
struct Gradient {
    kind: GradientKind,
    stops: Vec<(f64, Paint)>,
}

/// The state the context saves and restores (HTML §4.12.5.1.19).
#[derive(Clone, Debug)]
struct DrawingState {
    transform: Matrix,
    fill: SurfacePaint,
    stroke: SurfacePaint,
    global_alpha: f64,
    line_width: f64,
    line_cap: LineCap,
    line_join: LineJoin,
    miter_limit: f64,
    dash: Vec<f64>,
    dash_offset: f64,
    clip: Option<Arc<ClipMask>>,
    // The text, shadow and raster-quality state (HTML §4.12.5.1.12-13). The
    // enumerated attributes hold their validated keyword; the string ones
    // hold exactly what was last set and accepted.
    direction: String,
    filter: String,
    font: String,
    font_kerning: String,
    font_stretch: String,
    font_variant_caps: String,
    global_composite_operation: String,
    image_smoothing_enabled: bool,
    image_smoothing_quality: String,
    letter_spacing: String,
    shadow_blur: f64,
    shadow_color: String,
    shadow_offset_x: f64,
    shadow_offset_y: f64,
    text_align: String,
    text_baseline: String,
    text_rendering: String,
    word_spacing: String,
}

impl Default for DrawingState {
    fn default() -> Self {
        Self {
            transform: IDENTITY,
            fill: SurfacePaint::BLACK,
            stroke: SurfacePaint::BLACK,
            global_alpha: 1.0,
            line_width: 1.0,
            line_cap: LineCap::Butt,
            line_join: LineJoin::Miter,
            miter_limit: 10.0,
            dash: Vec::new(),
            dash_offset: 0.0,
            clip: None,
            direction: "inherit".to_owned(),
            filter: "none".to_owned(),
            font: "10px sans-serif".to_owned(),
            font_kerning: "auto".to_owned(),
            font_stretch: "normal".to_owned(),
            font_variant_caps: "normal".to_owned(),
            global_composite_operation: "source-over".to_owned(),
            image_smoothing_enabled: true,
            image_smoothing_quality: "low".to_owned(),
            letter_spacing: "0px".to_owned(),
            shadow_blur: 0.0,
            shadow_color: "rgba(0, 0, 0, 0)".to_owned(),
            shadow_offset_x: 0.0,
            shadow_offset_y: 0.0,
            text_align: "start".to_owned(),
            text_baseline: "alphabetic".to_owned(),
            text_rendering: "auto".to_owned(),
            word_spacing: "0px".to_owned(),
        }
    }
}

/// One canvas: its size, its bitmap, its context state and its path.
#[derive(Debug)]
struct Canvas {
    /// The `<canvas>` element this bitmap belongs to, or `None` for an
    /// `OffscreenCanvas`.
    element: Option<NodeId>,
    width: u32,
    height: u32,
    bitmap: Bitmap,
    state: DrawingState,
    saved: Vec<DrawingState>,
    path: Path,
}

impl Canvas {
    fn new(element: Option<NodeId>, width: u32, height: u32) -> Self {
        let (bitmap_width, bitmap_height) = bitmap_size(width, height);
        Self {
            element,
            width,
            height,
            bitmap: Bitmap::new(bitmap_width, bitmap_height),
            state: DrawingState::default(),
            saved: Vec::new(),
            path: Path::default(),
        }
    }

    /// Replaces the bitmap with a transparent one and restores every default,
    /// as setting the size and `reset()` both do.
    fn reset(&mut self) {
        let (bitmap_width, bitmap_height) = bitmap_size(self.width, self.height);
        self.bitmap = Bitmap::new(bitmap_width, bitmap_height);
        self.state = DrawingState::default();
        self.saved.clear();
        self.path.clear();
    }

    fn resize(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.reset();
    }

    fn stroke_style(&self) -> StrokeStyle {
        StrokeStyle {
            width: self.state.line_width,
            cap: self.state.line_cap,
            join: self.state.line_join,
            miter_limit: self.state.miter_limit,
            dash: self.state.dash.clone(),
            dash_offset: self.state.dash_offset,
        }
    }

    /// The device-space polygons of the current path's stroke.
    fn stroke_path_device(&self) -> Vec<Vec<Point>> {
        let matrix = self.state.transform;
        let Some(inverse) = invert(matrix) else {
            return Vec::new();
        };
        let subpaths: Vec<(Vec<Point>, bool)> = self
            .path
            .subpaths()
            .iter()
            .map(|subpath| {
                let points = subpath
                    .points
                    .iter()
                    .map(|point| apply(inverse, *point))
                    .collect();
                (points, subpath.closed)
            })
            .collect();
        stroke_polygons(&subpaths, &self.stroke_style())
            .iter()
            .map(|polygon| to_device(matrix, polygon))
            .collect()
    }

    /// Paints `polygons` (device coordinates) under `rule` with the context's
    /// paint: a solid colour everywhere, or a gradient evaluated per pixel in
    /// the user space the current transform maps the bitmap to (HTML
    /// §4.12.5.1.3: the gradient's coordinates live in the space current when
    /// the painting happens).
    /// The registered gradient a paint names, if it names one.
    fn gradient_of<'a>(
        paint: &SurfacePaint,
        gradients: &'a BTreeMap<u64, Gradient>,
    ) -> Option<&'a Gradient> {
        match paint {
            SurfacePaint::Solid(_) => None,
            SurfacePaint::Gradient(id) => gradients.get(id),
        }
    }

    fn paint(
        &mut self,
        polygons: &[Vec<Point>],
        rule: FillRule,
        paint: SurfacePaint,
        gradient: Option<&Gradient>,
    ) {
        let global_alpha = self.state.global_alpha;
        let mode = CompositeMode::parse(&self.state.global_composite_operation)
            .unwrap_or(CompositeMode::SourceOver);
        // A composite other than source-over is defined for every destination
        // pixel: outside the fill's coverage the source alpha is zero, so a
        // fill entirely off the bitmap still clears (or keeps) the whole
        // canvas per the mode. The coverage check therefore cannot run first.
        let coverage = polygon_coverage(polygons, rule, self.bitmap.width(), self.bitmap.height());
        if mode != CompositeMode::SourceOver {
            let sampler: Box<dyn Fn(u32, u32) -> Source> = match paint {
                SurfacePaint::Solid(colour) => {
                    let colour_alpha = f64::from(colour.alpha) * global_alpha;
                    Box::new(move |_x: u32, _y: u32| Source {
                        red: colour.red,
                        green: colour.green,
                        blue: colour.blue,
                        alpha: colour_alpha as f32,
                    })
                }
                SurfacePaint::Gradient(_) => {
                    let Some(gradient) = gradient else {
                        return;
                    };
                    if gradient.stops.is_empty() {
                        return;
                    }
                    let Some(inverse) = invert(self.state.transform) else {
                        return;
                    };
                    let geometry = gradient.kind;
                    let stops = gradient.stops.clone();
                    Box::new(move |x: u32, y: u32| {
                        let user = apply(inverse, (f64::from(x) + 0.5, f64::from(y) + 0.5));
                        let (red, green, blue, alpha) =
                            gradient_at(&geometry, &stops, user.0, user.1);
                        Source {
                            red,
                            green,
                            blue,
                            alpha: (f64::from(alpha) * global_alpha) as f32,
                        }
                    })
                }
            };
            self.bitmap.fill_composited(
                coverage.as_ref(),
                self.state.clip.as_deref(),
                mode,
                sampler,
            );
            return;
        }
        let Some(coverage) = coverage else {
            return;
        };
        match paint {
            SurfacePaint::Solid(colour) => {
                let source = Source {
                    red: colour.red,
                    green: colour.green,
                    blue: colour.blue,
                    alpha: (f64::from(colour.alpha) * global_alpha) as f32,
                };
                self.bitmap
                    .fill(&coverage, self.state.clip.as_deref(), source);
            }
            SurfacePaint::Gradient(_) => {
                let Some(gradient) = gradient else {
                    return;
                };
                if gradient.stops.is_empty() {
                    // A gradient with no stops paints nothing (HTML
                    // §4.12.5.1.3: it must be fully transparent black).
                    return;
                }
                let Some(inverse) = invert(self.state.transform) else {
                    return;
                };
                let geometry = gradient.kind;
                let stops = gradient.stops.clone();
                self.bitmap
                    .fill_where(&coverage, self.state.clip.as_deref(), |x, y| {
                        let user = apply(inverse, (f64::from(x) + 0.5, f64::from(y) + 0.5));
                        let (red, green, blue, alpha) =
                            gradient_at(&geometry, &stops, user.0, user.1);
                        Source {
                            red,
                            green,
                            blue,
                            alpha: (f64::from(alpha) * global_alpha) as f32,
                        }
                    });
            }
        }
    }

    fn clear(&mut self, polygons: &[Vec<Point>]) {
        if let Some(coverage) = polygon_coverage(
            polygons,
            FillRule::NonZero,
            self.bitmap.width(),
            self.bitmap.height(),
        ) {
            self.bitmap.clear(&coverage, self.state.clip.as_deref());
        }
    }

    fn attribute(&self, attribute: Attribute) -> CanvasValue {
        match attribute {
            Attribute::FillStyle => CanvasValue::Style(match self.state.fill {
                SurfacePaint::Solid(paint) => CanvasStyle::Color(serialize_paint(paint)),
                SurfacePaint::Gradient(id) => CanvasStyle::Gradient(id),
            }),
            Attribute::StrokeStyle => CanvasValue::Style(match self.state.stroke {
                SurfacePaint::Solid(paint) => CanvasStyle::Color(serialize_paint(paint)),
                SurfacePaint::Gradient(id) => CanvasStyle::Gradient(id),
            }),
            Attribute::GlobalAlpha => CanvasValue::Number(self.state.global_alpha),
            Attribute::LineWidth => CanvasValue::Number(self.state.line_width),
            Attribute::MiterLimit => CanvasValue::Number(self.state.miter_limit),
            Attribute::LineDashOffset => CanvasValue::Number(self.state.dash_offset),
            Attribute::Direction => CanvasValue::Text(self.state.direction.clone()),
            Attribute::Filter => CanvasValue::Text(self.state.filter.clone()),
            Attribute::Font => CanvasValue::Text(self.state.font.clone()),
            Attribute::FontKerning => CanvasValue::Text(self.state.font_kerning.clone()),
            Attribute::FontStretch => CanvasValue::Text(self.state.font_stretch.clone()),
            Attribute::FontVariantCaps => CanvasValue::Text(self.state.font_variant_caps.clone()),
            Attribute::GlobalCompositeOperation => {
                CanvasValue::Text(self.state.global_composite_operation.clone())
            }
            Attribute::ImageSmoothingEnabled => {
                CanvasValue::Bool(self.state.image_smoothing_enabled)
            }
            Attribute::ImageSmoothingQuality => {
                CanvasValue::Text(self.state.image_smoothing_quality.clone())
            }
            Attribute::LetterSpacing => CanvasValue::Text(self.state.letter_spacing.clone()),
            Attribute::ShadowBlur => CanvasValue::Number(self.state.shadow_blur),
            Attribute::ShadowColor => CanvasValue::Text(self.state.shadow_color.clone()),
            Attribute::ShadowOffsetX => CanvasValue::Number(self.state.shadow_offset_x),
            Attribute::ShadowOffsetY => CanvasValue::Number(self.state.shadow_offset_y),
            Attribute::TextAlign => CanvasValue::Text(self.state.text_align.clone()),
            Attribute::TextBaseline => CanvasValue::Text(self.state.text_baseline.clone()),
            Attribute::TextRendering => CanvasValue::Text(self.state.text_rendering.clone()),
            Attribute::WordSpacing => CanvasValue::Text(self.state.word_spacing.clone()),
            Attribute::LineCap => CanvasValue::Text(self.state.line_cap.keyword().to_owned()),
            Attribute::LineJoin => CanvasValue::Text(self.state.line_join.keyword().to_owned()),
        }
    }

    /// Sets an attribute. A value the specification says to ignore leaves the
    /// attribute unchanged. The script conversion has already produced the
    /// kind of value each attribute takes.
    fn set_attribute(&mut self, attribute: Attribute, value: CanvasValue) {
        match (attribute, value) {
            (Attribute::FillStyle, CanvasValue::Text(text)) => {
                if let Some(paint) = parse_paint(&text) {
                    self.state.fill = SurfacePaint::Solid(paint);
                }
            }
            (Attribute::StrokeStyle, CanvasValue::Text(text)) => {
                if let Some(paint) = parse_paint(&text) {
                    self.state.stroke = SurfacePaint::Solid(paint);
                }
            }
            (Attribute::FillStyle, CanvasValue::Style(CanvasStyle::Gradient(id))) => {
                self.state.fill = SurfacePaint::Gradient(id);
            }
            (Attribute::StrokeStyle, CanvasValue::Style(CanvasStyle::Gradient(id))) => {
                self.state.stroke = SurfacePaint::Gradient(id);
            }
            (Attribute::GlobalAlpha, CanvasValue::Number(value))
                if value.is_finite() && (0.0..=1.0).contains(&value) =>
            {
                self.state.global_alpha = value;
            }
            (Attribute::LineWidth, CanvasValue::Number(value))
                if value.is_finite() && value > 0.0 =>
            {
                self.state.line_width = value;
            }
            (Attribute::MiterLimit, CanvasValue::Number(value))
                if value.is_finite() && value > 0.0 =>
            {
                self.state.miter_limit = value;
            }
            (Attribute::LineDashOffset, CanvasValue::Number(value)) if value.is_finite() => {
                self.state.dash_offset = value;
            }
            (Attribute::LineCap, CanvasValue::Text(keyword)) => {
                if let Some(cap) = LineCap::parse(&keyword) {
                    self.state.line_cap = cap;
                }
            }
            (Attribute::LineJoin, CanvasValue::Text(keyword)) => {
                if let Some(join) = LineJoin::parse(&keyword) {
                    self.state.line_join = join;
                }
            }
            // The enumerated text/composite/raster attributes: an
            // unrecognized keyword leaves the attribute unchanged (HTML
            // §4.12.5.1.12: "return without updating").
            (
                Attribute::Direction
                | Attribute::FontKerning
                | Attribute::FontStretch
                | Attribute::FontVariantCaps
                | Attribute::GlobalCompositeOperation
                | Attribute::ImageSmoothingQuality
                | Attribute::TextAlign
                | Attribute::TextBaseline
                | Attribute::TextRendering,
                CanvasValue::Text(keyword),
            ) if keyword_is_valid(attribute, &keyword) => {
                self.set_state_text(attribute, keyword);
            }
            // The string attributes round-trip exactly what was set; a shadow
            // colour must parse as a colour first (HTML §4.12.5.1.13).
            (
                Attribute::Font
                | Attribute::Filter
                | Attribute::LetterSpacing
                | Attribute::WordSpacing,
                CanvasValue::Text(text),
            ) => {
                self.set_state_text(attribute, text);
            }
            (Attribute::ShadowColor, CanvasValue::Text(text)) if parse_paint(&text).is_some() => {
                self.state.shadow_color = text;
            }
            (Attribute::ShadowBlur, CanvasValue::Number(value))
                if value.is_finite() && value >= 0.0 =>
            {
                self.state.shadow_blur = value;
            }
            (Attribute::ShadowOffsetX, CanvasValue::Number(value)) if value.is_finite() => {
                self.state.shadow_offset_x = value;
            }
            (Attribute::ShadowOffsetY, CanvasValue::Number(value)) if value.is_finite() => {
                self.state.shadow_offset_y = value;
            }
            (Attribute::ImageSmoothingEnabled, CanvasValue::Bool(flag)) => {
                self.state.image_smoothing_enabled = flag;
            }
            _ => {}
        }
    }

    /// Writes one of the text-typed state fields, by attribute.
    fn set_state_text(&mut self, attribute: Attribute, value: String) {
        match attribute {
            Attribute::Direction => self.state.direction = value,
            Attribute::Filter => self.state.filter = value,
            Attribute::Font => self.state.font = value,
            Attribute::FontKerning => self.state.font_kerning = value,
            Attribute::FontStretch => self.state.font_stretch = value,
            Attribute::FontVariantCaps => self.state.font_variant_caps = value,
            Attribute::GlobalCompositeOperation => {
                self.state.global_composite_operation = value;
            }
            Attribute::ImageSmoothingQuality => {
                self.state.image_smoothing_quality = value;
            }
            Attribute::LetterSpacing => self.state.letter_spacing = value,
            Attribute::TextAlign => self.state.text_align = value,
            Attribute::TextBaseline => self.state.text_baseline = value,
            Attribute::TextRendering => self.state.text_rendering = value,
            Attribute::WordSpacing => self.state.word_spacing = value,
            _ => {}
        }
    }

    /// `setLineDash`: a list with a negative or non-finite value is ignored; an
    /// odd-length list is repeated so that the pattern has an even length.
    fn set_line_dash(&mut self, segments: Vec<f64>) {
        if segments
            .iter()
            .any(|value| !value.is_finite() || *value < 0.0)
        {
            return;
        }
        if segments.iter().all(|value| *value == 0.0) {
            self.state.dash = Vec::new();
            return;
        }
        self.state.dash = if segments.len() % 2 == 1 {
            let mut doubled = segments.clone();
            doubled.extend(segments);
            doubled
        } else {
            segments
        };
    }

    fn put_image_data(
        &mut self,
        pixels: &PixelBuffer,
        x: f64,
        y: f64,
        dirty: (f64, f64, f64, f64),
    ) {
        let (mut dirty_x, mut dirty_width) = normalise_span(dirty.0, dirty.2);
        let (mut dirty_y, mut dirty_height) = normalise_span(dirty.1, dirty.3);
        let left = dirty_x.max(0);
        let top = dirty_y.max(0);
        let right = (dirty_x + dirty_width).min(i64::from(pixels.width));
        let bottom = (dirty_y + dirty_height).min(i64::from(pixels.height));
        if right <= left || bottom <= top {
            return;
        }
        dirty_x = left;
        dirty_y = top;
        dirty_width = right - left;
        dirty_height = bottom - top;
        let rectangle = (
            clamp_u32(dirty_x),
            clamp_u32(dirty_y),
            clamp_u32(dirty_width),
            clamp_u32(dirty_height),
        );
        self.bitmap.write_straight(
            &pixels.data,
            pixels.width,
            pixels.height,
            x.trunc() as i64,
            y.trunc() as i64,
            rectangle,
        );
    }

    fn run(
        &mut self,
        op: CanvasOp,
        gradients: &BTreeMap<u64, Gradient>,
        image: Option<&Bitmap>,
    ) -> Result<CanvasValue, CanvasError> {
        let matrix = self.state.transform;
        match op {
            CanvasOp::Save => self.saved.push(self.state.clone()),
            CanvasOp::Restore => {
                if let Some(state) = self.saved.pop() {
                    self.state = state;
                }
            }
            CanvasOp::Reset => self.reset(),
            CanvasOp::Scale { x, y } => {
                if all_finite(&[x, y]) {
                    self.state.transform = multiply(matrix, [x, 0.0, 0.0, y, 0.0, 0.0]);
                }
            }
            CanvasOp::Rotate(angle) => {
                if all_finite(&[angle]) {
                    let (sin, cos) = angle.sin_cos();
                    self.state.transform = multiply(matrix, [cos, sin, -sin, cos, 0.0, 0.0]);
                }
            }
            CanvasOp::Translate { x, y } => {
                if all_finite(&[x, y]) {
                    self.state.transform = multiply(matrix, [1.0, 0.0, 0.0, 1.0, x, y]);
                }
            }
            CanvasOp::Transform(next) => {
                if all_finite(&next) {
                    self.state.transform = multiply(matrix, next);
                }
            }
            CanvasOp::SetTransform(next) => {
                if all_finite(&next) {
                    self.state.transform = next;
                }
            }
            CanvasOp::ResetTransform => self.state.transform = IDENTITY,
            CanvasOp::Get(attribute) => return Ok(self.attribute(attribute)),
            CanvasOp::Set(attribute, value) => self.set_attribute(attribute, value),
            CanvasOp::SetLineDash(segments) => self.set_line_dash(segments),
            CanvasOp::GetLineDash => return Ok(CanvasValue::Numbers(self.state.dash.clone())),
            CanvasOp::FillRect {
                x,
                y,
                width,
                height,
            } => {
                if all_finite(&[x, y, width, height]) && width != 0.0 && height != 0.0 {
                    let polygon = to_device(matrix, &rectangle(x, y, width, height));
                    let gradient = Self::gradient_of(&self.state.fill, gradients);
                    self.paint(&[polygon], FillRule::NonZero, self.state.fill, gradient);
                }
            }
            CanvasOp::ClearRect {
                x,
                y,
                width,
                height,
            } => {
                if all_finite(&[x, y, width, height]) && width != 0.0 && height != 0.0 {
                    let polygon = to_device(matrix, &rectangle(x, y, width, height));
                    self.clear(&[polygon]);
                }
            }
            CanvasOp::StrokeRect {
                x,
                y,
                width,
                height,
            } => {
                if all_finite(&[x, y, width, height]) && !(width == 0.0 && height == 0.0) {
                    let corners = rectangle(x, y, width, height);
                    let outline =
                        stroke_polygons(&[(corners.to_vec(), true)], &self.stroke_style());
                    let polygons: Vec<Vec<Point>> = outline
                        .iter()
                        .map(|polygon| to_device(matrix, polygon))
                        .collect();
                    let gradient = Self::gradient_of(&self.state.stroke, gradients);
                    self.paint(&polygons, FillRule::NonZero, self.state.stroke, gradient);
                }
            }
            CanvasOp::BeginPath => self.path.clear(),
            CanvasOp::ClosePath => self.path.close(),
            CanvasOp::MoveTo { x, y } => {
                if all_finite(&[x, y]) {
                    self.path.move_to(apply(matrix, (x, y)));
                }
            }
            CanvasOp::LineTo { x, y } => {
                if all_finite(&[x, y]) {
                    self.path.line_to(apply(matrix, (x, y)));
                }
            }
            CanvasOp::Rect {
                x,
                y,
                width,
                height,
            } => {
                if all_finite(&[x, y, width, height]) {
                    let corners = rectangle(x, y, width, height);
                    self.path.add_rect([
                        apply(matrix, corners[0]),
                        apply(matrix, corners[1]),
                        apply(matrix, corners[2]),
                        apply(matrix, corners[3]),
                    ]);
                }
            }
            CanvasOp::Arc {
                x,
                y,
                radius,
                start,
                end,
                counter_clockwise,
            } => {
                if radius < 0.0 {
                    return Err(index_size("the arc radius is negative"));
                }
                if all_finite(&[x, y, radius, start, end]) {
                    for point in arc_points(
                        matrix,
                        (x, y),
                        radius,
                        radius,
                        0.0,
                        start,
                        end,
                        counter_clockwise,
                    ) {
                        self.path.line_to(point);
                    }
                }
            }
            CanvasOp::Ellipse {
                x,
                y,
                radius_x,
                radius_y,
                rotation,
                start,
                end,
                counter_clockwise,
            } => {
                if radius_x < 0.0 || radius_y < 0.0 {
                    return Err(index_size("an ellipse radius is negative"));
                }
                if all_finite(&[x, y, radius_x, radius_y, rotation, start, end]) {
                    for point in arc_points(
                        matrix,
                        (x, y),
                        radius_x,
                        radius_y,
                        rotation,
                        start,
                        end,
                        counter_clockwise,
                    ) {
                        self.path.line_to(point);
                    }
                }
            }
            CanvasOp::ArcTo {
                x1,
                y1,
                x2,
                y2,
                radius,
            } => {
                if radius < 0.0 {
                    return Err(index_size("the arcTo radius is negative"));
                }
                if all_finite(&[x1, y1, x2, y2, radius]) {
                    self.arc_to((x1, y1), (x2, y2), radius);
                }
            }
            CanvasOp::BezierCurveTo {
                cp1x,
                cp1y,
                cp2x,
                cp2y,
                x,
                y,
            } => {
                if all_finite(&[cp1x, cp1y, cp2x, cp2y, x, y]) {
                    self.path.cubic_to(
                        apply(matrix, (cp1x, cp1y)),
                        apply(matrix, (cp2x, cp2y)),
                        apply(matrix, (x, y)),
                    );
                }
            }
            CanvasOp::QuadraticCurveTo { cpx, cpy, x, y } => {
                if all_finite(&[cpx, cpy, x, y]) {
                    self.path
                        .quadratic_to(apply(matrix, (cpx, cpy)), apply(matrix, (x, y)));
                }
            }
            CanvasOp::Fill(rule) => {
                let polygons = self.path.polygons();
                let gradient = Self::gradient_of(&self.state.fill, gradients);
                self.paint(&polygons, rule, self.state.fill, gradient);
            }
            CanvasOp::Stroke => {
                let polygons = self.stroke_path_device();
                let gradient = Self::gradient_of(&self.state.stroke, gradients);
                self.paint(&polygons, FillRule::NonZero, self.state.stroke, gradient);
            }
            CanvasOp::Clip(rule) => self.clip(rule),
            CanvasOp::IsPointInPath { x, y, rule } => {
                let inside = all_finite(&[x, y]) && contains(&self.path.polygons(), rule, x, y);
                return Ok(CanvasValue::Bool(inside));
            }
            CanvasOp::IsPointInStroke { x, y } => {
                let inside = all_finite(&[x, y])
                    && contains(&self.stroke_path_device(), FillRule::NonZero, x, y);
                return Ok(CanvasValue::Bool(inside));
            }
            CanvasOp::GetImageData {
                x,
                y,
                width,
                height,
            } => {
                let (left, span_width) = normalise_span(x, width);
                let (top, span_height) = normalise_span(y, height);
                let (span_width, span_height) = (clamp_u32(span_width), clamp_u32(span_height));
                if u64::from(span_width) * u64::from(span_height) > MAX_IMAGE_PIXELS {
                    return Err(CanvasError {
                        kind: CanvasErrorKind::Range,
                        message: "the image is too large for the canvas surface".to_owned(),
                    });
                }
                let data = self
                    .bitmap
                    .read_straight(left, top, span_width, span_height);
                return Ok(CanvasValue::Pixels(PixelBuffer {
                    width: span_width,
                    height: span_height,
                    data,
                }));
            }
            CanvasOp::PutImageData {
                pixels,
                x,
                y,
                dirty_x,
                dirty_y,
                dirty_width,
                dirty_height,
            } => {
                self.put_image_data(&pixels, x, y, (dirty_x, dirty_y, dirty_width, dirty_height));
            }
            CanvasOp::ToDataUrl { mime_type, quality } => {
                return Ok(CanvasValue::Text(export::data_url(
                    &self.bitmap,
                    &mime_type,
                    quality,
                )));
            }
            CanvasOp::DrawImage {
                source: _,
                source_rect,
                dest_rect,
            } => {
                // `execute` resolved and snapshotted the source bitmap; the
                // op itself only names it so the engine knows what to copy.
                let Some(source) = image else {
                    return Ok(CanvasValue::Undefined);
                };
                self.draw_image(source, source_rect, dest_rect);
            }
        }
        Ok(CanvasValue::Undefined)
    }

    /// `drawImage` (HTML §4.12.5.1.15): paints the source bitmap's rectangle
    /// through the current transform into the destination rectangle.
    ///
    /// The source rectangle is clipped to the source image first, and the
    /// destination is clipped in the same proportion, as the specification
    /// requires. Painting honours global alpha, the clip region, the
    /// compositing operator and `imageSmoothingEnabled`; the filter reads the
    /// image with clamp-to-edge at its border.
    fn draw_image(&mut self, source: &Bitmap, source_rect: [f64; 4], dest_rect: [f64; 4]) {
        let [sx, sy, sw, sh] = source_rect;
        let [dx, dy, dw, dh] = dest_rect;
        if !all_finite(&[sx, sy, sw, sh, dx, dy, dw, dh]) {
            return;
        }
        let Some(([sx, sy, sw, sh], [dx, dy, dw, dh])) =
            clip_image_rects(source, source_rect, dest_rect)
        else {
            return;
        };
        let matrix = self.state.transform;
        let Some(inverse) = invert(matrix) else {
            return;
        };
        let polygon = to_device(matrix, &rectangle(dx, dy, dw, dh));
        let coverage = polygon_coverage(
            &[polygon],
            FillRule::NonZero,
            self.bitmap.width(),
            self.bitmap.height(),
        );
        let global_alpha = self.state.global_alpha;
        let smoothing = self.state.image_smoothing_enabled;
        let mode = CompositeMode::parse(&self.state.global_composite_operation)
            .unwrap_or(CompositeMode::SourceOver);
        let sampler = |x: u32, y: u32| -> Source {
            let user = apply(inverse, (f64::from(x) + 0.5, f64::from(y) + 0.5));
            let u = (user.0 - dx) / dw;
            let v = (user.1 - dy) / dh;
            let pixel = sample_premultiplied(source, sx + u * sw, sy + v * sh, smoothing);
            let alpha = f32::from(pixel[3]);
            if alpha <= 0.0 {
                return Source {
                    red: 0,
                    green: 0,
                    blue: 0,
                    alpha: 0.0,
                };
            }
            let straight = 255.0 / alpha;
            Source {
                red: (f32::from(pixel[0]) * straight).round().clamp(0.0, 255.0) as u8,
                green: (f32::from(pixel[1]) * straight).round().clamp(0.0, 255.0) as u8,
                blue: (f32::from(pixel[2]) * straight).round().clamp(0.0, 255.0) as u8,
                alpha: (alpha / 255.0 * global_alpha as f32).clamp(0.0, 1.0),
            }
        };
        if mode == CompositeMode::SourceOver {
            if let Some(coverage) = coverage {
                self.bitmap
                    .fill_where(&coverage, self.state.clip.as_deref(), sampler);
            }
        } else {
            self.bitmap.fill_composited(
                coverage.as_ref(),
                self.state.clip.as_deref(),
                mode,
                sampler,
            );
        }
    }

    /// `arcTo`: a straight segment to the first tangent point, then the arc to
    /// the second. The geometry is worked in user space.
    fn arc_to(&mut self, corner: (f64, f64), target: (f64, f64), radius: f64) {
        let matrix = self.state.transform;
        let corner_device = apply(matrix, corner);
        let Some(current) = self.path.current_point() else {
            self.path.move_to(corner_device);
            return;
        };
        let Some(inverse) = invert(matrix) else {
            return;
        };
        let from = apply(inverse, current);
        let Some(arc) = arc_to_geometry(from, corner, target, radius) else {
            self.path.line_to(corner_device);
            return;
        };
        self.path.line_to(apply(matrix, arc.tangent_1));
        for point in sweep_points(
            matrix,
            arc.centre,
            radius,
            radius,
            0.0,
            arc.start_angle,
            arc.sweep,
        )
        .into_iter()
        .skip(1)
        {
            self.path.line_to(point);
        }
    }

    /// `clip`: intersects the clip region with the path's coverage. An empty
    /// path clips everything.
    fn clip(&mut self, rule: FillRule) {
        let polygons = self.path.polygons();
        let coverage = polygon_coverage(&polygons, rule, self.bitmap.width(), self.bitmap.height());
        let (width, height) = (self.bitmap.width(), self.bitmap.height());
        let next = match (self.state.clip.as_deref(), coverage) {
            (Some(current), Some(coverage)) => current.intersect(&coverage),
            (None, Some(coverage)) => ClipMask::full(width, height).intersect(&coverage),
            (_, None) => ClipMask::empty(width, height),
        };
        self.state.clip = Some(Arc::new(next));
    }
}

/// The engine behind every canvas of a page. The page and the script runtime
/// share one, through [`CanvasHandle`].
#[derive(Debug, Default)]
pub struct CanvasEngine {
    canvases: BTreeMap<CanvasId, Canvas>,
    elements: BTreeMap<NodeId, CanvasId>,
    next_id: u64,
    dirty: BTreeSet<CanvasId>,
    /// The gradients every canvas of this engine can paint with, by the id
    /// the script side chose (HTML §4.12.5.1.3: a gradient is not tied to
    /// the context that made it).
    gradients: BTreeMap<u64, Gradient>,
}

impl CanvasEngine {
    /// Creates or amends one gradient of the registry (HTML §4.12.5.1.3).
    ///
    /// # Errors
    ///
    /// An `IndexSizeError` for an offset outside `0..=1`, a `SyntaxError`
    /// for a colour that does not parse.
    fn define_gradient(&mut self, op: GradientOp) -> Result<(), CanvasError> {
        match op {
            GradientOp::Create { id, kind } => {
                self.gradients.insert(
                    id,
                    Gradient {
                        kind,
                        stops: Vec::new(),
                    },
                );
                Ok(())
            }
            GradientOp::AddColorStop { id, offset, color } => {
                if !offset.is_finite() || !(0.0..=1.0).contains(&offset) {
                    return Err(index_size(
                        "Failed to execute 'addColorStop' on 'CanvasGradient':                          The provided value is outside the range (0, 1).",
                    ));
                }
                let Some(paint) = parse_paint(&color) else {
                    return Err(CanvasError {
                        kind: CanvasErrorKind::Syntax,
                        message: format!(
                            "Failed to execute 'addColorStop' on 'CanvasGradient':                              '{color}' is not a valid colour."
                        ),
                    });
                };
                let gradient = self.gradients.entry(id).or_insert_with(|| Gradient {
                    kind: GradientKind::Linear {
                        x0: 0.0,
                        y0: 0.0,
                        x1: 0.0,
                        y1: 0.0,
                    },
                    stops: Vec::new(),
                });
                gradient.stops.push((offset, paint));
                Ok(())
            }
        }
    }

    fn create(&mut self, element: Option<NodeId>, width: u32, height: u32) -> CanvasId {
        let id = CanvasId(self.next_id);
        self.next_id += 1;
        self.canvases
            .insert(id, Canvas::new(element, width, height));
        id
    }

    /// The canvas of an element, created on first request with the size its
    /// attributes give.
    fn canvas_for(&mut self, dom: &Dom, node: NodeId) -> CanvasId {
        if let Some(id) = self.elements.get(&node).copied() {
            self.reconcile(dom, id);
            return id;
        }
        let (width, height) = element_size(dom, node);
        let id = self.create(Some(node), width, height);
        self.elements.insert(node, id);
        id
    }

    /// Brings an element canvas to the size its attributes currently give. A
    /// change resets the bitmap, as the specification requires.
    fn reconcile(&mut self, dom: &Dom, id: CanvasId) {
        let Some(canvas) = self.canvases.get_mut(&id) else {
            return;
        };
        let Some(node) = canvas.element else {
            return;
        };
        let (width, height) = element_size(dom, node);
        if (width, height) != (canvas.width, canvas.height) {
            canvas.resize(width, height);
            self.dirty.insert(id);
        }
    }

    fn execute(
        &mut self,
        dom: &Dom,
        id: CanvasId,
        op: CanvasOp,
    ) -> Result<CanvasValue, CanvasError> {
        self.reconcile(dom, id);
        let reads_only = is_read(&op);
        // A `drawImage` source is snapshotted before the destination is
        // touched: the specification requires a canvas drawn onto itself to
        // read the bitmap as it was before the call.
        let image = match &op {
            CanvasOp::DrawImage {
                source: CanvasImageSource::Canvas(source),
                ..
            } => self
                .canvases
                .get(source)
                .map(|canvas| canvas.bitmap.clone()),
            _ => None,
        };
        let Some(canvas) = self.canvases.get_mut(&id) else {
            return Ok(CanvasValue::Undefined);
        };
        let result = canvas.run(op, &self.gradients, image.as_ref());
        if !reads_only && result.is_ok() {
            self.dirty.insert(id);
        }
        result
    }

    /// The straight-alpha pixels of every element canvas whose bitmap changed
    /// since the last call, as `(element, width, height, rgba)`.
    pub fn take_changed_elements(&mut self, dom: &Dom) -> Vec<(NodeId, u32, u32, Vec<u8>)> {
        let changed: Vec<CanvasId> = self.dirty.iter().copied().collect();
        self.dirty.clear();
        let mut out = Vec::new();
        for id in changed {
            self.reconcile(dom, id);
            if let Some(canvas) = self.canvases.get(&id) {
                if let Some(node) = canvas.element {
                    out.push((
                        node,
                        canvas.bitmap.width(),
                        canvas.bitmap.height(),
                        canvas.bitmap.straight_rgba(),
                    ));
                }
            }
        }
        out
    }

    /// Whether any canvas bitmap changed since the last `take_changed_elements`.
    #[must_use]
    pub fn has_changes(&self) -> bool {
        !self.dirty.is_empty()
    }
}

/// The shared handle the script runtime is installed with and the page keeps.
#[derive(Clone, Debug, Default)]
pub struct CanvasHandle(Rc<RefCell<CanvasEngine>>);

impl CanvasHandle {
    /// The engine this handle drives, for the page's paint path.
    #[must_use]
    pub fn engine(&self) -> Rc<RefCell<CanvasEngine>> {
        Rc::clone(&self.0)
    }
}

impl CanvasHost for CanvasHandle {
    fn canvas_for(&mut self, dom: &Dom, node: NodeId) -> CanvasId {
        self.0.borrow_mut().canvas_for(dom, node)
    }

    fn create_offscreen(&mut self, width: u32, height: u32) -> CanvasId {
        self.0.borrow_mut().create(None, width, height)
    }

    fn size(&mut self, dom: &Dom, canvas: CanvasId) -> (u32, u32) {
        let mut engine = self.0.borrow_mut();
        engine.reconcile(dom, canvas);
        engine
            .canvases
            .get(&canvas)
            .map_or((0, 0), |found| (found.width, found.height))
    }

    fn set_offscreen_size(&mut self, canvas: CanvasId, width: u32, height: u32) {
        let mut engine = self.0.borrow_mut();
        if let Some(found) = engine.canvases.get_mut(&canvas) {
            found.resize(width, height);
            engine.dirty.insert(canvas);
        }
    }

    fn execute(
        &mut self,
        dom: &Dom,
        canvas: CanvasId,
        op: CanvasOp,
    ) -> Result<CanvasValue, CanvasError> {
        self.0.borrow_mut().execute(dom, canvas, op)
    }

    fn define_gradient(&mut self, op: GradientOp) -> Result<(), CanvasError> {
        self.0.borrow_mut().define_gradient(op)
    }
}

/// Whether an operation only reads the canvas.
fn is_read(op: &CanvasOp) -> bool {
    matches!(
        op,
        CanvasOp::Get(_)
            | CanvasOp::GetLineDash
            | CanvasOp::IsPointInPath { .. }
            | CanvasOp::IsPointInStroke { .. }
            | CanvasOp::GetImageData { .. }
            | CanvasOp::ToDataUrl { .. }
    )
}

/// The size of the bitmap that backs a canvas of the given attribute size.
const fn bitmap_size(width: u32, height: u32) -> (u32, u32) {
    if width > MAX_DIMENSION || height > MAX_DIMENSION {
        return (0, 0);
    }
    if (width as u64) * (height as u64) > MAX_PIXELS {
        return (0, 0);
    }
    (width, height)
}

/// The size an element canvas takes from its `width` and `height` attributes.
fn element_size(dom: &Dom, node: NodeId) -> (u32, u32) {
    let width = dom.attribute(node, "width").ok().flatten();
    let height = dom.attribute(node, "height").ok().flatten();
    (
        dimension(width, DEFAULT_WIDTH),
        dimension(height, DEFAULT_HEIGHT),
    )
}

/// The HTML rules for parsing a non-negative integer, with the reflected
/// `unsigned long` default when the value is absent, empty or out of range.
fn dimension(value: Option<&str>, default: u32) -> u32 {
    let Some(text) = value else {
        return default;
    };
    let trimmed = text.trim_start_matches(|c: char| c.is_ascii_whitespace());
    let trimmed = trimmed.strip_prefix('+').unwrap_or(trimmed);
    let digits: String = trimmed.chars().take_while(char::is_ascii_digit).collect();
    match digits.parse::<u64>() {
        Ok(parsed) if parsed <= 2_147_483_647 => u32::try_from(parsed).unwrap_or(default),
        _ => default,
    }
}

/// A rectangle as its four corners in order, starting at `(x, y)`.
fn rectangle(x: f64, y: f64, width: f64, height: f64) -> [Point; 4] {
    [
        (x, y),
        (x + width, y),
        (x + width, y + height),
        (x, y + height),
    ]
}

fn to_device(matrix: Matrix, polygon: &[Point]) -> Vec<Point> {
    polygon.iter().map(|point| apply(matrix, *point)).collect()
}

fn all_finite(values: &[f64]) -> bool {
    values.iter().all(|value| value.is_finite())
}

/// Clip the source rectangle of `drawImage` to the source bitmap, and clip the
/// destination rectangle in the same proportion (HTML §4.12.5.1.15).
///
/// Returns `None` when no part of the source overlaps the image. Negative
/// widths and heights are kept: the specification processes the image in its
/// original direction, so the mapping from source to destination stays affine
/// and the sign only mirrors the result.
fn clip_image_rects(
    source: &Bitmap,
    [sx, sy, sw, sh]: [f64; 4],
    [dx, dy, dw, dh]: [f64; 4],
) -> Option<([f64; 4], [f64; 4])> {
    let width = f64::from(source.width());
    let height = f64::from(source.height());
    let (u0, u1) = span_overlap(sx, sw, width)?;
    let (v0, v1) = span_overlap(sy, sh, height)?;
    Some((
        [sx + u0 * sw, sy + v0 * sh, (u1 - u0) * sw, (v1 - v0) * sh],
        [dx + u0 * dw, dy + v0 * dh, (u1 - u0) * dw, (v1 - v0) * dh],
    ))
}

/// The parameter interval of `origin + t * length` (`t` in `0..=1`) whose
/// values fall inside `0..=bound`, or `None` when the span misses it.
///
/// The span may run backwards; the returned interval is always `(low, high)`.
fn span_overlap(origin: f64, length: f64, bound: f64) -> Option<(f64, f64)> {
    if length == 0.0 || !length.is_finite() || bound <= 0.0 {
        return None;
    }
    let end = origin + length;
    let (start, end) = if length > 0.0 {
        (origin, end)
    } else {
        (end, origin)
    };
    let clipped_start = start.max(0.0);
    let clipped_end = end.min(bound);
    if clipped_end <= clipped_start {
        return None;
    }
    let u0 = (clipped_start - origin) / length;
    let u1 = (clipped_end - origin) / length;
    Some((u0.min(u1), u0.max(u1)))
}

/// The source pixel at `(x, y)` in premultiplied RGBA, filtered as
/// `imageSmoothingEnabled` asks: bilinear interpolation of the four nearest
/// pixels, or the enclosing pixel for nearest-neighbour. Samples outside the
/// image clamp to the nearest edge pixel, which is the filter behaviour HTML
/// §4.12.5.1.15 names.
fn sample_premultiplied(source: &Bitmap, x: f64, y: f64, smoothing: bool) -> [u8; 4] {
    if !smoothing {
        return source.pixel_clamped(x.floor() as i64, y.floor() as i64);
    }
    // Pixel centres sit at half-integers, so the enclosing pixel pair around
    // `x` shifts by 0.5 before the floor.
    let x = x - 0.5;
    let y = y - 0.5;
    let x0 = x.floor();
    let y0 = y.floor();
    let fx = (x - x0) as f32;
    let fy = (y - y0) as f32;
    let (x0, y0) = (x0 as i64, y0 as i64);
    let top_left = source.pixel_clamped(x0, y0);
    let top_right = source.pixel_clamped(x0 + 1, y0);
    let bottom_left = source.pixel_clamped(x0, y0 + 1);
    let bottom_right = source.pixel_clamped(x0 + 1, y0 + 1);
    let mut out = [0; 4];
    for channel in 0..4 {
        let top = f32::from(top_left[channel]) * (1.0 - fx) + f32::from(top_right[channel]) * fx;
        let bottom =
            f32::from(bottom_left[channel]) * (1.0 - fx) + f32::from(bottom_right[channel]) * fx;
        out[channel] = (top * (1.0 - fy) + bottom * fy).round().clamp(0.0, 255.0) as u8;
    }
    out
}

/// A span given by an origin and a signed length, as the start and length of
/// the span it covers. A negative length is read backwards from the origin.
fn normalise_span(origin: f64, length: f64) -> (i64, i64) {
    let (origin, length) = if length < 0.0 {
        (origin + length, -length)
    } else {
        (origin, length)
    };
    (origin.trunc() as i64, length.trunc() as i64)
}

fn clamp_u32(value: i64) -> u32 {
    u32::try_from(value.max(0)).unwrap_or(u32::MAX)
}

/// Whether `keyword` is one the enumerated attribute accepts (HTML
/// §4.12.5.1.12-13). An unrecognized keyword means the assignment is ignored,
/// which is why the caller asks rather than pattern-matching later.
fn keyword_is_valid(attribute: Attribute, keyword: &str) -> bool {
    let (list,): (&[&str],) = match attribute {
        Attribute::Direction => (&["ltr", "rtl", "inherit"],),
        Attribute::FontKerning => (&["auto", "normal", "none"],),
        Attribute::FontStretch => (&[
            "ultra-condensed",
            "extra-condensed",
            "condensed",
            "semi-condensed",
            "normal",
            "semi-expanded",
            "expanded",
            "extra-expanded",
            "ultra-expanded",
        ],),
        Attribute::FontVariantCaps => (&[
            "normal",
            "small-caps",
            "all-small-caps",
            "petite-caps",
            "all-petite-caps",
            "unicase",
            "titling-caps",
        ],),
        Attribute::GlobalCompositeOperation => (&[
            "clear",
            "source-over",
            "source-in",
            "source-out",
            "source-atop",
            "destination-over",
            "destination-in",
            "destination-out",
            "destination-atop",
            "lighter",
            "copy",
            "xor",
            "multiply",
            "screen",
            "overlay",
            "darken",
            "lighten",
            "color-dodge",
            "color-burn",
            "hard-light",
            "soft-light",
            "difference",
            "exclusion",
            "hue",
            "saturation",
            "color",
            "luminosity",
        ],),
        Attribute::ImageSmoothingQuality => (&["low", "medium", "high"],),
        Attribute::TextAlign => (&["start", "end", "left", "right", "center"],),
        Attribute::TextBaseline => (&[
            "alphabetic",
            "top",
            "hanging",
            "middle",
            "ideographic",
            "bottom",
        ],),
        Attribute::TextRendering => (&[
            "auto",
            "optimizeSpeed",
            "optimizeLegibility",
            "geometricPrecision",
        ],),
        _ => return false,
    };
    // These are Web IDL enum values on context attributes, not HTML content
    // attributes: their keywords are case-sensitive (WPT
    // `2d.composite.operation.casesensitive`).
    list.contains(&keyword)
}

fn index_size(message: &str) -> CanvasError {
    CanvasError {
        kind: CanvasErrorKind::IndexSize,
        message: message.to_owned(),
    }
}
