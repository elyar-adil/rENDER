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

The batch runner recursively discovers every local official HTML test that
declares `link rel="match"` and executes every discovered pair through
render-core's deterministic `Document` pipeline:

```powershell
python tools/run-wpt-reftests.py
```

The default scans the complete WPT checkout. Use `--suite css` or
`--suite html` for a focused subsystem run, and use `--path-prefix` for a
specific checkout-relative directory. `--max-cases` is only for an explicit
smoke run; it must not be used for a conformance baseline.

The Rust runner emits one `WPT_RESULT` record per pair and a final summary:

```text
WPT_SUMMARY  cases=...  pass=...  fail=...  unsupported=...  skip=...  infrastructure=...
```

Pixel mismatches and infrastructure errors make the command fail after all
cases have been processed. `unsupported` and `skip` are reported separately
and are never counted as passes.

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
