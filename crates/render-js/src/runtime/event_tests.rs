//! DOM event dispatch: the capture / target / bubble path and the listener
//! options that govern it.

use crate::JsRuntime;
use render_html::parse_document;

fn run(source: &str) -> String {
    let mut dom = parse_document(
        "<!doctype html><html><body><div id=outer><div id=inner><button id=leaf>x</button></div></div></body></html>",
    )
    .dom;
    let mut runtime = JsRuntime::new(&dom);
    runtime
        .execute(
            &mut dom,
            "var log = []; var outer = document.getElementById('outer'); \
             var inner = document.getElementById('inner'); var leaf = document.getElementById('leaf');",
        )
        .expect("setup");
    runtime
        .execute(&mut dom, source)
        .unwrap_or_else(|error| panic!("{source}\n=> {error}"));
    assert!(runtime.prelude_error().is_none());
    runtime
        .execute(&mut dom, "log.join(',')")
        .expect("log")
        .value
        .to_js_string()
}

#[test]
fn capture_runs_outside_in_then_bubble_inside_out() {
    assert_eq!(
        run(
            "outer.addEventListener('x', function () { log.push('outer-capture'); }, true); \
             outer.addEventListener('x', function () { log.push('outer-bubble'); }); \
             inner.addEventListener('x', function () { log.push('inner-capture'); }, { capture: true }); \
             inner.addEventListener('x', function () { log.push('inner-bubble'); }); \
             leaf.addEventListener('x', function () { log.push('leaf'); }); \
             window.addEventListener('x', function () { log.push('window-bubble'); }); \
             window.addEventListener('x', function () { log.push('window-capture'); }, true); \
             leaf.dispatchEvent(new Event('x', { bubbles: true }));"
        ),
        "window-capture,outer-capture,inner-capture,leaf,inner-bubble,outer-bubble,window-bubble"
    );
}

#[test]
fn a_non_bubbling_event_only_captures_and_hits_the_target() {
    assert_eq!(
        run(
            "outer.addEventListener('x', function () { log.push('outer-capture'); }, true); \
             outer.addEventListener('x', function () { log.push('outer-bubble'); }); \
             leaf.addEventListener('x', function () { log.push('leaf'); }); \
             leaf.dispatchEvent(new Event('x'));"
        ),
        "outer-capture,leaf"
    );
}

#[test]
fn at_the_target_capturing_listeners_run_before_the_others() {
    assert_eq!(
        run(
            "leaf.addEventListener('x', function () { log.push('plain-1'); }); \
             leaf.addEventListener('x', function () { log.push('capture'); }, true); \
             leaf.addEventListener('x', function () { log.push('plain-2'); }); \
             leaf.dispatchEvent(new Event('x'));"
        ),
        "capture,plain-1,plain-2"
    );
}

#[test]
fn stop_propagation_ends_the_path_but_not_the_current_stop() {
    assert_eq!(
        run(
            "leaf.addEventListener('x', function (e) { log.push('leaf-1'); e.stopPropagation(); }); \
             leaf.addEventListener('x', function () { log.push('leaf-2'); }); \
             outer.addEventListener('x', function () { log.push('outer'); }); \
             leaf.dispatchEvent(new Event('x', { bubbles: true }));"
        ),
        "leaf-1,leaf-2"
    );
    assert_eq!(
        run(
            "outer.addEventListener('x', function (e) { log.push('outer-capture'); e.stopPropagation(); }, true); \
             leaf.addEventListener('x', function () { log.push('leaf'); }); \
             leaf.dispatchEvent(new Event('x', { bubbles: true }));"
        ),
        "outer-capture"
    );
}

#[test]
fn stop_immediate_propagation_also_skips_later_listeners_on_the_same_target() {
    assert_eq!(
        run(
            "leaf.addEventListener('x', function (e) { log.push('a'); e.stopImmediatePropagation(); }); \
             leaf.addEventListener('x', function () { log.push('b'); }); \
             leaf.onx = null; leaf.dispatchEvent(new Event('x', { bubbles: true }));"
        ),
        "a"
    );
}

