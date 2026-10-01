use crate::{CompiledScript, JsErrorKind, JsRuntime, RuntimeLimits};
use render_html::parse_document;
use std::collections::BTreeMap;

/// Declare every `(key, source)` pair (specifiers are the keys themselves),
/// evaluate `entry`, and return `log.join(",")`.
fn run(modules: &[(&str, &str)], entry: &str) -> Result<String, crate::JsError> {
    let mut dom = parse_document("<!doctype html><html><body></body></html>").dom;
    let mut runtime = JsRuntime::new(&dom);
    runtime.execute(&mut dom, "var log = [];").expect("log");
    let limits = RuntimeLimits::default();
    for (key, source) in modules {
        let script = CompiledScript::compile_module(source, &limits)?;
        let resolutions: BTreeMap<String, String> = script
            .module_requests()
            .iter()
            .map(|request| (request.clone(), request.clone()))
            .collect();
        runtime.declare_module(key, &script, resolutions)?;
    }
    runtime.evaluate_module(&mut dom, entry)?;
    Ok(runtime
        .execute(&mut dom, "log.join(',')")
        .expect("read log")
        .value
        .to_js_string())
}

#[test]
fn named_exports_are_live_bindings() {
    let log = run(
        &[
            (
                "counter.js",
                "export let count = 0; export function bump() { count += 1; }",
            ),
            (
                "main.js",
                "import { count, bump } from 'counter.js'; log.push(count); bump(); bump(); log.push(count);",
            ),
        ],
        "main.js",
    )
    .expect("runs");
    assert_eq!(log, "0,2");
}

#[test]
fn export_declarations_define_their_bindings() {
    let log = run(
        &[
            (
                "lib.js",
                "export const a = 1, b = 2; export var c = 3; export class K { v() { return 4; } } \
                 export function f() { return 5; } export const { x, y: [z] } = { x: 6, y: [7] };",
            ),
            (
                "main.js",
                "import { a, b, c, K, f, x, z } from 'lib.js'; log.push(a, b, c, new K().v(), f(), x, z);",
            ),
        ],
        "main.js",
    )
    .expect("runs");
    assert_eq!(log, "1,2,3,4,5,6,7");
}

#[test]
fn default_exports_in_every_form() {
    let log = run(
        &[
            ("expr.js", "export default 40 + 2;"),
            ("fn.js", "export default function named() { return 'fn'; }"),
            ("anon.js", "export default function () { return 'anon'; }"),
            ("cls.js", "export default class { who() { return 'cls'; } }"),
            (
                "main.js",
                "import e from 'expr.js'; import f from 'fn.js'; import g from 'anon.js'; \
                 import C from 'cls.js'; log.push(e, f(), g(), new C().who());",
            ),
        ],
        "main.js",
    )
    .expect("runs");
    assert_eq!(log, "42,fn,anon,cls");
}

#[test]
fn renamed_imports_and_exports() {
    let log = run(
        &[
            ("lib.js", "const inner = 9; export { inner as outer, inner as default };"),
            (
                "main.js",
                "import d, { outer as o } from 'lib.js'; import { default as e } from 'lib.js'; log.push(d, o, e);",
            ),
        ],
        "main.js",
    )
    .expect("runs");
    assert_eq!(log, "9,9,9");
}

#[test]
fn reexports_and_star_exports() {
    let log = run(
        &[
            ("base.js", "export const one = 1; export const two = 2; export default 'dflt';"),
            (
                "mid.js",
                "export * from 'base.js'; export { one as uno } from 'base.js'; export const three = 3;",
            ),
            (
                "main.js",
                "import { one, two, uno, three } from 'mid.js'; import * as m from 'mid.js'; \
                 log.push(one, two, uno, three, typeof m.default, Object.keys(m).sort().join('|'));",
            ),
        ],
        "main.js",
    )
    .expect("runs");
    assert_eq!(log, "1,2,1,3,undefined,one|three|two|uno");
}

