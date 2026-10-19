//! Form collections and form-control naming (HTML §3.1.5, §4.10.2 to §4.10.4):
//! `document.forms`, `form.elements`, `fieldset.elements`, `form.length`, the
//! indexed and named members of a form, `control.form`, and `RadioNodeList`.

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

/// Two forms and a fieldset. `f1` holds a button, two radio buttons named `radio`,
/// an `input type=image` (never a control of `elements`) and an `object`.
const PAGE: &str = "<!doctype html><html><body>\
    <form id=f1><input type=button name=button>\
    <input type=radio name=radio value=x><input type=radio name=radio value=y>\
    <input type=image name=picture><object name=obj></object></form>\
    <form id=f2 name=named></form>\
    <fieldset id=fs><input name=inside></fieldset></body></html>";

#[test]
fn document_forms_is_a_live_html_collection_of_the_forms() {
    assert_eq!(
        run(
            PAGE,
            "var forms = document.forms; log.push(forms.length, forms[0].id, \
             forms.item(1).id, forms.namedItem('f2').id, forms.f1.id, \
             forms instanceof HTMLCollection, String(forms.namedItem('')));"
        ),
        "2,f1,f2,f2,f1,true,null"
    );
    assert_eq!(
        run(
            PAGE,
            "var forms = document.forms; var extra = document.createElement('form'); \
             document.body.appendChild(extra); log.push(forms.length, \
             document.forms === forms); extra.remove(); log.push(forms.length);"
        ),
        "3,true,2"
    );
}

#[test]
fn a_form_is_named_by_its_id_or_name_in_its_collections() {
    assert_eq!(
        run(
            PAGE,
            "log.push(document.forms.named === document.getElementById('f2'), \
             Object.getOwnPropertyNames(document.forms).join('|'));"
        ),
        "true,0|1|f1|f2|named"
    );
}

#[test]
fn form_elements_are_the_listed_controls_with_this_form_as_owner() {
    assert_eq!(
        run(
            PAGE,
            "var f = document.getElementById('f1'); var els = f.elements; \
             log.push(els.length, f.length, els[0].name, els[1].name, els[2].value, \
             els instanceof HTMLFormControlsCollection, els instanceof HTMLCollection, \
             f.elements === els, els[3].name, String(els[-1]), String(els[4]));"
        ),
        "4,4,button,radio,y,true,true,true,obj,undefined,undefined"
    );
}

#[test]
fn a_name_is_an_element_or_a_radio_node_list_when_several_match() {
    assert_eq!(
        run(
            PAGE,
            "var f = document.getElementById('f1'); var list = f.radio; \
             log.push(f.elements['radio'] instanceof RadioNodeList, list instanceof RadioNodeList, \
             list instanceof NodeList, list.length, f.button.tagName, \
             f.elements.namedItem('button') === f.button, String(f.elements.namedItem('picture')));"
        ),
        "true,true,true,2,INPUT,true,null"
    );
}

#[test]
fn radio_node_list_value_is_the_first_checked_radio_value() {
    assert_eq!(
        run(
            PAGE,
            "var f = document.getElementById('f1'); var list = f.radio; \
             log.push(JSON.stringify(list.value)); \
             f.radio[1].setAttribute('checked', ''); log.push(list.value); \
             list.value = 'x'; log.push(list.value, f.radio[0].hasAttribute('checked'), \
             f.radio[1].hasAttribute('checked')); list.value = 'missing'; \
             log.push(list.value);"
        ),
        "\"\",y,x,true,false,x"
    );
}

#[test]
fn a_form_member_is_not_writable_deletable_or_redefinable() {
    assert_eq!(
        run(
            PAGE,
            "var f = document.getElementById('f1'); var old = f.button; \
             f.button = 5; log.push(f.button === old); \
             try { (function () { 'use strict'; f.button = 5; })(); log.push('no'); } \
             catch (e) { log.push(e.constructor.name); } \
             log.push(delete f.button, f.button === old); \
             try { Object.defineProperty(f, 'button', { value: 5 }); log.push('no'); } \
             catch (e) { log.push(e.constructor.name); }"
        ),
        "true,TypeError,false,true,TypeError"
    );
}

#[test]
fn a_form_member_has_the_descriptor_the_web_idl_gives_a_named_property() {
    assert_eq!(
        run(
            PAGE,
            "var f = document.getElementById('f1'); var d = \
             Object.getOwnPropertyDescriptor(f, 'button'); \
             log.push(d.value === f.button, d.writable, d.enumerable, d.configurable, \
             f.hasOwnProperty('button'), 'button' in f);"
        ),
        "true,false,false,true,true,true"
    );
}

