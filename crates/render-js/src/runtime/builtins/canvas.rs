#![allow(
    clippy::similar_names,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::too_many_lines,
    clippy::needless_pass_by_value,
    clippy::match_same_arms
)]

//! The 2D canvas surface of script: `HTMLCanvasElement.prototype.getContext`,
//! `CanvasRenderingContext2D`, `OffscreenCanvas`, and `ImageData`
//! (HTML Standard §4.12.5).
//!
//! This file is the script-facing half only. It converts each call's
//! arguments with the `WebIDL` rules, raises the `DOMException` the
//! specification names, and forwards one [`CanvasOp`] to the embedder's
//! [`CanvasHost`]. The drawing state, the pixels and the rasterization live in
//! the host, so that the paint path and the script read one bitmap.
//!
//! Each member is installed only when its behaviour is implemented. A member
//! that is not listed here, such as `drawImage` or the gradient constructors,
//! is absent from its prototype rather than stubbed.

use crate::canvas::{
    Attribute, CanvasError, CanvasErrorKind, CanvasHost, CanvasId, CanvasOp, CanvasStyle,
    CanvasValue, FillRule, MAX_IMAGE_PIXELS, PixelBuffer,
};
use std::f64::consts::FRAC_PI_2;
use std::f64::consts::PI;

use crate::runtime::JsRuntime;
use crate::runtime::builtins::dom_exception::DomExceptionName;
use crate::runtime::convert::to_number;
use crate::value::{
    JsObject, JsSymbol, NativeFunction, ObjectHost, PropertyDescriptor, TypedArrayKind, TypedBuffer,
};
use crate::{JsError, JsValue, ObjectId};
use render_dom::{Dom, Namespace, NodeId, NodeKind};

/// The interfaces this file installs whose instances need a constructor host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CanvasInterface {
    Context2d,
    Gradient,
    OffscreenCanvas,
    ImageData,
}

/// The state of one canvas-surface object, kept in its `ObjectHost`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum CanvasObject {
    /// The interface object, which is callable only by `new` where a
    /// constructor exists.
    Constructor(CanvasInterface),
    /// A `CanvasRenderingContext2D`, with the canvas object it draws into.
    Context { id: CanvasId, canvas: ObjectId },
    /// An `OffscreenCanvas`.
    OffscreenCanvas(CanvasId),
    /// A `CanvasGradient` (HTML §4.12.5.1.3): its registry id, which names
    /// the definition the host holds and the stops `addColorStop` amends.
    Gradient(u64),
    /// An `ImageData`: its size and its `Uint8ClampedArray` of RGBA bytes.
    ImageData {
        width: u32,
        height: u32,
        data: ObjectId,
    },
}

/// The methods of the canvas surface, dispatched by `NativeFunction::Canvas`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CanvasMethod {
    GetContext,
    Save,
    Restore,
    Reset,
    Scale,
    Rotate,
    Translate,
    Transform,
    SetTransform,
    ResetTransform,
    SetLineDash,
    GetLineDash,
    FillRect,
    ClearRect,
    StrokeRect,
    BeginPath,
    ClosePath,
    MoveTo,
    LineTo,
    Rect,
    Arc,
    ArcTo,
    Ellipse,
    RoundRect,
    CreateLinearGradient,
    CreateRadialGradient,
    GradientAddColorStop,
    BezierCurveTo,
    QuadraticCurveTo,
    Fill,
    Stroke,
    Clip,
    IsPointInPath,
    IsPointInStroke,
    GetImageData,
    PutImageData,
    CreateImageData,
}

/// The accessors of the canvas surface, dispatched by
/// `NativeFunction::CanvasGetter` and `NativeFunction::CanvasSetter`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CanvasProperty {
    /// A plain attribute of the 2D context, read and written through the host.
    Attribute(Attribute),
    /// `CanvasRenderingContext2D.canvas`.
    Canvas,
    OffscreenWidth,
    OffscreenHeight,
    ImageWidth,
    ImageHeight,
    ImageData,
}

/// The context attributes this surface exposes, with the value kind each takes.
const CONTEXT_ATTRIBUTES: &[(&str, Attribute, AttributeKind)] = &[
    ("fillStyle", Attribute::FillStyle, AttributeKind::Style),
    ("strokeStyle", Attribute::StrokeStyle, AttributeKind::Style),
    ("globalAlpha", Attribute::GlobalAlpha, AttributeKind::Number),
    ("lineWidth", Attribute::LineWidth, AttributeKind::Number),
    ("lineCap", Attribute::LineCap, AttributeKind::Text),
    ("lineJoin", Attribute::LineJoin, AttributeKind::Text),
    ("miterLimit", Attribute::MiterLimit, AttributeKind::Number),
    (
        "lineDashOffset",
        Attribute::LineDashOffset,
        AttributeKind::Number,
    ),
];

/// How an attribute's value crosses the script boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AttributeKind {
    Number,
    Text,
    Style,
}

impl JsRuntime {
    /// Installs the embedder's canvas host. Without one, `getContext` answers
    /// `null` and the canvas surface stays inert.
    pub fn install_canvas_host(&mut self, host: Box<dyn CanvasHost>) {
        self.canvas_host = Some(host);
    }

    pub(in crate::runtime) fn dispatch_canvas_native(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match function {
            NativeFunction::Canvas(method) => self.canvas_method(dom, method, receiver, arguments),
            NativeFunction::CanvasGetter(property) => self.canvas_get(dom, property, receiver),
            NativeFunction::CanvasSetter(property) => {
                self.canvas_set(dom, property, receiver, arguments)
            }
            _ => Err(JsError::type_error("not a canvas surface function")),
        }
    }

    /// `new OffscreenCanvas(width, height)`, `new ImageData(...)`, and the
    /// refusal of the interfaces that have no constructor.
    pub(in crate::runtime) fn canvas_construct(
        &mut self,
        dom: &mut Dom,
        interface: CanvasInterface,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match interface {
            CanvasInterface::Context2d => Err(JsError::type_error("Illegal constructor")),
            CanvasInterface::OffscreenCanvas => {
                let width = self.canvas_unsigned_long(dom, arguments, 0)?;
                let height = self.canvas_unsigned_long(dom, arguments, 1)?;
                let Some(host) = self.canvas_host.as_mut() else {
                    return Err(JsError::type_error("OffscreenCanvas is not available"));
                };
                let id = host.create_offscreen(width, height);
                let prototype = self.canvas_prototype("OffscreenCanvas");
                self.ensure_heap_capacity(1)?;
                let object = self.realm.allocate(JsObject::with_host(
                    prototype,
                    ObjectHost::Canvas(CanvasObject::OffscreenCanvas(id)),
                ));
                Ok(JsValue::Object(object))
            }
            CanvasInterface::ImageData => self.image_data_construct(dom, arguments),
            // HTML §4.12.5.1.3: `new CanvasGradient()` is not how gradients
            // are made; the interface object exists for `instanceof`.
            CanvasInterface::Gradient => Err(JsError::type_error("Illegal constructor")),
        }
    }

