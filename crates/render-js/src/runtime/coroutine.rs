//! Generator and async function bodies, compiled to a flat instruction list.
//!
//! A tree-walking evaluator keeps its place on the Rust call stack, so it
//! cannot stop in the middle of a function and continue later. Rather than
//! thread resumability through the whole evaluator, a function whose body
//! contains `yield` or `await` is compiled once into instructions with an
//! explicit program counter. Everything that cannot suspend still runs on the
//! ordinary evaluator: a statement with no suspension point inside it becomes a
//! single [`Instr::Exec`]. Only the spine from the function body down to each
//! `yield`/`await` is flattened into jumps, scope pushes, loop entries and
//! `try` handlers, and that spine is what [`super::coroutine_run`] drives.
//!
//! Expressions are the other half. `a + await b` cannot be a single
//! instruction, so [`Compiler::explode`] rewrites an expression with a
//! suspension point inside into instructions that compute the pieces into
//! hidden `%t…` bindings first, in the original evaluation order, and returns
//! the suspension-free expression that combines them.

use crate::JsError;
use crate::JsValue;
use crate::parser::{
    BinaryOp, BindingPattern, BindingTarget, ClassElement, Expr, ObjectProperty, PropertyKey,
    Statement, VariableKind,
};
use std::rc::Rc;

/// What a [`Instr::Suspend`] waits for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SuspendKind {
    Yield,
    YieldDelegate,
    Await,
}

/// Where a `break`/`continue` can land, recorded when its loop is entered.
#[derive(Debug)]
pub(super) struct LoopInfo {
    pub(super) labels: Vec<Rc<str>>,
    pub(super) break_pc: usize,
    /// `None` for a `switch` or a labeled block, which `continue` skips over.
    pub(super) continue_pc: Option<usize>,
    /// Whether an unlabeled `break` stops here (loops and `switch`, not
    /// labeled blocks).
    pub(super) unlabeled_break: bool,
    /// Hidden-binding slot holding the iterator a `for…of` must close.
    pub(super) iterator_slot: Option<usize>,
}

/// How a `for…in`/`for…of` head binds each value.
#[derive(Debug)]
pub(super) struct LoopBinding {
    pub(super) kind: VariableKind,
    pub(super) name: String,
}

#[derive(Clone, Debug)]
pub(super) enum Instr {
    /// Run a statement that contains no suspension point.
    Exec(Rc<Statement>),
    /// Evaluate an expression for its effects.
    Eval(Rc<Expr>),
    /// Store an expression's value in a hidden function-scope binding.
    SetTemp(Rc<str>, Rc<Expr>),
    Suspend {
        kind: SuspendKind,
        argument: Option<Rc<Expr>>,
        /// Hidden binding that receives the value the coroutine is resumed with.
        target: Rc<str>,
    },
    Jump(usize),
    JumpIfFalse(Rc<Expr>, usize),
    JumpIfTrue(Rc<Expr>, usize),
    /// Push a block scope and create its lexical declarations.
    PushScope(Rc<Vec<Statement>>),
    PopScope,
    EnterLoop(Rc<LoopInfo>),
    ExitLoop,
    Break(Option<Rc<str>>),
    Continue(Option<Rc<str>>),
    Return(Option<Rc<Expr>>),
    ForOfInit {
        iterable: Rc<Expr>,
        slot: usize,
        binding: Rc<LoopBinding>,
    },
    ForOfNext {
        slot: usize,
        binding: Rc<LoopBinding>,
        done_pc: usize,
    },
    ForInInit {
        iterable: Rc<Expr>,
        slot: usize,
        binding: Rc<LoopBinding>,
    },
    ForInNext {
        slot: usize,
        binding: Rc<LoopBinding>,
        done_pc: usize,
    },
    /// Give a `for (let …;;)` loop a fresh copy of its scope for the next
    /// iteration, so closures made in the previous one keep their own values.
    CopyScope,
    TryEnter {
        catch_pc: Option<usize>,
        finally_pc: Option<usize>,
        slot: usize,
    },
    /// The `try` block finished normally.
    TryExit {
        finally_pc: Option<usize>,
        after_pc: usize,
        slot: usize,
    },
    CatchBind {
        parameter: Option<BindingTarget>,
        lexicals: Rc<Vec<Statement>>,
        slot: usize,
    },
    /// The `catch` block finished normally.
    CatchExit {
        finally_pc: Option<usize>,
        after_pc: usize,
        slot: usize,
    },
    /// The end of a `finally` block: carry on with whatever was pending.
    FinallyEnd {
        slot: usize,
    },
}

/// A compiled coroutine body.
#[derive(Debug, Default)]
pub(super) struct CoroutineCode {
    pub(super) instrs: Vec<Instr>,
}

/// Name of the hidden binding for expression temporary `index`.
pub(super) fn temp_name(index: usize) -> String {
    format!("%t{index}")
}

fn is_hidden(expression: &Expr) -> bool {
    matches!(expression, Expr::Identifier(name) if name.starts_with('%'))
}

// ---------------------------------------------------------------- analysis

