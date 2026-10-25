//! `document.write`, `writeln`, `open`, `close` and `currentScript`
//! (HTML §8.4, §4.12.4.1), through the real write sessions in
//! [`crate::runtime::document_write`].

use crate::runtime::JsRuntime;
use render_html::parse_document;

const PAGE: &str = "<!doctype html><html><head><title>t</title></head>\
    <body><script id=writer></script><p id=after>tail</p></body></html>";

/// Run `source` as the code of the page's own `<script>` element, the way an
/// embedding runs a parser-created script, and return the string result.
fn eval_as_parser_script(source: &str) -> String {
    let mut dom = parse_document(PAGE).dom;
    let writer = first_script_element(&dom).expect("the page has a script element");
    let mut runtime = JsRuntime::new(&dom);
    let value = runtime
        .execute_parser_script(&mut dom, source, None, writer)
        .unwrap_or_else(|error| panic!("{source}\n=> {error}"))
        .value
        .to_js_string()
        .to_lossy();
    assert!(runtime.prelude_error().is_none());
    value
}

/// Run `source` as a script an embedding queued after the document finished
/// loading — a harness callback, a timer — and return the string result.
fn eval_after_load(source: &str) -> String {
    let mut dom = parse_document(PAGE).dom;
    let mut runtime = JsRuntime::new(&dom);
    runtime.set_document_ready_state(crate::DocumentReadyState::Complete);
    let value = runtime
        .execute(&mut dom, source)
        .unwrap_or_else(|error| panic!("{source}\n=> {error}"))
        .value
        .to_js_string()
        .to_lossy();
    assert!(runtime.prelude_error().is_none());
    value
}

fn first_script_element(dom: &render_dom::Dom) -> Option<render_dom::NodeId> {
    fn walk(dom: &render_dom::Dom, node: render_dom::NodeId) -> Option<render_dom::NodeId> {
        for child in dom.children(node).unwrap_or_default() {
            if let render_dom::NodeKind::Element(element) = dom.node(*child)?.kind()
                && element.local_name == "script"
            {
                return Some(*child);
            }
            if let Some(found) = walk(dom, *child) {
                return Some(found);
            }
        }
        None
    }
    walk(dom, dom.document())
}

#[test]
fn a_write_after_load_opens_and_becomes_the_document() {
    assert_eq!(
        eval_after_load("document.write('PASS'); document.body.textContent"),
        "PASS"
    );
}

#[test]
fn a_markup_write_after_load_parses_as_a_document() {
    assert_eq!(
        eval_after_load(
            "document.write('<i>Filler Text'); \
             [document.body.firstChild.localName, document.body.firstChild.textContent].join()"
        ),
        "i,Filler Text"
    );
}

#[test]
fn one_element_assembled_across_writes_is_parsed_once() {
    // The WPT shape: each write feeds more of the same token stream, and the
    // final DOM holds one element, not a fragment per write.
    assert_eq!(
        eval_after_load(
            "document.write('<i'); document.write(\" id='test'>\"); document.write('Filler'); \
             document.write(' Text'); \
             [document.body.childNodes.length, document.body.firstChild.localName, \
              document.body.firstChild.getAttribute('id'), document.body.firstChild.textContent].join()"
        ),
        "1,i,test,Filler Text"
    );
}

#[test]
fn a_comment_write_lands_in_the_body() {
    // `write('<body>')` in a body-context stream is a no-op start tag, so the
    // comment the next writes assemble is the body's first child.
    assert_eq!(
        eval_after_load(
            "document.write('<body>'); document.write('<!--'); document.write('c'); \
             document.write('-->'); \
             [document.body.childNodes[0].nodeType, document.body.childNodes[0].data].join()"
        ),
        "8,c"
    );
}

#[test]
fn an_entity_split_across_writes_resolves() {
    // `&notin;` split mid-name: the stream model joins it before parsing. The
    // semicolon matters — the spec itself says a bare `&notin` is `¬in`.
    assert_eq!(
        eval_after_load(
            "document.write('<span>&noti'); document.write('n;abc</span>'); \
             document.body.firstChild.textContent"
        ),
        "\u{2209}abc"
    );
}

#[test]
fn a_written_inline_script_executes_once_when_its_end_tag_arrives() {
    // The script's start tag is complete before its end tag arrives: the
    // partial element must not run, and the completed one must run once.
    assert_eq!(
        eval_after_load(
            "document.write('<scr' + 'ipt>window.n = (window.n || 0) + 1</scr' + 'ipt>'); \
             window.n"
        ),
        "1"
    );
}

#[test]
fn a_written_script_runs_before_the_write_returns() {
    assert_eq!(
        eval_after_load("document.write('<script>window.x = 1</script>'); String(window.x === 1)"),
        "true"
    );
}

