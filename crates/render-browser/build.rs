fn main() {
    // The interpreter's frames are large. On macOS the browser owns the main
    // thread (winit/AppKit requires it), whose default stack is 8 MiB, and a
    // deep but ordinary page recursion can exhaust it before the interpreter's
    // 4096-frame limit. Link the executable with a 256 MiB main-thread stack,
    // matching the dedicated thread the other platforms run the event loop on.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-Wl,-stack_size,0x10000000");
    }
}
