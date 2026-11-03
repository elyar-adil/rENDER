#![allow(clippy::float_cmp)]

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

#[test]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the button fragment has a non-negative integral position inside the test surface"
)]
fn an_absolute_button_in_an_overflow_hidden_form_field_is_not_clipped_by_its_line_box() {
    // The button is statically placed in an anonymous line box after the
    // input. That wrapper inherits the parent's style source only for
    // inheritable properties; `overflow:hidden` on the field must clip the
    // field, not an anonymous line box at the button's static position.
    let document = Document::parse(
        "<!doctype html><style>\
         html, body { display:block; margin:0 }\
         #field { position:relative; display:block; width:352px; height:62px; overflow:hidden }\
         #spacer { display:block; height:44px }\
         #button { position:absolute; right:0; top:24px; transform:translateY(-50%);\
                   display:inline-block; border:0; padding:0; width:98px; height:24px;\
                   background:#ffff00 }\
         </style><div id=field><div id=spacer></div><button id=button></button></div>",
    );
    let render = document.render_reference(DocumentRenderOptions::default());
    let button = target_id(&document, "button");
    let fragment = render
        .layout
        .fragments
        .iter()
        .find(|fragment| fragment.source == Some(button))
        .expect("the button must be laid out");
    let rect = fragment.rect;
    let pixel = render
        .raster
        .surface
        .pixel(
            u32::try_from(rect.origin.x as i32 + 2).expect("inside the surface"),
            u32::try_from(rect.origin.y as i32 + 2).expect("inside the surface"),
        )
        .expect("the button is inside the surface");
    assert_eq!((pixel.red, pixel.green, pixel.blue), (255, 255, 0));
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

/// The real header pulls itself up by 10px under a higher-z-index white strip.
/// An absolute search box with `top:auto` must use the flow static position -
/// the flex container's content edge, below its `padding-top` - or it moves
/// under the strip and loses the top edge of its multi-layer border.
#[test]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "test geometry is non-negative and inside the render surface"
)]
fn search_form_top_border_survives_the_higher_header_strip() {
    use render_core::layout::PhysicalSize;

    let document = Document::parse(
        "<!doctype html><style>\
         html, body, div, form { display:block; margin:0; padding:0 }\
         #layout-header { position:relative; z-index:555; height:86px; background:#fff }\
         #header { position:relative; top:-10px; z-index:101; background:#fff;\
                   border-bottom:1px solid #d1d8e5 }\
         #group { display:flex; padding-top:10px; height:52px }\
         #search { position:absolute; width:942.5px; height:52px }\
         #form { display:inline-block; box-sizing:border-box; width:799.5px; height:52px;\
                 border:1.5px solid transparent; border-radius:15px;\
                 background:linear-gradient(#fff,#fff) padding-box,\
                            linear-gradient(160deg,#3377fe,#4c6fff,#8370ff,#ba59ff) border-box }\
         #hot { height:52px; margin:12px auto 8px; width:990px }\
         #nav { height:28px; background:#fff }\
         </style>\
         <div id=layout-header></div>\
         <div id=header><div id=group><div id=search><form id=form></form></div></div>\
         <div id=hot></div></div><div id=nav></div>",
    );
    let mut options = DocumentRenderOptions::default();
    options.layout.viewport = PhysicalSize {
        width: 1190.0,
        height: 400.0,
    };
    let render = document.render_reference(options);
    let rect = |id: &str| {
        let source = target_id(&document, id);
        render
            .layout
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(source))
            .unwrap_or_else(|| panic!("#{id} must be laid out"))
            .rect
    };
    let strip = rect("layout-header");
    let form = rect("form");
    assert_eq!(strip.origin.y + strip.size.height, 86.0);
    assert_eq!(form.origin.y, 86.0, "the form must clear the strip");
    assert_eq!(form.size.height, 52.0);

    // The 1.5px gradient border at the form's top edge is painted, not erased
    // by the white strip that covers the header's lifted 10px band.
    let x = u32::try_from(form.origin.x as i32 + 300).expect("inside the surface");
    let top = u32::try_from(form.origin.y as i32).expect("inside the surface");
    let pixel = render
        .raster
        .surface
        .pixel(x, top)
        .expect("the form's top edge is inside the surface");
    assert!(
        pixel.blue > 180 && pixel.red < 220,
        "expected the gradient border at the form top, found {pixel:?}"
    );
    let above = render
        .raster
        .surface
        .pixel(x, top - 1)
        .expect("the strip above the form is inside the surface");
    assert_eq!(
        (above.red, above.green, above.blue),
        (255, 255, 255),
        "the strip above the form stays white"
    );
}

/// A clipped hot-search row holds two fallback lines. Atomic inline badges
/// are baseline-aligned with the row's text, so the hidden second line's
/// badge sits below the 23px clip instead of leaking a 1px orange sliver at
/// the row's bottom edge.
#[test]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "test geometry is non-negative and inside the render surface"
)]
fn clipped_second_hot_row_does_not_leak_its_badge() {
    let document = Document::parse(
        "<!doctype html><style>\
         html, body, div { display:block; margin:0 }\
         #hot { position:relative; width:600px; height:23px; overflow:hidden;\
                font-size:13px; line-height:23px }\
         .row { height:23px; white-space:nowrap }\
         .item { display:inline-flex; align-items:center; font-size:13px; line-height:23px }\
         .badge { display:inline-block; position:relative; top:-1px; z-index:-1;\
                  width:16px; height:16px; line-height:16px; font-size:12px;\
                  color:#fff; background:#ff9812 }\
         </style>\
         <div id=hot>\
           <div class=row><a class=item href=#>headline</a><span id=b1 class=badge>hot</span></div>\
           <div class=row><a class=item href=#>second</a><span id=b2 class=badge>hot</span></div>\
         </div>",
    );
    let render = document.render_reference(DocumentRenderOptions::default());
    let rect = |id: &str| {
        let source = target_id(&document, id);
        render
            .layout
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(source))
            .unwrap_or_else(|| panic!("#{id} must be laid out"))
            .rect
    };
    let b1 = rect("b1");
    let b2 = rect("b2");
    assert!(b1.origin.y > 0.0, "the first badge is baseline-aligned");
    assert!(
        b2.origin.y >= 23.0,
        "the hidden second row badge starts below the clip: {b2:?}"
    );
    let x = u32::try_from(b2.origin.x as i32 + 2).expect("inside the surface");
    let pixel = render
        .raster
        .surface
        .pixel(x, 22)
        .expect("the row's bottom edge is inside the surface");
    assert_eq!(
        (pixel.red, pixel.green, pixel.blue),
        (255, 255, 255),
        "no badge pixel may leak into the clipped row"
    );
}
