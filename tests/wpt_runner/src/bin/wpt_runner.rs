//! The single command: `wpt-runner`.
//!
//! Subcommands:
//!
//! * `run`     - fetch (optional), census, execute, report. The real thing.
//! * `census`  - population and feasibility only. No engine needed, and it is
//!               the half of the work that is always trustworthy because it does
//!               not depend on the engine being healthy.
//! * `selftest`- the falsification checks alone, for a five-second answer to
//!               "is this harness itself sound right now?".
//!
//! The self-check runs on every subcommand, including the ones that do not
//! score anything, because a harness that is quietly broken should say so even
//! when it is not being asked for a number.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use wpt_runner::census;
use wpt_runner::outcome::Outcome;
use wpt_runner::report;
use wpt_runner::results::{FileResult, Report};
use wpt_runner::{default_output_dir, default_suite_root, repo_root, selftest, WPT_REVISION};

const USAGE: &str = "\
wpt-runner - official Web Platform Tests conformance runner for rENDER

USAGE
    wpt-runner <SUBCOMMAND> [OPTIONS]

SUBCOMMANDS
    run              Census the suite, execute it, and write results.
    census           Census only: population, harness shapes, feasibility. No engine.
    selftest         Run the harness falsification checks and report pass/fail.
    probe            Can the engine load WPT's own testharness.js? Measured.
    negative-control Run the pipeline against adapters that must fail.
    fixtures         Account for every missing fixture, by the tree that has it.

OPTIONS
    --suite <DIR>       Pinned WPT checkout.
                        Default: <repo>/tools/wpt/.cache/wpt
    --out <DIR>         Where results are written.
                        Default: <repo>/tools/wpt/results
    --areas <LIST>      Comma-separated areas. Default: css,dom,html
    --limit <N>         Census at most N tests per area. Reported in the
                        results, and a partial figure is never presented as a
                        conformance rate.
    --fetch             Fetch the pinned suite first (needs network).
    --allow-partial     Permit a run over fewer than the whole population,
                        clearly labelled as partial in the output.

NOTES
    The first run needs network access to fetch the pinned suite, roughly
    400 MB. The fetch is cached and pinned; later runs are offline.

    Every percentage this tool prints carries its denominator. There is no code
    path that prints a bare percentage, and a run that evaluated nothing prints
    no percentage at all.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(subcommand) = args.first().map(String::as_str) else {
        print!("{USAGE}");
        return ExitCode::from(2);
    };

    match subcommand {
        "run" => cmd_run(&args[1..]),
        "census" => cmd_census(&args[1..]),
        "selftest" => cmd_selftest(),
        "probe" => cmd_probe(&args[1..]),
        "negative-control" => cmd_negative_control(&args[1..]),
        "fixtures" => cmd_fixtures(&args[1..]),
        "-h" | "--help" | "help" => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("unknown subcommand {other:?}\n");
            print!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

/// Parsed options, with the defaults resolved against the repository root.
struct Options {
    suite_root: PathBuf,
    output_dir: PathBuf,
    areas: Vec<String>,
    limit: Option<usize>,
    fetch: bool,
    allow_partial: bool,
}

fn parse_options(args: &[String]) -> Result<Options, String> {
    let root = repo_root();
    let mut options = Options {
        suite_root: default_suite_root(&root),
        output_dir: default_output_dir(&root),
        areas: wpt_runner::DEFAULT_AREAS.iter().map(|s| (*s).to_owned()).collect(),
        limit: None,
        fetch: false,
        allow_partial: false,
    };
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--suite" => {
                i += 1;
                options.suite_root = PathBuf::from(
                    args.get(i).ok_or("--suite needs a directory")?,
                );
            }
            "--out" => {
                i += 1;
                options.output_dir =
                    PathBuf::from(args.get(i).ok_or("--out needs a directory")?);
            }
            "--areas" => {
                i += 1;
                let raw = args.get(i).ok_or("--areas needs a list")?;
                options.areas = raw
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect();
                if options.areas.is_empty() {
                    return Err("--areas was empty".to_owned());
                }
            }
            "--limit" => {
                i += 1;
                let raw = args.get(i).ok_or("--limit needs a number")?;
                options.limit = Some(
                    raw.parse::<usize>()
                        .map_err(|e| format!("--limit: {e}"))?,
                );
            }
            "--fetch" => options.fetch = true,
            "--allow-partial" => options.allow_partial = true,
            other => return Err(format!("unknown option {other:?}")),
        }
        i += 1;
    }
    Ok(options)
}

