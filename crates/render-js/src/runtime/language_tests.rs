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

#[test]
fn regexp_named_groups_and_lookbehind_at_the_script_level() {
    assert_eq!(
        ok(
            "var m = /(?<y>\\d{4})-(?<m>\\d\\d)/.exec('on 2020-05'); m.groups.y + '/' + m.groups.m + '/' + m.index"
        ),
        "2020/05/3"
    );
    assert_eq!(ok("String(/a/.exec('a').groups)"), "undefined");
    assert_eq!(
        ok("'2020-05'.replace(/(?<y>\\d+)-(?<m>\\d+)/, '$<m>/$<y>')"),
        "05/2020"
    );
    assert_eq!(
        ok(
            "'a1'.replace(/(?<d>\\d)/, function (m, p1, offset, whole, groups) { return '[' + groups.d + offset + ']'; })"
        ),
        "a[11]"
    );
    assert_eq!(ok("'$10 $20'.match(/(?<=\\$)\\d+/g).join()"), "10,20");
    assert_eq!(
        ok("'abcdefghijkl'.replace(/(a)(b)(c)(d)(e)(f)(g)(h)(i)(j)(k)/, '$11-$1')"),
        "k-al"
    );
    assert_eq!(ok("/\\p{L}+/u.exec('héllo!')[0]"), "héllo");
}

#[test]
fn long_inputs_do_not_overflow_the_native_stack() {
    // A run of one character class is matched iteratively, however long.
    assert_eq!(ok("'a'.repeat(200000).replace(/a*/, 'x')"), "x");
    assert_eq!(ok("/^[^\\n]*$/.test('line '.repeat(40000))"), "true");
    // A group body recurses, so it has a bound; inside it the answer is right...
    assert_eq!(ok("/^(?:a|b)*$/.test('ab'.repeat(4000))"), "true");
    // ...and beyond it the match fails instead of taking the process down.
    assert_eq!(
        ok("typeof /^(?:a|b)*$/.test('ab'.repeat(200000))"),
        "boolean"
    );
}

#[test]
fn for_let_gives_each_iteration_its_own_binding() {
    assert_eq!(
        ok(
            "var fs = []; for (let i = 0; i < 3; i++) fs.push(function () { return i; }); fs.map(function (f) { return f(); }).join()"
        ),
        "0,1,2"
    );
    assert_eq!(
        ok(
            "var fs = []; for (let i = 0, j = 10; i < 2; i++, j++) fs.push(function () { return i + j; }); fs.map(function (f) { return f(); }).join()"
        ),
        "10,12"
    );
    assert_eq!(
        ok(
            "var fs = []; for (var i = 0; i < 3; i++) fs.push(function () { return i; }); fs.map(function (f) { return f(); }).join()"
        ),
        "3,3,3"
    );
}

#[test]
fn labeled_continue_and_break_target_the_labeled_loop() {
    assert_eq!(
        ok(
            "var n = 0; outer: for (var i = 0; i < 3; i++) { for (var j = 0; j < 3; j++) { if (j == 1) continue outer; n++; } } n + ',' + i"
        ),
        "3,3"
    );
    assert_eq!(
        ok(
            "var r = []; a: for (var i = 0; i < 3; i++) { b: for (var j = 0; j < 3; j++) { if (j == 1) continue a; if (i == 2) break a; r.push(i + '' + j); } } r.join()"
        ),
        "00,10"
    );
    assert_eq!(
        ok(
            "var s = 0, i = 0; w: while (i < 5) { i++; for (;;) { if (i % 2) continue w; s += i; break; } } s"
        ),
        "6"
    );
    assert_eq!(
        ok(
            "var n = 0; k: do { n++; for (var m of [1, 2]) { if (n < 3) continue k; } } while (n < 5); n"
        ),
        "5"
    );
    assert_eq!(ok("var n = 0; l: { n++; break l; n++; } n"), "1");
    assert_eq!(
        ok(
            "var o = []; q: for (var k in {a: 1, b: 2, c: 3}) { for (;;) { if (k == 'b') continue q; o.push(k); break; } } o.join()"
        ),
        "a,c"
    );
}

#[test]
fn static_blocks_can_name_their_class() {
    assert_eq!(
        ok("class B { static #x = 1; static { B.y = B.#x + 1; } } B.y"),
        "2"
    );
    assert_eq!(
        ok(
            "class C { static a = 1; static { var local = C.a + 1; C.b = local; } } C.b + ',' + typeof local"
        ),
        "2,undefined"
    );
}

