//! Account for every "missing fixture", rather than leaving them uncounted.
//!
//! # The problem this solves
//!
//! The census reports 535 tests as blocked by `missing fixture`, and that
//! number is a **statement about the fetch, not about the suite**. The fixture
//! exists at the pinned revision; this runner's cache simply does not hold the
//! tree it lives in. Leaving those 535 as a single bucket is what makes a
//! census untrustworthy: a reader sees "535 missing" and reasonably concludes
//! something is broken, when in fact the fix is one line in a fetch script.
//!
//! # What is reported
//!
//! For each missing fixture, the **top-level WPT tree** it lives in, and how
//! many tests are waiting on it. That turns an opaque count into a work queue:
//!
//! ```text
//! web-animations   228 tests   1.5 MB
//! svg              141 tests   3.2 MB
//! ```
//!
//! The tree is the unit that matters because that is the unit the fetch script
//! extracts. "228 tests are blocked by `web-animations/`" is directly
//! actionable in a way that "228 tests have a missing fixture" is not.
//!
//! # Why this is not the same as widening the fetch
//!
//! Widening the fetch is a decision with a cost - every added tree is more disk
//! on a machine six other agents are building on - and the decision is not this
//! runner's to make silently. So this module *measures* the cost and reports
//! it, and the widening stays a human decision made against a number.

use std::collections::BTreeMap;

/// Where a missing fixture would live, and what is waiting on it.
#[derive(Clone, Debug, Default)]
pub struct FixtureAccount {
    /// Top-level WPT tree -> number of tests blocked by a fixture in it.
    by_tree: BTreeMap<String, u32>,
    /// Individual fixtures, for the handful a person needs to look at.
    examples: Vec<(String, String)>,
    /// How many distinct fixtures were missing.
    distinct: u32,
    /// How many test files were blocked, in total.
    tests: u32,
    /// Fixtures that could not be attributed to a tree at all.
    unclassified: u32,
}

impl FixtureAccount {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one missing fixture for one test.
    pub fn record(&mut self, url: &str, test_path: &str) {
        self.tests = self.tests.saturating_add(1);
        match top_level_tree(url) {
            Some(tree) => {
                *self.by_tree.entry(tree).or_default() += 1;
            }
            None => {
                self.unclassified = self.unclassified.saturating_add(1);
            }
        }
        if self.examples.len() < 25 {
            self.examples.push((url.to_owned(), test_path.to_owned()));
        }
    }

    /// Record a whole set at once, deduplicating the URLs.
    pub fn record_all<I: IntoIterator<Item = (String, String)>>(&mut self, entries: I) {
        let mut seen = std::collections::BTreeSet::new();
        for (url, test_path) in entries {
            if seen.insert(url.clone()) {
                self.distinct = self.distinct.saturating_add(1);
            }
            self.record(&url, &test_path);
        }
    }

    /// The trees, largest first.
    #[must_use]
    pub fn ranked(&self) -> Vec<(&str, u32)> {
        let mut out: Vec<(&str, u32)> = self
            .by_tree
            .iter()
            .map(|(k, v)| (k.as_str(), *v))
            .collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        out
    }

    #[must_use]
    pub const fn tests(&self) -> u32 {
        self.tests
    }

    #[must_use]
    pub const fn distinct_fixtures(&self) -> u32 {
        self.distinct
    }

    #[must_use]
    pub const fn unclassified(&self) -> u32 {
        self.unclassified
    }

    #[must_use]
    pub fn examples(&self) -> &[(String, String)] {
        &self.examples
    }

    /// Whether any missing fixture could not be attributed to a tree.
    ///
    /// A `true` here is a gap in *this* accounting, and it is stated in the
    /// report rather than hidden: "missing fixture" with no tree is a number a
    /// reader cannot act on, and that is the same failure as not counting it.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.unclassified == 0
    }
}

