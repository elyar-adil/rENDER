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
        .to_lossy()
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
        .to_js_string()
        .to_lossy();
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

#[test]
fn return_value_cancels_only_a_cancelable_event_and_true_is_a_no_op() {
    // DOM §2.2: `returnValue = false` sets the canceled flag on a cancelable
    // event; `returnValue = true` never clears it, and `defaultPrevented` is read-only.
    assert_eq!(
        run(
            "var plain = new Event('x'); plain.returnValue = false; log.push(plain.returnValue, plain.defaultPrevented);"
        ),
        "true,false"
    );
    assert_eq!(
        run(
            "var c = new Event('x', { cancelable: true }); c.returnValue = false; log.push(c.returnValue, c.defaultPrevented); c.returnValue = true; log.push(c.returnValue); c.defaultPrevented = false; log.push(c.defaultPrevented);"
        ),
        "false,true,false,true"
    );
}

#[test]
fn cancel_bubble_only_sets_and_clears_after_dispatch() {
    // DOM §2.2: the `cancelBubble` setter ignores `false`. The dispatch unsets
    // the stop-propagation flag when it ends, so the flag does not outlive it.
    assert_eq!(
        run(
            "var e = new Event('x'); e.cancelBubble = true; log.push(e.cancelBubble); e.cancelBubble = false; log.push(e.cancelBubble);"
        ),
        "true,true"
    );
    assert_eq!(
        run(
            "var ev = new Event('x', { bubbles: true }); leaf.addEventListener('x', function (e) { e.stopPropagation(); log.push('leaf'); }); leaf.dispatchEvent(ev); log.push(ev.cancelBubble);"
        ),
        "leaf,false"
    );
}

#[test]
fn prevent_default_during_dispatch_sets_the_canceled_flag_for_good() {
    assert_eq!(
        run(
            "var ev = new Event('x', { cancelable: true }); leaf.addEventListener('x', function (e) { e.preventDefault(); }); log.push(leaf.dispatchEvent(ev), ev.defaultPrevented, ev.returnValue);"
        ),
        "false,true,false"
    );
}

#[test]
fn init_event_reinitialises_the_flags_and_the_fields() {
    // DOM §2.2 initEvent: clears the canceled and stop-propagation flags and
    // sets type, bubbles and cancelable.
    assert_eq!(
        run(
            "var e = new Event('x', { cancelable: true }); e.preventDefault(); e.cancelBubble = true; e.initEvent('y', true, true); log.push(e.type, e.bubbles, e.cancelable, e.defaultPrevented, e.returnValue, e.cancelBubble);"
        ),
        "y,true,true,false,true,false"
    );
}

#[test]
fn touch_and_wheel_listeners_are_passive_by_default_on_the_document_and_body() {
    // DOM §2.10 "default passive value": touchstart, touchmove, wheel and
    // mousewheel listeners are passive on the document and its body unless
    // `passive` is given explicitly.
    assert_eq!(
        run(
            "document.addEventListener('wheel', function (e) { e.preventDefault(); log.push(e.defaultPrevented); }); \
             log.push(document.dispatchEvent(new Event('wheel', { cancelable: true })));"
        ),
        "false,true"
    );
    assert_eq!(
        run(
            "document.body.addEventListener('touchmove', function (e) { e.preventDefault(); }); \
             log.push(document.body.dispatchEvent(new Event('touchmove', { cancelable: true })));"
        ),
        "true"
    );
    // The default does not reach other nodes or other types, and an explicit
    // `passive: false` overrides it.
    assert_eq!(
        run(
            "leaf.addEventListener('touchstart', function (e) { e.preventDefault(); }); \
             document.addEventListener('touchend', function (e) { e.preventDefault(); }); \
             document.addEventListener('wheel', function (e) { e.preventDefault(); }, { passive: false }); \
             log.push(leaf.dispatchEvent(new Event('touchstart', { cancelable: true })), \
                      document.dispatchEvent(new Event('touchend', { cancelable: true })), \
                      document.dispatchEvent(new Event('wheel', { cancelable: true })));"
        ),
        "false,false,false"
    );
}

