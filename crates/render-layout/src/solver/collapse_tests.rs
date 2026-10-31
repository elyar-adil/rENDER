//! CSS 2.1 §8.3 vertical margin collapsing.
//!
//! Every assertion is a border-box edge in document space, so the tests pin
//! the geometry the collapsing rules produce rather than the margin values
//! that were resolved. `html` and `body` are reset to zero margins, and the
//! first margin group therefore lands at the document origin.

#![allow(clippy::float_cmp)]

use crate::PhysicalRect;
use crate::solver::LayoutOutput;
use render_dom::NodeId;

use super::tests::{find, fragment, pipeline};

const RESET: &str = "html, body { display:block; margin:0; padding:0; border:0 } \
                     div { display:block; width:100px }";

fn rect(layout: &LayoutOutput, source: NodeId) -> PhysicalRect {
    fragment(layout, source).rect
}

#[test]
fn adjacent_siblings_collapse_to_the_largest_positive_margin() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='a'></div><div id='b'></div></body>",
        &format!(
            "{RESET} #a {{ height:50px; margin-bottom:30px }} \
             #b {{ height:20px; margin-top:20px }}"
        ),
        400.0,
    );
    let a = rect(&layout, find(&output.dom, "#a"));
    let b = rect(&layout, find(&output.dom, "#b"));
    assert_eq!(a.origin.y, 0.0);
    assert_eq!(a.bottom(), 50.0);
    // max(30, 20) = 30, not 30 + 20.
    assert_eq!(b.origin.y, 80.0, "{b:?}");
}

#[test]
fn a_negative_margin_is_added_to_the_largest_positive_one() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='a'></div><div id='b'></div></body>",
        &format!(
            "{RESET} #a {{ height:50px; margin-bottom:20px }} \
             #b {{ height:20px; margin-top:-30px }}"
        ),
        400.0,
    );
    let a = rect(&layout, find(&output.dom, "#a"));
    let b = rect(&layout, find(&output.dom, "#b"));
    assert_eq!(a.bottom(), 50.0);
    // 20 + (-30) = -10: the boxes overlap by ten pixels.
    assert_eq!(b.origin.y, 40.0, "{a:?} {b:?}");

    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='a'></div><div id='b'></div></body>",
        &format!(
            "{RESET} #a {{ height:50px; margin-bottom:-10px }} \
             #b {{ height:20px; margin-top:-20px }}"
        ),
        400.0,
    );
    let b = rect(&layout, find(&output.dom, "#b"));
    // The most negative margin wins: max(0) + min(-10, -20) = -20.
    assert_eq!(b.origin.y, 30.0, "{b:?}");
}

#[test]
fn a_parent_top_margin_collapses_with_its_first_child() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='parent'><div id='child'></div></div></body>",
        &format!(
            "{RESET} #parent {{ margin-top:10px }} \
             #child {{ height:50px; margin-top:30px }}"
        ),
        400.0,
    );
    let parent = rect(&layout, find(&output.dom, "#parent"));
    let child = rect(&layout, find(&output.dom, "#child"));
    // max(10, 30) = 30, applied above the parent's border edge. The child's
    // margin is no longer repeated between the parent's content edge and the
    // child's border edge.
    assert_eq!(parent.origin.y, 30.0, "{parent:?}");
    assert_eq!(child.origin.y, 30.0, "{child:?}");
    assert_eq!(parent.size.height, 50.0, "{parent:?}");
    assert_eq!(parent.bottom(), 80.0, "{parent:?}");
}

#[test]
fn a_top_border_stops_the_parent_first_child_collapse() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='parent'><div id='child'></div></div></body>",
        &format!(
            "{RESET} #parent {{ margin-top:10px; border-top:2px solid black }} \
             #child {{ height:50px; margin-top:30px }}"
        ),
        400.0,
    );
    let parent = rect(&layout, find(&output.dom, "#parent"));
    let child = rect(&layout, find(&output.dom, "#child"));
    // The parent's margin is 10; the border starts at 10 and the child's own
    // margin is laid out inside at 10 + 2 + 30.
    assert_eq!(parent.origin.y, 10.0, "{parent:?}");
    assert_eq!(child.origin.y, 42.0, "{child:?}");
    assert_eq!(parent.size.height, 82.0, "{parent:?}");
}