pub(super) fn statements_have_suspend(statements: &[Statement]) -> bool {
    statements.iter().any(statement_has_suspend)
}

pub(super) fn statement_has_suspend(statement: &Statement) -> bool {
    match statement {
        Statement::Variable { value, .. } => value.as_ref().is_some_and(expr_has_suspend),
        Statement::VariableList { declarations, .. } => {
            declarations.iter().any(|(target, value)| {
                value.as_ref().is_some_and(expr_has_suspend) || target_has_suspend(target)
            })
        }
        Statement::Function { .. }
        | Statement::Break(_)
        | Statement::Continue(_)
        | Statement::ParameterDefault { .. } => false,
        Statement::Class {
            super_class,
            elements,
            ..
        } => {
            super_class.as_deref().is_some_and(expr_has_suspend) || elements_have_suspend(elements)
        }
        Statement::Return(value) => value.as_ref().is_some_and(expr_has_suspend),
        Statement::Throw(value) | Statement::Expression(value) => expr_has_suspend(value),
        Statement::Try {
            body,
            catch,
            finally,
            ..
        } => {
            statements_have_suspend(body)
                || catch
                    .as_ref()
                    .is_some_and(|catch| statements_have_suspend(&catch.body))
                || finally.as_deref().is_some_and(statements_have_suspend)
        }
        Statement::If {
            condition,
            consequent,
            alternate,
            ..
        } => {
            expr_has_suspend(condition)
                || statement_has_suspend(consequent)
                || alternate.as_deref().is_some_and(statement_has_suspend)
        }
        Statement::Switch {
            expression, cases, ..
        } => {
            expr_has_suspend(expression)
                || cases.iter().any(|(tests, body)| {
                    tests.iter().any(expr_has_suspend) || statements_have_suspend(body)
                })
        }
        Statement::While {
            condition, body, ..
        } => expr_has_suspend(condition) || statement_has_suspend(body),
        Statement::DoWhile {
            condition, body, ..
        } => expr_has_suspend(condition) || statement_has_suspend(body),
        Statement::For {
            initializer,
            condition,
            update,
            body,
            ..
        } => {
            initializer.as_deref().is_some_and(statement_has_suspend)
                || condition.as_ref().is_some_and(expr_has_suspend)
                || update.as_ref().is_some_and(expr_has_suspend)
                || statement_has_suspend(body)
        }
        Statement::ForIn { iterable, body, .. } | Statement::ForOf { iterable, body, .. } => {
            expr_has_suspend(iterable) || statement_has_suspend(body)
        }
        Statement::ForInExpr {
            target,
            iterable,
            body,
            ..
        } => expr_has_suspend(target) || expr_has_suspend(iterable) || statement_has_suspend(body),
        Statement::Labeled { body, .. } => statement_has_suspend(body),
        Statement::Block(statements) => statements_have_suspend(statements),
    }
}

fn target_has_suspend(target: &BindingTarget) -> bool {
    match target {
        BindingTarget::Name(_) => false,
        BindingTarget::Pattern(pattern) => pattern_has_suspend(pattern),
    }
}

fn pattern_has_suspend(pattern: &BindingPattern) -> bool {
    match pattern {
        BindingPattern::Identifier(_) => false,
        BindingPattern::Object { properties, rest } => {
            properties.iter().any(|(key, pattern)| {
                matches!(key, PropertyKey::Computed(expression) if expr_has_suspend(expression))
                    || pattern_has_suspend(pattern)
            }) || rest.as_deref().is_some_and(pattern_has_suspend)
        }
        BindingPattern::Array { elements, rest } => {
            elements.iter().flatten().any(pattern_has_suspend)
                || rest.as_deref().is_some_and(pattern_has_suspend)
        }
        BindingPattern::Default { pattern, value } => {
            pattern_has_suspend(pattern) || expr_has_suspend(value)
        }
    }
}

fn elements_have_suspend(elements: &[ClassElement]) -> bool {
    elements.iter().any(|element| {
        matches!(&element.key, PropertyKey::Computed(expression) if expr_has_suspend(expression))
    })
}

fn properties_have_suspend(properties: &[ObjectProperty]) -> bool {
    properties.iter().any(|property| {
        matches!(&property.key, PropertyKey::Computed(expression) if expr_has_suspend(expression))
            || expr_has_suspend(&property.value)
    })
}

