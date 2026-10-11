//! Direct and indirect eval (ECMA-262 19.2.1): the calling context a direct eval
//! inherits, the declarations it instantiates, and the completion it returns.

use crate::{JsErrorKind, JsRuntime, RuntimeLimits};
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

fn kind_of(source: &str) -> JsErrorKind {
    eval(source).expect_err("the source should throw").kind()
}

#[test]
fn global_eval_is_the_intrinsic_with_its_standard_length_and_name() {
    // 19.2.1: `eval` is a writable, configurable, non-enumerable global whose
    // length is 1 and whose name is "eval".
    assert_eq!(
        ok(
            "var d = Object.getOwnPropertyDescriptor(globalThis, 'eval'); [typeof eval, eval.length, eval.name, d.writable, d.enumerable, d.configurable].join()"
        ),
        "function,1,eval,true,false,true"
    );
}

#[test]
fn eval_returns_a_non_string_argument_unchanged() {
    // 19.2.1.1 step 2: a value that is not a String is the result.
    assert_eq!(ok("eval(5)"), "5");
    assert_eq!(ok("var o = {}; eval(o) === o"), "true");
    assert_eq!(ok("eval()"), "undefined");
}

#[test]
fn indirect_eval_returns_the_completion_value_of_its_script() {
    assert_eq!(ok("(0, eval)('1; 2')"), "2");
    assert_eq!(ok("(0, eval)('if (true) { 3 } else { 4 }')"), "3");
}

#[test]
fn declarations_complete_empty_so_the_previous_value_stands() {
    // 14.3.1.2, 15.2.6, 14.6.2: var, let, class and function declarations are empty.
    assert_eq!(ok("eval('1; var a = 2')"), "1");
    assert_eq!(ok("eval('3; function f() {}')"), "3");
    assert_eq!(ok("eval('4; let q = 5')"), "4");
    assert_eq!(ok("eval('5; class C {}')"), "5");
    assert_eq!(ok("eval('6; {}')"), "6");
}

#[test]
fn direct_eval_sees_the_caller_scope_and_its_arguments() {
    assert_eq!(
        ok("function f() { var a = 1; return eval('a + 1'); } f()"),
        "2"
    );
    assert_eq!(
        ok("function a() { return eval('arguments.length'); } a(1, 2)"),
        "2"
    );
    assert_eq!(
        ok("var r = 9; function g() { var r = 1; return (eval)('r'); } g()"),
        "1"
    );
}

#[test]
fn parenthesized_eval_is_still_direct() {
    assert_eq!(
        ok("function f() { var q = 9; return (eval)('q'); } f()"),
        "9"
    );
    assert_eq!(
        ok("function f() { var q = 9; return (0, eval)('typeof q'); } f()"),
        "undefined"
    );
}

#[test]
fn aliased_and_optional_eval_are_indirect() {
    assert_eq!(
        ok("var ev = eval; function f() { var inner = 1; return ev('typeof inner'); } f()"),
        "undefined"
    );
    assert_eq!(
        ok("function f() { var inner = 1; return eval?.('typeof inner'); } f()"),
        "undefined"
    );
}

#[test]
fn sloppy_direct_eval_var_lands_in_the_function_scope() {
    assert_eq!(ok("function f() { eval('var z = 4'); return z; } f()"), "4");
    assert_eq!(
        ok("function h() { eval('var hv = 1'); } h(); typeof hv"),
        "undefined"
    );
    assert_eq!(
        ok("function f() { eval('var arguments = 5'); return arguments; } f()"),
        "5"
    );
}

#[test]
fn sloppy_direct_eval_var_at_top_level_is_global() {
    assert_eq!(ok("eval('var topv = 1'); topv"), "1");
    assert_eq!(
        ok("eval('var topf = function () {}'); typeof topf"),
        "function"
    );
}

#[test]
fn indirect_eval_declares_globals_and_sees_no_caller_scope() {
    assert_eq!(ok("(0, eval)('var gv = 6'); gv"), "6");
    assert_eq!(
        ok("function f() { var local = 1; return (0, eval)('typeof local'); } f()"),
        "undefined"
    );
}

