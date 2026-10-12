//! Regression tests for CSS Multi-column Layout Level 1: how many columns a
//! container has, where its flow lands in them, and how a block that a column
//! break crosses is split.

use render_dom::NodeId;

use crate::PhysicalRect;
use crate::solver::LayoutOutput;

use super::tests::{find, fragment, pipeline};

const RESET: &str = "html, body, div, p { display: block; margin: 0; padding: 0 }";

fn near(actual: f32, expected: f32) -> bool {
    (actual - expected).abs() < 0.01
}

fn rect(layout: &LayoutOutput, source: NodeId) -> PhysicalRect {
    fragment(layout, source).rect
}

/// Every fragment that a source node produced, in fragment order. A split box
/// has one per column it touches.
fn pieces(layout: &LayoutOutput, source: NodeId) -> Vec<PhysicalRect> {
    layout
        .fragments
        .iter()
        .filter(|fragment| fragment.source == Some(source))
        .map(|fragment| fragment.rect)
        .collect()
}

/// `count` items, each `height` tall, as the children of `#c`.
fn items(count: usize) -> String {
    use std::fmt::Write as _;
    let mut html = String::new();
    for index in 0..count {
        // Writing to a `String` cannot fail.
        let _ = write!(html, "<div id='i{index}' class='item'></div>");
    }
    html
}

#[test]
fn a_multi_column_flow_is_balanced_over_its_columns() {
    // Eight 20px items in two 190px columns with a 20px gap: four per column,
    // so each column is 80px tall and the second starts at the first's pitch.
    let (output, _, layout) = pipeline(
        &format!("<!doctype html><body><div id='c'>{}</div></body>", items(8)),
        &format!(
            "{RESET} #c {{ width: 400px; columns: 2; column-gap: 20px }} .item {{ height: 20px }}"
        ),
        800.0,
    );
    let dom = &output.dom;
    let c = rect(&layout, find(dom, "#c"));
    assert!(near(c.size.height, 80.0), "the container is {c:?}");
    let first = rect(&layout, find(dom, "#i0"));
    assert!(
        near(first.origin.x, 0.0) && near(first.origin.y, 0.0),
        "{first:?}"
    );
    let last_of_first = rect(&layout, find(dom, "#i3"));
    assert!(near(last_of_first.origin.y, 60.0), "{last_of_first:?}");
    let second_column = rect(&layout, find(dom, "#i4"));
    assert!(
        near(second_column.origin.x, 210.0) && near(second_column.origin.y, 0.0),
        "the second column starts at 190px + 20px, from its top: {second_column:?}"
    );
}

#[test]
fn an_item_that_would_cross_a_column_end_moves_whole_to_the_next_column() {
    // Three 30px items in two 100px columns with no gap. The least column height
    // is 60px, so the third item cannot share the first column and must move
    // whole to the top of the second one, rather than be cut.
    let (output, _, layout) = pipeline(
        &format!("<!doctype html><body><div id='c'>{}</div></body>", items(3)),
        &format!(
            "{RESET} #c {{ width: 200px; columns: 2; column-gap: 0 }} .item {{ height: 30px }}"
        ),
        800.0,
    );
    let dom = &output.dom;
    assert!(near(rect(&layout, find(dom, "#c")).size.height, 60.0));
    let moved = rect(&layout, find(dom, "#i2"));
    assert!(
        near(moved.origin.x, 100.0) && near(moved.origin.y, 0.0),
        "the item that crossed the break is at {moved:?}"
    );
    assert!(near(rect(&layout, find(dom, "#i1")).origin.y, 30.0));
}

#[test]
fn a_block_that_a_column_break_crosses_is_split_and_its_edges_are_sliced() {
    // A 3px border and 2px padding around four 20px items, in two 100px columns:
    // the paragraph's flow is cut between its second and third item. Each piece
    // keeps only the edges of the side of the cut it lies on.
    let (output, _, layout) = pipeline(
        &format!(
            "<!doctype html><body><div id='c'><div id='p'>{}</div></div></body>",
            items(4)
        ),
        &format!(
            "{RESET} #c {{ width: 200px; columns: 2; column-gap: 0 }} \
             #p {{ border: 3px solid black; padding: 2px }} .item {{ height: 20px }}"
        ),
        800.0,
    );
    let dom = &output.dom;
    let p = find(dom, "#p");
    let split = pieces(&layout, p);
    assert_eq!(
        split.len(),
        2,
        "the paragraph has one piece per column: {split:?}"
    );
    assert!(
        near(split[0].origin.x, 0.0) && near(split[1].origin.x, 100.0),
        "{split:?}"
    );
    // Each piece is 40px of items plus the edges it keeps: 3px + 2px on the
    // side where its flow starts or ends, and nothing on the cut side.
    assert!(near(split[0].size.height, 45.0), "{split:?}");
    assert!(near(split[1].size.height, 45.0), "{split:?}");
    let style_of = |index: usize| {
        let owner = layout
            .fragments
            .iter()
            .filter(|fragment| fragment.source == Some(p))
            .nth(index)
            .expect("the paragraph has this piece");
        match &owner.kind {
            crate::fragment::FragmentKind::Box(geometry) => geometry.clone(),
            crate::fragment::FragmentKind::Text(_) => panic!("a paragraph is a box"),
        }
    };
    let first = style_of(0);
    assert!(
        near(first.border.top, 3.0) && near(first.border.bottom, 0.0),
        "{first:?}"
    );
    assert!(
        near(first.padding.top, 2.0) && near(first.padding.bottom, 0.0),
        "{first:?}"
    );
    let second = style_of(1);
    assert!(
        near(second.border.top, 0.0) && near(second.border.bottom, 3.0),
        "{second:?}"
    );
    assert!(
        near(second.padding.top, 0.0) && near(second.padding.bottom, 2.0),
        "{second:?}"
    );
}

