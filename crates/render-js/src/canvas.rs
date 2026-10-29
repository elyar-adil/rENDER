//! The embedder seam for `<canvas>` and `OffscreenCanvas` 2D rendering contexts
//! (HTML Standard §4.12.5, "The canvas element" and "The 2D rendering context").
//!
//! The runtime owns the JavaScript objects: the context, the gradient and
//! pattern handles, and the `ImageData` wrappers. The pixels, the drawing state
//! and the rasterization belong to the embedder, which implements
//! [`CanvasHost`] and shares its pixel store with the paint path. Each method
//! of `CanvasRenderingContext2D` reaches the host as one [`CanvasOp`] whose
//! arguments are already converted to Rust values, so the host never sees a
//! JavaScript value.

use render_dom::{Dom, NodeId};

/// The largest number of pixels a single `ImageData` or `getImageData` result may
/// hold (33,554,432 pixels, 128 MiB of RGBA bytes). A larger request throws a
/// `RangeError` before any memory is allocated for it.
pub const MAX_IMAGE_PIXELS: u64 = 1 << 25;

/// Identity of one canvas: an element's bitmap or an `OffscreenCanvas`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CanvasId(pub u64);

/// The fill rule of a path fill, clip or hit test (HTML §4.12.5.1.14).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillRule {
    NonZero,
    EvenOdd,
}

/// A `CanvasRenderingContext2D` attribute that holds a plain value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Attribute {
    Direction,
    FillStyle,
    Filter,
    Font,
    FontKerning,
    FontStretch,
    FontVariantCaps,
    GlobalAlpha,
    GlobalCompositeOperation,
    ImageSmoothingEnabled,
    ImageSmoothingQuality,
    LetterSpacing,
    LineCap,
    LineDashOffset,
    LineJoin,
    LineWidth,
    MiterLimit,
    ShadowBlur,
    ShadowColor,
    ShadowOffsetX,
    ShadowOffsetY,
    StrokeStyle,
    TextAlign,
    TextBaseline,
    TextRendering,
    WordSpacing,
}

/// A style value: a colour in its CSS text form, or a gradient the host
/// holds by id (HTML §4.12.5.1.3). The id is the registry key both sides
/// agreed on when the gradient was created; the definition itself travels
/// through [`GradientOp`], so a later `addColorStop` reaches the paint the
/// same way the creation did.
#[derive(Clone, Debug, PartialEq)]
pub enum CanvasStyle {
    Color(String),
    Gradient(u64),
}

/// Straight-alpha RGBA8 pixels, row-major, `4 * width * height` bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PixelBuffer {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// A value crossing the seam in either direction.
#[derive(Clone, Debug, PartialEq)]
pub enum CanvasValue {
    Undefined,
    Bool(bool),
    Number(f64),
    Text(String),
    Numbers(Vec<f64>),
    Style(CanvasStyle),
    Pixels(PixelBuffer),
}

/// A source a `drawImage` call can draw (HTML §4.12.5.1.16).
///
/// The canvases are the sources whose pixels the host already owns: every
/// element canvas and every `OffscreenCanvas` of the page shares one engine.
/// An image element names its node instead; the host resolves it against the
/// decoded pixels the embedder installed for that node, so the script runtime
/// never holds image data. Video and `ImageBitmap` sources remain unimplemented
/// and are rejected with the `TypeError` an unsupported `CanvasImageSource`
/// gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CanvasImageSource {
    /// A bitmap of the same engine: an element's canvas or an `OffscreenCanvas`.
    Canvas(CanvasId),
    /// An image element whose decoded pixels the embedder installed.
    Node(NodeId),
}

