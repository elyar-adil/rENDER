//! `using` and `await using` declarations (ECMA-262 14.3.1, 14.3.3): their
//! early errors, and the order in which their resources are disposed when the
//! scope they are declared in is left.

use crate::CompiledScript;
use crate::JsErrorKind;
use crate::JsRuntime;
use crate::RuntimeLimits;
use render_html::parse_document;
use std::collections::BTreeMap;

fn runtime_with_dom() -> (render_dom::Dom, JsRuntime) {
    let dom = parse_document("<!doctype html><html><body></body></html>").dom;
    let runtime = JsRuntime::new(&dom);
    (dom, runtime)
}

/// Run `source` and return the string form of its completion value.
fn ok(source: &str) -> String {
    let (mut dom, mut runtime) = runtime_with_dom();
    runtime
        .execute(&mut dom, source)
        .map(|outcome| outcome.value.to_js_string())
        .unwrap_or_else(|error| panic!("{source}\n=> {error}"))
}

/// Whether `source` is refused with a `SyntaxError` before it runs.
fn is_syntax_error(source: &str) -> bool {
    let (mut dom, mut runtime) = runtime_with_dom();
    match runtime.execute(&mut dom, source) {
        Err(error) => error.kind() == JsErrorKind::Syntax,
        Ok(_) => false,
    }
}

/// Run `source` with a `log` array, drain the microtasks it queued, and return
/// `log.join(',')`.
fn run_async(source: &str) -> String {
    let (mut dom, mut runtime) = runtime_with_dom();
    runtime
        .execute(&mut dom, "var log = [];")
        .unwrap_or_else(|error| panic!("setup => {error}"));
    runtime
        .execute(&mut dom, source)
        .unwrap_or_else(|error| panic!("{source}\n=> {error}"));
    drain(&mut runtime, &mut dom);
    runtime
        .execute(&mut dom, "log.join(',')")
        .unwrap_or_else(|error| panic!("{source}\n=> {error}"))
        .value
        .to_js_string()
}

fn drain(runtime: &mut JsRuntime, dom: &mut render_dom::Dom) {
    for _ in 0..200 {
        let microtasks = runtime.take_pending_microtasks();
        let mut progressed = !microtasks.is_empty();
        for microtask in microtasks {
            let _ = runtime.invoke_microtask(dom, microtask);
        }
        let timers = runtime.take_pending_timer_requests();
        progressed |= !timers.is_empty();
        for timer in timers {
            if let crate::TimerRequest::Schedule { id, .. } = timer {
                let _ = runtime.fire_timer(dom, id);
            }
        }
        if !progressed {
            break;
        }
    }
}

#[test]
fn using_is_refused_at_the_top_level_of_a_script() {
    assert_eq!(ok("var r = 0; { using x = null; r = 1; } r"), "1");
    assert!(is_syntax_error("using x = null;"));
    assert!(is_syntax_error("await using x = null;"));
    assert!(is_syntax_error("{ using x = null; } using y = null;"));
}

#[test]
fn using_is_refused_where_only_a_statement_is_grammatical() {
    assert!(!is_syntax_error("{ using x = null; }"));
    assert!(is_syntax_error("if (true) using x = null;"));
    assert!(is_syntax_error("while (false) using x = null;"));
    assert!(is_syntax_error("l: using x = null;"));
    assert!(is_syntax_error("switch (0) { case 0: using x = null; }"));
    assert!(is_syntax_error("switch (0) { default: using x = null; }"));
    assert!(is_syntax_error("{ using x; }"));
    assert!(is_syntax_error("{ using [] = null; }"));
    assert!(is_syntax_error("{ using x = null, [] = null; }"));
    assert!(is_syntax_error("for (using x in {}) {}"));
    assert!(is_syntax_error("for (using x = null in {}) {}"));
}

#[test]
fn using_on_a_line_of_its_own_is_an_expression() {
    assert_eq!(ok("var r = 0; { using x = null; r = 1; } r"), "1");
    assert_eq!(ok("var using = 1, x = 0; { using\nx = 5 } x"), "5");
    assert_eq!(
        ok("var using = [], x = 0; { using[x] = null; } using.length"),
        "1"
    );
    assert_eq!(
        ok(
            "var using, of = [[9], [8], [7]], result = []; for (using of of [0, 1, 2]) result.push(using); result[0]"
        ),
        "7"
    );
}

#[test]
fn using_then_a_line_break_then_let_is_two_statements() {
    assert_eq!(
        ok("var r; { using\nlet = 'irrelevant'; r = typeof let; var using, let; } r"),
        "string"
    );
    assert!(is_syntax_error("'use strict'; let = 1;"));
}

#[test]
fn using_for_heads_are_declarations() {
    assert_eq!(ok("for (using x = null;;) break; 'ok'"), "ok");
    assert_eq!(ok("for (using of = null;;) break; 'ok'"), "ok");
    assert_eq!(
        ok("var r = []; { for (using x of [null]) r.push(x); } r.length"),
        "1"
    );
}

