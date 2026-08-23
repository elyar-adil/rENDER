use super::lexer::{Token, TokenKind};
use super::{JsError, JsErrorKind, RuntimeLimits};
use crate::js::JsValue;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum VariableKind {
    Let,
    Const,
    Var,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum UnaryOp {
    Not,
    Plus,
    Minus,
    Typeof,
    Delete,
    BitwiseNot,
    Void,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BinaryOp {
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    Instanceof,
    In,
    Equal,
    NotEqual,
    StrictEqual,
    StrictNotEqual,
    LogicalAnd,
    LogicalOr,
    BitwiseAnd,
    BitwiseXor,
    BitwiseOr,
    LeftShift,
    RightShift,
    UnsignedRightShift,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct CatchClause {
    pub parameter: String,
    pub body: Vec<Statement>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Statement {
    Variable {
        kind: VariableKind,
        name: String,
        value: Option<Expr>,
    },
    VariableList {
        kind: VariableKind,
        declarations: Vec<(String, Option<Expr>)>,
    },
    Function {
        name: String,
        parameters: Vec<String>,
        body: Vec<Statement>,
    },
    Return(Option<Expr>),
    Throw(Expr),
    Try {
        body: Vec<Statement>,
        catch: Option<CatchClause>,
        finally: Option<Vec<Statement>>,
    },
    If {
        condition: Expr,
        consequent: Box<Statement>,
        alternate: Option<Box<Statement>>,
    },
    Switch {
        expression: Expr,
        cases: Vec<(Option<Expr>, Vec<Statement>)>,
    },
    While {
        condition: Expr,
        body: Box<Statement>,
    },
    DoWhile {
        condition: Box<Expr>,
        body: Box<Statement>,
    },
    For {
        initializer: Option<Box<Statement>>,
        condition: Option<Expr>,
        update: Option<Expr>,
        body: Box<Statement>,
    },
    ForIn {
        kind: VariableKind,
        name: String,
        iterable: Expr,
        body: Box<Statement>,
    },
    ForInExpr {
        target: Expr,
        iterable: Expr,
        body: Box<Statement>,
    },
    Labeled {
        label: String,
        body: Box<Statement>,
    },
    Break(Option<String>),
    Continue(Option<String>),
    Block(Vec<Statement>),
    Expression(Expr),
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Expr {
    Literal(JsValue),
    RegexLiteral {
        pattern: String,
        flags: String,
    },
    This,
    Identifier(String),
    Function {
        name: Option<String>,
        parameters: Vec<String>,
        body: Vec<Statement>,
    },
    Arrow {
        parameters: Vec<String>,
        body: Vec<Statement>,
    },
    Object(Vec<(String, Self)>),
    Array(Vec<Self>),
    Unary {
        operator: UnaryOp,
        operand: Box<Self>,
    },
    Binary {
        operator: BinaryOp,
        left: Box<Self>,
        right: Box<Self>,
    },
    Conditional {
        condition: Box<Self>,
        consequent: Box<Self>,
        alternate: Box<Self>,
    },
    Update {
        target: Box<Self>,
        operator: BinaryOp,
        prefix: bool,
    },
    Member {
        object: Box<Self>,
        property: String,
    },
    ComputedMember {
        object: Box<Self>,
        property: Box<Self>,
    },
    New {
        constructor: Box<Self>,
        arguments: Vec<Self>,
    },
    Call {
        callee: Box<Self>,
        arguments: Vec<Self>,
    },
    Assignment {
        target: Box<Self>,
        value: Box<Self>,
    },
    CompoundAssignment {
        target: Box<Self>,
        operator: BinaryOp,
        value: Box<Self>,
    },
    Sequence(Vec<Self>),
}

pub(super) fn parse(tokens: Vec<Token>, limits: &RuntimeLimits) -> Result<Vec<Statement>, JsError> {
    Parser {
        tokens,
        cursor: 0,
        statement_count: 0,
        max_statements: limits.max_statements,
        function_depth: 0,
        loop_depth: 0,
        switch_depth: 0,
        no_in: false,
    }
    .program()
}

struct Parser {
    tokens: Vec<Token>,
    cursor: usize,
    statement_count: usize,
    max_statements: usize,
    function_depth: usize,
    loop_depth: usize,
    switch_depth: usize,
    /// While set, `in` is not treated as a binary operator (for-heads).
    no_in: bool,
}

impl Parser {
    fn program(mut self) -> Result<Vec<Statement>, JsError> {
        let statements = self.statement_list(false)?;
        if has_use_strict_directive(&statements) {
            validate_strict_statements(&statements)?;
        }
        Ok(statements)
    }

    fn statement_list(&mut self, until_right_brace: bool) -> Result<Vec<Statement>, JsError> {
        let mut statements = Vec::new();
        while !self.at(&TokenKind::Eof) && (!until_right_brace || !self.at(&TokenKind::RightBrace))
        {
            self.reserve_statement()?;
            statements.push(self.statement()?);
        }
        if until_right_brace && self.at(&TokenKind::Eof) {
            return Err(self.error("unterminated block statement"));
        }
        Ok(statements)
    }

    fn reserve_statement(&mut self) -> Result<(), JsError> {
        if self.statement_count >= self.max_statements {
            return Err(JsError::new(
                JsErrorKind::ResourceLimit,
                format!("script exceeds the {} statement limit", self.max_statements),
                Some(self.current().offset),
            ));
        }
        self.statement_count = self.statement_count.saturating_add(1);
        Ok(())
    }

    fn statement(&mut self) -> Result<Statement, JsError> {
        if self.take(&TokenKind::Semicolon) {
            return Ok(Statement::Block(Vec::new()));
        }
        if self.take(&TokenKind::LeftBrace) {
            let statements = self.statement_list(true)?;
            self.require(&TokenKind::RightBrace, "expected '}' after block")?;
            return Ok(Statement::Block(statements));
        }
        if self.take(&TokenKind::Function) {
            return self.function_declaration();
        }
        if self.take(&TokenKind::If) {
            return self.if_statement();
        }
        if self.take(&TokenKind::Switch) {
            return self.switch_statement();
        }
        if self.take(&TokenKind::While) {
            return self.while_statement();
        }
        if self.take(&TokenKind::Do) {
            return self.do_while_statement();
        }
        if self.take(&TokenKind::For) {
            return self.for_statement();
        }
        if matches!(&self.current().kind, TokenKind::Identifier(_))
            && matches!(
                self.tokens.get(self.cursor + 1).map(|token| &token.kind),
                Some(TokenKind::Colon)
            )
        {
            let TokenKind::Identifier(label) = self.advance().kind else {
                unreachable!("checked above");
            };
            self.advance();
            let body = self.statement()?;
            return Ok(Statement::Labeled {
                label,
                body: Box::new(body),
            });
        }
        if self.take(&TokenKind::Break) {
            let label = self.take_loop_label();
            if self.loop_depth == 0 && self.switch_depth == 0 {
                return Err(self.error("break is only valid inside a loop or switch"));
            }
            self.end_statement();
            return Ok(Statement::Break(label));
        }
        if self.take(&TokenKind::Continue) {
            let label = self.take_loop_label();
            if self.loop_depth == 0 {
                return Err(self.error("continue is only valid inside a loop"));
            }
            self.end_statement();
            return Ok(Statement::Continue(label));
        }
        if self.take(&TokenKind::Try) {
            return self.try_statement();
        }
        if self.take(&TokenKind::Throw) {
            if self.at(&TokenKind::Semicolon) || self.at(&TokenKind::RightBrace) {
                return Err(self.error("throw requires an expression"));
            }
            let value = self.expression()?;
            self.end_statement();
            return Ok(Statement::Throw(value));
        }
        if self.take(&TokenKind::Return) {
            if self.function_depth == 0 {
                return Err(self.error("return is only valid inside a function"));
            }
            let value = if self.at(&TokenKind::Semicolon) || self.at(&TokenKind::RightBrace) {
                None
            } else {
                Some(self.expression()?)
            };
            self.end_statement();
            return Ok(Statement::Return(value));
        }
        if let Some(kind) = self.take_variable_kind() {
            return self.variable_declaration(kind, true);
        }
        let expression = self.expression()?;
        self.end_statement();
        Ok(Statement::Expression(expression))
    }

    /// Consume an identifier in `break`/`continue` label position.
    fn take_loop_label(&mut self) -> Option<String> {
        if let TokenKind::Identifier(name) = &self.current().kind {
            let name = name.clone();
            self.advance();
            Some(name)
        } else {
            None
        }
    }

    /// Comma-separated expression sequence (the comma operator).
    fn expression(&mut self) -> Result<Expr, JsError> {
        let mut expressions = vec![self.assignment()?];
        while self.take(&TokenKind::Comma) {
            expressions.push(self.assignment()?);
        }
        if expressions.len() == 1 {
            Ok(expressions.pop().expect("checked length"))
        } else {
            Ok(Expr::Sequence(expressions))
        }
    }

    fn take_variable_kind(&mut self) -> Option<VariableKind> {
        if self.take(&TokenKind::Let) {
            Some(VariableKind::Let)
        } else if self.take(&TokenKind::Const) {
            Some(VariableKind::Const)
        } else if self.take(&TokenKind::Var) {
            Some(VariableKind::Var)
        } else {
            None
        }
    }

    fn variable_declaration(
        &mut self,
        kind: VariableKind,
        end_statement: bool,
    ) -> Result<Statement, JsError> {
        let mut declarations = Vec::new();
        loop {
            let TokenKind::Identifier(name) = self.advance().kind else {
                return Err(self.error("expected an identifier after declaration keyword"));
            };
            let value = if self.take(&TokenKind::Equal) {
                Some(self.assignment()?)
            } else {
                None
            };
            if kind == VariableKind::Const && value.is_none() {
                return Err(self.error("const declarations require an initializer"));
            }
            declarations.push((name, value));
            if !self.take(&TokenKind::Comma) {
                break;
            }
        }
        if end_statement {
            self.end_statement();
        }
        if declarations.len() == 1 {
            let (name, value) = declarations.pop().expect("one declaration exists");
            Ok(Statement::Variable { kind, name, value })
        } else {
            Ok(Statement::VariableList { kind, declarations })
        }
    }

    fn function_declaration(&mut self) -> Result<Statement, JsError> {
        let TokenKind::Identifier(name) = self.advance().kind else {
            return Err(self.error("expected a function name"));
        };
        let (parameters, body) = self.function_tail()?;
        Ok(Statement::Function {
            name,
            parameters,
            body,
        })
    }

    fn if_statement(&mut self) -> Result<Statement, JsError> {
        self.require(&TokenKind::LeftParen, "expected '(' after if")?;
        let condition = self.expression()?;
        self.require(&TokenKind::RightParen, "expected ')' after if condition")?;
        let consequent = Box::new(self.statement()?);
        let alternate = if self.take(&TokenKind::Else) {
            Some(Box::new(self.statement()?))
        } else {
            None
        };
        Ok(Statement::If {
            condition,
            consequent,
            alternate,
        })
    }

    fn switch_statement(&mut self) -> Result<Statement, JsError> {
        self.require(&TokenKind::LeftParen, "expected '(' after switch")?;
        let expression = self.expression()?;
        self.require(
            &TokenKind::RightParen,
            "expected ')' after switch expression",
        )?;
        self.require(
            &TokenKind::LeftBrace,
            "expected '{' after switch expression",
        )?;
        let mut cases = Vec::new();
        self.switch_depth = self.switch_depth.saturating_add(1);
        while !self.at(&TokenKind::RightBrace) && !self.at(&TokenKind::Eof) {
            let test = if self.take(&TokenKind::Case) {
                let test = self.assignment()?;
                self.require(&TokenKind::Colon, "expected ':' after case expression")?;
                Some(test)
            } else if self.take(&TokenKind::Default) {
                self.require(&TokenKind::Colon, "expected ':' after default")?;
                None
            } else {
                return Err(self.error("expected case or default in switch"));
            };
            let mut consequent = Vec::new();
            while !self.at(&TokenKind::Case)
                && !self.at(&TokenKind::Default)
                && !self.at(&TokenKind::RightBrace)
                && !self.at(&TokenKind::Eof)
            {
                self.reserve_statement()?;
                consequent.push(self.statement()?);
            }
            cases.push((test, consequent));
        }
        self.switch_depth = self.switch_depth.saturating_sub(1);
        self.require(&TokenKind::RightBrace, "expected '}' after switch")?;
        Ok(Statement::Switch { expression, cases })
    }

    fn while_statement(&mut self) -> Result<Statement, JsError> {
        self.require(&TokenKind::LeftParen, "expected '(' after while")?;
        let condition = self.expression()?;
        self.require(&TokenKind::RightParen, "expected ')' after while condition")?;
        self.loop_depth = self.loop_depth.saturating_add(1);
        let body = self.statement();
        self.loop_depth = self.loop_depth.saturating_sub(1);
        Ok(Statement::While {
            condition,
            body: Box::new(body?),
        })
    }

    fn do_while_statement(&mut self) -> Result<Statement, JsError> {
        self.loop_depth = self.loop_depth.saturating_add(1);
        let body = self.statement();
        self.loop_depth = self.loop_depth.saturating_sub(1);
        let body = body?;
        self.require(&TokenKind::While, "expected 'while' after do body")?;
        self.require(&TokenKind::LeftParen, "expected '(' after while")?;
        let condition = self.expression()?;
        self.require(&TokenKind::RightParen, "expected ')' after while condition")?;
        let _ = self.take(&TokenKind::Semicolon);
        Ok(Statement::DoWhile {
            condition: Box::new(condition),
            body: Box::new(body),
        })
    }

    fn for_statement(&mut self) -> Result<Statement, JsError> {
        self.require(&TokenKind::LeftParen, "expected '(' after for")?;
        self.no_in = true;
        let initializer = if self.take(&TokenKind::Semicolon) {
            None
        } else if let Some(kind) = self.take_variable_kind() {
            let declaration_start = self.cursor;
            let TokenKind::Identifier(name) = self.advance().kind else {
                return Err(self.error("expected an identifier after declaration keyword"));
            };
            if self.take(&TokenKind::In) {
                self.no_in = false;
                let iterable = self.expression()?;
                self.require(&TokenKind::RightParen, "expected ')' after for-in clauses")?;
                self.loop_depth = self.loop_depth.saturating_add(1);
                let body = self.statement();
                self.loop_depth = self.loop_depth.saturating_sub(1);
                return Ok(Statement::ForIn {
                    kind,
                    name,
                    iterable,
                    body: Box::new(body?),
                });
            }
            self.cursor = declaration_start;
            let statement = self.variable_declaration(kind, false)?;
            self.require(&TokenKind::Semicolon, "expected ';' after for initializer")?;
            Some(Box::new(statement))
        } else {
            let expression = self.expression()?;
            if self.take(&TokenKind::In) {
                self.no_in = false;
                let iterable = self.expression()?;
                self.validate_assignment_target(&expression)?;
                self.require(&TokenKind::RightParen, "expected ')' after for-in clauses")?;
                self.loop_depth = self.loop_depth.saturating_add(1);
                let body = self.statement();
                self.loop_depth = self.loop_depth.saturating_sub(1);
                return Ok(Statement::ForInExpr {
                    target: expression,
                    iterable,
                    body: Box::new(body?),
                });
            }
            self.require(&TokenKind::Semicolon, "expected ';' after for initializer")?;
            Some(Box::new(Statement::Expression(expression)))
        };
        self.no_in = false;
        let condition = if self.take(&TokenKind::Semicolon) {
            None
        } else {
            let condition = self.expression()?;
            self.require(&TokenKind::Semicolon, "expected ';' after for condition")?;
            Some(condition)
        };
        let update = if self.at(&TokenKind::RightParen) {
            None
        } else {
            Some(self.expression()?)
        };
        self.require(&TokenKind::RightParen, "expected ')' after for clauses")?;
        self.loop_depth = self.loop_depth.saturating_add(1);
        let body = self.statement();
        self.loop_depth = self.loop_depth.saturating_sub(1);
        Ok(Statement::For {
            initializer,
            condition,
            update,
            body: Box::new(body?),
        })
    }

    fn try_statement(&mut self) -> Result<Statement, JsError> {
        let body = self.required_block("expected '{' after try")?;
        let catch = if self.take(&TokenKind::Catch) {
            let parameter = if self.take(&TokenKind::LeftParen) {
                let TokenKind::Identifier(parameter) = self.advance().kind else {
                    return Err(self.error("expected catch parameter"));
                };
                self.require(&TokenKind::RightParen, "expected ')' after catch parameter")?;
                parameter
            } else {
                "\0optional-catch-binding".to_owned()
            };
            Some(CatchClause {
                parameter,
                body: self.required_block("expected '{' after catch")?,
            })
        } else {
            None
        };
        let finally = if self.take(&TokenKind::Finally) {
            Some(self.required_block("expected '{' after finally")?)
        } else {
            None
        };
        if catch.is_none() && finally.is_none() {
            return Err(self.error("try requires catch or finally"));
        }
        Ok(Statement::Try {
            body,
            catch,
            finally,
        })
    }

    fn required_block(&mut self, message: &str) -> Result<Vec<Statement>, JsError> {
        self.require(&TokenKind::LeftBrace, message)?;
        let statements = self.statement_list(true)?;
        self.require(&TokenKind::RightBrace, "expected '}' after block")?;
        Ok(statements)
    }

    fn assignment(&mut self) -> Result<Expr, JsError> {
        if let Some(arrow) = self.arrow_function()? {
            return Ok(arrow);
        }
        let target = self.conditional()?;
        if self.take(&TokenKind::Equal) {
            self.assignment_value(target, None)
        } else if self.take(&TokenKind::PlusEqual) {
            self.assignment_value(target, Some(BinaryOp::Add))
        } else if self.take(&TokenKind::MinusEqual) {
            self.assignment_value(target, Some(BinaryOp::Subtract))
        } else if self.take(&TokenKind::StarEqual) {
            self.assignment_value(target, Some(BinaryOp::Multiply))
        } else if self.take(&TokenKind::SlashEqual) {
            self.assignment_value(target, Some(BinaryOp::Divide))
        } else if self.take(&TokenKind::PercentEqual) {
            self.assignment_value(target, Some(BinaryOp::Remainder))
        } else if self.take(&TokenKind::AmpersandEqual) {
            self.assignment_value(target, Some(BinaryOp::BitwiseAnd))
        } else if self.take(&TokenKind::CaretEqual) {
            self.assignment_value(target, Some(BinaryOp::BitwiseXor))
        } else if self.take(&TokenKind::PipeEqual) {
            self.assignment_value(target, Some(BinaryOp::BitwiseOr))
        } else if self.take(&TokenKind::LeftShiftEqual) {
            self.assignment_value(target, Some(BinaryOp::LeftShift))
        } else if self.take(&TokenKind::RightShiftEqual) {
            self.assignment_value(target, Some(BinaryOp::RightShift))
        } else if self.take(&TokenKind::UnsignedRightShiftEqual) {
            self.assignment_value(target, Some(BinaryOp::UnsignedRightShift))
        } else {
            Ok(target)
        }
    }

    fn arrow_function(&mut self) -> Result<Option<Expr>, JsError> {
        let checkpoint = self.cursor;
        let parameters = if let TokenKind::Identifier(name) = &self.current().kind {
            let name = name.clone();
            self.advance();
            if self.take(&TokenKind::Arrow) {
                vec![name]
            } else {
                self.cursor = checkpoint;
                return Ok(None);
            }
        } else if self.take(&TokenKind::LeftParen) {
            let mut parameters = Vec::new();
            if !self.at(&TokenKind::RightParen) {
                loop {
                    let TokenKind::Identifier(parameter) = self.advance().kind else {
                        self.cursor = checkpoint;
                        return Ok(None);
                    };
                    if parameters.contains(&parameter) {
                        return Err(self.error("duplicate arrow parameters are not supported"));
                    }
                    parameters.push(parameter);
                    if !self.take(&TokenKind::Comma) || self.at(&TokenKind::RightParen) {
                        break;
                    }
                }
            }
            if !self.take(&TokenKind::RightParen) || !self.take(&TokenKind::Arrow) {
                self.cursor = checkpoint;
                return Ok(None);
            }
            parameters
        } else {
            return Ok(None);
        };

        let body = if self.take(&TokenKind::LeftBrace) {
            let previous_function_depth = self.function_depth;
            let previous_loop_depth = self.loop_depth;
            self.function_depth = self.function_depth.saturating_add(1);
            self.loop_depth = 0;
            let body = self.statement_list(true);
            self.function_depth = previous_function_depth;
            self.loop_depth = previous_loop_depth;
            let body = body?;
            self.require(
                &TokenKind::RightBrace,
                "expected '}' after arrow function body",
            )?;
            body
        } else {
            vec![Statement::Return(Some(self.assignment()?))]
        };
        Ok(Some(Expr::Arrow { parameters, body }))
    }

    fn assignment_value(
        &mut self,
        target: Expr,
        operator: Option<BinaryOp>,
    ) -> Result<Expr, JsError> {
        self.validate_assignment_target(&target)?;
        let value = self.assignment()?;
        Ok(match operator {
            Some(operator) => Expr::CompoundAssignment {
                target: Box::new(target),
                operator,
                value: Box::new(value),
            },
            None => Expr::Assignment {
                target: Box::new(target),
                value: Box::new(value),
            },
        })
    }

    fn validate_assignment_target(&self, target: &Expr) -> Result<(), JsError> {
        if matches!(
            target,
            Expr::Identifier(_) | Expr::Member { .. } | Expr::ComputedMember { .. }
        ) {
            Ok(())
        } else {
            Err(self.error("invalid assignment target"))
        }
    }

    fn conditional(&mut self) -> Result<Expr, JsError> {
        let condition = self.logical_or()?;
        if !self.take(&TokenKind::Question) {
            return Ok(condition);
        }
        let consequent = self.assignment()?;
        self.require(&TokenKind::Colon, "expected ':' in conditional expression")?;
        let alternate = self.assignment()?;
        Ok(Expr::Conditional {
            condition: Box::new(condition),
            consequent: Box::new(consequent),
            alternate: Box::new(alternate),
        })
    }

    fn logical_or(&mut self) -> Result<Expr, JsError> {
        self.binary_level(
            Self::logical_and,
            &[(&TokenKind::OrOr, BinaryOp::LogicalOr)],
        )
    }

    fn logical_and(&mut self) -> Result<Expr, JsError> {
        self.binary_level(
            Self::bitwise_or,
            &[(&TokenKind::AndAnd, BinaryOp::LogicalAnd)],
        )
    }

    fn bitwise_or(&mut self) -> Result<Expr, JsError> {
        self.binary_level(
            Self::bitwise_xor,
            &[(&TokenKind::Pipe, BinaryOp::BitwiseOr)],
        )
    }

    fn bitwise_xor(&mut self) -> Result<Expr, JsError> {
        self.binary_level(
            Self::bitwise_and,
            &[(&TokenKind::Caret, BinaryOp::BitwiseXor)],
        )
    }

    fn bitwise_and(&mut self) -> Result<Expr, JsError> {
        self.binary_level(
            Self::equality,
            &[(&TokenKind::Ampersand, BinaryOp::BitwiseAnd)],
        )
    }

    fn equality(&mut self) -> Result<Expr, JsError> {
        self.binary_level(
            Self::comparison,
            &[
                (&TokenKind::EqualEqualEqual, BinaryOp::StrictEqual),
                (&TokenKind::BangEqualEqual, BinaryOp::StrictNotEqual),
                (&TokenKind::EqualEqual, BinaryOp::Equal),
                (&TokenKind::BangEqual, BinaryOp::NotEqual),
            ],
        )
    }

    fn comparison(&mut self) -> Result<Expr, JsError> {
        let mut operators = vec![
            (&TokenKind::LessEqual, BinaryOp::LessEqual),
            (&TokenKind::GreaterEqual, BinaryOp::GreaterEqual),
            (&TokenKind::Less, BinaryOp::Less),
            (&TokenKind::Greater, BinaryOp::Greater),
            (&TokenKind::Instanceof, BinaryOp::Instanceof),
        ];
        if !self.no_in {
            operators.push((&TokenKind::In, BinaryOp::In));
        }
        self.binary_level(Self::shift, &operators)
    }

    fn shift(&mut self) -> Result<Expr, JsError> {
        self.binary_level(
            Self::term,
            &[
                (&TokenKind::LeftShift, BinaryOp::LeftShift),
                (&TokenKind::RightShift, BinaryOp::RightShift),
                (&TokenKind::UnsignedRightShift, BinaryOp::UnsignedRightShift),
            ],
        )
    }

    fn term(&mut self) -> Result<Expr, JsError> {
        self.binary_level(
            Self::factor,
            &[
                (&TokenKind::Plus, BinaryOp::Add),
                (&TokenKind::Minus, BinaryOp::Subtract),
            ],
        )
    }

    fn factor(&mut self) -> Result<Expr, JsError> {
        self.binary_level(
            Self::unary,
            &[
                (&TokenKind::Star, BinaryOp::Multiply),
                (&TokenKind::Slash, BinaryOp::Divide),
                (&TokenKind::Percent, BinaryOp::Remainder),
            ],
        )
    }

    fn binary_level(
        &mut self,
        next: fn(&mut Self) -> Result<Expr, JsError>,
        operators: &[(&TokenKind, BinaryOp)],
    ) -> Result<Expr, JsError> {
        let mut expression = next(self)?;
        while let Some((_, operator)) = operators.iter().find(|(token, _)| self.at(token)) {
            self.advance();
            let right = next(self)?;
            expression = Expr::Binary {
                operator: *operator,
                left: Box::new(expression),
                right: Box::new(right),
            };
        }
        Ok(expression)
    }

    fn unary(&mut self) -> Result<Expr, JsError> {
        let update_operator = if self.take(&TokenKind::PlusPlus) {
            Some(BinaryOp::Add)
        } else if self.take(&TokenKind::MinusMinus) {
            Some(BinaryOp::Subtract)
        } else {
            None
        };
        if let Some(operator) = update_operator {
            let target = self.unary()?;
            self.validate_assignment_target(&target)?;
            return Ok(Expr::Update {
                target: Box::new(target),
                operator,
                prefix: true,
            });
        }
        let operator = if self.take(&TokenKind::Bang) {
            Some(UnaryOp::Not)
        } else if self.take(&TokenKind::Delete) {
            Some(UnaryOp::Delete)
        } else if self.take(&TokenKind::Typeof) {
            Some(UnaryOp::Typeof)
        } else if self.take(&TokenKind::Void) {
            Some(UnaryOp::Void)
        } else if self.take(&TokenKind::Plus) {
            Some(UnaryOp::Plus)
        } else if self.take(&TokenKind::Minus) {
            Some(UnaryOp::Minus)
        } else if self.take(&TokenKind::Tilde) {
            Some(UnaryOp::BitwiseNot)
        } else {
            None
        };
        if let Some(operator) = operator {
            return Ok(Expr::Unary {
                operator,
                operand: Box::new(self.unary()?),
            });
        }
        if self.take(&TokenKind::New) {
            // `new` binds to a whole member chain (`new A.B.C(...)`), so
            // consume dots/computed members BEFORE the argument list.
            let mut target = self.primary()?;
            loop {
                if self.take(&TokenKind::Dot) {
                    let property = self.property_name()?;
                    target = Expr::Member {
                        object: Box::new(target),
                        property,
                    };
                } else if self.take(&TokenKind::LeftBracket) {
                    let property = self.assignment()?;
                    self.require(
                        &TokenKind::RightBracket,
                        "expected ']' after computed property",
                    )?;
                    target = Expr::ComputedMember {
                        object: Box::new(target),
                        property: Box::new(property),
                    };
                } else {
                    break;
                }
            }
            let arguments = if self.take(&TokenKind::LeftParen) {
                self.arguments_after_left_paren()?
            } else {
                Vec::new()
            };
            let expression = Expr::New {
                constructor: Box::new(target),
                arguments,
            };
            return self.postfix_tail(expression);
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Result<Expr, JsError> {
        let expression = self.primary()?;
        let expression = self.postfix_tail(expression)?;
        let operator = if self.take(&TokenKind::PlusPlus) {
            Some(BinaryOp::Add)
        } else if self.take(&TokenKind::MinusMinus) {
            Some(BinaryOp::Subtract)
        } else {
            None
        };
        if let Some(operator) = operator {
            self.validate_assignment_target(&expression)?;
            Ok(Expr::Update {
                target: Box::new(expression),
                operator,
                prefix: false,
            })
        } else {
            Ok(expression)
        }
    }

    fn postfix_tail(&mut self, mut expression: Expr) -> Result<Expr, JsError> {
        loop {
            if self.take(&TokenKind::Dot) {
                let property = self.property_name()?;
                expression = Expr::Member {
                    object: Box::new(expression),
                    property,
                };
            } else if self.take(&TokenKind::LeftBracket) {
                let property = self.assignment()?;
                self.require(
                    &TokenKind::RightBracket,
                    "expected ']' after computed property",
                )?;
                expression = Expr::ComputedMember {
                    object: Box::new(expression),
                    property: Box::new(property),
                };
            } else if self.take(&TokenKind::LeftParen) {
                let arguments = self.arguments_after_left_paren()?;
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

    fn arguments_after_left_paren(&mut self) -> Result<Vec<Expr>, JsError> {
        let mut arguments = Vec::new();
        if !self.at(&TokenKind::RightParen) {
            loop {
                arguments.push(self.assignment()?);
                if !self.take(&TokenKind::Comma) || self.at(&TokenKind::RightParen) {
                    break;
                }
            }
        }
        self.require(&TokenKind::RightParen, "expected ')' after arguments")?;
        Ok(arguments)
    }

    fn primary(&mut self) -> Result<Expr, JsError> {
        let token = self.advance();
        match token.kind {
            TokenKind::Identifier(name) => Ok(Expr::Identifier(name)),
            TokenKind::This => Ok(Expr::This),
            TokenKind::String(value) => Ok(Expr::Literal(JsValue::String(value))),
            TokenKind::RegexLiteral { pattern, flags } => Ok(Expr::RegexLiteral { pattern, flags }),
            TokenKind::Number(value) => Ok(Expr::Literal(JsValue::Number(value))),
            TokenKind::True => Ok(Expr::Literal(JsValue::Boolean(true))),
            TokenKind::False => Ok(Expr::Literal(JsValue::Boolean(false))),
            TokenKind::Null => Ok(Expr::Literal(JsValue::Null)),
            TokenKind::Undefined => Ok(Expr::Literal(JsValue::Undefined)),
            TokenKind::Function => self.function_expression(),
            TokenKind::LeftBrace => self.object_literal(),
            TokenKind::LeftBracket => self.array_literal(),
            TokenKind::LeftParen => {
                // Parentheses restore normal `in` handling ([+In] context).
                let previous_no_in = self.no_in;
                self.no_in = false;
                let mut expressions = vec![self.assignment()?];
                while self.take(&TokenKind::Comma) {
                    expressions.push(self.assignment()?);
                }
                self.require(&TokenKind::RightParen, "expected ')' after expression")?;
                self.no_in = previous_no_in;
                if expressions.len() == 1 {
                    Ok(expressions.pop().expect("checked length"))
                } else {
                    Ok(Expr::Sequence(expressions))
                }
            }
            _ => Err(JsError::syntax("expected an expression", token.offset)),
        }
    }

    fn function_expression(&mut self) -> Result<Expr, JsError> {
        let name = if let TokenKind::Identifier(name) = &self.current().kind {
            let name = name.clone();
            self.advance();
            Some(name)
        } else {
            None
        };
        let (parameters, body) = self.function_tail()?;
        Ok(Expr::Function {
            name,
            parameters,
            body,
        })
    }

    fn function_tail(&mut self) -> Result<(Vec<String>, Vec<Statement>), JsError> {
        self.require(
            &TokenKind::LeftParen,
            "expected '(' before function parameters",
        )?;
        let mut parameters = Vec::new();
        if !self.at(&TokenKind::RightParen) {
            loop {
                // `undefined` is an ordinary identifier in parameter position.
                let parameter = match self.advance().kind {
                    TokenKind::Identifier(name) => name,
                    TokenKind::Undefined => "undefined".to_owned(),
                    _ => return Err(self.error("expected a parameter name")),
                };
                if parameters.contains(&parameter) {
                    return Err(self.error("duplicate function parameters are not supported"));
                }
                parameters.push(parameter);
                if !self.take(&TokenKind::Comma) || self.at(&TokenKind::RightParen) {
                    break;
                }
            }
        }
        self.require(&TokenKind::RightParen, "expected ')' after parameters")?;
        self.require(&TokenKind::LeftBrace, "expected '{' before function body")?;
        let previous_function_depth = self.function_depth;
        let previous_loop_depth = self.loop_depth;
        let previous_no_in = self.no_in;
        self.function_depth = self.function_depth.saturating_add(1);
        self.loop_depth = 0;
        self.no_in = false;
        let body = self.statement_list(true);
        self.function_depth = previous_function_depth;
        self.loop_depth = previous_loop_depth;
        self.no_in = previous_no_in;
        let body = body?;
        self.require(&TokenKind::RightBrace, "expected '}' after function body")?;
        Ok((parameters, body))
    }

    fn object_literal(&mut self) -> Result<Expr, JsError> {
        let mut properties = Vec::new();
        if !self.at(&TokenKind::RightBrace) {
            loop {
                let key = self.property_name()?;
                let value = if self.take(&TokenKind::Colon) {
                    self.assignment()?
                } else if self.at(&TokenKind::LeftParen) {
                    let (parameters, body) = self.function_tail()?;
                    Expr::Function {
                        name: Some(key.clone()),
                        parameters,
                        body,
                    }
                } else {
                    Expr::Identifier(key.clone())
                };
                properties.push((key, value));
                if !self.take(&TokenKind::Comma) {
                    break;
                }
                if self.at(&TokenKind::RightBrace) {
                    break;
                }
            }
        }
        self.require(&TokenKind::RightBrace, "expected '}' after object literal")?;
        Ok(Expr::Object(properties))
    }

    fn array_literal(&mut self) -> Result<Expr, JsError> {
        let mut elements = Vec::new();
        // Elisions (`[, ,]`) produce holes; the runtime models them as
        // `undefined` entries.
        while !self.at(&TokenKind::RightBracket) && !self.at(&TokenKind::Eof) {
            if self.take(&TokenKind::Comma) {
                elements.push(Expr::Literal(JsValue::Undefined));
                continue;
            }
            elements.push(self.assignment()?);
            if !self.take(&TokenKind::Comma) {
                break;
            }
        }
        self.require(&TokenKind::RightBracket, "expected ']' after array literal")?;
        Ok(Expr::Array(elements))
    }

    fn property_name(&mut self) -> Result<String, JsError> {
        let token = self.advance();
        match token.kind {
            TokenKind::Identifier(name) | TokenKind::String(name) => Ok(name),
            TokenKind::Number(value) => Ok(value.to_string()),
            // Keywords are valid property names after `.`.
            TokenKind::Let => Ok("let".to_owned()),
            TokenKind::Const => Ok("const".to_owned()),
            TokenKind::Var => Ok("var".to_owned()),
            TokenKind::Function => Ok("function".to_owned()),
            TokenKind::Return => Ok("return".to_owned()),
            TokenKind::New => Ok("new".to_owned()),
            TokenKind::Throw => Ok("throw".to_owned()),
            TokenKind::Try => Ok("try".to_owned()),
            TokenKind::Catch => Ok("catch".to_owned()),
            TokenKind::Finally => Ok("finally".to_owned()),
            TokenKind::If => Ok("if".to_owned()),
            TokenKind::Else => Ok("else".to_owned()),
            TokenKind::While => Ok("while".to_owned()),
            TokenKind::Do => Ok("do".to_owned()),
            TokenKind::For => Ok("for".to_owned()),
            TokenKind::Break => Ok("break".to_owned()),
            TokenKind::Continue => Ok("continue".to_owned()),
            TokenKind::Delete => Ok("delete".to_owned()),
            TokenKind::Typeof => Ok("typeof".to_owned()),
            TokenKind::Void => Ok("void".to_owned()),
            TokenKind::In => Ok("in".to_owned()),
            TokenKind::Instanceof => Ok("instanceof".to_owned()),
            TokenKind::Switch => Ok("switch".to_owned()),
            TokenKind::Case => Ok("case".to_owned()),
            TokenKind::Default => Ok("default".to_owned()),
            TokenKind::This => Ok("this".to_owned()),
            TokenKind::True => Ok("true".to_owned()),
            TokenKind::False => Ok("false".to_owned()),
            TokenKind::Null => Ok("null".to_owned()),
            TokenKind::Undefined => Ok("undefined".to_owned()),
            _ => Err(JsError::syntax("expected a property name", token.offset)),
        }
    }

    /// Automatic semicolon insertion: accept the statement as terminated
    /// when no explicit separator is present. The recursive-descent
    /// structure ensures each statement consumes exactly its own tokens.
    fn end_statement(&mut self) {
        let _ = self.take(&TokenKind::Semicolon);
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

fn has_use_strict_directive(statements: &[Statement]) -> bool {
    statements
        .iter()
        .take_while(|statement| {
            matches!(
                statement,
                Statement::Expression(Expr::Literal(JsValue::String(_)))
            )
        })
        .any(|statement| {
            matches!(
                statement,
                Statement::Expression(Expr::Literal(JsValue::String(value))) if value == "use strict"
            )
        })
}

fn validate_strict_statements(statements: &[Statement]) -> Result<(), JsError> {
    for statement in statements {
        validate_strict_statement(statement)?;
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "one arm per statement kind keeps the strict-mode rules explicit"
)]
fn validate_strict_statement(statement: &Statement) -> Result<(), JsError> {
    match statement {
        Statement::Variable {
            kind: VariableKind::Var,
            name,
            ..
        } if is_strict_reserved_word(name) => Err(JsError::syntax(
            format!("{name} is reserved in strict mode"),
            0,
        )),
        Statement::Function {
            parameters, body, ..
        } => {
            if parameters.iter().any(|name| is_strict_reserved_word(name)) {
                return Err(JsError::syntax(
                    "strict mode parameter uses a reserved word",
                    0,
                ));
            }
            if has_use_strict_directive(body) {
                validate_strict_statements(body)
            } else {
                Ok(())
            }
        }
        Statement::Block(statements) => validate_strict_statements(statements),
        Statement::If {
            consequent,
            alternate,
            ..
        } => {
            validate_strict_statement(consequent)?;
            if let Some(alternate) = alternate {
                validate_strict_statement(alternate)?;
            }
            Ok(())
        }
        Statement::While { body, .. } | Statement::For { body, .. } => {
            validate_strict_statement(body)
        }
        Statement::DoWhile { condition, body } => {
            validate_strict_expression(condition)?;
            validate_strict_statement(body)
        }
        Statement::Labeled { body, .. } => validate_strict_statement(body),
        Statement::ForIn {
            name,
            iterable,
            body,
            ..
        } => {
            if is_strict_reserved_word(name) {
                return Err(JsError::syntax(
                    format!("{name} is reserved in strict mode"),
                    0,
                ));
            }
            validate_strict_expression(iterable)?;
            validate_strict_statement(body)
        }
        Statement::ForInExpr {
            target,
            iterable,
            body,
        } => {
            validate_strict_expression(target)?;
            validate_strict_expression(iterable)?;
            validate_strict_statement(body)
        }
        Statement::Switch { expression, cases } => {
            validate_strict_expression(expression)?;
            for (test, statements) in cases {
                if let Some(test) = test {
                    validate_strict_expression(test)?;
                }
                validate_strict_statements(statements)?;
            }
            Ok(())
        }
        Statement::Try {
            body,
            catch,
            finally,
        } => {
            validate_strict_statements(body)?;
            if let Some(catch) = catch {
                validate_strict_statements(&catch.body)?;
            }
            if let Some(finally) = finally {
                validate_strict_statements(finally)?;
            }
            Ok(())
        }
        Statement::Expression(expression) => validate_strict_expression(expression),
        Statement::Variable { value, .. } => {
            value.as_ref().map_or(Ok(()), validate_strict_expression)
        }
        Statement::VariableList { declarations, .. } => declarations
            .iter()
            .filter_map(|(_, value)| value.as_ref())
            .try_for_each(validate_strict_expression),
        Statement::Return(value) => value.as_ref().map_or(Ok(()), validate_strict_expression),
        Statement::Throw(value) => validate_strict_expression(value),
        Statement::Break(_) | Statement::Continue(_) => Ok(()),
    }
}

fn validate_strict_expression(expression: &Expr) -> Result<(), JsError> {
    match expression {
        Expr::Assignment { target, value } | Expr::CompoundAssignment { target, value, .. } => {
            if matches!(target.as_ref(), Expr::Identifier(name) if is_strict_reserved_word(name)) {
                return Err(JsError::syntax(
                    "assignment to a strict mode reserved word",
                    0,
                ));
            }
            validate_strict_expression(target)?;
            validate_strict_expression(value)
        }
        Expr::Function {
            parameters, body, ..
        }
        | Expr::Arrow { parameters, body } => {
            if parameters.iter().any(|name| is_strict_reserved_word(name)) {
                return Err(JsError::syntax(
                    "strict mode parameter uses a reserved word",
                    0,
                ));
            }
            if has_use_strict_directive(body) {
                validate_strict_statements(body)
            } else {
                Ok(())
            }
        }
        Expr::Object(properties) => properties
            .iter()
            .try_for_each(|(_, value)| validate_strict_expression(value)),
        Expr::Array(elements) => elements.iter().try_for_each(validate_strict_expression),
        Expr::Unary {
            operator: UnaryOp::Delete,
            operand,
        } if matches!(operand.as_ref(), Expr::Identifier(_)) => Err(JsError::syntax(
            "delete of an unqualified identifier is not allowed in strict mode",
            0,
        )),
        Expr::Unary { operand, .. } => validate_strict_expression(operand),
        Expr::Binary { left, right, .. } => {
            validate_strict_expression(left)?;
            validate_strict_expression(right)
        }
        Expr::Conditional {
            condition,
            consequent,
            alternate,
        } => {
            validate_strict_expression(condition)?;
            validate_strict_expression(consequent)?;
            validate_strict_expression(alternate)
        }
        Expr::Update { target, .. } => {
            if matches!(target.as_ref(), Expr::Identifier(name) if is_strict_reserved_word(name)) {
                return Err(JsError::syntax("update of a strict mode reserved word", 0));
            }
            validate_strict_expression(target)
        }
        Expr::Member { object, .. } => validate_strict_expression(object),
        Expr::ComputedMember { object, property } => {
            validate_strict_expression(object)?;
            validate_strict_expression(property)
        }
        Expr::New {
            constructor,
            arguments,
        }
        | Expr::Call {
            callee: constructor,
            arguments,
        } => {
            validate_strict_expression(constructor)?;
            arguments.iter().try_for_each(validate_strict_expression)
        }
        Expr::Literal(_) | Expr::RegexLiteral { .. } | Expr::This | Expr::Identifier(_) => Ok(()),
        Expr::Sequence(expressions) => expressions.iter().try_for_each(validate_strict_expression),
    }
}

fn is_strict_reserved_word(name: &str) -> bool {
    matches!(
        name,
        "implements"
            | "interface"
            | "let"
            | "package"
            | "private"
            | "protected"
            | "public"
            | "static"
            | "yield"
    ) || name == "eval"
}

#[cfg(test)]
mod tests {
    use super::{Expr, Statement, parse};
    use crate::js::RuntimeLimits;
    use crate::js::lexer::tokenize;

    #[test]
    fn parses_function_control_flow_and_return() {
        let tokens = tokenize(
            "function classify(value) { if (value > 1) { return 'high'; } return 'low'; }",
            &RuntimeLimits::default(),
        )
        .expect("source should tokenize");
        let statements = parse(tokens, &RuntimeLimits::default()).expect("source should parse");
        assert!(
            matches!(statements.as_slice(), [Statement::Function { name, .. }] if name == "classify")
        );
    }

    #[test]
    fn parses_for_break_continue_and_new() {
        let tokens = tokenize(
            "for (let i = 0; i < 2; i = i + 1) { if (i === 1) continue; } new Factory(1);",
            &RuntimeLimits::default(),
        )
        .expect("source should tokenize");
        let statements = parse(tokens, &RuntimeLimits::default()).expect("source should parse");
        assert!(matches!(statements.first(), Some(Statement::For { .. })));
        assert!(matches!(
            statements.get(1),
            Some(Statement::Expression(Expr::New { .. }))
        ));
    }

    #[test]
    fn parses_for_in_declarations_and_delete() {
        let tokens = tokenize(
            "for (var key in object) { delete object[key]; } for (const name in object) {}",
            &RuntimeLimits::default(),
        )
        .expect("source should tokenize");
        let statements = parse(tokens, &RuntimeLimits::default()).expect("source should parse");
        assert!(matches!(
            statements.first(),
            Some(Statement::ForIn {
                kind: super::VariableKind::Var,
                name,
                ..
            }) if name == "key"
        ));
        assert!(matches!(
            statements.get(1),
            Some(Statement::ForIn {
                kind: super::VariableKind::Const,
                name,
                ..
            }) if name == "name"
        ));
    }

    #[test]
    fn strict_reserved_word_is_an_early_error() {
        let tokens = tokenize("\"use strict\"; var public = 1;", &RuntimeLimits::default())
            .expect("source should tokenize");
        let error = parse(tokens, &RuntimeLimits::default()).expect_err("strict error expected");
        assert_eq!(error.kind(), super::JsErrorKind::Syntax);
    }

    #[test]
    fn parses_unicode_escaped_variable_name() {
        let tokens = tokenize(r"let \u0061 = 1;", &RuntimeLimits::default())
            .expect("source should tokenize");
        let statements = parse(tokens, &RuntimeLimits::default()).expect("source should parse");
        assert!(matches!(statements.as_slice(), [Statement::Variable { name, .. }] if name == "a"));
    }
}
