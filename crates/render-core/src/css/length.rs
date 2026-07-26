use std::error::Error;
use std::fmt;

/// Inputs needed to turn supported CSS lengths into used pixel values.
///
/// Percentages deliberately remain unresolved when `percentage_base` is
/// absent. Font-relative `ex` and `ch` units also require measured metrics;
/// guessing them as half an `em` would not be standards-correct.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LengthContext {
    pub percentage_base: Option<f64>,
    pub em_base: f64,
    pub rem_base: f64,
    pub viewport_width: f64,
    pub viewport_height: f64,
    pub small_viewport_width: Option<f64>,
    pub small_viewport_height: Option<f64>,
    pub large_viewport_width: Option<f64>,
    pub large_viewport_height: Option<f64>,
    pub dynamic_viewport_width: Option<f64>,
    pub dynamic_viewport_height: Option<f64>,
    pub ex_base: Option<f64>,
    pub ch_base: Option<f64>,
}

impl Default for LengthContext {
    fn default() -> Self {
        Self {
            percentage_base: None,
            em_base: 16.0,
            rem_base: 16.0,
            viewport_width: 0.0,
            viewport_height: 0.0,
            small_viewport_width: None,
            small_viewport_height: None,
            large_viewport_width: None,
            large_viewport_height: None,
            dynamic_viewport_width: None,
            dynamic_viewport_height: None,
            ex_base: None,
            ch_base: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CssValueError {
    offset: usize,
    message: String,
}

impl CssValueError {
    #[must_use]
    pub fn offset(&self) -> usize {
        self.offset
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    fn at(offset: usize, message: impl Into<String>) -> Self {
        Self {
            offset,
            message: message.into(),
        }
    }
}

impl fmt::Display for CssValueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} at byte {}", self.message, self.offset)
    }
}

impl Error for CssValueError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NumericKind {
    Number,
    Length,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct NumericValue {
    value: f64,
    kind: NumericKind,
}

impl NumericValue {
    const fn number(value: f64) -> Self {
        Self {
            value,
            kind: NumericKind::Number,
        }
    }

    const fn length(value: f64) -> Self {
        Self {
            value,
            kind: NumericKind::Length,
        }
    }
}

/// Resolve a CSS length expression to CSS pixels.
///
/// This first Rust slice supports absolute lengths, common font/viewport
/// relative lengths, percentages, `calc()`, `min()`, `max()`, `clamp()`, and
/// unresolved `var()` fallbacks. It keeps number and length dimensions separate
/// so invalid expressions such as `1px + 2` or `1px * 2px` are rejected.
///
/// # Errors
///
/// Returns [`CssValueError`] when the expression is invalid, contains an
/// unsupported unit or function, or needs a percentage/font metric that is not
/// present in the supplied context.
pub fn resolve_length_expr(input: &str, context: &LengthContext) -> Result<f64, CssValueError> {
    let mut parser = Parser::new(input, context);
    parser.skip_whitespace_and_comments()?;
    if parser.is_eof() {
        return Err(parser.error("expected a CSS length"));
    }

    let result = parser.parse_sum()?;
    parser.skip_whitespace_and_comments()?;
    if !parser.is_eof() {
        return Err(parser.error("unexpected trailing input"));
    }

    match result.kind {
        NumericKind::Length => Ok(result.value),
        NumericKind::Number if result.value == 0.0 => Ok(0.0),
        NumericKind::Number => Err(CssValueError::at(
            0,
            "a non-zero unitless number is not a CSS length",
        )),
    }
}

struct Parser<'a> {
    input: &'a str,
    offset: usize,
    context: &'a LengthContext,
}

impl<'a> Parser<'a> {
    const fn new(input: &'a str, context: &'a LengthContext) -> Self {
        Self {
            input,
            offset: 0,
            context,
        }
    }

