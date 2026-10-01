//! Measure rENDER's HTML tree construction against the pinned html5lib
//! tree-construction suite.
//!
//! Run it with:
//!
//! ```text
//! powershell -File tools/html5lib/fetch-html5lib.ps1   # once, pinned
//! cargo run -p render-html --example html5lib
//! ```
//!
//! The measurement is the deliverable, so the report never collapses the four
//! outcomes into one percentage: a raw `fail / total` would count a test the
//! runner could not even attempt next to a test whose tree is wrong, and the
//! two are not the same claim about the engine.
//!
//! Four outcomes are reported separately.
//!
//! * **engine defect** - the tree is wrong in a way that matters.
//! * **acceptable difference** - the trees differ and the difference does not
//!   matter. Every rule in [`classify`] that produces this carries the reason,
//!   and the reason is printed with the count.
//! * **unimplemented feature** - the test exercises something this engine does
//!   not do, and the test says so (a `#document-fragment` section, a scripting
//!   test that needs a running script).
//! * **harness limitation** - the runner could not evaluate it.
//!
//! The classification is a rule table, not a heuristic: an unrecognised
//! difference falls through to **engine defect**, which is the pessimistic
//! direction on purpose. A number that quietly forgives what it does not
//! understand is how this project has been wrong three times already.

mod dat;
mod dump;

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use render_dom::{Dom, Namespace, NodeId, NodeKind};
use render_html::{parse_document_with_scripting, serialize_html_fragment_with_scripting};

use dat::DatTest;
use dump::{DiffEvent, TreeDiff};

/// The maximum number of diff events recorded per test. Enough to see a whole
/// mechanism, small enough that a wildly wrong tree cannot dominate the run.
const MAX_DIFF_EVENTS: usize = 24;

fn main() -> ExitCode {
    let options = Options::from_args();
    let suites = match discover_suites(&options) {
        Ok(suites) => suites,
        Err(message) => {
            eprintln!("error: {message}");
            return ExitCode::from(2);
        }
    };
    if suites.is_empty() {
        eprintln!("error: no .dat files found; run tools/html5lib/fetch-html5lib.ps1 first");
        return ExitCode::from(2);
    }

    let mut report = Report::default();
    for suite in &suites {
        let outcomes = run_suite(suite, &options);
        report.absorb(suite, outcomes);
    }
    report.print(&options);
    if options.negative_control {
        return report.run_negative_control(&suites, &options);
    }
    if options.strict && report.count(Outcome::EngineDefect) > 0 {
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Options {
    /// `html5lib`, `wpt`, or `all`.
    suite: String,
    /// A substring the test id must contain to be run.
    filter: Option<String>,
    /// Print the data and both trees for each failing test.
    explain: bool,
    /// Print the data and both trees for passing tests too.
    explain_all: bool,
    /// Cap the number of failing tests printed.
    print_limit: usize,
    /// Exit non-zero when any engine defect is found.
    strict: bool,
    /// Prove the runner can fail, by mutating the suite's expectations.
    negative_control: bool,
    /// Do not print the per-file or mechanism tables.
    quiet_tables: bool,
    /// Tabulate the parse-error-count disagreements by signed delta, which is
    /// what distinguishes a missing rule from a rule that is nearly right.
    error_gaps: bool,
}

impl Options {
    fn from_args() -> Self {
        let mut options = Self {
            suite: "all".to_owned(),
            filter: None,
            explain: false,
            explain_all: false,
            print_limit: 40,
            strict: false,
            negative_control: false,
            quiet_tables: false,
            error_gaps: false,
        };
        let mut args = env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--suite" => options.suite = args.next().unwrap_or_default(),
                "--filter" => options.filter = args.next(),
                "--explain" => options.explain = true,
                "--explain-all" => options.explain_all = true,
                "--limit" => {
                    options.print_limit = args
                        .next()
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(options.print_limit);
                }
                "--strict" => options.strict = true,
                "--negative-control" => options.negative_control = true,
                "--no-tables" => options.quiet_tables = true,
                "--error-gaps" => options.error_gaps = true,
                other => {
                    eprintln!("error: unknown argument {other}");
                    std::process::exit(2);
                }
            }
        }
        options
    }
}

// ---------------------------------------------------------------------------
// Suite discovery
// ---------------------------------------------------------------------------

/// A fetched suite: one directory of `.dat` files and the revision it came from.
struct Suite {
    label: String,
    files: Vec<PathBuf>,
}

