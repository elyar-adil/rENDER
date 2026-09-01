//! Immutable paint-scene metadata used by retained CPU rasterizers.
//!
//! A [`PaintScene`] deliberately sits between layout-owned display-list
//! construction and rasterization. Layout engines can keep producing
//! [`DisplayList`] values while raster backends gain a stable, immutable unit
//! for scheduling, damage calculation, and retained-frame reuse.

use std::sync::Arc;

use crate::dom::DomRevision;
use crate::layout::{PhysicalPoint, PhysicalRect, PhysicalSize};

use super::color::Color;
use super::display_list::{DisplayCommand, DisplayItem, DisplayList, PaintCoordinateSpace};
use super::raster::Surface;

/// The physical edge length of a paint tile.
///
/// Tiles are deliberately a raster concern rather than a layout primitive.
/// Keeping the value here makes it possible to revise layout independently of
/// work scheduling and invalidation.
pub const DEFAULT_PAINT_TILE_SIZE: u32 = 128;

/// A viewport-relative rectangular tile that can be scheduled independently
/// when its scene classification permits it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaintTile {
    pub column: u32,
    pub row: u32,
    pub bounds: PhysicalRect,
}

/// Why an item or scene must use the conservative complete-raster path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaintFallbackReason {
    /// The command changes clip, transform, or compositing state.
    StatefulCommand,
    /// The command has no bounded, independently replayable CPU path yet.
    UnsupportedCommand,
    /// Viewport and document items cannot safely share one retained surface.
    MixedCoordinateSpaces,
}

/// Whether a paint chunk can be replayed into an isolated damage rectangle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaintChunkClassification {
    TileSafe {
        coordinate_space: PaintCoordinateSpace,
    },
    FullRepaintRequired(PaintFallbackReason),
}

/// A contiguous immutable range of display-list items with one scheduling
/// classification.
#[derive(Clone, Debug, PartialEq)]
pub struct PaintChunk {
    item_start: usize,
    item_end: usize,
    bounds: PhysicalRect,
    classification: PaintChunkClassification,
}

impl PaintChunk {
    #[must_use]
    pub const fn item_start(&self) -> usize {
        self.item_start
    }

    #[must_use]
    pub const fn item_end(&self) -> usize {
        self.item_end
    }

    #[must_use]
    pub const fn item_count(&self) -> usize {
        self.item_end - self.item_start
    }

    #[must_use]
    pub const fn bounds(&self) -> PhysicalRect {
        self.bounds
    }

    #[must_use]
    pub const fn classification(&self) -> PaintChunkClassification {
        self.classification
    }
}

/// Scene-wide classification used to select retained-frame behavior.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaintSceneClassification {
    TileSafe,
    FullRepaintRequired(PaintFallbackReason),
}

/// Immutable, shareable representation of a display list and its raster
/// scheduling metadata.
#[derive(Clone, Debug, PartialEq)]
pub struct PaintScene {
    display_list: Arc<DisplayList>,
    chunks: Arc<[PaintChunk]>,
    classification: PaintSceneClassification,
}

impl PaintScene {
    #[must_use]
    pub fn from_display_list(display_list: DisplayList) -> Self {
        Self::from_shared_display_list(Arc::new(display_list))
    }

    #[must_use]
    pub fn from_shared_display_list(display_list: Arc<DisplayList>) -> Self {
        let chunks = display_list
            .items()
            .iter()
            .enumerate()
            .map(|(index, item)| PaintChunk {
                item_start: index,
                item_end: index + 1,
                bounds: item.bounds,
                classification: classify_item(item),
            })
            .collect::<Vec<_>>();

        let classification = classify_scene(&chunks);
        Self {
            display_list,
            chunks: chunks.into(),
            classification,
        }
    }

    #[must_use]
    pub fn display_list(&self) -> &DisplayList {
        &self.display_list
    }

    #[must_use]
    pub fn shared_display_list(&self) -> Arc<DisplayList> {
        Arc::clone(&self.display_list)
    }

    #[must_use]
    pub fn chunks(&self) -> &[PaintChunk] {
        &self.chunks
    }

    #[must_use]
    pub const fn classification(&self) -> PaintSceneClassification {
        self.classification
    }

