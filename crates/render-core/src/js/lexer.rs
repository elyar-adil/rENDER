use super::{JsError, JsErrorKind, RuntimeLimits};

#[derive(Clone, Debug, PartialEq)]
pub(super) enum TokenKind {
    Identifier(String),
    String(String),
    Number(f64),
    RegexLiteral { pattern: String, flags: String },
    Template(Vec<TemplatePart>),
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
    Do,
    For,
    Break,
    Continue,
    Delete,
    Typeof,
    Void,
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
    Ellipsis,
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
    StarStar,
    StarEqual,
    Slash,
    SlashEqual,
    Percent,
    PercentEqual,
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
pub(super) enum TemplatePart {
    String(String),
    Expression(String),
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
                '.' if self.source[self.offset..].starts_with("...") => {
                    self.advance();
                    self.advance();
                    self.advance();
                    TokenKind::Ellipsis
                }
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
                '*' if self.peek_second() == Some('*') => {
                    self.advance();
                    self.advance();
                    TokenKind::StarStar
                }
                '*' if self.peek_second() == Some('=') => {
                    self.advance();
                    self.advance();
                    TokenKind::StarEqual
                }
                '*' => self.single(TokenKind::Star),
                '/' if self.peek_second() == Some('=') && !self.regex_allowed() => {
                    self.advance();
                    self.advance();
                    TokenKind::SlashEqual
                }
                '%' if self.peek_second() == Some('=') => {
                    self.advance();
                    self.advance();
                    TokenKind::PercentEqual
                }
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
                '/' if self.regex_allowed() => self.regex_literal(start)?,
                '/' => self.single(TokenKind::Slash),
                '\'' | '"' => self.string(character)?,
                '`' => self.template()?,
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

    /// Decide whether the `/` at the cursor opens a regex literal instead of a
    /// division operator, using the standard "previous token" heuristic.
    fn regex_allowed(&self) -> bool {
        match self.tokens.last().map(|token| &token.kind) {
            None => true,
            Some(kind) => !matches!(
                kind,
                TokenKind::Identifier(_)
                    | TokenKind::String(_)
                    | TokenKind::Number(_)
                    | TokenKind::RegexLiteral { .. }
                    | TokenKind::Template(_)
                    | TokenKind::True
                    | TokenKind::False
                    | TokenKind::Null
                    | TokenKind::Undefined
                    | TokenKind::This
                    | TokenKind::RightParen
                    | TokenKind::RightBracket
                    | TokenKind::RightBrace
                    | TokenKind::PlusPlus
                    | TokenKind::MinusMinus
            ),
        }
    }

