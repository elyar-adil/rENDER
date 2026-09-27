//! Minimal probe: does a rule from a real stylesheet apply? Usage:
//! `cargo run -p render-core --example css_probe -- CSS_PATH [CLASS]`
use std::fs;

use render_core::css::computed::ComputedValue;
use render_core::document::{
    Document, DocumentRenderOptions, ExternalStyleSheetKey, ExternalStyleSheets,
};
use url::Url;

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let css = fs::read_to_string(&args[0]).expect("read css");
    let class = args.get(1).map_or("probe", String::as_str);
    let html = format!(
        "<!doctype html><head>\
         <link rel=\"stylesheet\" href=\"https://probe.test/s.css\">\
         </head><body><div id=target class={class}></div>"
    );
    let document = Document::parse(&html);
    let base = Url::parse("https://probe.test/").unwrap();
    let options = DocumentRenderOptions::default();

    let mut external = ExternalStyleSheets::default();
    let slots = document
        .discover_author_style_slots(&base, render_core::document::DocumentLimits::default());
    for slot in slots.slots {
        if let render_core::document::AuthorStyleSource::External {
            resolved_url: Some(resolved),
            ..
        } = slot.source
        {
            external.insert_css(ExternalStyleSheetKey::new(slot.owner, resolved), &css);
        }
    }
    let output = document.render_reference_with_external_style_sheets(options, &base, &external);
    let target = output
        .styles
        .keys()
        .copied()
        .last()
        .unwrap_or(document.dom().document());
    let style = output.styles.get(&target);
    for property in [
        "display",
        "color",
        "background-color",
        "width",
        "height",
        "float",
        "margin-top",
        "flex-direction",
        "padding-left",
    ] {
        let value = style
            .and_then(|style| style.get(property))
            .map(ComputedValue::css_text);
        eprintln!("{property} = {value:?}");
    }
}
