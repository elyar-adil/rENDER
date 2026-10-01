//! The expected diagnostic set for each fixture, and the check that reads it.
//!
//! This is a first-class output, not a smoke test. The engine reports a great
//! deal it cannot yet do, and `render-browser` currently *counts* those without
//! displaying them - each message still needs mapping to a
//! `StylesheetDiagnosticCode` variant, which does not exist yet - so this file
//! is the only place in the tree where a page's diagnostic stream can be read.
//!
//! The rule the sets encode is asymmetric, and the asymmetry is the point:
//!
//! * A diagnostic the engine **owes its author** appearing is correct
//!   behaviour. Every real page's stylesheet uses `@font-face`, and the honest
//!   expectation is that the engine says it dropped them. `n_ok` is the expected
//!   *count*, and a count of zero for something a real sheet contains would be
//!   the defect.
//! * A diagnostic **going missing** is worse than a wrong one, because nothing
//!   downstream can notice. A missing diagnostic is how a page silently loses
//!   92 rules (the `@supports` case) or a whole declaration block (the star-hack
//!   case). Every entry is required, so removal fails.
//!
//! So each fixture declares an exact set of `(kind, expected count)` pairs, and
//! three separate things are asserted: no kind outside the set, every kind in
//! the set present, and every count exactly right. A change in what the engine
//! reports is a visible diff on this table rather than a silent drift, and a
//! regression that swallows a diagnostic is a test failure.

use std::collections::BTreeMap;

use crate::diagnostics::{Kind, Stage, Stream};

/// What one fixture must report.
#[derive(Clone, Debug)]
pub struct Expectation {
    /// The diagnostic kinds the fixture's stylesheets must produce, and exactly
    /// how many times each.
    ///
    /// The count is over the whole load, so it covers the *number of sheets*
    /// too: a fixture with two external slots reports each at-rule twice, and a
    /// regression that only reported it once would be caught here.
    pub kinds: Vec<(Kind, usize)>,
    /// The coded diagnostics every stage must produce, and exactly how many
    /// times each. Keyed by `(stage, code)`.
    pub coded: Vec<((Stage, String), usize)>,
    /// Whether the embedded `<style>` and the external sheets must report
    /// *different* sets.
    ///
    /// This is the check that makes the two sources distinguishable. Without it
    /// a diagnostic could come from either and nothing would say which; with it,
    /// a fixture that puts its star hack in the embedded sheet and its
    /// `@font-face` in the external one proves that both paths are walked.
    pub sources_must_differ: bool,
}

/// The three kinds every fixture's *user-agent* stylesheet must report: none.
///
/// The user-agent sheet is the one stylesheet the engine wrote itself, and it
/// is parsed fresh on every load. Any diagnostic from it is a defect in this
/// repository rather than a property of a page, so it is required to be silent
/// separately from the author-sheet expectations.
#[must_use]
pub fn user_agent_sheet_is_silent(stream: &Stream) -> bool {
    // The user-agent sheet is collected as `(None, &ua_sheet)`, so its
    // diagnostics arrive with no owning node. Author sheets arrive with the
    // `<link>` or `<style>` element as the node, so a `None` node on the
    // stylesheet stage is exactly the user-agent sheet.
    !stream
        .entries()
        .iter()
        .any(|entry| entry.stage == Stage::StyleSheet && entry.node.is_none())
}

/// Stages whose diagnostics are counted by **code** and pinned per fixture,
/// rather than reduced to a message kind.
///
/// The first version of this file listed these as "must be silent". The
/// measurement rejected that: a normative table with a `caption`, a `colspan`
/// header and block content inside cells makes the formatting stage report
/// `BlockInsideInline` 161 times, and that is a *true* statement about the
/// engine's box tree rather than a capability loss. Demanding silence would have
/// been demanding that the engine not report what it can see - which is the
/// project's law, in the other direction.
///
/// So these are pinned as exact counts like everything else, and a count that
/// moves is a visible diff.
const COUNTED_STAGES: &[Stage] = &[
    Stage::HtmlDecode,
    Stage::HtmlParse,
    Stage::StyleDiscovery,
    Stage::ComputedStyle,
    Stage::Formatting,
    Stage::Layout,
    Stage::DisplayList,
    Stage::Image,
    Stage::Script,
    Stage::InlineSvg,
];

