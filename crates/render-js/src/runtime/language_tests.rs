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

#[test]
fn source_phase_import_calls_parse_and_only_other_import_forms_are_rejected() {
    assert_eq!(
        ok("var f = () => import.defer('./x.js'); typeof f"),
        "function"
    );
    assert_eq!(
        ok("var f = () => import.source('./x.js'); typeof f"),
        "function"
    );
    // The arguments are evaluated before the call, as for any call.
    assert_eq!(ok("var n = 0; import.defer(n++); n"), "1");
    // A bracketed argument allows `in` even inside a for-initializer.
    assert_eq!(
        ok("var p; for (p = import.defer('a' in {a: 1}); false;) {} typeof p"),
        "object"
    );
    assert_eq!(
        ok("var p; for (var q = Math.max('a' in {a: 1}); false;) {} typeof q"),
        "number"
    );
    for source in [
        "import.defer('./x.js', 'extra')",
        "import.defer('./x.js',)",
        "import.source('./x.js',)",
        "import.defer",
        "new import.defer('./x.js')",
        "new import.source('./x.js').x",
        "new import('./x.js')",
        "import.foo",
        "import.meta",
        "\\u0069mport.defer('./x.js')",
        "import.\\u0064efer('./x.js')",
    ] {
        assert_eq!(
            eval(source).unwrap_err().kind(),
            JsErrorKind::Syntax,
            "{source}"
        );
    }
    assert_eq!(
        ok("var r; try { new (import('')); } catch (e) { r = e.name; } r"),
        "TypeError"
    );
}

#[test]
fn string_positions_convert_with_to_integer_or_infinity() {
    // NaN is 0 (ToIntegerOrInfinity), so these read the first unit.
    assert_eq!(ok("'abc'.codePointAt(NaN)"), "97");
    assert_eq!(ok("'abc'.charAt(NaN)"), "a");
    assert_eq!(ok("'abc'.charCodeAt(NaN)"), "97");
    // An empty string has no unit at any index, and this must not panic.
    assert_eq!(ok("String(''.at(NaN))"), "undefined");
    assert_eq!(ok("'abc'.at(-0.5)"), "a");
    assert_eq!(ok("'ab'.repeat(2.9)"), "abab");
    assert_eq!(ok("'abc'.substr(-1)"), "c");
    assert_eq!(ok("'abc'.slice(NaN, 2)"), "ab");
}

#[test]
fn string_methods_call_valueof_on_object_arguments_and_keep_its_errors() {
    assert_eq!(
        ok("var n = 0; var p = {valueOf() { n++; return 1; }}; 'abc'.slice(p, p); n"),
        "2"
    );
    assert_eq!(
        ok(
            "var r; try { 'abc'.charAt({valueOf() { throw new TypeError('boom'); }}); } catch (e) { r = e.message; } r"
        ),
        "boom"
    );
    assert_eq!(ok("'abc'.charAt({valueOf() { return 2; }})"), "c");
    assert_eq!(ok("'ab'.repeat({valueOf() { return 3; }})"), "ababab");
    assert_eq!(ok("'x'.padStart({valueOf() { return 3; }}, 'ab')"), "abx");
    assert_eq!(ok("String.fromCharCode({valueOf() { return 65; }})"), "A");
    assert_eq!(
        ok("String.raw({raw: {length: '2', 0: 'a', 1: 'b'}}, 'X')"),
        "aXb"
    );
}

#[test]
fn starts_with_and_ends_with_honor_the_position() {
    assert_eq!(ok("'abc'.startsWith('b', 1)"), "true");
    assert_eq!(ok("'abc'.startsWith('a', 1)"), "false");
    assert_eq!(ok("'abc'.endsWith('ab', -1)"), "false");
    assert_eq!(ok("'abc'.endsWith('ab', 2)"), "true");
    assert_eq!(ok("'abc'.endsWith('c')"), "true");
    assert_eq!(ok("'abc'.startsWith('')"), "true");
    assert_eq!(ok("'abc'.startsWith(undefined)"), "false");
    assert_eq!(ok("'undefined'.startsWith()"), "true");
}

#[test]
fn from_code_point_rejects_invalid_points_with_a_range_error() {
    assert_eq!(
        ok("var r; try { String.fromCodePoint(-1); } catch (e) { r = e.name; } r"),
        "RangeError"
    );
    assert_eq!(
        ok("var r; try { String.fromCodePoint(1.5); } catch (e) { r = e.name; } r"),
        "RangeError"
    );
}

