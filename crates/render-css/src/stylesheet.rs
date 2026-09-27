//! CSS Syntax based stylesheet and declaration parsing.
//!
//! This module owns syntax recovery and produces selector ASTs once. Property
//! grammar validation and computed-value resolution belong to later stages.

use std::fmt;

use cssparser::{
    AtRuleParser, CowRcStr, DeclarationParser, ParseError, Parser, ParserInput, ParserState,
    QualifiedRuleParser, RuleBodyItemParser, RuleBodyParser, SourceLocation, StyleSheetParser,
    Token,
};

use super::selector::{
    NestedSelectors, SelectorList, parse_nested_selector_list, parse_selector_list,
};

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Declaration {
    pub name: String,
    pub value: String,
    pub important: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StyleRule {
    pub selectors: SelectorList,
    pub declarations: Vec<Declaration>,
    pub layer: Option<LayerName>,
    /// Every enclosing `@media` query, from outermost to innermost.
    pub media: Vec<String>,
    pub source_order: u64,
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
}

impl fmt::Display for RuleParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSelector(message) => write!(formatter, "invalid selector: {message}"),
            Self::InvalidLayerName => formatter.write_str("invalid cascade layer name"),
        }
    }
}

#[derive(Clone, Debug)]
enum ParsedRule {
    Style {
        selectors: SelectorList,
        declarations: Vec<Declaration>,
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
        declarations: Vec<Declaration>,
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
    },
    /// Animation/font at-rules are valid stylesheet rules even when the
    /// compositor does not yet sample their timelines. Keep them in the
    /// parsed rule stream so they do not poison the rest of the stylesheet
    /// with false syntax diagnostics.
    KeyframesBlock {
        name: String,
        location: SourceLocation,
    },
    FontFaceBlock {
        location: SourceLocation,
    },
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
        if prelude.name == "layer" {
            let layers = parse_layer_names(&prelude.value)?;
            if layers.is_empty() {
                return Err(());
            }
            return Ok(ParsedRule::LayerStatement {
                layers,
                location: start.source_location(),
            });
        }
        Ok(ParsedRule::IgnoredAtRule {
            name: prelude.name,
            location: start.source_location(),
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
            return Ok(ParsedRule::IgnoredAtRule {
                name: prelude.name,
                location,
            });
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
            location,
        });
    }

    if prelude.name == "supports" {
        let (body, diagnostics) = parse_group_body(input, next_anonymous_layer, parent);
        return Ok(ParsedRule::SupportsBlock {
            query: prelude.value,
            body,
            diagnostics,
        });
    }
    if prelude.name == "font-face" {
        let _ = parse_declarations(input);
        return Ok(ParsedRule::FontFaceBlock { location });
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

/// One item of a block's contents. CSS Syntax §5.4.5 has a style rule's block
/// parse as a list of declarations; CSS Nesting §3.1 adds nested rules to the
/// same list, so a nested style rule and its parent come out of one walker.
#[derive(Clone, Debug)]
enum RuleBodyItem {
    Declaration(Declaration),
    Rule(ParsedRule),
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
        _declaration_start: &ParserState,
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
            let trailing_block = matches!(input.next(), Ok(&Token::CurlyBracketBlock));
            if trailing_block || (blocks > 0 && components > blocks) {
                return Err(input.new_custom_error(RuleParseError::InvalidSelector(
                    "a declaration followed by a block is a nested style rule".to_owned(),
                )));
            }
        }

        let normalized_name = if is_custom_property {
            raw_name.to_owned()
        } else {
            raw_name.to_ascii_lowercase()
        };
        Ok(RuleBodyItem::Declaration(Declaration {
            name: normalized_name,
            value: input.slice(start..value_end).trim().to_owned(),
            important,
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
        let _ = prelude;
        // A nested at-rule with no block applies nothing to the enclosing style
        // rule's elements.
        let _ = start;
        Err(())
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

impl<'i> RuleBodyItemParser<'i, RuleBodyItem, RuleParseError> for PropertyParser<'_> {
    fn parse_declarations(&self) -> bool {
        true
    }

    /// CSS Syntax §5.5.5: a block's contents are declarations *and* rules, so
    /// both are attempted. cssparser reparses a failed declaration as a
    /// qualified rule, which is exactly the algorithm's "try a declaration,
    /// otherwise reparse as a rule".
    fn parse_qualified(&self) -> bool {
        self.allow_nesting
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
) -> (Vec<Declaration>, Vec<ParsedRule>, Vec<StyleSheetDiagnostic>) {
    let mut parser = PropertyParser {
        allow_nesting: true,
        parent: parent.cloned(),
        next_anonymous_layer,
    };
    let mut nested = Vec::new();
    let mut diagnostics = Vec::new();
    // The run of declarations before any nested rule belongs to this rule
    // itself; every run after one belongs to a nested declarations rule
    // (`CSS Nesting §5`), which §3.4 orders after the nested rules. A nested
    // group rule's declarations belong to the *enclosing* style rule (§3.3),
    // so they start out already interrupted.
    let mut own: Vec<Declaration> = Vec::new();
    let mut pending: Vec<Declaration> = Vec::new();
    let mut interrupted = !own_declarations;
    for item in RuleBodyParser::new(input, &mut parser) {
        match item {
            Ok(RuleBodyItem::Declaration(declaration))
                if !declaration.value.is_empty() || declaration.name.starts_with("--") =>
            {
                if interrupted {
                    pending.push(declaration);
                } else {
                    own.push(declaration);
                }
            }
            // An empty ordinary declaration, such as `display: ;`, is
            // invalid CSS and must not override a lower-priority value.
            // Pages commonly emit this form when an optional inline style is
            // generated by a template.
            Ok(RuleBodyItem::Declaration(_)) => {}
            Ok(RuleBodyItem::Rule(rule)) => {
                interrupted = true;
                flush_nested_declarations(&mut pending, &mut nested, parent);
                nested.push(rule);
            }
            Err((error, _source)) => diagnostics.push(diagnostic(&error)),
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
    declarations: &mut Vec<Declaration>,
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
    let run: Vec<Declaration> = run
        .into_iter()
        .filter(|declaration| !declaration.value.is_empty() || declaration.name.starts_with("--"))
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
/// attribute, and a descriptor list such as `@font-face`.
fn parse_declarations(input: &mut Parser<'_, '_>) -> (Vec<Declaration>, Vec<StyleSheetDiagnostic>) {
    let mut anonymous_layer = 0;
    let mut parser = PropertyParser::declarations_only(&mut anonymous_layer);
    let mut declarations = Vec::new();
    let mut diagnostics = Vec::new();
    for item in RuleBodyParser::new(input, &mut parser) {
        match item {
            Ok(RuleBodyItem::Declaration(declaration))
                if !declaration.value.is_empty() || declaration.name.starts_with("--") =>
            {
                declarations.push(declaration);
            }
            Ok(RuleBodyItem::Declaration(_)) => {}
            Ok(RuleBodyItem::Rule(_)) => {}
            Err((error, _source)) => diagnostics.push(diagnostic(&error)),
        }
    }
    (declarations, diagnostics)
}

fn diagnostic(error: &ParseError<'_, RuleParseError>) -> StyleSheetDiagnostic {
    StyleSheetDiagnostic {
        line: error.location.line.saturating_add(1),
        column: error.location.column,
        message: error.kind.to_string(),
    }
}

fn capability_diagnostic(location: SourceLocation, message: String) -> StyleSheetDiagnostic {
    StyleSheetDiagnostic {
        line: location.line.saturating_add(1),
        column: location.column,
        message,
    }
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
                sheet.rules.push(StyleRule {
                    selectors,
                    declarations,
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
                sheet.rules.push(StyleRule {
                    selectors,
                    declarations,
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
            } => {
                // Property support is intentionally permissive until the
                // computed-value registry grows a complete CSS.supports
                // implementation. Parsing the nested rules is still enough
                // to honor the common `@supports (display: grid)` blocks.
                let _ = query;
                sheet.diagnostics.extend(diagnostics);
                flatten_group_body(body, layer, media, sheet);
            }
            ParsedRule::KeyframesBlock { name, location } => {
                // The current paint pipeline has no animation clock, but the
                // at-rule is still valid and must not make a stylesheet fail.
                let _ = name;
                let _ = location;
            }
            ParsedRule::FontFaceBlock { location } => {
                let _ = location;
            }
            ParsedRule::IgnoredAtRule { name, location } => {
                sheet.diagnostics.push(capability_diagnostic(
                    location,
                    format!("@{name} is parsed but not evaluated yet"),
                ));
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
}
