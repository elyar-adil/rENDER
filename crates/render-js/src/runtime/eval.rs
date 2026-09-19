#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::float_cmp,
    clippy::if_not_else,
    clippy::manual_let_else,
    clippy::map_unwrap_or,
    clippy::match_same_arms,
    clippy::needless_ifs,
    clippy::too_many_lines,
    clippy::wrong_self_convention
)]

use render_css::stylesheet::parse_declaration_list;
use render_dom::Dom;
use render_dom::NodeKind;
use render_html::serialize_html_fragment;
use render_html::serialize_html_node;
use crate::JsError;
use crate::JsErrorKind;
use crate::JsObject;
use crate::JsSymbol;
use crate::JsValue;
use crate::ObjectId;
use crate::PropertyDescriptor;
use crate::Realm;
use crate::parser::BinaryOp;
use crate::parser::CatchClause;
use crate::parser::Expr;
use crate::parser::ObjectAccessorKind;
use crate::parser::ObjectProperty;
use crate::parser::PARAMETER_DEFAULT_MARKER;
use crate::parser::PARAMETER_REST_MARKER;
use crate::parser::PropertyKey;
use crate::parser::Statement;
use crate::parser::UnaryOp;
use crate::parser::VariableKind;
use crate::runtime::JsRuntime;
use crate::runtime::builtins::array::array_index;
use crate::runtime::builtins::dom::css_prop_from_member;
use crate::runtime::builtins::dom::is_valid_property_name;
use crate::runtime::builtins::dom::node_attribute_property;
use crate::runtime::builtins::dom::node_boolean_property;
use crate::runtime::builtins::string::string_method_native;
use crate::runtime::builtins::style::STYLE_METHOD_PROPERTIES;
use crate::runtime::convert::abstract_equal;
use crate::runtime::convert::bitwise_binary;
use crate::runtime::convert::compare;
use crate::runtime::convert::required_argument;
use crate::runtime::convert::shift_left;
use crate::runtime::convert::shift_right;
use crate::runtime::convert::strict_equal;
use crate::runtime::convert::to_int32;
use crate::runtime::convert::to_number;
use crate::runtime::convert::unsigned_shift_right;
use crate::runtime::types::Binding;
use crate::runtime::types::CallFrame;
use crate::runtime::types::EnvironmentRecord;
use crate::runtime::types::GlobalBinding;
use crate::runtime::types::NavigationRequest;
use crate::runtime::types::UserFunction;
use crate::value::ErrorKind;
use crate::value::NativeFunction;
use crate::value::ObjectHost;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::rc::Rc;

#[derive(Clone, Debug)]
pub(super) enum Completion {
    Normal(JsValue),
    Return(JsValue),
    Break(Option<String>),
    Continue(Option<String>),
}

#[derive(Clone, Debug)]
pub(super) enum AssignmentReference {
    Binding(String),
    Property { object: ObjectId, property: String },
    SymbolProperty { object: ObjectId, symbol: JsSymbol },
}