#[test]
fn split_limit_is_touint32_so_a_negative_limit_is_the_full_split() {
    assert_eq!(ok("'a,b,c'.split(',', -1).length"), "3");
    assert_eq!(ok("'a,b,c'.split(',', 2).join('|')"), "a|b");
    assert_eq!(ok("'a,b,c'.split(',', 0).length"), "0");
}

#[test]
fn to_precision_takes_to_integer_or_infinity_and_a_range_error() {
    assert_eq!(ok("(123.456).toPrecision(1.9)"), "1e+2");
    assert_eq!(
        ok("var r; try { (1).toPrecision(0); } catch (e) { r = e.name; } r"),
        "RangeError"
    );
}

#[test]
fn an_ordinary_object_with_no_primitive_value_throws_a_type_error() {
    assert_eq!(
        ok("var r; try { '' + Object.create(null); } catch (e) { r = e.name; } r"),
        "TypeError"
    );
    assert_eq!(
        ok("var r; try { [1].flat(Object.create(null)); } catch (e) { r = e.name; } r"),
        "TypeError"
    );
}

#[test]
fn a_line_terminator_before_a_postfix_operator_ends_the_statement() {
    // [no LineTerminator here] before `++`, and U+2028 is a line terminator.
    for source in [
        "var x = 0; x\n++;",
        "var x = 0; x\u{2028}++;",
        "var x = 0; x\u{2029}--;",
    ] {
        assert_eq!(
            eval(source).unwrap_err().kind(),
            JsErrorKind::Syntax,
            "{source}"
        );
    }
    assert_eq!(ok("var x = 0; x\n++x; x"), "1");
    assert_eq!(ok("var x = 0; x++\n x"), "1");
}

#[test]
fn a_parenthesized_identifier_target_does_not_name_the_function() {
    assert_eq!(ok("var fn; (fn) = function() {}; fn.name"), "");
    assert_eq!(ok("var g; g = function() {}; g.name"), "g");
    assert_eq!(ok("var h; ((h)) = () => 1; h.name"), "");
}

/// Early errors that must be reported when the script is parsed, before any of
/// it runs. Each group is paired with valid siblings in the tests below.
fn assert_early_syntax_errors(sources: &[&str]) {
    for source in sources {
        match eval(source) {
            Err(error) => assert_eq!(error.kind(), JsErrorKind::Syntax, "{source}"),
            Ok(value) => panic!("{source} should be a SyntaxError, got {value:?}"),
        }
    }
}

#[test]
fn use_strict_directive_is_refused_with_non_simple_parameters() {
    assert_early_syntax_errors(&[
        "function f(a = 1) { 'use strict'; }",
        "function f([a]) { 'use strict'; }",
        "function f({a}) { 'use strict'; }",
        "function f(...a) { 'use strict'; }",
        "(a = 1) => { 'use strict'; }",
        "({ m(a = 1) { 'use strict'; } })",
        "class C { m(a = 1) { 'use strict'; } }",
    ]);
    assert_eq!(ok("function f(a) { 'use strict'; return a; } f(4)"), "4");
    assert_eq!(ok("function f(a = 2) { return a; } f()"), "2");
    assert_eq!(ok("var g = (a = 3) => { return a; }; g()"), "3");
}

#[test]
fn class_field_initializers_reject_arguments_and_super_calls() {
    assert_early_syntax_errors(&[
        "class C { x = arguments; }",
        "class C { x = () => arguments; }",
        "class C { x = typeof arguments; }",
        "class C { static x = arguments; }",
        "class B {} class C extends B { x = super(); }",
        "class B {} class C extends B { x = () => super(); }",
    ]);
    assert_eq!(
        ok("class C { x = function() { return arguments.length; }; } new C().x()"),
        "0"
    );
    assert_eq!(
        ok("class B { m() { return 5; } } class C extends B { x = super.m(); } new C().x"),
        "5"
    );
}

#[test]
fn static_field_named_constructor_is_refused() {
    assert_early_syntax_errors(&[
        "class C { static constructor = 1; }",
        "class C { static 'constructor'; }",
    ]);
    assert_eq!(
        ok("class C { static constructor() { return 7; } } C.constructor()"),
        "7"
    );
}

