//! Named access keys: which elements a document exposes under a name.
//!
//! HTML §7.4.4 (a `Window`) and §3.1.5 (a `Document`) name an HTML element by
//! its `id` attribute, and an element that is an HTML `embed`, `form`, `iframe`,
//! `img` or `object` also by its `name` attribute, provided the element is in
//! the document. The index records every element under each non-empty key it
//! carries, and every attribute write keeps it current, so a lookup never walks
//! the tree to find candidates. A detached element stays in the index and is
//! filtered out by [`Dom::named_elements`], so moving an element needs no index
//! work.

use std::collections::BTreeMap;

use crate::{Dom, ElementData, Namespace, Node, NodeId, NodeKind};

/// Element keys: the `id` value of every HTML element and the `name` value of
/// every HTML element that takes one, each mapped to the elements carrying it.
#[derive(Clone, Debug, Default)]
pub(crate) struct NamedIndex {
    keys: BTreeMap<String, Vec<NodeId>>,
}

/// The `local_name` of an element whose `name` attribute names it (HTML §3.1.5).
fn takes_name_attribute(local_name: &str) -> bool {
    matches!(local_name, "embed" | "form" | "iframe" | "img" | "object")
}

/// The value of the attribute with no namespace and local name `local_name`.
fn attribute_value<'a>(data: &'a ElementData, local_name: &str) -> Option<&'a str> {
    data.attributes
        .iter()
        .find(|attribute| attribute.namespace.is_none() && attribute.local_name == local_name)
        .map(|attribute| attribute.value.as_str())
}

/// The non-empty keys an element is named by, without duplicates. Only an HTML
/// element is named at all.
fn element_keys(data: &ElementData) -> Vec<&str> {
    if data.namespace != Namespace::Html {
        return Vec::new();
    }
    let mut keys = Vec::with_capacity(2);
    if let Some(id) = attribute_value(data, "id").filter(|id| !id.is_empty()) {
        keys.push(id);
    }
    if takes_name_attribute(&data.local_name)
        && let Some(name) = attribute_value(data, "name").filter(|name| !name.is_empty())
        && !keys.contains(&name)
    {
        keys.push(name);
    }
    keys
}

impl Dom {
    /// Records element `node` under each key it currently carries. Called after
    /// every write that can change an element's `id` or `name`, and for every
    /// element a clone or a graft brings in with attributes.
    pub(crate) fn index_named(&mut self, node: NodeId) {
        let Some(keys) = self.named_keys_of(node) else {
            return;
        };
        for key in keys {
            let nodes = self.named.keys.entry(key).or_default();
            if !nodes.contains(&node) {
                nodes.push(node);
            }
        }
    }

    /// Removes element `node` from each key it currently carries. Called before
    /// an attribute write, so the keys are the ones the index recorded.
    pub(crate) fn unindex_named(&mut self, node: NodeId) {
        let Some(keys) = self.named_keys_of(node) else {
            return;
        };
        for key in keys {
            if let Some(nodes) = self.named.keys.get_mut(&key) {
                nodes.retain(|candidate| *candidate != node);
                if nodes.is_empty() {
                    self.named.keys.remove(&key);
                }
            }
        }
    }

    /// The keys of element `node`, owned, or `None` for a node that is not an
    /// element.
    fn named_keys_of(&self, node: NodeId) -> Option<Vec<String>> {
        match self.node(node).map(Node::kind)? {
            NodeKind::Element(data) => {
                Some(element_keys(data).into_iter().map(str::to_owned).collect())
            }
            _ => None,
        }
    }

    /// Whether element `node` is named `key`: it is an HTML element whose `id`
    /// is `key`, or which takes a `name` attribute and has `name` equal to `key`.
    #[must_use]
    pub fn names_element(&self, node: NodeId, key: &str) -> bool {
        matches!(
            self.node(node).map(Node::kind),
            Some(NodeKind::Element(data)) if element_keys(data).contains(&key)
        )
    }

    /// The elements of `document` that are named `key`, in tree order, each
    /// once. An element is in `document` when following its parents reaches
    /// `document` itself, so a detached element, a template's contents and an
    /// element of a shadow tree are all excluded. The empty key names nothing.
    #[must_use]
    pub fn named_elements(&self, document: NodeId, key: &str) -> Vec<NodeId> {
        if key.is_empty() {
            return Vec::new();
        }
        let Some(candidates) = self.named.keys.get(key) else {
            return Vec::new();
        };
        let mut found: Vec<(Vec<usize>, NodeId)> = candidates
            .iter()
            .copied()
            .filter(|node| self.names_element(*node, key))
            .filter_map(|node| {
                let (root, position) = self.tree_position(node);
                (root == document).then_some((position, node))
            })
            .collect();
        found.sort_by(|left, right| left.0.cmp(&right.0));
        found.into_iter().map(|(_, node)| node).collect()
    }

    /// Every name under which `document` names at least one element, in key
    /// order. This is the set [`Dom::named_elements`] answers non-empty for, so a
    /// caller that lists the named properties of a window sees each name once.
    #[must_use]
    pub fn named_keys(&self, document: NodeId) -> Vec<String> {
        self.named
            .keys
            .keys()
            .filter(|key| !self.named_elements(document, key).is_empty())
            .cloned()
            .collect()
    }

