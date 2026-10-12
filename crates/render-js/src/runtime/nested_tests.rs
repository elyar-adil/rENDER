//! Nested browsing contexts: iframes that display their own documents, their
//! `contentWindow` and `contentDocument`, `parent`, `top`, the `load` event of
//! an iframe, and `postMessage` between windows.

use std::collections::BTreeMap;

use crate::{DocumentLoader, JsRuntime, RuntimeLimits, TimerRequest};
use render_dom::Dom;
use render_html::parse_document;
use url::Url;

const PAGE_URL: &str = "http://example.test/page.html";

/// A loader over an in-memory table of documents, keyed by absolute URL.
#[derive(Debug)]
struct TableLoader(BTreeMap<String, String>);

impl DocumentLoader for TableLoader {
    fn load_text(&mut self, url: &Url) -> Option<String> {
        self.0.get(url.as_str()).cloned()
    }
}

/// A page from `html`, with `loader` as the host's documents.
fn page(html: &str, loader: BTreeMap<String, String>) -> (Dom, JsRuntime) {
    let mut dom = parse_document(html).dom;
    let url = Url::parse(PAGE_URL).expect("valid URL");
    let mut runtime = JsRuntime::with_limits_and_url(&dom, RuntimeLimits::default(), &url);
    runtime.set_document_loader(Box::new(TableLoader(loader)));
    // Any script run connects the page's iframes, so run an empty one.
    run(&mut dom, &mut runtime, "0");
    (dom, runtime)
}

fn run(dom: &mut Dom, runtime: &mut JsRuntime, source: &str) -> String {
    runtime
        .execute(dom, source)
        .map(|outcome| outcome.value.to_js_string().to_lossy())
        .unwrap_or_else(|error| panic!("{source}\n=> {error}"))
}

/// Runs queued microtasks and fires scheduled timers, which is how frame loads,
/// frame `load` events and messages make progress, until nothing is left.
fn settle(dom: &mut Dom, runtime: &mut JsRuntime) {
    for _ in 0..64 {
        let mut did_work = false;
        for microtask in runtime.take_pending_microtasks() {
            did_work = true;
            let _ = runtime.invoke_microtask(dom, microtask);
        }
        for request in runtime.take_pending_timer_requests() {
            if let TimerRequest::Schedule { id, .. } = request {
                did_work = true;
                let _ = runtime.fire_timer(dom, id);
            }
        }
        if !did_work {
            return;
        }
    }
    panic!("the event loop did not settle");
}

#[test]
fn an_inserted_iframe_has_its_own_window_and_document_at_once() {
    let (mut dom, mut runtime) = page("<body></body>", BTreeMap::new());
    run(
        &mut dom,
        &mut runtime,
        "var f = document.createElement('iframe'); document.body.appendChild(f); var w = f.contentWindow;",
    );
    assert_eq!(run(&mut dom, &mut runtime, "w !== window"), "true");
    assert_eq!(
        run(&mut dom, &mut runtime, "w.document !== document"),
        "true"
    );
    assert_eq!(
        run(&mut dom, &mut runtime, "f.contentDocument === w.document"),
        "true"
    );
    assert_eq!(run(&mut dom, &mut runtime, "w.parent === window"), "true");
    assert_eq!(run(&mut dom, &mut runtime, "w.top === window"), "true");
    assert_eq!(run(&mut dom, &mut runtime, "w.frameElement === f"), "true");
    assert_eq!(
        run(&mut dom, &mut runtime, "window.frameElement === null"),
        "true"
    );
}

#[test]
fn a_srcdoc_script_runs_in_its_own_realm_and_sees_its_parent_and_top() {
    let html = "<body><iframe srcdoc=\"<script>var inner = 'child'; var seenParent = parent.marker; var seenTop = top.marker;</script>\"></iframe></body>";
    let (mut dom, mut runtime) = page(html, BTreeMap::new());
    run(&mut dom, &mut runtime, "var marker = 'page';");
    settle(&mut dom, &mut runtime);
    assert_eq!(
        run(
            &mut dom,
            &mut runtime,
            "document.querySelector('iframe').contentWindow.inner"
        ),
        "child"
    );
    assert_eq!(
        run(
            &mut dom,
            &mut runtime,
            "document.querySelector('iframe').contentWindow.seenParent"
        ),
        "page"
    );
    assert_eq!(
        run(
            &mut dom,
            &mut runtime,
            "document.querySelector('iframe').contentWindow.seenTop"
        ),
        "page"
    );
    assert!(run(&mut dom, &mut runtime, "typeof inner").contains("undefined"));
}