/// Find the cached suites.
///
/// The cache root is `tools/html5lib/.cache`, which is gitignored: the suite is
/// never committed, so a clean checkout has to fetch it before this runner can
/// say anything, and a missing suite is reported as a missing suite rather than
/// as a perfect score.
fn discover_suites(options: &Options) -> Result<Vec<Suite>, String> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest
        .parent()
        .and_then(Path::parent)
        .ok_or("cannot locate the repository root from the crate manifest")?
        .join("tools")
        .join("html5lib")
        .join(".cache");

    let override_root = env::var_os("RENDER_HTML5LIB_CACHE")
        .map(PathBuf::from)
        .unwrap_or(root);
    if !override_root.is_dir() {
        return Ok(Vec::new());
    }

    let mut suites = Vec::new();
    let mut entries = fs::read_dir(&override_root)
        .map_err(|error| format!("cannot read {}: {error}", override_root.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    entries.sort();

    for path in entries {
        if !path.is_dir() {
            continue;
        }
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let revision = fs::read_to_string(path.join(".render-revision"))
            .unwrap_or_else(|_| "UNKNOWN".to_owned())
            .trim()
            .to_owned();
        // `html5lib-tests-<rev>/tree-construction/*.dat` and
        // `wpt-parsing-<rev>/*.dat`.
        let (label, directory) = if name.starts_with("html5lib-tests-") {
            ("html5lib-tests".to_owned(), path.join("tree-construction"))
        } else if name.starts_with("wpt-parsing-") {
            ("wpt/html".to_owned(), path.clone())
        } else {
            continue;
        };
        if !directory.is_dir() {
            continue;
        }
        if options.suite != "all" && !label.starts_with(&options.suite) {
            continue;
        }
        let mut files = fs::read_dir(&directory)
            .map_err(|error| format!("cannot read {}: {error}", directory.display()))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "dat"))
            .collect::<Vec<_>>();
        files.sort();
        if files.is_empty() {
            continue;
        }
        suites.push(Suite {
            // The revision is printed with every number this report contains,
            // because a conformance figure without the revision it was measured
            // on is not comparable to anything, including to itself next month.
            label: format!("{label}@{}", short(&revision, "")),
            files,
        });
    }
    Ok(suites)
}

/// The first twelve characters of a revision, which is enough to identify one
/// and short enough for a table.
/// The first twelve characters of a revision, which is enough to identify one
/// and short enough for a table.
fn short(revision: &str, fallback: &str) -> String {
    if revision == "UNKNOWN" {
        return fallback.to_owned();
    }
    revision.chars().take(12).collect()
}

// ---------------------------------------------------------------------------
// Outcomes
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Outcome {
    Pass,
    AcceptableDifference,
    Unimplemented,
    HarnessLimitation,
    EngineDefect,
}

impl Outcome {
    const ALL: [Self; 5] = [
        Self::Pass,
        Self::AcceptableDifference,
        Self::Unimplemented,
        Self::HarnessLimitation,
        Self::EngineDefect,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::AcceptableDifference => "acceptable difference",
            Self::Unimplemented => "unimplemented feature",
            Self::HarnessLimitation => "harness limitation",
            Self::EngineDefect => "engine defect",
        }
    }
}

/// One test in one scripting mode, with its verdict.
struct CaseResult {
    suite: String,
    test: DatTest,
    scripting_enabled: bool,
    outcome: Outcome,
    /// A stable name for the mechanism that produced the outcome. Engine
    /// defects always have one; so does every rule that produced a non-pass
    /// outcome, so that the tables rank causes and not files.
    mechanism: String,
    /// Why the outcome is what it is. Printed with the count.
    reason: String,
    expected: Vec<String>,
    actual: Vec<String>,
    diff: TreeDiff,
    /// The fragment serialisation the suite expects, when it has one.
    expected_serialization: Option<String>,
    actual_serialization: Option<String>,
    expected_error_count: usize,
    actual_error_count: usize,
    /// Set when the runner could not evaluate the test at all.
    note: Option<String>,
}

#[derive(Default)]
struct Report {
    results: Vec<CaseResult>,
    suites: Vec<String>,
}

impl Report {
    fn absorb(&mut self, suite: &Suite, mut results: Vec<CaseResult>) {
        self.suites.push(suite.label.clone());
        self.results.append(&mut results);
    }

    fn counts(&self) -> BTreeMap<Outcome, usize> {
        let mut counts = BTreeMap::new();
        for outcome in Outcome::ALL {
            counts.insert(outcome, 0);
        }
        for result in &self.results {
            *counts.entry(result.outcome).or_insert(0) += 1;
        }
        counts
    }

    fn count(&self, outcome: Outcome) -> usize {
        self.results.iter().filter(|r| r.outcome == outcome).count()
    }

    fn total(&self) -> usize {
        self.results.len()
    }

