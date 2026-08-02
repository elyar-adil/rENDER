//! DOM node storage and tree mutation primitives.
//!
//! Nodes live in an arena and receive monotonically increasing identifiers that
//! are never reused. This gives JavaScript wrappers, layout boxes, Agent
//! observations, and traces a shared identity without exposing Rust references
//! across mutation boundaries.

use std::collections::{BTreeSet, VecDeque};
use std::error::Error;
use std::fmt;

const DEFAULT_MUTATION_JOURNAL_CAPACITY: usize = 4_096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(u64);

impl NodeId {
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }

    fn from_index(index: usize) -> Self {
        Self(u64::try_from(index).expect("DOM arena exceeded u64 node capacity"))
    }

    fn index(self) -> Option<usize> {
        usize::try_from(self.0).ok()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DomRevision(u64);

impl DomRevision {
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MutationKind {
    ChildList {
        target: NodeId,
        added: Vec<NodeId>,
        removed: Vec<NodeId>,
    },
    Attribute {
        target: NodeId,
        local_name: String,
    },
    CharacterData {
        target: NodeId,
    },
}

impl MutationKind {
    #[must_use]
    pub const fn target(&self) -> NodeId {
        match self {
            Self::ChildList { target, .. }
            | Self::Attribute { target, .. }
            | Self::CharacterData { target } => *target,
        }
    }

    #[must_use]
    pub const fn impact(&self) -> MutationImpact {
        match self {
            Self::ChildList { .. } | Self::Attribute { .. } => MutationImpact::ALL_RENDERING,
            Self::CharacterData { .. } => MutationImpact::LAYOUT_PAINT_ACCESSIBILITY,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MutationRecord {
    pub revision: DomRevision,
    pub kind: MutationKind,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MutationImpact(u8);

impl MutationImpact {
    const STYLE_BIT: u8 = 1 << 0;
    const LAYOUT_BIT: u8 = 1 << 1;
    const PAINT_BIT: u8 = 1 << 2;
    const ACCESSIBILITY_BIT: u8 = 1 << 3;

    pub const ALL_RENDERING: Self =
        Self(Self::STYLE_BIT | Self::LAYOUT_BIT | Self::PAINT_BIT | Self::ACCESSIBILITY_BIT);
    pub const LAYOUT_PAINT_ACCESSIBILITY: Self =
        Self(Self::LAYOUT_BIT | Self::PAINT_BIT | Self::ACCESSIBILITY_BIT);

    #[must_use]
    pub const fn affects_style(self) -> bool {
        self.0 & Self::STYLE_BIT != 0
    }

    #[must_use]
    pub const fn affects_layout(self) -> bool {
        self.0 & Self::LAYOUT_BIT != 0
    }

    #[must_use]
    pub const fn affects_paint(self) -> bool {
        self.0 & Self::PAINT_BIT != 0
    }

    #[must_use]
    pub const fn affects_accessibility(self) -> bool {
        self.0 & Self::ACCESSIBILITY_BIT != 0
    }

    const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MutationBatch {
    pub from_revision: DomRevision,
    pub to_revision: DomRevision,
    pub records: Vec<MutationRecord>,
}

impl MutationBatch {
    #[must_use]
    pub fn impact(&self) -> MutationImpact {
        self.records
            .iter()
            .fold(MutationImpact::default(), |impact, record| {
                impact.union(record.kind.impact())
            })
    }

    /// Conservative roots for selector/style invalidation. The style engine
    /// may narrow these with selector dependency metadata later.
    #[must_use]
    pub fn invalidation_roots(&self) -> BTreeSet<NodeId> {
        self.records
            .iter()
            .map(|record| record.kind.target())
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MutationHistoryError {
    RevisionInFuture {
        requested: DomRevision,
        current: DomRevision,
    },
    HistoryDiscarded {
        requested: DomRevision,
        oldest_available: DomRevision,
    },
}

impl fmt::Display for MutationHistoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RevisionInFuture { requested, current } => write!(
                formatter,
                "requested DOM revision {} is newer than current revision {}",
                requested.as_u64(),
                current.as_u64()
            ),
            Self::HistoryDiscarded {
                requested,
                oldest_available,
            } => write!(
                formatter,
                "mutation history after revision {} was discarded; oldest available base is {}",
                requested.as_u64(),
                oldest_available.as_u64()
            ),
        }
    }
}

impl Error for MutationHistoryError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Namespace {
    Html,
    Svg,
    MathMl,
    Other(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attribute {
    pub namespace: Option<String>,
    pub prefix: Option<String>,
    pub local_name: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ElementData {
    pub namespace: Namespace,
    pub local_name: String,
    pub attributes: Vec<Attribute>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentTypeData {
    pub name: String,
    pub public_id: String,
    pub system_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeKind {
    Document,
    DocumentFragment,
    DocumentType(DocumentTypeData),
    Element(ElementData),
    Text(String),
    Comment(String),
    ProcessingInstruction { target: String, data: String },
}

impl NodeKind {
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Document => "#document",
            Self::DocumentFragment => "#document-fragment",
            Self::DocumentType(_) => "#doctype",
            Self::Element(_) => "element",
            Self::Text(_) => "#text",
            Self::Comment(_) => "#comment",
            Self::ProcessingInstruction { .. } => "#processing-instruction",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    id: NodeId,
    parent: Option<NodeId>,
    children: Vec<NodeId>,
    kind: NodeKind,
}

impl Node {
    #[must_use]
    pub const fn id(&self) -> NodeId {
        self.id
    }

    #[must_use]
    pub const fn parent(&self) -> Option<NodeId> {
        self.parent
    }

    #[must_use]
    pub fn children(&self) -> &[NodeId] {
        &self.children
    }

    #[must_use]
    pub const fn kind(&self) -> &NodeKind {
        &self.kind
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DomErrorKind {
    HierarchyRequest,
    NotFound,
    InvalidNodeType,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DomError {
    kind: DomErrorKind,
    message: String,
}

impl DomError {
    #[must_use]
    pub const fn kind(&self) -> DomErrorKind {
        self.kind
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    fn new(kind: DomErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    fn hierarchy(message: impl Into<String>) -> Self {
        Self::new(DomErrorKind::HierarchyRequest, message)
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self::new(DomErrorKind::NotFound, message)
    }

    fn invalid_node_type(message: impl Into<String>) -> Self {
        Self::new(DomErrorKind::InvalidNodeType, message)
    }
}

impl fmt::Display for DomError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}: {}", self.kind, self.message)
    }
}

impl Error for DomError {}

/// Arena-backed DOM with stable node identities and a monotonic mutation
/// revision suitable for incremental rendering and Agent observations.
#[derive(Clone, Debug)]
pub struct Dom {
    nodes: Vec<Node>,
    document: NodeId,
    mutation_revision: u64,
    mutation_journal: VecDeque<MutationRecord>,
    mutation_journal_capacity: usize,
    oldest_available_revision: DomRevision,
}

impl Default for Dom {
    fn default() -> Self {
        Self::new()
    }
}

impl Dom {
    #[must_use]
    pub fn new() -> Self {
        let document = NodeId::from_index(0);
        Self {
            nodes: vec![Node {
                id: document,
                parent: None,
                children: Vec::new(),
                kind: NodeKind::Document,
            }],
            document,
            mutation_revision: 0,
            mutation_journal: VecDeque::new(),
            mutation_journal_capacity: DEFAULT_MUTATION_JOURNAL_CAPACITY,
            oldest_available_revision: DomRevision::default(),
        }
    }

    #[must_use]
    pub const fn document(&self) -> NodeId {
        self.document
    }

    #[must_use]
    pub const fn mutation_revision(&self) -> u64 {
        self.mutation_revision
    }

    #[must_use]
    pub const fn revision(&self) -> DomRevision {
        DomRevision(self.mutation_revision)
    }

    /// Bound retained mutation history. A zero capacity keeps revision tracking
    /// but requires downstream consumers to perform a full refresh.
    pub fn set_mutation_journal_capacity(&mut self, capacity: usize) {
        self.mutation_journal_capacity = capacity;
        while self.mutation_journal.len() > capacity {
            if let Some(discarded) = self.mutation_journal.pop_front() {
                self.oldest_available_revision = discarded.revision;
            }
        }
    }

    /// Return every retained mutation newer than `revision` without consuming
    /// it, so style, layout, accessibility, and JS observers can advance
    /// independently.
    ///
    /// # Errors
    ///
    /// Returns an error when the revision is in the future or its required
    /// journal prefix has already been discarded.
    pub fn mutations_since(
        &self,
        revision: DomRevision,
    ) -> Result<MutationBatch, MutationHistoryError> {
        let current = self.revision();
        if revision > current {
            return Err(MutationHistoryError::RevisionInFuture {
                requested: revision,
                current,
            });
        }
        if revision < self.oldest_available_revision {
            return Err(MutationHistoryError::HistoryDiscarded {
                requested: revision,
                oldest_available: self.oldest_available_revision,
            });
        }
        Ok(MutationBatch {
            from_revision: revision,
            to_revision: current,
            records: self
                .mutation_journal
                .iter()
                .filter(|record| record.revision > revision)
                .cloned()
                .collect(),
        })
    }

    #[must_use]
    pub fn node(&self, node: NodeId) -> Option<&Node> {
        node.index().and_then(|index| self.nodes.get(index))
    }

    #[must_use]
    pub fn parent(&self, node: NodeId) -> Option<NodeId> {
        self.node(node).and_then(Node::parent)
    }

    #[must_use]
    pub fn children(&self, node: NodeId) -> Option<&[NodeId]> {
        self.node(node).map(Node::children)
    }

    #[must_use]
    pub fn next_sibling(&self, node: NodeId) -> Option<NodeId> {
        let parent = self.parent(node)?;
        let siblings = self.children(parent)?;
        let index = siblings.iter().position(|candidate| *candidate == node)?;
        siblings.get(index + 1).copied()
    }

    #[must_use]
    pub fn previous_sibling(&self, node: NodeId) -> Option<NodeId> {
        let parent = self.parent(node)?;
        let siblings = self.children(parent)?;
        let index = siblings.iter().position(|candidate| *candidate == node)?;
        index
            .checked_sub(1)
            .and_then(|previous| siblings.get(previous))
            .copied()
    }

    #[must_use]
    pub fn is_connected(&self, node: NodeId) -> bool {
        let mut current = Some(node);
        while let Some(candidate) = current {
            if candidate == self.document {
                return true;
            }
            current = self.parent(candidate);
        }
        false
    }

    pub fn create_document_fragment(&mut self) -> NodeId {
        self.allocate(NodeKind::DocumentFragment)
    }

    pub fn create_document_type(
        &mut self,
        name: impl Into<String>,
        public_id: impl Into<String>,
        system_id: impl Into<String>,
    ) -> NodeId {
        self.allocate(NodeKind::DocumentType(DocumentTypeData {
            name: name.into(),
            public_id: public_id.into(),
            system_id: system_id.into(),
        }))
    }

    pub fn create_element(&mut self, local_name: impl Into<String>) -> NodeId {
        self.allocate(NodeKind::Element(ElementData {
            namespace: Namespace::Html,
            local_name: local_name.into().to_ascii_lowercase(),
            attributes: Vec::new(),
        }))
    }

    pub fn create_element_ns(
        &mut self,
        namespace: Namespace,
        local_name: impl Into<String>,
    ) -> NodeId {
        self.allocate(NodeKind::Element(ElementData {
            namespace,
            local_name: local_name.into(),
            attributes: Vec::new(),
        }))
    }

    pub fn create_text(&mut self, data: impl Into<String>) -> NodeId {
        self.allocate(NodeKind::Text(data.into()))
    }

    pub fn create_comment(&mut self, data: impl Into<String>) -> NodeId {
        self.allocate(NodeKind::Comment(data.into()))
    }

    /// Insert character data, coalescing it with the parent's final Text node.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::append_child`] when a new Text node is
    /// required, or [`DomErrorKind::NotFound`] for an unknown parent.
    pub fn append_text(
        &mut self,
        parent: NodeId,
        data: impl AsRef<str>,
    ) -> Result<NodeId, DomError> {
        self.require_node(parent)?;
        let data = data.as_ref();
        if data.is_empty() {
            return Err(DomError::invalid_node_type(
                "cannot append an empty character token",
            ));
        }
        if let Some(last_child) = self
            .children(parent)
            .and_then(|children| children.last())
            .copied()
            && let NodeKind::Text(existing) = &mut self.node_mut(last_child)?.kind
        {
            existing.push_str(data);
            self.record_mutations([MutationKind::CharacterData { target: last_child }]);
            return Ok(last_child);
        }
        let text = self.create_text(data);
        self.append_child(parent, text)?;
        Ok(text)
    }

    pub fn create_processing_instruction(
        &mut self,
        target: impl Into<String>,
        data: impl Into<String>,
    ) -> NodeId {
        self.allocate(NodeKind::ProcessingInstruction {
            target: target.into(),
            data: data.into(),
        })
    }

    /// Insert `node` before `reference`, or append it when `reference` is None.
    /// A document fragment inserts its children and becomes empty.
    ///
    /// # Errors
    ///
    /// Returns [`DomErrorKind::NotFound`] for unknown nodes or a reference that
    /// is not a child of `parent`, and [`DomErrorKind::HierarchyRequest`] when
    /// the mutation would violate DOM tree/document constraints.
    pub fn insert_before(
        &mut self,
        parent: NodeId,
        node: NodeId,
        reference: Option<NodeId>,
    ) -> Result<NodeId, DomError> {
        self.require_node(parent)?;
        self.require_node(node)?;
        if let Some(reference) = reference {
            self.require_node(reference)?;
            if self.parent(reference) != Some(parent) {
                return Err(DomError::not_found(
                    "reference node is not a child of the insertion parent",
                ));
            }
        }

        self.validate_parent_kind(parent)?;
        if node == parent || self.is_inclusive_ancestor(node, parent) {
            return Err(DomError::hierarchy(
                "inserting the node would create a cycle",
            ));
        }

        let effective_reference = if reference == Some(node) {
            self.next_sibling(node)
        } else {
            reference
        };
        let insertion_nodes = match self.kind(node)? {
            NodeKind::DocumentFragment => self.children(node).unwrap_or_default().to_vec(),
            _ => vec![node],
        };

        for insertion_node in &insertion_nodes {
            if self.is_inclusive_ancestor(*insertion_node, parent) {
                return Err(DomError::hierarchy(
                    "inserting a fragment child would create a cycle",
                ));
            }
            self.validate_child_kind(parent, *insertion_node)?;
        }
        self.validate_document_children(parent, &insertion_nodes, effective_reference)?;

        if insertion_nodes.is_empty() {
            return Ok(node);
        }

        let removals = insertion_nodes
            .iter()
            .filter_map(|insertion_node| {
                self.parent(*insertion_node)
                    .map(|old_parent| (old_parent, *insertion_node))
            })
            .collect::<Vec<_>>();

        let parent_index = parent
            .index()
            .ok_or_else(|| DomError::not_found("insertion parent is outside arena capacity"))?;
        for insertion_node in &insertion_nodes {
            self.detach_without_revision(*insertion_node);
        }
        let insertion_index = match effective_reference {
            Some(reference) => self
                .node(parent)
                .and_then(|parent_node| {
                    parent_node
                        .children
                        .iter()
                        .position(|candidate| *candidate == reference)
                })
                .ok_or_else(|| {
                    DomError::not_found("reference node disappeared during insertion")
                })?,
            None => self
                .node(parent)
                .map_or(0, |parent_node| parent_node.children.len()),
        };

        for (offset, insertion_node) in insertion_nodes.iter().copied().enumerate() {
            let insertion_node_index = insertion_node
                .index()
                .ok_or_else(|| DomError::not_found("inserted node is outside arena capacity"))?;
            self.nodes[parent_index]
                .children
                .insert(insertion_index + offset, insertion_node);
            self.nodes[insertion_node_index].parent = Some(parent);
        }
        let mut mutations = removals
            .into_iter()
            .map(|(target, removed)| MutationKind::ChildList {
                target,
                added: Vec::new(),
                removed: vec![removed],
            })
            .collect::<Vec<_>>();
        mutations.push(MutationKind::ChildList {
            target: parent,
            added: insertion_nodes,
            removed: Vec::new(),
        });
        self.record_mutations(mutations);
        Ok(node)
    }

    /// Append a node or document fragment to a parent.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::insert_before`].
    pub fn append_child(&mut self, parent: NodeId, node: NodeId) -> Result<NodeId, DomError> {
        self.insert_before(parent, node, None)
    }

    /// Remove an existing child from a parent while preserving its stable ID.
    ///
    /// # Errors
    ///
    /// Returns [`DomErrorKind::NotFound`] when either node is unknown or `child`
    /// is not an immediate child of `parent`.
    pub fn remove_child(&mut self, parent: NodeId, child: NodeId) -> Result<NodeId, DomError> {
        self.require_node(parent)?;
        self.require_node(child)?;
        if self.parent(child) != Some(parent) {
            return Err(DomError::not_found(
                "node is not a child of the supplied parent",
            ));
        }
        self.detach_without_revision(child);
        self.record_mutations([MutationKind::ChildList {
            target: parent,
            added: Vec::new(),
            removed: vec![child],
        }]);
        Ok(child)
    }

    /// Set an HTML attribute using ASCII case-insensitive local-name matching.
    ///
    /// # Errors
    ///
    /// Returns [`DomErrorKind::NotFound`] for an unknown node and
    /// [`DomErrorKind::InvalidNodeType`] when the node is not an element.
    pub fn set_attribute(
        &mut self,
        element: NodeId,
        local_name: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<(), DomError> {
        let namespace = match self.kind(element)? {
            NodeKind::Element(data) => data.namespace.clone(),
            _ => return Err(DomError::invalid_node_type("attributes require an element")),
        };
        let local_name = local_name.into();
        let local_name = if namespace == Namespace::Html {
            local_name.to_ascii_lowercase()
        } else {
            local_name
        };
        let value = value.into();
        let mutation_name = local_name.clone();
        let node = self.node_mut(element)?;
        let NodeKind::Element(data) = &mut node.kind else {
            unreachable!("element kind was checked")
        };
        if let Some(attribute) = data
            .attributes
            .iter_mut()
            .find(|attribute| attribute.namespace.is_none() && attribute.local_name == local_name)
        {
            attribute.value = value;
        } else {
            data.attributes.push(Attribute {
                namespace: None,
                prefix: None,
                local_name,
                value,
            });
        }
        self.record_mutations([MutationKind::Attribute {
            target: element,
            local_name: mutation_name,
        }]);
        Ok(())
    }

    /// Remove an HTML attribute using ASCII case-insensitive local-name
    /// matching. Missing attributes are ignored, as in the platform DOM.
    pub fn remove_attribute(&mut self, element: NodeId, local_name: &str) -> Result<(), DomError> {
        let NodeKind::Element(data) = self.kind(element)? else {
            return Err(DomError::invalid_node_type("attributes require an element"));
        };
        let normalized = if data.namespace == Namespace::Html {
            local_name.to_ascii_lowercase()
        } else {
            local_name.to_owned()
        };
        let node = self.node_mut(element)?;
        let NodeKind::Element(data) = &mut node.kind else {
            unreachable!("element kind was checked")
        };
        let removed = data.attributes.iter().position(|attribute| {
            attribute.namespace.is_none() && attribute.local_name == normalized
        });
        if let Some(index) = removed {
            data.attributes.remove(index);
            self.record_mutations([MutationKind::Attribute {
                target: element,
                local_name: normalized,
            }]);
        }
        Ok(())
    }

    /// Return an attribute value from an element.
    ///
    /// # Errors
    ///
    /// Returns an error when the node is unknown or is not an element.
    pub fn attribute(&self, element: NodeId, local_name: &str) -> Result<Option<&str>, DomError> {
        let NodeKind::Element(data) = self.kind(element)? else {
            return Err(DomError::invalid_node_type("attributes require an element"));
        };
        let normalized = if data.namespace == Namespace::Html {
            local_name.to_ascii_lowercase()
        } else {
            local_name.to_owned()
        };
        Ok(data
            .attributes
            .iter()
            .find(|attribute| attribute.namespace.is_none() && attribute.local_name == normalized)
            .map(|attribute| attribute.value.as_str()))
    }

    /// Replace character data for a Text, Comment, or `ProcessingInstruction`.
    ///
    /// # Errors
    ///
    /// Returns an error when the node is unknown or cannot contain character
    /// data.
    pub fn set_character_data(
        &mut self,
        node: NodeId,
        value: impl Into<String>,
    ) -> Result<(), DomError> {
        let value = value.into();
        match &mut self.node_mut(node)?.kind {
            NodeKind::Text(data)
            | NodeKind::Comment(data)
            | NodeKind::ProcessingInstruction { data, .. } => *data = value,
            _ => {
                return Err(DomError::invalid_node_type(
                    "node does not implement CharacterData",
                ));
            }
        }
        self.record_mutations([MutationKind::CharacterData { target: node }]);
        Ok(())
    }

    fn allocate(&mut self, kind: NodeKind) -> NodeId {
        let id = NodeId::from_index(self.nodes.len());
        self.nodes.push(Node {
            id,
            parent: None,
            children: Vec::new(),
            kind,
        });
        id
    }

    fn kind(&self, node: NodeId) -> Result<&NodeKind, DomError> {
        self.node(node)
            .map(Node::kind)
            .ok_or_else(|| DomError::not_found(format!("unknown node {}", node.as_u64())))
    }

    fn node_mut(&mut self, node: NodeId) -> Result<&mut Node, DomError> {
        let index = node
            .index()
            .ok_or_else(|| DomError::not_found(format!("unknown node {}", node.as_u64())))?;
        self.nodes
            .get_mut(index)
            .ok_or_else(|| DomError::not_found(format!("unknown node {}", node.as_u64())))
    }

    fn require_node(&self, node: NodeId) -> Result<(), DomError> {
        self.kind(node).map(|_| ())
    }

    fn validate_parent_kind(&self, parent: NodeId) -> Result<(), DomError> {
        match self.kind(parent)? {
            NodeKind::Document | NodeKind::DocumentFragment | NodeKind::Element(_) => Ok(()),
            _ => Err(DomError::hierarchy(
                "only Document, DocumentFragment, and Element can be insertion parents",
            )),
        }
    }

    fn validate_child_kind(&self, parent: NodeId, child: NodeId) -> Result<(), DomError> {
        let parent_kind = self.kind(parent)?;
        let child_kind = self.kind(child)?;
        if matches!(child_kind, NodeKind::Document | NodeKind::DocumentFragment) {
            return Err(DomError::hierarchy(
                "Document cannot be inserted and DocumentFragment must be expanded",
            ));
        }
        match parent_kind {
            NodeKind::Document => match child_kind {
                NodeKind::Element(_)
                | NodeKind::DocumentType(_)
                | NodeKind::Comment(_)
                | NodeKind::ProcessingInstruction { .. } => Ok(()),
                NodeKind::Text(_) => Err(DomError::hierarchy(
                    "Text nodes cannot be children of Document",
                )),
                NodeKind::Document | NodeKind::DocumentFragment => unreachable!(),
            },
            NodeKind::DocumentFragment | NodeKind::Element(_) => {
                if matches!(child_kind, NodeKind::DocumentType(_)) {
                    Err(DomError::hierarchy(
                        "DocumentType can only be inserted into Document",
                    ))
                } else {
                    Ok(())
                }
            }
            _ => unreachable!("parent kind was checked"),
        }
    }

    fn validate_document_children(
        &self,
        parent: NodeId,
        insertion_nodes: &[NodeId],
        reference: Option<NodeId>,
    ) -> Result<(), DomError> {
        if !matches!(self.kind(parent)?, NodeKind::Document) {
            return Ok(());
        }

        let mut candidate = self.children(parent).unwrap_or_default().to_vec();
        candidate.retain(|existing| !insertion_nodes.contains(existing));
        let index = reference.map_or(candidate.len(), |reference| {
            candidate
                .iter()
                .position(|existing| *existing == reference)
                .unwrap_or(candidate.len())
        });
        candidate.splice(index..index, insertion_nodes.iter().copied());

        let mut element_index = None;
        let mut doctype_index = None;
        for (index, child) in candidate.iter().copied().enumerate() {
            match self.kind(child)? {
                NodeKind::Element(_) => {
                    if element_index.replace(index).is_some() {
                        return Err(DomError::hierarchy(
                            "Document cannot contain more than one element child",
                        ));
                    }
                }
                NodeKind::DocumentType(_) => {
                    if doctype_index.replace(index).is_some() {
                        return Err(DomError::hierarchy(
                            "Document cannot contain more than one doctype",
                        ));
                    }
                }
                NodeKind::Comment(_) | NodeKind::ProcessingInstruction { .. } => {}
                NodeKind::Text(_) | NodeKind::Document | NodeKind::DocumentFragment => {
                    return Err(DomError::hierarchy("invalid child type in Document"));
                }
            }
        }
        if let (Some(doctype), Some(element)) = (doctype_index, element_index)
            && doctype > element
        {
            return Err(DomError::hierarchy(
                "DocumentType must precede the document element",
            ));
        }
        Ok(())
    }

    fn is_inclusive_ancestor(&self, ancestor: NodeId, node: NodeId) -> bool {
        let mut current = Some(node);
        while let Some(candidate) = current {
            if candidate == ancestor {
                return true;
            }
            current = self.parent(candidate);
        }
        false
    }

    fn detach_without_revision(&mut self, node: NodeId) {
        let Some(parent) = self.parent(node) else {
            return;
        };
        if let Some(parent_node) = parent.index().and_then(|index| self.nodes.get_mut(index)) {
            parent_node.children.retain(|candidate| *candidate != node);
        }
        if let Some(node) = node.index().and_then(|index| self.nodes.get_mut(index)) {
            node.parent = None;
        }
    }

    fn record_mutations(&mut self, mutations: impl IntoIterator<Item = MutationKind>) {
        let mutations = mutations.into_iter().collect::<Vec<_>>();
        if mutations.is_empty() {
            return;
        }
        self.mutation_revision = self.mutation_revision.saturating_add(1);
        let revision = self.revision();
        for kind in mutations {
            self.mutation_journal
                .push_back(MutationRecord { revision, kind });
        }
        while self.mutation_journal.len() > self.mutation_journal_capacity {
            if let Some(discarded) = self.mutation_journal.pop_front() {
                self.oldest_available_revision = discarded.revision;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Dom, DomErrorKind, MutationHistoryError, MutationKind, Namespace, NodeKind};

    #[test]
    fn node_ids_are_stable_and_never_reused_when_detached() {
        let mut dom = Dom::new();
        let root = dom.create_element("HTML");
        let child = dom.create_element("body");
        assert!(root.as_u64() < child.as_u64());

        dom.append_child(dom.document(), root).unwrap();
        dom.append_child(root, child).unwrap();
        dom.remove_child(root, child).unwrap();

        assert_eq!(dom.node(child).unwrap().id(), child);
        assert_eq!(dom.parent(child), None);
        assert!(!dom.is_connected(child));
        assert!(dom.is_connected(root));
    }

    #[test]
    fn appending_an_existing_node_moves_it_without_changing_identity() {
        let mut dom = Dom::new();
        let parent = dom.create_element("div");
        let first = dom.create_element("span");
        let second = dom.create_element("strong");
        dom.append_child(parent, first).unwrap();
        dom.append_child(parent, second).unwrap();
        dom.append_child(parent, first).unwrap();

        assert_eq!(dom.children(parent).unwrap(), &[second, first]);
        assert_eq!(dom.parent(first), Some(parent));
    }

    #[test]
    fn insertion_rejects_cycles_and_invalid_parent_types() {
        let mut dom = Dom::new();
        let parent = dom.create_element("div");
        let child = dom.create_element("span");
        let text = dom.create_text("hello");
        dom.append_child(parent, child).unwrap();

        let cycle = dom.append_child(child, parent).unwrap_err();
        assert_eq!(cycle.kind(), DomErrorKind::HierarchyRequest);
        let invalid_parent = dom.append_child(text, child).unwrap_err();
        assert_eq!(invalid_parent.kind(), DomErrorKind::HierarchyRequest);
    }

    #[test]
    fn document_enforces_element_doctype_and_text_constraints() {
        let mut dom = Dom::new();
        let doctype = dom.create_document_type("html", "", "");
        let html = dom.create_element("html");
        dom.append_child(dom.document(), doctype).unwrap();
        dom.append_child(dom.document(), html).unwrap();

        let second_element = dom.create_element("svg");
        assert_eq!(
            dom.append_child(dom.document(), second_element)
                .unwrap_err()
                .kind(),
            DomErrorKind::HierarchyRequest
        );
        let text = dom.create_text("not allowed");
        assert_eq!(
            dom.append_child(dom.document(), text).unwrap_err().kind(),
            DomErrorKind::HierarchyRequest
        );

        let mut reversed = Dom::new();
        let reversed_html = reversed.create_element("html");
        let reversed_doctype = reversed.create_document_type("html", "", "");
        reversed
            .append_child(reversed.document(), reversed_html)
            .unwrap();
        assert_eq!(
            reversed
                .append_child(reversed.document(), reversed_doctype)
                .unwrap_err()
                .kind(),
            DomErrorKind::HierarchyRequest
        );
    }

    #[test]
    fn document_fragment_inserts_children_and_becomes_empty() {
        let mut dom = Dom::new();
        let parent = dom.create_element("div");
        let fragment = dom.create_document_fragment();
        let first = dom.create_element("span");
        let text = dom.create_text("middle");
        let last = dom.create_comment("last");
        dom.append_child(fragment, first).unwrap();
        dom.append_child(fragment, text).unwrap();
        dom.append_child(fragment, last).unwrap();

        let before_revision = dom.mutation_revision();
        dom.append_child(parent, fragment).unwrap();
        assert_eq!(dom.children(parent).unwrap(), &[first, text, last]);
        assert!(dom.children(fragment).unwrap().is_empty());
        assert_eq!(dom.parent(first), Some(parent));
        assert_eq!(dom.mutation_revision(), before_revision + 1);
    }

    #[test]
    fn invalid_fragment_insertion_is_atomic() {
        let mut dom = Dom::new();
        let fragment = dom.create_document_fragment();
        let first = dom.create_element("html");
        let second = dom.create_element("svg");
        dom.append_child(fragment, first).unwrap();
        dom.append_child(fragment, second).unwrap();

        assert_eq!(
            dom.append_child(dom.document(), fragment)
                .unwrap_err()
                .kind(),
            DomErrorKind::HierarchyRequest
        );
        assert_eq!(dom.children(fragment).unwrap(), &[first, second]);
        assert_eq!(dom.parent(first), Some(fragment));
    }

    #[test]
    fn insert_before_checks_reference_parent_and_preserves_order() {
        let mut dom = Dom::new();
        let parent = dom.create_element("div");
        let unrelated_parent = dom.create_element("section");
        let first = dom.create_element("a");
        let second = dom.create_element("b");
        let inserted = dom.create_element("i");
        dom.append_child(parent, first).unwrap();
        dom.append_child(parent, second).unwrap();
        dom.insert_before(parent, inserted, Some(second)).unwrap();
        assert_eq!(dom.children(parent).unwrap(), &[first, inserted, second]);

        let unrelated = dom.create_element("em");
        dom.append_child(unrelated_parent, unrelated).unwrap();
        assert_eq!(
            dom.insert_before(parent, unrelated_parent, Some(unrelated))
                .unwrap_err()
                .kind(),
            DomErrorKind::NotFound
        );
    }

    #[test]
    fn html_names_and_attributes_use_ascii_case_insensitive_normalization() {
        let mut dom = Dom::new();
        let element = dom.create_element("DIV");
        dom.set_attribute(element, "CLASS", "first").unwrap();
        dom.set_attribute(element, "class", "second").unwrap();

        let NodeKind::Element(data) = dom.node(element).unwrap().kind() else {
            panic!("expected element");
        };
        assert_eq!(data.namespace, Namespace::Html);
        assert_eq!(data.local_name, "div");
        assert_eq!(data.attributes.len(), 1);
        assert_eq!(dom.attribute(element, "Class").unwrap(), Some("second"));

        let namespaced = dom.create_element_ns(Namespace::Html, "My-Widget");
        let NodeKind::Element(namespaced_data) = dom.node(namespaced).unwrap().kind() else {
            panic!("expected namespaced element");
        };
        assert_eq!(namespaced_data.local_name, "My-Widget");
    }

    #[test]
    fn mutation_revision_changes_for_tree_attributes_and_character_data() {
        let mut dom = Dom::new();
        let element = dom.create_element("div");
        let text = dom.create_text("before");
        assert_eq!(dom.mutation_revision(), 0);
        dom.append_child(element, text).unwrap();
        let after_tree = dom.mutation_revision();
        dom.set_attribute(element, "id", "app").unwrap();
        let after_attribute = dom.mutation_revision();
        dom.set_character_data(text, "after").unwrap();

        assert!(after_tree > 0);
        assert!(after_attribute > after_tree);
        assert!(dom.mutation_revision() > after_attribute);
    }

    #[test]
    fn mutation_journal_drives_rendering_invalidation_without_being_consumed() {
        let mut dom = Dom::new();
        let element = dom.create_element("div");
        let text = dom.create_text("before");
        let base = dom.revision();
        dom.append_child(element, text).unwrap();
        dom.set_attribute(element, "class", "changed").unwrap();
        dom.set_character_data(text, "after").unwrap();

        let first = dom.mutations_since(base).unwrap();
        let second = dom.mutations_since(base).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.to_revision, dom.revision());
        assert_eq!(first.records.len(), 3);
        assert!(first.impact().affects_style());
        assert!(first.impact().affects_layout());
        assert!(first.impact().affects_paint());
        assert_eq!(first.invalidation_roots().len(), 2);
        assert!(matches!(
            first.records[1].kind,
            MutationKind::Attribute {
                target,
                ref local_name
            } if target == element && local_name == "class"
        ));
    }

    #[test]
    fn bounded_mutation_history_fails_closed_when_a_consumer_falls_behind() {
        let mut dom = Dom::new();
        dom.set_mutation_journal_capacity(1);
        let element = dom.create_element("div");
        let base = dom.revision();
        dom.set_attribute(element, "id", "one").unwrap();
        dom.set_attribute(element, "id", "two").unwrap();

        assert!(matches!(
            dom.mutations_since(base),
            Err(MutationHistoryError::HistoryDiscarded { .. })
        ));
        let latest_base = super::DomRevision(dom.revision().as_u64() - 1);
        assert_eq!(dom.mutations_since(latest_base).unwrap().records.len(), 1);
    }

    #[test]
    fn append_text_coalesces_adjacent_character_tokens() {
        let mut dom = Dom::new();
        let element = dom.create_element("p");
        let first = dom.append_text(element, "hello").unwrap();
        let second = dom.append_text(element, " world").unwrap();
        assert_eq!(first, second);
        assert_eq!(dom.children(element).unwrap(), &[first]);
        assert_eq!(
            dom.node(first).unwrap().kind(),
            &NodeKind::Text("hello world".into())
        );
    }
}
