//! Retained layer tree and banded compositor for scrolling.
//!
//! Layout owns geometry and display-list construction; this module owns the
//! retained frame that survives between scroll positions. A scene is split
//! into three kinds of retained surface:
//!
//! - **root segments**: contiguous runs of ordinary display items between
//!   top-level stacking contexts, so paint order is preserved;
//! - **layers**: a top-level transform/opacity stacking context, rasterized
//!   once and composited through its own matrix and alpha;
//! - **bands**: every surface is cut into fixed-height bands, so a long
//!   document does not need one giant allocation and a scroll only rasterizes
//!   the bands that were not already cached.
//!
//! The compositor is deliberately conservative. A scene containing a sticky
//! scope, a viewport-space item (fixed positioning), a filter, a mask, a
//! non-normal blend mode, or a non-translation transform returns `None` and
//! the caller keeps the existing complete-raster path. The supported subset is
//! the common scrolling page: ordinary document content plus translated or
//! transparent layers.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use crate::image::ImageResources;
use crate::layout::{PhysicalPoint, PhysicalRect, PhysicalSize};

use super::color::Color;
use super::display_list::{
    BlendMode, DisplayCommand, DisplayItem, DisplayList, PaintCoordinateSpace, Transform2D,
};
use super::raster::{
    CpuRasterOutput, CpuRasterizer, GlyphMaskProvider, RasterControl, Surface, composite_surface,
};

/// The physical height of one retained compositor band.
pub const DEFAULT_COMPOSITOR_BAND_HEIGHT: u32 = 512;

