# rENDER Improvement Plan

This document turns the current project gap analysis into a reviewable execution
plan. It is intentionally capability-oriented: fixes should improve the generic
engine rather than special-case individual websites.

## Guiding principles

- Keep runtime rendering on rENDER's own engine path. Do not delegate rendering
  to Chromium, a system WebView, screenshots, remote prerendering, or
  host-specific adapters.
- Prefer standards and interoperability evidence over Python prototype behavior.
  The Python implementation remains useful as a runnable migration reference,
  not as the final behavioral specification.
- Reduce every real-page failure to a generic fixture or standards-derived test
  before landing the engine fix.
- Migrate Rust subsystems one stable boundary at a time, keeping the Python path
  runnable until the replacement passes its conformance and integration gates.

## Priority 0: Runtime integrity and architecture

1. Keep `engine.py` and the native Rust browser on a single generic document
   pipeline: parse, load resources, execute supported scripts, compute style,
   layout, and paint.
2. Keep browser-comparison tooling in tests only; never use another browser as a
   runtime fallback.
3. Separate CI jobs for headless Rust core crates and platform GUI crates so
   native windowing dependencies cannot block core-engine feedback on unsupported
   runners.
4. Add explicit diagnostics for unsupported Rust subsystems instead of silently
   approximating complete support.

## Priority 1: JavaScript and the event loop

Modern pages often fail before rendering meaningful content if their bootstrap
scripts cannot run. The first compatibility push should therefore focus on the
minimum event-loop and language surface needed by common frameworks.

- Implement browser-like task sources for scripts, timers, network callbacks,
  and user interaction.
- Implement microtask checkpoints for `Promise.then`, `queueMicrotask`, and
  async continuation scheduling.
- Connect rendering opportunities to DOM mutation invalidation so script-driven
  changes can trigger style, layout, and paint flushes.
- Expand ES2015+ syntax and built-ins along real bootstrap paths: `let`,
  `const`, classes, arrow functions, destructuring, template strings, `Map`,
  `Set`, `Symbol`, and Promise combinators.
- Implement module-script loading as a dependency graph with browser-compatible
  execution order rather than a text-only parser feature.

## Priority 2: DOM, events, and Web APIs

Hydration frameworks depend on mutation, traversal, event, and geometry APIs.
These should be implemented before lower-value long-tail APIs.

- Complete common mutation APIs: `append`, `prepend`, `before`, `after`,
  `remove`, `replaceWith`, `insertAdjacentHTML`, and deep `cloneNode`.
- Complete traversal and matching APIs: `children`, sibling accessors,
  `matches`, `closest`, and `dataset`.
- Implement event dispatch with capture, target, bubble phases, listener options,
  `stopPropagation`, `stopImmediatePropagation`, and `preventDefault`.
- Add mouse, keyboard, form, focus, and blur event coverage that is driven by the
  same event loop used for scripts and rendering.
- Implement `getBoundingClientRect` from the layout result so scripts can observe
  geometry consistently.

## Priority 3: CSS selector, cascade, and computed-value correctness

CSS work should preserve the boundaries between specified, cascaded, computed,
used, and actual values. Avoid premature pixel conversion when percentages,
font metrics, custom properties, or layout context are still unresolved.

- Finish high-frequency selectors: full attribute selectors, structural
  pseudo-classes, `:not()`, `:is()`, `:where()`, and generated-content host
  selection for `::before` and `::after`.
- Finish cascade ordering across origin, importance, specificity, source order,
  and supported layer behavior.
- Preserve custom-property token streams through computed-value resolution,
  including nested fallbacks and cycle diagnostics.
- Expand property grammar validation for layout-driving properties before
  claiming broader CSS support.
- Keep unsupported CSS visible through diagnostics or counters while discarding
  invalid declarations according to CSS error-recovery rules.

## Priority 4: Layout and painting compatibility

Focus on features that visibly affect ordinary documents and portals before
expensive long-tail graphics APIs.

- Improve intrinsic sizing, shrink-to-fit, min-content, max-content, and
  fit-content behavior.
- Complete margin collapsing, block formatting context boundaries, and stacking
  context behavior.
- Continue flex, grid, table, float, and positioned-layout fixes through reduced
  generic fixtures.
- Implement pseudo-element painting, overflow clipping, SVG-as-image, common
  gradient forms, box shadows, and high-DPI consistent painting.
- Add explicit diagnostics for unsupported transforms, filters, media, and
  replaced content rather than silently painting incorrect output.

## Priority 5: Resource loading, networking, and security

The loading model should become browser-like without adding site-specific
fetching paths.

- Model blocking, async, defer, module, stylesheet, image, and font loading order
  with deterministic tests.
- Enforce HTTPS certificate validation, same-origin policy, file/remote
  isolation, and mixed-content blocking.
- Implement basic CORS, credentials handling, cookie semantics, and referrer
  policy for `fetch` and XHR.
- Add per-origin connection limits, request priority, redirect handling,
  charset sniffing, and memory-cache validation using `Cache-Control`, `ETag`,
  and `Last-Modified`.
- Keep resource failures isolated so a failed subresource produces diagnostics or
  placeholders without crashing the whole page.

## Priority 6: Testing and acceptance gates

The project should keep the existing layered strategy and make it more explicit
in CI.

- Run fast parser, cascade, layout, event-loop, and networking unit tests on
  every pull request.
- Require page rendering contract tests for changes that affect visible output.
- Run browser visual regression comparisons in nightly or release jobs, using
  them as evidence to create reduced contract tests rather than as the only
  merge gate.
- Import curated WPT subsets for HTML parser recovery, DOM nodes/events, CSS2,
  flexbox, grid, cascade, URL, encoding, fetch, redirects, and XHR.
- Import focused test262 subsets for the JavaScript features needed by the M2
  bootstrap surface.
- Track milestone pass rates for M1 static documents, M2 script-driven pages,
  and M3 basic SPA functionality.

## Suggested near-term milestones

### M1: Static documents are readable

- Parser recovery for common malformed markup.
- Selector and cascade fixes for author stylesheets.
- Intrinsic sizing, overflow clipping, pseudo-elements, SVG-as-image, and common
  background/gradient behavior.
- Charset sniffing and robust HTTPS/redirect handling.
- WPT subsets for HTML syntax and CSS layout basics.

### M2: Script-driven pages initialize and respond

- Event loop, Promise microtasks, timers, and rendering opportunities.
- DOM mutation and event dispatch APIs used by hydration frameworks.
- `fetch`, XHR, CORS basics, cookies, and local/session storage.
- Script loading semantics for blocking, async, defer, and initial module-script
  support.

### M3: Basic modern applications are usable

- History API, route changes, `requestAnimationFrame`, and matchMedia updates.
- Custom elements, open shadow roots, slots, and template content.
- Basic transitions/animations where they affect usability.
- Selected SPA fixtures with first-screen and navigation assertions.