    /// The canvas methods. Each validates its receiver, converts its
    /// arguments, and forwards one operation.
    fn canvas_method(
        &mut self,
        dom: &mut Dom,
        method: CanvasMethod,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match method {
            CanvasMethod::GetContext => self.canvas_get_context(dom, receiver, arguments),
            CanvasMethod::Save => self.canvas_run(dom, receiver, CanvasOp::Save),
            CanvasMethod::Restore => self.canvas_run(dom, receiver, CanvasOp::Restore),
            CanvasMethod::Reset => self.canvas_run(dom, receiver, CanvasOp::Reset),
            CanvasMethod::ResetTransform => {
                self.canvas_run(dom, receiver, CanvasOp::ResetTransform)
            }
            CanvasMethod::BeginPath => self.canvas_run(dom, receiver, CanvasOp::BeginPath),
            CanvasMethod::ClosePath => self.canvas_run(dom, receiver, CanvasOp::ClosePath),
            CanvasMethod::Stroke => self.canvas_run(dom, receiver, CanvasOp::Stroke),
            CanvasMethod::GetLineDash => self.canvas_run(dom, receiver, CanvasOp::GetLineDash),
            CanvasMethod::Scale => {
                let x = self.canvas_number(dom, arguments, 0)?;
                let y = self.canvas_number(dom, arguments, 1)?;
                self.canvas_run(dom, receiver, CanvasOp::Scale { x, y })
            }
            CanvasMethod::Translate => {
                let x = self.canvas_number(dom, arguments, 0)?;
                let y = self.canvas_number(dom, arguments, 1)?;
                self.canvas_run(dom, receiver, CanvasOp::Translate { x, y })
            }
            CanvasMethod::Rotate => {
                let angle = self.canvas_number(dom, arguments, 0)?;
                self.canvas_run(dom, receiver, CanvasOp::Rotate(angle))
            }
            CanvasMethod::Transform => {
                let matrix = self.canvas_matrix(dom, arguments)?;
                self.canvas_run(dom, receiver, CanvasOp::Transform(matrix))
            }
            CanvasMethod::SetTransform => {
                let matrix = self.canvas_matrix(dom, arguments)?;
                self.canvas_run(dom, receiver, CanvasOp::SetTransform(matrix))
            }
            CanvasMethod::SetLineDash => {
                let segments = self.canvas_number_sequence(dom, arguments)?;
                self.canvas_run(dom, receiver, CanvasOp::SetLineDash(segments))
            }
            CanvasMethod::FillRect | CanvasMethod::ClearRect | CanvasMethod::StrokeRect => {
                let [x, y, width, height] = self.canvas_rect_args(dom, arguments)?;
                let op = match method {
                    CanvasMethod::FillRect => CanvasOp::FillRect {
                        x,
                        y,
                        width,
                        height,
                    },
                    CanvasMethod::ClearRect => CanvasOp::ClearRect {
                        x,
                        y,
                        width,
                        height,
                    },
                    _ => CanvasOp::StrokeRect {
                        x,
                        y,
                        width,
                        height,
                    },
                };
                self.canvas_run(dom, receiver, op)
            }
            CanvasMethod::Rect => {
                let [x, y, width, height] = self.canvas_rect_args(dom, arguments)?;
                self.canvas_run(
                    dom,
                    receiver,
                    CanvasOp::Rect {
                        x,
                        y,
                        width,
                        height,
                    },
                )
            }
            CanvasMethod::MoveTo => {
                let x = self.canvas_number(dom, arguments, 0)?;
                let y = self.canvas_number(dom, arguments, 1)?;
                self.canvas_run(dom, receiver, CanvasOp::MoveTo { x, y })
            }
            CanvasMethod::LineTo => {
                let x = self.canvas_number(dom, arguments, 0)?;
                let y = self.canvas_number(dom, arguments, 1)?;
                self.canvas_run(dom, receiver, CanvasOp::LineTo { x, y })
            }
            CanvasMethod::Arc => {
                let x = self.canvas_number(dom, arguments, 0)?;
                let y = self.canvas_number(dom, arguments, 1)?;
                let radius = self.canvas_number(dom, arguments, 2)?;
                let start = self.canvas_number(dom, arguments, 3)?;
                let end = self.canvas_number(dom, arguments, 4)?;
                let counter_clockwise = Self::canvas_flag(arguments, 5);
                self.canvas_run(
                    dom,
                    receiver,
                    CanvasOp::Arc {
                        x,
                        y,
                        radius,
                        start,
                        end,
                        counter_clockwise,
                    },
                )
            }
            CanvasMethod::ArcTo => {
                let x1 = self.canvas_number(dom, arguments, 0)?;
                let y1 = self.canvas_number(dom, arguments, 1)?;
                let x2 = self.canvas_number(dom, arguments, 2)?;
                let y2 = self.canvas_number(dom, arguments, 3)?;
                let radius = self.canvas_number(dom, arguments, 4)?;
                self.canvas_run(
                    dom,
                    receiver,
                    CanvasOp::ArcTo {
                        x1,
                        y1,
                        x2,
                        y2,
                        radius,
                    },
                )
            }
            CanvasMethod::Ellipse => {
                let x = self.canvas_number(dom, arguments, 0)?;
                let y = self.canvas_number(dom, arguments, 1)?;
                let radius_x = self.canvas_number(dom, arguments, 2)?;
                let radius_y = self.canvas_number(dom, arguments, 3)?;
                let rotation = self.canvas_number(dom, arguments, 4)?;
                let start = self.canvas_number(dom, arguments, 5)?;
                let end = self.canvas_number(dom, arguments, 6)?;
                let counter_clockwise = Self::canvas_flag(arguments, 7);
                self.canvas_run(
                    dom,
                    receiver,
                    CanvasOp::Ellipse {
                        x,
                        y,
                        radius_x,
                        radius_y,
                        rotation,
                        start,
                        end,
                        counter_clockwise,
                    },
                )
            }
            CanvasMethod::CreateLinearGradient | CanvasMethod::CreateRadialGradient => {
                let radial = matches!(method, CanvasMethod::CreateRadialGradient);
                let numbers = self.canvas_numbers(dom, arguments, if radial { 6 } else { 4 })?;
                if !Self::all_finite(&numbers) {
                    // HTML §4.12.5.1.3: a nonfinite argument returns without
                    // creating anything.
                    return Ok(JsValue::Undefined);
                }
                let id = self.next_gradient_id;
                self.next_gradient_id += 1;
                let kind = if radial {
                    crate::canvas::GradientKind::Radial {
                        x0: numbers[0],
                        y0: numbers[1],
                        r0: numbers[2],
                        x1: numbers[3],
                        y1: numbers[4],
                        r1: numbers[5],
                    }
                } else {
                    crate::canvas::GradientKind::Linear {
                        x0: numbers[0],
                        y0: numbers[1],
                        x1: numbers[2],
                        y1: numbers[3],
                    }
                };
                self.canvas_host
                    .as_mut()
                    .ok_or_else(|| JsError::type_error("the canvas surface is not available"))?
                    .define_gradient(crate::canvas::GradientOp::Create { id, kind })
                    .map_err(|error| self.canvas_error(error))?;
                self.gradient_object(id)
            }
            CanvasMethod::GradientAddColorStop => {
                let id = match self.realm.host(receiver) {
                    Some(ObjectHost::Canvas(
                        crate::runtime::builtins::canvas::CanvasObject::Gradient(id),
                    )) => id,
                    _ => return Err(JsError::type_error("Illegal invocation")),
                };
                let offset = arguments
                    .first()
                    .ok_or_else(missing_argument)
                    .and_then(|value| self.to_number_value(dom, value))?;
                let color = arguments
                    .get(1)
                    .ok_or_else(missing_argument)
                    .map(|value| self.canvas_to_string(dom, value))??;
                self.canvas_host
                    .as_mut()
                    .ok_or_else(|| JsError::type_error("the canvas surface is not available"))?
                    .define_gradient(crate::canvas::GradientOp::AddColorStop { id, offset, color })
                    .map_err(|error| self.canvas_error(error))?;
                Ok(JsValue::Undefined)
            }
            CanvasMethod::RoundRect => {
                let x = self.canvas_number(dom, arguments, 0)?;
                let y = self.canvas_number(dom, arguments, 1)?;
                let width = self.canvas_number(dom, arguments, 2)?;
                let height = self.canvas_number(dom, arguments, 3)?;
                let radii = self.round_rect_radii(dom, arguments.get(4))?;
                self.round_rect_path(dom, receiver, x, y, width, height, radii)?;
                Ok(JsValue::Undefined)
            }
            CanvasMethod::BezierCurveTo => {
                let cp1x = self.canvas_number(dom, arguments, 0)?;
                let cp1y = self.canvas_number(dom, arguments, 1)?;
                let cp2x = self.canvas_number(dom, arguments, 2)?;
                let cp2y = self.canvas_number(dom, arguments, 3)?;
                let x = self.canvas_number(dom, arguments, 4)?;
                let y = self.canvas_number(dom, arguments, 5)?;
                self.canvas_run(
                    dom,
                    receiver,
                    CanvasOp::BezierCurveTo {
                        cp1x,
                        cp1y,
                        cp2x,
                        cp2y,
                        x,
                        y,
                    },
                )
            }
            CanvasMethod::QuadraticCurveTo => {
                let cpx = self.canvas_number(dom, arguments, 0)?;
                let cpy = self.canvas_number(dom, arguments, 1)?;
                let x = self.canvas_number(dom, arguments, 2)?;
                let y = self.canvas_number(dom, arguments, 3)?;
                self.canvas_run(dom, receiver, CanvasOp::QuadraticCurveTo { cpx, cpy, x, y })
            }
            CanvasMethod::Fill => {
                let rule = self.canvas_fill_rule(dom, arguments, 0)?;
                self.canvas_run(dom, receiver, CanvasOp::Fill(rule))
            }
            CanvasMethod::Clip => {
                let rule = self.canvas_fill_rule(dom, arguments, 0)?;
                self.canvas_run(dom, receiver, CanvasOp::Clip(rule))
            }
            CanvasMethod::IsPointInPath => {
                let x = self.canvas_number(dom, arguments, 0)?;
                let y = self.canvas_number(dom, arguments, 1)?;
                let rule = self.canvas_fill_rule(dom, arguments, 2)?;
                self.canvas_run(dom, receiver, CanvasOp::IsPointInPath { x, y, rule })
            }
            CanvasMethod::IsPointInStroke => {
                let x = self.canvas_number(dom, arguments, 0)?;
                let y = self.canvas_number(dom, arguments, 1)?;
                self.canvas_run(dom, receiver, CanvasOp::IsPointInStroke { x, y })
            }
            CanvasMethod::GetImageData => {
                let x = self.canvas_long(dom, arguments, 0)?;
                let y = self.canvas_long(dom, arguments, 1)?;
                let width = self.canvas_long(dom, arguments, 2)?;
                let height = self.canvas_long(dom, arguments, 3)?;
                if width == 0.0 || height == 0.0 {
                    return Err(self.canvas_error(CanvasError {
                        kind: CanvasErrorKind::IndexSize,
                        message: "getImageData requires a non-zero width and height".to_owned(),
                    }));
                }
                // The saturating cast keeps an absurd request within the bound check.
                self.checked_pixel_bytes(width.abs() as u32, height.abs() as u32)?;
                let value = self.canvas_call(
                    dom,
                    receiver,
                    CanvasOp::GetImageData {
                        x,
                        y,
                        width,
                        height,
                    },
                )?;
                match value {
                    CanvasValue::Pixels(pixels) => self.image_data_from_pixels(pixels),
                    _ => Err(JsError::type_error("getImageData returned no pixels")),
                }
            }
            CanvasMethod::PutImageData => self.canvas_put_image_data(dom, receiver, arguments),
            CanvasMethod::CreateImageData => {
                let width = self.canvas_unsigned_long(dom, arguments, 0)?;
                let height = self.canvas_unsigned_long(dom, arguments, 1)?;
                if width == 0 || height == 0 {
                    return Err(self.canvas_error(CanvasError {
                        kind: CanvasErrorKind::IndexSize,
                        message: "createImageData requires a non-zero width and height".to_owned(),
                    }));
                }
                let bytes = self.checked_pixel_bytes(width, height)?;
                self.image_data_blank(width, height, bytes)
            }
        }
    }