#[test]
fn a_parent_bottom_margin_collapses_with_its_last_child() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body>\
           <div id='parent'><div id='child'></div></div><div id='after'></div>\
         </body>",
        &format!(
            "{RESET} #parent {{ margin-bottom:50px }} \
             #child {{ height:50px; margin-bottom:30px }} \
             #after {{ height:20px }}"
        ),
        400.0,
    );
    let parent = rect(&layout, find(&output.dom, "#parent"));
    let after = rect(&layout, find(&output.dom, "#after"));
    // The child's bottom margin escapes the auto-height parent, so the parent
    // is exactly as tall as the child. The group max(50, 30) then separates it
    // from the next sibling.
    assert_eq!(parent.size.height, 50.0, "{parent:?}");
    assert_eq!(after.origin.y, 100.0, "{after:?}");
}

#[test]
fn bottom_padding_stops_the_parent_last_child_collapse() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body>\
           <div id='parent'><div id='child'></div></div><div id='after'></div>\
         </body>",
        &format!(
            "{RESET} #parent {{ padding-bottom:1px; margin-bottom:50px }} \
             #child {{ height:50px; margin-bottom:30px }} \
             #after {{ height:20px }}"
        ),
        400.0,
    );
    let parent = rect(&layout, find(&output.dom, "#parent"));
    let after = rect(&layout, find(&output.dom, "#after"));
    // The child's bottom margin is inside the parent now: 50 + 30 + 1.
    assert_eq!(parent.size.height, 81.0, "{parent:?}");
    assert_eq!(after.origin.y, 131.0, "{after:?}");
}

#[test]
fn an_empty_block_collapses_through_the_whole_group() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body>\
           <div id='a'></div><div id='empty'></div><div id='b'></div>\
         </body>",
        &format!(
            "{RESET} #a {{ height:50px; margin-bottom:10px }} \
             #empty {{ margin-top:20px; margin-bottom:30px }} \
             #b {{ height:20px; margin-top:40px }}"
        ),
        400.0,
    );
    let a = rect(&layout, find(&output.dom, "#a"));
    let empty = rect(&layout, find(&output.dom, "#empty"));
    let b = rect(&layout, find(&output.dom, "#b"));
    assert_eq!(a.bottom(), 50.0);
    // max(10, 20) places the empty box's zero-height border edge; its own
    // margins then join the same group rather than applying twice.
    assert_eq!(empty.origin.y, 70.0, "{empty:?}");
    assert_eq!(empty.size.height, 0.0, "{empty:?}");
    // max(10, 20, 30, 40) = 40 once between the two visible boxes.
    assert_eq!(b.origin.y, 90.0, "{b:?}");
}

#[test]
fn a_border_stops_an_empty_block_from_collapsing_through() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body>\
           <div id='a'></div><div id='empty'></div><div id='b'></div>\
         </body>",
        &format!(
            "{RESET} #a {{ height:50px; margin-bottom:10px }} \
             #empty {{ margin-top:20px; margin-bottom:30px; \
                       border-top:1px solid black }} \
             #b {{ height:20px; margin-top:40px }}"
        ),
        400.0,
    );
    let empty = rect(&layout, find(&output.dom, "#empty"));
    let b = rect(&layout, find(&output.dom, "#b"));
    // The border separates the two margin groups: max(10, 20) = 20 above the
    // border, then the 1px border, then max(30, 40) = 40 below it.
    assert_eq!(empty.origin.y, 70.0, "{empty:?}");
    assert_eq!(empty.size.height, 1.0, "{empty:?}");
    assert_eq!(b.origin.y, 111.0, "{b:?}");
}

