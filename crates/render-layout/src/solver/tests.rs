#![allow(clippy::float_cmp)]

use crate::PhysicalRect;
use crate::fragment::FragmentKind;
use crate::tree::{FormattingLimits, build_formatting_tree};
use render_css::cascade::{CascadeInput, CascadeOrigin};
use render_css::computed::{ComputationLimits, PropertyRegistry, compute_document_styles};
use render_css::selector::{MatchContext, parse_selector_list, select_all};
use render_css::stylesheet::parse_stylesheet;
use render_html::parse_document;

use crate::solver::{
    LayoutDiagnosticCode, LayoutLimits, LayoutOptions, SimpleTextMeasurer, layout_formatting_tree,
};

fn pipeline(
    html: &str,
    css: &str,
    width: f32,
) -> (
    render_html::ParseOutput,
    std::collections::BTreeMap<render_dom::NodeId, render_css::computed::ComputedStyle>,
    crate::solver::LayoutOutput,
) {
    let output = parse_document(html);
    let sheet = parse_stylesheet(css);
    let styles = compute_document_styles(
        &output.dom,
        &[CascadeInput {
            sheet: &sheet,
            origin: CascadeOrigin::Author,
        }],
        &PropertyRegistry::standard_baseline(),
        &ComputationLimits::default(),
        &MatchContext::default(),
    );
    let formatting = build_formatting_tree(&output.dom, &styles, &FormattingLimits::default());
    let layout = layout_formatting_tree(
        &output.dom,
        &formatting,
        &styles,
        LayoutOptions {
            viewport: crate::PhysicalSize {
                width,
                height: 600.0,
            },
            ..LayoutOptions::default()
        },
        &SimpleTextMeasurer,
    );
    (output, styles, layout)
}

fn find(dom: &render_dom::Dom, selector: &str) -> render_dom::NodeId {
    let selector = parse_selector_list(selector).unwrap();
    select_all(dom, dom.document(), &selector, &MatchContext::default())[0]
}

#[test]
fn inline_text_uses_computed_font_size_and_line_height() {
    let (_, _, layout) = pipeline(
        "<!doctype html><body><p id='large'>hello</p></body>",
        "html, body, p { display:block; margin:0 } #large { font-size:24px; line-height:36px }",
        400.0,
    );
    let text = layout
        .fragments
        .iter()
        .find(|fragment| matches!(fragment.kind, FragmentKind::Text(_)))
        .expect("paragraph text fragment");
    let FragmentKind::Text(text_data) = &text.kind else {
        unreachable!();
    };
    assert_eq!(text_data.font_size, 24.0);
    assert_eq!(text.rect.size.height, 36.0);
}

#[test]
fn text_align_centers_inline_text_inside_a_fixed_width_box() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><button id=search>go</button></body>",
        "html, body, button { display:block; margin:0 } #search { width:100px; height:30px; text-align:center }",
        240.0,
    );
    let button = layout
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(find(&output.dom, "#search")))
        .expect("button fragment");
    let text_node = output.dom.children(find(&output.dom, "#search")).unwrap()[0];
    let text = layout
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(text_node))
        .expect("button text fragment");
    let expected_center = button.rect.origin.x + button.rect.size.width / 2.0;
    let actual_center = text.rect.origin.x + text.rect.size.width / 2.0;
    assert!((actual_center - expected_center).abs() < 0.01);
    assert!(text.rect.origin.x > button.rect.origin.x);
}

#[test]
fn block_width_resolves_mixed_percentages_box_sizing_and_auto_margins() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='box'></div></body>",
        "html, body { display:block; margin-left:0; margin-right:0 } #box { display:block; width:calc(50% - 20px); padding-left:10px; padding-right:10px; border-left-width:5px; border-right-width:5px; border-left-style:solid; border-right-style:solid; margin-left:auto; margin-right:auto }",
        800.0,
    );
    let box_node = find(&output.dom, "#box");
    let fragment = layout
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(box_node))
        .unwrap();
    let FragmentKind::Box(geometry) = &fragment.kind else {
        panic!("expected box fragment")
    };
    assert_eq!(geometry.content_rect.size.width, 380.0);
    assert_eq!(geometry.margin.left, 195.0);
    assert_eq!(geometry.margin.right, 195.0);
    assert_eq!(fragment.rect.size.width, 410.0);
}

#[test]
fn auto_inline_block_max_content_includes_fixed_atomic_children() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='outer'><div id='sites'><span></span><span></span><span></span><span></span><span></span><span></span><span></span><span></span></div></div></body>",
        "html, body, #outer { display:block; margin:0 } #outer { width:1190px } #sites, #sites > span { display:inline-block } #sites > span { box-sizing:border-box; width:106px; height:20px; margin-left:23px }",
        1190.0,
    );
    let sites = find(&output.dom, "#sites");
    let fragment = layout
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(sites))
        .expect("sites fragment");
    let FragmentKind::Box(geometry) = &fragment.kind else {
        panic!("expected atomic box fragment")
    };
    assert_eq!(geometry.content_rect.size.width, 1032.0);
}

#[test]
fn border_box_min_max_width_constrain_the_border_box() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='min'></div><div id='max'></div></body>",
        "html, body, div { display:block; margin:0 } div { box-sizing:border-box; padding-left:10px; padding-right:10px; border-left:5px solid; border-right:5px solid } #min { width:20px; min-width:100px } #max { width:200px; max-width:120px }",
        800.0,
    );
    let fragment_for = |selector| {
        let node = find(&output.dom, selector);
        layout
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(node))
            .expect("box fragment")
    };
    assert_eq!(fragment_for("#min").rect.size.width, 100.0);
    assert_eq!(fragment_for("#max").rect.size.width, 120.0);
}

#[test]
fn block_height_applies_min_max_and_border_box_constraints() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id=min>x</div><div id=max>x</div><div id=conflict></div><div id=border></div></body>",
        "html, body, div { display:block; margin:0 } #min { min-height:80px } #max { height:100px; max-height:40px } #conflict { height:20px; min-height:60px; max-height:40px } #border { box-sizing:border-box; min-height:50px; padding-top:10px; padding-bottom:10px; border-top:2px solid; border-bottom:2px solid }",
        800.0,
    );
    let fragment_for = |selector| {
        let node = find(&output.dom, selector);
        layout
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(node))
            .expect("box fragment")
    };

    assert_eq!(fragment_for("#min").rect.size.height, 80.0);
    assert_eq!(fragment_for("#max").rect.size.height, 40.0);
    assert_eq!(fragment_for("#conflict").rect.size.height, 60.0);
    assert_eq!(fragment_for("#border").rect.size.height, 50.0);
}

