//! Regression tests for CSS Overflow 3 §3.1: what layout resolves for a box
//! that clips its overflow, and the difference between a clip and a scrollport.

#![allow(clippy::float_cmp)]

use render_dom::NodeId;

use crate::geometry::PhysicalPoint;
use crate::scrollport::{ClipMode, ScrollportGeometry};
use crate::solver::LayoutOutput;
use crate::solver::tests::{find, fragment, pipeline};

const RESET: &str = "html, body, div { display: block; margin: 0; padding: 0 }";

fn near(actual: f32, expected: f32) -> bool {
    (actual - expected).abs() < 0.01
}

fn scrollport(layout: &LayoutOutput, source: NodeId) -> ScrollportGeometry {
    *layout
        .fragments
        .scrollport(fragment(layout, source).id)
        .unwrap_or_else(|| panic!("the element does not clip its overflow"))
}

#[test]
fn a_fixed_height_box_that_overflows_reports_a_scrollable_range() {
    // §3.1: `overflow: auto` makes the padding box a scrollport. The clip is the
    // padding box in document space, and the range is what the content reaches
    // past it.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='feed'>\
           <div id='a'>one</div><div id='b'>two</div><div id='c'>three</div></div></body>",
        &format!(
            "{RESET} #feed {{ width: 200px; height: 100px; overflow: auto }}\
             #a, #b, #c {{ height: 40px }}"
        ),
        400.0,
    );
    let dom = &output.dom;
    let feed = scrollport(&layout, find(dom, "#feed"));
    assert_eq!(feed.mode, ClipMode::Scrollport);
    assert!(near(feed.clip.origin.y, 0.0), "{feed:?}");
    assert!(near(feed.clip.size.width, 200.0), "{feed:?}");
    assert!(near(feed.clip.size.height, 100.0), "{feed:?}");
    // Three 40px rows in a 100px box reach 120, so the vertical range is 20.
    assert!(near(feed.scrollable.height, 120.0), "{feed:?}");
    assert_eq!(feed.max_scroll_offset(), PhysicalPoint { x: 0.0, y: 20.0 });
    assert_eq!(
        feed.clamp_scroll_offset(PhysicalPoint { x: 5.0, y: 500.0 }),
        PhysicalPoint { x: 0.0, y: 20.0 }
    );
}

#[test]
fn a_clip_is_not_a_scrollport() {
    // §3.1: `hidden` and `auto` look the same to layout - both clip - and
    // differ in whether there is anything to scroll. A clip has no range at all,
    // so it is a different mode rather than a scrollport with a zero offset.
    let (output, _, layout) = pipeline(
        "<!doctype html><body>\
           <div id='clipped'><div id='a'>one</div><div id='b'>two</div></div>\
           <div id='scrolled'><div id='c'>three</div><div id='d'>four</div></div></body>",
        &format!(
            "{RESET} #clipped, #scrolled {{ width: 200px; height: 100px }}\
             #clipped {{ overflow: hidden }} #scrolled {{ overflow: scroll }}\
             #clipped div, #scrolled div {{ height: 60px }}"
        ),
        400.0,
    );
    let dom = &output.dom;
    let clipped = scrollport(&layout, find(dom, "#clipped"));
    let scrolled = scrollport(&layout, find(dom, "#scrolled"));
    assert_eq!(clipped.mode, ClipMode::Clip);
    assert_eq!(scrolled.mode, ClipMode::Scrollport);
    // The same geometry, so nothing about the clip rectangle distinguishes them.
    assert_eq!(clipped.clip.size, scrolled.clip.size);
    // Both report the same content extent, and only one of them can be scrolled:
    // a clip's overflow is measured but unreachable, which is the whole
    // difference between the two modes.
    assert_eq!(clipped.scrollable, scrolled.scrollable);
    assert!(near(scrolled.scrollable.height, 120.0), "{scrolled:?}");
    assert_eq!(clipped.max_scroll_offset(), PhysicalPoint::default());
    assert_eq!(
        scrolled.max_scroll_offset(),
        PhysicalPoint { x: 0.0, y: 20.0 }
    );
    assert_eq!(
        clipped.clamp_scroll_offset(PhysicalPoint { x: 0.0, y: 20.0 }),
        PhysicalPoint::default()
    );
    assert_eq!(
        scrolled.clamp_scroll_offset(PhysicalPoint { x: 0.0, y: 20.0 }),
        PhysicalPoint { x: 0.0, y: 20.0 }
    );
}

#[test]
fn a_visible_overflow_clips_nothing_and_is_not_recorded() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='open'><div id='a'>one</div></div></body>",
        &format!("{RESET} #open {{ width: 200px; height: 20px; overflow: visible }}"),
        400.0,
    );
    let dom = &output.dom;
    let open = fragment(&layout, find(dom, "#open"));
    assert!(layout.fragments.scrollport(open.id).is_none());
    assert_eq!(layout.fragments.scrollports().count(), 0);
}

