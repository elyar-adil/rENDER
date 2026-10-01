//! Attribute every non-result to a mechanism, ranked.
//!
//! A conformance figure is only useful with the reasons attached, and the
//! reasons are far more actionable than the figure: one root cause can account
//! for hundreds of tests, and a reader who is handed "1,772 blocked" learns
//! nothing while a reader who is handed "all 1,772 need `document.createElementNS`"
//! learns exactly what to implement next.
//!
//! # Why a mechanism table and not a failure list
//!
//! Grouping by mechanism rather than by file is the whole point. WPT's tests
//! fail in clusters because they share drivers: `testharness.js`, `testcss.js`,
//! `subtest.js`, `idlharness.js`. A ranked list of 30,000 paths tells a reader
//! that the number is large. A ranked list of nine mechanisms, each with a count
//! and a named one-line cause, tells them what to do on Monday.
//!
//! # The rule this module exists to enforce
//!
//! **Every test produces a row.** A test that is silently absent is
//! indistinguishable from a test that was never attempted, and that ambiguity is
//! how a run under-reports. The sweep's own accounting is checked against the
//! population it claims to have covered, and a mismatch is an error rather than
//! a rounding difference.

use std::collections::BTreeMap;

use crate::outcome::Outcome;

/// Why a test did not produce a pass or a fail, in the four categories the
/// brief requires.
///
/// The four are not interchangeable and the distinctions are load-bearing:
///
/// * an **engine defect** is a bug - the engine computed something and it was
///   wrong;
/// * an **acceptable difference** is a known, deliberate divergence where the
///   engine's answer is defensible;
/// * an **unimplemented feature** is a gap in the engine's surface - a missing
///   API, not a wrong answer;
/// * a **harness limitation** is a gap in *this runner*, and calling it
///   anything else is a false claim about the engine.
///
/// Collapsing the third into the first inflates the defect list with things
/// that are not defects, which destroys the defect list. Collapsing the fourth
/// into either of the others manufactures evidence against the engine. So the
/// categories are separate types, and the tally arithmetic refuses to add them
/// together into one number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Category {
    /// The engine produced a wrong answer. A bug, with a `file:line` when the
    /// engine exposes one.
    EngineDefect,
    /// A known divergence where the engine's behaviour is defensible and WPT
    /// permits it. Counted, and never presented as a defect.
    AcceptableDifference,
    /// The engine does not implement the API the test needs. A roadmap item.
    UnimplementedFeature,
    /// This runner cannot express the test. Not evidence about the engine in
    /// either direction.
    HarnessLimitation,
}

impl Category {
    pub const fn label(self) -> &'static str {
        match self {
            Self::EngineDefect => "engine-defect",
            Self::AcceptableDifference => "acceptable-difference",
            Self::UnimplementedFeature => "unimplemented-feature",
            Self::HarnessLimitation => "harness-limitation",
        }
    }

    /// Whether this category says the *engine* is wrong, as opposed to absent
    /// or unmeasured. Only [`Category::EngineDefect`] does.
    ///
    /// A defect list built from any other category is a false defect report,
    /// and a false defect report is worse than no defect list because it sends
    /// someone to fix something that is not broken.
    pub const fn is_a_defect(self) -> bool {
        matches!(self, Self::EngineDefect)
    }
}

/// One mechanism: a single root cause, with how many tests it accounts for.
#[derive(Clone, Debug)]
pub struct Mechanism {
    /// Stable key, for grouping and for regression guarding.
    pub key: String,
    /// One line a person can act on, naming the *thing* to implement or fix.
    pub cause: String,
    pub category: Category,
    /// Tests accounted for by this mechanism.
    pub tests: u32,
    /// Areas the mechanism reaches into, so a reader knows the blast radius.
    pub areas: Vec<String>,
    /// Up to `SAMPLE_PATHS` example paths. Examples, not the whole set: a
    /// mechanism with 4,000 tests does not become more actionable by printing
    /// 4,000 paths.
    pub examples: Vec<String>,
}

/// How many example paths a mechanism keeps.
const SAMPLE_PATHS: usize = 3;

/// The ranked mechanism table.
///
/// Ranked by count, because the reader's next action is "fix the top one" and
/// any other order makes them do the ranking themselves.
#[derive(Clone, Debug, Default)]
pub struct MechanismTable {
    mechanisms: BTreeMap<String, Mechanism>,
    /// Tests the table accounts for. Must equal the population swept, or the
    /// table is incomplete and says so rather than reading as complete.
    pub accounted: u32,
}

