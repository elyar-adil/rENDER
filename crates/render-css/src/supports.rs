//! CSS feature queries: the `<supports-condition>` grammar of `@supports` and
//! the oracle that answers it.
//!
//! `@supports` is not a rendering feature, it is a *claim* about the user agent
//! made by the stylesheet. Getting it wrong is worse than not having it, because
//! it exists so an author can ship a fallback and a progressive enhancement
//! together: applying both hands the author neither, and the page looks wrong
//! with nothing saying why. So the condition is parsed against the grammar in
//! CSS Conditional Rules 3 §6 (extended by Level 4 §2 and Level 5 §2) and
//! answered by a real oracle, never by a default.
//!
//! ## Where the oracle lives, and why
//!
//! Every question `@supports` can ask about a *declaration* is one this crate
//! already answers: does `render-css` accept `display: grid` or discard it as a
//! parse error. That is [`properties::parse_typed_property`] plus
//! [`computed::PropertyRegistry`], both in this crate, so the oracle is
//! [`supports_declaration`] and it reads the same functions the cascade reads.
//! A support question that had to be answered by a second, weaker parse is how
//! this project shipped a false `MediaQueryUnsupported` on every
//! `@media (min-width: ...)` in the corpus (see `docs/visual_fidelity_gaps.md`
//! §S17): one evaluator, one answer.
//!
//! Three of the features CSS Conditional Rules 5 §2 added cannot be answered
//! here, and they are the ones that name a capability living in another crate:
//! `font-tech()` and `font-format()` ask whether a font can be *used* (needs the
//! font backend in `render-browser` and font fetching in `render-net`), and
//! `at-rule()` asks whether a recognised-but-unimplemented at-rule is
//! *supported* (needs whatever consumer that at-rule is waiting for). Those come
//! back as [`SupportsCondition::Undecidable`] carrying the missing dependency,
//! never as a boolean. Answering them `true` would claim a capability this
//! crate cannot see; answering them `false` without saying so would silently
//! drop working CSS. Both are the project's law cut in opposite directions, and
//! the third state is the only one that obeys both.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use cssparser::{ParseError, Parser, ParserInput, SourcePosition, Token};

use super::at_rules::{AtRuleSupport, at_rule_support};
use super::cascade::expand_shorthand;
use super::computed::PropertyRegistry;
use super::properties::{DECLARED_GRAMMARS, parse_typed_property};
use super::selector::parse_selector_list;

/// A feature in a `@supports` condition that this crate cannot answer.
///
/// The reason names the crate that has to answer it, because a diagnostic that
/// says only "cannot evaluate" leaves the reader with nothing to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnansweredFeature {
    /// The feature as written, so the message points at something the author
    /// can find in the stylesheet.
    pub feature: String,
    /// Which capability is missing, and where it would have to live.
    pub reason: &'static str,
}

/// The result of evaluating one `<supports-condition>`.
///
/// Three states, not two. A `false` answer is a *complete* answer - the engine
/// does not support the thing asked about, which is exactly what the author
/// wrote the query to find out - and it is never a reason to report a gap. Only
/// [`Self::Undecidable`] is a gap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SupportsCondition {
    /// The condition matched the grammar and every feature in it was answered.
    Evaluated(bool),
    /// The condition does not match the grammar. CSS Conditional Rules 3 §6:
    /// "Any `@supports` rule that does not parse according to the grammar above
    /// [...] is invalid. Style sheets must not use such a rule and processors
    /// must ignore such a rule (including all of its contents)."
    Invalid,
    /// At least one feature needs a capability outside this crate, so no
    /// boolean is available.
    Undecidable(Vec<UnansweredFeature>),
}

impl SupportsCondition {
    /// Whether the condition is a true one, and its rules apply.
    #[must_use]
    pub fn applies(&self) -> bool {
        matches!(self, Self::Evaluated(true))
    }

    /// Whether the condition was fully answered, which is what a consumer
    /// reporting capability gaps needs. It is the counterpart of
    /// [`crate::cascade::media_query_list_is_supported`], and the two halves of
    /// the project's law fall out of it: an [`Self::Evaluated`] `false` is a
    /// complete answer about something this engine does not support, which is
    /// the author's question being answered rather than a gap being reported,
    /// and only [`Self::Undecidable`] means this engine could not pose the
    /// question at all.
    #[must_use]
    pub fn is_answered(&self) -> bool {
        !matches!(self, Self::Undecidable(_))
    }
}

/// Evaluate one `<supports-condition>`.
///
/// An empty condition is `Invalid`, not `false`: `@supports { }` has no
/// condition, and CSS Conditional Rules 3 §6 has no production that matches
/// nothing, so the rule is invalid rather than unmet.
#[must_use]
pub fn evaluate_supports_condition(source: &str) -> SupportsCondition {
    let mut input = ParserInput::new(source);
    let mut parser = Parser::new(&mut input);
    match parse_condition(&mut parser) {
        // The grammar has to consume the whole prelude. A leftover token means
        // the prelude is not what the condition claims to be - which is also
        // how `not(...)` ends up invalid, since it tokenizes as a function
        // token named `not` followed by a block, and §6 requires whitespace
        // after `not`, `and`, `or` and `else` precisely so the function token
        // cannot be formed.
        Ok(outcome) if parser.next().is_err() => outcome.into_condition(),
        _ => SupportsCondition::Invalid,
    }
}

/// CSS Conditional Rules 3 §7.5's `CSS.supports(conditionText)`: parse and
/// evaluate `source` as a `<supports-condition>`, and if that is not true, try
/// once more with the text wrapped in parentheses. §7.5: "If conditionText,
/// wrapped in parentheses and then parsed and evaluated as a
/// `<supports-condition>`, would return true, return true."
#[must_use]
pub fn supports_condition_text(source: &str) -> bool {
    if evaluate_supports_condition(source).applies() {
        return true;
    }
    evaluate_supports_condition(&format!("({source})")).applies()
}

