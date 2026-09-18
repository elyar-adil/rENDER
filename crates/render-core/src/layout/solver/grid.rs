//! Deterministic reference layout for block and inline formatting contexts.

use crate::css::computed::ComputedStyle;
use crate::css::properties::GridAutoRepeat;
use crate::css::properties::GridTemplate;
use crate::css::properties::GridTrack;
use crate::css::properties::GridTrackBreadth;
use crate::css::properties::LengthPercentage;
use crate::css::properties::Size;
use crate::css::properties::TypedPropertyValue;
use crate::dom::NodeId;
use crate::layout::fragment::FragmentId;
use crate::layout::geometry::PhysicalRect;
use crate::layout::grid::GridLimitError;
use crate::layout::grid::TrackSizing;
use crate::layout::grid::automatic_position;
use crate::layout::grid::expand_auto_repeat;
use crate::layout::grid::required_rows;
use crate::layout::grid::size_axis;
use crate::layout::solver::GridItem;
use crate::layout::solver::LayoutDiagnostic;
use crate::layout::solver::LayoutDiagnosticCode;
use crate::layout::solver::Solver;
use crate::layout::solver::resolve::length_depends_on_percentage;
use crate::layout::tree::FormattingNodeId;
use crate::layout::tree::FormattingNodeKind;

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
