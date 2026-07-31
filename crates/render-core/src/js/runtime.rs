use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use super::parser::{BinaryOp, CatchClause, Expr, Statement, UnaryOp, VariableKind};
use super::value::{NativeFunction, ObjectHost};
use super::{
    JsError, JsErrorKind, JsObject, JsValue, ObjectId, Realm, RuntimeLimits, ScriptOutcome,
};
use crate::dom::{Dom, DomError, NodeId, NodeKind};

#[derive(Clone, Debug)]
struct Binding {
    value: JsValue,
    mutable: bool,
    initialized: bool,
    kind: VariableKind,
}

#[derive(Clone, Copy, Debug)]
struct GlobalBinding {
    mutable: bool,
    initialized: bool,
    kind: VariableKind,
}

#[derive(Debug, Default)]
struct EnvironmentRecord {
    bindings: BTreeMap<String, Binding>,
    function_scope: bool,
}

type Environment = Rc<RefCell<EnvironmentRecord>>;

#[derive(Clone, Debug)]
struct UserFunction {
    parameters: Vec<String>,
    body: Vec<Statement>,
    captured_environment: Vec<Environment>,
    lexical_this: Option<JsValue>,
}

#[derive(Clone, Debug)]
enum PromiseState {
    Pending,
    Fulfilled(JsValue),
    Rejected(JsValue),
}

#[derive(Clone, Debug)]
struct PromiseReaction {
    on_fulfilled: Option<ObjectId>,
    on_rejected: Option<ObjectId>,
    result_promise: usize,
}

#[derive(Clone, Debug)]
struct PromiseRecord {
    state: PromiseState,
    reactions: Vec<PromiseReaction>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum JsMicrotask {
    Callback(ObjectId),
    PromiseReaction {
        handler: Option<ObjectId>,
        argument: JsValue,
        fulfilled: bool,
        result_promise: usize,
    },
}

#[derive(Clone, Debug)]
enum Completion {
    Normal(JsValue),
    Return(JsValue),
    Break,
    Continue,
}

#[derive(Clone, Debug)]
enum AssignmentReference {
    Binding(String),
    Property { object: ObjectId, property: String },
}

/// A realm-owning interpreter instance. DOM wrappers retain stable `NodeId`
/// identities, never Rust references, across calls to [`Self::execute`].
#[derive(Debug)]
pub struct JsRuntime {
    realm: Realm,
    limits: RuntimeLimits,
    steps_remaining: usize,
    calls_active: usize,
    dom_nodes_created: usize,
    this_stack: Vec<JsValue>,
    environment: Vec<Environment>,
    functions: Vec<UserFunction>,
    promises: Vec<PromiseRecord>,
    pending_microtasks: Vec<JsMicrotask>,
    global_bindings: BTreeMap<String, GlobalBinding>,
}

impl JsRuntime {
    #[must_use]
    pub fn new(dom: &Dom) -> Self {
        Self::with_limits(dom, RuntimeLimits::default())
    }

    #[must_use]
    pub fn with_limits(dom: &Dom, limits: RuntimeLimits) -> Self {
        Self {
            realm: Realm::bootstrap(dom.document()),
            steps_remaining: limits.max_execution_steps,
            calls_active: 0,
            dom_nodes_created: 0,
            this_stack: Vec::new(),
            environment: Vec::new(),
            functions: Vec::new(),
            promises: Vec::new(),
            pending_microtasks: Vec::new(),
            global_bindings: BTreeMap::new(),
            limits,
        }
    }

    #[must_use]
    pub const fn realm(&self) -> &Realm {
        &self.realm
    }

    /// Drain callbacks registered through `queueMicrotask()` in FIFO order.
    /// The embedding page owns scheduling; the runtime only retains callable
    /// identities from this realm.
    pub fn take_pending_microtasks(&mut self) -> Vec<JsMicrotask> {
        std::mem::take(&mut self.pending_microtasks)
    }

    /// Invoke one callable retained by the embedding page.
    ///
    /// # Errors
    ///
    /// Returns the same typed runtime or resource-limit errors as an ordinary
    /// JavaScript call.
    pub fn invoke_microtask(
        &mut self,
        dom: &mut Dom,
        microtask: JsMicrotask,
    ) -> Result<JsValue, JsError> {
        self.steps_remaining = self.limits.max_execution_steps;
        self.calls_active = 0;
        self.dom_nodes_created = 0;
        self.this_stack.clear();
        self.environment.clear();
        match microtask {
            JsMicrotask::Callback(callback) => self.call(dom, callback, &[]),
            JsMicrotask::PromiseReaction {
                handler,
                argument,
                fulfilled,
                result_promise,
            } => {
                let outcome = match handler {
                    Some(handler) => self.call(dom, handler, std::slice::from_ref(&argument)),
                    None if fulfilled => Ok(argument),
                    None => Err(JsError::thrown(argument)),
                };
                match outcome {
                    Ok(value) => self.resolve_promise_value(result_promise, &value)?,
                    Err(error) if error.kind() != JsErrorKind::ResourceLimit => {
                        let reason = error
                            .thrown_value()
                            .cloned()
                            .unwrap_or_else(|| JsValue::String(error.to_string()));
                        self.reject_promise(result_promise, &reason);
                    }
                    Err(error) => return Err(error),
                }
                Ok(JsValue::Undefined)
            }
        }
    }

    /// Parse and run one script against the existing DOM arena.
    ///
    /// # Errors
    ///
    /// Returns a typed syntax/runtime/DOM/resource-limit error. Unsupported
    /// syntax is never silently ignored.
    pub fn execute(&mut self, dom: &mut Dom, source: &str) -> Result<ScriptOutcome, JsError> {
        let script = super::CompiledScript::compile(source, &self.limits)?;
        self.execute_compiled(dom, &script)
    }

    /// Run a previously compiled script in this runtime's Realm.
    ///
    /// # Errors
    ///
    /// Returns typed declaration-instantiation, runtime, DOM, or resource-limit
    /// errors. Every invocation receives a fresh execution budget while Realm
    /// globals and heap identity intentionally persist between classic scripts.
    pub fn execute_compiled(
        &mut self,
        dom: &mut Dom,
        script: &super::CompiledScript,
    ) -> Result<ScriptOutcome, JsError> {
        let from_revision = dom.revision();
        self.steps_remaining = self.limits.max_execution_steps;
        self.calls_active = 0;
        self.dom_nodes_created = 0;
        self.this_stack.clear();
        self.environment.clear();
        self.instantiate_statements(&script.statements)?;
        let completion = self.evaluate_statements(dom, &script.statements)?;
        let value = match completion {
            Completion::Normal(value) => value,
            Completion::Return(_) | Completion::Break | Completion::Continue => {
                return Err(JsError::syntax(
                    "abrupt completion escaped the script body",
                    0,
                ));
            }
        };
        Ok(ScriptOutcome {
            value,
            from_revision,
            to_revision: dom.revision(),
        })
    }

    fn evaluate_statements(
        &mut self,
        dom: &mut Dom,
        statements: &[Statement],
    ) -> Result<Completion, JsError> {
        let mut value = JsValue::Undefined;
        for statement in statements {
            match self.evaluate_statement(dom, statement)? {
                Completion::Normal(next) => value = next,
                abrupt @ (Completion::Return(_) | Completion::Break | Completion::Continue) => {
                    return Ok(abrupt);
                }
            }
        }
        Ok(Completion::Normal(value))
    }

