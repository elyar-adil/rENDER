//! Deterministic reference layout for block and inline formatting contexts.

use crate::font::FontRequest;
use crate::fragment::BoxGeometry;
use crate::fragment::Fragment;
use crate::fragment::FragmentId;
use crate::fragment::FragmentKind;
use crate::fragment::FragmentTree;
use crate::geometry::EdgeSizes;
use crate::geometry::PhysicalRect;
use crate::geometry::PhysicalSize;
use crate::linebreak::LineBreakOptions;
use crate::scrollport::{ClipMode, ScrollportGeometry};
use crate::sticky::{StickyConstraint, StickyInsets};
use crate::tree::FormattingNodeId;
use crate::tree::FormattingNodeKind;
use crate::tree::FormattingTree;
use render_css::computed::ComputedStyle;
use render_css::properties::Float;
use render_dom::Dom;
use render_dom::NodeId;
use std::collections::BTreeMap;

mod block;
mod flex;
mod grid;
mod inline;
pub(crate) mod resolve;
mod table;

#[cfg(test)]
mod linebreak_tests;
#[cfg(test)]
mod scrollport_tests;
#[cfg(test)]
mod sticky_tests;
#[cfg(test)]
mod table_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod text_tests;
#[cfg(test)]
mod vanish_tests;

/// The typographic inputs one text run is measured with.
///
/// The family, weight and style are what CSS Fonts 4 §5.2 selects a face with.
/// They are read from the computed style once per element and borrowed rather
/// than owned, because this value is rebuilt for every typographic character
/// unit of an inline run; the owned copy that has to outlive the cascade lives
/// on [`crate::fragment::TextFragmentData`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextStyle<'a> {
    pub font_size: f32,
    pub line_height: f32,
    pub font: FontRequest<'a>,
}

impl Default for TextStyle<'_> {
    /// `font-size: 16px`, `line-height: normal`, and the initial font
    /// request. The two lengths are the values a document with no stylesheet
    /// and a 16px root font size produces.
    fn default() -> Self {
        Self {
            font_size: 16.0,
            line_height: 19.2,
            font: FontRequest::initial(),
        }
    }
}

/// The typographic spacing that changes a text run's advance width.
///
/// CSS Text 3 §7.1 (`word-spacing`) and §7.2 (`letter-spacing`) are part of
/// text *measurement*, not painting, so they travel beside [`TextStyle`] into
/// [`TextMeasurer`] rather than being resolved by the painter. That is what
/// makes the reference measurer's numbers and the geometry the painter
/// receives agree.
///
/// This is a separate value rather than two more `TextStyle` fields on
/// purpose. `TextStyle` is a public struct that embedders build as a struct
/// literal - `render-core`'s hit test builds one per character, in
/// `interaction::hit_test` - so every new field is a breaking change to a
/// public API of this crate, and the queued font-axis work (S1 in
/// `docs/visual_fidelity_gaps.md`) already has to add three of them. The
/// solver reads both values from the same computed style, so the two are
/// never out of step, and a measurer that does not override
/// [`TextMeasurer::measure_spaced`] still returns spec-correct advances.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TextSpacing {
    /// Extra advance inserted between adjacent typographic character units,
    /// half on each side. Negative values tighten text and are allowed.
    pub letter_spacing: f32,
    /// Extra advance applied to each word separator left in the text after
    /// white space processing. Negative values are allowed.
    pub word_spacing: f32,
}

impl TextSpacing {
    /// The extra advance CSS Text 3 §7 adds to a run of `text`.
    ///
    /// §7.2 inserts tracking *between* adjacent typographic character units,
    /// half on each side, and explicitly excludes the beginning and end of a
    /// line, so a run of `n` units carries `n - 1` gaps. §7.1 adds one extra
    /// advance per word separator, on top of the tracking that separator
    /// already receives as a character unit in its own right.
    #[must_use]
    pub fn extra_advance(&self, text: &str) -> f32 {
        let mut units = 0.0_f32;
        let mut separators = 0.0_f32;
        for character in text.chars() {
            units += 1.0;
            if is_word_separator(character) {
                separators += 1.0;
            }
        }
        (units - 1.0).max(0.0) * self.letter_spacing + separators * self.word_spacing
    }
}

