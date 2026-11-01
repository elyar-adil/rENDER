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

/// `margin-inline-start` is `margin-left` under LTR and `margin-right` under
/// RTL; the two spellings produce identical geometry on the same side.
#[test]
fn logical_and_physical_margins_produce_identical_ltr_and_rtl_geometry() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body>\
           <div id='ltr'><div id='ltr-logical'></div><div id='ltr-physical'></div></div>\
           <div id='rtl'><div id='rtl-logical'></div><div id='rtl-physical'></div></div>\
         </body>",
        &format!(
            "{RESET} #ltr, #rtl {{ width:200px; height:40px }} \
             #ltr-logical, #rtl-logical {{ width:50px; height:10px; margin-inline-start:12px }} \
             #ltr-physical {{ width:50px; height:10px; margin-left:12px }} \
             #rtl-physical {{ width:50px; height:10px; margin-right:12px }} \
             #rtl {{ direction:rtl }}"
        ),
        400.0,
    );
    let dom = &output.dom;

    assert_eq!(
        rect(&layout, find(dom, "#ltr-logical")),
        PhysicalRect::new(12.0, 0.0, 50.0, 10.0)
    );
    assert_eq!(
        rect(&layout, find(dom, "#ltr-physical")),
        PhysicalRect::new(12.0, 10.0, 50.0, 10.0)
    );
    // RTL inline-start is the right edge: 200 - 12 - 50.
    assert_eq!(
        rect(&layout, find(dom, "#rtl-logical")),
        PhysicalRect::new(138.0, 40.0, 50.0, 10.0)
    );
    assert_eq!(
        rect(&layout, find(dom, "#rtl-physical")),
        PhysicalRect::new(138.0, 50.0, 50.0, 10.0)
    );
}

/// The logical padding shorthand feeds the physical padding edges layout uses,
/// and the content box shrinks by exactly those used values.
#[test]
fn logical_padding_shorthand_maps_to_the_physical_edges() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body>\
           <div id='ltr'><div id='ltr-inner'></div></div>\
           <div id='rtl'><div id='rtl-inner'></div></div>\
         </body>",
        &format!(
            "{RESET} #ltr, #rtl {{ box-sizing:border-box; width:100px; height:30px; \
                 padding-inline-start:10px; padding-inline-end:20px }} \
             #ltr-inner, #rtl-inner {{ height:10px }} \
             #rtl {{ direction:rtl }}"
        ),
        400.0,
    );
    let dom = &output.dom;
    for (parent, selector, left_padding) in
        [("#ltr", "#ltr-inner", 10.0), ("#rtl", "#rtl-inner", 20.0)]
    {
        let parent_geometry = match &fragment(&layout, find(dom, parent)).kind {
            FragmentKind::Box(geometry) => geometry,
            FragmentKind::Text(_) => panic!("expected a box fragment"),
        };
        assert_eq!(parent_geometry.padding.left, left_padding, "{parent}");
        assert_eq!(
            parent_geometry.padding.right,
            30.0 - left_padding,
            "{parent}"
        );
        assert_eq!(
            parent_geometry.content_rect.size.width, 70.0,
            "{parent} content box excludes both paddings"
        );
        let geometry = match &fragment(&layout, find(dom, selector)).kind {
            FragmentKind::Box(geometry) => geometry,
            FragmentKind::Text(_) => panic!("expected a box fragment"),
        };
        assert_eq!(
            geometry.content_rect.size.width, 70.0,
            "{selector} fills the content box"
        );
        assert_eq!(
            rect(&layout, find(dom, selector)).origin.x,
            left_padding,
            "{selector} starts after the used left padding"
        );
    }
}

/// `inset-inline-*` and `inset-block-*` are the same used insets as
/// `left`/`right` and `top`/`bottom` for positioned layout.
#[test]
fn logical_insets_feed_positioned_layout_in_both_directions() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body>\
           <div id='ltr-cb'><div id='ltr-box'></div><div id='ltr-end'></div></div>\
           <div id='rtl-cb'><div id='rtl-box'></div><div id='rtl-end'></div></div>\
         </body>",
        &format!(
            "{RESET} #ltr-cb, #rtl-cb {{ position:relative; width:200px; height:100px }} \
             #ltr-cb, #ltr-box, #ltr-end {{ direction:ltr }} \
             #rtl-cb {{ direction:rtl }} \
             #ltr-box, #rtl-box {{ position:absolute; width:40px; height:10px; \
                 inset-inline-start:10px; inset-block-start:20px }} \
             #ltr-end {{ position:absolute; width:20px; height:10px; \
                 inset-inline-end:15px; inset-block-end:5px }} \
             #rtl-end {{ position:absolute; width:20px; height:10px; \
                 inset-inline-end:15px; inset-block-end:5px }}"
        ),
        400.0,
    );
    let dom = &output.dom;

    assert_eq!(
        rect(&layout, find(dom, "#ltr-box")),
        PhysicalRect::new(10.0, 20.0, 40.0, 10.0)
    );
    // RTL inline-start is the right inset: 200 - 10 - 40. The second
    // containing block starts 100px down the page.
    assert_eq!(
        rect(&layout, find(dom, "#rtl-box")),
        PhysicalRect::new(150.0, 120.0, 40.0, 10.0)
    );
    // LTR inline-end is right: 200 - 15 - 20; block-end is bottom: 100 - 5 - 10.
    assert_eq!(
        rect(&layout, find(dom, "#ltr-end")),
        PhysicalRect::new(165.0, 85.0, 20.0, 10.0)
    );
    // RTL inline-end is left: 15 from the left edge, in the second containing
    // block.
    assert_eq!(
        rect(&layout, find(dom, "#rtl-end")),
        PhysicalRect::new(15.0, 185.0, 20.0, 10.0)
    );
}

