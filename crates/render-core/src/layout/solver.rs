//! Deterministic reference layout for block and inline formatting contexts.

use std::collections::BTreeMap;

use crate::css::computed::ComputedStyle;
use crate::css::properties::{
    AlignItems, AutoLengthPercentage, BorderStyle, BorderWidth, BoxSizing, Clear, Display,
    DisplayInside, FlexBasis, FlexDirection, Float, Gap, GridAutoRepeat, GridTemplate, GridTrack,
    GridTrackBreadth, JustifyContent, LengthPercentage, LengthResolutionContext, MaxSize,
    NumericType, Overflow, Position, Size, TypedPropertyValue,
};
use crate::dom::{Dom, Node, NodeId, NodeKind};
use crate::image::ImageResources;

use super::fragment::{
    BoxGeometry, Fragment, FragmentId, FragmentKind, FragmentTree, TextFragmentData,
};
use super::geometry::{EdgeSizes, PhysicalRect, PhysicalSize};
use super::grid::{
    GridLimitError, TrackSizing, automatic_position, expand_auto_repeat, required_rows, size_axis,
};
use super::tree::{FormattingContextKind, FormattingNodeId, FormattingNodeKind, FormattingTree};

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

impl Solver<'_> {
    fn layout_block_like(
        &mut self,
        node_id: FormattingNodeId,
        containing: PhysicalRect,
        positioning_containing: PhysicalRect,
        margin_box_y: f32,
        depth: usize,
    ) -> Option<BlockResult> {
        if depth > self.options.limits.max_depth {
            self.diagnostics.push(LayoutDiagnostic {
                node: self.source(node_id),
                code: LayoutDiagnosticCode::DepthLimit,
                message: "layout depth limit exceeded".to_owned(),
            });
            return None;
        }
        let node = self.formatting.get(node_id)?.clone();
        match node.kind {
            FormattingNodeKind::AnonymousBlock
            | FormattingNodeKind::Inline
            | FormattingNodeKind::Text(_) => {
                self.layout_anonymous_block(node_id, containing, margin_box_y, depth)
            }
            FormattingNodeKind::BlockContainer { context } => {
                if !matches!(
                    context,
                    FormattingContextKind::Block
                        | FormattingContextKind::Flex
                        | FormattingContextKind::Grid
                ) {
                    self.diagnostics.push(LayoutDiagnostic {
                        node: node.source,
                        code: LayoutDiagnosticCode::FormattingContextNotImplemented,
                        message: format!(
                            "{context:?} formatting currently uses the block reference path"
                        ),
                    });
                }
                self.layout_block(
                    node_id,
                    containing,
                    positioning_containing,
                    margin_box_y,
                    depth,
                    None,
                )
            }
            FormattingNodeKind::AtomicInline { .. } => self.layout_block(
                node_id,
                containing,
                positioning_containing,
                margin_box_y,
                depth,
                None,
            ),
            FormattingNodeKind::Root => None,
        }
    }

    // Keep the CSS block constraint algorithm in specification order so each
    // sizing and auto-margin step remains directly auditable against CSS 2.
    #[allow(clippy::too_many_lines)]
    fn layout_block(
        &mut self,
        node_id: FormattingNodeId,
        containing: PhysicalRect,
        positioning_containing: PhysicalRect,
        margin_box_y: f32,
        depth: usize,
        forced_content_width: Option<f32>,
    ) -> Option<BlockResult> {
        let node = self.formatting.get(node_id)?.clone();
        let style = node
            .style_source
            .and_then(|source| self.styles.get(&source));
        if node.style_source.is_some() && style.is_none() {
            self.diagnostics.push(LayoutDiagnostic {
                node: node.source,
                code: LayoutDiagnosticCode::MissingComputedStyle,
                message: "formatting box has no computed style".to_owned(),
            });
        }
        let position = position(style);
        let out_of_flow = matches!(position, Position::Absolute | Position::Fixed);
        let containing = match position {
            Position::Fixed => PhysicalRect::new(
                0.0,
                0.0,
                self.options.viewport.width,
                self.options.viewport.height,
            ),
            Position::Absolute => positioning_containing,
            _ => containing,
        };
        let margin_box_y = if out_of_flow {
            containing.origin.y
        } else {
            margin_box_y
        };

        let basis = containing.size.width;
        let mut margin_left = self.resolve_auto_edge(style, "margin-left", basis, node.source);
        let mut margin_right = self.resolve_auto_edge(style, "margin-right", basis, node.source);
        let margin_top = self.resolve_edge(style, "margin-top", basis, node.source);
        let margin_bottom = self.resolve_edge(style, "margin-bottom", basis, node.source);
        let padding = EdgeSizes {
            top: self
                .resolve_edge(style, "padding-top", basis, node.source)
                .max(0.0),
            right: self
                .resolve_edge(style, "padding-right", basis, node.source)
                .max(0.0),
            bottom: self
                .resolve_edge(style, "padding-bottom", basis, node.source)
                .max(0.0),
            left: self
                .resolve_edge(style, "padding-left", basis, node.source)
                .max(0.0),
        };
        let border = EdgeSizes {
            top: self.resolve_border(style, "border-top-width", basis, node.source),
            right: self.resolve_border(style, "border-right-width", basis, node.source),
            bottom: self.resolve_border(style, "border-bottom-width", basis, node.source),
            left: self.resolve_border(style, "border-left-width", basis, node.source),
        };
        let box_sizing = match style.and_then(|style| style.typed("box-sizing")) {
            Some(TypedPropertyValue::BoxSizing(value)) => *value,
            _ => BoxSizing::ContentBox,
        };

        let css_width = self.resolve_size(style, "width", basis, node.source);
        let css_height = self.resolve_size(style, "height", containing.size.height, node.source);
        let replaced_size = self.replaced_size(node.source, css_width, css_height);
        let specified_width = css_width.or(replaced_size.map(|size| size.width));
        let non_content = padding.horizontal() + border.horizontal();
        let fixed_margins = margin_left.value + margin_right.value;
        let left = self.resolve_inset(style, "left", containing.size.width, node.source);
        let right = self.resolve_inset(style, "right", containing.size.width, node.source);
        let positioned_content_width = if out_of_flow && specified_width.is_none() {
            left.zip(right).map(|(left, right)| {
                (containing.size.width
                    - left
                    - right
                    - non_content
                    - margin_left.value
                    - margin_right.value)
                    .max(0.0)
            })
        } else {
            None
        };
        let shrink_to_fit_width = if out_of_flow
            && specified_width.is_none()
            && positioned_content_width.is_none()
            && (left.is_some() || right.is_some())
        {
            Some(
                self.max_content_width(node_id).min(
                    (containing.size.width - non_content - margin_left.value - margin_right.value)
                        .max(0.0),
                ),
            )
        } else {
            None
        };
        let forced_content_width = forced_content_width.or(positioned_content_width);
        let mut content_width = forced_content_width.unwrap_or_else(|| {
            specified_width.map_or_else(
                || {
                    shrink_to_fit_width.unwrap_or_else(|| {
                        (containing.size.width - non_content - fixed_margins).max(0.0)
                    })
                },
                |width| match (css_width.is_some(), box_sizing) {
                    (true, BoxSizing::BorderBox) => (width - non_content).max(0.0),
                    _ => width,
                },
            )
        });
        content_width = self.apply_min_max_width(
            style,
            content_width,
            basis,
            node.source,
            non_content,
            box_sizing,
        );

        if forced_content_width.is_some() {
            margin_left.value = if margin_left.auto {
                0.0
            } else {
                margin_left.value
            };
            margin_right.value = if margin_right.auto {
                0.0
            } else {
                margin_right.value
            };
        } else if specified_width.is_none() {
            margin_left.value = 0.0;
            margin_right.value = 0.0;
        } else {
            let remaining = containing.size.width
                - content_width
                - non_content
                - margin_left.value
                - margin_right.value;
            match (margin_left.auto, margin_right.auto) {
                (true, true) if remaining > 0.0 => {
                    margin_left.value = remaining / 2.0;
                    margin_right.value = remaining / 2.0;
                }
                (true, false) if remaining > 0.0 => margin_left.value = remaining,
                (false, true) if remaining > 0.0 => margin_right.value = remaining,
                _ => margin_right.value += remaining,
            }
        }

        let content_x = containing.origin.x + margin_left.value + border.left + padding.left;
        let content_y = margin_box_y + margin_top + border.top + padding.top;
        let provisional_content = PhysicalRect::new(content_x, content_y, content_width, 0.0);
        let fragment = self.allocate_fragment(
            node_id,
            node.source,
            PhysicalRect::new(
                content_x - padding.left - border.left,
                content_y - padding.top - border.top,
                content_width + padding.horizontal() + border.horizontal(),
                0.0,
            ),
            FragmentKind::Box(BoxGeometry {
                margin: EdgeSizes {
                    top: margin_top,
                    right: margin_right.value,
                    bottom: margin_bottom,
                    left: margin_left.value,
                },
                border,
                padding,
                content_rect: provisional_content,
            }),
        )?;

        let specified_content_height =
            css_height
                .or(replaced_size.map(|size| size.height))
                .map(|height| match (css_height.is_some(), box_sizing) {
                    (true, BoxSizing::BorderBox) => {
                        (height - padding.vertical() - border.vertical()).max(0.0)
                    }
                    _ => height,
                });
        let top = self.resolve_inset(style, "top", containing.size.height, node.source);
        let bottom = self.resolve_inset(style, "bottom", containing.size.height, node.source);
        let relative_offset = if position == Position::Relative {
            (
                left.map_or_else(|| right.map_or(0.0, |right| -right), |left| left),
                top.map_or_else(|| bottom.map_or(0.0, |bottom| -bottom), |top| top),
            )
        } else {
            (0.0, 0.0)
        };
        let context = match node.kind {
            FormattingNodeKind::BlockContainer { context }
            | FormattingNodeKind::AtomicInline { context } => context,
            _ => FormattingContextKind::Block,
        };
        let positioned_child_containing = if position == Position::Static {
            positioning_containing
        } else {
            PhysicalRect::new(
                content_x - padding.left,
                content_y - padding.top,
                content_width + padding.horizontal(),
                specified_content_height.unwrap_or(0.0) + padding.vertical(),
            )
        };
        let (flow_children, positioned_children): (Vec<_>, Vec<_>) = node
            .children
            .into_iter()
            .partition(|child| !self.is_out_of_flow(*child));
        let (mut children, auto_height) = match context {
            FormattingContextKind::Flex => self.layout_flex_children(
                &flow_children,
                PhysicalRect::new(content_x, content_y, content_width, 0.0),
                positioned_child_containing,
                specified_content_height,
                depth.saturating_add(1),
                style,
                node.source,
            ),
            FormattingContextKind::Grid => self.layout_grid_children(
                &flow_children,
                PhysicalRect::new(content_x, content_y, content_width, 0.0),
                positioned_child_containing,
                specified_content_height,
                depth.saturating_add(1),
                style,
                node.source,
            ),
            _ => {
                let mut cursor_y = content_y;
                let mut children = Vec::new();
                let mut floats = Vec::new();
                for child in flow_children {
                    cursor_y = self.cleared_y(child, cursor_y, &floats);
                    let float = self.float_side(child);
                    if float == Float::None {
                        let result = if matches!(
                            self.formatting.get(child).map(|node| &node.kind),
                            Some(
                                FormattingNodeKind::AnonymousBlock
                                    | FormattingNodeKind::Inline
                                    | FormattingNodeKind::Text(_)
                            )
                        ) {
                            self.layout_anonymous_block_with_floats(
                                child,
                                PhysicalRect::new(content_x, content_y, content_width, 0.0),
                                cursor_y,
                                depth.saturating_add(1),
                                &floats,
                            )
                        } else {
                            let band =
                                float_band(&floats, cursor_y, content_x, content_x + content_width);
                            self.layout_block_like(
                                child,
                                PhysicalRect::new(
                                    band.0,
                                    content_y,
                                    (band.1 - band.0).max(0.0),
                                    0.0,
                                ),
                                positioned_child_containing,
                                cursor_y,
                                depth.saturating_add(1),
                            )
                        };
                        if let Some(result) = result {
                            cursor_y += result.outer_height;
                            children.push(result.fragment);
                        }
                    } else if let Some((fragment, area)) = self.layout_float(
                        child,
                        float,
                        PhysicalRect::new(content_x, content_y, content_width, 0.0),
                        positioned_child_containing,
                        cursor_y,
                        &floats,
                        depth.saturating_add(1),
                    ) {
                        floats.push(area);
                        children.push(fragment);
                    }
                }
                let flow_height = (cursor_y - content_y).max(0.0);
                // A non-visible overflow establishes a block formatting
                // context, so floats inside it contribute to its auto height
                // instead of escaping into the parent flow.
                let contains_floats = matches!(node.kind, FormattingNodeKind::AtomicInline { .. })
                    || matches!(
                        style.and_then(|style| style.typed("display")),
                        Some(TypedPropertyValue::Display(Display::Normal {
                            inside: DisplayInside::FlowRoot,
                            ..
                        }))
                    )
                    || establishes_block_formatting_context(style);
                let float_height = floats
                    .iter()
                    .map(|area| area.rect.bottom() - content_y)
                    .fold(0.0_f32, f32::max);
                (
                    children,
                    if contains_floats {
                        flow_height.max(float_height)
                    } else {
                        flow_height
                    },
                )
            }
        };
        let content_height = self.apply_min_max_height(
            style,
            specified_content_height.unwrap_or(auto_height),
            containing.size.height,
            node.source,
            padding.vertical() + border.vertical(),
            box_sizing,
        );
        self.finish_box(fragment, content_height, children.clone());
        let positioned_child_containing = if position == Position::Static {
            positioning_containing
        } else {
            PhysicalRect::new(
                content_x - padding.left,
                content_y - padding.top,
                content_width + padding.horizontal(),
                content_height + padding.vertical(),
            )
        };
        for child in positioned_children {
            if let Some(result) = self.layout_block_like(
                child,
                PhysicalRect::new(content_x, content_y, content_width, content_height),
                positioned_child_containing,
                content_y,
                depth.saturating_add(1),
            ) {
                children.push(result.fragment);
            }
        }
        self.set_children(fragment, children);
        if position == Position::Relative {
            self.translate_fragment_subtree(fragment, relative_offset.0, relative_offset.1);
        }
        if out_of_flow {
            if specified_content_height.is_none()
                && let (Some(top), Some(bottom)) = (top, bottom)
            {
                self.resize_fragment_outer_height(
                    fragment,
                    (containing.size.height - top - bottom).max(0.0),
                );
            }
            if let Some(outer) = self.fragment_outer_rect(fragment) {
                let target_x = left.map_or_else(
                    || {
                        right.map_or(outer.origin.x, |right| {
                            containing.right() - right - outer.size.width
                        })
                    },
                    |left| containing.origin.x + left,
                );
                let target_y = top.map_or_else(
                    || {
                        bottom.map_or(outer.origin.y, |bottom| {
                            containing.bottom() - bottom - outer.size.height
                        })
                    },
                    |top| containing.origin.y + top,
                );
                self.translate_fragment_subtree(
                    fragment,
                    target_x - outer.origin.x,
                    target_y - outer.origin.y,
                );
            }
        }
        Some(BlockResult {
            fragment,
            outer_height: if out_of_flow {
                0.0
            } else {
                margin_top + border.vertical() + padding.vertical() + content_height + margin_bottom
            },
        })
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn layout_grid_children(
        &mut self,
        children: &[FormattingNodeId],
        containing: PhysicalRect,
        positioning_containing: PhysicalRect,
        specified_height: Option<f32>,
        depth: usize,
        container_style: Option<&ComputedStyle>,
        container_source: Option<NodeId>,
    ) -> (Vec<FragmentId>, f32) {
        let column_gap = self.resolve_gap(
            container_style,
            "column-gap",
            containing.size.width,
            container_source,
        );
        let row_gap = self.resolve_gap(
            container_style,
            "row-gap",
            specified_height.unwrap_or(0.0),
            container_source,
        );
        let column_template = grid_template(container_style, "grid-template-columns");
        let row_template = grid_template(container_style, "grid-template-rows");
        let Ok(mut columns) = self.expand_grid_template(
            &column_template,
            Some(containing.size.width),
            column_gap,
            children.len(),
            "grid-template-columns",
            container_source,
        ) else {
            return (Vec::new(), 0.0);
        };
        if columns.is_empty() {
            columns.push(TrackSizing::Flexible {
                minimum: 0.0,
                factor: 1.0,
            });
        }

        let required_row_count = required_rows(children.len(), columns.len());
        let Ok(mut rows) = self.expand_grid_template(
            &row_template,
            specified_height,
            row_gap,
            required_row_count,
            "grid-template-rows",
            container_source,
        ) else {
            return (Vec::new(), 0.0);
        };
        let row_count = rows.len().max(required_row_count);
        if columns.len().saturating_add(row_count) > self.options.limits.max_grid_tracks {
            self.report_grid_track_limit(container_source);
            return (Vec::new(), 0.0);
        }
        rows.resize(row_count, TrackSizing::Intrinsic { minimum: 0.0 });

        let column_axis = size_axis(&columns, Some(containing.size.width), column_gap, &[]);
        let mut row_contributions = vec![0.0_f32; rows.len()];
        let mut items = Vec::with_capacity(children.len());
        for (index, node) in children.iter().copied().enumerate() {
            let (row, column) = automatic_position(index, columns.len());
            let track_width = column_axis.size(column);
            let track_x = containing.origin.x + column_axis.offset(column);
            let item_style = self
                .formatting
                .get(node)
                .and_then(|node| node.style_source)
                .and_then(|source| self.styles.get(&source));
            let item_source = self.formatting.get(node).and_then(|node| node.source);
            let forced_content_width = self.flex_content_width(
                item_style,
                track_width,
                containing.size.width,
                item_source,
            );
            let result = match self.formatting.get(node).map(|node| &node.kind) {
                Some(FormattingNodeKind::BlockContainer { .. }) => self.layout_block(
                    node,
                    PhysicalRect::new(
                        track_x,
                        containing.origin.y,
                        track_width,
                        specified_height.unwrap_or(0.0),
                    ),
                    positioning_containing,
                    containing.origin.y,
                    depth,
                    Some(forced_content_width),
                ),
                _ => self.layout_anonymous_block(
                    node,
                    PhysicalRect::new(
                        track_x,
                        containing.origin.y,
                        track_width,
                        specified_height.unwrap_or(0.0),
                    ),
                    containing.origin.y,
                    depth,
                ),
            };
            let Some(result) = result else {
                continue;
            };
            if let Some(contribution) = row_contributions.get_mut(row) {
                *contribution = contribution.max(result.outer_height);
            }
            items.push(GridItem {
                fragment: result.fragment,
                row,
                column,
                natural_outer_height: result.outer_height,
                stretch_height: self.grid_item_axis_is_auto(node, "height"),
            });
        }

        let row_axis = size_axis(&rows, specified_height, row_gap, &row_contributions);
        let mut fragments = Vec::with_capacity(items.len());
        for item in items {
            let row_height = row_axis.size(item.row);
            if item.stretch_height {
                self.resize_fragment_outer_height(item.fragment, row_height);
            }
            let outer = self
                .fragment_outer_rect(item.fragment)
                .unwrap_or(PhysicalRect::new(
                    containing.origin.x,
                    containing.origin.y,
                    column_axis.size(item.column),
                    item.natural_outer_height,
                ));
            self.translate_fragment_subtree(
                item.fragment,
                containing.origin.x + column_axis.offset(item.column) - outer.origin.x,
                containing.origin.y + row_axis.offset(item.row) - outer.origin.y,
            );
            fragments.push(item.fragment);
        }
        (fragments, row_axis.extent())
    }

    fn expand_grid_template(
        &mut self,
        template: &GridTemplate,
        available: Option<f32>,
        gap: f32,
        item_count: usize,
        property: &str,
        source: Option<NodeId>,
    ) -> Result<Vec<TrackSizing>, ()> {
        let resolved = match template {
            GridTemplate::None => return Ok(Vec::new()),
            GridTemplate::Tracks(tracks) => tracks
                .iter()
                .map(|track| self.resolve_grid_track(track, available, property, source))
                .collect(),
            GridTemplate::AutoRepeat { kind, tracks } => {
                let pattern = tracks
                    .iter()
                    .map(|track| self.resolve_grid_track(track, available, property, source))
                    .collect::<Vec<_>>();
                match expand_auto_repeat(
                    &pattern,
                    available.unwrap_or(0.0),
                    gap,
                    item_count,
                    *kind == GridAutoRepeat::Fit,
                    self.options.limits.max_grid_tracks,
                ) {
                    Ok(tracks) => tracks,
                    Err(GridLimitError::TrackLimit) => {
                        self.report_grid_track_limit(source);
                        return Err(());
                    }
                }
            }
        };
        if resolved.len() > self.options.limits.max_grid_tracks {
            self.report_grid_track_limit(source);
            Err(())
        } else {
            Ok(resolved)
        }
    }

    fn resolve_grid_track(
        &mut self,
        track: &GridTrack,
        available: Option<f32>,
        property: &str,
        source: Option<NodeId>,
    ) -> TrackSizing {
        match track {
            GridTrack::Breadth(GridTrackBreadth::Fraction(factor)) => TrackSizing::Flexible {
                minimum: 0.0,
                factor: *factor,
            },
            GridTrack::Breadth(GridTrackBreadth::LengthPercentage(value)) => self
                .resolve_grid_length(value, available, property, source)
                .map_or(TrackSizing::Intrinsic { minimum: 0.0 }, TrackSizing::Fixed),
            GridTrack::MinMax { minimum, maximum } => {
                let minimum = self
                    .resolve_grid_length(minimum, available, property, source)
                    .unwrap_or(0.0);
                match maximum {
                    GridTrackBreadth::Fraction(factor) => TrackSizing::Flexible {
                        minimum,
                        factor: *factor,
                    },
                    GridTrackBreadth::LengthPercentage(maximum) => self
                        .resolve_grid_length(maximum, available, property, source)
                        .map_or(TrackSizing::Intrinsic { minimum }, |maximum| {
                            TrackSizing::Fixed(maximum.max(minimum))
                        }),
                }
            }
        }
    }

    fn resolve_grid_length(
        &mut self,
        value: &LengthPercentage,
        available: Option<f32>,
        property: &str,
        source: Option<NodeId>,
    ) -> Option<f32> {
        if available.is_none() && grid_length_depends_on_percentage(value) {
            None
        } else {
            Some(
                self.resolve_length(value, available.unwrap_or(0.0), source, property)
                    .max(0.0),
            )
        }
    }

    fn grid_item_axis_is_auto(&self, node: FormattingNodeId, property: &str) -> bool {
        let style = self
            .formatting
            .get(node)
            .and_then(|node| node.style_source)
            .and_then(|source| self.styles.get(&source));
        matches!(
            style.and_then(|style| style.typed(property)),
            Some(TypedPropertyValue::Size(Size::Auto)) | None
        )
    }

    fn report_grid_track_limit(&mut self, source: Option<NodeId>) {
        self.diagnostics.push(LayoutDiagnostic {
            node: source,
            code: LayoutDiagnosticCode::GridTrackLimit,
            message: "grid track limit exceeded".to_owned(),
        });
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn layout_flex_children(
        &mut self,
        children: &[FormattingNodeId],
        containing: PhysicalRect,
        positioning_containing: PhysicalRect,
        specified_height: Option<f32>,
        depth: usize,
        container_style: Option<&ComputedStyle>,
        container_source: Option<NodeId>,
    ) -> (Vec<FragmentId>, f32) {
        let direction = match container_style.and_then(|style| style.typed("flex-direction")) {
            Some(TypedPropertyValue::FlexDirection(value)) => *value,
            _ => FlexDirection::Row,
        };
        let horizontal = matches!(direction, FlexDirection::Row | FlexDirection::RowReverse);
        let reverse = matches!(
            direction,
            FlexDirection::RowReverse | FlexDirection::ColumnReverse
        );
        let main_size = if horizontal {
            containing.size.width
        } else {
            specified_height.unwrap_or(0.0)
        };
        let gap_property = if horizontal { "column-gap" } else { "row-gap" };
        let gap_basis = if horizontal {
            containing.size.width
        } else {
            specified_height.unwrap_or(0.0)
        };
        let gap = self.resolve_gap(container_style, gap_property, gap_basis, container_source);
        let mut items = children
            .iter()
            .filter_map(|node| {
                let formatting = self.formatting.get(*node)?;
                let source = formatting.source;
                let style = formatting
                    .style_source
                    .and_then(|style_source| self.styles.get(&style_source));
                let order = match style.and_then(|style| style.typed("order")) {
                    Some(TypedPropertyValue::Order(value)) => *value,
                    _ => 0,
                };
                let grow = match style.and_then(|style| style.typed("flex-grow")) {
                    Some(TypedPropertyValue::FlexGrow(value)) => *value,
                    _ => 0.0,
                };
                let shrink = match style.and_then(|style| style.typed("flex-shrink")) {
                    Some(TypedPropertyValue::FlexShrink(value)) => *value,
                    _ => 1.0,
                };
                let basis = self.flex_basis(*node, style, horizontal, main_size, source);
                let extras =
                    self.flex_outer_extras(style, horizontal, containing.size.width, source);
                let base_outer = (basis + extras).max(0.0);
                let (before_property, after_property) = match direction {
                    FlexDirection::Row => ("margin-left", "margin-right"),
                    FlexDirection::RowReverse => ("margin-right", "margin-left"),
                    FlexDirection::Column => ("margin-top", "margin-bottom"),
                    FlexDirection::ColumnReverse => ("margin-bottom", "margin-top"),
                };
                Some(FlexItem {
                    node: *node,
                    source,
                    order,
                    grow,
                    shrink,
                    base_outer,
                    target_outer: base_outer,
                    fragment: None,
                    natural_outer_cross: 0.0,
                    auto_main_before: Self::margin_is_auto(style, before_property),
                    auto_main_after: Self::margin_is_auto(style, after_property),
                })
            })
            .collect::<Vec<_>>();
        items.sort_by_key(|item| item.order);
        let gaps = gap * count_as_f32(items.len().saturating_sub(1));

        // An auto-height column first uses natural item heights as its flex
        // base. Definite-height columns and rows can distribute immediately.
        let initial_main_total = items.iter().map(|item| item.base_outer).sum::<f32>() + gaps;
        let mut available_main = if horizontal {
            containing.size.width
        } else {
            specified_height.unwrap_or(initial_main_total)
        };
        Self::distribute_flex_space(&mut items, available_main - gaps);

        let align = match container_style.and_then(|style| style.typed("align-items")) {
            Some(TypedPropertyValue::AlignItems(AlignItems::Normal)) => AlignItems::Stretch,
            Some(TypedPropertyValue::AlignItems(value)) => *value,
            _ => AlignItems::Stretch,
        };
        let cross_hint = if horizontal {
            specified_height
        } else {
            Some(containing.size.width)
        };

        // Lay out at a stable origin first. Once natural cross sizes are known,
        // alignment is a pure subtree translation and optional stretch.
        for item in &mut items {
            let item_style = self
                .formatting
                .get(item.node)
                .and_then(|node| node.style_source)
                .and_then(|source| self.styles.get(&source));
            let cross_outer = if horizontal {
                containing.size.width
            } else {
                self.flex_cross_outer_size(
                    item.node,
                    item_style,
                    cross_hint.unwrap_or(containing.size.width),
                    align,
                    item.source,
                )
            };
            let outer_width = if horizontal {
                item.target_outer
            } else {
                cross_outer
            };
            let forced_content_width = self.flex_content_width(
                item_style,
                outer_width,
                containing.size.width,
                item.source,
            );
            let result = match self.formatting.get(item.node).map(|node| &node.kind) {
                Some(FormattingNodeKind::BlockContainer { .. }) => self.layout_block(
                    item.node,
                    PhysicalRect::new(
                        containing.origin.x,
                        containing.origin.y,
                        outer_width,
                        specified_height.unwrap_or(0.0),
                    ),
                    positioning_containing,
                    containing.origin.y,
                    depth,
                    Some(forced_content_width),
                ),
                _ => self.layout_anonymous_block(
                    item.node,
                    PhysicalRect::new(
                        containing.origin.x,
                        containing.origin.y,
                        outer_width,
                        specified_height.unwrap_or(0.0),
                    ),
                    containing.origin.y,
                    depth,
                ),
            };
            if let Some(result) = result {
                item.fragment = Some(result.fragment);
                item.natural_outer_cross = if horizontal {
                    result.outer_height
                } else {
                    self.fragment_outer_rect(result.fragment)
                        .map_or(outer_width, |rect| rect.size.width)
                };
                if !horizontal && Self::flex_basis_is_auto(item_style) {
                    item.base_outer = result.outer_height;
                    item.target_outer = result.outer_height;
                }
            }
        }

        if !horizontal && specified_height.is_some() {
            Self::distribute_flex_space(&mut items, available_main - gaps);
        } else if !horizontal {
            available_main = items.iter().map(|item| item.target_outer).sum::<f32>() + gaps;
        }
        let natural_cross = items
            .iter()
            .map(|item| item.natural_outer_cross)
            .fold(0.0_f32, f32::max);
        let line_cross = if horizontal {
            specified_height.unwrap_or(natural_cross)
        } else {
            containing.size.width
        };
        let used_main = items.iter().map(|item| item.target_outer).sum::<f32>() + gaps;
        let free_main = (available_main - used_main).max(0.0);
        let auto_main_margin_count = items
            .iter()
            .map(|item| usize::from(item.auto_main_before) + usize::from(item.auto_main_after))
            .sum::<usize>();
        let auto_main_margin = if auto_main_margin_count > 0 {
            free_main / count_as_f32(auto_main_margin_count)
        } else {
            0.0
        };
        let justify = match container_style.and_then(|style| style.typed("justify-content")) {
            Some(TypedPropertyValue::JustifyContent(value)) => *value,
            _ => JustifyContent::Normal,
        };
        let (main_offset, distributed_gap) = if auto_main_margin_count > 0 {
            (0.0, gap)
        } else {
            justify_offsets(justify, free_main, items.len(), gap)
        };
        let mut cursor = main_offset;
        let mut fragments = Vec::new();
        for item in items {
            let Some(fragment) = item.fragment else {
                continue;
            };
            if item.auto_main_before {
                cursor += auto_main_margin;
            }
            if horizontal {
                if align == AlignItems::Stretch && self.flex_cross_is_auto(item.node, "height") {
                    self.stretch_fragment_outer_height(fragment, line_cross);
                }
                let outer = self
                    .fragment_outer_rect(fragment)
                    .unwrap_or(PhysicalRect::new(
                        containing.origin.x,
                        containing.origin.y,
                        item.target_outer,
                        item.natural_outer_cross,
                    ));
                let cross_offset = align_offset(align, line_cross, outer.size.height);
                let target_x = if reverse {
                    containing.origin.x + available_main - cursor - item.target_outer
                } else {
                    containing.origin.x + cursor
                };
                self.translate_fragment_subtree(
                    fragment,
                    target_x - outer.origin.x,
                    containing.origin.y + cross_offset - outer.origin.y,
                );
            } else {
                self.resize_fragment_outer_height(fragment, item.target_outer);
                let outer = self
                    .fragment_outer_rect(fragment)
                    .unwrap_or(PhysicalRect::new(
                        containing.origin.x,
                        containing.origin.y,
                        item.natural_outer_cross,
                        item.target_outer,
                    ));
                let cross_offset = align_offset(align, line_cross, outer.size.width);
                let target_y = if reverse {
                    containing.origin.y + available_main - cursor - item.target_outer
                } else {
                    containing.origin.y + cursor
                };
                self.translate_fragment_subtree(
                    fragment,
                    containing.origin.x + cross_offset - outer.origin.x,
                    target_y - outer.origin.y,
                );
            }
            cursor += item.target_outer + distributed_gap;
            if item.auto_main_after {
                cursor += auto_main_margin;
            }
            fragments.push(fragment);
        }
        let auto_height = if horizontal {
            line_cross
        } else {
            available_main
        };
        (fragments, auto_height)
    }

    fn distribute_flex_space(items: &mut [FlexItem], available_without_gaps: f32) {
        for item in items.iter_mut() {
            item.target_outer = item.base_outer;
        }
        let base = items.iter().map(|item| item.base_outer).sum::<f32>();
        let free = available_without_gaps - base;
        if free > 0.0 {
            let grow = items.iter().map(|item| item.grow).sum::<f32>();
            if grow > 0.0 {
                for item in items {
                    item.target_outer += free * item.grow / grow;
                }
            }
        } else if free < 0.0 {
            let scaled = items
                .iter()
                .map(|item| item.shrink * item.base_outer)
                .sum::<f32>();
            if scaled > 0.0 {
                for item in items {
                    item.target_outer =
                        (item.base_outer + free * item.shrink * item.base_outer / scaled).max(0.0);
                }
            }
        }
    }

    fn flex_basis(
        &mut self,
        node: FormattingNodeId,
        style: Option<&ComputedStyle>,
        horizontal: bool,
        basis: f32,
        source: Option<NodeId>,
    ) -> f32 {
        match style.and_then(|style| style.typed("flex-basis")) {
            Some(TypedPropertyValue::FlexBasis(FlexBasis::LengthPercentage(value))) => {
                let specified = self.resolve_length(value, basis, source, "flex-basis");
                self.flex_basis_content_box(style, specified, horizontal, basis, source)
            }
            Some(TypedPropertyValue::FlexBasis(FlexBasis::Auto)) | None => {
                let property = if horizontal { "width" } else { "height" };
                match self.resolve_size(style, property, basis, source) {
                    Some(specified) => {
                        self.flex_basis_content_box(style, specified, horizontal, basis, source)
                    }
                    None => self.intrinsic_flex_size(node, horizontal, 0),
                }
            }
            Some(TypedPropertyValue::FlexBasis(FlexBasis::Content)) => {
                self.intrinsic_flex_size(node, horizontal, 0)
            }
            _ => 0.0,
        }
        .max(0.0)
    }

    fn intrinsic_flex_size(&self, node: FormattingNodeId, horizontal: bool, depth: usize) -> f32 {
        if depth > self.options.limits.max_depth {
            return 0.0;
        }
        let Some(node) = self.formatting.get(node) else {
            return 0.0;
        };
        if let FormattingNodeKind::Text(text) = &node.kind {
            let style = TextStyle {
                font_size: self.options.root_font_size,
                line_height: self.options.default_line_height,
            };
            return if horizontal {
                self.text_measurer.measure(text, style).advance
            } else if text.chars().all(char::is_whitespace) {
                0.0
            } else {
                style.line_height
            };
        }
        if horizontal {
            node.children
                .iter()
                .map(|child| self.intrinsic_flex_size(*child, true, depth.saturating_add(1)))
                .sum()
        } else {
            node.children
                .iter()
                .map(|child| self.intrinsic_flex_size(*child, false, depth.saturating_add(1)))
                .sum()
        }
    }

    fn flex_outer_extras(
        &mut self,
        style: Option<&ComputedStyle>,
        horizontal: bool,
        basis: f32,
        source: Option<NodeId>,
    ) -> f32 {
        let margins = if horizontal {
            ["margin-left", "margin-right"]
        } else {
            ["margin-top", "margin-bottom"]
        };
        margins
            .iter()
            .map(|property| self.resolve_edge(style, property, basis, source).max(0.0))
            .sum::<f32>()
            + self.flex_non_margin_extras(style, horizontal, basis, source)
    }

    fn flex_non_margin_extras(
        &mut self,
        style: Option<&ComputedStyle>,
        horizontal: bool,
        basis: f32,
        source: Option<NodeId>,
    ) -> f32 {
        let padding = if horizontal {
            ["padding-left", "padding-right"]
        } else {
            ["padding-top", "padding-bottom"]
        };
        let mut total = padding
            .iter()
            .map(|property| self.resolve_edge(style, property, basis, source).max(0.0))
            .sum::<f32>();
        let borders = if horizontal {
            ["border-left-width", "border-right-width"]
        } else {
            ["border-top-width", "border-bottom-width"]
        };
        total += borders
            .iter()
            .map(|property| self.resolve_border(style, property, basis, source))
            .sum::<f32>();
        total
    }

    fn flex_basis_content_box(
        &mut self,
        style: Option<&ComputedStyle>,
        specified: f32,
        horizontal: bool,
        basis: f32,
        source: Option<NodeId>,
    ) -> f32 {
        if matches!(
            style.and_then(|style| style.typed("box-sizing")),
            Some(TypedPropertyValue::BoxSizing(BoxSizing::BorderBox))
        ) {
            (specified - self.flex_non_margin_extras(style, horizontal, basis, source)).max(0.0)
        } else {
            specified
        }
    }

    fn flex_content_width(
        &mut self,
        style: Option<&ComputedStyle>,
        outer_width: f32,
        basis: f32,
        source: Option<NodeId>,
    ) -> f32 {
        (outer_width - self.flex_outer_extras(style, true, basis, source)).max(0.0)
    }

    fn flex_cross_outer_size(
        &mut self,
        node: FormattingNodeId,
        style: Option<&ComputedStyle>,
        available: f32,
        align: AlignItems,
        source: Option<NodeId>,
    ) -> f32 {
        let extras = self.flex_outer_extras(style, true, available, source);
        if align == AlignItems::Stretch && self.flex_cross_is_auto(node, "width") {
            available
        } else {
            self.resolve_size(style, "width", available, source)
                .unwrap_or_else(|| self.intrinsic_flex_size(node, true, 0))
                + extras
        }
    }

    fn flex_basis_is_auto(style: Option<&ComputedStyle>) -> bool {
        matches!(
            style.and_then(|style| style.typed("flex-basis")),
            Some(TypedPropertyValue::FlexBasis(FlexBasis::Auto)) | None
        )
    }

    fn margin_is_auto(style: Option<&ComputedStyle>, property: &str) -> bool {
        matches!(
            style.and_then(|style| style.typed(property)),
            Some(TypedPropertyValue::Margin(AutoLengthPercentage::Auto))
        )
    }

    fn flex_cross_is_auto(&self, node: FormattingNodeId, property: &str) -> bool {
        let style = self
            .formatting
            .get(node)
            .and_then(|node| node.style_source)
            .and_then(|source| self.styles.get(&source));
        matches!(
            style.and_then(|style| style.typed(property)),
            Some(TypedPropertyValue::Size(Size::Auto)) | None
        )
    }

    fn resolve_gap(
        &mut self,
        style: Option<&ComputedStyle>,
        property: &str,
        basis: f32,
        source: Option<NodeId>,
    ) -> f32 {
        match style.and_then(|style| style.typed(property)) {
            Some(TypedPropertyValue::Gap(Gap::LengthPercentage(value))) => {
                self.resolve_length(value, basis, source, property).max(0.0)
            }
            _ => 0.0,
        }
    }

    fn fragment_outer_rect(&self, fragment: FragmentId) -> Option<PhysicalRect> {
        let fragment = self
            .fragments
            .get(usize::try_from(fragment.as_u32()).ok()?)?;
        match &fragment.kind {
            FragmentKind::Box(geometry) => Some(geometry.margin_rect()),
            FragmentKind::Text(_) => Some(fragment.rect),
        }
    }

    fn translate_fragment_subtree(&mut self, root: FragmentId, dx: f32, dy: f32) {
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            let Some(fragment) = self.fragment_mut(id) else {
                continue;
            };
            fragment.rect.origin.x += dx;
            fragment.rect.origin.y += dy;
            if let FragmentKind::Box(geometry) = &mut fragment.kind {
                geometry.content_rect.origin.x += dx;
                geometry.content_rect.origin.y += dy;
            } else if let FragmentKind::Text(text) = &mut fragment.kind {
                text.baseline += dy;
            }
            stack.extend(fragment.children.iter().copied());
        }
    }

    fn stretch_fragment_outer_height(&mut self, fragment: FragmentId, outer_height: f32) {
        self.resize_fragment_outer_height(fragment, outer_height);
    }

    fn resize_fragment_outer_height(&mut self, fragment: FragmentId, outer_height: f32) {
        let Some(fragment) = self.fragment_mut(fragment) else {
            return;
        };
        let FragmentKind::Box(geometry) = &mut fragment.kind else {
            return;
        };
        geometry.content_rect.size.height = (outer_height
            - geometry.margin.vertical()
            - geometry.border.vertical()
            - geometry.padding.vertical())
        .max(0.0);
        fragment.rect = geometry.border_rect();
    }

    fn layout_anonymous_block(
        &mut self,
        node_id: FormattingNodeId,
        containing: PhysicalRect,
        y: f32,
        depth: usize,
    ) -> Option<BlockResult> {
        self.layout_anonymous_block_with_floats(node_id, containing, y, depth, &[])
    }

    fn layout_anonymous_block_with_floats(
        &mut self,
        node_id: FormattingNodeId,
        containing: PhysicalRect,
        y: f32,
        depth: usize,
        floats: &[FloatArea],
    ) -> Option<BlockResult> {
        let node = self.formatting.get(node_id)?.clone();
        let inline_roots = if matches!(
            node.kind,
            FormattingNodeKind::Inline | FormattingNodeKind::Text(_)
        ) {
            vec![node_id]
        } else {
            node.children
        };
        if let [inline_root] = inline_roots.as_slice()
            && matches!(
                self.formatting.get(*inline_root).map(|node| &node.kind),
                Some(FormattingNodeKind::BlockContainer {
                    context: FormattingContextKind::Grid
                })
            )
        {
            // An isolated inline-grid is an atomic inline-level box. The
            // surrounding inline solver does not yet mix atomic boxes and text,
            // but it can preserve the grid formatting context and geometry.
            return self.layout_block(*inline_root, containing, containing, y, depth, None);
        }
        let fragment = self.allocate_fragment(
            node_id,
            node.source,
            PhysicalRect::new(containing.origin.x, y, containing.size.width, 0.0),
            FragmentKind::Box(BoxGeometry {
                margin: EdgeSizes::default(),
                border: EdgeSizes::default(),
                padding: EdgeSizes::default(),
                content_rect: PhysicalRect::new(containing.origin.x, y, containing.size.width, 0.0),
            }),
        )?;
        let (children, height) = self.layout_inline_content(
            &inline_roots,
            PhysicalRect::new(containing.origin.x, y, containing.size.width, 0.0),
            depth.saturating_add(1),
            floats,
        );
        self.finish_box(fragment, height, children);
        Some(BlockResult {
            fragment,
            outer_height: height,
        })
    }

    #[allow(clippy::too_many_lines)]
    fn layout_inline_content(
        &mut self,
        roots: &[FormattingNodeId],
        containing: PhysicalRect,
        depth: usize,
        floats: &[FloatArea],
    ) -> (Vec<FragmentId>, f32) {
        let atoms = self.collect_inline_content_atoms(roots, depth);
        if atoms.is_empty()
            || atoms.iter().all(|atom| {
                atom.atomic.is_none() && !atom.forced_break && atom.character.is_whitespace()
            })
        {
            return (Vec::new(), 0.0);
        }
        let ends_with_forced_break = atoms
            .iter()
            .rev()
            .find(|atom| atom.forced_break || !atom.character.is_whitespace())
            .is_some_and(|atom| atom.forced_break);

        let style = TextStyle {
            font_size: self.options.root_font_size,
            line_height: self.options.default_line_height,
        };
        let mut fragments = Vec::new();
        let mut line_y = containing.origin.y;
        let (mut line_left, mut line_right) =
            inline_float_band(floats, containing, &mut line_y, style.line_height, 0.0);
        let mut line_x = line_left;
        let mut current_line_height = style.line_height;
        let mut pending_space = None;
        let mut current_run: Option<TextRun> = None;
        let mut cursor = 0;

        while cursor < atoms.len() {
            let atom = atoms[cursor];
            if atom.forced_break {
                self.flush_text_run(&mut current_run, &mut fragments, style, line_y);
                line_y += current_line_height;
                current_line_height = style.line_height;
                (line_left, line_right) =
                    inline_float_band(floats, containing, &mut line_y, style.line_height, 0.0);
                line_x = line_left;
                pending_space = None;
                cursor += 1;
                continue;
            }
            if let Some(atomic) = atom.atomic {
                self.flush_text_run(&mut current_run, &mut fragments, style, line_y);
                if let Some(space) = pending_space.take()
                    && line_x > line_left
                {
                    let width = self.text_measurer.measure(" ", style).advance;
                    self.push_character(
                        &mut current_run,
                        &mut fragments,
                        space,
                        ' ',
                        line_x,
                        line_y,
                        width,
                        style,
                    );
                    self.flush_text_run(&mut current_run, &mut fragments, style, line_y);
                    line_x += width;
                }
                if let Some((fragment, outer)) = self.layout_atomic_inline(
                    atomic,
                    containing,
                    line_x,
                    line_y,
                    depth.saturating_add(1),
                ) {
                    if (line_x - line_left).abs() < f32::EPSILON
                        && line_x + outer.size.width > line_right
                    {
                        (line_left, line_right) = inline_float_band(
                            floats,
                            containing,
                            &mut line_y,
                            style.line_height,
                            outer.size.width,
                        );
                        line_x = line_left;
                        self.translate_fragment_subtree(
                            fragment,
                            line_x - outer.origin.x,
                            line_y - outer.origin.y,
                        );
                    }
                    if line_x > line_left && line_x + outer.size.width > line_right {
                        line_y += current_line_height;
                        current_line_height = style.line_height;
                        (line_left, line_right) = inline_float_band(
                            floats,
                            containing,
                            &mut line_y,
                            style.line_height,
                            0.0,
                        );
                        line_x = line_left;
                        self.translate_fragment_subtree(
                            fragment,
                            line_x - outer.origin.x,
                            line_y - outer.origin.y,
                        );
                    }
                    line_x += outer.size.width;
                    current_line_height = current_line_height.max(outer.size.height);
                    fragments.push(fragment);
                }
                cursor += 1;
                continue;
            }
            if atom.character.is_whitespace() {
                pending_space = Some(atom);
                cursor += 1;
                continue;
            }

            let segment_end = inline_segment_end(&atoms, cursor);
            let segment = &atoms[cursor..segment_end];
            let segment_width = self.measure_inline_segment(segment, style);
            if (line_x - line_left).abs() < f32::EPSILON && segment_width > line_right - line_left {
                (line_left, line_right) = inline_float_band(
                    floats,
                    containing,
                    &mut line_y,
                    style.line_height,
                    segment_width,
                );
                line_x = line_left;
            }
            let space_width = pending_space
                .as_ref()
                .filter(|_| line_x > line_left)
                .map_or(0.0, |_| self.text_measurer.measure(" ", style).advance);
            if segment.first().is_some_and(|atom| atom.wrap_allowed)
                && line_x > line_left
                && line_x + space_width + segment_width > line_right
            {
                self.flush_text_run(&mut current_run, &mut fragments, style, line_y);
                line_y += current_line_height;
                current_line_height = style.line_height;
                (line_left, line_right) =
                    inline_float_band(floats, containing, &mut line_y, style.line_height, 0.0);
                line_x = line_left;
                pending_space = None;
            }

            if let Some(space) = pending_space.take()
                && line_x > line_left
            {
                let width = self.text_measurer.measure(" ", style).advance;
                self.push_character(
                    &mut current_run,
                    &mut fragments,
                    space,
                    ' ',
                    line_x,
                    line_y,
                    width,
                    style,
                );
                line_x += width;
            }

            for atom in segment {
                let width = self.measure_inline_character(atom.character, style);
                if atom.wrap_allowed && line_x + width > line_right && line_x > line_left {
                    self.flush_text_run(&mut current_run, &mut fragments, style, line_y);
                    line_y += current_line_height;
                    current_line_height = style.line_height;
                    (line_left, line_right) =
                        inline_float_band(floats, containing, &mut line_y, style.line_height, 0.0);
                    line_x = line_left;
                }
                self.push_character(
                    &mut current_run,
                    &mut fragments,
                    *atom,
                    atom.character,
                    line_x,
                    line_y,
                    width,
                    style,
                );
                line_x += width;
            }
            cursor = segment_end;
        }
        self.flush_text_run(&mut current_run, &mut fragments, style, line_y);
        let trailing_line_height = if ends_with_forced_break {
            0.0
        } else {
            current_line_height
        };
        let height = line_y - containing.origin.y + trailing_line_height;
        (fragments, height)
    }

    fn collect_inline_content_atoms(
        &mut self,
        roots: &[FormattingNodeId],
        depth: usize,
    ) -> Vec<InlineAtom> {
        let mut atoms = Vec::new();
        for root in roots {
            self.collect_inline_atoms(*root, &mut atoms, depth);
        }
        atoms
    }

    fn measure_inline_segment(&self, segment: &[InlineAtom], style: TextStyle) -> f32 {
        segment
            .iter()
            .map(|atom| self.measure_inline_character(atom.character, style))
            .sum()
    }

    fn measure_inline_character(&self, character: char, style: TextStyle) -> f32 {
        let mut encoded = [0_u8; 4];
        self.text_measurer
            .measure(character.encode_utf8(&mut encoded), style)
            .advance
    }

    fn collect_inline_atoms(
        &mut self,
        node_id: FormattingNodeId,
        atoms: &mut Vec<InlineAtom>,
        depth: usize,
    ) {
        if depth > self.options.limits.max_depth {
            return;
        }
        let Some(node) = self.formatting.get(node_id).cloned() else {
            self.diagnostics.push(LayoutDiagnostic {
                node: None,
                code: LayoutDiagnosticCode::MissingFormattingNode,
                message: "inline layout referenced an unknown formatting node".to_owned(),
            });
            return;
        };
        if let FormattingNodeKind::Text(text) = node.kind {
            let wrap_allowed = node
                .style_source
                .and_then(|source| self.styles.get(&source))
                .and_then(|style| style.get("white-space"))
                .is_none_or(|value| !value.css_text().eq_ignore_ascii_case("nowrap"));
            for character in text.chars() {
                if self.inline_characters >= self.options.limits.max_inline_characters {
                    self.diagnostics.push(LayoutDiagnostic {
                        node: node.source,
                        code: LayoutDiagnosticCode::InlineTextLimit,
                        message: "inline character limit exceeded".to_owned(),
                    });
                    return;
                }
                self.inline_characters += 1;
                atoms.push(InlineAtom {
                    formatting_node: node_id,
                    source: node.source,
                    character,
                    forced_break: false,
                    wrap_allowed,
                    atomic: None,
                });
            }
            return;
        }
        if node.source.is_some_and(|source| {
            matches!(
                self.dom.node(source).map(Node::kind),
                Some(NodeKind::Element(data)) if data.local_name == "br"
            )
        }) {
            atoms.push(InlineAtom {
                formatting_node: node_id,
                source: node.source,
                character: '\n',
                forced_break: true,
                wrap_allowed: false,
                atomic: None,
            });
            return;
        }
        if matches!(node.kind, FormattingNodeKind::AtomicInline { .. }) {
            atoms.push(InlineAtom {
                formatting_node: node_id,
                source: node.source,
                character: '\0',
                forced_break: false,
                wrap_allowed: true,
                atomic: Some(node_id),
            });
            return;
        }
        for child in node.children {
            self.collect_inline_atoms(child, atoms, depth.saturating_add(1));
        }
    }

    fn layout_atomic_inline(
        &mut self,
        node_id: FormattingNodeId,
        containing: PhysicalRect,
        x: f32,
        y: f32,
        depth: usize,
    ) -> Option<(FragmentId, PhysicalRect)> {
        let content_width = self.atomic_inline_content_width(node_id, containing.size.width);
        let result = self.layout_block(
            node_id,
            PhysicalRect::new(
                x,
                containing.origin.y,
                containing.size.width,
                containing.size.height,
            ),
            containing,
            y,
            depth,
            Some(content_width),
        )?;
        let outer = self.fragment_outer_rect(result.fragment)?;
        Some((result.fragment, outer))
    }

    fn atomic_inline_content_width(
        &mut self,
        node_id: FormattingNodeId,
        containing_width: f32,
    ) -> f32 {
        let node = self.formatting.get(node_id).cloned();
        let source = node.as_ref().and_then(|node| node.source);
        let style = node
            .as_ref()
            .and_then(|node| node.style_source)
            .and_then(|source| self.styles.get(&source))
            .cloned();
        let padding = self.resolve_edge(style.as_ref(), "padding-left", containing_width, source)
            + self.resolve_edge(style.as_ref(), "padding-right", containing_width, source);
        let border = self.resolve_border(
            style.as_ref(),
            "border-left-width",
            containing_width,
            source,
        ) + self.resolve_border(
            style.as_ref(),
            "border-right-width",
            containing_width,
            source,
        );
        let box_sizing = match style.as_ref().and_then(|style| style.typed("box-sizing")) {
            Some(TypedPropertyValue::BoxSizing(value)) => *value,
            _ => BoxSizing::ContentBox,
        };
        let css_width = self.resolve_size(style.as_ref(), "width", containing_width, source);
        let css_height = self.resolve_size(
            style.as_ref(),
            "height",
            self.options.viewport.height,
            source,
        );
        let replaced_width = self
            .replaced_size(source, css_width, css_height)
            .map(|size| size.width);
        let width = css_width.or(replaced_width).map_or_else(
            || {
                self.atomic_inline_intrinsic_width(node_id)
                    .min(containing_width)
            },
            |width| match (css_width.is_some(), box_sizing) {
                (true, BoxSizing::BorderBox) => (width - padding - border).max(0.0),
                _ => width,
            },
        );
        self.apply_min_max_width(
            style.as_ref(),
            width,
            containing_width,
            source,
            padding + border,
            box_sizing,
        )
    }

    fn replaced_size(
        &self,
        source: Option<NodeId>,
        css_width: Option<f32>,
        css_height: Option<f32>,
    ) -> Option<PhysicalSize> {
        let source = source?;
        let Some(NodeKind::Element(element)) = self.dom.node(source).map(Node::kind) else {
            return None;
        };
        if element.local_name != "img" {
            return None;
        }
        let html_width = self.html_image_dimension(source, "width");
        let html_height = self.html_image_dimension(source, "height");
        let intrinsic = self
            .images
            .and_then(|images| images.get_for_node(source))
            .map(|loaded| {
                let (width, height) = loaded.image.intrinsic_size();
                (
                    image_dimension_to_f32(width),
                    image_dimension_to_f32(height),
                )
            });
        let ratio = intrinsic
            .filter(|(_, height)| *height > 0.0)
            .map(|(width, height)| width / height)
            .or_else(|| {
                html_width
                    .zip(html_height)
                    .filter(|(_, height)| *height > 0.0)
                    .map(|(width, height)| width / height)
            });
        let width = css_width
            .or(html_width)
            .or_else(|| {
                css_height
                    .or(html_height)
                    .zip(ratio)
                    .map(|(height, ratio)| height * ratio)
            })
            .or_else(|| intrinsic.map(|(width, _)| width))
            .unwrap_or(300.0);
        let height = css_height
            .or(html_height)
            .or_else(|| {
                ratio
                    .filter(|ratio| *ratio > 0.0)
                    .map(|ratio| width / ratio)
            })
            .or_else(|| intrinsic.map(|(_, height)| height))
            .unwrap_or(150.0);
        Some(PhysicalSize {
            width: width.max(0.0),
            height: height.max(0.0),
        })
    }

    fn html_image_dimension(&self, source: NodeId, name: &str) -> Option<f32> {
        self.dom
            .attribute(source, name)
            .ok()
            .flatten()
            .and_then(|value| value.trim().parse::<u32>().ok())
            .map(image_dimension_to_f32)
    }

    fn atomic_inline_intrinsic_width(&mut self, node_id: FormattingNodeId) -> f32 {
        let children = self
            .formatting
            .get(node_id)
            .map(|node| node.children.clone())
            .unwrap_or_default();
        children
            .into_iter()
            .map(|child| self.max_content_width(child))
            .fold(0.0_f32, f32::max)
    }

    fn max_content_width(&mut self, node_id: FormattingNodeId) -> f32 {
        let Some(node) = self.formatting.get(node_id).cloned() else {
            return 0.0;
        };
        if let FormattingNodeKind::Text(text) = &node.kind {
            return self
                .text_measurer
                .measure(
                    text,
                    TextStyle {
                        font_size: self.options.root_font_size,
                        line_height: self.options.default_line_height,
                    },
                )
                .advance;
        }
        if matches!(node.kind, FormattingNodeKind::AtomicInline { .. }) {
            return self.atomic_outer_max_content_width(node_id);
        }
        let inline_sequence = matches!(
            node.kind,
            FormattingNodeKind::AnonymousBlock | FormattingNodeKind::Inline
        );
        let widths = node
            .children
            .into_iter()
            .map(|child| self.max_content_width(child));
        if inline_sequence {
            widths.sum()
        } else {
            widths.fold(0.0_f32, f32::max)
        }
    }

    fn atomic_outer_max_content_width(&mut self, node_id: FormattingNodeId) -> f32 {
        let node = self.formatting.get(node_id).cloned();
        let source = node.as_ref().and_then(|node| node.source);
        let style = node
            .as_ref()
            .and_then(|node| node.style_source)
            .and_then(|source| self.styles.get(&source))
            .cloned();
        let basis = self.options.viewport.width;
        let margin = self.resolve_edge(style.as_ref(), "margin-left", basis, source)
            + self.resolve_edge(style.as_ref(), "margin-right", basis, source);
        let padding = self.resolve_edge(style.as_ref(), "padding-left", basis, source)
            + self.resolve_edge(style.as_ref(), "padding-right", basis, source);
        let border = self.resolve_border(style.as_ref(), "border-left-width", basis, source)
            + self.resolve_border(style.as_ref(), "border-right-width", basis, source);
        let non_content = padding + border;
        let box_sizing = match style.as_ref().and_then(|style| style.typed("box-sizing")) {
            Some(TypedPropertyValue::BoxSizing(value)) => *value,
            _ => BoxSizing::ContentBox,
        };
        let specified = self.resolve_size(style.as_ref(), "width", basis, source);
        let content_width = specified.map_or_else(
            || {
                node.map_or(0.0, |node| {
                    node.children
                        .into_iter()
                        .map(|child| self.max_content_width(child))
                        .fold(0.0_f32, f32::max)
                })
            },
            |width| match box_sizing {
                BoxSizing::ContentBox => width,
                BoxSizing::BorderBox => (width - non_content).max(0.0),
            },
        );
        self.apply_min_max_width(
            style.as_ref(),
            content_width,
            basis,
            source,
            non_content,
            box_sizing,
        ) + non_content
            + margin
    }

    #[allow(clippy::too_many_arguments)]
    fn push_character(
        &mut self,
        run: &mut Option<TextRun>,
        fragments: &mut Vec<FragmentId>,
        atom: InlineAtom,
        character: char,
        x: f32,
        y: f32,
        width: f32,
        style: TextStyle,
    ) {
        if run.as_ref().is_some_and(|run| {
            run.formatting_node != atom.formatting_node || (run.y - y).abs() > f32::EPSILON
        }) {
            self.flush_text_run(run, fragments, style, y);
        }
        let run = run.get_or_insert_with(|| TextRun {
            formatting_node: atom.formatting_node,
            source: atom.source,
            text: String::new(),
            x,
            y,
            width: 0.0,
        });
        run.text.push(character);
        run.width += width;
    }

    fn flush_text_run(
        &mut self,
        run: &mut Option<TextRun>,
        fragments: &mut Vec<FragmentId>,
        style: TextStyle,
        _line_y: f32,
    ) {
        let Some(run) = run.take() else {
            return;
        };
        let metrics = self.text_measurer.measure(&run.text, style);
        let rect = PhysicalRect::new(run.x, run.y, run.width, style.line_height);
        if let Some(fragment) = self.allocate_fragment(
            run.formatting_node,
            run.source,
            rect,
            FragmentKind::Text(TextFragmentData {
                text: run.text,
                baseline: run.y + metrics.ascent,
                font_size: style.font_size,
            }),
        ) {
            fragments.push(fragment);
        }
    }

    fn resolve_auto_edge(
        &mut self,
        style: Option<&ComputedStyle>,
        property: &str,
        basis: f32,
        node: Option<NodeId>,
    ) -> AutoEdge {
        match style.and_then(|style| style.typed(property)) {
            Some(TypedPropertyValue::Margin(AutoLengthPercentage::Auto)) => AutoEdge {
                value: 0.0,
                auto: true,
            },
            Some(TypedPropertyValue::Margin(AutoLengthPercentage::LengthPercentage(value))) => {
                AutoEdge {
                    value: self.resolve_length(value, basis, node, property),
                    auto: false,
                }
            }
            _ => AutoEdge::default(),
        }
    }

    fn resolve_edge(
        &mut self,
        style: Option<&ComputedStyle>,
        property: &str,
        basis: f32,
        node: Option<NodeId>,
    ) -> f32 {
        let value = match style.and_then(|style| style.typed(property)) {
            Some(
                TypedPropertyValue::Margin(AutoLengthPercentage::LengthPercentage(value))
                | TypedPropertyValue::Inset(AutoLengthPercentage::LengthPercentage(value))
                | TypedPropertyValue::Padding(value),
            ) => Some(value),
            _ => None,
        };
        value.map_or(0.0, |value| {
            self.resolve_length(value, basis, node, property)
        })
    }

    fn resolve_inset(
        &mut self,
        style: Option<&ComputedStyle>,
        property: &str,
        basis: f32,
        node: Option<NodeId>,
    ) -> Option<f32> {
        match style.and_then(|style| style.typed(property)) {
            Some(TypedPropertyValue::Inset(AutoLengthPercentage::LengthPercentage(value))) => {
                Some(self.resolve_length(value, basis, node, property))
            }
            _ => None,
        }
    }

    fn resolve_border(
        &mut self,
        style: Option<&ComputedStyle>,
        property: &str,
        basis: f32,
        node: Option<NodeId>,
    ) -> f32 {
        let style_property = match property {
            "border-top-width" => "border-top-style",
            "border-right-width" => "border-right-style",
            "border-bottom-width" => "border-bottom-style",
            "border-left-width" => "border-left-style",
            _ => unreachable!("only physical border-width longhands are resolved"),
        };
        if matches!(
            style.and_then(|style| style.typed(style_property)),
            Some(TypedPropertyValue::BorderStyle(
                BorderStyle::None | BorderStyle::Hidden
            )) | None
        ) {
            return 0.0;
        }
        match style.and_then(|style| style.typed(property)) {
            Some(TypedPropertyValue::BorderWidth(BorderWidth::Thin)) => 1.0,
            Some(TypedPropertyValue::BorderWidth(BorderWidth::Medium)) => 3.0,
            Some(TypedPropertyValue::BorderWidth(BorderWidth::Thick)) => 5.0,
            Some(TypedPropertyValue::BorderWidth(BorderWidth::Length(value))) => {
                self.resolve_length(value, basis, node, property).max(0.0)
            }
            _ => 0.0,
        }
    }

    fn resolve_size(
        &mut self,
        style: Option<&ComputedStyle>,
        property: &str,
        basis: f32,
        node: Option<NodeId>,
    ) -> Option<f32> {
        match style.and_then(|style| style.typed(property)) {
            Some(TypedPropertyValue::Size(Size::LengthPercentage(value))) => {
                Some(self.resolve_length(value, basis, node, property).max(0.0))
            }
            Some(TypedPropertyValue::Size(
                Size::MinContent | Size::MaxContent | Size::FitContent(_) | Size::Stretch,
            )) => {
                self.diagnostics.push(LayoutDiagnostic {
                    node,
                    code: LayoutDiagnosticCode::IntrinsicSizingNotImplemented,
                    message: format!("intrinsic sizing for '{property}' is not implemented"),
                });
                None
            }
            _ => None,
        }
    }

    fn apply_min_max_width(
        &mut self,
        style: Option<&ComputedStyle>,
        width: f32,
        basis: f32,
        node: Option<NodeId>,
        non_content: f32,
        box_sizing: BoxSizing,
    ) -> f32 {
        let to_content_width = |value: f32| match box_sizing {
            BoxSizing::ContentBox => value.max(0.0),
            BoxSizing::BorderBox => (value - non_content).max(0.0),
        };
        let min = match style.and_then(|style| style.typed("min-width")) {
            Some(TypedPropertyValue::Size(Size::LengthPercentage(value))) => {
                to_content_width(self.resolve_length(value, basis, node, "min-width"))
            }
            _ => 0.0,
        };
        let max = match style.and_then(|style| style.typed("max-width")) {
            Some(TypedPropertyValue::MaxSize(MaxSize::Size(Size::LengthPercentage(value)))) => {
                Some(to_content_width(self.resolve_length(
                    value,
                    basis,
                    node,
                    "max-width",
                )))
            }
            _ => None,
        };
        max.map_or(width.max(min), |max| width.max(min).min(max))
    }

    fn apply_min_max_height(
        &mut self,
        style: Option<&ComputedStyle>,
        height: f32,
        basis: f32,
        node: Option<NodeId>,
        non_content: f32,
        box_sizing: BoxSizing,
    ) -> f32 {
        let to_content_height = |value: f32| match box_sizing {
            BoxSizing::ContentBox => value.max(0.0),
            BoxSizing::BorderBox => (value - non_content).max(0.0),
        };
        let min = match style.and_then(|style| style.typed("min-height")) {
            Some(TypedPropertyValue::Size(Size::LengthPercentage(value))) => {
                to_content_height(self.resolve_length(value, basis, node, "min-height"))
            }
            _ => 0.0,
        };
        let max = match style.and_then(|style| style.typed("max-height")) {
            Some(TypedPropertyValue::MaxSize(MaxSize::Size(Size::LengthPercentage(value)))) => {
                Some(to_content_height(self.resolve_length(
                    value,
                    basis,
                    node,
                    "max-height",
                )))
            }
            _ => None,
        };
        max.map_or(height.max(min), |max| height.min(max).max(min))
    }

    fn resolve_length(
        &mut self,
        value: &LengthPercentage,
        basis: f32,
        node: Option<NodeId>,
        property: &str,
    ) -> f32 {
        let context = LengthResolutionContext {
            percentage_basis: Some(basis),
            font_size: self.options.root_font_size,
            root_font_size: self.options.root_font_size,
            line_height: self.options.default_line_height,
            root_line_height: self.options.default_line_height,
            viewport_width: self.options.viewport.width,
            viewport_height: self.options.viewport.height,
            ..LengthResolutionContext::default()
        };
        match value.resolve(&context) {
            Ok(value) => value,
            Err(error) => {
                self.diagnostics.push(LayoutDiagnostic {
                    node,
                    code: LayoutDiagnosticCode::UnresolvedUsedValue,
                    message: format!("could not resolve '{property}': {error}"),
                });
                0.0
            }
        }
    }

    fn is_out_of_flow(&self, node: FormattingNodeId) -> bool {
        let style = self
            .formatting
            .get(node)
            .and_then(|node| node.style_source)
            .and_then(|source| self.styles.get(&source));
        matches!(position(style), Position::Absolute | Position::Fixed)
    }

    fn float_side(&self, node: FormattingNodeId) -> Float {
        let style = self
            .formatting
            .get(node)
            .and_then(|node| node.style_source)
            .and_then(|source| self.styles.get(&source));
        match style.and_then(|style| style.typed("float")) {
            Some(TypedPropertyValue::Float(Float::Left | Float::InlineStart)) => Float::Left,
            Some(TypedPropertyValue::Float(Float::Right | Float::InlineEnd)) => Float::Right,
            _ => Float::None,
        }
    }

    fn cleared_y(&self, node: FormattingNodeId, y: f32, floats: &[FloatArea]) -> f32 {
        let style = self
            .formatting
            .get(node)
            .and_then(|node| node.style_source)
            .and_then(|source| self.styles.get(&source));
        let clear = match style.and_then(|style| style.typed("clear")) {
            Some(TypedPropertyValue::Clear(value)) => *value,
            _ => Clear::None,
        };
        floats
            .iter()
            .filter(|area| match clear {
                Clear::Left | Clear::InlineStart => area.side == Float::Left,
                Clear::Right | Clear::InlineEnd => area.side == Float::Right,
                Clear::Both => true,
                Clear::None => false,
            })
            .map(|area| area.rect.bottom())
            .fold(y, f32::max)
    }

    #[allow(clippy::too_many_arguments)]
    fn layout_float(
        &mut self,
        node: FormattingNodeId,
        side: Float,
        containing: PhysicalRect,
        positioning_containing: PhysicalRect,
        mut y: f32,
        floats: &[FloatArea],
        depth: usize,
    ) -> Option<(FragmentId, FloatArea)> {
        loop {
            let (left, right) = float_band(floats, y, containing.origin.x, containing.right());
            let available = (right - left).max(0.0);
            let forced_content_width = self.atomic_inline_content_width(node, available);
            let result = self.layout_block(
                node,
                PhysicalRect::new(left, containing.origin.y, available, 0.0),
                positioning_containing,
                y,
                depth,
                Some(forced_content_width),
            )?;
            let outer = self.fragment_outer_rect(result.fragment)?;
            if outer.size.width <= available || available >= containing.size.width {
                let target_x = if side == Float::Right {
                    right - outer.size.width
                } else {
                    left
                };
                self.translate_fragment_subtree(
                    result.fragment,
                    target_x - outer.origin.x,
                    y - outer.origin.y,
                );
                let rect = self.fragment_outer_rect(result.fragment)?;
                return Some((result.fragment, FloatArea { side, rect }));
            }
            let next_y = floats
                .iter()
                .filter(|area| area.rect.origin.y <= y && area.rect.bottom() > y)
                .map(|area| area.rect.bottom())
                .fold(y, f32::max);
            if next_y <= y {
                return Some((result.fragment, FloatArea { side, rect: outer }));
            }
            self.remove_fragment_subtree(result.fragment);
            y = next_y;
        }
    }

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

    fn source(&self, node: FormattingNodeId) -> Option<NodeId> {
        self.formatting.get(node).and_then(|node| node.source)
    }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "CSS layout geometry is f32 and decoded image dimensions are bounded by image limits"
)]
fn image_dimension_to_f32(value: u32) -> f32 {
    value as f32
}

