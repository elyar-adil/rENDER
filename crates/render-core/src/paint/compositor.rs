//! Retained layer tree and banded compositor for scrolling.
//!
//! Layout owns geometry and display-list construction; this module owns the
//! retained frame that survives between scroll positions. A scene is split
//! into three kinds of retained surface:
//!
//! - **root segments**: contiguous runs of ordinary display items between
//!   top-level scopes, split by coordinate space so fixed content keeps
//!   viewport coordinates and document content follows the scroll;
//! - **layers**: a top-level stacking context (transform/opacity) or sticky
//!   scope, rasterized once in its own space and composited through its own
//!   matrix and alpha;
//! - **bands**: every surface is cut into fixed-height bands, so a long
//!   document does not need one giant allocation and a scroll only rasterizes
//!   the bands that were not already cached.
//!
//! Fixed content is rasterized into a viewport-sized source and composited
//! without the scroll translation. A sticky scope is rasterized untransformed
//! and displaced at composite time, so its bands survive a scroll. Scale,
//! rotate and skew layers are supported by inverse-mapping the viewport into
//! source-local coordinates when choosing bands, then compositing the
//! untransformed source through the affine matrix.
//!
//! A top-level stacking context with a `filter` or `mask` is retained as one
//! effect surface instead of independent bands: filters move pixels (a blur
//! reaches across band boundaries and a drop shadow shifts them), so the
//! effect must see the layer's whole input and run exactly once before the
//! surface is composited. The surface is the layer's content bounds expanded
//! by the filter's reach, which costs more memory than a band but keeps the
//! result pixel-identical to the complete raster's offscreen pass.
//!
//! The compositor is deliberately conservative. A non-normal blend mode, an
//! ambient clip crossing a layer boundary, nested sticky, a layer wider than
//! the 8192px raster ceiling, or an effect surface above the retained-effect
//! limits returns `None` and the caller keeps the complete-raster path.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use crate::image::ImageResources;
use crate::layout::{PhysicalPoint, PhysicalRect, PhysicalSize, StickyConstraint};

use super::color::Color;
use super::display_list::{
    BlendMode, ClipShape, DisplayCommand, DisplayItem, DisplayList, FilterEffect, MaskLayer,
    PaintCoordinateSpace, Transform2D,
};
use super::raster::{
    CpuRasterOutput, CpuRasterizer, FilterOutset, GlyphMaskProvider, RasterControl, Surface,
    apply_layer_effects, composite_surface, filter_outset,
};
use super::sticky::sticky_displacement;

/// The physical height of one retained compositor band.
pub const DEFAULT_COMPOSITOR_BAND_HEIGHT: u32 = 512;

/// The widest effect surface the retained compositor will allocate.
///
/// This matches the rasterizer's general layer ceiling: a layer this wide is
/// already too expensive to retain as one offscreen surface.
const MAX_EFFECT_SURFACE_WIDTH: f32 = 8192.0;

/// The tallest effect surface the retained compositor will allocate.
///
/// A filtered or masked layer cannot be cut into bands without either
/// duplicating the effect work or getting the pixels wrong, so a very tall
/// layer would need one large surface. Falling back to the complete raster at
/// this point keeps peak memory bounded.
const MAX_EFFECT_SURFACE_HEIGHT: f32 = 4096.0;

/// The total pixel budget for one cached effect surface.
///
/// A 4M-pixel layer is about 16 MiB of `Color` pixels; the blur scratch
/// buffers are the larger cost because they store four `f32` channels.
const MAX_EFFECT_SURFACE_PIXELS: f32 = 4_194_304.0;

/// Cache key component reserved for the single effect surface of a layer.
///
/// Band indices are physical band numbers and can never reach `u32::MAX`.
const EFFECT_SURFACE_BAND: u32 = u32::MAX;

/// Transparent padding that keeps bilinear layer-edge sampling identical to
/// the complete raster's viewport-sized offscreen surface.
const EFFECT_SURFACE_MARGIN: f32 = 1.0;

/// A transformed, transparent, filtered, masked or sticky top-level scope.
#[derive(Clone, Debug, PartialEq)]
struct CompositedLayer {
    /// The display-list range, excluding the push/pop commands themselves.
    start: usize,
    end: usize,
    /// Untransformed bounds. Document layers use the item union; viewport
    /// layers use the viewport rectangle so a viewport-space item keeps its
    /// viewport coordinates.
    bounds: PhysicalRect,
    transform: Transform2D,
    opacity: f32,
    space: PaintCoordinateSpace,
    /// A sticky scope's displacement is resolved from the current scroll
    /// offset at composite time; the source bands stay fixed.
    sticky: Option<StickyConstraint>,
    /// A top-level `filter`, applied once to the whole effect surface before
    /// it is composited.
    filter: Option<FilterEffect>,
    /// A top-level `mask-image`, sampled in document or viewport coordinates
    /// and applied after the filter.
    mask: Option<MaskLayer>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LayerOp {
    Segment(usize),
    Layer(usize),
}

#[derive(Clone, Debug, PartialEq)]
struct RootSegment {
    start: usize,
    end: usize,
    bounds: PhysicalRect,
    space: PaintCoordinateSpace,
    /// Clips that were already open when this segment starts.
    ///
    /// A segment normally begins before its own clip commands, so this is
    /// empty. It is non-empty when a coordinate-space change split the display
    /// list inside a clip: the document side replays the clip while
    /// rasterizing its bands, and the viewport side applies it at composite
    /// time because its source surface is scroll-independent.
    clips: Vec<InheritedClip>,
}

/// One clip open at a root-segment boundary.
#[derive(Clone, Copy, Debug, PartialEq)]
struct InheritedClip {
    shape: ClipShape,
    space: PaintCoordinateSpace,
}

/// Per-frame compositor counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CompositorStats {
    /// Completed frames.
    pub frames: u64,
    /// Bands rasterized since the cache was built.
    pub bands_rasterized: u64,
    /// Bands rasterized by the most recent frame.
    pub last_frame_bands: u32,
    /// Bands currently retained.
    pub cached_bands: u32,
}

#[derive(Debug, Default)]
struct CompositorState {
    bands: HashMap<(u8, usize, u32), Surface>,
    stats: CompositorStats,
}

/// A retained composited scene.
///
/// `render` only touches bands that intersect the requested scroll viewport;
/// all other bands stay in the cache. The same value is shared by the browser's
/// page state and the render worker, so a scroll job does not need to rebuild
/// layout or the display list.
#[derive(Debug)]
pub struct CompositorCache {
    display_list: Arc<DisplayList>,
    segments: Vec<RootSegment>,
    layers: Vec<CompositedLayer>,
    ops: Vec<LayerOp>,
    background: Color,
    viewport: PhysicalSize,
    band_height: u32,
    state: Mutex<CompositorState>,
}

