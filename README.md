# rENDER

`rENDER` is a self-contained browser engine written in Rust.
It does not delegate HTML, CSS, JavaScript, layout, or painting to Chromium or a
system WebView. Parsing, cascade, layout, and painting live in this repository
and are readable end to end.

## Current Completion Snapshot

**As of 2026-09-01.** These are engineering estimates for the browser reaching
the project's minimum usable scope. They are not WPT, test262, or
web-compatibility pass rates; `100%` means the currently planned browser scope
is implemented and covered well enough to maintain.

| Component | Completion | Current boundary |
| --- | ---: | --- |
| HTML parsing and encoding | 55% | Tokenization, tree construction, recovery, and common Chinese encodings work; full HTML5 edge cases remain. |
| DOM, events, and forms | 40% | Tree mutation, selectors, common events, inputs, and GET submission work; broad Web APIs and event options remain. |
| CSS syntax, selectors, and cascade | 45% | Common selectors, specificity, inheritance, custom properties, and major value grammars work; full CSS syntax and cascade coverage remain. |
| Layout | 48% | Block/inline, floats, positioned boxes, flex, grid, table, overflow, and common sizing work; intrinsic and multi-axis edge cases remain. |
| Painting and images | 42% | CPU display lists, backgrounds, borders, clipping, opacity, transforms, and common raster images work; stacking, replaced-element, and SVG coverage remain. |
| JavaScript runtime | 15% | Common script execution, DOM mutation, promises, timers, and events work; the latest full test262 run is 11,628/98,096 variants passed (11.9%). |
| Network and resources | 50% | TLS HTTP(S), redirects, cookies, gzip/Brotli, CSS, scripts, images, and common lazy-image sources work; bounded workers and a conservative private HTTP cache are active, while Fetch/CORS and service workers remain. |
| Browser shell and interaction | 55% | Native window, tabs, address editing, history, scrolling, links, forms, and DPI-aware painting work; accessibility and broader input remain. |
| **Overall minimum usable browser** | **42%** | Enough infrastructure exists for iterative real-site compatibility work; this is not a claim of general web compatibility. |

## Quick Start

The native browser opens a window and presents the CPU surface produced by
`render-core`. With no argument it displays the built-in new-tab page:

```bash
cargo run --release -p render-browser
```

To open a local HTML file or a network page directly:

```bash
cargo run --release -p render-browser -- example/index.html
cargo run --release -p render-browser -- https://example.com/
```

The native chrome provides tabs, an editable address bar, history controls,
window controls, dark-theme colors, and fractional-DPI painting. HTTP/HTTPS
documents load on background workers; HTML encoding detection and external
stylesheets flow into the same DOM/style/layout/paint pipeline.

## Performance Measurements

Use the headless, deterministic `render-perf` binary to measure the stable
HTML-to-pixels pipeline without opening a window or accessing the network. It
emits one JSON document suitable for saving as a CI artifact:

```bash
cargo run --release -p render-browser --bin render-perf -- --fixture generated --iterations 20
cargo run --release -p render-browser --bin render-perf -- --fixture all --iterations 20 > perf.json
```

The report separates HTML parsing, first render, end-to-end first-visible work,
and repeated scroll renders. It deliberately excludes network, cache, native
presentation, and JavaScript execution so evolving web-compatibility work does
not invalidate renderer baselines. Pull requests run a small generated-fixture
smoke benchmark; scheduled and manually dispatched workflow runs cover all
fixtures and retain the JSON report as an artifact. Record a release-build
baseline before comparing changes; the current performance target is at least
30% lower `first_visible` p95 on the same machine, not an already-claimed
result.

The browser also keeps a conservative private HTTP cache (32 MiB memory
budget) for explicitly fresh anonymous responses and revalidates stale entries
with `ETag`/`Last-Modified`. The 512 MiB disk-cache store, checksummed atomic
records, generation-safe clearing, and settings-page status are hosted on a
dedicated I/O worker; browser resource read-through/write-back remains a
staged integration boundary while the renderer continues to evolve.

## Development

Run the required checks before finishing any change:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Conformance suites:

- `third_party/test262` (fetched by `tools/fetch-test262.sh`) drives the
  JavaScript conformance runner in `crates/render-core/tests/test262.rs`.
- WPT reftests (`crates/render-core/tests/wpt_reftests.rs`) run against a
  pinned WPT checkout fetched by `tools/fetch-wpt.ps1`, configured through
  `RENDER_WPT_*` environment variables.

## Repository Layout

```text
crates/render-core        Engine library: HTML, DOM, CSS, JS, layout, painting
crates/render-net         Bounded HTTP(S) transport (ureq + rustls)
crates/render-browser     Native desktop shell (winit + softbuffer)
example/                  Local HTML fixtures for manual testing
docs/                     Notes and compatibility analysis
tools/                    Pinned conformance-suite fetch scripts
third_party/              Vendored test262 checkout
```

## Architecture

1. `render-net` fetches documents and subresources over bounded HTTP(S).
2. `render-core` parses HTML into a DOM tree.
3. Stylesheets are parsed, cascaded, and computed into style trees.
4. The page session executes supported JavaScript against the DOM and keeps
   timers, promises, and invalidation alive after load.
5. Layout builds boxes for block, inline, float, flex, grid, table, and
   positioned flows; painting emits a display list rasterized on the CPU.
6. `render-browser` presents that surface in a native window with tabs,
   navigation, and input handling.

Standards are the authority: WHATWG/CSS/ECMAScript specifications, WPT, and
test262 outrank intuition. There is no legacy implementation kept as reference;
the engine is the only implementation.

## Compatibility Strategy

Compatibility improves through narrow, test-backed slices:

- conformance runs against pinned test262 and WPT checkouts
- focused regression tests around real-world fixtures
- compatibility notes under `docs/`

Site-specific render adapters and external-browser fallbacks are intentionally
out of scope. Missing capability should be tracked as generic engine work, not
patched per site.

## Security Model

- TLS certificate errors are fatal and are never retried with verification disabled.
- JavaScript `fetch` and XHR are same-origin only until CORS response handling is implemented.
- Remote pages cannot read `file:` URLs; local file access is enabled only for local documents and their resources.
- Network responses and data URIs have in-memory size limits.

This is still an experimental single-process browser engine, not a hardened sandbox for hostile web content.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for setup, review expectations, and change guidelines.

## License

Project source is distributed under the terms in [COPYING](COPYING).
