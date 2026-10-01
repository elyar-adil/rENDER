//! CSS Syntax based stylesheet and declaration parsing.
//!
//! This module owns syntax recovery and produces selector ASTs once. Property
//! grammar validation and computed-value resolution belong to later stages.

use std::fmt;

use cssparser::{
    AtRuleParser, CowRcStr, DeclarationParser, Delimiter, ParseError, ParseErrorKind, Parser,
    ParserInput, ParserState, QualifiedRuleParser, SourceLocation, StyleSheetParser, Token,
};

use super::at_rules::{at_rule_block_is_declaration_list, at_rule_diagnostic};
use super::selector::{
    NestedSelectors, SelectorList, parse_nested_selector_list, parse_selector_list,
};
use super::supports::{SupportsCondition, evaluate_supports_condition};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CssWideKeyword {
    Inherit,
    Initial,
    Unset,
    Revert,
    RevertLayer,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum LayerName {
    Named(Vec<String>),
    Anonymous(u32),
}

/// Where a declaration starts in the stylesheet it was parsed from.
///
/// The same shape and the same one-based-line convention as
/// [`StyleSheetDiagnostic`], and the same reason: a report that names a file
/// position has to write it down one way or a reader has to check two. Line 0 is
/// the start of the input, so a parsed position never has line 0.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeclarationLocation {
    /// One-based line of the declaration's first token.
    pub line: u32,
    /// Zero-based column, as cssparser counts them and as
    /// [`StyleSheetDiagnostic::column`] already is.
    pub column: u32,
}

impl From<SourceLocation> for DeclarationLocation {
    /// The one conversion, so every consumer of a cssparser position gets the
    /// same one-based line. The `saturating_add` is what §5.4.4's own
    /// diagnostics do: cssparser's line 0 is the first line of the input, which
    /// a reader calls line 1.
    fn from(location: SourceLocation) -> Self {
        Self {
            line: location.line.saturating_add(1),
            column: location.column,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Declaration {
    pub name: String,
    pub value: String,
    pub important: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StyleRule {
    pub selectors: SelectorList,
    /// The declarations this rule contributes, in source order.
    pub declarations: Vec<Declaration>,
    /// Where each entry of `declarations` starts in the stylesheet it was parsed
    /// from, in the same order and always the same length.
    ///
    /// It is a parallel vector rather than a field on [`Declaration`] because
    /// `Declaration` is built as a struct literal by `render-core`
    /// (`src/document.rs`, for presentational hints and quirks margins), so a new
    /// field on it would stop that crate compiling. What a declaration is stays
    /// three strings and a flag; where it came from travels beside it, and
    /// [`StyleRule::declaration_at`] hands the two out together so a consumer
    /// that reports a rejected declaration can name a `file:line`.
    pub declaration_locations: Vec<DeclarationLocation>,
    pub layer: Option<LayerName>,
    /// Every enclosing `@media` query, from outermost to innermost.
    pub media: Vec<String>,
    pub source_order: u64,
}

impl StyleRule {
    /// The declaration at `index` and where it starts, or `None` when the index
    /// is out of range. A rule whose two vectors ever disagreed would report
    /// `None` for the missing tail rather than a wrong position, so
    /// `declarations.len() == declaration_locations.len()` is the invariant this
    /// is written against.
    #[must_use]
    pub fn declaration_at(&self, index: usize) -> Option<(&Declaration, DeclarationLocation)> {
        Some((
            self.declarations.get(index)?,
            *self.declaration_locations.get(index)?,
        ))
    }

    /// Where the last declaration naming `property` starts, for reporting a
    /// declaration the cascade rejected.
    #[must_use]
    pub fn location_of(&self, property: &str) -> Option<DeclarationLocation> {
        self.declaration_at(self.index_of(property)?)
            .map(|(_, location)| location)
    }

    /// The index of the last declaration naming `property`, because a later
    /// declaration of the same property in one block is the one that wins and
    /// therefore the one whose value the cascade would have used.
    fn index_of(&self, property: &str) -> Option<usize> {
        self.declarations
            .iter()
            .rposition(|declaration| declaration.name.eq_ignore_ascii_case(property))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StyleSheetDiagnostic {
    pub line: u32,
    pub column: u32,
    pub message: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StyleSheet {
    pub rules: Vec<StyleRule>,
    pub layer_order: Vec<LayerName>,
    pub diagnostics: Vec<StyleSheetDiagnostic>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RuleParseError {
    InvalidSelector(String),
    InvalidLayerName,
    /// CSS Syntax §5.4.6: "if the next input token is anything other than a
    /// `<colon-token>`, this is a parse error. Return nothing." The token that
    /// turned up instead is named, and it is genuinely the token at fault, so
    /// this is cssparser's own wording rather than a new one - what changes is
    /// the *position*, which is now the start of the declaration instead of
    /// wherever the failure was noticed.
    UnexpectedToken(String),
    /// CSS Syntax §5.4.4/§5.4.5's closing case: the item cannot be read as a
    /// declaration and cannot be read as a rule either, so it is a bad
    /// declaration and §5.4.4 throws out that one declaration and starts fresh
    /// at the next one in the same block.
    ///
    /// The legacy star hack (`*zoom: 1`) lands here, and the message has to say
    /// so. Blaming the `;` that ended the declaration - which is what
    /// cssparser's `RuleBodyParser` does, because its `expect_curly_bracket_block`
    /// runs before the prelude's own error is returned - points a reader at a
    /// token that is entirely valid and up to fifteen bytes past the defect.
    InvalidDeclaration(String),
    /// An at-rule this crate's own parser could not name. Every
    /// `AtRuleParser::rule_without_block` in this file returns a `ParsedRule` for
    /// every at-rule it is handed, because `Err(())` there means cssparser
    /// discards the construct and reports nothing at all. Kept as a real error
    /// rather than a panic so that a future at-rule cannot quietly reintroduce
    /// the silent drop.
    UnnamedAtRule,
}

impl fmt::Display for RuleParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSelector(message) => write!(formatter, "invalid selector: {message}"),
            Self::InvalidLayerName => formatter.write_str("invalid cascade layer name"),
            Self::UnexpectedToken(token) => write!(formatter, "unexpected token: {token}"),
            Self::InvalidDeclaration(message) => {
                write!(formatter, "invalid declaration: {message}")
            }
            Self::UnnamedAtRule => {
                formatter.write_str("this at-rule was discarded without a reason")
            }
        }
    }
}

/// One thing thrown out of a declaration list, and where the discarded
/// declaration *starts*.
///
/// The location is the declaration's first token on purpose. §5.4.4 and §5.4.5
/// both discard the bad declaration and resume at the next one, so the thing
/// worth pointing a reader at is the declaration they have to delete, not
/// whatever token the failure happened to be noticed at.
type DeclarationDiagnostic = (RuleParseError, SourceLocation);

#[derive(Clone, Debug)]
enum ParsedRule {
    Style {
        selectors: SelectorList,
        declarations: Vec<SourcedDeclaration>,
        /// Nested style rules and nested group rules, in source order
        /// (`CSS Nesting §3.4`).
        nested: Vec<ParsedRule>,
        diagnostics: Vec<StyleSheetDiagnostic>,
    },
    /// A run of declarations that CSS Nesting §5 wraps in a nested
    /// declarations rule: it matches exactly what its parent style rule
    /// matches, with the same specificity, but is considered to come after it.
    NestedDeclarations {
        selectors: SelectorList,
        declarations: Vec<SourcedDeclaration>,
    },
    LayerStatement {
        layers: Vec<LayerName>,
        location: SourceLocation,
    },
    LayerBlock {
        layer: LayerName,
        rules: Vec<Self>,
        diagnostics: Vec<StyleSheetDiagnostic>,
        location: SourceLocation,
    },
    MediaBlock {
        query: String,
        body: RuleBody,
        diagnostics: Vec<StyleSheetDiagnostic>,
    },
    SupportsBlock {
        query: String,
        body: RuleBody,
        diagnostics: Vec<StyleSheetDiagnostic>,
        location: SourceLocation,
    },
    /// Animation/font at-rules are valid stylesheet rules even when the
    /// compositor does not yet sample their timelines. Keep them in the
    /// parsed rule stream so they do not poison the rest of the stylesheet
    /// with false syntax diagnostics.
    KeyframesBlock {
        name: String,
        /// The at-keyword as written, so the diagnostic quotes the spelling the
        /// author used: `@-webkit-keyframes` is not `@keyframes` to someone
        /// grepping their stylesheet.
        at_keyword: String,
        location: SourceLocation,
    },
    /// An at-rule whose block is a `<declaration-list>`: `@font-face`'s
    /// descriptors, `@page`'s margin properties, and the rest of
    /// [`at_rule_block_is_declaration_list`]. The engine reads the list and
    /// cannot use any of it, so the block is dropped for a reported reason.
    ///
    /// The list's own diagnostics travel with it for the same reason they do
    /// for `@keyframes`: a syntax error found inside a block that is then
    /// discarded is a fact that must not disappear. `@font-face` alone was
    /// walked this way, which left a `@page` block full of legacy hacks
    /// reporting nothing at all - the partial drop one level down.
    DeclarationListBlock {
        /// The at-keyword as written, so the diagnostic quotes the spelling the
        /// author used: `@-ms-viewport` is not `@viewport`.
        at_keyword: String,
        location: SourceLocation,
        diagnostics: Vec<StyleSheetDiagnostic>,
    },
    /// A statement at-rule whose prelude does not parse: `@layer;`, `@layer ;`,
    /// `@layer a.;`. CSS Syntax §4.2 discards it, and the only thing there is to
    /// report is the at-keyword. Pointing at the `;` instead would blame a
    /// token that is entirely valid, and the malformed prelude is what the
    /// author has to fix.
    InvalidLayerStatement { location: SourceLocation },
    /// A `@layer` block nested inside a style rule. `@layer` is recognised and
    /// evaluated at the top level, so this is the *partially* implemented case:
    /// the generic "not evaluated yet" wording would claim the engine has never
    /// heard of `@layer`, which is false and sends a reader looking for the
    /// wrong bug.
    NestedLayerBlock { location: SourceLocation },
    /// An at-rule whose block was consumed and then discarded: either
    /// recognised-but-unimplemented or not an at-rule any specification
    /// defines. The two are told apart by `at_rules::at_rule_support`, so the
    /// parse tree does not have to carry the distinction.
    IgnoredAtRule {
        name: String,
        location: SourceLocation,
    },
}

struct AtPrelude {
    name: String,
    value: String,
}

/// What a group at-rule's block turned out to hold.
#[derive(Clone, Debug)]
enum RuleBody {
    /// A `<rule-list>` (CSS Syntax §5.4.4): only style rules.
    Rules(Vec<ParsedRule>),
    /// A `<block-contents>` (CSS Syntax §5.4.5), which is what CSS Nesting
    /// §3.3 makes a *nested* group rule's block: nested style rules, plus the
    /// declarations that belong to the nearest enclosing style rule. Those
    /// declarations are already wrapped in a nested declarations rule, so this
    /// is the same list a style rule's `nested` field holds.
    Block(Vec<ParsedRule>),
}

/// The nearest enclosing style rule, which is what a nested rule's nesting
/// selector refers to (`CSS Nesting §4`).
#[derive(Clone, Debug)]
struct ParentRule {
    /// The prelude with this level's `&` already resolved, so a deeper level
    /// stacks on top of it.
    text: String,
    selectors: SelectorList,
}

struct RuleParser<'a> {
    next_anonymous_layer: &'a mut u32,
    /// `Some` when this parser is walking a block nested inside a style rule.
    parent: Option<ParentRule>,
}

impl<'i> QualifiedRuleParser<'i> for RuleParser<'_> {
    type Prelude = NestedSelectors;
    type QualifiedRule = ParsedRule;
    type Error = RuleParseError;

    fn parse_prelude<'t>(
        &mut self,
        input: &mut Parser<'i, 't>,
    ) -> Result<Self::Prelude, ParseError<'i, Self::Error>> {
        let selector_text = consume_raw(input);
        let resolved = match self.parent.as_ref() {
            Some(parent) => parse_nested_selector_list(&selector_text, &parent.text),
            None => parse_selector_list(&selector_text).map(|selectors| NestedSelectors {
                css: selector_text,
                selectors,
            }),
        };
        resolved.map_err(|error| {
            input.new_custom_error(RuleParseError::InvalidSelector(error.to_string()))
        })
    }

    fn parse_block<'t>(
        &mut self,
        resolved: Self::Prelude,
        _start: &ParserState,
        input: &mut Parser<'i, 't>,
    ) -> Result<Self::QualifiedRule, ParseError<'i, Self::Error>> {
        // A style rule is the parent of everything nested in its own block, so
        // its resolved prelude is what a nested `&` resolves against.
        let parent = ParentRule {
            text: resolved.css,
            selectors: resolved.selectors,
        };
        let (declarations, nested, diagnostics) =
            parse_block_contents(input, self.next_anonymous_layer, Some(&parent), true);
        Ok(ParsedRule::Style {
            selectors: parent.selectors,
            declarations,
            nested,
            diagnostics,
        })
    }
}

impl<'i> AtRuleParser<'i> for RuleParser<'_> {
    type Prelude = AtPrelude;
    type AtRule = ParsedRule;
    type Error = RuleParseError;

    fn parse_prelude<'t>(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i, 't>,
    ) -> Result<Self::Prelude, ParseError<'i, Self::Error>> {
        Ok(AtPrelude {
            name: name.to_ascii_lowercase(),
            value: consume_raw(input),
        })
    }

    fn rule_without_block(
        &mut self,
        prelude: Self::Prelude,
        start: &ParserState,
    ) -> Result<Self::AtRule, ()> {
        let location = start.source_location();
        if prelude.name == "layer" {
            return match parse_layer_names(&prelude.value) {
                Ok(layers) if !layers.is_empty() => {
                    Ok(ParsedRule::LayerStatement { layers, location })
                }
                // `@layer;` is discarded by CSS Syntax §4.2, and reported. The
                // alternative - `Err(())` - makes cssparser substitute its own
                // "unexpected token: Semicolon", which blames a valid `;` and
                // loses the fact that the layer name is what is malformed.
                _ => Ok(ParsedRule::InvalidLayerStatement { location }),
            };
        }
        Ok(ParsedRule::IgnoredAtRule {
            name: prelude.name,
            location,
        })
    }

    fn parse_block<'t>(
        &mut self,
        prelude: Self::Prelude,
        start: &ParserState,
        input: &mut Parser<'i, 't>,
    ) -> Result<Self::AtRule, ParseError<'i, Self::Error>> {
        parse_at_rule_block(
            prelude,
            start,
            input,
            self.next_anonymous_layer,
            self.parent.as_ref(),
        )
    }
}