#[test]
fn once_and_remove_event_listener() {
    assert_eq!(
        run(
            "var n = 0; leaf.addEventListener('x', function () { n++; }, { once: true }); \
             leaf.dispatchEvent(new Event('x')); leaf.dispatchEvent(new Event('x')); log.push(n);"
        ),
        "1"
    );
    assert_eq!(
        run("function handler() { log.push('ran'); } \
             leaf.addEventListener('x', handler); leaf.addEventListener('x', handler); \
             leaf.dispatchEvent(new Event('x')); \
             leaf.removeEventListener('x', handler, true); leaf.dispatchEvent(new Event('x')); \
             leaf.removeEventListener('x', handler); leaf.dispatchEvent(new Event('x'));"),
        "ran,ran"
    );
    assert_eq!(
        run("function second() { log.push('second'); } \
             leaf.addEventListener('x', function () { log.push('first'); leaf.removeEventListener('x', second); }); \
             leaf.addEventListener('x', second); leaf.dispatchEvent(new Event('x'));"),
        "first"
    );
}

#[test]
fn handle_event_objects_are_listeners() {
    assert_eq!(
        run(
            "var h = { handleEvent: function (e) { log.push(e.type + (this === h)); } }; \
             leaf.addEventListener('x', h); leaf.dispatchEvent(new Event('x'));"
        ),
        "xtrue"
    );
}

#[test]
fn event_properties_describe_the_dispatch() {
    assert_eq!(
        run("leaf.addEventListener('x', function (e) { \
               log.push(e.eventPhase, e.target === leaf, e.currentTarget === leaf, e.srcElement === leaf, \
                        e.composedPath().length, e.isTrusted, typeof e.timeStamp); }); \
             var ev = new Event('x', { bubbles: true }); leaf.dispatchEvent(ev); \
             log.push(ev.eventPhase, ev.currentTarget, ev.composedPath().length);"),
        "2,true,true,true,7,false,number,0,,0"
    );
    assert_eq!(
        run(
            "outer.addEventListener('x', function (e) { log.push(e.eventPhase, e.target === leaf, e.currentTarget === outer); }, true); \
             outer.addEventListener('x', function (e) { log.push(e.eventPhase); }); \
             leaf.dispatchEvent(new Event('x', { bubbles: true }));"
        ),
        "1,true,true,3"
    );
}

#[test]
fn prevent_default_and_passive_listeners() {
    assert_eq!(
        run(
            "leaf.addEventListener('x', function (e) { e.preventDefault(); }); \
             log.push(leaf.dispatchEvent(new Event('x', { cancelable: true })), \
                      leaf.dispatchEvent(new Event('x')));"
        ),
        "false,true"
    );
    assert_eq!(
        run(
            "leaf.addEventListener('y', function (e) { e.preventDefault(); log.push(e.defaultPrevented); }, { passive: true }); \
             log.push(leaf.dispatchEvent(new Event('y', { cancelable: true })));"
        ),
        "false,true"
    );
}

#[test]
fn a_throwing_listener_does_not_stop_the_others() {
    assert_eq!(
        run(
            "var reported = []; window.addEventListener('error', function (e) { reported.push(e.message); }); \
             leaf.addEventListener('x', function () { throw new Error('boom'); }); \
             leaf.addEventListener('x', function () { log.push('after'); }); \
             outer.addEventListener('x', function () { log.push('outer'); }); \
             leaf.dispatchEvent(new Event('x', { bubbles: true })); log.push(reported.length);"
        ),
        "after,outer,1"
    );
}

#[test]
fn on_handler_properties_run_with_the_bubble_listeners() {
    assert_eq!(
        run(
            "leaf.addEventListener('x', function () { log.push('listener'); }); \
             leaf.onx = function () { log.push('handler'); }; leaf.dispatchEvent(new Event('x'));"
        ),
        "listener,handler"
    );
}

