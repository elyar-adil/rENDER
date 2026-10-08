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
| `render-js` unit tests | 237 passed, 0 failed | this session |
| `render-core` unit tests | 192 passed, 0 failed | this session |
| `render-layout` unit tests | 202 passed, 0 failed, 1 ignored | this session |
| `render-css` unit tests | 183 passed, 0 failed | this session |
| `render-browser` unit tests | 161 passed, 0 failed | this session |
| `render-html` unit tests | 131 passed, 0 failed | this session |
| `render-net` unit tests | 81 passed, 0 failed | this session |
| `render-dom` unit tests | 29 passed, 0 failed | this session |
| real-site acceptance harness | 25 passed, 5 `#[ignore]`s each mapped to a named gap | this session |
| Python static gate | 15 passed, 55 subtests | this session |
| site-neutrality gate | **9 rules, 173 files, currently exit 1** on 9 genuine violations | this session |
| **test262** | **30,808 / 98,096 = 31.4%** | **2026-09-20, NOT re-measured this session** |
| **html5lib tree construction** | **5,810 / 7,377 pairs = 78.75%**; 190 acceptable difference, 776 unimplemented, **601 engine defect (8.15%)** | this session, pinned |
| html5lib parse-error count agreement | **4,784 / 6,601 = 72.47%** under `max`, **60.60%** under `sum` - the gap between those two is the choice of reading, not the parser | this session |
| **WPT `dom/`** | **32 / 297 executable = 10.8%**, 0 vacuous passes | this session, pinned `c7fdee80f3f1` |
| **WPT `html/`** | **165 / 2,520 executable = 6.5%**, 31 errored, 0 vacuous passes | this session, pinned |
| **WPT combined** | **197 / 2,817 = 7.0%** - 5,962 scored, 8,060 skipped, 1,684 with no assertion site at all | this session, pinned |
| **WPT `css/`** | **cannot be run** - `testcss.js` is an iframe driver and the engine has no nested browsing contexts | - |
| production HTML corpus sweep | **3,718 documents**, 0 of our diagnostic kinds at fault, 3 real defects found and fixed | this session |
| production CSS corpus | 11,102 rules, 42,356 declarations, 1.93 MB | this session |
| production JS | a 2.27 MB real bundle completes; a second probe also runs to completion, 0 threw | this session |
| clippy `-D warnings`, workspace-wide | clean | this session |
| mutation audit | 34 hand-picked mutations, **10 survived (29%)** | this session |

The test262 figure is the only **external** conformance number that exists, and it is five weeks
stale. Everything else is either the engine agreeing with itself or a measurement over real
production code - which is worth a great deal, and is not the same thing as an external authority
checking it. Two runners for that are in progress.

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

**Capability tracking.** `spec/registry.rs` has 36 entries and now **rejects any non-conformant
entry with a blank note**, so it cannot decay back into a list of `Partial`s that says nothing.

## Partial: works, with a named hole