/// Whether this engine supports a declaration, as CSS Conditional Rules 3 §6.1
/// defines the term: "A CSS processor is considered to support a declaration
/// (consisting of a property and value) if it accepts that declaration (rather
/// than discarding it as a parse error) within a style rule."
///
/// The same definition, applied to the same functions, is what keeps the answer
/// from ever reporting supported CSS as unsupported. §6.1 adds the other half of
/// the project's law: "If a processor does not implement, with a usable level of
/// support, both the property and the value given, then it must not accept the
/// declaration or claim support for it."
#[must_use]
pub fn supports_declaration(property: &str, value: &str) -> bool {
    let property = property.trim();
    if property.is_empty() {
        return false;
    }
    if let Some(name) = property.strip_prefix("--") {
        // CSS Variables 1 §2.1: a custom property's value is its whole token
        // stream, so every non-empty value is a valid one, and §7.5's
        // `CSS.supports` treats a custom property name string as a property the
        // UA supports. Only the `<dashed-ident>` production can reject it.
        return !value.trim().is_empty() && is_custom_property_name(name);
    }
    // §6.1: "implementations must implement all parts of the value in order to
    // consider the declaration supported". A shorthand has no grammar of its own
    // here - the longhands carry the types - so the answer is the conjunction
    // over the longhands the cascade actually stores, which is the same
    // expansion the cascade does. An unreadable value expands to nothing, and an
    // empty conjunction is not a supported declaration.
    let longhands = expand_shorthand(property, value);
    !longhands.is_empty()
        && longhands
            .iter()
            .all(|(name, text)| supports_longhand(name, text))
}

fn supports_longhand(property: &str, value: &str) -> bool {
    match parse_typed_property(property, value) {
        // A grammar this engine claims accepted the value, so the declaration
        // survives into the cascade and is supported.
        Some(Ok(_)) => true,
        // A claimed grammar rejected this value. This is the case §6.1 spells
        // out: the declaration is discarded as a parse error, so it is not
        // supported.
        Some(Err(_)) => false,
        // No grammar is claimed, so nothing in this engine can reject the value.
        // The property is still only supported if this engine has metadata for
        // it: a property nothing has heard of is not supported, and saying so is
        // how a page's `@supports (-moz-thing: 1)` correctly reaches the
        // fallback its author wrote, rather than being told the fallback is
        // unnecessary.
        //
        // This is the one place where the answer is weaker than a browser's,
        // and it is weak in the direction that cannot hide a drop: with no
        // grammar there is nothing to check, so every value is accepted here
        // exactly as it is accepted in a style rule. `@supports (font-size:
        // 1rem)` is therefore true, which is right - the engine reads
        // `font-size` - and `@supports (font-size: nonsense)` is also true,
        // which is right for the reason §6.1 gives rather than for a check this
        // engine has no grammar to perform.
        None => is_known_property(property),
    }
}

/// Whether this engine has any declared knowledge of `property`: a grammar in
/// [`properties`] or metadata in [`computed`].
///
/// This is [`property_support`] collapsed to the question `@supports` asks, so
/// the two cannot drift: a property is "known" here exactly when it is not
/// [`PropertySupport::Unsupported`]. §6.1 needs the bool because the answer it
/// feeds is whether a declaration is supported, and the third state has nowhere
/// to go in a boolean - it is carried by [`property_support`] for the consumers
/// that can report it.
fn is_known_property(property: &str) -> bool {
    property_support(property) != PropertySupport::Unsupported
}

/// The property names the baseline [`PropertyRegistry`] defines, read once
/// because a feature query is evaluated once per `@supports` block and
/// rebuilding the registry for every feature in it would be wasteful.
fn registry_names() -> &'static BTreeSet<String> {
    static NAMES: OnceLock<BTreeSet<String>> = OnceLock::new();
    NAMES.get_or_init(|| {
        PropertyRegistry::standard_baseline()
            .iter()
            .map(|(name, _)| name.to_owned())
            .collect()
    })
}

/// What this engine knows about an ordinary property **name** - with no element,
/// no declaration and no value in scope.
///
/// # Why this is not a state on [`super::computed::ComputedStyle`]
///
/// [`ComputedStyle::get`] answers "is there a value here to read?", and its
/// `Option` has two other answers folded into the same `None`. A consumer that
/// reaches for it to ask *whether this engine supports the property* gets a
/// wrong answer in both directions:
///
/// - a property the registry defines but this slice has no grammar for reads
///   back `Some(initial value)`, so "the document said nothing and the engine
///   cannot read it anyway" is indistinguishable from "here is the value"; and
/// - a property this engine has never heard of also reads `None`, so "no such
///   property" is indistinguishable from "no value on this element".
///
/// That is the same conflation `docs/visual_fidelity_gaps.md` S22 records for
/// the *author-intent* question, and it was fixed there by adding a query with
/// its own name rather than by reinterpreting `get` - [`ComputedStyle::
/// specified`] is that query. This is the support half of the same pair, and it
/// gets the same treatment for the same reason: `get` is read by roughly a
/// hundred call sites that mean "read the value", and a tri-state return would
/// force all of them to change to learn something none of them asked for.
///
/// The other reason is that the answer is a constant. Support does not vary per
/// element, so a per-element API would return the same answer wrapped in a
/// varying one, and the temptation to read it off a particular element's style
/// is exactly the mistake being removed.
///
/// Custom properties are not answered here: CSS Variables 1 §2.1 makes every
/// non-empty value of a valid `<dashed-ident>` a valid value, so a custom
/// property is supported whenever its name is one. [`supports_declaration`]
/// already answers that, and it is where a `@supports` condition about one
/// goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PropertySupport {
    /// Neither a grammar nor registry metadata: this engine has no such
    /// property, and a declaration of it is inert. A diagnostic or an oracle
    /// that reports this is telling the truth.
    Unsupported,
    /// Registry metadata only - an inheritance flag and an initial value - and
    /// no grammar. The property is real and the cascade can hold and inherit
    /// it, but this slice cannot parse a value for it, so a declaration is
    /// carried as tokens rather than as a typed value. **This is the state
    /// `ComputedStyle::get` cannot express.**
    MetadataOnly,
    /// A grammar is claimed, so [`parse_typed_property`] returns `Some` and a
    /// value can be rejected as invalid rather than merely unparsed.
    Grammar,
}

