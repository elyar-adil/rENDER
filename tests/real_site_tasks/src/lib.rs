//! Headless, site-neutral helpers for task-level web compatibility checks.
//!
//! The runtime remains standards-driven: site names occur only in this test
//! harness. A `Pass` means the narrow capability named by the record was
//! observed. It never implies that an entire real site works.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use render_core::dom::{Dom, ElementData, NodeId, NodeKind};
use render_net::Url;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResultStatus {
    Pass,
    Unsupported,
    Fail,
}

impl ResultStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Unsupported => "unsupported",
            Self::Fail => "fail",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityResult {
    pub layer: &'static str,
    pub scenario: &'static str,
    pub capability: &'static str,
    pub status: ResultStatus,
    pub detail: String,
}

impl CapabilityResult {
    #[must_use]
    pub fn to_json_line(&self) -> String {
        format!(
            "{{\"type\":\"capability\",\"layer\":\"{}\",\"scenario\":\"{}\",\"capability\":\"{}\",\"status\":\"{}\",\"detail\":\"{}\"}}",
            escape_json(self.layer),
            escape_json(self.scenario),
            escape_json(self.capability),
            self.status.as_str(),
            escape_json(&self.detail),
        )
    }
}

#[derive(Clone, Debug)]
pub struct CapabilityReport {
    layer: &'static str,
    scenario: &'static str,
    results: Vec<CapabilityResult>,
}

impl CapabilityReport {
    #[must_use]
    pub const fn new(layer: &'static str, scenario: &'static str) -> Self {
        Self {
            layer,
            scenario,
            results: Vec::new(),
        }
    }

    pub fn pass(&mut self, capability: &'static str, detail: impl Into<String>) {
        self.record(capability, ResultStatus::Pass, detail);
    }

    pub fn unsupported(&mut self, capability: &'static str, detail: impl Into<String>) {
        self.record(capability, ResultStatus::Unsupported, detail);
    }

    pub fn fail(&mut self, capability: &'static str, detail: impl Into<String>) {
        self.record(capability, ResultStatus::Fail, detail);
    }

    pub fn check(
        &mut self,
        condition: bool,
        capability: &'static str,
        pass_detail: impl Into<String>,
        fail_detail: impl Into<String>,
    ) {
        if condition {
            self.pass(capability, pass_detail);
        } else {
            self.fail(capability, fail_detail);
        }
    }

    #[must_use]
    pub fn results(&self) -> &[CapabilityResult] {
        &self.results
    }

    #[must_use]
    pub fn has_failures(&self) -> bool {
        self.results
            .iter()
            .any(|result| result.status == ResultStatus::Fail)
    }

    pub fn emit_json_lines(&self) {
        for result in &self.results {
            println!("{}", result.to_json_line());
        }
        let pass = self.count(ResultStatus::Pass);
        let unsupported = self.count(ResultStatus::Unsupported);
        let fail = self.count(ResultStatus::Fail);
        println!(
            "{{\"type\":\"summary\",\"layer\":\"{}\",\"scenario\":\"{}\",\"pass\":{pass},\"unsupported\":{unsupported},\"fail\":{fail}}}",
            escape_json(self.layer),
            escape_json(self.scenario),
        );
    }

    pub fn assert_no_failures(&self) {
        let failures = self
            .results
            .iter()
            .filter(|result| result.status == ResultStatus::Fail)
            .map(|result| format!("{}: {}", result.capability, result.detail))
            .collect::<Vec<_>>();
        assert!(
            failures.is_empty(),
            "{} fixture capability failures:\n{}",
            self.scenario,
            failures.join("\n")
        );
    }

    fn record(
        &mut self,
        capability: &'static str,
        status: ResultStatus,
        detail: impl Into<String>,
    ) {
        self.results.push(CapabilityResult {
            layer: self.layer,
            scenario: self.scenario,
            capability,
            status,
            detail: detail.into(),
        });
    }

