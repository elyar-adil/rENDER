//! Shadow trees: DOM Standard §4.2.2 (`attachShadow`, shadow roots, the
//! `ShadowRootMode` and `SlotAssignmentMode` enums), §4.2.2.2 (slots and slot
//! assignment), §2.9 (event retargeting and the event path), and the
//! shadow-including tree that decides connectedness (§4.2.3 and §4.4).
//!
//! A shadow root is a node in the arena, typed as a document fragment with no
//! parent. The host element records its root in [`ElementData::shadow_root`]
//! and the root records its host, so either direction is one lookup. Like a
//! template's content, the root is reachable from the host but is not one of
//! the host's children, so every walk that follows `children` stays inside one
//! tree.
//!
//! Rendering sees the *flat tree* (see [`Dom::flat_children`]). A host's flat
//! children are its shadow root's children, and a slot's flat children are the
//! nodes assigned to it. A host's light children are therefore drawn only where
//! a slot takes them, and nowhere when no slot does.
//!
//! [`ElementData::shadow_root`]: super::ElementData::shadow_root

use std::collections::BTreeMap;

use super::{Dom, DomError, MutationKind, Namespace, Node, NodeId, NodeKind};

/// The `mode` of a shadow root (DOM §4.2.2.1 `ShadowRootMode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShadowRootMode {
    /// `element.shadowRoot` returns the root to any script.
    Open,
    /// `element.shadowRoot` returns `null`, and the root is hidden from a
    /// composed event's path.
    Closed,
}

impl ShadowRootMode {
    /// The IDL value of this mode.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
        }
    }
}

/// The `slotAssignment` of a shadow root (DOM §4.2.2.1 `SlotAssignmentMode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotAssignmentMode {
    /// Slottables are assigned only by `slot.assign(...)`.
    Manual,
    /// Slottables are assigned by their `slot` attribute against each slot's
    /// `name`.
    Named,
}

impl SlotAssignmentMode {
    /// The IDL value of this mode.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Named => "named",
        }
    }
}

/// The data a shadow root node carries (DOM §4.2.2.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShadowRootData {
    /// The element the root is attached to.
    pub host: NodeId,
    pub mode: ShadowRootMode,
    /// `delegatesFocus`: focusing the host focuses its first focusable child.
    pub delegates_focus: bool,
    pub slot_assignment: SlotAssignmentMode,
}

/// One node an event visits (DOM §2.9 "event path" struct): the node the
/// listeners run on, and the `target` and `relatedTarget` they see there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EventPathStep {
    /// The node whose listeners run at this step.
    pub invocation_target: NodeId,
    /// The `target` a listener at this step reads: the shadow-adjusted target
    /// of the last step at or before this one that has one.
    pub target: NodeId,
    /// Whether this step has a shadow-adjusted target of its own. Its listeners
    /// then run in the `AT_TARGET` phase, even when the event does not bubble.
    pub at_target: bool,
    /// The `relatedTarget` a listener at this step reads, retargeted against
    /// this step's node.
    pub related_target: Option<NodeId>,
    /// Whether this step is a closed shadow root. `composedPath()` hides what
    /// lies inside such a root from listeners outside it.
    pub root_of_closed_tree: bool,
}

/// Whether `name` is a valid custom element name (HTML §4.13.2): it starts with
/// an ASCII lowercase letter, contains a hyphen, uses only `PCENChar`s, and is
/// not one of the names reserved for SVG and MathML.
#[must_use]
pub fn is_valid_custom_element_name(name: &str) -> bool {
    const RESERVED: [&str; 8] = [
        "annotation-xml",
        "color-profile",
        "font-face",
        "font-face-src",
        "font-face-uri",
        "font-face-format",
        "font-face-name",
        "missing-glyph",
    ];
    name.starts_with(|ch: char| ch.is_ascii_lowercase())
        && name.contains('-')
        && name.chars().all(is_pcen_char)
        && !RESERVED.contains(&name)
}

