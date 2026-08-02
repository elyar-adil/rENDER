# Rust Migration and Product Architecture

## Product objective

rENDER is becoming a complete, lightweight desktop browser with a self-owned
Rust engine. It is not a Chromium shell and does not delegate runtime rendering
to a system WebView. The native GUI is a first-class browser product, while
headless Agent workflows, automation, embedding, and future Python bindings all
consume the exact same web-platform semantics and rendering pipeline.

The long-term layers are:

```text
Native browser GUI / Agent API / Python and C host bindings
            |
Agent API: Browser, Page, Observation, Policy, Trace
            |
Automation: actions, waits, network interception, record/replay
            |
Web platform: DOM, HTML, CSS, JS, events, navigation, fetch
            |
Platform adapters: headless, native window, fonts, images, networking
```

The core must not depend on Qt, Python, a GUI event loop, or a system browser.
Platform and language bindings depend on the core, never the reverse.

## Sources of truth

Migration is not a line-by-line port of the current Python implementation.
Behavior is decided in this order:

1. WHATWG HTML and DOM, CSS specifications, and ECMAScript specifications.
2. Applicable Web Platform Tests and test262 cases.
3. Interoperable behavior observed in current Edge/Chromium and other engines.
4. Existing rENDER regression tests and Python behavior.

When Python disagrees with the standards or interoperable browser behavior,
Rust implements the correct behavior and adds a focused regression test that
documents the intentional difference. Real-site failures must be reduced to
generic standards tests; host, URL, and site-specific rendering branches remain
forbidden.

Differential tests are therefore split conceptually into two groups: values that
must remain equal because Python was already correct, and named intentional
differences where permissive Python behavior is replaced by standards-correct
Rust behavior. A raw "all outputs must match" gate is explicitly forbidden.

## Migration rules

- Keep Python runnable until a Rust subsystem passes its conformance and
  integration gates; switch one stable boundary at a time.
- Prefer pure, deterministic modules first, followed by DOM/style/layout,
  JavaScript and its event loop, networking, and finally GUI adapters.
- Every Rust subsystem exposes explicit errors and resource budgets. Silent
  syntax skipping is not an acceptable compatibility strategy.
- Stable node identifiers, structured observations, trace events, virtual time,
  network recording, and page-level capability policies are core design inputs,
  not wrappers added after rendering works.
- Python is a first-class ABI3 wheel built with PyO3. The public Python package
  stays separate from the native extension so its async Agent API can remain
  stable while internals migrate.

## Initial crate path

`render-core` began with standards-driven CSS numeric and length evaluation and
now owns the Rust DOM, HTML parsing, selector/cascade/computed-value slices,
formatting and fragment trees, display-list construction, CPU reference paint,
and deterministic event-loop foundation. Number and length dimensions remain
distinct, unresolved percentage and font-metric requirements remain explicit,
and invalid input is rejected instead of inheriting permissive Python behavior.

New crate boundaries are introduced only when they contain real behavior.
JavaScript, networking, the native browser shell, Agent runtime, and platform
backends will therefore grow around tested vertical slices instead of empty
placeholder crates.

The tracked `tests/fixtures/interop/css_math_oracle.html` fixture can be opened
or dumped with a current Edge/Chromium build to verify the initial CSS math
contract independently of both implementations. Browser output is evidence for
interoperability, while specification and WPT results remain authoritative.

## Current Rust foundation

- CSS math resolves typed number/length expressions without silently inventing
  units, containing blocks, or font metrics.
- The DOM arena assigns stable, non-reused `NodeId` values and implements
  pre-insert validation, node movement, document-fragment expansion, document
  child constraints, HTML name normalization, and a monotonic mutation revision.
- DOM mutations are validated before detaching any node, so a failed operation
  is atomic. This is required for reliable JavaScript exceptions, rendering
  invalidation, and Agent traces.
