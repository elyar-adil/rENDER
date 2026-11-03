//! Deterministic reference layout for block and inline formatting contexts.

use crate::fragment::FragmentId;
use crate::geometry::PhysicalRect;
use crate::grid::GridLimitError;
use crate::grid::TrackSizing;
use crate::grid::automatic_position;
use crate::grid::expand_auto_repeat;
use crate::grid::required_rows;
use crate::grid::size_axis;
use crate::solver::GridItem;
use crate::solver::LayoutDiagnostic;
use crate::solver::LayoutDiagnosticCode;
use crate::solver::Solver;
use crate::solver::resolve::count_as_f32;
use crate::solver::resolve::is_rtl;
use crate::solver::resolve::length_depends_on_percentage;
use crate::tree::FormattingNodeId;
use crate::tree::FormattingNodeKind;
use render_css::computed::ComputedStyle;
use render_css::properties::GridAutoFlow;
use render_css::properties::GridAutoRepeat;
use render_css::properties::GridLine;
use render_css::properties::GridTemplate;
use render_css::properties::GridTrack;
use render_css::properties::GridTrackBreadth;
use render_css::properties::JustifyContent;
use render_css::properties::LengthPercentage;
use render_css::properties::Size;
use render_css::properties::TypedPropertyValue;
use render_dom::NodeId;
use std::collections::HashSet;

/// One child's declared placement before the auto-placement pass resolves the
/// automatic axes. Spans are already normalized to at least one track.
#[derive(Clone, Copy)]
struct GridItemSpec {
    node: FormattingNodeId,
    explicit_row: Option<usize>,
    row_span: usize,
    explicit_column: Option<usize>,
    column_span: usize,
}

pub(super) fn grid_template(style: Option<&ComputedStyle>, property: &str) -> GridTemplate {
    match style.and_then(|style| style.typed(property)) {
        Some(TypedPropertyValue::GridTemplate(template)) => template.clone(),
        _ => GridTemplate::None,
    }
}

