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
| `render-js` unit tests | 1,116 passed, 0 failed | 2026-10-22 |
| `render-core` unit tests | 298 passed, 0 failed | 2026-10-23 |
| `render-layout` unit tests | 306 passed, 0 failed, 1 ignored | 2026-10-22 |
| `render-css` unit tests | 191 passed, 0 failed | 2026-10-22 |
| `render-browser` unit tests | 198 passed, 0 failed | 2026-10-23 |
| `render-browser` app tests (binary) | 99 passed, 0 failed | 2026-10-23 |
| `render-html` unit tests | 149 passed, 0 failed | 2026-10-22 |
| `render-net` unit tests | 32 passed, 0 failed | 2026-10-22 |
| `render-dom` unit tests | 77 passed, 0 failed | 2026-10-22 |
| real-site acceptance harness | 25 passed, 5 `#[ignore]`s each mapped to a named gap | last recorded |
| Python static gate | 15 passed, 55 subtests | last recorded |
| site-neutrality gate | **9 rules, 269 Rust source files, exit 0** with 25 advisory findings | 2026-10-23 |
| **test262** | **90,765 / 103,199 = 88.0%**; 264 buckets, 6 timeout, 0 crash; checked-in baseline floor = 90,765 | **2026-10-22, release profile, pinned revision** |
| **html5lib tree construction** | **5,810 / 7,377 pairs = 78.75%**; 190 acceptable difference, 776 unimplemented, **601 engine defect (8.15%)** | last recorded, pinned |
| html5lib parse-error count agreement | **4,784 / 6,601 = 72.47%** under `max`, **60.60%** under `sum` - the gap between those two is the choice of reading, not the parser | last recorded |
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
at the correct origin, CSS Nesting, `@supports` evaluated against a three-state oracle, the
`hsl()`/`hsla()` family, `color()` in sRGB, sRGB-linear and display-p3. Shorthand expansion is now
spec-correct for `text-decoration`, which is what fixed the most-reported visual symptom in the
project's history.

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
| **Web Workers** | zero occurrences of `Worker` |
| **History API** | `pushState`/`replaceState` change the URL and `history.state` without a load. Back/forward/go within the document fire `popstate` without a load; entries of another document reload. `history.length` reflects the list at each turn's start |
| **Iframe painting** | a nested document runs scripts in its own realm, but it is not laid out or painted, so a page embedding one loses that content visually |
| **Animations and transitions** | `@keyframes` is parsed, diagnosed and discarded - 249 blocks in the corpus. No clock, so nothing can be time-driven |
| **Selection / Range** | Range is implemented (DOM §5): `Range`, `StaticRange`, `createRange`, the boundary-point and content algorithms, and live updates on every tree mutation. There is no `getSelection` selection state, so `getSelection()` reports an empty selection, and no caret position APIs |
| **Shadow-scoped styling** | shadow trees, `attachShadow` and autonomous `customElements` exist; shadow-tree stylesheets are not collected, `:host`/`::slotted` match nothing, and `slotchange` is not dispatched |
| **Compositing** | there is no compositor. Every frame is one raster of the whole document, which is why scrolling cannot be cheap and why `position: fixed` cannot be a layer |
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

- **Tree construction: 78.75%** of 7,377 (test, scripting-mode) pairs pass.
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
   transitioncancel on style changes/node removal/document switches, but there is
   no compositor thread. A self-hosted WAAPI core shares that document clock and
   covers element.animate, play/pause/finish/cancel, playbackRate/reverse,
   an ordered effect stack with add/accumulate composition, per-keyframe easing,
   KeyframeEffect/GroupEffect with nested groups and independent per-child timing,
   and finish/cancel events; nested group local-time transforms and several
   timing-model edge cases remain incomplete
3. **Shadow-scoped styling** - shadow trees exist, but a `<style>` inside a shadow root applies
   nowhere and `:host`/`::slotted` match nothing
4. **Selection, bidi, workers and compositing** - the remaining structural absences. The
   compositor in particular is an architectural fact rather than a gap: it changes what scrolling
   and fixed positioning can cost

**Closed from this list:** dynamic interaction state is now wired through
`InteractionState` and `Document::render_with_interaction`, so `:hover`, `:active`, `:focus`,
`:focus-visible`, `:focus-within` and `:target` match from real pointer, keyboard, focus and URL
input. Tab/Shift-Tab walk the sequential focus order and every focus path fires
`blur`/`focusout`/`focus`/`focusin`. `:visited` remains unmatched because no visited-link
history is exposed.

**One measurement is deliberately not a claim.** The site-neutrality gate now has 9 rules, scans
269 Rust source files, and **exits 0 with 25 advisory findings**. The blocking violations this
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