/// Whether `expression` contains a `yield` or `await` of the function being
/// compiled. Nested functions and arrows are separate coroutines, so their
/// bodies do not count.
pub(super) fn expr_has_suspend(expression: &Expr) -> bool {
    match expression {
        Expr::Yield { .. } | Expr::Await(_) => true,
        Expr::Literal(_)
        | Expr::RegexLiteral { .. }
        | Expr::This
        | Expr::Identifier(_)
        | Expr::Function { .. }
        | Expr::Arrow { .. }
        | Expr::NewTarget
        | Expr::SuperMember { .. } => false,
        Expr::Class {
            super_class,
            elements,
            ..
        } => {
            super_class.as_deref().is_some_and(expr_has_suspend) || elements_have_suspend(elements)
        }
        Expr::SuperComputedMember { property, .. } => expr_has_suspend(property),
        Expr::SuperCall { arguments, .. } => arguments.iter().any(expr_has_suspend),
        Expr::PrivateMember { object, .. } | Expr::PrivateIn { object, .. } => {
            expr_has_suspend(object)
        }
        Expr::Object(properties) => properties_have_suspend(properties),
        Expr::Array(elements) | Expr::Sequence(elements) => elements.iter().any(expr_has_suspend),
        Expr::Spread(inner) | Expr::OptionalChain(inner) | Expr::OptionalGuard(inner) => {
            expr_has_suspend(inner)
        }
        Expr::Unary { operand, .. } => expr_has_suspend(operand),
        Expr::Binary { left, right, .. } => expr_has_suspend(left) || expr_has_suspend(right),
        Expr::Conditional {
            condition,
            consequent,
            alternate,
            ..
        } => {
            expr_has_suspend(condition)
                || expr_has_suspend(consequent)
                || expr_has_suspend(alternate)
        }
        Expr::Update { target, .. } => expr_has_suspend(target),
        Expr::Member { object, .. } => expr_has_suspend(object),
        Expr::ComputedMember {
            object, property, ..
        } => expr_has_suspend(object) || expr_has_suspend(property),
        Expr::New {
            constructor,
            arguments,
            ..
        } => expr_has_suspend(constructor) || arguments.iter().any(expr_has_suspend),
        Expr::Call {
            callee, arguments, ..
        } => expr_has_suspend(callee) || arguments.iter().any(expr_has_suspend),
        Expr::TaggedTemplate {
            tag, expressions, ..
        } => expr_has_suspend(tag) || expressions.iter().any(expr_has_suspend),
        Expr::Assignment { target, value, .. }
        | Expr::CompoundAssignment { target, value, .. }
        | Expr::LogicalAssignment { target, value, .. } => {
            expr_has_suspend(target) || expr_has_suspend(value)
        }
    }
}

// ---------------------------------------------------------------- compiler

pub(super) struct Compiler {
    instrs: Vec<Instr>,
    next_temp: usize,
    next_slot: usize,
    /// Labels waiting for the loop (or block) they were written in front of.
    pending_labels: Vec<Rc<str>>,
}

fn unsupported(what: &str, offset: usize) -> JsError {
    JsError::syntax(
        format!("`await`/`yield` inside {what} is not supported"),
        offset,
    )
}

impl Compiler {
    /// Compile a function body into instructions.
    pub(super) fn compile(body: &[Statement]) -> Result<CoroutineCode, JsError> {
        let mut compiler = Self {
            instrs: Vec::new(),
            next_temp: 0,
            next_slot: 0,
            pending_labels: Vec::new(),
        };
        for statement in body {
            compiler.statement(statement)?;
        }
        Ok(CoroutineCode {
            instrs: compiler.instrs,
        })
    }

    fn here(&self) -> usize {
        self.instrs.len()
    }

    fn emit(&mut self, instruction: Instr) -> usize {
        self.instrs.push(instruction);
        self.instrs.len() - 1
    }

    fn fresh_temp(&mut self) -> Rc<str> {
        let name = temp_name(self.next_temp);
        self.next_temp += 1;
        Rc::from(name)
    }

    fn fresh_slot(&mut self) -> usize {
        let slot = self.next_slot;
        self.next_slot += 1;
        slot
    }

    /// Point a previously emitted jump-like instruction at `target`.
    fn patch(&mut self, at: usize, target: usize) {
        match &mut self.instrs[at] {
            Instr::Jump(destination)
            | Instr::JumpIfFalse(_, destination)
            | Instr::JumpIfTrue(_, destination) => *destination = target,
            other => unreachable!("patching a non-jump instruction: {other:?}"),
        }
    }

    // ------------------------------------------------------------ statements