#[test]
fn column_count_and_column_width_together_take_the_smaller_column_count() {
    // 400px of content with a 10px gap: a 150px column width fits two columns,
    // so a column-count of three is capped at two.
    let (output, _, layout) = pipeline(
        &format!("<!doctype html><body><div id='c'>{}</div></body>", items(4)),
        &format!(
            "{RESET} #c {{ width: 400px; column-count: 3; column-width: 150px; column-gap: 10px }} \
             .item {{ height: 20px }}"
        ),
        800.0,
    );
    let dom = &output.dom;
    // Two columns of (400 - 10) / 2 = 195px, so the second starts at 205px.
    let second = rect(&layout, find(dom, "#i2"));
    assert!(near(second.origin.x, 205.0), "{second:?}");
}

#[test]
fn a_column_count_gives_that_many_columns_of_equal_width() {
    // Three columns of (400 - 20) / 3 wide: six items land two to a column.
    let (output, _, layout) = pipeline(
        &format!("<!doctype html><body><div id='c'>{}</div></body>", items(6)),
        &format!(
            "{RESET} #c {{ width: 400px; column-count: 3; column-gap: 10px }} \
             .item {{ height: 20px }}"
        ),
        800.0,
    );
    let dom = &output.dom;
    let pitch = (400.0 - 20.0) / 3.0 + 10.0;
    let third = rect(&layout, find(dom, "#i4"));
    assert!(near(third.origin.x, 2.0 * pitch), "{third:?}");
    assert!(near(rect(&layout, find(dom, "#c")).size.height, 40.0));
}

#[test]
fn a_normal_column_gap_is_one_em_of_the_container_font() {
    // With `column-gap: normal` and a 16px font, two columns of (210 - 16) / 2
    // are separated by 16px, so the second column starts at 97 + 16.
    let (output, _, layout) = pipeline(
        &format!("<!doctype html><body><div id='c'>{}</div></body>", items(4)),
        &format!("{RESET} #c {{ width: 210px; column-count: 2 }} .item {{ height: 20px }}"),
        800.0,
    );
    let dom = &output.dom;
    let second = rect(&layout, find(dom, "#i2"));
    assert!(near(second.origin.x, 113.0), "{second:?}");
}

#[test]
fn a_column_span_child_closes_its_column_set_and_takes_the_full_width() {
    // Two items, a full-width heading, then two more items, in two 100px columns
    // with no gap. Each set balances over its own columns, and the heading sits
    // between the sets at the container's full width.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='c'>\
           <div id='i0' class='item'></div><div id='i1' class='item'></div>\
           <div id='h' class='item span'></div>\
           <div id='i2' class='item'></div><div id='i3' class='item'></div>\
         </div></body>",
        &format!(
            "{RESET} #c {{ width: 200px; columns: 2; column-gap: 0 }} \
             .item {{ height: 20px }} .span {{ column-span: all }}"
        ),
        800.0,
    );
    let dom = &output.dom;
    let second_column = rect(&layout, find(dom, "#i1"));
    assert!(
        near(second_column.origin.x, 100.0) && near(second_column.origin.y, 0.0),
        "{second_column:?}"
    );
    let heading = rect(&layout, find(dom, "#h"));
    assert!(
        near(heading.origin.x, 0.0)
            && near(heading.origin.y, 20.0)
            && near(heading.size.width, 200.0),
        "the spanning child is {heading:?}"
    );
    let after = rect(&layout, find(dom, "#i3"));
    assert!(
        near(after.origin.x, 100.0) && near(after.origin.y, 40.0),
        "{after:?}"
    );
    assert!(near(rect(&layout, find(dom, "#c")).size.height, 60.0));
}

#[test]
fn a_constrained_height_with_column_fill_auto_overflows_into_further_columns() {
    // The container is 50px tall, so with `column-fill: auto` the columns fill to
    // 50px rather than balancing. Five 20px items need a third column, which lies
    // to the right of the two that the container's width gives.
    let (output, _, layout) = pipeline(
        &format!("<!doctype html><body><div id='c'>{}</div></body>", items(5)),
        &format!(
            "{RESET} #c {{ width: 200px; height: 50px; column-count: 2; column-gap: 0; \
             column-fill: auto }} .item {{ height: 20px }}"
        ),
        800.0,
    );
    let dom = &output.dom;
    assert!(near(rect(&layout, find(dom, "#c")).size.height, 50.0));
    let overflow = rect(&layout, find(dom, "#i4"));
    assert!(
        near(overflow.origin.x, 200.0) && near(overflow.origin.y, 0.0),
        "{overflow:?}"
    );
}
