# WPT runner: what the numbers mean

Findings from the Web Platform Tests conformance runner at `tests/wpt_runner/`,
run against WPT revision **`c7fdee80f3f17b4e9813964916afdfd57ace863f`**.

Everything below was measured, not estimated, unless a line says otherwise.

## Update: classic external scripts, the walk, and the false passes they exposed

The baseline for this update is `3210679`, whose tree is identical to master
`ed4d7cb`. The walk is the same dom/ and html/ walk, measured again on this branch.

**What changed**

1. **The walk no longer counts helper files.** Files under `resources/` and
   `support/` are loaded by tests and are not tests. The walk and the census share
   `census::is_helper_path`. The walk drops 574 rows (dom 7, html 567). The census gains
   a `resources-fixture` exclusion (dom 5, html 456).
2. **Classic `<script src>` runs.** Order and scheduling come from render-core's own
   `discover_scripts`. Parser-blocking scripts run where they appear, so an external one
   runs before the inline scripts after it. Then `async`, then `defer`, all before
   `DOMContentLoaded`. Each `src` resolves against the test URL and is read from the
   checkout. `testharness.js` is not run twice. Module scripts are not run, and the count
   is a note on the result.
3. **A declared script the runner cannot serve is a skip, not a failure.** An absent or
   unreadable file is `missing-fixture`. A script the WPT server generates (`.sub.`
   templates, `.py` handlers) is `server-generated-fixture`. Another origin or scheme is
   `script-from-elsewhere`. The decision is made before the test runs, so no test runs
   part way. The document loader refuses the same files when a nested browsing context
   asks for them.
4. **`window.onerror` takes the five-argument form** (HTML 8.1.8.1). An uncaught error
   reaches it as an ErrorEvent: `(message, filename, lineno, colno, error)`. A `true`
   return cancels the event, and `filename` is the classic script's URL for top-level
   code. `addEventListener('error')` is unchanged. Four render-js tests cover this.
5. **A file that ends in a harness ERROR cannot pass.** testharness reports an uncaught
   exception as status ERROR. The subtests that passed before it stopped do not cover the
   file. The adapter previously ignored that status when subtests existed.

**Numbers, dom/ and html/ walk (10,334 tests)**

| | baseline | this branch |
| --- | ---: | ---: |
| pass | 301 | **338** |
| fail | 3,383 | 3,715 |
| skip | 6,622 | 6,253 |
| error | 28 | 28 |
| pass / (pass + fail) | 8.17% (301 / 3,684) | 8.34% (338 / 4,053) |

Denominators. Rows: 10,908 → 10,334, with 574 helper rows removed. Census scored
population: about 6,095 → 5,634 (dom 554, html 5,080). The old figure is an estimate: the
461 files now under `resources-fixture` were scored before this change. Executed
(pass + fail): 3,684 → 4,053.

The raw pass count rises by 37: 52 gained and 15 lost, each listed below. The rate rises
less than the count because many FAIL rows became skips, which raises the executed
denominator by 369. Seven of the gains are the window.onerror tests, which failed only
because `window.onerror` never ran.

**The 1,623 baseline FAIL rows that say "X is not defined"**

- 39 now PASS.
- 456 now fail on a real assertion or a real gap: assertion-unclassified 283,
  wrong-value 82, value-shape 68, host-property-missing 16, host-not-iterable 7.
- 84 now fail as `needs-testdriver`. The helper loads, and its automation stubs throw
  "not implemented by testdriver-vendor.js". That is not an engine result.
- 528 still say "X is not defined". The names are not helper globals: named access to
  `id`ed elements (`item1`, `button`, `target`), DOMParser, Range, and similar. They are
  unchanged engine gaps.
- 516 became skips: 184 server-generated fixtures, 12 missing fixtures, 150 with no
  verdict or capability, 142 with no assertion site, and 28 stalls (below).

**Stalls, measured from the result notes (dom/ and html/ walk)**

| | baseline | this branch |
| --- | ---: | ---: |
| tests started, none finished | 287 | 507 |
| subtests registered but unreported | 59 | 141 |
| no testharness test registered | 2,150 | 2,184 |
| status NOT-RUN | 1,028 | 68 |