    fn statement(&mut self, statement: &Statement) -> Result<(), JsError> {
        if !statement_has_suspend(statement) {
            match statement {
                Statement::Break(label) => {
                    self.emit(Instr::Break(label.as_deref().map(Rc::from)));
                }
                Statement::Continue(label) => {
                    self.emit(Instr::Continue(label.as_deref().map(Rc::from)));
                }
                _ => {
                    self.pending_labels.clear();
                    self.emit(Instr::Exec(Rc::new(statement.clone())));
                }
            }
            return Ok(());
        }
        match statement {
            Statement::Expression(expression) => {
                let residual = self.explode(expression)?;
                if !is_hidden(&residual) {
                    self.emit(Instr::Eval(Rc::new(residual)));
                }
            }
            Statement::Variable {
                kind,
                name,
                value,
                offset,
            } => {
                let value = match value {
                    Some(value) => Some(self.explode(value)?),
                    None => None,
                };
                self.emit(Instr::Exec(Rc::new(Statement::Variable {
                    kind: *kind,
                    name: name.clone(),
                    value,
                    offset: *offset,
                })));
            }
            Statement::VariableList {
                kind,
                declarations,
                offset,
            } => {
                for (target, value) in declarations {
                    if target_has_suspend(target) {
                        return Err(unsupported("a destructuring default", *offset));
                    }
                    let value = match value {
                        Some(value) => Some(self.explode(value)?),
                        None => None,
                    };
                    self.emit(Instr::Exec(Rc::new(Statement::VariableList {
                        kind: *kind,
                        declarations: vec![(target.clone(), value)],
                        offset: *offset,
                    })));
                }
            }
            Statement::Return(value) => {
                let value = match value {
                    Some(value) => Some(Rc::new(self.explode(value)?)),
                    None => None,
                };
                self.emit(Instr::Return(value));
            }
            Statement::Throw(value) => {
                let residual = self.explode(value)?;
                self.emit(Instr::Exec(Rc::new(Statement::Throw(residual))));
            }
            Statement::Block(statements) => {
                self.emit(Instr::PushScope(Rc::new(statements.clone())));
                for statement in statements {
                    self.statement(statement)?;
                }
                self.emit(Instr::PopScope);
            }
            Statement::If {
                condition,
                consequent,
                alternate,
                offset,
            } => {
                let condition = self.explode(condition)?;
                let branches_suspend = statement_has_suspend(consequent)
                    || alternate.as_deref().is_some_and(statement_has_suspend);
                if !branches_suspend {
                    self.emit(Instr::Exec(Rc::new(Statement::If {
                        condition,
                        consequent: consequent.clone(),
                        alternate: alternate.clone(),
                        offset: *offset,
                    })));
                    return Ok(());
                }
                let to_else = self.emit(Instr::JumpIfFalse(Rc::new(condition), 0));
                self.statement(consequent)?;
                if let Some(alternate) = alternate {
                    let to_end = self.emit(Instr::Jump(0));
                    let else_pc = self.here();
                    self.patch(to_else, else_pc);
                    self.statement(alternate)?;
                    let end = self.here();
                    self.patch(to_end, end);
                } else {
                    let end = self.here();
                    self.patch(to_else, end);
                }
            }
            Statement::While {
                condition, body, ..
            } => {
                let labels = std::mem::take(&mut self.pending_labels);
                let enter = self.emit(Instr::Jump(0));
                let test = self.here();
                let condition = self.explode(condition)?;
                let to_exit = self.emit(Instr::JumpIfFalse(Rc::new(condition), 0));
                self.statement(body)?;
                self.emit(Instr::Jump(test));
                let exit = self.here();
                self.patch(to_exit, exit);
                self.emit(Instr::ExitLoop);
                let after = self.here();
                self.instrs[enter] = Instr::EnterLoop(Rc::new(LoopInfo {
                    labels,
                    break_pc: after,
                    continue_pc: Some(test),
                    unlabeled_break: true,
                    iterator_slot: None,
                }));
            }
            Statement::DoWhile {
                condition, body, ..
            } => {
                let labels = std::mem::take(&mut self.pending_labels);
                let enter = self.emit(Instr::Jump(0));
                let start = self.here();
                self.statement(body)?;
                let continue_pc = self.here();
                let condition = self.explode(condition)?;
                self.emit(Instr::JumpIfTrue(Rc::new(condition), start));
                self.emit(Instr::ExitLoop);
                let after = self.here();
                self.instrs[enter] = Instr::EnterLoop(Rc::new(LoopInfo {
                    labels,
                    break_pc: after,
                    continue_pc: Some(continue_pc),
                    unlabeled_break: true,
                    iterator_slot: None,
                }));
            }
            Statement::For {
                initializer,
                condition,
                update,
                body,
                ..
            } => {
                let labels = std::mem::take(&mut self.pending_labels);
                let lexical = matches!(
                    initializer.as_deref(),
                    Some(
                        Statement::Variable {
                            kind: VariableKind::Let | VariableKind::Const,
                            ..
                        } | Statement::VariableList {
                            kind: VariableKind::Let | VariableKind::Const,
                            ..
                        }
                    )
                );
                let scope_statements = initializer
                    .as_deref()
                    .map(|initializer| vec![initializer.clone()])
                    .unwrap_or_default();
                self.emit(Instr::PushScope(Rc::new(scope_statements)));
                if let Some(initializer) = initializer {
                    self.statement(initializer)?;
                }
                let enter = self.emit(Instr::Jump(0));
                let test = self.here();
                let to_exit = match condition {
                    Some(condition) => {
                        let condition = self.explode(condition)?;
                        Some(self.emit(Instr::JumpIfFalse(Rc::new(condition), 0)))
                    }
                    None => None,
                };
                self.statement(body)?;
                let continue_pc = self.here();
                if lexical {
                    self.emit(Instr::CopyScope);
                }
                if let Some(update) = update {
                    let update = self.explode(update)?;
                    self.emit(Instr::Eval(Rc::new(update)));
                }
                self.emit(Instr::Jump(test));
                let exit = self.here();
                if let Some(to_exit) = to_exit {
                    self.patch(to_exit, exit);
                }
                self.emit(Instr::ExitLoop);
                let after = self.here();
                self.emit(Instr::PopScope);
                self.instrs[enter] = Instr::EnterLoop(Rc::new(LoopInfo {
                    labels,
                    break_pc: after,
                    continue_pc: Some(continue_pc),
                    unlabeled_break: true,
                    iterator_slot: None,
                }));
            }
            Statement::ForOf {
                kind,
                name,
                iterable,
                body,
                ..
            } => self.for_each(true, *kind, name, iterable, body)?,
            Statement::ForIn {
                kind,
                name,
                iterable,
                body,
                ..
            } => self.for_each(false, *kind, name, iterable, body)?,
            Statement::ForInExpr { offset, .. } => {
                return Err(unsupported("a `for…in` with an assignment target", *offset));
            }
            Statement::Labeled { label, body, .. } => {
                self.pending_labels.push(Rc::from(label.as_str()));
                if matches!(
                    body.as_ref(),
                    Statement::While { .. }
                        | Statement::DoWhile { .. }
                        | Statement::For { .. }
                        | Statement::ForIn { .. }
                        | Statement::ForOf { .. }
                        | Statement::Labeled { .. }
                ) {
                    return self.statement(body);
                }
                // A labeled block or statement: `break label` leaves it.
                let labels = std::mem::take(&mut self.pending_labels);
                let enter = self.emit(Instr::Jump(0));
                self.statement(body)?;
                self.emit(Instr::ExitLoop);
                let after = self.here();
                self.instrs[enter] = Instr::EnterLoop(Rc::new(LoopInfo {
                    labels,
                    break_pc: after,
                    continue_pc: None,
                    unlabeled_break: false,
                    iterator_slot: None,
                }));
            }
            Statement::Switch {
                expression, cases, ..
            } => self.switch(expression, cases)?,
            Statement::Try {
                body,
                catch,
                finally,
                ..
            } => self.try_statement(body, catch.as_ref(), finally.as_deref())?,
            Statement::Class { offset, .. } => {
                return Err(unsupported(
                    "a class heritage or computed member name",
                    *offset,
                ));
            }
            Statement::Function { .. }
            | Statement::Break(_)
            | Statement::Continue(_)
            | Statement::ParameterDefault { .. } => {
                unreachable!("these never contain a suspension point of this function")
            }
        }
        Ok(())
    }