    /// The number of `(test, scripting mode)` pairs the suite defines. A test
    /// with neither `#script-on` nor `#script-off` runs twice, which is why the
    /// denominator is larger than the number of `#data` sections.
    fn print(&self, options: &Options) {
        let total = self.total();
        let counts = self.counts();
        println!("rENDER HTML tree construction against the html5lib suite");
        for suite in &self.suites {
            println!("  suite: {suite}");
        }
        println!();
        println!("{total} (test, scripting-mode) pairs");
        for outcome in Outcome::ALL {
            let count = counts[&outcome];
            println!(
                "  {:<22} {count:>6}  ({count}/{total}, {:.2}%)",
                outcome.label(),
                if total == 0 {
                    0.0
                } else {
                    100.0 * f64::from(u32::try_from(count).unwrap_or(u32::MAX)) / total as f64
                }
            );
        }
        println!();
        println!(
            "  conformance (pass / total)                    {:>6}  ({}/{}, {:.2}%)",
            counts[&Outcome::Pass],
            counts[&Outcome::Pass],
            total,
            if total == 0 {
                0.0
            } else {
                100.0 * f64::from(u32::try_from(counts[&Outcome::Pass]).unwrap_or(u32::MAX))
                    / total as f64
            }
        );
        let decided = total - counts[&Outcome::Unimplemented] - counts[&Outcome::HarnessLimitation];
        if decided > 0 {
            println!(
                "  conformance (pass / (total - unimplemented - harness)) {}/{} ({:.2}%)",
                counts[&Outcome::Pass],
                decided,
                100.0 * f64::from(u32::try_from(counts[&Outcome::Pass]).unwrap_or(u32::MAX))
                    / decided as f64
            );
        }
        self.print_error_counts();
        println!();

        if !options.quiet_tables {
            self.print_mechanisms();
            self.print_files();
        }
        self.print_error_gaps(options);
        self.print_failures(options);
    }

    /// The suite's `#errors` sections say only how many parse errors a
    /// conformant implementation reports, and the standard requires a
    /// conformance checker to report at least one when there is at least one and
    /// none when there are none. That is a second, separate conformance claim
    /// from the tree one, and it is reported separately rather than folded into
    /// it: a parser can build the right tree and still report the wrong number
    /// of errors, and a reader deciding whether to trust a tree figure has no way
    /// to see that unless the two are apart.
    fn print_error_counts(&self) {
        let mut compared = 0usize;
        let mut matching = 0usize;
        let mut under = 0usize;
        let mut over = 0usize;
        for result in &self.results {
            if result.test.fragment_context.is_some() {
                continue;
            }
            compared += 1;
            match result.actual_error_count.cmp(&result.expected_error_count) {
                std::cmp::Ordering::Equal => matching += 1,
                std::cmp::Ordering::Less => under += 1,
                std::cmp::Ordering::Greater => over += 1,
            }
        }
        if compared == 0 {
            return;
        }
        println!(
            "  parse-error counts agree      {:>6}  ({}/{}, {:.2}%)   [reported, not part of the tree figure]",
            matching,
            matching,
            compared,
            100.0 * f64::from(u32::try_from(matching).unwrap_or(u32::MAX)) / compared as f64
        );
        println!("  reported fewer than expected  {under:>6}   reported more: {over}");
    }

    /// The error-count gap, grouped by *how far off* each case is and by
    /// whether its tree is right.
    ///
    /// A single miscount of one error in forty documents and a single mechanism
    /// that never reports at all look identical in the totals, and they are not
    /// the same amount of work: the first is a rounding difference in a rule
    /// that mostly works, the second is a missing rule. So the deltas are
    /// tabulated, and the cases at each delta are named, which is what turns
    /// "3,100 cases disagree" into a list of causes.
    fn print_error_gaps(&self, options: &Options) {
        if !options.error_gaps {
            return;
        }
        // (delta, tree is right) -> the cases at that delta.
        let mut groups: BTreeMap<(i64, bool), Vec<String>> = BTreeMap::new();
        for result in &self.results {
            if result.test.fragment_context.is_some() {
                continue;
            }
            let delta = result.actual_error_count as i64 - result.expected_error_count as i64;
            if delta == 0 {
                continue;
            }
            let id = format!("{} [{}]", result.test.id(), mode_name(result.scripting_enabled));
            groups
                .entry((delta, result.outcome == Outcome::Pass))
                .or_default()
                .push(id);
        }
        println!();
        println!("parse-error gaps, by signed delta (negative = under-reported)");
        println!(
            "  {:>6}  {:>10}  {:>6}  {}",
            "delta", "tree right", "cases", "example"
        );
        for ((delta, tree_right), cases) in &groups {
            println!(
                "  {delta:>6}  {:>10}  {:>6}  {}",
                if *tree_right { "yes" } else { "no" },
                cases.len(),
                cases.first().map(String::as_str).unwrap_or("")
            );
        }
        if options.explain {
            println!();
            println!("every case at each delta:");
            for ((delta, tree_right), cases) in &groups {
                println!(
                    "\n== delta {delta:+} (tree right: {}) : {} cases",
                    if *tree_right { "yes" } else { "no" },
                    cases.len()
                );
                for id in cases {
                    println!("   {id}");
                }
            }
        }
    }

