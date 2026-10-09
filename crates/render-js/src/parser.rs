#![allow(
    clippy::cast_precision_loss,
    clippy::match_same_arms,
    clippy::too_many_lines,
    clippy::uninlined_format_args,
    clippy::unused_self
)]

use super::lexer::{TemplatePart, Token, TokenKind, tokenize};
use super::{JsError, JsErrorKind, RuntimeLimits};
use crate::JsValue;
use crate::module::{
    DEFAULT_BINDING, IMPORT_META_BINDING, ImportEntry, ImportName, IndirectExport, ModuleInfo,
};
use crate::value::number_to_string;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

/// Words that are reserved in every context but are not keyword tokens. The
/// lexer hands them out as identifiers, so the parser refuses them where a name
/// is bound or referenced. An escaped spelling decodes to the same name, which
/// the spec also refuses (ECMA-262 12.7.2).
const ALWAYS_RESERVED_NAMES: &[&str] = &[
    "class", "debugger", "enum", "export", "extends", "super", "with",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum VariableKind {
    Let,
    Const,
    Var,
}

/// What a function call does with its body (ECMA-262 §15.2-§15.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(super) enum FunctionKind {
    #[default]
    Normal,
    Async,
    Generator,
    AsyncGenerator,
}

impl FunctionKind {
    pub(super) const fn new(is_async: bool, is_generator: bool) -> Self {
        match (is_async, is_generator) {
            (false, false) => Self::Normal,
            (true, false) => Self::Async,
            (false, true) => Self::Generator,
            (true, true) => Self::AsyncGenerator,
        }
    }

    pub(super) const fn is_async(self) -> bool {
        matches!(self, Self::Async | Self::AsyncGenerator)
    }

    pub(super) const fn is_generator(self) -> bool {
        matches!(self, Self::Generator | Self::AsyncGenerator)
    }

    /// Whether calling the function runs its body as a coroutine.
    pub(super) const fn is_coroutine(self) -> bool {
        !matches!(self, Self::Normal)
    }
}

/// Marker prefix flagging a parameter that carried a default initializer.
/// Neither marker character can appear in a lexer-produced identifier, so the
/// prefixed names stay unambiguous; [`super::runtime::eval`] strips them when
/// binding arguments and derives the spec `length` from their positions.
pub(super) const PARAMETER_DEFAULT_MARKER: char = '\u{2}';

/// Marker prefix flagging a rest parameter (`...rest`), which is always the
/// final parameter and never counts toward the spec `length`.
pub(super) const PARAMETER_REST_MARKER: char = '\u{1}';

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
    /// Joins a template literal's text with one substitution. It is not a
    /// source operator: a substitution converts with `ToString` (the string
    /// hint), where `+` uses the default hint.
    TemplateConcat,
    Subtract,
    Multiply,
    Exponentiate,
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
    /// `??`: short-circuiting nullish coalescing.
    Nullish,
    BitwiseAnd,
    BitwiseXor,
    BitwiseOr,
    LeftShift,
    RightShift,
    UnsignedRightShift,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct CatchClause {
    /// `None` for the optional binding `catch { … }`. A name or a destructuring
    /// pattern (`catch ({ message })`) otherwise.
    pub parameter: Option<BindingTarget>,
    pub body: Vec<Statement>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum PropertyKey {
    Static(String),
    Computed(Expr),
    /// `#name`: a private class element.
    Private(String),
    Spread,
}

/// The role one class body element plays during class evaluation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ClassElementKind {
    Constructor,
    Method,
    Get,
    Set,
    Field,
    /// `static { ... }`: one or more statements run against the constructor.
    StaticBlock,
}

/// One element of a class body: a method/accessor/constructor, a field with
/// an optional initializer, or a static initialization block.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ClassElement {
    pub key: PropertyKey,
    pub kind: ClassElementKind,
    pub is_static: bool,
    pub is_async: bool,
    pub is_generator: bool,
    pub parameters: Vec<String>,
    pub body: Vec<Statement>,
    pub initializer: Option<Expr>,
    pub offset: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct ObjectProperty {
    pub key: PropertyKey,
    pub value: Expr,
    /// Set for `get x()` / `set x(v)` members of an object literal; the
    /// value is the accessor function expression.
    pub accessor: Option<ObjectAccessorKind>,
    /// Set for the `{ x }` shorthand form. It is the only member shape that is
    /// *not* a `PropertyName : AssignmentExpression`, which matters for
    /// `__proto__`: `{ __proto__: base }` sets the prototype while
    /// `{ __proto__ }` installs an own data property.
    pub shorthand: bool,
    /// Set for the `{ x() {} }` method form, which is likewise not a
    /// `PropertyName : AssignmentExpression` and so also keeps `__proto__`
    /// an ordinary own property.
    pub method: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ObjectAccessorKind {
    Getter,
    Setter,
}

/// A binding position: either a plain name or a destructuring pattern.
///
/// A pattern cannot be flattened into an equivalent list of index or member
/// accesses, because an array pattern draws its values from the *iterator
/// protocol* rather than from indexed properties. `var [a] = map` reads the
/// map's first entry, not `map[0]`; `var [a] = someTypedArray` reads through
/// `@@iterator`; and a non-iterable source must throw rather than silently
/// produce `undefined`. The runtime walks the pattern itself for that reason.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum BindingPattern {
    Identifier(String),
    Object {
        properties: Vec<(PropertyKey, Self)>,
        rest: Option<Box<Self>>,
    },
    Array {
        elements: Vec<Option<Self>>,
        rest: Option<Box<Self>>,
    },
    Default {
        pattern: Box<Self>,
        value: Expr,
    },
}

/// What one declarator in a `var`/`let`/`const` list binds.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum BindingTarget {
    Name(String),
    Pattern(BindingPattern),
}

impl BindingTarget {
    pub(super) fn names(&self) -> Vec<String> {
        let mut names = Vec::new();
        match self {
            Self::Name(name) => names.push(name.clone()),
            Self::Pattern(pattern) => collect_binding_names(pattern, &mut names),
        }
        names
    }
}

pub(super) fn collect_binding_names(pattern: &BindingPattern, names: &mut Vec<String>) {
    match pattern {
        BindingPattern::Identifier(name) => names.push(name.clone()),
        BindingPattern::Object { properties, rest } => {
            for (_, pattern) in properties {
                collect_binding_names(pattern, names);
            }
            if let Some(rest) = rest {
                collect_binding_names(rest, names);
            }
        }
        BindingPattern::Array { elements, rest } => {
            for pattern in elements.iter().flatten() {
                collect_binding_names(pattern, names);
            }
            if let Some(rest) = rest {
                collect_binding_names(rest, names);
            }
        }
        BindingPattern::Default { pattern, .. } => collect_binding_names(pattern, names),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Statement {
    Variable {
        kind: VariableKind,
        name: String,
        value: Option<Expr>,
        offset: usize,
    },
    VariableList {
        kind: VariableKind,
        declarations: Vec<(BindingTarget, Option<Expr>)>,
        offset: usize,
    },
    Function {
        name: String,
        parameters: Vec<String>,
        body: Vec<Statement>,
        offset: usize,
        kind: FunctionKind,
    },
    /// `class Name extends Base { ... }`, a lexical binding like `let`.
    Class {
        name: String,
        super_class: Option<Box<Expr>>,
        elements: Vec<ClassElement>,
        offset: usize,
    },
    Return(Option<Expr>),
    Throw(Expr),
    Try {
        body: Vec<Statement>,
        catch: Option<CatchClause>,
        finally: Option<Vec<Statement>>,
        offset: usize,
    },
    If {
        condition: Expr,
        consequent: Box<Statement>,
        alternate: Option<Box<Statement>>,
        offset: usize,
    },
    Switch {
        expression: Expr,
        // Each clause may list several test expressions (`case a, b:`); an
        // empty test list is the `default` clause.
        cases: Vec<(Vec<Expr>, Vec<Statement>)>,
        offset: usize,
    },
    /// `with (object) body` (ECMA-262 14.11). Sloppy code only.
    With {
        object: Expr,
        body: Box<Statement>,
        offset: usize,
    },
    While {
        condition: Expr,
        body: Box<Statement>,
        offset: usize,
    },
    DoWhile {
        condition: Box<Expr>,
        body: Box<Statement>,
        offset: usize,
    },
    For {
        initializer: Option<Box<Statement>>,
        condition: Option<Expr>,
        update: Option<Expr>,
        body: Box<Statement>,
        offset: usize,
    },
    ForIn {
        kind: VariableKind,
        name: String,
        iterable: Expr,
        body: Box<Statement>,
        offset: usize,
    },
    ForOf {
        kind: VariableKind,
        name: String,
        iterable: Expr,
        body: Box<Statement>,
        offset: usize,
        /// `for await (… of …)`: iterate with the async iterator protocol.
        is_await: bool,
    },
    ForInExpr {
        target: Expr,
        iterable: Expr,
        body: Box<Statement>,
        offset: usize,
    },
    Labeled {
        label: String,
        body: Box<Statement>,
        offset: usize,
    },
    Break(Option<String>),
    Continue(Option<String>),
    Block(Vec<Statement>),
    Expression(Expr),
    /// Parser-internal marker recording a parameter's default initializer.
    /// The interpreter extracts these while creating the function (before
    /// any body statement runs) and evaluates them during parameter
    /// binding, so they never execute as ordinary statements.
    ParameterDefault {
        index: usize,
        value: Expr,
        offset: usize,
    },
    /// Parser-internal marker holding the lowered declaration of a destructuring
    /// parameter at position `index`. Like [`Statement::ParameterDefault`] it is
    /// extracted while the function is created: a call binds it straight after
    /// the argument for that position (ECMA-262 10.2.11 step 25), so a generator
    /// or async body never sees it and an invalid pattern throws at the call.
    ParameterPattern {
        index: usize,
        declarations: Vec<(BindingTarget, Option<Expr>)>,
        offset: usize,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Expr {
    Literal(JsValue),
    RegexLiteral {
        pattern: String,
        flags: String,
        offset: usize,
    },
    This,
    Identifier(String),
    Function {
        name: Option<String>,
        parameters: Vec<String>,
        body: Vec<Statement>,
        offset: usize,
        kind: FunctionKind,
    },
    Arrow {
        parameters: Vec<String>,
        body: Vec<Statement>,
        offset: usize,
        is_async: bool,
    },
    /// `yield`, `yield value`, `yield* iterable`.
    Yield {
        argument: Option<Box<Self>>,
        delegate: bool,
        offset: usize,
    },
    /// `await operand`.
    Await(Box<Self>),
    /// `class [Name] [extends Base] { ... }` as an expression.
    Class {
        name: Option<String>,
        super_class: Option<Box<Expr>>,
        elements: Vec<ClassElement>,
        offset: usize,
    },
    /// `super.name` inside a method: a property reference with the current
    /// receiver as its get/set receiver.
    SuperMember {
        property: String,
        offset: usize,
    },
    /// `super[expression]`.
    SuperComputedMember {
        property: Box<Self>,
        offset: usize,
    },
    /// `super(...)` inside a derived constructor.
    SuperCall {
        arguments: Vec<Self>,
        offset: usize,
    },
    /// `new.target`.
    NewTarget,
    /// `object.#name`.
    PrivateMember {
        object: Box<Self>,
        name: String,
        offset: usize,
    },
    /// `#name in object`.
    PrivateIn {
        name: String,
        object: Box<Self>,
        offset: usize,
    },
    Object(Vec<ObjectProperty>),
    Array(Vec<Self>),
    Spread(Box<Self>),
    Unary {
        operator: UnaryOp,
        operand: Box<Self>,
        offset: usize,
    },
    Binary {
        operator: BinaryOp,
        left: Box<Self>,
        right: Box<Self>,
        offset: usize,
    },
    Conditional {
        condition: Box<Self>,
        consequent: Box<Self>,
        alternate: Box<Self>,
        offset: usize,
    },
    Update {
        target: Box<Self>,
        operator: BinaryOp,
        prefix: bool,
        offset: usize,
    },
    Member {
        object: Box<Self>,
        property: String,
        offset: usize,
    },
    ComputedMember {
        object: Box<Self>,
        property: Box<Self>,
        offset: usize,
    },
    New {
        constructor: Box<Self>,
        arguments: Vec<Self>,
        offset: usize,
    },
    Call {
        callee: Box<Self>,
        arguments: Vec<Self>,
        offset: usize,
    },
    /// `` tag`a${x}b` ``: the tag receives a template object followed by the
    /// substitution values, so it is not the same call shape as
    /// `Expr::Call` even though both end in a call.
    TaggedTemplate {
        tag: Box<Self>,
        /// The `(cooked, raw)` pairs, always `substitutions + 1` of them.
        quasis: Vec<(String, String)>,
        expressions: Vec<Self>,
        offset: usize,
    },
    Assignment {
        target: Box<Self>,
        value: Box<Self>,
        offset: usize,
        /// The target was written in parentheses. `(f) = function() {}` does
        /// not name the function, because a parenthesized target is not an
        /// `IdentifierReference` (ECMA-262 13.15.2).
        parenthesized_target: bool,
    },
    CompoundAssignment {
        target: Box<Self>,
        operator: BinaryOp,
        value: Box<Self>,
        offset: usize,
    },
    /// `target &&= value`, `target ||= value`, `target ??= value`: the
    /// operator's short-circuit decision chooses whether the write happens.
    LogicalAssignment {
        target: Box<Self>,
        operator: BinaryOp,
        value: Box<Self>,
        offset: usize,
    },
    Sequence(Vec<Self>),
    /// The extent of an optional chain (`a?.b.c(d)`): a nullish base reached
    /// at an [`Self::OptionalGuard`] inside ends the whole chain with
    /// `undefined`.
    OptionalChain(Box<Self>),
    /// The `?.` itself: evaluates its operand and short-circuits the
    /// enclosing [`Self::OptionalChain`] when the value is `null`/`undefined`.
    OptionalGuard(Box<Self>),
}

pub(super) fn parse(tokens: Vec<Token>, limits: &RuntimeLimits) -> Result<Vec<Statement>, JsError> {
    Parser::new(tokens, limits).program()
}

/// Parse a module body: `import`/`export` become tables in the returned
/// [`ModuleInfo`] and the declarations they wrap stay in the statement list.
pub(super) fn parse_module(
    tokens: Vec<Token>,
    limits: &RuntimeLimits,
) -> Result<(Vec<Statement>, ModuleInfo), JsError> {
    let mut parser = Parser::new(tokens, limits);
    parser.module = Some(ModuleInfo::default());
    // Top-level `await` is part of the module grammar (ECMA-262 §16.2).
    parser.in_async = true;
    // Module code is always strict (ECMA-262 11.2.2).
    parser.strict = true;
    let statements = parser.statement_list(false)?;
    validate_declaration_conflicts(&statements, true)?;
    let info = parser.module.take().unwrap_or_default();
    Ok((statements, info))
}

/// True for `import(...)`, `import.defer(...)` and `import.source(...)`. These
/// calls are not `MemberExpressions`, so `new` cannot apply to them.
fn is_import_call(expression: &Expr) -> bool {
    let Expr::Call { callee, .. } = expression else {
        return false;
    };
    match callee.as_ref() {
        Expr::Identifier(name) => name == "import",
        Expr::Member { object, .. } => {
            matches!(object.as_ref(), Expr::Identifier(name) if name == "import")
        }
        _ => false,
    }
}

/// The grammatical position a sub-statement occupies. Only a plain function
/// declaration can be a statement at all, and then only as an `if` clause or a
/// label body (ECMA-262 Annex B.3.2, B.3.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BodyPosition {
    IfClause,
    LoopBody,
    LabelBody,
}

/// Whether a statement is a declaration that the position does not admit.
/// Labels are looked through: `while (x) l: function f() {}` is refused, since
/// the labelled function is the loop's body (`IsLabelledFunction`).
fn is_refused_body(statement: &Statement, position: BodyPosition) -> bool {
    let mut innermost = statement;
    let mut labelled = false;
    while let Statement::Labeled { body, .. } = innermost {
        labelled = true;
        innermost = body;
    }
    match innermost {
        Statement::Variable { kind, .. } | Statement::VariableList { kind, .. } => {
            *kind != VariableKind::Var
        }
        Statement::Class { .. } => true,
        Statement::Function { kind, .. } => match position {
            BodyPosition::LoopBody => true,
            BodyPosition::IfClause => labelled || *kind != FunctionKind::Normal,
            BodyPosition::LabelBody => *kind != FunctionKind::Normal,
        },
        _ => false,
    }
}

/// Whether the token after a class modifier keyword (`get`, `set`, `async`,
/// `static`) lets it act as a modifier instead of an element name. A name
/// followed by `(`/`=`/`;`/`}` (or nothing) is an ordinary element.
fn class_modifier_follows(next: Option<&Token>) -> bool {
    next.is_some_and(|token| {
        !matches!(
            token.kind,
            TokenKind::LeftParen | TokenKind::Equal | TokenKind::Semicolon | TokenKind::RightBrace
        )
    })
}

/// The name a function built from a property key carries. A computed key has
/// no static name; the runtime names it from the evaluated key.
fn static_key_name(key: &PropertyKey) -> Option<String> {
    match key {
        PropertyKey::Static(name) => Some(name.clone()),
        _ => None,
    }
}

/// Class-body early errors (ECMA-262 §15.7.1): at most one constructor, no
/// field named `constructor`, no static element named `prototype`, no
/// private name `#constructor`, and no duplicate private names.
fn validate_class_elements(elements: &[ClassElement]) -> Result<(), JsError> {
    let mut constructors = 0usize;
    // Private name -> the kinds already declared under it; a getter/setter
    // pair may share one name, anything else is a duplicate.
    let mut private_kinds: BTreeMap<String, Vec<ClassElementKind>> = BTreeMap::new();
    for element in elements {
        if let PropertyKey::Private(name) = &element.key {
            if name == "constructor" {
                return Err(JsError::syntax(
                    "private name #constructor is invalid",
                    element.offset,
                ));
            }
            let kinds = private_kinds.entry(name.clone()).or_default();
            let paired = kinds.len() == 1
                && matches!(
                    (kinds[0], element.kind),
                    (ClassElementKind::Get, ClassElementKind::Set)
                        | (ClassElementKind::Set, ClassElementKind::Get)
                );
            if !paired && !kinds.is_empty() {
                return Err(JsError::syntax(
                    format!("private name #{name} is declared twice"),
                    element.offset,
                ));
            }
            kinds.push(element.kind);
        }
        match (element.kind, element.is_static) {
            (ClassElementKind::Constructor, false) => {
                constructors += 1;
                if constructors > 1 {
                    return Err(JsError::syntax(
                        "class may only have one constructor",
                        element.offset,
                    ));
                }
                if element.is_async || element.is_generator {
                    return Err(JsError::syntax(
                        "class constructor may not be an async or generator method",
                        element.offset,
                    ));
                }
            }
            (_, false) => {
                if matches!(
                    &element.key,
                    PropertyKey::Static(name) if name == "constructor"
                ) && matches!(
                    element.kind,
                    ClassElementKind::Field | ClassElementKind::Get | ClassElementKind::Set
                ) {
                    return Err(JsError::syntax(
                        "class may not have a field or accessor named constructor",
                        element.offset,
                    ));
                }
            }
            (_, true) => {
                if matches!(&element.key, PropertyKey::Static(name) if name == "prototype") {
                    return Err(JsError::syntax(
                        "static class element may not be named prototype",
                        element.offset,
                    ));
                }
                if element.kind == ClassElementKind::Field
                    && matches!(&element.key, PropertyKey::Static(name) if name == "constructor")
                {
                    return Err(JsError::syntax(
                        "static class field may not be named constructor",
                        element.offset,
                    ));
                }
            }
        }
        if let Some(initializer) = &element.initializer {
            if reaches_in_context(initializer, is_arguments_reference) {
                return Err(JsError::syntax(
                    "'arguments' is not allowed in a class field initializer",
                    element.offset,
                ));
            }
            if reaches_in_context(initializer, is_super_call) {
                return Err(JsError::syntax(
                    "'super()' is not allowed in a class field initializer",
                    element.offset,
                ));
            }
        }
    }
    Ok(())
}

fn is_arguments_reference(expression: &Expr) -> bool {
    matches!(expression, Expr::Identifier(name) if name == "arguments")
}

fn is_super_call(expression: &Expr) -> bool {
    matches!(expression, Expr::SuperCall { .. })
}

/// Whether `predicate` holds for `expression` or for a part of it that runs in
/// the same function context: arrow function bodies do, while ordinary function
/// bodies and class element bodies bring their own `arguments`, `super` and
/// `this`. A nested class's heritage and computed keys run in this context.
fn reaches_in_context(expression: &Expr, predicate: fn(&Expr) -> bool) -> bool {
    if predicate(expression) {
        return true;
    }
    let mut expressions = Vec::new();
    let mut statements = Vec::new();
    push_expression_parts(expression, &mut expressions, &mut statements);
    expressions
        .into_iter()
        .any(|part| reaches_in_context(part, predicate))
        || statements
            .into_iter()
            .any(|statement| statement_reaches(statement, predicate))
}

fn statement_reaches(statement: &Statement, predicate: fn(&Expr) -> bool) -> bool {
    let mut expressions = Vec::new();
    let mut statements = Vec::new();
    push_statement_parts(statement, &mut expressions, &mut statements);
    expressions
        .into_iter()
        .any(|part| reaches_in_context(part, predicate))
        || statements
            .into_iter()
            .any(|statement| statement_reaches(statement, predicate))
}

/// The direct subexpressions and substatements of an expression that stay in
/// the enclosing function context (see [`reaches_in_context`]).
fn push_expression_parts<'a>(
    expression: &'a Expr,
    expressions: &mut Vec<&'a Expr>,
    statements: &mut Vec<&'a Statement>,
) {
    match expression {
        Expr::Literal(_)
        | Expr::RegexLiteral { .. }
        | Expr::This
        | Expr::Identifier(_)
        | Expr::Function { .. }
        | Expr::SuperMember { .. }
        | Expr::NewTarget => {}
        Expr::Arrow { body, .. } => statements.extend(body.iter()),
        Expr::Class {
            super_class,
            elements,
            ..
        } => {
            expressions.extend(super_class.as_deref());
            push_class_key_parts(elements, expressions);
        }
        Expr::Yield { argument, .. } => expressions.extend(argument.as_deref()),
        Expr::Await(operand)
        | Expr::Spread(operand)
        | Expr::OptionalChain(operand)
        | Expr::OptionalGuard(operand)
        | Expr::Unary { operand, .. }
        | Expr::Member {
            object: operand, ..
        }
        | Expr::PrivateMember {
            object: operand, ..
        }
        | Expr::PrivateIn {
            object: operand, ..
        }
        | Expr::Update {
            target: operand, ..
        } => expressions.push(operand),
        Expr::SuperComputedMember { property, .. } => expressions.push(property),
        Expr::SuperCall { arguments, .. } => expressions.extend(arguments.iter()),
        Expr::Object(properties) => {
            for property in properties {
                if let PropertyKey::Computed(key) = &property.key {
                    expressions.push(key);
                }
                expressions.push(&property.value);
            }
        }
        Expr::Array(items) | Expr::Sequence(items) => expressions.extend(items.iter()),
        Expr::Binary { left, right, .. } => {
            expressions.push(left);
            expressions.push(right);
        }
        Expr::Conditional {
            condition,
            consequent,
            alternate,
            ..
        } => {
            expressions.push(condition);
            expressions.push(consequent);
            expressions.push(alternate);
        }
        Expr::ComputedMember {
            object, property, ..
        } => {
            expressions.push(object);
            expressions.push(property);
        }
        Expr::New {
            constructor: callee,
            arguments,
            ..
        }
        | Expr::Call {
            callee, arguments, ..
        } => {
            expressions.push(callee);
            expressions.extend(arguments.iter());
        }
        Expr::TaggedTemplate {
            tag,
            expressions: substitutions,
            ..
        } => {
            expressions.push(tag);
            expressions.extend(substitutions.iter());
        }
        Expr::Assignment { target, value, .. }
        | Expr::CompoundAssignment { target, value, .. }
        | Expr::LogicalAssignment { target, value, .. } => {
            expressions.push(target);
            expressions.push(value);
        }
    }
}

/// Keys of a nested class's elements are evaluated in the enclosing context.
fn push_class_key_parts<'a>(elements: &'a [ClassElement], expressions: &mut Vec<&'a Expr>) {
    for element in elements {
        if let PropertyKey::Computed(key) = &element.key {
            expressions.push(key);
        }
    }
}

/// The subexpressions and substatements of a statement that stay in the
/// enclosing function context (see [`reaches_in_context`]).
fn push_statement_parts<'a>(
    statement: &'a Statement,
    expressions: &mut Vec<&'a Expr>,
    statements: &mut Vec<&'a Statement>,
) {
    match statement {
        Statement::Variable { value, .. } => expressions.extend(value.as_ref()),
        Statement::VariableList { declarations, .. }
        | Statement::ParameterPattern { declarations, .. } => {
            for (target, value) in declarations {
                expressions.extend(value.as_ref());
                if let BindingTarget::Pattern(pattern) = target {
                    push_pattern_parts(pattern, expressions);
                }
            }
        }
        Statement::With { object, body, .. } => {
            expressions.push(object);
            statements.push(body);
        }
        Statement::Function { .. } | Statement::Break(_) | Statement::Continue(_) => {}
        Statement::Class {
            super_class,
            elements,
            ..
        } => {
            expressions.extend(super_class.as_deref());
            push_class_key_parts(elements, expressions);
        }
        Statement::Return(value) => expressions.extend(value.as_ref()),
        Statement::Throw(value) | Statement::Expression(value) => expressions.push(value),
        Statement::ParameterDefault { value, .. } => expressions.push(value),
        Statement::Try {
            body,
            catch,
            finally,
            ..
        } => {
            statements.extend(body.iter());
            if let Some(catch) = catch {
                if let Some(BindingTarget::Pattern(pattern)) = &catch.parameter {
                    push_pattern_parts(pattern, expressions);
                }
                statements.extend(catch.body.iter());
            }
            statements.extend(finally.iter().flatten());
        }
        Statement::If {
            condition,
            consequent,
            alternate,
            ..
        } => {
            expressions.push(condition);
            statements.push(consequent);
            statements.extend(alternate.as_deref());
        }
        Statement::Switch {
            expression, cases, ..
        } => {
            expressions.push(expression);
            for (tests, body) in cases {
                expressions.extend(tests.iter());
                statements.extend(body.iter());
            }
        }
        Statement::While {
            condition, body, ..
        } => {
            expressions.push(condition);
            statements.push(body);
        }
        Statement::DoWhile {
            condition, body, ..
        } => {
            expressions.push(condition);
            statements.push(body);
        }
        Statement::For {
            initializer,
            condition,
            update,
            body,
            ..
        } => {
            statements.extend(initializer.as_deref());
            expressions.extend(condition.as_ref());
            expressions.extend(update.as_ref());
            statements.push(body);
        }
        Statement::ForIn { iterable, body, .. } | Statement::ForOf { iterable, body, .. } => {
            expressions.push(iterable);
            statements.push(body);
        }
        Statement::ForInExpr {
            target,
            iterable,
            body,
            ..
        } => {
            expressions.push(target);
            expressions.push(iterable);
            statements.push(body);
        }
        Statement::Labeled { body, .. } => statements.push(body),
        Statement::Block(body) => statements.extend(body.iter()),
    }
}