#[derive(Clone, Copy)]
struct InlineAtom {
    formatting_node: FormattingNodeId,
    source: Option<NodeId>,
    character: char,
    forced_break: bool,
    wrap_allowed: bool,
    atomic: Option<FormattingNodeId>,
}

struct TextRun {
    formatting_node: FormattingNodeId,
    source: Option<NodeId>,
    text: String,
    x: f32,
    y: f32,
    width: f32,
}

fn grid_template(style: Option<&ComputedStyle>, property: &str) -> GridTemplate {
    match style.and_then(|style| style.typed(property)) {
        Some(TypedPropertyValue::GridTemplate(template)) => template.clone(),
        _ => GridTemplate::None,
    }
}

fn position(style: Option<&ComputedStyle>) -> Position {
    match style.and_then(|style| style.typed("position")) {
        Some(TypedPropertyValue::Position(position)) => *position,
        _ => Position::Static,
    }
}

fn establishes_block_formatting_context(style: Option<&ComputedStyle>) -> bool {
    ["overflow-x", "overflow-y"].into_iter().any(|property| {
        matches!(
            style.and_then(|style| style.typed(property)),
            Some(TypedPropertyValue::Overflow(value)) if !matches!(value, Overflow::Visible)
        )
    })
}

fn float_band(floats: &[FloatArea], y: f32, mut left: f32, mut right: f32) -> (f32, f32) {
    for area in floats
        .iter()
        .filter(|area| area.rect.origin.y <= y && area.rect.bottom() > y)
    {
        match area.side {
            Float::Left => left = left.max(area.rect.right()),
            Float::Right => right = right.min(area.rect.origin.x),
            _ => {}
        }
    }
    (left, right.max(left))
}

