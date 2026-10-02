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
use std::collections::BTreeMap;
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum VariableKind {
    Let,
    Const,
    Var,
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
    pub parameter: String,
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
    },
    Arrow {
        parameters: Vec<String>,
        body: Vec<Statement>,
        offset: usize,
    },
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
    let statements = parser.statement_list(false)?;
    let info = parser.module.take().unwrap_or_default();
    Ok((statements, info))
}

/// Whether the token after a class modifier keyword (`get`, `set`, `async`,
/// `static`) lets it act as a modifier instead of an element name. A name
/// followed by `(`/`=`/`;`/`}` (or nothing) is an ordinary element.
fn class_modifier_follows(next: Option<&TokenKind>) -> bool {
    !matches!(
        next,
        None | Some(
            TokenKind::LeftParen | TokenKind::Equal | TokenKind::Semicolon | TokenKind::RightBrace
        )
    )
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
            }
        }
    }
    Ok(())
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
            class_depth: 0,
            no_in: false,
            module: None,
        }
    }
}

struct Parser {
    previous_offset: usize,
    tokens: Vec<Token>,
    cursor: usize,
    statement_count: usize,
    max_statements: usize,
    function_depth: usize,
    loop_depth: usize,
    switch_depth: usize,
    /// Nesting depth of class bodies currently being parsed; `#name`
    /// references and `#name in` are only valid inside one.
    class_depth: usize,
    /// While set, `in` is not treated as a binary operator (for-heads).
    no_in: bool,
    /// Present while parsing a module; collects its import/export tables.
    module: Option<ModuleInfo>,
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
            // Treat async/generator declarations as ordinary functions. The
            // runtime does not suspend generator frames, but accepting their
            // syntax lets feature-detection and polyfill code load normally.
            let _ = self.take(&TokenKind::Star);
            return self.function_declaration();
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
            let _ = self.take(&TokenKind::Star);
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
                offset: self.previous_offset(),
                label,
                body: Box::new(body),
            });
        }
        if self.take(&TokenKind::Break) {
            let label = self.take_loop_label();
            if label.is_none() && self.loop_depth == 0 && self.switch_depth == 0 {
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
            let value = if self.at(&TokenKind::Semicolon)
                || self.at(&TokenKind::RightBrace)
                || self.current().after_newline
            {
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
            self.end_statement();
            return Ok(());
        }
        // (imported, local) pairs, resolved against the specifier afterwards.
        let mut entries: Vec<(ImportName, String)> = Vec::new();
        if matches!(&self.current().kind, TokenKind::Identifier(_)) {
            let local = self.local_identifier()?;
            entries.push((ImportName::Named("default".to_owned()), local));
            if !self.take(&TokenKind::Comma) {
                let request = self.expect_from()?;
                self.finish_import(&request, entries);
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
        self.finish_import(&request, entries);
        Ok(())
    }

    fn finish_import(&mut self, request: &str, entries: Vec<(ImportName, String)>) {
        self.end_statement();
        for (imported, local) in entries {
            self.module_info().imports.push(ImportEntry {
                request: request.to_owned(),
                imported,
                local,
            });
        }
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
            self.end_statement();
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
            self.end_statement();
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
        self.end_statement();
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
            self.end_statement();
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
                let key = if self.take(&TokenKind::LeftBracket) {
                    let expression = self.assignment()?;
                    self.require(
                        &TokenKind::RightBracket,
                        "expected ']' after computed binding key",
                    )?;
                    PropertyKey::Computed(expression)
                } else {
                    PropertyKey::Static(self.property_name()?)
                };
                let mut pattern = if self.take(&TokenKind::Colon) {
                    self.binding_pattern()?
                } else if let PropertyKey::Static(property) = &key
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

    fn function_declaration(&mut self) -> Result<Statement, JsError> {
        let TokenKind::Identifier(name) = self.advance().kind else {
            return Err(self.error("expected a function name"));
        };
        let (parameters, body) = self.function_tail()?;
        Ok(Statement::Function {
            offset: self.previous_offset(),
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
            offset: self.previous_offset(),
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
        let body = self.statement();
        self.loop_depth = self.loop_depth.saturating_sub(1);
        Ok(Statement::While {
            offset: self.previous_offset(),
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
            offset: self.previous_offset(),
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
            let pattern = if self.at(&TokenKind::LeftBrace) || self.at(&TokenKind::LeftBracket) {
                Some(self.binding_pattern()?)
            } else {
                let TokenKind::Identifier(name) = self.advance().kind else {
                    return Err(self.error("expected an identifier after declaration keyword"));
                };
                Some(BindingPattern::Identifier(name))
            };
            if self.take(&TokenKind::In) {
                self.no_in = false;
                let iterable = self.expression()?;
                self.require(&TokenKind::RightParen, "expected ')' after for-in clauses")?;
                self.loop_depth = self.loop_depth.saturating_add(1);
                let body = self.statement();
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
            if matches!(&self.current().kind, TokenKind::Identifier(name) if name == "of") {
                self.advance();
                self.no_in = false;
                let iterable = self.expression()?;
                self.require(&TokenKind::RightParen, "expected ')' after for-of clauses")?;
                self.loop_depth = self.loop_depth.saturating_add(1);
                let body = self.statement();
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
                        });
                    }
                };
                return Ok(Statement::ForOf {
                    offset: self.previous_offset(),
                    kind,
                    name,
                    iterable,
                    body: Box::new(body),
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
                    offset: self.previous_offset(),
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
        // Async arrows have the same callable shape in this synchronous
        // runtime; consume the marker while retaining their parameter/body.
        if matches!(&self.current().kind, TokenKind::Identifier(name) if name == "async")
            && matches!(
                self.tokens.get(self.cursor + 1).map(|token| &token.kind),
                Some(TokenKind::LeftParen | TokenKind::Identifier(_))
            )
        {
            self.advance();
        }
        let mut patterns = Vec::new();
        let mut defaults: Vec<Statement> = Vec::new();
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
                            patterns.push((temporary.clone(), pattern));
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
                        defaults.push(Statement::ParameterDefault {
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

        let mut body = if self.take(&TokenKind::LeftBrace) {
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
        if !defaults.is_empty() {
            defaults.extend(body);
            body = defaults;
        }
        if !patterns.is_empty() {
            let mut declarations = Vec::new();
            for (temporary, pattern) in patterns {
                Self::lower_declarator(pattern, Expr::Identifier(temporary), &mut declarations);
            }
            body.insert(
                0,
                Statement::VariableList {
                    offset: self.previous_offset(),
                    kind: VariableKind::Var,
                    declarations,
                },
            );
        }
        Ok(Some(Expr::Arrow {
            offset: self.previous_offset(),
            parameters,
            body,
        }))
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
                offset: self.previous_offset(),
                target: Box::new(target),
                operator,
                value: Box::new(value),
            },
            None => Expr::Assignment {
                offset: self.previous_offset(),
                target: Box::new(target),
                value: Box::new(value),
            },
        })
    }

    fn validate_assignment_target(&self, target: &Expr) -> Result<(), JsError> {
        if matches!(
            target,
            Expr::Identifier(_)
                | Expr::Member { .. }
                | Expr::ComputedMember { .. }
                | Expr::Array(_)
                | Expr::Object(_)
                | Expr::PrivateMember { .. }
                | Expr::SuperMember { .. }
                | Expr::SuperComputedMember { .. }
        ) {
            Ok(())
        } else {
            Err(self.error("invalid assignment target"))
        }
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
            if self.class_depth == 0 {
                return Err(self.error("private names are only allowed in class bodies"));
            }
            let offset = self.current().offset;
            self.advance();
            self.advance();
            let object = self.unary()?;
            return Ok(Expr::PrivateIn {
                name,
                object: Box::new(object),
                offset,
            });
        }
        if matches!(&self.current().kind, TokenKind::Identifier(name) if name == "await") {
            // Promise suspension is outside the synchronous interpreter, but
            // await has unary expression precedence. Parsing it here avoids
            // mistaking nested `await (...)` expressions for calls to a
            // missing global named await.
            self.advance();
            return self.unary();
        }
        if matches!(&self.current().kind, TokenKind::Identifier(name) if name == "yield") {
            // Generator suspension is also outside the synchronous
            // interpreter, but modern bundles wrap `yield` in helper-driven
            // generator bodies that must still parse. The operand (a full
            // AssignmentExpression per ECMA-262) keeps its side effects; a
            // bare `yield` followed by a terminator stays an expression.
            self.advance();
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
            return if terminated {
                Ok(Expr::Literal(JsValue::Undefined))
            } else {
                self.assignment()
            };
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
            return Ok(Expr::Unary {
                offset: self.previous_offset(),
                operator,
                operand: Box::new(self.unary()?),
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
                if self.function_depth == 0 && self.class_depth == 0 {
                    return Err(self.error("new.target is only allowed inside functions"));
                }
                return self.postfix_tail(Expr::NewTarget);
            }
            // `new` binds to a whole member chain (`new A.B.C(...)`), so
            // consume dots/computed members BEFORE the argument list.
            let mut target = self.primary()?;
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
                    if self.class_depth == 0 {
                        return Err(self.error("private names are only allowed in class bodies"));
                    }
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
                    if self.class_depth == 0 {
                        return Err(self.error("private names are only allowed in class bodies"));
                    }
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

    fn arguments_after_left_paren(&mut self) -> Result<Vec<Expr>, JsError> {
        let mut arguments = Vec::new();
        if !self.at(&TokenKind::RightParen) {
            loop {
                let argument = if self.take(&TokenKind::Ellipsis) {
                    Expr::Spread(Box::new(self.assignment()?))
                } else {
                    self.assignment()?
                };
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
                let _ = self.take(&TokenKind::Star);
                self.function_expression()
            }
            TokenKind::Identifier(name) if name == "class" => {
                // An anonymous class may be followed by `extends`; only a
                // real identifier names the class.
                let name = if let TokenKind::Identifier(name) = self.current().kind.clone()
                    && name != "extends"
                {
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
            TokenKind::Identifier(name) if name == "super" => self.super_expression(token.offset),
            TokenKind::PrivateName(name) => Err(JsError::syntax(
                format!("private name #{name} must be followed by 'in'"),
                token.offset,
            )),
            TokenKind::Identifier(name) => Ok(Expr::Identifier(name)),
            TokenKind::This => Ok(Expr::This),
            TokenKind::String(value) => Ok(Expr::Literal(JsValue::String(value))),
            TokenKind::RegexLiteral { pattern, flags } => Ok(Expr::RegexLiteral {
                offset: self.previous_offset(),
                pattern,
                flags,
            }),
            TokenKind::Template(parts) => self.template_literal(parts, token.offset),
            TokenKind::Number(value) => Ok(Expr::Literal(JsValue::Number(value))),
            TokenKind::True => Ok(Expr::Literal(JsValue::Boolean(true))),
            TokenKind::False => Ok(Expr::Literal(JsValue::Boolean(false))),
            TokenKind::Null => Ok(Expr::Literal(JsValue::Null)),
            TokenKind::Undefined => Ok(Expr::Literal(JsValue::Undefined)),
            TokenKind::Function => {
                let _ = self.take(&TokenKind::Star);
                self.function_expression()
            }
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
            self.advance();
            let arguments = self.arguments_after_left_paren()?;
            return Ok(Expr::SuperCall { arguments, offset });
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
        self.class_depth = self.class_depth.saturating_add(1);
        let mut elements = Vec::new();
        while !self.at(&TokenKind::RightBrace) {
            if self.at(&TokenKind::Eof) {
                self.class_depth = self.class_depth.saturating_sub(1);
                return Err(self.error("unterminated class body"));
            }
            if self.take(&TokenKind::Semicolon) {
                continue;
            }
            match self.class_element() {
                Ok(element) => elements.push(element),
                Err(error) => {
                    self.class_depth = self.class_depth.saturating_sub(1);
                    return Err(error);
                }
            }
        }
        self.class_depth = self.class_depth.saturating_sub(1);
        self.require(&TokenKind::RightBrace, "expected '}' after class body")?;
        validate_class_elements(&elements)?;
        Ok((super_class, elements))
    }

    /// Parse one class body element: a method, accessor, constructor, field,
    /// or static initialization block.
    fn class_element(&mut self) -> Result<ClassElement, JsError> {
        let offset = self.current().offset;
        let static_start = self.current().offset;
        let mut is_static = false;
        if matches!(&self.current().kind, TokenKind::Identifier(name) if name == "static")
            && class_modifier_follows(self.tokens.get(self.cursor + 1).map(|token| &token.kind))
        {
            self.advance();
            is_static = true;
            if self.take(&TokenKind::LeftBrace) {
                let body = self.statement_list(true)?;
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
        let mut kind = ClassElementKind::Method;
        if matches!(&self.current().kind, TokenKind::Identifier(name) if name == "get" || name == "set")
            && class_modifier_follows(self.tokens.get(self.cursor + 1).map(|token| &token.kind))
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
        let mut is_async = false;
        if matches!(kind, ClassElementKind::Method)
            && matches!(&self.current().kind, TokenKind::Identifier(name) if name == "async")
            && class_modifier_follows(self.tokens.get(self.cursor + 1).map(|token| &token.kind))
        {
            self.advance();
            is_async = true;
        }
        let is_generator = self.take(&TokenKind::Star);
        let key = self.class_element_key()?;
        if self.at(&TokenKind::LeftParen) {
            let (parameters, body) = self.function_tail()?;
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
            Some(self.assignment()?)
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
        if self.take(&TokenKind::LeftBracket) {
            let key = self.assignment()?;
            self.require(
                &TokenKind::RightBracket,
                "expected ']' after computed class element name",
            )?;
            return Ok(PropertyKey::Computed(key));
        }
        Ok(PropertyKey::Static(self.property_name()?))
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
                operator: BinaryOp::Add,
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
        let expression = parser.expression()?;
        if !parser.at(&TokenKind::Eof) {
            return Err(JsError::syntax(
                "unexpected token in template interpolation",
                offset,
            ));
        }
        Ok(expression)
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
            offset: self.previous_offset(),
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
        let mut defaults: Vec<Statement> = Vec::new();
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
                // the pattern itself is lowered to a `var` declaration at the
                // top of the body, exactly as arrow functions do.
                let mut bound_names = Vec::new();
                if self.at(&TokenKind::LeftBrace) || self.at(&TokenKind::LeftBracket) {
                    let pattern = self.binding_pattern()?;
                    let temporary = format!("\0param_{}", parameters.len());
                    patterns.push((temporary.clone(), pattern));
                    bound_names.push(temporary);
                } else {
                    // `undefined` is an ordinary identifier in parameter position.
                    let parameter = match self.advance().kind {
                        TokenKind::Identifier(name) => name,
                        TokenKind::Undefined => "undefined".to_owned(),
                        _ => return Err(self.error("expected a parameter name")),
                    };
                    bound_names.push(parameter);
                }
                let has_default = self.take(&TokenKind::Equal);
                if has_default {
                    let value = self.assignment()?;
                    defaults.push(Statement::ParameterDefault {
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
        let mut body = body?;
        self.require(&TokenKind::RightBrace, "expected '}' after function body")?;
        if !defaults.is_empty() {
            defaults.extend(body);
            body = defaults;
        }
        if !patterns.is_empty() {
            let mut declarations = Vec::new();
            for (temporary, pattern) in patterns {
                Self::lower_declarator(pattern, Expr::Identifier(temporary), &mut declarations);
            }
            body.insert(
                0,
                Statement::VariableList {
                    offset: self.previous_offset(),
                    kind: VariableKind::Var,
                    declarations,
                },
            );
        }
        Ok((parameters, body))
    }

    fn object_literal(&mut self) -> Result<Expr, JsError> {
        let mut properties = Vec::new();
        if !self.at(&TokenKind::RightBrace) {
            loop {
                if self.take(&TokenKind::Ellipsis) {
                    properties.push(ObjectProperty {
                        key: PropertyKey::Spread,
                        value: self.assignment()?,
                        accessor: None,
                        shorthand: false,
                        method: false,
                    });
                    if !self.take(&TokenKind::Comma) {
                        break;
                    }
                    if self.at(&TokenKind::RightBrace) {
                        break;
                    }
                    continue;
                }
                if matches!(&self.current().kind, TokenKind::Identifier(name) if name == "async")
                    && matches!(
                        self.tokens.get(self.cursor + 2).map(|token| &token.kind),
                        Some(TokenKind::LeftParen)
                    )
                {
                    self.advance();
                    let key = self.property_name()?;
                    let (parameters, body) = self.function_tail()?;
                    properties.push(ObjectProperty {
                        key: PropertyKey::Static(key.clone()),
                        value: Expr::Function {
                            offset: self.previous_offset(),
                            name: Some(key),
                            parameters,
                            body,
                        },
                        accessor: None,
                        shorthand: false,
                        method: false,
                    });
                    if !self.take(&TokenKind::Comma) {
                        break;
                    }
                    if self.at(&TokenKind::RightBrace) {
                        break;
                    }
                    continue;
                }
                if self.take(&TokenKind::LeftBracket) {
                    let key = self.assignment()?;
                    self.require(
                        &TokenKind::RightBracket,
                        "expected ']' after computed property name",
                    )?;
                    let value = if self.at(&TokenKind::LeftParen) {
                        let (parameters, body) = self.function_tail()?;
                        Expr::Function {
                            offset: self.previous_offset(),
                            name: None,
                            parameters,
                            body,
                        }
                    } else {
                        self.require(
                            &TokenKind::Colon,
                            "expected ':' after computed property name",
                        )?;
                        self.assignment()?
                    };
                    properties.push(ObjectProperty {
                        key: PropertyKey::Computed(key),
                        value,
                        accessor: None,
                        shorthand: false,
                        method: false,
                    });
                    if !self.take(&TokenKind::Comma) {
                        break;
                    }
                    if self.at(&TokenKind::RightBrace) {
                        break;
                    }
                    continue;
                }
                let accessor_kind = match &self.current().kind {
                    TokenKind::Identifier(name)
                        if (name == "get" || name == "set")
                            && !matches!(
                                self.tokens.get(self.cursor + 1).map(|token| &token.kind),
                                Some(TokenKind::LeftParen | TokenKind::Colon | TokenKind::Comma)
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
                if let Some(accessor_kind) = accessor_kind {
                    self.advance();
                    let key = self.property_name()?;
                    let (parameters, body) = self.function_tail()?;
                    properties.push(ObjectProperty {
                        key: PropertyKey::Static(key.clone()),
                        value: Expr::Function {
                            offset: self.previous_offset(),
                            name: Some(key),
                            parameters,
                            body,
                        },
                        accessor: Some(accessor_kind),
                        shorthand: false,
                        method: false,
                    });
                    if !self.take(&TokenKind::Comma) {
                        break;
                    }
                    if self.at(&TokenKind::RightBrace) {
                        break;
                    }
                    continue;
                }
                let key = self.property_name()?;
                let (value, shorthand, method) = if self.take(&TokenKind::Colon) {
                    (self.assignment()?, false, false)
                } else if self.at(&TokenKind::LeftParen) {
                    let (parameters, body) = self.function_tail()?;
                    (
                        Expr::Function {
                            offset: self.previous_offset(),
                            name: Some(key.clone()),
                            parameters,
                            body,
                        },
                        false,
                        true,
                    )
                } else {
                    (Expr::Identifier(key.clone()), true, false)
                };
                properties.push(ObjectProperty {
                    key: PropertyKey::Static(key),
                    value,
                    accessor: None,
                    shorthand,
                    method,
                });
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
        Statement::VariableList { declarations, .. } => declarations
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
        Expr::Object(properties) => properties.iter().try_for_each(|property| {
            if let PropertyKey::Computed(key) = &property.key {
                validate_strict_expression(key)?;
            }
            validate_strict_expression(&property.value)
        }),
        Expr::Array(elements) => elements.iter().try_for_each(validate_strict_expression),
        Expr::Spread(expression)
        | Expr::OptionalChain(expression)
        | Expr::OptionalGuard(expression) => validate_strict_expression(expression),

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
        Statement::VariableList { declarations, .. } => {
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
                check_reserved_identifier(&catch.parameter, context)?;
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
        Expr::Function {
            name,
            parameters,
            body,
            ..
        } => {
            if let Some(name) = name {
                check_reserved_identifier(name, context)?;
            }
            validate_reserved_parameters(parameters, context)?;
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
        Expr::Spread(inner) | Expr::OptionalChain(inner) | Expr::OptionalGuard(inner) => {
            validate_reserved_expression(inner, context)?;
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
