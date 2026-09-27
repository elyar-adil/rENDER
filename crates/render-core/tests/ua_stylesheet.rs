//! HTML user-agent style sheet coverage.
//!
//! The sheet lives in `document.rs` and is cited to the WHATWG HTML
//! Standard's Rendering chapter. These tests render through the document
//! pipeline, which is the only place the sheet is applied, and assert both the
//! computed declarations it contributes and the display-list commands those
//! declarations produce.

use render_core::document::{Document, DocumentRenderOptions};
use render_core::dom::NodeId;
use render_core::paint::{DisplayCommand, ListMarkerShape, TextDecorationLine};

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

fn computed(document: &Document, id: &str, property: &str) -> String {
    let node = find_by_id(document, id);
    document
        .render_reference(DocumentRenderOptions::default())
        .styles
        .get(&node)
        .and_then(|style| style.get(property))
        .map(|value| value.css_text().trim().to_owned())
        .unwrap_or_default()
}

fn computed_text(document: &Document, id: &str) -> String {
    let mut pending = vec![find_by_id(document, id)];
    let mut text = String::new();
    while let Some(node) = pending.pop() {
        if let Some(render_core::dom::NodeKind::Text(value)) =
            document.dom().node(node).map(render_core::dom::Node::kind)
        {
            text.push_str(value);
        }
        for child in document.dom().children(node).unwrap_or_default() {
            pending.insert(0, *child);
        }
    }
    text
}

#[test]
fn headings_get_the_specified_size_weight_and_margins() {
    // HTML §15.3.6: `:heading` is bold, and h1-h6 step 2.00em down to 0.67em
    // with margins that grow as the size shrinks.
    let document = Document::parse(
        "<!doctype html><body>\
         <h1 id=h1>one</h1><h2 id=h2>two</h2><h3 id=h3>three</h3>\
         <h4 id=h4>four</h4><h5 id=h5>five</h5><h6 id=h6>six</h6>\
         </body>",
    );
    for (id, size) in [
        ("h1", "32px"),
        ("h2", "24px"),
        ("h3", "18.72px"),
        ("h4", "16px"),
        ("h5", "13.28px"),
        ("h6", "10.72px"),
    ] {
        assert_eq!(computed(&document, id, "font-size"), size, "{id} size");
        assert_eq!(
            computed(&document, id, "font-weight"),
            "bold",
            "{id} weight"
        );
    }
    // `em` in a margin resolves against the element's own font size, so the
    // computed declaration keeps the em and the used value follows the size.
    assert_eq!(computed(&document, "h1", "margin-top"), "0.67em");
    assert_eq!(computed(&document, "h6", "margin-top"), "2.33em");
}

#[test]
fn paragraphs_and_blockquotes_get_their_margins() {
    // HTML §15.3.3: block-level flow content carries 1em block margins, and
    // blockquote/figure indent by 40px.
    let document = Document::parse(
        "<!doctype html><body><p id=p>text</p><blockquote id=q>quote</blockquote></body>",
    );
    assert_eq!(computed(&document, "p", "margin-top"), "1em");
    assert_eq!(computed(&document, "p", "margin-bottom"), "1em");
    assert_eq!(computed(&document, "q", "margin-top"), "1em");
    assert_eq!(computed(&document, "q", "margin-left"), "40px");
}

#[test]
fn links_are_underlined_and_link_coloured() {
    // HTML §15.3.4: `:link` is `#0000EE` with an underline.
    let document = Document::parse(
        "<!doctype html><body><a id=a href='https://example.test/'>link</a></body>",
    );
    assert_eq!(computed(&document, "a", "color"), "#0000ee");
    assert_eq!(
        computed(&document, "a", "text-decoration-line"),
        "underline"
    );

    // The underline reaches the painted output, not just the cascade. The
    // run belongs to the link's text node, so the assertion is on the command
    // stream rather than on an element id.
    let render = document.render_reference(DocumentRenderOptions::default());
    let underlines = render
        .display
        .list
        .items()
        .iter()
        .filter(|item| {
            matches!(
                &item.command,
                DisplayCommand::TextDecoration(decoration)
                    if decoration.line == TextDecorationLine::Underline
            )
        })
        .count();
    assert_eq!(underlines, 1, "the link text must paint one underline");
}

