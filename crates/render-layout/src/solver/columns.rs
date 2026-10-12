//! CSS Multi-column Layout Level 1: distributing a multi-column container's flow
//! into its columns.
//!
//! A multi-column container lays its in-flow children out at the width of one
//! column, so every line is wrapped for that width. The flow those children make
//! is then cut at column boundaries, and each piece is moved into its column.
//!
//! The cut falls between lines. A line that would cross the end of a column moves
//! to the top of the next one, because a line is never split (the unbreakable
//! content rule of CSS Fragmentation). A block box that the cut passes through is
//! split into one fragment per column it touches. A piece keeps the top border and
//! padding only where its flow starts, and the bottom ones only where it ends
//! (`box-decoration-break: slice`).
//!
//! The column height is the least height that fits the content into the column
//! count when the container balances its columns, and the container's constrained
//! height when it does not. Content beyond the last column makes overflow columns
//! to the right, as CSS requires for a constrained height.
//!
//! Not implemented: `column-span`, so a spanning element is columned with its
//! siblings, and `column-rule`.

use render_css::computed::ComputedStyle;
use render_css::properties::{
    ColumnCount, ColumnFill, ColumnSpan, ColumnWidth, Gap, TypedPropertyValue,
};
use render_dom::NodeId;

use super::inline::parse_font_size;
use super::resolve::count_as_f32;
use crate::fragment::{BoxGeometry, Fragment, FragmentId, FragmentKind};
use crate::geometry::{EdgeSizes, PhysicalRect};
use crate::solver::Solver;
use crate::tree::FormattingNodeId;

/// Tolerance for positions that are equal up to layout rounding.
const EPSILON: f32 = 0.01;

/// Bisection steps for the balanced column height. Each step halves the search
/// interval, so the result is exact to far below a layout unit.
const BISECTION_STEPS: usize = 40;

/// The used geometry of a multi-column container's columns.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ColumnGeometry {
    /// The number of columns, at least two.
    pub(super) count: usize,
    /// The width of one column.
    pub(super) width: f32,
    /// The space between two adjacent columns.
    pub(super) gap: f32,
    /// How the content fills the columns.
    pub(super) fill: ColumnFill,
}

/// A multi-column container's flow, laid out at the width of one column.
#[derive(Clone, Copy, Debug)]
pub(super) struct ColumnFlow {
    /// The document-space y of the content box's top, where the flow starts.
    pub(super) top: f32,
    /// The height the flow reaches before it is distributed.
    pub(super) height: f32,
    /// The container's content height, when it is specified.
    pub(super) constrained_height: Option<f32>,
    /// The columns the flow is distributed over.
    pub(super) geometry: ColumnGeometry,
}

/// A stretch of the flow that begins at `start`, in flow coordinates, and is
/// placed in `column`, from the top of that column.
#[derive(Clone, Copy, Debug)]
struct Segment {
    start: f32,
    column: usize,
}

/// A line-level unit of the flow that a column boundary may not cut.
#[derive(Clone, Copy, Debug)]
struct Unit {
    top: f32,
    bottom: f32,
}

/// The segments of one distribution, and the distance from one column to the next.
struct Placement<'a> {
    top: f32,
    pitch: f32,
    segments: &'a [Segment],
}

impl Placement<'_> {
    /// The index of the segment that a flow position falls in.
    fn segment_at(&self, flow_y: f32) -> usize {
        self.segments
            .iter()
            .rposition(|segment| segment.start <= flow_y + EPSILON)
            .unwrap_or(0)
    }

    fn column_x(&self, column: usize) -> f32 {
        count_as_f32(column) * self.pitch
    }
}