    fn for_each(
        &mut self,
        of: bool,
        kind: VariableKind,
        name: &str,
        iterable: &Expr,
        body: &Statement,
    ) -> Result<(), JsError> {
        let labels = std::mem::take(&mut self.pending_labels);
        let iterable = Rc::new(self.explode(iterable)?);
        let slot = self.fresh_slot();
        let binding = Rc::new(LoopBinding {
            kind,
            name: name.to_owned(),
        });
        self.emit(if of {
            Instr::ForOfInit {
                iterable,
                slot,
                binding: binding.clone(),
            }
        } else {
            Instr::ForInInit {
                iterable,
                slot,
                binding: binding.clone(),
            }
        });
        let enter = self.emit(Instr::Jump(0));
        let next = self.here();
        let step = self.emit(Instr::Jump(0));
        self.statement(body)?;
        self.emit(Instr::Jump(next));
        let done = self.here();
        self.emit(Instr::ExitLoop);
        let after = self.here();
        self.instrs[step] = if of {
            Instr::ForOfNext {
                slot,
                binding,
                done_pc: done,
            }
        } else {
            Instr::ForInNext {
                slot,
                binding,
                done_pc: done,
            }
        };
        self.instrs[enter] = Instr::EnterLoop(Rc::new(LoopInfo {
            labels,
            break_pc: after,
            continue_pc: Some(next),
            unlabeled_break: true,
            iterator_slot: of.then_some(slot),
        }));
        Ok(())
    }

    fn switch(
        &mut self,
        expression: &Expr,
        cases: &[(Vec<Expr>, Vec<Statement>)],
    ) -> Result<(), JsError> {
        let labels = std::mem::take(&mut self.pending_labels);
        let discriminant = self.explode(expression)?;
        let discriminant = self.spill(discriminant);
        let all_statements: Vec<Statement> = cases
            .iter()
            .flat_map(|(_, statements)| statements.iter().cloned())
            .collect();
        self.emit(Instr::PushScope(Rc::new(all_statements)));
        let enter = self.emit(Instr::Jump(0));
        // One jump per `case` test; they all land on that case's body.
        let mut test_jumps: Vec<(usize, usize)> = Vec::new();
        for (index, (tests, _)) in cases.iter().enumerate() {
            for test in tests {
                let test = self.explode(test)?;
                let comparison = Expr::Binary {
                    operator: BinaryOp::StrictEqual,
                    left: Box::new(discriminant.clone()),
                    right: Box::new(test),
                    offset: 0,
                };
                let jump = self.emit(Instr::JumpIfTrue(Rc::new(comparison), 0));
                test_jumps.push((jump, index));
            }
        }
        let no_match = self.emit(Instr::Jump(0));
        let mut case_starts = Vec::new();
        for (_, statements) in cases {
            case_starts.push(self.here());
            for statement in statements {
                self.statement(statement)?;
            }
        }
        let end = self.here();
        for (jump, index) in test_jumps {
            self.patch(jump, case_starts[index]);
        }
        let default = cases
            .iter()
            .position(|(tests, _)| tests.is_empty())
            .map_or(end, |index| case_starts[index]);
        self.patch(no_match, default);
        self.emit(Instr::ExitLoop);
        let after = self.here();
        self.emit(Instr::PopScope);
        self.instrs[enter] = Instr::EnterLoop(Rc::new(LoopInfo {
            labels,
            break_pc: after,
            continue_pc: None,
            unlabeled_break: true,
            iterator_slot: None,
        }));
        Ok(())
    }

