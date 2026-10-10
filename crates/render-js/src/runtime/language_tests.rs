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
fn typed_array_prototype_is_shared_through_the_intrinsic() {
    // §23.2.1: %TypedArray% is the abstract constructor the concrete ones inherit
    // from, and its prototype holds the methods every concrete prototype shares.
    assert_eq!(
        ok(
            "var TA = Object.getPrototypeOf(Int8Array); [TA.name, TA.length, typeof TA.prototype.map, Object.getPrototypeOf(Int8Array.prototype) === TA.prototype, Object.getPrototypeOf(Uint8Array) === TA, new Int8Array(2).hasOwnProperty('length')].join()"
        ),
        "TypedArray,0,function,true,true,false"
    );
    assert_eq!(
        ok("var r; try { Object.getPrototypeOf(Int8Array)(); } catch (e) { r = e.name; } r"),
        "TypeError"
    );
    assert_eq!(
        ok("Object.prototype.toString.call(new Float64Array(1))"),
        "[object Float64Array]"
    );
}

#[test]
fn typed_array_methods_read_the_view_and_follow_the_spec() {
    assert_eq!(
        ok(
            "var a = new Int8Array([3, 1, 2]); [a.at(-1), a.at(5) === undefined, a.every(function (x) { return x > 0; }), a.some(function (x) { return x > 2; }), a.find(function (x) { return x < 3; }), a.findIndex(function (x) { return x === 2; }), a.findLast(function (x) { return x < 3; }), a.reduce(function (s, x) { return s + x; }), a.reduceRight(function (s, x) { return s + '' + x; }, '')].join()"
        ),
        "2,true,true,true,1,2,2,6,213"
    );
    // Numeric sort puts `NaN` last and `-0` before `+0`.
    assert_eq!(
        ok(
            "var a = new Float64Array([3, NaN, -0, 0, -1]); a.sort(); var out = []; for (var i = 0; i < a.length; i++) out.push(Object.is(a[i], -0) ? '-0' : String(a[i])); out.join('|')"
        ),
        "-1|-0|0|3|NaN"
    );
    assert_eq!(
        ok(
            "var a = new Int8Array([1, 2, 3, 4, 5]); a.copyWithin(0, 3); var out = []; for (var i = 0; i < a.length; i++) out.push(a[i]); out.join()"
        ),
        "4,5,3,4,5"
    );
    assert_eq!(
        ok(
            "var a = new Int8Array([1, 2, 3]); var w = a.with(-1, 9); [a.join(), w.join(), a.toReversed().join(), a.toSorted(function (x, y) { return y - x; }).join(), a.reverse() === a, a.join()].join(' ')"
        ),
        "1,2,3 1,2,9 3,2,1 3,2,1 true 3,2,1"
    );
    assert_eq!(
        ok(
            "var a = new Int16Array([7, 8]); var r = []; for (var p of a.entries()) r.push(p.join(':')); for (var k of a.keys()) r.push(k); r.join()"
        ),
        "0:7,1:8,0,1"
    );
    assert_eq!(
        ok(
            "var r = []; var a = Int8Array.of(1, 2); r.push(a.length, a instanceof Int8Array, Int8Array.from([4, 5], function (x) { return x * 2; }).join()); r.join('|')"
        ),
        "2|true|8,10"
    );
    assert_eq!(
        ok(
            "var a = new Float64Array(3); [a.length, a.byteLength, a.byteOffset, a.hasOwnProperty('byteLength')].join()"
        ),
        "3,24,0,false"
    );
}

#[test]
fn integer_indexed_reads_never_consult_the_prototype_for_numeric_keys() {
    // §10.4.5.4 [[Get]]: a canonical numeric key is an element read, so a
    // prototype property with that name is not reached.
    assert_eq!(
        ok(
            "var a = new Int8Array(2); Object.defineProperty(Object.getPrototypeOf(Int8Array.prototype), '1.5', { get: function () { throw new Error('reached'); } }); [a['1.5'], a[-0], a[2], a['01']].join()"
        ),
        ",0,,"
    );
}

#[test]
fn array_and_object_members_report_the_spec_lengths() {
    assert_eq!(
        ok(
            "[Array.prototype.concat.length, Array.prototype.unshift.length, Object.setPrototypeOf.length, Object.hasOwn.length, Object.prototype.__defineGetter__.length, DataView.prototype.setFloat16.length].join()"
        ),
        "1,1,2,2,2,2"
    );
    // `indexOf` reports `+0` for a `-0` start.
    assert_eq!(ok("Object.is([1].indexOf(1, -0), 0)"), "true");
}

#[test]
fn define_property_rejects_a_non_object_target_or_descriptor() {
    // §20.1.2.4 step 1 and §6.2.6.5 step 1: a primitive target, and a descriptor
    // that is not an object (undefined included), are TypeErrors.
    assert_eq!(
        ok(
            "var r = []; try { Object.defineProperty(1, 'x', {}); } catch (e) { r.push(e.name); } try { Object.defineProperty(null, 'x', {}); } catch (e) { r.push(e.name); } try { Object.defineProperty({}, 'x', undefined); } catch (e) { r.push(e.name); } r.join()"
        ),
        "TypeError,TypeError,TypeError"
    );
    assert_eq!(
        ok(
            "var r = []; try { Object.create(1); } catch (e) { r.push(e.name); } try { Object.create({}, { prop: undefined }); } catch (e) { r.push(e.name); } r.join()"
        ),
        "TypeError,TypeError"
    );
}

#[test]
fn object_create_defines_the_properties_it_is_given() {
    assert_eq!(
        ok(
            "var o = Object.create({ inherited: 1 }, { x: { value: 2, enumerable: true }, y: { get: function () { return 3; } } }); [o.x, o.y, o.inherited, Object.keys(o).join()].join()"
        ),
        "2,3,1,x"
    );
}

#[test]
fn non_configurable_properties_accept_only_the_allowed_redefinitions() {
    // A writable, non-configurable data property takes a new value.
    assert_eq!(
        ok(
            "var o = {}; Object.defineProperty(o, 'x', { value: 1, writable: true }); Object.defineProperty(o, 'x', { value: 2 }); o.x"
        ),
        "2"
    );
    // A read-only one keeps its value, and a different value is a TypeError.
    assert_eq!(
        ok(
            "var o = {}; Object.defineProperty(o, 'x', { value: 1 }); var r; try { Object.defineProperty(o, 'x', { value: 2 }); } catch (e) { r = e.name; } [r, o.x].join()"
        ),
        "TypeError,1"
    );
    // It cannot become configurable, but re-stating its value is a no-op.
    assert_eq!(
        ok(
            "var o = {}; Object.defineProperty(o, 'x', { value: 1 }); var r = []; try { Object.defineProperty(o, 'x', { configurable: true }); } catch (e) { r.push(e.name); } Object.defineProperty(o, 'x', { value: 1 }); r.push(o.x); r.join()"
        ),
        "TypeError,1"
    );
    // A non-extensible object takes no new property.
    assert_eq!(
        ok(
            "var o = Object.preventExtensions({}); var r; try { Object.defineProperty(o, 'x', { value: 1 }); } catch (e) { r = e.name; } [r, 'x' in o].join()"
        ),
        "TypeError,false"
    );
}

#[test]
fn a_generic_descriptor_keeps_an_existing_accessor() {
    assert_eq!(
        ok(
            "var o = { get x() { return 7; } }; Object.defineProperty(o, 'x', { enumerable: false }); [o.x, Object.getOwnPropertyDescriptor(o, 'x').enumerable, typeof Object.getOwnPropertyDescriptor(o, 'x').get].join()"
        ),
        "7,false,function"
    );
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
    assert_eq!(ok("var hits = 0; var o = {}; o.f?.(hits++); hits"), "0");
    // Not an optional call: the argument runs first, then the missing callee throws.
    assert_eq!(
        ok(
            "var hits = 0; var o = {}; var r; try { o?.f(hits++); r = 'no'; } catch (e) { r = e.name; } r + hits"
        ),
        "TypeError1"
    );
    assert_eq!(ok("var f; String(f?.(1))"), "undefined");
}

