//! Regression tests for CSS 2.1 §17 table layout: anonymous table boxes, the
//! column width distribution, row heights, spanning and captions.

#![allow(clippy::float_cmp)]

use render_dom::NodeId;

use crate::PhysicalRect;
use crate::fragment::{BoxGeometry, Fragment, FragmentKind};
use crate::geometry::EdgeSizes;
use crate::solver::LayoutOutput;

use super::tests::{find, pipeline};

/// The UA origin a real page gets from render-core's stylesheet, expressed here
/// so a fixture states the table structure it exercises. `border-spacing` is
/// stated too, because the user-agent default of 2px would move every column
/// these geometry assertions measure.
const TABLE_CSS: &str = "html, body, table, caption, colgroup, col, thead, tbody, tfoot, tr, \
                         td, th, div, p, b, a, span { display: block; margin: 0; padding: 0 } \
                         table { display: table; border-spacing: 0 } \
                         caption { display: table-caption } colgroup { display: table-column-group } \
                         col { display: table-column } thead { display: table-header-group } \
                         tbody, tfoot { display: table-row-group } \
                         tr { display: table-row } td, th { display: table-cell }";

fn rect(layout: &LayoutOutput, source: NodeId) -> PhysicalRect {
    layout
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(source))
        .unwrap_or_else(|| panic!("no fragment for the requested element"))
        .rect
}

fn text_rect(layout: &LayoutOutput, needle: &str) -> PhysicalRect {
    layout
        .fragments
        .iter()
        .find(|fragment| matches!(&fragment.kind, FragmentKind::Text(text) if text.text.contains(needle)))
        .unwrap_or_else(|| panic!("no text fragment containing {needle:?}"))
        .rect
}

fn fragment(layout: &LayoutOutput, source: NodeId) -> &Fragment {
    layout
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(source))
        .unwrap_or_else(|| panic!("no fragment for the requested element"))
}

fn near(actual: f32, expected: f32) -> bool {
    (actual - expected).abs() < 0.01
}

/// The used border of a box, which is where the table algorithm's border
/// collapsing is observable.
fn border_of(layout: &LayoutOutput, source: NodeId) -> EdgeSizes {
    geometry_of(layout, source).border
}

/// The used box geometry of a box fragment.
fn geometry_of(layout: &LayoutOutput, source: NodeId) -> BoxGeometry {
    match &fragment(layout, source).kind {
        FragmentKind::Box(geometry) => geometry.clone(),
        other @ FragmentKind::Text(_) => panic!("{source:?} is {other:?}"),
    }
}

/// A reduced `th` + rank/title/points/comments table, the shape a
/// link-voting front page produces. Nothing about the site is encoded here: the
/// fixture only states the table structure.
const NEWS_HTML: &str = "<!doctype html><html><body>\
    <table id='list'>\
      <tr id='head'><th id='h-rank'>#</th><th id='h-title'>Title</th>\
        <th id='h-score'>Score</th><th id='h-comments'>Comments</th></tr>\
      <tr id='first'><td id='c1-rank'>1.</td>\
        <td id='c1-title'><a>First story headline here</a></td>\
        <td id='c1-score'>432</td><td id='c1-comments'>219</td></tr>\
      <tr id='second'><td id='c2-rank'>2.</td>\
        <td id='c2-title'>Second story headline</td>\
        <td id='c2-score'>271</td><td id='c2-comments'>88</td></tr>\
    </table></body></html>";

#[test]
fn table_columns_tile_the_used_width_instead_of_stacking_one_column() {
    // 900px viewport, `width: 85%` gives a 765px table content box. The four
    // columns are 16/188/40/64px of max-content, distributed proportionally
    // because they all fit (CSS 2.1 §17.5.2.5).
    let (output, _, layout) = pipeline(
        NEWS_HTML,
        &format!("{TABLE_CSS} table {{ width: 85% }}"),
        900.0,
    );
    let dom = &output.dom;
    let table = rect(&layout, find(dom, "#list"));
    assert!(near(table.size.width, 765.0), "{table:?}");

    for (rank, title, score, comments) in [
        ("#c1-rank", "#c1-title", "#c1-score", "#c1-comments"),
        ("#c2-rank", "#c2-title", "#c2-score", "#c2-comments"),
    ] {
        let mut expected_x = 0.0;
        let cells: Vec<PhysicalRect> = [rank, title, score, comments]
            .into_iter()
            .map(|selector| rect(&layout, find(dom, selector)))
            .collect();
        // Every cell of a row shares the row's y: the cells are side by side,
        // not one stacked column.
        for cell in &cells {
            assert!(near(cell.origin.y, cells[0].origin.y), "{cells:?}");
        }
        for (column, cell) in cells.iter().enumerate() {
            assert!(
                near(cell.origin.x, expected_x),
                "column {column} started at {} instead of {expected_x}: {cells:?}",
                cell.origin.x
            );
            expected_x = cell.right();
        }
        // The last column ends flush with the table's content edge.
        assert!(near(expected_x, table.right()), "{cells:?}");
    }

    let widths: Vec<f32> = ["#c1-rank", "#c1-title", "#c1-score", "#c1-comments"]
        .into_iter()
        .map(|selector| rect(&layout, find(dom, selector)).size.width)
        .collect();
    for (column, width) in widths.iter().enumerate() {
        let share = 765.0 * [16.0, 188.0, 40.0, 64.0][column] / 308.0;
        assert!(near(*width, share), "column {column}: {width} != {share}");
    }
    // The rank column is the leftmost, not shoved to the far right.
    assert!(near(
        rect(&layout, find(dom, "#c1-rank")).right(),
        widths[0]
    ));

    // Rows stack, the header row first, and the table is as tall as they are.
    let head = rect(&layout, find(dom, "#head"));
    let first = rect(&layout, find(dom, "#first"));
    let second = rect(&layout, find(dom, "#second"));
    assert!(near(head.origin.y, 0.0), "{head:?}");
    assert!(near(first.origin.y, head.bottom()), "{first:?} {head:?}");
    assert!(near(second.origin.y, first.bottom()), "{second:?}");
    assert!(near(table.size.height, second.bottom()), "{table:?}");
    assert!(near(first.size.height, 19.2), "{first:?}");
}

#[test]
fn header_cells_share_the_column_x_of_the_data_cells() {
    let (output, _, layout) = pipeline(NEWS_HTML, TABLE_CSS, 900.0);
    let dom = &output.dom;
    for (header, cell) in [
        ("#h-rank", "#c1-rank"),
        ("#h-title", "#c1-title"),
        ("#h-score", "#c1-score"),
        ("#h-comments", "#c1-comments"),
    ] {
        let header = rect(&layout, find(dom, header));
        let cell = rect(&layout, find(dom, cell));
        assert!(near(header.origin.x, cell.origin.x), "{header:?} {cell:?}");
        assert!(
            near(header.size.width, cell.size.width),
            "{header:?} {cell:?}"
        );
    }
}

#[test]
fn a_cell_with_a_definite_width_fixes_its_column() {
    // CSS 2.1 §17.5.2.5: the remaining space goes to the auto columns, so the
    // fixed column keeps its specified width and the table is exactly full.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><table id='t'>\
           <tr><td id='a' style='width:100px'>aaaaa</td><td id='b'>bbbbb</td></tr>\
         </table></body>",
        &format!("{TABLE_CSS} table {{ width: 400px }}"),
        900.0,
    );
    let dom = &output.dom;
    let a = rect(&layout, find(dom, "#a"));
    let b = rect(&layout, find(dom, "#b"));
    assert!(near(a.size.width, 100.0), "{a:?}");
    assert!(near(a.origin.x, 0.0), "{a:?}");
    assert!(near(b.origin.x, 100.0), "{b:?}");
    assert!(near(b.right(), 400.0), "{b:?}");
    assert!(near(rect(&layout, find(dom, "#t")).size.width, 400.0));
}

#[test]
fn a_cell_forced_break_does_not_inflate_the_column() {
    // §17.5.2.5 measures a cell's maximum width from the shared intrinsic-width
    // helper, so a `<br>` inside a cell ends the line instead of adding to it:
    // the column is the long line, not both lines summed.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><table id='t'>\
           <tr><td id='a'>aaaaaaaaaa<br>bb</td><td id='b' style='width:100px'>b</td></tr>\
         </table></body>",
        &format!("{TABLE_CSS} table {{ border-spacing: 0 }}"),
        900.0,
    );
    let dom = &output.dom;
    let a = rect(&layout, find(dom, "#a"));
    let b = rect(&layout, find(dom, "#b"));
    // 10 characters at 8px, not 10 + 2.
    assert!(near(a.size.width, 80.0), "{a:?}");
    assert!(near(b.origin.x, 80.0), "{b:?}");
}

