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
