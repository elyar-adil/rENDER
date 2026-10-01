//! Turn a [`Report`](crate::results::Report) into something a person can read.
//!
//! The report's job is to make three things impossible to misread:
//!
//! * a rate without its denominator;
//! * an area's *absent* score being read as a score of zero;
//! * a `fail` being read as an engine defect without the reader knowing whether
//!   it was an engine defect, a missing capability, or a harness limit.
//!
//! That last one is why every number is followed by the bucket breakdown rather
//! than placed above it. A reader who only wants the headline still cannot miss
//! what is underneath it.

use std::fmt::Write as _;

use crate::census::{population_statement, reference_rule_statement, AreaCensus, Census};
use crate::outcome::Tally;
use crate::results::Report;

const RULE: &str = "================================================================================";

/// Render just the self-check, for `wpt-runner selftest`.
///
/// Usable without a suite checkout, a census or an engine, because the question
/// it answers - "is this harness itself sound right now?" - must be answerable
/// even when nothing else works.
#[must_use]
pub fn render_selftest_only(check: &crate::selftest::SelfTest) -> String {
    let mut out = String::new();
    writeln!(out, "{RULE}").expect("write to String cannot fail");
    writeln!(out, "rENDER WPT runner - harness self-check").expect("write to String cannot fail");
    writeln!(out, "{RULE}").expect("write to String cannot fail");
    writeln!(
        out,
        "{} of {} checks held",
        check.passed,
        check.passed + check.failed
    )
    .expect("write to String cannot fail");
    writeln!(
        out,
        "harness trustworthy: {}",
        if check.trustworthy() { "yes" } else { "NO" }
    )
    .expect("write to String cannot fail");
    for entry in &check.checks {
        writeln!(
            out,
            "\n  [{}] {}\n      expected : {}\n      observed : {}\n      matters  : {}",
            if entry.held { "ok" } else { "FAIL" },
            entry.name,
            entry.expected,
            entry.observed,
            entry.why_it_matters
        )
        .expect("write to String cannot fail");
    }
    out
}

/// Render the full reading.
#[must_use]
pub fn render(report: &Report) -> String {
    let mut out = String::new();
    writeln!(out, "{RULE}").expect("write to String cannot fail");
    writeln!(out, "rENDER WPT conformance report").expect("write to String cannot fail");
    writeln!(out, "{RULE}").expect("write to String cannot fail");
    writeln!(
        out,
        "wpt revision : {}",
        if report.suite_revision.is_empty() {
            "<none recorded - this is not a measurement>"
        } else {
            &report.suite_revision
        }
    )
    .expect("write to String cannot fail");
    writeln!(out, "runner       : {}", report.tool_version).expect("write to String cannot fail");

    render_selftest(&mut out, report);
    render_engine_status(&mut out, report);
    render_population(&mut out, &report.census);

    let mut totals = Tally::new();
    let mut per_area: Vec<(&str, Tally)> = Vec::new();
    for area in &report.census.areas {
        let mut tally = Tally::new();
        for file in report.executed.iter().filter(|f| f.area == area.area) {
            tally.record(file.outcome);
        }
        per_area.push((area.area.as_str(), tally));
        totals.merge(&tally);
    }

    render_executed(&mut out, report, &totals, &per_area);
    render_mechanisms(&mut out, report);
    render_defects(&mut out, report);
    out
}

fn render_selftest(out: &mut String, report: &Report) {
    writeln!(out, "\n-- harness self-check -------------------------------------------------")
        .expect("write to String cannot fail");
    writeln!(
        out,
        "{} of {} checks held; harness trustworthy: {}",
        report.harness.passed,
        report.harness.passed + report.harness.failed,
        if report.harness.trustworthy() { "yes" } else { "NO" }
    )
    .expect("write to String cannot fail");
    for check in &report.harness.checks {
        writeln!(
            out,
            "  [{}] {}\n        expected {}\n        observed {}",
            if check.held { "ok" } else { "FAIL" },
            check.name,
            check.expected,
            check.observed
        )
        .expect("write to String cannot fail");
    }
    if !report.harness.trustworthy() {
        writeln!(
            out,
            "\n*** THE HARNESS IS NOT TRUSTWORTHY. Every rate below is withheld. ***"
        )
        .expect("write to String cannot fail");
    }
}

