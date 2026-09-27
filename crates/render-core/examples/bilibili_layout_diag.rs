//! Diagnose layout of a captured Bilibili page with its captured stylesheet.
//! Usage: `cargo run -p render-core --example bilibili_layout_diag -- PAGE.HTML STYLE.CSS`

use std::fs;

use render_core::document::{
    AuthorStyleSource, Document, DocumentLimits, DocumentRenderOptions, ExternalStyleSheetKey,
    ExternalStyleSheets,
};
use render_core::dom::{Dom, NodeId, NodeKind};
use url::Url;

fn describe(dom: &Dom, node: NodeId) -> String {
    let Some(node) = dom.node(node) else {
        return String::new();
    };
    match node.kind() {
        NodeKind::Element(element) => {
            let class = element
                .attributes
                .iter()
                .find(|a| a.local_name == "class")
                .map_or("", |a| a.value.as_str());
            format!("{} .{}", element.local_name, class)
        }
        kind => kind.name().to_owned(),
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "diagnostic dump reads as one listing"
)]
fn main() {
    let mut args = std::env::args().skip(1);
    let html = fs::read_to_string(args.next().expect("page path")).expect("read page");
    let css = fs::read_to_string(args.next().expect("style path")).expect("read style");
    let document = Document::parse(&html);
    let base = Url::parse("https://www.bilibili.com/").unwrap();
    let slots = document.discover_author_style_slots(&base, DocumentLimits::default());
    let mut external = ExternalStyleSheets::default();
    for slot in slots.slots {
        if let AuthorStyleSource::External {
            resolved_url: Some(url),
            ..
        } = slot.source
        {
            println!("stylesheet owner={:?} url={url}", slot.owner);
            external.insert_css(ExternalStyleSheetKey::new(slot.owner, url), &css);
        }
    }
    let mut options = DocumentRenderOptions::default();
    options.layout.viewport.width = 1770.0;
    options.layout.viewport.height = 1026.0;
    let output = document.render_reference_with_external_style_sheets(options, &base, &external);
    println!(
        "styles={} fragments={} height={}",
        output.styles.len(),
        output.layout.fragments.iter().count(),
        output.layout.fragments.scrollable_content_size.height
    );
    for i in [
        577, 578, 579, 580, 581, 604, 634, 771, 837, 903, 969, 1035, 1101,
    ] {
        let id = NodeId::from_u64(i);
        let desc = describe(document.dom(), id);
        let style = output.styles.get(&id);
        let get = |name| {
            style
                .and_then(|style| style.get(name))
                .map(render_core::css::computed::ComputedValue::css_text)
        };
        let rect = output
            .layout
            .fragments
            .iter()
            .find(|fragment| fragment.source == Some(id))
            .map(|fragment| fragment.rect);
        println!(
            "{i} {desc} display={:?} columns={:?} column={:?}/{:?} row={:?}/{:?} rect={rect:?}",
            get("display"),
            get("grid-template-columns"),
            get("grid-column-start"),
            get("grid-column-end"),
            get("grid-row-start"),
            get("grid-row-end")
        );
    }
    if std::env::var_os("RENDER_DIAG_CAROUSEL").is_some() {
        for i in 604..771 {
            let id = NodeId::from_u64(i);
            let desc = describe(document.dom(), id);
            if !desc.contains("carousel") && !desc.starts_with("img") && !desc.contains("v-img") {
                continue;
            }
            let rect = output
                .layout
                .fragments
                .iter()
                .find(|fragment| fragment.source == Some(id))
                .map(|fragment| fragment.rect);
            let style = output.styles.get(&id);
            let get = |name| {
                style
                    .and_then(|style| style.get(name))
                    .map(render_core::css::computed::ComputedValue::css_text)
            };
            println!(
                "subtree {i} {desc} rect={rect:?} position={:?} height={:?} object-fit={:?} background={:?}",
                get("position"),
                get("height"),
                get("object-fit"),
                get("background-color")
            );
        }
    }
    if std::env::var_os("RENDER_DIAG_CARD").is_some() {
        for i in 770..820 {
            let id = NodeId::from_u64(i);
            let desc = describe(document.dom(), id);
            let rect = output
                .layout
                .fragments
                .iter()
                .find(|fragment| fragment.source == Some(id))
                .map(|fragment| fragment.rect);
            let style = output.styles.get(&id);
            let get = |name| {
                style
                    .and_then(|style| style.get(name))
                    .map(render_core::css::computed::ComputedValue::css_text)
            };
            println!(
                "card {i} {desc} rect={rect:?} display={:?} position={:?} height={:?} padding-top={:?} top={:?} bottom={:?}",
                get("display"),
                get("position"),
                get("height"),
                get("padding-top"),
                get("top"),
                get("bottom")
            );
        }
    }
}
