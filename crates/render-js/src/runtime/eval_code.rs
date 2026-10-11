//! `PerformEval` and `EvalDeclarationInstantiation` (ECMA-262 19.2.1.1 and
//! 19.2.1.2), with the Annex B.3.2.3 rules that give a sloppy eval's block-level
//! function declarations a `var` binding.
//!
//! Eval code parses as a Script. It runs in a lexical environment pushed on top
//! of the caller's, or on top of nothing for an indirect eval, which is the
//! global environment. Its `var` and function declarations go to the variable
//! environment: the innermost function-scope record, or the global object when
//! there is none. Strict eval code is its own variable environment.

use crate::JsError;
use crate::JsErrorKind;
use crate::JsValue;
use crate::ObjectId;
use crate::PropertyDescriptor;
use crate::RealmId;
use crate::lexer;
use crate::parser::BindingTarget;
use crate::parser::EvalContext;
use crate::parser::FunctionKind;
use crate::parser::Statement;
use crate::parser::VariableKind;
use crate::parser::collect_var_names;
use crate::parser::parse_eval;
use crate::runtime::JsRuntime;
use crate::runtime::eval::Completion;
use crate::runtime::eval::labelled_function;
use crate::runtime::types::Binding;
use crate::runtime::types::ClassFrame;
use crate::runtime::types::EnvironmentRecord;
use crate::runtime::types::GlobalBinding;
use crate::value::NativeFunction;
use crate::value::ObjectHost;
use render_dom::Dom;
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;

/// A `SyntaxError` with no source position. Eval code's offsets index the eval
/// source, which the caller's source positions would misread.
fn syntax_error(message: impl Into<String>) -> JsError {
    JsError::new(JsErrorKind::Syntax, message, None)
}

/// Strips a parse error's offset, which indexes the eval source, not the caller's.
fn without_offset(error: &JsError) -> JsError {
    JsError::new(error.kind(), error.message(), None)
}

/// The function declaration a statement of a statement list is: a plain
/// declaration, or one a label names (Annex B.3.2).
fn function_declaration(statement: &Statement) -> Option<&Statement> {
    match statement {
        Statement::Function { .. } => Some(statement),
        _ => labelled_function(statement),
    }
}

/// The lexical declarations a statement makes directly in its statement list.
fn lexical_declarations(statement: &Statement, out: &mut Vec<(String, VariableKind)>) {
    match statement {
        Statement::Variable { kind, name, .. } if *kind != VariableKind::Var => {
            out.push((name.clone(), *kind));
        }
        Statement::VariableList {
            kind, declarations, ..
        } if *kind != VariableKind::Var => {
            for (target, _) in declarations {
                out.extend(target.names().into_iter().map(|name| (name, *kind)));
            }
        }
        Statement::Class { name, .. } => out.push((name.clone(), VariableKind::Let)),
        _ => {}
    }
}

/// The names a statement list declares lexically. Function declarations in a
/// block are lexical too; at the top of a script they are not.
fn lexical_names(statements: &[Statement], functions: bool) -> Vec<String> {
    let mut names = Vec::new();
    for statement in statements {
        if functions && let Some(Statement::Function { name, .. }) = function_declaration(statement)
        {
            names.push(name.clone());
            continue;
        }
        let mut declared = Vec::new();
        lexical_declarations(statement, &mut declared);
        names.extend(declared.into_iter().map(|(name, _)| name));
    }
    names
}

/// The state of one Annex B.3.2.3 walk over the eval code: `accept` says whether
/// a name gets a var binding in this eval, and `hoisted` collects the accepted names.
struct AnnexB<'a> {
    accept: &'a mut dyn FnMut(&str) -> bool,
    hoisted: Vec<String>,
}