Of the baseline FAIL rows that became skips, 217 became "started, none finished". The
causes are capability gaps, not drain or timer faults. About 119 are testdriver tests
that wait for automation the runner does not provide. About 92 are `svg-in-img` tests
that wait for an image `load` event. The runner fetches no images, so that event never
fires. About 70 unreported-subtest rows are nested documents served by WPT handlers
(`moving-between-documents`, `coop-coep-sandbox`). The drain bound of 512 turns is
unchanged, and no stall was hidden.

**Canvas, and a decision I reversed.** 942 canvas tests were NOT-RUN in the baseline and
now FAIL with `value of callee.getContext is undefined`. The census classes them
`lacks-canvas`, but the canvas call sits in `canvas-tests.js`, which the runner did not
load before. I tried declining on the text of loaded helpers. That over-declined three
tests that passed, because a helper defines a canvas function that the test never calls
(`common.js`), and testharness's testdriver files mention network APIs. I reverted it.
These rows are honest runtime failures on a missing API.

**The 15 lost passes, each explained**

- *Subset passes.* The old adapter counted only the subtests that ran before an error or
  a missing helper stopped the file. `EventTarget-dispatchEvent` (the `aliases` helper
  was not loaded, so 20 subtests were never created; the BeforeUnloadEvent alias is a
  real gap). `location-prototype-setting-same-origin` and
  `windowproxy-prototype-setting-same-origin` (a ReferenceError from a missing helper;
  the immutable-prototype check is a real gap). `window-composed-path`, `interface-objects`,
  `microtask_after_script` and `nested-options` (uncaught exceptions after some subtests).
- *The old scanner ran code the spec makes inert.* `template-for-script-eval` ran a
  script inside `<template>`. It is now discovered as inert, and `template for` is not
  implemented.
- *Module bodies ran as classic scripts.* `inline-async-onload`, `inline-defer-onload`,
  `inline-no-async-onload`, `invalid-import-errors-order`, `error-and-slow-dependency`
  and `script-for-event`. These passed because module code ran with classic semantics.
  Module scripts are not executed now, so they fail or have no verdict.
- *A real gap that the onerror change exposed.* `compile-error-in-body-onerror` passed
  because `window.onerror` never ran. The test needs `<body onerror>` to forward to the
  window and clear it on a compile error, which is not implemented.

Every lost pass is a false pass or a real gap, with no unexplained loss.

**What is left, in order of rows affected**

1. Testdriver automation and image loading (about 211 stalled rows).
2. Named access to `id`ed elements on the window (about 528 rows).
3. Module scripts: import/export and deferred module execution (about 413 tests).
4. Nested documents from WPT handlers, which need a served mode rather than files.
5. `<body>` and `<frameset>` window event-handler attributes, and `template for`.
6. Source positions for inline scripts. `lineno` is relative to the script text, not to
   the document, so `runtime-error-in-window-onerror` cannot see the line it expects.
7. Capability detection by text over-declines (`common.js`), and needs a runtime check.

## Update: a scorer bug, and nested browsing contexts

Two changes to the numbers below.

1. **The scorer read one subtest per file.** `Harvest::parse` stepped over each
   subtest but not the comma between them, so a file's verdict was the verdict of
   its first subtest alone. Master carried the same bug. The tables in the older
   sections were measured with it, so their pass and fail counts are not reliable
   for files with more than one subtest. The fix is in `wptdom.rs`, with the unit
   test `a_harvest_reads_every_subtest_not_only_the_first`, and a second rule: a
   file cannot pass when the harness registered more subtests than reached the
   runner.

2. **Nested browsing contexts are driven.** `NestedBrowsingContext` and
   `WindowProxy` are reported available for the dom/html adapter, so the
   `lacks-nested-browsing-context` and `lacks-window-proxy` mechanisms no longer
   occur there. Measured on `dom/` and `html/` (10,908 tests, pinned revision
   `c7fdee80`), with the same runner in every column except the first:

   | | master as shipped | master + scorer fix | branch |
   | --- | ---: | ---: | ---: |
   | pass | 332 | 232 | 250 |
   | fail | 2,157 | 2,248 | 2,635 |
   | skipped | 8,388 | 8,397 | 7,992 |
   | error | 31 | 31 | 31 |
   | skipped: `lacks-nested-browsing-context` | 643 | 643 | 0 |
   | skipped: `lacks-window-proxy` | 106 | 106 | 0 |

   Branch against master with the scorer fix: 0 lost passes, 18 gained, compared
   key by key on path. Against master as shipped, 98 passes are not passes on the
   branch; the scorer fix alone accounts for 100 of them. The one false pass that
   master as shipped had, and the reason for each figure, are in
   `docs/visual_fidelity_gaps.md`, S11.

