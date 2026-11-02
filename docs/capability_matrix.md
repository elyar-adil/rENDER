# Capability matrix

What this engine can and cannot do, with the measurement behind each claim. This is the honest
answer to "how far off is it". It is deliberately **not** a percentage.

**Why not a percentage.** Three were tried and all three were refused for cause: two were
inaccurate by 20+ points, and the third was a guess dressed as a number. A single figure also
hides the shape of the problem, and the shape is the useful part - see the last section.

Every claim below cites a measurement. Where a claim is an inference rather than a measurement it
says so. `docs/visual_fidelity_gaps.md` is the working register; this file is the summary.

## The measurements this is built on

| what | number | when |
| --- | --- | --- |
| `render-js` unit tests | 1,234 passed, 0 failed | 2026-11-02 |
| `render-css` unit tests | 319 passed, 0 failed | 2026-11-01 |
| `render-core` unit tests | 532 passed, 0 failed, 1 ignored | 2026-11-02 |
| `render-core` webfont integration tests | 4 passed, 0 failed | 2026-11-01 |
| `render-core` RTL layout integration tests | 2 passed, 0 failed | 2026-11-01 |
| `render-layout` unit tests | 389 passed, 1 ignored | 2026-11-01 |
| `render-browser` unit tests | 231 passed, 0 failed | 2026-11-02 |
| `render-browser` app tests (binary) | 117 passed, 0 failed | 2026-11-02 |
| `render-html` unit tests | 176 passed, 0 failed | 2026-11-02 |
| `render-net` unit tests | 66 passed, 0 failed | 2026-11-02 |
| `render-dom` unit tests | 90 passed, 0 failed | 2026-11-02 |
| real-site acceptance harness | 25 passed, 5 `#[ignore]`s each mapped to a named gap | last recorded |
| Python static gate | 15 passed, 55 subtests | last recorded |
| site-neutrality gate | **9 rules, 312 Rust source files, exit 0** with 26 advisory findings | 2026-11-02 |
| **test262** | **90,965 / 103,199 = 88.15%**; 264 buckets, 8 timeout, 0 crash; checked-in baseline floor = 90,765 | **2026-11-01, release profile, pinned revision** |
| **html5lib tree construction** | **7,196 / 7,377 pairs = 97.55%**; 140 acceptable difference, 0 unimplemented, **41 engine defect (0.56%)**; html5lib-only is **3,541 / 3,549 = 99.77%** with 8 defects | **2026-11-02, pinned revision** |
| html5lib parse-error count agreement | **5,098 / 6,601 = 77.23%** under `max`, **63.93%** under `sum` - the gap between those two is the choice of reading, not the parser | **2026-11-02** |
| **WPT `dom/`** | **32 / 297 executable = 10.8%**, 0 vacuous passes | last recorded, pinned `c7fdee80f3f1` |
| **WPT `html/`** | **165 / 2,520 executable = 6.5%**, 31 errored, 0 vacuous passes | last recorded, pinned |
| **WPT combined** | **197 / 2,817 = 7.0%** - 5,962 scored, 8,060 skipped, 1,684 with no assertion site at all | last recorded, pinned |
| **WPT `css/`** | **cannot be run** - `testcss.js` is an iframe driver, and while nested contexts now exist, an iframe document is not painted and the adapter hosts one document | - |
| production HTML corpus sweep | **3,718 documents**, 0 of our diagnostic kinds at fault, 3 real defects found and fixed | last recorded |
| production CSS corpus | 11,102 rules, 42,356 declarations, 1.93 MB | last recorded |
| production JS | a 2.27 MB real bundle completes; a second probe also runs to completion, 0 threw | last recorded |
| clippy `-D warnings`, workspace-wide | clean | 2026-10-22 |
| mutation audit | 34 hand-picked mutations, **10 survived (29%)** | last recorded |

The test262 figure is the external authority for the JavaScript language; the html5lib and WPT
figures cover tree construction and the DOM/HTML surface. Everything else is either the engine
agreeing with itself or a measurement over real production code - which is worth a great deal, and
is not the same thing as an external authority checking it. Both external runners now exist and
have produced the numbers in the table.

## Strong: deeper than the outside suggests

**HTML tree construction.** 120 tests. Foreign content with the full adjustment tables, `<template>`
contents as an inert parentless fragment, the list of active formatting elements with the adoption
agency and the specification's own three worked examples, `in table text`, and a form owner that
is *derived on every read* rather than stored, so the entire class of staleness bug cannot exist.
The last correction here was the sixth agent to overturn a spec claim I wrote into a work order.