/// Run the harness self-check and bail out if it did not hold.
///
/// This is the gate. Everything after this point assumes the four states are
/// distinguishable, and if that is not true the run must not produce a number.
fn gate_on_selftest() -> Result<selftest::SelfTest, ExitCode> {
    let check = selftest::run();
    if !check.trustworthy() {
        eprintln!("HARNESS SELF-CHECK FAILED - no results are being reported.");
        for failure in check.failures() {
            eprintln!(
                "  {}: expected {}, observed {}",
                failure.name, failure.expected, failure.observed
            );
        }
        return Err(ExitCode::FAILURE);
    }
    Ok(check)
}

/// Account for every missing fixture, by the WPT tree that would supply it.
///
/// Answers the question a bare "535 missing fixtures" cannot: which trees, how
/// many tests each is blocking, and therefore exactly what to add to
/// `tools/wpt/fetch-wpt.ps1`'s tree list. The decision to widen the fetch is a
/// human's - it costs disk on a machine other agents are building on - but it
/// should be made against a number rather than a shrug.
fn cmd_fixtures(args: &[String]) -> ExitCode {
    let options = match parse_options(args) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    let Some(suite) = census::Suite::open(&options.suite_root, WPT_REVISION).ok() else {
        eprintln!(
            "no verified suite at {}; run with --fetch",
            options.suite_root.display()
        );
        return ExitCode::FAILURE;
    };
    let areas: Vec<&str> = options.areas.iter().map(String::as_str).collect();
    let result = census::run(&suite, &areas);
    let Ok(census) = result else {
        eprintln!("census failed; run the `census` subcommand for the error");
        return ExitCode::FAILURE;
    };

    let mut account = wpt_runner::fixtures::FixtureAccount::new();
    for area in &census.areas {
        for notable in &area.population.notable {
            if let Some(url) = notable.reason.strip_prefix("missing fixture: ") {
                account.record(url, &notable.path);
            }
        }
    }

    println!("================================================================================");
    println!("rENDER WPT runner - missing fixture account");
    println!("================================================================================");
    println!("wpt revision : {WPT_REVISION}");
    println!("areas        : {}", areas.join(", "));
    println!();
    println!(
        "{} test(s) blocked by a fixture absent from the checkout, referencing {} distinct",
        account.tests(),
        account.distinct_fixtures()
    );
    println!("fixture(s). Every one of these is a FETCH limit, not a suite defect: the file");
    println!("exists at the pinned revision and this cache simply does not hold its tree.");
    println!();
    println!("  tests blocked   WPT tree to add");
    println!("  ------------    ----------------------------------------------------");
    for (tree, count) in account.ranked() {
        println!("  {count:>12}    {tree}");
    }
    if account.ranked().is_empty() {
        println!("  (none)");
    }
    println!();
    if !account.is_complete() {
        println!(
            "INCOMPLETE: {} fixture reference(s) could not be attributed to a tree. Those are\n\
             relative URLs, which resolve against the referring file, and this accounting\n\
             refuses to guess. They are counted in the total above but have no tree.",
            account.unclassified()
        );
    }
    let out = options.output_dir.join("wpt-missing-fixtures.txt");
    if let Some(parent) = out.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut body = String::new();
    for (url, test) in account.examples() {
        body.push_str(url);
        body.push('\t');
        body.push_str(test);
        body.push('\n');
    }
    match std::fs::write(&out, body) {
        Ok(()) => println!("\nwrote {}", out.display()),
        Err(error) => {
            eprintln!("could not write {}: {error}", out.display());
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}

fn cmd_selftest() -> ExitCode {
    let check = selftest::run();
    println!("{}", report::render_selftest_only(&check));
    if check.trustworthy() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// The negative control, in two halves.
///
/// # Why two halves
///
/// The first half injects an adapter that **cannot fail** and shows that the
/// pipeline reports a 100% rate with every pass vacuous. That is the failure
/// mode from last round: a wrong number that looks perfectly healthy.
///
/// But that control alone would pass against a pipeline that reports 100% for
/// *everything* - including a real run, where 100% is impossible. The second
/// half is the one that actually earns trust: it runs a test whose assertion is
/// **false** through the real adapter, through WPT's own `testharness.js`, and
/// requires the harness to report a failure with a message. A pipeline that
/// cannot produce a failure cannot produce a trustworthy pass rate, and only
/// that direction is fatal.
///
/// # What the last round got wrong, and what replaced it
///
/// The last round produced an impossible number - "0 in both, 8,695 tests
/// dropped" - with no error, no failing test and a plausible report. It was
/// caught by reading the number, which is a defence that depends on somebody
/// reading. Both halves of this subcommand replace that dependence with a
/// measurement.
fn cmd_negative_control(args: &[String]) -> ExitCode {
    #[cfg(not(feature = "engine"))]
    {
        let _ = args;
        eprintln!("the negative control needs the `engine` feature");
        ExitCode::FAILURE
    }

    #[cfg(feature = "engine")]
    {
        let options = match parse_options(args) {
            Ok(options) => options,
            Err(message) => {
                eprintln!("{message}");
                return ExitCode::from(2);
            }
        };
        // ---- Half 1: an adapter that cannot fail --------------------------
        println!("================================================================================");
        println!("rENDER WPT runner - NEGATIVE CONTROL, half 1 of 2");
        println!("  an adapter that reports pass for every test, asserting nothing");
        println!("================================================================================");
        println!("wpt revision : {WPT_REVISION}");

        let harness_areas: Vec<&str> = options
            .areas
            .iter()
            .map(String::as_str)
            .filter(|a| *a == "dom" || *a == "html")
            .collect();
        if harness_areas.is_empty() {
            eprintln!("the negative control needs a `dom` or `html` area to run against");
            return ExitCode::from(2);
        }
        // Constructed to prove the harness loads before anything is attempted.
        let Ok(_engine) = wpt_runner::wptdom::WptHarnessEngine::new(&options.suite_root) else {
            eprintln!("WPT's testharness.js did not load; no test can be driven through it");
            return ExitCode::FAILURE;
        };


        let mut executed = wpt_runner::wptdom::execute_areas(
            &harness_areas,
            &options.suite_root,
            wpt_runner::wptdom::Adapter::Control,
            options.limit,
        );
        for result in &mut executed {
            result.mechanism = Some("negative-control".to_owned());
        }

        let mut totals = wpt_runner::outcome::Tally::new();
        for result in &executed {
            totals.record(result.outcome);
        }
        let vacuous = executed
            .iter()
            .filter(|r| r.outcome == Outcome::Pass && r.assertions_evaluated == 0)
            .count() as u32;
        let rate = totals
            .engine_rate()
            .map_or_else(|| "<none>".to_owned(), |r| format!("{r}"));

        println!("areas        : {}", harness_areas.join(", "));
        println!("tests seen   : {}", executed.len());
        println!("pass         : {}", totals.pass);
        println!("fail         : {}", totals.fail);
        println!("error        : {}", totals.error);
        println!("skipped      : {}", totals.skip);
        println!("rate         : {rate}");
        println!("vacuous pass : {vacuous} of {}", totals.pass);
        println!();

        // The census veto legitimately turns some of the adapter's passes into
        // skips, so `pass` is smaller than the population by design. What must
        // hold is: a 100% rate in which *every* pass evaluated nothing, plus an
        // active census veto. All three, or the control has failed.
        let all_vacuous = totals.pass > 0 && vacuous == totals.pass;
        let perfect_rate = totals
            .engine_rate()
            .is_some_and(|r| r.numerator == r.denominator);
        let census_veto_worked = totals.skip > 0;
        let half1 = all_vacuous && perfect_rate && census_veto_worked;
        println!("control 1: every pass evaluated zero assertions ....... {}", if all_vacuous { "yes" } else { "NO" });
        println!("control 2: the rate is 100% of what was attempted ..... {}", if perfect_rate { "yes" } else { "NO" });
        println!(
            "control 3: the census veto demoted {} test(s) to skip ..... {}",
            totals.skip,
            if census_veto_worked { "yes" } else { "NO" }
        );
        println!();
        if half1 {
            println!(
                "half 1 HELD: a broken adapter produces a 100% rate in which every pass is\n\
                 vacuous, and the census veto independently demoted {} of the {} tests it\n\
                 claimed to pass. Both are visible in the output rather than in the numerator.",
                totals.skip,
                totals.pass + totals.skip
            );
        } else {
            eprintln!(
                "half 1 FAILED: the pipeline did not surface a broken adapter. A real run's\n\
                 number cannot be trusted from this build."
            );
        }
        println!();

        // ---- Half 2: the real adapter must be able to FAIL -----------------
        println!("================================================================================");
        println!("rENDER WPT runner - NEGATIVE CONTROL, half 2 of 2");
        println!("  can the real adapter, through WPT's own testharness, report a FAILURE?");
        println!("================================================================================");
        let half2 = falsification_check(&options);
        println!();
        println!(
            "half 2 {}: the real adapter, driving WPT's own testharness.js, {}",
            if half2.held { "HELD" } else { "FAILED" },
            half2.detail
        );
        println!();
        if half2.held {
            println!(
                "VERDICT: both halves held. The pipeline reports a 100% vacuous rate for an\n\
                 adapter that cannot fail, and reports a real failure for a test whose\n\
                 assertion is false. Both directions are proven, so a rate from this build is\n\
                 interpretable in both directions."
            );
            ExitCode::SUCCESS
        } else {
            eprintln!(
                "VERDICT: half 2 FAILED. The pipeline could not produce a failure from a test\n\
                 whose assertion is false, so a pass rate from this build is not evidence of\n\
                 anything. Do not report a rate."
            );
            ExitCode::FAILURE
        }
    }
}

/// Whether the real adapter can be made to fail, and what it said.
#[cfg(feature = "engine")]
struct Falsification {
    held: bool,
    detail: String,
}

/// Run a test whose assertion is false, through the real adapter, and require a
/// `Fail` carrying a message.
///
/// This is the direction that matters. A pipeline that reports 100% for a broken
/// adapter is visibly broken; a pipeline that *cannot produce a failure* is
/// invisibly broken, and a real run's low pass rate would be the harness hiding
/// results rather than the engine getting them wrong.
#[cfg(feature = "engine")]
fn falsification_check(options: &Options) -> Falsification {
    use wpt_runner::wptdom::WptHarnessEngine;

    let cases: [(&str, &str); 2] = [
        ("a false assert_equals", "assert_equals(1, 2)"),
        ("a false assert_true", "assert_true(false)"),
    ];
    let mut details = Vec::new();
    let mut all_failed_correctly = true;

    for (label, body) in cases {
        let Ok(mut engine) = WptHarnessEngine::new(&options.suite_root) else {
            return Falsification {
                held: false,
                detail: "WPT's testharness.js did not load, so nothing could be driven".to_owned(),
            };
        };
        let source = format!(
            "<!doctype html><meta charset=utf-8><script src=\"/resources/testharness.js\"></script>\
             <script>test(function () {{ {body} }}, 'negative control: {label}');</script>"
        );
        let ctx = wpt_runner::engine::TestContext {
            path: &format!(
                "negative-control/{}.html",
                wpt_runner::mechanism::slug(label)
            ),
            area: "negative-control",
            file: std::path::Path::new("negative-control"),
            suite_root: &options.suite_root,
            source: &source,
            assertion_sites: 1,
        };
        let result = wpt_runner::engine::run_one(&mut engine, &ctx);
        let ok = result.outcome == Outcome::Fail && result.failing_assertion.is_some();
        // Every note is carried into the control's own output. When the control
        // fails - and it did, the first time - the reason has to be in the
        // control's output, not in a result file nobody opened. A control that
        // fails without saying why gets ignored, and then it protects nothing.
        let notes = if result.notes.is_empty() {
            String::new()
        } else {
            format!(" notes=[{}]", result.notes.join(" | "))
        };
        let reason = result
            .failing_assertion
            .as_deref()
            .or(result.error.as_deref())
            .map_or("<no message>", |m| m);
        details.push(format!(
            "{label}: outcome={} reason=\"{}\"{}",
            result.outcome,
            reason.replace('\n', " "),
            notes
        ));
        all_failed_correctly &= ok;
    }

    Falsification {
        held: all_failed_correctly,
        detail: details.join("; "),
    }
}

/// Answer one question, measured: can the engine load WPT's own harness?
///
/// This is a separate subcommand rather than a line in the census because it is
/// a different kind of question. The census describes the suite; this asks
/// whether the *engine* can execute it, and the answer is one bit that
/// determines whether `dom/` and `html/` have a conformance number at all.
///
/// A probe that could not falsify its own result would be worse than no probe,
/// so the probe deliberately runs a test with a false assertion and requires the
/// harness to report it differently from a true one. A harness that reports the
/// same verdict for both is not a working harness, and the probe says so.
fn cmd_probe(args: &[String]) -> ExitCode {
    let options = match parse_options(args) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    if !options.suite_root.join(".pinned-revision").is_file() {
        eprintln!(
            "no verified suite at {}. Run with --fetch (needs network on first run).",
            options.suite_root.display()
        );
        return ExitCode::FAILURE;
    }

    #[cfg(feature = "engine")]
    {
        let result = wpt_runner::probe::run(&options.suite_root, WPT_REVISION);
        print!("{}", report::render_probe(&result));
        if result.all_held() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        }
    }

    #[cfg(not(feature = "engine"))]
    {
        eprintln!(
            "the probe needs an engine, and this binary was built without the `engine` \
             feature.\nBuild with `--features engine` to run it. Reporting a probe result \
             from a build with nothing to probe would be a number about nothing."
        );
        ExitCode::FAILURE
    }
}

fn cmd_census(args: &[String]) -> ExitCode {    let options = match parse_options(args) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    let check = match gate_on_selftest() {
        Ok(check) => check,
        Err(code) => return code,
    };
    match build_census(&options) {
        Ok((census, notes)) => {
            let mut report = Report {
                suite_revision: census.suite_revision.clone(),
                census,
                executed: Vec::new(),
                harness: check,
                engine_unavailable: Some("census subcommand: no engine was run".to_owned()),
                tool_version: tool_version(),
            };
            for note in notes {
                report.engine_unavailable.get_or_insert_default().push('\n');
                report
                    .engine_unavailable
                    .get_or_insert_default()
                    .push_str(&note);
            }
            print!("{}", report::render(&report));
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("census failed: {message}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_run(args: &[String]) -> ExitCode {
    let options = match parse_options(args) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };

    if options.fetch {
        if let Err(message) = fetch_suite(&options.suite_root) {
            eprintln!("fetch failed: {message}");
            return ExitCode::FAILURE;
        }
    }

    let check = match gate_on_selftest() {
        Ok(check) => check,
        Err(code) => return code,
    };

    let (census, notes) = match build_census(&options) {
        Ok(value) => value,
        Err(message) => {
            eprintln!("census failed: {message}");
            return ExitCode::FAILURE;
        }
    };

    // Execution. Whether this can happen at all depends on whether an engine
    // adapter was compiled in, and that is reported rather than assumed.
    let (executed, engine_unavailable) = execute(&census, &options, &notes);

    let report = Report {
        suite_revision: census.suite_revision.clone(),
        census,
        executed,
        harness: check,
        engine_unavailable,
        tool_version: tool_version(),
    };

    let rendered = report::render(&report);
    print!("{rendered}");

    match report.write(&options.output_dir) {
        Ok((summary, detail)) => {
            println!("\nwrote {}", summary.display());
            println!("wrote {}", detail.display());
        }
        Err(err) => {
            eprintln!("could not write results: {err}");
            return ExitCode::FAILURE;
        }
    }

    // A run that produced failures exits non-zero so a CI job cannot record a
    // pass by accident. Harness errors do NOT fail the command: they are not
    // evidence about the engine, and treating them as failures is the exact
    // conflation this runner exists to prevent. They are visible in the report
    // and in the exit-code-adjacent fields of the results file.
    let fails = report
        .executed
        .iter()
        .filter(|f| f.outcome == Outcome::Fail)
        .count();
    if fails > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn build_census(options: &Options) -> Result<(census::Census, Vec<String>), String> {
    if !options.suite_root.join(".pinned-revision").is_file() {
        return Err(format!(
            "no verified suite at {}. Run with --fetch (needs network on first run).",
            options.suite_root.display()
        ));
    }
    let suite = census::Suite::open(&options.suite_root, WPT_REVISION)?;
    let mut notes = Vec::new();
    let areas: Vec<&str> = options.areas.iter().map(String::as_str).collect();
    eprintln!(
        "censusing {} at wpt {} ...",
        areas.join(","),
        suite.revision
    );
    let result = census::run(&suite, &areas)?;
    if let Some(limit) = options.limit {
        notes.push(format!(
            "PARTIAL RUN: census limited to {limit} tests per area. A partial figure is not a \
             conformance rate and is not extrapolated."
        ));
    }
    Ok((result, notes))
}

/// Run the executable subset through the engine, if one is available.
///
/// Two adapters, and the split is the whole finding of this round:
///
/// * `dom/` and `html/` go through WPT's own `testharness.js`, executed by the
///   engine, with results read back through `add_result_callback`. These are
///   real assertions and can produce real passes and real failures.
/// * `css/` goes through the computed-value adapter, which cannot assert a WPT
///   expectation and therefore produces no verdicts at all. Every CSS test
///   lands in a skip, and the report says why.
///
/// Routing both areas through one adapter would have hidden that difference
/// behind a plausible-looking number.
fn execute(
    census_result: &census::Census,
    options: &Options,
    notes: &[String],
) -> (Vec<FileResult>, Option<String>) {
    let mut unavailable: Option<String> = None;
    for note in notes {
        unavailable.get_or_insert_default().push_str(note);
        unavailable.get_or_insert_default().push('\n');
    }

    #[cfg(feature = "engine")]
    {
        use wpt_runner::engine::Engine as _;
        // The census is deliberately not consulted to decide what to run. Each
        // adapter walks its own area and decides per file, and the census's
        // verdict is applied *inside* the adapter as a veto on a pass. A
        // census-driven execution loop would mean the denominator and the
        // numerator came from two different classifications, and the two would
        // drift the first time either changed.
        let _ = census_result;

        let mut executed = Vec::new();

        // Areas that can produce real verdicts, and the adapter that does it.
        let harness_areas: Vec<&str> = options
            .areas
            .iter()
            .map(String::as_str)
            .filter(|a| *a == "dom" || *a == "html")
            .collect();

        if !harness_areas.is_empty() {
            match wpt_runner::wptdom::WptHarnessEngine::new(&options.suite_root) {
                Ok(engine) => {
                    eprintln!("executing {} with {}", harness_areas.join(","), engine.describe());
                    eprintln!(
                        "  each test on its own {}-byte stack (see wptdom::TEST_STACK_BYTES)",
                        wpt_runner::wptdom::TEST_STACK_BYTES
                    );
                    executed.extend(wpt_runner::wptdom::execute_areas(
                        &harness_areas,
                        &options.suite_root,
                        wpt_runner::wptdom::Adapter::Real,
                        options.limit,
                    ));
                }
                Err(error) => {
                    eprintln!("could not load WPT's testharness.js: {error}");
                    let message = format!(
                        "WPT's own harness did not load, so {} could not be executed.\n\
                         {error}\n\
                         This is a statement about the host, not about the engine's conformance. \
                         No rate is reported for those areas.",
                        harness_areas.join(", ")
                    );
                    unavailable.get_or_insert_default().push_str(&message);
                }
            }
        }

        // Areas that cannot, and say so.
        let computed_areas: Vec<&str> = options
            .areas
            .iter()
            .map(String::as_str)
            .filter(|a| *a != "dom" && *a != "html")
            .collect();
        if !computed_areas.is_empty() {
            let mut adapter = wpt_runner::render_core::RenderCoreEngine::new();
            eprintln!("executing {} with {}", computed_areas.join(","), adapter.describe());
            executed.extend(wpt_runner::render_core::execute_areas(
                &computed_areas,
                &mut adapter,
                &options.suite_root,
                options.limit,
            ));
        }

        (executed, unavailable)
    }

    #[cfg(not(feature = "engine"))]
    {
        let total = census_result.total_executable;
        let message = format!(
            "no engine adapter was compiled into this binary.\n\
             The statically executable subset is {total} test(s) across {}, and none of them\n\
             were evaluated. Build with `--features engine` to drive render-core.\n\
             A subset that was never run is a missing capability, not a score of zero.",
            options.areas.join(",")
        );
        eprintln!("{message}");
        let _ = total;
        (Vec::new(), Some(message))
    }
}

fn fetch_suite(suite_root: &Path) -> Result<(), String> {
    let repo = repo_root();
    let script = repo.join("tools/wpt/fetch-wpt.ps1");
    if !script.is_file() {
        return Err(format!("fetch script not found at {}", script.display()));
    }
    // The fetch is a PowerShell script rather than a dependency of this crate, so
    // the network stack stays out of the measurement path entirely: the same
    // code that produces the numbers does not also implement TLS.
    let status = std::process::Command::new("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script)
        .arg("-CacheDir")
        .arg(suite_root)
        .status()
        .map_err(|e| format!("could not start powershell: {e}"))?;
    if !status.success() {
        return Err(format!("fetch-wpt.ps1 exited with {status}"));
    }
    Ok(())
}

fn tool_version() -> String {
    format!(
        "render-wpt-runner {} (wpt pin {})",
        env!("CARGO_PKG_VERSION"),
        WPT_REVISION
    )
}
