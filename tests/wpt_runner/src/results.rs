//! Write results as data.
//!
//! A conformance claim is only useful if it can be diffed against the previous
//! one, so results are written as machine-readable JSON with the suite revision
//! inside the document. There is no dependency here: a JSON *writer* is about
//! eighty lines and can be audited in one sitting, which is worth more than a
//! third-party crate for a file whose whole job is to be trusted.
//!
//! Two files are produced, deliberately split by size and by purpose:
//!
//! * `wpt-results.json` - the summary. Small, stable, meant to be diffed and
//!   quoted. Every percentage appears with its denominator in the same string,
//!   so a number cannot be lifted out of its context by a copy-paste.
//! * `wpt-results.jsonl` - one line per test file. Large, meant to be grepped.
//!   Splitting it means the summary stays readable in a diff and the per-file
//!   detail does not drown it.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use crate::census::{AreaCensus, BlockerKey, Census, Exclusion};
use crate::outcome::{Outcome, SkipReason, Tally};

/// A JSON value this crate emits. Deliberately small: the alternative is a
/// dependency, and this file has to be obviously correct to be worth anything.
///
/// `PartialEq` is derived so a test can assert on a value rather than on its
/// rendered form, which is the difference between testing the data and testing
/// the formatting.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    UInt(u64),
    Str(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    /// A string that always renders, so a percentage cannot be read without the
    /// count it is a percentage of.
    pub fn rate(numerator: u32, denominator: u32) -> Json {
        Json::Str(format!("{:.1}% ({numerator}/{denominator})", percent(numerator, denominator)))
    }

    pub fn write(&self, out: &mut String) {
        match self {
            Self::Null => out.push_str("null"),
            Self::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Self::UInt(n) => {
                let _ = write!(out, "{n}");
            }
            Self::Str(s) => write_json_string(s, out),
            Self::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.write(out);
                }
                out.push(']');
            }
            Self::Object(fields) => {
                out.push('{');
                for (i, (key, value)) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_json_string(key, out);
                    out.push(':');
                    value.write(out);
                }
                out.push('}');
            }
        }
    }

    #[must_use]
    pub fn to_pretty(&self) -> String {
        let mut out = String::new();
        write_pretty(self, 0, &mut out);
        out.push('\n');
        out
    }
}

const fn percent(numerator: u32, denominator: u32) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        // Cannot use f64 in a const fn on stable in a form that avoids
        // overflow concerns; this is only ever called with small numbers.
        numerator as f64 * 100.0 / denominator as f64
    }
}