    /// Whether element `node` is named `key` as a listed element of a form: its
    /// `id` or its `name` attribute is `key` (HTML §4.10.3). Unlike
    /// [`Self::names_element`], a `name` attribute names every listed element,
    /// not only the ones that take one, and the empty key names nothing.
    #[must_use]
    pub fn names_form_control(&self, node: NodeId, key: &str) -> bool {
        if key.is_empty() || !self.is_listed_element(node) {
            return false;
        }
        matches!(
            self.node(node).map(Node::kind),
            Some(NodeKind::Element(data))
                if attribute_value(data, "id") == Some(key)
                    || attribute_value(data, "name") == Some(key)
        )
    }

    /// The members of the form-control collection rooted at `root` (HTML
    /// §4.10.3, §4.10.4), in tree order. For a `form` root these are the listed
    /// elements whose form owner is that form, rooted at the form's own tree (see
    /// [`Self::form_owner_elements`]). For any other root, a `fieldset`, they are
    /// the listed descendants of the root, the root itself excluded.
    #[must_use]
    pub fn form_control_members(&self, root: NodeId) -> Vec<NodeId> {
        if self.is_form_element(root) {
            return self.form_owner_elements(root);
        }
        self.children(root)
            .unwrap_or_default()
            .iter()
            .flat_map(|child| self.listed_elements_within(*child))
            .collect()
    }

    /// The members of the form-control collection rooted at `root` that are
    /// named `key`, in tree order (HTML §4.10.3, the named item of
    /// `HTMLFormControlsCollection`).
    #[must_use]
    pub fn form_control_named(&self, root: NodeId, key: &str) -> Vec<NodeId> {
        if key.is_empty() {
            return Vec::new();
        }
        self.form_control_members(root)
            .into_iter()
            .filter(|node| self.names_form_control(*node, key))
            .collect()
    }

    /// The names the form-control collection rooted at `root` supports: the
    /// non-empty `id` and `name` of each member, in tree order, each name once.
    /// These are the supported property names of a form (HTML §4.10.3).
    #[must_use]
    pub fn form_control_names(&self, root: NodeId) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for node in self.form_control_members(root) {
            let Some(NodeKind::Element(data)) = self.node(node).map(Node::kind) else {
                continue;
            };
            for key in [attribute_value(data, "id"), attribute_value(data, "name")]
                .into_iter()
                .flatten()
                .filter(|key| !key.is_empty())
            {
                if !names.iter().any(|name| name.as_str() == key) {
                    names.push(key.to_owned());
                }
            }
        }
        names
    }

    /// The root of `node` reached through parents, and the child index of each
    /// step from that root down to `node`. Comparing two positions in order
    /// gives document order: an ancestor's position is a prefix, so it sorts
    /// first.
    fn tree_position(&self, node: NodeId) -> (NodeId, Vec<usize>) {
        let mut position = Vec::new();
        let mut current = node;
        while let Some(parent) = self.parent(current) {
            let index = self
                .children(parent)
                .and_then(|siblings| siblings.iter().position(|sibling| *sibling == current))
                .unwrap_or(0);
            position.push(index);
            current = parent;
        }
        position.reverse();
        (current, position)
    }
}

#[cfg(test)]
mod tests {
    use crate::{Dom, Namespace, NodeId};

    /// A document whose `main` holds `<div id=a>` and `<img name=a>` in that order.
    fn sample() -> (Dom, NodeId, NodeId, NodeId) {
        let mut dom = Dom::new();
        let document = dom.document();
        let container = dom.create_element("main");
        dom.append_child(document, container).unwrap();
        let div = dom.create_element("div");
        dom.set_attribute(div, "id", "a").unwrap();
        let img = dom.create_element("img");
        dom.set_attribute(img, "name", "a").unwrap();
        dom.append_child(container, div).unwrap();
        dom.append_child(container, img).unwrap();
        (dom, document, div, img)
    }

    #[test]
    fn named_elements_are_the_connected_ones_in_tree_order() {
        let (mut dom, document, div, img) = sample();
        assert_eq!(dom.named_elements(document, "a"), vec![div, img]);
        assert!(dom.names_element(div, "a"));
        assert!(dom.names_element(img, "a"));
        let container = dom.parent(img).unwrap();
        dom.remove_child(container, img).unwrap();
        assert_eq!(dom.named_elements(document, "a"), vec![div]);
    }

    #[test]
    fn a_name_attribute_names_only_the_elements_that_take_one() {
        let mut dom = Dom::new();
        let document = dom.document();
        let div = dom.create_element("div");
        dom.set_attribute(div, "name", "n").unwrap();
        dom.append_child(document, div).unwrap();
        assert!(dom.named_elements(document, "n").is_empty());
        assert!(dom.named_elements(document, "").is_empty());
    }

