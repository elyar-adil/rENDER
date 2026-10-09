//! Behaviour of the `BigInt` primitive (ECMA-262 6.1.6.2 and 21.2): literals,
//! operators, conversions, comparisons and the `BigInt` built-in. Each case is
//! a complete script evaluated in a fresh realm.

use crate::JsErrorKind;
use crate::runtime::JsRuntime;
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

/// The `name` of the error a snippet throws, or `none` when it completes.
fn thrown_name(source: &str) -> String {
    ok(&format!(
        "var r = 'none'; try {{ {source}; }} catch (e) {{ r = e.name; }} r"
    ))
}

#[test]
fn bigint_literals_cover_every_radix_separator_and_zero() {
    assert_eq!(ok("typeof 1n"), "bigint");
    assert_eq!(ok("String(0n)"), "0");
    assert_eq!(ok("String(0x1fn)"), "31");
    assert_eq!(ok("String(0XFFn)"), "255");
    assert_eq!(ok("String(0o17n)"), "15");
    assert_eq!(ok("String(0b101n)"), "5");
    assert_eq!(ok("String(1_000_000n)"), "1000000");
    assert_eq!(ok("String(0xf_fn)"), "255");
    assert_eq!(
        ok("String(123456789012345678901234567890n)"),
        "123456789012345678901234567890"
    );
    assert_eq!(ok("(1n).toString(2)"), "1");
}

#[test]
fn malformed_bigint_literals_are_syntax_errors() {
    for source in [
        "1.5n", "1e3n", ".5n", "01n", "00n", "07n", "08n", "09n", "012348n", "0_1n", "1__0n",
        "1_n", "0x_1n", "0xn", "0b2n", "1nx", "1n1", "1nn", "0x1nn", "1n_",
    ] {
        let error = eval(source).expect_err("not a BigInt literal");
        assert_eq!(error.kind(), JsErrorKind::Syntax, "{source}");
    }
}

#[test]
fn bigint_arithmetic_is_exact_and_truncates_toward_zero() {
    assert_eq!(ok("String(2n + 3n)"), "5");
    assert_eq!(ok("String(5n - 7n)"), "-2");
    assert_eq!(ok("String(3n * -4n)"), "-12");
    assert_eq!(ok("String(-7n / 2n)"), "-3");
    assert_eq!(ok("String(-7n % 2n)"), "-1");
    assert_eq!(ok("String(7n % -2n)"), "1");
    assert_eq!(ok("String(2n ** 100n)"), "1267650600228229401496703205376");
    assert_eq!(ok("String((-2n) ** 3n)"), "-8");
    assert_eq!(ok("String(0n ** 0n)"), "1");
    assert_eq!(
        ok("String(4294967295n * 4294967295n + 1n)"),
        "18446744065119617026"
    );
    assert_eq!(ok("String(-(2n ** 64n))"), "-18446744073709551616");
}

#[test]
fn bigint_bitwise_and_shift_operators_use_two_complement() {
    assert_eq!(ok("String(-5n & 3n)"), "3");
    assert_eq!(ok("String(5n | -8n)"), "-3");
    assert_eq!(ok("String(5n ^ 3n)"), "6");
    assert_eq!(ok("String(~5n)"), "-6");
    assert_eq!(ok("String(1n << 70n)"), "1180591620717411303424");
    assert_eq!(ok("String(-9n >> 1n)"), "-5");
    assert_eq!(ok("String(1n << -1n)"), "0");
    assert_eq!(ok("String(8n >> -2n)"), "32");
    assert_eq!(ok("String(-1n >> 1000000n)"), "-1");
}

#[test]
fn bigint_operators_reject_mixed_operands_and_unsigned_shift() {
    assert_eq!(thrown_name("1n + 1"), "TypeError");
    assert_eq!(thrown_name("1 - 1n"), "TypeError");
    assert_eq!(thrown_name("1n * 1.5"), "TypeError");
    assert_eq!(thrown_name("1n & 1"), "TypeError");
    assert_eq!(thrown_name("1n >>> 0n"), "TypeError");
    assert_eq!(thrown_name("+1n"), "TypeError");
    assert_eq!(thrown_name("1n / 0n"), "RangeError");
    assert_eq!(thrown_name("1n % 0n"), "RangeError");
    assert_eq!(thrown_name("2n ** -1n"), "RangeError");
    assert_eq!(thrown_name("1n << 9007199254740991n"), "RangeError");
    assert_eq!(thrown_name("Math.max(1n)"), "TypeError");
    assert_eq!(thrown_name("isNaN(1n)"), "TypeError");
    // String concatenation is the one mixed case that succeeds.
    assert_eq!(ok("1n + 'x'"), "1x");
    assert_eq!(ok("'' + 1n"), "1");
}

