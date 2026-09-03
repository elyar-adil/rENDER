//! Token-level computed-value resolution.
//!
//! Custom properties are inherited and resolved before ordinary properties.
//! Property-specific grammars and canonical computed-value conversions remain
//! separate; this layer preserves token boundaries so later parsers cannot
//! accidentally turn `var(--n)px` into a dimension token.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;

use cssparser::{ParseError, Parser, ParserInput, Token};

use crate::dom::{Dom, Node, NodeId, NodeKind};

use super::cascade::{CascadeInput, CascadedStyle, cascade_element_with_inline};
use super::properties::{TypedPropertyValue, parse_typed_property};
use super::selector::MatchContext;
use super::stylesheet::{CssWideKeyword, css_wide_keyword, parse_declaration_list};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PropertyDefinition {
    pub inherited: bool,
    pub initial_value: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PropertyRegistry {
    definitions: BTreeMap<String, PropertyDefinition>,
}

impl PropertyRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Define ordinary-property inheritance metadata and its initial value.
    ///
    /// # Panics
    ///
    /// Panics when `name` is a custom property. Unregistered custom properties
    /// always use the inheritance rules from CSS Variables instead.
    pub fn define(
        &mut self,
        name: impl Into<String>,
        inherited: bool,
        initial_value: impl Into<String>,
    ) {
        let name = name.into().to_ascii_lowercase();
        assert!(
            !name.starts_with("--"),
            "custom properties are not ordinary property definitions"
        );
        self.definitions.insert(
            name,
            PropertyDefinition {
                inherited,
                initial_value: initial_value.into(),
            },
        );
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&PropertyDefinition> {
        self.definitions.get(name)
    }

    #[must_use]
    pub fn standard_baseline() -> Self {
        let mut registry = Self::new();
        for (name, initial) in [
            ("color", "canvastext"),
            ("direction", "ltr"),
            ("font-size", "medium"),
            ("font-style", "normal"),
            ("font-weight", "normal"),
            ("letter-spacing", "normal"),
            ("line-height", "normal"),
            ("text-align", "start"),
            ("text-indent", "0px"),
            ("text-transform", "none"),
            ("visibility", "visible"),
            ("white-space", "normal"),
            ("word-spacing", "normal"),
        ] {
            registry.define(name, true, initial);
        }
        for (name, initial) in [
            ("background-color", "transparent"),
            ("background-image", "none"),
            ("background-repeat", "repeat"),
            ("background-position", "0% 0%"),
            ("background-size", "auto"),
            ("object-fit", "fill"),
            ("display", "inline"),
            ("position", "static"),
            ("float", "none"),
            ("clear", "none"),
            ("width", "auto"),
            ("height", "auto"),
            ("min-width", "auto"),
            ("min-height", "auto"),
            ("max-width", "none"),
            ("max-height", "none"),
            ("margin-top", "0px"),
            ("margin-right", "0px"),
            ("margin-bottom", "0px"),
            ("margin-left", "0px"),
            ("padding-top", "0px"),
            ("padding-right", "0px"),
            ("padding-bottom", "0px"),
            ("padding-left", "0px"),
            ("border-top-width", "medium"),
            ("border-right-width", "medium"),
            ("border-bottom-width", "medium"),
            ("border-left-width", "medium"),
            ("border-top-style", "none"),
            ("border-right-style", "none"),
            ("border-bottom-style", "none"),
            ("border-left-style", "none"),
            ("border-top-color", "currentcolor"),
            ("border-right-color", "currentcolor"),
            ("border-bottom-color", "currentcolor"),
            ("border-left-color", "currentcolor"),
            ("top", "auto"),
            ("right", "auto"),
            ("bottom", "auto"),
            ("left", "auto"),
            ("opacity", "1"),
            ("overflow-x", "visible"),
            ("overflow-y", "visible"),
            ("box-sizing", "content-box"),
            ("z-index", "auto"),
            ("flex-direction", "row"),
            ("flex-basis", "auto"),
            ("flex-grow", "0"),
            ("flex-shrink", "1"),
            ("justify-content", "normal"),
            ("align-items", "stretch"),
            ("order", "0"),
            ("row-gap", "normal"),
            ("column-gap", "normal"),
            ("grid-template-columns", "none"),
            ("grid-template-rows", "none"),
        ] {
            registry.define(name, false, initial);
        }
        registry
    }

    fn iter(&self) -> impl Iterator<Item = (&String, &PropertyDefinition)> {
        self.definitions.iter()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComputationLimits {
    pub max_custom_properties: usize,
    pub max_dependency_depth: usize,
    pub max_component_depth: usize,
    pub max_component_values: usize,
    pub max_value_bytes: usize,
}

impl Default for ComputationLimits {
    fn default() -> Self {
        Self {
            max_custom_properties: 4_096,
            max_dependency_depth: 128,
            max_component_depth: 128,
            max_component_values: 65_536,
            max_value_bytes: 1_048_576,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComputationDiagnostic {
    pub property: Option<String>,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComputedValue {
    css_text: String,
    tokens: TokenStream,
}

impl ComputedValue {
    fn new(tokens: TokenStream) -> Self {
        Self {
            css_text: tokens.to_css().trim().to_owned(),
            tokens,
        }
    }

    fn within_limits(tokens: TokenStream, limits: &ComputationLimits) -> Option<Self> {
        if tokens.serialized_len_exceeds(limits.max_value_bytes)
            || tokens.component_count_exceeds(limits.max_component_values)
        {
            None
        } else {
            Some(Self::new(tokens))
        }
    }

    #[must_use]
    pub fn css_text(&self) -> &str {
        &self.css_text
    }

    /// A serialization safe to feed back into a tokenizer. Synthetic comments
    /// preserve component boundaries introduced by `var()` substitution.
    #[must_use]
    pub fn parseable_css(&self) -> String {
        self.tokens.to_parseable_css().trim().to_owned()
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ComputedStyle {
    properties: BTreeMap<String, ComputedValue>,
    typed_properties: BTreeMap<String, TypedPropertyValue>,
    custom_properties: BTreeMap<String, ComputedValue>,
    invalid_custom_properties: BTreeSet<String>,
    diagnostics: Vec<ComputationDiagnostic>,
}

impl ComputedStyle {
    #[must_use]
    pub fn get(&self, property: &str) -> Option<&ComputedValue> {
        if property.starts_with("--") {
            self.custom_properties.get(property)
        } else {
            self.properties.get(property)
        }
    }

    #[must_use]
    pub const fn properties(&self) -> &BTreeMap<String, ComputedValue> {
        &self.properties
    }

    /// Layout-facing values whose property grammars are implemented by this
    /// migration slice. Unsupported properties remain available only through
    /// the token-level map and are never mistaken for typed values.
    #[must_use]
    pub const fn typed_properties(&self) -> &BTreeMap<String, TypedPropertyValue> {
        &self.typed_properties
    }

    #[must_use]
    pub fn typed(&self, property: &str) -> Option<&TypedPropertyValue> {
        self.typed_properties.get(property)
    }

    #[must_use]
    pub const fn custom_properties(&self) -> &BTreeMap<String, ComputedValue> {
        &self.custom_properties
    }

    #[must_use]
    pub const fn invalid_custom_properties(&self) -> &BTreeSet<String> {
        &self.invalid_custom_properties
    }

    #[must_use]
    pub fn diagnostics(&self) -> &[ComputationDiagnostic] {
        &self.diagnostics
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TokenClass {
    Whitespace,
    Comment,
    Comma,
    Ident(String),
    Other,
}

impl TokenClass {
    const fn insignificant(&self) -> bool {
        matches!(self, Self::Whitespace | Self::Comment)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ComponentValue {
    Token {
        class: TokenClass,
        source: String,
    },
    Function {
        name: String,
        prefix: String,
        values: TokenStream,
        suffix: String,
    },
    Block {
        prefix: String,
        values: TokenStream,
        suffix: String,
    },
    Substitution(Box<TokenStream>),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct TokenStream(Vec<ComponentValue>);

impl TokenStream {
    fn to_css(&self) -> String {
        let mut output = String::new();
        self.write_css(&mut output, false);
        output
    }

    fn to_parseable_css(&self) -> String {
        let mut output = String::new();
        self.write_css(&mut output, true);
        output
    }

    fn write_css(&self, output: &mut String, preserve_substitution_boundaries: bool) {
        for value in &self.0 {
            match value {
                ComponentValue::Token { source, .. } => output.push_str(source),
                ComponentValue::Function {
                    prefix,
                    values,
                    suffix,
                    ..
                }
                | ComponentValue::Block {
                    prefix,
                    values,
                    suffix,
                } => {
                    output.push_str(prefix);
                    values.write_css(output, preserve_substitution_boundaries);
                    output.push_str(suffix);
                }
                ComponentValue::Substitution(values) => {
                    if preserve_substitution_boundaries {
                        output.push_str("/**/");
                    }
                    values.write_css(output, preserve_substitution_boundaries);
                    if preserve_substitution_boundaries {
                        output.push_str("/**/");
                    }
                }
            }
        }
    }

    fn serialized_len_exceeds(&self, limit: usize) -> bool {
        fn add_stream(stream: &TokenStream, total: &mut usize, limit: usize) -> bool {
            for value in &stream.0 {
                let exceeded = match value {
                    ComponentValue::Token { source, .. } => add(total, source.len(), limit),
                    ComponentValue::Function {
                        prefix,
                        values,
                        suffix,
                        ..
                    }
                    | ComponentValue::Block {
                        prefix,
                        values,
                        suffix,
                    } => {
                        add(total, prefix.len(), limit)
                            || add_stream(values, total, limit)
                            || add(total, suffix.len(), limit)
                    }
                    ComponentValue::Substitution(values) => add_stream(values, total, limit),
                };
                if exceeded {
                    return true;
                }
            }
            false
        }

        fn add(total: &mut usize, amount: usize, limit: usize) -> bool {
            *total = total.saturating_add(amount);
            *total > limit
        }

        add_stream(self, &mut 0, limit)
    }

    fn component_count_exceeds(&self, limit: usize) -> bool {
        self.component_count_up_to(limit).is_none()
    }

    fn component_count_up_to(&self, limit: usize) -> Option<usize> {
        fn count_stream(stream: &TokenStream, total: &mut usize, limit: usize) -> bool {
            for value in &stream.0 {
                *total = total.saturating_add(1);
                if *total > limit {
                    return true;
                }
                let nested = match value {
                    ComponentValue::Function { values, .. }
                    | ComponentValue::Block { values, .. } => Some(values),
                    ComponentValue::Substitution(values) => Some(values.as_ref()),
                    ComponentValue::Token { .. } => None,
                };
                if nested.is_some_and(|values| count_stream(values, total, limit)) {
                    return true;
                }
            }
            false
        }

        let mut total = 0;
        if count_stream(self, &mut total, limit) {
            None
        } else {
            Some(total)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TokenStreamError {
    InvalidToken,
    ComponentLimit,
    NestingLimit,
}

impl fmt::Display for TokenStreamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidToken => formatter.write_str("invalid CSS component value"),
            Self::ComponentLimit => formatter.write_str("CSS component-value limit exceeded"),
            Self::NestingLimit => formatter.write_str("CSS component nesting limit exceeded"),
        }
    }
}

fn tokenize(source: &str, limits: &ComputationLimits) -> Result<TokenStream, String> {
    let mut input = ParserInput::new(source);
    let mut parser = Parser::new(&mut input);
    let mut count = 0;
    parse_component_values(&mut parser, source, &mut count, limits, 0)
        .map_err(|error| error.kind.to_string())
}

fn parse_component_values<'i>(
    input: &mut Parser<'i, '_>,
    source: &'i str,
    count: &mut usize,
    limits: &ComputationLimits,
    depth: usize,
) -> Result<TokenStream, ParseError<'i, TokenStreamError>> {
    let mut values = Vec::new();
    loop {
        let start = input.position().byte_index();
        let Ok(token) = input.next_including_whitespace_and_comments().cloned() else {
            break;
        };
        *count = count.saturating_add(1);
        if *count > limits.max_component_values {
            return Err(input.new_custom_error(TokenStreamError::ComponentLimit));
        }
        if token.is_parse_error() {
            return Err(input.new_custom_error(TokenStreamError::InvalidToken));
        }

        match token {
            Token::Function(name) => {
                if depth >= limits.max_component_depth {
                    return Err(input.new_custom_error(TokenStreamError::NestingLimit));
                }
                let prefix_end = input.position().byte_index();
                let (nested, nested_end) = input.parse_nested_block(|nested| {
                    let values = parse_component_values(
                        nested,
                        source,
                        count,
                        limits,
                        depth.saturating_add(1),
                    )?;
                    Ok((values, nested.position().byte_index()))
                })?;
                let end = input.position().byte_index();
                values.push(ComponentValue::Function {
                    name: name.to_string(),
                    prefix: source[start..prefix_end].to_owned(),
                    values: nested,
                    suffix: source[nested_end..end].to_owned(),
                });
            }
            Token::ParenthesisBlock | Token::SquareBracketBlock | Token::CurlyBracketBlock => {
                if depth >= limits.max_component_depth {
                    return Err(input.new_custom_error(TokenStreamError::NestingLimit));
                }
                let prefix_end = input.position().byte_index();
                let (nested, nested_end) = input.parse_nested_block(|nested| {
                    let values = parse_component_values(
                        nested,
                        source,
                        count,
                        limits,
                        depth.saturating_add(1),
                    )?;
                    Ok((values, nested.position().byte_index()))
                })?;
                let end = input.position().byte_index();
                values.push(ComponentValue::Block {
                    prefix: source[start..prefix_end].to_owned(),
                    values: nested,
                    suffix: source[nested_end..end].to_owned(),
                });
            }
            other => {
                let end = input.position().byte_index();
                let class = match other {
                    Token::WhiteSpace(_) => TokenClass::Whitespace,
                    Token::Comment(_) => TokenClass::Comment,
                    Token::Comma => TokenClass::Comma,
                    Token::Ident(name) => TokenClass::Ident(name.to_string()),
                    _ => TokenClass::Other,
                };
                values.push(ComponentValue::Token {
                    class,
                    source: source[start..end].to_owned(),
                });
            }
        }
    }
    Ok(TokenStream(values))
}

#[derive(Clone, Debug)]
enum CustomSource {
    Resolved(TokenStream),
    Specified(TokenStream),
}

struct VarArguments {
    name: String,
    fallback: Option<TokenStream>,
}

fn var_arguments(values: &TokenStream) -> Option<VarArguments> {
    let first = values.0.iter().position(|value| !is_insignificant(value))?;
    let name = match &values.0[first] {
        ComponentValue::Token {
            class: TokenClass::Ident(name),
            ..
        } if name.starts_with("--") => name.clone(),
        _ => return None,
    };
    let next = values.0[first + 1..]
        .iter()
        .position(|value| !is_insignificant(value))
        .map(|offset| first + 1 + offset);
    match next {
        None => Some(VarArguments {
            name,
            fallback: None,
        }),
        Some(index)
            if matches!(
                values.0[index],
                ComponentValue::Token {
                    class: TokenClass::Comma,
                    ..
                }
            ) =>
        {
            Some(VarArguments {
                name,
                fallback: Some(TokenStream(values.0[index + 1..].to_vec())),
            })
        }
        Some(_) => None,
    }
}

fn is_insignificant(value: &ComponentValue) -> bool {
    matches!(
        value,
        ComponentValue::Token { class, .. } if class.insignificant()
    )
}

fn collect_dependencies(stream: &TokenStream, output: &mut BTreeSet<String>) -> bool {
    for value in &stream.0 {
        match value {
            ComponentValue::Function { name, values, .. } if name.eq_ignore_ascii_case("var") => {
                let Some(arguments) = var_arguments(values) else {
                    return false;
                };
                output.insert(arguments.name);
                if let Some(fallback) = arguments.fallback
                    && !collect_dependencies(&fallback, output)
                {
                    return false;
                }
            }
            ComponentValue::Function { values, .. } | ComponentValue::Block { values, .. } => {
                if !collect_dependencies(values, output) {
                    return false;
                }
            }
            ComponentValue::Substitution(values) => {
                if !collect_dependencies(values, output) {
                    return false;
                }
            }
            ComponentValue::Token { .. } => {}
        }
    }
    true
}

fn find_cycles(
    graph: &BTreeMap<String, Vec<String>>,
    max_depth: usize,
) -> (BTreeSet<String>, BTreeSet<String>) {
    fn visit(
        name: &str,
        graph: &BTreeMap<String, Vec<String>>,
        states: &mut HashMap<String, u8>,
        path: &mut Vec<String>,
        cycles: &mut BTreeSet<String>,
        limited: &mut BTreeSet<String>,
        max_depth: usize,
    ) {
        if path.len() >= max_depth {
            limited.extend(path.iter().cloned());
            limited.insert(name.to_owned());
            return;
        }
        match states.get(name).copied().unwrap_or_default() {
            2 => return,
            1 => {
                if let Some(start) = path.iter().position(|candidate| candidate == name) {
                    cycles.extend(path[start..].iter().cloned());
                }
                return;
            }
            _ => {}
        }
        states.insert(name.to_owned(), 1);
        path.push(name.to_owned());
        if let Some(dependencies) = graph.get(name) {
            for dependency in dependencies {
                visit(dependency, graph, states, path, cycles, limited, max_depth);
            }
        }
        path.pop();
        states.insert(name.to_owned(), 2);
    }

    let mut states = HashMap::new();
    let mut path = Vec::new();
    let mut cycles = BTreeSet::new();
    let mut limited = BTreeSet::new();
    for name in graph.keys() {
        visit(
            name,
            graph,
            &mut states,
            &mut path,
            &mut cycles,
            &mut limited,
            max_depth,
        );
    }
    (cycles, limited)
}

struct CustomResolver<'a> {
    sources: &'a BTreeMap<String, CustomSource>,
    invalid: &'a BTreeSet<String>,
    memo: BTreeMap<String, Option<TokenStream>>,
    limits: &'a ComputationLimits,
    diagnostics: &'a mut Vec<ComputationDiagnostic>,
    diagnosed: BTreeSet<String>,
}

impl CustomResolver<'_> {
    fn resolve(&mut self, name: &str, depth: usize) -> Option<TokenStream> {
        if let Some(value) = self.memo.get(name) {
            return value.clone();
        }
        if self.invalid.contains(name) || depth >= self.limits.max_dependency_depth {
            if depth >= self.limits.max_dependency_depth && self.diagnosed.insert(name.to_owned()) {
                self.diagnostics.push(ComputationDiagnostic {
                    property: Some(name.to_owned()),
                    message: "custom-property dependency depth exceeded".to_owned(),
                });
            }
            self.memo.insert(name.to_owned(), None);
            return None;
        }
        let source = self.sources.get(name)?.clone();
        let resolved = match source {
            CustomSource::Resolved(value) => Some(value),
            CustomSource::Specified(value) => self.substitute(&value, depth.saturating_add(1)),
        };
        let resolved = resolved.filter(|value| {
            if !value.serialized_len_exceeds(self.limits.max_value_bytes)
                && !value.component_count_exceeds(self.limits.max_component_values)
            {
                true
            } else {
                if self.diagnosed.insert(name.to_owned()) {
                    self.diagnostics.push(ComputationDiagnostic {
                        property: Some(name.to_owned()),
                        message: "computed custom-property value exceeds byte limit".to_owned(),
                    });
                }
                false
            }
        });
        self.memo.insert(name.to_owned(), resolved.clone());
        resolved
    }

    fn substitute(&mut self, stream: &TokenStream, depth: usize) -> Option<TokenStream> {
        if depth >= self.limits.max_dependency_depth {
            return None;
        }
        let mut output = Vec::new();
        let mut output_count = 0_usize;
        for value in &stream.0 {
            let substituted = match value {
                ComponentValue::Function { name, values, .. }
                    if name.eq_ignore_ascii_case("var") =>
                {
                    let arguments = var_arguments(values)?;
                    let replacement = self
                        .resolve(&arguments.name, depth.saturating_add(1))
                        .or_else(|| {
                            arguments.fallback.as_ref().and_then(|fallback| {
                                self.substitute(fallback, depth.saturating_add(1))
                            })
                        })?;
                    ComponentValue::Substitution(Box::new(replacement))
                }
                ComponentValue::Function {
                    name,
                    prefix,
                    values,
                    suffix,
                } => ComponentValue::Function {
                    name: name.clone(),
                    prefix: prefix.clone(),
                    values: self.substitute(values, depth.saturating_add(1))?,
                    suffix: suffix.clone(),
                },
                ComponentValue::Block {
                    prefix,
                    values,
                    suffix,
                } => ComponentValue::Block {
                    prefix: prefix.clone(),
                    values: self.substitute(values, depth.saturating_add(1))?,
                    suffix: suffix.clone(),
                },
                ComponentValue::Substitution(values) => {
                    ComponentValue::Substitution(values.clone())
                }
                ComponentValue::Token { .. } => value.clone(),
            };
            let nested_count = match &substituted {
                ComponentValue::Function { values, .. } | ComponentValue::Block { values, .. } => {
                    values.component_count_up_to(
                        self.limits
                            .max_component_values
                            .saturating_sub(output_count.saturating_add(1)),
                    )?
                }
                ComponentValue::Substitution(values) => values.component_count_up_to(
                    self.limits
                        .max_component_values
                        .saturating_sub(output_count.saturating_add(1)),
                )?,
                ComponentValue::Token { .. } => 0,
            };
            output_count = output_count.saturating_add(1).saturating_add(nested_count);
            if output_count > self.limits.max_component_values {
                return None;
            }
            output.push(substituted);
        }
        let output = TokenStream(output);
        if output.component_count_exceeds(self.limits.max_component_values)
            || output.serialized_len_exceeds(self.limits.max_value_bytes)
        {
            None
        } else {
            Some(output)
        }
    }
}

/// Resolve token-level computed values for one element.
#[must_use]
#[allow(clippy::too_many_lines)] // Kept linear to mirror the ordered CSS computed-value stages.
pub fn compute_style(
    cascaded: &CascadedStyle,
    parent: Option<&ComputedStyle>,
    registry: &PropertyRegistry,
    limits: &ComputationLimits,
) -> ComputedStyle {
    let mut diagnostics = Vec::new();
    let mut invalid = parent
        .map(|style| style.invalid_custom_properties.clone())
        .unwrap_or_default();
    let mut sources: BTreeMap<String, CustomSource> = parent
        .map(|style| {
            style
                .custom_properties
                .iter()
                .map(|(name, value)| (name.clone(), CustomSource::Resolved(value.tokens.clone())))
                .collect()
        })
        .unwrap_or_default();

    let cascaded_custom = cascaded
        .properties()
        .iter()
        .filter(|(name, _)| name.starts_with("--"))
        .collect::<Vec<_>>();
    if cascaded_custom.len() > limits.max_custom_properties {
        diagnostics.push(ComputationDiagnostic {
            property: None,
            message: "custom-property count limit exceeded".to_owned(),
        });
    }
    for (name, value) in cascaded_custom
        .into_iter()
        .take(limits.max_custom_properties)
    {
        match css_wide_keyword(&value.value) {
            Some(CssWideKeyword::Inherit | CssWideKeyword::Unset) => {
                if let Some(parent_value) =
                    parent.and_then(|style| style.custom_properties.get(name))
                {
                    sources.insert(
                        name.clone(),
                        CustomSource::Resolved(parent_value.tokens.clone()),
                    );
                    invalid.remove(name);
                } else {
                    sources.remove(name);
                    invalid.insert(name.clone());
                }
            }
            Some(CssWideKeyword::Initial) => {
                sources.remove(name);
                invalid.insert(name.clone());
            }
            Some(CssWideKeyword::Revert | CssWideKeyword::RevertLayer) => {
                sources.remove(name);
                invalid.insert(name.clone());
                diagnostics.push(ComputationDiagnostic {
                    property: Some(name.clone()),
                    message: "unresolved cascade rollback keyword reached computed-value stage"
                        .to_owned(),
                });
            }
            None => match tokenize(&value.value, limits) {
                Ok(tokens) => {
                    sources.insert(name.clone(), CustomSource::Specified(tokens));
                    invalid.remove(name);
                }
                Err(message) => {
                    sources.remove(name);
                    invalid.insert(name.clone());
                    diagnostics.push(ComputationDiagnostic {
                        property: Some(name.clone()),
                        message,
                    });
                }
            },
        }
    }

    let mut graph = BTreeMap::new();
    for (name, source) in &sources {
        if let CustomSource::Specified(tokens) = source {
            let mut dependencies = BTreeSet::new();
            if collect_dependencies(tokens, &mut dependencies) {
                graph.insert(
                    name.clone(),
                    dependencies
                        .into_iter()
                        .filter(|dependency| {
                            matches!(sources.get(dependency), Some(CustomSource::Specified(_)))
                        })
                        .collect(),
                );
            } else {
                invalid.insert(name.clone());
                diagnostics.push(ComputationDiagnostic {
                    property: Some(name.clone()),
                    message: "invalid var() syntax".to_owned(),
                });
            }
        }
    }
    let (cycles, depth_limited) = find_cycles(&graph, limits.max_dependency_depth);
    for name in &cycles {
        diagnostics.push(ComputationDiagnostic {
            property: Some(name.clone()),
            message: "custom-property dependency cycle".to_owned(),
        });
    }
    for name in &depth_limited {
        diagnostics.push(ComputationDiagnostic {
            property: Some(name.clone()),
            message: "custom-property dependency depth exceeded".to_owned(),
        });
    }
    invalid.extend(cycles);
    invalid.extend(depth_limited);

    let source_names = sources.keys().cloned().collect::<Vec<_>>();
    let mut resolver = CustomResolver {
        sources: &sources,
        invalid: &invalid,
        memo: BTreeMap::new(),
        limits,
        diagnostics: &mut diagnostics,
        diagnosed: BTreeSet::new(),
    };
    let mut custom_properties = BTreeMap::new();
    let mut resolution_invalid = BTreeSet::new();
    for name in source_names {
        if let Some(value) = resolver
            .resolve(&name, 0)
            .and_then(|tokens| ComputedValue::within_limits(tokens, limits))
        {
            custom_properties.insert(name, value);
        } else {
            resolution_invalid.insert(name);
        }
    }

    let mut properties = BTreeMap::new();
    let mut typed_properties = BTreeMap::new();
    for (name, definition) in registry.iter() {
        let mut computed =
            compute_registered_property(name, definition, cascaded, parent, &mut resolver, limits);
        if let Some(value) = computed.as_ref()
            && let Some(typed) = parse_typed_property(name, &value.parseable_css())
        {
            match typed {
                Ok(value) => {
                    typed_properties.insert(name.clone(), value);
                }
                Err(error) => {
                    resolver.diagnostics.push(ComputationDiagnostic {
                        property: Some(name.clone()),
                        message: format!("{error}; using the unset fallback"),
                    });
                    computed = registered_unset_value(name, definition, parent, limits);
                    if let Some(fallback) = computed.as_ref()
                        && let Some(Ok(value)) =
                            parse_typed_property(name, &fallback.parseable_css())
                    {
                        typed_properties.insert(name.clone(), value);
                    }
                }
            }
        }
        if let Some(value) = computed {
            properties.insert(name.clone(), value);
        }
    }
    for (name, value) in cascaded.properties() {
        if name.starts_with("--") || registry.get(name).is_some() {
            continue;
        }
        match css_wide_keyword(&value.value) {
            Some(CssWideKeyword::Inherit) => {
                if let Some(parent_value) = parent.and_then(|style| style.properties.get(name)) {
                    properties.insert(name.clone(), parent_value.clone());
                }
            }
            Some(_) => resolver.diagnostics.push(ComputationDiagnostic {
                property: Some(name.clone()),
                message: "property metadata is required to resolve this CSS-wide keyword"
                    .to_owned(),
            }),
            None => {
                if let Ok(tokens) = tokenize(&value.value, limits)
                    && let Some(tokens) = resolver.substitute(&tokens, 0)
                {
                    if let Some(value) = ComputedValue::within_limits(tokens, limits) {
                        properties.insert(name.clone(), value);
                    }
                }
            }
        }
    }
    drop(resolver);
    invalid.extend(resolution_invalid);

    ComputedStyle {
        properties,
        typed_properties,
        custom_properties,
        invalid_custom_properties: invalid,
        diagnostics,
    }
}

fn registered_unset_value(
    name: &str,
    definition: &PropertyDefinition,
    parent: Option<&ComputedStyle>,
    limits: &ComputationLimits,
) -> Option<ComputedValue> {
    if definition.inherited
        && let Some(value) = parent.and_then(|style| style.properties.get(name))
    {
        return Some(value.clone());
    }
    tokenize(&definition.initial_value, limits)
        .ok()
        .and_then(|tokens| ComputedValue::within_limits(tokens, limits))
}

fn compute_registered_property(
    name: &str,
    definition: &PropertyDefinition,
    cascaded: &CascadedStyle,
    parent: Option<&ComputedStyle>,
    resolver: &mut CustomResolver<'_>,
    limits: &ComputationLimits,
) -> Option<ComputedValue> {
    let initial = || {
        tokenize(&definition.initial_value, limits)
            .ok()
            .and_then(|tokens| ComputedValue::within_limits(tokens, limits))
    };
    let inherited = || {
        parent
            .and_then(|style| style.properties.get(name))
            .cloned()
            .or_else(initial)
    };
    let unset = || {
        if definition.inherited {
            inherited()
        } else {
            initial()
        }
    };
    let Some(value) = cascaded.get(name) else {
        return unset();
    };
    match css_wide_keyword(&value.value) {
        Some(CssWideKeyword::Inherit) => inherited(),
        Some(CssWideKeyword::Initial) => initial(),
        Some(CssWideKeyword::Unset | CssWideKeyword::Revert | CssWideKeyword::RevertLayer) => {
            unset()
        }
        None => tokenize(&value.value, limits)
            .ok()
            .and_then(|tokens| resolver.substitute(&tokens, 0))
            .and_then(|tokens| ComputedValue::within_limits(tokens, limits))
            .or_else(unset),
    }
}

/// Cascade and compute every element in parent-before-child order.
#[must_use]
pub fn compute_document_styles(
    dom: &Dom,
    sources: &[CascadeInput<'_>],
    registry: &PropertyRegistry,
    limits: &ComputationLimits,
    context: &MatchContext,
) -> BTreeMap<NodeId, ComputedStyle> {
    let mut styles = BTreeMap::new();
    let mut stack = vec![(dom.document(), None)];
    while let Some((node, parent_element)) = stack.pop() {
        let current_parent = if matches!(dom.node(node).map(Node::kind), Some(NodeKind::Element(_)))
        {
            let inline = dom
                .attribute(node, "style")
                .ok()
                .flatten()
                .map_or_else(Vec::new, |source| parse_declaration_list(source).0);
            let cascaded = cascade_element_with_inline(dom, node, sources, context, &inline);
            let style = compute_style(
                &cascaded,
                parent_element.and_then(|parent| styles.get(&parent)),
                registry,
                limits,
            );
            styles.insert(node, style);
            Some(node)
        } else {
            parent_element
        };
        if let Some(children) = dom.children(node) {
            for child in children.iter().rev() {
                stack.push((*child, current_parent));
            }
        }
    }
    styles
}

#[cfg(test)]
mod tests {
    use super::{ComputationLimits, PropertyRegistry, compute_document_styles, compute_style};
    use crate::css::cascade::{CascadeInput, CascadeOrigin, cascade_element};
    use crate::css::properties::TypedPropertyValue;
    use crate::css::selector::{MatchContext, parse_selector_list, select_all};
    use crate::css::stylesheet::parse_stylesheet;
    use crate::html::parse_document;

    fn target_id(dom: &crate::dom::Dom, selector: &str) -> crate::dom::NodeId {
        let selector = parse_selector_list(selector).expect("valid test selector");
        select_all(dom, dom.document(), &selector, &MatchContext::default())[0]
    }

    #[test]
    fn inherited_custom_properties_are_already_computed() {
        let output =
            parse_document("<!doctype html><div id='parent'><span id='child'></span></div>");
        let sheet = parse_stylesheet(
            "#parent { --brand: red; --derived: calc(var(--brand) + 1px); color: var(--brand) } \
             #child { --brand: blue; border-color: var(--missing, var(--brand)) }",
        );
        let mut registry = PropertyRegistry::new();
        registry.define("color", true, "canvastext");
        registry.define("border-color", false, "currentcolor");
        let styles = compute_document_styles(
            &output.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &registry,
            &ComputationLimits::default(),
            &MatchContext::default(),
        );
        let child = &styles[&target_id(&output.dom, "#child")];

        assert_eq!(
            child.get("--brand").map(super::ComputedValue::css_text),
            Some("blue")
        );
        assert_eq!(
            child.get("--derived").map(super::ComputedValue::css_text),
            Some("calc(red + 1px)")
        );
        assert_eq!(
            child.get("color").map(super::ComputedValue::css_text),
            Some("red")
        );
        assert_eq!(
            child
                .get("border-color")
                .map(super::ComputedValue::css_text),
            Some("blue")
        );
    }

    #[test]
    fn cycles_are_invalid_even_when_the_cycle_edges_have_fallbacks() {
        let output = parse_document("<!doctype html><div id='target'></div>");
        let sheet = parse_stylesheet(
            "#target { \
                --a: var(--b, red); --b: var(--a, blue); \
                --safe: var(--a, green); color: var(--a, purple); \
             }",
        );
        let mut registry = PropertyRegistry::new();
        registry.define("color", true, "canvastext");
        let target = target_id(&output.dom, "#target");
        let cascaded = cascade_element(
            &output.dom,
            target,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &MatchContext::default(),
        );
        let style = compute_style(&cascaded, None, &registry, &ComputationLimits::default());

        assert!(style.get("--a").is_none());
        assert!(style.get("--b").is_none());
        assert_eq!(
            style.get("--safe").map(super::ComputedValue::css_text),
            Some("green")
        );
        assert_eq!(
            style.get("color").map(super::ComputedValue::css_text),
            Some("purple")
        );
    }

    #[test]
    fn css_wide_keywords_use_property_inheritance_metadata() {
        let parent_cascade =
            parse_stylesheet("#x { color: red; margin-left: 12px; --token: blue }");
        let child_cascade = parse_stylesheet(
            "#x { color: unset; margin-left: unset; --token: inherit; --gone: initial }",
        );
        let output = parse_document("<!doctype html><div id='x'></div>");
        let target = target_id(&output.dom, "#x");
        let mut registry = PropertyRegistry::new();
        registry.define("color", true, "canvastext");
        registry.define("margin-left", false, "0px");
        let parent = compute_style(
            &cascade_element(
                &output.dom,
                target,
                &[CascadeInput {
                    sheet: &parent_cascade,
                    origin: CascadeOrigin::Author,
                }],
                &MatchContext::default(),
            ),
            None,
            &registry,
            &ComputationLimits::default(),
        );
        let child = compute_style(
            &cascade_element(
                &output.dom,
                target,
                &[CascadeInput {
                    sheet: &child_cascade,
                    origin: CascadeOrigin::Author,
                }],
                &MatchContext::default(),
            ),
            Some(&parent),
            &registry,
            &ComputationLimits::default(),
        );

        assert_eq!(
            child.get("color").map(super::ComputedValue::css_text),
            Some("red")
        );
        assert_eq!(
            child.get("margin-left").map(super::ComputedValue::css_text),
            Some("0px")
        );
        assert_eq!(
            child.get("--token").map(super::ComputedValue::css_text),
            Some("blue")
        );
        assert!(child.get("--gone").is_none());
    }

    #[test]
    fn substitution_boundaries_prevent_invalid_dimension_fusion() {
        let output = parse_document("<!doctype html><div id='target'></div>");
        let sheet = parse_stylesheet("#target { --n: 1; width: var(--n)px }");
        let target = target_id(&output.dom, "#target");
        let mut registry = PropertyRegistry::new();
        registry.define("width", false, "auto");
        let style = compute_style(
            &cascade_element(
                &output.dom,
                target,
                &[CascadeInput {
                    sheet: &sheet,
                    origin: CascadeOrigin::Author,
                }],
                &MatchContext::default(),
            ),
            None,
            &registry,
            &ComputationLimits::default(),
        );

        assert_eq!(
            style.get("width").map(super::ComputedValue::css_text),
            Some("auto")
        );
        assert_eq!(
            style.typed("width").map(TypedPropertyValue::to_css),
            Some("auto".to_owned())
        );
        assert!(
            style
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.property.as_deref() == Some("width"))
        );
    }

    #[test]
    fn resource_limits_invalidate_values_and_allow_call_site_fallbacks() {
        let output = parse_document("<!doctype html><div id='target'></div>");
        let sheet = parse_stylesheet(
            "#target { --large: 12345678901234567890; --deep: f(f(f(value))); \
             color: var(--large, green); visibility: var(--deep, hidden) }",
        );
        let target = target_id(&output.dom, "#target");
        let mut registry = PropertyRegistry::new();
        registry.define("color", true, "canvastext");
        registry.define("visibility", true, "visible");
        let limits = ComputationLimits {
            max_value_bytes: 10,
            max_component_depth: 2,
            ..ComputationLimits::default()
        };
        let style = compute_style(
            &cascade_element(
                &output.dom,
                target,
                &[CascadeInput {
                    sheet: &sheet,
                    origin: CascadeOrigin::Author,
                }],
                &MatchContext::default(),
            ),
            None,
            &registry,
            &limits,
        );

        assert!(style.invalid_custom_properties().contains("--large"));
        assert!(style.invalid_custom_properties().contains("--deep"));
        assert_eq!(
            style.get("color").map(super::ComputedValue::css_text),
            Some("green")
        );
        assert_eq!(
            style.get("visibility").map(super::ComputedValue::css_text),
            Some("hidden")
        );
    }
}
