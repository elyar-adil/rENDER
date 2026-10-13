//! Focus state of a document and the rules for what may hold it.
//!
//! The focused area is document state, so it lives on [`Dom`] beside the tree
//! it refers to. The focusable-area rules are DOM-level (HTML §6.6.3, with the
//! "being rendered" condition approximated by the `hidden` attribute, because
//! this crate has no layout), and they are shared by the script bindings and
//! by sequential focus navigation in `render-core`.
//!
//! Focus is cleared, not re-derived. When a mutation leaves the focused element
//! unable to hold focus, the focus fixup rule (HTML §6.6.3) sets the focused area
//! to `None`, so a control that is disabled and later re-enabled does not regain
//! focus on its own. The specification runs this rule when the document is
//! rendered; this crate applies it at the mutation that causes it. The two differ
//! only when a focused element is detached and reinserted within one task.

use super::{Dom, Namespace, NodeId};

/// Parse a string with the HTML "rules for parsing integers" (HTML §2.3.4.2).
///
/// Leading ASCII whitespace is skipped, an optional sign is read, and digits are
/// taken up to the first non-digit. Trailing characters are ignored, so
/// `"3px"` is `3`. Returns `None` when there are no digits or the value does not
/// fit in an `i32`.
#[must_use]
pub fn parse_html_integer(input: &str) -> Option<i32> {
    let trimmed = input.trim_start_matches([' ', '\t', '\n', '\x0C', '\r']);
    let (negative, digits) = match trimmed.as_bytes().first() {
        Some(b'-') => (true, &trimmed[1..]),
        Some(b'+') => (false, &trimmed[1..]),
        _ => (false, trimmed),
    };
    let length = digits.bytes().take_while(u8::is_ascii_digit).count();
    if length == 0 {
        return None;
    }
    let magnitude: i64 = digits[..length].parse().ok()?;
    let signed = if negative { -magnitude } else { magnitude };
    i32::try_from(signed).ok()
}

impl Dom {
    /// The element holding focus in this document, or `None` when the viewport
    /// does. This is the document's focused area (HTML §6.6.2).
    #[must_use]
    pub const fn focused_element(&self) -> Option<NodeId> {
        self.focused_area
    }

    /// Record `node` as the focused area, or the viewport for `None`.
    ///
    /// Returns `false` and leaves the state unchanged when `node` is not a
    /// focusable area, so the focused area is always either the viewport or a
    /// focusable element. Callers run the focusing steps and fire the focus
    /// events; this method only stores the state.
    pub fn set_focused_element(&mut self, node: Option<NodeId>) -> bool {
        match node {
            Some(target) if !self.is_focusable_area(target) => false,
            _ => {
                self.focused_area = node;
                true
            }
        }
    }

    /// Whether `node` is a focusable area (HTML §6.6.3), as far as the tree and
    /// its attributes decide it.
    ///
    /// A node is focusable when it is connected, not hidden by the `hidden`
    /// attribute, not an actually disabled form control, and either has a valid
    /// `tabindex` or is natively focusable.
    #[must_use]
    pub fn is_focusable_area(&self, node: NodeId) -> bool {
        self.is_connected(node)
            && !self.is_hidden_by_attribute(node)
            && !self.is_disabled_form_control(node)
            && (self.parsed_tab_index(node).is_some() || self.is_natively_focusable(node))
    }

    /// The focus delegate of a shadow host (HTML §6.6.3): when the host's shadow
    /// root delegates focus, the first focusable area in that root's tree, in tree
    /// order. `None` when the host does not delegate, or its root has no focusable
    /// area.
    #[must_use]
    pub fn focus_delegate(&self, host: NodeId) -> Option<NodeId> {
        let root = self.shadow_root(host)?;
        if !self.shadow_root_data(root)?.delegates_focus {
            return None;
        }
        let mut stack: Vec<NodeId> = self
            .children(root)
            .unwrap_or_default()
            .iter()
            .rev()
            .copied()
            .collect();
        while let Some(node) = stack.pop() {
            if self.is_focusable_area(node) {
                return Some(node);
            }
            stack.extend(
                self.children(node)
                    .unwrap_or_default()
                    .iter()
                    .rev()
                    .copied(),
            );
        }
        None
    }