#[test]
fn legacy_163_news_display_values_keep_rows_in_normal_flow() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><ul id=news><li id=first><a>first</a></li><li id=second><a>second</a></li></ul></body>",
        "html, body, ul { display:block; margin:0 } #news li { display:-webkit-box; height:30px; line-height:30px; overflow:hidden } a { display:inline }",
        800.0,
    );
    let rect_for = |selector| {
        layout
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(find(&output.dom, selector)))
            .map(|fragment| fragment.rect)
            .expect("news row fragment")
    };

    assert_eq!(rect_for("#first").origin.y, 0.0);
    assert_eq!(rect_for("#second").origin.y, 30.0);
    assert_eq!(rect_for("#first").size.height, 30.0);
}

#[test]
fn fixed_163_columns_honor_body_min_width_and_float_containment() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id=container><div id=area><div id=left></div><div id=right></div></div></div></body>",
        "html, body { display:block; margin:0 } body { min-width:1220px } #container { display:block; width:1200px; margin-left:auto; margin-right:auto } #area { display:block; overflow:hidden } #left { display:block; float:left; width:860px; height:20px } #right { display:block; float:right; width:300px; height:20px }",
        800.0,
    );
    let rect_for = |selector| {
        layout
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(find(&output.dom, selector)))
            .map(|fragment| fragment.rect)
            .expect("163 layout fragment")
    };

    assert_eq!(rect_for("body").size.width, 1220.0);
    assert_eq!(rect_for("#container").origin.x, 10.0);
    assert_eq!(rect_for("#left").origin.x, 10.0);
    assert_eq!(rect_for("#right").origin.x, 910.0);
    assert_eq!(rect_for("#area").size.height, 20.0);
}

#[test]
fn inline_text_collapses_spaces_wraps_and_preserves_text_node_identity() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><p id='p'>hello    world世界</p></body>",
        "html, body, p { display:block; margin-left:0; margin-right:0 }",
        64.0,
    );
    let paragraph = find(&output.dom, "#p");
    let text = output.dom.children(paragraph).unwrap()[0];
    let fragments = layout
        .fragments
        .iter()
        .filter(|fragment| fragment.source == Some(text))
        .collect::<Vec<_>>();
    assert!(fragments.len() >= 2);
    let rendered = fragments
        .iter()
        .filter_map(|fragment| match &fragment.kind {
            FragmentKind::Text(text) => Some(text.text.as_str()),
            FragmentKind::Box(_) => None,
        })
        .collect::<String>();
    assert_eq!(rendered, "helloworld世界");
}

#[test]
fn ordinary_words_wrap_at_spaces_instead_of_splitting_to_fill_a_line() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><p id='p'>HOME belongs interface</p></body>",
        "html, body, p { display:block; margin-left:0; margin-right:0 }",
        80.0,
    );
    let paragraph = find(&output.dom, "#p");
    let text = output.dom.children(paragraph).unwrap()[0];
    let lines = layout
        .fragments
        .iter()
        .filter(|fragment| fragment.source == Some(text))
        .filter_map(|fragment| match &fragment.kind {
            FragmentKind::Text(text) => Some((text.text.as_str(), fragment.rect.origin.y)),
            FragmentKind::Box(_) => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(
        lines.iter().map(|(text, _)| *text).collect::<Vec<_>>(),
        ["HOME", "belongs", "interface"]
    );
    assert!(lines.windows(2).all(|lines| lines[0].1 < lines[1].1));
}

#[test]
fn a_single_overlong_word_uses_the_existing_emergency_character_wrap() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><p id='p'>abcdefgh</p></body>",
        "html, body, p { display:block; margin-left:0; margin-right:0 }",
        40.0,
    );
    let paragraph = find(&output.dom, "#p");
    let text = output.dom.children(paragraph).unwrap()[0];
    let fragments = layout
        .fragments
        .iter()
        .filter(|fragment| fragment.source == Some(text))
        .filter_map(|fragment| match &fragment.kind {
            FragmentKind::Text(text) => Some(text.text.as_str()),
            FragmentKind::Box(_) => None,
        })
        .collect::<Vec<_>>();

    assert!(fragments.len() > 1);
    assert_eq!(fragments.concat(), "abcdefgh");
}

#[test]
fn nowrap_text_overflows_a_narrow_container_without_character_wrapping() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='narrow'><span id='label'>complex question</span></div></body>",
        "html, body, div { display:block; margin:0 } #narrow { width:20px } #label { display:inline; white-space:nowrap }",
        320.0,
    );
    let label = find(&output.dom, "#label");
    let text = output.dom.children(label).unwrap()[0];
    let fragments = layout
        .fragments
        .iter()
        .filter(|fragment| fragment.source == Some(text))
        .filter_map(|fragment| match &fragment.kind {
            FragmentKind::Text(text) => Some((text.text.as_str(), fragment.rect)),
            FragmentKind::Box(_) => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(fragments.len(), 1);
    assert_eq!(fragments[0].0, "complex question");
    assert_eq!(fragments[0].1.origin.y, 0.0);
    assert!(fragments[0].1.size.width > 20.0);
}

#[test]
fn inline_blocks_are_atomic_and_preserve_their_box_model_between_text() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><p id=row>start<a id=one><span>one</span><span id=inside>inner</span></a><a id=two>two</a>end</p></body>",
        "html, body, p { display:block; margin:0 } a { display:inline-block; width:40px; height:24px; padding-left:5px; padding-right:5px; border-left-width:2px; border-left-style:solid; border-right-width:2px; border-right-style:solid } #one { background-color:red } #two { background-color:blue }",
        320.0,
    );
    let one = layout
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(find(&output.dom, "#one")))
        .expect("first atomic box");
    let two = layout
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(find(&output.dom, "#two")))
        .expect("second atomic box");
    let FragmentKind::Box(one_geometry) = &one.kind else {
        panic!("expected atomic box fragment")
    };

    assert_eq!(
        one.rect.size,
        crate::PhysicalSize {
            width: 54.0,
            height: 24.0
        }
    );
    assert_eq!(one_geometry.content_rect.size.width, 40.0);
    assert_eq!(two.rect.origin.x, one.rect.right());
    assert_eq!(one.rect.origin.y, two.rect.origin.y);
    let inside_text = output.dom.children(find(&output.dom, "#inside")).unwrap()[0];
    let inside_fragment = layout
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(inside_text))
        .expect("second inline child text");
    assert_eq!(
        inside_fragment.rect.origin.y,
        one_geometry.content_rect.origin.y
    );
    assert!(inside_fragment.rect.origin.x > one_geometry.content_rect.origin.x);
    assert!(one.children.iter().any(|child| {
        matches!(
            layout.fragments.get(*child).map(|fragment| &fragment.kind),
            Some(FragmentKind::Box(_))
        )
    }));
}