#[test]
fn number_of_a_bigint_converts_but_implicit_to_number_does_not() {
    assert_eq!(ok("Number(1n)"), "1");
    assert_eq!(ok("Number(-7n)"), "-7");
    assert_eq!(ok("Number(2n ** 64n)"), "18446744073709552000");
    assert_eq!(ok("new Number(3n).valueOf() === 3"), "true");
    assert_eq!(ok("Number(2n ** 1100n)"), "Infinity");
}

#[test]
fn bigint_relational_operators_compare_exactly_across_types() {
    assert_eq!(ok("1n < 2"), "true");
    assert_eq!(ok("2n > 1.5"), "true");
    assert_eq!(ok("1n < 1.5"), "true");
    assert_eq!(ok("2n <= 2"), "true");
    assert_eq!(ok("3n >= 4"), "false");
    assert_eq!(ok("1n < '2'"), "true");
    assert_eq!(ok("'10' > 9n"), "true");
    assert_eq!(ok("1n < 'x'"), "false");
    assert_eq!(ok("1n <= 'x'"), "false");
    assert_eq!(ok("1n >= 'x'"), "false");
    assert_eq!(ok("1n < NaN"), "false");
    assert_eq!(ok("1n > NaN"), "false");
    assert_eq!(ok("1n < Infinity"), "true");
    assert_eq!(ok("1n > -Infinity"), "true");
    assert_eq!(ok("(2n ** 70n) > 1e20"), "true");
    assert_eq!(ok("9007199254740993n > 9007199254740992"), "true");
    assert_eq!(ok("1n < true"), "false");
    assert_eq!(ok("2n > true"), "true");
}

#[test]
fn bigint_equality_is_mathematical_and_strict_equality_is_typed() {
    assert_eq!(ok("1n == 1"), "true");
    assert_eq!(ok("1n == 1.5"), "false");
    assert_eq!(ok("1n == NaN"), "false");
    assert_eq!(ok("1n == Infinity"), "false");
    assert_eq!(ok("1n == '1'"), "true");
    assert_eq!(ok("1n == ' 1 '"), "true");
    assert_eq!(ok("1n == 'x'"), "false");
    assert_eq!(ok("1n == true"), "true");
    assert_eq!(ok("0n == false"), "true");
    assert_eq!(ok("1n == null"), "false");
    assert_eq!(ok("0n == undefined"), "false");
    assert_eq!(ok("1n == Object(1n)"), "true");
    assert_eq!(ok("1n != 2"), "true");
    assert_eq!(ok("1n === 1"), "false");
    assert_eq!(ok("1n === 1n"), "true");
    assert_eq!(ok("0n === -0n"), "true");
    assert_eq!(ok("Object.is(0n, -0n)"), "true");
    assert_eq!(ok("1n !== 1n"), "false");
    assert_eq!(ok("2n ** 64n === 2n ** 64n"), "true");
}

#[test]
fn bigint_truthiness_and_typeof() {
    assert_eq!(ok("!!0n"), "false");
    assert_eq!(ok("!!1n"), "true");
    assert_eq!(ok("!!-1n"), "true");
    assert_eq!(ok("0n ? 'yes' : 'no'"), "no");
    assert_eq!(ok("typeof BigInt"), "function");
    assert_eq!(ok("typeof Object(1n)"), "object");
    assert_eq!(ok("typeof Object(1n).valueOf()"), "bigint");
    assert_eq!(ok("typeof (1n).valueOf()"), "bigint");
    assert_eq!(ok("Object.prototype.toString.call(1n)"), "[object BigInt]");
}

