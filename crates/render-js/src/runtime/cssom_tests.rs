//! `document.styleSheets` and the `CSSStyleSheet` surface (CSSOM §6).
//!
//! The cascade reads style text from the DOM, so these tests pin the property
//! that matters to pages: the CSSOM and the cascade observe the same rules.
//! A `insertRule` that only updated a JavaScript-side list would pass a
//! presence check while leaving the page unstyled.

use crate::JsRuntime;
use render_html::parse_document;

const PAGE: &str = "<!doctype html><html><head>\
    <style id=s1>a { color: red }</style>\
    <link id=l1 rel=stylesheet href='/a.css'>\
    <link id=l2 rel=alternate href='/b.css'>\
    </head><body><style id=s2>p{color:blue}</style></body></html>";

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
fn document_style_sheets_lists_style_and_stylesheet_links_in_tree_order() {
    assert_eq!(
        eval(
            "var sheets = document.styleSheets; \
             [sheets.length, sheets[0].ownerNode.id, sheets[1].ownerNode.id, \
              sheets[2].ownerNode.id, sheets.item(3)].join(',')"
        ),
        "3,s1,l1,s2,"
    );
    assert_eq!(
        eval("String(document.styleSheets[3])"),
        "undefined",
        "indexed access past the end is undefined"
    );
    assert_eq!(
        eval(
            "[document.styleSheets === document.styleSheets, document.styleSheets instanceof StyleSheetList].join()"
        ),
        "true,true"
    );
}

#[test]
fn an_element_sheet_is_the_same_object_the_document_list_holds() {
    assert_eq!(
        eval(
            "[$('s1').sheet === document.styleSheets[0], \
              $('l1').sheet === document.styleSheets[1], \
              String($('l2').sheet), \
              $('s1').sheet instanceof CSSStyleSheet, \
              $('s1').sheet instanceof StyleSheet, \
              $('s1').sheet.ownerNode === $('s1'), \
              $('s1').sheet.type].join()"
        ),
        "true,true,undefined,true,true,true,text/css"
    );
}

#[test]
fn a_style_sheets_rule_list_is_read_from_the_element_text() {
    assert_eq!(
        eval(
            "var rules = $('s1').sheet.cssRules; \
             [rules.length, rules[0].cssText, rules[0].type, rules.item(1), \
              rules instanceof CSSRuleList, rules[0] instanceof CSSRule, \
              $('s1').sheet.cssRules === rules, $('s1').sheet.rules === rules].join('|')"
        ),
        "1|a { color: red }|1||true|true|true|true"
    );
}

#[test]
fn rule_interfaces_are_globals_and_instances_get_the_typed_prototype() {
    assert_eq!(
        eval(
            "var sheet = $('s2').sheet; \
             sheet.insertRule('@media (min-width: 1px) { p { color: red } }', 0); \
             var rules = sheet.cssRules; \
             [typeof CSSStyleRule, typeof CSSMediaRule, typeof CSSGroupingRule, \
              CSSStyleRule.prototype instanceof CSSRule, \
              CSSMediaRule.prototype instanceof CSSGroupingRule, \
              CSSGroupingRule.prototype instanceof CSSRule, \
              rules[0] instanceof CSSMediaRule, rules[1] instanceof CSSStyleRule, \
              rules[0].constructor.name, rules[1].constructor.name].join('|')"
        ),
        "function|function|function|true|true|true|true|true|CSSMediaRule|CSSStyleRule"
    );
}

#[test]
fn insert_rule_rewrites_the_style_element_and_the_rule_list() {
    assert_eq!(
        eval(
            "var sheet = $('s2').sheet; \
             var index = sheet.insertRule('b { color: green }', 1); \
             var rules = sheet.cssRules; \
             [index, rules.length, rules[1].cssText, $('s2').textContent].join('|')"
        ),
        "1|2|b { color: green }|p{color:blue}\nb { color: green }"
    );
    assert_eq!(
        eval(
            "var sheet = $('s2').sheet; \
             sheet.insertRule('b { color: green }', 1); \
             sheet.insertRule('i { color: black }', 0); \
             sheet.deleteRule(1); \
             [$('s2').textContent, sheet.cssRules.length].join('|')"
        ),
        "i { color: black }\nb { color: green }|2"
    );
}

#[test]
fn insert_rule_moves_the_cached_rule_wrappers_with_their_rules() {
    // The wrapper cache is keyed by rule position, the way CSSRuleList is
    // live; a stale wrapper would hand back the previous rule's text.
    assert_eq!(
        eval(
            "var sheet = $('s2').sheet; \
             sheet.insertRule('b { color: green }', 1); \
             var first = sheet.cssRules[0]; \
             sheet.insertRule('i { color: black }', 0); \
             [first.cssText, sheet.cssRules[1].cssText].join('|')"
        ),
        "i { color: black }|p{color:blue}"
    );
}

#[test]
fn invalid_rule_text_and_out_of_range_indices_are_dom_exceptions() {
    assert_eq!(
        eval(
            "var out = []; \
             try { $('s1').sheet.insertRule('not a rule', 0); } \
             catch (e) { out.push(e.name, e instanceof DOMException); } \
             try { $('s1').sheet.insertRule('a { color: red }', 2); } \
             catch (e) { out.push(e.name); } \
             try { $('s1').sheet.deleteRule(1); } \
             catch (e) { out.push(e.name); } \
             out.join()"
        ),
        "SyntaxError,true,IndexSizeError,IndexSizeError"
    );
}

#[test]
fn insert_rule_consumes_one_rule_and_ignores_the_rest() {
    assert_eq!(
        eval(
            "var sheet = $('s2').sheet; \
             var index = sheet.insertRule('b { color: green } i { color: black }', 1); \
             [index, sheet.cssRules.length, sheet.cssRules[1].cssText].join('|')"
        ),
        "1|2|b { color: green }"
    );
}

#[test]
fn a_link_sheets_rules_are_a_cssom_only_list() {
    // A link's CSS was fetched as a resource and is not a DOM text child, so
    // the sheet keeps programmatic rules in the CSSOM list. The test pins the
    // boundary so a future loader can move them into the cascade.
    assert_eq!(
        eval(
            "var sheet = $('l1').sheet; \
             sheet.insertRule('a { color: red }', 0); \
             [sheet.cssRules.length, sheet.cssRules[0].cssText, $('l1').textContent].join('|')"
        ),
        "1|a { color: red }|"
    );
}
