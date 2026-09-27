use render_js::{CompiledScript, RuntimeLimits};
use std::fs;
fn main() {
    std::thread::Builder::new()
        .stack_size(512 * 1024 * 1024)
        .spawn(|| {
            let s = fs::read_to_string(".diag/bilibili/video.js").unwrap();
            match CompiledScript::compile(&s, &RuntimeLimits::default()) {
                Ok(c) => println!("ok {}", c.source().len()),
                Err(e) => println!("err kind={:?} {}", e.kind(), e),
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