    fn try_statement(
        &mut self,
        body: &[Statement],
        catch: Option<&crate::parser::CatchClause>,
        finally: Option<&[Statement]>,
    ) -> Result<(), JsError> {
        let slot = self.fresh_slot();
        let enter = self.emit(Instr::Jump(0));
        self.emit(Instr::PushScope(Rc::new(body.to_vec())));
        for statement in body {
            self.statement(statement)?;
        }
        self.emit(Instr::PopScope);
        let try_exit = self.emit(Instr::Jump(0));
        let mut catch_pc = None;
        let mut catch_exit = None;
        if let Some(catch) = catch {
            catch_pc = Some(self.here());
            self.emit(Instr::CatchBind {
                parameter: catch.parameter.clone(),
                lexicals: Rc::new(catch.body.clone()),
                slot,
            });
            for statement in &catch.body {
                self.statement(statement)?;
            }
            self.emit(Instr::PopScope);
            catch_exit = Some(self.emit(Instr::Jump(0)));
        }
        let mut finally_pc = None;
        if let Some(finally) = finally {
            finally_pc = Some(self.here());
            self.emit(Instr::PushScope(Rc::new(finally.to_vec())));
            for statement in finally {
                self.statement(statement)?;
            }
            self.emit(Instr::PopScope);
            self.emit(Instr::FinallyEnd { slot });
        }
        let after = self.here();
        self.instrs[enter] = Instr::TryEnter {
            catch_pc,
            finally_pc,
            slot,
        };
        self.instrs[try_exit] = Instr::TryExit {
            finally_pc,
            after_pc: after,
            slot,
        };
        if let Some(catch_exit) = catch_exit {
            self.instrs[catch_exit] = Instr::CatchExit {
                finally_pc,
                after_pc: after,
                slot,
            };
        }
        Ok(())
    }

    // ----------------------------------------------------------- expressions

    /// Store `expression` in a fresh temporary unless it is already a constant
    /// or a temporary, so a later suspension cannot change what it evaluates to.
    fn spill(&mut self, expression: Expr) -> Expr {
        if matches!(expression, Expr::Literal(_)) || is_hidden(&expression) {
            return expression;
        }
        let name = self.fresh_temp();
        self.emit(Instr::SetTemp(name.clone(), Rc::new(expression)));
        Expr::Identifier(name.to_string())
    }