impl<'a> Solver<'a> {
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub(super) fn layout_grid_children(
        &mut self,
        children: &[FormattingNodeId],
        containing: PhysicalRect,
        positioning_containing: PhysicalRect,
        positioning_containing_rtl: bool,
        specified_height: Option<f32>,
        depth: usize,
        container_style: Option<&ComputedStyle>,
        container_source: Option<NodeId>,
    ) -> (Vec<FragmentId>, f32) {
        let container_rtl = is_rtl(container_style);
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
        let flow = match container_style.and_then(|style| style.typed("grid-auto-flow")) {
            Some(TypedPropertyValue::GridAutoFlow(flow)) => *flow,
            _ => GridAutoFlow::Row,
        };
        let auto_columns = match container_style.and_then(|style| style.typed("grid-auto-columns"))
        {
            Some(TypedPropertyValue::GridAutoTracks(tracks)) => tracks
                .0
                .iter()
                .map(|track| {
                    self.resolve_grid_track(
                        track,
                        Some(containing.size.width),
                        "grid-auto-columns",
                        container_source,
                    )
                })
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        };
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
        // The number of explicit tracks before any implicit column is added.
        // `grid-auto-columns` cycles over the implicit tracks after this point.
        let template_column_count = columns.len();
        // Positive grid-line starts may create implicit columns beyond the
        // explicit template. Account for those before sizing the column axis.
        let mut required_columns = columns.len();
        for node in children.iter().copied() {
            let style = self.grid_item_style(node);
            let (start, span) = grid_axis_placement(
                style,
                "grid-column-start",
                "grid-column-end",
                template_column_count,
            );
            if let Some(start) = start {
                required_columns = required_columns.max(start.saturating_add(span));
            }
        }
        if required_columns > self.options.limits.max_grid_tracks {
            self.report_grid_track_limit(container_source);
            return (Vec::new(), 0.0);
        }
        grow_grid_columns(
            &mut columns,
            required_columns,
            template_column_count,
            &auto_columns,
        );
        // A row-flow grid with no explicit columns still has one implicit
        // column. Keeping it flexible preserves the stretch-to-container
        // behaviour the row-major path has always had.
        if columns.is_empty() && !flow.is_column() {
            columns.push(TrackSizing::Flexible {
                minimum: 0.0,
                factor: 1.0,
            });
        }

        let row_item_count = if flow.is_column() {
            children.len()
        } else {
            required_rows(children.len(), columns.len())
        };
        let Ok(mut rows) = self.expand_grid_template(
            &row_template,
            specified_height,
            row_gap,
            row_item_count,
            "grid-template-rows",
            container_source,
        ) else {
            return (Vec::new(), 0.0);
        };
        let template_row_count = rows.len();

        let mut specs = Vec::with_capacity(children.len());
        for node in children.iter().copied() {
            let style = self.grid_item_style(node);
            let (explicit_column, column_span) = grid_axis_placement(
                style,
                "grid-column-start",
                "grid-column-end",
                template_column_count,
            );
            let (explicit_row, row_span) =
                grid_axis_placement(style, "grid-row-start", "grid-row-end", template_row_count);
            if row_span
                .checked_mul(column_span)
                .is_none_or(|area| area > self.options.limits.max_grid_tracks)
            {
                self.report_grid_track_limit(container_source);
                return (Vec::new(), 0.0);
            }
            specs.push(GridItemSpec {
                node,
                explicit_row,
                row_span,
                explicit_column,
                column_span,
            });
        }

        let mut row_count = rows.len();
        for spec in &specs {
            if let Some(row) = spec.explicit_row {
                row_count = row_count.max(row.saturating_add(spec.row_span));
            }
        }
        if !specs.is_empty() {
            row_count = row_count.max(if flow.is_column() {
                1
            } else {
                required_rows(children.len(), columns.len())
            });
        }
        if columns.len().saturating_add(row_count) > self.options.limits.max_grid_tracks {
            self.report_grid_track_limit(container_source);
            return (Vec::new(), 0.0);
        }
        rows.resize(row_count, TrackSizing::Intrinsic { minimum: 0.0 });

        let Some(placements) = self.auto_place_grid_items(
            &specs,
            &mut columns,
            template_column_count,
            &auto_columns,
            rows.len(),
            flow,
            container_source,
        ) else {
            return (Vec::new(), 0.0);
        };
        // Placement can create tracks in either axis (an auto item may land
        // past the rows sized for the explicit tracks, and a column-flow item
        // may need another implicit column).
        let mut final_columns = columns.len();
        let mut final_rows = rows.len();
        for (spec, placement) in specs.iter().zip(&placements) {
            let Some((row, column)) = placement else {
                continue;
            };
            final_columns = final_columns.max(column.saturating_add(spec.column_span));
            final_rows = final_rows.max(row.saturating_add(spec.row_span));
        }
        if final_columns > self.options.limits.max_grid_tracks
            || final_columns.saturating_add(final_rows) > self.options.limits.max_grid_tracks
        {
            self.report_grid_track_limit(container_source);
            return (Vec::new(), 0.0);
        }
        grow_grid_columns(
            &mut columns,
            final_columns,
            template_column_count,
            &auto_columns,
        );
        rows.resize(final_rows, TrackSizing::Intrinsic { minimum: 0.0 });

        // Auto-sized implicit columns take their width from the largest item
        // in the column, exactly as the row axis takes its height from the
        // row's tallest item.
        let mut column_contributions = vec![0.0_f32; columns.len()];
        for (spec, placement) in specs.iter().zip(&placements) {
            let Some((_, column)) = *placement else {
                continue;
            };
            let contribution =
                self.grid_item_max_content_outer_width(spec.node, containing.size.width);
            let column_end = column.saturating_add(spec.column_span);
            let current = column_contributions[column..column_end].iter().sum::<f32>()
                + column_gap * count_as_f32(spec.column_span.saturating_sub(1));
            if contribution > current {
                let additional = (contribution - current) / count_as_f32(spec.column_span);
                for track in &mut column_contributions[column..column_end] {
                    *track += additional;
                }
            }
        }

        let column_axis = size_axis(
            &columns,
            Some(containing.size.width),
            column_gap,
            &column_contributions,
        );
        // CSS Grid §12.2: `justify-content` aligns the grid inside the grid
        // container along the inline axis. `start`/`end` name writing-mode
        // edges, so the free space moves to the other side under RTL.
        let justify = match container_style.and_then(|style| style.typed("justify-content")) {
            Some(TypedPropertyValue::JustifyContent(value)) => *value,
            _ => JustifyContent::Normal,
        };
        let free_inline = (containing.size.width - column_axis.extent()).max(0.0);
        let (justify_offset, extra_column_gap) =
            grid_justify_offsets(justify, free_inline, columns.len());
        let column_width = |column: usize, span: usize| -> f32 {
            (column..column.saturating_add(span))
                .map(|column| column_axis.size(column))
                .sum::<f32>()
                + (column_gap + extra_column_gap) * count_as_f32(span.saturating_sub(1))
        };
        let column_origin = |column: usize, span: usize| -> f32 {
            let logical = column_axis.offset(column) + extra_column_gap * count_as_f32(column);
            let width = column_width(column, span);
            if container_rtl {
                // Positive grid line numbers count from the inline-start edge
                // (CSS Grid §8.3), which is the right edge under RTL.
                containing.origin.x + containing.size.width - justify_offset - logical - width
            } else {
                containing.origin.x + justify_offset + logical
            }
        };
        let mut row_contributions = vec![0.0_f32; rows.len()];
        let mut items = Vec::with_capacity(children.len());
        for (spec, placement) in specs.iter().zip(&placements) {
            let Some((row, column)) = *placement else {
                continue;
            };
            let node = spec.node;
            let row_span = spec.row_span;
            let column_span = spec.column_span;
            let item_style = self.grid_item_style(node);
            let row_end = row.saturating_add(row_span);
            let track_width = column_width(column, column_span);
            let track_x = column_origin(column, column_span);
            let item_source = self.formatting.get(node).and_then(|node| node.source);
            let forced_content_width = self.flex_content_width(
                item_style,
                track_width,
                containing.size.width,
                item_source,
            );
            let result = match self.formatting.get(node).map(|node| &node.kind) {
                Some(FormattingNodeKind::BlockContainer { .. }) => self
                    .layout_block_with_containing_height(
                        node,
                        PhysicalRect::new(
                            track_x,
                            containing.origin.y,
                            track_width,
                            specified_height.unwrap_or(0.0),
                        ),
                        container_rtl,
                        positioning_containing,
                        positioning_containing_rtl,
                        containing.origin.y,
                        depth,
                        Some(forced_content_width),
                        // Grid items are in-flow: their percentage heights are
                        // definite only against a definite grid height
                        // (CSS 2 §10.5).
                        // Grid items establish independent formatting
                        // contexts; their margins do not collapse with their
                        // children (CSS Grid §6).
                        specified_height.is_some(),
                        false,
                        None,
                    ),
                _ => self.layout_anonymous_block(
                    node,
                    PhysicalRect::new(
                        track_x,
                        containing.origin.y,
                        track_width,
                        specified_height.unwrap_or(0.0),
                    ),
                    container_rtl,
                    positioning_containing,
                    positioning_containing_rtl,
                    containing.origin.y,
                    depth,
                ),
            };
            let Some(result) = result else {
                continue;
            };
            let current_height = row_contributions[row..row_end].iter().sum::<f32>()
                + row_gap * count_as_f32(row_span.saturating_sub(1));
            if result.outer_height > current_height {
                let additional = (result.outer_height - current_height) / count_as_f32(row_span);
                for contribution in &mut row_contributions[row..row_end] {
                    *contribution += additional;
                }
            }
            items.push(GridItem {
                fragment: result.fragment,
                row,
                row_span,
                column,
                column_span,
                natural_outer_height: result.outer_height,
                stretch_height: self.grid_item_axis_is_auto(node, "height"),
            });
        }

        let row_axis = size_axis(&rows, specified_height, row_gap, &row_contributions);
        let mut fragments = Vec::with_capacity(items.len());
        for item in items {
            let row_height = (item.row..item.row.saturating_add(item.row_span))
                .map(|row| row_axis.size(row))
                .sum::<f32>()
                + row_gap * count_as_f32(item.row_span.saturating_sub(1));
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
                column_origin(item.column, item.column_span) - outer.origin.x,
                containing.origin.y + row_axis.offset(item.row) - outer.origin.y,
            );
            fragments.push(item.fragment);
        }
        (fragments, row_axis.extent())
    }

    pub(super) fn expand_grid_template(
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

    pub(super) fn resolve_grid_track(
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
            GridTrack::Breadth(GridTrackBreadth::Auto) => TrackSizing::Intrinsic { minimum: 0.0 },
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
                    GridTrackBreadth::Auto => TrackSizing::Intrinsic { minimum },
                    GridTrackBreadth::LengthPercentage(maximum) => self
                        .resolve_grid_length(maximum, available, property, source)
                        .map_or(TrackSizing::Intrinsic { minimum }, |maximum| {
                            TrackSizing::Fixed(maximum.max(minimum))
                        }),
                }
            }
        }
    }

    pub(super) fn resolve_grid_length(
        &mut self,
        value: &LengthPercentage,
        available: Option<f32>,
        property: &str,
        source: Option<NodeId>,
    ) -> Option<f32> {
        if available.is_none() && length_depends_on_percentage(value) {
            None
        } else {
            Some(
                self.resolve_length(value, available.unwrap_or(0.0), source, property)
                    .max(0.0),
            )
        }
    }

    pub(super) fn grid_item_axis_is_auto(&self, node: FormattingNodeId, property: &str) -> bool {
        let style = self.grid_item_style(node);
        matches!(
            style.and_then(|style| style.typed(property)),
            Some(TypedPropertyValue::Size(Size::Auto)) | None
        )
    }

    /// The computed style of a grid item. The returned reference borrows the
    /// document's style map, not `self`, so callers may measure or lay out the
    /// item while holding it.
    pub(super) fn grid_item_style(&self, node: FormattingNodeId) -> Option<&'a ComputedStyle> {
        self.formatting
            .get(node)
            .and_then(|node| node.style_source)
            .and_then(|source| self.styles.get(&source))
    }

    /// The margin-box max-content width of a grid item, used as an auto
    /// track's intrinsic contribution. A specified border-box width already
    /// contains its padding and border, so only a content-box or an auto width
    /// adds those edges again.
    pub(super) fn grid_item_max_content_outer_width(
        &mut self,
        node: FormattingNodeId,
        available: f32,
    ) -> f32 {
        let source = self.formatting.get(node).and_then(|node| node.source);
        let style = self.grid_item_style(node).cloned();
        let content = self.max_content_width(node);
        let margin = self.resolve_edge(style.as_ref(), "margin-left", available, source)
            + self.resolve_edge(style.as_ref(), "margin-right", available, source);
        let padding = self.resolve_edge(style.as_ref(), "padding-left", available, source)
            + self.resolve_edge(style.as_ref(), "padding-right", available, source);
        let border = self.resolve_border(style.as_ref(), "border-left-width", available, source)
            + self.resolve_border(style.as_ref(), "border-right-width", available, source);
        let width_is_auto = matches!(
            style.as_ref().and_then(|style| style.typed("width")),
            Some(TypedPropertyValue::Size(Size::Auto)) | None
        );
        let border_box = matches!(
            style.as_ref().and_then(|style| style.typed("box-sizing")),
            Some(TypedPropertyValue::BoxSizing(
                render_css::properties::BoxSizing::BorderBox
            ))
        );
        let extras = margin
            + if width_is_auto || !border_box {
                padding + border
            } else {
                0.0
            };
        (content + extras).max(0.0)
    }

    /// CSS Grid §8.5 auto-placement. Items with a definite placement are fixed
    /// first, then items locked to one axis fill the other, then the remaining
    /// items walk a sparse cursor (or restart at the grid origin for `dense`).
    /// The returned position is the zero-based (row, column) of each item's
    /// start cell.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn auto_place_grid_items(
        &mut self,
        specs: &[GridItemSpec],
        columns: &mut Vec<TrackSizing>,
        template_column_count: usize,
        auto_columns: &[TrackSizing],
        row_count: usize,
        flow: GridAutoFlow,
        container_source: Option<NodeId>,
    ) -> Option<Vec<Option<(usize, usize)>>> {
        let max_tracks = self.options.limits.max_grid_tracks;
        let mut occupied = HashSet::new();
        let mut placements = vec![None; specs.len()];

        // 1. Definite row and column.
        for (index, spec) in specs.iter().enumerate() {
            if let (Some(row), Some(column)) = (spec.explicit_row, spec.explicit_column) {
                placements[index] = Some((row, column));
                occupy_grid_area(row, column, spec.row_span, spec.column_span, &mut occupied);
            }
        }

        // 2. Definite row, automatic column: the first free column in that row.
        for (index, spec) in specs.iter().enumerate() {
            let Some(row) = spec.explicit_row else {
                continue;
            };
            if spec.explicit_column.is_some() {
                continue;
            }
            let mut column = 0usize;
            loop {
                if column.saturating_add(spec.column_span) > max_tracks {
                    self.report_grid_track_limit(container_source);
                    return None;
                }
                grow_grid_columns(
                    columns,
                    column.saturating_add(spec.column_span),
                    template_column_count,
                    auto_columns,
                );
                if grid_area_is_free(row, column, spec.row_span, spec.column_span, &occupied) {
                    break;
                }
                column = column.saturating_add(1);
            }
            placements[index] = Some((row, column));
            occupy_grid_area(row, column, spec.row_span, spec.column_span, &mut occupied);
        }

        // 3. Definite column, automatic row: the first free row in that column.
        for (index, spec) in specs.iter().enumerate() {
            let Some(column) = spec.explicit_column else {
                continue;
            };
            if spec.explicit_row.is_some() {
                continue;
            }
            let mut row = 0usize;
            loop {
                if row.saturating_add(spec.row_span) > max_tracks {
                    self.report_grid_track_limit(container_source);
                    return None;
                }
                if grid_area_is_free(row, column, spec.row_span, spec.column_span, &occupied) {
                    break;
                }
                row = row.saturating_add(1);
            }
            placements[index] = Some((row, column));
            occupy_grid_area(row, column, spec.row_span, spec.column_span, &mut occupied);
        }

        let automatic = specs.iter().enumerate().filter_map(|(index, spec)| {
            (spec.explicit_row.is_none() && spec.explicit_column.is_none()).then_some(index)
        });

        if flow.is_column() {
            // In column flow the explicit rows define one column's height. An
            // item spanning more rows than the template gets a taller flow
            // cell, matching the implicit-row growth the row axis would do.
            let flow_rows = row_count
                .max(
                    automatic
                        .clone()
                        .map(|index| specs[index].row_span)
                        .max()
                        .unwrap_or(1),
                )
                .max(1);
            let mut cursor = 0usize;
            for index in automatic {
                let spec = specs[index];
                let mut scan = if flow.is_dense() { 0 } else { cursor };
                loop {
                    let row = scan % flow_rows;
                    let column = scan / flow_rows;
                    if column.saturating_add(spec.column_span) > max_tracks {
                        self.report_grid_track_limit(container_source);
                        return None;
                    }
                    grow_grid_columns(
                        columns,
                        column.saturating_add(spec.column_span),
                        template_column_count,
                        auto_columns,
                    );
                    if row.saturating_add(spec.row_span) <= flow_rows
                        && grid_area_is_free(
                            row,
                            column,
                            spec.row_span,
                            spec.column_span,
                            &occupied,
                        )
                    {
                        break;
                    }
                    scan = scan.saturating_add(1);
                    if scan > flow_rows.saturating_mul(max_tracks) {
                        self.report_grid_track_limit(container_source);
                        return None;
                    }
                }
                let row = scan % flow_rows;
                let column = scan / flow_rows;
                placements[index] = Some((row, column));
                occupy_grid_area(row, column, spec.row_span, spec.column_span, &mut occupied);
                if !flow.is_dense() {
                    cursor = scan.saturating_add(spec.row_span);
                }
            }
        } else {
            let column_count = columns.len().max(1);
            let mut cursor = 0usize;
            for index in automatic {
                let spec = specs[index];
                let mut scan = if flow.is_dense() { 0 } else { cursor };
                loop {
                    let (row, column) = automatic_position(scan, column_count);
                    if row.saturating_add(spec.row_span) > max_tracks {
                        self.report_grid_track_limit(container_source);
                        return None;
                    }
                    if column.saturating_add(spec.column_span) <= column_count
                        && grid_area_is_free(
                            row,
                            column,
                            spec.row_span,
                            spec.column_span,
                            &occupied,
                        )
                    {
                        break;
                    }
                    scan = scan.saturating_add(1);
                    if scan > max_tracks.saturating_mul(column_count) {
                        self.report_grid_track_limit(container_source);
                        return None;
                    }
                }
                let row = scan / column_count;
                let column = scan % column_count;
                placements[index] = Some((row, column));
                occupy_grid_area(row, column, spec.row_span, spec.column_span, &mut occupied);
                if !flow.is_dense() {
                    cursor = scan.saturating_add(spec.column_span);
                }
            }
        }

        Some(placements)
    }

    pub(super) fn report_grid_track_limit(&mut self, source: Option<NodeId>) {
        self.diagnostics.push(LayoutDiagnostic {
            node: source,
            code: LayoutDiagnosticCode::GridTrackLimit,
            message: "grid track limit exceeded".to_owned(),
        });
    }
}