    /// `HTMLCanvasElement.prototype.getContext`. The options argument is
    /// converted as `WebIDL` requires. These values are converted but not applied:
    /// `alpha: false`, which requires an opaque bitmap (every alpha byte 255,
    /// `clearRect` leaving opaque black); `colorSpace: "display-p3"`, which
    /// requires the bitmap to be display-p3; `colorType: "float16"`, which
    /// requires 16-bit float components; `desynchronized`, which the
    /// specification says the context "might be" desynchronized; and
    /// `willReadFrequently`, which only marks the context for readback
    /// optimization. Only `"2d"` is supported;
    /// every other kind answers `null`, as the specification allows for a
    /// context the user agent does not implement.
    fn canvas_get_context(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        // Both canvas kinds answer `getContext`: an element by its node, an
        // offscreen canvas by the id it was created with.
        let owner = match self.realm.host(receiver) {
            Some(ObjectHost::Node(node)) if is_html_canvas(dom, node) => Some(Ok(node)),
            Some(ObjectHost::Canvas(CanvasObject::OffscreenCanvas(id))) => Some(Err(id)),
            _ => None,
        };
        let Some(owner) = owner else {
            return Err(JsError::type_error("Illegal invocation"));
        };
        let kind = self.canvas_string(dom, arguments, 0)?;
        match kind.as_str() {
            "2d" => {}
            // Context kinds of the specification that this surface does not provide.
            "bitmaprenderer" | "webgl" | "webgl2" | "webgpu" => return Ok(JsValue::Null),
            _ => {
                return Err(JsError::type_error(format!(
                    "The provided value '{kind}' is not a valid enum value of type CanvasContextId."
                )));
            }
        }
        self.convert_context_settings(dom, arguments)?;
        let Some(host) = self.canvas_host.as_mut() else {
            return Ok(JsValue::Null);
        };
        let id = match owner {
            Ok(node) => host.canvas_for(dom, node),
            Err(id) => id,
        };
        self.context_object(id, receiver)
    }

    /// The `CanvasRenderingContext2DSettings` dictionary of `getContext`, converted
    /// as `WebIDL` requires: members are read in alphabetical order, so each getter
    /// runs once, and an enumerated value outside its set is a `TypeError`, even
    /// when the context already exists. The converted values are not applied to
    /// the context; the members this surface ignores are listed on
    /// `canvas_get_context`.
    fn convert_context_settings(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
    ) -> Result<(), JsError> {
        let object = match arguments.get(1) {
            None | Some(JsValue::Undefined | JsValue::Null) => return Ok(()),
            Some(JsValue::Object(object)) => *object,
            Some(_) => return Err(JsError::type_error("The options argument is not an object")),
        };
        self.get_value(dom, object, "alpha")?;
        let color_space = self.get_value(dom, object, "colorSpace")?;
        if !matches!(color_space, JsValue::Undefined) {
            let text = self.canvas_to_string(dom, &color_space)?;
            if !matches!(text.as_str(), "srgb" | "display-p3") {
                return Err(JsError::type_error(format!(
                    "The provided value '{text}' is not a valid enum value of type PredefinedColorSpace."
                )));
            }
        }
        let color_type = self.get_value(dom, object, "colorType")?;
        if !matches!(color_type, JsValue::Undefined) {
            let text = self.canvas_to_string(dom, &color_type)?;
            if !matches!(text.as_str(), "unorm8" | "float16") {
                return Err(JsError::type_error(format!(
                    "The provided value '{text}' is not a valid enum value of type CanvasColorType."
                )));
            }
        }
        self.get_value(dom, object, "desynchronized")?;
        self.get_value(dom, object, "willReadFrequently")?;
        Ok(())
    }

    /// The context object of `id`, created on first request. Later requests
    /// return the same object, as the specification requires.
    fn context_object(&mut self, id: CanvasId, canvas: ObjectId) -> Result<JsValue, JsError> {
        if let Some(existing) = self.canvas_contexts.get(&id) {
            return Ok(JsValue::Object(*existing));
        }
        let prototype = self.canvas_prototype("CanvasRenderingContext2D");
        self.ensure_heap_capacity(1)?;
        let object = self.realm.allocate(JsObject::with_host(
            prototype,
            ObjectHost::Canvas(CanvasObject::Context { id, canvas }),
        ));
        self.canvas_contexts.insert(id, object);
        Ok(JsValue::Object(object))
    }