## The headline, and what changed

**Last round reported no number at all.** It established that none of the 20,986
statically executable tests ran, because the CSS tests are `testcss.js` tests and
`testcss.js` is an iframe driver. It declined to report a conformance rate, which
was correct.

**This round executed them.** `dom/` and `html/` run, through WPT's own
`testharness.js`, executed unmodified by the engine's own JavaScript runtime,
with results read back through `add_result_callback`. The runner does not
reimplement `test()`, `assert_equals()`, `promise_test()` or `async_test()`, and
it does not interpret a test's source to decide whether it passed.

| Area | scored tests | **executable population** | passed | failed | **rate** |
| --- | --- | --- | --- | --- | --- |
| `dom/` | 559 | **297** | 32 | 265 | **10.8% (32/297)** |
| `html/` | 5,403 | **2,520** | 165 | 2,355 | **6.5% (165/2,520)** |
| **total** | **5,962** | **2,817** | **197** | **2,620** | **7.0% (197/2,817)** |

Plus **31 harness errors** and **8,060 skips**, counted as neither pass nor fail.
**Zero passes evaluated zero assertions** — every one of the 197 is a real
assertion that held, 2,073 assertions in total.

### The denominator, stated three ways, because they differ

| | count | what it is |
| --- | --- | --- |
| `.html` files present across `dom/` + `html/` | **10,908** | what is on disk |
| **scored test population** | **5,962** | after named exclusions: 1,696 reference, 108 support, 346 manual, 14 tooling, 31 unreadable, 2,495 canvas, 960 browsers |
| **executable population** | **2,817** | what actually produced a pass or a fail |

Last round statically predicted 2,812 executable in these two areas (20,986
across all three). The actual population is **2,817** — five higher. The two are
close, but they are different things and the difference matters: the estimate
came from `classify()` reasoning about a file's source, and the 2,817 came from
running it. Five disagreements is the static classifier being nearly right, not
right, and the executable number is the only one a rate may be computed over.

**Across all three areas the static executable count rose from 20,986 to 21,607**
once the `setup()` blocker was removed. The census still totals **32,576** scored
tests and **6,466** that cannot fail — both unchanged — so the +621 is the
`setup()` fix and nothing else moved.

**`css/` is still blocked** and contributes nothing to the rate above. It is not
in the table because it produced no verdicts at all; see *What still blocks CSS*.

## The ranked mechanism list

This is the deliverable. 9,192 test results across 103 named mechanisms, with the
cause spelled out and the category attached.

| # | tests | mechanism | category |
| --- | --- | --- | --- |
| 1 | **2,596** | `no-assertion-site` — the test declares no assertion and no reftest reference, so it has no way to fail **in any browser** | harness |
| 2 | **1,564** | `lacks-canvas` — needs a 2D or WebGL canvas context | unimplemented |
| 3 | **1,046** | `assertion-unclassified` — a real failure this runner could not attribute to a named cause. **Needs a human to read** | harness |
| 4 | **655** | `missing-fixture` — a declared fixture absent from this cache. **A fetch limit, not a suite defect** | harness |
| 5 | **630** | `lacks-nested-browsing-context` — an `iframe` renders as nothing | unimplemented |
| 6 | **484** | `value-shape-differs` — wrong type, or a collection of the wrong length | **engine defect** |
| 7 | **307** | `needs-testdriver` — uses `/resources/testdriver.js`, WPT's second harness. **The same problem as #5 in a different file** | unimplemented |
| 8 | **231** | `host-property-missing` — a property the engine's host object does not have | unimplemented |
| 9 | **197** | `lacks-animation` — needs `requestAnimationFrame` or `Element.animate` | unimplemented |
| 10 | **197** | `no-mechanism-recorded` — passed, and this runner did not record why | harness |
| 11 | **165** | `lacks-network-fetch` — needs `fetch`/`XHR` against a real network | unimplemented |
| 12 | **158** | `missing-support-helper` — a global the test's own `support/` file should define | harness |
| 13 | **154** | `idl-default-not-initialised` — a Web IDL declared default is `undefined` rather than its declared value | **engine defect** |
| 14 | **152** | `lacks-dedicated-worker` — needs a `Worker` realm | unimplemented |
| 15 | **132** | `lacks-navigation` — needs cross-document navigation or `history` | unimplemented |
| 16 | **104** | `lacks-window-proxy` — reads `parent`/`top` | unimplemented |
| 17 | **79** | `lacks-shadow-dom` — needs `attachShadow`/`ShadowRoot` | unimplemented |
| 18 | **31** | `test-file-unreadable` — not valid UTF-8; a charset test | harness |
| 19 | **26** | `lacks-dynamic-import` — dynamic `import()` | unimplemented |
| 20 | **25** | `promise-shape-differs` — a non-promise returned, or one that did not settle as expected | **engine defect** |