**CSS cascade and selectors.** Specificity, inheritance, the cascade origins, presentational hints
at the correct origin, CSS Nesting, `@supports` evaluated against a three-state oracle. CSS Cascade 6
`@scope` is evaluated: explicit and implicit scoping roots, scoping limits, relative preludes with
zero-specificity `:where(:scope)` anchoring, `:scope` and `&`, nested scopes, declarations directly
in `@scope`, and scope proximity between specificity and order of appearance. Author stylesheets
are scoped to their node tree, with Cascade Context ordering; stylesheets inside shadow roots are
collected and reach their own tree, and `:host`/`:host()`/`:host-context()`/`::slotted()` provide
the two intentional cross-tree paths. `slotchange` fires after a slot's assigned nodes change.
CSS Color 4 is implemented through a single conversion layer: `hsl()`/`hsla()`/`hwb()`,
`lab()`/`lch()`, `oklab()`/`oklch()`, `color()` in every predefined space (sRGB, linear sRGB,
Display P3, A98 RGB, ProPhoto RGB, Rec. 2020, XYZ D50/D65), and `color-mix()` in those spaces
with premultiplied alpha and all four hue paths. Absolute mixes resolve at parse time; mixes
naming `currentcolor` or a system color resolve at paint time. Out-of-gamut colors use the
CSS Color 4 §14.2.2 Binary Search + Local MINDE mapping at the sRGB device boundary. `none`
components carry forward to analogous components during `color-mix()` and serialize as `none`;
outside interpolation they behave as zero, including canvas paint and transitions. Relative color
syntax works in `rgb()`/`hsl()`/`hwb()`/`lab()`/`lch()`/`oklab()`/`oklch()`/`color()` with
channel keywords, `none`, inherited alpha and basic `calc()` arithmetic; `alpha()` changes
only the origin's alpha, and `light-dark()` selects its arm from the element's used
color scheme. `color-scheme` is a typed inherited property; `normal` follows the page
scheme from `DocumentRenderOptions.color_scheme`, while an explicit `light`/`dark`/list
resolves per element and feeds both `light-dark()` and `Canvas`/`CanvasText`. The same
property currently does not change `prefers-color-scheme`, because the page scheme is still
the render input. Shorthand
expansion is now
spec-correct for `text-decoration`, which is what fixed the most-reported visual symptom in the
project's history. `@property` registrations validate their syntax and initial value, stop
`inherits: false` properties from inheriting, and replace invalid computed values with the
registered unset value before `var()` substitution. Media Queries 4/5 conditions support
`not`/`and`/`or`, nested parentheses and range syntax, plus the common environment features.
`@container` size queries parse at stylesheet time and evaluate against measured ancestor content
boxes, with a bounded three-pass style/layout feedback loop. `style()` queries read the query
container's already-computed style during the same top-down cascade, so every element is a style
container without `container-type` or geometry; declarations, boolean features, registered syntax,
and `<color>` equivalence are supported, and a condition mixing size and style features is
answered by one container. Generated-content pseudo-elements query their originating element
inclusively, including a style computed in the same pass. Container-relative units
(`cqw`/`cqh`/`cqi`/`cqb`/`cqmin`/`cqmax`) resolve per axis against the nearest eligible ancestor,
falling back to the small viewport size when none exists. `container-type` applies size/inline-size
containment and makes the box an independent formatting context, but by itself is not a positioning
containing block (CSS Conditional 5 §5.1). The independent `contain` property is implemented per
CSS Containment 2 §2: `size`/`inline-size` zero the intrinsic size of the contained axes,
`layout`/`paint` add an independent formatting context, a containing block for absolute and fixed
descendants, and a stacking context; `paint` also clips to the padding box. `strict` and `content`
expand at parse time and serialization follows grammar order. `style` containment is parsed but its
counter/quote isolation is not yet observable. `scroll-state()` queries and `style()` shorthand
features remain gaps.

**Block flow, vertical margin collapsing and bidi.** CSS 2.1 §8.3 is implemented for the block
formatting context: adjoining sibling margins, a parent's first/last child margins, and an
empty block's own top/bottom margins all collapse through a `MarginStrut` that keeps the
largest positive and most negative components (`max(+) + min(-)`). Parent-first and
parent-last collapsing move the parent's border edge and remove the escaped margin from its
content height; an empty block passes its whole strut to the next sibling. Floats, absolutely
positioned boxes, flex and grid items, table cells, BFC roots (`overflow`, containment,
`container-type`, `flow-root`, multi-column) and clearance are excluded from the collapse,
and positioned descendants whose containing block lies outside a shifted subtree keep their
document position. Evidence: 13 exact-geometry tests in `solver/collapse_tests.rs`.

**Bidirectional text.** CSS Writing Modes 3 `direction` is a typed inherited property, and the
UA sheet maps the HTML `dir` attribute's `ltr`/`rtl` states. Inline layout runs UAX #9 through
`unicode-bidi` with the CSS `direction` as paragraph level, then places level runs in visual
order per line; odd-level runs are reversed for the left-to-right painter. `text-align:start`
and `end` resolve against the paragraph direction, `text-indent` shortens the RTL right edge,
and atomic inlines participate as U+FFFC objects. Evidence: 12 exact-geometry tests in
`solver/bidi_tests.rs`. Not implemented: UAX #9 L3/L4 (combining marks and mirroring),
`unicode-bidi` embeddings/isolates, per-inline-element `direction`, block-axis RTL mirroring,
`dir=auto`, and complex-script shaping.

**Content visibility.** `content-visibility: hidden` keeps the element's own box but skips
its contents: size containment makes an `auto` content height zero, layout and paint
containment make the box independent, and no descendant fragments are produced. `auto` is
parsed and currently lays its contents out normally; skipping off-screen content and
`contain-intrinsic-size` are not implemented. Evidence: 5 exact-geometry tests in
`solver/content_visibility_tests.rs`.