    fn instantiate_statements(&mut self, statements: &[Statement]) -> Result<(), JsError> {
        let mut lexical_declarations = BTreeMap::new();
        let mut var_names = BTreeSet::new();
        let mut functions = Vec::new();
        for statement in statements {
            match statement {
                Statement::Variable {
                    kind: kind @ (VariableKind::Let | VariableKind::Const),
                    name,
                    ..
                } => {
                    if lexical_declarations.insert(name.clone(), *kind).is_some() {
                        return Err(JsError::syntax(
                            format!("binding {name:?} is declared more than once"),
                            0,
                        ));
                    }
                }
                Statement::VariableList { kind, declarations } => {
                    for (name, _) in declarations {
                        if *kind != VariableKind::Var
                            && lexical_declarations.insert(name.clone(), *kind).is_some()
                        {
                            return Err(JsError::syntax(
                                format!("binding {name:?} is declared more than once"),
                                0,
                            ));
                        }
                    }
                }
                Statement::Function {
                    name,
                    parameters,
                    body,
                } => {
                    var_names.insert(name.clone());
                    functions.push((name, parameters, body));
                }
                _ => collect_var_names(statement, &mut var_names),
            }
        }
        if let Some(name) = lexical_declarations
            .keys()
            .find(|name| var_names.contains(*name))
        {
            return Err(JsError::syntax(
                format!("binding {name:?} conflicts with a var declaration"),
                0,
            ));
        }

        for (name, kind) in lexical_declarations {
            self.create_binding(&name, kind, false, JsValue::Undefined)?;
        }
        for name in var_names {
            self.create_binding(&name, VariableKind::Var, true, JsValue::Undefined)?;
        }
        for (name, parameters, body) in functions {
            let value = self.create_user_function(parameters, body)?;
            self.initialize_binding(name, value, VariableKind::Var)?;
        }
        Ok(())
    }

    fn instantiate_block_lexicals(&mut self, statements: &[Statement]) -> Result<(), JsError> {
        let mut declarations = BTreeMap::new();
        let mut functions = Vec::new();
        for statement in statements {
            match statement {
                Statement::VariableList {
                    kind,
                    declarations: variables,
                } => {
                    for (name, _) in variables {
                        if *kind != VariableKind::Var
                            && declarations.insert(name.clone(), *kind).is_some()
                        {
                            return Err(JsError::syntax(
                                format!("binding {name:?} is declared more than once"),
                                0,
                            ));
                        }
                    }
                }
                Statement::Variable {
                    kind: kind @ (VariableKind::Let | VariableKind::Const),
                    name,
                    ..
                } if declarations.insert(name.clone(), *kind).is_some() => {
                    return Err(JsError::syntax(
                        format!("binding {name:?} is declared more than once"),
                        0,
                    ));
                }
                Statement::Function {
                    name,
                    parameters,
                    body,
                } => {
                    if declarations
                        .insert(name.clone(), VariableKind::Const)
                        .is_some()
                    {
                        return Err(JsError::syntax(
                            format!("binding {name:?} is declared more than once"),
                            0,
                        ));
                    }
                    functions.push((name, parameters, body));
                }
                _ => {}
            }
        }
        for (name, kind) in declarations {
            self.create_binding(&name, kind, false, JsValue::Undefined)?;
        }
        for (name, parameters, body) in functions {
            let value = self.create_user_function(parameters, body)?;
            self.initialize_binding(name, value, VariableKind::Const)?;
        }
        Ok(())
    }

    fn create_user_function(
        &mut self,
        parameters: &[String],
        body: &[Statement],
    ) -> Result<JsValue, JsError> {
        self.create_function(parameters, body, None)
    }

    fn create_arrow_function(
        &mut self,
        parameters: &[String],
        body: &[Statement],
    ) -> Result<JsValue, JsError> {
        let lexical_this = self
            .this_stack
            .last()
            .cloned()
            .unwrap_or(JsValue::Undefined);
        self.create_function(parameters, body, Some(lexical_this))
    }

    fn create_function(
        &mut self,
        parameters: &[String],
        body: &[Statement],
        lexical_this: Option<JsValue>,
    ) -> Result<JsValue, JsError> {
        let is_arrow = lexical_this.is_some();
        self.ensure_heap_capacity(if is_arrow { 1 } else { 2 })?;
        let function_index = self.functions.len();
        self.functions.push(UserFunction {
            parameters: parameters.to_vec(),
            body: body.to_vec(),
            captured_environment: self.environment.clone(),
            lexical_this,
        });
        let function = if is_arrow {
            self.realm.arrow_function(function_index)
        } else {
            self.realm.user_function(function_index)
        };
        Ok(JsValue::Object(function))
    }

    fn evaluate_statement(
        &mut self,
        dom: &mut Dom,
        statement: &Statement,
    ) -> Result<Completion, JsError> {
        self.consume_step()?;
        match statement {
            Statement::Variable { kind, name, value } => {
                if *kind == VariableKind::Var && value.is_none() {
                    return Ok(Completion::Normal(JsValue::Undefined));
                }
                let value = match value {
                    Some(expression) => self.evaluate(dom, expression)?,
                    None => JsValue::Undefined,
                };
                self.initialize_binding(name, value.clone(), *kind)?;
                Ok(Completion::Normal(value))
            }
            Statement::VariableList { kind, declarations } => {
                let mut value = JsValue::Undefined;
                for (name, expression) in declarations {
                    if let Some(expression) = expression {
                        value = self.evaluate(dom, expression)?;
                        self.initialize_binding(name, value.clone(), *kind)?;
                    }
                }
                Ok(Completion::Normal(value))
            }
            Statement::Function { name, .. } => self.lookup_binding(name).map(Completion::Normal),
            Statement::Return(value) => {
                let value = match value {
                    Some(expression) => self.evaluate(dom, expression)?,
                    None => JsValue::Undefined,
                };
                Ok(Completion::Return(value))
            }
            Statement::Throw(expression) => {
                let value = self.evaluate(dom, expression)?;
                Err(JsError::thrown(value))
            }
            Statement::Try {
                body,
                catch,
                finally,
            } => self.evaluate_try_statement(dom, body, catch.as_ref(), finally.as_deref()),
            Statement::If {
                condition,
                consequent,
                alternate,
            } => {
                if self.evaluate(dom, condition)?.is_truthy() {
                    self.evaluate_statement(dom, consequent)
                } else if let Some(alternate) = alternate {
                    self.evaluate_statement(dom, alternate)
                } else {
                    Ok(Completion::Normal(JsValue::Undefined))
                }
            }
            Statement::Switch { expression, cases } => {
                self.evaluate_switch_statement(dom, expression, cases)
            }
            Statement::While { condition, body } => {
                let mut value = JsValue::Undefined;
                loop {
                    self.consume_step()?;
                    if !self.evaluate(dom, condition)?.is_truthy() {
                        break;
                    }
                    match self.evaluate_statement(dom, body)? {
                        Completion::Normal(next) => value = next,
                        Completion::Continue => {}
                        Completion::Break => break,
                        returned @ Completion::Return(_) => return Ok(returned),
                    }
                }
                Ok(Completion::Normal(value))
            }
            Statement::For {
                initializer,
                condition,
                update,
                body,
            } => self.evaluate_for_statement(
                dom,
                initializer.as_deref(),
                condition.as_ref(),
                update.as_ref(),
                body,
            ),
            Statement::Break => Ok(Completion::Break),
            Statement::Continue => Ok(Completion::Continue),
            Statement::Block(statements) => self.evaluate_scoped_statements(dom, statements),
            Statement::Expression(expression) => {
                self.evaluate(dom, expression).map(Completion::Normal)
            }
        }
    }

    fn evaluate_switch_statement(
        &mut self,
        dom: &mut Dom,
        expression: &Expr,
        cases: &[(Option<Expr>, Vec<Statement>)],
    ) -> Result<Completion, JsError> {
        let discriminant = self.evaluate(dom, expression)?;
        let mut active = false;
        let mut value = JsValue::Undefined;
        for (test, statements) in cases {
            if !active {
                active = match test {
                    Some(test) => strict_equal(&discriminant, &self.evaluate(dom, test)?),
                    None => true,
                };
            }
            if !active {
                continue;
            }
            match self.evaluate_statements(dom, statements)? {
                Completion::Normal(next) => value = next,
                Completion::Break => break,
                abrupt => return Ok(abrupt),
            }
        }
        Ok(Completion::Normal(value))
    }

