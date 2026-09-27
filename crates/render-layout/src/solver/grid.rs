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
use crate::solver::resolve::length_depends_on_percentage;
use crate::tree::FormattingNodeId;
use crate::tree::FormattingNodeKind;
use render_css::computed::ComputedStyle;
use render_css::properties::GridAutoRepeat;
use render_css::properties::GridLine;
use render_css::properties::GridTemplate;
use render_css::properties::GridTrack;
use render_css::properties::GridTrackBreadth;
use render_css::properties::LengthPercentage;
use render_css::properties::Size;
use render_css::properties::TypedPropertyValue;
use render_dom::NodeId;
use std::collections::HashSet;

pub(super) fn grid_template(style: Option<&ComputedStyle>, property: &str) -> GridTemplate {
    match style.and_then(|style| style.typed(property)) {
        Some(TypedPropertyValue::GridTemplate(template)) => template.clone(),
        _ => GridTemplate::None,
    }
}

impl Solver<'_> {
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub(super) fn layout_grid_children(
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
        // Positive grid-line starts may create implicit columns beyond the
        // explicit template. Account for those before sizing the column axis.
        let explicit_columns = children
            .iter()
            .filter_map(|node| {
                let style = self
                    .formatting
                    .get(*node)
                    .and_then(|node| node.style_source)
                    .and_then(|source| self.styles.get(&source));
                let (start, span) = grid_axis_placement(
                    style,
                    "grid-column-start",
                    "grid-column-end",
                    columns.len(),
                );
                start.unwrap_or(0).checked_add(span)
            })
            .max()
            .map_or(columns.len(), |count| count.max(columns.len()));
        if explicit_columns > self.options.limits.max_grid_tracks {
            self.report_grid_track_limit(container_source);
            return (Vec::new(), 0.0);
        }
        columns.resize(explicit_columns, TrackSizing::Intrinsic { minimum: 0.0 });

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
        let mut occupied = HashSet::new();
        let mut auto_cursor = 0usize;
        for node in children.iter().copied() {
            let item_style = self
                .formatting
                .get(node)
                .and_then(|node| node.style_source)
                .and_then(|source| self.styles.get(&source));
            let (explicit_column, column_span) = grid_axis_placement(
                item_style,
                "grid-column-start",
                "grid-column-end",
                columns.len(),
            );
            let (explicit_row, row_span) =
                grid_axis_placement(item_style, "grid-row-start", "grid-row-end", rows.len());
            if row_span
                .checked_mul(column_span)
                .is_none_or(|area| area > self.options.limits.max_grid_tracks)
            {
                self.report_grid_track_limit(container_source);
                return (Vec::new(), 0.0);
            }
            let available_columns = columns.len().saturating_sub(column_span);
            let fits = |row: usize, column: usize, occupied: &HashSet<(usize, usize)>| {
                column <= available_columns
                    && (row..row.saturating_add(row_span)).all(|r| {
                        (column..column.saturating_add(column_span))
                            .all(|c| !occupied.contains(&(r, c)))
                    })
            };
            let (row, column) = match (explicit_row, explicit_column) {
                (Some(row), Some(column)) => (row, column),
                (Some(row), None) => {
                    let column = (0..=available_columns)
                        .find(|column| fits(row, *column, &occupied))
                        .unwrap_or(0);
                    (row, column)
                }
                (None, Some(column)) => {
                    let start_row = auto_cursor / columns.len();
                    let row = (start_row..self.options.limits.max_grid_tracks)
                        .find(|row| fits(*row, column, &occupied))
                        .unwrap_or(start_row);
                    (row, column)
                }
                (None, None) => {
                    let limit = self
                        .options
                        .limits
                        .max_grid_tracks
                        .saturating_sub(columns.len())
                        .saturating_mul(columns.len());
                    while auto_cursor < limit {
                        let (row, column) = automatic_position(auto_cursor, columns.len());
                        if fits(row, column, &occupied) {
                            break;
                        }
                        auto_cursor = auto_cursor.saturating_add(1);
                    }
                    if auto_cursor >= limit {
                        self.report_grid_track_limit(container_source);
                        return (Vec::new(), 0.0);
                    }
                    let (row, column) = automatic_position(auto_cursor, columns.len());
                    auto_cursor = auto_cursor.saturating_add(column_span);
                    (row, column)
                }
            };
            for r in row..row.saturating_add(row_span) {
                for c in column..column.saturating_add(column_span) {
                    occupied.insert((r, c));
                }
            }
            let row_end = row.saturating_add(row_span);
            if row_end > rows.len() {
                if columns.len().saturating_add(row_end) > self.options.limits.max_grid_tracks {
                    self.report_grid_track_limit(container_source);
                    return (Vec::new(), 0.0);
                }
                rows.resize(row_end, TrackSizing::Intrinsic { minimum: 0.0 });
                row_contributions.resize(rows.len(), 0.0);
            }
            let track_width = (column..column.saturating_add(column_span))
                .map(|column| column_axis.size(column))
                .sum::<f32>()
                + column_gap * count_as_f32(column_span.saturating_sub(1));
            let track_x = containing.origin.x + column_axis.offset(column);
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
                        positioning_containing,
                        containing.origin.y,
                        depth,
                        Some(forced_content_width),
                        // Grid items are in-flow: their percentage heights are
                        // definite only against a definite grid height
                        // (CSS 2 §10.5).
                        specified_height.is_some(),
                    ),
                _ => self.layout_anonymous_block(
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
                containing.origin.x + column_axis.offset(item.column) - outer.origin.x,
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

    pub(super) fn report_grid_track_limit(&mut self, source: Option<NodeId>) {
        self.diagnostics.push(LayoutDiagnostic {
            node: source,
            code: LayoutDiagnosticCode::GridTrackLimit,
            message: "grid track limit exceeded".to_owned(),
        });
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
