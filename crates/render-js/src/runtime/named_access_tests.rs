//! Named access on the window and the document (HTML §7.4.4, §3.1.5): an
//! element named by its `id`, or by its `name` where the element takes one, is a
//! property of `window` and `document`, below every property of the chain.

use crate::JsRuntime;
use render_html::parse_document;

/// Run `source` on `page`, and return the `log` array the script filled.
fn run(page: &str, source: &str) -> String {
    let mut dom = parse_document(page).dom;
    let mut runtime = JsRuntime::new(&dom);
    runtime.execute(&mut dom, "var log = [];").expect("setup");
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

const PAGE: &str = "<!doctype html><html><body><div id=host></div>\
    <img name=pic id=pic-id><img name=pic2><embed name=emb>\
    <div name=not-named></div><span id=shadowed></span>\
    <div id=toString></div></body></html>";

#[test]
fn an_element_id_is_a_property_of_window() {
    assert_eq!(
        run(
            PAGE,
            "log.push(host === document.getElementById('host'), window.host === host, \
             typeof host, 'host' in window);"
        ),
        "true,true,object,true"
    );
}

#[test]
fn a_name_is_named_only_by_the_elements_that_take_one() {
    assert_eq!(
        run(
            PAGE,
            "log.push(window.pic2.getAttribute('name'), window.emb.tagName, \
             window['not-named'] === undefined, 'not-named' in window);"
        ),
        "pic2,EMBED,true,false"
    );
}

#[test]
fn an_element_named_by_id_and_name_is_one_match() {
    assert_eq!(
        run(
            "<!doctype html><img id=dupe name=dupe>",
            "log.push(window.dupe.tagName, window.dupe instanceof HTMLCollection, \
             document.dupe === window.dupe);"
        ),
        "IMG,false,true"
    );
}

#[test]
fn several_matches_give_a_live_collection_in_tree_order() {
    assert_eq!(
        run(
            "<!doctype html><body><img name=m><div id=m></div></body>",
            "var before = window.m; \
             log.push(before instanceof HTMLCollection, before.length, before[0].tagName, \
             before[1].tagName); \
             var added = document.createElement('p'); added.id = 'm'; \
             document.body.appendChild(added); \
             log.push(before.length, window.m[2].tagName);"
        ),
        "true,2,IMG,DIV,3,P"
    );
}

#[test]
fn a_real_property_of_window_shadows_a_named_element() {
    assert_eq!(
        run(
            PAGE,
            "log.push(window.toString === Object.prototype.toString, \
             typeof window.toString, window.toString !== document.getElementById('toString'), \
             window.document === document);"
        ),
        "true,function,true,true"
    );
}

#[test]
fn a_user_value_shadows_the_named_element() {
    assert_eq!(
        run(
            "<!doctype html><span id=shadowed></span>",
            "window.shadowed = 5; log.push(window.shadowed, \
             Object.getOwnPropertyDescriptor(window, 'shadowed').value);"
        ),
        "5,5"
    );
}

#[test]
fn a_named_property_is_not_enumerable_or_own() {
    assert_eq!(
        run(
            PAGE,
            "var seen = false; for (var key in window) { if (key === 'host') seen = true; } \
             log.push(seen, window.hasOwnProperty('host'), \
             Object.getOwnPropertyDescriptor(window, 'host') === undefined);"
        ),
        "false,false,true"
    );
}

#[test]
fn a_named_property_follows_the_document() {
    assert_eq!(
        run(
            PAGE,
            "var target = document.getElementById('host'); target.id = 'moved'; \
             log.push(window.host === undefined, window.moved === target, 'host' in window); \
             target.removeAttribute('id'); log.push(window.moved === undefined);"
        ),
        "true,true,false,true"
    );
}

#[test]
fn a_detached_element_is_not_named() {
    assert_eq!(
        run(
            PAGE,
            "var detached = document.createElement('div'); detached.id = 'loose'; \
             log.push(window.loose === undefined, 'loose' in window); \
             document.body.appendChild(detached); log.push(window.loose === detached);"
        ),
        "true,false,true"
    );
}

#[test]
fn the_empty_name_names_nothing() {
    assert_eq!(
        run(
            "<!doctype html><div id=''></div><img name=''>",
            "log.push(window[''] === undefined, '' in window);"
        ),
        "true,false"
    );
}

#[test]
fn a_document_names_its_elements_too() {
    assert_eq!(
        run(
            PAGE,
            "log.push(document.host === document.getElementById('host'), \
             document.pic2 === window.pic2, document.constructor === HTMLDocument);"
        ),
        "true,true,true"
    );
}

#[test]
fn an_unnamed_identifier_is_still_not_defined() {
    assert_eq!(
        run(
            PAGE,
            "log.push(typeof nothingHere, 'nothingHere' in window); \
             try { nothingHere; } catch (error) { log.push(error.name); }"
        ),
        "undefined,false,ReferenceError"
    );
}

#[test]
fn the_window_prototype_chain_runs_through_the_named_properties_object() {
    assert_eq!(
        run(
            PAGE,
            "var npo = Object.getPrototypeOf(Window.prototype); \
             log.push(Object.getPrototypeOf(window) === Window.prototype, \
             Object.getPrototypeOf(npo) === EventTarget.prototype, \
             Object.getPrototypeOf(EventTarget.prototype) === Object.prototype, \
             Object.prototype.toString.call(window), \
             Object.prototype.toString.call(Window.prototype), \
             Object.prototype.toString.call(npo), \
             window.constructor === Window, \
             Window.prototype.constructor === Window, \
             Object.getPrototypeOf(Window) === EventTarget);"
        ),
        "true,true,true,[object Window],[object Window],[object WindowProperties],true,true,true"
    );
}

#[test]
fn window_constructor_is_a_non_writable_prototype_with_a_writable_global() {
    assert_eq!(
        run(
            PAGE,
            "var prototype = Object.getOwnPropertyDescriptor(Window, 'prototype'); \
             var global = Object.getOwnPropertyDescriptor(window, 'Window'); \
             log.push(typeof Window, prototype.writable, prototype.configurable, \
             global.writable, global.enumerable, global.configurable, \
             window.hasOwnProperty('Window'));"
        ),
        "function,false,false,true,false,true,true"
    );
}

#[test]
fn window_interface_object_is_not_constructible() {
    assert_eq!(
        run(
            PAGE,
            "try { new Window(); } catch (error) { log.push(error.name); } \
             try { Window(); } catch (error) { log.push(error.name); }"
        ),
        "TypeError,TypeError"
    );
}

#[test]
fn named_property_is_an_own_property_of_the_named_object_only() {
    assert_eq!(
        run(
            PAGE,
            "var npo = Object.getPrototypeOf(Window.prototype); \
             var desc = Object.getOwnPropertyDescriptor(npo, 'host'); \
             log.push(desc.value === host, desc.writable, desc.enumerable, desc.configurable, \
             Object.getOwnPropertyDescriptor(window, 'host'), \
             Object.getOwnPropertyDescriptor(Window.prototype, 'host'), \
             npo.hasOwnProperty('host'), Window.prototype.hasOwnProperty('host'), \
             'host' in Window.prototype, Window.prototype.host === host);"
        ),
        "true,true,false,true,,,true,false,true,true"
    );
}

#[test]
fn an_event_target_member_shadows_a_named_property_of_the_same_name() {
    assert_eq!(
        run(
            PAGE,
            "var npo = Object.getPrototypeOf(Window.prototype); \
             EventTarget.prototype.host = 'shadowing'; \
             log.push(window.host, Window.prototype.host, npo.host, \
             Object.getOwnPropertyDescriptor(npo, 'host'), npo.hasOwnProperty('host'), \
             'host' in npo);"
        ),
        "shadowing,shadowing,shadowing,,false,true"
    );
}

#[test]
fn a_window_member_shadows_the_named_property_below_it() {
    assert_eq!(
        run(
            PAGE,
            "Window.prototype.host = 'shadowing'; \
             log.push(window.host, Object.getOwnPropertyDescriptor(window, 'host'), \
             Object.getPrototypeOf(Window.prototype).host === document.getElementById('host'));"
        ),
        "shadowing,,true"
    );
}

#[test]
fn window_dispatch_event_reaches_the_window_handlers() {
    assert_eq!(
        run(
            PAGE,
            "window.onresize = function (e) { log.push(e.type, e.currentTarget === window); }; \
             window.dispatchEvent(new Event('resize')); \
             log.push(window.hasOwnProperty('dispatchEvent'));"
        ),
        "resize,true,true"
    );
}

#[test]
fn own_property_names_of_the_named_object_list_its_visible_names() {
    assert_eq!(
        run(
            PAGE,
            "var npo = Object.getPrototypeOf(Window.prototype); \
             var names = Object.getOwnPropertyNames(npo); \
             log.push(names.indexOf('host') >= 0, names.indexOf('pic-id') >= 0, \
             names.indexOf('shadowed') >= 0, names.indexOf('toString') >= 0, \
             Object.getOwnPropertyNames(window).indexOf('host') >= 0, \
             Object.keys(npo).length);"
        ),
        "true,true,true,false,false,0"
    );
}
