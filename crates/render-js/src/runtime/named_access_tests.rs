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
