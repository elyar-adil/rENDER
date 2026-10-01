//! Which at-rules this engine recognises, and what it does with each one.
//!
//! CSS Syntax §4.2 makes an at-rule the engine does not recognise invalid, and
//! CSS Syntax §5.4.4 then discards it. Discarding it is the specified outcome,
//! but it is not by itself a *reported* one, and this project's law is that
//! unsupported CSS must never be silently ignored. The two states a discarded
//! at-rule can be in are different facts, so they get different words:
//!
//! - **recognised and unimplemented** - a specification defines it, this
//!   engine parses its prelude and its block, and nothing consumes either. The
//!   223 `@font-face` and 249 `@keyframes` blocks in the production corpus are
//!   in this state, and before this table existed a page using a webfont looked
//!   simply wrong with nothing anywhere saying why.
//! - **unrecognised** - no specification defines an at-rule by that name. It is
//!   still reported, because a report that names the at-keyword is what turns
//!   "my page is broken" into "this engine does not know about `@foo`".
//!
//! The table is also what answers `at-rule()` in a CSS feature query
//! (CSS Conditional Rules 5 §2.1.2), so a `@supports at-rule(@x)` block and the
//! diagnostic for `@x` cannot disagree about whether `@x` is known.

/// The at-rules a specification defines, with the section that defines each.
///
/// A name that is not here, and is not a prefixed alias of a name that is
/// here, is an unknown at-rule. The citation is what makes the table a claim
/// rather than a list: "recognised" means some specification says so.
///
/// `@charset` is deliberately absent. CSS Syntax §4.1 has the tokenizer consume
/// it before any at-rule parser sees it, so there is nothing here that could
/// recognise or report it, and CSS Conditional Rules 5 §2.1.2 says outright that
/// "because `@charset` is not a valid at-rule, it is not considered to be
/// supported under this definition". Listing it would claim a capability the
/// code does not have.
const RECOGNISED_AT_RULES: &[(&str, &str)] = &[
    ("color-profile", "CSS Color 4 §12"),
    ("container", "CSS Conditional 5 §5.4"),
    ("counter-style", "CSS Counter Styles 3 §3"),
    ("custom-media", "CSS Media Queries 5 §2"),
    ("else", "CSS Conditional 5 §4"),
    ("font-face", "CSS Fonts 4 §5"),
    ("font-feature-values", "CSS Fonts 4 §9"),
    ("font-palette-values", "CSS Fonts 4 §10"),
    ("import", "CSS Cascade 5 §6"),
    ("keyframes", "CSS Animations 1 §3"),
    ("layer", "CSS Cascade 5 §7"),
    ("media", "CSS Media Queries 4 §2"),
    ("namespace", "CSS Namespaces 3 §3"),
    ("page", "CSS Paged Media 3 §3"),
    ("position-try", "CSS Anchor Positioning 1 §2"),
    ("property", "CSS Properties and Values 1 §2"),
    ("scope", "CSS Cascade 6 §3"),
    ("starting-style", "CSS Transitions 2 §2"),
    ("supports", "CSS Conditional 3 §6"),
    ("view-transition", "CSS View Transitions 2 §2"),
    ("viewport", "CSS Device Adaptation 1 §3"),
    ("when", "CSS Conditional 5 §3"),
];

/// The recognised at-rules whose contents reach the cascade. Everything else in
/// [`RECOGNISED_AT_RULES`] parses and is then dropped.
const EVALUATED_AT_RULES: &[&str] = &["layer", "media", "supports"];

/// The recognised at-rules whose block is a `<declaration-list>` rather than a
/// `<rule-list>`, with the section that defines each block's contents.
///
/// The block's *contents* are the point. None of these at-rules is evaluated, so
/// the block is dropped either way, and a dropped block still has to be walked:
/// a syntax error inside it is a fact the parser found and then threw away, and
/// discarding a parse error that was found is the silent drop this project
/// forbids. Before this table existed only `@font-face` was walked, so a `@page`
/// block or a `@counter-style` block full of legacy hacks produced no diagnostic
/// anywhere - a partial drop of the defect `at_rules` was written to end.
///
/// `@font-feature-values` is deliberately absent: its block holds nested
/// at-rules (`@styleset`, `@character-variant`, ...), not descriptors, so
/// reading it as a declaration list would report every one of them as a bad
/// declaration. An at-rule that is not in [`RECOGNISED_AT_RULES`] at all is
/// absent for a stronger reason: CSS Syntax §4.2 makes it invalid, so nothing
/// is known about its block's grammar and guessing one would be a claim this
/// engine cannot support.
pub const DECLARATION_LIST_AT_RULES: &[(&str, &str)] = &[
    ("color-profile", "CSS Color 4 §12"),
    ("counter-style", "CSS Counter Styles 3 §3"),
    ("font-face", "CSS Fonts 4 §5"),
    ("font-palette-values", "CSS Fonts 4 §10"),
    ("page", "CSS Paged Media 3 §3"),
    ("position-try", "CSS Anchor Positioning 1 §2"),
    ("property", "CSS Properties and Values 1 §2"),
    ("view-transition", "CSS View Transitions 2 §2"),
    ("viewport", "CSS Device Adaptation 1 §3"),
];

