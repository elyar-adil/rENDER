//! Built-ins of explicit resource management: `Symbol.dispose`,
//! `Symbol.asyncDispose`, `SuppressedError`, `DisposableStack` and
//! `AsyncDisposableStack` (ECMA-262 explicit resource management). The
//! `using` declaration syntax is not exercised here.

use crate::JsRuntime;
use render_html::parse_document;

/// Run `source`, then drain timers and microtasks, and return `log.join(',')`.
fn run(source: &str) -> String {
    let mut dom = parse_document("<!doctype html><html><body></body></html>").dom;
    let mut runtime = JsRuntime::new(&dom);
    runtime
        .execute(&mut dom, "var log = [];")
        .unwrap_or_else(|error| panic!("setup => {error}"));
    runtime
        .execute(&mut dom, source)
        .unwrap_or_else(|error| panic!("{source}\n=> {error}"));
    drain(&mut runtime, &mut dom);
    let outcome = runtime
        .execute(&mut dom, "log.join(',')")
        .unwrap_or_else(|error| panic!("{source}\n=> {error}"));
    outcome.value.to_js_string()
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
    let mut dom = parse_document("<!doctype html><html><body></body></html>").dom;
    let mut runtime = JsRuntime::new(&dom);
    runtime
        .execute(&mut dom, source)
        .map(|outcome| outcome.value.to_js_string())
        .unwrap_or_else(|error| panic!("{source}\n=> {error}"))
}

#[test]
fn dispose_symbols_are_well_known_and_unregistered() {
    assert_eq!(
        ok(
            "[typeof Symbol.dispose, typeof Symbol.asyncDispose, Symbol.dispose.toString(), Symbol.asyncDispose.description, Symbol.keyFor(Symbol.dispose) === undefined].join()"
        ),
        "symbol,symbol,Symbol(Symbol.dispose),Symbol.asyncDispose,true"
    );
    assert_eq!(
        ok(
            "var d = Object.getOwnPropertyDescriptor(Symbol, 'asyncDispose'); [d.writable, d.enumerable, d.configurable].join()"
        ),
        "false,false,false"
    );
}

#[test]
fn suppressed_error_has_the_specified_shape() {
    assert_eq!(
        ok(
            "[SuppressedError.length, SuppressedError.name, typeof SuppressedError.prototype, Object.getPrototypeOf(SuppressedError.prototype) === Error.prototype, Object.getPrototypeOf(SuppressedError) === Error, SuppressedError.prototype.name, SuppressedError.prototype.message === '', SuppressedError.prototype.hasOwnProperty('error'), SuppressedError.prototype.hasOwnProperty('suppressed')].join()"
        ),
        "3,SuppressedError,object,true,true,SuppressedError,true,false,false"
    );
    assert_eq!(
        ok(
            "var d = Object.getOwnPropertyDescriptor(SuppressedError, 'prototype'); [d.writable, d.enumerable, d.configurable].join()"
        ),
        "false,false,false"
    );
}

#[test]
fn suppressed_error_stores_error_suppressed_and_message() {
    assert_eq!(
        ok(
            "var e = SuppressedError('a', 'b', 42); var n = Object.getOwnPropertyNames(new SuppressedError(1, 2)); [e instanceof SuppressedError, e instanceof Error, e.error, e.suppressed, e.message, Object.keys(e).length, Object.prototype.toString.call(e), n.indexOf('error') === n.indexOf('suppressed') - 1].join('|')"
        ),
        "true|true|a|b|42|0|[object Error]|true"
    );
    assert_eq!(
        ok(
            "var e = new SuppressedError(1, 2); [e.hasOwnProperty('message'), e.message === '', e.error, e.suppressed].join('|')"
        ),
        "false|true|1|2"
    );
    assert_eq!(
        ok(
            "var r; try { new SuppressedError(undefined, undefined, Symbol()); } catch (e) { r = e.name; } r"
        ),
        "TypeError"
    );
}

#[test]
fn suppressed_error_takes_its_prototype_from_new_target() {
    assert_eq!(
        ok(
            "class MyError extends SuppressedError {} var e = new MyError(1, 2); [e instanceof MyError, e instanceof SuppressedError, e.error].join()"
        ),
        "true,true,1"
    );
    assert_eq!(
        ok(
            "var NewTarget = function () {}; NewTarget.prototype = 42; Object.getPrototypeOf(Reflect.construct(SuppressedError, [], NewTarget)) === SuppressedError.prototype"
        ),
        "true"
    );
}