#[test]
fn calling_a_nullish_or_primitive_callee_throws_after_the_arguments() {
    // ECMA-262 13.3.6.1: only an optional call short-circuits; every other
    // callee that is not callable is a TypeError, and the arguments run first.
    for source in [
        "var f; f(1)",
        "var n = null; n()",
        "var o = {}; o.missing()",
        "var five = 5; five()",
        "var s = 'x'; s.call(1)",
        "var t; t`x`",
    ] {
        assert_eq!(
            ok(&format!(
                "var r; try {{ {source}; r = 'no'; }} catch (e) {{ r = e.name; }} r"
            )),
            "TypeError",
            "{source}"
        );
    }
    assert_eq!(
        ok("var hits = 0; var f; var r; try { f(hits++); } catch (e) { r = e.name; } r + hits"),
        "TypeError1"
    );
    assert_eq!(
        ok("var five = 5; var r; try { five?.(); r = 'no'; } catch (e) { r = e.name; } r"),
        "TypeError"
    );
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
fn computed_class_element_keys_run_to_property_key_in_source_order() {
    // §15.7.14 and §13.2.5.5: each computed key is converted with ToPropertyKey
    // (string hint, so user toString runs) as soon as it is evaluated.
    assert_eq!(
        ok(
            "var key = { toString() { return 'named'; } }; class C { [key]() { return 1; } static [key] = 2; } [new C().named(), C.named].join()"
        ),
        "1,2"
    );
    assert_eq!(
        ok(
            "var log = []; class C { [{ toString() { log.push('b'); return 'x'; } }]() {} [log.push('c')]() {} } log.join()"
        ),
        "b,c"
    );
    assert_eq!(
        ok("class C { [() => 1]() { return 7; } } new C()[String(() => 1)]()"),
        "7"
    );
    assert_eq!(
        ok(
            "var r; try { class C { [{ toString: 1, valueOf: 2 }]() {} } } catch (e) { r = e.name; } r"
        ),
        "TypeError"
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
fn private_names_resolve_where_the_code_is_written() {
    // §9.2.1 and §15.7.14: a function reads `#name` through the class body it
    // was created in, however it is later called.
    assert_eq!(
        ok(
            "class C { #f = 'ok'; m() { let self = this; function inner() { return self.#f; } return inner(); } } new C().m()"
        ),
        "ok"
    );
    assert_eq!(
        ok("var g; class C { #x = 5; m() { g = () => this.#x; } } new C().m(); g()"),
        "5"
    );
    assert_eq!(
        ok(
            "class A { run(f) { return f(); } } class B { #x = 2; get() { return () => this.#x; } } new A().run(new B().get())"
        ),
        "2"
    );
}

#[test]
fn static_initializers_and_blocks_have_the_class_as_home_object() {
    assert_eq!(
        ok(
            "class A { static f() { return 'A'; } } class B extends A { static x = super.f(); } B.x"
        ),
        "A"
    );
    assert_eq!(
        ok(
            "class A { static f() { return 'a'; } } class B extends A { static { this.y = super.f(); } } B.y"
        ),
        "a"
    );
    assert_eq!(
        ok(
            "class A { static f() { return 'k'; } } class B extends A { static g = () => super.f(); } B.g()"
        ),
        "k"
    );
}

#[test]
fn class_expression_heritage_closures_see_the_inner_name() {
    assert_eq!(
        ok(
            "var probeBefore = function() { return C; }; var probeHeritage; var C = 'outside'; var cls = class C extends (probeHeritage = function() { return C; }, Object) { method() { return C; } }; [probeBefore(), probeHeritage() === cls, cls.prototype.method() === cls].join()"
        ),
        "outside,true,true"
    );
    assert_eq!(
        ok(
            "var r; function f() { try { var x = (class x extends x {}); } catch (e) { r = e.name; } return r; } f()"
        ),
        "ReferenceError"
    );
    // The heritage's `prototype` is read with [[Get]] once, and it must be an
    // object or null.
    assert_eq!(
        ok(
            "var calls = 0; var Base = function() {}.bind(); Object.defineProperty(Base, 'prototype', { get() { calls++; return null; }, configurable: true }); class C extends Base {} calls"
        ),
        "1"
    );
    assert_eq!(
        ok(
            "var r; var Base = function() {}.bind(); Object.defineProperty(Base, 'prototype', { get() { return 42; }, configurable: true }); try { class C extends Base {} } catch (e) { r = e.name; } r"
        ),
        "TypeError"
    );
}

#[test]
fn class_declarations_are_mutable_and_the_inner_name_is_separate() {
    // §15.7.14: the outer declaration is a `let`-style binding, while the inner
    // name is immutable and already live in the heritage (in its TDZ until the
    // constructor exists).
    assert_eq!(ok("class C {} C = 5; C"), "5");
    // NamedEvaluation names an anonymous class but gives it no inner binding,
    // so its methods read the outer variable.
    assert_eq!(
        ok("var C = class { m() { return C; } }; var D = C; C = null; D.prototype.m() === null"),
        "true"
    );
    assert_eq!(
        ok(
            "var probe; class C extends (probe = function() { return C; }, Object) {} var cls = C; C = null; probe() === cls"
        ),
        "true"
    );
    assert_eq!(
        ok("var r; try { var x = (class x extends x {}); } catch (e) { r = e.name; } r"),
        "ReferenceError"
    );
}

#[test]
fn class_elements_the_class_refuses_throw_and_private_targets_destructure() {
    // §15.7.14 and §7.3.8: a public element the class refuses (a static
    // `prototype`) is a TypeError, not a silent no-op.
    assert_eq!(
        ok("var r; try { class C { static ['prototype'] = 1; } } catch (e) { r = e.name; } r"),
        "TypeError"
    );
    assert_eq!(
        ok("var r; try { class C { static ['prototype']() {} } } catch (e) { r = e.name; } r"),
        "TypeError"
    );
    // §13.15.5: a private member is a valid destructuring target.
    assert_eq!(
        ok("class C { #f; m() { [this.#f] = [7]; return this.#f; } } new C().m()"),
        "7"
    );
    assert_eq!(
        ok("class C { #f; m() { ({ a: this.#f } = { a: 8 }); return this.#f; } } new C().m()"),
        "8"
    );
    // The target reference is evaluated before the property is read, so a
    // `this` that is still uninitialized throws before the getter runs.
    assert_eq!(
        ok(
            "var r; class A {} class C extends A { #field; constructor() { var init = () => super(); var object = { get a() { init(); } }; ({ a: this.#field } = object); } } try { new C(); } catch (e) { r = e.name; } r"
        ),
        "ReferenceError"
    );
}

#[test]
fn class_accessor_arity_and_private_accessor_pairs_are_early_errors() {
    // §15.4.1 and §15.7.1: a getter takes no parameters, a setter exactly one
    // non-rest parameter, and a private getter/setter pair must share staticness.
    assert_early_syntax_errors(&[
        "class C { get a(p) {} }",
        "class C { get a(p = null) {} }",
        "class C { set a() {} }",
        "class C { set a(...p) {} }",
        "class C { get #a() {} static set #a(v) {} }",
        "class C { static get #a() {} set #a(v) {} }",
        "class C { static { class await {} } }",
    ]);
    assert_eq!(
        ok("class C { get #a() { return 4; } set #a(v) {} m() { return this.#a; } } new C().m()"),
        "4"
    );
    assert_eq!(
        ok("class C { set a(v = 1) { this.b = v; } } var c = new C(); c.a = undefined; c.b"),
        "1"
    );
}

#[test]
fn parameter_expressions_run_in_their_own_scope_apart_from_the_body_vars() {
    // §10.2.11 step 28: with a default initializer, body `var`s live in a separate
    // environment, so the closure in a parameter does not see them.
    assert_eq!(
        ok(
            "var x = 'outside'; var probeP, probeB; function f(_ = probeP = function() { return x; }) { var x = 'inside'; probeB = function() { return x; }; } f(); probeP() + ',' + probeB()"
        ),
        "outside,inside"
    );
    assert_eq!(
        ok(
            "var x = 'outside'; var probeP, probeB; function* g(_ = probeP = function() { return x; }) { var x = 'inside'; probeB = function() { return x; }; yield x; } g().next().value + ',' + probeP() + ',' + probeB()"
        ),
        "inside,outside,inside"
    );
    // A body `var` that shares a parameter's name starts with the argument's value.
    assert_eq!(
        ok("function f(a = 1) { var a; return a; } [f(), f(5)].join()"),
        "1,5"
    );
}

#[test]
fn class_fields_are_defined_through_define_own_property() {
    // §7.3.7 CreateDataPropertyOrThrow: a field on a proxy reaches its
    // `defineProperty` trap, and a field a frozen object refuses is a TypeError.
    assert_eq!(
        ok(
            "var log = []; var P = new Proxy({}, { defineProperty(t, k, d) { log.push(k + ':' + d.value); return Reflect.defineProperty(t, k, d); } }); class A { constructor() { return P; } } class B extends A { f = 7; } new B(); log.join()"
        ),
        "f:7"
    );
    assert_eq!(
        ok(
            "var r; class A { constructor() { return Object.freeze({}); } } class B extends A { f = 1; } try { new B(); } catch (e) { r = e.name; } r"
        ),
        "TypeError"
    );
}

#[test]
fn class_constructor_own_keys_start_with_length_name_prototype() {
    // §10.2.5 and §15.7.14: a class constructor's own keys are `length`, `name`,
    // and `prototype`, and its static members follow them.
    assert_eq!(
        ok("class A { static method() {} } Object.getOwnPropertyNames(A).join()"),
        "length,name,prototype,method"
    );
    assert_eq!(
        ok("class B { static [String('length')]() {} } Object.getOwnPropertyNames(B).join()"),
        "length,name,prototype"
    );
}

#[test]
fn class_methods_have_no_prototype_and_accessors_take_prefixed_names() {
    // §15.4.4 and §15.4.5: a method, getter or setter is not a constructor, so
    // it has no `prototype`; an accessor is named with its `get` or `set` prefix.
    assert_eq!(
        ok(
            "class C { m() {} *g() {} get x() { return 1; } set x(v) {} } var d = Object.getOwnPropertyDescriptor(C.prototype, 'x'); [C.prototype.m.hasOwnProperty('prototype'), C.prototype.g.hasOwnProperty('prototype'), d.get.name, d.set.name].join()"
        ),
        "false,true,get x,set x"
    );
}

#[test]
fn private_elements_cannot_be_added_to_a_non_extensible_object() {
    // PrivateFieldAdd and PrivateMethodOrAccessorAdd: a base constructor that
    // sealed the instance leaves no room for the subclass's private elements.
    for source in [
        "class B { constructor(s) { if (s) Object.preventExtensions(this); } } class C extends B { #v = 1; } new C(true)",
        "class B { constructor(s) { if (s) Object.preventExtensions(this); } } class C extends B { #m() {} m() { return this.#m; } } new C(true)",
    ] {
        assert_eq!(
            ok(&format!(
                "var r; try {{ {source}; r = 'no'; }} catch (e) {{ r = e.name; }} r"
            )),
            "TypeError",
            "{source}"
        );
    }
    assert_eq!(
        ok(
            "class B { constructor(s) { if (s) Object.preventExtensions(this); } } class C extends B { #v = 1; v() { return this.#v; } } new C(false).v()"
        ),
        "1"
    );
}

#[test]
fn class_heritage_must_be_a_constructor() {
    // §15.7.14 step 6.a: IsConstructor(superclass) must hold. Arrows, methods,
    // generators, and async functions have no [[Construct]].
    for source in [
        "class C extends (() => {}) {}",
        "class C extends (async function () {}) {}",
        "class C extends (function* () {}) {}",
        "class C extends ({ m() {} }).m {}",
        "class A { m() {} } class C extends A.prototype.m {}",
    ] {
        assert_eq!(
            ok(&format!(
                "var r; try {{ {source} }} catch (e) {{ r = e.name; }} r"
            )),
            "TypeError",
            "{source}"
        );
    }
    assert_eq!(
        ok("function F() {} class C extends F {} new C() instanceof F"),
        "true"
    );
    assert_eq!(ok("class C extends Symbol {} typeof C"), "function");
}

#[test]
fn class_inner_name_rejects_assignment_in_members() {
    for source in [
        "class C { get x() { C = 42; } }; new C().x",
        "class C { set x(_) { C = 42; } }; new C().x = 15;",
        "(new (class C { get x() { C = 42; } })).x",
        "(new (class C { set x(_) { C = 42; } })).x = 15;",
    ] {
        assert_eq!(
            ok(&format!(
                "var r; try {{ {source} }} catch (e) {{ r = e.name; }} r"
            )),
            "TypeError",
            "{source}"
        );
    }
    assert_eq!(
        ok("var r; try { class C { m() { C = 42; } } new C().m(); } catch (e) { r = e.name; } r"),
        "TypeError"
    );
    assert_eq!(
        ok(
            "var r; try { class C { constructor() { C = 42; } } new C(); } catch (e) { r = e.name; } r"
        ),
        "TypeError"
    );
}

#[test]
fn this_before_super_is_a_reference_error_in_every_position() {
    assert_eq!(
        ok(
            "class A {} class B extends A { constructor() { var e; try { this.p = 3; } catch (x) { e = x.name; } super(); this.r = e; } } new B().r"
        ),
        "ReferenceError"
    );
}

#[test]
fn super_property_reads_check_this_before_the_key_expression() {
    assert_eq!(
        ok(
            "var r; class A {} class B extends A { constructor() { try { super[super()]; } catch (e) { r = e.name; } super(); } } new B(); r"
        ),
        "ReferenceError"
    );
}

#[test]
fn super_binds_this_once_after_the_parent_constructs() {
    // §13.3.7.1 steps 3-6: the arguments and the parent construction happen
    // before BindThisValue, which throws when `this` is already bound.
    assert_eq!(
        ok(
            "var calls = 0; function f() { calls++; return 3; } class A {} class B extends A { constructor() { super(); try { super(f()); } catch (e) { this.r = e.name; this.calls = calls; } } } var b = new B(); b.r + ',' + b.calls"
        ),
        "ReferenceError,1"
    );
}

#[test]
fn class_field_initializers_name_their_anonymous_functions() {
    // §15.7.10 and §8.4.5 NamedEvaluation: a field's anonymous function takes
    // the field's key, and a private field's `#name`.
    assert_eq!(
        ok(
            "class C { static #f = () => 1; static g = function() {}; static read() { return this.#f.name; } } C.read() + ',' + C.g.name"
        ),
        "#f,g"
    );
    assert_eq!(
        ok(
            "class D { h = () => 1; #p = function() {}; n() { return this.#p.name; } } var d = new D(); d.h.name + ',' + d.n()"
        ),
        "h,#p"
    );
}

#[test]
fn super_property_assignment_works_inside_a_field_arrow() {
    assert_eq!(
        ok(
            "class B { set prop(v) { this.stored = v; } } class C extends B { func = () => { super.prop = 'x'; }; } var c = new C(); c.func(); c.stored"
        ),
        "x"
    );
}

#[test]
fn super_call_in_an_arrow_inside_a_derived_constructor_runs_later() {
    assert_eq!(
        ok(
            "var iter = { [Symbol.iterator]() { return this; }, next() { return { done: false }; }, return() { this.f(); return { done: true }; } }; class C extends class {} { constructor() { iter.f = () => super(); for (var k of iter) { return; } } } var o = new C(); typeof o"
        ),
        "object"
    );
}

#[test]
fn a_throwing_field_initializer_restores_the_caller_environment() {
    assert_eq!(
        ok(
            "function make() { return class C { x = (() => { throw 1; })(); }; } \
            function g() { let q = 'Q'; try { new (make())(); } catch (e) {} return q; } g()"
        ),
        "Q"
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

#[test]
fn an_escaped_of_is_not_the_for_of_keyword() {
    let error = eval("var x; for (x o\\u0066 []) ;").expect_err("escaped of");
    assert_eq!(error.kind(), JsErrorKind::Syntax);
    let error = eval("async function* f() { for await (var x o\\u0066 []) ; }")
        .expect_err("escaped of in for await");
    assert_eq!(error.kind(), JsErrorKind::Syntax);
}

#[test]
fn strict_code_refuses_eval_and_arguments_as_assignment_targets() {
    let error = eval("'use strict'; for ([arguments] of [[]]) ;").expect_err("strict target");
    assert_eq!(error.kind(), JsErrorKind::Syntax);
    let error = eval("'use strict'; arguments = 1;").expect_err("strict simple target");
    assert_eq!(error.kind(), JsErrorKind::Syntax);
    assert_eq!(ok("var arguments = 1; arguments = 2; arguments"), "2");
}

#[test]
fn generators_and_async_functions_are_not_constructors() {
    assert_eq!(
        eval("function* g() {} new g();").unwrap_err().kind(),
        JsErrorKind::Type
    );
    assert_eq!(
        eval("new (async function () {})();").unwrap_err().kind(),
        JsErrorKind::Type
    );
    assert_eq!(ok("function f() {} String(new f() instanceof f)"), "true");
}

#[test]
fn non_writable_symbol_keyed_properties_ignore_writes() {
    assert_eq!(
        ok(
            "var o = Object.defineProperty({}, Symbol.toStringTag, { value: 'x', writable: false }); o[Symbol.toStringTag] = 'y'; o[Symbol.toStringTag]"
        ),
        "x"
    );
    assert_eq!(
        ok(
            "var p = Object.defineProperty({}, Symbol.toStringTag, { value: 'x', writable: false }); var o = Object.create(p); o[Symbol.toStringTag] = 'z'; Object.prototype.hasOwnProperty.call(o, Symbol.toStringTag) ? 'own' : o[Symbol.toStringTag]"
        ),
        "x"
    );
}

#[test]
fn iterator_steps_read_done_and_value_through_getters() {
    assert_eq!(
        ok(
            "var iter = {}; iter[Symbol.iterator] = function () { return { next() { \
            return Object.defineProperty({}, 'value', { get() { throw new Error('v'); } }); } }; }; \
            var r; try { var [...x] = iter; r = 'no'; } catch (e) { r = e.message; } r"
        ),
        "v"
    );
}

#[test]
fn array_patterns_use_the_array_iterator_protocol() {
    assert_eq!(
        ok(
            "delete Array.prototype[Symbol.iterator]; var r; try { var [x] = [1]; r = 'no'; } catch (e) { r = e.constructor.name; } r"
        ),
        "TypeError"
    );
    assert_eq!(
        ok(
            "var saved = Array.prototype[Symbol.iterator]; Array.prototype[Symbol.iterator] = function () { return { next() { return { done: true }; } }; }; var [a] = [1]; Array.prototype[Symbol.iterator] = saved; String(a)"
        ),
        "undefined"
    );
}

#[test]
fn regexp_indices_surrogates_and_flag_strings_at_the_script_level() {
    // The `d` flag adds `indices`: a [start, end] pair per group, and a
    // `groups` object for named groups.
    assert_eq!(ok("/(a)(?<n>b)?/d.exec('ab').indices[1].join()"), "0,1");
    assert_eq!(
        ok("String(/(a)(?<n>x)?/d.exec('a').indices[2])"),
        "undefined"
    );
    assert_eq!(ok("/(?<n>b)/d.exec('ab').indices.groups.n.join()"), "1,2");
    assert_eq!(ok("String(/a/.exec('a').indices)"), "undefined");
    assert_eq!(ok("/a/d.hasIndices + '/' + /a/.hasIndices"), "true/false");
    // The flags string lists d, g, i, m, s, u, v, y in that order.
    assert_eq!(ok("/a/d.flags + '/' + /a/gimsuy.flags"), "d/gimsuy");
    assert_eq!(ok("/a/v.flags"), "v");
    // Under u a pair is one character; without it, one code unit.
    assert_eq!(ok("'\\u{1F600}'.match(/./u)[0].length"), "2");
    assert_eq!(ok("'\\u{1F600}'.match(/./)[0].length"), "1");
    assert_eq!(ok("/\\udf06/u.test('\\ud834\\udf06')"), "false");
    assert_eq!(ok("/\\udf06/.test('\\ud834\\udf06')"), "true");
}

#[test]
fn regexp_flags_and_source_are_prototype_accessors_not_own_properties() {
    assert_eq!(
        ok("var r = /a/gi; [r.global, r.ignoreCase, r.multiline].join()"),
        "true,true,false"
    );
    assert_eq!(ok("/a/gimsuy.flags"), "gimsuy");
    assert_eq!(ok("/x\\/y/.source"), "x\\/y");
    assert_eq!(ok("Object.keys(/a/g).length"), "0");
    assert_eq!(ok("Object.getOwnPropertyNames(/a/).join()"), "lastIndex");
    assert_eq!(ok("'global' in /a/ ? 'yes' : 'no'"), "yes");
    assert_eq!(ok("String(RegExp.prototype.source)"), "(?:)");
    assert_eq!(ok("String(RegExp.prototype.global)"), "undefined");
    assert_eq!(ok("RegExp.prototype.flags === ''"), "true");
}

#[test]
fn regexp_flags_getter_reads_each_flag_through_get() {
    assert_eq!(
        ok("var r = /a/; Object.defineProperty(r, 'global', { get() { return true; } }); r.flags"),
        "g"
    );
}

#[test]
fn a_unicode_back_reference_matches_whole_code_points() {
    assert_eq!(
        ok(r#"String(/foo(.+)bar\1/u.exec("foo\uD834bar\uD834\uDC00"))"#),
        "null"
    );
    assert_eq!(
        ok(r#"String(/foo(.+)bar\1/u.exec("foo\uD834bar\uD834"))"#),
        "foo\u{f0034}bar\u{f0034},\u{f0034}"
    );
    assert_eq!(
        ok(r#"String(/^(.+)\1$/u.exec("\uDC00foobar\uD834\uDC00foobar\uD834"))"#),
        "null"
    );
}

#[test]
fn property_is_enumerable_reads_symbol_keyed_own_properties() {
    // \u00A720.1.3.4: the key goes through ToPropertyKey, so a symbol key finds
    // its own property the same way a string key does.
    assert_eq!(
        ok("var s = Symbol(); var o = {}; o[s] = 1; o.propertyIsEnumerable(s)"),
        "true"
    );
    assert_eq!(
        ok(
            "var s = Symbol(); var o = {}; Object.defineProperty(o, s, { value: 1 }); o.propertyIsEnumerable(s)"
        ),
        "false"
    );
    assert_eq!(
        ok("var s = Symbol(); var o = {}; o.propertyIsEnumerable(s)"),
        "false"
    );
    assert_eq!(
        ok(
            "var s = Symbol(); var o = {}; o[s] = 1; Object.prototype.propertyIsEnumerable.call(o, s)"
        ),
        "true"
    );
    assert_eq!(ok("var o = { a: 1 }; o.propertyIsEnumerable('a')"), "true");
}

#[test]
fn cover_initialized_names_are_patterns_only() {
    // §13.2.5.1 CoverInitializedName: `{ name = value }` is an AssignmentPattern
    // once the literal is the target of `=` or a for-in/of head.
    assert_eq!(ok("var x; ({ x = 1 } = {}); x"), "1");
    assert_eq!(ok("var x; ({ x = 2 } = { x: 5 }); x"), "5");
    assert_eq!(ok("var a; [{ a = 3 }] = [{}]; a"), "3");
    assert_eq!(ok("var n; for ({ n = 4 } of [{}]) ; n"), "4");
    assert_eq!(ok("var b; ({ p: { b = 2 } } = { p: {} }); b"), "2");
    assert_eq!(ok("var f; ({ f = function () {} } = {}); f.name"), "f");
    assert_eq!(ok("var z; (({ z = 9 } = {})); z"), "9");
    assert_eq!(ok("(({ x = 7 }) => x)({})"), "7");
    // A `=` nested inside a pattern position resolves its own members.
    assert_eq!(
        ok("var p, q; [{ p = 1 }, { q = 2 }] = [{}, {}]; p + ':' + q"),
        "1:2"
    );
    assert_eq!(ok("var r; [{ r = 5 } = {}] = []; r"), "5");
    for source in [
        "({ x = 1 })",
        "var o = { x = 1 };",
        "function f() {} f({ x = 1 });",
        "var a = [{ x = 1 }];",
        "var a; [{ a = 1 }.a] = [];",
        "var a; ({ a = 1 }).a = 2;",
        "var a; ({ a = 1 }) += 1;",
        "var a; [{ a = 1 } += 1] = [];",
        "for ({ x = 1 }; false;) ;",
        "var a = true ? { x = 1 } : 2;",
        "var a; [a = { x = 1 }] = [];",
        "var a; ({ a = 1 } &&= 2);",
        "'use strict'; var r; ({ eval = 1 } = {});",
        "var o; ({ ...{ x = 1 } } = {});",
        "var r; ({ if = 1 } = {});",
    ] {
        assert_eq!(
            eval(source).unwrap_err().kind(),
            JsErrorKind::Syntax,
            "{source}"
        );
    }
}

#[test]
fn for_in_destructuring_assigns_each_key_to_the_pattern() {
    // §14.7.5.6: a for-in head may bind or assign a pattern from each key.
    assert_eq!(ok("var a; for (var [a] in { xy: 1 }) ; a"), "x");
    assert_eq!(ok("var z; for (var { 0: z } in { ab: 1 }) ; z"), "a");
    assert_eq!(ok("var w; for ([w] in { ab: 1 }) ; w"), "a");
    assert_eq!(ok("var v; for ({ 0: v } in { ab: 1 }) ; v"), "a");
    assert_eq!(
        ok(
            "var fs = []; for (let [k] in { ab: 1, cd: 1 }) fs.push(function () { return k; }); fs[0]() + fs[1]()"
        ),
        "ac"
    );
    assert_eq!(ok("var t; for (t in { p: 1 }) ; t"), "p");
    assert_eq!(ok("var o = {}; for (o.k in { q: 1 }) ; o.k"), "q");
    // `var` may repeat a name, but a lexical head may not (ECMA-262 14.7.5.1).
    assert_eq!(ok("var x, y; for (var [x, y] in { ab: 1 }) ; x + y"), "ab");
    for source in [
        "for (let [x, x] in {}) {}",
        "for (const [x, x] of []) {}",
        "for (let { a: x, b: x } of []) {}",
    ] {
        assert_eq!(
            eval(source).unwrap_err().kind(),
            JsErrorKind::Syntax,
            "{source}"
        );
    }
}

#[test]
fn private_methods_are_own_elements_installed_on_each_instance() {
    // §7.3.31 PrivateMethodOrAccessorAdd and §7.3.32 PrivateElementFind: a
    // private method is the instance's own element, so a prototype or a
    // subclass cannot supply it, and a second initialization is a TypeError.
    assert_eq!(
        ok(
            "class C { #m() { return 7; } get #g() { return 3; } t() { return this.#m() + this.#g; } } new C().t()"
        ),
        "10"
    );
    assert_eq!(
        ok(
            "class C { #m() { return 7; } static r(o) { return o.#m(); } } var r; try { C.r(Object.create(C.prototype)); } catch (e) { r = e.name; } r"
        ),
        "TypeError"
    );
    assert_eq!(
        ok(
            "class C { static #s() { return 1; } static s() { return this.#s(); } } class D extends C {} var r; try { D.s(); } catch (e) { r = e.name; } r"
        ),
        "TypeError"
    );
    assert_eq!(
        ok(
            "class C { #m() {} static has(o) { return #m in o; } } C.has(new C()) + ':' + C.has({})"
        ),
        "true:false"
    );
    assert_eq!(
        ok(
            "class Base { constructor(o) { return o; } } class C extends Base { #m() {} } var obj = {}; new C(obj); var r; try { new C(obj); } catch (e) { r = e.name; } r"
        ),
        "TypeError"
    );
    assert_eq!(
        ok(
            "class A { constructor(arg) { return arg; } } class C extends A { #x; constructor(arg) { super(arg); } } var holder = new C(); var r; try { new C(holder); } catch (e) { r = e.name; } r"
        ),
        "TypeError"
    );
}

#[test]
fn switch_selects_the_matching_clause_before_falling_back_to_default() {
    // §14.12.4 CaseBlockEvaluation: every case is tested before `default` is
    // used, so a `default` that comes first does not run when a later case matches.
    assert_eq!(
        ok("var r = ''; switch (2) { default: r += 'd'; case 2: r += 'b'; } r"),
        "b"
    );
    assert_eq!(
        ok("var r = ''; switch (9) { default: r += 'd'; case 2: r += 'b'; } r"),
        "db"
    );
    assert_eq!(
        ok("var r = ''; switch (1) { case 0: r += 'a'; default: r += 'd'; case 1: r += 'b'; } r"),
        "b"
    );
    assert_eq!(
        ok("var r = ''; switch (7) { case 0: r += 'a'; default: r += 'd'; case 1: r += 'b'; } r"),
        "db"
    );
    assert_eq!(
        ok("var r = 'none'; switch (5) { case 1: r = 'one'; } r"),
        "none"
    );
}

#[test]
fn switch_case_block_is_one_lexical_scope_with_hoisted_functions() {
    // §14.12.4 step 2: the case block's declarations are instantiated before any
    // clause runs, and they are scoped to the switch.
    assert_eq!(
        ok("var r; switch (1) { case 1: function f() { return 5; } r = f(); } r"),
        "5"
    );
    assert_eq!(
        ok("var r; switch (0) { default: function g() { return 6; } r = g(); } r"),
        "6"
    );
    assert_eq!(
        ok("switch (1) { case 1: let a = 4; } typeof a"),
        "undefined"
    );
    assert_eq!(
        ok("var r = 'ok'; switch (0) { case 1: function f() {} default: function f() {} } r"),
        "ok"
    );
}

#[test]
fn object_literal_values_name_anonymous_functions_after_their_key() {
    // §13.2.5.5 step 7: `key: AssignmentExpression` names an anonymous function,
    // arrow or class after the key; a symbol key gives `[description]`.
    assert_eq!(
        ok(
            "var o = { id: () => {}, f: function () {}, c: class {} }; [o.id.name, o.f.name, o.c.name].join()"
        ),
        "id,f,c"
    );
    assert_eq!(
        ok(
            "var s = Symbol('d'); var t = Symbol(); var o = { [s]: () => {}, [t]: function () {} }; o[s].name + '|' + o[t].name"
        ),
        "[d]|"
    );
    // A named function keeps its own name.
    assert_eq!(ok("({ g: function h() {} }).g.name"), "h");
}

#[test]
fn object_literal_methods_read_super_from_their_home_object() {
    // §13.2.5.4 and §15.4.4: a method's [[HomeObject]] is its object literal, so
    // `super.name` reads the prototype of that object, and a method is not a constructor.
    assert_eq!(
        ok(
            "var proto = { m() { return 'p'; } }; var o = { __proto__: proto, m() { return super.m() + 'o'; } }; o.m()"
        ),
        "po"
    );
    assert_eq!(
        ok("var o = { m() { return super.x; } }; Object.setPrototypeOf(o, { x: 5 }); o.m()"),
        "5"
    );
    assert_eq!(
        ok(
            "var base = { get v() { return 2; } }; var o = { __proto__: base, get v() { return super.v * 10; } }; o.v"
        ),
        "20"
    );
    assert_eq!(
        ok("var p = { x: 3 }; var o = { __proto__: p, m() { return (() => super.x)(); } }; o.m()"),
        "3"
    );
    assert_eq!(
        ok("var o = { __proto__: {}, m() { super.y = 4; return this.y; } }; o.m()"),
        "4"
    );
    assert_eq!(
        ok("var r; try { new ({ m() {} }).m(); } catch (e) { r = e.name; } r"),
        "TypeError"
    );
}

#[test]
fn labels_refuse_strict_reserved_words_and_labelled_functions_are_sloppy_only() {
    // §13.1.1 and §14.13.1: a label is a LabelIdentifier, and Annex B.3.2 allows a
    // labelled plain function only where the code is sloppy.
    assert_eq!(ok("l: function f() {} 'ok'"), "ok");
    assert_eq!(ok("yield: 1; 'ok'"), "ok");
    for source in [
        "'use strict'; l: function f() {}",
        "'use strict'; yield: 1;",
        "'use strict'; let: 1;",
    ] {
        assert_eq!(
            eval(source).unwrap_err().kind(),
            JsErrorKind::Syntax,
            "{source}"
        );
    }
}

#[test]
fn an_escaped_contextual_keyword_is_an_identifier_not_a_modifier() {
    // §12.7.2: an identifier spelled with escapes is never the keyword it spells,
    // so `get`, `async` and `new.target` are early errors where the keyword is required.
    for source in [
        "({ \\u0067et x() {} })",
        "({ \\u0061sync m() {} })",
        "class C { st\\u0061tic m() {} }",
        "function f() { return new.\\u0074arget; }",
        "\\u0061sync function f() {}",
    ] {
        assert_eq!(
            eval(source).unwrap_err().kind(),
            JsErrorKind::Syntax,
            "{source}"
        );
    }
}

#[test]
fn a_slash_after_a_private_name_is_division() {
    // §12.5: a private name is an operand, so `/` after it is not a regex.
    assert_eq!(
        ok("class C { #f = 4; m() { return this.#f /= 2; } } new C().m()"),
        "2"
    );
    assert_eq!(
        ok("class C { #f = 4; m() { return this.#f / 2; } } new C().m()"),
        "2"
    );
}

#[test]
fn array_assignment_pattern_takes_only_what_it_needs_and_closes_the_iterator() {
    // §13.15.5.5: the pattern steps the iterator per element, and an early
    // finish calls `return()`, which must produce an object.
    assert_eq!(
        ok("var r; try { [] = 1; } catch (e) { r = e.name; } r"),
        "TypeError"
    );
    assert_eq!(
        ok(
            "var x; var it = { [Symbol.iterator]() { return { next() { return { value: 1, done: false }; }, return() { return null; } }; } }; var r; try { [x] = it; } catch (e) { r = e.name; } r"
        ),
        "TypeError"
    );
    assert_eq!(
        ok(
            "var q; var it = { [Symbol.iterator]() { return { next() { return { value: 1, done: false }; }, return() { throw new RangeError('stop'); } }; } }; var r; try { [q] = it; } catch (e) { r = e.name; } r"
        ),
        "RangeError"
    );
    assert_eq!(
        ok(
            "var n = 0; var a; var it = { [Symbol.iterator]() { return { next() { n++; return { value: n, done: false }; }, return() { return {}; } }; } }; [a] = it; a + ':' + n"
        ),
        "1:1"
    );
    assert_eq!(
        ok("function* g() { var i = 0; while (true) yield i++; } var b, c; [b, c] = g(); b + c"),
        "1"
    );
    assert_eq!(ok("var r; [...r] = [1, 2, 3]; r.join()"), "1,2,3");
    // A `next()` that throws leaves the iterator done, so it is not closed.
    assert_eq!(
        ok(
            "var n = 0, c = 0; var it = { [Symbol.iterator]() { return { next() { n++; throw new RangeError('x'); }, return() { c++; return {}; } }; } }; var a; try { [a] = it; } catch (e) {} n + ':' + c"
        ),
        "1:0"
    );
    assert_eq!(
        ok("var a, b; [, a, , b] = [1, 2, 3, 4]; a + ':' + b"),
        "2:4"
    );
}

#[test]
fn object_keys_convert_with_to_property_key() {
    // §7.1.19 ToPropertyKey: an object key runs ToPrimitive (string hint)
    // first, so a Symbol wrapper addresses the symbol it wraps everywhere a
    // key is taken: member read and write, `in`, literals and `hasOwnProperty`.
    assert_eq!(
        ok(
            "var s = Object(Symbol()); var obj = {}; obj[s] = 'ok'; [s in obj, obj.hasOwnProperty(s), obj[s], Object.getOwnPropertySymbols(obj).length].join()"
        ),
        "true,true,ok,1"
    );
    assert_eq!(
        ok(
            "var u = Symbol(); var s = Object(u); var obj = {[s]: 1}; [Object.getOwnPropertySymbols(obj)[0] === u, obj[u], u in obj].join()"
        ),
        "true,1,true"
    );
    assert_eq!(
        ok(
            "var k = { toString() { return 'named'; } }; var obj = {}; obj[k] = 1; Object.keys(obj).join()"
        ),
        "named"
    );
}

#[test]
fn computed_keys_convert_after_the_base_check_and_the_value() {
    // A null base throws before its key converts (GetValue and PutValue coerce
    // the base first), and a plain assignment converts its key in PutValue,
    // after the value expression runs.
    assert_eq!(
        ok(
            "var r = []; var k = { toString() { r.push('key'); return 'k'; } }; try { null[k] += (r.push('rhs'), 1); } catch (e) { r.push(e.name); } r.join()"
        ),
        "TypeError"
    );
    assert_eq!(
        ok(
            "var log = []; var base = {}; var k = { toString() { log.push('key'); return 'k'; } }; base[k] = (log.push('rhs'), 1); log.join()"
        ),
        "rhs,key"
    );
}

#[test]
fn unicode_property_space_is_the_white_space_alias() {
    // ECMA-262 Table 67: `\p{space}` names the White_Space property.
    assert_eq!(
        ok(
            "[/^\\p{space}+$/u.test(' \\t\\u00a0\\u3000\\u2028'), /\\p{space}/u.test('a'), /\\P{space}/u.test('a')].join()"
        ),
        "true,false,true"
    );
}

#[test]
fn array_buffer_is_view_getter_names_and_buffer_constructors() {
    // §25.1.5.1: `isView` is true for typed arrays and DataViews by slot; the
    // getters are named `get <property>`; buffer hosts are constructors.
    assert_eq!(
        ok(
            "[ArrayBuffer.isView(new Uint8Array(1)), ArrayBuffer.isView(new DataView(new ArrayBuffer(1))), ArrayBuffer.isView([]), ArrayBuffer.isView(), ArrayBuffer.isView.length, ArrayBuffer.isView.name].join()"
        ),
        "true,true,false,false,1,isView"
    );
    assert_eq!(
        ok(
            "var d = Object.getOwnPropertyDescriptor(ArrayBuffer.prototype, 'byteLength'); d.get.name"
        ),
        "get byteLength"
    );
    assert_eq!(
        ok(
            "class Sized extends ArrayBuffer {}; [new Sized(3).byteLength, Reflect.construct(ArrayBuffer, [5]).byteLength, Reflect.construct(DataView, [new ArrayBuffer(2)]).byteLength].join()"
        ),
        "3,5,2"
    );
}

#[test]
fn data_view_bigint_accessors_read_and_write_both_byte_orders() {
    // ECMA-262 25.2.1.5 and 25.2.1.6: the eight bytes hold the value modulo
    // 2^64, big-endian unless the accessor is asked for little-endian.
    assert_eq!(
        ok(
            "var v = new DataView(new ArrayBuffer(16)); v.setBigInt64(0, -2n); v.setBigUint64(8, (2n ** 64n) - 1n); [v.getBigInt64(0), v.getBigUint64(8), v.getBigUint64(0), v.getBigInt64(8, true), v.getUint8(7)].join()"
        ),
        "-2,18446744073709551615,18446744073709551614,-1,254"
    );
    assert_eq!(
        ok(
            "var v = new DataView(new ArrayBuffer(8)); var r; try { v.setBigInt64(0, 1); } catch (e) { r = e.name; } [r, DataView.prototype.getBigInt64.length, DataView.prototype.setBigInt64.length].join()"
        ),
        "TypeError,1,2"
    );
}

#[test]
fn bigint_typed_arrays_store_and_read_bigints() {
    // ECMA-262 23.2 and 10.4.5.11: BigInt64Array and BigUint64Array elements
    // are BigInts, stored modulo 2^64.
    assert_eq!(
        ok(
            "var a = new BigInt64Array(2); a[0] = -5n; a[1] = 2n ** 63n; [a[0], a[1], a.length, a.byteLength, BigInt64Array.BYTES_PER_ELEMENT, Object.prototype.toString.call(a)].join()"
        ),
        "-5,-9223372036854775808,2,16,8,[object BigInt64Array]"
    );
    assert_eq!(
        ok(
            "var b = new BigUint64Array([1n, (2n ** 64n) - 1n]); var c = BigInt64Array.from([3n, -1n]); var d = BigInt64Array.of(7n); [b[0], b[1], typeof b[0], c[1], d[0]].join()"
        ),
        "1,18446744073709551615,bigint,-1,7"
    );
    assert_eq!(
        ok("[Int8Array.from('ab').join(), Int8Array.from([1, 2], (x) => x * 3).join()].join()"),
        "0,0,3,6"
    );
}

#[test]
fn bigint_typed_array_methods_work_on_bigints() {
    // The element methods read and write BigInts, and compare and sort them as
    // BigInts (ECMA-262 23.2.3).
    assert_eq!(
        ok(
            "var a = new BigInt64Array([3n, -1n, 2n]); [a.indexOf(2n), a.includes(-1n), a.indexOf(2), a.join('|'), a.at(-1), a.slice(1).join(), a.map((x) => x * 2n).join(), a.filter((x) => x > 0n).join(), a.reduce((s, x) => s + x, 0n), a.sort().join(), a.toSorted().join(), a.with(0, 9n)[0], a.reverse().join()].join(';')"
        ),
        "2;true;-1;3|-1|2;2;-1,2;6,-2,4;3,2;4;-1,2,3;-1,2,3;9;3,2,-1"
    );
}

#[test]
fn array_define_own_property_grows_and_shrinks_length() {
    // §10.4.2.1: an index at or past the length grows it; §10.4.2.4
    // `ArraySetLength` deletes the top indices and stops at the first one that
    // refuses, leaving `length` one past it and reporting failure.
    assert_eq!(
        ok(
            "var a = []; Object.defineProperty(a, '5', { value: 1, writable: true, enumerable: true, configurable: true }); a.length"
        ),
        "6"
    );
    assert_eq!(
        ok(
            "var a = [0, 1, 2]; Object.defineProperty(a, 'length', { value: 1 }); a.length + ':' + a.join()"
        ),
        "1:0"
    );
    assert_eq!(
        ok(
            "var a = [0, 1, 2]; Object.defineProperty(a, '1', { configurable: false }); var r; try { Object.defineProperty(a, 'length', { value: 0 }); } catch (e) { r = e.name; } [r, a.length, a.hasOwnProperty('0')].join()"
        ),
        "TypeError,2,true"
    );
    // A non-writable length refuses an index at or past it.
    assert_eq!(
        ok(
            "var a = []; Object.defineProperty(a, 'length', { writable: false }); var r; try { Object.defineProperty(a, '0', { value: 1, writable: true, enumerable: true, configurable: true }); } catch (e) { r = e.name; } [r, a.length, a.hasOwnProperty('0')].join()"
        ),
        "TypeError,0,false"
    );
    // §10.4.2.4 step 3: a length that is not a uint32 is a RangeError.
    assert_eq!(
        ok(
            "var r; try { Object.defineProperty([], 'length', { value: -1 }); } catch (e) { r = e.name; } r"
        ),
        "RangeError"
    );
    // §10.4.2.4 step 16: `writable: false` in the same define takes effect last.
    assert_eq!(
        ok(
            "var a = [1, 2, 3]; Object.defineProperty(a, 'length', { value: 1, writable: false }); var r; try { a.push(9); } catch (e) { r = e.name; } [a.length, a.join(), Object.getOwnPropertyDescriptor(a, 'length').writable, r].join()"
        ),
        "1,1,false,TypeError"
    );
}

#[test]
fn object_assign_reads_getters_copies_symbols_and_throws_on_refused_writes() {
    // §20.1.2.1: [[Get]] on each enumerable own key (strings, then symbols),
    // then a throwing [[Set]] on the target.
    assert_eq!(ok("Object.assign({}, { get a() { return 1; } }).a"), "1");
    assert_eq!(
        ok("var s = Symbol('x'); var src = {}; src[s] = 2; Object.assign({}, src)[s]"),
        "2"
    );
    assert_eq!(ok("Object.assign([], { 2: 'x' }).length"), "3");
    assert_eq!(
        ok(
            "var r = []; try { Object.assign(Object.freeze({ a: 1 }), { a: 2 }); } catch (e) { r.push(e.name); } try { Object.assign(null); } catch (e) { r.push(e.name); } r.join()"
        ),
        "TypeError,TypeError"
    );
    assert_eq!(
        ok(
            "var calls = []; var s = { get a() { calls.push('get a'); return 1; }, b: 2 }; Object.assign({}, s); calls.join()"
        ),
        "get a"
    );
}

#[test]
fn enumerable_own_property_helpers_use_get_and_throw_on_nullish() {
    // §7.3.22 EnumerableOwnProperties: `values` and `entries` read through [[Get]].
    assert_eq!(ok("Object.values({ get a() { return 7; } }).join()"), "7");
    assert_eq!(
        ok("Object.entries({ get a() { return 7; } })[0].join()"),
        "a,7"
    );
    assert_eq!(
        ok("var r; try { Object.keys(undefined); } catch (e) { r = e.name; } r"),
        "TypeError"
    );
}

#[test]
fn integrity_builtins_pass_non_objects_through() {
    // §20.1.2.5, 20.1.2.6, 20.1.2.13, 20.1.2.14, 20.1.2.15, 20.1.2.16 (ECMA-262).
    assert_eq!(
        ok(
            "[Object.isFrozen(1), Object.isSealed('s'), Object.isExtensible(1), Object.seal('s'), Object.freeze(true), Object.preventExtensions(null), Object.freeze(undefined) === undefined].join()"
        ),
        "true,true,false,s,true,,true"
    );
    // A string wrapper's virtual slots are already frozen-compatible.
    assert_eq!(
        ok("var s = Object.freeze(new String('ab')); [Object.isFrozen(s), s[0]].join()"),
        "true,a"
    );
}

#[test]
fn object_to_string_tags_follow_builtin_and_symbol_to_string_tag() {
    // §20.1.3.6: a string-valued @@toStringTag wins, a non-string one is
    // ignored, and a function or array proxy tags as its target does.
    assert_eq!(
        ok(
            "var t = Object.prototype.toString; [t.call(new Map()), t.call(new Set()), t.call(Symbol('d')), t.call(1n)].join()"
        ),
        "[object Map],[object Set],[object Symbol],[object BigInt]"
    );
    assert_eq!(
        ok(
            "var t = Object.prototype.toString; delete Set.prototype[Symbol.toStringTag]; var r = t.call(new Set()); Object.defineProperty(Math, Symbol.toStringTag, { value: Symbol() }); r + t.call(Math)"
        ),
        "[object Object][object Object]"
    );
    assert_eq!(
        ok(
            "var t = Object.prototype.toString; [t.call(new Proxy(function () {}, {})), t.call(new Proxy([], {}))].join()"
        ),
        "[object Function],[object Array]"
    );
    assert_eq!(
        ok(
            "var r; try { Object.prototype.toString.call(Object.defineProperty({}, Symbol.toStringTag, { get() { throw new RangeError('tag'); } })); } catch (e) { r = e.name; } r"
        ),
        "RangeError"
    );
    // §20.1.3.5: `Object.prototype.toLocaleString` is `Invoke(this, "toString")`.
    assert_eq!(
        ok(
            "({ toString() { return 'T'; } }).toLocaleString() + ':' + Object.prototype.toLocaleString.call(5)"
        ),
        "T:5"
    );
}

#[test]
fn annex_b_accessor_helpers_walk_the_chain_and_check_their_arguments() {
    // B.2.2.4: `__lookupGetter__` reads the first own-or-inherited descriptor.
    assert_eq!(
        ok(
            "var p = { get x() { return 1; } }; var o = Object.create(p); [typeof o.__lookupGetter__('x'), o.__lookupGetter__('x') === Object.getOwnPropertyDescriptor(p, 'x').get, o.__lookupSetter__('x')].join()"
        ),
        "function,true,"
    );
    // B.2.2.2: a non-callable getter is a TypeError.
    assert_eq!(
        ok("var r; try { ({}).__defineGetter__('x', 1); } catch (e) { r = e.name; } r"),
        "TypeError"
    );
    assert_eq!(
        ok("var o = {}; o.__defineSetter__('y', function (v) { this.z = v; }); o.y = 4; o.z"),
        "4"
    );
}

#[test]
fn object_get_prototype_of_and_is_prototype_of_use_the_proxy_trap() {
    // §10.5.1 [[GetPrototypeOf]] runs the handler's trap.
    assert_eq!(
        ok(
            "Object.getPrototypeOf(new Proxy({}, { getPrototypeOf() { return Array.prototype; } })) === Array.prototype"
        ),
        "true"
    );
    assert_eq!(
        ok("Array.prototype.isPrototypeOf(new Proxy([], {}))"),
        "true"
    );
}

#[test]
fn proto_setter_and_entry_builders_follow_the_spec_on_edge_inputs() {
    // B.2.2.1.2: `RequireObjectCoercible(this)` runs before the argument.
    assert_eq!(
        ok(
            "var set = Object.getOwnPropertyDescriptor(Object.prototype, '__proto__').set; var r; try { set.call(null, {}); } catch (e) { r = e.name; } r"
        ),
        "TypeError"
    );
    // §20.1.2.7 `Object.fromEntries` uses CreateDataPropertyOrThrow, so an
    // inherited setter does not run.
    assert_eq!(
        ok(
            "Object.defineProperty(Object.prototype, 'fe_probe', { set() { throw new Error('setter ran'); }, configurable: true }); var v = Object.fromEntries([['fe_probe', 1]]).fe_probe; delete Object.prototype.fe_probe; v"
        ),
        "1"
    );
    // §20.1.2.10.1 `Object.groupBy` rejects a non-callable callback.
    assert_eq!(
        ok("var r; try { Object.groupBy([1], 5); } catch (e) { r = e.name; } r"),
        "TypeError"
    );
}

#[test]
fn proxy_own_keys_keep_symbols_and_reject_other_key_types() {
    // §10.5.11 [[OwnPropertyKeys]]: the trap's symbols reach the descriptor
    // trap as symbols, and Object.keys drops them.
    assert_eq!(
        ok(
            "var s = Symbol('k'); var seen = []; var p = new Proxy({}, { ownKeys() { return [s, 'a']; }, getOwnPropertyDescriptor(t, k) { seen.push(typeof k); return { value: 1, enumerable: true, configurable: true, writable: true }; } }); Object.assign({}, p); seen.join()"
        ),
        "symbol,string"
    );
    assert_eq!(
        ok(
            "var p = new Proxy({}, { ownKeys() { return [Symbol('x'), 'b']; }, getOwnPropertyDescriptor() { return { value: 1, enumerable: true, configurable: true }; } }); Object.keys(p).join()"
        ),
        "b"
    );
    assert_eq!(
        ok(
            "var r; try { Object.keys(new Proxy({}, { ownKeys() { return [1]; } })); } catch (e) { r = e.name; } r"
        ),
        "TypeError"
    );
}

#[test]
fn boolean_value_of_returns_the_boolean_and_accessor_functions_are_named() {
    // §20.3.3.3 `Boolean.prototype.valueOf` returns the Boolean, not its text.
    assert_eq!(
        ok("[new Boolean(true).valueOf() === true, String(new Boolean(false))].join()"),
        "true,false"
    );
    // §10.2.9 SetFunctionName: an accessor's function is named with its prefix.
    assert_eq!(
        ok(
            "var d = Object.getOwnPropertyDescriptor(Object.prototype, '__proto__'); [d.get.name, d.set.name, d.get.length, d.set.length].join()"
        ),
        "get __proto__,set __proto__,0,1"
    );
}

/// Declares `thrown(f)`, which reports the class of the error `f` throws.
const THROWN_HELPER: &str = "function thrown(f) { try { f(); return 'none'; } catch (e) { return e instanceof TypeError ? 'TypeError' : String(e); } }";

fn with_thrown_helper(body: &str) -> String {
    format!("{THROWN_HELPER}\n{body}")
}

#[test]
fn proxy_revocable_drops_the_handler_and_every_operation_throws() {
    assert_eq!(
        ok(&with_thrown_helper(
            "var r = Proxy.revocable({ x: 1 }, {}); var before = r.proxy.x;\
             r.revoke(); r.revoke();\
             before + ',' + thrown(function () { return r.proxy.x; }) + ',' + \
             thrown(function () { return Object.getPrototypeOf(r.proxy); }) + ',' + \
             thrown(function () { return 'x' in r.proxy; })"
        )),
        "1,TypeError,TypeError,TypeError"
    );
    assert_eq!(
        ok(
            "var r = Proxy.revocable({}, {}); [typeof r.revoke, r.revoke.length, r.revoke.name, Object.keys(r).join()].join('|')"
        ),
        "function|0||proxy,revoke"
    );
}

#[test]
fn callable_proxy_forwards_calls_to_the_apply_trap() {
    assert_eq!(
        ok("var seen = [];\
            var p = new Proxy(function (a, b) { return a + b; }, {\
              apply: function (target, self, args) { seen.push(self === undefined, args.length, args[0]); return 42; }\
            });\
            p(1, 2) + ',' + seen.join('|') + ',' + typeof p"),
        "42,true|2|1,function"
    );
    assert_eq!(
        ok("var p = new Proxy(function (a) { return a * 2; }, {}); p(21)"),
        "42"
    );
}

#[test]
fn constructable_proxy_forwards_new_to_the_construct_trap() {
    assert_eq!(
        ok("var P = new Proxy(function () { this.made = true; }, {\
              construct: function (target, args, newTarget) { return { viaTrap: args[0], sameNewTarget: newTarget === P }; }\
            });\
            var o = new P(7); o.viaTrap + ',' + o.sameNewTarget + ',' + ('made' in o)"),
        "7,true,false"
    );
    assert_eq!(
        ok("var P = new Proxy(function () { this.made = true; }, {}); var o = new P(); o.made"),
        "true"
    );
}

#[test]
fn get_own_property_descriptor_trap_keeps_the_target_invariants() {
    assert_eq!(
        ok(&with_thrown_helper(
            "var t = {}; Object.defineProperty(t, 'x', { value: 1, configurable: false });\
             var p = new Proxy(t, { getOwnPropertyDescriptor: function () { return undefined; } });\
             thrown(function () { return Object.getOwnPropertyDescriptor(p, 'x'); })"
        )),
        "TypeError"
    );
    assert_eq!(
        ok(
            "var p = new Proxy({}, { getOwnPropertyDescriptor: function () { return { value: 3, configurable: true }; } });\
            var d = Object.getOwnPropertyDescriptor(p, 'k'); d.value + ',' + d.writable + ',' + d.enumerable + ',' + d.configurable"
        ),
        "3,false,false,true"
    );
}

#[test]
fn define_property_trap_result_must_agree_with_the_target() {
    assert_eq!(
        ok(&with_thrown_helper(
            "var p = new Proxy({}, { defineProperty: function () { return true; } });\
             thrown(function () { Object.defineProperty(p, 'x', { value: 1, configurable: false }); })"
        )),
        "TypeError"
    );
    assert_eq!(
        ok(
            "var p = new Proxy({}, { defineProperty: function () { return false; } });\
            Reflect.defineProperty(p, 'x', { value: 1 }) + ',' + Object.keys(p).length"
        ),
        "false,0"
    );
}

#[test]
fn own_keys_trap_rejects_duplicates_and_missing_non_configurable_keys() {
    assert_eq!(
        ok(&with_thrown_helper(
            "var p = new Proxy({}, { ownKeys: function () { return ['a', 'a']; } });\
             thrown(function () { return Reflect.ownKeys(p); })"
        )),
        "TypeError"
    );
    assert_eq!(
        ok(&with_thrown_helper(
            "var t = {}; Object.defineProperty(t, 'k', { value: 1, configurable: false });\
             var p = new Proxy(t, { ownKeys: function () { return []; } });\
             thrown(function () { return Reflect.ownKeys(p); })"
        )),
        "TypeError"
    );
}

#[test]
fn own_keys_trap_returns_string_and_symbol_keys_in_order() {
    assert_eq!(
        ok("var s = Symbol('k');\
            var p = new Proxy({}, { ownKeys: function () { return ['a', s]; },\
              getOwnPropertyDescriptor: function () { return { value: 1, enumerable: true, configurable: true }; } });\
            var keys = Reflect.ownKeys(p); keys.length + ',' + (keys[1] === s)"),
        "2,true"
    );
}

#[test]
fn has_trap_cannot_hide_a_non_configurable_property() {
    assert_eq!(
        ok(&with_thrown_helper(
            "var t = {}; Object.defineProperty(t, 'x', { value: 1, configurable: false });\
             var p = new Proxy(t, { has: function () { return false; } });\
             thrown(function () { return 'x' in p; })"
        )),
        "TypeError"
    );
}

#[test]
fn set_trap_false_is_reported_by_reflect_and_ignored_by_assignment() {
    assert_eq!(
        ok(
            "var p = new Proxy({}, { set: function () { return false; } });\
            Reflect.set(p, 'a', 1) + ',' + (function () { p.a = 1; return 'silent'; })()"
        ),
        "false,silent"
    );
}

#[test]
fn get_trap_must_agree_with_a_non_writable_non_configurable_property() {
    assert_eq!(
        ok(&with_thrown_helper(
            "var t = {}; Object.defineProperty(t, 'x', { value: 1, writable: false, configurable: false });\
             var p = new Proxy(t, { get: function () { return 2; } });\
             thrown(function () { return p.x; })"
        )),
        "TypeError"
    );
}

#[test]
fn delete_property_trap_cannot_delete_a_non_configurable_property() {
    assert_eq!(
        ok(&with_thrown_helper(
            "var t = {}; Object.defineProperty(t, 'x', { value: 1, configurable: false });\
             var p = new Proxy(t, { deleteProperty: function () { return true; } });\
             thrown(function () { return Reflect.deleteProperty(p, 'x'); })"
        )),
        "TypeError"
    );
}

#[test]
fn get_prototype_of_trap_must_match_a_non_extensible_target() {
    assert_eq!(
        ok(&with_thrown_helper(
            "var t = Object.preventExtensions({});\
             var p = new Proxy(t, { getPrototypeOf: function () { return Array.prototype; } });\
             thrown(function () { return Object.getPrototypeOf(p); })"
        )),
        "TypeError"
    );
}

#[test]
fn proxy_on_the_prototype_chain_answers_get_and_in_with_the_child_receiver() {
    assert_eq!(
        ok("var seen;\
            var p = new Proxy({}, { get: function (t, k, receiver) { seen = receiver; return 'via-trap:' + k; } });\
            var child = Object.create(p);\
            child.foo + ',' + (seen === child)"),
        "via-trap:foo,true"
    );
    assert_eq!(
        ok(
            "var p = new Proxy({}, { has: function (t, k) { return k === 'hit'; } });\
            var child = Object.create(p);\
            ('hit' in child) + ',' + ('miss' in child)"
        ),
        "true,false"
    );
}

#[test]
fn function_prototype_methods_carry_their_spec_length_and_name() {
    assert_eq!(
        ok(
            "[Function.prototype.toString.length, Function.prototype.call.length, Function.prototype.apply.length, Function.prototype.bind.length, Function.prototype.call.name, Function.length, Function.prototype.length].join()"
        ),
        "0,1,2,1,call,1,0"
    );
    assert_eq!(
        ok(
            "var d = Object.getOwnPropertyDescriptor(Function.prototype, 'name'); [d.writable, d.enumerable, d.configurable].join()"
        ),
        "false,false,true"
    );
}

#[test]
fn apply_rejects_a_primitive_argument_array_and_reads_array_likes() {
    assert_eq!(
        ok(&with_thrown_helper(
            "thrown(function () { return Function.prototype.apply.call(function () {}, null, 1); })"
        )),
        "TypeError"
    );
    assert_eq!(
        ok(
            "Math.max.apply(null, { length: 2, 0: 3, 1: 5 }) + ',' + (function () { return arguments.length; }).apply(null, undefined)"
        ),
        "5,0"
    );
}

#[test]
fn bind_computes_length_and_name_from_the_target_own_properties() {
    assert_eq!(
        ok(
            "function f(a, b, c) {} [f.bind(null, 1).length, f.bind(null, 1, 2, 3, 4).length, f.bind().name].join('|')"
        ),
        "2|0|bound f"
    );
    assert_eq!(
        ok(
            "var g = function () {}; Object.defineProperty(g, 'name', { value: 5 }); g.bind().name + '|' + g.bind().length"
        ),
        "bound |0"
    );
    assert_eq!(
        ok(
            "var h = function () {}; Object.defineProperty(h, 'length', { value: Infinity }); String(h.bind(null, 1).length)"
        ),
        "Infinity"
    );
}

#[test]
fn has_instance_is_the_intrinsic_that_instanceof_consults() {
    assert_eq!(
        ok(
            "var d = Object.getOwnPropertyDescriptor(Function.prototype, Symbol.hasInstance);\
            [typeof d.value, d.writable, d.enumerable, d.configurable, d.value.name, d.value.length].join('|')"
        ),
        "function|false|false|false|[Symbol.hasInstance]|1"
    );
    assert_eq!(
        ok(
            "[Function.prototype[Symbol.hasInstance].call(Object, {}), Function.prototype[Symbol.hasInstance].call(1, {}), [] instanceof Array, ({}) instanceof Array].join()"
        ),
        "true,false,true,false"
    );
    assert_eq!(
        ok(
            "var C = { [Symbol.hasInstance](value) { return value === 1; } }; [1 instanceof C, 2 instanceof C].join()"
        ),
        "true,false"
    );
    assert_eq!(
        ok(&with_thrown_helper(
            "thrown(function () { return ({}) instanceof {}; })"
        )),
        "TypeError"
    );
}

#[test]
fn symbol_keyed_get_and_delete_run_the_proxy_traps() {
    assert_eq!(
        ok("var s = Symbol('s');\
            var p = new Proxy({}, { get: function (t, k) { return k === s ? 'sym' : undefined; } });\
            p[s]"),
        "sym"
    );
    assert_eq!(
        ok("var s = Symbol('s'); var seen = null;\
            var p = new Proxy({}, { deleteProperty: function (t, k) { seen = k; return true; } });\
            delete p[s]; seen === s"),
        "true"
    );
}