    /// Rewrite `expression` into instructions plus a suspension-free residual
    /// expression, preserving left-to-right evaluation.
    fn explode(&mut self, expression: &Expr) -> Result<Expr, JsError> {
        if !expr_has_suspend(expression) {
            return Ok(expression.clone());
        }
        match expression {
            Expr::Await(operand) => {
                let argument = self.explode(operand)?;
                let target = self.fresh_temp();
                self.emit(Instr::Suspend {
                    kind: SuspendKind::Await,
                    argument: Some(Rc::new(argument)),
                    target: target.clone(),
                });
                Ok(Expr::Identifier(target.to_string()))
            }
            Expr::Yield {
                argument, delegate, ..
            } => {
                let argument = match argument {
                    Some(argument) => Some(Rc::new(self.explode(argument)?)),
                    None => None,
                };
                let target = self.fresh_temp();
                self.emit(Instr::Suspend {
                    kind: if *delegate {
                        SuspendKind::YieldDelegate
                    } else {
                        SuspendKind::Yield
                    },
                    argument,
                    target: target.clone(),
                });
                Ok(Expr::Identifier(target.to_string()))
            }
            Expr::Binary {
                operator,
                left,
                right,
                offset,
            } => {
                let logical = matches!(
                    operator,
                    BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Nullish
                );
                if logical && expr_has_suspend(right) {
                    // `a && await b`: the right side only runs when the left
                    // does not decide the result.
                    let left = self.explode(left)?;
                    let result = self.fresh_temp();
                    self.emit(Instr::SetTemp(result.clone(), Rc::new(left)));
                    let identifier = Expr::Identifier(result.to_string());
                    let skip = match operator {
                        BinaryOp::LogicalAnd => {
                            self.emit(Instr::JumpIfFalse(Rc::new(identifier.clone()), 0))
                        }
                        BinaryOp::LogicalOr => {
                            self.emit(Instr::JumpIfTrue(Rc::new(identifier.clone()), 0))
                        }
                        _ => {
                            // `a ?? b` keeps `a` unless it is null or undefined.
                            let not_nullish = Expr::Binary {
                                operator: BinaryOp::NotEqual,
                                left: Box::new(identifier.clone()),
                                right: Box::new(Expr::Literal(JsValue::Null)),
                                offset: *offset,
                            };
                            self.emit(Instr::JumpIfTrue(Rc::new(not_nullish), 0))
                        }
                    };
                    let right = self.explode(right)?;
                    self.emit(Instr::SetTemp(result, Rc::new(right)));
                    let end = self.here();
                    self.patch(skip, end);
                    return Ok(identifier);
                }
                let mut left = self.explode(left)?;
                if expr_has_suspend(right) {
                    left = self.spill(left);
                }
                let right = self.explode(right)?;
                Ok(Expr::Binary {
                    operator: *operator,
                    left: Box::new(left),
                    right: Box::new(right),
                    offset: *offset,
                })
            }
            Expr::Conditional {
                condition,
                consequent,
                alternate,
                offset,
            } => {
                let condition = self.explode(condition)?;
                if !expr_has_suspend(consequent) && !expr_has_suspend(alternate) {
                    return Ok(Expr::Conditional {
                        condition: Box::new(condition),
                        consequent: consequent.clone(),
                        alternate: alternate.clone(),
                        offset: *offset,
                    });
                }
                let result = self.fresh_temp();
                let to_else = self.emit(Instr::JumpIfFalse(Rc::new(condition), 0));
                let consequent = self.explode(consequent)?;
                self.emit(Instr::SetTemp(result.clone(), Rc::new(consequent)));
                let to_end = self.emit(Instr::Jump(0));
                let else_pc = self.here();
                self.patch(to_else, else_pc);
                let alternate = self.explode(alternate)?;
                self.emit(Instr::SetTemp(result.clone(), Rc::new(alternate)));
                let end = self.here();
                self.patch(to_end, end);
                Ok(Expr::Identifier(result.to_string()))
            }
            Expr::Unary {
                operator,
                operand,
                offset,
            } => Ok(Expr::Unary {
                operator: *operator,
                operand: Box::new(self.explode(operand)?),
                offset: *offset,
            }),
            Expr::Spread(inner) => Ok(Expr::Spread(Box::new(self.explode(inner)?))),
            Expr::Member {
                object,
                property,
                offset,
            } => Ok(Expr::Member {
                object: Box::new(self.explode(object)?),
                property: property.clone(),
                offset: *offset,
            }),
            Expr::ComputedMember {
                object,
                property,
                offset,
            } => {
                let mut object = self.explode(object)?;
                if expr_has_suspend(property) {
                    object = self.spill(object);
                }
                Ok(Expr::ComputedMember {
                    object: Box::new(object),
                    property: Box::new(self.explode(property)?),
                    offset: *offset,
                })
            }
            Expr::Call {
                callee,
                arguments,
                offset,
            } => {
                let callee = self.explode_callee(callee, arguments.iter().any(expr_has_suspend))?;
                let arguments = self.explode_list(arguments)?;
                Ok(Expr::Call {
                    callee: Box::new(callee),
                    arguments,
                    offset: *offset,
                })
            }
            Expr::New {
                constructor,
                arguments,
                offset,
            } => {
                let mut constructor = self.explode(constructor)?;
                if arguments.iter().any(expr_has_suspend) {
                    constructor = self.spill(constructor);
                }
                let arguments = self.explode_list(arguments)?;
                Ok(Expr::New {
                    constructor: Box::new(constructor),
                    arguments,
                    offset: *offset,
                })
            }
            Expr::Array(elements) => Ok(Expr::Array(self.explode_list(elements)?)),
            Expr::Sequence(expressions) => {
                let mut residual = Expr::Literal(JsValue::Undefined);
                let last = expressions.len().saturating_sub(1);
                for (index, expression) in expressions.iter().enumerate() {
                    let value = self.explode(expression)?;
                    if index == last {
                        residual = value;
                    } else if !is_hidden(&value) {
                        self.emit(Instr::Eval(Rc::new(value)));
                    }
                }
                Ok(residual)
            }
            Expr::Object(properties) => {
                let mut exploded = Vec::with_capacity(properties.len());
                let last_suspending = properties
                    .iter()
                    .rposition(|property| expr_has_suspend(&property.value))
                    .unwrap_or(0);
                for (index, property) in properties.iter().enumerate() {
                    if matches!(&property.key, PropertyKey::Computed(key) if expr_has_suspend(key))
                    {
                        return Err(unsupported("a computed property key", 0));
                    }
                    let mut value = self.explode(&property.value)?;
                    if index < last_suspending && property.accessor.is_none() && !property.method {
                        value = self.spill(value);
                    }
                    exploded.push(ObjectProperty {
                        key: property.key.clone(),
                        value,
                        accessor: property.accessor,
                        shorthand: false,
                        method: property.method,
                    });
                }
                Ok(Expr::Object(exploded))
            }
            Expr::Assignment {
                target,
                value,
                offset,
            } => {
                let target = self.explode_target(target, expr_has_suspend(value))?;
                let value = self.explode(value)?;
                Ok(Expr::Assignment {
                    target: Box::new(target),
                    value: Box::new(value),
                    offset: *offset,
                })
            }
            Expr::CompoundAssignment {
                target,
                operator,
                value,
                offset,
            } => {
                // `x += await y` reads `x` before the await.
                let target = self.explode_target(target, true)?;
                let old = self.spill(target.clone());
                let value = self.explode(value)?;
                Ok(Expr::Assignment {
                    target: Box::new(target),
                    value: Box::new(Expr::Binary {
                        operator: *operator,
                        left: Box::new(old),
                        right: Box::new(value),
                        offset: *offset,
                    }),
                    offset: *offset,
                })
            }
            Expr::TaggedTemplate {
                tag,
                quasis,
                expressions,
                offset,
            } => {
                let tag = self.explode_callee(tag, true)?;
                let expressions = self.explode_list(expressions)?;
                Ok(Expr::TaggedTemplate {
                    tag: Box::new(tag),
                    quasis: quasis.clone(),
                    expressions,
                    offset: *offset,
                })
            }
            Expr::Update { offset, .. } => Err(unsupported("an update expression", *offset)),
            Expr::LogicalAssignment { offset, .. } => {
                Err(unsupported("a logical assignment", *offset))
            }
            Expr::OptionalChain(_) | Expr::OptionalGuard(_) => {
                Err(unsupported("an optional chain", 0))
            }
            Expr::SuperCall { offset, .. } | Expr::SuperMember { offset, .. } => {
                Err(unsupported("a `super` expression", *offset))
            }
            Expr::SuperComputedMember { offset, .. } => {
                Err(unsupported("a `super` expression", *offset))
            }
            Expr::PrivateMember { offset, .. } | Expr::PrivateIn { offset, .. } => {
                Err(unsupported("a private-name expression", *offset))
            }
            Expr::Class { offset, .. } => Err(unsupported("a class heritage", *offset)),
            Expr::Literal(_)
            | Expr::RegexLiteral { .. }
            | Expr::This
            | Expr::Identifier(_)
            | Expr::Function { .. }
            | Expr::Arrow { .. }
            | Expr::NewTarget => unreachable!("these contain no suspension point"),
        }
    }

