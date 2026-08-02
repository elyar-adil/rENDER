use super::{JsError, JsErrorKind, RuntimeLimits};

#[derive(Clone, Debug, PartialEq)]
pub(super) enum TokenKind {
    Identifier(String),
    String(String),
    Number(f64),
    Let,
    Const,
    Var,
    Function,
    Return,
    New,
    Throw,
    Try,
    Catch,
    Finally,
    If,
    Else,
    While,
    For,
    Break,
    Continue,
    Delete,
    Typeof,
    In,
    Instanceof,
    Switch,
    Case,
    Default,
    True,
    False,
    Null,
    Undefined,
    This,
    Dot,
    Comma,
    Semicolon,
    LeftParen,
    RightParen,
    LeftBrace,
    RightBrace,
    LeftBracket,
    RightBracket,
    Colon,
    Plus,
    PlusPlus,
    PlusEqual,
    Minus,
    MinusMinus,
    MinusEqual,
    Star,
    Slash,
    Percent,
    Bang,
    Equal,
    EqualEqual,
    EqualEqualEqual,
    BangEqual,
    BangEqualEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    LeftShift,
    LeftShiftEqual,
    RightShift,
    RightShiftEqual,
    UnsignedRightShift,
    UnsignedRightShiftEqual,
    Ampersand,
    AmpersandEqual,
    AndAnd,
    Pipe,
    PipeEqual,
    OrOr,
    Caret,
    CaretEqual,
    Tilde,
    Question,
    Arrow,
    Eof,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Token {
    pub kind: TokenKind,
    pub offset: usize,
}

pub(super) fn tokenize(source: &str, limits: &RuntimeLimits) -> Result<Vec<Token>, JsError> {
    if source.len() > limits.max_source_bytes {
        return Err(JsError::new(
            JsErrorKind::ResourceLimit,
            format!(
                "script contains {} bytes, exceeding the {} byte limit",
                source.len(),
                limits.max_source_bytes
            ),
            None,
        ));
    }
    Lexer {
        source,
        offset: 0,
        tokens: Vec::new(),
        max_tokens: limits.max_tokens,
    }
    .run()
}

struct Lexer<'a> {
    source: &'a str,
    offset: usize,
    tokens: Vec<Token>,
    max_tokens: usize,
}