    fn count(&self, status: ResultStatus) -> usize {
        self.results
            .iter()
            .filter(|result| result.status == status)
            .count()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum ResourceKind {
    StyleSheet,
    Image,
    Media,
    Script,
}

impl ResourceKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StyleSheet => "stylesheet",
            Self::Image => "image",
            Self::Media => "media",
            Self::Script => "script",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceReference {
    pub kind: ResourceKind,
    pub url: Url,
}

#[derive(Clone, Debug, Default)]
pub struct DocumentFacts {
    pub node_count: usize,
    pub element_count: usize,
    pub text_node_count: usize,
    pub tags: BTreeMap<String, usize>,
    pub resources: Vec<ResourceReference>,
    pub hyperlinks: Vec<(NodeId, String)>,
}

impl DocumentFacts {
    #[must_use]
    pub fn tag_count(&self, tag: &str) -> usize {
        self.tags.get(tag).copied().unwrap_or_default()
    }
}

#[must_use]
pub fn inspect_document(dom: &Dom, base_url: &Url) -> DocumentFacts {
    let mut facts = DocumentFacts::default();
    walk_dom(dom, dom.document(), &mut |node, kind| {
        facts.node_count += 1;
        match kind {
            NodeKind::Element(element) => {
                facts.element_count += 1;
                *facts.tags.entry(element.local_name.clone()).or_default() += 1;
                collect_element_facts(node, element, base_url, &mut facts);
            }
            NodeKind::Text(_) => facts.text_node_count += 1,
            _ => {}
        }
    });
    facts
}

#[must_use]
pub fn first_element_named(dom: &Dom, local_name: &str) -> Option<NodeId> {
    find_node(dom, |_, kind| {
        matches!(kind, NodeKind::Element(element) if element.local_name == local_name)
    })
}

#[must_use]
pub fn first_element_by_id(dom: &Dom, id: &str) -> Option<NodeId> {
    find_node(dom, |_, kind| {
        matches!(kind, NodeKind::Element(element) if attribute(element, "id") == Some(id))
    })
}

#[must_use]
pub fn first_text_descendant(dom: &Dom, root: NodeId) -> Option<NodeId> {
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        if matches!(dom.node(node).map(render_core::dom::Node::kind), Some(NodeKind::Text(text)) if !text.trim().is_empty())
        {
            return Some(node);
        }
        pending.extend(dom.children(node).unwrap_or_default().iter().rev());
    }
    None
}

fn collect_element_facts(
    node: NodeId,
    element: &ElementData,
    base_url: &Url,
    facts: &mut DocumentFacts,
) {
    if element.local_name == "a"
        && let Some(href) = attribute(element, "href")
    {
        facts.hyperlinks.push((node, href.to_owned()));
    }

    let reference = match element.local_name.as_str() {
        "link" if rel_contains(element, "stylesheet") => {
            attribute(element, "href").map(|href| (ResourceKind::StyleSheet, href))
        }
        "link" if attribute(element, "as").is_some_and(|value| {
            value.eq_ignore_ascii_case("image")
        }) => attribute(element, "href").map(|href| (ResourceKind::Image, href)),
        "link" if attribute(element, "as").is_some_and(|value| {
            value.eq_ignore_ascii_case("video") || value.eq_ignore_ascii_case("audio")
        }) => attribute(element, "href").map(|href| (ResourceKind::Media, href)),
        "img" => attribute(element, "src").map(|src| (ResourceKind::Image, src)),
        "video" => attribute(element, "poster").map(|src| (ResourceKind::Image, src)),
        "source" => attribute(element, "src").map(|src| (ResourceKind::Media, src)),
        "script" => attribute(element, "src").map(|src| (ResourceKind::Script, src)),
        _ => None,
    };
    if let Some((kind, reference)) = reference
        && let Ok(url) = base_url.join(reference)
        && !facts
            .resources
            .iter()
            .any(|resource| resource.kind == kind && resource.url == url)
    {
        facts.resources.push(ResourceReference { kind, url });
    }
}

fn walk_dom(dom: &Dom, root: NodeId, visitor: &mut impl FnMut(NodeId, &NodeKind)) {
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        let Some(current) = dom.node(node) else {
            continue;
        };
        visitor(node, current.kind());
        pending.extend(current.children().iter().rev());
    }
}

fn find_node(dom: &Dom, mut predicate: impl FnMut(NodeId, &NodeKind) -> bool) -> Option<NodeId> {
    let mut result = None;
    walk_dom(dom, dom.document(), &mut |node, kind| {
        if result.is_none() && predicate(node, kind) {
            result = Some(node);
        }
    });
    result
}

fn rel_contains(element: &ElementData, expected: &str) -> bool {
    attribute(element, "rel").is_some_and(|rel| {
        rel.split_ascii_whitespace()
            .any(|token| token.eq_ignore_ascii_case(expected))
    })
}

fn attribute<'a>(element: &'a ElementData, name: &str) -> Option<&'a str> {
    element
        .attributes
        .iter()
        .find(|attribute| attribute.namespace.is_none() && attribute.local_name == name)
        .map(|attribute| attribute.value.as_str())
}

fn escape_json(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character.is_control() => {
                write!(&mut escaped, "\\u{:04x}", u32::from(character))
                    .expect("writing to String cannot fail");
            }
            character => escaped.push(character),
        }
    }
    escaped
}