#[test]
fn private_names_must_be_declared_by_an_enclosing_class_body() {
    assert_early_syntax_errors(&[
        "this.#x;",
        "class C { constructor() { this.#x; } }",
        "class C { m() { return `${this.#y}`; } }",
        "class C extends (class { x = this.#foo; }) { #foo; }",
    ]);
    // A declaration may follow its use, and a nested class may use a name
    // declared by an enclosing class body.
    assert_eq!(
        ok("class C { m() { return this.#x; } #x = 9; } new C().m()"),
        "9"
    );
    assert_eq!(
        ok(
            "class C { #x = 3; m() { return class { g(o) { return o.#x; } }; } } new (new C().m())().g(new C())"
        ),
        "3"
    );
    assert_eq!(
        ok("class C { #x = 4; static has(o) { return #x in o; } } String(C.has(new C()))"),
        "true"
    );
}

#[test]
fn declarations_are_refused_where_only_a_statement_can_stand() {
    assert_early_syntax_errors(&[
        "while (false) const x = 1;",
        "for (;false;) let x = 1;",
        "if (true) class C {}",
        "do let x = 1; while (false);",
        "if (true) function* g() {}",
        "if (true) async function h() {}",
        "while (false) function f() {}",
        "while (false) l: function f() {}",
        "if (true) l: function f() {}",
        "while (false) let [a] = [1];",
    ]);
    // Sloppy code may put a plain function declaration in an `if` clause, and
    // `let` followed by a line break is an expression statement (ASI).
    assert_eq!(ok("var r = 'ok'; if (false) function f() {} r"), "ok");
    assert_eq!(ok("var r = 'none'; if (false) let \n r = 1; r"), "1");
}

#[test]
fn yield_is_an_operator_only_at_assignment_level_in_generators() {
    assert_early_syntax_errors(&[
        "function* g() { void yield; }",
        "function* g() { 1 + yield; }",
        "function* g() { ({ yield }); }",
        "function* g(a = yield) {}",
        "async function f(a = await 1) {}",
    ]);
    assert_eq!(
        ok(
            "function* g() { var x = yield 1; yield* [2]; return x; } var it = g(); it.next().value"
        ),
        "1"
    );
    assert_eq!(
        ok(
            "function* g() { return [yield, (yield 3)]; } var it = g(); it.next(); String(it.next(7).value)"
        ),
        "3"
    );
    // `yield +1` is a YieldExpression whose operand is the unary `+1`.
    assert_eq!(ok("function* g() { yield +1; } g().next().value"), "1");
    // Outside generators `yield` is an ordinary sloppy identifier.
    assert_eq!(ok("var yield = 5; yield + 1"), "6");
}

#[test]
fn escaped_reserved_words_are_never_identifiers_or_keywords() {
    assert_early_syntax_errors(&[
        "\\u0069f (true) {}",
        "var \\u0069f = 1;",
        "var x = { i\\u0066 } = { if: 42 };",
        "var o = { \\u0074his }; ",
        "var \\u0074his = 1;",
    ]);
    // Property names may spell a reserved word, escaped or not.
    assert_eq!(ok("var o = { \\u0069f: 4 }; o.if + o.\\u0069f"), "8");
    assert_eq!(ok("var l\\u0065t = 2; l\\u0065t"), "2");
}

#[test]
fn shorthand_properties_must_name_an_identifier_reference() {
    assert_early_syntax_errors(&[
        "({ if })",
        "({ this })",
        "({ 'a' })",
        "var { if } = {};",
        "async function f() { ({ await }); }",
    ]);
    assert_eq!(ok("var u = 1; ({ u, undefined: 2 }).u"), "1");
    assert_eq!(ok("var n = 3; var { n: m } = { n }; m"), "3");
}

#[test]
fn super_is_valid_only_where_its_method_context_allows_it() {
    assert_early_syntax_errors(&[
        "function f() { super.x; }",
        "function f() { super(); }",
        "({ m() { function f() { super.x; } } })",
        "class C { constructor() { super(); } }",
        "class B {} class C extends B { m() { super(); } }",
        "class B {} class C extends B { constructor(a = super()) {} }",
        "class C { static { super(); } }",
        "class C { x = super(); }",
        "class C { m() { return () => super(); } }",
        "class B {} class C extends B { get g() { super(); } }",
    ]);
    assert_eq!(
        ok(
            "class B { m() { return 1; } } class C extends B { constructor() { super(); this.v = super.m(); } } new C().v"
        ),
        "1"
    );
    assert_eq!(
        ok(
            "class B {} class C extends B { constructor() { var f = () => super(); f(); this.ok = true; } } String(new C().ok)"
        ),
        "true"
    );
    // Object literal methods may name `super` (the parse is what is checked here;
    // running `super` in an object literal method is not implemented).
    assert_eq!(
        ok("var o = { m() { return super.x; } }; 'parsed'"),
        "parsed"
    );
    assert_eq!(
        ok(
            "class B { static m() { return 2; } } class C extends B { static n() { return super.m(); } } C.n()"
        ),
        "2"
    );
    assert_eq!(
        ok("class B {} class C extends B { x = super.constructor === B; } String(new C().x)"),
        "true"
    );
}