#[test]
fn emphasis_and_monospace_declare_their_specified_faces() {
    // HTML §15.3.4. These computed values are correct but do not reach the
    // rasterizer: render-layout's `TextStyle` carries only `font_size` and
    // `line_height`, so `font-weight`, `font-style` and `font-family` are
    // dropped before shaping. The rules stay because the day that gap closes
    // these are what must already be right.
    let document = Document::parse(
        "<!doctype html><body>\
         <b id=b>b</b><strong id=s>s</strong><i id=i>i</i><em id=e>e</em>\
         <cite id=c>c</cite><dfn id=d>d</dfn><code id=k>k</code>\
         <kbd id=kd>k</kbd><samp id=sm>s</samp><tt id=t>t</tt>\
         <small id=sm2>s</small><big id=bg>b</big>\
         </body>",
    );
    for id in ["b", "s"] {
        assert_eq!(computed(&document, id, "font-weight"), "bolder", "{id}");
    }
    for id in ["i", "e", "c", "d"] {
        assert_eq!(computed(&document, id, "font-style"), "italic", "{id}");
    }
    for id in ["k", "kd", "sm", "t"] {
        assert_eq!(computed(&document, id, "font-family"), "monospace", "{id}");
    }
    // `smaller` is 5/6 of the parent size and `larger` 6/5, and the computed
    // value of `font-size` is absolute (CSS 2.1 §6.1.1), so the keywords arrive
    // already resolved.
    assert_eq!(computed(&document, "sm2", "font-size"), "13.333333px");
    assert_eq!(computed(&document, "bg", "font-size"), "19.2px");
}

#[test]
fn preformatted_blocks_keep_their_white_space() {
    // HTML §15.3.3: `pre` is monospace and preserves white space.
    let document = Document::parse("<!doctype html><body><pre id=pre>a  b</pre></body>");
    assert_eq!(computed(&document, "pre", "white-space"), "pre");
    assert_eq!(computed(&document, "pre", "font-family"), "monospace");
}

#[test]
fn sub_and_superscript_move_off_the_baseline() {
    let document =
        Document::parse("<!doctype html><body><sub id=s>x</sub><sup id=p>y</sup></body>");
    assert_eq!(computed(&document, "s", "vertical-align"), "sub");
    assert_eq!(computed(&document, "p", "vertical-align"), "super");
    // `sub`/`sup` are `smaller`, which the absolute computed font size
    // resolves to five sixths of the surrounding text.
    assert_eq!(computed(&document, "s", "font-size"), "13.333333px");
}

#[test]
fn tables_space_their_cells_and_emphasise_headers() {
    // HTML §15.3.8: `border-spacing: 2px` is the separated-border default and
    // cells carry 1px of padding; `th` is bold.
    let document = Document::parse(
        "<!doctype html><body><table id=t><tr><td id=td>c</td><th id=th>h</th></tr></table></body>",
    );
    assert_eq!(computed(&document, "t", "border-spacing"), "2px");
    assert_eq!(computed(&document, "t", "box-sizing"), "border-box");
    assert_eq!(computed(&document, "td", "padding-top"), "1px");
    assert_eq!(computed(&document, "th", "padding-left"), "1px");
    assert_eq!(computed(&document, "th", "font-weight"), "bold");
}

#[test]
fn lists_pad_their_leading_edge_and_choose_a_marker_family() {
    // HTML §15.3.7: lists indent 40px; `ol` numbers, `ul` and `menu` bullet,
    // and a nested list of the same kind switches to `circle`, then `square`.
    let document = Document::parse(
        "<!doctype html><body><ul id=u><li>a<ul id=inner><li>b</li></ul></li></ul>\
         <ol id=o><li>c</li></ol></body>",
    );
    assert_eq!(computed(&document, "u", "padding-left"), "40px");
    assert_eq!(computed(&document, "u", "list-style-type"), "disc");
    assert_eq!(computed(&document, "o", "list-style-type"), "decimal");
    assert_eq!(computed(&document, "inner", "list-style-type"), "circle");
    assert_eq!(computed(&document, "u", "margin-top"), "1em");
}

