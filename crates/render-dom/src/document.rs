//! Documents other than a page's own: their content type (DOM §4.4), the node
//! document of every node (DOM §4.4 "node document"), and adoption (DOM §4.2.3
//! "adopt"). A document created by `DOMImplementation` or the `Document`
//! constructor is an ordinary document node in the same arena, with no browsing
//! context, so every node it owns names it as the node document.

use crate::{Dom, DomError, NodeId, NodeKind};

/// The content type of an HTML document, and the one a document has when it is
/// not given another (DOM §4.4 "content type").
pub const HTML_CONTENT_TYPE: &str = "text/html";

impl Dom {
    /// The content type of `document` (DOM §4.4). A document that was not made
    /// by [`Dom::create_document`] is an HTML document.
    #[must_use]
    pub fn content_type(&self, document: NodeId) -> &str {
        self.document_content_types
            .get(&document)
            .map_or(HTML_CONTENT_TYPE, String::as_str)
    }

    /// Whether `document` is an HTML document (DOM §4.4): its content type is
    /// `text/html`. An HTML document lowercases the names it creates and puts
    /// them in the HTML namespace; any other document keeps them as given.
    #[must_use]
    pub fn is_html_document(&self, document: NodeId) -> bool {
        self.content_type(document) == HTML_CONTENT_TYPE
    }

    /// Creates a new document node with no children, of the given content type
    /// (DOM §4.5 "create a new document"). The document is its own node document.
    pub fn create_document(&mut self, content_type: impl Into<String>) -> NodeId {
        let document = self.allocate(NodeKind::Document);
        self.set_node_document(document, document);
        self.document_content_types
            .insert(document, content_type.into());
        document
    }

    /// The node document of `node` (DOM §4.4): the document that created the
    /// node, or the one it was last adopted into. A document is its own node
    /// document.
    #[must_use]
    pub fn node_document(&self, node: NodeId) -> Option<NodeId> {
        self.node(node).map(crate::Node::owner)
    }

    /// Makes `document` the node document of a node that was just created, or
    /// that was never given one. It does not change the node's children.
    pub fn set_node_document(&mut self, node: NodeId, document: NodeId) {
        if let Some(index) = node.index()
            && let Some(entry) = self.nodes.get_mut(index)
        {
            entry.owner = document;
        }
    }

    /// Adopts `node` into `document` (DOM §4.5 `adoptNode`): the node leaves its
    /// parent, and it and its inclusive descendants get `document` as their node
    /// document.
    ///
    /// # Errors
    ///
    /// Returns `NotSupported` for a document, which cannot be adopted, and
    /// `HierarchyRequest` for a shadow root, which belongs to its host.
    pub fn adopt_node(&mut self, document: NodeId, node: NodeId) -> Result<(), DomError> {
        match self.kind(node)? {
            NodeKind::Document => {
                return Err(DomError::not_supported("a Document cannot be adopted"));
            }
            NodeKind::ShadowRoot(_) => {
                return Err(DomError::hierarchy("a shadow root cannot be adopted"));
            }
            _ => {}
        }
        self.require_node(document)?;
        if let Some(parent) = self.parent(node) {
            self.remove_child(parent, node)?;
        }
        self.adopt_subtree(node, document);
        Ok(())
    }

    /// Sets `document` as the node document of `root` and of every descendant,
    /// including the shadow trees of the elements in it (DOM §4.5 "adopt" step
    /// 2). A template's contents belong to their own inert document and are not
    /// visited.
    pub(crate) fn adopt_subtree(&mut self, root: NodeId, document: NodeId) {
        let mut pending = vec![root];
        while let Some(node) = pending.pop() {
            self.set_node_document(node, document);
            pending.extend(self.children(node).unwrap_or_default().iter().copied());
            if let Some(NodeKind::Element(data)) = self.node(node).map(crate::Node::kind)
                && let Some(shadow_root) = data.shadow_root
            {
                pending.push(shadow_root);
            }
        }
    }

