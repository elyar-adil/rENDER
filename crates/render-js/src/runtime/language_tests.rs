//! Core-language behaviour that real bundles depend on. Each case is a
//! complete script evaluated in a fresh realm.

use crate::{JsErrorKind, JsRuntime};
use render_html::parse_document;

fn eval(source: &str) -> Result<String, crate::JsError> {
    let mut dom = parse_document("<!doctype html><html><body></body></html>").dom;
    let mut runtime = JsRuntime::new(&dom);
    runtime
        .execute(&mut dom, source)
        .map(|outcome| outcome.value.to_js_string())
}

fn ok(source: &str) -> String {
    eval(source).unwrap_or_else(|error| panic!("{source}\n=> {error}"))
}

#[test]
fn optional_member_access_short_circuits_the_whole_chain() {
    assert_eq!(ok("var a = null; String(a?.b)"), "undefined");
    assert_eq!(ok("var a = null; String(a?.b.c.d)"), "undefined");
    assert_eq!(ok("var a = {b: {c: 4}}; a?.b.c"), "4");
    assert_eq!(ok("var a; String(a?.[0])"), "undefined");
    assert_eq!(ok("var a = [7]; a?.[0]"), "7");
    assert_eq!(ok("var o = {a: null}; String(o.a?.b?.c)"), "undefined");
}

#[test]
fn optional_call_keeps_the_receiver_and_skips_arguments() {
    assert_eq!(
        ok("var o = {n: 3, f() { return this.n; }}; o.f?.() + o?.f()"),
        "6"
    );
    assert_eq!(ok("var o = {}; String(o.f?.())"), "undefined");
    assert_eq!(
        ok("var hits = 0; var o = {}; o.f?.(hits++); o?.f(hits++); hits"),
        "0"
    );
    assert_eq!(ok("var f; String(f?.(1))"), "undefined");
}

#[test]
fn optional_chain_does_not_swallow_errors_after_the_guard() {
    let error = eval("var a = {b: null}; a?.b.c").expect_err("b is null");
    assert_eq!(error.kind(), JsErrorKind::Type);
}

#[test]
fn optional_chain_ends_at_a_group_and_the_conditional_operator_still_parses() {
    assert_eq!(ok("var a = null; (a?.b) === undefined"), "true");
    assert_eq!(ok("var a = true; a?.5:1"), "0.5");
    assert_eq!(ok("var x = 1; x ?.5 : 2"), "0.5");
}

#[test]
fn function_parameters_destructure() {
    assert_eq!(
        ok("(function({a, b}) { return a + b; })({a: 1, b: 2})"),
        "3"
    );
    assert_eq!(ok("(function([a, b]) { return a + b; })([1, 2])"), "3");
    assert_eq!(
        ok("(function({a, b} = {a: 5, b: 1}) { return a + b; })()"),
        "6"
    );
    assert_eq!(
        ok(
            "(function(x, {y, z: [w]}, ...r) { return x + y + w + r.length; })(1, {y: 2, z: [3]}, 9, 9)"
        ),
        "8"
    );
    assert_eq!(
        ok("function f({a = 4, ...rest}) { return a + Object.keys(rest).join(); } f({b: 1})"),
        "4b"
    );
    assert_eq!(ok("({m({a}) { return a; }}).m({a: 7})"), "7");
    assert_eq!(
        ok("class K { m([a, b]) { return b; } } new K().m([1, 2])"),
        "2"
    );
}

#[test]
fn math_has_its_constants_and_functions() {
    assert_eq!(ok("Math.PI.toFixed(5)"), "3.14159");
    assert_eq!(ok("Math.E.toFixed(3)"), "2.718");
    assert_eq!(ok("Math.sin(0) + Math.cos(0)"), "1");
    assert_eq!(ok("Math.atan2(1, 1).toFixed(4)"), "0.7854");
    assert_eq!(
        ok("Math.log(Math.E) + Math.log2(8) + Math.log10(1000)"),
        "7"
    );
    assert_eq!(ok("Math.exp(0)"), "1");
    assert_eq!(
        ok("[Math.sign(-3), Math.sign(0), Math.sign(2), String(Math.sign(NaN))].join()"),
        "-1,0,1,NaN"
    );
    assert_eq!(
        ok("[Math.trunc(-1.7), Math.trunc(1.7), Math.cbrt(27)].join()"),
        "-1,1,3"
    );
    assert_eq!(ok("Math.hypot(3, 4)"), "5");
    assert_eq!(ok("Math.hypot()"), "0");
    assert_eq!(ok("Math.imul(0xffffffff, 5)"), "-5");
    assert_eq!(ok("Math.clz32(1)"), "31");
    assert_eq!(ok("Math.fround(5.5)"), "5.5");
}

#[test]
fn math_round_matches_the_specification() {
    assert_eq!(ok("Math.round(0.49999999999999994)"), "0");
    assert_eq!(ok("Math.round(2.5)"), "3");
    assert_eq!(ok("Math.round(-2.5)"), "-2");
    assert_eq!(ok("1 / Math.round(-0.2)"), "-Infinity");
}

#[test]
fn number_predicates_do_not_coerce() {
    assert_eq!(
        ok(
            "[Number.isInteger(5), Number.isInteger(5.5), Number.isInteger('5'), Number.isInteger(Infinity)].join()"
        ),
        "true,false,false,false"
    );
    assert_eq!(
        ok("[Number.isFinite(1), Number.isFinite('1'), Number.isFinite(1/0)].join()"),
        "true,false,false"
    );
    assert_eq!(
        ok("[Number.isNaN(NaN), Number.isNaN('x'), Number.isNaN(1)].join()"),
        "true,false,false"
    );
    assert_eq!(
        ok("[Number.isSafeInteger(2**53), Number.isSafeInteger(2**53 - 1)].join()"),
        "false,true"
    );
}

#[test]
fn array_from_and_collections_use_the_iteration_protocol() {
    assert_eq!(ok("Array.from(new Set([1, 2, 2])).join()"), "1,2");
    assert_eq!(ok("Array.from(new Map([[1, 'a']]).values()).join()"), "a");
    assert_eq!(
        ok("Array.from([1, 2].values(), function (x) { return x * 2; }).join()"),
        "2,4"
    );
    assert_eq!(ok("Array.from({length: 2}).length"), "2");
    assert_eq!(ok("Array.from(5).length"), "0");
    assert_eq!(
        ok(
            "var it = { [Symbol.iterator]() { var i = 0; return { next() { return i < 3 ? {value: i++, done: false} : {done: true}; } }; } }; Array.from(it).join()"
        ),
        "0,1,2"
    );
    assert_eq!(ok("new Set([1, 2].values()).size"), "2");
    assert_eq!(ok("new Map(new Map([[1, 2]]).entries()).get(1)"), "2");
    assert_eq!(ok("new Set('aab').size"), "2");
    assert_eq!(eval("new Set(5)").unwrap_err().kind(), JsErrorKind::Type);
}