#[test]
fn a_column_box_width_sizes_the_columns_it_covers() {
    // §17.5.1: a column box's `width` is a minimum and a preferred width for
    // its column, so it wins over the cell content that would otherwise widen
    // it. `span` covers several columns at once.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><table id='t'>\
           <colgroup><col id='first' style='width:100px'><col id='pair' span='2'></colgroup>\
           <tr><td id='a'>aaaaaaaaaa</td><td id='b'>b</td><td id='c'>c</td></tr>\
         </table></body>",
        &format!("{TABLE_CSS} table {{ width: 400px }}"),
        900.0,
    );
    let dom = &output.dom;
    let a = rect(&layout, find(dom, "#a"));
    let b = rect(&layout, find(dom, "#b"));
    let c = rect(&layout, find(dom, "#c"));
    // The first column is 100px even though "aaaaaaaaaa" wants 80px plus the
    // 200px the other two would share.
    assert!(near(a.size.width, 100.0), "{a:?}");
    assert!(near(b.origin.x, 100.0), "{b:?} {a:?}");
    assert!(near(b.size.width, 150.0), "{b:?}");
    assert!(near(c.origin.x, 250.0), "{c:?} {b:?}");
    assert!(near(c.size.width, 150.0), "{c:?}");
    assert!(near(c.right(), 400.0), "{c:?}");
}

#[test]
fn a_fixed_layout_table_measures_no_cells() {
    // §17.5.2.1: with `table-layout: fixed` only the table's own width, the
    // first row's cells and the column boxes decide the column widths.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><table id='t'>\
           <tr><td id='a' style='width:50px'>aaaaaaaaaa</td><td id='b'>b</td></tr>\
         </table></body>",
        &format!("{TABLE_CSS} table {{ width: 200px; table-layout: fixed }}"),
        900.0,
    );
    let dom = &output.dom;
    let a = rect(&layout, find(dom, "#a"));
    let b = rect(&layout, find(dom, "#b"));
    assert!(near(a.size.width, 50.0), "{a:?}");
    // The unspecified column takes the rest equally instead of being measured.
    assert!(near(b.size.width, 150.0), "{b:?}");
    assert!(near(b.origin.x, 50.0), "{b:?}");
    // The content is not measured, so it overflows the 50px cell rather than
    // widening it. UAX #14 rule LB28 gives "aaaaaaaaaa" no break opportunity, so
    // CSS 2.1 §9.4.2 says the run "overflows the line box" and the cell is one
    // line tall.
    assert!(near(a.size.height, 19.2), "{a:?}");
    // A fixed-layout table takes its width from `width`, not from its content.
    assert!(near(rect(&layout, find(dom, "#t")).size.width, 200.0));
}

#[test]
fn a_fixed_layout_table_divides_the_remaining_width_equally() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><table id='t'>\
           <tr><td id='a'>a</td><td id='b'>b</td><td id='c'>c</td><td id='d'>d</td></tr>\
         </table></body>",
        &format!("{TABLE_CSS} table {{ width: 400px; table-layout: fixed }}"),
        900.0,
    );
    let dom = &output.dom;
    for (selector, x) in [("#a", 0.0), ("#b", 100.0), ("#c", 200.0), ("#d", 300.0)] {
        let cell = rect(&layout, find(dom, selector));
        assert!(near(cell.origin.x, x), "{cell:?}");
        assert!(near(cell.size.width, 100.0), "{cell:?}");
    }
}

#[test]
fn a_fixed_layout_table_reserves_a_border_spacing_at_each_table_edge() {
    // §17.5.2.1: the fixed algorithm divides "the remaining horizontal table
    // space (minus borders or cell spacing)", and §17.6.1 says the spacing
    // precedes every column *and* the table's own edges, so a two-column table
    // spends three of them and not two. The auto-layout counterpart of this is
    // `border_spacing_separates_every_column_and_the_table_edges`; fixed layout
    // is where data-heavy pages live and it is the branch that had no coverage.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><table id='t'>\
           <tr><td id='a' style='width:40px'>a</td><td id='b'>b</td></tr>\
         </table></body>",
        &format!("{TABLE_CSS} table {{ width: 200px; table-layout: fixed; border-spacing: 7px }}"),
        900.0,
    );
    let dom = &output.dom;
    let a = rect(&layout, find(dom, "#a"));
    let b = rect(&layout, find(dom, "#b"));
    // One spacing still precedes the first column and still follows the last.
    assert!(near(a.origin.x, 7.0), "{a:?}");
    assert!(near(b.origin.x, 54.0), "{b:?}");
    assert!(near(a.size.width, 40.0), "{a:?}");
    // 200 - 3 * 7 = 179 is what the columns divide, so the auto column takes
    // 179 - 40. Charging two spacings instead would hand it 186 - 40.
    assert!(near(b.size.width, 139.0), "{b:?}");
    assert!(near(b.right(), 200.0 - 7.0), "{b:?}");
    assert!(near(rect(&layout, find(dom, "#t")).size.width, 200.0));
}

#[test]
fn a_forced_break_does_not_inflate_a_shrink_to_fit_block() {
    // The same helper sizes a shrink-to-fit block (CSS 2.1 §10.3.5), so an
    // absolutely positioned box with only `left` set is as wide as its widest
    // line.
    let (output, _, layout) = pipeline(
        "<!doctype html><body>\
           <div id='box' style='position:absolute;left:0'>aaaaaaaaaa<br>bb</div>\
         </body>",
        "html, body { display: block; margin: 0; padding: 0 } div { display: block; margin: 0 }",
        900.0,
    );
    let dom = &output.dom;
    let box_rect = rect(&layout, find(dom, "#box"));
    assert!(near(box_rect.size.width, 80.0), "{box_rect:?}");
    // The two lines still stack, so the box is two lines tall.
    assert!(near(box_rect.size.height, 38.4), "{box_rect:?}");
}

#[test]
fn a_forced_break_does_not_inflate_an_inline_block_or_a_flex_item() {
    // The same helper sizes an inline-block's shrink-to-fit width and a flex
    // item's `flex-basis: content`.
    let (output, _, layout) = pipeline(
        "<!doctype html><body>\
           <span id='tile'>aaaaaaaaaa<br>bb</span>\
           <div id='row'><div id='item'>aaaaaaaaaa<br>bb</div></div>\
         </body>",
        "html, body { display: block; margin: 0; padding: 0 } \
         #tile { display: inline-block; margin: 0; padding: 0 } \
         #row { display: flex; margin: 0; padding: 0 } #item { display: block; margin: 0 }",
        900.0,
    );
    let dom = &output.dom;
    assert!(near(rect(&layout, find(dom, "#tile")).size.width, 80.0));
    assert!(near(rect(&layout, find(dom, "#item")).size.width, 80.0));
}

#[test]
fn a_positioned_descendant_resolves_against_the_grown_table_width() {
    // §17.5.2.2: the table grows to fit its columns, and the containing block of
    // its positioned descendants is that grown box, not the width it asked for.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><table id='t' style='width:50px;position:relative'>\
           <tr><td id='a'>aaaaaaaaaa</td>\
             <td id='host'><span id='abs' style='position:absolute;left:0;right:0'>x</span></td></tr>\
         </table></body>",
        TABLE_CSS,
        900.0,
    );
    let dom = &output.dom;
    let table = rect(&layout, find(dom, "#t"));
    let absolute = rect(&layout, find(dom, "#abs"));
    // The columns need 80 + 8, so the table grows from 50 to 88 and the absolute
    // box spans the grown width. Before the growth reached the containing rect it
    // was laid out against the 50px that was asked for.
    assert!(near(table.size.width, 88.0), "{table:?}");
    assert!(
        near(absolute.origin.x, table.origin.x),
        "{absolute:?} {table:?}"
    );
    assert!(near(absolute.size.width, 88.0), "{absolute:?} {table:?}");
}

