#![allow(clippy::float_cmp)]

use render_core::css::cascade::{CascadeInput, CascadeOrigin};
use render_core::css::computed::{ComputationLimits, PropertyRegistry, compute_document_styles};
use render_core::css::selector::{MatchContext, parse_selector_list, select_all};
use render_core::css::stylesheet::parse_stylesheet;
use render_core::dom::{Dom, NodeId};
use render_core::html::parse_document;
use render_core::layout::{
    FormattingLimits, LayoutOptions, LayoutOutput, PhysicalRect, PhysicalSize, SimpleTextMeasurer,
    build_formatting_tree, layout_formatting_tree,
};

fn layout(html: &str, css: &str, viewport: PhysicalSize) -> (Dom, LayoutOutput) {
    let parsed = parse_document(html);
    let sheet = parse_stylesheet(css);
    let styles = compute_document_styles(
        &parsed.dom,
        &[CascadeInput {
            sheet: &sheet,
            origin: CascadeOrigin::Author,
        }],
        &PropertyRegistry::standard_baseline(),
        &ComputationLimits::default(),
        &MatchContext::default(),
    );
    let formatting = build_formatting_tree(&parsed.dom, &styles, &FormattingLimits::default());
    let output = layout_formatting_tree(
        &parsed.dom,
        &formatting,
        &styles,
        LayoutOptions {
            viewport,
            ..LayoutOptions::default()
        },
        &SimpleTextMeasurer,
    );
    (parsed.dom, output)
}

fn node(dom: &Dom, selector: &str) -> NodeId {
    let selectors = parse_selector_list(selector).expect("test selector must parse");
    select_all(dom, dom.document(), &selectors, &MatchContext::default())[0]
}

fn rect(dom: &Dom, output: &LayoutOutput, selector: &str) -> PhysicalRect {
    let source = node(dom, selector);
    output
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(source))
        .unwrap_or_else(|| panic!("missing fragment for {selector}"))
        .rect
}

const RESET: &str = "html, body, div { display:block; margin-top:0; margin-right:0; margin-bottom:0; margin-left:0; padding-top:0; padding-right:0; padding-bottom:0; padding-left:0 }";

#[test]
fn absolute_box_uses_nearest_positioned_ancestor_not_dom_parent() {
    let (dom, output) = layout(
        "<!doctype html><body><div id='containing'><div id='static-parent'><div id='absolute'></div></div></div></body>",
        &format!(
            "{RESET} #containing {{ position:relative; width:200px; height:100px; margin-left:40px; margin-top:20px }} #static-parent {{ width:100px; height:40px; margin-left:30px; margin-top:10px }} #absolute {{ position:absolute; left:25px; top:15px; width:40px; height:10px }}"
        ),
        PhysicalSize {
            width: 400.0,
            height: 300.0,
        },
    );

    assert_eq!(
        rect(&dom, &output, "#absolute"),
        PhysicalRect::new(65.0, 35.0, 40.0, 10.0)
    );
}

#[test]
fn absolute_box_is_out_of_flow_and_does_not_set_auto_height() {
    let (dom, output) = layout(
        "<!doctype html><body><div id='container'><div id='absolute'></div><div id='normal'></div></div><div id='after'></div></body>",
        &format!(
            "{RESET} #container {{ position:relative; width:200px }} #absolute {{ position:absolute; left:0; top:0; width:50px; height:80px }} #normal {{ height:20px }} #after {{ height:10px }}"
        ),
        PhysicalSize {
            width: 320.0,
            height: 200.0,
        },
    );

    assert_eq!(rect(&dom, &output, "#container").size.height, 20.0);
    assert_eq!(rect(&dom, &output, "#normal").origin.y, 0.0);
    assert_eq!(rect(&dom, &output, "#after").origin.y, 20.0);
}

#[test]
fn fixed_box_uses_viewport_and_does_not_advance_document_flow() {
    let viewport = PhysicalSize {
        width: 320.0,
        height: 200.0,
    };
    let (dom, output) = layout(
        "<!doctype html><body><div id='offset-parent'><div id='fixed'></div></div><div id='spacer'></div></body>",
        &format!(
            "{RESET} #offset-parent {{ position:relative; margin-left:80px; margin-top:60px }} #fixed {{ position:fixed; left:15px; top:25px; width:50px; height:30px }} #spacer {{ height:600px }}"
        ),
        viewport,
    );

    assert_eq!(
        rect(&dom, &output, "#fixed"),
        PhysicalRect::new(15.0, 25.0, 50.0, 30.0)
    );
    assert_eq!(rect(&dom, &output, "#spacer").origin.y, 60.0);
    assert_eq!(output.fragments.viewport, viewport);
    assert_eq!(output.fragments.scrollable_content_size.height, 660.0);
}

#[test]
fn overflow_hidden_contains_floats_in_auto_height() {
    let (dom, output) = layout(
        "<!doctype html><body><div id='clip'><div id='float'></div></div><div id='after'></div></body>",
        &format!(
            "{RESET} #clip {{ width:200px; overflow:hidden }} #float {{ float:left; width:60px; height:40px }} #after {{ height:10px }}"
        ),
        PhysicalSize {
            width: 320.0,
            height: 200.0,
        },
    );

    assert_eq!(rect(&dom, &output, "#clip").size.height, 40.0);
    assert_eq!(rect(&dom, &output, "#after").origin.y, 40.0);
}