#[test]
fn class_names_are_strict_code_even_in_sloppy_scripts() {
    assert_early_syntax_errors(&[
        "class let {}",
        "class static {}",
        "class arguments {}",
        "var C = class yield {};",
        "class l\\u0065t {}",
    ]);
    assert_eq!(ok("class C {} String(typeof C)"), "function");
}

#[test]
fn destructuring_assignment_targets_follow_the_pattern_grammar() {
    assert_early_syntax_errors(&[
        "[...x, y] = [];",
        "var x; [...x = 1] = [];",
        "var x; [...[x], y] = [];",
        "var x, y; ({...x, y} = {});",
        "var x; ({...{x}} = {});",
        "[1] = [];",
        "var x; ({a: 1} = {});",
        "var a; [a] += 1;",
        "for ([...x, y] of []) {}",
    ]);
    assert_eq!(ok("var a, b; [a, ...b] = [1, 2, 3]; b.length"), "2");
    assert_eq!(ok("var x; ({a: [x = 4] = []} = {}); x"), "4");
    assert_eq!(ok("var b; [, [b] = [5]] = [0, undefined]; b"), "5");
    assert_eq!(ok("var o = {}; [o.x, o['y']] = [1, 2]; o.x + o.y"), "3");
}

#[test]
fn break_and_continue_targets_must_enclose_them_in_the_same_function() {
    assert_early_syntax_errors(&[
        "foo: { break bar; }",
        "l: { continue l; }",
        "while (false) { break nope; }",
        "a: a: ;",
        "l: while (false) { (function() { continue l; }); }",
        "l: while (false) { (() => { break l; }); }",
        "switch (1) { case 1: (function() { break; }); }",
        "for (;;) { class C { static { break; } } }",
    ]);
    assert_eq!(
        ok(
            "var r = 0; outer: for (var i = 0; i < 3; i++) { inner: for (;;) { r++; break outer; } } r"
        ),
        "1"
    );
    assert_eq!(ok("var r = 0; blk: { r = 1; break blk; r = 2; } r"), "1");
    assert_eq!(
        ok("var r = 0; a: b: while (r < 2) { r++; continue a; } r"),
        "2"
    );
}

#[test]
fn class_static_blocks_are_function_like_for_control_flow_and_names() {
    assert_early_syntax_errors(&[
        "class C { static { return; } }",
        "class C { static { arguments; } }",
        "class C { static { () => arguments; } }",
        "class C { static { var await; } }",
        "class C { static { (x = await) => 0; } }",
        "class C { static { ({ await }); } }",
        "class C { static { l: { break l2; } } }",
    ]);
    assert_eq!(ok("class C { static { var x = 5; this.y = x; } } C.y"), "5");
}

#[test]
fn strict_class_code_refuses_reserved_identifier_references() {
    assert_early_syntax_errors(&[
        "class C { m() { return implements; } }",
        "class C { m() { return yield; } }",
        "class C { static { var x = package; } }",
    ]);
    // `eval` and `arguments` are ordinary references in strict code.
    assert_eq!(
        ok("class C { m() { var q = typeof eval; return q; } } 'ok'"),
        "ok"
    );
}

#[test]
fn a_statement_ends_at_a_semicolon_a_brace_the_end_or_a_line_break() {
    assert_early_syntax_errors(&[
        "var x = 1 y;",
        "var x = 1 var y = 2;",
        "x = 1 y = 2",
        "{ 1 2 }",
        "throw 1 2;",
    ]);
    assert_eq!(ok("var x = 1\nvar y = 2; x + y"), "3");
    assert_eq!(ok("var a = 1; var b = a\n++a; b"), "1");
    assert_eq!(ok("var r = 0; do r++; while (r < 3) r"), "3");
    assert_eq!(ok("var t = 0; { t = 5 } t"), "5");
}