#[test]
fn collapsed_borders_leave_the_shared_line_to_one_cell() {
    // §17.6.2: two cells meeting on a grid line collapse into one border, and
    // the wider one wins. `border-collapse` is stated because the user-agent
    // default of `separate` would leave both borders in place.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><table id='t' style='border-collapse:collapse'>\
           <tr><td id='a' style='border-right-width:3px;border-right-style:solid'>a</td>\
             <td id='b' style='border-left-width:1px;border-left-style:solid'>b</td></tr>\
         </table></body>",
        TABLE_CSS,
        900.0,
    );
    let dom = &output.dom;
    let a = rect(&layout, find(dom, "#a"));
    let b = rect(&layout, find(dom, "#b"));
    // The winner keeps the whole 3px and the loser collapses to nothing, so the
    // line between them is 3px wide and not 4px. The cells start flush, because
    // the winning cell's border is inside its own border box.
    assert_eq!(border_of(&layout, find(dom, "#a")).right, 3.0);
    assert_eq!(border_of(&layout, find(dom, "#b")).left, 0.0);
    assert!(near(b.origin.x, a.right()), "{a:?} {b:?}");
    assert!(near(a.size.width, 11.0), "{a:?}");
    // 8 of text plus the 3px shared border, then 8 more.
    let table = rect(&layout, find(dom, "#t"));
    assert!(near(table.size.width, 19.0), "{table:?}");
}

#[test]
fn a_hidden_border_wins_the_conflict_and_takes_no_space() {
    // §17.6.2: `hidden` overrides a wider neighbour, and its own used width is
    // zero.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><table id='t' style='border-collapse:collapse'>\
           <tr><td id='a' style='border-right-width:5px;border-right-style:solid'>a</td>\
             <td id='b' style='border-left-style:hidden;border-left-width:9px'>b</td></tr>\
         </table></body>",
        TABLE_CSS,
        900.0,
    );
    let dom = &output.dom;
    let a = rect(&layout, find(dom, "#a"));
    let b = rect(&layout, find(dom, "#b"));
    assert_eq!(border_of(&layout, find(dom, "#a")).right, 0.0);
    assert_eq!(border_of(&layout, find(dom, "#b")).left, 0.0);
    assert!(near(b.origin.x, a.right()), "{a:?} {b:?}");
}

#[test]
fn the_table_border_wins_the_outer_grid_lines() {
    // §17.6.2: the table's own border beats the cells on the outside, so the
    // outer cells keep nothing there and the table's border occupies the space.
    let (output, _, layout) = pipeline(
        "<!doctype html><body>\
           <table id='t' style='border-collapse:collapse;border-top-width:4px;\
             border-top-style:solid'>\
           <tr><td id='a' style='border-top-width:2px;border-top-style:solid'>a</td></tr>\
         </table></body>",
        TABLE_CSS,
        900.0,
    );
    let dom = &output.dom;
    let table = rect(&layout, find(dom, "#t"));
    let a = rect(&layout, find(dom, "#a"));
    assert_eq!(border_of(&layout, find(dom, "#t")).top, 4.0);
    assert_eq!(border_of(&layout, find(dom, "#a")).top, 0.0);
    // The cell starts below the table's border, not below its own.
    assert!(near(a.origin.y, table.origin.y + 4.0), "{a:?} {table:?}");
}

#[test]
fn the_wider_border_wins_a_shared_line_even_when_it_is_the_second_claim() {
    // §17.6.2.1: "narrow borders are discarded in favor of wider ones". The
    // wider claim is deliberately the *second* one collected, so a
    // first-claim-wins resolution would keep the 1px and this would fail.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><table id='t' style='border-collapse:collapse'>\
           <tr><td id='a' style='border-right-width:1px;border-right-style:solid'>a</td>\
             <td id='b' style='border-left-width:3px;border-left-style:solid'>b</td></tr>\
         </table></body>",
        TABLE_CSS,
        900.0,
    );
    let dom = &output.dom;
    let a = rect(&layout, find(dom, "#a"));
    let b = rect(&layout, find(dom, "#b"));
    assert_eq!(border_of(&layout, find(dom, "#a")).right, 0.0);
    assert_eq!(border_of(&layout, find(dom, "#b")).left, 3.0);
    // The surviving border is 3px wide on the shared line, not 1px + 3px, and
    // the loser is flush against the winner.
    assert!(near(b.origin.x, a.right()), "{a:?} {b:?}");
}

#[test]
fn equal_widths_are_decided_by_the_border_style_precedence_order() {
    // §17.6.2.1: "If several have the same 'border-width' then styles are
    // preferred in this order: 'double', 'solid', 'dashed', 'dotted', ..."
    // `dashed` outranks `dotted` and `solid` outranks `dashed`, and the losing
    // side is whichever happens to be collected first, so both directions of
    // the order are exercised here.
    for (left_style, right_style, winner) in [
        // `a` is first and declares the weaker style.
        ("dotted", "dashed", "#b"),
        ("dashed", "dotted", "#a"),
        ("dashed", "solid", "#b"),
        ("solid", "dashed", "#a"),
        ("dotted", "double", "#b"),
    ] {
        let (output, _, layout) = pipeline(
            &format!(
                "<!doctype html><body><table id='t' style='border-collapse:collapse'>\
                   <tr><td id='a' style='border-right-width:2px;border-right-style:{left_style}'>a</td>\
                     <td id='b' style='border-left-width:2px;border-left-style:{right_style}'>b</td></tr>\
                 </table></body>"
            ),
            TABLE_CSS,
            900.0,
        );
        let dom = &output.dom;
        let a_border = border_of(&layout, find(dom, "#a")).right;
        let b_border = border_of(&layout, find(dom, "#b")).left;
        if winner == "#a" {
            assert_eq!(a_border, 2.0, "{left_style} vs {right_style}");
            assert_eq!(b_border, 0.0, "{left_style} vs {right_style}");
        } else {
            assert_eq!(b_border, 2.0, "{left_style} vs {right_style}");
            assert_eq!(a_border, 0.0, "{left_style} vs {right_style}");
        }
    }
}

#[test]
fn border_width_is_compared_before_border_style() {
    // §17.6.2.1 reads the two criteria in order: width first, style only among
    // equal widths. `double` is the highest-ranked style, so a resolution that
    // compared the style first would hand the line to the 1px `double` and give
    // the 3px `dotted` nothing.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><table id='t' style='border-collapse:collapse'>\
           <tr><td id='a' style='border-right-width:1px;border-right-style:double'>a</td>\
             <td id='b' style='border-left-width:3px;border-left-style:dotted'>b</td></tr>\
         </table></body>",
        TABLE_CSS,
        900.0,
    );
    let dom = &output.dom;
    assert_eq!(border_of(&layout, find(dom, "#a")).right, 0.0);
    assert_eq!(border_of(&layout, find(dom, "#b")).left, 3.0);
    assert!(near(
        rect(&layout, find(dom, "#b")).origin.x,
        rect(&layout, find(dom, "#a")).right()
    ));
}

#[test]
fn a_hidden_border_still_wins_when_the_first_claim_alone_would_have_won() {
    // The `hidden` trump has to beat the width rule as well as document order:
    // here the first claim is 6px and the second is a `hidden`, so a resolution
    // that only compared widths would leave the 6px in place.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><table id='t' style='border-collapse:collapse'>\
           <tr><td id='a' style='border-right-width:6px;border-right-style:solid'>a</td>\
             <td id='b' style='border-left-style:hidden'>b</td></tr>\
         </table></body>",
        TABLE_CSS,
        900.0,
    );
    let dom = &output.dom;
    let a = rect(&layout, find(dom, "#a"));
    let b = rect(&layout, find(dom, "#b"));
    assert_eq!(border_of(&layout, find(dom, "#a")).right, 0.0);
    assert_eq!(border_of(&layout, find(dom, "#b")).left, 0.0);
    assert!(near(b.origin.x, a.right()), "{a:?} {b:?}");
}

#[test]
fn collapsed_borders_remove_the_border_spacing() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><table id='t' style='width:200px;\
           border-collapse:collapse;border-spacing:7px'>\
           <tr><td id='a'>a</td><td id='b'>b</td></tr>\
         </table></body>",
        TABLE_CSS,
        900.0,
    );
    let dom = &output.dom;
    let a = rect(&layout, find(dom, "#a"));
    let b = rect(&layout, find(dom, "#b"));
    assert!(near(a.origin.x, 0.0), "{a:?}");
    assert!(near(b.origin.x, a.right()), "{a:?} {b:?}");
    assert!(near(rect(&layout, find(dom, "#t")).size.width, 200.0));
}

