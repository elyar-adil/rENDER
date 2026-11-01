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

fn text_rect(layout: &crate::solver::LayoutOutput, needle: &str) -> PhysicalRect {
    layout
        .fragments
        .iter()
        .find(|candidate| {
            matches!(&candidate.kind, FragmentKind::Text(text) if text.text.contains(needle))
        })
        .unwrap_or_else(|| panic!("no text run containing {needle:?}"))
        .rect
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

// ---------------------------------------------------------------------------
// Flexbox: the main axis is the inline axis for `row`, so RTL reverses the
// physical order and `justify-content` measures free space from the right.
// ---------------------------------------------------------------------------

const FLEX_CSS: &str = "html, body, div { display:block; margin:0; padding:0; border:0 } \
     .flex { display:flex; width:150px; height:20px } \
     .flex > div { flex:none; width:50px; height:20px }";

#[test]
fn flex_row_main_axis_order_follows_direction_and_row_reverse_flips_it() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body>\
           <div id='ltr' class='flex'><div id='ltr-a'></div><div id='ltr-b'></div><div id='ltr-c'></div></div>\
           <div id='rtl' class='flex'><div id='rtl-a'></div><div id='rtl-b'></div><div id='rtl-c'></div></div>\
           <div id='ltr-rev' class='flex'><div id='lr-a'></div><div id='lr-b'></div><div id='lr-c'></div></div>\
           <div id='rtl-rev' class='flex'><div id='rr-a'></div><div id='rr-b'></div><div id='rr-c'></div></div>\
         </body>",
        &format!(
            "{FLEX_CSS} #rtl, #rtl-rev {{ direction:rtl }} \
             #ltr-rev, #rtl-rev {{ flex-direction:row-reverse }}"
        ),
        400.0,
    );
    let dom = &output.dom;
    for (selectors, expected) in [
        (["#ltr-a", "#ltr-b", "#ltr-c"], [0.0, 50.0, 100.0]),
        (["#rtl-a", "#rtl-b", "#rtl-c"], [100.0, 50.0, 0.0]),
        (["#lr-a", "#lr-b", "#lr-c"], [100.0, 50.0, 0.0]),
        (["#rr-a", "#rr-b", "#rr-c"], [0.0, 50.0, 100.0]),
    ] {
        for (selector, x) in selectors.into_iter().zip(expected) {
            assert_eq!(rect(&layout, find(dom, selector)).origin.x, x, "{selector}");
        }
    }
}

