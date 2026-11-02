//! WHATWG `ReadableStream` default streams: controller, reader, backpressure,
//! cancellation, `tee` and async iteration.

use crate::JsRuntime;
use render_html::parse_document;

/// Run `source`, drain the script and microtask queues, and return `log`.
fn run(source: &str) -> String {
    let mut dom = parse_document("<!doctype html><html><body></body></html>").dom;
    let mut runtime = JsRuntime::new(&dom);
    runtime
        .execute(&mut dom, "var log = [];")
        .expect("the log declaration runs");
    runtime
        .execute(&mut dom, source)
        .unwrap_or_else(|error| panic!("{source}\n=> {error}"));
    for _ in 0..200 {
        let microtasks = runtime.take_pending_microtasks();
        let mut progressed = !microtasks.is_empty();
        for microtask in microtasks {
            runtime
                .invoke_microtask(&mut dom, microtask)
                .expect("microtask executes");
        }
        let timers = runtime.take_pending_timer_requests();
        progressed |= !timers.is_empty();
        for timer in timers {
            if let crate::TimerRequest::Schedule { id, .. } = timer {
                let _ = runtime.fire_timer(&mut dom, id);
            }
        }
        if !progressed {
            break;
        }
    }
    let outcome = runtime
        .execute(&mut dom, "log.join(',')")
        .expect("log reads");
    assert!(runtime.prelude_error().is_none());
    outcome.value.to_js_string().to_lossy()
}

fn ok(source: &str) -> String {
    run(source)
}

/// Evaluate `source` and return its completion value (not `log`).
fn eval_value(source: &str) -> String {
    let mut dom = parse_document("<!doctype html><html><body></body></html>").dom;
    let mut runtime = JsRuntime::new(&dom);
    let outcome = runtime
        .execute(&mut dom, source)
        .unwrap_or_else(|error| panic!("{source}\n=> {error}"));
    assert!(runtime.prelude_error().is_none());
    outcome.value.to_js_string().to_lossy()
}

fn throws_name(expression: &str) -> String {
    eval_value(&format!(
        "var r; try {{ {expression}; r = 'no error' }} catch (e) {{ r = e.name }} r"
    ))
}

#[test]
fn constructor_validation_uses_type_and_range_errors() {
    assert_eq!(
        throws_name("new ReadableStream({}, { highWaterMark: -1 })"),
        "RangeError"
    );
    assert_eq!(
        throws_name("new ReadableStream({ type: 'bytes' })"),
        "TypeError"
    );
    assert_eq!(throws_name("new ReadableStream({ start: 1 })"), "TypeError");
    assert_eq!(throws_name("ReadableStream()"), "TypeError");
}