/// HTML §4.13.2 `PCENChar`.
fn is_pcen_char(ch: char) -> bool {
    matches!(
        ch,
        '-' | '.'
            | '0'..='9'
            | '_'
            | 'a'..='z'
            | '\u{B7}'
            | '\u{C0}'..='\u{D6}'
            | '\u{D8}'..='\u{F6}'
            | '\u{F8}'..='\u{37D}'
            | '\u{37F}'..='\u{1FFF}'
            | '\u{200C}'..='\u{200D}'
            | '\u{203F}'..='\u{2040}'
            | '\u{2070}'..='\u{218F}'
            | '\u{2C00}'..='\u{2FEF}'
            | '\u{3001}'..='\u{D7FF}'
            | '\u{F900}'..='\u{FDCF}'
            | '\u{FDF0}'..='\u{FFFD}'
            | '\u{10000}'..='\u{EFFFF}'
    )
}

/// DOM §4.2.2 "valid shadow host name" for an element in the HTML namespace:
/// a valid custom element name, or one of the elements the standard lists.
#[must_use]
pub fn is_valid_shadow_host_name(name: &str) -> bool {
    is_valid_custom_element_name(name)
        || matches!(
            name,
            "article"
                | "aside"
                | "blockquote"
                | "body"
                | "div"
                | "footer"
                | "h1"
                | "h2"
                | "h3"
                | "h4"
                | "h5"
                | "h6"
                | "header"
                | "main"
                | "nav"
                | "p"
                | "section"
                | "span"
        )
}

impl Dom {
    /// `Element.attachShadow(init)` (DOM §4.2.2): create a shadow root on `host`
    /// and return it.
    ///
    /// # Errors
    ///
    /// Returns [`DomErrorKind::NotSupported`](super::DomErrorKind::NotSupported)
    /// when the element is not in the HTML namespace or cannot host a shadow root,
    /// [`DomErrorKind::InvalidState`](super::DomErrorKind::InvalidState) when it
    /// already hosts one, and
    /// [`DomErrorKind::InvalidNodeType`](super::DomErrorKind::InvalidNodeType) when
    /// `host` is not an element.
    pub fn attach_shadow(
        &mut self,
        host: NodeId,
        mode: ShadowRootMode,
        delegates_focus: bool,
        slot_assignment: SlotAssignmentMode,
    ) -> Result<NodeId, DomError> {
        let (valid_host, attached) = match self.kind(host)? {
            NodeKind::Element(data) => (
                data.namespace == Namespace::Html && is_valid_shadow_host_name(&data.local_name),
                data.shadow_root.is_some(),
            ),
            _ => return Err(DomError::invalid_node_type("attachShadow needs an element")),
        };
        if !valid_host {
            return Err(DomError::not_supported(
                "this element cannot host a shadow root",
            ));
        }
        if attached {
            return Err(DomError::invalid_state(
                "this element already hosts a shadow root",
            ));
        }
        let root = self.allocate(NodeKind::ShadowRoot(ShadowRootData {
            host,
            mode,
            delegates_focus,
            slot_assignment,
        }));
        // A shadow root is in its host's node document (DOM §4.2.2.1).
        let owner = self.owner_of(host);
        self.set_node_document(root, owner);
        if let Some(data) = self.element_data_mut(host) {
            data.shadow_root = Some(root);
        }
        self.record_mutations([MutationKind::ShadowRootAttached { host }]);
        Ok(root)
    }

    /// The shadow root attached to `host`, whatever its mode. Whether a script
    /// may see it is decided by the caller from [`ShadowRootData::mode`].
    #[must_use]
    pub fn shadow_root(&self, host: NodeId) -> Option<NodeId> {
        match self.node(host).map(Node::kind) {
            Some(NodeKind::Element(data)) => data.shadow_root,
            _ => None,
        }
    }

    /// The data of a shadow root node, and `None` for every other node.
    #[must_use]
    pub fn shadow_root_data(&self, node: NodeId) -> Option<&ShadowRootData> {
        match self.node(node).map(Node::kind) {
            Some(NodeKind::ShadowRoot(data)) => Some(data),
            _ => None,
        }
    }

    /// The host of a shadow root, and `None` for every other node.
    #[must_use]
    pub fn shadow_host(&self, node: NodeId) -> Option<NodeId> {
        self.shadow_root_data(node).map(|data| data.host)
    }