#[test]
fn a_nested_scrollport_does_not_enlarge_the_one_around_it() {
    // The inner box clips its own content, so that content is not reachable
    // through the outer box either: the outer's range stops at the inner box.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='outer'>\
           <div id='inner'><div id='a'>one</div><div id='b'>two</div>\
             <div id='c'>three</div></div></div></body>",
        &format!(
            "{RESET} #outer {{ width: 300px; height: 60px; overflow: auto }}\
             #inner {{ width: 200px; height: 50px; overflow: auto }}\
             #a, #b, #c {{ height: 40px }}"
        ),
        400.0,
    );
    let dom = &output.dom;
    let outer = scrollport(&layout, find(dom, "#outer"));
    let inner = scrollport(&layout, find(dom, "#inner"));
    assert!(near(inner.scrollable.height, 120.0), "{inner:?}");
    // The inner box is 50 tall and does not itself overflow the outer box, so
    // the outer range is its own height: the inner content is not counted.
    assert!(near(outer.scrollable.height, 60.0), "{outer:?}");
    assert_eq!(outer.max_scroll_offset(), PhysicalPoint::default());
}

#[test]
fn the_scrollport_rectangle_is_the_padding_box_not_the_border_box() {
    // §3.1: the scrollport is the padding box, so the border and padding of the
    // scrolling box are inside the clip and do not move it.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='feed'><div id='a'>one</div>\
           <div id='b'>two</div></div></body>",
        &format!(
            "{RESET} #feed {{ width: 200px; height: 40px; overflow: auto;\
               border: 5px solid black; padding: 7px }} #a, #b {{ height: 40px }}"
        ),
        400.0,
    );
    let dom = &output.dom;
    let box_rect = fragment(&layout, find(dom, "#feed")).rect;
    let feed = scrollport(&layout, find(dom, "#feed"));
    // The clip starts inside the border and stops before the content.
    assert!(
        near(feed.clip.origin.x, box_rect.origin.x + 5.0),
        "{feed:?}"
    );
    assert!(
        near(feed.clip.origin.y, box_rect.origin.y + 5.0),
        "{feed:?}"
    );
    // The padding box is the border box without the border: the padding is
    // inside the scrollport, not outside it.
    assert!(
        near(feed.clip.size.width, box_rect.size.width - 10.0),
        "{feed:?} {box_rect:?}"
    );
    assert!(near(feed.clip.size.height, 54.0), "{feed:?} {box_rect:?}");
    // The content is laid out inside the padding box, so what it reaches is
    // measured from the clip's own origin and includes the padding below it.
    assert!(near(feed.scrollable.height, 87.0), "{feed:?}");
}

#[test]
fn overflow_on_one_axis_only_scrolls_that_axis() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='feed'>\
           <div id='wide'>a wide row</div></div></body>",
        &format!(
            "{RESET} #feed {{ width: 100px; height: 100px; overflow-x: auto;\
               overflow-y: hidden }} #wide {{ width: 300px; height: 40px }}"
        ),
        400.0,
    );
    let dom = &output.dom;
    let feed = scrollport(&layout, find(dom, "#feed"));
    assert_eq!(feed.mode, ClipMode::Scrollport);
    assert!(
        near(feed.scrollable.width, 300.0),
        "the row is wider than the clip: {feed:?}"
    );
    assert!(
        near(feed.scrollable.height, feed.clip.size.height),
        "the vertical axis does not scroll: {feed:?}"
    );
    assert_eq!(
        feed.max_scroll_offset(),
        PhysicalPoint { x: 200.0, y: 0.0 },
        "{feed:?}: one axis scrolls, so the box is a scrollport, but not on y"
    );
}

#[test]
fn a_scrollport_reports_the_contract_a_shell_needs_to_drive_it() {
    // What the shell needs for each box: the rectangle to clip to, and the
    // range to clamp a wheel or a drag against. `hidden` gets a rectangle and no
    // range; `auto` gets both.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='hidden'><div id='a'>one</div>\
           <div id='b'>two</div></div>\
           <div id='auto'><div id='c'>three</div><div id='d'>four</div></div></body>",
        &format!(
            "{RESET} #hidden, #auto {{ width: 200px; height: 100px }}\
             #hidden {{ overflow: hidden }} #auto {{ overflow: auto }}\
             #hidden div, #auto div {{ height: 60px }}"
        ),
        400.0,
    );
    let dom = &output.dom;
    let entries: Vec<_> = layout.fragments.scrollports().collect();
    assert_eq!(entries.len(), 2, "both boxes clip, and nothing else does");
    let (hidden_id, hidden) = entries[0];
    let (_, auto) = entries[1];
    assert_eq!(hidden.mode, ClipMode::Clip);
    assert_eq!(auto.mode, ClipMode::Scrollport);
    assert_eq!(hidden_id, fragment(&layout, find(dom, "#hidden")).id);
    assert_eq!(auto.max_scroll_offset().y, 20.0, "two 60px rows: {auto:?}");
    assert_eq!(hidden.max_scroll_offset().y, 0.0);
}

#[test]
fn a_text_overflow_clip_is_still_a_clip() {
    // `text-overflow: ellipsis` needs a clipping overflow to mean anything
    // (§3.1), and it must not be mistaken for a scrollport.
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='line'>a long line of text</div></body>",
        &format!(
            "{RESET} #line {{ width: 60px; overflow: hidden; text-overflow: ellipsis;\
               white-space: nowrap }}"
        ),
        400.0,
    );
    let dom = &output.dom;
    let line = scrollport(&layout, find(dom, "#line"));
    assert_eq!(line.mode, ClipMode::Clip);
    assert_eq!(line.max_scroll_offset(), PhysicalPoint::default());
}