#[test]
fn create_event_makes_an_uninitialized_event_that_init_event_sets_up() {
    // DOM §4.5: `createEvent("Event")` has an empty type until `initEvent`.
    assert_eq!(
        eval_dom(
            "var e = document.createEvent('Event'); var before = [e.type, e.bubbles, e.cancelable].join('/'); \
             e.initEvent('foo', true, false); before + ';' + e.type + '/' + e.bubbles + '/' + e.cancelable"
        ),
        "/false/false;foo/true/false"
    );
}

#[test]
fn time_stamp_is_coarsened_to_a_hundred_microseconds() {
    // HR-Time §5.4: the clock is coarsened, so a timestamp is a whole number of
    // hundredths of a millisecond.
    assert_eq!(
        eval_dom(
            "var t = new Event('x').timeStamp; String(Math.abs(Math.round(t * 10) - t * 10) < 1e-6)"
        ),
        "true"
    );
}

/// Run `source` as a classic script that throws, then read the log the page's
/// `seen` array collected. The script's name is given as its `filename`.
fn uncaught_error_log(setup: &str, source: &str, filename: &str) -> String {
    let mut dom = parse_document("<!doctype html><html><body></body></html>").dom;
    let mut runtime = JsRuntime::new(&dom);
    runtime
        .execute(&mut dom, &format!("var seen = []; {setup}"))
        .expect("setup");
    assert!(
        runtime
            .execute_with_filename(&mut dom, source, filename)
            .is_err(),
        "the script was meant to throw"
    );
    runtime
        .execute(&mut dom, "seen.join(';')")
        .expect("read the log")
        .value
        .to_js_string()
        .to_lossy()
}

#[test]
fn window_onerror_is_called_with_the_five_error_event_arguments() {
    // HTML 8.1.8.1: an ErrorEvent reaches `window.onerror` as (message, filename,
    // lineno, colno, error), and the filename is the script's own URL.
    assert_eq!(
        uncaught_error_log(
            "window.onerror = function (message, filename, lineno, colno, error) { \
             seen.push([arguments.length, typeof message, filename, \
             error instanceof Error, error.message].join('|')); };",
            "throw new Error('boom')",
            "http://web-platform.test/resources/helper.js",
        ),
        "5|string|http://web-platform.test/resources/helper.js|true|boom"
    );
}

#[test]
fn an_error_listener_gets_the_event_and_not_the_onerror_arguments() {
    // `addEventListener('error')` is an ordinary listener: one argument, the event.
    assert_eq!(
        uncaught_error_log(
            "window.addEventListener('error', function (event) { \
             seen.push([arguments.length, event.type, event.message].join('|')); });",
            "throw new Error('boom')",
            "http://web-platform.test/a.js",
        ),
        "1|error|boom"
    );
}

#[test]
fn window_onerror_runs_once_for_an_uncaught_error_and_not_as_a_plain_listener() {
    // The generic `on<type>` path must not also call `window.onerror` with the
    // event object: if it did, the log would hold a second entry.
    assert_eq!(
        uncaught_error_log(
            "window.onerror = function () { seen.push('onerror:' + arguments.length); };",
            "throw new Error('boom')",
            "http://web-platform.test/a.js",
        ),
        "onerror:5"
    );
}

#[test]
fn an_inline_script_reports_the_document_as_filename_and_the_name_does_not_linger() {
    // A script read from a URL names that URL only while it runs. The next script
    // that is not read from a URL reports the document, as an inline script does.
    let mut dom = parse_document("<!doctype html><html><body></body></html>").dom;
    let mut runtime = JsRuntime::new(&dom);
    runtime
        .execute(
            &mut dom,
            "var seen = []; window.onerror = function (message, filename) { seen.push(filename); };",
        )
        .expect("setup");
    assert!(
        runtime
            .execute_with_filename(&mut dom, "throw new Error('named')", "http://x.test/n.js")
            .is_err()
    );
    assert!(
        runtime
            .execute(&mut dom, "throw new Error('inline')")
            .is_err()
    );
    let log = runtime
        .execute(&mut dom, "seen.join(';') + '#' + location.href")
        .expect("read the log")
        .value
        .to_js_string()
        .to_lossy();
    let (names, href) = log.split_once('#').expect("log separator");
    assert_eq!(names, format!("http://x.test/n.js;{href}"));
}