/// The honest answer to "does this engine support `property`?", independent of
/// any element.
///
/// Read [`supports_declaration`] for the per-*declaration* question, which adds
/// the value: this function cannot say whether `width: nonsense` is supported,
/// because that is a question about the value rather than the name.
#[must_use]
pub fn property_support(property: &str) -> PropertySupport {
    let name = property.to_ascii_lowercase();
    if DECLARED_GRAMMARS.contains(&name.as_str()) {
        return PropertySupport::Grammar;
    }
    if registry_names().contains(name.as_str()) {
        return PropertySupport::MetadataOnly;
    }
    PropertySupport::Unsupported
}

/// CSS Variables 1 §2.1's `<dashed-ident> = --<custom-ident>`, and
/// `<custom-ident>` excludes `default` and the CSS-wide keywords.
///
/// An identifier cannot start with a digit, and a non-ASCII start is left to
/// cssparser's own tokenizer to accept or reject elsewhere; this only has to
/// catch the ASCII case a bare "not empty" check would let through.
fn is_custom_property_name(name: &str) -> bool {
    let Some(first) = name.chars().next() else {
        return false;
    };
    if first.is_ascii() && !(first.is_ascii_alphabetic() || first == '_' || first == '-') {
        return false;
    }
    !matches!(
        name.to_ascii_lowercase().as_str(),
        "default" | "inherit" | "initial" | "unset" | "revert" | "revert-layer"
    )
}

/// The `<ident>`s of CSS Conditional Rules 5 §2.1.3's named-feature list that
/// this engine implements.
///
/// The list is closed - "If the feature is not listed the processor does not
/// support the named feature" - and it has exactly two entries, neither of which
/// is implemented: anchoring through a transform belongs to `render-layout`'s
/// anchor positioning, and a single-axis scroll container to its overflow.
///
/// The two names are therefore `anchor-position-follows-transforms` and
/// `single-axis-scroll-container`, and neither is implemented, so the list is
/// empty. Naming the answer to that question in one place means the next engine
/// to implement one edits this line and nothing else.
const IMPLEMENTED_NAMED_FEATURES: &[&str] = &[];

/// A boolean, plus every question the condition could not answer.
///
/// Both sides of a conjunction and of a disjunction are always evaluated, so
/// one unanswerable feature cannot hide another behind a short circuit, and the
/// gap list names all of them.
#[derive(Clone, Debug)]
struct Outcome {
    value: bool,
    gaps: Vec<UnansweredFeature>,
}

impl Outcome {
    const fn answered(value: bool) -> Self {
        Self {
            value,
            gaps: Vec::new(),
        }
    }

    fn undecidable(feature: String, reason: &'static str) -> Self {
        Self {
            value: false,
            gaps: vec![UnansweredFeature { feature, reason }],
        }
    }

    /// Combine two terms with `and`, or with `or`. An unanswerable term stays
    /// unanswerable: `not at-rule(@font-face)` is as unknown as its child, not
    /// the opposite of it, and `false and at-rule(@font-face)` cannot be
    /// reported as a plain `false` without hiding the question.
    fn combine(self, other: Self, connective: Connective) -> Self {
        let value = match connective {
            Connective::And => self.value && other.value,
            Connective::Or => self.value || other.value,
        };
        let mut gaps = self.gaps;
        gaps.extend(other.gaps);
        Self { value, gaps }
    }

