//! Regression tests for CSS Positioned Layout 4 §4.1 sticky positioning.
//!
//! Layout cannot apply the displacement itself - it is the one positioning
//! feature whose result depends on where the page is scrolled to, and the
//! fragment tree is built in document space and translated when it paints. The
//! tests therefore assert the constraint layout resolves, and the displacement
//! [`crate::sticky_offset`] derives from it.

#![allow(clippy::float_cmp)]

use render_dom::NodeId;

use crate::PhysicalRect;
use crate::geometry::PhysicalPoint;
use crate::solver::LayoutOutput;
use crate::sticky::StickyConstraint;
use crate::sticky::sticky_offset;

use super::tests::{find, fragment, pipeline};

const RESET: &str = "html, body, div, p { display: block; margin: 0; padding: 0 }";

fn rect(layout: &LayoutOutput, source: NodeId) -> PhysicalRect {
    fragment(layout, source).rect
}

fn near(actual: f32, expected: f32) -> bool {
    (actual - expected).abs() < 0.01
}

/// The sticky constraint of an element, if layout resolved one.
fn constraint(layout: &LayoutOutput, source: NodeId) -> StickyConstraint {
    *layout
        .fragments
        .sticky_constraint(fragment(layout, source).id)
        .unwrap_or_else(|| panic!("the element has no sticky constraint"))
}

/// Where the element is painted, in document space, for a scroll offset.
fn painted(constraint: &StickyConstraint, scroll: PhysicalPoint) -> PhysicalRect {
    let offset = sticky_offset(constraint, scroll);
    PhysicalRect::new(
        constraint.margin_rect.origin.x + offset.x,
        constraint.margin_rect.origin.y + offset.y,
        constraint.margin_rect.size.width,
        constraint.margin_rect.size.height,
    )
}

fn scrolled(x: f32, y: f32) -> PhysicalPoint {
    PhysicalPoint { x, y }
}

#[test]
fn a_sticky_box_sticks_to_the_top_of_the_scrollport() {
    // §4.1: a box with `top: 0` is displaced down as the page scrolls, so it
    // stays at the top of the scrollport.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='page'>\
           <div id='bar'>header</div><div id='rest'>body</div></div></body>",
        &format!(
            "{RESET} #page {{ height: 2000px }} #bar {{ position: sticky; top: 0; height: 50px }}\
             #rest {{ height: 1950px }}"
        ),
        400.0,
    );
    let dom = &output.dom;
    let bar = constraint(&layout, find(dom, "#bar"));
    // Unscrolled, the box is where layout put it.
    assert!(
        near(painted(&bar, scrolled(0.0, 0.0)).origin.y, 0.0),
        "{bar:?}"
    );
    // Scrolled to 100 and 1000 it sits at the top of the 600px scrollport.
    assert!(
        near(painted(&bar, scrolled(0.0, 100.0)).origin.y, 100.0),
        "{bar:?}"
    );
    assert!(
        near(painted(&bar, scrolled(0.0, 1000.0)).origin.y, 1000.0),
        "{bar:?}"
    );
    // The constraint is the containing block, not the scrollport: the 2000px
    // page bounds the displacement.
    assert!(near(bar.containing_block.size.height, 2000.0), "{bar:?}");
    assert!(near(bar.scrollport.height, 600.0), "{bar:?}");
    assert_eq!(bar.insets.top, Some(0.0));
    assert_eq!(bar.insets.bottom, None);
}

#[test]
fn a_sticky_box_is_never_moved_out_of_its_containing_block() {
    // §4.1: the box is constrained to its containing block, so a sticky element
    // in a parent that is not as tall as the scrollport stops at the parent's
    // bottom edge rather than pinning itself to the top of the scrollport.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='page'><div id='panel'>\
           <div id='bar'>header</div></div></div></body>",
        &format!(
            "{RESET} #page {{ height: 3000px }} #panel {{ height: 900px }}\
             #bar {{ position: sticky; top: 0; height: 50px }}"
        ),
        400.0,
    );
    let dom = &output.dom;
    let panel = rect(&layout, find(dom, "#panel"));
    let bar = constraint(&layout, find(dom, "#bar"));
    assert!(near(panel.size.height, 900.0), "{panel:?}");
    // While the panel extends past the bottom of the scrollport, the box sticks.
    assert!(
        near(painted(&bar, scrolled(0.0, 0.0)).origin.y, 0.0),
        "{bar:?}"
    );
    assert!(
        near(painted(&bar, scrolled(0.0, 100.0)).origin.y, 100.0),
        "{bar:?}"
    );
    // At 880 the scrollport's top is inside the panel, so the box stops at the
    // panel's bottom edge: 900 - 50.
    assert!(
        near(
            painted(&bar, scrolled(0.0, 880.0)).origin.y,
            panel.bottom() - 50.0
        ),
        "{bar:?} {panel:?}"
    );
}