#[test]
fn flex_justify_content_start_and_end_follow_direction() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body>\
           <div id='ltr-start' class='wide'><div id='ls-a'></div><div id='ls-b'></div><div id='ls-c'></div></div>\
           <div id='rtl-start' class='wide'><div id='rs-a'></div><div id='rs-b'></div><div id='rs-c'></div></div>\
           <div id='ltr-flex-start' class='wide'><div id='lfs-a'></div><div id='lfs-b'></div><div id='lfs-c'></div></div>\
           <div id='rtl-flex-start' class='wide'><div id='rfs-a'></div><div id='rfs-b'></div><div id='rfs-c'></div></div>\
           <div id='ltr-end' class='wide'><div id='le-a'></div><div id='le-b'></div><div id='le-c'></div></div>\
           <div id='rtl-end' class='wide'><div id='re-a'></div><div id='re-b'></div><div id='re-c'></div></div>\
           <div id='ltr-flex-end' class='wide'><div id='lfe-a'></div><div id='lfe-b'></div><div id='lfe-c'></div></div>\
           <div id='rtl-flex-end' class='wide'><div id='rfe-a'></div><div id='rfe-b'></div><div id='rfe-c'></div></div>\
         </body>",
        &format!(
            "{FLEX_CSS} .wide {{ display:flex; width:200px; height:20px }} \
             .wide > div {{ flex:none; width:50px; height:20px }} \
             #rtl-start, #rtl-flex-start, #rtl-end, #rtl-flex-end {{ direction:rtl }} \
             #ltr-start, #rtl-start {{ justify-content:start }} \
             #ltr-flex-start, #rtl-flex-start {{ justify-content:flex-start }} \
             #ltr-end, #rtl-end {{ justify-content:end }} \
             #ltr-flex-end, #rtl-flex-end {{ justify-content:flex-end }}"
        ),
        400.0,
    );
    let dom = &output.dom;
    for (selectors, expected) in [
        // `start`/`end` name the writing-mode inline edges: left in LTR,
        // right in RTL.
        (["#ls-a", "#ls-b", "#ls-c"], [0.0, 50.0, 100.0]),
        (["#rs-a", "#rs-b", "#rs-c"], [150.0, 100.0, 50.0]),
        (["#le-a", "#le-b", "#le-c"], [50.0, 100.0, 150.0]),
        (["#re-a", "#re-b", "#re-c"], [100.0, 50.0, 0.0]),
        // `flex-start`/`flex-end` name the main-start/main-end sides, which
        // for `row` are the same edges.
        (["#lfs-a", "#lfs-b", "#lfs-c"], [0.0, 50.0, 100.0]),
        (["#rfs-a", "#rfs-b", "#rfs-c"], [150.0, 100.0, 50.0]),
        (["#lfe-a", "#lfe-b", "#lfe-c"], [50.0, 100.0, 150.0]),
        (["#rfe-a", "#rfe-b", "#rfe-c"], [100.0, 50.0, 0.0]),
    ] {
        for (selector, x) in selectors.into_iter().zip(expected) {
            assert_eq!(rect(&layout, find(dom, selector)).origin.x, x, "{selector}");
        }
    }
}

#[test]
fn flex_justify_content_start_ignores_row_reverse_unlike_flex_start() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body>\
           <div id='rev-start' class='wide'><div id='rs-a'></div><div id='rs-b'></div><div id='rs-c'></div></div>\
           <div id='rev-flex-start' class='wide'><div id='rfs-a'></div><div id='rfs-b'></div><div id='rfs-c'></div></div>\
         </body>",
        &format!(
            "{FLEX_CSS} .wide {{ display:flex; width:200px; height:20px }} \
             .wide > div {{ flex:none; width:50px; height:20px }} \
             #rev-start, #rev-flex-start {{ flex-direction:row-reverse }} \
             #rev-start {{ justify-content:start }} \
             #rev-flex-start {{ justify-content:flex-start }}"
        ),
        400.0,
    );
    let dom = &output.dom;
    // CSS Box Alignment: `start` is the writing-mode start (left in LTR) even
    // though `row-reverse` makes the main-start the right side. The group is
    // flush left, and the first item sits at the group's main-start (right)
    // edge, so it is the rightmost of the three.
    for (selector, x) in [("#rs-a", 100.0), ("#rs-b", 50.0), ("#rs-c", 0.0)] {
        assert_eq!(rect(&layout, find(dom, selector)).origin.x, x, "{selector}");
    }
    for (selector, x) in [("#rfs-a", 150.0), ("#rfs-b", 100.0), ("#rfs-c", 50.0)] {
        assert_eq!(rect(&layout, find(dom, selector)).origin.x, x, "{selector}");
    }
}

#[test]
fn a_column_flex_container_mirrors_cross_axis_start_alignment_under_rtl() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body>\
           <div id='ltr' class='column'><div id='ltr-a'></div></div>\
           <div id='rtl' class='column'><div id='rtl-a'></div></div>\
         </body>",
        &format!(
            "{FLEX_CSS} .column {{ display:flex; flex-direction:column; width:200px; height:40px }} \
             .column > div {{ flex:none; width:50px; height:20px }} \
             #ltr {{ align-items:start }} #rtl {{ direction:rtl; align-items:start }}"
        ),
        400.0,
    );
    let dom = &output.dom;
    // The cross axis of a column is the inline axis: start is left in LTR and
    // right in RTL.
    assert_eq!(rect(&layout, find(dom, "#ltr-a")).origin.x, 0.0);
    assert_eq!(rect(&layout, find(dom, "#rtl-a")).origin.x, 150.0);
}