/// Parse one at-rule's block. Shared by a top-level at-rule and by one nested
/// inside a style rule, so the meaning of an at-rule cannot drift between the
/// two; only the shape of a group rule's body differs, and that is decided by
/// `parent`.
fn parse_at_rule_block<'i, 't>(
    prelude: AtPrelude,
    start: &ParserState,
    input: &mut Parser<'i, 't>,
    next_anonymous_layer: &mut u32,
    parent: Option<&ParentRule>,
) -> Result<ParsedRule, ParseError<'i, RuleParseError>> {
    let location = start.source_location();
    if prelude.name == "layer" {
        if parent.is_some() {
            // A nested cascade layer would need the enclosing selector folded
            // into its name, which this engine's layer model does not express.
            consume_raw(input);
            return Ok(ParsedRule::NestedLayerBlock { location });
        }
        let layer = if prelude.value.trim().is_empty() {
            let id = *next_anonymous_layer;
            *next_anonymous_layer = id.saturating_add(1);
            LayerName::Anonymous(id)
        } else {
            let mut layers = parse_layer_names(&prelude.value)
                .map_err(|()| input.new_custom_error(RuleParseError::InvalidLayerName))?;
            if layers.len() != 1 {
                return Err(input.new_custom_error(RuleParseError::InvalidLayerName));
            }
            layers.remove(0)
        };
        let (rules, diagnostics) = parse_rule_list(input, next_anonymous_layer);
        return Ok(ParsedRule::LayerBlock {
            layer,
            rules,
            diagnostics,
            location,
        });
    }

    if prelude.name == "media" {
        let (body, diagnostics) = parse_group_body(input, next_anonymous_layer, parent);
        return Ok(ParsedRule::MediaBlock {
            query: prelude.value,
            body,
            diagnostics,
        });
    }

    if prelude.name.ends_with("keyframes") {
        consume_raw(input);
        return Ok(ParsedRule::KeyframesBlock {
            name: prelude.value.trim().to_owned(),
            at_keyword: prelude.name,
            location,
        });
    }

    if prelude.name == "supports" {
        let (body, diagnostics) = parse_group_body(input, next_anonymous_layer, parent);
        return Ok(ParsedRule::SupportsBlock {
            query: prelude.value,
            body,
            diagnostics,
            location,
        });
    }
    // Every recognised at-rule whose block is a `<declaration-list>` is walked
    // the same way, not just `@font-face`. The declarations are dropped either
    // way - nothing in this engine reads a descriptor - but a declaration with a
    // syntax error inside one is a fact the parser found, and a block that is
    // `consume_raw`'d instead reports nothing about its contents. That is how
    // `@page { *zoom: 1; margin: 1cm }` used to reach the cascade with one
    // diagnostic about `@page` and nothing at all about the bad declaration.
    if at_rule_block_is_declaration_list(&prelude.name) {
        let (_, _, diagnostics) = parse_declarations(input).into_parts();
        return Ok(ParsedRule::DeclarationListBlock {
            at_keyword: prelude.name,
            location,
            diagnostics,
        });
    }
    consume_raw(input);
    Ok(ParsedRule::IgnoredAtRule {
        name: prelude.name,
        location,
    })
}

/// A group at-rule's body. CSS Nesting §3.3 makes a *nested* group rule's
/// block a `<block-contents>` rather than a `<rule-list>`, so it may also hold
/// declarations, and the style rules inside it nest against the enclosing
/// style rule rather than against nothing. Those declarations belong to the
/// enclosing style rule, which is why `own_declarations` is false: they become
/// a nested declarations rule rather than the group's own.
fn parse_group_body<'i, 't>(
    input: &mut Parser<'i, 't>,
    next_anonymous_layer: &mut u32,
    parent: Option<&ParentRule>,
) -> (RuleBody, Vec<StyleSheetDiagnostic>) {
    if parent.is_none() {
        let (rules, diagnostics) = parse_rule_list(input, next_anonymous_layer);
        (RuleBody::Rules(rules), diagnostics)
    } else {
        let (_, nested, diagnostics) =
            parse_block_contents(input, next_anonymous_layer, parent, false);
        (RuleBody::Block(nested), diagnostics)
    }
}

/// A declaration together with the position it starts at.
///
/// The pair is carried through the parser rather than the two values separately,
/// so they cannot drift apart while §5.4.4's recovery is still moving
/// declarations between a rule and a nested declarations rule. It is split again
/// only at [`StyleRule`], where the two live as parallel vectors so that
/// `StyleRule::declarations` keeps the type every consumer already iterates.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SourcedDeclaration {
    declaration: Declaration,
    location: DeclarationLocation,
}

impl SourcedDeclaration {
    /// CSS Syntax §5.4.4's "declaration is either empty or valid": an ordinary
    /// declaration with no value is not one, and dropping it is what keeps
    /// `display: ;` from overriding a lower-priority value.
    fn is_usable(&self) -> bool {
        !self.declaration.value.is_empty() || self.declaration.name.starts_with("--")
    }
}

/// One item of a block's contents. CSS Syntax §5.4.5 has a style rule's block
/// parse as a list of declarations; CSS Nesting §3.1 adds nested rules to the
/// same list, so a nested style rule and its parent come out of one walker.
#[derive(Clone, Debug)]
enum RuleBodyItem {
    Declaration(SourcedDeclaration),
    Rule(ParsedRule),
}

/// What the next token of a block's contents turned out to be, read before any
/// branch so the walker is not holding a borrow of the parser across the
/// decision.
enum NextItem<'i> {
    /// §5.4.4/§5.4.5's first case: whitespace, a comment, or a `;` between two
    /// declarations. "Do nothing", and then look again.
    Nothing,
    /// The block's `}` or the end of the input: §5.4.4's "extend decls with
    /// rules, then return decls".
    End,
    AtRule(CowRcStr<'i>),
    /// An `<ident-token>`, so §5.4.6 "consume a declaration" can be attempted.
    Ident(CowRcStr<'i>),
    /// Anything else, named for the diagnostic.
    ///
    /// §5.4.4 gives a `<delim-token>` of `&` its own branch and sends
    /// everything else to "this is a parse error, reconsume, and consume
    /// component values until a `;`". CSS Nesting §3.1 fills the same block with
    /// rules that may start with any token at all - `> .child`, `.class`,
    /// `#id` - so this walker re-reads every one of them as a qualified rule
    /// before it calls the item a bad declaration. That is the only way to tell
    /// `& { color: red }` from `*zoom: 1`, and guessing wrong in either
    /// direction is a mangled tree rather than a recovered one.
    Other(String),
}

/// Walk a block's contents as a list of declarations, discarding each invalid
/// one **on its own** and continuing with the next one in the same block.
///
/// This is cssparser's `RuleBodyParser` with the reporting replaced, and the
/// reporting is the reason. `RuleBodyParser` recovers correctly - measured over
/// the production corpus, all 134 legacy star hacks cost the block nothing but
/// the offending declaration - but the error it hands back for a malformed
/// declaration is the token that *followed* the declaration, because
/// `expect_curly_bracket_block` runs before the prelude's own error is returned
/// and the real reason is dropped on the way out. So 97 of those star hacks
/// were reported as "unexpected token: Semicolon" and 37 as "unexpected end of
/// input": two spellings of one defect, neither naming it, both pointing at a
/// token that is entirely valid, and the second up to fifteen bytes past the
/// `*` that is not.
///
/// The recovery boundary is `Parser::parse_until_before`, which is §5.4.4's
/// "consume a component value" made literal. It stops at the first top-level
/// `;` and matches `()`, `[]`, `{}` and function blocks whole, so a `;` or a `}`
/// inside one is part of the value and not the end of the declaration. That is
/// the part that is easy to get wrong: treat either as a boundary and
/// `background: image-set("a;b.png" 1x)` costs the block every declaration
/// after it, which is a far worse bug than the one being fixed here.
struct BlockContents<'i, 't, 'a, 'b> {
    input: &'a mut Parser<'i, 't>,
    parser: PropertyParser<'b>,
}

/// What followed an at-rule's prelude: §5.4.2 ends the at-rule at a `;` and
/// finds its block at a `{}`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AfterPrelude {
    NoBlock,
    Block,
}

impl<'i, 't, 'a, 'b> BlockContents<'i, 't, 'a, 'b> {
    fn next(&mut self) -> Option<Result<RuleBodyItem, DeclarationDiagnostic>> {
        loop {
            self.input.skip_whitespace();
            // The item's first token, so a discard is reported where the
            // declaration starts rather than where the failure surfaced.
            let start = self.input.state();
            let next = match self.input.next_including_whitespace_and_comments() {
                Ok(Token::WhiteSpace(_) | Token::Comment(_) | Token::Semicolon) => {
                    NextItem::Nothing
                }
                Ok(Token::CloseCurlyBracket) | Err(_) => NextItem::End,
                Ok(Token::AtKeyword(name)) => NextItem::AtRule(name.clone()),
                Ok(Token::Ident(name)) => NextItem::Ident(name.clone()),
                Ok(token) => NextItem::Other(describe(token)),
            };
            match next {
                NextItem::Nothing => continue,
                NextItem::End => return None,
                NextItem::AtRule(name) => return Some(self.at_rule(&start, name)),
                NextItem::Ident(name) => return Some(self.declaration(&start, name)),
                NextItem::Other(token) => {
                    return Some(self.rule_or_discard(
                        &start,
                        RuleParseError::InvalidDeclaration(format!(
                            "a declaration starts with a property name, and {token} is not one"
                        )),
                    ));
                }
            }
        }
    }

    /// CSS Syntax §5.4.2: the prelude runs to the first `;` - which ends the
    /// at-rule with no block - or to a `{}` block, which is its block. A `;` or
    /// a `}` inside a function, a `()`/`[]` group or a `{}` block is part of the
    /// prelude, which is what `parse_until_before` gives.
    fn at_rule(
        &mut self,
        start: &ParserState,
        name: CowRcStr<'i>,
    ) -> Result<RuleBodyItem, DeclarationDiagnostic> {
        let result = {
            let input = &mut *self.input;
            let parser = &mut self.parser;
            input.parse_until_before(
                Delimiter::Semicolon | Delimiter::CurlyBracketBlock,
                |input| AtRuleParser::parse_prelude(parser, name, input),
            )
        };
        let after = match self.input.next() {
            // §5.4.2: reaching the end of the input here is a parse error, and
            // the at-rule is still returned.
            Ok(&Token::Semicolon) | Err(_) => AfterPrelude::NoBlock,
            Ok(&Token::CurlyBracketBlock) => AfterPrelude::Block,
            Ok(_) => unreachable!("parse_until_before stopped at a semicolon or a brace"),
        };
        match result {
            Ok(prelude) => match after {
                AfterPrelude::NoBlock => self
                    .parser
                    .rule_without_block(prelude, start)
                    .map_err(|()| (RuleParseError::UnnamedAtRule, start.source_location())),
                AfterPrelude::Block => {
                    let input = &mut *self.input;
                    let parser = &mut self.parser;
                    input
                        .parse_nested_block(|input| {
                            AtRuleParser::parse_block(parser, prelude, start, input)
                        })
                        .map_err(|error| (from_kind(error.kind), error.location))
                }
            },
            // §5.4.2 still has to consume the `;` or `{` that ended the prelude
            // even when the prelude did not parse, or the remainder of it is
            // read as though it were declarations or rules of the enclosing
            // block. The `;` has already been consumed above, and the `{` is
            // consumed here with its whole block, because a `{` inside the
            // prelude that is not its block is §5.4.2's "simple block with an
            // associated token of `<{-token>`" case: the at-rule is complete and
            // the block belongs to nothing this walker will read again.
            Err(error) => {
                if after == AfterPrelude::Block {
                    let _: Result<(), ParseError<RuleParseError>> =
                        self.input.parse_nested_block(|input| {
                            while input.next_including_whitespace_and_comments().is_ok() {}
                            Ok(())
                        });
                }
                Err((from_kind(error.kind), error.location))
            }
        }
    }

    /// CSS Syntax §5.4.6: consume a declaration whose name has been read.
    fn declaration(
        &mut self,
        start: &ParserState,
        name: CowRcStr<'i>,
    ) -> Result<RuleBodyItem, DeclarationDiagnostic> {
        // §5.4.6: "if the next input token is anything other than a
        // <colon-token>, this is a parse error. Return nothing." The token is
        // kept so `missing_colon` can name it.
        let after_name = self.input.next().ok().cloned();
        if !matches!(after_name, Some(Token::Colon)) {
            return self.rule_or_discard(start, missing_colon(after_name));
        }
        let result = {
            let input = &mut *self.input;
            let parser = &mut self.parser;
            input.parse_until_before(Delimiter::Semicolon, |input| {
                parser.parse_value(name, input, start)
            })
        };
        match result {
            Ok(item) => Ok(item),
            // CSS Syntax §5.5.5's "a declaration followed by a block is a nested
            // style rule". The re-read either finds that rule or does not, and
            // when it does not the declaration is simply invalid, which is what
            // §5.4.4 does with it and what is reported - the error `parse_value`
            // produced is kept, so the reason is the one the declaration's own
            // reading gave.
            Err(error) => {
                let invalid = from_kind(error.kind);
                self.rule_or_discard(start, invalid)
            }
        }
    }

