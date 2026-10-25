//! `HTMLDialogElement` (HTML §4.11.2): show, showModal, close, requestClose
//! and `returnValue`, through the real methods in
//! [`crate::runtime::builtins::dom_dialog`].

use crate::runtime::JsRuntime;
use render_html::parse_document;

const PAGE: &str = "<!doctype html><html><head><title>t</title></head><body>\
    <dialog id=closed><p>c</p></dialog>\
    <dialog id=opened open><p>o</p></dialog>\
    <div id=detached-host></div>\
    </body></html>";

fn eval(source: &str) -> String {
    let mut dom = parse_document(PAGE).dom;
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
fn show_adds_the_open_attribute_and_is_idempotent() {
    assert_eq!(
        eval(
            "var d = document.getElementById('closed'); d.show(); \
             [d.hasAttribute('open'), d.open, (d.show(), d.hasAttribute('open'))].join()"
        ),
        "true,true,true"
    );
}

#[test]
fn show_modal_refuses_an_open_dialog_and_a_detached_element() {
    assert_eq!(
        eval(
            "var out = []; \
             try { document.getElementById('opened').showModal(); } \
             catch (e) { out.push(e.name); } \
             var detached = document.createElement('dialog'); \
             try { detached.showModal(); } catch (e) { out.push(e.name); } \
             out.join()"
        ),
        "InvalidStateError,InvalidStateError"
    );
}

#[test]
fn show_modal_opens_a_connected_closed_dialog() {
    assert_eq!(
        eval(
            "var d = document.getElementById('closed'); d.showModal(); \
             [d.hasAttribute('open'), d.open].join()"
        ),
        "true,true"
    );
}

#[test]
fn close_removes_open_and_sets_the_return_value() {
    // HTML §4.11.2 through WPT's dialog-close.html: close(x) sets
    // returnValue to x; close() with no argument keeps the previous value.
    assert_eq!(
        eval(
            "var d = document.getElementById('opened'); \
             d.close('done'); \
             [d.hasAttribute('open'), d.returnValue].join()"
        ),
        "false,done"
    );
    assert_eq!(
        eval(
            "var d = document.getElementById('opened'); \
             d.returnValue = 'kept'; d.close(); \
             [d.hasAttribute('open'), d.returnValue].join()"
        ),
        "false,kept"
    );
}

#[test]
fn close_on_a_closed_dialog_is_a_no_op() {
    // WPT dialog-close.html: the steps abort, so returnValue is untouched.
    assert_eq!(
        eval(
            "var d = document.getElementById('closed'); d.close('nope'); \
             d.returnValue"
        ),
        ""
    );
}

#[test]
fn close_sets_the_idl_value_without_invoking_a_script_setter() {
    // WPT dialog-close.html defines a throwing setter on the prototype and on
    // the instance; close() must not run either.
    assert_eq!(
        eval(
            "var d = document.getElementById('opened'); \
             Object.defineProperty(HTMLDialogElement.prototype, 'returnValue', { \
                 set: function () { throw new Error('prototype setter ran'); }, \
                 configurable: true \
             }); \
             d.close('internal'); \
             [d.returnValue, d.hasAttribute('open')].join()"
        ),
        "internal,false"
    );
}

#[test]
fn close_queues_the_close_event_instead_of_running_it_inline() {
    // WPT dialog-close.html: the event must not have run by the time close()
    // returns, and must run afterwards with the standard's event init.
    assert_eq!(
        eval(
            "var out = []; \
             var d = document.getElementById('opened'); \
             d.onclose = function (event) { \
                 out.push(['close', String(event.isTrusted), String(event.bubbles), \
                           String(event.cancelable), queued].join(',')); \
             }; \
             var queued = false; \
             d.close(); queued = true; out.join('|')"
        ),
        // The handler has not run yet, so the string is still empty; the
        // event fires when the embedding drains.
        ""
    );
}

#[test]
fn the_queued_close_event_runs_with_the_standard_init() {
    let mut dom = parse_document(PAGE).dom;
    let mut runtime = JsRuntime::new(&dom);
    runtime
        .execute(
            &mut dom,
            "var seen = null; \
             document.getElementById('opened').onclose = function (event) { \
                 seen = [event.type, String(event.isTrusted), String(event.bubbles), \
                         String(event.cancelable)].join(','); \
             }; \
             document.getElementById('opened').close();",
        )
        .expect("setup");
    for microtask in runtime.take_pending_microtasks() {
        runtime
            .invoke_microtask(&mut dom, microtask)
            .expect("drain queued close events");
    }
    let value = runtime
        .execute(&mut dom, "seen")
        .expect("read")
        .value
        .to_js_string()
        .to_lossy();
    assert_eq!(value, "close,true,false,false");
}

#[test]
fn request_close_fires_cancel_then_queues_close() {
    assert_eq!(
        eval(
            "var events = []; \
             var d = document.getElementById('opened'); \
             d.addEventListener('cancel', function () { events.push('cancel'); }); \
             d.addEventListener('close', function () { events.push('close'); }); \
             d.requestClose('x'); \
             [d.hasAttribute('open'), d.returnValue, events.join(',')].join()"
        ),
        // close is queued, so only cancel has run at this point.
        "false,x,cancel"
    );
}

#[test]
fn a_prevented_cancel_keeps_the_dialog_open() {
    assert_eq!(
        eval(
            "var d = document.getElementById('opened'); \
             d.addEventListener('cancel', function (event) { event.preventDefault(); }); \
             d.requestClose(); \
             [d.hasAttribute('open'), d.returnValue].join()"
        ),
        "true,"
    );
}

#[test]
fn request_close_on_a_closed_dialog_is_a_no_op() {
    assert_eq!(
        eval(
            "var events = 0; \
             var d = document.getElementById('closed'); \
             d.addEventListener('cancel', function () { events++; }); \
             d.requestClose(); \
             String(events)"
        ),
        "0"
    );
}

#[test]
fn the_methods_reject_non_dialog_receivers() {
    assert_eq!(
        eval(
            "var out = []; \
             try { document.getElementById('detached-host').showModal(); } \
             catch (e) { out.push(e instanceof TypeError); } \
             try { document.getElementById('detached-host').close(); } \
             catch (e) { out.push(e instanceof TypeError); } \
             out.join()"
        ),
        "true,true"
    );
}
