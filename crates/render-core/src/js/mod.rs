//! Resource-bounded JavaScript interpreter foundation and direct DOM bindings.
//!
//! This is deliberately a standards-oriented vertical slice, not a claim of
//! ECMAScript conformance. Unsupported syntax produces an explicit error. DOM
//! host objects mutate the existing arena so downstream rendering can consume
//! its [`crate::dom::MutationBatch`] without reparsing HTML.

mod lexer;
mod parser;
mod runtime;
mod value;

use std::error::Error;
use std::fmt;

pub use runtime::{JsMicrotask, JsRuntime};
pub use value::{JsObject, JsValue, ObjectId, PropertyDescriptor, Realm};

use parser::Statement;

use crate::dom::DomRevision;

/// Hard limits applied independently to every script execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeLimits {
    pub max_source_bytes: usize,
    pub max_tokens: usize,
    pub max_statements: usize,
    pub max_execution_steps: usize,
    pub max_call_depth: usize,
    pub max_heap_objects: usize,
    pub max_dom_nodes_created: usize,
}

impl Default for RuntimeLimits {
    fn default() -> Self {
        Self {
            max_source_bytes: 256 * 1_024,
            max_tokens: 32_768,
            max_statements: 4_096,
            max_execution_steps: 100_000,
            max_call_depth: 64,
            max_heap_objects: 16_384,
            max_dom_nodes_created: 4_096,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsErrorKind {
    Syntax,
    Reference,
    Type,
    Dom,
    ResourceLimit,
    Throw,
}

#[derive(Clone, Debug, PartialEq)]
pub struct JsError {
    kind: JsErrorKind,
    message: String,
    offset: Option<usize>,
    thrown: Option<JsValue>,
}

impl JsError {
    #[must_use]
    pub const fn kind(&self) -> JsErrorKind {
        self.kind
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    #[must_use]
    pub const fn offset(&self) -> Option<usize> {
        self.offset
    }

    #[must_use]
    pub fn thrown_value(&self) -> Option<&JsValue> {
        self.thrown.as_ref()
    }

    pub(crate) fn new(
        kind: JsErrorKind,
        message: impl Into<String>,
        offset: Option<usize>,
    ) -> Self {
        Self {
            kind,
            message: message.into(),
            offset,
            thrown: None,
        }
    }

    pub(crate) fn syntax(message: impl Into<String>, offset: usize) -> Self {
        Self::new(JsErrorKind::Syntax, message, Some(offset))
    }

    pub(crate) fn reference(message: impl Into<String>) -> Self {
        Self::new(JsErrorKind::Reference, message, None)
    }

    pub(crate) fn type_error(message: impl Into<String>) -> Self {
        Self::new(JsErrorKind::Type, message, None)
    }

    pub(crate) fn dom(message: impl Into<String>) -> Self {
        Self::new(JsErrorKind::Dom, message, None)
    }

    pub(crate) fn resource(message: impl Into<String>) -> Self {
        Self::new(JsErrorKind::ResourceLimit, message, None)
    }

    pub(crate) fn thrown(value: JsValue) -> Self {
        Self {
            kind: JsErrorKind::Throw,
            message: value.to_js_string(),
            offset: None,
            thrown: Some(value),
        }
    }
}

impl fmt::Display for JsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(offset) = self.offset {
            write!(
                formatter,
                "{:?} at byte {offset}: {}",
                self.kind, self.message
            )
        } else {
            write!(formatter, "{:?}: {}", self.kind, self.message)
        }
    }
}

impl Error for JsError {}

/// Parsed script reusable across isolated realms.
///
/// Compilation enforces source, token, and statement limits. Execution applies
/// the target runtime's independent step, call-depth, heap, and DOM limits.
#[derive(Clone, Debug, PartialEq)]
pub struct CompiledScript {
    statements: Vec<Statement>,
}

impl CompiledScript {
    /// Tokenize and parse one classic script without creating a Realm.
    ///
    /// # Errors
    ///
    /// Returns a typed syntax or resource-limit error. Unsupported syntax is
    /// never ignored or deferred until execution.
    pub fn compile(source: &str, limits: &RuntimeLimits) -> Result<Self, JsError> {
        let tokens = lexer::tokenize(source, limits)?;
        let statements = parser::parse(tokens, limits)?;
        Ok(Self { statements })
    }
}

/// Observable result of running one script.
#[derive(Clone, Debug, PartialEq)]
pub struct ScriptOutcome {
    pub value: JsValue,
    pub from_revision: DomRevision,
    pub to_revision: DomRevision,
}

#[cfg(test)]
mod tests {
    use super::{CompiledScript, JsErrorKind, JsRuntime, RuntimeLimits};
    use crate::dom::{MutationKind, NodeKind};
    use crate::html::parse_document;

    fn element_with_id(dom: &crate::dom::Dom, id: &str) -> crate::dom::NodeId {
        let mut pending = vec![dom.document()];
        while let Some(node) = pending.pop() {
            if matches!(
                dom.node(node).map(crate::dom::Node::kind),
                Some(NodeKind::Element(_))
            ) && dom.attribute(node, "id").ok().flatten() == Some(id)
            {
                return node;
            }
            pending.extend(dom.children(node).unwrap_or_default().iter().rev());
        }
        panic!("test element #{id} should exist");
    }

    #[test]
    fn script_mutates_the_existing_dom_and_journal() {
        let mut parsed =
            parse_document("<!doctype html><html><body><p id='message'>old</p></body></html>");
        let message = element_with_id(&parsed.dom, "message");
        let before = parsed.dom.revision();
        let mut runtime = JsRuntime::new(&parsed.dom);

        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r#"
                    const target = document.getElementById("message");
                    target.textContent = "updated";
                    target.setAttribute("class", "live");
                    const badge = document.createElement("span");
                    badge.textContent = "!";
                    target.appendChild(badge);
                "#,
            )
            .expect("supported DOM script should execute");

        assert_eq!(outcome.from_revision, before);
        assert_eq!(outcome.to_revision, parsed.dom.revision());
        assert!(outcome.to_revision > before);
        assert_eq!(parsed.dom.attribute(message, "class"), Ok(Some("live")));
        let batch = parsed
            .dom
            .mutations_since(before)
            .expect("script mutations should remain in the journal");
        assert!(batch.records.iter().any(|record| matches!(
            record.kind,
            MutationKind::Attribute { target, .. } if target == message
        )));
        assert!(batch.records.iter().any(|record| matches!(
            &record.kind,
            MutationKind::ChildList { target, added, .. }
                if *target == message && !added.is_empty()
        )));
        let text = parsed
            .dom
            .children(message)
            .unwrap_or_default()
            .iter()
            .filter_map(|child| parsed.dom.node(*child))
            .filter_map(|node| match node.kind() {
                NodeKind::Text(value) => Some(value.as_str()),
                _ => None,
            })
            .collect::<String>();
        assert_eq!(text, "updated");
    }

    #[test]
    fn compiled_script_can_run_in_isolated_realms() {
        let limits = RuntimeLimits::default();
        let script = CompiledScript::compile(
            "var counter = typeof counter === 'undefined' ? 1 : counter + 1; counter;",
            &limits,
        )
        .expect("supported script should compile once");
        let mut first = parse_document("<!doctype html><p>first</p>");
        let mut second = parse_document("<!doctype html><p>second</p>");
        let mut first_runtime = JsRuntime::with_limits(&first.dom, limits);
        let mut second_runtime = JsRuntime::with_limits(&second.dom, limits);

        let first_value = first_runtime
            .execute_compiled(&mut first.dom, &script)
            .expect("first Realm executes")
            .value;
        let repeated_value = first_runtime
            .execute_compiled(&mut first.dom, &script)
            .expect("same Realm preserves globals")
            .value;
        let isolated_value = second_runtime
            .execute_compiled(&mut second.dom, &script)
            .expect("second Realm executes independently")
            .value;

        assert_eq!(first_value, super::JsValue::Number(1.0));
        assert_eq!(repeated_value, super::JsValue::Number(2.0));
        assert_eq!(isolated_value, super::JsValue::Number(1.0));
    }

    #[test]
    fn selector_class_list_and_dom_insertion_drive_the_shared_tree() {
        let mut parsed = parse_document(
            "<!doctype html><main id='app'><article class='card first'>one</article><article class='card'>two</article></main>",
        );
        let mut runtime = JsRuntime::new(&parsed.dom);

        runtime
            .execute(
                &mut parsed.dom,
                r##"
                    const root = document.querySelector("#app");
                    const cards = root.querySelectorAll("article.card");
                    cards[0].classList.add("selected", "visible");
                    cards[1].classList.toggle("card", false);
                    const badge = document.createElement("span");
                    badge.className = "badge";
                    badge.textContent = "ok";
                    root.insertBefore(badge, cards[0]);
                    const selected = root.querySelector(".selected");
                    const result = selected.classList.contains("visible") &&
                        selected.classList.item(0) === "card" &&
                        selected.classList.toString() === "card first selected visible" &&
                        root.querySelectorAll(".card").length === 1 &&
                        root.firstChild.className === "badge" &&
                        root.textContent === "okonetwo";
                "##,
            )
            .expect("selector and classList APIs should mutate the shared DOM");

        assert_eq!(
            runtime.realm().global("result"),
            Some(super::JsValue::Boolean(true))
        );
    }

    #[test]
    fn click_dispatches_listener_property_handler_and_bubbles() {
        let mut parsed =
            parse_document("<!doctype html><main id='app'><button id='go'>go</button></main>");
        let mut runtime = JsRuntime::new(&parsed.dom);

        runtime
            .execute(
                &mut parsed.dom,
                r##"
                    const app = document.querySelector("#app");
                    const button = document.querySelector("#go");
                    let log = "";
                    app.addEventListener("click", function(event) {
                        log = log + event.type + event.target.id + event.currentTarget.id;
                    });
                    button.addEventListener("click", function() { log = log + "listener"; });
                    button.onclick = function() { log = log + "property"; };
                    button.click();
                    const result = log;
                "##,
            )
            .expect("click should invoke listeners, onclick, and bubbling ancestors");

        assert_eq!(
            runtime.realm().global("result"),
            Some(super::JsValue::String(
                "listenerpropertyclickgoapp".to_owned(),
            ))
        );
    }

    #[test]
    fn declaration_lists_are_instantiated_without_internal_panics() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);

