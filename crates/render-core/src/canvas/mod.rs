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
    Attribute, CanvasError, CanvasErrorKind, CanvasHost, CanvasId, CanvasOp, CanvasStyle,
    CanvasValue, FillRule, MAX_IMAGE_PIXELS, PixelBuffer,
};

use crate::paint::coverage::{contains, polygon_coverage};

use bitmap::{Bitmap, ClipMask, Source};
use path::{
    IDENTITY, Matrix, Path, Point, apply, arc_points, arc_to_geometry, invert, multiply,
    sweep_points,
};
use stroke::{StrokeStyle, stroke_polygons};
use style::{LineCap, LineJoin, Paint, parse_paint, serialize_paint};

/// The width and height of a `<canvas>` with no valid `width` or `height`.
const DEFAULT_WIDTH: u32 = 300;
const DEFAULT_HEIGHT: u32 = 150;

/// The largest side a canvas bitmap is allocated with. A larger canvas keeps its
/// attribute values but has an empty bitmap, so nothing is drawn on it.
const MAX_DIMENSION: u32 = 32_767;

/// The largest pixel count a canvas bitmap is allocated with (16,777,216 pixels,
/// 64 MiB of premultiplied RGBA8).
const MAX_PIXELS: u64 = 1 << 24;

/// The state the context saves and restores (HTML §4.12.5.1.19).
#[derive(Clone, Debug)]
struct DrawingState {
    transform: Matrix,
    fill: Paint,
    stroke: Paint,
    global_alpha: f64,
    line_width: f64,
    line_cap: LineCap,
    line_join: LineJoin,
    miter_limit: f64,
    dash: Vec<f64>,
    dash_offset: f64,
    clip: Option<Arc<ClipMask>>,
}

impl Default for DrawingState {
    fn default() -> Self {
        Self {
            transform: IDENTITY,
            fill: Paint::BLACK,
            stroke: Paint::BLACK,
            global_alpha: 1.0,
            line_width: 1.0,
            line_cap: LineCap::Butt,
            line_join: LineJoin::Miter,
            miter_limit: 10.0,
            dash: Vec::new(),
            dash_offset: 0.0,
            clip: None,
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

    /// Paints `polygons` (device coordinates) with a solid colour under `rule`.
    fn paint(&mut self, polygons: &[Vec<Point>], rule: FillRule, colour: Paint) {
        let Some(coverage) =
            polygon_coverage(polygons, rule, self.bitmap.width(), self.bitmap.height())
        else {
            return;
        };
        let source = Source {
            red: colour.red,
            green: colour.green,
            blue: colour.blue,
            alpha: (f64::from(colour.alpha) * self.state.global_alpha) as f32,
        };
        self.bitmap
            .fill(&coverage, self.state.clip.as_deref(), source);
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
            Attribute::FillStyle => {
                CanvasValue::Style(CanvasStyle::Color(serialize_paint(self.state.fill)))
            }
            Attribute::StrokeStyle => {
                CanvasValue::Style(CanvasStyle::Color(serialize_paint(self.state.stroke)))
            }
            Attribute::GlobalAlpha => CanvasValue::Number(self.state.global_alpha),
            Attribute::LineWidth => CanvasValue::Number(self.state.line_width),
            Attribute::MiterLimit => CanvasValue::Number(self.state.miter_limit),
            Attribute::LineDashOffset => CanvasValue::Number(self.state.dash_offset),
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
                    self.state.fill = paint;
                }
            }
            (Attribute::StrokeStyle, CanvasValue::Text(text)) => {
                if let Some(paint) = parse_paint(&text) {
                    self.state.stroke = paint;
                }
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

    fn run(&mut self, op: CanvasOp) -> Result<CanvasValue, CanvasError> {
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
                    self.paint(&[polygon], FillRule::NonZero, self.state.fill);
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
                    self.paint(&polygons, FillRule::NonZero, self.state.stroke);
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
                self.paint(&polygons, rule, self.state.fill);
            }
            CanvasOp::Stroke => {
                let polygons = self.stroke_path_device();
                self.paint(&polygons, FillRule::NonZero, self.state.stroke);
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
        }
        Ok(CanvasValue::Undefined)
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
}

impl CanvasEngine {
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
        let Some(canvas) = self.canvases.get_mut(&id) else {
            return Ok(CanvasValue::Undefined);
        };
        let result = canvas.run(op);
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

fn index_size(message: &str) -> CanvasError {
    CanvasError {
        kind: CanvasErrorKind::IndexSize,
        message: message.to_owned(),
    }
}