#[test]
fn empty_cells_hide_collapses_the_border_and_padding_of_an_empty_cell() {
    // §17.5.2.1: with `empty-cells: hide` an empty cell keeps neither border
    // nor padding, so its content is not inset.
    let html = "<!doctype html><body><table id='t'>\
         <tr><td id='empty' style='padding-left:6px;border-left-width:2px;\
           border-left-style:solid'></td><td id='full'>x</td></tr></table></body>";
    let (output, _, layout) = pipeline(
        html,
        &format!("{TABLE_CSS} table {{ empty-cells: hide; width: 200px }}"),
        900.0,
    );
    let dom = &output.dom;
    let empty = rect(&layout, find(dom, "#empty"));
    let full = rect(&layout, find(dom, "#full"));
    let hidden = geometry_of(&layout, find(dom, "#empty"));
    assert_eq!(hidden.border, EdgeSizes::default());
    assert_eq!(hidden.padding, EdgeSizes::default());
    // The cell contributes no content and no edges, so its column collapses and
    // the content of the other cell is not inset.
    assert!(near(empty.size.width, 0.0), "{empty:?}");
    assert!(near(full.origin.x, empty.right()), "{empty:?} {full:?}");
    let (shown, _, shown_layout) = pipeline(
        html,
        &format!("{TABLE_CSS} table {{ empty-cells: show; width: 200px }}"),
        900.0,
    );
    let shown_empty = geometry_of(&shown_layout, find(&shown.dom, "#empty"));
    // With `show` the same cell keeps the 2px border and the 6px padding.
    assert_eq!(shown_empty.border.left, 2.0);
    assert_eq!(shown_empty.padding.left, 6.0);
}

#[test]
fn a_row_group_aligns_its_rows_in_a_taller_table() {
    // §17.5.3: a table taller than its rows has the extra space to place, and a
    // row group's `vertical-align` decides where its own rows sit inside it.
    let html = "<!doctype html><body><table id='t' style='height:96px'>\
         <tbody id='g'><tr id='r'><td id='a'>x</td></tr></tbody></table></body>";
    let (output, _, layout) = pipeline(html, TABLE_CSS, 900.0);
    let dom = &output.dom;
    let table = rect(&layout, find(dom, "#t"));
    let group = rect(&layout, find(dom, "#g"));
    let row = rect(&layout, find(dom, "#r"));
    // The row is 19.2 tall and the table 96, so there are 76.8px to place.
    assert!(near(table.size.height, 96.0), "{table:?}");
    assert!(near(group.size.height, 96.0), "{group:?}");
    // Without a `vertical-align` the group keeps its rows at the top.
    assert!(near(row.origin.y, 0.0), "{row:?} {group:?}");

    let (_, _, bottom) = pipeline(
        html,
        &format!("{TABLE_CSS} tbody {{ vertical-align: bottom }}"),
        900.0,
    );
    let dom = &output.dom;
    let row = rect(&bottom, find(dom, "#r"));
    assert!(near(row.origin.y, 96.0 - 19.2), "{row:?}");

    let (_, _, middle) = pipeline(
        html,
        &format!("{TABLE_CSS} tbody {{ vertical-align: middle }}"),
        900.0,
    );
    let dom = &output.dom;
    let row = rect(&middle, find(dom, "#r"));
    assert!(near(row.origin.y, (96.0 - 19.2) / 2.0), "{row:?}");
}

/// The companion to `a_row_group_without_its_own_alignment_follows_the_table`
/// and `a_row_group_aligns_its_rows_in_a_taller_table`: a group that *does*
/// set `vertical-align` must keep its own alignment even when the table sets a
/// conflicting one.
///
/// The row-group test is green for the right reason only if the solver asks
/// "did the document set this on this element?" rather than "is there a
/// computed value?". Since `vertical-align` is registered, every element has a
/// computed value, so a presence test would make *every* group look
/// self-specified and the fallback would become dead code. Pinning the
/// override direction catches a fix that simply dropped the fallback.
#[test]
fn a_row_groups_own_alignment_beats_the_tables() {
    let html = "<!doctype html><body><table id='t' style='height:96px;\
         vertical-align:bottom'><tbody id='g'><tr id='r'><td id='a'>x</td></tr>\
         </tbody></table></body>";
    // The table wants its lone group at the bottom; the group asks for the top.
    let (output, _, layout) = pipeline(
        html,
        &format!("{TABLE_CSS} tbody {{ vertical-align: top }}"),
        900.0,
    );
    let dom = &output.dom;
    let table = rect(&layout, find(dom, "#t"));
    let row = rect(&layout, find(dom, "#r"));
    assert!(near(table.size.height, 96.0), "{table:?}");
    assert!(
        near(row.origin.y, 0.0),
        "a group that specified top must not be moved to the bottom \
         because the table said bottom: {row:?}"
    );

    // And with no `tbody` rule at all, the table's `bottom` applies, which is
    // the fallback the other test covers. Both directions together mean the
    // distinction is real rather than an unconditional preference.
    let (output, _, fallback) = pipeline(html, TABLE_CSS, 900.0);
    let dom = &output.dom;
    let row = rect(&fallback, find(dom, "#r"));
    assert!(near(row.origin.y, 96.0 - 19.2), "{row:?}");
}

#[test]
fn a_row_group_without_its_own_alignment_follows_the_table() {
    // §17.5.3: the table's `vertical-align` is what positions the row groups
    // inside a table that is taller than they are. The parser gives a bare
    // `<tr>` a `<tbody>`, so that is the group that has to follow.
    let html = "<!doctype html><body><table id='t' style='height:60px;\
         vertical-align:bottom'><tr id='r'><td id='a'>x</td></tr></table></body>";
    let (output, _, layout) = pipeline(html, TABLE_CSS, 900.0);
    let dom = &output.dom;
    let group = rect(&layout, find(dom, "tbody"));
    let row = rect(&layout, find(dom, "#r"));
    assert!(near(group.size.height, 60.0), "{group:?}");
    assert!(near(row.origin.y, 60.0 - 19.2), "{row:?} {group:?}");
}

#[test]
fn thead_tbody_and_tfoot_stack_in_order_with_a_colspan_in_the_header() {
    // The shape a real page uses: three row groups, and a header cell that
    // spans two of the body columns.
    let html = "<!doctype html><body><table id='t' style='width:150px'>\
         <thead id='head'><tr id='hrow'>\
           <th id='h1' style='width:50px'>Rank</th><th id='h2' colspan='2'>Title</th>\
         </tr></thead>\
         <tbody id='body'><tr id='brow'>\
           <td id='b1' style='width:50px'>1.</td>\
           <td id='b2' style='width:60px'>A story</td>\
           <td id='b3' style='width:40px'>9</td>\
         </tr></tbody>\
         <tfoot id='foot'><tr id='frow'>\
           <td id='f1' style='width:50px'>Rank</td><td id='f2' colspan='2'>Title</td>\
         </tr></tfoot></table></body>";
    let (output, _, layout) = pipeline(html, TABLE_CSS, 900.0);
    let dom = &output.dom;
    let table = rect(&layout, find(dom, "#t"));

    // The groups stack in document order, and `tfoot` comes after the body.
    let head = rect(&layout, find(dom, "#head"));
    let body = rect(&layout, find(dom, "#body"));
    let foot = rect(&layout, find(dom, "#foot"));
    assert!(near(head.origin.y, 0.0), "{head:?}");
    assert!(near(body.origin.y, head.bottom()), "{body:?} {head:?}");
    assert!(near(foot.origin.y, body.bottom()), "{foot:?} {body:?}");
    assert!(near(table.size.height, 57.6), "{table:?}");
    // Every group is exactly as tall as its own row, so they do not collapse
    // into one another.
    assert!(near(head.size.height, 19.2), "{head:?}");
    assert!(near(body.size.height, 19.2), "{body:?}");
    assert!(near(foot.size.height, 19.2), "{foot:?}");

    // The header's colspan lands on the two body columns beneath it, and the
    // footer's does too.
    let h2 = rect(&layout, find(dom, "#h2"));
    let b2 = rect(&layout, find(dom, "#b2"));
    let b3 = rect(&layout, find(dom, "#b3"));
    let f2 = rect(&layout, find(dom, "#f2"));
    assert!(near(h2.origin.x, b2.origin.x), "{h2:?} {b2:?}");
    assert!(near(h2.size.width, b2.size.width + b3.size.width), "{h2:?}");
    assert!(near(h2.right(), b3.right()), "{h2:?} {b3:?}");
    assert!(near(f2.origin.x, b2.origin.x), "{f2:?} {b2:?}");
    assert!(near(f2.size.width, h2.size.width), "{f2:?} {h2:?}");

    // The single-cell rows span the same three columns as the body.
    let h1 = rect(&layout, find(dom, "#h1"));
    let b1 = rect(&layout, find(dom, "#b1"));
    assert!(near(h1.origin.x, b1.origin.x), "{h1:?} {b1:?}");
    assert!(near(h1.size.width, b1.size.width), "{h1:?} {b1:?}");
}