impl CompositorCache {
    /// Builds a compositor for `display_list`.
    ///
    /// `None` means the scene uses a command the retained path cannot replay
    /// independently, and the caller must use the conservative raster path.
    #[must_use]
    #[allow(
        clippy::too_many_lines,
        reason = "the extraction walk is one state machine over the display list; splitting it would duplicate the root-segment bookkeeping"
    )]
    pub fn from_display_list(display_list: Arc<DisplayList>, background: Color) -> Option<Self> {
        let items = display_list.items();
        let viewport = display_list.viewport;
        let mut segments = Vec::new();
        let mut layers = Vec::new();
        let mut ops = Vec::new();
        let mut index = 0_usize;
        let mut root_start = 0_usize;
        let mut root_space: Option<PaintCoordinateSpace> = None;
        let mut root_clips: Vec<InheritedClip> = Vec::new();
        let mut active_clips: Vec<InheritedClip> = Vec::new();
        let mut clip_depth = 0_usize;
        while index < items.len() {
            let item = &items[index];
            let space = item.coordinate_space;
            match &item.command {
                DisplayCommand::PushClip(_) | DisplayCommand::PopClip => {
                    if root_space.is_none() {
                        root_space = Some(space);
                    }
                    if root_space != Some(space) {
                        split_root_segment(
                            items,
                            viewport,
                            &mut root_start,
                            &mut root_space,
                            &root_clips,
                            index,
                            space,
                            &mut segments,
                            &mut ops,
                        )?;
                        root_clips.clone_from(&active_clips);
                    }
                    match item.command {
                        DisplayCommand::PushClip(shape) => {
                            active_clips.push(InheritedClip { shape, space });
                            clip_depth += 1;
                        }
                        DisplayCommand::PopClip => {
                            active_clips.pop();
                            clip_depth = clip_depth.saturating_sub(1);
                        }
                        _ => unreachable!("matched clip commands"),
                    }
                    index += 1;
                }
                DisplayCommand::PushSticky(constraint) => {
                    // A sticky scope is a retained layer with a displacement
                    // resolved from the current scroll offset. Nested sticky or
                    // a stacking context inside it would need the outer
                    // displacement when rebasing the inner transform pivot, so
                    // keep those on the complete-raster path for now.
                    if clip_depth != 0 || space != PaintCoordinateSpace::Document {
                        return None;
                    }
                    let end = matching_sticky_pop(items, index)?;
                    if items[index + 1..end].iter().any(|item| {
                        item.coordinate_space != PaintCoordinateSpace::Document
                            || matches!(
                                &item.command,
                                DisplayCommand::PushSticky(_)
                                    | DisplayCommand::PopSticky
                                    | DisplayCommand::PushStackingContext(_)
                                    | DisplayCommand::PopStackingContext
                                    | DisplayCommand::PushTransform(_)
                                    | DisplayCommand::PopTransform
                            )
                    }) {
                        return None;
                    }
                    push_segment(
                        items,
                        viewport,
                        root_start,
                        index,
                        root_space,
                        &root_clips,
                        &mut segments,
                        &mut ops,
                    )?;
                    let bounds = item_bounds(&items[index + 1..end])?;
                    if bounds.size.width > MAX_EFFECT_SURFACE_WIDTH {
                        return None;
                    }
                    let layer = layers.len();
                    layers.push(CompositedLayer {
                        start: index + 1,
                        end,
                        bounds,
                        transform: Transform2D::default(),
                        opacity: 1.0,
                        space: PaintCoordinateSpace::Document,
                        sticky: Some(**constraint),
                        filter: None,
                        mask: None,
                    });
                    ops.push(LayerOp::Layer(layer));
                    index = end + 1;
                    root_start = index;
                    root_space = None;
                }
                DisplayCommand::PushStackingContext(context) => {
                    // A context inside an ambient clip would need that clip
                    // copied into its layer and removed from the root segment.
                    // The conservative answer is the complete raster path.
                    let start = index + 1;
                    let end = matching_pop(items, index)?;
                    if start == end {
                        // An empty scope paints no pixels. Leave the balanced
                        // push/pop in the surrounding segment so the band
                        // rasterizer replays it; extracting a layer would
                        // require an empty bounds rectangle.
                        if root_space.is_some_and(|current| current != space) {
                            return None;
                        }
                        root_space = Some(space);
                        index = end + 1;
                        continue;
                    }
                    if clip_depth != 0 || context.blend_mode != BlendMode::Normal {
                        return None;
                    }
                    if items[start..end]
                        .iter()
                        .any(|item| item.coordinate_space != space)
                    {
                        return None;
                    }
                    if items[start..end].iter().any(|item| {
                        matches!(
                            &item.command,
                            DisplayCommand::PushSticky(_)
                                | DisplayCommand::PopSticky
                                | DisplayCommand::PushTransform(_)
                                | DisplayCommand::PopTransform
                        )
                    }) {
                        return None;
                    }
                    let bounds = if space == PaintCoordinateSpace::Viewport {
                        viewport_rect(viewport)
                    } else {
                        item_bounds(&items[start..end])?
                    };
                    let has_effect = context.filter.is_some() || context.mask.is_some();
                    if has_effect {
                        // The effect surface must be one allocation: a banded
                        // filter would either clip a blur at a band edge or
                        // repeat the effect over overlapping inputs. Reject a
                        // layer that would exceed the retention budget so the
                        // caller keeps the complete-raster path.
                        effect_surface_bounds(bounds, space, context.filter.as_ref(), viewport)?;
                    } else if bounds.size.width > MAX_EFFECT_SURFACE_WIDTH {
                        return None;
                    }
                    push_segment(
                        items,
                        viewport,
                        root_start,
                        index,
                        root_space,
                        &root_clips,
                        &mut segments,
                        &mut ops,
                    )?;
                    let layer = layers.len();
                    layers.push(CompositedLayer {
                        start,
                        end,
                        bounds,
                        transform: context.transform,
                        opacity: context.opacity.clamp(0.0, 1.0),
                        space,
                        sticky: None,
                        filter: context.filter.clone(),
                        mask: context.mask.clone(),
                    });
                    ops.push(LayerOp::Layer(layer));
                    index = end + 1;
                    root_start = index;
                    root_space = None;
                }
                DisplayCommand::PushTransform(_) | DisplayCommand::PopTransform => return None,
                _ => {
                    if let Some(current) = root_space
                        && current != space
                    {
                        split_root_segment(
                            items,
                            viewport,
                            &mut root_start,
                            &mut root_space,
                            &root_clips,
                            index,
                            space,
                            &mut segments,
                            &mut ops,
                        )?;
                        root_clips = if clip_depth == 0 {
                            Vec::new()
                        } else {
                            active_clips.clone()
                        };
                    }
                    root_space = Some(space);
                    index += 1;
                }
            }
        }
        push_segment(
            items,
            viewport,
            root_start,
            items.len(),
            root_space,
            &root_clips,
            &mut segments,
            &mut ops,
        )?;
        Some(Self {
            display_list,
            segments,
            layers,
            ops,
            background,
            viewport,
            band_height: DEFAULT_COMPOSITOR_BAND_HEIGHT.max(1),
            state: Mutex::new(CompositorState::default()),
        })
    }

    /// The scene's viewport, which is the size of the surface `render` returns.
    #[must_use]
    pub const fn viewport(&self) -> PhysicalSize {
        self.viewport
    }

    /// The number of retained layers, including root segments.
    #[must_use]
    pub fn layer_count(&self) -> usize {
        self.segments.len() + self.layers.len()
    }

    /// The current counters. Cheap enough for a frame log and tests.
    #[must_use]
    pub fn stats(&self) -> CompositorStats {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .stats
    }

    /// Composites the scene at `viewport_origin`, reusing every cached band.
    ///
    /// `viewport_origin` is the document-space scroll offset, so a scroll by
    /// one pixel changes only the blit matrix; it does not invalidate a band.
    #[must_use]
    pub fn render(
        &self,
        viewport_origin: PhysicalPoint,
        glyphs: &dyn GlyphMaskProvider,
        images: Option<&ImageResources>,
        control: Option<&dyn RasterControl>,
    ) -> CpuRasterOutput {
        let width = ceil_to_u32(self.viewport.width);
        let height = ceil_to_u32(self.viewport.height);
        let mut output = Surface::new(width, height, self.background);
        let mut diagnostics = Vec::new();
        let mut last_frame_bands = 0_u32;
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        for op in &self.ops {
            match *op {
                LayerOp::Segment(index) => {
                    let segment = &self.segments[index];
                    last_frame_bands += self.composite_surface(
                        &mut output,
                        &mut state,
                        segment.start,
                        segment.end,
                        segment.bounds,
                        Transform2D::default(),
                        1.0,
                        segment.space,
                        None,
                        &segment.clips,
                        viewport_origin,
                        glyphs,
                        images,
                        control,
                        0,
                        index,
                        &mut diagnostics,
                    );
                }
                LayerOp::Layer(index) => {
                    let layer = &self.layers[index];
                    if layer.filter.is_some() || layer.mask.is_some() {
                        last_frame_bands += self.composite_effect_layer(
                            &mut output,
                            &mut state,
                            index,
                            layer,
                            viewport_origin,
                            glyphs,
                            images,
                            control,
                            &mut diagnostics,
                        );
                    } else {
                        last_frame_bands += self.composite_surface(
                            &mut output,
                            &mut state,
                            layer.start,
                            layer.end,
                            layer.bounds,
                            layer.transform,
                            layer.opacity,
                            layer.space,
                            layer.sticky,
                            &[],
                            viewport_origin,
                            glyphs,
                            images,
                            control,
                            1,
                            index,
                            &mut diagnostics,
                        );
                    }
                }
            }
        }
        state.stats.frames = state.stats.frames.saturating_add(1);
        state.stats.last_frame_bands = last_frame_bands;
        state.stats.cached_bands = u32::try_from(state.bands.len()).unwrap_or(u32::MAX);
        CpuRasterOutput {
            surface: output,
            diagnostics,
        }
    }

    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "one band compositing path carries the layer parameters and the shared cache; band indices are bounded physical pixels"
    )]
    fn composite_surface(
        &self,
        output: &mut Surface,
        state: &mut CompositorState,
        start: usize,
        end: usize,
        bounds: PhysicalRect,
        transform: Transform2D,
        opacity: f32,
        space: PaintCoordinateSpace,
        sticky: Option<StickyConstraint>,
        clips: &[InheritedClip],
        viewport_origin: PhysicalPoint,
        glyphs: &dyn GlyphMaskProvider,
        images: Option<&ImageResources>,
        control: Option<&dyn RasterControl>,
        kind: u8,
        index: usize,
        diagnostics: &mut Vec<super::raster::RasterDiagnostic>,
    ) -> u32 {
        if bounds.size.width <= 0.0 || bounds.size.height <= 0.0 {
            return 0;
        }
        // The complete raster paints a top-level context into a viewport-sized
        // offscreen surface whose paint offset is `-viewport_origin` in both
        // coordinate spaces. Its pop warps that surface through
        // `T(offset) * transform * T(-offset)`, so a fixed transformed layer is
        // conjugated by the scroll offset even though its items never scroll.
        // The retained source is anchored differently, but the same conjugation
        // reproduces the reference pixels.
        let scroll = viewport_origin;
        let sticky_offset = sticky.map_or_else(PhysicalPoint::default, |constraint| {
            sticky_displacement(&constraint, PhysicalPoint::default(), viewport_origin)
        });
        let transform = Transform2D {
            translate_x: sticky_offset.x,
            translate_y: sticky_offset.y,
            ..Transform2D::default()
        }
        .then(&transform);
        let viewport_rect = viewport_rect(self.viewport);
        // A viewport segment split out of a document clip keeps its source
        // surface scroll-independent, so its inherited clips are applied here
        // in viewport coordinates. Document segments replay clips while
        // rasterizing their bands instead.
        let composite_clip = if space == PaintCoordinateSpace::Viewport {
            let Some(clip) = inherited_viewport_clip(clips, viewport_origin, viewport_rect) else {
                return 0;
            };
            clip
        } else {
            viewport_rect
        };
        let band_height = if space == PaintCoordinateSpace::Viewport {
            ceil_to_u32(self.viewport.height).max(1)
        } else {
            self.band_height
        };
        let source_height = if space == PaintCoordinateSpace::Viewport {
            self.viewport.height
        } else {
            bounds.size.height
        };
        let source_width = if space == PaintCoordinateSpace::Viewport {
            self.viewport.width
        } else {
            bounds.size.width
        };
        if source_width <= 0.0 || source_height <= 0.0 {
            return 0;
        }
        let to_viewport = Transform2D {
            translate_x: -scroll.x,
            translate_y: -scroll.y,
            ..Transform2D::default()
        };
        // Source pixels are document coordinates for document layers and
        // viewport coordinates for fixed layers. The reference surface is
        // anchored at `-scroll` in both cases, so a viewport source contributes
        // `T(scroll)` before the transform and `T(-scroll)` after it; the two
        // cancel for the identity matrix and leave fixed content in place.
        let to_band = Transform2D {
            translate_x: if space == PaintCoordinateSpace::Viewport {
                scroll.x
            } else {
                bounds.origin.x
            },
            translate_y: if space == PaintCoordinateSpace::Viewport {
                scroll.y
            } else {
                bounds.origin.y
            },
            ..Transform2D::default()
        };
        let base_mapped = to_viewport.then(&transform.then(&to_band));
        let Some(inverse) = base_mapped.inverse() else {
            return 0;
        };
        let mut min_x = f32::INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        for (x, y) in [
            (0.0, 0.0),
            (viewport_rect.size.width, 0.0),
            (0.0, viewport_rect.size.height),
            (viewport_rect.size.width, viewport_rect.size.height),
        ] {
            let (source_x, source_y) = inverse.apply(x, y);
            min_x = min_x.min(source_x);
            min_y = min_y.min(source_y);
            max_x = max_x.max(source_x);
            max_y = max_y.max(source_y);
        }
        let source_rect = PhysicalRect::new(0.0, 0.0, source_width, source_height);
        let Some(visible) = intersect_rect(
            PhysicalRect::new(min_x, min_y, max_x - min_x, max_y - min_y),
            source_rect,
        ) else {
            return 0;
        };
        let first = (visible.origin.y / band_height as f32).floor().max(0.0) as u32;
        let last = (visible.bottom() / band_height as f32).ceil().max(0.0) as u32;
        let band_count = (source_height / band_height as f32).ceil().max(0.0) as u32;
        let last = last.min(band_count);
        let mut rasterized = 0_u32;
        for band in first..last {
            if control.is_some_and(RasterControl::is_cancelled) {
                break;
            }
            let local_y = band as f32 * band_height as f32;
            let band_height = (source_height - local_y).min(band_height as f32).max(0.0);
            let band_size = PhysicalSize {
                width: source_width,
                height: band_height,
            };
            if band_size.width <= 0.0 || band_size.height <= 0.0 {
                continue;
            }
            let key = (kind, index, band);
            if let std::collections::hash_map::Entry::Vacant(entry) = state.bands.entry(key) {
                let raster_origin = if space == PaintCoordinateSpace::Viewport {
                    PhysicalPoint { x: 0.0, y: 0.0 }
                } else {
                    PhysicalPoint {
                        x: bounds.origin.x,
                        y: bounds.origin.y + local_y,
                    }
                };
                let raster = if space == PaintCoordinateSpace::Document && !clips.is_empty() {
                    let inherited = clips
                        .iter()
                        .map(|clip| (clip.shape, clip.space))
                        .collect::<Vec<_>>();
                    CpuRasterizer.rasterize_item_slice_with_clips(
                        &self.display_list.items()[start..end],
                        band_size,
                        Color::TRANSPARENT,
                        glyphs,
                        raster_origin,
                        images,
                        &inherited,
                    )
                } else {
                    CpuRasterizer.rasterize_item_slice(
                        &self.display_list.items()[start..end],
                        band_size,
                        Color::TRANSPARENT,
                        glyphs,
                        raster_origin,
                        images,
                    )
                };
                diagnostics.extend(raster.diagnostics);
                entry.insert(raster.surface);
                state.stats.bands_rasterized = state.stats.bands_rasterized.saturating_add(1);
                rasterized = rasterized.saturating_add(1);
            }
            let Some(source) = state.bands.get(&key) else {
                continue;
            };
            let band_origin = Transform2D {
                translate_x: if space == PaintCoordinateSpace::Viewport {
                    scroll.x
                } else {
                    bounds.origin.x
                },
                translate_y: if space == PaintCoordinateSpace::Viewport {
                    scroll.y
                } else {
                    bounds.origin.y + local_y
                },
                ..Transform2D::default()
            };
            let mapped = to_viewport.then(&transform.then(&band_origin));
            let clip = Some(composite_clip);
            if !composite_surface(output, source, mapped, opacity, clip, control) {
                break;
            }
        }
        rasterized
    }

    /// Rasterizes and composites a top-level layer that carries a filter or a
    /// mask.
    ///
    /// The layer is one surface, not a set of bands: a filter can move pixels
    /// across any band boundary, and applying it to each band in isolation
    /// would clip the blur or repeat the border. The surface is cached exactly
    /// like a band, so scrolling still reuses the finished effect.
    #[allow(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        clippy::cast_precision_loss,
        reason = "the effect path carries the same layer context as the band path; cache dimensions are bounded physical pixels"
    )]
    fn composite_effect_layer(
        &self,
        output: &mut Surface,
        state: &mut CompositorState,
        layer_index: usize,
        layer: &CompositedLayer,
        viewport_origin: PhysicalPoint,
        glyphs: &dyn GlyphMaskProvider,
        images: Option<&ImageResources>,
        control: Option<&dyn RasterControl>,
        diagnostics: &mut Vec<super::raster::RasterDiagnostic>,
    ) -> u32 {
        let Some(surface_bounds) = effect_surface_bounds(
            layer.bounds,
            layer.space,
            layer.filter.as_ref(),
            self.viewport,
        ) else {
            return 0;
        };
        if surface_bounds.size.width <= 0.0 || surface_bounds.size.height <= 0.0 {
            return 0;
        }
        let sticky_offset = layer
            .sticky
            .map_or_else(PhysicalPoint::default, |constraint| {
                sticky_displacement(&constraint, PhysicalPoint::default(), viewport_origin)
            });
        let transform = Transform2D {
            translate_x: sticky_offset.x,
            translate_y: sticky_offset.y,
            ..Transform2D::default()
        }
        .then(&layer.transform);
        let viewport_rect = viewport_rect(self.viewport);
        // Document source pixels are anchored at the surface origin; viewport
        // source pixels use the reference offscreen surface's imaginary
        // `-scroll` anchor, which the two scroll translations cancel around
        // the identity transform.
        let source_anchor = match layer.space {
            PaintCoordinateSpace::Document => surface_bounds.origin,
            PaintCoordinateSpace::Viewport => viewport_origin,
        };
        let to_source = Transform2D {
            translate_x: source_anchor.x,
            translate_y: source_anchor.y,
            ..Transform2D::default()
        };
        let to_viewport = Transform2D {
            translate_x: -viewport_origin.x,
            translate_y: -viewport_origin.y,
            ..Transform2D::default()
        };
        let base_mapped = to_viewport.then(&transform.then(&to_source));
        let Some(inverse) = base_mapped.inverse() else {
            return 0;
        };
        let mut min_x = f32::INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        for (x, y) in [
            (0.0, 0.0),
            (viewport_rect.size.width, 0.0),
            (0.0, viewport_rect.size.height),
            (viewport_rect.size.width, viewport_rect.size.height),
        ] {
            let (source_x, source_y) = inverse.apply(x, y);
            min_x = min_x.min(source_x);
            min_y = min_y.min(source_y);
            max_x = max_x.max(source_x);
            max_y = max_y.max(source_y);
        }
        let source_rect = PhysicalRect::new(
            0.0,
            0.0,
            surface_bounds.size.width,
            surface_bounds.size.height,
        );
        if intersect_rect(
            PhysicalRect::new(min_x, min_y, max_x - min_x, max_y - min_y),
            source_rect,
        )
        .is_none()
        {
            return 0;
        }
        let key = (2_u8, layer_index, EFFECT_SURFACE_BAND);
        let mut rasterized = 0_u32;
        if let std::collections::hash_map::Entry::Vacant(entry) = state.bands.entry(key) {
            let raster = CpuRasterizer.rasterize_item_slice(
                &self.display_list.items()[layer.start..layer.end],
                PhysicalSize {
                    width: surface_bounds.size.width,
                    height: surface_bounds.size.height,
                },
                Color::TRANSPARENT,
                glyphs,
                surface_bounds.origin,
                images,
            );
            diagnostics.extend(raster.diagnostics);
            let mut surface = raster.surface;
            let mask_offset = match layer.space {
                // `apply_mask_image` recovers the coordinate of pixel `(0, 0)`
                // as `-offset`, so the retained surface feeds its own origin.
                PaintCoordinateSpace::Document => PhysicalPoint {
                    x: -surface_bounds.origin.x,
                    y: -surface_bounds.origin.y,
                },
                PaintCoordinateSpace::Viewport => PhysicalPoint::default(),
            };
            apply_layer_effects(
                &mut surface,
                layer.filter.as_ref(),
                layer.mask.as_ref(),
                mask_offset,
                images,
            );
            entry.insert(surface);
            state.stats.bands_rasterized = state.stats.bands_rasterized.saturating_add(1);
            rasterized = 1;
        }
        let Some(source) = state.bands.get(&key) else {
            return rasterized;
        };
        let mapped = to_viewport.then(&transform.then(&to_source));
        if !composite_surface(
            output,
            source,
            mapped,
            layer.opacity,
            Some(viewport_rect),
            control,
        ) {
            return rasterized;
        }
        rasterized
    }
}

