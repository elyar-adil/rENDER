use super::parser::{Expr, Statement};
use super::value::{NativeFunction, ObjectHost};
use super::{JsError, JsErrorKind, JsValue, ObjectId, Realm, RuntimeLimits, ScriptOutcome};
use crate::dom::{Dom, DomError, NodeId, NodeKind};

/// A realm-owning interpreter instance. DOM wrappers retain stable `NodeId`
/// identities, never Rust references, across calls to [`Self::execute`].
#[derive(Debug)]
pub struct JsRuntime {
    realm: Realm,
    limits: RuntimeLimits,
    steps_remaining: usize,
    calls_active: usize,
    dom_nodes_created: usize,
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
            limits,
        }
    }

    #[must_use]
    pub const fn realm(&self) -> &Realm {
        &self.realm
    }

    /// Parse and run one script against the existing DOM arena.
    ///
    /// # Errors
    ///
    /// Returns a typed syntax/runtime/DOM/resource-limit error. Unsupported
    /// syntax is never silently ignored.
    pub fn execute(&mut self, dom: &mut Dom, source: &str) -> Result<ScriptOutcome, JsError> {
        let from_revision = dom.revision();
        self.steps_remaining = self.limits.max_execution_steps;
        self.calls_active = 0;
        self.dom_nodes_created = 0;
        let tokens = super::lexer::tokenize(source, &self.limits)?;
        let statements = super::parser::parse(tokens, &self.limits)?;
        let mut value = JsValue::Undefined;
        for statement in &statements {
            value = self.evaluate_statement(dom, statement)?;
        }
        Ok(ScriptOutcome {
            value,
            from_revision,
            to_revision: dom.revision(),
        })
    }

    fn evaluate_statement(
        &mut self,
        dom: &mut Dom,
        statement: &Statement,
    ) -> Result<JsValue, JsError> {
        self.consume_step()?;
        match statement {
            Statement::Variable { name, value } => {
                let value = match value {
                    Some(expression) => self.evaluate(dom, expression)?,
                    None => JsValue::Undefined,
                };
                if !self.realm.set_global(name.clone(), value.clone()) {
                    return Err(JsError::type_error(format!(
                        "global property {name:?} is not writable"
                    )));
                }
                Ok(value)
            }
            Statement::Expression(expression) => self.evaluate(dom, expression),
        }
    }

    fn evaluate(&mut self, dom: &mut Dom, expression: &Expr) -> Result<JsValue, JsError> {
        self.consume_step()?;
        match expression {
            Expr::Literal(value) => Ok(value.clone()),
            Expr::Identifier(name) => self
                .realm
                .global(name)
                .ok_or_else(|| JsError::reference(format!("{name} is not defined"))),
            Expr::Member { object, property } => {
                let evaluated = self.evaluate(dom, object)?;
                let object = Self::require_object(&evaluated)?;
                self.get_member(dom, object, property)
            }
            Expr::Call { callee, arguments } => {
                let evaluated = self.evaluate(dom, callee)?;
                let callee = Self::require_object(&evaluated)?;
                let mut values = Vec::with_capacity(arguments.len());
                for argument in arguments {
                    values.push(self.evaluate(dom, argument)?);
                }
                self.call(dom, callee, &values)
            }
            Expr::Assignment { target, value } => {
                let value = self.evaluate(dom, value)?;
                match target.as_ref() {
                    Expr::Identifier(name) => {
                        if !self.realm.set_global(name.clone(), value.clone()) {
                            return Err(JsError::type_error(format!(
                                "global property {name:?} is not writable"
                            )));
                        }
                    }
                    Expr::Member { object, property } => {
                        let evaluated = self.evaluate(dom, object)?;
                        let object = Self::require_object(&evaluated)?;
                        self.set_member(dom, object, property, value.clone())?;
                    }
                    _ => return Err(JsError::syntax("invalid assignment target", 0)),
                }
                Ok(value)
            }
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

    fn call(
        &mut self,
        dom: &mut Dom,
        callee: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        self.consume_step()?;
        let Some(ObjectHost::BoundFunction { function, receiver }) = self.realm.host(callee) else {
            return Err(JsError::type_error("value is not callable"));
        };
        if self.calls_active >= self.limits.max_call_depth {
            return Err(JsError::resource("maximum call depth exceeded"));
        }
        self.calls_active = self.calls_active.saturating_add(1);
        let result = self.call_native(dom, function, receiver, arguments);
        self.calls_active = self.calls_active.saturating_sub(1);
        result
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
