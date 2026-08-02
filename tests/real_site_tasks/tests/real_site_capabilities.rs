use std::collections::BTreeSet;

use render_browser::images::{ImageDiagnosticSeverity, ImageResourceDiagnosticCode, plan_images};
use render_core::document::Document;
use render_core::dom::{Dom, ElementData, NodeId, NodeKind};
use render_core::html::parse_document;
use render_core::image::{ImageDiscoveryDiagnosticCode, ImageLimits};
use render_net::Url;
use render_real_site_tasks::{
    CapabilityReport, ResourceKind, first_element_by_id, first_element_named, inspect_document,
};

struct Fixture {
    name: &'static str,
    source: &'static str,
    base_url: &'static str,
    required_tags: &'static [&'static str],
    minimum_links: usize,
    minimum_scroll_text_chars: usize,
}

const FIXTURES: &[Fixture] = &[
    Fixture {
        name: "baidu_home",
        source: include_str!("../../fixtures/real_sites/baidu_home.html"),
        base_url: "https://www.baidu.com/",
        required_tags: &["header", "nav", "main", "section", "article"],
        minimum_links: 8,
        minimum_scroll_text_chars: 240,
    },
    Fixture {
        name: "baidu_search",
        source: include_str!("../../fixtures/real_sites/baidu_search.html"),
        base_url: "https://www.baidu.com/s?wd=browser",
        required_tags: &["header", "nav", "main", "section", "article"],
        minimum_links: 8,
        minimum_scroll_text_chars: 240,
    },
    Fixture {
        name: "zhihu_home",
        source: include_str!("../../fixtures/real_sites/zhihu_home.html"),
        base_url: "https://www.zhihu.com/",
        required_tags: &["header", "nav", "main", "section", "article"],
        minimum_links: 8,
        minimum_scroll_text_chars: 240,
    },
    Fixture {
        name: "zhihu_article",
        source: include_str!("../../fixtures/real_sites/zhihu_article.html"),
        base_url: "https://zhuanlan.zhihu.com/p/123456789",
        required_tags: &["header", "nav", "main", "article", "aside"],
        minimum_links: 6,
        minimum_scroll_text_chars: 300,
    },
    Fixture {
        name: "163_home",
        source: include_str!("../../fixtures/real_sites/163_home.html"),
        base_url: "https://www.163.com/",
        required_tags: &["header", "nav", "main", "section", "article", "aside"],
        minimum_links: 12,
        minimum_scroll_text_chars: 900,
    },
];

#[test]
fn baidu_home_covers_common_document_capabilities() {
    check_fixture(&FIXTURES[0]);
}

#[test]
fn baidu_search_covers_common_document_capabilities() {
    check_fixture(&FIXTURES[1]);
}

#[test]
fn zhihu_home_covers_common_document_capabilities() {
    check_fixture(&FIXTURES[2]);
}

#[test]
fn zhihu_article_covers_common_document_capabilities() {
    check_fixture(&FIXTURES[3]);
}

#[test]
fn netease_home_lazy_images_have_no_severe_discovery_diagnostics() {
    let fixture = &FIXTURES[4];
    check_fixture(fixture);

    let document = Document::parse(fixture.source);
    let base_url = Url::parse(fixture.base_url).expect("fixture base URL");
    let plan = plan_images(&document, &base_url, ImageLimits::default());
    let severe = plan
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == ImageDiagnosticSeverity::Error)
        .collect::<Vec<_>>();
    assert!(
        severe.is_empty(),
        "163 lazy image discovery produced severe diagnostics: {severe:?}"
    );

    let missing_source_warnings = plan
        .diagnostics
        .iter()
        .filter(|diagnostic| {
            diagnostic.code
                == ImageResourceDiagnosticCode::Discovery(
                    ImageDiscoveryDiagnosticCode::MissingSource,
                )
        })
        .count();
    assert!(
        missing_source_warnings >= 4,
        "expected deferred 163 images to be warning-only MissingSource diagnostics, got {missing_source_warnings}: {:?}",
        plan.diagnostics
    );
    assert!(plan.diagnostics.iter().any(|diagnostic| {
        diagnostic.code
            == ImageResourceDiagnosticCode::Discovery(
                ImageDiscoveryDiagnosticCode::SrcsetUnsupported,
            )
    }));
    assert!(
        plan.resources.len() >= 2,
        "expected immediate 163 image sources to remain fetchable"
    );
}