#[test]
fn row_groups_of_differing_heights_do_not_collapse() {
    let html = "<!doctype html><body><table id='t'>\
         <thead id='head'><tr id='hrow'><th id='h1'>head</th></tr></thead>\
         <tbody id='body'><tr id='brow'>\
           <td id='b1'>one<br>two<br>three</td></tr></tbody>\
         <tfoot id='foot'><tr id='frow'><td id='f1'>foot</td></tr></tfoot></table></body>";
    let (output, _, layout) = pipeline(html, TABLE_CSS, 900.0);
    let dom = &output.dom;
    let head = rect(&layout, find(dom, "#head"));
    let body = rect(&layout, find(dom, "#body"));
    let foot = rect(&layout, find(dom, "#foot"));
    let table = rect(&layout, find(dom, "#t"));
    assert!(near(head.size.height, 19.2), "{head:?}");
    assert!(near(body.size.height, 57.6), "{body:?}");
    assert!(near(foot.size.height, 19.2), "{foot:?}");
    assert!(near(body.origin.y, 19.2), "{body:?}");
    assert!(near(foot.origin.y, 76.8), "{foot:?}");
    assert!(near(table.size.height, 96.0), "{table:?}");
}

#[test]
fn collapsed_borders_and_empty_cells_compose_with_several_row_groups() {
    let html = "<!doctype html><body><table id='t' style='width:120px;\
         border-collapse:collapse;empty-cells:hide'>\
         <thead id='head'><tr id='hrow'>\
           <th id='h1' style='width:60px;border-bottom-width:2px;\
             border-bottom-style:solid'>head</th>\
           <th id='h2' style='border-top-width:1px;border-top-style:solid'>b</th>\
         </tr></thead>\
         <tbody id='body'><tr id='brow'>\
           <td id='b1' style='width:60px;border-bottom-width:2px;\
             border-bottom-style:solid'>a</td><td id='b2'></td></tr></tbody>\
         </table></body>";
    let (output, _, layout) = pipeline(html, TABLE_CSS, 900.0);
    let dom = &output.dom;
    let table = rect(&layout, find(dom, "#t"));
    // `border-collapse: collapse` still wins over the border spacing, and the
    // groups still stack.
    let head = rect(&layout, find(dom, "#head"));
    let body = rect(&layout, find(dom, "#body"));
    assert!(near(body.origin.y, head.bottom()), "{body:?} {head:?}");
    assert!(near(table.size.width, 120.0), "{table:?}");
    // The 2px bottom border of the header cell beats the 1px top border of the
    // cell below it, so the header keeps 2 and the body cell keeps nothing.
    assert_eq!(border_of(&layout, find(dom, "#h1")).bottom, 2.0);
    assert_eq!(border_of(&layout, find(dom, "#b1")).top, 0.0);
    // §17.5.2.1: `empty-cells` only applies to the separated border model, so in
    // a collapsed table the empty cell keeps its edges and its column. The
    // composition is that `empty-cells` is ignored, not that it half-applies.
    let b2 = geometry_of(&layout, find(dom, "#b2"));
    assert_eq!(b2.border, EdgeSizes::default());
    assert_eq!(b2.padding, EdgeSizes::default());
    assert!(near(rect(&layout, find(dom, "#b2")).size.width, 60.0));
    assert!(near(rect(&layout, find(dom, "#h2")).size.width, 60.0));
}

#[test]
fn a_fixed_layout_table_can_be_sized_by_column_boxes_alone() {
    // §17.5.2.1: with no width on any cell, the column boxes are the only
    // source, and the columns with no width of their own divide the rest.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><table id='t' style='width:200px;table-layout:fixed'>\
           <colgroup><col id='c1' style='width:50px'><col id='c2'></colgroup>\
           <tr><td id='a'>averylongwordthatcannotfit</td><td id='b'>b</td></tr>\
         </table></body>",
        TABLE_CSS,
        900.0,
    );
    let dom = &output.dom;
    let a = rect(&layout, find(dom, "#a"));
    let b = rect(&layout, find(dom, "#b"));
    assert!(near(a.size.width, 50.0), "{a:?}");
    assert!(near(b.origin.x, 50.0), "{b:?}");
    assert!(near(b.size.width, 150.0), "{b:?}");
    // The long word is not measured, so it overflows its column instead of
    // widening it. UAX #14 rule LB28 disallows a break between two Latin
    // letters, so the run has no opportunity inside it and CSS 2.1 §9.4.2 says
    // it "overflows the line box": one line, not several.
    assert!(near(rect(&layout, find(dom, "#a")).size.height, 19.2));
}

#[test]
fn vertical_align_positions_a_spanning_cell_in_its_spanned_rows() {
    // §17.5.3: a cell that spans rows is aligned inside the whole area it
    // covers, not inside its own height.
    let html = "<!doctype html><body><table id='t'>\
         <tr id='r1'><td id='a'>one<br>two</td>\
           <td id='sp' rowspan='2'>s</td></tr>\
         <tr id='r2'><td id='b'>b</td></tr></table></body>";
    let (bottom_dom, _, bottom_layout) = pipeline(
        html,
        &format!("{TABLE_CSS} #sp {{ vertical-align: bottom }}"),
        900.0,
    );
    // Two rows of 38.4 and 19.2, so the spanner is 57.6 tall.
    let spanner = rect(&bottom_layout, find(&bottom_dom.dom, "#sp"));
    assert!(near(spanner.size.height, 57.6), "{spanner:?}");
    // `bottom` puts its line at the bottom of that area.
    assert!(
        near(text_rect(&bottom_layout, "s").origin.y, 38.4),
        "{spanner:?}"
    );

    let (_, _, top) = pipeline(
        html,
        &format!("{TABLE_CSS} #sp {{ vertical-align: top }}"),
        900.0,
    );
    assert!(near(text_rect(&top, "s").origin.y, 0.0), "{spanner:?}");

    let (_, _, middle) = pipeline(
        html,
        &format!("{TABLE_CSS} #sp {{ vertical-align: middle }}"),
        900.0,
    );
    assert!(near(text_rect(&middle, "s").origin.y, 19.2), "{spanner:?}");
}

#[test]
fn an_over_constrained_table_grows_instead_of_squeezing_its_columns() {
    // §17.5.2.2: a table whose columns need more room than its used width is
    // increased to that sum. Squeezing the columns instead would re-wrap every
    // cell's text, which is visible.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><table id='t'>\
           <tr><td id='a'>aaaaaaaaaa</td><td id='b'>bbbbbbbbbb</td></tr>\
         </table></body>",
        &format!("{TABLE_CSS} table {{ width: 100px; border-spacing: 0 }}"),
        900.0,
    );
    let dom = &output.dom;
    let table = rect(&layout, find(dom, "#t"));
    let a = rect(&layout, find(dom, "#a"));
    let b = rect(&layout, find(dom, "#b"));
    // Ten characters at 8px each, twice.
    assert!(near(a.size.width, 80.0), "{a:?}");
    assert!(near(b.size.width, 80.0), "{b:?}");
    assert!(near(b.origin.x, 80.0), "{b:?}");
    assert!(near(table.size.width, 160.0), "{table:?}");
    assert!(near(b.right(), 160.0), "{b:?}");
}

#[test]
fn an_auto_width_table_shrink_to_fits_its_content() {
    // §17.5.2.2: `width: auto` is the greater of the min-content and max-content
    // widths, never more than the containing block, so a narrow table stays
    // narrow inside a wide block.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='card'>\
           <table id='t'><tr><td id='a'>ab</td><td id='b'>cd</td></tr></table>\
         </div></body>",
        &format!("{TABLE_CSS} div {{ display: block }} table {{ border-spacing: 0 }}"),
        1200.0,
    );
    let dom = &output.dom;
    let card = rect(&layout, find(dom, "#card"));
    let table = rect(&layout, find(dom, "#t"));
    let a = rect(&layout, find(dom, "#a"));
    let b = rect(&layout, find(dom, "#b"));
    assert!(near(card.size.width, 1200.0), "{card:?}");
    // "ab" and "cd" are 16px each, so the table is 32px wide, not 1200px.
    assert!(near(table.size.width, 32.0), "{table:?}");
    assert!(near(a.size.width, 16.0), "{a:?}");
    assert!(near(b.origin.x, 16.0), "{b:?}");
    assert!(near(b.right(), 32.0), "{b:?}");
}

