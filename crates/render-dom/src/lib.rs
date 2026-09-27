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

    #[must_use]
    pub const fn from_u64(value: u64) -> Self {
        Self(value)
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
    /// The element's template contents, for an HTML `template` element and
    /// nothing else: the `DocumentFragment` reachable as `template.content`.
    ///
    /// The fragment is deliberately **not** a child of the element. It is a
    /// separate node with no parent, so the contents of a template are not
    /// connected to the document, are not in document order, and are invisible
    /// to child traversal, `getElementById`, and selector matching over the
    /// document. See [`Dom::template_contents`].
    pub template_contents: Option<NodeId>,
    /// The form owner the HTML parser associated with this element through the
    /// form element pointer, and the parent it was created for. `None` for every
    /// element the parser did not associate that way, which is every element
    /// whose owner is decided by [`Dom::form_owner`]'s derived rules.
    ///
    /// This is deliberately not the form owner itself. Storing the owner would
    /// mean every rule that can change it has to be re-run on every tree
    /// mutation; deriving it on read cannot go stale, and this one record is the
    /// only thing that is not derivable. See [`ParserInsertedFormOwner`].
    pub parser_inserted_form_owner: Option<ParserInsertedFormOwner>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentTypeData {
    pub name: String,
    pub public_id: String,
    pub system_id: String,
}
/// The form owner the HTML parser associated with a form-associated element
/// through the form element pointer (13.2.4.4), together with the parent the
/// parser was going to insert the element into.
///
/// This is the one part of the form owner that cannot be derived from the
/// finished tree, because the form the parser points at need not be an ancestor
/// of the control: the pointer exists so that form controls associate with forms
/// "in the face of dramatically bad markup" (13.2.4.4). Everything else about
/// the owner is derived on read by [`Dom::form_owner`], so this record is the
/// only state the form owner needs, and it is invalidated by reading it: it
/// applies only while the element is still the child it was created for, which
/// is exactly when the parser's association is still the current one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParserInsertedFormOwner {
    /// The element the parser's form element pointer was set to.
    pub form: NodeId,
    /// The intended parent from "the appropriate place for inserting a node",
    /// which is the element's parent once it has been inserted.
    pub intended_parent: NodeId,
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
        let local_name = local_name.into().to_ascii_lowercase();
        let data = self.new_element_data(Namespace::Html, local_name);
        self.allocate(NodeKind::Element(data))
    }

    pub fn create_element_ns(
        &mut self,
        namespace: Namespace,
        local_name: impl Into<String>,
    ) -> NodeId {
        let data = self.new_element_data(namespace, local_name.into());
        self.allocate(NodeKind::Element(data))
    }

    /// The `DocumentFragment` reachable as `element.content` for an HTML
    /// `template` element, and `None` for every other node.
    ///
    /// The fragment is not a child of the element, so its contents are inert:
    /// they are not connected to the document, not in document order, and not
    /// reachable by document traversal, `getElementById`, or selector matching.
    /// Only code that asks for the contents by element sees them.
    #[must_use]
    pub fn template_contents(&self, element: NodeId) -> Option<NodeId> {
        match self.node(element).map(Node::kind) {
            Some(NodeKind::Element(data)) => data.template_contents,
            _ => None,
        }
    }

    /// Whether the node is a form-associated element: an HTML `button`,
    /// `fieldset`, `input`, `object`, `output`, `select`, `textarea`, or `img`
    /// (4.10.2).
    ///
    /// `form` is deliberately absent, as are the elements that are only
    /// form-associated when they are form-associated custom elements: this DOM
    /// has no custom element registry, so a custom element is an element with
    /// whatever local name it was created with and is never form-associated.
    /// An SVG `input` is an SVG `input`, not an HTML one, so the namespace is
    /// part of the test.
    #[must_use]
    pub fn is_form_associated(&self, node: NodeId) -> bool {
        matches!(self.element_data(node), Some((Namespace::Html, name))
        if matches!(
            name,
            "button"
                | "fieldset"
                | "input"
                | "object"
                | "output"
                | "select"
                | "textarea"
                | "img"
        ))
    }

    /// Whether the node is a listed element: a form-associated element that
    /// appears in `form.elements` and `fieldset.elements`, and that therefore
    /// has a `form` content attribute and a `form` IDL attribute that name an
    /// explicit form owner (4.10.2).
    ///
    /// This is [`Self::is_form_associated`] minus `img`: an `img` is
    /// form-associated, so it has a form owner and takes part in submission, but
    /// it is not listed, so the `form` content attribute does not apply to it and
    /// it is not in `form.elements`.
    #[must_use]
    pub fn is_listed_element(&self, node: NodeId) -> bool {
        self.is_form_associated(node)
            && !matches!(self.element_data(node), Some((Namespace::Html, "img")))
    }

    /// Whether the node is a submittable element: one whose value can end up in
    /// a form's entry list (4.10.2). `fieldset` and `object` are
    /// form-associated and listed but not submittable, and `img` is neither.
    #[must_use]
    pub fn is_submittable_element(&self, node: NodeId) -> bool {
        matches!(self.element_data(node), Some((Namespace::Html, name))
            if matches!(name, "button" | "input" | "select" | "textarea"))
    }

    /// Whether the node is an HTML `form` element.
    #[must_use]
    pub fn is_form_element(&self, node: NodeId) -> bool {
        matches!(self.element_data(node), Some((Namespace::Html, "form")))
    }

    /// The form owner of a form-associated element, or `None` for an element
    /// that is not form-associated and for a form-associated element with no
    /// form (4.10.18.3).
    ///
    /// This is the whole of the form owner, and it is **derived on read** from
    /// the tree and the attributes, so it cannot be stale: there is no owner to
    /// keep in step with insertions, removals, moves, and attribute changes.
    /// The three steps are the spec's, in order:
    ///
    /// 1. If the HTML parser associated the element with a form through the form
    ///    element pointer and that association still holds, that is the owner.
    ///    This is the one case the finished tree cannot express, because the form
    ///    need not be an ancestor of the element; see
    ///    [`Self::set_parser_inserted_form_owner`].
    /// 2. Otherwise, if the element is listed, has a `form` content attribute,
    ///    and is connected: if the first element in the element's own tree, in
    ///    tree order, whose ID is that attribute's value is a `form` element, it
    ///    is the owner — and if nothing has that ID, or the element with that ID
    ///    is not a `form`, the owner is `None`.
    ///
    ///    This step is an `if`/`else` with the next one, not a fallback: a `form`
    ///    attribute that names nothing is **not** quietly ignored in favour of an
    ///    ancestor. A control written `<input form="typo">` inside a form has no
    ///    form owner, which is what tells an author the attribute is wrong.
    ///
    /// 3. Otherwise, the nearest ancestor `form` element, and `None` if there is
    ///    none.
    ///
    /// Step 3 is what makes removal correct without a hook: when a `form`
    /// ancestor is removed, the control is no longer inside it, so the walk from
    /// the control simply does not reach it and the owner is `None`. It does not
    /// keep pointing at the detached form, and it does not keep searching past
    /// the nearest form.
    #[must_use]
    pub fn form_owner(&self, node: NodeId) -> Option<NodeId> {
        if !self.is_form_associated(node) {
            return None;
        }
        if let Some(link) = self.parser_inserted_form_owner(node)
            && self.parent(node) == Some(link.intended_parent)
            && self.node(link.form).is_some_and(|form| {
                matches!(form.kind(), NodeKind::Element(_)) && self.is_in_same_tree(node, link.form)
            })
        {
            return Some(link.form);
        }
        if self.is_listed_element(node)
            && self.is_connected(node)
            && let Some(id) = self.attribute(node, "form").ok().flatten()
        {
            return self
                .first_element_in_tree_with_id(node, id)
                .filter(|found| self.is_form_element(*found));
        }
        self.nearest_ancestor_form(node)
    }

    /// Record that the HTML parser associated `element` with `form` through the
    /// form element pointer, with `intended_parent` as the parent it was
    /// inserted into (13.2.6.1).
    ///
    /// Returns `false` and records nothing when `element` is not a
    /// form-associated element or `form` is not an element, because the spec
    /// only allows the association for a form-associated element. Only the HTML
    /// parser should call this: it is how badly-marked-up form controls end up
    /// associated with a form that is not their ancestor, and every other owner
    /// is derived.
    pub fn set_parser_inserted_form_owner(
        &mut self,
        element: NodeId,
        form: NodeId,
        intended_parent: NodeId,
    ) -> bool {
        if !self.is_form_associated(element) || self.node(form).is_none() {
            return false;
        }
        let Some(data) = self.element_data_mut(element) else {
            return false;
        };
        data.parser_inserted_form_owner = Some(ParserInsertedFormOwner {
            form,
            intended_parent,
        });
        true
    }

    /// Forget any parser-inserted form owner on `root` and everything below it,
    /// leaving the derived rules in charge. The HTML parser does not need this,
    /// because it creates each element once; it exists so that code which
    /// reparents a subtree wholesale can drop the associations in it in one call
    /// instead of relying on [`Self::form_owner`] noticing.
    pub fn clear_parser_inserted_form_owners(&mut self, root: NodeId) {
        if let Some(data) = self.element_data_mut(root) {
            data.parser_inserted_form_owner = None;
        }
        for child in self.children(root).unwrap_or_default().to_vec() {
            self.clear_parser_inserted_form_owners(child);
        }
    }

    /// The form owner the HTML parser associated with the node, if any, with no
    /// check of whether the association still holds. Prefer [`Self::form_owner`].
    #[must_use]
    pub fn parser_inserted_form_owner(&self, node: NodeId) -> Option<ParserInsertedFormOwner> {
        match self.node(node).map(Node::kind) {
            Some(NodeKind::Element(data)) => data.parser_inserted_form_owner,
            _ => None,
        }
    }

    /// The `elements` of a `form` element: every listed element whose form owner
    /// is this form element, in tree order, excluding `input` elements whose
    /// `type` attribute is in the Image Button state, which the standard
    /// excludes from this particular collection "for historical reasons" (4.10.3).
    ///
    /// The collection is rooted at the **form element's root**, not at the form
    /// element, so a control elsewhere in the tree that names this form with a
    /// `form` content attribute is in the list. `img` is not a listed element, so
    /// it is not here even though it has this form owner and is submitted with
    /// it.
    #[must_use]
    pub fn form_owner_elements(&self, form: NodeId) -> Vec<NodeId> {
        if !self.is_form_element(form) {
            return Vec::new();
        }
        let root = self.tree_root(form);
        self.listed_elements_within(root)
            .into_iter()
            .filter(|element| self.form_owner(*element) == Some(form))
            .filter(|element| !self.is_image_button(*element))
            .collect()
    }

    /// Every listed element at or below `root`, in tree order. This is the
    /// `fieldset.elements` shape: "an HTMLCollection rooted at the fieldset
    /// element, whose filter matches listed elements" (4.10.4).
    ///
    /// Note that this is a descendant filter, not a form-owner filter. The
    /// standard roots the collection at the fieldset and filters only on
    /// listed-ness, so it is the root that scopes it, and a listed descendant
    /// whose form owner is some other form is still in it.
    ///
    /// The current standard has no rule that the descendants of a *disabled*
    /// fieldset stop being listed; that rule was removed along with the old
    /// "listed element" definition. Disabling a fieldset therefore does not
    /// change this list — it changes which elements are barred from constraint
    /// validation and submission, which is not modelled here.
    #[must_use]
    pub fn listed_elements_within(&self, root: NodeId) -> Vec<NodeId> {
        let mut found = Vec::new();
        self.collect_listed_elements_within(root, &mut found);
        found
    }

    /// Whether two nodes are in the same tree: both connected, or both
    /// disconnected. This is the DOM's reading of "in the same tree" (13.2.6.1),
    /// which is what the parser's form-owner association is conditioned on.
    #[must_use]
    pub fn is_in_same_tree(&self, left: NodeId, right: NodeId) -> bool {
        self.is_connected(left) == self.is_connected(right)
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
    ///
    /// # Errors
    ///
    /// Returns an error when `element` does not refer to a valid node or does
    /// not resolve to an element.
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

    /// Set a namespaced attribute, as `Element.setAttributeNS` does.
    ///
    /// An attribute is identified by the `(namespace, local name)` pair only;
    /// the prefix is part of the attribute's qualified name and is replaced,
    /// never used to find an existing attribute. A `None` namespace is the null
    /// namespace, which is what plain HTML attributes use. Unlike
    /// [`Self::set_attribute`], `local_name` is never case-folded: only the HTML
    /// tokenizer lower-cases names, and foreign content carries case-sensitive
    /// names such as `viewBox`.
    ///
    /// # Errors
    ///
    /// Returns [`DomErrorKind::NotFound`] for an unknown node and
    /// [`DomErrorKind::InvalidNodeType`] when the node is not an element.
    pub fn set_attribute_ns(
        &mut self,
        element: NodeId,
        namespace: Option<&str>,
        prefix: Option<&str>,
        local_name: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<(), DomError> {
        if !matches!(self.kind(element)?, NodeKind::Element(_)) {
            return Err(DomError::invalid_node_type("attributes require an element"));
        }
        let local_name = local_name.into();
        let value = value.into();
        let mutation_name = local_name.clone();
        let namespace = namespace.map(str::to_owned);
        let prefix = prefix.map(str::to_owned);
        let node = self.node_mut(element)?;
        let NodeKind::Element(data) = &mut node.kind else {
            unreachable!("element kind was checked")
        };
        if let Some(attribute) = data.attributes.iter_mut().find(|attribute| {
            attribute.namespace == namespace && attribute.local_name == local_name
        }) {
            attribute.value = value;
            attribute.prefix = prefix;
        } else {
            data.attributes.push(Attribute {
                namespace,
                prefix,
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

    /// Return a namespaced attribute value, as `Element.getAttributeNS` does.
    ///
    /// # Errors
    ///
    /// Returns [`DomErrorKind::NotFound`] for an unknown node and
    /// [`DomErrorKind::InvalidNodeType`] when the node is not an element.
    pub fn attribute_ns(
        &self,
        element: NodeId,
        namespace: Option<&str>,
        local_name: &str,
    ) -> Result<Option<&str>, DomError> {
        let NodeKind::Element(data) = self.kind(element)? else {
            return Err(DomError::invalid_node_type("attributes require an element"));
        };
        Ok(data
            .attributes
            .iter()
            .find(|attribute| {
                attribute.namespace.as_deref() == namespace && attribute.local_name == local_name
            })
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

    /// Create the data for a new element, following the DOM's "creating an
    /// element" algorithm: "If localName is 'template' and namespace is the
    /// HTML namespace, then set element's template contents to a new
    /// DocumentFragment owned by element's node document."
    ///
    /// The fragment is allocated before the element, so the element is the node
    /// that a caller ends up holding. Identifiers are only ever compared, never
    /// used to imply tree order.
    fn new_element_data(&mut self, namespace: Namespace, local_name: String) -> ElementData {
        let template_contents = (namespace == Namespace::Html && local_name == "template")
            .then(|| self.create_document_fragment());
        ElementData {
            namespace,
            local_name,
            attributes: Vec::new(),
            template_contents,
            parser_inserted_form_owner: None,
        }
    }

    /// `(namespace, local name)` for an element, and `None` for every other
    /// node kind, so the category tests can match on the pair.
    fn element_data(&self, node: NodeId) -> Option<(&Namespace, &str)> {
        match self.node(node).map(Node::kind) {
            Some(NodeKind::Element(data)) => Some((&data.namespace, data.local_name.as_str())),
            _ => None,
        }
    }

    /// A mutable handle on an element's data, and `None` for every other node
    /// kind.
    fn element_data_mut(&mut self, node: NodeId) -> Option<&mut ElementData> {
        match &mut self.node_mut(node).ok()?.kind {
            NodeKind::Element(data) => Some(data),
            _ => None,
        }
    }

    /// The topmost ancestor of `node`, which is `node` itself if it has no
    /// parent. For a node in a document this is the document.
    fn tree_root(&self, node: NodeId) -> NodeId {
        let mut current = node;
        while let Some(parent) = self.parent(current) {
            current = parent;
        }
        current
    }

    /// The nearest ancestor `form` element of `node`, not including `node`
    /// itself. The walk stops at the first form it reaches, so it never looks
    /// past a nearer form to a further one.
    fn nearest_ancestor_form(&self, node: NodeId) -> Option<NodeId> {
        let mut current = self.parent(node);
        while let Some(candidate) = current {
            if self.is_form_element(candidate) {
                return Some(candidate);
            }
            current = self.parent(candidate);
        }
        None
    }

    /// "The first element in `node`'s tree, in tree order, to have an ID that is
    /// identical to `id`" (4.10.18.3). The search starts at the tree's root, so
    /// it can reach a form that is not an ancestor of `node`.
    fn first_element_in_tree_with_id(&self, node: NodeId, id: &str) -> Option<NodeId> {
        let root = self.tree_root(node);
        let mut stack = vec![root];
        while let Some(candidate) = stack.pop() {
            if self
                .node(candidate)
                .is_some_and(|found| matches!(found.kind(), NodeKind::Element(_)))
                && self.attribute(candidate, "id").ok().flatten() == Some(id)
            {
                return Some(candidate);
            }
            // Reversed, so that the children come off the stack in tree order.
            for child in self.children(candidate).unwrap_or_default().iter().rev() {
                stack.push(*child);
            }
        }
        None
    }

    fn is_image_button(&self, node: NodeId) -> bool {
        matches!(self.element_data(node), Some((Namespace::Html, "input")))
            && self
                .attribute(node, "type")
                .ok()
                .flatten()
                .is_some_and(|value| value.eq_ignore_ascii_case("image"))
    }

    fn collect_listed_elements_within(&self, node: NodeId, found: &mut Vec<NodeId>) {
        if self.is_listed_element(node) {
            found.push(node);
        }
        for child in self.children(node).unwrap_or_default() {
            self.collect_listed_elements_within(*child, found);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Dom, DomErrorKind, MutationHistoryError, MutationKind, Namespace, NodeId, NodeKind,
    };

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
    fn namespaced_attributes_are_keyed_by_namespace_and_local_name() {
        let mut dom = Dom::new();
        let element = dom.create_element_ns(Namespace::Svg, "use");
        dom.set_attribute_ns(element, None, None, "d", "M0 0")
            .unwrap();
        dom.set_attribute_ns(
            element,
            Some("http://www.w3.org/1999/xlink"),
            Some("xlink"),
            "href",
            "#a",
        )
        .unwrap();
        dom.set_attribute_ns(
            element,
            Some("http://www.w3.org/1999/xlink"),
            Some("xlink"),
            "href",
            "#b",
        )
        .unwrap();
        dom.set_attribute_ns(
            element,
            Some("http://www.w3.org/2000/xmlns/"),
            Some("xmlns"),
            "xlink",
            "http://www.w3.org/1999/xlink",
        )
        .unwrap();

        // The null-namespace and the XLink attribute are distinct even though
        // their local names differ only by the prefix in the source markup.
        assert_eq!(dom.attribute(element, "d").unwrap(), Some("M0 0"));
        assert_eq!(dom.attribute(element, "href").unwrap(), None);
        assert_eq!(dom.attribute(element, "xlink:href").unwrap(), None);
        assert_eq!(
            dom.attribute_ns(element, Some("http://www.w3.org/1999/xlink"), "href")
                .unwrap(),
            Some("#b")
        );
        assert_eq!(dom.attribute_ns(element, None, "href").unwrap(), None);
        assert_eq!(
            dom.attribute_ns(element, Some("http://www.w3.org/2000/xmlns/"), "xlink")
                .unwrap(),
            Some("http://www.w3.org/1999/xlink")
        );

        let NodeKind::Element(data) = dom.node(element).unwrap().kind() else {
            panic!("expected element");
        };
        assert_eq!(data.attributes.len(), 3);
        assert_eq!(data.attributes[1].prefix.as_deref(), Some("xlink"));
        assert_eq!(data.attributes[1].local_name, "href");
    }

    #[test]
    fn namespaced_attribute_local_names_keep_their_case() {
        let mut dom = Dom::new();
        let element = dom.create_element_ns(Namespace::Svg, "svg");
        dom.set_attribute_ns(element, None, None, "viewBox", "0 0 1 1")
            .unwrap();
        assert_eq!(
            dom.attribute_ns(element, None, "viewBox").unwrap(),
            Some("0 0 1 1")
        );
        assert_eq!(dom.attribute_ns(element, None, "viewbox").unwrap(), None);
    }

    #[test]
    fn a_template_element_owns_a_detached_contents_fragment() {
        let mut dom = Dom::new();
        let body = dom.create_element("body");
        dom.append_child(dom.document(), body).unwrap();
        let template = dom.create_element("template");
        dom.append_child(body, template).unwrap();
        let contents = dom.template_contents(template).expect("template contents");
        assert!(matches!(
            dom.node(contents).unwrap().kind(),
            NodeKind::DocumentFragment
        ));

        // The contents are not a child of the element: `children` of the
        // template is empty, which is what keeps the contents out of document
        // order and out of every document traversal.
        assert!(dom.children(template).unwrap().is_empty());
        let child = dom.create_element("tr");
        dom.append_child(contents, child).unwrap();
        assert_eq!(dom.children(contents).unwrap(), &[child]);
        assert!(dom.children(template).unwrap().is_empty());
        assert_eq!(dom.parent(child), Some(contents));

        // The fragment is not connected to the document, so nothing inside a
        // template is reachable from the document root.
        assert!(!dom.is_connected(contents));
        assert!(!dom.is_connected(child));
        assert!(dom.is_connected(template));
    }

    /// Build `outer > inner > control` by hand and return
    /// `(outer, inner, control)`. Nested `form` elements cannot come out of a
    /// conforming parser — the parser ignores a second `form` start tag — so the
    /// nesting rules have to be exercised through the DOM.
    fn nested_forms() -> (Dom, NodeId, NodeId, NodeId) {
        let mut dom = Dom::new();
        let body = dom.create_element("body");
        dom.append_child(dom.document(), body).unwrap();
        let outer = dom.create_element("form");
        dom.set_attribute(outer, "id", "outer").unwrap();
        dom.append_child(body, outer).unwrap();
        let inner = dom.create_element("form");
        dom.set_attribute(inner, "id", "inner").unwrap();
        dom.append_child(outer, inner).unwrap();
        let control = dom.create_element("input");
        dom.set_attribute(control, "name", "a").unwrap();
        dom.append_child(inner, control).unwrap();
        (dom, outer, inner, control)
    }

    #[test]
    fn form_owner_is_the_nearest_ancestor_form_and_the_walk_stops_there() {
        let (mut dom, outer, inner, control) = nested_forms();
        // The inner form wins even though the outer form is also an ancestor: the
        // owner is the *nearest* form, and the walk never looks past it.
        assert_eq!(dom.form_owner(control), Some(inner));
        assert_ne!(dom.form_owner(control), Some(outer));
        // A control with no form ancestor at all has no owner, rather than
        // reaching one.
        let body = dom.children(dom.document()).unwrap()[0];
        let stray = dom.create_element("input");
        dom.append_child(body, stray).unwrap();
        assert_eq!(dom.form_owner(stray), None);
    }

    #[test]
    fn removing_a_form_ancestor_resets_the_owner_to_null() {
        let (mut dom, outer, _inner, control) = nested_forms();
        assert!(dom.form_owner(control).is_some());
        // Take the inner form, and the control inside it, out of the outer form.
        dom.remove_child(outer, _inner).unwrap();
        // The control is still inside the form it was associated with, and that
        // form is still the nearest ancestor form, so the owner is unchanged:
        // the form was detached, not removed from the control.
        assert_eq!(dom.form_owner(control), Some(_inner));
        // Now take just the control out of the form. The form is no longer an
        // ancestor, so the owner resets to null. It does not keep pointing at the
        // detached form.
        dom.remove_child(_inner, control).unwrap();
        assert_eq!(dom.form_owner(control), None);
        // And it does not fall back to a form that used to be further up: the
        // outer form is still an ancestor of the inner form, but it is no longer
        // an ancestor of the control.
        assert!(dom.is_connected(outer));
    }

    #[test]
    fn moving_a_control_re_resolves_its_owner_rather_than_keeping_it() {
        let (mut dom, outer, inner, control) = nested_forms();
        assert_eq!(dom.form_owner(control), Some(inner));
        // Move the control up one form. The nearest ancestor form is now the outer
        // one, so that is the owner: re-resolved, not remembered.
        dom.append_child(outer, control).unwrap();
        assert_eq!(dom.form_owner(control), Some(outer));
    }

    #[test]
    fn a_form_content_attribute_overrides_the_ancestor_and_survives_being_outside_it() {
        let mut dom = Dom::new();
        let body = dom.create_element("body");
        dom.append_child(dom.document(), body).unwrap();
        let form = dom.create_element("form");
        dom.set_attribute(form, "id", "f").unwrap();
        dom.append_child(body, form).unwrap();
        // The control is outside the form entirely, which is the whole point of
        // the attribute: it works around the lack of nested `form` support.
        let control = dom.create_element("input");
        dom.set_attribute(control, "form", "f").unwrap();
        dom.append_child(body, control).unwrap();
        assert_eq!(dom.form_owner(control), Some(form));

        // It also wins over an ancestor, and it is found by ID anywhere in the
        // element's own tree, not only among ancestors.
        let wrapper = dom.create_element("div");
        dom.append_child(body, wrapper).unwrap();
        dom.append_child(wrapper, control).unwrap();
        assert_eq!(dom.form_owner(control), Some(form));
    }

    #[test]
    fn a_form_content_attribute_that_resolves_to_nothing_leaves_no_owner() {
        // A `form` attribute naming a missing ID, and one naming an element that
        // is not a `form`, both leave the control with no owner. Neither falls
        // back to the enclosing form: the attribute was taken, so the answer is
        // already determined, and a control with no owner is how an author finds
        // out the attribute is wrong.
        for attribute in ["typo", "not-a-form"] {
            let mut dom = Dom::new();
            let body = dom.create_element("body");
            dom.append_child(dom.document(), body).unwrap();
            let form = dom.create_element("form");
            dom.set_attribute(form, "id", "f").unwrap();
            dom.append_child(body, form).unwrap();
            if attribute == "not-a-form" {
                let div = dom.create_element("div");
                dom.set_attribute(div, "id", "not-a-form").unwrap();
                dom.append_child(body, div).unwrap();
            }
            let control = dom.create_element("input");
            dom.set_attribute(control, "form", attribute).unwrap();
            dom.append_child(form, control).unwrap();
            // The control *is* inside the form, and still has no owner.
            assert_eq!(dom.parent(control), Some(form));
            assert_eq!(dom.form_owner(control), None, "form={attribute}");
        }
    }

    #[test]
    fn img_is_form_associated_but_not_listed_so_the_form_attribute_does_not_apply() {
        let mut dom = Dom::new();
        let body = dom.create_element("body");
        dom.append_child(dom.document(), body).unwrap();
        let form = dom.create_element("form");
        dom.set_attribute(form, "id", "f").unwrap();
        dom.append_child(body, form).unwrap();

        // An `img` is form-associated, so it has an owner, and that owner comes
        // from the ancestor rule.
        let inside = dom.create_element("img");
        dom.append_child(form, inside).unwrap();
        assert!(dom.is_form_associated(inside));
        assert!(!dom.is_listed_element(inside));
        assert_eq!(dom.form_owner(inside), Some(form));

        // It is not listed, so the `form` content attribute is ignored for it:
        // outside the form with `form="f"`, it still has no owner.
        let outside = dom.create_element("img");
        dom.set_attribute(outside, "form", "f").unwrap();
        dom.append_child(body, outside).unwrap();
        assert!(dom.is_form_associated(outside));
        assert_eq!(dom.form_owner(outside), None);
    }

    #[test]
    fn form_association_is_by_html_namespace_and_name() {
        let mut dom = Dom::new();
        let body = dom.create_element("body");
        dom.append_child(dom.document(), body).unwrap();
        let form = dom.create_element("form");
        dom.append_child(body, form).unwrap();

        // Every form-associated element in the standard.
        for name in [
            "button", "fieldset", "input", "object", "output", "select", "textarea", "img",
        ] {
            let element = dom.create_element(name);
            dom.append_child(form, element).unwrap();
            assert!(dom.is_form_associated(element), "{name} is form-associated");
            assert_eq!(dom.form_owner(element), Some(form), "{name} owner");
        }
        // `form` itself is not form-associated: a form has no form owner.
        assert!(!dom.is_form_associated(form));
        assert_eq!(dom.form_owner(form), None);
        // Neither is a non-form element that looks like one.
        for name in ["div", "label", "form-associated-custom-element"] {
            let element = dom.create_element(name);
            dom.append_child(form, element).unwrap();
            assert!(
                !dom.is_form_associated(element),
                "{name} is not form-associated"
            );
            assert_eq!(dom.form_owner(element), None, "{name} owner");
        }
        // An `input` in the SVG namespace is an SVG element, not an HTML one, so
        // the category does not apply to it.
        let svg = dom.create_element_ns(Namespace::Svg, "svg");
        dom.append_child(body, svg).unwrap();
        let svg_input = dom.create_element_ns(Namespace::Svg, "input");
        dom.append_child(svg, svg_input).unwrap();
        assert!(!dom.is_form_associated(svg_input));
        assert_eq!(dom.form_owner(svg_input), None);
    }

    #[test]
    fn listed_and_submittable_differ_from_form_associated() {
        let mut dom = Dom::new();
        for (name, listed, submittable) in [
            ("button", true, true),
            ("fieldset", true, false),
            ("input", true, true),
            ("object", true, false),
            ("output", true, false),
            ("select", true, true),
            ("textarea", true, true),
            // Form-associated, but not listed and not submittable.
            ("img", false, false),
        ] {
            let element = dom.create_element(name);
            assert!(dom.is_form_associated(element), "{name}");
            assert_eq!(dom.is_listed_element(element), listed, "{name} listed");
            assert_eq!(
                dom.is_submittable_element(element),
                submittable,
                "{name} submittable"
            );
        }
    }

    #[test]
    fn a_form_content_attribute_does_not_apply_to_a_disconnected_control() {
        // The `form` attribute needs the control to be connected. A control in a
        // template's contents is not, so the attribute is ignored there and the
        // ancestor rule applies instead — which is what makes a template's
        // `form=` attribute take effect only once the contents are cloned into
        // the document.
        let mut dom = Dom::new();
        let body = dom.create_element("body");
        dom.append_child(dom.document(), body).unwrap();
        let form = dom.create_element("form");
        dom.set_attribute(form, "id", "f").unwrap();
        dom.append_child(body, form).unwrap();

        let template = dom.create_element("template");
        dom.append_child(body, template).unwrap();
        let contents = dom.template_contents(template).unwrap();

        // An ancestor form inside the same fragment still counts.
        let local_form = dom.create_element("form");
        dom.append_child(contents, local_form).unwrap();
        let inner = dom.create_element("input");
        dom.append_child(local_form, inner).unwrap();
        assert!(!dom.is_connected(inner));
        assert_eq!(dom.form_owner(inner), Some(local_form));

        // A `form` attribute naming a form in the *document* does not, because
        // the control is not connected.
        let detached = dom.create_element("input");
        dom.set_attribute(detached, "form", "f").unwrap();
        dom.append_child(contents, detached).unwrap();
        assert_eq!(dom.form_owner(detached), None);
    }

    #[test]
    fn a_parser_inserted_owner_is_ignored_once_the_element_moves() {
        // The one part of the owner that cannot be derived: the HTML parser can
        // associate a control with a form that is not its ancestor. It is
        // recorded, and it applies only while the control is still the child it
        // was created for.
        let mut dom = Dom::new();
        let body = dom.create_element("body");
        dom.append_child(dom.document(), body).unwrap();
        let form = dom.create_element("form");
        dom.append_child(body, form).unwrap();
        let cell = dom.create_element("td");
        dom.append_child(body, cell).unwrap();
        let control = dom.create_element("input");
        dom.append_child(cell, control).unwrap();

        // Not an ancestor, so the derived rules alone would find nothing.
        assert_eq!(dom.form_owner(control), None);
        assert!(dom.set_parser_inserted_form_owner(control, form, cell));
        assert_eq!(dom.form_owner(control), Some(form));

        // Move the control: the recorded parent no longer matches, so the
        // association stops applying and the derived rules answer instead.
        dom.append_child(form, control).unwrap();
        assert_eq!(dom.form_owner(control), Some(form));
        let elsewhere = dom.create_element("div");
        dom.append_child(body, elsewhere).unwrap();
        dom.append_child(elsewhere, control).unwrap();
        assert_eq!(dom.form_owner(control), None);
    }

    #[test]
    fn a_parser_inserted_owner_is_refused_for_a_non_form_associated_element() {
        let mut dom = Dom::new();
        let body = dom.create_element("body");
        dom.append_child(dom.document(), body).unwrap();
        let form = dom.create_element("form");
        dom.append_child(body, form).unwrap();
        let div = dom.create_element("div");
        dom.append_child(body, div).unwrap();
        assert!(!dom.set_parser_inserted_form_owner(div, form, body));
        assert_eq!(dom.parser_inserted_form_owner(div), None);
        // And clearing one is possible, so a caller that reparents a subtree can
        // drop every association in it at once.
        let control = dom.create_element("input");
        dom.append_child(div, control).unwrap();
        assert!(dom.set_parser_inserted_form_owner(control, form, div));
        assert!(dom.parser_inserted_form_owner(control).is_some());
        dom.clear_parser_inserted_form_owners(div);
        assert_eq!(dom.parser_inserted_form_owner(control), None);
        assert_eq!(dom.form_owner(control), None);
    }

    #[test]
    fn form_owner_elements_excludes_images_and_image_buttons() {
        let mut dom = Dom::new();
        let body = dom.create_element("body");
        dom.append_child(dom.document(), body).unwrap();
        let form = dom.create_element("form");
        dom.set_attribute(form, "id", "f").unwrap();
        dom.append_child(body, form).unwrap();
        let fieldset = dom.create_element("fieldset");
        dom.set_attribute(fieldset, "name", "group").unwrap();
        dom.append_child(form, fieldset).unwrap();

        let mut controls = Vec::new();
        for (name, type_attribute) in [
            ("a", None),
            // An image button is excluded from `form.elements` for historical
            // reasons, even though it is a listed element and is submitted.
            ("b", Some("image")),
            ("c", Some("hidden")),
        ] {
            let control = dom.create_element("input");
            dom.set_attribute(control, "name", name).unwrap();
            if let Some(value) = type_attribute {
                dom.set_attribute(control, "type", value).unwrap();
            }
            dom.append_child(fieldset, control).unwrap();
            controls.push(control);
        }
        let image = dom.create_element("img");
        dom.set_attribute(image, "name", "i").unwrap();
        dom.append_child(form, image).unwrap();
        // A control outside the form, naming it with a `form` attribute, is in
        // the collection: it is rooted at the form's *root*, not at the form.
        let outside = dom.create_element("input");
        dom.set_attribute(outside, "name", "d").unwrap();
        dom.set_attribute(outside, "form", "f").unwrap();
        dom.append_child(body, outside).unwrap();
        // A control belonging to a different form is not.
        let other_form = dom.create_element("form");
        dom.set_attribute(other_form, "id", "g").unwrap();
        dom.append_child(body, other_form).unwrap();
        let other_control = dom.create_element("input");
        dom.set_attribute(other_control, "name", "e").unwrap();
        dom.append_child(other_form, other_control).unwrap();

        let members = dom.form_owner_elements(form);
        assert_eq!(
            members,
            vec![fieldset, controls[0], controls[2], outside],
            "the fieldset is itself a listed element, `b` is an image button and `i` is not listed"
        );
        // A non-form node has no elements collection.
        assert!(dom.form_owner_elements(body).is_empty());
    }

    #[test]
    fn listed_elements_within_a_fieldset_is_a_descendant_filter() {
        // `fieldset.elements` is "an HTMLCollection rooted at the fieldset
        // element, whose filter matches listed elements": it is the root that
        // scopes the collection, and the filter is only listed-ness. So the
        // fieldset is in its own collection, an image button is, and an `img` is
        // not.
        let mut dom = Dom::new();
        let body = dom.create_element("body");
        dom.append_child(dom.document(), body).unwrap();
        let form = dom.create_element("form");
        dom.append_child(body, form).unwrap();
        let fieldset = dom.create_element("fieldset");
        dom.append_child(form, fieldset).unwrap();
        let group = dom.create_element("div");
        dom.append_child(fieldset, group).unwrap();
        let mut expected = vec![fieldset];
        for name in ["a", "b"] {
            let control = dom.create_element("input");
            dom.set_attribute(control, "name", name).unwrap();
            dom.append_child(group, control).unwrap();
            expected.push(control);
        }
        let image = dom.create_element("img");
        dom.append_child(fieldset, image).unwrap();
        let image_button = dom.create_element("input");
        dom.set_attribute(image_button, "name", "c").unwrap();
        dom.set_attribute(image_button, "type", "image").unwrap();
        dom.append_child(fieldset, image_button).unwrap();
        expected.push(image_button);

        assert_eq!(dom.listed_elements_within(fieldset), expected);
        // The collection is rooted at the fieldset, so the form is not in it
        // even though the form is a listed element and an ancestor.
        assert!(!dom.listed_elements_within(fieldset).contains(&form));
        // Disabling a fieldset does not change the list: the current standard has
        // no rule that the descendants of a disabled fieldset stop being listed.
        dom.set_attribute(fieldset, "disabled", "").unwrap();
        assert_eq!(dom.listed_elements_within(fieldset), expected);
    }

    #[test]
    fn only_an_html_template_element_has_contents() {
        let mut dom = Dom::new();
        let div = dom.create_element("div");
        let foreign = dom.create_element_ns(Namespace::Svg, "template");
        let text = dom.create_text("t");
        let fragment = dom.create_document_fragment();
        assert!(dom.template_contents(div).is_none());
        assert!(dom.template_contents(foreign).is_none());
        assert!(dom.template_contents(text).is_none());
        assert!(dom.template_contents(fragment).is_none());
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