/// Append implicit columns until the axis has `target` tracks. Each new track
/// cycles through `grid-auto-columns`; an absent list means the initial `auto`,
/// which is an intrinsic track with no fixed minimum.
fn grow_grid_columns(
    columns: &mut Vec<TrackSizing>,
    target: usize,
    template_column_count: usize,
    auto_columns: &[TrackSizing],
) {
    while columns.len() < target {
        let implicit_index = columns.len().saturating_sub(template_column_count);
        let track = if auto_columns.is_empty() {
            TrackSizing::Intrinsic { minimum: 0.0 }
        } else {
            auto_columns[implicit_index % auto_columns.len()]
        };
        columns.push(track);
    }
}

fn grid_area_is_free(
    row: usize,
    column: usize,
    row_span: usize,
    column_span: usize,
    occupied: &HashSet<(usize, usize)>,
) -> bool {
    (row..row.saturating_add(row_span)).all(|row| {
        (column..column.saturating_add(column_span))
            .all(|column| !occupied.contains(&(row, column)))
    })
}

fn occupy_grid_area(
    row: usize,
    column: usize,
    row_span: usize,
    column_span: usize,
    occupied: &mut HashSet<(usize, usize)>,
) {
    for row in row..row.saturating_add(row_span) {
        for column in column..column.saturating_add(column_span) {
            occupied.insert((row, column));
        }
    }
}

