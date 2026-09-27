# rENDER — Minimal Browser (Rust)

**Goal**: A lean, correct, usable browser built from scratch in Rust.
No Chromium, no system WebView. Every line must earn its place.

## Architecture

```
crates/render-dom      → DOM tree, namespaces, revisions, mutation journal
crates/render-html     → HTML5 tokenizer, tree builder, encoding sniffing
crates/render-css      → stylesheet parsing, selectors, cascade, typed values,
                          computed values
crates/render-layout   → CSS formatting structure + solver (block, inline, flex,
                          grid, table) → immutable fragments
crates/render-js       → JS lexer, parser, tree-walking interpreter, Web API
                          builtins per domain, video module
crates/render-core     → page session: load → style → layout → paint pipeline,
                          image decode, display list, CPU rasterization,
                          interaction/hit testing, navigation, event loop,
                          capability registry
  src/paint/            display list construction and CPU rasterization
  src/image/            image and SVG decoders
  src/interaction/      hit testing and content interaction
  src/spec/             declared capability registry (see docs/visual_fidelity_gaps.md)
crates/render-net      → bounded HTTP(S) transport (ureq + rustls)
crates/render-browser  → native desktop shell (winit + softbuffer)
                         font backend, resource scheduling, tab chrome,
                         private memory/disk cache and cache settings
```

The engine crates are layered: `render-dom` knows nothing about CSS, `render-css`
knows nothing about layout, and `render-layout` knows nothing about painting.
`render-core` is the only crate that composes them. When a fix seems to require a
layer to reach sideways, that is usually the signal that a seam is in the wrong place.

## Capability gaps

`docs/visual_fidelity_gaps.md` records the verified list of capabilities that are
parsed-but-not-consumed, computed-but-dropped, or absent entirely, with `file:line`
evidence. Read it before concluding that a property "is supported" - a property
reaching the computed style map is not the same as a property having a consumer in
layout or paint. See also `docs/generic-browser-todo.md`, whose "Priority -1" section
states the rule that a gap gets implemented forward and never removed.

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
- WPT reftests (`crates/render-core/tests/wpt_reftests.rs) run against a pinned WPT checkout fetched by `tools/fetch-wpt.ps1, configured through `RENDER_WPT_* env vars.

The test262 gate is expensive: a full `cargo test --workspace` or a bare
`cargo test -p render-core` runs it and takes about eight minutes, during which it
holds the build lock and starves every other workspace. While iterating, scope to
`cargo test -p <crate> --lib` or name the test targets explicitly, and run
`cargo test -p render-core --test test262` on its own when the gate is the point.

## Required checks before finishing any change

Run them all with `tools/check.sh (git bash), or individually:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
python tools/check_site_neutrality.py
cargo test --workspace
```

`tools/check_site_neutrality.py` enforces the no-per-site-branches rule from
`docs/generic-browser-todo.md` mechanically. A real commercial domain in a
comparison, match arm, or substring test is a failure; a domain used as inert
test data is not. A genuine exception needs a `// site-neutral: <reason>`
comment on the line or the two lines above it.

## Design Principles

- **Minimal**: solve the problem with the least code that works correctly
- **No third-party engines or WebViews**; small audited crates only
- **Correctness over completeness**: implement features fully or not at all
- **Delete before adding**: prefer removing dead code to working around it
- Standards are the authority: WHATWG/CSS/ECMAScript specs, WPT, and test262 outrank intuition; no legacy implementation is kept as reference
- All required checks above must pass before a change is considered done