#[test]
fn subclassed_events_carry_their_own_fields() {
    assert_eq!(
        run(
            "class Tick extends Event { constructor(n) { super('tick', { bubbles: true }); this.n = n; } } \
             outer.addEventListener('tick', function (e) { log.push(e instanceof Tick, e.n); }); \
             leaf.dispatchEvent(new Tick(3));"
        ),
        "true,3"
    );
}

fn eval_dom(source: &str) -> String {
    let mut dom = parse_document(
        "<!doctype html><html><head><title>t</title></head><body><div id=a>text<!--c--><svg id=s><circle/></svg><input id=i></div></body></html>",
    )
    .dom;
    let mut runtime = JsRuntime::new(&dom);
    let value = runtime
        .execute(&mut dom, source)
        .unwrap_or_else(|error| panic!("{source}\n=> {error}"))
        .value
        .to_js_string();
    assert!(runtime.prelude_error().is_none());
    value
}

#[test]
fn nodes_inherit_from_their_real_interfaces() {
    assert_eq!(
        eval_dom(
            "var div = document.getElementById('a'); var text = div.firstChild; var comment = div.childNodes[1]; \
             var input = document.getElementById('i'); var svg = document.getElementById('s'); \
             [div instanceof HTMLDivElement, div instanceof HTMLElement, div instanceof Element, div instanceof Node, \
              div instanceof EventTarget, div instanceof Text, \
              text instanceof Text, text instanceof CharacterData, text instanceof Node, text instanceof Element, \
              comment instanceof Comment, comment instanceof CharacterData, comment instanceof Text, \
              input instanceof HTMLInputElement, input instanceof HTMLDivElement, \
              svg instanceof SVGElement, svg instanceof SVGSVGElement, svg instanceof HTMLElement, svg instanceof Element, \
              document instanceof Document, document instanceof Node, document instanceof Element, \
              document.createDocumentFragment() instanceof DocumentFragment].join()"
        ),
        "true,true,true,true,true,false,\
         true,true,true,false,\
         true,true,false,\
         true,false,\
         true,true,false,true,\
         true,true,false,\
         true"
    );
}

#[test]
fn interface_objects_have_names_constants_and_a_chain() {
    assert_eq!(
        eval_dom(
            "[Node.TEXT_NODE, Node.ELEMENT_NODE, Node.DOCUMENT_POSITION_CONTAINS, HTMLElement.name, \
              Object.getPrototypeOf(HTMLElement) === Element, Object.getPrototypeOf(Element) === Node, \
              Object.getPrototypeOf(HTMLDivElement.prototype) === HTMLElement.prototype, \
              Object.prototype.toString.call(document.getElementById('a')), \
              Object.prototype.toString.call(document.createTextNode('x')), \
              document.getElementById('a').constructor === HTMLDivElement, \
              HTMLVideoElement.prototype instanceof HTMLMediaElement].join()"
        ),
        "3,1,8,HTMLElement,true,true,true,[object HTMLDivElement],[object Text],true,true"
    );
}

#[test]
fn text_comment_and_fragment_are_constructible() {
    assert_eq!(
        eval_dom(
            "var t = new Text('hi'); var c = new Comment('note'); var f = new DocumentFragment(); \
             f.appendChild(t); document.body.appendChild(f); \
             [t instanceof Text, t.nodeType, t.data, c.nodeType, c.data, f.childNodes.length, \
              document.body.lastChild === t].join()"
        ),
        "true,3,hi,8,note,0,true"
    );
    assert_eq!(
        eval_dom("try { new Element(); } catch (e) { e instanceof TypeError }"),
        "true"
    );
    assert_eq!(
        eval_dom("try { Text('x'); } catch (e) { e instanceof TypeError }"),
        "true"
    );
}
