//! `CSSStyleSheet.insertRule` must reach the same cascade the page renders,
//! not a JavaScript-only rule list. A CSS-in-JS library (emotion, styled-
//! components) builds its whole style sheet through this API after hydration;
//! if the edit stays in the CSSOM, the page renders as unstyled markup.

use render_core::page::Page;
use url::Url;

fn page(html: &str) -> Page {
    Page::with_url(
        html,
        &Url::parse("https://example.test/app").expect("test URL"),
    )
}

fn run(page: &mut Page, source: &str) {
    page.queue_script(source).expect("script should queue");
    let turn = page
        .run_one_turn_reference()
        .expect("turn should run")
        .expect("script turn");
    turn.executions[0].result.as_ref().expect("script result");
}

fn background(page: &Page, id: &str) -> String {
    let dom = page.document().dom();
    let mut pending = vec![dom.document()];
    while let Some(node) = pending.pop() {
        if dom.attribute(node, "id").ok().flatten() == Some(id) {
            return page
                .snapshot()
                .styles
                .get(&node)
                .and_then(|style| style.get("background-color"))
                .map(|value| value.css_text().to_owned())
                .unwrap_or_default();
        }
        pending.extend(dom.children(node).unwrap_or_default().iter().rev());
    }
    panic!("missing #{id}");
}

#[test]
fn insert_rule_changes_the_rendered_cascade() {
    let mut page = page(
        "<!doctype html><style id=sheet>#box { display: block; width: 40px; height: 20px }</style>\
         <div id=box></div>",
    );
    run(&mut page, "0");
    assert_eq!(background(&page, "box"), "transparent");

    run(
        &mut page,
        "document.getElementById('sheet').sheet.insertRule('#box { background-color: rgb(0, 255, 0) }', 1)",
    );
    assert_eq!(background(&page, "box"), "rgb(0, 255, 0)");
}

#[test]
fn delete_rule_removes_it_from_the_rendered_cascade() {
    let mut page = page(
        "<!doctype html><style id=sheet>\
         #box { display: block; width: 40px; height: 20px; background-color: rgb(0, 0, 255) }\
         </style><div id=box></div>",
    );
    run(&mut page, "0");
    assert_eq!(background(&page, "box"), "rgb(0, 0, 255)");

    run(
        &mut page,
        "document.getElementById('sheet').sheet.deleteRule(0)",
    );
    assert_eq!(background(&page, "box"), "transparent");
}

#[test]
fn a_dom_text_edit_and_a_cssom_edit_observe_the_same_rules() {
    let mut page = page(
        "<!doctype html><style id=sheet>#box { display: block; width: 40px; height: 20px }</style>\
         <div id=box></div>",
    );
    run(&mut page, "0");
    run(
        &mut page,
        "var sheet = document.getElementById('sheet').sheet; \
         sheet.insertRule('#box { background-color: rgb(1, 2, 3) }', 1); \
         document.getElementById('sheet').textContent = '#box { background-color: rgb(4, 5, 6) }';",
    );
    // The DOM text is the sheet's truth, so the CSSOM edit is replaced by the
    // assignment rather than layered underneath it.
    assert_eq!(background(&page, "box"), "rgb(4, 5, 6)");
}