    /// Explode a call's callee, keeping a method call's receiver intact.
    fn explode_callee(&mut self, callee: &Expr, later_suspends: bool) -> Result<Expr, JsError> {
        match callee {
            Expr::Member {
                object,
                property,
                offset,
            } => {
                let mut object = self.explode(object)?;
                if later_suspends {
                    object = self.spill(object);
                }
                Ok(Expr::Member {
                    object: Box::new(object),
                    property: property.clone(),
                    offset: *offset,
                })
            }
            Expr::ComputedMember {
                object,
                property,
                offset,
            } => {
                let mut object = self.explode(object)?;
                if later_suspends || expr_has_suspend(property) {
                    object = self.spill(object);
                }
                let property = self.explode(property)?;
                let property = if later_suspends {
                    self.spill(property)
                } else {
                    property
                };
                Ok(Expr::ComputedMember {
                    object: Box::new(object),
                    property: Box::new(property),
                    offset: *offset,
                })
            }
            other => self.explode(other),
        }
    }

    /// Explode the target of an assignment so its object and key are fixed
    /// before the right-hand side runs.
    fn explode_target(&mut self, target: &Expr, value_suspends: bool) -> Result<Expr, JsError> {
        match target {
            Expr::Member {
                object,
                property,
                offset,
            } => {
                let mut object = self.explode(object)?;
                if value_suspends {
                    object = self.spill(object);
                }
                Ok(Expr::Member {
                    object: Box::new(object),
                    property: property.clone(),
                    offset: *offset,
                })
            }
            Expr::ComputedMember {
                object,
                property,
                offset,
            } => {
                let mut object = self.explode(object)?;
                let mut property = self.explode(property)?;
                if value_suspends {
                    object = self.spill(object);
                    property = self.spill(property);
                }
                Ok(Expr::ComputedMember {
                    object: Box::new(object),
                    property: Box::new(property),
                    offset: *offset,
                })
            }
            other => Ok(other.clone()),
        }
    }

    /// Explode an argument or element list, spilling each earlier item when a
    /// later one suspends.
    fn explode_list(&mut self, items: &[Expr]) -> Result<Vec<Expr>, JsError> {
        let last_suspending = items.iter().rposition(expr_has_suspend);
        let mut exploded = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            let mut value = self.explode(item)?;
            if last_suspending.is_some_and(|last| index < last) {
                value = match value {
                    Expr::Spread(inner) => Expr::Spread(Box::new(self.spill(*inner))),
                    other => self.spill(other),
                };
            }
            exploded.push(value);
        }
        Ok(exploded)
    }
}