    /// The node document of `node`, or the document itself when `node` is not in
    /// the arena. Used where the owner is needed for a tree mutation.
    pub(crate) fn owner_of(&self, node: NodeId) -> NodeId {
        self.node_document(node).unwrap_or(self.document)
    }
}

#[cfg(test)]
mod tests {
    use crate::{Dom, DomErrorKind, Namespace, Node, NodeKind, ShadowRootMode, SlotAssignmentMode};

    #[test]
    fn a_created_document_is_typed_and_owns_its_nodes() {
        let mut dom = Dom::new();
        let page = dom.document();
        assert!(dom.is_html_document(page));
        assert_eq!(dom.content_type(page), "text/html");
        let xml = dom.create_document("application/xml");
        assert!(!dom.is_html_document(xml));
        assert_eq!(dom.node_document(xml), Some(xml));
        let element = dom.create_element_qualified(Namespace::Svg, None, "svg");
        dom.set_node_document(element, xml);
        assert_eq!(dom.node_document(element), Some(xml));
        assert_eq!(dom.node_document(page), Some(page));
    }

    #[test]
    fn appending_a_node_adopts_its_whole_subtree() {
        let mut dom = Dom::new();
        let page = dom.document();
        let other = dom.create_document("text/html");
        let outer = dom.create_element("div");
        let inner = dom.create_element("span");
        dom.append_child(outer, inner).expect("append");
        assert_eq!(dom.node_document(inner), Some(page));
        // Adopting the root takes the whole subtree with it.
        dom.adopt_node(other, outer).expect("adopt");
        assert_eq!(dom.node_document(outer), Some(other));
        assert_eq!(dom.node_document(inner), Some(other));
        // A node appended into the subtree is adopted as well.
        let detached = dom.create_element("p");
        assert_eq!(dom.node_document(detached), Some(page));
        dom.append_child(outer, detached).expect("append");
        assert_eq!(dom.node_document(detached), Some(other));
        // A node appended into a document's tree from outside takes that document.
        let moved = dom.create_text("t");
        dom.append_child(inner, moved).expect("append");
        assert_eq!(dom.node_document(moved), Some(other));
    }

    #[test]
    fn adopt_node_leaves_the_parent_and_refuses_documents_and_shadow_roots() {
        let mut dom = Dom::new();
        let page = dom.document();
        let document = dom.create_document("application/xml");
        let host = dom.create_element("div");
        let child = dom.create_element("b");
        dom.append_child(page, host).expect("append");
        dom.append_child(host, child).expect("append");
        dom.adopt_node(document, child).expect("adopt");
        assert_eq!(dom.parent(child), None);
        assert_eq!(dom.node_document(child), Some(document));
        let error = dom.adopt_node(document, page).expect_err("a document");
        assert_eq!(error.kind(), DomErrorKind::NotSupported);
        let shadow = dom
            .attach_shadow(host, ShadowRootMode::Open, false, SlotAssignmentMode::Named)
            .expect("attach");
        assert!(matches!(
            dom.node(shadow).map(Node::kind),
            Some(NodeKind::ShadowRoot(_))
        ));
        assert_eq!(dom.node_document(shadow), Some(page));
        let error = dom.adopt_node(document, shadow).expect_err("a shadow root");
        assert_eq!(error.kind(), DomErrorKind::HierarchyRequest);
    }

    #[test]
    fn cloning_a_document_copies_its_content_type_and_children() {
        let mut dom = Dom::new();
        let source = dom.create_document("application/xml");
        let doctype = dom.create_document_type("html", "", "");
        dom.append_child(source, doctype).expect("append");
        let copy = dom.clone_node(source, true).expect("clone");
        assert_ne!(copy, source);
        assert_eq!(dom.content_type(copy), "application/xml");
        assert_eq!(dom.node_document(copy), Some(copy));
        let children = dom.children(copy).expect("children").to_vec();
        assert_eq!(children.len(), 1);
        assert_eq!(dom.node_document(children[0]), Some(copy));
    }
}