/// Marks the block-level function declarations of one statement list (ECMA-262
/// B.3.2.3 step 1). `scope` holds the names the list's scope declares, and
/// `enclosing` those of the scopes around it. A declaration is marked when a
/// `var` of its name would be no early error, which needs the name to be
/// declared once in its scope and not in any enclosing one, and `accept` agrees.
fn mark_annex_b_list(
    statements: &mut [Statement],
    scope: &[String],
    enclosing: &mut Vec<String>,
    block: bool,
    annex_b: &mut AnnexB<'_>,
) {
    if block {
        for statement in statements.iter_mut() {
            let Statement::Function {
                name,
                kind: FunctionKind::Normal,
                annex_b_var,
                ..
            } = statement
            else {
                continue;
            };
            let declared_once = scope
                .iter()
                .filter(|other| other.as_str() == name.as_str())
                .count()
                == 1;
            if declared_once && !enclosing.contains(name) && (annex_b.accept)(name.as_str()) {
                *annex_b_var = true;
                if !annex_b.hoisted.contains(name) {
                    annex_b.hoisted.push(name.clone());
                }
            }
        }
    }
    let depth = enclosing.len();
    enclosing.extend(scope.iter().cloned());
    for statement in statements.iter_mut() {
        mark_annex_b_nested(statement, enclosing, annex_b);
    }
    enclosing.truncate(depth);
}

/// Descends from a statement into the statement lists it contains, each of which
/// is a scope of its own, or shares one with the lists around it.
fn mark_annex_b_nested(
    statement: &mut Statement,
    enclosing: &mut Vec<String>,
    annex_b: &mut AnnexB<'_>,
) {
    match statement {
        Statement::Block(statements) => {
            let scope = lexical_names(statements, true);
            mark_annex_b_list(statements, &scope, enclosing, true, annex_b);
        }
        Statement::If {
            consequent,
            alternate,
            ..
        } => {
            mark_annex_b_nested(consequent, enclosing, annex_b);
            if let Some(alternate) = alternate {
                mark_annex_b_nested(alternate, enclosing, annex_b);
            }
        }
        Statement::While { body, .. }
        | Statement::DoWhile { body, .. }
        | Statement::With { body, .. }
        | Statement::Labeled { body, .. }
        | Statement::ForInExpr { body, .. } => mark_annex_b_nested(body, enclosing, annex_b),
        Statement::For {
            initializer, body, ..
        } => {
            let depth = enclosing.len();
            if let Some(initializer) = initializer {
                enclosing.extend(lexical_names(std::slice::from_ref(&**initializer), false));
            }
            mark_annex_b_nested(body, enclosing, annex_b);
            enclosing.truncate(depth);
        }
        Statement::ForIn {
            kind, name, body, ..
        }
        | Statement::ForOf {
            kind, name, body, ..
        } => {
            let depth = enclosing.len();
            if *kind != VariableKind::Var {
                enclosing.push(name.clone());
            }
            mark_annex_b_nested(body, enclosing, annex_b);
            enclosing.truncate(depth);
        }
        Statement::Try {
            body,
            catch,
            finally,
            ..
        } => {
            let scope = lexical_names(body, true);
            mark_annex_b_list(body, &scope, enclosing, true, annex_b);
            if let Some(catch) = catch {
                // A simple catch parameter may be `var`-declared (Annex B.3.4), so
                // only a destructuring one shadows a block function here.
                let depth = enclosing.len();
                if let Some(parameter) = &catch.parameter
                    && !matches!(parameter, BindingTarget::Name(_))
                {
                    enclosing.extend(parameter.names());
                }
                let scope = lexical_names(&catch.body, true);
                mark_annex_b_list(&mut catch.body, &scope, enclosing, true, annex_b);
                enclosing.truncate(depth);
            }
            if let Some(finally) = finally {
                let scope = lexical_names(finally, true);
                mark_annex_b_list(finally, &scope, enclosing, true, annex_b);
            }
        }
        Statement::Switch { cases, .. } => {
            // Every clause of a switch shares one block scope.
            let mut scope = Vec::new();
            for (_, statements) in cases.iter() {
                scope.extend(lexical_names(statements, true));
            }
            for (_, statements) in cases.iter_mut() {
                mark_annex_b_list(statements, &scope, enclosing, true, annex_b);
            }
        }
        _ => {}
    }
}

/// Creates a deletable var binding `name` in a function-scope record, holding
/// `value` (ECMA-262 19.2.1.2 `CreateMutableBinding(name, true)` and
/// `InitializeBinding`).
fn create_eval_var(scope: &mut EnvironmentRecord, name: &str, value: JsValue) {
    scope.bindings.insert(
        name.to_owned(),
        Binding {
            value,
            mutable: true,
            initialized: true,
            kind: VariableKind::Var,
        },
    );
    scope.deletable.insert(name.to_owned());
}

