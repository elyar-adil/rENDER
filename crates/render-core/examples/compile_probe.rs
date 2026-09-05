use render_core::js::{CompiledScript, RuntimeLimits};

fn main() {
    let path = std::env::args().nth(1).expect("source path");
    let source = std::fs::read_to_string(path).expect("source");
    match CompiledScript::compile(&source, &RuntimeLimits::default()) {
        Ok(_) => println!("OK"),
        Err(error) => println!("ERR {} at {:?}", error.message(), error.offset()),
    }
}