#[test]
fn using_declarations_need_an_initializer_and_a_plain_name() {
    assert!(!is_syntax_error("{ using x = null; }"));
    assert!(is_syntax_error("{ using x; }"));
    assert!(is_syntax_error("{ using [] = null; }"));
    assert!(is_syntax_error("{ using {x} = null; }"));
    assert!(is_syntax_error("{ using let = null; }"));
}

#[test]
fn using_declarations_complete_empty() {
    assert_eq!(ok("4; {using x = null;}"), "4");
    assert_eq!(ok("6; {using x = null, y = null;}"), "6");
    assert_eq!(ok("7; {using x = null; {}}"), "7");
    assert_eq!(ok("{using x = null;}"), "undefined");
}

#[test]
fn block_resources_are_disposed_last_declared_first() {
    assert_eq!(
        run_async(
            r"
            function d(n) { return { n: n, [Symbol.dispose]() { log.push('dispose' + this.n); } }; }
            { using a = d(1); using b = d(2); log.push('body'); }
            "
        ),
        "body,dispose2,dispose1"
    );
}

#[test]
fn block_resources_are_disposed_on_break_continue_return_and_throw() {
    assert_eq!(
        run_async(
            r"
            function d(n) { return { n: n, [Symbol.dispose]() { log.push('dispose' + this.n); } }; }
            for (using x of [d(1), d(2), d(3)]) { log.push('body' + x.n); if (x.n == 2) break; }
            log.push('end');
            "
        ),
        "body1,dispose1,body2,dispose2,end"
    );
    assert_eq!(
        run_async(
            r"
            function d(n) { return { n: n, [Symbol.dispose]() { log.push('dispose' + this.n); } }; }
            for (using x of [d(1), d(2)]) { if (x.n == 1) continue; log.push('b' + x.n); }
            "
        ),
        "dispose1,b2,dispose2"
    );
    assert_eq!(
        run_async(
            r"
            function d(n) { return { n: n, [Symbol.dispose]() { log.push('dispose' + this.n); } }; }
            function f() { { using x = d(1); return 'r'; } }
            log.push(f());
            "
        ),
        "dispose1,r"
    );
    assert_eq!(
        run_async(
            r"
            function d(n) { return { n: n, [Symbol.dispose]() { log.push('dispose' + this.n); } }; }
            try { { using x = d(1); throw 'boom'; } } catch (e) { log.push('caught:' + e); }
            "
        ),
        "dispose1,caught:boom"
    );
}

#[test]
fn a_null_or_undefined_using_value_is_not_disposed() {
    assert_eq!(
        ok("var n = 0; { using a = null; using b = undefined; n = 1; } n"),
        "1"
    );
}

#[test]
fn a_non_disposable_using_value_throws_at_its_declaration() {
    assert_eq!(
        run_async(
            r"
            try { { using c = {}; log.push('after'); } } catch (e) { log.push(e.name); }
            "
        ),
        "TypeError"
    );
}

#[test]
fn a_disposal_error_without_a_pending_throw_is_thrown_as_is() {
    assert_eq!(
        run_async(
            r"
            try { { using x = { [Symbol.dispose]() { throw 'dd'; } }; } } catch (e) { log.push(e); }
            "
        ),
        "dd"
    );
}

#[test]
fn a_disposal_error_over_a_pending_throw_is_a_suppressed_error() {
    assert_eq!(
        run_async(
            r"
            try { { using x = { [Symbol.dispose]() { throw 'd'; } }; throw 'p'; } }
            catch (e) { log.push(e.name + ':' + e.error + ':' + e.suppressed); }
            "
        ),
        "SuppressedError:d:p"
    );
}

#[test]
fn a_using_binding_is_immutable() {
    assert_eq!(
        run_async(
            r"
            { using x = null; try { x = 1; } catch (e) { log.push(e.name); } }
            "
        ),
        "TypeError"
    );
}

#[test]
fn a_using_binding_is_in_its_temporal_dead_zone_until_initialized() {
    assert_eq!(
        run_async(
            r"
            try { { using x = x; } } catch (e) { log.push(e.name); }
            "
        ),
        "ReferenceError"
    );
}

#[test]
fn a_for_statement_head_resource_is_disposed_after_the_loop() {
    assert_eq!(
        run_async(
            r"
            function d(n) { return { n: n, [Symbol.dispose]() { log.push('dispose' + this.n); } }; }
            for (using x = d(1); ; ) { log.push('loop'); break; }
            log.push('after');
            "
        ),
        "loop,dispose1,after"
    );
}

#[test]
fn an_await_using_block_awaits_its_disposal() {
    assert_eq!(
        run_async(
            r"
            async function f() {
              {
                await using a = { async [Symbol.asyncDispose]() { log.push('a'); } };
                await using b = { async [Symbol.asyncDispose]() { log.push('b'); } };
                log.push('body');
              }
              log.push('after');
            }
            f();
            "
        ),
        "body,b,a,after"
    );
}

