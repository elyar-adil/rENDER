//! Display-list construction, incremental diffing, and CPU reference painting.

mod color;
mod display_list;
mod raster;

pub use color::{Color, SystemPalette};
pub use display_list::{
    BlendMode, BorderPaint, BoxShadowPaint, CanvasResourceId, ClipShape, CompositingReason,
    DisplayCommand, DisplayItem, DisplayItemId, DisplayList, DisplayListBuildOutput,
    DisplayListBuilderLimits, DisplayListBuilderOptions, DisplayListDiagnostic,
    DisplayListDiagnosticCode, DisplayListDiff, FontInstanceId, GlyphId, GlyphInstance, GlyphRun,
    GradientStop, ImagePaint, ImageResourceId, LinearGradient, PaintCoordinateSpace, PaintPhase,
    RadialGradient, ReferenceTextShaper, StackingContext, TextDecoration, TextDecorationLine,
    TextShaper, Transform2D, build_display_list, build_display_list_with_images,
};
pub use raster::{
    CpuRasterOutput, CpuRasterizer, GlyphMask, GlyphMaskProvider, NoGlyphMasks, RasterDiagnostic,
    RasterDiagnosticCode, Surface,
};