    /// The interesting table: distinct mechanisms, ranked by how many cases each
    /// accounts for. A single root cause can account for forty tests, and
    /// fixing it is worth more than the count suggests.
    fn print_mechanisms(&self) {
        let mut grouped: BTreeMap<(Outcome, String, String), usize> = BTreeMap::new();
        for result in &self.results {
            if result.outcome == Outcome::Pass {
                continue;
            }
            *grouped
                .entry((
                    result.outcome,
                    result.mechanism.clone(),
                    result.reason.clone(),
                ))
                .or_insert(0) += 1;
        }
        if grouped.is_empty() {
            println!("no non-pass outcomes");
            println!();
            return;
        }
        let mut rows = grouped.into_iter().collect::<Vec<_>>();
        rows.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        println!("mechanisms, ranked by cases accounted for");
        println!("  {:>5}  {:<22} mechanism", "cases", "outcome");
        for ((outcome, mechanism, reason), count) in rows {
            println!("  {count:>5}  {:<22} {mechanism}", outcome.label());
            println!("         {}", reason);
        }
        println!();
    }

    fn print_files(&self) {
        let mut by_file: BTreeMap<String, BTreeMap<Outcome, usize>> = BTreeMap::new();
        for result in &self.results {
            let entry = by_file
                .entry(format!("{}/{}", result.suite, result.test.file))
                .or_default();
            *entry.entry(result.outcome).or_insert(0) += 1;
        }
        println!("per file (total = pass + acceptable + unimplemented + harness + defect)");
        println!(
            "  {:<48} {:>5} {:>5} {:>5} {:>5} {:>5}",
            "file", "total", "pass", "ok-d", "unimp", "defect"
        );
        for (file, counts) in &by_file {
            let total: usize = counts.values().sum();
            // `get` rather than the index operator: a file with no cases in one
            // category is the normal case, and a measurement tool that panics on
            // its own zero is a measurement tool whose last line is a panic.
            let of = |outcome: Outcome| counts.get(&outcome).copied().unwrap_or(0);
            println!(
                "  {file:<48} {total:>5} {:>5} {:>5} {:>5} {:>5}",
                of(Outcome::Pass),
                of(Outcome::AcceptableDifference),
                of(Outcome::Unimplemented) + of(Outcome::HarnessLimitation),
                of(Outcome::EngineDefect),
            );
        }
        println!();
    }

    fn print_failures(&self, options: &Options) {
        let mut shown = 0usize;
        for result in &self.results {
            if result.outcome == Outcome::Pass && !options.explain_all {
                continue;
            }
            if !options.explain && !options.explain_all {
                println!(
                    "{} {} [{}] {} :: {} :: {} :: data {:?}",
                    result.suite,
                    result.test.id(),
                    mode_name(result.scripting_enabled),
                    result.outcome.label(),
                    result.mechanism,
                    first_difference_text(&result.diff),
                    clipped(&result.test.data)
                );
                if shown >= options.print_limit {
                    println!("  ... more suppressed; raise --limit");
                    break;
                }
                shown += 1;
                continue;
            }
            println!("--------------------------------------------------------------");
            println!(
                "{} {} [{}]",
                result.suite,
                result.test.id(),
                mode_name(result.scripting_enabled)
            );
            println!(
                "  outcome:   {} ({})",
                result.outcome.label(),
                result.mechanism
            );
            println!("  reason:    {}", result.reason);
            if let Some(context) = &result.test.fragment_context {
                println!("  context:   {context}");
            }
            println!("  data:      {:?}", result.test.data);
            println!(
                "  errors:    suite expects {}, parser reported {}",
                result.expected_error_count, result.actual_error_count
            );
            if let (Some(expected), Some(actual)) =
                (&result.expected_serialization, &result.actual_serialization)
            {
                println!("  serialization expected: {expected}");
                println!("  serialization actual:   {actual}");
            }
            println!("  expected tree:");
            for line in &result.expected {
                println!("    {line}");
            }
            println!("  actual tree:");
            for line in &result.actual {
                println!("    {line}");
            }
            if !result.diff.is_empty() {
                println!("  difference:");
                for event in &result.diff.events {
                    let at = event.position();
                    match event {
                        DiffEvent::Missing { expected, .. } => {
                            println!("    line {at}: missing {expected:?}");
                        }
                        DiffEvent::Unexpected { actual, .. } => {
                            println!("    line {at}: unexpected {actual:?}");
                        }
                        DiffEvent::Substituted {
                            expected, actual, ..
                        } => {
                            println!("    line {at}: expected {expected:?}, got {actual:?}");
                        }
                    }
                }
            }
            if let Some(note) = &result.note {
                println!("  note:      {note}");
            }
            if shown >= options.print_limit {
                println!("  ... more suppressed; raise --limit");
                break;
            }
            shown += 1;
        }
        if !options.explain && !options.explain_all && self.count(Outcome::Pass) > 0 {
            let pass = self.count(Outcome::Pass);
            println!();
            println!("{pass} passing cases are not listed; --explain-all shows them too.");
        }
    }