**Nested browsing contexts.** A loaded iframe now has a real replaced box (300x150 default,
HTML width/height, CSS sizing) and is painted as a clipped child document. The render
pipeline is root-aware: style computation, presentational hints, the formatting tree, root
font size, layout and the display list can start at a nested document root in the shared DOM
arena. Each child renders at its iframe content-box size into a separate transparent surface,
installed as an `ImageSource::FrameDocument`; the parent list gains one ordinary image command
sourced from the iframe element, so child nodes never join the parent list. Child scripts and
load events keep their own realm; a child-only DOM change carries its revision into the parent
image command and repaints. Nested frames recurse to depth 8. The iframe box hit-tests as the
iframe element (child coordinate mapping remains open), and a release-shell screenshot shows
the child block/text at the exact parent coordinates.

**Selection and Range.** `window.getSelection()` and `document.getSelection()` return one live
`Selection` per document. `anchorNode`/`focusNode`/offsets, direction/type, `getRangeAt`,
`addRange`, `removeAllRanges`, `collapse`, `collapseToStart/End`, `extend`,
`setBaseAndExtent`, `selectAllChildren`, `deleteFromDocument`, `containsNode` and `toString`
operate on the existing live `Range` table, so mutations clamp removed boundaries and text
splits stay valid. Iframe realms have independent selections. Selection-highlight and caret
painting plus `selectionchange`/`selectstart` and `Selection.modify()` remain open.

**HTTP/2.** `render-net` now has a real HTTP/2 stack: RFC 9113 frame codec (DATA, HEADERS,
CONTINUATION, PRIORITY, RST_STREAM, SETTINGS, PUSH_PROMISE, PING, GOAWAY, WINDOW_UPDATE),
RFC 7541 HPACK plus the full Huffman table, stream lifecycle, flow-control accounting,
multiplexed response dispatch and GOAWAY/RST handling. The transport connects direct HTTPS
origins with rustls, requests ALPN `h2`, and pools one multiplexed connection per origin; a
server that selects HTTP/1.1 is cached as HTTP/1.1-only and falls back to ureq. Proxied
origins stay on the HTTP/1.1 CONNECT path so no request bypasses a proxy. Loopback TLS tests
prove ALPN h2, two concurrent streams on one connection, out-of-order responses, a 120KB body
beyond the initial window, GOAWAY eviction/reconnect, and HTTP/1.1 fallback. h2 bodies are
assembled in memory and request bodies above 60KB use HTTP/1.1; h2 over proxies, server push
and priority scheduling remain open.

**Commit starvation and main-thread scheduling.** The render worker now drains the newest
completed frame per tab even when a newer job is already queued. `is_tab_busy` includes queued
and completed-but-undrained work; the event loop polls completed frames before recovery
resubmits; and the commit gate accepts a frame only when its document serial, requested
viewport and scroll position still apply and its generation is newer than the presented frame.
This lets the first post-image/post-CSS frame reach the screen instead of being silently
dropped behind a newer submission. Page pumping also has a 24ms wall-clock slice between
turns, and the classic-script batch yields after 32ms/1024 turns so remaining work drains
through the bounded pump. A single script turn is still not preemptible.

**HTML tree construction.** The html5lib/WPT corpus is now **7,196/7,377 = 97.55%**
(html5lib-only **3,541/3,549 = 99.77%**). Button scope and paragraph closing, adoption agency,
generic end tags, table insertion modes, tokenizer comment/EOF states, NUL/CR handling,
foreign-content end tags, customizable select `selectedcontent` and quirks doctypes were
fixed, and the `.dat` harness no longer swallows attribute names beginning with `<`. The
remaining 41 engine defects are 37 old processing-instruction expectations (the current
standard emits PI nodes where the pinned suites expect comments) and 4 scripted fixtures.

**Retained compositor.** `CompositorCache` turns a display list into a small top-level layer
tree: root segments preserve paint order and are split by coordinate space, transform/opacity
stacking contexts become layers, and each surface is divided into 512px bands. `render` only
rasterizes bands that are not cached; scrolling changes the composite matrices, not the scene.
Fixed content is rasterized in viewport space and composited without the scroll translation;
a top-level sticky scope is rasterized untransformed and displaced at composite time; scale,
rotate and skew layers inverse-map the viewport into source-local coordinates and composite the
untransformed source through the affine matrix. A dedicated **compositor worker** owns scroll
frames: the render worker keeps style, layout, display-list construction and the complete raster,
while scroll jobs run on a separate thread and commit only the presentable surface. A scroll no
longer waits behind a full render. Top-level `filter` and `mask` layers are retained too: a
filtered/masked layer becomes one effect surface (not independent bands) so blur/drop-shadow can
cross band boundaries, and its filter, mask, transform and opacity are applied in complete-raster
order. Non-normal blending, ambient clips crossing a layer, nested sticky, and effects beyond the
8192x4096/4.2M-pixel safety ceiling still use the complete raster. Twelve core tests compare
composited pixels with a full raster, pin band reuse over twenty 16px scroll frames, and cover
fixed content away from the origin, sticky displacement at five scroll offsets, scale/rotation,
blur across band boundaries, gradient masks, filter+opacity+transform, and viewport-space
filter/mask layers. One dedicated-worker test runs twenty scroll frames on a non-caller thread and
records per-frame timing; a browser test blocks the render worker for the whole test and still
commits a scroll frame. `cargo run --release -p render-browser --bin render-scroll-bench` reports
**2.23ms/frame retained vs 5.87ms/frame full raster** with **116/120 frames rasterizing no new
band**. The compositor worker presents through a `PlatformPresenter` sink directly:
`cargo run --release -p render-browser --bin render-compositor-bench` completes and presents
**120/120 scroll frames during a 500ms simulated render-worker block** (mean 1.72ms, mean platform
present 253us). A real shell page containing a fixed header, a sticky nav and a rotated card logs
`render-browser compositor retained layers=5`, so the live display list takes the retained path
rather than the fallback.