    fn evaluate_for_statement(
        &mut self,
        dom: &mut Dom,
        initializer: Option<&Statement>,
        condition: Option<&Expr>,
        update: Option<&Expr>,
        body: &Statement,
    ) -> Result<Completion, JsError> {
        self.environment
            .push(Rc::new(RefCell::new(EnvironmentRecord::default())));
        let result = (|| {
            if let Some(initializer) = initializer {
                if let Statement::Variable { kind, name, .. } = initializer {
                    self.create_binding(
                        name,
                        *kind,
                        *kind == VariableKind::Var,
                        JsValue::Undefined,
                    )?;
                }
                match self.evaluate_statement(dom, initializer)? {
                    Completion::Normal(_) => {}
                    abrupt => return Ok(abrupt),
                }
            }
            let mut value = JsValue::Undefined;
            loop {
                self.consume_step()?;
                if let Some(condition) = condition
                    && !self.evaluate(dom, condition)?.is_truthy()
                {
                    break;
                }
                match self.evaluate_statement(dom, body)? {
                    Completion::Normal(next) => value = next,
                    Completion::Continue => {}
                    Completion::Break => break,
                    returned @ Completion::Return(_) => return Ok(returned),
                }
                if let Some(update) = update {
                    self.evaluate(dom, update)?;
                }
            }
            Ok(Completion::Normal(value))
        })();
        self.environment.pop();
        result
    }

    fn evaluate_try_statement(
        &mut self,
        dom: &mut Dom,
        body: &[Statement],
        catch: Option<&CatchClause>,
        finally: Option<&[Statement]>,
    ) -> Result<Completion, JsError> {
        let mut result = self.evaluate_scoped_statements(dom, body);
        if let Err(error) = &result
            && error.kind() != JsErrorKind::ResourceLimit
            && let Some(catch) = catch
        {
            let value = error
                .thrown_value()
                .cloned()
                .unwrap_or_else(|| JsValue::String(error.to_string()));
            let catch_environment = Rc::new(RefCell::new(EnvironmentRecord::default()));
            catch_environment.borrow_mut().bindings.insert(
                catch.parameter.clone(),
                Binding {
                    value,
                    mutable: true,
                    initialized: true,
                    kind: VariableKind::Let,
                },
            );
            self.environment.push(catch_environment);
            result = self
                .instantiate_block_lexicals(&catch.body)
                .and_then(|()| self.evaluate_statements(dom, &catch.body));
            self.environment.pop();
        }
        if let Some(finally) = finally {
            match self.evaluate_scoped_statements(dom, finally) {
                Ok(Completion::Normal(_)) => {}
                abrupt => return abrupt,
            }
        }
        result
    }

    fn evaluate_scoped_statements(
        &mut self,
        dom: &mut Dom,
        statements: &[Statement],
    ) -> Result<Completion, JsError> {
        self.environment
            .push(Rc::new(RefCell::new(EnvironmentRecord::default())));
        let result = self
            .instantiate_block_lexicals(statements)
            .and_then(|()| self.evaluate_statements(dom, statements));
        self.environment.pop();
        result
    }

    fn evaluate(&mut self, dom: &mut Dom, expression: &Expr) -> Result<JsValue, JsError> {
        self.consume_step()?;
        match expression {
            Expr::Literal(value) => Ok(value.clone()),
            Expr::This => Ok(self
                .this_stack
                .last()
                .cloned()
                .unwrap_or(JsValue::Undefined)),
            Expr::Identifier(name) => self.lookup_binding(name),
            Expr::Function {
                name,
                parameters,
                body,
            } => self.evaluate_function_expression(name.as_deref(), parameters, body),
            Expr::Arrow { parameters, body } => self.create_arrow_function(parameters, body),
            Expr::Object(properties) => self.evaluate_object_literal(dom, properties),
            Expr::Array(elements) => self.evaluate_array_literal(dom, elements),
            Expr::Unary { operator, operand } => {
                let value = self.evaluate(dom, operand)?;
                Self::evaluate_unary(*operator, &value)
            }
            Expr::Binary {
                operator,
                left,
                right,
            } => self.evaluate_binary(dom, *operator, left, right),
            Expr::Conditional {
                condition,
                consequent,
                alternate,
            } => {
                if self.evaluate(dom, condition)?.is_truthy() {
                    self.evaluate(dom, consequent)
                } else {
                    self.evaluate(dom, alternate)
                }
            }
            Expr::Update {
                target,
                operator,
                prefix,
            } => {
                let reference = self.resolve_assignment_reference(dom, target)?;
                let previous = self.read_assignment_reference(dom, &reference)?;
                let next =
                    Self::evaluate_binary_values(*operator, &previous, &JsValue::Number(1.0))?;
                self.write_assignment_reference(dom, &reference, next.clone())?;
                Ok(if *prefix { next } else { previous })
            }
            Expr::Member { object, property } => {
                let evaluated = self.evaluate(dom, object)?;
                let object = Self::require_object(&evaluated)?;
                self.get_member(dom, object, property)
            }
            Expr::ComputedMember { object, property } => {
                let evaluated = self.evaluate(dom, object)?;
                let object = Self::require_object(&evaluated)?;
                let property = self.evaluate(dom, property)?.to_js_string();
                self.get_member(dom, object, &property)
            }
            Expr::New {
                constructor,
                arguments,
            } => {
                let evaluated = self.evaluate(dom, constructor)?;
                let constructor = Self::require_object(&evaluated)?;
                let mut values = Vec::with_capacity(arguments.len());
                for argument in arguments {
                    values.push(self.evaluate(dom, argument)?);
                }
                self.construct(dom, constructor, &values)
            }
            Expr::Call { callee, arguments } => self.evaluate_call(dom, callee, arguments),
            Expr::CompoundAssignment {
                target,
                operator,
                value,
            } => {
                let reference = self.resolve_assignment_reference(dom, target)?;
                let current = self.read_assignment_reference(dom, &reference)?;
                let right = self.evaluate(dom, value)?;
                let combined = Self::evaluate_binary_values(*operator, &current, &right)?;
                self.write_assignment_reference(dom, &reference, combined.clone())?;
                Ok(combined)
            }
            Expr::Assignment { target, value } => {
                let reference = self.resolve_assignment_reference(dom, target)?;
                let value = self.evaluate(dom, value)?;
                self.write_assignment_reference(dom, &reference, value.clone())?;
                Ok(value)
            }
        }
    }

    fn resolve_assignment_reference(
        &mut self,
        dom: &mut Dom,
        target: &Expr,
    ) -> Result<AssignmentReference, JsError> {
        match target {
            Expr::Identifier(name) => Ok(AssignmentReference::Binding(name.clone())),
            Expr::Member { object, property } => {
                let evaluated = self.evaluate(dom, object)?;
                let object = Self::require_object(&evaluated)?;
                Ok(AssignmentReference::Property {
                    object,
                    property: property.clone(),
                })
            }
            Expr::ComputedMember { object, property } => {
                let evaluated = self.evaluate(dom, object)?;
                let object = Self::require_object(&evaluated)?;
                let property = self.evaluate(dom, property)?.to_js_string();
                Ok(AssignmentReference::Property { object, property })
            }
            _ => Err(JsError::syntax("invalid assignment target", 0)),
        }
    }

    fn read_assignment_reference(
        &mut self,
        dom: &Dom,
        reference: &AssignmentReference,
    ) -> Result<JsValue, JsError> {
        match reference {
            AssignmentReference::Binding(name) => self.lookup_binding(name),
            AssignmentReference::Property { object, property } => {
                self.get_member(dom, *object, property)
            }
        }
    }