    /// The shadow-including root of `node` (DOM §4.2.3): its root, or if that is
    /// a shadow root, the shadow-including root of that root's host.
    #[must_use]
    pub fn shadow_including_root(&self, node: NodeId) -> NodeId {
        let mut root = self.tree_root(node);
        while let Some(host) = self.shadow_host(root) {
            root = self.tree_root(host);
        }
        root
    }

    /// `Node.getRootNode({composed})` (DOM §4.4): the root of `node`, or with
    /// `composed`, its shadow-including root.
    #[must_use]
    pub fn root_node(&self, node: NodeId, composed: bool) -> NodeId {
        if composed {
            self.shadow_including_root(node)
        } else {
            self.tree_root(node)
        }
    }

    /// Whether `ancestor` is a shadow-including inclusive ancestor of `node`:
    /// an ancestor by the parent chain, or reached by crossing from a shadow
    /// root to its host (DOM §4.2.3).
    #[must_use]
    pub fn is_shadow_including_inclusive_ancestor(&self, ancestor: NodeId, node: NodeId) -> bool {
        let mut current = Some(node);
        while let Some(candidate) = current {
            if candidate == ancestor {
                return true;
            }
            current = self
                .parent(candidate)
                .or_else(|| self.shadow_host(candidate));
        }
        false
    }

    /// DOM §4.2.5 "retarget A against B": `node` moved up out of any shadow
    /// trees that `other` cannot see into. `other` of `None` stands for a
    /// `Window`, which no shadow tree contains, so the result is outside all
    /// shadow trees.
    #[must_use]
    pub fn retarget(&self, node: NodeId, other: Option<NodeId>) -> NodeId {
        let mut current = node;
        loop {
            let root = self.tree_root(current);
            let Some(host) = self.shadow_host(root) else {
                return current;
            };
            if other.is_some_and(|other| self.is_shadow_including_inclusive_ancestor(root, other)) {
                return current;
            }
            current = host;
        }
    }

    /// DOM §2.9 "get the parent" for dispatch: the assigned slot of a
    /// slottable, else the host of a shadow root, else the parent. A shadow root
    /// does not pass a non-composed event to its host, unless the event started
    /// inside that root.
    #[must_use]
    pub fn event_parent(
        &self,
        node: NodeId,
        composed: bool,
        invocation_target: NodeId,
    ) -> Option<NodeId> {
        if let Some(slot) = self.assigned_slot(node) {
            return Some(slot);
        }
        if let Some(data) = self.shadow_root_data(node) {
            if !composed && self.tree_root(invocation_target) == node {
                return None;
            }
            return Some(data.host);
        }
        self.parent(node)
    }

    /// The nodes an event dispatched at `target` visits, in path order, as DOM
    /// §2.9 "dispatch" builds them. The first step is `target` itself; the
    /// document and the window above it are the caller's to append.
    ///
    /// `related` is the event's `relatedTarget`, if it is a node. An empty path
    /// means the dispatch visits nothing: the target is the event's
    /// `relatedTarget` after retargeting, but was not the original one.
    #[must_use]
    pub fn event_path(
        &self,
        target: NodeId,
        related: Option<NodeId>,
        composed: bool,
    ) -> Vec<EventPathStep> {
        let related_for_target = related.map(|node| self.retarget(node, Some(target)));
        if related_for_target == Some(target) && related != Some(target) {
            return Vec::new();
        }
        // Each visited node with its shadow-adjusted target, which is `None`
        // where the node is in the same tree as the target.
        let mut visited: Vec<(NodeId, Option<NodeId>)> = vec![(target, Some(target))];
        let mut current_target = target;
        let mut parent = self.event_parent(target, composed, target);
        while let Some(node) = parent {
            if self.is_shadow_including_inclusive_ancestor(self.tree_root(current_target), node) {
                visited.push((node, None));
            } else if Some(node) == related_for_target {
                break;
            } else {
                current_target = node;
                visited.push((node, Some(node)));
            }
            parent = self.event_parent(node, composed, target);
        }
        let mut resolved = target;
        visited
            .into_iter()
            .map(|(node, adjusted)| {
                if let Some(adjusted) = adjusted {
                    resolved = adjusted;
                }
                EventPathStep {
                    invocation_target: node,
                    target: resolved,
                    at_target: adjusted.is_some(),
                    related_target: related.map(|candidate| self.retarget(candidate, Some(node))),
                    root_of_closed_tree: self
                        .shadow_root_data(node)
                        .is_some_and(|data| data.mode == ShadowRootMode::Closed),
                }
            })
            .collect()
    }

