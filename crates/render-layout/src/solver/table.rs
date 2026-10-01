//! CSS 2.1 §17 table layout.
//!
//! The `display: table` box itself is an ordinary block-level box, so the
//! block solver has already resolved its used width, margin, border and
//! padding. This module implements the table-specific remainder: the column
//! width distribution of §17.5.2, the row heights, spanning and
//! `vertical-align` of §17.5.3, and the caption placement of §17.4.

use std::collections::{BTreeMap, HashSet};

use render_css::computed::ComputedStyle;
use render_css::properties::{BorderStyle, Display, DisplayInternal, Size, TypedPropertyValue};
use render_dom::NodeId;

use crate::fragment::BoxGeometry;
use crate::fragment::FragmentId;
use crate::fragment::FragmentKind;
use crate::geometry::EdgeSizes;
use crate::geometry::PhysicalRect;
use crate::solver::CollapsedEdges;
use crate::solver::LayoutDiagnostic;
use crate::solver::LayoutDiagnosticCode;
use crate::solver::Solver;
use crate::solver::inline::parse_text_length;
use crate::solver::resolve::count_as_f32;
use crate::tree::FormattingContextKind;
use crate::tree::FormattingNodeId;
use crate::tree::FormattingNodeKind;

/// Cell alignment inside its row (CSS 2.1 §17.5.3).
enum VerticalAlign {
    Top,
    Middle,
    Bottom,
    Baseline,
}

/// A row of the table structure together with the row group that declares it.
struct TableRow {
    group: Option<FormattingNodeId>,
    node: FormattingNodeId,
}

/// A cell's position in the table grid (CSS 2.1 §17.2.1, §17.5).
#[derive(Clone, Copy)]
struct CellPlacement {
    node: FormattingNodeId,
    column: usize,
    column_span: usize,
    row_span: usize,
}

/// A cell laid out at its used width, before the row heights are known.
struct TableCell {
    placement: CellPlacement,
    fragment: FragmentId,
    /// Margin-box height the cell needs at its used width.
    natural: f32,
    /// Content-box height the cell's in-flow children occupy, which is what
    /// `vertical-align` places inside the cell.
    flow: f32,
    /// `false` when the cell carries a height of its own to preserve.
    stretch: bool,
}

/// A caption placed before or after the row stack.
struct TableCaption {
    fragment: FragmentId,
    outer_height: f32,
    top: bool,
}

/// CSS 2.1 §17.2.1: the box tree of a table, once the anonymous boxes and the
/// row groups have been flattened.
struct TableStructure {
    captions: Vec<FormattingNodeId>,
    rows: Vec<TableRow>,
    /// The grid slot every cell occupies, one entry per row.
    grid: Vec<Vec<CellPlacement>>,
    columns: usize,
    /// The column boxes whose `width` sizes the columns they cover (§17.5.1).
    column_boxes: Vec<FormattingNodeId>,
}

/// One side of a cell's border, named so that the collapsing rules can talk
/// about the grid line it sits on without index arithmetic.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Side {
    Top,
    Right,
    Bottom,
    Left,
}

const SIDES: [Side; 4] = [Side::Top, Side::Right, Side::Bottom, Side::Left];

impl Side {
    /// The `border-<side>-style` longhand this side reads.
    const fn style_property(self) -> &'static str {
        match self {
            Self::Top => "border-top-style",
            Self::Right => "border-right-style",
            Self::Bottom => "border-bottom-style",
            Self::Left => "border-left-style",
        }
    }

    const fn with(self, sizes: EdgeSizes, value: f32) -> EdgeSizes {
        match self {
            Self::Top => EdgeSizes {
                top: value,
                ..sizes
            },
            Self::Right => EdgeSizes {
                right: value,
                ..sizes
            },
            Self::Bottom => EdgeSizes {
                bottom: value,
                ..sizes
            },
            Self::Left => EdgeSizes {
                left: value,
                ..sizes
            },
        }
    }
}

/// CSS 2.1 §17.6.2: the border that survives where two compete. A `hidden`
/// border beats everything, then the wider border, then the style with the
/// higher precedence, and a complete tie goes to the claim collected first -
/// which, because the cells are collected before the table's own claims and in
/// top-left order, is §17.6.2.1 rule 4's own answer for the common case
/// (see [`BorderClaim::outranks`]).
fn collapsed_border_winner(competing: &[BorderClaim]) -> Option<usize> {
    let mut winner: Option<usize> = None;
    for (index, claim) in competing.iter().enumerate() {
        if winner.is_none_or(|current| claim.outranks(&competing[current])) {
            winner = Some(index);
        }
    }
    winner
}

/// A border competing for one grid line of the table.
#[derive(Clone, Copy)]
struct BorderClaim {
    /// The cell the border belongs to, or the table itself. The table is an
    /// ordinary claimant on every outer line: §17.6.2.1 gives it no exemption.
    source: Option<NodeId>,
    side: Side,
    width: f32,
    style: BorderStyle,
}

impl BorderClaim {
    /// CSS 2.1 §17.6.2.1 conflict resolution, in the order the specification
    /// states the rules:
    ///
    /// 1. `hidden` beats everything.
    /// 2. A wider border beats a narrower one.
    /// 3. Among equal widths, styles are preferred in the order `double`,
    ///    `solid`, `dashed`, `dotted`, `ridge`, `outset`, `groove`, `inset`.
    /// 4. **Not implemented, and it cannot be implemented from what a claim
    ///    carries.** Rule 4 orders claims "if border styles differ only in
    ///    color", and [`BorderClaim`] holds no colour: a pair that differs only
    ///    in colour compares equal here, and the winner is then the one
    ///    collected first. That happens to be rule 4's *second* sentence for
    ///    two elements of the same type - "the one further to the left and
    ///    further to the top wins" - and, because the cells are collected
    ///    before the table's own claims, it is also rule 4's *type* order
    ///    (`cell > row > row group > column > column group > table`) for the
    ///    only pair of types this engine can compare. Colouring the claim is
    ///    the missing half; see the report on `docs/visual_fidelity_gaps.md`
    ///    S29's rule-4 entry.
    ///
    /// There is deliberately **no** table-ness shortcut here. A previous
    /// version returned `self.table` on a table-ness difference, which made
    /// the table win the outer lines whatever the widths: `table { border: 1px
    /// solid }` beside `td { border: 8px solid }` resolved to a 1px line where
    /// §17.6.2.1 rule 2 says the 8px border wins. §17.6.2.1 has no exception
    /// for the table at any width - the table is only *last* in rule 4's
    /// colour-only tie - and rule 2 comes before rule 4 at all.
    fn outranks(&self, other: &Self) -> bool {
        let hidden = |claim: &Self| claim.style == BorderStyle::Hidden;
        match (hidden(self), hidden(other)) {
            (true, false) => return true,
            (false, true) => return false,
            _ => {}
        }
        (self.width, style_precedence(self.style)) > (other.width, style_precedence(other.style))
    }
}

fn style_precedence(style: BorderStyle) -> u8 {
    match style {
        BorderStyle::Double => 9,
        BorderStyle::Solid => 8,
        BorderStyle::Dashed => 7,
        BorderStyle::Dotted => 6,
        BorderStyle::Ridge => 5,
        BorderStyle::Outset => 4,
        BorderStyle::Groove => 3,
        BorderStyle::Inset => 2,
        BorderStyle::Hidden | BorderStyle::None => 1,
    }
}

