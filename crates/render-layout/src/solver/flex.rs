//! Deterministic reference layout for block and inline formatting contexts.

use crate::fragment::FragmentId;
use crate::geometry::PhysicalRect;
use crate::solver::FlexItem;
use crate::solver::Solver;
use crate::solver::inline::align_offset;
use crate::solver::inline::justify_offsets;
use crate::solver::resolve::count_as_f32;
use crate::solver::resolve::length_depends_on_percentage;
use crate::tree::FormattingContextKind;
use crate::tree::FormattingNodeId;
use crate::tree::FormattingNodeKind;
use render_css::computed::ComputedStyle;
use render_css::properties::AlignItems;
use render_css::properties::AutoLengthPercentage;
use render_css::properties::BoxSizing;
use render_css::properties::FlexBasis;
use render_css::properties::FlexDirection;
use render_css::properties::JustifyContent;
use render_css::properties::Size;
use render_css::properties::TypedPropertyValue;
use render_dom::NodeId;

impl Solver<'_> {
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub(super) fn layout_flex_children(
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
        // The main axis basis is definite when the container has a definite
        // main size; on the inline axis it always is (containing block
        // widths never depend on content).
        let main_axis_definite = horizontal || specified_height.is_some();
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
                let basis = self.flex_basis(
                    *node,
                    style,
                    horizontal,
                    main_size,
                    main_axis_definite,
                    source,
                );
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
                    min_outer: 0.0,
                    fragment: None,
                    natural_outer_cross: 0.0,
                    auto_main_before: Self::margin_is_auto(style, before_property),
                    auto_main_after: Self::margin_is_auto(style, after_property),
                })
            })
            .collect::<Vec<_>>();
        items.sort_by_key(|item| item.order);
        for item in &mut items {
            let node = item.node;
            let source = item.source;
            let style = self
                .formatting
                .get(node)
                .and_then(|node| node.style_source)
                .and_then(|source| self.styles.get(&source))
                .cloned();
            let property = if horizontal {
                "min-width"
            } else {
                "min-height"
            };
            let automatic_minimum = style
                .as_ref()
                .and_then(|style| style.typed(property))
                .is_none_or(|value| matches!(value, TypedPropertyValue::Size(Size::Auto)));
            if automatic_minimum {
                let intrinsic = self.intrinsic_flex_size(node, horizontal, main_size, 0);
                item.min_outer = (intrinsic
                    + self.flex_outer_extras(
                        style.as_ref(),
                        horizontal,
                        containing.size.width,
                        source,
                    ))
                .max(0.0);
            }
        }
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
                Some(FormattingNodeKind::BlockContainer { .. }) => self
                    .layout_block_with_containing_height(
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
                        // Flex items are in-flow: their percentage heights are
                        // definite only against a definite container height
                        // (CSS 2 §10.5).
                        specified_height.is_some(),
                    ),
                _ => self.layout_anonymous_block(
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

    pub(super) fn distribute_flex_space(items: &mut [FlexItem], available_without_gaps: f32) {
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
                    item.target_outer = (item.base_outer
                        + free * item.shrink * item.base_outer / scaled)
                        .max(item.min_outer);
                }
            }
        }
    }

    pub(super) fn flex_basis(
        &mut self,
        node: FormattingNodeId,
        style: Option<&ComputedStyle>,
        horizontal: bool,
        basis: f32,
        basis_definite: bool,
        source: Option<NodeId>,
    ) -> f32 {
        let basis_value = if basis_definite { Some(basis) } else { None };
        match style.and_then(|style| style.typed("flex-basis")) {
            Some(TypedPropertyValue::FlexBasis(FlexBasis::LengthPercentage(value))) => {
                if !basis_definite && length_depends_on_percentage(value) {
                    // A percentage flex-basis against an indefinite main size
                    // resolves as `content` (CSS Flexbox §7.2.2).
                    self.intrinsic_flex_size(node, horizontal, basis, 0)
                } else {
                    let specified = self.resolve_length(value, basis, source, "flex-basis");
                    self.flex_basis_content_box(style, specified, horizontal, basis, source)
                }
            }
            Some(TypedPropertyValue::FlexBasis(FlexBasis::Auto)) | None => {
                let property = if horizontal { "width" } else { "height" };
                match self.resolve_size_against(style, property, basis_value, source) {
                    Some(specified) => {
                        self.flex_basis_content_box(style, specified, horizontal, basis, source)
                    }
                    None => self.intrinsic_flex_size(node, horizontal, basis, 0),
                }
            }
            Some(TypedPropertyValue::FlexBasis(FlexBasis::Content)) => {
                self.intrinsic_flex_size(node, horizontal, basis, 0)
            }
            _ => 0.0,
        }
        .max(0.0)
    }

    pub(super) fn intrinsic_flex_size(
        &mut self,
        node: FormattingNodeId,
        horizontal: bool,
        basis: f32,
        depth: usize,
    ) -> f32 {
        if depth > self.options.limits.max_depth {
            return 0.0;
        }
        let Some(node) = self.formatting.get(node) else {
            return 0.0;
        };
        if let FormattingNodeKind::Text(text) = &node.kind {
            let style = self.text_style(node.style_source);
            return if horizontal {
                self.intrinsic_text_width(text, node.style_source, style)
            } else if text.chars().all(char::is_whitespace) {
                0.0
            } else {
                style.line_height
            };
        }
        let style = node
            .style_source
            .and_then(|source| self.styles.get(&source))
            .cloned();
        let is_flex = matches!(
            node.kind,
            FormattingNodeKind::BlockContainer {
                context: FormattingContextKind::Flex
            }
        );
        let children = node.children.clone();
        let content = children
            .iter()
            .map(|child| {
                let child_size =
                    self.intrinsic_flex_size(*child, horizontal, basis, depth.saturating_add(1));
                if !is_flex {
                    return child_size;
                }
                let child_node = self.formatting.get(*child);
                let child_source = child_node.and_then(|node| node.source);
                let child_style = child_node
                    .and_then(|node| node.style_source)
                    .and_then(|source| self.styles.get(&source))
                    .cloned();
                child_size
                    + self.flex_outer_extras(child_style.as_ref(), horizontal, basis, child_source)
            })
            .sum::<f32>();
        let content = if is_flex {
            content
                + self.resolve_gap(
                    style.as_ref(),
                    if horizontal { "column-gap" } else { "row-gap" },
                    basis,
                    node.source,
                ) * count_as_f32(children.len().saturating_sub(1))
        } else {
            content
        };
        let property = if horizontal { "width" } else { "height" };
        self.resolve_size(style.as_ref(), property, basis, node.source)
            .map_or(content, |specified| specified.max(content))
    }

    pub(super) fn flex_outer_extras(
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

    pub(super) fn flex_non_margin_extras(
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

    pub(super) fn flex_basis_content_box(
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

    pub(super) fn flex_content_width(
        &mut self,
        style: Option<&ComputedStyle>,
        outer_width: f32,
        basis: f32,
        source: Option<NodeId>,
    ) -> f32 {
        (outer_width - self.flex_outer_extras(style, true, basis, source)).max(0.0)
    }

    pub(super) fn flex_cross_outer_size(
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
                .unwrap_or_else(|| {
                    // Non-stretch items with an auto cross size use their
                    // fit-content size: the max-content intrinsic clamped to
                    // the flex line (CSS Flexbox §9.4.8 uses fit-content;
                    // the min-content floor is not modeled yet). Without the
                    // clamp, an auto-width child of a `flex-direction:column;
                    // align-items:center` container keeps its full
                    // max-content width and is centered into negative
                    // coordinates, pushing it off-screen (zhihu signin card).
                    self.intrinsic_flex_size(node, true, available, 0)
                        .min((available - extras).max(0.0))
                })
                + extras
        }
    }

    pub(super) fn flex_basis_is_auto(style: Option<&ComputedStyle>) -> bool {
        matches!(
            style.and_then(|style| style.typed("flex-basis")),
            Some(TypedPropertyValue::FlexBasis(FlexBasis::Auto)) | None
        )
    }

    pub(super) fn margin_is_auto(style: Option<&ComputedStyle>, property: &str) -> bool {
        matches!(
            style.and_then(|style| style.typed(property)),
            Some(TypedPropertyValue::Margin(AutoLengthPercentage::Auto))
        )
    }

    pub(super) fn flex_cross_is_auto(&self, node: FormattingNodeId, property: &str) -> bool {
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
}