    fn parse_sum(&mut self) -> Result<NumericValue, CssValueError> {
        let mut left = self.parse_product()?;

        loop {
            let before_space = self.offset;
            self.skip_whitespace_and_comments()?;
            let had_space_before = self.offset > before_space;
            let Some(operator) = self.peek_char() else {
                break;
            };
            if operator != '+' && operator != '-' {
                break;
            }
            if !had_space_before {
                return Err(self.error("binary '+' and '-' require surrounding whitespace"));
            }
            self.bump_char();
            let after_operator = self.offset;
            self.skip_whitespace_and_comments()?;
            if self.offset == after_operator {
                return Err(self.error("binary '+' and '-' require surrounding whitespace"));
            }

            let right = self.parse_product()?;
            if left.kind != right.kind {
                return Err(self.error("cannot add or subtract incompatible CSS numeric types"));
            }
            left.value = if operator == '+' {
                left.value + right.value
            } else {
                left.value - right.value
            };
        }

        Ok(left)
    }

    fn parse_product(&mut self) -> Result<NumericValue, CssValueError> {
        let mut left = self.parse_unary()?;

        loop {
            let before_space = self.offset;
            self.skip_whitespace_and_comments()?;
            let Some(operator) = self.peek_char() else {
                break;
            };
            if operator != '*' && operator != '/' {
                self.offset = before_space;
                break;
            }
            self.bump_char();
            self.skip_whitespace_and_comments()?;
            let right = self.parse_unary()?;

            left = match operator {
                '*' => match (left.kind, right.kind) {
                    (NumericKind::Number, NumericKind::Number) => {
                        NumericValue::number(left.value * right.value)
                    }
                    (NumericKind::Length, NumericKind::Number)
                    | (NumericKind::Number, NumericKind::Length) => {
                        NumericValue::length(left.value * right.value)
                    }
                    (NumericKind::Length, NumericKind::Length) => {
                        return Err(self.error("multiplying two CSS lengths is invalid"));
                    }
                },
                '/' => {
                    if right.value == 0.0 {
                        return Err(self.error("division by zero in CSS expression"));
                    }
                    if right.kind != NumericKind::Number {
                        return Err(self.error("a CSS length may only be divided by a number"));
                    }
                    NumericValue {
                        value: left.value / right.value,
                        kind: left.kind,
                    }
                }
                _ => unreachable!(),
            };
        }

        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<NumericValue, CssValueError> {
        self.skip_whitespace_and_comments()?;
        match self.peek_char() {
            Some('+') => {
                self.bump_char();
                self.parse_unary()
            }
            Some('-') => {
                self.bump_char();
                let mut value = self.parse_unary()?;
                value.value = -value.value;
                Ok(value)
            }
            _ => self.parse_primary(),
        }
    }

    fn parse_primary(&mut self) -> Result<NumericValue, CssValueError> {
        self.skip_whitespace_and_comments()?;
        match self.peek_char() {
            Some('(') => {
                self.bump_char();
                let value = self.parse_sum()?;
                self.expect_char(')')?;
                Ok(value)
            }
            Some(character) if character.is_ascii_digit() || character == '.' => {
                self.parse_numeric()
            }
            Some(character) if is_identifier_start(character) => self.parse_function(),
            Some(_) => Err(self.error("expected a CSS number, dimension, or math function")),
            None => Err(self.error("unexpected end of CSS expression")),
        }
    }

    fn parse_numeric(&mut self) -> Result<NumericValue, CssValueError> {
        let number_offset = self.offset;
        let number = self.parse_number()?;
        if !number.is_finite() {
            return Err(CssValueError::at(number_offset, "CSS number is not finite"));
        }

        if self.consume_char('%') {
            let base = self.context.percentage_base.ok_or_else(|| {
                CssValueError::at(
                    number_offset,
                    "percentage has no containing-block reference",
                )
            })?;
            return Ok(NumericValue::length(number * base / 100.0));
        }

        let unit = self.parse_identifier();
        if unit.is_empty() {
            return Ok(NumericValue::number(number));
        }

        let factor = self.unit_factor(&unit, number_offset)?;
        Ok(NumericValue::length(number * factor))
    }

    fn parse_function(&mut self) -> Result<NumericValue, CssValueError> {
        let function_offset = self.offset;
        let name = self.parse_identifier().to_ascii_lowercase();
        self.skip_whitespace_and_comments()?;
        if !self.consume_char('(') {
            return Err(CssValueError::at(
                function_offset,
                format!("unsupported CSS identifier '{name}'"),
            ));
        }

        match name.as_str() {
            "calc" => {
                let value = self.parse_sum()?;
                self.expect_char(')')?;
                Ok(value)
            }
            "min" => self.parse_min_max(true),
            "max" => self.parse_min_max(false),
            "clamp" => self.parse_clamp(),
            "var" => self.parse_var_fallback(),
            _ => Err(CssValueError::at(
                function_offset,
                format!("unsupported CSS function '{name}'"),
            )),
        }
    }

    fn parse_min_max(&mut self, is_min: bool) -> Result<NumericValue, CssValueError> {
        let mut result = self.parse_sum()?;
        let mut count = 1_usize;
        while self.consume_comma()? {
            let candidate = self.parse_sum()?;
            if candidate.kind != result.kind {
                return Err(self.error("math function arguments have incompatible types"));
            }
            result.value = if is_min {
                result.value.min(candidate.value)
            } else {
                result.value.max(candidate.value)
            };
            count += 1;
        }
        self.expect_char(')')?;
        if count == 0 {
            return Err(self.error("math function requires an argument"));
        }
        Ok(result)
    }

    fn parse_clamp(&mut self) -> Result<NumericValue, CssValueError> {
        let lower = self.parse_sum()?;
        self.expect_comma()?;
        let preferred = self.parse_sum()?;
        self.expect_comma()?;
        let upper = self.parse_sum()?;
        self.expect_char(')')?;

        if lower.kind != preferred.kind || preferred.kind != upper.kind {
            return Err(self.error("clamp() arguments have incompatible types"));
        }
        Ok(NumericValue {
            value: preferred.value.max(lower.value).min(upper.value),
            kind: preferred.kind,
        })
    }

    fn parse_var_fallback(&mut self) -> Result<NumericValue, CssValueError> {
        self.skip_whitespace_and_comments()?;
        let variable_offset = self.offset;
        if !self.remaining().starts_with("--") {
            return Err(CssValueError::at(
                variable_offset,
                "var() requires a custom property name",
            ));
        }
        self.offset += 2;
        let name_tail = self.parse_identifier();
        if name_tail.is_empty() {
            return Err(CssValueError::at(
                variable_offset,
                "var() requires a custom property name",
            ));
        }
        self.skip_whitespace_and_comments()?;
        if self.consume_char(')') {
            return Err(CssValueError::at(
                variable_offset,
                "custom property is unresolved and var() has no fallback",
            ));
        }
        self.expect_char(',')?;
        let fallback = self.parse_sum()?;
        self.expect_char(')')?;
        Ok(fallback)
    }

    fn parse_number(&mut self) -> Result<f64, CssValueError> {
        let start = self.offset;
        let mut saw_digit = false;

        while self
            .peek_char()
            .is_some_and(|character| character.is_ascii_digit())
        {
            saw_digit = true;
            self.bump_char();
        }
        if self.consume_char('.') {
            while self
                .peek_char()
                .is_some_and(|character| character.is_ascii_digit())
            {
                saw_digit = true;
                self.bump_char();
            }
        }
        if !saw_digit {
            return Err(CssValueError::at(start, "invalid CSS number"));
        }

        if matches!(self.peek_char(), Some('e' | 'E')) {
            let exponent_start = self.offset;
            self.bump_char();
            if matches!(self.peek_char(), Some('+' | '-')) {
                self.bump_char();
            }
            let digits_start = self.offset;
            while self
                .peek_char()
                .is_some_and(|character| character.is_ascii_digit())
            {
                self.bump_char();
            }
            if self.offset == digits_start {
                self.offset = exponent_start;
            }
        }

        self.input[start..self.offset]
            .parse::<f64>()
            .map_err(|_| CssValueError::at(start, "invalid CSS number"))
    }

    fn unit_factor(&self, unit: &str, offset: usize) -> Result<f64, CssValueError> {
        let factor = match unit.to_ascii_lowercase().as_str() {
            "px" => 1.0,
            "em" => self.context.em_base,
            "rem" => self.context.rem_base,
            "vw" => self.context.viewport_width / 100.0,
            "vh" => self.context.viewport_height / 100.0,
            "vmin" => {
                self.context
                    .viewport_width
                    .min(self.context.viewport_height)
                    / 100.0
            }
            "vmax" => {
                self.context
                    .viewport_width
                    .max(self.context.viewport_height)
                    / 100.0
            }
            "svw" => {
                self.context
                    .small_viewport_width
                    .unwrap_or(self.context.viewport_width)
                    / 100.0
            }
            "svh" => {
                self.context
                    .small_viewport_height
                    .unwrap_or(self.context.viewport_height)
                    / 100.0
            }
            "lvw" => {
                self.context
                    .large_viewport_width
                    .unwrap_or(self.context.viewport_width)
                    / 100.0
            }
            "lvh" => {
                self.context
                    .large_viewport_height
                    .unwrap_or(self.context.viewport_height)
                    / 100.0
            }
            "dvw" => {
                self.context
                    .dynamic_viewport_width
                    .unwrap_or(self.context.viewport_width)
                    / 100.0
            }
            "dvh" => {
                self.context
                    .dynamic_viewport_height
                    .unwrap_or(self.context.viewport_height)
                    / 100.0
            }
            "in" => 96.0,
            "cm" => 96.0 / 2.54,
            "mm" => 96.0 / 25.4,
            "q" => 96.0 / 101.6,
            "pt" => 96.0 / 72.0,
            "pc" => 16.0,
            "ex" => self
                .context
                .ex_base
                .ok_or_else(|| CssValueError::at(offset, "ex requires a measured font x-height"))?,
            "ch" => self.context.ch_base.ok_or_else(|| {
                CssValueError::at(offset, "ch requires a measured zero-glyph advance")
            })?,
            _ => {
                return Err(CssValueError::at(
                    offset,
                    format!("unsupported CSS length unit '{unit}'"),
                ));
            }
        };
        Ok(factor)
    }

    fn parse_identifier(&mut self) -> String {
        let start = self.offset;
        while self.peek_char().is_some_and(|character| {
            character.is_ascii_alphanumeric() || character == '-' || character == '_'
        }) {
            self.bump_char();
        }
        self.input[start..self.offset].to_owned()
    }

    fn consume_comma(&mut self) -> Result<bool, CssValueError> {
        self.skip_whitespace_and_comments()?;
        Ok(self.consume_char(','))
    }

    fn expect_comma(&mut self) -> Result<(), CssValueError> {
        if self.consume_comma()? {
            Ok(())
        } else {
            Err(self.error("expected ','"))
        }
    }

    fn expect_char(&mut self, expected: char) -> Result<(), CssValueError> {
        self.skip_whitespace_and_comments()?;
        if self.consume_char(expected) {
            Ok(())
        } else {
            Err(self.error(format!("expected '{expected}'")))
        }
    }

    fn skip_whitespace_and_comments(&mut self) -> Result<(), CssValueError> {
        loop {
            while self.peek_char().is_some_and(char::is_whitespace) {
                self.bump_char();
            }
            if !self.remaining().starts_with("/*") {
                return Ok(());
            }
            let comment_offset = self.offset;
            self.offset += 2;
            let Some(end) = self.remaining().find("*/") else {
                return Err(CssValueError::at(
                    comment_offset,
                    "unterminated CSS comment",
                ));
            };
            self.offset += end + 2;
        }
    }

    fn consume_char(&mut self, expected: char) -> bool {
        if self.peek_char() == Some(expected) {
            self.bump_char();
            true
        } else {
            false
        }
    }

    fn peek_char(&self) -> Option<char> {
        self.remaining().chars().next()
    }

    fn bump_char(&mut self) -> Option<char> {
        let character = self.peek_char()?;
        self.offset += character.len_utf8();
        Some(character)
    }

    fn remaining(&self) -> &'a str {
        &self.input[self.offset..]
    }