### Four categories, never summed

| category | tests | meaning |
| --- | --- | --- |
| **engine-defect** | **664** | the engine produced a wrong answer. A bug. |
| acceptable-difference | **0** | a documented, defensible divergence |
| unimplemented-feature | **3,837** | a missing API. A roadmap item. |
| harness-limitation | **4,691** | a gap in **this runner**. Not evidence about the engine. |

**The defect list is 664 tests, and 638 of them are two mechanisms.** That is the
useful fact: a ranked list of 2,620 failing paths tells a reader nothing, and
`value-shape-differs` + `idl-default-not-initialised` + `promise-shape-differs`
tells them exactly what to fix first.

### What to do first, in order

1. **Host objects do not initialise their Web IDL defaults** — 484 + 154 tests.
   `Event.cancelBubble` must default to `false`; `cancelable` to `false`;
   `HTMLCollection` must be a live object rather than `undefined`. Every default
   is *declared in the interface's IDL*, so the fix is to read the declared
   default when constructing each host object — one change per interface, not 638
   changes. This is the single highest-leverage defect fix in the project, and it
   is the only category here that is unambiguously a bug.
2. **`document.createElementNS` is not implemented** — see below. One method.
3. **Nested browsing contexts** — 630 tests, plus 307 more that need
   `/resources/testdriver.js`, which is the same limitation in a different file.
   Together, **937 tests**, and it is the only blocker gating the CSS area.
4. **Widen the fetch** — 535 tests blocked by fixtures this cache does not hold,
   across 61 distinct fixtures and 15 trees. **Not a single line of engine work.**
   Run `powershell -File tools/wpt/run.ps1 -Fixtures` for the list.
5. **Read the 1,046 unclassified failures** — they are real failures this runner
   could not name. They are in `wpt-results.jsonl`, and they are the largest
   block of unexplained evidence in the run.

## The negative control, and what it caught

`wpt-runner negative-control` runs the pipeline against adapters that must fail.
**Both halves held.**

**Half 1 — an adapter that cannot fail.** It produces a 100.0% (152/152) rate in
which *every* pass evaluated zero assertions, and the census veto independently
demoted 283 further tests to skip. Both are visible in the output, not just in
the numerator.

**Half 2 — the real adapter, through WPT's own harness.** Three cases:

| case | expected | observed |
| --- | --- | --- |
| `assert_equals(1, 2)` | fail, with the assertion | **fail** ✓ |
| `assert_true(false)` | fail, with the assertion | **fail** ✓ |
| `assert_equals(1, 1)` | **pass, with a non-zero assertion count** | **pass, evaluated=1** ✓ |

### What the control actually caught

**It caught this round's most dangerous result.** The first full run reported
**210 passes — and all 210 had evaluated zero assertions.** The rate read a clean
`7.4% (210/2830)` and was made *entirely* of tests that asserted nothing. No
error, no failing test, no implausible number. The cause was a single wrong
field name: the result sink read `test.asserts`, which `testharness.js` does not
define, so every count came back `undefined` and became 0.

It was caught by reading the report and noticing that `pass` and
`executed_with_zero_assertions_evaluated` were *the same number, 210*. That is
the trap from last round again — a metric wrong in a consistent direction — and
the only defence is reading the number.

Two things were then fixed, and both are permanent:

- the assertion count now comes from **wrapping** every `assert_*` entry point, so
  it counts what the engine actually evaluated;