#[test]
fn this_is_the_caller_this_for_direct_and_the_global_for_indirect() {
    assert_eq!(
        ok("var obj = { m() { return eval('this'); } }; obj.m() === obj"),
        "true"
    );
    assert_eq!(
        ok("var obj2 = { m() { return (0, eval)('this'); } }; obj2.m() === this"),
        "true"
    );
}

#[test]
fn strict_eval_keeps_its_declarations_to_itself() {
    // 19.2.1.1 step 12.a: strict eval code has a variable environment of its own.
    assert_eq!(
        ok("function f() { 'use strict'; eval('var s = 1'); return typeof s; } f()"),
        "undefined"
    );
    assert_eq!(
        ok("eval('\"use strict\"; var t = 1'); typeof t"),
        "undefined"
    );
    assert_eq!(
        ok("eval('\"use strict\"; function sf() {} '); typeof sf"),
        "undefined"
    );
}

#[test]
fn strict_caller_makes_direct_eval_strict() {
    // strictCaller: the call's source is strict, so its eval code is strict too.
    assert_eq!(
        ok("'use strict'; eval('var sv = 1'); typeof sv"),
        "undefined"
    );
    assert_eq!(
        ok("class K { m() { return eval('var ck = 1; typeof ck'); } } new K().m()"),
        "number"
    );
    assert_eq!(
        ok("class K2 { m() { eval('var ck2 = 1'); return typeof ck2; } } new K2().m()"),
        "undefined"
    );
}

#[test]
fn indirect_eval_is_sloppy_unless_its_own_source_is_strict() {
    assert_eq!(ok("(0, eval)('var iv = 1'); typeof iv"), "number");
    assert_eq!(
        ok("(0, eval)('\"use strict\"; var iv2 = 1'); typeof iv2"),
        "undefined"
    );
}

#[test]
fn eval_lexical_declarations_stay_in_the_eval_code() {
    assert_eq!(ok("eval('let lx = 1'); typeof lx"), "undefined");
    assert_eq!(kind_of("eval('lt; let lt = 1')"), JsErrorKind::Reference);
}

#[test]
fn eval_declares_global_and_function_scoped_functions() {
    assert_eq!(ok("eval('function ef() { return 9; }'); ef()"), "9");
    assert_eq!(
        ok("function k() { eval('function kk() { return 2; }'); return kk(); } k()"),
        "2"
    );
    assert_eq!(
        ok("eval('function dupf() { return 1; } function dupf() { return 2; }'); dupf()"),
        "2"
    );
}

#[test]
fn eval_var_conflicting_with_an_enclosing_lexical_declaration_is_a_syntax_error() {
    assert_eq!(
        kind_of("function f() { let c; eval('var c'); } f()"),
        JsErrorKind::Syntax
    );
    assert_eq!(kind_of("let gc = 1; eval('var gc')"), JsErrorKind::Syntax);
    assert_eq!(kind_of("{ let bb; eval('var bb'); }"), JsErrorKind::Syntax);
    assert_eq!(
        kind_of(
            "function g() { 'use strict'; eval('var d'); } g(); let d2 = 1; eval('function d2(){}')"
        ),
        JsErrorKind::Syntax
    );
}

#[test]
fn catch_parameter_may_be_var_declared_by_eval() {
    // Annex B.3.4: a simple catch parameter does not conflict with a var, and the
    // initializer assigns the catch parameter.
    assert_eq!(
        ok("var r; try { throw 1; } catch (e) { eval('var e = 2'); r = e; } r"),
        "2"
    );
    assert_eq!(
        ok("try { throw 1; } catch (e) { eval('var e = 2'); } typeof e"),
        "undefined"
    );
}

#[test]
fn global_function_that_cannot_be_declared_is_a_type_error() {
    assert_eq!(kind_of("eval('function NaN() {}')"), JsErrorKind::Type);
}

#[test]
fn global_function_declaration_keeps_a_non_configurable_property_attributes() {
    assert_eq!(
        ok(
            "Object.defineProperty(globalThis, 'nc', { value: 1, writable: true, enumerable: true, configurable: false }); eval('function nc() { return 2; }'); var d = Object.getOwnPropertyDescriptor(globalThis, 'nc'); [typeof nc, d.writable, d.enumerable, d.configurable].join()"
        ),
        "function,true,true,false"
    );
}

