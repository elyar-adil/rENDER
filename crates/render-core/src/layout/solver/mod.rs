//! Deterministic reference layout for block and inline formatting contexts.

use crate::css::computed::ComputedStyle;
use crate::css::properties::Float;
use crate::dom::Dom;
use crate::dom::NodeId;
use crate::image::ImageResources;
use crate::layout::fragment::BoxGeometry;
use crate::layout::fragment::Fragment;
use crate::layout::fragment::FragmentId;
use crate::layout::fragment::FragmentKind;
use crate::layout::fragment::FragmentTree;
use crate::layout::geometry::EdgeSizes;
use crate::layout::geometry::PhysicalRect;
use crate::layout::geometry::PhysicalSize;
use crate::layout::solver::inline::is_wide_character;
use crate::layout::tree::FormattingNodeId;
use crate::layout::tree::FormattingTree;
use std::collections::BTreeMap;

mod block;
mod flex;
mod grid;
mod inline;
mod resolve;

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextStyle {
    pub font_size: f32,
    pub line_height: f32,
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
    fn measure(&self, text: &str, style: TextStyle) -> TextMeasure;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SimpleTextMeasurer;

impl TextMeasurer for SimpleTextMeasurer {
    fn measure(&self, text: &str, style: TextStyle) -> TextMeasure {
        let advance = text
            .chars()
            .map(|character| {
                if character.is_whitespace() {
                    style.font_size * 0.25
                } else if is_wide_character(character) {
                    style.font_size
                } else {
                    style.font_size * 0.5
                }
            })
            .sum();
        TextMeasure {
            advance,
            ascent: style.font_size * 0.8,
            descent: style.font_size * 0.2,
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
    layout_formatting_tree_with_images(dom, formatting, styles, options, text_measurer, None)
}

/// Layout with decoded replaced-element resources available for intrinsic sizing.
#[must_use]
pub fn layout_formatting_tree_with_images(
    dom: &Dom,
    formatting: &FormattingTree,
    styles: &BTreeMap<NodeId, ComputedStyle>,
    options: LayoutOptions,
    text_measurer: &dyn TextMeasurer,
    images: Option<&ImageResources>,
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
        if let Some(result) =
            solver.layout_block_like(child, viewport_rect, viewport_rect, cursor_y, 0)
        {
            cursor_y += result.outer_height;
            children.push(result.fragment);
        }
    }
    solver.set_children(root, children);
    let fragments = FragmentTree::new(
        formatting.dom_revision,
        options.viewport,
        root,
        solver.fragments,
    );
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
    images: Option<&'a ImageResources>,
    fragments: Vec<Fragment>,
    diagnostics: Vec<LayoutDiagnostic>,
    inline_characters: usize,
    fragment_limit_reported: bool,
}

#[derive(Clone, Copy)]
struct BlockResult {
    fragment: FragmentId,
    outer_height: f32,
}

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
}

struct GridItem {
    fragment: FragmentId,
    row: usize,
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
struct InlineAtom {
    formatting_node: FormattingNodeId,
    source: Option<NodeId>,
    character: char,
    forced_break: bool,
    wrap_allowed: bool,
    atomic: Option<FormattingNodeId>,
    style: TextStyle,
}

struct TextRun {
    formatting_node: FormattingNodeId,
    source: Option<NodeId>,
    text: String,
    x: f32,
    y: f32,
    width: f32,
    style: TextStyle,
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
}