- **a pass with zero evaluated assertions is not a pass.** It becomes an
  explained skip. That can only lower the rate, never raise it.

The third control case exists solely because of this. A control that checked only
failures would have passed against that run: it *could* fail, it just could not
count a success.

**A second control bug, also caught.** The first version of half 1 asserted
`pass == total_seen`, and reported `THE CONTROL FAILED` against a pipeline
working correctly — the census veto legitimately turns some adapter passes into
skips. A miscalibrated control is worse than none, because it cries wolf until
somebody turns it off.

## Four defects found in the *instrument*, not the engine

Reported because each would have produced a wrong number while looking healthy.

**1. 210 of 210 passes were vacuous** (fixed). Described above. The most
consequential bug this project has produced through a *measurement* tool, and the
reason the third control case exists.

**2. The `setup()` blocker was wrong, and cost 772 tests** (fixed). Last round
recorded the legacy `setup()` harness as unsupported and skipped 772 tests on it.
`testharness.js` implements it — `expose(setup, 'setup')` at line 1294 — and this
runner loads that harness rather than reimplementing it, so the shape works.
`HarnessShape::is_supported` now says so, and the general rule is encoded: **a
shape is unsupported when *this runner* cannot express it, never when it looks
old.** Deciding by age would have blocked `setup`, and would next block
`async_test` for being older still, and the executable population would shrink
with every release of the *suite* rather than of this runner.

**3. The reference-identity metric was silently broken** (fixed last round).
Worth restating because it is the same failure mode as #1: two sets keyed in
different coordinate systems, reporting that the rules agreed on nothing, with no
error. Caught only by reading an impossible number.

**4. A fetch bug doubled the entire population** (fixed last round). Same
family again. The census now refuses to run against a self-nested checkout.

## A mechanism this runner does not contain

**A stack overflow aborts the process.** It is not a panic, so `catch_unwind`
does not catch it, and the defence in `engine.rs` against a truncated result file
simply does not apply. The first full run died exactly that way — `thread 'main'
has overflowed its stack`, no results file, nothing naming the test.

The cause is a disagreement between two limits that do not know about each other.
`RuntimeLimits::max_call_depth` is 4,096 interpreter frames, and `render-js`
budgets roughly 2 KiB of native stack per frame — so the engine assumes a stack of
several hundred megabytes. The main thread on this platform has 1 MiB. **The
engine's own recursion guard is unreachable: the process dies before the guard
can fire.**

Each test now runs on its own 64 MiB stack (`wptdom::TEST_STACK_BYTES`). That is
supplying the precondition the engine's own documented limit depends on, not
hiding a bug — the engine still enforces 4,096 frames, this just makes the
enforcement reachable. It does **not** conceal a genuine runaway: such a test
still overflows and still produces no result, which remains an engine finding.

**Reported as a needed engine contract**, because the right fix is in the engine:

```rust
// In render-js: derive the recursion limit from the stack actually available,
// rather than assuming one. A fixed 4,096 is only safe on a thread with the
// several hundred megabytes the interpreter's own per-frame cost implies.
pub fn stack_budget_bytes() -> Option<usize>;
```

**One test still takes ~9 minutes** (`html/semantics/tabular-data/processing-model-1/span-limits.html`,
which builds 131,064 `<tr>` elements via `innerHTML`). There is no per-test
wall-clock bound, and this is the runner's next gap.

## What still blocks CSS

Unchanged and confirmed. `render-core` implements no nested browsing contexts —
its own registry says so at `crates/render-core/src/spec/registry.rs:687`:

> Nested browsing contexts are not, so an `iframe` renders as nothing and
> `window.open` and `target=_blank` do nothing.

WPT's CSS tests are `testcss.js` tests, `testcss.js` is an iframe driver, and 1,314
of them build an iframe. The CSS adapter therefore produces **no verdicts at all**
— every CSS test is an explained skip, never a pass.

The adapter that was an unproven sketch last round has now been **built and run**,
and it was **discarded as a conformance adapter**. It reported `Pass` for any
document the style pipeline produced computed values for, counting properties
*read* as "assertions evaluated". That is a vacuous pass across an entire area,
and it is now a skip whose reason says exactly what was and was not checked. A
`render_core::tests::a_test_the_engine_runs_still_produces_no_verdict` test fails
if that ever changes back.

