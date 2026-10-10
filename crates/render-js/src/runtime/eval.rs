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

use crate::JsBigInt;
use crate::JsError;
use crate::JsErrorKind;
use crate::JsObject;
use crate::JsSymbol;
use crate::JsValue;
use crate::ObjectId;
use crate::PropertyDescriptor;
use crate::Realm;
use crate::parser::BinaryOp;
use crate::parser::BindingPattern;
use crate::parser::BindingTarget;
use crate::parser::CatchClause;
use crate::parser::Expr;
use crate::parser::FunctionKind;
use crate::parser::ObjectAccessorKind;
use crate::parser::ObjectProperty;
use crate::parser::PARAMETER_DEFAULT_MARKER;
use crate::parser::PARAMETER_REST_MARKER;
use crate::parser::PropertyKey;
use crate::parser::Statement;
use crate::parser::UnaryOp;
use crate::parser::VariableKind;
use crate::parser::collect_var_names;
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
use crate::runtime::convert::relational_compare;
use crate::runtime::convert::shift_left;
use crate::runtime::convert::shift_right;
use crate::runtime::convert::strict_equal;
use crate::runtime::convert::to_int32;
use crate::runtime::convert::to_number;
use crate::runtime::convert::unsigned_shift_right;
use crate::runtime::coroutine_run::Resume;
use crate::runtime::types::Binding;
use crate::runtime::types::CallFrame;
use crate::runtime::types::ClassFrame;
use crate::runtime::types::Environment;
use crate::runtime::types::EnvironmentRecord;
use crate::runtime::types::FunctionFlags;
use crate::runtime::types::GlobalBinding;
use crate::runtime::types::NavigationRequest;
use crate::runtime::types::UserFunction;
use crate::utf16;
use crate::value::ErrorKind;
use crate::value::NativeFunction;
use crate::value::ObjectHost;
use render_css::stylesheet::parse_declaration_list;
use render_dom::Dom;
use render_dom::NodeKind;
use render_html::serialize_html_fragment;
use render_html::serialize_html_node;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::rc::Rc;

/// The prelude global the evaluator calls around native node property writes.
const CUSTOM_ELEMENT_REACTION: &str = "__customElementReaction";

/// Coercion hint passed to `ToPrimitive` (`Symbol.toPrimitive` receives the
/// name, `OrdinaryToPrimitive` uses the method order).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PrimitiveHint {
    Default,
    Number,
    String,
}

impl PrimitiveHint {
    const fn name(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Number => "number",
            Self::String => "string",
        }
    }
}

#[derive(Clone, Debug)]
pub(super) enum Completion {
    Normal(JsValue),
    Return(JsValue),
    Break(Option<String>),
    Continue(Option<String>),
}

#[derive(Clone, Debug)]
pub(super) enum AssignmentReference {
    /// An identifier, resolved when the reference is created (ECMA-262
    /// 13.15.2 evaluates the target before the value), so a write goes to the
    /// record that was found even if the value expression changes the scope.
    /// `scope` is `None` for the global environment.
    Binding {
        name: String,
        scope: Option<usize>,
    },
    Property {
        object: ObjectId,
        property: String,
    },
    SymbolProperty {
        object: ObjectId,
        symbol: JsSymbol,
    },
    Private {
        object: ObjectId,
        name: String,
    },
    SuperProperty {
        property: String,
    },
}

/// One live iterator being consumed by an array binding pattern.
///
/// An array pattern cannot be lowered to indexed member access, because the
/// specification's `IteratorBindingInitialization` pulls values one at a time
/// through `@@iterator` and *stops early*: `[a] = fiveThings` calls `next`
/// once and then closes the iterator. Draining into an array first would call
/// `next` five times, and would never terminate on an infinite generator. So
/// the pattern holds this state instead.
struct ArrayDestructuring {
    iterator: Option<ObjectId>,
    next: Option<ObjectId>,
    done: bool,
    /// Set instead of `iterator` when the elements are already in hand.
    values: Option<std::vec::IntoIter<JsValue>>,
}

impl ArrayDestructuring {
    /// `GetIterator(value, sync)`. A source with no `@@iterator` is a
    /// `TypeError`, not an empty list.
    fn open(runtime: &mut JsRuntime, dom: &mut Dom, value: &JsValue) -> Result<Self, JsError> {
        // A string is walked by code point so an astral character binds as one
        // element. Arrays go through their `@@iterator` like any other source:
        // replacing `Array.prototype[@@iterator]` is observable (ECMA-262 8.6.2).
        if let JsValue::String(text) = value {
            let values = text
                .chars()
                .map(|character| JsValue::String(character.to_string()))
                .collect();
            return Ok(Self::over(values));
        }
        match runtime.get_iterator(dom, value)? {
            Some((iterator, next)) => Ok(Self {
                iterator: Some(iterator),
                next: Some(next),
                done: false,
                values: None,
            }),
            None => Err(JsError::type_error(format!(
                "{} is not iterable",
                describe_source(value)
            ))),
        }
    }

    /// An in-memory value list, used where the engine already knows the
    /// elements without consulting `@@iterator`.
    fn over(values: Vec<JsValue>) -> Self {
        Self {
            iterator: None,
            next: None,
            done: true,
            values: Some(values.into_iter()),
        }
    }

    /// `IteratorStep`, returning `undefined` once the source is exhausted so
    /// a short source binds `undefined` rather than failing.
    fn next(&mut self, runtime: &mut JsRuntime, dom: &mut Dom) -> Result<JsValue, JsError> {
        if let Some(values) = &mut self.values {
            return Ok(values.next().unwrap_or(JsValue::Undefined));
        }
        if self.done {
            return Ok(JsValue::Undefined);
        }
        if let Some(value) = self.iterator_next_value(runtime, dom)? {
            Ok(value)
        } else {
            self.done = true;
            Ok(JsValue::Undefined)
        }
    }

    /// Drain what is left into a real `Array`, which is what a rest element
    /// binds even when the source was a typed array or a `Set`.
    fn rest(&mut self, runtime: &mut JsRuntime, dom: &mut Dom) -> Result<ObjectId, JsError> {
        let mut values = Vec::new();
        if let Some(remaining) = &mut self.values {
            values.extend(remaining.by_ref());
        } else {
            while let Some(value) = self.iterator_next_value(runtime, dom)? {
                values.push(value);
            }
            self.done = true;
        }
        runtime.create_array_from_values(&values)
    }

    /// `IteratorClose`, for a pattern that finished before the source did.
    ///
    /// Never fails: a throwing `return` must not mask the completion that
    /// prompted the close, which is why both this and the call are discarded.
    fn close(&mut self, runtime: &mut JsRuntime, dom: &mut Dom) {
        if self.done {
            return;
        }
        self.done = true;
        let (Some(iterator), Some(_)) = (self.iterator, self.next) else {
            return;
        };
        let return_method = runtime
            .get_member(dom, iterator, "return")
            .ok()
            .filter(|value| {
                matches!(value, JsValue::Object(object)
                    if JsRuntime::is_callable_object(*object, &runtime.realm))
            });
        if let Some(JsValue::Object(return_method)) = return_method {
            let _ = runtime.call_with_this(dom, return_method, &[], JsValue::Object(iterator));
        }
    }

    /// `IteratorClose` for a pattern that finished normally (ECMA-262 7.4.11
    /// with a normal completion). Unlike `close`, a throwing `return` propagates,
    /// and so does a result that is not an object.
    fn close_normal(&mut self, runtime: &mut JsRuntime, dom: &mut Dom) -> Result<(), JsError> {
        if self.done {
            return Ok(());
        }
        self.done = true;
        let (Some(iterator), Some(_)) = (self.iterator, self.next) else {
            return Ok(());
        };
        let Some(return_method) = runtime.get_method(dom, iterator, "return")? else {
            return Ok(());
        };
        let result = runtime.call_with_this(dom, return_method, &[], JsValue::Object(iterator))?;
        if matches!(result, JsValue::Object(_)) {
            Ok(())
        } else {
            Err(JsError::type_error(
                "iterator return() must produce an object",
            ))
        }
    }

    fn iterator_next_value(
        &mut self,
        runtime: &mut JsRuntime,
        dom: &mut Dom,
    ) -> Result<Option<JsValue>, JsError> {
        let (Some(iterator), Some(next)) = (self.iterator, self.next) else {
            return Ok(None);
        };
        // A step that throws leaves the iterator done, so the pattern's
        // IteratorClose must not call `return` on it (ECMA-262 7.4.6 IteratorStep).
        let result = match runtime.iterator_next(dom, iterator, next) {
            Ok(result) => result,
            Err(error) => {
                self.done = true;
                return Err(error);
            }
        };
        if result.is_none() {
            self.done = true;
        }
        Ok(result)
    }
}

/// The plain function a chain of labels names, when a labelled function
/// declaration (Annex B.3.2) is what the statement is. It is declared like an
/// unlabelled function, so hoisting looks through the labels.
fn labelled_function(statement: &Statement) -> Option<&Statement> {
    let mut inner = statement;
    let mut labelled = false;
    while let Statement::Labeled { body, .. } = inner {
        inner = body.as_ref();
        labelled = true;
    }
    (labelled && matches!(inner, Statement::Function { .. })).then_some(inner)
}

/// The name `SetFunctionName` gives a function defined under a property key
/// (ECMA-262 8.4.5): a symbol key contributes `[description]`, or the empty name
/// when the symbol has no description.
fn property_key_function_name(key: &JsValue) -> String {
    match key {
        JsValue::Symbol(symbol) => symbol
            .description()
            .map_or_else(String::new, |description| format!("[{description}]")),
        other => other.to_js_string(),
    }
}

/// ECMA-262 6.1.6.1.3 `Number::exponentiate`. A NaN exponent gives NaN, and so
/// does a base of ±1 raised to ±Infinity; `powf` answers 1 for both.
fn number_exponentiate(base: f64, exponent: f64) -> f64 {
    if exponent.is_nan() || (base.abs() == 1.0 && exponent.is_infinite()) {
        return f64::NAN;
    }
    base.powf(exponent)
}