fn inline_float_band(
    floats: &[FloatArea],
    containing: PhysicalRect,
    y: &mut f32,
    line_height: f32,
    minimum_width: f32,
) -> (f32, f32) {
    loop {
        let (left, right) = float_line_band(
            floats,
            *y,
            line_height,
            containing.origin.x,
            containing.right(),
        );
        let active = floats
            .iter()
            .any(|area| area.rect.origin.y < *y + line_height && area.rect.bottom() > *y);
        if right - left >= minimum_width && right > left || !active {
            return (left, right);
        }
        let Some(next_y) = floats
            .iter()
            .filter(|area| area.rect.origin.y < *y + line_height && area.rect.bottom() > *y)
            .map(|area| area.rect.bottom())
            .min_by(f32::total_cmp)
        else {
            return (left, right);
        };
        if next_y <= *y {
            return (left, right);
        }
        *y = next_y;
    }
}

fn float_line_band(
    floats: &[FloatArea],
    y: f32,
    line_height: f32,
    mut left: f32,
    mut right: f32,
) -> (f32, f32) {
    for area in floats
        .iter()
        .filter(|area| area.rect.origin.y < y + line_height && area.rect.bottom() > y)
    {
        match area.side {
            Float::Left => left = left.max(area.rect.right()),
            Float::Right => right = right.min(area.rect.origin.x),
            _ => {}
        }
    }
    (left.min(right), right.max(left))
}