#[test]
fn inline_block_wraps_as_one_unit_when_the_line_is_full() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><p>abcdefgh<a id=tile>inside</a></p></body>",
        "html, body, p { display:block; margin:0 } #tile { display:inline-block; width:60px; height:30px; padding-top:5px; padding-right:5px; padding-bottom:5px; padding-left:5px; border-top-width:1px; border-right-width:1px; border-bottom-width:1px; border-left-width:1px; border-top-style:solid; border-right-style:solid; border-bottom-style:solid; border-left-style:solid }",
        100.0,
    );
    let tile = layout
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(find(&output.dom, "#tile")))
        .expect("atomic box");

    assert_eq!(tile.rect.origin.x, 0.0);
    assert_eq!(
        tile.rect.origin.y,
        LayoutOptions::default().default_line_height
    );
    assert_eq!(tile.rect.size.width, 72.0);
    assert_eq!(tile.rect.size.height, 42.0);
}

#[test]
fn inline_icon_title_and_badge_share_one_line() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><ul><li id=item><a id=title><i id=icon>^</i><span id=text>headline</span></a><span id=badge>hot</span></li></ul></body>",
        "html, body, ul, li { display:block; margin:0 } #item { width:369px; height:36px; clear:both; white-space:nowrap } #title { display:inline; float:left; max-width:284px; height:36px; line-height:36px } #icon { display:inline-block; width:18px; height:18px; line-height:18px } #text { font-size:16px; line-height:36px } #badge { display:inline-block; margin-left:6px; padding-left:2px; padding-right:2px; height:16px; line-height:16px; font-size:12px }",
        800.0,
    );
    let rect = |selector| {
        layout
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(find(&output.dom, selector)))
            .map(|fragment| fragment.rect)
    };

    let text = output.dom.children(find(&output.dom, "#text")).unwrap()[0];
    let text_rect = layout
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(text))
        .map(|fragment| fragment.rect)
        .expect("headline text fragment");
    assert_eq!(rect("#title").unwrap().size.width, 82.0);
    assert_eq!(rect("#icon").unwrap().origin.x, 0.0);
    assert_eq!(rect("#icon").unwrap().origin.y, 0.0);
    assert_eq!(text_rect.origin.x, 18.0);
    assert_eq!(text_rect.origin.y, 0.0);
    assert_eq!(rect("#badge").unwrap().origin.x, 88.0);
    assert_eq!(rect("#badge").unwrap().origin.y, 0.0);
}

#[test]
fn left_and_right_floats_share_a_row_and_following_block_uses_the_remaining_band() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id=left></div><div id=right></div><div id=middle></div></body>",
        "html, body, div { display:block; margin:0 } #left { float:left; width:60px; height:40px } #right { float:right; width:50px; height:30px } #middle { height:20px; background-color:red }",
        240.0,
    );
    let rect = |selector| {
        layout
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(find(&output.dom, selector)))
            .expect("box fragment")
            .rect
    };

    assert_eq!(rect("#left"), PhysicalRect::new(0.0, 0.0, 60.0, 40.0));
    assert_eq!(rect("#right"), PhysicalRect::new(190.0, 0.0, 50.0, 30.0));
    assert_eq!(rect("#middle"), PhysicalRect::new(60.0, 0.0, 130.0, 20.0));
}

#[test]
fn inline_lines_avoid_a_float_and_restore_full_width_below_it() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id=float></div>aaaaaaaaaa aaaaaaaaaa aaaaaaaaaaaaaaaa</body>",
        "html, body, div { display:block; margin:0 } #float { float:left; width:60px; height:38.4px }",
        160.0,
    );
    let text = output
        .dom
        .children(find(&output.dom, "body"))
        .unwrap()
        .iter()
        .copied()
        .find(|node| {
            matches!(
                output.dom.node(*node).map(render_dom::Node::kind),
                Some(render_dom::NodeKind::Text(_))
            )
        })
        .expect("body text node");
    let lines = layout
        .fragments
        .iter()
        .filter(|fragment| fragment.source == Some(text))
        .filter_map(|fragment| match &fragment.kind {
            FragmentKind::Text(text) => Some((text.text.as_str(), fragment.rect)),
            FragmentKind::Box(_) => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0].1.origin.x, 60.0);
    assert_eq!(lines[1].1.origin.x, 60.0);
    assert_eq!(lines[2].0, "aaaaaaaaaaaaaaaa");
    assert_eq!(lines[2].1.origin.x, 0.0);
    assert_eq!(lines[2].1.origin.y, 38.4);
    assert!(lines[2].1.size.width > 100.0);
}

#[test]
fn inline_line_advances_when_opposing_floats_leave_no_space() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id=left></div><div id=right></div>word</body>",
        "html, body, div { display:block; margin:0 } #left { float:left; width:80px; height:40px } #right { float:right; width:80px; height:20px }",
        160.0,
    );
    let body = find(&output.dom, "body");
    let text = output
        .dom
        .children(body)
        .unwrap()
        .iter()
        .copied()
        .find(|node| {
            matches!(
                output.dom.node(*node).map(render_dom::Node::kind),
                Some(render_dom::NodeKind::Text(_))
            )
        })
        .expect("body text node");
    let fragment = layout
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(text))
        .expect("text fragment");

    assert_eq!(fragment.rect.origin.x, 80.0);
    assert_eq!(fragment.rect.origin.y, 20.0);
}

#[test]
fn clear_both_moves_below_floats_and_restores_the_full_containing_width() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id=left></div><div id=right></div><div id=clear></div></body>",
        "html, body, div { display:block; margin:0 } #left { float:left; width:60px; height:40px } #right { float:right; width:50px; height:30px } #clear { clear:both; height:10px }",
        240.0,
    );
    let clear = layout
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(find(&output.dom, "#clear")))
        .expect("cleared block");

    assert_eq!(clear.rect, PhysicalRect::new(0.0, 40.0, 240.0, 10.0));
}

#[test]
fn ordinary_auto_height_excludes_floats_but_flow_root_contains_them() {
    let (ordinary_output, _, ordinary) = pipeline(
        "<!doctype html><body><div id=container><div id=float></div></div></body>",
        "html, body, div { display:block; margin:0 } #float { float:left; width:50px; height:35px }",
        200.0,
    );
    let ordinary_container = ordinary
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(find(&ordinary_output.dom, "#container")))
        .expect("ordinary container");
    assert_eq!(ordinary_container.rect.size.height, 0.0);

    let (flow_root_output, _, flow_root) = pipeline(
        "<!doctype html><body><div id=container><div id=float></div></div></body>",
        "html, body, div { display:block; margin:0 } #container { display:flow-root } #float { float:left; width:50px; height:35px }",
        200.0,
    );
    let flow_root_container = flow_root
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(find(&flow_root_output.dom, "#container")))
        .expect("flow-root container");
    assert_eq!(flow_root_container.rect.size.height, 35.0);
}

