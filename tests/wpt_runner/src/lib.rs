//! Official Web Platform Tests conformance runner for the rENDER engine.
//!
//! # What this crate is for
//!
//! The project has an objective measure of JavaScript conformance (test262) and
//! no objective measure of CSS, HTML or DOM conformance. Every other number in
//! the register is a claim: a test count, a corpus measurement, a status field
//! someone chose. This crate produces a measured figure for the other two thirds
//! of a browser.
//!
//! # The posture
//!
//! A conformance number produced by a subtly wrong harness is worse than no
//! number, because it is quotable, believable, and actionable. Three rules
//! follow, and they are visible in the module layout rather than in a comment:
//!
//! * [`outcome`] has four states, not three. A harness failure is never a test
//!   failure and never a test pass. `error` is reported in its own bucket and is
//!   excluded from both.
//! * [`selftest`] runs on every invocation and proves the four states are
//!   actually distinguishable, including the panic path. Its result is embedded
//!   in the results file. A run whose self-check failed does not report a rate.
//! * [`census`] produces the denominator before any numerator exists, and
//!   classifies every exclusion with a named reason. A test that cannot fail is
//!   counted, not scored.
//!
//! # What a reader is entitled to
//!
//! Every percentage in every artefact this crate writes is rendered with its
//! denominator attached, and there is no code path that emits a bare percentage
//! ([`outcome::Rate`] has no bare-`Display`). Every result file records the
//! pinned WPT revision, because a conformance figure without a suite revision is
//! not comparable to anything.
//!
//! # Module map
//!
//! | Module | Role |
//! | --- | --- |
//! | [`outcome`] | the four-state model and honest rate arithmetic |
//! | [`source`] | tolerant HTML/script scanner; reports its own uncertainty |
//! | [`classify`] | harness shape, required capabilities, feasibility |
//! | [`census`] | the pinned suite's real population, and the denominator |
//! | [`engine`] | the four-state boundary around an engine; panic containment |
//! | [`mechanism`] | the ranked table: one root cause, one count, one fix |
//! | [`probe`] | can the engine load WPT's own harness? measured, not argued |
//! | [`selftest`] | the falsification checks that gate reporting a rate |
//! | [`results`] | JSON summary plus per-file JSONL, as data |
//! | [`report`] | the human-readable reading, with denominators inline |
//! | [`wptdom`] | executes `dom/` and `html/` through WPT's own testharness |

pub mod census;
pub mod classify;
pub mod engine;
pub mod fixtures;
pub mod mechanism;
pub mod outcome;
pub mod probe;
pub mod report;
pub mod results;
pub mod selftest;
pub mod source;

#[cfg(feature = "engine")]
pub mod wptdom;

#[cfg(feature = "engine")]
pub mod render_core;

use std::path::PathBuf;

/// The pinned WPT revision every recorded number is stated against.
///
/// Kept in sync with `tools/wpt/fetch-wpt.ps1` and with the pin in
/// `docs/wpt.md`. Duplicated deliberately: a runner that read its pin from a
/// script could not state, in its own results file, which suite it measured.
pub const WPT_REVISION: &str = "c7fdee80f3f17b4e9813964916afdfd57ace863f";

/// The areas this runner executes, in the order the brief prioritises them.
///
/// `canvas/`, `webgpu/`, `webaudio/`, `webrtc/`, `wasm/`, `network/`,
/// `service-workers/` and `storage/` are excluded by decision, not by
/// difficulty: they need a browser this engine is not, and counting them as
/// failures would manufacture a number out of missing capability. Within
/// `html/`, the `canvas/` and `browsers/` subtrees are excluded for the same
/// reason and are reported as their own exclusion buckets so the decision is
/// visible in the results rather than implied by a denominator.
pub const DEFAULT_AREAS: &[&str] = &["css", "dom", "html"];

/// Where a run's output goes.
#[derive(Clone, Debug)]
pub struct RunPaths {
    pub suite_root: PathBuf,
    pub output_dir: PathBuf,
}

/// Default locations, relative to the repository root.
#[must_use]
pub fn default_suite_root(repo_root: &std::path::Path) -> PathBuf {
    repo_root.join("tools/wpt/.cache/wpt")
}

/// Default output directory, gitignored alongside the cache.
#[must_use]
pub fn default_output_dir(repo_root: &std::path::Path) -> PathBuf {
    repo_root.join("tools/wpt/results")
}

/// Find the repository root from this crate's manifest directory.
///
/// The crate is at `<repo>/tests/wpt_runner`, so the root is two levels up. The
/// `CARGO_MANIFEST_DIR` fallback covers being invoked from a copied tree.
#[must_use]
pub fn repo_root() -> PathBuf {
    if let Ok(dir) = std::env::var("CARGO_MANIFEST_DIR") {
        let path = PathBuf::from(dir);
        if let Some(root) = path.parent().and_then(std::path::Path::parent) {
            return root.to_path_buf();
        }
    }
    PathBuf::from(".")
}