#[test]
fn disposable_stack_has_the_specified_shape() {
    assert_eq!(
        ok(
            "var P = DisposableStack.prototype; var g = Object.getOwnPropertyDescriptor(P, 'disposed'); [DisposableStack.length, DisposableStack.name, P[Symbol.dispose] === P.dispose, P.dispose.name, P.dispose.length, P.use.length, P.use.name, P.defer.length, P.adopt.length, P.adopt.name, P.move.length, Object.prototype.toString.call(P), g.get.name, g.get.length, typeof g.set, Object.getPrototypeOf(P) === Object.prototype].join()"
        ),
        "0,DisposableStack,true,dispose,0,1,use,1,2,adopt,0,[object DisposableStack],get disposed,0,undefined,true"
    );
    assert_eq!(
        ok(
            "var d = Object.getOwnPropertyDescriptor(DisposableStack.prototype, 'dispose'); [typeof DisposableStack, d.writable, d.enumerable, d.configurable, new DisposableStack() instanceof DisposableStack].join()"
        ),
        "function,true,false,true,true"
    );
    assert_eq!(
        ok("var r; try { DisposableStack(); } catch (e) { r = e.name; } r"),
        "TypeError"
    );
}

#[test]
fn async_disposable_stack_has_the_specified_shape() {
    assert_eq!(
        ok(
            "var P = AsyncDisposableStack.prototype; [AsyncDisposableStack.length, AsyncDisposableStack.name, P[Symbol.asyncDispose] === P.disposeAsync, P.disposeAsync.name, P.disposeAsync.length, P.use.length, P.defer.length, P.adopt.length, P.move.length, Object.prototype.toString.call(P), Object.prototype.hasOwnProperty.call(P, Symbol.dispose), typeof P.dispose].join()"
        ),
        "0,AsyncDisposableStack,true,disposeAsync,0,1,1,2,0,[object AsyncDisposableStack],false,undefined"
    );
}

#[test]
fn disposable_stack_disposes_in_reverse_order_and_is_idempotent() {
    assert_eq!(
        ok(
            "var log = []; var s = new DisposableStack(); s.defer(function () { log.push(1); }); s.defer(function () { log.push(2); }); var r = s.dispose(); [log.join(), r === undefined, s.disposed, s.dispose() === undefined, log.join()].join('|')"
        ),
        "2,1|true|true|true|2,1"
    );
}

#[test]
fn disposable_stack_use_adopt_and_defer_register_disposers() {
    assert_eq!(
        ok(
            "var log = []; var s = new DisposableStack(); var res = { [Symbol.dispose]() { log.push('use'); } }; var same = s.use(res) === res; s.use(null); s.use(undefined); s.adopt(7, function (v) { log.push('adopt' + v); }); s.defer(function () { log.push('defer'); }); s.dispose(); [same, log.join()].join('|')"
        ),
        "true|defer,adopt7,use"
    );
    assert_eq!(
        ok(
            "var n = 0; var res = { get [Symbol.dispose]() { n++; return function () {}; } }; var s = new DisposableStack(); s.use(res); s.dispose(); n"
        ),
        "1"
    );
}

#[test]
fn disposable_stack_move_transfers_its_resources() {
    assert_eq!(
        ok(
            "var log = []; class Sub extends DisposableStack {} var a = new Sub(); a.defer(function () { log.push('x'); }); var b = a.move(); [b instanceof DisposableStack, b instanceof Sub, a.disposed, b.disposed, log.length].join()"
        ),
        "true,false,true,false,0"
    );
    assert_eq!(
        ok(
            "var log = []; var a = new DisposableStack(); a.defer(function () { log.push('x'); }); var b = a.move(); a.dispose(); var before = log.length; b.dispose(); before + ':' + log.join()"
        ),
        "0:x"
    );
}

#[test]
fn disposable_stack_nests_errors_in_suppressed_errors() {
    assert_eq!(
        ok(
            "var e1 = new Error('e1'), e2 = new Error('e2'), e3 = new Error('e3'); var s = new DisposableStack(); s.defer(function () { throw e1; }); s.defer(function () { throw e2; }); s.defer(function () { throw e3; }); var out; try { s.dispose(); } catch (e) { out = [e instanceof SuppressedError, e.error === e1, e.suppressed instanceof SuppressedError, e.suppressed.error === e2, e.suppressed.suppressed === e3].join(); } out"
        ),
        "true,true,true,true,true"
    );
    assert_eq!(
        ok(
            "var e = new Error('x'); var s = new DisposableStack(); s.defer(function () {}); s.defer(function () { throw e; }); var out; try { s.dispose(); } catch (c) { out = c === e; } out"
        ),
        "true"
    );
}

#[test]
fn disposable_stack_reports_the_specified_error_types() {
    assert_eq!(
        ok(
            "var out = []; function check(f) { try { f(); out.push('ok'); } catch (e) { out.push(e.name); } } check(function () { DisposableStack.prototype.dispose.call({}); }); check(function () { DisposableStack.prototype.use.call(new AsyncDisposableStack(), null); }); check(function () { new DisposableStack().use(1); }); check(function () { new DisposableStack().use({}); }); check(function () { new DisposableStack().defer(1); }); check(function () { new DisposableStack().adopt(1); }); var s = new DisposableStack(); s.dispose(); check(function () { s.use(null); }); check(function () { s.defer(function () {}); }); check(function () { s.adopt(1, function () {}); }); check(function () { s.move(); }); check(function () { s.dispose(); }); out.join()"
        ),
        "TypeError,TypeError,TypeError,TypeError,TypeError,TypeError,ReferenceError,ReferenceError,ReferenceError,ReferenceError,ok"
    );
}