#[test]
fn new_target_is_allowed_in_functions_only() {
    assert_eq!(
        ok("function G() { this.t = eval('new.target') === G; } new G().t"),
        "true"
    );
    assert_eq!(kind_of("eval('new.target')"), JsErrorKind::Syntax);
    assert_eq!(kind_of("(() => eval('new.target'))()"), JsErrorKind::Syntax);
}

#[test]
fn super_property_is_allowed_in_methods_and_their_arrows() {
    assert_eq!(
        ok(
            "var o = { m() { return eval('super.x'); } }; Object.setPrototypeOf(o, { x: 7 }); o.m()"
        ),
        "7"
    );
    assert_eq!(
        ok(
            "var o2 = { m() { return (() => eval('super.x'))(); } }; Object.setPrototypeOf(o2, { x: 8 }); o2.m()"
        ),
        "8"
    );
    assert_eq!(kind_of("eval('super.x')"), JsErrorKind::Syntax);
    assert_eq!(
        kind_of("var f = () => eval('super.x'); f();"),
        JsErrorKind::Syntax
    );
}

#[test]
fn super_call_is_allowed_in_derived_constructors_only() {
    assert_eq!(
        ok(
            "class A {} class B extends A { constructor() { eval('super()'); } } new B() instanceof B"
        ),
        "true"
    );
    assert_eq!(
        kind_of("class A2 {} class B2 extends A2 { m() { eval('super()'); } } new B2().m()"),
        JsErrorKind::Syntax
    );
    assert_eq!(
        kind_of("class A3 { constructor() { eval('super()'); } } new A3()"),
        JsErrorKind::Syntax
    );
}

#[test]
fn arguments_is_an_early_error_in_eval_inside_a_class_field_initializer() {
    assert_eq!(
        kind_of("class C { x = eval('arguments'); } new C()"),
        JsErrorKind::Syntax
    );
    assert_eq!(
        ok("class D { x = eval('typeof this'); } new D().x"),
        "object"
    );
}

#[test]
fn private_names_resolve_through_the_enclosing_class() {
    assert_eq!(
        ok("class P { #p = 3; m() { return eval('this.#p'); } } new P().m()"),
        "3"
    );
    assert_eq!(kind_of("eval('this.#nope')"), JsErrorKind::Syntax);
}

#[test]
fn block_function_in_sloppy_eval_is_var_scoped() {
    // Annex B.3.2.3: a block-level function is also a var of the eval's variable
    // environment, set when its declaration is evaluated.
    assert_eq!(ok("eval('{ function bf() { return 5; } }'); bf()"), "5");
    assert_eq!(
        ok("function w() { eval('{ function wf() { return 3; } }'); return wf(); } w()"),
        "3"
    );
}

#[test]
fn block_function_in_strict_eval_is_not_var_scoped() {
    assert_eq!(
        ok("eval('\"use strict\"; { function sf2() {} }'); typeof sf2"),
        "undefined"
    );
    assert_eq!(
        ok("eval('switch (1) { case 1: function sw() {} }'); typeof sw"),
        "function"
    );
}

#[test]
fn if_clause_function_is_a_block_function_in_sloppy_code() {
    // Annex B.3.4: a plain function declaration as an `if` clause is the sole
    // statement of a block, so eval code var-scopes it like any block function.
    assert_eq!(
        ok("eval('if (true) function ifb() { return 7; }'); ifb()"),
        "7"
    );
    assert_eq!(
        ok("eval('if (false) function ifc() {} else function ifd() {}'); typeof ifd"),
        "function"
    );
    assert_eq!(
        ok("eval('if (false) function ife() {}'); typeof ife"),
        "undefined"
    );
    assert_eq!(
        kind_of("eval('\"use strict\"; if (true) function ifs() {}')"),
        JsErrorKind::Syntax
    );
}

#[test]
fn block_function_is_not_hoisted_past_a_lexical_binding() {
    // `let bx` in the enclosing block makes a `var bx` an early error, so the
    // block function keeps its block scope.
    assert_eq!(
        ok("{ let bx = 1; eval('{ function bx() {} }'); } typeof bx"),
        "undefined"
    );
}