/// How many times each coded diagnostic this load reported, as
/// `(stage, code, count)`, for the stages in [`COUNTED_STAGES`].
#[must_use]
pub fn counted_by_code(stream: &Stream) -> BTreeMap<(Stage, String), usize> {
    let mut counts: BTreeMap<(Stage, String), usize> = BTreeMap::new();
    for entry in stream.entries() {
        if !COUNTED_STAGES.contains(&entry.stage) {
            continue;
        }
        let code = match &entry.kind {
            Kind::Coded(code, _) => (*code).to_owned(),
            other => other.summary(),
        };
        *counts.entry((entry.stage, code)).or_default() += 1;
    }
    counts
}

/// Every coded diagnostic this load reported, in a form a fixture table can be
/// written against.
#[must_use]
pub fn counted_set(stream: &Stream) -> Vec<((Stage, String), usize)> {
    counted_by_code(stream).into_iter().collect()
}

/// Compare a fixture's stream against its expectation.
///
/// Returns every way it differs, so a failure message can list all of them
/// rather than the first.
#[must_use]
pub fn differences(stream: &Stream, expected: &Expectation) -> Vec<String> {
    let mut problems = Vec::new();

    // Count by kind alone, so a diagnostic that is right but attributed to the
    // wrong node does not read as missing. Attribution is a separate question and
    // is not part of the set.
    let counts: BTreeMap<Kind, usize> = stream
        .entries()
        .iter()
        .filter(|entry| entry.stage == Stage::StyleSheet)
        .fold(BTreeMap::new(), |mut tally, entry| {
            *tally.entry(entry.kind.clone()).or_default() += 1;
            tally
        });

    for (kind, want) in &expected.kinds {
        let got = counts.get(kind).copied().unwrap_or(0);
        if got != *want {
            problems.push(format!(
                "{}: expected {want}, the engine reported {got}",
                kind.summary()
            ));
        }
    }
    for (kind, got) in &counts {
        if !expected.kinds.iter().any(|(wanted, _)| wanted == kind) {
            problems.push(format!(
                "{}: reported {got} times, which the fixture does not expect; \
                 add it to the expected set rather than deleting this line",
                kind.summary()
            ));
        }
    }

    if !user_agent_sheet_is_silent(stream) {
        let offenders: Vec<String> = stream
            .entries()
            .iter()
            .filter(|entry| entry.stage == Stage::StyleSheet && entry.node.is_none())
            .map(ToString::to_string)
            .collect();
        problems.push(format!(
            "the user-agent stylesheet reported {} diagnostics, and every one of them is a \
             defect in this repository: {}",
            offenders.len(),
            offenders.join("; ")
        ));
    }

    // The coded stages, by the same rule: an exact count per `(stage, code)`.
    let coded = counted_by_code(stream);
    for ((stage, code), want) in &expected.coded {
        let got = coded.get(&(*stage, code.clone())).copied().unwrap_or(0);
        if got != *want {
            problems.push(format!(
                "{stage} {code}: expected {want}, the engine reported {got}"
            ));
        }
    }
    for ((stage, code), got) in &coded {
        if !expected
            .coded
            .iter()
            .any(|((s, c), _)| s == stage && c == code)
        {
            problems.push(format!(
                "{stage} {code}: reported {got} times, which the fixture does not expect"
            ));
        }
    }

    problems
}

#[cfg(test)]
mod tests {
    use super::{Expectation, differences, user_agent_sheet_is_silent};
    use crate::diagnostics::{Kind, Stage, Stream, author_stylesheet_entry, stylesheet_entry};
    use render_core::dom::Dom;

    /// A node id that belongs to a real document.
    ///
    /// `NodeId`'s constructor is private to `render-dom` - a `NodeId` is an
    /// index into an arena, not a value you can invent - so a test that needs one
    /// has to get it from a `Dom`. Using the document node is the cheapest real
    /// one: the point of these tests is whether an entry carries a node at all,
    /// not which node it is.
    fn real_node() -> render_core::dom::NodeId {
        Dom::new().document()
    }

    fn font_face() -> Kind {
        Kind::AtRuleNotEvaluated("font-face".to_owned())
    }

    /// A page's own stylesheet diagnostics, which always carry the element that
    /// declared the sheet. This is the distinction the whole module turns on: a
    /// `None` node means the *user-agent* sheet, and a user-agent diagnostic is a
    /// defect in this repository rather than a property of a page.
    fn stream_of(messages: &[&str]) -> Stream {
        Stream::new(
            messages
                .iter()
                .copied()
                .map(|message| author_stylesheet_entry(real_node(), message))
                .collect(),
        )
    }

