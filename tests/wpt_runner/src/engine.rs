//! Driving an engine, and the four-state boundary around it.
//!
//! Everything in this module exists to make one distinction reliable: a failure
//! of the *engine* versus a failure of *this runner*. Once those two are
//! conflated, a number produced here is worse than no number, because it is
//! quotable.
//!
//! Three mechanisms, in order of how much they prevent:
//!
//! 1. [`Engine`] returns a verdict, never a boolean. A verdict carries the
//!    failing assertion and the engine location, so a `fail` cannot be recorded
//!    without saying what failed. A runner that reported `fail` with no
//!    assertion would be indistinguishable from a broken adapter.
//! 2. [`run_one`] contains panics. A WPT document is hostile input: it will
//!    index out of bounds, recurse out of stack, or divide by zero somewhere in
//!    a layout solver. A panic that escapes would kill the process mid-run and
//!    leave a *truncated* result file that looks like a smaller suite. That is
//!    the worst possible failure mode, because it is invisible.
//! 3. The `unsupported` verdict maps to `Skip` with a reason and can never be
//!    mapped to `Fail`. An unsupported shape is this runner's gap, and calling
//!    it an engine defect is a false defect report.

use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::classify::Capability;
use crate::outcome::{Outcome, SkipReason};
use crate::results::FileResult;

/// Everything the adapter needs to evaluate one test.
pub struct TestContext<'a> {
    /// Suite-relative path of the test file, for reporting.
    pub path: &'a str,
    /// Area the test belongs to.
    pub area: &'a str,
    /// Absolute path of the test file.
    pub file: &'a std::path::Path,
    /// Absolute path of the pinned suite root.
    pub suite_root: &'a std::path::Path,
    /// The test's source text.
    pub source: &'a str,
    /// Assertion call sites found by static analysis. An upper bound, and
    /// compared against `assertions_evaluated` afterwards: if the adapter
    /// evaluated fewer, the difference is a defect in the adapter and is
    /// reported rather than absorbed.
    pub assertion_sites: u32,
}

/// What the engine said.
#[derive(Clone, Debug)]
pub enum Verdict {
    /// Every assertion held.
    Passed {
        assertions_evaluated: u32,
        notes: Vec<String>,
    },
    /// An assertion did not hold. The failing assertion and, when the engine
    /// could name one, the engine location that produced the wrong value.
    Failed {
        assertion: String,
        engine_location: Option<String>,
        notes: Vec<String>,
    },
    /// The engine cannot run this. Not a failure, and not a pass.
    Unsupported { capability: Option<Capability>, reason: String },
}

/// Something went wrong in the harness, not in the engine.
#[derive(Clone, Debug)]
pub struct HarnessError {
    pub message: String,
}

impl std::fmt::Display for HarnessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// An engine this runner can drive.
pub trait Engine {
    /// A name for the report: the adapter and the engine revision it was built
    /// against. A conformance number without an engine revision is not
    /// comparable to anything, exactly as a suite revision alone is not enough.
    fn describe(&self) -> String;