#[test]
fn fragment_tree_is_bound_to_the_dynamic_dom_revision() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><main>dynamic</main></body>",
        "html, body, main { display:block }",
        320.0,
    );
    assert_eq!(layout.fragments.dom_revision, output.dom.revision());
    assert_eq!(layout.fragments.root().as_u32(), 0);
}

#[test]
fn collapsible_whitespace_between_blocks_does_not_create_line_boxes() {
    let (output, _, layout) = pipeline(
        "<!doctype html><html><head><title>x</title></head><body>\n  <main id='content'>content</main>\n</body></html>",
        "html, body, main { display:block; margin-top:0; margin-right:0; margin-bottom:0; margin-left:0 } head, title { display:none }",
        320.0,
    );
    let main = find(&output.dom, "#content");
    let fragment = layout
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(main))
        .expect("main box fragment");

    assert_eq!(fragment.rect.origin.y, 0.0);
}

#[test]
fn br_still_creates_a_line_when_collapsible_text_is_empty() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><p id='line'><br></p></body>",
        "html, body, p { display:block; margin-top:0; margin-right:0; margin-bottom:0; margin-left:0 }",
        320.0,
    );
    let paragraph = find(&output.dom, "#line");
    let fragment = layout
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(paragraph))
        .expect("paragraph box fragment");

    assert_eq!(
        fragment.rect.size.height,
        LayoutOptions::default().default_line_height
    );
}

#[test]
fn explicit_grid_tracks_auto_place_items_with_gaps_and_box_model() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='grid'><div id='a'></div><div id='b'></div><div id='c'></div><div id='d'></div></div></body>",
        "html, body, #grid, #a, #b, #c, #d { display:block; margin:0 } #grid { display:grid; width:400px; height:115px; grid-template-columns:100px 25% 1fr; grid-template-rows:50px 1fr; column-gap:10px; row-gap:5px } #b { margin-top:5px; margin-right:5px; margin-bottom:5px; margin-left:5px; padding-left:10px; padding-right:10px; border-left-width:5px; border-right-width:5px; border-left-style:solid; border-right-style:solid }",
        400.0,
    );
    let rect = |selector| {
        layout
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(find(&output.dom, selector)))
            .unwrap()
            .rect
    };
    assert_eq!(rect("#a"), PhysicalRect::new(0.0, 0.0, 100.0, 50.0));
    assert_eq!(rect("#b"), PhysicalRect::new(115.0, 5.0, 90.0, 40.0));
    assert_eq!(rect("#c"), PhysicalRect::new(220.0, 0.0, 180.0, 50.0));
    assert_eq!(rect("#d"), PhysicalRect::new(0.0, 55.0, 100.0, 60.0));

    let b = layout
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(find(&output.dom, "#b")))
        .unwrap();
    let FragmentKind::Box(geometry) = &b.kind else {
        panic!("expected grid item box")
    };
    assert_eq!(geometry.content_rect.size.width, 60.0);
    assert_eq!(geometry.margin_rect().size.width, 100.0);
}

#[test]
fn auto_fit_minmax_grid_responds_to_available_inline_size() {
    let html = "<!doctype html><body><div id='grid'><div id='a'></div><div></div><div></div><div></div><div id='e'></div></div></body>";
    let css = "html, body, #grid, #grid > div { display:block; margin:0 } #grid { display:grid; grid-template-columns:repeat(auto-fit, minmax(140px, 1fr)); gap:10px } #grid > div { height:20px }";
    let (wide_output, _, wide) = pipeline(html, css, 620.0);
    let wide_a = wide
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(find(&wide_output.dom, "#a")))
        .unwrap();
    let wide_e = wide
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(find(&wide_output.dom, "#e")))
        .unwrap();
    assert_eq!(wide_a.rect.size.width, 147.5);
    assert_eq!(wide_e.rect.origin.y, 30.0);

    let (narrow_output, _, narrow) = pipeline(html, css, 320.0);
    let narrow_a = narrow
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(find(&narrow_output.dom, "#a")))
        .unwrap();
    let narrow_e = narrow
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(find(&narrow_output.dom, "#e")))
        .unwrap();
    assert_eq!(narrow_a.rect.size.width, 155.0);
    assert_eq!(narrow_e.rect.origin.y, 60.0);
}

#[test]
fn isolated_inline_grid_preserves_its_grid_formatting_context() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='grid'><span id='a'></span><span id='b'></span></div></body>",
        "html, body { display:block; margin:0 } #grid { display:inline-grid; width:200px; grid-template-columns:1fr 1fr } #grid > span { display:inline; height:10px }",
        300.0,
    );
    let a = layout
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(find(&output.dom, "#a")))
        .unwrap();
    let b = layout
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(find(&output.dom, "#b")))
        .unwrap();
    assert_eq!(a.rect, PhysicalRect::new(0.0, 0.0, 100.0, 10.0));
    assert_eq!(b.rect, PhysicalRect::new(100.0, 0.0, 100.0, 10.0));
}

#[test]
fn class_mutation_rebuilds_grid_geometry_for_the_new_dom_revision() {
    let mut output = parse_document(
        "<!doctype html><body><div id='grid' class='two'><div id='a'></div><div id='b'></div></div></body>",
    );
    let sheet = parse_stylesheet(
        "html, body, #grid, #a, #b { display:block; margin:0 } #grid { display:grid; width:200px } #grid.two { grid-template-columns:1fr 1fr } #grid.one { grid-template-columns:1fr } #a, #b { height:20px }",
    );
    let render = |dom: &render_dom::Dom| {
        let styles = compute_document_styles(
            dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        let formatting = build_formatting_tree(dom, &styles, &FormattingLimits::default());
        layout_formatting_tree(
            dom,
            &formatting,
            &styles,
            LayoutOptions::default(),
            &SimpleTextMeasurer,
        )
    };
    let grid = find(&output.dom, "#grid");
    let b = find(&output.dom, "#b");
    let before = render(&output.dom);
    let before_rect = before
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(b))
        .unwrap()
        .rect;

    output.dom.set_attribute(grid, "class", "one").unwrap();
    let after = render(&output.dom);
    let after_rect = after
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(b))
        .unwrap()
        .rect;
    assert_eq!(after.fragments.dom_revision, output.dom.revision());
    assert!(before_rect.origin.x > after_rect.origin.x);
    assert!(after_rect.origin.y > before_rect.origin.y);
}