    /// Scan a regex literal body plus flags. `/` inside a character class
    /// never terminates the literal, per the ECMAScript lexical grammar.
    fn regex_literal(&mut self, start: usize) -> Result<TokenKind, JsError> {
        self.advance();
        let mut pattern = String::new();
        let mut in_class = false;
        loop {
            let Some(character) = self.peek() else {
                return Err(JsError::syntax("unterminated regex literal", start));
            };
            if matches!(character, '\n' | '\r') {
                return Err(JsError::syntax("newline in regex literal", self.offset));
            }
            self.advance();
            match character {
                '\\' => {
                    let Some(escaped) = self.peek() else {
                        return Err(JsError::syntax("unterminated regex escape", self.offset));
                    };
                    if matches!(escaped, '\n' | '\r') {
                        return Err(JsError::syntax("newline in regex literal", self.offset));
                    }
                    self.advance();
                    pattern.push('\\');
                    pattern.push(escaped);
                }
                '[' => {
                    in_class = true;
                    pattern.push(character);
                }
                ']' => {
                    in_class = false;
                    pattern.push(character);
                }
                '/' if !in_class => break,
                other => pattern.push(other),
            }
        }
        let mut flags = String::new();
        while let Some(character) = self.peek()
            && character.is_ascii_alphabetic()
        {
            self.advance();
            if flags.contains(character) {
                return Err(JsError::syntax(
                    format!("duplicate regex flag {character:?}"),
                    start,
                ));
            }
            flags.push(character);
        }
        if self.peek().is_some_and(is_identifier_start) {
            return Err(JsError::syntax("invalid regex flag", start));
        }
        Ok(TokenKind::RegexLiteral { pattern, flags })
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
            "do" => TokenKind::Do,
            "for" => TokenKind::For,
            "break" => TokenKind::Break,
            "continue" => TokenKind::Continue,
            "delete" => TokenKind::Delete,
            "typeof" => TokenKind::Typeof,
            "void" => TokenKind::Void,
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
                    'u' => self.unicode_escape()?,
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

    fn unicode_escape(&mut self) -> Result<char, JsError> {
        if self.peek() == Some('{') {
            let start = self.offset;
            self.advance();
            let digits_start = self.offset;
            while self.peek().is_some_and(|value| value.is_ascii_hexdigit()) {
                self.advance();
            }
            if self.offset == digits_start || self.peek() != Some('}') {
                return Err(JsError::syntax("invalid Unicode escape", start));
            }
            let value = u32::from_str_radix(&self.source[digits_start..self.offset], 16)
                .map_err(|_| JsError::syntax("invalid Unicode escape", start))?;
            self.advance();
            return char::from_u32(value)
                .ok_or_else(|| JsError::syntax("invalid Unicode escape", start));
        }

        let start = self.offset;
        let high = self.hex_escape_value(4)?;
        if (0xd800..=0xdbff).contains(&high)
            && self.peek() == Some('\\')
            && self.peek_second() == Some('u')
        {
            self.advance();
            self.advance();
            let low = self.hex_escape_value(4)?;
            if (0xdc00..=0xdfff).contains(&low) {
                let scalar = 0x1_0000 + ((high - 0xd800) << 10) + (low - 0xdc00);
                return char::from_u32(scalar)
                    .ok_or_else(|| JsError::syntax("invalid Unicode surrogate pair", start));
            }
            return Ok('\u{fffd}');
        }
        Ok(char::from_u32(high).unwrap_or_else(|| surrogate_placeholder(high)))
    }

    fn hex_escape_value(&mut self, digits: usize) -> Result<u32, JsError> {
        let start = self.offset;
        for _ in 0..digits {
            if !self.peek().is_some_and(|value| value.is_ascii_hexdigit()) {
                return Err(JsError::syntax("invalid hexadecimal escape", start));
            }
            self.advance();
        }
        u32::from_str_radix(&self.source[start..self.offset], 16)
            .map_err(|_| JsError::syntax("invalid hexadecimal escape", start))
    }

    fn template(&mut self) -> Result<TokenKind, JsError> {
        let start = self.offset;
        self.advance();
        let mut parts = Vec::new();
        let mut text = String::new();
        loop {
            let Some(character) = self.peek() else {
                return Err(JsError::syntax("unterminated template literal", start));
            };
            self.advance();
            match character {
                '`' => {
                    parts.push(TemplatePart::String(text));
                    return Ok(TokenKind::Template(parts));
                }
                '$' if self.peek() == Some('{') => {
                    self.advance();
                    parts.push(TemplatePart::String(std::mem::take(&mut text)));
                    parts.push(TemplatePart::Expression(
                        self.template_interpolation(start)?,
                    ));
                }
                '\\' => {
                    let escaped = self.peek().ok_or_else(|| {
                        JsError::syntax("unterminated template escape", self.offset)
                    })?;
                    self.advance();
                    let escaped = match escaped {
                        '\n' => continue,
                        '\r' => {
                            if self.peek() == Some('\n') {
                                self.advance();
                            }
                            continue;
                        }
                        '0' if !self.peek().is_some_and(|value| value.is_ascii_digit()) => '\0',
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        'b' => '\u{0008}',
                        'f' => '\u{000c}',
                        'v' => '\u{000b}',
                        'x' => self.hex_escape(2)?,
                        'u' => self.unicode_escape()?,
                        other => other,
                    };
                    text.push(escaped);
                }
                other => text.push(other),
            }
        }
    }

    fn template_interpolation(&mut self, template_start: usize) -> Result<String, JsError> {
        let expression_start = self.offset;
        let mut depth = 1_u32;
        let mut quote = None;
        while let Some(character) = self.peek() {
            if let Some(delimiter) = quote {
                self.advance();
                if character == '\\' {
                    if self.peek().is_some() {
                        self.advance();
                    }
                } else if character == delimiter {
                    quote = None;
                }
                continue;
            }
            match character {
                '\'' | '"' | '`' => {
                    quote = Some(character);
                    self.advance();
                }
                '{' => {
                    depth = depth.saturating_add(1);
                    self.advance();
                }
                '}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        let expression = self.source[expression_start..self.offset].to_owned();
                        self.advance();
                        return Ok(expression);
                    }
                    self.advance();
                }
                _ => self.advance(),
            }
        }
        Err(JsError::syntax(
            "unterminated template interpolation",
            template_start,
        ))
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

pub(super) fn surrogate_placeholder(value: u32) -> char {
    // Rust strings contain Unicode scalar values while ECMAScript strings are
    // UTF-16 code-unit sequences. Reserve a private-use range for unpaired
    // surrogates so regexes such as /[\uD800-\uDFFF]/ keep their meaning.
    char::from_u32(0xf_0000 + value.saturating_sub(0xd800)).unwrap_or('\u{fffd}')
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