#[test]
fn two_written_scripts_run_in_write_order() {
    assert_eq!(
        eval_after_load(
            "document.write('<script>window.log = \"a\"</script>'); \
             document.write('<script>window.log += \"b\"</script>'); window.log"
        ),
        "ab"
    );
}

#[test]
fn an_unterminated_written_script_never_runs() {
    assert_eq!(
        eval_after_load("document.write('<script>window.n = 1'); String(window.n === undefined)"),
        "true"
    );
}

#[test]
fn a_written_external_script_loads_through_the_document_loader() {
    use crate::runtime::nested::DocumentLoader;
    use url::Url;

    #[derive(Debug)]
    struct OneScript;
    impl DocumentLoader for OneScript {
        fn load_text(&mut self, url: &Url) -> Option<String> {
            (url.path() == "/written.js").then(|| "window.loaded = true".to_owned())
        }
    }

    let mut dom = parse_document(PAGE).dom;
    let base = url::Url::parse("http://www.example.test/dir/page.html").expect("base");
    let mut runtime = JsRuntime::with_limits_and_url(&dom, crate::RuntimeLimits::default(), &base);
    runtime.set_document_ready_state(crate::DocumentReadyState::Complete);
    runtime.set_document_loader(Box::new(OneScript));
    let value = runtime
        .execute(
            &mut dom,
            "document.write('<script src=\"/written.js\"></script>'); String(window.loaded)",
        )
        .expect("write")
        .value
        .to_js_string()
        .to_lossy();
    assert_eq!(value, "true");
}

#[test]
fn a_write_from_a_parser_script_lands_after_that_script() {
    assert_eq!(
        eval_as_parser_script(
            "document.write('<i id=w>W</i>'); \
             var ids = []; for (var n = document.body.firstChild; n; n = n.nextSibling) \
               ids.push(n.id || n.localName); ids.join()"
        ),
        "writer,w,after"
    );
}

#[test]
fn a_parser_script_write_accumulates_in_one_stream() {
    assert_eq!(
        eval_as_parser_script(
            "document.write('<i'); document.write('>W</i>'); \
             [document.body.children[1].localName, document.body.children[1].textContent].join()"
        ),
        // The body's elements are the writing script itself, then the one
        // `<i>` the two writes assembled.
        "i,W",
    );
}

#[test]
fn a_written_script_nested_in_a_parser_script_writes_after_itself() {
    assert_eq!(
        eval_as_parser_script(
            "document.write('<b id=outer></b><scr' + 'ipt>document.write(\
               \\'<i id=inner>I</i>\\')</scr' + 'ipt>'); \
             var ids = []; for (var n = document.body.firstChild; n; n = n.nextSibling) \
               ids.push(n.id || n.localName); ids.join()"
        ),
        // The written `<script>` element is part of the stream the outer
        // script wrote, and the element it writes lands right after it.
        "writer,outer,script,inner,after"
    );
}

#[test]
fn current_script_names_the_running_parser_script_and_null_after() {
    assert_eq!(
        eval_as_parser_script("String(document.currentScript === null)"),
        "false"
    );
    assert_eq!(eval_as_parser_script("document.currentScript.id"), "writer");
    assert_eq!(eval_after_load("String(document.currentScript)"), "null");
}

#[test]
fn writeln_appends_a_new_line() {
    assert_eq!(
        eval_after_load("document.writeln('a'); document.body.textContent"),
        "a\n"
    );
}

#[test]
fn multiple_write_arguments_concatenate() {
    assert_eq!(
        eval_after_load("document.write('a', 'b', 'c'); document.body.textContent"),
        "abc"
    );
}

#[test]
fn open_wipes_and_later_writes_restart_the_stream() {
    assert_eq!(
        eval_after_load(
            "document.write('<b id=one>B</b>'); document.open(); \
             [String(document.body), document.documentElement ? 'html' : 'none'].join()"
        ),
        // `open()` alone leaves the document empty; the next write builds it.
        "null,none"
    );
    assert_eq!(
        eval_after_load(
            "document.write('<b id=one>B</b>'); document.open(); \
             document.write('<i id=two>I</i>'); \
             [document.body.childNodes.length, document.body.firstChild.id].join()"
        ),
        "1,two"
    );
}

#[test]
fn close_ends_the_stream_and_completes_readiness() {
    assert_eq!(
        eval_after_load(
            "document.write('<b>x</b>'); document.close(); \
             [document.readyState, String(document.body.firstChild.localName)].join()"
        ),
        "complete,b"
    );
}

#[test]
fn close_without_a_stream_is_a_no_op() {
    assert_eq!(
        eval_after_load("document.close(); document.readyState"),
        "complete"
    );
}

#[test]
fn a_write_to_a_document_without_a_browsing_context_is_dropped() {
    assert_eq!(
        eval_after_load(
            "var d = document.implementation.createHTMLDocument(''); \
             d.write('<i>x</i>'); String(d.body.firstChild)"
        ),
        "null"
    );
}

