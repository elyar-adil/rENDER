//! CSS Properties and Values API Level 1: `@property` registration.
//!
//! A registration is document-wide, not a style rule: it gives a custom
//! property an inheritance flag, an initial value, and a grammar. The grammar
//! is what turns a token stream into a typed custom property, and the
//! inheritance flag is what makes the property stop inheriting even though an
//! unregistered custom property always does.
//!
//! The grammar is kept as data rather than as a callback so the same parsed
//! registration can answer both the parse-time question ("is `initial-value`
//! valid for this syntax?") and the per-element question ("does this cascaded
//! token stream match the syntax?"). CSS Properties and Values 1 §2.3 defines
//! the syntax descriptor grammar this module implements.

use cssparser::{ParseError, Parser, ParserInput, Token};

use super::stylesheet::Declaration;

/// One valid `@property` rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PropertyRegistration {
    /// The custom property name, including the leading `--`.
    pub name: String,
    pub syntax: RegisteredSyntax,
    pub inherits: bool,
    /// The `initial-value` descriptor as written. `None` is only legal for the
    /// universal `*` syntax.
    pub initial_value: Option<String>,
    /// Enclosing `@media` queries, so a registration inside a false query does
    /// not become visible to elements outside it.
    pub media: Vec<String>,
}

impl PropertyRegistration {
    /// The computed initial value for this registration, if it has one.
    ///
    /// A universal registration without `initial-value` has the guaranteed
    /// invalid initial value; every other registration has `Some`.
    #[must_use]
    pub fn effective_initial_value(&self) -> Option<&str> {
        match self.syntax {
            RegisteredSyntax::Universal => self.initial_value.as_deref(),
            RegisteredSyntax::Alternatives(_) => self.initial_value.as_deref(),
        }
    }
}