    #[must_use]
    pub const fn is_tile_safe(&self) -> bool {
        matches!(self.classification, PaintSceneClassification::TileSafe)
    }

    /// Builds conservative damage from `previous` to this scene.
    ///
    /// The returned partial damage is valid only with a retained frame built
    /// from `previous`. Scenes with stateful, unsupported, or mixed-coordinate
    /// commands always request a complete raster.
    #[must_use]
    pub fn damage_from(&self, previous: &Self) -> PaintDamage {
        let diff = self.display_list.diff(previous.display_list());
        if diff.full_repaint
            || !self.is_tile_safe()
            || !previous.is_tile_safe()
            || self.chunks.is_empty()
            || previous.chunks.is_empty()
            || self.tile_coordinate_space() != previous.tile_coordinate_space()
        {
            return PaintDamage::full(previous, self);
        }

        PaintDamage::partial(previous, self, diff.dirty_rects)
    }

    /// Returns viewport-relative tiles intersecting `damage`.
    ///
    /// Returning `None` means callers must use a full-raster fallback. This
    /// keeps tile scheduling conservative when scene state cannot be replayed
    /// independently.
    #[must_use]
    pub fn tiles_for_damage(
        &self,
        damage: &PaintDamage,
        viewport_origin: PhysicalPoint,
    ) -> Option<Vec<PaintTile>> {
        if !self.is_tile_safe() || damage.is_full_repaint() {
            return None;
        }
        if damage.is_empty() {
            return Some(Vec::new());
        }

        let coordinate_space = self.tile_coordinate_space()?;
        let rect = damage.viewport_bounds(self.viewport(), viewport_origin, coordinate_space)?;
        Some(tiles_covering(
            rect,
            self.viewport(),
            DEFAULT_PAINT_TILE_SIZE,
        ))
    }

    #[must_use]
    pub fn revision(&self) -> DomRevision {
        self.display_list.dom_revision
    }

    #[must_use]
    pub fn viewport(&self) -> PhysicalSize {
        self.display_list.viewport
    }

    pub(crate) fn tile_coordinate_space(&self) -> Option<PaintCoordinateSpace> {
        match self.chunks.first()?.classification {
            PaintChunkClassification::TileSafe { coordinate_space } if self.is_tile_safe() => {
                Some(coordinate_space)
            }
            PaintChunkClassification::TileSafe { .. }
            | PaintChunkClassification::FullRepaintRequired(_) => None,
        }
    }
}

/// Damage derived from two immutable paint scenes.
#[derive(Clone, Debug, PartialEq)]
pub struct PaintDamage {
    from_list: Arc<DisplayList>,
    to_list: Arc<DisplayList>,
    from_revision: DomRevision,
    to_revision: DomRevision,
    rects: Vec<PhysicalRect>,
    full_repaint: bool,
}

impl PaintDamage {
    #[must_use]
    pub fn is_full_repaint(&self) -> bool {
        self.full_repaint
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.full_repaint && self.rects.is_empty()
    }

    #[must_use]
    pub fn rects(&self) -> &[PhysicalRect] {
        &self.rects
    }

    #[must_use]
    pub const fn from_revision(&self) -> DomRevision {
        self.from_revision
    }

    #[must_use]
    pub const fn to_revision(&self) -> DomRevision {
        self.to_revision
    }

    fn full(previous: &PaintScene, current: &PaintScene) -> Self {
        Self {
            from_list: previous.shared_display_list(),
            to_list: current.shared_display_list(),
            from_revision: previous.revision(),
            to_revision: current.revision(),
            rects: Vec::new(),
            full_repaint: true,
        }
    }

    fn partial(previous: &PaintScene, current: &PaintScene, rects: Vec<PhysicalRect>) -> Self {
        Self {
            from_list: previous.shared_display_list(),
            to_list: current.shared_display_list(),
            from_revision: previous.revision(),
            to_revision: current.revision(),
            rects: merge_rects(rects),
            full_repaint: false,
        }
    }

    pub(crate) fn matches_transition(&self, previous: &PaintScene, current: &PaintScene) -> bool {
        !self.full_repaint
            && self.from_revision == previous.revision()
            && self.to_revision == current.revision()
            && Arc::ptr_eq(&self.from_list, &previous.display_list)
            && Arc::ptr_eq(&self.to_list, &current.display_list)
    }