/// Sets the binding `name` of a function-scope record to `value`, creating it
/// when no declaration has (ECMA-262 19.2.1.2 step 14.d, `SetMutableBinding`).
fn set_scope_binding(scope: &mut EnvironmentRecord, name: &str, value: JsValue) {
    if let Some(binding) = scope.bindings.get_mut(name) {
        binding.value = value;
    } else {
        create_eval_var(scope, name, value);
    }
}

/// Creates the var binding `name` of a function-scope record, initialized to
/// undefined, unless a declaration already binds it (ECMA-262 19.2.1.2 step 16.b).
fn declare_scope_var(scope: &mut EnvironmentRecord, name: &str) {
    if !scope.bindings.contains_key(name) {
        create_eval_var(scope, name, JsValue::Undefined);
    }
}

impl JsRuntime {
    /// `PerformEval(x, strictCaller, direct)` (ECMA-262 19.2.1.1). A non-string
    /// argument comes back unchanged. `strict_caller` matters only for a direct
    /// eval, which runs in the caller's scope and class context; an indirect one
    /// runs in the global environment, sloppy unless its own source is strict.
    pub(super) fn perform_eval(
        &mut self,
        dom: &mut Dom,
        argument: JsValue,
        strict_caller: bool,
        direct: bool,
    ) -> Result<JsValue, JsError> {
        let source = match argument {
            JsValue::String(source) => source,
            other => return Ok(other),
        };
        // Steps 5 to 7: a direct eval inherits the function context and the
        // private names of the code that calls it.
        let frame = if direct {
            self.class_frames.last().cloned()
        } else {
            None
        };
        let this_context = frame
            .as_ref()
            .map(|frame| frame.this_context)
            .unwrap_or_default();
        let context = EvalContext {
            strict: direct && strict_caller,
            new_target: this_context.function,
            super_property: this_context.method,
            super_call: this_context.derived_constructor,
            class_field_initializer: this_context.class_field_initializer,
            private_names: frame
                .as_ref()
                .and_then(|frame| frame.private_scope.as_ref())
                .map(|scope| scope.visible_names())
                .unwrap_or_default(),
        };
        // A call of eval is a call for the depth and step accounting of user code.
        self.consume_step()?;
        if self.calls_active >= self.limits.max_call_depth {
            return Err(self.call_depth_exceeded());
        }
        self.calls_active = self.calls_active.saturating_add(1);
        let result = self.run_eval(dom, &(source).to_lossy(), &context, direct);
        self.calls_active = self.calls_active.saturating_sub(1);
        result
    }

    /// Whether `callee` is the intrinsic `%eval%` (ECMA-262 19.2.1.1 step 6).
    pub(super) fn is_intrinsic_eval(&self, callee: ObjectId) -> bool {
        matches!(
            self.realm.host(callee),
            Some(ObjectHost::BoundFunction {
                function: NativeFunction::GlobalEval,
                ..
            })
        )
    }

    /// Parses `source` as eval code and runs it in the environment its call
    /// selects (step 8 onwards).
    fn run_eval(
        &mut self,
        dom: &mut Dom,
        source: &str,
        context: &EvalContext,
        direct: bool,
    ) -> Result<JsValue, JsError> {
        let tokens =
            lexer::tokenize(source, &self.limits).map_err(|error| without_offset(&error))?;
        let (mut statements, strict) =
            parse_eval(tokens, &self.limits, context).map_err(|error| without_offset(&error))?;
        if direct {
            if strict == self.code_is_strict() {
                return self.evaluate_eval_code(dom, &mut statements, strict);
            }
            // Strict eval code inside sloppy code runs under a frame that keeps the
            // caller's class context and differs only in strictness.
            let mut frame = self.class_frames.last().cloned().unwrap_or_default();
            frame.strict = strict;
            self.class_frames.push(frame);
            let outcome = self.evaluate_eval_code(dom, &mut statements, strict);
            self.class_frames.pop();
            return outcome;
        }
        self.run_indirect_eval(dom, &mut statements, strict)
    }

