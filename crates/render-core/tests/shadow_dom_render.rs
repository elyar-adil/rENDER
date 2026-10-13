//! The flat tree is what gets painted: a host's shadow tree is drawn, a light
//! child is drawn only where a slot takes it, and an author rule of the
//! document does not reach into a shadow tree.

use render_core::document::{Document, DocumentRenderOptions};
use render_core::dom::{NodeId, ShadowRootMode, SlotAssignmentMode};
use render_core::paint::{Color, DisplayCommand};

fn target_id(document: &Document, id: &str) -> NodeId {
    let mut pending = vec![document.dom().document()];
    while let Some(node) = pending.pop() {
        if document.dom().attribute(node, "id").ok().flatten() == Some(id) {
            return node;
        }
        pending.extend(
            document
                .dom()
                .children(node)
                .unwrap_or_default()
                .iter()
                .rev(),
        );
    }
    panic!("missing test element #{id}");
}

fn paints(list: &render_core::paint::DisplayList, source: NodeId, color: Color) -> bool {
    list.items().iter().any(|item| {
        item.source == Some(source)
            && matches!(
                item.command,
                DisplayCommand::SolidRect { color: painted, .. } if painted == color
            )
    })
}

fn paints_anything(list: &render_core::paint::DisplayList, source: NodeId) -> bool {
    list.items().iter().any(|item| item.source == Some(source))
}

#[test]
fn a_slot_paints_its_light_child_and_an_unslotted_child_paints_nothing() {
    let mut document = Document::parse(
        "<!doctype html><style>\
         html, body { display:block; margin:0 }\
         span { display:block; width:80px; height:20px; background-color:#00ff00 }\
         </style><div id=host>\
         <span id=light style=\"background-color:#ff0000\">light</span>\
         <span id=gone slot=nowhere style=\"background-color:#ff00ff\">gone</span>\
         </div>",
    );
    let host = target_id(&document, "host");
    let dom = document.dom_mut();
    let root = dom
        .attach_shadow(host, ShadowRootMode::Open, false, SlotAssignmentMode::Named)
        .expect("a div can host a shadow root");
    let shadow_span = dom.create_element("span");
    dom.append_child(root, shadow_span)
        .expect("append shadow span");
    let slot = dom.create_element("slot");
    dom.append_child(root, slot).expect("append slot");
    let shadow_text = dom.create_text("shadow");
    dom.append_child(shadow_span, shadow_text)
        .expect("append shadow text");

    let render = document.render_reference(DocumentRenderOptions::default());
    let list = &render.display.list;
    let light = target_id(&document, "light");
    let gone = target_id(&document, "gone");

    // The light child the default slot takes paints with its own style.
    assert!(paints(list, light, Color::rgb(255, 0, 0)));
    // The child no slot takes is not in the flat tree, so it is not painted.
    assert!(!paints_anything(list, gone));
    // The document's `span` rule does not reach the shadow tree's span, so the
    // shadow span paints nothing at all: it has no background of its own.
    assert!(!paints_anything(list, shadow_span));
}

#[test]
fn a_shadow_element_paints_with_its_own_inline_style() {
    let mut document = Document::parse(
        "<!doctype html><style>html, body { display:block; margin:0 }</style>\
         <div id=host></div>",
    );
    let host = target_id(&document, "host");
    let dom = document.dom_mut();
    let root = dom
        .attach_shadow(
            host,
            ShadowRootMode::Closed,
            false,
            SlotAssignmentMode::Named,
        )
        .expect("a div can host a shadow root");
    let block = dom.create_element("div");
    dom.set_attribute(
        block,
        "style",
        "display:block;width:40px;height:10px;background-color:#0000ff",
    )
    .expect("set inline style");
    dom.append_child(root, block).expect("append block");

    let render = document.render_reference(DocumentRenderOptions::default());
    assert!(paints(&render.display.list, block, Color::rgb(0, 0, 255)));
}