#[test]
fn grid_track_limit_fails_closed_before_item_fragment_allocation() {
    let output = parse_document(
        "<!doctype html><body><div id='grid'><div id='a'></div><div></div><div></div><div></div></div></body>",
    );
    let sheet = parse_stylesheet(
        "html, body, #grid, #grid > div { display:block; margin:0 } #grid { display:grid; grid-template-columns:repeat(4, 1fr) }",
    );
    let styles = compute_document_styles(
        &output.dom,
        &[CascadeInput {
            sheet: &sheet,
            origin: CascadeOrigin::Author,
        }],
        &PropertyRegistry::standard_baseline(),
        &ComputationLimits::default(),
        &MatchContext::default(),
    );
    let formatting = build_formatting_tree(&output.dom, &styles, &FormattingLimits::default());
    let layout = layout_formatting_tree(
        &output.dom,
        &formatting,
        &styles,
        LayoutOptions {
            limits: LayoutLimits {
                max_grid_tracks: 4,
                ..LayoutLimits::default()
            },
            ..LayoutOptions::default()
        },
        &SimpleTextMeasurer,
    );
    assert!(
        layout
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == LayoutDiagnosticCode::GridTrackLimit)
    );
    assert!(
        layout
            .fragments
            .iter()
            .all(|fragment| fragment.source != Some(find(&output.dom, "#a")))
    );
}

#[test]
fn single_line_row_honors_order_gap_justification_and_cross_axis_alignment() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='flex'><span id='a'>A</span><span id='b'>B</span><span id='c'>C</span></div></body>",
        "html, body { display:block; margin:0 } #flex { display:flex; width:500px; height:100px; gap:20px; justify-content:center; align-items:center } #flex > span { width:100px; height:20px } #b { order:-1 }",
        500.0,
    );
    let a = layout
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(find(&output.dom, "#a")))
        .unwrap();
    let b = layout
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(find(&output.dom, "#b")))
        .unwrap();
    let c = layout
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(find(&output.dom, "#c")))
        .unwrap();

    assert_eq!(b.rect, PhysicalRect::new(80.0, 40.0, 100.0, 20.0));
    assert_eq!(a.rect, PhysicalRect::new(200.0, 40.0, 100.0, 20.0));
    assert_eq!(c.rect, PhysicalRect::new(320.0, 40.0, 100.0, 20.0));
}

#[test]
fn flex_grow_and_shrink_distribute_content_box_space_with_gap() {
    let (output, _, grown) = pipeline(
        "<!doctype html><body><div id='flex'><div id='a'></div><div id='b'></div></div></body>",
        "html, body, #a, #b { display:block; margin:0 } #flex { display:flex; width:300px; gap:20px } #a, #b { flex-basis:100px } #a { flex-grow:1 } #b { flex-grow:2 }",
        300.0,
    );
    let a = grown
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(find(&output.dom, "#a")))
        .unwrap();
    let b = grown
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(find(&output.dom, "#b")))
        .unwrap();
    assert!((a.rect.size.width - 126.666_67).abs() < 0.001);
    assert!((b.rect.size.width - 153.333_33).abs() < 0.001);
    assert!((b.rect.origin.x - 146.666_67).abs() < 0.001);

    let (output, _, shrunk) = pipeline(
        "<!doctype html><body><div id='flex'><div id='a'></div><div id='b'></div></div></body>",
        "html, body, #a, #b { display:block; margin:0 } #flex { display:flex; width:150px; gap:10px } #a, #b { flex-basis:100px; flex-shrink:1 }",
        150.0,
    );
    let widths = ["#a", "#b"].map(|selector| {
        shrunk
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(find(&output.dom, selector)))
            .unwrap()
            .rect
            .size
            .width
    });
    assert_eq!(widths, [70.0, 70.0]);
}

#[test]
fn flex_basis_honors_border_box_padding_and_border_constraints() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='flex'><div id='item'></div></div></body>",
        "html, body, #item { display:block; margin:0 } #flex { display:flex; width:200px; justify-content:center } #item { flex-basis:100px; box-sizing:border-box; padding-left:10px; padding-right:10px; border-left-width:5px; border-right-width:5px; border-left-style:solid; border-right-style:solid }",
        200.0,
    );
    let item = layout
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(find(&output.dom, "#item")))
        .unwrap();
    let FragmentKind::Box(geometry) = &item.kind else {
        panic!("expected flex item box")
    };
    assert_eq!(item.rect, PhysicalRect::new(50.0, 0.0, 100.0, 0.0));
    assert_eq!(geometry.content_rect.size.width, 70.0);
}

#[test]
fn definite_height_column_uses_main_axis_gap_and_alignment() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='flex'><div id='a'></div><div id='b'></div></div></body>",
        "html, body, #a, #b { display:block; margin:0 } #flex { display:flex; flex-direction:column; width:200px; height:300px; justify-content:space-between; align-items:center; row-gap:10px } #a, #b { flex-basis:50px; width:40px }",
        200.0,
    );
    let rects = ["#a", "#b"].map(|selector| {
        layout
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(find(&output.dom, selector)))
            .unwrap()
            .rect
    });
    assert_eq!(rects[0], PhysicalRect::new(80.0, 0.0, 40.0, 50.0));
    assert_eq!(rects[1], PhysicalRect::new(80.0, 250.0, 40.0, 50.0));
}

#[test]
fn flex_main_axis_auto_margins_absorb_positive_free_space() {
    let (output, _, row) = pipeline(
        "<!doctype html><body><div id='row'><div id='a'></div><div id='b'></div></div></body>",
        "html, body, #a, #b { display:block; margin:0 } #row { display:flex; width:300px } #a, #b { flex:0 0 50px } #b { margin-left:auto }",
        300.0,
    );
    let b = row
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(find(&output.dom, "#b")))
        .unwrap();
    assert_eq!(b.rect.origin.x, 250.0);

    let (output, _, column) = pipeline(
        "<!doctype html><body><div id='column'><div id='a'></div><div id='b'></div></div></body>",
        "html, body, #a, #b { display:block; margin:0 } #column { display:flex; flex-direction:column; width:100px; height:300px } #a, #b { flex:0 0 50px } #b { margin-top:auto }",
        100.0,
    );
    let b = column
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(find(&output.dom, "#b")))
        .unwrap();
    assert_eq!(b.rect.origin.y, 250.0);
}

