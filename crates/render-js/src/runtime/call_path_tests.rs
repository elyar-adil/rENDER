//! Regression tests for the interpreter's call path: function code is shared
//! between calls, frame labels are rendered lazily, and none of that may
//! change observable function or stack behaviour.

use crate::JsRuntime;
use render_html::parse_document;

/// Run `work` on a thread with the large stack the interpreter expects. Deep
/// recursion or a deep expression nests many native frames even when the
/// JavaScript depth is modest.
fn with_runtime<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(work)
        .expect("test thread starts")
        .join()
        .expect("test thread finishes")
}

fn eval(source: &str) -> Result<String, crate::JsError> {
    let source = source.to_owned();
    with_runtime(move || {
        let mut dom = parse_document("<!doctype html><html><body></body></html>").dom;
        let mut runtime = JsRuntime::new(&dom);
        runtime
            .execute(&mut dom, &source)
            .map(|outcome| outcome.value.to_js_string().to_lossy())
    })
}

fn ok(source: &str) -> String {
    eval(source).unwrap_or_else(|error| panic!("{source}\n=> {error}"))
}

#[test]
fn a_large_body_with_defaults_and_patterns_is_callable_many_times() {
    let result = ok(r"
        function work(a, b = a + 1, { c, d = 4 } = {}, ...rest) {
            var t0 = a + b;
            var t1 = t0 * 2;
            var t2 = t1 - c;
            var t3 = t2 + d;
            var t4 = t3 * 3;
            var t5 = t4 - t0;
            var t6 = t5 + t1;
            var t7 = t6 * 2;
            var t8 = t7 - t2;
            var t9 = t8 + t3;
            var t10 = t9 * 3;
            var t11 = t10 - t4;
            var t12 = t11 + t5;
            var t13 = t12 * 2;
            var t14 = t13 - t6;
            var t15 = t14 + t7;
            var t16 = t15 * 3;
            var t17 = t16 - t8;
            var t18 = t17 + t9;
            var t19 = t18 * 2;
            if (rest.length > 1) { t19 = t19 + rest[0] - rest[1]; }
            return t19 & 0x3fffffff;
        }
        var total = 0;
        for (var i = 0; i < 2000; i++) {
            total = (total + work(i, undefined, { c: i * 2 }, 9, 10)) & 0x3fffffff;
        }
        [total, work(1, 2, { c: 3, d: 4 }, 5, 6)].join('|');
        ");
    // The total and the single call were cross-checked against Node.js.
    assert_eq!(result, "136904176|1993");
}

#[test]
fn recursion_and_closures_keep_their_own_state() {
    let result = ok(r"
        function fib(n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); }
        function counter(start) {
            var value = start;
            return function (step) {
                value = value + step;
                return value;
            };
        }
        var first = counter(10);
        var second = counter(100);
        first(1); first(2);
        second(5);
        [fib(15), first(0), second(0)].join(',');
        ");
    assert_eq!(result, "610,13,105");
}

#[test]
fn function_source_and_error_stack_keep_their_labels() {
    let result = ok(r"
        function outer() { return inner(); }
        function inner() { return new Error('boom'); }
        var error = outer();
        var source = outer.toString();
        var stack = error.stack;
        [
            source.indexOf('function outer') >= 0,
            source.indexOf('inner()') >= 0,
            stack.indexOf('at inner') >= 0,
            stack.indexOf('at outer') >= 0,
            stack.split('\n').length >= 3
        ].join(',');
        ");
    assert_eq!(result, "true,true,true,true,true");
}

#[test]
fn anonymous_function_frames_still_get_a_label() {
    let result = ok(r"
        var stack = (function () { return new Error('x').stack; })();
        [stack.indexOf('<anonymous fn #') >= 0, stack.indexOf('at ') >= 0].join(',');
        ");
    assert_eq!(result, "true,true");
}

#[test]
fn generator_and_async_calls_still_start_coroutines() {
    with_runtime(|| {
        let mut dom = parse_document("<!doctype html><html><body></body></html>").dom;
        let mut runtime = JsRuntime::new(&dom);
        runtime
            .execute(
                &mut dom,
                r"
                var log = [];
                function* range(start, stop = start + 2) {
                    for (var i = start; i < stop; i++) { yield i; }
                    return 'done';
                }
                async function add(a, b = 1) { return a + b; }
                var it = range(3);
                log.push(it.next().value, it.next().value, it.next().done);
                add(5).then(function (value) { log.push('async' + value); });
                ",
            )
            .expect("generator and async calls execute");
        drain(&mut runtime, &mut dom);
        let outcome = runtime
            .execute(&mut dom, "log.join(',')")
            .expect("log reads");
        assert_eq!(outcome.value.to_js_string().to_lossy(), "3,4,true,async6");
    });
}

fn drain(runtime: &mut JsRuntime, dom: &mut render_dom::Dom) {
    for _ in 0..100 {
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