/// A transformed or transparent top-level stacking context.
#[derive(Clone, Debug, PartialEq)]
struct CompositedLayer {
    /// The display-list range, excluding the push/pop commands themselves.
    start: usize,
    end: usize,
    /// Untransformed document-space bounds of the items.
    bounds: PhysicalRect,
    transform: Transform2D,
    opacity: f32,
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
    pub fn from_display_list(display_list: Arc<DisplayList>, background: Color) -> Option<Self> {
        let items = display_list.items();
        if items
            .iter()
            .any(|item| item.coordinate_space != PaintCoordinateSpace::Document)
        {
            return None;
        }
        let mut segments = Vec::new();
        let mut layers = Vec::new();
        let mut ops = Vec::new();
        let mut index = 0_usize;
        let mut root_start = 0_usize;
        let mut clip_depth = 0_usize;
        while index < items.len() {
            match &items[index].command {
                DisplayCommand::PushClip(_) => {
                    clip_depth += 1;
                    index += 1;
                }
                DisplayCommand::PopClip => {
                    clip_depth = clip_depth.saturating_sub(1);
                    index += 1;
                }
                DisplayCommand::PushSticky(_)
                | DisplayCommand::PopSticky
                | DisplayCommand::PushTransform(_)
                | DisplayCommand::PopTransform => return None,
                DisplayCommand::PushStackingContext(context) => {
                    // A context inside an ambient clip would need that clip
                    // copied into its layer and removed from the root segment.
                    // The conservative answer is the complete raster path.
                    if clip_depth != 0
                        || context.filter.is_some()
                        || context.mask.is_some()
                        || context.blend_mode != BlendMode::Normal
                        || !context.transform.is_translation()
                    {
                        return None;
                    }
                    let start = index + 1;
                    let end = matching_pop(items, index)?;
                    // Sticky and viewport items inside a layer are
                    // scroll-dependent for the same reason they are at the
                    // root: the retained surface is in document coordinates.
                    if items[start..end].iter().any(|item| {
                        item.coordinate_space != PaintCoordinateSpace::Document
                            || matches!(
                                &item.command,
                                DisplayCommand::PushSticky(_) | DisplayCommand::PopSticky
                            )
                    }) {
                        return None;
                    }
                    let bounds = item_bounds(&items[start..end])?;
                    if bounds.size.width > 8192.0 {
                        return None;
                    }
                    if index > root_start {
                        let segment = make_segment(items, root_start, index)?;
                        ops.push(LayerOp::Segment(segments.len()));
                        segments.push(segment);
                    }
                    let layer = layers.len();
                    layers.push(CompositedLayer {
                        start,
                        end,
                        bounds,
                        transform: context.transform,
                        opacity: context.opacity.clamp(0.0, 1.0),
                    });
                    ops.push(LayerOp::Layer(layer));
                    index = end + 1;
                    root_start = index;
                }
                _ => index += 1,
            }
        }
        if root_start < items.len() {
            let segment = make_segment(items, root_start, items.len())?;
            ops.push(LayerOp::Segment(segments.len()));
            segments.push(segment);
        }
        let viewport = display_list.viewport;
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
                    last_frame_bands += self.composite_surface(
                        &mut output,
                        &mut state,
                        layer.start,
                        layer.end,
                        layer.bounds,
                        layer.transform,
                        layer.opacity,
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
        let viewport_top = viewport_origin.y.max(0.0);
        let viewport_bottom = viewport_top + self.viewport.height.max(0.0);
        let first = ((viewport_top - bounds.origin.y) / self.band_height as f32)
            .floor()
            .max(0.0) as u32;
        let last = ((viewport_bottom - bounds.origin.y) / self.band_height as f32)
            .ceil()
            .max(0.0) as u32;
        let band_count = (bounds.size.height / self.band_height as f32)
            .ceil()
            .max(0.0) as u32;
        let last = last.min(band_count);
        let mut rasterized = 0_u32;
        for band in first..last {
            if control.is_some_and(RasterControl::is_cancelled) {
                break;
            }
            let band_y = bounds.origin.y + band as f32 * self.band_height as f32;
            let band_height = (bounds.origin.y + bounds.size.height - band_y)
                .min(self.band_height as f32)
                .max(0.0);
            let band_size = PhysicalSize {
                width: bounds.size.width,
                height: band_height,
            };
            if band_size.width <= 0.0 || band_size.height <= 0.0 {
                continue;
            }
            let key = (kind, index, band);
            if let std::collections::hash_map::Entry::Vacant(entry) = state.bands.entry(key) {
                let raster = CpuRasterizer.rasterize_item_slice(
                    &self.display_list.items()[start..end],
                    band_size,
                    Color::TRANSPARENT,
                    glyphs,
                    PhysicalPoint {
                        x: bounds.origin.x,
                        y: band_y,
                    },
                    images,
                );
                diagnostics.extend(raster.diagnostics);
                entry.insert(raster.surface);
                state.stats.bands_rasterized = state.stats.bands_rasterized.saturating_add(1);
                rasterized = rasterized.saturating_add(1);
            }
            let Some(source) = state.bands.get(&key) else {
                continue;
            };
            let to_viewport = Transform2D {
                translate_x: -viewport_origin.x,
                translate_y: -viewport_origin.y,
                ..Transform2D::default()
            };
            let to_band = Transform2D {
                translate_x: bounds.origin.x,
                translate_y: band_y,
                ..Transform2D::default()
            };
            let mapped = if kind == 0 {
                to_viewport.then(&to_band)
            } else {
                to_viewport.then(&transform.then(&to_band))
            };
            let clip = Some(PhysicalRect::new(
                0.0,
                0.0,
                self.viewport.width,
                self.viewport.height,
            ));
            if !composite_surface(output, source, mapped, opacity, clip, control) {
                break;
            }
        }
        rasterized
    }
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

fn make_segment(items: &[DisplayItem], start: usize, end: usize) -> Option<RootSegment> {
    let bounds = item_bounds(&items[start..end])?;
    if bounds.size.width > 8192.0 {
        return None;
    }
    Some(RootSegment { start, end, bounds })
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
    #![allow(clippy::float_cmp)]

    use super::{CompositorCache, DEFAULT_COMPOSITOR_BAND_HEIGHT};
    use crate::dom::DomRevision;
    use crate::layout::{PhysicalPoint, PhysicalRect, PhysicalSize};
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
        StackingContext {
            opacity,
            transform,
            blend_mode: BlendMode::Normal,
            isolated: true,
            reason: CompositingReason::Transform,
            filter: None,
            mask: None,
        }
    }

    fn list(viewport: PhysicalSize, items: Vec<DisplayItem>) -> DisplayList {
        DisplayList::from_items(DomRevision::default(), viewport, items)
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
    fn fixed_content_falls_back_to_the_complete_raster() {
        let viewport = PhysicalSize {
            width: 40.0,
            height: 40.0,
        };
        let mut fixed = item(
            0,
            PhysicalRect::new(0.0, 0.0, 40.0, 40.0),
            rect(0.0, 0.0, 40.0, 40.0, Color::WHITE),
        );
        fixed.coordinate_space = PaintCoordinateSpace::Viewport;
        let fixed_display = list(viewport, vec![fixed]);
        assert!(
            CompositorCache::from_display_list(std::sync::Arc::new(fixed_display), Color::WHITE)
                .is_none(),
            "a viewport-space item cannot be retained in document coordinates"
        );
        assert_eq!(DEFAULT_COMPOSITOR_BAND_HEIGHT, 512);
    }
}