#[test]
fn class_mutation_rebuilds_flex_geometry_for_the_new_dom_revision() {
    let mut output = parse_document(
        "<!doctype html><body><div id='flex' class='row'><div id='a'></div><div id='b'></div></div></body>",
    );
    let sheet = parse_stylesheet(
        "html, body, #a, #b { display:block; margin:0 } #flex { display:flex; width:200px; height:200px } #flex.row { flex-direction:row } #flex.column { flex-direction:column } #a, #b { flex-basis:50px }",
    );
    let render = |dom: &render_dom::Dom| {
        let styles = compute_document_styles(
            dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        let formatting = build_formatting_tree(dom, &styles, &FormattingLimits::default());
        layout_formatting_tree(
            dom,
            &formatting,
            &styles,
            LayoutOptions {
                viewport: crate::PhysicalSize {
                    width: 200.0,
                    height: 200.0,
                },
                ..LayoutOptions::default()
            },
            &SimpleTextMeasurer,
        )
    };
    let flex = find(&output.dom, "#flex");
    let b = find(&output.dom, "#b");
    let before = render(&output.dom);
    let before_rect = before
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(b))
        .unwrap()
        .rect;

    output.dom.set_attribute(flex, "class", "column").unwrap();
    let after = render(&output.dom);
    let after_rect = after
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(b))
        .unwrap()
        .rect;
    assert_eq!(after.fragments.dom_revision, output.dom.revision());
    assert!(before_rect.origin.x > after_rect.origin.x);
    assert!(after_rect.origin.y > before_rect.origin.y);
}

#[test]
fn flex_layout_stops_cleanly_at_the_fragment_limit() {
    let output = parse_document(
        "<!doctype html><body><div id='flex'><div></div><div></div><div></div><div></div></div></body>",
    );
    let sheet = parse_stylesheet("html, body, div { display:block } #flex { display:flex }");
    let styles = compute_document_styles(
        &output.dom,
        &[CascadeInput {
            sheet: &sheet,
            origin: CascadeOrigin::Author,
        }],
        &PropertyRegistry::standard_baseline(),
        &ComputationLimits::default(),
        &MatchContext::default(),
    );
    let formatting = build_formatting_tree(&output.dom, &styles, &FormattingLimits::default());
    let layout = layout_formatting_tree(
        &output.dom,
        &formatting,
        &styles,
        LayoutOptions {
            limits: LayoutLimits {
                max_fragments: 4,
                ..LayoutLimits::default()
            },
            ..LayoutOptions::default()
        },
        &SimpleTextMeasurer,
    );
    assert!(layout.fragments.iter().count() <= 4);
    assert!(
        layout
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.code == LayoutDiagnosticCode::FragmentLimit })
    );
}

#[test]
fn replaced_block_with_horizontal_auto_margins_centers_in_its_container() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><img id='logo' src='x.png' width='270' height='129'></body>",
        "html, body { display:block; margin:0 } #logo { display:block; margin:33px auto 0 auto }",
        1000.0,
    );
    let logo = layout
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(find(&output.dom, "#logo")))
        .expect("logo fragment");
    assert_eq!(logo.rect.origin.x, 365.0);
}

#[test]
fn absolute_left_50_percent_with_negative_margin_centers_the_logo() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='head'><img id='logo' src='x.png' width='270' height='129'></div></body>",
        "html, body, div { display:block; margin:0 } #head { position:relative; width:1000px; height:400px } #logo { position:absolute; bottom:10px; left:50%; margin-left:-135px }",
        1000.0,
    );
    let logo = layout
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(find(&output.dom, "#logo")))
        .expect("logo fragment");
    assert_eq!(logo.rect.origin.x, 365.0);
    assert_eq!(logo.rect.origin.y, 400.0 - 10.0 - 129.0);
}

#[test]
fn absolute_right_inset_hugs_the_containing_block_right_edge() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='head'><div id='u'><span class='b'></span></div></div></body>",
        "html, body, div { display:block; margin:0 } #head { position:relative; width:800px; height:40px } #u { position:absolute; right:10px; top:4px } #u .b { display:inline-block; width:70px; height:24px }",
        800.0,
    );
    let u = layout
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(find(&output.dom, "#u")))
        .expect("header fragment");
    assert_eq!(u.rect.size.width, 70.0);
    assert_eq!(u.rect.origin.x, 800.0 - 10.0 - 70.0);
    assert_eq!(u.rect.origin.y, 4.0);
}

#[test]
fn fixed_positioning_anchors_to_the_viewport_and_ignores_scrolling() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='side'></div></body>",
        "html, body, div { display:block; margin:0 } #side { position:fixed; right:24px; bottom:44px; width:44px; height:88px }",
        800.0,
    );
    let side = layout
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(find(&output.dom, "#side")))
        .expect("side widget fragment");
    assert_eq!(side.rect.origin.x, 800.0 - 24.0 - 44.0);
    assert_eq!(side.rect.origin.y, 600.0 - 44.0 - 88.0);
}

#[test]
fn absolute_replaced_box_is_centered_by_left_and_negative_margin_despite_text_align() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='lg'><img id='logo' src='x.png' width='270' height='129'></div></body>",
        "html, body, div { display:block; margin:0 } #lg { position:relative; width:800px; text-align:center } #logo { position:absolute; left:50%; bottom:15px; margin-left:-135px }",
        2560.0,
    );
    let logo = layout
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(find(&output.dom, "#logo")))
        .expect("logo fragment");
    assert_eq!(logo.rect.origin.x, 400.0 - 135.0);
}

#[test]
fn inline_block_input_and_submit_wrappers_share_one_full_height_row() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><form id='form'><span class='ipt'><input id='kw'></span><span class='btn'><input id='su'></span></form></body>",
        "html, body, form, input { display:block; margin:0 } .ipt, .btn { display:inline-block; vertical-align:top } .ipt { width:546px; height:44px } .btn { width:108px; height:44px }",
        800.0,
    );
    let fragment_for = |selector| {
        layout
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(find(&output.dom, selector)))
            .expect("wrapper fragment")
            .rect
    };
    let ipt = fragment_for(".ipt");
    let btn = fragment_for(".btn");
    assert_eq!(btn.origin.x, ipt.origin.x + 546.0);
    assert_eq!(btn.origin.y, ipt.origin.y);
    assert_eq!(btn.size.height, 44.0);
}

// Regression test for the zhihu.com signin page: `.SignFlowHomepage-content`
// is a block-level column flex container with `align-items:center` inside a
// stretched page shell. Its auto width must fill the containing block (the
// flex line) instead of growing to max-content, and its narrow login card
// must be centered on the cross axis.
#[test]
fn column_flex_container_fills_containing_block_and_centers_narrow_children() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='home'><div id='content'><div id='card'></div></div></div></body>",
        "html, body, div { display:block; margin:0 } \
         #home { display:flex; flex-direction:column; height:600px } \
         #content { display:flex; flex-direction:column; align-items:center; justify-content:center; flex:1 1; min-height:100% } \
         #card { width:400px; height:503px }",
        1770.0,
    );
    let fragment_for = |selector| {
        layout
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(find(&output.dom, selector)))
            .expect("fragment")
            .rect
    };
    let content = fragment_for("#content");
    assert_eq!(content.origin.x, 0.0);
    assert_eq!(content.size.width, 1770.0);
    assert_eq!(content.size.height, 600.0);
    let card = fragment_for("#card");
    assert_eq!(card.size.width, 400.0);
    assert_eq!(card.origin.x, (1770.0 - 400.0) / 2.0);
    // NOTE: browsers also center the card vertically (justify-content:center
    // against the post-flexed definite main size of the grown #content item,
    // CSS Flexbox §4.1). That requires the flexed main size to reach the
    // nested flex layout through block.rs's specified-content-height, which
    // currently only reads the `height` property; until block.rs grows a
    // forced-content-height parameter the card sticks to the top (y = 0).
    // The horizontal cross-axis centering that keeps the card on-screen is
    // asserted above and is the behavior this regression covers.
}