    /// The `tabindex` content attribute parsed as an integer, when it is present
    /// and parses (HTML §6.6.4).
    #[must_use]
    pub fn parsed_tab_index(&self, node: NodeId) -> Option<i32> {
        self.attribute(node, "tabindex")
            .ok()
            .flatten()
            .and_then(parse_html_integer)
    }

    /// Whether `node` is an HTML element that is focusable without a `tabindex`
    /// (HTML §6.6.3, "focusable area" list).
    #[must_use]
    pub fn is_natively_focusable(&self, node: NodeId) -> bool {
        let Some((Namespace::Html, local_name)) = self.element_data(node) else {
            return false;
        };
        match local_name {
            "a" | "area" => self.has_attribute(node, "href"),
            "button" | "select" | "textarea" | "iframe" => true,
            "input" => !self.attribute_equals_ascii_case(node, "type", "hidden"),
            _ => false,
        }
    }

    /// Whether `node` is a form control that is disabled in the sense of HTML
    /// §4.10.18.5: it has a `disabled` attribute, or it is a descendant of a
    /// `fieldset` with a `disabled` attribute other than inside that fieldset's
    /// first `legend` child.
    #[must_use]
    pub fn is_disabled_form_control(&self, node: NodeId) -> bool {
        let Some((Namespace::Html, local_name)) = self.element_data(node) else {
            return false;
        };
        if !matches!(local_name, "button" | "input" | "select" | "textarea") {
            return false;
        }
        if self.has_attribute(node, "disabled") {
            return true;
        }
        let mut ancestor = self.parent(node);
        while let Some(current) = ancestor {
            if self.is_html_element_named(current, "fieldset")
                && self.has_attribute(current, "disabled")
                && !self.is_in_first_legend_of(node, current)
            {
                return true;
            }
            ancestor = self.parent(current);
        }
        false
    }

    /// The focus fixup rule (HTML §6.6.3): when the focused element stops being
    /// focusable, the focused area becomes the viewport. No event fires.
    ///
    /// Called after every recorded mutation. It does nothing when nothing has
    /// focus, so pages that never focus anything pay one comparison.
    pub(crate) fn apply_focus_fixup(&mut self) {
        if let Some(node) = self.focused_area
            && !self.is_focusable_area(node)
        {
            self.focused_area = None;
        }
    }

    fn is_hidden_by_attribute(&self, node: NodeId) -> bool {
        let mut candidate = Some(node);
        while let Some(current) = candidate {
            if self.has_attribute(current, "hidden") {
                return true;
            }
            candidate = self.parent(current);
        }
        false
    }

    fn is_html_element_named(&self, node: NodeId, name: &str) -> bool {
        matches!(self.element_data(node), Some((Namespace::Html, local)) if local == name)
    }

    fn is_in_first_legend_of(&self, node: NodeId, fieldset: NodeId) -> bool {
        let first_legend = self.children(fieldset).and_then(|children| {
            children
                .iter()
                .copied()
                .find(|child| matches!(self.element_data(*child), Some((_, "legend"))))
        });
        let Some(legend) = first_legend else {
            return false;
        };
        let mut candidate = Some(node);
        while let Some(current) = candidate {
            if current == legend {
                return true;
            }
            candidate = self.parent(current);
        }
        false
    }

    fn has_attribute(&self, node: NodeId, name: &str) -> bool {
        self.attribute(node, name)
            .is_ok_and(|value| value.is_some())
    }

    fn attribute_equals_ascii_case(&self, node: NodeId, name: &str, expected: &str) -> bool {
        self.attribute(node, name)
            .ok()
            .flatten()
            .is_some_and(|value| value.eq_ignore_ascii_case(expected))
    }
}

#[cfg(test)]
mod tests {
    use super::{Dom, NodeId, parse_html_integer};

