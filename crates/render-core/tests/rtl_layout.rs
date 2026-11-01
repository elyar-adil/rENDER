//! `direction: rtl` geometry must be the same coordinate system the painter
//! and hit tester consume.
//!
//! Layout is the only place that decides the right-edge x coordinate. These
//! tests build the display list and the interaction scene from that same
//! layout output, so a future change that mirrors layout without mirroring the
//! consumers (or the reverse) fails here instead of only on a screenshot.

#![allow(clippy::float_cmp)]

use render_core::css::cascade::{CascadeInput, CascadeOrigin};
use render_core::css::computed::{ComputationLimits, PropertyRegistry, compute_document_styles};
use render_core::css::selector::{MatchContext, parse_selector_list, select_all};
use render_core::css::stylesheet::parse_stylesheet;
use render_core::dom::{Dom, NodeId};
use render_core::html::parse_document;
use render_core::interaction::hit_test::{InteractionLimits, InteractionScene};
use render_core::layout::{
    FormattingLimits, LayoutOptions, LayoutOutput, PhysicalPoint, PhysicalRect, PhysicalSize,
    SimpleTextMeasurer, build_formatting_tree, layout_formatting_tree,
};
use render_core::paint::{
    DisplayCommand, DisplayList, DisplayListBuilderOptions, ReferenceTextShaper, build_display_list,
};

const RESET: &str = "html, body, div { display:block; margin:0; padding:0; border:0 }";

fn pipeline(html: &str, css: &str) -> (Dom, LayoutOutput, DisplayList) {
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
    let layout = layout_formatting_tree(
        &parsed.dom,
        &formatting,
        &styles,
        LayoutOptions {
            viewport: PhysicalSize {
                width: 400.0,
                height: 200.0,
            },
            ..LayoutOptions::default()
        },
        &SimpleTextMeasurer,
    );
    let display = build_display_list(
        &layout.fragments,
        &formatting,
        &styles,
        DisplayListBuilderOptions::default(),
        &ReferenceTextShaper,
    )
    .list;
    (parsed.dom, layout, display)
}

fn node(dom: &Dom, selector: &str) -> NodeId {
    let selectors = parse_selector_list(selector).expect("test selector must parse");
    select_all(dom, dom.document(), &selectors, &MatchContext::default())[0]
}

#[test]
fn an_rtl_background_box_paints_and_hit_tests_at_its_layout_x() {
    let (dom, layout, display) = pipeline(
        "<!doctype html><body><div id='container'><div id='box'></div></div></body>",
        &format!(
            "{RESET} #container {{ width:200px; height:40px; direction:rtl }} \
             #box {{ width:60px; height:20px; background-color:#ff0000 }}"
        ),
    );
    let box_node = node(&dom, "#box");
    let box_fragment = layout
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(box_node))
        .expect("box fragment");
    assert_eq!(box_fragment.rect, PhysicalRect::new(140.0, 0.0, 60.0, 20.0));

    let background = display
        .items()
        .iter()
        .find_map(
            |item| match (&item.command, item.source == Some(box_node)) {
                (DisplayCommand::SolidRect { rect, .. }, true) => Some(*rect),
                _ => None,
            },
        )
        .unwrap_or_else(|| panic!("no background command for the RTL box: {display:?}"));
    assert_eq!(background, box_fragment.rect);

    let scene = InteractionScene::new(
        &dom,
        &layout.fragments,
        &SimpleTextMeasurer,
        PhysicalPoint::default(),
        InteractionLimits::default(),
    )
    .expect("scene");
    let hit = scene
        .hit_test(PhysicalPoint { x: 150.0, y: 5.0 })
        .expect("hit test")
        .expect("the right-edge box covers this point");
    assert_eq!(hit.node, box_node);
    assert_eq!(hit.document_point, PhysicalPoint { x: 150.0, y: 5.0 });
    let outside = scene
        .hit_test(PhysicalPoint { x: 10.0, y: 5.0 })
        .expect("hit test")
        .expect("the container covers this point");
    assert_eq!(outside.node, node(&dom, "#container"));
}

#[test]
fn an_rtl_text_run_paints_and_hit_tests_at_its_layout_x() {
    let (dom, layout, display) = pipeline(
        "<!doctype html><body><div id='container'>abc</div></body>",
        &format!("{RESET} #container {{ width:200px; direction:rtl }}"),
    );
    let text_fragment = layout
        .fragments
        .iter()
        .find(|fragment| matches!(fragment.kind, render_core::layout::FragmentKind::Text(_)))
        .expect("text fragment");
    assert_eq!(
        text_fragment.rect,
        PhysicalRect::new(176.0, 0.0, 24.0, 19.2)
    );

    let first_glyph_x = display
        .items()
        .iter()
        .find_map(|item| match &item.command {
            DisplayCommand::GlyphRun(run) => run.glyphs.first().map(|glyph| glyph.position.x),
            _ => None,
        })
        .expect("the RTL text has a glyph run");
    assert_eq!(first_glyph_x, text_fragment.rect.origin.x);

    let text_node = text_fragment.source.expect("text fragment source");
    let scene = InteractionScene::new(
        &dom,
        &layout.fragments,
        &SimpleTextMeasurer,
        PhysicalPoint::default(),
        InteractionLimits::default(),
    )
    .expect("scene");
    let hit = scene
        .hit_test(PhysicalPoint { x: 180.0, y: 5.0 })
        .expect("hit test")
        .expect("the right-aligned text covers this point");
    assert_eq!(hit.node, text_node);
    assert!(hit.text_position.is_some());
}