    /// CSS Nesting §3.1: an item that cannot be a declaration is re-read as a
    /// qualified rule, because a nested style rule may start with any token at
    /// all. If that fails too then §5.4.4's closing case applies: the remnants
    /// of one bad declaration are thrown out, parsing starts fresh at the next
    /// declaration in the same block, and `invalid` is the diagnostic that owes
    /// the author an explanation.
    fn rule_or_discard(
        &mut self,
        start: &ParserState,
        invalid: RuleParseError,
    ) -> Result<RuleBodyItem, DeclarationDiagnostic> {
        if self.parser.allow_nesting {
            self.input.reset(start);
            if let Ok(item) = self.qualified_rule(start) {
                return Ok(item);
            }
        }
        self.discard(start);
        Err((invalid, start.source_location()))
    }

    /// Skip the remnants of a bad declaration: to the next top-level `;`, or to
    /// the end of the block. `parse_until_after` matches `()`/`[]`/`{}` and
    /// function blocks whole on the way, which is what keeps the `;` inside one
    /// from being mistaken for the boundary, and it is why a discarded
    /// declaration cannot eat the declarations after it.
    fn discard(&mut self, start: &ParserState) {
        self.input.reset(start);
        let _: Result<(), ParseError<()>> = self
            .input
            .parse_until_after(Delimiter::Semicolon, |_| Ok(()));
    }

    /// CSS Syntax §5.4.3, with CSS Nesting §3.1's extra stop at a `;`: inside a
    /// block that may hold declarations, a nested rule's prelude ends at a `;`
    /// as well as at the `{`, or `a { color: red; b; margin: 1px }` would take
    /// the rest of the block with it.
    fn qualified_rule(
        &mut self,
        start: &ParserState,
    ) -> Result<RuleBodyItem, ParseError<'i, RuleParseError>> {
        self.input.reset(start);
        let prelude = {
            let input = &mut *self.input;
            let parser = &mut self.parser;
            input.parse_until_before(
                Delimiter::Semicolon | Delimiter::CurlyBracketBlock,
                |input| QualifiedRuleParser::parse_prelude(parser, input),
            )
        };
        // The `{` is consumed whether or not the prelude parsed, so a rule with
        // an invalid selector does not leave its block to be read a second time
        // as a rule of its own.
        self.input.expect_curly_bracket_block()?;
        let prelude = prelude?;
        let input = &mut *self.input;
        let parser = &mut self.parser;
        input.parse_nested_block(|input| {
            QualifiedRuleParser::parse_block(parser, prelude, start, input)
        })
    }
}

/// What to say about an `<ident-token>` that is not followed by a `:`.
///
/// The offending token is named, because it is the one thing the author has to
/// delete: `font-family 'X'` wants `unexpected token: QuotedString("X")`. That
/// is cssparser's own wording and it is kept, for two reasons. It is accurate
/// here, unlike the star hack's case - the token really is the one at fault -
/// and it keeps the shape of a message a reader already has tooling for.
///
/// `None` means the block ended before the colon arrived, so there was no token
/// to name and no declaration either. Saying that is better than naming nothing.
fn missing_colon(token: Option<Token<'_>>) -> RuleParseError {
    match token {
        Some(token) => RuleParseError::UnexpectedToken(format!("{token:?}")),
        None => RuleParseError::InvalidDeclaration(
            "a declaration is a property name, a colon, and a value, and the block ended first"
                .to_owned(),
        ),
    }
}

/// The crate's own error out of a `ParseError`.
///
/// A `Basic` error is cssparser's own, and keeping its wording is deliberate: it
/// is the shape a reader already has tooling for, and this crate only lets one
/// through where cssparser's own recovery has already rejected the construct.
fn from_kind(kind: ParseErrorKind<'_, RuleParseError>) -> RuleParseError {
    match kind {
        ParseErrorKind::Custom(error) => error,
        ParseErrorKind::Basic(error) => RuleParseError::UnexpectedToken(error.to_string()),
    }
}

/// How a token is named in a diagnostic.
///
/// Only the *first* token of a discarded item ever reaches this, and only when
/// it is not an `<ident-token>`, so the list is short. What matters is that the
/// name is the thing the author has to delete: for the legacy star hack that is
/// the `*`, not the `;` that ended the declaration.
fn describe(token: &Token<'_>) -> String {
    match token {
        Token::Delim(value) => format!("'{value}'"),
        Token::Ident(value) | Token::AtKeyword(value) => format!("{value}"),
        Token::Function(value) => format!("{value}()"),
        Token::Hash(value) | Token::IDHash(value) => format!("#{value}"),
        Token::QuotedString(value) => format!("\"{value}\""),
        Token::UnquotedUrl(value) | Token::BadUrl(value) => format!("url({value})"),
        Token::BadString(_) => "an unterminated string".to_owned(),
        Token::Number { int_value, .. } => {
            int_value.map_or_else(|| "a number".to_owned(), |value| value.to_string())
        }
        Token::Percentage { int_value, .. } => {
            int_value.map_or_else(|| "a percentage".to_owned(), |value| format!("{value}%"))
        }
        Token::Dimension { value, unit, .. } => int_or_float(*value)
            .map_or_else(|| unit.to_string(), |number| format!("{number}{unit}")),
        Token::Colon => "':'".to_owned(),
        Token::Comma => "','".to_owned(),
        Token::CDO => "'<!--'".to_owned(),
        Token::CDC => "'-->'".to_owned(),
        Token::ParenthesisBlock => "'('".to_owned(),
        Token::SquareBracketBlock => "'['".to_owned(),
        Token::CurlyBracketBlock => "'{'".to_owned(),
        // Reached only for an unclosed `(` or `[` at the end of the input, which
        // the tokenizer has already run to the end of.
        Token::CloseParenthesis => "')'".to_owned(),
        Token::CloseSquareBracket => "']'".to_owned(),
        Token::CloseCurlyBracket => "'}'".to_owned(),
        // Whitespace, a comment and `;` are handled before this point, and the
        // identifier-like tokens above are the only remaining ones cssparser
        // defines.
        _ => "a token".to_owned(),
    }
}

/// A dimension's number as an integer when the source wrote one, so `50%` and
/// `0.5em` are named the way they appear rather than the way they were parsed.
fn int_or_float(value: f32) -> Option<String> {
    if value.fract() == 0.0 {
        return Some(format!("{}", value as i64));
    }
    Some(format!("{value}"))
}

struct PropertyParser<'a> {
    /// CSS Nesting §3.1: only a style rule's block (and a nested group rule's
    /// block) also holds nested rules. A descriptor list such as `@font-face`
    /// and the `style` attribute do not.
    allow_nesting: bool,
    /// The nearest enclosing style rule, which is what a nested rule's nesting
    /// selector refers to.
    parent: Option<ParentRule>,
    next_anonymous_layer: &'a mut u32,
}

impl<'a> PropertyParser<'a> {
    /// A declaration list that may not contain nested rules: the `style`
    /// attribute and a descriptor list.
    fn declarations_only(next_anonymous_layer: &'a mut u32) -> Self {
        Self {
            allow_nesting: false,
            parent: None,
            next_anonymous_layer,
        }
    }
}

impl<'i> DeclarationParser<'i> for PropertyParser<'_> {
    type Declaration = RuleBodyItem;
    type Error = RuleParseError;

    fn parse_value<'t>(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i, 't>,
        declaration_start: &ParserState,
    ) -> Result<Self::Declaration, ParseError<'i, Self::Error>> {
        // CSS Syntax §3.3 removes comments while preprocessing the token
        // stream, so leading whitespace and comments are not part of the
        // value. Start the value slice at the first real token.
        let mut leading = input.state();
        while let Ok(token) = input.next_including_whitespace_and_comments() {
            if !matches!(token, Token::WhiteSpace(_) | Token::Comment(_)) {
                input.reset(&leading);
                break;
            }
            leading = input.state();
        }
        let start = input.position();
        let mut value_end = start;
        let mut important = false;
        // Component counts, so §5.5.5's "the value is either solely a
        // {}-block or contains none at all" test is available.
        let mut components = 0_usize;
        let mut blocks = 0_usize;

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
                important = true;
                break;
            }
            input.reset(&state);

            match input.next_including_whitespace_and_comments().cloned() {
                Ok(Token::WhiteSpace(_) | Token::Comment(_)) => {}
                Ok(
                    token @ (Token::Function(_)
                    | Token::ParenthesisBlock
                    | Token::SquareBracketBlock
                    | Token::CurlyBracketBlock),
                ) => {
                    // Only a `{}`-block counts for §5.5.5's "solely a {}-block
                    // or no {}-block at all" test. A function, a `()` group or
                    // a `[]` group is an ordinary value component, so
                    // `margin: calc(50% - 10px) auto` stays a declaration.
                    let is_curly_block = matches!(token, Token::CurlyBracketBlock);
                    input.parse_nested_block(|nested| {
                        while nested.next_including_whitespace_and_comments().is_ok() {}
                        Ok(())
                    })?;
                    value_end = input.position();
                    components += 1;
                    blocks += usize::from(is_curly_block);
                }
                Ok(_) => {
                    value_end = input.position();
                    components += 1;
                }
                Err(_) => break,
            }
        }

        let raw_name = name.as_ref();
        let is_custom_property = raw_name.starts_with("--");
        if self.allow_nesting && !is_custom_property {
            // CSS Syntax §5.5.5's early exits, spelled out in that section's
            // implementation note: no currently-defined property takes a
            // {}-block as part of a longer value, so `div:hover { ... }` inside
            // a block is a nested style rule and not a declaration named
            // `div`. Returning an error here is what makes the block walker
            // reparse it as a qualified rule. A custom property is exempt,
            // because it keeps its whole token stream and `--foo: {}` is a
            // declaration everywhere.
            //
            // The error is a *declaration* error, not a selector one: §5.4.4
            // discards a bad declaration here, and a message that began "invalid
            // selector" would name a selector the author never wrote.
            let trailing_block = matches!(input.next(), Ok(&Token::CurlyBracketBlock));
            if trailing_block || (blocks > 0 && components > blocks) {
                return Err(input.new_custom_error(RuleParseError::InvalidDeclaration(
                    "its value is a block, which makes this a nested style rule".to_owned(),
                )));
            }
        }

        let normalized_name = if is_custom_property {
            raw_name.to_owned()
        } else {
            raw_name.to_ascii_lowercase()
        };
        Ok(RuleBodyItem::Declaration(SourcedDeclaration {
            declaration: Declaration {
                name: normalized_name,
                value: input.slice(start..value_end).trim().to_owned(),
                important,
            },
            // The declaration's first token, which is the same position the
            // discard diagnostics for this declaration already use, so a
            // declaration that was *kept* is reported at the place a discarded
            // one would have been.
            location: declaration_start.source_location().into(),
        }))
    }
}

impl<'i> QualifiedRuleParser<'i> for PropertyParser<'_> {
    type Prelude = NestedSelectors;
    type QualifiedRule = RuleBodyItem;
    type Error = RuleParseError;

    fn parse_prelude<'t>(
        &mut self,
        input: &mut Parser<'i, 't>,
    ) -> Result<Self::Prelude, ParseError<'i, Self::Error>> {
        let selector_text = consume_raw(input);
        let resolved = match self.parent.as_ref() {
            Some(parent) => parse_nested_selector_list(&selector_text, &parent.text),
            None => parse_selector_list(&selector_text).map(|selectors| NestedSelectors {
                css: selector_text,
                selectors,
            }),
        };
        resolved.map_err(|error| {
            input.new_custom_error(RuleParseError::InvalidSelector(error.to_string()))
        })
    }

    fn parse_block<'t>(
        &mut self,
        resolved: Self::Prelude,
        _start: &ParserState,
        input: &mut Parser<'i, 't>,
    ) -> Result<Self::QualifiedRule, ParseError<'i, Self::Error>> {
        // This rule is the parent for anything nested inside its own block, so
        // the resolved text has to travel with the rule rather than be
        // recomputed from the prelude, which is already gone.
        let parent = ParentRule {
            text: resolved.css,
            selectors: resolved.selectors,
        };
        let (declarations, nested, diagnostics) =
            parse_block_contents(input, self.next_anonymous_layer, Some(&parent), true);
        Ok(RuleBodyItem::Rule(ParsedRule::Style {
            selectors: parent.selectors,
            declarations,
            nested,
            diagnostics,
        }))
    }
}

impl<'i> AtRuleParser<'i> for PropertyParser<'_> {
    type Prelude = AtPrelude;
    type AtRule = RuleBodyItem;
    type Error = RuleParseError;

    fn parse_prelude<'t>(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i, 't>,
    ) -> Result<Self::Prelude, ParseError<'i, Self::Error>> {
        Ok(AtPrelude {
            name: name.to_ascii_lowercase(),
            value: consume_raw(input),
        })
    }

    fn rule_without_block(
        &mut self,
        prelude: Self::Prelude,
        start: &ParserState,
    ) -> Result<Self::AtRule, ()> {
        // A statement at-rule nested in a style rule. `@layer a, b;` is
        // CSS Cascade 5 §7.1's layer *statement*, which has nothing to do with
        // the enclosing selector, so it registers layer order exactly as one at
        // the top level does. Every other at-rule applies nothing to the
        // enclosing rule's elements, and each is reported rather than dropped:
        // `Err(())` here would make cssparser discard the construct with no
        // diagnostic at all, which is the silent drop this project forbids.
        let location = start.source_location();
        if prelude.name == "layer" {
            return match parse_layer_names(&prelude.value) {
                Ok(layers) if !layers.is_empty() => {
                    Ok(RuleBodyItem::Rule(ParsedRule::LayerStatement {
                        layers,
                        location,
                    }))
                }
                _ => Ok(RuleBodyItem::Rule(ParsedRule::InvalidLayerStatement {
                    location,
                })),
            };
        }
        Ok(RuleBodyItem::Rule(ParsedRule::IgnoredAtRule {
            name: prelude.name,
            location,
        }))
    }

    fn parse_block<'t>(
        &mut self,
        prelude: Self::Prelude,
        start: &ParserState,
        input: &mut Parser<'i, 't>,
    ) -> Result<Self::AtRule, ParseError<'i, Self::Error>> {
        // CSS Nesting §3.3: a nested group rule keeps the meaning of the
        // at-rule; only the shape of its block changes. The shared at-rule
        // parser is what guarantees that.
        parse_at_rule_block(
            prelude,
            start,
            input,
            self.next_anonymous_layer,
            self.parent.as_ref(),
        )
        .map(RuleBodyItem::Rule)
    }
}

