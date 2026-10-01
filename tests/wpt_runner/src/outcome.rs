//! The four-state result model.
//!
//! Every design decision in this file exists because of one failure mode: a
//! conformance number that is subtly wrong. Three states are not enough, for two
//! symmetric reasons.
//!
//! * If a test that throws inside the harness is recorded as a failure, the
//!   harness manufactures failures and the engine looks worse than it is.
//! * If a test that throws inside the harness is recorded as a pass, the
//!   harness manufactures passes and the engine looks better than it is.
//!
//! The second mistake is the dangerous one, because it is the one that survives
//! review. A crash in the adapter is not evidence about the engine either way,
//! so it gets its own state and is never allowed into a pass or a fail.
//!
//! A fourth consideration: "skipped" is not one thing. A test is skipped because
//! the engine lacks a capability, because the harness cannot express the test's
//! shape, or because the area is out of scope. Those are different claims and
//! they are separated in [`SkipReason`], because a skip rate with a single
//! opaque bucket is how "62% of 4,180" becomes a lie.

use std::fmt;

/// The verdict for one test.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Every assertion the test makes was evaluated and held.
    Pass,
    /// At least one assertion was evaluated and did not hold. The failing
    /// assertion is recorded alongside this verdict, never just the verdict.
    Fail,
    /// The test could not be evaluated, for a reason that is about this
    /// runner, the suite, or the environment - not about the engine. A thrown
    /// adapter, an unreadable file, a harness global that does not exist.
    ///
    /// This state must never be collapsed into [`Outcome::Fail`] or
    /// [`Outcome::Pass`]. It is reported in its own bucket and excluded from
    /// both numerators.
    Error,
    /// Deliberately not run, with a reason from a fixed vocabulary.
    Skip,
}

impl Outcome {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Error => "error",
            Self::Skip => "skipped",
        }
    }

    /// Whether this verdict is evidence about the engine.
    ///
    /// Only `pass` and `fail` are. `error` is evidence about the harness and
    /// `skip` is evidence about scope. Any percentage computed over all four
    /// states is meaningless, which is why [`Tally::engine_rate`] refuses to
    /// include them.
    pub const fn is_engine_evidence(self) -> bool {
        matches!(self, Self::Pass | Self::Fail)
    }
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Why a test was not run.
///
/// The variants are ordered from "the engine cannot do this" to "this runner
/// cannot do this", because that is the order in which the counts are
/// interpretable. A skip is a statement about the system under test, and which
/// of these it is determines whether fixing it is an engine task, a runner
/// task, or neither.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SkipReason {
    /// The test needs a host capability the engine does not implement at all.
    /// The most important category: a large skip count here is a feature
    /// roadmap, not an engine defect.
    MissingEngineCapability,
    /// The test needs a capability the engine has not verified, and this
    /// runner has no way to check it. Distinct from the above because the
    /// engine may well work; the runner simply cannot tell.
    UnverifiedCapability,
    /// The test's assertion shape is not one of the four this runner supports
    /// (`test`, `promise_test`, `async_test`, `assert_throws_js`, plus the
    /// `testcss.js` entry points). Never a failure.
    UnsupportedHarnessShape,
    /// A resource the test declares does not exist in the checkout, or the
    /// checkout is missing a tree the test needs.
    MissingFixture,
    /// The area is out of scope by deliberate decision, not by accident.
    OutOfScopeArea,
    /// Nothing about the test was wrong; it was simply not selected.
    NotSelected,
}

impl SkipReason {
    pub const fn label(self) -> &'static str {
        match self {
            Self::MissingEngineCapability => "missing-engine-capability",
            Self::UnverifiedCapability => "unverified-capability",
            Self::UnsupportedHarnessShape => "unsupported-harness-shape",
            Self::MissingFixture => "missing-fixture",
            Self::OutOfScopeArea => "out-of-scope-area",
            Self::NotSelected => "not-selected",
        }
    }

    /// A one-line explanation for a report, so a skip count is never an
    /// unexplained number.
    pub const fn explain(self) -> &'static str {
        match self {
            Self::MissingEngineCapability => {
                "the engine does not implement a host capability this test needs"
            }
            Self::UnverifiedCapability => {
                "this runner cannot establish whether the engine supports this"
            }
            Self::UnsupportedHarnessShape => {
                "the test uses an assertion shape this runner does not implement"
            }
            Self::MissingFixture => "a declared support file is absent from the checkout",
            Self::OutOfScopeArea => "excluded by deliberate scope decision, not by failure",
            Self::NotSelected => "not selected for this run",
        }
    }

    /// Whether this reason is a statement about the engine rather than about
    /// this runner. Only these skips say something about conformance.
    pub const fn blames_engine(self) -> bool {
        matches!(self, Self::MissingEngineCapability | Self::UnverifiedCapability)
    }
}

impl fmt::Display for SkipReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Counts by verdict, with the arithmetic that makes a rate honest.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    pub pass: u32,
    pub fail: u32,
    pub error: u32,
    pub skip: u32,
}

impl Tally {
    #[must_use]
    pub const fn new() -> Self {
        Self { pass: 0, fail: 0, error: 0, skip: 0 }
    }