    fn is_eof(&self) -> bool {
        self.offset >= self.input.len()
    }

    fn error(&self, message: impl Into<String>) -> CssValueError {
        CssValueError::at(self.offset, message)
    }
}

const fn is_identifier_start(character: char) -> bool {
    character.is_ascii_alphabetic() || character == '_'
}

#[cfg(test)]
mod tests {
    #![allow(clippy::float_cmp)]

    use super::{LengthContext, resolve_length_expr};

    fn context() -> LengthContext {
        LengthContext {
            percentage_base: Some(1_440.0),
            em_base: 20.0,
            rem_base: 16.0,
            viewport_width: 1_440.0,
            viewport_height: 900.0,
            ..LengthContext::default()
        }
    }

    fn resolve(value: &str) -> f64 {
        resolve_length_expr(value, &context()).unwrap()
    }

    #[test]
    fn resolves_absolute_relative_and_percentage_lengths() {
        assert_eq!(resolve("1in"), 96.0);
        assert!((resolve("2.54cm") - 96.0).abs() < 0.000_001);
        assert_eq!(resolve("calc(2rem + 0.5em)"), 42.0);
        assert_eq!(resolve("100%"), 1_440.0);
    }

    #[test]
    fn resolves_css_math_functions() {
        assert_eq!(resolve("calc(456px*2)"), 912.0);
        assert_eq!(resolve("calc(100% - 40px)"), 1_400.0);
        assert_eq!(resolve("calc((100vw - 40px) / 2)"), 700.0);
        assert_eq!(resolve("min(20px, 5vw)"), 20.0);
        assert_eq!(resolve("max(10px, 2vw)"), 28.8);
        assert_eq!(resolve("clamp(10px, 5vw, 80px)"), 72.0);
    }

