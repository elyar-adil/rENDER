//! Generators and async functions: suspension, resumption, and the control
//! flow that has to survive both.

use crate::{JsErrorKind, JsRuntime};
use render_html::parse_document;

/// Run `source`, then drain timers and microtasks, and return `log.join(',')`.
fn run(source: &str) -> Result<String, crate::JsError> {
    let mut dom = parse_document("<!doctype html><html><body></body></html>").dom;
    let mut runtime = JsRuntime::new(&dom);
    runtime.execute(&mut dom, "var log = [];")?;
    runtime.execute(&mut dom, source)?;
    drain(&mut runtime, &mut dom);
    let outcome = runtime.execute(&mut dom, "log.join(',')")?;
    assert!(runtime.prelude_error().is_none());
    Ok(outcome.value.to_js_string())
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

fn ok(source: &str) -> String {
    run(source).unwrap_or_else(|error| panic!("{source}\n=> {error}"))
}

#[test]
fn a_generator_suspends_and_resumes_with_sent_values() {
    assert_eq!(
        ok(
            "function* g() { log.push('a'); var x = yield 1; log.push('b' + x); var y = yield 2; return x + y; } \
            var it = g(); var r1 = it.next(); var r2 = it.next(10); var r3 = it.next(5); \
            log.push(r1.value, r1.done, r2.value, r2.done, r3.value, r3.done);"
        ),
        "a,b10,1,false,2,false,15,true"
    );
}

#[test]
fn a_generator_does_not_start_until_next_is_called() {
    assert_eq!(
        ok(
            "function* g() { log.push('started'); yield 1; } var it = g(); log.push('created'); it.next();"
        ),
        "created,started"
    );
}

#[test]
fn generators_are_lazy_iterables() {
    assert_eq!(
        ok("function* count() { var i = 0; while (true) yield i++; } \
            var out = []; for (var n of count()) { if (n > 3) break; out.push(n); } log.push(out.join('')); \
            log.push(Array.from({ [Symbol.iterator]: function* () { yield 'x'; yield 'y'; } }).join(''));"),
        "0123,xy"
    );
}

#[test]
fn spread_destructuring_and_collections_consume_generators() {
    assert_eq!(
        ok("function* g() { yield 1; yield 2; yield 3; } \
            log.push([...g()].join('')); var [a, b] = g(); log.push(a + b); \
            log.push(new Set(g()).size); log.push(Math.max(...g()));"),
        "123,3,3,3"
    );
}

#[test]
fn return_and_throw_drive_a_suspended_generator() {
    assert_eq!(
        ok(
            "function* g() { try { yield 1; yield 2; } finally { log.push('cleanup'); } } \
            var it = g(); it.next(); var r = it.return('bye'); log.push(r.value, r.done, it.next().done);"
        ),
        "cleanup,bye,true,true"
    );
    assert_eq!(
        ok(
            "function* g() { try { yield 1; } catch (e) { log.push('caught ' + e); yield 'recovered'; } } \
            var it = g(); it.next(); log.push(it.throw('boom').value);"
        ),
        "caught boom,recovered"
    );
    assert_eq!(
        ok(
            "function* g() { yield 1; } var it = g(); try { it.throw(new Error('early')); } catch (e) { log.push(e.message, it.next().done); }"
        ),
        "early,true"
    );
}

#[test]
fn yield_star_delegates_values_and_the_return_value() {
    assert_eq!(
        ok(
            "function* inner() { var got = yield 'i1'; log.push('inner got ' + got); return 'inner-done'; } \
            function* outer() { var r = yield* inner(); log.push(r); yield* [10, 20]; } \
            var it = outer(); log.push(it.next().value); log.push(it.next('sent').value); log.push(it.next().value);"
        ),
        "i1,inner got sent,inner-done,10,20"
    );
}

#[test]
fn loops_and_labels_work_across_a_yield() {
    assert_eq!(
        ok(
            "function* g() { outer: for (var i = 0; i < 3; i++) { for (var j = 0; j < 3; j++) { if (j == 1) continue outer; if (i == 2) break outer; yield i * 10 + j; } } yield 'end'; } \
            log.push([...g()].join('|'));"
        ),
        "0|10|end"
    );
    assert_eq!(
        ok(
            "function* g() { var i = 0; do { yield i; i++; } while (i < 3); } log.push([...g()].join(''));"
        ),
        "012"
    );
    assert_eq!(
        ok(
            "function* g() { switch (yield 'ask') { case 1: yield 'one'; break; case 2: yield 'two'; default: yield 'fall'; } } \
            var it = g(); it.next(); log.push(it.next(2).value, it.next().value, it.next().done);"
        ),
        "two,fall,true"
    );
}

#[test]
fn per_iteration_bindings_survive_a_suspension() {
    assert_eq!(
        ok(
            "function* g() { var fs = []; for (let i = 0; i < 3; i++) { yield i; fs.push(function () { return i; }); } return fs; } \
            var it = g(); var r; while (!(r = it.next()).done) {} log.push(r.value.map(function (f) { return f(); }).join(''));"
        ),
        "012"
    );
    assert_eq!(
        ok(
            "function* g() { var fs = []; for (const x of [1, 2, 3]) { yield x; fs.push(function () { return x; }); } return fs; } \
            var it = g(); var r; while (!(r = it.next()).done) {} log.push(r.value.map(function (f) { return f(); }).join(''));"
        ),
        "123"
    );
}

#[test]
fn finally_runs_on_every_way_out() {
    assert_eq!(
        ok(
            "function* g() { for (var i = 0; i < 3; i++) { try { if (i == 1) continue; if (i == 2) break; yield i; } finally { log.push('f' + i); } } yield 'after'; } \
            log.push([...g()].join(''));"
        ),
        "f0,f1,f2,0after"
    );
    assert_eq!(
        ok(
            "function* g() { try { try { yield 1; return 'r'; } finally { log.push('inner'); } } finally { log.push('outer'); } } \
            var it = g(); it.next(); log.push(it.next().value);"
        ),
        "inner,outer,r"
    );
}

#[test]
fn breaking_out_of_for_of_closes_the_generator() {
    assert_eq!(
        ok(
            "function* g() { try { yield 1; yield 2; } finally { log.push('closed'); } } \
            for (var x of g()) { log.push(x); break; }"
        ),
        "1,closed"
    );
}

#[test]
fn a_running_generator_cannot_be_reentered() {
    let error = run("var it; function* g() { it.next(); yield 1; } it = g(); it.next();")
        .expect_err("re-entry throws");
    assert_eq!(error.kind(), JsErrorKind::Type);
}

#[test]
fn generator_methods_in_classes_and_objects() {
    assert_eq!(
        ok(
            "class C { constructor() { this.items = [1, 2]; } *[Symbol.iterator]() { for (var x of this.items) yield x * 2; } } \
            log.push([...new C()].join(''));"
        ),
        "24"
    );
    assert_eq!(
        ok(
            "var o = { *gen() { yield 'a'; yield 'b'; }, async *noop() {} }; log.push([...o.gen()].join(''));"
        ),
        "ab"
    );
}

// ------------------------------------------------------------------- async

#[test]
fn an_async_function_returns_a_promise_and_runs_to_the_first_await_synchronously() {
    assert_eq!(
        ok(
            "async function f() { log.push('f1'); await null; log.push('f2'); return 7; } \
            var p = f(); log.push('sync', p instanceof Promise); p.then(function (v) { log.push('then' + v); });"
        ),
        "f1,sync,true,f2,then7"
    );
}

#[test]
fn await_waits_for_a_timer() {
    assert_eq!(
        ok(
            "(async function () { log.push(1); var v = await new Promise(function (r) { setTimeout(function () { r(5); }, 0); }); log.push(v); })();"
        ),
        "1,5"
    );
}

#[test]
fn await_inside_expressions_keeps_evaluation_order() {
    assert_eq!(
        ok("function v(x) { log.push('eval' + x); return x; } \
            (async function () { var r = v(1) + await v(2) + v(3); log.push(r); })();"),
        "eval1,eval2,eval3,6"
    );
    assert_eq!(
        ok("var o = { n: 1, m(a, b) { return this.n + a + b; } }; \
            (async function () { log.push(o.m(await 2, await 3)); })();"),
        "6"
    );
    assert_eq!(
        ok(
            "(async function () { var a = [await 1, await 2]; var b = { x: await 3 }; log.push(a.join(''), b.x, `t${await 4}`); })();"
        ),
        "12,3,t4"
    );
    assert_eq!(
        ok(
            "(async function () { var x = 10; x += await 5; log.push(x); log.push((await 0) || (await 'fallback')); log.push(true ? await 'yes' : await 'no'); })();"
        ),
        "15,fallback,yes"
    );
}

#[test]
fn rejection_becomes_an_exception_and_catch_recovers() {
    assert_eq!(
        ok(
            "(async function () { try { await Promise.reject(new Error('nope')); log.push('unreachable'); } catch (e) { log.push('caught ' + e.message); } finally { log.push('fin'); } })();"
        ),
        "caught nope,fin"
    );
    assert_eq!(
        ok(
            "async function f() { throw new Error('inside'); } f().catch(function (e) { log.push('rejected ' + e.message); });"
        ),
        "rejected inside"
    );
    assert_eq!(
        ok(
            "async function f() { await null; null.x; } f().then(null, function (e) { log.push(e instanceof TypeError); });"
        ),
        "true"
    );
}

#[test]
fn async_functions_compose_and_chain() {
    assert_eq!(
        ok("async function add(a, b) { await null; return a + b; } \
            async function main() { var x = await add(1, 2); var y = await add(x, 10); return x + y; } \
            main().then(function (v) { log.push(v); });"),
        "16"
    );
    assert_eq!(
        ok(
            "async function f() { return Promise.resolve('inner'); } f().then(function (v) { log.push(v); });"
        ),
        "inner"
    );
    assert_eq!(
        ok(
            "(async function () { var rs = await Promise.all([1, Promise.resolve(2), (async () => 3)()]); log.push(rs.join('')); })();"
        ),
        "123"
    );
}

#[test]
fn await_adopts_thenables() {
    assert_eq!(
        ok(
            "(async function () { var v = await { then: function (resolve) { resolve('thenable'); } }; log.push(v); })();"
        ),
        "thenable"
    );
}

#[test]
fn await_in_loops_and_with_closures() {
    assert_eq!(
        ok(
            "(async function () { var sum = 0; for (var i = 1; i <= 3; i++) { sum += await i; } log.push(sum); \
            for (const x of [10, 20]) { log.push(await x); } \
            var n = 0; while (n < 2) { await null; n++; } log.push('n' + n); })();"
        ),
        "6,10,20,n2"
    );
    assert_eq!(
        ok(
            "(async function () { var fs = []; for (let i = 0; i < 3; i++) { await null; fs.push(function () { return i; }); } log.push(fs.map(function (f) { return f(); }).join('')); })();"
        ),
        "012"
    );
}

#[test]
fn async_arrows_and_methods() {
    assert_eq!(
        ok(
            "var f = async (x) => { await null; return x * 2; }; f(4).then(function (v) { log.push(v); }); \
            var g = async x => x + 1; g(1).then(function (v) { log.push(v); });"
        ),
        "2,8"
    );
    assert_eq!(
        ok(
            "class A { constructor() { this.k = 3; } async m() { await null; return this.k; } static async s() { return 's'; } } \
            new A().m().then(function (v) { log.push(v); }); A.s().then(function (v) { log.push(v); });"
        ),
        "s,3"
    );
    assert_eq!(
        ok(
            "var o = { v: 9, async m() { await null; return this.v; } }; o.m().then(function (v) { log.push(v); });"
        ),
        "9"
    );
}

#[test]
fn interleaving_follows_the_microtask_queue() {
    assert_eq!(
        ok(
            "async function a() { log.push('a1'); await null; log.push('a2'); } \
            async function b() { log.push('b1'); await null; log.push('b2'); } \
            a(); b(); log.push('main');"
        ),
        "a1,b1,main,a2,b2"
    );
}

#[test]
fn identifiers_named_await_and_yield_still_work_outside_coroutines() {
    assert_eq!(
        ok("var await = 3, yield = 4; log.push(await + yield);"),
        "7"
    );
    assert_eq!(
        ok("function f(await) { return await; } log.push(f(2));"),
        "2"
    );
}

#[test]
fn suspended_coroutines_survive_garbage_collection() {
    let mut dom = parse_document("<!doctype html><html><body></body></html>").dom;
    let mut runtime = JsRuntime::new(&dom);
    runtime
        .execute(
            &mut dom,
            "var log = []; \
             function* g() { var held = { tag: 'kept' }; var items = [1, 2, 3]; for (var x of items) { yield { x: x, held: held }; } } \
             var it = g(); it.next(); \
             (async function () { var local = { v: 'async-local' }; var set = new Set([1, 2]); \
                await new Promise(function (r) { setTimeout(r, 0); }); \
                log.push(local.v, set.size); })();",
        )
        .expect("setup");
    // Allocate and drop plenty of garbage while both coroutines are suspended.
    runtime
        .execute(
            &mut dom,
            "for (var i = 0; i < 3000; i++) { var junk = { a: [i], b: 'x' + i }; }",
        )
        .expect("churn");
    assert!(runtime.collect_garbage() > 0);
    runtime.collect_garbage();
    drain(&mut runtime, &mut dom);
    let outcome = runtime
        .execute(
            &mut dom,
            "var r = it.next(); log.push(r.value.x, r.value.held.tag, it.next().value.x); log.join(',')",
        )
        .expect("resume");
    assert_eq!(outcome.value.to_js_string(), "async-local,2,2,kept,3");
}

#[test]
fn async_generator_yields_values_and_then_completes() {
    assert_eq!(
        ok(
            "async function* g() { yield 1; yield 2; return 3; } \
            var it = g(); \
            it.next().then(function (r) { log.push(r.value, r.done); }); \
            it.next().then(function (r) { log.push(r.value, r.done); }); \
            it.next().then(function (r) { log.push(r.value, r.done); }); \
            it.next().then(function (r) { log.push(r.value, r.done); });"
        ),
        "1,false,2,false,3,true,,true"
    );
}

#[test]
fn async_generator_body_starts_on_the_first_next() {
    assert_eq!(
        ok("async function* g() { log.push('started'); } var it = g(); log.push('created'); it.next();"),
        "created,started"
    );
}

#[test]
fn async_generator_queues_requests_in_order() {
    assert_eq!(
        ok(
            "async function* g() { var a = yield 1; log.push('a' + a); var b = yield 2; log.push('b' + b); } \
            var it = g(); it.next('x'); it.next('p'); it.next('q');"
        ),
        "ap,bq"
    );
}

#[test]
fn async_generator_yield_awaits_its_operand() {
    assert_eq!(
        ok("async function* g() { yield Promise.resolve(7); } g().next().then(function (r) { log.push(r.value, r.done); });"),
        "7,false"
    );
}

#[test]
fn async_generator_return_awaits_its_operand() {
    assert_eq!(
        ok("async function* g() { return Promise.resolve(9); } g().next().then(function (r) { log.push(r.value, r.done); });"),
        "9,true"
    );
}

#[test]
fn async_generator_return_resumes_through_finally() {
    assert_eq!(
        ok(
            "async function* g() { try { yield 1; } finally { log.push('fin'); } } \
            var it = g(); it.next().then(function () { return it.return(5); }) \
              .then(function (r) { log.push(r.value, r.done); });"
        ),
        "fin,5,true"
    );
}

#[test]
fn async_generator_throw_is_delivered_into_the_body_at_yield() {
    assert_eq!(
        ok(
            "async function* g() { try { yield 1; } catch (e) { log.push('caught' + e); yield 2; } } \
            var it = g(); it.next().then(function () { return it.throw('boom'); }) \
              .then(function (r) { log.push(r.value, r.done); });"
        ),
        "caughtboom,2,false"
    );
}

#[test]
fn async_generator_return_before_start_completes_without_running_the_body() {
    assert_eq!(
        ok(
            "async function* g() { log.push('never'); } var it = g(); \
            it.return(4).then(function (r) { log.push(r.value, r.done); });"
        ),
        "4,true"
    );
}

#[test]
fn async_generator_throw_before_start_rejects() {
    assert_eq!(
        ok(
            "async function* g() { log.push('never'); } var it = g(); \
            it.throw('x').catch(function (e) { log.push('rejected' + e); });"
        ),
        "rejectedx"
    );
}

#[test]
fn async_generator_after_completion_answers_done() {
    assert_eq!(
        ok(
            "async function* g() {} var it = g(); \
            it.next().then(function () { return it.next(); }).then(function (r) { log.push(r.value, r.done); });"
        ),
        ",true"
    );
}

#[test]
fn for_await_reads_an_async_generator() {
    assert_eq!(
        ok(
            "async function* g() { yield 1; yield 2; } \
            (async function () { for await (var x of g()) { log.push(x); } })();"
        ),
        "1,2"
    );
}

#[test]
fn for_await_over_a_sync_iterable_awaits_each_value() {
    assert_eq!(
        ok(
            "(async function () { for await (var x of [Promise.resolve('a'), 'b']) { log.push(x); } })();"
        ),
        "a,b"
    );
}

#[test]
fn breaking_out_of_for_await_awaits_return() {
    assert_eq!(
        ok(
            "var iterable = { [Symbol.asyncIterator]() { var i = 0; return { \
                next() { return Promise.resolve({ value: i++, done: false }); }, \
                return() { log.push('return'); return Promise.resolve({ done: true }); } }; } }; \
            (async function () { for await (var x of iterable) { log.push(x); if (x === 1) { break; } } log.push('after'); })();"
        ),
        "0,1,return,after"
    );
}