fn grid_length_depends_on_percentage(value: &LengthPercentage) -> bool {
    match value {
        LengthPercentage::Percentage(_) => true,
        LengthPercentage::Calculation(calculation) => matches!(
            calculation.value_type,
            NumericType::Percentage | NumericType::LengthPercentage
        ),
        LengthPercentage::Zero | LengthPercentage::Length(_) => false,
    }
}

fn justify_offsets(
    justify: JustifyContent,
    free: f32,
    item_count: usize,
    base_gap: f32,
) -> (f32, f32) {
    let slots = count_as_f32(item_count.saturating_sub(1));
    match justify {
        JustifyContent::FlexEnd | JustifyContent::End => (free, base_gap),
        JustifyContent::Center => (free / 2.0, base_gap),
        JustifyContent::SpaceBetween if item_count > 1 => (0.0, base_gap + free / slots),
        JustifyContent::SpaceAround if item_count > 0 => {
            let distributed = free / count_as_f32(item_count);
            (distributed / 2.0, base_gap + distributed)
        }
        JustifyContent::SpaceEvenly if item_count > 0 => {
            let distributed = free / count_as_f32(item_count.saturating_add(1));
            (distributed, base_gap + distributed)
        }
        JustifyContent::Normal
        | JustifyContent::FlexStart
        | JustifyContent::Start
        | JustifyContent::SpaceBetween
        | JustifyContent::SpaceAround
        | JustifyContent::SpaceEvenly => (0.0, base_gap),
    }
}