#[test]
fn an_iframe_fires_load_after_its_document_scripts_ran() {
    let html = "<body><iframe srcdoc=\"<script>var ran = true;</script>\"></iframe></body>";
    let (mut dom, mut runtime) = page(html, BTreeMap::new());
    run(
        &mut dom,
        &mut runtime,
        "var order = []; var f = document.querySelector('iframe'); f.addEventListener('load', function () { order.push('frame-load:' + f.contentWindow.ran); });",
    );
    settle(&mut dom, &mut runtime);
    assert_eq!(
        run(&mut dom, &mut runtime, "order.join()"),
        "frame-load:true"
    );
}

#[test]
fn an_iframe_src_loads_through_the_document_loader_with_its_own_url() {
    let mut documents = BTreeMap::new();
    documents.insert(
        "http://example.test/frame.html".to_owned(),
        "<script>var loaded = 'from src';</script>".to_owned(),
    );
    let html = "<body><iframe src=\"frame.html\"></iframe></body>";
    let (mut dom, mut runtime) = page(html, documents);
    settle(&mut dom, &mut runtime);
    let frame = "document.querySelector('iframe').contentWindow";
    assert_eq!(
        run(&mut dom, &mut runtime, &format!("{frame}.loaded")),
        "from src"
    );
    assert_eq!(
        run(&mut dom, &mut runtime, &format!("{frame}.location.href")),
        "http://example.test/frame.html"
    );
}

#[test]
fn an_iframe_whose_src_cannot_load_still_loads_an_empty_document() {
    let html = "<body><iframe src=\"missing.html\"></iframe></body>";
    let (mut dom, mut runtime) = page(html, BTreeMap::new());
    run(
        &mut dom,
        &mut runtime,
        "var fired = 0; document.querySelector('iframe').addEventListener('load', function () { fired++; });",
    );
    settle(&mut dom, &mut runtime);
    assert_eq!(run(&mut dom, &mut runtime, "fired"), "1");
    assert_eq!(
        run(
            &mut dom,
            &mut runtime,
            "document.querySelector('iframe').contentDocument.body !== null"
        ),
        "true"
    );
}

#[test]
fn a_child_posts_a_message_that_reaches_the_parent_window_with_a_copy_of_its_data() {
    let html = "<body><iframe srcdoc=\"<script>var payload = {n: 7}; parent.postMessage(payload, '*');</script>\"></iframe></body>";
    let (mut dom, mut runtime) = page(html, BTreeMap::new());
    run(
        &mut dom,
        &mut runtime,
        "var got = null; var sameSource = null; window.addEventListener('message', function (e) { got = e.data; sameSource = e.source === document.querySelector('iframe').contentWindow; });",
    );
    settle(&mut dom, &mut runtime);
    assert_eq!(run(&mut dom, &mut runtime, "got.n"), "7");
    assert_eq!(run(&mut dom, &mut runtime, "sameSource"), "true");
    assert_eq!(
        run(
            &mut dom,
            &mut runtime,
            "got !== document.querySelector('iframe').contentWindow.payload"
        ),
        "true",
        "postMessage delivers a structured clone, not the sender's object"
    );
}

#[test]
fn a_message_to_a_different_origin_is_discarded() {
    let html = "<body><iframe srcdoc=\"<script>parent.postMessage('hello', 'https://elsewhere.test');</script>\"></iframe></body>";
    let (mut dom, mut runtime) = page(html, BTreeMap::new());
    run(
        &mut dom,
        &mut runtime,
        "var count = 0; window.addEventListener('message', function () { count++; });",
    );
    settle(&mut dom, &mut runtime);
    assert_eq!(run(&mut dom, &mut runtime, "count"), "0");
}