#[test]
fn a_sticky_box_that_has_been_scrolled_past_is_not_pinned_to_the_scrollport() {
    // §4.1: the box is only displaced while its containing block is inside the
    // scrollport. Once the whole parent has been scrolled past, the box scrolls
    // away with it instead of staying stuck to the top of the scrollport.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='page'><div id='panel'>\
           <div id='bar'>header</div></div></div></body>",
        &format!(
            "{RESET} #page {{ height: 3000px }} #panel {{ height: 200px }}\
             #bar {{ position: sticky; top: 0; height: 50px }}"
        ),
        400.0,
    );
    let dom = &output.dom;
    let bar = constraint(&layout, find(dom, "#bar"));
    // The parent is still partly visible, so the box sticks.
    assert!(
        near(painted(&bar, scrolled(0.0, 100.0)).origin.y, 100.0),
        "{bar:?}"
    );
    // The parent is entirely above the scrollport, so the box goes with it.
    assert_eq!(
        sticky_offset(&bar, scrolled(0.0, 300.0)),
        PhysicalPoint::default(),
        "{bar:?}"
    );
}

#[test]
fn a_sticky_box_further_down_the_page_is_not_dragged_into_view() {
    // The same rule from the other end: a `top` inset only constrains the top
    // edge, so it never pulls a box up towards the scrollport, and a box whose
    // containing block the scrollport has not reached is not displaced at all.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='page'>\
           <div id='spacer'>above</div>\
           <div id='bar'>header</div></div></body>",
        &format!(
            "{RESET} #page {{ height: 3000px }} #spacer {{ height: 1000px }}\
             #bar {{ position: sticky; top: 0; height: 50px }}"
        ),
        400.0,
    );
    let dom = &output.dom;
    let bar = constraint(&layout, find(dom, "#bar"));
    assert!(near(bar.margin_rect.origin.y, 1000.0), "{bar:?}");
    assert_eq!(
        sticky_offset(&bar, scrolled(0.0, 0.0)),
        PhysicalPoint::default(),
        "{bar:?}"
    );
    // Once the scrollport reaches it, it sticks like any other header.
    assert!(
        near(painted(&bar, scrolled(0.0, 1200.0)).origin.y, 1200.0),
        "{bar:?}"
    );
}

#[test]
fn a_sticky_box_larger_than_its_containing_block_is_left_alone() {
    // The two edges of the constraint cannot both be satisfied, so the box is
    // not displaced on that axis at all.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='page'><div id='panel'>\
           <div id='bar'>header</div></div></div></body>",
        &format!(
            "{RESET} #page {{ height: 3000px }} #panel {{ height: 900px }}\
             #bar {{ position: sticky; top: 0; height: 1500px }}"
        ),
        400.0,
    );
    let dom = &output.dom;
    let bar = constraint(&layout, find(dom, "#bar"));
    assert_eq!(
        sticky_offset(&bar, scrolled(0.0, 300.0)),
        PhysicalPoint::default()
    );
}

#[test]
fn a_top_inset_holds_the_sticky_box_below_the_top_of_the_scrollport() {
    // §4.1: the insets form the sticky view rectangle, so `top: 10px` holds the
    // box 10px below the top of the scrollport - including at rest, where the
    // box's top edge is already violating that rectangle.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='page'><div id='bar'>header</div></div></body>",
        &format!(
            "{RESET} #page {{ height: 2000px }}\
             #bar {{ position: sticky; top: 10px; height: 50px }}"
        ),
        400.0,
    );
    let dom = &output.dom;
    let bar = constraint(&layout, find(dom, "#bar"));
    assert_eq!(bar.insets.top, Some(10.0));
    assert_eq!(bar.insets.bottom, None);
    assert!(
        near(painted(&bar, scrolled(0.0, 0.0)).origin.y, 10.0),
        "{bar:?}"
    );
    assert!(
        near(painted(&bar, scrolled(0.0, 200.0)).origin.y, 210.0),
        "{bar:?}"
    );
}