/// The wording a `TypeError` uses for a non-iterable source. The
/// specification's own text differs per type, and matching the common cases is
/// what a bundle's error handling keys on.
fn describe_source(value: &JsValue) -> String {
    match value {
        JsValue::Null => "null".to_owned(),
        JsValue::Undefined => "undefined".to_owned(),
        JsValue::Object(_) => "object".to_owned(),
        JsValue::Number(number) => format!("number {number}"),
        JsValue::BigInt(value) => format!("bigint {}", value.to_string_radix(10)),
        JsValue::Boolean(value) => format!("boolean {value}"),
        JsValue::Symbol(_) => "symbol".to_owned(),
        JsValue::String(text) => text.clone(),
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
            // Wrappers `ToObject` boxed for a previous statement are unreachable
            // from this one, so drop their GC pins; inside a native dispatch a
            // Rust frame may still hold one, so leave the pins alone there.
            if self.call_stack.is_empty() {
                self.transient_roots.clear();
            }
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
            let statement = labelled_function(statement).unwrap_or(statement);
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
                    for (target, _) in declarations {
                        for name in target.names() {
                            if *kind == VariableKind::Var {
                                var_names.insert(name);
                            } else if lexical_declarations.insert(name.clone(), *kind).is_some() {
                                return Err(JsError::syntax(
                                    format!("binding {name:?} is declared more than once"),
                                    0,
                                ));
                            }
                        }
                    }
                }
                Statement::Function {
                    name,
                    parameters,
                    body,
                    kind,
                    ..
                } => {
                    var_names.insert(name.clone());
                    functions.push((name, parameters, body, *kind));
                }
                Statement::Class { name, .. } => {
                    if lexical_declarations
                        .insert(name.clone(), VariableKind::Const)
                        .is_some()
                    {
                        return Err(JsError::syntax(
                            format!("binding {name:?} is declared more than once"),
                            0,
                        ));
                    }
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
        for (name, parameters, body, kind) in functions {
            let value = self.create_function(Some(name), parameters, body, kind)?;
            self.initialize_declared_binding(name, value, VariableKind::Var)?;
        }
        Ok(())
    }

    pub(super) fn instantiate_block_lexicals<'a>(
        &mut self,
        statements: impl IntoIterator<Item = &'a Statement>,
    ) -> Result<(), JsError> {
        let mut declarations = BTreeMap::new();
        let mut function_names = BTreeSet::new();
        let mut functions = Vec::new();
        for statement in statements {
            let statement = labelled_function(statement).unwrap_or(statement);
            match statement {
                Statement::VariableList {
                    kind,
                    declarations: variables,
                    ..
                } => {
                    for (target, _) in variables {
                        for name in target.names() {
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
                    kind,
                    ..
                } => {
                    // Sloppy code may repeat a block-level function declaration
                    // (Annex B.3.3.4); the last one wins, and the binding is shared.
                    if !function_names.contains(name)
                        && declarations
                            .insert(name.clone(), VariableKind::Const)
                            .is_some()
                    {
                        return Err(JsError::syntax(
                            format!("binding {name:?} is declared more than once"),
                            0,
                        ));
                    }
                    function_names.insert(name.clone());
                    functions.push((name, parameters, body, *kind));
                }
                Statement::Class { name, .. }
                    if declarations
                        .insert(name.clone(), VariableKind::Const)
                        .is_some() =>
                {
                    return Err(JsError::syntax(
                        format!("binding {name:?} is declared more than once"),
                        0,
                    ));
                }
                _ => {}
            }
        }
        for (name, kind) in declarations {
            self.create_binding(&name, kind, false, JsValue::Undefined)?;
        }
        for (name, parameters, body, kind) in functions {
            let value = self.create_function(Some(name), parameters, body, kind)?;
            self.initialize_declared_binding(name, value, VariableKind::Var)?;
        }
        Ok(())
    }

    pub(super) fn create_user_function(
        &mut self,
        parameters: &[String],
        body: &[Statement],
        kind: FunctionKind,
    ) -> Result<JsValue, JsError> {
        self.create_function(None, parameters, body, kind)
    }

    pub(super) fn create_arrow_function(
        &mut self,
        name: Option<&str>,
        parameters: &[String],
        body: &[Statement],
        is_async: bool,
    ) -> Result<JsValue, JsError> {
        // Arrows inherit `super` and `this` lexically from the function they
        // execute inside, so they carry that class's metadata and bind no
        // `this` of their own.
        let class = self
            .class_frames
            .last()
            .and_then(|frame| frame.function.clone());
        self.create_function_meta(
            name,
            parameters,
            body,
            FunctionFlags {
                arrow: true,
                strict: false,
                class,
                kind: FunctionKind::new(is_async, false),
            },
        )
    }

    pub(super) fn create_function(
        &mut self,
        name: Option<&str>,
        parameters: &[String],
        body: &[Statement],
        kind: FunctionKind,
    ) -> Result<JsValue, JsError> {
        self.create_function_meta(
            name,
            parameters,
            body,
            FunctionFlags {
                arrow: false,
                strict: false,
                class: None,
                kind,
            },
        )
    }

    pub(super) fn create_function_meta(
        &mut self,
        name: Option<&str>,
        parameters: &[String],
        body: &[Statement],
        flags: FunctionFlags,
    ) -> Result<JsValue, JsError> {
        let FunctionFlags {
            arrow,
            strict,
            class,
            kind,
        } = flags;
        self.ensure_heap_capacity(if arrow { 1 } else { 2 })?;
        let function_index = self.functions.len();
        let (body, defaults, patterns) = Self::extract_parameter_markers(body);
        let (parameters, length, rest) = Self::binding_parameters(parameters);
        let defaults = (0..parameters.len())
            .map(|index| defaults.get(&index).cloned())
            .collect();
        let patterns = (0..parameters.len())
            .map(|index| patterns.get(&index).cloned())
            .collect();
        self.functions.push(UserFunction {
            name: name.map(str::to_owned),
            parameters,
            defaults,
            patterns,
            body,
            captured_environment: self.environment.clone(),
            arrow,
            strict,
            class,
            rest,
            kind,
        });
        // Spec: the `name` of an anonymous function in progress is the empty
        // string (anonymous arrows included); `length` counts parameters
        // before the first default initializer, excluding the rest parameter.
        let name = name.unwrap_or("");
        let function = if arrow {
            self.realm.arrow_function(function_index, name, length)
        } else {
            self.realm.user_function(function_index, name, length, kind)
        };
        Ok(JsValue::Object(function))
    }

    /// Strip the parser's default/rest parameter markers into plain binding
    /// names and derive the spec `length`: the parameter count before the
    /// first default initializer, with the rest parameter excluded. The
    /// `\0`-prefixed arrow destructuring temporaries pass through untouched.
    fn binding_parameters(parameters: &[String]) -> (Vec<String>, usize, bool) {
        let mut names = Vec::with_capacity(parameters.len());
        let mut length = 0_usize;
        let mut counting = true;
        let mut rest = false;
        for parameter in parameters {
            if let Some(binding) = parameter.strip_prefix(PARAMETER_REST_MARKER) {
                names.push(binding.to_owned());
                counting = false;
                rest = true;
            } else if let Some(binding) = parameter.strip_prefix(PARAMETER_DEFAULT_MARKER) {
                names.push(binding.to_owned());
                counting = false;
            } else {
                names.push(parameter.clone());
                if counting {
                    length += 1;
                }
            }
        }
        (names, length, rest)
    }

    /// Remove the parser's [`Statement::ParameterDefault`] and
    /// [`Statement::ParameterPattern`] markers from a function body and return
    /// them keyed by parameter position. The parser prepends them ahead of the
    /// body; the evaluator never sees them as ordinary statements. A pattern
    /// comes back as the `var` declaration it lowers to.
    fn extract_parameter_markers(
        body: &[Statement],
    ) -> (
        Vec<Statement>,
        BTreeMap<usize, Expr>,
        BTreeMap<usize, Statement>,
    ) {
        let mut defaults = BTreeMap::new();
        let mut patterns = BTreeMap::new();
        let mut statements = Vec::with_capacity(body.len());
        for statement in body {
            match statement {
                Statement::ParameterDefault { index, value, .. } => {
                    defaults.insert(*index, value.clone());
                }
                Statement::ParameterPattern {
                    index,
                    declarations,
                    offset,
                } => {
                    patterns.insert(
                        *index,
                        Statement::VariableList {
                            kind: VariableKind::Var,
                            declarations: declarations.clone(),
                            offset: *offset,
                        },
                    );
                }
                statement => statements.push(statement.clone()),
            }
        }
        (statements, defaults, patterns)
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
                    Some(expression) => self.evaluate_named(dom, expression, name)?,
                    None => JsValue::Undefined,
                };
                self.initialize_binding(dom, name, value.clone(), *kind)?;
                Ok(Completion::Normal(value))
            }
            Statement::VariableList {
                kind, declarations, ..
            } => {
                let mut value = JsValue::Undefined;
                for (target, expression) in declarations {
                    if let Some(expression) = expression {
                        value = match target {
                            BindingTarget::Name(name) => {
                                self.evaluate_named(dom, expression, name)?
                            }
                            BindingTarget::Pattern(_) => self.evaluate(dom, expression)?,
                        };
                        match target {
                            BindingTarget::Name(name) => {
                                self.initialize_binding(dom, name, value.clone(), *kind)?;
                            }
                            BindingTarget::Pattern(pattern) => {
                                self.initialize_binding_pattern(
                                    dom,
                                    pattern,
                                    value.clone(),
                                    *kind,
                                )?;
                            }
                        }
                    } else if *kind == VariableKind::Let
                        && let BindingTarget::Name(name) = target
                    {
                        self.initialize_binding(dom, name, JsValue::Undefined, *kind)?;
                    }
                }
                Ok(Completion::Normal(value))
            }
            Statement::Function { name, .. } => {
                self.lookup_binding(dom, name).map(Completion::Normal)
            }
            Statement::Class {
                name,
                super_class,
                elements,
                ..
            } => {
                let value =
                    self.evaluate_class(dom, Some(name), super_class.as_deref(), elements)?;
                self.initialize_binding(dom, name, value.clone(), VariableKind::Const)?;
                Ok(Completion::Normal(value))
            }
            Statement::Return(value) => {
                let value = match value {
                    Some(expression) => self.evaluate(dom, expression)?,
                    None => JsValue::Undefined,
                };
                Ok(Completion::Return(value))
            }
            Statement::Throw(expression) => {
                let value = self.evaluate(dom, expression)?;
                // A thrown object with a string `message` (an Error subclass
                // or a user exception) reports that message, not
                // "[object Object]", as a browser's uncaught-error line does.
                let object_message = match &value {
                    JsValue::Object(object) => match self.realm.get_property(*object, "message") {
                        Some(JsValue::String(message)) => Some(message),
                        _ => None,
                    },
                    _ => None,
                };
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
                            self.error_to_string(dom, object).to_js_string(),
                        )
                    }
                    value => match object_message {
                        Some(message) => JsError::thrown_with_message(value, message),
                        None => JsError::thrown(value),
                    },
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
                let own_labels = std::mem::take(&mut self.pending_loop_labels);
                let mut value = JsValue::Undefined;
                loop {
                    self.consume_step()?;
                    if !self.evaluate(dom, condition)?.is_truthy() {
                        break;
                    }
                    match own_loop_completion(&own_labels, self.evaluate_statement(dom, body)?) {
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
                let own_labels = std::mem::take(&mut self.pending_loop_labels);
                let mut value = JsValue::Undefined;
                loop {
                    self.consume_step()?;
                    match own_loop_completion(&own_labels, self.evaluate_statement(dom, body)?) {
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
            // `for await` suspends, so it runs in a coroutine. Only module
            // bodies reach this evaluator, and they have no coroutine to run in.
            Statement::ForOf {
                is_await: true,
                offset,
                ..
            } => Err(JsError::syntax(
                "`for await` outside an async function is not supported yet",
                *offset,
            )),
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
            Statement::Labeled { label, body, .. } => {
                // A label in front of a loop (or of another label) belongs to
                // that loop, which needs it to recognise `continue label`.
                if matches!(
                    body.as_ref(),
                    Statement::While { .. }
                        | Statement::DoWhile { .. }
                        | Statement::For { .. }
                        | Statement::ForIn { .. }
                        | Statement::ForOf { .. }
                        | Statement::ForInExpr { .. }
                        | Statement::Labeled { .. }
                ) {
                    self.pending_loop_labels.push(label.clone());
                } else {
                    self.pending_loop_labels.clear();
                }
                match self.evaluate_statement(dom, body)? {
                    Completion::Break(Some(target)) | Completion::Continue(Some(target))
                        if *target == *label =>
                    {
                        Ok(Completion::Normal(JsValue::Undefined))
                    }
                    other => Ok(other),
                }
            }
            Statement::Break(label) => Ok(Completion::Break(label.clone())),
            Statement::Continue(label) => Ok(Completion::Continue(label.clone())),
            Statement::Block(statements) => self.evaluate_scoped_statements(dom, statements),
            Statement::With { object, body, .. } => self.evaluate_with_statement(dom, object, body),
            Statement::Expression(expression) => {
                self.evaluate(dom, expression).map(Completion::Normal)
            }
            // Parameter defaults are consumed while binding a call's
            // parameters; reaching one here means it was not extracted.
            Statement::ParameterDefault { .. } | Statement::ParameterPattern { .. } => {
                Ok(Completion::Normal(JsValue::Undefined))
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
        // The case block is one lexical scope shared by every clause (ECMA-262
        // 14.12.4): its declarations are instantiated before any clause runs.
        self.environment
            .push(Rc::new(RefCell::new(EnvironmentRecord::default())));
        let result = self
            .instantiate_block_lexicals(cases.iter().flat_map(|(_, statements)| statements))
            .and_then(|()| self.run_case_clauses(dom, &discriminant, cases));
        self.environment.pop();
        result
    }

    /// `CaseBlockEvaluation` (ECMA-262 14.12.4): the tests of the non-default
    /// clauses run in source order, and the first match starts execution there.
    /// With no match, execution starts at the default clause, if any, and falls
    /// through to the clauses after it.
    fn run_case_clauses(
        &mut self,
        dom: &mut Dom,
        discriminant: &JsValue,
        cases: &[(Vec<Expr>, Vec<Statement>)],
    ) -> Result<Completion, JsError> {
        let mut start = None;
        'search: for (index, (tests, _)) in cases.iter().enumerate() {
            for test in tests {
                if strict_equal(discriminant, &self.evaluate(dom, test)?) {
                    start = Some(index);
                    break 'search;
                }
            }
        }
        let start = start.or_else(|| cases.iter().position(|(tests, _)| tests.is_empty()));
        let Some(start) = start else {
            return Ok(Completion::Normal(JsValue::Undefined));
        };
        let mut value = JsValue::Undefined;
        for (_, statements) in &cases[start..] {
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
        let own_labels = std::mem::take(&mut self.pending_loop_labels);
        // `for (let …;;)` gives every iteration its own copy of the loop
        // variables (ECMA-262 §14.7.4.4 CreatePerIterationEnvironment), which
        // is what lets closures made in an iteration keep that iteration's value.
        let per_iteration_bindings = matches!(
            initializer,
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
        self.environment
            .push(Rc::new(RefCell::new(EnvironmentRecord::default())));
        let result = (|| {
            if let Some(initializer) = initializer {
                // Lexical loop variables live in the loop's own scope. `var`
                // ones were hoisted to the function (or the global scope)
                // before the statement ran, so there is nothing to create.
                match initializer {
                    Statement::Variable { kind, name, .. } if *kind != VariableKind::Var => {
                        self.create_binding(name, *kind, false, JsValue::Undefined)?;
                    }
                    Statement::VariableList {
                        kind, declarations, ..
                    } if *kind != VariableKind::Var => {
                        for (target, _) in declarations {
                            for name in target.names() {
                                self.create_binding(&name, *kind, false, JsValue::Undefined)?;
                            }
                        }
                    }
                    _ => {}
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
                match own_loop_completion(&own_labels, self.evaluate_statement(dom, body)?) {
                    Completion::Normal(next) => value = next,
                    Completion::Continue(None) => {}
                    Completion::Break(None) => break,
                    returned @ Completion::Return(_) => return Ok(returned),
                    labeled @ (Completion::Break(Some(_)) | Completion::Continue(Some(_))) => {
                        return Ok(labeled);
                    }
                }
                if per_iteration_bindings && let Some(top) = self.environment.last() {
                    let copy = EnvironmentRecord {
                        bindings: top.borrow().bindings.clone(),
                        ..EnvironmentRecord::default()
                    };
                    let last = self.environment.len() - 1;
                    self.environment[last] = Rc::new(RefCell::new(copy));
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
        let own_labels = std::mem::take(&mut self.pending_loop_labels);
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
                self.assign_binding(dom, name, JsValue::String(property))?;
            }
            let completion = self.evaluate_statement(dom, body);
            if iteration_environment.is_some() {
                self.environment.pop();
            }
            match own_loop_completion(&own_labels, completion?) {
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
        let own_labels = std::mem::take(&mut self.pending_loop_labels);
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
            match own_loop_completion(&own_labels, self.evaluate_statement(dom, body)?) {
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
        let own_labels = std::mem::take(&mut self.pending_loop_labels);
        let iterable = self.evaluate(dom, iterable)?;
        // Arrays and strings are walked directly. Any other iterable goes
        // through the iterator protocol one step at a time, so an infinite
        // generator works and leaving the loop early closes the iterator
        // (ECMA-262 §7.4.12 `IteratorClose`).
        let mut eager = None;
        let mut lazy = None;
        match &iterable {
            JsValue::Object(object)
                if !matches!(self.realm.host(*object), Some(ObjectHost::Array)) =>
            {
                match self.get_iterator(dom, &iterable)? {
                    Some(pair) => lazy = Some(pair),
                    None => eager = Some(self.iterate_values(dom, &iterable)?.into_iter()),
                }
            }
            _ => eager = Some(self.iterate_values(dom, &iterable)?.into_iter()),
        }
        if kind == VariableKind::Var {
            self.create_binding(name, kind, true, JsValue::Undefined)?;
        }
        let mut value = JsValue::Undefined;
        loop {
            self.consume_step()?;
            let item = if let Some(items) = eager.as_mut() {
                match items.next() {
                    Some(item) => item,
                    None => break,
                }
            } else if let Some((iterator, next)) = lazy {
                match self.iterator_next(dom, iterator, next)? {
                    Some(item) => item,
                    None => break,
                }
            } else {
                break;
            };
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
            let completion = if kind == VariableKind::Var {
                self.assign_binding(dom, name, item)
                    .and_then(|()| self.evaluate_statement(dom, body))
            } else {
                self.evaluate_statement(dom, body)
            };
            if iteration_environment.is_some() {
                self.environment.pop();
            }
            let completion =
                completion.map(|completion| own_loop_completion(&own_labels, completion));
            match completion {
                Err(error) => {
                    // The original error wins over anything `return()` throws.
                    if let Some((iterator, _)) = lazy {
                        let _ = self.close_iterator_object(dom, iterator);
                    }
                    return Err(error);
                }
                Ok(Completion::Normal(next)) => value = next,
                Ok(Completion::Continue(None)) => {}
                Ok(abrupt) => {
                    if let Some((iterator, _)) = lazy {
                        self.close_iterator_object(dom, iterator)?;
                    }
                    match abrupt {
                        Completion::Break(None) => break,
                        other => return Ok(other),
                    }
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
            let value = self.error_to_thrown_value(error)?;
            let catch_environment = Rc::new(RefCell::new(EnvironmentRecord::default()));
            if let Some(parameter) = &catch.parameter {
                // The parameter's names exist, uninitialized, while a pattern's
                // defaults run, so a default that reads a later name is a
                // ReferenceError rather than a leak from an outer scope.
                let mut scope = catch_environment.borrow_mut();
                for name in parameter.names() {
                    scope.bindings.insert(
                        name,
                        Binding {
                            value: JsValue::Undefined,
                            mutable: true,
                            initialized: false,
                            kind: VariableKind::Let,
                        },
                    );
                }
            }
            self.environment.push(catch_environment);
            result = self
                .bind_catch_parameter(dom, catch.parameter.as_ref(), value)
                .and_then(|()| self.instantiate_block_lexicals(&catch.body))
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

    /// Initializes a `catch` clause's parameter from the thrown value. A pattern
    /// destructures it, so a `null` or `undefined` value throws, as it does for a
    /// `let` declaration.
    pub(super) fn bind_catch_parameter(
        &mut self,
        dom: &mut Dom,
        parameter: Option<&BindingTarget>,
        value: JsValue,
    ) -> Result<(), JsError> {
        match parameter {
            None => Ok(()),
            Some(BindingTarget::Name(name)) => {
                self.initialize_binding(dom, name, value, VariableKind::Let)
            }
            Some(BindingTarget::Pattern(pattern)) => {
                self.initialize_binding_pattern(dom, pattern, value, VariableKind::Let)
            }
        }
    }

    /// The JavaScript value a `catch` clause (or a rejected promise) sees for
    /// `error`: the thrown value itself, or a standard error instance.
    pub(super) fn error_to_thrown_value(&mut self, error: &JsError) -> Result<JsValue, JsError> {
        if let Some(value) = error.thrown_value().cloned() {
            return Ok(value);
        }
        let kind = match error.kind() {
            JsErrorKind::Syntax => ErrorKind::SyntaxError,
            JsErrorKind::Reference => ErrorKind::ReferenceError,
            JsErrorKind::Type => ErrorKind::TypeError,
            JsErrorKind::ResourceLimit => ErrorKind::RangeError,
            JsErrorKind::Dom | JsErrorKind::Throw => ErrorKind::Error,
        };
        self.construct_standard_error(kind, error.message())
    }

    /// A promise already rejected with `error`.
    pub(super) fn rejected_promise_for(&mut self, error: &JsError) -> Result<JsValue, JsError> {
        let reason = self.error_to_thrown_value(error)?;
        let (promise, result) = self.create_promise()?;
        self.reject_promise(promise, &reason);
        Ok(result)
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
            // Only an array literal or pattern reads an elision, and it handles
            // the hole itself; evaluated on its own it is `undefined`.
            Expr::Elision => Ok(JsValue::Undefined),
            Expr::RegexLiteral { pattern, flags, .. } => {
                let object = self.construct_regex(pattern, flags)?;
                Ok(JsValue::Object(object))
            }
            Expr::This => self.current_this(),
            Expr::Identifier(name) => self.lookup_binding(dom, name),
            Expr::Function {
                name,
                parameters,
                body,
                kind,
                ..
            } => self.evaluate_function_expression(name.as_deref(), parameters, body, *kind),
            Expr::Arrow {
                parameters,
                body,
                is_async,
                ..
            } => self.create_arrow_function(None, parameters, body, *is_async),
            Expr::Class {
                name,
                super_class,
                elements,
                ..
            } => self.evaluate_class(dom, name.as_deref(), super_class.as_deref(), elements),
            Expr::SuperMember { property, .. } => self.read_super_property(dom, property),
            Expr::SuperComputedMember { property, .. } => {
                let key = self.evaluate(dom, property)?.to_js_string();
                self.read_super_property(dom, &key)
            }
            Expr::SuperCall { arguments, .. } => {
                let class = self.class_frame().ok_or_else(|| {
                    JsError::new(JsErrorKind::Syntax, "'super' keyword unexpected here", None)
                })?;
                if !class.derived {
                    return Err(JsError::type_error(
                        "super() is only valid in derived constructors",
                    ));
                }
                let Some(super_constructor) = class.super_constructor else {
                    return Err(JsError::type_error(
                        "super() called in a class without a parent constructor",
                    ));
                };
                let mut values = Vec::with_capacity(arguments.len());
                for argument in arguments {
                    self.evaluate_argument(dom, argument, &mut values)?;
                }
                // The parent runs with the derived class's `new.target`
                // (ECMA-262 §13.3.7.1), so a plain-function or class parent
                // can tell which constructor the instance is for.
                self.super_call_pending = true;
                let instance = self.construct(dom, super_constructor, &values);
                self.super_call_pending = false;
                let instance = instance?;
                let JsValue::Object(instance) = instance else {
                    return Err(JsError::type_error(
                        "super constructor returned a non-object",
                    ));
                };
                // A built-in parent builds its instance from its own prototype;
                // `super()` must re-parent it onto `new.target.prototype`, or a
                // subclass of `Error`, `Map`, `Array`, `Event`… would not be an
                // instance of itself (ECMA-262 `OrdinaryCreateFromConstructor`).
                if let Some(JsValue::Object(new_target)) = self.new_target_stack.last().cloned()
                    && let Some(JsValue::Object(prototype)) =
                        self.realm.get_property(new_target, "prototype")
                    && self.realm.get_prototype(instance) != Some(prototype)
                {
                    self.realm.set_prototype(instance, Some(prototype));
                }
                self.initialize_this(JsValue::Object(instance));
                self.run_instance_fields(dom, &class, instance)?;
                Ok(JsValue::Object(instance))
            }
            Expr::NewTarget => Ok(self
                .new_target_stack
                .last()
                .cloned()
                .unwrap_or(JsValue::Undefined)),
            Expr::PrivateMember { object, name, .. } => {
                let object = self.evaluate(dom, object)?;
                self.read_private(dom, &object, name)
            }
            Expr::PrivateIn { name, object, .. } => {
                let object = self.evaluate(dom, object)?;
                self.private_in(&object, name).map(JsValue::Boolean)
            }
            Expr::Object(properties) => self.evaluate_object_literal(dom, properties),
            Expr::Array(elements) => self.evaluate_array_literal(dom, elements),
            Expr::Spread(expression) => self.evaluate(dom, expression),
            // Outside a coroutine (a module's top level, which is not yet run
            // as one) suspension is unavailable, so the operand is evaluated
            // and its value stands in for the awaited or yielded result.
            Expr::Await(operand) => self.evaluate(dom, operand),
            Expr::Yield { argument, .. } => match argument {
                Some(argument) => self.evaluate(dom, argument),
                None => Ok(JsValue::Undefined),
            },
            Expr::OptionalChain(inner) => match self.evaluate(dom, inner) {
                Err(error) if error.is_optional_short_circuit() => Ok(JsValue::Undefined),
                other => other,
            },
            Expr::OptionalGuard(inner) => {
                let value = self.evaluate(dom, inner)?;
                if matches!(value, JsValue::Null | JsValue::Undefined) {
                    return Err(JsError::optional_short_circuit());
                }
                Ok(value)
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
            } if matches!(operand.as_ref(), Expr::Identifier(_)) => {
                let Expr::Identifier(name) = operand.as_ref() else {
                    unreachable!("matched by the guard")
                };
                let scope = self.resolve_name(dom, name)?;
                if scope.is_none() && !self.global_name_exists(name) {
                    return Ok(JsValue::String("undefined".to_owned()));
                }
                let value = self.read_resolved_binding(dom, scope, name)?;
                self.evaluate_unary(dom, UnaryOp::Typeof, &value)
            }
            Expr::Unary {
                operator, operand, ..
            } => {
                let value = self.evaluate(dom, operand)?;
                self.evaluate_unary(dom, *operator, &value)
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
                // ECMA-262 13.4.2: the operand is converted with ToNumeric, and
                // the step is 1 of that same numeric type (`1n` for a BigInt).
                // A postfix expression answers the converted old value.
                let numeric = self.to_numeric_value(dom, &previous)?;
                let step = match numeric {
                    JsValue::BigInt(_) => JsValue::BigInt(JsBigInt::from_i64(1)),
                    _ => JsValue::Number(1.0),
                };
                let next = self.binary_operation(dom, *operator, numeric.clone(), step)?;
                self.write_assignment_reference(dom, &reference, next.clone())?;
                Ok(if *prefix { next } else { numeric })
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
                let key_value = self.to_property_key_value(dom, key_value)?;
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
                // A `new` expression constructs with its own new.target: the
                // constructor being evaluated. Push it so a nested `new` inside
                // another constructor's body does not inherit the enclosing
                // construction's new.target — that leak gave the nested
                // instance the outer prototype and tripped transpiled
                // `_classCallCheck` guards ("Cannot call a class as a
                // function"). `super()` keeps using the stack top because it
                // must construct the parent with the derived new.target.
                self.new_target_stack.push(JsValue::Object(constructor));
                let constructed = self.construct(dom, constructor, &values);
                self.new_target_stack.pop();
                constructed
            }
            Expr::Call {
                callee, arguments, ..
            } => self.evaluate_call(dom, callee, arguments),
            Expr::TaggedTemplate {
                tag,
                quasis,
                expressions,
                ..
            } => {
                // §13.3.11: the tag is resolved first, then the template object
                // is created, then the substitutions run left to right, and only
                // then is the tag called with all of them.
                let Some((callee, receiver)) = self.resolve_call_target(dom, tag)? else {
                    return Ok(JsValue::Undefined);
                };
                let template = self.create_template_object(quasis)?;
                let mut values = Vec::with_capacity(expressions.len() + 1);
                values.push(JsValue::Object(template));
                for expression in expressions {
                    self.evaluate_argument(dom, expression, &mut values)?;
                }
                self.call_with_this(dom, callee, &values, receiver)
            }
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
                let combined = self.binary_operation(dom, *operator, current, right)?;
                self.write_assignment_reference(dom, &reference, combined.clone())?;
                Ok(combined)
            }
            Expr::Assignment {
                target,
                value,
                parenthesized_target,
                ..
            } => {
                if matches!(target.as_ref(), Expr::Array(_) | Expr::Object(_)) {
                    let value = self.evaluate(dom, value)?;
                    self.assign_destructuring_target(dom, target, value.clone())?;
                    return Ok(value);
                }
                // ECMA-262 13.15.2: a computed key converts (ToPropertyKey) in
                // PutValue, which runs after the value expression.
                match target.as_ref() {
                    Expr::ComputedMember {
                        object, property, ..
                    } => {
                        let (object, key) = self.computed_member_parts(dom, object, property)?;
                        let value = self.evaluate(dom, value)?;
                        let reference = self.property_key_reference(dom, object, key)?;
                        self.write_assignment_reference(dom, &reference, value.clone())?;
                        return Ok(value);
                    }
                    Expr::SuperComputedMember { property, .. } => {
                        let key = self.evaluate(dom, property)?;
                        let value = self.evaluate(dom, value)?;
                        let key = self.to_property_key_value(dom, key)?.to_js_string();
                        let reference = AssignmentReference::SuperProperty { property: key };
                        self.write_assignment_reference(dom, &reference, value.clone())?;
                        return Ok(value);
                    }
                    _ => {}
                }
                let reference = self.resolve_assignment_reference(dom, target)?;
                let value = match target.as_ref() {
                    Expr::Identifier(name) if !parenthesized_target => {
                        self.evaluate_named(dom, value, name)?
                    }
                    _ => self.evaluate(dom, value)?,
                };
                self.write_assignment_reference(dom, &reference, value.clone())?;
                Ok(value)
            }
            Expr::LogicalAssignment {
                target,
                operator,
                value,
                ..
            } => {
                let reference = self.resolve_assignment_reference(dom, target)?;
                let current = self.read_assignment_reference(dom, &reference)?;
                let short_circuits = match operator {
                    BinaryOp::LogicalAnd => !current.is_truthy(),
                    BinaryOp::LogicalOr => current.is_truthy(),
                    BinaryOp::Nullish => !matches!(current, JsValue::Null | JsValue::Undefined),
                    _ => false,
                };
                if short_circuits {
                    return Ok(current);
                }
                let value = match target.as_ref() {
                    Expr::Identifier(name) => self.evaluate_named(dom, value, name)?,
                    _ => self.evaluate(dom, value)?,
                };
                self.write_assignment_reference(dom, &reference, value.clone())?;
                Ok(value)
            }
        }
    }

    /// The base object and the unconverted key of `object[property]`. The base
    /// is checked for `null` and `undefined` here, before any key conversion
    /// (ECMA-262 `GetValue` and `PutValue` both coerce the base first), so the
    /// key stays a value and the caller decides when `ToPropertyKey` runs.
    fn computed_member_parts(
        &mut self,
        dom: &mut Dom,
        object: &Expr,
        property: &Expr,
    ) -> Result<(ObjectId, JsValue), JsError> {
        let evaluated = self.evaluate(dom, object)?;
        let key_value = self.evaluate(dom, property)?;
        let object = self.coerce_member_base(&evaluated, &key_value.to_js_string())?;
        Ok((object, key_value))
    }

    /// The reference for `object[key]` once `key` has been converted with
    /// `ToPropertyKey`: a symbol names a symbol slot, anything else a string.
    fn property_key_reference(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        key: JsValue,
    ) -> Result<AssignmentReference, JsError> {
        match self.to_property_key_value(dom, key)? {
            JsValue::Symbol(symbol) => Ok(AssignmentReference::SymbolProperty { object, symbol }),
            key => Ok(AssignmentReference::Property {
                object,
                property: key.to_js_string(),
            }),
        }
    }

    pub(super) fn resolve_assignment_reference(
        &mut self,
        dom: &mut Dom,
        target: &Expr,
    ) -> Result<AssignmentReference, JsError> {
        match target {
            Expr::Identifier(name) => Ok(AssignmentReference::Binding {
                name: name.clone(),
                scope: self.resolve_name(dom, name)?,
            }),
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
                let (object, key) = self.computed_member_parts(dom, object, property)?;
                self.property_key_reference(dom, object, key)
            }
            Expr::PrivateMember { object, name, .. } => {
                let evaluated = self.evaluate(dom, object)?;
                let object = self.coerce_member_base(&evaluated, name)?;
                Ok(AssignmentReference::Private {
                    object,
                    name: name.clone(),
                })
            }
            Expr::SuperMember { property, .. } => Ok(AssignmentReference::SuperProperty {
                property: property.clone(),
            }),
            Expr::SuperComputedMember { property, .. } => {
                let key = self.evaluate(dom, property)?;
                let key = self.to_property_key_value(dom, key)?.to_js_string();
                Ok(AssignmentReference::SuperProperty { property: key })
            }
            _ => Err(JsError::new(
                JsErrorKind::Syntax,
                "invalid assignment target",
                None,
            )),
        }
    }

    /// `BindingInitialization` (ECMA-262 8.5.3): walk a declaration pattern
    /// and initialize each leaf binding.
    ///
    /// This is deliberately *not* an `assign_destructuring_target` call on a
    /// synthesized target. The array form draws its values from the iterator
    /// protocol: `var [a] = map` binds the map's first entry, `var [a] = new
    /// Uint8Array(..)` goes through `@@iterator`, and a source with no
    /// `@@iterator` throws a `TypeError` instead of quietly reading indexed
    /// properties off an object that happens to have them.
    pub(super) fn initialize_binding_pattern(
        &mut self,
        dom: &mut Dom,
        pattern: &BindingPattern,
        value: JsValue,
        kind: VariableKind,
    ) -> Result<(), JsError> {
        match pattern {
            BindingPattern::Identifier(name) => self.initialize_binding(dom, name, value, kind),
            BindingPattern::Default {
                pattern,
                value: fallback,
            } => {
                let value = if matches!(value, JsValue::Undefined) {
                    match pattern.as_ref() {
                        BindingPattern::Identifier(name) => {
                            self.evaluate_named(dom, fallback, name)?
                        }
                        _ => self.evaluate(dom, fallback)?,
                    }
                } else {
                    value
                };
                self.initialize_binding_pattern(dom, pattern, value, kind)
            }
            BindingPattern::Object { properties, rest } => {
                // `BindingInitialization` for an object pattern starts from
                // `RequireObjectCoercible`: `null` and `undefined` throw,
                // while a primitive boxes so `var {length} = 'abc'` works.
                if matches!(value, JsValue::Null | JsValue::Undefined) {
                    return Err(JsError::type_error(format!(
                        "Cannot destructure '{}' as it is {}",
                        value.to_js_string(),
                        value.to_js_string()
                    )));
                }
                let object = self.to_object(&value)?;
                let mut excluded: Vec<String> = Vec::new();
                for (key, pattern) in properties {
                    let name = match key {
                        PropertyKey::Static(name) => name.clone(),
                        PropertyKey::Computed(expression) => {
                            let computed = self.evaluate(dom, expression)?;
                            self.to_property_key_value(dom, computed)?.to_js_string()
                        }
                        PropertyKey::Spread => {
                            unreachable!("the rest element is parsed separately")
                        }
                        PropertyKey::Private(_) => {
                            unreachable!("a binding pattern cannot carry a private name")
                        }
                    };
                    // Read before pushing the exclusion so the getter runs even
                    // when a later property repeats the key.
                    let property_value = self.get_member(dom, object, &name)?;
                    excluded.push(name);
                    self.initialize_binding_pattern(dom, pattern, property_value, kind)?;
                }
                if let Some(rest) = rest {
                    let object = self.create_object_rest(dom, &value, &excluded)?;
                    self.initialize_binding_pattern(dom, rest, JsValue::Object(object), kind)?;
                }
                Ok(())
            }
            BindingPattern::Array { elements, rest } => {
                let mut iterator = ArrayDestructuring::open(self, dom, &value)?;
                // An abrupt completion part-way through the pattern still has
                // to close the iterator, or a source holding a resource leaks
                // it. The original error wins; `close` never masks it.
                let outcome = (|| -> Result<(), JsError> {
                    for element in elements {
                        match element {
                            Some(pattern) => {
                                let element = iterator.next(self, dom)?;
                                self.initialize_binding_pattern(dom, pattern, element, kind)?;
                            }
                            // An elision still consumes a value. `[,,a]` over
                            // five values reads three of them, not one, and a
                            // source counting its own steps sees the difference.
                            None => {
                                iterator.next(self, dom)?;
                            }
                        }
                    }
                    match rest {
                        Some(rest) => {
                            let remaining = iterator.rest(self, dom)?;
                            self.initialize_binding_pattern(
                                dom,
                                rest,
                                JsValue::Object(remaining),
                                kind,
                            )?;
                        }
                        // A pattern that consumed fewer values than the source
                        // offers must close the iterator rather than drain it,
                        // or an endless generator would never terminate.
                        None => iterator.close_normal(self, dom)?,
                    }
                    Ok(())
                })();
                if outcome.is_err() {
                    iterator.close(self, dom);
                }
                outcome
            }
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
                    match target.as_ref() {
                        Expr::Identifier(name) => self.evaluate_named(dom, default, name)?,
                        _ => self.evaluate(dom, default)?,
                    }
                } else {
                    value
                };
                self.assign_destructuring_target(dom, target, value)
            }
            Expr::Array(targets) => {
                // Like a binding pattern, an assignment pattern takes only the
                // values its elements need, and closes the iterator if it stops
                // before the source is done (ECMA-262 13.15.5.5).
                let mut iterator = ArrayDestructuring::open(self, dom, &value)?;
                let outcome = (|| -> Result<(), JsError> {
                    for target in targets {
                        match target {
                            // An elision still consumes a value.
                            Expr::Elision => {
                                iterator.next(self, dom)?;
                            }
                            // A rest target is the last element and takes what is left.
                            Expr::Spread(target) => {
                                let remaining = iterator.rest(self, dom)?;
                                return self.assign_destructuring_target(
                                    dom,
                                    target,
                                    JsValue::Object(remaining),
                                );
                            }
                            target => {
                                let element = iterator.next(self, dom)?;
                                self.assign_destructuring_target(dom, target, element)?;
                            }
                        }
                    }
                    iterator.close_normal(self, dom)
                })();
                if outcome.is_err() {
                    iterator.close(self, dom);
                }
                outcome
            }
            Expr::Object(properties) => {
                // `DestructuringAssignmentEvaluation` for an object pattern
                // starts with `RequireObjectCoercible`, which throws for
                // `null` and `undefined` while a primitive boxes into its
                // wrapper. The earlier version of this comment claimed
                // `ToObject` and invented a fresh object for the nullish
                // case; that is not what the specification says, and it left
                // `({a} = undefined)` silently assigning `undefined` where
                // every real engine throws.
                if matches!(value, JsValue::Null | JsValue::Undefined) {
                    return Err(JsError::type_error(format!(
                        "Cannot destructure '{}' as it is {}",
                        value.to_js_string(),
                        value.to_js_string()
                    )));
                }
                let object = self.to_object(&value)?;
                let mut excluded = Vec::new();
                for property in properties {
                    if matches!(&property.key, PropertyKey::Spread) {
                        let rest = self.create_object_rest(dom, &value, &excluded)?;
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
                            let computed = self.evaluate(dom, expression)?;
                            self.to_property_key_value(dom, computed)?.to_js_string()
                        }
                        PropertyKey::Spread => unreachable!("spread handled above"),
                        PropertyKey::Private(_) => {
                            unreachable!("object literals cannot carry private names")
                        }
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
            AssignmentReference::Binding { name, scope } => {
                self.read_resolved_binding(dom, *scope, name)
            }
            AssignmentReference::Property { object, property } => {
                self.get_member(dom, *object, property)
            }
            AssignmentReference::SymbolProperty { object, symbol } => {
                self.get_symbol_value(dom, *object, symbol)
            }
            AssignmentReference::Private { object, name } => {
                self.read_private(dom, &JsValue::Object(*object), name)
            }
            AssignmentReference::SuperProperty { property } => {
                self.read_super_property(dom, property)
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
            AssignmentReference::Binding { name, scope } => {
                self.write_resolved_binding(dom, *scope, name, value)
            }
            AssignmentReference::Property { object, property } => {
                self.set_member(dom, *object, property, value)
            }
            AssignmentReference::SymbolProperty { object, symbol } => {
                self.set_symbol_value(dom, *object, symbol, value)
            }
            AssignmentReference::Private { object, name } => {
                self.write_private(dom, &JsValue::Object(*object), name, value)
            }
            AssignmentReference::SuperProperty { property } => {
                self.write_super_property(dom, property, value)
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
                            self.delete_property_value(dom, object, &property)?,
                        ))
                    }
                    AssignmentReference::SymbolProperty { object, symbol } => Ok(JsValue::Boolean(
                        self.realm.delete_symbol_property(object, &symbol),
                    )),
                    AssignmentReference::Private { .. }
                    | AssignmentReference::SuperProperty { .. } => Err(JsError::new(
                        JsErrorKind::Syntax,
                        "private and super members cannot be deleted",
                        None,
                    )),
                    AssignmentReference::Binding { .. } => {
                        unreachable!("member expressions resolve to property references");
                    }
                }
            }
            // A binding in a `with` object environment is deleted from that
            // object (ECMA-262 9.1.1.2.7); other bindings cannot be deleted.
            Expr::Identifier(name) => {
                match self
                    .resolve_name(dom, name)?
                    .and_then(|depth| self.with_object_at(depth))
                {
                    Some(object) => Ok(JsValue::Boolean(
                        self.delete_property_value(dom, object, name)?,
                    )),
                    None => Ok(JsValue::Boolean(false)),
                }
            }
            _ => {
                self.evaluate(dom, operand)?;
                Ok(JsValue::Boolean(true))
            }
        }
    }

    /// ECMA-262 [[Get]]: read a property through the prototype chain,
    /// invoking accessor getters with `this` bound to the original receiver.
    /// Ordinary property reads keep using `Realm::get_property` fast paths;
    /// this entry point is required wherever accessors may exist.
    /// The active `this` binding, walking the environment chain past block
    /// and arrow scopes. Top-level code has no binding and sees the global
    /// object; a derived constructor's binding is uninitialized until
    /// `super()` runs.
    pub(super) fn current_this(&self) -> Result<JsValue, JsError> {
        for environment in self.environment.iter().rev() {
            let environment = environment.borrow();
            if let Some(binding) = environment.bindings.get("this") {
                if !binding.initialized {
                    return Err(JsError::reference(
                        "must call super constructor before accessing 'this'",
                    ));
                }
                return Ok(binding.value.clone());
            }
        }
        Ok(JsValue::Object(self.realm.global_object()))
    }

    /// Bind `this` in the nearest environment that declares the binding (the
    /// active derived constructor's call environment).
    pub(super) fn initialize_this(&mut self, value: JsValue) {
        for environment in self.environment.iter().rev() {
            let mut environment = environment.borrow_mut();
            if let Some(binding) = environment.bindings.get_mut("this") {
                binding.value = value;
                binding.initialized = true;
                return;
            }
        }
    }

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
        // IteratorStep reads `done` and `value` with [[Get]], so getters run
        // and their errors propagate (ECMA-262 7.4.5-7.4.6).
        if self.get_member(dom, result, "done")?.is_truthy() {
            return Ok(None);
        }
        Ok(Some(self.get_member(dom, result, "value")?))
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
                self.array_elements_for(*object)
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
                    .map(|length| super::builtins::array::to_length(&length))
                    .transpose()?
                    .unwrap_or(0.0);
                if array_like > super::builtins::array::MAX_MATERIALIZED_ELEMENTS as f64 {
                    return Err(JsError::resource(
                        "array-like length exceeds the materialization bound",
                    ));
                }
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let count = array_like as usize;
                let mut values = Vec::new();
                values
                    .try_reserve_exact(count)
                    .map_err(|_| JsError::resource("array-like exceeds the available heap"))?;
                for index in 0..count {
                    values.push(self.get_member(dom, *object, &index.to_string())?);
                }
                Ok(values)
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
            // A non-writable data property ignores the write, as for string keys.
            if !own.writable {
                return Ok(());
            }
            self.realm
                .define_symbol_property(object, symbol, PropertyDescriptor::data(value));
            return Ok(());
        }
        if let Some(descriptor) = self.realm.get_symbol_descriptor(object, symbol) {
            if descriptor.is_accessor() {
                if let Some(setter) = descriptor.setter {
                    self.call_with_this(dom, setter, &[value], JsValue::Object(object))?;
                }
                return Ok(());
            }
            // An inherited non-writable data property blocks creating an own one
            // (ECMA-262 10.1.9.2 OrdinarySetWithOwnDescriptor).
            if !descriptor.writable {
                return Ok(());
            }
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
        if let Some(descriptor) = inherited.filter(crate::value::PropertyDescriptor::is_accessor) {
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

    /// ECMA-262 `ToNumeric` (7.1.3): the primitive a numeric operator works
    /// on, which is a `BigInt` or a Number. The hint is `number`, as the spec
    /// requests for every numeric operator.
    pub(super) fn to_numeric_value(
        &mut self,
        dom: &mut Dom,
        value: &JsValue,
    ) -> Result<JsValue, JsError> {
        match self.to_primitive_with_hint(dom, value.clone(), PrimitiveHint::Number)? {
            primitive @ JsValue::BigInt(_) => Ok(primitive),
            primitive => Ok(JsValue::Number(to_number(&primitive)?)),
        }
    }

    /// `Number(value)` (ECMA-262 21.1.1.1): `ToNumeric`, then a `BigInt` becomes
    /// the Number nearest its value. Unlike `ToNumber`, this accepts a `BigInt`.
    pub(super) fn number_conversion(
        &mut self,
        dom: &mut Dom,
        value: &JsValue,
    ) -> Result<f64, JsError> {
        match self.to_numeric_value(dom, value)? {
            JsValue::BigInt(number) => Ok(number.to_f64()),
            primitive => to_number(&primitive),
        }
    }

    /// ECMA-262 `ToNumber` for values that may be objects: run `ToPrimitive`
    /// (number hint) so user-defined `valueOf`/`toString` participate, then
    /// apply the primitive conversion.
    pub(super) fn to_number_value(
        &mut self,
        dom: &mut Dom,
        value: &JsValue,
    ) -> Result<f64, JsError> {
        if matches!(value, JsValue::Object(_)) {
            let primitive =
                self.to_primitive_with_hint(dom, value.clone(), PrimitiveHint::Number)?;
            return to_number(&primitive);
        }
        to_number(value)
    }

    /// `ToIntegerOrInfinity` for an optional position argument, where an absent
    /// argument and `undefined` both read as 0.
    pub(super) fn optional_integer_value(
        &mut self,
        dom: &mut Dom,
        value: Option<&JsValue>,
    ) -> Result<f64, JsError> {
        match value {
            None | Some(JsValue::Undefined) => Ok(0.0),
            Some(other) => self.to_integer_value(dom, other),
        }
    }

    /// ECMA-262 `ToIntegerOrInfinity` for a value that may be an object, so a
    /// built-in's index or count argument can carry a `valueOf`.
    pub(super) fn to_integer_value(
        &mut self,
        dom: &mut Dom,
        value: &JsValue,
    ) -> Result<f64, JsError> {
        let number = self.to_number_value(dom, value)?;
        Ok(super::convert::integer_or_infinity(number))
    }

    /// ECMA-262 `ToLength` for a value that may be an object.
    pub(super) fn to_length_value(
        &mut self,
        dom: &mut Dom,
        value: &JsValue,
    ) -> Result<f64, JsError> {
        let integer = self.to_integer_value(dom, value)?;
        Ok(integer.clamp(0.0, 9_007_199_254_740_991.0))
    }

    /// ECMA-262 7.1.19 `ToPropertyKey` for an evaluated key: an object runs
    /// `ToPrimitive` with the string hint first, so a symbol wrapper addresses
    /// the symbol it wraps and `[1, 2]` names `"1,2"`. Symbols and strings come
    /// back unchanged; any other object becomes its primitive's string key.
    pub(super) fn to_property_key_value(
        &mut self,
        dom: &mut Dom,
        value: JsValue,
    ) -> Result<JsValue, JsError> {
        if !matches!(value, JsValue::Object(_)) {
            return Ok(value);
        }
        match self.to_primitive_with_hint(dom, value, PrimitiveHint::String)? {
            symbol @ JsValue::Symbol(_) => Ok(symbol),
            primitive => Ok(JsValue::String(primitive.to_js_string())),
        }
    }

    /// ECMA-262 `ToString` for values that may be objects: run `ToPrimitive`
    /// (string hint) so user-defined `toString`/`valueOf` participate, the
    /// way real-world code and polyfills (`String(obj)`) expect.
    pub(super) fn to_string_value(
        &mut self,
        dom: &mut Dom,
        value: &JsValue,
    ) -> Result<String, JsError> {
        let primitive = self.to_primitive_with_hint(dom, value.clone(), PrimitiveHint::String)?;
        // The trace is for objects only, so a primitive does not pay for an
        // environment lookup on every conversion.
        if let JsValue::Object(object) = value
            && std::env::var_os("RENDER_TRACE_STRING").is_some()
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

    /// `ToString` of one template substitution (ECMA-262 13.2.8.6): an object
    /// takes the string hint, and a `Symbol` is a `TypeError`.
    fn template_substitution_text(
        &mut self,
        dom: &mut Dom,
        value: &JsValue,
    ) -> Result<String, JsError> {
        match value {
            JsValue::Symbol(_) => Err(JsError::type_error(
                "Cannot convert a Symbol value to a string",
            )),
            JsValue::Object(_) => self.to_string_value(dom, value),
            other => Ok(other.to_js_string()),
        }
    }

    fn join_array_elements(&mut self, dom: &mut Dom, array: ObjectId) -> Result<String, JsError> {
        let mut parts = Vec::new();
        for value in self.array_elements_for(array)? {
            parts.push(match value {
                JsValue::Null | JsValue::Undefined => String::new(),
                value => self.to_string_value(dom, &value)?,
            });
        }
        Ok(parts.join(","))
    }

    pub(super) fn to_primitive_with_hint(
        &mut self,
        dom: &mut Dom,
        value: JsValue,
        hint: PrimitiveHint,
    ) -> Result<JsValue, JsError> {
        let JsValue::Object(object) = value else {
            return Ok(value);
        };
        match self.realm.host(object) {
            Some(ObjectHost::DateInstance(ms)) if hint != PrimitiveHint::String => {
                return Ok(JsValue::Number(ms));
            }
            Some(ObjectHost::StringPrimitive(text)) => return Ok(JsValue::String(text)),
            Some(ObjectHost::NumberPrimitive(number)) => return Ok(JsValue::Number(number)),
            Some(ObjectHost::BigIntPrimitive(value)) => return Ok(JsValue::BigInt(value)),
            Some(ObjectHost::BooleanPrimitive(value)) => return Ok(JsValue::Boolean(value)),
            // §20.4.3 `Symbol.prototype[@@toPrimitive]` returns the symbol itself,
            // so a symbol wrapper converts to the symbol (and not its description).
            Some(ObjectHost::SymbolInstance(symbol)) => return Ok(JsValue::Symbol(symbol)),
            Some(ObjectHost::Array) => {
                // An array already being joined further up the stack is a
                // cycle and contributes nothing (ECMA-262 leaves this to the
                // host; every engine returns the empty string).
                if self.arrays_joining.contains(&object) {
                    return Ok(JsValue::String(String::new()));
                }
                self.arrays_joining.push(object);
                let text = self.join_array_elements(dom, object);
                self.arrays_joining.pop();
                return Ok(JsValue::String(text?));
            }
            _ => {}
        }
        // An exotic `Symbol.toPrimitive` method gets first refusal, called
        // with the coercion hint; a primitive result short-circuits. The method
        // is read with [[Get]]: an accessor is run as its getter, with the object
        // as receiver, and the getter's result is the method.
        let exotic_method = match self
            .realm
            .get_symbol_descriptor(object, &JsSymbol::well_known("@@toPrimitive"))
        {
            None => JsValue::Undefined,
            Some(descriptor) if descriptor.is_accessor() => match descriptor.getter {
                Some(getter) => self.call_with_this(dom, getter, &[], JsValue::Object(object))?,
                None => JsValue::Undefined,
            },
            Some(descriptor) => descriptor.value,
        };
        // ECMA-262 7.1.1 `GetMethod`: `undefined` and `null` mean there is no
        // exotic method, and any other value must be callable or the
        // conversion throws.
        match exotic_method {
            JsValue::Undefined | JsValue::Null => {}
            JsValue::Object(method) if Self::is_callable_object(method, &self.realm) => {
                let invoked = self.call_with_this(
                    dom,
                    method,
                    &[JsValue::String(hint.name().to_owned())],
                    JsValue::Object(object),
                )?;
                if !matches!(invoked, JsValue::Object(_)) {
                    return Ok(invoked);
                }
                return Err(JsError::type_error(
                    "Cannot convert object to primitive value",
                ));
            }
            _ => {
                return Err(JsError::type_error("Symbol.toPrimitive is not a function"));
            }
        }
        // OrdinaryToPrimitive: `number` tries valueOf then toString; `string`
        // reverses the order; `default` follows the number order.
        let method_order: [&str; 2] = if hint == PrimitiveHint::String {
            ["toString", "valueOf"]
        } else {
            ["valueOf", "toString"]
        };
        for method in method_order {
            let JsValue::Object(callable) = self.get_member(dom, object, method)? else {
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
        // OrdinaryToPrimitive has no primitive left to return for an ordinary
        // object (ECMA-262 7.1.1.1), so the conversion throws. A host object
        // without a usable `valueOf`/`toString` keeps the ordinary object string
        // representation, so string concatenation and URL/logging code do not
        // abort on an incomplete platform object.
        if matches!(self.realm.host(object), Some(ObjectHost::Ordinary) | None) {
            return Err(JsError::type_error(
                "Cannot convert object to primitive value",
            ));
        }
        Ok(JsValue::String("[object Object]".to_owned()))
    }

    pub(super) fn evaluate_call(
        &mut self,
        dom: &mut Dom,
        callee: &Expr,
        arguments: &[Expr],
    ) -> Result<JsValue, JsError> {
        let Some((callee, receiver)) = self.resolve_call_target(dom, callee)? else {
            return Ok(JsValue::Undefined);
        };
        let mut values = Vec::with_capacity(arguments.len());
        for argument in arguments {
            self.evaluate_argument(dom, argument, &mut values)?;
        }
        self.call_with_this(dom, callee, &values, receiver)
    }

    /// Resolve the callee of a call expression to the callable to invoke plus
    /// the receiver `this` will see. Shared with `Expr::TaggedTemplate`, whose
    /// argument list is already-built values rather than expressions.
    ///
    /// `None` is the engine's documented lenient path for a callee that is not
    /// a function at all: a nullish or primitive callee makes the whole
    /// expression evaluate to `undefined` without evaluating its arguments.
    pub(super) fn resolve_call_target(
        &mut self,
        dom: &mut Dom,
        callee: &Expr,
    ) -> Result<Option<(ObjectId, JsValue)>, JsError> {
        let callee_label = match callee {
            Expr::Member { property, .. } => format!(".{property}"),
            Expr::ComputedMember { .. } => "[]".to_owned(),
            Expr::SuperMember { property, .. } => format!(".{property}"),
            Expr::SuperComputedMember { .. } => "[]".to_owned(),
            Expr::PrivateMember { name, .. } => format!(".#{name}"),
            _ => String::new(),
        };
        let (callee_value, receiver) = match callee {
            // `f?.()` / `o.m?.()`: resolve the inner callee with its receiver;
            // a callee that is nullish ends the whole chain.
            Expr::OptionalGuard(inner) => {
                return match self.resolve_call_target(dom, inner)? {
                    Some(target) => Ok(Some(target)),
                    None => Err(JsError::optional_short_circuit()),
                };
            }
            Expr::SuperMember { property, .. } => {
                let value = self.read_super_property(dom, property)?;
                let receiver = self.current_this()?;
                (value, receiver)
            }
            Expr::SuperComputedMember { property, .. } => {
                let key = self.evaluate(dom, property)?;
                let key = self.to_property_key_value(dom, key)?.to_js_string();
                let value = self.read_super_property(dom, &key)?;
                let receiver = self.current_this()?;
                (value, receiver)
            }
            Expr::PrivateMember { object, name, .. } => {
                let receiver = self.evaluate(dom, object)?;
                let value = self.read_private(dom, &receiver, name)?;
                (value, receiver)
            }
            Expr::Member {
                object, property, ..
            } => {
                let receiver = self.evaluate(dom, object)?;
                if matches!(receiver, JsValue::Null | JsValue::Undefined) {
                    return Ok(None);
                }
                let object = self.coerce_member_base(&receiver, property)?;
                (self.get_member(dom, object, property)?, receiver)
            }
            Expr::ComputedMember {
                object, property, ..
            } => {
                let receiver = self.evaluate(dom, object)?;
                if matches!(receiver, JsValue::Null | JsValue::Undefined) {
                    return Ok(None);
                }
                let key_value = self.evaluate(dom, property)?;
                let object = self.coerce_member_base(&receiver, &key_value.to_js_string())?;
                let key_value = self.to_property_key_value(dom, key_value)?;
                let key = key_value.to_js_string();
                let callee = if let JsValue::Symbol(symbol) = &key_value {
                    self.get_symbol_value(dom, object, symbol)?
                } else {
                    self.get_member(dom, object, &key)?
                };
                (callee, receiver)
            }
            Expr::Identifier(name) => {
                let scope = self.resolve_name(dom, name)?;
                // A function found on a `with` object is called with that
                // object as `this` (ECMA-262 9.1.1.2.7, WithBaseObject).
                let receiver = match scope.and_then(|depth| self.with_object_at(depth)) {
                    Some(object) => JsValue::Object(object),
                    None => JsValue::Undefined,
                };
                (self.read_resolved_binding(dom, scope, name)?, receiver)
            }
            _ => (self.evaluate(dom, callee)?, JsValue::Undefined),
        };
        let callee = match callee_value {
            // Web pages routinely feature-detect optional host methods through
            // a call guarded by a surrounding branch. Treat a missing host hook
            // as an inert call so one telemetry shim cannot abort the entire
            // application bootstrap.
            JsValue::Undefined
            | JsValue::Null
            | JsValue::String(_)
            | JsValue::Number(_)
            | JsValue::Boolean(_)
            | JsValue::Symbol(_) => return Ok(None),
            // A BigInt is a primitive that is never callable, so unlike the
            // missing host hooks above it is a TypeError (ECMA-262 13.3.6.2).
            JsValue::BigInt(_) => {
                return Err(JsError::type_error(format!(
                    "value of callee{callee_label} is undefined or not callable"
                )));
            }
            value @ JsValue::Object(_) => Self::require_object(&value).map_err(|_| {
                JsError::type_error(format!(
                    "value of callee{callee_label} is undefined or not callable"
                ))
            })?,
        };
        Ok(Some((callee, receiver)))
    }

    /// ECMA-262 13.3.6 `GetTemplateObject`: a template object is an array
    /// exotic object whose indices are the cooked strings and whose `raw`
    /// property holds the unprocessed texts. The indices are ordinary data
    /// properties; only `raw` is non-enumerable, so a tag function can walk the
    /// strings with `for`/`map` exactly as it walks a real array.
    fn create_template_object(&mut self, quasis: &[(String, String)]) -> Result<ObjectId, JsError> {
        self.ensure_heap_capacity(2)?;
        let mut cooked = Vec::with_capacity(quasis.len());
        let mut raw = Vec::with_capacity(quasis.len());
        for (cooked_value, raw_value) in quasis {
            cooked.push(JsValue::String(cooked_value.clone()));
            raw.push(JsValue::String(raw_value.clone()));
        }
        let raw_object = self.create_array_from_values(&raw)?;
        let template = self.create_array_from_values(&cooked)?;
        let _ = self.realm.define_property(
            template,
            "raw",
            PropertyDescriptor {
                value: JsValue::Object(raw_object),
                writable: false,
                getter: None,
                setter: None,
                enumerable: false,
                configurable: false,
            },
        );
        Ok(template)
    }

    /// ECMA-262 8.4.5 `NamedEvaluation`. An anonymous function, arrow or class
    /// that is the whole value of a named binding, assignment or default takes
    /// that name. Any other expression is evaluated as usual.
    pub(super) fn evaluate_named(
        &mut self,
        dom: &mut Dom,
        expression: &Expr,
        name: &str,
    ) -> Result<JsValue, JsError> {
        match expression {
            Expr::Function {
                name: None,
                parameters,
                body,
                kind,
                ..
            } => self.create_function(Some(name), parameters, body, *kind),
            Expr::Arrow {
                parameters,
                body,
                is_async,
                ..
            } => self.create_arrow_function(Some(name), parameters, body, *is_async),
            Expr::Class {
                name: None,
                super_class,
                elements,
                ..
            } => self.evaluate_anonymous_class_named(dom, name, super_class.as_deref(), elements),
            _ => self.evaluate(dom, expression),
        }
    }

    pub(super) fn evaluate_function_expression(
        &mut self,
        name: Option<&str>,
        parameters: &[String],
        body: &[Statement],
        kind: FunctionKind,
    ) -> Result<JsValue, JsError> {
        let Some(name) = name else {
            return self.create_user_function(parameters, body, kind);
        };
        self.environment
            .push(Rc::new(RefCell::new(EnvironmentRecord::default())));
        let result = (|| {
            self.create_binding(name, VariableKind::Const, false, JsValue::Undefined)?;
            let value = self.create_function(Some(name), parameters, body, kind)?;
            self.initialize_declared_binding(name, value.clone(), VariableKind::Const)?;
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
                        // `CopyDataProperties` reads each key with `Get`, so a
                        // getter on the spread source contributes its result
                        // rather than the descriptor's empty value slot.
                        let keys = self.realm.enumerable_own_keys(source).unwrap_or_default();
                        for key in keys {
                            let value = self.get_member(dom, source, &key)?;
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
                    JsValue::Boolean(_)
                    | JsValue::Number(_)
                    | JsValue::BigInt(_)
                    | JsValue::Symbol(_) => {}
                }
                continue;
            }
            // The key expression evaluates exactly once (spec); a symbol
            // result installs a symbol-keyed property instead.
            let key_value = match &property.key {
                PropertyKey::Static(key) => JsValue::String(key.clone()),
                PropertyKey::Computed(expression) => {
                    let computed = self.evaluate(dom, expression)?;
                    self.to_property_key_value(dom, computed)?
                }
                PropertyKey::Spread => unreachable!("spread property handled above"),
                PropertyKey::Private(_) => {
                    unreachable!("object literals cannot carry private names")
                }
            };
            // ECMA-262 13.2.5.1 step 5.a: only the `__proto__: value` colon
            // member is special. A shorthand `{ __proto__ }`, a method
            // `{ __proto__() {} }`, an accessor, and the computed
            // `{ ["__proto__"]: v }` all install an ordinary own property.
            if matches!(&property.key, PropertyKey::Static(key) if key == "__proto__")
                && property.accessor.is_none()
                && !property.shorthand
                && !property.method
            {
                let value = self.evaluate(dom, &property.value)?;
                let prototype = match value {
                    JsValue::Object(object) => Some(object),
                    JsValue::Null => None,
                    // Any other primitive leaves `[[Prototype]]` untouched.
                    _ => continue,
                };
                self.realm.set_prototype(object, prototype);
                continue;
            }
            let symbol_key = match &key_value {
                JsValue::Symbol(symbol) => Some(symbol.clone()),
                _ => None,
            };
            let key = key_value.to_js_string();
            // `PropertyName : AssignmentExpression` names an anonymous function,
            // arrow or class after its key (ECMA-262 13.2.5.5 step 7). Methods and
            // accessors arrive already named by the parser.
            let value = if property.accessor.is_none() && !property.method && !property.shorthand {
                let name = property_key_function_name(&key_value);
                self.evaluate_named(dom, &property.value, &name)?
            } else {
                self.evaluate(dom, &property.value)?
            };
            if property.method || property.accessor.is_some() {
                self.set_method_home_object(&value, object);
            }
            if let Some(accessor) = property.accessor {
                // `{get x(){}}` / `{set x(v){}}`, and their symbol-keyed forms
                // `{get [s](){}}`, install accessor slots; a repeated member of
                // either kind extends the same descriptor.
                let function = match value {
                    JsValue::Object(function)
                        if JsRuntime::is_callable_object(function, &self.realm) =>
                    {
                        function
                    }
                    _ => return Err(JsError::type_error("object accessor must be a function")),
                };
                let existing = match &symbol_key {
                    Some(symbol) => self.realm.own_symbol_property(object, symbol),
                    None => self.realm.own_property(object, &key),
                };
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
                let descriptor = PropertyDescriptor {
                    value: JsValue::Undefined,
                    writable: false,
                    getter,
                    setter,
                    enumerable: true,
                    configurable: true,
                };
                let defined = match &symbol_key {
                    Some(symbol) => self
                        .realm
                        .define_symbol_property(object, symbol, descriptor),
                    None => self.realm.define_property(object, key, descriptor),
                };
                if !defined {
                    return Err(JsError::type_error("could not define object accessor"));
                }
                continue;
            }
            if let Some(symbol) = symbol_key {
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
            if !self.realm.set_property(object, key, value) {
                return Err(JsError::type_error("could not define object property"));
            }
        }
        Ok(JsValue::Object(object))
    }

    pub(super) fn create_object_rest(
        &mut self,
        dom: &mut Dom,
        value: &JsValue,
        excluded: &[String],
    ) -> Result<ObjectId, JsError> {
        if matches!(value, JsValue::Null | JsValue::Undefined) {
            return Ok(self.realm.create_ordinary_object());
        }
        // Object rest starts with `CopyDataProperties`, whose first step is
        // `ToObject`, so a primitive rest source boxes into its wrapper.
        let source = self.to_object(value)?;
        let keys = self.realm.enumerable_own_keys(source).unwrap_or_default();
        // Reserve the result before reading the wrapper's keys so no collection
        // can tombstone the wrapper between the two steps.
        self.ensure_heap_capacity(1)?;
        let result = self.realm.create_ordinary_object();
        for key in keys {
            if excluded.contains(&key) {
                continue;
            }
            // `CopyDataProperties` reads each key with `Get`, so a getter on
            // the source runs and contributes its result. Copying the
            // descriptor's value slot instead silently dropped every accessor.
            let property_value = self.get_member(dom, source, &key)?;
            if !self.realm.set_property(result, key, property_value) {
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
        // One slot per index: an elision leaves its slot empty, so the hole has
        // no property and reads through the prototype chain like any other.
        let mut slots: Vec<Option<JsValue>> = Vec::with_capacity(elements.len());
        for expression in elements {
            if matches!(expression, Expr::Elision) {
                slots.push(None);
                continue;
            }
            let mut produced = Vec::new();
            self.evaluate_argument(dom, expression, &mut produced)?;
            slots.extend(produced.into_iter().map(Some));
        }
        for (index, slot) in slots.iter().enumerate() {
            if let Some(value) = slot
                && !self
                    .realm
                    .set_property(object, index.to_string(), value.clone())
            {
                return Err(JsError::type_error("could not define array element"));
            }
        }
        let length = u32::try_from(slots.len())
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
        &mut self,
        dom: &mut Dom,
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
                    JsValue::BigInt(_) => "bigint",
                    JsValue::String(_) => "string",
                    JsValue::Symbol(_) => "symbol",
                }
                .to_owned(),
            )),
            // Unary `+` is `ToNumber`, which throws for a BigInt; `-` and `~`
            // are `ToNumeric` and keep a BigInt operand a BigInt.
            UnaryOp::Plus => Ok(JsValue::Number(self.to_number_value(dom, value)?)),
            UnaryOp::Minus => match self.to_numeric_value(dom, value)? {
                JsValue::BigInt(number) => Ok(JsValue::BigInt(number.negate())),
                primitive => Ok(JsValue::Number(-to_number(&primitive)?)),
            },
            UnaryOp::BitwiseNot => match self.to_numeric_value(dom, value)? {
                JsValue::BigInt(number) => Ok(JsValue::BigInt(number.bit_not())),
                primitive => Ok(JsValue::Number(f64::from(!to_int32(&primitive)?))),
            },
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
        if operator == BinaryOp::Nullish && !matches!(left, JsValue::Null | JsValue::Undefined) {
            return Ok(left);
        }
        let right = self.evaluate(dom, right)?;
        // Logical operators return one of their original operands. Applying
        // ToPrimitive here changes objects such as `globalThis` into
        // "[object Object]", which breaks feature detection patterns like
        // `typeof globalThis !== "undefined" && globalThis`.
        if matches!(
            operator,
            BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Nullish
        ) {
            return Ok(right);
        }
        if operator == BinaryOp::Instanceof {
            return self.instanceof(dom, &left, &right).map(JsValue::Boolean);
        }
        if operator == BinaryOp::In {
            return self.property_in(dom, &left, &right).map(JsValue::Boolean);
        }
        self.binary_operation(dom, operator, left, right)
    }

    /// The value of `left op right` once both operands are evaluated. Binary
    /// expressions and compound assignment both come here, so `x op= y` and
    /// `x = x op y` cannot disagree. `+` converts with the default hint, as
    /// ECMA-262 13.15.3 requires, the relational operators with the number hint
    /// (13.10.1), and the other operators with `ToNumeric`.
    pub(super) fn binary_operation(
        &mut self,
        dom: &mut Dom,
        operator: BinaryOp,
        left: JsValue,
        right: JsValue,
    ) -> Result<JsValue, JsError> {
        match operator {
            BinaryOp::StrictEqual => return Ok(JsValue::Boolean(strict_equal(&left, &right))),
            BinaryOp::StrictNotEqual => {
                return Ok(JsValue::Boolean(!strict_equal(&left, &right)));
            }
            BinaryOp::Equal => return Ok(JsValue::Boolean(self.loose_equal(dom, &left, &right)?)),
            BinaryOp::NotEqual => {
                return Ok(JsValue::Boolean(!self.loose_equal(dom, &left, &right)?));
            }
            BinaryOp::Instanceof => {
                return self.instanceof(dom, &left, &right).map(JsValue::Boolean);
            }
            BinaryOp::In => return self.property_in(dom, &left, &right).map(JsValue::Boolean),
            BinaryOp::LogicalAnd | BinaryOp::LogicalOr | BinaryOp::Nullish => return Ok(right),
            BinaryOp::TemplateConcat => {
                let text = self.template_substitution_text(dom, &right)?;
                let mut units = utf16::utf16_units(&left.to_js_string());
                units.extend(utf16::utf16_units(&text));
                return Ok(JsValue::String(utf16::string_from_utf16(&units)));
            }
            BinaryOp::Less | BinaryOp::LessEqual | BinaryOp::Greater | BinaryOp::GreaterEqual => {
                let left = self.to_primitive_with_hint(dom, left, PrimitiveHint::Number)?;
                let right = self.to_primitive_with_hint(dom, right, PrimitiveHint::Number)?;
                return relational_compare(operator, &left, &right).map(JsValue::Boolean);
            }
            _ => {}
        }
        let (left, right) = if operator == BinaryOp::Add {
            let left = self.to_primitive_with_hint(dom, left, PrimitiveHint::Default)?;
            let right = self.to_primitive_with_hint(dom, right, PrimitiveHint::Default)?;
            if matches!(left, JsValue::String(_)) || matches!(right, JsValue::String(_)) {
                // StringAdd converts both operands with ToString, which throws
                // for a Symbol.
                if matches!(left, JsValue::Symbol(_)) || matches!(right, JsValue::Symbol(_)) {
                    return Err(JsError::type_error(
                        "Cannot convert a Symbol value to a string",
                    ));
                }
                // `+` on strings is §13.15.2 `StringAdd`, which concatenates
                // two code-unit sequences. The engine holds a lone surrogate
                // as a private-use placeholder, so a plain `format!` would
                // keep two halves of a pair as two separate placeholders and
                // `'\uD83D' + '\uDE00'` would not equal `'\u{1F600}'`.
                // Decoding through the code units re-joins them, which is
                // what makes `charAt(0) + charAt(1)` and
                // `String.fromCharCode(0xD83D) + String.fromCharCode(0xDE00)`
                // both answer the original pair.
                let mut units = utf16::utf16_units(&left.to_js_string());
                units.extend(utf16::utf16_units(&right.to_js_string()));
                return Ok(JsValue::String(utf16::string_from_utf16(&units)));
            }
            (left, right)
        } else {
            (left, right)
        };
        let left = self.to_numeric_value(dom, &left)?;
        let right = self.to_numeric_value(dom, &right)?;
        self.numeric_operation(operator, &left, &right)
    }

    /// The numeric operators on two `ToNumeric` results. A `BigInt` operates
    /// only with another `BigInt` (ECMA-262 6.1.6.2), so the mixed pairing is a
    /// `TypeError` rather than a silent conversion.
    fn numeric_operation(
        &mut self,
        operator: BinaryOp,
        left: &JsValue,
        right: &JsValue,
    ) -> Result<JsValue, JsError> {
        match (left, right) {
            (JsValue::BigInt(left), JsValue::BigInt(right)) => {
                return self.bigint_binary_operation(operator, left, right);
            }
            (JsValue::BigInt(_), _) | (_, JsValue::BigInt(_)) => {
                return Err(JsError::type_error(
                    "Cannot mix BigInt and other types, use explicit conversions",
                ));
            }
            _ => {}
        }
        match operator {
            BinaryOp::Add => Ok(JsValue::Number(to_number(left)? + to_number(right)?)),
            BinaryOp::Subtract => Ok(JsValue::Number(to_number(left)? - to_number(right)?)),
            BinaryOp::Multiply => Ok(JsValue::Number(to_number(left)? * to_number(right)?)),
            BinaryOp::Exponentiate => Ok(JsValue::Number(number_exponentiate(
                to_number(left)?,
                to_number(right)?,
            ))),
            BinaryOp::Divide => Ok(JsValue::Number(to_number(left)? / to_number(right)?)),
            BinaryOp::Remainder => Ok(JsValue::Number(to_number(left)? % to_number(right)?)),
            BinaryOp::BitwiseAnd => bitwise_binary(left, right, |left, right| left & right),
            BinaryOp::BitwiseXor => bitwise_binary(left, right, |left, right| left ^ right),
            BinaryOp::BitwiseOr => bitwise_binary(left, right, |left, right| left | right),
            BinaryOp::LeftShift => shift_left(left, right),
            BinaryOp::RightShift => shift_right(left, right),
            BinaryOp::UnsignedRightShift => unsigned_shift_right(left, right),
            _ => unreachable!("operator is handled before numeric conversion"),
        }
    }

    /// ECMA-262 7.2.14 `IsLooselyEqual`. An object compared with a string, number,
    /// boolean, bigint or symbol is converted with `ToPrimitive` (default hint), so
    /// `valueOf` and `toString` take part. An object is never loosely equal to
    /// `null` or `undefined`.
    pub(super) fn loose_equal(
        &mut self,
        dom: &mut Dom,
        left: &JsValue,
        right: &JsValue,
    ) -> Result<bool, JsError> {
        match (left, right) {
            (JsValue::Object(_), JsValue::Object(_)) => abstract_equal(left, right),
            (JsValue::Object(_), JsValue::Null | JsValue::Undefined)
            | (JsValue::Null | JsValue::Undefined, JsValue::Object(_)) => Ok(false),
            (JsValue::Object(_), _) => {
                let primitive =
                    self.to_primitive_with_hint(dom, left.clone(), PrimitiveHint::Default)?;
                self.loose_equal(dom, &primitive, right)
            }
            (_, JsValue::Object(_)) => {
                let primitive =
                    self.to_primitive_with_hint(dom, right.clone(), PrimitiveHint::Default)?;
                self.loose_equal(dom, left, &primitive)
            }
            _ => abstract_equal(left, right),
        }
    }

    /// The `in` operator: property existence on objects, index bounds on
    /// strings.
    pub(super) fn property_in(
        &mut self,
        dom: &mut Dom,
        key: &JsValue,
        container: &JsValue,
    ) -> Result<bool, JsError> {
        // ToPropertyKey runs only once the right-hand side is known to be an
        // object (ECMA-262 13.10.1), so a TypeError comes before any `toString`.
        let converted;
        let key = if matches!(container, JsValue::Object(_)) {
            converted = self.to_property_key_value(dom, key.clone())?;
            &converted
        } else {
            key
        };
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
                if matches!(self.realm.host(*object), Some(ObjectHost::Proxy { .. })) {
                    return self.proxy_has(dom, *object, &name);
                }
                if self.realm.get_property(*object, &name).is_some() {
                    return Ok(true);
                }
                if let Some(ObjectHost::StringPrimitive(text)) = self.realm.host(*object) {
                    // A String exotic object's indexed properties are numbered
                    // in code units, so `2 in '\u{1F600}'` is false and
                    // `1 in '\u{1F600}'` is true.
                    if let Ok(index) = name.parse::<usize>() {
                        return Ok(index < utf16::utf16_length(&text));
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
            // Global `var` declarations are allowed to coexist with an
            // existing Window property. The declaration itself must not
            // overwrite hosts such as the read-only `window.parent`.
            let existing_window_property = kind == VariableKind::Var
                && self
                    .realm
                    .own_property(self.realm.global_object(), name)
                    .is_some();
            if !existing_window_property && !self.realm.set_global(name.to_owned(), value) {
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
        dom: &mut Dom,
        name: &str,
        value: JsValue,
        kind: VariableKind,
    ) -> Result<(), JsError> {
        // `var x = v` is a PutValue on the resolved reference (ECMA-262
        // 14.3.2.1), so inside a `with` body it writes the binding object.
        if kind == VariableKind::Var
            && let Some(depth) = self.resolve_name(dom, name)?
            && let Some(object) = self.with_object_at(depth)
        {
            return self.set_with_binding(dom, object, name, value);
        }
        self.initialize_declared_binding(name, value, kind)
    }

    /// Initialize the innermost binding of `name` without consulting `with`
    /// objects. Hoisted declarations use this: they are created before any
    /// `with` body inside their scope runs.
    pub(super) fn initialize_declared_binding(
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
            if kind == VariableKind::Var {
                // A sloppy global `var` initializer assignment to a
                // read-only Window property fails silently in browsers.
                return Ok(());
            }
            return Err(JsError::type_error(format!(
                "global property {name:?} is not writable"
            )));
        }
        Err(JsError::reference(format!("{name} is not defined")))
    }

    /// Resolve `name` through the environment chain (ECMA-262 9.1.2.1
    /// `GetIdentifierReference`). The result is the index in `self.environment`
    /// of the innermost record that binds `name`, or `None` when only the
    /// global environment can bind it.
    pub(super) fn resolve_name(
        &mut self,
        dom: &mut Dom,
        name: &str,
    ) -> Result<Option<usize>, JsError> {
        let mut depth = self.environment.len();
        while depth > 0 {
            depth -= 1;
            let (declares, with_object) = {
                let scope = self.environment[depth].borrow();
                (
                    scope.bindings.contains_key(name) || scope.imports.contains_key(name),
                    scope.with_object,
                )
            };
            if declares {
                return Ok(Some(depth));
            }
            if let Some(object) = with_object
                && self.with_object_has_binding(dom, object, name)?
            {
                return Ok(Some(depth));
            }
        }
        Ok(None)
    }

    /// The binding object of the `with` record at `depth`, if it is one.
    fn with_object_at(&self, depth: usize) -> Option<ObjectId> {
        self.environment
            .get(depth)
            .and_then(|scope| scope.borrow().with_object)
    }

    /// `HasBinding` of an object environment record with `withEnvironment`
    /// (ECMA-262 9.1.1.2.1): the property exists and `@@unscopables` does not
    /// block it.
    fn with_object_has_binding(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        name: &str,
    ) -> Result<bool, JsError> {
        if !self.property_in(
            dom,
            &JsValue::String(name.to_owned()),
            &JsValue::Object(object),
        )? {
            return Ok(false);
        }
        let unscopables =
            self.get_symbol_value(dom, object, &JsSymbol::well_known("@@unscopables"))?;
        if let JsValue::Object(unscopables) = unscopables {
            return Ok(!self.get_member(dom, unscopables, name)?.is_truthy());
        }
        Ok(true)
    }

    /// `SetMutableBinding` of an object environment record (ECMA-262 9.1.1.2.5).
    /// Sloppy code never throws for a property that has since gone, but the
    /// existence check is still performed, as the specification does.
    fn set_with_binding(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        name: &str,
        value: JsValue,
    ) -> Result<(), JsError> {
        let _still_exists = self.property_in(
            dom,
            &JsValue::String(name.to_owned()),
            &JsValue::Object(object),
        )?;
        self.set_member(dom, object, name, value)
    }

    /// `GetValue` of an identifier reference resolved by [`Self::resolve_name`];
    /// `None` reads the global environment.
    pub(super) fn read_resolved_binding(
        &mut self,
        dom: &mut Dom,
        scope: Option<usize>,
        name: &str,
    ) -> Result<JsValue, JsError> {
        let Some(depth) = scope else {
            return self.read_global_binding(name);
        };
        if let Some(object) = self.with_object_at(depth) {
            // GetBindingValue of an object environment record (ECMA-262
            // 9.1.1.2.6): a property that has gone reads as `undefined`.
            if !self.property_in(
                dom,
                &JsValue::String(name.to_owned()),
                &JsValue::Object(object),
            )? {
                return Ok(JsValue::Undefined);
            }
            return self.get_member(dom, object, name);
        }
        let environment = self.environment[depth].borrow();
        if let Some(binding) = environment.bindings.get(name) {
            if !binding.initialized {
                return Err(JsError::reference(format!(
                    "cannot access {name} before initialization"
                )));
            }
            return Ok(binding.value.clone());
        }
        let import = environment.imports.get(name).cloned();
        drop(environment);
        match import {
            Some(import) => self.read_import(&import),
            None => unreachable!("resolution found a binding or import in this record"),
        }
    }

    fn read_global_binding(&self, name: &str) -> Result<JsValue, JsError> {
        if let Some(binding) = self.global_bindings.get(name)
            && !binding.initialized
        {
            return Err(JsError::reference(format!(
                "cannot access {name} before initialization"
            )));
        }
        if let Some(value) = match name {
            "innerWidth" | "outerWidth" => Some(self.viewport.width),
            "innerHeight" | "outerHeight" => Some(self.viewport.height),
            _ => None,
        } {
            return Ok(JsValue::Number(f64::from(value)));
        }
        self.realm
            .global(name)
            .ok_or_else(|| JsError::reference(format!("{name} is not defined")))
    }

    pub(super) fn lookup_binding(&mut self, dom: &mut Dom, name: &str) -> Result<JsValue, JsError> {
        let scope = self.resolve_name(dom, name)?;
        self.read_resolved_binding(dom, scope, name)
    }

    /// `PutValue` of an identifier reference resolved by [`Self::resolve_name`];
    /// `None` writes the global environment.
    pub(super) fn write_resolved_binding(
        &mut self,
        dom: &mut Dom,
        scope: Option<usize>,
        name: &str,
        value: JsValue,
    ) -> Result<(), JsError> {
        let Some(depth) = scope else {
            return self.write_global_binding(name, value);
        };
        if let Some(object) = self.with_object_at(depth) {
            return self.set_with_binding(dom, object, name, value);
        }
        let mut environment = self.environment[depth].borrow_mut();
        if let Some(binding) = environment.bindings.get_mut(name) {
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
        // Only an import binding is left, and imports are immutable.
        Err(JsError::type_error(format!(
            "assignment to constant binding {name:?}"
        )))
    }

    fn write_global_binding(&mut self, name: &str, value: JsValue) -> Result<(), JsError> {
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

    pub(super) fn assign_binding(
        &mut self,
        dom: &mut Dom,
        name: &str,
        value: JsValue,
    ) -> Result<(), JsError> {
        let scope = self.resolve_name(dom, name)?;
        self.write_resolved_binding(dom, scope, name, value)
    }

    /// Whether `name` is bound in the global environment (its global
    /// bindings or the global object).
    fn global_name_exists(&self, name: &str) -> bool {
        self.global_bindings.contains_key(name) || self.realm.global(name).is_some()
    }

    /// `with (object) body` (ECMA-262 14.11.2): the body runs in an object
    /// environment record over `ToObject(object)`.
    pub(super) fn evaluate_with_statement(
        &mut self,
        dom: &mut Dom,
        object: &Expr,
        body: &Statement,
    ) -> Result<Completion, JsError> {
        let value = self.evaluate(dom, object)?;
        let object = self.to_object(&value)?;
        self.environment
            .push(Rc::new(RefCell::new(EnvironmentRecord {
                with_object: Some(object),
                ..EnvironmentRecord::default()
            })));
        let result = self.evaluate_statement(dom, body);
        self.environment.pop();
        result
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn get_member(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        property: &str,
    ) -> Result<JsValue, JsError> {
        self.consume_step()?;
        if matches!(self.realm.host(object), Some(ObjectHost::Proxy { .. })) {
            return self.proxy_get(dom, object, property);
        }
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
                "nodeValue" | "data" => {
                    return Ok(match dom.node(node_id).map(render_dom::Node::kind) {
                        Some(NodeKind::Text(data) | NodeKind::Comment(data)) => {
                            JsValue::String(data.clone())
                        }
                        _ => JsValue::Null,
                    });
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
                        Some(node) => self.wrap_node(dom, node),
                        None => Ok(JsValue::Null),
                    };
                }
                "parentNode" | "parentElement" | "nextSibling" | "previousSibling"
                | "ownerDocument" => return Ok(JsValue::Null),
                "firstChild" | "lastChild" => {
                    let child = dom.children(document).and_then(|children| {
                        if property == "firstChild" {
                            children.first()
                        } else {
                            children.last()
                        }
                        .copied()
                    });
                    return match child {
                        Some(child) => self.wrap_node(dom, child),
                        None => Ok(JsValue::Null),
                    };
                }
                "childNodes" | "children" => {
                    let elements_only = property == "children";
                    let values = dom
                        .children(document)
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
                        .map(|child| self.wrap_node(dom, child))
                        .collect::<Result<Vec<_>, _>>()?;
                    return Ok(JsValue::Object(self.create_array_from_values(&values)?));
                }
                "readyState" => {
                    return Ok(JsValue::String(self.ready_state.as_str().to_owned()));
                }
                "cookie" => {
                    return Ok(JsValue::String(self.js_cookie_jar_serialize()));
                }
                // The embedding window is the realm's global object.
                "defaultView" | "parentWindow" => {
                    return Ok(JsValue::Object(self.realm.global_object()));
                }
                "activeElement" => {
                    return match self.find_element_by_tag(dom, document, "body")? {
                        Some(node) => self.wrap_node(dom, node),
                        None => Ok(JsValue::Null),
                    };
                }
                _ => {}
            },
            Some(ObjectHost::Blob {
                bytes,
                content_type,
            }) => match property {
                "size" => {
                    return Ok(JsValue::Number(bytes.len() as f64));
                }
                "type" => return Ok(JsValue::String(content_type.clone())),
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
                        Some(parent) => self.wrap_node(dom, parent),
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
                        Some(related) => self.wrap_node(dom, related),
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
                        .map(|child| self.wrap_node(dom, child))
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
                            Ok(JsValue::Object(
                                self.realm.attr_wrapper(node, attribute.local_name.clone()),
                            ))
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
                if let Some(value) = JsRuntime::dataset_member_attribute_value(dom, node, property)?
                {
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
                // §10.4.5.4 [[Get]]: a canonical numeric key is an element read,
                // and one that is not a valid integer index reads as undefined
                // without consulting the prototype chain. Any other key is a
                // method or property access.
                if let Some(number) = crate::value::canonical_numeric_key(property) {
                    #[allow(
                        clippy::cast_precision_loss,
                        reason = "typed-array lengths stay far below any precision boundary"
                    )]
                    let in_range = number.is_sign_positive()
                        && number.fract() == 0.0
                        && number < length as f64;
                    let element = if in_range {
                        #[allow(
                            clippy::cast_possible_truncation,
                            clippy::cast_sign_loss,
                            reason = "the index is a non-negative integer below the length"
                        )]
                        let index = number as usize;
                        buffer.element_value(kind, start + index)
                    } else {
                        None
                    };
                    return Ok(element.unwrap_or(JsValue::Undefined));
                }
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
            // Web Storage: `length` is the live entry count and the numeric
            // slots are the stored values, both provided by the area's own
            // property table (WHATWG HTML §11.2.3).
            Some(ObjectHost::Storage) => {
                if property == "length" {
                    #[allow(
                        clippy::cast_precision_loss,
                        reason = "entry counts stay far below any precision boundary"
                    )]
                    let length = self
                        .realm
                        .own_property_names(object)
                        .map_or(0, |keys| keys.len());
                    return Ok(JsValue::Number(length as f64));
                }
                if property.parse::<usize>().is_ok() {
                    return Ok(self
                        .realm
                        .own_property(object, property)
                        .map_or(JsValue::Null, |descriptor| descriptor.value));
                }
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
            (Some(ObjectHost::Document(_)), "createComment") => Some(NativeFunction::CreateComment),
            (Some(ObjectHost::Document(_)), "createDocumentFragment") => {
                Some(NativeFunction::CreateDocumentFragment)
            }
            (Some(ObjectHost::Document(_)), "createEvent") => Some(NativeFunction::CreateEvent),
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
            (Some(ObjectHost::IteratorHelper { .. }), "next") => {
                Some(NativeFunction::IteratorHelperNext)
            }
            (Some(ObjectHost::IteratorHelper { .. }), "return") => {
                Some(NativeFunction::IteratorHelperReturn)
            }
            (Some(ObjectHost::StringPrimitive(_)), name) => string_method_native(name),
            (Some(ObjectHost::Blob { .. }), "text") => Some(NativeFunction::BlobText),
            (Some(ObjectHost::Blob { .. }), "arrayBuffer") => Some(NativeFunction::BlobArrayBuffer),
            (Some(ObjectHost::Blob { .. }), "slice") => Some(NativeFunction::BlobSlice),
            (Some(ObjectHost::UrlConstructor), "createObjectURL") => {
                Some(NativeFunction::UrlCreateObjectUrl)
            }
            (Some(ObjectHost::UrlConstructor), "revokeObjectURL") => {
                Some(NativeFunction::UrlRevokeObjectUrl)
            }
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

    /// Writes the engine performs natively on a node (`innerHTML`, `id`,
    /// `className`, ...) are where custom element reactions have to be
    /// observed. The prelude installs [`CUSTOM_ELEMENT_REACTION`] with the
    /// first `customElements.define`, so a page without one pays one lookup.
    pub(super) fn set_member(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        property: &str,
        value: JsValue,
    ) -> Result<(), JsError> {
        let reacts = matches!(self.realm.host(object), Some(ObjectHost::Node(_)))
            && (matches!(property, "innerHTML" | "outerHTML" | "textContent")
                || node_attribute_property(property).is_some());
        let hook = match self.realm.global(CUSTOM_ELEMENT_REACTION) {
            Some(JsValue::Object(hook)) if reacts => hook,
            _ => return self.set_member_native(dom, object, property, value),
        };
        let property_name = JsValue::String(property.to_owned());
        let after = self.call(dom, hook, &[JsValue::Object(object), property_name])?;
        self.set_member_native(dom, object, property, value)?;
        if let JsValue::Object(after) = after {
            self.call(dom, after, &[])?;
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "host-object write paths each need their own arm"
    )]
    fn set_member_native(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        property: &str,
        value: JsValue,
    ) -> Result<(), JsError> {
        self.consume_step()?;
        if matches!(self.realm.host(object), Some(ObjectHost::Proxy { .. })) {
            return self.proxy_set(dom, object, property, value);
        }
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
                    return self.set_array_length_value(dom, object, &value);
                }
                if let Some(index) = array_index(property) {
                    // ECMA-262 10.1.9.2: an accessor at this index, own or
                    // inherited, runs its setter instead of storing a value; one
                    // without a setter refuses the write.
                    if let Some(accessor) = self
                        .realm
                        .get_descriptor(object, property)
                        .filter(crate::value::PropertyDescriptor::is_accessor)
                    {
                        if accessor.setter.is_none() {
                            return Err(JsError::type_error(format!(
                                "property {property:?} is not writable"
                            )));
                        }
                        return self.set_value(dom, object, property, value);
                    }
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
                    // Only a valid index converts and stores. This path does not
                    // know the receiver, and §10.4.5.5 converts an out-of-range
                    // value only when the receiver is the typed array itself.
                    if index < length {
                        if kind.is_bigint() {
                            let bigint = self.to_bigint_value(dom, &value)?;
                            buffer.set_bigint_element(kind, start + index, &bigint);
                        } else {
                            let number = to_number(&value)?;
                            buffer.set_element(kind, start + index, number);
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
                return self.set_dataset_member(dom, node, property, &value);
            }
            // String exotic objects are ordinary apart from their virtual
            // indexed characters and `length`; writes to those are ignored,
            // but ordinary properties must be stored.
            Some(ObjectHost::StringPrimitive(text)) => {
                if property == "length" {
                    return Ok(());
                }
                if let Ok(index) = property.parse::<usize>()
                    && index < utf16::utf16_length(&text)
                {
                    return Ok(());
                }
            }
            // `length` is read-only; a numeric slot writes the entry.
            Some(ObjectHost::Storage) => {
                if property == "length" {
                    return Ok(());
                }
                if property.parse::<usize>().is_ok() {
                    return if self.realm.set_property(
                        object,
                        property.to_owned(),
                        JsValue::String(value.to_js_string()),
                    ) {
                        Ok(())
                    } else {
                        Err(JsError::type_error("could not store the value"))
                    };
                }
            }
            _ => {}
        }
        self.set_value(dom, object, property, value)
    }

    /// `IsConstructor` (§7.2.5): mirrors the constructor arms of
    /// `construct_dispatch` so callers such as `Reflect.construct` reject
    /// non-constructors with a `TypeError` before dispatching.
    pub(super) fn is_constructor(&self, object: ObjectId) -> bool {
        match self.realm.host(object) {
            Some(
                ObjectHost::ObjectConstructor
                | ObjectHost::ArrayConstructor
                | ObjectHost::StringConstructor
                | ObjectHost::NumberConstructor
                // `BigInt` has [[Construct]] and throws from it (21.2.1.1),
                // so it can be extended; a plain `new` still fails.
                | ObjectHost::BigIntConstructor
                | ObjectHost::BooleanConstructor
                | ObjectHost::FunctionConstructor
                | ObjectHost::DateConstructor
                | ObjectHost::ErrorConstructor(_)
                | ObjectHost::AggregateErrorConstructor
                | ObjectHost::PromiseConstructor
                | ObjectHost::EventConstructor
                | ObjectHost::DomConstructor
                | ObjectHost::ImageConstructor
                | ObjectHost::VideoConstructor
                | ObjectHost::XmlHttpRequestConstructor
                | ObjectHost::AbortControllerConstructor
                | ObjectHost::FormDataConstructor
                | ObjectHost::ResponseConstructor
                | ObjectHost::BlobConstructor
                | ObjectHost::ProxyConstructor
                | ObjectHost::IntersectionObserverConstructor
                | ObjectHost::MutationObserverConstructor
                | ObjectHost::CollectionConstructor(_)
                | ObjectHost::TypedArrayConstructor(_)
                | ObjectHost::RegExpConstructor
                | ObjectHost::UrlConstructor
                | ObjectHost::UrlSearchParamsConstructor
                // ArrayBuffer, DataView and the text codecs are constructors (ECMA-262 25.1.4.1, 25.2.2.1).
                | ObjectHost::ArrayBufferConstructor
                | ObjectHost::DataViewConstructor
                | ObjectHost::TextEncoderConstructor
                | ObjectHost::TextDecoderConstructor,
            ) => true,
            // Iterator is an abstract constructor: `new Iterator()` throws,
            // but a subclass's `super()` constructs through it.
            Some(ObjectHost::NativeFunction(NativeFunction::IteratorConstructor)) => true,
            // Only ordinary functions and class constructors have [[Construct]];
            // generators and async functions do not (ECMA-262 27.3.1, 27.7.1, 15.8.1).
            Some(ObjectHost::UserFunction(index)) => {
                self.functions.get(index).is_none_or(|function| {
                    function.kind == FunctionKind::Normal
                        && !matches!(&function.class, Some(class) if !class.constructor)
                })
            }
            // A bound function is constructable exactly when its target is:
            // `IsConstructor` looks through the [[BoundFunction]] wrapper.
            Some(ObjectHost::BoundCallable { target, .. }) => self.is_constructor(target),
            _ => false,
        }
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

    /// ECMA-262 20.2.1.1 `Function(...parameterArgs, bodyArg)`: build the
    /// corresponding function expression, parse it, and evaluate it in the
    /// global scope (the constructor never closes over the caller's locals).
    fn function_constructor(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let mut parts = Vec::with_capacity(arguments.len());
        for argument in arguments {
            parts.push(self.to_string_value(dom, argument)?);
        }
        let body = parts.pop().unwrap_or_default();
        let parameters = parts.join(",");
        let source = format!("(function anonymous({parameters}\n) {{\n{body}\n}})");
        self.evaluate_function_source(dom, &source)
    }

    /// Evaluate `source` in the global scope, which must produce a function
    /// object. The `Function` constructor and `Array.fromAsync` both build their
    /// functions this way, so the closure never sees the caller's locals.
    pub(in crate::runtime) fn evaluate_function_source(
        &mut self,
        dom: &mut Dom,
        source: &str,
    ) -> Result<JsValue, JsError> {
        let script = crate::CompiledScript::compile(source, &self.limits)?;
        let environment = std::mem::take(&mut self.environment);
        let result = (|| {
            self.instantiate_statements(&script.statements)?;
            match self.evaluate_statements(dom, &script.statements)? {
                Completion::Normal(value @ JsValue::Object(_)) => Ok(value),
                _ => Err(JsError::syntax(
                    "function source did not produce a function",
                    0,
                )),
            }
        })();
        self.environment = environment;
        result
    }

    /// ECMA-262 22.2.4.1 `RegExp(pattern, flags)`: no arguments yields an
    /// empty pattern, an existing `RegExp` supplies its source and flags, and
    /// an explicit flags argument recompiles.
    fn regexp_constructor_value(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
        called_without_new: bool,
    ) -> Result<JsValue, JsError> {
        let pattern = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let flags = arguments.get(1).cloned().unwrap_or(JsValue::Undefined);
        if let JsValue::Object(object) = &pattern
            && let Some(ObjectHost::RegExp(index)) = self.realm.host(*object)
        {
            if called_without_new
                && matches!(flags, JsValue::Undefined)
                && let Some(JsValue::Object(constructor)) = self.realm.global("RegExp")
                && matches!(
                    self.get_value(dom, *object, "constructor")?,
                    JsValue::Object(pattern_constructor) if pattern_constructor == constructor
                )
            {
                return Ok(pattern.clone());
            }
            let source = self.regexes[index].compiled.source().to_owned();
            let flags_text = match &flags {
                JsValue::Undefined => self.regexes[index].compiled.flags().describe(),
                value => self.to_string_value(dom, value)?,
            };
            let object = self.construct_regex(&source, &flags_text)?;
            return Ok(JsValue::Object(object));
        }
        let pattern_text = match &pattern {
            JsValue::Undefined => String::new(),
            value => self.to_string_value(dom, value)?,
        };
        let flags_text = match &flags {
            JsValue::Undefined => String::new(),
            value => self.to_string_value(dom, value)?,
        };
        let object = self.construct_regex(&pattern_text, &flags_text)?;
        Ok(JsValue::Object(object))
    }

    /// The `new.target` a constructor runs with: itself, except when it is the
    /// parent of a `super()` call, where it inherits the derived class's.
    fn dispatch_new_target(&mut self, constructor: ObjectId) -> JsValue {
        if std::mem::take(&mut self.super_call_pending)
            && let Some(top) = self.new_target_stack.last().cloned()
        {
            return top;
        }
        JsValue::Object(constructor)
    }

    pub(super) fn construct_dispatch(
        &mut self,
        dom: &mut Dom,
        constructor: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        // `new.target` supplies the instance prototype: a `super()` call from
        // a derived constructor must create an object whose prototype is the
        // derived class's prototype, not the parent's.
        let new_target = self
            .new_target_stack
            .last()
            .cloned()
            .unwrap_or(JsValue::Object(constructor));
        let instance_prototype = |runtime: &mut Self| -> Option<ObjectId> {
            let target = match &new_target {
                JsValue::Object(target) => Some(*target),
                _ => None,
            }
            .unwrap_or(constructor);
            runtime
                .realm
                .get_property(target, "prototype")
                .and_then(|value| match value {
                    JsValue::Object(prototype) => Some(prototype),
                    _ => None,
                })
        };
        match self.realm.host(constructor) {
            Some(ObjectHost::ObjectConstructor) => self.object_constructor(arguments),
            Some(ObjectHost::ArrayConstructor) => self.array_constructor(arguments),
            Some(ObjectHost::StringConstructor) => {
                let text = match arguments.first() {
                    None => String::new(),
                    Some(value) => self.to_string_value(dom, value)?,
                };
                self.ensure_heap_capacity(1)?;
                Ok(JsValue::Object(self.realm.string_wrapper(text)))
            }
            Some(ObjectHost::NumberConstructor) => {
                let number = match arguments.first() {
                    None | Some(JsValue::Undefined) => 0.0,
                    Some(value) => self.number_conversion(dom, value)?,
                };
                self.ensure_heap_capacity(1)?;
                Ok(JsValue::Object(self.realm.number_primitive_wrapper(number)))
            }
            Some(ObjectHost::BooleanConstructor) => {
                // ECMA-262 20.3.1.1: an absent value is `undefined`, which is falsy.
                let value = arguments.first().is_some_and(JsValue::is_truthy);
                self.ensure_heap_capacity(1)?;
                Ok(JsValue::Object(self.realm.boolean_primitive_wrapper(value)))
            }
            Some(ObjectHost::FunctionConstructor) => self.function_constructor(dom, arguments),
            Some(ObjectHost::DateConstructor) => {
                let ms = self.date_from_constructor_arguments(dom, arguments)?;
                self.ensure_heap_capacity(1)?;
                Ok(JsValue::Object(self.realm.date_wrapper(ms)))
            }
            Some(ObjectHost::ErrorConstructor(kind)) => {
                self.error_constructor(constructor, kind, arguments)
            }
            Some(ObjectHost::DomExceptionConstructor) => {
                self.dom_exception_constructor(constructor, arguments)
            }
            // Like every `Error` subclass, `AggregateError` needs `new`.
            Some(ObjectHost::AggregateErrorConstructor) => {
                self.aggregate_error_constructor(dom, constructor, arguments)
            }
            Some(ObjectHost::PromiseConstructor) => self.construct_promise(dom, arguments),
            Some(ObjectHost::EventConstructor) => self.event_constructor(arguments),
            Some(ObjectHost::DomNodeConstructor(kind)) => {
                self.construct_dom_node(dom, kind, arguments)
            }
            Some(ObjectHost::DomConstructor) => Err(JsError::type_error("Illegal constructor")),
            Some(ObjectHost::ImageConstructor) => self.image_constructor(dom, arguments),
            Some(ObjectHost::VideoConstructor) => self.video_constructor(constructor, arguments),
            Some(ObjectHost::XmlHttpRequestConstructor) => {
                self.xml_http_request_constructor(constructor)
            }
            Some(ObjectHost::AbortControllerConstructor) => {
                self.abort_controller_constructor(constructor)
            }
            Some(ObjectHost::FormDataConstructor) => self.form_data_constructor(constructor),
            Some(ObjectHost::TextEncoderConstructor) => self.text_encoder_constructor(constructor),
            Some(ObjectHost::TextDecoderConstructor) => {
                self.text_decoder_constructor(constructor, arguments)
            }
            Some(ObjectHost::DataViewConstructor) => {
                self.data_view_constructor(dom, constructor, arguments)
            }
            Some(ObjectHost::ArrayBufferConstructor) => {
                self.array_buffer_constructor(dom, constructor, arguments)
            }
            Some(ObjectHost::ResponseConstructor) => {
                self.response_constructor(constructor, arguments)
            }
            Some(ObjectHost::BlobConstructor) => self.blob_constructor(dom, constructor, arguments),
            Some(ObjectHost::ProxyConstructor) => self.proxy_constructor(constructor, arguments),
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
                self.regexp_constructor_value(dom, arguments, false)
            }
            Some(ObjectHost::UrlConstructor) => self.url_constructor(constructor, arguments),
            Some(ObjectHost::UrlSearchParamsConstructor) => {
                self.url_search_params_constructor(constructor, arguments)
            }
            Some(ObjectHost::ArrowFunction(_)) => {
                Err(JsError::type_error("arrow function is not a constructor"))
            }
            Some(ObjectHost::NativeFunction(NativeFunction::IteratorConstructor)) => {
                // `new Iterator()` directly is abstract, but `super()` from a
                // subclass constructs an ordinary object with the subclass's
                // prototype, exactly like the spec's NewTarget check. The
                // nested-`new` scope push means a direct `new Iterator()`
                // always resolves new.target to Iterator itself.
                let new_target = self
                    .new_target_stack
                    .last()
                    .cloned()
                    .unwrap_or(JsValue::Object(constructor));
                if new_target == JsValue::Object(constructor) {
                    return Err(JsError::type_error("Iterator is abstract"));
                }
                self.ensure_heap_capacity(1)?;
                let prototype = instance_prototype(self);
                Ok(JsValue::Object(self.realm.create_object(prototype)))
            }
            Some(ObjectHost::UserFunction(index))
                if self
                    .functions
                    .get(index)
                    .is_some_and(|function| function.kind != FunctionKind::Normal) =>
            {
                Err(JsError::type_error(
                    "generator or async function is not a constructor",
                ))
            }
            Some(ObjectHost::UserFunction(index)) => {
                let class = self
                    .functions
                    .get(index)
                    .and_then(|function| function.class.clone());
                match class {
                    Some(class) if !class.constructor => {
                        Err(JsError::type_error("class method is not a constructor"))
                    }
                    Some(class) => {
                        let new_target = self.dispatch_new_target(constructor);
                        self.new_target_stack.push(new_target);
                        let result = if class.derived {
                            // A derived constructor receives `this` from its
                            // `super()` call; call_user returns the final
                            // `this` on normal completion.
                            let result = self.call_user(
                                dom,
                                constructor,
                                index,
                                arguments,
                                JsValue::Undefined,
                                true,
                            );
                            match result {
                                Ok(JsValue::Object(instance)) => Ok(JsValue::Object(instance)),
                                // ECMA-262 9.2.2 step 12: an undefined result returns
                                // GetThisBinding(), which throws while `this` is unset.
                                Ok(JsValue::Undefined) => Err(JsError::reference(
                                    "Must call super constructor in derived class before accessing 'this' or returning from derived constructor",
                                )),
                                Ok(_) => Err(JsError::type_error(
                                    "derived constructor did not return an object",
                                )),
                                Err(error) => Err(error),
                            }
                        } else {
                            self.ensure_heap_capacity(1)?;
                            let prototype = instance_prototype(self);
                            let instance = self.realm.create_object(prototype);
                            self.run_instance_fields(dom, &class, instance)?;
                            let result = self.call_user(
                                dom,
                                constructor,
                                index,
                                arguments,
                                JsValue::Object(instance),
                                true,
                            )?;
                            if matches!(result, JsValue::Object(_)) {
                                Ok(result)
                            } else {
                                Ok(JsValue::Object(instance))
                            }
                        };
                        self.new_target_stack.pop();
                        result
                    }
                    None => {
                        self.ensure_heap_capacity(1)?;
                        let prototype = instance_prototype(self);
                        let instance = self.realm.create_object(prototype);
                        let new_target = self.dispatch_new_target(constructor);
                        self.new_target_stack.push(new_target);
                        let result = self.call_user(
                            dom,
                            constructor,
                            index,
                            arguments,
                            JsValue::Object(instance),
                            true,
                        );
                        self.new_target_stack.pop();
                        let result = result?;
                        if matches!(result, JsValue::Object(_)) {
                            Ok(result)
                        } else {
                            Ok(JsValue::Object(instance))
                        }
                    }
                }
            }
            Some(ObjectHost::BoundCallable { .. }) => {
                self.construct_bound_callable(dom, constructor, arguments)
            }
            _ => Err(JsError::type_error("value is not a constructor")),
        }
    }

    /// ECMA-262 10.2.5.4 `BoundFunctionCreate` + `[[Construct]]`: a bound
    /// function forwards construction to its target with the bound receiver and
    /// the bound arguments prepended, and the wrapper itself supplies the
    /// `new.target` the target observes.
    fn construct_bound_callable(
        &mut self,
        dom: &mut Dom,
        constructor: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let Some(ObjectHost::BoundCallable {
            target,
            arguments: bound_arguments,
            ..
        }) = self.realm.host(constructor)
        else {
            return Err(JsError::type_error("value is not a constructor"));
        };
        if !self.is_constructor(target) {
            return Err(JsError::type_error("value is not a constructor"));
        }
        let mut combined = bound_arguments;
        combined.extend_from_slice(arguments);
        // §10.2.5.4: the wrapper is only transparent to `new.target` when it is
        // itself the new target, as in `new (C.bind(null, 8))()`. An explicit
        // `Reflect.construct(B, args, Other)` keeps `Other`.
        let new_target = self
            .new_target_stack
            .last()
            .filter(|value| **value != JsValue::Object(constructor))
            .cloned()
            .unwrap_or(JsValue::Object(target));
        self.new_target_stack.push(new_target);
        let constructed = self.construct_dispatch(dom, target, &combined);
        self.new_target_stack.pop();
        constructed
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
            Some(ObjectHost::FunctionConstructor) => self.function_constructor(dom, arguments),
            Some(ObjectHost::StringConstructor) => Ok(JsValue::String(match arguments.first() {
                None => String::new(),
                Some(value) => self.to_string_value(dom, value)?,
            })),
            Some(ObjectHost::NumberConstructor) => Ok(JsValue::Number(match arguments.first() {
                None | Some(JsValue::Undefined) => 0.0,
                Some(value) => self.number_conversion(dom, value)?,
            })),
            Some(ObjectHost::BigIntConstructor) => self.bigint_function(dom, arguments),
            Some(ObjectHost::BooleanConstructor) => Ok(JsValue::Boolean(
                arguments.first().is_some_and(JsValue::is_truthy),
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
            Some(ObjectHost::DomNodeConstructor(_)) => Err(JsError::type_error(
                "Failed to construct a DOM node: please use the 'new' operator",
            )),
            Some(ObjectHost::ImageConstructor) => self.image_constructor(dom, arguments),
            // Legacy web compatibility: `Video()` without `new` constructs,
            // exactly like `Image()`.
            Some(ObjectHost::VideoConstructor) => self.video_constructor(callee, arguments),
            Some(ObjectHost::XmlHttpRequestConstructor) => Err(JsError::type_error(
                "XMLHttpRequest constructor requires 'new'",
            )),
            Some(ObjectHost::AbortControllerConstructor) => Err(JsError::type_error(
                "AbortController constructor requires 'new'",
            )),
            Some(ObjectHost::FormDataConstructor) => {
                Err(JsError::type_error("FormData constructor requires 'new'"))
            }
            // §25.2.5 `DataView`, §10.2.4 `TextEncoder` and
            // §10.2.4.1 `TextDecoder` are all `new`-only.
            Some(ObjectHost::TextEncoderConstructor) => Err(JsError::type_error(
                "TextEncoder constructor requires 'new'",
            )),
            Some(ObjectHost::TextDecoderConstructor) => Err(JsError::type_error(
                "TextDecoder constructor requires 'new'",
            )),
            Some(ObjectHost::DataViewConstructor) => {
                Err(JsError::type_error("DataView constructor requires 'new'"))
            }
            Some(ObjectHost::ArrayBufferConstructor) => Err(JsError::type_error(
                "ArrayBuffer constructor requires 'new'",
            )),
            Some(ObjectHost::ResponseConstructor) => {
                Err(JsError::type_error("Response constructor requires 'new'"))
            }
            Some(ObjectHost::BlobConstructor) => self.blob_constructor(dom, callee, arguments),
            Some(ObjectHost::ProxyConstructor) => {
                Err(JsError::type_error("Proxy constructor requires 'new'"))
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
                // `RegExp(re)` called without `new` behaves like construction,
                // except that a same-realm pattern is returned unchanged.
                self.regexp_constructor_value(dom, arguments, true)
            }
            Some(ObjectHost::UrlConstructor) => self.url_constructor(callee, arguments),
            Some(ObjectHost::UrlSearchParamsConstructor) => {
                self.url_search_params_constructor(callee, arguments)
            }
            Some(ObjectHost::ErrorConstructor(kind)) => {
                self.error_constructor(callee, kind, arguments)
            }
            // WebIDL §4.4's interface object has a constructor operation, so
            // `DOMException("m", "NotFoundError")` without `new` constructs one
            // exactly as `new DOMException("m", "NotFoundError")` does.
            Some(ObjectHost::DomExceptionConstructor) => {
                self.dom_exception_constructor(callee, arguments)
            }
            // Annex B.2.2.1.2 `RequireObjectCoercible(this)` runs before the
            // argument, so a nullish receiver throws; a native call would
            // otherwise see the global object in its place.
            Some(ObjectHost::NativeFunction(NativeFunction::ObjectProtoSetter))
                if matches!(receiver, JsValue::Null | JsValue::Undefined) =>
            {
                Err(JsError::type_error(
                    "Object.prototype.__proto__ setter called on null or undefined",
                ))
            }
            Some(ObjectHost::NativeFunction(NativeFunction::ObjectPrototypeToString)) => {
                // `toString.call(primitive)` never materializes an object.
                Ok(JsValue::String(self.object_to_string_tag(dom, &receiver)?))
            }
            // §20.1.3.7 `Object.prototype.valueOf` is `ToObject(this)`: a
            // primitive yields its wrapper, and a nullish receiver throws.
            Some(ObjectHost::NativeFunction(NativeFunction::ObjectPrototypeValueOf)) => {
                Ok(JsValue::Object(self.to_object(&receiver)?))
            }
            Some(ObjectHost::NativeFunction(NativeFunction::FunctionPrototype)) => {
                Ok(JsValue::Undefined)
            }
            Some(ObjectHost::NativeFunction(NativeFunction::FunctionToString)) => {
                let function = Self::require_callable_object(&receiver, &self.realm)?;
                let name = self
                    .realm
                    .get_property(function, "name")
                    .map_or_else(String::new, |value| value.to_js_string());
                let native = !matches!(
                    self.realm.host(function),
                    Some(ObjectHost::UserFunction(_) | ObjectHost::ArrowFunction(_))
                );
                Ok(JsValue::String(if native {
                    format!("function {name}() {{ [native code] }}")
                } else {
                    format!("function {name}() {{ }}")
                }))
            }
            Some(ObjectHost::NativeFunction(NativeFunction::FunctionCall)) => {
                self.function_call(dom, &receiver, arguments)
            }
            Some(ObjectHost::NativeFunction(NativeFunction::FunctionApply)) => {
                // apply(thisArg, [args...])
                let callable = Self::require_callable_object(&receiver, &self.realm)?;
                let this_argument = arguments.first().cloned().unwrap_or(JsValue::Undefined);
                let call_arguments = match arguments.get(1) {
                    Some(JsValue::Object(array)) => self.array_elements_for(*array)?,
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
            // A built-in whose receiver is `RequireObjectCoercible`d gets the
            // nullish `this` as a `TypeError`, not the global object.
            Some(ObjectHost::NativeFunction(function))
                if matches!(receiver, JsValue::Null | JsValue::Undefined)
                    && function.requires_coercible_this() =>
            {
                Err(JsError::type_error(format!(
                    "{function:?} called on null or undefined"
                )))
            }
            Some(ObjectHost::NativeFunction(
                function @ (NativeFunction::RegExpSymbolMatch
                | NativeFunction::RegExpSymbolMatchAll
                | NativeFunction::RegExpSymbolReplace
                | NativeFunction::RegExpSymbolSearch
                | NativeFunction::RegExpSymbolSplit),
            )) => {
                // ECMA-262 22.2.6.8 step 2: `this` must be an Object, so the
                // receiver is passed unboxed. Pins follow `call_native`.
                let pinned = self.transient_roots.len();
                self.transient_roots.extend(
                    arguments
                        .iter()
                        .chain(std::iter::once(&receiver))
                        .filter_map(|value| match value {
                            JsValue::Object(object) => Some(*object),
                            _ => None,
                        }),
                );
                let result = self.regexp_symbol_method(dom, function, &receiver, arguments);
                self.transient_roots.truncate(pinned);
                result
            }
            Some(ObjectHost::NativeFunction(function)) => match &receiver {
                JsValue::Object(object) => self.call_native(dom, function, *object, arguments),
                // Iterator methods read `this` unchanged: a primitive is a TypeError, not a wrapper.
                _ if crate::runtime::builtins::iterator::reads_receiver_unchanged(function) => Err(
                    JsError::type_error("iterator method called on a non-object receiver"),
                ),
                JsValue::Null | JsValue::Undefined => {
                    let global = self.realm.global_object();
                    self.call_native(dom, function, global, arguments)
                }
                _ => {
                    let receiver =
                        self.coerce_member_base(&receiver, &format!("{function:?} receiver"))?;
                    self.call_native(dom, function, receiver, arguments)
                }
            },
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
                if self
                    .functions
                    .get(index)
                    .and_then(|function| function.class.as_ref())
                    .is_some_and(|class| class.constructor)
                {
                    return Err(JsError::type_error(
                        "class constructor cannot be invoked without 'new'",
                    ));
                }
                self.call_user(dom, callee, index, arguments, receiver, true)
            }
            Some(ObjectHost::ArrowFunction(index)) => {
                self.call_user(dom, callee, index, arguments, receiver, false)
            }
            Some(ObjectHost::AsyncResume {
                coroutine,
                rejected,
            }) => {
                let value = arguments.first().cloned().unwrap_or(JsValue::Undefined);
                let resume = if rejected {
                    Resume::Throw(value)
                } else {
                    Resume::Next(value)
                };
                self.async_continue(dom, coroutine, resume);
                Ok(JsValue::Undefined)
            }
            Some(ObjectHost::AsyncFromSyncValue { done }) => {
                let value = arguments.first().cloned().unwrap_or(JsValue::Undefined);
                self.iteration_result(value, done)
            }
            Some(ObjectHost::AsyncFromSyncClose { iterator }) => {
                let reason = arguments.first().cloned().unwrap_or(JsValue::Undefined);
                // IteratorClose with a throw completion: errors from closing are dropped.
                let _ = self.close_iterator_object(dom, iterator);
                Err(JsError::thrown(reason))
            }
            Some(ObjectHost::PromiseSettler {
                promise,
                fulfilled,
                pair,
            }) => {
                let value = arguments.first().cloned().unwrap_or(JsValue::Undefined);
                self.call_promise_settler(dom, promise, fulfilled, pair, &value)?;
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

    /// Call the user function object `callee`, whose code is `index`. The
    /// callee is needed when the call starts a generator: its `prototype`
    /// property becomes the generator object's prototype.
    pub(super) fn call_user(
        &mut self,
        dom: &mut Dom,
        callee: ObjectId,
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
        // Class methods and constructors are strict: a nullish `this` stays
        // undefined instead of falling back to the global object.
        let receiver = if function.strict {
            receiver
        } else {
            match receiver {
                JsValue::Undefined | JsValue::Null => JsValue::Object(self.realm.global_object()),
                other => other,
            }
        };
        let previous_environment =
            std::mem::replace(&mut self.environment, function.captured_environment.clone());
        let mut call_environment = EnvironmentRecord {
            function_scope: true,
            ..EnvironmentRecord::default()
        };
        // Every non-arrow function binds its own `this`; a derived
        // constructor starts with an uninitialized binding that `super()`
        // replaces, so reading `this` before then is a ReferenceError.
        if !function.arrow {
            let derived_constructor = function
                .class
                .as_ref()
                .is_some_and(|class| class.constructor && class.derived);
            call_environment.bindings.insert(
                "this".to_owned(),
                Binding {
                    value: receiver,
                    mutable: true,
                    initialized: !derived_constructor,
                    kind: VariableKind::Var,
                },
            );
        }
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
        // A parameter list with any default initializer is "non-simple" per
        // spec: every parameter binding is created uninitialized first so a
        // default that reads a later (or its own) parameter observes the
        // temporal dead zone instead of an outer binding.
        if function.defaults.iter().any(Option::is_some) {
            for parameter in &function.parameters {
                call_environment.bindings.insert(
                    parameter.clone(),
                    Binding {
                        value: JsValue::Undefined,
                        mutable: true,
                        initialized: false,
                        kind: VariableKind::Var,
                    },
                );
            }
        }
        let call_environment = Rc::new(RefCell::new(call_environment));
        self.environment.push(call_environment.clone());
        // Arrows inherit the enclosing class context dynamically; named
        // functions use their own class metadata.
        let class_context = if function.arrow {
            self.class_frames.last().cloned().unwrap_or_default()
        } else {
            ClassFrame {
                function: function.class.clone(),
                private_scope: function
                    .class
                    .as_ref()
                    .map(|class| class.private_names.clone()),
            }
        };
        self.class_frames.push(class_context);
        let label = function
            .name
            .clone()
            .unwrap_or_else(|| format!("<anonymous fn #{index}>"));
        // Diagnostic: append the body source offset to the frame label so
        // offline stack traces map back to minified bundle positions
        // (RENDER_JS_FRAME_OFFSETS=1).
        let label = match function.body.first().and_then(statement_offset) {
            Some(offset) if std::env::var_os("RENDER_JS_FRAME_OFFSETS").is_some() => {
                format!("{label}@{offset}")
            }
            _ => label,
        };
        self.call_stack.push(CallFrame { name: label });
        let prepared = self
            .bind_parameters(dom, &function, arguments, &call_environment)
            .and_then(|()| self.instantiate_statements(&function.body));
        let result = match prepared {
            Ok(()) if function.kind.is_coroutine() => self
                .start_coroutine(dom, callee, index, &function, &call_environment)
                .map(Completion::Return),
            Ok(()) => self.evaluate_statements(dom, &function.body),
            // An async function reports a failing parameter list through its
            // promise rather than by throwing (ECMA-262 §10.2.1.1).
            Err(error) if function.kind == FunctionKind::Async => {
                self.rejected_promise_for(&error).map(Completion::Return)
            }
            Err(error) => Err(error),
        };
        self.call_stack.pop();
        self.class_frames.pop();
        // A constructor's `[[Construct]]` result is its final `this`
        // (a base constructor's pre-created instance, or the object a
        // derived constructor received from `super()`).
        let final_this = call_environment
            .borrow()
            .bindings
            .get("this")
            .map(|binding| binding.value.clone());
        let this_initialized = call_environment
            .borrow()
            .bindings
            .get("this")
            .is_some_and(|binding| binding.initialized);
        self.environment = previous_environment;
        match result? {
            Completion::Normal(_)
                if function
                    .class
                    .as_ref()
                    .is_some_and(|class| class.constructor) =>
            {
                Ok(final_this.unwrap_or(JsValue::Undefined))
            }
            Completion::Normal(_) => Ok(JsValue::Undefined),
            // `return;` (or `return undefined`) from a constructor yields its `this`
            // once it is set. Before that, the undefined result lets the caller
            // throw the derived constructor's ReferenceError.
            Completion::Return(JsValue::Undefined)
                if this_initialized
                    && function
                        .class
                        .as_ref()
                        .is_some_and(|class| class.constructor) =>
            {
                Ok(final_this.unwrap_or(JsValue::Undefined))
            }
            Completion::Return(value) => Ok(value),
            Completion::Break(_) | Completion::Continue(_) => Err(JsError::new(
                JsErrorKind::Syntax,
                "loop control escaped a function body",
                None,
            )),
        }
    }

    /// Bind a user function's parameters in order. Each parameter takes its
    /// positional argument, except that an absent or `undefined` argument
    /// evaluates the parameter's default initializer in the environment
    /// built so far, so later defaults see earlier bindings. The final rest
    /// parameter collects the remaining arguments into an array.
    fn bind_parameters(
        &mut self,
        dom: &mut Dom,
        function: &UserFunction,
        arguments: &[JsValue],
        call_environment: &Environment,
    ) -> Result<(), JsError> {
        for (index, parameter) in function.parameters.iter().enumerate() {
            let value = if function.rest && index + 1 == function.parameters.len() {
                let rest =
                    self.create_array_from_values(arguments.get(index..).unwrap_or_default())?;
                JsValue::Object(rest)
            } else {
                match arguments.get(index) {
                    Some(argument) if !matches!(argument, JsValue::Undefined) => argument.clone(),
                    _ => match function.defaults.get(index).and_then(Option::as_ref) {
                        Some(default) => self.evaluate_named(dom, default, parameter)?,
                        None => JsValue::Undefined,
                    },
                }
            };
            {
                let mut environment = call_environment.borrow_mut();
                match environment.bindings.get_mut(parameter) {
                    Some(binding) => {
                        binding.value = value;
                        binding.initialized = true;
                    }
                    None => {
                        environment.bindings.insert(
                            parameter.clone(),
                            Binding {
                                value,
                                mutable: true,
                                initialized: true,
                                kind: VariableKind::Var,
                            },
                        );
                    }
                }
            }
            // A destructuring parameter binds right after its argument
            // (ECMA-262 10.2.11 step 25), so an error in the pattern throws
            // from the call itself, including for generators.
            if let Some(Some(declaration)) = function.patterns.get(index) {
                self.instantiate_statements(std::slice::from_ref(declaration))?;
                self.evaluate_statements(dom, std::slice::from_ref(declaration))?;
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn call_native(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        if std::env::var_os("RENDER_TRACE_NATIVE").is_some() {
            eprintln!(
                "[native] {function:?} receiver={:?} args={}",
                self.realm.host(receiver),
                arguments.len()
            );
        }
        // ECMA-262 does not specify when memory is reclaimed, so this runtime
        // defines it: an active call frame's arguments are reachable, exactly
        // like the variable environments the collector already treats as roots.
        // Without that, a value the caller computed into a Rust argument — a
        // `ToObject` wrapper, a freshly built array — could be swept while the
        // callee is still reading it. Pins taken here are released when the
        // dispatch returns, and a `ToObject` wrapper's pin survives until the
        // outermost frame unwinds, which is when no frame can still read it.
        let pinned = self.transient_roots.len();
        self.transient_roots
            .extend(arguments.iter().filter_map(|argument| match argument {
                JsValue::Object(object) => Some(*object),
                _ => None,
            }));
        self.call_stack.push(CallFrame {
            name: format!("{function:?}"),
        });
        let result = self.call_native_dispatch(dom, function, receiver, arguments);
        self.call_stack.pop();
        self.transient_roots.truncate(pinned);
        result
    }

    pub(super) fn require_callable_object(
        value: &JsValue,
        realm: &Realm,
    ) -> Result<ObjectId, JsError> {
        let object = Self::require_object(value)?;
        if Self::is_callable_object(object, realm) {
            Ok(object)
        } else {
            Err(JsError::type_error(format!(
                "value is not callable (callee host {:?})",
                realm.host(object)
            )))
        }
    }

    pub(super) fn is_callable_object(object: ObjectId, realm: &Realm) -> bool {
        realm.host(object).is_some_and(|host| host.is_callable())
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
            JsValue::BigInt(value) => Ok(self.realm.bigint_primitive_wrapper(value.clone())),
            JsValue::Boolean(value) => Ok(self.realm.boolean_primitive_wrapper(*value)),
        }
    }

    /// ECMA-262 `ToObject` (§7.1.18): every primitive except `null` and
    /// `undefined` boxes into a fresh wrapper whose `[[Prototype]]` is the
    /// matching `%TypeName%.prototype%`, so `valueOf`/`toString` stay on the
    /// prototype and the Number/Boolean wrappers own no properties at all.
    ///
    /// `ToObject` deliberately does not cache: `Object(1) !== Object(1)` in
    /// every engine, so each call allocates its own wrapper. The one cached
    /// wrapper in this runtime is the `Symbol`-as-property-key object built by
    /// `get_symbol_value`, which is a different operation.
    pub(crate) fn to_object(&mut self, value: &JsValue) -> Result<ObjectId, JsError> {
        let object = match value {
            JsValue::Object(object) => return Ok(*object),
            JsValue::String(text) => {
                self.ensure_heap_capacity(1)?;
                self.realm.string_wrapper(text.clone())
            }
            JsValue::Number(number) => {
                self.ensure_heap_capacity(1)?;
                self.realm.number_primitive_wrapper(*number)
            }
            JsValue::BigInt(number) => {
                self.ensure_heap_capacity(1)?;
                self.realm.bigint_primitive_wrapper(number.clone())
            }
            JsValue::Boolean(flag) => {
                self.ensure_heap_capacity(1)?;
                self.realm.boolean_primitive_wrapper(*flag)
            }
            JsValue::Symbol(symbol) => {
                self.ensure_heap_capacity(1)?;
                self.realm.symbol_instance_wrapper(symbol.clone())
            }
            JsValue::Null | JsValue::Undefined => {
                return Err(JsError::type_error(
                    "cannot convert null or undefined to object",
                ));
            }
        };
        // Nothing script-visible holds this wrapper yet, so pin it: a later
        // allocation in the same builtin may collect, and a swept slot reads
        // back as a prototype-less tombstone.
        self.transient_roots.push(object);
        Ok(object)
    }
}

/// Byte offset a parsed expression node carries for diagnostics, when the
/// variant is a positioned struct variant.
pub(super) fn expr_offset(expression: &Expr) -> Option<usize> {
    match expression {
        Expr::RegexLiteral { offset, .. }
        | Expr::Function { offset, .. }
        | Expr::Arrow { offset, .. }
        | Expr::Unary { offset, .. }
        | Expr::Binary { offset, .. }
        | Expr::Conditional { offset, .. }
        | Expr::Update { offset, .. }
        | Expr::Member { offset, .. }
        | Expr::ComputedMember { offset, .. }
        | Expr::New { offset, .. }
        | Expr::Call { offset, .. }
        | Expr::TaggedTemplate { offset, .. }
        | Expr::Assignment { offset, .. }
        | Expr::Class { offset, .. }
        | Expr::SuperMember { offset, .. }
        | Expr::SuperComputedMember { offset, .. }
        | Expr::SuperCall { offset, .. }
        | Expr::PrivateMember { offset, .. }
        | Expr::PrivateIn { offset, .. }
        | Expr::CompoundAssignment { offset, .. }
        | Expr::LogicalAssignment { offset, .. } => Some(*offset),
        Expr::Literal(_)
        | Expr::Elision
        | Expr::This
        | Expr::Identifier(_)
        | Expr::Object(_)
        | Expr::Array(_)
        | Expr::Spread(_)
        | Expr::OptionalChain(_)
        | Expr::OptionalGuard(_)
        | Expr::Await(_)
        | Expr::Yield { .. }
        | Expr::NewTarget
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
        | Statement::With { offset, .. }
        | Statement::While { offset, .. }
        | Statement::DoWhile { offset, .. }
        | Statement::For { offset, .. }
        | Statement::ForIn { offset, .. }
        | Statement::ForOf { offset, .. }
        | Statement::ForInExpr { offset, .. }
        | Statement::Labeled { offset, .. }
        | Statement::Class { offset, .. }
        | Statement::ParameterDefault { offset, .. }
        | Statement::ParameterPattern { offset, .. } => Some(*offset),
        Statement::Return(_)
        | Statement::Throw(_)
        | Statement::Break(_)
        | Statement::Continue(_)
        | Statement::Block(_)
        | Statement::Expression(_) => None,
    }
}

/// A `break`/`continue` aimed at a label this loop carries is aimed at the loop
/// itself, so it is read as the unlabeled form.
fn own_loop_completion(labels: &[String], completion: Completion) -> Completion {
    match completion {
        Completion::Continue(Some(label)) if labels.contains(&label) => Completion::Continue(None),
        Completion::Break(Some(label)) if labels.contains(&label) => Completion::Break(None),
        other => other,
    }
}