/// One `CanvasRenderingContext2D` operation, with its arguments converted.
#[derive(Clone, Debug, PartialEq)]
pub enum CanvasOp {
    Save,
    Restore,
    Reset,
    Scale {
        x: f64,
        y: f64,
    },
    Rotate(f64),
    Translate {
        x: f64,
        y: f64,
    },
    Transform([f64; 6]),
    SetTransform([f64; 6]),
    ResetTransform,
    Get(Attribute),
    Set(Attribute, CanvasValue),
    SetLineDash(Vec<f64>),
    GetLineDash,
    FillRect {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    },
    ClearRect {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    },
    StrokeRect {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    },
    BeginPath,
    ClosePath,
    MoveTo {
        x: f64,
        y: f64,
    },
    LineTo {
        x: f64,
        y: f64,
    },
    Rect {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    },
    Arc {
        x: f64,
        y: f64,
        radius: f64,
        start: f64,
        end: f64,
        counter_clockwise: bool,
    },
    ArcTo {
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
        radius: f64,
    },
    Ellipse {
        x: f64,
        y: f64,
        radius_x: f64,
        radius_y: f64,
        rotation: f64,
        start: f64,
        end: f64,
        counter_clockwise: bool,
    },
    BezierCurveTo {
        cp1x: f64,
        cp1y: f64,
        cp2x: f64,
        cp2y: f64,
        x: f64,
        y: f64,
    },
    QuadraticCurveTo {
        cpx: f64,
        cpy: f64,
        x: f64,
        y: f64,
    },
    Fill(FillRule),
    Stroke,
    Clip(FillRule),
    IsPointInPath {
        x: f64,
        y: f64,
        rule: FillRule,
    },
    IsPointInStroke {
        x: f64,
        y: f64,
    },
    GetImageData {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    },
    PutImageData {
        pixels: PixelBuffer,
        x: f64,
        y: f64,
        dirty_x: f64,
        dirty_y: f64,
        dirty_width: f64,
        dirty_height: f64,
    },
    /// `HTMLCanvasElement.prototype.toDataURL`: a `data:` URL of the bitmap
    /// encoded in `mime_type`, with `quality` when it is a usable number
    /// (HTML §4.12.5.3).
    ToDataUrl {
        mime_type: String,
        quality: Option<f64>,
    },
    /// `drawImage` (HTML §4.12.5.1.16): `source_rect` is `(sx, sy, sw, sh)`
    /// in source pixels and `dest_rect` is `(dx, dy, dw, dh)` in the current
    /// user space. The script side has already applied the overload defaults
    /// and the zero/extent checks the specification names.
    DrawImage {
        source: CanvasImageSource,
        source_rect: [f64; 4],
        dest_rect: [f64; 4],
    },
}

/// A failure that the script sees as a `DOMException` of the named kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanvasError {
    pub kind: CanvasErrorKind,
    pub message: String,
}

/// The `DOMException` names a canvas operation can raise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CanvasErrorKind {
    IndexSize,
    /// A size the implementation will not allocate; reported as a `RangeError`.
    Range,
    /// A colour that does not parse; reported as a `SyntaxError`.
    Syntax,
}

/// The host side of the 2D canvas seam.
pub trait CanvasHost: std::fmt::Debug {
    /// The canvas of a `<canvas>` element, creating its bitmap on first use.
    /// Its size is read from the element's `width` and `height` attributes
    /// whenever the host needs it.
    fn canvas_for(&mut self, dom: &Dom, node: NodeId) -> CanvasId;

    /// A new `OffscreenCanvas` bitmap of the given size.
    fn create_offscreen(&mut self, width: u32, height: u32) -> CanvasId;

    /// The bitmap size of a canvas: its attributes for an element, its own
    /// size for an offscreen canvas.
    fn size(&mut self, dom: &Dom, canvas: CanvasId) -> (u32, u32);

    /// Sets the size of an offscreen canvas, resetting its bitmap.
    fn set_offscreen_size(&mut self, canvas: CanvasId, width: u32, height: u32);

    /// Runs one context operation.
    ///
    /// # Errors
    ///
    /// Returns the `DOMException` the operation raises, when it raises one.
    fn execute(
        &mut self,
        dom: &Dom,
        canvas: CanvasId,
        op: CanvasOp,
    ) -> Result<CanvasValue, CanvasError>;

    /// Creates or amends a gradient in the host's registry (HTML
    /// §4.12.5.1.3). Gradients are context-independent: a gradient made on
    /// one canvas paints on another, so the registry is the host's own, not
    /// one canvas's state.
    ///
    /// The default is a host that has no gradient support.
    ///
    /// # Errors
    ///
    /// Returns the `DOMException` the operation raises: an `IndexSizeError`
    /// for an offset outside `0..=1`, a `SyntaxError` for a colour that does
    /// not parse.
    fn define_gradient(&mut self, op: GradientOp) -> Result<(), CanvasError> {
        let _ = op;
        Ok(())
    }
}

/// One gradient registry operation.
#[derive(Clone, Debug, PartialEq)]
pub enum GradientOp {
    Create { id: u64, kind: GradientKind },
    AddColorStop { id: u64, offset: f64, color: String },
}

/// The geometry of a gradient, in the user space of the paint that uses it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GradientKind {
    Linear {
        x0: f64,
        y0: f64,
        x1: f64,
        y1: f64,
    },
    Radial {
        x0: f64,
        y0: f64,
        r0: f64,
        x1: f64,
        y1: f64,
        r1: f64,
    },
}