    fn write_assignment_reference(
        &mut self,
        dom: &mut Dom,
        reference: &AssignmentReference,
        value: JsValue,
    ) -> Result<(), JsError> {
        match reference {
            AssignmentReference::Binding(name) => self.assign_binding(name, value),
            AssignmentReference::Property { object, property } => {
                self.set_member(dom, *object, property, value)
            }
        }
    }

    fn evaluate_binary_values(
        operator: BinaryOp,
        left: &JsValue,
        right: &JsValue,
    ) -> Result<JsValue, JsError> {
        match operator {
            BinaryOp::Add => {
                if matches!(left, JsValue::String(_)) || matches!(right, JsValue::String(_)) {
                    Ok(JsValue::String(format!(
                        "{}{}",
                        left.to_js_string(),
                        right.to_js_string()
                    )))
                } else {
                    Ok(JsValue::Number(to_number(left)? + to_number(right)?))
                }
            }
            BinaryOp::Subtract => Ok(JsValue::Number(to_number(left)? - to_number(right)?)),
            _ => Err(JsError::type_error(
                "unsupported compound assignment operator",
            )),
        }
    }

    fn evaluate_call(
        &mut self,
        dom: &mut Dom,
        callee: &Expr,
        arguments: &[Expr],
    ) -> Result<JsValue, JsError> {
        let (callee, receiver) = match callee {
            Expr::Member { object, property } => {
                let receiver = self.evaluate(dom, object)?;
                let object = Self::require_object(&receiver)?;
                (self.get_member(dom, object, property)?, receiver)
            }
            Expr::ComputedMember { object, property } => {
                let receiver = self.evaluate(dom, object)?;
                let object = Self::require_object(&receiver)?;
                let property = self.evaluate(dom, property)?.to_js_string();
                (self.get_member(dom, object, &property)?, receiver)
            }
            _ => (self.evaluate(dom, callee)?, JsValue::Undefined),
        };
        let callee = Self::require_object(&callee)?;
        let mut values = Vec::with_capacity(arguments.len());
        for argument in arguments {
            values.push(self.evaluate(dom, argument)?);
        }
        self.call_with_this(dom, callee, &values, receiver)
    }

    fn evaluate_function_expression(
        &mut self,
        name: Option<&str>,
        parameters: &[String],
        body: &[Statement],
    ) -> Result<JsValue, JsError> {
        let Some(name) = name else {
            return self.create_user_function(parameters, body);
        };
        self.environment
            .push(Rc::new(RefCell::new(EnvironmentRecord::default())));
        let result = (|| {
            self.create_binding(name, VariableKind::Const, false, JsValue::Undefined)?;
            let value = self.create_user_function(parameters, body)?;
            self.initialize_binding(name, value.clone(), VariableKind::Const)?;
            Ok(value)
        })();
        self.environment.pop();
        result
    }

    fn evaluate_object_literal(
        &mut self,
        dom: &mut Dom,
        properties: &[(String, Expr)],
    ) -> Result<JsValue, JsError> {
        self.ensure_heap_capacity(1)?;
        let object = self.realm.create_object(None);
        for (key, expression) in properties {
            let value = self.evaluate(dom, expression)?;
            if !self.realm.set_property(object, key.clone(), value) {
                return Err(JsError::type_error("could not define object property"));
            }
        }
        Ok(JsValue::Object(object))
    }

    fn evaluate_array_literal(
        &mut self,
        dom: &mut Dom,
        elements: &[Expr],
    ) -> Result<JsValue, JsError> {
        self.ensure_heap_capacity(1)?;
        let object = self.realm.create_array();
        for (index, expression) in elements.iter().enumerate() {
            let value = self.evaluate(dom, expression)?;
            if !self.realm.set_property(object, index.to_string(), value) {
                return Err(JsError::type_error("could not define array element"));
            }
        }
        let length = u32::try_from(elements.len())
            .map_err(|_| JsError::resource("array length exceeds the supported u32 range"))?;
        if !self.realm.set_property(
            object,
            "length".to_owned(),
            JsValue::Number(f64::from(length)),
        ) {
            return Err(JsError::type_error("could not define array length"));
        }
        Ok(JsValue::Object(object))
    }

    fn evaluate_unary(operator: UnaryOp, value: &JsValue) -> Result<JsValue, JsError> {
        match operator {
            UnaryOp::Not => Ok(JsValue::Boolean(!value.is_truthy())),
            UnaryOp::Typeof => Ok(JsValue::String(
                match value {
                    JsValue::Undefined => "undefined",
                    JsValue::Null | JsValue::Object(_) => "object",
                    JsValue::Boolean(_) => "boolean",
                    JsValue::Number(_) => "number",
                    JsValue::String(_) => "string",
                }
                .to_owned(),
            )),
            UnaryOp::Plus => Ok(JsValue::Number(to_number(value)?)),
            UnaryOp::Minus => Ok(JsValue::Number(-to_number(value)?)),
        }
    }

    fn evaluate_binary(
        &mut self,
        dom: &mut Dom,
        operator: BinaryOp,
        left: &Expr,
        right: &Expr,
    ) -> Result<JsValue, JsError> {
        let left = self.evaluate(dom, left)?;
        if operator == BinaryOp::LogicalAnd && !left.is_truthy() {
            return Ok(left);
        }
        if operator == BinaryOp::LogicalOr && left.is_truthy() {
            return Ok(left);
        }
        let right = self.evaluate(dom, right)?;
        if operator == BinaryOp::Instanceof {
            return self.instanceof(&left, &right).map(JsValue::Boolean);
        }
        match operator {
            BinaryOp::LogicalAnd | BinaryOp::LogicalOr => Ok(right),
            BinaryOp::Add => {
                if matches!(left, JsValue::String(_)) || matches!(right, JsValue::String(_)) {
                    Ok(JsValue::String(format!(
                        "{}{}",
                        left.to_js_string(),
                        right.to_js_string()
                    )))
                } else {
                    Ok(JsValue::Number(to_number(&left)? + to_number(&right)?))
                }
            }
            BinaryOp::Subtract => Ok(JsValue::Number(to_number(&left)? - to_number(&right)?)),
            BinaryOp::Multiply => Ok(JsValue::Number(to_number(&left)? * to_number(&right)?)),
            BinaryOp::Divide => Ok(JsValue::Number(to_number(&left)? / to_number(&right)?)),
            BinaryOp::Remainder => Ok(JsValue::Number(to_number(&left)? % to_number(&right)?)),
            BinaryOp::Less => compare(&left, &right, |a, b| a < b, |a, b| a < b),
            BinaryOp::LessEqual => compare(&left, &right, |a, b| a <= b, |a, b| a <= b),
            BinaryOp::Greater => compare(&left, &right, |a, b| a > b, |a, b| a > b),
            BinaryOp::GreaterEqual => compare(&left, &right, |a, b| a >= b, |a, b| a >= b),
            BinaryOp::StrictEqual => Ok(JsValue::Boolean(strict_equal(&left, &right))),
            BinaryOp::StrictNotEqual => Ok(JsValue::Boolean(!strict_equal(&left, &right))),
            BinaryOp::Equal => Ok(JsValue::Boolean(abstract_equal(&left, &right)?)),
            BinaryOp::NotEqual => Ok(JsValue::Boolean(!abstract_equal(&left, &right)?)),
            BinaryOp::Instanceof => unreachable!("instanceof is handled before numeric operators"),
        }
    }