#[allow(clippy::cast_precision_loss)]
fn count_as_f32(count: usize) -> f32 {
    count as f32
}

const fn align_offset(align: AlignItems, line_cross: f32, item_cross: f32) -> f32 {
    match align {
        AlignItems::FlexEnd | AlignItems::End => line_cross - item_cross,
        AlignItems::Center => (line_cross - item_cross) / 2.0,
        AlignItems::Normal | AlignItems::Stretch | AlignItems::FlexStart | AlignItems::Start => 0.0,
    }
}

fn inline_segment_end(atoms: &[InlineAtom], start: usize) -> usize {
    let mut end = start + 1;
    while let Some(next) = atoms.get(end) {
        let previous = atoms[end - 1];
        if next.forced_break
            || next.atomic.is_some()
            || previous.atomic.is_some()
            || next.character.is_whitespace()
            || (previous.wrap_allowed
                && next.wrap_allowed
                && is_soft_line_break(previous.character, next.character))
        {
            break;
        }
        end += 1;
    }
    end
}

fn is_soft_line_break(previous: char, next: char) -> bool {
    if is_prohibited_line_end(previous) || is_prohibited_line_start(next) {
        return false;
    }
    is_wide_character(previous)
        || is_wide_character(next)
        || matches!(previous, '-' | '/' | '\u{2010}')
}

