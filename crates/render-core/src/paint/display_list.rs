//! Immutable retained display-list model and incremental diffing.

use std::collections::BTreeMap;

use crate::css::computed::{ComputedStyle, ComputedValue};
use crate::css::properties::{
    BorderStyle, CssColor, Display, LengthPercentage, LengthResolutionContext, ObjectFit, Overflow,
    Position, Size, TransformFunction, TransformList, TransformOrigin, TypedPropertyValue,
    Visibility, parse_typed_property,
};
use crate::dom::{DomRevision, NodeId};
use crate::image::ImageResources;
use crate::layout::{
    EdgeSizes, FormattingNodeId, FormattingTree, Fragment, FragmentId, FragmentKind, FragmentTree,
    PhysicalPoint, PhysicalRect, PhysicalSize,
};

use super::color::{Color, SystemPalette};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform2D {
    pub scale_x: f32,
    pub skew_x: f32,
    pub skew_y: f32,
    pub scale_y: f32,
    pub translate_x: f32,
    pub translate_y: f32,
}

impl Default for Transform2D {
    fn default() -> Self {
        Self {
            scale_x: 1.0,
            skew_x: 0.0,
            skew_y: 0.0,
            scale_y: 1.0,
            translate_x: 0.0,
            translate_y: 0.0,
        }
    }
}

