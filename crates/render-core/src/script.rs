//! Revision-bound discovery and preparation metadata for HTML script elements.
//!
//! This module performs no network I/O and executes no JavaScript. It turns a
//! parsed document into an ordered, bounded plan that browser coordinators can
//! fetch and execute without reparsing HTML or silently ignoring unsupported
//! script semantics.

use url::Url;

use crate::document::Document;
use crate::dom::{Dom, DomRevision, ElementData, NodeId, NodeKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScriptDiscoveryLimits {
    pub max_visited_nodes: usize,
    pub max_script_elements: usize,
    pub max_inline_script_bytes: usize,
    pub max_external_url_bytes: usize,
}

impl Default for ScriptDiscoveryLimits {
    fn default() -> Self {
        Self {
            max_visited_nodes: 1_000_000,
            max_script_elements: 4_096,
            max_inline_script_bytes: 16 * 1_024 * 1_024,
            max_external_url_bytes: 1024 * 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScriptScheduling {
    ParserBlocking,
    Defer,
    Async,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScriptSource {
    Inline { source: String },
    External { src: String, resolved_url: Url },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClassicScript {
    pub owner: NodeId,
    pub source_order: usize,
    pub scheduling: ScriptScheduling,
    pub source: ScriptSource,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScriptDiagnosticCode {
    NodeLimit,
    ScriptElementLimit,
    InlineBytesLimit,
    ExternalUrlBytesLimit,
    EmptyExternalSource,
    UnresolvedExternalSource,
    ImportMapUnsupported,
    UnsupportedType,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptDiagnostic {
    pub owner: Option<NodeId>,
    pub source_order: Option<usize>,
    pub code: ScriptDiagnosticCode,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptDiscovery {
    pub revision: DomRevision,
    pub scripts: Vec<ClassicScript>,
    pub diagnostics: Vec<ScriptDiagnostic>,
}

/// Discover executable scripts in DOM tree order.
///
/// Inline, external, and module scripts are represented uniformly. Module
/// scripts use deferred scheduling and execute in the page realm; import-map
/// semantics remain an explicit diagnostic until resolution maps are added.
#[must_use]
pub fn discover_scripts(
    document: &Document,
    base_url: &Url,
    limits: ScriptDiscoveryLimits,
) -> ScriptDiscovery {
    discover_dom_scripts(document.dom(), base_url, limits)
}

fn discover_dom_scripts(
    dom: &Dom,
    base_url: &Url,
    limits: ScriptDiscoveryLimits,
) -> ScriptDiscovery {
    let mut state = DiscoveryState::new(dom.revision());
    let mut stack = vec![dom.document()];
    let mut visited = 0_usize;

    while let Some(node_id) = stack.pop() {
        if visited >= limits.max_visited_nodes {
            state.diagnostics.push(ScriptDiagnostic {
                owner: None,
                source_order: None,
                code: ScriptDiagnosticCode::NodeLimit,
                message: format!(
                    "script discovery stopped after visiting {} DOM nodes",
                    limits.max_visited_nodes
                ),
            });
            break;
        }
        visited = visited.saturating_add(1);
        let Some(node) = dom.node(node_id) else {
            continue;
        };
        if let NodeKind::Element(element) = node.kind()
            && element.local_name == "script"
        {
            state.inspect_script(dom, node_id, element, base_url, limits);
        }
        if !matches!(node.kind(), NodeKind::Element(element) if element.local_name == "template") {
            stack.extend(node.children().iter().rev().copied());
        }
    }
    state.finish()
}

struct DiscoveryState {
    revision: DomRevision,
    scripts: Vec<ClassicScript>,
    diagnostics: Vec<ScriptDiagnostic>,
    source_order: usize,
    inline_bytes: usize,
    external_url_bytes: usize,
    limit_reported: bool,
}

impl DiscoveryState {
    const fn new(revision: DomRevision) -> Self {
        Self {
            revision,
            scripts: Vec::new(),
            diagnostics: Vec::new(),
            source_order: 0,
            inline_bytes: 0,
            external_url_bytes: 0,
            limit_reported: false,
        }
    }

    fn inspect_script(
        &mut self,
        dom: &Dom,
        owner: NodeId,
        element: &ElementData,
        base_url: &Url,
        limits: ScriptDiscoveryLimits,
    ) {
        let source_order = self.source_order;
        self.source_order = self.source_order.saturating_add(1);
        if !self.accept_element(owner, source_order, limits.max_script_elements) {
            return;
        }
        let Some(scheduling) = self.classify_script(owner, source_order, element) else {
            return;
        };
        let Some(source) = self.resolve_source(dom, owner, source_order, element, base_url, limits)
        else {
            return;
        };
        self.scripts.push(ClassicScript {
            owner,
            source_order,
            scheduling,
            source,
        });
    }

    fn accept_element(&mut self, owner: NodeId, source_order: usize, limit: usize) -> bool {
        if source_order < limit {
            return true;
        }
        if !self.limit_reported {
            self.diagnose(
                owner,
                source_order,
                ScriptDiagnosticCode::ScriptElementLimit,
                format!("script element limit of {limit} was reached"),
            );
            self.limit_reported = true;
        }
        false
    }

    fn classify_script(
        &mut self,
        owner: NodeId,
        source_order: usize,
        element: &ElementData,
    ) -> Option<ScriptScheduling> {
        let script_type = attribute(element, "type").map_or("", str::trim);
        let unsupported = if script_type.eq_ignore_ascii_case("importmap") {
            Some((
                ScriptDiagnosticCode::ImportMapUnsupported,
                "import maps are not implemented".to_owned(),
            ))
        } else if !script_type.eq_ignore_ascii_case("module")
            && !is_classic_javascript_type(script_type)
        {
            Some((
                ScriptDiagnosticCode::UnsupportedType,
                format!(
                    "script type {script_type:?} is not a supported classic JavaScript MIME type"
                ),
            ))
        } else {
            None
        };
        if let Some((code, message)) = unsupported {
            self.diagnose(owner, source_order, code, message);
            None
        } else {
            // Module scripts are deferred by definition. The JavaScript
            // runtime accepts their import/export grammar as a single realm
            // script; static imports are handled by the parser's module
            // recovery path and dynamic import is provided by the realm.
            let is_module = script_type.eq_ignore_ascii_case("module");
            Some(if is_module {
                ScriptScheduling::Defer
            } else if has_attribute(element, "nomodule") {
                // A module-capable browser must not execute the legacy
                // fallback. `nomodule` is a presence attribute.
                return None;
            } else if has_attribute(element, "src") && has_attribute(element, "async") {
                ScriptScheduling::Async
            } else if has_attribute(element, "src") && has_attribute(element, "defer") {
                ScriptScheduling::Defer
            } else {
                ScriptScheduling::ParserBlocking
            })
        }
    }

    fn resolve_source(
        &mut self,
        dom: &Dom,
        owner: NodeId,
        source_order: usize,
        element: &ElementData,
        base_url: &Url,
        limits: ScriptDiscoveryLimits,
    ) -> Option<ScriptSource> {
        match attribute(element, "src") {
            Some(src) => self.resolve_external_source(owner, source_order, src, base_url, limits),
            None => self.resolve_inline_source(dom, owner, source_order, limits),
        }
    }

    fn resolve_external_source(
        &mut self,
        owner: NodeId,
        source_order: usize,
        src: &str,
        base_url: &Url,
        limits: ScriptDiscoveryLimits,
    ) -> Option<ScriptSource> {
        let src = src.trim();
        if src.is_empty() {
            self.diagnose(
                owner,
                source_order,
                ScriptDiagnosticCode::EmptyExternalSource,
                "external script has an empty src attribute",
            );
            return None;
        }
        if self
            .external_url_bytes
            .checked_add(src.len())
            .is_none_or(|total| total > limits.max_external_url_bytes)
        {
            self.diagnose(
                owner,
                source_order,
                ScriptDiagnosticCode::ExternalUrlBytesLimit,
                format!(
                    "external script URL bytes exceed the {} byte discovery limit",
                    limits.max_external_url_bytes
                ),
            );
            return None;
        }
        let resolved_url = base_url.join(src).map_err(|_| ()).ok().or_else(|| {
            self.diagnose(
                owner,
                source_order,
                ScriptDiagnosticCode::UnresolvedExternalSource,
                format!("external script URL {src:?} could not be resolved"),
            );
            None
        })?;
        self.external_url_bytes = self.external_url_bytes.saturating_add(src.len());
        Some(ScriptSource::External {
            src: src.to_owned(),
            resolved_url,
        })
    }

    fn resolve_inline_source(
        &mut self,
        dom: &Dom,
        owner: NodeId,
        source_order: usize,
        limits: ScriptDiscoveryLimits,
    ) -> Option<ScriptSource> {
        let source = descendant_text(dom, owner);
        if self
            .inline_bytes
            .checked_add(source.len())
            .is_none_or(|total| total > limits.max_inline_script_bytes)
        {
            self.diagnose(
                owner,
                source_order,
                ScriptDiagnosticCode::InlineBytesLimit,
                format!(
                    "inline script bytes exceed the {} byte discovery limit",
                    limits.max_inline_script_bytes
                ),
            );
            return None;
        }
        self.inline_bytes = self.inline_bytes.saturating_add(source.len());
        Some(ScriptSource::Inline { source })
    }

    fn diagnose(
        &mut self,
        owner: NodeId,
        source_order: usize,
        code: ScriptDiagnosticCode,
        message: impl Into<String>,
    ) {
        self.diagnostics.push(ScriptDiagnostic {
            owner: Some(owner),
            source_order: Some(source_order),
            code,
            message: message.into(),
        });
    }

    fn finish(self) -> ScriptDiscovery {
        ScriptDiscovery {
            revision: self.revision,
            scripts: self.scripts,
            diagnostics: self.diagnostics,
        }
    }
}

fn attribute<'a>(element: &'a ElementData, name: &str) -> Option<&'a str> {
    element
        .attributes
        .iter()
        .find(|attribute| attribute.namespace.is_none() && attribute.local_name == name)
        .map(|attribute| attribute.value.as_str())
}

fn has_attribute(element: &ElementData, name: &str) -> bool {
    attribute(element, name).is_some()
}

fn is_classic_javascript_type(value: &str) -> bool {
    value.is_empty()
        || [
            "application/ecmascript",
            "application/javascript",
            "application/x-ecmascript",
            "application/x-javascript",
            "text/ecmascript",
            "text/javascript",
            "text/javascript1.0",
            "text/javascript1.1",
            "text/javascript1.2",
            "text/javascript1.3",
            "text/javascript1.4",
            "text/javascript1.5",
            "text/jscript",
            "text/livescript",
            "text/x-ecmascript",
            "text/x-javascript",
        ]
        .iter()
        .any(|mime| value.eq_ignore_ascii_case(mime))
}

fn descendant_text(dom: &Dom, root: NodeId) -> String {
    let mut output = String::new();
    let mut stack = dom
        .node(root)
        .map(|node| node.children().iter().rev().copied().collect::<Vec<_>>())
        .unwrap_or_default();
    while let Some(node_id) = stack.pop() {
        let Some(node) = dom.node(node_id) else {
            continue;
        };
        if let NodeKind::Text(text) = node.kind() {
            output.push_str(text);
        }
        stack.extend(node.children().iter().rev().copied());
    }
    output
}

#[cfg(test)]
mod tests {
    use url::Url;

    use super::{
        ScriptDiagnosticCode, ScriptDiscoveryLimits, ScriptScheduling, ScriptSource,
        discover_scripts,
    };
    use crate::document::Document;

    fn base_url() -> Url {
        Url::parse("https://example.test/path/page.html").expect("test URL")
    }

    #[test]
    fn discovers_inline_and_external_classic_scripts_in_dom_order() {
        let document = Document::parse(
            "<!doctype html><script>var first = 1;</script>\
             <template><script>var hidden = 1;</script></template>\
             <script src='../app.js'></script><script type='text/javascript'>var third = 3;</script>",
        );
        let discovery = discover_scripts(&document, &base_url(), ScriptDiscoveryLimits::default());

        assert_eq!(discovery.revision, document.dom().revision());
        assert_eq!(discovery.scripts.len(), 3);
        assert_eq!(discovery.scripts[0].source_order, 0);
        assert_eq!(discovery.scripts[1].source_order, 1);
        assert_eq!(discovery.scripts[2].source_order, 2);
        assert!(
            discovery
                .scripts
                .iter()
                .all(|script| script.scheduling == ScriptScheduling::ParserBlocking)
        );
        assert!(matches!(
            &discovery.scripts[0].source,
            ScriptSource::Inline { source } if source == "var first = 1;"
        ));
        assert!(matches!(
            &discovery.scripts[1].source,
            ScriptSource::External { resolved_url, .. }
                if resolved_url.as_str() == "https://example.test/app.js"
        ));
        assert!(discovery.diagnostics.is_empty());
    }

    #[test]
    fn discovers_async_and_defer_while_rejecting_unsupported_types() {
        let document = Document::parse(
            "<script type=module src=m.js></script>\
             <script async src=a.js></script>\
             <script defer src=d.js></script>\
             <script nomodule src=legacy.js></script>\
             <script async>var inline_async = true;</script>\
             <script defer>var inline_defer = true;</script>\
             <script type=application/json>{}</script>",
        );
        let discovery = discover_scripts(&document, &base_url(), ScriptDiscoveryLimits::default());
        let codes = discovery
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect::<Vec<_>>();

        assert_eq!(
            discovery
                .scripts
                .iter()
                .map(|script| script.scheduling)
                .collect::<Vec<_>>(),
            [
                ScriptScheduling::Defer,
                ScriptScheduling::Async,
                ScriptScheduling::Defer,
                ScriptScheduling::ParserBlocking,
                ScriptScheduling::ParserBlocking,
                ScriptScheduling::ParserBlocking,
            ]
        );
        assert_eq!(codes, [ScriptDiagnosticCode::UnsupportedType,]);
    }

    #[test]
    fn byte_and_element_limits_stop_retention_without_panicking() {
        let document = Document::parse("<script>12345</script><script>6</script>");
        let discovery = discover_scripts(
            &document,
            &base_url(),
            ScriptDiscoveryLimits {
                max_script_elements: 1,
                max_inline_script_bytes: 4,
                ..ScriptDiscoveryLimits::default()
            },
        );
        let codes = discovery
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect::<Vec<_>>();

        assert!(discovery.scripts.is_empty());
        assert_eq!(
            codes,
            [
                ScriptDiagnosticCode::InlineBytesLimit,
                ScriptDiagnosticCode::ScriptElementLimit,
            ]
        );
    }
}