    pub(crate) fn viewport_bounds(
        &self,
        viewport: PhysicalSize,
        viewport_origin: PhysicalPoint,
        coordinate_space: PaintCoordinateSpace,
    ) -> Option<PhysicalRect> {
        let bounds = self.rects.iter().copied().fold(None, union_rect);
        let bounds = bounds?;
        let offset = match coordinate_space {
            PaintCoordinateSpace::Document => PhysicalPoint {
                x: -finite_non_negative(viewport_origin.x),
                y: -finite_non_negative(viewport_origin.y),
            },
            PaintCoordinateSpace::Viewport => PhysicalPoint::default(),
        };
        let translated = PhysicalRect::new(
            bounds.origin.x + offset.x,
            bounds.origin.y + offset.y,
            bounds.size.width,
            bounds.size.height,
        );
        intersect_rect(translated, viewport_rect(viewport))
    }
}

/// A completed frame that may be reused by a matching damage request.
#[derive(Clone, Debug, PartialEq)]
pub struct RetainedFrame {
    scene: PaintScene,
    viewport_origin: PhysicalPoint,
    background: Color,
    surface: Surface,
}

impl RetainedFrame {
    #[must_use]
    pub fn new(
        scene: PaintScene,
        viewport_origin: PhysicalPoint,
        background: Color,
        surface: Surface,
    ) -> Self {
        Self {
            scene,
            viewport_origin,
            background,
            surface,
        }
    }

    #[must_use]
    pub fn scene(&self) -> &PaintScene {
        &self.scene
    }

    #[must_use]
    pub const fn viewport_origin(&self) -> PhysicalPoint {
        self.viewport_origin
    }

    #[must_use]
    pub const fn background(&self) -> Color {
        self.background
    }

    #[must_use]
    pub fn surface(&self) -> &Surface {
        &self.surface
    }

    pub(crate) fn is_compatible_with(
        &self,
        scene: &PaintScene,
        viewport_origin: PhysicalPoint,
        background: Color,
    ) -> bool {
        self.background == background
            && same_point(self.viewport_origin, viewport_origin)
            && self.surface.width() == ceil_to_u32(scene.viewport().width)
            && self.surface.height() == ceil_to_u32(scene.viewport().height)
    }
}

fn classify_item(item: &DisplayItem) -> PaintChunkClassification {
    match &item.command {
        DisplayCommand::SolidRect { rect, .. } if item.bounds == *rect => {
            PaintChunkClassification::TileSafe {
                coordinate_space: item.coordinate_space,
            }
        }
        DisplayCommand::TextDecoration(decoration) if item.bounds == decoration.rect => {
            PaintChunkClassification::TileSafe {
                coordinate_space: item.coordinate_space,
            }
        }
        DisplayCommand::PushClip(_)
        | DisplayCommand::PopClip
        | DisplayCommand::PushTransform(_)
        | DisplayCommand::PopTransform
        | DisplayCommand::PushStackingContext(_)
        | DisplayCommand::PopStackingContext => {
            PaintChunkClassification::FullRepaintRequired(PaintFallbackReason::StatefulCommand)
        }
        DisplayCommand::SolidRect { .. }
        | DisplayCommand::TextDecoration(_)
        | DisplayCommand::Border(_)
        | DisplayCommand::BoxShadow(_)
        | DisplayCommand::GlyphRun(_)
        | DisplayCommand::Image(_)
        | DisplayCommand::LinearGradient(_)
        | DisplayCommand::RadialGradient(_)
        | DisplayCommand::Canvas { .. } => {
            PaintChunkClassification::FullRepaintRequired(PaintFallbackReason::UnsupportedCommand)
        }
    }
}

