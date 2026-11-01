//! CSS Writing Modes 3 block-axis mirroring for `direction: rtl`.
//!
//! The bidi tests pin inline-level reordering. These tests pin the other half:
//! where the block itself, its margins, its floats, its line boxes against a
//! float, and its out-of-flow descendants are placed when the containing block
//! runs right-to-left. Every expected coordinate follows CSS 2.1 §9.4.3 /
//! §10.3.3 / §10.3.7 for a horizontal-tb flow, and the reference measurer's
//! 8px-per-narrow-character advance makes them checkable by hand.

#![allow(clippy::float_cmp)]

use crate::PhysicalRect;
use crate::fragment::FragmentKind;

use super::tests::{find, fragment, pipeline};

const RESET: &str = "html, body, div, p { display:block; margin:0; padding:0; border:0 }";

fn rect(layout: &crate::solver::LayoutOutput, source: render_dom::NodeId) -> PhysicalRect {
    fragment(layout, source).rect
}

#[test]
fn a_fixed_width_block_starts_at_the_right_edge_of_an_rtl_containing_block() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body>\
           <div id='rtl'><div id='box'>abc</div></div>\
           <div id='ltr'><div id='ltr-box'></div></div></body>",
        &format!(
            "{RESET} #rtl {{ width:200px; height:10px; direction:rtl }} \
             #ltr {{ width:200px; height:10px; direction:ltr }} \
             #box, #ltr-box {{ width:60px; height:10px }}"
        ),
        400.0,
    );
    let dom = &output.dom;
    assert_eq!(
        rect(&layout, find(dom, "#box")),
        PhysicalRect::new(140.0, 0.0, 60.0, 10.0)
    );
    // The direction-selected margin absorbs the free space when it is the one
    // the over-constrained equation ignores: the left margin in RTL and the
    // right margin in LTR.
    let FragmentKind::Box(rtl) = &fragment(&layout, find(dom, "#box")).kind else {
        panic!("expected a box fragment")
    };
    assert_eq!(rtl.margin.left, 140.0);
    assert_eq!(rtl.margin.right, 0.0);
    // The box's own start edge is also the start edge of its line box, so the
    // three narrow LTR characters are right-aligned inside the 60px box.
    let text = layout
        .fragments
        .iter()
        .find(|candidate| matches!(candidate.kind, FragmentKind::Text(_)))
        .expect("the RTL box has one text run");
    assert_eq!(text.rect.origin.x, 176.0);
    // LTR geometry is unchanged: the box still starts at the left edge.
    assert_eq!(
        rect(&layout, find(dom, "#ltr-box")),
        PhysicalRect::new(0.0, 10.0, 60.0, 10.0)
    );
}

#[test]
fn the_root_direction_sets_the_initial_containing_block_direction() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='box'></div></body>",
        &format!("{RESET} html {{ direction:rtl }} #box {{ width:60px; height:10px }}"),
        400.0,
    );
    assert_eq!(
        rect(&layout, find(&output.dom, "#box")),
        PhysicalRect::new(340.0, 0.0, 60.0, 10.0)
    );
}

#[test]
fn logical_float_sides_follow_the_declaring_elements_direction() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='rtl'><div id='start'></div><div id='end'></div></div></body>",
        &format!(
            "{RESET} #rtl {{ display:flow-root; width:200px; direction:rtl }} \
             #start {{ float:inline-start; width:50px; height:20px }} \
             #end {{ float:inline-end; width:30px; height:20px }}"
        ),
        400.0,
    );
    let dom = &output.dom;
    assert_eq!(rect(&layout, find(dom, "#start")).origin.x, 150.0);
    assert_eq!(rect(&layout, find(dom, "#end")).origin.x, 0.0);
}

#[test]
fn logical_clear_sides_follow_the_declaring_elements_direction() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='rtl'><div id='float'></div><div id='clear'></div></div></body>",
        &format!(
            "{RESET} #rtl {{ width:200px; direction:rtl }} \
             #float {{ float:right; width:50px; height:30px }} \
             #clear {{ clear:inline-start; height:10px }}"
        ),
        400.0,
    );
    assert_eq!(rect(&layout, find(&output.dom, "#clear")).origin.y, 30.0);
}