/// Logical border longhands set the physical border edges, including the
/// direction-selected start/end swap, without changing the content width.
#[test]
fn logical_borders_affect_physical_geometry_and_follow_direction() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body>\
           <div id='ltr'></div><div id='rtl'></div>\
         </body>",
        &format!(
            "{RESET} #ltr, #rtl {{ width:100px; height:10px; \
                 border-inline-start:3px solid; border-inline-end:7px solid; \
                 border-block-start:4px solid; border-block-end:2px solid }} \
             #rtl {{ direction:rtl }}"
        ),
        400.0,
    );
    let dom = &output.dom;
    for (selector, left, right) in [("#ltr", 3.0, 7.0), ("#rtl", 7.0, 3.0)] {
        let geometry = match &fragment(&layout, find(dom, selector)).kind {
            FragmentKind::Box(geometry) => geometry,
            FragmentKind::Text(_) => panic!("expected a box fragment"),
        };
        assert_eq!(geometry.border.left, left, "{selector}");
        assert_eq!(geometry.border.right, right, "{selector}");
        // The block axis is top/bottom in horizontal-tb and does not swap for
        // RTL.
        assert_eq!(geometry.border.top, 4.0, "{selector}");
        assert_eq!(geometry.border.bottom, 2.0, "{selector}");
        assert_eq!(geometry.content_rect.size.width, 100.0, "{selector}");
        assert_eq!(
            geometry.border_rect().size.width,
            110.0,
            "{selector} outer width includes both borders"
        );
        assert_eq!(
            geometry.border_rect().size.height,
            16.0,
            "{selector} outer height includes both block borders"
        );
    }
}

/// Logical values go through the same length engine as physical ones:
/// percentages against the containing block, calc/em against font size, rem
/// against the root font size, and viewport units against the viewport.
#[test]
fn logical_values_resolve_percentages_calc_em_rem_and_viewport_units() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body>\
           <div id='pct-ltr'><div id='pct-box'></div></div>\
           <div id='pct-rtl'><div id='pct-rtl-box'></div></div>\
           <div id='units'></div>\
           <div id='cb'><div id='inset'></div></div>\
         </body>",
        &format!(
            "{RESET} #pct-ltr, #pct-rtl {{ width:200px; height:10px }} \
             #pct-box, #pct-rtl-box {{ width:50px; height:10px; margin-inline-start:10% }} \
             #pct-rtl {{ direction:rtl }} \
             #units {{ width:100px; height:10px; font-size:10px; \
                 margin-inline-start:5vw; \
                 border-inline-start:0.5rem solid; \
                 padding-inline-start:calc(1em + 2px) }} \
             #cb {{ position:relative; width:200px; height:100px; direction:rtl }} \
             #inset {{ position:absolute; inset-inline-start:calc(25% + 2px); \
                 width:40px; height:10px }}"
        ),
        400.0,
    );
    let dom = &output.dom;

    assert_eq!(
        rect(&layout, find(dom, "#pct-box")),
        PhysicalRect::new(20.0, 0.0, 50.0, 10.0),
        "10% of a 200px containing block"
    );
    assert_eq!(
        rect(&layout, find(dom, "#pct-rtl-box")),
        PhysicalRect::new(130.0, 10.0, 50.0, 10.0),
        "RTL maps inline-start to the right: 200 - 20 - 50"
    );
    // 5vw of 400 = 20; rem borders are 8px (16px root); calc(1em + 2px)
    // is 12px (10px font). The content box stays 100px wide, so the border
    // rect is 100 + 12 padding + 8 border = 120px.
    let units = match &fragment(&layout, find(dom, "#units")).kind {
        FragmentKind::Box(geometry) => geometry,
        FragmentKind::Text(_) => panic!("expected a box fragment"),
    };
    assert_eq!(rect(&layout, find(dom, "#units")).origin.x, 20.0);
    assert_eq!(units.border.left, 8.0);
    assert_eq!(units.padding.left, 12.0);
    assert_eq!(units.content_rect.size.width, 100.0);
    assert_eq!(units.border_rect().size.width, 120.0);
    // 25% of 200 + 2 = 52 from the RTL start (right) edge. The containing
    // block starts 30px down after the three preceding 10px boxes.
    assert_eq!(
        rect(&layout, find(dom, "#inset")),
        PhysicalRect::new(108.0, 30.0, 40.0, 10.0)
    );
}