/// CSS Text 3 §7.1: a word separator is "a typographic character unit whose
/// primary purpose and general usage is to separate words". The specification
/// lists these code points as included but "not exhaustively defined", and
/// explicitly excludes fixed-width spaces such as U+3000 IDEOGRAPHIC SPACE
/// and U+2000..U+200A, which separate words often but exist for other
/// reasons.
#[must_use]
pub const fn is_word_separator(character: char) -> bool {
    matches!(
        character,
        '\u{0020}'
            | '\u{00a0}'
            | '\u{1361}'
            | '\u{10100}'
            | '\u{10101}'
            | '\u{1039f}'
            | '\u{1091f}'
    )
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextMeasure {
    pub advance: f32,
    pub ascent: f32,
    pub descent: f32,
}

/// Font backends are leaf adapters. The reference solver remains deterministic
/// and parallel-safe as long as the supplied measurer is.
pub trait TextMeasurer: Sync {
    /// Measure `text` in the face `style.font` selects.
    ///
    /// The request reaches the measurer inside `TextStyle` rather than as extra
    /// parameters, so an implementor that already took a `TextStyle` starts
    /// seeing the axis without its signature changing - but it also cannot
    /// ignore it without deliberately ignoring it. A backend with no face
    /// table resolves the request the only way it can, through
    /// [`crate::font::nominal_face`].
    fn measure(&self, text: &str, style: TextStyle<'_>) -> TextMeasure;

    /// Measure `text` with the CSS Text 3 §7 spacing that applies to it.
    ///
    /// The default body keeps this trait source-compatible for existing
    /// backends while still returning the used advance: the unspaced advance
    /// plus the extra advance §7 requires, counted with
    /// [`is_word_separator`]. A backend only needs to override this when it
    /// shapes whole runs itself and can fold the spacing into the shaped run
    /// rather than adding it to the shaped result.
    fn measure_spaced(
        &self,
        text: &str,
        style: TextStyle<'_>,
        spacing: TextSpacing,
    ) -> TextMeasure {
        let mut metrics = self.measure(text, style);
        metrics.advance = (metrics.advance + spacing.extra_advance(text)).max(0.0);
        metrics
    }
}

/// Read-only view of decoded image resources for replaced-element sizing.
///
/// The concrete resource store is owned by the embedder because it shares deep
/// dependencies with painting and networking, so the reference solver depends
/// only on this seam.
pub trait ImageResourceProvider: Sync {
    /// Intrinsic pixel size of the decoded image currently loaded for `node`.
    #[must_use]
    fn intrinsic_size_for_node(&self, node: NodeId) -> Option<(u32, u32)>;
}

/// The deterministic, font-free measurer the reference path measures with.
///
/// It resolves `style.font` through [`crate::font::nominal_face`] and reads
/// advances from [`crate::font::NominalFace`], which is the same table
/// `render-core`'s `ReferenceTextShaper` shapes with. That shared table is the
/// whole reason a reference-path line box matches the reference-path glyph run
/// it is about to paint: neither half of the path has a font file, and both
/// halves derive their numbers from the same nominal face.
#[derive(Clone, Copy, Debug, Default)]
pub struct SimpleTextMeasurer;

impl TextMeasurer for SimpleTextMeasurer {
    fn measure(&self, text: &str, style: TextStyle<'_>) -> TextMeasure {
        let face = crate::font::nominal_face(&style.font);
        let ascent = face.ascent_em() * style.font_size;
        let descent = face.descent_em() * style.font_size;
        if text.is_empty() {
            return TextMeasure {
                advance: 0.0,
                ascent,
                descent,
            };
        }
        TextMeasure {
            advance: crate::font::nominal_advance(face, text, style.font_size),
            ascent,
            descent,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayoutLimits {
    pub max_fragments: usize,
    pub max_depth: usize,
    pub max_inline_characters: usize,
    pub max_grid_tracks: usize,
}

impl Default for LayoutLimits {
    fn default() -> Self {
        Self {
            max_fragments: 2_000_000,
            max_depth: 4_096,
            max_inline_characters: 64 * 1_024 * 1_024,
            max_grid_tracks: 65_536,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutOptions {
    pub viewport: PhysicalSize,
    pub root_font_size: f32,
    pub default_line_height: f32,
    pub limits: LayoutLimits,
}

impl Default for LayoutOptions {
    fn default() -> Self {
        Self {
            viewport: PhysicalSize {
                width: 1_280.0,
                height: 720.0,
            },
            root_font_size: 16.0,
            default_line_height: 19.2,
            limits: LayoutLimits::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutDiagnosticCode {
    FragmentLimit,
    DepthLimit,
    InlineTextLimit,
    GridTrackLimit,
    MissingFormattingNode,
    MissingComputedStyle,
    UnresolvedUsedValue,
    IntrinsicSizingNotImplemented,
    FormattingContextNotImplemented,
    PositioningNotImplemented,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutDiagnostic {
    pub node: Option<NodeId>,
    pub code: LayoutDiagnosticCode,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LayoutOutput {
    pub fragments: FragmentTree,
    pub diagnostics: Vec<LayoutDiagnostic>,
}

/// Resolve a formatting tree into immutable fragments for the same DOM
/// revision. This is the deterministic reference path; optimized schedulers
/// may execute independent work units concurrently and must produce equivalent
/// fragments.
#[must_use]
pub fn layout_formatting_tree(
    dom: &Dom,
    formatting: &FormattingTree,
    styles: &BTreeMap<NodeId, ComputedStyle>,
    options: LayoutOptions,
    text_measurer: &dyn TextMeasurer,
) -> LayoutOutput {
    run_reference_layout(dom, formatting, styles, options, text_measurer, None)
}

/// Layout with decoded replaced-element resources available for intrinsic sizing.
///
/// The provider is generic so embedders can pass their concrete resource store
/// by reference without naming a trait object.
#[must_use]
pub fn layout_formatting_tree_with_images<I: ImageResourceProvider>(
    dom: &Dom,
    formatting: &FormattingTree,
    styles: &BTreeMap<NodeId, ComputedStyle>,
    options: LayoutOptions,
    text_measurer: &dyn TextMeasurer,
    images: Option<&I>,
) -> LayoutOutput {
    run_reference_layout(
        dom,
        formatting,
        styles,
        options,
        text_measurer,
        images.map(|provider| provider as &dyn ImageResourceProvider),
    )
}

fn run_reference_layout(
    dom: &Dom,
    formatting: &FormattingTree,
    styles: &BTreeMap<NodeId, ComputedStyle>,
    options: LayoutOptions,
    text_measurer: &dyn TextMeasurer,
    images: Option<&dyn ImageResourceProvider>,
) -> LayoutOutput {
    let mut solver = Solver {
        dom,
        formatting,
        styles,
        options,
        text_measurer,
        images,
        fragments: Vec::new(),
        diagnostics: Vec::new(),
        inline_characters: 0,
        fragment_limit_reported: false,
        table_columns: BTreeMap::new(),
        collapsed_edges: BTreeMap::new(),
        sticky_boxes: Vec::new(),
        clipping_boxes: Vec::new(),
    };
    let viewport_rect = PhysicalRect::new(
        0.0,
        0.0,
        options.viewport.width.max(0.0),
        options.viewport.height.max(0.0),
    );
    let root = solver
        .allocate_fragment(
            formatting.root(),
            None,
            viewport_rect,
            FragmentKind::Box(BoxGeometry {
                margin: EdgeSizes::default(),
                border: EdgeSizes::default(),
                padding: EdgeSizes::default(),
                content_rect: viewport_rect,
            }),
        )
        .unwrap_or(FragmentId::from_index(0));

    let root_children = formatting
        .get(formatting.root())
        .map(|node| node.children.clone())
        .unwrap_or_default();
    let mut cursor_y = viewport_rect.origin.y;
    let mut children = Vec::new();
    for child in root_children {
        // The viewport is the root containing block: its height is definite,
        // so top-level percentage heights resolve against it (CSS 2 §10.5).
        if let Some(result) =
            solver.layout_block_like(child, viewport_rect, viewport_rect, cursor_y, 0, true)
        {
            cursor_y += result.outer_height;
            children.push(result.fragment);
        }
    }
    solver.set_children(root, children);
    let sticky = solver.sticky_constraints();
    let clips = solver.scrollport_geometries(root);
    let fragments = FragmentTree::new(
        formatting.dom_revision,
        options.viewport,
        root,
        solver.fragments,
    )
    .with_sticky_constraints(sticky)
    .with_scrollports(clips);
    LayoutOutput {
        fragments,
        diagnostics: solver.diagnostics,
    }
}

struct Solver<'a> {
    dom: &'a Dom,
    formatting: &'a FormattingTree,
    styles: &'a BTreeMap<NodeId, ComputedStyle>,
    options: LayoutOptions,
    text_measurer: &'a dyn TextMeasurer,
    images: Option<&'a dyn ImageResourceProvider>,
    fragments: Vec<Fragment>,
    diagnostics: Vec<LayoutDiagnostic>,
    inline_characters: usize,
    fragment_limit_reported: bool,
    /// The column widths and used content width the table algorithm resolved
    /// for each table box, so its sizing pass and its layout pass agree without
    /// measuring the same cells twice.
    table_columns: BTreeMap<FormattingNodeId, (f32, Vec<f32>)>,
    /// The border and padding the table algorithm collapsed for a cell, keyed by
    /// the element they belong to.
    collapsed_edges: BTreeMap<NodeId, CollapsedEdges>,
    /// Boxes whose computed `position` is `sticky`, in allocation order. The
    /// displacement itself is not applied here (§4.1): it depends on the scroll
    /// offset, which layout never sees, so these are resolved into constraints
    /// once the whole tree is in its final position.
    sticky_boxes: Vec<FragmentId>,
    /// Boxes whose `overflow` is not `visible`, with the element to read the
    /// declaration from. Like the sticky boxes, the geometry is resolved once
    /// the tree is finished.
    clipping_boxes: Vec<(FragmentId, Option<NodeId>)>,
}

/// The used border and padding of a box whose edges the table algorithm has
/// collapsed. `None` on either side leaves that edge to the cascade.
#[derive(Clone, Copy, Default)]
struct CollapsedEdges {
    border: Option<EdgeSizes>,
    padding: Option<EdgeSizes>,
}

#[derive(Clone, Copy)]
struct BlockResult {
    fragment: FragmentId,
    outer_height: f32,
    /// The height the box's in-flow children occupy, before a used `height`
    /// stretches the content box past them. This is what a consumer aligns
    /// inside the box - a table cell's `vertical-align` (§17.5.3) places the
    /// content within the cell's content box, and the content box is not the
    /// content's height.
    flow_height: f32,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is one independent margin edge of the item"
)]
struct FlexItem {
    node: FormattingNodeId,
    source: Option<NodeId>,
    order: i32,
    grow: f32,
    shrink: f32,
    base_outer: f32,
    target_outer: f32,
    min_outer: f32,
    fragment: Option<FragmentId>,
    natural_outer_cross: f32,
    auto_main_before: bool,
    auto_main_after: bool,
    /// Auto margins on the cross axis. They absorb the free cross space before
    /// `align-self` applies (CSS Flexbox §8.1), so they win over the alignment.
    auto_cross_before: bool,
    auto_cross_after: bool,
}

struct GridItem {
    fragment: FragmentId,
    row: usize,
    row_span: usize,
    column: usize,
    natural_outer_height: f32,
    stretch_height: bool,
}

#[derive(Clone, Copy)]
struct FloatArea {
    side: Float,
    rect: PhysicalRect,
}

#[derive(Clone, Copy, Debug, Default)]
struct AutoEdge {
    value: f32,
    auto: bool,
}

#[derive(Clone, Copy)]
pub(super) struct InlineAtom<'a> {
    formatting_node: FormattingNodeId,
    source: Option<NodeId>,
    character: char,
    forced_break: bool,
    /// Whether a line may end immediately before this unit.
    ///
    /// This is the output of the UAX #14 line breaking algorithm in
    /// [`crate::linebreak`], narrowed by `word-break`, `line-break` and
    /// `white-space`, and it is what CSS Text 3 §5 means by "Wrapping is only
    /// performed at an allowed break point". Kinsoku shori is *not* a separate
    /// pass over it: a position the forbidden-line-start or forbidden-line-end
    /// classes prohibit is simply not an opportunity, so the line filler below
    /// moves to the next one and no measurement changes.
    break_before: bool,
    atomic: Option<FormattingNodeId>,
    /// The text properties that decide this unit's own break opportunities.
    /// CSS Text 3 §1.5 ignores inline box boundaries when determining adjacency
    /// for line breaking, so the opportunities are computed over the whole
    /// inline sequence while these stay per unit.
    line_breaking: LineBreakOptions,
    style: TextStyle<'a>,
    spacing: TextSpacing,
}

struct TextRun<'a> {
    formatting_node: FormattingNodeId,
    source: Option<NodeId>,
    text: String,
    x: f32,
    y: f32,
    width: f32,
    typography: inline::InlineTextStyle<'a>,
}

/// The typographic character unit immediately before the one being placed on
/// the current line.
///
/// CSS Text 3 §7.2 inserts half of a unit's tracking on each side, so the gap
/// between two units with different values is their average, and nothing is
/// inserted at the beginning or end of a line. That makes the previous unit -
/// not just the current one - part of the measurement.
#[derive(Clone, Copy)]
struct PreviousUnit<'a> {
    typography: inline::InlineTextStyle<'a>,
    /// Part of a consecutive run of atomic inlines, which §7.2 treats as a
    /// single typographic character unit.
    atomic: bool,
}

impl Solver<'_> {
    fn remove_fragment_subtree(&mut self, root: FragmentId) {
        let root = usize::try_from(root.as_u32()).unwrap_or(self.fragments.len());
        if root < self.fragments.len() {
            self.fragments.truncate(root);
        }
    }

    fn allocate_fragment(
        &mut self,
        formatting_node: FormattingNodeId,
        source: Option<NodeId>,
        rect: PhysicalRect,
        kind: FragmentKind,
    ) -> Option<FragmentId> {
        if self.fragments.len() >= self.options.limits.max_fragments {
            if !self.fragment_limit_reported {
                self.fragment_limit_reported = true;
                self.diagnostics.push(LayoutDiagnostic {
                    node: source,
                    code: LayoutDiagnosticCode::FragmentLimit,
                    message: "fragment limit exceeded".to_owned(),
                });
            }
            return None;
        }
        let id = FragmentId::from_index(self.fragments.len());
        self.fragments.push(Fragment {
            id,
            formatting_node,
            source,
            rect,
            kind,
            children: Vec::new(),
        });
        Some(id)
    }

    fn finish_box(&mut self, fragment: FragmentId, content_height: f32, children: Vec<FragmentId>) {
        let Some(fragment) = self.fragment_mut(fragment) else {
            return;
        };
        if let FragmentKind::Box(geometry) = &mut fragment.kind {
            geometry.content_rect.size.height = content_height;
            fragment.rect = geometry.border_rect();
        }
        fragment.children = children;
    }

    fn set_children(&mut self, fragment: FragmentId, children: Vec<FragmentId>) {
        if let Some(fragment) = self.fragment_mut(fragment) {
            fragment.children = children;
        }
    }

    fn fragment_mut(&mut self, id: FragmentId) -> Option<&mut Fragment> {
        usize::try_from(id.as_u32())
            .ok()
            .and_then(|index| self.fragments.get_mut(index))
    }

    fn fragment_z_index(&self, id: FragmentId) -> i32 {
        let Some(fragment) = usize::try_from(id.as_u32())
            .ok()
            .and_then(|index| self.fragments.get(index))
        else {
            return 0;
        };
        let own = self
            .formatting
            .get(fragment.formatting_node)
            .and_then(|node| node.style_source)
            .and_then(|source| self.styles.get(&source))
            .and_then(|style| style.get("z-index"))
            .and_then(|value| value.css_text().trim().parse::<i32>().ok())
            .unwrap_or(0);
        // A positioned descendant can escape an otherwise unpositioned
        // wrapper and overlap a later sibling. Carrying the highest descendant
        // layer upward preserves that ordering until a full stacking-context
        // tree is available.
        own.max(
            fragment
                .children
                .iter()
                .map(|child| self.fragment_z_index(*child))
                .max()
                .unwrap_or(0),
        )
    }

    fn source(&self, node: FormattingNodeId) -> Option<NodeId> {
        self.formatting.get(node).and_then(|node| node.source)
    }

    /// CSS Position 4 §4.1: resolve every sticky box's constraint from the
    /// finished tree.
    ///
    /// This runs after layout rather than during it because the rectangles have
    /// to be in final document coordinates: a relatively positioned ancestor, an
    /// out-of-flow ancestor, a table row or a cell's `vertical-align` offset all
    /// move a subtree once the box in it is already finished, and a constraint
    /// recorded before that movement would be stale.
    ///
    /// The containing block of an in-flow box is the content box of its nearest
    /// block container ancestor (§10.1), so anonymous block boxes and inline
    /// boxes in the chain are skipped.
    fn sticky_constraints(&mut self) -> BTreeMap<FragmentId, StickyConstraint> {
        let mut constraints = BTreeMap::new();
        if self.sticky_boxes.is_empty() {
            return constraints;
        }
        let mut parents: Vec<u32> = vec![u32::MAX; self.fragments.len()];
        for fragment in &self.fragments {
            for child in &fragment.children {
                if let Some(slot) = parents.get_mut(child.as_u32() as usize) {
                    *slot = fragment.id.as_u32();
                }
            }
        }
        for id in std::mem::take(&mut self.sticky_boxes) {
            let Some(fragment) = self.fragments.get(id.as_u32() as usize) else {
                continue;
            };
            let FragmentKind::Box(geometry) = &fragment.kind else {
                continue;
            };
            let margin_rect = geometry.margin_rect();
            let Some(block) = self.sticky_containing_block(id, &parents) else {
                continue;
            };
            let insets = self.sticky_insets(fragment.formatting_node, block.size.width);
            constraints.insert(
                id,
                StickyConstraint {
                    margin_rect,
                    containing_block: block,
                    insets,
                    scrollport: self.options.viewport,
                },
            );
        }
        constraints
    }

    /// The content box of the nearest block container ancestor of `id`.
    fn sticky_containing_block(&self, id: FragmentId, parents: &[u32]) -> Option<PhysicalRect> {
        let mut current = id.as_u32();
        loop {
            let parent = *parents.get(current as usize)?;
            // A fragment with no parent is the root, whose content box is the
            // initial containing block.
            if parent == u32::MAX {
                return match self.fragments.first()?.kind {
                    FragmentKind::Box(ref geometry) => Some(geometry.content_rect),
                    FragmentKind::Text(_) => None,
                };
            }
            let parent = self.fragments.get(parent as usize)?;
            let is_block_container =
                self.formatting
                    .get(parent.formatting_node)
                    .is_some_and(|node| {
                        matches!(
                            node.kind,
                            FormattingNodeKind::Root
                                | FormattingNodeKind::BlockContainer { .. }
                                | FormattingNodeKind::AtomicInline { .. }
                        )
                    });
            if is_block_container {
                return match parent.kind {
                    FragmentKind::Box(ref geometry) => Some(geometry.content_rect),
                    FragmentKind::Text(_) => None,
                };
            }
            current = parent.id.as_u32();
        }
    }

    /// §9.4.3: the four insets that form the sticky view rectangle, with an
    /// `auto` inset left as `None` because an unconstrained side is not the
    /// same as a zero one.
    fn sticky_insets(&mut self, node_id: FormattingNodeId, basis: f32) -> StickyInsets {
        let Some(node) = self.formatting.get(node_id) else {
            return StickyInsets::default();
        };
        let style = node
            .style_source
            .and_then(|source| self.styles.get(&source));
        let source = node.source;
        StickyInsets {
            top: self.resolve_inset(style, "top", basis, source),
            right: self.resolve_inset(style, "right", basis, source),
            bottom: self.resolve_inset(style, "bottom", basis, source),
            left: self.resolve_inset(style, "left", basis, source),
        }
    }

    /// CSS Overflow 3 §3.1: the padding box of every box that clips its
    /// overflow, and how much of its content is reachable by scrolling.
    ///
    /// One walk resolves all of them, because a nested scrollport's content is
    /// clipped by that scrollport and so must not count towards an outer one's
    /// range: each box only collects the content of the boxes between it and the
    /// nearest clipping descendant. The reach of a box is the end direction only,
    /// since overflow past the start edge is clipped away rather than scrollable
    /// to.
    fn scrollport_geometries(
        &mut self,
        root: FragmentId,
    ) -> BTreeMap<FragmentId, ScrollportGeometry> {
        let mut result = BTreeMap::new();
        if self.clipping_boxes.is_empty() {
            return result;
        }
        let modes: BTreeMap<FragmentId, ClipMode> = self
            .clipping_boxes
            .iter()
            .filter_map(|(fragment, source)| {
                let style = source.and_then(|source| self.styles.get(&source));
                block::overflow_clip_mode(style).map(|mode| (*fragment, mode))
            })
            .collect();
        // Each scrollport still open, with the content it can reach so far.
        let mut open: Vec<(FragmentId, PhysicalRect, PhysicalSize)> = Vec::new();
        // (fragment, next child, scrollports open before this box, first
        // scrollport this box's content counts towards).
        let mut stack = vec![(root, 0_usize, 0_usize, 0_usize)];
        while let Some((id, next, base, reachable)) = stack.pop() {
            let Some(fragment) = self.fragments.get(id.as_u32() as usize) else {
                continue;
            };
            let margin_rect = match &fragment.kind {
                FragmentKind::Box(geometry) => geometry.margin_rect(),
                FragmentKind::Text(_) => fragment.rect,
            };
            let own = modes.contains_key(&id);
            // Only on the way in: a box is not its own scrollable overflow, and
            // it opens its scrollport exactly once however many children it has.
            if next == 0 {
                // Past the nearest enclosing scrollport this content is
                // unreachable from further out: an inner scrollport clips it.
                for entry in open.iter_mut().skip(reachable) {
                    entry.2.width = entry.2.width.max(margin_rect.right() - entry.1.origin.x);
                    entry.2.height = entry.2.height.max(margin_rect.bottom() - entry.1.origin.y);
                }
                if let Some(mode) = modes.get(&id).copied() {
                    let clip = match &fragment.kind {
                        FragmentKind::Box(geometry) => geometry.padding_rect(),
                        FragmentKind::Text(_) => fragment.rect,
                    };
                    result.insert(
                        id,
                        ScrollportGeometry {
                            mode,
                            clip,
                            scrollable: clip.size,
                        },
                    );
                    open.push((id, clip, clip.size));
                }
            }
            // A box that clips its own content keeps that content to itself.
            let child_reachable = if own {
                open.len().saturating_sub(1)
            } else {
                reachable
            };
            if next < fragment.children.len() {
                // One child per visit, and a frame to come back on afterwards so
                // that the last child still closes this box's scrollport.
                if next < fragment.children.len() {
                    stack.push((id, next + 1, base, reachable));
                }
                stack.push((fragment.children[next], 0, open.len(), child_reachable));
            } else {
                while open.len() > base {
                    let (closed, _, scrollable) = open.pop().expect("length is checked");
                    if let Some(entry) = result.get_mut(&closed) {
                        entry.scrollable = scrollable;
                    }
                }
            }
        }
        result
    }
}