## Two engine contracts that would unblock more than any bug fix

**1. `document.createElementNS` is not implemented.** It is one method, and it is
the reason `dom/` and `html/` were unrunnable a week ago: `testharness.js`'s
`Output` object calls it to build its on-page results table, so *every* test threw
`Cannot read properties of undefined (reading 'id')` before recording anything.

It is now bypassed with `setup({output: false})`, which is a **documented
`testharness.js` property that exists for exactly this** — a runner collecting
results out-of-band. It is not the same as reimplementing the harness:
`testcss.js`'s iframe is the *subject* of a CSS test, whereas the harness's output
object is a *display* concern that renders verdicts already decided. Turning it
off cannot change whether `assert_equals(1, 2)` holds. It is a one-way door, so a
test cannot re-enable it.

It is still worth implementing, because a real browser has it.

**2. Distinguish "does not apply" from "not implemented" on a computed value.**

`render_css::computed::ComputedStyle::get` (`crates/render-css/src/computed.rs:301`)
takes a property name and returns `Option<&ComputedValue>`. `None` is ambiguous
between *the property does not apply to this element* and *the engine does not
implement the property*, and those need different verdicts — one is a pass, the
other a skip. Needed:

```rust
pub enum ComputedValueLookup {
    /// The property applies and the engine computed it.
    Value(ComputedValue),
    /// The property applies to this element but is not initialised.
    NotInitialised,
    /// The engine does not implement this property at all.
    Unsupported,
    /// The property does not apply to this element.
    DoesNotApply,
}

impl ComputedStyle {
    pub fn lookup(&self, property: &str) -> ComputedValueLookup;
}
```

Without it, an adapter that reads a fixed property list gets a vacuous pass for
every unimplemented property.

Also still needed, unchanged from last round: **the source location of the
declaration that won the cascade** (`Declaration` at
`crates/render-css/src/stylesheet.rs:35` discards the `line`/`column` that
`StyleSheetDiagnostic` already computes at line 1270). It is what turns "a WPT
test failed" into "this function is wrong", and `engine_location` is empty in
every record until it exists. **Nothing in this run is reported as
`engine_location`, and it is not guessed.**

## The 772 and the 535

**The 772** — closed. `setup()` is implemented by the pinned harness; the blocker
was wrong and is fixed. See *Two defects found in the instrument*, above.

**The 535** — accounted for, and confirmed to still be 535. It splits
**228 in `css/`** and **307 in `dom/` + `html/`**. Every one is a **fetch limit**,
not a suite defect, and `wpt-runner fixtures` names the trees:

```text
tests blocked   WPT tree to add
------------    ----------------------------------------------------
         130    shadow-dom        2  cors            1  fullscreen
          47    images            2  media-source    1  pointerevents
          33    service-workers   1  fetch           1  visual-viewport
          29    media             1  xhr
           8    scroll-to-text-fragment
           6    uievents
           5    close-watcher
           5    permissions-policy
```

307 tests, 61 distinct fixtures. The list is ordered by count, and the whole
account takes one line to act on: add those trees to `$WptTrees` in
`tools/wpt/fetch-wpt.ps1`.

**A further 35 references are attributed to no fetch scope.** Those are relative
URLs and URLs inside a tree the runner already holds, so they are *not* a reason
to fetch anything. They are counted in the total and reported as needing a
human, because silently dropping them is what makes a census untrustworthy.

**Two counts, and they differ on purpose.** The census reports **535** (228 +
307) because that is how many tests are blocked *statically*. The mechanism table
reports **655** `missing-fixture` skips because the execution path finds more:
a test can be statically clean and still reference a fixture at run time, when
its own scripts build a URL. Both are correct measures of different things, and
the census figure is the one to quote.

**One measurement bug, found and fixed while accounting for this.** The account
was first built from the census's `notable` list, which is a *truncated display
list* capped for readability. That reported **307** where the truth was **655** —
wrong by more than half, and wrong in the direction that looks like a smaller
problem. The census now keeps a complete `missing_fixtures` list separately from
the display list, because a truncated list is for reading and a count is for
arithmetic.

## Tests that could not fail

Counted and reported, never scored. **1,684 of the 5,962 scored tests (28.2%)
declare no assertion site at all.** Most are reftests whose comparison is a
`link rel=match` the scanner could not attribute, plus crash guards that assert
only that the page did not crash. A crash guard is a real WPT idiom, not a defect,
but scoring it as a pass inflates the pass count, which is the exact failure this
project has shipped twice.