    fn into_condition(self) -> SupportsCondition {
        if self.gaps.is_empty() {
            SupportsCondition::Evaluated(self.value)
        } else {
            SupportsCondition::Undecidable(self.gaps)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Connective {
    And,
    Or,
}

/// `<supports-condition> = not <supports-in-parens>
///                       | <supports-in-parens> [ and <supports-in-parens> ]*
///                       | <supports-in-parens> [ or <supports-in-parens> ]*`
///
/// The three alternatives are the grammar's three top-level forms. The second
/// and the third cannot both appear at one level, because §6 says so: "the
/// syntax does not allow and, or, and not operators to be mixed without a layer
/// of parentheses", and its own counter-example `@supports (a) or (b) and (c)`
/// is invalid rather than false.
fn parse_condition(input: &mut Parser<'_, '_>) -> Result<Outcome, ()> {
    if let Ok(negated) = input.try_parse(|candidate| -> Result<Outcome, ParseError<'_, ()>> {
        if candidate.expect_ident_matching("not").is_err() {
            return Err(candidate.new_custom_error(()));
        }
        parse_in_parens(candidate).map_err(|()| candidate.new_custom_error(()))
    }) {
        return Ok(Outcome {
            value: !negated.value,
            gaps: negated.gaps,
        });
    }
    let first = parse_in_parens(input)?;
    let mut accumulated: Option<(Connective, Outcome)> = None;
    while let Some(connective) = parse_connective(input) {
        if accumulated
            .as_ref()
            .is_some_and(|(previous, _)| *previous != connective)
        {
            return Err(());
        }
        let right = parse_in_parens(input)?;
        // The first term is the left operand of the first connective, so it is
        // combined rather than replaced: dropping it would both lose its
        // boolean and hide a question it could not answer behind the term that
        // follows it.
        accumulated = Some(match accumulated {
            None => (connective, first.clone().combine(right, connective)),
            Some((previous, left)) => (previous, left.combine(right, connective)),
        });
    }
    Ok(match accumulated {
        None => first,
        Some((_, outcome)) => outcome,
    })
}

/// The `and` or `or` connective, leaving the input untouched when the next
/// token is anything else. A connective is a keyword token here, and §6's
/// requirement of whitespace after the keyword is what stops `and(` from
/// arriving here at all: it tokenizes as a function, not as a keyword and a
/// block.
fn parse_connective(input: &mut Parser<'_, '_>) -> Option<Connective> {
    let state = input.state();
    let Token::Ident(name) = input.next().ok()? else {
        input.reset(&state);
        return None;
    };
    let connective = if name.eq_ignore_ascii_case("and") {
        Some(Connective::And)
    } else if name.eq_ignore_ascii_case("or") {
        Some(Connective::Or)
    } else {
        None
    };
    if connective.is_none() {
        input.reset(&state);
    }
    connective
}

/// `<supports-in-parens> = ( <supports-condition> ) | <supports-feature>
///                        | <general-enclosed>`
///
/// The two alternatives that need a decision are a parenthesised condition and
/// the function features, because a `<supports-decl>` is also a parenthesised
/// thing. The function features come first here so that `selector(...)` is never
/// read as a `<general-enclosed>`, which §6 would have made unconditionally
/// false.
fn parse_in_parens(input: &mut Parser<'_, '_>) -> Result<Outcome, ()> {
    // A function token has to be recognised before the parser is used again,
    // because the token borrows it, so the head of the term is classified into
    // an owned value first.
    enum Head {
        Function(String),
        Block,
        Other,
    }
    let Ok(token) = input.next() else {
        return Err(());
    };
    let head = match token {
        Token::Function(name) => Head::Function(name.to_ascii_lowercase()),
        Token::ParenthesisBlock => Head::Block,
        // A bare ident is none of the three alternatives. §6 calls
        // `@supports display: flex` invalid rather than false, and calls
        // `not display: flex` invalid for the same reason: `not` has to be
        // followed by a `<supports-in-parens>`.
        _ => Head::Other,
    };
    match head {
        Head::Function(name) => parse_feature_function(input, &name),
        Head::Block => input
            .parse_nested_block(parse_paren_contents)
            .map_err(|_: ParseError<'_, ()>| ()),
        Head::Other => Err(()),
    }
}

/// The inside of a `( ... )` block: a nested condition, or a
/// `<supports-decl>`, or a `<general-enclosed>`.
///
/// The condition reading is attempted first and is *not* the general one: for
/// `((a) and (b))` the whole block is the condition and nothing inside it is a
/// declaration, while for `(display: grid)` the condition reading fails on the
/// first token and the declaration reading takes over. `try_parse` restores the
/// input when the callback fails, so the two readings start from the same token.
fn parse_paren_contents<'i, 't>(input: &mut Parser<'i, 't>) -> Result<Outcome, ParseError<'i, ()>> {
    if let Ok(outcome) = input.try_parse(parse_condition) {
        return Ok(outcome);
    }
    let outcome = parse_declaration_feature(input)?;
    // `parse_nested_block` fails the whole call if the closure leaves input
    // behind, and a `<general-enclosed>` may hold any token sequence at all, so
    // the rest of the block is drained here rather than treated as an error. The
    // condition reading above cannot leave anything behind either: it only
    // succeeds when every connective it saw is followed by a term.
    while input.next_including_whitespace_and_comments().is_ok() {}
    Ok(outcome)
}

/// `<supports-decl> = ( [ <declaration> | <supports-condition-name> ] )`, and
/// CSS Conditional Rules 5 §2 widens `<declaration>` to "anything that would be
/// successfully parsed by `consume a declaration`", which includes a trailing
/// `!important` that is then ignored.
///
/// A block that is none of those is a `<general-enclosed>`, whose result §6
/// fixes as false, so it is answered rather than rejected: that production
/// exists so new syntax does not invalidate too much of a condition in an older
/// engine. A bare `<ident>` is a `<supports-condition-name>`, which §2 defines
/// as false when the name is not recognised, and this engine defines none - the
/// same false, reached for the same reason.
fn parse_declaration_feature<'i, 't>(
    input: &mut Parser<'i, 't>,
) -> Result<Outcome, ParseError<'i, ()>> {
    let Ok(Token::Ident(name)) = input.next() else {
        return Ok(Outcome::answered(false));
    };
    let name = name.as_ref().to_owned();
    if !matches!(input.next(), Ok(Token::Colon)) {
        return Ok(Outcome::answered(false));
    }
    let value_start = input.position();
    // The end of the value, which is the end of the block unless a trailing
    // `!important` shortens it. It is only known once the whole block has been
    // consumed: cssparser defers the skip past a block's closing delimiter to
    // the call *after* the block token, so a position taken at the last token
    // stops one delimiter early and would cut `rgb(1, 2, 3)` down to
    // `rgb(1, 2, 3`.
    let mut value_end: Option<SourcePosition> = None;
    let mut stray_bang = false;
    loop {
        let state = input.state();
        if input
            .try_parse(|candidate| {
                candidate.expect_delim('!')?;
                candidate.expect_ident_matching("important")?;
                candidate.expect_exhausted()
            })
            .is_ok()
        {
            value_end = Some(state.position());
            break;
        }
        input.reset(&state);
        match input.next_including_whitespace_and_comments() {
            // "Declaration cannot include semicolon" (WPT `css-supports-039.xht`):
            // a second declaration in the same parentheses is not a declaration.
            Ok(Token::Semicolon) => return Err(input.new_custom_error(())),
            Ok(Token::Delim('!')) => stray_bang = true,
            Ok(_) => {}
            Err(_) => break,
        }
    }
    if stray_bang {
        // A `!` that is not `!important` leaves tokens that are not a
        // declaration (WPT `css-supports-043.xht`), so the condition is invalid
        // rather than merely unmet.
        return Err(input.new_custom_error(()));
    }
    let value_end = value_end.unwrap_or_else(|| input.position());
    let value = input.slice(value_start..value_end).trim();
    // "Declaration value can be empty" (WPT `css-supports-022.xht`): the
    // condition is valid, and no property accepts an empty value, so it is
    // false. `!important` on its own leaves the same empty value.
    if value.is_empty() {
        return Ok(Outcome::answered(false));
    }
    Ok(Outcome::answered(supports_declaration(&name, value)))
}

/// The function features of CSS Conditional Rules 4 §2 and Level 5 §2, and the
/// `<general-enclosed>` that catches everything else.
fn parse_feature_function(input: &mut Parser<'_, '_>, name: &str) -> Result<Outcome, ()> {
    let argument = input
        .parse_nested_block(slice_tokens)
        .map_err(|_: ParseError<'_, ()>| ())?;
    let argument = argument.trim();
    Ok(match name {
        "selector" => Outcome::answered(supports_selector(argument)),
        "at-rule" => at_rule_feature(argument),
        "font-tech" | "font-format" => Outcome::undecidable(
            format!("{name}({argument})"),
            // §2.1.1: "A CSS processor is considered to support a font tech
            // when it is capable of utilizing the specified CSS Fonts 4 §11.1
            // Font tech in layout and rendering." Nothing in this crate can
            // know that, and the two crates that could cannot be reached from
            // here.
            "deciding it needs a font backend that can use a font tech in layout and rendering, \
             which is render-browser's, plus font fetching in render-net; neither exists yet",
        ),
        "named-feature" => Outcome::answered(IMPLEMENTED_NAMED_FEATURES.contains(&argument)),
        // §2.1.5: an environment variable is supported if the `<ident>` names
        // one this engine substitutes. There is no `env()` substitution in the
        // computed-value stage at all, so no environment variable is supported,
        // and that is a statement about the engine rather than a guess. Adding
        // `env()` support has to change this line.
        "env" => Outcome::answered(false),
        // A function that is not one of the features is not a
        // `<general-enclosed>` either: §6 defines that production as
        // `[ <any-token> ]? ')'`, so it is a parenthesised thing, not a
        // function. Nothing here matches, so the whole rule is invalid. This
        // is also what makes `not(...)` invalid, which §6 requires: the
        // whitespace after `not` is what stops the function token forming.
        _ => return Err(()),
    })
}

/// `<supports-at-rule-fn> = at-rule( <at-keyword-token> )`.
fn at_rule_feature(argument: &str) -> Outcome {
    let Some(name) = argument.strip_prefix('@') else {
        // Not an `<at-keyword-token>`, so the feature does not parse and the
        // term is a `<general-enclosed>`: false, and the rule stays valid.
        return Outcome::answered(false);
    };
    let name = name.trim();
    if name.is_empty() {
        return Outcome::answered(false);
    }
    let name = name.to_ascii_lowercase();
    match at_rule_support(&name) {
        AtRuleSupport::Evaluated => Outcome::answered(true),
        AtRuleSupport::Unrecognised => Outcome::answered(false),
        AtRuleSupport::Unimplemented => Outcome::undecidable(
            format!("at-rule(@{name})"),
            unimplemented_at_rule_reason(&name),
        ),
    }
}

/// Why this crate cannot answer `at-rule(@x)` for a recognised at-rule it does
/// not evaluate, naming the capability that would have to exist first.
///
/// §2.1.2 defines support for an at-rule as whether the processor "would accept
/// an at-rule beginning with the specified at-keyword within any context", and
/// this crate does accept one: it parses the prelude and the block. Answering
/// `true` from that would make a syntactic test stand in for a semantic one -
/// `at-rule(@font-face)` would report that webfonts work, on a page whose 223
/// `@font-face` blocks are all dropped with no font ever loaded, and the lie
/// becomes visible the moment a font backend lands. So the answer belongs to
/// whoever owns the missing capability, and this crate says so instead.
fn unimplemented_at_rule_reason(name: &str) -> &'static str {
    match name {
        "font-face" => {
            "a @font-face rule needs its src fetched by render-net and its face \
             registered with a font backend that selects by family and weight in render-browser; \
             this crate can only parse the descriptors"
        }
        "keyframes" => {
            "@keyframes needs an animation clock and a paint pipeline that can be \
             re-run with an interpolated value, which are render-core's and render-browser's"
        }
        _ => {
            "the at-rule is recognised and parsed here but nothing evaluates it, and whatever \
             would evaluate it is outside this crate"
        }
    }
}

/// `<supports-selector-fn> = selector( <complex-selector> )`.
///
/// §2.1: "A CSS processor is considered to support a CSS selector if it accepts
/// that all aspects of that selector, recursively." The selector parser is the
/// one a style rule's prelude goes through, so this is a real answer and not an
/// approximation: a selector it accepts is one the engine can match, and one it
/// rejects is one it cannot.
///
/// The one thing §2.1 asks for that this cannot see is the forgiving parsing of
/// some functional selectors - "if some arguments are unknown/invalid, the
/// selector itself is not invalidated. These are nonetheless unsupported" - which
/// is invisible from inside a parser that already accepted the selector. The
/// failure direction is the safe one, because a selector this parser rejects is
/// reported as unsupported, which is a browser with more selector support also
/// reporting a gap rather than a page losing a rule.
fn supports_selector(source: &str) -> bool {
    let Ok(list) = parse_selector_list(source) else {
        return false;
    };
    // The production takes one `<complex-selector>`, so a comma-separated list
    // is not the argument.
    list.selectors().len() == 1
}

fn slice_tokens<'i, 't>(input: &mut Parser<'i, 't>) -> Result<String, ParseError<'i, ()>> {
    let start = input.position();
    while input.next_including_whitespace_and_comments().is_ok() {}
    Ok(input.slice_from(start).to_owned())
}