/// Parse a stylesheet using CSS Syntax recovery rules.
#[must_use]
pub fn parse_stylesheet(source: &str) -> StyleSheet {
    let mut input = ParserInput::new(source);
    let mut parser = Parser::new(&mut input);
    let mut next_anonymous_layer = 0;
    let (rules, diagnostics) = parse_rule_list(&mut parser, &mut next_anonymous_layer);
    let mut sheet = StyleSheet {
        diagnostics,
        ..StyleSheet::default()
    };
    flatten_rules(rules, None, &[], &mut sheet);
    sheet
}

/// Parse an HTML `style` attribute as a CSS declaration list.
#[must_use]
pub fn parse_declaration_list(source: &str) -> (Vec<Declaration>, Vec<StyleSheetDiagnostic>) {
    let (declarations, _, diagnostics) = parse_declaration_list_with_locations(source).into_parts();
    (declarations, diagnostics)
}

/// [`parse_declaration_list`] with each kept declaration's position.
///
/// The `style` attribute is the one declaration list with no file behind it, so
/// its positions are positions within the attribute's own text. They are still
/// worth having: a presentational hint is a declaration that a cascade can
/// reject, and a report that can name the offset inside the attribute is the
/// difference between "this hint is invalid" and "this hint is invalid".
#[must_use]
pub fn parse_declaration_list_with_locations(source: &str) -> DeclarationList {
    let mut input = ParserInput::new(source);
    let mut parser = Parser::new(&mut input);
    parse_declarations(&mut parser)
}

pub(crate) fn css_wide_keyword(source: &str) -> Option<CssWideKeyword> {
    let mut input = ParserInput::new(source);
    let mut parser = Parser::new(&mut input);
    let ident = parser.expect_ident_cloned().ok()?;
    parser.expect_exhausted().ok()?;
    if ident.eq_ignore_ascii_case("inherit") {
        Some(CssWideKeyword::Inherit)
    } else if ident.eq_ignore_ascii_case("initial") {
        Some(CssWideKeyword::Initial)
    } else if ident.eq_ignore_ascii_case("unset") {
        Some(CssWideKeyword::Unset)
    } else if ident.eq_ignore_ascii_case("revert") {
        Some(CssWideKeyword::Revert)
    } else if ident.eq_ignore_ascii_case("revert-layer") {
        Some(CssWideKeyword::RevertLayer)
    } else {
        None
    }
}

fn parse_rule_list(
    input: &mut Parser<'_, '_>,
    next_anonymous_layer: &mut u32,
) -> (Vec<ParsedRule>, Vec<StyleSheetDiagnostic>) {
    let mut parser = RuleParser {
        next_anonymous_layer,
        parent: None,
    };
    let mut rules = Vec::new();
    let mut diagnostics = Vec::new();
    for item in StyleSheetParser::new(input, &mut parser) {
        match item {
            Ok(rule) => rules.push(rule),
            Err((error, _source)) => diagnostics.push(diagnostic(&error)),
        }
    }
    (rules, diagnostics)
}

/// Parse a block's contents (`CSS Syntax §5.4.5`) with CSS Nesting §3.1
/// enabled: the block holds declarations *and* nested rules.
///
/// A run of declarations is only returned as this rule's own declarations while
/// no nested rule has been seen yet. Once one has, later declarations belong to
/// a nested declarations rule (`CSS Nesting §5`), which is emitted into
/// `nested` with the parent's own selectors and a later source order.
fn parse_block_contents(
    input: &mut Parser<'_, '_>,
    next_anonymous_layer: &mut u32,
    parent: Option<&ParentRule>,
    own_declarations: bool,
) -> (
    Vec<SourcedDeclaration>,
    Vec<ParsedRule>,
    Vec<StyleSheetDiagnostic>,
) {
    let mut nested = Vec::new();
    let mut diagnostics = Vec::new();
    // The run of declarations before any nested rule belongs to this rule
    // itself; every run after one belongs to a nested declarations rule
    // (`CSS Nesting §5`), which §3.4 orders after the nested rules. A nested
    // group rule's declarations belong to the *enclosing* style rule (§3.3),
    // so they start out already interrupted.
    let mut own: Vec<SourcedDeclaration> = Vec::new();
    let mut pending: Vec<SourcedDeclaration> = Vec::new();
    let mut interrupted = !own_declarations;
    let mut contents = BlockContents {
        input,
        parser: PropertyParser {
            allow_nesting: true,
            parent: parent.cloned(),
            next_anonymous_layer,
        },
    };
    while let Some(item) = contents.next() {
        match item {
            // An empty ordinary declaration, such as `display: ;`, is invalid
            // CSS and must not override a lower-priority value. Pages commonly
            // emit this form when an optional inline style is generated by a
            // template, so it is dropped rather than kept with an empty value.
            Ok(RuleBodyItem::Declaration(declaration)) => {
                if declaration.is_usable() {
                    if interrupted {
                        pending.push(declaration);
                    } else {
                        own.push(declaration);
                    }
                }
            }
            Ok(RuleBodyItem::Rule(rule)) => {
                interrupted = true;
                flush_nested_declarations(&mut pending, &mut nested, parent);
                nested.push(rule);
            }
            Err((error, location)) => diagnostics.push(at(&error, location)),
        }
    }
    if interrupted {
        flush_nested_declarations(&mut pending, &mut nested, parent);
    }
    (own, nested, diagnostics)
}

/// CSS Nesting §5: declarations that follow a nested rule are wrapped in a
/// nested declarations rule, which matches exactly what its parent style rule
/// matches and has the same specificity. Emitting it as a separate rule here
/// with the parent's selectors is what gives it the later Order Of Appearance
/// that §3.4 requires.
fn flush_nested_declarations(
    declarations: &mut Vec<SourcedDeclaration>,
    nested: &mut Vec<ParsedRule>,
    parent: Option<&ParentRule>,
) {
    let Some(parent) = parent else {
        return;
    };
    if declarations.is_empty() {
        return;
    }
    let run = std::mem::take(declarations);
    // An empty ordinary declaration does not override a lower-priority value,
    // and a run holding nothing but those is not a rule.
    let run: Vec<SourcedDeclaration> = run
        .into_iter()
        .filter(SourcedDeclaration::is_usable)
        .collect();
    if run.is_empty() {
        return;
    }
    nested.push(ParsedRule::NestedDeclarations {
        selectors: parent.selectors.clone(),
        declarations: run,
    });
}

/// Parse a declaration list that may not contain nested rules: an HTML `style`
/// attribute, and a descriptor list such as `@font-face`'s or `@page`'s.
///
/// The walker is shared with a style rule's block, so the recovery and the
/// reporting cannot differ between the two. The only difference is that a
/// descriptor list never tries to read a discarded declaration as a nested rule
/// (§5.4.5 has no such branch), and an at-rule found inside one is reported here
/// rather than being dropped with the rest of the list.
fn parse_declarations(input: &mut Parser<'_, '_>) -> DeclarationList {
    let mut anonymous_layer = 0;
    let mut declarations = Vec::new();
    let mut locations = Vec::new();
    let mut diagnostics = Vec::new();
    let mut contents = BlockContents {
        input,
        parser: PropertyParser::declarations_only(&mut anonymous_layer),
    };
    while let Some(item) = contents.next() {
        match item {
            Ok(RuleBodyItem::Declaration(declaration)) => {
                if declaration.is_usable() {
                    locations.push(declaration.location);
                    declarations.push(declaration.declaration);
                }
            }
            // §5.4.5 does append the at-rules it finds to the list it returns,
            // but a descriptor list this engine cannot use has no at-rule to
            // keep. What it does have to do is report it: `@nonesuch {}` inside
            // a `@font-face` is a discard, and `discard_diagnostic` is the same
            // one `flatten_rules` would have produced.
            Ok(RuleBodyItem::Rule(rule)) => {
                if let Some(diagnostic) = discard_diagnostic(&rule) {
                    diagnostics.push(diagnostic);
                }
            }
            Err((error, location)) => diagnostics.push(at(&error, location)),
        }
    }
    DeclarationList {
        declarations,
        locations,
        diagnostics,
    }
}

/// What a declaration list parse produced, with each kept declaration's position
/// alongside it.
///
/// A style rule keeps the two apart because [`StyleRule::declarations`] is a
/// `Vec<Declaration>` that half the tree iterates. A bare declaration list has
/// no such consumer, so it keeps the pair together and hands the two out
/// separately from [`DeclarationList::into_parts`].
#[derive(Clone, Debug, Default)]
pub struct DeclarationList {
    /// The declarations that were kept, in source order.
    pub declarations: Vec<Declaration>,
    /// The position of each declaration in `declarations`, in the same order.
    /// The two are always the same length.
    pub locations: Vec<DeclarationLocation>,
    /// What was thrown out, and why.
    pub diagnostics: Vec<StyleSheetDiagnostic>,
}

impl DeclarationList {
    /// The three values the tuple-shaped callers before `DeclarationLocation`
    /// existed expect, so a caller that does not want a position is unchanged.
    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        Vec<Declaration>,
        Vec<DeclarationLocation>,
        Vec<StyleSheetDiagnostic>,
    ) {
        (self.declarations, self.locations, self.diagnostics)
    }
}

/// The diagnostic a rule this parser throws away owes its author, or `None` for a
/// rule that is kept.
///
/// One function, so that a rule discarded where it was parsed and one discarded
/// later by `flatten_rules` cannot be reported differently - the same reasoning
/// that made `at_rule_support` a single three-state answer rather than a
/// `match` at each call site.
fn discard_diagnostic(rule: &ParsedRule) -> Option<StyleSheetDiagnostic> {
    match rule {
        // `at_rule_diagnostic` is what tells "never heard of it" apart from
        // "know exactly what it is and cannot do it yet", and both are
        // reported: dropping an at-rule with no diagnostic is the bug this
        // crate's own `css.font-face` and `css.animations` registry entries are
        // about.
        ParsedRule::IgnoredAtRule { name, location } => {
            Some(capability_diagnostic(*location, at_rule_diagnostic(name)))
        }
        ParsedRule::InvalidLayerStatement { location } => Some(capability_diagnostic(
            *location,
            RuleParseError::InvalidLayerName.to_string(),
        )),
        _ => None,
    }
}

/// The one place a `StyleSheetDiagnostic` is built, so a parse error and a
/// capability report cannot disagree about how a position is written down.
///
/// The conversion goes through [`DeclarationLocation`], which is the same shape
/// and the same one-based line, so a kept declaration and a discarded one cannot
/// be reported at different conventions either.
fn at(error: &RuleParseError, location: SourceLocation) -> StyleSheetDiagnostic {
    from_location(location, error.to_string())
}

fn diagnostic(error: &ParseError<'_, RuleParseError>) -> StyleSheetDiagnostic {
    at(&from_kind_ref(&error.kind), error.location)
}

/// [`from_kind`] for a borrow, because the top-level rule list reports
/// cssparser's `ParseError` values directly.
fn from_kind_ref(kind: &ParseErrorKind<'_, RuleParseError>) -> RuleParseError {
    match kind {
        ParseErrorKind::Custom(error) => error.clone(),
        ParseErrorKind::Basic(error) => RuleParseError::UnexpectedToken(error.to_string()),
    }
}

/// Every `StyleSheetDiagnostic` is built here, so the one-based line of
/// [`DeclarationLocation`] is the one every diagnostic uses.
fn from_location(location: SourceLocation, message: String) -> StyleSheetDiagnostic {
    let DeclarationLocation { line, column } = location.into();
    StyleSheetDiagnostic {
        line,
        column,
        message,
    }
}

fn capability_diagnostic(location: SourceLocation, message: String) -> StyleSheetDiagnostic {
    from_location(location, message)
}

fn consume_raw(input: &mut Parser<'_, '_>) -> String {
    let start = input.position();
    while input.next_including_whitespace_and_comments().is_ok() {}
    input.slice_from(start).trim().to_owned()
}

fn parse_layer_names(source: &str) -> Result<Vec<LayerName>, ()> {
    let mut input = ParserInput::new(source);
    let mut parser = Parser::new(&mut input);
    parser
        .parse_comma_separated(|part| -> Result<LayerName, ParseError<'_, ()>> {
            let segments = vec![part.expect_ident_cloned()?.to_string()];
            if part
                .try_parse(|candidate| candidate.expect_delim('.'))
                .is_ok()
            {
                return Err(part.new_custom_error(()));
            }
            Ok(LayerName::Named(segments))
        })
        .map_err(|_| ())
}

fn register_layer(sheet: &mut StyleSheet, layer: &LayerName) {
    if !sheet.layer_order.contains(layer) {
        sheet.layer_order.push(layer.clone());
    }
}

/// The two parallel vectors [`StyleRule`] carries, split out of the parser's
/// paired form. One function so the two cannot come from different orderings.
fn kept_declarations(
    sourced: &[SourcedDeclaration],
) -> (Vec<Declaration>, Vec<DeclarationLocation>) {
    sourced
        .iter()
        .map(|entry| (entry.declaration.clone(), entry.location))
        .unzip()
}