fn render_engine_status(out: &mut String, report: &Report) {
    writeln!(out, "\n-- engine status ------------------------------------------------------")
        .expect("write to String cannot fail");
    match &report.engine_unavailable {
        Some(reason) => {
            writeln!(out, "The engine could not be driven:").expect("write to String cannot fail");
            for line in reason.lines() {
                writeln!(out, "  {line}").expect("write to String cannot fail");
            }
            writeln!(
                out,
                "\nNo conformance percentage is reported. An engine that did not run is not\n\
                 an engine that scored zero, and reporting a rate here would be a claim\n\
                 about the engine that the run has no evidence for."
            )
            .expect("write to String cannot fail");
        }
        None => {
            #[cfg(feature = "engine")]
            writeln!(out, "engine adapter: {}", crate::render_core::ENGINE_DESCRIPTION)
                .expect("write to String cannot fail");
            #[cfg(not(feature = "engine"))]
            writeln!(
                out,
                "no engine adapter compiled in (build with `--features engine`)"
            )
            .expect("write to String cannot fail");
        }
    }
}

fn render_population(out: &mut String, census: &Census) {
    writeln!(
        out,
        "\n-- population (the denominator) -----------------------------------------"
    )
    .expect("write to String cannot fail");
    writeln!(
        out,
        "wpt revision {}: {} .html files present across the requested areas",
        census.suite_revision,
        census.total_files_present
    )
    .expect("write to String cannot fail");

    for area in &census.areas {
        render_area_population(out, area);
    }

    writeln!(
        out,
        "\nTOTAL scored test population: {}",
        census.total_tests
    )
    .expect("write to String cannot fail");
    writeln!(
        out,
        "  of which statically executable: {} ({:.1}% of the {} scored tests; a static-analysis\n\
         \x20 figure, not an engine result)",
        census.total_executable,
        ratio(census.total_executable, census.total_tests),
        census.total_tests
    )
    .expect("write to String cannot fail");
    writeln!(
        out,
        "  of which cannot fail (no assertion site): {} ({:.1}% of the {} scored tests)",
        census.total_cannot_fail,
        ratio(census.total_cannot_fail, census.total_tests),
        census.total_tests
    )
    .expect("write to String cannot fail");
}

fn render_area_population(out: &mut String, area: &AreaCensus) {
    writeln!(out, "\n{}:", area.area).expect("write to String cannot fail");
    writeln!(out, "  {}", population_statement(area)).expect("write to String cannot fail");
    if let Some(statement) = reference_rule_statement(area) {
        writeln!(out, "  {statement}").expect("write to String cannot fail");
    }
    for (reason, count) in &area.population.excluded {
        writeln!(out, "    excluded {:<26} {count:>6}", reason.label())
            .expect("write to String cannot fail");
    }
    if !area.population.notable.is_empty() {
        writeln!(
            out,
            "    notable ({} shown, these need a human):",
            area.population.notable.len()
        )
        .expect("write to String cannot fail");
        for notable in area.population.notable.iter().take(10) {
            writeln!(out, "      {}: {}", notable.path, notable.reason)
                .expect("write to String cannot fail");
        }
    }
    writeln!(
        out,
        "  harness shapes: {}",
        if area.shapes.is_empty() {
            "none detected".to_owned()
        } else {
            area.shapes
                .iter()
                .map(|(shape, n)| format!("{}={n}", shape.label()))
                .collect::<Vec<_>>()
                .join(" ")
        }
    )
    .expect("write to String cannot fail");
    writeln!(
        out,
        "  capabilities required: {}",
        if area.capabilities.is_empty() {
            "none detected".to_owned()
        } else {
            area.capabilities
                .iter()
                .map(|(cap, n)| format!("{}={n}", cap.label()))
                .collect::<Vec<_>>()
                .join(" ")
        }
    )
    .expect("write to String cannot fail");
    writeln!(
        out,
        "  blockers: {}",
        if area.blockers.is_empty() {
            "none".to_owned()
        } else {
            area.blockers
                .iter()
                .map(|(key, n)| format!("{}={n}", key.describe()))
                .collect::<Vec<_>>()
                .join(" | ")
        }
    )
    .expect("write to String cannot fail");
}

