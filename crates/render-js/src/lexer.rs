use super::{JsError, JsErrorKind, RuntimeLimits};

#[derive(Clone, Debug, PartialEq)]
pub(super) enum TokenKind {
    Identifier(String),
    /// `#name`, the private-name form used by class fields and methods.
    PrivateName(String),
    String(String),
    Number(f64),
    RegexLiteral {
        pattern: String,
        flags: String,
    },
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
    StarStarEqual,
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
    AndAndEqual,
    Pipe,
    PipeEqual,
    OrOr,
    OrOrEqual,
    Caret,
    CaretEqual,
    Tilde,
    Question,
    QuestionQuestion,
    /// `?.` (not when a digit follows: `a?.5:1` is a conditional).
    QuestionDot,
    QuestionQuestionEqual,
    Arrow,
    Eof,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum TemplatePart {
    /// One template character chunk. `cooked` is the value an untagged template
    /// literal contributes, with escapes resolved; `raw` is the unprocessed
    /// source text of the same chunk, which is what a template object's `raw`
    /// property and therefore `String.raw` hand to a tag function.
    Quasi {
        cooked: String,
        raw: String,
    },
    Expression(String),
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Token {
    pub kind: TokenKind,
    pub offset: usize,
    /// Whether a line terminator (or a comment containing one) precedes this
    /// token. Restricted productions such as `return` need this for correct
    /// automatic semicolon insertion.
    pub after_newline: bool,
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
        brace_stack: Vec::new(),
        last_block_close: true,
        newline: false,
    }
    .run()
}

struct Lexer<'a> {
    source: &'a str,
    offset: usize,
    tokens: Vec<Token>,
    max_tokens: usize,
    /// Whether each open `{` likely starts a block (statement, function, or
    /// control-flow body) instead of an object literal. Decides whether the
    /// matching `}` may be followed by a regex literal.
    brace_stack: Vec<bool>,
    /// Whether the most recently closed `{}` was a block.
    last_block_close: bool,
    /// Whether a line terminator was seen since the previous token.
    newline: bool,
}

