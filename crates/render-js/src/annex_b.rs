//! Annex B.3.2: block-level function declarations in sloppy code.
//!
//! A plain function declared directly in a block, case clause or default clause
//! also gets a `var` binding of its name when a `var` of that name would be no
//! early error where the declaration stands (B.3.2.1 step 1.b, B.3.2.2 step 2.b).
//! Evaluating the declaration then copies the block binding's value to that var
//! binding.
//!
//! The static half lives here: [`mark_block_functions`] sets `annex_b_var` on the
//! declarations that qualify, and [`collect_hoisted_names`] lists them for
//! instantiation. Scripts and function bodies are marked when they are parsed.
//! Eval code is marked when it is instantiated, because its runtime conditions
//! (ECMA-262 B.3.2.3) are only known then.

use crate::parser::BindingTarget;
use crate::parser::FunctionKind;
use crate::parser::Statement;
use crate::parser::VariableKind;
use crate::parser::parameter_binding_name;
use std::collections::BTreeSet;

/// The function declaration a statement of a statement list is: a plain
/// declaration, or one a label names (Annex B.3.2).
pub(crate) fn function_declaration(statement: &Statement) -> Option<&Statement> {
    match statement {
        Statement::Function { .. } => Some(statement),
        _ => labelled_function(statement),
    }
}

/// The plain function a chain of labels names, when a labelled function
/// declaration (Annex B.3.2) is what the statement is. It is declared like an
/// unlabelled function, so hoisting looks through the labels.
pub(crate) fn labelled_function(statement: &Statement) -> Option<&Statement> {
    let mut inner = statement;
    let mut labelled = false;
    while let Statement::Labeled { body, .. } = inner {
        inner = body.as_ref();
        labelled = true;
    }
    (labelled && matches!(inner, Statement::Function { .. })).then_some(inner)
}

/// The lexical declarations a statement makes directly in its statement list.
pub(crate) fn lexical_declarations(statement: &Statement, out: &mut Vec<(String, VariableKind)>) {
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
pub(crate) fn lexical_names(statements: &[Statement], functions: bool) -> Vec<String> {
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

/// The state of one marking walk: `accept` says whether a name may be hoisted
/// beyond the static conditions, and `hoisted` collects the names marked.
struct Walk<'a> {
    accept: &'a mut dyn FnMut(&str) -> bool,
    hoisted: Vec<String>,
}

/// Marks the block-level function declarations of a function body, a script or
/// eval code that Annex B.3.2 gives a var binding, and returns the names marked.
/// `accept` decides the runtime conditions a static walk cannot: eval's lexical
/// and global checks. A caller with no runtime condition passes `|_| true`.
pub(crate) fn mark_block_functions(
    statements: &mut [Statement],
    accept: &mut dyn FnMut(&str) -> bool,
) -> Vec<String> {
    let scope = lexical_names(statements, false);
    let mut walk = Walk {
        accept,
        hoisted: Vec::new(),
    };
    mark_list(statements, &scope, &mut Vec::new(), false, &mut walk);
    walk.hoisted
}

/// Marks the block-level functions of a sloppy function body. The parameter
/// names are excluded: a block-level function named like a parameter keeps the
/// parameter's binding (B.3.2.1 step 1.b). So is `arguments`, which keeps the
/// arguments object: a block-level function of that name neither creates a var
/// binding nor replaces the object (test262 annexB/language/function-code
/// block-decl-func-skip-arguments).
pub(crate) fn mark_function_body(body: &mut [Statement], parameters: &[String]) {
    // Destructuring parameters bind their names through a `ParameterPattern`
    // at the top of the body rather than through `parameters`.
    let mut bound: BTreeSet<&str> = parameters
        .iter()
        .map(|parameter| parameter_binding_name(parameter))
        .collect();
    let patterns: Vec<String> = body
        .iter()
        .filter_map(|statement| match statement {
            Statement::ParameterPattern { declarations, .. } => Some(
                declarations
                    .iter()
                    .flat_map(|(target, _)| target.names())
                    .collect::<Vec<_>>(),
            ),
            _ => None,
        })
        .flatten()
        .collect();
    bound.extend(patterns.iter().map(String::as_str));
    mark_block_functions(body, &mut |name: &str| {
        name != "arguments" && !bound.contains(name)
    });
}

/// Adds to `names` the name of every block-level function that `statement`
/// contains and that [`mark_block_functions`] marked. Function bodies are not
/// entered: their declarations belong to them.
pub(crate) fn collect_hoisted_names(statement: &Statement, names: &mut BTreeSet<String>) {
    visit_nested(statement, &mut |nested| {
        if let Statement::Function {
            name,
            annex_b_var: true,
            ..
        } = nested
        {
            names.insert(name.clone());
        }
    });
}

/// Calls `visit` on `statement` and on every statement nested in it, without
/// entering function bodies.
fn visit_nested(statement: &Statement, visit: &mut dyn FnMut(&Statement)) {
    visit(statement);
    match statement {
        Statement::Block(statements) => {
            for statement in statements {
                visit_nested(statement, visit);
            }
        }
        Statement::If {
            consequent,
            alternate,
            ..
        } => {
            visit_nested(consequent, visit);
            if let Some(alternate) = alternate {
                visit_nested(alternate, visit);
            }
        }
        Statement::While { body, .. }
        | Statement::DoWhile { body, .. }
        | Statement::With { body, .. }
        | Statement::Labeled { body, .. }
        | Statement::ForInExpr { body, .. }
        | Statement::ForIn { body, .. }
        | Statement::ForOf { body, .. } => visit_nested(body, visit),
        Statement::For {
            initializer, body, ..
        } => {
            if let Some(initializer) = initializer {
                visit_nested(initializer, visit);
            }
            visit_nested(body, visit);
        }
        Statement::Try {
            body,
            catch,
            finally,
            ..
        } => {
            for statement in body {
                visit_nested(statement, visit);
            }
            if let Some(catch) = catch {
                for statement in &catch.body {
                    visit_nested(statement, visit);
                }
            }
            for statement in finally.iter().flatten() {
                visit_nested(statement, visit);
            }
        }
        Statement::Switch { cases, .. } => {
            for (_, statements) in cases {
                for statement in statements {
                    visit_nested(statement, visit);
                }
            }
        }
        _ => {}
    }
}

/// Marks the block-level functions of one statement list (B.3.2.1 step 1).
/// `scope` holds the names the list's scope declares, and `enclosing` those of the
/// scopes around it. A declaration is marked when a `var` of its name would be no
/// early error, which needs the name to be declared once in its scope and not in
/// any enclosing one, and `walk.accept` agrees.
fn mark_list(
    statements: &mut [Statement],
    scope: &[String],
    enclosing: &mut Vec<String>,
    block: bool,
    walk: &mut Walk<'_>,
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
            if declared_once && !enclosing.contains(name) && (walk.accept)(name.as_str()) {
                *annex_b_var = true;
                if !walk.hoisted.contains(name) {
                    walk.hoisted.push(name.clone());
                }
            }
        }
    }
    let depth = enclosing.len();
    enclosing.extend(scope.iter().cloned());
    for statement in statements.iter_mut() {
        mark_nested(statement, enclosing, walk);
    }
    enclosing.truncate(depth);
}