impl Solver<'_> {
    /// The columns of a container whose `column-count` or `column-width` asks for
    /// more than one, or `None` for an ordinary block.
    ///
    /// CSS Multi-column §3: with both properties set, `column-count` is a maximum
    /// and `column-width` a minimum, so the count is the smaller of the number
    /// that `column-count` asks for and the number of columns of the given width
    /// that fit.
    pub(super) fn column_geometry(
        &mut self,
        style: Option<&ComputedStyle>,
        content_width: f32,
        source: Option<NodeId>,
    ) -> Option<ColumnGeometry> {
        let count = match style.and_then(|style| style.typed("column-count")) {
            Some(TypedPropertyValue::ColumnCount(ColumnCount::Count(count))) => {
                usize::try_from(*count).ok()
            }
            _ => None,
        };
        let width = match style.and_then(|style| style.typed("column-width")) {
            Some(TypedPropertyValue::ColumnWidth(ColumnWidth::Length(length))) => {
                let width = self.resolve_length(length, content_width, source, "column-width");
                (width > 0.0).then_some(width)
            }
            _ => None,
        };
        if count.is_none() && width.is_none() {
            return None;
        }
        let gap = self.column_gap(style, content_width, source);
        let available = content_width.max(0.0);
        let fitting = width.map(|width| fitting_columns(available, gap, width));
        let columns = match (count, fitting) {
            (Some(count), Some(fitting)) => count.min(fitting),
            (Some(count), None) => count,
            (None, Some(fitting)) => fitting,
            (None, None) => return None,
        };
        if columns <= 1 {
            return None;
        }
        let fill = match style.and_then(|style| style.typed("column-fill")) {
            Some(TypedPropertyValue::ColumnFill(fill)) => *fill,
            _ => ColumnFill::Balance,
        };
        let width =
            ((available - gap * count_as_f32(columns - 1)) / count_as_f32(columns)).max(0.0);
        Some(ColumnGeometry {
            count: columns,
            width,
            gap,
            fill,
        })
    }

    /// Moves a multi-column container's flow into its columns.
    ///
    /// Returns the new in-flow children, in flow order, and the height of the
    /// columns. The children are the same fragments where nothing was split, and
    /// otherwise the pieces that the split made.
    pub(super) fn distribute_columns(
        &mut self,
        children: Vec<FragmentId>,
        flow: ColumnFlow,
    ) -> (Vec<FragmentId>, f32) {
        let mut units = Vec::new();
        for child in &children {
            self.collect_units(*child, flow.top, &mut units);
        }
        let extent = units
            .iter()
            .map(|unit| unit.bottom)
            .fold(flow.height, f32::max);
        let column_height = column_height(&units, extent, flow);
        if column_height.is_nan() || column_height <= 0.0 {
            return (children, flow.height);
        }
        let segments = plan_segments(&units, column_height);
        let placement = Placement {
            top: flow.top,
            pitch: flow.geometry.width + flow.geometry.gap,
            segments: &segments,
        };
        let mut placed = Vec::with_capacity(children.len());
        let mut reach = 0.0_f32;
        for child in children {
            if self.fragment_is_out_of_flow(child) {
                placed.push(child);
                continue;
            }
            for (_, piece) in self.place_in_columns(child, &placement) {
                if let Some(fragment) = self.fragment_ref(piece) {
                    reach = reach.max(fragment.rect.bottom() - flow.top);
                }
                placed.push(piece);
            }
        }
        (placed, column_height.max(reach))
    }

    /// Whether a child of a multi-column container spans all of its columns
    /// (CSS Multi-column §4). Only the container's direct children can.
    pub(super) fn spans_all_columns(&self, child: FormattingNodeId) -> bool {
        let style = self
            .formatting
            .get(child)
            .and_then(|node| node.style_source)
            .and_then(|source| self.styles.get(&source));
        matches!(
            style.and_then(|style| style.typed("column-span")),
            Some(TypedPropertyValue::ColumnSpan(ColumnSpan::All))
        )
    }

    /// Distributes the children from `start` on, which make one column set, and
    /// returns the y where the set ends.
    pub(super) fn distribute_column_set(
        &mut self,
        children: &mut Vec<FragmentId>,
        start: usize,
        flow: ColumnFlow,
    ) -> f32 {
        let set = children.split_off(start);
        let (placed, height) = self.distribute_columns(set, flow);
        children.extend(placed);
        flow.top + height
    }

    /// The gap between columns. `normal` is one em of the container's font
    /// (CSS Multi-column §3.3), where flex and grid treat it as zero.
    fn column_gap(
        &mut self,
        style: Option<&ComputedStyle>,
        basis: f32,
        source: Option<NodeId>,
    ) -> f32 {
        match style.and_then(|style| style.typed("column-gap")) {
            Some(TypedPropertyValue::Gap(Gap::LengthPercentage(value))) => self
                .resolve_length(value, basis, source, "column-gap")
                .max(0.0),
            _ => source
                .and_then(|node| self.styles.get(&node))
                .and_then(|style| style.get("font-size"))
                .and_then(|value| parse_font_size(value.css_text(), self.options.root_font_size))
                .unwrap_or(self.options.root_font_size),
        }
    }

    fn fragment_ref(&self, id: FragmentId) -> Option<&Fragment> {
        usize::try_from(id.as_u32())
            .ok()
            .and_then(|index| self.fragments.get(index))
    }

    fn fragment_is_out_of_flow(&self, id: FragmentId) -> bool {
        self.fragment_ref(id)
            .is_some_and(|fragment| self.source_is_out_of_flow(fragment.source))
    }

    /// Collects the line-level units under `id`, in flow order. A box with in-flow
    /// children is not a unit: its children are, and its own edges follow them.
    fn collect_units(&self, id: FragmentId, flow_top: f32, units: &mut Vec<Unit>) {
        let Some(fragment) = self.fragment_ref(id) else {
            return;
        };
        if self.source_is_out_of_flow(fragment.source) {
            return;
        }
        let is_container = !matches!(fragment.kind, FragmentKind::Text(_))
            && fragment
                .children
                .iter()
                .any(|child| !self.fragment_is_out_of_flow(*child));
        if is_container {
            for child in &fragment.children {
                self.collect_units(*child, flow_top, units);
            }
        } else {
            units.push(Unit {
                top: fragment.rect.origin.y - flow_top,
                bottom: fragment.rect.bottom() - flow_top,
            });
        }
    }

    /// Places the fragment `id` in its column, and returns the pieces that now
    /// represent it, each with the segment it landed in.
    ///
    /// A leaf moves whole. A box whose children all land in one segment moves
    /// with them. A box whose children span several segments is split.
    fn place_in_columns(
        &mut self,
        id: FragmentId,
        placement: &Placement<'_>,
    ) -> Vec<(usize, FragmentId)> {
        let Some(fragment) = self.fragment_ref(id) else {
            return Vec::new();
        };
        let in_flow: Vec<FragmentId> = fragment
            .children
            .iter()
            .copied()
            .filter(|child| !self.fragment_is_out_of_flow(*child))
            .collect();
        let is_container = !matches!(fragment.kind, FragmentKind::Text(_)) && !in_flow.is_empty();
        if !is_container {
            let index = placement.segment_at(fragment.rect.origin.y - placement.top);
            self.move_into_segment(id, index, placement);
            return vec![(index, id)];
        }
        let mut pieces = Vec::new();
        for child in &in_flow {
            pieces.extend(self.place_in_columns(*child, placement));
        }
        let groups = group_by_segment(&pieces);
        match groups.as_slice() {
            [] => Vec::new(),
            [(index, _)] => {
                self.move_into_segment(id, *index, placement);
                vec![(*index, id)]
            }
            groups => self.split_in_columns(id, groups, placement),
        }
    }

    /// Moves a fragment's own rectangle into segment `index`, leaving its
    /// children, which are placed on their own.
    fn move_into_segment(&mut self, id: FragmentId, index: usize, placement: &Placement<'_>) {
        let segment = placement.segments[index];
        self.translate_fragment_own(id, placement.column_x(segment.column), -segment.start);
    }

    /// Splits a box across the segments its children landed in, into one piece
    /// per segment. The first piece keeps the box's id, and the others are new
    /// fragments with the same style.
    fn split_in_columns(
        &mut self,
        id: FragmentId,
        groups: &[(usize, Vec<FragmentId>)],
        placement: &Placement<'_>,
    ) -> Vec<(usize, FragmentId)> {
        let Some(original) = self.fragment_ref(id).cloned() else {
            return Vec::new();
        };
        let FragmentKind::Box(geometry) = &original.kind else {
            return Vec::new();
        };
        let out_of_flow: Vec<FragmentId> = original
            .children
            .iter()
            .copied()
            .filter(|child| self.fragment_is_out_of_flow(*child))
            .collect();
        let box_top = original.rect.origin.y - placement.top;
        let box_bottom = original.rect.bottom() - placement.top;
        let last = groups.len() - 1;
        let mut pieces: Vec<(usize, FragmentId)> = Vec::with_capacity(groups.len());
        for (index, (segment_index, members)) in groups.iter().enumerate() {
            let segment = placement.segments[*segment_index];
            let segment_end = placement
                .segments
                .get(segment_index + 1)
                .map_or(f32::INFINITY, |next| next.start);
            let first = index == 0;
            let is_last = index == last;
            let (member_top, member_bottom) = self.member_extent(members, placement.top);
            let top = if first && box_top >= segment.start - EPSILON {
                box_top - segment.start
            } else if first {
                member_top - geometry.border.top - geometry.padding.top
            } else {
                member_top
            };
            let bottom = if is_last && box_bottom <= segment_end + EPSILON {
                box_bottom - segment.start
            } else if is_last {
                member_bottom + geometry.border.bottom + geometry.padding.bottom
            } else {
                member_bottom
            };
            // `box-decoration-break: slice`: the top edges belong to the first
            // piece and the bottom edges to the last.
            let slice = |edges: &EdgeSizes| EdgeSizes {
                top: if first { edges.top } else { 0.0 },
                right: edges.right,
                bottom: if is_last { edges.bottom } else { 0.0 },
                left: edges.left,
            };
            let margin = slice(&geometry.margin);
            let border = slice(&geometry.border);
            let padding = slice(&geometry.padding);
            let content_height = (bottom - top - border.vertical() - padding.vertical()).max(0.0);
            let piece_geometry = BoxGeometry {
                margin,
                border,
                padding,
                content_rect: PhysicalRect::new(
                    geometry.content_rect.origin.x + placement.column_x(segment.column),
                    placement.top + top + border.top + padding.top,
                    geometry.content_rect.size.width,
                    content_height,
                ),
            };
            let rect = piece_geometry.border_rect();
            let mut children = members.clone();
            if first {
                children.extend(out_of_flow.iter().copied());
            }
            let piece = if first {
                id
            } else if let Some(piece) = self.allocate_fragment(
                original.formatting_node,
                original.source,
                rect,
                FragmentKind::Box(piece_geometry.clone()),
            ) {
                piece
            } else {
                // The fragment budget is spent, so this segment's members stay
                // with the piece before them rather than being dropped.
                if let Some(previous) = pieces.last().map(|(_, previous)| *previous)
                    && let Some(fragment) = self.fragment_mut(previous)
                {
                    fragment.children.extend(members.iter().copied());
                }
                continue;
            };
            if let Some(fragment) = self.fragment_mut(piece) {
                fragment.rect = rect;
                fragment.kind = FragmentKind::Box(piece_geometry);
                fragment.children = children;
            }
            pieces.push((*segment_index, piece));
        }
        pieces
    }

    /// The top and bottom of a group of placed fragments, in flow coordinates.
    fn member_extent(&self, members: &[FragmentId], flow_top: f32) -> (f32, f32) {
        members.iter().filter_map(|id| self.fragment_ref(*id)).fold(
            (f32::INFINITY, f32::NEG_INFINITY),
            |(top, bottom), fragment| {
                (
                    top.min(fragment.rect.origin.y - flow_top),
                    bottom.max(fragment.rect.bottom() - flow_top),
                )
            },
        )
    }
}