**GPU presentation.** On Windows the shell now prefers a real wgpu/D3D12 swapchain:
`GpuPresenter` creates a `wgpu::Surface` from the winit window, requests a DX12 adapter/device,
uploads the composed ARGB frame into an `Rgba8UnormSrgb` texture with 256-byte row padding, draws a
fullscreen triangle sampling that texture, and presents with `queue.present`. The compositor
worker owns presentation after the surface is installed, so scroll frames reach the swapchain
without returning through the event-loop thread. A hidden-window live benchmark reports the
adapter and backend, and the observed machine is
`NVIDIA GeForce RTX 4060 Ti / Dx12`: **120/120 frames presented**, mean GPU present **3.86ms**,
median **4.03ms**, max **4.37ms**, mean compositor **2.90ms**. When no adapter/device is usable,
the shell logs the failure and falls back to the softbuffer sink; `RENDER_FORCE_SOFTBUFFER=1`
exercises that path explicitly.

**CSS text spacing.** `text-indent`, `letter-spacing` and `word-spacing` now have real consumers
in measurement, line breaking, intrinsic widths, glyph painting and hit testing. `text-indent`
supports lengths, percentages, `calc()`, `em`/`rem`/viewport units, negative values, `hanging` and
`each-line`, applies only to the first formatted line (or after a forced break with `each-line`),
and mirrors the start edge under RTL. `letter-spacing` uses the CSS Text 3 §7.2 reading of
`n - 1` gaps between character units, averages at inline boundaries, and is absent at line
edges; `word-spacing` adds one separator advance per word separator. `TextFragmentData` carries
the used spacing plus leading/trailing boundary values; `TextShaper::shape_spaced` keeps painting
and hit-test character cells aligned with the measured run. Tests pin two-line indent, RTL indent,
negative/cross-inline tracking, word-spacing line breaks, display-list glyph origins and hit-test
caret boundaries. Grapheme-cluster spacing remains per-scalar, and `ex`/`ch` remain unresolved by
the shared length context.

**RTL and logical box properties.** Horizontal-tb RTL now mirrors block placement: over-constrained
margins, auto margins with negative free space, root direction, floats/clear logical keywords,
relative/absolute inset preference, static positions, and line alignment within a float-shortened
band. Flex row/row-reverse order, `justify-content`, column cross-axis alignment and wrapping;
grid logical columns with right-edge placement and `justify-content`; and table column order and
spanning-cell anchoring all mirror under RTL while LTR is unchanged. Exact-geometry tests live in
`rtl_layout_tests.rs`, with display-list and hit-test integration tests in
`render-core/tests/rtl_layout.rs`. CSS logical properties are resolved end-to-end for
horizontal-tb: 24 logical longhands (`margin/padding/inset/border-{inline,block}-{start,end}-*`),
logical shorthands, percentage/calc/em/rem/viewport units, and physical-vs-logical cascade
competition. Vertical writing modes, logical sizing/corner/scroll properties, table `<col>`
direction and `justify-items`/`justify-self` remain open.

**`@starting-style`.** CSS Transitions 2 entry transitions are implemented: the parser retains
`@starting-style` at top level, nested in style rules, and inside conditional/layer/scope groups;
the ordinary cascade skips it; `compute_starting_styles` applies it only for an element entering
the rendered set (first style change, insertion, or `display:none` becoming rendered); the
existing transition sampler starts from those values when `transition-property` covers the
property, then emits the normal run/start/end/cancel events. Tests cover first render, nested
selectors, no-transition properties, `display:none`, DOM insertion, specificity/source order and
static-style exclusion. Pseudo-element starting styles, `content-visibility` rendered-state
fidelity and CSSOM `CSSStartingStyleRule` remain open.

**SVG filter primitive chains.** Inline SVG now parses an ordered filter chain with `in`/`in2`/
`result`, `SourceGraphic`/`SourceAlpha`, named results, filter/primitive units, percentages,
subregions and transformed regions. Implemented primitives: `feGaussianBlur` (premultiplied
separable, SVG edge clamp), `feColorMatrix` (matrix/saturate/hueRotate/luminanceToAlpha),
`feOffset`, `feBlend` (all W3C blend modes), `feComposite` (over/in/out/atop/xor/arithmetic),
`feFlood`, `feMerge`/`feMergeNode`, `feDropShadow`, and existing `feMorphology`. Pixel tests cover
edge falloff, exact matrices, offset coverage, blend/composite math, merge/flood, region and
subregion clipping; unsupported primitives produce diagnostics and pass input through.
`feTurbulence`, `feDisplacementMap`, `feImage`, `feComponentTransfer`, `feConvolveMatrix`,
`feTile`, lighting primitives, `linearRGB` filter interpolation and fractional offsets remain
open.