    fn instanceof(&self, value: &JsValue, constructor: &JsValue) -> Result<bool, JsError> {
        let constructor = Self::require_object(constructor)?;
        if !matches!(
            self.realm.host(constructor),
            Some(ObjectHost::UserFunction(_))
        ) {
            return Err(JsError::type_error(
                "right-hand side of instanceof is not callable",
            ));
        }
        let prototype = self
            .realm
            .get_property(constructor, "prototype")
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            })
            .ok_or_else(|| JsError::type_error("constructor prototype is not an object"))?;
        let JsValue::Object(object) = value else {
            return Ok(false);
        };
        let mut candidate = self.realm.object(*object).and_then(JsObject::prototype);
        let mut visited = 0_usize;
        while let Some(current) = candidate {
            if current == prototype {
                return Ok(true);
            }
            if visited >= self.limits.max_heap_objects {
                return Err(JsError::resource(
                    "prototype chain exceeds the heap object limit",
                ));
            }
            visited = visited.saturating_add(1);
            candidate = self.realm.object(current).and_then(JsObject::prototype);
        }
        Ok(false)
    }

    fn create_binding(
        &mut self,
        name: &str,
        kind: VariableKind,
        initialized: bool,
        value: JsValue,
    ) -> Result<(), JsError> {
        let mutable = kind != VariableKind::Const;
        if self.environment.is_empty() {
            if let Some(existing) = self.global_bindings.get(name) {
                if kind == VariableKind::Var && existing.kind == VariableKind::Var {
                    return Ok(());
                }
                return Err(JsError::syntax(
                    format!("global binding {name:?} is already declared"),
                    0,
                ));
            }
            if !self.realm.set_global(name.to_owned(), value) {
                return Err(JsError::type_error(format!(
                    "global property {name:?} is not writable"
                )));
            }
            self.global_bindings.insert(
                name.to_owned(),
                GlobalBinding {
                    mutable,
                    initialized,
                    kind,
                },
            );
            return Ok(());
        }
        let target_index = if kind == VariableKind::Var {
            self.environment
                .iter()
                .rposition(|scope| scope.borrow().function_scope)
                .unwrap_or_else(|| self.environment.len().saturating_sub(1))
        } else {
            self.environment.len().saturating_sub(1)
        };
        let target = self
            .environment
            .get(target_index)
            .expect("non-empty environment chain has a declaration target");
        let mut target = target.borrow_mut();
        if let Some(existing) = target.bindings.get(name) {
            if kind == VariableKind::Var && existing.kind == VariableKind::Var {
                return Ok(());
            }
            return Err(JsError::syntax(
                format!("binding {name:?} is already declared in this scope"),
                0,
            ));
        }
        target.bindings.insert(
            name.to_owned(),
            Binding {
                value,
                mutable,
                initialized,
                kind,
            },
        );
        Ok(())
    }

    fn initialize_binding(
        &mut self,
        name: &str,
        value: JsValue,
        kind: VariableKind,
    ) -> Result<(), JsError> {
        for scope in self.environment.iter().rev() {
            let mut scope = scope.borrow_mut();
            if let Some(binding) = scope.bindings.get_mut(name) {
                if binding.initialized && kind != VariableKind::Var {
                    return Err(JsError::syntax(
                        format!("binding {name:?} is already initialized"),
                        0,
                    ));
                }
                binding.value = value;
                binding.initialized = true;
                return Ok(());
            }
        }
        if let Some(binding) = self.global_bindings.get_mut(name) {
            if binding.initialized && kind != VariableKind::Var {
                return Err(JsError::syntax(
                    format!("global binding {name:?} is already initialized"),
                    0,
                ));
            }
            binding.initialized = true;
            if self.realm.set_global(name.to_owned(), value) {
                return Ok(());
            }
            return Err(JsError::type_error(format!(
                "global property {name:?} is not writable"
            )));
        }
        Err(JsError::reference(format!("{name} is not defined")))
    }

    fn lookup_binding(&self, name: &str) -> Result<JsValue, JsError> {
        for scope in self.environment.iter().rev() {
            if let Some(binding) = scope.borrow().bindings.get(name) {
                if !binding.initialized {
                    return Err(JsError::reference(format!(
                        "cannot access {name} before initialization"
                    )));
                }
                return Ok(binding.value.clone());
            }
        }
        if let Some(binding) = self.global_bindings.get(name)
            && !binding.initialized
        {
            return Err(JsError::reference(format!(
                "cannot access {name} before initialization"
            )));
        }
        self.realm
            .global(name)
            .ok_or_else(|| JsError::reference(format!("{name} is not defined")))
    }

    fn assign_binding(&mut self, name: &str, value: JsValue) -> Result<(), JsError> {
        for scope in self.environment.iter().rev() {
            let mut scope = scope.borrow_mut();
            if let Some(binding) = scope.bindings.get_mut(name) {
                if !binding.initialized {
                    return Err(JsError::reference(format!(
                        "cannot access {name} before initialization"
                    )));
                }
                if !binding.mutable {
                    return Err(JsError::type_error(format!(
                        "assignment to constant binding {name:?}"
                    )));
                }
                binding.value = value;
                return Ok(());
            }
        }
        if let Some(binding) = self.global_bindings.get(name) {
            if !binding.initialized {
                return Err(JsError::reference(format!(
                    "cannot access {name} before initialization"
                )));
            }
            if !binding.mutable {
                return Err(JsError::type_error(format!(
                    "assignment to constant binding {name:?}"
                )));
            }
        } else if self.realm.global(name).is_none() {
            return Err(JsError::reference(format!("{name} is not defined")));
        }
        if self.realm.set_global(name.to_owned(), value) {
            Ok(())
        } else {
            Err(JsError::type_error(format!(
                "global property {name:?} is not writable"
            )))
        }
    }

    fn get_member(
        &mut self,
        dom: &Dom,
        object: ObjectId,
        property: &str,
    ) -> Result<JsValue, JsError> {
        self.consume_step()?;
        if let Some(value) = self.realm.get_property(object, property) {
            return Ok(value);
        }
        let function = match (self.realm.host(object), property) {
            (Some(ObjectHost::Promise(_)), "then") => Some(NativeFunction::PromiseThen),
            (Some(ObjectHost::Promise(_)), "catch") => Some(NativeFunction::PromiseCatch),
            (Some(ObjectHost::Document(_)), "getElementById") => {
                Some(NativeFunction::GetElementById)
            }
            (Some(ObjectHost::Document(_)), "createElement") => Some(NativeFunction::CreateElement),
            (Some(ObjectHost::Node(_)), "setAttribute") => Some(NativeFunction::SetAttribute),
            (Some(ObjectHost::Node(_)), "appendChild") => Some(NativeFunction::AppendChild),
            (Some(ObjectHost::Node(node)), "textContent") => {
                return self.text_content(dom, node).map(JsValue::String);
            }
            _ => None,
        };
        if let Some(function) = function {
            self.ensure_heap_capacity(1)?;
            return Ok(JsValue::Object(self.realm.bound_function(function, object)));
        }
        Ok(JsValue::Undefined)
    }

    fn set_member(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        property: &str,
        value: JsValue,
    ) -> Result<(), JsError> {
        self.consume_step()?;
        if let (Some(ObjectHost::Node(node)), "textContent") = (self.realm.host(object), property) {
            return self.set_text_content(dom, node, value.to_js_string());
        }
        if !self.realm.set_property(object, property.to_owned(), value) {
            return Err(JsError::type_error(format!(
                "property {property:?} is not writable"
            )));
        }
        Ok(())
    }

    fn construct(
        &mut self,
        dom: &mut Dom,
        constructor: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match self.realm.host(constructor) {
            Some(ObjectHost::PromiseConstructor) => {
                let executor = Self::require_callable_object(
                    required_argument(arguments, 0, "Promise")?,
                    &self.realm,
                )?;
                let (promise, value) = self.create_promise()?;
                self.ensure_heap_capacity(2)?;
                let resolve = self.realm.promise_settler(promise, true);
                let reject = self.realm.promise_settler(promise, false);
                if let Err(error) = self.call(
                    dom,
                    executor,
                    &[JsValue::Object(resolve), JsValue::Object(reject)],
                ) {
                    if error.kind() == JsErrorKind::ResourceLimit {
                        return Err(error);
                    }
                    let reason = error
                        .thrown_value()
                        .cloned()
                        .unwrap_or_else(|| JsValue::String(error.to_string()));
                    self.reject_promise(promise, &reason);
                }
                Ok(value)
            }
            Some(ObjectHost::ArrowFunction(_)) => {
                Err(JsError::type_error("arrow function is not a constructor"))
            }
            Some(ObjectHost::UserFunction(index)) => {
                self.ensure_heap_capacity(1)?;
                let prototype = match self.realm.get_property(constructor, "prototype") {
                    Some(JsValue::Object(prototype)) => Some(prototype),
                    _ => None,
                };
                let instance = self.realm.create_object(prototype);
                let result = self.call_user(dom, index, arguments, JsValue::Object(instance))?;
                if matches!(result, JsValue::Object(_)) {
                    Ok(result)
                } else {
                    Ok(JsValue::Object(instance))
                }
            }
            _ => Err(JsError::type_error("value is not a constructor")),
        }
    }

    fn call(
        &mut self,
        dom: &mut Dom,
        callee: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        self.call_with_this(dom, callee, arguments, JsValue::Undefined)
    }

    fn call_with_this(
        &mut self,
        dom: &mut Dom,
        callee: ObjectId,
        arguments: &[JsValue],
        receiver: JsValue,
    ) -> Result<JsValue, JsError> {
        self.consume_step()?;
        if self.calls_active >= self.limits.max_call_depth {
            return Err(JsError::resource("maximum call depth exceeded"));
        }
        self.calls_active = self.calls_active.saturating_add(1);
        let result = match self.realm.host(callee) {
            Some(ObjectHost::NativeFunction(function)) => {
                let receiver = Self::require_object(&receiver)?;
                self.call_native(dom, function, receiver, arguments)
            }
            Some(ObjectHost::BoundFunction { function, receiver }) => {
                self.call_native(dom, function, receiver, arguments)
            }
            Some(ObjectHost::UserFunction(index) | ObjectHost::ArrowFunction(index)) => {
                self.call_user(dom, index, arguments, receiver)
            }
            Some(ObjectHost::PromiseSettler { promise, fulfilled }) => {
                let value = arguments.first().cloned().unwrap_or(JsValue::Undefined);
                if fulfilled {
                    self.resolve_promise_value(promise, &value)?;
                } else {
                    self.reject_promise(promise, &value);
                }
                Ok(JsValue::Undefined)
            }
            _ => Err(JsError::type_error("value is not callable")),
        };
        self.calls_active = self.calls_active.saturating_sub(1);
        result
    }

    fn call_user(
        &mut self,
        dom: &mut Dom,
        index: usize,
        arguments: &[JsValue],
        receiver: JsValue,
    ) -> Result<JsValue, JsError> {
        let function = self
            .functions
            .get(index)
            .cloned()
            .ok_or_else(|| JsError::type_error("function object refers to unknown code"))?;
        let previous_environment =
            std::mem::replace(&mut self.environment, function.captured_environment);
        let mut call_environment = EnvironmentRecord {
            function_scope: true,
            ..EnvironmentRecord::default()
        };
        for (index, parameter) in function.parameters.iter().enumerate() {
            call_environment.bindings.insert(
                parameter.clone(),
                Binding {
                    value: arguments.get(index).cloned().unwrap_or(JsValue::Undefined),
                    mutable: true,
                    initialized: true,
                    kind: VariableKind::Var,
                },
            );
        }
        self.environment
            .push(Rc::new(RefCell::new(call_environment)));
        self.this_stack
            .push(function.lexical_this.clone().unwrap_or(receiver));
        let result = self
            .instantiate_statements(&function.body)
            .and_then(|()| self.evaluate_statements(dom, &function.body));
        self.this_stack.pop();
        self.environment = previous_environment;
        match result? {
            Completion::Normal(_) => Ok(JsValue::Undefined),
            Completion::Return(value) => Ok(value),
            Completion::Break | Completion::Continue => {
                Err(JsError::syntax("loop control escaped a function body", 0))
            }
        }
    }

    fn call_native(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match function {
            NativeFunction::GetElementById => {
                self.require_document(receiver)?;
                let id = required_argument(arguments, 0, "getElementById")?.to_js_string();
                match self.find_element_by_id(dom, &id)? {
                    Some(node) => self.wrap_node(node),
                    None => Ok(JsValue::Null),
                }
            }
            NativeFunction::CreateElement => {
                self.require_document(receiver)?;
                let name = required_argument(arguments, 0, "createElement")?.to_js_string();
                if !valid_html_local_name(&name) {
                    return Err(JsError::dom(format!(
                        "{name:?} is not a supported HTML local name"
                    )));
                }
                if self.dom_nodes_created >= self.limits.max_dom_nodes_created {
                    return Err(JsError::resource("DOM node creation limit exceeded"));
                }
                self.ensure_heap_capacity(1)?;
                self.dom_nodes_created = self.dom_nodes_created.saturating_add(1);
                let node = dom.create_element(name);
                Ok(JsValue::Object(self.realm.node_wrapper(node)))
            }
            NativeFunction::SetAttribute => {
                let node = self.require_node(receiver)?;
                let name = required_argument(arguments, 0, "setAttribute")?.to_js_string();
                let value = required_argument(arguments, 1, "setAttribute")?.to_js_string();
                dom.set_attribute(node, name, value)?;
                Ok(JsValue::Undefined)
            }
            NativeFunction::AppendChild => {
                let parent = self.require_node(receiver)?;
                let child = self.value_as_node(required_argument(arguments, 0, "appendChild")?)?;
                dom.append_child(parent, child)?;
                self.wrap_node(child)
            }
            NativeFunction::QueueMicrotask => {
                let callback = Self::require_callable_object(
                    required_argument(arguments, 0, "queueMicrotask")?,
                    &self.realm,
                )?;
                self.pending_microtasks
                    .push(JsMicrotask::Callback(callback));
                Ok(JsValue::Undefined)
            }
            NativeFunction::PromiseResolve => {
                let value = arguments.first().cloned().unwrap_or(JsValue::Undefined);
                if let JsValue::Object(object) = value
                    && matches!(self.realm.host(object), Some(ObjectHost::Promise(_)))
                {
                    return Ok(JsValue::Object(object));
                }
                let (promise, result) = self.create_promise()?;
                self.resolve_promise(promise, &value);
                Ok(result)
            }
            NativeFunction::PromiseReject => {
                let reason = arguments.first().cloned().unwrap_or(JsValue::Undefined);
                let (promise, result) = self.create_promise()?;
                self.reject_promise(promise, &reason);
                Ok(result)
            }
            NativeFunction::PromiseThen => self.perform_promise_then(receiver, arguments),
            NativeFunction::PromiseCatch => {
                let handler = arguments.first().cloned().unwrap_or(JsValue::Undefined);
                self.perform_promise_then(receiver, &[JsValue::Undefined, handler])
            }
            NativeFunction::ArrayIsArray => Ok(JsValue::Boolean(matches!(
                arguments.first(),
                Some(JsValue::Object(object))
                    if matches!(self.realm.host(*object), Some(ObjectHost::Array))
            ))),
            NativeFunction::ArrayPush => self.array_push(receiver, arguments),
            NativeFunction::ArrayPop => self.array_pop(receiver),
        }
    }

    fn array_push(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        self.require_array(receiver)?;
        let mut length = self.array_length(receiver)?;
        for value in arguments {
            if !self
                .realm
                .set_property(receiver, length.to_string(), value.clone())
            {
                return Err(JsError::type_error("could not append array element"));
            }
            length = length
                .checked_add(1)
                .ok_or_else(|| JsError::resource("array length exceeds the supported u32 range"))?;
        }
        self.set_array_length(receiver, length)?;
        Ok(JsValue::Number(f64::from(length)))
    }

    fn array_pop(&mut self, receiver: ObjectId) -> Result<JsValue, JsError> {
        self.require_array(receiver)?;
        let length = self.array_length(receiver)?;
        if length == 0 {
            return Ok(JsValue::Undefined);
        }
        let index = length.saturating_sub(1);
        let value = self
            .realm
            .remove_property(receiver, &index.to_string())
            .unwrap_or(JsValue::Undefined);
        self.set_array_length(receiver, index)?;
        Ok(value)
    }

    fn require_array(&self, object: ObjectId) -> Result<(), JsError> {
        if matches!(self.realm.host(object), Some(ObjectHost::Array)) {
            Ok(())
        } else {
            Err(JsError::type_error("incompatible Array method receiver"))
        }
    }

    fn array_length(&self, object: ObjectId) -> Result<u32, JsError> {
        match self.realm.get_property(object, "length") {
            Some(JsValue::Number(length))
                if length.is_finite()
                    && length >= 0.0
                    && length <= f64::from(u32::MAX)
                    && length.fract() == 0.0 =>
            {
                length
                    .to_string()
                    .parse::<u32>()
                    .map_err(|_| JsError::type_error("array length is outside the u32 range"))
            }
            _ => Err(JsError::type_error(
                "array length is not a supported integer",
            )),
        }
    }

    fn set_array_length(&mut self, object: ObjectId, length: u32) -> Result<(), JsError> {
        if self.realm.set_property(
            object,
            "length".to_owned(),
            JsValue::Number(f64::from(length)),
        ) {
            Ok(())
        } else {
            Err(JsError::type_error("array length is not writable"))
        }
    }

    fn create_promise(&mut self) -> Result<(usize, JsValue), JsError> {
        self.ensure_heap_capacity(1)?;
        let index = self.promises.len();
        let object = self.realm.promise(index);
        self.promises.push(PromiseRecord {
            state: PromiseState::Pending,
            reactions: Vec::new(),
        });
        Ok((index, JsValue::Object(object)))
    }

    fn resolve_promise(&mut self, promise: usize, value: &JsValue) {
        self.settle_promise(promise, value, true);
    }

    fn resolve_promise_value(&mut self, promise: usize, value: &JsValue) -> Result<(), JsError> {
        if let JsValue::Object(object) = value
            && let Some(ObjectHost::Promise(source)) = self.realm.host(*object)
        {
            if source == promise {
                self.reject_promise(
                    promise,
                    &JsValue::String("a promise cannot resolve to itself".to_owned()),
                );
                return Ok(());
            }
            let state = self
                .promises
                .get(source)
                .map(|record| record.state.clone())
                .ok_or_else(|| JsError::type_error("Promise object refers to unknown state"))?;
            match state {
                PromiseState::Pending => self.promises[source].reactions.push(PromiseReaction {
                    on_fulfilled: None,
                    on_rejected: None,
                    result_promise: promise,
                }),
                PromiseState::Fulfilled(value) => self.resolve_promise(promise, &value),
                PromiseState::Rejected(reason) => self.reject_promise(promise, &reason),
            }
            return Ok(());
        }
        self.resolve_promise(promise, value);
        Ok(())
    }

    fn reject_promise(&mut self, promise: usize, reason: &JsValue) {
        self.settle_promise(promise, reason, false);
    }

    fn settle_promise(&mut self, promise: usize, value: &JsValue, fulfilled: bool) {
        let Some(record) = self.promises.get_mut(promise) else {
            return;
        };
        if !matches!(record.state, PromiseState::Pending) {
            return;
        }
        record.state = if fulfilled {
            PromiseState::Fulfilled(value.clone())
        } else {
            PromiseState::Rejected(value.clone())
        };
        let reactions = std::mem::take(&mut record.reactions);
        for reaction in reactions {
            self.enqueue_reaction(&reaction, value.clone(), fulfilled);
        }
    }

    fn enqueue_reaction(&mut self, reaction: &PromiseReaction, argument: JsValue, fulfilled: bool) {
        let handler = if fulfilled {
            reaction.on_fulfilled
        } else {
            reaction.on_rejected
        };
        self.pending_microtasks.push(JsMicrotask::PromiseReaction {
            handler,
            argument,
            fulfilled,
            result_promise: reaction.result_promise,
        });
    }

    fn perform_promise_then(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let Some(ObjectHost::Promise(promise)) = self.realm.host(receiver) else {
            return Err(JsError::type_error("incompatible Promise method receiver"));
        };
        let on_fulfilled = self.optional_callable(arguments.first())?;
        let on_rejected = self.optional_callable(arguments.get(1))?;
        let (result_promise, result) = self.create_promise()?;
        let reaction = PromiseReaction {
            on_fulfilled,
            on_rejected,
            result_promise,
        };
        let state = self
            .promises
            .get(promise)
            .map(|record| record.state.clone())
            .ok_or_else(|| JsError::type_error("Promise object refers to unknown state"))?;
        match state {
            PromiseState::Pending => self.promises[promise].reactions.push(reaction),
            PromiseState::Fulfilled(value) => self.enqueue_reaction(&reaction, value, true),
            PromiseState::Rejected(reason) => self.enqueue_reaction(&reaction, reason, false),
        }
        Ok(result)
    }

    fn optional_callable(&self, value: Option<&JsValue>) -> Result<Option<ObjectId>, JsError> {
        let Some(value) = value else {
            return Ok(None);
        };
        if matches!(value, JsValue::Undefined | JsValue::Null) {
            return Ok(None);
        }
        Self::require_callable_object(value, &self.realm).map(Some)
    }

    fn require_callable_object(value: &JsValue, realm: &Realm) -> Result<ObjectId, JsError> {
        let object = Self::require_object(value)?;
        if matches!(
            realm.host(object),
            Some(
                ObjectHost::NativeFunction(_)
                    | ObjectHost::BoundFunction { .. }
                    | ObjectHost::UserFunction(_)
                    | ObjectHost::ArrowFunction(_)
                    | ObjectHost::PromiseSettler { .. }
            )
        ) {
            Ok(object)
        } else {
            Err(JsError::type_error("value is not callable"))
        }
    }

    fn find_element_by_id(&mut self, dom: &Dom, id: &str) -> Result<Option<NodeId>, JsError> {
        let mut pending = vec![dom.document()];
        while let Some(node) = pending.pop() {
            self.consume_step()?;
            if matches!(
                dom.node(node).map(crate::dom::Node::kind),
                Some(NodeKind::Element(_))
            ) && dom.attribute(node, "id")? == Some(id)
            {
                return Ok(Some(node));
            }
            if let Some(children) = dom.children(node) {
                pending.extend(children.iter().rev());
            }
        }
        Ok(None)
    }

    fn text_content(&mut self, dom: &Dom, node: NodeId) -> Result<String, JsError> {
        let Some(root) = dom.node(node) else {
            return Err(JsError::dom("DOM wrapper refers to an unknown node"));
        };
        if let NodeKind::Text(data) | NodeKind::Comment(data) = root.kind() {
            return Ok(data.clone());
        }
        let mut result = String::new();
        let mut pending = root.children().iter().rev().copied().collect::<Vec<_>>();
        while let Some(candidate) = pending.pop() {
            self.consume_step()?;
            let Some(current) = dom.node(candidate) else {
                continue;
            };
            if let NodeKind::Text(data) = current.kind() {
                result.push_str(data);
            }
            pending.extend(current.children().iter().rev());
        }
        Ok(result)
    }

    fn set_text_content(
        &mut self,
        dom: &mut Dom,
        node: NodeId,
        value: String,
    ) -> Result<(), JsError> {
        let kind = dom
            .node(node)
            .map(crate::dom::Node::kind)
            .ok_or_else(|| JsError::dom("DOM wrapper refers to an unknown node"))?;
        if matches!(
            kind,
            NodeKind::Text(_) | NodeKind::Comment(_) | NodeKind::ProcessingInstruction { .. }
        ) {
            dom.set_character_data(node, value)?;
            return Ok(());
        }
        if !matches!(kind, NodeKind::Element(_) | NodeKind::DocumentFragment) {
            return Ok(());
        }
        let children = dom.children(node).unwrap_or_default().to_vec();
        for child in children {
            self.consume_step()?;
            dom.remove_child(node, child)?;
        }
        if !value.is_empty() {
            if self.dom_nodes_created >= self.limits.max_dom_nodes_created {
                return Err(JsError::resource("DOM node creation limit exceeded"));
            }
            self.dom_nodes_created = self.dom_nodes_created.saturating_add(1);
            let text = dom.create_text(value);
            dom.append_child(node, text)?;
        }
        Ok(())
    }

    fn require_document(&self, object: ObjectId) -> Result<NodeId, JsError> {
        match self.realm.host(object) {
            Some(ObjectHost::Document(document)) => Ok(document),
            _ => Err(JsError::type_error("incompatible Document method receiver")),
        }
    }

    fn require_node(&self, object: ObjectId) -> Result<NodeId, JsError> {
        match self.realm.host(object) {
            Some(ObjectHost::Node(node)) => Ok(node),
            _ => Err(JsError::type_error("incompatible Node method receiver")),
        }
    }

    fn require_object(value: &JsValue) -> Result<ObjectId, JsError> {
        match value {
            JsValue::Object(object) => Ok(*object),
            JsValue::Null | JsValue::Undefined => Err(JsError::type_error(
                "cannot access a property of null or undefined",
            )),
            _ => Err(JsError::type_error(
                "primitive object coercion is not implemented in this runtime slice",
            )),
        }
    }

    fn value_as_node(&self, value: &JsValue) -> Result<NodeId, JsError> {
        let JsValue::Object(object) = value else {
            return Err(JsError::type_error("argument is not a Node"));
        };
        self.require_node(*object)
    }

    fn wrap_node(&mut self, node: NodeId) -> Result<JsValue, JsError> {
        self.ensure_heap_capacity(1)?;
        Ok(JsValue::Object(self.realm.node_wrapper(node)))
    }

    fn ensure_heap_capacity(&self, additional: usize) -> Result<(), JsError> {
        if self.realm.object_count().saturating_add(additional) > self.limits.max_heap_objects {
            Err(JsError::resource("JavaScript object heap limit exceeded"))
        } else {
            Ok(())
        }
    }

    fn consume_step(&mut self) -> Result<(), JsError> {
        if self.steps_remaining == 0 {
            return Err(JsError::resource(
                "JavaScript execution step limit exceeded",
            ));
        }
        self.steps_remaining = self.steps_remaining.saturating_sub(1);
        Ok(())
    }
}