    #[test]
    fn a_write_moves_the_element_between_keys() {
        let (mut dom, document, div, _img) = sample();
        dom.set_attribute(div, "id", "b").unwrap();
        assert!(!dom.named_elements(document, "a").contains(&div));
        assert_eq!(dom.named_elements(document, "b"), vec![div]);
        dom.remove_attribute(div, "id").unwrap();
        assert!(dom.named_elements(document, "b").is_empty());
    }

    #[test]
    fn an_svg_element_with_an_id_is_not_named() {
        let mut dom = Dom::new();
        let document = dom.document();
        let svg = dom.create_element_ns(Namespace::Svg, "svg");
        dom.set_attribute(svg, "id", "s").unwrap();
        dom.append_child(document, svg).unwrap();
        assert!(dom.named_elements(document, "s").is_empty());
    }

    #[test]
    fn a_clone_is_indexed_and_a_graft_is_indexed_under_its_own_document() {
        let (mut dom, document, div, img) = sample();
        let container = dom.parent(div).unwrap();
        let copy = dom.clone_node(div, false).unwrap();
        dom.append_child(container, copy).unwrap();
        assert_eq!(dom.named_elements(document, "a"), vec![div, img, copy]);

        let mut other = Dom::new();
        let other_document = other.document();
        let span = other.create_element("span");
        other.set_attribute(span, "id", "g").unwrap();
        other.append_child(other_document, span).unwrap();
        let grafted = dom.graft_document(other);
        let grafted_span = dom.children(grafted).unwrap()[0];
        assert_eq!(dom.named_elements(grafted, "g"), vec![grafted_span]);
        assert!(dom.named_elements(document, "g").is_empty());
    }

    /// A form holding two inputs named `b` (one with an id), a text input
    /// named `t`, an `input type=image` named `img`, and an `img` named `m`
    /// (not listed, so never a control). Returns the form and its controls.
    fn form_sample() -> (Dom, NodeId, Vec<NodeId>) {
        let mut dom = Dom::new();
        let document = dom.document();
        let form = dom.create_element("form");
        dom.append_child(document, form).unwrap();
        let mut controls = Vec::new();
        for (tag, attributes) in [
            ("input", vec![("name", "b"), ("id", "i1")]),
            ("input", vec![("name", "b")]),
            ("input", vec![("type", "text"), ("name", "t")]),
            ("input", vec![("type", "image"), ("name", "img")]),
            ("img", vec![("name", "m")]),
        ] {
            let node = dom.create_element(tag);
            for (name, value) in attributes {
                dom.set_attribute(node, name, value).unwrap();
            }
            dom.append_child(form, node).unwrap();
            controls.push(node);
        }
        (dom, form, controls)
    }

    #[test]
    fn a_form_control_is_named_by_its_id_or_its_name() {
        let (dom, form, controls) = form_sample();
        assert!(dom.names_form_control(controls[0], "i1"));
        assert!(dom.names_form_control(controls[0], "b"));
        assert!(dom.names_form_control(controls[1], "b"));
        assert!(!dom.names_form_control(controls[0], ""));
        assert!(
            !dom.names_form_control(controls[4], "m"),
            "an img is not listed"
        );
        assert_eq!(
            dom.form_control_named(form, "b"),
            vec![controls[0], controls[1]]
        );
        assert_eq!(dom.form_control_named(form, "t"), vec![controls[2]]);
        assert!(dom.form_control_named(form, "img").is_empty());
        assert!(dom.form_control_named(form, "m").is_empty());
        assert!(dom.form_control_named(form, "").is_empty());
    }

    #[test]
    fn form_control_members_exclude_image_inputs_and_images() {
        let (dom, form, controls) = form_sample();
        assert_eq!(
            dom.form_control_members(form),
            vec![controls[0], controls[1], controls[2]]
        );
    }

    #[test]
    fn form_control_names_are_ids_and_names_in_tree_order_each_once() {
        let (dom, form, _) = form_sample();
        assert_eq!(dom.form_control_names(form), vec!["i1", "b", "t"]);
    }

    #[test]
    fn a_fieldset_collection_holds_its_listed_descendants_not_itself() {
        let mut dom = Dom::new();
        let document = dom.document();
        let fieldset = dom.create_element("fieldset");
        dom.set_attribute(fieldset, "name", "fs").unwrap();
        dom.append_child(document, fieldset).unwrap();
        let inner = dom.create_element("fieldset");
        let input = dom.create_element("input");
        let span = dom.create_element("span");
        dom.append_child(fieldset, inner).unwrap();
        dom.append_child(inner, input).unwrap();
        dom.append_child(fieldset, span).unwrap();
        assert_eq!(dom.form_control_members(fieldset), vec![inner, input]);
        assert_eq!(dom.form_control_named(fieldset, "fs"), Vec::<NodeId>::new());
    }

    #[test]
    fn a_removed_control_leaves_its_form_collection() {
        let (mut dom, form, controls) = form_sample();
        dom.remove_child(form, controls[1]).unwrap();
        assert_eq!(dom.form_control_named(form, "b"), vec![controls[0]]);
        assert_eq!(dom.form_owner(controls[1]), None);
    }
}