    /// Prove the runner can fail.
    ///
    /// The failure mode this guards against is a comparison that reports zero
    /// failures because it silently matched nothing: no data parsed, no tree
    /// compared, everything counted as a pass. A pass count alone cannot detect
    /// that, because "0 failures" and "0 comparisons" print the same. So the
    /// suite's own expectations are mutated in four different ways and the
    /// runner is re-run over exactly the cases each mutation changed.
    ///
    /// What is counted is **cases that were passing and stop passing**. A
    /// mutation that merely re-reports cases which were already failing has
    /// proved nothing however many of them there are: an earlier version of this
    /// control accepted 128 such cases and called it a detection, which would
    /// have passed with a comparison that never looked at a text node at all. A
    /// mutation counts as detected only if it turns at least one pass into a
    /// non-pass, and the report says how many cases it actually changed, so a
    /// mutation that could not apply to anything is visible as such.
    fn run_negative_control(&self, suites: &[Suite], options: &Options) -> ExitCode {
        println!("==============================================================");
        println!("NEGATIVE CONTROL: mutate the suite's expectations");
        println!("(detected = a case that was passing and no longer is)");
        println!("==============================================================");
        let mut uncaught = Vec::new();
        for mutation in Mutation::ALL {
            let mut changed = 0usize;
            let mut newly_non_pass = 0usize;
            let mut still_passing = 0usize;
            let mut examples = Vec::new();
            for suite in suites {
                for test in read_suite(suite) {
                    for scripting_enabled in test.scripting.modes() {
                        let mut mutated = test.clone();
                        if !(mutation.apply)(&mut mutated) {
                            continue;
                        }
                        changed += 1;
                        let (before, _, _) = evaluate(&test, scripting_enabled);
                        if before != Outcome::Pass {
                            continue;
                        }
                        let (after, mechanism, _) = evaluate(&mutated, scripting_enabled);
                        if after == Outcome::Pass {
                            still_passing += 1;
                            continue;
                        }
                        newly_non_pass += 1;
                        if examples.len() < 3 {
                            examples.push(format!(
                                "{} [{}] -> {mechanism}",
                                mutated.id(),
                                mode_name(scripting_enabled)
                            ));
                        }
                    }
                }
            }
            println!();
            println!("mutation: {}", mutation.name);
            println!("  description: {}", mutation.description);
            println!("  cases the mutation changed: {changed}");
            println!("  detected (was passing, no longer is): {newly_non_pass}");
            println!("  undetected (was passing, still is):  {still_passing}");
            for example in &examples {
                println!("  e.g. {example}");
            }
            if changed == 0 {
                println!("  VACUOUS: the mutation changed no case, so it proves nothing");
                uncaught.push(mutation.name);
            } else if newly_non_pass == 0 {
                println!("  NOT DETECTED - the comparison ignored the mutation");
                uncaught.push(mutation.name);
            } else {
                println!("  detected");
            }
        }
        println!();
        if uncaught.is_empty() {
            println!(
                "negative control PASSED: all {} mutations were detected, so the runner can fail.",
                Mutation::ALL.len()
            );
            let _ = options;
            ExitCode::SUCCESS
        } else {
            println!("negative control FAILED: undetected mutations: {uncaught:?}");
            ExitCode::from(1)
        }
    }
}

fn mode_name(scripting_enabled: bool) -> &'static str {
    if scripting_enabled {
        "script-on"
    } else {
        "script-off"
    }
}

/// One line describing the first difference, for the compact failure list.
fn first_difference_text(diff: &TreeDiff) -> String {
    match diff.first() {
        None => String::new(),
        Some(DiffEvent::Missing { at, expected }) => format!("L{at} missing {expected:?}"),
        Some(DiffEvent::Unexpected { at, actual }) => format!("L{at} extra {actual:?}"),
        Some(DiffEvent::Substituted {
            at,
            expected,
            actual,
        }) => format!("L{at} want {expected:?} got {actual:?}"),
    }
}

// ---------------------------------------------------------------------------
// Negative control mutations
// ---------------------------------------------------------------------------

struct Mutation {
    name: &'static str,
    description: &'static str,
    apply: fn(&mut DatTest) -> bool,
}