- The Python ABI3 wheel currently exposes the first migrated CSS capability.
  It also exposes `parse_html_snapshot()`, an immutable structured DOM snapshot
  with stable node IDs, quirks mode, and parse diagnostics. Mutable DOM objects
  will be exposed through the future `Browser`/`Page` API rather than freezing a
  premature public wrapper around internal arena types.
- The first incremental HTML tokenizer/tree-builder slice covers complete named
  and numeric character references, attributes and duplicate recovery,
  comments/doctypes, RCDATA/RAWTEXT/script data, implicit html/head/body,
  common optional end tags, and core table insertion modes including foster
  parenting.
- The selector slice parses strict top-level selector lists and supports the
  common Selectors Level 3/4 surface: type, ID, class and attribute selectors;
  all four combinators; structural pseudo-classes; forgiving `:is()`,
  `:where()`, `:not()` and relative `:has()` lists; `An+B of S`; CSS escapes;
  dynamic state through an explicit match context; pseudo-element host
  selection; quirks-mode ID/class matching; and Level 4 specificity. The
  Python wheel exposes this through `query_html_snapshot()` without creating a
  second DOM identity space.
- Focused Edge interoperability fixtures cover parser recovery and selector
  behavior. Named differential tests document intentional corrections where
  the Python reference currently mishandles whitespace in `:empty`, lacks
  `:nth-child(... of S)`, or assigns non-zero specificity to `:where()`.
- The first stylesheet/cascade slice uses the Servo `cssparser` tokenizer for
  CSS Syntax recovery rather than copying Python's string splitting. It keeps
  parsed selector ASTs on style rules, preserves custom-property name case,
  identifies trailing `!important` across comments and whitespace, and selects
  cascaded winners by actual matching specificity, origin, top-level layer,
  importance, and source order. `cascade_html_snapshot()` exposes cascaded
  values to Python without pretending that they are computed or used values.
  A focused Edge fixture confirms author-layer ordering, reversed important
  layer order, source order, custom-property case, nested component values, and
  matching-selector specificity. Cascade rollback keeps lower candidates so
  `revert` and `revert-layer` work without reconstructing discarded rules;
  Edge confirms that an unlayered `revert-layer` exposes the last explicit
  layer in the same origin.
- The first computed-value slice resolves the document in parent-before-child
  order. Unregistered custom properties inherit their parent's already
  computed token stream, `var()` supports nested and empty fallbacks, dependency
  cycles remain invalid even when cycle edges contain fallbacks, and ordinary
  properties apply `inherit`, `initial`, and `unset` using explicit property
  metadata. Synthetic token boundaries prevent `var(--n)px` from being
  re-tokenized incorrectly as a dimension. Limits on custom-property count,
  component count/depth, dependency depth, and serialized bytes make this path
  suitable for hostile Agent workloads. `computed_html_snapshot()` exposes
  values, invalid custom-property names, and diagnostics to Python. Its Edge
  oracle covers inherited computed custom properties, case sensitivity,
  nested/comma/empty fallbacks, cyclic variables, and invalid-at-computed-value
  fallback to inherited or initial values.
- Typed CSS property values preserve `calc()` and percentage dependencies until
  used-value resolution. The immutable formatting tree, block/inline reference
  layout, fragment tree, and display list are all stamped with the DOM revision
  they consumed. Flex, grid, table, positioning, and unsupported paint commands
  report explicit diagnostics instead of masquerading as complete support.
- Every DOM child, attribute, and character-data update enters a bounded
  `MutationJournal`. Independent consumers can request a `MutationBatch` since
  their own revision; loss of required history forces an explicit full refresh.
- The document pipeline parses once, recollects embedded author styles from the
  current DOM, cascades UA and author origins, computes style, lays out fragments,
  builds a stable-ID display list, diffs dirty geometry, and produces a CPU
  reference surface. Inline, media-qualified, and external CSS paths that are
  not wired yet produce structured diagnostics.