impl MechanismTable {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one test's cause.
    pub fn record(&mut self, key: &str, cause: &str, category: Category, area: &str, path: &str) {
        let tests = self.accounted.saturating_add(1);
        self.accounted = tests;
        let entry = self.mechanisms.entry(key.to_owned()).or_insert_with(|| Mechanism {
            key: key.to_owned(),
            cause: cause.to_owned(),
            category,
            tests: 0,
            areas: Vec::new(),
            examples: Vec::new(),
        });
        entry.tests = entry.tests.saturating_add(1);
        if !entry.areas.iter().any(|a| a == area) {
            entry.areas.push(area.to_owned());
            entry.areas.sort();
        }
        if entry.examples.len() < SAMPLE_PATHS {
            entry.examples.push(path.to_owned());
        }
    }

    /// The mechanisms, largest first.
    ///
    /// Ties break on the key so the table is stable across runs: a table that
    /// reorders itself between runs cannot be diffed, and a table that cannot
    /// be diffed is not a measurement instrument.
    #[must_use]
    pub fn ranked(&self) -> Vec<&Mechanism> {
        let mut out: Vec<&Mechanism> = self.mechanisms.values().collect();
        out.sort_by(|a, b| b.tests.cmp(&a.tests).then_with(|| a.key.cmp(&b.key)));
        out
    }

    /// Total tests attributed to each category.
    ///
    /// This is the four-category breakdown the brief requires, and it is kept
    /// as four separate numbers on purpose. A single "failed: N" line would
    /// put an unimplemented API and a wrong computed value in the same bucket,
    /// and the two mean opposite things to whoever reads it.
    #[must_use]
    pub fn by_category(&self) -> BTreeMap<Category, u32> {
        let mut out = BTreeMap::new();
        for mechanism in self.mechanisms.values() {
            *out.entry(mechanism.category).or_insert(0) += mechanism.tests;
        }
        out
    }

    /// The number of tests attributed to engine defects, and only to defects.
    #[must_use]
    pub fn defect_count(&self) -> u32 {
        self.by_category()
            .get(&Category::EngineDefect)
            .copied()
            .unwrap_or(0)
    }
}

