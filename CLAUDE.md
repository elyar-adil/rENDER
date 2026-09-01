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
                         private memory/disk cache and cache settings
```

## Running

```bash
cargo run --release -p render-browser                          # built-in new-tab page
cargo run --release -p render-browser -- example/index.html    # local file
cargo run --release -p render-browser -- https://example.com   # fetch URL
```

## Performance measurement

```bash
cargo run --release -p render-browser --bin render-perf -- --fixture generated --iterations 20
```

`render-perf` is a headless deterministic HTML-to-pixels benchmark. It emits
JSON timing distributions for parse, first render, first visible output, and
scroll renders; use `--fixture all` for the repository fixtures. It deliberately
does not measure network, cache, native-window presentation, or JS execution.
PRs run the generated-fixture smoke command in `.github/workflows/perf.yml`;
scheduled and manually dispatched runs benchmark all fixtures. Compare release
builds on the same machine and require a recorded baseline before evaluating
the project target of at least 30% lower `first_visible` p95.

The browser-side HTTP cache is intentionally conservative: a 32 MiB private
memory LRU stores only explicitly fresh anonymous responses, and stale entries
carry `ETag`/`Last-Modified` validators for conditional revalidation. A bounded
512 MiB disk store and generation-safe clear operation run on a dedicated I/O
worker; persistence read-through/write-back is kept behind that boundary while
the page and rendering contracts continue to change.

## Performance invariants

- Reserve one logical CPU for the event loop and operating system when sizing
  network and render workers, with a bounded upper cap.
- Give the active tab the larger script-turn budget; background tabs remain
  bounded and cannot starve foreground rendering.
- Submit immutable render inputs, cancel superseded work, and commit only the
  latest tab/revision identity.
- Present only damage regions when the native surface can preserve its buffer;
  resize, first-frame, and uncertain buffer-age paths fall back to a full copy.

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
