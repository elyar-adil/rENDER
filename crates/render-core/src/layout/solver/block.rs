//! Deterministic reference layout for block and inline formatting contexts.

use crate::css::computed::ComputedStyle;
use crate::css::properties::BoxSizing;
use crate::css::properties::Clear;
use crate::css::properties::Display;
use crate::css::properties::DisplayInside;
use crate::css::properties::Float;
use crate::css::properties::Overflow;
use crate::css::properties::Position;
use crate::css::properties::TypedPropertyValue;
use crate::dom::NodeId;
use crate::layout::fragment::BoxGeometry;
use crate::layout::fragment::FragmentId;
use crate::layout::fragment::FragmentKind;
use crate::layout::geometry::EdgeSizes;
use crate::layout::geometry::PhysicalRect;
use crate::layout::solver::BlockResult;
use crate::layout::solver::FloatArea;
use crate::layout::solver::LayoutDiagnostic;
use crate::layout::solver::LayoutDiagnosticCode;
use crate::layout::solver::Solver;
use crate::layout::solver::resolve::position;
use crate::layout::tree::FormattingContextKind;
use crate::layout::tree::FormattingNodeId;
use crate::layout::tree::FormattingNodeKind;

pub(super) fn establishes_block_formatting_context(style: Option<&ComputedStyle>) -> bool {
    ["overflow-x", "overflow-y"].into_iter().any(|property| {
        matches!(
            style.and_then(|style| style.typed(property)),
            Some(TypedPropertyValue::Overflow(value)) if !matches!(value, Overflow::Visible)
        )
    })
}