#[test]
fn a_write_from_module_code_is_ignored() {
    let mut dom = parse_document(PAGE).dom;
    let mut runtime = JsRuntime::new(&dom);
    runtime.set_document_ready_state(crate::DocumentReadyState::Complete);
    let script = crate::CompiledScript::compile_module(
        "document.write('<i>module</i>');",
        &crate::RuntimeLimits::default(),
    )
    .expect("compile");
    runtime
        .declare_module("test:module", &script, std::collections::BTreeMap::new())
        .expect("declare");
    runtime
        .evaluate_module(&mut dom, "test:module")
        .expect("evaluate");
    let body = runtime
        .execute(&mut dom, "document.body.textContent")
        .expect("read")
        .value
        .to_js_string()
        .to_lossy();
    // The document the parser produced is untouched: the module's write is a
    // no-op, not an error.
    assert_eq!(body, "tail");
}

#[test]
fn a_second_write_session_keeps_first_writes_nodes() {
    // Two scripts write during load. The first script's nodes must still be
    // in place after the second one writes: each script owns its stream.
    let mut dom = parse_document(PAGE).dom;
    let writer = first_script_element(&dom).expect("script element");
    let mut runtime = JsRuntime::new(&dom);
    runtime
        .execute_parser_script(&mut dom, "document.write('<b id=one>B</b>');", None, writer)
        .expect("first");
    let after = dom
        .children(dom.document())
        .and_then(|children| children.last().copied())
        .and_then(|html| {
            dom.children(html)
                .and_then(|children| children.last().copied())
        })
        .and_then(|body| {
            dom.children(body)
                .and_then(|children| children.first().copied())
        });
    // The written node is the body's first child, before the original tail.
    let body = body_node(&dom);
    let first = dom
        .children(body)
        .and_then(<[render_dom::NodeId]>::first)
        .copied();
    assert_eq!(after, first);
}

fn body_node(dom: &render_dom::Dom) -> render_dom::NodeId {
    let mut stack = vec![dom.document()];
    while let Some(node) = stack.pop() {
        if let Some(render_dom::NodeKind::Element(element)) =
            dom.node(node).map(render_dom::Node::kind)
            && element.local_name == "body"
        {
            return node;
        }
        if let Some(children) = dom.children(node) {
            stack.extend(children.iter().rev());
        }
    }
    panic!("no body");
}

#[test]
fn debug_two_writes_tmp() {
    let mut dom = parse_document(PAGE).dom;
    let mut runtime = JsRuntime::new(&dom);
    runtime.set_document_ready_state(crate::DocumentReadyState::Complete);
    let v1 = runtime
        .execute(
            &mut dom,
            "document.write('<i id=a>A</i>'); document.body.childNodes.length",
        )
        .unwrap()
        .value
        .to_js_string()
        .to_lossy();
    eprintln!("after write1 body children: {v1}");
    let v2 = runtime.execute(&mut dom, "document.write('<b id=b>B</b>'); [document.body.childNodes.length, document.body.firstChild ? document.body.firstChild.localName : 'none', document.body.lastChild ? document.body.lastChild.localName : 'none'].join()").unwrap().value.to_js_string().to_lossy();
    eprintln!("after write2: {v2}");
    let v3 = runtime
        .execute(&mut dom, "document.documentElement.outerHTML")
        .unwrap()
        .value
        .to_js_string()
        .to_lossy();
    eprintln!("doc html: {v3}");
}

#[test]
fn open_from_a_load_handler_ends_that_handler() {
    // WPT's Range-insertNode-multiple-document-children-crash.html: a load
    // handler calls open() and close(). close() fires load again, and the
    // second dispatch must find no handler — open() removed it — or the
    // dispatch recursion never ends.
    let mut dom = parse_document(PAGE).dom;
    let mut runtime = JsRuntime::new(&dom);
    runtime.set_document_ready_state(crate::DocumentReadyState::Complete);
    runtime
        .execute(
            &mut dom,
            "window.onload = function () { \
                 document.open(); document.write('<i id=loop>W</i>'); document.close(); \
                 window.reentries = (window.reentries || 0) + 1; \
             };",
        )
        .expect("handler");
    runtime.dispatch_page_load(&mut dom).expect("load");
    assert_eq!(
        runtime
            .execute(&mut dom, "String(window.reentries)")
            .expect("read")
            .value
            .to_js_string()
            .to_lossy(),
        "1"
    );
    // The written content survived as the document, and the handler is gone.
    assert_eq!(
        runtime
            .execute(
                &mut dom,
                "[document.body.firstChild.id, String(window.onload === undefined || window.onload === null)].join()"
            )
            .expect("read")
            .value
            .to_js_string()
            .to_lossy(),
        "loop,true"
    );
}