impl Lexer<'_> {
    #[allow(clippy::too_many_lines)]
    fn run(mut self) -> Result<Vec<Token>, JsError> {
        while let Some(character) = self.peek() {
            if character.is_ascii_whitespace() {
                self.advance();
                continue;
            }
            let start = self.offset;
            let kind = match character {
                '.' if self
                    .peek_second()
                    .is_some_and(|value| value.is_ascii_digit()) =>
                {
                    self.number()?
                }
                '.' => self.single(TokenKind::Dot),
                ',' => self.single(TokenKind::Comma),
                ';' => self.single(TokenKind::Semicolon),
                '(' => self.single(TokenKind::LeftParen),
                ')' => self.single(TokenKind::RightParen),
                '{' => self.single(TokenKind::LeftBrace),
                '}' => self.single(TokenKind::RightBrace),
                '[' => self.single(TokenKind::LeftBracket),
                ']' => self.single(TokenKind::RightBracket),
                ':' => self.single(TokenKind::Colon),
                '?' => self.single(TokenKind::Question),
                '~' => self.single(TokenKind::Tilde),
                '+' if self.peek_second() == Some('+') => {
                    self.advance();
                    self.advance();
                    TokenKind::PlusPlus
                }
                '+' if self.peek_second() == Some('=') => {
                    self.advance();
                    self.advance();
                    TokenKind::PlusEqual
                }
                '+' => self.single(TokenKind::Plus),
                '-' if self.peek_second() == Some('-') => {
                    self.advance();
                    self.advance();
                    TokenKind::MinusMinus
                }
                '-' if self.peek_second() == Some('=') => {
                    self.advance();
                    self.advance();
                    TokenKind::MinusEqual
                }
                '-' => self.single(TokenKind::Minus),
                '*' => self.single(TokenKind::Star),
                '%' => self.single(TokenKind::Percent),
                '=' => self.equals(),
                '!' => self.bang(),
                '<' => self.less(),
                '>' => self.greater(),
                '&' if self.peek_second() == Some('&') => {
                    self.advance();
                    self.advance();
                    TokenKind::AndAnd
                }
                '&' if self.peek_second() == Some('=') => {
                    self.advance();
                    self.advance();
                    TokenKind::AmpersandEqual
                }
                '&' => self.single(TokenKind::Ampersand),
                '|' if self.peek_second() == Some('|') => {
                    self.advance();
                    self.advance();
                    TokenKind::OrOr
                }
                '|' if self.peek_second() == Some('=') => {
                    self.advance();
                    self.advance();
                    TokenKind::PipeEqual
                }
                '|' => self.single(TokenKind::Pipe),
                '^' if self.peek_second() == Some('=') => {
                    self.advance();
                    self.advance();
                    TokenKind::CaretEqual
                }
                '^' => self.single(TokenKind::Caret),
                '/' if self.peek_second() == Some('/') => {
                    self.line_comment();
                    continue;
                }
                '/' if self.peek_second() == Some('*') => {
                    self.block_comment(start)?;
                    continue;
                }
                '/' => self.single(TokenKind::Slash),
                '\'' | '"' => self.string(character)?,
                '0'..='9' => self.number()?,
                '\\' if self.peek_second() == Some('u') => self.identifier()?,
                value if is_identifier_start(value) => self.identifier()?,
                _ => {
                    return Err(JsError::syntax(
                        format!("unsupported character {character:?}"),
                        start,
                    ));
                }
            };
            self.push(kind, start)?;
        }
        self.push(TokenKind::Eof, self.offset)?;
        Ok(self.tokens)
    }

    fn single(&mut self, kind: TokenKind) -> TokenKind {
        self.advance();
        kind
    }

    fn equals(&mut self) -> TokenKind {
        self.advance();
        if self.peek() == Some('>') {
            self.advance();
            return TokenKind::Arrow;
        }
        if self.peek() != Some('=') {
            return TokenKind::Equal;
        }
        self.advance();
        if self.peek() == Some('=') {
            self.advance();
            TokenKind::EqualEqualEqual
        } else {
            TokenKind::EqualEqual
        }
    }

    fn bang(&mut self) -> TokenKind {
        self.advance();
        if self.peek() != Some('=') {
            return TokenKind::Bang;
        }
        self.advance();
        if self.peek() == Some('=') {
            self.advance();
            TokenKind::BangEqualEqual
        } else {
            TokenKind::BangEqual
        }
    }

    fn less(&mut self) -> TokenKind {
        self.advance();
        if self.peek() == Some('<') {
            self.advance();
            if self.peek() == Some('=') {
                self.advance();
                return TokenKind::LeftShiftEqual;
            }
            return TokenKind::LeftShift;
        }
        if self.peek() == Some('=') {
            self.advance();
            TokenKind::LessEqual
        } else {
            TokenKind::Less
        }
    }

    fn greater(&mut self) -> TokenKind {
        self.advance();
        if self.peek() == Some('>') {
            self.advance();
            if self.peek() == Some('>') {
                self.advance();
                if self.peek() == Some('=') {
                    self.advance();
                    return TokenKind::UnsignedRightShiftEqual;
                }
                return TokenKind::UnsignedRightShift;
            }
            if self.peek() == Some('=') {
                self.advance();
                return TokenKind::RightShiftEqual;
            }
            return TokenKind::RightShift;
        }
        if self.peek() == Some('=') {
            self.advance();
            TokenKind::GreaterEqual
        } else {
            TokenKind::Greater
        }
    }

    fn identifier(&mut self) -> Result<TokenKind, JsError> {
        let start = self.offset;
        let first = self.identifier_character(start)?;
        if !is_identifier_start(first) {
            return Err(JsError::syntax("invalid identifier start", start));
        }
        let mut identifier = String::from(first);
        while let Some(character) = self.peek() {
            let character = if character == '\\' && self.peek_second() == Some('u') {
                self.identifier_escape(start)?
            } else if is_identifier_continue(character) {
                self.advance();
                character
            } else {
                break;
            };
            if !is_identifier_continue(character) {
                return Err(JsError::syntax("invalid identifier character", start));
            }
            identifier.push(character);
        }
        Ok(match identifier.as_str() {
            "let" => TokenKind::Let,
            "const" => TokenKind::Const,
            "var" => TokenKind::Var,
            "function" => TokenKind::Function,
            "return" => TokenKind::Return,
            "new" => TokenKind::New,
            "throw" => TokenKind::Throw,
            "try" => TokenKind::Try,
            "catch" => TokenKind::Catch,
            "finally" => TokenKind::Finally,
            "if" => TokenKind::If,
            "else" => TokenKind::Else,
            "while" => TokenKind::While,
            "for" => TokenKind::For,
            "break" => TokenKind::Break,
            "continue" => TokenKind::Continue,
            "delete" => TokenKind::Delete,
            "typeof" => TokenKind::Typeof,
            "in" => TokenKind::In,
            "instanceof" => TokenKind::Instanceof,
            "switch" => TokenKind::Switch,
            "case" => TokenKind::Case,
            "default" => TokenKind::Default,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            "null" => TokenKind::Null,
            "undefined" => TokenKind::Undefined,
            "this" => TokenKind::This,
            _ => TokenKind::Identifier(identifier),
        })
    }

    fn identifier_character(&mut self, start: usize) -> Result<char, JsError> {
        if self.peek() == Some('\\') {
            self.identifier_escape(start)
        } else {
            let character = self
                .peek()
                .expect("identifier scanning starts at a source character");
            self.advance();
            Ok(character)
        }
    }

    fn identifier_escape(&mut self, start: usize) -> Result<char, JsError> {
        self.advance();
        if self.peek() != Some('u') {
            return Err(JsError::syntax(
                "invalid Unicode escape in identifier",
                start,
            ));
        }
        self.advance();

        let digits_start = self.offset;
        let value = if self.peek() == Some('{') {
            self.advance();
            let digits_start = self.offset;
            while self.peek().is_some_and(|value| value.is_ascii_hexdigit()) {
                self.advance();
            }
            if self.offset == digits_start || self.offset - digits_start > 6 {
                return Err(JsError::syntax(
                    "invalid Unicode escape in identifier",
                    start,
                ));
            }
            let value = u32::from_str_radix(&self.source[digits_start..self.offset], 16)
                .map_err(|_| JsError::syntax("invalid Unicode escape in identifier", start))?;
            if self.peek() != Some('}') {
                return Err(JsError::syntax(
                    "invalid Unicode escape in identifier",
                    start,
                ));
            }
            self.advance();
            value
        } else {
            for _ in 0..4 {
                if !self.peek().is_some_and(|value| value.is_ascii_hexdigit()) {
                    return Err(JsError::syntax(
                        "invalid Unicode escape in identifier",
                        start,
                    ));
                }
                self.advance();
            }
            u32::from_str_radix(&self.source[digits_start..self.offset], 16)
                .map_err(|_| JsError::syntax("invalid Unicode escape in identifier", start))?
        };
        char::from_u32(value)
            .ok_or_else(|| JsError::syntax("invalid Unicode escape in identifier", start))
    }

    #[allow(
        clippy::cast_precision_loss,
        reason = "ECMAScript numeric literals are rounded to binary64 Number values"
    )]
    fn number(&mut self) -> Result<TokenKind, JsError> {
        let start = self.offset;
        if self.peek() == Some('0') {
            let radix = match self.peek_second() {
                Some('x' | 'X') => Some(16),
                Some('o' | 'O') => Some(8),
                Some('b' | 'B') => Some(2),
                _ => None,
            };
            if let Some(radix) = radix {
                self.advance();
                self.advance();
                let digits_start = self.offset;
                while self.peek().is_some_and(|value| value.is_digit(radix)) {
                    self.advance();
                }
                if self.offset == digits_start || self.peek().is_some_and(is_identifier_continue) {
                    return Err(JsError::syntax("invalid numeric literal", start));
                }
                return u64::from_str_radix(&self.source[digits_start..self.offset], radix)
                    .map(|value| TokenKind::Number(value as f64))
                    .map_err(|_| JsError::syntax("invalid numeric literal", start));
            }
        }
        while self.peek().is_some_and(|value| value.is_ascii_digit()) {
            self.advance();
        }
        if self.peek() == Some('.') {
            self.advance();
            while self.peek().is_some_and(|value| value.is_ascii_digit()) {
                self.advance();
            }
        }
        if self.peek().is_some_and(|value| matches!(value, 'e' | 'E')) {
            self.advance();
            if self.peek().is_some_and(|value| matches!(value, '+' | '-')) {
                self.advance();
            }
            let exponent_start = self.offset;
            while self.peek().is_some_and(|value| value.is_ascii_digit()) {
                self.advance();
            }
            if self.offset == exponent_start {
                return Err(JsError::syntax("invalid numeric literal", start));
            }
        }
        if self.peek().is_some_and(is_identifier_start) {
            return Err(JsError::syntax("invalid numeric literal", start));
        }
        self.source[start..self.offset]
            .parse::<f64>()
            .map(TokenKind::Number)
            .map_err(|_| JsError::syntax("invalid numeric literal", start))
    }

    fn string(&mut self, quote: char) -> Result<TokenKind, JsError> {
        let start = self.offset;
        self.advance();
        let mut value = String::new();
        while let Some(character) = self.peek() {
            self.advance();
            if character == quote {
                return Ok(TokenKind::String(value));
            }
            if character == '\\' {
                let escaped = self
                    .peek()
                    .ok_or_else(|| JsError::syntax("unterminated string escape", self.offset))?;
                self.advance();
                let escaped = match escaped {
                    '\n' => continue,
                    '\r' => {
                        if self.peek() == Some('\n') {
                            self.advance();
                        }
                        continue;
                    }
                    '0' if !self.peek().is_some_and(|value| value.is_ascii_digit()) => {
                        value.push('\0');
                        continue;
                    }
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    'b' => '\u{0008}',
                    'f' => '\u{000c}',
                    'v' => '\u{000b}',
                    '\\' => '\\',
                    '\'' => '\'',
                    '"' => '"',
                    'x' => self.hex_escape(2)?,
                    'u' => self.hex_escape(4)?,
                    other => other,
                };
                value.push(escaped);
            } else if matches!(character, '\n' | '\r') {
                return Err(JsError::syntax("newline in string literal", self.offset));
            } else {
                value.push(character);
            }
        }
        Err(JsError::syntax("unterminated string literal", start))
    }

    fn hex_escape(&mut self, digits: usize) -> Result<char, JsError> {
        let start = self.offset;
        for _ in 0..digits {
            if !self.peek().is_some_and(|value| value.is_ascii_hexdigit()) {
                return Err(JsError::syntax("invalid hexadecimal escape", start));
            }
            self.advance();
        }
        let value = u32::from_str_radix(&self.source[start..self.offset], 16)
            .map_err(|_| JsError::syntax("invalid hexadecimal escape", start))?;
        char::from_u32(value).ok_or_else(|| JsError::syntax("invalid Unicode escape", start))
    }

    fn line_comment(&mut self) {
        self.advance();
        self.advance();
        while self.peek().is_some_and(|value| value != '\n') {
            self.advance();
        }
    }

    fn block_comment(&mut self, start: usize) -> Result<(), JsError> {
        self.advance();
        self.advance();
        while let Some(character) = self.peek() {
            if character == '*' && self.peek_second() == Some('/') {
                self.advance();
                self.advance();
                return Ok(());
            }
            self.advance();
        }
        Err(JsError::syntax("unterminated block comment", start))
    }

    fn push(&mut self, kind: TokenKind, offset: usize) -> Result<(), JsError> {
        if self.tokens.len() >= self.max_tokens {
            return Err(JsError::new(
                JsErrorKind::ResourceLimit,
                format!("script exceeds the {} token limit", self.max_tokens),
                Some(offset),
            ));
        }
        self.tokens.push(Token { kind, offset });
        Ok(())
    }

    fn peek(&self) -> Option<char> {
        self.source[self.offset..].chars().next()
    }

    fn peek_second(&self) -> Option<char> {
        let mut characters = self.source[self.offset..].chars();
        characters.next()?;
        characters.next()
    }

    fn advance(&mut self) {
        if let Some(character) = self.peek() {
            self.offset += character.len_utf8();
        }
    }
}