fn check_fixture(fixture: &Fixture) {
    let parsed = parse_document(fixture.source);
    let base_url = Url::parse(fixture.base_url).expect("fixture base URL");
    let facts = inspect_document(&parsed.dom, &base_url);
    let mut report = CapabilityReport::new("offline-real-site", fixture.name);

    report.check(
        parsed.errors.is_empty(),
        "html-parse",
        "fixture parsed without HTML diagnostics",
        format!(
            "parser returned {} diagnostics: {:?}",
            parsed.errors.len(),
            parsed.errors
        ),
    );

    let title = first_element_named(&parsed.dom, "title")
        .map(|node| text_content(&parsed.dom, node))
        .unwrap_or_default();
    report.check(
        !title.trim().is_empty(),
        "document-title",
        title.trim().to_owned(),
        "title element has no text",
    );

    for tag in fixture.required_tags {
        report.check(
            facts.tag_count(tag) > 0,
            "semantic-section",
            format!("{tag} present"),
            format!("missing required semantic element <{tag}>"),
        );
    }

    report.check(
        facts.hyperlinks.len() >= fixture.minimum_links,
        "navigation-links",
        format!("{} links discovered", facts.hyperlinks.len()),
        format!(
            "expected at least {} links, found {}",
            fixture.minimum_links,
            facts.hyperlinks.len()
        ),
    );

    report.check(
        has_search_form(&parsed.dom),
        "search-form",
        "role=search form has an input and submit control",
        "missing a submittable role=search form",
    );

    let resource_kinds = facts
        .resources
        .iter()
        .map(|resource| resource.kind)
        .collect::<BTreeSet<_>>();
    for kind in [
        ResourceKind::StyleSheet,
        ResourceKind::Image,
        ResourceKind::Script,
    ] {
        report.check(
            resource_kinds.contains(&kind),
            "external-resource-classification",
            format!("{} resource discovered", kind.as_str()),
            format!("no {} resource discovered", kind.as_str()),
        );
    }

    let scroll_text = first_element_by_id(&parsed.dom, "scroll-content")
        .map(|node| text_content(&parsed.dom, node))
        .unwrap_or_default();
    report.check(
        scroll_text.chars().count() >= fixture.minimum_scroll_text_chars,
        "scroll-content",
        format!(
            "{} text characters in scroll region",
            scroll_text.chars().count()
        ),
        format!(
            "expected at least {} text characters in scroll region, found {}",
            fixture.minimum_scroll_text_chars,
            scroll_text.chars().count()
        ),
    );

    report.assert_no_failures();
}

fn text_content(dom: &Dom, root: NodeId) -> String {
    let mut output = String::new();
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        let Some(current) = dom.node(node) else {
            continue;
        };
        if let NodeKind::Text(text) = current.kind() {
            output.push_str(text);
        }
        pending.extend(current.children().iter().rev());
    }
    output
}

fn has_search_form(dom: &Dom) -> bool {
    let mut pending = vec![dom.document()];
    while let Some(node) = pending.pop() {
        let Some(current) = dom.node(node) else {
            continue;
        };
        if let NodeKind::Element(element) = current.kind()
            && element.local_name == "form"
            && attribute(element, "role") == Some("search")
            && has_search_controls(dom, node)
        {
            return true;
        }
        pending.extend(current.children().iter().rev());
    }
    false
}

fn has_search_controls(dom: &Dom, form: NodeId) -> bool {
    let mut has_input = false;
    let mut has_submit = false;
    let mut pending = vec![form];
    while let Some(node) = pending.pop() {
        let Some(current) = dom.node(node) else {
            continue;
        };
        if let NodeKind::Element(element) = current.kind() {
            match element.local_name.as_str() {
                "input"
                    if matches!(
                        attribute(element, "type"),
                        None | Some("text") | Some("search")
                    ) && attribute(element, "name").is_some() =>
                {
                    has_input = true;
                }
                "button" if matches!(attribute(element, "type"), None | Some("submit")) => {
                    has_submit = true;
                }
                "input" if attribute(element, "type") == Some("submit") => {
                    has_submit = true;
                }
                _ => {}
            }
        }
        pending.extend(current.children().iter().rev());
    }
    has_input && has_submit
}

fn attribute<'a>(element: &'a ElementData, name: &str) -> Option<&'a str> {
    element
        .attributes
        .iter()
        .find(|attribute| attribute.namespace.is_none() && attribute.local_name == name)
        .map(|attribute| attribute.value.as_str())
}
