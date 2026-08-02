use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use super::parser::{BinaryOp, CatchClause, Expr, Statement, UnaryOp, VariableKind};
use super::value::{ErrorKind, NativeFunction, ObjectHost};
use super::{
    JsError, JsErrorKind, JsObject, JsValue, ObjectId, PropertyDescriptor, Realm, RuntimeLimits,
    ScriptOutcome,
};
use crate::dom::{Dom, DomError, NodeId, NodeKind};
use url::Url;

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

#[derive(Clone, Copy)]
enum ObjectEntryKind {
    Keys,
    Values,
    Entries,
}

impl ObjectEntryKind {
    fn function_name(self) -> &'static str {
        match self {
            Self::Keys => "Object.keys",
            Self::Values => "Object.values",
            Self::Entries => "Object.entries",
        }
    }
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
    event_listeners: BTreeMap<NodeId, BTreeMap<String, Vec<ObjectId>>>,
    global_bindings: BTreeMap<String, GlobalBinding>,
}

impl JsRuntime {
    #[must_use]
    ///
    /// # Panics
    ///
    /// The built-in `about:blank` URL is a constant and always parses.
    pub fn new(dom: &Dom) -> Self {
        Self::with_url(
            dom,
            &Url::parse("about:blank").expect("about:blank is a valid URL"),
        )
    }

    #[must_use]
    pub fn with_url(dom: &Dom, document_url: &Url) -> Self {
        Self::with_limits_and_url(dom, RuntimeLimits::default(), document_url)
    }

    #[must_use]
    ///
    /// # Panics
    ///
    /// The built-in `about:blank` URL is a constant and always parses.
    pub fn with_limits(dom: &Dom, limits: RuntimeLimits) -> Self {
        Self::with_limits_and_url(
            dom,
            limits,
            &Url::parse("about:blank").expect("about:blank is a valid URL"),
        )
    }