fn render_executed(
    out: &mut String,
    report: &Report,
    totals: &Tally,
    per_area: &[(&str, Tally)],
) {
    writeln!(
        out,
        "\n-- executed results (four states) ---------------------------------------"
    )
    .expect("write to String cannot fail");

    if totals.total() == 0 {
        writeln!(
            out,
            "No test produced a verdict. There is no rate to report, and none is reported."
        )
        .expect("write to String cannot fail");
    } else {
        writeln!(
            out,
            "pass {}   fail {}   error {}   skipped {}   (total seen {})",
            totals.pass,
            totals.fail,
            totals.error,
            totals.skip,
            totals.total()
        )
        .expect("write to String cannot fail");
        match totals.engine_rate() {
            Some(rate) => writeln!(
                out,
                "conformance: {rate}   <-- denominator is pass+fail only; {} harness errors and \
                 {} skips are excluded",
                totals.error, totals.skip
            )
            .expect("write to String cannot fail"),
            None => writeln!(
                out,
                "conformance: not computed - no test produced engine evidence"
            )
            .expect("write to String cannot fail"),
        }
    }

    for (area, tally) in per_area {
        // Every state prints a number, always. A tally that is entirely skips
        // rendered an empty skip column on a real run, and the resulting line
        // read as "nothing here" rather than "thousands of tests were excluded".
        write!(
            out,
            "  {area:<6} pass={:<6} fail={:<6} error={:<6} skipped={:<6} attempted={}",
            tally.pass, tally.fail, tally.error, tally.skip, tally.attempted()
        )
        .expect("write to String cannot fail");
        match tally.engine_rate() {
            Some(rate) => writeln!(out, "  rate={rate}").expect("write to String cannot fail"),
            None => writeln!(out, "  rate=not computed").expect("write to String cannot fail"),
        }
    }

    let vacuous = report.vacuous_passes();
    writeln!(
        out,
        "\nTests that could not fail (reported, not scored):\n  \
         {} declared no assertion site statically, {} passed with zero assertions evaluated",
        report.census.total_cannot_fail, vacuous
    )
    .expect("write to String cannot fail");
    if vacuous > 0 {
        writeln!(
            out,
            "  {} passes are vacuous: the adapter reported pass having evaluated nothing. These\n\
             \x20 are included in `pass` above and are listed here so the pass count is not read\n\
             \x20 as stronger evidence than it is.",
            vacuous
        )
        .expect("write to String cannot fail");
    }
}

/// Render the ranked mechanism table, which is the part of this report anyone
/// will act on.
///
/// Ranked by count, because the reader's next action is "fix the top one" and
/// any other order makes them do the ranking themselves. Every row carries its
/// count, its share of the accounted population, and a one-line cause naming the
/// thing to change - a table of buckets without causes is a census, and this
/// project already has one of those.
fn render_mechanisms(out: &mut String, report: &Report) {
    let table = report.mechanism_table();
    writeln!(
        out,
        "\n-- ranked mechanisms (the work queue) -----------------------------------"
    )
    .expect("write to String cannot fail");
    if table.accounted == 0 {
        writeln!(out, "  no mechanism was attributed to any test")
            .expect("write to String cannot fail");
        return;
    }
    writeln!(
        out,
        "  {} test(s) accounted for across {} distinct mechanisms",
        table.accounted,
        table.ranked().len()
    )
    .expect("write to String cannot fail");
    writeln!(out).expect("write to String cannot fail");

    for (index, mechanism) in table.ranked().iter().enumerate() {
        writeln!(
            out,
            "  {:>2}. {:<46} {:>6} test(s)  {:>5.1}% of accounted",
            index + 1,
            mechanism.key,
            mechanism.tests,
            ratio(mechanism.tests, table.accounted)
        )
        .expect("write to String cannot fail");
        writeln!(out, "      cause     : {}", mechanism.cause)
            .expect("write to String cannot fail");
        writeln!(out, "      category  : {}", mechanism.category.label())
            .expect("write to String cannot fail");
        if !mechanism.areas.is_empty() {
            writeln!(out, "      areas     : {}", mechanism.areas.join(", "))
                .expect("write to String cannot fail");
        }
        if !mechanism.examples.is_empty() {
            writeln!(out, "      examples  : {}", mechanism.examples.join(", "))
                .expect("write to String cannot fail");
        }
    }

    writeln!(out).expect("write to String cannot fail");
    writeln!(out, "  four-category breakdown (never summed into one number):")
        .expect("write to String cannot fail");
    for category in [
        crate::mechanism::Category::EngineDefect,
        crate::mechanism::Category::AcceptableDifference,
        crate::mechanism::Category::UnimplementedFeature,
        crate::mechanism::Category::HarnessLimitation,
    ] {
        let count = table.by_category().get(&category).copied().unwrap_or(0);
        writeln!(
            out,
            "    {:<24} {:>6} test(s)  {:>5.1}% of {} accounted",
            category.label(),
            count,
            ratio(count, table.accounted),
            table.accounted
        )
        .expect("write to String cannot fail");
    }
    writeln!(
        out,
        "\n  Only `engine-defect` is a bug. `unimplemented-feature` is a missing API,\n\
         \x20`harness-limitation` is a gap in this runner, and `acceptable-difference` is a\n\
         \x20documented divergence. A defect list built from any of the other three is a false\n\
         \x20defect report."
    )
    .expect("write to String cannot fail");
}