#[test]
fn list_items_paint_their_markers() {
    // The UA sheet's `display: list-item` and `list-style-type` reach paint:
    // an unordered item paints a disc and an ordered item its ordinal.
    let document = Document::parse(
        "<!doctype html><body><ul><li id=item>a</li></ul><ol><li>b</li></ol></body>",
    );
    let render = document.render_reference(DocumentRenderOptions::default());
    let bullets = render
        .display
        .list
        .items()
        .iter()
        .filter_map(|item| match &item.command {
            DisplayCommand::ListMarker(marker) => Some(marker.shape),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(bullets, vec![ListMarkerShape::Disc]);

    // The ordered item's marker is text, so it arrives as a glyph run.
    let texts = render
        .display
        .list
        .items()
        .iter()
        .filter_map(|item| match &item.command {
            DisplayCommand::GlyphRun(run) => Some(
                run.glyphs
                    .iter()
                    .map(|glyph| char::from_u32(glyph.glyph.0).unwrap_or('\u{fffd}'))
                    .collect::<String>(),
            ),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(texts.contains(&"1.".to_owned()), "{texts:?}");
    assert!(texts.contains(&"a".to_owned()), "{texts:?}");
}

#[test]
fn form_controls_use_a_smaller_face_and_a_box_border() {
    // HTML §15.3.10 defers the widget look to §15.5; the smaller UI font and
    // border-box sizing are the parts a document can observe.
    let document = Document::parse(
        "<!doctype html><body><input id=i type=text><button id=b>go</button>\
         <textarea id=ta></textarea><select id=sel><option>x</option></select></body>",
    );
    for id in ["i", "b", "ta", "sel"] {
        assert_eq!(
            computed(&document, id, "font-size"),
            "13.3333px",
            "{id} control font"
        );
        assert_eq!(computed(&document, id, "box-sizing"), "border-box", "{id}");
    }
    assert_eq!(computed(&document, "ta", "white-space"), "pre-wrap");
    assert_eq!(computed(&document, "b", "text-align"), "center");
}

#[test]
fn rules_hr_and_mark_carry_their_specified_appearance() {
    let document = Document::parse(
        "<!doctype html><body><hr id=r><mark id=m>hi</mark><q id=q>quoted</q>\
         <abbr id=a title=abbreviation>abbr</abbr><del id=d>gone</del>\
         <ins id=n>new</ins></body>",
    );
    assert_eq!(computed(&document, "r", "border-top-width"), "1px");
    assert_eq!(computed(&document, "r", "margin-top"), "0.5em");
    assert_eq!(computed(&document, "m", "background-color"), "yellow");
    assert_eq!(computed(&document, "m", "color"), "black");
    // `q` has no UA rule: §15.3.4 styles it with `content: open-quote` and
    // `content: close-quote` on its pseudo-elements, which needs generated
    // content this engine does not model. The inherited face is untouched.
    assert_eq!(computed(&document, "q", "font-style"), "normal");
    assert_eq!(computed(&document, "a", "text-decoration-style"), "dotted");
    assert_eq!(
        computed(&document, "a", "text-decoration-line"),
        "underline"
    );
    assert_eq!(
        computed(&document, "d", "text-decoration-line"),
        "line-through"
    );
    assert_eq!(
        computed(&document, "n", "text-decoration-line"),
        "underline"
    );
}

#[test]
fn hidden_elements_stay_unrendered() {
    let document = Document::parse(
        "<!doctype html><body><span id=s hidden>x</span>\
         <input id=i type=hidden><template id=t></template></body>",
    );
    let render = document.render_reference(DocumentRenderOptions::default());
    for id in ["s", "i", "t"] {
        let node = find_by_id(&document, id);
        assert!(
            !render
                .display
                .list
                .items()
                .iter()
                .any(|item| item.source == Some(node)),
            "#{id} must not paint"
        );
    }
}

#[test]
fn an_author_rule_overrides_a_user_agent_default() {
    // The sheet cascades at the user-agent origin, so any author declaration
    // wins without `!important`.
    let document = Document::parse(
        "<!doctype html><head><style>\
         h1 { font-size: 9px } a { color: red } ul { padding-left: 0 }\
         </style></head><body><h1 id=h>t</h1><a id=a href='#'>l</a><ul id=u><li>x</li></ul></body>",
    );
    assert_eq!(computed(&document, "h", "font-size"), "9px");
    assert_eq!(computed(&document, "a", "color"), "red");
    assert_eq!(computed(&document, "u", "padding-left"), "0");
}

#[test]
fn an_unauthored_document_renders_headings_at_their_own_size() {
    // The end-to-end symptom the sheet exists to fix: with no author CSS at
    // all, a heading is laid out and painted larger than its body text.
    let document =
        Document::parse("<!doctype html><body><h1 id=h>title</h1><p id=p>body</p></body>");
    let render = document.render_reference(DocumentRenderOptions::default());
    let heading = find_by_id(&document, "h");
    let paragraph = find_by_id(&document, "p");
    let heading_size = render
        .styles
        .get(&heading)
        .and_then(|style| style.get("font-size"))
        .map(|value| value.css_text().to_owned())
        .expect("heading font size");
    let paragraph_size = render
        .styles
        .get(&paragraph)
        .and_then(|style| style.get("font-size"))
        .map(|value| value.css_text().to_owned())
        .expect("paragraph font size");
    assert_eq!(heading_size, "32px");
    assert_eq!(paragraph_size, "16px");
    let _ = computed_text(&document, "h");
}