    /// Runs parsed eval code as an indirect eval does: it sees no caller scope
    /// and no class context, and runs in the global environment of the current
    /// realm. The caller's environments stay rooted while it runs.
    fn run_indirect_eval(
        &mut self,
        dom: &mut Dom,
        statements: &mut [Statement],
        strict: bool,
    ) -> Result<JsValue, JsError> {
        let depth = self.suspend_scopes();
        let caller_environment = std::mem::take(&mut self.environment);
        self.class_frames.push(ClassFrame {
            strict,
            ..ClassFrame::default()
        });
        let outcome = self.evaluate_eval_code(dom, statements, strict);
        self.class_frames.pop();
        self.environment = caller_environment;
        self.resume_scopes(depth);
        outcome
    }

    /// Parses the source of a `ShadowRealm.prototype.evaluate` or `importValue`
    /// call as a Script (`ShadowRealm` proposal, `PerformShadowRealmEval` step 2).
    /// A parse error is the caller's `SyntaxError`, so it is reported apart from
    /// what the evaluation later throws.
    pub(in crate::runtime) fn parse_for_realm(
        &self,
        source: &str,
    ) -> Result<(Vec<Statement>, bool), JsError> {
        let tokens =
            lexer::tokenize(source, &self.limits).map_err(|error| without_offset(&error))?;
        parse_eval(tokens, &self.limits, &EvalContext::default())
            .map_err(|error| without_offset(&error))
    }

    /// The evaluation half of `PerformShadowRealmEval`: `statements` run as
    /// indirect eval code in `realm`, which is current while they run.
    pub(in crate::runtime) fn evaluate_parsed_in_realm(
        &mut self,
        dom: &mut Dom,
        statements: &mut [Statement],
        strict: bool,
        realm: RealmId,
    ) -> Result<JsValue, JsError> {
        self.with_realm(realm, |runtime| {
            runtime.run_indirect_eval(dom, statements, strict)
        })
    }

    /// Runs eval code in a new lexical environment, after its declarations are
    /// instantiated (ECMA-262 19.2.1.1 steps 9 to 12, 14 and 15).
    fn evaluate_eval_code(
        &mut self,
        dom: &mut Dom,
        statements: &mut [Statement],
        strict: bool,
    ) -> Result<JsValue, JsError> {
        let depth = self.environment.len();
        // Strict eval code keeps its `var` declarations in this record too, so they
        // do not reach the caller (step 12.a).
        self.environment
            .push(Rc::new(RefCell::new(EnvironmentRecord {
                function_scope: strict,
                ..EnvironmentRecord::default()
            })));
        let outcome = self
            .eval_declaration_instantiation(statements, strict)
            .and_then(|()| self.evaluate_statements(dom, statements));
        self.environment.truncate(depth);
        match outcome? {
            Completion::Normal(value) => Ok(value),
            Completion::Return(_) | Completion::Break(_) | Completion::Continue(_) => {
                Err(syntax_error("abrupt completion escaped eval code"))
            }
        }
    }

