//! Nested browsing contexts through the page pipeline: an iframe's document
//! runs in its own realm, its `load` event reaches the page, and it is not
//! painted, because painting nested documents is not implemented yet.

use std::time::Duration;

use render_core::js::JsValue;
use render_core::page::Page;
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
fn a_nested_document_is_not_painted_into_the_page() {
    let mut plain = page("<!doctype html><body><p>page</p></body>");
    let mut framed = page(
        "<!doctype html><body><p>page</p><iframe srcdoc=\"<p>child text</p>\"></iframe></body>",
    );
    settle(&mut plain);
    settle(&mut framed);
    assert_eq!(
        framed.snapshot().display.list.items().len(),
        plain.snapshot().display.list.items().len(),
        "an iframe paints as nothing, so its child document adds no display items"
    );
}
