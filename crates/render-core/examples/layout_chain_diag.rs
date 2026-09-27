//! Diagnostic: dump the ancestor chain, computed styles, and fragment rects
//! for the first element matching a class. Usage:
//! `cargo run -p render-core --example layout_chain_diag -- PAGE.HTML URL_SUBSTR=CSS_FILE... -- CLASS`
use std::fs;

use render_core::css::computed::ComputedValue;
use render_core::document::{
    AuthorStyleSource, Document, DocumentLimits, DocumentRenderOptions, ExternalStyleSheetKey,
    ExternalStyleSheets,
};
use render_core::dom::{NodeId, NodeKind};
use render_layout::FragmentKind;
use url::Url;

#[allow(
    clippy::too_many_lines,
    reason = "diagnostic chain dump reads as one listing"
)]
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let Some(class_marker) = args.iter().position(|arg| arg == "--") else {
        panic!("usage: layout_chain_diag PAGE.HTML URL_SUBSTR=CSS... -- CLASS");
    };
    let class = args[class_marker + 1].clone();
    let page_path = args[0].clone();
    let css_map = args[1..class_marker]
        .iter()
        .map(|pair| {
            let (url_part, path) = pair.split_once('=').expect("URL_SUBSTR=CSS_FILE pair");
            (url_part.to_owned(), path.to_owned())
        })
        .collect::<Vec<_>>();

    let html = fs::read_to_string(&page_path).expect("read page");
    let document = Document::parse(&html);
    let base = guess_base_url(&page_path);
    let slots = document.discover_author_style_slots(&base, DocumentLimits::default());
    eprintln!("discovered {} style slots", slots.slots.len());
    let mut external = ExternalStyleSheets::default();
    for slot in slots.slots {
        if let AuthorStyleSource::External {
            resolved_url: Some(url),
            ..
        } = slot.source
        {
            for (url_part, path) in &css_map {
                if !url.as_str().contains(url_part.as_str()) {
                    continue;
                }
                let css = fs::read_to_string(path).expect("read css");
                external.insert_css(ExternalStyleSheetKey::new(slot.owner, url.clone()), &css);
                eprintln!("  {url} <- {path}");
            }
        }
    }
    let mut options = DocumentRenderOptions::default();
    options.layout.viewport.width = 1770.0;
    options.layout.viewport.height = 1026.0;
    let output = document.render_reference_with_external_style_sheets(options, &base, &external);

    let Some(target) = output.layout.fragments.iter().find_map(|fragment| {
        let source = fragment.source?;
        matches!(fragment.kind, FragmentKind::Box(_))
            .then_some(source)
            .filter(|&source| has_class(&document, source, &class))
    }) else {
        eprintln!("no box fragment matches class {class}");
        return;
    };
    // Dump the fragment subtree rooted at the target's parent.
    let parent = document.dom().parent(target);
    if let Some(parent) = parent {
        for fragment in output.layout.fragments.iter() {
            if fragment.source == Some(parent) {
                eprintln!("---- fragment subtree of parent ----");
                for child in &fragment.children {
                    walk(&output, &document, *child, 1);
                }
            }
        }
    }
    let mut node = Some(target);
    let mut depth = 0;
    while let Some(current) = node {
        let name = describe(&document, current);
        let style = output.styles.get(&current);
        let get = |name: &str| {
            style
                .and_then(|style| style.get(name))
                .map_or("-", ComputedValue::css_text)
        };
        let rects: Vec<_> = output
            .layout
            .fragments
            .iter()
            .filter(|fragment| fragment.source == Some(current))
            .map(|fragment| (fragment.rect, fragment.kind.clone()))
            .collect();
        eprintln!(
            "{depth:>2} {name} | display={} position={} width={} height={} min-height={} padding-top={} align-items={} justify-content={} flex-direction={} align-self={} flex-basis={} grow={} bg={} color={} disp2={} rect={rects:?}",
            get("display"),
            get("position"),
            get("width"),
            get("height"),
            get("min-height"),
            get("padding-top"),
            get("align-items"),
            get("justify-content"),
            get("flex-direction"),
            get("align-self"),
            get("flex-basis"),
            get("flex-grow"),
            get("background-color"),
            get("color"),
            get("display")
        );
        node = document.dom().parent(current);
        depth += 1;
        if depth > 10 {
            break;
        }
    }
}

fn walk(
    out: &render_core::document::DocumentRenderOutput,
    document: &Document,
    id: render_layout::FragmentId,
    depth: usize,
) {
    let Some(fragment) = out.layout.fragments.get(id) else {
        return;
    };
    let name = fragment.source.map_or_else(
        || "anonymous".to_owned(),
        |source| describe(document, source),
    );
    eprintln!(
        "{}{name} kind={:?} rect={:?}",
        "  ".repeat(depth),
        fragment.kind,
        fragment.rect
    );
    for child in &fragment.children {
        walk(out, document, *child, depth + 1);
    }
}

fn guess_base_url(_page_path: &str) -> Url {
    // The base URL is set per diagnosis by editing this constant; the saved
    // pages currently diagnosed are bilibili.com, zhihu.com, jd.com, and
    // news.ycombinator.com.
    Url::parse("https://www.jd.com/")
        .unwrap_or_else(|_| Url::parse("https://www.example.com/").expect("static base URL"))
}

fn describe(document: &Document, node: NodeId) -> String {
    match document.dom().node(node).map(|n| n.kind().clone()) {
        Some(NodeKind::Element(element)) => {
            let class = element
                .attributes
                .iter()
                .find(|a| a.local_name == "class")
                .map_or("", |a| a.value.as_str());
            format!("{}.{}", element.local_name, class)
        }
        Some(kind) => format!("{kind:?}"),
        None => "detached".to_owned(),
    }
}

fn has_class(document: &Document, node: NodeId, wanted: &str) -> bool {
    matches!(
        document.dom().node(node).map(|n| n.kind().clone()),
        Some(NodeKind::Element(element))
            if element.attributes.iter().any(|a| {
                a.local_name == "class"
                    && a.value.split_whitespace().any(|c| c == wanted)
            })
    )
}
