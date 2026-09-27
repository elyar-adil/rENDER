# Official WPT reftests

rENDER uses the official `web-platform-tests/wpt` repository as an external
checkout. It is not vendored into this repository and is pinned to:

```text
c7fdee80f3f17b4e9813964916afdfd57ace863f
```

Fetch a clean, complete checkout next to the repository:

```powershell
pwsh -File tools/fetch-wpt.ps1
```

Use `-Target C:\path\to\wpt` to choose another external directory. The
script rejects a dirty checkout, verifies the official remote and revision,
and never replaces an existing non-empty directory.

## Full static reftest run

The runner is the Rust integration test `crates/render-core/tests/wpt_reftests.rs`.
It recursively discovers local official HTML tests that declare `link rel="match"`
and executes every discovered pair through render-core's deterministic `Document`
pipeline. There is no separate Python batch script.

```powershell
cargo test -p render-core --test wpt_reftests -- --ignored --nocapture
```

Select a subset through the environment rather than through command-line flags:

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

**This suite has never actually been executed.** The test, the fetch script and the
runner all exist, and CI has a job for it (`.github/workflows/rust.yml`, `wpt-reftests`),
but no run has ever completed against a real checkout, so no pass count in this
repository is a WPT result. Treat any claim about WPT conformance as unverified until
someone runs it and records the output. The job is `continue-on-error: true` precisely
because an unobtainable checkout must not be reported as a pass.

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