- The deterministic event loop models typed task sources, FIFO tasks, complete
  microtask checkpoints, resource-bounded virtual timers, and a rendering
  opportunity after each turn. Rendering decisions consume the same mutation
  journal used by the document pipeline.
- Classic-script discovery now distinguishes parser-blocking, `async`, and
  `defer` external scripts while correctly ignoring those attributes on inline
  classic scripts. Until module execution lands, `nomodule` classic scripts are
  executed as the standards-defined compatibility fallback. The browser
  resource adapter preserves that scheduling
  metadata through fetch, MIME/UTF-8 validation, and compilation. Embeddings
  can submit the resulting revision-bound compiled batch to `Page` without
  reparsing source or creating a second DOM/Realm; stale batches fail
  atomically.

## Fully functional browser critical path

The next compatibility work is ordered by whether ordinary applications can
boot and remain interactive, not by raw feature count:

1. Complete classic-script lifecycle semantics: parser blocking during tree
   construction, independent async completion tasks, ordered defer execution,
   load/error events, `DOMContentLoaded`, cancellation on navigation, and
   dynamic script insertion.
2. Add ECMAScript modules with URL-keyed module maps, dependency fetching,
   linking/evaluation, import maps, and top-level await. Module support must use
   the same page Realm and network policy as classic scripts.
3. Expand event targets and Web APIs needed by application bootstraps: complete
   capture/target/bubble dispatch, default actions, `fetch`, abort signals,
   URL APIs, storage policy, mutation observers, and navigation/location.
4. Close layout and paint blockers after dynamic applications can boot:
   intrinsic sizing, positioned/fixed/sticky layout, replaced elements,
   stacking contexts, transforms, overflow/clipping, and font shaping.
5. Add an end-to-end media pipeline: ranged resource loading, media element
   state, MP4/DASH demuxing, H.264/AAC decode, audio output, A/V clocks,
   compositing, and the Media Source Extensions subset used by major video
   sites.

The native browser now keeps one persistent `Page` per committed document.
External classic-script completions are revision-bound, queued into that
page's Realm and event loop, and DOM mutations feed subsequent display lists.
Navigation and source replacement cancel outstanding script batches.

Passing a hand-picked JavaScript subset or rendering a static first frame is
not a completion criterion. A capability is complete only when the native
browser and embedding API exercise the same persistent DOM, Realm, event loop,
network loader, and rendering revisions.

Remaining HTML parser work includes the adoption agency algorithm, templates,
select-specific modes, foreign SVG/MathML content, form pointers, and the full
script escaped-state family. Remaining selector work includes namespaces,
complete disabled-fieldset semantics, shadow-tree selectors, and advanced
pseudo-elements. These are tracked as missing standards capability, not
silently approximated as complete. Remaining CSS parsing/cascade work includes
grouping-condition evaluation (`@media`, `@supports`, and containers), imports,
nested/hierarchical and anonymous sublayers, CSS nesting, inline style,
animations/transitions, registered custom properties (`@property`), and the
full property registry. Property-specific grammar validation and canonical
computed conversions exist for the first layout-driving subset and must expand
across the remaining properties; unsupported properties are not claimed as
complete. Specified, cascaded, computed, used, and actual values remain explicit
boundaries rather than one mutable style dictionary. JavaScript bindings consume
the same `NodeId` identity instead of maintaining a parallel tree.

## Local Rust and Python binding checks

```powershell
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings

python -m venv .venv
.venv\Scripts\Activate.ps1
python -m pip install maturin pytest
maturin develop --manifest-path bindings/python/Cargo.toml
python -m pytest -q bindings/python/tests

# Optional installed Edge/Chromium interoperability comparison
python tests/html_interop_oracle.py
python tests/selector_interop_oracle.py
python tests/cascade_interop_oracle.py
python tests/computed_interop_oracle.py
```
