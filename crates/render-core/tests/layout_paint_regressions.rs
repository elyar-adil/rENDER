use render_core::document::{Document, DocumentRenderOptions};
use render_core::paint::{Color, DisplayCommand};

fn target_id(document: &Document, id: &str) -> render_core::dom::NodeId {
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

#[test]
fn visibility_hidden_skips_own_paint_but_allows_visible_descendants() {
    let document = Document::parse(
        "<!doctype html><style>\
         html, body { display:block; margin:0 }\
         #hidden { display:block; visibility:hidden; width:200px; height:50px; background-color:#ff0000 }\
         #shown { display:block; visibility:visible; width:100px; height:20px; background-color:#0000ff }\
         </style><div id=hidden><div id=shown>visible</div></div>",
    );
    let render = document.render_reference(DocumentRenderOptions::default());
    let hidden = target_id(&document, "hidden");
    let shown = target_id(&document, "shown");

    assert!(!render.display.list.items().iter().any(|item| {
        item.source == Some(hidden)
            && matches!(
                item.command,
                DisplayCommand::SolidRect {
                    color,
                    ..
                } if color == Color::rgb(255, 0, 0)
            )
    }));
    assert!(render.display.list.items().iter().any(|item| {
        item.source == Some(shown)
            && matches!(
                item.command,
                DisplayCommand::SolidRect {
                    color,
                    ..
                } if color == Color::rgb(0, 0, 255)
            )
    }));
}
