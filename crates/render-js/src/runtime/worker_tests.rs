//! Dedicated Web Workers: message delivery, `importScripts`, termination,
//! error reporting, and the worker global scope.

use std::collections::BTreeMap;

use crate::{DocumentLoader, JsRuntime, RuntimeLimits, TimerRequest};
use render_dom::Dom;
use render_html::parse_document;
use url::Url;

const PAGE_URL: &str = "http://example.test/page.html";

#[derive(Debug)]
struct TableLoader(BTreeMap<String, String>);

impl DocumentLoader for TableLoader {
    fn load_text(&mut self, url: &Url) -> Option<String> {
        self.0.get(url.as_str()).cloned()
    }
}

fn page_with_loader(html: &str, loader: BTreeMap<String, String>) -> (Dom, JsRuntime) {
    let mut dom = parse_document(html).dom;
    let url = Url::parse(PAGE_URL).expect("valid URL");
    let mut runtime = JsRuntime::with_limits_and_url(&dom, RuntimeLimits::default(), &url);
    runtime.set_document_loader(Box::new(TableLoader(loader)));
    run(&mut dom, &mut runtime, "0");
    (dom, runtime)
}

fn page(html: &str, loader: BTreeMap<String, String>) -> (Dom, JsRuntime) {
    page_with_loader(html, loader)
}

fn run(dom: &mut Dom, runtime: &mut JsRuntime, source: &str) -> String {
    runtime
        .execute(dom, source)
        .map(|outcome| outcome.value.to_js_string().to_lossy())
        .unwrap_or_else(|error| panic!("{source}\n=> {error}"))
}

/// Runs microtasks and timer/internal tasks until the page settles.
fn settle(dom: &mut Dom, runtime: &mut JsRuntime) {
    for _ in 0..128 {
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

fn worker(documents: &[(&str, &str)]) -> BTreeMap<String, String> {
    documents
        .iter()
        .map(|(path, source)| (format!("http://example.test/{path}"), (*source).to_owned()))
        .collect()
}

#[test]
fn worker_is_a_function_and_a_worker_echoes_a_message() {
    let documents = worker(&[(
        "echo.js",
        "onmessage = function (event) { postMessage(event.data + 1); };",
    )]);
    let (mut dom, mut runtime) = page("<body></body>", documents);
    assert_eq!(run(&mut dom, &mut runtime, "typeof Worker"), "function");
    run(
        &mut dom,
        &mut runtime,
        "var got = 0; var failed = null; \
         var worker = new Worker('echo.js'); \
         worker.onmessage = function (event) { got = event.data; }; \
         worker.onerror = function (event) { failed = event.message; }; \
         worker.postMessage(1);",
    );
    settle(&mut dom, &mut runtime);
    assert_eq!(run(&mut dom, &mut runtime, "got"), "2");
    assert_eq!(run(&mut dom, &mut runtime, "String(failed)"), "null");
}

#[test]
fn import_scripts_runs_in_argument_order() {
    let documents = worker(&[
        (
            "main.js",
            "var order = []; importScripts('a.js', 'b.js', 'a.js'); postMessage(order.join(','));",
        ),
        ("a.js", "order.push('a');"),
        ("b.js", "order.push('b');"),
    ]);
    let (mut dom, mut runtime) = page("<body></body>", documents);
    run(
        &mut dom,
        &mut runtime,
        "var got = ''; var worker = new Worker('main.js'); \
         worker.onmessage = function (event) { got = event.data; };",
    );
    settle(&mut dom, &mut runtime);
    assert_eq!(run(&mut dom, &mut runtime, "got"), "a,b,a");
}

#[test]
fn terminate_stops_future_messages_and_timers() {
    let documents = worker(&[(
        "echo.js",
        "onmessage = function (event) { postMessage(event.data); };",
    )]);
    let (mut dom, mut runtime) = page("<body></body>", documents);
    run(
        &mut dom,
        &mut runtime,
        "var seen = []; var worker = new Worker('echo.js'); \
         worker.onmessage = function (event) { seen.push(event.data); };",
    );
    settle(&mut dom, &mut runtime);
    run(&mut dom, &mut runtime, "worker.postMessage('first');");
    settle(&mut dom, &mut runtime);
    run(
        &mut dom,
        &mut runtime,
        "worker.terminate(); worker.postMessage('second');",
    );
    settle(&mut dom, &mut runtime);
    assert_eq!(run(&mut dom, &mut runtime, "seen.join(',')"), "first");
}

#[test]
fn a_worker_exception_fires_error_at_the_worker_object() {
    let documents = worker(&[("bad.js", "throw new Error('worker boom');")]);
    let (mut dom, mut runtime) = page("<body></body>", documents);
    run(
        &mut dom,
        &mut runtime,
        "var message = null; var worker = new Worker('bad.js'); \
         worker.onerror = function (event) { message = event.message; };",
    );
    settle(&mut dom, &mut runtime);
    assert_eq!(run(&mut dom, &mut runtime, "message"), "worker boom");
}

#[test]
fn a_worker_message_handler_exception_fires_error() {
    let documents = worker(&[(
        "bad.js",
        "onmessage = function () { throw new TypeError('handler boom'); };",
    )]);
    let (mut dom, mut runtime) = page("<body></body>", documents);
    run(
        &mut dom,
        &mut runtime,
        "var message = null; var worker = new Worker('bad.js'); \
         worker.addEventListener('error', function (event) { message = event.message; });",
    );
    settle(&mut dom, &mut runtime);
    run(&mut dom, &mut runtime, "worker.postMessage(1);");
    settle(&mut dom, &mut runtime);
    assert_eq!(run(&mut dom, &mut runtime, "message"), "handler boom");
}

#[test]
fn a_transferred_array_buffer_is_detached_and_moved() {
    let documents = worker(&[(
        "echo.js",
        "onmessage = function (event) { postMessage([event.data.byteLength, new Uint8Array(event.data)[0]]); };",
    )]);
    let (mut dom, mut runtime) = page("<body></body>", documents);
    run(
        &mut dom,
        &mut runtime,
        "var got = null; var detached = null; var worker = new Worker('echo.js'); \
         worker.onmessage = function (event) { got = event.data.join(','); }; \
         var buffer = new ArrayBuffer(4); new Uint8Array(buffer)[0] = 42; \
         worker.postMessage(buffer, [buffer]); detached = buffer.detached;",
    );
    settle(&mut dom, &mut runtime);
    assert_eq!(run(&mut dom, &mut runtime, "detached"), "true");
    assert_eq!(run(&mut dom, &mut runtime, "got"), "4,42");
}

#[test]
fn a_worker_timer_runs_in_the_worker_realm() {
    let documents = worker(&[(
        "timer.js",
        "setTimeout(function () { postMessage('timer fired'); }, 5);",
    )]);
    let (mut dom, mut runtime) = page("<body></body>", documents);
    run(
        &mut dom,
        &mut runtime,
        "var got = ''; var worker = new Worker('timer.js'); \
         worker.onmessage = function (event) { got = event.data; };",
    );
    settle(&mut dom, &mut runtime);
    assert_eq!(run(&mut dom, &mut runtime, "got"), "timer fired");
}

#[test]
fn a_worker_fetch_resolves_against_the_worker_url() {
    let documents = worker(&[(
        "sub/worker.js",
        "fetch('data.json').then(function (response) { return response.text(); }).then(function (text) { postMessage(text); });",
    )]);
    let (mut dom, mut runtime) = page("<body></body>", documents);
    run(
        &mut dom,
        &mut runtime,
        "var got = ''; var worker = new Worker('sub/worker.js'); \
         worker.onmessage = function (event) { got = event.data; };",
    );
    // Worker load, then the worker's fetch.
    settle(&mut dom, &mut runtime);
    let requests = runtime.take_pending_fetch_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].url, "http://example.test/sub/data.json",
        "a worker resolves relative fetches against its script URL"
    );
    runtime.settle_fetch(
        &mut dom,
        requests[0].id,
        Ok(crate::FetchOutcome {
            status: 200,
            status_text: "OK".to_owned(),
            headers: Vec::new(),
            body: b"worker data".to_vec(),
        }),
    );
    settle(&mut dom, &mut runtime);
    assert_eq!(run(&mut dom, &mut runtime, "got"), "worker data");
}