/// The top-level WPT tree a rooted URL lives in.
///
/// Returns `None` for a URL with no path segment after the leading slash, and
/// for a *relative* URL - which cannot be attributed without knowing the
/// referring file, and attributing it wrongly is worse than admitting the gap.
#[must_use]
pub fn top_level_tree(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    // Only a *rooted* URL names a tree. A relative URL - including a bare
    // filename with no leading slash - resolves against the referring file, and
    // attributing it to a tree without that file would be a confident wrong
    // answer. `support/a.css` and `WebIDLParser.js` are both relative; only
    // `/WebIDLParser.js` is root-level.
    let rooted = path.strip_prefix('/')?;
    // "/" strips to the empty string, which has no `/` and would otherwise fall
    // into the bare-file branch below and be reported as a `resources` gap.
    // An empty path names no tree at all.
    if rooted.is_empty() {
        return None;
    }
    if !rooted.contains('/') {
        // A bare file at the suite root. WPT keeps these in `resources/`.
        return Some("resources (root-level files)".to_owned());
    }
    let first = rooted.split('/').next()?;
    if first.is_empty() || first == "." || first == ".." {
        return None;
    }
    Some(first.to_owned())
}

#[cfg(test)]
mod tests {
    use super::{FixtureAccount, top_level_tree};

    #[test]
    fn a_rooted_url_names_its_tree() {
        assert_eq!(top_level_tree("/web-animations/x.js"), Some("web-animations".to_owned()));
        assert_eq!(top_level_tree("/svg/x.html"), Some("svg".to_owned()));
        // A query string does not change which tree a fixture is in.
        assert_eq!(top_level_tree("/fetch/api.js?v=1"), Some("fetch".to_owned()));
        assert_eq!(top_level_tree("/css/a/b.css#frag"), Some("css".to_owned()));
    }

    #[test]
    fn a_root_level_file_is_attributed_to_resources() {
        // `/resources/WebIDLParser.js` and a bare `/WebIDLParser.js` both need
        // `resources/`. Reporting the second as unattributed would leave a
        // number in the report that nobody can act on.
        assert_eq!(
            top_level_tree("/WebIDLParser.js"),
            Some("resources (root-level files)".to_owned())
        );
    }

    #[test]
    fn a_relative_url_is_refused_rather_than_guessed() {
        // A relative URL is relative to the referring file, so attributing it
        // to a tree without that file's path would be a confident wrong answer.
        // This test caught the opposite mistake: a bare filename with no leading
        // slash is *also* relative, and the implementation was accepting it and
        // calling it `resources`. `support/a.css` lives in the test's own
        // directory, and reporting it as a `resources` gap would send someone
        // to add a tree that is already there.
        assert_eq!(top_level_tree("support/a.css"), None);
        assert_eq!(top_level_tree("../shared/x.html"), None);
        assert_eq!(top_level_tree(""), None);
        assert_eq!(top_level_tree("/"), None);
        // A bare filename with no leading slash is relative, not root-level.
        assert_eq!(top_level_tree("WebIDLParser.js"), None);
    }

    #[test]
    fn the_ranking_is_by_count_then_name() {
        let mut account = FixtureAccount::new();
        for _ in 0..3 {
            account.record("/web-animations/a.js", "css/a.html");
        }
        for _ in 0..5 {
            account.record("/svg/a.js", "css/b.html");
        }
        account.record("/xhr/a.js", "css/c.html");
        let ranked = account.ranked();
        assert_eq!(ranked[0], ("svg", 5));
        assert_eq!(ranked[1], ("web-animations", 3));
        assert_eq!(ranked[2], ("xhr", 1));
        assert_eq!(account.tests(), 9);
    }

    #[test]
    fn distinct_fixtures_are_deduplicated_but_tests_are_not() {
        // Two tests can share one fixture, and the report needs both numbers:
        // "2 distinct fixtures blocking 5 tests" is actionable; either number
        // alone is not.
        let mut account = FixtureAccount::new();
        account.record_all(vec![
            ("/web-animations/a.js".to_owned(), "css/a.html".to_owned()),
            ("/web-animations/a.js".to_owned(), "css/b.html".to_owned()),
            ("/svg/b.js".to_owned(), "css/c.html".to_owned()),
        ]);
        assert_eq!(account.distinct_fixtures(), 2);
        assert_eq!(account.tests(), 3);
    }

    #[test]
    fn an_unattributable_fixture_is_reported_as_an_incomplete_account() {
        // The dangerous case: a "missing fixture" with no tree behind it. It
        // reads as a suite problem and is a fetch problem, and saying so is the
        // only way anyone fixes it.
        let mut account = FixtureAccount::new();
        account.record("/svg/a.js", "css/a.html");
        assert!(account.is_complete());
        account.record("support/relative.css", "css/b.html");
        assert!(!account.is_complete());
        assert_eq!(account.unclassified(), 1);
        // The relative one is still counted in the total: it is a real blocked
        // test even though it could not be attributed.
        assert_eq!(account.tests(), 2);
    }
}