/// CSS Grid §12.2 / CSS Box Alignment §5: distribute the inline free space
/// around the grid tracks. Returns the offset from the inline-start edge and
/// any extra gap between tracks.
fn grid_justify_offsets(justify: JustifyContent, free: f32, count: usize) -> (f32, f32) {
    let slots = count_as_f32(count.saturating_sub(1));
    match justify {
        JustifyContent::End | JustifyContent::FlexEnd => (free, 0.0),
        JustifyContent::Center => (free / 2.0, 0.0),
        JustifyContent::SpaceBetween if count > 1 && free > 0.0 => (0.0, free / slots),
        JustifyContent::SpaceAround if count > 0 && free > 0.0 => {
            let distributed = free / count_as_f32(count);
            (distributed / 2.0, distributed)
        }
        JustifyContent::SpaceEvenly if count > 0 && free > 0.0 => {
            let distributed = free / count_as_f32(count.saturating_add(1));
            (distributed, distributed)
        }
        // `normal` stretches auto tracks; the track sizer already gives
        // flexible tracks the remaining space, so it resolves to `start` here.
        JustifyContent::Normal
        | JustifyContent::FlexStart
        | JustifyContent::Start
        | JustifyContent::SpaceBetween
        | JustifyContent::SpaceAround
        | JustifyContent::SpaceEvenly => (0.0, 0.0),
    }
}