#[test]
fn flex_order_and_wrapping_keep_their_order_while_rtl_mirrors_the_line() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body>\
           <div id='ltr' class='flex'><div id='la' class='order-2'></div><div id='lb' class='order-0'></div><div id='lc' class='order-1'></div></div>\
           <div id='rtl' class='flex'><div id='ra' class='order-2'></div><div id='rb' class='order-0'></div><div id='rc' class='order-1'></div></div>\
           <div id='ltr-wrap' class='wrap'><div id='lwa'></div><div id='lwb'></div><div id='lwc'></div></div>\
           <div id='rtl-wrap' class='wrap'><div id='rwa'></div><div id='rwb'></div><div id='rwc'></div></div>\
         </body>",
        &format!(
            "{FLEX_CSS} .order-0 {{ order:-1 }} .order-1 {{ order:0 }} .order-2 {{ order:1 }} \
             #rtl {{ direction:rtl }} \
             .wrap {{ display:flex; flex-wrap:wrap; width:100px; height:40px }} \
             .wrap > div {{ flex:none; width:50px; height:20px }} \
             #rtl-wrap {{ direction:rtl }}"
        ),
        400.0,
    );
    let dom = &output.dom;
    // `order:-1` moves b to the main-start side; the physical line still runs
    // left to right in LTR and right to left in RTL.
    for (selector, x) in [
        ("#lb", 0.0),
        ("#lc", 50.0),
        ("#la", 100.0),
        ("#rb", 100.0),
        ("#rc", 50.0),
        ("#ra", 0.0),
    ] {
        assert_eq!(rect(&layout, find(dom, selector)).origin.x, x, "{selector}");
    }
    // Two 50px items fill the first wrapped line, and the third starts a new
    // line. The second line is still block-axis order, but its single item
    // sits at the inline-start edge of the RTL container.
    assert_eq!(rect(&layout, find(dom, "#lwa")).origin.x, 0.0);
    assert_eq!(rect(&layout, find(dom, "#lwb")).origin.x, 50.0);
    assert_eq!(rect(&layout, find(dom, "#lwc")).origin.x, 0.0);
    assert_eq!(
        rect(&layout, find(dom, "#lwc")).origin.y - rect(&layout, find(dom, "#ltr-wrap")).origin.y,
        20.0
    );
    assert_eq!(rect(&layout, find(dom, "#rwa")).origin.x, 50.0);
    assert_eq!(rect(&layout, find(dom, "#rwb")).origin.x, 0.0);
    assert_eq!(rect(&layout, find(dom, "#rwc")).origin.x, 50.0);
    assert_eq!(
        rect(&layout, find(dom, "#rwc")).origin.y - rect(&layout, find(dom, "#rtl-wrap")).origin.y,
        20.0
    );
}

#[test]
fn a_column_flex_container_honours_physical_cross_auto_margins_under_rtl() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body>\
           <div id='ltr-left' class='column'><div id='ll'></div></div>\
           <div id='rtl-left' class='column'><div id='rl'></div></div>\
           <div id='ltr-right' class='column'><div id='lr'></div></div>\
           <div id='rtl-right' class='column'><div id='rr'></div></div>\
         </body>",
        &format!(
            "{FLEX_CSS} .column {{ display:flex; flex-direction:column; width:200px; height:20px }} \
             .column > div {{ flex:none; width:50px; height:20px }} \
             #rtl-left, #rtl-right {{ direction:rtl }} \
             #ll, #rl {{ margin-left:auto }} #lr, #rr {{ margin-right:auto }}"
        ),
        400.0,
    );
    let dom = &output.dom;
    // Auto margins are physical: `margin-left:auto` leaves the free space on
    // the left and pushes the item right in both directions, and
    // `margin-right:auto` does the opposite.
    assert_eq!(rect(&layout, find(dom, "#ll")).origin.x, 150.0);
    assert_eq!(rect(&layout, find(dom, "#rl")).origin.x, 150.0);
    assert_eq!(rect(&layout, find(dom, "#lr")).origin.x, 0.0);
    assert_eq!(rect(&layout, find(dom, "#rr")).origin.x, 0.0);
}