fn flatten_rules(
    rules: Vec<ParsedRule>,
    layer: Option<&LayerName>,
    media: &[String],
    sheet: &mut StyleSheet,
) {
    for rule in rules {
        match rule {
            ParsedRule::Style {
                selectors,
                declarations,
                nested,
                diagnostics,
            } => {
                sheet.diagnostics.extend(diagnostics);
                // CSS Nesting §3.4: a nested rule is considered to come after
                // its parent, so the parent's own declarations are emitted
                // first and the nested rules afterwards in source order. A
                // nested declarations rule resolves to the same selectors and
                // gets a later `source_order` for free by being emitted here.
                // A rule whose declarations were all dropped is still emitted,
                // so the parsed rule list stays a faithful image of the sheet.
                let (declarations, declaration_locations) = kept_declarations(&declarations);
                sheet.rules.push(StyleRule {
                    selectors,
                    declarations,
                    declaration_locations,
                    layer: layer.cloned(),
                    media: media.to_vec(),
                    source_order: u64::try_from(sheet.rules.len()).unwrap_or(u64::MAX),
                });
                flatten_rules(nested, layer, media, sheet);
            }
            ParsedRule::NestedDeclarations {
                selectors,
                declarations,
            } => {
                // A run of declarations that follows a nested rule
                // (`CSS Nesting §5`). It carries its parent style rule's
                // selectors, so it matches the same elements and has the same
                // specificity; the later Order Of Appearance is what decides
                // between the two.
                let (declarations, declaration_locations) = kept_declarations(&declarations);
                sheet.rules.push(StyleRule {
                    selectors,
                    declarations,
                    declaration_locations,
                    layer: layer.cloned(),
                    media: media.to_vec(),
                    source_order: u64::try_from(sheet.rules.len()).unwrap_or(u64::MAX),
                });
            }
            ParsedRule::LayerStatement { layers, location } => {
                if layer.is_none() {
                    for layer_name in layers {
                        register_layer(sheet, &layer_name);
                    }
                } else {
                    sheet.diagnostics.push(capability_diagnostic(
                        location,
                        "nested cascade layers are not implemented yet".to_owned(),
                    ));
                }
            }
            ParsedRule::LayerBlock {
                layer: nested_layer,
                rules,
                diagnostics,
                location,
            } => {
                sheet.diagnostics.extend(diagnostics);
                if layer.is_some() {
                    sheet.diagnostics.push(capability_diagnostic(
                        location,
                        "nested cascade layers are not implemented yet".to_owned(),
                    ));
                } else {
                    register_layer(sheet, &nested_layer);
                    flatten_rules(rules, Some(&nested_layer), media, sheet);
                }
            }
            ParsedRule::MediaBlock {
                query,
                body,
                diagnostics,
            } => {
                sheet.diagnostics.extend(diagnostics);
                let mut nested_media = media.to_vec();
                nested_media.push(query);
                flatten_group_body(body, layer, &nested_media, sheet);
            }
            ParsedRule::SupportsBlock {
                query,
                body,
                diagnostics,
                location,
            } => {
                // CSS Conditional Rules 3 §3: an invalid rule inside a
                // conditional group rule is ignored on its own, and §6 makes an
                // invalid *condition* drop the whole rule. Either way the
                // block's own diagnostics are the author's problem whether or
                // not the block applies, so they are reported first and
                // unconditionally - which is also what makes a diagnostic
                // survive being nested inside a group rule.
                sheet.diagnostics.extend(diagnostics);
                match evaluate_supports_condition(&query) {
                    // §2: when the condition is true the rules inside apply as
                    // though they were at the group rule's location, and when
                    // it is false none of them apply.
                    SupportsCondition::Evaluated(true) => {
                        flatten_group_body(body, layer, media, sheet);
                    }
                    // A false condition is a complete answer, not a gap: the
                    // author wrote the query to find out whether the engine
                    // supports the thing, and the answer was no. The fallback
                    // the author wrote for exactly that case now gets to run.
                    SupportsCondition::Evaluated(false) => {}
                    // §6: "processors must ignore such a rule (including all of
                    // its contents)", and the author cannot tell an ignored rule
                    // from a supported engine without being told.
                    SupportsCondition::Invalid => {
                        sheet.diagnostics.push(capability_diagnostic(
                            location,
                            "@supports condition is not valid, so the rule and its contents \
                             are dropped"
                                .to_owned(),
                        ));
                    }
                    // A feature this crate cannot pose a question about. The
                    // rules are not applied - claiming support would be the lie
                    // this project forbids - and the reason names the crate that
                    // has to answer it.
                    SupportsCondition::Undecidable(gaps) => {
                        for gap in gaps {
                            sheet.diagnostics.push(capability_diagnostic(
                                location,
                                format!(
                                    "@supports {} is not answered: {}",
                                    gap.feature, gap.reason
                                ),
                            ));
                        }
                    }
                }
            }
            ParsedRule::KeyframesBlock {
                name,
                at_keyword,
                location,
            } => {
                // The current paint pipeline has no animation clock, but the
                // at-rule is still valid and must not make a stylesheet fail.
                // The block's contents are dropped for the reported reason
                // below, so they need no diagnostics of their own.
                let _ = name;
                sheet.diagnostics.push(capability_diagnostic(
                    location,
                    at_rule_diagnostic(&at_keyword),
                ));
            }
            ParsedRule::DeclarationListBlock {
                at_keyword,
                location,
                diagnostics,
            } => {
                // A descriptor list this engine parses and then cannot use: no
                // page box is laid out, no font is fetched, no custom property is
                // registered. The list's own syntax errors are reported too, and
                // first, because a parse error that was found and thrown away
                // with the block is the same silent drop one level down. This is
                // every at-rule `at_rule_block_is_declaration_list` names, not
                // just `@font-face`: reading only one of them was how a `@page`
                // block full of legacy hacks produced no diagnostic at all.
                sheet.diagnostics.extend(diagnostics);
                sheet.diagnostics.push(capability_diagnostic(
                    location,
                    at_rule_diagnostic(&at_keyword),
                ));
            }
            ParsedRule::NestedLayerBlock { location } => {
                sheet.diagnostics.push(capability_diagnostic(
                    location,
                    "nested cascade layers are not implemented yet".to_owned(),
                ));
            }
            // An at-rule this engine throws away, a `@layer` statement whose
            // name does not parse, and a block at-rule written as a statement.
            // `discard_diagnostic` is the same function the declaration-list
            // walker uses, so a rule discarded where it was parsed and one
            // discarded here cannot be reported differently.
            rule
            @ (ParsedRule::IgnoredAtRule { .. } | ParsedRule::InvalidLayerStatement { .. }) => {
                if let Some(diagnostic) = discard_diagnostic(&rule) {
                    sheet.diagnostics.push(diagnostic);
                }
            }
        }
    }
}

/// Flatten a group at-rule's body. A `<block-contents>` (a group rule nested in
/// a style rule, `CSS Nesting §3.3`) is the same rule list a style rule's
/// `nested` field holds, with the group's media condition applied to all of it.
fn flatten_group_body(
    body: RuleBody,
    layer: Option<&LayerName>,
    media: &[String],
    sheet: &mut StyleSheet,
) {
    let rules = match body {
        RuleBody::Rules(rules) | RuleBody::Block(rules) => rules,
    };
    flatten_rules(rules, layer, media, sheet);
}

#[cfg(test)]
mod tests {
    use super::{
        CssWideKeyword, LayerName, css_wide_keyword, parse_declaration_list, parse_stylesheet,
    };
    use crate::selector::{MatchContext, Specificity, select_all};
    use render_html::parse_document;

    /// The ids each parsed style rule matches, together with the rule's
    /// greatest specificity. A nested rule's resolved selector is then observed
    /// as behaviour rather than as text, which is what the cascade consumes.
    fn matched_ids(html: &str, source: &str) -> Vec<(Vec<String>, Specificity)> {
        let output = parse_document(html);
        let sheet = parse_stylesheet(source);
        sheet
            .rules
            .iter()
            .map(|rule| {
                let ids = select_all(
                    &output.dom,
                    output.dom.document(),
                    &rule.selectors,
                    &MatchContext::default(),
                )
                .iter()
                .map(|node| {
                    output
                        .dom
                        .attribute(*node, "id")
                        .ok()
                        .flatten()
                        .unwrap_or_default()
                        .to_owned()
                })
                .collect();
                (ids, rule.selectors.max_specificity())
            })
            .collect()
    }

    const NESTED_DOM: &str = "<!doctype html><article id='a' class='foo bar'>\
         <div id='f'><span id='fc'><b id='p'>x</b></span></div>\
         <div id='d' class='baz'></div><span id='s' class='qux'></span>\
         <em id='e' class='plain'></em></article>";

    /// CSS Nesting §3.1/§3.2: a style rule's block holds nested style rules, and
    /// every `&` form the spec lists resolves against the parent. Each entry is
    /// the ids that rule's selector matches.
    #[test]
    fn nested_style_rules_resolve_the_nesting_selector() {
        let matched = matched_ids(
            NESTED_DOM,
            ".foo { color: blue; \
               & { padding: 1px } \
               &.bar { padding: 2px } \
               & > .baz { padding: 3px } \
               &:hover { padding: 4px } \
               > .qux { padding: 5px } \
               .plain { padding: 6px } \
               > div > span > b { padding: 7px } }",
        );

        let ids: Vec<Vec<String>> = matched.iter().map(|(ids, _)| ids.clone()).collect();
        let none: Vec<String> = Vec::new();
        assert_eq!(
            ids,
            [
                vec!["a".to_owned()], // the parent's own declarations
                vec!["a".to_owned()], // &            -> :is(.foo)
                vec!["a".to_owned()], // &.bar        -> :is(.foo).bar
                vec!["d".to_owned()], // & > .baz     -> :is(.foo) > .baz
                none,                 // &:hover     -> :is(.foo):hover, no hover state
                vec!["s".to_owned()], // > .qux       -> :is(.foo) > .qux
                vec!["e".to_owned()], // .plain       -> :is(.foo) .plain
                vec!["p".to_owned()], // > div > span > b
            ]
        );
        // Each nested rule's specificity counts the parent through the implied
        // `:is(.foo)`, which is (0,1,0). `&` on its own would be (0,0,0)
        // without it, and `& > .baz` counts two classes, not a class and a
        // type. The last rule has three type selectors and so separates the
        // two columns.
        let specificity = |classes, types| Specificity {
            ids: 0,
            classes,
            types,
        };
        assert_eq!(matched[1].1, specificity(1, 0)); // &            -> :is(.foo)
        assert_eq!(matched[2].1, specificity(2, 0)); // &.bar        -> :is(.foo).bar
        assert_eq!(matched[3].1, specificity(2, 0)); // & > .baz     -> :is(.foo) > .baz
        assert_eq!(matched[5].1, specificity(2, 0)); // > .qux       -> :is(.foo) > .qux
        assert_eq!(matched[6].1, specificity(2, 0)); // .plain       -> :is(.foo) .plain
        assert_eq!(matched[7].1, specificity(1, 3)); // > div > span > b
    }

    /// CSS Nesting §3.2: several levels stack, each resolving against the
    /// already-resolved text of the level above it. The two enclosing rules
    /// declare nothing of their own, so they still appear as rules.
    #[test]
    fn nesting_levels_stack() {
        let matched = matched_ids(NESTED_DOM, "div { > span { > b { color: red } } }");

        assert_eq!(matched.len(), 3);
        assert_eq!(matched[2].0, ["p"]);
    }

    /// CSS Nesting §3.3: a nested group rule keeps its at-rule's meaning, and
    /// the rules in it apply to the parent selector under the group's
    /// condition. `.foo` declares nothing of its own, so it appears as an empty
    /// first rule and each group rule contributes one rule after it.
    #[test]
    fn nested_group_rules_apply_to_the_parent_selector() {
        let sheet = parse_stylesheet(
            ".foo { \
               @media screen and (min-width: 700px) { color: green } \
               @media screen and (min-width: 700px) { &.bar { color: blue } } \
               @supports (display: grid) { display: grid } }",
        );

        assert!(sheet.diagnostics.is_empty(), "{:?}", sheet.diagnostics);
        assert_eq!(sheet.rules.len(), 4);
        // The parent itself, with nothing of its own.
        assert!(sheet.rules[0].declarations.is_empty());
        // A declaration written directly in the group rule becomes a nested
        // declarations rule under the group's media condition.
        assert_eq!(sheet.rules[1].declarations[0].value, "green");
        assert_eq!(sheet.rules[1].media, ["screen and (min-width: 700px)"]);
        assert_eq!(sheet.rules[2].declarations[0].value, "blue");
        assert_eq!(sheet.rules[2].media, ["screen and (min-width: 700px)"]);
        assert_eq!(sheet.rules[3].declarations[0].value, "grid");
        assert!(sheet.rules[3].media.is_empty());
    }

    /// CSS Nesting §3.4/§5: declarations that follow a nested rule become a
    /// nested declarations rule. It must match exactly what its parent matches,
    /// so it carries the parent's selectors, and it is emitted after the nested
    /// rules, which is §3.4's Order Of Appearance rule.
    #[test]
    fn declarations_after_a_nested_rule_form_a_nested_declarations_rule() {
        let sheet = parse_stylesheet("article { color: green; & { color: blue } color: red }");

        assert!(sheet.diagnostics.is_empty(), "{:?}", sheet.diagnostics);
        let values: Vec<&str> = sheet
            .rules
            .iter()
            .map(|rule| rule.declarations[0].value.as_str())
            .collect();
        // green (the parent's own), then the nested rule, then the trailing
        // run as a nested declarations rule.
        assert_eq!(values, ["green", "blue", "red"]);
        let orders: Vec<u64> = sheet.rules.iter().map(|rule| rule.source_order).collect();
        assert_eq!(orders, [0, 1, 2]);
    }

    /// CSS Syntax §5.5.5: `div:hover { ... }` inside a block is a nested style
    /// rule, not a declaration named `div`, because no property takes a
    /// {}-block as part of a longer value.
    #[test]
    fn a_declaration_shaped_like_a_rule_is_a_nested_rule() {
        let sheet =
            parse_stylesheet("a { color: red; b:hover { color: blue } &[data-x] { color: lime } }");

        assert!(sheet.diagnostics.is_empty(), "{:?}", sheet.diagnostics);
        assert_eq!(sheet.rules.len(), 3);
        assert_eq!(sheet.rules[0].declarations[0].value, "red");
        assert_eq!(sheet.rules[1].declarations[0].value, "blue");
        assert_eq!(sheet.rules[2].declarations[0].value, "lime");
    }