#[test]
fn disposable_stack_use_validates_the_dispose_method() {
    assert_eq!(
        ok(
            "var out = []; function check(v) { try { new DisposableStack().use(v); out.push('ok'); } catch (e) { out.push(e.name); } } check({ [Symbol.dispose]: null }); check({ [Symbol.dispose]: 1 }); check({ [Symbol.dispose]: function () {} }); out.join()"
        ),
        "TypeError,TypeError,ok"
    );
}

#[test]
fn disposable_stack_subclass_instances_keep_their_prototype() {
    assert_eq!(
        ok(
            "class Sub extends DisposableStack {} var s = new Sub(); [s instanceof Sub, s instanceof DisposableStack, Object.getPrototypeOf(s.move()) === DisposableStack.prototype].join()"
        ),
        "true,true,true"
    );
}

#[test]
fn async_disposable_stack_awaits_each_disposer_before_settling() {
    // `use(null)` still takes an await, so the disposal settles after one job.
    assert_eq!(
        run(
            "var s = new AsyncDisposableStack(); s.use(null); Promise.resolve().then(function () {}).then(function () { log.push('job 1'); }); s.disposeAsync().then(function () { log.push('dispose'); }); Promise.resolve().then(function () {}).then(function () { log.push('job 2'); });"
        ),
        "job 1,dispose,job 2"
    );
    // With nothing to dispose there is no await.
    assert_eq!(
        run(
            "var s2 = new AsyncDisposableStack(); Promise.resolve().then(function () { log.push('job 1'); }); s2.disposeAsync().then(function () { log.push('dispose'); }); Promise.resolve().then(function () { log.push('job 2'); });"
        ),
        "job 1,dispose,job 2"
    );
}

#[test]
fn async_disposable_stack_runs_disposers_in_reverse_and_waits_for_each() {
    assert_eq!(
        run(
            "var s = new AsyncDisposableStack(); s.defer(async function () { log.push('a-start'); await null; log.push('a-end'); }); s.defer(function () { log.push('b'); return Promise.resolve().then(function () { log.push('b-later'); }); }); s.disposeAsync().then(function () { log.push('done'); });"
        ),
        "b,b-later,a-start,a-end,done"
    );
}

#[test]
fn async_disposable_stack_rejects_with_suppressed_errors() {
    assert_eq!(
        run(
            "var e1 = new Error('1'), e2 = new Error('2'); var s = new AsyncDisposableStack(); s.defer(function () { return Promise.reject(e1); }); s.defer(function () { throw e2; }); s.disposeAsync().then(function () { log.push('ok'); }, function (e) { log.push(e.name + ':' + (e.error === e1) + ':' + (e.suppressed === e2)); });"
        ),
        "SuppressedError:true:true"
    );
    assert_eq!(
        run(
            "var e = new Error('solo'); var s = new AsyncDisposableStack(); s.defer(function () { return Promise.reject(e); }); s.disposeAsync().then(function () {}, function (r) { log.push(r === e); });"
        ),
        "true"
    );
}

#[test]
fn async_disposable_stack_dispose_async_is_idempotent() {
    assert_eq!(
        run(
            "var n = 0; var s = new AsyncDisposableStack(); s.defer(function () { n++; }); var p1 = s.disposeAsync(); var p2 = s.disposeAsync(); Promise.all([p1, p2]).then(function (v) { log.push(n, v[0] === undefined, v[1] === undefined, s.disposed); });"
        ),
        "1,true,true,true"
    );
}

#[test]
fn async_disposable_stack_reports_a_foreign_receiver_by_rejecting() {
    assert_eq!(
        run(
            "AsyncDisposableStack.prototype.disposeAsync.call({}).then(function () { log.push('resolved'); }, function (e) { log.push(e.name); });"
        ),
        "TypeError"
    );
}

#[test]
fn async_use_prefers_async_dispose_and_falls_back_to_dispose() {
    assert_eq!(
        run(
            "var s = new AsyncDisposableStack(); s.use({ get [Symbol.asyncDispose]() { log.push('get async'); return undefined; }, get [Symbol.dispose]() { log.push('get sync'); return function () { log.push('sync called'); }; } }); s.disposeAsync().then(function () { log.push('done'); });"
        ),
        "get async,get sync,sync called,done"
    );
    assert_eq!(
        ok(
            "var out = []; function check(v) { try { new AsyncDisposableStack().use(v); out.push('ok'); } catch (e) { out.push(e.name); } } check({ [Symbol.asyncDispose]: null }); check({ [Symbol.asyncDispose]: 1 }); check({}); check(null); out.join()"
        ),
        "TypeError,TypeError,TypeError,ok"
    );
}