pub(super) fn collect_var_names(statement: &Statement, names: &mut BTreeSet<String>) {
    match statement {
        Statement::Variable {
            kind: VariableKind::Var,
            name,
            ..
        } => {
            names.insert(name.clone());
        }
        Statement::VariableList {
            kind: VariableKind::Var,
            declarations,
            ..
        } => {
            for (name, _) in declarations {
                names.insert(name.clone());
            }
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
        Statement::While { body, .. }
        | Statement::Labeled { body, .. }
        | Statement::ForInExpr { body, .. }
        | Statement::DoWhile { body, .. } => collect_var_names(body, names),
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
        Statement::ForOf {
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
            ..
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
        | Statement::Break(_)
        | Statement::Continue(_)
        | Statement::Expression(_) => {}
    }
}

impl JsRuntime {
    pub(super) fn evaluate_statements(
        &mut self,
        dom: &mut Dom,
        statements: &[Statement],
    ) -> Result<Completion, JsError> {
        let mut value = JsValue::Undefined;
        for statement in statements {
            match self.evaluate_statement(dom, statement)? {
                Completion::Normal(next) => value = next,
                abrupt @ (Completion::Return(_)
                | Completion::Break(_)
                | Completion::Continue(_)) => {
                    return Ok(abrupt);
                }
            }
        }
        Ok(Completion::Normal(value))
    }

    pub(super) fn instantiate_statements(
        &mut self,
        statements: &[Statement],
    ) -> Result<(), JsError> {
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
                Statement::Variable {
                    kind: VariableKind::Var,
                    name,
                    ..
                } => {
                    var_names.insert(name.clone());
                }
                Statement::VariableList {
                    kind, declarations, ..
                } => {
                    for (name, _) in declarations {
                        if *kind == VariableKind::Var {
                            var_names.insert(name.clone());
                        } else if lexical_declarations.insert(name.clone(), *kind).is_some() {
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
                    ..
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
            let value = self.create_function(Some(name), parameters, body, None)?;
            self.initialize_binding(name, value, VariableKind::Var)?;
        }
        Ok(())
    }

    pub(super) fn instantiate_block_lexicals(
        &mut self,
        statements: &[Statement],
    ) -> Result<(), JsError> {
        let mut declarations = BTreeMap::new();
        let mut functions = Vec::new();
        for statement in statements {
            match statement {
                Statement::VariableList {
                    kind,
                    declarations: variables,
                    ..
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
                    ..
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
            let value = self.create_function(Some(name), parameters, body, None)?;
            self.initialize_binding(name, value, VariableKind::Const)?;
        }
        Ok(())
    }

    pub(super) fn create_user_function(
        &mut self,
        parameters: &[String],
        body: &[Statement],
    ) -> Result<JsValue, JsError> {
        self.create_function(None, parameters, body, None)
    }

    pub(super) fn create_arrow_function(
        &mut self,
        parameters: &[String],
        body: &[Statement],
    ) -> Result<JsValue, JsError> {
        let lexical_this = self
            .this_stack
            .last()
            .cloned()
            .unwrap_or(JsValue::Undefined);
        self.create_function(None, parameters, body, Some(lexical_this))
    }

    pub(super) fn create_function(
        &mut self,
        name: Option<&str>,
        parameters: &[String],
        body: &[Statement],
        lexical_this: Option<JsValue>,
    ) -> Result<JsValue, JsError> {
        let is_arrow = lexical_this.is_some();
        self.ensure_heap_capacity(if is_arrow { 1 } else { 2 })?;
        let function_index = self.functions.len();
        let (parameters, length) = Self::binding_parameters(parameters);
        self.functions.push(UserFunction {
            name: name.map(str::to_owned),
            parameters,
            body: body.to_vec(),
            captured_environment: self.environment.clone(),
            lexical_this,
        });
        // Spec: the `name` of an anonymous function in progress is the empty
        // string (anonymous arrows included); `length` counts parameters
        // before the first default initializer, excluding the rest parameter.
        let name = name.unwrap_or("");
        let function = if is_arrow {
            self.realm.arrow_function(function_index, name, length)
        } else {
            self.realm.user_function(function_index, name, length)
        };
        Ok(JsValue::Object(function))
    }

    /// Strip the parser's default/rest parameter markers into plain binding
    /// names and derive the spec `length`: the parameter count before the
    /// first default initializer, with the rest parameter excluded. The
    /// `\0`-prefixed arrow destructuring temporaries pass through untouched.
    fn binding_parameters(parameters: &[String]) -> (Vec<String>, usize) {
        let mut names = Vec::with_capacity(parameters.len());
        let mut length = 0_usize;
        let mut counting = true;
        for parameter in parameters {
            let binding = parameter
                .strip_prefix(PARAMETER_DEFAULT_MARKER)
                .or_else(|| parameter.strip_prefix(PARAMETER_REST_MARKER));
            if let Some(binding) = binding {
                names.push(binding.to_owned());
                counting = false;
            } else {
                names.push(parameter.clone());
                if counting {
                    length += 1;
                }
            }
        }
        (names, length)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "statement dispatch mirrors the AST one-to-one"
    )]
    pub(super) fn evaluate_statement(
        &mut self,
        dom: &mut Dom,
        statement: &Statement,
    ) -> Result<Completion, JsError> {
        self.consume_step()?;
        let result = self.evaluate_statement_dispatched(dom, statement);
        if let Err(error) = &result
            && error.offset().is_none()
            && let Some(offset) = statement_offset(statement)
        {
            return Err(error.clone().at_offset(offset));
        }
        result
    }

    fn evaluate_statement_dispatched(
        &mut self,
        dom: &mut Dom,
        statement: &Statement,
    ) -> Result<Completion, JsError> {
        if Self::statement_trace_enabled() {
            let rendered = format!("{statement:?}");
            let truncated: String = rendered.chars().take(1400).collect();
            eprintln!("[stmt depth={}] {truncated}", self.calls_active);
        }
        // Cheap depth probe independent of Debug formatting.
        if Self::depth_trace_enabled() {
            eprintln!("[d={}]", self.calls_active);
        }
        if self.calls_active == 30 && Self::depth_trace_enabled() {
            eprintln!("{}", std::backtrace::Backtrace::force_capture());
        }
        match statement {
            Statement::Variable {
                kind, name, value, ..
            } => {
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
            Statement::VariableList {
                kind, declarations, ..
            } => {
                let mut value = JsValue::Undefined;
                for (name, expression) in declarations {
                    if let Some(expression) = expression {
                        value = self.evaluate(dom, expression)?;
                        self.initialize_binding(name, value.clone(), *kind)?;
                    } else if *kind == VariableKind::Let {
                        self.initialize_binding(name, JsValue::Undefined, *kind)?;
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
                // Thrown Error instances surface as "Name: message" (what a
                // real engine prints), not "[object Object]".
                let mut error = match value {
                    JsValue::Object(object)
                        if self.realm.get_property(object, "toString").is_some_and(
                            |inherited_to_string| {
                                matches!(
                                    inherited_to_string,
                                    JsValue::Object(to_string_fn)
                                        if self.realm.host(to_string_fn)
                                            == Some(ObjectHost::NativeFunction(
                                                NativeFunction::ErrorPrototypeToString
                                            ))
                                )
                            },
                        ) =>
                    {
                        JsError::thrown_with_message(
                            value,
                            self.error_to_string(object).to_js_string(),
                        )
                    }
                    value => JsError::thrown(value),
                };
                if let Some(offset) = expr_offset(expression) {
                    error = error.at_offset(offset);
                }
                Err(error)
            }
            Statement::Try {
                body,
                catch,
                finally,
                ..
            } => self.evaluate_try_statement(dom, body, catch.as_ref(), finally.as_deref()),
            Statement::If {
                condition,
                consequent,
                alternate,
                ..
            } => {
                if self.evaluate(dom, condition)?.is_truthy() {
                    self.evaluate_statement(dom, consequent)
                } else if let Some(alternate) = alternate {
                    self.evaluate_statement(dom, alternate)
                } else {
                    Ok(Completion::Normal(JsValue::Undefined))
                }
            }
            Statement::Switch {
                expression, cases, ..
            } => self.evaluate_switch_statement(dom, expression, cases),
            Statement::While {
                condition, body, ..
            } => {
                let mut value = JsValue::Undefined;
                loop {
                    self.consume_step()?;
                    if !self.evaluate(dom, condition)?.is_truthy() {
                        break;
                    }
                    match self.evaluate_statement(dom, body)? {
                        Completion::Normal(next) => value = next,
                        Completion::Continue(None) => {}
                        Completion::Break(None) => break,
                        returned @ Completion::Return(_) => return Ok(returned),
                        labeled @ (Completion::Break(Some(_)) | Completion::Continue(Some(_))) => {
                            return Ok(labeled);
                        }
                    }
                }
                Ok(Completion::Normal(value))
            }
            Statement::DoWhile {
                condition, body, ..
            } => {
                let mut value = JsValue::Undefined;
                loop {
                    self.consume_step()?;
                    match self.evaluate_statement(dom, body)? {
                        Completion::Normal(next) => value = next,
                        Completion::Continue(None) => {}
                        Completion::Break(None) => break,
                        returned @ Completion::Return(_) => return Ok(returned),
                        labeled @ (Completion::Break(Some(_)) | Completion::Continue(Some(_))) => {
                            return Ok(labeled);
                        }
                    }
                    if !self.evaluate(dom, condition)?.is_truthy() {
                        break;
                    }
                }
                Ok(Completion::Normal(value))
            }
            Statement::For {
                initializer,
                condition,
                update,
                body,
                ..
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
                ..
            } => self.evaluate_for_in_statement(dom, *kind, name, iterable, body),
            Statement::ForOf {
                kind,
                name,
                iterable,
                body,
                ..
            } => self.evaluate_for_of_statement(dom, *kind, name, iterable, body),
            Statement::ForInExpr {
                target,
                iterable,
                body,
                ..
            } => self.evaluate_for_in_expr_statement(dom, target, iterable, body),
            // Labels bind `break label` / `continue label` to this statement;
            // unlabeled control flow binds to the nearest enclosing loop.
            Statement::Labeled { label, body, .. } => match self.evaluate_statement(dom, body)? {
                Completion::Break(Some(target)) | Completion::Continue(Some(target))
                    if *target == *label =>
                {
                    Ok(Completion::Normal(JsValue::Undefined))
                }
                other => Ok(other),
            },
            Statement::Break(label) => Ok(Completion::Break(label.clone())),
            Statement::Continue(label) => Ok(Completion::Continue(label.clone())),
            Statement::Block(statements) => self.evaluate_scoped_statements(dom, statements),
            Statement::Expression(expression) => {
                self.evaluate(dom, expression).map(Completion::Normal)
            }
        }
    }

    pub(super) fn evaluate_switch_statement(
        &mut self,
        dom: &mut Dom,
        expression: &Expr,
        cases: &[(Vec<Expr>, Vec<Statement>)],
    ) -> Result<Completion, JsError> {
        let discriminant = self.evaluate(dom, expression)?;
        let mut active = false;
        let mut value = JsValue::Undefined;
        for (tests, statements) in cases {
            if !active {
                if tests.is_empty() {
                    active = true;
                } else {
                    for test in tests {
                        if strict_equal(&discriminant, &self.evaluate(dom, test)?) {
                            active = true;
                            break;
                        }
                    }
                }
            }
            if !active {
                continue;
            }
            match self.evaluate_statements(dom, statements)? {
                Completion::Normal(next) => value = next,
                Completion::Break(None) => break,
                abrupt => return Ok(abrupt),
            }
        }
        Ok(Completion::Normal(value))
    }

    pub(super) fn evaluate_for_statement(
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
                    Completion::Continue(None) => {}
                    Completion::Break(None) => break,
                    returned @ Completion::Return(_) => return Ok(returned),
                    labeled @ (Completion::Break(Some(_)) | Completion::Continue(Some(_))) => {
                        return Ok(labeled);
                    }
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

    pub(super) fn evaluate_for_in_statement(
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
                Completion::Continue(None) => {}
                Completion::Break(None) => break,
                returned @ Completion::Return(_) => return Ok(returned),
                labeled @ (Completion::Break(Some(_)) | Completion::Continue(Some(_))) => {
                    return Ok(labeled);
                }
            }
        }
        Ok(Completion::Normal(value))
    }

    /// `for (target in iterable)` with an assignment target instead of a
    /// declared binding.
    pub(super) fn evaluate_for_in_expr_statement(
        &mut self,
        dom: &mut Dom,
        target: &Expr,
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
        for property in names {
            self.consume_step()?;
            let reference = self.resolve_assignment_reference(dom, target)?;
            self.write_assignment_reference(dom, &reference, JsValue::String(property.clone()))?;
            match self.evaluate_statement(dom, body)? {
                Completion::Normal(next) => value = next,
                Completion::Continue(None) => {}
                Completion::Break(None) => break,
                returned @ Completion::Return(_) => return Ok(returned),
                labeled @ (Completion::Break(Some(_)) | Completion::Continue(Some(_))) => {
                    return Ok(labeled);
                }
            }
        }
        Ok(Completion::Normal(value))
    }

    pub(super) fn evaluate_for_of_statement(
        &mut self,
        dom: &mut Dom,
        kind: VariableKind,
        name: &str,
        iterable: &Expr,
        body: &Statement,
    ) -> Result<Completion, JsError> {
        let iterable = self.evaluate(dom, iterable)?;
        let values = self.iterate_values(dom, &iterable)?;
        if kind == VariableKind::Var {
            self.create_binding(name, kind, true, JsValue::Undefined)?;
        }
        let mut value = JsValue::Undefined;
        for item in values {
            self.consume_step()?;
            let iteration_environment = if kind == VariableKind::Var {
                None
            } else {
                let environment = Rc::new(RefCell::new(EnvironmentRecord::default()));
                environment.borrow_mut().bindings.insert(
                    name.to_owned(),
                    Binding {
                        value: item.clone(),
                        mutable: kind != VariableKind::Const,
                        initialized: true,
                        kind,
                    },
                );
                self.environment.push(Rc::clone(&environment));
                Some(environment)
            };
            if kind == VariableKind::Var {
                self.assign_binding(name, item)?;
            }
            let completion = self.evaluate_statement(dom, body);
            if iteration_environment.is_some() {
                self.environment.pop();
            }
            match completion? {
                Completion::Normal(next) => value = next,
                Completion::Continue(None) => {}
                Completion::Break(None) => break,
                returned @ Completion::Return(_) => return Ok(returned),
                labeled @ (Completion::Break(Some(_)) | Completion::Continue(Some(_))) => {
                    return Ok(labeled);
                }
            }
        }
        Ok(Completion::Normal(value))
    }

    pub(super) fn evaluate_try_statement(
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
            // Native engine errors materialize as standard Error instances so
            // `instanceof TypeError` and `error.stack` behave like a real
            // engine inside catch blocks.
            let value = if let Some(value) = error.thrown_value().cloned() {
                value
            } else {
                let message = error.message().to_owned();
                let kind = match error.kind() {
                    JsErrorKind::Syntax => ErrorKind::SyntaxError,
                    JsErrorKind::Reference => ErrorKind::ReferenceError,
                    JsErrorKind::Type => ErrorKind::TypeError,
                    JsErrorKind::ResourceLimit => ErrorKind::RangeError,
                    JsErrorKind::Dom | JsErrorKind::Throw => ErrorKind::Error,
                };
                self.construct_standard_error(kind, &message)?
            };
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

    pub(super) fn evaluate_scoped_statements(
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

    #[allow(
        clippy::too_many_lines,
        reason = "expression dispatch mirrors the AST one-to-one"
    )]
    pub(super) fn evaluate(
        &mut self,
        dom: &mut Dom,
        expression: &Expr,
    ) -> Result<JsValue, JsError> {
        self.consume_step()?;
        let result = self.evaluate_dispatched(dom, expression);
        // Position errors at the innermost enclosing node that carries a
        // span; deeper dispatch layers attach first, so exact throw sites
        // win over enclosing wrappers.
        if let Err(error) = &result
            && error.offset().is_none()
            && let Some(offset) = expr_offset(expression)
        {
            return Err(error.clone().at_offset(offset));
        }
        result
    }

    fn evaluate_dispatched(
        &mut self,
        dom: &mut Dom,
        expression: &Expr,
    ) -> Result<JsValue, JsError> {
        match expression {
            Expr::Literal(value) => Ok(value.clone()),
            Expr::RegexLiteral { pattern, flags, .. } => {
                let object = self.construct_regex(pattern, flags)?;
                Ok(JsValue::Object(object))
            }
            Expr::This => Ok(self
                .this_stack
                .last()
                .cloned()
                // Bare `this` outside any function refers to the global object.
                .unwrap_or_else(|| JsValue::Object(self.realm.global_object()))),
            Expr::Identifier(name) => self.lookup_binding(name),
            Expr::Function {
                name,
                parameters,
                body,
                ..
            } => self.evaluate_function_expression(name.as_deref(), parameters, body),
            Expr::Arrow {
                parameters, body, ..
            } => self.create_arrow_function(parameters, body),
            Expr::Object(properties) => self.evaluate_object_literal(dom, properties),
            Expr::Array(elements) => self.evaluate_array_literal(dom, elements),
            Expr::Spread(expression) => self.evaluate(dom, expression),
            Expr::ObjectRest {
                object, excluded, ..
            } => {
                let value = self.evaluate(dom, object)?;
                self.create_object_rest(&value, excluded)
                    .map(JsValue::Object)
            }
            Expr::Unary {
                operator: UnaryOp::Delete,
                operand,
                ..
            } => self.evaluate_delete(dom, operand),
            // `typeof identifier` never throws for undeclared bindings.
            Expr::Unary {
                operator: UnaryOp::Typeof,
                operand,
                ..
            } if matches!(operand.as_ref(), Expr::Identifier(name) if !self.binding_exists(name)) => {
                Ok(JsValue::String("undefined".to_owned()))
            }
            Expr::Unary {
                operator, operand, ..
            } => {
                let value = self.evaluate(dom, operand)?;
                self.evaluate_unary(*operator, &value)
            }
            Expr::Binary {
                operator,
                left,
                right,
                ..
            } => self.evaluate_binary(dom, *operator, left, right),
            Expr::Conditional {
                condition,
                consequent,
                alternate,
                ..
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
                ..
            } => {
                let reference = self.resolve_assignment_reference(dom, target)?;
                let previous = self.read_assignment_reference(dom, &reference)?;
                let next =
                    Self::evaluate_binary_values(*operator, &previous, &JsValue::Number(1.0))?;
                self.write_assignment_reference(dom, &reference, next.clone())?;
                Ok(if *prefix { next } else { previous })
            }
            Expr::Member {
                object, property, ..
            } => {
                let evaluated = self.evaluate(dom, object)?;
                let object = self.coerce_member_base(&evaluated, property)?;
                self.get_member(dom, object, property)
            }
            Expr::ComputedMember {
                object, property, ..
            } => {
                let evaluated = self.evaluate(dom, object)?;
                let key_value = self.evaluate(dom, property)?;
                let receiver = self.coerce_member_base(&evaluated, &key_value.to_js_string())?;
                if let JsValue::Symbol(symbol) = &key_value {
                    return self.get_symbol_value(dom, receiver, symbol);
                }
                let key = key_value.to_js_string();
                self.get_member(dom, receiver, &key)
            }
            Expr::New {
                constructor,
                arguments,
                ..
            } => {
                let evaluated = self.evaluate(dom, constructor)?;
                if matches!(evaluated, JsValue::Null | JsValue::Undefined) {
                    return Ok(JsValue::Undefined);
                }
                let JsValue::Object(constructor) = evaluated else {
                    // An unavailable feature constructor evaluates to a
                    // primitive in some compatibility branches.  Treat that
                    // branch as an inert construction result so the rest of
                    // the page can continue initializing.
                    return Ok(JsValue::Undefined);
                };
                let mut values = Vec::with_capacity(arguments.len());
                for argument in arguments {
                    self.evaluate_argument(dom, argument, &mut values)?;
                }
                self.construct(dom, constructor, &values)
            }
            Expr::Call {
                callee, arguments, ..
            } => self.evaluate_call(dom, callee, arguments),
            Expr::Sequence(expressions) => {
                let mut value = JsValue::Undefined;
                for expression in expressions {
                    value = self.evaluate(dom, expression)?;
                }
                Ok(value)
            }
            Expr::CompoundAssignment {
                target,
                operator,
                value,
                ..
            } => {
                let reference = self.resolve_assignment_reference(dom, target)?;
                let current = self.read_assignment_reference(dom, &reference)?;
                let right = self.evaluate(dom, value)?;
                // Match the plain binary path: object operands go through
                // ToPrimitive so `x += value` agrees with `x = x + value`.
                let combined = match *operator {
                    BinaryOp::Add
                    | BinaryOp::Subtract
                    | BinaryOp::Multiply
                    | BinaryOp::Divide
                    | BinaryOp::Remainder
                    | BinaryOp::Exponentiate => {
                        let current = self.to_numeric_primitive(dom, current)?;
                        let right = self.to_numeric_primitive(dom, right)?;
                        Self::evaluate_binary_values(*operator, &current, &right)?
                    }
                    _ => Self::evaluate_binary_values(*operator, &current, &right)?,
                };
                self.write_assignment_reference(dom, &reference, combined.clone())?;
                Ok(combined)
            }
            Expr::Assignment { target, value, .. } => {
                if matches!(target.as_ref(), Expr::Array(_) | Expr::Object(_)) {
                    let value = self.evaluate(dom, value)?;
                    self.assign_destructuring_target(dom, target, value.clone())?;
                    return Ok(value);
                }
                let reference = self.resolve_assignment_reference(dom, target)?;
                let value = self.evaluate(dom, value)?;
                self.write_assignment_reference(dom, &reference, value.clone())?;
                Ok(value)
            }
        }
    }

    pub(super) fn resolve_assignment_reference(
        &mut self,
        dom: &mut Dom,
        target: &Expr,
    ) -> Result<AssignmentReference, JsError> {
        match target {
            Expr::Identifier(name) => Ok(AssignmentReference::Binding(name.clone())),
            Expr::Member {
                object, property, ..
            } => {
                let evaluated = self.evaluate(dom, object)?;
                let object = self.coerce_member_base(&evaluated, property)?;
                Ok(AssignmentReference::Property {
                    object,
                    property: property.clone(),
                })
            }
            Expr::ComputedMember {
                object, property, ..
            } => {
                let evaluated = self.evaluate(dom, object)?;
                let key_value = self.evaluate(dom, property)?;
                let key_text = key_value.to_js_string();
                let object = self.coerce_member_base(&evaluated, &key_text)?;
                if let JsValue::Symbol(symbol) = key_value {
                    return Ok(AssignmentReference::SymbolProperty { object, symbol });
                }
                Ok(AssignmentReference::Property {
                    object,
                    property: key_text,
                })
            }
            _ => Err(JsError::new(
                JsErrorKind::Syntax,
                "invalid assignment target",
                None,
            )),
        }
    }

    pub(super) fn assign_destructuring_target(
        &mut self,
        dom: &mut Dom,
        target: &Expr,
        value: JsValue,
    ) -> Result<(), JsError> {
        match target {
            Expr::Identifier(_) | Expr::Member { .. } | Expr::ComputedMember { .. } => {
                let reference = self.resolve_assignment_reference(dom, target)?;
                self.write_assignment_reference(dom, &reference, value)
            }
            Expr::Assignment {
                target,
                value: default,
                ..
            } => {
                let value = if matches!(value, JsValue::Undefined) {
                    self.evaluate(dom, default)?
                } else {
                    value
                };
                self.assign_destructuring_target(dom, target, value)
            }
            Expr::Array(targets) => {
                let values = self.iterate_values(dom, &value)?;
                for (index, target) in targets.iter().enumerate() {
                    if matches!(target, Expr::Literal(JsValue::Undefined)) {
                        continue;
                    }
                    if let Expr::Spread(target) = target {
                        let rest =
                            self.create_array_from_values(values.get(index..).unwrap_or_default())?;
                        self.assign_destructuring_target(dom, target, JsValue::Object(rest))?;
                        break;
                    }
                    self.assign_destructuring_target(
                        dom,
                        target,
                        values.get(index).cloned().unwrap_or(JsValue::Undefined),
                    )?;
                }
                Ok(())
            }
            Expr::Object(properties) => {
                let object = match value {
                    JsValue::Null | JsValue::Undefined => self.realm.create_ordinary_object(),
                    JsValue::Object(object) => object,
                    _ => Self::require_object(&value)?,
                };
                let mut excluded = Vec::new();
                for property in properties {
                    if matches!(&property.key, PropertyKey::Spread) {
                        let rest = self.create_object_rest(&value, &excluded)?;
                        self.assign_destructuring_target(
                            dom,
                            &property.value,
                            JsValue::Object(rest),
                        )?;
                        continue;
                    }
                    let key = match &property.key {
                        PropertyKey::Static(key) => key.clone(),
                        PropertyKey::Computed(expression) => {
                            self.evaluate(dom, expression)?.to_js_string()
                        }
                        PropertyKey::Spread => unreachable!("spread handled above"),
                    };
                    let property_value = self.get_member(dom, object, &key)?;
                    excluded.push(key);
                    self.assign_destructuring_target(dom, &property.value, property_value)?;
                }
                Ok(())
            }
            _ => Err(JsError::syntax(
                "invalid destructuring assignment target",
                0,
            )),
        }
    }

    pub(super) fn read_assignment_reference(
        &mut self,
        dom: &mut Dom,
        reference: &AssignmentReference,
    ) -> Result<JsValue, JsError> {
        match reference {
            AssignmentReference::Binding(name) => self.lookup_binding(name),
            AssignmentReference::Property { object, property } => {
                self.get_member(dom, *object, property)
            }
            AssignmentReference::SymbolProperty { object, symbol } => {
                self.get_symbol_value(dom, *object, symbol)
            }
        }
    }

    pub(super) fn write_assignment_reference(
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
            AssignmentReference::SymbolProperty { object, symbol } => {
                self.set_symbol_value(dom, *object, symbol, value)
            }
        }
    }

    pub(super) fn evaluate_delete(
        &mut self,
        dom: &mut Dom,
        operand: &Expr,
    ) -> Result<JsValue, JsError> {
        match operand {
            Expr::Member { .. } | Expr::ComputedMember { .. } => {
                let reference = self.resolve_assignment_reference(dom, operand)?;
                match reference {
                    AssignmentReference::Property { object, property } => {
                        if let Some(ObjectHost::DataSet(node)) = self.realm.host(object) {
                            return Ok(JsValue::Boolean(JsRuntime::delete_dataset_member(
                                dom, node, &property,
                            )?));
                        }
                        Ok(JsValue::Boolean(
                            self.realm.delete_property(object, &property),
                        ))
                    }
                    AssignmentReference::SymbolProperty { object, symbol } => Ok(JsValue::Boolean(
                        self.realm.delete_symbol_property(object, &symbol),
                    )),
                    AssignmentReference::Binding(_) => {
                        unreachable!("member expressions resolve to property references");
                    }
                }
            }
            Expr::Identifier(_) => Ok(JsValue::Boolean(false)),
            _ => {
                self.evaluate(dom, operand)?;
                Ok(JsValue::Boolean(true))
            }
        }
    }

    /// Numeric-hint `ToPrimitive` for the hosts we can convert directly.
    pub(super) fn numeric_primitive(&self, value: &JsValue) -> JsValue {
        if let JsValue::Object(object) = value {
            return match self.realm.host(*object) {
                Some(ObjectHost::DateInstance(ms)) => JsValue::Number(ms),
                Some(ObjectHost::StringPrimitive(text)) => JsValue::String(text.clone()),
                Some(ObjectHost::NumberPrimitive(number)) => JsValue::Number(number),
                Some(ObjectHost::BooleanPrimitive(value)) => JsValue::Boolean(value),
                _ => value.clone(),
            };
        }
        value.clone()
    }

    /// ECMA-262 [[Get]]: read a property through the prototype chain,
    /// invoking accessor getters with `this` bound to the original receiver.
    /// Ordinary property reads keep using `Realm::get_property` fast paths;
    /// this entry point is required wherever accessors may exist.
    pub(super) fn get_value(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        key: &str,
    ) -> Result<JsValue, JsError> {
        match self.realm.get_descriptor(object, key) {
            Some(descriptor) if descriptor.is_accessor() => {
                let getter = match descriptor.getter {
                    Some(getter) => getter,
                    None => return Ok(JsValue::Undefined),
                };
                self.call_with_this(dom, getter, &[], JsValue::Object(object))
            }
            Some(descriptor) => Ok(descriptor.value),
            None => Ok(JsValue::Undefined),
        }
    }

    /// ECMA-262 : call `value[Symbol.iterator]()` and read the
    /// `next` method. `Ok(None)` when the value exposes no @@iterator.
    pub(super) fn get_iterator(
        &mut self,
        dom: &mut Dom,
        value: &JsValue,
    ) -> Result<Option<(ObjectId, ObjectId)>, JsError> {
        let JsValue::Object(target) = value else {
            return Ok(None);
        };
        let method = self.get_symbol_value(dom, *target, &JsSymbol::well_known("@@iterator"))?;
        let JsValue::Object(method) = method else {
            return Ok(None);
        };
        let result = self.call_with_this(dom, method, &[], value.clone())?;
        let JsValue::Object(iterator) = result else {
            return Err(JsError::type_error("iterator result is not an object"));
        };
        let next = self.get_member(dom, iterator, "next")?;
        let JsValue::Object(next) = next else {
            return Err(JsError::type_error("iterator has no callable 'next'"));
        };
        if !Self::is_callable_object(next, &self.realm) {
            return Err(JsError::type_error("iterator has no callable 'next'"));
        }
        Ok(Some((iterator, next)))
    }

    /// Step an iterator; `Ok(None)` marks exhaustion (`done`).
    pub(super) fn iterator_next(
        &mut self,
        dom: &mut Dom,
        iterator: ObjectId,
        next: ObjectId,
    ) -> Result<Option<JsValue>, JsError> {
        let result = self.call_with_this(dom, next, &[], JsValue::Object(iterator))?;
        let JsValue::Object(result) = result else {
            return Err(JsError::type_error("iterator result is not an object"));
        };
        let done = self
            .realm
            .get_property(result, "done")
            .map(|value| value.is_truthy())
            .unwrap_or(false);
        if done {
            return Ok(None);
        }
        Ok(Some(
            self.realm
                .get_property(result, "value")
                .unwrap_or(JsValue::Undefined),
        ))
    }

    /// Drain any iterable into an eager value list. Arrays and strings use
    /// the established fast paths; other objects go through the iterator
    /// protocol, with a length-based fallback for array-likes (typed arrays
    /// and `arguments` shapes) that predate iterator support.
    pub(super) fn iterate_values(
        &mut self,
        dom: &mut Dom,
        value: &JsValue,
    ) -> Result<Vec<JsValue>, JsError> {
        match value {
            JsValue::Object(object)
                if matches!(self.realm.host(*object), Some(ObjectHost::Array)) =>
            {
                Ok(self.array_elements_for(*object))
            }
            JsValue::String(text) => Ok(text
                .chars()
                .map(|character| JsValue::String(character.to_string()))
                .collect()),
            JsValue::Object(object) => {
                if let Some((iterator, next)) = self.get_iterator(dom, value)? {
                    let mut values = Vec::new();
                    while let Some(step) = self.iterator_next(dom, iterator, next)? {
                        values.push(step);
                    }
                    return Ok(values);
                }
                let array_like = self
                    .realm
                    .get_property(*object, "length")
                    .map(|length| to_number(&length).map(|length| length.max(0.0) as usize))
                    .unwrap_or(Ok(0))?;
                (0..array_like)
                    .map(|index| self.get_member(dom, *object, &index.to_string()))
                    .collect()
            }
            JsValue::Null | JsValue::Undefined | JsValue::Symbol(_) => Err(JsError::type_error(
                format!("{} is not iterable", value.to_js_string()),
            )),
            _ => Ok(Vec::new()),
        }
    }

    /// [[Get]] for a symbol-keyed property.
    pub(super) fn get_symbol_value(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        symbol: &JsSymbol,
    ) -> Result<JsValue, JsError> {
        match self.realm.get_symbol_descriptor(object, symbol) {
            Some(descriptor) if descriptor.is_accessor() => {
                let Some(getter) = descriptor.getter else {
                    return Ok(JsValue::Undefined);
                };
                self.call_with_this(dom, getter, &[], JsValue::Object(object))
            }
            Some(descriptor) => Ok(descriptor.value),
            None => Ok(JsValue::Undefined),
        }
    }

    /// [[Set]] for a symbol-keyed property.
    pub(super) fn set_symbol_value(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        symbol: &JsSymbol,
        value: JsValue,
    ) -> Result<(), JsError> {
        if let Some(own) = self.realm.own_symbol_property(object, symbol) {
            if own.is_accessor() {
                if let Some(setter) = own.setter {
                    self.call_with_this(dom, setter, &[value], JsValue::Object(object))?;
                }
                return Ok(());
            }
            self.realm
                .define_symbol_property(object, symbol, PropertyDescriptor::data(value));
            return Ok(());
        }
        let inherited = self.realm.get_symbol_descriptor(object, symbol);
        if let Some(descriptor) =
            inherited.filter(crate::value::PropertyDescriptor::is_accessor)
        {
            if let Some(setter) = descriptor.setter {
                self.call_with_this(dom, setter, &[value], JsValue::Object(object))?;
            }
            return Ok(());
        }
        self.realm
            .define_symbol_property(object, symbol, PropertyDescriptor::data(value));
        Ok(())
    }

    /// ECMA-262 [[Set]] with a receiver distinct from the lookup start
    /// object: update an own data property, invoke an inherited setter, or
    /// create a fresh own data property (sloppy mode: a getter-only
    /// inherited accessor silently ignores the write).
    pub(super) fn set_value(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        key: &str,
        value: JsValue,
    ) -> Result<(), JsError> {
        if let Some(own) = self.realm.own_property(object, key) {
            if own.is_accessor() {
                if let Some(setter) = own.setter {
                    self.call_with_this(dom, setter, &[value], JsValue::Object(object))?;
                }
                return Ok(());
            }
            // Sloppy-mode [[Set]]: a non-writable data property silently
            // ignores the write (strict mode would throw).
            self.realm.set_property(object, key.to_owned(), value);
            return Ok(());
        }
        let inherited = self.realm.get_descriptor(object, key);
        if let Some(descriptor) =
            inherited.filter(crate::value::PropertyDescriptor::is_accessor)
        {
            if let Some(setter) = descriptor.setter {
                self.call_with_this(dom, setter, &[value], JsValue::Object(object))?;
            }
            return Ok(());
        }
        if !self.realm.set_property(object, key.to_owned(), value) {
            return Err(JsError::type_error(format!(
                "property {key:?} is not writable"
            )));
        }
        Ok(())
    }

    pub(super) fn to_numeric_primitive(
        &mut self,
        dom: &mut Dom,
        value: JsValue,
    ) -> Result<JsValue, JsError> {
        self.to_primitive_with_hint(dom, value, false)
    }

    /// ECMA-262 `ToString` for values that may be objects: run `ToPrimitive`
    /// (string hint) so user-defined `toString`/`valueOf` participate, the
    /// way real-world code and polyfills (`String(obj)`) expect.
    pub(super) fn to_string_value(
        &mut self,
        dom: &mut Dom,
        value: &JsValue,
    ) -> Result<String, JsError> {
        let primitive = self.to_primitive_with_hint(dom, value.clone(), true)?;
        if std::env::var_os("RENDER_TRACE_STRING").is_some()
            && let JsValue::Object(object) = value
        {
            let tag = self
                .realm
                .get_symbol_descriptor(*object, &JsSymbol::well_known("@@toStringTag"))
                .map(|descriptor| descriptor.value.to_js_string());
            eprintln!(
                "TRACE String(obj) -> {:?} tag={tag:?}",
                primitive.to_js_string()
            );
        }
        Ok(primitive.to_js_string())
    }

    fn to_primitive_with_hint(
        &mut self,
        dom: &mut Dom,
        value: JsValue,
        prefer_string: bool,
    ) -> Result<JsValue, JsError> {
        let JsValue::Object(object) = value else {
            return Ok(value);
        };
        match self.realm.host(object) {
            Some(ObjectHost::DateInstance(ms)) => return Ok(JsValue::Number(ms)),
            Some(ObjectHost::StringPrimitive(text)) => return Ok(JsValue::String(text)),
            Some(ObjectHost::NumberPrimitive(number)) => return Ok(JsValue::Number(number)),
            Some(ObjectHost::BooleanPrimitive(value)) => return Ok(JsValue::Boolean(value)),
            Some(ObjectHost::Array) => {
                let text = self
                    .array_elements_for(object)
                    .iter()
                    .map(|value| match value {
                        JsValue::Null | JsValue::Undefined => String::new(),
                        value => value.to_js_string(),
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                return Ok(JsValue::String(text));
            }
            _ => {}
        }
        // An exotic `Symbol.toPrimitive` method gets first refusal, called
        // with the coercion hint; a primitive result short-circuits.
        let hint = if prefer_string { "string" } else { "default" };
        let to_primitive_method = self
            .realm
            .get_symbol_descriptor(object, &JsSymbol::well_known("@@toPrimitive"))
            .and_then(|descriptor| {
                if descriptor.is_accessor() {
                    descriptor.getter
                } else {
                    match descriptor.value {
                        JsValue::Object(function) => Some(function),
                        _ => None,
                    }
                }
            });
        if let Some(method) = to_primitive_method {
            let invoked = self.call_with_this(
                dom,
                method,
                &[JsValue::String(hint.to_owned())],
                JsValue::Object(object),
            )?;
            if !matches!(invoked, JsValue::Object(_)) {
                return Ok(invoked);
            }
            return Err(JsError::type_error(
                "Cannot convert object to primitive value",
            ));
        }
        let method_order: [&str; 2] = if prefer_string {
            ["toString", "valueOf"]
        } else {
            ["valueOf", "toString"]
        };
        for method in method_order {
            let Some(JsValue::Object(callable)) = self.realm.get_property(object, method) else {
                continue;
            };
            if !Self::is_callable_object(callable, &self.realm) {
                continue;
            }
            let result = self.call_with_this(dom, callable, &[], JsValue::Object(object))?;
            if !matches!(result, JsValue::Object(_)) {
                return Ok(result);
            }
        }
        // Host objects without a usable `valueOf`/`toString` still have the
        // ordinary object string representation.  Returning it keeps string
        // concatenation and URL/logging code from aborting on an incomplete
        // platform object.
        Ok(JsValue::String("[object Object]".to_owned()))
    }

    /// Whether `name` resolves in any scope, the global bindings, or the
    /// global object itself.
    pub(super) fn binding_exists(&self, name: &str) -> bool {
        if self
            .environment
            .iter()
            .rev()
            .any(|scope| scope.borrow().bindings.contains_key(name))
        {
            return true;
        }
        self.global_bindings.contains_key(name) || self.realm.global(name).is_some()
    }

    pub(super) fn evaluate_binary_values(
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

    pub(super) fn evaluate_call(
        &mut self,
        dom: &mut Dom,
        callee: &Expr,
        arguments: &[Expr],
    ) -> Result<JsValue, JsError> {
        let callee_label = match callee {
            Expr::Member { property, .. } => format!(".{property}"),
            Expr::ComputedMember { .. } => "[]".to_owned(),
            _ => String::new(),
        };
        let (callee_value, receiver) = match callee {
            Expr::Member {
                object, property, ..
            } => {
                let receiver = self.evaluate(dom, object)?;
                if matches!(receiver, JsValue::Null | JsValue::Undefined) {
                    return Ok(JsValue::Undefined);
                }
                let object = self.coerce_member_base(&receiver, property)?;
                (self.get_member(dom, object, property)?, receiver)
            }
            Expr::ComputedMember {
                object, property, ..
            } => {
                let receiver = self.evaluate(dom, object)?;
                if matches!(receiver, JsValue::Null | JsValue::Undefined) {
                    return Ok(JsValue::Undefined);
                }
                let key = self.evaluate(dom, property)?.to_js_string();
                let object = self.coerce_member_base(&receiver, &key)?;
                (self.get_member(dom, object, &key)?, receiver)
            }
            _ => (self.evaluate(dom, callee)?, JsValue::Undefined),
        };
        let callee = match callee_value {
            JsValue::Undefined | JsValue::Null => {
                // Web pages routinely feature-detect optional host methods
                // through a call guarded by a surrounding branch. Treat a
                // missing host hook as an inert call so one telemetry shim
                // cannot abort the entire application bootstrap.
                return Ok(JsValue::Undefined);
            }
            JsValue::String(_) | JsValue::Number(_) | JsValue::Boolean(_) => {
                return Ok(JsValue::Undefined);
            }
            JsValue::Symbol(_) => {
                return Ok(JsValue::Undefined);
            }
            value @ JsValue::Object(_) => Self::require_object(&value).map_err(|_| {
                JsError::type_error(format!(
                    "value of callee{callee_label} is undefined or not callable"
                ))
            })?,
        };
        let mut values = Vec::with_capacity(arguments.len());
        for argument in arguments {
            self.evaluate_argument(dom, argument, &mut values)?;
        }
        self.call_with_this(dom, callee, &values, receiver)
    }

    pub(super) fn evaluate_function_expression(
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
            let value = self.create_function(Some(name), parameters, body, None)?;
            self.initialize_binding(name, value.clone(), VariableKind::Const)?;
            Ok(value)
        })();
        self.environment.pop();
        result
    }

    pub(super) fn evaluate_object_literal(
        &mut self,
        dom: &mut Dom,
        properties: &[ObjectProperty],
    ) -> Result<JsValue, JsError> {
        self.ensure_heap_capacity(1)?;
        let object = self.realm.create_ordinary_object();
        for property in properties {
            if matches!(property.key, PropertyKey::Spread) {
                match self.evaluate(dom, &property.value)? {
                    JsValue::Null | JsValue::Undefined => {}
                    JsValue::Object(source) => {
                        for (key, value) in self
                            .realm
                            .enumerable_own_properties(source)
                            .unwrap_or_default()
                        {
                            if !self.realm.set_property(object, key, value) {
                                return Err(JsError::type_error(
                                    "could not define spread object property",
                                ));
                            }
                        }
                    }
                    JsValue::String(text) => {
                        for (index, character) in text.chars().enumerate() {
                            if !self.realm.set_property(
                                object,
                                index.to_string(),
                                JsValue::String(character.to_string()),
                            ) {
                                return Err(JsError::type_error(
                                    "could not define spread string property",
                                ));
                            }
                        }
                    }
                    JsValue::Boolean(_) | JsValue::Number(_) | JsValue::Symbol(_) => {}
                }
                continue;
            }
            // The key expression evaluates exactly once (spec); a symbol
            // result installs a symbol-keyed property instead.
            let key_value = match &property.key {
                PropertyKey::Static(key) => JsValue::String(key.clone()),
                PropertyKey::Computed(expression) => self.evaluate(dom, expression)?,
                PropertyKey::Spread => unreachable!("spread property handled above"),
            };
            let symbol_key = match &key_value {
                JsValue::Symbol(symbol) => Some(symbol.clone()),
                _ => None,
            };
            if let Some(symbol) = symbol_key {
                let value = self.evaluate(dom, &property.value)?;
                if !self.realm.define_symbol_property(
                    object,
                    &symbol,
                    PropertyDescriptor {
                        getter: None,
                        setter: None,
                        value,
                        writable: true,
                        enumerable: true,
                        configurable: true,
                    },
                ) {
                    return Err(JsError::type_error("could not define symbol property"));
                }
                continue;
            }
            let key = key_value.to_js_string();
            let value = self.evaluate(dom, &property.value)?;
            if let Some(accessor) = property.accessor {
                // `{get x(){}}` / `{set x(v){}}` install accessor slots; a
                // repeated member of either kind extends the same descriptor.
                let function = match value {
                    JsValue::Object(function)
                        if JsRuntime::is_callable_object(function, &self.realm) =>
                    {
                        function
                    }
                    _ => return Err(JsError::type_error("object accessor must be a function")),
                };
                let existing = self.realm.own_property(object, &key);
                let (getter, setter) = match (accessor, existing) {
                    (ObjectAccessorKind::Getter, Some(existing)) => {
                        (Some(function), existing.setter)
                    }
                    (ObjectAccessorKind::Setter, Some(existing)) => {
                        (existing.getter, Some(function))
                    }
                    (ObjectAccessorKind::Getter, None) => (Some(function), None),
                    (ObjectAccessorKind::Setter, None) => (None, Some(function)),
                };
                if !self.realm.define_property(
                    object,
                    key,
                    PropertyDescriptor {
                        value: JsValue::Undefined,
                        writable: false,
                        getter,
                        setter,
                        enumerable: true,
                        configurable: true,
                    },
                ) {
                    return Err(JsError::type_error("could not define object accessor"));
                }
                continue;
            }
            if !self.realm.set_property(object, key, value) {
                return Err(JsError::type_error("could not define object property"));
            }
        }
        Ok(JsValue::Object(object))
    }

    pub(super) fn create_object_rest(
        &mut self,
        value: &JsValue,
        excluded: &[String],
    ) -> Result<ObjectId, JsError> {
        if matches!(value, JsValue::Null | JsValue::Undefined) {
            return Ok(self.realm.create_ordinary_object());
        }
        let source = Self::require_object(value)?;
        self.ensure_heap_capacity(1)?;
        let result = self.realm.create_ordinary_object();
        for (key, value) in self
            .realm
            .enumerable_own_properties(source)
            .unwrap_or_default()
        {
            if !excluded.contains(&key) && !self.realm.set_property(result, key, value) {
                return Err(JsError::type_error("could not define object rest property"));
            }
        }
        Ok(result)
    }

    pub(super) fn evaluate_array_literal(
        &mut self,
        dom: &mut Dom,
        elements: &[Expr],
    ) -> Result<JsValue, JsError> {
        self.ensure_heap_capacity(1)?;
        let object = self.realm.create_array();
        let mut values = Vec::with_capacity(elements.len());
        for expression in elements {
            self.evaluate_argument(dom, expression, &mut values)?;
        }
        for (index, value) in values.iter().cloned().enumerate() {
            if !self.realm.set_property(object, index.to_string(), value) {
                return Err(JsError::type_error("could not define array element"));
            }
        }
        let length = u32::try_from(values.len())
            .map_err(|_| JsError::resource("array literal exceeds the supported u32 range"))?;
        if !self.realm.set_property(
            object,
            "length".to_owned(),
            JsValue::Number(f64::from(length)),
        ) {
            return Err(JsError::type_error("could not define array length"));
        }
        Ok(JsValue::Object(object))
    }

    pub(super) fn evaluate_argument(
        &mut self,
        dom: &mut Dom,
        expression: &Expr,
        output: &mut Vec<JsValue>,
    ) -> Result<(), JsError> {
        let Expr::Spread(iterable) = expression else {
            output.push(self.evaluate(dom, expression)?);
            return Ok(());
        };
        let iterable = self.evaluate(dom, iterable)?;
        output.extend(self.iterate_values(dom, &iterable)?);
        Ok(())
    }

    pub(super) fn evaluate_unary(
        &self,
        operator: UnaryOp,
        value: &JsValue,
    ) -> Result<JsValue, JsError> {
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
                    JsValue::Symbol(_) => "symbol",
                }
                .to_owned(),
            )),
            UnaryOp::Plus => Ok(JsValue::Number(to_number(&self.numeric_primitive(value))?)),
            UnaryOp::Minus => Ok(JsValue::Number(-to_number(&self.numeric_primitive(value))?)),
            UnaryOp::BitwiseNot => Ok(JsValue::Number(f64::from(!to_int32(value)?))),
            UnaryOp::Void => Ok(JsValue::Undefined),
            UnaryOp::Delete => unreachable!("delete evaluates an assignment reference"),
        }
    }

    pub(super) fn evaluate_binary(
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
        // Logical operators return one of their original operands. Applying
        // ToPrimitive here changes objects such as `globalThis` into
        // "[object Object]", which breaks feature detection patterns like
        // `typeof globalThis !== "undefined" && globalThis`.
        if matches!(operator, BinaryOp::LogicalAnd | BinaryOp::LogicalOr) {
            return Ok(right);
        }
        if operator == BinaryOp::Instanceof {
            return self.instanceof(dom, &left, &right).map(JsValue::Boolean);
        }
        if operator == BinaryOp::In {
            return self.property_in(&left, &right).map(JsValue::Boolean);
        }
        // ECMA-262 IsLooselyEqual/IsStrictEqual: equality operators never
        // coerce their operands through ToPrimitive here — strict equality
        // compares raw values and loose equality applies the spec algorithm
        // in abstract_equal. Arithmetic and relational operators below keep
        // the numeric conversion.
        match operator {
            BinaryOp::StrictEqual => {
                return Ok(JsValue::Boolean(strict_equal(&left, &right)));
            }
            BinaryOp::StrictNotEqual => {
                return Ok(JsValue::Boolean(!strict_equal(&left, &right)));
            }
            BinaryOp::Equal => {
                return Ok(JsValue::Boolean(abstract_equal(&left, &right)?));
            }
            BinaryOp::NotEqual => {
                return Ok(JsValue::Boolean(!abstract_equal(&left, &right)?));
            }
            _ => {}
        }
        let left = self.to_numeric_primitive(dom, left)?;
        let right = self.to_numeric_primitive(dom, right)?;
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
            BinaryOp::Exponentiate => {
                Ok(JsValue::Number(to_number(&left)?.powf(to_number(&right)?)))
            }
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
            BinaryOp::In => unreachable!("in is handled before numeric operators"),
        }
    }

    /// The `in` operator: property existence on objects, index bounds on
    /// strings.
    pub(super) fn property_in(&self, key: &JsValue, container: &JsValue) -> Result<bool, JsError> {
        if let JsValue::Symbol(symbol) = key {
            return match container {
                JsValue::Object(object) => {
                    Ok(self.realm.get_symbol_descriptor(*object, symbol).is_some())
                }
                _ => Err(JsError::type_error(
                    "right-hand side of 'in' must be an object",
                )),
            };
        }
        let name = key.to_js_string();
        match container {
            JsValue::Object(object) => {
                if self.realm.get_property(*object, &name).is_some() {
                    return Ok(true);
                }
                if let Some(ObjectHost::StringPrimitive(text)) = self.realm.host(*object) {
                    let characters: Vec<char> = text.chars().collect();
                    if let Ok(index) = name.parse::<usize>() {
                        return Ok(index < characters.len());
                    }
                    return Ok(name == "length");
                }
                Ok(false)
            }
            _ => Err(JsError::type_error(
                "right-hand side of 'in' must be an object",
            )),
        }
    }

    pub(super) fn instanceof(
        &mut self,
        dom: &mut Dom,
        value: &JsValue,
        constructor: &JsValue,
    ) -> Result<bool, JsError> {
        let constructor = Self::require_object(constructor)?;
        if !Self::is_callable_object(constructor, &self.realm) {
            return Err(JsError::type_error(format!(
                "right-hand side of instanceof is not callable: host={:?}",
                self.realm.host(constructor)
            )));
        }
        // `Symbol.hasInstance` overrides the prototype-chain walk.
        let has_instance_method = self
            .realm
            .get_symbol_descriptor(constructor, &JsSymbol::well_known("@@hasInstance"))
            .and_then(|descriptor| {
                if descriptor.is_accessor() {
                    descriptor.getter
                } else {
                    match descriptor.value {
                        JsValue::Object(function) => Some(function),
                        _ => None,
                    }
                }
            });
        if let Some(method) =
            has_instance_method.filter(|method| Self::is_callable_object(*method, &self.realm))
        {
            let result = self.call_with_this(
                dom,
                method,
                std::slice::from_ref(value),
                JsValue::Object(constructor),
            )?;
            return Ok(result.is_truthy());
        }
        let prototype = self
            .realm
            .get_property(constructor, "prototype")
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            });
        let Some(prototype) = prototype else {
            // Transpiled feature probes occasionally use a callable shim with
            // a primitive prototype.  It cannot match any object, so the
            // observable result is simply false.
            return Ok(false);
        };
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

    pub(super) fn create_binding(
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
        if Self::binding_trace_enabled() && name == "document" {
            eprintln!(
                "[bind create document depth={} scopes={} initialized={initialized} | stack {:?}]",
                self.calls_active,
                self.environment.len(),
                self.call_stack
            );
        }
        Ok(())
    }

    pub(super) fn initialize_binding(
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
                if Self::binding_trace_enabled() && name == "document" {
                    let assigned_object = matches!(binding.value, JsValue::Object(_));
                    eprintln!(
                        "[bind assign document depth={} scopes={} is_object={assigned_object}]",
                        self.calls_active,
                        self.environment.len()
                    );
                }
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

    pub(super) fn lookup_binding(&self, name: &str) -> Result<JsValue, JsError> {
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

    pub(super) fn assign_binding(&mut self, name: &str, value: JsValue) -> Result<(), JsError> {
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
        }
        // Implicit global creation (sloppy-mode assignment to undeclared).
        // Per spec, strict mode would throw here; we treat all code as sloppy.
        if self.realm.set_global(name.to_owned(), value) {
            Ok(())
        } else {
            Err(JsError::type_error(format!(
                "global property {name:?} is not writable"
            )))
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn get_member(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        property: &str,
    ) -> Result<JsValue, JsError> {
        self.consume_step()?;
        // Own data properties win outright (instance fields such as a
        // RegExp's `source`); own accessors run their getter.
        if let Some(descriptor) = self.realm.own_property(object, property) {
            if descriptor.is_accessor() {
                return self.get_value(dom, object, property);
            }
            return Ok(descriptor.value);
        }
        let inherited_origin = self.realm.get_property_with_origin(object, property);
        if object == self.realm.global_object() {
            let value = match property {
                "innerWidth" | "outerWidth" => Some(self.viewport.width),
                "innerHeight" | "outerHeight" => Some(self.viewport.height),
                "scrollX" | "pageXOffset" => Some(self.viewport.x),
                "scrollY" | "pageYOffset" => Some(self.viewport.y),
                _ => None,
            };
            if let Some(value) = value {
                return Ok(JsValue::Number(f64::from(value)));
            }
        }
        // Node identity properties apply to every node wrapper, including
        // the Document host.
        if matches!(
            self.realm.host(object),
            Some(ObjectHost::Document(_) | ObjectHost::Node(_))
        ) {
            let Some(ObjectHost::Document(node_id) | ObjectHost::Node(node_id)) =
                self.realm.host(object)
            else {
                unreachable!("checked above")
            };
            match property {
                "nodeType" => {
                    return Ok(JsValue::Number(
                        match dom.node(node_id).map(render_dom::Node::kind) {
                            Some(NodeKind::Element(_)) => 1.0,
                            Some(NodeKind::Text(_)) => 3.0,
                            Some(NodeKind::Comment(_)) => 8.0,
                            Some(NodeKind::Document) => 9.0,
                            Some(NodeKind::DocumentType(_)) => 10.0,
                            Some(NodeKind::DocumentFragment) => 11.0,
                            _ => 0.0,
                        },
                    ));
                }
                "nodeName" | "tagName" => {
                    let name = match dom.node(node_id).map(render_dom::Node::kind) {
                        Some(NodeKind::Element(element)) => element.local_name.to_ascii_uppercase(),
                        Some(other) => other.name().to_owned(),
                        None => String::new(),
                    };
                    return Ok(JsValue::String(name));
                }
                _ => {}
            }
        }
        match self.realm.host(object) {
            Some(ObjectHost::Document(document)) => match property {
                "documentElement" | "body" | "head" => {
                    let tag = match property {
                        "documentElement" => "html",
                        "body" => "body",
                        _ => "head",
                    };
                    return match self.find_element_by_tag(dom, document, tag)? {
                        Some(node) => self.wrap_node(node),
                        None => Ok(JsValue::Null),
                    };
                }
                "readyState" => return Ok(JsValue::String("complete".to_owned())),
                "cookie" => {
                    return Ok(JsValue::String(self.js_cookie_jar_serialize()));
                }
                // The embedding window is the realm's global object.
                "defaultView" | "parentWindow" => {
                    return Ok(JsValue::Object(self.realm.global_object()));
                }
                "activeElement" => {
                    return match self.find_element_by_tag(dom, document, "body")? {
                        Some(node) => self.wrap_node(node),
                        None => Ok(JsValue::Null),
                    };
                }
                _ => {}
            },
            Some(ObjectHost::Node(node)) => match property {
                "textContent" => return self.text_content(dom, node).map(JsValue::String),
                "clientWidth" | "offsetWidth" | "scrollWidth" => {
                    let width = self
                        .element_geometry
                        .get(&node.as_u64())
                        .map_or(0.0, |rect| rect.width);
                    return Ok(JsValue::Number(f64::from(width)));
                }
                "clientHeight" | "offsetHeight" | "scrollHeight" => {
                    let height = self
                        .element_geometry
                        .get(&node.as_u64())
                        .map_or(0.0, |rect| rect.height);
                    return Ok(JsValue::Number(f64::from(height)));
                }
                "attributes" => {
                    self.ensure_heap_capacity(1)?;
                    return Ok(JsValue::Object(self.realm.named_node_map_wrapper(node)));
                }
                "classList" => {
                    self.ensure_heap_capacity(1)?;
                    return Ok(JsValue::Object(self.realm.class_list_wrapper(node)));
                }
                "dataset" => {
                    self.ensure_heap_capacity(1)?;
                    return Ok(JsValue::Object(self.realm.dataset_wrapper(node)));
                }
                "style" => {
                    self.ensure_heap_capacity(1)?;
                    return Ok(JsValue::Object(self.realm.style_declaration_wrapper(node)));
                }
                "innerHTML" | "outerHTML" => {
                    let html = if property == "innerHTML" {
                        serialize_html_fragment(dom, node)
                    } else {
                        serialize_html_node(dom, node)
                    };
                    return Ok(JsValue::String(html));
                }
                "className" => {
                    return Ok(JsValue::String(
                        dom.attribute(node, "class")?.unwrap_or_default().to_owned(),
                    ));
                }
                "parentNode" | "parentElement" => {
                    let parent = dom.parent(node).filter(|parent| {
                        property == "parentNode"
                            || matches!(
                                dom.node(*parent).map(render_dom::Node::kind),
                                Some(NodeKind::Element(_))
                            )
                    });
                    return match parent {
                        Some(parent) => self.wrap_node(parent),
                        None => Ok(JsValue::Null),
                    };
                }
                "firstChild" | "lastChild" | "nextSibling" | "previousSibling" => {
                    let related = match property {
                        "firstChild" => dom
                            .children(node)
                            .and_then(|children| children.first())
                            .copied(),
                        "lastChild" => dom
                            .children(node)
                            .and_then(|children| children.last())
                            .copied(),
                        "nextSibling" => dom.next_sibling(node),
                        _ => dom.previous_sibling(node),
                    };
                    return match related {
                        Some(related) => self.wrap_node(related),
                        None => Ok(JsValue::Null),
                    };
                }
                "ownerDocument" => return Ok(JsValue::Object(self.realm.document_object())),
                "children" | "childNodes" => {
                    let elements_only = property == "children";
                    let values = dom
                        .children(node)
                        .unwrap_or_default()
                        .iter()
                        .copied()
                        .filter(|child| {
                            !elements_only
                                || matches!(
                                    dom.node(*child).map(render_dom::Node::kind),
                                    Some(NodeKind::Element(_))
                                )
                        })
                        .map(|child| self.wrap_node(child))
                        .collect::<Result<Vec<_>, _>>()?;
                    return Ok(JsValue::Object(self.create_array_from_values(&values)?));
                }
                property if property.starts_with("on") && property.len() > 2 => {
                    let event_type = property[2..].to_ascii_lowercase();
                    return Ok(self
                        .event_handlers
                        .get(&node)
                        .and_then(|handlers| handlers.get(&event_type))
                        .copied()
                        .map_or(JsValue::Null, JsValue::Object));
                }
                property if node_attribute_property(property).is_some() => {
                    let attribute = node_attribute_property(property).expect("checked above");
                    if node_boolean_property(property) {
                        return Ok(JsValue::Boolean(dom.attribute(node, attribute)?.is_some()));
                    }
                    return Ok(JsValue::String(
                        dom.attribute(node, attribute)?
                            .unwrap_or_default()
                            .to_owned(),
                    ));
                }
                _ => {}
            },
            Some(ObjectHost::NamedNodeMap(node)) => {
                let attributes = match dom.node(node).map(render_dom::Node::kind) {
                    Some(NodeKind::Element(element)) => &element.attributes,
                    _ => &Vec::new(),
                };
                if property == "length" {
                    #[allow(
                        clippy::cast_precision_loss,
                        reason = "attribute counts stay far below any precision boundary"
                    )]
                    return Ok(JsValue::Number(attributes.len() as f64));
                }
                if let Ok(index) = property.parse::<usize>() {
                    return match attributes.get(index) {
                        Some(attribute) => {
                            self.ensure_heap_capacity(1)?;
                            Ok(JsValue::Object(self.realm.attr_wrapper(
                                node,
                                attribute.local_name.clone(),
                            )))
                        }
                        None => Ok(JsValue::Undefined),
                    };
                }
                // Named access: the attribute node, or `null` per spec.
                if let Some(attribute) = attributes
                    .iter()
                    .find(|candidate| candidate.local_name == property)
                {
                    self.ensure_heap_capacity(1)?;
                    let name = attribute.local_name.clone();
                    return Ok(JsValue::Object(self.realm.attr_wrapper(node, name)));
                }
                return Ok(JsValue::Null);
            }
            Some(ObjectHost::Attr { owner, name }) => {
                let value = match dom.attribute(owner, &name.clone())? {
                    Some(value) => value.to_owned(),
                    None => String::new(),
                };
                match property {
                    "name" | "nodeName" => return Ok(JsValue::String(name.clone())),
                    "value" | "nodeValue" | "textContent" => {
                        return Ok(JsValue::String(value));
                    }
                    "specified" => return Ok(JsValue::Boolean(true)),
                    _ => {}
                }
            }
            Some(ObjectHost::ClassList(node)) => match property {
                "length" => {
                    #[allow(
                        clippy::cast_precision_loss,
                        reason = "class-list sizes are far below any precision boundary"
                    )]
                    let length = Self::class_list_tokens(dom, node)?.len() as f64;
                    return Ok(JsValue::Number(length));
                }
                "value" => {
                    return Ok(JsValue::String(
                        Self::class_list_tokens(dom, node)?.join(" "),
                    ));
                }
                _ => {}
            },
            Some(ObjectHost::DataSet(node)) => {
                // DOMStringMap named-property visibility: only members whose
                // mapped `data-*` attribute exists shadow the ordinary
                // prototype-chain lookup; everything else falls through.
                if let Some(value) = JsRuntime::dataset_member_attribute_value(dom, node, property)? {
                    return Ok(JsValue::String(value));
                }
            }
            Some(ObjectHost::CssStyleDeclaration(node)) => {
                if property == "cssText" {
                    let declarations = Self::inline_declarations(dom, node);
                    let text = declarations
                        .iter()
                        .map(|(name, value, important)| {
                            if *important {
                                format!("{name}: {value} !important;")
                            } else {
                                format!("{name}: {value};")
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    return Ok(JsValue::String(text));
                }
                if property == "length" || property.parse::<usize>().is_ok() {
                    let declarations = Self::inline_declarations(dom, node);
                    if property == "length" {
                        #[allow(
                            clippy::cast_precision_loss,
                            reason = "declaration counts are far below any precision boundary"
                        )]
                        let length = declarations.len() as f64;
                        return Ok(JsValue::Number(length));
                    }
                    if let Ok(index) = property.parse::<usize>() {
                        return Ok(match declarations.get(index) {
                            Some((name, _, _)) => JsValue::String(name.clone()),
                            None => JsValue::Undefined,
                        });
                    }
                }
                // camelCase member access maps to kebab-case properties, but
                // the interface's methods always take precedence.
                if !STYLE_METHOD_PROPERTIES.contains(&property) {
                    let css_name = css_prop_from_member(property);
                    if is_valid_property_name(&css_name) {
                        return Ok(JsValue::String(
                            Self::inline_declarations(dom, node)
                                .into_iter()
                                .find(|(name, _, _)| *name == css_name)
                                .map_or_else(String::new, |(_, value, _)| value),
                        ));
                    }
                }
            }
            Some(ObjectHost::RegExp(index))
                // `lastIndex` lives in the record so exec/test updates stay
                // visible even when the cached property lags behind.
                if property == "lastIndex" => {
                    #[allow(
                        clippy::cast_precision_loss,
                        reason = "string lengths stay far below any precision boundary"
                    )]
                    let last_index = self.regexes[index].last_index as f64;
                    return Ok(JsValue::Number(last_index));
                }
            Some(ObjectHost::Collection { kind, entries })
                if property == "size" && !kind.is_weak() =>
            {
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "collection sizes are bounded by the JavaScript heap limit"
                )]
                return Ok(JsValue::Number(entries.len() as f64));
            }
            Some(ObjectHost::TypedArray {
                kind,
                buffer,
                start,
                length,
            }) => {
                if property == "BYTES_PER_ELEMENT" {
                    #[allow(
                        clippy::cast_precision_loss,
                        reason = "element sizes are tiny integers"
                    )]
                    return Ok(JsValue::Number(kind.element_size() as f64));
                }
                if let Ok(index) = property.parse::<usize>() {
                    if index >= length {
                        return Ok(JsValue::Undefined);
                    }
                    let element = buffer.0.borrow().get(start + index).copied();
                    return Ok(element.map_or(JsValue::Undefined, JsValue::Number));
                }
                // Method access falls through to the inherited-prototype path.
            }
            Some(ObjectHost::StringPrimitive(text)) => {
                let characters: Vec<char> = text.chars().collect();
                match property {
                    "length" => {
                        #[allow(
                            clippy::cast_precision_loss,
                            reason = "string lengths stay far below any precision boundary"
                        )]
                        let length = characters.len() as f64;
                        return Ok(JsValue::Number(length));
                    }
                    _ => {
                        if let Ok(index) = property.parse::<usize>() {
                            return Ok(characters
                                .get(index)
                                .map_or(JsValue::Undefined, |character| {
                                    JsValue::String(character.to_string())
                                }));
                        }
                    }
                }
                // Method access falls through to the table below and binds
                // to this wrapper instance.
            }
            _ => {}
        }
        // Precedence for every host: a property found on a real interface
        // prototype (anything above `Object.prototype`) wins, so pages and
        // polyfills can override `Promise.prototype.then`,
        // `Function.prototype.call`, and friends. Only when the chain holds
        // nothing but `Object.prototype`'s generic members do the synthesized
        // host methods apply; the final read goes through [[Get]].
        if inherited_origin
            .as_ref()
            .is_some_and(|(_, holder)| *holder != self.realm.object_prototype_id())
        {
            return self.get_value(dom, object, property);
        }
        let function = match (self.realm.host(object), property) {
            (Some(ObjectHost::Promise(_)), "then") => Some(NativeFunction::PromiseThen),
            (Some(ObjectHost::SymbolConstructor), "for") => Some(NativeFunction::SymbolFor),
            (Some(ObjectHost::SymbolConstructor), "keyFor") => Some(NativeFunction::SymbolKeyFor),
            (Some(ObjectHost::Promise(_)), "catch") => Some(NativeFunction::PromiseCatch),
            (_, "addEventListener") if object == self.realm.global_object() => {
                Some(NativeFunction::WindowAddEventListener)
            }
            (_, "removeEventListener") if object == self.realm.global_object() => {
                Some(NativeFunction::WindowRemoveEventListener)
            }
            (Some(ObjectHost::Document(_)), "getElementById") => {
                Some(NativeFunction::GetElementById)
            }
            (Some(ObjectHost::Document(_) | ObjectHost::Node(_)), "querySelector") => {
                Some(NativeFunction::QuerySelector)
            }
            (Some(ObjectHost::Document(_) | ObjectHost::Node(_)), "querySelectorAll") => {
                Some(NativeFunction::QuerySelectorAll)
            }
            (Some(ObjectHost::Document(_) | ObjectHost::Node(_)), "getElementsByTagName") => {
                Some(NativeFunction::GetElementsByTagName)
            }
            (Some(ObjectHost::Document(_) | ObjectHost::Node(_)), "getElementsByClassName") => {
                Some(NativeFunction::GetElementsByClassName)
            }
            (Some(ObjectHost::Document(_) | ObjectHost::Node(_)), "cloneNode") => {
                Some(NativeFunction::CloneNode)
            }
            (Some(ObjectHost::Document(_)), "createTextNode") => {
                Some(NativeFunction::CreateTextNode)
            }
            (Some(ObjectHost::Document(_)), "createDocumentFragment") => {
                Some(NativeFunction::CreateDocumentFragment)
            }
            (Some(ObjectHost::Node(_)), "compareDocumentPosition") => {
                Some(NativeFunction::CompareDocumentPosition)
            }
            (Some(ObjectHost::NamedNodeMap(_)), "item") => Some(NativeFunction::NamedMapItem),
            (Some(ObjectHost::NamedNodeMap(_)), "getNamedItem") => {
                Some(NativeFunction::NamedMapGetNamedItem)
            }
            (Some(ObjectHost::Attr { .. }), "getName") => Some(NativeFunction::AttrGetName),
            (Some(ObjectHost::Attr { .. }), "getValue") => Some(NativeFunction::AttrGetValue),
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
            (Some(ObjectHost::Node(_)), "getAttribute") => Some(NativeFunction::GetAttribute),
            (Some(ObjectHost::Node(_)), "hasAttribute") => Some(NativeFunction::HasAttribute),
            (Some(ObjectHost::Node(_)), "removeAttribute") => Some(NativeFunction::RemoveAttribute),
            (Some(ObjectHost::Node(_)), "appendChild") => Some(NativeFunction::AppendChild),
            (Some(ObjectHost::Node(_)), "removeChild") => Some(NativeFunction::RemoveChild),
            (Some(ObjectHost::Node(_)), "insertBefore") => Some(NativeFunction::InsertBefore),
            (Some(ObjectHost::Node(_)), "remove") => Some(NativeFunction::RemoveNode),
            (Some(ObjectHost::Node(_)), "contains") => Some(NativeFunction::Contains),
            (Some(ObjectHost::Node(_)), "matches") => Some(NativeFunction::Matches),
            (Some(ObjectHost::Node(_)), "click") => Some(NativeFunction::Click),
            (Some(ObjectHost::Node(_)), "getBoundingClientRect") => {
                Some(NativeFunction::GetBoundingClientRect)
            }
            (Some(ObjectHost::CssStyleDeclaration(_)), "getPropertyValue") => {
                Some(NativeFunction::StyleGetProperty)
            }
            (Some(ObjectHost::CssStyleDeclaration(_)), "setProperty") => {
                Some(NativeFunction::StyleSetProperty)
            }
            (Some(ObjectHost::CssStyleDeclaration(_)), "removeProperty") => {
                Some(NativeFunction::StyleRemoveProperty)
            }
            (Some(ObjectHost::CssStyleDeclaration(_)), "item") => Some(NativeFunction::StyleItem),
            (Some(ObjectHost::ClassList(_)), "add") => Some(NativeFunction::ClassListAdd),
            (Some(ObjectHost::ClassList(_)), "remove") => Some(NativeFunction::ClassListRemove),
            (Some(ObjectHost::ClassList(_)), "toggle") => Some(NativeFunction::ClassListToggle),
            (Some(ObjectHost::ClassList(_)), "contains") => Some(NativeFunction::ClassListContains),
            (Some(ObjectHost::ClassList(_)), "item") => Some(NativeFunction::ClassListItem),
            (Some(ObjectHost::ClassList(_)), "toString") => Some(NativeFunction::ClassListToString),
            (Some(ObjectHost::RegExp(_)), "exec") => Some(NativeFunction::RegExpExec),
            (Some(ObjectHost::RegExp(_)), "test") => Some(NativeFunction::RegExpTest),
            (Some(ObjectHost::RegExp(_)), "toString") => Some(NativeFunction::RegExpToString),
            (Some(ObjectHost::CollectionIterator { .. }), "next") => {
                Some(NativeFunction::CollectionIteratorNext)
            }
            (Some(ObjectHost::StringPrimitive(_)), name) => string_method_native(name),
            (Some(ObjectHost::MutationObserver { .. }), "observe") => {
                Some(NativeFunction::MutationObserve)
            }
            (Some(ObjectHost::MutationObserver { .. }), "disconnect") => {
                Some(NativeFunction::MutationDisconnect)
            }
            (Some(ObjectHost::MutationObserver { .. }), "takeRecords") => {
                Some(NativeFunction::MutationTakeRecords)
            }
            _ => None,
        };
        if let Some(function) = function {
            self.ensure_heap_capacity(1)?;
            return Ok(JsValue::Object(self.realm.bound_function(function, object)));
        }
        // Inherited prototype members come last so host interfaces keep
        // precedence over `Object.prototype` fallbacks. The pure chain walk
        // above cannot invoke accessors, so the final read goes through
        // [[Get]].
        self.get_value(dom, object, property)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "host-object write paths each need their own arm"
    )]
    pub(super) fn set_member(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        property: &str,
        value: JsValue,
    ) -> Result<(), JsError> {
        self.consume_step()?;
        let location_base = match self.realm.host(object) {
            Some(ObjectHost::Location(url)) => Some(url.clone()),
            _ => None,
        };
        // `document.cookie = "name=value"`: store in the JS-visible jar.
        if property == "cookie" && matches!(self.realm.host(object), Some(ObjectHost::Document(_)))
        {
            self.js_cookie_store(&value.to_js_string());
            return Ok(());
        }
        if let Some(base) = location_base {
            if property == "href" {
                let target = value.to_js_string();
                let resolved = base.join(&target).map_err(|error| {
                    JsError::dom(format!("invalid navigation URL {target:?}: {error}"))
                })?;
                self.pending_navigations.push(NavigationRequest {
                    url: resolved.to_string(),
                    replace: false,
                });
                return Ok(());
            }
            return Err(JsError::type_error(format!(
                "Location property {property:?} is read-only; navigation is owned by the embedding browser"
            )));
        }
        if let (Some(ObjectHost::Node(node)), "textContent") = (self.realm.host(object), property) {
            return self.set_text_content(dom, node, value.to_js_string());
        }
        if let (Some(ObjectHost::Node(node)), "innerHTML" | "outerHTML") =
            (self.realm.host(object), property)
        {
            let source = match &value {
                JsValue::Null | JsValue::Undefined => String::new(),
                other => other.to_js_string(),
            };
            if property == "innerHTML" {
                return self.set_inner_html(dom, node, &source);
            }
            return self.set_outer_html(dom, node, &source);
        }
        if let Some(ObjectHost::CssStyleDeclaration(node)) = self.realm.host(object) {
            if property == "cssText" {
                let source = value.to_js_string();
                let declarations = parse_declaration_list(&source)
                    .0
                    .into_iter()
                    .map(|declaration| {
                        (
                            declaration.name.to_ascii_lowercase(),
                            declaration.value,
                            declaration.important,
                        )
                    })
                    .collect::<Vec<_>>();
                Self::write_inline_declarations(dom, node, &declarations)?;
                return Ok(());
            }
            // camelCase member assignment maps to kebab-case properties, but
            // the interface's methods always take precedence.
            if !STYLE_METHOD_PROPERTIES.contains(&property) {
                let css_name = css_prop_from_member(property);
                if is_valid_property_name(&css_name) {
                    let mut declarations: Vec<(String, String, bool)> =
                        Self::inline_declarations(dom, node)
                            .into_iter()
                            .filter(|(name, _, _)| *name != css_name)
                            .collect();
                    let css_value = value.to_js_string();
                    let css_value = css_value.trim().to_owned();
                    if !css_value.is_empty() {
                        declarations.push((css_name.clone(), css_value, false));
                    }
                    Self::write_inline_declarations(dom, node, &declarations)?;
                    return Ok(());
                }
            }
        }
        match self.realm.host(object) {
            Some(ObjectHost::Array) => {
                if property == "length" {
                    return self.set_array_length_value(object, &value);
                }
                if let Some(index) = array_index(property) {
                    if !self.realm.set_property(object, property.to_owned(), value) {
                        return Err(JsError::type_error(format!(
                            "property {property:?} is not writable"
                        )));
                    }
                    let length = self.array_length(object)?;
                    if index >= length {
                        let next_length = index.checked_add(1).ok_or_else(|| {
                            JsError::type_error("array length exceeds the supported range")
                        })?;
                        self.set_array_length(object, next_length)?;
                    }
                    return Ok(());
                }
            }
            Some(ObjectHost::TypedArray {
                kind,
                buffer,
                start,
                length,
            }) => {
                if let Ok(index) = property.parse::<usize>() {
                    // Out-of-bounds indexed writes are silently ignored and
                    // never create ordinary properties, per the
                    // integer-indexed exotic object contract.
                    if index < length {
                        let encoded = kind.encode(to_number(&value)?);
                        let mut elements = buffer.0.borrow_mut();
                        if let Some(slot) = elements.get_mut(start + index) {
                            *slot = encoded;
                        }
                    }
                    return Ok(());
                }
            }
            Some(ObjectHost::Node(node)) => {
                if property == "className" {
                    return Ok(dom.set_attribute(node, "class", value.to_js_string())?);
                }
                if property.starts_with("on") && property.len() > 2 {
                    let event_type = property[2..].to_ascii_lowercase();
                    match value {
                        JsValue::Null | JsValue::Undefined => {
                            if let Some(handlers) = self.event_handlers.get_mut(&node) {
                                handlers.remove(&event_type);
                            }
                        }
                        value => {
                            let callback = Self::require_callable_object(&value, &self.realm)?;
                            self.event_handlers
                                .entry(node)
                                .or_default()
                                .insert(event_type, callback);
                        }
                    }
                    return Ok(());
                }
                if let Some(attribute) = node_attribute_property(property) {
                    if node_boolean_property(property) && !value.is_truthy() {
                        return Ok(dom.remove_attribute(node, attribute)?);
                    }
                    return Ok(dom.set_attribute(node, attribute, value.to_js_string())?);
                }
            }
            Some(ObjectHost::ClassList(node)) if property == "value" => {
                return Ok(dom.set_attribute(node, "class", value.to_js_string())?);
            }
            Some(ObjectHost::DataSet(node)) => {
                // DOMStringMap writes go straight to the element's `data-*`
                // attributes (stringified, like the IDL DOMString setter).
                return JsRuntime::set_dataset_member(dom, node, property, &value);
            }
            // Assignments to primitive string wrappers are silently ignored,
            // mirroring how non-strict engines drop them.
            Some(ObjectHost::StringPrimitive(_)) => return Ok(()),
            Some(ObjectHost::RegExp(index)) if property == "lastIndex" => {
                let number = to_number(&value)?;
                #[allow(
                    clippy::cast_sign_loss,
                    clippy::cast_possible_truncation,
                    reason = "negative and fractional indices floor toward zero"
                )]
                let last_index = number.floor().max(0.0) as usize;
                self.regexes[index].last_index = last_index;
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "string lengths stay far below any precision boundary"
                )]
                let stored = last_index as f64;
                self.realm
                    .set_property(object, "lastIndex".to_owned(), JsValue::Number(stored));
                return Ok(());
            }
            _ => {}
        }
        self.set_value(dom, object, property, value)
    }

    pub(super) fn construct(
        &mut self,
        dom: &mut Dom,
        constructor: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        // Constructors recurse through user code just like calls; keep the
        // same depth accounting so runaway `new` chains stay bounded.
        self.consume_step()?;
        if self.calls_active >= self.limits.max_call_depth {
            return Err(self.call_depth_exceeded());
        }
        self.calls_active = self.calls_active.saturating_add(1);
        let result = self.construct_dispatch(dom, constructor, arguments);
        self.calls_active = self.calls_active.saturating_sub(1);
        result
    }

    pub(super) fn construct_dispatch(
        &mut self,
        dom: &mut Dom,
        constructor: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match self.realm.host(constructor) {
            Some(ObjectHost::ObjectConstructor) => self.object_constructor(arguments),
            Some(ObjectHost::ArrayConstructor) => self.array_constructor(arguments),
            Some(ObjectHost::NumberConstructor) => Ok(JsValue::Number(match arguments.first() {
                None | Some(JsValue::Undefined) => 0.0,
                Some(value) => to_number(value)?,
            })),
            Some(ObjectHost::BooleanConstructor) => Ok(JsValue::Boolean(
                arguments.first().is_none_or(JsValue::is_truthy),
            )),
            Some(ObjectHost::DateConstructor) => {
                let ms = Self::date_from_constructor_arguments(arguments)?;
                self.ensure_heap_capacity(1)?;
                Ok(JsValue::Object(self.realm.date_wrapper(ms)))
            }
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
            Some(ObjectHost::DomConstructor) => Err(JsError::type_error("Illegal constructor")),
            Some(ObjectHost::ImageConstructor) => self.image_constructor(dom, arguments),
            Some(ObjectHost::VideoConstructor) => self.video_constructor(constructor, arguments),
            Some(ObjectHost::XmlHttpRequestConstructor) => {
                self.xml_http_request_constructor(constructor)
            }
            Some(ObjectHost::ResponseConstructor) => {
                self.response_constructor(constructor, arguments)
            }
            Some(ObjectHost::IntersectionObserverConstructor) => {
                self.intersection_observer_constructor(constructor, arguments)
            }
            Some(ObjectHost::MutationObserverConstructor) => {
                self.mutation_observer_constructor(constructor, arguments)
            }
            Some(ObjectHost::CollectionConstructor(kind)) => {
                self.collection_constructor(dom, constructor, kind, arguments)
            }
            Some(ObjectHost::TypedArrayConstructor(kind)) => {
                self.typed_array_constructor(dom, constructor, kind, arguments)
            }
            Some(ObjectHost::RegExpConstructor) => {
                let pattern = required_argument(arguments, 0, "RegExp")?.to_js_string();
                let flags = match arguments.get(1) {
                    None | Some(JsValue::Undefined) => String::new(),
                    Some(value) => value.to_js_string(),
                };
                let object = self.construct_regex(&pattern, &flags)?;
                Ok(JsValue::Object(object))
            }
            Some(ObjectHost::UrlConstructor) => self.url_constructor(constructor, arguments),
            Some(ObjectHost::UrlSearchParamsConstructor) => {
                self.url_search_params_constructor(constructor, arguments)
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

    pub(super) fn call(
        &mut self,
        dom: &mut Dom,
        callee: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        self.call_with_this(dom, callee, arguments, JsValue::Undefined)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "call dispatch covers all callable host variants"
    )]
    pub(super) fn call_with_this(
        &mut self,
        dom: &mut Dom,
        callee: ObjectId,
        arguments: &[JsValue],
        receiver: JsValue,
    ) -> Result<JsValue, JsError> {
        self.consume_step()?;
        if self.calls_active >= self.limits.max_call_depth {
            return Err(self.call_depth_exceeded());
        }
        self.calls_active = self.calls_active.saturating_add(1);
        let result = match self.realm.host(callee) {
            Some(ObjectHost::ObjectConstructor) => self.object_constructor(arguments),
            Some(ObjectHost::ArrayConstructor) => self.array_constructor(arguments),
            Some(ObjectHost::FunctionConstructor) => Ok(JsValue::Undefined),
            Some(ObjectHost::StringConstructor) => Ok(JsValue::String(match arguments.first() {
                None => String::new(),
                Some(value) => self.to_string_value(dom, value)?,
            })),
            Some(ObjectHost::NumberConstructor) => Ok(JsValue::Number(match arguments.first() {
                None | Some(JsValue::Undefined) => 0.0,
                Some(value) => to_number(value)?,
            })),
            Some(ObjectHost::BooleanConstructor) => Ok(JsValue::Boolean(
                arguments.first().is_none_or(JsValue::is_truthy),
            )),
            // `Date()` called as a function yields the current time string.
            Some(ObjectHost::DateConstructor) => {
                Ok(JsValue::String(Self::format_date_utc(Self::now_ms())))
            }
            Some(ObjectHost::SymbolInstance(_)) => {
                Err(JsError::type_error("Symbol wrapper is not callable"))
            }
            // `Symbol(desc)` creates a unique primitive symbol.
            Some(ObjectHost::SymbolConstructor) => Ok(JsValue::Symbol(self.create_symbol(
                arguments.first().and_then(|value| {
                    if matches!(value, JsValue::Undefined) {
                        None
                    } else {
                        Some(value.to_js_string())
                    }
                }),
            ))),
            Some(ObjectHost::EventConstructor) => {
                Err(JsError::type_error("Event constructor requires 'new'"))
            }
            Some(ObjectHost::DomConstructor) => Err(JsError::type_error("Illegal constructor")),
            Some(ObjectHost::ImageConstructor) => self.image_constructor(dom, arguments),
            // Legacy web compatibility: `Video()` without `new` constructs,
            // exactly like `Image()`.
            Some(ObjectHost::VideoConstructor) => self.video_constructor(callee, arguments),
            Some(ObjectHost::XmlHttpRequestConstructor) => Err(JsError::type_error(
                "XMLHttpRequest constructor requires 'new'",
            )),
            Some(ObjectHost::ResponseConstructor) => {
                Err(JsError::type_error("Response constructor requires 'new'"))
            }
            Some(ObjectHost::IntersectionObserverConstructor) => Err(JsError::type_error(
                "IntersectionObserver constructor requires 'new'",
            )),
            Some(ObjectHost::MutationObserverConstructor) => Err(JsError::type_error(
                "MutationObserver constructor requires 'new'",
            )),
            Some(ObjectHost::CollectionConstructor(_)) => {
                Err(JsError::type_error("collection constructors require 'new'"))
            }
            // Typed-array constructors also work when called without `new`.
            Some(ObjectHost::TypedArrayConstructor(kind)) => {
                self.typed_array_constructor(dom, callee, kind, arguments)
            }
            Some(ObjectHost::RegExpConstructor) => {
                // `RegExp(re)` called without `new` behaves like construction.
                let pattern = required_argument(arguments, 0, "RegExp")?.to_js_string();
                let flags = match arguments.get(1) {
                    None | Some(JsValue::Undefined) => String::new(),
                    Some(value) => value.to_js_string(),
                };
                let object = self.construct_regex(&pattern, &flags)?;
                Ok(JsValue::Object(object))
            }
            Some(ObjectHost::UrlConstructor) => self.url_constructor(callee, arguments),
            Some(ObjectHost::UrlSearchParamsConstructor) => {
                self.url_search_params_constructor(callee, arguments)
            }
            Some(ObjectHost::ErrorConstructor(kind)) => {
                self.error_constructor(callee, kind, arguments)
            }
            Some(ObjectHost::NativeFunction(NativeFunction::ObjectPrototypeToString)) => {
                // `toString.call(primitive)` never materializes an object.
                Ok(JsValue::String(self.object_to_string_tag(&receiver)))
            }
            Some(ObjectHost::NativeFunction(NativeFunction::ObjectPrototypeValueOf)) => {
                Ok(receiver.clone())
            }
            Some(ObjectHost::NativeFunction(NativeFunction::FunctionPrototype)) => {
                Ok(JsValue::Undefined)
            }
            Some(ObjectHost::NativeFunction(NativeFunction::FunctionCall)) => {
                self.function_call(dom, &receiver, arguments)
            }
            Some(ObjectHost::NativeFunction(NativeFunction::FunctionApply)) => {
                // apply(thisArg, [args...])
                let callable = Self::require_callable_object(&receiver, &self.realm)?;
                let this_argument = arguments.first().cloned().unwrap_or(JsValue::Undefined);
                let call_arguments = match arguments.get(1) {
                    Some(JsValue::Object(array)) => self.array_elements_for(*array),
                    _ => Vec::new(),
                };
                self.call_with_this(dom, callable, &call_arguments, this_argument)
            }
            Some(ObjectHost::NativeFunction(NativeFunction::FunctionBind)) => {
                self.function_bind(&receiver, arguments)
            }
            Some(ObjectHost::NativeFunction(NativeFunction::UrlToString)) => {
                Ok(self.url_to_string(&receiver))
            }
            Some(ObjectHost::NativeFunction(
                function @ (NativeFunction::UrlSearchParamsGet
                | NativeFunction::UrlSearchParamsHas
                | NativeFunction::UrlSearchParamsSet
                | NativeFunction::UrlSearchParamsAppend
                | NativeFunction::UrlSearchParamsToString
                | NativeFunction::UrlSearchParamsForEach),
            )) => Ok(self.url_search_params_method(&receiver, function, arguments, dom)),
            Some(ObjectHost::NativeFunction(function)) => {
                let receiver = match &receiver {
                    JsValue::Object(object) => *object,
                    JsValue::Null | JsValue::Undefined => self.realm.global_object(),
                    _ => self.coerce_member_base(&receiver, &format!("{function:?} receiver"))?,
                };
                self.call_native(dom, function, receiver, arguments)
            }
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
            _ => Err(JsError::type_error(format!(
                "value is not callable (callee host {:?})",
                self.realm.host(callee)
            ))),
        };
        self.calls_active = self.calls_active.saturating_sub(1);
        result
    }

    pub(super) fn function_call(
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

    pub(super) fn function_bind(
        &mut self,
        receiver: &JsValue,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let target = Self::require_callable_object(receiver, &self.realm)?;
        self.ensure_heap_capacity(1)?;
        let bound_receiver = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let bound_arguments = arguments.get(1..).unwrap_or_default().to_vec();
        let bound = self
            .realm
            .bound_callable(target, bound_receiver, bound_arguments.clone());
        // Spec `Function.prototype.bind` metadata: `name` becomes
        // `"bound " + target.name` and `length` shrinks by the number of
        // prepended arguments, never below zero.
        let target_name = self
            .realm
            .get_property(target, "name")
            .map(|value| value.to_js_string())
            .unwrap_or_default();
        let target_length = match self.realm.get_property(target, "length") {
            Some(JsValue::Number(number)) => number.floor().max(0.0) as usize,
            _ => 0,
        };
        let length = target_length.saturating_sub(bound_arguments.len());
        self.realm
            .install_function_metadata(bound, &format!("bound {target_name}"), length);
        Ok(JsValue::Object(bound))
    }

    pub(super) fn call_user(
        &mut self,
        dom: &mut Dom,
        index: usize,
        arguments: &[JsValue],
        receiver: JsValue,
        create_arguments_binding: bool,
    ) -> Result<JsValue, JsError> {
        // All user functions are treated as sloppy mode: a nullish `this`
        // falls back to the global object.
        let receiver = match receiver {
            JsValue::Undefined | JsValue::Null => JsValue::Object(self.realm.global_object()),
            other => other,
        };
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
        let label = function
            .name
            .clone()
            .unwrap_or_else(|| format!("<anonymous fn #{index}>"));
        self.call_stack.push(CallFrame { name: label });
        let result = self
            .instantiate_statements(&function.body)
            .and_then(|()| self.evaluate_statements(dom, &function.body));
        self.call_stack.pop();
        self.this_stack.pop();
        self.environment = previous_environment;
        match result? {
            Completion::Normal(_) => Ok(JsValue::Undefined),
            Completion::Return(value) => Ok(value),
            Completion::Break(_) | Completion::Continue(_) => Err(JsError::new(
                JsErrorKind::Syntax,
                "loop control escaped a function body",
                None,
            )),
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn call_native(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        self.call_stack.push(CallFrame {
            name: format!("{function:?}"),
        });
        let result = self.call_native_dispatch(dom, function, receiver, arguments);
        self.call_stack.pop();
        result
    }

    pub(super) fn optional_callable(
        &self,
        value: Option<&JsValue>,
    ) -> Result<Option<ObjectId>, JsError> {
        let Some(value) = value else {
            return Ok(None);
        };
        if matches!(value, JsValue::Undefined | JsValue::Null) {
            return Ok(None);
        }
        Self::require_callable_object(value, &self.realm).map(Some)
    }

    pub(super) fn require_callable_object(
        value: &JsValue,
        realm: &Realm,
    ) -> Result<ObjectId, JsError> {
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
                    | ObjectHost::NumberConstructor
                    | ObjectHost::BooleanConstructor
                    | ObjectHost::DateConstructor
                    | ObjectHost::SymbolConstructor
                    | ObjectHost::ArrayConstructor
                    | ObjectHost::RegExpConstructor
                    | ObjectHost::EventConstructor
                    | ObjectHost::DomConstructor
                    | ObjectHost::ImageConstructor
                    | ObjectHost::VideoConstructor
                    | ObjectHost::ObjectConstructor
                    | ObjectHost::PromiseConstructor
                    | ObjectHost::MutationObserverConstructor
                    | ObjectHost::UrlConstructor
                    | ObjectHost::UrlSearchParamsConstructor
                    | ObjectHost::XmlHttpRequestConstructor
                    | ObjectHost::ResponseConstructor
                    | ObjectHost::IntersectionObserverConstructor
                    | ObjectHost::CollectionConstructor(_)
                    | ObjectHost::TypedArrayConstructor(_)
                    | ObjectHost::ErrorConstructor(_)
            )
        ) {
            Ok(object)
        } else {
            Err(JsError::type_error(format!(
                "value is not callable (callee host {:?})",
                realm.host(object)
            )))
        }
    }

    pub(super) fn is_callable_object(object: ObjectId, realm: &Realm) -> bool {
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
                    | ObjectHost::NumberConstructor
                    | ObjectHost::BooleanConstructor
                    | ObjectHost::DateConstructor
                    | ObjectHost::SymbolConstructor
                    | ObjectHost::ArrayConstructor
                    | ObjectHost::RegExpConstructor
                    | ObjectHost::EventConstructor
                    | ObjectHost::DomConstructor
                    | ObjectHost::ImageConstructor
                    | ObjectHost::VideoConstructor
                    | ObjectHost::ObjectConstructor
                    | ObjectHost::PromiseConstructor
                    | ObjectHost::MutationObserverConstructor
                    | ObjectHost::UrlConstructor
                    | ObjectHost::UrlSearchParamsConstructor
                    | ObjectHost::XmlHttpRequestConstructor
                    | ObjectHost::ResponseConstructor
                    | ObjectHost::IntersectionObserverConstructor
                    | ObjectHost::CollectionConstructor(_)
                    | ObjectHost::TypedArrayConstructor(_)
                    | ObjectHost::ErrorConstructor(_)
                    | ObjectHost::PromiseSettler { .. }
            )
        )
    }

    /// Accept an object receiver for member access, wrapping primitives that
    /// carry methods (strings) instead of rejecting them outright.
    pub(super) fn coerce_member_base(
        &mut self,
        value: &JsValue,
        context: &str,
    ) -> Result<ObjectId, JsError> {
        match value {
            JsValue::Object(object) => Ok(*object),
            JsValue::Null | JsValue::Undefined => Err(JsError::type_error(format!(
                "Cannot read properties of {} (reading '{}')",
                if matches!(value, JsValue::Null) {
                    "null"
                } else {
                    "undefined"
                },
                context.trim_start_matches('.')
            ))),
            JsValue::String(text) => Ok(self.string_wrapper(text.clone())),
            JsValue::Symbol(symbol) => Ok(self.realm.symbol_instance_wrapper(symbol.clone())),
            JsValue::Number(value) => Ok(self.realm.number_primitive_wrapper(*value)),
            JsValue::Boolean(value) => Ok(self.realm.boolean_primitive_wrapper(*value)),
        }
    }
}

