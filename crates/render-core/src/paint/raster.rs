//! Deterministic CPU reference rasterizer.

use std::error::Error;
use std::fmt;
use std::sync::Arc;

use crate::css::properties::BorderStyle;
use crate::image::ImageResources;
use crate::layout::{PhysicalPoint, PhysicalRect};

use super::color::{Color, clamped_rounded_u8};
use super::display_list::{
    BorderPaint, ClipShape, DisplayCommand, DisplayItem, DisplayItemId, DisplayList,
    FontInstanceId, GlyphId, GlyphRun, PaintCoordinateSpace,
};
use super::scene::{PaintDamage, PaintScene, RetainedFrame};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Surface {
    width: u32,
    height: u32,
    pixels: Vec<Color>,
}

impl Surface {
    /// Allocates a surface initialized to `color`.
    ///
    /// # Panics
    ///
    /// Panics when the dimensions exceed the process's addressable memory.
    #[must_use]
    pub fn new(width: u32, height: u32, color: Color) -> Self {
        let len = usize::try_from(width)
            .ok()
            .and_then(|width| {
                usize::try_from(height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .expect("surface dimensions exceed addressable memory");
        Self {
            width,
            height,
            pixels: vec![color; len],
        }
    }

    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    #[must_use]
    pub fn pixels(&self) -> &[Color] {
        &self.pixels
    }

    #[must_use]
    pub fn pixel(&self, x: u32, y: u32) -> Option<Color> {
        self.index(x, y)
            .and_then(|index| self.pixels.get(index))
            .copied()
    }

    fn index(&self, x: u32, y: u32) -> Option<usize> {
        if x >= self.width || y >= self.height {
            return None;
        }
        usize::try_from(y)
            .ok()?
            .checked_mul(usize::try_from(self.width).ok()?)?
            .checked_add(usize::try_from(x).ok()?)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlyphMask {
    pub width: u32,
    pub height: u32,
    pub left: i32,
    pub top: i32,
    pub coverage: Vec<u8>,
}

pub trait GlyphMaskProvider: Sync {
    fn mask(&self, font: FontInstanceId, glyph: GlyphId, font_size: f32) -> Option<GlyphMask>;

    /// Returns a shareable glyph mask for raster hot paths.
    ///
    /// Providers that do not maintain their own shared cache retain the
    /// original [`Self::mask`] behavior. Backends with a glyph cache can
    /// override this method to avoid cloning the coverage buffer for every
    /// painted glyph.
    fn shared_mask(
        &self,
        font: FontInstanceId,
        glyph: GlyphId,
        font_size: f32,
    ) -> Option<Arc<GlyphMask>> {
        self.mask(font, glyph, font_size).map(Arc::new)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoGlyphMasks;

impl GlyphMaskProvider for NoGlyphMasks {
    fn mask(&self, _font: FontInstanceId, _glyph: GlyphId, _font_size: f32) -> Option<GlyphMask> {
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RasterDiagnosticCode {
    UnsupportedCommand,
    UnsupportedBorderStyle,
    MissingGlyph,
    UnbalancedState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RasterDiagnostic {
    pub item: DisplayItemId,
    pub code: RasterDiagnosticCode,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CpuRasterOutput {
    pub surface: Surface,
    pub diagnostics: Vec<RasterDiagnostic>,
}

/// Cooperative cancellation hook for raster work.
///
/// Render schedulers may implement this with a page epoch or work-queue
/// cancellation token. The rasterizer never assumes a particular threading
/// model, which keeps this boundary usable by future renderers as well.
pub trait RasterControl: Sync {
    #[must_use]
    fn is_cancelled(&self) -> bool;
}

/// The default control used by the legacy, infallible raster APIs.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoRasterCancellation;

impl RasterControl for NoRasterCancellation {
    fn is_cancelled(&self) -> bool {
        false
    }
}

/// Returned when a caller cancels a cooperative raster request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RasterCancelled;

impl fmt::Display for RasterCancelled {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("raster request was cancelled")
    }
}

impl Error for RasterCancelled {}

/// Input to the scene-based raster path.
///
/// It uses references rather than owning page state, so callers can submit
/// immutable snapshots without cloning images, glyph providers, or retained
/// surfaces.
#[derive(Clone, Copy)]
pub struct RasterRequest<'a> {
    scene: &'a PaintScene,
    background: Color,
    glyphs: &'a dyn GlyphMaskProvider,
    viewport_origin: PhysicalPoint,
    images: Option<&'a ImageResources>,
    retained: Option<&'a RetainedFrame>,
    damage: Option<&'a PaintDamage>,
}

impl<'a> RasterRequest<'a> {
    #[must_use]
    pub fn new(
        scene: &'a PaintScene,
        background: Color,
        glyphs: &'a dyn GlyphMaskProvider,
    ) -> Self {
        Self {
            scene,
            background,
            glyphs,
            viewport_origin: PhysicalPoint::default(),
            images: None,
            retained: None,
            damage: None,
        }
    }

    #[must_use]
    pub fn with_viewport_origin(mut self, viewport_origin: PhysicalPoint) -> Self {
        self.viewport_origin = viewport_origin;
        self
    }

    #[must_use]
    pub fn with_images(mut self, images: &'a ImageResources) -> Self {
        self.images = Some(images);
        self
    }

    /// Requests a partial update against `retained` when the scene proves it
    /// is safe. Invalid or conservative damage automatically uses a complete
    /// raster, so callers never need a separate correctness fallback.
    #[must_use]
    pub fn with_retained_damage(
        mut self,
        retained: &'a RetainedFrame,
        damage: &'a PaintDamage,
    ) -> Self {
        self.retained = Some(retained);
        self.damage = Some(damage);
        self
    }

    #[must_use]
    pub fn scene(&self) -> &PaintScene {
        self.scene
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct CpuRasterizer;

impl CpuRasterizer {
    #[must_use]
    pub fn rasterize(
        &self,
        list: &DisplayList,
        background: Color,
        glyphs: &dyn GlyphMaskProvider,
    ) -> CpuRasterOutput {
        self.rasterize_viewport(list, background, glyphs, PhysicalPoint::default())
    }

    /// Rasterize a viewport-sized window whose origin is expressed in
    /// document coordinates. Document-space items are translated by the
    /// inverse origin and clipped to the unchanged surface; viewport-space
    /// items are left in place.
    #[must_use]
    pub fn rasterize_viewport(
        &self,
        list: &DisplayList,
        background: Color,
        glyphs: &dyn GlyphMaskProvider,
        viewport_origin: PhysicalPoint,
    ) -> CpuRasterOutput {
        self.rasterize_viewport_with_images(list, background, glyphs, viewport_origin, None)
    }

    #[must_use]
    pub fn rasterize_viewport_with_images(
        &self,
        list: &DisplayList,
        background: Color,
        glyphs: &dyn GlyphMaskProvider,
        viewport_origin: PhysicalPoint,
        images: Option<&ImageResources>,
    ) -> CpuRasterOutput {
        Self::rasterize_list(list, background, glyphs, viewport_origin, images)
    }

    /// Rasterizes an immutable paint scene with cooperative cancellation.
    ///
    /// The method chooses the retained damage path only when both scene and
    /// request metadata prove it can be replayed independently. All other
    /// cases use the legacy complete raster path and remain pixel-identical to
    /// [`Self::rasterize_viewport_with_images`].
    ///
    /// # Errors
    ///
    /// Returns [`RasterCancelled`] when `control` requests cancellation before
    /// the completed frame can be returned.
    pub fn rasterize_request(
        &self,
        request: RasterRequest<'_>,
        control: &dyn RasterControl,
    ) -> Result<CpuRasterOutput, RasterCancelled> {
        if control.is_cancelled() {
            return Err(RasterCancelled);
        }

        if let (Some(retained), Some(damage)) = (request.retained, request.damage) {
            if Self::can_apply_damage(&request, retained, damage) {
                return Self::rasterize_damage(&request, retained, damage, control);
            }
        }

        Self::rasterize_list_cancellable(
            request.scene.display_list(),
            request.background,
            request.glyphs,
            request.viewport_origin,
            request.images,
            control,
        )
    }

    fn can_apply_damage(
        request: &RasterRequest<'_>,
        retained: &RetainedFrame,
        damage: &PaintDamage,
    ) -> bool {
        request.scene.is_tile_safe()
            && !damage.is_full_repaint()
            && retained.scene().is_tile_safe()
            && damage.matches_transition(retained.scene(), request.scene)
            && retained.is_compatible_with(
                request.scene,
                request.viewport_origin,
                request.background,
            )
    }

    fn rasterize_damage(
        request: &RasterRequest<'_>,
        retained: &RetainedFrame,
        damage: &PaintDamage,
        control: &dyn RasterControl,
    ) -> Result<CpuRasterOutput, RasterCancelled> {
        let Some(damage_bounds) = damage.viewport_bounds(
            request.scene.viewport(),
            request.viewport_origin,
            request
                .scene
                .tile_coordinate_space()
                .expect("tile-safe scene"),
        ) else {
            return Ok(CpuRasterOutput {
                surface: retained.surface().clone(),
                diagnostics: Vec::new(),
            });
        };

        let mut surface = retained.surface().clone();
        if !clear_rect(
            &mut surface,
            damage_bounds,
            request.background,
            Some(control),
        ) {
            return Err(RasterCancelled);
        }
        let mut state = RasterState::from_surface(
            surface,
            request.viewport_origin,
            damage_bounds,
            Some(control),
        );
        for item in request.scene.display_list().items() {
            if control.is_cancelled() || state.was_cancelled() {
                return Err(RasterCancelled);
            }
            state.process_item(item, request.glyphs, request.images);
        }
        if control.is_cancelled() || state.was_cancelled() {
            return Err(RasterCancelled);
        }
        state.finish_cancellable()
    }

    fn rasterize_list(
        list: &DisplayList,
        background: Color,
        glyphs: &dyn GlyphMaskProvider,
        viewport_origin: PhysicalPoint,
        images: Option<&ImageResources>,
    ) -> CpuRasterOutput {
        let width = ceil_to_u32(list.viewport.width);
        let height = ceil_to_u32(list.viewport.height);
        let mut state = RasterState::new(width, height, background, viewport_origin);
        for item in list.items() {
            state.process_item(item, glyphs, images);
        }
        state.finish()
    }

    fn rasterize_list_cancellable(
        list: &DisplayList,
        background: Color,
        glyphs: &dyn GlyphMaskProvider,
        viewport_origin: PhysicalPoint,
        images: Option<&ImageResources>,
        control: &dyn RasterControl,
    ) -> Result<CpuRasterOutput, RasterCancelled> {
        let width = ceil_to_u32(list.viewport.width);
        let height = ceil_to_u32(list.viewport.height);
        let mut state = RasterState::new_with_control(
            width,
            height,
            background,
            viewport_origin,
            Some(control),
        );
        for item in list.items() {
            if control.is_cancelled() || state.was_cancelled() {
                return Err(RasterCancelled);
            }
            state.process_item(item, glyphs, images);
        }
        if control.is_cancelled() || state.was_cancelled() {
            return Err(RasterCancelled);
        }
        state.finish_cancellable()
    }
}

struct Layer {
    surface: Surface,
    opacity: f32,
}

struct RasterState<'a> {
    width: u32,
    height: u32,
    layers: Vec<Layer>,
    clips: Vec<PhysicalRect>,
    diagnostics: Vec<RasterDiagnostic>,
    viewport_origin: PhysicalPoint,
    control: Option<&'a dyn RasterControl>,
    cancelled: bool,
}

impl<'a> RasterState<'a> {
    fn new(width: u32, height: u32, background: Color, viewport_origin: PhysicalPoint) -> Self {
        Self::new_with_control(width, height, background, viewport_origin, None)
    }

    fn new_with_control(
        width: u32,
        height: u32,
        background: Color,
        viewport_origin: PhysicalPoint,
        control: Option<&'a dyn RasterControl>,
    ) -> Self {
        Self {
            width,
            height,
            layers: vec![Layer {
                surface: Surface::new(width, height, background),
                opacity: 1.0,
            }],
            clips: vec![PhysicalRect::new(
                0.0,
                0.0,
                u32_to_f32(width),
                u32_to_f32(height),
            )],
            diagnostics: Vec::new(),
            viewport_origin: PhysicalPoint {
                x: finite_non_negative(viewport_origin.x),
                y: finite_non_negative(viewport_origin.y),
            },
            control,
            cancelled: false,
        }
    }

    fn from_surface(
        surface: Surface,
        viewport_origin: PhysicalPoint,
        clip: PhysicalRect,
        control: Option<&'a dyn RasterControl>,
    ) -> Self {
        let width = surface.width();
        let height = surface.height();
        Self {
            width,
            height,
            layers: vec![Layer {
                surface,
                opacity: 1.0,
            }],
            clips: vec![clip],
            diagnostics: Vec::new(),
            viewport_origin: PhysicalPoint {
                x: finite_non_negative(viewport_origin.x),
                y: finite_non_negative(viewport_origin.y),
            },
            control,
            cancelled: false,
        }
    }

    fn process_item(
        &mut self,
        item: &DisplayItem,
        glyphs: &dyn GlyphMaskProvider,
        images: Option<&ImageResources>,
    ) {
        if self.is_cancelled() {
            self.cancelled = true;
            return;
        }
        let offset = self.item_offset(item.coordinate_space);
        match &item.command {
            DisplayCommand::SolidRect { rect, color } => {
                let clip = self.current_clip();
                let control = self.control;
                if !fill_rect(
                    self.current_surface(),
                    translate_rect(*rect, offset),
                    *color,
                    clip,
                    control,
                ) {
                    self.cancelled = true;
                }
            }
            DisplayCommand::Border(border) => self.paint_border(item.id, border, offset),
            DisplayCommand::PushClip(ClipShape::Rect(rect)) => {
                self.push_clip(translate_rect(*rect, offset));
            }
            DisplayCommand::PushClip(ClipShape::RoundedRect { rect, .. }) => {
                self.push_clip(translate_rect(*rect, offset));
                self.diagnostics.push(RasterDiagnostic {
                    item: item.id,
                    code: RasterDiagnosticCode::UnsupportedCommand,
                    message: "rounded clip rasterization is not implemented".to_owned(),
                });
            }
            DisplayCommand::PopClip => self.pop_clip(item.id),
            DisplayCommand::PushStackingContext(context) => self.layers.push(Layer {
                surface: Surface::new(self.width, self.height, Color::TRANSPARENT),
                opacity: context.opacity,
            }),
            DisplayCommand::PopStackingContext => self.pop_layer(item.id),
            DisplayCommand::GlyphRun(run) => {
                self.paint_glyph_run(item.id, run, glyphs, offset);
            }
            DisplayCommand::TextDecoration(decoration) => {
                let clip = self.current_clip();
                let control = self.control;
                if !fill_rect(
                    self.current_surface(),
                    translate_rect(decoration.rect, offset),
                    decoration.color,
                    clip,
                    control,
                ) {
                    self.cancelled = true;
                }
            }
            DisplayCommand::Image(image) => {
                if let Some(decoded) = images.and_then(|images| images.get(image.resource)) {
                    let clip = self.current_clip();
                    let control = self.control;
                    if !paint_image(
                        self.current_surface(),
                        decoded,
                        translate_rect(image.destination, offset),
                        image.source,
                        clip,
                        control,
                    ) {
                        self.cancelled = true;
                    }
                } else {
                    self.diagnostics.push(RasterDiagnostic {
                        item: item.id,
                        code: RasterDiagnosticCode::UnsupportedCommand,
                        message: "image resource is not available to the rasterizer".to_owned(),
                    });
                }
            }
            DisplayCommand::BoxShadow(_)
            | DisplayCommand::PushTransform(_)
            | DisplayCommand::PopTransform
            | DisplayCommand::LinearGradient(_)
            | DisplayCommand::RadialGradient(_)
            | DisplayCommand::Canvas { .. } => self.diagnostics.push(RasterDiagnostic {
                item: item.id,
                code: RasterDiagnosticCode::UnsupportedCommand,
                message: "display command is represented but not rasterized yet".to_owned(),
            }),
        }
    }

    fn item_offset(&self, coordinate_space: PaintCoordinateSpace) -> PhysicalPoint {
        match coordinate_space {
            PaintCoordinateSpace::Document => PhysicalPoint {
                x: -self.viewport_origin.x,
                y: -self.viewport_origin.y,
            },
            PaintCoordinateSpace::Viewport => PhysicalPoint::default(),
        }
    }

    fn paint_border(&mut self, item: DisplayItemId, border: &BorderPaint, offset: PhysicalPoint) {
        for style in border.styles {
            if !matches!(
                style,
                BorderStyle::None | BorderStyle::Hidden | BorderStyle::Solid
            ) {
                self.diagnostics.push(RasterDiagnostic {
                    item,
                    code: RasterDiagnosticCode::UnsupportedBorderStyle,
                    message: format!("{style:?} border rasterization is not implemented"),
                });
            }
        }
        let rect = translate_rect(border.rect, offset);
        let edges = [
            PhysicalRect::new(
                rect.origin.x,
                rect.origin.y,
                rect.size.width,
                border.widths.top,
            ),
            PhysicalRect::new(
                rect.right() - border.widths.right,
                rect.origin.y,
                border.widths.right,
                rect.size.height,
            ),
            PhysicalRect::new(
                rect.origin.x,
                rect.bottom() - border.widths.bottom,
                rect.size.width,
                border.widths.bottom,
            ),
            PhysicalRect::new(
                rect.origin.x,
                rect.origin.y,
                border.widths.left,
                rect.size.height,
            ),
        ];
        let clip = self.current_clip();
        let control = self.control;
        for (edge, color) in edges.into_iter().zip(border.colors) {
            if !fill_rect(self.current_surface(), edge, color, clip, control) {
                self.cancelled = true;
                return;
            }
        }
    }

    fn paint_glyph_run(
        &mut self,
        item: DisplayItemId,
        run: &GlyphRun,
        glyphs: &dyn GlyphMaskProvider,
        offset: PhysicalPoint,
    ) {
        for glyph in &run.glyphs {
            let Some(mask) = glyphs.shared_mask(run.font, glyph.glyph, run.font_size) else {
                self.diagnostics.push(RasterDiagnostic {
                    item,
                    code: RasterDiagnosticCode::MissingGlyph,
                    message: format!("no mask for glyph {}", glyph.glyph.0),
                });
                continue;
            };
            let clip = self.current_clip();
            let control = self.control;
            if !paint_glyph(
                self.current_surface(),
                mask.as_ref(),
                translate_point(glyph.position, offset),
                run.color,
                clip,
                control,
            ) {
                self.cancelled = true;
                return;
            }
        }
    }

    fn push_clip(&mut self, rect: PhysicalRect) {
        self.clips
            .push(intersection(self.current_clip(), rect).unwrap_or_default());
    }

    fn pop_clip(&mut self, item: DisplayItemId) {
        if self.clips.len() > 1 {
            self.clips.pop();
        } else {
            self.diagnostics.push(RasterDiagnostic {
                item,
                code: RasterDiagnosticCode::UnbalancedState,
                message: "unbalanced display-list clip pop".to_owned(),
            });
        }
    }

    fn pop_layer(&mut self, item: DisplayItemId) {
        if self.layers.len() > 1 {
            self.composite_top_layer();
        } else {
            self.diagnostics.push(RasterDiagnostic {
                item,
                code: RasterDiagnosticCode::UnbalancedState,
                message: "unbalanced stacking-context pop".to_owned(),
            });
        }
    }

    fn composite_top_layer(&mut self) {
        let layer = self.layers.pop().expect("non-root layer");
        if !composite_surface(
            &mut self.layers.last_mut().expect("parent layer").surface,
            &layer.surface,
            layer.opacity,
            self.control,
        ) {
            self.cancelled = true;
        }
    }

    fn current_surface(&mut self) -> &mut Surface {
        &mut self.layers.last_mut().expect("root layer").surface
    }

    fn current_clip(&self) -> Option<PhysicalRect> {
        self.clips.last().copied()
    }

    fn is_cancelled(&self) -> bool {
        self.control.is_some_and(RasterControl::is_cancelled)
    }

    fn was_cancelled(&self) -> bool {
        self.cancelled
    }

    fn finish(mut self) -> CpuRasterOutput {
        while self.layers.len() > 1 {
            self.composite_top_layer();
        }
        self.output()
    }

    fn finish_cancellable(mut self) -> Result<CpuRasterOutput, RasterCancelled> {
        while self.layers.len() > 1 {
            if self.is_cancelled() {
                return Err(RasterCancelled);
            }
            self.composite_top_layer();
            if self.was_cancelled() {
                return Err(RasterCancelled);
            }
        }
        if self.is_cancelled() {
            return Err(RasterCancelled);
        }
        Ok(self.output())
    }

    fn output(mut self) -> CpuRasterOutput {
        CpuRasterOutput {
            surface: self.layers.pop().expect("root layer").surface,
            diagnostics: self.diagnostics,
        }
    }
}

fn translate_rect(rect: PhysicalRect, offset: PhysicalPoint) -> PhysicalRect {
    PhysicalRect::new(
        rect.origin.x + offset.x,
        rect.origin.y + offset.y,
        rect.size.width,
        rect.size.height,
    )
}

fn translate_point(point: PhysicalPoint, offset: PhysicalPoint) -> PhysicalPoint {
    PhysicalPoint {
        x: point.x + offset.x,
        y: point.y + offset.y,
    }
}

fn finite_non_negative(value: f32) -> f32 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

fn fill_rect(
    surface: &mut Surface,
    rect: PhysicalRect,
    color: Color,
    clip: Option<PhysicalRect>,
    control: Option<&dyn RasterControl>,
) -> bool {
    if color.alpha == 0 || rect.size.width <= 0.0 || rect.size.height <= 0.0 {
        return true;
    }
    let Some(rect) = intersection(Some(rect), clip.unwrap_or(rect)) else {
        return true;
    };
    let left = floor_to_u32(rect.origin.x);
    let top = floor_to_u32(rect.origin.y);
    let right = ceil_to_u32(rect.right().min(u32_to_f32(surface.width)));
    let bottom = ceil_to_u32(rect.bottom().min(u32_to_f32(surface.height)));
    for y in top..bottom {
        if control.is_some_and(RasterControl::is_cancelled) {
            return false;
        }
        for x in left..right {
            if let Some(index) = surface.index(x, y) {
                surface.pixels[index] = blend(surface.pixels[index], color, 1.0);
            }
        }
    }
    true
}

fn clear_rect(
    surface: &mut Surface,
    rect: PhysicalRect,
    color: Color,
    control: Option<&dyn RasterControl>,
) -> bool {
    if rect.size.width <= 0.0 || rect.size.height <= 0.0 {
        return true;
    }
    let left = floor_to_u32(rect.origin.x);
    let top = floor_to_u32(rect.origin.y);
    let right = ceil_to_u32(rect.right().min(u32_to_f32(surface.width)));
    let bottom = ceil_to_u32(rect.bottom().min(u32_to_f32(surface.height)));
    for y in top..bottom {
        if control.is_some_and(RasterControl::is_cancelled) {
            return false;
        }
        for x in left..right {
            if let Some(index) = surface.index(x, y) {
                surface.pixels[index] = color;
            }
        }
    }
    true
}

fn paint_glyph(
    surface: &mut Surface,
    mask: &GlyphMask,
    position: PhysicalPoint,
    color: Color,
    clip: Option<PhysicalRect>,
    control: Option<&dyn RasterControl>,
) -> bool {
    for y in 0..mask.height {
        if control.is_some_and(RasterControl::is_cancelled) {
            return false;
        }
        for x in 0..mask.width {
            let index = usize::try_from(y)
                .ok()
                .and_then(|y| {
                    usize::try_from(mask.width)
                        .ok()
                        .and_then(|width| y.checked_mul(width))
                })
                .and_then(|row| usize::try_from(x).ok().and_then(|x| row.checked_add(x)));
            let Some(coverage) = index.and_then(|index| mask.coverage.get(index)).copied() else {
                continue;
            };
            let Some((target_x, target_y)) = glyph_pixel_position(position, mask, x, y) else {
                continue;
            };
            let point = PhysicalRect::new(u32_to_f32(target_x), u32_to_f32(target_y), 1.0, 1.0);
            if clip.is_some_and(|clip| intersection(Some(point), clip).is_none()) {
                continue;
            }
            if let Some(surface_index) = surface.index(target_x, target_y) {
                surface.pixels[surface_index] = blend(
                    surface.pixels[surface_index],
                    color.with_opacity(f32::from(coverage) / 255.0),
                    1.0,
                );
            }
        }
    }
    true
}

fn paint_image(
    surface: &mut Surface,
    image: &crate::image::DecodedImage,
    destination: PhysicalRect,
    source: PhysicalRect,
    clip: Option<PhysicalRect>,
    control: Option<&dyn RasterControl>,
) -> bool {
    if destination.size.width <= 0.0
        || destination.size.height <= 0.0
        || source.size.width <= 0.0
        || source.size.height <= 0.0
    {
        return true;
    }
    let Some(visible) = intersection(Some(destination), clip.unwrap_or(destination)) else {
        return true;
    };
    let left = floor_to_u32(visible.origin.x);
    let top = floor_to_u32(visible.origin.y);
    let right = ceil_to_u32(visible.right().min(u32_to_f32(surface.width)));
    let bottom = ceil_to_u32(visible.bottom().min(u32_to_f32(surface.height)));
    for y in top..bottom {
        if control.is_some_and(RasterControl::is_cancelled) {
            return false;
        }
        for x in left..right {
            let unit_x = (u32_to_f32(x) + 0.5 - destination.origin.x) / destination.size.width;
            let unit_y = (u32_to_f32(y) + 0.5 - destination.origin.y) / destination.size.height;
            let source_x = floor_to_u32(source.origin.x + unit_x * source.size.width);
            let source_y = floor_to_u32(source.origin.y + unit_y * source.size.height);
            if let (Some(color), Some(index)) =
                (image.pixel(source_x, source_y), surface.index(x, y))
            {
                surface.pixels[index] = blend(surface.pixels[index], color, 1.0);
            }
        }
    }
    true
}

fn composite_surface(
    destination: &mut Surface,
    source: &Surface,
    opacity: f32,
    control: Option<&dyn RasterControl>,
) -> bool {
    let row_width = usize::try_from(destination.width).expect("surface width fits usize");
    for (index, (destination, source)) in destination
        .pixels
        .iter_mut()
        .zip(&source.pixels)
        .enumerate()
    {
        if index % row_width == 0 && control.is_some_and(RasterControl::is_cancelled) {
            return false;
        }
        *destination = blend(*destination, *source, opacity);
    }
    true
}

fn blend(destination: Color, source: Color, opacity: f32) -> Color {
    let source_alpha = f32::from(source.alpha) / 255.0 * opacity.clamp(0.0, 1.0);
    let destination_alpha = f32::from(destination.alpha) / 255.0;
    let output_alpha = source_alpha + destination_alpha * (1.0 - source_alpha);
    if output_alpha <= f32::EPSILON {
        return Color::TRANSPARENT;
    }
    let channel = |source: u8, destination: u8| {
        clamped_rounded_u8(
            (f32::from(source) * source_alpha
                + f32::from(destination) * destination_alpha * (1.0 - source_alpha))
                / output_alpha,
        )
    };
    Color::rgba(
        channel(source.red, destination.red),
        channel(source.green, destination.green),
        channel(source.blue, destination.blue),
        clamped_rounded_u8(output_alpha * 255.0),
    )
}

fn glyph_pixel_position(
    position: PhysicalPoint,
    mask: &GlyphMask,
    x: u32,
    y: u32,
) -> Option<(u32, u32)> {
    let x = i32::try_from(x).ok()?;
    let y = i32::try_from(y).ok()?;
    let target_x = rounded_i32(position.x)
        .checked_add(mask.left)?
        .checked_add(x)?;
    let target_y = rounded_i32(position.y)
        .checked_sub(mask.top)?
        .checked_add(y)?;
    Some((u32::try_from(target_x).ok()?, u32::try_from(target_y).ok()?))
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is rounded and the float-to-integer cast saturates to u32"
)]
fn ceil_to_u32(value: f32) -> u32 {
    value.ceil().max(0.0) as u32
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is rounded and the float-to-integer cast saturates to u32"
)]
fn floor_to_u32(value: f32) -> u32 {
    value.floor().max(0.0) as u32
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "glyph device coordinates intentionally use saturating float-to-i32 conversion"
)]
fn rounded_i32(value: f32) -> i32 {
    value.round() as i32
}

#[allow(
    clippy::cast_precision_loss,
    reason = "raster geometry uses f32 and surfaces cannot exceed u32 dimensions"
)]
fn u32_to_f32(value: u32) -> f32 {
    value as f32
}

fn intersection(first: Option<PhysicalRect>, second: PhysicalRect) -> Option<PhysicalRect> {
    let first = first?;
    let left = first.origin.x.max(second.origin.x);
    let top = first.origin.y.max(second.origin.y);
    let right = first.right().min(second.right());
    let bottom = first.bottom().min(second.bottom());
    (right > left && bottom > top).then(|| PhysicalRect::new(left, top, right - left, bottom - top))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::dom::Dom;
    use crate::layout::{FragmentId, PhysicalRect, PhysicalSize};
    use crate::paint::display_list::{
        DisplayCommand, DisplayItem, DisplayItemId, DisplayList, FontInstanceId, GlyphId,
        GlyphInstance, GlyphRun, PaintCoordinateSpace, PaintPhase,
    };
    use crate::paint::{
        GlyphMask, GlyphMaskProvider, PaintScene, RasterControl, RasterRequest, RetainedFrame,
    };

    use super::{Color, CpuRasterizer, NoGlyphMasks, NoRasterCancellation, RasterCancelled};

    #[derive(Clone, Copy, Debug)]
    struct Cancelled;

    impl RasterControl for Cancelled {
        fn is_cancelled(&self) -> bool {
            true
        }
    }

    #[derive(Default)]
    struct SharedMasks {
        legacy_calls: AtomicUsize,
        shared_calls: AtomicUsize,
    }

    impl GlyphMaskProvider for SharedMasks {
        fn mask(
            &self,
            _font: FontInstanceId,
            _glyph: GlyphId,
            _font_size: f32,
        ) -> Option<GlyphMask> {
            self.legacy_calls.fetch_add(1, Ordering::Relaxed);
            None
        }

        fn shared_mask(
            &self,
            _font: FontInstanceId,
            _glyph: GlyphId,
            _font_size: f32,
        ) -> Option<Arc<GlyphMask>> {
            self.shared_calls.fetch_add(1, Ordering::Relaxed);
            Some(Arc::new(GlyphMask {
                width: 1,
                height: 1,
                left: 0,
                top: 1,
                coverage: vec![255],
            }))
        }
    }

    fn solid_list(color: Color) -> DisplayList {
        let dom = Dom::new();
        let rect = PhysicalRect::new(1.0, 1.0, 2.0, 2.0);
        DisplayList {
            dom_revision: dom.revision(),
            viewport: PhysicalSize {
                width: 4.0,
                height: 4.0,
            },
            items: vec![DisplayItem {
                id: DisplayItemId {
                    source: None,
                    fragment_hint: 0,
                    phase: PaintPhase::Background,
                    ordinal: 0,
                },
                fragment: FragmentId::from_index(0),
                source: None,
                bounds: rect,
                coordinate_space: PaintCoordinateSpace::Document,
                command: DisplayCommand::SolidRect { rect, color },
            }],
        }
    }

    #[test]
    fn rasterizer_uses_shared_glyph_masks() {
        let dom = Dom::new();
        let list = DisplayList {
            dom_revision: dom.revision(),
            viewport: PhysicalSize {
                width: 4.0,
                height: 4.0,
            },
            items: vec![DisplayItem {
                id: DisplayItemId {
                    source: None,
                    fragment_hint: 0,
                    phase: PaintPhase::Content,
                    ordinal: 0,
                },
                fragment: FragmentId::from_index(0),
                source: None,
                bounds: PhysicalRect::new(1.0, 1.0, 1.0, 1.0),
                coordinate_space: PaintCoordinateSpace::Document,
                command: DisplayCommand::GlyphRun(GlyphRun {
                    font: FontInstanceId(0),
                    font_size: 1.0,
                    color: Color::BLACK,
                    glyphs: vec![GlyphInstance {
                        glyph: GlyphId(1),
                        position: crate::layout::PhysicalPoint { x: 1.0, y: 2.0 },
                        advance: 1.0,
                    }],
                }),
            }],
        };
        let provider = SharedMasks::default();

        let output = CpuRasterizer.rasterize(&list, Color::WHITE, &provider);

        assert!(output.diagnostics.is_empty());
        assert_eq!(provider.shared_calls.load(Ordering::Relaxed), 1);
        assert_eq!(provider.legacy_calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn cpu_reference_rasterizer_blends_solid_rectangles() {
        let dom = Dom::new();
        let item = DisplayItem {
            id: DisplayItemId {
                source: None,
                fragment_hint: 0,
                phase: PaintPhase::Background,
                ordinal: 0,
            },
            fragment: FragmentId::from_index(0),
            source: None,
            bounds: PhysicalRect::new(1.0, 1.0, 2.0, 2.0),
            coordinate_space: PaintCoordinateSpace::Document,
            command: DisplayCommand::SolidRect {
                rect: PhysicalRect::new(1.0, 1.0, 2.0, 2.0),
                color: Color::rgba(255, 0, 0, 128),
            },
        };
        let list = DisplayList {
            dom_revision: dom.revision(),
            viewport: PhysicalSize {
                width: 4.0,
                height: 4.0,
            },
            items: vec![item],
        };
        let output = CpuRasterizer.rasterize(&list, Color::WHITE, &NoGlyphMasks);
        assert_eq!(output.surface.pixel(0, 0), Some(Color::WHITE));
        assert_eq!(output.surface.pixel(1, 1), Some(Color::rgb(255, 127, 127)));
        assert!(output.diagnostics.is_empty());
    }

    #[test]
    fn viewport_origin_scrolls_document_items_but_not_viewport_items() {
        let dom = Dom::new();
        let make_item = |ordinal, y, size, color, coordinate_space| DisplayItem {
            id: DisplayItemId {
                source: None,
                fragment_hint: ordinal,
                phase: PaintPhase::Background,
                ordinal,
            },
            fragment: FragmentId::from_index(0),
            source: None,
            bounds: PhysicalRect::new(0.0, y, size, size),
            coordinate_space,
            command: DisplayCommand::SolidRect {
                rect: PhysicalRect::new(0.0, y, size, size),
                color,
            },
        };
        let list = DisplayList {
            dom_revision: dom.revision(),
            viewport: PhysicalSize {
                width: 2.0,
                height: 2.0,
            },
            items: vec![
                make_item(
                    0,
                    2.0,
                    2.0,
                    Color::rgb(0, 0, 255),
                    PaintCoordinateSpace::Document,
                ),
                make_item(
                    1,
                    0.0,
                    1.0,
                    Color::rgb(255, 0, 0),
                    PaintCoordinateSpace::Viewport,
                ),
            ],
        };

        let output = CpuRasterizer.rasterize_viewport(
            &list,
            Color::WHITE,
            &NoGlyphMasks,
            crate::layout::PhysicalPoint { x: 0.0, y: 2.0 },
        );

        // The document item moved into view, then the viewport-attached item
        // painted over it without moving.
        assert_eq!(output.surface.pixel(0, 0), Some(Color::rgb(255, 0, 0)));
        assert_eq!(output.surface.pixel(1, 1), Some(Color::rgb(0, 0, 255)));
    }

    #[test]
    fn retained_damage_matches_a_complete_raster_for_direct_paint() {
        let previous = PaintScene::from_display_list(solid_list(Color::BLACK));
        let current = PaintScene::from_display_list(solid_list(Color::rgb(0, 0, 255)));
        let rasterizer = CpuRasterizer;
        let previous_output =
            rasterizer.rasterize(previous.display_list(), Color::WHITE, &NoGlyphMasks);
        let retained = RetainedFrame::new(
            previous.clone(),
            crate::layout::PhysicalPoint::default(),
            Color::WHITE,
            previous_output.surface,
        );
        let damage = current.damage_from(&previous);
        let partial = rasterizer
            .rasterize_request(
                RasterRequest::new(&current, Color::WHITE, &NoGlyphMasks)
                    .with_retained_damage(&retained, &damage),
                &NoRasterCancellation,
            )
            .expect("safe direct-paint damage should rasterize");
        let complete = rasterizer.rasterize(current.display_list(), Color::WHITE, &NoGlyphMasks);

        assert_eq!(partial, complete);
    }

    #[test]
    fn cancelled_request_does_not_create_a_frame() {
        let scene = PaintScene::from_display_list(solid_list(Color::BLACK));
        let result = CpuRasterizer.rasterize_request(
            RasterRequest::new(&scene, Color::WHITE, &NoGlyphMasks),
            &Cancelled,
        );

        assert_eq!(result, Err(RasterCancelled));
    }
}
