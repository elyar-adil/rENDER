//! Synthetic throughput benchmark for the interpreter's call paths.
//!
//! Run with `cargo run --release -p render-js --example runtime_bench`.
//! Each case runs in a fresh script execution and reports the interpreter
//! steps the runtime consumed and the wall time, so the ratio is directly
//! comparable with the `RENDER_JS_STEPS` diagnostics on a real bundle.
//! Pass a saved script path to measure one real bundle through the same path:
//! `cargo run --release -p render-js --example runtime_bench -- bundle.js`.
#![allow(
    clippy::cast_precision_loss,
    reason = "the benchmark prints step counts as f64 rates"
)]

use std::time::Instant;

use render_html::parse_document;
use render_js::JsRuntime;

fn main() {
    std::thread::Builder::new()
        .stack_size(512 * 1024 * 1024)
        .spawn(run)
        .unwrap()
        .join()
        .unwrap();
}

fn run() {
    if let Some(path) = std::env::args().nth(1) {
        run_bundle(&path);
        return;
    }
    for (name, source) in cases() {
        let mut parsed = parse_document("<!doctype html><p>bench</p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        // Warm the prelude and the realm once, so only the workload is timed.
        runtime
            .execute(&mut parsed.dom, "0;")
            .expect("warmup script");
        let started = Instant::now();
        let outcome = runtime.execute(&mut parsed.dom, source);
        let elapsed = started.elapsed();
        match outcome {
            Ok(outcome) => {
                let steps = runtime.steps_consumed();
                let seconds = elapsed.as_secs_f64();
                println!(
                    "{name:16} steps={steps:9} time={seconds:8.3}s rate={:8.0} steps/s result={:?}",
                    steps as f64 / seconds,
                    outcome.value
                );
            }
            Err(error) => println!("{name:16} ERROR {error}"),
        }
    }
}

/// Run one saved script file as a classic script. This is how the real-bundle
/// numbers were measured: the same `execute` path the browser shell uses,
/// with the interpreter's own step count as the work metric.
fn run_bundle(path: &str) {
    let source = std::fs::read_to_string(path).expect("bundle reads");
    let mut parsed = parse_document("<!doctype html><p>bench</p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    runtime
        .execute(&mut parsed.dom, "0;")
        .expect("warmup script");
    let started = Instant::now();
    let outcome = runtime.execute(&mut parsed.dom, &source);
    let elapsed = started.elapsed();
    let steps = runtime.steps_consumed();
    let seconds = elapsed.as_secs_f64();
    println!(
        "{path} steps={steps} time={seconds:.3}s rate={:.0} steps/s",
        steps as f64 / seconds
    );
    match outcome {
        Ok(outcome) => println!("result={:?}", outcome.value),
        Err(error) => println!("error={error}"),
    }
}

fn cases() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "small_calls",
            r"
            function add3(a, b, c) {
                var x = a + b;
                var y = x + c;
                if (y > 1000000) { y = y - 1000000; }
                return y;
            }
            var total = 0;
            for (var i = 0; i < 60000; i++) {
                total = add3(total, i, 3);
            }
            total;
            ",
        ),
        (
            "closure_calls",
            r"
            function make(seed) {
                var state = seed;
                return function step(amount, flag) {
                    state = state + amount;
                    if (flag) { state = state ^ 1; }
                    return state;
                };
            }
            var total = 0;
            for (var j = 0; j < 60000; j++) {
                var f = make(j);
                total = total + f(2, j & 1);
            }
            total;
            ",
        ),
        (
            "large_body",
            r"
            function dispatch(kind, value, extra, unused) {
                var a = value + extra;
                var b = a * 2 - kind;
                var c = b + a - extra;
                var d = c * 3 + kind;
                var e = d - b + value;
                var f = e + c - a;
                var g = f * 2 + d;
                var h = g - e + b;
                var i = h + f - c;
                var j = i * 2 + g;
                var k = j - h + d;
                var l = k + i - e;
                var m = l * 2 + j;
                var n = m - k + f;
                var o = n + m - g;
                var p = o * 2 + n;
                if (kind === 0) { a = a + 1; }
                else if (kind === 1) { b = b + 1; }
                else if (kind === 2) { c = c + 1; }
                else { d = d + 1; }
                return a + b + c + d + e + f + g + h + i + j + k + l + m + n + o + p;
            }
            var total = 0;
            for (var i2 = 0; i2 < 40000; i2++) {
                total = (total + dispatch(i2 & 3, i2, 7, 0)) & 0x7fffffff;
            }
            total;
            ",
        ),
        (
            "object_churn",
            r"
            function Point(x, y) { this.x = x; this.y = y; }
            Point.prototype.length = function () {
                return Math.sqrt(this.x * this.x + this.y * this.y);
            };
            var total = 0;
            for (var n = 0; n < 40000; n++) {
                var point = new Point(n, n + 1);
                total = total + point.length();
            }
            total;
            ",
        ),
    ]
}