#[test]
fn bigint_function_converts_values_with_to_bigint() {
    assert_eq!(ok("String(BigInt(10))"), "10");
    assert_eq!(ok("String(BigInt(-0))"), "0");
    assert_eq!(ok("String(BigInt(2 ** 60))"), "1152921504606846976");
    assert_eq!(ok("String(BigInt('0x10'))"), "16");
    assert_eq!(ok("String(BigInt(' 12 '))"), "12");
    assert_eq!(ok("String(BigInt('-7'))"), "-7");
    assert_eq!(ok("String(BigInt(''))"), "0");
    assert_eq!(ok("String(BigInt(true))"), "1");
    assert_eq!(ok("String(BigInt(false))"), "0");
    assert_eq!(ok("String(BigInt(1n))"), "1");
    assert_eq!(ok("String(BigInt({valueOf() { return 5; }}))"), "5");
    assert_eq!(ok("String(BigInt(Object(7n)))"), "7");
    assert_eq!(thrown_name("BigInt(1.5)"), "RangeError");
    assert_eq!(thrown_name("BigInt(NaN)"), "RangeError");
    assert_eq!(thrown_name("BigInt(Infinity)"), "RangeError");
    assert_eq!(thrown_name("BigInt('1.5')"), "SyntaxError");
    assert_eq!(thrown_name("BigInt('1e3')"), "SyntaxError");
    assert_eq!(thrown_name("BigInt('1_000')"), "SyntaxError");
    assert_eq!(thrown_name("BigInt('-0x10')"), "SyntaxError");
    assert_eq!(thrown_name("BigInt(undefined)"), "TypeError");
    assert_eq!(thrown_name("BigInt(null)"), "TypeError");
    assert_eq!(thrown_name("BigInt(Symbol())"), "TypeError");
    assert_eq!(thrown_name("new BigInt(1)"), "TypeError");
}

#[test]
fn bigint_as_int_n_and_as_uint_n_wrap_to_the_width() {
    assert_eq!(ok("String(BigInt.asIntN(8, 255n))"), "-1");
    assert_eq!(ok("String(BigInt.asIntN(8, 127n))"), "127");
    assert_eq!(ok("String(BigInt.asIntN(8, -129n))"), "127");
    assert_eq!(ok("String(BigInt.asUintN(8, -1n))"), "255");
    assert_eq!(ok("String(BigInt.asUintN(8, 256n))"), "0");
    assert_eq!(ok("String(BigInt.asIntN(0, 5n))"), "0");
    assert_eq!(ok("String(BigInt.asUintN(0, 5n))"), "0");
    assert_eq!(
        ok("String(BigInt.asIntN(64, 2n ** 63n))"),
        "-9223372036854775808"
    );
    assert_eq!(ok("String(BigInt.asUintN(64, 2n ** 64n + 1n))"), "1");
    assert_eq!(ok("String(BigInt.asIntN(9007199254740991, -1n))"), "-1");
    assert_eq!(thrown_name("BigInt.asUintN(-1, 1n)"), "RangeError");
    assert_eq!(thrown_name("BigInt.asIntN(2, 1)"), "TypeError");
}

#[test]
fn bigint_prototype_to_string_and_value_of_check_their_receiver() {
    assert_eq!(ok("(255n).toString(16)"), "ff");
    assert_eq!(ok("(255n).toString()"), "255");
    assert_eq!(ok("(-255n).toString(2)"), "-11111111");
    assert_eq!(ok("(35n).toString(36)"), "z");
    assert_eq!(ok("(10n).toString(undefined)"), "10");
    assert_eq!(ok("Object(1n).valueOf() === 1n"), "true");
    assert_eq!(thrown_name("(1n).toString(37)"), "RangeError");
    assert_eq!(thrown_name("(1n).toString(1)"), "RangeError");
    assert_eq!(
        thrown_name("BigInt.prototype.toString.call(1)"),
        "TypeError"
    );
    assert_eq!(
        thrown_name("BigInt.prototype.valueOf.call('1')"),
        "TypeError"
    );
    assert_eq!(ok("BigInt.prototype[Symbol.toStringTag]"), "BigInt");
}

#[test]
fn bigint_increment_and_decrement_step_by_one_bigint() {
    assert_eq!(ok("var x = 1n; x++; String(x)"), "2");
    assert_eq!(ok("var x = 1n; ++x; String(x)"), "2");
    assert_eq!(ok("var x = 5n; String(x--)"), "5");
    assert_eq!(ok("var x = 5n; String(--x)"), "4");
    assert_eq!(ok("var x = -1n; x++; String(x)"), "0");
    // A postfix expression answers the ToNumeric of the old value.
    assert_eq!(ok("var x = '5'; String(x++)"), "5");
}