#[test]
fn subclasses_of_built_ins_are_instances_of_themselves() {
    assert_eq!(
        ok(
            "class E extends Error { constructor(m) { super(m); this.name = 'E'; } } var e = new E('hi'); \
            [e instanceof E, e instanceof Error, e.message, e.name, String(e), Object.prototype.toString.call(e)].join()"
        ),
        "true,true,hi,E,E: hi,[object Error]"
    );
    assert_eq!(
        ok(
            "class M extends Map { get2(k) { return this.get(k) * 2; } } var m = new M([[1, 5]]); \
            [m instanceof M, m instanceof Map, m.get2(1), m.size].join()"
        ),
        "true,true,10,1"
    );
    assert_eq!(
        ok(
            "class A extends Array { sum() { return this.reduce(function (a, b) { return a + b; }, 0); } } \
            var a = new A(); a.push(1, 2, 3); [a instanceof A, Array.isArray(a), a.length, a.sum()].join()"
        ),
        "true,true,3,6"
    );
    assert_eq!(
        ok(
            "class P extends Promise {} var p = new P(function (r) { r(1); }); [p instanceof P, p instanceof Promise].join()"
        ),
        "true,true"
    );
    assert_eq!(
        ok(
            "class CE extends Event { constructor(t, d) { super(t); this.detail = d; } } var e = new CE('x', 7); \
            [e instanceof CE, e instanceof Event, e.type, e.detail].join()"
        ),
        "true,true,x,7"
    );
    assert_eq!(
        ok(
            "class T extends TypeError {} class U extends T {} var u = new U('z'); \
            [u instanceof U, u instanceof T, u instanceof TypeError, u instanceof Error, u.message].join()"
        ),
        "true,true,true,true,z"
    );
}

#[test]
fn a_catch_parameter_may_be_a_destructuring_pattern() {
    assert_eq!(
        ok(
            "var r; try { throw {message: 'm', code: 7}; } catch ({ message, code: c }) { r = message + c; } r"
        ),
        "m7"
    );
    assert_eq!(
        ok("var r; try { throw [1, [2]]; } catch ([a, [b = 9]]) { r = a + b; } r"),
        "3"
    );
    // A default can read an earlier name of the same pattern.
    assert_eq!(
        ok("var r; try { throw {}; } catch ({ x = 4, y = x + 1 }) { r = y; } r"),
        "5"
    );
}

#[test]
fn destructuring_a_null_catch_parameter_throws_a_type_error() {
    assert_eq!(
        ok(
            "var r; try { try { throw null; } catch ({ a }) { r = 'no'; } } catch (e) { r = e.name; } r"
        ),
        "TypeError"
    );
}

#[test]
fn a_generator_catch_parameter_may_be_a_destructuring_pattern() {
    assert_eq!(
        ok(r"
            function* g() {
                try { yield 1; throw { v: 'thrown' }; } catch ({ v }) { yield v; }
            }
            var it = g(); it.next(); it.next().value
        "),
        "thrown"
    );
}

#[test]
fn for_of_assigns_identifier_member_and_destructuring_targets() {
    assert_eq!(
        ok("var x, out = []; for (x of [1, 2]) out.push(x); out.join()"),
        "1,2"
    );
    assert_eq!(
        ok(
            "var a, b, out = []; for ([a, b = 5] of [[1], [2, 3]]) out.push(a + ':' + b); out.join()"
        ),
        "1:5,2:3"
    );
    assert_eq!(
        ok("var o = {}, out = []; for (o.p of ['x']) out.push(o.p); out.join()"),
        "x"
    );
    assert_eq!(
        ok(
            "var x, out = []; for (x of [1, 2, 3]) { if (x == 2) continue; out.push(x); } out.join()"
        ),
        "1,3"
    );
}

#[test]
fn array_from_rejects_null_and_undefined_and_a_non_callable_mapper() {
    assert_eq!(
        ok("var r; try { Array.from(null); } catch (e) { r = e.name; } r"),
        "TypeError"
    );
    assert_eq!(
        ok("var r; try { Array.from(undefined); } catch (e) { r = e.name; } r"),
        "TypeError"
    );
    assert_eq!(
        ok("var r; try { Array.from([1], 5); } catch (e) { r = e.name; } r"),
        "TypeError"
    );
    assert_eq!(ok("Array.from(new Set([1, 2])).join()"), "1,2");
}