**HTTP cache variants.** The private HTTP cache now implements RFC 9111 §4.1 `Vary`: field names
are parsed case-insensitively, multiple variants per URL are stored side by side, lookup and
revalidation select only a matching variant, absent differs from present-empty, repeated request
field lines combine in order, and `Vary: *` is never stored or served. 304 updates preserve or
recompute the variant and refresh only the selected one; `merge_headers` replaces every stored
field line of an updated name. Transport-injected constant defaults are not synthesized into the
key, so a mismatch is a safe miss rather than a wrong hit.

**Table layout.** 47 tests. CSS 2.1 §17 including border-collapse conflict resolution, row groups,
`colspan`/`rowspan`, `caption-side`, fixed layout, intrinsic widths. This is the single most-used
feature in the corpus and it is the deepest suite in the tree.

**The font axis and font matching.** CSS Fonts §5 implemented as a **pure function over a face
table** - 42 tests against synthetic tables - covering family fallback, style-before-weight
ordering, the weight search as a total order, per-character coverage, and negative oblique.
Synthetic bold and oblique are implemented in the glyph mask provider and change **no advance**,
which is what keeps measurement and painting in agreement. Two things it deliberately does not do:
synthesise italic (the specification forbids it for engines that treat italic and oblique
distinctly, and this one does), and variable-font axes (the rasteriser exposes no axis accessors
and the corpus has no variable font).

**`@font-face` collection, network loading and WOFF decoding.** Author stylesheet discovery
hands every eligible `<style>` and fetched `<link>` sheet's `@font-face` rules to a
`FontFaceInstaller` before the first layout pass. `crates/render-browser/src/font_faces.rs`
resolves §4.3.2 URLs against the sheet, loads `local()`, `file:` and base64 `data:` faces
synchronously, and publishes the document table through `SystemFontBackend::set_document_faces`.
The render worker plans the HTTP(S) URLs §4.8.1 requires from the frame's computed families and
page characters; the app submits them through its cached network batch; the returned bodies are
decoded from raw sfnt, WOFF 1.0 (zlib) or WOFF2 (Brotli) and installed before the next
measurement. The loader keeps decoded faces across a re-render but not across a document.
`crates/render-core/tests/webfont_layout.rs` proves the geometry change (24px -> 48px for `HHH`
at 16px) with an embedded sheet, and `render-browser` has a loopback HTTP test that fetches a
WOFF2, decodes it, and measures the same 48px. `@font-face` rules now retain and evaluate their
`@media`/`@supports` conditions against the render environment; a non-matching face is never
planned, fetched or installed, including nested conditions and external sheets. `font-display` has
a real document timeline: `auto`/`block` use a 3s block with infinite swap, `swap` uses no block
with infinite swap, `fallback` allows arrival before 3.1s, `optional` before 100ms; an arrival at
the failure boundary is too late, and expired sources remain missing under §5.2 shadowing. The
engine deliberately measures/paints the visible fallback during the block period because it has a
single measure/paint table and §4.8.1 requires visible text. Still open: §4.6/§4.7 variation
descriptors, Embedded OpenType and SVG fonts, WOFF2 files using the decoder's unsupported `hmtx`
transform, `@container`-gated `@font-face`, and pseudo-element starting styles for fonts.

**Network transport.** 81 tests. HTTP/1.1 with connection reuse proven by measurement, a
same-origin ceiling that is an explicit measured choice rather than a dependency default
(9.4 -> 6.0 connections for a 40-asset page, which is the HTTP/1.1 floor), separate connect /
response / body budgets, and a per-request terminal outcome naming the phase and the elapsed time.
The body budget is an **idle-read** interval, not a total, so a large slow download survives it -
proved live by a 502,941-byte fetch that outlived its own 500 ms response budget and still
completed.

**Capability tracking.** `spec/registry.rs` has 35 entries and now **rejects any non-conformant
entry with a blank note**, so it cannot decay back into a list of `Partial`s that says nothing.

## Partial: works, with a named hole