    /// The custom-property exception in CSS Syntax §5.5.5: `--foo: {}` is a
    /// declaration everywhere, because a custom property keeps its whole token
    /// stream.
    #[test]
    fn a_custom_property_may_still_hold_a_block() {
        let sheet = parse_stylesheet("a { --block: { color: red }; color: blue }");

        assert!(sheet.diagnostics.is_empty(), "{:?}", sheet.diagnostics);
        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(sheet.rules[0].declarations[0].name, "--block");
        assert_eq!(sheet.rules[0].declarations[0].value, "{ color: red }");
        assert_eq!(sheet.rules[0].declarations[1].value, "blue");
    }

    /// CSS Nesting §3.1: an invalid nested rule is ignored along with its
    /// contents but does not invalidate its parent rule. `&div` is the spec's
    /// example of one: a type selector has to come first in a compound.
    #[test]
    fn an_invalid_nested_rule_does_not_invalidate_its_parent() {
        let sheet = parse_stylesheet("a { color: red; &div { color: blue } }");

        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(sheet.rules[0].declarations[0].value, "red");
        assert_eq!(sheet.diagnostics.len(), 1);
    }

    /// A `style` attribute is a declaration list, not a block that may nest
    /// (CSS Nesting applies to style rules, not to presentational hints).
    #[test]
    fn a_style_attribute_does_not_nest() {
        let (declarations, _) = parse_declaration_list("color: red; b { color: blue }");
        assert_eq!(declarations.len(), 1);
        assert_eq!(declarations[0].value, "red");
    }

    #[test]
    fn parses_component_values_and_trailing_important() {
        let sheet = parse_stylesheet(
            r#"a:is(.x, .y) {
                COLOR: rgb(1, 2, 3);
                content: "a;!important";
                --Theme: calc(1px + var(--gap));
                border-color: red ! important;
            }"#,
        );

        assert!(sheet.diagnostics.is_empty(), "{:?}", sheet.diagnostics);
        let declarations = &sheet.rules[0].declarations;
        assert_eq!(declarations[0].name, "color");
        assert_eq!(declarations[0].value, "rgb(1, 2, 3)");
        assert!(!declarations[1].important);
        assert_eq!(declarations[1].value, r#""a;!important""#);
        assert_eq!(declarations[2].name, "--Theme");
        assert_eq!(declarations[2].value, "calc(1px + var(--gap))");
        assert!(declarations[3].important);
        assert_eq!(declarations[3].value, "red");
    }

    #[test]
    fn invalid_rules_recover_at_the_next_rule() {
        let sheet = parse_stylesheet("div, :unsupported() { color: red } p { color: blue }");

        assert_eq!(sheet.rules.len(), 2);
        assert_eq!(sheet.rules[1].declarations[0].value, "blue");
        assert!(sheet.diagnostics.is_empty());
    }

    #[test]
    fn empty_ordinary_inline_declarations_do_not_override_styles() {
        let (declarations, diagnostics) =
            parse_declaration_list("display: ; color: red; --optional: ; visibility: ;");

        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(
            declarations
                .iter()
                .map(|declaration| declaration.name.as_str())
                .collect::<Vec<_>>(),
            ["color", "--optional"]
        );
    }

    #[test]
    fn records_top_level_layer_order_and_membership() {
        let sheet = parse_stylesheet(
            "@layer reset, theme; @layer theme { .x { color: blue } } \
             @layer reset { .x { color: red } }",
        );

        assert!(sheet.diagnostics.is_empty(), "{:?}", sheet.diagnostics);
        assert_eq!(
            sheet.layer_order,
            vec![
                LayerName::Named(vec!["reset".to_owned()]),
                LayerName::Named(vec!["theme".to_owned()]),
            ]
        );
        assert_eq!(sheet.rules.len(), 2);
        assert_eq!(sheet.rules[0].layer, Some(sheet.layer_order[1].clone()));
        assert_eq!(sheet.rules[1].layer, Some(sheet.layer_order[0].clone()));
    }

    #[test]
    fn unsupported_hierarchical_layers_are_not_applied_as_flat_layers() {
        let sheet =
            parse_stylesheet("@layer framework.layout { .x { color: red } } .x { color: blue }");

        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(sheet.rules[0].declarations[0].value, "blue");
        assert_eq!(sheet.diagnostics.len(), 1);
    }

    #[test]
    fn recognizes_css_wide_keywords_as_decoded_single_identifiers() {
        assert_eq!(
            css_wide_keyword(r"\69 nherit"),
            Some(CssWideKeyword::Inherit)
        );
        assert_eq!(
            css_wide_keyword("revert-layer"),
            Some(CssWideKeyword::RevertLayer)
        );
        assert_eq!(css_wide_keyword("inherit extra"), None);
        assert_eq!(css_wide_keyword("var(--inherit)"), None);
    }

    /// CSS Syntax §3.3: comments are removed while preprocessing the token
    /// stream, so a comment is not part of a declaration's value and the
    /// declaration is still valid.
    #[test]
    fn comments_inside_declaration_values_are_not_part_of_the_value() {
        let sheet =
            parse_stylesheet("a { color: red /* trailing */; /* lead */ background: /*x*/ blue }");
        assert!(sheet.diagnostics.is_empty(), "{:?}", sheet.diagnostics);
        let declarations = &sheet.rules[0].declarations;
        assert_eq!(declarations[0].value, "red");
        assert_eq!(declarations[1].value, "blue");
    }

    /// CSS Variables 3 §2: a custom property keeps its whole token stream, so
    /// braces, semicolons inside them, and trailing comments are all part of
    /// the value, and `!important` is still recognized.
    #[test]
    fn custom_properties_keep_braces_semicolons_and_trailing_comments() {
        let sheet = parse_stylesheet(
            "a { --block: { color: red; content: ';' }; --plain: 1px /*c*/; --bang: 2px !important }",
        );

        assert!(sheet.diagnostics.is_empty(), "{:?}", sheet.diagnostics);
        let declarations = &sheet.rules[0].declarations;
        assert_eq!(declarations[0].value, "{ color: red; content: ';' }");
        assert_eq!(declarations[1].value, "1px");
        assert_eq!(declarations[2].value, "2px");
        assert!(declarations[2].important);
    }

    /// CSS Syntax §3.2: at-keywords are ASCII case-insensitive, and
    /// conditional group rules nest freely.
    #[test]
    fn at_rule_names_are_ascii_case_insensitive_and_nest() {
        let sheet = parse_stylesheet(
            "@LAYER base { @Media screen { @SUPPORTS (display: grid) { a { color: red } } } }",
        );

        assert!(sheet.diagnostics.is_empty(), "{:?}", sheet.diagnostics);
        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(sheet.rules[0].declarations[0].value, "red");
        assert_eq!(sheet.rules[0].media, ["screen"]);
        assert_eq!(
            sheet.rules[0].layer,
            Some(LayerName::Named(vec!["base".to_owned()]))
        );
    }

    /// CSS Web Animations 1 §4.1: a keyframe block holds `<keyframe-selector>`
    /// lists, and the body is not a style rule block. `from`/`to` and
    /// percentages must not be reported as selector errors, and the at-rule
    /// must not poison the rules that follow it.
    #[test]
    fn keyframes_bodies_accept_keyframe_selector_lists() {
        let sheet = parse_stylesheet(
            "@keyframes spin { 0%, 50% { opacity: 0 } from, to { opacity: 1 } } \
             @-webkit-keyframes spin { to { opacity: 1 } } a { color: red }",
        );

        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(sheet.rules[0].declarations[0].value, "red");
    }

    /// CSS Fonts 3 §2.2: an `@font-face` block is a descriptor list, so its
    /// contents are never selector syntax, and it must not swallow or poison
    /// the rules around it.
    #[test]
    fn font_face_bodies_are_descriptor_lists() {
        let sheet = parse_stylesheet(
            "@font-face { font-family: 'X'; src: url(a.woff2) format('woff2'); \
             font-weight: 400 700 } a { color: red }",
        );

        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(sheet.rules[0].declarations[0].value, "red");
    }

