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
fn create_range_and_boundary_setters_set_the_boundary_points() {
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

#[test]
fn get_selection_is_one_live_object_per_document() {
    assert_eq!(
        eval(
            "var s = getSelection(); \
             [s === document.getSelection(), window.getSelection() === s, s instanceof Selection, \
              Object.prototype.toString.call(s), s.rangeCount, s.type, s.isCollapsed, \
              String(s.anchorNode), s.anchorOffset, String(s.focusNode), s.focusOffset, \
              s.toString(), s.direction].join('|')"
        ),
        "true|true|true|[object Selection]|0|None|true|null|0|null|0||none"
    );
}

#[test]
fn a_collapsed_selection_has_caret_geometry_and_to_string_names_the_text() {
    assert_eq!(
        eval(
            "var t = $('para').firstChild; var w = $('bold').firstChild; \
             var s = getSelection(); s.collapse(t, 2); \
             var caret = [s.anchorNode === t, s.anchorOffset, s.focusNode === t, s.focusOffset, \
                          s.isCollapsed, s.rangeCount, s.type, s.toString()].join('|'); \
             s.setBaseAndExtent(t, 2, w, 3); \
             caret + ' ' + [s.toString(), s.type, s.direction, s.isCollapsed].join('|')"
        ),
        "true|2|true|2|true|1|Caret| llo wor|Range|forward|false"
    );
}

#[test]
fn get_range_at_returns_the_selection_range_by_reference() {
    assert_eq!(
        eval(
            "var t = $('para').firstChild; var w = $('bold').firstChild; var s = getSelection(); \
             s.setBaseAndExtent(t, 2, w, 3); var r = s.getRangeAt(0); \
             var same = [s.getRangeAt(0) === r, r.startContainer === t, r.startOffset, \
                         r.endContainer === w, r.endOffset, r.collapsed, s.isCollapsed, s.rangeCount].join(); \
             var failure = []; \
             try { s.getRangeAt(1); } catch (e) { failure.push(e.name, e instanceof DOMException); } \
             same + ' ' + failure.join()"
        ),
        "true,true,2,true,3,false,false,1 IndexSizeError,true"
    );
}

#[test]
fn add_range_keeps_only_the_first_range_and_remove_all_ranges_empties_it() {
    assert_eq!(
        eval(
            "var one = $('one'); var three = $('three'); \
             var r1 = document.createRange(); r1.selectNodeContents(one); \
             var r2 = document.createRange(); r2.selectNodeContents(three); \
             var s = getSelection(); s.addRange(r1); s.addRange(r2); \
             var adopted = [s.rangeCount, s.getRangeAt(0) === r1, s.toString(), \
                            s.anchorNode === one, s.focusNode === one, s.type].join(); \
             r1.setStart(one.firstChild, 0); r1.setEnd(one.firstChild, 0); \
             var mutated = [s.isCollapsed, s.type, s.toString()].join(); \
             s.removeAllRanges(); \
             adopted + ' ' + mutated + ' ' + [s.rangeCount, s.type, s.isCollapsed, \
                                              String(s.anchorNode), s.toString()].join()"
        ),
        // The adopted range's own mutations are reflected, because the selection
        // holds that range object rather than a copy.
        "1,true,1,true,true,Range true,Caret, 0,None,true,null,"
    );
}

#[test]
fn remove_range_set_position_and_empty_operate_on_the_single_range() {
    assert_eq!(
        eval(
            "var one = $('one'), two = $('two'); var r = document.createRange(); \
             r.selectNodeContents(one); var s = getSelection(); s.addRange(r); \
             var adopted = [s.rangeCount, s.getRangeAt(0) === r].join(); \
             s.removeRange(r); var removed = [s.rangeCount, s.type].join(); \
             var again = ''; \
             try { s.removeRange(r); } catch (e) { again = e.name; } \
             s.setPosition(two, 1); \
             var position = [s.rangeCount, s.anchorNode === two, s.anchorOffset, s.type].join(); \
             s.empty(); \
             [adopted, removed, again, position, s.rangeCount].join(' ')"
        ),
        "1,true 0,None NotFoundError 1,true,1,Caret 0"
    );
}

#[test]
fn collapse_and_extend_follow_the_anchor_focus_direction() {
    assert_eq!(
        eval(
            "var t = $('para').firstChild; var w = $('bold').firstChild; var s = getSelection(); \
             s.setBaseAndExtent(w, 3, t, 1); \
             var back = [s.direction, s.anchorNode === w, s.anchorOffset, s.focusNode === t, \
                         s.focusOffset, s.getRangeAt(0).startContainer === t, \
                         s.getRangeAt(0).endContainer === w].join(); \
             s.extend(w, 2); \
             var extended = [s.direction, s.anchorNode === w, s.anchorOffset, s.focusNode === w, \
                             s.focusOffset, s.type].join(); \
             s.collapseToEnd(); \
             var end = [s.isCollapsed, s.anchorNode === w, s.anchorOffset, s.type].join(); \
             s.setBaseAndExtent(t, 2, w, 3); s.collapseToStart(); \
             var start = [s.isCollapsed, s.anchorNode === t, s.anchorOffset, s.type].join(); \
             var empty = ''; \
             try { s.removeAllRanges(); s.collapseToStart(); } catch (e) { empty = e.name; } \
             [back, extended, end, start, empty].join(' ')"
        ),
        "backward,true,3,true,1,true,true backward,true,3,true,2,Range \
         true,true,3,Caret true,true,2,Caret InvalidStateError"
    );
}

#[test]
fn select_all_children_and_delete_from_document_change_the_tree() {
    assert_eq!(
        eval(
            "var p = $('para'); var s = getSelection(); s.selectAllChildren(p); \
             var selected = s.toString(); \
             var contains = [s.containsNode(p), s.containsNode(p, true), s.containsNode($('bold'))].join(); \
             s.deleteFromDocument(); \
             [selected, contains, p.childNodes.length, p.textContent.length, s.isCollapsed, \
              s.type, s.toString(), s.rangeCount].join()"
        ),
        "hello world,true,true,true,0,0,true,Caret,,1"
    );
}

#[test]
fn removing_a_node_moves_the_selection_out_of_it_and_never_leaves_a_dangling_node() {
    assert_eq!(
        eval(
            "var p = $('para'); var t = p.firstChild; var s = getSelection(); s.collapse(t, 2); \
             p.removeChild(t); \
             var text = [s.anchorNode === p, s.anchorOffset, s.focusNode === p, s.focusOffset, \
                         s.isCollapsed, s.type, s.rangeCount].join(); \
             var one = $('one'); s.selectAllChildren(one); $('list').removeChild(one); \
             text + ' ' + [s.anchorNode === $('list'), s.anchorOffset, s.isCollapsed].join()"
        ),
        "true,0,true,0,true,Caret,1 true,0,true"
    );
}

#[test]
fn two_documents_have_independent_selections() {
    assert_eq!(
        eval(
            "var d2 = document.implementation.createHTMLDocument('two'); \
             d2.body.textContent = 'second'; \
             var main = document.createElement('p'); main.textContent = 'first'; \
             document.body.appendChild(main); \
             var s1 = getSelection(); var s2 = d2.getSelection(); \
             s1.selectAllChildren(main); s2.selectAllChildren(d2.body); \
             [s1 !== s2, s1.rangeCount, s2.rangeCount, s1.toString(), s2.toString(), \
              s2 instanceof Selection, d2.getSelection() === s2].join()"
        ),
        "true,1,1,first,second,true,true"
    );
}