const fn is_prohibited_line_end(character: char) -> bool {
    matches!(
        character,
        '(' | '['
            | '{'
            | '\u{00ab}'
            | '\u{2018}'
            | '\u{201c}'
            | '\u{3008}'
            | '\u{300a}'
            | '\u{300c}'
            | '\u{300e}'
            | '\u{3010}'
            | '\u{3014}'
            | '\u{3016}'
            | '\u{3018}'
            | '\u{301a}'
            | '\u{ff08}'
            | '\u{ff3b}'
            | '\u{ff5b}'
            | '\u{ff5f}'
    )
}

const fn is_prohibited_line_start(character: char) -> bool {
    matches!(
        character,
        '!' | '%' | ')' | ','
            ..='.'
                | ':'
                | ';'
                | '?'
                | ']'
                | '}'
                | '\u{00bb}'
                | '\u{2019}'
                | '\u{201d}'
                | '\u{3001}'
                | '\u{3002}'
                | '\u{3009}'
                | '\u{300b}'
                | '\u{300d}'
                | '\u{300f}'
                | '\u{3011}'
                | '\u{3015}'
                | '\u{3017}'
                | '\u{3019}'
                | '\u{301b}'
                | '\u{ff01}'
                | '\u{ff09}'
                | '\u{ff0c}'
                | '\u{ff0e}'
                | '\u{ff1a}'
                | '\u{ff1b}'
                | '\u{ff1f}'
                | '\u{ff3d}'
                | '\u{ff5d}'
                | '\u{ff60}'
    )
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
    #![allow(clippy::float_cmp)]

    use crate::css::cascade::{CascadeInput, CascadeOrigin};
    use crate::css::computed::{ComputationLimits, PropertyRegistry, compute_document_styles};
    use crate::css::selector::{MatchContext, parse_selector_list, select_all};
    use crate::css::stylesheet::parse_stylesheet;
    use crate::html::parse_document;
    use crate::layout::PhysicalRect;
    use crate::layout::fragment::FragmentKind;
    use crate::layout::tree::{FormattingLimits, build_formatting_tree};

    use super::{
        LayoutDiagnosticCode, LayoutLimits, LayoutOptions, SimpleTextMeasurer,
        layout_formatting_tree,
    };

    fn pipeline(
        html: &str,
        css: &str,
        width: f32,
    ) -> (
        crate::html::ParseOutput,
        std::collections::BTreeMap<crate::dom::NodeId, crate::css::computed::ComputedStyle>,
        super::LayoutOutput,
    ) {
        let output = parse_document(html);
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
            LayoutOptions {
                viewport: crate::layout::PhysicalSize {
                    width,
                    height: 600.0,
                },
                ..LayoutOptions::default()
            },
            &SimpleTextMeasurer,
        );
        (output, styles, layout)
    }

    fn find(dom: &crate::dom::Dom, selector: &str) -> crate::dom::NodeId {
        let selector = parse_selector_list(selector).unwrap();
        select_all(dom, dom.document(), &selector, &MatchContext::default())[0]
    }

    #[test]
    fn block_width_resolves_mixed_percentages_box_sizing_and_auto_margins() {
        let (output, _, layout) = pipeline(
            "<!doctype html><body><div id='box'></div></body>",
            "html, body { display:block; margin-left:0; margin-right:0 } #box { display:block; width:calc(50% - 20px); padding-left:10px; padding-right:10px; border-left-width:5px; border-right-width:5px; border-left-style:solid; border-right-style:solid; margin-left:auto; margin-right:auto }",
            800.0,
        );
        let box_node = find(&output.dom, "#box");
        let fragment = layout
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(box_node))
            .unwrap();
        let FragmentKind::Box(geometry) = &fragment.kind else {
            panic!("expected box fragment")
        };
        assert_eq!(geometry.content_rect.size.width, 380.0);
        assert_eq!(geometry.margin.left, 195.0);
        assert_eq!(geometry.margin.right, 195.0);
        assert_eq!(fragment.rect.size.width, 410.0);
    }

    #[test]
    fn auto_inline_block_max_content_includes_fixed_atomic_children() {
        let (output, _, layout) = pipeline(
            "<!doctype html><body><div id='outer'><div id='sites'><span></span><span></span><span></span><span></span><span></span><span></span><span></span><span></span></div></div></body>",
            "html, body, #outer { display:block; margin:0 } #outer { width:1190px } #sites, #sites > span { display:inline-block } #sites > span { box-sizing:border-box; width:106px; height:20px; margin-left:23px }",
            1190.0,
        );
        let sites = find(&output.dom, "#sites");
        let fragment = layout
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(sites))
            .expect("sites fragment");
        let FragmentKind::Box(geometry) = &fragment.kind else {
            panic!("expected atomic box fragment")
        };
        assert_eq!(geometry.content_rect.size.width, 1032.0);
    }

    #[test]
    fn border_box_min_max_width_constrain_the_border_box() {
        let (output, _, layout) = pipeline(
            "<!doctype html><body><div id='min'></div><div id='max'></div></body>",
            "html, body, div { display:block; margin:0 } div { box-sizing:border-box; padding-left:10px; padding-right:10px; border-left:5px solid; border-right:5px solid } #min { width:20px; min-width:100px } #max { width:200px; max-width:120px }",
            800.0,
        );
        let fragment_for = |selector| {
            let node = find(&output.dom, selector);
            layout
                .fragments
                .iter()
                .find(|fragment| fragment.source == Some(node))
                .expect("box fragment")
        };
        assert_eq!(fragment_for("#min").rect.size.width, 100.0);
        assert_eq!(fragment_for("#max").rect.size.width, 120.0);
    }

    #[test]
    fn block_height_applies_min_max_and_border_box_constraints() {
        let (output, _, layout) = pipeline(
            "<!doctype html><body><div id=min>x</div><div id=max>x</div><div id=conflict></div><div id=border></div></body>",
            "html, body, div { display:block; margin:0 } #min { min-height:80px } #max { height:100px; max-height:40px } #conflict { height:20px; min-height:60px; max-height:40px } #border { box-sizing:border-box; min-height:50px; padding-top:10px; padding-bottom:10px; border-top:2px solid; border-bottom:2px solid }",
            800.0,
        );
        let fragment_for = |selector| {
            let node = find(&output.dom, selector);
            layout
                .fragments
                .iter()
                .find(|fragment| fragment.source == Some(node))
                .expect("box fragment")
        };

        assert_eq!(fragment_for("#min").rect.size.height, 80.0);
        assert_eq!(fragment_for("#max").rect.size.height, 40.0);
        assert_eq!(fragment_for("#conflict").rect.size.height, 60.0);
        assert_eq!(fragment_for("#border").rect.size.height, 50.0);
    }

    #[test]
    fn legacy_163_news_display_values_keep_rows_in_normal_flow() {
        let (output, _, layout) = pipeline(
            "<!doctype html><body><ul id=news><li id=first><a>first</a></li><li id=second><a>second</a></li></ul></body>",
            "html, body, ul { display:block; margin:0 } #news li { display:-webkit-box; height:30px; line-height:30px; overflow:hidden } a { display:inline }",
            800.0,
        );
        let rect_for = |selector| {
            layout
                .fragments
                .iter()
                .find(|fragment| fragment.source == Some(find(&output.dom, selector)))
                .map(|fragment| fragment.rect)
                .expect("news row fragment")
        };

        assert_eq!(rect_for("#first").origin.y, 0.0);
        assert_eq!(rect_for("#second").origin.y, 30.0);
        assert_eq!(rect_for("#first").size.height, 30.0);
    }

    #[test]
    fn fixed_163_columns_honor_body_min_width_and_float_containment() {
        let (output, _, layout) = pipeline(
            "<!doctype html><body><div id=container><div id=area><div id=left></div><div id=right></div></div></div></body>",
            "html, body { display:block; margin:0 } body { min-width:1220px } #container { display:block; width:1200px; margin-left:auto; margin-right:auto } #area { display:block; overflow:hidden } #left { display:block; float:left; width:860px; height:20px } #right { display:block; float:right; width:300px; height:20px }",
            800.0,
        );
        let rect_for = |selector| {
            layout
                .fragments
                .iter()
                .find(|fragment| fragment.source == Some(find(&output.dom, selector)))
                .map(|fragment| fragment.rect)
                .expect("163 layout fragment")
        };

        assert_eq!(rect_for("body").size.width, 1220.0);
        assert_eq!(rect_for("#container").origin.x, 10.0);
        assert_eq!(rect_for("#left").origin.x, 10.0);
        assert_eq!(rect_for("#right").origin.x, 910.0);
        assert_eq!(rect_for("#area").size.height, 20.0);
    }

    #[test]
    fn inline_text_collapses_spaces_wraps_and_preserves_text_node_identity() {
        let (output, _, layout) = pipeline(
            "<!doctype html><body><p id='p'>hello    world世界</p></body>",
            "html, body, p { display:block; margin-left:0; margin-right:0 }",
            64.0,
        );
        let paragraph = find(&output.dom, "#p");
        let text = output.dom.children(paragraph).unwrap()[0];
        let fragments = layout
            .fragments
            .iter()
            .filter(|fragment| fragment.source == Some(text))
            .collect::<Vec<_>>();
        assert!(fragments.len() >= 2);
        let rendered = fragments
            .iter()
            .filter_map(|fragment| match &fragment.kind {
                FragmentKind::Text(text) => Some(text.text.as_str()),
                FragmentKind::Box(_) => None,
            })
            .collect::<String>();
        assert_eq!(rendered, "helloworld世界");
    }

    #[test]
    fn ordinary_words_wrap_at_spaces_instead_of_splitting_to_fill_a_line() {
        let (output, _, layout) = pipeline(
            "<!doctype html><body><p id='p'>HOME belongs interface</p></body>",
            "html, body, p { display:block; margin-left:0; margin-right:0 }",
            80.0,
        );
        let paragraph = find(&output.dom, "#p");
        let text = output.dom.children(paragraph).unwrap()[0];
        let lines = layout
            .fragments
            .iter()
            .filter(|fragment| fragment.source == Some(text))
            .filter_map(|fragment| match &fragment.kind {
                FragmentKind::Text(text) => Some((text.text.as_str(), fragment.rect.origin.y)),
                FragmentKind::Box(_) => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(
            lines.iter().map(|(text, _)| *text).collect::<Vec<_>>(),
            ["HOME", "belongs", "interface"]
        );
        assert!(lines.windows(2).all(|lines| lines[0].1 < lines[1].1));
    }

    #[test]
    fn a_single_overlong_word_uses_the_existing_emergency_character_wrap() {
        let (output, _, layout) = pipeline(
            "<!doctype html><body><p id='p'>abcdefgh</p></body>",
            "html, body, p { display:block; margin-left:0; margin-right:0 }",
            40.0,
        );
        let paragraph = find(&output.dom, "#p");
        let text = output.dom.children(paragraph).unwrap()[0];
        let fragments = layout
            .fragments
            .iter()
            .filter(|fragment| fragment.source == Some(text))
            .filter_map(|fragment| match &fragment.kind {
                FragmentKind::Text(text) => Some(text.text.as_str()),
                FragmentKind::Box(_) => None,
            })
            .collect::<Vec<_>>();

        assert!(fragments.len() > 1);
        assert_eq!(fragments.concat(), "abcdefgh");
    }

    #[test]
    fn nowrap_text_overflows_a_narrow_container_without_character_wrapping() {
        let (output, _, layout) = pipeline(
            "<!doctype html><body><div id='narrow'><span id='label'>complex question</span></div></body>",
            "html, body, div { display:block; margin:0 } #narrow { width:20px } #label { display:inline; white-space:nowrap }",
            320.0,
        );
        let label = find(&output.dom, "#label");
        let text = output.dom.children(label).unwrap()[0];
        let fragments = layout
            .fragments
            .iter()
            .filter(|fragment| fragment.source == Some(text))
            .filter_map(|fragment| match &fragment.kind {
                FragmentKind::Text(text) => Some((text.text.as_str(), fragment.rect)),
                FragmentKind::Box(_) => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(fragments.len(), 1);
        assert_eq!(fragments[0].0, "complex question");
        assert_eq!(fragments[0].1.origin.y, 0.0);
        assert!(fragments[0].1.size.width > 20.0);
    }

    #[test]
    fn inline_blocks_are_atomic_and_preserve_their_box_model_between_text() {
        let (output, _, layout) = pipeline(
            "<!doctype html><body><p id=row>start<a id=one><span>one</span><span id=inside>inner</span></a><a id=two>two</a>end</p></body>",
            "html, body, p { display:block; margin:0 } a { display:inline-block; width:40px; height:24px; padding-left:5px; padding-right:5px; border-left-width:2px; border-left-style:solid; border-right-width:2px; border-right-style:solid } #one { background-color:red } #two { background-color:blue }",
            320.0,
        );
        let one = layout
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(find(&output.dom, "#one")))
            .expect("first atomic box");
        let two = layout
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(find(&output.dom, "#two")))
            .expect("second atomic box");
        let FragmentKind::Box(one_geometry) = &one.kind else {
            panic!("expected atomic box fragment")
        };

        assert_eq!(
            one.rect.size,
            crate::layout::PhysicalSize {
                width: 54.0,
                height: 24.0
            }
        );
        assert_eq!(one_geometry.content_rect.size.width, 40.0);
        assert_eq!(two.rect.origin.x, one.rect.right());
        assert_eq!(one.rect.origin.y, two.rect.origin.y);
        let inside_text = output.dom.children(find(&output.dom, "#inside")).unwrap()[0];
        let inside_fragment = layout
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(inside_text))
            .expect("second inline child text");
        assert_eq!(
            inside_fragment.rect.origin.y,
            one_geometry.content_rect.origin.y
        );
        assert!(inside_fragment.rect.origin.x > one_geometry.content_rect.origin.x);
        assert!(one.children.iter().any(|child| {
            matches!(
                layout.fragments.get(*child).map(|fragment| &fragment.kind),
                Some(FragmentKind::Box(_))
            )
        }));
    }

    #[test]
    fn inline_block_wraps_as_one_unit_when_the_line_is_full() {
        let (output, _, layout) = pipeline(
            "<!doctype html><body><p>abcdefgh<a id=tile>inside</a></p></body>",
            "html, body, p { display:block; margin:0 } #tile { display:inline-block; width:60px; height:30px; padding-top:5px; padding-right:5px; padding-bottom:5px; padding-left:5px; border-top-width:1px; border-right-width:1px; border-bottom-width:1px; border-left-width:1px; border-top-style:solid; border-right-style:solid; border-bottom-style:solid; border-left-style:solid }",
            100.0,
        );
        let tile = layout
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(find(&output.dom, "#tile")))
            .expect("atomic box");

        assert_eq!(tile.rect.origin.x, 0.0);
        assert_eq!(
            tile.rect.origin.y,
            LayoutOptions::default().default_line_height
        );
        assert_eq!(tile.rect.size.width, 72.0);
        assert_eq!(tile.rect.size.height, 42.0);
    }

    #[test]
    fn left_and_right_floats_share_a_row_and_following_block_uses_the_remaining_band() {
        let (output, _, layout) = pipeline(
            "<!doctype html><body><div id=left></div><div id=right></div><div id=middle></div></body>",
            "html, body, div { display:block; margin:0 } #left { float:left; width:60px; height:40px } #right { float:right; width:50px; height:30px } #middle { height:20px; background-color:red }",
            240.0,
        );
        let rect = |selector| {
            layout
                .fragments
                .iter()
                .find(|fragment| fragment.source == Some(find(&output.dom, selector)))
                .expect("box fragment")
                .rect
        };

        assert_eq!(rect("#left"), PhysicalRect::new(0.0, 0.0, 60.0, 40.0));
        assert_eq!(rect("#right"), PhysicalRect::new(190.0, 0.0, 50.0, 30.0));
        assert_eq!(rect("#middle"), PhysicalRect::new(60.0, 0.0, 130.0, 20.0));
    }

    #[test]
    fn inline_lines_avoid_a_float_and_restore_full_width_below_it() {
        let (output, _, layout) = pipeline(
            "<!doctype html><body><div id=float></div>aaaaaaaaaa aaaaaaaaaa aaaaaaaaaaaaaaaa</body>",
            "html, body, div { display:block; margin:0 } #float { float:left; width:60px; height:38.4px }",
            160.0,
        );
        let text = output
            .dom
            .children(find(&output.dom, "body"))
            .unwrap()
            .iter()
            .copied()
            .find(|node| {
                matches!(
                    output.dom.node(*node).map(crate::dom::Node::kind),
                    Some(crate::dom::NodeKind::Text(_))
                )
            })
            .expect("body text node");
        let lines = layout
            .fragments
            .iter()
            .filter(|fragment| fragment.source == Some(text))
            .filter_map(|fragment| match &fragment.kind {
                FragmentKind::Text(text) => Some((text.text.as_str(), fragment.rect)),
                FragmentKind::Box(_) => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].1.origin.x, 60.0);
        assert_eq!(lines[1].1.origin.x, 60.0);
        assert_eq!(lines[2].0, "aaaaaaaaaaaaaaaa");
        assert_eq!(lines[2].1.origin.x, 0.0);
        assert_eq!(lines[2].1.origin.y, 38.4);
        assert!(lines[2].1.size.width > 100.0);
    }

    #[test]
    fn inline_line_advances_when_opposing_floats_leave_no_space() {
        let (output, _, layout) = pipeline(
            "<!doctype html><body><div id=left></div><div id=right></div>word</body>",
            "html, body, div { display:block; margin:0 } #left { float:left; width:80px; height:40px } #right { float:right; width:80px; height:20px }",
            160.0,
        );
        let body = find(&output.dom, "body");
        let text = output
            .dom
            .children(body)
            .unwrap()
            .iter()
            .copied()
            .find(|node| {
                matches!(
                    output.dom.node(*node).map(crate::dom::Node::kind),
                    Some(crate::dom::NodeKind::Text(_))
                )
            })
            .expect("body text node");
        let fragment = layout
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(text))
            .expect("text fragment");

        assert_eq!(fragment.rect.origin.x, 80.0);
        assert_eq!(fragment.rect.origin.y, 20.0);
    }

    #[test]
    fn clear_both_moves_below_floats_and_restores_the_full_containing_width() {
        let (output, _, layout) = pipeline(
            "<!doctype html><body><div id=left></div><div id=right></div><div id=clear></div></body>",
            "html, body, div { display:block; margin:0 } #left { float:left; width:60px; height:40px } #right { float:right; width:50px; height:30px } #clear { clear:both; height:10px }",
            240.0,
        );
        let clear = layout
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(find(&output.dom, "#clear")))
            .expect("cleared block");

        assert_eq!(clear.rect, PhysicalRect::new(0.0, 40.0, 240.0, 10.0));
    }

    #[test]
    fn ordinary_auto_height_excludes_floats_but_flow_root_contains_them() {
        let (ordinary_output, _, ordinary) = pipeline(
            "<!doctype html><body><div id=container><div id=float></div></div></body>",
            "html, body, div { display:block; margin:0 } #float { float:left; width:50px; height:35px }",
            200.0,
        );
        let ordinary_container = ordinary
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(find(&ordinary_output.dom, "#container")))
            .expect("ordinary container");
        assert_eq!(ordinary_container.rect.size.height, 0.0);

        let (flow_root_output, _, flow_root) = pipeline(
            "<!doctype html><body><div id=container><div id=float></div></div></body>",
            "html, body, div { display:block; margin:0 } #container { display:flow-root } #float { float:left; width:50px; height:35px }",
            200.0,
        );
        let flow_root_container = flow_root
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(find(&flow_root_output.dom, "#container")))
            .expect("flow-root container");
        assert_eq!(flow_root_container.rect.size.height, 35.0);
    }

    #[test]
    fn fragment_tree_is_bound_to_the_dynamic_dom_revision() {
        let (output, _, layout) = pipeline(
            "<!doctype html><body><main>dynamic</main></body>",
            "html, body, main { display:block }",
            320.0,
        );
        assert_eq!(layout.fragments.dom_revision, output.dom.revision());
        assert_eq!(layout.fragments.root().as_u32(), 0);
    }

    #[test]
    fn collapsible_whitespace_between_blocks_does_not_create_line_boxes() {
        let (output, _, layout) = pipeline(
            "<!doctype html><html><head><title>x</title></head><body>\n  <main id='content'>content</main>\n</body></html>",
            "html, body, main { display:block; margin-top:0; margin-right:0; margin-bottom:0; margin-left:0 } head, title { display:none }",
            320.0,
        );
        let main = find(&output.dom, "#content");
        let fragment = layout
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(main))
            .expect("main box fragment");

        assert_eq!(fragment.rect.origin.y, 0.0);
    }

    #[test]
    fn br_still_creates_a_line_when_collapsible_text_is_empty() {
        let (output, _, layout) = pipeline(
            "<!doctype html><body><p id='line'><br></p></body>",
            "html, body, p { display:block; margin-top:0; margin-right:0; margin-bottom:0; margin-left:0 }",
            320.0,
        );
        let paragraph = find(&output.dom, "#line");
        let fragment = layout
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(paragraph))
            .expect("paragraph box fragment");

        assert_eq!(
            fragment.rect.size.height,
            LayoutOptions::default().default_line_height
        );
    }

    #[test]
    fn explicit_grid_tracks_auto_place_items_with_gaps_and_box_model() {
        let (output, _, layout) = pipeline(
            "<!doctype html><body><div id='grid'><div id='a'></div><div id='b'></div><div id='c'></div><div id='d'></div></div></body>",
            "html, body, #grid, #a, #b, #c, #d { display:block; margin:0 } #grid { display:grid; width:400px; height:115px; grid-template-columns:100px 25% 1fr; grid-template-rows:50px 1fr; column-gap:10px; row-gap:5px } #b { margin-top:5px; margin-right:5px; margin-bottom:5px; margin-left:5px; padding-left:10px; padding-right:10px; border-left-width:5px; border-right-width:5px; border-left-style:solid; border-right-style:solid }",
            400.0,
        );
        let rect = |selector| {
            layout
                .fragments
                .iter()
                .find(|fragment| fragment.source == Some(find(&output.dom, selector)))
                .unwrap()
                .rect
        };
        assert_eq!(rect("#a"), PhysicalRect::new(0.0, 0.0, 100.0, 50.0));
        assert_eq!(rect("#b"), PhysicalRect::new(115.0, 5.0, 90.0, 40.0));
        assert_eq!(rect("#c"), PhysicalRect::new(220.0, 0.0, 180.0, 50.0));
        assert_eq!(rect("#d"), PhysicalRect::new(0.0, 55.0, 100.0, 60.0));

        let b = layout
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(find(&output.dom, "#b")))
            .unwrap();
        let FragmentKind::Box(geometry) = &b.kind else {
            panic!("expected grid item box")
        };
        assert_eq!(geometry.content_rect.size.width, 60.0);
        assert_eq!(geometry.margin_rect().size.width, 100.0);
    }

    #[test]
    fn auto_fit_minmax_grid_responds_to_available_inline_size() {
        let html = "<!doctype html><body><div id='grid'><div id='a'></div><div></div><div></div><div></div><div id='e'></div></div></body>";
        let css = "html, body, #grid, #grid > div { display:block; margin:0 } #grid { display:grid; grid-template-columns:repeat(auto-fit, minmax(140px, 1fr)); gap:10px } #grid > div { height:20px }";
        let (wide_output, _, wide) = pipeline(html, css, 620.0);
        let wide_a = wide
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(find(&wide_output.dom, "#a")))
            .unwrap();
        let wide_e = wide
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(find(&wide_output.dom, "#e")))
            .unwrap();
        assert_eq!(wide_a.rect.size.width, 147.5);
        assert_eq!(wide_e.rect.origin.y, 30.0);

        let (narrow_output, _, narrow) = pipeline(html, css, 320.0);
        let narrow_a = narrow
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(find(&narrow_output.dom, "#a")))
            .unwrap();
        let narrow_e = narrow
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(find(&narrow_output.dom, "#e")))
            .unwrap();
        assert_eq!(narrow_a.rect.size.width, 155.0);
        assert_eq!(narrow_e.rect.origin.y, 60.0);
    }

    #[test]
    fn isolated_inline_grid_preserves_its_grid_formatting_context() {
        let (output, _, layout) = pipeline(
            "<!doctype html><body><div id='grid'><span id='a'></span><span id='b'></span></div></body>",
            "html, body { display:block; margin:0 } #grid { display:inline-grid; width:200px; grid-template-columns:1fr 1fr } #grid > span { display:inline; height:10px }",
            300.0,
        );
        let a = layout
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(find(&output.dom, "#a")))
            .unwrap();
        let b = layout
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(find(&output.dom, "#b")))
            .unwrap();
        assert_eq!(a.rect, PhysicalRect::new(0.0, 0.0, 100.0, 10.0));
        assert_eq!(b.rect, PhysicalRect::new(100.0, 0.0, 100.0, 10.0));
    }

    #[test]
    fn class_mutation_rebuilds_grid_geometry_for_the_new_dom_revision() {
        let mut output = parse_document(
            "<!doctype html><body><div id='grid' class='two'><div id='a'></div><div id='b'></div></div></body>",
        );
        let sheet = parse_stylesheet(
            "html, body, #grid, #a, #b { display:block; margin:0 } #grid { display:grid; width:200px } #grid.two { grid-template-columns:1fr 1fr } #grid.one { grid-template-columns:1fr } #a, #b { height:20px }",
        );
        let render = |dom: &crate::dom::Dom| {
            let styles = compute_document_styles(
                dom,
                &[CascadeInput {
                    sheet: &sheet,
                    origin: CascadeOrigin::Author,
                }],
                &PropertyRegistry::standard_baseline(),
                &ComputationLimits::default(),
                &MatchContext::default(),
            );
            let formatting = build_formatting_tree(dom, &styles, &FormattingLimits::default());
            layout_formatting_tree(
                dom,
                &formatting,
                &styles,
                LayoutOptions::default(),
                &SimpleTextMeasurer,
            )
        };
        let grid = find(&output.dom, "#grid");
        let b = find(&output.dom, "#b");
        let before = render(&output.dom);
        let before_rect = before
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(b))
            .unwrap()
            .rect;

        output.dom.set_attribute(grid, "class", "one").unwrap();
        let after = render(&output.dom);
        let after_rect = after
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(b))
            .unwrap()
            .rect;
        assert_eq!(after.fragments.dom_revision, output.dom.revision());
        assert!(before_rect.origin.x > after_rect.origin.x);
        assert!(after_rect.origin.y > before_rect.origin.y);
    }

    #[test]
    fn grid_track_limit_fails_closed_before_item_fragment_allocation() {
        let output = parse_document(
            "<!doctype html><body><div id='grid'><div id='a'></div><div></div><div></div><div></div></div></body>",
        );
        let sheet = parse_stylesheet(
            "html, body, #grid, #grid > div { display:block; margin:0 } #grid { display:grid; grid-template-columns:repeat(4, 1fr) }",
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
                limits: LayoutLimits {
                    max_grid_tracks: 4,
                    ..LayoutLimits::default()
                },
                ..LayoutOptions::default()
            },
            &SimpleTextMeasurer,
        );
        assert!(
            layout
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == LayoutDiagnosticCode::GridTrackLimit)
        );
        assert!(
            layout
                .fragments
                .iter()
                .all(|fragment| fragment.source != Some(find(&output.dom, "#a")))
        );
    }

    #[test]
    fn single_line_row_honors_order_gap_justification_and_cross_axis_alignment() {
        let (output, _, layout) = pipeline(
            "<!doctype html><body><div id='flex'><span id='a'>A</span><span id='b'>B</span><span id='c'>C</span></div></body>",
            "html, body { display:block; margin:0 } #flex { display:flex; width:500px; height:100px; gap:20px; justify-content:center; align-items:center } #flex > span { width:100px; height:20px } #b { order:-1 }",
            500.0,
        );
        let a = layout
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(find(&output.dom, "#a")))
            .unwrap();
        let b = layout
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(find(&output.dom, "#b")))
            .unwrap();
        let c = layout
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(find(&output.dom, "#c")))
            .unwrap();

        assert_eq!(b.rect, PhysicalRect::new(80.0, 40.0, 100.0, 20.0));
        assert_eq!(a.rect, PhysicalRect::new(200.0, 40.0, 100.0, 20.0));
        assert_eq!(c.rect, PhysicalRect::new(320.0, 40.0, 100.0, 20.0));
    }

    #[test]
    fn flex_grow_and_shrink_distribute_content_box_space_with_gap() {
        let (output, _, grown) = pipeline(
            "<!doctype html><body><div id='flex'><div id='a'></div><div id='b'></div></div></body>",
            "html, body, #a, #b { display:block; margin:0 } #flex { display:flex; width:300px; gap:20px } #a, #b { flex-basis:100px } #a { flex-grow:1 } #b { flex-grow:2 }",
            300.0,
        );
        let a = grown
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(find(&output.dom, "#a")))
            .unwrap();
        let b = grown
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(find(&output.dom, "#b")))
            .unwrap();
        assert!((a.rect.size.width - 126.666_67).abs() < 0.001);
        assert!((b.rect.size.width - 153.333_33).abs() < 0.001);
        assert!((b.rect.origin.x - 146.666_67).abs() < 0.001);

        let (output, _, shrunk) = pipeline(
            "<!doctype html><body><div id='flex'><div id='a'></div><div id='b'></div></div></body>",
            "html, body, #a, #b { display:block; margin:0 } #flex { display:flex; width:150px; gap:10px } #a, #b { flex-basis:100px; flex-shrink:1 }",
            150.0,
        );
        let widths = ["#a", "#b"].map(|selector| {
            shrunk
                .fragments
                .iter()
                .find(|fragment| fragment.source == Some(find(&output.dom, selector)))
                .unwrap()
                .rect
                .size
                .width
        });
        assert_eq!(widths, [70.0, 70.0]);
    }

    #[test]
    fn flex_basis_honors_border_box_padding_and_border_constraints() {
        let (output, _, layout) = pipeline(
            "<!doctype html><body><div id='flex'><div id='item'></div></div></body>",
            "html, body, #item { display:block; margin:0 } #flex { display:flex; width:200px; justify-content:center } #item { flex-basis:100px; box-sizing:border-box; padding-left:10px; padding-right:10px; border-left-width:5px; border-right-width:5px; border-left-style:solid; border-right-style:solid }",
            200.0,
        );
        let item = layout
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(find(&output.dom, "#item")))
            .unwrap();
        let FragmentKind::Box(geometry) = &item.kind else {
            panic!("expected flex item box")
        };
        assert_eq!(item.rect, PhysicalRect::new(50.0, 0.0, 100.0, 0.0));
        assert_eq!(geometry.content_rect.size.width, 70.0);
    }

    #[test]
    fn definite_height_column_uses_main_axis_gap_and_alignment() {
        let (output, _, layout) = pipeline(
            "<!doctype html><body><div id='flex'><div id='a'></div><div id='b'></div></div></body>",
            "html, body, #a, #b { display:block; margin:0 } #flex { display:flex; flex-direction:column; width:200px; height:300px; justify-content:space-between; align-items:center; row-gap:10px } #a, #b { flex-basis:50px; width:40px }",
            200.0,
        );
        let rects = ["#a", "#b"].map(|selector| {
            layout
                .fragments
                .iter()
                .find(|fragment| fragment.source == Some(find(&output.dom, selector)))
                .unwrap()
                .rect
        });
        assert_eq!(rects[0], PhysicalRect::new(80.0, 0.0, 40.0, 50.0));
        assert_eq!(rects[1], PhysicalRect::new(80.0, 250.0, 40.0, 50.0));
    }

    #[test]
    fn flex_main_axis_auto_margins_absorb_positive_free_space() {
        let (output, _, row) = pipeline(
            "<!doctype html><body><div id='row'><div id='a'></div><div id='b'></div></div></body>",
            "html, body, #a, #b { display:block; margin:0 } #row { display:flex; width:300px } #a, #b { flex:0 0 50px } #b { margin-left:auto }",
            300.0,
        );
        let b = row
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(find(&output.dom, "#b")))
            .unwrap();
        assert_eq!(b.rect.origin.x, 250.0);

        let (output, _, column) = pipeline(
            "<!doctype html><body><div id='column'><div id='a'></div><div id='b'></div></div></body>",
            "html, body, #a, #b { display:block; margin:0 } #column { display:flex; flex-direction:column; width:100px; height:300px } #a, #b { flex:0 0 50px } #b { margin-top:auto }",
            100.0,
        );
        let b = column
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(find(&output.dom, "#b")))
            .unwrap();
        assert_eq!(b.rect.origin.y, 250.0);
    }

    #[test]
    fn class_mutation_rebuilds_flex_geometry_for_the_new_dom_revision() {
        let mut output = parse_document(
            "<!doctype html><body><div id='flex' class='row'><div id='a'></div><div id='b'></div></div></body>",
        );
        let sheet = parse_stylesheet(
            "html, body, #a, #b { display:block; margin:0 } #flex { display:flex; width:200px; height:200px } #flex.row { flex-direction:row } #flex.column { flex-direction:column } #a, #b { flex-basis:50px }",
        );
        let render = |dom: &crate::dom::Dom| {
            let styles = compute_document_styles(
                dom,
                &[CascadeInput {
                    sheet: &sheet,
                    origin: CascadeOrigin::Author,
                }],
                &PropertyRegistry::standard_baseline(),
                &ComputationLimits::default(),
                &MatchContext::default(),
            );
            let formatting = build_formatting_tree(dom, &styles, &FormattingLimits::default());
            layout_formatting_tree(
                dom,
                &formatting,
                &styles,
                LayoutOptions {
                    viewport: crate::layout::PhysicalSize {
                        width: 200.0,
                        height: 200.0,
                    },
                    ..LayoutOptions::default()
                },
                &SimpleTextMeasurer,
            )
        };
        let flex = find(&output.dom, "#flex");
        let b = find(&output.dom, "#b");
        let before = render(&output.dom);
        let before_rect = before
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(b))
            .unwrap()
            .rect;

        output.dom.set_attribute(flex, "class", "column").unwrap();
        let after = render(&output.dom);
        let after_rect = after
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(b))
            .unwrap()
            .rect;
        assert_eq!(after.fragments.dom_revision, output.dom.revision());
        assert!(before_rect.origin.x > after_rect.origin.x);
        assert!(after_rect.origin.y > before_rect.origin.y);
    }

    #[test]
    fn flex_layout_stops_cleanly_at_the_fragment_limit() {
        let output = parse_document(
            "<!doctype html><body><div id='flex'><div></div><div></div><div></div><div></div></div></body>",
        );
        let sheet = parse_stylesheet("html, body, div { display:block } #flex { display:flex }");
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
                limits: LayoutLimits {
                    max_fragments: 4,
                    ..LayoutLimits::default()
                },
                ..LayoutOptions::default()
            },
            &SimpleTextMeasurer,
        );
        assert!(layout.fragments.iter().count() <= 4);
        assert!(
            layout
                .diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.code == LayoutDiagnosticCode::FragmentLimit })
        );
    }
}