// `justify-content` on a `flex-direction:column` container works on the
// vertical main axis: with `align-items:center` the item stack is centered
// on the cross axis too, `center` centers the stack in the definite
// container height, and `flex-end` pushes it to the bottom edge.
#[test]
fn column_flex_justify_content_positions_items_along_the_vertical_main_axis() {
    let html =
        "<!doctype html><body><div id='col'><div id='a'></div><div id='b'></div></div></body>";
    let css = "html, body, #a, #b { display:block; margin:0 } \
         #col { display:flex; flex-direction:column; align-items:center; width:200px; height:400px } \
         #a, #b { width:40px; height:50px }";
    let pipeline_with = |justify: &str| {
        let css = css.replace("#col {", &format!("#col {{ justify-content:{justify};"));
        pipeline(html, &css, 200.0)
    };
    let rect = |layout: &crate::solver::LayoutOutput,
                output: &render_html::ParseOutput,
                selector: &str| {
        layout
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(find(&output.dom, selector)))
            .expect("column item fragment")
            .rect
    };

    let (center_output, _, center) = pipeline_with("center");
    assert_eq!(
        rect(&center, &center_output, "#a"),
        PhysicalRect::new(80.0, 150.0, 40.0, 50.0)
    );
    assert_eq!(
        rect(&center, &center_output, "#b"),
        PhysicalRect::new(80.0, 200.0, 40.0, 50.0)
    );

    let (end_output, _, end) = pipeline_with("flex-end");
    assert_eq!(
        rect(&end, &end_output, "#a"),
        PhysicalRect::new(80.0, 300.0, 40.0, 50.0)
    );
    assert_eq!(
        rect(&end, &end_output, "#b"),
        PhysicalRect::new(80.0, 350.0, 40.0, 50.0)
    );
}

// A column flex container with `align-items:center` must center an
// auto-width (max-content) child on the cross axis, and that child's
// intrinsic width must be clamped to the container line (fit-content) so
// the centered item stays inside the containing block instead of
// overflowing symmetrically off-screen.
#[test]
fn column_flex_align_center_clamps_auto_width_item_to_the_flex_line() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='column'><div id='panel'><div id='inner'></div><div id='wide'></div></div></div></body>",
        "html, body, div { display:block; margin:0 } \
         #column { display:flex; flex-direction:column; align-items:center; width:800px; height:600px } \
         #panel { display:flex; flex-direction:column; align-items:center } \
         #inner { width:200px; height:100px } \
         #wide { width:2000px; height:10px }",
        800.0,
    );
    let fragment_for = |selector| {
        layout
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(find(&output.dom, selector)))
            .expect("fragment")
            .rect
    };
    let panel = fragment_for("#panel");
    assert!(
        panel.size.width <= 800.0,
        "auto-width panel must clamp to the flex line, got {}",
        panel.size.width
    );
    assert!(panel.origin.x >= 0.0, "centered panel must stay on-screen");
    let inner = fragment_for("#inner");
    assert_eq!(inner.size.width, 200.0);
    let expected_x = panel.origin.x + (panel.size.width - 200.0) / 2.0;
    assert!(
        (inner.origin.x - expected_x).abs() < 0.5,
        "inner box must be centered within the panel"
    );
}

// CSS 2 §10.5: a percentage height against an indefinite containing height
// computes to `auto`. Regression test for the bilibili feed: the
// `.vui_carousel { height: 100% }` pattern sat inside auto-height wrappers,
// resolved against a tentative 0-height parent, and reserved a large blank
// band above the feed. It must size its content instead.
#[test]
fn percentage_height_child_of_auto_height_parent_behaves_as_auto() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id=wrap><div id=carousel><div id=slide></div></div></div></body>",
        "html, body, div { display:block; margin:0 } #slide { height:64px } #carousel { height:100% }",
        400.0,
    );
    let rect = |selector| {
        layout
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(find(&output.dom, selector)))
            .expect("carousel fragment")
            .rect
    };
    // The 100% height computes to auto, so the carousel is exactly as tall
    // as its content and reserves no extra band.
    assert_eq!(rect("#carousel").size.height, 64.0);
    assert_eq!(rect("#carousel").origin.y, 0.0);
    assert_eq!(rect("#wrap").size.height, 64.0);
}

// A child of a block with a definite height resolves its percentage height
// against that height, including through intermediate percentage levels.
#[test]
fn percentage_height_child_of_definite_parent_fills_exactly() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id=outer><div id=half><div id=leaf></div></div><div id=full></div></div></body>",
        "html, body, div { display:block; margin:0 } #outer { height:200px } #half { height:50% } #leaf { height:50% } #full { height:100% }",
        400.0,
    );
    let rect = |selector| {
        layout
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(find(&output.dom, selector)))
            .expect("outer fragment")
            .rect
    };
    assert_eq!(rect("#half").size.height, 100.0);
    // #leaf resolves against #half's definite resolved height (100px).
    assert_eq!(rect("#leaf").size.height, 50.0);
    assert_eq!(rect("#full").size.height, 200.0);
    // The specified container height wins over the overflowing flow sum.
    assert_eq!(rect("#outer").size.height, 200.0);
}

// Flex column items are in-flow boxes: their percentage heights resolve
// against the container's definite height, and compute to auto (content
// sizing) when the column container's height is indefinite.
#[test]
fn flex_column_child_percentage_height_follows_container_definiteness() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id=col><div id=a></div><div id=b></div></div></body>",
        "html, body, div { display:block; margin:0 } #col { display:flex; flex-direction:column; width:200px; height:300px } #a { height:50% } #b { height:25% }",
        200.0,
    );
    let rect = |selector| {
        layout
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(find(&output.dom, selector)))
            .expect("flex column item fragment")
            .rect
    };
    assert_eq!(rect("#a").size.height, 150.0);
    assert_eq!(rect("#b").size.height, 75.0);
    assert_eq!(rect("#b").origin.y, 150.0);

    let (indefinite_output, _, indefinite) = pipeline(
        "<!doctype html><body><div id=flow><div id=item><div id=inner></div></div></div></body>",
        "html, body, div { display:block; margin:0 } #flow { display:flex; flex-direction:column; width:200px } #item { height:100% } #inner { height:40px }",
        200.0,
    );
    let item_rect = |selector| {
        indefinite
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(find(&indefinite_output.dom, selector)))
            .expect("auto column item fragment")
            .rect
    };
    // The 100% height computes to auto, so the item hugs its content and
    // the auto-height column grows to fit it.
    assert_eq!(item_rect("#item").size.height, 40.0);
    assert_eq!(item_rect("#flow").size.height, 40.0);
}

