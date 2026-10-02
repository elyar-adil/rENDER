//! Times the first script (which installs the prelude) against a second one,
//! and prints the prelude's error, if it had one.
use render_html::parse_document;
use render_js::JsRuntime;
use std::time::Instant;

fn main() {
    let mut dom = parse_document("<!doctype html><html><body></body></html>").dom;
    let started = Instant::now();
    let mut runtime = JsRuntime::new(&dom);
    let created = started.elapsed();
    let _ = runtime.execute(&mut dom, "0");
    let first = started.elapsed().saturating_sub(created);
    let second_start = Instant::now();
    let _ = runtime.execute(&mut dom, "0");
    let second = second_start.elapsed();
    println!(
        "new runtime: {created:?}, first script (prelude): {first:?}, second script: {second:?}"
    );
    // A second runtime on the same thread reuses the parsed prelude.
    let mut second_dom = parse_document("<!doctype html><html><body></body></html>").dom;
    let mut other = JsRuntime::new(&second_dom);
    let other_start = Instant::now();
    let _ = other.execute(&mut second_dom, "0");
    println!("second runtime, first script: {:?}", other_start.elapsed());
    match runtime.prelude_error() {
        Some(error) => println!("prelude failed: {error}"),
        None => println!("prelude ok"),
    }
}