fn classify_scene(chunks: &[PaintChunk]) -> PaintSceneClassification {
    let mut coordinate_space = None;
    for chunk in chunks {
        match chunk.classification {
            PaintChunkClassification::FullRepaintRequired(reason) => {
                return PaintSceneClassification::FullRepaintRequired(reason);
            }
            PaintChunkClassification::TileSafe {
                coordinate_space: item_space,
            } => match coordinate_space {
                Some(previous) if previous != item_space => {
                    return PaintSceneClassification::FullRepaintRequired(
                        PaintFallbackReason::MixedCoordinateSpaces,
                    );
                }
                Some(_) => {}
                None => coordinate_space = Some(item_space),
            },
        }
    }
    PaintSceneClassification::TileSafe
}

fn merge_rects(mut rects: Vec<PhysicalRect>) -> Vec<PhysicalRect> {
    rects.retain(is_non_empty_finite);
    let mut index = 0;
    while index < rects.len() {
        let mut candidate = rects[index];
        let mut other = index + 1;
        while other < rects.len() {
            if touches_or_overlaps(candidate, rects[other]) {
                candidate = union_rect(Some(candidate), rects[other]).expect("finite rectangles");
                rects.swap_remove(other);
            } else {
                other += 1;
            }
        }
        rects[index] = candidate;
        index += 1;
    }
    rects
}

#[allow(
    clippy::cast_precision_loss,
    reason = "physical paint coordinates are represented as f32 throughout the layout API"
)]
fn tiles_covering(rect: PhysicalRect, viewport: PhysicalSize, tile_size: u32) -> Vec<PaintTile> {
    let tile_size = tile_size.max(1);
    let viewport = viewport_rect(viewport);
    let Some(rect) = intersect_rect(rect, viewport) else {
        return Vec::new();
    };

    let tile_size_f32 = tile_size as f32;
    let first_column = floor_to_u32(rect.origin.x / tile_size_f32);
    let first_row = floor_to_u32(rect.origin.y / tile_size_f32);
    let last_column = ceil_to_u32(rect.right() / tile_size_f32);
    let last_row = ceil_to_u32(rect.bottom() / tile_size_f32);
    let mut tiles = Vec::new();
    for row in first_row..last_row {
        for column in first_column..last_column {
            let bounds = PhysicalRect::new(
                column as f32 * tile_size_f32,
                row as f32 * tile_size_f32,
                tile_size_f32,
                tile_size_f32,
            );
            if let Some(bounds) = intersect_rect(bounds, viewport) {
                tiles.push(PaintTile {
                    column,
                    row,
                    bounds,
                });
            }
        }
    }
    tiles
}

fn viewport_rect(viewport: PhysicalSize) -> PhysicalRect {
    PhysicalRect::new(
        0.0,
        0.0,
        finite_non_negative(viewport.width),
        finite_non_negative(viewport.height),
    )
}

fn union_rect(current: Option<PhysicalRect>, next: PhysicalRect) -> Option<PhysicalRect> {
    if !is_non_empty_finite(&next) {
        return current;
    }
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
}

fn intersect_rect(first: PhysicalRect, second: PhysicalRect) -> Option<PhysicalRect> {
    let left = first.origin.x.max(second.origin.x);
    let top = first.origin.y.max(second.origin.y);
    let right = first.right().min(second.right());
    let bottom = first.bottom().min(second.bottom());
    (right > left && bottom > top).then(|| PhysicalRect::new(left, top, right - left, bottom - top))
}

fn touches_or_overlaps(first: PhysicalRect, second: PhysicalRect) -> bool {
    first.origin.x <= second.right()
        && first.right() >= second.origin.x
        && first.origin.y <= second.bottom()
        && first.bottom() >= second.origin.y
}

fn same_point(first: PhysicalPoint, second: PhysicalPoint) -> bool {
    first.x.to_bits() == second.x.to_bits() && first.y.to_bits() == second.y.to_bits()
}

fn is_non_empty_finite(rect: &PhysicalRect) -> bool {
    rect.origin.x.is_finite()
        && rect.origin.y.is_finite()
        && rect.size.width.is_finite()
        && rect.size.height.is_finite()
        && rect.size.width > 0.0
        && rect.size.height > 0.0
}

fn finite_non_negative(value: f32) -> f32 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "paint tiles use clamped physical raster coordinates"
)]
fn ceil_to_u32(value: f32) -> u32 {
    value.ceil().max(0.0) as u32
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "paint tiles use clamped physical raster coordinates"
)]
fn floor_to_u32(value: f32) -> u32 {
    value.floor().max(0.0) as u32
}

