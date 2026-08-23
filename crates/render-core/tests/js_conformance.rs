//! Executable conformance inventory for rENDER's deliberately small JS slice.
//!
//! These cases are local reductions inspired by ECMAScript semantics. They are
//! not imported test262 cases and must not be reported as a test262 pass rate.

use render_core::html::parse_document;
use render_core::js::{JsErrorKind, JsRuntime, JsValue, RuntimeLimits};

#[derive(Clone, Copy)]
enum Expected {
    Value(&'static str, JsValueRef),
    Error(JsErrorKind),
}

#[derive(Clone, Copy)]
enum JsValueRef {
    Number(f64),
    String(&'static str),
    Boolean(bool),
}

struct Case {
    id: &'static str,
    source: &'static str,
    expected: Expected,
}

const LANGUAGE_CASES: &[Case] = &[
    Case {
        id: "expressions/operator-precedence",
        source: "const result = 1 + 2 * 3;",
        expected: Expected::Value("result", JsValueRef::Number(7.0)),
    },
    Case {
        id: "expressions/string-concatenation",
        source: "const result = 'render-' + 4;",
        expected: Expected::Value("result", JsValueRef::String("render-4")),
    },
    Case {
        id: "expressions/logical-short-circuit",
        source: "let touched = false; false && (touched = true); const result = touched;",
        expected: Expected::Value("result", JsValueRef::Boolean(false)),
    },
    Case {
        id: "statements/if-else",
        source: "let result = 'bad'; if (3 > 2) { result = 'ok'; } else { result = 'bad'; }",
        expected: Expected::Value("result", JsValueRef::String("ok")),
    },
    Case {
        id: "statements/while",
        source: "let n = 0; while (n < 4) { n = n + 1; } const result = n;",
        expected: Expected::Value("result", JsValueRef::Number(4.0)),
    },
    Case {
        id: "statements/for",
        source: "let result = 0; for (let i = 0; i < 4; i = i + 1) { result = result + i; }",
        expected: Expected::Value("result", JsValueRef::Number(6.0)),
    },
    Case {
        id: "statements/for-break-continue",
        source: "let result = 0; for (let i = 0; i < 6; i = i + 1) { if (i === 2) { continue; } if (i === 5) { break; } result = result + i; }",
        expected: Expected::Value("result", JsValueRef::Number(8.0)),
    },
    Case {
        id: "statements/while-break-continue",
        source: "let i = 0; let result = 0; while (i < 5) { i = i + 1; if (i === 2) { continue; } if (i === 5) { break; } result = result + i; }",
        expected: Expected::Value("result", JsValueRef::Number(8.0)),
    },
    Case {
        id: "functions/declaration-call-return",
        source: "function add(a, b) { return a + b; } const result = add(20, 22);",
        expected: Expected::Value("result", JsValueRef::Number(42.0)),
    },
    Case {
        id: "functions/lexical-read",
        source: "const prefix = 'r'; function join(value) { return prefix + value; } const result = join('ENDER');",
        expected: Expected::Value("result", JsValueRef::String("rENDER")),
    },
    Case {
        id: "functions/shared-closure-environment",
        source: "function makeCounter() { let count = 0; function next() { count = count + 1; return count; } return next; } const counter = makeCounter(); counter(); const result = counter();",
        expected: Expected::Value("result", JsValueRef::Number(2.0)),
    },
    Case {
        id: "functions/independent-closure-environments",
        source: "function makeCounter() { let count = 0; function next() { count = count + 1; return count; } return next; } const first = makeCounter(); const second = makeCounter(); first(); first(); const result = second();",
        expected: Expected::Value("result", JsValueRef::Number(1.0)),
    },
    Case {
        id: "functions/recursive-binding-through-shared-environment",
        source: "function factorial(n) { if (n <= 1) { return 1; } return n * factorial(n - 1); } const result = factorial(5);",
        expected: Expected::Value("result", JsValueRef::Number(120.0)),
    },
    Case {
        id: "declarations/function-hoisting",
        source: "const result = answer(); function answer() { return 42; }",
        expected: Expected::Value("result", JsValueRef::Number(42.0)),
    },
    Case {
        id: "declarations/block-function-hoisting",
        source: "let result = 0; { result = answer(); function answer() { return 42; } }",
        expected: Expected::Value("result", JsValueRef::Number(42.0)),
    },
    Case {
        id: "declarations/var-hoisting",
        source: "const before = value; var value = 5; const result = before === undefined;",
        expected: Expected::Value("result", JsValueRef::Boolean(true)),
    },
    Case {
        id: "declarations/var-redeclaration",
        source: "var result = 1; var result = result + 1;",
        expected: Expected::Value("result", JsValueRef::Number(2.0)),
    },
    Case {
        id: "declarations/var-hoists-out-of-unreached-block",
        source: "if (false) { var hidden = 1; } const result = hidden === undefined;",
        expected: Expected::Value("result", JsValueRef::Boolean(true)),
    },
    Case {
        id: "functions/function-expression-closure",
        source: "function makeAdder(base) { return function(value) { return base + value; }; } const addTwo = makeAdder(2); const result = addTwo(40);",
        expected: Expected::Value("result", JsValueRef::Number(42.0)),
    },
    Case {
        id: "functions/named-function-expression-recursion",
        source: "const factorial = function inner(n) { if (n <= 1) { return 1; } return n * inner(n - 1); }; const result = factorial(5);",
        expected: Expected::Value("result", JsValueRef::Number(120.0)),
    },
    Case {
        id: "objects/object-literal-member-access",
        source: "const point = { x: 20, y: 22 }; const result = point.x + point['y'];",
        expected: Expected::Value("result", JsValueRef::Number(42.0)),
    },
    Case {
        id: "objects/method-call-this-binding",
        source: "const object = { value: 42, read: function() { return this.value; } }; const result = object.read();",
        expected: Expected::Value("result", JsValueRef::Number(42.0)),
    },
    Case {
        id: "objects/computed-method-call-this-binding",
        source: "const object = { value: 42, read: function() { return this.value; } }; const key = 'read'; const result = object[key]();",
        expected: Expected::Value("result", JsValueRef::Number(42.0)),
    },
    Case {
        id: "objects/user-constructor-initializes-this",
        source: "function Point(x, y) { this.x = x; this.y = y; } Point.prototype.sum = function() { return this.x + this.y; }; const point = new Point(20, 22); const result = point.sum();",
        expected: Expected::Value("result", JsValueRef::Number(42.0)),
    },
    Case {
        id: "objects/constructor-object-return-overrides-instance",
        source: "function Factory() { this.value = 1; return { value: 42 }; } const result = new Factory().value;",
        expected: Expected::Value("result", JsValueRef::Number(42.0)),
    },
    Case {
        id: "objects/constructor-primitive-return-keeps-instance",
        source: "function Factory() { this.value = 42; return 1; } const result = new Factory().value;",
        expected: Expected::Value("result", JsValueRef::Number(42.0)),
    },
    Case {
        id: "objects/plain-call-this-is-global",
        source: "function read() { return this === undefined; } const result = read();",
        expected: Expected::Value("result", JsValueRef::Boolean(false)),
    },
    Case {
        id: "objects/computed-member-assignment",
        source: "const state = { value: 1 }; const key = 'value'; state[key] = 42; const result = state.value;",
        expected: Expected::Value("result", JsValueRef::Number(42.0)),
    },
    Case {
        id: "arrays/literal-index-and-length",
        source: "const values = [20, 22]; const result = values[0] + values[1] + values.length;",
        expected: Expected::Value("result", JsValueRef::Number(44.0)),
    },
    Case {
        id: "arrays/is-array",
        source: "const result = Array.isArray([]) && !Array.isArray({});",
        expected: Expected::Value("result", JsValueRef::Boolean(true)),
    },
    Case {
        id: "arrays/push-updates-elements-and-length",
        source: "const values = [20]; values.push(22); const result = values[0] + values[1] + values.length;",
        expected: Expected::Value("result", JsValueRef::Number(44.0)),
    },
    Case {
        id: "arrays/pop-removes-the-last-element",
        source: "const values = [20, 22]; const last = values.pop(); const result = last + values.length;",
        expected: Expected::Value("result", JsValueRef::Number(23.0)),
    },
    Case {
        id: "math/common-unary-methods",
        source: "const result = Math.abs(-5) + Math.ceil(1.2) + Math.floor(3.8) + Math.round(4.5) + Math.sqrt(81);",
        expected: Expected::Value("result", JsValueRef::Number(24.0)),
    },
    Case {
        id: "math/min-max-variable-arity",
        source: "const result = Math.max(1, 9, 3) + Math.min(7, 2, 4);",
        expected: Expected::Value("result", JsValueRef::Number(11.0)),
    },
    Case {
        id: "exceptions/catch-thrown-value",
        source: "let result = 'bad'; try { throw 'caught'; } catch (error) { result = error; }",
        expected: Expected::Value("result", JsValueRef::String("caught")),
    },
    Case {
        id: "exceptions/propagates-through-function",
        source: "function fail() { throw 42; } let result = 0; try { fail(); } catch (error) { result = error; }",
        expected: Expected::Value("result", JsValueRef::Number(42.0)),
    },
    Case {
        id: "exceptions/catch-runtime-error-value",
        source: "let result = false; try { missingName; } catch (error) { result = true; }",
        expected: Expected::Value("result", JsValueRef::Boolean(true)),
    },
    Case {
        id: "exceptions/finally-runs-on-return",
        source: "let cleaned = false; function work() { try { return 42; } finally { cleaned = true; } } const result = work();",
        expected: Expected::Value("cleaned", JsValueRef::Boolean(true)),
    },
    Case {
        id: "exceptions/finally-overrides-return",
        source: "function work() { try { return 1; } finally { return 2; } } const result = work();",
        expected: Expected::Value("result", JsValueRef::Number(2.0)),
    },
    Case {
        id: "exceptions/finally-runs-before-uncaught-throw",
        source: "let cleaned = false; function work() { try { throw 'boom'; } finally { cleaned = true; } } try { work(); } catch (error) {} const result = cleaned;",
        expected: Expected::Value("result", JsValueRef::Boolean(true)),
    },
    Case {
        id: "errors/let-temporal-dead-zone",
        source: "const result = value; let value = 1;",
        expected: Expected::Error(JsErrorKind::Reference),
    },
    Case {
        id: "errors/lexical-redeclaration",
        source: "let value = 1; const value = 2;",
        expected: Expected::Error(JsErrorKind::Syntax),
    },
    Case {
        id: "errors/const-requires-initializer",
        source: "const missing;",
        expected: Expected::Error(JsErrorKind::Syntax),
    },
    Case {
        id: "errors/const-assignment",
        source: "const fixed = 1; fixed = 2;",
        expected: Expected::Error(JsErrorKind::Type),
    },
    Case {
        id: "errors/unknown-identifier",
        source: "missingName;",
        expected: Expected::Error(JsErrorKind::Reference),
    },
    Case {
        id: "errors/top-level-return",
        source: "return 1;",
        expected: Expected::Error(JsErrorKind::Syntax),
    },
];

#[test]
fn supported_language_inventory_is_executable_and_honest() {
    for case in LANGUAGE_CASES {
        let mut parsed = parse_document("<!doctype html><p id=target>before</p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let result = runtime.execute(&mut parsed.dom, case.source);
        match case.expected {
            Expected::Value(name, expected) => {
                result.unwrap_or_else(|error| panic!("{} failed: {error}", case.id));
                let actual = runtime
                    .realm()
                    .global(name)
                    .unwrap_or_else(|| panic!("{} did not define {name}", case.id));
                assert_value(case.id, actual, expected);
            }
            Expected::Error(kind) => {
                let Err(error) = result else {
                    panic!("{} unexpectedly passed", case.id);
                };
                assert_eq!(error.kind(), kind, "{} returned {error}", case.id);
            }
        }
    }
}

#[test]
fn function_control_flow_mutates_the_shared_dom() {
    let mut parsed = parse_document("<!doctype html><p id=target>before</p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    runtime
        .execute(
            &mut parsed.dom,
            "function update(value) { const node = document.getElementById('target'); if (value > 1) { node.textContent = 'after-' + value; } } update(2);",
        )
        .expect("function should update the same DOM arena");
    let target = find_by_id(&parsed.dom, "target");
    assert_eq!(text_content(&parsed.dom, target), "after-2");
}

#[test]
fn compound_values_respect_the_existing_heap_budget() {
    let mut parsed = parse_document("<!doctype html><p>bounded</p>");
    let mut runtime = JsRuntime::with_limits(
        &parsed.dom,
        RuntimeLimits {
            max_heap_objects: 3,
            ..RuntimeLimits::default()
        },
    );
    let error = runtime
        .execute(&mut parsed.dom, "const first = {}; const second = [];")
        .expect_err("compound literals must not bypass the realm object budget");
    assert_eq!(error.kind(), JsErrorKind::ResourceLimit);
}

#[test]
fn runaway_loop_is_stopped_by_the_existing_execution_budget() {
    let mut parsed = parse_document("<!doctype html><p>bounded</p>");
    let mut runtime = JsRuntime::with_limits(
        &parsed.dom,
        RuntimeLimits {
            max_execution_steps: 40,
            ..RuntimeLimits::default()
        },
    );
    let error = runtime
        .execute(&mut parsed.dom, "while (true) { 1 + 1; }")
        .expect_err("an infinite loop must not escape the runtime budget");
    assert_eq!(error.kind(), JsErrorKind::ResourceLimit);
}

#[allow(clippy::float_cmp)]
fn assert_value(id: &str, actual: JsValue, expected: JsValueRef) {
    match (actual, expected) {
        (JsValue::Number(actual), JsValueRef::Number(expected)) => {
            // These conformance reductions require exact ECMAScript Number
            // results rather than approximate numerical calculations.
            assert_eq!(actual, expected, "{id}");
        }
        (JsValue::String(actual), JsValueRef::String(expected)) => {
            assert_eq!(actual, expected, "{id}");
        }
        (JsValue::Boolean(actual), JsValueRef::Boolean(expected)) => {
            assert_eq!(actual, expected, "{id}");
        }
        (actual, _) => panic!("{id} produced unexpected value {actual:?}"),
    }
}

fn find_by_id(dom: &render_core::dom::Dom, id: &str) -> render_core::dom::NodeId {
    let mut pending = vec![dom.document()];
    while let Some(node) = pending.pop() {
        if dom.attribute(node, "id").ok().flatten() == Some(id) {
            return node;
        }
        pending.extend(dom.children(node).unwrap_or_default().iter().rev());
    }
    panic!("element #{id} not found");
}

fn text_content(dom: &render_core::dom::Dom, root: render_core::dom::NodeId) -> String {
    let mut result = String::new();
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        let Some(node) = dom.node(node) else {
            continue;
        };
        if let render_core::dom::NodeKind::Text(text) = node.kind() {
            result.push_str(text);
        }
        pending.extend(node.children().iter().rev());
    }
    result
}