/// The single surface a filtered or masked layer is rasterized into.
///
/// Document layers use their content union grown by the filter's reach and
/// aligned to the physical pixel grid. The left/top edge is also clamped to
/// the document origin: the complete raster paints document layers into a
/// viewport-sized surface, so content left of or above the origin is clipped
/// before the effect runs.
///
/// Viewport layers keep the viewport rectangle. Their complete-raster
/// offscreen surface is exactly the viewport, so expanding it would both waste
/// memory and change a filter's edge samples.
///
/// `None` means the surface would exceed the retention budget and the caller
/// must use the complete-raster path.
#[allow(
    clippy::cast_precision_loss,
    reason = "filter reaches and layer sizes are small bounded pixel counts"
)]
fn effect_surface_bounds(
    bounds: PhysicalRect,
    space: PaintCoordinateSpace,
    filter: Option<&FilterEffect>,
    viewport: PhysicalSize,
) -> Option<PhysicalRect> {
    if space == PaintCoordinateSpace::Viewport {
        let viewport_rect = viewport_rect(viewport);
        if viewport_rect.size.width <= 0.0 || viewport_rect.size.height <= 0.0 {
            return None;
        }
        if !within_effect_budget(viewport_rect.size.width, viewport_rect.size.height) {
            return None;
        }
        return Some(viewport_rect);
    }
    let outset = filter.map_or_else(FilterOutset::default, filter_outset);
    // One transparent pixel of margin reproduces the reference surface's
    // sampling at a transformed layer edge: bilinear warping may sample just
    // outside the content bounds, and the complete raster's viewport-sized
    // offscreen surface supplies a transparent neighbor there instead of
    // rejecting the whole sample.
    let left = (bounds.origin.x - outset.left as f32 - EFFECT_SURFACE_MARGIN)
        .floor()
        .max(0.0);
    let top = (bounds.origin.y - outset.top as f32 - EFFECT_SURFACE_MARGIN)
        .floor()
        .max(0.0);
    let right = (bounds.right() + outset.right as f32 + EFFECT_SURFACE_MARGIN).ceil();
    let bottom = (bounds.bottom() + outset.bottom as f32 + EFFECT_SURFACE_MARGIN).ceil();
    if !left.is_finite()
        || !top.is_finite()
        || !right.is_finite()
        || !bottom.is_finite()
        || right <= left
        || bottom <= top
    {
        return None;
    }
    let width = right - left;
    let height = bottom - top;
    if !within_effect_budget(width, height) {
        return None;
    }
    Some(PhysicalRect::new(left, top, width, height))
}

