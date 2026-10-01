# Official WPT reftests

rENDER uses the official `web-platform-tests/wpt` repository as an external
checkout. It is not vendored into this repository and is pinned to:

```text
c7fdee80f3f17b4e9813964916afdfd57ace863f
```

Fetch a pinned checkout into the gitignored cache:

```powershell
powershell -File tools/wpt/fetch-wpt.ps1
```

This downloads the GitHub codeload tarball for revision
`c7fdee80f3f17b4e9813964916afdfd57ace863f` and extracts only `css/`, `dom/`,
`html/`, `resources/`, `common/` and `fonts/`. Roughly 400 MB on first run;
later runs are offline.

Two host-specific notes, both verified here:

- **Do not use `tools/fetch-wpt.ps1`.** It drives `git` against github.com, and this
  machine's global git config forces
  `http.https://github.com.proxy=socks5://localhost:10808`, a proxy that is not
  running. Every git operation against GitHub fails. `tools/wpt/fetch-wpt.ps1`
  uses plain HTTPS and works.
- **Use `powershell`, not `pwsh`.** Only Windows PowerShell 5.1 is installed.

Then run it and read the figure out of the results file:

```powershell
powershell -File tools/wpt/run.ps1 -Areas dom,html
```

Measurements, mechanisms and reproduction steps live in `tools/wpt/FINDINGS.md`; **do not duplicate
its numbers here.** A number in this file that disagrees with that one is a bug in whichever is
wrong, and the results file is the source.

## Two runners answer two different questions

**The reftest runner is the Rust integration test `crates/render-core/tests/wpt_reftests.rs`.** It
discovers local official HTML tests declaring `link rel="match"` and runs each pair through
render-core's deterministic `Document` pipeline, comparing rendered output against the reference.
That is a **pixel** question.

```powershell
cargo test -p render-core --test wpt_reftests -- --ignored --nocapture
```

Select a subset through the environment:

| Variable | Effect |
| --- | --- |
| `RENDER_WPT_ROOT` | the WPT checkout directory |
| `RENDER_WPT_TEST` | run one test, with `RENDER_WPT_REFERENCE` naming its reference |
| `RENDER_WPT_MANIFEST` | restrict discovery to a manifest of pairs |

**The conformance runner is `tools/wpt/`**, and it answers a different question: not "does the
rendered output match" but "does the engine give the right answer to the test's assertions". It
covers `dom/` and `html/`, and it **cannot cover `css/`**, because `testcss.js` is an iframe driver
and the engine has no nested browsing contexts.

So both statements are true and they are not in tension: **the reftest runner exists and answers a
pixel question, and it is no longer the only runner.** An earlier version of this file said the
suite "has never actually been executed" - that is no longer true. The suite is fetchable and has
been run; `tools/wpt/FINDINGS.md` holds the output.

## What a WPT number from this repository does and does not mean

The runner reports **four** result categories - engine defect, acceptable difference, unimplemented
feature, harness limitation - and **never sums them**, because a missing API and a wrong answer mean
opposite things. Every percentage is printed with its denominator beside it, and the denominator
that usually matters is the **executable** population rather than the scored one.

**A pass with zero assertions evaluated is scored as a skip, not a pass.** That is not a detail: an
earlier run of this very runner reported a clean 7.4% in which *every* pass was vacuous, because the
result sink read a field WPT does not define and so every assertion count was zero. It was caught
only by noticing that two counters held the same number. The count now comes from wrapping the
assertions. The runner also has a negative control, and **the control exercises tree mutations
only - it says nothing about the error-count comparison.**

## Recorded figures

Produced by `tools/wpt/` at revision `c7fdee80f3f17b4e9813964916afdfd57ace863f`. **Every percentage
is stated with the denominator it belongs to**, because the three differ by more than an order of
magnitude and averaging them would be a lie.

| area | scored | executable | pass | rate |
| --- | --- | --- | --- | --- |
| `dom/` | 559 | 297 | 32 | **10.8%** (32/297) |
| `html/` | 5,403 | 2,520 | 165 | **6.5%** (165/2,520) |
| **combined** | **5,962** | **2,817** | **197** | **7.0%** (197/2,817) |
| `css/` | 26,614 | - | - | **cannot be run** - `testcss.js` is an iframe driver |

Also in that run: 31 errored, 8,060 skipped, **0 passes with zero assertions evaluated**, and 1,684
tests with no assertion site at all - reported, never scored.

The categories do not sum, and the split is the useful part: **664 engine defect, 3,837
unimplemented feature, 4,691 harness limitation, 0 acceptable difference.** The largest single
mechanisms are 1,564 lacking canvas, 630 lacking a nested browsing context, and 638 host objects
not initialising Web IDL defaults - **and that last one is one change per interface rather than 638
changes**, because every default is declared in the interface's IDL.

**1,046 failures are unclassified** - real, unnamed, and the first thing to read after the two
defect mechanisms. There is no `engine_location` recorded for any of them, because the cascade does
not retain a declaration's origin.

| Variable | Effect |
| --- | --- |
| `RENDER_WPT_ROOT` | the WPT checkout directory |
| `RENDER_WPT_TEST` | run one test, with `RENDER_WPT_REFERENCE` naming its reference |
| `RENDER_WPT_MANIFEST` | restrict discovery to a manifest of pairs |

The runner emits one `WPT_RESULT` record per pair and a final summary:

```text
WPT_SUMMARY  cases=...  pass=...  fail=...  unsupported=...  skip=...  infrastructure=...
```

Pixel mismatches and infrastructure errors make the command fail after all cases
have been processed. `unsupported` and `skip` are reported separately and are never
counted as passes.

**Corrected.** This section previously read "**This suite has never actually been executed.**" That
was true when written and **is no longer true**: `tools/wpt/` fetches a pinned checkout, runs the
`dom/` and `html/` areas, and records its output, and the figures are in the *Recorded figures*
section above. The reftest runner described here still exists, still answers the pixel question,
and is still the runner `.github/workflows/rust.yml`'s `wpt-reftests` job invokes.

What remains true and worth keeping: **an unobtainable checkout must never be reported as a
pass**, which is why that job is `continue-on-error: true`, and why the conformance runner has no
committed baseline and reports four categories rather than one number.

## Scope

This adapter covers official static reftests. It does not claim full WPT
conformance: testharness JavaScript, navigation, network resources, fonts,
interactive/manual tests, and other browser harness features are classified
as unsupported or skipped by the render-core path. Those suites require a
browser-level WPT product adapter rather than a reduced fixture set.

For debugging one pair without discovery:

```powershell
$env:RENDER_WPT_ROOT = "C:\Users\Elyar\Desktop\wpt"
$env:RENDER_WPT_TEST = "css\path\to\test.html"
$env:RENDER_WPT_REFERENCE = "css\path\to\reference.html"
cargo test -p render-core --test wpt_reftests official_wpt_reftests -- --exact --ignored --nocapture
```
