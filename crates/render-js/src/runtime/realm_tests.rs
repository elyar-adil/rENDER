//! Multiple realms (ECMA-262 9.3, `CreateRealm`) and the `ShadowRealm` proposal
//! built on them: separate globals and intrinsics on one heap, calls that run
//! in the callee's realm, and the wrapping of values across a realm boundary.

use super::ModuleLoader;
use crate::{CompiledScript, JsError, JsRuntime, RealmId, RuntimeLimits};
use render_dom::Dom;
use render_html::parse_document;

fn page() -> (Dom, JsRuntime) {
    let dom = parse_document("<!doctype html><html><body></body></html>").dom;
    let runtime = JsRuntime::new(&dom);
    (dom, runtime)
}

/// Runs `source` as a script in the main realm and returns its completion as text.
fn run(dom: &mut Dom, runtime: &mut JsRuntime, source: &str) -> String {
    runtime
        .execute(dom, source)
        .map(|outcome| outcome.value.to_js_string().to_lossy())
        .unwrap_or_else(|error| panic!("{source}\n=> {error}"))
}

/// Runs `source` as a script in `realm`, as a host function of that realm does.
fn run_in(dom: &mut Dom, runtime: &mut JsRuntime, realm: RealmId, source: &str) -> String {
    let script = CompiledScript::compile(source, &RuntimeLimits::default())
        .unwrap_or_else(|error| panic!("{source}\n=> {error}"));
    runtime
        .with_realm(realm, |runtime| runtime.execute_nested(dom, &script))
        .map(|outcome| outcome.value.to_js_string().to_lossy())
        .unwrap_or_else(|error| panic!("{source}\n=> {error}"))
}

#[test]
fn a_created_realm_has_a_global_object_of_its_own() {
    let (mut dom, mut runtime) = page();
    run(&mut dom, &mut runtime, "var mine = 1;");
    let other = runtime.create_realm(&mut dom);

    assert_eq!(
        run_in(&mut dom, &mut runtime, other, "typeof mine"),
        "undefined"
    );
    assert_eq!(
        run_in(&mut dom, &mut runtime, other, "var mine = 2; mine"),
        "2"
    );
    assert_eq!(run(&mut dom, &mut runtime, "mine"), "1");
    assert_eq!(
        run_in(
            &mut dom,
            &mut runtime,
            other,
            "globalThis.globalThis === globalThis"
        ),
        "true"
    );
}

#[test]
fn a_created_realm_has_the_ecmascript_globals_and_no_browser_globals() {
    let (mut dom, mut runtime) = page();
    let other = runtime.create_realm(&mut dom);

    assert_eq!(
        run_in(
            &mut dom,
            &mut runtime,
            other,
            "[typeof Array, typeof Symbol, typeof ShadowRealm, typeof document, typeof fetch, typeof localStorage, typeof console].join()"
        ),
        "function,function,function,undefined,undefined,undefined,undefined"
    );
}

#[test]
fn intrinsics_are_distinct_per_realm_and_a_call_runs_in_the_callee_realm() {
    let (mut dom, mut runtime) = page();
    let other = runtime.create_realm(&mut dom);
    run_in(
        &mut dom,
        &mut runtime,
        other,
        "globalThis.make = function () { return []; };
         globalThis.isOther = function (value) { return Object.getPrototypeOf(value) === Array.prototype; };",
    );
    let make = runtime
        .with_realm(other, |runtime| runtime.realm().global("make"))
        .expect("make is defined in the other realm");
    let is_other = runtime
        .with_realm(other, |runtime| runtime.realm().global("isOther"))
        .expect("isOther is defined in the other realm");
    runtime.realm.set_global("make".to_owned(), make);
    runtime.realm.set_global("isOther".to_owned(), is_other);

    // The array is made by code of the other realm, so it has that realm's prototype.
    assert_eq!(
        run(
            &mut dom,
            &mut runtime,
            "[make() instanceof Array, isOther(make())].join()"
        ),
        "false,true"
    );
}

#[test]
fn a_shadow_realm_evaluates_source_and_wraps_what_crosses_back() {
    let (mut dom, mut runtime) = page();
    assert_eq!(
        run(
            &mut dom,
            &mut runtime,
            "var r = new ShadowRealm();
             var doubled = r.evaluate('(x) => x * 2');
             [r.evaluate('1 + 1'), typeof doubled, doubled(21), Object.prototype.toString.call(r),
              r.evaluate('var declared = 5; declared')].join()"
        ),
        "2,function,42,[object ShadowRealm],5"
    );
}