| area | what works | the hole |
| --- | --- | --- |
| inline SVG | shapes, path commands, group transforms, fill/stroke inheritance, `viewBox` scaling, sized from attributes and painted through the ordinary image path | `use`/`symbol`/`defs` indirection, `foreignObject`, aspect-ratio preservation when the box ratio differs from the viewBox |
| scrollports | `ScrollportGeometry` with mode, clip and scrollable range, structurally distinct from `overflow: hidden` clipping | the shell side: no offset store, no scrollbar chrome, no keyboard scrolling, no scroll anchoring, and a sticky box in a *nested* scrollport cannot be resolved |
| position | `static`/`relative`/`absolute`/`sticky`: the sticky constraint is resolved against the nearest scroll container and painted and hit tested at the scroll offset; the containing-block clamp; an auto inset is not a zero inset | `z-index` sorts block siblings only (with positioned boxes after in-flow ones of the same layer); flex and grid children unsorted; no stacking-context tree; only `transform`/`opacity` create a stacking context; a nested scroll container's offset is zero |
| video | container and bitstream parsing, poster handling | **pixel decode** - no frame is ever rasterised |
| text properties | `text-decoration` (all four longhands, with the spec's omitted-values rule), `text-shadow`, `text-overflow`, `text-indent`, `letter-spacing`, `text-transform` | the font axis landed this session; `font-synthesis` is not in the property registry, so `font-synthesis: none` does nothing yet |
| quirks mode | the flag is wired into the headless path and the rendering rules are implemented with a per-rule citation | two box-model items remain uncited and unimplemented, and they are solver behaviour |
| diagnostics | 606 produced over the production corpus, by kind and per file | **counted but not displayed** - `render-browser` has no mapping from message to `StylesheetDiagnosticCode`, so the panel shows nothing |

## Absent: structural, and this is the real distance

Verified by grep, not inferred. Full discussion in the register's S30.

| absent | consequence |
| --- | --- |
| **ES modules** | **Largely closed.** `render-js` parses import/export into tables (`src/module.rs`) and links them with live bindings (`src/runtime/module.rs`): named/default/namespace imports, re-exports, `export *`, `export * as`, cycles for hoisted declarations, `import.meta.url`, evaluate-once ordering, remembered failures. `render-core/src/module_graph.rs` assembles the graph and `render-browser/src/scripts.rs` fetches it in rounds. **Still open:** dynamic `import()` resolves through the host loader (browser fetch rounds + import map); CORS enforcement for cross-origin modules and top-level `await`; import maps are parsed for static module graphs (multiple-map merging and dynamic-import loading remain open); module code runs sloppy, not strict; a namespace object is a snapshot rather than a live exotic object; classic scripts still skip `import`/`export` instead of throwing a SyntaxError; `type=module` `async` scheduling is treated as defer; `<script type=module>` inserted after load goes through the same path as classic follow-up scripts but is untested in a live window |
| **Web Workers** | **Dedicated workers implemented.** `new Worker(url, {name})` loads a classic script through the page network seam (or a `data:` URL), runs it in an isolated same-thread realm with `self`, `postMessage`, `onmessage`/`onerror`/`onmessageerror`, `importScripts`, timers, `fetch`, `structuredClone`, `Blob`/`URL`, and `close`. Messages use structured clones and `ArrayBuffer` transfer/detach. `terminate()` stops queued work and timers. Still absent: OS-thread parallelism, module workers, `SharedWorker`, `MessageChannel`/ports, and `navigator.serviceWorker` |
| **History API** | `pushState`/`replaceState` change the URL and `history.state` without a load. Back/forward/go within the document fire `popstate` without a load; entries of another document reload. `history.length` reflects the list at each turn's start |
| **Iframe painting** | a nested document runs scripts in its own realm, but it is not laid out or painted, so a page embedding one loses that content visually |
| **Animations and transitions** | *Closed for the CSS surface.* `@keyframes`, `animation-*`, and CSS transitions are sampled by a browser-driven 16 ms timeline clock; keyframes inside `@media` are gated, while keyframes inside `@container` are global per CSS Conditional 5 §5.4. Multi-animation lists, reversing shortening and WAAPI edge cases remain partial. |
| **Selection / Range** | Range is implemented (DOM §5): `Range`, `StaticRange`, `createRange`, the boundary-point and content algorithms, and live updates on every tree mutation. There is no `getSelection` selection state, so `getSelection()` reports an empty selection, and no caret position APIs |
| **Shadow-scoped styling** | shadow trees, `attachShadow` and autonomous `customElements` exist; shadow-tree stylesheets are collected and isolated per tree, `:host`/`:host()`/`:host-context()`/`::slotted()` match, and `slotchange` fires. `::part()`, `:has-slotted()`, declarative shadow roots and `::slotted()::before/after` remain |
| **Compositing** | **Partial.** `CompositorCache` builds a retained top-level layer tree: root segments preserve z-order and coordinate space, transform/opacity/sticky scopes become layers, and surfaces use 512px bands. A dedicated compositor worker runs scroll jobs independently of the render worker; fixed content lives in viewport space, sticky scopes displace at composite time, and scale/rotate/skew layers inverse-map the viewport for band selection. Top-level `filter` and `mask` layers are retained as single effect surfaces so blur/drop-shadow crosses band boundaries, with full-raster pixel equality. On Windows the worker presents through a wgpu/D3D12 swapchain (`GpuPresenter`): ARGB frame → texture upload → fullscreen triangle → `queue.present`; softbuffer remains the fallback. Non-normal blend modes, ambient clips crossing a layer, nested sticky and effects above the 8192x4096/4.2M-pixel ceiling still fall back. Live GPU benchmark: 120/120 frames, mean present 3.86ms (RTX 4060 Ti/Dx12); CPU compositor benchmark: 120/120 during a 500ms render-worker block. |
| **Bidi** | UAX #9 has zero occurrences, so right-to-left scripts lay out left-to-right. Correctly lower priority given the corpus, but recorded rather than left to be rediscovered |
| **HTTP/2** | structurally rejected: a pooled connection leaves the pool while in use and is held for the whole request, so one connection carries one in-flight request. Multiplexing is the inverse |
| Multi-column layout | `column-count`, `column-width`, `columns`, `column-gap`, `column-fill` and `column-span: all`: the flow is cut at line boundaries, and a block that a break crosses is split with `box-decoration-break: slice` | `column-rule` is not implemented; `column-span` applies to direct block children only |
| `filter`, `clip-path`, `backdrop-filter`, `mask-image`, `mix-blend-mode` | zero occurrences in the paint layer. Not approximated - absent |
| `WebSocket`, `requestIdleCallback`, `PerformanceObserver` | absent |

**Closed since this table was first written:** line breaking now implements UAX #14 and the CSS
tailorings (S34); `DOMException` is a real interface object; `window.matchMedia` is implemented and
tracks viewport changes; form submission has a cancelable `submit` event, GET/POST urlencoded
navigation and `FormData`; and shadow trees exist, with styling still open as the row above says.

## What the numbers do not tell you

**A test count is not a conformance claim.** A mutation audit this session attempted 34 hand-picked
mutations across four crates; **10 survived - 29%**. The code in all three most important cases is
**correct today**; the finding is that nothing would notice if it stopped being. Concretely:
`table-layout: fixed` with a non-zero `border-spacing` has no test - and `border-spacing` is
non-zero in most real tables. `position: sticky` with `bottom` or `right` has **no test at all**,
and all 12 sticky tests use `top` or `left`. The initial value of `text-decoration-line` is
unpinned, and that is precisely the longhand whose default value caused the original most-reported
bug.

**So the mutation survival rate is a better instrument than the test count**, and it says the newest
code - the code written fastest, by agents - is the least pinned. That is a real and actionable
finding, and it is the opposite of what a test-count table suggests.

**Five test functions cannot fail**, two of them live and counted, and one reads fixtures from a
site-named directory. That is how a count gets quietly inflated, and it happened twice this
session.

**The first external numbers of any kind for CSS, HTML or DOM now exist**, from two pinned
suites.

- **Tree construction: 97.55%** of 7,377 (test, scripting-mode) pairs pass; 140 acceptable differences and 41 engine defects remain, and the html5lib-only corpus is **99.77%**.
- **DOM and HTML: 7.0%** - 197 of 2,817 executable tests, with 5,962 scored and 8,060 skipped.
  Per area, `dom/` is **10.8%** and `html/` is **6.5%**.
- **CSS still cannot be run at all**, and the reason is structural rather than a gap in the runner:
  `testcss.js` is an iframe driver, nested contexts exist, but an iframe document is not painted.

**So the honest answer to "how far off is it" is now three numbers rather than one estimate, and
they disagree by an order of magnitude** - a parser that is nearly conformant, a DOM API surface
that is not, and a CSS conformance figure that does not exist. That disagreement *is* the finding.
An average of them would be a lie, and it is why no single percentage is quoted anywhere in this
document.

The DOM and HTML figure is low for a reason that is worth naming rather than excusing: of 2,817
executable tests, the largest single mechanism is **1,564 that lack canvas** and **630 that lack a
painted nested browsing context**, both unimplemented capabilities rather than wrong answers. **The
defects proper are 664 tests**, and the largest defect mechanism - host objects not initialising
Web IDL defaults - accounts for 638 of them across two mechanisms, where **every default is declared
in the interface's IDL, so it is one change per interface rather than 638 changes.**

**How the number arrived is part of it.** The first run reported a clean 7.4% - **and all 210 of
its passes were vacuous**, because the result sink read a field WPT does not define, so every
assertion count was zero. It was caught by noticing that `pass` and
`executed_with_zero_assertions_evaluated` were **the same number**. The count now comes from
wrapping the assertions, **and a pass with zero assertions is scored as a skip rather than a
pass.** That is the fourth instrument defect in this project and the deepest one: the others
produced wrong numbers, and this one produced a fabricated one.

**And one engine finding came out of building the runner:** a stack overflow **aborts the process**,
which `catch_unwind` cannot catch, so a runner's defence against a truncated result file does not
apply. The cause is that `max_call_depth: 4,096` at roughly 2 KiB per frame assumes hundreds of
megabytes while the main thread has 1 MiB - **so the engine's own recursion guard is unreachable.**

**The remaining 1,046 unclassified failures are real and unnamed**, and they are the first thing to
read after the two defect mechanisms.

**That suite also produced the most instructive result in this document, and it is a failure.** It
found that a specification reading had been made **backwards, in the direction that made a broken
engine look fixed**: a bogus comment was changed to end at `-->` when the current specification says
it ends at the next `>`. Two rounds of internal review had missed it, because the implementation was
changed to match the reading and then verified against fixtures built from the new behaviour - so code
and tests agreed and nothing contradicted either. **The resulting figure, one comment on a real
government page growing from 92 characters to 714,283, was written into the gap register and
presented as a find. It was an artefact of the regression.**

The generalisation is worth more than the percentage: **a specification reading verified only against
code you just changed is not verified.** The check has to be an authority the implementation cannot
influence. That is the entire argument for an external suite, and it is why one now exists.

**One more number from the same suite, deliberately not folded into the pass rate:** the tree
comparison agrees on 77% of cases while the **parse-error count agrees on only 40.39%** - the engine
under-reports on 3,100 and over-reports on 835. A reader of the pass rate alone could not see that,
because **a parser can build the right tree and still report the wrong number of errors.**

**The WPT conformance runner has real numbers for `dom/` and `html/`; `css/` is still blocked by
iframe painting.** WPT's CSS tests are `testcss.js` tests, and `testcss.js` is an **iframe driver**.
Nested browsing contexts now exist, but an iframe document is not laid out or painted, and the CSS
computed-value adapter hosts one document. **The largest area with a real cascade, a real selector
matcher and a real layout engine is still the one whose harness cannot run.**

The static census counted 26,614 scored CSS tests, of which 18,174 (68.3%) looked executable. That
is a feasibility count, not a pass rate. `dom/` and `html/` have since been run (above), and the
runner still reports no rate where the denominator is zero, so an empty run prints nothing rather
than `0%`.

This reframes iframes. They are not just one more feature gap: **they block embedded content on
real pages and the external measurement of CSS.** An engine that cannot paint a nested document
cannot be scored by the largest CSS conformance suite in existence, so the absence of the iframe
paint path costs not one page but the project's ability to know whether its CSS is right.

The runner also found **two defects in its own instrument**, both the same failure class this
project keeps meeting: a reference-identity metric whose two sets were in different key spaces, so
the report read an impossible "0 in both, and 8,695 tests dropped" **with no error and no failing
test** - caught only by reading the number; and a fetch that used `Copy-Item -Recurse` onto an
existing cache, producing `css/css/`, which **doubled the whole population to 92,650 while the tree
still looked like a valid checkout.** The census now refuses a self-nested checkout.

One measurement worth keeping: **6,466 of 32,576 tests (19.8%) declare no assertion site at all** -
detected statically *and* at runtime, with method calls and function declarations excluded after the
self-check caught the classifier counting them. **One test in five in the reference suite has no way
to fail**, which is a useful calibration for anyone who reads a conformance percentage.

## The shape of the remaining distance

It is **not a long tail of small gaps.** It is a set of structural absences, and a short list of
them would break more real pages than everything else combined:

1. **Iframe painting** - nested documents run scripts in their own realm, but they are not laid
   out or painted, so any embedded content is invisible
2. **Animations and transitions** - `@keyframes` and CSS transitions are sampled by a
   browser-driven clock with multi-animation lists, transform:none interpolation,
   reversing shortening, reversing-adjusted start values, allow-discrete and
   transitioncancel on style changes/node removal/document switches, but animation
   sampling still runs on the event-loop thread rather than a compositor timeline. A
   self-hosted WAAPI core shares that document clock and
   covers element.animate, play/pause/finish/cancel, playbackRate/reverse,
   an ordered effect stack with add/accumulate composition, per-keyframe easing,
   KeyframeEffect/GroupEffect with nested group local-time transforms and
   independent per-child timing, Effect.updateTiming and animation.effect
   replacement, and finish/cancel events; several timing-model edge cases remain
   incomplete
3. **Shadow-scoped styling** - stylesheets, `:host` and `::slotted` now work; `::part()`,
   declarative shadow roots and nested pseudo-elements on `::slotted()` remain
4. **Selection, bidi, workers and compositing** - the remaining structural absences. The
   retained compositor now has a dedicated scroll worker, a wgpu/D3D12 swapchain on Windows,
   fixed/sticky/affine layers, filter/mask effect surfaces and RTL-aware axes, but animation
   sampling does not share the compositor timeline, and non-normal blend modes, ambient clips
   crossing a layer and nested sticky scopes still take the complete raster

**Closed from this list:** dynamic interaction state is now wired through
`InteractionState` and `Document::render_with_interaction`, so `:hover`, `:active`, `:focus`,
`:focus-visible`, `:focus-within` and `:target` match from real pointer, keyboard, focus and URL
input. Tab/Shift-Tab walk the sequential focus order and every focus path fires
`blur`/`focusout`/`focus`/`focusin`. `:visited` remains unmatched because no visited-link
history is exposed.

**One measurement is deliberately not a claim.** The site-neutrality gate now has 9 rules, scans
312 Rust source files, and **exits 0 with 26 advisory findings**. The blocking violations this
section used to record are gone; the advisories are site names in comments and diagnostic file
names, and they are still reported so the vocabulary does not become invisible. A gate that reads
as enforcement but has stopped detecting is worse than no gate, which is why the self-test runs
first.

**And one symptom is still unexplained.** A large real portal renders some regions as bare
unstyled text. Four candidate causes have been eliminated by measurement, including the lead the
register was opened on - that turned out to be a **defect in the counter that produced it**, not in
the engine, and all 151 of its elements are accounted for. So the box tree for that document is
**correct**, and the remaining candidate is the DOM *after script execution*. It stays an open item
rather than a closure, because two agents - including me - have already been wrong about its cause,
and because the screenshot that established it may itself have been a frame that was never
committed.