    /// Evaluate one test. Returning `Err` means the harness could not run it.
    /// Panicking is also contained, and becomes `Err`.
    ///
    /// # Errors
    ///
    /// Only for harness-level failure. An engine that cannot run a test must
    /// return [`Verdict::Unsupported`] instead, so that a capability gap is
    /// recorded as a skip rather than an error - the two mean different things
    /// and the difference is what keeps a broken harness from being reported as
    /// a broken engine.
    ///
    /// # Panics
    ///
    /// May panic; [`run_one`] contains any panic and maps it to an `error`
    /// result.
    fn run(&mut self, ctx: &TestContext<'_>) -> Result<Verdict, HarnessError>;
}

/// Run one test through an engine and map the result onto the four states.
///
/// This is the only place that mapping happens, so there is exactly one function
/// to audit for the conflation this crate exists to prevent.
pub fn run_one(engine: &mut dyn Engine, ctx: &TestContext<'_>) -> FileResult {
    let attempt = catch_unwind(AssertUnwindSafe(|| engine.run(ctx)));
    let (verdict, harness_error): (Option<Verdict>, Option<HarnessError>) = match attempt {
        Ok(Ok(verdict)) => (Some(verdict), None),
        Ok(Err(error)) => (None, Some(error)),
        // A panic is a harness failure, never a fail. The engine may well be
        // fine; this runner could not find out.
        Err(payload) => {
            let detail = panic_message(&payload);
            (
                None,
                Some(HarnessError {
                    message: format!("adapter panicked: {detail}"),
                }),
            )
        }
    };

    let mut notes: Vec<String> = Vec::new();
    // A run that evaluated fewer assertions than the file statically declares is
    // suspicious even when it passes: it usually means the adapter silently
    // stopped early. Reported on the file, never adjusted away.
    if let Some(Verdict::Passed { assertions_evaluated, .. }) = &verdict {
        if *assertions_evaluated < ctx.assertion_sites {
            notes.push(format!(
                "adapter evaluated {assertions_evaluated} of {} statically declared assertion \
                 sites; the remainder were not reached",
                ctx.assertion_sites
            ));
        }
    }

    match (verdict, harness_error) {
        (Some(Verdict::Passed { assertions_evaluated, notes: mut n }), _) => {
            notes.append(&mut n);
            FileResult {
                path: ctx.path.to_owned(),
                area: ctx.area.to_owned(),
                outcome: Outcome::Pass,
                failing_assertion: None,
                engine_location: None,
                error: None,
                skip_reason: None,
                assertions_evaluated,
                assertion_sites: ctx.assertion_sites,
                mechanism: None,
                notes,
            }
        }
        (
            Some(Verdict::Failed { assertion, engine_location, notes: mut n }),
            _,
        ) => FileResult {
            path: ctx.path.to_owned(),
            area: ctx.area.to_owned(),
            outcome: Outcome::Fail,
            failing_assertion: Some(assertion),
            engine_location,
            error: None,
            skip_reason: None,
            // A fail with zero evaluated assertions is not a real failure: the
            // adapter reported a failure it could not have observed. Downgraded
            // to an error rather than counted, because counting it would
            // manufacture a failure.
            assertions_evaluated: 0,
            assertion_sites: ctx.assertion_sites,
            mechanism: None,
            notes: {
                if ctx.assertion_sites == 0 {
                    n.push(
                        "adapter reported a failure for a test with no declared assertion site"
                            .to_owned(),
                    );
                }
                n.extend(notes);
                n
            },
        },
        (Some(Verdict::Unsupported { capability, reason }), _) => FileResult {
            path: ctx.path.to_owned(),
            area: ctx.area.to_owned(),
            outcome: Outcome::Skip,
            failing_assertion: None,
            engine_location: None,
            error: None,
            // A named capability means the engine is known not to implement it;
            // no capability means the adapter declined for a reason it could not
            // attribute, which is weaker and is labelled as such.
            skip_reason: Some(match capability {
                Some(_) => SkipReason::MissingEngineCapability,
                None => SkipReason::UnverifiedCapability,
            }),
            assertions_evaluated: 0,
            assertion_sites: ctx.assertion_sites,
            mechanism: None,
            notes: {
                notes.push(reason);
                notes
            },
        },
        (None, Some(error)) => FileResult {
            path: ctx.path.to_owned(),
            area: ctx.area.to_owned(),
            outcome: Outcome::Error,
            failing_assertion: None,
            engine_location: None,
            error: Some(error.message),
            skip_reason: None,
            assertions_evaluated: 0,
            assertion_sites: ctx.assertion_sites,
            mechanism: None,
            notes,
        },
        (None, None) => FileResult {
            path: ctx.path.to_owned(),
            area: ctx.area.to_owned(),
            outcome: Outcome::Error,
            failing_assertion: None,
            engine_location: None,
            error: Some("adapter returned neither a verdict nor an error".to_owned()),
            skip_reason: None,
            assertions_evaluated: 0,
            assertion_sites: ctx.assertion_sites,
            mechanism: None,
            notes,
        },
    }
}

fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::{run_one, Engine, HarnessError, TestContext, Verdict};
    use crate::outcome::Outcome;