#[test]
fn a_shadow_realm_reports_failures_as_the_callers_errors() {
    let (mut dom, mut runtime) = page();
    assert_eq!(
        run(
            &mut dom,
            &mut runtime,
            "var r = new ShadowRealm();
             function kind(source) {
               try { r.evaluate(source); return 'none'; }
               catch (error) { return error instanceof SyntaxError ? 'SyntaxError' : error instanceof TypeError ? 'TypeError' : 'other'; }
             }
             [kind('...'), kind('throw 42'), kind('globalThis'), kind(\"'use strict'; eval('var public = 1')\"), kind(1)].join()"
        ),
        "SyntaxError,TypeError,TypeError,TypeError,TypeError"
    );
}

/// Runs queued jobs until none remain, as a host's microtask checkpoint does.
fn drain(dom: &mut Dom, runtime: &mut JsRuntime) {
    loop {
        let pending = runtime.take_pending_microtasks();
        if pending.is_empty() {
            return;
        }
        for microtask in pending {
            runtime
                .invoke_microtask(dom, microtask)
                .expect("queued job runs");
        }
    }
}

/// A loader over an in-memory module table; specifiers are the table's keys.
#[derive(Debug)]
struct TableLoader(std::collections::BTreeMap<&'static str, &'static str>);

impl ModuleLoader for TableLoader {
    fn load(
        &mut self,
        _referrer: &str,
        specifier: &str,
        _attributes: &[(String, String)],
    ) -> Result<(String, CompiledScript), JsError> {
        let Some(source) = self.0.get(specifier) else {
            return Err(JsError::type_error(format!(
                "{specifier} is not in the table"
            )));
        };
        let script = CompiledScript::compile_module(source, &RuntimeLimits::default())?;
        Ok((specifier.to_owned(), script))
    }
}

fn table_loader(runtime: &mut JsRuntime) {
    let modules = [
        ("dep.js", "export var x = 1; export default 42;"),
        ("throws.js", "throw new Error('module body');"),
        (
            "cycle.js",
            "export var v = 7; export { v as again } from 'cycle.js';",
        ),
    ]
    .into_iter()
    .collect();
    runtime.set_module_loader(Box::new(TableLoader(modules)));
}

#[test]
fn dynamic_import_settles_with_the_namespace_of_the_loaded_module() {
    let (mut dom, mut runtime) = page();
    table_loader(&mut runtime);
    run(
        &mut dom,
        &mut runtime,
        "var seen = [];
         import('dep.js').then(ns => seen.push(ns.x, ns.default));
         import('missing.js').catch(error => seen.push(error instanceof TypeError));
         import('throws.js').catch(error => seen.push(error instanceof Error));
         import('cycle.js').then(ns => seen.push(ns.again));",
    );
    drain(&mut dom, &mut runtime);
    // A module that cannot be loaded rejects inside the `import()` call
    // (ContinueDynamicImport's abrupt completion), so its reaction is queued
    // first. A loaded module continues in a job (`PerformPromiseThen` on the
    // load promise), so the `then` reactions follow in order of their jobs.
    assert_eq!(
        run(&mut dom, &mut runtime, "seen.join()"),
        "true,1,42,true,7"
    );
}

#[test]
fn shadow_realm_import_value_loads_into_the_shadow_realm_and_wraps_the_export() {
    let (mut dom, mut runtime) = page();
    table_loader(&mut runtime);
    run(
        &mut dom,
        &mut runtime,
        "var r = new ShadowRealm(); var seen = [];
         r.importValue('dep.js', 'x').then(value => seen.push(value));
         r.importValue('dep.js', 'missing').catch(error => seen.push(error instanceof TypeError));
         r.importValue('throws.js', 'x').catch(error => seen.push(error instanceof TypeError));",
    );
    drain(&mut dom, &mut runtime);
    assert_eq!(run(&mut dom, &mut runtime, "seen.join()"), "1,true,true");
}

#[test]
fn a_wrapped_function_carries_arguments_in_and_results_out() {
    let (mut dom, mut runtime) = page();
    assert_eq!(
        run(
            &mut dom,
            &mut runtime,
            "var r = new ShadowRealm();
             var callsBack = r.evaluate('(f) => f(() => 7)');
             var identity = r.evaluate('(f) => f');
             [callsBack(function (g) { return g(); }), identity(function () {}) instanceof Function].join()"
        ),
        "7,true"
    );
}
