use super::{JsError, JsErrorKind, RuntimeLimits};

#[derive(Clone, Debug, PartialEq)]
pub(super) enum TokenKind {
    Identifier(String),
    String(String),
    Number(f64),
    Let,
    Const,
    Var,
    True,
    False,
    Null,
    Undefined,
    Dot,
    Comma,
    Semicolon,
    Equal,
    LeftParen,
    RightParen,
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
    fn run(mut self) -> Result<Vec<Token>, JsError> {
        while let Some(character) = self.peek() {
            if character.is_ascii_whitespace() {
                self.advance();
                continue;
            }
            let start = self.offset;
            let kind = match character {
                '.' => self.single(TokenKind::Dot),
                ',' => self.single(TokenKind::Comma),
                ';' => self.single(TokenKind::Semicolon),
                '=' => self.single(TokenKind::Equal),
                '(' => self.single(TokenKind::LeftParen),
                ')' => self.single(TokenKind::RightParen),
                '\'' | '"' => self.string(character)?,
                '0'..='9' => self.number()?,
                value if is_identifier_start(value) => self.identifier(),
                '/' if self.peek_second() == Some('/') => {
                    self.line_comment();
                    continue;
                }
                '/' if self.peek_second() == Some('*') => {
                    self.block_comment(start)?;
                    continue;
                }
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

    fn identifier(&mut self) -> TokenKind {
        let start = self.offset;
        self.advance();
        while self.peek().is_some_and(is_identifier_continue) {
            self.advance();
        }
        match &self.source[start..self.offset] {
            "let" => TokenKind::Let,
            "const" => TokenKind::Const,
            "var" => TokenKind::Var,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            "null" => TokenKind::Null,
            "undefined" => TokenKind::Undefined,
            identifier => TokenKind::Identifier((*identifier).to_owned()),
        }
    }

    fn number(&mut self) -> Result<TokenKind, JsError> {
        let start = self.offset;
        while self.peek().is_some_and(|value| value.is_ascii_digit()) {
            self.advance();
        }
        if self.peek() == Some('.')
            && self
                .peek_second()
                .is_some_and(|value| value.is_ascii_digit())
        {
            self.advance();
            while self.peek().is_some_and(|value| value.is_ascii_digit()) {
                self.advance();
            }
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
                value.push(match escaped {
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    '\\' => '\\',
                    '\'' => '\'',
                    '"' => '"',
                    _ => {
                        return Err(JsError::syntax(
                            format!("unsupported string escape \\{escaped}"),
                            self.offset,
                        ));
                    }
                });
            } else if matches!(character, '\n' | '\r') {
                return Err(JsError::syntax("newline in string literal", self.offset));
            } else {
                value.push(character);
            }
        }
        Err(JsError::syntax("unterminated string literal", start))
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

const fn is_identifier_start(character: char) -> bool {
    character.is_ascii_alphabetic() || matches!(character, '_' | '$')
}

const fn is_identifier_continue(character: char) -> bool {
    is_identifier_start(character) || character.is_ascii_digit()
}

#[cfg(test)]
mod tests {
    use super::{TokenKind, tokenize};
    use crate::js::RuntimeLimits;

    #[test]
    fn tokenizes_member_calls_and_strings() {
        let tokens = tokenize(
            "const node = document.getElementById('message');",
            &RuntimeLimits::default(),
        )
        .expect("supported source should tokenize");
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
}