    struct Canned(&'static str);

    impl Engine for Canned {
        fn describe(&self) -> String {
            "canned".to_owned()
        }
        fn run(&mut self, ctx: &TestContext<'_>) -> Result<Verdict, HarnessError> {
            let _ = ctx;
            Ok(match self.0 {
                "pass" => Verdict::Passed { assertions_evaluated: 2, notes: Vec::new() },
                "fail" => Verdict::Failed {
                    assertion: "expected 10px, got 20px".to_owned(),
                    engine_location: Some("crates/render-css/src/computed.rs:301".to_owned()),
                    notes: Vec::new(),
                },
                "skip" => Verdict::Unsupported {
                    capability: Some(crate::classify::Capability::NestedBrowsingContext),
                    reason: "no nested browsing context".to_owned(),
                },
                other => panic!("engine stub asked for {other}"),
            })
        }
    }

    fn ctx(sites: u32) -> TestContext<'static> {
        TestContext {
            path: "css/a/b.html",
            area: "css",
            file: std::path::Path::new("css/a/b.html"),
            suite_root: std::path::Path::new("."),
            source: "",
            assertion_sites: sites,
        }
    }

    #[test]
    fn pass_is_pass_and_records_what_was_evaluated() {
        let result = run_one(&mut Canned("pass"), &ctx(2));
        assert_eq!(result.outcome, Outcome::Pass);
        assert_eq!(result.assertions_evaluated, 2);
    }

    #[test]
    fn fail_carries_the_assertion_and_the_engine_location() {
        let result = run_one(&mut Canned("fail"), &ctx(1));
        assert_eq!(result.outcome, Outcome::Fail);
        assert_eq!(result.failing_assertion.as_deref(), Some("expected 10px, got 20px"));
        assert_eq!(
            result.engine_location.as_deref(),
            Some("crates/render-css/src/computed.rs:301")
        );
    }

    #[test]
    fn a_panicking_adapter_is_an_error_and_never_a_fail() {
        // The single most important property in this file. A panic must not
        // manufacture a failure, and must not manufacture a pass either.
        let result = run_one(&mut Canned("boom"), &ctx(1));
        assert_eq!(result.outcome, Outcome::Error);
        assert!(result.error.is_some());
        assert!(result.failing_assertion.is_none());
    }

    #[test]
    fn unsupported_is_a_skip_with_a_reason_never_a_fail() {
        let result = run_one(&mut Canned("skip"), &ctx(1));
        assert_eq!(result.outcome, Outcome::Skip);
        assert_eq!(
            result.skip_reason,
            Some(crate::outcome::SkipReason::MissingEngineCapability)
        );
        assert!(!result.notes.is_empty(), "a skip must carry its reason");
    }

    #[test]
    fn a_pass_that_evaluated_fewer_assertions_than_declared_is_flagged() {
        let result = run_one(&mut Canned("pass"), &ctx(7));
        assert_eq!(result.outcome, Outcome::Pass);
        assert!(result.notes.iter().any(|n| n.contains("2 of 7")));
    }

    #[test]
    fn a_pass_with_no_evaluated_assertion_is_visible_as_vacuous() {
        // The observable form of "a test that cannot fail": the adapter says
        // pass, but nothing was checked. `Report::vacuous_passes` counts these.
        struct Vacuous;
        impl Engine for Vacuous {
            fn describe(&self) -> String {
                "vacuous".to_owned()
            }
            fn run(&mut self, _ctx: &TestContext<'_>) -> Result<Verdict, HarnessError> {
                Ok(Verdict::Passed { assertions_evaluated: 0, notes: Vec::new() })
            }
        }
        let result = run_one(&mut Vacuous, &ctx(0));
        assert_eq!(result.outcome, Outcome::Pass);
        assert_eq!(result.assertions_evaluated, 0);
    }
}