impl Lexer<'_> {
    #[allow(clippy::too_many_lines)]
    fn run(mut self) -> Result<Vec<Token>, JsError> {
        while let Some(character) = self.peek() {
            // ECMA-262 WhiteSpace includes <ZWNBSP> (U+FEFF); source files
            // saved with a UTF-8 byte-order mark must still tokenize.
            if character.is_ascii_whitespace() || character == '\u{feff}' {
                if matches!(character, '\n' | '\r') {
                    self.newline = true;
                }
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
                '{' => {
                    self.advance();
                    self.enter_brace();
                    TokenKind::LeftBrace
                }
                '}' => {
                    self.advance();
                    self.exit_brace();
                    TokenKind::RightBrace
                }
                '[' => self.single(TokenKind::LeftBracket),
                ']' => self.single(TokenKind::RightBracket),
                ':' => self.single(TokenKind::Colon),
                '?' if self.peek_second() == Some('?') => {
                    self.advance();
                    self.advance();
                    if self.peek() == Some('=') {
                        self.advance();
                        TokenKind::QuestionQuestionEqual
                    } else {
                        TokenKind::QuestionQuestion
                    }
                }
                '?' if self.peek_second() == Some('.')
                    && !self.peek_third().is_some_and(|next| next.is_ascii_digit()) =>
                {
                    self.advance();
                    self.advance();
                    TokenKind::QuestionDot
                }
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
                '*' if self.peek_second() == Some('*') && self.peek_third() == Some('=') => {
                    self.advance();
                    self.advance();
                    self.advance();
                    TokenKind::StarStarEqual
                }
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
                    if self.peek() == Some('=') {
                        self.advance();
                        TokenKind::AndAndEqual
                    } else {
                        TokenKind::AndAnd
                    }
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
                    if self.peek() == Some('=') {
                        self.advance();
                        TokenKind::OrOrEqual
                    } else {
                        TokenKind::OrOr
                    }
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
                '#' => self.private_name(start)?,
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
            Some(TokenKind::RightBrace) => self.last_block_close,
            // A regexp may follow the closing parenthesis of a control
            // statement (`if (x) /re/.test(x)`).  Treating every `)` as an
            // expression value misclassifies the slash as division and leaves
            // the regexp escape (for example `\w`) as an unsupported token.
            Some(TokenKind::RightParen) if self.paren_closes_control_head() => true,
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
                    | TokenKind::PlusPlus
                    | TokenKind::MinusMinus
            ),
        }
    }

    /// Return whether the most recent `)` closes the condition of a control
    /// statement rather than a function/method call.  This is the same
    /// backwards matching used by `paren_opens_block`, but is needed at the
    /// token immediately after the right parenthesis.
    fn paren_closes_control_head(&self) -> bool {
        let mut depth = 0_usize;
        for (reverse_index, token) in self.tokens.iter().rev().enumerate().skip(1) {
            match token.kind {
                TokenKind::RightParen => depth += 1,
                TokenKind::LeftParen => {
                    if depth == 0 {
                        let before = self
                            .tokens
                            .iter()
                            .rev()
                            .nth(reverse_index + 1)
                            .map(|token| &token.kind);
                        return matches!(
                            before,
                            Some(
                                TokenKind::If
                                    | TokenKind::While
                                    | TokenKind::For
                                    | TokenKind::Switch
                                    | TokenKind::Catch
                            )
                        );
                    }
                    depth -= 1;
                }
                _ => {}
            }
        }
        false
    }

    /// Record one `{` and classify it as a block or object literal opener.
    fn enter_brace(&mut self) {
        let is_block = self.brace_opens_block();
        self.brace_stack.push(is_block);
    }

    /// Record one `}` and remember whether its `{` opened a block, which
    /// controls whether a following `/` starts a regex literal (e.g. the
    /// statement after `return}` in a function body) or division (e.g. the
    /// numerator after `{a:1}`).
    fn exit_brace(&mut self) {
        self.last_block_close = self.brace_stack.pop().unwrap_or(true);
    }

    /// Classify a `{` from the token immediately before it. Value positions
    /// (`return {a:1}`, `x = {…}`, `[{}]`) open object literals; statement
    /// keywords, separators, arrows, and the start of a script open blocks.
    fn brace_opens_block(&self) -> bool {
        let Some(last) = self.tokens.last() else {
            return true;
        };
        match last.kind {
            TokenKind::RightParen => self.paren_opens_block(),
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
            | TokenKind::Return
            | TokenKind::Typeof
            | TokenKind::New
            | TokenKind::In
            | TokenKind::Instanceof
            | TokenKind::Delete
            | TokenKind::Void
            | TokenKind::Throw
            | TokenKind::Dot
            | TokenKind::Comma
            | TokenKind::Question
            | TokenKind::Colon
            | TokenKind::Equal
            | TokenKind::Plus
            | TokenKind::PlusEqual
            | TokenKind::Minus
            | TokenKind::MinusEqual
            | TokenKind::Star
            | TokenKind::StarEqual
            | TokenKind::StarStar
            | TokenKind::StarStarEqual
            | TokenKind::Slash
            | TokenKind::SlashEqual
            | TokenKind::Percent
            | TokenKind::PercentEqual
            | TokenKind::Bang
            | TokenKind::Less
            | TokenKind::LessEqual
            | TokenKind::Greater
            | TokenKind::GreaterEqual
            | TokenKind::LeftShift
            | TokenKind::LeftShiftEqual
            | TokenKind::RightShift
            | TokenKind::RightShiftEqual
            | TokenKind::UnsignedRightShift
            | TokenKind::UnsignedRightShiftEqual
            | TokenKind::Ampersand
            | TokenKind::AmpersandEqual
            | TokenKind::AndAnd
            | TokenKind::AndAndEqual
            | TokenKind::Pipe
            | TokenKind::PipeEqual
            | TokenKind::OrOr
            | TokenKind::OrOrEqual
            | TokenKind::Caret
            | TokenKind::CaretEqual
            | TokenKind::Tilde
            | TokenKind::QuestionQuestion
            | TokenKind::QuestionQuestionEqual
            | TokenKind::RightBracket
            | TokenKind::RightBrace
            | TokenKind::LeftParen
            | TokenKind::LeftBracket
            | TokenKind::PlusPlus
            | TokenKind::MinusMinus => false,
            // Statement keywords, separators, arrows, and braces begin
            // block-shaped constructs.
            _ => true,
        }
    }

    /// Disambiguate `) {`: it closes a function or control-flow head when the
    /// token before the matching `(` is such a keyword, and starts an object
    /// literal otherwise (e.g. `(cond) ? {a:1} : {b:2}` or `(value) {}`).
    fn paren_opens_block(&self) -> bool {
        let mut depth = 0_usize;
        for (reverse_index, token) in self.tokens.iter().rev().enumerate().skip(1) {
            match token.kind {
                TokenKind::RightParen => depth += 1,
                TokenKind::LeftParen => {
                    if depth == 0 {
                        let before = self
                            .tokens
                            .iter()
                            .rev()
                            .nth(reverse_index + 1)
                            .map(|token| token.kind.clone());
                        return matches!(
                            before,
                            Some(
                                TokenKind::Function
                                    | TokenKind::Catch
                                    | TokenKind::If
                                    | TokenKind::While
                                    | TokenKind::For
                                    | TokenKind::Switch
                            )
                        );
                    }
                    depth -= 1;
                }
                _ => {}
            }
        }
        false
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

    /// Scan `#` followed by an identifier: a private name. The `#` itself is
    /// not part of the name; the class evaluator scopes names per class body.
    fn private_name(&mut self, start: usize) -> Result<TokenKind, JsError> {
        self.advance();
        let first = self.identifier_character(start)?;
        if !is_identifier_start(first) {
            return Err(JsError::syntax("expected a private name after '#'", start));
        }
        let mut name = String::from(first);
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
                return Err(JsError::syntax("invalid private name character", start));
            }
            name.push(character);
        }
        Ok(TokenKind::PrivateName(name))
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
                let digits = self.digit_run(|value| value.is_digit(radix));
                if digits == 0 || self.peek().is_some_and(is_identifier_continue) {
                    return Err(JsError::syntax("invalid numeric literal", start));
                }
                let text: String = self.source[digits_start..self.offset]
                    .chars()
                    .filter(|value| *value != '_')
                    .collect();
                return Ok(TokenKind::Number(radix_digits_to_number(&text, radix)));
            }
        }
        // A separator cannot follow a leading zero (`0_1` is not a literal).
        if self.peek() == Some('0') && self.peek_second() == Some('_') {
            return Err(JsError::syntax("invalid numeric literal", start));
        }
        self.digit_run(|value| value.is_ascii_digit());
        if self.peek() == Some('.') {
            self.advance();
            self.digit_run(|value| value.is_ascii_digit());
        }
        if self.peek().is_some_and(|value| matches!(value, 'e' | 'E')) {
            self.advance();
            if self.peek().is_some_and(|value| matches!(value, '+' | '-')) {
                self.advance();
            }
            if self.digit_run(|value| value.is_ascii_digit()) == 0 {
                return Err(JsError::syntax("invalid numeric literal", start));
            }
        }
        if self.peek().is_some_and(is_identifier_start) {
            return Err(JsError::syntax("invalid numeric literal", start));
        }
        // NumericLiteralSeparator (ECMA-262 12.8.6): the underscores are not part
        // of the value.
        let text: String = self.source[start..self.offset]
            .chars()
            .filter(|value| *value != '_')
            .collect();
        text.parse::<f64>()
            .map(TokenKind::Number)
            .map_err(|_| JsError::syntax("invalid numeric literal", start))
    }

    /// Consumes one run of digits, with `_` separators allowed only between two
    /// digits (ECMA-262 12.8.6). Returns how many digits were consumed.
    fn digit_run(&mut self, is_digit: impl Fn(char) -> bool) -> usize {
        let mut count = 0;
        loop {
            match self.peek() {
                Some(value) if is_digit(value) => {
                    self.advance();
                    count += 1;
                }
                Some('_') if count > 0 && self.peek_second().is_some_and(&is_digit) => {
                    self.advance();
                }
                _ => return count,
            }
        }
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
            // Look ahead at the four hex digits without consuming them, so a
            // high surrogate that is *not* followed by a trailing one stays a
            // lone surrogate instead of being replaced. §12.9.4.1 says a
            // `SurrogatePair` is only formed when a leading surrogate is
            // followed by `TrailingSurrogate`, and `'\uD83D\uD83D'` is two lone
            // surrogates of length 2, not one replacement character.
            let pending = self.offset;
            self.advance();
            self.advance();
            let low = self.hex_escape_value(4)?;
            if (0xdc00..=0xdfff).contains(&low) {
                let scalar = 0x1_0000 + ((high - 0xd800) << 10) + (low - 0xdc00);
                return char::from_u32(scalar)
                    .ok_or_else(|| JsError::syntax("invalid Unicode surrogate pair", start));
            }
            self.offset = pending;
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
        // Byte offset where the current chunk's source text begins, so its raw
        // value can be sliced out verbatim once the chunk's end is reached.
        let mut quasi_start = self.offset;
        loop {
            let Some(character) = self.peek() else {
                return Err(JsError::syntax("unterminated template literal", start));
            };
            self.advance();
            match character {
                '`' => {
                    parts.push(TemplatePart::Quasi {
                        cooked: text,
                        raw: self.template_raw_value(quasi_start, self.offset - 1),
                    });
                    return Ok(TokenKind::Template(parts));
                }
                '$' if self.peek() == Some('{') => {
                    // The quasi's raw value ends *before* the `${`, which is
                    // why the slice is taken while the offset still points just
                    // past the `$`.
                    let raw = self.template_raw_value(quasi_start, self.offset - 1);
                    self.advance();
                    parts.push(TemplatePart::Quasi {
                        cooked: std::mem::take(&mut text),
                        raw,
                    });
                    parts.push(TemplatePart::Expression(
                        self.template_interpolation(start)?,
                    ));
                    quasi_start = self.offset;
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

    /// ECMA-262 12.9.6 `TRV`: the raw value of a template character sequence,
    /// with `CR` and `CR LF` folded to `LF` so the raw text agrees with the
    /// cooked one. Slicing the source rather than re-accumulating characters is
    /// what keeps `\\n` distinguishable from a line continuation in `raw`.
    fn template_raw_value(&self, from: usize, to: usize) -> String {
        self.source[from..to]
            .replace("\r\n", "\n")
            .replace('\r', "\n")
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
            if matches!(character, '\n' | '\r') {
                self.newline = true;
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
        self.tokens.push(Token {
            kind,
            offset,
            after_newline: std::mem::take(&mut self.newline),
        });
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

    fn peek_third(&self) -> Option<char> {
        self.source[self.offset..].chars().nth(2)
    }

    fn advance(&mut self) {
        if let Some(character) = self.peek() {
            self.offset += character.len_utf8();
        }
    }
}

/// ECMAScript accepts arbitrarily long binary, octal and hexadecimal integer
/// literals. Keep the most significant 53 bits and use the remaining bits to
/// round to the nearest binary64 value (ties to even).
#[allow(
    clippy::cast_precision_loss,
    reason = "top contains at most 53 significant bits, exactly representable as binary64"
)]
fn radix_digits_to_number(digits: &str, radix: u32) -> f64 {
    let bits_per_digit = radix.trailing_zeros() as usize;
    let Some(first_nonzero) = digits.find(|digit: char| digit != '0') else {
        return 0.0;
    };
    let significant = &digits[first_nonzero..];
    let first = significant
        .chars()
        .next()
        .and_then(|digit| digit.to_digit(radix))
        .expect("lexer already validated radix digits");
    let first_bits = (u32::BITS - first.leading_zeros()) as usize;
    let bit_len = first_bits + (significant.len() - 1) * bits_per_digit;
    if bit_len > 1024 {
        return f64::INFINITY;
    }

    let mut top = 0_u64;
    let mut guard = false;
    let mut sticky = false;
    let mut bit_index = 0;
    for (index, digit) in significant.chars().enumerate() {
        let value = digit
            .to_digit(radix)
            .expect("lexer already validated radix digits");
        let width = if index == 0 {
            first_bits
        } else {
            bits_per_digit
        };
        for shift in (0..width).rev() {
            let bit = (value >> shift) & 1 != 0;
            match bit_index.cmp(&53) {
                std::cmp::Ordering::Less => top = (top << 1) | u64::from(bit),
                std::cmp::Ordering::Equal => guard = bit,
                std::cmp::Ordering::Greater => sticky |= bit,
            }
            bit_index += 1;
        }
    }

    if bit_len <= 53 {
        return top as f64;
    }
    if guard && (sticky || top & 1 != 0) {
        top += 1;
    }
    let exponent = i32::try_from(bit_len - 53).expect("bounded by binary64 exponent range");
    (top as f64) * 2_f64.powi(exponent)
}

fn is_identifier_start(character: char) -> bool {
    // ECMAScript IdentifierStart is UnicodeIDStart plus `$` and `_`.
    unicode_ident::is_xid_start(character) || matches!(character, '_' | '$')
}

pub(super) fn surrogate_placeholder(value: u32) -> char {
    // Rust strings contain Unicode scalar values while ECMAScript strings are
    // UTF-16 code-unit sequences. Reserve a private-use range for unpaired
    // surrogates so regexes such as /[\uD800-\uDFFF]/ keep their meaning.
    char::from_u32(0xf_0000 + value.saturating_sub(0xd800)).unwrap_or('\u{fffd}')
}

fn is_identifier_continue(character: char) -> bool {
    unicode_ident::is_xid_continue(character)
        || matches!(character, '_' | '$' | '\u{200c}' | '\u{200d}')
}

#[cfg(test)]
mod tests {
    use super::{TokenKind, tokenize};
    use crate::RuntimeLimits;

    #[test]
    fn accepts_large_radix_literals_and_rounds_ties_to_even() {
        for (source, expected) in [
            ("0x10000000000000000", 18_446_744_073_709_551_616.0),
            ("0x20000000000001", 9_007_199_254_740_992.0),
            ("0x20000000000003", 9_007_199_254_740_996.0),
            (
                "0b100000000000000000000000000000000000000000000000000000",
                9_007_199_254_740_992.0,
            ),
            ("0o1000000000000000000000", 9_223_372_036_854_775_808.0),
        ] {
            let tokens = tokenize(source, &RuntimeLimits::default()).expect(source);
            assert_eq!(tokens[0].kind, TokenKind::Number(expected), "{source}");
        }
        let huge = format!("0x{}", "f".repeat(500));
        let tokens = tokenize(&huge, &RuntimeLimits::default()).expect("large hex literal");
        assert_eq!(tokens[0].kind, TokenKind::Number(f64::INFINITY));
    }

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
        assert_eq!(error.kind(), crate::JsErrorKind::Syntax);
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

    #[test]
    fn tokenizes_regex_after_control_condition() {
        let tokens = tokenize(
            r"if (value) /^on\w+/.test(value); if (value) /[\\/]/.test(value);",
            &RuntimeLimits::default(),
        )
        .expect("a regexp after an if condition must not be parsed as division");
        assert_eq!(
            tokens
                .iter()
                .filter(|token| matches!(token.kind, TokenKind::RegexLiteral { .. }))
                .count(),
            2
        );
        tokenize(
            r#"if("function"==typeof t[c])/^on\w+/.test(c)?fn():other();"#,
            &RuntimeLimits::default(),
        )
        .expect("regex in a minified if condition should tokenize");
    }
}