#[test]
fn a_data_url_worker_runs_without_the_network() {
    let mut dom = parse_document("<body></body>").dom;
    let url = Url::parse(PAGE_URL).expect("valid URL");
    let mut runtime = JsRuntime::with_limits_and_url(&dom, RuntimeLimits::default(), &url);
    run(
        &mut dom,
        &mut runtime,
        "var got = 0; \
         var blob = new Blob(['postMessage(7);'], {type: 'text/javascript'}); \
         var worker = new Worker(URL.createObjectURL(blob)); \
         worker.onmessage = function (event) { got = event.data; };",
    );
    settle(&mut dom, &mut runtime);
    assert_eq!(run(&mut dom, &mut runtime, "got"), "7");
}

#[test]
fn a_remote_worker_script_loads_through_the_fetch_seam() {
    let mut dom = parse_document("<body></body>").dom;
    let url = Url::parse(PAGE_URL).expect("valid URL");
    let mut runtime = JsRuntime::with_limits_and_url(&dom, RuntimeLimits::default(), &url);
    run(
        &mut dom,
        &mut runtime,
        "var got = 0; \
         var worker = new Worker('https://cdn.example.test/worker.js'); \
         worker.onmessage = function (event) { got = event.data; };",
    );
    // The constructor schedules the load as a task; running it queues a fetch.
    settle(&mut dom, &mut runtime);
    let requests = runtime.take_pending_fetch_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url, "https://cdn.example.test/worker.js");
    runtime.settle_fetch(
        &mut dom,
        requests[0].id,
        Ok(crate::FetchOutcome {
            status: 200,
            status_text: "OK".to_owned(),
            headers: Vec::new(),
            body: b"postMessage(11);".to_vec(),
        }),
    );
    settle(&mut dom, &mut runtime);
    assert_eq!(run(&mut dom, &mut runtime, "got"), "11");
}

#[test]
fn the_worker_scope_has_no_page_globals() {
    let documents = worker(&[(
        "scope.js",
        "postMessage([typeof window, typeof document, typeof importScripts, self === globalThis, self.name].join(','));",
    )]);
    let (mut dom, mut runtime) = page("<body></body>", documents);
    run(
        &mut dom,
        &mut runtime,
        "var got = ''; var worker = new Worker('scope.js', {name: 'upper'}); \
         worker.onmessage = function (event) { got = event.data; };",
    );
    settle(&mut dom, &mut runtime);
    assert_eq!(
        run(&mut dom, &mut runtime, "got"),
        "undefined,undefined,function,true,upper"
    );
    assert_eq!(
        run(&mut dom, &mut runtime, "typeof worker.terminate"),
        "function"
    );
}
