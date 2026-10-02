//! Evaluates each non-empty line of a file as its own script in one realm and
//! prints the result: `cargo run -p render-js --example call_probe -- lines.txt`.
use render_html::parse_document;
use render_js::JsRuntime;
fn main() {
    let mut dom =
        parse_document("<!doctype html><html><body><div id=a><p id=b>x</p></div></body></html>")
            .dom;
    let mut runtime = JsRuntime::new(&dom);
    let cases = std::fs::read_to_string(std::env::args().nth(1).expect("file")).expect("read");
    for source in cases.lines().filter(|line| !line.trim().is_empty()) {
        let result = runtime.execute(&mut dom, source).map_or_else(
            |error| format!("ERR {error}"),
            |outcome| outcome.value.to_js_string(),
        );
        println!("{source:110} => {result}");
    }
}