    /// Whether `node` is an HTML `slot` element.
    #[must_use]
    pub fn is_slot(&self, node: NodeId) -> bool {
        matches!(
            self.node(node).map(Node::kind),
            Some(NodeKind::Element(data))
                if data.namespace == Namespace::Html && data.local_name == "slot"
        )
    }

    /// Whether `node` is a slottable: an element or a text node (DOM §4.2.2.2).
    fn is_slottable(&self, node: NodeId) -> bool {
        matches!(
            self.node(node).map(Node::kind),
            Some(NodeKind::Element(_) | NodeKind::Text(_))
        )
    }

    /// The name a slottable asks for: its `slot` attribute, or `""`. A text
    /// node has no `slot` attribute and asks for `""`.
    fn slottable_name(&self, node: NodeId) -> String {
        self.attribute(node, "slot")
            .ok()
            .flatten()
            .unwrap_or_default()
            .to_owned()
    }

    /// A slot's name: its `name` attribute, or `""`.
    fn slot_name(&self, slot: NodeId) -> String {
        self.attribute(slot, "name")
            .ok()
            .flatten()
            .unwrap_or_default()
            .to_owned()
    }

    /// The slots of the shadow tree rooted at `root`, in tree order, including
    /// slots inside fallback content. Nested shadow trees are separate trees and
    /// are not searched.
    fn slots_in_tree(&self, root: NodeId) -> Vec<NodeId> {
        let mut found = Vec::new();
        let mut stack: Vec<NodeId> = self
            .children(root)
            .unwrap_or_default()
            .iter()
            .rev()
            .copied()
            .collect();
        while let Some(node) = stack.pop() {
            if self.is_slot(node) {
                found.push(node);
            }
            stack.extend(
                self.children(node)
                    .unwrap_or_default()
                    .iter()
                    .rev()
                    .copied(),
            );
        }
        found
    }

    /// The nodes a slot has been given by `slot.assign(...)`, in the order given.
    fn manually_assigned_nodes(&self, slot: NodeId) -> &[NodeId] {
        match self.node(slot).map(Node::kind) {
            Some(NodeKind::Element(data)) => &data.manually_assigned_nodes,
            _ => &[],
        }
    }

    /// The slot assignment of every slottable child of the host of the shadow
    /// root `root`, as `(slottable, slot)` pairs in the host's child order
    /// (DOM §4.2.2.2 "assign slottables").
    ///
    /// Named: a slottable goes to the first slot in tree order whose name is its
    /// `slot` attribute. Manual: a slottable goes to the first slot whose
    /// manually assigned nodes contain it, and is otherwise unassigned.
    fn slot_assignments(&self, root: NodeId) -> Vec<(NodeId, NodeId)> {
        let Some(data) = self.shadow_root_data(root) else {
            return Vec::new();
        };
        let slots = self.slots_in_tree(root);
        if slots.is_empty() {
            return Vec::new();
        }
        let slottables = self
            .children(data.host)
            .unwrap_or_default()
            .iter()
            .copied()
            .filter(|child| self.is_slottable(*child));
        match data.slot_assignment {
            SlotAssignmentMode::Named => {
                let mut first_slot: BTreeMap<String, NodeId> = BTreeMap::new();
                for slot in &slots {
                    first_slot.entry(self.slot_name(*slot)).or_insert(*slot);
                }
                slottables
                    .filter_map(|child| {
                        first_slot
                            .get(&self.slottable_name(child))
                            .map(|slot| (child, *slot))
                    })
                    .collect()
            }
            SlotAssignmentMode::Manual => slottables
                .filter_map(|child| {
                    slots
                        .iter()
                        .find(|slot| self.manually_assigned_nodes(**slot).contains(&child))
                        .map(|slot| (child, *slot))
                })
                .collect(),
        }
    }

