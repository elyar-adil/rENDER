use crate::{CompiledScript, DynamicModuleUnit, JsErrorKind, JsRuntime, RuntimeLimits};
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
        .to_js_string()
        .to_lossy())
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

/// A loader over an in-memory map: the specifier is the module key.
#[derive(Debug)]
struct MapLoader(Vec<(&'static str, &'static str)>);

impl crate::ModuleLoader for MapLoader {
    fn load(
        &mut self,
        _referrer: &str,
        specifier: &str,
        _attributes: &[(String, String)],
    ) -> Result<(String, CompiledScript), crate::JsError> {
        let (_, source) = self
            .0
            .iter()
            .find(|(key, _)| *key == specifier)
            .ok_or_else(|| crate::JsError::type_error(format!("no module {specifier}")))?;
        let script = CompiledScript::compile_module(source, &RuntimeLimits::default())?;
        Ok((specifier.to_owned(), script))
    }
}

/// Run `entry` as module `main.js` with `modules` loadable through the loader,
/// drain the job queue, and return `log` joined by commas.
fn run_with_loader(
    modules: &[(&'static str, &'static str)],
    entry: &str,
) -> Result<String, crate::JsError> {
    let mut dom = parse_document("<!doctype html><html><body></body></html>").dom;
    let mut runtime = JsRuntime::new(&dom);
    runtime.execute(&mut dom, "var log = [];").expect("log");
    runtime.set_module_loader(Box::new(MapLoader(modules.to_vec())));
    let script = CompiledScript::compile_module(entry, &RuntimeLimits::default())?;
    runtime.load_module("main.js", &script)?;
    runtime.evaluate_module(&mut dom, "main.js")?;
    while let pending = runtime.take_pending_microtasks()
        && !pending.is_empty()
    {
        for microtask in pending {
            runtime.invoke_microtask(&mut dom, microtask)?;
        }
    }
    Ok(runtime
        .execute(&mut dom, "log.join(',')")
        .expect("read log")
        .value
        .to_js_string()
        .to_lossy())
}

#[test]
fn dynamic_import_resolves_to_the_namespace_after_evaluation() {
    let log = run_with_loader(
        &[("lib.js", "export const value = 42; log.push('lib');")],
        "import('lib.js').then(ns => log.push('ns', ns.value));",
    )
    .expect("runs");
    assert_eq!(log, "lib,ns,42");
}

#[test]
fn dynamic_import_of_a_missing_module_rejects() {
    let log = run_with_loader(
        &[],
        "import('missing.js').then(() => log.push('resolved'), error => log.push(error.name));",
    )
    .expect("runs");
    assert_eq!(log, "TypeError");
}

#[test]
fn import_defer_evaluates_its_module_on_the_first_property_read() {
    let log = run_with_loader(
        &[("dep.js", "log.push('dep'); export const value = 1;")],
        "import defer * as ns from 'dep.js'; log.push('imported'); log.push(ns.value);",
    )
    .expect("runs");
    assert_eq!(log, "imported,dep,1");
}

#[test]
fn a_namespace_refuses_writes_and_deletes_of_its_exports() {
    let log = run(
        &[
            ("m.js", "export var a = 1;"),
            (
                "main.js",
                "import * as ns from 'm.js'; var out = []; \
                 try { ns.a = 2; out.push('written'); } catch (error) { out.push(error.name); } \
                 try { delete ns.a; out.push('deleted'); } catch (error) { out.push(error.name); } \
                 log.push(out.join('|'), Reflect.set(ns, 'a', 1), ns.a, delete ns.absent);",
            ),
        ],
        "main.js",
    )
    .expect("runs");
    assert_eq!(log, "TypeError|TypeError,false,1,true");
}

#[test]
fn top_level_await_completes_a_module_before_its_importers_run() {
    let log = run_with_loader(
        &[
            ("a.js", "log.push('a1'); await null; log.push('a2');"),
            ("b.js", "import 'a.js'; log.push('b');"),
        ],
        "import 'b.js'; log.push('main');",
    )
    .expect("runs");
    assert_eq!(log, "a1,a2,b,main");
}

#[test]
fn a_repeated_import_attribute_key_is_an_early_error() {
    let error = CompiledScript::compile_module(
        "import x from 'x.js' with { type: 'json', type: 'json' };",
        &RuntimeLimits::default(),
    )
    .expect_err("duplicate attribute keys must not compile");
    assert_eq!(error.kind(), crate::JsErrorKind::Syntax);
}

#[test]
fn an_async_dynamic_import_waits_for_the_embedding_and_then_evaluates() {
    let mut dom = parse_document("<!doctype html><html><body></body></html>").dom;
    let mut runtime = JsRuntime::new(&dom);
    runtime
        .execute(
            &mut dom,
            "var log = []; import('widgets').then(ns => log.push('ns', ns.value));",
        )
        .expect("dynamic import queues");
    let pending = runtime.take_pending_module_loads();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].specifier, "widgets");
    assert!(!runtime.pending_module_load_queue_empty());

    let key = "https://example.test/widgets.js";
    let compiled =
        CompiledScript::compile_module("export const value = 42;", &RuntimeLimits::default())
            .expect("module compiles");
    runtime
        .resolve_dynamic_import(
            &mut dom,
            pending[0].id,
            key,
            &[DynamicModuleUnit {
                key: key.to_owned(),
                compiled,
                resolutions: BTreeMap::new(),
            }],
        )
        .expect("dynamic import resolves");
    assert!(runtime.pending_module_load_queue_empty());
    while let Some(microtask) = runtime.take_next_microtask() {
        runtime
            .invoke_microtask(&mut dom, microtask)
            .expect("microtask runs");
    }
    let log = runtime
        .execute(&mut dom, "log.join(',')")
        .expect("reads log")
        .value
        .to_js_string();
    assert_eq!(log.to_lossy(), "ns,42");
}

#[test]
fn an_async_dynamic_import_can_be_rejected_by_the_embedding() {
    let mut dom = parse_document("<!doctype html><html><body></body></html>").dom;
    let mut runtime = JsRuntime::new(&dom);
    runtime
        .execute(
            &mut dom,
            "var log = []; import('missing').catch(error => log.push(error.name));",
        )
        .expect("dynamic import queues");
    let pending = runtime.take_pending_module_loads();
    runtime.reject_dynamic_import(pending[0].id, "network failed");
    while let Some(microtask) = runtime.take_next_microtask() {
        runtime
            .invoke_microtask(&mut dom, microtask)
            .expect("microtask runs");
    }
    let log = runtime
        .execute(&mut dom, "log.join(',')")
        .expect("reads log")
        .value
        .to_js_string();
    assert_eq!(log.to_lossy(), "TypeError");
}