    /// The diagnostic messages, so a test can name the one it expects without
    /// repeating a string that is supposed to be the established shape.
    fn messages(sheet: &super::StyleSheet) -> Vec<&str> {
        sheet
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.message.as_str())
            .collect()
    }

    /// The `name: value` of every declaration that reached a cascade, across all
    /// rules, so a recovery test reads as the list of declarations that survived
    /// rather than as indices into a nested structure.
    fn cascaded(sheet: &super::StyleSheet) -> Vec<String> {
        sheet
            .rules
            .iter()
            .flat_map(|rule| rule.declarations.iter())
            .map(|declaration| format!("{}: {}", declaration.name, declaration.value))
            .collect()
    }

    /// The diagnostic positions, as `(line, column)` with the column made
    /// 0-based, so a test can index into the source line with it and so a
    /// reported position and a character in the text cannot be confused.
    ///
    /// `cssparser` counts a column from 1 (CSS Syntax §3.2's "starting at 1 for
    /// the first character of the line"), and this crate reports that number
    /// unchanged, so the conversion belongs here rather than in every caller.
    fn positions(sheet: &super::StyleSheet) -> Vec<(u32, u32)> {
        sheet
            .diagnostics
            .iter()
            .map(|diagnostic| (diagnostic.line, diagnostic.column.saturating_sub(1)))
            .collect()
    }

    /// A *kept* declaration knows where it came from, at the same position and
    /// under the same convention a *discarded* one is reported at.
    ///
    /// This is the whole point of [`super::DeclarationLocation`]: a declaration
    /// the parser accepted can still be rejected later - by the property
    /// grammar, or by the cascade - and a report about that has to name a
    /// `file:line`. Before the location was threaded, a rejected declaration
    /// could only be reported as "somewhere in this sheet".
    #[test]
    fn a_kept_declaration_knows_where_it_starts() {
        let source = "a {\n  color: red;\n  *zoom: 1;\n  margin: 1px\n}";
        let sheet = parse_stylesheet(source);

        assert_eq!(cascaded(&sheet), ["color: red", "margin: 1px"]);
        let rule = &sheet.rules[0];
        assert_eq!(
            rule.declarations.len(),
            rule.declaration_locations.len(),
            "the two vectors are parallel by construction"
        );
        let locations: Vec<(u32, u32)> = rule
            .declaration_locations
            .iter()
            .map(|location| (location.line, location.column))
            .collect();
        // One-based lines, and the columns count from 1 exactly as
        // `StyleSheetDiagnostic::column` does.
        assert_eq!(locations, vec![(2, 3), (4, 3)], "{source}");
        // And the reported position really is the `c` and the `m`.
        for (index, name) in ["color", "margin"].into_iter().enumerate() {
            let location = rule.declaration_locations[index];
            let line = source.lines().nth(location.line as usize - 1).unwrap();
            let start = location.column as usize - 1;
            assert!(line[start..].starts_with(name), "{line:?} at {start}");
        }
    }

    /// The discard diagnostic for the same block points at the discarded
    /// declaration, and the kept ones point at themselves, so one convention
    /// covers both. The discarded declaration's position is the one the
    /// diagnostic already used, and the kept ones come from the same
    /// `ParserState`.
    #[test]
    fn a_kept_and_a_discarded_declaration_use_the_same_position_convention() {
        let sheet = parse_stylesheet("a { *one: 1; color: red }");

        assert_eq!(positions(&sheet), vec![(1, 4)]);
        let rule = &sheet.rules[0];
        let location = rule.location_of("color").expect("the kept declaration");
        // `*one: 1; ` is eight characters, so the `c` of `color` is the ninth and
        // its one-based column is 14 - which is the same numbering
        // `a_discarded_declaration_is_reported_where_it_starts` checks against
        // for the `*` at column 4.
        assert_eq!((location.line, location.column), (1, 14));
    }

    /// `location_of` answers for the declaration that *wins*, which is the one
    /// whose value the cascade used: a later declaration of the same property in
    /// one block is the one a report about that property is about.
    #[test]
    fn location_of_names_the_declaration_that_wins() {
        let sheet = parse_stylesheet("a {\n  color: red;\n  color: green\n}");
        let location = sheet.rules[0].location_of("color").expect("a location");

        assert_eq!(location.line, 3);
        assert_eq!(sheet.rules[0].declarations[1].value, "green");
    }

    /// A declaration list parsed on its own - an HTML `style` attribute, or a
    /// `@font-face` descriptor list - carries the same positions.
    #[test]
    fn a_declaration_list_carries_positions_too() {
        let (declarations, locations, diagnostics) =
            super::parse_declaration_list_with_locations("color: red; margin: 1px").into_parts();

        assert_eq!(declarations.len(), 2);
        assert_eq!(declarations.len(), locations.len());
        assert!(diagnostics.is_empty());
        assert_eq!(
            locations
                .iter()
                .map(|location| (location.line, location.column))
                .collect::<Vec<_>>(),
            vec![(1, 1), (1, 13)]
        );
    }

    /// CSS Syntax §5.4.4: an invalid declaration is discarded **on its own** and
    /// parsing continues with the next declaration in the same block. The
    /// star hack is the case that matters on the real web: 134 of them sit in
    /// the production corpus, and the one that kept a site's top bar horizontal
    /// was written `display:inline-block;*display:inline;*zoom:1;min-width:50px`.
    ///
    /// What this pins is the *rest of the block surviving*, which is the whole
    /// cost of the bug: a list item falling back to the user-agent `list-item`
    /// is a vertically stacked page, not a cosmetic difference. The discard is
    /// still required - browsers do not support the hack either - and the
    /// declarations around it are what the page is made of.
    #[test]
    fn an_invalid_declaration_does_not_cost_the_block_its_others() {
        let sheet = parse_stylesheet(
            ".item{display:inline-block;*display:inline;*zoom:1;min-width:50px;text-align:center}",
        );

        assert_eq!(
            cascaded(&sheet),
            [
                "display: inline-block",
                "min-width: 50px",
                "text-align: center",
            ]
        );
        // One diagnostic per discarded declaration, so two star hacks are two
        // diagnostics and not one and not three.
        assert_eq!(sheet.diagnostics.len(), 2, "{:?}", messages(&sheet));
        for diagnostic in &sheet.diagnostics {
            assert!(
                diagnostic.message.starts_with("invalid declaration:"),
                "{:?}",
                messages(&sheet)
            );
        }
    }

    /// The diagnostic is positioned at the *declaration*, not at the token that
    /// ended it. This is the difference between a report a reader can act on and
    /// one that sends them to look at a `;` which is entirely valid: the two
    /// offsets are 1-based and name the first `*` in each hack.
    #[test]
    fn a_discarded_declaration_is_reported_where_it_starts() {
        let source = "a { *one: 1; color: red; *two: 2; margin: 1px }";
        let sheet = parse_stylesheet(source);

        assert_eq!(cascaded(&sheet), ["color: red", "margin: 1px"]);
        assert_eq!(positions(&sheet), vec![(1, 4), (1, 25)]);
        // And the offsets really are the `*`s, which is the only way to know the
        // position means anything. The reported column counts from 1, so the
        // two conventions cannot be silently swapped.
        for (line_number, column) in positions(&sheet) {
            let line = source.lines().nth(line_number as usize - 1).unwrap();
            assert_eq!(
                line.chars().nth(column as usize),
                Some('*'),
                "column {column} of {line:?} is not the declaration's first character"
            );
        }
    }

    /// The recovery boundary is `consume a component value` (CSS Syntax
    /// §5.4.7), so a `;` or a `}` inside a function, a `()` group, a `[]` group
    /// or a `{}` block is part of the value and not the end of the declaration.
    /// Getting this wrong is worse than the bug being fixed: a boundary that
    /// stops inside a block turns one recoverable error into a mangled tree.
    #[test]
    fn a_semicolon_or_brace_inside_a_block_is_not_a_declaration_boundary() {
        let sheet = parse_stylesheet(
            "a { \
               background: image-set(\"a;b.png\" 1x, url(\"c}d.png\") 2x); \
               width: calc(1px; 2px); \
               height: var(--x, [a;b]); \
               --raw: {a;b;c}; \
               color: red }",
        );

        assert!(sheet.diagnostics.is_empty(), "{:?}", messages(&sheet));
        assert_eq!(
            cascaded(&sheet),
            [
                r#"background: image-set("a;b.png" 1x, url("c}d.png") 2x)"#,
                "width: calc(1px; 2px)",
                "height: var(--x, [a;b])",
                "--raw: {a;b;c}",
                "color: red",
            ]
        );
    }

    /// The same boundary has to hold while *discarding*, or the skip for a bad
    /// declaration stops at the `;` inside a function and the rest of the block
    /// is thrown out with it. A discarded declaration runs to the next top-level
    /// `;` and no further.
    #[test]
    fn discarding_a_declaration_stops_at_the_next_top_level_semicolon() {
        let sheet = parse_stylesheet(
            "a { *bad: image-set(\"x;y\" 1x); color: red; \
               *also-bad: calc(1px; 2px); margin: 1px }",
        );

        assert_eq!(cascaded(&sheet), ["color: red", "margin: 1px"]);
        assert_eq!(sheet.diagnostics.len(), 2, "{:?}", messages(&sheet));
    }

    /// §5.4.4's recovery is the *only* level of the grammar that discards one
    /// declaration and resumes at the next, so it has to behave the same way
    /// wherever a declaration list appears: a style rule's block (with CSS
    /// Nesting §3.1 rules in it too), a nested rule's block, a nested group
    /// rule's block, a descriptor list, and the `style` attribute. A recovery
    /// right in one of these and wrong in the others is the failure this pins.
    #[test]
    fn a_discarded_declaration_costs_nothing_at_any_level_that_has_a_list() {
        for (source, kept) in [
            // A style rule's block.
            ("a { *bad: 1; color: red }", "color: red"),
            // A nested style rule's block.
            ("a { b { *bad: 1; color: red } }", "color: red"),
            // A nested group rule's block, which is a `<block-contents>`.
            ("a { @media screen { *bad: 1; color: red } }", "color: red"),
            // A descriptor list. Its declarations are parsed and then dropped
            // for the already-reported reason that this engine registers no
            // font, so what is asserted is that the discard is reported once -
            // not that a value came out, since nothing can.
            ("@font-face { *bad: 1; font-family: X }", ""),
        ] {
            let sheet = parse_stylesheet(source);
            let cascaded = cascaded(&sheet);
            if kept.is_empty() {
                assert!(
                    cascaded.is_empty(),
                    "{source:?} cascades {:?}, which a descriptor list must not",
                    cascaded
                );
            } else {
                assert_eq!(
                    cascaded,
                    [kept],
                    "{source:?} loses a declaration that follows a discarded one"
                );
            }
            // Exactly one discard diagnostic, and it is the only thing this test
            // counts: a descriptor list also reports the already-known reason
            // its whole block is unusable, which is a different fact and is
            // pinned by `a_descriptor_error_is_reported_in_every_declaration_list_at_rule`.
            let discards: Vec<&str> = messages(&sheet)
                .into_iter()
                .filter(|message| message.starts_with("invalid declaration:"))
                .collect();
            assert_eq!(
                discards.len(),
                1,
                "{source:?} reports {:#?}",
                messages(&sheet)
            );
        }
        // And the `style` attribute, which is a declaration list that never
        // becomes a rule at all, so the declarations are the only observable.
        let (declarations, diagnostics) = parse_declaration_list("*bad: 1; color: red");
        assert_eq!(
            declarations
                .iter()
                .map(|declaration| declaration.value.as_str())
                .collect::<Vec<_>>(),
            ["red"],
            "a discarded declaration in a style attribute costs the ones after it"
        );
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    }

    /// Every at-rule whose block is a `<declaration-list>` has to be walked, not
    /// just `@font-face`. The reason is that a `@page` block full of legacy
    /// hacks used to reach the cascade producing exactly one diagnostic - about
    /// `@page` - and nothing at all about the bad declarations inside it, which
    /// is a partial drop of the defect `at_rules` exists to end.
    #[test]
    fn a_descriptor_error_is_reported_in_every_declaration_list_at_rule() {
        for name in crate::at_rules::DECLARATION_LIST_AT_RULES
            .iter()
            .map(|(name, _)| *name)
        {
            let source = format!("@{name} {{ *zoom: 1; size: a4 }} a {{ color: red }}");
            let sheet = parse_stylesheet(&source);

            assert_eq!(
                cascaded(&sheet),
                ["color: red"],
                "@{name} must not affect the rules around it"
            );
            assert_eq!(
                messages(&sheet),
                [
                    "invalid declaration: a declaration starts with a property name, and '*' is not one",
                    &format!("@{name} is parsed but not evaluated yet"),
                ],
                "@{name}'s descriptor list reports its own syntax errors"
            );
        }
    }

    /// A declaration whose *value* is invalid is a different question from one
    /// whose name is, and the answer is that it is not the same defect: the
    /// syntax layer stores a value as the token stream it read, so
    /// `color: notacolor` is a well-formed declaration and the typed grammar
    /// rejects it later, on its own, without touching the declarations beside
    /// it. Measured over the production corpus: 2053 declarations reach the
    /// typed stage and 6 of them are rejected, and the 151 that never reach the
    /// cascade are value truncations inside `var()`/`oklch()`, not blocks
    /// emptied by a bad name.
    ///
    /// So the *parser* keeps both, and the two-stage split is what keeps
    /// `color: notacolor` from costing `margin: 10px` - which is the far more
    /// common shape on the real web and would be a much larger version of the
    /// same bug if it were true.
    #[test]
    fn an_invalid_value_does_not_cost_the_block_its_other_declarations() {
        let sheet = parse_stylesheet("a { color: notacolor; margin: 10px; padding: 1px }");

        assert!(sheet.diagnostics.is_empty(), "{:?}", messages(&sheet));
        assert_eq!(
            cascaded(&sheet),
            ["color: notacolor", "margin: 10px", "padding: 1px"]
        );
        // The invalid value is rejected at the typed stage instead, and only
        // that declaration. `padding-top` rather than `margin`, because a
        // shorthand has no typed grammar of its own: it is expanded into
        // longhands and `None` here means "expanded", not "untyped".
        assert!(
            crate::properties::parse_typed_property("color", "notacolor")
                .is_some_and(|result| result.is_err()),
            "an invalid value is the typed stage's error, not the parser's"
        );
        assert!(
            crate::properties::parse_typed_property("padding-top", "10px")
                .is_some_and(|result| result.is_ok())
        );
    }

    /// The star hack stays a parse error. Skipping a declaration that starts with
    /// `*` and carrying on as though the next one were authoritative would be
    /// exactly the "unsupported CSS silently accepted" outcome this project
    /// treats as its worst, and it is not what browsers do either.
    #[test]
    fn the_star_hack_is_still_reported_rather_than_accepted() {
        let sheet = parse_stylesheet("a { *zoom: 1; color: red }");

        assert_eq!(cascaded(&sheet), ["color: red"]);
        assert_eq!(
            messages(&sheet),
            ["invalid declaration: a declaration starts with a property name, and '*' is not one"]
        );
        // And nothing anywhere claims a `*`-prefixed property reached the
        // cascade.
        for rule in &sheet.rules {
            for declaration in &rule.declarations {
                assert!(
                    !declaration.name.starts_with('*'),
                    "{} reached the cascade",
                    declaration.name
                );
            }
        }
    }

    /// An at-rule that reaches the end of a block without its `;` or its `{` is
    /// §5.4.2's "this is a parse error, return the at-rule", so the at-rule is
    /// still reported rather than dropped. And a `@layer` statement whose name
    /// does not parse is discarded *and* says so: it used to come out as
    /// `Err(())`, which made cssparser report `unexpected token: Semicolon` and
    /// blame a token that is entirely valid.
    #[test]
    fn an_at_rule_prelude_that_does_not_parse_is_still_reported() {
        let sheet = parse_stylesheet("@layer; a { @layer; color: red }");

        assert_eq!(cascaded(&sheet), ["color: red"]);
        assert_eq!(messages(&sheet), ["invalid cascade layer name"; 2]);
    }

    /// A statement at-rule is §5.4.2's at-rule with no block, and every level
    /// that reads one has to report it the same way. A nested statement at-rule
    /// used to come out as `Err(())`, which made cssparser substitute its own
    /// `unexpected token: Semicolon` - blaming a valid `;` - and it was round
    /// five's `@import` case that first pinned it. The block form of the same
    /// at-rule still reports the reason it cannot be used, so the two spellings
    /// of a discard keep their own wording and only the *position* and the
    /// nesting change.
    #[test]
    fn a_nested_statement_at_rule_is_reported_at_every_level() {
        for (source, expected) in [
            (
                "a { @import url(b.css); color: red }",
                "@import is parsed but not evaluated yet",
            ),
            (
                "@import url(b.css); a { color: red }",
                "@import is parsed but not evaluated yet",
            ),
        ] {
            let sheet = parse_stylesheet(source);
            assert_eq!(cascaded(&sheet), ["color: red"], "{source:?}");
            assert_eq!(messages(&sheet), [expected], "{source:?}");
            // The position is the at-keyword, in both directions, which is what
            // a reader has to go and delete.
            let (line, column) = positions(&sheet)[0];
            let source_line = source.lines().nth(line as usize - 1).unwrap();
            assert_eq!(
                source_line.chars().nth(column as usize),
                Some('@'),
                "{source:?} reports at {line}:{column}, which is not the at-keyword"
            );
        }
    }

    /// An at-rule found inside a descriptor list is a discard like any other, so
    /// it is reported. §5.4.5 does append at-rules to the list it returns, but a
    /// descriptor list this engine cannot use has nowhere to put one, and
    /// dropping it silently would be the same defect one level in.
    #[test]
    fn an_at_rule_inside_a_descriptor_list_is_reported() {
        let sheet = parse_stylesheet("@font-face { @nonesuch { } font-family: X }");

        assert_eq!(
            messages(&sheet),
            [
                "@nonesuch is not an at-rule this engine knows",
                "@font-face is parsed but not evaluated yet",
            ]
        );
    }

    /// Every recognised at-rule this engine does not evaluate is reported, once
    /// per occurrence, naming itself. This is the defect `css.font-face` and
    /// `css.animations` in the capability registry are about: 223 `@font-face`
    /// and 249 `@keyframes` blocks of real production CSS were parsed and then
    /// discarded with nothing anywhere saying why, and this project's law is
    /// that unsupported CSS must never be silently ignored.
    #[test]
    fn every_recognised_but_unimplemented_at_rule_is_reported() {
        for name in [
            "font-face",
            "keyframes",
            "-webkit-keyframes",
            "page",
            "property",
            "counter-style",
            "namespace",
            "color-profile",
            "container",
            "starting-style",
            "position-try",
            "view-transition",
            "viewport",
            "when",
            "else",
            "scope",
            "custom-media",
            "font-palette-values",
            "font-feature-values",
        ] {
            // A block at-rule and a statement at-rule take different paths
            // through the parser, so both are exercised for every name.
            let sheet = parse_stylesheet(&format!("@{name} {{}} @{name} prelude"));
            let expected = format!("@{name} is parsed but not evaluated yet");
            assert_eq!(
                messages(&sheet),
                [expected.as_str(), expected.as_str()],
                "@{name} is recognised and unimplemented, so both spellings are reported"
            );
        }
        // A statement at-rule with a real prelude, which is the form
        // `@import` and `@namespace` are written in.
        let sheet =
            parse_stylesheet("@import url(a.css); @namespace svg url(http://www.w3.org/2000/svg)");
        assert_eq!(
            messages(&sheet),
            [
                "@import is parsed but not evaluated yet",
                "@namespace is parsed but not evaluated yet",
            ]
        );
    }

    /// Every recognised at-rule that is not evaluated has a case in
    /// `every_recognised_but_unimplemented_at_rule_is_reported`, so adding an
    /// at-rule to the table without deciding what it reports fails here rather
    /// than in a browser.
    #[test]
    fn every_unimplemented_at_rule_is_covered_by_the_reporting_test() {
        for name in [
            "font-face",
            "keyframes",
            "page",
            "property",
            "counter-style",
            "namespace",
            "color-profile",
            "container",
            "starting-style",
            "position-try",
            "view-transition",
            "viewport",
            "when",
            "else",
            "scope",
            "custom-media",
            "font-palette-values",
            "font-feature-values",
        ] {
            let source = format!("@{name} {{}}");
            assert_eq!(
                messages(&parse_stylesheet(&source)),
                [format!("@{name} is parsed but not evaluated yet").as_str()],
                "@{name} has a case in every_recognised_but_unimplemented_at_rule_is_reported"
            );
        }
        // `@charset` is the one recognised at-rule with no case, and it has none
        // on purpose: CSS Syntax §4.1 has the tokenizer consume it before any
        // at-rule parser runs, so this crate never sees it. Asserting that here
        // is what stops someone adding it to the table and expecting a
        // diagnostic this code cannot produce.
        assert!(messages(&parse_stylesheet("@charset \"utf-8\"; a { color: red }")).is_empty());
    }

    /// An at-rule no specification defines is reported too, and with different
    /// words, because "I have never heard of `@foo`" and "I know exactly what
    /// `@foo` means and cannot do it yet" send a reader to different places.
    #[test]
    fn an_unrecognised_at_rule_is_reported_differently() {
        let sheet =
            parse_stylesheet("@nonesuch { color: red } @-webkit-nonesuch x; a { color: red }");

        assert_eq!(
            messages(&sheet),
            [
                "@nonesuch is not an at-rule this engine knows",
                "@-webkit-nonesuch is not an at-rule this engine knows",
            ]
        );
        // The rules around it are untouched: an unknown at-rule is invalid and
        // ignored (CSS Syntax §4.2), and it does not invalidate its neighbours.
        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(sheet.rules[0].declarations[0].value, "red");
    }

    /// The diagnostic has to survive the grouping, in every direction and at
    /// every level: an at-rule inside a conditional group rule is reported
    /// exactly as one at the top level is, and a conditional group rule inside
    /// an unimplemented at-rule's group is not a place it can hide. This is the
    /// more serious half of the defect, because the group rule is the one
    /// production real stylesheets put at-rules inside.
    #[test]
    fn at_rule_diagnostics_survive_conditional_grouping() {
        // Inside `@media`, inside `@supports`, and inside both, in either order.
        for source in [
            "@media screen { @font-face { font-family: 'X' } }",
            "@supports (display: grid) { @font-face { font-family: 'X' } }",
            "@media screen { @supports (display: grid) { @keyframes spin { to { opacity: 1 } } } }",
            "@supports (display: grid) { @media screen { @keyframes spin { to { opacity: 1 } } } }",
            // And inside a cascade layer, which is a group rule too.
            "@layer base { @font-face { font-family: 'X' } }",
            "@media screen { @layer base { @keyframes spin { to { opacity: 1 } } } }",
            // Nested in a style rule, where the block is a `<block-contents>`.
            "a { @media screen { @font-face { font-family: 'X' } } }",
        ] {
            let sheet = parse_stylesheet(source);
            assert!(
                !messages(&sheet).is_empty(),
                "{source:?} drops an at-rule with no diagnostic"
            );
            assert!(
                messages(&sheet)
                    .iter()
                    .any(|message| message.ends_with("is parsed but not evaluated yet")),
                "{source:?} reports {:#?}",
                sheet.diagnostics
            );
        }
        // The grouping itself is still not broken by reporting: an at-rule
        // inside `@media` contributes no rules, and the rules beside it do.
        let sheet =
            parse_stylesheet("@media screen { @font-face { font-family: 'X' } } b { color: red }");
        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(sheet.rules[0].declarations[0].value, "red");
    }

    /// A *partially* implemented at-rule dropping only part of itself is the
    /// same silent drop in a harder form, and this is that case: `@font-face`
    /// is recognised, and its descriptor list is *parsed*, so a descriptor with
    /// a syntax error is found and was being thrown away with the rest of the
    /// block. A parse error that was found must be reported even when the
    /// construct it was found in is being discarded for another reason.
    #[test]
    fn a_font_face_descriptor_error_is_not_swallowed_by_the_unimplemented_report() {
        let sheet = parse_stylesheet("@font-face { font-family 'X'; src: url(a.woff2) }");

        assert_eq!(
            messages(&sheet),
            [
                r#"unexpected token: QuotedString("X")"#,
                "@font-face is parsed but not evaluated yet",
            ],
            "both the descriptor's own error and why the block is unusable"
        );
        // And the same error is reported when the block is inside a group rule,
        // which is where 223 of them sit in the production corpus.
        let nested = parse_stylesheet("@media screen { @font-face { font-family 'X' } }");
        assert_eq!(messages(&nested), messages(&sheet));
    }

    /// `@layer` is evaluated at the top level and *not* inside a style rule, so
    /// the generic "not evaluated yet" wording would be a false statement about
    /// an at-rule the engine implements. The partial case gets its own words.
    #[test]
    fn a_nested_layer_block_is_reported_as_partial_rather_than_unknown() {
        let sheet = parse_stylesheet("a { @layer base { color: red } }");

        assert_eq!(
            messages(&sheet),
            ["nested cascade layers are not implemented yet"]
        );
        // A nested layer *statement* is a different thing and is implemented:
        // CSS Cascade 5 §7.1 registers the name whatever encloses it, so it
        // produces no diagnostic and takes part in layer order. It declares the
        // layer, it does not put the enclosing rule in it, so the rule's own
        // `layer` stays `None` - which is the whole difference between §7.1's
        // statement and §7.2's block.
        let statement = parse_stylesheet("@layer reset, theme; a { @layer nested; color: red }");
        assert!(
            messages(&statement).is_empty(),
            "{:?}",
            statement.diagnostics
        );
        assert_eq!(
            statement.layer_order,
            vec![
                LayerName::Named(vec!["reset".to_owned()]),
                LayerName::Named(vec!["theme".to_owned()]),
                LayerName::Named(vec!["nested".to_owned()]),
            ]
        );
        assert_eq!(statement.rules[0].layer, None);
    }

    /// A statement at-rule nested in a style rule used to be discarded with no
    /// diagnostic at all, because the only way for the block walker to skip it
    /// was to fail, and a failure there is cssparser's own error rather than
    /// this crate's.
    #[test]
    fn a_nested_statement_at_rule_is_reported() {
        let sheet = parse_stylesheet("a { @import url(b.css); color: red }");

        assert_eq!(
            messages(&sheet),
            ["@import is parsed but not evaluated yet"]
        );
        // The declaration beside it still applies. It is now a nested
        // declarations rule (`CSS Nesting` §5) because the at-rule interrupted
        // the parent's run of declarations, which is the specified consequence
        // of a nested at-rule being a rule rather than nothing at all.
        assert_eq!(sheet.rules.len(), 2);
        assert!(sheet.rules[0].declarations.is_empty());
        assert_eq!(sheet.rules[1].declarations[0].value, "red");
    }

    /// CSS Conditional Rules 3 §2: a true condition applies the block's rules as
    /// though they were at the group rule's location, and a false one applies
    /// none of them. Before this the condition was never read at all, so both
    /// halves of a progressive enhancement were applied together and the author
    /// got neither.
    #[test]
    fn a_supports_condition_gates_its_block() {
        let source = "@supports (display: grid) { #a { color: red } } \
             @supports (backdrop-filter: blur(2px)) { #b { color: blue } } \
             @supports (color: lab(from red l 1 1%/calc(alpha + 0.1))) { #c { color: lime } }";
        let sheet = parse_stylesheet(source);

        assert!(messages(&sheet).is_empty(), "{:?}", sheet.diagnostics);
        // The first is supported and applies; the second is a property this
        // engine has no grammar and no metadata for, and the third is a colour
        // space this engine cannot represent. Both are complete false answers,
        // not gaps, so neither reports anything - and neither applies.
        //
        // Observed as what the rules match, which is the only thing a consumer
        // can see, rather than as a count of rules.
        let matched = matched_ids("<i id='a'></i><i id='b'></i><i id='c'></i>", source);
        assert_eq!(
            matched.len(),
            1,
            "only the met condition contributes a rule"
        );
        assert_eq!(matched[0].0, ["a"]);
        assert_eq!(sheet.rules[0].declarations[0].value, "red");
    }

    /// A condition that does not match the grammar drops the rule and says so.
    /// Reporting it is the difference between "this engine does not understand
    /// this query" and "this page is broken for no visible reason".
    #[test]
    fn an_invalid_supports_condition_drops_the_block_and_says_so() {
        let sheet =
            parse_stylesheet("@supports display: grid { .a { color: red } } b { color: blue }");

        assert_eq!(
            messages(&sheet),
            ["@supports condition is not valid, so the rule and its contents are dropped"]
        );
        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(sheet.rules[0].declarations[0].value, "blue");
        // Inside a group rule too, and with the rule's own contents reported
        // alongside: an invalid rule inside a group rule does not invalidate
        // the group rule (§3), so the inner error is still the author's problem.
        let nested =
            parse_stylesheet("@media screen { @supports display: grid { .a { color: red } } }");
        assert_eq!(messages(&nested), messages(&sheet));
    }

    /// A condition holding a feature this crate cannot ask about drops the
    /// block and names the capability, rather than guessing either way. This is
    /// the same defect as the at-rule one, reached through a different door: a
    /// partially implemented at-rule is applied, and it lies.
    #[test]
    fn an_unanswerable_supports_condition_drops_the_block_and_says_so() {
        let sheet = parse_stylesheet(
            "@supports at-rule(@font-face) { .a { color: red } } \
             @supports font-tech(color-COLRv1) { .b { color: blue } } c { color: lime }",
        );

        let messages = messages(&sheet);
        assert_eq!(messages.len(), 2, "{messages:#?}");
        assert!(
            messages[0].starts_with("@supports at-rule(@font-face) is not answered: "),
            "{}",
            messages[0]
        );
        // The reason has to name the missing dependency, or the diagnostic is
        // just a refusal.
        assert!(messages[0].contains("render-net"), "{}", messages[0]);
        assert!(messages[0].contains("render-browser"), "{}", messages[0]);
        assert!(
            messages[1].starts_with("@supports font-tech(color-COLRv1) is not answered: "),
            "{}",
            messages[1]
        );
        // Neither block applied, and the rule beside them is untouched.
        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(sheet.rules[0].declarations[0].value, "lime");
    }

    /// The nesting is where this kind of implementation usually breaks, in both
    /// orders and at more than one level: an enclosing `@media` still gates the
    /// rules a true `@supports` lets through, and an enclosing `@supports` still
    /// drops them when it is false.
    #[test]
    fn supports_nests_with_media_in_both_orders() {
        // `@supports` inside `@media`: true condition, and the media query
        // still reaches the rule.
        let supported =
            parse_stylesheet("@media screen { @supports (display: grid) { a { color: red } } }");
        assert!(
            messages(&supported).is_empty(),
            "{:?}",
            supported.diagnostics
        );
        assert_eq!(supported.rules.len(), 1);
        assert_eq!(supported.rules[0].media, ["screen"]);

        // `@media` inside `@supports`: the same.
        let nested =
            parse_stylesheet("@supports (display: grid) { @media screen { a { color: red } } }");
        assert!(messages(&nested).is_empty(), "{:?}", nested.diagnostics);
        assert_eq!(nested.rules.len(), 1);
        assert_eq!(nested.rules[0].media, ["screen"]);

        // A false `@supports` inside a true `@media` drops the media-gated
        // rules, and the media query is not what dropped them.
        let false_supports = parse_stylesheet(
            "@media screen { @supports (text-overflow: clip) { a { color: red } } }",
        );
        assert!(messages(&false_supports).is_empty());
        assert!(false_supports.rules.is_empty());

        // A false `@supports` wrapping a true `@media` drops them too, so the
        // order cannot be used to get a rule past an unmet condition.
        let wrapped = parse_stylesheet(
            "@supports (text-overflow: clip) { @media screen { a { color: red } } }",
        );
        assert!(messages(&wrapped).is_empty());
        assert!(wrapped.rules.is_empty());

        // Two levels of each, and an unmet condition at either level.
        let deep = parse_stylesheet(
            "@media screen { @supports (display: grid) { @media print { \
               @supports (display: grid) { a { color: red } } } } }",
        );
        assert!(messages(&deep).is_empty(), "{:?}", deep.diagnostics);
        assert_eq!(deep.rules.len(), 1);
        assert_eq!(deep.rules[0].media, ["screen", "print"]);

        let deep_false = parse_stylesheet(
            "@media screen { @supports (display: grid) { @media print { \
               @supports (text-overflow: clip) { a { color: red } } } } }",
        );
        assert!(
            messages(&deep_false).is_empty(),
            "{:?}",
            deep_false.diagnostics
        );
        assert!(deep_false.rules.is_empty());
    }

    /// A `@supports` inside a style rule, which is CSS Nesting §3.3's
    /// `<block-contents>`, and where the declarations inside a group rule belong
    /// to the *enclosing* style rule. The condition has to gate them the same
    /// way it gates a rule list.
    #[test]
    fn a_nested_supports_condition_gates_the_enclosing_rules_declarations() {
        let supported = parse_stylesheet("a { @supports (display: grid) { display: grid } }");
        assert!(
            messages(&supported).is_empty(),
            "{:?}",
            supported.diagnostics
        );
        assert_eq!(supported.rules.len(), 2);
        assert_eq!(supported.rules[1].declarations[0].value, "grid");

        let unmet = parse_stylesheet("a { @supports (text-overflow: clip) { display: grid } }");
        assert!(messages(&unmet).is_empty(), "{:?}", unmet.diagnostics);
        assert_eq!(unmet.rules.len(), 1);
        assert!(unmet.rules[0].declarations.is_empty());
    }
}