#[test]
fn relative_offsets_move_the_subtree_without_changing_document_flow() {
    let (dom, output) = layout(
        "<!doctype html><body><div id='before'></div><div id='relative'><div id='absolute'></div></div><div id='after'></div></body>",
        &format!(
            "{RESET} #before {{ height:20px }} #relative {{ position:relative; left:15px; top:10px; width:100px; height:30px }} #absolute {{ position:absolute; left:0; top:0; width:10px; height:10px }} #after {{ height:20px }}"
        ),
        PhysicalSize {
            width: 320.0,
            height: 200.0,
        },
    );

    assert_eq!(
        rect(&dom, &output, "#relative"),
        PhysicalRect::new(15.0, 30.0, 100.0, 30.0)
    );
    assert_eq!(
        rect(&dom, &output, "#absolute"),
        PhysicalRect::new(15.0, 30.0, 10.0, 10.0)
    );
    assert_eq!(rect(&dom, &output, "#after").origin.y, 50.0);
}

#[test]
fn opposing_insets_stretch_auto_sized_absolute_box() {
    let (dom, output) = layout(
        "<!doctype html><body><div id='container'><div id='absolute'></div></div></body>",
        &format!(
            "{RESET} #container {{ position:relative; width:240px; height:140px; margin-left:30px; margin-top:20px }} #absolute {{ position:absolute; left:10px; right:20px; top:15px; bottom:25px }}"
        ),
        PhysicalSize {
            width: 400.0,
            height: 300.0,
        },
    );

    assert_eq!(
        rect(&dom, &output, "#absolute"),
        PhysicalRect::new(40.0, 35.0, 210.0, 100.0)
    );
}

/// CSS 2.1 §10.3.7/§10.6.4: an absolutely positioned box with `top:auto` uses
/// its static position, i.e. where it would have been in its parent's normal
/// flow. A flex container's content box starts below its `padding-top`, and the
/// nearest positioned ancestor is a different box; the containing-block padding
/// box must not replace the parent's flow origin.
#[test]
fn auto_inset_absolute_flex_child_uses_the_parent_content_edge() {
    let (dom, output) = layout(
        "<!doctype html><body><div id='header'><div id='group'><div id='search'></div></div></div></body>",
        &format!(
            "{RESET} #header {{ position:relative; height:120px }} #group {{ display:flex; padding-top:10px; height:52px }} #search {{ position:absolute; width:100px; height:20px }}"
        ),
        PhysicalSize {
            width: 400.0,
            height: 300.0,
        },
    );

    // `#group` is the flex container at the header's content top (0); its
    // content box starts at 10px. The absolute child's static position is that
    // content edge, not the header's padding-box origin at 0.
    assert_eq!(rect(&dom, &output, "#group").origin.y, 0.0);
    assert_eq!(
        rect(&dom, &output, "#search"),
        PhysicalRect::new(0.0, 10.0, 100.0, 20.0)
    );
}

/// The static position is only the fallback for `auto` insets. A specified
/// `top`/`left` still resolves against the positioned containing block's
/// padding box, which may start above the parent's content edge.
#[test]
fn explicit_insets_still_resolve_against_the_positioned_ancestor() {
    let (dom, output) = layout(
        "<!doctype html><body><div id='header'><div id='group'><div id='search'></div></div></div></body>",
        &format!(
            "{RESET} #header {{ position:relative; margin-top:40px; padding-top:6px; height:120px }} #group {{ display:flex; padding-top:10px; height:52px }} #search {{ position:absolute; left:3px; top:5px; width:100px; height:20px }}"
        ),
        PhysicalSize {
            width: 400.0,
            height: 300.0,
        },
    );

    assert_eq!(
        rect(&dom, &output, "#search"),
        PhysicalRect::new(3.0, 45.0, 100.0, 20.0)
    );
}

/// Reduced hao123 header: a white strip with a higher z-index paints over the
/// top 10px the header pulls itself up by. The search form's static position
/// must therefore start at the strip's bottom edge, or the strip erases the
/// form's top border.
#[test]
fn search_form_clears_the_header_strip_above_it() {
    let (dom, output) = layout(
        "<!doctype html><body>\
         <div id='layout-header'></div>\
         <div id='header'><div id='group'><div id='search'>\
         <form id='form'><input id='input'></form>\
         </div></div><div id='hot'></div></div>\
         <div id='nav'></div></body>",
        &format!(
            "{RESET} #layout-header {{ position:relative; z-index:555; height:86px; background:#fff }} \
             #header {{ position:relative; top:-10px; z-index:101; background:#fff; border-bottom:1px solid #ddd }} \
             #group {{ display:flex; padding-top:10px; height:52px }} \
             #search {{ position:absolute; width:942.5px; height:52px }} \
             #form {{ display:inline-block; box-sizing:border-box; width:799.5px; height:52px; border:1.5px solid transparent }} \
             #hot {{ margin:12px auto 8px; width:990px; height:52px }} \
             #nav {{ height:28px; background:#fff }}"
        ),
        PhysicalSize {
            width: 1190.0,
            height: 400.0,
        },
    );

    let layout_header = rect(&dom, &output, "#layout-header");
    let form = rect(&dom, &output, "#form");
    let search = rect(&dom, &output, "#search");
    let nav = rect(&dom, &output, "#nav");

    assert_eq!(layout_header.origin.y + layout_header.size.height, 86.0);
    assert_eq!(search.origin.y, 86.0);
    assert_eq!(form.origin.y, 86.0);
    assert_eq!(form.size.height, 52.0);
    assert_eq!(form.origin.y + form.size.height, 138.0);
    // Header layout starts at 86, is 62 (group) + 62 (hot + margins) tall, and
    // is shifted up 10px for painting; the nav follows its layout bottom.
    assert_eq!(nav.origin.y, 221.0);
    assert!(form.origin.y >= layout_header.origin.y + layout_header.size.height);
}