/// The number of columns of `width` that fit in `available` with `gap` between
/// them, and at least one (CSS Multi-column §3.2).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the floor of a non-negative ratio is a column count"
)]
fn fitting_columns(available: f32, gap: f32, width: f32) -> usize {
    ((available + gap) / (width + gap)).floor().max(1.0) as usize
}

/// The column height: balanced when the container balances, or its constrained
/// height when it is constrained and does not (CSS Multi-column §7.1).
fn column_height(units: &[Unit], extent: f32, flow: ColumnFlow) -> f32 {
    match (flow.geometry.fill, flow.constrained_height) {
        (ColumnFill::Auto, Some(height)) => height,
        (ColumnFill::Balance, Some(height)) => {
            least_height(units, extent, flow.geometry.count).min(height)
        }
        (_, None) => least_height(units, extent, flow.geometry.count),
    }
}

/// The least height that fits the units into `count` columns. The bisection keeps
/// the invariant that its upper bound fits, and the upper bound starts at the
/// extent of the flow, which always fits in one column.
fn least_height(units: &[Unit], extent: f32, count: usize) -> f32 {
    let tallest = units
        .iter()
        .map(|unit| unit.bottom - unit.top)
        .fold(0.0_f32, f32::max);
    let mut low = tallest;
    let mut high = extent.max(tallest);
    if columns_used(units, low) <= count {
        return low;
    }
    for _ in 0..BISECTION_STEPS {
        let middle = f32::midpoint(low, high);
        if columns_used(units, middle) <= count {
            high = middle;
        } else {
            low = middle;
        }
    }
    high
}