impl Mutation {
    const ALL: [Self; 4] = [
        Self {
            name: "rename-body",
            description: "rename the expected `body` element to `bodyy`",
            apply: |test: &mut DatTest| {
                let mut changed = false;
                for line in &mut test.expected_tree {
                    if line.trim_start().starts_with("| ") && line.ends_with("<body>") {
                        let prefix = &line[..line.len() - "<body>".len()];
                        *line = format!("{prefix}<bodyy>");
                        changed = true;
                    }
                }
                changed
            },
        },
        Self {
            name: "drop-last-line",
            description: "delete the last line of the expected tree",
            apply: |test: &mut DatTest| {
                if test.expected_tree.len() < 2 {
                    return false;
                }
                test.expected_tree.pop();
                true
            },
        },
        Self {
            name: "swap-root-children",
            description: "swap the first two children of the expected document element",
            apply: |test: &mut DatTest| {
                if test.expected_tree.len() < 3 || !test.expected_tree[0].ends_with("<html>") {
                    return false;
                }
                test.expected_tree.swap(1, 2);
                true
            },
        },
        Self {
            name: "perturb-text",
            description: "append `!` to every expected text node and attribute value, and \
                          append a marker element to the input when the expected tree has \
                          neither",
            apply: |test: &mut DatTest| {
                let mut changed = false;
                for line in &mut test.expected_tree {
                    if line.ends_with('"')
                        && (line.trim_start().starts_with("| \"") || line.contains("=\""))
                    {
                        line.insert(line.len() - 1, '!');
                        changed = true;
                    }
                }
                if !changed {
                    // A tree with neither a text node nor an attribute cannot be
                    // perturbed that way, and a mutation that applies to nothing
                    // proves nothing, so the input itself is changed instead.
                    test.data.push_str("<b>mutation-sentinel</b>");
                    changed = true;
                }
                changed
            },
        },
    ];
}

// ---------------------------------------------------------------------------
// Running the suite
// ---------------------------------------------------------------------------

fn read_suite(suite: &Suite) -> Vec<DatTest> {
    let mut tests = Vec::new();
    for file in &suite.files {
        match dat::parse_file(file) {
            Ok(parsed) => tests.extend(parsed),
            Err(error) => {
                eprintln!("warning: skipping {}", error);
            }
        }
    }
    tests
}

fn run_suite(suite: &Suite, options: &Options) -> Vec<CaseResult> {
    let tests = read_suite(suite);
    let mut results = Vec::new();
    for test in tests {
        if let Some(filter) = &options.filter
            && !format!("{}/{}", suite.label, test.id()).contains(filter.as_str())
        {
            continue;
        }
        for scripting_enabled in test.scripting.modes() {
            let _ = test.scripting;
            let (outcome, mechanism, reason) = evaluate(&test, scripting_enabled);
            let (expected, actual, diff, serialization) = trees(&test, scripting_enabled);
            let (expected_errors, actual_errors) = error_counts(&test, scripting_enabled);
            results.push(CaseResult {
                suite: suite.label.clone(),
                test: test.clone(),
                scripting_enabled,
                outcome,
                mechanism,
                reason,
                expected,
                actual,
                diff,
                expected_serialization: test.expected_serialization.clone(),
                actual_serialization: serialization,
                expected_error_count: expected_errors,
                actual_error_count: actual_errors,
                note: None,
            });
        }
    }
    results
}

/// Run the engine on the test and collect both trees.
fn trees(
    test: &DatTest,
    scripting_enabled: bool,
) -> (Vec<String>, Vec<String>, TreeDiff, Option<String>) {
    if test.fragment_context.is_some() {
        // The engine has no fragment parsing entry point; `evaluate` reports
        // this as an unimplemented feature, and there is no tree to compare.
        return (
            test.expected_tree.clone(),
            Vec::new(),
            TreeDiff::default(),
            None,
        );
    }
    let output = parse_document_with_scripting(&test.data, scripting_enabled);
    let dump = dump::dump_document(&output.dom);
    let diff = dump::diff(&test.expected_tree, &dump.lines, MAX_DIFF_EVENTS);
    let body = output
        .dom
        .children(output.dom.document())
        .unwrap_or_default()
        .iter()
        .find_map(|node| body_element(&output.dom, *node));
    let serialization = body
        .map(|body| serialize_html_fragment_with_scripting(&output.dom, body, scripting_enabled));
    (test.expected_tree.clone(), dump.lines, diff, serialization)
}

fn error_counts(test: &DatTest, scripting_enabled: bool) -> (usize, usize) {
    if test.fragment_context.is_some() {
        return (0, 0);
    }
    let output = parse_document_with_scripting(&test.data, scripting_enabled);
    (
        test.expected_error_count + test.expected_new_error_count,
        output.errors.len(),
    )
}