#[test]
fn a_vertical_scrollbar_does_not_move_a_top_constrained_box_horizontally() {
    // The offset applies in the scrollport's coordinate space and each axis is
    // constrained on its own. A document that is both taller and wider than the
    // viewport produces a vertical scrollbar; a `top`-constrained box has no
    // horizontal inset, so nothing on that axis may move it, at any scroll
    // offset.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='page'>\
           <div id='bar'>header</div></div></body>",
        &format!(
            "{RESET} #page {{ width: 800px; height: 2000px }}\
             #bar {{ position: sticky; top: 0; width: 300px; height: 50px }}"
        ),
        400.0,
    );
    let dom = &output.dom;
    let bar = constraint(&layout, find(dom, "#bar"));
    assert!(near(bar.scrollport.width, 400.0), "{bar:?}");
    for y in [0.0_f32, 1.0, 250.0, 1400.0] {
        let offset = sticky_offset(&bar, scrolled(0.0, y));
        assert!(near(offset.x, 0.0), "scrolled to {y}: {bar:?}");
        assert!(near(offset.y, y), "scrolled to {y}: {bar:?}");
    }
}

#[test]
fn a_left_inset_constrains_the_box_horizontally_and_leaves_it_vertical() {
    // The mirror image: the inset is honoured on the axis it is written for, and
    // the other axis is unconstrained rather than guessed at.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='page'>\
           <div id='bar'>header</div></div></body>",
        &format!(
            "{RESET} #page {{ width: 800px; height: 2000px }}\
             #bar {{ position: sticky; left: 20px; width: 300px; height: 50px }}"
        ),
        400.0,
    );
    let dom = &output.dom;
    let bar = constraint(&layout, find(dom, "#bar"));
    assert_eq!(bar.insets.left, Some(20.0));
    assert_eq!(bar.insets.top, None);
    assert!(
        near(bar.margin_rect.size.width, 300.0),
        "{bar:?} (a box as wide as its containing block could never move)"
    );
    for y in [0.0_f32, 200.0, 900.0] {
        let offset = sticky_offset(&bar, scrolled(0.0, y));
        assert!(near(offset.x, 20.0), "scrolled to {y}: {bar:?}");
        assert!(near(offset.y, 0.0), "scrolled to {y}: {bar:?}");
    }
}

#[test]
fn a_bottom_inset_holds_the_sticky_box_above_the_bottom_of_the_scrollport() {
    // §4.1: `bottom` constrains the box's *end* edge. A box with `bottom: 0`
    // further down the page than the scrollport reaches is displaced up, so it
    // sits against the bottom of the scrollport instead of below it. This is
    // the end-axis half of the rule; every other sticky fixture here writes
    // `top` or `left`, so the `(None, Some(_))` arm had no test at all.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='page'>\
           <div id='lead'>body</div><div id='foot'>footer</div></div></body>",
        &format!(
            "{RESET} #page {{ height: 2000px }} #lead {{ height: 1400px }}\
             #foot {{ position: sticky; bottom: 0; height: 50px }}"
        ),
        400.0,
    );
    let dom = &output.dom;
    let foot = constraint(&layout, find(dom, "#foot"));
    assert_eq!(foot.insets.bottom, Some(0.0));
    assert_eq!(foot.insets.top, None);
    // Layout left the box where it flows, at 1400..1450 in the document.
    assert!(near(foot.margin_rect.origin.y, 1400.0), "{foot:?}");
    assert!(near(foot.containing_block.size.height, 2000.0), "{foot:?}");

    // Unscrolled the footer is 850px past the bottom of the 600px scrollport,
    // so it is pulled up by exactly that much and its bottom edge lands on
    // 600, the bottom of the view rectangle.
    let unpainted = foot.margin_rect;
    let pulled = painted(&foot, scrolled(0.0, 0.0));
    assert!(near(pulled.origin.y, 550.0), "{pulled:?} {unpainted:?}");
    assert!(
        near(pulled.origin.y + pulled.size.height, 600.0),
        "{pulled:?}"
    );
    assert!(
        near(sticky_offset(&foot, scrolled(0.0, 0.0)).y, -850.0),
        "{foot:?}"
    );

    // At 850 the scrollport's bottom edge has reached the footer's own, so the
    // displacement is exactly zero and the two agree.
    assert!(
        near(painted(&foot, scrolled(0.0, 850.0)).origin.y, 1400.0),
        "{foot:?}"
    );

    // Past that the footer is entirely inside the scrollport and has nothing
    // left to be constrained against: an end inset only ever pulls a box back
    // towards the view, it must never push it further down. A resolution that
    // resolved the end edge in the wrong direction would add 50px here.
    assert!(
        near(painted(&foot, scrolled(0.0, 900.0)).origin.y, 1400.0),
        "{foot:?}"
    );
    assert!(
        near(painted(&foot, scrolled(0.0, 1400.0)).origin.y, 1400.0),
        "{foot:?}"
    );

    // The containing block still bounds the travel: once the page's own bottom
    // is above the scrollport's top the box is left entirely alone.
    assert!(
        near(painted(&foot, scrolled(0.0, 2000.0)).origin.y, 1400.0),
        "{foot:?}"
    );
}