    #[test]
    fn resolves_unset_custom_property_fallback_without_erasing_units() {
        assert_eq!(resolve("calc(108px * var(--clientfont-scale, 1))"), 108.0);
    }

    #[test]
    fn rejects_dimensionally_invalid_math() {
        assert!(resolve_length_expr("calc(1px + 2)", &context()).is_err());
        assert!(resolve_length_expr("calc(1px * 2px)", &context()).is_err());
        assert!(resolve_length_expr("calc(10px / 2px)", &context()).is_err());
        assert!(resolve_length_expr("12", &context()).is_err());
    }

    #[test]
    fn follows_css_whitespace_rules_for_addition_and_subtraction() {
        assert!(resolve_length_expr("calc(10px+2px)", &context()).is_err());
        assert_eq!(resolve("calc(10px + 2px)"), 12.0);
    }

    #[test]
    fn does_not_guess_font_metrics_for_ex_and_ch() {
        assert!(resolve_length_expr("1ex", &context()).is_err());
        assert!(resolve_length_expr("1ch", &context()).is_err());

        let measured = LengthContext {
            ex_base: Some(9.5),
            ch_base: Some(10.25),
            ..context()
        };
        assert_eq!(resolve_length_expr("2ex", &measured).unwrap(), 19.0);
        assert_eq!(resolve_length_expr("2ch", &measured).unwrap(), 20.5);
    }

    #[test]
    fn rejects_unknown_or_incomplete_syntax_instead_of_skipping_it() {
        assert!(resolve_length_expr("calc(10px + garbage)", &context()).is_err());
        assert!(resolve_length_expr("calc(10px / 0)", &context()).is_err());
        assert!(resolve_length_expr("calc(10px + 2px", &context()).is_err());
        assert!(resolve_length_expr("calc(10px +/* broken)", &context()).is_err());
    }
}