    /// The gradient object of `id`, created on first request. Later reads of
    /// `fillStyle` return the same object, which the specification requires.
    fn gradient_object(&mut self, id: u64) -> Result<JsValue, JsError> {
        if let Some(existing) = self.canvas_gradient_objects.get(&id) {
            return Ok(JsValue::Object(*existing));
        }
        let prototype = self.canvas_prototype("CanvasGradient");
        self.ensure_heap_capacity(1)?;
        let object = self.realm.allocate(JsObject::with_host(
            prototype,
            ObjectHost::Canvas(CanvasObject::Gradient(id)),
        ));
        self.canvas_gradient_objects.insert(id, object);
        Ok(JsValue::Object(object))
    }

    /// Runs an operation whose receiver is a context and returns its value.
    fn canvas_run(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        op: CanvasOp,
    ) -> Result<JsValue, JsError> {
        let value = self.canvas_call(dom, receiver, op)?;
        Ok(self.canvas_value_to_js(value))
    }

    /// Forwards one operation for the context `receiver`, raising the
    /// specification's `DOMException` when the host refuses it.
    fn canvas_call(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        op: CanvasOp,
    ) -> Result<CanvasValue, JsError> {
        let id = self.canvas_context_id(receiver)?;
        self.canvas_execute(dom, id, op)
    }

    fn canvas_execute(
        &mut self,
        dom: &mut Dom,
        id: CanvasId,
        op: CanvasOp,
    ) -> Result<CanvasValue, JsError> {
        let Some(mut host) = self.canvas_host.take() else {
            return Err(JsError::type_error("the canvas surface is not available"));
        };
        let result = host.execute(dom, id, op);
        self.canvas_host = Some(host);
        result.map_err(|error| self.canvas_error(error))
    }

    fn canvas_error(&mut self, error: CanvasError) -> JsError {
        let name = match error.kind {
            CanvasErrorKind::IndexSize => DomExceptionName::IndexSize,
            CanvasErrorKind::Range => return self.range_error(&error.message),
            CanvasErrorKind::Syntax => DomExceptionName::Syntax,
        };
        match self.construct_dom_exception(name, &error.message) {
            Ok(value) => JsError::thrown(value),
            Err(error) => error,
        }
    }

    fn canvas_context_id(&self, receiver: ObjectId) -> Result<CanvasId, JsError> {
        match self.realm.host(receiver) {
            Some(ObjectHost::Canvas(CanvasObject::Context { id, .. })) => Ok(id),
            _ => Err(JsError::type_error("Illegal invocation")),
        }
    }

    /// The accessors of the context, the offscreen canvas and `ImageData`.
    fn canvas_get(
        &mut self,
        dom: &mut Dom,
        property: CanvasProperty,
        receiver: ObjectId,
    ) -> Result<JsValue, JsError> {
        match property {
            CanvasProperty::Canvas => match self.realm.host(receiver) {
                Some(ObjectHost::Canvas(CanvasObject::Context { canvas, .. })) => {
                    Ok(JsValue::Object(canvas))
                }
                _ => Err(JsError::type_error("Illegal invocation")),
            },
            CanvasProperty::Attribute(attribute) => {
                let id = self.canvas_context_id(receiver)?;
                let value = self.canvas_execute(dom, id, CanvasOp::Get(attribute))?;
                Ok(self.canvas_value_to_js(value))
            }
            CanvasProperty::OffscreenWidth | CanvasProperty::OffscreenHeight => {
                let Some(ObjectHost::Canvas(CanvasObject::OffscreenCanvas(id))) =
                    self.realm.host(receiver)
                else {
                    return Err(JsError::type_error("Illegal invocation"));
                };
                let Some(host) = self.canvas_host.as_mut() else {
                    return Ok(JsValue::Number(0.0));
                };
                let (width, height) = host.size(dom, id);
                let value = if property == CanvasProperty::OffscreenWidth {
                    width
                } else {
                    height
                };
                Ok(JsValue::Number(f64::from(value)))
            }
            CanvasProperty::ImageWidth
            | CanvasProperty::ImageHeight
            | CanvasProperty::ImageData => match self.realm.host(receiver) {
                Some(ObjectHost::Canvas(CanvasObject::ImageData {
                    width,
                    height,
                    data,
                })) => Ok(match property {
                    CanvasProperty::ImageWidth => JsValue::Number(f64::from(width)),
                    CanvasProperty::ImageHeight => JsValue::Number(f64::from(height)),
                    _ => JsValue::Object(data),
                }),
                _ => Err(JsError::type_error("Illegal invocation")),
            },
        }
    }

    fn canvas_set(
        &mut self,
        dom: &mut Dom,
        property: CanvasProperty,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let value = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        match property {
            CanvasProperty::Attribute(attribute) => {
                let id = self.canvas_context_id(receiver)?;
                let kind = attribute_kind(attribute);
                let converted = match kind {
                    AttributeKind::Number => {
                        CanvasValue::Number(self.to_number_value(dom, &value)?)
                    }
                    // A style accepts a gradient object itself; anything else
                    // converts as a string, which the host parses as a colour
                    // (HTML §4.12.5.1.3).
                    AttributeKind::Style => {
                        let gradient = match &value {
                            JsValue::Object(object) => match self.realm.host(*object) {
                                Some(ObjectHost::Canvas(
                                    crate::runtime::builtins::canvas::CanvasObject::Gradient(id),
                                )) => Some(id),
                                _ => None,
                            },
                            _ => None,
                        };
                        match gradient {
                            Some(id) => CanvasValue::Style(CanvasStyle::Gradient(id)),
                            None => CanvasValue::Text(self.canvas_to_string(dom, &value)?),
                        }
                    }
                    AttributeKind::Text => CanvasValue::Text(self.canvas_to_string(dom, &value)?),
                };
                self.canvas_execute(dom, id, CanvasOp::Set(attribute, converted))?;
                Ok(JsValue::Undefined)
            }
            CanvasProperty::OffscreenWidth | CanvasProperty::OffscreenHeight => {
                let Some(ObjectHost::Canvas(CanvasObject::OffscreenCanvas(id))) =
                    self.realm.host(receiver)
                else {
                    return Err(JsError::type_error("Illegal invocation"));
                };
                let number = self.to_number_value(dom, &value)?;
                let (current_width, current_height) = match self.canvas_host.as_mut() {
                    Some(host) => host.size(dom, id),
                    None => (0, 0),
                };
                let converted = enforce_unsigned_long(number)?;
                let (width, height) = if property == CanvasProperty::OffscreenWidth {
                    (converted, current_height)
                } else {
                    (current_width, converted)
                };
                if let Some(host) = self.canvas_host.as_mut() {
                    host.set_offscreen_size(id, width, height);
                }
                Ok(JsValue::Undefined)
            }
            CanvasProperty::Canvas
            | CanvasProperty::ImageWidth
            | CanvasProperty::ImageHeight
            | CanvasProperty::ImageData => Ok(JsValue::Undefined),
        }
    }