/// Whether an at-rule's block is a `<declaration-list>`, so its contents parse
/// as declarations and each invalid one is reported on its own.
///
/// `name` must already be lowercased the way CSS Syntax §3.2 decodes an
/// at-keyword. A vendor-prefixed spelling answers for the at-rule it aliases,
/// for the same reason `at_rule_support` does.
#[must_use]
pub fn at_rule_block_is_declaration_list(name: &str) -> bool {
    canonical_at_rule(name).is_some_and(|at_rule| {
        DECLARATION_LIST_AT_RULES
            .iter()
            .any(|(candidate, _)| *candidate == at_rule)
    })
}

/// What this engine does with an at-rule whose at-keyword it has parsed.
///
/// The three states are not interchangeable, and collapsing them is the defect
/// this module exists to prevent: "I have never heard of `@foo`" and "I know
/// exactly what `@foo` means and cannot do it yet" call for different
/// follow-ups from whoever reads the diagnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AtRuleSupport {
    /// Its contents reach the cascade, under the condition the at-rule carries.
    Evaluated,
    /// A specification defines it, its prelude and block parse, and nothing
    /// consumes either.
    Unimplemented,
    /// No specification defines an at-rule by this name.
    Unrecognised,
}

/// The at-rule `name` is a spelling of, if any.
///
/// Vendor prefixes do not make a separate at-rule. `@-webkit-keyframes`,
/// `@-moz-keyframes` and `@-o-keyframes` are prefixed spellings of
/// `@keyframes` that every engine accepting one accepts, and the production
/// corpus ships all of them; `@-ms-viewport` is the same for `@viewport`.
/// Anything else behind a vendor prefix is this engine's own business and is
/// left unrecognised.
fn canonical_at_rule(name: &str) -> Option<&'static str> {
    if name.ends_with("keyframes") {
        return Some("keyframes");
    }
    if name == "-ms-viewport" {
        return Some("viewport");
    }
    RECOGNISED_AT_RULES
        .iter()
        .find(|(at_rule, _)| *at_rule == name)
        .map(|(at_rule, _)| *at_rule)
}

/// The section that defines the at-rule `name` is a spelling of, if any.
#[must_use]
pub fn at_rule_specification(name: &str) -> Option<&'static str> {
    canonical_at_rule(name).and_then(|at_rule| {
        RECOGNISED_AT_RULES
            .iter()
            .find(|(candidate, _)| *candidate == at_rule)
            .map(|(_, specification)| *specification)
    })
}

/// What this engine does with `name`, which must already be lowercased the way
/// CSS Syntax §3.2 decodes an at-keyword.
#[must_use]
pub fn at_rule_support(name: &str) -> AtRuleSupport {
    let Some(at_rule) = canonical_at_rule(name) else {
        return AtRuleSupport::Unrecognised;
    };
    if EVALUATED_AT_RULES.contains(&at_rule) {
        AtRuleSupport::Evaluated
    } else {
        AtRuleSupport::Unimplemented
    }
}