#[test]
fn a_right_inset_holds_the_sticky_box_against_the_right_of_the_scrollport() {
    // §4.1: `right` is `left`'s mirror on the horizontal axis, and it is the
    // horizontal `(None, Some(_))` arm - a right rail that stays put while the
    // page is scrolled sideways.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='page'>\
           <div id='rail'>rail</div></div></body>",
        &format!(
            "{RESET} #page {{ width: 900px; height: 600px }}\
             #rail {{ position: sticky; right: 0; float: right; width: 120px; height: 400px }}"
        ),
        400.0,
    );
    let dom = &output.dom;
    let rail = constraint(&layout, find(dom, "#rail"));
    assert_eq!(rail.insets.right, Some(0.0));
    assert_eq!(rail.insets.left, None);
    // A box as wide as its containing block could never move, so the rail is
    // given a margin that leaves room to be pulled left.
    assert!(near(rail.margin_rect.origin.x, 780.0), "{rail:?}");

    // Unscrolled the rail's right edge is 900, 500px past the right of the
    // 400px scrollport, so it is pulled left to 400 - 120 = 280.
    let pulled = painted(&rail, scrolled(0.0, 0.0));
    assert!(near(pulled.origin.x, 280.0), "{pulled:?}");
    assert!(
        near(pulled.origin.x + pulled.size.width, 400.0),
        "{pulled:?}"
    );

    // At 500 the scrollport's right edge has reached the rail's own.
    assert!(
        near(painted(&rail, scrolled(500.0, 0.0)).origin.x, 780.0),
        "{rail:?}"
    );
    // Past that the rail is inside the scrollport and is never pushed further
    // right, which is the direction the end inset must not act in.
    assert!(
        near(painted(&rail, scrolled(600.0, 0.0)).origin.x, 780.0),
        "{rail:?}"
    );

    // The other axis is unconstrained: a vertical scroll moves it not at all.
    for x in [0.0_f32, 200.0, 500.0] {
        assert!(
            near(painted(&rail, scrolled(x, 250.0)).origin.y, 0.0),
            "x={x}: {rail:?}"
        );
    }
}

#[test]
fn a_box_whose_computed_position_is_not_sticky_is_never_moved() {
    // `top` on a static or relatively positioned box is not a sticky inset, so
    // there is no constraint to apply at all.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='page'>\
           <div id='static'>a</div><div id='relative'>b</div>\
           <div id='sticky'>c</div></div></body>",
        &format!(
            "{RESET} #page {{ height: 2000px }}\
             #static, #relative, #sticky {{ height: 50px }}\
             #static {{ top: 0 }} #relative {{ position: relative; top: 0 }}\
             #sticky {{ position: sticky; top: 0 }}"
        ),
        400.0,
    );
    let dom = &output.dom;
    let sticky = fragment(&layout, find(dom, "#sticky")).id;
    assert!(
        layout.fragments.sticky_constraint(sticky).is_some(),
        "a sticky box has a constraint"
    );
    for selector in ["#static", "#relative"] {
        let id = fragment(&layout, find(dom, selector)).id;
        assert!(
            layout.fragments.sticky_constraint(id).is_none(),
            "{selector} has no sticky constraint"
        );
    }
    assert_eq!(
        layout.fragments.sticky_fragments().count(),
        1,
        "only the sticky box is listed"
    );
    // A sticky box keeps the position layout gave it: sticky displaces the
    // painted position only. It is the third of three 50px boxes.
    assert!(near(rect(&layout, find(dom, "#static")).origin.y, 0.0));
    assert!(near(rect(&layout, find(dom, "#relative")).origin.y, 50.0));
    assert!(near(rect(&layout, find(dom, "#sticky")).origin.y, 100.0));
}