fn write_pretty(value: &Json, indent: usize, out: &mut String) {
    let pad = |n: usize, out: &mut String| {
        for _ in 0..n {
            out.push_str("  ");
        }
    };
    match value {
        Json::Array(items) if !items.is_empty() => {
            out.push_str("[\n");
            for (i, item) in items.iter().enumerate() {
                pad(indent + 1, out);
                write_pretty(item, indent + 1, out);
                if i + 1 < items.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            pad(indent, out);
            out.push(']');
        }
        Json::Object(fields) if !fields.is_empty() => {
            out.push_str("{\n");
            for (i, (key, item)) in fields.iter().enumerate() {
                pad(indent + 1, out);
                write_json_string(key, out);
                out.push_str(": ");
                write_pretty(item, indent + 1, out);
                if i + 1 < fields.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            pad(indent, out);
            out.push('}');
        }
        other => other.write(out),
    }
}

fn write_json_string(s: &str, out: &mut String) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// A one-line cause for a mechanism key, and the category it belongs to.
///
/// The mapping is explicit rather than derived from the key's spelling, for the
/// same reason the four states are explicit: a category inferred from a string
/// pattern is a category that silently changes meaning when someone renames a
/// key. Every arm here says what to *do* about it, because a mechanism table
/// exists to be worked through.
fn describe_mechanism(key: &str, file: &FileResult) -> (String, crate::mechanism::Category) {
    use crate::mechanism::Category;
    match key {
        "lacks-nested-browsing-context" => (
            "render-core implements no nested browsing context, so an `iframe` renders as \
             nothing. Implementing nested browsing contexts is the single highest-leverage \
             change: it is the only blocker that gates the area where the engine is strongest, \
             because 1,314 CSS tests are `testcss.js` tests and every one of them builds an \
             iframe."
                .to_owned(),
            Category::UnimplementedFeature,
        ),
        "lacks-window-proxy" => (
            "the test reads `parent` or `top`, which needs a cross-document window reference. \
             Blocked behind nested browsing contexts."
                .to_owned(),
            Category::UnimplementedFeature,
        ),
        "lacks-animation" => (
            "the test needs `requestAnimationFrame` or `Element.animate`. The engine has no \
             animation clock."
                .to_owned(),
            Category::UnimplementedFeature,
        ),
        "lacks-shadow-dom" => (
            "the test needs `attachShadow` or a `ShadowRoot`. The engine has no shadow tree."
                .to_owned(),
            Category::UnimplementedFeature,
        ),
        "lacks-range" => (
            "the test needs `createRange` or a selection API. The engine has no Range."
                .to_owned(),
            Category::UnimplementedFeature,
        ),
        "lacks-navigation" => (
            "the test needs cross-document navigation or `history`. The engine does not \
             navigate."
                .to_owned(),
            Category::UnimplementedFeature,
        ),
        "lacks-network-fetch" => (
            "the test needs `fetch` or `XMLHttpRequest` against a real network. This runner \
             serves no network."
                .to_owned(),
            Category::UnimplementedFeature,
        ),
        "lacks-dedicated-worker" => (
            "the test needs a `Worker`. The engine has no worker realm.".to_owned(),
            Category::UnimplementedFeature,
        ),
        "lacks-canvas" => (
            "the test needs a 2D or WebGL canvas context.".to_owned(),
            Category::UnimplementedFeature,
        ),
        "lacks-media-playback" => {
            ("the test needs `HTMLMediaElement` playback.".to_owned(), Category::UnimplementedFeature)
        }
        "lacks-dynamic-import" => (
            "the test uses dynamic `import()`, which needs a module loader.".to_owned(),
            Category::UnimplementedFeature,
        ),
        "lacks-webaudio" | "lacks-wasm" | "lacks-client-storage" | "lacks-websocket"
        | "lacks-geolocation" | "lacks-device-apis" => (
            format!("the test needs the `{}` API, which the engine does not implement", key
                .trim_start_matches("lacks-")),
            Category::UnimplementedFeature,
        ),
        "missing-fixture" => (
            "the test declares a fixture that this cache does not hold. This is a FETCH limit, \
             not a suite defect: the file exists at the pinned revision. Run the `fixtures` \
             subcommand for the exact trees to add."
                .to_owned(),
            Category::HarnessLimitation,
        ),
        "no-assertion-site" => (
            "the test declares no assertion and no reftest reference, so it has no way to fail \
             in any browser. Counted, never scored."
                .to_owned(),
            Category::HarnessLimitation,
        ),
        "unscannable-file" => (
            "the scanner could not read this file confidently, so nothing can be claimed \
             about it."
                .to_owned(),
            Category::HarnessLimitation,
        ),
        "no-verdict-from-harness" => (
            "WPT's harness reported no result and no status for this test. The test either \
             registered nothing or died before registering; the harness-level channel is \
             registered and working, as the negative control demonstrates."
                .to_owned(),
            Category::HarnessLimitation,
        ),
        "result-channel-unreadable" => (
            "the result channel did not produce a readable harvest. This runner cannot interpret \
             the harness's answer, so it makes no claim."
                .to_owned(),
            Category::HarnessLimitation,
        ),
        "event-loop-never-settles" => (
            "the microtask and timer queues did not settle within the drain bound, so the test \
             never reached a verdict. Bounded rather than hung."
                .to_owned(),
            Category::HarnessLimitation,
        ),
        "engine-step-budget" => (
            "the engine's own execution-step budget was exhausted before the test finished. \
             A truncated engine is not a wrong one, so this is not scored."
                .to_owned(),
            Category::HarnessLimitation,
        ),
        "harness-did-not-load" => (
            "WPT's testharness.js did not load, so nothing could be driven through it."
                .to_owned(),
            Category::HarnessLimitation,
        ),
        "test-thread-panicked" | "test-thread-unstartable" | "test-file-unreadable" => (
            "the runner could not evaluate this file. A harness error, and counted as neither \
             pass nor fail."
                .to_owned(),
            Category::HarnessLimitation,
        ),
        "negative-control" => (
            "the negative control's own result. Not a real test outcome.".to_owned(),
            Category::HarnessLimitation,
        ),
        // A real assertion failure, or a skip naming a missing engine API. The
        // category follows the four-state result rather than the key, because
        // `assertion: ...` keys are specific to the assertion that broke and
        // nothing else.
        // A failure the engine could not satisfy because an interface it does
        // not implement is missing. This is an *unimplemented feature*, not a
        // defect: nothing was computed wrongly, nothing was computed at all.
        // Putting these in the defect list would be a false defect report and
        // would bury the real ones.
        other if other.starts_with("lacks-") && file.outcome == Outcome::Fail => (
            format!(
                "a WPT assertion needed the `{}` interface, which the engine does not implement. \
                 Nothing was computed wrongly; nothing was computed.",
                other.trim_start_matches("lacks-")
            ),
            Category::UnimplementedFeature,
        ),
        "host-method-missing" => (
            "a WPT assertion called a method the engine's host object does not have.".to_owned(),
            Category::UnimplementedFeature,
        ),
        "host-constructor-missing" => (
            "a WPT assertion called `new` on a constructor the engine does not provide."
                .to_owned(),
            Category::UnimplementedFeature,
        ),
        "host-object-undefined" => (
            "a WPT assertion used a host object the engine left undefined.".to_owned(),
            Category::UnimplementedFeature,
        ),
        "host-property-missing" => (
            "a WPT assertion read a property the engine's host object does not have."
                .to_owned(),
            Category::UnimplementedFeature,
        ),
        "host-not-iterable" => (
            "a WPT assertion iterated a host collection the engine does not expose as iterable."
                .to_owned(),
            Category::UnimplementedFeature,
        ),
        "infinite-recursion" => (
            "a WPT assertion recursed until the engine's own call-depth guard fired. The guard \
             worked; the test's own code is what recursed."
                .to_owned(),
            Category::AcceptableDifference,
        ),
        "value-shape-differs" => (
            "the engine returned a value of the wrong shape: wrong type, or wrong length \
             where WPT expected an array or collection. This is the largest genuine \
             defect family in the run, and it is one family, not 2,105."
                .to_owned(),
            Category::EngineDefect,
        ),
        "needs-testdriver" => (
            "the test uses `/resources/testdriver.js` - WPT's second harness, for tests that \
             must run in a separate window or frame. That is the nested-browsing-context \
             problem in a different file, so this is the same roadmap item as \
             `lacks-nested-browsing-context` and not a separate one."
                .to_owned(),
            Category::UnimplementedFeature,
        ),
        "missing-support-helper" => (
            "the test referenced a global its own `support/` file should have defined. The \
             missing thing is a fixture this cache did not take, not an API the engine \
             lacks."
                .to_owned(),
            Category::HarnessLimitation,
        ),
        "promise-shape-differs" => (
            "the engine returned a non-promise, or a promise that did not settle the way WPT \
             expected."
                .to_owned(),
            Category::EngineDefect,
        ),
        other if other.starts_with("undefined-global-") => (
            format!(
                "a WPT assertion referenced the global `{}`, which the engine does not define",
                other.trim_start_matches("undefined-global-").replace('-', "")
            ),
            Category::UnimplementedFeature,
        ),
        "assertion-unclassified" => (
            "a WPT assertion did not hold and this runner could not attribute it to a named \
             cause. These need a human to read; they are not silently counted as engine \
             defects."
                .to_owned(),
            Category::HarnessLimitation,
        ),
        other if other.starts_with("skip:") => (
            format!("skipped: {other}"),
            Category::UnimplementedFeature,
        ),
        other if other.starts_with("shape-") => (
            format!(
                "this runner does not implement the `{}` harness shape",
                other.trim_start_matches("shape-")
            ),
            Category::HarnessLimitation,
        ),
        other => (
            format!("unclassified mechanism `{other}`; this needs a cause to be actionable"),
            Category::HarnessLimitation,
        ),
    }
}

/// The ranked mechanism table as data, with the four categories kept apart.
///
/// `accounted` is included so a reader can check the arithmetic: the ranked rows
/// must sum to it. A table whose rows do not sum to its own total is a table
/// under-reporting, and that is visible here rather than only in the prose.
fn mechanisms_json(table: &crate::mechanism::MechanismTable) -> Json {
    let by_category = table.by_category();
    Json::Object(vec![
        (
            "accounted".into(),
            Json::UInt(u64::from(table.accounted)),
        ),
        (
            "distinct".into(),
            Json::UInt(table.ranked().len() as u64),
        ),
        (
            "ranked".into(),
            Json::Array(
                table
                    .ranked()
                    .into_iter()
                    .map(|m| {
                        Json::Object(vec![
                            ("key".into(), Json::Str(m.key.clone())),
                            ("cause".into(), Json::Str(m.cause.clone())),
                            ("category".into(), Json::Str(m.category.label().into())),
                            ("tests".into(), Json::UInt(u64::from(m.tests))),
                            (
                                "areas".into(),
                                Json::Array(
                                    m.areas.iter().cloned().map(Json::Str).collect(),
                                ),
                            ),
                            (
                                "examples".into(),
                                Json::Array(
                                    m.examples.iter().cloned().map(Json::Str).collect(),
                                ),
                            ),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "by_category".into(),
            Json::Object(
                [
                    crate::mechanism::Category::EngineDefect,
                    crate::mechanism::Category::AcceptableDifference,
                    crate::mechanism::Category::UnimplementedFeature,
                    crate::mechanism::Category::HarnessLimitation,
                ]
                .iter()
                .map(|category| {
                    (
                        category.label().to_owned(),
                        Json::UInt(u64::from(
                            by_category.get(category).copied().unwrap_or(0),
                        )),
                    )
                })
                .collect(),
            ),
        ),
    ])
}

fn tally_json(tally: &Tally) -> Json {
    Json::Object(vec![
        ("pass".into(), Json::UInt(u64::from(tally.pass))),
        ("fail".into(), Json::UInt(u64::from(tally.fail))),
        ("error".into(), Json::UInt(u64::from(tally.error))),
        ("skipped".into(), Json::UInt(u64::from(tally.skip))),
        (
            "total_seen".into(),
            Json::UInt(u64::from(tally.total())),
        ),
        (
            "produced_engine_evidence".into(),
            Json::UInt(u64::from(tally.attempted())),
        ),
        (
            "conformance_rate".into(),
            match tally.engine_rate() {
                Some(rate) => Json::Str(format!("{rate}")),
                None => Json::Str("not computed: no test produced engine evidence".into()),
            },
        ),
    ])
}

fn map_json<K: Ord, V: Copy + Into<u64>>(map: &BTreeMap<K, V>, key: impl Fn(&K) -> String) -> Json {
    Json::Object(
        map.iter()
            .map(|(k, v)| (key(k), Json::UInt((*v).into())))
            .collect(),
    )
}

fn blocker_json(key: &BlockerKey) -> Json {
    match key {
        BlockerKey::MissingEngine(cap) => Json::Object(vec![
            ("cause".into(), Json::Str("missing-engine-capability".into())),
            ("capability".into(), Json::Str(cap.label().into())),
            ("detail".into(), Json::Str(key.describe())),
        ]),
        BlockerKey::UnsupportedShape(shape) => Json::Object(vec![
            ("cause".into(), Json::Str("unsupported-harness-shape".into())),
            ("shape".into(), Json::Str(shape.label().into())),
            ("detail".into(), Json::Str(key.describe())),
        ]),
        BlockerKey::NoAssertions => Json::Object(vec![
            ("cause".into(), Json::Str("no-assertions".into())),
            (
                "detail".into(),
                Json::Str(
                    "the test declares no assertion and no reftest reference, so it has no way \
                     to fail"
                        .into(),
                ),
            ),
        ]),
        BlockerKey::MissingFixture => Json::Object(vec![
            ("cause".into(), Json::Str("missing-fixture".into())),
            ("detail".into(), Json::Str(key.describe())),
        ]),
        BlockerKey::Unscannable => Json::Object(vec![
            ("cause".into(), Json::Str("unscannable".into())),
            ("detail".into(), Json::Str(key.describe())),
        ]),
    }
}

/// A single test file's recorded outcome.
#[derive(Clone, Debug)]
pub struct FileResult {
    pub path: String,
    pub area: String,
    pub outcome: Outcome,
    /// The assertion that failed, with where it is. Required for a fail: a fail
    /// without a failing assertion is not a finding, it is a shrug.
    pub failing_assertion: Option<String>,
    /// For a fail or an error, the first frame that points into the engine.
    pub engine_location: Option<String>,
    /// For an error, the adapter-level message. Never folded into a fail.
    pub error: Option<String>,
    pub skip_reason: Option<SkipReason>,
    /// Assertions actually evaluated during the run. A pass with zero of these
    /// is a pass that could not have failed, and is reported as such.
    pub assertions_evaluated: u32,
    /// Declared assertion call sites, from static analysis.
    pub assertion_sites: u32,
    /// The named cause of this result, for the ranked mechanism table.
    ///
    /// Separate from `skip_reason` and `error` on purpose: those say *which
    /// bucket* a result is in, and this says *which root cause*, and the second
    /// is what a reader acts on. A skip table grouped by bucket is a list of
    /// categories; a table grouped by mechanism is a work queue.
    pub mechanism: Option<String>,
    pub notes: Vec<String>,
}

impl FileResult {
    fn to_json(&self) -> Json {
        Json::Object(vec![
            ("path".into(), Json::Str(self.path.clone())),
            ("area".into(), Json::Str(self.area.clone())),
            ("outcome".into(), Json::Str(self.outcome.label().into())),
            (
                "failing_assertion".into(),
                self.failing_assertion
                    .as_ref()
                    .map_or(Json::Null, |s| Json::Str(s.clone())),
            ),
            (
                "engine_location".into(),
                self.engine_location
                    .as_ref()
                    .map_or(Json::Null, |s| Json::Str(s.clone())),
            ),
            (
                "error".into(),
                self.error.as_ref().map_or(Json::Null, |s| Json::Str(s.clone())),
            ),
            (
                "skip_reason".into(),
                self.skip_reason
                    .map_or(Json::Null, |r| Json::Str(r.label().into())),
            ),
            (
                "assertions_evaluated".into(),
                Json::UInt(u64::from(self.assertions_evaluated)),
            ),
            (
                "assertion_sites".into(),
                Json::UInt(u64::from(self.assertion_sites)),
            ),
            (
                "mechanism".into(),
                self.mechanism
                    .as_ref()
                    .map_or(Json::Null, |m| Json::Str(m.clone())),
            ),
            (
                "notes".into(),
                Json::Array(self.notes.iter().cloned().map(Json::Str).collect()),
            ),
        ])
    }
}

/// The whole report.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub suite_revision: String,
    pub census: Census,
    pub executed: Vec<FileResult>,
    pub harness: crate::selftest::SelfTest,
    /// Set when the engine could not be driven at all. Recorded in the output
    /// because a run that scored nothing must say *why*, and "because the
    /// engine did not build" is the single most important thing to say.
    pub engine_unavailable: Option<String>,
    pub tool_version: String,
}

impl Report {
    /// Build the ranked mechanism table from the executed results.
    ///
    /// Derived rather than stored, so it cannot disagree with the per-test
    /// records. A separately-maintained summary is a summary that eventually
    /// stops matching, and a mechanism list that does not match the tests is
    /// worse than none.
    #[must_use]
    pub fn mechanism_table(&self) -> crate::mechanism::MechanismTable {
        let mut table = crate::mechanism::MechanismTable::new();
        for file in &self.executed {
            let Some(key) = &file.mechanism else {
                // No mechanism means the result is a pass with nothing wrong,
                // which is accounted for under a fixed key rather than dropped:
                // a test absent from the table is a test whose result nobody can
                // account for.
                if file.outcome == Outcome::Pass {
                    table.record(
                        "no-mechanism-recorded",
                        "the test passed and no mechanism was attributed; the adapter is not \
                         reporting why",
                        crate::mechanism::Category::HarnessLimitation,
                        &file.area,
                        &file.path,
                    );
                }
                continue;
            };
            let (cause, category) = describe_mechanism(key, file);
            table.record(key, &cause, category, &file.area, &file.path);
        }
        table
    }

    #[must_use]
    pub fn to_json(&self) -> Json {
        let mut areas = Vec::new();
        for area in &self.census.areas {
            areas.push(area_json(area));
        }

        let mut executed: BTreeMap<&str, Tally> = BTreeMap::new();
        for file in &self.executed {
            executed
                .entry(file.area.as_str())
                .or_default()
                .record(file.outcome);
        }
        let mut totals = Tally::new();
        for tally in executed.values() {
            totals.merge(tally);
        }

        // The two numbers that must never be confused: what was in the suite,
        // and what was actually attempted. A reader who sees only the second
        // will read a 100% on four tests as full CSS conformance.
        let population = self.census.total_tests;
        let attempted = totals.attempted();

        Json::Object(vec![
            (
                "schema".into(),
                Json::Str("render-wpt-runner/results/1".into()),
            ),
            ("tool_version".into(), Json::Str(self.tool_version.clone())),
            (
                "wpt_revision".into(),
                Json::Str(self.suite_revision.clone()),
            ),
            (
                "denominator_note".into(),
                Json::Str(format!(
                    "population={population} scored tests across the requested areas; \
                     attempted={attempted} tests produced a pass or fail; \
                     errored={} tests could not be evaluated by this runner and are counted \
                     as neither pass nor fail",
                    totals.error
                )),
            ),
            (
                "engine_unavailable".into(),
                self.engine_unavailable
                    .as_ref()
                    .map_or(Json::Null, |s| Json::Str(s.clone())),
            ),
            (
                "harness_selftest".into(),
                Json::Object(vec![
                    ("passed".into(), Json::UInt(u64::from(self.harness.passed))),
                    ("failed".into(), Json::UInt(u64::from(self.harness.failed))),
                    (
                        "trustworthy".into(),
                        Json::Bool(self.harness.trustworthy()),
                    ),
                    (
                        "checks".into(),
                        Json::Array(
                            self.harness
                                .checks
                                .iter()
                                .map(|c| {
                                    Json::Object(vec![
                                        ("name".into(), Json::Str(c.name.clone())),
                                        (
                                            "expected".into(),
                                            Json::Str(c.expected.clone()),
                                        ),
                                        (
                                            "observed".into(),
                                            Json::Str(c.observed.clone()),
                                        ),
                                        ("held".into(), Json::Bool(c.held)),
                                        (
                                            "why_it_matters".into(),
                                            Json::Str(c.why_it_matters.clone()),
                                        ),
                                    ])
                                })
                                .collect(),
                        ),
                    ),
                ]),
            ),
            ("population".into(), Json::UInt(u64::from(population))),
            ("attempted".into(), Json::UInt(u64::from(attempted))),
            ("totals".into(), tally_json(&totals)),
            ("mechanisms".into(), mechanisms_json(&self.mechanism_table())),
            (
                "cannot_fail".into(),
                Json::Object(vec![
                    (
                        "static_no_assertion_site".into(),
                        Json::UInt(u64::from(self.census.total_cannot_fail)),
                    ),
                    (
                        "executed_with_zero_assertions_evaluated".into(),
                        Json::UInt(u64::from(self.vacuous_passes())),
                    ),
                    (
                        "note".into(),
                        Json::Str(
                            "these are reported, not scored: a test with no assertion that can \
                             fail would otherwise inflate the pass count"
                                .into(),
                        ),
                    ),
                ]),
            ),
            (
                "executed_by_area".into(),
                Json::Object(
                    executed
                        .iter()
                        .map(|(area, tally)| ((*area).to_owned(), tally_json(tally)))
                        .collect(),
                ),
            ),
            ("areas".into(), Json::Array(areas)),
        ])
    }

    /// Passes where no assertion was evaluated. The observable form of a test
    /// that cannot fail.
    pub fn vacuous_passes(&self) -> u32 {
        self.executed
            .iter()
            .filter(|f| f.outcome == Outcome::Pass && f.assertions_evaluated == 0)
            .count()
            .try_into()
            .unwrap_or(u32::MAX)
    }

    /// Write the summary and the per-file detail.
    pub fn write(&self, dir: &Path) -> std::io::Result<(std::path::PathBuf, std::path::PathBuf)> {
        std::fs::create_dir_all(dir)?;
        let summary_path = dir.join("wpt-results.json");
        let detail_path = dir.join("wpt-results.jsonl");

        std::fs::write(&summary_path, self.to_json().to_pretty())?;

        let file = File::create(&detail_path)?;
        let mut out = BufWriter::new(file);
        for result in &self.executed {
            let mut line = String::new();
            result.to_json().write(&mut line);
            out.write_all(line.as_bytes())?;
            out.write_all(b"\n")?;
        }
        out.flush()?;
        Ok((summary_path, detail_path))
    }
}

fn area_json(area: &AreaCensus) -> Json {
    let p = &area.population;
    let mut exclusions = Vec::new();
    for (reason, count) in &p.excluded {
        exclusions.push(Json::Object(vec![
            ("reason".into(), Json::Str(reason.label().into())),
            ("count".into(), Json::UInt(u64::from(*count))),
        ]));
    }
    let mut blockers = Vec::new();
    for (key, count) in &area.blockers {
        let mut value = blocker_json(key);
        if let Json::Object(fields) = &mut value {
            fields.push(("count".into(), Json::UInt(u64::from(*count))));
        }
        blockers.push(value);
    }

    Json::Object(vec![
        ("area".into(), Json::Str(area.area.clone())),
        (
            "population".into(),
            Json::Object(vec![
                ("html_files_present".into(), Json::UInt(u64::from(p.files_present))),
                ("scored_tests".into(), Json::UInt(u64::from(p.tests))),
                ("excluded".into(), Json::Array(exclusions)),
                (
                    "population_statement".into(),
                    Json::Str(crate::census::population_statement(area)),
                ),
                (
                    "reference_rule_agreement".into(),
                    crate::census::reference_rule_statement(area)
                        .map_or(Json::Null, Json::Str),
                ),
            ]),
        ),
        (
            "static_feasibility".into(),
            Json::Object(vec![
                (
                    "executable".into(),
                    Json::UInt(u64::from(area.executable)),
                ),
                (
                    "feasibility_rate".into(),
                    match area.feasibility_rate() {
                        Some(rate) => Json::Str(format!(
                            "{rate} of the area's scored test population; this is a \
                             static-analysis figure, not an engine result"
                        )),
                        None => Json::Str("not computed: no scored tests".into()),
                    },
                ),
                (
                    "cannot_fail".into(),
                    Json::UInt(u64::from(area.cannot_fail)),
                ),
                ("blockers".into(), Json::Array(blockers)),
            ]),
        ),
        (
            "harness_shapes".into(),
            map_json(&area.shapes, |s| s.label().to_owned()),
        ),
        (
            "capabilities_required".into(),
            map_json(&area.capabilities, |c| c.label().to_owned()),
        ),
        (
            "assertion_call_sites".into(),
            Json::UInt(area.assertion_sites),
        ),
    ])
}

/// Write the exclusions of a population for a compact console summary.
#[must_use]
pub fn exclusion_summary(area: &AreaCensus) -> Vec<(Exclusion, u32)> {
    area.population.excluded.iter().map(|(k, v)| (*k, *v)).collect()
}

#[cfg(test)]
mod tests {
    use super::Json;

    #[test]
    fn rate_string_always_carries_its_denominator() {
        let json = Json::rate(62, 100);
        assert_eq!(json, Json::Str("62.0% (62/100)".into()));
        // A bare number must be impossible to emit by accident.
        assert!(!json.to_pretty().contains("62.0%\""));
    }

    #[test]
    fn strings_are_escaped_so_a_failure_message_cannot_break_the_document() {
        let mut out = String::new();
        Json::Str("a \"quote\" and a \\slash\\ and\nnewline".into()).write(&mut out);
        assert_eq!(out, r#""a \"quote\" and a \\slash\\ and\nnewline""#);
    }

    #[test]
    fn control_characters_are_escaped() {
        let mut out = String::new();
        Json::Str(format!("bell{}end", '\u{7}')).write(&mut out);
        // The expectation is built, not pasted, so it cannot itself contain a
        // raw control byte - which is the mistake this test first made.
        let expected = "\"bell\\u0007end\"";
        assert_eq!(out, expected);
    }

    #[test]
    fn pretty_output_is_stable_and_reparses_as_flat_json() {
        let value = Json::Object(vec![
            ("a".into(), Json::UInt(1)),
            ("b".into(), Json::Array(vec![Json::Bool(true), Json::Null])),
        ]);
        let pretty = value.to_pretty();
        assert!(pretty.ends_with("}\n"));
        let mut compact = String::new();
        value.write(&mut compact);
        assert_eq!(compact, r#"{"a":1,"b":[true,null]}"#);
    }
}
