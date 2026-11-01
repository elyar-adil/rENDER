//! Nested browsing contexts through the page pipeline: an iframe's document
//! runs in its own realm, its `load` event reaches the page, and its rendered
//! viewport is composited into the iframe's content box.
//!
//! The child document lives in the same DOM arena as the embedding page. It is
//! laid out from its own root and rasterised into a surface image; its nodes do
//! not join the parent display list, so a parent display item keeps the iframe
//! element as its source.

#![allow(
    clippy::float_cmp,
    reason = "the iframe box and its child block are sized from exact CSS pixel lengths"
)]

use std::time::Duration;

use render_core::document::NestedFrameRender;
use render_core::dom::{NodeId, NodeKind};
use render_core::interaction::hit_test::{InteractionLimits, InteractionScene};
use render_core::js::JsValue;
use render_core::layout::{FragmentKind, PhysicalPoint, SimpleTextMeasurer};
use render_core::page::Page;
use render_core::paint::{Color, DisplayCommand};
use url::Url;

fn page(html: &str) -> Page {
    let url = Url::parse("https://example.test/app").expect("page URL");
    Page::with_url(html, &url)
}

/// Runs `source` as the next page script and returns its completion value.
fn evaluate(page: &mut Page, source: &str) -> JsValue {
    page.queue_script(source).expect("script should queue");
    let turn = page
        .run_one_turn_reference()
        .expect("turn should run")
        .expect("script turn");
    turn.executions[0]
        .result
        .as_ref()
        .expect("script result")
        .value
        .clone()
}

/// Runs an empty script, which is when the page notices its iframes, and then
/// the event loop, which loads their documents and fires their `load` events.
fn settle(page: &mut Page) {
    evaluate(page, "0");
    page.pump_until_idle_reference(Duration::ZERO)
        .expect("the page settles");
}

fn html_element(dom: &render_core::dom::Dom, name: &str) -> NodeId {
    let mut pending = vec![dom.document()];
    while let Some(node) = pending.pop() {
        if matches!(
            dom.node(node).map(render_core::dom::Node::kind),
            Some(NodeKind::Element(element))
                if element.local_name == name
        ) {
            return node;
        }
        pending.extend(dom.children(node).unwrap_or_default().iter().rev());
    }
    panic!("missing <{name}>");
}

fn iframe_node(page: &Page) -> NodeId {
    html_element(page.document().dom(), "iframe")
}

fn only_frame(page: &Page) -> &NestedFrameRender {
    let frames = &page.snapshot().frames;
    assert_eq!(frames.len(), 1, "the page should render one iframe");
    &frames[0]
}

fn child_text(page: &Page) -> Vec<String> {
    only_frame(page)
        .output
        .layout
        .fragments
        .iter()
        .filter_map(|fragment| match &fragment.kind {
            FragmentKind::Text(text) => Some(text.text.clone()),
            FragmentKind::Box(_) => None,
        })
        .collect()
}

fn frame_image_revision(page: &Page) -> Option<render_core::dom::DomRevision> {
    let iframe = iframe_node(page);
    page.snapshot()
        .display
        .list
        .items()
        .iter()
        .find_map(|item| match &item.command {
            DisplayCommand::Image(image) if item.source == Some(iframe) => image.content_revision,
            _ => None,
        })
}

fn paints_image_for(list: &render_core::paint::DisplayList, node: NodeId) -> bool {
    list.items()
        .iter()
        .any(|item| item.source == Some(node) && matches!(item.command, DisplayCommand::Image(_)))
}

#[test]
fn an_iframe_srcdoc_script_runs_in_its_own_realm() {
    let mut page = page(
        "<!doctype html><body><iframe srcdoc=\"<script>var inner = 'child';</script>\"></iframe></body>",
    );
    settle(&mut page);
    assert_eq!(
        evaluate(
            &mut page,
            "document.querySelector('iframe').contentWindow.inner"
        ),
        JsValue::String("child".into())
    );
    assert_eq!(
        evaluate(&mut page, "typeof inner"),
        JsValue::String("undefined".into())
    );
}

#[test]
fn an_iframe_load_event_reaches_the_page_after_the_child_script() {
    let mut page = page(
        "<!doctype html><body><iframe srcdoc=\"<script>var ready = true;</script>\"></iframe></body>",
    );
    evaluate(
        &mut page,
        "var seen = 'none'; document.querySelector('iframe').addEventListener('load', function () { seen = String(document.querySelector('iframe').contentWindow.ready); });",
    );
    settle(&mut page);
    assert_eq!(evaluate(&mut page, "seen"), JsValue::String("true".into()));
}

