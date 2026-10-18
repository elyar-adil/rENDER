//! DOM Standard §4.5 "clone a node": a copy of one node, with its subtree when
//! asked. Range contents (§5.5) are copied with it.

use crate::{Dom, DomError, NodeId, NodeKind};

impl Dom {
    /// Copy `node`, and its descendants when `deep` is true (DOM §4.5 "clone a
    /// node").
    ///
    /// The copy keeps the element's namespace, prefix and attributes exactly, as
    /// the specification requires, and a `template` element's content is copied
    /// into the copy's own template contents. The copy is detached. A Document
    /// copy is a new document with the same content type, and its children are
    /// copied with it when `deep` is true. Every other copy has the node document
    /// of the node it copies.
    ///
    /// # Errors
    ///
    /// Returns [`DomErrorKind::NotFound`](crate::DomErrorKind::NotFound) for an
    /// unknown node, and [`DomErrorKind::NotSupported`](crate::DomErrorKind::NotSupported)
    /// for a shadow root, which `cloneNode()` refuses (DOM §4.5).
    pub fn clone_node(&mut self, node: NodeId, deep: bool) -> Result<NodeId, DomError> {
        let source = self.kind(node)?.clone();
        let is_document = matches!(source, NodeKind::Document);
        let copy = match source {
            NodeKind::Document => {
                let content_type = self.content_type(node).to_owned();
                self.create_document(content_type)
            }
            NodeKind::ShadowRoot(_) => {
                return Err(DomError::not_supported(
                    "a shadow root cannot be cloned (DOM NotSupportedError)",
                ));
            }
            NodeKind::DocumentType(data) => {
                self.create_document_type(data.name, data.public_id, data.system_id)
            }
            NodeKind::DocumentFragment => self.create_document_fragment(),
            NodeKind::Text(data) => self.create_text(data),
            NodeKind::Comment(data) => self.create_comment(data),
            NodeKind::ProcessingInstruction { target, data } => {
                self.create_processing_instruction(target, data)
            }
            NodeKind::Element(data) => {
                let copy = self.create_element_qualified(
                    data.namespace.clone(),
                    data.prefix.clone(),
                    data.local_name.clone(),
                );
                if let Some(element) = self.element_data_mut(copy) {
                    element.attributes = data.attributes.clone();
                }
                self.index_named(copy);
                copy
            }
        };
        if !is_document {
            let owner = self.owner_of(node);
            self.set_node_document(copy, owner);
        }
        if deep {
            if let (Some(source_contents), Some(copy_contents)) =
                (self.template_contents(node), self.template_contents(copy))
            {
                for child in self.children(source_contents).unwrap_or_default().to_vec() {
                    let child_copy = self.clone_node(child, true)?;
                    self.append_child(copy_contents, child_copy)?;
                }
            }
            for child in self.children(node).unwrap_or_default().to_vec() {
                let child_copy = self.clone_node(child, true)?;
                self.append_child(copy, child_copy)?;
            }
        }
        Ok(copy)
    }
}