fn within_effect_budget(width: f32, height: f32) -> bool {
    width > 0.0
        && height > 0.0
        && width <= MAX_EFFECT_SURFACE_WIDTH
        && height <= MAX_EFFECT_SURFACE_HEIGHT
        && width * height <= MAX_EFFECT_SURFACE_PIXELS
}

fn matching_pop(items: &[DisplayItem], push: usize) -> Option<usize> {
    let mut depth = 0_usize;
    for (index, item) in items.iter().enumerate().skip(push) {
        match &item.command {
            DisplayCommand::PushStackingContext(_) => depth += 1,
            DisplayCommand::PopStackingContext => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

fn matching_sticky_pop(items: &[DisplayItem], push: usize) -> Option<usize> {
    let mut depth = 0_usize;
    for (index, item) in items.iter().enumerate().skip(push) {
        match &item.command {
            DisplayCommand::PushSticky(_) => depth += 1,
            DisplayCommand::PopSticky => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

/// Starts a new root segment at `index` after closing the segment that ends
/// there.
#[allow(clippy::too_many_arguments)]
fn split_root_segment(
    items: &[DisplayItem],
    viewport: PhysicalSize,
    root_start: &mut usize,
    root_space: &mut Option<PaintCoordinateSpace>,
    root_clips: &[InheritedClip],
    index: usize,
    next_space: PaintCoordinateSpace,
    segments: &mut Vec<RootSegment>,
    ops: &mut Vec<LayerOp>,
) -> Option<()> {
    push_segment(
        items,
        viewport,
        *root_start,
        index,
        *root_space,
        root_clips,
        segments,
        ops,
    )?;
    *root_start = index;
    *root_space = Some(next_space);
    Some(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "the segment boundary is one parser state: source range, space, inherited clips and the two output lists"
)]
fn push_segment(
    items: &[DisplayItem],
    viewport: PhysicalSize,
    start: usize,
    end: usize,
    space: Option<PaintCoordinateSpace>,
    clips: &[InheritedClip],
    segments: &mut Vec<RootSegment>,
    ops: &mut Vec<LayerOp>,
) -> Option<()> {
    if start >= end {
        return Some(());
    }
    let segment = make_segment(items, viewport, start, end, space?, clips)?;
    ops.push(LayerOp::Segment(segments.len()));
    segments.push(segment);
    Some(())
}

fn make_segment(
    items: &[DisplayItem],
    viewport: PhysicalSize,
    start: usize,
    end: usize,
    space: PaintCoordinateSpace,
    clips: &[InheritedClip],
) -> Option<RootSegment> {
    if items[start..end]
        .iter()
        .any(|item| item.coordinate_space != space)
    {
        return None;
    }
    // A viewport segment's surface is cached independently of the scroll
    // offset, so its inherited clips are applied when it is composited. That
    // only supports axis-aligned rectangles exactly; anything else keeps the
    // complete-raster fallback. A document segment replays its clips while
    // rasterizing each band, and that replay is only valid for clips that live
    // in document coordinates.
    match space {
        PaintCoordinateSpace::Viewport => {
            if clips
                .iter()
                .any(|clip| !matches!(clip.shape, ClipShape::Rect(_)))
            {
                return None;
            }
        }
        PaintCoordinateSpace::Document => {
            if clips
                .iter()
                .any(|clip| clip.space != PaintCoordinateSpace::Document)
            {
                return None;
            }
        }
    }
    let bounds = if space == PaintCoordinateSpace::Viewport {
        viewport_rect(viewport)
    } else {
        item_bounds(&items[start..end])?
    };
    if bounds.size.width > 8192.0 {
        return None;
    }
    Some(RootSegment {
        start,
        end,
        bounds,
        space,
        clips: clips.to_vec(),
    })
}

fn viewport_rect(viewport: PhysicalSize) -> PhysicalRect {
    PhysicalRect::new(0.0, 0.0, viewport.width.max(0.0), viewport.height.max(0.0))
}

fn intersect_rect(first: PhysicalRect, second: PhysicalRect) -> Option<PhysicalRect> {
    let left = first.origin.x.max(second.origin.x);
    let top = first.origin.y.max(second.origin.y);
    let right = first.right().min(second.right());
    let bottom = first.bottom().min(second.bottom());
    (right > left && bottom > top).then(|| PhysicalRect::new(left, top, right - left, bottom - top))
}

/// Resolves inherited clips into one viewport-space rectangle.
///
/// The complete rasterizer's all-rect fast path includes integer pixel `x`
/// when `x >= left && x < right` in the clip's own coordinates. Rounding the
/// edges to `ceil(left)` / `ceil(right)` before handing them to the blit's
/// pixel-centre test preserves exactly that set of pixels.
fn inherited_viewport_clip(
    clips: &[InheritedClip],
    viewport_origin: PhysicalPoint,
    viewport: PhysicalRect,
) -> Option<PhysicalRect> {
    let mut clip = viewport;
    for inherited in clips {
        let ClipShape::Rect(rect) = inherited.shape else {
            return None;
        };
        let rect = match inherited.space {
            PaintCoordinateSpace::Document => PhysicalRect::new(
                rect.origin.x - viewport_origin.x,
                rect.origin.y - viewport_origin.y,
                rect.size.width,
                rect.size.height,
            ),
            PaintCoordinateSpace::Viewport => rect,
        };
        let left = rect.origin.x.ceil();
        let top = rect.origin.y.ceil();
        let right = rect.right().ceil();
        let bottom = rect.bottom().ceil();
        if !left.is_finite() || !top.is_finite() || !right.is_finite() || !bottom.is_finite() {
            return None;
        }
        clip = intersect_rect(
            clip,
            PhysicalRect::new(left, top, right - left, bottom - top),
        )?;
    }
    Some(clip)
}

fn item_bounds(items: &[DisplayItem]) -> Option<PhysicalRect> {
    items
        .iter()
        .filter(|item| {
            item.bounds.size.width > 0.0
                && item.bounds.size.height > 0.0
                && item.bounds.origin.x.is_finite()
                && item.bounds.origin.y.is_finite()
                && item.bounds.size.width.is_finite()
                && item.bounds.size.height.is_finite()
        })
        .fold(None, |current, item| {
            let next = item.bounds;
            Some(match current {
                Some(current) => {
                    let left = current.origin.x.min(next.origin.x);
                    let top = current.origin.y.min(next.origin.y);
                    let right = current.right().max(next.right());
                    let bottom = current.bottom().max(next.bottom());
                    PhysicalRect::new(left, top, right - left, bottom - top)
                }
                None => next,
            })
        })
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the compositor viewport and band sizes are clamped physical pixel counts"
)]
fn ceil_to_u32(value: f32) -> u32 {
    value.ceil().max(0.0) as u32
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::float_cmp,
        clippy::cast_precision_loss,
        reason = "fixture geometry and band counters are exact small integers"
    )]

    use std::sync::Arc;

    use super::CompositorCache;
    use crate::dom::DomRevision;
    use crate::layout::{PhysicalPoint, PhysicalRect, PhysicalSize};
    use crate::paint::display_list::{
        ClipShape, FilterEffect, FilterOperation, GradientStop, LinearGradient, MaskImage,
        MaskLayer,
    };
    use crate::paint::{
        BlendMode, Color, CompositingReason, CpuRasterizer, DisplayCommand, DisplayItem,
        DisplayItemId, DisplayList, NoGlyphMasks, PaintCoordinateSpace, PaintPhase,
        StackingContext, Transform2D,
    };

    fn item(index: u32, bounds: PhysicalRect, command: DisplayCommand) -> DisplayItem {
        DisplayItem {
            id: DisplayItemId {
                source: None,
                fragment_hint: index,
                phase: PaintPhase::Background,
                ordinal: index,
            },
            fragment: crate::layout::FragmentId::from_index(index as usize),
            source: None,
            bounds,
            coordinate_space: PaintCoordinateSpace::Document,
            command,
        }
    }

    fn rect(x: f32, y: f32, width: f32, height: f32, color: Color) -> DisplayCommand {
        DisplayCommand::SolidRect {
            rect: PhysicalRect::new(x, y, width, height),
            color,
        }
    }

    fn context(transform: Transform2D, opacity: f32) -> StackingContext {
        context_with_effects(transform, opacity, None, None)
    }

    fn context_with_effects(
        transform: Transform2D,
        opacity: f32,
        filter: Option<FilterEffect>,
        mask: Option<MaskLayer>,
    ) -> StackingContext {
        StackingContext {
            opacity,
            transform,
            blend_mode: BlendMode::Normal,
            isolated: true,
            reason: CompositingReason::Transform,
            filter,
            mask,
        }
    }

    fn list(viewport: PhysicalSize, items: Vec<DisplayItem>) -> DisplayList {
        DisplayList::from_items(DomRevision::default(), viewport, items)
    }

    fn linear_alpha_mask(rect: PhysicalRect) -> MaskLayer {
        MaskLayer {
            image: MaskImage::Linear(LinearGradient {
                rect,
                start: PhysicalPoint {
                    x: rect.origin.x,
                    y: rect.origin.y,
                },
                end: PhysicalPoint {
                    x: rect.origin.x,
                    y: rect.bottom(),
                },
                stops: vec![
                    GradientStop {
                        offset: 0.0,
                        color: Color::rgba(0, 0, 0, 0),
                    },
                    GradientStop {
                        offset: 1.0,
                        color: Color::rgba(0, 0, 0, 255),
                    },
                ],
                repeating: false,
            }),
            tile: rect,
            clip: rect,
            repeat_x: true,
            repeat_y: true,
        }
    }

    fn blur_filter(sigma: f32) -> FilterEffect {
        FilterEffect {
            operations: vec![FilterOperation::Blur(sigma)],
        }
    }

    fn assert_matches_complete_raster(
        cache: &CompositorCache,
        display: &std::sync::Arc<DisplayList>,
        background: Color,
        offset: PhysicalPoint,
        label: &str,
    ) {
        let composited = cache.render(offset, &NoGlyphMasks, None, None);
        let complete =
            CpuRasterizer.rasterize_viewport(display.as_ref(), background, &NoGlyphMasks, offset);
        assert_eq!(
            composited.surface.width(),
            complete.surface.width(),
            "{label} width differs at scroll ({}, {})",
            offset.x,
            offset.y
        );
        assert_eq!(
            composited.surface.height(),
            complete.surface.height(),
            "{label} height differs at scroll ({}, {})",
            offset.x,
            offset.y
        );
        for y in 0..complete.surface.height() {
            for x in 0..complete.surface.width() {
                let actual = composited.surface.pixel(x, y);
                let expected = complete.surface.pixel(x, y);
                assert_eq!(
                    actual, expected,
                    "{label} differs at pixel ({x}, {y}) for scroll ({}, {})",
                    offset.x, offset.y
                );
            }
        }
        assert!(composited.diagnostics.is_empty());
    }

    #[test]
    fn a_translated_opacity_layer_is_composited_at_its_transform() {
        let items = vec![
            item(
                0,
                PhysicalRect::new(0.0, 0.0, 40.0, 40.0),
                rect(0.0, 0.0, 40.0, 40.0, Color::WHITE),
            ),
            item(
                1,
                PhysicalRect::new(0.0, 0.0, 40.0, 40.0),
                DisplayCommand::PushStackingContext(context(
                    Transform2D {
                        translate_x: 10.0,
                        ..Transform2D::default()
                    },
                    0.5,
                )),
            ),
            item(
                2,
                PhysicalRect::new(0.0, 0.0, 10.0, 10.0),
                rect(0.0, 0.0, 10.0, 10.0, Color::BLACK),
            ),
            item(
                3,
                PhysicalRect::new(0.0, 0.0, 40.0, 40.0),
                DisplayCommand::PopStackingContext,
            ),
        ];
        let display = list(
            PhysicalSize {
                width: 40.0,
                height: 40.0,
            },
            items,
        );
        let cache = CompositorCache::from_display_list(std::sync::Arc::new(display), Color::WHITE)
            .expect("the scene is compositable");
        assert_eq!(cache.layer_count(), 2, "one root segment and one layer");
        let output = cache.render(PhysicalPoint::default(), &NoGlyphMasks, None, None);
        assert_eq!(output.surface.pixel(0, 0), Some(Color::WHITE));
        // The child moves right by 10 and is half transparent over white.
        assert_eq!(output.surface.pixel(15, 5), Some(Color::rgb(128, 128, 128)));
        assert_eq!(output.surface.pixel(5, 5), Some(Color::WHITE));
        assert!(output.diagnostics.is_empty());
    }

    #[test]
    fn scrolling_reuses_cached_bands_and_only_rasterizes_new_ones() {
        // A 600px viewport over 2048px of content: the first frame needs two
        // bands, a 16px scroll needs neither band again, and a jump to the
        // bottom exposes new bands.
        let viewport = PhysicalSize {
            width: 100.0,
            height: 600.0,
        };
        let items = vec![
            item(
                0,
                PhysicalRect::new(0.0, 0.0, 100.0, 2048.0),
                rect(0.0, 0.0, 100.0, 2048.0, Color::WHITE),
            ),
            item(
                1,
                PhysicalRect::new(0.0, 200.0, 100.0, 20.0),
                rect(0.0, 200.0, 100.0, 20.0, Color::rgb(255, 0, 0)),
            ),
        ];
        let display = list(viewport, items);
        let cache = CompositorCache::from_display_list(std::sync::Arc::new(display), Color::WHITE)
            .expect("the scene is compositable");
        let first = cache.render(PhysicalPoint::default(), &NoGlyphMasks, None, None);
        assert_eq!(first.surface.pixel(10, 210), Some(Color::rgb(255, 0, 0)));
        let first_stats = cache.stats();
        assert!(first_stats.bands_rasterized > 0);
        assert_eq!(first_stats.last_frame_bands, 2);
        assert_eq!(first_stats.frames, 1);

        let scrolled = cache.render(PhysicalPoint { x: 0.0, y: 16.0 }, &NoGlyphMasks, None, None);
        let second_stats = cache.stats();
        assert_eq!(
            second_stats.last_frame_bands, 0,
            "the same two bands cover 16px"
        );
        assert_eq!(second_stats.bands_rasterized, first_stats.bands_rasterized);
        assert_eq!(scrolled.surface.pixel(10, 194), Some(Color::rgb(255, 0, 0)));

        let _ = cache.render(
            PhysicalPoint { x: 0.0, y: 1400.0 },
            &NoGlyphMasks,
            None,
            None,
        );
        let third_stats = cache.stats();
        assert!(
            third_stats.last_frame_bands > 0,
            "the jump exposes new bands"
        );
        assert!(third_stats.cached_bands <= 4);
        assert_eq!(third_stats.frames, 3);
    }

    #[test]
    fn the_composited_pixels_match_a_full_raster() {
        let viewport = PhysicalSize {
            width: 80.0,
            height: 60.0,
        };
        let items = vec![
            item(
                0,
                PhysicalRect::new(0.0, 0.0, 80.0, 60.0),
                rect(0.0, 0.0, 80.0, 60.0, Color::WHITE),
            ),
            item(
                1,
                PhysicalRect::new(0.0, 10.0, 20.0, 20.0),
                rect(0.0, 10.0, 20.0, 20.0, Color::rgb(0, 0, 255)),
            ),
            item(
                2,
                PhysicalRect::new(0.0, 0.0, 20.0, 20.0),
                DisplayCommand::PushStackingContext(context(
                    Transform2D {
                        translate_x: 5.0,
                        translate_y: 5.0,
                        ..Transform2D::default()
                    },
                    0.5,
                )),
            ),
            item(
                3,
                PhysicalRect::new(0.0, 0.0, 20.0, 20.0),
                rect(0.0, 0.0, 20.0, 20.0, Color::rgb(255, 0, 0)),
            ),
            item(
                4,
                PhysicalRect::new(0.0, 0.0, 20.0, 20.0),
                DisplayCommand::PopStackingContext,
            ),
            item(
                5,
                PhysicalRect::new(0.0, 40.0, 80.0, 10.0),
                rect(0.0, 40.0, 80.0, 10.0, Color::rgb(0, 128, 0)),
            ),
        ];
        let display = std::sync::Arc::new(list(viewport, items));
        let compositor =
            CompositorCache::from_display_list(std::sync::Arc::clone(&display), Color::BLACK)
                .expect("the scene is compositable");
        let composited = compositor.render(PhysicalPoint::default(), &NoGlyphMasks, None, None);
        let full = CpuRasterizer.rasterize(display.as_ref(), Color::BLACK, &NoGlyphMasks);
        assert_eq!(composited.surface, full.surface);
    }

    #[test]
    fn a_transformed_layer_is_not_clipped_to_its_untransformed_bounds() {
        let viewport = PhysicalSize {
            width: 80.0,
            height: 60.0,
        };
        let items = vec![
            item(
                0,
                PhysicalRect::new(0.0, 0.0, 80.0, 60.0),
                rect(0.0, 0.0, 80.0, 60.0, Color::WHITE),
            ),
            item(
                1,
                PhysicalRect::new(0.0, 0.0, 20.0, 20.0),
                DisplayCommand::PushStackingContext(context(
                    Transform2D {
                        translate_x: 50.0,
                        ..Transform2D::default()
                    },
                    1.0,
                )),
            ),
            item(
                2,
                PhysicalRect::new(0.0, 0.0, 20.0, 20.0),
                rect(0.0, 0.0, 20.0, 20.0, Color::rgb(255, 0, 0)),
            ),
            item(
                3,
                PhysicalRect::new(0.0, 0.0, 20.0, 20.0),
                DisplayCommand::PopStackingContext,
            ),
        ];
        let display = list(viewport, items);
        let cache = CompositorCache::from_display_list(std::sync::Arc::new(display), Color::BLACK)
            .expect("translation is compositable");
        let output = cache.render(PhysicalPoint::default(), &NoGlyphMasks, None, None);
        assert_eq!(output.surface.pixel(55, 5), Some(Color::rgb(255, 0, 0)));
        assert_eq!(output.surface.pixel(5, 5), Some(Color::WHITE));
    }

    #[test]
    fn twenty_sixteen_pixel_scrolls_rasterize_at_most_one_new_band() {
        let viewport = PhysicalSize {
            width: 120.0,
            height: 600.0,
        };
        let items = vec![
            item(
                0,
                PhysicalRect::new(0.0, 0.0, 120.0, 4096.0),
                rect(0.0, 0.0, 120.0, 4096.0, Color::WHITE),
            ),
            item(
                1,
                PhysicalRect::new(0.0, 100.0, 120.0, 20.0),
                rect(0.0, 100.0, 120.0, 20.0, Color::rgb(0, 0, 255)),
            ),
        ];
        let display = list(viewport, items);
        let cache = CompositorCache::from_display_list(std::sync::Arc::new(display), Color::WHITE)
            .expect("the scene is compositable");
        let _ = cache.render(PhysicalPoint::default(), &NoGlyphMasks, None, None);
        for frame in 1..=20 {
            let _ = cache.render(
                PhysicalPoint {
                    x: 0.0,
                    y: frame as f32 * 16.0,
                },
                &NoGlyphMasks,
                None,
                None,
            );
            let stats = cache.stats();
            assert_eq!(stats.frames, frame + 1);
            assert!(
                stats.last_frame_bands <= 1,
                "frame {frame} rasterized {} new bands; a 16px scroll crosses at most one 512px band boundary",
                stats.last_frame_bands
            );
        }
    }

    #[test]
    fn fixed_content_is_retained_in_viewport_space() {
        let viewport = PhysicalSize {
            width: 80.0,
            height: 60.0,
        };
        let mut fixed = item(
            1,
            PhysicalRect::new(0.0, 10.0, 20.0, 10.0),
            rect(0.0, 10.0, 20.0, 10.0, Color::rgb(255, 0, 0)),
        );
        fixed.coordinate_space = PaintCoordinateSpace::Viewport;
        let items = vec![
            item(
                0,
                PhysicalRect::new(0.0, 0.0, 80.0, 200.0),
                rect(0.0, 0.0, 80.0, 200.0, Color::WHITE),
            ),
            item(
                2,
                PhysicalRect::new(0.0, 100.0, 80.0, 10.0),
                rect(0.0, 100.0, 80.0, 10.0, Color::rgb(0, 0, 255)),
            ),
        ];
        let mut all = items;
        all.insert(1, fixed);
        let display = std::sync::Arc::new(list(viewport, all));
        let cache = CompositorCache::from_display_list(Arc::clone(&display), Color::WHITE)
            .expect("a fixed item is a viewport-space segment");
        let composited = cache.render(PhysicalPoint { x: 0.0, y: 50.0 }, &NoGlyphMasks, None, None);
        let full = CpuRasterizer.rasterize_viewport(
            display.as_ref(),
            Color::WHITE,
            &NoGlyphMasks,
            PhysicalPoint { x: 0.0, y: 50.0 },
        );
        assert_eq!(composited.surface, full.surface);
        assert_eq!(composited.surface.pixel(5, 15), Some(Color::rgb(255, 0, 0)));
        assert_eq!(composited.surface.pixel(5, 55), Some(Color::rgb(0, 0, 255)));
    }

    #[test]
    fn scaled_and_rotated_layers_match_the_full_raster() {
        let viewport = PhysicalSize {
            width: 80.0,
            height: 60.0,
        };
        let transforms = [
            Transform2D {
                scale_x: 2.0,
                scale_y: 2.0,
                ..Transform2D::default()
            },
            Transform2D {
                scale_x: 0.866_025_4,
                skew_x: -0.5,
                skew_y: 0.5,
                scale_y: 0.866_025_4,
                ..Transform2D::default()
            },
        ];
        for transform in transforms {
            let items = vec![
                item(
                    0,
                    PhysicalRect::new(0.0, 0.0, 80.0, 60.0),
                    rect(0.0, 0.0, 80.0, 60.0, Color::WHITE),
                ),
                item(
                    1,
                    PhysicalRect::new(0.0, 0.0, 20.0, 20.0),
                    DisplayCommand::PushStackingContext(context(transform, 0.75)),
                ),
                item(
                    2,
                    PhysicalRect::new(0.0, 0.0, 20.0, 20.0),
                    rect(0.0, 0.0, 20.0, 20.0, Color::rgb(255, 0, 0)),
                ),
                item(
                    3,
                    PhysicalRect::new(0.0, 0.0, 20.0, 20.0),
                    DisplayCommand::PopStackingContext,
                ),
            ];
            let display = std::sync::Arc::new(list(viewport, items));
            let cache =
                CompositorCache::from_display_list(std::sync::Arc::clone(&display), Color::WHITE)
                    .expect("an affine layer is compositable");
            let composited = cache.render(PhysicalPoint::default(), &NoGlyphMasks, None, None);
            let full = CpuRasterizer.rasterize(display.as_ref(), Color::WHITE, &NoGlyphMasks);
            assert_eq!(
                composited.surface, full.surface,
                "transform {transform:?} differs from the complete raster"
            );
        }
    }

    #[test]
    fn a_sticky_layer_is_displaced_at_composite_time() {
        let viewport = PhysicalSize {
            width: 80.0,
            height: 60.0,
        };
        let constraint = crate::layout::StickyConstraint {
            margin_rect: PhysicalRect::new(0.0, 0.0, 80.0, 20.0),
            containing_block: PhysicalRect::new(0.0, 0.0, 80.0, 200.0),
            insets: crate::layout::StickyInsets {
                top: Some(0.0),
                ..crate::layout::StickyInsets::default()
            },
            scrollport: viewport,
            scrollport_origin: PhysicalPoint::default(),
            scroll_container: None,
        };
        let items = vec![
            item(
                0,
                PhysicalRect::new(0.0, 0.0, 80.0, 200.0),
                rect(0.0, 0.0, 80.0, 200.0, Color::WHITE),
            ),
            item(
                1,
                PhysicalRect::new(0.0, 0.0, 80.0, 20.0),
                DisplayCommand::PushSticky(Box::new(constraint)),
            ),
            item(
                2,
                PhysicalRect::new(0.0, 0.0, 80.0, 20.0),
                rect(0.0, 0.0, 80.0, 20.0, Color::rgb(255, 0, 0)),
            ),
            item(
                3,
                PhysicalRect::new(0.0, 0.0, 80.0, 20.0),
                DisplayCommand::PopSticky,
            ),
            item(
                4,
                PhysicalRect::new(0.0, 100.0, 80.0, 10.0),
                rect(0.0, 100.0, 80.0, 10.0, Color::rgb(0, 0, 255)),
            ),
        ];
        let display = std::sync::Arc::new(list(viewport, items));
        let cache =
            CompositorCache::from_display_list(std::sync::Arc::clone(&display), Color::WHITE)
                .expect("a top-level sticky scope is compositable");
        for offset in [0.0, 10.0, 50.0, 120.0, 300.0] {
            let composited = cache.render(
                PhysicalPoint { x: 0.0, y: offset },
                &NoGlyphMasks,
                None,
                None,
            );
            let full = CpuRasterizer.rasterize_viewport(
                display.as_ref(),
                Color::WHITE,
                &NoGlyphMasks,
                PhysicalPoint { x: 0.0, y: offset },
            );
            assert_eq!(
                composited.surface, full.surface,
                "sticky scene differs from the complete raster at scroll {offset}"
            );
        }
        let composited = cache.render(PhysicalPoint { x: 0.0, y: 50.0 }, &NoGlyphMasks, None, None);
        assert_eq!(composited.surface.pixel(5, 5), Some(Color::rgb(255, 0, 0)));
        assert_eq!(composited.surface.pixel(5, 55), Some(Color::rgb(0, 0, 255)));
    }

    #[test]
    fn a_blurred_layer_matches_the_complete_raster_across_band_boundaries() {
        // The layer is taller than one 512px band. A banded filter would blur
        // each half against transparent pixels at the split; the retained
        // effect surface filters the whole layer once.
        let viewport = PhysicalSize {
            width: 120.0,
            height: 700.0,
        };
        let items = vec![
            item(
                0,
                PhysicalRect::new(0.0, 0.0, 120.0, 700.0),
                rect(0.0, 0.0, 120.0, 700.0, Color::WHITE),
            ),
            item(
                1,
                PhysicalRect::new(0.0, 0.0, 120.0, 700.0),
                DisplayCommand::PushStackingContext(context_with_effects(
                    Transform2D::default(),
                    1.0,
                    Some(blur_filter(3.0)),
                    None,
                )),
            ),
            item(
                2,
                PhysicalRect::new(30.0, 50.0, 60.0, 600.0),
                rect(30.0, 50.0, 60.0, 600.0, Color::rgb(220, 30, 30)),
            ),
            item(
                3,
                PhysicalRect::new(30.0, 470.0, 60.0, 80.0),
                rect(30.0, 470.0, 60.0, 80.0, Color::rgb(30, 60, 220)),
            ),
            item(
                4,
                PhysicalRect::new(0.0, 0.0, 120.0, 700.0),
                DisplayCommand::PopStackingContext,
            ),
        ];
        let display = std::sync::Arc::new(list(viewport, items));
        let cache =
            CompositorCache::from_display_list(std::sync::Arc::clone(&display), Color::WHITE)
                .expect("a filtered top-level layer is retainable");
        assert_eq!(cache.layer_count(), 2, "one root segment and one layer");
        let blur_reach = (3.0_f32 * 3.0).ceil();
        for offset in [PhysicalPoint::default(), PhysicalPoint { x: 0.0, y: 16.0 }] {
            // Keep the fixture's whole blur support inside the complete
            // raster's viewport-sized offscreen surface so the reference does
            // not clip effect input at its own edge.
            assert!(50.0 - blur_reach >= offset.y);
            assert!(650.0 + blur_reach <= offset.y + viewport.height);
            assert_matches_complete_raster(&cache, &display, Color::WHITE, offset, "blurred layer");
        }
        let stats = cache.stats();
        assert!(
            stats.cached_bands >= 1,
            "the effect surface must be retained"
        );
    }

    #[test]
    fn a_gradient_masked_layer_matches_the_complete_raster() {
        let viewport = PhysicalSize {
            width: 120.0,
            height: 120.0,
        };
        let masked = PhysicalRect::new(20.0, 20.0, 60.0, 60.0);
        let items = vec![
            item(
                0,
                PhysicalRect::new(0.0, 0.0, 120.0, 120.0),
                rect(0.0, 0.0, 120.0, 120.0, Color::WHITE),
            ),
            item(
                1,
                masked,
                DisplayCommand::PushStackingContext(context_with_effects(
                    Transform2D::default(),
                    1.0,
                    None,
                    Some(linear_alpha_mask(masked)),
                )),
            ),
            item(
                2,
                masked,
                rect(
                    masked.origin.x,
                    masked.origin.y,
                    masked.size.width,
                    masked.size.height,
                    Color::rgb(220, 40, 40),
                ),
            ),
            item(3, masked, DisplayCommand::PopStackingContext),
        ];
        let display = std::sync::Arc::new(list(viewport, items));
        let cache =
            CompositorCache::from_display_list(std::sync::Arc::clone(&display), Color::WHITE)
                .expect("a masked top-level layer is retainable");
        for offset in [PhysicalPoint::default(), PhysicalPoint { x: 0.0, y: 37.0 }] {
            assert_matches_complete_raster(
                &cache,
                &display,
                Color::WHITE,
                offset,
                "gradient-masked layer",
            );
        }
        let output = cache.render(PhysicalPoint::default(), &NoGlyphMasks, None, None);
        let middle = output.surface.pixel(50, 50).expect("inside the mask");
        assert_ne!(middle, Color::rgb(220, 40, 40), "the mask alpha must apply");
        assert_ne!(
            middle,
            Color::WHITE,
            "the masked rect still covers the pixel"
        );
    }

    #[test]
    fn a_filtered_layer_with_opacity_and_transform_matches_the_complete_raster() {
        let viewport = PhysicalSize {
            width: 160.0,
            height: 160.0,
        };
        let filter = FilterEffect {
            operations: vec![
                FilterOperation::Brightness(1.3),
                FilterOperation::Grayscale(0.4),
            ],
        };
        let transform = Transform2D {
            scale_x: 1.25,
            scale_y: 1.25,
            translate_x: 3.0,
            translate_y: 2.0,
            ..Transform2D::default()
        };
        let items = vec![
            item(
                0,
                PhysicalRect::new(0.0, 0.0, 160.0, 160.0),
                rect(0.0, 0.0, 160.0, 160.0, Color::WHITE),
            ),
            item(
                1,
                PhysicalRect::new(30.0, 30.0, 40.0, 40.0),
                DisplayCommand::PushStackingContext(context_with_effects(
                    transform,
                    0.55,
                    Some(filter),
                    None,
                )),
            ),
            item(
                2,
                PhysicalRect::new(30.0, 30.0, 40.0, 40.0),
                rect(30.0, 30.0, 40.0, 40.0, Color::rgb(210, 60, 30)),
            ),
            item(
                3,
                PhysicalRect::new(48.0, 38.0, 18.0, 24.0),
                rect(48.0, 38.0, 18.0, 24.0, Color::rgb(20, 80, 200)),
            ),
            item(
                4,
                PhysicalRect::new(30.0, 30.0, 40.0, 40.0),
                DisplayCommand::PopStackingContext,
            ),
        ];
        let display = std::sync::Arc::new(list(viewport, items));
        let cache =
            CompositorCache::from_display_list(std::sync::Arc::clone(&display), Color::WHITE)
                .expect("a filtered transformed layer is retainable");
        for offset in [PhysicalPoint::default(), PhysicalPoint { x: 0.0, y: 16.0 }] {
            assert_matches_complete_raster(
                &cache,
                &display,
                Color::WHITE,
                offset,
                "filtered transformed layer",
            );
        }
    }

    #[test]
    fn a_viewport_space_filter_and_mask_match_the_complete_raster() {
        let viewport = PhysicalSize {
            width: 120.0,
            height: 80.0,
        };
        let masked = PhysicalRect::new(10.0, 10.0, 70.0, 50.0);
        let mut items = vec![
            item(
                0,
                PhysicalRect::new(0.0, 0.0, 120.0, 400.0),
                rect(0.0, 0.0, 120.0, 400.0, Color::WHITE),
            ),
            item(
                1,
                masked,
                DisplayCommand::PushStackingContext(context_with_effects(
                    Transform2D::default(),
                    1.0,
                    Some(FilterEffect {
                        operations: vec![FilterOperation::Brightness(0.8)],
                    }),
                    Some(linear_alpha_mask(masked)),
                )),
            ),
            item(
                2,
                masked,
                rect(10.0, 10.0, 70.0, 50.0, Color::rgb(40, 160, 60)),
            ),
            item(3, masked, DisplayCommand::PopStackingContext),
        ];
        for fixed in &mut items {
            fixed.coordinate_space = PaintCoordinateSpace::Viewport;
        }
        let display = std::sync::Arc::new(list(viewport, items));
        let cache =
            CompositorCache::from_display_list(std::sync::Arc::clone(&display), Color::WHITE)
                .expect("a fixed filtered/masked layer is retainable");
        for offset in [PhysicalPoint::default(), PhysicalPoint { x: 0.0, y: 41.0 }] {
            assert_matches_complete_raster(
                &cache,
                &display,
                Color::WHITE,
                offset,
                "viewport-space filter and mask",
            );
        }
    }

    /// A fixed-position item inside an overflow clip is legal in a display
    /// list: the complete raster applies the scrolled document clip to the
    /// viewport-space item. The retained path must keep that scene instead of
    /// rejecting it and forcing a full raster for every scroll frame.
    #[test]
    fn a_viewport_item_inside_a_document_clip_stays_retained() {
        let viewport = PhysicalSize {
            width: 120.0,
            height: 100.0,
        };
        let clip = PhysicalRect::new(8.0, 8.0, 80.0, 50.0);
        let mut fixed = item(
            2,
            PhysicalRect::new(0.0, 0.0, 40.0, 20.0),
            rect(0.0, 0.0, 40.0, 20.0, Color::rgb(200, 40, 40)),
        );
        fixed.coordinate_space = PaintCoordinateSpace::Viewport;
        let items = vec![
            item(0, clip, DisplayCommand::PushClip(ClipShape::Rect(clip))),
            item(
                1,
                clip,
                rect(0.0, 0.0, 120.0, 300.0, Color::rgb(238, 238, 238)),
            ),
            fixed,
            item(3, clip, DisplayCommand::PopClip),
            item(
                4,
                PhysicalRect::new(0.0, 60.0, 120.0, 240.0),
                rect(0.0, 60.0, 120.0, 240.0, Color::rgb(10, 120, 10)),
            ),
        ];
        let display = std::sync::Arc::new(list(viewport, items));
        let cache =
            CompositorCache::from_display_list(std::sync::Arc::clone(&display), Color::WHITE)
                .expect("a viewport item inside a document clip is retainable");
        for y in [0.0, 7.0, 30.0, 55.0, 90.0] {
            assert_matches_complete_raster(
                &cache,
                &display,
                Color::WHITE,
                PhysicalPoint { x: 0.0, y },
                "viewport item inside a document clip",
            );
        }
    }

    /// The composite-time clip only supports axis-aligned rectangles. A
    /// rounded clip around a coordinate-space boundary must keep the
    /// conservative complete-raster fallback instead of approximating it.
    #[test]
    fn a_rounded_clip_across_a_coordinate_space_boundary_falls_back() {
        let viewport = PhysicalSize {
            width: 120.0,
            height: 100.0,
        };
        let clip = ClipShape::RoundedRect {
            rect: PhysicalRect::new(8.0, 8.0, 80.0, 50.0),
            radii: crate::paint::display_list::CornerRadii {
                top_left: 6.0,
                top_right: 6.0,
                bottom_right: 6.0,
                bottom_left: 6.0,
            },
        };
        let mut fixed = item(
            2,
            PhysicalRect::new(0.0, 0.0, 40.0, 20.0),
            rect(0.0, 0.0, 40.0, 20.0, Color::rgb(200, 40, 40)),
        );
        fixed.coordinate_space = PaintCoordinateSpace::Viewport;
        let items = vec![
            item(
                0,
                PhysicalRect::new(8.0, 8.0, 80.0, 50.0),
                DisplayCommand::PushClip(clip),
            ),
            fixed,
            item(
                3,
                PhysicalRect::new(8.0, 8.0, 80.0, 50.0),
                DisplayCommand::PopClip,
            ),
        ];
        let display = list(viewport, items);
        assert!(
            CompositorCache::from_display_list(std::sync::Arc::new(display), Color::WHITE)
                .is_none(),
            "the complete-raster fallback must remain for an unsupported clip shape"
        );
    }

    /// The all-rect fast path of the complete rasterizer admits integer pixel
    /// `x` when `x >= left && x < right`. Rounding clip edges outward to whole
    /// pixels reproduces exactly that set when they reach the blit.
    #[test]
    fn inherited_viewport_clips_follow_the_rect_cell_rule() {
        let viewport = PhysicalRect::new(0.0, 0.0, 120.0, 100.0);
        let clips = [super::InheritedClip {
            shape: ClipShape::Rect(PhysicalRect::new(8.4, 7.6, 80.2, 50.3)),
            space: PaintCoordinateSpace::Document,
        }];
        assert_eq!(
            super::inherited_viewport_clip(&clips, PhysicalPoint { x: 0.0, y: 0.0 }, viewport),
            Some(PhysicalRect::new(9.0, 8.0, 80.0, 50.0))
        );
        assert_eq!(
            super::inherited_viewport_clip(&clips, PhysicalPoint { x: 10.0, y: 4.0 }, viewport),
            Some(PhysicalRect::new(0.0, 4.0, 79.0, 50.0))
        );
    }

    /// An empty stacking context paints no pixels anywhere. It must not force
    /// the complete-raster fallback just because its bounds are empty.
    #[test]
    fn an_empty_stacking_context_does_not_invalidate_the_scene() {
        let viewport = PhysicalSize {
            width: 80.0,
            height: 60.0,
        };
        let bounds = PhysicalRect::new(0.0, 0.0, 80.0, 60.0);
        let items = vec![
            item(0, bounds, rect(0.0, 0.0, 80.0, 60.0, Color::WHITE)),
            item(
                1,
                bounds,
                DisplayCommand::PushStackingContext(context(
                    Transform2D {
                        translate_x: 8.0,
                        ..Transform2D::default()
                    },
                    0.0,
                )),
            ),
            item(2, bounds, DisplayCommand::PopStackingContext),
            item(
                3,
                PhysicalRect::new(10.0, 10.0, 20.0, 20.0),
                rect(10.0, 10.0, 20.0, 20.0, Color::rgb(0, 0, 255)),
            ),
        ];
        let display = std::sync::Arc::new(list(viewport, items));
        let cache =
            CompositorCache::from_display_list(std::sync::Arc::clone(&display), Color::WHITE)
                .expect("an empty stacking context is retainable");
        assert_matches_complete_raster(
            &cache,
            &display,
            Color::WHITE,
            PhysicalPoint::default(),
            "empty stacking context",
        );
    }
}
