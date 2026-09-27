//! Diagnostic: parse a page, run its classic scripts (network unavailable, so
//! only inline scripts execute), then serialize the DOM back to HTML.
//! Usage: `cargo run -p render-core --examples dom_dump -- PAGE.html OUT.html`
use std::fs;

use render_core::dom::NodeKind;

fn serialize(dom: &render_core::dom::Dom, node: render_core::dom::NodeId, out: &mut String) {
    let Some(node_ref) = dom.node(node) else {
        return;
    };
    match node_ref.kind() {
        NodeKind::Element(element) => {
            out.push('<');
            out.push_str(&element.local_name);
            for attribute in &element.attributes {
                out.push(' ');
                out.push_str(&attribute.local_name);
                out.push_str("=\"");
                for ch in attribute.value.chars() {
                    match ch {
                        '"' => out.push_str("&quot;"),
                        '&' => out.push_str("&amp;"),
                        _ => out.push(ch),
                    }
                }
                out.push('"');
            }
            out.push('>');
            for child in dom.children(node).unwrap_or_default() {
                serialize(dom, *child, out);
            }
            out.push_str("</");
            out.push_str(&element.local_name);
            out.push('>');
        }
        NodeKind::Text(text) => {
            for ch in text.chars() {
                match ch {
                    '<' => out.push_str("&lt;"),
                    '&' => out.push_str("&amp;"),
                    _ => out.push(ch),
                }
            }
        }
        NodeKind::Comment(_)
        | NodeKind::DocumentType(_)
        | NodeKind::ProcessingInstruction { .. } => {}
        NodeKind::Document | NodeKind::DocumentFragment => {
            for child in dom.children(node).unwrap_or_default() {
                serialize(dom, *child, out);
            }
        }
    }
    let _ = node_ref;
}

fn main() {
    let mut args = std::env::args().skip(1);
    let page = args.next().expect("page path");
    let out_path = args.next().expect("out path");
    let html = fs::read_to_string(&page).expect("read page");
    let url = url::Url::parse("https://www.jd.com/").expect("base URL");
    let mut page = render_core::page::Page::with_url(&html, &url);
    // Drain the page's parser-inserted inline scripts and their microtasks.
    for _ in 0..64 {
        match page.run_one_turn_without_render() {
            Ok(Some(_)) => {}
            _ => break,
        }
    }
    let dom = page.document().dom();
    let mut out = String::new();
    serialize(dom, dom.document(), &mut out);
    fs::write(&out_path, &out).expect("write out");
    eprintln!("wrote {} bytes", out.len());
}