fn collect_var_names(statement: &Statement, names: &mut BTreeSet<String>) {
    match statement {
        Statement::Variable {
            kind: VariableKind::Var,
            name,
            ..
        } => {
            names.insert(name.clone());
        }
        Statement::If {
            consequent,
            alternate,
            ..
        } => {
            collect_var_names(consequent, names);
            if let Some(alternate) = alternate {
                collect_var_names(alternate, names);
            }
        }
        Statement::Switch { cases, .. } => {
            for (_, statements) in cases {
                for statement in statements {
                    collect_var_names(statement, names);
                }
            }
        }
        Statement::While { body, .. } => collect_var_names(body, names),
        Statement::For {
            initializer, body, ..
        } => {
            if let Some(initializer) = initializer {
                collect_var_names(initializer, names);
            }
            collect_var_names(body, names);
        }
        Statement::Block(statements) => {
            for statement in statements {
                collect_var_names(statement, names);
            }
        }
        Statement::Try {
            body,
            catch,
            finally,
        } => {
            for statement in body {
                collect_var_names(statement, names);
            }
            if let Some(catch) = catch {
                for statement in &catch.body {
                    collect_var_names(statement, names);
                }
            }
            if let Some(finally) = finally {
                for statement in finally {
                    collect_var_names(statement, names);
                }
            }
        }
        Statement::Function { .. }
        | Statement::Variable { .. }
        | Statement::VariableList { .. }
        | Statement::Return(_)
        | Statement::Throw(_)
        | Statement::Break
        | Statement::Continue
        | Statement::Expression(_) => {}
    }
}