#[test]
fn nested_empty_blocks_share_one_collapse_group() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body>\
           <div id='a'></div>\
           <div id='empty'><div id='inner'></div></div>\
           <div id='b'></div>\
         </body>",
        &format!(
            "{RESET} #a {{ height:50px; margin-bottom:10px }} \
             #empty {{ margin-top:20px; margin-bottom:30px }} \
             #inner {{ margin-top:15px; margin-bottom:25px }} \
             #b {{ height:20px; margin-top:40px }}"
        ),
        400.0,
    );
    let b = rect(&layout, find(&output.dom, "#b"));
    // The empty parent and its empty child both collapse through, so the
    // entire set max(10, 20, 15, 25, 30, 40) applies once.
    assert_eq!(b.origin.y, 90.0, "{b:?}");
}

#[test]
fn parent_child_collapse_moves_positioned_descendants_with_their_containing_block() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body>\
           <div id='parent'><div id='child'><div id='absolute'></div></div></div>\
         </body>",
        &format!(
            "{RESET} #parent {{ position:relative; margin-top:10px; width:200px }} \
             #child {{ height:40px; margin-top:30px }} \
             #absolute {{ position:absolute; left:0; top:5px; width:10px; height:10px }}"
        ),
        400.0,
    );
    let parent = rect(&layout, find(&output.dom, "#parent"));
    let child = rect(&layout, find(&output.dom, "#child"));
    let absolute = rect(&layout, find(&output.dom, "#absolute"));
    // The parent's padding box is the absolute box's containing block. It
    // moves with the collapsed top edge, so the inset is measured from y=30.
    assert_eq!(parent.origin.y, 30.0, "{parent:?}");
    assert_eq!(child.origin.y, 30.0, "{child:?}");
    assert_eq!(absolute.origin.y, 35.0, "{absolute:?}");
}

#[test]
fn a_parent_top_margin_reaches_across_an_empty_first_child() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body>\
           <div id='parent'><div id='empty'></div><div id='child'></div></div>\
         </body>",
        &format!(
            "{RESET} #parent {{ margin-top:10px }} \
             #empty {{ margin-top:20px; margin-bottom:30px }} \
             #child {{ height:40px; margin-top:15px }}"
        ),
        400.0,
    );
    let parent = rect(&layout, find(&output.dom, "#parent"));
    let child = rect(&layout, find(&output.dom, "#child"));
    // The empty first child passes the whole group to the first non-empty
    // child: max(10, 20, 30, 15) = 30.
    assert_eq!(parent.origin.y, 30.0, "{parent:?}");
    assert_eq!(child.origin.y, 30.0, "{child:?}");
    assert_eq!(parent.size.height, 40.0, "{parent:?}");
}

#[test]
fn a_block_formatting_context_does_not_collapse_with_its_children() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body>\
           <div id='a'></div>\
           <div id='b'><div id='inner'></div></div>\
         </body>",
        &format!(
            "{RESET} #a {{ height:50px; margin-bottom:30px }} \
             #b {{ overflow:hidden; margin-top:20px; height:50px }} \
             #inner {{ height:10px; margin-top:20px }}"
        ),
        400.0,
    );
    let b = rect(&layout, find(&output.dom, "#b"));
    let inner = rect(&layout, find(&output.dom, "#inner"));
    // `overflow: hidden` still lets the box's own top margin collapse with
    // the preceding sibling, but the child's top margin stays inside.
    assert_eq!(b.origin.y, 80.0, "{b:?}");
    assert_eq!(inner.origin.y, 100.0, "{inner:?}");
}

#[test]
fn a_negative_parent_bottom_margin_shrinks_the_following_gap() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body>\
           <div id='parent'><div id='child'></div></div><div id='after'></div>\
         </body>",
        &format!(
            "{RESET} #parent {{ margin-bottom:10px }} \
             #child {{ height:50px; margin-bottom:-30px }} \
             #after {{ height:20px }}"
        ),
        400.0,
    );
    let after = rect(&layout, find(&output.dom, "#after"));
    // The parent/child group is 10 + (-30) = -20, so the next box starts
    // twenty pixels above the parent's border bottom.
    assert_eq!(after.origin.y, 30.0, "{after:?}");
}
