use super::lexer::{Token, TokenKind};
use super::{JsError, JsErrorKind, RuntimeLimits};
use crate::js::JsValue;

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Statement {
    Variable { name: String, value: Option<Expr> },
    Expression(Expr),
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Expr {
    Literal(JsValue),
    Identifier(String),
    Member {
        object: Box<Self>,
        property: String,
    },
    Call {
        callee: Box<Self>,
        arguments: Vec<Self>,
    },
    Assignment {
        target: Box<Self>,
        value: Box<Self>,
    },
}

pub(super) fn parse(tokens: Vec<Token>, limits: &RuntimeLimits) -> Result<Vec<Statement>, JsError> {
    Parser {
        tokens,
        cursor: 0,
        max_statements: limits.max_statements,
    }
    .program()
}

struct Parser {
    tokens: Vec<Token>,
    cursor: usize,
    max_statements: usize,
}

impl Parser {
    fn program(mut self) -> Result<Vec<Statement>, JsError> {
        let mut statements = Vec::new();
        while !self.at(&TokenKind::Eof) {
            if statements.len() >= self.max_statements {
                return Err(JsError::new(
                    JsErrorKind::ResourceLimit,
                    format!("script exceeds the {} statement limit", self.max_statements),
                    Some(self.current().offset),
                ));
            }
            statements.push(self.statement()?);
        }
        Ok(statements)
    }

    fn statement(&mut self) -> Result<Statement, JsError> {
        if self.take(&TokenKind::Let) || self.take(&TokenKind::Const) || self.take(&TokenKind::Var)
        {
            let TokenKind::Identifier(name) = self.advance().kind else {
                return Err(self.error("expected an identifier after declaration keyword"));
            };
            let value = if self.take(&TokenKind::Equal) {
                Some(self.assignment()?)
            } else {
                None
            };
            self.end_statement()?;
            Ok(Statement::Variable { name, value })
        } else {
            let expression = self.assignment()?;
            self.end_statement()?;
            Ok(Statement::Expression(expression))
        }
    }

    fn assignment(&mut self) -> Result<Expr, JsError> {
        let target = self.postfix()?;
        if self.take(&TokenKind::Equal) {
            if !matches!(target, Expr::Identifier(_) | Expr::Member { .. }) {
                return Err(self.error("invalid assignment target"));
            }
            let value = self.assignment()?;
            Ok(Expr::Assignment {
                target: Box::new(target),
                value: Box::new(value),
            })
        } else {
            Ok(target)
        }
    }

    fn postfix(&mut self) -> Result<Expr, JsError> {
        let mut expression = self.primary()?;
        loop {
            if self.take(&TokenKind::Dot) {
                let TokenKind::Identifier(property) = self.advance().kind else {
                    return Err(self.error("expected a property name after '.'"));
                };
                expression = Expr::Member {
                    object: Box::new(expression),
                    property,
                };
            } else if self.take(&TokenKind::LeftParen) {
                let mut arguments = Vec::new();
                if !self.at(&TokenKind::RightParen) {
                    loop {
                        arguments.push(self.assignment()?);
                        if !self.take(&TokenKind::Comma) {
                            break;
                        }
                    }
                }
                self.require(&TokenKind::RightParen, "expected ')' after arguments")?;
                expression = Expr::Call {
                    callee: Box::new(expression),
                    arguments,
                };
            } else {
                break;
            }
        }
        Ok(expression)
    }

    fn primary(&mut self) -> Result<Expr, JsError> {
        let token = self.advance();
        match token.kind {
            TokenKind::Identifier(name) => Ok(Expr::Identifier(name)),
            TokenKind::String(value) => Ok(Expr::Literal(JsValue::String(value))),
            TokenKind::Number(value) => Ok(Expr::Literal(JsValue::Number(value))),
            TokenKind::True => Ok(Expr::Literal(JsValue::Boolean(true))),
            TokenKind::False => Ok(Expr::Literal(JsValue::Boolean(false))),
            TokenKind::Null => Ok(Expr::Literal(JsValue::Null)),
            TokenKind::Undefined => Ok(Expr::Literal(JsValue::Undefined)),
            TokenKind::LeftParen => {
                let expression = self.assignment()?;
                self.require(&TokenKind::RightParen, "expected ')' after expression")?;
                Ok(expression)
            }
            _ => Err(JsError::syntax("expected an expression", token.offset)),
        }
    }

    fn end_statement(&mut self) -> Result<(), JsError> {
        if self.take(&TokenKind::Semicolon) || self.at(&TokenKind::Eof) {
            Ok(())
        } else {
            Err(self.error("expected ';' between statements"))
        }
    }

    fn require(&mut self, expected: &TokenKind, message: &str) -> Result<(), JsError> {
        if self.take(expected) {
            Ok(())
        } else {
            Err(self.error(message))
        }
    }

    fn take(&mut self, expected: &TokenKind) -> bool {
        if self.at(expected) {
            self.cursor = self.cursor.saturating_add(1);
            true
        } else {
            false
        }
    }

    fn at(&self, expected: &TokenKind) -> bool {
        std::mem::discriminant(&self.current().kind) == std::mem::discriminant(expected)
    }

    fn advance(&mut self) -> Token {
        let token = self.current().clone();
        if !matches!(token.kind, TokenKind::Eof) {
            self.cursor = self.cursor.saturating_add(1);
        }
        token
    }

    fn current(&self) -> &Token {
        self.tokens
            .get(self.cursor)
            .unwrap_or_else(|| self.tokens.last().expect("lexer always emits EOF"))
    }

    fn error(&self, message: &str) -> JsError {
        JsError::syntax(message, self.current().offset)
    }
}