    #[test]
    fn integers_follow_the_html_parsing_rules() {
        assert_eq!(parse_html_integer("0"), Some(0));
        assert_eq!(parse_html_integer("  -7"), Some(-7));
        assert_eq!(parse_html_integer("+12"), Some(12));
        assert_eq!(parse_html_integer("3px"), Some(3));
        assert_eq!(parse_html_integer("\t\n 5"), Some(5));
        assert_eq!(parse_html_integer(""), None);
        assert_eq!(parse_html_integer("-"), None);
        assert_eq!(parse_html_integer("x1"), None);
        assert_eq!(parse_html_integer("99999999999"), None);
    }

    fn element(dom: &mut Dom, parent: NodeId, name: &str) -> NodeId {
        let node = dom.create_element(name);
        dom.append_child(parent, node).expect("append");
        node
    }

    fn attach_body(dom: &mut Dom) -> NodeId {
        let document = dom.document();
        element(dom, document, "body")
    }

    #[test]
    fn focusable_areas_follow_tabindex_natural_focus_and_disabled_rules() {
        let mut dom = Dom::new();
        let body = attach_body(&mut dom);
        let button = element(&mut dom, body, "button");
        let anchor_without_href = element(&mut dom, body, "a");
        let anchor = element(&mut dom, body, "a");
        dom.set_attribute(anchor, "href", "/x").expect("href");
        let plain_div = element(&mut dom, body, "div");
        let negative_div = element(&mut dom, body, "div");
        dom.set_attribute(negative_div, "tabindex", "-1")
            .expect("tabindex");
        let junk_div = element(&mut dom, body, "div");
        dom.set_attribute(junk_div, "tabindex", "x")
            .expect("tabindex");

        assert!(dom.is_focusable_area(button));
        assert!(!dom.is_focusable_area(anchor_without_href));
        assert!(dom.is_focusable_area(anchor));
        assert!(!dom.is_focusable_area(plain_div));
        assert!(dom.is_focusable_area(negative_div));
        assert!(!dom.is_focusable_area(junk_div));

        dom.set_attribute(button, "disabled", "").expect("disabled");
        assert!(!dom.is_focusable_area(button));
    }

    #[test]
    fn a_disabled_fieldset_disables_its_controls_except_inside_the_first_legend() {
        let mut dom = Dom::new();
        let body = attach_body(&mut dom);
        let fieldset = element(&mut dom, body, "fieldset");
        dom.set_attribute(fieldset, "disabled", "")
            .expect("disabled");
        let legend = element(&mut dom, fieldset, "legend");
        let legend_input = element(&mut dom, legend, "input");
        let body_input = element(&mut dom, fieldset, "input");

        assert!(dom.is_focusable_area(legend_input));
        assert!(!dom.is_focusable_area(body_input));
        assert!(!dom.is_focusable_area(fieldset));
    }

    #[test]
    fn hidden_and_detached_elements_are_not_focusable() {
        let mut dom = Dom::new();
        let body = attach_body(&mut dom);
        let section = element(&mut dom, body, "section");
        dom.set_attribute(section, "hidden", "").expect("hidden");
        let inside = element(&mut dom, section, "button");
        let detached = element(&mut dom, body, "button");
        dom.remove_child(body, detached).expect("remove");

        assert!(!dom.is_focusable_area(inside));
        assert!(!dom.is_focusable_area(detached));
    }

    #[test]
    fn focus_is_cleared_when_the_focused_element_stops_being_focusable() {
        let mut dom = Dom::new();
        let body = attach_body(&mut dom);
        let button = element(&mut dom, body, "button");
        let div = element(&mut dom, body, "div");

        assert!(
            !dom.set_focused_element(Some(div)),
            "a plain div cannot hold focus"
        );
        assert_eq!(dom.focused_element(), None);
        assert!(dom.set_focused_element(Some(button)));
        assert_eq!(dom.focused_element(), Some(button));

        dom.set_attribute(button, "disabled", "").expect("disabled");
        assert_eq!(
            dom.focused_element(),
            None,
            "disabling the focused control clears focus"
        );
        assert!(!dom.set_focused_element(Some(button)));
        dom.remove_attribute(button, "disabled").expect("re-enable");
        assert_eq!(
            dom.focused_element(),
            None,
            "a re-enabled control does not regain focus on its own"
        );
    }
}