    /// `EvalDeclarationInstantiation` (ECMA-262 19.2.1.2) with the Annex B.3.2.3
    /// changes. The innermost record is the eval code's lexical environment, and
    /// the variable environment is the innermost function-scope record, or the
    /// global environment when there is none.
    #[allow(
        clippy::too_many_lines,
        reason = "the steps of EvalDeclarationInstantiation read best in order"
    )]
    fn eval_declaration_instantiation(
        &mut self,
        statements: &mut [Statement],
        strict: bool,
    ) -> Result<(), JsError> {
        let var_scope = self.variable_scope_depth();
        let global = var_scope.is_none();

        // Steps 1 and 3: the var names, and the early errors against the lexical
        // declarations between the eval code and its variable environment. The
        // names of the top-level functions are var names too (TopLevelVarDeclaredNames).
        let mut var_names = BTreeSet::new();
        for statement in statements.iter() {
            collect_var_names(statement, &mut var_names);
            if let Some(Statement::Function { name, .. }) = function_declaration(statement) {
                var_names.insert(name.clone());
            }
        }
        if !strict {
            for name in &var_names {
                if (global && self.global_lexical(name))
                    || self.eval_var_conflicts(name, var_scope, true)
                {
                    return Err(syntax_error(format!(
                        "binding {name:?} conflicts with a lexical declaration"
                    )));
                }
            }
        }

        // Steps 5 to 8: the top-level function declarations. The last declaration
        // of a name wins, and a global variable environment must be able to
        // declare each name as a global function.
        let function_indices: Vec<usize> = statements
            .iter()
            .enumerate()
            .filter(|(_, statement)| function_declaration(statement).is_some())
            .map(|(index, _)| index)
            .collect();
        let mut declared_functions: Vec<String> = Vec::new();
        let mut functions_to_initialize: Vec<usize> = Vec::new();
        for &index in function_indices.iter().rev() {
            let Some(Statement::Function { name, .. }) = function_declaration(&statements[index])
            else {
                continue;
            };
            if declared_functions.contains(name) {
                continue;
            }
            if global && !self.can_declare_global_function(name) {
                return Err(JsError::type_error(format!(
                    "cannot declare global function {name:?}"
                )));
            }
            declared_functions.push(name.clone());
            functions_to_initialize.insert(0, index);
        }

        // Step 9: the other var names, which a global variable environment must be
        // able to declare as global variables.
        let mut declared_vars: Vec<String> = Vec::new();
        for name in &var_names {
            if declared_functions.contains(name) {
                continue;
            }
            if global && !self.can_declare_global_var(name) {
                return Err(JsError::type_error(format!(
                    "cannot declare global variable {name:?}"
                )));
            }
            declared_vars.push(name.clone());
        }

        // Annex B.3.2.3: in sloppy eval code, a block-level function declaration
        // also gets a var binding when no scope between the eval code and the
        // variable environment already binds its name.
        if !strict {
            let scope = lexical_names(statements, false);
            let accept_var_scope = var_scope;
            let mut accept = |name: &str| self.annex_b_hoistable(name, accept_var_scope);
            let mut annex_b = AnnexB {
                accept: &mut accept,
                hoisted: Vec::new(),
            };
            mark_annex_b_list(statements, &scope, &mut Vec::new(), false, &mut annex_b);
            for name in annex_b.hoisted {
                if !declared_functions.contains(&name) && !declared_vars.contains(&name) {
                    declared_vars.push(name);
                }
            }
        }

        // Step 13: the lexical declarations of the eval code, uninitialized.
        let mut lexical = Vec::new();
        for statement in statements.iter() {
            lexical_declarations(statement, &mut lexical);
        }
        let lexical_scope = self
            .environment
            .last()
            .cloned()
            .expect("eval code runs in a lexical environment");
        {
            let mut lexical_scope = lexical_scope.borrow_mut();
            for (name, kind) in lexical {
                lexical_scope.bindings.insert(
                    name,
                    Binding {
                        value: JsValue::Undefined,
                        mutable: !kind.is_immutable(),
                        initialized: false,
                        kind,
                    },
                );
            }
        }

        // Step 14: the function objects, closed over the eval code's lexical
        // environment.
        for index in functions_to_initialize {
            let Some(Statement::Function {
                name,
                parameters,
                body,
                kind,
                strict: function_strict,
                ..
            }) = function_declaration(&statements[index])
            else {
                continue;
            };
            let function = self.create_function(
                Some(name.as_str()),
                parameters,
                body,
                *kind,
                *function_strict,
            )?;
            match var_scope {
                None => self.create_global_function_binding(name, function)?,
                Some(depth) => {
                    set_scope_binding(&mut self.environment[depth].borrow_mut(), name, function);
                }
            }
        }

        // Step 15: the var bindings, initialized to undefined where new.
        for name in declared_vars {
            match var_scope {
                None => self.create_global_var_binding(&name)?,
                Some(depth) => declare_scope_var(&mut self.environment[depth].borrow_mut(), &name),
            }
        }
        Ok(())
    }

    /// `DeleteBinding` of a declarative record (ECMA-262 9.1.1.1.5): only a
    /// binding eval code created is deletable. Returns whether it was removed.
    pub(super) fn delete_scope_binding(&self, depth: usize, name: &str) -> bool {
        let mut record = self.environment[depth].borrow_mut();
        if record.deletable.remove(name) {
            record.bindings.remove(name);
            return true;
        }
        false
    }

    /// The innermost function-scope record, which is the variable environment, or
    /// `None` for the global environment.
    pub(super) fn variable_scope_depth(&self) -> Option<usize> {
        self.environment
            .iter()
            .rposition(|scope| scope.borrow().function_scope)
    }

    /// Whether the global environment binds `name` lexically
    /// (`HasLexicalDeclaration`).
    fn global_lexical(&self, name: &str) -> bool {
        self.global_bindings()
            .get(name)
            .is_some_and(|binding| binding.kind != VariableKind::Var)
    }

    /// Whether a var declaration of `name` conflicts with a binding between the
    /// eval code and its variable environment (ECMA-262 19.2.1.2 step 3.d). The
    /// variable environment's own record conflicts only through lexical bindings,
    /// since parameters and vars share it. A simple catch parameter does not
    /// conflict when `catch_exempt` is set (Annex B.3.4). While parameters are
    /// bound, the variable environment conflicts through every binding.
    fn eval_var_conflicts(&self, name: &str, var_scope: Option<usize>, catch_exempt: bool) -> bool {
        (var_scope.unwrap_or(0)..self.environment.len())
            .rev()
            .any(|depth| {
                let record = self.environment[depth].borrow();
                if record.with_object.is_some() {
                    return false;
                }
                let Some(binding) = record.bindings.get(name) else {
                    return false;
                };
                if Some(depth) == var_scope {
                    return binding.kind != VariableKind::Var || record.parameter_scope;
                }
                !(catch_exempt && record.simple_catch_parameter.as_deref() == Some(name))
            })
    }

    /// Whether Annex B.3.2.3 gives the block-level function `name` a var binding:
    /// no scope between the eval code and the variable environment binds it, and
    /// a global variable environment can declare it.
    fn annex_b_hoistable(&self, name: &str, var_scope: Option<usize>) -> bool {
        if self.eval_var_conflicts(name, var_scope, false) {
            return false;
        }
        var_scope.is_some() || (!self.global_lexical(name) && self.can_declare_global_var(name))
    }

    /// `CanDeclareGlobalVar` (ECMA-262 9.1.1.4.15).
    pub(super) fn can_declare_global_var(&self, name: &str) -> bool {
        let global = self.realm.global_object();
        self.realm.own_property(global, name).is_some() || self.realm.is_extensible(global)
    }

    /// `CanDeclareGlobalFunction` (ECMA-262 9.1.1.4.16).
    pub(super) fn can_declare_global_function(&self, name: &str) -> bool {
        let global = self.realm.global_object();
        match self.realm.own_property(global, name) {
            None => self.realm.is_extensible(global),
            Some(existing) => {
                existing.configurable
                    || (!existing.is_accessor() && existing.writable && existing.enumerable)
            }
        }
    }

    /// `CreateGlobalVarBinding(name, true)` (ECMA-262 9.1.1.4.17).
    fn create_global_var_binding(&mut self, name: &str) -> Result<(), JsError> {
        let global = self.realm.global_object();
        if self.realm.own_property(global, name).is_none()
            && self.realm.is_extensible(global)
            && !self.realm.define_property(
                global,
                name,
                PropertyDescriptor::data(JsValue::Undefined),
            )
        {
            return Err(JsError::type_error(format!(
                "global property {name:?} cannot be defined"
            )));
        }
        self.declare_global_var_name(name);
        Ok(())
    }

    /// `CreateGlobalFunctionBinding(name, value, true)` (ECMA-262 9.1.1.4.18). A
    /// non-configurable existing property keeps its attributes and takes the value.
    fn create_global_function_binding(
        &mut self,
        name: &str,
        value: JsValue,
    ) -> Result<(), JsError> {
        let global = self.realm.global_object();
        let descriptor = match self.realm.own_property(global, name) {
            Some(existing) if !existing.configurable => PropertyDescriptor { value, ..existing },
            _ => PropertyDescriptor::data(value),
        };
        if !self.realm.define_property(global, name, descriptor) {
            return Err(JsError::type_error(format!(
                "global property {name:?} cannot be defined"
            )));
        }
        self.declare_global_var_name(name);
        Ok(())
    }

    /// Records a global var name, so that later scripts see it as a var.
    fn declare_global_var_name(&mut self, name: &str) {
        self.global_bindings_mut()
            .entry(name.to_owned())
            .or_insert(GlobalBinding {
                mutable: true,
                initialized: true,
                kind: VariableKind::Var,
            });
    }
}