#[test]
fn a_cell_min_width_can_exceed_the_tables_max_content_width() {
    // §17.5.2.2: the used width is the greater of the min-content and
    // max-content widths, so a `min-width` floor on a cell raises the table even
    // when its content would fit in less.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='card'>\
           <table id='t'><tr><td id='a' style='min-width:500px'>ab</td></tr></table>\
         </div></body>",
        &format!("{TABLE_CSS} div {{ display: block }} table {{ border-spacing: 0 }}"),
        1200.0,
    );
    let dom = &output.dom;
    let table = rect(&layout, find(dom, "#t"));
    let a = rect(&layout, find(dom, "#a"));
    // The max-content width is 16px and the min-content width is 500px.
    assert!(near(table.size.width, 500.0), "{table:?}");
    assert!(near(a.size.width, 500.0), "{a:?}");
}

#[test]
fn border_spacing_separates_every_column_and_the_table_edges() {
    // §17.6.1: one horizontal spacing precedes every column and the table.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><table id='t'>\
           <tr><td id='a' style='width:40px'>a</td><td id='b'>b</td></tr>\
         </table></body>",
        &format!("{TABLE_CSS} table {{ width: 200px; border-spacing: 7px }}"),
        900.0,
    );
    let dom = &output.dom;
    let a = rect(&layout, find(dom, "#a"));
    let b = rect(&layout, find(dom, "#b"));
    assert!(near(a.origin.x, 7.0), "{a:?}");
    assert!(near(a.size.width, 40.0), "{a:?}");
    assert!(near(b.origin.x, 54.0), "{b:?}");
    // 200 - 3 * 7 = 179 left for the columns.
    assert!(near(b.size.width, 139.0), "{b:?}");
    assert!(near(b.right(), 200.0 - 7.0), "{b:?}");
}

#[test]
fn collapsed_table_borders_share_the_space_instead_of_spacing_it() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><table id='t'>\
           <tr><td id='a'>a</td><td id='b'>b</td></tr>\
         </table></body>",
        &format!(
            "{TABLE_CSS} table {{ width: 200px; border-spacing: 7px; border-collapse: collapse }}"
        ),
        900.0,
    );
    let dom = &output.dom;
    let a = rect(&layout, find(dom, "#a"));
    let b = rect(&layout, find(dom, "#b"));
    assert!(near(a.origin.x, 0.0), "{a:?}");
    assert!(near(a.size.width, 100.0), "{a:?}");
    assert!(near(b.origin.x, 100.0), "{b:?}");
}

#[test]
fn cells_share_the_row_height_and_align_their_content_to_the_row_baseline() {
    // §17.5.3: the row is as tall as its tallest cell, and a cell with no
    // `vertical-align` of its own is baseline-aligned, so the short cell's line
    // sits on the tall cell's first line rather than in the middle of the row.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><table id='t'><tr id='r'>\
           <td id='tall'>one<br>two<br>three</td><td id='short'>x</td>\
         </tr></table></body>",
        TABLE_CSS,
        900.0,
    );
    let dom = &output.dom;
    let row = rect(&layout, find(dom, "#r"));
    let tall = rect(&layout, find(dom, "#tall"));
    let short = rect(&layout, find(dom, "#short"));
    assert!(near(row.size.height, 57.6), "{row:?}");
    assert!(near(tall.size.height, 57.6), "{tall:?}");
    assert!(near(short.size.height, 57.6), "{short:?}");
    let first = text_rect(&layout, "one");
    let aligned = text_rect(&layout, "x");
    assert!(
        near(aligned.origin.y, first.origin.y),
        "{aligned:?} {first:?}"
    );
}

#[test]
fn vertical_align_middle_has_to_be_asked_for() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><table id='t'><tr id='r'>\
           <td id='tall'>one<br>two<br>three</td><td id='middle'>x</td>\
         </tr></table></body>",
        &format!("{TABLE_CSS} #middle {{ vertical-align: middle }}"),
        900.0,
    );
    let dom = &output.dom;
    let row = rect(&layout, find(dom, "#r"));
    assert!(near(row.size.height, 57.6), "{row:?}");
    assert!(near(text_rect(&layout, "x").origin.y, 19.2), "{row:?}");
}

#[test]
fn baseline_alignment_uses_each_cells_own_first_line() {
    // §17.5.3: the row's baseline is the first cell that has one, and a
    // baseline-aligned cell puts its own first line on it. The padded cell
    // starts lower, so the plain cell has to move down to meet it.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><table id='t'><tr id='r'>\
           <td id='padded' style='padding-top:20px'>one<br>two</td>\
           <td id='plain'>x</td>\
           <td id='empty'><div id='empty-content' style='height:10px'></div></td>\
         </tr></table></body>",
        TABLE_CSS,
        900.0,
    );
    let dom = &output.dom;
    let row = rect(&layout, find(dom, "#r"));
    // 20px of padding plus two lines.
    assert!(near(row.size.height, 58.4), "{row:?}");
    let padded = text_rect(&layout, "one");
    let plain = text_rect(&layout, "x");
    assert!(near(padded.origin.y, 20.0), "{padded:?} {row:?}");
    // The plain cell is baseline-aligned, so its line lands on the padded
    // cell's first line instead of staying at the top of the row.
    assert!(
        near(plain.origin.y, padded.origin.y),
        "{plain:?} {padded:?}"
    );
    // A cell with no in-flow line box has no baseline, so its bottom margin
    // edge is aligned with the row's baseline instead.
    let empty = rect(&layout, find(dom, "#empty-content"));
    assert!(
        near(empty.bottom(), row.origin.y + padded.origin.y + 12.8),
        "{empty:?} {row:?} {padded:?}"
    );
}

#[test]
fn vertical_align_positions_cell_content_at_top_and_bottom() {
    let html = "<!doctype html><body><table id='t'><tr id='r'>\
         <td id='tall'>one<br>two<br>three</td><td id='top'>top</td>\
         <td id='bottom'>bottom</td><td id='baseline'>baseline</td>\
       </tr></table></body>";
    let (output, _, layout) = pipeline(
        html,
        &format!(
            "{TABLE_CSS} #top {{ vertical-align: top }} \
                  #bottom {{ vertical-align: bottom }} #baseline {{ vertical-align: baseline }}"
        ),
        900.0,
    );
    let dom = &output.dom;
    let row = rect(&layout, find(dom, "#r"));
    assert!(near(row.size.height, 57.6), "{row:?}");
    assert!(near(text_rect(&layout, "top").origin.y, 0.0), "{row:?}");
    assert!(
        near(text_rect(&layout, "bottom").bottom(), row.bottom()),
        "{row:?} {:?}",
        text_rect(&layout, "bottom")
    );
    // `baseline` puts the short cell's first line on the first cell's first
    // line, so it is aligned with `top`, not with `bottom`.
    let first = text_rect(&layout, "one");
    let aligned = text_rect(&layout, "baseline");
    assert!(
        near(aligned.origin.y, first.origin.y),
        "{aligned:?} {first:?}"
    );
}

#[test]
fn cell_padding_and_border_reserve_column_width() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><table id='t'>\
           <tr><td id='a' style='padding-left:5px;padding-right:5px;\
             border-left-width:2px;border-left-style:solid'>a</td>\
             <td id='b' style='width:180px'>b</td></tr></table></body>",
        &format!("{TABLE_CSS} table {{ width: 200px }}"),
        900.0,
    );
    let dom = &output.dom;
    // "a" is 8px of text plus 10px of padding plus a 2px border, and the fixed
    // column beside it leaves exactly that much free space.
    assert!(near(rect(&layout, find(dom, "#a")).size.width, 20.0));
    assert!(near(rect(&layout, find(dom, "#b")).origin.x, 20.0));
    assert!(near(rect(&layout, find(dom, "#b")).size.width, 180.0));
}

