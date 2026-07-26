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

pub use runtime::JsRuntime;
pub use value::{JsObject, JsValue, ObjectId, PropertyDescriptor, Realm};

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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsError {
    kind: JsErrorKind,
    message: String,
    offset: Option<usize>,
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

    pub(crate) fn new(
        kind: JsErrorKind,
        message: impl Into<String>,
        offset: Option<usize>,
    ) -> Self {
        Self {
            kind,
            message: message.into(),
            offset,
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

/// Observable result of running one script.
#[derive(Clone, Debug, PartialEq)]
pub struct ScriptOutcome {
    pub value: JsValue,
    pub from_revision: DomRevision,
    pub to_revision: DomRevision,
}

#[cfg(test)]
mod tests {
    use super::{JsErrorKind, JsRuntime, RuntimeLimits};
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
