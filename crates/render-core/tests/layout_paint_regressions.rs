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

// HTML5 rendering §15.3: presentational attributes cascade at the UA origin
// so classic markup styles pages without a stylesheet, while any author rule
// still wins.
mod presentational_attributes {
    use render_core::document::{Document, DocumentRenderOptions};
    use render_core::dom::{NodeId, NodeKind};

    fn computed_background(document: &Document, selector_owner: NodeId) -> String {
        let output = document.render_reference(DocumentRenderOptions::default());
        output
            .styles
            .get(&selector_owner)
            .and_then(|style| style.get("background-color"))
            .map(|value| value.css_text().to_owned())
            .unwrap_or_default()
    }

    fn find_by_id(document: &Document, id: &str) -> NodeId {
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
        panic!("missing #{id}");
    }

    #[test]
    fn bgcolor_maps_to_background_color_at_ua_origin() {
        let document = Document::parse(
            "<!doctype html><body>\
             <table id=t bgcolor=\"#f6f6ef\"><tr><td id=cell bgcolor=\"#ff6600\">x</td></tr></table>\
             </body>",
        );
        let cell = find_by_id(&document, "cell");
        assert_eq!(
            computed_background(&document, cell).to_ascii_lowercase(),
            "#ff6600"
        );
        let table = find_by_id(&document, "t");
        assert_eq!(
            computed_background(&document, table).to_ascii_lowercase(),
            "#f6f6ef"
        );
    }

    #[test]
    fn author_rule_overrides_presentational_background() {
        let document = Document::parse(
            "<!doctype html><head><style>td { background-color: blue }</style></head>\
             <body><table><tr><td id=cell bgcolor=\"#ff6600\">x</td></tr></table></body>",
        );
        let cell = find_by_id(&document, "cell");
        assert_eq!(
            computed_background(&document, cell).to_ascii_lowercase(),
            "blue"
        );
    }

    #[test]
    fn width_and_align_attributes_apply() {
        let document = Document::parse(
            "<!doctype html><body>\
             <table id=t width=\"85%\" align=\"center\" cellpadding=\"6\">\
             <tr><td id=cell width=\"120\" align=\"center\" valign=\"top\">x</td></tr></table>\
             </body>",
        );
        let output = document.render_reference(DocumentRenderOptions::default());
        let cell = find_by_id(&document, "cell");
        let style = output.styles.get(&cell).expect("cell style");
        let get = |name: &str| {
            style
                .get(name)
                .map(|value| value.css_text().to_owned())
                .unwrap_or_default()
        };
        assert_eq!(get("width"), "120px");
        assert_eq!(get("text-align"), "center");
        assert_eq!(get("vertical-align"), "top");
        assert_eq!(get("padding-top"), "6px");
        let table = find_by_id(&document, "t");
        let style = output.styles.get(&table).expect("table style");
        let get = |name: &str| {
            style
                .get(name)
                .map(|value| value.css_text().to_owned())
                .unwrap_or_default()
        };
        assert_eq!(get("width"), "85%");
        assert_eq!(get("margin-left"), "auto");
        assert_eq!(get("margin-right"), "auto");
    }

    #[test]
    fn center_element_and_img_align_center() {
        let document = Document::parse(
            "<!doctype html><body><center><p id=p>x</p></center>\
             <img id=i src=\"a.png\" align=\"center\"></body>",
        );
        let output = document.render_reference(DocumentRenderOptions::default());
        let p = find_by_id(&document, "p");
        let style = output.styles.get(&p).expect("p style");
        let text_align = style
            .get("text-align")
            .map(|value| value.css_text().to_owned())
            .unwrap_or_default();
        assert_eq!(text_align, "center");
        let img = find_by_id(&document, "i");
        let style = output.styles.get(&img).expect("img style");
        let margin_left = style
            .get("margin-left")
            .map(|value| value.css_text().to_owned())
            .unwrap_or_default();
        assert_eq!(margin_left, "auto");
        assert!(matches!(
            document.dom().node(img).map(|n| n.kind().clone()),
            Some(NodeKind::Element(_))
        ));
    }
}