#[test]
fn colspan_lays_one_cell_over_several_columns() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><table id='t'><tr id='r'>\
           <td id='a' style='width:50px'>a</td><td id='b' style='width:60px'>b</td>\
           <td id='wide' colspan='2' style='width:90px'>wide</td>\
         </tr></table></body>",
        &format!("{TABLE_CSS} table {{ width: 300px }}"),
        900.0,
    );
    let dom = &output.dom;
    let a = rect(&layout, find(dom, "#a"));
    let b = rect(&layout, find(dom, "#b"));
    let wide = rect(&layout, find(dom, "#wide"));
    // Four columns: the specified widths fix the first three, and the fourth
    // only exists because the spanning cell covers it. A spanning cell shares
    // its width between its columns, so the cell keeps its 90px.
    assert!(near(a.size.width, 50.0), "{a:?}");
    assert!(near(b.size.width, 60.0), "{b:?}");
    assert!(near(wide.origin.x, 110.0), "{wide:?}");
    assert!(near(wide.size.width, 90.0), "{wide:?}");
    // §17.5.2.2: columns with a definite width are not grown to fill the
    // table, so the leftover space stays at the table's right edge.
    assert!(near(rect(&layout, find(dom, "#t")).size.width, 300.0));
    assert!(wide.right() < 300.0, "{wide:?}");
}

#[test]
fn rowspan_leaves_the_slot_empty_and_stretches_over_its_rows() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><table id='t'>\
           <tr id='r1'><td id='tall' rowspan='2'>one<br>two<br>three</td>\
             <td id='b1'>b</td></tr>\
           <tr id='r2'><td id='b2'>b</td></tr>\
         </table></body>",
        TABLE_CSS,
        900.0,
    );
    let dom = &output.dom;
    let first = rect(&layout, find(dom, "#r1"));
    let second = rect(&layout, find(dom, "#r2"));
    let tall = rect(&layout, find(dom, "#tall"));
    let b1 = rect(&layout, find(dom, "#b1"));
    let b2 = rect(&layout, find(dom, "#b2"));
    // The spanning cell occupies column 0, so the second row's only cell moves
    // into column 1 instead of colliding with it.
    assert!(near(tall.origin.x, 0.0), "{tall:?}");
    assert!(near(b1.origin.x, tall.right()), "{b1:?} {tall:?}");
    assert!(near(b2.origin.x, b1.origin.x), "{b2:?} {b1:?}");
    // The spanner keeps its own height and grows the last row it covers.
    assert!(near(tall.origin.y, 0.0), "{tall:?}");
    assert!(near(tall.size.height, 57.6), "{tall:?}");
    assert!(near(first.size.height, 19.2), "{first:?}");
    assert!(near(second.size.height, 38.4), "{second:?}");
    assert!(near(second.origin.y, 19.2), "{second:?}");
}

#[test]
fn content_that_is_not_a_cell_is_wrapped_in_an_anonymous_cell() {
    // CSS 2.1 §17.2.1: a row child that is not a cell becomes an anonymous
    // table cell, so it still occupies a column of its own.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><table id='t'><tr id='r'>\
           <td id='a' style='display:block'>block</td>\
           <td id='b' style='width:800px'>b</td>\
         </tr></table></body>",
        TABLE_CSS,
        900.0,
    );
    let dom = &output.dom;
    let table = rect(&layout, find(dom, "#t"));
    let a = rect(&layout, find(dom, "#a"));
    let b = rect(&layout, find(dom, "#b"));
    // The row has two cells: the anonymous one that wraps the block, and the
    // fixed cell beside it. The table shrink-to-fits to 40 + 800.
    assert_eq!(
        fragment(&layout, find(dom, "#r")).children.len(),
        2,
        "the block was not wrapped in an anonymous cell"
    );
    assert!(near(table.size.width, 840.0), "{table:?}");
    assert!(near(a.size.width, 40.0), "{a:?}");
    assert!(near(a.origin.x, 0.0), "{a:?}");
    assert!(near(b.origin.x, 40.0), "{b:?}");
    assert!(near(b.size.width, 800.0), "{b:?}");
    assert!(
        layout.fragments.iter().any(|candidate| {
            candidate.source.is_none()
                && near(candidate.rect.origin.x, 0.0)
                && near(candidate.rect.size.width, 40.0)
        }),
        "no anonymous cell box for the block: {a:?}"
    );
}

#[test]
fn content_that_is_not_a_row_is_wrapped_in_an_anonymous_row_and_cell() {
    // A caption that is not `display: table-caption` is not a row-level box, so
    // §17.2.1 wraps it in an anonymous row and an anonymous cell.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><table id='t'>\
           <caption id='cap' style='display:block'>loose</caption>\
           <tr id='r'><td id='a'>a</td></tr></table></body>",
        TABLE_CSS,
        900.0,
    );
    let dom = &output.dom;
    let table = rect(&layout, find(dom, "#t"));
    let cap = rect(&layout, find(dom, "#cap"));
    let row = rect(&layout, find(dom, "#r"));
    assert!(near(text_rect(&layout, "loose").origin.y, 0.0));
    assert!(near(cap.origin.y, 0.0), "{cap:?}");
    assert!(near(cap.size.height, 19.2), "{cap:?}");
    // The anonymous row pushed the real row down instead of losing it.
    assert!(near(row.origin.y, 19.2), "{row:?} {cap:?}");
    assert!(near(table.size.height, 38.4), "{table:?}");
}

#[test]
fn whitespace_only_table_content_generates_no_anonymous_box() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><table id='t'>\
           \n  <tr id='r'>\n  <td id='a'>a</td>\n  <td id='b'>b</td>\n  </tr>\n\
         </table></body>",
        TABLE_CSS,
        900.0,
    );
    let dom = &output.dom;
    // Exactly two cells: the inter-row whitespace did not become a third.
    assert_eq!(
        fragment(&layout, find(dom, "#r")).children.len(),
        2,
        "collapsible whitespace between cells became an anonymous cell"
    );
    let a = rect(&layout, find(dom, "#a"));
    let b = rect(&layout, find(dom, "#b"));
    assert!(near(a.origin.x, 0.0), "{a:?}");
    assert!(near(b.origin.x, a.right()), "{a:?} {b:?}");
    assert!(
        !layout
            .fragments
            .iter()
            .any(|fragment| matches!(&fragment.kind, FragmentKind::Text(text) if text.text.trim().is_empty() && text.text.len() > 1)),
        "collapsible whitespace between rows became a text fragment"
    );
}

#[test]
fn a_caption_sits_above_the_rows_by_default_and_below_on_request() {
    let html = "<!doctype html><body><table id='t'>\
         <caption id='cap'>Totals</caption>\
         <tr id='r'><td id='a'>a</td></tr></table></body>";
    let (output, _, layout) = pipeline(html, TABLE_CSS, 900.0);
    let dom = &output.dom;
    let table = rect(&layout, find(dom, "#t"));
    let caption = rect(&layout, find(dom, "#cap"));
    let row = rect(&layout, find(dom, "#r"));
    assert!(near(caption.origin.y, 0.0), "{caption:?}");
    assert!(near(row.origin.y, caption.bottom()), "{row:?} {caption:?}");
    assert!(near(caption.size.width, table.size.width), "{caption:?}");
    assert!(
        near(table.size.height, caption.size.height + row.size.height),
        "{table:?}"
    );

    let (_, _, bottom) = pipeline(
        html,
        &format!("{TABLE_CSS} #cap {{ caption-side: bottom }}"),
        900.0,
    );
    let dom = &output.dom;
    let caption = rect(&bottom, find(dom, "#cap"));
    let row = rect(&bottom, find(dom, "#r"));
    assert!(near(row.origin.y, 0.0), "{row:?}");
    assert!(near(caption.origin.y, row.bottom()), "{caption:?} {row:?}");
}

#[test]
fn text_align_is_inherited_into_cells() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><table id='t' style='text-align:right'>\
           <tr><td id='a' style='width:200px'>abcd</td></tr></table></body>",
        TABLE_CSS,
        900.0,
    );
    let dom = &output.dom;
    let a = rect(&layout, find(dom, "#a"));
    let text = text_rect(&layout, "abcd");
    assert!(near(text.right(), a.right()), "{a:?} {text:?}");
    assert!(near(text.size.width, 32.0), "{text:?}");
}

