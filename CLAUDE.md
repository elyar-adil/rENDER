# rENDER — Minimal Browser (Rust)

**Goal**: A lean, correct, usable browser built from scratch in Rust.
No Chromium, no system WebView. Every line must earn its place.

## Architecture

```
crates/render-core      → engine library: DOM, CSS, JS, layout, painting
  src/html/               HTML5 tokenizer + tree builder
  src/dom.rs              Document/Element/Text tree
  src/css/                stylesheet parsing, selectors, cascade, computed values
  src/js/                 lexer, parser, interpreter, builtins, event loop
  src/layout/             block/inline/flex/grid/table formatting contexts
  src/paint/              display list construction and CPU rasterization
  src/page.rs             page session: load → style → layout → paint pipeline
crates/render-net       → bounded HTTP(S) transport (ureq + rustls)
crates/render-browser   → native desktop shell (winit + softbuffer)
```

## Running

```bash
cargo run -p render-browser                          # built-in new-tab page
cargo run -p render-browser -- example/index.html    # local file
cargo run -p render-browser -- https://example.com   # fetch URL
```

## Tests

```bash
cargo test --workspace
```

- `third_party/test262` (pin via `tools/fetch-test262.sh`) drives the JS conformance runner in `crates/render-core/tests/test262.rs`.
- WPT reftests (`crates/render-core/tests/wpt_reftests.rs`) run against a pinned WPT checkout fetched by `tools/fetch-wpt.ps1`, configured through `RENDER_WPT_*` env vars.

## Required checks before finishing any change

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Design Principles

- **Minimal**: solve the problem with the least code that works correctly
- **No third-party engines or WebViews**; small audited crates only
- **Correctness over completeness**: implement features fully or not at all
- **Delete before adding**: prefer removing dead code to working around it
- Standards are the authority: WHATWG/CSS/ECMAScript specs, WPT, and test262 outrank intuition; no legacy implementation is kept as reference
- All required checks above must pass before a change is considered done