#[test]
fn arrow_parameter_names_are_never_duplicated() {
    assert_early_syntax_errors(&[
        "var f = (a, a) => 1;",
        "var f = ([a, a]) => 1;",
        "var f = ({a}, a) => 1;",
    ]);
    assert_eq!(ok("var f = (a, b) => a + b; f(2, 3)"), "5");
}
#[test]
fn object_accessors_take_computed_names_and_install_accessor_slots() {
    assert_eq!(
        ok("var s = Symbol('k'); var o = { get [s]() { return 1; } }; typeof o[s]"),
        "number"
    );
    assert_eq!(
        ok(
            "var s = Symbol('k'); var o = { get [s]() { return 1; }, set [s](v) { this.hit = v; } }; o[s] = 5; String(o.hit)"
        ),
        "5"
    );
    assert_eq!(
        ok(
            "var o = { get ['a' + 'b']() { return 'ab'; }, set ['a' + 'b'](v) { this.v = v; } }; o.ab + (o.ab = 2)"
        ),
        "ab2"
    );
    assert_eq!(
        ok("var o = { get [1E+9]() { return 'g'; } }; o['1000000000']"),
        "g"
    );
}

#[test]
fn object_get_and_set_are_ordinary_names_before_a_separator() {
    assert_eq!(
        ok("var get = 1, set = 2; var o = { get, set }; o.get + o.set"),
        "3"
    );
    assert_eq!(
        ok("var o = { get() { return 4; }, set: 5 }; o.get() + o.set"),
        "9"
    );
}

#[test]
fn numeric_property_names_are_their_to_string_value() {
    assert_eq!(
        ok(
            "var o = { 1e21: 'a', 0.0000001: 'b', 0x10: 'c', .5: 'd' }; o['1e+21'] + o['1e-7'] + o['16'] + o['0.5']"
        ),
        "abcd"
    );
}

#[test]
fn class_modifiers_stop_at_a_line_break_where_the_grammar_requires_it() {
    assert_eq!(
        ok(
            "var i = new (class { async\n m() { return 1; } })(); String('async' in i) + ':' + typeof i.m"
        ),
        "true:function"
    );
    assert_eq!(
        ok("var C = class { get\n *g() { return 7; } }; new C().g().next().value"),
        "7"
    );
    assert_eq!(
        ok("var i = new (class { get\n x() { return 9; } })(); i.x"),
        "9"
    );
}

#[test]
fn a_star_after_get_or_set_and_malformed_shorthand_members_are_syntax_errors() {
    for source in [
        "class C { get *x() {} }",
        "class C { set *x(v) {} }",
        "({ get *x() {} })",
        "({ set *x(v) {} })",
        "({ async\nfoo() {} })",
        "({ null })",
        "({ 'a' })",
        "({ this })",
        "({ 1 })",
    ] {
        assert_eq!(
            eval(source).unwrap_err().kind(),
            JsErrorKind::Syntax,
            "{source}"
        );
    }
}

#[test]
fn computed_object_accessor_keys_parse_inside_a_for_initializer() {
    assert_eq!(
        ok(
            "var o, v; for (o = { get ['x' in {x: 1}]() { return 'hit'; } }; ; ) { v = o.true; break; } v"
        ),
        "hit"
    );
}
#[test]
fn with_reads_and_writes_consult_the_object_before_outer_scopes() {
    assert_eq!(ok("var r; with ({a: 1}) { r = a; } r"), "1");
    assert_eq!(ok("var o = {a: 1}; with (o) { a = 2; } o.a"), "2");
    assert_eq!(
        ok("var a = 0; var o = {}; with (o) { a = 5; } a + ':' + ('a' in o)"),
        "5:false"
    );
    assert_eq!(ok("var o = {n: 2}; with (o) { n++; n += 3; } o.n"), "6");
}

#[test]
fn with_resolves_inherited_properties_and_calls_with_the_object_as_this() {
    assert_eq!(ok("with ([1, 2, 3]) { join('-') }"), "1-2-3");
    assert_eq!(
        ok("var o = {f: function() { return this === o; }}; var r; with (o) { r = f(); } r"),
        "true"
    );
}

#[test]
fn with_honours_unscopables_before_the_object_property() {
    assert_eq!(
        ok(
            "var a = 'outer'; var o = {a: 'inner', [Symbol.unscopables]: {a: true}}; var r; with (o) { r = a; } r"
        ),
        "outer"
    );
}

#[test]
fn var_initializers_inside_with_write_the_object() {
    assert_eq!(
        ok("var o = {x: 1}; with (o) { var x = 2; } o.x + ':' + x"),
        "2:undefined"
    );
    assert_eq!(ok("var o = {}; with (o) { var y; } 'y' in o"), "false");
}