#[cfg(test)]
mod tests {
    use crate::dom::Dom;
    use crate::layout::{FragmentId, PhysicalRect, PhysicalSize};
    use crate::paint::{
        Color, DisplayCommand, DisplayItem, DisplayItemId, DisplayList, PaintCoordinateSpace,
        PaintPhase,
    };

    use super::{PaintFallbackReason, PaintScene, PaintSceneClassification};

    fn item(
        ordinal: u32,
        rect: PhysicalRect,
        coordinate_space: PaintCoordinateSpace,
        command: DisplayCommand,
    ) -> DisplayItem {
        DisplayItem {
            id: DisplayItemId {
                source: None,
                fragment_hint: ordinal,
                phase: PaintPhase::Background,
                ordinal,
            },
            fragment: FragmentId::from_index(0),
            source: None,
            bounds: rect,
            coordinate_space,
            command,
        }
    }

    fn list(items: Vec<DisplayItem>) -> DisplayList {
        DisplayList {
            dom_revision: Dom::new().revision(),
            viewport: PhysicalSize {
                width: 256.0,
                height: 256.0,
            },
            items,
        }
    }

    #[test]
    fn stateful_commands_force_the_conservative_path() {
        let scene = PaintScene::from_display_list(list(vec![item(
            0,
            PhysicalRect::new(0.0, 0.0, 20.0, 20.0),
            PaintCoordinateSpace::Document,
            DisplayCommand::PopClip,
        )]));

        assert_eq!(
            scene.classification(),
            PaintSceneClassification::FullRepaintRequired(PaintFallbackReason::StatefulCommand)
        );
    }

    #[test]
    fn mixed_coordinate_spaces_force_the_conservative_path() {
        let rect = PhysicalRect::new(0.0, 0.0, 20.0, 20.0);
        let scene = PaintScene::from_display_list(list(vec![
            item(
                0,
                rect,
                PaintCoordinateSpace::Document,
                DisplayCommand::SolidRect {
                    rect,
                    color: Color::BLACK,
                },
            ),
            item(
                1,
                rect,
                PaintCoordinateSpace::Viewport,
                DisplayCommand::SolidRect {
                    rect,
                    color: Color::WHITE,
                },
            ),
        ]));

        assert_eq!(
            scene.classification(),
            PaintSceneClassification::FullRepaintRequired(
                PaintFallbackReason::MixedCoordinateSpaces
            )
        );
    }

    #[test]
    fn direct_scene_damage_selects_intersecting_tiles() {
        let rect = PhysicalRect::new(129.0, 129.0, 10.0, 10.0);
        let previous = PaintScene::from_display_list(list(vec![item(
            0,
            rect,
            PaintCoordinateSpace::Document,
            DisplayCommand::SolidRect {
                rect,
                color: Color::BLACK,
            },
        )]));
        let current = PaintScene::from_display_list(list(vec![item(
            0,
            rect,
            PaintCoordinateSpace::Document,
            DisplayCommand::SolidRect {
                rect,
                color: Color::WHITE,
            },
        )]));

        let damage = current.damage_from(&previous);
        let tiles = current
            .tiles_for_damage(&damage, crate::layout::PhysicalPoint::default())
            .expect("direct scene is tile-safe");

        assert_eq!(tiles.len(), 1);
        assert_eq!(tiles[0].column, 1);
        assert_eq!(tiles[0].row, 1);
    }

    #[test]
    fn identical_safe_scenes_produce_empty_partial_damage() {
        let rect = PhysicalRect::new(1.0, 1.0, 10.0, 10.0);
        let make_scene = || {
            PaintScene::from_display_list(list(vec![item(
                0,
                rect,
                PaintCoordinateSpace::Document,
                DisplayCommand::SolidRect {
                    rect,
                    color: Color::BLACK,
                },
            )]))
        };
        let previous = make_scene();
        let current = make_scene();

        let damage = current.damage_from(&previous);

        assert!(!damage.is_full_repaint());
        assert!(damage.is_empty());
        assert_eq!(
            current.tiles_for_damage(&damage, crate::layout::PhysicalPoint::default()),
            Some(Vec::new())
        );
    }
}
