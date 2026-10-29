//! Host-side derivation of dynamic pseudo-class state from pointer, focus and
//! URL inputs.

use render_core::css::selector::InteractionState;
use render_core::dom::Dom;
use render_core::dom::Namespace;
use render_core::dom::Node;
use render_core::dom::NodeId;
use render_core::dom::NodeKind;
use render_net::Url;
use std::collections::HashSet;

/// The element under the pointer and every ancestor up to the document.
///
/// Selectors 4 §6.6.1: `:hover` matches an element while the pointer is over
/// it *or one of its descendants*, so the set is a chain, not one node.
pub(super) fn ancestor_set(dom: &Dom, node: Option<NodeId>) -> HashSet<NodeId> {
    let mut chain = HashSet::new();
    let mut current = node;
    while let Some(candidate) = current {
        if !chain.insert(candidate) {
            break;
        }
        current = dom.parent(candidate);
    }
    chain
}

/// What moved focus, which decides whether `:focus-visible` shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FocusSource {
    /// A pointer press: the ring stays off except on text entry.
    Pointer,
    /// Sequential keyboard navigation: the ring always shows.
    Keyboard,
    /// `focus()`/`blur()` from script: the ring follows the last real input.
    Script,
}

/// Whether `node` should match `:focus-visible` now that it holds focus.
///
/// Selectors 4 §6.4.3 leaves the answer to the user agent. This follows the
/// browsers' rule: sequential keyboard navigation always shows the ring, while
/// a pointer press shows it only for text entry, where it doubles as the caret
/// cue. Script focus inherits the modality of the last user input.
pub(super) fn focus_visible(
    dom: &Dom,
    node: NodeId,
    source: FocusSource,
    keyboard_last_input: bool,
) -> bool {
    let keyboard = match source {
        FocusSource::Keyboard => true,
        FocusSource::Pointer => false,
        FocusSource::Script => keyboard_last_input,
    };
    keyboard || is_text_entry(dom, node)
}

/// The text controls a browser keeps the focus ring on when clicked.
fn is_text_entry(dom: &Dom, node: NodeId) -> bool {
    if crate::content_interaction::is_content_editable(dom, node) {
        return true;
    }
    let Some(NodeKind::Element(data)) = dom.node(node).map(Node::kind) else {
        return false;
    };
    if data.namespace != Namespace::Html {
        return false;
    }
    match data.local_name.as_str() {
        "textarea" => true,
        "input" => {
            let input_type = dom
                .attribute(node, "type")
                .ok()
                .flatten()
                .unwrap_or("text")
                .to_ascii_lowercase();
            render_core::interaction::is_text_entry_input_type(&input_type)
        }
        _ => false,
    }
}

/// The element a URL fragment names, if any.
///
/// HTML's target element is the first element in tree order with a matching
/// `id`; an `<a name>` is the legacy fallback. Fragments arrive percent-encoded
/// from the URL parser, so `#a%20b` must find `id="a b"`.
pub(super) fn target_node(dom: &Dom, url: &Url) -> Option<NodeId> {
    let fragment = url.fragment()?;
    let fragment = percent_decode(fragment)?;
    if fragment.is_empty() {
        return None;
    }
    let mut pending = vec![dom.document()];
    let mut named_anchor = None;
    while let Some(node) = pending.pop() {
        if let Some(NodeKind::Element(data)) = dom.node(node).map(Node::kind) {
            if dom.attribute(node, "id").ok().flatten() == Some(fragment.as_str()) {
                return Some(node);
            }
            if named_anchor.is_none()
                && data.namespace == Namespace::Html
                && data.local_name == "a"
                && dom.attribute(node, "name").ok().flatten() == Some(fragment.as_str())
            {
                named_anchor = Some(node);
            }
        }
        // Reversed, so the children come off the stack in tree order.
        for child in dom.children(node).unwrap_or_default().iter().rev() {
            pending.push(*child);
        }
    }
    named_anchor
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) =
                (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
        {
            decoded.push((high << 4) | low);
            index += 3;
            continue;
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8(decoded).ok()
}

const fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// The interaction state a freshly loaded document starts with.
pub(super) fn initial_state(dom: &Dom, url: &Url) -> InteractionState {
    InteractionState {
        target: target_node(dom, url),
        ..InteractionState::default()
    }
}

#[cfg(test)]
mod tests {
    use super::{ancestor_set, initial_state, target_node};
    use render_core::html::parse_document;
    use render_net::Url;

    fn url(fragment: &str) -> Url {
        Url::parse(&format!("https://example.test/page{fragment}")).expect("test URL")
    }

    #[test]
    fn hover_chain_contains_the_element_and_every_ancestor() {
        let document = parse_document(
            "<!doctype html><div id=outer><p id=middle><span id=inner></span></p></div>",
        );
        let dom = &document.dom;
        let inner = target_node(dom, &url("#inner")).expect("inner target");
        let outer = target_node(dom, &url("#outer")).expect("outer target");
        let chain = ancestor_set(dom, Some(inner));

        assert!(chain.contains(&inner));
        assert!(chain.contains(&outer));
        assert!(chain.contains(&dom.document()));
        assert!(
            chain.len() >= 5,
            "html, body and the intermediate elements are all hovered"
        );
        assert!(ancestor_set(dom, None).is_empty());
    }

    #[test]
    fn fragment_target_finds_an_id_and_decodes_percent_escapes() {
        let document = parse_document("<!doctype html><div id='a b'></div>");
        assert_eq!(
            target_node(&document.dom, &url("#a%20b")),
            target_node(&document.dom, &url("#a b"))
        );
        assert!(target_node(&document.dom, &url("#missing")).is_none());
        assert!(target_node(&document.dom, &url("#")).is_none());
        assert!(target_node(&document.dom, &url("")).is_none());
    }

    #[test]
    fn fragment_target_falls_back_to_a_named_anchor() {
        let document = parse_document("<!doctype html><a name=legacy></a><div id=current></div>");
        let dom = &document.dom;
        let named = target_node(dom, &url("#legacy")).expect("named anchor");
        let current = target_node(dom, &url("#current")).expect("id target");

        assert!(matches!(
            dom.node(named).map(render_core::dom::Node::kind),
            Some(render_core::dom::NodeKind::Element(element)) if element.local_name == "a"
        ));
        assert_ne!(named, current);
    }

    #[test]
    fn initial_state_carries_the_target_from_the_committed_url() {
        let document = parse_document("<!doctype html><div id=part></div>");
        let state = initial_state(&document.dom, &url("#part"));
        assert_eq!(state.target, target_node(&document.dom, &url("#part")));
        assert!(state.hovered.is_empty());
        assert!(state.active.is_empty());
        assert!(state.focused.is_none());
    }
}