fn body_element(dom: &Dom, node: NodeId) -> Option<NodeId> {
    let mut stack = vec![node];
    while let Some(candidate) = stack.pop() {
        if let Some(NodeKind::Element(data)) = dom.node(candidate).map(render_dom::Node::kind)
            && data.namespace == Namespace::Html
            && data.local_name == "body"
        {
            return Some(candidate);
        }
        for child in dom.children(candidate).unwrap_or_default() {
            stack.push(*child);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Classification
// ---------------------------------------------------------------------------

/// Classify one test in one scripting mode. Returns the outcome, the mechanism
/// name, and the reason.
///
/// The fall-through is [`Outcome::EngineDefect`]: anything the rule table does
/// not explicitly explain is treated as a defect, because the alternative is a
/// runner that invents excuses.
fn evaluate(test: &DatTest, scripting_enabled: bool) -> (Outcome, String, String) {
    if test.fragment_context.is_none() {
        let (expected, actual, diff, _) = trees(test, scripting_enabled);
        if diff.is_empty() {
            return (Outcome::Pass, String::new(), String::new());
        }
        if let Some(reason) = is_processing_instruction_spelling(&diff) {
            return (
                Outcome::AcceptableDifference,
                "processing-instruction-dump-spelling".to_owned(),
                reason,
            );
        }
        if is_null_dropped_in_foreign_content(&diff) {
            return (
                Outcome::AcceptableDifference,
                "null-dropped-in-foreign-content".to_owned(),
                "the suite drops U+0000 in foreign content, while the standard replaces it \
                 with U+FFFD in the data state (13.2.5.1) *and* inserts U+FFFD for a NULL \
                 character token in foreign content (13.2.6.5), and this engine does both; \
                 U+0000 is invisible in any rendering, so the two trees cannot differ on \
                 screen"
                    .to_owned(),
            );
        }
        let (mechanism, reason) = describe_defect(&expected, &actual, &diff);
        return (Outcome::EngineDefect, mechanism, reason);
    }

    // --- Rules for tests the engine is not expected to pass outright. -------

    // A `#document-fragment` test asks for the HTML fragment parsing algorithm
    // with a context element (13.4). This engine exposes only whole-document
    // parsing, so there is nothing to compare and calling it a defect would be
    // a measurement artefact rather than a finding.
    (
        Outcome::Unimplemented,
        "html-fragment-parsing".to_owned(),
        "the test sets a #document-fragment context element, so it exercises the \
         HTML fragment parsing algorithm (13.4); this engine parses whole documents only"
            .to_owned(),
    )
}

/// The one difference this engine is allowed to have with the suite, and why.
///
/// **The suite's own format documentation and its own data disagree about how a
/// processing instruction is written.** `tree-construction/README.md` says
/// "Processing instructions must be `<?`, then the target, a space, the data and
/// then `>`", which is what this engine emits. Every expectation in
/// `processing-instructions.dat` is instead written `<?target data?>`: the `?`
/// is inside the delimiters, so an empty data reads `<?something ?>`.
///
/// The data itself is not in dispute. What this engine produces is what the
/// standard's own tokenization produces (13.2.5.72-76) for the same input,
/// including the whitespace runs the "after processing instruction target"
/// state drops and the `?` the questionable state consumes without appending.
/// Only the spelling of the expected line differs, so the tree is the same and
/// this is a difference in the suite's dump format, not in the parse.
///
/// The rule is deliberately narrow: it fires only when **every** difference in
/// the case is a processing-instruction line that becomes the engine's line by
/// having one `?` inserted before its closing `>`. Any other difference in the
/// same file falls through to **engine defect**, so a real tree error in a
/// processing-instruction test cannot hide behind this rule.
fn is_processing_instruction_spelling(diff: &TreeDiff) -> Option<String> {
    if diff.events.is_empty() {
        return None;
    }
    for event in &diff.events {
        let DiffEvent::Substituted {
            expected, actual, ..
        } = event
        else {
            return None;
        };
        let (Some(want), Some(got)) = (
            processing_instruction_body(expected),
            processing_instruction_body(actual),
        ) else {
            return None;
        };
        if want.strip_suffix("?>") != got.strip_suffix('>') {
            return None;
        }
    }
    Some(
        "the suite writes a processing instruction as `<?target data?>` while its own \
         format documentation (tree-construction/README.md) and this engine's tree use \
         `<?target data>`; the data is identical either way, and it is the specification's \
         own tokenization (13.2.5.72-76) that produces it"
            .to_owned(),
    )
}

/// Name a defect's mechanism and give the reason the table prints.
///
/// This is a shape classification, not an explanation: it turns a tree
/// difference into a group name so that the ranking counts causes. A group with
/// forty members is one bug; a group with one member is forty bugs.
/// Whether every difference in the case is a text node that differs only by
/// `U+FFFD REPLACEMENT CHARACTER` standing where the suite has nothing.
///
/// Narrow on purpose: the expected and the actual line must be the same text
/// node at the same depth, and the actual one must be exactly the expected one
/// with every `U+FFFD` deleted. A difference in anything else in the same case
/// falls through to **engine defect**.
fn is_null_dropped_in_foreign_content(diff: &TreeDiff) -> bool {
    if diff.events.is_empty() {
        return false;
    }
    for event in &diff.events {
        let DiffEvent::Substituted {
            expected, actual, ..
        } = event
        else {
            return false;
        };
        let (Some(want), Some(got)) = (text_node_body(expected), text_node_body(actual)) else {
            return false;
        };
        if got.replace('\u{fffd}', "") != want {
            return false;
        }
    }
    true
}

/// The value of a dumped text-node line, or `None` for any other line.
fn text_node_body(line: &str) -> Option<&str> {
    let value = line.trim_start().strip_prefix('|')?.trim_start();
    let value = value.strip_prefix('"')?;
    value.strip_suffix('"')
}

/// The `<?target data>` payload of a dumped processing-instruction line, or
/// `None` for any other line. The indentation between the `|` and the `<?` is
/// the node's depth and is the same in both trees, so it is dropped.
fn processing_instruction_body(line: &str) -> Option<&str> {
    line.trim_start()
        .strip_prefix('|')?
        .trim_start()
        .strip_prefix("<?")
}

fn describe_defect(expected: &[String], actual: &[String], diff: &TreeDiff) -> (String, String) {
    let Some(first) = diff.first() else {
        return (
            "no-events".to_owned(),
            "the trees differ but the walk found no differing line".to_owned(),
        );
    };
    let shape = match first {
        DiffEvent::Missing { expected, .. } => {
            format!("missing-node:missing {}", node_summary(expected))
        }
        DiffEvent::Unexpected { actual, .. } => {
            format!("extra-node:unexpected {}", node_summary(actual))
        }
        DiffEvent::Substituted {
            expected, actual, ..
        } => format!(
            "wrong-node:substituted {} with {}",
            node_summary(expected),
            node_summary(actual)
        ),
    };
    let extra = if diff.events.len() == 1 {
        String::new()
    } else {
        format!(
            " (first of {} differences; expected tree has {} lines, actual has {})",
            diff.events.len(),
            expected.len(),
            actual.len()
        )
    };
    (
        shape,
        format!(
            "the parsed tree diverges from the suite's expected tree{extra}; \
             see the first difference reported for this case"
        ),
    )
}

/// A short, stable description of a dumped line, used to name a mechanism.
///
/// The payload matters: a bare `comment` would merge every comment whose data
/// differs into one group, and that group is a symptom, not a mechanism.
fn node_summary(line: &str) -> String {
    let trimmed = line.trim_start();
    let Some(rest) = trimmed.strip_prefix("| ") else {
        return "malformed-line".to_owned();
    };
    let rest = rest.trim_start();
    if rest.starts_with('<') {
        if let Some(data) = rest.strip_prefix("<!-- ") {
            return format!("comment {}", clipped(data.trim_end_matches(" -->")));
        }
        if let Some(name) = rest.strip_prefix("<!DOCTYPE ") {
            return format!("doctype {}", clipped(name.trim_end_matches('>')));
        }
        if rest.starts_with("<?") {
            return "processing-instruction".to_owned();
        }
        if rest == "content" {
            return "template-content".to_owned();
        }
        let name = rest
            .trim_start_matches('<')
            .split(['>', ' '])
            .next()
            .unwrap_or("element");
        return format!("element {name}");
    }
    if let Some(data) = rest.strip_prefix('"') {
        return format!("text {}", clipped(data.trim_end_matches('"')));
    }
    format!("attribute {}", clipped(rest))
}

fn clipped(value: &str) -> String {
    const LIMIT: usize = 36;
    let mut out = value.chars().take(LIMIT).collect::<String>();
    if value.chars().nth(LIMIT).is_some() {
        out.push('…');
    }
    out.replace(['\n', '\r'], "\\n")
}

// ---------------------------------------------------------------------------
// Entry points kept reachable from the example
// ---------------------------------------------------------------------------

/// The fragment serialisation of the body is reported alongside the tree when
/// the suite supplies one, which is the only way a `#document-fragment` test's
/// serialisation could ever be checked; whole-document tests do not get one.
#[allow(dead_code)]
fn body_serialization(dom: &Dom, scripting_enabled: bool) -> Option<String> {
    dom.children(dom.document())
        .unwrap_or_default()
        .iter()
        .find_map(|node| body_element(dom, *node))
        .map(|body| serialize_html_fragment_with_scripting(dom, body, scripting_enabled))
}

#[allow(dead_code)]
fn _assert_path_is_used(path: &Path) -> bool {
    path.is_dir()
}