/// Resolve numbered grid lines to zero-based boundaries. `-1` is the line
/// after the final explicit track; named lines need separate line metadata.
fn explicit_grid_line(
    style: Option<&ComputedStyle>,
    property: &str,
    track_count: usize,
) -> Option<usize> {
    let Some(TypedPropertyValue::GridLine(line)) = style.and_then(|style| style.typed(property))
    else {
        return None;
    };
    let index = match line {
        render_css::properties::GridLine::Line(value) if *value > 0 => {
            usize::try_from(*value - 1).ok()?
        }
        render_css::properties::GridLine::Line(value) if *value < 0 => {
            let offset = usize::try_from(value.unsigned_abs()).ok()?;
            track_count.checked_add(1)?.checked_sub(offset)?
        }
        _ => return None,
    };
    Some(index)
}

fn grid_axis_placement(
    style: Option<&ComputedStyle>,
    start_property: &str,
    end_property: &str,
    track_count: usize,
) -> (Option<usize>, usize) {
    let start = explicit_grid_line(style, start_property, track_count);
    let end = explicit_grid_line(style, end_property, track_count);
    let start_span = match style.and_then(|style| style.typed(start_property)) {
        Some(TypedPropertyValue::GridLine(GridLine::Span(value))) => usize::try_from(*value).ok(),
        _ => None,
    };
    let end_span = match style.and_then(|style| style.typed(end_property)) {
        Some(TypedPropertyValue::GridLine(GridLine::Span(value))) => usize::try_from(*value).ok(),
        _ => None,
    };
    match (start, end) {
        (Some(start), Some(end)) if end > start => (Some(start), end - start),
        (Some(start), _) => (Some(start), end_span.unwrap_or(1).max(1)),
        (_, Some(end)) => {
            let span = start_span.unwrap_or(1).max(1);
            (Some(end.saturating_sub(span)), span)
        }
        _ => (None, start_span.or(end_span).unwrap_or(1).max(1)),
    }
}