/// The value of a `syntax:` descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegisteredSyntax {
    /// `*`: any token stream, and `initial-value` is optional.
    Universal,
    /// One or more `|`-separated alternatives. Every alternative is a sequence
    /// of terms that must consume the whole value.
    Alternatives(Vec<SyntaxAlternative>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyntaxAlternative {
    pub terms: Vec<SyntaxTerm>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyntaxTerm {
    pub kind: SyntaxKind,
    pub multiplier: SyntaxMultiplier,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyntaxMultiplier {
    /// Exactly one value.
    Single,
    /// One or more values separated by whitespace (`<length>+`).
    SpaceSeparated,
    /// One or more values separated by commas (`<color>#`).
    CommaSeparated,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyntaxKind {
    Length,
    Number,
    Percentage,
    LengthPercentage,
    Color,
    Image,
    Url,
    Integer,
    Angle,
    Time,
    Resolution,
    TransformFunction,
    TransformList,
    CustomIdent,
    String,
    /// An identifier written literally in the syntax descriptor, such as
    /// `auto` in `<length> | auto`.
    Literal(String),
}

impl RegisteredSyntax {
    /// Whether `css` is a valid value for this syntax.
    #[must_use]
    pub fn matches(&self, css: &str) -> bool {
        match self {
            Self::Universal => true,
            Self::Alternatives(alternatives) => {
                let Some(values) = tokenize_values(css) else {
                    return false;
                };
                !values.is_empty()
                    && alternatives
                        .iter()
                        .any(|alternative| alternative.matches(&values))
            }
        }
    }
}

impl SyntaxAlternative {
    fn matches(&self, values: &[Value]) -> bool {
        let mut cursor = 0_usize;
        for term in &self.terms {
            if !term.matches(values, &mut cursor) {
                return false;
            }
        }
        cursor == values.len()
    }
}

impl SyntaxTerm {
    fn matches(&self, values: &[Value], cursor: &mut usize) -> bool {
        if self.kind == SyntaxKind::TransformList {
            // `<transform-list>` is the pre-multiplied `<transform-function>+`:
            // one or more functions, with no `none` and no extra multiplier.
            let mut count = 0_usize;
            while values
                .get(*cursor)
                .is_some_and(Value::is_transform_function)
            {
                *cursor += 1;
                count += 1;
            }
            return count > 0;
        }
        match self.multiplier {
            SyntaxMultiplier::Single => self.matches_one(values, cursor),
            SyntaxMultiplier::SpaceSeparated => {
                if !self.matches_one(values, cursor) {
                    return false;
                }
                while self.matches_one(values, cursor) {}
                true
            }
            SyntaxMultiplier::CommaSeparated => {
                if !self.matches_one(values, cursor) {
                    return false;
                }
                loop {
                    if values.get(*cursor) != Some(&Value::Comma) {
                        break;
                    }
                    *cursor += 1;
                    if !self.matches_one(values, cursor) {
                        return false;
                    }
                }
                true
            }
        }
    }

    fn matches_one(&self, values: &[Value], cursor: &mut usize) -> bool {
        let Some(value) = values.get(*cursor) else {
            return false;
        };
        if !self.kind.matches(value) {
            return false;
        }
        *cursor += 1;
        true
    }
}

impl SyntaxKind {
    fn matches(&self, value: &Value) -> bool {
        match self {
            Self::Length => value.is_length(),
            Self::Number => value.is_number(),
            Self::Percentage => value.is_percentage(),
            Self::LengthPercentage => value.is_length() || value.is_percentage(),
            Self::Color => value.is_color(),
            Self::Image => value.is_image(),
            Self::Url => value.is_url(),
            Self::Integer => value.is_integer(),
            Self::Angle => value.is_angle(),
            Self::Time => value.is_time(),
            Self::Resolution => value.is_resolution(),
            Self::TransformFunction => value.is_transform_function(),
            Self::TransformList => value.is_transform_function(),
            Self::CustomIdent => match value {
                Value::Ident(name) => {
                    !is_css_wide_keyword(name) && !name.eq_ignore_ascii_case("default")
                }
                _ => false,
            },
            Self::String => matches!(value, Value::String(_)),
            Self::Literal(expected) => match value {
                Value::Ident(name) => name.eq_ignore_ascii_case(expected),
                _ => false,
            },
        }
    }
}

fn is_css_wide_keyword(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "inherit" | "initial" | "unset" | "revert" | "revert-layer"
    )
}

/// Parse a `syntax:` descriptor.
///
/// # Errors
///
/// Returns a human-readable reason when the descriptor is not one this module
/// can implement. An unimplemented type is rejected rather than treated as
/// universal, because silently widening the grammar would accept values the
/// author asked us to reject.
pub fn parse_syntax(source: &str) -> Result<RegisteredSyntax, String> {
    let source = source.trim();
    if source == "*" {
        return Ok(RegisteredSyntax::Universal);
    }
    if source.is_empty() {
        return Err("the syntax descriptor is empty".to_owned());
    }
    let mut alternatives = Vec::new();
    for part in source.split('|') {
        alternatives.push(parse_alternative(part.trim())?);
    }
    if alternatives.is_empty() {
        return Err("the syntax descriptor has no component".to_owned());
    }
    Ok(RegisteredSyntax::Alternatives(alternatives))
}

fn parse_alternative(source: &str) -> Result<SyntaxAlternative, String> {
    if source.is_empty() {
        return Err("the syntax descriptor contains an empty alternative".to_owned());
    }
    if source.contains('*') {
        return Err("`*` cannot be combined with other syntax components".to_owned());
    }
    let mut terms = Vec::new();
    let bytes = source.as_bytes();
    let mut index = 0_usize;
    while index < bytes.len() {
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= bytes.len() {
            break;
        }
        let (kind, next) = if bytes[index] == b'<' {
            let end = source[index + 1..]
                .find('>')
                .map(|offset| index + 1 + offset)
                .ok_or_else(|| format!("unterminated type in syntax `{source}`"))?;
            let name = source[index + 1..end].trim().to_ascii_lowercase();
            (parse_syntax_kind(&name)?, end + 1)
        } else {
            let start = index;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || matches!(bytes[index], b'-' | b'_'))
            {
                index += 1;
            }
            if start == index {
                return Err(format!("unexpected character in syntax `{source}`"));
            }
            (
                SyntaxKind::Literal(source[start..index].to_ascii_lowercase()),
                index,
            )
        };
        index = next;
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        let multiplier = match bytes.get(index) {
            Some(b'+') => {
                index += 1;
                SyntaxMultiplier::SpaceSeparated
            }
            Some(b'#') => {
                index += 1;
                SyntaxMultiplier::CommaSeparated
            }
            _ => SyntaxMultiplier::Single,
        };
        if kind == SyntaxKind::TransformList && multiplier != SyntaxMultiplier::Single {
            return Err("`<transform-list>` is already a list and cannot be multiplied".to_owned());
        }
        terms.push(SyntaxTerm { kind, multiplier });
    }
    if terms.is_empty() {
        return Err("the syntax descriptor has an empty component".to_owned());
    }
    Ok(SyntaxAlternative { terms })
}

fn parse_syntax_kind(name: &str) -> Result<SyntaxKind, String> {
    Ok(match name {
        "length" => SyntaxKind::Length,
        "number" => SyntaxKind::Number,
        "percentage" => SyntaxKind::Percentage,
        "length-percentage" => SyntaxKind::LengthPercentage,
        "color" => SyntaxKind::Color,
        "image" => SyntaxKind::Image,
        "url" => SyntaxKind::Url,
        "integer" => SyntaxKind::Integer,
        "angle" => SyntaxKind::Angle,
        "time" => SyntaxKind::Time,
        "resolution" => SyntaxKind::Resolution,
        "transform-function" => SyntaxKind::TransformFunction,
        "transform-list" => SyntaxKind::TransformList,
        "custom-ident" => SyntaxKind::CustomIdent,
        "string" => SyntaxKind::String,
        other => return Err(format!("`<{other}>` is not a supported syntax type")),
    })
}

/// Parse and validate one `@property` rule.
///
/// `prelude` is the at-rule prelude as written (the custom property name), and
/// `descriptors` is the block's declaration list.
///
/// # Errors
///
/// Returns a reason when a required descriptor is missing or malformed, when
/// `initial-value` is required but absent, or when the initial value does not
/// match the declared syntax. CSS Properties and Values 1 §2.2 makes such a
/// rule invalid.
pub fn parse_registration(
    prelude: &str,
    descriptors: &[Declaration],
) -> Result<PropertyRegistration, String> {
    let name = parse_custom_property_name(prelude)?;
    let mut syntax = None;
    let mut inherits = None;
    let mut initial_value = None;
    for descriptor in descriptors {
        let descriptor_name = descriptor.name.to_ascii_lowercase();
        match descriptor_name.as_str() {
            "syntax" => {
                let value = parse_string_descriptor(&descriptor.value)?;
                syntax = Some(parse_syntax(&value)?);
            }
            "inherits" => {
                let value = descriptor.value.trim().to_ascii_lowercase();
                inherits = Some(match value.as_str() {
                    "true" => true,
                    "false" => false,
                    _ => return Err("`inherits` must be `true` or `false`".to_owned()),
                });
            }
            "initial-value" => {
                let value = descriptor.value.trim();
                if value.is_empty() {
                    return Err("`initial-value` is empty".to_owned());
                }
                if mentions_var(value) {
                    return Err("`initial-value` must not contain var()".to_owned());
                }
                initial_value = Some(value.to_owned());
            }
            // CSS Properties and Values 1 §2.2: an unknown descriptor is
            // ignored, exactly as an unknown property declaration is.
            _ => {}
        }
    }
    let syntax = syntax.ok_or_else(|| "`syntax` is required".to_owned())?;
    let inherits = inherits.ok_or_else(|| "`inherits` is required".to_owned())?;
    if !matches!(syntax, RegisteredSyntax::Universal) && initial_value.is_none() {
        return Err("`initial-value` is required unless the syntax is `*`".to_owned());
    }
    if let Some(value) = &initial_value
        && !syntax.matches(value)
    {
        return Err(format!(
            "`initial-value` `{value}` does not match the declared syntax"
        ));
    }
    Ok(PropertyRegistration {
        name,
        syntax,
        inherits,
        initial_value,
        media: Vec::new(),
    })
}

fn parse_custom_property_name(prelude: &str) -> Result<String, String> {
    let mut input = ParserInput::new(prelude);
    let mut parser = Parser::new(&mut input);
    let name = parser
        .expect_ident_cloned()
        .map_err(|_| "the at-rule prelude is not a custom property name".to_owned())?
        .to_string();
    parser
        .expect_exhausted()
        .map_err(|_| "the at-rule prelude has extra tokens".to_owned())?;
    if !name.starts_with("--") || name.len() == 2 {
        return Err(format!("`{name}` is not a custom property name"));
    }
    Ok(name)
}

fn parse_string_descriptor(value: &str) -> Result<String, String> {
    let mut input = ParserInput::new(value);
    let mut parser = Parser::new(&mut input);
    let string = parser
        .expect_string_cloned()
        .map_err(|_| "`syntax` must be a single string".to_owned())?
        .to_string();
    parser
        .expect_exhausted()
        .map_err(|_| "`syntax` must be a single string".to_owned())?;
    Ok(string)
}

fn mentions_var(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("var(")
}

#[derive(Clone, Debug, PartialEq)]
enum Value {
    Ident(String),
    Number { value: f32, integer: bool },
    Percentage,
    Dimension { value: f32, unit: String },
    Hash(String),
    String(String),
    Url(String),
    Function { name: String, args: Vec<Value> },
    Block(Vec<Value>),
    Delim(char),
    Comma,
}

impl Value {
    fn unit_dimension(&self, units: &[&str]) -> bool {
        match self {
            Self::Dimension { unit, .. } => units.contains(&unit.as_str()),
            Self::Number { value, .. } => *value == 0.0,
            _ => false,
        }
    }

    fn is_length(&self) -> bool {
        match self {
            Self::Function { name, .. } => matches!(
                name.as_str(),
                "calc" | "min" | "max" | "clamp" | "fit-content"
            ),
            other => other.unit_dimension(&[
                "px", "cm", "mm", "q", "in", "pt", "pc", "em", "rem", "ex", "rex", "ch", "rch",
                "cap", "rcap", "ic", "ric", "lh", "rlh", "vw", "vh", "vi", "vb", "vmin", "vmax",
                "svw", "svh", "svi", "svb", "svmin", "svmax", "lvw", "lvh", "lvi", "lvb", "lvmin",
                "lvmax", "dvw", "dvh", "dvi", "dvb", "dvmin", "dvmax", "cqw", "cqh", "cqi", "cqb",
                "cqmin", "cqmax",
            ]),
        }
    }

    fn is_number(&self) -> bool {
        match self {
            Self::Number { .. } => true,
            Self::Function { name, .. } => {
                matches!(name.as_str(), "calc" | "min" | "max" | "clamp")
            }
            _ => false,
        }
    }

    fn is_percentage(&self) -> bool {
        match self {
            Self::Percentage => true,
            Self::Function { name, .. } => {
                matches!(name.as_str(), "calc" | "min" | "max" | "clamp")
            }
            _ => false,
        }
    }

    fn is_integer(&self) -> bool {
        match self {
            Self::Number { value, integer } => *integer && value.fract() == 0.0,
            Self::Function { name, .. } => {
                matches!(name.as_str(), "calc" | "min" | "max" | "clamp")
            }
            _ => false,
        }
    }

    fn is_angle(&self) -> bool {
        match self {
            Self::Function { name, .. } => {
                matches!(name.as_str(), "calc" | "min" | "max" | "clamp")
            }
            other => other.unit_dimension(&["deg", "grad", "rad", "turn"]),
        }
    }

    fn is_time(&self) -> bool {
        match self {
            Self::Function { name, .. } => {
                matches!(name.as_str(), "calc" | "min" | "max" | "clamp")
            }
            other => other.unit_dimension(&["s", "ms"]),
        }
    }

    fn is_resolution(&self) -> bool {
        self.unit_dimension(&["dpi", "dpcm", "dppx", "x"])
    }

    fn is_color(&self) -> bool {
        match self {
            Self::Hash(_) => true,
            Self::Ident(name) => !is_css_wide_keyword(name),
            Self::Function { name, .. } => matches!(
                name.as_str(),
                "rgb"
                    | "rgba"
                    | "hsl"
                    | "hsla"
                    | "hwb"
                    | "lab"
                    | "lch"
                    | "oklab"
                    | "oklch"
                    | "color"
                    | "color-mix"
                    | "light-dark"
                    | "contrast-color"
            ),
            _ => false,
        }
    }

    fn is_url(&self) -> bool {
        match self {
            Self::Url(_) => true,
            Self::Function { name, .. } => matches!(name.as_str(), "url" | "src"),
            _ => false,
        }
    }

    fn is_image(&self) -> bool {
        match self {
            Self::Url(_) => true,
            Self::Function { name, .. } => matches!(
                name.as_str(),
                "url"
                    | "src"
                    | "linear-gradient"
                    | "radial-gradient"
                    | "conic-gradient"
                    | "repeating-linear-gradient"
                    | "repeating-radial-gradient"
                    | "repeating-conic-gradient"
                    | "image-set"
                    | "-webkit-image-set"
                    | "cross-fade"
                    | "element"
                    | "paint"
            ),
            _ => false,
        }
    }

    fn is_transform_function(&self) -> bool {
        let Self::Function { name, .. } = self else {
            return false;
        };
        matches!(
            name.as_str(),
            "matrix"
                | "matrix3d"
                | "translate"
                | "translatex"
                | "translatey"
                | "translatez"
                | "translate3d"
                | "scale"
                | "scalex"
                | "scaley"
                | "scalez"
                | "scale3d"
                | "rotate"
                | "rotatex"
                | "rotatey"
                | "rotatez"
                | "rotate3d"
                | "skew"
                | "skewx"
                | "skewy"
                | "perspective"
        )
    }
}

/// Tokenize a value into the small tree the grammar matcher consumes.
///
/// Whitespace and comments are not semantic separators here: the syntax
/// grammar's `+` multiplier is matched over the remaining value list, which is
/// equivalent because two adjacent tokens are exactly a whitespace-separated
/// sequence. Commas stay as explicit values so `#` can require them.
fn tokenize_values(source: &str) -> Option<Vec<Value>> {
    let mut input = ParserInput::new(source);
    let mut parser = Parser::new(&mut input);
    let values = parse_values(&mut parser).ok()?;
    parser.expect_exhausted().ok()?;
    Some(values)
}

fn parse_values<'i, 't>(parser: &mut Parser<'i, 't>) -> Result<Vec<Value>, ParseError<'i, ()>> {
    let mut values = Vec::new();
    loop {
        let token = match parser.next_including_whitespace_and_comments() {
            Ok(token) => token.clone(),
            Err(_) => break,
        };
        match token {
            Token::WhiteSpace(_) | Token::Comment(_) => {}
            Token::Ident(name) => values.push(Value::Ident(name.to_string())),
            Token::Number {
                value, int_value, ..
            } => values.push(Value::Number {
                value,
                integer: int_value.is_some(),
            }),
            Token::Percentage { .. } => values.push(Value::Percentage),
            Token::Dimension { value, unit, .. } => values.push(Value::Dimension {
                value,
                unit: unit.to_ascii_lowercase(),
            }),
            Token::Hash(value) | Token::IDHash(value) => {
                values.push(Value::Hash(value.to_string()))
            }
            Token::QuotedString(value) => values.push(Value::String(value.to_string())),
            Token::UnquotedUrl(value) => values.push(Value::Url(value.to_string())),
            Token::Function(name) => {
                let name = name.to_ascii_lowercase();
                let args = parser.parse_nested_block(parse_values)?;
                values.push(Value::Function { name, args });
            }
            Token::ParenthesisBlock | Token::SquareBracketBlock | Token::CurlyBracketBlock => {
                let args = parser.parse_nested_block(parse_values)?;
                values.push(Value::Block(args));
            }
            Token::Comma => values.push(Value::Comma),
            Token::Delim(character) => values.push(Value::Delim(character)),
            // A token the grammar has no place for is still a value. Keeping
            // it is what makes the match fail rather than silently skip it.
            _ => values.push(Value::Delim('\0')),
        }
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::{PropertyRegistration, RegisteredSyntax, SyntaxKind, parse_syntax};
    use crate::stylesheet::{Declaration, parse_declaration_list};

    fn registration(source: &str) -> Result<PropertyRegistration, String> {
        // Parse the whole at-rule through the stylesheet parser in the tests
        // that care about integration; this helper isolates descriptor
        // validation so a failure names the registration, not the sheet.
        let (declarations, _) = parse_declaration_list(source);
        super::parse_registration("--token", &declarations)
    }

    fn find(syntax: &RegisteredSyntax, kind: SyntaxKind) -> bool {
        match syntax {
            RegisteredSyntax::Universal => false,
            RegisteredSyntax::Alternatives(alternatives) => alternatives
                .iter()
                .any(|alternative| alternative.terms.iter().any(|term| term.kind == kind)),
        }
    }

    #[test]
    fn parses_supported_syntax_types_and_multipliers() {
        let syntax = parse_syntax("<length> | <color># | <length-percentage>+ | auto").unwrap();
        assert!(find(&syntax, SyntaxKind::Length));
        assert!(find(&syntax, SyntaxKind::Color));
        assert!(find(&syntax, SyntaxKind::LengthPercentage));
        assert!(find(&syntax, SyntaxKind::Literal("auto".to_owned())));
        assert!(syntax.matches("12px"));
        assert!(syntax.matches("red"));
        assert!(syntax.matches("red, blue, #fff"));
        assert!(syntax.matches("10px 20%"));
        assert!(syntax.matches("auto"));
        assert!(!syntax.matches("12"));
        assert!(!syntax.matches("red blue"));
        assert!(!syntax.matches(""));
    }

    #[test]
    fn rejects_syntax_that_cannot_be_implemented() {
        assert!(parse_syntax("").is_err());
        assert!(parse_syntax("<wat>").is_err());
        assert!(parse_syntax("<length").is_err());
        assert!(parse_syntax("<length> |").is_err());
        assert!(parse_syntax("* | <length>").is_err());
    }

    #[test]
    fn registrations_require_the_descriptors_the_spec_requires() {
        assert!(registration("syntax: \"<length>\"; inherits: false; initial-value: 0px").is_ok());
        assert!(registration("syntax: \"*\"; inherits: true").is_ok());
        assert!(registration("inherits: false; initial-value: 0px").is_err());
        assert!(registration("syntax: \"<length>\"; initial-value: 0px").is_err());
        assert!(registration("syntax: \"<length>\"; inherits: maybe; initial-value: 0px").is_err());
        assert!(registration("syntax: \"<length>\"; inherits: false").is_err());
        assert!(registration("syntax: \"<length>\"; inherits: false; initial-value: red").is_err());
        assert!(
            registration("syntax: \"<length>\"; inherits: false; initial-value: var(--x)").is_err()
        );
        assert!(registration("syntax: length; inherits: false; initial-value: 0px").is_err());
    }

    #[test]
    fn all_value_families_match_their_token_shapes() {
        let cases = [
            ("<number>", "1.5", true),
            ("<number>", "1px", false),
            ("<integer>", "2", true),
            ("<integer>", "2.5", false),
            ("<percentage>", "50%", true),
            ("<length>", "2rem", true),
            ("<length>", "calc(1px + 2em)", true),
            ("<length>", "0", true),
            ("<angle>", "45deg", true),
            ("<angle>", "45", false),
            ("<time>", "200ms", true),
            ("<resolution>", "2dppx", true),
            ("<color>", "#fff", true),
            ("<color>", "rgb(1 2 3 / 50%)", true),
            ("<color>", "12px", false),
            ("<image>", "url(a.png)", true),
            ("<image>", "none", false),
            ("<image>", "red", false),
            ("<url>", "url(a.png)", true),
            ("<transform-function>", "translateX(2px)", true),
            ("<transform-list>", "none", false),
            ("<transform-list>", "rotate(1turn) scale(2)", true),
            ("<transform-list>", "red", false),
            ("<custom-ident>", "auto", true),
            ("<custom-ident>", "initial", false),
            ("<string>", "\"hello\"", true),
            ("<string>", "hello", false),
        ];
        for (syntax, value, expected) in cases {
            assert_eq!(
                parse_syntax(syntax).unwrap().matches(value),
                expected,
                "{syntax} / {value}"
            );
        }
    }

    #[test]
    fn descriptor_names_are_ascii_case_insensitive_and_unknown_ones_are_ignored() {
        let registration =
            registration("SYNTAX: \"<color>\"; INHERITS: TRUE; initial-value: red; nonsense: 1")
                .unwrap();
        assert!(registration.inherits);
        assert_eq!(registration.initial_value.as_deref(), Some("red"));
    }

    #[test]
    fn declaration_helper_is_used_not_a_second_parser() {
        // The helper above is the same declaration-list parser the stylesheet
        // uses, so this pins that a malformed descriptor is rejected there and
        // not silently repaired here.
        let (declarations, diagnostics) =
            parse_declaration_list("@nonesuch x; syntax: \"<length>\"");
        assert_eq!(declarations.len(), 1);
        assert!(!diagnostics.is_empty());
        assert!(super::parse_registration("--x", &declarations).is_err());
        let declaration = Declaration {
            name: "syntax".to_owned(),
            value: "\"<length>\"".to_owned(),
            important: false,
        };
        assert!(
            super::parse_registration(
                "--x",
                &[
                    declaration,
                    Declaration {
                        name: "inherits".to_owned(),
                        value: "false".to_owned(),
                        important: false,
                    },
                    Declaration {
                        name: "initial-value".to_owned(),
                        value: "0px".to_owned(),
                        important: false,
                    }
                ]
            )
            .is_ok()
        );
    }
}