impl Transform2D {
    /// Maps a point through the affine matrix
    /// `[[scale_x, skew_x, translate_x], [skew_y, scale_y, translate_y]]`.
    #[must_use]
    pub fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        (
            self.scale_x * x + self.skew_x * y + self.translate_x,
            self.skew_y * x + self.scale_y * y + self.translate_y,
        )
    }

    #[must_use]
    #[allow(clippy::float_cmp)] // identity is an exact bit-pattern property
    pub fn is_identity(&self) -> bool {
        self.scale_x == 1.0
            && self.skew_x == 0.0
            && self.skew_y == 0.0
            && self.scale_y == 1.0
            && self.translate_x == 0.0
            && self.translate_y == 0.0
    }

    /// True when the matrix reduces to a pure translation, which the
    /// rasterizer can apply as a paint offset without an offscreen pass.
    #[must_use]
    #[allow(clippy::float_cmp)] // exact unit coefficients, not measured values
    pub fn is_translation(&self) -> bool {
        self.scale_x == 1.0 && self.skew_x == 0.0 && self.skew_y == 0.0 && self.scale_y == 1.0
    }

    #[must_use]
    pub fn translation(&self) -> (f32, f32) {
        (self.translate_x, self.translate_y)
    }

    /// Composes two matrices with `applied_first` mapping points before
    /// `self` (i.e. `self �?applied_first`).
    #[must_use]
    pub fn then(&self, applied_first: &Self) -> Self {
        Self {
            scale_x: self.scale_x * applied_first.scale_x + self.skew_x * applied_first.skew_y,
            skew_x: self.scale_x * applied_first.skew_x + self.skew_x * applied_first.scale_y,
            skew_y: self.skew_y * applied_first.scale_x + self.scale_y * applied_first.skew_y,
            scale_y: self.skew_y * applied_first.skew_x + self.scale_y * applied_first.scale_y,
            translate_x: self.scale_x * applied_first.translate_x
                + self.skew_x * applied_first.translate_y
                + self.translate_x,
            translate_y: self.skew_y * applied_first.translate_x
                + self.scale_y * applied_first.translate_y
                + self.translate_y,
        }
    }

    /// Inverts the affine matrix; returns `None` when it is singular.
    #[must_use]
    pub fn inverse(&self) -> Option<Self> {
        let determinant = self.scale_x * self.scale_y - self.skew_x * self.skew_y;
        if determinant == 0.0 {
            return None;
        }
        let inverse = 1.0 / determinant;
        Some(Self {
            scale_x: self.scale_y * inverse,
            skew_x: -self.skew_x * inverse,
            skew_y: -self.skew_y * inverse,
            scale_y: self.scale_x * inverse,
            translate_x: (self.skew_x * self.translate_y - self.scale_y * self.translate_x)
                * inverse,
            translate_y: (self.skew_y * self.translate_x - self.scale_x * self.translate_y)
                * inverse,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CornerRadii {
    pub top_left: f32,
    pub top_right: f32,
    pub bottom_right: f32,
    pub bottom_left: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlendMode {
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompositingReason {
    Root,
    Opacity,
    Transform,
    Isolation,
    Filter,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StackingContext {
    pub opacity: f32,
    pub transform: Transform2D,
    pub blend_mode: BlendMode,
    pub isolated: bool,
    pub reason: CompositingReason,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ClipShape {
    Rect(PhysicalRect),
    RoundedRect {
        rect: PhysicalRect,
        radii: CornerRadii,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BorderPaint {
    pub rect: PhysicalRect,
    pub widths: EdgeSizes,
    pub colors: [Color; 4],
    pub styles: [BorderStyle; 4],
    pub radii: CornerRadii,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoxShadowPaint {
    pub rect: PhysicalRect,
    pub offset: PhysicalPoint,
    pub blur_radius: f32,
    pub spread_radius: f32,
    pub color: Color,
    pub inset: bool,
    pub radii: CornerRadii,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FontInstanceId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GlyphId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlyphInstance {
    pub glyph: GlyphId,
    pub position: PhysicalPoint,
    pub advance: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GlyphRun {
    pub font: FontInstanceId,
    pub font_size: f32,
    pub color: Color,
    pub glyphs: Vec<GlyphInstance>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextDecorationLine {
    Underline,
    Overline,
    LineThrough,
}

/// Stroke pattern of a text decoration line (CSS Text Decoration Level 3 §3.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextDecorationStyle {
    Solid,
    Double,
    Dotted,
    Dashed,
    Wavy,
}

/// One text decoration line over a run of text.
///
/// Geometry is resolved where the style is known, not per pixel: `rect` is the
/// run's horizontal extent with `rect.origin.y` set to the line's top edge in
/// document space and `rect.size.height` set to the stroke thickness. The
/// rasterizer turns `line`, `style`, and `thickness` into the individual stroke
/// rectangles, so a dashed or wavy run stays a single retained item.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextDecoration {
    pub rect: PhysicalRect,
    pub color: Color,
    pub line: TextDecorationLine,
    pub style: TextDecorationStyle,
    pub thickness: f32,
}

/// Parameters of one `text-shadow` layer.
///
/// A shadow item is emitted immediately before the glyph run it describes and
/// names it through `run`, so the rasterizer reuses that run's glyph geometry
/// instead of storing a second copy of it. The shadow paints behind the
/// glyphs (CSS Text Decoration Level 3 §4.2).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextShadowPaint {
    /// `DisplayItemId::fragment_hint` of the glyph run this shadow belongs to.
    pub run: u32,
    pub font: FontInstanceId,
    pub font_size: f32,
    pub offset: PhysicalPoint,
    /// Blur radius; zero paints an unblurred copy of the glyph outlines.
    pub blur_radius: f32,
    pub color: Color,
}

/// Bullet shape of a list marker (CSS Lists Level 3 §3.1). Ordered markers are
/// text and paint as an ordinary glyph run instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListMarkerShape {
    Square,
    Disc,
    Circle,
}

/// The bullet painted beside a `display: list-item` box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ListMarkerPaint {
    pub rect: PhysicalRect,
    pub color: Color,
    pub shape: ListMarkerShape,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GradientStop {
    pub offset: f32,
    pub color: Color,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LinearGradient {
    pub rect: PhysicalRect,
    pub start: PhysicalPoint,
    pub end: PhysicalPoint,
    pub stops: Vec<GradientStop>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RadialGradient {
    pub rect: PhysicalRect,
    pub center: PhysicalPoint,
    pub radius_x: f32,
    pub radius_y: f32,
    pub stops: Vec<GradientStop>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ImageResourceId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImagePaint {
    pub resource: ImageResourceId,
    pub destination: PhysicalRect,
    pub source: PhysicalRect,
    pub interpolate: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CanvasResourceId(pub u64);

#[derive(Clone, Debug, PartialEq)]
pub enum DisplayCommand {
    SolidRect {
        rect: PhysicalRect,
        color: Color,
    },
    Border(BorderPaint),
    BoxShadow(BoxShadowPaint),
    PushClip(ClipShape),
    PopClip,
    PushTransform(Transform2D),
    PopTransform,
    GlyphRun(GlyphRun),
    TextDecoration(TextDecoration),
    TextShadow(TextShadowPaint),
    ListMarker(ListMarkerPaint),
    Image(ImagePaint),
    LinearGradient(LinearGradient),
    RadialGradient(RadialGradient),
    Canvas {
        resource: CanvasResourceId,
        destination: PhysicalRect,
    },
    PushStackingContext(StackingContext),
    PopStackingContext,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PaintPhase {
    StackingContext,
    BoxShadow,
    Background,
    Border,
    Content,
    TextDecoration,
    Outline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DisplayItemId {
    pub source: Option<NodeId>,
    pub fragment_hint: u32,
    pub phase: PaintPhase,
    pub ordinal: u32,
}

/// Coordinate space used when a retained item is composited into a viewport.
/// Keeping this on each item lets scrolling translate document content without
/// translating viewport-attached content. Sticky positioning can later switch
/// an item's resolved space/translation without changing raster surface size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PaintCoordinateSpace {
    #[default]
    Document,
    Viewport,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DisplayItem {
    pub id: DisplayItemId,
    pub fragment: FragmentId,
    pub source: Option<NodeId>,
    pub bounds: PhysicalRect,
    pub coordinate_space: PaintCoordinateSpace,
    pub command: DisplayCommand,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DisplayList {
    pub dom_revision: DomRevision,
    pub viewport: PhysicalSize,
    pub(crate) items: Vec<DisplayItem>,
}

impl DisplayList {
    #[must_use]
    pub fn items(&self) -> &[DisplayItem] {
        &self.items
    }

    #[must_use]
    pub fn diff(&self, previous: &Self) -> DisplayListDiff {
        if self.viewport != previous.viewport {
            return DisplayListDiff {
                from_revision: previous.dom_revision,
                to_revision: self.dom_revision,
                inserted: self.items.iter().map(|item| item.id).collect(),
                removed: previous.items.iter().map(|item| item.id).collect(),
                changed: Vec::new(),
                moved: Vec::new(),
                dirty_rects: vec![PhysicalRect::new(
                    0.0,
                    0.0,
                    self.viewport.width,
                    self.viewport.height,
                )],
                full_repaint: true,
            };
        }

        let old = previous
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| (item.id, (index, item)))
            .collect::<BTreeMap<_, _>>();
        let new = self
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| (item.id, (index, item)))
            .collect::<BTreeMap<_, _>>();
        let mut inserted = Vec::new();
        let mut removed = Vec::new();
        let mut changed = Vec::new();
        let mut moved = Vec::new();
        let mut dirty_rects = Vec::new();

        for (id, (index, item)) in &new {
            match old.get(id) {
                None => {
                    inserted.push(*id);
                    dirty_rects.push(item.bounds);
                }
                Some((old_index, old_item)) => {
                    if *item != *old_item {
                        changed.push(*id);
                        dirty_rects.push(old_item.bounds);
                        dirty_rects.push(item.bounds);
                    }
                    if index != old_index {
                        moved.push(*id);
                        dirty_rects.push(old_item.bounds);
                        dirty_rects.push(item.bounds);
                    }
                }
            }
        }
        for (id, (_, item)) in &old {
            if !new.contains_key(id) {
                removed.push(*id);
                dirty_rects.push(item.bounds);
            }
        }
        DisplayListDiff {
            from_revision: previous.dom_revision,
            to_revision: self.dom_revision,
            inserted,
            removed,
            changed,
            moved,
            dirty_rects,
            full_repaint: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DisplayListDiff {
    pub from_revision: DomRevision,
    pub to_revision: DomRevision,
    pub inserted: Vec<DisplayItemId>,
    pub removed: Vec<DisplayItemId>,
    pub changed: Vec<DisplayItemId>,
    pub moved: Vec<DisplayItemId>,
    pub dirty_rects: Vec<PhysicalRect>,
    pub full_repaint: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplayListBuilderLimits {
    pub max_items: usize,
    pub max_glyphs: usize,
}

impl Default for DisplayListBuilderLimits {
    fn default() -> Self {
        Self {
            max_items: 4_000_000,
            max_glyphs: 64 * 1_024 * 1_024,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct DisplayListBuilderOptions {
    pub palette: SystemPalette,
    pub limits: DisplayListBuilderLimits,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayListDiagnosticCode {
    ItemLimit,
    GlyphLimit,
    MissingFragment,
    MissingStyle,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplayListDiagnostic {
    pub node: Option<NodeId>,
    pub code: DisplayListDiagnosticCode,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DisplayListBuildOutput {
    pub list: DisplayList,
    pub diagnostics: Vec<DisplayListDiagnostic>,
}

pub trait TextShaper: Sync {
    fn shape(&self, text: &str, font_size: f32, origin: PhysicalPoint, color: Color) -> GlyphRun;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ReferenceTextShaper;

impl TextShaper for ReferenceTextShaper {
    fn shape(&self, text: &str, font_size: f32, origin: PhysicalPoint, color: Color) -> GlyphRun {
        let mut x = origin.x;
        let glyphs = text
            .chars()
            .map(|character| {
                let advance = if is_wide_character(character) {
                    font_size
                } else if character.is_whitespace() {
                    font_size * 0.25
                } else {
                    font_size * 0.5
                };
                let glyph = GlyphInstance {
                    glyph: GlyphId(character as u32),
                    position: PhysicalPoint { x, y: origin.y },
                    advance,
                };
                x += advance;
                glyph
            })
            .collect();
        GlyphRun {
            font: FontInstanceId(0),
            font_size,
            color,
            glyphs,
        }
    }
}

#[must_use]
pub fn build_display_list(
    fragments: &FragmentTree,
    formatting: &FormattingTree,
    styles: &BTreeMap<NodeId, ComputedStyle>,
    options: DisplayListBuilderOptions,
    shaper: &dyn TextShaper,
) -> DisplayListBuildOutput {
    build_display_list_with_images(fragments, formatting, styles, options, shaper, None)
}

#[must_use]
pub fn build_display_list_with_images(
    fragments: &FragmentTree,
    formatting: &FormattingTree,
    styles: &BTreeMap<NodeId, ComputedStyle>,
    options: DisplayListBuilderOptions,
    shaper: &dyn TextShaper,
    images: Option<&ImageResources>,
) -> DisplayListBuildOutput {
    let parents = if images.is_some() {
        let mut parents = BTreeMap::<FragmentId, FragmentId>::new();
        for fragment in fragments.iter() {
            for child in &fragment.children {
                parents.insert(*child, fragment.id);
            }
        }
        parents
    } else {
        BTreeMap::new()
    };
    // Text decoration propagation and list-marker resolution both walk
    // formatting ancestors, and `FormattingTree` only stores child links. A
    // flat arena-indexed parent table answers those walks in one step.
    let mut formatting_parents = vec![None; formatting.iter().count()];
    for node in formatting.iter() {
        for child in &node.children {
            if let Some(slot) = formatting_parents.get_mut(child.as_u32() as usize) {
                *slot = Some(node.id);
            }
        }
    }
    let mut builder = Builder {
        fragments,
        formatting,
        styles,
        options,
        shaper,
        images,
        parents,
        formatting_parents,
        items: Vec::new(),
        diagnostics: Vec::new(),
        ordinals: BTreeMap::new(),
        glyphs: 0,
        limit_reported: false,
    };
    builder.paint_fragment(fragments.root(), PaintCoordinateSpace::Document);
    DisplayListBuildOutput {
        list: DisplayList {
            dom_revision: fragments.dom_revision,
            viewport: fragments.viewport,
            items: builder.items,
        },
        diagnostics: builder.diagnostics,
    }
}

struct Builder<'a> {
    fragments: &'a FragmentTree,
    formatting: &'a FormattingTree,
    styles: &'a BTreeMap<NodeId, ComputedStyle>,
    options: DisplayListBuilderOptions,
    shaper: &'a dyn TextShaper,
    images: Option<&'a ImageResources>,
    parents: BTreeMap<FragmentId, FragmentId>,
    formatting_parents: Vec<Option<FormattingNodeId>>,
    items: Vec<DisplayItem>,
    diagnostics: Vec<DisplayListDiagnostic>,
    ordinals: BTreeMap<(Option<NodeId>, PaintPhase), u32>,
    glyphs: usize,
    limit_reported: bool,
}

impl Builder<'_> {
    fn paint_fragment(&mut self, id: FragmentId, parent_space: PaintCoordinateSpace) {
        let Some(fragment) = self.fragments.get(id).cloned() else {
            self.diagnostics.push(DisplayListDiagnostic {
                node: None,
                code: DisplayListDiagnosticCode::MissingFragment,
                message: "display list referenced an unknown fragment".to_owned(),
            });
            return;
        };
        let style = self.style_for(&fragment).cloned();
        let coordinate_space = fragment_coordinate_space(style.as_ref(), parent_space);
        let current_color = self.current_color(style.as_ref());
        let opacity = fragment_opacity(style.as_ref());
        // Transforms are anchored to the border box: percentages in the
        // transform list and the default transform-origin resolve against
        // it (CSS Transforms Level 1 §4). Text fragments have no border
        // box, so their fragment rect stands in.
        let border_rect = match &fragment.kind {
            FragmentKind::Box(geometry) => geometry.border_rect(),
            FragmentKind::Text(_) => fragment.rect,
        };
        let transform = self.fragment_transform(style.as_ref(), border_rect);
        let hidden = matches!(
            style.as_ref().and_then(|style| style.typed("visibility")),
            Some(TypedPropertyValue::Visibility(
                Visibility::Hidden | Visibility::Collapse
            ))
        );
        // A transform groups the whole fragment �?background, shadow, text,
        // image, overflow clip and children �?behind one affine matrix the
        // same way opacity groups it behind an alpha. When both apply, a
        // single stacking context carries them so rasterization only pays
        // for one offscreen group.
        let stacking_context = fragment_stacking_context(opacity, transform);
        if let Some(context) = stacking_context {
            self.push(
                &fragment,
                PaintPhase::StackingContext,
                fragment.rect,
                coordinate_space,
                DisplayCommand::PushStackingContext(context),
            );
        }

        if !hidden {
            match &fragment.kind {
                FragmentKind::Box(geometry) => {
                    self.paint_box(
                        &fragment,
                        geometry,
                        style.as_ref(),
                        current_color,
                        coordinate_space,
                    );
                    self.paint_image(&fragment, geometry, style.as_ref(), coordinate_space);
                }
                FragmentKind::Text(text) => {
                    self.paint_text(
                        &fragment,
                        text,
                        style.as_ref(),
                        current_color,
                        coordinate_space,
                    );
                }
            }
        }
        let overflow_clip = match (&fragment.kind, style.as_ref()) {
            (FragmentKind::Box(geometry), Some(style)) => {
                let mut clip_geometry = (*geometry).clone();
                clip_geometry.content_rect =
                    self.recovered_content_rect(&fragment, geometry, Some(style));
                if let Some(shape) = overflow_clip_shape(&clip_geometry, style) {
                    let rect = clip_shape_rect(shape);
                    self.push(
                        &fragment,
                        PaintPhase::Content,
                        rect,
                        coordinate_space,
                        DisplayCommand::PushClip(shape),
                    );
                    Some(())
                } else {
                    None
                }
            }
            _ => None,
        };
        for child in &fragment.children {
            self.paint_fragment(*child, coordinate_space);
        }
        if overflow_clip.is_some() {
            self.push(
                &fragment,
                PaintPhase::Content,
                fragment.rect,
                coordinate_space,
                DisplayCommand::PopClip,
            );
        }
        if stacking_context.is_some() {
            self.push(
                &fragment,
                PaintPhase::StackingContext,
                fragment.rect,
                coordinate_space,
                DisplayCommand::PopStackingContext,
            );
        }
    }

    /// Resolves the fragment's `transform` property into the affine matrix
    /// paint applies around the border box (CSS Transforms Level 1 §4–�?).
    ///
    /// Percentages inside `translate()` resolve against the border-box width
    /// or height, the same per-axis basis CSS uses. Font-relative units use
    /// the element's computed font size (stored as absolute pixels by the
    /// computed-value stage) with the 16px engine default as fallback;
    /// layout's already-resolved used lengths are not visible to paint, so
    /// the list is re-resolved from the style here. Returns `None` when no
    /// transform applies or the result is the identity, keeping the command
    /// stream of untransformed fragments unchanged.
    fn fragment_transform(
        &self,
        style: Option<&ComputedStyle>,
        border_rect: PhysicalRect,
    ) -> Option<Transform2D> {
        let style = style?;
        let Some(TypedPropertyValue::Transform(TransformList(functions))) =
            style.typed("transform")
        else {
            return None;
        };
        if functions.is_empty() {
            return None;
        }
        let font_size = style_font_size(style);
        let viewport = self.fragments.viewport;
        // The first function is the outermost mapping: M = F1 �?F2 �?�?�?Fn.
        let mut matrix = Transform2D::default();
        for function in functions {
            matrix = matrix.then(&transform_function_matrix(
                function,
                border_rect,
                font_size,
                viewport,
            ));
        }
        // transform-origin re-bases the list around a fixed point:
        // T(origin) �?M �?T(−origin). It defaults to 50% 50% of the box.
        let origin = style
            .typed("transform-origin")
            .and_then(|value| match value {
                TypedPropertyValue::TransformOrigin(origin) => Some(origin.clone()),
                _ => None,
            })
            .unwrap_or(TransformOrigin(
                LengthPercentage::Percentage(0.5),
                LengthPercentage::Percentage(0.5),
            ));
        let origin_x =
            resolve_transform_length(&origin.0, border_rect.size.width, font_size, viewport);
        let origin_y =
            resolve_transform_length(&origin.1, border_rect.size.height, font_size, viewport);
        let to_origin = Transform2D {
            translate_x: origin_x,
            translate_y: origin_y,
            ..Transform2D::default()
        };
        let from_origin = Transform2D {
            translate_x: -origin_x,
            translate_y: -origin_y,
            ..Transform2D::default()
        };
        matrix = to_origin.then(&matrix).then(&from_origin);
        if matrix.is_identity() {
            None
        } else {
            Some(matrix)
        }
    }

    fn current_color(&self, style: Option<&ComputedStyle>) -> Color {
        style.and_then(|style| typed_color(style, "color")).map_or(
            self.options.palette.canvas_text,
            |color| {
                self.options
                    .palette
                    .resolve(color, self.options.palette.canvas_text)
            },
        )
    }

    fn paint_box(
        &mut self,
        fragment: &Fragment,
        geometry: &crate::layout::BoxGeometry,
        style: Option<&ComputedStyle>,
        current_color: Color,
        coordinate_space: PaintCoordinateSpace,
    ) {
        let (background_rect, background_space) =
            self.background_rect(fragment, geometry, coordinate_space);
        let radii = corner_radii(style, background_rect);
        for shadow in box_shadows(
            style,
            geometry.border_rect(),
            radii,
            current_color,
            self.options.palette,
        ) {
            let bounds = shadow_bounds(&shadow);
            self.push(
                fragment,
                PaintPhase::BoxShadow,
                bounds,
                coordinate_space,
                DisplayCommand::BoxShadow(shadow),
            );
        }
        let rounded_background = has_corner_radius(radii);
        if rounded_background {
            self.push(
                fragment,
                PaintPhase::Background,
                background_rect,
                background_space,
                DisplayCommand::PushClip(ClipShape::RoundedRect {
                    rect: background_rect,
                    radii,
                }),
            );
        }
        if let Some(background) = style
            .and_then(|style| typed_color(style, "background-color"))
            .map(|color| self.options.palette.resolve(color, current_color))
            && background.alpha > 0
        {
            self.push(
                fragment,
                PaintPhase::Background,
                background_rect,
                background_space,
                DisplayCommand::SolidRect {
                    rect: background_rect,
                    color: background,
                },
            );
        }
        self.paint_background_image(fragment, geometry, style, current_color, coordinate_space);
        if rounded_background {
            self.push(
                fragment,
                PaintPhase::Background,
                background_rect,
                background_space,
                DisplayCommand::PopClip,
            );
        }
        let border = border_paint(style, geometry, current_color, self.options.palette);
        if border.widths.horizontal() > 0.0 || border.widths.vertical() > 0.0 {
            self.push(
                fragment,
                PaintPhase::Border,
                geometry.border_rect(),
                coordinate_space,
                DisplayCommand::Border(border),
            );
        }
        if let Some(style) = style {
            self.paint_list_marker(fragment, geometry, style, current_color, coordinate_space);
        }
    }

    #[allow(clippy::too_many_lines)]
    fn paint_background_image(
        &mut self,
        fragment: &Fragment,
        geometry: &crate::layout::BoxGeometry,
        style: Option<&ComputedStyle>,
        current_color: Color,
        coordinate_space: PaintCoordinateSpace,
    ) {
        let Some(style) = style else { return };
        let Some(TypedPropertyValue::BackgroundImage(snapshot)) = style.typed("background-image")
        else {
            return;
        };
        // The default background-origin is the padding box, while the
        // default background-clip is the border box. Keep those spaces
        // separate so transparent borders can reveal the background image.
        let positioning_area = geometry.padding_rect();
        let painting_area = geometry.border_rect();
        if positioning_area.size.width <= 0.0
            || positioning_area.size.height <= 0.0
            || painting_area.size.width <= 0.0
            || painting_area.size.height <= 0.0
        {
            return;
        }
        let gradient_layers = split_gradient_arguments(snapshot)
            .iter()
            .enumerate()
            .filter_map(|(index, layer)| {
                let area = if index == 0 {
                    positioning_area
                } else {
                    painting_area
                };
                parse_linear_gradient(layer, area, self.options.palette, current_color)
                    .map(|gradient| (index, gradient))
            })
            .collect::<Vec<_>>();
        if !gradient_layers.is_empty() {
            // Each layer is clipped to its own background-clip area. With
            // rounded corners those areas are rounded too, so a padding-box
            // layer cannot square off the border ring revealed by a
            // transparent border.
            let border_radii = corner_radii(Some(style), painting_area);
            let padding_radii = inset_corner_radii(
                border_radii,
                geometry.border.top,
                geometry.border.right,
                geometry.border.bottom,
                geometry.border.left,
            );
            let border_clip = background_clip_shape(painting_area, border_radii);
            let padding_clip = background_clip_shape(positioning_area, padding_radii);
            for (index, gradient) in gradient_layers.into_iter().rev() {
                let clip = if index == 0 {
                    padding_clip
                } else {
                    border_clip
                };
                self.push(
                    fragment,
                    PaintPhase::Background,
                    painting_area,
                    coordinate_space,
                    DisplayCommand::PushClip(clip),
                );
                self.push(
                    fragment,
                    PaintPhase::Background,
                    gradient.rect,
                    coordinate_space,
                    DisplayCommand::LinearGradient(gradient),
                );
                self.push(
                    fragment,
                    PaintPhase::Background,
                    painting_area,
                    coordinate_space,
                    DisplayCommand::PopClip,
                );
            }
            return;
        }
        let Some(loaded) = fragment.source.and_then(|node| {
            self.images
                .and_then(|images| images.get_css_background(node, snapshot))
        }) else {
            return;
        };
        let (width, height) = loaded.image.intrinsic_size();
        if width == 0 || height == 0 {
            return;
        }
        let size = style
            .typed("background-size")
            .and_then(|value| match value {
                TypedPropertyValue::BackgroundSize(value) => Some(value.as_str()),
                _ => None,
            })
            .unwrap_or("auto");
        let position = style
            .typed("background-position")
            .and_then(|value| match value {
                TypedPropertyValue::BackgroundPosition(value) => Some(value.as_str()),
                _ => None,
            })
            .unwrap_or("0% 0%");
        let repeat = style
            .typed("background-repeat")
            .and_then(|value| match value {
                TypedPropertyValue::BackgroundRepeat(value) => Some(value.as_str()),
                _ => None,
            })
            .unwrap_or("repeat");
        let intrinsic_width = image_dimension_to_f32(width);
        let intrinsic_height = image_dimension_to_f32(height);
        let (paint_width, paint_height, source) = match size {
            "cover" => {
                let scale = (positioning_area.size.width / intrinsic_width)
                    .max(positioning_area.size.height / intrinsic_height);
                let source_width = positioning_area.size.width / scale;
                let source_height = positioning_area.size.height / scale;
                let (position_x, position_y) = background_position(position);
                let source_x = (intrinsic_width - source_width) * position_x;
                let source_y = (intrinsic_height - source_height) * position_y;
                (
                    positioning_area.size.width,
                    positioning_area.size.height,
                    PhysicalRect::new(source_x, source_y, source_width, source_height),
                )
            }
            "contain" => {
                let scale = (positioning_area.size.width / intrinsic_width)
                    .min(positioning_area.size.height / intrinsic_height);
                (
                    intrinsic_width * scale,
                    intrinsic_height * scale,
                    PhysicalRect::new(0.0, 0.0, intrinsic_width, intrinsic_height),
                )
            }
            _ => (
                intrinsic_width,
                intrinsic_height,
                PhysicalRect::new(0.0, 0.0, intrinsic_width, intrinsic_height),
            ),
        };
        let (position_x, position_y) = background_position(position);
        let origin_x =
            positioning_area.origin.x + (positioning_area.size.width - paint_width) * position_x;
        let origin_y =
            positioning_area.origin.y + (positioning_area.size.height - paint_height) * position_y;
        self.push(
            fragment,
            PaintPhase::Background,
            painting_area,
            coordinate_space,
            DisplayCommand::PushClip(ClipShape::Rect(painting_area)),
        );
        let repeat_x = matches!(repeat, "repeat" | "repeat-x");
        let repeat_y = matches!(repeat, "repeat" | "repeat-y");
        let start_x = if repeat_x {
            origin_x - ((origin_x - painting_area.origin.x) / paint_width).ceil() * paint_width
        } else {
            origin_x
        };
        let start_y = if repeat_y {
            origin_y - ((origin_y - painting_area.origin.y) / paint_height).ceil() * paint_height
        } else {
            origin_y
        };
        let end_x = if repeat_x {
            painting_area.origin.x + painting_area.size.width
        } else {
            start_x + paint_width
        };
        let end_y = if repeat_y {
            painting_area.origin.y + painting_area.size.height
        } else {
            start_y + paint_height
        };
        let mut y = start_y;
        let mut tiles = 0_usize;
        while y < end_y && tiles < 4_096 {
            let mut x = start_x;
            while x < end_x && tiles < 4_096 {
                let destination = PhysicalRect::new(x, y, paint_width, paint_height);
                self.push(
                    fragment,
                    PaintPhase::Background,
                    destination,
                    coordinate_space,
                    DisplayCommand::Image(ImagePaint {
                        resource: loaded.id,
                        destination,
                        source,
                        interpolate: true,
                    }),
                );
                tiles += 1;
                if !repeat_x {
                    break;
                }
                x += paint_width;
            }
            if !repeat_y {
                break;
            }
            y += paint_height;
        }
        self.push(
            fragment,
            PaintPhase::Background,
            painting_area,
            coordinate_space,
            DisplayCommand::PopClip,
        );
    }

    /// CSS paints the root element background over the whole canvas rather
    /// than clipping it to the root element's content-dependent border box.
    fn background_rect(
        &self,
        fragment: &Fragment,
        geometry: &crate::layout::BoxGeometry,
        coordinate_space: PaintCoordinateSpace,
    ) -> (PhysicalRect, PaintCoordinateSpace) {
        let is_document_root_box = self
            .fragments
            .get(self.fragments.root())
            .is_some_and(|root| root.children.contains(&fragment.id));
        if is_document_root_box {
            (
                PhysicalRect::new(
                    0.0,
                    0.0,
                    self.fragments.viewport.width,
                    self.fragments.viewport.height,
                ),
                PaintCoordinateSpace::Viewport,
            )
        } else {
            (geometry.border_rect(), coordinate_space)
        }
    }

    /// Paints one text run: its shadow, its glyphs, and its decoration lines.
    ///
    /// Order follows CSS Text Decoration Level 3 §4: the shadow paints first so
    /// it stays behind the glyphs, and the decoration lines follow the glyphs
    /// so an underline is not erased by the descenders it crosses.
    fn paint_text(
        &mut self,
        fragment: &Fragment,
        text: &crate::layout::TextFragmentData,
        style: Option<&ComputedStyle>,
        current_color: Color,
        coordinate_space: PaintCoordinateSpace,
    ) {
        let run = self.shaper.shape(
            &text.text,
            text.font_size,
            PhysicalPoint {
                x: fragment.rect.origin.x,
                y: text.baseline,
            },
            current_color,
        );
        self.glyphs = self.glyphs.saturating_add(run.glyphs.len());
        if self.glyphs > self.options.limits.max_glyphs {
            self.diagnostics.push(DisplayListDiagnostic {
                node: fragment.source,
                code: DisplayListDiagnosticCode::GlyphLimit,
                message: "display-list glyph limit exceeded".to_owned(),
            });
            return;
        }
        for layer in text_shadows(style, current_color, self.options.palette) {
            let shadow = TextShadowPaint {
                run: fragment.id.as_u32(),
                font: run.font,
                font_size: run.font_size,
                offset: layer.offset,
                blur_radius: layer.blur_radius,
                color: layer.color,
            };
            self.push(
                fragment,
                PaintPhase::TextDecoration,
                text_shadow_bounds(&shadow, fragment.rect),
                coordinate_space,
                DisplayCommand::TextShadow(shadow),
            );
        }
        // Resolved before the run is moved into its command so the shaping
        // result is not cloned for every line of text on the page.
        let decorations =
            self.text_decorations(fragment, text.font_size, text.baseline, current_color);
        self.push(
            fragment,
            PaintPhase::Content,
            fragment.rect,
            coordinate_space,
            DisplayCommand::GlyphRun(run),
        );
        for decoration in decorations {
            self.push(
                fragment,
                PaintPhase::TextDecoration,
                decoration.rect,
                coordinate_space,
                DisplayCommand::TextDecoration(decoration),
            );
        }
    }

    /// Resolves the decoration lines that apply to a text run.
    ///
    /// CSS propagates decorations to descendant inline boxes, so a
    /// `text-decoration: underline` on a block underlines every run inside it
    /// (CSS Text Decoration Level 3 §2). The cascade does not implement that
    /// propagation yet, so paint approximates it by walking formatting
    /// ancestors and taking the first decoration that specifies a line: for the
    /// common `a`, `u`, `abbr` and heading cases that is exact, but an
    /// intermediate inline that sets `text-decoration-line: none` does not yet
    /// switch the decoration back off. Moving this into render-css is the
    /// correct fix; the walk then disappears.
    ///
    /// Style, colour and thickness come from the run's own element rather than
    /// the ancestor that supplied the line, which is where the specification
    /// looks for them.
    fn text_decorations(
        &self,
        fragment: &Fragment,
        font_size: f32,
        baseline: f32,
        current_color: Color,
    ) -> Vec<TextDecoration> {
        let Some(lines) = self.inherited_text_decoration(fragment) else {
            return Vec::new();
        };
        let style = self
            .formatting
            .get(fragment.formatting_node)
            .and_then(|node| node.style_source)
            .and_then(|source| self.styles.get(&source));
        let color = style
            .and_then(|style| style.get("text-decoration-color"))
            .and_then(|value| parse_typed_property("color", &value.parseable_css()))
            .and_then(|value| match value {
                Ok(TypedPropertyValue::Color(color)) => Some(color),
                _ => None,
            })
            .map_or(current_color, |color| {
                self.options.palette.resolve(color, current_color)
            });
        let line_style = decoration_style(style);
        let thickness = decoration_thickness(style, font_size);
        lines
            .into_iter()
            .map(|line| TextDecoration {
                rect: decoration_rect(fragment.rect, line, baseline, font_size, thickness),
                color,
                line,
                style: line_style,
                thickness,
            })
            .collect()
    }

    /// Walks formatting ancestors for the first element that specifies a
    /// `text-decoration-line` other than `none`.
    fn inherited_text_decoration(&self, fragment: &Fragment) -> Option<DecorationLines> {
        let mut node = self
            .formatting
            .get(fragment.formatting_node)
            .map(|node| node.id);
        while let Some(current) = node {
            let source = self
                .formatting
                .get(current)
                .and_then(|node| node.style_source);
            if let Some(line) = source
                .and_then(|source| self.styles.get(&source))
                .and_then(decoration_lines_from_style)
            {
                return Some(line);
            }
            node = self.formatting_parent(current);
        }
        None
    }

    /// Paints the marker of a `display: list-item` box.
    ///
    /// Marker geometry is user-agent defined (CSS Lists Level 3 §3.1): the
    /// marker box sits outside the principal block box by default and is
    /// emitted as its own command so paint, not layout, owns its placement.
    /// `list-style-position: outside` puts the marker to the leading side of
    /// the content edge; `inside` puts it at the content origin, where the
    /// principal box's own text follows it.
    fn paint_list_marker(
        &mut self,
        fragment: &Fragment,
        geometry: &crate::layout::BoxGeometry,
        style: &ComputedStyle,
        current_color: Color,
        coordinate_space: PaintCoordinateSpace,
    ) {
        // The marker belongs to the list item's own principal box (CSS 2.1
        // §12.5). The anonymous block that wraps a list item's inline content
        // inherits the item's `display: list-item` through its style source,
        // so the test is on the box's own element: an anonymous box carries no
        // element, and an element whose `display` was changed away from
        // `list-item` generates no `::marker` at all.
        let Some(source) = fragment.source else {
            return;
        };
        if !self.styles.get(&source).is_some_and(is_list_item) {
            return;
        }
        let Some(marker) = self.list_style_type(fragment) else {
            return;
        };
        // The marker shares the list item's first-line baseline, which paint
        // only learns from the item's first text fragment.
        let Some(baseline) = self.first_line_baseline(fragment) else {
            return;
        };
        let font_size = style_font_size(style);
        let color = self.options.palette.resolve(
            typed_color(style, "color").unwrap_or(CssColor::CurrentColor),
            current_color,
        );
        let inside = self.list_style_position(fragment) == ListStylePosition::Inside;
        let gap = marker_gap(font_size);
        match marker {
            ListMarker::Bullet(shape) => {
                let size = bullet_size(font_size);
                let rect = PhysicalRect::new(
                    marker_origin_x(geometry.content_rect, gap, inside, size),
                    baseline - font_size * BULLET_ASCENT,
                    size,
                    size,
                );
                self.push(
                    fragment,
                    PaintPhase::Content,
                    rect,
                    coordinate_space,
                    DisplayCommand::ListMarker(ListMarkerPaint { rect, color, shape }),
                );
            }
            ListMarker::Ordered { ordinal, kind } => {
                // Ordered markers are text, so they shape and paint exactly like
                // any other run: only their origin differs from the item's.
                let text = format_list_marker(kind, ordinal);
                let run = self.shaper.shape(
                    &text,
                    font_size,
                    PhysicalPoint {
                        x: 0.0,
                        y: baseline,
                    },
                    color,
                );
                let width = run_advance(&run);
                let origin_x = marker_origin_x(geometry.content_rect, gap, inside, width);
                let run = offset_run(run, origin_x);
                let rect = PhysicalRect::new(
                    origin_x,
                    baseline - font_size * ASCENT_RATIO,
                    width,
                    font_size,
                );
                self.glyphs = self.glyphs.saturating_add(run.glyphs.len());
                self.push(
                    fragment,
                    PaintPhase::Content,
                    rect,
                    coordinate_space,
                    DisplayCommand::GlyphRun(run),
                );
            }
        }
    }

    /// The baseline of the first text fragment in a box's subtree, which is the
    /// first line box's baseline for ordinary flow content.
    fn first_line_baseline(&self, fragment: &Fragment) -> Option<f32> {
        self.first_text_baseline(fragment, 0)
    }

    fn first_text_baseline(&self, fragment: &Fragment, depth: u32) -> Option<f32> {
        if depth > MARKER_BASELINE_DEPTH {
            return None;
        }
        if let FragmentKind::Text(text) = &fragment.kind {
            return Some(text.baseline);
        }
        fragment.children.iter().find_map(|child| {
            self.fragments
                .get(*child)
                .and_then(|child| self.first_text_baseline(child, depth.saturating_add(1)))
        })
    }

    /// Resolves `list-style-type` for a list item, which CSS defines as an
    /// inherited property. Neither it nor `list-style-position` is a registered
    /// property yet, so the computed map only carries a value on elements that
    /// declare one themselves; paint therefore takes the value from the nearest
    /// formatting ancestor that specifies it, which is what inheritance would
    /// produce unless an intermediate element resets it. Registering both in
    /// render-css makes this walk redundant.
    fn list_style_type(&self, fragment: &Fragment) -> Option<ListMarker> {
        let kind = self.inherited_style_value(fragment, "list-style-type", ListStyleType::parse)?;
        let ordinal = self.list_item_ordinal(fragment);
        Some(match kind {
            ListStyleType::None => return None,
            ListStyleType::Bullet(shape) => ListMarker::Bullet(shape),
            ListStyleType::Ordered(kind) => ListMarker::Ordered { ordinal, kind },
        })
    }

    fn list_style_position(&self, fragment: &Fragment) -> ListStylePosition {
        self.inherited_style_value(fragment, "list-style-position", ListStylePosition::parse)
            .unwrap_or(ListStylePosition::Outside)
    }

    /// The one-based position of a list item among the list items that precede
    /// it in the same list, which is the value its marker renders.
    ///
    /// `start`, `reversed` and `value` on `ol`/`li` change the first and
    /// individual ordinals; those attributes are not visible to paint, which
    /// has no DOM, so they are not applied yet.
    fn list_item_ordinal(&self, fragment: &Fragment) -> u32 {
        let Some(node) = self.formatting.get(fragment.formatting_node) else {
            return 1;
        };
        let Some(parent) = self
            .formatting_parent(node.id)
            .and_then(|id| self.formatting.get(id))
        else {
            return 1;
        };
        let mut ordinal = 1;
        for sibling in &parent.children {
            if *sibling == node.id {
                break;
            }
            if self
                .formatting
                .get(*sibling)
                .and_then(|sibling| sibling.style_source)
                .and_then(|source| self.styles.get(&source))
                .is_some_and(is_list_item)
            {
                ordinal += 1;
            }
        }
        ordinal
    }

    /// Nearest specified value of a property that is inherited by CSS but not
    /// registered in the computed-value registry, searched from the run's own
    /// element outwards.
    fn inherited_style_value<T>(
        &self,
        fragment: &Fragment,
        property: &str,
        parse: impl Fn(&str) -> Option<T>,
    ) -> Option<T> {
        let mut node = Some(fragment.formatting_node);
        while let Some(current) = node {
            let style = self
                .formatting
                .get(current)
                .and_then(|node| node.style_source)
                .and_then(|source| self.styles.get(&source));
            if let Some(value) = style
                .and_then(|style| style.get(property))
                .and_then(|value| parse(value.css_text()))
            {
                return Some(value);
            }
            node = self.formatting_parent(current);
        }
        None
    }

    fn formatting_parent(&self, id: FormattingNodeId) -> Option<FormattingNodeId> {
        self.formatting_parents
            .get(id.as_u32() as usize)
            .copied()
            .flatten()
    }

    fn paint_image(
        &mut self,
        fragment: &Fragment,
        geometry: &crate::layout::BoxGeometry,
        style: Option<&ComputedStyle>,
        coordinate_space: PaintCoordinateSpace,
    ) {
        let Some(loaded) = fragment
            .source
            .and_then(|source| self.images.and_then(|images| images.get_for_node(source)))
        else {
            return;
        };
        let content = self.recovered_content_rect(fragment, geometry, style);
        let (width, height) = loaded.image.intrinsic_size();
        if width == 0 || height == 0 || content.size.width <= 0.0 || content.size.height <= 0.0 {
            return;
        }
        let destination = Self::object_fit_rect(
            content,
            image_dimension_to_f32(width),
            image_dimension_to_f32(height),
            style
                .and_then(|style| style.typed("object-fit"))
                .and_then(|value| match value {
                    TypedPropertyValue::ObjectFit(value) => Some(*value),
                    _ => None,
                })
                .unwrap_or(ObjectFit::Fill),
        );
        let clips = destination != content;
        if clips {
            self.push(
                fragment,
                PaintPhase::Content,
                content,
                coordinate_space,
                DisplayCommand::PushClip(ClipShape::Rect(content)),
            );
        }
        self.push(
            fragment,
            PaintPhase::Content,
            destination,
            coordinate_space,
            DisplayCommand::Image(ImagePaint {
                resource: loaded.id,
                destination,
                source: PhysicalRect::new(
                    0.0,
                    0.0,
                    image_dimension_to_f32(width),
                    image_dimension_to_f32(height),
                ),
                interpolate: true,
            }),
        );
        if clips {
            self.push(
                fragment,
                PaintPhase::Content,
                content,
                coordinate_space,
                DisplayCommand::PopClip,
            );
        }
    }

    /// Recovers a fragment's content box when layout collapsed a percentage
    /// dimension to zero.
    ///
    /// The block solver measures flow and positioned children against
    /// provisional zero-height containing rects, so percentage sizes in the
    /// `position:relative` + `padding-top` aspect-ratio wrapper pattern used
    /// by media cards (and in any statically wrapped chain below them) can
    /// collapse to a zero-height box before paint. CSS instead resolves those
    /// percentages against a real containing block: the padding box of the
    /// nearest positioned ancestor for absolutely positioned boxes and the
    /// content box of the containing block for in-flow boxes (CSS 2 §10.1).
    /// Paint restores that axis from the enclosing fragments so replaced
    /// content such as decoded card covers is not silently dropped. Only a
    /// bare percentage on the collapsed axis triggers recovery, so explicit
    /// zero sizes and auto-sized boxes keep their laid-out geometry.
    fn recovered_content_rect(
        &self,
        fragment: &Fragment,
        geometry: &crate::layout::BoxGeometry,
        style: Option<&ComputedStyle>,
    ) -> PhysicalRect {
        let mut content = geometry.content_rect;
        if content.size.width > 0.0 && content.size.height > 0.0 {
            return content;
        }
        let positioned = is_positioned(style);
        let mut ancestor = self.parent_box(fragment.id);
        while let Some(parent) = ancestor {
            let FragmentKind::Box(parent_geometry) = &parent.kind else {
                break;
            };
            let containing = if positioned {
                parent_geometry.padding_rect()
            } else {
                parent_geometry.content_rect
            };
            let mut recovered = false;
            if collapses_percentage_size(style, "width")
                && content.size.width <= 0.0
                && containing.size.width > 0.0
            {
                content.origin.x = containing.origin.x;
                content.size.width = containing.size.width;
                recovered = true;
            }
            if collapses_percentage_size(style, "height")
                && content.size.height <= 0.0
                && containing.size.height > 0.0
            {
                content.origin.y = containing.origin.y;
                content.size.height = containing.size.height;
                recovered = true;
            }
            // In-flow boxes take their basis from the containing block, so a
            // single level is enough. Absolutely positioned boxes skip static
            // wrappers until the nearest positioned ancestor (their CSS
            // containing block) is reached.
            if recovered
                || !positioned
                || is_positioned(parent.source.and_then(|source| self.styles.get(&source)))
            {
                break;
            }
            ancestor = self.parent_box(parent.id);
        }
        content
    }

    fn parent_box(&self, fragment_id: FragmentId) -> Option<&Fragment> {
        self.parents
            .get(&fragment_id)
            .and_then(|parent| self.fragments.get(*parent))
    }

    fn object_fit_rect(
        content: PhysicalRect,
        intrinsic_width: f32,
        intrinsic_height: f32,
        fit: ObjectFit,
    ) -> PhysicalRect {
        let contain_scale =
            (content.size.width / intrinsic_width).min(content.size.height / intrinsic_height);
        let cover_scale =
            (content.size.width / intrinsic_width).max(content.size.height / intrinsic_height);
        let scale = match fit {
            ObjectFit::Fill => return content,
            ObjectFit::Contain => contain_scale,
            ObjectFit::Cover => cover_scale,
            ObjectFit::None => 1.0,
            ObjectFit::ScaleDown => contain_scale.min(1.0),
        };
        let width = intrinsic_width * scale;
        let height = intrinsic_height * scale;
        PhysicalRect::new(
            content.origin.x + (content.size.width - width) / 2.0,
            content.origin.y + (content.size.height - height) / 2.0,
            width,
            height,
        )
    }

    fn style_for(&mut self, fragment: &Fragment) -> Option<&ComputedStyle> {
        let source = self
            .formatting
            .get(fragment.formatting_node)
            .and_then(|node| node.style_source);
        let style = source.and_then(|source| self.styles.get(&source));
        if source.is_some() && style.is_none() {
            self.diagnostics.push(DisplayListDiagnostic {
                node: source,
                code: DisplayListDiagnosticCode::MissingStyle,
                message: "fragment style source has no computed style".to_owned(),
            });
        }
        style
    }

    fn push(
        &mut self,
        fragment: &Fragment,
        phase: PaintPhase,
        bounds: PhysicalRect,
        coordinate_space: PaintCoordinateSpace,
        command: DisplayCommand,
    ) {
        if self.items.len() >= self.options.limits.max_items {
            if !self.limit_reported {
                self.limit_reported = true;
                self.diagnostics.push(DisplayListDiagnostic {
                    node: fragment.source,
                    code: DisplayListDiagnosticCode::ItemLimit,
                    message: "display-list item limit exceeded".to_owned(),
                });
            }
            return;
        }
        let ordinal = self.ordinals.entry((fragment.source, phase)).or_default();
        let id = DisplayItemId {
            source: fragment.source,
            fragment_hint: fragment.id.as_u32(),
            phase,
            ordinal: *ordinal,
        };
        *ordinal = ordinal.saturating_add(1);
        self.items.push(DisplayItem {
            id,
            fragment: fragment.id,
            source: fragment.source,
            bounds,
            coordinate_space,
            command,
        });
    }
}

fn overflow_clip_shape(
    geometry: &crate::layout::BoxGeometry,
    style: &ComputedStyle,
) -> Option<ClipShape> {
    let clips_x = matches!(
        style.typed("overflow-x"),
        Some(TypedPropertyValue::Overflow(value))
            if !matches!(value, Overflow::Visible)
    );
    let clips_y = matches!(
        style.typed("overflow-y"),
        Some(TypedPropertyValue::Overflow(value))
            if !matches!(value, Overflow::Visible)
    );
    if clips_x || clips_y {
        let rect = geometry.padding_rect();
        let radii = inset_corner_radii(
            corner_radii(Some(style), geometry.border_rect()),
            geometry.border.top,
            geometry.border.right,
            geometry.border.bottom,
            geometry.border.left,
        );
        Some(if has_corner_radius(radii) {
            ClipShape::RoundedRect { rect, radii }
        } else {
            ClipShape::Rect(rect)
        })
    } else {
        None
    }
}

fn clip_shape_rect(shape: ClipShape) -> PhysicalRect {
    match shape {
        ClipShape::Rect(rect) | ClipShape::RoundedRect { rect, .. } => rect,
    }
}

/// Whether the computed size is a bare percentage, the only basis the layout
/// solver can collapse to zero while measuring children.
fn collapses_percentage_size(style: Option<&ComputedStyle>, property: &str) -> bool {
    matches!(
        style.and_then(|style| style.typed(property)),
        Some(TypedPropertyValue::Size(Size::LengthPercentage(
            LengthPercentage::Percentage(_),
        )))
    )
}

fn is_positioned(style: Option<&ComputedStyle>) -> bool {
    matches!(
        style.and_then(|style| style.typed("position")),
        Some(TypedPropertyValue::Position(
            Position::Absolute | Position::Fixed
        ))
    )
}

fn has_corner_radius(radii: CornerRadii) -> bool {
    radii.top_left > 0.0
        || radii.top_right > 0.0
        || radii.bottom_right > 0.0
        || radii.bottom_left > 0.0
}

fn background_clip_shape(rect: PhysicalRect, radii: CornerRadii) -> ClipShape {
    if has_corner_radius(radii) {
        ClipShape::RoundedRect { rect, radii }
    } else {
        ClipShape::Rect(rect)
    }
}

fn inset_corner_radii(
    radii: CornerRadii,
    top: f32,
    right: f32,
    bottom: f32,
    left: f32,
) -> CornerRadii {
    CornerRadii {
        top_left: (radii.top_left - top.max(left)).max(0.0),
        top_right: (radii.top_right - top.max(right)).max(0.0),
        bottom_right: (radii.bottom_right - bottom.max(right)).max(0.0),
        bottom_left: (radii.bottom_left - bottom.max(left)).max(0.0),
    }
}

fn corner_radii(style: Option<&ComputedStyle>, rect: PhysicalRect) -> CornerRadii {
    let mut radii = style
        .and_then(|style| style.get("border-radius"))
        .and_then(|value| parse_corner_radii(value.css_text(), rect))
        .unwrap_or_default();
    for (property, slot) in [
        ("border-top-left-radius", &mut radii.top_left),
        ("border-top-right-radius", &mut radii.top_right),
        ("border-bottom-right-radius", &mut radii.bottom_right),
        ("border-bottom-left-radius", &mut radii.bottom_left),
    ] {
        if let Some(value) = style
            .and_then(|style| style.get(property))
            .and_then(|value| {
                parse_radius_value(
                    value.css_text().split_ascii_whitespace().next()?,
                    rect.size.width,
                )
            })
        {
            *slot = value;
        }
    }
    normalize_corner_radii(radii, rect)
}

fn parse_corner_radii(value: &str, rect: PhysicalRect) -> Option<CornerRadii> {
    let (horizontal, vertical) = value
        .split_once('/')
        .map_or((value, None), |(a, b)| (a, Some(b)));
    let horizontal = parse_radius_list(horizontal, rect.size.width)?;
    let vertical = match vertical {
        Some(value) => parse_radius_list(value, rect.size.height)?,
        None => horizontal,
    };
    Some(CornerRadii {
        top_left: f32::midpoint(horizontal[0], vertical[0]),
        top_right: f32::midpoint(horizontal[1], vertical[1]),
        bottom_right: f32::midpoint(horizontal[2], vertical[2]),
        bottom_left: f32::midpoint(horizontal[3], vertical[3]),
    })
}

fn parse_radius_list(value: &str, basis: f32) -> Option<[f32; 4]> {
    let values = value
        .split_ascii_whitespace()
        .filter_map(|value| parse_radius_value(value, basis))
        .collect::<Vec<_>>();
    let [first, second, third, fourth] = match values.as_slice() {
        [first] => [*first, *first, *first, *first],
        [first, second] => [*first, *second, *first, *second],
        [first, second, third] => [*first, *second, *third, *second],
        [first, second, third, fourth] => [*first, *second, *third, *fourth],
        _ => return None,
    };
    Some([first, second, third, fourth])
}

fn parse_radius_value(value: &str, basis: f32) -> Option<f32> {
    let value = value.trim().to_ascii_lowercase();
    if let Some(value) = value.strip_suffix('%') {
        return value
            .trim()
            .parse::<f32>()
            .ok()
            .map(|value| (value / 100.0 * basis).max(0.0));
    }
    value
        .strip_suffix("px")
        .unwrap_or(&value)
        .trim()
        .parse::<f32>()
        .ok()
        .map(|value| value.max(0.0))
}

fn normalize_corner_radii(mut radii: CornerRadii, rect: PhysicalRect) -> CornerRadii {
    let mut scale = 1.0_f32;
    for (sum, available) in [
        (radii.top_left + radii.top_right, rect.size.width),
        (radii.bottom_left + radii.bottom_right, rect.size.width),
        (radii.top_left + radii.bottom_left, rect.size.height),
        (radii.top_right + radii.bottom_right, rect.size.height),
    ] {
        if sum > available && sum > 0.0 {
            scale = scale.min(available / sum);
        }
    }
    radii.top_left *= scale;
    radii.top_right *= scale;
    radii.bottom_right *= scale;
    radii.bottom_left *= scale;
    let max_radius = (rect.size.width.min(rect.size.height) / 2.0).max(0.0);
    radii.top_left = radii.top_left.min(max_radius);
    radii.top_right = radii.top_right.min(max_radius);
    radii.bottom_right = radii.bottom_right.min(max_radius);
    radii.bottom_left = radii.bottom_left.min(max_radius);
    radii
}

fn box_shadows(
    style: Option<&ComputedStyle>,
    rect: PhysicalRect,
    radii: CornerRadii,
    current_color: Color,
    palette: SystemPalette,
) -> Vec<BoxShadowPaint> {
    let Some(value) = style.and_then(|style| style.get("box-shadow")) else {
        return Vec::new();
    };
    split_gradient_arguments(value.css_text())
        .into_iter()
        .filter_map(|shadow| parse_box_shadow(shadow, rect, radii, current_color, palette))
        .collect()
}

fn parse_box_shadow(
    value: &str,
    rect: PhysicalRect,
    radii: CornerRadii,
    current_color: Color,
    palette: SystemPalette,
) -> Option<BoxShadowPaint> {
    if value.eq_ignore_ascii_case("none") {
        return None;
    }
    let mut inset = false;
    let mut color = None;
    let mut lengths = Vec::new();
    for token in split_css_whitespace(value) {
        if token.eq_ignore_ascii_case("inset") {
            inset = true;
        } else if color.is_none()
            && let Some(Ok(TypedPropertyValue::Color(value))) = parse_typed_property("color", token)
        {
            color = Some(palette.resolve(value, current_color));
        } else if let Some(length) = parse_shadow_length(token) {
            lengths.push(length);
        }
    }
    if lengths.len() < 2 {
        return None;
    }
    Some(BoxShadowPaint {
        rect,
        offset: PhysicalPoint {
            x: lengths[0],
            y: lengths[1],
        },
        blur_radius: lengths.get(2).copied().unwrap_or(0.0),
        spread_radius: lengths.get(3).copied().unwrap_or(0.0),
        color: color.unwrap_or(current_color),
        inset,
        radii,
    })
}

fn split_css_whitespace(value: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut start = None;
    let mut depth = 0_u32;
    for (index, character) in value.char_indices() {
        match character {
            '(' => depth = depth.saturating_add(1),
            ')' => depth = depth.saturating_sub(1),
            character if character.is_whitespace() && depth == 0 => {
                if let Some(start) = start.take() {
                    tokens.push(value[start..index].trim());
                }
            }
            _ if start.is_none() => start = Some(index),
            _ => {}
        }
    }
    if let Some(start) = start {
        tokens.push(value[start..].trim());
    }
    tokens
}

fn parse_shadow_length(value: &str) -> Option<f32> {
    let value = value.trim().to_ascii_lowercase();
    value
        .strip_suffix("px")
        .unwrap_or(&value)
        .parse::<f32>()
        .ok()
        .filter(|value| value.is_finite())
}

fn shadow_bounds(shadow: &BoxShadowPaint) -> PhysicalRect {
    if shadow.inset {
        return shadow.rect;
    }
    let expansion = shadow.blur_radius.max(0.0) + shadow.spread_radius.max(0.0);
    PhysicalRect::new(
        shadow.rect.origin.x + shadow.offset.x - expansion,
        shadow.rect.origin.y + shadow.offset.y - expansion,
        shadow.rect.size.width + expansion * 2.0,
        shadow.rect.size.height + expansion * 2.0,
    )
}

#[allow(
    clippy::cast_precision_loss,
    reason = "gradient stop counts are bounded by display-list limits"
)]
fn parse_linear_gradient(
    value: &str,
    rect: PhysicalRect,
    palette: SystemPalette,
    current_color: Color,
) -> Option<LinearGradient> {
    let value = value.trim();
    let lower = value.to_ascii_lowercase();
    if !lower.starts_with("linear-gradient(") || !value.ends_with(')') {
        return None;
    }
    let open = value.find('(')?;
    let arguments = split_gradient_arguments(&value[open + 1..value.len() - 1]);
    if arguments.len() < 2 {
        return None;
    }

    let (angle, first_stop) =
        parse_gradient_direction(arguments[0]).map_or((180.0, 0), |angle| (angle, 1));
    let stops = arguments[first_stop..]
        .iter()
        .enumerate()
        .filter_map(|(index, argument)| {
            let (color, offset) = parse_gradient_stop(argument, palette, current_color)?;
            let default_offset = if arguments.len().saturating_sub(first_stop) <= 1 {
                0.0
            } else {
                index as f32 / (arguments.len() - first_stop - 1) as f32
            };
            Some(GradientStop {
                offset: offset.unwrap_or(default_offset).clamp(0.0, 1.0),
                color,
            })
        })
        .collect::<Vec<_>>();
    if stops.len() < 2 {
        return None;
    }

    let radians = angle.to_radians();
    let direction = (radians.sin(), -radians.cos());
    let half_length = f32::midpoint(
        direction.0.abs() * rect.size.width,
        direction.1.abs() * rect.size.height,
    );
    let center = PhysicalPoint {
        x: rect.origin.x + rect.size.width / 2.0,
        y: rect.origin.y + rect.size.height / 2.0,
    };
    Some(LinearGradient {
        rect,
        start: PhysicalPoint {
            x: center.x - direction.0 * half_length,
            y: center.y - direction.1 * half_length,
        },
        end: PhysicalPoint {
            x: center.x + direction.0 * half_length,
            y: center.y + direction.1 * half_length,
        },
        stops,
    })
}

fn split_gradient_arguments(value: &str) -> Vec<&str> {
    let mut arguments = Vec::new();
    let mut start = 0;
    let mut depth = 0_u32;
    let mut quote = None;
    for (index, character) in value.char_indices() {
        match (quote, character) {
            (Some(expected), character) if character == expected => quote = None,
            (None, '\'' | '"') => quote = Some(character),
            (None, '(') => depth = depth.saturating_add(1),
            (None, ')') => depth = depth.saturating_sub(1),
            (None, ',') if depth == 0 => {
                arguments.push(value[start..index].trim());
                start = index + character.len_utf8();
            }
            _ => {}
        }
    }
    arguments.push(value[start..].trim());
    arguments
}

fn parse_gradient_direction(value: &str) -> Option<f32> {
    let value = value.trim().to_ascii_lowercase();
    if let Some(degrees) = value.strip_suffix("deg") {
        return degrees.trim().parse::<f32>().ok();
    }
    let value = value.strip_prefix("to ")?;
    let horizontal = if value.contains("right") {
        Some(90.0)
    } else if value.contains("left") {
        Some(270.0)
    } else {
        None
    };
    let vertical = if value.contains("bottom") {
        Some(180.0)
    } else if value.contains("top") {
        Some(0.0)
    } else {
        None
    };
    match (horizontal, vertical) {
        (Some(horizontal), Some(vertical)) => Some(f32::midpoint(horizontal, vertical)),
        (Some(horizontal), None) | (None, Some(horizontal)) => Some(horizontal),
        (None, None) => None,
    }
}

fn parse_gradient_stop(
    value: &str,
    palette: SystemPalette,
    current_color: Color,
) -> Option<(Color, Option<f32>)> {
    let value = value.trim();
    let (color_value, offset) = value
        .rsplit_once(char::is_whitespace)
        .and_then(|(color, offset)| {
            let offset = offset.trim().strip_suffix('%')?.parse::<f32>().ok()?;
            Some((color.trim(), Some(offset / 100.0)))
        })
        .unwrap_or((value, None));
    let typed = parse_typed_property("color", color_value)?.ok()?;
    let TypedPropertyValue::Color(color) = typed else {
        return None;
    };
    Some((palette.resolve(color, current_color), offset))
}

#[allow(
    clippy::cast_precision_loss,
    reason = "display-list geometry is f32 and decoded image dimensions are bounded by image limits"
)]
fn image_dimension_to_f32(value: u32) -> f32 {
    value as f32
}

fn background_position(value: &str) -> (f32, f32) {
    let lower = value.to_ascii_lowercase();
    let horizontal = if lower.split_ascii_whitespace().any(|part| part == "right") {
        1.0
    } else if lower.split_ascii_whitespace().any(|part| part == "center") {
        0.5
    } else {
        lower
            .split_ascii_whitespace()
            .next()
            .and_then(|part| part.strip_suffix('%'))
            .and_then(|number| number.parse::<f32>().ok())
            .map_or(0.0, |number| (number / 100.0).clamp(0.0, 1.0))
    };
    let vertical = if lower.split_ascii_whitespace().any(|part| part == "bottom") {
        1.0
    } else if lower.split_ascii_whitespace().any(|part| part == "center") {
        0.5
    } else {
        lower
            .split_ascii_whitespace()
            .nth(1)
            .and_then(|part| part.strip_suffix('%'))
            .and_then(|number| number.parse::<f32>().ok())
            .map_or(0.0, |number| (number / 100.0).clamp(0.0, 1.0))
    };
    (horizontal, vertical)
}

fn fragment_coordinate_space(
    style: Option<&ComputedStyle>,
    parent: PaintCoordinateSpace,
) -> PaintCoordinateSpace {
    match style.and_then(|style| style.typed("position")) {
        Some(TypedPropertyValue::Position(Position::Fixed)) => PaintCoordinateSpace::Viewport,
        _ => parent,
    }
}

fn fragment_opacity(style: Option<&ComputedStyle>) -> f32 {
    style
        .and_then(|style| match style.typed("opacity") {
            Some(TypedPropertyValue::Opacity(value)) => Some(*value),
            _ => None,
        })
        .unwrap_or(1.0)
}

/// The stacking context grouping a fragment's whole subtree, if any: a
/// transform or a sub-unity opacity (or both) paints through one group, so
/// rasterization pays for a single offscreen composite per fragment.
fn fragment_stacking_context(
    opacity: f32,
    transform: Option<Transform2D>,
) -> Option<StackingContext> {
    match transform {
        Some(transform) => Some(StackingContext {
            opacity,
            transform,
            blend_mode: BlendMode::Normal,
            isolated: true,
            reason: CompositingReason::Transform,
        }),
        None if opacity < 1.0 => Some(StackingContext {
            opacity,
            transform: Transform2D::default(),
            blend_mode: BlendMode::Normal,
            isolated: true,
            reason: CompositingReason::Opacity,
        }),
        None => None,
    }
}

/// Converts one transform list function into its 2D affine matrix.
///
/// `matrix(a, b, c, d, e, f)` uses the spec's argument order, which maps a
/// column-vector matrix `[[a, c, e], [b, d, f]]` onto the `Transform2D`
/// field layout; `rotate(θ)` is `matrix(cos θ, sin θ, −sin θ, cos θ, 0, 0)`,
/// clockwise in the y-down screen coordinate system; `skew(ax, ay)` is
/// `matrix(1, tan ay, tan ax, 1, 0, 0)`.
#[allow(clippy::many_single_char_names)] // matrix(a, b, c, d, e, f) spec names
fn transform_function_matrix(
    function: &TransformFunction,
    border_rect: PhysicalRect,
    font_size: f32,
    viewport: PhysicalSize,
) -> Transform2D {
    match function {
        TransformFunction::Matrix([a, b, c, d, e, f]) => Transform2D {
            scale_x: *a,
            skew_x: *c,
            skew_y: *b,
            scale_y: *d,
            translate_x: *e,
            translate_y: *f,
        },
        TransformFunction::Translate(x, y) => Transform2D {
            translate_x: resolve_transform_length(x, border_rect.size.width, font_size, viewport),
            translate_y: resolve_transform_length(y, border_rect.size.height, font_size, viewport),
            ..Transform2D::default()
        },
        TransformFunction::Scale(x, y) => Transform2D {
            scale_x: *x,
            scale_y: *y,
            ..Transform2D::default()
        },
        TransformFunction::Rotate(radians) => {
            let (sin, cos) = radians.sin_cos();
            Transform2D {
                scale_x: cos,
                skew_x: -sin,
                skew_y: sin,
                scale_y: cos,
                ..Transform2D::default()
            }
        }
        TransformFunction::Skew(ax, ay) => Transform2D {
            skew_x: ax.tan(),
            skew_y: ay.tan(),
            ..Transform2D::default()
        },
    }
}

/// Resolves one component of `translate()` or `transform-origin` against the
/// border-box dimension of its axis. Unresolvable components (a calc the
/// resolver rejects) fall back to zero rather than dropping the whole
/// transform, mirroring how paint tolerates degenerate lengths elsewhere.
fn resolve_transform_length(
    value: &LengthPercentage,
    basis: f32,
    font_size: f32,
    viewport: PhysicalSize,
) -> f32 {
    let context = LengthResolutionContext {
        percentage_basis: Some(basis),
        font_size,
        viewport_width: viewport.width,
        viewport_height: viewport.height,
        ..LengthResolutionContext::default()
    };
    value.resolve(&context).unwrap_or(0.0)
}

/// Best-effort element font size for font-relative transform lengths. The
/// computed-value stage stores an absolute pixel size (CSS 2.1 §6.1.1), so
/// this is a cheap parse of the stored value; missing or unusual values fall
/// back to the 16px engine default that `LengthResolutionContext::default`
/// and the layout solver agree on.
fn style_font_size(style: &ComputedStyle) -> f32 {
    style
        .get("font-size")
        .map(ComputedValue::css_text)
        .and_then(|text| {
            text.trim()
                .strip_suffix("px")
                .unwrap_or(text.trim())
                .parse::<f32>()
                .ok()
        })
        .filter(|size| size.is_finite() && *size > 0.0)
        .unwrap_or(16.0)
}

fn typed_color(style: &ComputedStyle, property: &str) -> Option<CssColor> {
    match style.typed(property) {
        Some(TypedPropertyValue::Color(color)) => Some(*color),
        _ => None,
    }
}

// --- text decorations -------------------------------------------------------

/// Distance from the baseline to the alphabetic top of the reference em box,
/// the same ratio the reference text measurer uses for ascent
/// (`render-layout`'s `SimpleTextMeasurer`). Line positions are measured from
/// it because paint has no font metrics beyond the computed font size.
const ASCENT_RATIO: f32 = 0.8;
/// A bullet's diameter, as a fraction of the font size.
const BULLET_RATIO: f32 = 0.25;
/// A bullet's top edge above the baseline, as a fraction of the font size.
const BULLET_ASCENT: f32 = 0.75;
/// The gap between an outside marker and the content edge, as a fraction of
/// the font size.
const MARKER_GAP_RATIO: f32 = 0.35;
/// The smallest marker and gap that stays visible at small font sizes.
const MARKER_MINIMUM: f32 = 2.0;
/// How far the marker baseline search descends from a list item's box. Inline
/// content is a handful of levels deep; the bound keeps a pathological tree
/// from turning marker placement into a full subtree walk.
const MARKER_BASELINE_DEPTH: u32 = 16;
/// An automatic decoration thickness, as a fraction of the font size
/// (CSS Text Decoration Level 3 §2: the used value is a UA-chosen length).
const DECORATION_THICKNESS_RATIO: f32 = 0.0625;
const DECORATION_THICKNESS_MINIMUM: f32 = 1.0;
/// Where an automatic decoration line sits relative to the baseline. CSS
/// leaves these to the user agent (CSS Text Decoration Level 3 §4.1); the
/// values below place the overline at the top of the em box, the underline
/// just below the baseline, and the line-through near the x-height.
const OVERLINE_OFFSET: f32 = -0.95;
const LINE_THROUGH_OFFSET: f32 = -0.28;
const UNDERLINE_OFFSET: f32 = 0.12;

type DecorationLines = Vec<TextDecorationLine>;

fn decoration_lines_from_style(style: &ComputedStyle) -> Option<DecorationLines> {
    let raw = style
        .get("text-decoration-line")
        .map(ComputedValue::css_text)
        .or_else(|| {
            // The `text-decoration` shorthand is not expanded by the cascade
            // yet (render-css), so a sheet that uses the shorthand stores the
            // whole declaration under its own name. Read the line component
            // out of it here rather than leaving real underlines unpainted;
            // once the shorthand expands, this fallback stops matching.
            style.get("text-decoration").map(ComputedValue::css_text)
        })?;
    let lines = decoration_lines(raw);
    (!lines.is_empty()).then_some(lines)
}

fn decoration_lines(value: &str) -> DecorationLines {
    let mut lines = Vec::new();
    for token in split_css_whitespace(value) {
        let line = if token.eq_ignore_ascii_case("underline") {
            TextDecorationLine::Underline
        } else if token.eq_ignore_ascii_case("overline") {
            TextDecorationLine::Overline
        } else if token.eq_ignore_ascii_case("line-through") {
            TextDecorationLine::LineThrough
        } else {
            continue;
        };
        if !lines.contains(&line) {
            lines.push(line);
        }
    }
    lines
}

fn decoration_style(style: Option<&ComputedStyle>) -> TextDecorationStyle {
    let Some(style) = style else {
        return TextDecorationStyle::Solid;
    };
    let raw = style
        .get("text-decoration-style")
        .map(ComputedValue::css_text)
        .unwrap_or_default();
    if raw.trim().eq_ignore_ascii_case("double") {
        TextDecorationStyle::Double
    } else if raw.trim().eq_ignore_ascii_case("dotted") {
        TextDecorationStyle::Dotted
    } else if raw.trim().eq_ignore_ascii_case("dashed") {
        TextDecorationStyle::Dashed
    } else if raw.trim().eq_ignore_ascii_case("wavy") {
        TextDecorationStyle::Wavy
    } else {
        TextDecorationStyle::Solid
    }
}

/// The stroke thickness for a decoration line. `auto` picks a length from the
/// font size, which is what CSS leaves to the user agent (CSS Text
/// Decoration Level 3 §2); an explicit length is used as written, so
/// `text-decoration-thickness: 0` really does mean no stroke.
fn decoration_thickness(style: Option<&ComputedStyle>, font_size: f32) -> f32 {
    if let Some(value) = style
        .and_then(|style| style.get("text-decoration-thickness"))
        .map(ComputedValue::css_text)
        .and_then(|value| parse_shadow_length(value.trim()))
    {
        return value.max(0.0);
    }
    (font_size * DECORATION_THICKNESS_RATIO).max(DECORATION_THICKNESS_MINIMUM)
}

/// The stroke rectangle of one decoration line over a text run: the run's
/// horizontal extent, with the line's own top edge and thickness.
fn decoration_rect(
    run: PhysicalRect,
    line: TextDecorationLine,
    baseline: f32,
    font_size: f32,
    thickness: f32,
) -> PhysicalRect {
    let offset = match line {
        TextDecorationLine::Overline => OVERLINE_OFFSET,
        TextDecorationLine::LineThrough => LINE_THROUGH_OFFSET,
        TextDecorationLine::Underline => UNDERLINE_OFFSET,
    };
    PhysicalRect::new(
        run.origin.x,
        baseline + offset * font_size,
        run.size.width,
        thickness,
    )
}

/// `text-shadow` layers in paint order: the first layer paints furthest back
/// (CSS Text Decoration Level 3 §4.2).
fn text_shadows(
    style: Option<&ComputedStyle>,
    current_color: Color,
    palette: SystemPalette,
) -> Vec<ShadowLayer> {
    let Some(value) = style.and_then(|style| style.get("text-shadow")) else {
        return Vec::new();
    };
    let raw = value.css_text();
    if raw.trim().eq_ignore_ascii_case("none") {
        return Vec::new();
    }
    let mut layers = Vec::new();
    for layer in split_gradient_arguments(raw) {
        if let Some(shadow) = parse_text_shadow(layer, current_color, palette) {
            layers.push(shadow);
        }
    }
    layers
}

/// The offset, blur and colour of one `text-shadow` layer, before the run's
/// own font and geometry are attached.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ShadowLayer {
    offset: PhysicalPoint,
    blur_radius: f32,
    color: Color,
}

fn parse_text_shadow(
    value: &str,
    current_color: Color,
    palette: SystemPalette,
) -> Option<ShadowLayer> {
    let mut color = None;
    let mut lengths = Vec::new();
    for token in split_css_whitespace(value) {
        if color.is_none()
            && let Some(Ok(TypedPropertyValue::Color(parsed))) =
                parse_typed_property("color", token)
        {
            color = Some(palette.resolve(parsed, current_color));
        } else if let Some(length) = parse_shadow_length(token) {
            lengths.push(length);
        }
    }
    if lengths.len() < 2 {
        return None;
    }
    Some(ShadowLayer {
        offset: PhysicalPoint {
            x: lengths[0],
            y: lengths[1],
        },
        blur_radius: lengths.get(2).copied().unwrap_or(0.0).max(0.0),
        color: color.unwrap_or(current_color),
    })
}

/// The shadow's damage bounds: the run box displaced by the offset and grown
/// by the blur radius, which is where the rasterized shadow can reach.
fn text_shadow_bounds(shadow: &TextShadowPaint, run: PhysicalRect) -> PhysicalRect {
    let blur = shadow.blur_radius;
    PhysicalRect::new(
        run.origin.x + shadow.offset.x - blur,
        run.origin.y + shadow.offset.y - blur,
        run.size.width + blur * 2.0,
        run.size.height + blur * 2.0,
    )
}

// --- list markers -----------------------------------------------------------

/// The marker a list item renders, once `list-style-type` has been resolved
/// against the item's position in its list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ListMarker {
    Bullet(ListMarkerShape),
    Ordered {
        ordinal: u32,
        kind: OrderedListStyle,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ListStylePosition {
    Outside,
    Inside,
}

/// The `list-style-type` families this engine renders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ListStyleType {
    None,
    Bullet(ListMarkerShape),
    Ordered(OrderedListStyle),
}

/// The ordered marker families named by CSS Lists Level 3 §3.1 that have a
/// direct textual form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OrderedListStyle {
    Decimal,
    DecimalLeadingZero,
    LowerAlpha,
    UpperAlpha,
    LowerLatin,
    UpperLatin,
    LowerRoman,
    UpperRoman,
}

impl ListStyleType {
    fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        if value.eq_ignore_ascii_case("none") {
            Some(Self::None)
        } else if value.eq_ignore_ascii_case("disc") {
            Some(Self::Bullet(ListMarkerShape::Disc))
        } else if value.eq_ignore_ascii_case("circle") {
            Some(Self::Bullet(ListMarkerShape::Circle))
        } else if value.eq_ignore_ascii_case("square") {
            Some(Self::Bullet(ListMarkerShape::Square))
        } else {
            OrderedListStyle::parse(value).map(Self::Ordered)
        }
    }
}

impl ListStylePosition {
    fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        if value.eq_ignore_ascii_case("inside") {
            Some(Self::Inside)
        } else if value.eq_ignore_ascii_case("outside") {
            Some(Self::Outside)
        } else {
            None
        }
    }
}

impl OrderedListStyle {
    fn parse(value: &str) -> Option<Self> {
        let kind = if value.eq_ignore_ascii_case("decimal") {
            Self::Decimal
        } else if value.eq_ignore_ascii_case("decimal-leading-zero") {
            Self::DecimalLeadingZero
        } else if value.eq_ignore_ascii_case("lower-alpha") {
            Self::LowerAlpha
        } else if value.eq_ignore_ascii_case("upper-alpha") {
            Self::UpperAlpha
        } else if value.eq_ignore_ascii_case("lower-latin") {
            Self::LowerLatin
        } else if value.eq_ignore_ascii_case("upper-latin") {
            Self::UpperLatin
        } else if value.eq_ignore_ascii_case("lower-roman") {
            Self::LowerRoman
        } else if value.eq_ignore_ascii_case("upper-roman") {
            Self::UpperRoman
        } else {
            return None;
        };
        Some(kind)
    }
}

/// True when the element generates a `::marker`, which CSS restricts to
/// `display: list-item` boxes.
fn is_list_item(style: &ComputedStyle) -> bool {
    matches!(
        style.typed("display"),
        Some(TypedPropertyValue::Display(Display::Normal {
            list_item: true,
            ..
        }))
    )
}

fn bullet_size(font_size: f32) -> f32 {
    (font_size * BULLET_RATIO).max(MARKER_MINIMUM)
}

fn marker_gap(font_size: f32) -> f32 {
    (font_size * MARKER_GAP_RATIO).max(MARKER_MINIMUM)
}

/// Where the marker box starts relative to the list item's content box.
/// `outside` places it before the content edge, separated by `gap`; `inside`
/// places it at the content origin so the item's own text follows it.
fn marker_origin_x(content: PhysicalRect, gap: f32, inside: bool, width: f32) -> f32 {
    if inside {
        content.origin.x
    } else {
        content.origin.x - gap - width
    }
}

/// Formats an ordinal in its ordered marker family. The `.` suffix is the
/// default `suff` of the UA counter styles.
fn format_list_marker(kind: OrderedListStyle, ordinal: u32) -> String {
    let text = match kind {
        OrderedListStyle::Decimal => ordinal.to_string(),
        OrderedListStyle::DecimalLeadingZero => format!("{ordinal:02}"),
        OrderedListStyle::LowerAlpha | OrderedListStyle::LowerLatin => {
            alphabetic_marker(ordinal, false)
        }
        OrderedListStyle::UpperAlpha | OrderedListStyle::UpperLatin => {
            alphabetic_marker(ordinal, true)
        }
        OrderedListStyle::LowerRoman => roman_marker(ordinal, false),
        OrderedListStyle::UpperRoman => roman_marker(ordinal, true),
    };
    format!("{text}.")
}

/// The alphabetic series of CSS Lists Level 3 §3.1: `a`..`z`, then `aa`..`zz`,
/// bijectively base-26.
///
/// The *Latin* series is specified as the Latin alphabet, which extends past
/// the 26 ASCII letters; only the ASCII part is implemented here, so the Latin
/// families render as their alphabetic counterparts. The Unicode extension is a
/// follow-up, not a silent difference: an ordinal past 26 already rolls over to
/// `aa`, which is where the two series agree.
fn alphabetic_marker(ordinal: u32, upper: bool) -> String {
    // CSS Lists 3 §3.1: the series repeats `a`..`z`, then `aa`..`zz`,
    // bijectively base-26, least significant letter first.
    if ordinal == 0 {
        return String::new();
    }
    let mut index = ordinal - 1;
    let mut letters: Vec<char> = Vec::new();
    while {
        letters.push((b'a' + u8::try_from(index % 26).unwrap_or(0)) as char);
        index /= 26;
        index > 0
    } {}
    let ordered = if upper {
        letters
            .into_iter()
            .map(|letter| letter.to_ascii_uppercase())
            .collect::<Vec<_>>()
    } else {
        letters
    };
    ordered.iter().rev().collect()
}

fn roman_marker(ordinal: u32, upper: bool) -> String {
    const VALUES: [(u32, &str); 13] = [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ];
    // CSS Lists 3 §3.1 defines no roman representation past 3999; the
    // algorithm below produces empty text there, so the ordinal falls back to
    // its decimal form rather than an empty marker.
    if ordinal == 0 || ordinal > 3999 {
        return ordinal.to_string();
    }
    let mut remaining = ordinal;
    let mut text = String::new();
    for (value, symbol) in VALUES {
        while remaining >= value {
            text.push_str(symbol);
            remaining -= value;
        }
    }
    if upper {
        text.to_ascii_uppercase()
    } else {
        text
    }
}

/// The advance width a shaped run covers, which is the marker box width for an
/// ordered marker.
fn run_advance(run: &GlyphRun) -> f32 {
    run.glyphs.last().map_or(0.0, |glyph| {
        glyph.position.x + glyph.advance - glyphs_start(run)
    })
}

fn glyphs_start(run: &GlyphRun) -> f32 {
    run.glyphs.first().map_or(0.0, |glyph| glyph.position.x)
}

fn offset_run(run: GlyphRun, x: f32) -> GlyphRun {
    GlyphRun {
        glyphs: run
            .glyphs
            .into_iter()
            .map(|glyph| GlyphInstance {
                position: PhysicalPoint {
                    x: glyph.position.x + x,
                    ..glyph.position
                },
                ..glyph
            })
            .collect(),
        ..run
    }
}

fn border_paint(
    style: Option<&ComputedStyle>,
    geometry: &crate::layout::BoxGeometry,
    current_color: Color,
    palette: SystemPalette,
) -> BorderPaint {
    let style_at = |property, default| match style.and_then(|style| style.typed(property)) {
        Some(TypedPropertyValue::BorderStyle(value)) => *value,
        _ => default,
    };
    let color_at = |property| {
        style
            .and_then(|style| typed_color(style, property))
            .map_or(current_color, |color| palette.resolve(color, current_color))
    };
    BorderPaint {
        rect: geometry.border_rect(),
        widths: geometry.border,
        colors: [
            color_at("border-top-color"),
            color_at("border-right-color"),
            color_at("border-bottom-color"),
            color_at("border-left-color"),
        ],
        styles: [
            style_at("border-top-style", BorderStyle::None),
            style_at("border-right-style", BorderStyle::None),
            style_at("border-bottom-style", BorderStyle::None),
            style_at("border-left-style", BorderStyle::None),
        ],
        radii: corner_radii(style, geometry.border_rect()),
    }
}

const fn is_wide_character(character: char) -> bool {
    matches!(
        character as u32,
        0x1100..=0x115f
            | 0x2e80..=0xa4cf
            | 0xac00..=0xd7a3
            | 0xf900..=0xfaff
            | 0xfe10..=0xfe6f
            | 0xff00..=0xff60
            | 0xffe0..=0xffe6
            | 0x1f300..=0x1faff
    )
}

#[cfg(test)]
mod tests {
    // Decoration, marker and shadow geometry is resolved from exact font-size
    // fractions, so the assertions below compare exact values on purpose.
    #![allow(
        clippy::float_cmp,
        reason = "geometry under test is computed from exact font-size fractions"
    )]
    use crate::css::cascade::{CascadeInput, CascadeOrigin};
    use crate::css::computed::{ComputationLimits, PropertyRegistry, compute_document_styles};
    use crate::css::properties::ObjectFit;
    use crate::css::selector::{MatchContext, parse_selector_list, select_all};
    use crate::css::stylesheet::parse_stylesheet;
    use crate::dom::NodeId;
    use crate::html::{ParseOutput, parse_document};
    use crate::layout::{
        FormattingLimits, FragmentKind, LayoutOptions, PhysicalPoint, PhysicalRect,
        SimpleTextMeasurer, build_formatting_tree, layout_formatting_tree,
    };
    use crate::paint::Color;

    use super::{
        Builder, ClipShape, CompositingReason, DisplayCommand, DisplayListBuildOutput,
        DisplayListBuilderOptions, GlyphRun, ImagePaint, ListMarkerPaint, ListMarkerShape,
        ReferenceTextShaper, StackingContext, TextDecoration, TextDecorationLine,
        TextDecorationStyle, TextShadowPaint, Transform2D, build_display_list,
        build_display_list_with_images,
    };

    #[test]
    fn gradient_background_layers_clip_to_their_own_rounded_areas() {
        let output = parse_document("<!doctype html><body><div id=box></div></body>");
        let sheet = parse_stylesheet(
            "html, body { display:block; margin:0 } #box { display:block; width:80px; height:40px; border:2px solid transparent; border-radius:12px; background:linear-gradient(#ffffff,#ffffff) padding-box, linear-gradient(#3377fe,#ba59ff) border-box }",
        );
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        let formatting = build_formatting_tree(&output.dom, &styles, &FormattingLimits::default());
        let layout = layout_formatting_tree(
            &output.dom,
            &formatting,
            &styles,
            LayoutOptions::default(),
            &SimpleTextMeasurer,
        );
        let display = build_display_list(
            &layout.fragments,
            &formatting,
            &styles,
            DisplayListBuilderOptions::default(),
            &ReferenceTextShaper,
        );
        let selector = parse_selector_list("#box").unwrap();
        let box_node = select_all(
            &output.dom,
            output.dom.document(),
            &selector,
            &MatchContext::default(),
        )[0];

        let clips = display
            .list
            .items()
            .iter()
            .filter(|item| item.source == Some(box_node))
            .filter_map(|item| match &item.command {
                DisplayCommand::PushClip(ClipShape::RoundedRect { rect, radii }) => {
                    Some((*rect, *radii))
                }
                _ => None,
            })
            .collect::<Vec<_>>();

        // Border-box layer clip: the rounded border rect.
        assert!(
            clips.iter().any(|(rect, radii)| {
                *rect == PhysicalRect::new(0.0, 0.0, 84.0, 44.0)
                    && (radii.top_left - 12.0).abs() < f32::EPSILON
            }),
            "missing rounded border-box clip: {clips:?}"
        );
        // Padding-box layer clip: the rounded padding rect with inset radii.
        assert!(
            clips.iter().any(|(rect, radii)| {
                *rect == PhysicalRect::new(2.0, 2.0, 80.0, 40.0)
                    && (radii.top_left - 10.0).abs() < f32::EPSILON
            }),
            "missing rounded padding-box clip: {clips:?}"
        );
    }

    #[test]
    fn display_list_contains_background_border_glyphs_and_opacity_group() {
        let output = parse_document("<!doctype html><body><p>paint me</p></body>");
        let sheet = parse_stylesheet(
            "html, body, p { display:block } html { background-color:#102030 } p { background-color:#336699; color:white; opacity:.5; border-left-width:2px; border-left-style:solid; border-left-color:red }",
        );
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        let formatting = build_formatting_tree(&output.dom, &styles, &FormattingLimits::default());
        let layout = layout_formatting_tree(
            &output.dom,
            &formatting,
            &styles,
            LayoutOptions::default(),
            &SimpleTextMeasurer,
        );
        let display = build_display_list(
            &layout.fragments,
            &formatting,
            &styles,
            DisplayListBuilderOptions::default(),
            &ReferenceTextShaper,
        );
        assert!(display.diagnostics.is_empty());
        assert!(
            display
                .list
                .items()
                .iter()
                .any(|item| { matches!(item.command, DisplayCommand::SolidRect { .. }) })
        );
        assert!(
            display
                .list
                .items()
                .iter()
                .any(|item| { matches!(item.command, DisplayCommand::Border(_)) })
        );
        assert!(
            display
                .list
                .items()
                .iter()
                .any(|item| { matches!(item.command, DisplayCommand::GlyphRun(_)) })
        );
        assert!(
            display
                .list
                .items()
                .iter()
                .any(|item| { matches!(item.command, DisplayCommand::PushStackingContext(_)) })
        );
        assert!(display.list.items().iter().any(|item| {
            matches!(
                item.command,
                DisplayCommand::SolidRect { rect, color }
                    if rect == PhysicalRect::new(0.0, 0.0, 1_280.0, 720.0)
                        && color == Color::rgb(0x10, 0x20, 0x30)
            )
        }));
    }

    #[test]
    fn atomic_inline_box_emits_its_own_background_and_border() {
        let output = parse_document("<!doctype html><body>before<a id=tile>inside</a>after</body>");
        let sheet = parse_stylesheet(
            "html, body { display:block; margin:0 } #tile { display:inline-block; width:80px; height:20px; padding-left:4px; padding-right:4px; background-color:#123456; border-left-width:2px; border-left-style:solid; border-right-width:2px; border-right-style:solid }",
        );
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        let formatting = build_formatting_tree(&output.dom, &styles, &FormattingLimits::default());
        let layout = layout_formatting_tree(
            &output.dom,
            &formatting,
            &styles,
            LayoutOptions::default(),
            &SimpleTextMeasurer,
        );
        let display = build_display_list(
            &layout.fragments,
            &formatting,
            &styles,
            DisplayListBuilderOptions::default(),
            &ReferenceTextShaper,
        );
        let selector = parse_selector_list("#tile").unwrap();
        let tile = select_all(
            &output.dom,
            output.dom.document(),
            &selector,
            &MatchContext::default(),
        )[0];

        assert!(display.list.items().iter().any(|item| {
            item.source == Some(tile)
                && matches!(
                    item.command,
                    DisplayCommand::SolidRect { rect, color }
                        if (rect.size.width - 92.0).abs() < f32::EPSILON
                            && color == Color::rgb(0x12, 0x34, 0x56)
                )
        }));
        assert!(display.list.items().iter().any(|item| {
            item.source == Some(tile) && matches!(item.command, DisplayCommand::Border(_))
        }));
    }

    #[test]
    fn linear_gradient_background_becomes_a_paint_command() {
        let output =
            parse_document("<!doctype html><body><button id=search>百度一�?/button></body>");
        let sheet = parse_stylesheet(
            "html, body { display:block; margin:0 } #search { display:block; width:120px; height:40px; color:white; background:linear-gradient(90deg, #286aff, #9f66ff); }",
        );
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        let formatting = build_formatting_tree(&output.dom, &styles, &FormattingLimits::default());
        let layout = layout_formatting_tree(
            &output.dom,
            &formatting,
            &styles,
            LayoutOptions::default(),
            &SimpleTextMeasurer,
        );
        let display = build_display_list(
            &layout.fragments,
            &formatting,
            &styles,
            DisplayListBuilderOptions::default(),
            &ReferenceTextShaper,
        );
        assert!(display.list.items().iter().any(|item| {
            matches!(
                &item.command,
                DisplayCommand::LinearGradient(gradient)
                    if gradient.stops.len() == 2
                        && gradient.rect.size.width > 0.0
                        && gradient.rect.size.height > 0.0
            )
        }));
    }

    #[test]
    fn rounded_box_emits_rounded_clip_and_box_shadow() {
        let output = parse_document("<!doctype html><body><div id=card>card</div></body>");
        let sheet = parse_stylesheet(
            "html, body { display:block; margin:0 } #card { display:block; width:120px; height:40px; border-radius:12px; background-color:#ffffff; box-shadow:0 4px 12px rgba(0,0,0,.25) }",
        );
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        let formatting = build_formatting_tree(&output.dom, &styles, &FormattingLimits::default());
        let layout = layout_formatting_tree(
            &output.dom,
            &formatting,
            &styles,
            LayoutOptions::default(),
            &SimpleTextMeasurer,
        );
        let display = build_display_list(
            &layout.fragments,
            &formatting,
            &styles,
            DisplayListBuilderOptions::default(),
            &ReferenceTextShaper,
        );

        assert!(display.diagnostics.is_empty());
        assert!(display.list.items().iter().any(|item| {
            matches!(
                &item.command,
                DisplayCommand::PushClip(ClipShape::RoundedRect { radii, .. })
                    if radii.top_left > 0.0
            )
        }));
        assert!(display.list.items().iter().any(|item| {
            matches!(
                &item.command,
                DisplayCommand::BoxShadow(shadow)
                    if (shadow.blur_radius - 12.0).abs() < f32::EPSILON
                        && (shadow.offset.y - 4.0).abs() < f32::EPSILON
                        && shadow.color == Color::rgba(0, 0, 0, 64)
            )
        }));
    }

    #[test]
    fn overflow_hidden_wraps_descendants_in_a_padding_clip() {
        let output =
            parse_document("<!doctype html><body><div id=clip><span>child</span></div></body>");
        let sheet = parse_stylesheet(
            "html, body { display:block; margin:0 } #clip { display:block; width:100px; height:20px; padding:4px; overflow-x:hidden; overflow-y:hidden; background-color:#123456 }",
        );
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        let formatting = build_formatting_tree(&output.dom, &styles, &FormattingLimits::default());
        let layout = layout_formatting_tree(
            &output.dom,
            &formatting,
            &styles,
            LayoutOptions::default(),
            &SimpleTextMeasurer,
        );
        let display = build_display_list(
            &layout.fragments,
            &formatting,
            &styles,
            DisplayListBuilderOptions::default(),
            &ReferenceTextShaper,
        );
        let selector = parse_selector_list("#clip").unwrap();
        let clip_node = select_all(
            &output.dom,
            output.dom.document(),
            &selector,
            &MatchContext::default(),
        )[0];
        let items: Vec<_> = display
            .list
            .items()
            .iter()
            .filter(|item| item.source == Some(clip_node))
            .collect();
        assert!(items.iter().any(|item| {
            matches!(item.command, DisplayCommand::PushClip(ClipShape::Rect(rect)) if rect.size.width > 0.0 && rect.size.height > 0.0)
        }));
        assert!(
            items
                .iter()
                .any(|item| matches!(item.command, DisplayCommand::PopClip))
        );
    }

    #[test]
    fn overflow_visible_does_not_emit_a_clip() {
        let output = parse_document("<!doctype html><body><div id=box>child</div></body>");
        let sheet = parse_stylesheet(
            "html, body { display:block; margin:0 } #box { display:block; overflow:visible }",
        );
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        let formatting = build_formatting_tree(&output.dom, &styles, &FormattingLimits::default());
        let layout = layout_formatting_tree(
            &output.dom,
            &formatting,
            &styles,
            LayoutOptions::default(),
            &SimpleTextMeasurer,
        );
        let display = build_display_list(
            &layout.fragments,
            &formatting,
            &styles,
            DisplayListBuilderOptions::default(),
            &ReferenceTextShaper,
        );
        assert!(!display.list.items().iter().any(|item| matches!(
            item.command,
            DisplayCommand::PushClip(_) | DisplayCommand::PopClip
        )));
    }

    fn decoded_cover_resources(cover: NodeId) -> crate::image::ImageResources {
        let mut images = crate::image::ImageResources::default();
        let key = crate::image::ImageResourceKey {
            owner: cover,
            requested_url: url::Url::parse("https://example.test/cover.png").unwrap(),
            source_snapshot: String::new(),
            source: crate::image::ImageSource::Element,
            selection_context: crate::image::ImageSelectionContext::default(),
        };
        let decoded = crate::image::DecodedImage::from_pixels(
            672,
            378,
            vec![Color::rgb(255, 0, 0); 672 * 378],
        )
        .unwrap();
        images
            .insert(key, decoded, crate::image::ImageLimits::default())
            .unwrap();
        images
    }

    fn cover_image_paints(
        display: &crate::paint::DisplayListBuildOutput,
        cover: NodeId,
    ) -> Vec<&ImagePaint> {
        display
            .list
            .items()
            .iter()
            .filter(|item| item.source == Some(cover))
            .filter_map(|item| match &item.command {
                DisplayCommand::Image(paint) => Some(paint),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn padding_top_hack_wrapper_paints_decoded_absolute_image() {
        let output = parse_document(
            "<!doctype html><body><div id=wrap><img id=cover src='cover.png'></div></body>",
        );
        let sheet = parse_stylesheet(
            "html, body { display:block; margin:0 } \
             #wrap { display:block; position:relative; width:400px; padding-top:56.25%; background-color:#f1f2f3 } \
             #cover { position:absolute; top:0; left:0; display:block; width:100%; height:100% }",
        );
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        let formatting = build_formatting_tree(&output.dom, &styles, &FormattingLimits::default());
        let layout = layout_formatting_tree(
            &output.dom,
            &formatting,
            &styles,
            LayoutOptions {
                viewport: crate::layout::PhysicalSize {
                    width: 400.0,
                    height: 300.0,
                },
                ..LayoutOptions::default()
            },
            &SimpleTextMeasurer,
        );
        let selector = parse_selector_list("#cover").unwrap();
        let cover = select_all(
            &output.dom,
            output.dom.document(),
            &selector,
            &MatchContext::default(),
        )[0];
        let images = decoded_cover_resources(cover);
        let display = build_display_list_with_images(
            &layout.fragments,
            &formatting,
            &styles,
            DisplayListBuilderOptions::default(),
            &ReferenceTextShaper,
            Some(&images),
        );
        assert!(display.diagnostics.is_empty());

        // The wrapper's padding-top hack gives it a 400x225 padding box while
        // its content box stays empty; the absolute cover must still draw the
        // decoded bitmap across that box.
        let paints = cover_image_paints(&display, cover);
        assert_eq!(paints.len(), 1);
        assert_eq!(
            paints[0].destination,
            PhysicalRect::new(0.0, 0.0, 400.0, 225.0)
        );
        assert!(paints[0].destination.size.width > 0.0);
        assert!(paints[0].destination.size.height > 0.0);
        assert_eq!(paints[0].source, PhysicalRect::new(0.0, 0.0, 672.0, 378.0));
        assert_eq!(
            images
                .get(paints[0].resource)
                .expect("decoded bitmap stays registered")
                .intrinsic_size(),
            (672, 378)
        );
    }

    #[test]
    fn absolute_cover_image_survives_a_static_intermediate_wrapper() {
        let output = parse_document(
            "<!doctype html><body><div id=wrap><a id=link><img id=cover src='cover.png'></a></div></body>",
        );
        let sheet = parse_stylesheet(
            "html, body { display:block; margin:0 } \
             #wrap { display:block; position:relative; width:400px; padding-top:56.25%; background-color:#f1f2f3 } \
             #link { display:block } \
             #cover { position:absolute; top:0; left:0; display:block; width:100%; height:100% }",
        );
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        let formatting = build_formatting_tree(&output.dom, &styles, &FormattingLimits::default());
        let layout = layout_formatting_tree(
            &output.dom,
            &formatting,
            &styles,
            LayoutOptions {
                viewport: crate::layout::PhysicalSize {
                    width: 400.0,
                    height: 300.0,
                },
                ..LayoutOptions::default()
            },
            &SimpleTextMeasurer,
        );
        let selector = parse_selector_list("#cover").unwrap();
        let cover = select_all(
            &output.dom,
            output.dom.document(),
            &selector,
            &MatchContext::default(),
        )[0];
        let images = decoded_cover_resources(cover);
        let display = build_display_list_with_images(
            &layout.fragments,
            &formatting,
            &styles,
            DisplayListBuilderOptions::default(),
            &ReferenceTextShaper,
            Some(&images),
        );
        assert!(display.diagnostics.is_empty());

        // The static intermediate collapses, so the absolute cover's laid-out
        // box is degenerate. Paint must still recover the wrapper's padding
        // box (the cover's CSS containing block) and draw the bitmap.
        let paints = cover_image_paints(&display, cover);
        assert_eq!(paints.len(), 1);
        assert_eq!(
            paints[0].destination,
            PhysicalRect::new(0.0, 0.0, 400.0, 225.0)
        );
        assert!(paints[0].destination.size.width > 0.0);
        assert!(paints[0].destination.size.height > 0.0);
        assert_eq!(
            images
                .get(paints[0].resource)
                .expect("decoded bitmap stays registered")
                .intrinsic_size(),
            (672, 378)
        );
    }

    #[test]
    fn overflow_hidden_picture_over_the_padding_hack_keeps_the_cover_visible() {
        let output = parse_document(
            "<!doctype html><body><div id=wrap><picture id=frame><img id=cover src='cover.png'></picture></div></body>",
        );
        let sheet = parse_stylesheet(
            "html, body { display:block; margin:0 } \
             #wrap { display:block; position:relative; width:298px; padding-top:56.25%; background-color:#f1f2f3 } \
             #frame { position:absolute; top:0; left:0; display:block; width:100%; height:100%; overflow:hidden } \
             #cover { display:block; width:100%; height:100% }",
        );
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        let formatting = build_formatting_tree(&output.dom, &styles, &FormattingLimits::default());
        let layout = layout_formatting_tree(
            &output.dom,
            &formatting,
            &styles,
            LayoutOptions::default(),
            &SimpleTextMeasurer,
        );
        let selector = parse_selector_list("#cover").unwrap();
        let cover = select_all(
            &output.dom,
            output.dom.document(),
            &selector,
            &MatchContext::default(),
        )[0];
        let frame_selector = parse_selector_list("#frame").unwrap();
        let frame = select_all(
            &output.dom,
            output.dom.document(),
            &frame_selector,
            &MatchContext::default(),
        )[0];
        let images = decoded_cover_resources(cover);
        let display = build_display_list_with_images(
            &layout.fragments,
            &formatting,
            &styles,
            DisplayListBuilderOptions::default(),
            &ReferenceTextShaper,
            Some(&images),
        );
        assert!(display.diagnostics.is_empty());

        // 56.25% of the 1280px viewport width is a 720px-tall padding box.
        let paints = cover_image_paints(&display, cover);
        assert_eq!(paints.len(), 1);
        assert_eq!(
            paints[0].destination,
            PhysicalRect::new(0.0, 0.0, 298.0, 720.0)
        );
        assert!(paints[0].destination.size.width > 0.0);
        assert!(paints[0].destination.size.height > 0.0);
        let clips = display
            .list
            .items()
            .iter()
            .filter(|item| item.source == Some(frame))
            .filter_map(|item| match &item.command {
                DisplayCommand::PushClip(ClipShape::Rect(rect)) => Some(*rect),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(
            clips.contains(&PhysicalRect::new(0.0, 0.0, 298.0, 720.0)),
            "picture overflow clip must cover the padding box: {clips:?}"
        );
    }

    #[test]
    fn paint_recovers_a_degenerate_percentage_cover_box() {
        let output = parse_document(
            "<!doctype html><body><div id=wrap><img id=cover src='cover.png'></div></body>",
        );
        let sheet = parse_stylesheet(
            "html, body { display:block; margin:0 } \
             #wrap { display:block; position:relative; width:400px; padding-top:56.25%; background-color:#f1f2f3 } \
             #cover { position:absolute; top:0; left:0; display:block; width:100%; height:100% }",
        );
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        let formatting = build_formatting_tree(&output.dom, &styles, &FormattingLimits::default());
        let layout = layout_formatting_tree(
            &output.dom,
            &formatting,
            &styles,
            LayoutOptions {
                viewport: crate::layout::PhysicalSize {
                    width: 400.0,
                    height: 300.0,
                },
                ..LayoutOptions::default()
            },
            &SimpleTextMeasurer,
        );
        let selector = parse_selector_list("#cover").unwrap();
        let cover = select_all(
            &output.dom,
            output.dom.document(),
            &selector,
            &MatchContext::default(),
        )[0];

        // Simulate a layout that collapses the cover's percentage height
        // against the wrapper's empty content box: the cover fragment ends up
        // degenerate at the bottom edge of the padding box.
        let mut fragments = layout.fragments.iter().cloned().collect::<Vec<_>>();
        for fragment in &mut fragments {
            if fragment.source == Some(cover) {
                if let FragmentKind::Box(geometry) = &mut fragment.kind {
                    geometry.content_rect = PhysicalRect::new(0.0, 225.0, 400.0, 0.0);
                }
                fragment.rect = PhysicalRect::new(0.0, 225.0, 400.0, 0.0);
            }
        }
        let degraded = crate::layout::FragmentTree::new(
            layout.fragments.dom_revision,
            layout.fragments.viewport,
            layout.fragments.root(),
            fragments,
        );

        let images = decoded_cover_resources(cover);
        let display = build_display_list_with_images(
            &degraded,
            &formatting,
            &styles,
            DisplayListBuilderOptions::default(),
            &ReferenceTextShaper,
            Some(&images),
        );
        assert!(display.diagnostics.is_empty());

        // Paint restores the wrapper's padding box as the cover's content box
        // and still emits the bitmap draw.
        let paints = cover_image_paints(&display, cover);
        assert_eq!(paints.len(), 1);
        assert_eq!(
            paints[0].destination,
            PhysicalRect::new(0.0, 0.0, 400.0, 225.0)
        );
        assert_eq!(paints[0].source, PhysicalRect::new(0.0, 0.0, 672.0, 378.0));
    }

    #[test]
    fn object_fit_preserves_aspect_ratio_and_centers_content() {
        let content = PhysicalRect::new(10.0, 20.0, 200.0, 100.0);
        assert_eq!(
            Builder::object_fit_rect(content, 100.0, 100.0, ObjectFit::Contain),
            PhysicalRect::new(60.0, 20.0, 100.0, 100.0)
        );
        assert_eq!(
            Builder::object_fit_rect(content, 100.0, 100.0, ObjectFit::Cover),
            PhysicalRect::new(10.0, -30.0, 200.0, 200.0)
        );
        assert_eq!(
            Builder::object_fit_rect(content, 500.0, 100.0, ObjectFit::ScaleDown),
            PhysicalRect::new(10.0, 50.0, 200.0, 40.0)
        );
    }

    /// Builds the display list for a fixed two-box document styled by `css`,
    /// returning the parsed DOM and the list for per-node assertions.
    fn transform_display(css: &str) -> (ParseOutput, DisplayListBuildOutput) {
        let output =
            parse_document("<!doctype html><body><div id=box><div id=inner>x</div></div></body>");
        let sheet = parse_stylesheet(css);
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        let formatting = build_formatting_tree(&output.dom, &styles, &FormattingLimits::default());
        let layout = layout_formatting_tree(
            &output.dom,
            &formatting,
            &styles,
            LayoutOptions::default(),
            &SimpleTextMeasurer,
        );
        let display = build_display_list(
            &layout.fragments,
            &formatting,
            &styles,
            DisplayListBuilderOptions::default(),
            &ReferenceTextShaper,
        );
        assert!(display.diagnostics.is_empty(), "{:?}", display.diagnostics);
        (output, display)
    }

    fn node_contexts<'a>(
        output: &ParseOutput,
        selector: &str,
        display: &'a DisplayListBuildOutput,
    ) -> Vec<&'a StackingContext> {
        let selector = parse_selector_list(selector).unwrap();
        let node = select_all(
            &output.dom,
            output.dom.document(),
            &selector,
            &MatchContext::default(),
        )[0];
        display
            .list
            .items()
            .iter()
            .filter(|item| {
                item.source == Some(node)
                    && matches!(item.command, DisplayCommand::PushStackingContext(_))
            })
            .map(|item| match &item.command {
                DisplayCommand::PushStackingContext(context) => context,
                _ => unreachable!("filtered above"),
            })
            .collect()
    }

    #[test]
    fn transform_translate_resolves_percentages_against_the_border_box() {
        let (output, display) = transform_display(
            "html, body, #box, #inner { display:block; margin:0 } \
             #box { width:100px; height:50px; transform: translate(50%, 20px) }",
        );
        let contexts = node_contexts(&output, "#box", &display);
        assert_eq!(contexts.len(), 1);
        assert_eq!(contexts[0].reason, CompositingReason::Transform);
        assert!((contexts[0].opacity - 1.0).abs() < f32::EPSILON);
        assert!(contexts[0].transform.is_translation());
        let (tx, ty) = contexts[0].transform.translation();
        assert!((tx - 50.0).abs() < 1e-4, "{tx}");
        assert!((ty - 20.0).abs() < 1e-4, "{ty}");
    }

    #[test]
    fn transform_rotate_uses_the_border_box_center_by_default() {
        let (output, display) = transform_display(
            "html, body, #box, #inner { display:block; margin:0 } \
             #box { width:100px; height:50px; transform: rotate(90deg) }",
        );
        let contexts = node_contexts(&output, "#box", &display);
        assert_eq!(contexts.len(), 1);
        let t = contexts[0].transform;
        // rotate(90deg) about (50, 25): (0, 0) maps to (75, -25).
        assert!((t.scale_x - 0.0).abs() < 1e-4, "{t:?}");
        assert!((t.skew_x - -1.0).abs() < 1e-4, "{t:?}");
        assert!((t.skew_y - 1.0).abs() < 1e-4, "{t:?}");
        assert!((t.scale_y - 0.0).abs() < 1e-4, "{t:?}");
        assert!((t.translate_x - 75.0).abs() < 1e-4, "{t:?}");
        assert!((t.translate_y - -25.0).abs() < 1e-4, "{t:?}");
    }

    #[test]
    fn transform_origin_left_top_rebases_rotation_around_the_corner() {
        let (output, display) = transform_display(
            "html, body, #box, #inner { display:block; margin:0 } \
             #box { width:100px; height:50px; transform: rotate(90deg); transform-origin: left top }",
        );
        let contexts = node_contexts(&output, "#box", &display);
        assert_eq!(contexts.len(), 1);
        let t = contexts[0].transform;
        // With the origin at the corner the rotation keeps (0, 0) fixed, so
        // the matrix is the bare rotation with zero translation.
        assert!((t.scale_x - 0.0).abs() < 1e-4, "{t:?}");
        assert!((t.skew_x - -1.0).abs() < 1e-4, "{t:?}");
        assert!((t.skew_y - 1.0).abs() < 1e-4, "{t:?}");
        assert!((t.scale_y - 0.0).abs() < 1e-4, "{t:?}");
        assert!((t.translate_x - 0.0).abs() < 1e-4, "{t:?}");
        assert!((t.translate_y - 0.0).abs() < 1e-4, "{t:?}");
    }

    #[test]
    fn transform_none_and_absent_transform_emit_no_stacking_context() {
        for css in [
            "html, body, #box, #inner { display:block; margin:0 } \
             #box { width:100px; height:50px; transform: none }",
            "html, body, #box, #inner { display:block; margin:0 } \
             #box { width:100px; height:50px }",
        ] {
            let (_output, display) = transform_display(css);
            assert!(
                display
                    .list
                    .items()
                    .iter()
                    .all(|item| !matches!(item.command, DisplayCommand::PushStackingContext(_))),
                "unexpected stacking context for {css}"
            );
        }
    }

    #[test]
    fn opacity_and_transform_share_a_single_stacking_context() {
        let (output, display) = transform_display(
            "html, body, #box, #inner { display:block; margin:0 } \
             #box { width:100px; height:50px; opacity:0.5; transform: translate(10px, 0) }",
        );
        let contexts = node_contexts(&output, "#box", &display);
        assert_eq!(contexts.len(), 1);
        assert!((contexts[0].opacity - 0.5).abs() < 1e-6);
        assert!(!contexts[0].transform.is_identity());
        assert_eq!(contexts[0].reason, CompositingReason::Transform);
    }

    #[test]
    fn transform_group_wraps_child_commands() {
        let (output, display) = transform_display(
            "html, body, #box, #inner { display:block; margin:0 } \
             #box { width:100px; height:50px; transform: translate(10px, 0) } \
             #inner { width:10px; height:10px; background-color:#123456 }",
        );
        let selector = parse_selector_list("#box").unwrap();
        let box_node = select_all(
            &output.dom,
            output.dom.document(),
            &selector,
            &MatchContext::default(),
        )[0];
        let selector = parse_selector_list("#inner").unwrap();
        let inner_node = select_all(
            &output.dom,
            output.dom.document(),
            &selector,
            &MatchContext::default(),
        )[0];
        let items = display.list.items();
        let push_index = items
            .iter()
            .position(|item| {
                item.source == Some(box_node)
                    && matches!(item.command, DisplayCommand::PushStackingContext(_))
            })
            .expect("transform push");
        let pop_index = items
            .iter()
            .position(|item| {
                item.source == Some(box_node)
                    && matches!(item.command, DisplayCommand::PopStackingContext)
            })
            .expect("transform pop");
        let child_index = items
            .iter()
            .position(|item| {
                item.source == Some(inner_node)
                    && matches!(item.command, DisplayCommand::SolidRect { .. })
            })
            .expect("child background");
        assert!(push_index < child_index && child_index < pop_index);
    }

    #[test]
    fn transform2d_math_composes_inverts_and_classifies() {
        let translation = Transform2D {
            translate_x: 5.0,
            translate_y: 7.0,
            ..Transform2D::default()
        };
        assert!(translation.is_translation());
        let scale = Transform2D {
            scale_x: 2.0,
            scale_y: 3.0,
            ..Transform2D::default()
        };
        assert!(!scale.is_translation());
        assert_eq!(scale.apply(4.0, 5.0), (8.0, 15.0));

        // `then` applies its argument first: scale �?translation.
        let composed = scale.then(&translation);
        assert_eq!(composed.apply(1.0, 1.0), (12.0, 24.0));

        let inverse = composed.inverse().expect("invertible");
        let (x, y) = inverse.apply(12.0, 24.0);
        assert!((x - 1.0).abs() < 1e-4 && (y - 1.0).abs() < 1e-4);
        assert!(
            Transform2D {
                scale_x: 0.0,
                ..Transform2D::default()
            }
            .inverse()
            .is_none()
        );
        assert!(Transform2D::default().is_identity());
    }

    /// Builds a display list from an arbitrary body and one author sheet.
    fn paint_body(html: &str, css: &str) -> (ParseOutput, DisplayListBuildOutput) {
        let output = parse_document(&format!("<!doctype html><body>{html}</body>"));
        let sheet = parse_stylesheet(css);
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        let formatting = build_formatting_tree(&output.dom, &styles, &FormattingLimits::default());
        let layout = layout_formatting_tree(
            &output.dom,
            &formatting,
            &styles,
            LayoutOptions::default(),
            &SimpleTextMeasurer,
        );
        let display = build_display_list(
            &layout.fragments,
            &formatting,
            &styles,
            DisplayListBuilderOptions::default(),
            &ReferenceTextShaper,
        );
        assert!(display.diagnostics.is_empty(), "{:?}", display.diagnostics);
        (output, display)
    }

    /// The reference shaper maps one glyph per `char`, so a run's text is
    /// exactly its glyph ids read back as characters.
    fn run_text(run: &GlyphRun) -> String {
        run.glyphs
            .iter()
            .map(|glyph| char::from_u32(glyph.glyph.0).unwrap_or('\u{fffd}'))
            .collect()
    }

    /// Text runs carry the text node as their source, so tests select them by
    /// their content rather than by an element id.
    fn text_runs(display: &DisplayListBuildOutput) -> Vec<GlyphRun> {
        display
            .list
            .items()
            .iter()
            .filter_map(|item| match &item.command {
                DisplayCommand::GlyphRun(run) => Some(run.clone()),
                _ => None,
            })
            .collect()
    }

    fn decorations(display: &DisplayListBuildOutput) -> Vec<TextDecoration> {
        display
            .list
            .items()
            .iter()
            .filter_map(|item| match &item.command {
                DisplayCommand::TextDecoration(decoration) => Some(*decoration),
                _ => None,
            })
            .collect()
    }

    fn shadows(display: &DisplayListBuildOutput) -> Vec<TextShadowPaint> {
        display
            .list
            .items()
            .iter()
            .filter_map(|item| match &item.command {
                DisplayCommand::TextShadow(shadow) => Some(*shadow),
                _ => None,
            })
            .collect()
    }

    fn markers(display: &DisplayListBuildOutput) -> Vec<ListMarkerPaint> {
        display
            .list
            .items()
            .iter()
            .filter_map(|item| match &item.command {
                DisplayCommand::ListMarker(marker) => Some(*marker),
                _ => None,
            })
            .collect()
    }

    fn marker_labels(display: &DisplayListBuildOutput) -> Vec<String> {
        text_runs(display)
            .iter()
            .map(run_text)
            .filter(|text| text.ends_with('.'))
            .collect()
    }

    #[test]
    fn underline_paints_one_stroke_spanning_the_run() {
        let (_, display) = paint_body(
            "<p id=t>Hi</p>",
            "html, body { display:block; margin:0 } \
             #t { display:block; text-decoration-line:underline }",
        );
        let runs = text_runs(&display);
        assert_eq!(runs.len(), 1, "{runs:?}");
        let found = decorations(&display);
        assert_eq!(found.len(), 1, "{found:?}");
        let decoration = found[0];
        assert_eq!(decoration.line, TextDecorationLine::Underline);
        assert_eq!(decoration.style, TextDecorationStyle::Solid);
        assert_eq!(decoration.color, Color::rgb(0, 0, 0));
        // An automatic thickness at the 16px default font size is one pixel.
        assert_eq!(decoration.thickness, 1.0);
        assert_eq!(decoration.rect.size.height, 1.0);
        // The stroke spans the whole run and sits just below its baseline.
        let first = runs[0].glyphs.first().expect("run has glyphs");
        let last = runs[0].glyphs.last().expect("run has glyphs");
        assert_eq!(decoration.rect.origin.x, first.position.x);
        assert_eq!(
            decoration.rect.size.width,
            last.position.x + last.advance - first.position.x
        );
        assert!(
            decoration.rect.origin.y >= first.position.y,
            "underline must not cross the baseline: {:?} vs {}",
            decoration.rect,
            first.position.y
        );
    }

    #[test]
    fn overline_and_line_through_bracket_the_baseline() {
        let (_, display) = paint_body(
            "<p id=t>Hi</p>",
            "html, body { display:block; margin:0 } \
             #t { display:block; text-decoration-line:overline line-through }",
        );
        let mut found = decorations(&display);
        found.sort_by_key(|decoration| match decoration.line {
            TextDecorationLine::Overline => 0,
            TextDecorationLine::LineThrough => 1,
            TextDecorationLine::Underline => 2,
        });
        assert_eq!(found.len(), 2, "{found:?}");
        let baseline = text_runs(&display)[0].glyphs[0].position.y;
        assert_eq!(found[0].line, TextDecorationLine::Overline);
        assert_eq!(found[1].line, TextDecorationLine::LineThrough);
        assert!(found[0].rect.origin.y < baseline);
        assert!(found[1].rect.origin.y < baseline);
        assert!(found[0].rect.origin.y < found[1].rect.origin.y);
    }

    #[test]
    fn decoration_colour_style_and_thickness_come_from_the_element() {
        let (_, display) = paint_body(
            "<p id=t>Hi</p>",
            "html, body { display:block; margin:0 } \
             #t { display:block; text-decoration-line:underline; \
                   text-decoration-color:#ff0000; text-decoration-style:dashed; \
                   text-decoration-thickness:3px }",
        );
        let found = decorations(&display);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].color, Color::rgb(255, 0, 0));
        assert_eq!(found[0].style, TextDecorationStyle::Dashed);
        assert_eq!(found[0].thickness, 3.0);
        assert_eq!(found[0].rect.size.height, 3.0);
    }

    #[test]
    fn decoration_reaches_runs_inside_descendant_inlines() {
        // CSS Text Decoration 3 §2: a decoration set on a box is propagated to
        // every line box it contains. The cascade does not implement that
        // propagation, so paint walks formatting ancestors; this is the case
        // that walk exists for.
        let (_, display) = paint_body(
            "<p id=p><em><span id=t>Hi</span></em></p>",
            "html, body, p { display:block; margin:0 } \
             #p { text-decoration-line:underline }",
        );
        let found = decorations(&display);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].line, TextDecorationLine::Underline);
    }

    #[test]
    fn an_undecorated_run_paints_no_decoration_or_shadow() {
        let (_, display) = paint_body(
            "<p id=t>Hi</p>",
            "html, body { display:block; margin:0 } #t { display:block }",
        );
        assert!(decorations(&display).is_empty());
        assert!(shadows(&display).is_empty());
    }

    #[test]
    fn text_shadow_layers_become_commands_ahead_of_their_run() {
        let (_, display) = paint_body(
            "<p id=t>Hi</p>",
            "html, body { display:block; margin:0 } \
             #t { display:block; text-shadow:2px 3px 4px #ff0000, 1px 1px #000000 }",
        );
        let found = shadows(&display);
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].offset, PhysicalPoint { x: 2.0, y: 3.0 });
        assert_eq!(found[0].blur_radius, 4.0);
        assert_eq!(found[0].color, Color::rgb(255, 0, 0));
        assert_eq!(found[1].offset, PhysicalPoint { x: 1.0, y: 1.0 });
        assert_eq!(found[1].blur_radius, 0.0);
        assert_eq!(found[1].color, Color::rgb(0, 0, 0));

        // Each shadow is emitted before the run it describes, and its damage
        // bounds cover the run grown by the blur.
        let run_index = display
            .list
            .items()
            .iter()
            .position(|item| matches!(item.command, DisplayCommand::GlyphRun(_)))
            .expect("run painted");
        let shadow_indices = display
            .list
            .items()
            .iter()
            .enumerate()
            .filter(|(_, item)| matches!(item.command, DisplayCommand::TextShadow(_)))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        assert_eq!(shadow_indices.len(), 2);
        for index in shadow_indices {
            let item = &display.list.items()[index];
            let DisplayCommand::TextShadow(shadow) = item.command else {
                unreachable!("filtered to shadows");
            };
            assert!(index < run_index, "a shadow must precede its run");
            assert_eq!(shadow.run, item.id.fragment_hint);
            assert!(item.bounds.size.width > 0.0 && item.bounds.size.height > 0.0);
        }
    }

    #[test]
    fn unordered_items_paint_a_disc_beside_the_content_edge() {
        let (_, display) = paint_body(
            "<ul><li>Hi</li></ul>",
            "html, body { display:block; margin:0 } \
             ul { display:block; padding-left:40px; list-style-type:disc } \
             li { display:list-item }",
        );
        let found = markers(&display);
        assert_eq!(found.len(), 1, "{found:?}");
        let marker = found[0];
        assert_eq!(marker.shape, ListMarkerShape::Disc);
        // A quarter-em bullet at 16px, and clear of the item's own text.
        assert_eq!(marker.rect.size.width, 4.0);
        assert_eq!(marker.rect.size.height, 4.0);
        let text_x = text_runs(&display)[0].glyphs[0].position.x;
        assert!(
            marker.rect.right() < text_x,
            "outside marker must precede the content: {:?} vs {text_x}",
            marker.rect
        );
        assert_eq!(marker.color, Color::rgb(0, 0, 0));
    }

    #[test]
    fn ordered_items_paint_their_ordinal() {
        let (_, display) = paint_body(
            "<ol><li>one</li><li>two</li><li>three</li></ol>",
            "html, body { display:block; margin:0 } \
             ol { display:block; padding-left:40px; list-style-type:decimal } \
             li { display:list-item }",
        );
        assert_eq!(marker_labels(&display), vec!["1.", "2.", "3."]);
        assert!(markers(&display).is_empty(), "ordered markers are text");
    }

    #[test]
    fn ordered_markers_follow_their_family() {
        let cases = [
            ("decimal-leading-zero", vec!["01.", "02.", "03."]),
            ("lower-alpha", vec!["a.", "b.", "c."]),
            ("upper-alpha", vec!["A.", "B.", "C."]),
            ("lower-roman", vec!["i.", "ii.", "iii."]),
            ("upper-roman", vec!["I.", "II.", "III."]),
            ("lower-latin", vec!["a.", "b.", "c."]),
            ("upper-latin", vec!["A.", "B.", "C."]),
        ];
        for (family, expected) in cases {
            let (_, display) = paint_body(
                "<ol><li>a</li><li>b</li><li>c</li></ol>",
                &format!(
                    "html, body {{ display:block; margin:0 }} \
                     ol {{ display:block; padding-left:40px; list-style-type:{family} }} \
                     li {{ display:list-item }}"
                ),
            );
            assert_eq!(marker_labels(&display), expected, "{family}");
        }
    }

    #[test]
    fn nested_lists_take_their_own_marker_family() {
        let (_, display) = paint_body(
            "<ul><li>a<ul><li>x</li></ul></li></ul>",
            "html, body { display:block; margin:0 } \
             ul { display:block; padding-left:40px } \
             li { display:list-item } \
             ul > li { list-style-type:disc } \
             ul ul > li { list-style-type:circle }",
        );
        let shapes = markers(&display)
            .iter()
            .map(|marker| marker.shape)
            .collect::<Vec<_>>();
        assert_eq!(shapes, vec![ListMarkerShape::Disc, ListMarkerShape::Circle]);
    }

    #[test]
    fn inside_markers_start_at_the_content_edge() {
        let (_, outside_display) = paint_body(
            "<ul><li>Hi</li></ul>",
            "html, body { display:block; margin:0 } \
             ul { display:block; padding-left:40px; list-style-type:disc } \
             li { display:list-item }",
        );
        let (_, inside_display) = paint_body(
            "<ul><li>Hi</li></ul>",
            "html, body { display:block; margin:0 } \
             ul { display:block; padding-left:40px; list-style-type:disc } \
             li { display:list-item; list-style-position:inside }",
        );
        let outside_marker = markers(&outside_display)[0].rect;
        let inside_marker = markers(&inside_display)[0].rect;
        let outside_text = text_runs(&outside_display)[0].glyphs[0].position.x;
        let inside_text = text_runs(&inside_display)[0].glyphs[0].position.x;
        assert!(outside_marker.origin.x < outside_text);
        assert_eq!(inside_marker.origin.x, inside_text);
    }

    #[test]
    fn no_marker_is_painted_when_the_item_loses_its_list_display() {
        let (_, display) = paint_body(
            "<ul><li>Hi</li></ul>",
            "html, body { display:block; margin:0 } \
             ul { display:block; padding-left:40px; list-style-type:disc } \
             li { display:block }",
        );
        assert!(markers(&display).is_empty());
    }

    #[test]
    fn list_style_type_none_paints_no_marker() {
        let (_, display) = paint_body(
            "<ul><li>Hi</li></ul>",
            "html, body { display:block; margin:0 } \
             ul { display:block; padding-left:40px; list-style-type:none } \
             li { display:list-item }",
        );
        assert!(markers(&display).is_empty());
    }
}