    /// `slottable.assignedSlot` (DOM §4.2.2.2): the slot `slottable` is assigned
    /// to, or `None` when it is not assigned to a slot in its parent's shadow
    /// tree.
    #[must_use]
    pub fn assigned_slot(&self, slottable: NodeId) -> Option<NodeId> {
        if !self.is_slottable(slottable) {
            return None;
        }
        let host = self.parent(slottable)?;
        let root = self.shadow_root(host)?;
        self.slot_assignments(root)
            .into_iter()
            .find(|(node, _)| *node == slottable)
            .map(|(_, slot)| slot)
    }

    /// The nodes assigned to `slot` directly, in host child order. A slot that
    /// is not in a shadow tree has none.
    fn directly_assigned(&self, slot: NodeId) -> Vec<NodeId> {
        let root = self.tree_root(slot);
        if self.shadow_root_data(root).is_none() {
            return Vec::new();
        }
        self.slot_assignments(root)
            .into_iter()
            .filter(|(_, assigned_slot)| *assigned_slot == slot)
            .map(|(node, _)| node)
            .collect()
    }

    /// `slot.assignedNodes({flatten})` (DOM §4.2.2.2). Without `flatten` these
    /// are the assigned slottables. With it, a slot assigned nothing contributes
    /// its own children, and a slot among the assigned nodes contributes its
    /// flattened slottables in its place.
    #[must_use]
    pub fn assigned_nodes(&self, slot: NodeId, flatten: bool) -> Vec<NodeId> {
        if !self.is_slot(slot) {
            return Vec::new();
        }
        if flatten {
            self.flattened_slottables(slot)
        } else {
            self.directly_assigned(slot)
        }
    }

    /// DOM §4.2.2.2 "find flattened slottables".
    fn flattened_slottables(&self, slot: NodeId) -> Vec<NodeId> {
        let assigned = self.directly_assigned(slot);
        if assigned.is_empty() {
            return self.children(slot).unwrap_or_default().to_vec();
        }
        let mut result = Vec::new();
        for node in assigned {
            let in_shadow_tree = self.shadow_root_data(self.tree_root(node)).is_some();
            if self.is_slot(node) && in_shadow_tree {
                result.extend(self.flattened_slottables(node));
            } else {
                result.push(node);
            }
        }
        result
    }

    /// The children of `node` in the flat tree, which is what rendering walks.
    ///
    /// A host's flat children are its shadow root's children. A slot's flat
    /// children are the nodes assigned to it, or its own children when it has
    /// none. Every other node's flat children are its children.
    #[must_use]
    pub fn flat_children(&self, node: NodeId) -> Vec<NodeId> {
        if let Some(root) = self.shadow_root(node) {
            return self.children(root).unwrap_or_default().to_vec();
        }
        if self.is_slot(node) {
            let assigned = self.directly_assigned(node);
            if !assigned.is_empty() {
                return assigned;
            }
        }
        self.children(node).unwrap_or_default().to_vec()
    }