#[test]
fn a_wrapped_column_flex_container_mirrors_its_line_boxes_under_rtl() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body>\
           <div id='l-wrap' class='wrapcol'>\
             <div id='lw0'></div><div id='lw1'></div><div id='lw2'></div>\
             <div id='lw3'></div><div id='lw4'></div><div id='lw5'></div>\
           </div>\
           <div id='r-wrap' class='wrapcol'>\
             <div id='rw0'></div><div id='rw1'></div><div id='rw2'></div>\
             <div id='rw3'></div><div id='rw4'></div><div id='rw5'></div>\
           </div>\
         </body>",
        &format!(
            "{FLEX_CSS} .wrapcol {{ display:flex; flex-direction:column; flex-wrap:wrap; \
                 align-content:start; width:200px; height:40px }} \
             .wrapcol > div {{ flex:none; width:50px; height:20px }} \
             #r-wrap {{ direction:rtl }}"
        ),
        400.0,
    );
    let dom = &output.dom;
    // Two 20px items fill a 40px column; the six items make three columns.
    // Cross-start is the left edge in LTR and the right edge in RTL.
    for prefix in ["l", "r"] {
        let expected = if prefix == "l" {
            [0.0, 50.0, 100.0]
        } else {
            [150.0, 100.0, 50.0]
        };
        for (line, x) in expected.into_iter().enumerate() {
            let selector = format!("#{prefix}w{}", line * 2);
            assert_eq!(
                rect(&layout, find(dom, &selector)).origin.x,
                x,
                "{selector}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Grid: line numbers count from the inline-start edge, so an RTL grid places
// column 1 at the right and `justify-content` aligns the tracks from there.
// ---------------------------------------------------------------------------

const GRID_CSS: &str = "html, body, div { display:block; margin:0; padding:0; border:0 } \
     .grid { display:grid; width:200px; grid-template-columns:50px 50px; grid-auto-rows:20px } \
     .grid > div { height:20px }";

#[test]
fn grid_column_placement_starts_at_the_right_edge_under_rtl() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body>\
           <div id='ltr' class='grid'><div id='ltr-a'></div><div id='ltr-b'></div></div>\
           <div id='rtl' class='grid'><div id='rtl-a'></div><div id='rtl-b'></div></div>\
         </body>",
        &format!("{GRID_CSS} #rtl {{ direction:rtl }}"),
        400.0,
    );
    let dom = &output.dom;
    assert_eq!(rect(&layout, find(dom, "#ltr-a")).origin.x, 0.0);
    assert_eq!(rect(&layout, find(dom, "#ltr-b")).origin.x, 50.0);
    // Auto-placement fills line 1 first; line 1 is the inline-start edge.
    assert_eq!(rect(&layout, find(dom, "#rtl-a")).origin.x, 150.0);
    assert_eq!(rect(&layout, find(dom, "#rtl-b")).origin.x, 100.0);
}

#[test]
fn grid_column_one_is_the_logical_start_line_in_both_directions() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body>\
           <div id='ltr' class='grid'><div id='ltr-a' class='first'></div><div id='ltr-b' class='second'></div></div>\
           <div id='rtl' class='grid'><div id='rtl-a' class='first'></div><div id='rtl-b' class='second'></div></div>\
         </body>",
        &format!(
            "{GRID_CSS} #rtl {{ direction:rtl }} \
             .first {{ grid-column:1 }} .second {{ grid-column:2 }}"
        ),
        400.0,
    );
    let dom = &output.dom;
    assert_eq!(rect(&layout, find(dom, "#ltr-a")).origin.x, 0.0);
    assert_eq!(rect(&layout, find(dom, "#ltr-b")).origin.x, 50.0);
    // `grid-column:1` is the first line from the inline-start edge, which is
    // the right edge under RTL.
    assert_eq!(rect(&layout, find(dom, "#rtl-a")).origin.x, 150.0);
    assert_eq!(rect(&layout, find(dom, "#rtl-b")).origin.x, 100.0);
}

