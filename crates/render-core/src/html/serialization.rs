//! HTML fragment serialization (`Element.innerHTML` / `outerHTML`).

use crate::dom::{Dom, NodeId, NodeKind};

/// Void elements per the HTML standard: serialized without an end tag and
/// never descended into.
const VOID_ELEMENTS: [&str; 14] = [
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// Raw-text elements whose children serialize verbatim (no entity escaping).
const RAW_TEXT_ELEMENTS: [&str; 2] = ["script", "style"];

/// Serialize all children of `parent` as an HTML fragment string.
#[must_use]
pub fn serialize_html_fragment(dom: &Dom, parent: NodeId) -> String {
    let mut output = String::new();
    for child in dom.children(parent).unwrap_or_default() {
        serialize_node(dom, *child, &mut output);
    }
    output
}

/// Serialize one node including its own start/end tags.
#[must_use]
pub fn serialize_html_node(dom: &Dom, node: NodeId) -> String {
    let mut output = String::new();
    serialize_node(dom, node, &mut output);
    output
}

fn serialize_node(dom: &Dom, node: NodeId, output: &mut String) {
    let Some(node_ref) = dom.node(node) else {
        return;
    };
    match node_ref.kind() {
        NodeKind::Element(element) => {
            let local_name = element.local_name.as_str();
            output.push('<');
            output.push_str(local_name);
            for attribute in &element.attributes {
                if attribute.prefix.is_some() || attribute.namespace.is_some() {
                    // Foreign-object attributes are out of scope for this
                    // minimal serializer.
                    continue;
                }
                output.push(' ');
                output.push_str(&attribute.local_name);
                output.push_str("=\"");
                escape_attribute(&attribute.value, output);
                output.push('"');
            }
            output.push('>');
            if VOID_ELEMENTS.contains(&local_name) {
                return;
            }
            if RAW_TEXT_ELEMENTS.contains(&local_name) {
                if let Some(child) = node_ref.children().first() {
                    if let Some(NodeKind::Text(data)) = dom.node(*child).map(crate::dom::Node::kind)
                    {
                        output.push_str(data);
                    }
                }
            } else {
                for child in node_ref.children() {
                    serialize_node(dom, *child, output);
                }
            }
            output.push_str("</");
            output.push_str(local_name);
            output.push('>');
        }
        NodeKind::Text(data) => escape_text(data, output),
        NodeKind::Comment(data) => {
            output.push_str("<!--");
            output.push_str(data);
            output.push_str("-->");
        }
        NodeKind::DocumentType(data) => {
            output.push_str("<!DOCTYPE ");
            output.push_str(&data.name);
            output.push('>');
        }
        NodeKind::ProcessingInstruction { target, data } => {
            output.push_str("<?");
            output.push_str(target);
            output.push(' ');
            output.push_str(data);
            output.push_str("?>");
        }
        NodeKind::Document | NodeKind::DocumentFragment => {
            for child in node_ref.children() {
                serialize_node(dom, *child, output);
            }
        }
    }
}

fn escape_text(text: &str, output: &mut String) {
    for character in text.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            other => output.push(other),
        }
    }
}

fn escape_attribute(value: &str, output: &mut String) {
    for character in value.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '"' => output.push_str("&quot;"),
            other => output.push(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::html::parse_document;

    fn body_of(source: &str) -> (Dom, NodeId) {
        fn find_element_by_name(dom: &Dom, root: NodeId, local_name: &str) -> Option<NodeId> {
            for child in dom.children(root).unwrap_or_default() {
                if let Some(NodeKind::Element(element)) =
                    dom.node(*child).map(crate::dom::Node::kind)
                {
                    if element.local_name == local_name {
                        return Some(*child);
                    }
                }
                if let Some(found) = find_element_by_name(dom, *child, local_name) {
                    return Some(found);
                }
            }
            None
        }

        let parsed = parse_document(source);
        let body =
            find_element_by_name(&parsed.dom, parsed.dom.document(), "body").expect("body exists");
        (parsed.dom, body)
    }

    #[test]
    fn round_trips_elements_and_text() {
        let (dom, body) = body_of("<!doctype html><p>Hello <b>world</b>!</p>");
        assert_eq!(
            serialize_html_fragment(&dom, body),
            "<p>Hello <b>world</b>!</p>"
        );
    }

    #[test]
    fn escapes_text_but_not_raw_text_children() {
        let (dom, body) =
            body_of("<!doctype html><p>a &amp; b</p><style>p > b { color: red }</style>");
        assert_eq!(
            serialize_html_fragment(&dom, body),
            "<p>a &amp; b</p><style>p > b { color: red }</style>"
        );
    }

    #[test]
    fn void_elements_have_no_end_tag() {
        let (dom, body) = body_of("<!doctype html><br><img src=\"x.png\" alt=\"a&quot;b\">");
        assert_eq!(
            serialize_html_fragment(&dom, body),
            "<br><img src=\"x.png\" alt=\"a&quot;b\">"
        );
    }
}