    #[must_use]
    pub fn with_limits_and_url(dom: &Dom, limits: RuntimeLimits, document_url: &Url) -> Self {
        Self {
            realm: Realm::bootstrap(dom.document(), document_url),
            steps_remaining: limits.max_execution_steps,
            calls_active: 0,
            dom_nodes_created: 0,
            this_stack: Vec::new(),
            environment: Vec::new(),
            functions: Vec::new(),
            promises: Vec::new(),
            pending_microtasks: Vec::new(),
            event_listeners: BTreeMap::new(),
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
            Statement::ForIn {
                kind,
                name,
                iterable,
                body,
            } => self.evaluate_for_in_statement(dom, *kind, name, iterable, body),
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

    fn evaluate_for_in_statement(
        &mut self,
        dom: &mut Dom,
        kind: VariableKind,
        name: &str,
        iterable: &Expr,
        body: &Statement,
    ) -> Result<Completion, JsError> {
        let iterable = self.evaluate(dom, iterable)?;
        let names = match iterable {
            JsValue::Object(object) => self
                .realm
                .enumerable_property_names(object)
                .ok_or_else(|| JsError::type_error("could not enumerate object properties"))?,
            _ => Vec::new(),
        };
        let mut value = JsValue::Undefined;
        if kind == VariableKind::Var {
            self.create_binding(name, kind, true, JsValue::Undefined)?;
        }
        for property in names {
            self.consume_step()?;
            let iteration_environment = if kind == VariableKind::Var {
                None
            } else {
                let environment = Rc::new(RefCell::new(EnvironmentRecord::default()));
                environment.borrow_mut().bindings.insert(
                    name.to_owned(),
                    Binding {
                        value: JsValue::String(property.clone()),
                        mutable: kind != VariableKind::Const,
                        initialized: true,
                        kind,
                    },
                );
                self.environment.push(Rc::clone(&environment));
                Some(environment)
            };
            if kind == VariableKind::Var {
                self.assign_binding(name, JsValue::String(property))?;
            }
            let completion = self.evaluate_statement(dom, body);
            if iteration_environment.is_some() {
                self.environment.pop();
            }
            match completion? {
                Completion::Normal(next) => value = next,
                Completion::Continue => {}
                Completion::Break => break,
                returned @ Completion::Return(_) => return Ok(returned),
            }
        }
        Ok(Completion::Normal(value))
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
            Expr::Unary {
                operator: UnaryOp::Delete,
                operand,
            } => self.evaluate_delete(dom, operand),
            Expr::Unary { operator, operand } => {
                let value = self.evaluate(dom, operand)?;
                self.evaluate_unary(*operator, &value)
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

    fn evaluate_delete(&mut self, dom: &mut Dom, operand: &Expr) -> Result<JsValue, JsError> {
        match operand {
            Expr::Member { .. } | Expr::ComputedMember { .. } => {
                let reference = self.resolve_assignment_reference(dom, operand)?;
                let AssignmentReference::Property { object, property } = reference else {
                    unreachable!("member expressions resolve to property references");
                };
                Ok(JsValue::Boolean(
                    self.realm.delete_property(object, &property),
                ))
            }
            Expr::Identifier(_) => Ok(JsValue::Boolean(false)),
            _ => {
                self.evaluate(dom, operand)?;
                Ok(JsValue::Boolean(true))
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
            BinaryOp::BitwiseAnd => bitwise_binary(left, right, |left, right| left & right),
            BinaryOp::BitwiseXor => bitwise_binary(left, right, |left, right| left ^ right),
            BinaryOp::BitwiseOr => bitwise_binary(left, right, |left, right| left | right),
            BinaryOp::LeftShift => shift_left(left, right),
            BinaryOp::RightShift => shift_right(left, right),
            BinaryOp::UnsignedRightShift => unsigned_shift_right(left, right),
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
        let object = self.realm.create_ordinary_object();
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

    fn evaluate_unary(&self, operator: UnaryOp, value: &JsValue) -> Result<JsValue, JsError> {
        match operator {
            UnaryOp::Not => Ok(JsValue::Boolean(!value.is_truthy())),
            UnaryOp::Typeof => Ok(JsValue::String(
                match value {
                    JsValue::Undefined => "undefined",
                    JsValue::Object(object) if Self::is_callable_object(*object, &self.realm) => {
                        "function"
                    }
                    JsValue::Null | JsValue::Object(_) => "object",
                    JsValue::Boolean(_) => "boolean",
                    JsValue::Number(_) => "number",
                    JsValue::String(_) => "string",
                }
                .to_owned(),
            )),
            UnaryOp::Plus => Ok(JsValue::Number(to_number(value)?)),
            UnaryOp::Minus => Ok(JsValue::Number(-to_number(value)?)),
            UnaryOp::BitwiseNot => Ok(JsValue::Number(f64::from(!to_int32(value)?))),
            UnaryOp::Delete => unreachable!("delete evaluates an assignment reference"),
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
            BinaryOp::BitwiseAnd => bitwise_binary(&left, &right, |left, right| left & right),
            BinaryOp::BitwiseXor => bitwise_binary(&left, &right, |left, right| left ^ right),
            BinaryOp::BitwiseOr => bitwise_binary(&left, &right, |left, right| left | right),
            BinaryOp::LeftShift => shift_left(&left, &right),
            BinaryOp::RightShift => shift_right(&left, &right),
            BinaryOp::UnsignedRightShift => unsigned_shift_right(&left, &right),
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
            Some(ObjectHost::UserFunction(_) | ObjectHost::ErrorConstructor(_))
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
            (Some(ObjectHost::Document(_) | ObjectHost::Node(_)), "addEventListener") => {
                Some(NativeFunction::AddEventListener)
            }
            (Some(ObjectHost::Document(_) | ObjectHost::Node(_)), "removeEventListener") => {
                Some(NativeFunction::RemoveEventListener)
            }
            (Some(ObjectHost::Document(_) | ObjectHost::Node(_)), "dispatchEvent") => {
                Some(NativeFunction::DispatchEvent)
            }
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
        if matches!(self.realm.host(object), Some(ObjectHost::Location(_))) {
            return Err(JsError::type_error(format!(
                "Location property {property:?} is read-only; navigation is owned by the embedding browser"
            )));
        }
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
            Some(ObjectHost::ObjectConstructor) => self.object_constructor(arguments),
            Some(ObjectHost::ErrorConstructor(kind)) => {
                self.error_constructor(constructor, kind, arguments)
            }
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
            Some(ObjectHost::EventConstructor) => self.event_constructor(arguments),
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
                let result =
                    self.call_user(dom, index, arguments, JsValue::Object(instance), true)?;
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
            Some(ObjectHost::ObjectConstructor) => self.object_constructor(arguments),
            Some(ObjectHost::FunctionConstructor) => Ok(JsValue::Undefined),
            Some(ObjectHost::StringConstructor) => Ok(JsValue::String(
                arguments
                    .first()
                    .map_or_else(String::new, JsValue::to_js_string),
            )),
            Some(ObjectHost::EventConstructor) => {
                Err(JsError::type_error("Event constructor requires 'new'"))
            }
            Some(ObjectHost::ErrorConstructor(kind)) => {
                self.error_constructor(callee, kind, arguments)
            }
            Some(ObjectHost::NativeFunction(NativeFunction::FunctionPrototype)) => {
                Ok(JsValue::Undefined)
            }
            Some(ObjectHost::NativeFunction(NativeFunction::FunctionCall)) => {
                self.function_call(dom, &receiver, arguments)
            }
            Some(ObjectHost::NativeFunction(NativeFunction::FunctionBind)) => {
                self.function_bind(&receiver, arguments)
            }
            Some(ObjectHost::NativeFunction(function)) => Self::require_object(&receiver)
                .and_then(|receiver| self.call_native(dom, function, receiver, arguments)),
            Some(ObjectHost::BoundFunction { function, receiver }) => {
                self.call_native(dom, function, receiver, arguments)
            }
            Some(ObjectHost::BoundCallable {
                target,
                receiver,
                arguments: bound_arguments,
            }) => {
                let mut combined = bound_arguments;
                combined.extend_from_slice(arguments);
                self.call_with_this(dom, target, &combined, receiver)
            }
            Some(ObjectHost::UserFunction(index)) => {
                self.call_user(dom, index, arguments, receiver, true)
            }
            Some(ObjectHost::ArrowFunction(index)) => {
                self.call_user(dom, index, arguments, receiver, false)
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

    fn function_call(
        &mut self,
        dom: &mut Dom,
        receiver: &JsValue,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let callable = Self::require_callable_object(receiver, &self.realm)?;
        let this_argument = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        self.call_with_this(
            dom,
            callable,
            arguments.get(1..).unwrap_or_default(),
            this_argument,
        )
    }

    fn function_bind(
        &mut self,
        receiver: &JsValue,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let target = Self::require_callable_object(receiver, &self.realm)?;
        self.ensure_heap_capacity(1)?;
        let bound_receiver = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let bound_arguments = arguments.get(1..).unwrap_or_default().to_vec();
        Ok(JsValue::Object(self.realm.bound_callable(
            target,
            bound_receiver,
            bound_arguments,
        )))
    }

    fn call_user(
        &mut self,
        dom: &mut Dom,
        index: usize,
        arguments: &[JsValue],
        receiver: JsValue,
        create_arguments_binding: bool,
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
        if create_arguments_binding {
            let arguments_object = self.create_array_from_values(arguments)?;
            call_environment.bindings.insert(
                "arguments".to_owned(),
                Binding {
                    value: JsValue::Object(arguments_object),
                    mutable: true,
                    initialized: true,
                    kind: VariableKind::Var,
                },
            );
        }
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

    #[allow(clippy::too_many_lines)]
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
            NativeFunction::AddEventListener => self.add_event_listener(receiver, arguments),
            NativeFunction::RemoveEventListener => self.remove_event_listener(receiver, arguments),
            NativeFunction::DispatchEvent => self.dispatch_event(dom, receiver, arguments),
            NativeFunction::EventPreventDefault => Ok(self.event_prevent_default(receiver)),
            NativeFunction::LocationToString => match self.realm.host(receiver) {
                Some(ObjectHost::Location(url)) => Ok(JsValue::String(url.to_string())),
                _ => Err(JsError::type_error("incompatible Location method receiver")),
            },
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
            NativeFunction::ArrayJoin => self.array_join(receiver, arguments),
            NativeFunction::MathPow => Self::math_pow(arguments),
            NativeFunction::ObjectAssign => self.object_assign(arguments),
            NativeFunction::ObjectKeys => self.object_entries(arguments, ObjectEntryKind::Keys),
            NativeFunction::ObjectValues => self.object_entries(arguments, ObjectEntryKind::Values),
            NativeFunction::ObjectEntries => {
                self.object_entries(arguments, ObjectEntryKind::Entries)
            }
            NativeFunction::ObjectCreate => self.object_create(arguments),
            NativeFunction::ObjectDefineProperty => self.object_define_property(arguments),
            NativeFunction::ObjectGetOwnPropertyDescriptor => {
                self.object_get_own_property_descriptor(arguments)
            }
            NativeFunction::ObjectGetOwnPropertyNames => {
                self.object_get_own_property_names(arguments)
            }
            NativeFunction::ObjectGetPrototypeOf => self.object_get_prototype_of(arguments),
            NativeFunction::ObjectHasOwn => self.object_has_own(arguments),
            NativeFunction::ObjectPrototypeHasOwnProperty => {
                self.object_prototype_has_own_property(receiver, arguments)
            }
            NativeFunction::ObjectPrototypeIsPrototypeOf => {
                Ok(self.object_prototype_is_prototype_of(receiver, arguments))
            }
            NativeFunction::ObjectPrototypePropertyIsEnumerable => {
                self.object_prototype_property_is_enumerable(receiver, arguments)
            }
            NativeFunction::ErrorPrototypeToString => Ok(self.error_to_string(receiver)),
            NativeFunction::FunctionPrototype
            | NativeFunction::FunctionCall
            | NativeFunction::FunctionBind => {
                unreachable!("Function prototype methods use arbitrary receivers")
            }
        }
    }

    fn object_constructor(&mut self, arguments: &[JsValue]) -> Result<JsValue, JsError> {
        if let Some(JsValue::Object(object)) = arguments.first() {
            return Ok(JsValue::Object(*object));
        }
        self.ensure_heap_capacity(1)?;
        Ok(JsValue::Object(self.realm.create_ordinary_object()))
    }

    fn event_constructor(&mut self, arguments: &[JsValue]) -> Result<JsValue, JsError> {
        let event_type = required_argument(arguments, 0, "Event")?.to_js_string();
        if event_type.is_empty() {
            return Err(JsError::type_error("Event type must not be empty"));
        }
        let options = arguments.get(1).and_then(|value| match value {
            JsValue::Object(object) => Some(*object),
            _ => None,
        });
        let bubbles = options
            .and_then(|object| self.realm.get_property(object, "bubbles"))
            .is_some_and(|value| value.is_truthy());
        let cancelable = options
            .and_then(|object| self.realm.get_property(object, "cancelable"))
            .is_some_and(|value| value.is_truthy());
        let constructor = self
            .realm
            .global("Event")
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            })
            .ok_or_else(|| JsError::type_error("Event constructor is unavailable"))?;
        let prototype = self
            .realm
            .get_property(constructor, "prototype")
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            });
        self.ensure_heap_capacity(1)?;
        let event = self.realm.create_object(prototype);
        for (name, value) in [
            ("type", JsValue::String(event_type)),
            ("bubbles", JsValue::Boolean(bubbles)),
            ("cancelable", JsValue::Boolean(cancelable)),
            ("defaultPrevented", JsValue::Boolean(false)),
            ("target", JsValue::Null),
            ("currentTarget", JsValue::Null),
        ] {
            self.realm.set_property(event, name.to_owned(), value);
        }
        Ok(JsValue::Object(event))
    }

    fn event_target_node(&self, receiver: ObjectId) -> Result<NodeId, JsError> {
        match self.realm.host(receiver) {
            Some(ObjectHost::Document(node) | ObjectHost::Node(node)) => Ok(node),
            _ => Err(JsError::type_error(
                "incompatible EventTarget method receiver",
            )),
        }
    }

    fn add_event_listener(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let target = self.event_target_node(receiver)?;
        let event_type = required_argument(arguments, 0, "addEventListener")?.to_js_string();
        let callback = match arguments.get(1) {
            None | Some(JsValue::Null | JsValue::Undefined) => return Ok(JsValue::Undefined),
            Some(value) => Self::require_callable_object(value, &self.realm)?,
        };
        let callbacks = self
            .event_listeners
            .entry(target)
            .or_default()
            .entry(event_type)
            .or_default();
        if !callbacks.contains(&callback) {
            callbacks.push(callback);
        }
        Ok(JsValue::Undefined)
    }

    fn remove_event_listener(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let target = self.event_target_node(receiver)?;
        let event_type = required_argument(arguments, 0, "removeEventListener")?.to_js_string();
        let Some(JsValue::Object(callback)) = arguments.get(1) else {
            return Ok(JsValue::Undefined);
        };
        if let Some(callbacks) = self
            .event_listeners
            .get_mut(&target)
            .and_then(|listeners| listeners.get_mut(&event_type))
        {
            callbacks.retain(|candidate| candidate != callback);
        }
        Ok(JsValue::Undefined)
    }

    fn dispatch_event(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let target = self.event_target_node(receiver)?;
        let event = Self::require_object(required_argument(arguments, 0, "dispatchEvent")?)?;
        let event_type = self
            .realm
            .get_property(event, "type")
            .map(|value| value.to_js_string())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| JsError::type_error("dispatchEvent argument is not an Event"))?;
        let bubbles = self
            .realm
            .get_property(event, "bubbles")
            .is_some_and(|value| value.is_truthy());
        self.realm
            .set_property(event, "target".to_owned(), JsValue::Object(receiver));

        let mut path = vec![target];
        if bubbles {
            let mut ancestor = dom.parent(target);
            while let Some(node) = ancestor {
                path.push(node);
                ancestor = dom.parent(node);
            }
        }
        for node in path {
            let current_target = if node == dom.document() {
                self.realm.document_object()
            } else {
                self.ensure_heap_capacity(1)?;
                self.realm.node_wrapper(node)
            };
            self.realm.set_property(
                event,
                "currentTarget".to_owned(),
                JsValue::Object(current_target),
            );
            let callbacks = self
                .event_listeners
                .get(&node)
                .and_then(|listeners| listeners.get(&event_type))
                .cloned()
                .unwrap_or_default();
            for callback in callbacks {
                self.call_with_this(
                    dom,
                    callback,
                    &[JsValue::Object(event)],
                    JsValue::Object(current_target),
                )?;
            }
        }
        self.realm
            .set_property(event, "currentTarget".to_owned(), JsValue::Null);
        let canceled = self
            .realm
            .get_property(event, "defaultPrevented")
            .is_some_and(|value| value.is_truthy());
        Ok(JsValue::Boolean(!canceled))
    }

    fn event_prevent_default(&mut self, receiver: ObjectId) -> JsValue {
        let cancelable = self
            .realm
            .get_property(receiver, "cancelable")
            .is_some_and(|value| value.is_truthy());
        if cancelable {
            self.realm.set_property(
                receiver,
                "defaultPrevented".to_owned(),
                JsValue::Boolean(true),
            );
        }
        JsValue::Undefined
    }

    fn error_constructor(
        &mut self,
        constructor: ObjectId,
        _kind: ErrorKind,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        self.ensure_heap_capacity(1)?;
        let prototype = self
            .realm
            .get_property(constructor, "prototype")
            .and_then(|value| match value {
                JsValue::Object(prototype) => Some(prototype),
                _ => None,
            })
            .ok_or_else(|| JsError::type_error("Error constructor prototype is not an object"))?;
        let message = arguments
            .first()
            .and_then(|value| (!matches!(value, JsValue::Undefined)).then(|| value.to_js_string()));
        Ok(JsValue::Object(self.realm.create_error(prototype, message)))
    }

    fn error_to_string(&self, receiver: ObjectId) -> JsValue {
        let name = self
            .realm
            .get_property(receiver, "name")
            .unwrap_or_else(|| JsValue::String("Error".to_owned()))
            .to_js_string();
        let message = self
            .realm
            .get_property(receiver, "message")
            .unwrap_or_else(|| JsValue::String(String::new()))
            .to_js_string();
        let result = match (name.is_empty(), message.is_empty()) {
            (true, _) => message,
            (_, true) => name,
            (false, false) => format!("{name}: {message}"),
        };
        JsValue::String(result)
    }

    fn object_assign(&mut self, arguments: &[JsValue]) -> Result<JsValue, JsError> {
        let target = Self::require_object(required_argument(arguments, 0, "Object.assign")?)?;
        for source in &arguments[1..] {
            match source {
                JsValue::Object(source) => {
                    let properties = self
                        .realm
                        .enumerable_own_properties(*source)
                        .ok_or_else(|| JsError::type_error("Object.assign source is invalid"))?;
                    for (key, value) in properties {
                        if !self.realm.set_property(target, key, value) {
                            return Err(JsError::type_error(
                                "Object.assign could not write target property",
                            ));
                        }
                    }
                }
                JsValue::String(source) => {
                    for (index, character) in source.chars().enumerate() {
                        if !self.realm.set_property(
                            target,
                            index.to_string(),
                            JsValue::String(character.to_string()),
                        ) {
                            return Err(JsError::type_error(
                                "Object.assign could not write target property",
                            ));
                        }
                    }
                }
                JsValue::Undefined | JsValue::Null | JsValue::Boolean(_) | JsValue::Number(_) => {}
            }
        }
        Ok(JsValue::Object(target))
    }

    fn object_entries(
        &mut self,
        arguments: &[JsValue],
        kind: ObjectEntryKind,
    ) -> Result<JsValue, JsError> {
        let value = required_argument(arguments, 0, kind.function_name())?;
        let properties = match value {
            JsValue::Undefined | JsValue::Null => {
                return Err(JsError::type_error(format!(
                    "{} cannot convert null or undefined to object",
                    kind.function_name()
                )));
            }
            JsValue::Object(object) => self
                .realm
                .enumerable_own_properties(*object)
                .ok_or_else(|| JsError::type_error("object is invalid"))?,
            JsValue::String(value) => value
                .chars()
                .enumerate()
                .map(|(index, character)| {
                    (index.to_string(), JsValue::String(character.to_string()))
                })
                .collect(),
            JsValue::Boolean(_) | JsValue::Number(_) => Vec::new(),
        };
        let mut output = Vec::with_capacity(properties.len());
        for (key, value) in properties {
            output.push(match kind {
                ObjectEntryKind::Keys => JsValue::String(key),
                ObjectEntryKind::Values => value,
                ObjectEntryKind::Entries => {
                    JsValue::Object(self.create_array_from_values(&[JsValue::String(key), value])?)
                }
            });
        }
        Ok(JsValue::Object(self.create_array_from_values(&output)?))
    }

    fn object_create(&mut self, arguments: &[JsValue]) -> Result<JsValue, JsError> {
        let prototype = match required_argument(arguments, 0, "Object.create")? {
            JsValue::Null => None,
            JsValue::Object(object) => Some(*object),
            _ => {
                return Err(JsError::type_error(
                    "Object prototype may only be an object or null",
                ));
            }
        };
        self.ensure_heap_capacity(1)?;
        Ok(JsValue::Object(self.realm.create_object(prototype)))
    }

    fn object_define_property(&mut self, arguments: &[JsValue]) -> Result<JsValue, JsError> {
        let object =
            Self::require_object(required_argument(arguments, 0, "Object.defineProperty")?)?;
        let key = required_argument(arguments, 1, "Object.defineProperty")?.to_js_string();
        let descriptor =
            Self::require_object(required_argument(arguments, 2, "Object.defineProperty")?)?;
        let descriptor = PropertyDescriptor {
            value: self
                .realm
                .get_property(descriptor, "value")
                .unwrap_or(JsValue::Undefined),
            writable: self
                .realm
                .get_property(descriptor, "writable")
                .is_some_and(|value| value.is_truthy()),
            enumerable: self
                .realm
                .get_property(descriptor, "enumerable")
                .is_some_and(|value| value.is_truthy()),
            configurable: self
                .realm
                .get_property(descriptor, "configurable")
                .is_some_and(|value| value.is_truthy()),
        };
        if !self.realm.define_property(object, key, descriptor) {
            return Err(JsError::type_error("cannot redefine object property"));
        }
        Ok(JsValue::Object(object))
    }

    fn object_get_own_property_descriptor(
        &mut self,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let object = Self::require_object(required_argument(
            arguments,
            0,
            "Object.getOwnPropertyDescriptor",
        )?)?;
        let key =
            required_argument(arguments, 1, "Object.getOwnPropertyDescriptor")?.to_js_string();
        let Some(descriptor) = self.realm.own_property(object, &key) else {
            return Ok(JsValue::Undefined);
        };
        self.ensure_heap_capacity(1)?;
        let result = self.realm.create_ordinary_object();
        for (key, value) in [
            ("value", descriptor.value),
            ("writable", JsValue::Boolean(descriptor.writable)),
            ("enumerable", JsValue::Boolean(descriptor.enumerable)),
            ("configurable", JsValue::Boolean(descriptor.configurable)),
        ] {
            self.realm.set_property(result, key.to_owned(), value);
        }
        Ok(JsValue::Object(result))
    }

    fn object_get_prototype_of(&self, arguments: &[JsValue]) -> Result<JsValue, JsError> {
        let object =
            Self::require_object(required_argument(arguments, 0, "Object.getPrototypeOf")?)?;
        Ok(self
            .realm
            .object(object)
            .and_then(JsObject::prototype)
            .map_or(JsValue::Null, JsValue::Object))
    }

    fn object_get_own_property_names(&mut self, arguments: &[JsValue]) -> Result<JsValue, JsError> {
        let object = Self::require_object(required_argument(
            arguments,
            0,
            "Object.getOwnPropertyNames",
        )?)?;
        let names = self
            .realm
            .own_property_names(object)
            .ok_or_else(|| JsError::type_error("object is invalid"))?
            .into_iter()
            .map(JsValue::String)
            .collect::<Vec<_>>();
        Ok(JsValue::Object(self.create_array_from_values(&names)?))
    }

    fn object_has_own(&self, arguments: &[JsValue]) -> Result<JsValue, JsError> {
        let object = Self::require_object(required_argument(arguments, 0, "Object.hasOwn")?)?;
        let key = required_argument(arguments, 1, "Object.hasOwn")?.to_js_string();
        Ok(JsValue::Boolean(
            self.realm.own_property(object, &key).is_some(),
        ))
    }

    fn object_prototype_has_own_property(
        &self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let key =
            required_argument(arguments, 0, "Object.prototype.hasOwnProperty")?.to_js_string();
        Ok(JsValue::Boolean(
            self.realm.own_property(receiver, &key).is_some(),
        ))
    }

    fn object_prototype_is_prototype_of(
        &self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> JsValue {
        let Some(&JsValue::Object(mut candidate)) = arguments.first() else {
            return JsValue::Boolean(false);
        };
        for _ in 0..self.realm.object_count() {
            let Some(object) = self.realm.object(candidate) else {
                return JsValue::Boolean(false);
            };
            let Some(prototype) = object.prototype() else {
                return JsValue::Boolean(false);
            };
            if prototype == receiver {
                return JsValue::Boolean(true);
            }
            candidate = prototype;
        }
        JsValue::Boolean(false)
    }

    fn object_prototype_property_is_enumerable(
        &self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let key = required_argument(arguments, 0, "Object.prototype.propertyIsEnumerable")?
            .to_js_string();
        Ok(JsValue::Boolean(
            self.realm
                .own_property(receiver, &key)
                .is_some_and(|descriptor| descriptor.enumerable),
        ))
    }

    fn create_array_from_values(&mut self, values: &[JsValue]) -> Result<ObjectId, JsError> {
        self.ensure_heap_capacity(1)?;
        let array = self.realm.create_array();
        for (index, value) in values.iter().enumerate() {
            self.realm
                .set_property(array, index.to_string(), value.clone());
        }
        let length = u32::try_from(values.len())
            .map_err(|_| JsError::resource("array length exceeds the supported u32 range"))?;
        self.set_array_length(array, length)?;
        Ok(array)
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

    fn array_join(&self, receiver: ObjectId, arguments: &[JsValue]) -> Result<JsValue, JsError> {
        self.require_array(receiver)?;
        let separator = match arguments.first() {
            None | Some(JsValue::Undefined) => ",".to_owned(),
            Some(value) => value.to_js_string(),
        };
        let length = self.array_length(receiver)?;
        let mut output = String::new();
        for index in 0..length {
            if index != 0 {
                output.push_str(&separator);
            }
            match self.realm.get_property(receiver, &index.to_string()) {
                None | Some(JsValue::Undefined | JsValue::Null) => {}
                Some(value) => output.push_str(&value.to_js_string()),
            }
        }
        Ok(JsValue::String(output))
    }

    fn math_pow(arguments: &[JsValue]) -> Result<JsValue, JsError> {
        let base = arguments.first().unwrap_or(&JsValue::Undefined);
        let exponent = arguments.get(1).unwrap_or(&JsValue::Undefined);
        Ok(JsValue::Number(to_number(base)?.powf(to_number(exponent)?)))
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
                    | ObjectHost::BoundCallable { .. }
                    | ObjectHost::UserFunction(_)
                    | ObjectHost::ArrowFunction(_)
                    | ObjectHost::FunctionConstructor
                    | ObjectHost::StringConstructor
                    | ObjectHost::EventConstructor
                    | ObjectHost::ErrorConstructor(_)
                    | ObjectHost::PromiseSettler { .. }
            )
        ) {
            Ok(object)
        } else {
            Err(JsError::type_error("value is not callable"))
        }
    }

    fn is_callable_object(object: ObjectId, realm: &Realm) -> bool {
        matches!(
            realm.host(object),
            Some(
                ObjectHost::NativeFunction(_)
                    | ObjectHost::BoundFunction { .. }
                    | ObjectHost::BoundCallable { .. }
                    | ObjectHost::UserFunction(_)
                    | ObjectHost::ArrowFunction(_)
                    | ObjectHost::FunctionConstructor
                    | ObjectHost::StringConstructor
                    | ObjectHost::EventConstructor
                    | ObjectHost::ErrorConstructor(_)
                    | ObjectHost::PromiseSettler { .. }
            )
        )
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
        Statement::ForIn {
            kind, name, body, ..
        } => {
            if *kind == VariableKind::Var {
                names.insert(name.clone());
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

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn to_uint32(value: &JsValue) -> Result<u32, JsError> {
    let number = to_number(value)?;
    if !number.is_finite() || number == 0.0 {
        return Ok(0);
    }
    let integer = number.trunc();
    let modulo = integer.rem_euclid(4_294_967_296.0);
    Ok(modulo as u32)
}

#[allow(clippy::cast_possible_wrap)]
fn to_int32(value: &JsValue) -> Result<i32, JsError> {
    Ok(to_uint32(value)? as i32)
}

fn bitwise_binary(
    left: &JsValue,
    right: &JsValue,
    operation: impl FnOnce(i32, i32) -> i32,
) -> Result<JsValue, JsError> {
    Ok(JsValue::Number(f64::from(operation(
        to_int32(left)?,
        to_int32(right)?,
    ))))
}

fn shift_count(value: &JsValue) -> Result<u32, JsError> {
    Ok(to_uint32(value)? & 0x1f)
}

fn shift_left(left: &JsValue, right: &JsValue) -> Result<JsValue, JsError> {
    Ok(JsValue::Number(f64::from(
        to_int32(left)?.wrapping_shl(shift_count(right)?),
    )))
}

fn shift_right(left: &JsValue, right: &JsValue) -> Result<JsValue, JsError> {
    Ok(JsValue::Number(f64::from(
        to_int32(left)? >> shift_count(right)?,
    )))
}

fn unsigned_shift_right(left: &JsValue, right: &JsValue) -> Result<JsValue, JsError> {
    Ok(JsValue::Number(f64::from(
        to_uint32(left)? >> shift_count(right)?,
    )))
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

#[cfg(test)]
mod tests {
    use super::JsRuntime;
    use crate::html::parse_document;
    use crate::js::JsValue;
    use url::Url;

    #[test]
    fn location_exposes_normalized_committed_url_components() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let url =
            Url::parse("https://user:pass@example.test:8443/a/b?q=rust#part").expect("test URL");
        let mut runtime = JsRuntime::with_url(&parsed.dom, &url);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r##"
                    window.location === location && document.location === location &&
                    location.href === "https://user:pass@example.test:8443/a/b?q=rust#part" &&
                    location.origin === "https://example.test:8443" &&
                    location.protocol === "https:" && location.host === "example.test:8443" &&
                    location.hostname === "example.test" && location.port === "8443" &&
                    location.pathname === "/a/b" && location.search === "?q=rust" &&
                    location.hash === "#part" && location.toString() === location.href;
                "##,
            )
            .expect("Location reads should execute");
        assert_eq!(outcome.value, JsValue::Boolean(true));
    }

    #[test]
    fn location_writes_fail_instead_of_faking_navigation() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let url = Url::parse("https://example.test/current").expect("test URL");
        let mut runtime = JsRuntime::with_url(&parsed.dom, &url);
        let error = runtime
            .execute(&mut parsed.dom, "location.href = '/next';")
            .expect_err("unsupported navigation must be explicit");
        assert_eq!(error.kind(), crate::js::JsErrorKind::Type);
        assert!(error.message().contains("embedding browser"));
    }

    #[test]
    fn navigator_exposes_browser_bootstrap_properties() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"navigator.appName === "Netscape" &&
                    navigator.userAgent === "Mozilla/5.0 rENDER/0.1" &&
                    navigator.language === "zh-CN" &&
                    navigator.cookieEnabled && navigator.onLine;"#,
            )
            .expect("navigator feature detection should execute");

        assert_eq!(outcome.value, JsValue::Boolean(true));
    }

    #[test]
    fn event_target_registers_removes_and_cancels_listeners() {
        let mut parsed = parse_document("<!doctype html><button id='button'>go</button>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    var button = document.getElementById("button");
                    var calls = 0;
                    function listener(event) {
                        calls = calls + 1;
                        event.preventDefault();
                    }
                    button.addEventListener("activate", listener);
                    button.addEventListener("activate", listener);
                    var event = new Event("activate", { cancelable: true });
                    var accepted = button.dispatchEvent(event);
                    button.removeEventListener("activate", listener);
                    button.dispatchEvent(new Event("activate"));
                    calls + ":" + accepted + ":" + event.defaultPrevented;
                "#,
            )
            .expect("EventTarget methods should dispatch a cancelable event");
        assert_eq!(outcome.value, JsValue::String("1:false:true".to_owned()));
    }

    #[test]
    fn bubbling_event_exposes_target_current_target_and_listener_this() {
        let mut parsed =
            parse_document("<!doctype html><div id='parent'><button id='child'>go</button></div>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    var parent = document.getElementById("parent");
                    var child = document.getElementById("child");
                    var observed = false;
                    function listener(event) {
                        observed = event.target === child &&
                            event.currentTarget === parent && this === parent;
                    }
                    parent.addEventListener("activate", listener);
                    var event = new Event("activate", { bubbles: true });
                    child.dispatchEvent(event);
                    observed && event.currentTarget === null;
                "#,
            )
            .expect("bubbling should traverse DOM ancestors");
        assert_eq!(outcome.value, JsValue::Boolean(true));
    }

    #[test]
    fn object_reflection_and_assignment_cover_common_runtime_usage() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    var source = { b: 2, a: 1 };
                    var target = Object.assign({}, source);
                    var descriptor = Object.getOwnPropertyDescriptor(target, "a");
                    Object.defineProperty(target, "hidden", { value: 9, enumerable: false, writable: false, configurable: false });
                    Object.keys(target)[0] + Object.keys(target)[1] + Object.values(target)[0] + Object.values(target)[1] + Object.hasOwn(target, "hidden") + descriptor.value;
                "#,
            )
            .expect("Object builtins should execute");
        assert_eq!(outcome.value, JsValue::String("ab12true1".to_owned()));
    }

    #[test]
    fn object_create_and_constructor_preserve_prototypes() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r"
                    var prototype = { answer: 42 };
                    var child = Object.create(prototype);
                    var wrapped = { value: 7 };
                    Object.getPrototypeOf(child) === prototype && child.answer === 42 && Object(wrapped) === wrapped;
                ",
            )
            .expect("Object.create and Object() should execute");
        assert_eq!(outcome.value, JsValue::Boolean(true));
    }

