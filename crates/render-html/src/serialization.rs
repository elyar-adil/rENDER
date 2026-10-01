//! HTML fragment serialization (`Element.innerHTML` / `outerHTML`).

use render_dom::{Dom, NodeId, NodeKind};

/// Void elements per the HTML standard: serialized without an end tag and
/// never descended into.
const VOID_ELEMENTS: [&str; 14] = [
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// Elements whose text children serialize verbatim, with no entity escaping.
///
/// The fragment serialisation algorithm decides this per **text node**, by
/// looking at the node's parent: "If the parent of current node is a style,
/// script, xmp, iframe, noembed, noframes, or plaintext element, or if the
/// parent of current node is a noscript element and scripting is enabled for
/// the node, then append the value of current node's data literally"
/// (13.3.2). Escaping the data of any of these produces markup that does not
/// parse back to the same text, because the tokenizer does not resolve
/// character references inside a raw text element: `&lt;` written literally
/// stays the five characters `&lt;`.
///
/// `title` and `textarea` are deliberately absent. They are RCDATA, not raw
/// text, so their contents *are* character references and must be escaped.
const RAW_TEXT_ELEMENTS: [&str; 7] = [
    "iframe",
    "noembed",
    "noframes",
    "plaintext",
    "script",
    "style",
    "xmp",
];

/// Whether a `noscript` element's text children serialize verbatim.
///
/// A `noscript` element's contents are raw text only when scripting is enabled
/// (13.2.4.5), so the serialiser needs the same half of the scripting mode the
/// tree builder did to decide. With scripting disabled the contents are
/// markup, and its text nodes are ordinary text that must be escaped.
fn is_raw_text_element(local_name: &str, scripting_enabled: bool) -> bool {
    RAW_TEXT_ELEMENTS.contains(&local_name) || (scripting_enabled && local_name == "noscript")
}

/// The XLink namespace, whose prefix is always `xlink`.
const XLINK_NAMESPACE: &str = "http://www.w3.org/1999/xlink";
/// The XML namespace, whose prefix is always `xml`.
const XML_NAMESPACE: &str = "http://www.w3.org/XML/1998/namespace";
/// The XMLNS namespace, which carries `xmlns` and `xmlns:*`.
const XMLNS_NAMESPACE: &str = "http://www.w3.org/2000/xmlns/";

/// The qualified name to serialize an attribute under.
///
/// Foreign content carries namespaced attributes (`xlink:href` in particular),
/// and the fragment serialisation algorithm writes them with their prefix
/// restored rather than dropping them.
fn attribute_name(attribute: &render_dom::Attribute) -> String {
    match attribute.namespace.as_deref() {
        Some(XLINK_NAMESPACE) => format!("xlink:{}", attribute.local_name),
        Some(XML_NAMESPACE) => format!("xml:{}", attribute.local_name),
        Some(XMLNS_NAMESPACE) => match attribute.prefix.as_deref() {
            // The default namespace declaration has no prefix.
            None => attribute.local_name.clone(),
            Some(prefix) => format!("{prefix}:{}", attribute.local_name),
        },
        _ => match attribute.prefix.as_deref() {
            Some(prefix) => format!("{prefix}:{}", attribute.local_name),
            None => attribute.local_name.clone(),
        },
    }
}

/// Serialize all children of `parent` as an HTML fragment string.
///
/// "If current node is a template element, then let current node instead be the
/// template element's template contents" (13.3.2), so this is what
/// `template.innerHTML` returns.
///
/// This is [`serialize_html_fragment_with_scripting`] with scripting enabled,
/// which is the scripting mode rENDER parses in: it has a script execution
/// engine, so "scripting is enabled for the node" (13.3.2) holds for every node
/// in a document it parsed itself.
#[must_use]
pub fn serialize_html_fragment(dom: &Dom, parent: NodeId) -> String {
    serialize_html_fragment_with_scripting(dom, parent, true)
}

/// Serialize all children of `parent` as an HTML fragment string, with the
/// scripting flag chosen by the caller.
///
/// The flag is the tree builder's half of the scripting mode (13.2.4.5), and the
/// serialiser needs it for exactly one decision: whether the text inside a
/// `noscript` element is raw text or markup. It must be the same value the
/// document was parsed with, because the two are only distinguishable by the
/// mode they were built in.
#[must_use]
pub fn serialize_html_fragment_with_scripting(
    dom: &Dom,
    parent: NodeId,
    scripting_enabled: bool,
) -> String {
    let mut output = String::new();
    let source = dom.template_contents(parent).unwrap_or(parent);
    for child in dom.children(source).unwrap_or_default() {
        serialize_node(dom, *child, &mut output, scripting_enabled);
    }
    output
}

/// Serialize one node including its own start/end tags.
///
/// This is [`serialize_html_node_with_scripting`] with scripting enabled; see
/// [`serialize_html_fragment`] for why that is the default.
#[must_use]
pub fn serialize_html_node(dom: &Dom, node: NodeId) -> String {
    serialize_html_node_with_scripting(dom, node, true)
}

/// Serialize one node including its own start/end tags, with the scripting flag
/// chosen by the caller.
///
/// See [`serialize_html_fragment_with_scripting`] for what the flag decides.
#[must_use]
pub fn serialize_html_node_with_scripting(
    dom: &Dom,
    node: NodeId,
    scripting_enabled: bool,
) -> String {
    let mut output = String::new();
    serialize_node(dom, node, &mut output, scripting_enabled);
    output
}

fn serialize_node(dom: &Dom, node: NodeId, output: &mut String, scripting_enabled: bool) {
    let Some(node_ref) = dom.node(node) else {
        return;
    };
    match node_ref.kind() {
        NodeKind::Element(element) => {
            let local_name = element.local_name.as_str();
            output.push('<');
            output.push_str(local_name);
            for attribute in &element.attributes {
                output.push(' ');
                output.push_str(&attribute_name(attribute));
                output.push_str("=\"");
                escape_attribute(&attribute.value, output);
                output.push('"');
            }
            output.push('>');
            if VOID_ELEMENTS.contains(&local_name) {
                return;
            }
            if let Some(contents) = dom.template_contents(node) {
                // A template element has no children of its own: its markup is
                // in the template contents, and serializing the element means
                // serializing those.
                for child in dom.children(contents).unwrap_or_default() {
                    serialize_node(dom, *child, output, scripting_enabled);
                }
            } else {
                // The raw text decision belongs to each text node's parent, so
                // it is made here rather than once for the element: a script can
                // append a second text node to a script element, and the spec
                // writes both of them literally.
                let raw_text = is_raw_text_element(local_name, scripting_enabled);
                for child in node_ref.children() {
                    if raw_text
                        && matches!(
                            dom.node(*child).map(render_dom::Node::kind),
                            Some(NodeKind::Text(_))
                        )
                    {
                        if let Some(NodeKind::Text(data)) =
                            dom.node(*child).map(render_dom::Node::kind)
                        {
                            output.push_str(data);
                        }
                        continue;
                    }
                    serialize_node(dom, *child, output, scripting_enabled);
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
                serialize_node(dom, *child, output, scripting_enabled);
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
    use crate::parse_document;

    fn body_of(source: &str) -> (Dom, NodeId) {
        fn find_element_by_name(dom: &Dom, root: NodeId, local_name: &str) -> Option<NodeId> {
            for child in dom.children(root).unwrap_or_default() {
                if let Some(NodeKind::Element(element)) =
                    dom.node(*child).map(render_dom::Node::kind)
                    && element.local_name == local_name
                {
                    return Some(*child);
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

    #[test]
    fn namespaced_and_case_adjusted_foreign_attributes_round_trip() {
        let (dom, body) = body_of(
            "<!doctype html><svg viewbox='0 0 1 1'><use xlink:href='#i' xml:lang='en'/></svg>",
        );
        assert_eq!(
            serialize_html_fragment(&dom, body),
            "<svg viewBox=\"0 0 1 1\"><use xlink:href=\"#i\" xml:lang=\"en\"></use></svg>"
        );
    }

    /// Real pages write markup-looking text into every raw text element, and the
    /// serialiser has to write it back the way it found it. Escaping any of
    /// these produces markup that does not parse to the same text, because a
    /// raw text element's contents are never character references: the
    /// `&lt;` comes back as the five characters `&lt;`, and every round trip
    /// after that grows the string.
    #[test]
    fn every_raw_text_element_serializes_its_contents_literally() {
        for (element, source) in [
            ("style", "p > b { content: \"<\" }"),
            ("script", "if (a < b && c > d) { s = \"&amp;\" }"),
            ("xmp", "a < b & c"),
            ("iframe", "<p>no frames</p>"),
            ("noembed", "<p>no embed</p>"),
            ("noframes", "<p>no frames</p>"),
            (
                "noscript",
                "<meta http-equiv=\"refresh\" content=\"0; url=/x\" />",
            ),
        ] {
            // An explicit `body`, because `style`, `title` and `textarea` are
            // head elements when a document starts with them and body elements
            // when it does not, and this is about the raw text rule rather than
            // about where the element lands.
            let fragment = format!("<{element}>{source}</{element}>");
            let (dom, body) = body_of(&format!("<!doctype html><body>{fragment}"));
            let serialized = serialize_html_fragment(&dom, body);
            assert_eq!(serialized, fragment, "{element}");
            // And the round trip is a fixed point, which is the property that
            // escaping breaks: the second pass has to reproduce the first. The
            // reparse is wrapped in the same `body` so that an element which
            // would otherwise start the document is in the same place both
            // times.
            let reparsed = parse_document(&format!("<!doctype html><body>{serialized}</body>"));
            let again = serialize_html_fragment(&reparsed.dom, find_body(&reparsed.dom));
            assert_eq!(again, fragment, "{element}");
        }
    }

    /// `plaintext` is in the raw text set and is the one member of it that no
    /// end tag closes, so it is also the one that cannot be a serialisation
    /// fixed point: the closing tag the serialiser writes becomes text when the
    /// output is read back. That is inherent rather than a defect — the
    /// standard's serialisation algorithm writes `</plaintext>` and the
    /// plaintext state consumes it — so what is asserted here is the property
    /// that does hold, that the text is written literally.
    #[test]
    fn a_plaintext_element_is_raw_text_that_no_end_tag_closes() {
        let (dom, body) = body_of("<!doctype html><body><plaintext>a < b & c</plaintext>");
        let once = serialize_html_fragment(&dom, body);
        assert_eq!(once, "<plaintext>a < b & c</plaintext></plaintext>");

        let reparsed = parse_document(&format!("<!doctype html><body>{once}"));
        let twice = serialize_html_fragment(&reparsed.dom, find_body(&reparsed.dom));
        // The first `</plaintext>` is text now, and it is written literally
        // rather than escaped, which is the property under test.
        assert_eq!(
            twice,
            "<plaintext>a < b & c</plaintext></plaintext></plaintext>"
        );
    }

    /// RCDATA is the other half of the rule: `title` and `textarea` *do* resolve
    /// character references, so their contents must be escaped, or the `&` that
    /// `&amp;` stands for would be written back bare and read as the start of
    /// another reference.
    #[test]
    fn rcdata_elements_still_escape_their_contents() {
        let (dom, body) =
            body_of("<!doctype html><body><title>a &amp; b</title><textarea>&lt;x&gt;</textarea>");
        assert_eq!(
            serialize_html_fragment(&dom, body),
            "<title>a &amp; b</title><textarea>&lt;x&gt;</textarea>"
        );
    }

    /// The one raw text element whose serialisation depends on the scripting
    /// flag, and the reason the flag is a parameter rather than something the
    /// serialiser can work out for itself. The *same markup* holds different
    /// text in the two modes, and each has to be written back the way it was
    /// read: as raw text `a &amp; b` is seven literal characters, while as
    /// resolved text it is `a & b`, whose ampersand has to be escaped again.
    #[test]
    fn a_noscript_element_is_raw_text_only_while_scripting_is_enabled() {
        let markup = "<!doctype html><body><noscript>a &amp; b</noscript>";
        let fragment = "<noscript>a &amp; b</noscript>";

        // Read as raw text, so the `&amp;` was never a character reference and
        // both the text and its serialisation are the seven literal characters.
        let enabled = parse_document(markup);
        let body = find_body(&enabled.dom);
        let noscript = find_noscript(&enabled.dom, body);
        assert_eq!(text_of(&enabled.dom, noscript), "a &amp; b");
        assert_eq!(
            serialize_html_fragment_with_scripting(&enabled.dom, body, true),
            fragment
        );

        // Read as markup, so the text is the three characters `a & b` and the
        // serialisation has to escape the ampersand to say the same thing.
        let disabled = crate::parse_document_with_scripting(markup, false);
        let body = find_body(&disabled.dom);
        let noscript = find_noscript(&disabled.dom, body);
        assert_eq!(text_of(&disabled.dom, noscript), "a & b");
        assert_eq!(
            serialize_html_fragment_with_scripting(&disabled.dom, body, false),
            fragment
        );
    }

    fn find_body(dom: &Dom) -> NodeId {
        for child in dom.children(dom.document()).unwrap_or_default() {
            for inner in dom.children(*child).unwrap_or_default() {
                if let Some(NodeKind::Element(element)) =
                    dom.node(*inner).map(render_dom::Node::kind)
                    && element.local_name == "body"
                {
                    return *inner;
                }
            }
        }
        panic!("a body");
    }

    fn find_noscript(dom: &Dom, body: NodeId) -> NodeId {
        dom.children(body)
            .unwrap_or_default()
            .iter()
            .copied()
            .find(|child| {
                matches!(dom.node(*child).map(render_dom::Node::kind),
                    Some(NodeKind::Element(e)) if e.local_name == "noscript")
            })
            .expect("a noscript")
    }

    fn text_of(dom: &Dom, node: NodeId) -> String {
        dom.children(node)
            .unwrap_or_default()
            .iter()
            .filter_map(|child| match dom.node(*child).map(render_dom::Node::kind) {
                Some(NodeKind::Text(data)) => Some(data.as_str()),
                _ => None,
            })
            .collect()
    }
}