impl JsValue {
    fn is_truthy(&self) -> bool {
        match self {
            Self::Undefined | Self::Null => false,
            Self::Boolean(value) => *value,
            Self::Number(value) => *value != 0.0 && !value.is_nan(),
            Self::String(value) => !value.is_empty(),
            Self::Object(_) => true,
        }
    }
}

fn to_number(value: &JsValue) -> Result<f64, JsError> {
    match value {
        JsValue::Undefined => Ok(f64::NAN),
        JsValue::Null => Ok(0.0),
        JsValue::Boolean(value) => Ok(u8::from(*value).into()),
        JsValue::Number(value) => Ok(*value),
        JsValue::String(value) => {
            let value = value.trim();
            if value.is_empty() {
                Ok(0.0)
            } else {
                value.parse().map_err(|_| {
                    JsError::type_error(
                        "numeric string conversion is not implemented for this value",
                    )
                })
            }
        }
        JsValue::Object(_) => Err(JsError::type_error(
            "object-to-primitive numeric conversion is not implemented",
        )),
    }
}

fn compare(
    left: &JsValue,
    right: &JsValue,
    numeric: impl FnOnce(f64, f64) -> bool,
    string: impl FnOnce(&str, &str) -> bool,
) -> Result<JsValue, JsError> {
    if let (JsValue::String(left), JsValue::String(right)) = (left, right) {
        return Ok(JsValue::Boolean(string(left, right)));
    }
    Ok(JsValue::Boolean(numeric(
        to_number(left)?,
        to_number(right)?,
    )))
}