    #[test]
    fn object_prototype_methods_cover_ordinary_arrays_and_null_prototypes() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    var ordinary = { visible: 1 };
                    var dictionary = Object.create(null);
                    Object.getPrototypeOf(ordinary) === Object.prototype &&
                        ordinary.hasOwnProperty("visible") &&
                        !ordinary.hasOwnProperty("toString") &&
                        ordinary.propertyIsEnumerable("visible") &&
                        !ordinary.propertyIsEnumerable("hasOwnProperty") &&
                        Object.prototype.isPrototypeOf(ordinary) &&
                        Object.prototype.isPrototypeOf([]) &&
                        Array.hasOwnProperty("prototype") &&
                        dictionary.hasOwnProperty === undefined;
                "#,
            )
            .expect("Object.prototype methods should execute");
        assert_eq!(outcome.value, JsValue::Boolean(true));
    }

    #[test]
    fn for_in_enumerates_prototypes_and_delete_honors_descriptors() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    var prototype = { inherited: 1 };
                    var object = Object.create(prototype);
                    object.own = 2;
                    Object.defineProperty(object, "hidden", { value: 3, enumerable: false });
                    Object.defineProperty(object, "fixed", { value: 4, enumerable: true, configurable: false });
                    var names = "";
                    for (var name in object) { names = names + name + ","; }
                    var removed = delete object.own;
                    var retained = delete object.fixed;
                    names + removed + "," + retained + "," + object.own + "," + object.fixed;
                "#,
            )
            .expect("for-in and delete should execute");
        assert_eq!(
            outcome.value,
            JsValue::String("fixed,own,inherited,true,false,undefined,4".to_owned())
        );
    }

    #[test]
    fn function_call_bind_and_typeof_share_callable_semantics() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    function add(left, right) { return this.base + left + right; }
                    var bound = add.bind({ base: 10 }, 2);
                    typeof Function === "function" &&
                        typeof Function.prototype === "function" &&
                        typeof bound === "function" &&
                        add.call({ base: 1 }, 3, 4) === 8 &&
                        bound(5) === 17;
                "#,
            )
            .expect("Function.prototype call and bind should execute");
        assert_eq!(outcome.value, JsValue::Boolean(true));
    }

    #[test]
    fn property_helper_primordials_can_be_uncurried() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    var join = Function.prototype.call.bind(Array.prototype.join);
                    var push = Function.prototype.call.bind(Array.prototype.push);
                    var values = ["a"];
                    push(values, "b", "c");
                    var target = { visible: 1 };
                    Object.defineProperty(target, "hidden", { value: 2, enumerable: false });
                    join(values, ";") + ":" +
                        join(Object.getOwnPropertyNames(target), ",") + ":" +
                        Math.pow(2, 5);
                "#,
            )
            .expect("propertyHelper primordial operations should execute");
        assert_eq!(
            outcome.value,
            JsValue::String("a;b;c:hidden,visible:32".to_owned())
        );
    }

    #[test]
    fn user_functions_expose_arguments_and_string_conversion() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    function inspect(value) {
                        var first = () => arguments[0];
                        return arguments.length + ":" + String(first()) + ":" + typeof String;
                    }
                    inspect(42, "extra");
                "#,
            )
            .expect("ordinary functions should expose arguments");
        assert_eq!(outcome.value, JsValue::String("2:42:function".to_owned()));
    }

    #[test]
    fn native_error_constructors_share_the_error_prototype_contract() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    var error = new TypeError("bad input");
                    var called = RangeError("out of range");
                    typeof Error + ":" +
                        (error instanceof TypeError) + ":" +
                        (error instanceof Error) + ":" +
                        (called instanceof RangeError) + ":" +
                        error.name + ":" + error.message + ":" + error.toString();
                "#,
            )
            .expect("native Error constructors should call and construct");

        assert_eq!(
            outcome.value,
            JsValue::String(
                "function:true:true:true:TypeError:bad input:TypeError: bad input".to_owned()
            )
        );
    }

    #[test]
    fn native_errors_have_distinct_prototypes_and_optional_messages() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    var empty = new Error();
                    var syntax = new SyntaxError("parse");
                    Object.getPrototypeOf(empty) === Error.prototype &&
                        Object.getPrototypeOf(syntax) === SyntaxError.prototype &&
                        Object.getPrototypeOf(SyntaxError.prototype) === Error.prototype &&
                        !empty.hasOwnProperty("message") &&
                        syntax.propertyIsEnumerable("message") === false &&
                        empty.toString() === "Error";
                "#,
            )
            .expect("native Error prototypes and descriptors should execute");

        assert_eq!(outcome.value, JsValue::Boolean(true));
    }

    #[test]
    fn bitwise_operators_follow_ecmascript_precedence_and_int32_semantics() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    var flags = 6;
                    var jqueryToggle = flags ^ 1;
                    var precedence = 1 | 2 ^ 3 & 1;
                    var shifts = (1 << 31) + ":" + (-1 >> 1) + ":" + (-1 >>> 0);
                    var coercion = (NaN | 0) + ":" + (Infinity & 7) + ":" + (~0);
                    jqueryToggle + ":" + precedence + ":" + shifts + ":" + coercion;
                "#,
            )
            .expect("site-style bitwise expressions should execute");

        assert_eq!(
            outcome.value,
            JsValue::String("7:3:-2147483648:-1:4294967295:0:0:-1".to_owned())
        );
    }

    #[test]
    fn bitwise_compound_assignment_evaluates_member_reference_once() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    var calls = 0;
                    var values = [6];
                    function index() { calls += 1; return 0; }
                    values[index()] ^= 3;
                    values[index()] <<= 2;
                    values[index()] >>>= 1;
                    values[0] + ":" + calls;
                "#,
            )
            .expect("compound bitwise assignment should preserve a single reference evaluation");

        assert_eq!(outcome.value, JsValue::String("10:3".to_owned()));
    }
}