    #[test]
    fn an_exactly_matched_set_has_no_differences() {
        let stream = stream_of(&[
            "@font-face is parsed but not evaluated yet",
            "@font-face is parsed but not evaluated yet",
            "@keyframes is parsed but not evaluated yet",
        ]);
        let expected = Expectation {
            kinds: vec![
                (font_face(), 2),
                (Kind::AtRuleNotEvaluated("keyframes".to_owned()), 1),
            ],
            sources_must_differ: false,
            coded: Vec::new(),
        };
        assert!(differences(&stream, &expected).is_empty());
    }

    /// The direction that matters most: a diagnostic that stops being reported
    /// must fail, and it must fail with a count so a *partial* loss is visible
    /// too.
    #[test]
    fn a_missing_diagnostic_fails_with_both_counts() {
        let stream = stream_of(&["@font-face is parsed but not evaluated yet"]);
        let expected = Expectation {
            kinds: vec![
                (font_face(), 2),
                (Kind::AtRuleNotEvaluated("keyframes".to_owned()), 1),
            ],
            sources_must_differ: false,
            coded: Vec::new(),
        };
        let problems = differences(&stream, &expected);
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(
            problems[0].contains("expected 2, the engine reported 1"),
            "{:?}",
            problems[0]
        );
        assert!(
            problems[1].contains("expected 1, the engine reported 0"),
            "{:?}",
            problems[1]
        );
    }

    /// A diagnostic appearing that the fixture does not expect is a *diff* the
    /// table should absorb deliberately, not a failure to route around. The
    /// message says so, so the natural response is to update the set.
    #[test]
    fn an_unexpected_diagnostic_is_reported_rather_than_dropped() {
        let stream = stream_of(&["@nonesuch is not an at-rule this engine knows"]);
        let expected = Expectation {
            kinds: vec![(font_face(), 1)],
            sources_must_differ: false,
            coded: Vec::new(),
        };
        let problems = differences(&stream, &expected);
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(
            problems[1].contains("which the fixture does not expect"),
            "{:?}",
            problems[1]
        );
    }

    #[test]
    fn the_user_agent_sheet_is_required_to_be_silent() {
        let clean = Stream::new(vec![crate::diagnostics::Entry::new(
            Stage::StyleSheet,
            font_face(),
            Some(real_node()),
            "@font-face is parsed but not evaluated yet".to_owned(),
        )]);
        assert!(user_agent_sheet_is_silent(&clean));
        let expected = Expectation {
            kinds: vec![(font_face(), 1)],
            sources_must_differ: false,
            coded: Vec::new(),
        };
        assert!(differences(&clean, &expected).is_empty());

        // The same message with no owning node is the user-agent sheet.
        let loud = Stream::new(vec![stylesheet_entry(
            "@font-face is parsed but not evaluated yet",
        )]);
        assert!(!user_agent_sheet_is_silent(&loud));
        let problems = differences(&loud, &expected);
        assert!(
            problems
                .iter()
                .any(|problem| problem.contains("the user-agent stylesheet reported")),
            "{problems:?}"
        );
    }

    /// A coded diagnostic from a stage the fixture does not expect is a diff the
    /// table should absorb deliberately, not a failure to route around. This is
    /// the same rule as the stylesheet kinds, applied to the stages that carry a
    /// typed code.
    #[test]
    fn a_coded_diagnostic_from_an_unexpected_stage_is_reported() {
        let stream = Stream::new(vec![crate::diagnostics::coded_entry(
            Stage::Layout,
            "SomeLayoutProblem",
            Some(real_node()),
            "SomeLayoutProblem: some layout diagnostic",
        )]);
        let expected = Expectation {
            kinds: vec![],
            coded: Vec::new(),
            sources_must_differ: false,
        };
        let problems = differences(&stream, &expected);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0].contains("which the fixture does not expect"),
            "{:?}",
            problems[0]
        );
    }

    /// The direction that matters most for a coded diagnostic: a **partial** loss
    /// must fail too, and must say how many are left.
    #[test]
    fn a_partially_missing_coded_diagnostic_fails_with_both_counts() {
        let stream = Stream::new(vec![crate::diagnostics::coded_entry(
            Stage::Formatting,
            "BlockInsideInline",
            Some(real_node()),
            "BlockInsideInline: one",
        )]);
        let expected = Expectation {
            kinds: vec![],
            coded: vec![((Stage::Formatting, "BlockInsideInline".to_owned()), 161)],
            sources_must_differ: false,
        };
        let problems = differences(&stream, &expected);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0].contains("expected 161, the engine reported 1"),
            "{:?}",
            problems[0]
        );
    }
}