#[cfg(test)]
mod tests {
    use super::{
        SupportsCondition, evaluate_supports_condition, supports_condition_text,
        supports_declaration,
    };

    /// The answer as a plain bool, so a test about the oracle's *value* does not
    /// have to spell out the enum, and a test about the third state has to.
    fn evaluates(source: &str) -> bool {
        match evaluate_supports_condition(source) {
            SupportsCondition::Evaluated(value) => value,
            other => panic!("{source:?} should be a complete answer, got {other:?}"),
        }
    }

    fn is_invalid(source: &str) -> bool {
        matches!(
            evaluate_supports_condition(source),
            SupportsCondition::Invalid
        )
    }

    /// CSS Conditional Rules 3 §6.1 defines support for a declaration, and
    /// every answer comes from the same functions the cascade reads. The
    /// conditions here are the ones real sheets write, and the expected answers
    /// are what this engine actually does with the declaration.
    #[test]
    fn a_declaration_condition_is_answered_by_the_declaration_oracle() {
        // Typed grammars the engine claims and the values it accepts.
        assert!(evaluates("(display: grid)"));
        assert!(evaluates("(display: flex)"));
        assert!(evaluates("(color: rgb(1, 2, 3))"));
        assert!(evaluates("(color: color(display-p3 0 0 0%))"));
        assert!(evaluates("(aspect-ratio: 16 / 9)"));
        assert!(evaluates("(transform: translate(1px))"));
        // The engine's own `position` grammar accepts `sticky`, so the answer is
        // true, and it comes from the same function the computed-value stage
        // uses. Whether layout honours a sticky box is a different question, in
        // a different crate, and it is not this one's to answer.
        assert!(evaluates("(position: sticky)"));
        // A claimed grammar and a value it rejects: the declaration is discarded
        // as a parse error, so §6.1 says it is not supported. `lab()` is the
        // rejection 91 `@supports` blocks in the production corpus test for.
        assert!(!evaluates(
            "(color: lab(from red l 1 1%/calc(alpha + 0.1)))"
        ));
        assert!(!evaluates("(display: nonsense)"));
        // A property with no grammar but with metadata, which the engine does
        // read: supported.
        assert!(evaluates("(font-size: 1rem)"));
        assert!(evaluates("(letter-spacing: normal)"));
        assert!(evaluates("(z-index: auto)"));
        // A property with neither a grammar nor metadata, which is the case a
        // real page uses to detect a feature it does not implement.
        assert!(!evaluates("(backdrop-filter: blur(2px))"));
        assert!(!evaluates("(-moz-box-shadow: 0 0 2px black)"));
        assert!(!evaluates("(text-overflow: ellipsis)"));
        // `font-family` is the sharpest of those: `render-layout`'s `TextStyle`
        // carries no family, so no engine grammar for it exists at all.
        assert!(!evaluates("(font-family: Arial)"));
        // An empty value is a valid condition and an unsupported one.
        assert!(!evaluates("(display:)"));
    }