#[test]
fn a_nested_document_paints_text_inside_the_iframe_box() {
    let mut plain = page(
        "<!doctype html><style>html,body{margin:0}iframe{display:block;width:160px;height:100px;border:0}</style>\
         <body><p>page</p></body>",
    );
    let mut framed = page(
        "<!doctype html><style>html,body{margin:0}\
         iframe{display:block;width:160px;height:100px;border:0}\
         </style><body><p>page</p><iframe srcdoc=\"<p>child text</p>\"></iframe></body>",
    );
    settle(&mut plain);
    settle(&mut framed);

    let plain_items = plain.snapshot().display.list.items().len();
    let framed_items = framed.snapshot().display.list.items().len();
    assert!(
        framed_items > plain_items,
        "the iframe surface adds display items: plain {plain_items}, framed {framed_items}"
    );

    let frame = only_frame(&framed);
    assert!(
        child_text(&framed)
            .iter()
            .any(|text| text.contains("child text")),
        "the child document lays out its paragraph text: {:?}",
        child_text(&framed)
    );
    assert!(
        frame.output.display.list.items().iter().any(|item| {
            matches!(item.command, DisplayCommand::GlyphRun(_))
                && item.bounds.origin.x >= 0.0
                && item.bounds.origin.y >= 0.0
                && item.bounds.origin.x < frame.content_rect.size.width
                && item.bounds.origin.y < frame.content_rect.size.height
        }),
        "the child glyph run is inside the iframe content box"
    );
    // The parent paints the child viewport as one surface image whose source is
    // still the iframe element; no child node enters the parent display list.
    let iframe = iframe_node(&framed);
    assert!(paints_image_for(&framed.snapshot().display.list, iframe));
    let image = framed
        .snapshot()
        .display
        .list
        .items()
        .iter()
        .find(|item| {
            item.source == Some(iframe) && matches!(item.command, DisplayCommand::Image(_))
        })
        .expect("iframe surface image");
    assert_eq!(image.bounds, frame.content_rect);
}

#[test]
fn an_iframe_defaults_to_the_replaced_content_box_and_uses_html_dimensions() {
    let mut default = page(
        "<!doctype html><style>html,body{margin:0}iframe{display:block;border:0}</style>\
         <body><iframe></iframe></body>",
    );
    settle(&mut default);
    let frame = only_frame(&default);
    assert_eq!(frame.content_rect.origin.x, 0.0);
    assert_eq!(frame.content_rect.origin.y, 0.0);
    assert_eq!(frame.content_rect.size.width, 300.0);
    assert_eq!(frame.content_rect.size.height, 150.0);

    let mut sized = page(
        "<!doctype html><style>html,body{margin:0}iframe{display:block;border:0}</style>\
         <body><iframe width=180 height=90></iframe></body>",
    );
    settle(&mut sized);
    let frame = only_frame(&sized);
    assert_eq!(frame.content_rect.size.width, 180.0);
    assert_eq!(frame.content_rect.size.height, 90.0);
}

#[test]
fn child_pixels_appear_only_inside_the_iframe_content_box() {
    let mut page = page(
        r#"<!doctype html><style>
           html,body{margin:0;background:rgb(255,255,0)}
           iframe{display:block;width:120px;height:80px;
                  border:5px solid rgb(0,0,255);background:rgb(0,255,0)}
           </style><body><iframe srcdoc="<style>html,body{margin:0;background:transparent}</style><body><div style='width:40px;height:30px;background:rgb(255,0,0)'></div></body>"></iframe></body>"#,
    );
    settle(&mut page);
    let frame = only_frame(&page);
    #[allow(
        clippy::float_cmp,
        reason = "the iframe box is sized from exact CSS pixel lengths"
    )]
    {
        assert_eq!(
            frame.content_rect,
            render_core::layout::PhysicalRect::new(5.0, 5.0, 120.0, 80.0)
        );
    }

    let surface = &page.snapshot().raster.surface;
    // The child's red block begins at the content box origin.
    assert_eq!(surface.pixel(10, 10), Some(Color::rgb(255, 0, 0)));
    // The child viewport is transparent outside its block, so the iframe
    // element's green background is visible there.
    assert_eq!(surface.pixel(100, 60), Some(Color::rgb(0, 255, 0)));
    // The iframe border paints around the content box.
    assert_eq!(surface.pixel(2, 2), Some(Color::rgb(0, 0, 255)));
    // Nothing from the child escapes the iframe: the page background remains.
    assert_eq!(surface.pixel(140, 10), Some(Color::rgb(255, 255, 0)));
    assert_eq!(surface.pixel(60, 100), Some(Color::rgb(255, 255, 0)));
}