/// A stable, readable key derived from a free-text cause.
///
/// Stable because a mechanism table whose keys change every run cannot be
/// diffed, and a table that cannot be diffed cannot show progress. Readable
/// because a table of hashed keys is a table nobody can act on.
#[must_use]
pub fn slug(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
        if out.len() >= 48 {
            break;
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "other".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// The category a `fail` belongs to, decided from the evidence attached to it.
///
/// The default matters. A failure with no mechanism attached is *not* a defect
/// and must not be counted as one: the engine could be right and the harness
/// wrong, and there is no way to tell from the record alone. So the default is
/// [`Category::HarnessLimitation`], which attributes the uncertainty to this
/// runner rather than to the engine. A defect list that quietly includes
/// unattributed failures is a defect list with an unknown error bar.
#[must_use]
pub fn categorise(outcome: Outcome, mechanism: Option<&str>) -> Category {
    match outcome {
        Outcome::Pass | Outcome::Error => Category::HarnessLimitation,
        Outcome::Skip => match mechanism {
            // A named missing API is an unimplemented feature, which is a
            // roadmap item and not a bug. Naming it in the record is what makes
            // the difference between the two, so the absence of a name is
            // itself the signal that this runner cannot tell.
            Some(_) => Category::UnimplementedFeature,
            None => Category::HarnessLimitation,
        },
        Outcome::Fail => match mechanism {
            Some(_) => Category::EngineDefect,
            None => Category::HarnessLimitation,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{Category, MechanismTable, SAMPLE_PATHS, categorise};
    use crate::outcome::Outcome;

    #[test]
    fn mechanisms_rank_by_count_and_break_ties_stably() {
        let mut table = MechanismTable::new();
        for _ in 0..5 {
            table.record("b", "cause b", Category::UnimplementedFeature, "dom", "d/1.html");
        }
        for _ in 0..9 {
            table.record("a", "cause a", Category::EngineDefect, "html", "h/1.html");
        }
        table.record("c", "cause c", Category::HarnessLimitation, "html", "h/2.html");
        let ranked = table.ranked();
        assert_eq!(ranked[0].key, "a");
        assert_eq!(ranked[0].tests, 9);
        assert_eq!(ranked[1].key, "b");
        assert_eq!(ranked[2].key, "c");
    }

    #[test]
    fn a_tie_breaks_on_the_key_so_the_table_can_be_diffed() {
        let mut first = MechanismTable::new();
        let mut second = MechanismTable::new();
        for table in [&mut first, &mut second] {
            table.record("z", "z", Category::EngineDefect, "dom", "z.html");
            table.record("a", "a", Category::EngineDefect, "dom", "a.html");
        }
        let keys: Vec<&str> = first.ranked().iter().map(|m| m.key.as_str()).collect();
        let keys2: Vec<&str> = second.ranked().iter().map(|m| m.key.as_str()).collect();
        assert_eq!(keys, keys2);
        // Insertion order must not decide it; that would make a run's output
        // depend on directory iteration order.
        assert_eq!(keys, vec!["a", "z"]);
    }

    #[test]
    fn examples_are_capped_but_counts_are_not() {
        let mut table = MechanismTable::new();
        for i in 0..50 {
            table.record("k", "cause", Category::UnimplementedFeature, "dom", &format!("d/{i}.html"));
        }
        let ranked = table.ranked();
        assert_eq!(ranked[0].tests, 50);
        assert_eq!(ranked[0].examples.len(), SAMPLE_PATHS);
    }

    #[test]
    fn the_four_categories_stay_separate() {
        // The point of the exercise: four numbers, not one. A single "failures:
        // 17" would put an unimplemented API and a wrong computed value in the
        // same bucket and the two mean opposite things.
        let mut table = MechanismTable::new();
        table.record("d", "wrong value", Category::EngineDefect, "css", "c/1.html");
        table.record("u", "no such API", Category::UnimplementedFeature, "css", "c/2.html");
        table.record("h", "cannot express", Category::HarnessLimitation, "dom", "d/1.html");
        table.record("a", "documented divergence", Category::AcceptableDifference, "html", "h/1.html");
        let by_category = table.by_category();
        assert_eq!(by_category[&Category::EngineDefect], 1);
        assert_eq!(by_category[&Category::UnimplementedFeature], 1);
        assert_eq!(by_category[&Category::HarnessLimitation], 1);
        assert_eq!(by_category[&Category::AcceptableDifference], 1);
        assert_eq!(table.accounted, 4);
    }

    #[test]
    fn only_defects_count_as_defects() {
        let mut table = MechanismTable::new();
        table.record("u", "no such API", Category::UnimplementedFeature, "css", "c/1.html");
        table.record("h", "cannot express", Category::HarnessLimitation, "dom", "d/1.html");
        assert_eq!(table.defect_count(), 0, "a missing API is not a bug");
    }

    #[test]
    fn an_unattributed_failure_is_a_harness_problem_not_a_defect() {
        // The default that protects the defect list. A `fail` with no named
        // mechanism could be a wrong engine answer or a broken harness, and
        // there is nothing in the record to tell them apart. Attributing it to
        // the engine would be a guess presented as a finding.
        assert_eq!(categorise(Outcome::Fail, None), Category::HarnessLimitation);
        assert_eq!(
            categorise(Outcome::Fail, Some("computed colour of border-image")),
            Category::EngineDefect
        );
    }

    #[test]
    fn a_skip_with_no_named_mechanism_is_not_an_unimplemented_feature() {
        // Claiming "unimplemented" without naming the missing thing is a guess.
        assert_eq!(categorise(Outcome::Skip, None), Category::HarnessLimitation);
        assert_eq!(
            categorise(Outcome::Skip, Some("document.createElementNS")),
            Category::UnimplementedFeature
        );
    }

    #[test]
    fn accounting_covers_every_recorded_test() {
        let mut table = MechanismTable::new();
        for i in 0..30 {
            table.record("k", "cause", Category::UnimplementedFeature, "html", &format!("h/{i}.html"));
        }
        assert_eq!(table.accounted, 30);
        assert_eq!(
            table.ranked().iter().map(|m| m.tests).sum::<u32>(),
            30,
            "every recorded test must be attributed to exactly one mechanism"
        );
    }
}