#[test]
fn grid_justify_content_start_and_end_follow_direction() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body>\
           <div id='ltr-start' class='grid'><div id='ls-a'></div><div id='ls-b'></div></div>\
           <div id='rtl-start' class='grid'><div id='rs-a'></div><div id='rs-b'></div></div>\
           <div id='ltr-end' class='grid'><div id='le-a'></div><div id='le-b'></div></div>\
           <div id='rtl-end' class='grid'><div id='re-a'></div><div id='re-b'></div></div>\
         </body>",
        &format!(
            "{GRID_CSS} #rtl-start, #rtl-end {{ direction:rtl }} \
             #ltr-start, #rtl-start {{ justify-content:start }} \
             #ltr-end, #rtl-end {{ justify-content:end }}"
        ),
        400.0,
    );
    let dom = &output.dom;
    for (selectors, expected) in [
        (["#ls-a", "#ls-b"], [0.0, 50.0]),
        (["#rs-a", "#rs-b"], [150.0, 100.0]),
        // `end` leaves the 100px of free space at the inline-start edge, so
        // an LTR grid sits on the right and an RTL grid on the left.
        (["#le-a", "#le-b"], [100.0, 150.0]),
        (["#re-a", "#re-b"], [50.0, 0.0]),
    ] {
        for (selector, x) in selectors.into_iter().zip(expected) {
            assert_eq!(rect(&layout, find(dom, selector)).origin.x, x, "{selector}");
        }
    }
}

// ---------------------------------------------------------------------------
// Tables: CSS 2.1 §17.1 makes the table's direction the column direction, so
// an RTL table numbers column 1 from the right. Cell and caption inline
// alignment stays direction-relative (CSS 2.1 §17.5.3, §17.4).
// ---------------------------------------------------------------------------

const TABLE_RTL_CSS: &str = "html, body, table, caption, thead, tbody, tfoot, tr, td, div { \
     display:block; margin:0; padding:0; border:0 } \
     table { display:table; table-layout:fixed; width:200px; border-spacing:0 } \
     caption { display:table-caption } thead { display:table-header-group } \
     tbody, tfoot { display:table-row-group } \
     tr { display:table-row } td { display:table-cell }";

#[test]
fn table_columns_are_ordered_from_the_right_under_rtl() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body>\
           <table id='ltr'><tr><td id='ltr-a'>a</td><td id='ltr-b'>b</td></tr></table>\
           <table id='rtl'><tr><td id='rtl-a'>a</td><td id='rtl-b'>b</td></tr></table>\
         </body>",
        &format!(
            "{TABLE_RTL_CSS} #rtl {{ direction:rtl }} \
             #ltr-a, #rtl-a, #ltr-b, #rtl-b {{ width:100px }}"
        ),
        400.0,
    );
    let dom = &output.dom;
    assert_eq!(rect(&layout, find(dom, "#ltr-a")).origin.x, 0.0);
    assert_eq!(rect(&layout, find(dom, "#ltr-b")).origin.x, 100.0);
    // Column 0 is the rightmost column of the RTL table.
    assert_eq!(rect(&layout, find(dom, "#rtl-a")).origin.x, 100.0);
    assert_eq!(rect(&layout, find(dom, "#rtl-b")).origin.x, 0.0);
    // Default `text-align:start` follows the cell's inherited direction: the
    // first cell's text sits at the right edge of its box.
    let ltr_text = text_rect(&layout, "a");
    assert_eq!(ltr_text.origin.x, 0.0);
    let rtl_text = layout
        .fragments
        .iter()
        .filter(|candidate| matches!(candidate.kind, FragmentKind::Text(_)))
        .map(|candidate| candidate.rect)
        .find(|candidate_rect| {
            (candidate_rect.origin.y - rect(&layout, find(dom, "#rtl-a")).origin.y).abs() < 0.01
                && candidate_rect.origin.x >= 100.0
        })
        .expect("the RTL cell has a text run");
    assert_eq!(rtl_text.origin.x, 192.0);
}

