//! Immutable retained display-list model and incremental diffing.

use std::collections::BTreeMap;

use crate::css::computed::ComputedStyle;
use crate::css::properties::{
    BorderStyle, CssColor, ObjectFit, Overflow, Position, TypedPropertyValue, Visibility,
    parse_typed_property,
};
use crate::dom::{DomRevision, NodeId};
use crate::image::ImageResources;
use crate::layout::{
    EdgeSizes, FormattingTree, Fragment, FragmentId, FragmentKind, FragmentTree, PhysicalPoint,
    PhysicalRect, PhysicalSize,
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

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextDecoration {
    pub rect: PhysicalRect,
    pub color: Color,
    pub line: TextDecorationLine,
    pub thickness: f32,
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
    let mut builder = Builder {
        fragments,
        formatting,
        styles,
        options,
        shaper,
        images,
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
        let hidden = matches!(
            style.as_ref().and_then(|style| style.typed("visibility")),
            Some(TypedPropertyValue::Visibility(
                Visibility::Hidden | Visibility::Collapse
            ))
        );
        if opacity < 1.0 {
            self.push(
                &fragment,
                PaintPhase::StackingContext,
                fragment.rect,
                coordinate_space,
                DisplayCommand::PushStackingContext(StackingContext {
                    opacity,
                    transform: Transform2D::default(),
                    blend_mode: BlendMode::Normal,
                    isolated: true,
                    reason: CompositingReason::Opacity,
                }),
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
                    self.paint_text(&fragment, text, current_color, coordinate_space);
                }
            }
        }
        let overflow_clip = match (&fragment.kind, style.as_ref()) {
            (FragmentKind::Box(geometry), Some(style)) => {
                if let Some(shape) = overflow_clip_shape(geometry, style) {
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
        if opacity < 1.0 {
            self.push(
                &fragment,
                PaintPhase::StackingContext,
                fragment.rect,
                coordinate_space,
                DisplayCommand::PopStackingContext,
            );
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
            })
            .collect::<Vec<_>>();
        if !gradient_layers.is_empty() {
            self.push(
                fragment,
                PaintPhase::Background,
                painting_area,
                coordinate_space,
                DisplayCommand::PushClip(ClipShape::Rect(painting_area)),
            );
            for gradient in gradient_layers.into_iter().rev() {
                self.push(
                    fragment,
                    PaintPhase::Background,
                    gradient.rect,
                    coordinate_space,
                    DisplayCommand::LinearGradient(gradient),
                );
            }
            self.push(
                fragment,
                PaintPhase::Background,
                painting_area,
                coordinate_space,
                DisplayCommand::PopClip,
            );
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

    fn paint_text(
        &mut self,
        fragment: &Fragment,
        text: &crate::layout::TextFragmentData,
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
        } else {
            self.push(
                fragment,
                PaintPhase::Content,
                fragment.rect,
                coordinate_space,
                DisplayCommand::GlyphRun(run),
            );
        }
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
        let (width, height) = loaded.image.intrinsic_size();
        if width == 0
            || height == 0
            || geometry.content_rect.size.width <= 0.0
            || geometry.content_rect.size.height <= 0.0
        {
            return;
        }
        let destination = Self::object_fit_rect(
            geometry.content_rect,
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
        let clips = destination != geometry.content_rect;
        if clips {
            self.push(
                fragment,
                PaintPhase::Content,
                geometry.content_rect,
                coordinate_space,
                DisplayCommand::PushClip(ClipShape::Rect(geometry.content_rect)),
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
                geometry.content_rect,
                coordinate_space,
                DisplayCommand::PopClip,
            );
        }
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

fn has_corner_radius(radii: CornerRadii) -> bool {
    radii.top_left > 0.0
        || radii.top_right > 0.0
        || radii.bottom_right > 0.0
        || radii.bottom_left > 0.0
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

fn typed_color(style: &ComputedStyle, property: &str) -> Option<CssColor> {
    match style.typed(property) {
        Some(TypedPropertyValue::Color(color)) => Some(*color),
        _ => None,
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
    use crate::css::cascade::{CascadeInput, CascadeOrigin};
    use crate::css::computed::{ComputationLimits, PropertyRegistry, compute_document_styles};
    use crate::css::properties::ObjectFit;
    use crate::css::selector::{MatchContext, parse_selector_list, select_all};
    use crate::css::stylesheet::parse_stylesheet;
    use crate::html::parse_document;
    use crate::layout::{
        FormattingLimits, LayoutOptions, PhysicalRect, SimpleTextMeasurer, build_formatting_tree,
        layout_formatting_tree,
    };
    use crate::paint::Color;

    use super::{
        Builder, ClipShape, DisplayCommand, DisplayListBuilderOptions, ReferenceTextShaper,
        build_display_list,
    };

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
            parse_document("<!doctype html><body><button id=search>百度一下</button></body>");
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
}