/// The diagnostic an at-rule this engine drops owes its author.
///
/// The name is written as it was, not as the canonical spelling, because a
/// reader looking for `@-webkit-keyframes` in a stylesheet must find the string
/// they wrote.
#[must_use]
pub fn at_rule_diagnostic(name: &str) -> String {
    match at_rule_support(name) {
        AtRuleSupport::Unrecognised => format!("@{name} is not an at-rule this engine knows"),
        // Already the established wording for a recognised at-rule that is
        // parsed and then dropped, kept verbatim so the diagnostics an
        // unimplemented at-rule produces do not change shape.
        AtRuleSupport::Unimplemented => format!("@{name} is parsed but not evaluated yet"),
        // Unreachable from the parser, and saying so is the point: a caller that
        // reached this arm has a bug, and the alternative - reusing the wording
        // above - would emit a diagnostic stating that an at-rule the engine
        // *does* evaluate was not evaluated. This project's law cuts both ways.
        AtRuleSupport::Evaluated => {
            format!("@{name} is parsed and evaluated, so it is not a discarded at-rule")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AtRuleSupport, at_rule_block_is_declaration_list, at_rule_diagnostic,
        at_rule_specification, at_rule_support,
    };

    /// Every name in the recognised table has to answer `Evaluated` or
    /// `Unimplemented` with a citation, and nothing outside it may answer with
    /// one. The two properties are what make "recognised" a claim, so they are
    /// what gets pinned.
    #[test]
    fn recognised_at_rules_are_cited_and_the_rest_are_not() {
        for (name, specification) in super::RECOGNISED_AT_RULES {
            assert!(
                at_rule_specification(name).is_some(),
                "{name} is in the recognised table and must be cited"
            );
            assert_eq!(at_rule_specification(name), Some(*specification));
            assert_ne!(
                at_rule_support(name),
                AtRuleSupport::Unrecognised,
                "{name} is in the recognised table and must not be reported unknown"
            );
        }
        for name in [
            "foo",
            "-webkit-foo",
            "-moz-thing",
            "supports-not",
            "keyframesx",
        ] {
            assert_eq!(
                at_rule_support(name),
                AtRuleSupport::Unrecognised,
                "{name} is not an at-rule any specification defines"
            );
            assert_eq!(at_rule_specification(name), None);
        }
    }

    /// A vendor-prefixed spelling is the same at-rule, so it inherits both the
    /// citation and the state, and the diagnostic still quotes the spelling the
    /// author wrote.
    #[test]
    fn prefixed_spellings_resolve_to_the_at_rule_they_alias() {
        for name in [
            "keyframes",
            "-webkit-keyframes",
            "-moz-keyframes",
            "-o-keyframes",
        ] {
            assert_eq!(at_rule_support(name), AtRuleSupport::Unimplemented);
            assert_eq!(at_rule_specification(name), Some("CSS Animations 1 §3"));
        }
        assert_eq!(
            at_rule_support("-ms-viewport"),
            AtRuleSupport::Unimplemented
        );
        assert_eq!(
            at_rule_diagnostic("-webkit-keyframes"),
            "@-webkit-keyframes is parsed but not evaluated yet"
        );
    }

    /// An unknown at-rule and a known-but-unimplemented one are reported with
    /// different words, because they are different facts. The unimplemented
    /// wording is the one the codebase already used, so it does not change.
    #[test]
    fn the_two_discard_reasons_read_differently() {
        assert_eq!(
            at_rule_diagnostic("font-face"),
            "@font-face is parsed but not evaluated yet"
        );
        assert_eq!(
            at_rule_diagnostic("nonesuch"),
            "@nonesuch is not an at-rule this engine knows"
        );
    }

    /// The three at-rules whose contents reach the cascade. A new at-rule is not
    /// evaluated by appearing here; it becomes evaluated by being implemented,
    /// and this test is what stops the list from growing by accident.
    #[test]
    fn only_three_at_rules_are_evaluated() {
        let evaluated: Vec<&str> = super::RECOGNISED_AT_RULES
            .iter()
            .map(|(name, _)| *name)
            .filter(|name| at_rule_support(name) == AtRuleSupport::Evaluated)
            .collect();
        assert_eq!(evaluated, ["layer", "media", "supports"]);
    }

    /// Every at-rule whose block is read as a declaration list has to be an
    /// at-rule a specification defines, has to be cited, and has to be one this
    /// engine drops rather than evaluates. A name that failed any of those would
    /// make the table claim something the code cannot do.
    #[test]
    fn declaration_list_at_rules_are_recognised_cited_and_unevaluated() {
        for (name, specification) in super::DECLARATION_LIST_AT_RULES {
            assert_eq!(
                at_rule_specification(name),
                Some(*specification),
                "{name} is read as a declaration list, so its grammar must be cited"
            );
            assert_eq!(
                at_rule_support(name),
                AtRuleSupport::Unimplemented,
                "{name}'s block is walked and then dropped, not applied"
            );
            assert!(at_rule_block_is_declaration_list(name), "{name}");
        }
        for name in [
            "media",
            "supports",
            "layer",
            "keyframes",
            "font-feature-values",
            "import",
            "nonesuch",
        ] {
            assert!(
                !at_rule_block_is_declaration_list(name),
                "{name}'s block is not a declaration list"
            );
        }
        // A prefixed spelling is the at-rule it aliases, so its block is read
        // the same way.
        assert!(at_rule_block_is_declaration_list("-ms-viewport"));
        assert!(!at_rule_block_is_declaration_list("-ms-font-face"));
    }
}