#[test]
fn a_start_callback_that_enqueues_then_closes_delivers_chunks_in_order() {
    assert_eq!(
        ok(r"
            var rs = new ReadableStream({ start: function (c) { c.enqueue('x'); c.enqueue('y'); c.close(); } });
            var reader = rs.getReader();
            reader.read().then(function (r) { log.push('1:' + r.value + ':' + r.done); return reader.read(); })
              .then(function (r) { log.push('2:' + r.value + ':' + r.done); return reader.read(); })
              .then(function (r) { log.push('3:' + r.value + ':' + r.done); });
            "),
        "1:x:false,2:y:false,3:undefined:true"
    );
}

#[test]
fn a_pull_based_stream_respects_backpressure_and_delivers_in_order() {
    assert_eq!(
        ok(r"
            var pulls = 0;
            var rs = new ReadableStream({
              start: function (c) { window.firstDesired = c.desiredSize; },
              pull: function (c) {
                pulls += 1;
                c.enqueue('v' + pulls);
                if (pulls === 3) c.close();
              }
            }, { highWaterMark: 1 });
            var reader = rs.getReader();
            reader.read().then(function (r) { log.push(r.value); return reader.read(); })
              .then(function (r) { log.push(r.value); return reader.read(); })
              .then(function (r) { log.push(r.value); return reader.read(); })
              .then(function (r) { log.push(r.done + ':' + pulls); });
            "),
        "v1,v2,v3,true:3"
    );
    assert_eq!(
        ok(r"
            var requested = [];
            var rs = new ReadableStream({
              pull: function (c) { requested.push(c.desiredSize); c.enqueue(requested.length); }
            }, { highWaterMark: 2 });
            rs.getReader().read().then(function () { log.push(requested.join('/')); });
            "),
        "2"
    );
}

#[test]
fn desired_size_accounts_for_queued_chunk_sizes() {
    assert_eq!(
        ok(r"
            var controller;
            var rs = new ReadableStream({
              start: function (c) { controller = c; c.enqueue('a'); c.enqueue('b'); }
            }, { highWaterMark: 3, size: function (chunk) { return chunk.length; } });
            var before = controller.desiredSize;
            var reader = rs.getReader();
            reader.read().then(function () { log.push(before + ':' + controller.desiredSize); });
            "),
        "1:2"
    );
}

#[test]
fn errors_reject_reads_and_the_closed_promise() {
    assert_eq!(
        ok(r"
            var controller;
            var rs = new ReadableStream({ start: function (c) { controller = c; } });
            var reader = rs.getReader();
            var reason = new Error('boom');
            reader.read().then(function () { log.push('resolved'); }, function (error) { log.push('read:' + error.message); });
            reader.closed.then(function () { log.push('closed'); }, function (error) { log.push('closed:' + error.message); });
            controller.error(reason);
            "),
        "read:boom,closed:boom"
    );
    assert_eq!(
        ok(r"
            var controller = null;
            var rs = new ReadableStream({ start: function (c) { controller = c; c.close(); } });
            try { controller.enqueue('late'); } catch (e) { log.push(e.name); }
            try { controller.close(); } catch (e) { log.push(e.name); }
            "),
        "TypeError,TypeError"
    );
}

#[test]
fn a_pending_read_is_resolved_by_a_later_enqueue() {
    assert_eq!(
        ok(r"
            var controller;
            var rs = new ReadableStream({ start: function (c) { controller = c; } });
            var reader = rs.getReader();
            reader.read().then(function (r) { log.push(r.value + ':' + r.done); });
            log.push('pending');
            controller.enqueue('late');
            "),
        "pending,late:false"
    );
}

#[test]
fn releasing_a_lock_allows_a_new_reader_and_rejects_old_ones() {
    assert_eq!(
        ok(r"
            var rs = new ReadableStream({ start: function (c) { c.enqueue('x'); c.close(); } });
            var first = rs.getReader();
            log.push(rs.locked);
            first.releaseLock();
            log.push(rs.locked);
            first.read().then(function () { log.push('read'); }, function (e) { log.push('read:' + e.name); });
            var second = rs.getReader();
            second.read().then(function (r) { log.push('second:' + r.value); });
            try { rs.getReader(); } catch (e) { log.push(e.name); }
            "),
        "true,false,TypeError,read:TypeError,second:x"
    );
}

#[test]
fn for_await_uses_the_async_iterator_protocol() {
    assert_eq!(
        ok(r"
            var rs = new ReadableStream({ start: function (c) { c.enqueue('a'); c.enqueue('b'); c.close(); } });
            (async function () {
              var seen = [];
              for await (var chunk of rs) seen.push(chunk);
              log.push(seen.join(''));
              log.push(rs.locked);
            })();
            "),
        "ab,false"
    );
}

#[test]
fn tee_delivers_the_same_chunks_to_both_branches() {
    assert_eq!(
        ok(r"
            var rs = new ReadableStream({ start: function (c) { c.enqueue('a'); c.enqueue('b'); c.enqueue('c'); c.close(); } });
            var pair = rs.tee();
            var left = pair[0].getReader();
            var right = pair[1].getReader();
            function readAll(reader, name) {
              var values = [];
              function step() {
                return reader.read().then(function (r) {
                  if (r.done) { log.push(name + ':' + values.join('')); return; }
                  values.push(r.value);
                  return step();
                });
              }
              return step();
            }
            readAll(left, 'left');
            readAll(right, 'right');
            "),
        "left:abc,right:abc"
    );
}

#[test]
fn cancel_closes_pending_reads_and_calls_the_underlying_cancel() {
    assert_eq!(
        ok(r"
            var canceled = null;
            var rs = new ReadableStream({
              start: function (c) { window.controller = c; },
              cancel: function (reason) { canceled = reason; log.push('cancel:' + reason); }
            });
            var reader = rs.getReader();
            reader.read().then(function (r) { log.push('read:' + r.done); });
            reader.cancel('stop').then(function () { log.push('canceled:' + canceled); });
            "),
        "cancel:stop,read:true,canceled:stop"
    );
}

#[test]
fn the_x_stream_shape_enqueues_from_an_external_listener_without_loss() {
    assert_eq!(
        ok(r"
            var listener = null;
            function makeStream() {
              return new ReadableStream({
                start: function (controller) {
                  listener = {
                    next: function (value) { controller.enqueue(value); },
                    error: function (value) { controller.error(value); },
                    done: function () { controller.close(); }
                  };
                }
              });
            }
            var reader = makeStream().getReader();
            listener.next({ kind: 1 });
            listener.next({ kind: 2 });
            reader.read().then(function (a) { log.push('a' + a.value.kind); return reader.read(); })
              .then(function (b) { log.push('b' + b.value.kind); listener.done(); return reader.read(); })
              .then(function (c) { log.push('done:' + c.done); });
            "),
        "a1,b2,done:true"
    );
}