#[test]
fn an_await_using_null_still_awaits_when_it_is_evaluated() {
    assert_eq!(
        run_async(
            r"
            async function f() { { await using x = null; } log.push('f'); }
            f();
            log.push('sync');
            "
        ),
        "sync,f"
    );
}

#[test]
fn an_await_using_that_is_not_evaluated_does_not_await() {
    assert_eq!(
        run_async(
            r"
            async function f() { outer: { break outer; await using x = null; } log.push('f'); }
            f();
            log.push('sync');
            "
        ),
        "f,sync"
    );
}

#[test]
fn an_await_using_rejection_becomes_a_disposal_error() {
    assert_eq!(
        run_async(
            r"
            async function f() {
              try {
                await using x = { async [Symbol.asyncDispose]() { throw 'd'; } };
                throw 'b';
              } catch (e) { log.push(e.name + ':' + e.error + ':' + e.suppressed); }
            }
            f();
            "
        ),
        "SuppressedError:d:b"
    );
}

#[test]
fn an_await_using_for_of_disposes_each_iteration_with_an_await() {
    assert_eq!(
        run_async(
            r"
            function d(n) { return { n: n, async [Symbol.asyncDispose]() { log.push('dispose' + this.n); } }; }
            async function f() {
              for (await using x of [d(1), d(2)]) { log.push('body' + x.n); }
              log.push('end');
            }
            f();
            "
        ),
        "body1,dispose1,body2,dispose2,end"
    );
}

#[test]
fn an_await_using_in_a_function_body_is_disposed_on_return() {
    assert_eq!(
        run_async(
            r"
            async function f() {
              await using x = { async [Symbol.asyncDispose]() { log.push('dispose'); } };
              log.push('body');
              return 'r';
            }
            f().then((v) => log.push(v));
            "
        ),
        "body,dispose,r"
    );
}

#[test]
fn a_generator_disposes_its_block_when_it_is_returned_or_thrown_into() {
    assert_eq!(
        run_async(
            r"
            function d(n) { return { [Symbol.dispose]() { log.push('dispose' + n); } }; }
            function* g() { { using x = d(1); yield 1; log.push('no'); } }
            var it = g(); it.next(); it.return(5); log.push('done');
            "
        ),
        "dispose1,done"
    );
    assert_eq!(
        run_async(
            r"
            function d(n) { return { [Symbol.dispose]() { log.push('dispose' + n); } }; }
            function* g() { { using x = d(1); yield 1; } }
            var it = g(); it.next();
            try { it.throw('e'); } catch (e) { log.push('caught' + e); }
            "
        ),
        "dispose1,caughte"
    );
}

#[test]
fn an_async_generator_disposes_its_block_when_it_is_returned_into() {
    assert_eq!(
        run_async(
            r"
            async function* ag() {
              { await using x = { async [Symbol.asyncDispose]() { log.push('dispose'); } }; yield 1; }
            }
            (async () => { var it = ag(); await it.next(); await it.return('r'); log.push('done'); })();
            "
        ),
        "dispose,done"
    );
}

#[test]
fn an_await_using_in_a_try_body_is_disposed_before_the_catch_runs() {
    assert_eq!(
        run_async(
            r"
            async function f() {
              try {
                await using x = { async [Symbol.asyncDispose]() { log.push('dispose'); } };
                throw 'b';
              } catch (e) { log.push('caught' + e); }
            }
            f();
            "
        ),
        "dispose,caughtb"
    );
}

#[test]
fn an_await_using_in_a_for_head_is_disposed_after_the_loop() {
    assert_eq!(
        run_async(
            r"
            async function f() {
              for (await using x = { async [Symbol.asyncDispose]() { log.push('dispose'); } }; ; ) {
                log.push('loop');
                break;
              }
              log.push('after');
            }
            f();
            "
        ),
        "loop,dispose,after"
    );
}

/// Evaluate `source` as the module `main.js` and return `log.join(',')`.
fn run_module(source: &str) -> Result<String, crate::JsError> {
    let (mut dom, mut runtime) = runtime_with_dom();
    runtime.execute(&mut dom, "var log = [];")?;
    let script = CompiledScript::compile_module(source, &RuntimeLimits::default())?;
    runtime.declare_module("main.js", &script, BTreeMap::new())?;
    runtime.evaluate_module(&mut dom, "main.js")?;
    Ok(runtime
        .execute(&mut dom, "log.join(',')")?
        .value
        .to_js_string())
}

#[test]
fn a_module_disposes_its_top_level_using_when_it_finishes() {
    assert_eq!(
        run_module(
            r"
            var d = { [Symbol.dispose]() { log.push('dispose'); } };
            using x = d;
            log.push('body');
            "
        )
        .expect("runs"),
        "body,dispose"
    );
}

#[test]
fn await_using_is_refused_at_a_modules_top_level() {
    assert_eq!(
        run_module("using x = null; log.push('ok');").expect("runs"),
        "ok"
    );
    assert!(run_module("await using x = null;").is_err());
}