// A column flex container with a definite width and height centers a
// fixed-size child on both axes: `align-items:center` offsets the cross
// axis (horizontal) by (line width - item width) / 2 and
// `justify-content:center` starts the main-axis (vertical) cursor at
// (container height - item height) / 2 (CSS Flexbox §9.5).
#[test]
fn column_flex_centering_offsets_child_on_both_axes() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='col'><div id='card'></div></div></body>",
        "html, body, div { display:block; margin:0 } \
         #col { display:flex; flex-direction:column; align-items:center; justify-content:center; width:1770px; height:600px } \
         #card { width:400px; height:503px }",
        1770.0,
    );
    let card = layout
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(find(&output.dom, "#card")))
        .expect("card fragment")
        .rect;
    assert_eq!(card.size.width, 400.0);
    assert_eq!(card.size.height, 503.0);
    assert_eq!(card.origin.x, (1770.0 - 400.0) / 2.0);
    assert_eq!(card.origin.y, (600.0 - 503.0) / 2.0);
}

// `align-items:stretch` with an auto cross size (width) makes a column
// flex item fill the container's width instead of hugging its content
// (CSS Flexbox §9.5 cross-axis alignment).
#[test]
fn column_flex_stretch_item_fills_the_container_width() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='col'><div id='item'></div></div></body>",
        "html, body, div { display:block; margin:0 } \
         #col { display:flex; flex-direction:column; align-items:stretch; width:800px; height:400px } \
         #item { height:120px }",
        800.0,
    );
    let item = layout
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(find(&output.dom, "#item")))
        .expect("stretched item fragment")
        .rect;
    assert_eq!(item.size.width, 800.0);
    assert_eq!(item.origin.x, 0.0);
    assert_eq!(item.size.height, 120.0);
    assert_eq!(item.origin.y, 0.0);
}

// Percentage paddings always resolve against the containing WIDTH
// (CSS 2 §10.5), so the classic `padding-top` aspect-ratio box keeps its
// width-derived size even inside auto-height ancestors. The interaction
// with this change: a `height: 100%` child inside such a padding-top box
// still computes to auto, because the padding box trick does not make the
// parent's height definite — aspect-ratio wrappers that need their content
// to fill the box must position it absolutely.
#[test]
fn padding_top_percentage_boxes_keep_width_based_resolution_under_auto_heights() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id=frame><div id=ratio><div id=fill></div></div></div></body>",
        "html, body, div { display:block; margin:0 } #ratio { padding-top:100% } #fill { height:100% }",
        240.0,
    );
    let rect = |selector| {
        layout
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(find(&output.dom, selector)))
            .expect("aspect ratio fragment")
            .rect
    };
    assert_eq!(rect("#ratio").size.height, 240.0);
    assert_eq!(rect("#frame").size.height, 240.0);
    // The percentage height of the empty fill child computes to auto.
    assert_eq!(rect("#fill").size.height, 0.0);
}

// `border: 0` is the classic reset over a user-agent border. The shorthand
// must set the border WIDTH (not be misread as a color) and reset style and
// color, so the used border size becomes zero.
#[test]
fn border_zero_shorthand_removes_a_earlier_border() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><input id='q'></body>",
        "html, body { display:block; margin:0 } input { display:inline-block; width:100px; height:30px; box-sizing:border-box; border:1px solid #888; border-style:solid } #q { border:0 }",
        400.0,
    );
    let input = find(&output.dom, "#q");
    let fragment = layout
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(input))
        .expect("input fragment");
    let FragmentKind::Box(geometry) = &fragment.kind else {
        panic!("expected box fragment")
    };
    assert_eq!(geometry.border.top, 0.0);
    assert_eq!(geometry.border.right, 0.0);
    assert_eq!(geometry.border.bottom, 0.0);
    assert_eq!(geometry.border.left, 0.0);
    // Border-box sizing keeps the full 100px as content once borders drop.
    assert_eq!(geometry.content_rect.size.width, 100.0);
}

// CSS 2.1 §4.3.2: `em` lengths refer to the element's own computed font
// size, not the root font size.
#[test]
fn em_margins_resolve_against_the_element_font_size() {
    let (output, _, layout) = pipeline(
        "<!doctype html><body><div id='t'></div></body>",
        "html, body, div { display:block; margin:0 } #t { font-size:20px; margin-left:0.5em; width:100px; height:10px }",
        400.0,
    );
    let target = find(&output.dom, "#t");
    let fragment = layout
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(target))
        .expect("target fragment");
    // 0.5em at the element's 20px font size is 10px.
    assert_eq!(fragment.rect.origin.x, 10.0);
}

// A percentage font-size computes to pixels on the parent, and the child's
// em-based font-size compounds against that absolute inherited size.
#[test]
fn font_size_compounds_through_em_inheritance() {
    let (output, styles, layout) = pipeline(
        "<!doctype html><body><div id='p'><span id='c'>x</span></div></body>",
        "html, body, div, span { display:inline; margin:0 } #p { display:block; font-size:200% } #c { font-size:1.5em }",
        400.0,
    );
    // 200% of the 16px default is 32px on the parent.
    assert_eq!(
        styles
            .get(&find(&output.dom, "#p"))
            .and_then(|style| style.get("font-size"))
            .map(render_css::computed::ComputedValue::css_text),
        Some("32px")
    );
    // The child's `1.5em` compounds against that absolute inherited size.
    assert_eq!(
        styles
            .get(&find(&output.dom, "#c"))
            .and_then(|style| style.get("font-size"))
            .map(render_css::computed::ComputedValue::css_text),
        Some("48px")
    );
    // The text laid out inside the span uses the compounded size.
    let span = find(&output.dom, "#c");
    let text_node = output.dom.children(span).expect("span text child")[0];
    let fragment = layout
        .fragments
        .iter()
        .find(|fragment| {
            fragment.source == Some(text_node) && matches!(fragment.kind, FragmentKind::Text(_))
        })
        .expect("child text fragment");
    let FragmentKind::Text(text_data) = &fragment.kind else {
        panic!("expected text fragment")
    };
    assert_eq!(text_data.font_size, 48.0);
}