    /// §6.1: "implementations must implement all parts of the value in order to
    /// consider the declaration supported." A shorthand has no grammar of its
    /// own, so the answer has to be the conjunction over its longhands - which
    /// is why a shorthand can be supported while one of its own spellings is
    /// not.
    #[test]
    fn shorthand_support_is_the_conjunction_over_its_longhands() {
        assert!(evaluates("(border: 1px solid red)"));
        assert!(evaluates("(border: 1px dotted red)"));
        // `solid` and `currentcolor` are values of the `-style` and `-color`
        // longhands, and all twelve `border` longhands have grammars here.
        assert!(evaluates("(border: 1px solid)"));
        assert!(evaluates("(border: medium none currentcolor)"));
        assert!(evaluates("(margin: 1px 2px)"));
        assert!(evaluates("(overflow: hidden auto)"));
        assert!(evaluates("(flex: 1 1 auto)"));
        assert!(evaluates("(gap: 1px 2px)"));
        assert!(evaluates("(grid-row: 1 / 3)"));
        // `font` expands to a `font-family` this engine has no grammar and no
        // metadata for, so the compound value is not fully implemented and the
        // whole declaration is unsupported. A browser with a font backend says
        // true; saying so here would be claiming a capability this crate cannot
        // see, which is the direction the project's law forbids.
        assert!(!evaluates("(font: 12px/1.5 Arial)"));
        // A shorthand whose value cannot be read at all expands to nothing, and
        // an empty conjunction must not be vacuously true.
        assert!(!evaluates("(font: 12px)"));
    }

    /// The three top-level forms of `<supports-condition>`, and the rule that
    /// the second and third cannot be mixed at one level.
    #[test]
    fn the_three_top_level_forms_evaluate_and_mixing_them_does_not() {
        // Form one: `not <supports-in-parens>`.
        assert!(!evaluates("not (display: grid)"));
        assert!(evaluates("not (backdrop-filter: blur(2px))"));
        // Form two: a conjunction.
        assert!(evaluates("(display: grid) and (color: red)"));
        assert!(!evaluates(
            "(display: grid) and (backdrop-filter: blur(2px))"
        ));
        // Form three: a disjunction.
        assert!(evaluates("(backdrop-filter: blur(2px)) or (display: grid)"));
        assert!(!evaluates(
            "(backdrop-filter: blur(2px)) or (text-overflow: clip)"
        ));
        // Mixing them at one level is invalid, which is §6's own counter-example,
        // and the spec's prescribed rewrite is legal.
        assert!(is_invalid(
            "(display: grid) or (color: red) and (font-size: 1rem)"
        ));
        assert!(is_invalid(
            "(display: grid) and (color: red) or (font-size: 1rem)"
        ));
        assert!(evaluates(
            "((backdrop-filter: blur(2px)) or (display: grid)) and (color: red)"
        ));
        assert!(evaluates(
            "(display: grid) or ((backdrop-filter: blur(2px)) and (color: red))"
        ));
    }

