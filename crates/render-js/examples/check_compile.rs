//! Compile-only probe for a captured bundle: reports whether the compiler
//! accepts a file, and where it stops.
//!
//! Usage: `cargo run -p render-js --example check_compile -- <bundle.js>`
//!
//! This exists because compile failure and runtime failure are different
//! problems. When a production bundle stops the engine, the first question is
//! which one it is, and this answers that question in isolation: it never
//! executes the source, so a bundle that parses but throws at run time still
//! prints `ok <bytes>`.
//!
//! A real bundle recurses deeply in the parser, so it runs on a dedicated
//! big-stack thread.

use render_js::{CompiledScript, RuntimeLimits};
use std::fs;

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: check_compile <bundle.js>");
        return;
    };
    let Ok(source) = fs::read_to_string(&path) else {
        eprintln!("cannot read {path}");
        return;
    };
    let bytes = source.len();
    std::thread::Builder::new()
        .stack_size(512 * 1024 * 1024)
        .spawn(
            move || match CompiledScript::compile(&source, &RuntimeLimits::default()) {
                Ok(compiled) => println!("ok {}", compiled.source().len()),
                Err(error) => println!("err kind={:?} {error}", error.kind()),
            },
        )
        .expect("spawn")
        .join()
        .expect("join");
    println!("read {bytes} bytes from {path}");
}
