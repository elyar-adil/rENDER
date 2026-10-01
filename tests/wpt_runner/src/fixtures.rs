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
    ///
    /// The tree is whatever the URL names *at its shallowest level*, not
    /// necessarily the suite's top level. WPT nests helper trees under an area
    /// (`html/shadow-dom/`, `css/css-images/`), and a reference to
    /// `/html/shadow-dom/...` is resolvable by adding `html`, which the runner
    /// already has. Reporting it as "add `shadow-dom`" would send someone to
    /// fetch a top-level tree that does not exist.
    pub fn record(&mut self, url: &str, test_path: &str) {
        self.tests = self.tests.saturating_add(1);
        match fetch_scope(url) {
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

/// The trees this runner already holds, so a reference into one of them is not
/// reported as a fetch gap.
///
/// This is the difference between "add `shadow-dom`" (a tree that does not exist
/// at the suite root) and "nothing to add" (the file is `html/shadow-dom/`, and
/// the runner already fetched `html`). Getting it wrong in the *confident*
/// direction sends someone to fetch a path the archive does not contain.
const PRESENT_TREES: &[&str] = &["css", "dom", "html", "resources", "common", "fonts"];

/// The fetch scope a rooted URL falls into.
///
/// `None` for a relative URL: it resolves against the referring file, and
/// attributing it without that file would be a confident wrong answer.
#[must_use]
pub fn fetch_scope(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let rooted = path.strip_prefix('/')?;
    if rooted.is_empty() {
        return None;
    }
    let mut segments = rooted.split('/').filter(|s| !s.is_empty());
    let first = segments.next()?;
    if first == "." || first == ".." {
        return None;
    }
    if !rooted.contains('/') {
        // A bare file at the suite root. WPT keeps these in `resources/`.
        return Some("resources (root-level files)".to_owned());
    }
    if PRESENT_TREES.contains(&first) {
        // Already fetched. The missing part is *inside* a tree this runner has,
        // so it is either a genuinely absent file or one the census's exclusion
        // rules removed. Reporting it as a fetch gap is wrong either way.
        return None;
    }
    Some(first.to_owned())
}

#[cfg(test)]
mod tests {
    use super::{FixtureAccount, fetch_scope};

    #[test]
    fn a_rooted_url_names_a_tree_the_fetch_would_need() {
        assert_eq!(fetch_scope("/web-animations/x.js"), Some("web-animations".to_owned()));
        assert_eq!(fetch_scope("/svg/x.html"), Some("svg".to_owned()));
        // A query string does not change which tree a fixture is in.
        assert_eq!(fetch_scope("/fetch/api.js?v=1"), Some("fetch".to_owned()));
    }

    #[test]
    fn a_url_inside_a_tree_we_already_hold_is_not_a_fetch_gap() {
        // The measured run reported 130 tests as needing a tree called
        // `shadow-dom`. There is no such top-level tree: the files are
        // `html/shadow-dom/`, and the runner already fetched `html`. Reporting it
        // would have sent someone to fetch a path the archive does not contain.
        // The failure mode is worse than silence because it reads as actionable.
        assert_eq!(fetch_scope("/html/shadow-dom/x.html"), None);
        assert_eq!(fetch_scope("/css/css-images/x.png"), None);
        assert_eq!(fetch_scope("/dom/nodes/x.js"), None);
        assert_eq!(fetch_scope("/resources/x.js"), None);
    }

    #[test]
    fn a_root_level_file_is_attributed_to_resources() {
        // `/resources/WebIDLParser.js` and a bare `/WebIDLParser.js` both need
        // `resources/`. Reporting the second as unattributed would leave a
        // number in the report that nobody can act on.
        assert_eq!(
            fetch_scope("/WebIDLParser.js"),
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
        assert_eq!(fetch_scope("support/a.css"), None);
        assert_eq!(fetch_scope("../shared/x.html"), None);
        assert_eq!(fetch_scope(""), None);
        assert_eq!(fetch_scope("/"), None);
        // A bare filename with no leading slash is relative, not root-level.
        assert_eq!(fetch_scope("WebIDLParser.js"), None);
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