#[test]
fn an_indexed_form_member_is_enumerable_and_refuses_delete() {
    assert_eq!(
        run(
            PAGE,
            "var f = document.getElementById('f1'); var old = f[0]; \
             var d = Object.getOwnPropertyDescriptor(f, 0); \
             log.push(d.value === old, d.enumerable, d.configurable, d.writable); \
             Object.prototype[0] = 5; log.push(f[0] === old); delete Object.prototype[0]; \
             delete f[0]; log.push(f[0] === old, String(f[4]));"
        ),
        "true,true,true,false,true,true,undefined"
    );
}

#[test]
fn an_expando_wins_over_a_name_that_is_added_later() {
    assert_eq!(
        run(
            PAGE,
            "var f = document.getElementById('f1'); f['late'] = 5; \
             var input = document.createElement('input'); input.name = 'late'; \
             f.appendChild(input); log.push(f['late']); delete f['late']; \
             log.push(f['late'] === input);"
        ),
        "5,true"
    );
}

#[test]
fn a_named_form_member_overrides_the_builtins_of_the_form() {
    assert_eq!(
        run(
            "<!doctype html><form id=f><input name=length><input name=elements></form>",
            "var f = document.getElementById('f'); log.push(f.length.name, \
             f.elements.name, f.elements.length === undefined);"
        ),
        "length,elements,true"
    );
}

#[test]
fn a_control_belongs_to_its_nearest_form_and_to_the_form_attribute() {
    assert_eq!(
        run(
            "<!doctype html><html><body></body></html>",
            "var outer = document.createElement('form'); outer.id = 'outer'; \
             document.body.appendChild(outer); \
             var inner = document.createElement('form'); inner.id = 'inner'; \
             outer.appendChild(inner); \
             var i = document.createElement('input'); i.id = 'i'; inner.appendChild(i); \
             var o = document.createElement('input'); o.id = 'other'; \
             o.setAttribute('form', 'outer'); document.body.appendChild(o); \
             var s = document.createElement('input'); s.id = 'stray'; \
             s.setAttribute('form', 'nope'); document.body.appendChild(s); \
             log.push(i.form.id, o.form.id, String(s.form), outer.elements.length, \
             inner.elements.length);"
        ),
        "inner,outer,null,1,1"
    );
}

#[test]
fn a_control_removed_from_its_form_has_no_owner() {
    assert_eq!(
        run(
            "<!doctype html><form id=f><input id=i></form>",
            "var f = document.getElementById('f'); var i = document.getElementById('i'); \
             f.remove(); log.push(i.form === f, f.elements.length); i.remove(); \
             log.push(String(i.form), f.elements.length);"
        ),
        "true,1,null,0"
    );
}

#[test]
fn a_fieldset_elements_is_an_html_collection_of_its_listed_descendants() {
    assert_eq!(
        run(
            PAGE,
            "var fs = document.getElementById('fs'); log.push(fs.elements instanceof \
             HTMLCollection, fs.elements.length, fs.elements[0].name, \
             fs.elements.inside === undefined, String(fs.form));"
        ),
        "true,1,inside,true,null"
    );
}

#[test]
fn form_members_are_the_listed_controls_only_and_not_images() {
    assert_eq!(
        run(
            "<!doctype html><form id=f><img name=pic><object name=o></object>\
             <output name=out></output><select name=s></select>\
             <textarea name=t></textarea><button name=b></button></form>",
            "var f = document.getElementById('f'); log.push(f.elements.length, \
             String(f.pic), f.o.tagName, f.out.form === f, f.s.form === f);"
        ),
        "5,undefined,OBJECT,true,true"
    );
}

#[test]
fn an_html_collection_names_its_items_by_id_and_name() {
    assert_eq!(
        run(
            "<!doctype html><div id=wrap><a id=ai name=an></a><span id=sp name=sn></span></div>",
            "var c = document.getElementById('wrap').children; log.push(c.ai.tagName, \
             c.sp.tagName, c.an.tagName, String(c.sn), c.namedItem('sp').tagName, \
             Object.getOwnPropertyNames(c).join('|'));"
        ),
        "A,SPAN,A,undefined,SPAN,0|1|ai|sp|an"
    );
}