#[test]
fn functions_created_in_with_keep_the_scope_they_were_created_in() {
    assert_eq!(
        ok(
            "var o = {p: 'before'}; var f; with (o) { f = function() { return p; }; } o.p = 'after'; f()"
        ),
        "after"
    );
}

#[test]
fn typeof_and_delete_follow_the_with_object() {
    assert_eq!(
        ok("var o = {a: 1}; var r; with (o) { r = typeof a; } r"),
        "number"
    );
    assert_eq!(
        ok("var r; with ({}) { r = typeof nothingHere; } r"),
        "undefined"
    );
    assert_eq!(
        ok("var o = {a: 1}; with (o) { delete a; } 'a' in o"),
        "false"
    );
}

#[test]
fn with_assignment_target_is_resolved_before_the_value_is_evaluated() {
    // The target resolves to the object while it still has `x`; the value
    // then deletes that property, so the write creates it on the object.
    assert_eq!(
        ok("var o = {x: 1}; var x = 'g'; with (o) { x = (delete o.x, 2); } o.x + ':' + x"),
        "2:g"
    );
}

#[test]
fn with_restores_the_scope_when_its_body_throws() {
    assert_eq!(
        ok(
            "var a = 1; var o = {a: 2}; try { with (o) { a = 3; throw 1; } } catch (e) {} a + ':' + o.a"
        ),
        "1:3"
    );
}

#[test]
fn with_object_must_not_be_null_or_undefined() {
    let error = eval("with (null) {}").expect_err("null has no object form");
    assert_eq!(error.kind(), JsErrorKind::Type);
    let error = eval("with (undefined) x = 2").expect_err("undefined has no object form");
    assert_eq!(error.kind(), JsErrorKind::Type);
}

#[test]
fn with_is_an_early_error_in_strict_code() {
    for source in [
        "'use strict'; with ({}) {}",
        "'use strict'; function f() { with ({}) {} }",
        "function f() { 'use strict'; with ({}) {} }",
        "'use strict'; function f() { return function() { with ({}) {} }; }",
        "class C { m() { with ({}) {} } }",
    ] {
        let error = eval(source).expect_err("with is forbidden in strict code");
        assert_eq!(error.kind(), JsErrorKind::Syntax, "{source}");
    }
    assert_eq!(
        ok("function f() { var o = {a: 3}; with (o) return a; } f()"),
        "3"
    );
}

#[test]
fn with_completion_value_follows_its_body_and_a_let_body_ends_at_the_line_break() {
    assert_eq!(ok("1; with({}) { }"), "undefined");
    assert_eq!(ok("2; with({}) { 3; }"), "3");
    // `let` is an identifier here: the line break ends the statement, so the
    // assignment on the next line is a separate statement.
    assert_eq!(ok("if (false) { with ({}) let \n x = 1; } 'ok'"), "ok");
    assert_eq!(ok("if (false) { with ({}) let \n {} } 'ok'"), "ok");
    for source in [
        "if (false) { with ({}) let \n [a] = 0; }",
        "if (false) { with ({}) let x; }",
    ] {
        let error = eval(source).expect_err("let begins a declaration here");
        assert_eq!(error.kind(), JsErrorKind::Syntax, "{source}");
    }
}

#[test]
fn with_is_a_reserved_word_and_its_body_is_a_statement() {
    for source in [
        "var with = 1;",
        // An escaped spelling is not the keyword, and `with` is reserved.
        "w\\u0069th ({}) {}",
        "with ({}) let x;",
        "with ({}) const x = 1;",
        "with ({}) function f() {}",
        "with ({}) class C {}",
        "with ({}) label: function f() {}",
    ] {
        let error = eval(source).expect_err("not valid with syntax");
        assert_eq!(error.kind(), JsErrorKind::Syntax, "{source}");
    }
    assert_eq!(ok("var o = {with: 1}; o.with"), "1");
}

#[test]
fn a_block_comment_with_a_line_separator_allows_automatic_semicolon_insertion() {
    assert_eq!(ok("var a = 1 /*\u{2028}*/ var b = 2; a + b"), "3");
    assert_eq!(ok("var a = 1 /*\u{2029}*/ var b = 2; a + b"), "3");
    assert_eq!(
        eval("var a = 1 /* no break */ var b = 2;")
            .unwrap_err()
            .kind(),
        JsErrorKind::Syntax
    );
}