#[test]
fn a_nested_table_lays_out_its_own_columns_inside_the_cell() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><table id='outer' style='width:200px'><tr><td id='host'>\
           <table id='inner'><tr><td id='x'>x</td><td id='y'>y</td></tr></table>\
         </td></tr></table></body>",
        TABLE_CSS,
        900.0,
    );
    let dom = &output.dom;
    let host = rect(&layout, find(dom, "#host"));
    let inner = rect(&layout, find(dom, "#inner"));
    let x = rect(&layout, find(dom, "#x"));
    let y = rect(&layout, find(dom, "#y"));
    // The outer table is 200px wide; the inner one shrink-to-fits its own two
    // 8px columns inside the cell.
    assert!(near(host.size.width, 200.0), "{host:?}");
    assert!(near(inner.size.width, 16.0), "{inner:?}");
    assert!(near(x.size.width, 8.0), "{x:?}");
    assert!(near(y.origin.x, x.right()), "{y:?} {x:?}");
    assert!(near(y.right(), inner.right()), "{y:?} {inner:?}");
}

#[test]
fn a_nested_table_measures_its_columns_not_its_widest_row() {
    // §17.5.2.2: a nested table's max-content width is the sum of its column
    // widths, so the cell around it is never narrower than the table inside it.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><table id='outer'><tr><td id='host'>\
           <table id='inner'><tr><td id='x'>x</td><td id='y'>y</td></tr></table>\
         </td></tr></table></body>",
        TABLE_CSS,
        900.0,
    );
    let dom = &output.dom;
    let host = rect(&layout, find(dom, "#host"));
    let inner = rect(&layout, find(dom, "#inner"));
    let x = rect(&layout, find(dom, "#x"));
    let y = rect(&layout, find(dom, "#y"));
    // The outer table's only column is as wide as both of the inner columns, so
    // the cell and the table inside it are the same 16px.
    assert!(near(host.size.width, 16.0), "{host:?}");
    assert!(near(inner.size.width, 16.0), "{inner:?}");
    assert!(near(x.size.width, 8.0), "{x:?}");
    assert!(near(y.origin.x, x.right()), "{y:?} {x:?}");
    assert!(near(y.right(), inner.right()), "{y:?} {inner:?}");
}

#[test]
fn a_nested_table_with_a_wide_row_widens_the_cell_around_it() {
    // The same sum, with a column wide enough to need wrapping: the cell is as
    // wide as the table's columns, not as wide as its one visible cell.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><table id='outer'><tr><td id='host'>\
           <table id='inner'>\
             <tr><td id='wide'>aaaaaaaaaa</td><td id='y'>y</td></tr>\
           </table></td></tr></table></body>",
        TABLE_CSS,
        900.0,
    );
    let dom = &output.dom;
    let host = rect(&layout, find(dom, "#host"));
    let inner = rect(&layout, find(dom, "#inner"));
    let wide = rect(&layout, find(dom, "#wide"));
    let y = rect(&layout, find(dom, "#y"));
    // 80px of text plus the 8px column beside it.
    assert!(near(host.size.width, 88.0), "{host:?}");
    assert!(near(inner.size.width, 88.0), "{inner:?}");
    assert!(near(wide.size.width, 80.0), "{wide:?}");
    assert!(near(y.origin.x, 80.0), "{y:?} {wide:?}");
}

#[test]
fn a_table_outside_a_table_structure_box_still_lays_out_as_a_block() {
    // A stray row or cell is not a table structure box on its own; the engine
    // must not lose the content.
    //
    // The markup is a real `<table><tr><td>`, which is the only shape the HTML
    // parser keeps: HTML 13.2.6.4.7 makes a `<td>` start tag outside a cell
    // context a parse error whose token is ignored, so the old `<div><td>`
    // fixture no longer described anything the parser produces. The condition
    // is then made with `display`, which is where CSS puts it: the table is a
    // block container, so its rows and cells carry table-internal display
    // values with no table structure around them.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><table id='t'><tr id='r'><td id='a'>content</td></tr></table></body>",
        &format!("{TABLE_CSS} table {{ display: block }} tr {{ display: block }}"),
        900.0,
    );
    let dom = &output.dom;
    let cell = rect(&layout, find(dom, "#a"));
    // The content survives, and the cell sits at the left edge of its container
    // rather than being dropped or pushed into a column.
    assert!(
        near(text_rect(&layout, "content").origin.x, 0.0),
        "the text inside a stray cell must not be lost"
    );
    assert!(near(cell.origin.x, 0.0), "{cell:?}");
}

#[test]
fn zprobe_two_cell_collapse() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><table id='t' style='border-collapse:collapse'>\
           <tr><td id='a' style='border:1px solid'>a</td>\
             <td id='b' style='border:1px solid'>b</td></tr>\
         </table></body>",
        TABLE_CSS,
        900.0,
    );
    let dom = &output.dom;
    for id in ["#t", "#a", "#b"] {
        let r = rect(&layout, find(dom, id));
        let g = geometry_of(&layout, find(dom, id));
        println!(
            "{id}: rect={:?} border={:?} padding={:?} content_rect={:?}",
            r, g.border, g.padding, g.content_rect
        );
    }
    println!("text a at {:?}", text_rect(&layout, "a"));
}

#[test]
fn zprobe_table_vs_td_border() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><table id='t' style='border-collapse:collapse;\
           border-left-width:1px;border-left-style:solid'>\
           <tr><td id='a' style='border-left-width:8px;border-left-style:solid'>a</td></tr>\
         </table></body>",
        TABLE_CSS,
        900.0,
    );
    let dom = &output.dom;
    for id in ["#t", "#a"] {
        let r = rect(&layout, find(dom, id));
        let g = geometry_of(&layout, find(dom, id));
        println!("{id}: rect={:?} border={:?}", r, g.border);
    }
    println!("text a at {:?}", text_rect(&layout, "a"));
}

#[test]
fn zprobe_fixed_layout_overflow() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><table id='t' style='border-collapse:collapse;\
           width:200px;table-layout:fixed'>\
           <tr><td id='a' style='border:1px solid'>a</td>\
             <td id='b' style='border:1px solid'>b</td></tr>\
         </table></body>",
        TABLE_CSS,
        900.0,
    );
    let dom = &output.dom;
    for id in ["#t", "#a", "#b"] {
        let r = rect(&layout, find(dom, id));
        let g = geometry_of(&layout, find(dom, id));
        println!("{id}: rect={:?} border={:?}", r, g.border);
    }
}

#[test]
fn zprobe_row_border_ignored() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><table id='t' style='border-collapse:collapse'>\
           <tr id='r' style='border-left-width:8px;border-left-style:solid'>\
           <td id='a' style='border-left-width:1px;border-left-style:solid'>a</td></tr>\
         </table></body>",
        TABLE_CSS,
        900.0,
    );
    let dom = &output.dom;
    for id in ["#t", "#a"] {
        let r = rect(&layout, find(dom, id));
        let g = geometry_of(&layout, find(dom, id));
        println!("{id}: rect={:?} border={:?}", r, g.border);
    }
}

#[test]
fn zprobe_fixture() {
    let html = std::fs::read_to_string(
        r"C:\Users\Elyar\Desktop\rENDER\tests\fixtures\real_sites\spec_data_table.html",
    )
    .unwrap();
    let css = std::fs::read_to_string(
        r"C:\Users\Elyar\Desktop\rENDER\tests\fixtures\real_sites\spec_data_table.css",
    )
    .unwrap();
    let (output, _, layout) = pipeline(&html, &css, 1280.0);
    for f in layout.fragments.iter() {
        let Some(source) = f.source else { continue };
        let Some(node) = output.dom.node(source) else {
            continue;
        };
        let render_dom::NodeKind::Element(element) = node.kind() else {
            continue;
        };
        if !matches!(
            element.local_name.as_str(),
            "table" | "caption" | "thead" | "tbody" | "tfoot" | "tr" | "td" | "th" | "colgroup"
                | "main" | "article" | "aside" | "section" | "div" | "p"
        ) {
            continue;
        }
        println!(
            "{} x={:.2} y={:.2} w={:.2} h={:.2}",
            element.local_name,
            f.rect.origin.x,
            f.rect.origin.y,
            f.rect.size.width,
            f.rect.size.height
        );
    }
    println!("--- all divs ---");
    for f in layout.fragments.iter() {
        let Some(source) = f.source else { continue };
        let Some(node) = output.dom.node(source) else { continue };
        let render_dom::NodeKind::Element(element) = node.kind() else { continue };
        if element.local_name != "div" {
            continue;
        }
        println!(
            "div x={:.2} y={:.2} w={:.2} h={:.2}",
            f.rect.origin.x,
            f.rect.origin.y,
            f.rect.size.width,
            f.rect.size.height
        );
    }
}