        let global = runtime
            .execute(
                &mut parsed.dom,
                "let first = 1, second = 2; const third = 3, fourth = 4; first + second + third + fourth;",
            )
            .expect("global declaration lists should instantiate every lexical binding");
        assert_eq!(global.value, super::JsValue::Number(10.0));

        let block = runtime
            .execute(
                &mut parsed.dom,
                "{ let fifth = 5, sixth = 6; const seventh = 7, eighth = 8; fifth + sixth + seventh + eighth; }",
            )
            .expect("block declaration lists should instantiate every lexical binding");
        assert_eq!(block.value, super::JsValue::Number(26.0));
    }

    #[test]
    fn arrow_functions_support_expression_bodies_closures_and_lexical_this() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r"
                    var addArrow = (left, right) => left + right;
                    var makeAdderArrow = base => value => base + value;
                    var holder = {
                        value: 40,
                        read: function() {
                            var arrow = () => this.value;
                            return arrow();
                        }
                    };
                    addArrow(1, 2) + makeAdderArrow(4)(5) + holder.read();
                ",
            )
            .expect("arrow functions should execute");
        assert_eq!(outcome.value, super::JsValue::Number(52.0));

        let error = runtime
            .execute(&mut parsed.dom, "new (() => 1); ")
            .expect_err("arrow functions are not constructors");
        assert_eq!(error.kind(), JsErrorKind::Type);
    }

    #[test]
    fn update_expressions_and_member_assignments_evaluate_references_once() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r"
                    var calls = 0;
                    var item = { value: 4 };
                    function getItem() { calls += 1; return item; }
                    var postfix = getItem().value++;
                    var prefix = ++getItem().value;
                    getItem().value = calls;
                    postfix * 1000 + prefix * 100 + item.value * 10 + calls;
                ",
            )
            .expect("updates and assignments should execute");

        assert_eq!(outcome.value, super::JsValue::Number(4_633.0));
    }

    #[test]
    fn execution_step_limit_is_reported() {
        let mut parsed = parse_document("<!doctype html><p id='x'>x</p>");
        let limits = RuntimeLimits {
            max_execution_steps: 2,
            ..RuntimeLimits::default()
        };
        let mut runtime = JsRuntime::with_limits(&parsed.dom, limits);
        let error = runtime
            .execute(&mut parsed.dom, "document.getElementById('x');")
            .expect_err("small execution budget should stop traversal");
        assert_eq!(error.kind(), JsErrorKind::ResourceLimit);
    }
}