#[test]
fn sticky_does_not_change_the_paint_order_of_a_box() {
    // §4.1: sticky does not create a stacking context, so it neither escapes its
    // parent in the fragment tree nor is re-ordered against its siblings.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='page'><div id='first'>a</div>\
           <div id='sticky'>b</div><div id='last'>c</div></div></body>",
        &format!(
            "{RESET} #page {{ height: 2000px }} #first, #sticky, #last {{ height: 50px }}\
             #sticky {{ position: sticky; top: 0 }}"
        ),
        400.0,
    );
    let dom = &output.dom;
    assert!(near(rect(&layout, find(dom, "#first")).origin.y, 0.0));
    assert!(near(rect(&layout, find(dom, "#sticky")).origin.y, 50.0));
    assert!(near(rect(&layout, find(dom, "#last")).origin.y, 100.0));
    let page = fragment(&layout, find(dom, "#page"));
    let order: Vec<NodeId> = page
        .children
        .iter()
        .filter_map(|child| layout.fragments.get(*child))
        .filter_map(|child| child.source)
        .collect();
    assert_eq!(
        order,
        vec![
            find(dom, "#first"),
            find(dom, "#sticky"),
            find(dom, "#last")
        ]
    );
}

#[test]
fn a_sticky_box_inside_a_positioned_ancestor_is_constrained_by_final_geometry() {
    // A relatively positioned ancestor moves its whole subtree after the sticky
    // box inside it is finished, so the constraint has to be resolved from the
    // finished tree. The ancestor sits 60px down and is 400px tall, so the box
    // may travel 350px - to the ancestor's bottom edge - and no further.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='page'><div id='shift'>\
           <div id='bar'>header</div></div></div></body>",
        &format!(
            "{RESET} #page {{ height: 3000px }}\
             #shift {{ position: relative; top: 60px; height: 400px }}\
             #bar {{ position: sticky; top: 0; height: 50px }}"
        ),
        400.0,
    );
    let dom = &output.dom;
    let shift = rect(&layout, find(dom, "#shift"));
    let bar = constraint(&layout, find(dom, "#bar"));
    assert!(near(shift.origin.y, 60.0), "{shift:?}");
    assert!(near(bar.margin_rect.origin.y, 60.0), "{bar:?}");
    assert!(near(bar.containing_block.origin.y, 60.0), "{bar:?}");
    // At 430 the scrollport's top is inside the ancestor, and the box has run out
    // of containing block: it sits against the ancestor's bottom edge.
    let at_edge = painted(&bar, scrolled(0.0, 430.0));
    assert!(
        near(bar.containing_block.origin.y, shift.origin.y),
        "{bar:?}"
    );
    assert!(
        near(at_edge.origin.y, shift.bottom() - 50.0),
        "{shift:?} {bar:?}"
    );
    // While there is room, it sticks to the top of the scrollport instead.
    assert!(
        near(painted(&bar, scrolled(0.0, 300.0)).origin.y, 300.0),
        "{bar:?}"
    );
}

#[test]
fn a_sticky_box_in_a_table_cell_is_constrained_by_the_cell() {
    // A cell is the nearest block container of its content, and `vertical-align`
    // moves that content after it is laid out, so both the margin box and the
    // constraint have to come from the finished tree.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><table id='t'><tbody><tr>\
           <td id='c' style='vertical-align:bottom'><div id='bar'>header</div></td>\
           <td id='other'>x</td></tr></tbody></table></body>",
        "html, body, table, tbody, tr, td, div { display: block; margin: 0; padding: 0 }\
         table { display: table; border-spacing: 0 } tbody { display: table-row-group }\
         tr { display: table-row } td { display: table-cell; width: 200px; height: 200px }\
         #bar { position: sticky; top: 0; height: 50px }",
        400.0,
    );
    let dom = &output.dom;
    let cell = rect(&layout, find(dom, "#c"));
    let bar = constraint(&layout, find(dom, "#bar"));
    // The cell is the constraint, not the row and not the page.
    assert!(near(cell.size.height, 200.0), "{cell:?}");
    assert!(
        near(bar.containing_block.size.height, 200.0),
        "{bar:?} {cell:?}"
    );
    assert!(
        near(bar.containing_block.origin.y, cell.origin.y),
        "{bar:?}"
    );
    // The box's own position reflects the cell's `vertical-align` offset.
    assert!(
        near(bar.margin_rect.bottom(), cell.bottom()),
        "{bar:?} {cell:?}"
    );
    // It is already at the bottom of its containing block, so there is nowhere
    // for it to travel.
    assert_eq!(
        sticky_offset(&bar, scrolled(0.0, 100.0)),
        PhysicalPoint::default()
    );
}
