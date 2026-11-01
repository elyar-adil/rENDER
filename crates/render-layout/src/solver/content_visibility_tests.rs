//! CSS Containment 2 §4 `content-visibility`.
//!
//! `hidden` skips the element's contents while keeping the element's own box:
//! size containment makes an `auto` content height zero, and no descendant
//! fragment exists to paint. `auto` is laid out normally here because the
//! off-screen skipping optimization is not implemented yet.

#![allow(clippy::float_cmp)]

use crate::PhysicalRect;
use crate::fragment::FragmentKind;
use crate::solver::LayoutOutput;
use render_dom::NodeId;

use super::tests::{find, fragment, pipeline};

const RESET: &str = "html, body { display:block; margin:0; padding:0; border:0 } \
                     div { display:block }";

fn rect(layout: &LayoutOutput, source: NodeId) -> PhysicalRect {
    fragment(layout, source).rect
}

#[test]
fn hidden_keeps_the_box_and_skips_its_contents() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body>\
           <div id='hidden'><div id='child'>hello</div></div>\
           <div id='after'></div>\
         </body>",
        &format!(
            "{RESET} #hidden {{ content-visibility:hidden; width:100px; \
             border:5px solid black; padding:10px }} \
             #child {{ height:50px }} #after {{ height:20px }}"
        ),
        400.0,
    );
    let hidden = rect(&layout, find(&output.dom, "#hidden"));
    let after = rect(&layout, find(&output.dom, "#after"));
    // The box survives, but its auto content height is zero: 5 + 10 top and
    // bottom border/padding only.
    assert_eq!(hidden.size.height, 30.0, "{hidden:?}");
    assert_eq!(after.origin.y, 30.0, "{after:?}");
    let child = find(&output.dom, "#child");
    assert!(
        layout
            .fragments
            .iter()
            .all(|fragment| fragment.source != Some(child)),
        "the hidden child must not be laid out"
    );
    assert!(
        layout.fragments.iter().all(|fragment| {
            !matches!(
                &fragment.kind,
                FragmentKind::Text(text) if text.text.contains("hello")
            )
        }),
        "the hidden child's text must not be painted"
    );
}

#[test]
fn a_specified_height_survives_hidden_size_containment() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='hidden'><div id='child'></div></div></body>",
        &format!(
            "{RESET} #hidden {{ content-visibility:hidden; width:100px; height:80px }} \
             #child {{ height:50px }}"
        ),
        400.0,
    );
    let hidden = rect(&layout, find(&output.dom, "#hidden"));
    assert_eq!(hidden.size.height, 80.0, "{hidden:?}");
    assert!(
        layout
            .fragments
            .iter()
            .all(|fragment| fragment.source != Some(find(&output.dom, "#child"))),
        "the hidden child must not be laid out"
    );
}

#[test]
fn auto_still_lays_out_its_contents() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='auto'><div id='child'>hello</div></div></body>",
        &format!(
            "{RESET} #auto {{ content-visibility:auto; width:100px }} \
             #child {{ height:50px }}"
        ),
        400.0,
    );
    let auto = rect(&layout, find(&output.dom, "#auto"));
    let child = rect(&layout, find(&output.dom, "#child"));
    assert_eq!(auto.size.height, 50.0, "{auto:?}");
    assert_eq!(child.size.height, 50.0, "{child:?}");
    assert!(
        layout.fragments.iter().any(|fragment| {
            matches!(
                &fragment.kind,
                FragmentKind::Text(text) if text.text.contains("hello")
            )
        }),
        "auto content is rendered when it has not been skipped"
    );
}

#[test]
fn an_inline_hidden_element_contributes_no_text() {
    let (_, _, layout) = pipeline(
        "<!doctype html><body><p id='p'>aa<span id='hidden'>bb</span>cc</p></body>",
        &format!(
            "{RESET} p {{ display:block; width:200px }} \
             #hidden {{ content-visibility:hidden }}"
        ),
        400.0,
    );
    let runs = layout
        .fragments
        .iter()
        .filter_map(|fragment| match &fragment.kind {
            FragmentKind::Text(text) => Some(text.text.clone()),
            FragmentKind::Box(_) => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(runs.concat(), "aacc", "{runs:?}");
}

#[test]
fn hidden_skips_positioned_descendants_too() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body>\
           <div id='hidden'><div id='absolute'></div></div>\
         </body>",
        &format!(
            "{RESET} #hidden {{ content-visibility:hidden; width:100px }} \
             #absolute {{ position:absolute; left:0; top:0; width:10px; height:10px }}"
        ),
        400.0,
    );
    assert!(
        layout
            .fragments
            .iter()
            .all(|fragment| fragment.source != Some(find(&output.dom, "#absolute"))),
        "a hidden subtree's positioned descendants are skipped"
    );
}
