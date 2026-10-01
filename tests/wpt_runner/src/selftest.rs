//! Prove the harness can tell the four states apart, on every run.
//!
//! The premise of this whole crate is that a subtly wrong conformance number is
//! worse than no number. A harness that cannot demonstrate its own correctness
//! cannot be used to produce one, so this module runs before anything is scored
//! and its result is recorded in the results file alongside the counts.
//!
//! What is checked, and why each one matters:
//!
//! | Check | The failure it catches |
//! | --- | --- |
//! | a real failure is a `fail` | a harness that swallows failures and reports a pass rate of 100% |
//! | a thrown adapter is an `error` | a harness that manufactures failures out of its own bugs |
//! | an unsupported shape is a `skip` | a harness that reports its own gaps as engine defects |
//! | a no-assertion test is not scored | an inflated pass count, which this project has shipped once |
//! | a percentage carries its denominator | a quotable number detached from what it is a fraction of |
//! | a missing engine is not a zero score | a run that silently reports 0% for a reason that has nothing to do with conformance |
//! | a blocked test is never `executable` | a denominator inflated with tests that cannot run |
//!
//! The engine-dependent checks run against a stub engine, deliberately. That is
//! the point: the four-state boundary is this crate's own logic, so it can be
//! exercised with a stub whether or not the real engine builds. If the real
//! engine is unavailable, the boundary is *still* proven, and the run says so
//! rather than quietly reporting nothing.

use crate::classify::{Blocker, Capability, HarnessShape};
use crate::engine::{run_one, Engine, HarnessError, TestContext, Verdict};
use crate::outcome::{Outcome, SkipReason, Tally};

/// One self-check and what it observed.
#[derive(Clone, Debug)]
pub struct Check {
    pub name: String,
    pub expected: String,
    pub observed: String,
    pub held: bool,
    pub why_it_matters: String,
}

/// The self-check outcome, embedded in every results file.
#[derive(Clone, Debug, Default)]
pub struct SelfTest {
    pub passed: u32,
    pub failed: u32,
    pub checks: Vec<Check>,
}

impl SelfTest {
    /// Whether the harness may be trusted to produce a number at all.
    ///
    /// A run with an untrustworthy self-check must not report a conformance
    /// rate, and the runner treats it as a hard failure rather than a warning.
    #[must_use]
    pub fn trustworthy(&self) -> bool {
        self.failed == 0 && !self.checks.is_empty()
    }

    /// Why the self-check failed, for the report.
    #[must_use]
    pub fn failures(&self) -> Vec<&Check> {
        self.checks.iter().filter(|c| !c.held).collect()
    }
}

/// A stub engine that returns a scripted verdict, including a panic.
struct Stub(&'static str);

impl Engine for Stub {
    fn describe(&self) -> String {
        format!("selftest-stub:{}", self.0)
    }
    fn run(&mut self, ctx: &TestContext<'_>) -> Result<Verdict, HarnessError> {
        let _ = ctx;
        match self.0 {
            "pass" => Ok(Verdict::Passed { assertions_evaluated: 3, notes: Vec::new() }),
            "fail" => Ok(Verdict::Failed {
                assertion: "assert_equals: expected \"10px\" but got \"20px\"".to_owned(),
                engine_location: None,
                notes: Vec::new(),
            }),
            "unsupported" => Ok(Verdict::Unsupported {
                capability: Some(Capability::NestedBrowsingContext),
                reason: "the adapter cannot create a nested browsing context".to_owned(),
            }),
            "error" => Err(HarnessError { message: "fixture read failed".to_owned() }),
            // A panic path, so the containment is exercised rather than assumed.
            _ => panic!("selftest stub: scripted panic"),
        }
    }
}

fn ctx() -> TestContext<'static> {
    TestContext {
        path: "selftest/fixture.html",
        area: "selftest",
        file: std::path::Path::new("selftest/fixture.html"),
        suite_root: std::path::Path::new("."),
        source: "<script>assert_equals(1, 1);</script>",
        assertion_sites: 1,
    }
}

fn record(out: &mut SelfTest, name: &str, expected: &str, observed: String, held: bool, why: &str) {
    out.checks.push(Check {
        name: name.to_owned(),
        expected: expected.to_owned(),
        observed,
        held,
        why_it_matters: why.to_owned(),
    });
    if held {
        out.passed += 1;
    } else {
        out.failed += 1;
    }
}

