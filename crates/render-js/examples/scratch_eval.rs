//! TEMPORARY diagnostic probe: evaluate a JS file and print result/error.
//! Delete before handoff.
use render_html::parse_document;
use render_js::{CompiledScript, JsRuntime, RuntimeLimits};

fn main() {
    std::thread::Builder::new()
        .stack_size(512 * 1024 * 1024)
        .spawn(run)
        .unwrap()
        .join()
        .unwrap();
}

fn run() {
    let path = std::env::args().nth(1).expect("source path");
    let source = std::fs::read_to_string(&path).unwrap();
    let mut parsed = parse_document("<!doctype html><p>probe</p>");
    let mut runtime = JsRuntime::new(&parsed.dom);
    match CompiledScript::compile(&source, &RuntimeLimits::default()) {
        Ok(script) => match runtime.execute_compiled(&mut parsed.dom, &script) {
            Ok(outcome) => println!("OK {:?}", outcome.value),
            Err(error) => println!(
                "ERR kind={:?} message={:?} display={}",
                error.kind(),
                error.message(),
                error
            ),
        },
        Err(error) => println!("COMPILE ERR kind={:?} {}", error.kind(), error),
    }
}