/// Default values and computed keys inside a binding pattern.
fn push_pattern_parts<'a>(pattern: &'a BindingPattern, expressions: &mut Vec<&'a Expr>) {
    match pattern {
        BindingPattern::Identifier(_) => {}
        BindingPattern::Object { properties, rest } => {
            for (key, nested) in properties {
                if let PropertyKey::Computed(key) = key {
                    expressions.push(key);
                }
                push_pattern_parts(nested, expressions);
            }
            if let Some(rest) = rest {
                push_pattern_parts(rest, expressions);
            }
        }
        BindingPattern::Array { elements, rest } => {
            for nested in elements.iter().flatten() {
                push_pattern_parts(nested, expressions);
            }
            if let Some(rest) = rest {
                push_pattern_parts(rest, expressions);
            }
        }
        BindingPattern::Default { pattern, value } => {
            expressions.push(value);
            push_pattern_parts(pattern, expressions);
        }
    }
}

impl Parser {
    fn new(tokens: Vec<Token>, limits: &RuntimeLimits) -> Self {
        Self {
            tokens,
            cursor: 0,
            previous_offset: 0,
            statement_count: 0,
            max_statements: limits.max_statements,
            function_depth: 0,
            loop_depth: 0,
            switch_depth: 0,
            private_references: Vec::new(),
            no_in: false,
            module: None,
            in_async: false,
            in_generator: false,
            in_parameters: false,
            super_property_allowed: false,
            super_call_allowed: false,
            labels: Vec::new(),
            static_block: false,
            static_block_await: false,
            strict: false,
        }
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent grammar context (no_in, async, generator, parameters, strict, ...)"
)]
struct Parser {
    previous_offset: usize,
    tokens: Vec<Token>,
    cursor: usize,
    statement_count: usize,
    max_statements: usize,
    function_depth: usize,
    loop_depth: usize,
    switch_depth: usize,
    /// One entry per class body being parsed (innermost last), listing the
    /// `#name` references written in it that no declaration has resolved yet.
    /// A reference is valid only when some enclosing class body declares the
    /// name (ECMA-262 `AllPrivateNamesValid`), whatever order they appear in.
    private_references: Vec<Vec<(String, usize)>>,
    /// While set, `in` is not treated as a binary operator (for-heads).
    no_in: bool,
    /// Present while parsing a module; collects its import/export tables.
    module: Option<ModuleInfo>,
    /// Whether the function being parsed is async (`await` is an operator) or
    /// a generator (`yield` is an operator). Outside both, they are ordinary
    /// identifiers.
    in_async: bool,
    in_generator: bool,
    /// Set while the formal parameters of a function are parsed. A `YieldExpression`
    /// or `AwaitExpression` there is an early error (ECMA-262 15.5.1, 15.8.1).
    in_parameters: bool,
    /// `super.name` and `super[expression]` are valid: inside a method, a class
    /// field initializer, or a static block, and in arrows nested in them
    /// (ECMA-262 13.3.7.1).
    super_property_allowed: bool,
    /// `super(...)` is valid only in the constructor of a derived class, and in
    /// arrows nested in it (ECMA-262 13.3.7.1, 15.7.1).
    super_call_allowed: bool,
    /// The labels enclosing the statement being parsed, innermost last, with
    /// whether each labels an iteration statement (`break`/`continue` targets).
    labels: Vec<(String, bool)>,
    /// Inside a class static block, where `arguments` is not an identifier
    /// reference (ECMA-262 15.7.1). Arrows keep the restriction; functions drop it.
    static_block: bool,
    /// Inside a class static block's `await` restriction, which arrow parameters
    /// keep but arrow bodies do not (ECMA-262 15.7.1, 15.3).
    static_block_await: bool,
    /// Whether the code being parsed is strict: a `"use strict"` directive
    /// is in force in an enclosing program or function body. `with` is an
    /// early error there (ECMA-262 14.11.1), and it must be known while the
    /// body is parsed because the directive comes before the `with`.
    strict: bool,
}