#[test]
fn an_rtl_table_places_a_spanning_cell_over_its_logical_columns() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body>\
           <table id='ltr'><tr><td id='ltr-span' colspan='2'>s</td></tr>\
             <tr><td id='ltr-a'>a</td><td id='ltr-b'>b</td></tr></table>\
           <table id='rtl'><tr><td id='rtl-span' colspan='2'>s</td></tr>\
             <tr><td id='rtl-a'>a</td><td id='rtl-b'>b</td></tr></table>\
         </body>",
        &format!(
            "{TABLE_RTL_CSS} #rtl {{ direction:rtl }} \
             #ltr-span, #rtl-span {{ width:200px }} \
             #ltr-a, #ltr-b, #rtl-a, #rtl-b {{ width:100px }}"
        ),
        400.0,
    );
    let dom = &output.dom;
    // The spanning cell covers both 100px columns in both directions.
    assert_eq!(rect(&layout, find(dom, "#ltr-span")).origin.x, 0.0);
    assert_eq!(rect(&layout, find(dom, "#ltr-span")).size.width, 200.0);
    assert_eq!(rect(&layout, find(dom, "#rtl-span")).origin.x, 0.0);
    assert_eq!(rect(&layout, find(dom, "#rtl-span")).size.width, 200.0);
    // Column 0 is still the rightmost column under RTL.
    assert_eq!(rect(&layout, find(dom, "#rtl-a")).origin.x, 100.0);
    assert_eq!(rect(&layout, find(dom, "#rtl-b")).origin.x, 0.0);
}

#[test]
fn rtl_table_caption_text_alignment_uses_logical_start_and_end() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body>\
           <table id='start'><caption id='start-caption'>אבג</caption>\
             <tr><td id='start-cell'>x</td></tr></table>\
           <table id='end'><caption id='end-caption'>אבג</caption>\
             <tr><td id='end-cell'>x</td></tr></table>\
         </body>",
        &format!(
            "{TABLE_RTL_CSS} #start, #end {{ direction:rtl }} \
             #start-caption {{ text-align:start }} #end-caption {{ text-align:end }}"
        ),
        400.0,
    );
    let dom = &output.dom;
    // Each caption is one line of three 8px Hebrew letters; the first table's
    // caption starts at the right edge of the 200px table and the second's
    // ends at the left edge.
    assert_eq!(rect(&layout, find(dom, "#start-caption")).size.width, 200.0);
    assert_eq!(rect(&layout, find(dom, "#start-caption")).origin.y, 0.0);
    let start_text = layout
        .fragments
        .iter()
        .filter(|candidate| matches!(candidate.kind, FragmentKind::Text(_)))
        .map(|candidate| candidate.rect)
        .find(|rect| rect.origin.y == 0.0 && rect.origin.x >= 100.0)
        .expect("the start-aligned caption has a right-side text run");
    assert_eq!(start_text.origin.x, 176.0);
    let end_caption = rect(&layout, find(dom, "#end-caption"));
    assert_eq!(end_caption.size.width, 200.0);
    assert_eq!(end_caption.origin.y, 19.2 * 2.0);
    let end_text = layout
        .fragments
        .iter()
        .filter(|candidate| matches!(candidate.kind, FragmentKind::Text(_)))
        .map(|candidate| candidate.rect)
        .find(|rect| (rect.origin.y - end_caption.origin.y).abs() < 0.01)
        .expect("the end-aligned caption has a text run");
    assert_eq!(end_text.origin.x, 0.0);
}