/// Run every check.
#[must_use]
pub fn run() -> SelfTest {
    let mut out = SelfTest::default();

    // 1. A genuine failure must be reported as a failure, with the assertion.
    {
        let result = run_one(&mut Stub("fail"), &ctx());
        let assertion_present = result.failing_assertion.is_some();
        let held = result.outcome == Outcome::Fail && assertion_present;
        record(
            &mut out,
            "a failing assertion is reported as fail, with the assertion text",
            "outcome=fail, failing_assertion=Some",
            format!("outcome={}, failing_assertion={:?}", result.outcome, result.failing_assertion),
            held,
            "a fail with no failing assertion is not a finding; a harness that cannot \
             produce one is scoring noise",
        );
    }

    // 2. A genuine pass must be reported as a pass, and must record that
    //    something was actually evaluated.
    {
        let result = run_one(&mut Stub("pass"), &ctx());
        let held = result.outcome == Outcome::Pass && result.assertions_evaluated > 0;
        record(
            &mut out,
            "a satisfied assertion is reported as pass and records assertions evaluated",
            "outcome=pass, assertions_evaluated>0",
            format!(
                "outcome={}, assertions_evaluated={}",
                result.outcome, result.assertions_evaluated
            ),
            held,
            "a pass that evaluated nothing is indistinguishable from a pass that checked \
             everything; the count is the only thing that separates them",
        );
    }

    // 3. A thrown adapter is an error, not a fail and not a pass.
    {
        let result = run_one(&mut Stub("error"), &ctx());
        let held = result.outcome == Outcome::Error;
        record(
            &mut out,
            "an adapter error is reported as error, never as fail",
            "outcome=error",
            format!("outcome={}, error={:?}", result.outcome, result.error),
            held,
            "folding harness errors into failures manufactures failures and understates \
             the engine",
        );
    }

    // 4. A panicking adapter is contained and becomes an error.
    {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_one(&mut Stub("panic"), &ctx())
        }));
        std::panic::set_hook(previous);
        let observed = match &result {
            Ok(r) => format!("outcome={}, error={:?}", r.outcome, r.error),
            Err(_) => "the panic escaped run_one entirely".to_owned(),
        };
        let held = matches!(&result, Ok(r) if r.outcome == Outcome::Error);
        record(
            &mut out,
            "a panicking adapter is contained and becomes error",
            "outcome=error, panic contained",
            observed,
            held,
            "an escaping panic kills the run and leaves a truncated result file that looks \
             like a smaller suite, which is an invisible wrong number",
        );
    }

    // 5. An unsupported shape is a skip with a reason, never a fail.
    {
        let result = run_one(&mut Stub("unsupported"), &ctx());
        let held = result.outcome == Outcome::Skip
            && result.skip_reason == Some(SkipReason::MissingEngineCapability)
            && !result.notes.is_empty();
        record(
            &mut out,
            "an unsupported capability is a skip with a reason, never a fail",
            "outcome=skip, skip_reason=Some, notes non-empty",
            format!(
                "outcome={}, skip_reason={:?}, notes={}",
                result.outcome,
                result.skip_reason,
                result.notes.len()
            ),
            held,
            "reporting this runner's gaps as engine defects is a false defect report, and a \
             false defect report is worse than no report",
        );
    }

    // 6. A test with no assertion is counted, not scored.
    {
        let scan = crate::source::scan("<script>var unused = 1;</script>");
        let inline: Vec<&str> = scan
            .scripts
            .iter()
            .filter_map(|s| s.inline.as_deref())
            .collect();
        let classification = crate::classify::classify(&scan, &inline, None, None);
        let held = classification.cannot_fail()
            && matches!(classification.blocker, Some(Blocker::NoAssertions))
            && !classification.is_executable();
        record(
            &mut out,
            "a test with no assertion site is flagged cannot-fail, not executable",
            "cannot_fail=true, is_executable=false",
            format!(
                "cannot_fail={}, is_executable={}, sites={}",
                classification.cannot_fail(),
                classification.is_executable(),
                classification.assertion_sites
            ),
            held,
            "a vacuous test scored as a pass inflates the pass count, which is the exact \
             failure this project already shipped once",
        );
    }

    // 7. A percentage cannot be emitted without its denominator.
    {
        let mut tally = Tally::new();
        tally.record(Outcome::Pass);
        tally.record(Outcome::Fail);
        tally.record(Outcome::Error);
        tally.record(Outcome::Skip);
        let rate = tally.engine_rate().expect("two attempted");
        let rendered = rate.to_string();
        let held = rendered.contains("/2") && rendered.contains('%');
        // And a tally with nothing attempted has no rate at all.
        let empty_has_no_rate = Tally::new().engine_rate().is_none();
        record(
            &mut out,
            "a reported rate always renders with its denominator, and an empty run has none",
            "rate string contains (pass/total) and an empty tally has no rate",
            format!("rate={rendered}, empty_tally_has_no_rate={empty_has_no_rate}"),
            held && empty_has_no_rate,
            "\"62%\" and \"62% of 4,180, with 2,280 skipped as unimplemented\" are different \
             claims; the second is the only usable one",
        );
    }

    // 8. A test blocked by a missing engine capability is never executable.
    {
        let scan = crate::source::scan("<script>test_parsed_value('color','red','red');</script>");
        let inline: Vec<&str> = scan
            .scripts
            .iter()
            .filter_map(|s| s.inline.as_deref())
            .collect();
        let classification = crate::classify::classify(&scan, &inline, None, None);
        let held = !classification.is_executable()
            && matches!(classification.blocker, Some(Blocker::MissingEngine(_)))
            && classification.shapes.contains(&HarnessShape::TestCss);
        record(
            &mut out,
            "a test needing a missing capability is blocked, never executable",
            "is_executable=false, blocker=MissingEngine",
            format!("is_executable={}, blocker={:?}", classification.is_executable(), classification.blocker.as_ref().map(Blocker::describe)),
            held,
            "counting a test as runnable when it is known not to run is how a denominator \
             grows to cover the whole suite while the numerator stays small",
        );
    }

    // 9. A missing engine must not be reported as a score of zero.
    {
        // The runner's own guard: with no engine there is no attempted count, so
        // there is no rate, and the report says the engine was unavailable
        // rather than printing 0%.
        let tally = Tally::new();
        let no_rate = tally.engine_rate().is_none();
        let zero_would_be_misleading = Tally { pass: 0, fail: 0, error: 0, skip: 0 }.total() == 0;
        record(
            &mut out,
            "an unavailable engine yields no rate at all, not a zero rate",
            "engine_rate is None when nothing was attempted",
            format!("no_rate={no_rate}, empty_total={zero_would_be_misleading}"),
            no_rate,
            "\"0% conformance\" produced by a runner that could not start is a false claim \
             about the engine, and it is the kind that gets quoted",
        );
    }

    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_self_check_itself_passes() {
        let result = super::run();
        assert!(
            result.trustworthy(),
            "harness self-check failed: {:?}",
            result
                .failures()
                .iter()
                .map(|c| (&c.name, &c.observed))
                .collect::<Vec<_>>()
        );
    }
}