impl Parser {
    fn program(mut self) -> Result<Vec<Statement>, JsError> {
        self.strict = self.prologue_is_strict(self.cursor);
        let statements = self.statement_list(false)?;
        validate_declaration_conflicts(&statements, true)?;
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

    /// A statement that sits where only a `Statement` is grammatical, not a
    /// `StatementListItem`: the clause of an `if`, a loop body, or a label body.
    fn statement_in(&mut self, position: BodyPosition) -> Result<Statement, JsError> {
        // ExpressionStatement excludes `let [` (ECMA-262 14.5), and `let` then a
        // binding on the same line is a declaration. A line break after `let`
        // ends the statement (ASI), leaving `let` as an expression of its own.
        if self.at(&TokenKind::Let) {
            let next = self.tokens.get(self.cursor + 1);
            if next.is_some_and(|next| matches!(next.kind, TokenKind::LeftBracket)) {
                return Err(self.error("'let [' is not allowed in statement position"));
            }
            if let Some(next) = next {
                if !next.after_newline
                    && matches!(next.kind, TokenKind::Identifier(_) | TokenKind::LeftBrace)
                {
                    return Err(self.error("declaration is not allowed in statement position"));
                }
                if next.after_newline && matches!(next.kind, TokenKind::Identifier(_)) {
                    self.advance();
                    self.end_statement()?;
                    return Ok(Statement::Expression(Expr::Identifier("let".to_owned())));
                }
            }
        }
        let statement = self.statement()?;
        if is_refused_body(&statement, position) {
            return Err(self.error("declaration is not allowed in statement position"));
        }
        Ok(statement)
    }

    fn statement(&mut self) -> Result<Statement, JsError> {
        if self.take(&TokenKind::Semicolon) {
            return Ok(Statement::Block(Vec::new()));
        }
        if self.take(&TokenKind::LeftBrace) {
            let statements = self.statement_list(true)?;
            self.require(&TokenKind::RightBrace, "expected '}' after block")?;
            validate_declaration_conflicts(&statements, false)?;
            return Ok(Statement::Block(statements));
        }
        if self.take(&TokenKind::Function) {
            let is_generator = self.take(&TokenKind::Star);
            return self.function_declaration(FunctionKind::new(false, is_generator));
        }
        // Module declarations are intentionally lowered into the shared page
        // realm. Static imports/exports are dependency metadata for the
        // browser loader, so consume their declaration here; the surrounding
        // module remains executable without aborting on syntax it cannot bind.
        if matches!(&self.current().kind, TokenKind::Identifier(name) if name == "import" || name == "export")
            && !matches!(
                self.tokens.get(self.cursor + 1).map(|token| &token.kind),
                Some(TokenKind::Dot | TokenKind::LeftParen)
            )
        {
            if self.module.is_some() {
                return self.module_declaration();
            }
            self.skip_module_declaration();
            return Ok(Statement::Block(Vec::new()));
        }
        if matches!(&self.current().kind, TokenKind::Identifier(name) if name == "class") {
            self.advance();
            let TokenKind::Identifier(name) = self.current().kind.clone() else {
                return Err(self.error("class declaration requires a name"));
            };
            self.check_class_name(&name)?;
            self.advance();
            let (super_class, elements) = self.class_tail()?;
            return Ok(Statement::Class {
                offset: self.previous_offset(),
                name,
                super_class,
                elements,
            });
        }
        if matches!(
            &self.current().kind,
            TokenKind::Identifier(name) if name == "async"
        ) && matches!(
            self.tokens.get(self.cursor + 1).map(|token| &token.kind),
            Some(TokenKind::Function)
        ) {
            self.advance();
            self.advance();
            let is_generator = self.take(&TokenKind::Star);
            return self.function_declaration(FunctionKind::new(true, is_generator));
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
        // `with` is a reserved word (ECMA-262 13.1.1), so at statement start
        // it always begins a `with` statement. An escaped spelling never acts
        // as a keyword, and then the reserved-name check refuses it instead.
        if self.at_contextual("with") && !self.current().escaped {
            self.advance();
            return self.with_statement();
        }
        if matches!(&self.current().kind, TokenKind::Identifier(_))
            && matches!(
                self.tokens.get(self.cursor + 1).map(|token| &token.kind),
                Some(TokenKind::Colon)
            )
        {
            // The label is a loop label when the statement it names, past any
            // further labels, is an iteration statement (ECMA-262 14.7.1).
            let targets_loop = self.label_targets_loop();
            let TokenKind::Identifier(label) = self.advance().kind else {
                unreachable!("checked above");
            };
            self.check_contextual_binding(&label)?;
            if self.labels.iter().any(|(active, _)| *active == label) {
                return Err(self.error("duplicate label in the label set"));
            }
            self.advance();
            self.labels.push((label.clone(), targets_loop));
            let body = self.statement_in(BodyPosition::LabelBody);
            self.labels.pop();
            let body = body?;
            return Ok(Statement::Labeled {
                offset: self.previous_offset(),
                label,
                body: Box::new(body),
            });
        }
        if self.take(&TokenKind::Break) {
            let label = self.take_loop_label();
            match &label {
                Some(name) if !self.labels.iter().any(|(active, _)| active == name) => {
                    return Err(self.error("break targets a label that does not enclose it"));
                }
                None if self.loop_depth == 0 && self.switch_depth == 0 => {
                    return Err(self.error("break is only valid inside a loop or switch"));
                }
                _ => {}
            }
            self.end_statement()?;
            return Ok(Statement::Break(label));
        }
        if self.take(&TokenKind::Continue) {
            let label = self.take_loop_label();
            match &label {
                Some(name)
                    if !self
                        .labels
                        .iter()
                        .any(|(active, is_loop)| active == name && *is_loop) =>
                {
                    return Err(self.error("continue targets a label that is not a loop"));
                }
                None if self.loop_depth == 0 => {
                    return Err(self.error("continue is only valid inside a loop"));
                }
                _ => {}
            }
            self.end_statement()?;
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
            self.end_statement()?;
            return Ok(Statement::Throw(value));
        }
        if self.take(&TokenKind::Return) {
            if self.function_depth == 0 {
                return Err(self.error("return is only valid inside a function"));
            }
            let value = if self.at(&TokenKind::Semicolon)
                || self.at(&TokenKind::RightBrace)
                || self.current().after_newline
            {
                None
            } else {
                Some(self.expression()?)
            };
            self.end_statement()?;
            return Ok(Statement::Return(value));
        }
        if let Some(kind) = self.take_variable_kind() {
            return self.variable_declaration(kind, true);
        }
        let expression = self.expression()?;
        self.end_statement()?;
        Ok(Statement::Expression(expression))
    }

    /// One `import` or `export` declaration of a module (ECMA-262 §16.2).
    fn module_declaration(&mut self) -> Result<Statement, JsError> {
        let is_import =
            matches!(&self.advance().kind, TokenKind::Identifier(name) if name == "import");
        if is_import {
            self.import_declaration()?;
            Ok(Statement::Block(Vec::new()))
        } else {
            self.export_declaration()
        }
    }

    fn module_info(&mut self) -> &mut ModuleInfo {
        self.module.as_mut().expect("module mode checked by caller")
    }

    fn at_contextual(&self, word: &str) -> bool {
        matches!(&self.current().kind, TokenKind::Identifier(name) if name == word)
    }

    fn module_specifier(&mut self) -> Result<String, JsError> {
        let TokenKind::String(specifier) = self.current().kind.clone() else {
            return Err(self.error("expected a module specifier string"));
        };
        self.advance();
        self.module_info().add_request(&specifier);
        // Import attributes (`with { type: "json" }`) carry no linking meaning
        // here; consume the block so the declaration ends cleanly.
        if (self.at_contextual("with") || self.at_contextual("assert"))
            && matches!(
                self.tokens.get(self.cursor + 1).map(|token| &token.kind),
                Some(TokenKind::LeftBrace)
            )
        {
            self.advance();
            self.skip_module_declaration();
        }
        Ok(specifier)
    }

    /// An `IdentifierName` or string literal in import/export-name position.
    fn module_export_name(&mut self) -> Result<String, JsError> {
        match self.current().kind.clone() {
            TokenKind::Identifier(name) | TokenKind::String(name) => {
                self.advance();
                Ok(name)
            }
            TokenKind::Default => {
                self.advance();
                Ok("default".to_owned())
            }
            _ => Err(self.error("expected an import or export name")),
        }
    }

    fn expect_from(&mut self) -> Result<String, JsError> {
        if !self.at_contextual("from") {
            return Err(self.error("expected 'from' in module declaration"));
        }
        self.advance();
        self.module_specifier()
    }

    /// `await` cannot name a binding or label in async code and `yield` cannot
    /// in generator code (ECMA-262 13.1.1, 14.1.1, 14.4.1). A module's top level
    /// counts as async, so `await` is reserved there too.
    fn check_contextual_binding(&self, name: &str) -> Result<(), JsError> {
        if ALWAYS_RESERVED_NAMES.contains(&name) {
            return Err(self.error("reserved word cannot be used as a binding name"));
        }
        if self.in_async && name == "await" {
            return Err(self.error("await cannot be a binding name in async code"));
        }
        if self.in_generator && name == "yield" {
            return Err(self.error("yield cannot be a binding name in generator code"));
        }
        if self.static_block_await && name == "await" {
            return Err(self.error("await cannot be a binding name in a class static block"));
        }
        Ok(())
    }

    fn local_identifier(&mut self) -> Result<String, JsError> {
        let TokenKind::Identifier(name) = self.current().kind.clone() else {
            return Err(self.error("expected a binding identifier"));
        };
        self.advance();
        Ok(name)
    }

    fn import_declaration(&mut self) -> Result<(), JsError> {
        if matches!(self.current().kind, TokenKind::String(_)) {
            self.module_specifier()?;
            self.end_statement()?;
            return Ok(());
        }
        // (imported, local) pairs, resolved against the specifier afterwards.
        let mut entries: Vec<(ImportName, String)> = Vec::new();
        if matches!(&self.current().kind, TokenKind::Identifier(_)) {
            let local = self.local_identifier()?;
            entries.push((ImportName::Named("default".to_owned()), local));
            if !self.take(&TokenKind::Comma) {
                let request = self.expect_from()?;
                self.finish_import(&request, entries)?;
                return Ok(());
            }
        }
        if self.take(&TokenKind::Star) {
            if !self.at_contextual("as") {
                return Err(self.error("expected 'as' after '*' in import"));
            }
            self.advance();
            let local = self.local_identifier()?;
            entries.push((ImportName::Namespace, local));
        } else if self.take(&TokenKind::LeftBrace) {
            while !self.at(&TokenKind::RightBrace) {
                let imported = self.module_export_name()?;
                let local = if self.at_contextual("as") {
                    self.advance();
                    self.local_identifier()?
                } else {
                    imported.clone()
                };
                entries.push((ImportName::Named(imported), local));
                if !self.take(&TokenKind::Comma) {
                    break;
                }
            }
            self.require(&TokenKind::RightBrace, "expected '}' in import list")?;
        } else {
            return Err(self.error("malformed import declaration"));
        }
        let request = self.expect_from()?;
        self.finish_import(&request, entries)?;
        Ok(())
    }

    fn finish_import(
        &mut self,
        request: &str,
        entries: Vec<(ImportName, String)>,
    ) -> Result<(), JsError> {
        self.end_statement()?;
        for (imported, local) in entries {
            self.module_info().imports.push(ImportEntry {
                request: request.to_owned(),
                imported,
                local,
            });
        }
        Ok(())
    }

    fn export_declaration(&mut self) -> Result<Statement, JsError> {
        if self.take(&TokenKind::Star) {
            let exported = if self.at_contextual("as") {
                self.advance();
                Some(self.module_export_name()?)
            } else {
                None
            };
            let request = self.expect_from()?;
            self.end_statement()?;
            match exported {
                Some(exported) => self.module_info().indirect_exports.push(IndirectExport {
                    exported,
                    request,
                    imported: ImportName::Namespace,
                }),
                None => self.module_info().star_exports.push(request),
            }
            return Ok(Statement::Block(Vec::new()));
        }
        if self.take(&TokenKind::LeftBrace) {
            let mut names = Vec::new();
            while !self.at(&TokenKind::RightBrace) {
                let local = self.module_export_name()?;
                let exported = if self.at_contextual("as") {
                    self.advance();
                    self.module_export_name()?
                } else {
                    local.clone()
                };
                names.push((local, exported));
                if !self.take(&TokenKind::Comma) {
                    break;
                }
            }
            self.require(&TokenKind::RightBrace, "expected '}' in export list")?;
            if self.at_contextual("from") {
                self.advance();
                let request = self.module_specifier()?;
                for (imported, exported) in names {
                    self.module_info().indirect_exports.push(IndirectExport {
                        exported,
                        request: request.clone(),
                        imported: ImportName::Named(imported),
                    });
                }
            } else {
                for (local, exported) in names {
                    self.module_info().local_exports.push((exported, local));
                }
            }
            self.end_statement()?;
            return Ok(Statement::Block(Vec::new()));
        }
        if self.take(&TokenKind::Default) {
            return self.export_default();
        }
        // `export var|let|const|function|class|async function`: the wrapped
        // declaration stays in the body and its bound names become exports.
        let declaration = self.statement()?;
        let mut names = Vec::new();
        match &declaration {
            Statement::Variable { name, .. }
            | Statement::Function { name, .. }
            | Statement::Class { name, .. } => names.push(name.clone()),
            Statement::VariableList { declarations, .. } => {
                for (target, _) in declarations {
                    names.extend(target.names());
                }
            }
            _ => return Err(self.error("unsupported export declaration")),
        }
        for name in names {
            self.module_info().local_exports.push((name.clone(), name));
        }
        Ok(declaration)
    }

    fn export_default(&mut self) -> Result<Statement, JsError> {
        let named_declaration = match &self.current().kind {
            TokenKind::Function => {
                matches!(
                    self.tokens.get(self.cursor + 1).map(|token| &token.kind),
                    Some(TokenKind::Identifier(_))
                ) || matches!(
                    (
                        self.tokens.get(self.cursor + 1).map(|token| &token.kind),
                        self.tokens.get(self.cursor + 2).map(|token| &token.kind),
                    ),
                    (Some(TokenKind::Star), Some(TokenKind::Identifier(_)))
                )
            }
            TokenKind::Identifier(word) if word == "class" => matches!(
                self.tokens.get(self.cursor + 1).map(|token| &token.kind),
                Some(TokenKind::Identifier(name)) if name != "extends"
            ),
            TokenKind::Identifier(word) if word == "async" => matches!(
                (
                    self.tokens.get(self.cursor + 1).map(|token| &token.kind),
                    self.tokens.get(self.cursor + 2).map(|token| &token.kind),
                ),
                (Some(TokenKind::Function), Some(TokenKind::Identifier(_)))
            ),
            _ => false,
        };
        if named_declaration {
            let declaration = self.statement()?;
            let (Statement::Function { name, .. } | Statement::Class { name, .. }) = &declaration
            else {
                return Err(self.error("unsupported default export declaration"));
            };
            let local = name.clone();
            self.module_info()
                .local_exports
                .push(("default".to_owned(), local));
            return Ok(declaration);
        }
        // Anonymous function/class declarations and every other expression
        // are evaluated in place and bound to the hidden default binding.
        let value = self.assignment()?;
        self.end_statement()?;
        self.module_info()
            .local_exports
            .push(("default".to_owned(), DEFAULT_BINDING.to_owned()));
        Ok(Statement::Variable {
            kind: VariableKind::Const,
            name: DEFAULT_BINDING.to_owned(),
            value: Some(value),
            offset: self.previous_offset(),
        })
    }

    fn skip_module_declaration(&mut self) {
        let mut parens = 0_u32;
        let mut brackets = 0_u32;
        let mut braces = 0_u32;
        while !self.at(&TokenKind::Eof) {
            match self.current().kind {
                TokenKind::LeftParen => parens = parens.saturating_add(1),
                TokenKind::RightParen => parens = parens.saturating_sub(1),
                TokenKind::LeftBracket => brackets = brackets.saturating_add(1),
                TokenKind::RightBracket => brackets = brackets.saturating_sub(1),
                TokenKind::LeftBrace => braces = braces.saturating_add(1),
                TokenKind::RightBrace => {
                    if braces == 0 && parens == 0 && brackets == 0 {
                        break;
                    }
                    braces = braces.saturating_sub(1);
                }
                TokenKind::Semicolon if parens == 0 && brackets == 0 && braces == 0 => {
                    self.advance();
                    break;
                }
                _ => {}
            }
            self.advance();
        }
    }

    /// Consume an identifier in `break`/`continue` label position. A line
    /// terminator ends the statement instead of introducing a label.
    /// Whether the labelled statement at the cursor (`label :`) names an
    /// iteration statement, looking past any further labels (ECMA-262 14.7.1).
    fn label_targets_loop(&self) -> bool {
        let mut index = self.cursor + 2;
        loop {
            let next = self.tokens.get(index + 1).map(|token| &token.kind);
            match self.tokens.get(index).map(|token| &token.kind) {
                Some(TokenKind::Identifier(_)) if matches!(next, Some(TokenKind::Colon)) => {
                    index += 2;
                }
                Some(kind) => {
                    return matches!(kind, TokenKind::For | TokenKind::While | TokenKind::Do);
                }
                None => return false,
            }
        }
    }

    fn take_loop_label(&mut self) -> Option<String> {
        if let TokenKind::Identifier(name) = &self.current().kind {
            if self.current().after_newline {
                return None;
            }
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
            if self.at(&TokenKind::LeftBrace) || self.at(&TokenKind::LeftBracket) {
                let pattern = self.binding_pattern()?;
                self.require(
                    &TokenKind::Equal,
                    "destructuring declarations require an initializer",
                )?;
                let initializer = self.assignment()?;
                declarations.push((BindingTarget::Pattern(pattern), Some(initializer)));
            } else {
                let TokenKind::Identifier(name) = self.advance().kind else {
                    return Err(self.error("expected a binding after declaration keyword"));
                };
                self.check_contextual_binding(&name)?;
                let value = if self.take(&TokenKind::Equal) {
                    Some(self.assignment()?)
                } else {
                    None
                };
                if kind == VariableKind::Const && value.is_none() {
                    return Err(self.error("const declarations require an initializer"));
                }
                declarations.push((BindingTarget::Name(name), value));
            }
            if !self.take(&TokenKind::Comma) {
                break;
            }
        }
        if end_statement {
            self.end_statement()?;
        }
        // A single plain name keeps the compact `Statement::Variable` shape,
        // which the for-loop head and several early-error checks match on.
        if declarations.len() == 1
            && matches!(declarations.first(), Some((BindingTarget::Name(_), _)))
            && let (BindingTarget::Name(name), value) = declarations.pop().expect("one exists")
        {
            return Ok(Statement::Variable {
                offset: self.previous_offset(),
                kind,
                name,
                value,
            });
        }
        Ok(Statement::VariableList {
            offset: self.previous_offset(),
            kind,
            declarations,
        })
    }

    fn binding_pattern(&mut self) -> Result<BindingPattern, JsError> {
        if self.take(&TokenKind::LeftBrace) {
            let mut properties = Vec::new();
            let mut rest = None;
            while !self.at(&TokenKind::RightBrace) && !self.at(&TokenKind::Eof) {
                if self.take(&TokenKind::Ellipsis) {
                    rest = Some(Box::new(self.binding_pattern()?));
                    let _ = self.take(&TokenKind::Comma);
                    break;
                }
                // `PropertyName : AssignmentElement`, so a computed key may
                // carry an arbitrary expression evaluated in binding order.
                // Only an identifier token may stand alone; a keyword or an
                // escaped reserved word spelling the same name may not.
                let shorthand_candidate = matches!(
                    &self.current().kind,
                    TokenKind::Identifier(_) | TokenKind::Undefined
                );
                let key = self.property_key()?;
                let mut pattern = if self.take(&TokenKind::Colon) {
                    self.binding_pattern()?
                } else if let PropertyKey::Static(property) = &key
                    && shorthand_candidate
                    && is_identifier_name(property)
                {
                    BindingPattern::Identifier(property.clone())
                } else {
                    return Err(self.error("object binding shorthand requires an identifier"));
                };
                if self.take(&TokenKind::Equal) {
                    pattern = BindingPattern::Default {
                        pattern: Box::new(pattern),
                        value: self.assignment()?,
                    };
                }
                properties.push((key, pattern));
                if !self.take(&TokenKind::Comma) {
                    break;
                }
            }
            self.require(
                &TokenKind::RightBrace,
                "expected '}' after object binding pattern",
            )?;
            return Ok(BindingPattern::Object { properties, rest });
        }
        if self.take(&TokenKind::LeftBracket) {
            let mut elements = Vec::new();
            let mut rest = None;
            while !self.at(&TokenKind::RightBracket) && !self.at(&TokenKind::Eof) {
                if self.take(&TokenKind::Comma) {
                    elements.push(None);
                    continue;
                }
                if self.take(&TokenKind::Ellipsis) {
                    rest = Some(Box::new(self.binding_pattern()?));
                    let _ = self.take(&TokenKind::Comma);
                    break;
                }
                let mut pattern = self.binding_pattern()?;
                if self.take(&TokenKind::Equal) {
                    pattern = BindingPattern::Default {
                        pattern: Box::new(pattern),
                        value: self.assignment()?,
                    };
                }
                elements.push(Some(pattern));
                if !self.take(&TokenKind::Comma) {
                    break;
                }
            }
            self.require(
                &TokenKind::RightBracket,
                "expected ']' after array binding pattern",
            )?;
            return Ok(BindingPattern::Array { elements, rest });
        }
        let TokenKind::Identifier(name) = self.advance().kind else {
            return Err(self.error("expected a binding identifier or pattern"));
        };
        self.check_contextual_binding(&name)?;
        Ok(BindingPattern::Identifier(name))
    }

    /// Lower one pattern-bearing declarator into the list form, keeping a
    /// single pattern declarator on the compact `Variable` shape by way of a
    /// synthetic single-element list the caller unwraps.
    fn lower_declarator(
        pattern: BindingPattern,
        initializer: Expr,
        declarations: &mut Vec<(BindingTarget, Option<Expr>)>,
    ) {
        declarations.push((BindingTarget::Pattern(pattern), Some(initializer)));
    }

    fn function_declaration(&mut self, kind: FunctionKind) -> Result<Statement, JsError> {
        let TokenKind::Identifier(name) = self.advance().kind else {
            return Err(self.error("expected a function name"));
        };
        let (parameters, body) = self.function_tail(kind)?;
        Ok(Statement::Function {
            offset: self.previous_offset(),
            name,
            parameters,
            body,
            kind,
        })
    }

    fn if_statement(&mut self) -> Result<Statement, JsError> {
        self.require(&TokenKind::LeftParen, "expected '(' after if")?;
        let condition = self.expression()?;
        self.require(&TokenKind::RightParen, "expected ')' after if condition")?;
        let consequent = Box::new(self.statement_in(BodyPosition::IfClause)?);
        let alternate = if self.take(&TokenKind::Else) {
            Some(Box::new(self.statement_in(BodyPosition::IfClause)?))
        } else {
            None
        };
        Ok(Statement::If {
            offset: self.previous_offset(),
            condition,
            consequent,
            alternate,
        })
    }

    fn with_statement(&mut self) -> Result<Statement, JsError> {
        // Class bodies are strict code too (ECMA-262 11.2.2).
        if self.strict {
            return Err(self.error("with statement is not allowed in strict mode"));
        }
        self.require(&TokenKind::LeftParen, "expected '(' after with")?;
        let object = self.expression()?;
        self.require(&TokenKind::RightParen, "expected ')' after with object")?;
        let body = if self.at(&TokenKind::Let) {
            self.let_expression_body()?
        } else {
            let body = self.statement()?;
            if is_declaration(&body) {
                return Err(self.error("a declaration cannot be the body of a with statement"));
            }
            body
        };
        Ok(Statement::With {
            offset: self.previous_offset(),
            object,
            body: Box::new(body),
        })
    }

    /// A `with` body that starts with `let`. A lexical declaration is not a
    /// `Statement` (ECMA-262 14.11), so `let` is an identifier there and a line
    /// break ends the expression statement (ASI). `let [` can never begin an
    /// expression statement (ECMA-262 14.1.1), line break or not.
    fn let_expression_body(&mut self) -> Result<Statement, JsError> {
        let next = self.tokens.get(self.cursor + 1);
        if matches!(next.map(|token| &token.kind), Some(TokenKind::LeftBracket)) {
            return Err(self.error("let [ cannot begin a statement"));
        }
        let ends_statement = next.is_none_or(|token| {
            token.after_newline
                || matches!(
                    token.kind,
                    TokenKind::Semicolon | TokenKind::RightBrace | TokenKind::Eof
                )
        });
        if !ends_statement {
            return Err(self.error("a declaration cannot be the body of a with statement"));
        }
        self.advance();
        self.end_statement()?;
        Ok(Statement::Expression(Expr::Identifier("let".to_owned())))
    }

    /// Whether the directive prologue starting at token `index` contains
    /// `"use strict"` (ECMA-262 11.2.1). Each directive is a string literal
    /// that forms a whole expression statement.
    fn prologue_is_strict(&self, mut index: usize) -> bool {
        loop {
            let Some(TokenKind::String(text)) = self.tokens.get(index).map(|token| &token.kind)
            else {
                return false;
            };
            let next = index + 1;
            index = match self.tokens.get(next) {
                Some(Token {
                    kind: TokenKind::Semicolon,
                    ..
                }) => next + 1,
                Some(Token {
                    kind: TokenKind::RightBrace | TokenKind::Eof,
                    ..
                })
                | None => next,
                // A line break ends the directive only when the next token
                // cannot continue the expression (`"a"\n(b)` is a call).
                Some(token) if token.after_newline && !continues_expression(&token.kind) => next,
                Some(_) => return false,
            };
            if text == "use strict" {
                return true;
            }
        }
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
            let tests = if self.take(&TokenKind::Case) {
                // A case label is a full Expression, so comma-separated
                // alternatives such as `case a, b:` are legal.
                let mut tests = vec![self.assignment()?];
                while self.take(&TokenKind::Comma) {
                    tests.push(self.assignment()?);
                }
                self.require(&TokenKind::Colon, "expected ':' after case expression")?;
                tests
            } else if self.take(&TokenKind::Default) {
                self.require(&TokenKind::Colon, "expected ':' after default")?;
                Vec::new()
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
            cases.push((tests, consequent));
        }
        self.switch_depth = self.switch_depth.saturating_sub(1);
        self.require(&TokenKind::RightBrace, "expected '}' after switch")?;
        validate_declaration_conflicts(cases.iter().flat_map(|(_, body)| body), false)?;
        Ok(Statement::Switch {
            offset: self.previous_offset(),
            expression,
            cases,
        })
    }

    fn while_statement(&mut self) -> Result<Statement, JsError> {
        self.require(&TokenKind::LeftParen, "expected '(' after while")?;
        let condition = self.expression()?;
        self.require(&TokenKind::RightParen, "expected ')' after while condition")?;
        self.loop_depth = self.loop_depth.saturating_add(1);
        let body = self.statement_in(BodyPosition::LoopBody);
        self.loop_depth = self.loop_depth.saturating_sub(1);
        Ok(Statement::While {
            offset: self.previous_offset(),
            condition,
            body: Box::new(body?),
        })
    }

    fn do_while_statement(&mut self) -> Result<Statement, JsError> {
        self.loop_depth = self.loop_depth.saturating_add(1);
        let body = self.statement_in(BodyPosition::LoopBody);
        self.loop_depth = self.loop_depth.saturating_sub(1);
        let body = body?;
        self.require(&TokenKind::While, "expected 'while' after do body")?;
        self.require(&TokenKind::LeftParen, "expected '(' after while")?;
        let condition = self.expression()?;
        self.require(&TokenKind::RightParen, "expected ')' after while condition")?;
        let _ = self.take(&TokenKind::Semicolon);
        Ok(Statement::DoWhile {
            offset: self.previous_offset(),
            condition: Box::new(condition),
            body: Box::new(body),
        })
    }

    fn for_statement(&mut self) -> Result<Statement, JsError> {
        // `for await` is only a loop head inside an async body (ECMA-262 §14.7.5).
        let is_await = self.in_async
            && matches!(&self.current().kind, TokenKind::Identifier(name) if name == "await");
        if is_await {
            self.advance();
        }
        let statement = self.for_statement_head(is_await)?;
        if is_await && !matches!(statement, Statement::ForOf { .. }) {
            return Err(self.error("expected 'of' after for await"));
        }
        Ok(statement)
    }

    fn for_statement_head(&mut self, is_await: bool) -> Result<Statement, JsError> {
        self.require(&TokenKind::LeftParen, "expected '(' after for")?;
        self.no_in = true;
        let initializer = if self.take(&TokenKind::Semicolon) {
            None
        } else if let Some(kind) = self.take_variable_kind() {
            let declaration_start = self.cursor;
            let pattern = if self.at(&TokenKind::LeftBrace) || self.at(&TokenKind::LeftBracket) {
                Some(self.binding_pattern()?)
            } else {
                let TokenKind::Identifier(name) = self.advance().kind else {
                    return Err(self.error("expected an identifier after declaration keyword"));
                };
                self.check_contextual_binding(&name)?;
                Some(BindingPattern::Identifier(name))
            };
            if self.take(&TokenKind::In) {
                self.no_in = false;
                let iterable = self.expression()?;
                self.require(&TokenKind::RightParen, "expected ')' after for-in clauses")?;
                self.loop_depth = self.loop_depth.saturating_add(1);
                let body = self.statement_in(BodyPosition::LoopBody);
                self.loop_depth = self.loop_depth.saturating_sub(1);
                return Ok(Statement::ForIn {
                    offset: self.previous_offset(),
                    kind,
                    name: match pattern.expect("pattern parsed") {
                        BindingPattern::Identifier(name) => name,
                        _ => return Err(self.error("for-in destructuring is not supported")),
                    },
                    iterable,
                    body: Box::new(body?),
                });
            }
            // An escaped `of` is not the contextual keyword (ECMA-262 12.7.2).
            if self.at_contextual("of") && !self.current().escaped {
                self.advance();
                self.no_in = false;
                let iterable = self.expression()?;
                self.require(&TokenKind::RightParen, "expected ')' after for-of clauses")?;
                self.loop_depth = self.loop_depth.saturating_add(1);
                let body = self.statement_in(BodyPosition::LoopBody);
                self.loop_depth = self.loop_depth.saturating_sub(1);
                let body = body?;
                let pattern = pattern.expect("pattern parsed");
                let name = match pattern {
                    BindingPattern::Identifier(name) => name,
                    pattern => {
                        // The loop variable is one temporary, and the pattern is
                        // destructured from it at the top of the body. The
                        // temporary is a `var` so it is shared across
                        // iterations; the pattern's own names keep the declared
                        // kind, which is what gives `for (const [k] of ..)` its
                        // per-iteration binding.
                        let temporary = format!("\0for_of_{}", declaration_start);
                        let mut declarations = Vec::new();
                        Self::lower_declarator(
                            pattern,
                            Expr::Identifier(temporary.clone()),
                            &mut declarations,
                        );
                        let body = Statement::Block(vec![
                            Statement::VariableList {
                                offset: self.previous_offset(),
                                kind,
                                declarations,
                            },
                            body,
                        ]);
                        return Ok(Statement::ForOf {
                            offset: self.previous_offset(),
                            kind: VariableKind::Var,
                            name: temporary,
                            iterable,
                            body: Box::new(body),
                            is_await,
                        });
                    }
                };
                return Ok(Statement::ForOf {
                    offset: self.previous_offset(),
                    kind,
                    name,
                    iterable,
                    body: Box::new(body),
                    is_await,
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
                let body = self.statement_in(BodyPosition::LoopBody);
                self.loop_depth = self.loop_depth.saturating_sub(1);
                return Ok(Statement::ForInExpr {
                    offset: self.previous_offset(),
                    target: expression,
                    iterable,
                    body: Box::new(body?),
                });
            }
            if self.at_contextual("of") && !self.current().escaped {
                // `for (LHS of iterable)` with an assignment target, which may be
                // a destructuring pattern. The loop binds one temporary, and the
                // body first assigns the pattern from it, so the target is
                // evaluated and written each iteration as the spec requires.
                self.advance();
                self.no_in = false;
                self.validate_assignment_target(&expression)?;
                let iterable = self.assignment()?;
                self.require(&TokenKind::RightParen, "expected ')' after for-of clauses")?;
                self.loop_depth = self.loop_depth.saturating_add(1);
                let body = self.statement_in(BodyPosition::LoopBody);
                self.loop_depth = self.loop_depth.saturating_sub(1);
                let offset = self.previous_offset();
                let temporary = format!("\0for_of_expr_{}", self.cursor);
                let assign = Statement::Expression(Expr::Assignment {
                    target: Box::new(expression),
                    value: Box::new(Expr::Identifier(temporary.clone())),
                    offset,
                    parenthesized_target: false,
                });
                return Ok(Statement::ForOf {
                    offset,
                    kind: VariableKind::Var,
                    name: temporary,
                    iterable,
                    body: Box::new(Statement::Block(vec![assign, body?])),
                    is_await,
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
        let body = self.statement_in(BodyPosition::LoopBody);
        self.loop_depth = self.loop_depth.saturating_sub(1);
        Ok(Statement::For {
            offset: self.previous_offset(),
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
                let target = if self.at(&TokenKind::LeftBrace) || self.at(&TokenKind::LeftBracket) {
                    BindingTarget::Pattern(self.binding_pattern()?)
                } else {
                    let TokenKind::Identifier(name) = self.advance().kind else {
                        return Err(self.error("expected catch parameter"));
                    };
                    BindingTarget::Name(name)
                };
                self.require(&TokenKind::RightParen, "expected ')' after catch parameter")?;
                Some(target)
            } else {
                None
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
            offset: self.previous_offset(),
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

    /// `yield`, `yield value`, `yield* iterable` (ECMA-262 15.5). A
    /// `YieldExpression` is an `AssignmentExpression`, so it can never be the
    /// operand of an operator: `void yield` is not a `YieldExpression`.
    fn yield_expression(&mut self) -> Result<Expr, JsError> {
        if self.in_parameters {
            return Err(self.error("yield expression is not allowed in formal parameters"));
        }
        let offset = self.current().offset;
        self.advance();
        let delegate = !self.current().after_newline && self.take(&TokenKind::Star);
        let terminated = self.current().after_newline
            || matches!(
                self.current().kind,
                TokenKind::Semicolon
                    | TokenKind::Comma
                    | TokenKind::RightParen
                    | TokenKind::RightBracket
                    | TokenKind::RightBrace
                    | TokenKind::Colon
                    | TokenKind::Question
                    | TokenKind::Dot
                    | TokenKind::Eof
            );
        let argument = if terminated && !delegate {
            None
        } else {
            Some(Box::new(self.assignment()?))
        };
        Ok(Expr::Yield {
            argument,
            delegate,
            offset,
        })
    }

    fn assignment(&mut self) -> Result<Expr, JsError> {
        if self.in_generator
            && matches!(&self.current().kind, TokenKind::Identifier(name) if name == "yield")
        {
            return self.yield_expression();
        }
        if let Some(arrow) = self.arrow_function()? {
            return Ok(arrow);
        }
        let parenthesized_target = self.at(&TokenKind::LeftParen);
        let target = self.conditional()?;
        if self.take(&TokenKind::Equal) {
            self.assignment_value(target, None, parenthesized_target)
        } else if self.take(&TokenKind::PlusEqual) {
            self.assignment_value(target, Some(BinaryOp::Add), parenthesized_target)
        } else if self.take(&TokenKind::MinusEqual) {
            self.assignment_value(target, Some(BinaryOp::Subtract), parenthesized_target)
        } else if self.take(&TokenKind::StarStarEqual) {
            self.assignment_value(target, Some(BinaryOp::Exponentiate), parenthesized_target)
        } else if self.take(&TokenKind::StarEqual) {
            self.assignment_value(target, Some(BinaryOp::Multiply), parenthesized_target)
        } else if self.take(&TokenKind::SlashEqual) {
            self.assignment_value(target, Some(BinaryOp::Divide), parenthesized_target)
        } else if self.take(&TokenKind::PercentEqual) {
            self.assignment_value(target, Some(BinaryOp::Remainder), parenthesized_target)
        } else if self.take(&TokenKind::AmpersandEqual) {
            self.assignment_value(target, Some(BinaryOp::BitwiseAnd), parenthesized_target)
        } else if self.take(&TokenKind::CaretEqual) {
            self.assignment_value(target, Some(BinaryOp::BitwiseXor), parenthesized_target)
        } else if self.take(&TokenKind::PipeEqual) {
            self.assignment_value(target, Some(BinaryOp::BitwiseOr), parenthesized_target)
        } else if self.take(&TokenKind::LeftShiftEqual) {
            self.assignment_value(target, Some(BinaryOp::LeftShift), parenthesized_target)
        } else if self.take(&TokenKind::RightShiftEqual) {
            self.assignment_value(target, Some(BinaryOp::RightShift), parenthesized_target)
        } else if self.take(&TokenKind::UnsignedRightShiftEqual) {
            self.assignment_value(
                target,
                Some(BinaryOp::UnsignedRightShift),
                parenthesized_target,
            )
        } else if self.take(&TokenKind::AndAndEqual) {
            self.logical_assignment_value(target, BinaryOp::LogicalAnd)
        } else if self.take(&TokenKind::OrOrEqual) {
            self.logical_assignment_value(target, BinaryOp::LogicalOr)
        } else if self.take(&TokenKind::QuestionQuestionEqual) {
            self.logical_assignment_value(target, BinaryOp::Nullish)
        } else {
            Ok(target)
        }
    }

    /// `x &&= y`, `x ||= y`, `x ??= y`: a short-circuit assignment whose
    /// right-hand side only evaluates (and writes) when the current value
    /// fails to short-circuit.
    fn logical_assignment_value(
        &mut self,
        target: Expr,
        operator: BinaryOp,
    ) -> Result<Expr, JsError> {
        self.validate_assignment_target(&target)?;
        let value = self.assignment()?;
        Ok(Expr::LogicalAssignment {
            offset: self.previous_offset(),
            target: Box::new(target),
            operator,
            value: Box::new(value),
        })
    }

    fn arrow_function(&mut self) -> Result<Option<Expr>, JsError> {
        let checkpoint = self.cursor;
        let mut is_async_arrow = false;
        // Async arrows have the same callable shape in this synchronous
        // runtime; consume the marker while retaining their parameter/body.
        if matches!(&self.current().kind, TokenKind::Identifier(name) if name == "async")
            && matches!(
                self.tokens.get(self.cursor + 1).map(|token| &token.kind),
                Some(TokenKind::LeftParen | TokenKind::Identifier(_))
            )
        {
            self.advance();
            is_async_arrow = true;
        }
        let mut patterns = Vec::new();
        let mut markers: Vec<Statement> = Vec::new();
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
                    if self.take(&TokenKind::Ellipsis) {
                        let TokenKind::Identifier(parameter) = self.advance().kind else {
                            self.cursor = checkpoint;
                            return Ok(None);
                        };
                        parameters.push(format!("{PARAMETER_REST_MARKER}{parameter}"));
                        break;
                    }
                    let parameter =
                        if self.at(&TokenKind::LeftBrace) || self.at(&TokenKind::LeftBracket) {
                            let Ok(pattern) = self.binding_pattern() else {
                                self.cursor = checkpoint;
                                return Ok(None);
                            };
                            let temporary = format!("\0arrow_param_{}", parameters.len());
                            patterns.push((parameters.len(), temporary.clone(), pattern));
                            temporary
                        } else {
                            let TokenKind::Identifier(parameter) = self.advance().kind else {
                                self.cursor = checkpoint;
                                return Ok(None);
                            };
                            parameter
                        };
                    let has_default = self.take(&TokenKind::Equal);
                    if has_default {
                        let value = self.assignment()?;
                        markers.push(Statement::ParameterDefault {
                            index: parameters.len(),
                            value,
                            offset: self.previous_offset(),
                        });
                    }
                    parameters.push(if has_default {
                        format!("{PARAMETER_DEFAULT_MARKER}{parameter}")
                    } else {
                        parameter
                    });
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

        // An arrow function has no `yield`, and its `await` is its own: it is
        // an operator only in an async arrow.
        let previous_async = std::mem::replace(&mut self.in_async, is_async_arrow);
        let previous_generator = std::mem::replace(&mut self.in_generator, false);
        let previous_parameters = std::mem::replace(&mut self.in_parameters, false);
        // An arrow body is a jump target of its own, like any function body.
        let previous_labels = std::mem::take(&mut self.labels);
        let previous_switch_depth = std::mem::replace(&mut self.switch_depth, 0);
        // The parameters above keep the static block's `await` restriction; the
        // body does not (ECMA-262 15.7.1).
        let previous_static_await = std::mem::replace(&mut self.static_block_await, false);
        let body = if self.take(&TokenKind::LeftBrace) {
            let previous_function_depth = self.function_depth;
            let previous_loop_depth = self.loop_depth;
            let previous_strict = self.strict;
            self.function_depth = self.function_depth.saturating_add(1);
            self.loop_depth = 0;
            self.strict = previous_strict || self.prologue_is_strict(self.cursor);
            let body = self.statement_list(true);
            self.function_depth = previous_function_depth;
            self.loop_depth = previous_loop_depth;
            self.strict = previous_strict;
            body.and_then(|body| {
                self.require(
                    &TokenKind::RightBrace,
                    "expected '}' after arrow function body",
                )?;
                Ok(body)
            })
        } else {
            self.assignment()
                .map(|value| vec![Statement::Return(Some(value))])
        };
        self.in_async = previous_async;
        self.in_generator = previous_generator;
        self.in_parameters = previous_parameters;
        self.labels = previous_labels;
        self.switch_depth = previous_switch_depth;
        self.static_block_await = previous_static_await;
        let mut body = body?;
        // An arrow's parameter names are never duplicated, even in sloppy code
        // (ECMA-262 15.3.1); destructured names count too.
        let mut seen = BTreeSet::new();
        let mut names: Vec<String> = parameters
            .iter()
            .map(|parameter| parameter_binding_name(parameter).to_owned())
            .collect();
        for (_, _, pattern) in &patterns {
            collect_binding_names(pattern, &mut names);
        }
        if names.iter().any(|name| !seen.insert(name.as_str())) {
            return Err(self.error("duplicate parameter name in an arrow function"));
        }
        if !is_simple_parameter_list(&parameters) && has_use_strict_directive(&body) {
            return Err(self.error("'use strict' is not allowed with non-simple parameters"));
        }
        if has_use_strict_directive(&body) {
            validate_strict_parameters(&parameters)?;
            validate_strict_statements(&body)?;
        }
        let offset = self.previous_offset();
        for (index, temporary, pattern) in patterns {
            let mut declarations = Vec::new();
            Self::lower_declarator(pattern, Expr::Identifier(temporary), &mut declarations);
            markers.push(Statement::ParameterPattern {
                index,
                declarations,
                offset,
            });
        }
        if !markers.is_empty() {
            markers.extend(body);
            body = markers;
        }
        validate_declaration_conflicts(&body, true)?;
        Ok(Some(Expr::Arrow {
            offset: self.previous_offset(),
            parameters,
            body,
            is_async: is_async_arrow,
        }))
    }

    fn assignment_value(
        &mut self,
        target: Expr,
        operator: Option<BinaryOp>,
        parenthesized_target: bool,
    ) -> Result<Expr, JsError> {
        // Only a plain `=` takes a destructuring pattern (ECMA-262 13.15).
        if operator.is_some() && !is_simple_target(&target) {
            return Err(self.error("invalid compound assignment target"));
        }
        self.validate_assignment_target(&target)?;
        let value = self.assignment()?;
        Ok(match operator {
            Some(operator) => Expr::CompoundAssignment {
                offset: self.previous_offset(),
                target: Box::new(target),
                operator,
                value: Box::new(value),
            },
            None => Expr::Assignment {
                offset: self.previous_offset(),
                target: Box::new(target),
                value: Box::new(value),
                parenthesized_target,
            },
        })
    }

    fn validate_assignment_target(&self, target: &Expr) -> Result<(), JsError> {
        match target {
            Expr::Array(_) | Expr::Object(_) => self.validate_destructuring_target(target, false),
            _ if is_simple_target(target) => self.validate_simple_target(target),
            _ => Err(self.error("invalid assignment target")),
        }
    }

    /// In strict code `eval` and `arguments` are not assignment targets
    /// (ECMA-262 13.15.1.1), whether assigned alone or inside a pattern.
    fn validate_simple_target(&self, target: &Expr) -> Result<(), JsError> {
        match target {
            Expr::Identifier(name) if self.strict && (name == "eval" || name == "arguments") => {
                Err(self.error("assignment to eval or arguments in strict code"))
            }
            _ => Ok(()),
        }
    }

    /// A target of a destructuring assignment pattern (ECMA-262 13.15.5). A
    /// default (`target = value`) is allowed only where the grammar takes an
    /// `AssignmentElement`, not in a rest element.
    fn validate_destructuring_target(
        &self,
        target: &Expr,
        allow_default: bool,
    ) -> Result<(), JsError> {
        match target {
            Expr::Array(elements) => self.validate_array_pattern(elements),
            Expr::Object(properties) => self.validate_object_pattern(properties),
            Expr::Assignment { target: inner, .. } if allow_default => {
                self.validate_destructuring_target(inner, false)
            }
            _ if is_simple_target(target) => self.validate_simple_target(target),
            _ => Err(self.error("invalid destructuring assignment target")),
        }
    }

    /// Array pattern elements: a rest element is the last one and has no
    /// initializer. An elision is a hole, which the parser records as `undefined`.
    fn validate_array_pattern(&self, elements: &[Expr]) -> Result<(), JsError> {
        for (index, element) in elements.iter().enumerate() {
            match element {
                Expr::Spread(rest) => {
                    if index + 1 != elements.len() {
                        return Err(self.error("rest element must be last in a pattern"));
                    }
                    self.validate_destructuring_target(rest, false)?;
                }
                Expr::Literal(JsValue::Undefined) => {}
                _ => self.validate_destructuring_target(element, true)?,
            }
        }
        Ok(())
    }

    /// Object pattern properties: a rest property is the last one and takes a
    /// simple target only.
    fn validate_object_pattern(&self, properties: &[ObjectProperty]) -> Result<(), JsError> {
        for (index, property) in properties.iter().enumerate() {
            if property.method || property.accessor.is_some() {
                return Err(self.error("a method cannot be an assignment target"));
            }
            if property.key == PropertyKey::Spread {
                if index + 1 != properties.len() {
                    return Err(self.error("rest property must be last in a pattern"));
                }
                if !is_simple_target(&property.value) {
                    return Err(self.error("rest property target must be a simple target"));
                }
            } else {
                self.validate_destructuring_target(&property.value, true)?;
            }
        }
        Ok(())
    }

    fn conditional(&mut self) -> Result<Expr, JsError> {
        let condition = self.nullish()?;
        if !self.take(&TokenKind::Question) {
            return Ok(condition);
        }
        let consequent = self.assignment()?;
        self.require(&TokenKind::Colon, "expected ':' in conditional expression")?;
        let alternate = self.assignment()?;
        Ok(Expr::Conditional {
            offset: self.previous_offset(),
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

    /// `??` binds tighter than `||` in this parser's table; the spec forbids
    /// mixing them unparenthesized, which this engine accepts.
    fn nullish(&mut self) -> Result<Expr, JsError> {
        self.binary_level(
            Self::logical_or,
            &[(&TokenKind::QuestionQuestion, BinaryOp::Nullish)],
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
            Self::exponent,
            &[
                (&TokenKind::Star, BinaryOp::Multiply),
                (&TokenKind::Slash, BinaryOp::Divide),
                (&TokenKind::Percent, BinaryOp::Remainder),
            ],
        )
    }

    fn exponent(&mut self) -> Result<Expr, JsError> {
        let left = self.unary()?;
        if !self.take(&TokenKind::StarStar) {
            return Ok(left);
        }
        Ok(Expr::Binary {
            offset: self.previous_offset(),
            operator: BinaryOp::Exponentiate,
            left: Box::new(left),
            right: Box::new(self.exponent()?),
        })
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
                offset: self.previous_offset(),
                operator: *operator,
                left: Box::new(expression),
                right: Box::new(right),
            };
        }
        Ok(expression)
    }

    fn unary(&mut self) -> Result<Expr, JsError> {
        if let TokenKind::PrivateName(name) = self.current().kind.clone()
            && matches!(
                self.tokens.get(self.cursor + 1).map(|token| &token.kind),
                Some(TokenKind::In)
            )
        {
            let offset = self.current().offset;
            self.private_reference(&name, offset)?;
            self.advance();
            self.advance();
            let object = self.unary()?;
            return Ok(Expr::PrivateIn {
                name,
                object: Box::new(object),
                offset,
            });
        }
        if self.in_async
            && matches!(&self.current().kind, TokenKind::Identifier(name) if name == "await")
        {
            // `await` has unary-expression precedence (ECMA-262 §15.8).
            if self.in_parameters {
                return Err(self.error("await expression is not allowed in formal parameters"));
            }
            self.advance();
            return Ok(Expr::Await(Box::new(self.unary()?)));
        }
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
                offset: self.previous_offset(),
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
            let operand = self.unary()?;
            // ECMA-262 13.5.1.1: a private reference cannot be deleted. Parentheses
            // leave no node, so `delete (this.#x)` is caught here too.
            if operator == UnaryOp::Delete && matches!(operand, Expr::PrivateMember { .. }) {
                return Err(self.error("private fields cannot be deleted"));
            }
            return Ok(Expr::Unary {
                offset: self.previous_offset(),
                operator,
                operand: Box::new(operand),
            });
        }
        if self.take(&TokenKind::New) {
            if self.take(&TokenKind::Dot) {
                let property = self.property_name()?;
                if property != "target" {
                    return Err(self.error("expected 'new.target'"));
                }
                // `new.target` is only valid in function code, eval code
                // contained in a function, and class field/static-block
                // initializers.
                if self.function_depth == 0 && self.private_references.is_empty() {
                    return Err(self.error("new.target is only allowed inside functions"));
                }
                return self.postfix_tail(Expr::NewTarget);
            }
            // `new` binds to a whole member chain (`new A.B.C(...)`), so
            // consume dots/computed members BEFORE the argument list.
            // A bare import call is not a MemberExpression, so `new` cannot apply
            // to it. A parenthesized one is a PrimaryExpression and is allowed.
            let starts_with_import =
                matches!(&self.current().kind, TokenKind::Identifier(name) if name == "import");
            let mut target = self.primary()?;
            if starts_with_import && is_import_call(&target) {
                return Err(self.error("'new' cannot be applied to an import call"));
            }
            loop {
                if self.take(&TokenKind::Dot) {
                    let property = self.property_name()?;
                    target = Expr::Member {
                        offset: self.previous_offset(),
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
                        offset: self.previous_offset(),
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
                offset: self.previous_offset(),
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
        // [no LineTerminator here] (ECMA-262 13.4): a line terminator before
        // the operator ends the statement, so `a` then `++b` is two statements.
        let operator = if self.current().after_newline {
            None
        } else if self.take(&TokenKind::PlusPlus) {
            Some(BinaryOp::Add)
        } else if self.take(&TokenKind::MinusMinus) {
            Some(BinaryOp::Subtract)
        } else {
            None
        };
        if let Some(operator) = operator {
            self.validate_assignment_target(&expression)?;
            Ok(Expr::Update {
                offset: self.previous_offset(),
                target: Box::new(expression),
                operator,
                prefix: false,
            })
        } else {
            Ok(expression)
        }
    }

    fn postfix_tail(&mut self, mut expression: Expr) -> Result<Expr, JsError> {
        let mut optional = false;
        loop {
            if self.take(&TokenKind::QuestionDot) {
                optional = true;
                expression = Expr::OptionalGuard(Box::new(expression));
                if self.take(&TokenKind::LeftParen) {
                    let arguments = self.arguments_after_left_paren()?;
                    expression = Expr::Call {
                        offset: self.previous_offset(),
                        callee: Box::new(expression),
                        arguments,
                    };
                } else if self.take(&TokenKind::LeftBracket) {
                    let property = self.assignment()?;
                    self.require(
                        &TokenKind::RightBracket,
                        "expected ']' after computed property",
                    )?;
                    expression = Expr::ComputedMember {
                        offset: self.previous_offset(),
                        object: Box::new(expression),
                        property: Box::new(property),
                    };
                } else if let TokenKind::PrivateName(name) = self.current().kind.clone() {
                    self.private_reference(&name, self.current().offset)?;
                    self.advance();
                    expression = Expr::PrivateMember {
                        offset: self.previous_offset(),
                        object: Box::new(expression),
                        name,
                    };
                } else {
                    let property = self.property_name()?;
                    expression = Expr::Member {
                        offset: self.previous_offset(),
                        object: Box::new(expression),
                        property,
                    };
                }
            } else if self.take(&TokenKind::Dot) {
                if let TokenKind::PrivateName(name) = self.current().kind.clone() {
                    self.private_reference(&name, self.current().offset)?;
                    self.advance();
                    expression = Expr::PrivateMember {
                        offset: self.previous_offset(),
                        object: Box::new(expression),
                        name,
                    };
                    continue;
                }
                let property = self.property_name()?;
                expression = Expr::Member {
                    offset: self.previous_offset(),
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
                    offset: self.previous_offset(),
                    object: Box::new(expression),
                    property: Box::new(property),
                };
            } else if self.take(&TokenKind::LeftParen) {
                let arguments = self.arguments_after_left_paren()?;
                expression = Expr::Call {
                    offset: self.previous_offset(),
                    callee: Box::new(expression),
                    arguments,
                };
            } else if matches!(self.current().kind, TokenKind::Template(_)) {
                if optional {
                    return Err(self.error("tagged templates are not allowed in an optional chain"));
                }
                // ECMA-262 13.3.11 `TaggedTemplate`: the tag is handed a
                // template object and the substitution values, not the
                // concatenated string an untagged template literal produces.
                let token = self.advance();
                let TokenKind::Template(parts) = token.kind else {
                    unreachable!("checked above")
                };
                expression = self.tagged_template(expression, parts, token.offset)?;
            } else {
                break;
            }
        }
        if optional {
            expression = Expr::OptionalChain(Box::new(expression));
        }
        Ok(expression)
    }

    /// Runs `parse` with the `in` operator allowed. A bracketed context such
    /// as call arguments or an import call is `[+In]` even inside a for-loop
    /// initializer, where a bare declaration initializer is `[~In]`.
    fn in_allowed<T>(
        &mut self,
        parse: impl FnOnce(&mut Self) -> Result<T, JsError>,
    ) -> Result<T, JsError> {
        let previous_no_in = std::mem::replace(&mut self.no_in, false);
        let result = parse(self);
        self.no_in = previous_no_in;
        result
    }

    fn arguments_after_left_paren(&mut self) -> Result<Vec<Expr>, JsError> {
        let mut arguments = Vec::new();
        if !self.at(&TokenKind::RightParen) {
            loop {
                let argument = self.in_allowed(|parser| {
                    if parser.take(&TokenKind::Ellipsis) {
                        Ok(Expr::Spread(Box::new(parser.assignment()?)))
                    } else {
                        parser.assignment()
                    }
                })?;
                arguments.push(argument);
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
            TokenKind::Identifier(name) if name == "async" && self.at(&TokenKind::Function) => {
                self.advance();
                self.function_expression(true)
            }
            TokenKind::Identifier(name) if name == "class" => {
                // An anonymous class may be followed by `extends`; only a
                // real identifier names the class.
                let name = if let TokenKind::Identifier(name) = self.current().kind.clone()
                    && name != "extends"
                {
                    self.check_class_name(&name)?;
                    self.advance();
                    Some(name)
                } else {
                    None
                };
                let (super_class, elements) = self.class_tail()?;
                Ok(Expr::Class {
                    offset: self.previous_offset(),
                    name,
                    super_class,
                    elements,
                })
            }
            TokenKind::Identifier(name)
                if name == "import"
                    && self.module.is_some()
                    && self.at(&TokenKind::Dot)
                    && matches!(
                        self.tokens.get(self.cursor + 1).map(|next| &next.kind),
                        Some(TokenKind::Identifier(meta)) if meta == "meta"
                    ) =>
            {
                self.advance();
                self.advance();
                Ok(Expr::Identifier(IMPORT_META_BINDING.to_owned()))
            }
            // An escaped spelling is not the keyword (ECMA-262 12.7.2), so it is
            // refused wherever the keyword would start an import form.
            TokenKind::Identifier(name) if name == "import" && token.escaped => Err(
                JsError::syntax("'import' must not contain escape characters", token.offset),
            ),
            // `import(specifier[, options][,])` (ECMA-262 13.3.10). It is a call
            // of the host's `import`, with one or two arguments and no spread.
            TokenKind::Identifier(name) if name == "import" && self.at(&TokenKind::LeftParen) => {
                self.advance();
                let arguments = self.in_allowed(|parser| {
                    let mut arguments = vec![parser.assignment()?];
                    if parser.take(&TokenKind::Comma) && !parser.at(&TokenKind::RightParen) {
                        arguments.push(parser.assignment()?);
                        let _ = parser.take(&TokenKind::Comma);
                    }
                    Ok(arguments)
                })?;
                self.require(
                    &TokenKind::RightParen,
                    "expected ')' after import() arguments",
                )?;
                Ok(Expr::Call {
                    offset: self.previous_offset(),
                    callee: Box::new(Expr::Identifier(name)),
                    arguments,
                })
            }
            // `import.defer(specifier)` and `import.source(specifier)` (the
            // source-phase import proposals). They take exactly one argument, with
            // no trailing comma, and reach the host's `import` hook like `import()`.
            // Any other `import.` form is a syntax error, and `import.meta` is only
            // valid in modules. A bare `import` is a reserved word.
            TokenKind::Identifier(name) if name == "import" && self.at(&TokenKind::Dot) => {
                self.advance();
                let phase_token = self.advance();
                let phase = match &phase_token.kind {
                    TokenKind::Identifier(phase) if !phase_token.escaped => phase.clone(),
                    _ => String::new(),
                };
                if phase == "meta" {
                    return Err(JsError::syntax(
                        "import.meta is only valid in modules",
                        token.offset,
                    ));
                }
                if !matches!(phase.as_str(), "defer" | "source") || !self.at(&TokenKind::LeftParen)
                {
                    return Err(JsError::syntax(
                        "'import.' must be followed by 'meta', 'defer(' or 'source('",
                        token.offset,
                    ));
                }
                self.advance();
                let specifier = self.in_allowed(Self::assignment)?;
                self.require(
                    &TokenKind::RightParen,
                    "expected ')' after import.defer() or import.source() argument",
                )?;
                Ok(Expr::Call {
                    offset: self.previous_offset(),
                    callee: Box::new(Expr::Member {
                        object: Box::new(Expr::Identifier(name)),
                        property: phase,
                        offset: token.offset,
                    }),
                    arguments: vec![specifier],
                })
            }
            TokenKind::Identifier(name) if name == "import" => Err(JsError::syntax(
                "'import' must be followed by '(' or '.'",
                token.offset,
            )),
            TokenKind::Identifier(name) if name == "super" => self.super_expression(token.offset),
            TokenKind::PrivateName(name) => Err(JsError::syntax(
                format!("private name #{name} must be followed by 'in'"),
                token.offset,
            )),
            TokenKind::Identifier(name) if ALWAYS_RESERVED_NAMES.contains(&name.as_str()) => Err(
                JsError::syntax(format!("{name} is a reserved word"), token.offset),
            ),
            // `yield` in generator code is always the operator (handled by
            // `assignment`), so reaching it here means it is not an operand.
            TokenKind::Identifier(name) if self.in_generator && name == "yield" => {
                Err(JsError::syntax(
                    "yield is not a valid identifier reference in a generator",
                    token.offset,
                ))
            }
            TokenKind::Identifier(name)
                if (self.static_block_await && name == "await")
                    || (self.static_block && name == "arguments") =>
            {
                Err(JsError::syntax(
                    format!("{name} is not allowed in a class static block"),
                    token.offset,
                ))
            }
            TokenKind::Identifier(name) => Ok(Expr::Identifier(name)),
            TokenKind::This => Ok(Expr::This),
            TokenKind::String(value) => Ok(Expr::Literal(JsValue::String(value))),
            TokenKind::RegexLiteral { pattern, flags } => {
                // ECMA-262 §22.2.1: a regular expression literal whose pattern or
                // flags are early errors is a SyntaxError at parse time.
                crate::regex::validate(&pattern, &flags).map_err(|error| {
                    JsError::syntax(
                        format!("invalid regular expression /{pattern}/{flags}: {error}"),
                        token.offset,
                    )
                })?;
                Ok(Expr::RegexLiteral {
                    offset: self.previous_offset(),
                    pattern,
                    flags,
                })
            }
            TokenKind::Template(parts) => self.template_literal(parts, token.offset),
            TokenKind::Number(value) => Ok(Expr::Literal(JsValue::Number(value))),
            TokenKind::BigInt(value) => Ok(Expr::Literal(JsValue::BigInt(value))),
            TokenKind::True => Ok(Expr::Literal(JsValue::Boolean(true))),
            TokenKind::False => Ok(Expr::Literal(JsValue::Boolean(false))),
            TokenKind::Null => Ok(Expr::Literal(JsValue::Null)),
            TokenKind::Undefined => Ok(Expr::Literal(JsValue::Undefined)),
            TokenKind::Function => self.function_expression(false),
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

    /// `super` in primary position: `super(...)`, `super.name`, or
    /// `super[expr]`. A following call/member chain is handled by the
    /// postfix tail, which sees the resulting value expression.
    fn super_expression(&mut self, offset: usize) -> Result<Expr, JsError> {
        if self.at(&TokenKind::LeftParen) {
            if !self.super_call_allowed {
                return Err(JsError::syntax(
                    "'super()' is only valid in a derived class constructor",
                    offset,
                ));
            }
            self.advance();
            let arguments = self.arguments_after_left_paren()?;
            return Ok(Expr::SuperCall { arguments, offset });
        }
        if !self.super_property_allowed {
            return Err(JsError::syntax(
                "'super' property access is only valid in a method",
                offset,
            ));
        }
        if self.take(&TokenKind::Dot) {
            let property = self.property_name()?;
            return Ok(Expr::SuperMember { property, offset });
        }
        if self.take(&TokenKind::LeftBracket) {
            let property = self.assignment()?;
            self.require(
                &TokenKind::RightBracket,
                "expected ']' after computed super property",
            )?;
            return Ok(Expr::SuperComputedMember {
                property: Box::new(property),
                offset,
            });
        }
        Err(JsError::syntax(
            "'super' must be followed by '(' or a property access",
            offset,
        ))
    }

    /// Parse the `[extends Base] { elements }` tail shared by class
    /// declarations and expressions, with the class name already consumed.
    fn class_tail(&mut self) -> Result<(Option<Box<Expr>>, Vec<ClassElement>), JsError> {
        // All parts of a class, including its heritage, are strict code (ECMA-262 11.2.2).
        let previous_strict = std::mem::replace(&mut self.strict, true);
        let result = self.class_tail_body();
        self.strict = previous_strict;
        result
    }

    fn class_tail_body(&mut self) -> Result<(Option<Box<Expr>>, Vec<ClassElement>), JsError> {
        let super_class = if matches!(&self.current().kind, TokenKind::Identifier(value) if value == "extends")
        {
            self.advance();
            // `extends` takes a LeftHandSideExpression: member chains and
            // calls (`extends mixin(A)`), never a bare `new`.
            Some(Box::new(self.postfix()?))
        } else {
            None
        };
        self.require(&TokenKind::LeftBrace, "expected '{' after class header")?;
        // The heritage above is evaluated outside this class's private
        // environment, so its references were recorded for the outer scope.
        self.private_references.push(Vec::new());
        let elements = self.class_elements(super_class.is_some());
        let references = self.private_references.pop().unwrap_or_default();
        let elements = elements?;
        self.require(&TokenKind::RightBrace, "expected '}' after class body")?;
        validate_class_elements(&elements)?;
        // Every part of a class is strict code, whatever the enclosing script.
        validate_strict_class(super_class.as_deref(), &elements)?;
        self.close_private_scope(&elements, references)?;
        Ok((super_class, elements))
    }

    /// The members of a class body up to, not including, its closing brace.
    fn class_elements(&mut self, derived: bool) -> Result<Vec<ClassElement>, JsError> {
        let mut elements = Vec::new();
        while !self.at(&TokenKind::RightBrace) {
            if self.at(&TokenKind::Eof) {
                return Err(self.error("unterminated class body"));
            }
            if self.take(&TokenKind::Semicolon) {
                continue;
            }
            elements.push(self.class_element(derived)?);
        }
        Ok(elements)
    }

    /// Record a `#name` reference in the innermost class body. Outside every
    /// class body there is nothing that could declare it.
    fn private_reference(&mut self, name: &str, offset: usize) -> Result<(), JsError> {
        match self.private_references.last_mut() {
            Some(references) => {
                references.push((name.to_owned(), offset));
                Ok(())
            }
            None => Err(JsError::syntax(
                "private names are only allowed in class bodies",
                offset,
            )),
        }
    }

    /// Resolve the references a finished class body made. Names it declares
    /// are satisfied here; the rest belong to the enclosing class body, and
    /// are an early error when no class body encloses this one.
    fn close_private_scope(
        &mut self,
        elements: &[ClassElement],
        references: Vec<(String, usize)>,
    ) -> Result<(), JsError> {
        let declared: BTreeSet<&str> = elements
            .iter()
            .filter_map(|element| match &element.key {
                PropertyKey::Private(name) => Some(name.as_str()),
                _ => None,
            })
            .collect();
        for (name, offset) in references {
            if declared.contains(name.as_str()) {
                continue;
            }
            match self.private_references.last_mut() {
                Some(outer) => outer.push((name, offset)),
                None => {
                    return Err(JsError::syntax(
                        format!("private name #{name} is not declared in an enclosing class"),
                        offset,
                    ));
                }
            }
        }
        Ok(())
    }

    /// Parse one class body element: a method, accessor, constructor, field,
    /// or static initialization block.
    fn class_element(&mut self, derived: bool) -> Result<ClassElement, JsError> {
        let offset = self.current().offset;
        let static_start = self.current().offset;
        let mut is_static = false;
        if matches!(&self.current().kind, TokenKind::Identifier(name) if name == "static")
            && class_modifier_follows(self.tokens.get(self.cursor + 1))
        {
            self.advance();
            is_static = true;
            if self.take(&TokenKind::LeftBrace) {
                let body = self.static_block_body()?;
                self.require(&TokenKind::RightBrace, "expected '}' after static block")?;
                return Ok(ClassElement {
                    key: PropertyKey::Static("static".to_owned()),
                    kind: ClassElementKind::StaticBlock,
                    is_static: true,
                    is_async: false,
                    is_generator: false,
                    parameters: Vec::new(),
                    body,
                    initializer: None,
                    offset: static_start,
                });
            }
        }
        // `get *x` is not an accessor: the `*` cannot follow the word `get`, so
        // `get` is the element's name, and `get` then `*x` on the same line is a
        // syntax error. On a new line the `*` starts a generator element.
        let mut kind = ClassElementKind::Method;
        let next = self.tokens.get(self.cursor + 1);
        if matches!(&self.current().kind, TokenKind::Identifier(name) if name == "get" || name == "set")
            && class_modifier_follows(next)
            && !matches!(next, Some(token) if token.kind == TokenKind::Star)
        {
            let TokenKind::Identifier(name) = self.advance().kind else {
                unreachable!("checked above");
            };
            kind = if name == "get" {
                ClassElementKind::Get
            } else {
                ClassElementKind::Set
            };
        }
        // `async [no LineTerminator here] *name`: on a new line `async` is the
        // element's own name, a field, and the next line starts a new element.
        let mut is_async = false;
        let next = self.tokens.get(self.cursor + 1);
        if matches!(kind, ClassElementKind::Method)
            && matches!(&self.current().kind, TokenKind::Identifier(name) if name == "async")
            && class_modifier_follows(next)
            && next.is_some_and(|token| !token.after_newline)
        {
            self.advance();
            is_async = true;
        }
        let is_generator = self.take(&TokenKind::Star);
        let key = self.class_element_key()?;
        if self.at(&TokenKind::LeftParen) {
            let is_constructor = derived
                && !is_static
                && kind == ClassElementKind::Method
                && !is_async
                && !is_generator
                && matches!(&key, PropertyKey::Static(name) if name == "constructor");
            let (parameters, body) =
                self.method_tail(FunctionKind::new(is_async, is_generator), is_constructor)?;
            let kind = if matches!(kind, ClassElementKind::Get | ClassElementKind::Set) {
                kind
            } else if !is_static
                && matches!(&key, PropertyKey::Static(name) if name == "constructor")
            {
                ClassElementKind::Constructor
            } else {
                ClassElementKind::Method
            };
            return Ok(ClassElement {
                key,
                kind,
                is_static,
                is_async,
                is_generator,
                parameters,
                body,
                initializer: None,
                offset,
            });
        }
        if matches!(kind, ClassElementKind::Get | ClassElementKind::Set) {
            return Err(self.error("expected '(' after accessor name"));
        }
        if is_async || is_generator {
            return Err(self.error("expected '(' after method name"));
        }
        let initializer = if self.take(&TokenKind::Equal) {
            Some(self.with_super_property(Self::assignment)?)
        } else {
            None
        };
        self.class_field_terminator()?;
        Ok(ClassElement {
            key,
            kind: ClassElementKind::Field,
            is_static,
            is_async: false,
            is_generator: false,
            parameters: Vec::new(),
            body: Vec::new(),
            initializer,
            offset,
        })
    }

    /// Field terminator with automatic semicolon insertion: an explicit `;`,
    /// the next element on a new line, or the class body's closing brace.
    fn class_field_terminator(&mut self) -> Result<(), JsError> {
        if self.take(&TokenKind::Semicolon) || self.at(&TokenKind::RightBrace) {
            return Ok(());
        }
        if self.current().after_newline {
            return Ok(());
        }
        Err(self.error("expected ';' after class field"))
    }

    fn class_element_key(&mut self) -> Result<PropertyKey, JsError> {
        if let TokenKind::PrivateName(name) = self.current().kind.clone() {
            self.advance();
            return Ok(PropertyKey::Private(name));
        }
        self.property_key()
    }

    /// An untagged template literal: the chunks and the substituted values are
    /// concatenated at parse time, because that is all the value of `` `a${x}` ``
    /// is.
    fn template_literal(
        &mut self,
        parts: Vec<TemplatePart>,
        offset: usize,
    ) -> Result<Expr, JsError> {
        let mut result = Expr::Literal(JsValue::String(String::new()));
        for part in parts {
            let next = match part {
                TemplatePart::Quasi { cooked, .. } => Expr::Literal(JsValue::String(cooked)),
                TemplatePart::Expression(source) => self.template_expression(&source, offset)?,
            };
            result = Expr::Binary {
                offset: self.previous_offset(),
                operator: BinaryOp::TemplateConcat,
                left: Box::new(result),
                right: Box::new(next),
            };
        }
        Ok(result)
    }

    /// ECMA-262 13.3.11 `TaggedTemplate`. The quasis and the substitutions are
    /// kept apart so the runtime can materialise a template object whose indices
    /// are the cooked strings and whose `raw` property is the unprocessed text.
    fn tagged_template(
        &mut self,
        tag: Expr,
        parts: Vec<TemplatePart>,
        offset: usize,
    ) -> Result<Expr, JsError> {
        let mut quasis = Vec::new();
        let mut expressions = Vec::new();
        for part in parts {
            match part {
                TemplatePart::Quasi { cooked, raw } => quasis.push((cooked, raw)),
                TemplatePart::Expression(source) => {
                    expressions.push(self.template_expression(&source, offset)?);
                }
            }
        }
        Ok(Expr::TaggedTemplate {
            tag: Box::new(tag),
            quasis,
            expressions,
            offset,
        })
    }

    /// A `${...}` substitution, re-lexed from the source the template lexer
    /// captured so it can be parsed as an ordinary expression.
    fn template_expression(&mut self, source: &str, offset: usize) -> Result<Expr, JsError> {
        let limits = RuntimeLimits::default();
        let tokens = tokenize(source, &limits)?;
        let mut parser = Parser::new(tokens, &limits);
        // The substitution is part of the enclosing function, class and module,
        // so it sees the same `await`/`yield`, private names and `import.meta`.
        parser.in_async = self.in_async;
        parser.in_generator = self.in_generator;
        parser
            .private_references
            .clone_from(&self.private_references);
        parser.super_property_allowed = self.super_property_allowed;
        parser.super_call_allowed = self.super_call_allowed;
        parser.function_depth = self.function_depth;
        parser.module = self.module.as_ref().map(|_| ModuleInfo::default());
        let expression = parser.expression()?;
        if !parser.at(&TokenKind::Eof) {
            return Err(JsError::syntax(
                "unexpected token in template interpolation",
                offset,
            ));
        }
        // References the substitution made to an enclosing class body belong
        // to that body now.
        self.private_references = parser.private_references;
        Ok(expression)
    }

    fn function_expression(&mut self, is_async: bool) -> Result<Expr, JsError> {
        let is_generator = self.take(&TokenKind::Star);
        let kind = FunctionKind::new(is_async, is_generator);
        let name = if let TokenKind::Identifier(name) = &self.current().kind {
            let name = name.clone();
            self.advance();
            Some(name)
        } else {
            None
        };
        // The name of a function expression takes its own kind's `yield`/`await`
        // restriction (ECMA-262 15.5, 15.6, 15.8). Code inside a class body is
        // strict, so the strict reserved words are refused too.
        if let Some(name) = &name {
            let restricted = (kind.is_generator() && name == "yield")
                || (kind.is_async() && name == "await")
                || (!self.private_references.is_empty()
                    && (is_strict_reserved_word(name) || name == "arguments"));
            if restricted {
                return Err(self.error("function expression name is not allowed here"));
            }
        }
        let (parameters, body) = self.function_tail(kind)?;
        Ok(Expr::Function {
            offset: self.previous_offset(),
            name,
            parameters,
            body,
            kind,
        })
    }

    /// A class name is strict-mode code even in a sloppy script (ECMA-262
    /// 15.7.1), so the strict reserved words and `arguments`/`eval` are refused.
    fn check_class_name(&self, name: &str) -> Result<(), JsError> {
        if is_strict_reserved_word(name) || name == "arguments" {
            return Err(self.error("class name is reserved in strict mode"));
        }
        Ok(())
    }

    /// The statements of a class static block. It is a function-like boundary
    /// of its own: no label or loop encloses it, `return` is not allowed, and
    /// `await` and `arguments` are not identifiers (ECMA-262 15.7.1).
    fn static_block_body(&mut self) -> Result<Vec<Statement>, JsError> {
        let previous_labels = std::mem::take(&mut self.labels);
        let previous_switch_depth = std::mem::replace(&mut self.switch_depth, 0);
        let previous_loop_depth = std::mem::replace(&mut self.loop_depth, 0);
        let previous_function_depth = std::mem::replace(&mut self.function_depth, 0);
        let previous_static_block = std::mem::replace(&mut self.static_block, true);
        let previous_static_await = std::mem::replace(&mut self.static_block_await, true);
        let previous_async = std::mem::replace(&mut self.in_async, false);
        let previous_generator = std::mem::replace(&mut self.in_generator, false);
        let result = self.with_super_property(|parser| parser.statement_list(true));
        self.labels = previous_labels;
        self.switch_depth = previous_switch_depth;
        self.loop_depth = previous_loop_depth;
        self.function_depth = previous_function_depth;
        self.static_block = previous_static_block;
        self.static_block_await = previous_static_await;
        self.in_async = previous_async;
        self.in_generator = previous_generator;
        let body = result?;
        validate_declaration_conflicts(&body, true)?;
        Ok(body)
    }

    /// Parse a class field initializer or static block: `super.name` is valid
    /// there, and `super()` is not (ECMA-262 15.7.1).
    fn with_super_property<T>(
        &mut self,
        parse: impl FnOnce(&mut Self) -> Result<T, JsError>,
    ) -> Result<T, JsError> {
        let previous_property = std::mem::replace(&mut self.super_property_allowed, true);
        let previous_call = std::mem::replace(&mut self.super_call_allowed, false);
        let result = parse(self);
        self.super_property_allowed = previous_property;
        self.super_call_allowed = previous_call;
        result
    }

    /// The parameters and body of an ordinary function, which has no `super`.
    fn function_tail(
        &mut self,
        kind: FunctionKind,
    ) -> Result<(Vec<String>, Vec<Statement>), JsError> {
        self.function_tail_with(kind, false, false)
    }

    /// The parameters and body of a method: `super.name` is valid in it, and
    /// `super()` only when `super_call` (a derived class's constructor).
    fn method_tail(
        &mut self,
        kind: FunctionKind,
        super_call: bool,
    ) -> Result<(Vec<String>, Vec<Statement>), JsError> {
        self.function_tail_with(kind, true, super_call)
    }

    fn function_tail_with(
        &mut self,
        kind: FunctionKind,
        super_property: bool,
        super_call: bool,
    ) -> Result<(Vec<String>, Vec<Statement>), JsError> {
        let previous_async = std::mem::replace(&mut self.in_async, kind.is_async());
        let previous_generator = std::mem::replace(&mut self.in_generator, kind.is_generator());
        let previous_parameters = std::mem::replace(&mut self.in_parameters, false);
        let previous_super_property =
            std::mem::replace(&mut self.super_property_allowed, super_property);
        let previous_super_call = std::mem::replace(&mut self.super_call_allowed, false);
        // A function body is a new jump target: no label or loop encloses it.
        let previous_labels = std::mem::take(&mut self.labels);
        let previous_switch_depth = std::mem::replace(&mut self.switch_depth, 0);
        let previous_static_block = std::mem::replace(&mut self.static_block, false);
        let previous_static_await = std::mem::replace(&mut self.static_block_await, false);
        let result = self.function_tail_inner(super_call);
        self.in_async = previous_async;
        self.in_generator = previous_generator;
        self.in_parameters = previous_parameters;
        self.super_property_allowed = previous_super_property;
        self.super_call_allowed = previous_super_call;
        self.labels = previous_labels;
        self.switch_depth = previous_switch_depth;
        self.static_block = previous_static_block;
        self.static_block_await = previous_static_await;
        let (parameters, body) = result?;
        validate_declaration_conflicts(&body, true)?;
        // A function whose own body is strict is checked as strict code here,
        // whatever the enclosing script is (ECMA-262 11.2.2).
        if has_use_strict_directive(&body) {
            validate_strict_parameters(&parameters)?;
            validate_strict_statements(&body)?;
        }
        Ok((parameters, body))
    }

    /// `super_call` is the call permission of the body; parameters never have it.
    fn function_tail_inner(
        &mut self,
        super_call: bool,
    ) -> Result<(Vec<String>, Vec<Statement>), JsError> {
        self.require(
            &TokenKind::LeftParen,
            "expected '(' before function parameters",
        )?;
        self.in_parameters = true;
        let mut parameters = Vec::new();
        let mut markers: Vec<Statement> = Vec::new();
        let mut patterns = Vec::new();
        let mut bound = BTreeSet::new();
        if !self.at(&TokenKind::RightParen) {
            loop {
                if self.take(&TokenKind::Ellipsis) {
                    let TokenKind::Identifier(parameter) = self.advance().kind else {
                        return Err(self.error("expected a rest parameter name"));
                    };
                    if !bound.insert(parameter.clone()) {
                        return Err(self.error("duplicate function parameters are not supported"));
                    }
                    parameters.push(format!("{PARAMETER_REST_MARKER}{parameter}"));
                    break;
                }
                // A destructuring parameter binds an anonymous argument slot;
                // the pattern is lowered to a `var` declaration that the call
                // runs for that slot (see `Statement::ParameterPattern`).
                let mut bound_names = Vec::new();
                if self.at(&TokenKind::LeftBrace) || self.at(&TokenKind::LeftBracket) {
                    let pattern = self.binding_pattern()?;
                    let temporary = format!("\0param_{}", parameters.len());
                    patterns.push((parameters.len(), temporary.clone(), pattern));
                    bound_names.push(temporary);
                } else {
                    // `undefined` is an ordinary identifier in parameter position.
                    let parameter = match self.advance().kind {
                        TokenKind::Identifier(name) => name,
                        TokenKind::Undefined => "undefined".to_owned(),
                        _ => return Err(self.error("expected a parameter name")),
                    };
                    self.check_contextual_binding(&parameter)?;
                    bound_names.push(parameter);
                }
                let has_default = self.take(&TokenKind::Equal);
                if has_default {
                    let value = self.assignment()?;
                    markers.push(Statement::ParameterDefault {
                        index: parameters.len(),
                        value,
                        offset: self.previous_offset(),
                    });
                }
                for parameter in bound_names {
                    if !bound.insert(parameter.clone()) {
                        return Err(self.error("duplicate function parameters are not supported"));
                    }
                    parameters.push(if has_default {
                        format!("{PARAMETER_DEFAULT_MARKER}{parameter}")
                    } else {
                        parameter
                    });
                }
                if !self.take(&TokenKind::Comma) || self.at(&TokenKind::RightParen) {
                    break;
                }
            }
        }
        self.require(&TokenKind::RightParen, "expected ')' after parameters")?;
        self.in_parameters = false;
        self.super_call_allowed = super_call;
        self.require(&TokenKind::LeftBrace, "expected '{' before function body")?;
        let previous_function_depth = self.function_depth;
        let previous_loop_depth = self.loop_depth;
        let previous_no_in = self.no_in;
        let previous_strict = self.strict;
        self.function_depth = self.function_depth.saturating_add(1);
        self.loop_depth = 0;
        self.no_in = false;
        self.strict = previous_strict || self.prologue_is_strict(self.cursor);
        let body = self.statement_list(true);
        self.function_depth = previous_function_depth;
        self.loop_depth = previous_loop_depth;
        self.no_in = previous_no_in;
        self.strict = previous_strict;
        let mut body = body?;
        self.require(&TokenKind::RightBrace, "expected '}' after function body")?;
        // Checked before the lowering below moves the parameter defaults ahead
        // of the body, which would hide the directive prologue.
        if !is_simple_parameter_list(&parameters) && has_use_strict_directive(&body) {
            return Err(self.error("'use strict' is not allowed with non-simple parameters"));
        }
        let offset = self.previous_offset();
        for (index, temporary, pattern) in patterns {
            let mut declarations = Vec::new();
            Self::lower_declarator(pattern, Expr::Identifier(temporary), &mut declarations);
            markers.push(Statement::ParameterPattern {
                index,
                declarations,
                offset,
            });
        }
        if !markers.is_empty() {
            markers.extend(body);
            body = markers;
        }
        Ok((parameters, body))
    }

    fn object_literal(&mut self) -> Result<Expr, JsError> {
        // Member values are `[+In]` even inside a for-loop initializer.
        self.in_allowed(|parser| {
            let mut properties = Vec::new();
            while !parser.at(&TokenKind::RightBrace) {
                properties.push(parser.object_member()?);
                if !parser.take(&TokenKind::Comma) {
                    break;
                }
            }
            parser.require(&TokenKind::RightBrace, "expected '}' after object literal")?;
            Ok(Expr::Object(properties))
        })
    }

    /// One member of an object literal (ECMA-262 13.2.5): a spread, a method,
    /// an accessor, a `PropertyName : AssignmentExpression` pair, or a shorthand
    /// `IdentifierReference`.
    fn object_member(&mut self) -> Result<ObjectProperty, JsError> {
        if self.take(&TokenKind::Ellipsis) {
            return Ok(ObjectProperty {
                key: PropertyKey::Spread,
                value: self.assignment()?,
                accessor: None,
                shorthand: false,
                method: false,
            });
        }
        // `async` is a method modifier only when a property name follows on
        // the same line (`async [no LineTerminator here] *`). `{ async }`,
        // `{ async: 1 }` and `{ async() {} }` keep it as the name.
        let is_async = matches!(&self.current().kind, TokenKind::Identifier(name) if name == "async")
            && self.tokens.get(self.cursor + 1).is_some_and(|next| {
                !next.after_newline
                    && !matches!(
                        next.kind,
                        TokenKind::LeftParen
                            | TokenKind::Colon
                            | TokenKind::Comma
                            | TokenKind::RightBrace
                            | TokenKind::Equal
                    )
            });
        if is_async {
            self.advance();
        }
        let is_generator = self.take(&TokenKind::Star);
        if is_async || is_generator {
            let key = self.property_key()?;
            return self.object_method(key, FunctionKind::new(is_async, is_generator));
        }
        // `get` and `set` are accessor keywords only when a property name
        // follows; `{ get }`, `{ get: 1 }`, `{ get() {} }` and `{ get = 1 }`
        // keep them as names.
        let next_kind = self.tokens.get(self.cursor + 1).map(|token| &token.kind);
        let accessor = match &self.current().kind {
            TokenKind::Identifier(name)
                if (name == "get" || name == "set")
                    && !matches!(
                        next_kind,
                        Some(
                            TokenKind::LeftParen
                                | TokenKind::Colon
                                | TokenKind::Comma
                                | TokenKind::RightBrace
                                | TokenKind::Equal
                        )
                    ) =>
            {
                if name == "get" {
                    Some(ObjectAccessorKind::Getter)
                } else {
                    Some(ObjectAccessorKind::Setter)
                }
            }
            _ => None,
        };
        if let Some(accessor) = accessor {
            self.advance();
            let key = self.property_key()?;
            let name = static_key_name(&key);
            let (parameters, body) = self.method_tail(FunctionKind::Normal, false)?;
            return Ok(ObjectProperty {
                key,
                value: Expr::Function {
                    offset: self.previous_offset(),
                    name,
                    parameters,
                    body,
                    kind: FunctionKind::Normal,
                },
                accessor: Some(accessor),
                shorthand: false,
                method: false,
            });
        }
        // A shorthand member names an IdentifierReference, so a reserved word,
        // a literal or a string cannot stand alone.
        // `let` and `undefined` are lexed as keywords but are ordinary names here.
        let shorthand_allowed = match &self.current().kind {
            TokenKind::Identifier(name) => !ALWAYS_RESERVED_NAMES.contains(&name.as_str()),
            TokenKind::Let | TokenKind::Undefined => true,
            _ => false,
        };
        let key = self.property_key()?;
        if self.at(&TokenKind::LeftParen) {
            return self.object_method(key, FunctionKind::Normal);
        }
        if self.take(&TokenKind::Colon) {
            return Ok(ObjectProperty {
                key,
                value: self.assignment()?,
                accessor: None,
                shorthand: false,
                method: false,
            });
        }
        match key {
            PropertyKey::Static(name) if shorthand_allowed => {
                // The shorthand is an IdentifierReference (ECMA-262 13.2.5.1).
                let restricted = (self.in_generator && name == "yield")
                    || (self.in_async && name == "await")
                    || (self.static_block_await && name == "await")
                    || (self.static_block && name == "arguments");
                if restricted {
                    return Err(self.error("name is not a valid identifier reference here"));
                }
                Ok(ObjectProperty {
                    key: PropertyKey::Static(name.clone()),
                    value: Expr::Identifier(name),
                    accessor: None,
                    shorthand: true,
                    method: false,
                })
            }
            PropertyKey::Computed(_) => {
                Err(self.error("expected ':' after computed property name"))
            }
            _ => Err(self.error("expected ':' after property name")),
        }
    }

    /// The parameters and body of an object-literal method whose key has been
    /// read. Only a literal key names the function; a computed key is named at
    /// run time.
    fn object_method(
        &mut self,
        key: PropertyKey,
        kind: FunctionKind,
    ) -> Result<ObjectProperty, JsError> {
        let name = static_key_name(&key);
        let (parameters, body) = self.method_tail(kind, false)?;
        Ok(ObjectProperty {
            key,
            value: Expr::Function {
                offset: self.previous_offset(),
                name,
                parameters,
                body,
                kind,
            },
            accessor: None,
            shorthand: false,
            method: true,
        })
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
            let element = if self.take(&TokenKind::Ellipsis) {
                Expr::Spread(Box::new(self.assignment()?))
            } else {
                self.assignment()?
            };
            elements.push(element);
            if !self.take(&TokenKind::Comma) {
                break;
            }
        }
        self.require(&TokenKind::RightBracket, "expected ']' after array literal")?;
        Ok(Expr::Array(elements))
    }

    /// ECMA-262 13.2.5 `PropertyName`: a literal name, or `[ AssignmentExpression ]`
    /// whose value is converted to a key at run time.
    fn property_key(&mut self) -> Result<PropertyKey, JsError> {
        if self.take(&TokenKind::LeftBracket) {
            // The bracketed name is `[+In]` even inside a for-loop initializer.
            let key = self.in_allowed(Self::assignment)?;
            self.require(
                &TokenKind::RightBracket,
                "expected ']' after computed property name",
            )?;
            return Ok(PropertyKey::Computed(key));
        }
        Ok(PropertyKey::Static(self.property_name()?))
    }

    fn property_name(&mut self) -> Result<String, JsError> {
        let token = self.advance();
        match token.kind {
            TokenKind::Identifier(name)
            | TokenKind::EscapedReserved(name)
            | TokenKind::String(name) => Ok(name),
            // A numeric name is the ToString of its value: `1e21` is "1e+21".
            TokenKind::Number(value) => Ok(number_to_string(value)),
            // A BigInt name is the ToString of its value, which is decimal.
            TokenKind::BigInt(value) => Ok(value.to_string_radix(10)),
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
    /// The end of a simple statement. A `;` ends it, and so does a position where
    /// automatic semicolon insertion applies: before `}`, at the end of input, or
    /// after a line break (ECMA-262 12.9.1).
    fn end_statement(&mut self) -> Result<(), JsError> {
        if self.take(&TokenKind::Semicolon)
            || self.at(&TokenKind::RightBrace)
            || self.at(&TokenKind::Eof)
            || self.current().after_newline
        {
            return Ok(());
        }
        Err(self.error("expected ';' after statement"))
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
        self.previous_offset = token.offset;
        if !matches!(token.kind, TokenKind::Eof) {
            self.cursor = self.cursor.saturating_add(1);
        }
        token
    }

    /// Offset of the most recently consumed token: the best cheap position
    /// for a node constructed after parsing its tokens.
    fn previous_offset(&self) -> usize {
        self.previous_offset
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

/// Whether a token after a line break continues the expression before it, so
/// no automatic semicolon is inserted there (ECMA-262 12.9.1).
fn continues_expression(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::LeftParen
            | TokenKind::LeftBracket
            | TokenKind::Dot
            | TokenKind::QuestionDot
            | TokenKind::Question
            | TokenKind::QuestionQuestion
            | TokenKind::Template(_)
            | TokenKind::Comma
            | TokenKind::Equal
            | TokenKind::Plus
            | TokenKind::Minus
            | TokenKind::Star
            | TokenKind::StarStar
            | TokenKind::Slash
            | TokenKind::Percent
            | TokenKind::Less
            | TokenKind::LessEqual
            | TokenKind::Greater
            | TokenKind::GreaterEqual
            | TokenKind::LeftShift
            | TokenKind::RightShift
            | TokenKind::UnsignedRightShift
            | TokenKind::EqualEqual
            | TokenKind::EqualEqualEqual
            | TokenKind::BangEqual
            | TokenKind::BangEqualEqual
            | TokenKind::Ampersand
            | TokenKind::Pipe
            | TokenKind::Caret
            | TokenKind::AndAnd
            | TokenKind::OrOr
            | TokenKind::In
            | TokenKind::Instanceof
    )
}

/// Whether `statement` is a declaration rather than a `Statement` (ECMA-262
/// 14.1, 14.11): a function, class, or `let`/`const` declaration, possibly
/// labelled.
fn is_declaration(statement: &Statement) -> bool {
    match statement {
        Statement::Function { .. } | Statement::Class { .. } => true,
        Statement::Variable { kind, .. } | Statement::VariableList { kind, .. } => {
            *kind != VariableKind::Var
        }
        Statement::Labeled { body, .. } => is_declaration(body),
        _ => false,
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

fn is_identifier_name(name: &str) -> bool {
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|character| character.is_alphabetic() || matches!(character, '_' | '$'))
        && characters.all(|character| {
            character.is_alphanumeric() || matches!(character, '_' | '$' | '\u{200c}' | '\u{200d}')
        })
        && !matches!(
            name,
            "let"
                | "const"
                | "var"
                | "function"
                | "return"
                | "new"
                | "throw"
                | "try"
                | "catch"
                | "finally"
                | "if"
                | "else"
                | "while"
                | "do"
                | "for"
                | "break"
                | "continue"
                | "delete"
                | "typeof"
                | "void"
                | "in"
                | "instanceof"
                | "switch"
                | "case"
                | "default"
                | "this"
                | "true"
                | "false"
                | "null"
                | "undefined"
        )
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
        // Reached only from strict code, so the body is strict whatever its own
        // directive says.
        Statement::Function {
            name,
            parameters,
            body,
            ..
        } => {
            if is_strict_reserved_word(name) || name == "arguments" {
                return Err(JsError::syntax(
                    format!("{name} is reserved in strict mode"),
                    0,
                ));
            }
            validate_strict_parameters(parameters)?;
            validate_strict_statements(body)
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
        Statement::With { object, body, .. } => {
            validate_strict_expression(object)?;
            validate_strict_statement(body)
        }
        Statement::DoWhile {
            condition, body, ..
        } => {
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
        Statement::ForOf {
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
            ..
        } => {
            validate_strict_expression(target)?;
            validate_strict_expression(iterable)?;
            validate_strict_statement(body)
        }
        Statement::Switch {
            expression, cases, ..
        } => {
            validate_strict_expression(expression)?;
            for (tests, statements) in cases {
                for test in tests {
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
            ..
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
        Statement::Class {
            name,
            super_class,
            elements,
            ..
        } => {
            if is_strict_reserved_word(name) {
                return Err(JsError::syntax(
                    format!("{name} is reserved in strict mode"),
                    0,
                ));
            }
            validate_strict_class(super_class.as_deref(), elements)
        }
        Statement::Variable { value, .. } => {
            value.as_ref().map_or(Ok(()), validate_strict_expression)
        }
        Statement::VariableList { declarations, .. }
        | Statement::ParameterPattern { declarations, .. } => declarations
            .iter()
            .filter_map(|(_, value)| value.as_ref())
            .try_for_each(validate_strict_expression),
        Statement::Return(value) => value.as_ref().map_or(Ok(()), validate_strict_expression),
        Statement::Throw(value) => validate_strict_expression(value),
        Statement::ParameterDefault { value, .. } => validate_strict_expression(value),
        Statement::Break(_) | Statement::Continue(_) => Ok(()),
    }
}

fn validate_strict_expression(expression: &Expr) -> Result<(), JsError> {
    match expression {
        Expr::Assignment { target, value, .. }
        | Expr::CompoundAssignment { target, value, .. }
        | Expr::LogicalAssignment { target, value, .. } => {
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
        | Expr::Arrow {
            parameters, body, ..
        } => {
            validate_strict_parameters(parameters)?;
            validate_strict_statements(body)
        }
        Expr::Object(properties) => properties.iter().try_for_each(|property| {
            if let PropertyKey::Computed(key) = &property.key {
                validate_strict_expression(key)?;
            }
            validate_strict_expression(&property.value)
        }),
        Expr::Array(elements) => elements.iter().try_for_each(validate_strict_expression),
        Expr::Spread(expression)
        | Expr::OptionalChain(expression)
        | Expr::OptionalGuard(expression)
        | Expr::Await(expression) => validate_strict_expression(expression),
        Expr::Yield { argument, .. } => argument
            .as_deref()
            .map_or(Ok(()), validate_strict_expression),

        Expr::Unary {
            operator: UnaryOp::Delete,
            operand,
            ..
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
            ..
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
        Expr::ComputedMember {
            object, property, ..
        } => {
            validate_strict_expression(object)?;
            validate_strict_expression(property)
        }
        Expr::New {
            constructor,
            arguments,
            ..
        }
        | Expr::Call {
            callee: constructor,
            arguments,
            ..
        } => {
            validate_strict_expression(constructor)?;
            arguments.iter().try_for_each(validate_strict_expression)
        }
        Expr::TaggedTemplate {
            tag, expressions, ..
        } => {
            validate_strict_expression(tag)?;
            expressions.iter().try_for_each(validate_strict_expression)
        }
        // `eval` and `arguments` are ordinary references in strict code; the
        // other strict reserved words are not identifiers there.
        Expr::Identifier(name) if name != "eval" && is_strict_reserved_word(name) => Err(
            JsError::syntax(format!("{name} is reserved in strict mode"), 0),
        ),
        Expr::Literal(_) | Expr::RegexLiteral { .. } | Expr::This | Expr::Identifier(_) => Ok(()),
        Expr::Sequence(expressions) => expressions.iter().try_for_each(validate_strict_expression),
        Expr::Class {
            name,
            super_class,
            elements,
            ..
        } => {
            if name.as_deref().is_some_and(is_strict_reserved_word) {
                return Err(JsError::syntax("class name is reserved in strict mode", 0));
            }
            validate_strict_class(super_class.as_deref(), elements)
        }
        Expr::SuperMember { .. } | Expr::NewTarget => Ok(()),
        Expr::SuperComputedMember { property, .. } => validate_strict_expression(property),
        Expr::SuperCall { arguments, .. } => {
            arguments.iter().try_for_each(validate_strict_expression)
        }
        Expr::PrivateMember { object, .. } | Expr::PrivateIn { object, .. } => {
            validate_strict_expression(object)
        }
    }
}

/// Class bodies are always strict code, so every element body and initializer
/// is validated regardless of a `"use strict"` directive.
fn validate_strict_class(
    super_class: Option<&Expr>,
    elements: &[ClassElement],
) -> Result<(), JsError> {
    if let Some(super_class) = super_class {
        validate_strict_expression(super_class)?;
    }
    for element in elements {
        if let PropertyKey::Computed(key) = &element.key {
            validate_strict_expression(key)?;
        }
        if let Some(initializer) = &element.initializer {
            validate_strict_expression(initializer)?;
            // Class field initializers may not reference `await`/`yield`.
            validate_reserved_expression(
                initializer,
                ReservedContext {
                    async_context: true,
                    generator_context: true,
                },
            )?;
        }
        if element
            .parameters
            .iter()
            .any(|name| is_strict_reserved_word(name))
        {
            return Err(JsError::syntax(
                "strict mode parameter uses a reserved word",
                0,
            ));
        }
        let context = ReservedContext {
            async_context: element.is_async,
            generator_context: element.is_generator,
        };
        for statement in &element.body {
            validate_reserved_statement(statement, context)?;
        }
        validate_strict_statements(&element.body)?;
    }
    Ok(())
}

/// Whether an async or generator context reserves the given identifier.
#[derive(Clone, Copy, Default)]
struct ReservedContext {
    async_context: bool,
    generator_context: bool,
}

/// A simple assignment target: a name or a property reference (ECMA-262 13.15.1).
fn is_simple_target(expression: &Expr) -> bool {
    matches!(
        expression,
        Expr::Identifier(_)
            | Expr::Member { .. }
            | Expr::ComputedMember { .. }
            | Expr::PrivateMember { .. }
            | Expr::SuperMember { .. }
            | Expr::SuperComputedMember { .. }
    )
}

/// Whether a parameter list is a `SimpleParameterList` (ECMA-262 15.1.1): plain
/// names only, with no default, rest, or destructuring parameter. Destructuring
/// parameters are recorded under the `\0`-prefixed temporaries of the lowering.
fn is_simple_parameter_list(parameters: &[String]) -> bool {
    parameters.iter().all(|parameter| {
        !parameter.starts_with(PARAMETER_DEFAULT_MARKER)
            && !parameter.starts_with(PARAMETER_REST_MARKER)
            && !parameter.starts_with('\0')
    })
}

/// Parameter names in strict code may be neither `eval`, `arguments`, nor a
/// strict reserved word (ECMA-262 15.2.1).
fn validate_strict_parameters(parameters: &[String]) -> Result<(), JsError> {
    for parameter in parameters {
        let name = parameter_binding_name(parameter);
        if is_strict_reserved_word(name) || name == "arguments" {
            return Err(JsError::syntax(
                "strict mode parameter uses a reserved word",
                0,
            ));
        }
    }
    Ok(())
}

/// Strip a parameter's default/rest marker to recover its binding name.
fn parameter_binding_name(parameter: &str) -> &str {
    parameter
        .strip_prefix(PARAMETER_DEFAULT_MARKER)
        .or_else(|| parameter.strip_prefix(PARAMETER_REST_MARKER))
        .unwrap_or(parameter)
}

fn check_reserved_identifier(name: &str, context: ReservedContext) -> Result<(), JsError> {
    if context.async_context && name == "await" {
        return Err(JsError::syntax(
            "'await' is not allowed in this async context",
            0,
        ));
    }
    if context.generator_context && name == "yield" {
        return Err(JsError::syntax(
            "'yield' is not allowed as an identifier in a generator",
            0,
        ));
    }
    Ok(())
}

fn validate_reserved_parameters(
    parameters: &[String],
    context: ReservedContext,
) -> Result<(), JsError> {
    for parameter in parameters {
        check_reserved_identifier(parameter_binding_name(parameter), context)?;
    }
    Ok(())
}

fn validate_reserved_statements(
    statements: &[Statement],
    context: ReservedContext,
) -> Result<(), JsError> {
    for statement in statements {
        validate_reserved_statement(statement, context)?;
    }
    Ok(())
}

/// Validate a nested class's elements against async/generator reservations.
fn validate_reserved_class(elements: &[ClassElement]) -> Result<(), JsError> {
    for element in elements {
        if let PropertyKey::Computed(key) = &element.key {
            validate_reserved_expression(key, ReservedContext::default())?;
        }
        if let Some(initializer) = &element.initializer {
            validate_reserved_expression(
                initializer,
                ReservedContext {
                    async_context: true,
                    generator_context: true,
                },
            )?;
        }
        let context = ReservedContext {
            async_context: element.is_async,
            generator_context: element.is_generator,
        };
        for statement in &element.body {
            validate_reserved_statement(statement, context)?;
        }
    }
    Ok(())
}

fn validate_reserved_statement(
    statement: &Statement,
    context: ReservedContext,
) -> Result<(), JsError> {
    match statement {
        Statement::Variable { name, value, .. } => {
            check_reserved_identifier(name, context)?;
            if let Some(value) = value {
                validate_reserved_expression(value, context)?;
            }
        }
        Statement::VariableList { declarations, .. }
        | Statement::ParameterPattern { declarations, .. } => {
            for (target, value) in declarations {
                if let BindingTarget::Name(name) = target {
                    check_reserved_identifier(name, context)?;
                }
                if let Some(value) = value {
                    validate_reserved_expression(value, context)?;
                }
            }
        }
        Statement::Function {
            name,
            parameters,
            body,
            ..
        } => {
            check_reserved_identifier(name, context)?;
            validate_reserved_parameters(parameters, context)?;
            validate_reserved_statements(body, ReservedContext::default())?;
        }
        Statement::Class {
            name,
            super_class,
            elements,
            ..
        } => {
            check_reserved_identifier(name, context)?;
            if let Some(super_class) = super_class {
                validate_reserved_expression(super_class, context)?;
            }
            validate_reserved_class(elements)?;
        }
        Statement::Return(value) => {
            if let Some(value) = value {
                validate_reserved_expression(value, context)?;
            }
        }
        Statement::Throw(value) | Statement::Expression(value) => {
            validate_reserved_expression(value, context)?;
        }
        Statement::If {
            condition,
            consequent,
            alternate,
            ..
        } => {
            validate_reserved_expression(condition, context)?;
            validate_reserved_statement(consequent, context)?;
            if let Some(alternate) = alternate {
                validate_reserved_statement(alternate, context)?;
            }
        }
        Statement::Switch {
            expression, cases, ..
        } => {
            validate_reserved_expression(expression, context)?;
            for (tests, statements) in cases {
                for test in tests {
                    validate_reserved_expression(test, context)?;
                }
                validate_reserved_statements(statements, context)?;
            }
        }
        Statement::With { object, body, .. } => {
            validate_reserved_expression(object, context)?;
            validate_reserved_statement(body, context)?;
        }
        Statement::While {
            condition, body, ..
        } => {
            validate_reserved_expression(condition, context)?;
            validate_reserved_statement(body, context)?;
        }
        Statement::DoWhile {
            condition, body, ..
        } => {
            validate_reserved_expression(condition, context)?;
            validate_reserved_statement(body, context)?;
        }
        Statement::For {
            initializer,
            condition,
            update,
            body,
            ..
        } => {
            if let Some(initializer) = initializer {
                validate_reserved_statement(initializer, context)?;
            }
            if let Some(condition) = condition {
                validate_reserved_expression(condition, context)?;
            }
            if let Some(update) = update {
                validate_reserved_expression(update, context)?;
            }
            validate_reserved_statement(body, context)?;
        }
        Statement::ForIn {
            name,
            iterable,
            body,
            ..
        }
        | Statement::ForOf {
            name,
            iterable,
            body,
            ..
        } => {
            check_reserved_identifier(name, context)?;
            validate_reserved_expression(iterable, context)?;
            validate_reserved_statement(body, context)?;
        }
        Statement::ForInExpr {
            target,
            iterable,
            body,
            ..
        } => {
            validate_reserved_expression(target, context)?;
            validate_reserved_expression(iterable, context)?;
            validate_reserved_statement(body, context)?;
        }
        Statement::Labeled { label, body, .. } => {
            check_reserved_identifier(label, context)?;
            validate_reserved_statement(body, context)?;
        }
        Statement::Try {
            body,
            catch,
            finally,
            ..
        } => {
            validate_reserved_statements(body, context)?;
            if let Some(catch) = catch {
                if let Some(parameter) = &catch.parameter {
                    for name in parameter.names() {
                        check_reserved_identifier(&name, context)?;
                    }
                }
                validate_reserved_statements(&catch.body, context)?;
            }
            if let Some(finally) = finally {
                validate_reserved_statements(finally, context)?;
            }
        }
        Statement::Block(statements) => validate_reserved_statements(statements, context)?,
        Statement::ParameterDefault { value, .. } => {
            validate_reserved_expression(value, context)?;
        }
        Statement::Break(_) | Statement::Continue(_) => {}
    }
    Ok(())
}

fn validate_reserved_expression(
    expression: &Expr,
    context: ReservedContext,
) -> Result<(), JsError> {
    match expression {
        Expr::Identifier(name) => check_reserved_identifier(name, context)?,
        // A function expression's `yield`/`await` restrictions come from its own
        // kind, not the enclosing one. The `name` of a method is a property name
        // and is not checked here.
        Expr::Function {
            parameters,
            body,
            kind,
            ..
        } => {
            let own = ReservedContext {
                async_context: kind.is_async(),
                generator_context: kind.is_generator(),
            };
            validate_reserved_parameters(parameters, own)?;
            validate_reserved_statements(body, ReservedContext::default())?;
        }
        Expr::Arrow {
            parameters, body, ..
        } => {
            validate_reserved_parameters(parameters, context)?;
            validate_reserved_statements(body, ReservedContext::default())?;
        }
        Expr::Class {
            name,
            super_class,
            elements,
            ..
        } => {
            if let Some(name) = name {
                check_reserved_identifier(name, context)?;
            }
            if let Some(super_class) = super_class {
                validate_reserved_expression(super_class, context)?;
            }
            validate_reserved_class(elements)?;
        }
        Expr::Object(properties) => {
            for property in properties {
                if let PropertyKey::Computed(key) = &property.key {
                    validate_reserved_expression(key, context)?;
                }
                validate_reserved_expression(&property.value, context)?;
            }
        }
        Expr::Array(elements) => {
            for element in elements {
                validate_reserved_expression(element, context)?;
            }
        }
        Expr::Spread(inner)
        | Expr::OptionalChain(inner)
        | Expr::OptionalGuard(inner)
        | Expr::Await(inner) => {
            validate_reserved_expression(inner, context)?;
        }
        Expr::Yield { argument, .. } => {
            if let Some(argument) = argument {
                validate_reserved_expression(argument, context)?;
            }
        }
        Expr::Unary { operand, .. } => validate_reserved_expression(operand, context)?,
        Expr::Binary { left, right, .. } => {
            validate_reserved_expression(left, context)?;
            validate_reserved_expression(right, context)?;
        }
        Expr::Conditional {
            condition,
            consequent,
            alternate,
            ..
        } => {
            validate_reserved_expression(condition, context)?;
            validate_reserved_expression(consequent, context)?;
            validate_reserved_expression(alternate, context)?;
        }
        Expr::Update { target, .. } => validate_reserved_expression(target, context)?,
        Expr::Member { object, .. } => validate_reserved_expression(object, context)?,
        Expr::ComputedMember {
            object, property, ..
        } => {
            validate_reserved_expression(object, context)?;
            validate_reserved_expression(property, context)?;
        }
        Expr::New {
            constructor,
            arguments,
            ..
        }
        | Expr::Call {
            callee: constructor,
            arguments,
            ..
        } => {
            validate_reserved_expression(constructor, context)?;
            for argument in arguments {
                validate_reserved_expression(argument, context)?;
            }
        }
        Expr::TaggedTemplate {
            tag, expressions, ..
        } => {
            validate_reserved_expression(tag, context)?;
            for expression in expressions {
                validate_reserved_expression(expression, context)?;
            }
        }
        Expr::Assignment { target, value, .. }
        | Expr::CompoundAssignment { target, value, .. }
        | Expr::LogicalAssignment { target, value, .. } => {
            validate_reserved_expression(target, context)?;
            validate_reserved_expression(value, context)?;
        }
        Expr::Sequence(expressions) => {
            for expression in expressions {
                validate_reserved_expression(expression, context)?;
            }
        }
        Expr::SuperComputedMember { property, .. } => {
            validate_reserved_expression(property, context)?;
        }
        Expr::SuperCall { arguments, .. } => {
            for argument in arguments {
                validate_reserved_expression(argument, context)?;
            }
        }
        Expr::PrivateMember { object, .. } | Expr::PrivateIn { object, .. } => {
            validate_reserved_expression(object, context)?;
        }
        Expr::Literal(_)
        | Expr::RegexLiteral { .. }
        | Expr::This
        | Expr::SuperMember { .. }
        | Expr::NewTarget => {}
    }
    Ok(())
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
        }
        | Statement::ParameterPattern { declarations, .. } => {
            for (target, _) in declarations {
                names.extend(target.names());
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
        | Statement::With { body, .. }
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
        | Statement::Class { .. }
        | Statement::Variable { .. }
        | Statement::VariableList { .. }
        | Statement::ParameterDefault { .. }
        | Statement::Return(_)
        | Statement::Throw(_)
        | Statement::Break(_)
        | Statement::Continue(_)
        | Statement::Expression(_) => {}
    }
}

/// Early errors for declarations that clash within one statement list
/// (ECMA-262 14.2.1 for blocks, 15.2.1 for function bodies, 16.1.1 for scripts).
/// A lexical name (`let`, `const`, `class`, or a block-level function) may not
/// be declared twice, nor be declared by a `var` in the list or in a block
/// nested in it. In a function body or script (`top_level`), a function
/// declaration is var-scoped rather than lexical. Duplicate block-level
/// functions are accepted: sloppy code allows them (Annex B.3.3.4), and the
/// parser does not yet know the code's strictness.
fn validate_declaration_conflicts<'a>(
    statements: impl IntoIterator<Item = &'a Statement>,
    top_level: bool,
) -> Result<(), JsError> {
    let mut lexical = BTreeSet::new();
    let mut block_functions = BTreeSet::new();
    let mut var_names = BTreeSet::new();
    let duplicate =
        |name: &str| JsError::syntax(format!("binding {name:?} is declared more than once"), 0);
    for statement in statements {
        match statement {
            Statement::Variable {
                kind: VariableKind::Let | VariableKind::Const,
                name,
                ..
            } => {
                if !lexical.insert(name.clone()) {
                    return Err(duplicate(name));
                }
            }
            Statement::VariableList {
                kind, declarations, ..
            } => {
                for (target, _) in declarations {
                    for name in target.names() {
                        if *kind == VariableKind::Var {
                            var_names.insert(name);
                        } else if !lexical.insert(name.clone()) {
                            return Err(duplicate(&name));
                        }
                    }
                }
            }
            // A destructuring parameter's names are `var` names of the body.
            Statement::ParameterPattern { declarations, .. } => {
                for (target, _) in declarations {
                    var_names.extend(target.names());
                }
            }
            Statement::Class { name, .. } => {
                if !lexical.insert(name.clone()) {
                    return Err(duplicate(name));
                }
            }
            Statement::Function { name, .. } => {
                if top_level {
                    var_names.insert(name.clone());
                } else {
                    block_functions.insert(name.clone());
                }
            }
            other => collect_var_names(other, &mut var_names),
        }
    }
    for name in &block_functions {
        if lexical.contains(name) {
            return Err(duplicate(name));
        }
    }
    lexical.extend(block_functions);
    if let Some(name) = lexical.iter().find(|name| var_names.contains(*name)) {
        return Err(JsError::syntax(
            format!("binding {name:?} conflicts with a var declaration"),
            0,
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Expr, Statement, parse};
    use crate::RuntimeLimits;
    use crate::lexer::tokenize;

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