    /// §6 fixes `<general-enclosed>` at false and, crucially, calls it valid:
    /// "The result is false. Authors must not use `<general-enclosed>` in their
    /// stylesheets. It exists only for future-compatibility." So an
    /// unrecognised-but-grammatical condition is a complete `false`, never a
    /// diagnostic and never an invalid rule.
    #[test]
    fn general_enclosed_is_false_and_the_rule_stays_valid() {
        assert!(!evaluates("(some future feature)"));
        assert!(!evaluates("(some-future-feature: 1)"));
        assert!(!evaluates("(42)"));
        assert!(!evaluates("(a b c)"));
        // An empty pair of parentheses is the production with its one optional
        // token absent, and `not ()` is therefore a true condition.
        assert!(!evaluates("()"));
        assert!(evaluates("not ()"));
        // A named condition this engine does not define is false, per Level 5
        // §2, and lands in the same place.
        assert!(!evaluates("(some-named-condition)"));
        // And it composes, which is the point of the production.
        assert!(evaluates("(some future feature) or (display: grid)"));
        assert!(!evaluates("(some future feature) and (display: grid)"));
    }

    /// The forms §6 calls invalid, against the "must be true" assertions in
    /// Conditional Rules 3's own Tests blocks. An invalid condition drops the
    /// whole rule, so mistaking one for a false or a true is how a page loses
    /// both halves of a progressive enhancement.
    #[test]
    fn invalid_conditions_are_reported_rather_than_guessed() {
        // "The declaration being tested must always occur within parentheses."
        assert!(is_invalid("display: flex"));
        // "Not must be followed by space": `not(...)` tokenizes as a function
        // token, which matches no production.
        assert!(is_invalid("not(display: grid)"));
        // "Not requires parentheses."
        assert!(is_invalid("not display: grid"));
        assert!(is_invalid("not"));
        // "And requires parentheses", "Or requires parentheses".
        assert!(is_invalid("(display: grid) and"));
        assert!(is_invalid("(display: grid) and (color: red) and"));
        assert!(is_invalid("(display: grid) or"));
        // "Declaration cannot include invalid !tokens."
        assert!(is_invalid("(color: red !importantish)"));
        // "Declaration cannot include semicolon."
        assert!(is_invalid("(display: flex; color: red)"));
        // An unknown function matches no production either: §6's
        // `<general-enclosed>` is `[ <any-token> ]? ')'`, a parenthesised thing,
        // not a function.
        assert!(is_invalid("somethingnew(1)"));
        // An empty condition matches no production.
        assert!(is_invalid(""));
        assert!(is_invalid("   "));
    }

    /// CSS Conditional Rules 4 §2's `selector()`, answered by the selector
    /// parser rather than guessed. `selector()` is in the production the
    /// specification added, so it is a question this engine can pose, and the
    /// answer is the same answer a style rule's prelude gets.
    #[test]
    fn selector_is_answered_by_the_selector_parser() {
        assert!(evaluates("selector(a > b)"));
        assert!(evaluates("selector(:is(.x, .y))"));
        assert!(evaluates("selector(:has(> img))"));
        assert!(evaluates("selector(:where(a))"));
        // The spec's own example of the feature, the column combinator. This
        // engine's selector parser has no `||`, so the honest answer is false
        // and the rule does not apply - which is the point of the feature, and
        // which a hardcoded `true` would have got exactly backwards.
        assert!(!evaluates("selector(col || td)"));
        // Selectors this engine's parser does not accept are unsupported, which
        // is a real answer and not a fallback.
        assert!(!evaluates("selector(a b c d e ! ! ! f)"));
        assert!(!evaluates("selector(&&)"));
        // The production takes one `<complex-selector>`, not a list, and not
        // nothing.
        assert!(!evaluates("selector(a, b)"));
        assert!(!evaluates("selector()"));
        // No `selector()` condition is ever undecidable, which is what makes the
        // third state mean something where it is used.
        assert!(evaluate_supports_condition("selector(a)").is_answered());
        assert!(evaluate_supports_condition("selector(col || td)").is_answered());
    }

    /// CSS Conditional Rules 5 §2's `at-rule()`, `font-tech()` and
    /// `font-format()`: the two states this crate can answer and the one it
    /// cannot. The `at-rule` cases that matter most are `@font-face` and
    /// `@keyframes`, because reporting them as supported is precisely the lie
    /// this project forbids.
    #[test]
    fn at_rule_and_font_features_are_answered_only_where_they_can_be() {
        // The at-rules whose contents reach the cascade.
        assert!(evaluates("at-rule(@media)"));
        assert!(evaluates("at-rule(@supports)"));
        assert!(evaluates("at-rule(@layer)"));
        // An at-rule no specification defines.
        assert!(!evaluates("at-rule(@nonesuch)"));
        // Not an `<at-keyword-token>`, so the feature does not parse and the
        // term is a `<general-enclosed>`.
        assert!(!evaluates("at-rule(media)"));
        assert!(!evaluates("at-rule()"));
        // The two that must NOT be answered here.
        for source in [
            "at-rule(@font-face)",
            "at-rule(@keyframes)",
            "at-rule(@-webkit-keyframes)",
            "at-rule(@page)",
            "font-tech(color-COLRv1)",
            "font-format(woff2)",
        ] {
            let SupportsCondition::Undecidable(gaps) = evaluate_supports_condition(source) else {
                panic!("{source:?} must not be answered from this crate");
            };
            assert_eq!(gaps.len(), 1, "{source:?} names one feature");
            assert!(!gaps[0].reason.is_empty(), "{source:?} must say why");
        }
        // The reason has to name the capability, not just the refusal.
        let SupportsCondition::Undecidable(gaps) =
            evaluate_supports_condition("at-rule(@font-face)")
        else {
            unreachable!()
        };
        assert!(gaps[0].reason.contains("render-net"), "{}", gaps[0].reason);
        assert!(
            gaps[0].reason.contains("render-browser"),
            "{}",
            gaps[0].reason
        );
    }