    pub fn record(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::Pass => self.pass += 1,
            Outcome::Fail => self.fail += 1,
            Outcome::Error => self.error += 1,
            Outcome::Skip => self.skip += 1,
        }
    }

    pub fn merge(&mut self, other: &Self) {
        self.pass += other.pass;
        self.fail += other.fail;
        self.error += other.error;
        self.skip += other.skip;
    }

    /// Every test seen, in every state. This is the *total* denominator and the
    /// one that belongs next to a statement like "the css/ area has N tests".
    pub const fn total(&self) -> u32 {
        self.pass + self.fail + self.error + self.skip
    }

    /// Tests that produced evidence about the engine: passes plus failures.
    ///
    /// This is the denominator of a conformance rate, and it is deliberately
    /// *not* `total()`. A suite where 4,000 tests were skipped for missing
    /// capability and 100 ran has a 100-test denominator, not a 4,100-test one.
    pub const fn attempted(&self) -> u32 {
        self.pass + self.fail
    }

    /// The conformance rate, or `None` when nothing was attempted.
    ///
    /// The denominator is returned alongside rather than discarded, so a caller
    /// cannot print a bare percentage. This is deliberate: the number is only
    /// meaningful attached to "of N, with M skipped as ...".
    pub const fn engine_rate(&self) -> Option<Rate> {
        if self.attempted() == 0 {
            return None;
        }
        Some(Rate {
            numerator: self.pass,
            denominator: self.attempted(),
        })
    }
}

/// A pass rate that carries its denominator, so it cannot be reported bare.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rate {
    pub numerator: u32,
    pub denominator: u32,
}

impl fmt::Display for Rate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // One decimal place. Two is false precision on a denominator in the
        // hundreds; zero hides real movement between runs.
        let percent = if self.denominator == 0 {
            0.0
        } else {
            f64::from(self.numerator) * 100.0 / f64::from(self.denominator)
        };
        write!(f, "{percent:.1}% ({}/{})", self.numerator, self.denominator)
    }
}

#[cfg(test)]
mod tests {
    use super::{Outcome, SkipReason, Tally};

    #[test]
    fn tally_keeps_four_states_distinct() {
        let mut tally = Tally::new();
        tally.record(Outcome::Pass);
        tally.record(Outcome::Fail);
        tally.record(Outcome::Error);
        tally.record(Outcome::Skip);
        assert_eq!(
            (tally.pass, tally.fail, tally.error, tally.skip),
            (1, 1, 1, 1)
        );
        assert_eq!(tally.total(), 4);
        assert_eq!(tally.attempted(), 2);
    }

    #[test]
    fn skip_is_summarised_separately_so_it_cannot_read_as_a_pass() {
        // Observed on a real run: a `Tally` with 0 passes, 0 fails and 6466 skips
        // rendered as an empty string for the skip column, so the one line a
        // reader skims read as "nothing here" rather than "6466 tests were
        // excluded". Every state must always print a number.
        let mut tally = Tally::new();
        for _ in 0..6466 {
            tally.record(Outcome::Skip);
        }
        assert_eq!(tally.engine_rate(), None, "no attempted test means no rate");
        assert_eq!(tally.total(), 6466);
    }

    #[test]
    fn a_partial_run_is_never_presented_as_a_conformance_rate() {
        // A rate over a subset is still a rate, so the denominator has to carry
        // the subset size; the report layer adds the PARTIAL label. Here the
        // check is that the two numbers are available and consistent.
        let mut tally = Tally::new();
        tally.record(Outcome::Pass);
        tally.record(Outcome::Pass);
        tally.record(Outcome::Fail);
        let rate = tally.engine_rate().expect("attempted");
        assert_eq!((rate.numerator, rate.denominator), (2, 3));
        assert_eq!(tally.total(), 3, "no test escapes the total");
    }

    #[test]
    fn engine_rate_excludes_harness_errors() {
        // 90 errors and 10 passes must not read as 100% conformance, and must
        // not read as 50% either. The attempted denominator is 10.
        let mut tally = Tally::new();
        for _ in 0..90 {
            tally.record(Outcome::Error);
        }
        for _ in 0..10 {
            tally.record(Outcome::Pass);
        }
        let rate = tally.engine_rate().expect("attempted");
        assert_eq!(rate.denominator, 10);
        assert_eq!(rate.to_string(), "100.0% (10/10)");
    }

    #[test]
    fn empty_tally_has_no_rate_rather_than_a_zero_percent() {
        // "0% of 0" is a claim that reads as total failure. It must be absent.
        let tally = Tally::new();
        assert!(tally.engine_rate().is_none());
    }

    #[test]
    fn only_pass_and_fail_are_engine_evidence() {
        assert!(Outcome::Pass.is_engine_evidence());
        assert!(Outcome::Fail.is_engine_evidence());
        assert!(!Outcome::Error.is_engine_evidence());
        assert!(!Outcome::Skip.is_engine_evidence());
    }

    #[test]
    fn skip_reasons_separate_engine_gaps_from_runner_gaps() {
        assert!(SkipReason::MissingEngineCapability.blames_engine());
        assert!(SkipReason::UnverifiedCapability.blames_engine());
        // A shape this runner cannot express is this runner's problem, and
        // counting it against the engine would be a false defect report.
        assert!(!SkipReason::UnsupportedHarnessShape.blames_engine());
        assert!(!SkipReason::MissingFixture.blames_engine());
        assert!(!SkipReason::OutOfScopeArea.blames_engine());
    }
}