fn columns_used(units: &[Unit], height: f32) -> usize {
    plan_segments(units, height)
        .last()
        .map_or(1, |segment| segment.column + 1)
}

/// Cuts the flow at column boundaries `height` apart. A unit that would cross the
/// end of the current column, and that is not already at its top, starts the next
/// column. A unit taller than a column is left to overflow it, since no column can
/// hold it.
fn plan_segments(units: &[Unit], height: f32) -> Vec<Segment> {
    let mut segments = vec![Segment {
        start: 0.0,
        column: 0,
    }];
    for unit in units {
        let Some(&current) = segments.last() else {
            break;
        };
        let starts_below_top = unit.top - current.start > EPSILON;
        let crosses_end = unit.bottom - current.start > height + EPSILON;
        let fits_a_column = unit.bottom - unit.top <= height + EPSILON;
        if starts_below_top && crosses_end && fits_a_column {
            segments.push(Segment {
                start: unit.top,
                column: current.column + 1,
            });
        }
    }
    segments
}

/// Groups consecutive pieces that landed in the same segment, keeping their order.
fn group_by_segment(pieces: &[(usize, FragmentId)]) -> Vec<(usize, Vec<FragmentId>)> {
    let mut groups: Vec<(usize, Vec<FragmentId>)> = Vec::new();
    for (index, piece) in pieces {
        match groups.last_mut() {
            Some((last, members)) if last == index => members.push(*piece),
            _ => groups.push((*index, vec![*piece])),
        }
    }
    groups
}
