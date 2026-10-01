# WPT conformance runner

Measures CSS, DOM and HTML conformance against the official
[Web Platform Tests](https://github.com/web-platform-tests/wpt) at a pinned
revision, and produces a number that means what it says.

## Run it

```powershell
powershell -File tools/wpt/run.ps1
```

That is the whole thing. It self-checks the harness, fetches the pinned suite if
the cache is missing (**network required on first run**), runs, and writes
results. Later runs are offline.

| Command | What it does |
| --- | --- |
| `tools/wpt/run.ps1` | everything: self-check, fetch if needed, census, execute, report |
| `tools/wpt/run.ps1 -Census` | population and feasibility only. No engine needed. |
| `tools/wpt/run.ps1 -Areas css` | narrow the scope |
| `tools/wpt/run.ps1 -Limit 200` | partial run, labelled `PARTIAL`, never called a conformance rate |
| `tools/wpt/fetch-wpt.ps1` | fetch and pin the suite on its own |
| `cargo run --manifest-path tests/wpt_runner/Cargo.toml -- selftest` | the harness checks alone, ~1 second |

`wpt-runner run` writes `tools/wpt/results/wpt-results.json` (summary) and
`wpt-results.jsonl` (one line per test file).

## Why it is built this way

A conformance number produced by a subtly wrong harness is worse than no number,
because it gets quoted, believed, and used. Four decisions follow from that, and
each is enforced by a test rather than by a comment:

**Four states, not three.** `pass`, `fail`, `error`, `skipped`. A harness that
reports its own crash as a test failure manufactures failures; one that reports
it as a pass manufactures passes. The second survives review. `error` gets its own
bucket and is excluded from both.

**The self-check gates reporting.** Nine checks run before anything is scored, and
the run refuses to produce a result if one fails — including a check that a
panicking adapter becomes `error` rather than killing the process and leaving a
truncated file that looks like a smaller suite.

**No percentage can print without its denominator.** `Rate` has no bare-`Display`.
An empty run prints no percentage rather than `0%`, because "0% conformance" from
a runner that could not start is a false claim about the engine.

**The denominator is derived, and stated.** WPT commits no test manifest, so the
population is derived — and every exclusion is a named bucket with arithmetic that
closes in the output. Reference identity is checked structurally *and* by naming
convention, and the two are reported against each other, because a denominator
that is quietly wrong is invisible otherwise.

See `tools/wpt/FINDINGS.md` for what the current numbers mean and which failures
are engine defects versus missing capabilities versus harness limits.

## Layout

| Path | Role |
| --- | --- |
| `src/outcome.rs` | the four-state model and the rate arithmetic |
| `src/source.rs` | tolerant HTML/script scanner; reports its own uncertainty |
| `src/classify.rs` | harness shape, required capabilities, feasibility |
| `src/census.rs` | the population, and therefore the denominator |
| `src/engine.rs` | the four-state boundary around an engine; panic containment |
| `src/selftest.rs` | the falsification checks that gate reporting |
| `src/results.rs` | JSON summary plus per-file JSONL |
| `src/report.rs` | the human-readable reading |
| `src/render_core.rs` | the `render-core` adapter, behind the `engine` feature |

## Dependencies

None by default. `render-core` and `url` are optional and off unless
`--features engine` is passed. Everything else is `std`.

The engine dependency is optional for a non-stylistic reason: the engine was
under active concurrent edit and did not compile during the run that produced the
current findings. With the feature off this crate builds and runs against `std`
alone, so the census and the self-check keep working while the engine is broken.
The parts that must never depend on the engine being healthy do not.

The fetch is a PowerShell script rather than an HTTP dependency, which keeps the
network stack out of the measurement path entirely: the code that produces the
numbers does not also implement TLS.

## Build

Scoped to this manifest. Do not run workspace-wide cargo commands — other agents
are building in this tree.

```powershell
cargo test  --manifest-path tests/wpt_runner/Cargo.toml
cargo build --manifest-path tests/wpt_runner/Cargo.toml --features engine
```

`target/` lands in `tests/wpt_runner/target/`, not the repository `target/`.
That duplicate compile is deliberate: sharing the directory would queue this
build behind every other agent's.