#[test]
fn eval_in_parameter_expressions_cannot_var_declare_a_parameter_scope_name() {
    // A parameter, or the implicit `arguments` of a non-arrow function, is bound
    // while the parameter expressions run.
    assert_eq!(
        kind_of("function f(a = eval('var a = 42')) {} f()"),
        JsErrorKind::Syntax
    );
    assert_eq!(
        kind_of("function f(p = eval('var arguments')) {} f()"),
        JsErrorKind::Syntax
    );
    assert_eq!(
        kind_of("function f(p = eval('var arguments'), arguments) {} f()"),
        JsErrorKind::Syntax
    );
    assert_eq!(
        kind_of("var f = (a = eval('var a')) => {}; f()"),
        JsErrorKind::Syntax
    );
    // An arrow has no `arguments` of its own, so its parameter list binds none.
    assert_eq!(
        ok("var a1 = (p = eval(\"var arguments = 'param'\")) => arguments; a1()"),
        "param"
    );
    // A name no parameter binds is an ordinary var of the function.
    assert_eq!(
        ok("function g(p = eval('var z = 3')) { return z; } g()"),
        "3"
    );
}

#[test]
fn eval_created_local_bindings_may_be_deleted_and_declared_ones_may_not() {
    // 19.2.1.2 step 16.b: a binding eval code creates is deletable; one the
    // function already declares is not.
    assert_eq!(
        ok("(function () { eval('var dv = 1'); return [delete dv, typeof dv].join(); })()"),
        "true,undefined"
    );
    assert_eq!(
        ok("(function () { eval('function df() {}'); return [delete df, typeof df].join(); })()"),
        "true,undefined"
    );
    assert_eq!(
        ok("(function () { var nv = 1; return delete nv; })()"),
        "false"
    );
}

#[test]
fn eval_var_of_an_existing_local_keeps_its_value() {
    // 19.2.1.2 step 16.b: a var the function already binds is not created again.
    assert_eq!(
        ok("var initial; (function () { var x = 44443; eval('initial = x; var x;'); })(); initial"),
        "44443"
    );
}

#[test]
fn script_global_var_is_non_configurable_and_eval_redeclaration_keeps_it() {
    // 9.1.1.4.17 CreateGlobalVarBinding: a script's `var` is not configurable, and
    // an eval `var` of the same name only assigns it.
    assert_eq!(
        ok(
            "var initial; var xx = 23; eval('initial = xx; var xx = 45;'); [initial, xx, Object.getOwnPropertyDescriptor(this, 'xx').configurable].join()"
        ),
        "23,45,false"
    );
}

#[test]
fn eval_recursion_is_bounded_by_the_call_depth_limit() {
    // Each direct eval counts as a call, so runaway eval recursion ends in a
    // RangeError at the depth limit. A small limit, on a thread with room for
    // the debug build's large interpreter frames, keeps the test quick.
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            let limits = RuntimeLimits {
                max_call_depth: 64,
                ..RuntimeLimits::default()
            };
            let mut dom = parse_document("<!doctype html><html><body></body></html>").dom;
            let mut runtime = JsRuntime::with_limits(&dom, limits);
            let outcome = runtime
                .execute(
                    &mut dom,
                    "var r; try { (function f() { eval('f()'); })(); } catch (e) { r = e instanceof RangeError; } r",
                )
                .expect("the recursion is caught by the script");
            assert_eq!(outcome.value.to_js_string(), "true");
        })
        .expect("spawn test thread")
        .join()
        .expect("recursion test thread");
}

#[test]
fn eval_is_not_a_constructor() {
    assert_eq!(kind_of("new eval('1')"), JsErrorKind::Type);
}

#[test]
fn eval_parse_errors_are_catchable_syntax_errors() {
    assert_eq!(
        ok("var r; try { eval('var'); } catch (e) { r = e instanceof SyntaxError; } r"),
        "true"
    );
    assert_eq!(kind_of("eval('return 1')"), JsErrorKind::Syntax);
    assert_eq!(kind_of("eval('break')"), JsErrorKind::Syntax);
}