fn is_identifier_start(character: char) -> bool {
    character.is_alphabetic() || matches!(character, '_' | '$')
}

fn is_identifier_continue(character: char) -> bool {
    is_identifier_start(character)
        || character.is_alphanumeric()
        || matches!(character, '\u{200c}' | '\u{200d}')
}

#[cfg(test)]
mod tests {
    use super::{TokenKind, tokenize};
    use crate::js::RuntimeLimits;

    #[test]
    fn tokenizes_member_calls_strings_and_control_flow() {
        let tokens = tokenize(
            "if (value >= 2 && value !== 3) { const node = document.getElementById('message'); }",
            &RuntimeLimits::default(),
        )
        .expect("supported source should tokenize");
        assert!(tokens.iter().any(|token| token.kind == TokenKind::If));
        assert!(
            tokens
                .iter()
                .any(|token| token.kind == TokenKind::GreaterEqual)
        );
        assert!(tokens.iter().any(|token| token.kind == TokenKind::AndAnd));
        assert!(
            tokens
                .iter()
                .any(|token| token.kind == TokenKind::BangEqualEqual)
        );
        assert!(
            tokens
                .iter()
                .any(|token| { token.kind == TokenKind::Identifier("getElementById".to_owned()) })
        );
        assert!(
            tokens
                .iter()
                .any(|token| token.kind == TokenKind::String("message".to_owned()))
        );
    }

