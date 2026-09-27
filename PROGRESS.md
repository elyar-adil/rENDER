# Progress and Current State

This page is a pointer and a working inventory. It is not the log.

- **`HANDOFF.md`** is the chronological record. Newest section first; each entry records
  what shipped, what was measured, and what was deliberately left undone.
- **`docs/visual_fidelity_gaps.md`** is the evidence register. Every capability that is
  parsed-but-not-consumed, computed-but-dropped, or absent entirely, with a `file:line`
  citation and, where possible, a count measured over the real stylesheet corpora in
  `.diag/`. Read this before concluding that a CSS property "is supported".
- **`docs/generic-browser-todo.md`** is the law, including the rule that a gap gets
  implemented forward and never removed.
- **`docs/real_site_acceptance.md`** describes the offline real-site acceptance
  contract: which structural properties of a real page must hold, with no Internet
  access.

## Verified baseline

Measured on the tree as of commit `2bd8e8c`, before the parallel workstream landed:

| Check | Result |
| --- | --- |
| `cargo test --workspace` | all targets ok, 0 failed |
| test262 conformance gate | 9 passed, 484 s |
| `render-js` unit tests | 195 passed |
| `render-layout` unit tests | 81 passed, 1 ignored |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |

These numbers are the arbitration baseline. When an agent reports a pass count, check
it against the crate's own previous count rather than taking the report at face value.

## Diagnostic tools

Headless HTML-to-pixels and layout probes. All are `cargo run` targets.

| Command | What it answers |
| --- | --- |
| `cargo run -p render-core --example layout_chain_diag` | computed style and fragment rect for an element's ancestor chain, by class name; supports `URL_SUBSTR=CSS` for multi-file mapping. The single most useful tool when a real page lays out wrongly. |
| `cargo run -p render-core --example css_probe` | whether one stylesheet takes effect, by progressive-prefix probing |
| `cargo run -p render-core --example dom_dump` | serialises the DOM back to HTML after scripts have run, so a post-JS DOM can be replayed offline |
| `cargo run -p render-core --example js_probe -- <file.js>` | JS regression probe set |
| `cargo run -p render-core --example jquery_bisect` | bisects a script to the first offset that breaks |
| `cargo run -p render-core --example baidu_diag` | offline page replay against a local `manifest.txt` |
| `cargo run -p render-core --example hao123_diag` | same, for hao123 |
| `cargo run -p render-core --example bilibili_layout_diag` | carousel and card geometry probes |
| `cargo run -p render-js --example qq_bundle_probe` | runs a large production bundle offline and reports the first failure with its byte offset |
| `cargo run -p render-js --example bilibili_diag` | offline bundle replay with stack traces |
| `cargo run -p render-css --example css_corpus` | parses every stylesheet in `.diag/**` and reports rules parsed vs dropped |
| `cargo run -p render-net --example fetch_bench` | transport timing for a URL list |
| `cargo run -p render-net --example connect_probe` | per-address connect behaviour |
| `cargo run -p render-browser --bin render-perf -- --fixture all` | headless performance distributions |

## Debug switches

All verified live. These are the supported observability seams; prefer them over adding
a new print statement.

| Variable | Effect |
| --- | --- |
| `RENDER_DEBUG_FRAME=1` | per-frame render pipeline diagnostics: stylesheet count, computed style count, fragment count, display item count, content height, and per-image fragment rects |
| `RENDER_DUMP_FRAME=<path>` | write each frame as a PPM. Convert with `python tools/ppm2png.py <in.ppm> <out.png>` |
| `RENDER_STAGE_TIMING=1` | per-stage timing during document render |
| `RENDER_JS_TRACE=1`, `RENDER_JS_DEPTH=1`, `RENDER_JS_BINDINGS=1` | JS execution trace, depth budget, and binding trace |
| `RENDER_JS_GC=1` | force JS garbage collection reporting |
| `RENDER_JS_FRAME_OFFSETS=1` | include frame offsets in JS errors |
| `RENDER_TRACE_STRING=1`, `RENDER_TRACE_NATIVE=1` | narrow JS traces for string and native dispatch |
| `RENDER_DEBUG_INTRINSIC=1` | intrinsic sizing trace in the inline solver |
| `RENDER_DIAG_STACK=1` | stack traces in the bilibili replay |
| `RENDER_DIAG_CAROUSEL=1`, `RENDER_DIAG_CARD=1` | carousel and card probes |
| `RENDER_DIAG_BASE=<url>` | base URL for the offline page replays |
| `RENDER_DUMP_INLINE=1` | export failing inline scripts from a replay |
| `RENDER_DIAG_FRAGMENTS=<n>` | fragment dump limit |
| `RENDER_PERF_DIAG=1` | extra `render-perf` diagnostics |
| `RENDER_NET_LOG=1`, `RENDER_NET_SLOW_MS=<n>` | transport request logging and the slow-request threshold |
| `RENDER_TEST262_*`, `RENDER_WPT_*` | conformance-runner configuration; see `docs/test262.md` and `docs/wpt.md` |

## Claims that were checked and found false

Kept so nobody spends a session re-chasing them. Each was verified against the code, not
against an older report.

- **Gradients are painted.** `parse_background_image` consumes the nested block only for
  validation and returns the raw source text via `slice_from(start)`
  (`crates/render-css/src/properties.rs:1729`); `crates/render-core/src/paint/display_list.rs:929-975`
  builds real `LinearGradient` and `RadialGradient` commands.
- **`calc()`, `min()`, `max()`, `clamp()` are supported**
  (`crates/render-css/src/length.rs:316-323`).
- **Cascade layers are fully supported**, including `@layer` statements, `@layer` blocks
  and `revert-layer` (`crates/render-css/src/stylesheet.rs:22-88`, `cascade.rs:879`).
- **`:is()`, `:where()`, `:not(<list>)` and `:has()` are supported**
  (`crates/render-css/src/selector.rs:665-668`).
- **`srcset` is supported**, including `sizes` and both the `w` and `x` descriptors
  (`crates/render-core/src/image.rs:952-1152`).
- **`document.cookie` is implemented** - the `document.cookie` accessor and setter in
  `crates/render-js/src/runtime/eval.rs` (cite the symbol, not a line number: that file
  is under active edit and the line numbers move every session).
- **`render-dom` is namespace-ready** (`Namespace`, `ElementData.namespace`,
  `create_element_ns`, and HTML-only lowercasing at `render-dom/src/lib.rs:192,209,502,726`).

And one that is only half true, so it is easy to get wrong in either direction:

- **Video stops at the bitstream layer.** The MP4 demuxer and the H.264 bitstream work
  are real - `avcC` parsing, NAL classification, SPS dimension extraction, Annex-B
  conversion. The entropy decode and reconstruction are not, and the shipped default is
  still `PlaceholderDecoder`, so no site ever presents a decoded frame.