/// Descends from a statement into the statement lists it contains, each of which
/// is a scope of its own, or shares one with the lists around it.
fn mark_nested(statement: &mut Statement, enclosing: &mut Vec<String>, walk: &mut Walk<'_>) {
    match statement {
        Statement::Block(statements) => {
            let scope = lexical_names(statements, true);
            mark_list(statements, &scope, enclosing, true, walk);
        }
        Statement::If {
            consequent,
            alternate,
            ..
        } => {
            mark_nested(consequent, enclosing, walk);
            if let Some(alternate) = alternate {
                mark_nested(alternate, enclosing, walk);
            }
        }
        Statement::While { body, .. }
        | Statement::DoWhile { body, .. }
        | Statement::With { body, .. }
        | Statement::Labeled { body, .. }
        | Statement::ForInExpr { body, .. } => mark_nested(body, enclosing, walk),
        Statement::For {
            initializer, body, ..
        } => {
            let depth = enclosing.len();
            if let Some(initializer) = initializer {
                enclosing.extend(lexical_names(std::slice::from_ref(&**initializer), false));
            }
            mark_nested(body, enclosing, walk);
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
            mark_nested(body, enclosing, walk);
            enclosing.truncate(depth);
        }
        Statement::Try {
            body,
            catch,
            finally,
            ..
        } => {
            let scope = lexical_names(body, true);
            mark_list(body, &scope, enclosing, true, walk);
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
                mark_list(&mut catch.body, &scope, enclosing, true, walk);
                enclosing.truncate(depth);
            }
            if let Some(finally) = finally {
                let scope = lexical_names(finally, true);
                mark_list(finally, &scope, enclosing, true, walk);
            }
        }
        Statement::Switch { cases, .. } => {
            // Every clause of a switch shares one block scope.
            let mut scope = Vec::new();
            for (_, statements) in cases.iter() {
                scope.extend(lexical_names(statements, true));
            }
            for (_, statements) in cases.iter_mut() {
                mark_list(statements, &scope, enclosing, true, walk);
            }
        }
        _ => {}
    }
}