#[test]
fn a_child_script_mutation_is_painted_on_the_next_render() {
    let mut page = page(
        r#"<!doctype html><style>
           html,body{margin:0;background:rgb(255,255,255)}
           iframe{display:block;width:100px;height:60px;border:0}
           </style><body><iframe srcdoc="<style>html,body{margin:0}</style><body><div style='width:20px;height:20px;background:rgb(255,0,0)'></div><script>window.addBlock = function () { var d = document.createElement('div'); d.style = 'width:30px;height:30px;background:rgb(0,0,255)'; document.body.appendChild(d); };</script></body>"></iframe></body>"#,
    );
    settle(&mut page);
    let before_items = only_frame(&page).output.display.list.items().len();
    let before_revision = frame_image_revision(&page);
    assert_eq!(
        page.snapshot().raster.surface.pixel(10, 30),
        Some(Color::rgb(255, 255, 255)),
        "the child has only its red block before the mutation"
    );

    let result = evaluate(
        &mut page,
        "document.querySelector('iframe').contentWindow.addBlock()",
    );
    assert_eq!(result, JsValue::Undefined);
    let after = only_frame(&page);
    assert!(
        after.output.display.list.items().len() > before_items,
        "the child's inserted block adds display items"
    );
    let after_revision = after.output.revision;
    assert_ne!(
        frame_image_revision(&page),
        before_revision,
        "the parent image command carries the child revision so the diff repaints the frame"
    );
    assert_eq!(frame_image_revision(&page), Some(after_revision));
    assert_eq!(
        page.snapshot().raster.surface.pixel(10, 30),
        Some(Color::rgb(0, 0, 255)),
        "the child's blue block is re-rendered through the parent surface"
    );
}

#[test]
fn a_nested_iframe_paints_recursively() {
    let mut page = page(
        r#"<!doctype html><style>
           html,body{margin:0;background:rgb(255,255,255)}
           iframe{display:block;border:0}
           #outer{width:120px;height:80px}
           </style><body><iframe id=outer srcdoc="<style>html,body{margin:0;background:transparent}iframe{display:block;border:0;width:50px;height:40px}</style><body><iframe srcdoc=&quot;<style>html,body{margin:0;background:transparent}</style><body><div style='width:20px;height:20px;background:rgb(255,0,0)'></div></body>&quot;></iframe></body>"></iframe></body>"#,
    );
    settle(&mut page);
    let outer = only_frame(&page);
    assert_eq!(
        outer.output.frames.len(),
        1,
        "the outer frame renders its child"
    );
    let inner = &outer.output.frames[0];
    assert_eq!(inner.content_rect.size.width, 50.0);
    assert_eq!(inner.content_rect.size.height, 40.0);
    assert_eq!(
        page.snapshot().raster.surface.pixel(5, 5),
        Some(Color::rgb(255, 0, 0)),
        "the second-level frame's block reaches the page surface"
    );
}

#[test]
fn an_iframe_box_is_hit_as_the_iframe_element() {
    let mut page = page(
        "<!doctype html><style>html,body{margin:0}iframe{display:block;width:100px;height:60px;border:0}</style>\
         <body><iframe srcdoc=\"<p>child</p>\"></iframe></body>",
    );
    settle(&mut page);
    let iframe = iframe_node(&page);
    let scene = InteractionScene::new(
        page.document().dom(),
        &page.snapshot().layout.fragments,
        &SimpleTextMeasurer,
        PhysicalPoint::default(),
        InteractionLimits::default(),
    )
    .expect("the interaction scene matches the painted snapshot");

    let hit = scene
        .hit_test(PhysicalPoint { x: 10.0, y: 10.0 })
        .expect("hit test runs")
        .expect("the iframe box is hit");
    assert_eq!(hit.node, iframe);
    assert_eq!(hit.document_point, PhysicalPoint { x: 10.0, y: 10.0 });

    let outside = scene
        .hit_test(PhysicalPoint { x: 300.0, y: 300.0 })
        .expect("hit test runs");
    assert!(
        outside.is_none_or(|hit| hit.node != iframe),
        "a point outside the iframe does not hit the frame"
    );
}