    /// One unanswerable feature makes the whole condition unanswerable, and it
    /// does so even when the boolean would have been decidable without it. A
    /// short circuit that dropped the question would be a guess in the
    /// direction that silently discards CSS.
    #[test]
    fn one_unanswerable_feature_makes_the_whole_condition_unanswerable() {
        for source in [
            "(display: grid) and at-rule(@font-face)",
            "at-rule(@font-face) or (display: grid)",
            "not at-rule(@font-face)",
            "((display: grid) and at-rule(@font-face)) or (color: red)",
        ] {
            assert!(
                !evaluate_supports_condition(source).is_answered(),
                "{source:?} contains a feature this crate cannot answer"
            );
        }
        // Every gap is reported, not just the first one: both sides of a
        // conjunction are evaluated.
        let SupportsCondition::Undecidable(gaps) =
            evaluate_supports_condition("at-rule(@font-face) and font-tech(variations)")
        else {
            panic!("two unanswerable features must both be reported");
        };
        assert_eq!(gaps.len(), 2);
        assert_eq!(gaps[0].feature, "at-rule(@font-face)");
        assert_eq!(gaps[1].feature, "font-tech(variations)");
    }

    /// Level 5 §2 widens `<declaration>` to anything `consume a declaration`
    /// accepts, so a trailing `!important` is valid and ignored, and a comment is
    /// not part of the value. §6's own note on the connectives says the same:
    /// "white space--or a comment--is still required after these keywords, since
    /// without it they and the ensuing opening parenthesis will be tokenized as
    /// a function opening token", which makes `and(` invalid and `and /*c*/ (`
    /// valid.
    #[test]
    fn important_is_allowed_and_comments_are_not_part_of_the_condition() {
        assert!(evaluates("(display: flex !important)"));
        assert!(evaluates("(display: /* the point */ grid)"));
        assert!(evaluates("(display:grid) /*c*/ and /*c*/ (color:red)"));
        // Without the whitespace or the comment the `(` is part of the token.
        assert!(is_invalid("(display:grid)and(color:red)"));
        assert!(is_invalid("(display:grid)/*c*/and(color:red)"));
        // An `!important` the engine cannot read as a value is still false, not
        // invalid, because `!important` is explicitly ignored.
        assert!(!evaluates("(backdrop-filter: blur(2px) !important)"));
    }

    /// §7.5's `CSS.supports(conditionText)`: the wrapped retry is a real
    /// second parse, not the first answer reused.
    #[test]
    fn supports_condition_text_implies_parentheses() {
        assert!(supports_condition_text("(display: grid)"));
        assert!(supports_condition_text("display: grid"));
        assert!(supports_condition_text("not (backdrop-filter: blur(2px))"));
        assert!(!supports_condition_text("not (display: grid)"));
        assert!(!supports_condition_text("not display: grid"));
        assert!(!supports_condition_text("backdrop-filter: blur(2px)"));
        // An undecidable condition is not true, and wrapping it does not make
        // it true either.
        assert!(!supports_condition_text("at-rule(@font-face)"));
    }

    /// The declaration oracle on its own, so the property and value rules are
    /// pinned independently of the grammar around them.
    #[test]
    fn the_declaration_oracle_answers_from_the_engine_not_from_a_table() {
        // A custom property keeps its whole token stream, so any non-empty
        // value is valid and only the name can reject it.
        assert!(supports_declaration("--theme", "1px solid red"));
        assert!(supports_declaration("--Theme", "1px solid red"));
        assert!(supports_declaration("--theme", "{ color: red }"));
        assert!(!supports_declaration("--theme", ""));
        assert!(!supports_declaration("--", "1px"));
        // `--default` and the CSS-wide spellings are excluded by
        // `<custom-ident>`.
        assert!(!supports_declaration("--default", "1px"));
        assert!(!supports_declaration("--initial", "1px"));
        assert!(!supports_declaration("--revert-layer", "1px"));
        // A property name is required.
        assert!(!supports_declaration("", "red"));
        assert!(!supports_declaration("   ", "red"));
    }

    /// The two halves of the project's law, on the same oracle, so the property
    /// that reporting a gap depends on is pinned: supported CSS is never
    /// reported as unsupported, and unsupported CSS is never claimed.
    #[test]
    fn the_oracle_reports_both_directions_the_way_the_law_requires() {
        // Everything the engine accepts in a style rule is supported.
        for (property, value) in [
            ("display", "grid"),
            ("display", "flex"),
            ("color", "rgb(1, 2, 3)"),
            ("position", "sticky"),
            ("font-size", "1rem"),
            ("z-index", "auto"),
            ("--custom", "anything at all"),
        ] {
            assert!(
                supports_declaration(property, value),
                "{property}: {value} is accepted in a style rule, so it is supported"
            );
        }
        // Nothing the engine discards as a parse error is supported.
        for (property, value) in [
            ("display", "nonsense"),
            ("color", "lab(from red l 1 1%/calc(alpha + 0.1))"),
            ("backdrop-filter", "blur(2px)"),
            ("text-overflow", "clip"),
            ("-moz-box-shadow", "0 0 2px black"),
            ("nonesuch", "1px"),
        ] {
            assert!(
                !supports_declaration(property, value),
                "{property}: {value} is not accepted in a style rule, so it is unsupported"
            );
        }
    }
}
