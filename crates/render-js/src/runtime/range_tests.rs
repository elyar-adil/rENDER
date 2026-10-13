//! `Range`, `StaticRange` and `Document.createRange()` (DOM §5) through script.

use crate::JsRuntime;
use render_html::parse_document;

const PAGE: &str = "<!doctype html><html><head><title>t</title></head><body>\
    <ul id=list><li id=one>1</li><li id=two>2</li><li id=three>3</li></ul>\
    <p id=para>hello <b id=bold>world</b></p></body></html>";

fn eval(source: &str) -> String {
    let mut dom = parse_document(PAGE).dom;
    let mut runtime = JsRuntime::new(&dom);
    runtime
        .execute(
            &mut dom,
            "var $ = function (id) { return document.getElementById(id); };",
        )
        .expect("setup");
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
fn a_new_range_is_collapsed_at_the_start_of_the_document() {
    assert_eq!(
        eval(
            "var r = new Range(); [r.startContainer === document, r.startOffset, r.endOffset, r.collapsed, r.commonAncestorContainer === document].join()"
        ),
        "true,0,0,true,true"
    );
}

#[test]
fn create_range_and_boundary_setters_describe_the_selection() {
    assert_eq!(
        eval(
            "var list = $('list'); var r = document.createRange(); \
             r.setStart(list, 1); r.setEnd(list, 3); \
             [r.startContainer.id, r.startOffset, r.endContainer.id, r.endOffset, r.collapsed].join()"
        ),
        "list,1,list,3,false"
    );
}

#[test]
fn a_live_range_follows_insertions_and_removals() {
    assert_eq!(
        eval(
            "var list = $('list'); var r = document.createRange(); \
             r.setStart(list, 1); r.setEnd(list, 3); \
             list.insertBefore(document.createElement('x'), list.firstChild); \
             var after_insert = [r.startOffset, r.endOffset].join(); \
             list.removeChild(list.firstChild); \
             after_insert + ' ' + [r.startOffset, r.endOffset].join()"
        ),
        "2,4 1,3"
    );
}

#[test]
fn set_start_past_the_node_length_is_an_index_size_error() {
    assert_eq!(
        eval(
            "var list = $('list'); var r = document.createRange(); \
             try { r.setStart(list, 99); 'no error'; } \
             catch (e) { [e.name, e instanceof DOMException, e.code].join(); }"
        ),
        "IndexSizeError,true,1"
    );
}

#[test]
fn set_start_before_the_root_is_an_invalid_node_type_error() {
    assert_eq!(
        eval(
            "var r = document.createRange(); \
             try { r.setStartBefore(document); 'no error'; } \
             catch (e) { e.name; }"
        ),
        "InvalidNodeTypeError"
    );
}

#[test]
fn compare_boundary_points_and_compare_point_order_the_ranges() {
    assert_eq!(
        eval(
            "var list = $('list'); var a = document.createRange(); a.setStart(list, 0); a.setEnd(list, 1); \
             var b = document.createRange(); b.setStart(list, 2); b.setEnd(list, 3); \
             [a.compareBoundaryPoints(Range.START_TO_START, b), b.compareBoundaryPoints(Range.START_TO_START, a), \
              a.comparePoint(list, 0), a.comparePoint(list, 2), a.isPointInRange(list, 1), a.intersectsNode($('one'))].join()"
        ),
        "-1,1,0,1,true,true"
    );
}

#[test]
fn a_static_range_is_not_updated_by_mutation() {
    assert_eq!(
        eval(
            "var list = $('list'); \
             var s = new StaticRange({startContainer: list, startOffset: 1, endContainer: list, endOffset: 3}); \
             list.insertBefore(document.createElement('x'), list.firstChild); \
             [s.startOffset, s.endOffset, s instanceof StaticRange, s instanceof AbstractRange, 'setStart' in s].join()"
        ),
        "1,3,true,true,false"
    );
}

#[test]
fn split_text_and_normalize_keep_the_range_on_its_text() {
    assert_eq!(
        eval(
            "var p = $('para'); var t = p.firstChild; \
             var r = document.createRange(); r.setStart(t, 3); r.setEnd(t, 5); \
             var rest = t.splitText(2); \
             var inside = [t.data, rest.data, r.startContainer === rest, r.startOffset, r.endOffset].join(); \
             p.normalize(); \
             inside + ' ' + [p.childNodes.length, p.firstChild.data, r.startContainer === p.firstChild, r.startOffset].join()"
        ),
        "he,llo ,true,1,3 2,hello ,true,3"
    );
}

#[test]
fn extract_and_clone_contents_return_fragments() {
    assert_eq!(
        eval(
            "var list = $('list'); var r = document.createRange(); \
             r.setStart(list, 0); r.setEnd(list, 2); \
             var copy = r.cloneContents(); \
             var before = list.childNodes.length; \
             var moved = r.extractContents(); \
             [copy.childNodes.length, copy.firstChild.id, before, list.childNodes.length, moved.childNodes.length, \
              r.collapsed, moved.nodeType].join()"
        ),
        "2,one,3,1,2,true,11"
    );
}

#[test]
fn to_string_joins_the_selected_text() {
    assert_eq!(
        eval(
            "var p = $('para'); var r = document.createRange(); \
             r.setStart(p.firstChild, 2); r.setEnd(p.lastChild.firstChild, 3); r.toString()"
        ),
        "llo wor"
    );
}

#[test]
fn a_range_method_on_a_static_range_is_an_illegal_invocation() {
    assert_eq!(
        eval(
            "var s = new StaticRange({startContainer: document, startOffset: 0, endContainer: document, endOffset: 0}); \
             try { Range.prototype.setStart.call(s, document, 0); 'no error'; } \
             catch (e) { e.name; }"
        ),
        "TypeError"
    );
}

#[test]
fn a_range_constructor_without_new_is_refused() {
    assert_eq!(
        eval("try { Range(); 'no error'; } catch (e) { e.name; }"),
        "TypeError"
    );
}