fn render_defects(out: &mut String, report: &Report) {
    writeln!(
        out,
        "\n-- ranked failures --------------------------------------------------"
    )
    .expect("write to String cannot fail");
    let mut fails: Vec<_> = report
        .executed
        .iter()
        .filter(|f| f.outcome == crate::outcome::Outcome::Fail)
        .collect();
    if fails.is_empty() {
        writeln!(out, "  none recorded").expect("write to String cannot fail");
        return;
    }
    // Rank by how much a reader should care: a failure with a named engine
    // location is an actionable defect, and one without is a harness question.
    fails.sort_by_key(|f| (f.engine_location.is_none(), f.area.clone(), f.path.clone()));
    for file in fails {
        writeln!(out, "  {}", file.path).expect("write to String cannot fail");
        writeln!(
            out,
            "    failing assertion: {}",
            file.failing_assertion.as_deref().unwrap_or("<none recorded>")
        )
        .expect("write to String cannot fail");
        writeln!(
            out,
            "    engine location  : {}",
            file.engine_location.as_deref().unwrap_or(
                "<not available: the engine does not expose the source location of the winning \
                 declaration>"
            )
        )
        .expect("write to String cannot fail");
        for note in &file.notes {
            writeln!(out, "    note: {note}").expect("write to String cannot fail");
        }
    }

    let errors: Vec<_> = report
        .executed
        .iter()
        .filter(|f| f.outcome == crate::outcome::Outcome::Error)
        .collect();
    writeln!(
        out,
        "\n  {} harness errors (not failures; each is a gap in this runner or the environment):",
        errors.len()
    )
    .expect("write to String cannot fail");
    for file in errors.iter().take(20) {
        writeln!(
            out,
            "    {}: {}",
            file.path,
            file.error.as_deref().unwrap_or("<no message>")
        )
        .expect("write to String cannot fail");
    }
    if errors.len() > 20 {
        writeln!(out, "    ... {} more, see the results file", errors.len() - 20)
            .expect("write to String cannot fail");
    }
}

/// Render the engine-capability probe, for `wpt-runner probe`.
///
/// The framing matters as much as the numbers. A reader who sees "the harness
/// loads" and "the harness does not load" needs to know which of those is a
/// statement about the engine and which is a statement about this runner, so
/// the verdict is spelled out rather than left to the exit code.
#[must_use]
pub fn render_probe(probe: &crate::probe::Probe) -> String {
    let mut out = String::new();
    writeln!(out, "{RULE}").expect("write to String cannot fail");
    writeln!(
        out,
        "rENDER WPT runner - can the engine run WPT's own harness?"
    )
    .expect("write to String cannot fail");
    writeln!(out, "{RULE}").expect("write to String cannot fail");
    writeln!(out, "wpt revision : {}", probe.suite_revision).expect("write to String cannot fail");
    writeln!(
        out,
        "held         : {} of {} steps",
        probe.steps.iter().filter(|s| s.held).count(),
        probe.steps.len()
    )
    .expect("write to String cannot fail");
    for step in &probe.steps {
        writeln!(
            out,
            "\n  [{}] {}\n      observed: {}",
            if step.held { "ok" } else { "FAIL" },
            step.name,
            step.detail
        )
        .expect("write to String cannot fail");
    }
    writeln!(out).expect("write to String cannot fail");
    if probe.all_held() {
        writeln!(
            out,
            "VERDICT: the engine can load and drive WPT's own testharness.js. `dom/` and\n\
             \x20`html/` are executable. A conformance number for those areas is a matter of\n\
             \x20the adapter, not of a missing engine capability."
        )
        .expect("write to String cannot fail");
    } else {
        let failures: Vec<&crate::probe::ProbeStep> =
            probe.steps.iter().filter(|s| !s.held).collect();
        writeln!(
            out,
            "VERDICT: {} of {} steps did not hold. The first is \"{}\".",
            failures.len(),
            probe.steps.len(),
            failures[0].name
        )
        .expect("write to String cannot fail");
        writeln!(out).expect("write to String cannot fail");
        for failure in &failures {
            writeln!(out, "  [FAIL] {}", failure.name).expect("write to String cannot fail");
            writeln!(out, "         observed: {}", failure.detail)
                .expect("write to String cannot fail");
        }
        writeln!(out).expect("write to String cannot fail");
        writeln!(
            out,
            "Read the LAST two failures first. They are the ones that decide whether a\n\
             \x20conformance number is obtainable, and they fail here because\n\
             \x20`document.createElementNS` is unimplemented - which stops WPT's harness from\n\
             \x20building its on-page results table. The runner works around it with\n\
             \x20`setup({{output: false}})`, a documented harness property for exactly this case,\n\
             \x20so the conformance run proceeds; see tools/wpt/FINDINGS.md."
        )
        .expect("write to String cannot fail");
    }
    out
}

const fn ratio(numerator: u32, denominator: u32) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 * 100.0 / denominator as f64
    }
}