    /// `slot.assign(...nodes)` (DOM §4.2.2.2, manual slot assignment): make
    /// `nodes` the slot's manually assigned nodes. Each slottable is taken away
    /// from any other slot of the same shadow tree, and duplicates and
    /// non-slottables are dropped.
    ///
    /// # Errors
    ///
    /// Returns [`DomErrorKind::InvalidNodeType`](super::DomErrorKind::InvalidNodeType)
    /// when `slot` is not an HTML slot element.
    pub fn assign_slot_nodes(&mut self, slot: NodeId, nodes: &[NodeId]) -> Result<(), DomError> {
        if !self.is_slot(slot) {
            return Err(DomError::invalid_node_type("assign needs a slot element"));
        }
        let mut unique: Vec<NodeId> = Vec::new();
        for node in nodes {
            if self.is_slottable(*node) && !unique.contains(node) {
                unique.push(*node);
            }
        }
        let root = self.tree_root(slot);
        if self.shadow_root_data(root).is_some() {
            for other in self.slots_in_tree(root) {
                if other != slot
                    && let Some(data) = self.element_data_mut(other)
                {
                    data.manually_assigned_nodes
                        .retain(|candidate| !unique.contains(candidate));
                }
            }
        }
        if let Some(data) = self.element_data_mut(slot) {
            data.manually_assigned_nodes = unique;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{EventPathStep, ShadowRootMode, SlotAssignmentMode, is_valid_custom_element_name};
    use crate::{Dom, NodeId};

    /// A `<div id=host>` with a shadow root in `mode`.
    fn host_with_root(dom: &mut Dom, mode: ShadowRootMode) -> (NodeId, NodeId) {
        let host = dom.create_element("div");
        let body = dom.create_element("body");
        dom.append_child(dom.document(), body).unwrap();
        dom.append_child(body, host).unwrap();
        let root = dom
            .attach_shadow(host, mode, false, SlotAssignmentMode::Named)
            .unwrap();
        (host, root)
    }

    #[test]
    fn attach_shadow_validates_the_host_and_refuses_a_second_root() {
        let mut dom = Dom::new();
        let div = dom.create_element("div");
        let input = dom.create_element("input");
        let custom = dom.create_element("my-widget");
        let svg = dom.create_element_ns(crate::Namespace::Svg, "g");
        let root = dom
            .attach_shadow(div, ShadowRootMode::Open, true, SlotAssignmentMode::Manual)
            .unwrap();
        assert_eq!(dom.shadow_root(div), Some(root));
        assert_eq!(dom.shadow_host(root), Some(div));
        let data = dom.shadow_root_data(root).unwrap();
        assert!(data.delegates_focus);
        assert_eq!(data.slot_assignment, SlotAssignmentMode::Manual);
        assert_eq!(
            dom.attach_shadow(div, ShadowRootMode::Open, false, SlotAssignmentMode::Named)
                .unwrap_err()
                .kind(),
            crate::DomErrorKind::InvalidState
        );
        for refused in [input, svg] {
            assert_eq!(
                dom.attach_shadow(
                    refused,
                    ShadowRootMode::Open,
                    false,
                    SlotAssignmentMode::Named
                )
                .unwrap_err()
                .kind(),
                crate::DomErrorKind::NotSupported
            );
        }
        // A valid custom element name is a valid host even when undefined.
        assert!(
            dom.attach_shadow(
                custom,
                ShadowRootMode::Closed,
                false,
                SlotAssignmentMode::Named
            )
            .is_ok()
        );
    }

    #[test]
    fn custom_element_names_follow_the_pcen_grammar() {
        assert!(is_valid_custom_element_name("my-widget"));
        assert!(is_valid_custom_element_name("a-b.c_d"));
        assert!(!is_valid_custom_element_name("widget"));
        assert!(!is_valid_custom_element_name("My-widget"));
        assert!(!is_valid_custom_element_name("-widget"));
        assert!(!is_valid_custom_element_name("font-face"));
        assert!(!is_valid_custom_element_name("a-\u{D7FF}\u{E000}"));
    }

    #[test]
    fn shadow_root_is_a_parentless_fragment_reachable_only_from_its_host() {
        let mut dom = Dom::new();
        let (host, root) = host_with_root(&mut dom, ShadowRootMode::Open);
        assert_eq!(dom.parent(root), None);
        assert!(dom.children(host).unwrap().is_empty());
        let inner = dom.create_element("span");
        dom.append_child(root, inner).unwrap();
        assert_eq!(dom.parent(inner), Some(root));
        assert_eq!(dom.flat_children(host), vec![inner]);
    }

    #[test]
    fn connectedness_follows_the_shadow_including_root() {
        let mut dom = Dom::new();
        let (host, root) = host_with_root(&mut dom, ShadowRootMode::Closed);
        let inner = dom.create_element("span");
        dom.append_child(root, inner).unwrap();
        assert!(dom.is_connected(inner));
        assert_eq!(dom.shadow_including_root(inner), dom.document());
        assert_eq!(dom.root_node(inner, false), root);
        assert_eq!(dom.root_node(inner, true), dom.document());
        let body = dom.parent(host).unwrap();
        dom.remove_child(body, host).unwrap();
        assert!(!dom.is_connected(inner));
        assert_eq!(dom.root_node(inner, true), host);
    }

    #[test]
    fn retarget_climbs_out_of_shadow_trees_the_other_node_cannot_see() {
        let mut dom = Dom::new();
        let (host, root) = host_with_root(&mut dom, ShadowRootMode::Open);
        let inner = dom.create_element("input");
        dom.append_child(root, inner).unwrap();
        let outside = dom.create_element("p");
        let body = dom.parent(host).unwrap();
        dom.append_child(body, outside).unwrap();
        assert_eq!(dom.retarget(inner, Some(outside)), host);
        assert_eq!(dom.retarget(inner, Some(inner)), inner);
        assert_eq!(dom.retarget(inner, None), host);
        assert_eq!(dom.retarget(host, None), host);
    }

    #[test]
    fn a_composed_event_visits_the_host_and_a_non_composed_one_stops_at_the_root() {
        let mut dom = Dom::new();
        let (host, root) = host_with_root(&mut dom, ShadowRootMode::Open);
        let inner = dom.create_element("span");
        dom.append_child(root, inner).unwrap();
        let nodes = |steps: &[EventPathStep]| {
            steps
                .iter()
                .map(|step| step.invocation_target)
                .collect::<Vec<_>>()
        };
        let body = dom.parent(host).unwrap();
        let composed = dom.event_path(inner, None, true);
        assert_eq!(
            nodes(&composed),
            vec![inner, root, host, body, dom.document()]
        );
        // Adjusted targets: the host and everything above it see the host.
        assert_eq!(composed[0].target, inner);
        assert_eq!(composed[1].target, inner);
        assert_eq!(composed[2].target, host);
        assert_eq!(composed[4].target, host);
        let plain = dom.event_path(inner, None, false);
        assert_eq!(nodes(&plain), vec![inner, root]);
    }

    #[test]
    fn a_closed_root_is_marked_in_the_path_and_an_open_one_is_not() {
        let mut dom = Dom::new();
        let (host, root) = host_with_root(&mut dom, ShadowRootMode::Closed);
        let inner = dom.create_element("span");
        dom.append_child(root, inner).unwrap();
        let steps = dom.event_path(inner, None, true);
        assert!(
            steps
                .iter()
                .any(|step| step.invocation_target == root && step.root_of_closed_tree)
        );
        assert!(
            steps
                .iter()
                .all(|step| step.invocation_target != host || !step.root_of_closed_tree)
        );
    }

    #[test]
    fn a_related_target_inside_a_shadow_tree_reads_as_its_host_outside() {
        let mut dom = Dom::new();
        let (host, root) = host_with_root(&mut dom, ShadowRootMode::Open);
        let inner = dom.create_element("input");
        dom.append_child(root, inner).unwrap();
        let light = dom.create_element("input");
        let body = dom.parent(host).unwrap();
        dom.append_child(body, light).unwrap();
        let steps = dom.event_path(light, Some(inner), true);
        let first = steps[0];
        assert_eq!(first.related_target, Some(host));
        // The path climbs from the light input to the document, and every step
        // reads the related target as the host, which is outside the shadow tree.
        assert_eq!(steps.last().unwrap().related_target, Some(host));
        assert!(steps.iter().all(|step| step.related_target == Some(host)));
    }

    #[test]
    fn named_slots_take_slottables_by_their_slot_attribute() {
        let mut dom = Dom::new();
        let (host, root) = host_with_root(&mut dom, ShadowRootMode::Open);
        let default_slot = dom.create_element("slot");
        let named_slot = dom.create_element("slot");
        dom.set_attribute(named_slot, "name", "title").unwrap();
        dom.append_child(root, default_slot).unwrap();
        dom.append_child(root, named_slot).unwrap();
        let text = dom.create_text("plain");
        let title = dom.create_element("h1");
        dom.set_attribute(title, "slot", "title").unwrap();
        let stray = dom.create_element("b");
        dom.set_attribute(stray, "slot", "nowhere").unwrap();
        dom.append_child(host, text).unwrap();
        dom.append_child(host, title).unwrap();
        dom.append_child(host, stray).unwrap();
        assert_eq!(dom.assigned_slot(text), Some(default_slot));
        assert_eq!(dom.assigned_slot(title), Some(named_slot));
        assert_eq!(dom.assigned_slot(stray), None);
        assert_eq!(dom.assigned_nodes(default_slot, false), vec![text]);
        assert_eq!(dom.flat_children(named_slot), vec![title]);
        assert_eq!(dom.flat_children(default_slot), vec![text]);
    }

    #[test]
    fn a_slot_with_nothing_assigned_shows_its_fallback_children() {
        let mut dom = Dom::new();
        let (host, root) = host_with_root(&mut dom, ShadowRootMode::Open);
        let slot = dom.create_element("slot");
        let fallback = dom.create_text("fallback");
        dom.append_child(slot, fallback).unwrap();
        dom.append_child(root, slot).unwrap();
        assert_eq!(dom.flat_children(slot), vec![fallback]);
        assert_eq!(dom.assigned_nodes(slot, true), vec![fallback]);
        // An element without a `slot` attribute asks for the default slot, so
        // it takes the place of the fallback content.
        let light = dom.create_element("i");
        dom.append_child(host, light).unwrap();
        assert_eq!(dom.flat_children(slot), vec![light]);
        // Naming another slot sends it away, and the fallback comes back.
        dom.set_attribute(light, "slot", "x").unwrap();
        assert_eq!(dom.flat_children(slot), vec![fallback]);
    }

    #[test]
    fn a_nested_slot_flattens_into_what_it_is_assigned() {
        // H0 hosts R0, which holds H and a default slot S0. H hosts R, which
        // holds `outer`. H's light child is S_x, a slot of R0 with fallback
        // text. Flattening `outer` replaces the nested slot S_x with its own
        // flattened slottables, which is its fallback here.
        let mut dom = Dom::new();
        let (h0, r0) = host_with_root(&mut dom, ShadowRootMode::Open);
        let s0 = dom.create_element("slot");
        dom.append_child(r0, s0).unwrap();
        let h = dom.create_element("div");
        dom.append_child(r0, h).unwrap();
        let r = dom
            .attach_shadow(h, ShadowRootMode::Open, false, SlotAssignmentMode::Named)
            .unwrap();
        let outer = dom.create_element("slot");
        dom.append_child(r, outer).unwrap();
        let s_x = dom.create_element("slot");
        dom.append_child(h, s_x).unwrap();
        let fallback = dom.create_text("fb");
        dom.append_child(s_x, fallback).unwrap();
        let light_text = dom.create_text("t");
        dom.append_child(h0, light_text).unwrap();
        assert_eq!(dom.assigned_slot(s_x), Some(outer));
        assert_eq!(dom.assigned_nodes(outer, false), vec![s_x]);
        assert_eq!(dom.assigned_nodes(outer, true), vec![fallback]);
        assert_eq!(dom.assigned_nodes(s0, false), vec![light_text]);
    }

    #[test]
    fn manual_assignment_takes_a_slottable_from_another_slot_and_is_lazy_about_removal() {
        let mut dom = Dom::new();
        let manual_host = dom.create_element("div");
        let body = dom.document();
        dom.append_child(body, manual_host).unwrap();
        let manual_root = dom
            .attach_shadow(
                manual_host,
                ShadowRootMode::Open,
                false,
                SlotAssignmentMode::Manual,
            )
            .unwrap();
        let first = dom.create_element("slot");
        let second = dom.create_element("slot");
        dom.append_child(manual_root, first).unwrap();
        dom.append_child(manual_root, second).unwrap();
        let child = dom.create_element("em");
        dom.append_child(manual_host, child).unwrap();
        assert_eq!(dom.assigned_slot(child), None);
        dom.assign_slot_nodes(first, &[child]).unwrap();
        assert_eq!(dom.assigned_slot(child), Some(first));
        dom.assign_slot_nodes(second, &[child, child]).unwrap();
        assert_eq!(dom.assigned_slot(child), Some(second));
        assert_eq!(dom.assigned_nodes(second, false), vec![child]);
        assert!(dom.assigned_nodes(first, false).is_empty());
        // Removing the child unassigns it, and it is not visible in the flat tree.
        dom.remove_child(manual_host, child).unwrap();
        assert_eq!(dom.assigned_slot(child), None);
        assert!(dom.flat_children(second).is_empty());
    }
}