    /// `ImageData(sw, sh)` and `ImageData(data, sw[, sh])`.
    fn image_data_construct(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        if let Some(JsValue::Object(data)) = arguments.first() {
            let data = *data;
            let Some(ObjectHost::TypedArray { kind, length, .. }) = self.realm.host(data) else {
                return Err(JsError::type_error(
                    "ImageData requires a Uint8ClampedArray as its data",
                ));
            };
            if kind != TypedArrayKind::Uint8Clamped {
                return Err(JsError::type_error(
                    "ImageData requires a Uint8ClampedArray as its data",
                ));
            }
            let width = self.canvas_unsigned_long(dom, arguments, 1)?;
            let byte_length = length.unwrap_or(0);
            let height = if arguments.len() > 2 && !matches!(arguments[2], JsValue::Undefined) {
                self.canvas_unsigned_long(dom, arguments, 2)?
            } else {
                let row = u64::from(width) * 4;
                if width == 0 || row == 0 || !(byte_length as u64).is_multiple_of(row) {
                    return Err(self.canvas_error(CanvasError {
                        kind: CanvasErrorKind::IndexSize,
                        message: "ImageData data length is not a multiple of the row size"
                            .to_owned(),
                    }));
                }
                u32::try_from(byte_length as u64 / row).unwrap_or(u32::MAX)
            };
            if width == 0 || height == 0 {
                return Err(self.canvas_error(CanvasError {
                    kind: CanvasErrorKind::IndexSize,
                    message: "ImageData requires a non-zero width and height".to_owned(),
                }));
            }
            let expected = u64::from(width) * u64::from(height) * 4;
            if expected != byte_length as u64 {
                return Err(self.canvas_error(CanvasError {
                    kind: CanvasErrorKind::IndexSize,
                    message: "ImageData data length does not match its dimensions".to_owned(),
                }));
            }
            return self.image_data_object(width, height, data);
        }
        let width = self.canvas_unsigned_long(dom, arguments, 0)?;
        let height = self.canvas_unsigned_long(dom, arguments, 1)?;
        if width == 0 || height == 0 {
            return Err(self.canvas_error(CanvasError {
                kind: CanvasErrorKind::IndexSize,
                message: "ImageData requires a non-zero width and height".to_owned(),
            }));
        }
        let bytes = self.checked_pixel_bytes(width, height)?;
        self.image_data_blank(width, height, bytes)
    }

    /// The RGBA byte count of a `width` by `height` image, or a `RangeError`
    /// when the image would exceed `MAX_IMAGE_PIXELS`.
    fn checked_pixel_bytes(&mut self, width: u32, height: u32) -> Result<usize, JsError> {
        let pixels = u64::from(width) * u64::from(height);
        if pixels > MAX_IMAGE_PIXELS {
            return Err(self.range_error("the image is too large for the canvas surface"));
        }
        usize::try_from(pixels * 4)
            .map_err(|_| self.range_error("the image is too large for the canvas surface"))
    }

    /// An all-zero `ImageData` of the given size.
    fn image_data_blank(
        &mut self,
        width: u32,
        height: u32,
        bytes: usize,
    ) -> Result<JsValue, JsError> {
        let data = self.uint8_clamped_array(vec![0; bytes])?;
        self.image_data_object(width, height, data)
    }

    /// An `ImageData` over pixels the host produced.
    fn image_data_from_pixels(&mut self, pixels: PixelBuffer) -> Result<JsValue, JsError> {
        let data = self.uint8_clamped_array(pixels.data)?;
        self.image_data_object(pixels.width, pixels.height, data)
    }

    fn image_data_object(
        &mut self,
        width: u32,
        height: u32,
        data: ObjectId,
    ) -> Result<JsValue, JsError> {
        let prototype = self.canvas_prototype("ImageData");
        self.ensure_heap_capacity(1)?;
        let object = self.realm.allocate(JsObject::with_host(
            prototype,
            ObjectHost::Canvas(CanvasObject::ImageData {
                width,
                height,
                data,
            }),
        ));
        Ok(JsValue::Object(object))
    }

    /// A `Uint8ClampedArray` holding exactly `bytes`.
    fn uint8_clamped_array(&mut self, bytes: Vec<u8>) -> Result<ObjectId, JsError> {
        let length = bytes.len();
        let prototype = self.canvas_prototype_of_global("Uint8ClampedArray");
        self.ensure_heap_capacity(1)?;
        let buffer = TypedBuffer::new(bytes);
        Ok(self.realm.typed_array(
            TypedArrayKind::Uint8Clamped,
            buffer,
            0,
            Some(length),
            prototype,
        ))
    }

    /// `putImageData(imagedata, dx, dy[, dirtyX, dirtyY, dirtyWidth, dirtyHeight])`.
    fn canvas_put_image_data(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let Some(ObjectHost::Canvas(CanvasObject::ImageData {
            width,
            height,
            data,
        })) = arguments.first().and_then(|value| match value {
            JsValue::Object(object) => self.realm.host(*object),
            _ => None,
        })
        else {
            return Err(JsError::type_error(
                "putImageData requires an ImageData as its first argument",
            ));
        };
        let bytes = self.typed_bytes(data);
        let x = self.canvas_long(dom, arguments, 1)?;
        let y = self.canvas_long(dom, arguments, 2)?;
        let (dirty_x, dirty_y, dirty_width, dirty_height) = if arguments.len() > 3 {
            (
                self.canvas_long(dom, arguments, 3)?,
                self.canvas_long(dom, arguments, 4)?,
                self.canvas_long(dom, arguments, 5)?,
                self.canvas_long(dom, arguments, 6)?,
            )
        } else {
            (0.0, 0.0, f64::from(width), f64::from(height))
        };
        let pixels = PixelBuffer {
            width,
            height,
            data: bytes,
        };
        self.canvas_run(
            dom,
            receiver,
            CanvasOp::PutImageData {
                pixels,
                x,
                y,
                dirty_x,
                dirty_y,
                dirty_width,
                dirty_height,
            },
        )
    }

    /// The bytes a typed array views, read without copying through script.
    fn typed_bytes(&self, array: ObjectId) -> Vec<u8> {
        match self.realm.host(array) {
            Some(ObjectHost::TypedArray { buffer, .. }) => buffer.bytes(),
            _ => Vec::new(),
        }
    }

    fn canvas_value_to_js(&mut self, value: CanvasValue) -> JsValue {
        match value {
            CanvasValue::Undefined => JsValue::Undefined,
            CanvasValue::Bool(value) => JsValue::Boolean(value),
            CanvasValue::Number(value) => JsValue::Number(value),
            CanvasValue::Text(text) => JsValue::String(text.into()),
            CanvasValue::Numbers(values) => {
                let elements: Vec<JsValue> = values.into_iter().map(JsValue::Number).collect();
                match self.create_array_from_values(&elements) {
                    Ok(array) => JsValue::Object(array),
                    Err(_) => JsValue::Null,
                }
            }
            CanvasValue::Style(CanvasStyle::Color(text)) => JsValue::String(text.into()),
            // The getter returns the gradient object itself, so `fillStyle`
            // reads back as what was assigned (HTML §4.12.5.1.3).
            CanvasValue::Style(CanvasStyle::Gradient(id)) => self
                .canvas_gradient_objects
                .get(&id)
                .map(|object| JsValue::Object(*object))
                .unwrap_or(JsValue::Undefined),
            // Pixels are wrapped into an ImageData by `getImageData` before they
            // reach this conversion, so no operation returns them through here.
            CanvasValue::Pixels(_) => JsValue::Undefined,
        }
    }

    fn canvas_prototype(&self, name: &str) -> Option<ObjectId> {
        self.canvas_prototype_of_global(name)
    }