fn border_style_of(style: Option<&ComputedStyle>, side: Side) -> BorderStyle {
    let value = style
        .and_then(|style| style.get(side.style_property()))
        .map(|value| value.css_text().trim().to_ascii_lowercase());
    match value.as_deref() {
        Some("double") => BorderStyle::Double,
        Some("solid") => BorderStyle::Solid,
        Some("dashed") => BorderStyle::Dashed,
        Some("dotted") => BorderStyle::Dotted,
        Some("ridge") => BorderStyle::Ridge,
        Some("outset") => BorderStyle::Outset,
        Some("groove") => BorderStyle::Groove,
        Some("inset") => BorderStyle::Inset,
        Some("hidden") => BorderStyle::Hidden,
        _ => BorderStyle::None,
    }
}

/// CSS 2.1 §17.6.1: in the separated table model every column, and the table
/// itself, is preceded by one `border-spacing` amount.
fn column_offsets(origin_x: f32, widths: &[f32], spacing: f32) -> Vec<f32> {
    let mut offsets = Vec::with_capacity(widths.len());
    let mut x = origin_x + spacing;
    for width in widths {
        offsets.push(x);
        x += width + spacing;
    }
    offsets
}

impl Solver<'_> {
    /// CSS 2.1 §17.5.3: how a row group places its rows inside a table that is
    /// taller than they are.
    ///
    /// Returns the y offset of every row and the extra height each row group
    /// box gained. A table with no row groups at all aligns its whole row stack
    /// with the table's own `vertical-align`, which is the single-group case the
    /// specification describes.
    fn row_group_offsets(
        &mut self,
        extra: f32,
        structure: &TableStructure,
        group_boxes: &[(usize, usize, FormattingNodeId, FragmentId)],
        table_style: Option<&ComputedStyle>,
    ) -> (Vec<f32>, Vec<f32>) {
        let mut row_offsets = vec![0.0_f32; structure.rows.len()];
        let mut group_extra = vec![0.0_f32; group_boxes.len()];
        if extra <= 0.0 {
            return (row_offsets, group_extra);
        }
        let runs: Vec<(usize, usize, Option<FormattingNodeId>)> = if group_boxes.is_empty() {
            vec![(0, structure.rows.len(), None)]
        } else {
            group_boxes
                .iter()
                .map(|(start, end, group, _)| (*start, *end, Some(*group)))
                .collect()
        };
        let share = extra / count_as_f32(runs.len());
        let mut base = 0.0;
        for (index, (start, end, group)) in runs.into_iter().enumerate() {
            let align = match group {
                Some(group) => self.row_group_vertical_align(group, table_style),
                None => Self::table_vertical_align(table_style),
            };
            let inner = match align {
                VerticalAlign::Top | VerticalAlign::Baseline => 0.0,
                VerticalAlign::Middle => share / 2.0,
                VerticalAlign::Bottom => share,
            };
            for offset in row_offsets.iter_mut().take(end).skip(start) {
                *offset = base + inner;
            }
            if group.is_some()
                && let Some(slot) = group_extra.get_mut(index)
            {
                *slot = share;
            }
            base += share;
        }
        (row_offsets, group_extra)
    }

    /// The `vertical-align` of a row group.
    ///
    /// A group that the document said nothing about takes the table's own
    /// `vertical-align`, because that is what positions row groups inside a
    /// table taller than its rows. §17.5.3 is where a table cell's alignment
    /// within its row is defined; it is worth being precise that CSS 2.1
    /// explicitly leaves the distribution of a table's *extra* height
    /// undefined ("CSS 2.1 does not define how extra space is distributed when
    /// the 'height' property causes the table to be taller than it otherwise
    /// would be"), so the group-to-table relationship here follows what user
    /// agents do rather than a normative sentence in §17.5.3.
    ///
    /// The test is `specified`, not `get(..).is_some()`. `get` answers "is
    /// there a computed value?", and since the property registry installs an
    /// initial value for `vertical-align` that is *always* true, so the
    /// presence test cannot tell a group the document aligned from one that
    /// merely holds the initial `baseline`. `specified` is the question this
    /// actually means: did the document set this on this element.
    fn row_group_vertical_align(
        &self,
        group: FormattingNodeId,
        table_style: Option<&ComputedStyle>,
    ) -> VerticalAlign {
        let style = self
            .formatting
            .get(group)
            .and_then(|node| node.style_source)
            .and_then(|source| self.styles.get(&source));
        if style.is_some_and(|style| style.specified("vertical-align")) {
            Self::table_vertical_align(style)
        } else {
            Self::table_vertical_align(table_style)
        }
    }

    /// CSS 2.1 §17.5.2: lay out the rows, cells and captions of a `display:
    /// table` box inside the used width the block solver has already resolved
    /// for it. Returns the table's children and the height they occupy.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub(super) fn layout_table_children(
        &mut self,
        node: FormattingNodeId,
        style: Option<&ComputedStyle>,
        children: &[FormattingNodeId],
        containing: PhysicalRect,
        positioning_containing: PhysicalRect,
        used_height: Option<f32>,
        depth: usize,
    ) -> (Vec<FragmentId>, f32) {
        // §17.2.1: the table structure.
        let structure = self.table_structure(children);
        let columns = structure.columns;
        if columns > self.options.limits.max_grid_tracks {
            self.diagnostics.push(LayoutDiagnostic {
                node: None,
                code: LayoutDiagnosticCode::GridTrackLimit,
                message: "table column limit exceeded".to_owned(),
            });
            return (Vec::new(), 0.0);
        }
        // The sizing pass has already resolved the table's edges and width; this
        // repeats the edge rules because a table reached through a shrink-to-fit
        // ancestor is laid out without going through them.
        self.apply_table_edge_rules(node, style, containing.size.width, &structure);
        let spacing = Self::table_border_spacing(style, containing.size.width);
        let (_, widths) = self.table_column_layout(node, style, containing.size.width);
        let offsets = column_offsets(containing.origin.x, &widths, spacing);

        // Allocate the row and row-group boxes in document order, then fill
        // them with the cells they position.
        let mut row_boxes: Vec<Option<FragmentId>> = vec![None; structure.rows.len()];
        let mut group_boxes: Vec<(usize, usize, FormattingNodeId, FragmentId)> = Vec::new();
        let mut index = 0;
        while index < structure.rows.len() {
            let Some(group) = structure.rows[index].group else {
                row_boxes[index] = self.new_table_box(structure.rows[index].node, containing);
                index += 1;
                continue;
            };
            let start = index;
            let group_box = self.new_table_box(group, containing);
            while index < structure.rows.len() && structure.rows[index].group == Some(group) {
                row_boxes[index] = self.new_table_box(structure.rows[index].node, containing);
                index += 1;
            }
            if let Some(fragment) = group_box {
                group_boxes.push((start, index, group, fragment));
            }
        }

        // Every cell is laid out at its used width first; the row heights
        // follow from what the cells asked for.
        let mut cells: Vec<Vec<TableCell>> = Vec::with_capacity(structure.rows.len());
        for row in &structure.grid {
            let mut laid_out = Vec::with_capacity(row.len());
            for placement in row {
                // §17.5.2.3: a cell fills the columns it covers, whatever its
                // own `width` says, so the used width is forced here.
                let width = widths[placement.column..placement.column + placement.column_span]
                    .iter()
                    .sum::<f32>()
                    + spacing * count_as_f32(placement.column_span.saturating_sub(1));
                let cell_style = self
                    .formatting
                    .get(placement.node)
                    .and_then(|node| node.style_source)
                    .and_then(|source| self.styles.get(&source));
                let cell_source = self
                    .formatting
                    .get(placement.node)
                    .and_then(|node| node.source);
                let stretch = matches!(
                    cell_style.and_then(|style| style.typed("height")),
                    None | Some(TypedPropertyValue::Size(Size::Auto))
                );
                // A column width is a border-box width, while the block
                // algorithm takes a content width.
                let content_width =
                    (width - self.cell_horizontal_extras(cell_style, width, cell_source)).max(0.0);
                let Some(result) = self.layout_block_with_containing_height(
                    placement.node,
                    PhysicalRect::new(offsets[placement.column], 0.0, content_width, 0.0),
                    positioning_containing,
                    0.0,
                    depth.saturating_add(1),
                    Some(content_width),
                    // A cell's percentage heights are definite only against a
                    // definite row height, which is content-sized here
                    // (CSS 2 §10.5).
                    false,
                ) else {
                    continue;
                };
                laid_out.push(TableCell {
                    placement: *placement,
                    fragment: result.fragment,
                    natural: result.outer_height,
                    flow: result.flow_height,
                    stretch,
                });
            }
            cells.push(laid_out);
        }

        // §17.5.3: a row is as tall as its tallest cell. A cell spanning
        // several rows grows the last row it covers.
        let mut row_heights = vec![0.0_f32; structure.rows.len()];
        for (index, row) in cells.iter().enumerate() {
            for cell in row.iter().filter(|cell| cell.placement.row_span == 1) {
                row_heights[index] = row_heights[index].max(cell.natural);
            }
        }
        for (index, row) in cells.iter().enumerate() {
            for cell in row.iter().filter(|cell| cell.placement.row_span > 1) {
                let last = index + cell.placement.row_span - 1;
                let spanned: f32 = row_heights[index..=last].iter().sum();
                if cell.natural > spanned {
                    row_heights[last] += cell.natural - spanned;
                }
            }
        }

        // §17.4: a caption is a block-level box at the table's used width,
        // before or after the row stack. Its own width, margins and
        // text-alignment belong to the caption box, so the block solver lays it
        // out unchanged; only its position depends on the row heights.
        let mut captions = Vec::with_capacity(structure.captions.len());
        for caption in &structure.captions {
            let Some(result) = self.layout_block_like(
                *caption,
                PhysicalRect::new(containing.origin.x, 0.0, containing.size.width, 0.0),
                positioning_containing,
                0.0,
                depth.saturating_add(1),
                false,
            ) else {
                continue;
            };
            let top = self.caption_side(*caption);
            captions.push(TableCaption {
                fragment: result.fragment,
                outer_height: result.outer_height,
                top,
            });
        }
        let caption_extent = |top: bool| -> f32 {
            captions
                .iter()
                .filter(|caption| caption.top == top)
                .map(|caption| caption.outer_height)
                .sum()
        };
        let rows_height: f32 = row_heights.iter().sum();
        // §17.5.3: a table taller than its rows has the extra space to place.
        // Each row group takes an equal share of it and aligns its own rows
        // inside that share, so a group with `vertical-align: bottom` ends at
        // the bottom of the table.
        let extra = used_height.map_or(0.0, |height| {
            (height - caption_extent(true) - caption_extent(false) - rows_height).max(0.0)
        });
        let (row_offsets, group_extra) =
            self.row_group_offsets(extra, &structure, &group_boxes, style);
        let mut row_ys = Vec::with_capacity(row_heights.len());
        let mut y = containing.origin.y + caption_extent(true);
        for (height, offset) in row_heights.iter().zip(row_offsets.iter()) {
            row_ys.push(y + offset);
            y += height;
        }

        let mut fragments: Vec<FragmentId> = captions
            .iter()
            .filter(|caption| caption.top)
            .map(|caption| caption.fragment)
            .collect();
        let mut next_group = 0;
        for (index, row) in cells.iter().enumerate() {
            // A row group covers a run of rows, and its box spans them all plus
            // the share of a taller table it was given.
            while next_group < group_boxes.len() && group_boxes[next_group].1 <= index {
                let (start, end, _, fragment) = group_boxes[next_group];
                self.translate_fragment_subtree(fragment, 0.0, row_ys[start]);
                self.finish_box(
                    fragment,
                    row_heights[start..end].iter().sum::<f32>() + group_extra[next_group],
                    (start..end).filter_map(|row| row_boxes[row]).collect(),
                );
                fragments.push(fragment);
                next_group += 1;
            }
            // The row's baseline is the first cell of the row that has one
            // (§17.5.3).
            let row_baseline = row
                .iter()
                .find_map(|cell| self.fragment_first_baseline(cell.fragment));
            for cell in row {
                let target = row_heights[index..index + cell.placement.row_span]
                    .iter()
                    .sum::<f32>()
                    .max(cell.natural);
                if cell.stretch {
                    self.resize_fragment_outer_height(cell.fragment, target);
                }
                // §17.5.3 places the cell's *content* within the cell's content
                // box, which is not the same height as the content: a cell with
                // a height of its own is taller than what is in it.
                let content_box = self
                    .fragment_content_height(cell.fragment)
                    .unwrap_or(cell.flow);
                let cell_style = self
                    .formatting
                    .get(cell.placement.node)
                    .and_then(|node| node.style_source)
                    .and_then(|source| self.styles.get(&source));
                let offset = match Self::table_vertical_align(cell_style) {
                    VerticalAlign::Top => 0.0,
                    VerticalAlign::Middle => (content_box - cell.flow) / 2.0,
                    VerticalAlign::Bottom => content_box - cell.flow,
                    // §17.5.3: the cell's baseline goes on the row's baseline.
                    // A cell with no in-flow line box has no baseline of its
                    // own, so its bottom margin edge is used instead (CSS
                    // Tables 3 §3.1.1 baseline synthesis).
                    VerticalAlign::Baseline => {
                        row_baseline.unwrap_or(0.0)
                            - self
                                .fragment_first_baseline(cell.fragment)
                                .unwrap_or(cell.natural)
                    }
                };
                // The cell box itself fills its row; `vertical-align` only
                // positions the in-flow content inside it.
                self.translate_fragment_subtree(cell.fragment, 0.0, row_ys[index]);
                self.translate_fragment_content(cell.fragment, offset);
            }
            if let Some(fragment) = row_boxes[index] {
                self.translate_fragment_subtree(fragment, 0.0, row_ys[index]);
                self.finish_box(
                    fragment,
                    row_heights[index],
                    row.iter().map(|cell| cell.fragment).collect(),
                );
                if structure.rows[index].group.is_none() {
                    fragments.push(fragment);
                }
            }
        }
        for (offset, (start, end, _, fragment)) in
            group_boxes[next_group..].iter().copied().enumerate()
        {
            self.translate_fragment_subtree(fragment, 0.0, row_ys[start]);
            self.finish_box(
                fragment,
                row_heights[start..end].iter().sum::<f32>() + group_extra[next_group + offset],
                (start..end).filter_map(|row| row_boxes[row]).collect(),
            );
            fragments.push(fragment);
        }
        for caption in &captions {
            self.translate_fragment_subtree(
                caption.fragment,
                0.0,
                if caption.top {
                    containing.origin.y
                } else {
                    containing.origin.y + caption_extent(true) + rows_height
                },
            );
            if !caption.top {
                fragments.push(caption.fragment);
            }
        }
        (
            fragments,
            caption_extent(true) + rows_height + caption_extent(false),
        )
    }

    /// CSS 2.1 §17.2.1: flatten the row groups into the table's row list.
    /// `tree.rs` has already generated the anonymous rows, so anything that is
    /// not a declared row group is a row.
    fn collect_table_rows(
        &mut self,
        node: FormattingNodeId,
        group: Option<FormattingNodeId>,
        rows: &mut Vec<TableRow>,
    ) {
        match self.table_display(node) {
            Some(
                DisplayInternal::TableRowGroup
                | DisplayInternal::TableHeaderGroup
                | DisplayInternal::TableFooterGroup,
            ) => {
                let children = self
                    .formatting
                    .get(node)
                    .map(|node| node.children.clone())
                    .unwrap_or_default();
                for child in children {
                    self.collect_table_rows(child, Some(node), rows);
                }
            }
            _ => rows.push(TableRow { group, node }),
        }
    }

    /// CSS 2.1 §17.2.1: assign every cell a grid position. A cell that spans
    /// columns or rows occupies those slots, so the cells that follow it in a
    /// later row step over them.
    fn table_grid(&mut self, rows: &[TableRow]) -> (Vec<Vec<CellPlacement>>, usize) {
        let mut spanned: Vec<HashSet<usize>> = vec![HashSet::new(); rows.len()];
        let mut grid = Vec::with_capacity(rows.len());
        let mut columns = 0;
        for (index, row) in rows.iter().enumerate() {
            let children = self
                .formatting
                .get(row.node)
                .map(|node| node.children.clone())
                .unwrap_or_default();
            let mut cells = Vec::new();
            let mut column = 0;
            for child in children {
                let source = self.formatting.get(child).and_then(|node| node.source);
                let column_span = self.span_attribute(source, "colspan");
                // A rowspan reaching past the last row is ignored (§17.5).
                let row_span = self
                    .span_attribute(source, "rowspan")
                    .min(rows.len() - index);
                while spanned[index].contains(&column) {
                    column += 1;
                }
                for slots in &mut spanned[index..index + row_span] {
                    slots.extend(column..column + column_span);
                }
                cells.push(CellPlacement {
                    node: child,
                    column,
                    column_span,
                    row_span,
                });
                column += column_span;
                columns = columns.max(column);
            }
            grid.push(cells);
        }
        (grid, columns)
    }

    /// `colspan`, `rowspan` and `span` are HTML attributes with no
    /// presentational-attribute equivalent, so unlike `valign` or `cellpadding`
    /// there is no hint layer to route them through: they are read straight from
    /// the DOM.
    fn span_attribute(&self, source: Option<NodeId>, name: &str) -> usize {
        source
            .and_then(|source| self.dom.attribute(source, name).ok().flatten())
            .and_then(|value| value.trim().parse::<usize>().ok())
            .filter(|span| *span > 0)
            .unwrap_or(1)
    }

    /// CSS 2.1 §17.5.2.5 automatic table layout: a column is as wide as the
    /// widest of its cells, and a cell spanning several columns shares its
    /// width equally between them. A cell with a definite width fixes its
    /// column; the width the table has left over is shared out among the
    /// remaining columns proportionally to what they asked for, so a table
    /// whose content fits fills its used width exactly.
    ///
    /// Returns the column widths and the table content width they occupy. §17.5.2.2
    /// grows an over-constrained table to the sum of its columns rather than
    /// squeezing text that has nowhere to wrap, so the returned width can be
    /// larger than the one that was asked for.
    #[allow(clippy::too_many_arguments)]
    fn table_column_widths(
        &mut self,
        style: Option<&ComputedStyle>,
        grid: &[Vec<CellPlacement>],
        column_boxes: &[FormattingNodeId],
        table_width: f32,
        spacing: f32,
        columns: usize,
    ) -> (f32, Vec<f32>) {
        let mut fixed = vec![None; columns];
        if Self::table_layout_is_fixed(style) {
            // §17.5.2.1: no cell is measured. The first row's cells and the
            // column boxes are the only sources of a column width.
            self.apply_column_box_widths(column_boxes, table_width, columns, &mut fixed);
            if let Some(first) = grid.first() {
                for cell in first {
                    let source = self.formatting.get(cell.node).and_then(|node| node.source);
                    let style = self
                        .formatting
                        .get(cell.node)
                        .and_then(|node| node.style_source)
                        .and_then(|source| self.styles.get(&source));
                    let Some(width) =
                        self.resolve_size_against(style, "width", Some(table_width), source)
                    else {
                        continue;
                    };
                    let extras = self.cell_horizontal_extras(style, table_width, source);
                    let share = count_as_f32(cell.column_span);
                    for slot in fixed.iter_mut().skip(cell.column).take(cell.column_span) {
                        let value = ((width + extras) / share).max(0.0);
                        if slot.is_none_or(|current| value > current) {
                            *slot = Some(value);
                        }
                    }
                }
            }
            return Self::fixed_table_column_widths(&fixed, table_width, spacing, columns);
        }
        let (minimum, maximum, fixed) =
            self.table_column_intrinsic_widths(grid, table_width, columns);
        let mut fixed = fixed;
        // §17.5.1: a column box's `width` is a minimum and a preferred width
        // for the columns it covers, so it overrides what the cells asked for.
        self.apply_column_box_widths(column_boxes, table_width, columns, &mut fixed);
        // §17.5.2.2: the columns share the table's used width, and the border
        // spacing that separates and surrounds them is taken out first.
        let spacing_total = spacing * count_as_f32(columns.saturating_add(1));
        let available = (table_width - spacing_total).max(0.0);
        let mut widths = vec![0.0_f32; columns];
        let mut auto = Vec::with_capacity(columns);
        for column in 0..columns {
            match fixed[column] {
                Some(width) => widths[column] = width,
                None => auto.push(column),
            }
        }
        let free = (available - widths.iter().sum::<f32>()).max(0.0);
        let minimum: Vec<f32> = auto.iter().map(|column| minimum[*column]).collect();
        let maximum: Vec<f32> = auto.iter().map(|column| maximum[*column]).collect();
        let (base, span): (Vec<f32>, Vec<f32>) = if maximum.iter().sum::<f32>() <= free {
            // Everything fits, so each auto column takes its max-content share
            // of the remaining space and the table is exactly full.
            (vec![0.0; auto.len()], maximum)
        } else {
            // The columns that do not fit keep their minimum width and share
            // what is left of the free space in proportion to the room they
            // could still use. When the free space does not even cover the
            // minimums the remainder is zero and the table grows instead.
            let span = maximum
                .iter()
                .zip(&minimum)
                .map(|(maximum, minimum)| (maximum - minimum).max(0.0))
                .collect();
            (minimum, span)
        };
        let share: f32 = span.iter().sum();
        let remainder = (free - base.iter().sum::<f32>()).max(0.0);
        for (index, column) in auto.iter().enumerate() {
            widths[*column] = base[index]
                + if share > 0.0 {
                    remainder * span[index] / share
                } else {
                    0.0
                };
        }
        (
            (widths.iter().sum::<f32>() + spacing_total).max(table_width),
            widths,
        )
    }

    /// §17.5.1: a column box's `width` sizes the columns its `span` covers.
    fn apply_column_box_widths(
        &mut self,
        column_boxes: &[FormattingNodeId],
        table_width: f32,
        columns: usize,
        fixed: &mut [Option<f32>],
    ) {
        let mut column = 0;
        for box_id in column_boxes.iter().copied() {
            let source = self.formatting.get(box_id).and_then(|node| node.source);
            let style = self
                .formatting
                .get(box_id)
                .and_then(|node| node.style_source)
                .and_then(|source| self.styles.get(&source));
            let span = self.span_attribute(source, "span").max(1);
            if let Some(width) =
                self.resolve_size_against(style, "width", Some(table_width), source)
            {
                let share = count_as_f32(span);
                for slot in fixed
                    .iter_mut()
                    .skip(column)
                    .take(span.min(columns - column))
                {
                    let value = (width / share).max(0.0);
                    if slot.is_none_or(|current| value > current) {
                        *slot = Some(value);
                    }
                }
            }
            column += span;
        }
    }

    /// CSS 2.1 §17.5.2.1: with `table-layout: fixed` the columns that have no
    /// width of their own divide the table's used width equally, and a table
    /// whose fixed columns do not fit grows to fit them.
    fn fixed_table_column_widths(
        fixed: &[Option<f32>],
        table_width: f32,
        spacing: f32,
        columns: usize,
    ) -> (f32, Vec<f32>) {
        let spacing_total = spacing * count_as_f32(columns.saturating_add(1));
        let available = (table_width - spacing_total).max(0.0);
        let mut widths = vec![0.0_f32; columns];
        let mut auto = Vec::with_capacity(columns);
        for column in 0..columns {
            match fixed[column] {
                Some(width) => widths[column] = width,
                None => auto.push(column),
            }
        }
        if !auto.is_empty() {
            let free = (available - widths.iter().sum::<f32>()).max(0.0);
            let share = free / count_as_f32(auto.len());
            for column in auto {
                widths[column] = share;
            }
        }
        (
            (widths.iter().sum::<f32>() + spacing_total).max(table_width),
            widths,
        )
    }

    /// `table-layout: fixed` takes its column widths from the table alone.
    fn table_layout_is_fixed(style: Option<&ComputedStyle>) -> bool {
        style
            .and_then(|style| style.get("table-layout"))
            .is_some_and(|value| value.css_text().trim().eq_ignore_ascii_case("fixed"))
    }

    /// CSS 2.1 §17.6: `border-collapse` is `separate` by default, and only
    /// `collapse` shares the cell borders with their neighbours.
    fn borders_are_collapsed(style: Option<&ComputedStyle>) -> bool {
        style
            .and_then(|style| style.get("border-collapse"))
            .is_some_and(|value| value.css_text().trim().eq_ignore_ascii_case("collapse"))
    }

    /// CSS 2.1 §17.5.2.5 step 1: the minimum, maximum and definite width each
    /// column asks for. A cell spanning several columns divides each of them
    /// equally between the columns it covers.
    fn table_column_intrinsic_widths(
        &mut self,
        grid: &[Vec<CellPlacement>],
        table_width: f32,
        columns: usize,
    ) -> (Vec<f32>, Vec<f32>, Vec<Option<f32>>) {
        let mut minimum = vec![0.0_f32; columns];
        let mut maximum = vec![0.0_f32; columns];
        let mut fixed: Vec<Option<f32>> = vec![None; columns];
        for row in grid {
            for cell in row {
                let source = self.formatting.get(cell.node).and_then(|node| node.source);
                let style = self
                    .formatting
                    .get(cell.node)
                    .and_then(|node| node.style_source)
                    .and_then(|source| self.styles.get(&source));
                let share = count_as_f32(cell.column_span);
                let minimum_width = self.cell_border_box_intrinsic_width(cell.node, true) / share;
                let maximum_width = self.cell_border_box_intrinsic_width(cell.node, false) / share;
                // §10.4: a cell's `min-width` is a floor on the column it sits
                // in, and a specified `width` is a content width, so its column
                // must also fit the cell's padding and border.
                let floor =
                    self.resolve_size_against(style, "min-width", Some(table_width), source);
                let specified =
                    self.resolve_size_against(style, "width", Some(table_width), source);
                let extras = self.cell_horizontal_extras(style, table_width, source);
                for column in cell.column..cell.column + cell.column_span {
                    minimum[column] = minimum[column].max(minimum_width);
                    if let Some(floor) = floor {
                        minimum[column] = minimum[column].max((floor + extras) / share);
                    }
                    maximum[column] = maximum[column].max(maximum_width);
                    if let Some(width) = specified {
                        // §17.5.2.5 shares a cell's width equally between the
                        // columns it spans, exactly like its intrinsic widths.
                        let value = ((width + extras) / share).max(0.0);
                        fixed[column] =
                            Some(fixed[column].map_or(value, |current: f32| current.max(value)));
                    }
                }
            }
        }
        (minimum, maximum, fixed)
    }

    /// CSS 2.1 §17.5.2.2 and §17.5.2.5: the used content width of a `display:
    /// table` box, and the column widths that fill it.
    ///
    /// `available` is the width the block algorithm resolved for the box and
    /// `definite` says whether it came from `width` or from the block rule that
    /// fills the containing block. An `auto` table shrink-to-fits the sum of its
    /// columns first, clamped to the containing block; either way the result is
    /// then grown to the sum of the column widths, so an over-constrained table
    /// overflows instead of squeezing text that has nowhere to wrap. The caller
    /// applies `min-width` and `max-width` to the result.
    ///
    /// This runs before the rows are laid out so that the table's fragment, the
    /// containing block of its positioned descendants and its columns all use
    /// one width.
    pub(super) fn table_content_width(
        &mut self,
        node: FormattingNodeId,
        style: Option<&ComputedStyle>,
        available: f32,
        definite: bool,
    ) -> f32 {
        let available = available.max(0.0);
        let structure = self.table_structure(&self.in_flow_children(node));
        // A collapsed border and a hidden empty cell change what a cell occupies,
        // so they have to be resolved before the columns are measured.
        self.apply_table_edge_rules(node, style, available, &structure);
        let spacing = Self::table_border_spacing(style, available);
        let columns = structure.columns;
        if Self::table_layout_is_fixed(style) || definite {
            // §17.5.2.2: with a definite width the columns share the table's used
            // width and the result is grown to the sum of the columns.
            //
            // §17.5.2.1 says the same of a fixed-layout table, whose columns
            // never measure their cells but whose width is still "the greater of
            // the value of the 'width' property for the table element and the sum
            // of the column widths (plus cell spacing or borders)". Answering a
            // fixed table with `available` unconditionally left it narrower than
            // the columns its own first row declares, so the cells overflowed it:
            // a 300px table with three 100px cells measured 300 while its cells
            // tiled 306 and the last hung 6px outside the table box. A cell's
            // `width` is a *content* width (§10.4), so its padding and border
            // belong to the column width and the declared 300 never covered them.
            return self
                .table_column_widths(
                    style,
                    &structure.grid,
                    &structure.column_boxes,
                    available,
                    spacing,
                    columns,
                )
                .0;
        }
        let spacing_total = spacing * count_as_f32(columns.saturating_add(1));
        let (minimum, maximum, fixed) =
            self.table_column_intrinsic_widths(&structure.grid, available, columns);
        let mut fixed = fixed;
        self.apply_column_box_widths(&structure.column_boxes, available, columns, &mut fixed);
        let total = |widths: &[f32]| -> f32 {
            (0..columns)
                .map(|column| widths[column].max(fixed[column].unwrap_or(0.0)))
                .sum::<f32>()
                + spacing_total
        };
        let fit = total(&minimum).max(total(&maximum)).min(available);
        self.table_column_widths(
            style,
            &structure.grid,
            &structure.column_boxes,
            fit,
            spacing,
            columns,
        )
        .0
    }

    /// The column widths and the used content width for a table box. The
    /// sizing pass and the layout pass have to agree on them, and measuring the
    /// cells twice is not free, so the sizing pass leaves its result here.
    pub(super) fn table_column_layout(
        &mut self,
        node: FormattingNodeId,
        style: Option<&ComputedStyle>,
        available: f32,
    ) -> (f32, Vec<f32>) {
        if let Some(columns) = self.table_columns.get(&node) {
            return (columns.0, columns.1.clone());
        }
        let structure = self.table_structure(&self.in_flow_children(node));
        let spacing = Self::table_border_spacing(style, available);
        let sized = self.table_column_widths(
            style,
            &structure.grid,
            &structure.column_boxes,
            available,
            spacing,
            structure.columns,
        );
        self.table_columns.insert(node, (sized.0, sized.1.clone()));
        sized
    }

    /// CSS 2.1 §17.5.2.2: a table's min-content and max-content widths are the
    /// sums of its column widths plus the border spacing, not the widest of its
    /// rows. A table nested in a cell is measured through this, so the cell
    /// around it is never narrower than the table inside it.
    pub(super) fn table_intrinsic_widths(
        &mut self,
        node: FormattingNodeId,
        style: Option<&ComputedStyle>,
        available: f32,
        minimum: bool,
    ) -> f32 {
        if Self::table_layout_is_fixed(style) {
            // §17.5.2.1: a fixed-layout table's width comes from the table, not
            // from its content, so it only has the width it is given.
            return available.max(0.0);
        }
        let structure = self.table_structure(&self.in_flow_children(node));
        self.apply_table_edge_rules(node, style, available, &structure);
        let (minimum_total, maximum_total) =
            self.table_column_totals_of(&structure, style, available);
        let columns = if minimum {
            minimum_total
        } else {
            maximum_total
        };
        // A generic intrinsic measurement is a border-box width, so the table's
        // own padding and border are part of it.
        columns
            + self.cell_horizontal_extras(
                style,
                available,
                self.formatting.get(node).and_then(|node| node.source),
            )
    }

    /// `true` for a `display: table` box, the only kind of box whose intrinsic
    /// width the table algorithm measures with its own column widths.
    pub(super) fn is_table(&self, node: FormattingNodeId) -> bool {
        matches!(
            self.formatting.get(node).map(|node| &node.kind),
            Some(FormattingNodeKind::BlockContainer {
                context: FormattingContextKind::Table
            })
        )
    }

    /// The (min-content, max-content) widths of a table box, both measured as
    /// the sum of its column widths.
    /// The (min-content, max-content) widths of a table box, both measured as
    /// the sum of its column widths.
    fn table_column_totals_of(
        &mut self,
        structure: &TableStructure,
        style: Option<&ComputedStyle>,
        available: f32,
    ) -> (f32, f32) {
        let available = available.max(0.0);
        let columns = structure.columns;
        let spacing = Self::table_border_spacing(style, available);
        let spacing_total = spacing * count_as_f32(columns.saturating_add(1));
        let (minimum, maximum, fixed) =
            self.table_column_intrinsic_widths(&structure.grid, available, columns);
        let mut fixed = fixed;
        self.apply_column_box_widths(&structure.column_boxes, available, columns, &mut fixed);
        let total = |widths: &[f32]| -> f32 {
            (0..columns)
                .map(|column| widths[column].max(fixed[column].unwrap_or(0.0)))
                .sum::<f32>()
                + spacing_total
        };
        (total(&minimum), total(&maximum))
    }

    /// The in-flow children of a table box, which are the ones that make up its
    /// rows. An out-of-flow child takes no grid slot.
    fn in_flow_children(&self, node: FormattingNodeId) -> Vec<FormattingNodeId> {
        self.formatting
            .get(node)
            .map(|node| node.children.clone())
            .unwrap_or_default()
            .into_iter()
            .filter(|child| !self.is_out_of_flow(*child))
            .collect()
    }

    /// CSS 2.1 §17.5.2.5: a cell's minimum width is its min-content width and
    /// its maximum width is its max-content width, and both are border-box
    /// widths, so the cell's own padding and border are added to the shared
    /// intrinsic measurements of its content.
    fn cell_border_box_intrinsic_width(&mut self, node: FormattingNodeId, minimum: bool) -> f32 {
        let content = if minimum {
            self.min_content_width(node)
        } else {
            self.max_content_width(node)
        };
        let source = self.formatting.get(node).and_then(|node| node.source);
        let style = self
            .formatting
            .get(node)
            .and_then(|node| node.style_source)
            .and_then(|source| self.styles.get(&source));
        if matches!(
            self.formatting.get(node).map(|node| &node.kind),
            Some(
                FormattingNodeKind::AtomicInline { .. }
                    | FormattingNodeKind::Text(_)
                    | FormattingNodeKind::Inline
                    | FormattingNodeKind::AnonymousBlock
            )
        ) {
            // These boxes have no content box of their own: an inline
            // sequence's measurement already covers its whole width, and a
            // replaced box's measurement is already an outer width.
            return content;
        }
        content + self.cell_horizontal_extras(style, self.options.viewport.width, source)
    }

    /// CSS 2.1 §17.6.2: in the collapsed border model every cell border is shared
    /// with its neighbours and with the table's own border, so only one border
    /// survives on each grid line and every loser collapses to nothing.
    ///
    /// The winner keeps the whole resolved width rather than half of it. The
    /// space the grid line occupies stays correct, which is what the column
    /// widths and the content offsets depend on, and the surviving border is
    /// painted by the cell that won it with its own style and colour, so the
    /// painter needs no knowledge of the conflict.
    ///
    /// The residual difference from §17.6.2's row-width equation is that the
    /// winner's content is inset by the whole border rather than by half of it,
    /// which makes the columns *unequal*: three cells that declare the same
    /// 1px border resolve to 104/103/103 rather than to 103/103/103, a 1px
    /// drift that accumulates across the grid. That is a real per-column error
    /// and not a sub-pixel one, but closing it needs the per-side resolved
    /// *colour* on the fragment so one border can be painted across both
    /// halves - `BoxGeometry` has no slot for it and the painter resolves
    /// colour from the element's computed style
    /// (`crates/render-core/src/paint/display_list.rs:2842`). Splitting the
    /// width here without that would paint a shared line in two colours and
    /// would make §17.6.2.1 rule 3's equal-width tie-break unobservable, so
    /// the asymmetry is recorded rather than taken.
    fn collapse_table_borders(
        &mut self,
        node: FormattingNodeId,
        table_style: Option<&ComputedStyle>,
        basis: f32,
        structure: &TableStructure,
    ) {
        // A claim is keyed by the grid line it sits on: a horizontal line
        // separates two rows at one column, a vertical line separates two
        // columns at one row.
        let mut lines: BTreeMap<(bool, usize, usize), Vec<BorderClaim>> = BTreeMap::new();
        // The border each box resolves to before any of it collapses, so that
        // losing one edge leaves the other three alone.
        let mut resolved: BTreeMap<NodeId, EdgeSizes> = BTreeMap::new();
        for (row_index, row) in structure.grid.iter().enumerate() {
            for cell in row {
                let Some(source) = self.formatting.get(cell.node).and_then(|node| node.source)
                else {
                    continue;
                };
                let style = self
                    .formatting
                    .get(cell.node)
                    .and_then(|node| node.style_source)
                    .and_then(|source| self.styles.get(&source));
                let mut edges = EdgeSizes::default();
                for side in SIDES {
                    let (horizontal, line_row, line_column) =
                        Self::grid_line(row_index, cell, side);
                    let claim = self.border_claim(Some(source), style, side, basis);
                    edges = side.with(edges, claim.width);
                    lines
                        .entry((horizontal, line_row, line_column))
                        .or_default()
                        .push(claim);
                }
                resolved.insert(source, edges);
            }
        }
        // The table's own border claims the outer grid lines like any other
        // claimant (§17.6.2.1). It is collected *after* the cells, so a tie goes
        // to the cell - which is what rule 4's type order says, since the
        // table is last in it - and it is recorded in `resolved` like every
        // other claimant so that a cell which beats it can collapse the table's
        // own edge to nothing.
        if let Some(source) = self.formatting.get(node).and_then(|node| node.source) {
            let mut edges = EdgeSizes::default();
            for column in 0..structure.columns {
                for (side, line_row) in [(Side::Top, 0), (Side::Bottom, structure.rows.len())] {
                    let claim = self.border_claim(Some(source), table_style, side, basis);
                    edges = side.with(edges, claim.width);
                    lines
                        .entry((true, line_row, column))
                        .or_default()
                        .push(claim);
                }
            }
            for row in 0..structure.rows.len() {
                for (side, line_column) in [(Side::Left, 0), (Side::Right, structure.columns)] {
                    let claim = self.border_claim(Some(source), table_style, side, basis);
                    edges = side.with(edges, claim.width);
                    lines
                        .entry((false, row, line_column))
                        .or_default()
                        .push(claim);
                }
            }
            resolved.insert(source, edges);
        }
        for (source, edges) in resolved {
            self.collapsed_edges.entry(source).or_default().border = Some(edges);
        }
        for competing in lines.values() {
            let Some(winner) = collapsed_border_winner(competing) else {
                continue;
            };
            for (index, claim) in competing.iter().enumerate() {
                if index != winner
                    && let Some(source) = claim.source
                {
                    self.collapse_cell_edge(source, claim.side);
                }
            }
        }
    }

    /// The grid line one edge of a cell sits on, as (is horizontal, row line,
    /// column line).
    const fn grid_line(row_index: usize, cell: &CellPlacement, side: Side) -> (bool, usize, usize) {
        match side {
            Side::Top => (true, row_index, cell.column),
            Side::Bottom => (true, row_index + 1, cell.column),
            Side::Left => (false, row_index, cell.column),
            Side::Right => (false, row_index, cell.column + cell.column_span),
        }
    }

    /// The border one side of a box declares, as a claim on its grid line.
    fn border_claim(
        &mut self,
        source: Option<NodeId>,
        style: Option<&ComputedStyle>,
        side: Side,
        basis: f32,
    ) -> BorderClaim {
        let (border, _) = self.resolve_box_edges(style, basis, source);
        let width = match side {
            Side::Top => border.top,
            Side::Right => border.right,
            Side::Bottom => border.bottom,
            Side::Left => border.left,
        };
        let border_style = border_style_of(style, side);
        BorderClaim {
            source,
            side,
            width: if border_style == BorderStyle::Hidden {
                // A `hidden` border takes no space; it only wins.
                0.0
            } else {
                width
            },
            style: border_style,
        }
    }

    /// Collapse one edge of a claimant so that it occupies no space. The box's
    /// resolved border has to be in the map already, otherwise the sides that
    /// did not lose would be zeroed as well.
    ///
    /// This is also how the table's own border loses an outer grid line: the
    /// table is an ordinary claimant, so a cell with a wider `border-left`
    /// takes the line and the table's edge collapses to nothing rather than
    /// winning by being the table (§17.6.2.1).
    fn collapse_cell_edge(&mut self, source: NodeId, side: Side) {
        let Some(edges) = self.collapsed_edges.get_mut(&source) else {
            return;
        };
        let Some(border) = edges.border.as_mut() else {
            return;
        };
        *border = side.with(*border, 0.0);
    }

    /// CSS 2.1 ��17.5.2.1: `empty-cells: hide` collapses the borders and padding
    /// of a cell with no content so that the cell takes no space. It only
    /// applies to the separated border model.
    fn hide_empty_cells(&mut self, style: Option<&ComputedStyle>, structure: &TableStructure) {
        let hidden = style
            .and_then(|style| style.get("empty-cells"))
            .is_some_and(|value| value.css_text().trim().eq_ignore_ascii_case("hide"));
        if !hidden {
            return;
        }
        for row in &structure.grid {
            for cell in row {
                if self
                    .formatting
                    .get(cell.node)
                    .is_none_or(|node| !node.children.is_empty())
                {
                    continue;
                }
                let Some(source) = self.formatting.get(cell.node).and_then(|node| node.source)
                else {
                    continue;
                };
                self.collapsed_edges.insert(
                    source,
                    CollapsedEdges {
                        border: Some(EdgeSizes::default()),
                        padding: Some(EdgeSizes::default()),
                    },
                );
            }
        }
    }

    /// The effects the table algorithm has on the cells' own edges, resolved
    /// before the columns are measured because a column has to reserve exactly
    /// what its cells will occupy.
    fn apply_table_edge_rules(
        &mut self,
        node: FormattingNodeId,
        style: Option<&ComputedStyle>,
        basis: f32,
        structure: &TableStructure,
    ) {
        if Self::borders_are_collapsed(style) {
            self.collapse_table_borders(node, style, basis, structure);
        } else {
            self.hide_empty_cells(style, structure);
        }
    }

    /// [`Self::apply_table_edge_rules`] for the caller that has to run it
    /// *before* the block algorithm resolves the table box's own edges.
    ///
    /// §17.6.2.1 makes the table an ordinary claimant on the outer grid lines,
    /// so the table's used border is whatever survives there - not what it
    /// declares. A cell with a wider `border-left` collapses the table's own
    /// left border to nothing, and a content width computed against the
    /// declared border would then be 2px narrower than the box the grid is
    /// placed in, putting the cells outside their own table. Resolving it here
    /// makes `non_content` and the columns agree on one number.
    pub(super) fn resolve_table_edge_rules_before_sizing(
        &mut self,
        node: FormattingNodeId,
        style: Option<&ComputedStyle>,
        basis: f32,
    ) {
        let structure = self.table_structure(&self.in_flow_children(node));
        self.apply_table_edge_rules(node, style, basis, &structure);
    }

    /// The used border and padding of a cell, with the table's collapsing
    /// applied. A column width has to reserve exactly what the cell will occupy.
    fn cell_horizontal_extras(
        &mut self,
        style: Option<&ComputedStyle>,
        basis: f32,
        source: Option<NodeId>,
    ) -> f32 {
        let (border, padding) = self.resolve_box_edges(style, basis, source);
        padding.horizontal() + border.horizontal()
    }

    /// CSS 2.1 §17.6.1: `border-spacing` separates adjacent cell borders in the
    /// separated table model, and a percentage resolves against the table's used
    /// width. `border-collapse: collapse` removes the spacing, which is the other
    /// half of §17.6: the space between two cells is where their shared border
    /// lives.
    fn table_border_spacing(style: Option<&ComputedStyle>, table_width: f32) -> f32 {
        if Self::borders_are_collapsed(style) {
            return 0.0;
        }
        style
            .and_then(|style| style.get("border-spacing"))
            .and_then(|value| {
                let horizontal = value.css_text().split_whitespace().next()?.to_owned();
                parse_text_length(&horizontal.to_ascii_lowercase(), table_width)
            })
            .unwrap_or(0.0)
            .max(0.0)
    }

    /// CSS 2.1 §17.4: `caption-side: top` is the initial value.
    fn caption_side(&self, node: FormattingNodeId) -> bool {
        !self
            .formatting
            .get(node)
            .and_then(|node| node.source)
            .and_then(|source| self.styles.get(&source))
            .and_then(|style| style.get("caption-side"))
            .is_some_and(|value| value.css_text().trim().eq_ignore_ascii_case("bottom"))
    }

    /// CSS 2.1 §17.2.1: the table structure - the captions, the rows in
    /// document order, the grid slot every cell occupies, and the column boxes
    /// whose `width` sizes the columns they cover (§17.5.1).
    fn table_structure(&mut self, children: &[FormattingNodeId]) -> TableStructure {
        let mut captions = Vec::new();
        let mut rows = Vec::new();
        let mut column_boxes = Vec::new();
        for child in children.iter().copied() {
            match self.table_display(child) {
                Some(DisplayInternal::TableCaption) => captions.push(child),
                Some(DisplayInternal::TableColumn) => column_boxes.push(child),
                Some(DisplayInternal::TableColumnGroup) => {
                    // §17.5.1: a column group sizes the columns of the column
                    // boxes inside it; one without any of them covers a single
                    // column.
                    let nested: Vec<FormattingNodeId> = self
                        .formatting
                        .get(child)
                        .map(|node| node.children.clone())
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|nested| {
                            self.table_display(*nested) == Some(DisplayInternal::TableColumn)
                        })
                        .collect();
                    if nested.is_empty() {
                        column_boxes.push(child);
                    } else {
                        column_boxes.extend(nested);
                    }
                }
                _ => self.collect_table_rows(child, None, &mut rows),
            }
        }
        let (grid, columns) = self.table_grid(&rows);
        TableStructure {
            captions,
            rows,
            grid,
            columns,
            column_boxes,
        }
    }

    /// CSS 2.1 §10.8.3 gives `vertical-align` the initial value `baseline`, and
    /// CSS 2.1 §17.5.3 is what applies it inside a table cell: the cell's
    /// baseline is aligned with the row's. No user-agent stylesheet overrides
    /// it, so a cell with no `vertical-align` of its own is baseline-aligned;
    /// `middle` has to be asked for.
    fn table_vertical_align(style: Option<&ComputedStyle>) -> VerticalAlign {
        match style
            .and_then(|style| style.get("vertical-align"))
            .map(|value| value.css_text().trim().to_ascii_lowercase())
            .as_deref()
        {
            Some("top") => VerticalAlign::Top,
            Some("middle") => VerticalAlign::Middle,
            Some("bottom") => VerticalAlign::Bottom,
            _ => VerticalAlign::Baseline,
        }
    }

    /// The children of a fragment, so a cell's in-flow content can be moved
    /// without moving the cell's own border box.
    fn fragment_children(&self, fragment: FragmentId) -> Vec<FragmentId> {
        usize::try_from(fragment.as_u32())
            .ok()
            .and_then(|index| self.fragments.get(index))
            .map(|fragment| fragment.children.clone())
            .unwrap_or_default()
    }

    /// Position a cell's in-flow content inside its border box, which is what
    /// `vertical-align` controls (CSS 2.1 §17.5.3).
    fn translate_fragment_content(&mut self, fragment: FragmentId, dy: f32) {
        for child in self.fragment_children(fragment) {
            self.translate_fragment_subtree(child, 0.0, dy);
        }
    }

    /// The first text baseline inside a fragment subtree. CSS 2.1 §17.5.3
    /// aligns `vertical-align: baseline` cells on the baseline of their first
    /// line box, and a row's baseline is the one of the first cell that has it.
    fn fragment_first_baseline(&self, root: FragmentId) -> Option<f32> {
        let mut stack = vec![root];
        let mut baseline: Option<f32> = None;
        while let Some(id) = stack.pop() {
            let Some(fragment) = usize::try_from(id.as_u32())
                .ok()
                .and_then(|index| self.fragments.get(index))
            else {
                continue;
            };
            if let FragmentKind::Text(text) = &fragment.kind {
                baseline =
                    Some(baseline.map_or(text.baseline, |current: f32| current.min(text.baseline)));
            }
            stack.extend(fragment.children.iter().copied());
        }
        baseline
    }

    /// CSS 2.1 §17.2.1: a row or row group is a box spanning the table's
    /// content width, with no margin, border or padding of its own.
    fn new_table_box(
        &mut self,
        node: FormattingNodeId,
        containing: PhysicalRect,
    ) -> Option<FragmentId> {
        let source = self.formatting.get(node).and_then(|node| node.source);
        let rect = PhysicalRect::new(containing.origin.x, 0.0, containing.size.width, 0.0);
        self.allocate_fragment(
            node,
            source,
            rect,
            FragmentKind::Box(BoxGeometry {
                margin: EdgeSizes::default(),
                border: EdgeSizes::default(),
                padding: EdgeSizes::default(),
                content_rect: rect,
            }),
        )
    }

    /// The table-internal display role of a box. Anonymous boxes never have one:
    /// they carry no source element and `tree.rs` has already placed them.
    fn table_display(&self, node: FormattingNodeId) -> Option<DisplayInternal> {
        self.formatting
            .get(node)
            .and_then(|node| node.source)
            .and_then(|source| self.styles.get(&source))
            .and_then(|style| style.typed("display"))
            .and_then(|display| match display {
                TypedPropertyValue::Display(Display::Internal(internal)) => Some(*internal),
                _ => None,
            })
    }
}