/// Byte offset a parsed expression node carries for diagnostics, when the
/// variant is a positioned struct variant.
pub(super) fn expr_offset(expression: &Expr) -> Option<usize> {
    match expression {
        Expr::RegexLiteral { offset, .. }
        | Expr::Function { offset, .. }
        | Expr::Arrow { offset, .. }
        | Expr::ObjectRest { offset, .. }
        | Expr::Unary { offset, .. }
        | Expr::Binary { offset, .. }
        | Expr::Conditional { offset, .. }
        | Expr::Update { offset, .. }
        | Expr::Member { offset, .. }
        | Expr::ComputedMember { offset, .. }
        | Expr::New { offset, .. }
        | Expr::Call { offset, .. }
        | Expr::Assignment { offset, .. }
        | Expr::CompoundAssignment { offset, .. } => Some(*offset),
        Expr::Literal(_)
        | Expr::This
        | Expr::Identifier(_)
        | Expr::Object(_)
        | Expr::Array(_)
        | Expr::Spread(_)
        | Expr::Sequence(_) => None,
    }
}

/// Byte offset a parsed statement node carries for diagnostics.
pub(super) fn statement_offset(statement: &Statement) -> Option<usize> {
    match statement {
        Statement::Variable { offset, .. }
        | Statement::VariableList { offset, .. }
        | Statement::Function { offset, .. }
        | Statement::Try { offset, .. }
        | Statement::If { offset, .. }
        | Statement::Switch { offset, .. }
        | Statement::While { offset, .. }
        | Statement::DoWhile { offset, .. }
        | Statement::For { offset, .. }
        | Statement::ForIn { offset, .. }
        | Statement::ForOf { offset, .. }
        | Statement::ForInExpr { offset, .. }
        | Statement::Labeled { offset, .. } => Some(*offset),
        Statement::Return(_)
        | Statement::Throw(_)
        | Statement::Break(_)
        | Statement::Continue(_)
        | Statement::Block(_)
        | Statement::Expression(_) => None,
    }
}
