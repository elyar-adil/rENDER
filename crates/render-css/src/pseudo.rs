//! Generated content: `::before` and `::after`.
//!
//! A pseudo-element is not a DOM node, but its computed style has to live in
//! the same table as element styles because layout and paint look styles up by
//! `NodeId`. Generated styles therefore use synthetic ids built from the host's
//! id, with the top bit set so they can never collide with an arena index. The
//! formatting tree probes the two ids of every element and turns the ones with
//! a generating `content` into boxes.

use render_dom::NodeId;

use crate::computed::ComputedStyle;

/// The two generated-content pseudo-elements this engine creates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PseudoElement {
    Before,
    After,
}

impl PseudoElement {
    /// The Selectors spelling, which is also the value the cascade's
    /// [`MatchContext`](crate::selector::MatchContext) matches.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Before => "before",
            Self::After => "after",
        }
    }

    const fn bit(self) -> u64 {
        match self {
            Self::Before => 0,
            Self::After => 1,
        }
    }
}

/// The top bit marks a generated style. DOM arena ids are usize indices and
/// stay far below it.
const GENERATED: u64 = 1 << 63;

/// The synthetic `NodeId` that keys a pseudo-element's computed style.
#[must_use]
pub fn style_source(host: NodeId, pseudo: PseudoElement) -> NodeId {
    NodeId::from_u64(GENERATED | (host.as_u64() << 1) | pseudo.bit())
}

/// Whether `id` names generated content rather than a DOM node.
#[must_use]
pub const fn is_generated_style_source(id: NodeId) -> bool {
    id.as_u64() & GENERATED != 0
}

/// The text a generated box contains, or `None` when the pseudo-element
/// generates no box at all.
///
/// Only string content is implemented. `none` and `normal` (the initial value,
/// which computes to `none` on `::before`/`::after`) generate nothing, and
/// `counter()`, `attr()` and quotations are left for a later round rather than
/// guessed at.
#[must_use]
pub fn generated_content(style: &ComputedStyle) -> Option<String> {
    let text = style.get("content")?.css_text().trim();
    if text.is_empty() || text.eq_ignore_ascii_case("none") || text.eq_ignore_ascii_case("normal") {
        return None;
    }
    let quote = text.as_bytes().first().copied()?;
    if !matches!(quote, b'"' | b'\'') || text.as_bytes().last().copied() != Some(quote) {
        return None;
    }
    Some(unescape_css_string(&text[1..text.len() - 1]))
}

/// CSS Syntax 3 §4.3.5: a backslash escapes the next character, a hex escape
/// is one to six hex digits optionally followed by whitespace, and an escaped
/// newline is removed.
fn unescape_css_string(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if character != '\\' {
            output.push(character);
            continue;
        }
        let Some(next) = characters.next() else {
            break;
        };
        if next == '\n' || next == '\r' || next == '\u{c}' {
            // Escaped newline: no character.
            continue;
        }
        if next.is_ascii_hexdigit() {
            let mut digits = String::from(next);
            while digits.len() < 6 {
                let Some(candidate) = characters.peek().copied() else {
                    break;
                };
                if !candidate.is_ascii_hexdigit() {
                    break;
                }
                digits.push(candidate);
                characters.next();
            }
            if characters
                .peek()
                .is_some_and(|candidate| candidate.is_whitespace())
            {
                characters.next();
            }
            if let Ok(value) = u32::from_str_radix(&digits, 16)
                && let Some(value) = char::from_u32(value)
            {
                output.push(value);
            }
            continue;
        }
        output.push(next);
    }
    output
}

#[cfg(test)]
mod tests {
    use render_dom::NodeId;

    use super::{PseudoElement, generated_content, is_generated_style_source, style_source};
    use crate::cascade::{CascadeInput, CascadeOrigin};
    use crate::computed::PropertyRegistry;
    use crate::computed::compute_document_styles;
    use crate::selector::MatchContext;
    use crate::stylesheet::parse_stylesheet;
    use render_html::parse_document;

    #[test]
    fn generated_ids_are_disjoint_from_dom_ids() {
        let host = NodeId::from_u64(7);
        let before = style_source(host, PseudoElement::Before);
        let after = style_source(host, PseudoElement::After);
        assert!(is_generated_style_source(before));
        assert!(is_generated_style_source(after));
        assert_ne!(before, after);
        assert_eq!(style_source(host, PseudoElement::Before), before);
    }

    #[test]
    fn string_content_is_the_generated_text() {
        assert_eq!(
            generated_content(&style("", "content: 'a\\a b'")),
            Some("a\nb".to_owned())
        );
        assert_eq!(
            generated_content(&style("", "content: ''")),
            Some(String::new())
        );
        assert_eq!(generated_content(&style("", "content: none")), None);
        assert_eq!(generated_content(&style("", "")), None);
    }

    fn style(selector: &str, declarations: &str) -> crate::computed::ComputedStyle {
        let parsed = parse_document("<!doctype html><p id=x></p>");
        let selector = if selector.is_empty() { "#x" } else { selector };
        let sheet = parse_stylesheet(&format!("{selector} {{ {declarations} }}"));
        let styles = compute_document_styles(
            &parsed.dom,
            &[CascadeInput {
                sheet: &sheet,
                origin: CascadeOrigin::Author,
            }],
            &PropertyRegistry::standard_baseline(),
            &crate::computed::ComputationLimits::default(),
            &MatchContext::default(),
        );
        let selector = crate::selector::parse_selector_list("#x").expect("selector");
        let nodes = crate::selector::select_all(
            &parsed.dom,
            parsed.dom.document(),
            &selector,
            &MatchContext::default(),
        );
        styles[&nodes[0]].clone()
    }
}