fn strict_equal(left: &JsValue, right: &JsValue) -> bool {
    match (left, right) {
        (JsValue::Undefined, JsValue::Undefined) | (JsValue::Null, JsValue::Null) => true,
        (JsValue::Boolean(left), JsValue::Boolean(right)) => left == right,
        (JsValue::Number(left), JsValue::Number(right)) => number_equal(*left, *right),
        (JsValue::String(left), JsValue::String(right)) => left == right,
        (JsValue::Object(left), JsValue::Object(right)) => left == right,
        _ => false,
    }
}

#[allow(clippy::float_cmp)]
fn number_equal(left: f64, right: f64) -> bool {
    // ECMAScript Number equality is exact IEEE-754 equality: NaN differs from
    // every value, while +0 and -0 compare equal. An epsilon comparison would
    // implement different language semantics.
    left == right
}

fn abstract_equal(left: &JsValue, right: &JsValue) -> Result<bool, JsError> {
    if strict_equal(left, right) {
        return Ok(true);
    }
    if matches!(
        (left, right),
        (JsValue::Null, JsValue::Undefined) | (JsValue::Undefined, JsValue::Null)
    ) {
        return Ok(true);
    }
    match (left, right) {
        (JsValue::Number(left), JsValue::String(_)) => Ok(number_equal(*left, to_number(right)?)),
        (JsValue::String(_), JsValue::Number(right)) => Ok(number_equal(to_number(left)?, *right)),
        (JsValue::Boolean(_), _) => abstract_equal(&JsValue::Number(to_number(left)?), right),
        (_, JsValue::Boolean(_)) => abstract_equal(left, &JsValue::Number(to_number(right)?)),
        _ => Ok(false),
    }
}

fn required_argument<'a>(
    arguments: &'a [JsValue],
    index: usize,
    function: &str,
) -> Result<&'a JsValue, JsError> {
    arguments.get(index).ok_or_else(|| {
        JsError::type_error(format!(
            "{function} requires at least {} argument(s)",
            index.saturating_add(1)
        ))
    })
}

fn valid_html_local_name(name: &str) -> bool {
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic() || matches!(character, '_' | ':'))
        && characters.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | ':' | '-' | '.')
        })
}

impl From<DomError> for JsError {
    fn from(error: DomError) -> Self {
        Self::new(JsErrorKind::Dom, error.to_string(), None)
    }
}