#[test]
fn a_window_listener_belongs_to_its_own_window() {
    let html = "<body><iframe srcdoc=\"<script>window.addEventListener('message', function () { window.childHeard = true; });</script>\"></iframe></body>";
    let (mut dom, mut runtime) = page(html, BTreeMap::new());
    run(
        &mut dom,
        &mut runtime,
        "var pageHeard = false; window.addEventListener('message', function () { pageHeard = true; }); window.postMessage('to the page', '*');",
    );
    settle(&mut dom, &mut runtime);
    assert_eq!(run(&mut dom, &mut runtime, "pageHeard"), "true");
    assert!(
        run(
            &mut dom,
            &mut runtime,
            "document.querySelector('iframe').contentWindow.childHeard"
        )
        .contains("undefined")
    );
}

#[test]
fn getelementbyid_searches_the_document_it_is_called_on() {
    let html =
        "<body><p id=\"shared\">page</p><iframe srcdoc=\"<p id=inner>child</p>\"></iframe></body>";
    let (mut dom, mut runtime) = page(html, BTreeMap::new());
    settle(&mut dom, &mut runtime);
    let child = "document.querySelector('iframe').contentDocument";
    assert_eq!(
        run(
            &mut dom,
            &mut runtime,
            &format!("{child}.getElementById('inner').textContent")
        ),
        "child"
    );
    assert_eq!(
        run(
            &mut dom,
            &mut runtime,
            &format!("{child}.getElementById('shared')")
        ),
        "null"
    );
    assert_eq!(
        run(&mut dom, &mut runtime, "document.getElementById('inner')"),
        "null"
    );
}

#[test]
fn a_node_of_a_frame_document_has_the_frame_realms_prototype() {
    let html = "<body><iframe srcdoc=\"<p id=inner>child</p>\"></iframe></body>";
    let (mut dom, mut runtime) = page(html, BTreeMap::new());
    settle(&mut dom, &mut runtime);
    let body = "document.querySelector('iframe').contentDocument.body";
    assert_eq!(
        run(
            &mut dom,
            &mut runtime,
            &format!(
                "{body} instanceof document.querySelector('iframe').contentWindow.HTMLElement"
            )
        ),
        "true"
    );
    assert_eq!(
        run(
            &mut dom,
            &mut runtime,
            &format!("{body} instanceof HTMLElement")
        ),
        "false"
    );
}

#[test]
fn removing_an_iframe_drops_its_browsing_context() {
    let html = "<body><iframe srcdoc=\"<p>x</p>\"></iframe></body>";
    let (mut dom, mut runtime) = page(html, BTreeMap::new());
    settle(&mut dom, &mut runtime);
    run(
        &mut dom,
        &mut runtime,
        "var f = document.querySelector('iframe'); var kept = f.contentWindow; f.parentNode.removeChild(f);",
    );
    assert_eq!(run(&mut dom, &mut runtime, "f.contentWindow"), "null");
    assert_eq!(run(&mut dom, &mut runtime, "kept !== null"), "true");
}

#[test]
fn a_frame_that_cannot_get_a_realm_is_refused_and_the_page_keeps_running() {
    // With room for two realms, the third frame has no window, and nothing panics.
    let mut dom = parse_document("<body></body>").dom;
    let url = Url::parse(PAGE_URL).expect("valid URL");
    let limits = RuntimeLimits {
        max_heap_objects: 6_000,
        ..RuntimeLimits::default()
    };
    let mut runtime = JsRuntime::with_limits_and_url(&dom, limits, &url);
    run(
        &mut dom,
        &mut runtime,
        "for (var i = 0; i < 3; i++) { var f = document.createElement('iframe'); f.setAttribute('srcdoc', '<p>x</p>'); document.body.appendChild(f); }",
    );
    settle(&mut dom, &mut runtime);
    assert_eq!(
        run(
            &mut dom,
            &mut runtime,
            "var frames = document.querySelectorAll('iframe'); [0, 1, 2].map(function (i) { return frames[i].contentWindow === null; }).join()"
        ),
        "false,false,true"
    );
    assert_eq!(run(&mut dom, &mut runtime, "1 + 1"), "2");
}