pub(super) fn float_band(
    floats: &[FloatArea],
    y: f32,
    mut left: f32,
    mut right: f32,
) -> (f32, f32) {
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

pub(super) fn inline_float_band(
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

pub(super) fn float_line_band(
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

impl Solver<'_> {
    pub(super) fn layout_block_like(
        &mut self,
        node_id: FormattingNodeId,
        containing: PhysicalRect,
        positioning_containing: PhysicalRect,
        margin_box_y: f32,
        depth: usize,
        containing_height_definite: bool,
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
            | FormattingNodeKind::Text(_) => self.layout_anonymous_block(
                node_id,
                containing,
                positioning_containing,
                margin_box_y,
                depth,
            ),
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
                self.layout_block_with_containing_height(
                    node_id,
                    containing,
                    positioning_containing,
                    margin_box_y,
                    depth,
                    None,
                    containing_height_definite,
                )
            }
            FormattingNodeKind::AtomicInline { .. } => self.layout_block_with_containing_height(
                node_id,
                containing,
                positioning_containing,
                margin_box_y,
                depth,
                None,
                containing_height_definite,
            ),
            FormattingNodeKind::Root => None,
        }
    }

    /// Entry point for callers that do not track containing-height
    /// definiteness (the inline solver's atomic inlines). Their percentage
    /// heights take the CSS 2 §10.5 conservative default: an auto-height
    /// inline formatting context is indefinite, so the height computes to
    /// `auto`.
    pub(super) fn layout_block(
        &mut self,
        node_id: FormattingNodeId,
        containing: PhysicalRect,
        positioning_containing: PhysicalRect,
        margin_box_y: f32,
        depth: usize,
        forced_content_width: Option<f32>,
    ) -> Option<BlockResult> {
        self.layout_block_with_containing_height(
            node_id,
            containing,
            positioning_containing,
            margin_box_y,
            depth,
            forced_content_width,
            false,
        )
    }

    // Keep the CSS block constraint algorithm in specification order so each
    // sizing and auto-margin step remains directly auditable against CSS 2.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub(super) fn layout_block_with_containing_height(
        &mut self,
        node_id: FormattingNodeId,
        containing: PhysicalRect,
        positioning_containing: PhysicalRect,
        margin_box_y: f32,
        depth: usize,
        forced_content_width: Option<f32>,
        containing_height_definite: bool,
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
        // An out-of-flow box is never subject to the CSS 2 §10.5
        // "percentage height computes to auto" rule: its containing block is
        // the positioning ancestor's padding box (or the viewport), whose
        // used height is known by the time positioned children are laid out.
        let (containing, containing_height_definite) = match position {
            Position::Fixed => (
                PhysicalRect::new(
                    0.0,
                    0.0,
                    self.options.viewport.width,
                    self.options.viewport.height,
                ),
                true,
            ),
            Position::Absolute => (positioning_containing, true),
            _ => (containing, containing_height_definite),
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
        // CSS 2 §10.5: against an indefinite containing height a percentage
        // height computes to `auto`; it must never resolve against the
        // tentative 0 used value of a content-sized parent.
        let css_height = self.resolve_size_against(
            style,
            "height",
            containing_height_definite.then_some(containing.size.height),
            node.source,
        );
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
        } else if out_of_flow {
            // CSS 2 §10.3.7: with a definite width, auto margins absorb the
            // space left by both definite insets; otherwise auto margins are
            // zero and the auto inset resolves from the constraint equation
            // (handled by the out-of-flow anchor below). The in-flow
            // over-constrained margin adjustment must not run here.
            let definite_insets = left.unwrap_or(0.0) + right.unwrap_or(0.0);
            let remaining = containing.size.width
                - definite_insets
                - content_width
                - non_content
                - margin_left.value
                - margin_right.value;
            match (margin_left.auto, margin_right.auto) {
                (true, true) if remaining >= 0.0 => {
                    margin_left.value = remaining / 2.0;
                    margin_right.value = remaining / 2.0;
                }
                // Equal auto margins would go negative, so the left margin
                // (ltr) is zeroed and the right margin solves the equation.
                (true, true) => {
                    margin_left.value = 0.0;
                    margin_right.value = remaining;
                }
                (true, false) => margin_left.value = remaining,
                (false, true) => margin_right.value = remaining,
                _ => {}
            }
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
        // A specified (or replaced) content height is definite, so in-flow
        // children may resolve percentage heights against it; a content-sized
        // parent is indefinite (CSS 2 §10.5) and only contributes a tentative
        // 0 basis that children must ignore.
        let child_containing_height = specified_content_height.unwrap_or(0.0);
        let child_containing_height_definite = specified_content_height.is_some();
        let top = self.resolve_inset(style, "top", containing.size.height, node.source);
        let bottom = self.resolve_inset(style, "bottom", containing.size.height, node.source);
        let relative_offset = if position == Position::Relative {
            let horizontal = match (left, right) {
                (Some(left), _) => left,
                (None, Some(right)) => -right,
                (None, None) => 0.0,
            };
            let vertical = match (top, bottom) {
                (Some(top), _) => top,
                (None, Some(bottom)) => -bottom,
                (None, None) => 0.0,
            };
            (horizontal, vertical)
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
                                PhysicalRect::new(
                                    content_x,
                                    content_y,
                                    content_width,
                                    child_containing_height,
                                ),
                                positioned_child_containing,
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
                                    child_containing_height,
                                ),
                                positioned_child_containing,
                                cursor_y,
                                depth.saturating_add(1),
                                child_containing_height_definite,
                            )
                        };
                        if let Some(result) = result {
                            cursor_y += result.outer_height;
                            children.push(result.fragment);
                        }
                    } else if let Some((fragment, area)) = self.layout_float(
                        child,
                        float,
                        PhysicalRect::new(
                            content_x,
                            content_y,
                            content_width,
                            child_containing_height,
                        ),
                        positioned_child_containing,
                        cursor_y,
                        &floats,
                        depth.saturating_add(1),
                        child_containing_height_definite,
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
            containing_height_definite,
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
                // Positioned children resolve against used heights: the
                // final containing rects above are the post-layout padding
                // box (or viewport), never a tentative content estimate.
                true,
            ) {
                children.push(result.fragment);
            }
        }
        // Positioned descendants are laid out separately, but their paint
        // order still follows z-index. Sorting the completed fragments keeps
        // low-index overlays (such as a decorative border) behind higher
        // positioned content while preserving source order within a layer.
        children.sort_by_key(|child| self.fragment_z_index(*child));
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

    pub(super) fn layout_anonymous_block(
        &mut self,
        node_id: FormattingNodeId,
        containing: PhysicalRect,
        positioning_containing: PhysicalRect,
        y: f32,
        depth: usize,
    ) -> Option<BlockResult> {
        self.layout_anonymous_block_with_floats(
            node_id,
            containing,
            positioning_containing,
            y,
            depth,
            &[],
        )
    }

    pub(super) fn layout_anonymous_block_with_floats(
        &mut self,
        node_id: FormattingNodeId,
        containing: PhysicalRect,
        positioning_containing: PhysicalRect,
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
            // Its containing block is the auto-height anonymous wrapper, so
            // its percentage heights stay indefinite (CSS 2 §10.5).
            return self.layout_block_with_containing_height(
                *inline_root,
                containing,
                positioning_containing,
                y,
                depth,
                None,
                false,
            );
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
            positioning_containing,
            self.text_align(node.style_source),
            depth.saturating_add(1),
            floats,
        );
        self.finish_box(fragment, height, children);
        Some(BlockResult {
            fragment,
            outer_height: height,
        })
    }

    pub(super) fn source_is_out_of_flow(&self, source: Option<NodeId>) -> bool {
        source.is_some_and(|source| {
            matches!(
                position(self.styles.get(&source)),
                Position::Absolute | Position::Fixed
            )
        })
    }

    pub(super) fn is_out_of_flow(&self, node: FormattingNodeId) -> bool {
        let style = self
            .formatting
            .get(node)
            .and_then(|node| node.style_source)
            .and_then(|source| self.styles.get(&source));
        matches!(position(style), Position::Absolute | Position::Fixed)
    }

    pub(super) fn float_side(&self, node: FormattingNodeId) -> Float {
        let Some(formatting_node) = self.formatting.get(node) else {
            return Float::None;
        };
        // Anonymous inline wrappers borrow their parent's text style, but
        // float is not inherited and the wrapper has no box of its own.
        let Some(source) = formatting_node.source else {
            return Float::None;
        };
        let style = self.styles.get(&source);
        match style.and_then(|style| style.typed("float")) {
            Some(TypedPropertyValue::Float(Float::Left | Float::InlineStart)) => Float::Left,
            Some(TypedPropertyValue::Float(Float::Right | Float::InlineEnd)) => Float::Right,
            _ => Float::None,
        }
    }

    pub(super) fn cleared_y(&self, node: FormattingNodeId, y: f32, floats: &[FloatArea]) -> f32 {
        let Some(formatting_node) = self.formatting.get(node) else {
            return y;
        };
        // Anonymous inline wrappers borrow the surrounding text style, but
        // clear is a box property and must not be inherited by the wrapper.
        let style = formatting_node
            .source
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
    pub(super) fn layout_float(
        &mut self,
        node: FormattingNodeId,
        side: Float,
        containing: PhysicalRect,
        positioning_containing: PhysicalRect,
        mut y: f32,
        floats: &[FloatArea],
        depth: usize,
        containing_height_definite: bool,
    ) -> Option<(FragmentId, FloatArea)> {
        loop {
            let (left, right) = float_band(floats, y, containing.origin.x, containing.right());
            let available = (right - left).max(0.0);
            let forced_content_width = self.atomic_inline_content_width(node, available);
            let result = self.layout_block_with_containing_height(
                node,
                PhysicalRect::new(left, containing.origin.y, available, containing.size.height),
                positioning_containing,
                y,
                depth,
                Some(forced_content_width),
                containing_height_definite,
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
}