#[test]
fn auto_margins_center_in_both_directions_and_rtl_solves_a_negative_remainder_from_the_start() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body>\
           <div id='wide'><div id='centered'></div></div>\
           <div id='narrow'><div id='overflow'></div></div></body>",
        &format!(
            "{RESET} #wide {{ width:200px; height:10px; direction:rtl }} \
             #narrow {{ width:60px; height:10px; direction:rtl }} \
             #centered {{ width:100px; height:10px; margin-left:auto; margin-right:auto }} \
             #overflow {{ width:100px; height:10px; margin-left:auto; margin-right:auto }}"
        ),
        400.0,
    );
    let dom = &output.dom;
    // Positive free space is still split equally; direction does not change
    // the centering rule (CSS 2.1 §10.3.3).
    assert_eq!(rect(&layout, find(dom, "#centered")).origin.x, 50.0);
    let FragmentKind::Box(centered) = &fragment(&layout, find(dom, "#centered")).kind else {
        panic!("expected a box fragment")
    };
    assert_eq!(centered.margin.left, 50.0);
    assert_eq!(centered.margin.right, 50.0);

    // With negative free space the auto margins are treated as zero and the
    // over-constrained equation ignores the end margin. In RTL that is
    // `margin-left`, so the box's right edge stays on the containing block's
    // right edge and it overflows to the left.
    let overflow = rect(&layout, find(dom, "#overflow"));
    assert_eq!(overflow, PhysicalRect::new(-40.0, 10.0, 100.0, 10.0));
    let FragmentKind::Box(overflow_geometry) = &fragment(&layout, find(dom, "#overflow")).kind
    else {
        panic!("expected a box fragment")
    };
    assert_eq!(overflow_geometry.margin.left, -40.0);
    assert_eq!(overflow_geometry.margin.right, 0.0);
}

#[test]
fn an_rtl_absolute_box_uses_the_right_inset_or_static_start_edge() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='cb'>\
           <div id='both'></div><div id='static'></div><div id='left'></div>\
         </div></body>",
        &format!(
            "{RESET} #cb {{ position:relative; width:200px; height:100px; direction:rtl }} \
             #both {{ position:absolute; left:10px; right:20px; top:0; width:40px; height:10px }} \
             #static {{ position:absolute; top:20px; width:30px; height:10px }} \
             #left {{ position:absolute; left:15px; top:40px; width:10px; height:10px }}"
        ),
        400.0,
    );
    let dom = &output.dom;
    // CSS 2.1 §10.3.7: with a definite width and both insets, the
    // direction-selected inset wins, so RTL keeps `right:20px` and resolves
    // `left`.
    assert_eq!(
        rect(&layout, find(dom, "#both")),
        PhysicalRect::new(140.0, 0.0, 40.0, 10.0)
    );
    // All three of left/width/right auto uses the static start edge, which is
    // the containing block's right edge under RTL.
    assert_eq!(
        rect(&layout, find(dom, "#static")),
        PhysicalRect::new(170.0, 20.0, 30.0, 10.0)
    );
    // A lone physical `left` keeps its physical meaning.
    assert_eq!(
        rect(&layout, find(dom, "#left")),
        PhysicalRect::new(15.0, 40.0, 10.0, 10.0)
    );
}

#[test]
fn a_relatively_positioned_rtl_box_prefers_right_over_left() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='parent'><div id='rel'></div></div></body>",
        &format!(
            "{RESET} #parent {{ width:200px }} \
             #rel {{ position:relative; direction:rtl; left:10px; right:20px; \
                     width:50px; height:10px }}"
        ),
        400.0,
    );
    // CSS 2.1 §9.4.3: the RTL element's right inset wins, so the subtree moves
    // by -20px from its normal left-edge position in the LTR parent.
    assert_eq!(
        rect(&layout, find(&output.dom, "#rel")),
        PhysicalRect::new(-20.0, 0.0, 50.0, 10.0)
    );
}

#[test]
fn rtl_line_alignment_uses_the_band_left_by_a_right_float() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='p'><div id='float'></div>אבג</div></body>",
        &format!(
            "{RESET} #p {{ width:200px; direction:rtl }} \
             #float {{ float:right; width:50px; height:20px }}"
        ),
        400.0,
    );
    let dom = &output.dom;
    assert_eq!(rect(&layout, find(dom, "#float")).origin.x, 150.0);
    let text = layout
        .fragments
        .iter()
        .find(|candidate| matches!(candidate.kind, FragmentKind::Text(_)))
        .expect("the paragraph has one text run");
    // The start edge of the line is the float's left edge, not the paragraph's
    // right edge: 150 - 24 (three 8px Hebrew letters) = 126.
    assert_eq!(text.rect, PhysicalRect::new(126.0, 0.0, 24.0, 19.2));
}
