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

use super::selector::{SelectorList, parse_selector_list};

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
        diagnostics: Vec<StyleSheetDiagnostic>,
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
        rules: Vec<Self>,
        diagnostics: Vec<StyleSheetDiagnostic>,
    },
    SupportsBlock {
        query: String,
        rules: Vec<Self>,
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

struct RuleParser<'a> {
    next_anonymous_layer: &'a mut u32,
}

impl<'i> QualifiedRuleParser<'i> for RuleParser<'_> {
    type Prelude = SelectorList;
    type QualifiedRule = ParsedRule;
    type Error = RuleParseError;

    fn parse_prelude<'t>(
        &mut self,
        input: &mut Parser<'i, 't>,
    ) -> Result<Self::Prelude, ParseError<'i, Self::Error>> {
        let selector_text = consume_raw(input);
        parse_selector_list(&selector_text).map_err(|error| {
            input.new_custom_error(RuleParseError::InvalidSelector(error.to_string()))
        })
    }

    fn parse_block<'t>(
        &mut self,
        selectors: Self::Prelude,
        _start: &ParserState,
        input: &mut Parser<'i, 't>,
    ) -> Result<Self::QualifiedRule, ParseError<'i, Self::Error>> {
        let (declarations, diagnostics) = parse_declarations(input);
        Ok(ParsedRule::Style {
            selectors,
            declarations,
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
        if prelude.name == "layer" {
            let layer = if prelude.value.trim().is_empty() {
                let id = *self.next_anonymous_layer;
                *self.next_anonymous_layer = id.saturating_add(1);
                LayerName::Anonymous(id)
            } else {
                let mut layers = parse_layer_names(&prelude.value)
                    .map_err(|()| input.new_custom_error(RuleParseError::InvalidLayerName))?;
                if layers.len() != 1 {
                    return Err(input.new_custom_error(RuleParseError::InvalidLayerName));
                }
                layers.remove(0)
            };
            let (rules, diagnostics) = parse_rule_list(input, self.next_anonymous_layer);
            return Ok(ParsedRule::LayerBlock {
                layer,
                rules,
                diagnostics,
                location: start.source_location(),
            });
        }

        if prelude.name == "media" {
            let (rules, diagnostics) = parse_rule_list(input, self.next_anonymous_layer);
            return Ok(ParsedRule::MediaBlock {
                query: prelude.value,
                rules,
                diagnostics,
            });
        }

        if prelude.name.ends_with("keyframes") {
            consume_raw(input);
            return Ok(ParsedRule::KeyframesBlock {
                name: prelude.value.trim().to_owned(),
                location: start.source_location(),
            });
        }

        if prelude.name == "supports" {
            let (rules, diagnostics) = parse_rule_list(input, self.next_anonymous_layer);
            return Ok(ParsedRule::SupportsBlock {
                query: prelude.value,
                rules,
                diagnostics,
            });
        }
        if prelude.name == "font-face" {
            let _ = parse_declarations(input);
            return Ok(ParsedRule::FontFaceBlock {
                location: start.source_location(),
            });
        }
        consume_raw(input);
        Ok(ParsedRule::IgnoredAtRule {
            name: prelude.name,
            location: start.source_location(),
        })
    }
}

struct PropertyParser;

impl<'i> DeclarationParser<'i> for PropertyParser {
    type Declaration = Declaration;
    type Error = RuleParseError;

    fn parse_value<'t>(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i, 't>,
        _declaration_start: &ParserState,
    ) -> Result<Self::Declaration, ParseError<'i, Self::Error>> {
        let start = input.position();
        let mut value_end = start;
        let mut important = false;

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
                    Token::Function(_)
                    | Token::ParenthesisBlock
                    | Token::SquareBracketBlock
                    | Token::CurlyBracketBlock,
                ) => {
                    input.parse_nested_block(|nested| {
                        while nested.next_including_whitespace_and_comments().is_ok() {}
                        Ok(())
                    })?;
                    value_end = input.position();
                }
                Ok(_) => value_end = input.position(),
                Err(_) => break,
            }
        }

        let raw_name = name.as_ref();
        let normalized_name = if raw_name.starts_with("--") {
            raw_name.to_owned()
        } else {
            raw_name.to_ascii_lowercase()
        };
        Ok(Declaration {
            name: normalized_name,
            value: input.slice(start..value_end).trim().to_owned(),
            important,
        })
    }
}

impl AtRuleParser<'_> for PropertyParser {
    type Prelude = ();
    type AtRule = Declaration;
    type Error = RuleParseError;
}

impl QualifiedRuleParser<'_> for PropertyParser {
    type Prelude = ();
    type QualifiedRule = Declaration;
    type Error = RuleParseError;
}

impl RuleBodyItemParser<'_, Declaration, RuleParseError> for PropertyParser {
    fn parse_declarations(&self) -> bool {
        true
    }

    fn parse_qualified(&self) -> bool {
        false
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

fn parse_declarations(input: &mut Parser<'_, '_>) -> (Vec<Declaration>, Vec<StyleSheetDiagnostic>) {
    let mut parser = PropertyParser;
    let mut declarations = Vec::new();
    let mut diagnostics = Vec::new();
    for item in RuleBodyParser::new(input, &mut parser) {
        match item {
            Ok(declaration) => declarations.push(declaration),
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
                diagnostics,
            } => {
                sheet.diagnostics.extend(diagnostics);
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
                rules,
                diagnostics,
            } => {
                sheet.diagnostics.extend(diagnostics);
                let mut nested_media = media.to_vec();
                nested_media.push(query);
                flatten_rules(rules, layer, &nested_media, sheet);
            }
            ParsedRule::SupportsBlock {
                query,
                rules,
                diagnostics,
            } => {
                // Property support is intentionally permissive until the
                // computed-value registry grows a complete CSS.supports
                // implementation. Parsing the nested rules is still enough
                // to honor the common `@supports (display: grid)` blocks.
                let _ = query;
                sheet.diagnostics.extend(diagnostics);
                flatten_rules(rules, layer, media, sheet);
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

#[cfg(test)]
mod tests {
    use super::{CssWideKeyword, LayerName, css_wide_keyword, parse_stylesheet};

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

        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(sheet.rules[0].declarations[0].value, "blue");
        assert_eq!(sheet.diagnostics.len(), 1);
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
}