| area | what works | the hole |
| --- | --- | --- |
| inline SVG | shapes, path commands, group transforms, fill/stroke inheritance, `viewBox` scaling, sized from attributes and painted through the ordinary image path | `use`/`symbol`/`defs` indirection, `foreignObject`, aspect-ratio preservation when the box ratio differs from the viewBox |
| scrollports | `ScrollportGeometry` with mode, clip and scrollable range, structurally distinct from `overflow: hidden` clipping | the shell side: no offset store, no scrollbar chrome, no keyboard scrolling, no scroll anchoring, and a sticky box in a *nested* scrollport cannot be resolved |
| position | `static`/`relative`/`absolute`/`sticky` with the constraint rectangle, the containing-block clamp, and the rule that an auto inset is not a zero inset | `z-index` is read as raw text and sorts block siblings only; flex and grid children unsorted; the paint layer has no z-index awareness; only `transform`/`opacity` create a stacking context |
| video | container and bitstream parsing, poster handling | **pixel decode** - no frame is ever rasterised |
| text properties | `text-decoration` (all four longhands, with the spec's omitted-values rule), `text-shadow`, `text-overflow`, `text-indent`, `letter-spacing`, `text-transform` | the font axis landed this session; `font-synthesis` is not in the property registry, so `font-synthesis: none` does nothing yet |
| quirks mode | the flag is wired into the headless path and the rendering rules are implemented with a per-rule citation | two box-model items remain uncited and unimplemented, and they are solver behaviour |
| diagnostics | 606 produced over the production corpus, by kind and per file | **counted but not displayed** - `render-browser` has no mapping from message to `StylesheetDiagnosticCode`, so the panel shows nothing |

## Absent: structural, and this is the real distance

Verified by grep, not inferred. Full discussion in the register's S30.

| absent | consequence |
| --- | --- |
| **ES modules** | **Largely closed.** `render-js` parses import/export into tables (`src/module.rs`) and links them with live bindings (`src/runtime/module.rs`): named/default/namespace imports, re-exports, `export *`, `export * as`, cycles for hoisted declarations, `import.meta.url`, evaluate-once ordering, remembered failures. `render-core/src/module_graph.rs` assembles the graph and `render-browser/src/scripts.rs` fetches it in rounds. **Still open:** dynamic `import()` is a stub that resolves an empty object; top-level `await`; import maps (bare specifiers are diagnosed, not resolved); module code runs sloppy, not strict; a namespace object is a snapshot rather than a live exotic object; classic scripts still skip `import`/`export` instead of throwing a SyntaxError; `type=module` `async` scheduling is treated as defer; `<script type=module>` inserted after load goes through the same path as classic follow-up scripts but is untested in a live window |
| **Line breaking** | there is **no break-opportunity algorithm at all** - text is exploded per character and every character is a wrap opportunity. UAX #14, `kinsoku` and `LineBreak` all have zero occurrences. CJK wrapping is right *by accident*; CJK **kinsoku shori is absent, so punctuation can begin a line**; and `min_content_width` splits on `split_whitespace()`, so **an unspaced Chinese run counts as one unbreakable word** - meaning `width: min-content`, a shrink-to-fit float, an auto table column or a flex item all derive their width from a whole paragraph. Detail in S34 |
| **`DOMException`** | every Web API error is a plain `Error`, so **`instanceof DOMException` is false for all of them** - and that is the idiom real code branches on. `DataCloneError`, selector `SyntaxError`, `NotFoundError`, `AbortError` and the rest |
| **`window.matchMedia`** | every responsive site that gates behaviour in script gets nothing |
| **Form submission** | the owner relationship exists and is derived on read, but there is **no submit event path and no navigation**. A search box and a login do not submit - the single most-used interactive path on the web |
| **Web Workers** | zero occurrences of `Worker` |
| **History API** | `pushState`/`replaceState` change the URL and `history.state` without a load. Back/forward/go within the document fire `popstate` without a load; entries of another document reload. `history.length` reflects the list at each turn's start |
| **Iframes** | no second document, so any page embedding one loses that content entirely |
| **Animations and transitions** | `@keyframes` is parsed, diagnosed and discarded - 249 blocks in the corpus. No clock, so nothing can be time-driven |
| **Selection / Range** | no `getSelection`, `createRange`, or caret position APIs |
| **Shadow DOM** | no `customElements` or `attachShadow` |
| **Compositing** | there is no compositor. Every frame is one raster of the whole document, which is why scrolling cannot be cheap and why `position: fixed` cannot be a layer |
| **Bidi** | UAX #9 has zero occurrences, so right-to-left scripts lay out left-to-right. Correctly lower priority given the corpus, but recorded rather than left to be rediscovered |
| **HTTP/2** | structurally rejected: a pooled connection leaves the pool while in use and is held for the whole request, so one connection carries one in-flight request. Multiplexing is the inverse |
| Multi-column layout | no `column-count`/`column-width` |
| `filter`, `clip-path`, `backdrop-filter`, `mask-image`, `mix-blend-mode` | zero occurrences in the paint layer. Not approximated - absent |
| `WebSocket`, `requestIdleCallback`, `PerformanceObserver` | absent |

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
  `testcss.js` is an iframe driver and the engine has no nested browsing contexts.

**So the honest answer to "how far off is it" is now three numbers rather than one estimate, and
they disagree by an order of magnitude** - a parser that is nearly conformant, a DOM API surface
that is not, and a CSS conformance figure that does not exist. That disagreement *is* the finding.
An average of them would be a lie, and it is why no single percentage is quoted anywhere in this
document.

The DOM and HTML figure is low for a reason that is worth naming rather than excusing: of 2,817
executable tests, the largest single mechanism is **1,564 that lack canvas** and **630 that lack a
nested browsing context**, both unimplemented capabilities rather than wrong answers. **The
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

**The WPT runner is built and the suite is censusued, and it cannot run a single test - and that is
the correct result, reported as such.** WPT's CSS tests are `testcss.js` tests, and `testcss.js` is
an **iframe driver**. `render-core` has no nested browsing contexts. **The largest area with a real
cascade, a real selector matcher and a real layout engine is the one whose harness cannot run.**

| area | scored tests | statically executable | run |
| --- | --- | --- | --- |
| `css/` | 26,614 | 18,174 (68.3%) | 0 |
| `dom/` | 559 | 252 (45.1%) | 0 |
| `html/` | 5,403 | 2,560 (47.4%) | 0 |
| **total** | **32,576** | **20,986 (64.4%)** | **0** |

The runner **has no code path that can report a conformance rate** - `Rate` returns `None` on a
zero denominator, so an empty run prints nothing rather than `0%`. Its defensible claim is static
feasibility, stated as such, and it names the capability that blocks the most: nested browsing
contexts alone block **1,772** of the executable-looking tests.

**And no engine defect list is reported, which the report correctly labels "blocked, not empty"** -
nothing was evaluated, so a defect list would be an artefact of the harness rather than a fact
about the engine.

This reframes iframes. They are not just one more feature gap among twelve: **they are what blocks
the external measurement of everything else.** An engine that cannot host a nested document cannot
be scored by the largest CSS conformance suite in existence, so the absence of iframes is costing
not one page but the project's ability to know whether its CSS is right.

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

It is **not a long tail of small gaps.** It is roughly a dozen structural absences, and a short
list of them would break more real pages than everything else combined:

1. **Form submission** - the search box and the login, on every site. The owner relationship is
   derived and correct; what is missing is the submit event path and the navigation
2. **ES modules** - static graphs now link and run (see the row above); dynamic `import()` and top-level `await` are what remain for modern bundles
3. **`matchMedia`** - every responsive site that gates behaviour in script
4. **Line breaking** - **specific to this project's corpus.** There is no break-opportunity
   algorithm, so kinsoku shori is absent and CJK punctuation can begin a line, and
   `min_content_width` treats an unspaced Chinese paragraph as one unbreakable word, so **every
   intrinsic width on a Chinese page is wrong today**. The first three are mostly a matter of
   wiring up what exists - the form owner is derived and correct, the parser runs script bodies,
   the viewport is known. **This one is a real hole in a core algorithm**, and it is the only item
   on this list that is wrong *right now* on the pages this project is built around

**One measurement is deliberately not a claim.** The site-neutrality gate now has 9 rules and
currently **exits 1** on 9 genuine violations - live tests named after specific commercial sites,
and diagnostic tools reading site-named corpus paths. It reads all of them and reports them, so
nothing fails silently, but `tools/check.sh` and CI are red until they are removed. The gate also
turned out to have **two of its own rules dead for their own target** - `\b` does not fire next to
`_`, so the identifier rule never matched the one name it existed for - which is exactly what a
gate that looks like it is working, and is not, looks like.

Behind the list: iframes, animations, workers, shadow DOM, selection, bidi, and a compositor.
Those are larger, and the compositor in particular is an architectural fact rather than a gap - it
changes what scrolling and fixed positioning can cost.

**And one symptom is still unexplained.** A large real portal renders some regions as bare
unstyled text. Four candidate causes have been eliminated by measurement, including the lead the
register was opened on - that turned out to be a **defect in the counter that produced it**, not in
the engine, and all 151 of its elements are accounted for. So the box tree for that document is
**correct**, and the remaining candidate is the DOM *after script execution*. It stays an open item
rather than a closure, because two agents - including me - have already been wrong about its cause,
and because the screenshot that established it may itself have been a frame that was never
committed.