    fn canvas_prototype_of_global(&self, name: &str) -> Option<ObjectId> {
        match self.realm.global(name) {
            Some(JsValue::Object(constructor)) => {
                match self.realm.get_property(constructor, "prototype") {
                    Some(JsValue::Object(prototype)) => Some(prototype),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// The `index`th argument as a number, or the `WebIDL` missing-argument error.
    fn canvas_number(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
        index: usize,
    ) -> Result<f64, JsError> {
        match arguments.get(index) {
            Some(value) => self.to_number_value(dom, value),
            None => Err(missing_argument()),
        }
    }

    /// The first `count` arguments as `WebIDL` doubles, each required.
    fn canvas_numbers(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
        count: usize,
    ) -> Result<Vec<f64>, JsError> {
        (0..count)
            .map(|index| self.canvas_number(dom, arguments, index))
            .collect()
    }

    /// The `index`th argument as a `WebIDL` `long`.
    fn canvas_long(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
        index: usize,
    ) -> Result<f64, JsError> {
        let number = self.canvas_number(dom, arguments, index)?;
        Ok(f64::from(signed_long(number)))
    }

    /// The `index`th argument as a `WebIDL` `unsigned long`.
    fn canvas_unsigned_long(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
        index: usize,
    ) -> Result<u32, JsError> {
        let number = self.canvas_number(dom, arguments, index)?;
        Ok(unsigned_long(number))
    }

    fn canvas_string(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
        index: usize,
    ) -> Result<String, JsError> {
        match arguments.get(index) {
            Some(value) => self.canvas_to_string(dom, value),
            None => Err(missing_argument()),
        }
    }

    fn canvas_to_string(&mut self, dom: &mut Dom, value: &JsValue) -> Result<String, JsError> {
        let text = self.to_string_value(dom, value)?;
        Ok(text.to_lossy())
    }

    /// A `WebIDL` boolean argument; an absent argument is `false`.
    fn canvas_flag(arguments: &[JsValue], index: usize) -> bool {
        arguments.get(index).is_some_and(JsValue::is_truthy)
    }

    /// Whether every value is finite, the gate every canvas method applies
    /// before it draws (HTML §4.12.5.1.1: a nonfinite argument draws nothing).
    fn all_finite(values: &[f64]) -> bool {
        values.iter().all(|value| value.is_finite())
    }

    /// The radii of `roundRect`, normalized per HTML §4.12.5.1.4: a missing
    /// argument is `0`; a number is one radius for both axes; an object is a
    /// `DOMPointInit` read through `x` and `y` with the dictionary default of
    /// `0` (so `{ foo: "bar" }` and `[[]]` are all-zero radii, and a `BigInt`
    /// member is a `TypeError`); an array is a sequence of those, whose length
    /// must be one to four.
    fn round_rect_radii(
        &mut self,
        dom: &mut Dom,
        value: Option<&JsValue>,
    ) -> Result<Vec<(f64, f64)>, JsError> {
        let value = match value {
            None | Some(JsValue::Undefined) => return Ok(vec![(0.0, 0.0)]),
            Some(value) => value,
        };
        let sequence = matches!(value, JsValue::Object(_)) && self.is_array_value(value)?;
        let items: Vec<JsValue> = if sequence {
            self.iterate_values(dom, value)?
        } else {
            vec![value.clone()]
        };
        if sequence && !(1..=4).contains(&items.len()) {
            return Err(self.range_error(
                "Failed to execute 'roundRect' on 'CanvasRenderingContext2D': \
                 the radii sequence must hold one to four items",
            ));
        }
        let mut radii = Vec::with_capacity(items.len());
        for item in &items {
            radii.push(self.round_rect_radius_item(dom, item)?);
        }
        for (radius_x, radius_y) in &radii {
            if *radius_x < 0.0 || *radius_y < 0.0 {
                return Err(self.range_error(
                    "Failed to execute 'roundRect' on 'CanvasRenderingContext2D': \
                     the radii must be non-negative",
                ));
            }
        }
        Ok(radii)
    }

    /// One `roundRect` radius item: the union `(unrestricted double or
    /// DOMPointInit)`. A number is circular; an object (an array included) is
    /// a `DOMPointInit`; `undefined` and `null` are the empty dictionary;
    /// everything else converts as a double, where a `BigInt` is a
    /// `TypeError`.
    fn round_rect_radius_item(
        &mut self,
        dom: &mut Dom,
        value: &JsValue,
    ) -> Result<(f64, f64), JsError> {
        match value {
            JsValue::Number(number) => Ok((*number, *number)),
            JsValue::Object(_) => {
                let x = self.round_rect_point_member(dom, value, "x")?;
                let y = self.round_rect_point_member(dom, value, "y")?;
                Ok((x, y))
            }
            JsValue::Undefined | JsValue::Null => Ok((0.0, 0.0)),
            other => {
                let radius = to_number(other)?;
                Ok((radius, radius))
            }
        }
    }

    /// One `DOMPointInit` member: `undefined` is the dictionary default `0`,
    /// and everything present converts as an `unrestricted double`.
    fn round_rect_point_member(
        &mut self,
        dom: &mut Dom,
        value: &JsValue,
        member: &str,
    ) -> Result<f64, JsError> {
        let JsValue::Object(object) = value else {
            return Ok(0.0);
        };
        match self.get_value(dom, *object, member)? {
            JsValue::Undefined => Ok(0.0),
            present => to_number(&present),
        }
    }

    /// Build the `roundRect` path (HTML §4.12.5.1.4) from the primitives the
    /// path model already has: one `moveTo`, four straight edges and four
    /// elliptical arcs, closed. Nonfinite geometry draws nothing, as every
    /// canvas method with a nonfinite argument does.
    #[allow(
        clippy::too_many_arguments,
        reason = "the method's own signature is x, y, width, height plus the radii list"
    )]
    fn round_rect_path(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        x: f64,
        y: f64,
        width: f64,
        height: f64,
        radii: Vec<(f64, f64)>,
    ) -> Result<(), JsError> {
        if !Self::all_finite(&[x, y, width, height]) {
            return Ok(());
        }
        // The match expands one, two and three radii to the four corners per
        // the standard's own rule; an empty list is the all-zero rectangle.
        let mut corners = match radii.as_slice() {
            [tl, tr, br, bl] => [*tl, *tr, *br, *bl],
            [single] => [*single; 4],
            [first, second] => [*first, *second, *first, *second],
            [first, second, third] => [*first, *second, *third, *second],
            _ => [(0.0, 0.0); 4],
        };
        // A negative width or height flips the rectangle, so the corners the
        // names refer to swap with it.
        if width < 0.0 {
            corners.swap(0, 1);
            corners.swap(2, 3);
        }
        if height < 0.0 {
            corners.swap(0, 3);
            corners.swap(1, 2);
        }
        // Scale every radius by the one factor that keeps adjacent corners of
        // an edge from overlapping.
        let edge_width = width.abs();
        let edge_height = height.abs();
        let mut factor: f64 = 1.0;
        for (sum, length) in [
            (corners[0].0 + corners[1].0, edge_width),
            (corners[2].0 + corners[3].0, edge_width),
            (corners[0].1 + corners[3].1, edge_height),
            (corners[1].1 + corners[2].1, edge_height),
        ] {
            if sum > length && sum > 0.0 {
                factor = factor.min(length / sum);
            }
        }
        for corner in &mut corners {
            corner.0 *= factor;
            corner.1 *= factor;
        }
        let [tl, tr, br, bl] = corners;
        // Both dimensions of one sign traverse clockwise; mixed signs reverse.
        let clockwise = (width >= 0.0) == (height >= 0.0);
        // A negative width or height puts the named origin outside the shape:
        // the edges are the axis-aligned extremes, and the swapped corners
        // belong to them.
        let left = x.min(x + width);
        let top = y.min(y + height);
        let right = x.max(x + width);
        let bottom = y.max(y + height);
        // A corner with no radii contributes no arc: the straight edges meet,
        // and a zero-radius ellipse would add a stray point.
        let arc = |runtime: &mut Self,
                   center: (f64, f64),
                   radii: (f64, f64),
                   start: f64,
                   end: f64,
                   dom: &mut Dom,
                   receiver: ObjectId|
         -> Result<(), JsError> {
            if radii.0 == 0.0 && radii.1 == 0.0 {
                return Ok(());
            }
            runtime
                .canvas_run(
                    dom,
                    receiver,
                    CanvasOp::Ellipse {
                        x: center.0,
                        y: center.1,
                        radius_x: radii.0,
                        radius_y: radii.1,
                        rotation: 0.0,
                        start,
                        end,
                        counter_clockwise: !clockwise,
                    },
                )
                .map(|_| ())
        };
        self.canvas_run(
            dom,
            receiver,
            CanvasOp::MoveTo {
                x: left + tl.0,
                y: top,
            },
        )?;
        if clockwise {
            self.canvas_run(
                dom,
                receiver,
                CanvasOp::LineTo {
                    x: right - tr.0,
                    y: top,
                },
            )?;
            arc(
                self,
                (right - tr.0, top + tr.1),
                tr,
                -FRAC_PI_2,
                0.0,
                dom,
                receiver,
            )?;
            self.canvas_run(
                dom,
                receiver,
                CanvasOp::LineTo {
                    x: right,
                    y: bottom - br.1,
                },
            )?;
            arc(
                self,
                (right - br.0, bottom - br.1),
                br,
                0.0,
                FRAC_PI_2,
                dom,
                receiver,
            )?;
            self.canvas_run(
                dom,
                receiver,
                CanvasOp::LineTo {
                    x: left + bl.0,
                    y: bottom,
                },
            )?;
            arc(
                self,
                (left + bl.0, bottom - bl.1),
                bl,
                FRAC_PI_2,
                PI,
                dom,
                receiver,
            )?;
            self.canvas_run(
                dom,
                receiver,
                CanvasOp::LineTo {
                    x: left,
                    y: top + tl.1,
                },
            )?;
            arc(
                self,
                (left + tl.0, top + tl.1),
                tl,
                PI,
                PI + FRAC_PI_2,
                dom,
                receiver,
            )?;
        } else {
            self.canvas_run(
                dom,
                receiver,
                CanvasOp::LineTo {
                    x: left,
                    y: top + tl.1,
                },
            )?;
            arc(
                self,
                (left + tl.0, top + tl.1),
                tl,
                -FRAC_PI_2,
                -PI,
                dom,
                receiver,
            )?;
            self.canvas_run(
                dom,
                receiver,
                CanvasOp::LineTo {
                    x: left,
                    y: bottom - bl.1,
                },
            )?;
            arc(
                self,
                (left + bl.0, bottom - bl.1),
                bl,
                PI,
                FRAC_PI_2,
                dom,
                receiver,
            )?;
            self.canvas_run(
                dom,
                receiver,
                CanvasOp::LineTo {
                    x: right - br.0,
                    y: bottom,
                },
            )?;
            arc(
                self,
                (right - br.0, bottom - br.1),
                br,
                FRAC_PI_2,
                0.0,
                dom,
                receiver,
            )?;
            self.canvas_run(
                dom,
                receiver,
                CanvasOp::LineTo {
                    x: right,
                    y: top + tr.1,
                },
            )?;
            arc(
                self,
                (right - tr.0, top + tr.1),
                tr,
                0.0,
                -FRAC_PI_2,
                dom,
                receiver,
            )?;
        }
        self.canvas_run(dom, receiver, CanvasOp::ClosePath)
            .map(|_| ())
    }

    /// The four numbers of a rectangle method, each required.
    fn canvas_rect_args(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
    ) -> Result<[f64; 4], JsError> {
        Ok([
            self.canvas_number(dom, arguments, 0)?,
            self.canvas_number(dom, arguments, 1)?,
            self.canvas_number(dom, arguments, 2)?,
            self.canvas_number(dom, arguments, 3)?,
        ])
    }

    /// The six numbers of a transformation matrix, each required.
    fn canvas_matrix(&mut self, dom: &mut Dom, arguments: &[JsValue]) -> Result<[f64; 6], JsError> {
        let mut matrix = [0.0; 6];
        for (index, slot) in matrix.iter_mut().enumerate() {
            *slot = self.canvas_number(dom, arguments, index)?;
        }
        Ok(matrix)
    }

    /// A sequence of numbers, as `setLineDash` takes.
    fn canvas_number_sequence(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
    ) -> Result<Vec<f64>, JsError> {
        let Some(JsValue::Object(array)) = arguments.first() else {
            return Err(JsError::type_error(
                "setLineDash requires a sequence of numbers",
            ));
        };
        let length_value = self
            .realm
            .get_property(*array, "length")
            .unwrap_or(JsValue::Number(0.0));
        let length = self.to_number_value(dom, &length_value)?;
        let length = if length.is_finite() && length > 0.0 {
            length as usize
        } else {
            0
        };
        // The length is the script's own claim, so memory is reserved as the
        // elements are read rather than up front.
        let mut values = Vec::new();
        for index in 0..length {
            let element = self
                .realm
                .get_property(*array, &index.to_string())
                .unwrap_or(JsValue::Undefined);
            values.push(self.to_number_value(dom, &element)?);
        }
        Ok(values)
    }

    /// An optional fill rule: `"nonzero"` by default, or `"evenodd"`.
    fn canvas_fill_rule(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
        index: usize,
    ) -> Result<FillRule, JsError> {
        match arguments.get(index) {
            None | Some(JsValue::Undefined) => Ok(FillRule::NonZero),
            Some(value) => {
                let text = self.canvas_to_string(dom, value)?;
                match text.as_str() {
                    "nonzero" => Ok(FillRule::NonZero),
                    "evenodd" => Ok(FillRule::EvenOdd),
                    _ => Err(JsError::type_error(format!(
                        "The provided value '{text}' is not a valid enum value of type CanvasFillRule."
                    ))),
                }
            }
        }
    }
}

/// Whether `node` is an HTML `canvas` element.
fn is_html_canvas(dom: &Dom, node: NodeId) -> bool {
    matches!(
        dom.node(node).map(render_dom::Node::kind),
        Some(NodeKind::Element(element))
            if element.namespace == Namespace::Html && element.local_name == "canvas"
    )
}

fn attribute_kind(attribute: Attribute) -> AttributeKind {
    CONTEXT_ATTRIBUTES
        .iter()
        .find(|(_, candidate, _)| *candidate == attribute)
        .map_or(AttributeKind::Number, |(_, _, kind)| *kind)
}

/// The `WebIDL` `unsigned long` conversion of an already-converted number.
fn unsigned_long(number: f64) -> u32 {
    if !number.is_finite() {
        return 0;
    }
    let truncated = number.trunc();
    let modulo = truncated.rem_euclid(4_294_967_296.0);
    modulo as u32
}

/// The `WebIDL` `[EnforceRange] unsigned long` conversion: a value that is not
/// finite, or lies outside `0..=4294967295` after truncation, is a `TypeError`.
fn enforce_unsigned_long(number: f64) -> Result<u32, JsError> {
    let truncated = number.trunc();
    if !number.is_finite() || !(0.0..=4_294_967_295.0).contains(&truncated) {
        return Err(JsError::type_error(
            "Value is outside the 'unsigned long' value range",
        ));
    }
    Ok(truncated as u32)
}

/// The `WebIDL` `long` conversion of an already-converted number.
fn signed_long(number: f64) -> i32 {
    if !number.is_finite() {
        return 0;
    }
    let truncated = number.trunc();
    let modulo = truncated.rem_euclid(4_294_967_296.0);
    if modulo >= 2_147_483_648.0 {
        (modulo - 4_294_967_296.0) as i32
    } else {
        modulo as i32
    }
}

fn missing_argument() -> JsError {
    JsError::type_error("Not enough arguments")
}

/// The `CanvasRenderingContext2D`, `OffscreenCanvas` and `ImageData`
/// interfaces, and `HTMLCanvasElement.prototype.getContext`.
pub(crate) fn install_canvas(
    objects: &mut Vec<JsObject>,
    global: ObjectId,
    object_prototype: ObjectId,
    function_prototype: ObjectId,
) {
    let context_prototype = new_prototype(objects, object_prototype);
    install_methods(
        objects,
        context_prototype,
        function_prototype,
        CONTEXT_METHODS,
    );
    install_accessor(
        objects,
        context_prototype,
        function_prototype,
        "canvas",
        CanvasProperty::Canvas,
        false,
    );
    for (name, attribute, _) in CONTEXT_ATTRIBUTES {
        install_accessor(
            objects,
            context_prototype,
            function_prototype,
            name,
            CanvasProperty::Attribute(*attribute),
            true,
        );
    }
    install_interface(
        objects,
        global,
        function_prototype,
        "CanvasRenderingContext2D",
        CanvasInterface::Context2d,
        context_prototype,
    );

    let offscreen_prototype = new_prototype(objects, object_prototype);
    install_methods(
        objects,
        offscreen_prototype,
        function_prototype,
        &[("getContext", CanvasMethod::GetContext)],
    );
    for (name, property) in [
        ("width", CanvasProperty::OffscreenWidth),
        ("height", CanvasProperty::OffscreenHeight),
    ] {
        install_accessor(
            objects,
            offscreen_prototype,
            function_prototype,
            name,
            property,
            true,
        );
    }
    install_interface(
        objects,
        global,
        function_prototype,
        "OffscreenCanvas",
        CanvasInterface::OffscreenCanvas,
        offscreen_prototype,
    );

    let image_data_prototype = new_prototype(objects, object_prototype);
    for (name, property, setter) in [
        ("width", CanvasProperty::ImageWidth, false),
        ("height", CanvasProperty::ImageHeight, false),
        ("data", CanvasProperty::ImageData, false),
    ] {
        install_accessor(
            objects,
            image_data_prototype,
            function_prototype,
            name,
            property,
            setter,
        );
    }
    install_interface(
        objects,
        global,
        function_prototype,
        "ImageData",
        CanvasInterface::ImageData,
        image_data_prototype,
    );

    // HTML §4.12.5.1.3: `CanvasGradient` has no constructor of its own; its
    // instances come from the context's `createLinearGradient` and
    // `createRadialGradient`.
    let gradient_prototype = new_prototype(objects, object_prototype);
    install_methods(
        objects,
        gradient_prototype,
        function_prototype,
        &[("addColorStop", CanvasMethod::GradientAddColorStop)],
    );
    install_interface(
        objects,
        global,
        function_prototype,
        "CanvasGradient",
        CanvasInterface::Gradient,
        gradient_prototype,
    );

    if let Some(JsValue::Object(canvas_constructor)) = objects[global.0]
        .properties
        .get("HTMLCanvasElement")
        .map(|d| d.value.clone())
        && let Some(JsValue::Object(canvas_prototype)) = objects[canvas_constructor.0]
            .properties
            .get("prototype")
            .map(|d| d.value.clone())
    {
        install_methods(
            objects,
            canvas_prototype,
            function_prototype,
            &[("getContext", CanvasMethod::GetContext)],
        );
    }
}

/// The methods of `CanvasRenderingContext2D` in this surface.
const CONTEXT_METHODS: &[(&str, CanvasMethod)] = &[
    ("save", CanvasMethod::Save),
    ("restore", CanvasMethod::Restore),
    ("reset", CanvasMethod::Reset),
    ("scale", CanvasMethod::Scale),
    ("rotate", CanvasMethod::Rotate),
    ("translate", CanvasMethod::Translate),
    ("transform", CanvasMethod::Transform),
    ("setTransform", CanvasMethod::SetTransform),
    ("resetTransform", CanvasMethod::ResetTransform),
    ("setLineDash", CanvasMethod::SetLineDash),
    ("getLineDash", CanvasMethod::GetLineDash),
    ("fillRect", CanvasMethod::FillRect),
    ("clearRect", CanvasMethod::ClearRect),
    ("strokeRect", CanvasMethod::StrokeRect),
    ("beginPath", CanvasMethod::BeginPath),
    ("closePath", CanvasMethod::ClosePath),
    ("moveTo", CanvasMethod::MoveTo),
    ("lineTo", CanvasMethod::LineTo),
    ("rect", CanvasMethod::Rect),
    ("arc", CanvasMethod::Arc),
    ("arcTo", CanvasMethod::ArcTo),
    ("ellipse", CanvasMethod::Ellipse),
    ("roundRect", CanvasMethod::RoundRect),
    ("createLinearGradient", CanvasMethod::CreateLinearGradient),
    ("createRadialGradient", CanvasMethod::CreateRadialGradient),
    ("bezierCurveTo", CanvasMethod::BezierCurveTo),
    ("quadraticCurveTo", CanvasMethod::QuadraticCurveTo),
    ("fill", CanvasMethod::Fill),
    ("stroke", CanvasMethod::Stroke),
    ("clip", CanvasMethod::Clip),
    ("isPointInPath", CanvasMethod::IsPointInPath),
    ("isPointInStroke", CanvasMethod::IsPointInStroke),
    ("getImageData", CanvasMethod::GetImageData),
    ("putImageData", CanvasMethod::PutImageData),
    ("createImageData", CanvasMethod::CreateImageData),
];

fn new_prototype(objects: &mut Vec<JsObject>, object_prototype: ObjectId) -> ObjectId {
    let prototype = ObjectId(objects.len());
    objects.push(JsObject::with_host(
        Some(object_prototype),
        ObjectHost::default(),
    ));
    prototype
}

fn install_methods(
    objects: &mut Vec<JsObject>,
    target: ObjectId,
    function_prototype: ObjectId,
    methods: &[(&str, CanvasMethod)],
) {
    for (name, method) in methods {
        let function = ObjectId(objects.len());
        objects.push(JsObject::with_host(
            Some(function_prototype),
            ObjectHost::NativeFunction(NativeFunction::Canvas(*method)),
        ));
        objects[target.0].properties.insert(
            (*name).to_owned(),
            PropertyDescriptor::builtin(JsValue::Object(function)),
        );
    }
}

fn install_accessor(
    objects: &mut Vec<JsObject>,
    target: ObjectId,
    function_prototype: ObjectId,
    name: &str,
    property: CanvasProperty,
    writable: bool,
) {
    let getter = ObjectId(objects.len());
    objects.push(JsObject::with_host(
        Some(function_prototype),
        ObjectHost::NativeFunction(NativeFunction::CanvasGetter(property)),
    ));
    let setter = if writable {
        let setter = ObjectId(objects.len());
        objects.push(JsObject::with_host(
            Some(function_prototype),
            ObjectHost::NativeFunction(NativeFunction::CanvasSetter(property)),
        ));
        Some(setter)
    } else {
        None
    };
    objects[target.0].properties.insert(
        name.to_owned(),
        PropertyDescriptor {
            value: JsValue::Undefined,
            writable: false,
            getter: Some(getter),
            setter,
            enumerable: false,
            configurable: true,
        },
    );
}

fn install_interface(
    objects: &mut Vec<JsObject>,
    global: ObjectId,
    function_prototype: ObjectId,
    name: &str,
    interface: CanvasInterface,
    prototype: ObjectId,
) {
    let constructor = ObjectId(objects.len());
    objects.push(JsObject::with_host(
        Some(function_prototype),
        ObjectHost::Canvas(CanvasObject::Constructor(interface)),
    ));
    objects[constructor.0].properties.insert(
        "prototype".to_owned(),
        PropertyDescriptor {
            getter: None,
            setter: None,
            value: JsValue::Object(prototype),
            writable: false,
            enumerable: false,
            configurable: false,
        },
    );
    objects[prototype.0].properties.insert(
        "constructor".to_owned(),
        PropertyDescriptor::builtin(JsValue::Object(constructor)),
    );
    // WebIDL §2.7.2: the interface prototype's @@toStringTag names the
    // interface, which is what `Object.prototype.toString` reads.
    let tag = JsSymbol::well_known("@@toStringTag");
    crate::value::define_symbol_property_on(
        objects,
        prototype,
        &tag,
        PropertyDescriptor {
            getter: None,
            setter: None,
            value: JsValue::String((name.to_owned()).into()),
            writable: false,
            enumerable: false,
            configurable: true,
        },
    );
    objects[global.0].properties.insert(
        name.to_owned(),
        PropertyDescriptor {
            getter: None,
            setter: None,
            value: JsValue::Object(constructor),
            writable: true,
            enumerable: false,
            configurable: true,
        },
    );
}