The run-time counterpart — a test reported as `pass` having evaluated **zero**
assertions — is **0** in this run. That number is the reason to believe the
report, and it is the number that would have caught defect #1 above.

## What is a harness limitation, stated as such

- **1,046 `assertion-unclassified` failures.** Real failures this runner could
  not attribute to a named cause. Deliberately *not* counted as engine defects:
  attributing an unexplained failure to the engine is a guess, and a defect list
  with a guess in it stops being a defect list.
- **655 missing-fixture skips at run time** (535 measured statically — see
  *The 772 and the 535*). A fetch limit.
- **158 missing support helpers.** A global the test's own `support/` file should
  define; a fixture gap, not an API gap.
- **No per-test wall-clock bound.** One test takes ~9 minutes.
- **Reftests are counted but not scored.** They declare no assertion, so scoring
  them would be scoring a comparison this runner does not perform. The existing
  `crates/render-core/tests/wpt_reftests.rs` covers the static reftest route and
  is not duplicated here.

## How to reproduce

```powershell
# The measurement, in one command. Self-check first, fetch if needed, then run.
powershell -File tools/wpt/run.ps1 -Areas dom,html

# Can the engine load WPT's own harness? Measured, not argued.
powershell -File tools/wpt/run.ps1 -Areas dom -Probe

# The negative control. Both halves must hold before any rate is trustworthy.
powershell -File tools/wpt/run.ps1 -NegativeControl -Areas dom

# Which trees does the fetch need?
powershell -File tools/wpt/run.ps1 -Fixtures

# Population and feasibility only; needs no engine, so it works even while
# render-js is mid-edit by another agent.
powershell -File tools/wpt/run.ps1 -Census
```

Results land in `tools/wpt/results/wpt-results.json` (summary, meant to be diffed)
and `wpt-results.jsonl` (one line per test, meant to be grepped). Both record the
pinned revision, because a conformance figure without a suite revision is not
comparable to anything.

**Every percentage carries its denominator, and there is no code path in this
crate that prints one bare.** `results::tests::rate_string_always_carries_its_denominator`
keeps it that way.

## Operational notes for this host

- **The suite is fetchable, and the fetch works.** `tools/wpt/fetch-wpt.ps1` uses
  the `codeload.github.com` tarball over plain HTTPS. It does **not** use git for
  transport, because the global git config on this machine forces
  `http.https://github.com.proxy=socks5://localhost:10808` and that proxy is **not
  running** — every git operation against GitHub fails with `Failed to connect to
  localhost port 10808`. `tools/fetch-wpt.ps1` (the other one) **cannot work on
  this host**.
- **`pwsh` is not installed**; only Windows PowerShell 5.1. The fetch script
  targets 5.1 and says so.
- **A full `dom/`+`html/` run takes ~20 minutes**, most of it in one test. A
  `-Limit N` run is labelled `PARTIAL` and is never presented as a conformance
  rate.
- **`.gitignore`** — these three lines were added last round:
  `tools/wpt/.cache/`, `tools/wpt/results/`, `tests/wpt_runner/Cargo.lock`.
  `tests/wpt_runner/target/` is already covered by the existing `target/` rule.

## What was not done, and why

- **No CSS conformance rate.** Blocked on nested browsing contexts. 1,314 CSS
  tests are `testcss.js` tests and every one builds an iframe.
- **No engine `file:line` in any failure record.** Blocked on the cascade-origin
  contract. `engine_location` is empty everywhere and is not guessed.
- **No pixel comparison.** The reftest path needs a rasteriser and a reference
  decode; `crates/render-core/tests/wpt_reftests.rs` covers the static reftest
  route and is not duplicated.
- **1,046 failures are unattributed.** Real, and unclassified. They are the first
  thing to read after the two defect mechanisms.
- **No committed baseline of the score.** A baseline makes every future run
  compare against the current state and the suite stops being an authority. The
  regression guards here are the *mechanisms* — `classify::tests` pins the
  `setup` fix, `wptdom::tests` pins the failure classifier, and the negative
  control proves the harness can fail — not the number.
- **Not attempted: running `test262`**, per the task constraints.