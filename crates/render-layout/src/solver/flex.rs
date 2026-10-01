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
use render_css::properties::AlignContent;
use render_css::properties::AlignItems;
use render_css::properties::AlignSelf;
use render_css::properties::AutoLengthPercentage;
use render_css::properties::BoxSizing;
use render_css::properties::FlexBasis;
use render_css::properties::FlexDirection;
use render_css::properties::FlexWrap;
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
        let wrapping = match container_style.and_then(|style| style.typed("flex-wrap")) {
            Some(TypedPropertyValue::FlexWrap(value)) => *value,
            _ => FlexWrap::NoWrap,
        };
        let direction = match container_style.and_then(|style| style.typed("flex-direction")) {
            Some(TypedPropertyValue::FlexDirection(value)) => *value,
            _ => FlexDirection::Row,
        };
        if wrapping != FlexWrap::NoWrap
            && matches!(direction, FlexDirection::Row | FlexDirection::RowReverse)
            && !children.is_empty()
        {
            return self.layout_wrapped_flex_rows(
                children,
                containing,
                positioning_containing,
                specified_height,
                depth,
                container_style,
                container_source,
                wrapping == FlexWrap::WrapReverse,
            );
        }
        if wrapping != FlexWrap::NoWrap
            && matches!(
                direction,
                FlexDirection::Column | FlexDirection::ColumnReverse
            )
            && let Some(height) = specified_height
            && !children.is_empty()
        {
            return self.layout_wrapped_flex_columns(
                children,
                containing,
                positioning_containing,
                height,
                depth,
                container_style,
                container_source,
                wrapping == FlexWrap::WrapReverse,
            );
        }
        self.layout_flex_line(
            children,
            containing,
            positioning_containing,
            specified_height,
            depth,
            container_style,
            container_source,
        )
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn layout_wrapped_flex_rows(
        &mut self,
        children: &[FormattingNodeId],
        containing: PhysicalRect,
        positioning_containing: PhysicalRect,
        specified_height: Option<f32>,
        depth: usize,
        container_style: Option<&ComputedStyle>,
        container_source: Option<NodeId>,
        wrap_reverse: bool,
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
        let mut ordered = children.to_vec();
        ordered.sort_by_key(|id| {
            let style = self
                .formatting
                .get(*id)
                .and_then(|node| node.style_source)
                .and_then(|source| self.styles.get(&source));
            match style.and_then(|style| style.typed("order")) {
                Some(TypedPropertyValue::Order(value)) => *value,
                _ => 0,
            }
        });
        let mut lines: Vec<Vec<FormattingNodeId>> = vec![Vec::new()];
        let mut line_width = 0.0;
        for node in ordered {
            let source = self.formatting.get(node).and_then(|node| node.source);
            let style = self
                .formatting
                .get(node)
                .and_then(|node| node.style_source)
                .and_then(|source| self.styles.get(&source));
            let basis = self.flex_basis(node, style, true, containing.size.width, true, source);
            let extras = self.flex_outer_extras(style, true, containing.size.width, source);
            let minimum = match style.and_then(|style| style.typed("min-width")) {
                Some(TypedPropertyValue::Size(Size::Auto)) | None => {
                    self.intrinsic_flex_size(node, true, containing.size.width, 0)
                }
                _ => self
                    .resolve_size_against(style, "min-width", Some(containing.size.width), source)
                    .unwrap_or(0.0),
            };
            let outer = (basis.max(minimum) + extras).max(0.0);
            let next_width = if lines.last().is_some_and(Vec::is_empty) {
                outer
            } else {
                line_width + column_gap + outer
            };
            if next_width > containing.size.width
                && lines.last().is_some_and(|line| !line.is_empty())
            {
                lines.push(Vec::new());
                line_width = outer;
            } else {
                line_width = next_width;
            }
            lines.last_mut().expect("at least one flex line").push(node);
        }

        let mut laid_out = Vec::with_capacity(lines.len());
        for line in &lines {
            // A line is sized against the full inline size. The cross size is
            // initially intrinsic; after all lines have been measured a
            // definite container can distribute extra cross space to them.
            let (fragments, height) = self.layout_flex_line(
                line,
                containing,
                positioning_containing,
                None,
                depth,
                container_style,
                container_source,
            );
            laid_out.push((fragments, height));
        }
        let natural_height = laid_out.iter().map(|(_, height)| height).sum::<f32>()
            + row_gap * count_as_f32(laid_out.len().saturating_sub(1));
        let free_cross = specified_height.map_or(0.0, |height| (height - natural_height).max(0.0));
        let align_content = match container_style.and_then(|style| style.typed("align-content")) {
            Some(TypedPropertyValue::AlignContent(value)) => *value,
            _ => AlignContent::Normal,
        };
        let line_count = laid_out.len();
        let (extra_per_line, line_offset, extra_gap) =
            Self::flex_line_distribution(align_content, line_count, free_cross);
        let used_height = specified_height
            .unwrap_or(natural_height)
            .max(natural_height);
        let align = match container_style.and_then(|style| style.typed("align-items")) {
            Some(TypedPropertyValue::AlignItems(AlignItems::Normal)) => AlignItems::Stretch,
            Some(TypedPropertyValue::AlignItems(value)) => *value,
            _ => AlignItems::Stretch,
        };
        let mut cursor = line_offset;
        let mut fragments = Vec::new();
        for (line_fragments, natural_line_height) in laid_out {
            let line_height = natural_line_height + extra_per_line;
            let line_y = if wrap_reverse {
                used_height - cursor - line_height
            } else {
                cursor
            };
            for fragment in line_fragments {
                let node = self
                    .fragments
                    .get(fragment.as_u32() as usize)
                    .map(|fragment| fragment.formatting_node);
                let item_align = node.map_or(align, |node| self.flex_item_align(node, align));
                let auto_height = node.is_some_and(|node| self.flex_cross_is_auto(node, "height"));
                let original_height = self
                    .fragment_outer_rect(fragment)
                    .map_or(0.0, |rect| rect.size.height);
                let cross_shift = if item_align == AlignItems::Stretch && auto_height {
                    self.stretch_fragment_outer_height(fragment, line_height);
                    0.0
                } else {
                    align_offset(item_align, line_height, original_height)
                        - align_offset(item_align, natural_line_height, original_height)
                };
                self.translate_fragment_subtree(fragment, 0.0, line_y + cross_shift);
                fragments.push(fragment);
            }
            cursor += line_height + row_gap + extra_gap;
        }
        (fragments, used_height)
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn layout_wrapped_flex_columns(
        &mut self,
        children: &[FormattingNodeId],
        containing: PhysicalRect,
        positioning_containing: PhysicalRect,
        specified_height: f32,
        depth: usize,
        container_style: Option<&ComputedStyle>,
        container_source: Option<NodeId>,
        wrap_reverse: bool,
    ) -> (Vec<FragmentId>, f32) {
        let row_gap = self.resolve_gap(
            container_style,
            "row-gap",
            specified_height,
            container_source,
        );
        let column_gap = self.resolve_gap(
            container_style,
            "column-gap",
            containing.size.width,
            container_source,
        );
        let mut ordered = children.to_vec();
        ordered.sort_by_key(|id| {
            let style = self
                .formatting
                .get(*id)
                .and_then(|node| node.style_source)
                .and_then(|source| self.styles.get(&source));
            match style.and_then(|style| style.typed("order")) {
                Some(TypedPropertyValue::Order(value)) => *value,
                _ => 0,
            }
        });
        let mut columns: Vec<Vec<FormattingNodeId>> = vec![Vec::new()];
        let mut column_widths = vec![0.0_f32];
        let mut column_height = 0.0;
        for node in ordered {
            let source = self.formatting.get(node).and_then(|node| node.source);
            let style = self
                .formatting
                .get(node)
                .and_then(|node| node.style_source)
                .and_then(|source| self.styles.get(&source));
            let basis = self.flex_basis(node, style, false, specified_height, true, source);
            let vertical_extras =
                self.flex_outer_extras(style, false, containing.size.width, source);
            let minimum = match style.and_then(|style| style.typed("min-height")) {
                Some(TypedPropertyValue::Size(Size::Auto)) | None => {
                    self.intrinsic_flex_size(node, false, specified_height, 0)
                }
                _ => self
                    .resolve_size_against(style, "min-height", Some(specified_height), source)
                    .unwrap_or(0.0),
            };
            let outer_height = (basis.max(minimum) + vertical_extras).max(0.0);
            let next_height = if columns.last().is_some_and(Vec::is_empty) {
                outer_height
            } else {
                column_height + row_gap + outer_height
            };
            if next_height > specified_height
                && columns.last().is_some_and(|column| !column.is_empty())
            {
                columns.push(Vec::new());
                column_widths.push(0.0);
                column_height = outer_height;
            } else {
                column_height = next_height;
            }
            let width = self.intrinsic_flex_size(node, true, containing.size.width, 0)
                + self.flex_outer_extras(style, true, containing.size.width, source);
            let last_width = column_widths.last_mut().expect("at least one flex column");
            *last_width = last_width.max(width);
            columns
                .last_mut()
                .expect("at least one flex column")
                .push(node);
        }

        let total_width = column_widths.iter().sum::<f32>()
            + column_gap * count_as_f32(columns.len().saturating_sub(1));
        let free_cross = (containing.size.width - total_width).max(0.0);
        let align_content = match container_style.and_then(|style| style.typed("align-content")) {
            Some(TypedPropertyValue::AlignContent(value)) => *value,
            _ => AlignContent::Normal,
        };
        let (extra_per_column, offset, extra_gap) =
            Self::flex_line_distribution(align_content, columns.len(), free_cross);
        let used_width = containing.size.width.max(total_width);
        let mut cursor = offset;
        let mut fragments = Vec::new();
        for (column, natural_width) in columns.into_iter().zip(column_widths) {
            let width = natural_width + extra_per_column;
            let x = if wrap_reverse {
                containing.origin.x + used_width - cursor - width
            } else {
                containing.origin.x + cursor
            };
            let (column_fragments, _) = self.layout_flex_line(
                &column,
                PhysicalRect::new(x, containing.origin.y, width, specified_height),
                positioning_containing,
                Some(specified_height),
                depth,
                container_style,
                container_source,
            );
            fragments.extend(column_fragments);
            cursor += width + column_gap + extra_gap;
        }
        (fragments, specified_height)
    }

    fn flex_line_distribution(align: AlignContent, count: usize, free: f32) -> (f32, f32, f32) {
        if count == 1 || matches!(align, AlignContent::Normal | AlignContent::Stretch) {
            return (free / count_as_f32(count), 0.0, 0.0);
        }
        match align {
            AlignContent::FlexEnd | AlignContent::End => (0.0, free, 0.0),
            AlignContent::Center => (0.0, free / 2.0, 0.0),
            AlignContent::SpaceBetween => (0.0, 0.0, free / count_as_f32(count - 1)),
            AlignContent::SpaceAround => {
                let gap = free / count_as_f32(count);
                (0.0, gap / 2.0, gap)
            }
            AlignContent::SpaceEvenly => {
                let gap = free / count_as_f32(count + 1);
                (0.0, gap, gap)
            }
            _ => (0.0, 0.0, 0.0),
        }
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn layout_flex_line(
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
                // Flexbox §4.5: the used value of an `auto` main-axis minimum
                // size on a non-scrollable flex item is its content-based
                // minimum size, and §4.5's content size suggestion is "the
                // min-content size in the main axis". Max-content is the wrong
                // number here and the error is visible: a row of items whose
                // text can wrap would each hold their whole unwrapped line as a
                // floor, so nothing ever shrank and a narrow container overflowed
                // instead of wrapping. `flex_intrinsic_size` also keeps §4.5's
                // "capped by the specified size suggestion", which is what the
                // definite-size branch at the bottom of this function already
                // did for the max-content measurement.
                let intrinsic = if horizontal {
                    self.flex_intrinsic_size(node, horizontal, main_size, 0, true)
                } else {
                    self.intrinsic_flex_size(node, horizontal, main_size, 0)
                };
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
            let item_align = self.flex_item_align(item.node, align);
            let cross_outer = if horizontal {
                containing.size.width
            } else {
                self.flex_cross_outer_size(
                    item.node,
                    item_style,
                    cross_hint.unwrap_or(containing.size.width),
                    item_align,
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
            let item_align = self.flex_item_align(item.node, align);
            if item.auto_main_before {
                cursor += auto_main_margin;
            }
            if horizontal {
                if item_align == AlignItems::Stretch && self.flex_cross_is_auto(item.node, "height")
                {
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
                let cross_offset = align_offset(item_align, line_cross, outer.size.height);
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
                let cross_offset = align_offset(item_align, line_cross, outer.size.width);
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

    fn flex_item_align(&self, node: FormattingNodeId, inherited: AlignItems) -> AlignItems {
        let style = self
            .formatting
            .get(node)
            .and_then(|node| node.style_source)
            .and_then(|source| self.styles.get(&source));
        match style.and_then(|style| style.typed("align-self")) {
            Some(TypedPropertyValue::AlignSelf(AlignSelf::Normal | AlignSelf::Stretch)) => {
                AlignItems::Stretch
            }
            Some(TypedPropertyValue::AlignSelf(AlignSelf::FlexStart)) => AlignItems::FlexStart,
            Some(TypedPropertyValue::AlignSelf(AlignSelf::FlexEnd)) => AlignItems::FlexEnd,
            Some(TypedPropertyValue::AlignSelf(AlignSelf::Start)) => AlignItems::Start,
            Some(TypedPropertyValue::AlignSelf(AlignSelf::End)) => AlignItems::End,
            Some(TypedPropertyValue::AlignSelf(AlignSelf::Center)) => AlignItems::Center,
            _ => inherited,
        }
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
        self.flex_intrinsic_size(node, horizontal, basis, depth, false)
    }

    /// Flexbox §9.9: a box's main-axis intrinsic size, which `minimum` selects
    /// between the two halves of the pair - the max-content size (§10.3.5's
    /// "preferred width") and the min-content size (§10.3.5's "preferred
    /// minimum width").
    ///
    /// They are different numbers wherever there is a soft wrap opportunity to
    /// take, so a consumer that wants the smaller one must ask for it: §9.9.1's
    /// max-content main size is the preferred width of a flex item, while §4.5's
    /// content-based minimum size is its min-content width.
    pub(super) fn flex_intrinsic_size(
        &mut self,
        node: FormattingNodeId,
        horizontal: bool,
        basis: f32,
        depth: usize,
        minimum: bool,
    ) -> f32 {
        if depth > self.options.limits.max_depth {
            return 0.0;
        }
        let Some(entry) = self.formatting.get(node) else {
            return 0.0;
        };
        if let FormattingNodeKind::Text(text) = &entry.kind {
            let style_source = entry.style_source;
            let style = self.inline_text_style(style_source);
            return if horizontal {
                if minimum {
                    // §4.5's content size suggestion is "the min-content size
                    // in the main axis", which for a text run is the width of
                    // its widest unbreakable run - the same measurement §10.3.5
                    // defines, so the helper answers rather than a second one.
                    self.min_content_width(node)
                } else {
                    self.intrinsic_text_width(text, style_source, style)
                }
            } else if text.chars().all(char::is_whitespace) {
                0.0
            } else {
                style.style.line_height
            };
        }
        let style = entry
            .style_source
            .and_then(|source| self.styles.get(&source))
            .cloned();
        let is_flex = matches!(
            entry.kind,
            FormattingNodeKind::BlockContainer {
                context: FormattingContextKind::Flex
            }
        );
        let children = entry.children.clone();
        let mut line: f32 = 0.0;
        let mut widest: f32 = 0.0;
        let mut broadest: f32 = 0.0;
        let mut total: f32 = 0.0;
        for child in children.iter().copied() {
            let child_size = self.flex_intrinsic_size(
                child,
                horizontal,
                basis,
                depth.saturating_add(1),
                minimum,
            );
            let child_size = if is_flex {
                let child_node = self.formatting.get(child);
                let child_source = child_node.and_then(|node| node.source);
                let child_style = child_node
                    .and_then(|node| node.style_source)
                    .and_then(|source| self.styles.get(&source))
                    .cloned();
                child_size
                    + self.flex_outer_extras(child_style.as_ref(), horizontal, basis, child_source)
            } else if horizontal && self.is_forced_break(child) {
                // CSS 2.1 §10.3.5: the max-content width of an inline sequence
                // is its widest line, so a forced break ends the run instead of
                // adding to it.
                widest = widest.max(line);
                line = 0.0;
                0.0
            } else {
                child_size
            };
            broadest = broadest.max(child_size);
            line += child_size;
            total += child_size;
        }
        // A non-flex inline sequence reports the widest of its lines; the flex
        // case keeps its sum and adds the gap below. Under the min-content
        // measurement the same sequence reports the width of its widest
        // unbreakable run, which is a property of the sequence rather than of
        // any one of its children, and a non-flex block container reports the
        // widest of its block children.
        let content = if !is_flex && horizontal && minimum {
            if matches!(
                entry.kind,
                FormattingNodeKind::AnonymousBlock | FormattingNodeKind::Inline
            ) {
                self.inline_sequence_min_content_width(&children, depth)
            } else {
                broadest
            }
        } else if !is_flex && horizontal {
            widest.max(line)
        } else {
            total
        };
        let content = if is_flex {
            content
                + self.resolve_gap(
                    style.as_ref(),
                    if horizontal { "column-gap" } else { "row-gap" },
                    basis,
                    entry.source,
                ) * count_as_f32(children.len().saturating_sub(1))
        } else {
            content
        };
        let property = if horizontal { "width" } else { "height" };
        // Intrinsic sizing treats percentage-dependent sizes as auto: the
        // percentage basis would itself depend on content (CSS Sizing §5),
        // so a `width:100%` descendant must not resolve against the
        // available space here. A definite size caps the box's intrinsic
        // contribution, which is §4.5's "capped by the specified size
        // suggestion" for the min-content measurement and the max-content
        // counterpart for the other: the intrinsic size of a box with a
        // definite size is that size, however wide its contents are.
        self.resolve_size_against(style.as_ref(), property, None, entry.source)
            .unwrap_or(content)
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
                    // CSS Flexbox §9.4.8: an auto cross size is the fit-content
                    // size, `min(max-content, max(min-content, available))`.
                    // Without the min-content floor the clamp is a bare
                    // `min`, and an item whose longest unbreakable run is wider
                    // than the flex line comes out narrower than that run and
                    // wraps inside itself instead of overflowing the line. The
                    // clamp is also what keeps a non-stretch item of an
                    // `align-items:center` column from keeping its full
                    // max-content width and being centered into negative
                    // coordinates (zhihu signin card).
                    let available = (available - extras).max(0.0);
                    f32::min(
                        self.intrinsic_flex_size(node, true, available, 0),
                        self.flex_intrinsic_size(node, true, available, 0, true)
                            .max(available),
                    )
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