    #[test]
    fn tokenizes_unicode_escaped_identifiers_and_keywords() {
        let tokens = tokenize(
            r"let \u{61} = 1; \u0069f (true) {}",
            &RuntimeLimits::default(),
        )
        .expect("Unicode escapes in identifiers should tokenize");

        assert!(
            tokens
                .iter()
                .any(|token| token.kind == TokenKind::Identifier("a".to_owned()))
        );
        assert!(tokens.iter().any(|token| token.kind == TokenKind::If));
    }

    #[test]
    fn rejects_unicode_escaped_non_identifier_start() {
        let error = tokenize(r"\u0030name", &RuntimeLimits::default())
            .expect_err("an identifier cannot start with a digit");
        assert_eq!(error.kind(), crate::js::JsErrorKind::Syntax);
    }

    #[test]
    fn tokenizes_bitwise_shift_and_compound_operators() {
        let tokens = tokenize(
            "mask &= 3; mask |= 4; mask ^= 1; mask <<= 2; mask >>= 1; mask >>>= 1; ~mask;",
            &RuntimeLimits::default(),
        )
        .expect("bitwise operators should tokenize");
        for expected in [
            TokenKind::AmpersandEqual,
            TokenKind::PipeEqual,
            TokenKind::CaretEqual,
            TokenKind::LeftShiftEqual,
            TokenKind::RightShiftEqual,
            TokenKind::UnsignedRightShiftEqual,
            TokenKind::Tilde,
        ] {
            assert!(tokens.iter().any(|token| token.kind == expected));
        }
    }
}