#[test]
fn bigint_property_keys_and_template_text_are_decimal() {
    assert_eq!(ok("`${10n}`"), "10");
    assert_eq!(ok("[1n, 2n].join('-')"), "1-2");
    assert_eq!(ok("var o = {1n: 'a'}; o[1]"), "a");
    assert_eq!(ok("var o = {}; o[2n] = 'b'; o['2']"), "b");
    assert_eq!(ok("var o = {3: 'c'}; String(3n in o)"), "true");
    assert_eq!(ok("String(-0n)"), "0");
}

#[test]
fn bigint_json_stringify_throws_and_wrapper_arithmetic_unwraps() {
    assert_eq!(thrown_name("JSON.stringify(1n)"), "TypeError");
    assert_eq!(thrown_name("JSON.stringify({a: 1n})"), "TypeError");
    assert_eq!(ok("String(Object(1n) + 1n)"), "2");
    assert_eq!(ok("Object(3n) * 2n === 6n"), "true");
}

#[test]
fn to_primitive_requires_a_callable_exotic_method_or_none() {
    assert_eq!(thrown_name("({[Symbol.toPrimitive]: 1}) + 0n"), "TypeError");
    assert_eq!(thrown_name("+({[Symbol.toPrimitive]: 'x'})"), "TypeError");
    assert_eq!(
        ok("String(({[Symbol.toPrimitive]: undefined, valueOf() { return 4; }}) + 1)"),
        "5"
    );
    assert_eq!(
        ok("String(({[Symbol.toPrimitive]: null, valueOf() { return 4; }}) * 2)"),
        "8"
    );
}

#[test]
fn template_substitutions_use_the_string_hint_and_reject_symbols() {
    assert_eq!(
        ok("var o = {toString() { return 'a'; }, valueOf() { return 'b'; }}; `${o}`"),
        "a"
    );
    assert_eq!(
        ok("var o = {toString() { return 'a'; }, valueOf() { return 'b'; }}; '' + o"),
        "b"
    );
    assert_eq!(thrown_name("`${Symbol()}`"), "TypeError");
    assert_eq!(thrown_name("'' + Symbol()"), "TypeError");
    assert_eq!(thrown_name("Symbol() + 'x'"), "TypeError");
}

#[test]
fn bigint_is_a_constructor_that_refuses_to_construct() {
    assert_eq!(thrown_name("Reflect.construct(BigInt, [])"), "TypeError");
    assert_eq!(thrown_name("new BigInt()"), "TypeError");
    assert_eq!(
        thrown_name("class X extends BigInt {}; new X()"),
        "TypeError"
    );
}

#[test]
fn bigint_built_in_properties_have_spec_lengths_and_attributes() {
    assert_eq!(ok("BigInt.length"), "1");
    assert_eq!(ok("BigInt.asIntN.length"), "2");
    assert_eq!(ok("BigInt.asUintN.length"), "2");
    assert_eq!(ok("BigInt.prototype.toString.length"), "0");
    assert_eq!(
        ok("Object.getOwnPropertyDescriptor(BigInt.prototype, Symbol.toStringTag).writable"),
        "false"
    );
    assert_eq!(
        ok("Object.getOwnPropertyDescriptor(BigInt, 'prototype').writable"),
        "false"
    );
}

#[test]
fn exponentiation_of_one_and_negative_one_by_infinity_is_nan() {
    assert_eq!(ok("String((-1) ** Infinity)"), "NaN");
    assert_eq!(ok("String((1) ** -Infinity)"), "NaN");
    assert_eq!(ok("String(1 ** NaN)"), "NaN");
    assert_eq!(ok("String((-2) ** Infinity)"), "Infinity");
}

#[test]
fn bigint_callee_and_object_spread_follow_the_primitive_rules() {
    assert_eq!(thrown_name("1n()"), "TypeError");
    assert_eq!(ok("var o = {...1n}; String(Object.keys(o).length)"), "0");
    assert_eq!(ok("Object.assign({a: 1}, 2n).a"), "1");
    assert_eq!(ok("Object.keys(Object(1n)).length"), "0");
}
