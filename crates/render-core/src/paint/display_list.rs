//! Immutable retained display-list model and incremental diffing.

use std::collections::BTreeMap;

use crate::css::computed::ComputedStyle;
use crate::css::properties::{BorderStyle, CssColor, Position, TypedPropertyValue};
use crate::dom::{DomRevision, NodeId};
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
    let mut builder = Builder {
        fragments,
        formatting,
        styles,
        options,
        shaper,
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

        match &fragment.kind {
            FragmentKind::Box(geometry) => {
                self.paint_box(
                    &fragment,
                    geometry,
                    style.as_ref(),
                    current_color,
                    coordinate_space,
                );
            }
            FragmentKind::Text(text) => {
                self.paint_text(&fragment, text, current_color, coordinate_space);
            }
        }
        for child in &fragment.children {
            self.paint_fragment(*child, coordinate_space);
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
        if let Some(background) = style
            .and_then(|style| typed_color(style, "background-color"))
            .map(|color| self.options.palette.resolve(color, current_color))
            && background.alpha > 0
        {
            let (rect, background_space) =
                self.background_rect(fragment, geometry, coordinate_space);
            self.push(
                fragment,
                PaintPhase::Background,
                rect,
                background_space,
                DisplayCommand::SolidRect {
                    rect,
                    color: background,
                },
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
        radii: CornerRadii::default(),
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
    use crate::css::selector::MatchContext;
    use crate::css::stylesheet::parse_stylesheet;
    use crate::html::parse_document;
    use crate::layout::{
        FormattingLimits, LayoutOptions, PhysicalRect, SimpleTextMeasurer, build_formatting_tree,
        layout_formatting_tree,
    };
    use crate::paint::Color;

    use super::{
        DisplayCommand, DisplayListBuilderOptions, ReferenceTextShaper, build_display_list,
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
}