#[test]
fn namespace_import_and_namespace_reexport() {
    let log = run(
        &[
            (
                "lib.js",
                "export const v = 'v'; export function f() { return 'f'; }",
            ),
            ("ns.js", "export * as lib from 'lib.js';"),
            (
                "main.js",
                "import * as direct from 'lib.js'; import { lib } from 'ns.js'; \
                 log.push(direct.v, direct.f(), lib.v);",
            ),
        ],
        "main.js",
    )
    .expect("runs");
    assert_eq!(log, "v,f,v");
}

#[test]
fn dependencies_run_first_and_exactly_once() {
    let log = run(
        &[
            ("shared.js", "log.push('shared');"),
            ("a.js", "import 'shared.js'; log.push('a');"),
            ("b.js", "import 'shared.js'; log.push('b');"),
            ("main.js", "import 'a.js'; import 'b.js'; log.push('main');"),
        ],
        "main.js",
    )
    .expect("runs");
    assert_eq!(log, "shared,a,b,main");
}

#[test]
fn cyclic_imports_see_hoisted_functions() {
    let log = run(
        &[
            (
                "a.js",
                "import { fromB } from 'b.js'; export function fromA() { return 'A'; } log.push(fromB());",
            ),
            (
                "b.js",
                "import { fromA } from 'a.js'; export function fromB() { return 'B' + fromA(); }",
            ),
        ],
        "a.js",
    )
    .expect("runs");
    assert_eq!(log, "BA");
}

#[test]
fn import_meta_url_is_the_module_key() {
    let log = run(
        &[("https://example.test/m.js", "log.push(import.meta.url);")],
        "https://example.test/m.js",
    )
    .expect("runs");
    assert_eq!(log, "https://example.test/m.js");
}

#[test]
fn module_scope_does_not_leak_into_the_global_scope() {
    let log = run(
        &[(
            "m.js",
            "const hidden = 1; var alsoHidden = 2; log.push(typeof globalThis.hidden, typeof globalThis.alsoHidden, typeof this);",
        )],
        "m.js",
    )
    .expect("runs");
    assert_eq!(log, "undefined,undefined,undefined");
}

#[test]
fn missing_export_is_a_link_error_before_the_body_runs() {
    let error = run(
        &[
            ("lib.js", "export const present = 1;"),
            (
                "main.js",
                "log.push('ran'); import { absent } from 'lib.js';",
            ),
        ],
        "main.js",
    )
    .expect_err("link fails");
    assert_eq!(error.kind(), JsErrorKind::Syntax);
    assert!(error.message().contains("absent"), "{}", error.message());
}

#[test]
fn assigning_to_an_import_throws() {
    let error = run(
        &[
            ("lib.js", "export let n = 0;"),
            ("main.js", "import { n } from 'lib.js'; n = 1;"),
        ],
        "main.js",
    )
    .expect_err("assignment fails");
    assert_eq!(error.kind(), JsErrorKind::Type);
}

#[test]
fn a_failed_module_fails_every_importer() {
    let error = run(
        &[
            ("bad.js", "throw new Error('boom');"),
            ("main.js", "import 'bad.js'; log.push('unreachable');"),
        ],
        "main.js",
    )
    .expect_err("dependency failure propagates");
    assert!(error.message().contains("boom"), "{}", error.message());
}

#[test]
fn export_of_an_imported_binding_is_an_indirect_export() {
    let log = run(
        &[
            (
                "lib.js",
                "export let v = 1; export function set() { v = 2; }",
            ),
            (
                "mid.js",
                "import { v, set } from 'lib.js'; export { v, set };",
            ),
            (
                "main.js",
                "import { v, set } from 'mid.js'; log.push(v); set(); log.push(v);",
            ),
        ],
        "main.js",
    )
    .expect("runs");
    assert_eq!(log, "1,2");
}
