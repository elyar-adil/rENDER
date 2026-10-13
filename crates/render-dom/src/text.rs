//! The character data algorithms that change text in place: replace data
//! (DOM §4.10), split text (DOM §4.11.1) and normalize (DOM §4.4). Each one
//! runs the live-range updates its specification names, so a range over the
//! text stays on the same characters.

use crate::{Dom, DomError, MutationKind, Node, NodeId, NodeKind};

impl Dom {
    /// DOM §4.10 "replace data": replace `count` UTF-16 code units at `offset`
    /// with `data`. `count` is clamped to the data that remains.
    ///
    /// # Errors
    ///
    /// Returns [`DomErrorKind::InvalidNodeType`](crate::DomErrorKind::InvalidNodeType)
    /// for a node that is not character data, [`DomErrorKind::IndexSize`](crate::DomErrorKind::IndexSize)
    /// when `offset` is past the end, and [`DomErrorKind::NotFound`](crate::DomErrorKind::NotFound)
    /// for an unknown node.
    pub fn replace_data(
        &mut self,
        node: NodeId,
        offset: usize,
        count: usize,
        data: &str,
    ) -> Result<(), DomError> {
        let units: Vec<u16> = self.character_data_text(node)?.encode_utf16().collect();
        let length = units.len();
        if offset > length {
            return Err(DomError::index_size(
                "the offset is greater than the length of the data",
            ));
        }
        let count = count.min(length - offset);
        let inserted: Vec<u16> = data.encode_utf16().collect();
        let mut next = Vec::with_capacity(length - count + inserted.len());
        next.extend_from_slice(&units[..offset]);
        next.extend_from_slice(&inserted);
        next.extend_from_slice(&units[offset + count..]);
        self.set_data_text(node, String::from_utf16_lossy(&next));
        self.update_ranges_for_replace_data(node, offset, count, inserted.len());
        self.record_character_data(node);
        Ok(())
    }

    /// DOM §4.11.1 `splitText(offset)`: split the Text node at `offset` and
    /// return the new node holding the rest of the data. When the node has a
    /// parent, the new node is inserted after it.
    ///
    /// # Errors
    ///
    /// Returns [`DomErrorKind::InvalidNodeType`](crate::DomErrorKind::InvalidNodeType)
    /// for a node that is not a Text node, and [`DomErrorKind::IndexSize`](crate::DomErrorKind::IndexSize)
    /// when `offset` is past the end of the data.
    pub fn split_text(&mut self, node: NodeId, offset: usize) -> Result<NodeId, DomError> {
        if !matches!(self.node_kind_or_error(node)?, NodeKind::Text(_)) {
            return Err(DomError::invalid_node_type(
                "splitText requires a Text node",
            ));
        }
        let units: Vec<u16> = self.character_data_text(node)?.encode_utf16().collect();
        let length = units.len();
        if offset > length {
            return Err(DomError::index_size(
                "the offset is greater than the length of the data",
            ));
        }
        let count = length - offset;
        let new_node = self.create_text(String::from_utf16_lossy(&units[offset..]));
        if let Some(parent) = self.parent(node) {
            let index = self.index_in_parent(node).unwrap_or(0);
            let next = self.next_sibling(node);
            self.insert_before(parent, new_node, next)?;
            self.update_ranges_for_split_text(node, new_node, parent, offset, index);
        }
        self.replace_data(node, offset, count, "")?;
        Ok(new_node)
    }

    /// DOM §4.4 `normalize()`: remove empty Text nodes and merge each run of
    /// contiguous Text nodes below `root` into its first node, in tree order.
    ///
    /// # Errors
    ///
    /// Returns [`DomErrorKind::NotFound`](crate::DomErrorKind::NotFound) for an
    /// unknown node, and the errors of the remove and replace-data steps.
    pub fn normalize(&mut self, root: NodeId) -> Result<(), DomError> {
        self.node_kind_or_error(root)?;
        let mut texts = Vec::new();
        let mut stack: Vec<NodeId> = self
            .children(root)
            .unwrap_or_default()
            .iter()
            .rev()
            .copied()
            .collect();
        while let Some(node) = stack.pop() {
            if matches!(self.node(node).map(Node::kind), Some(NodeKind::Text(_))) {
                texts.push(node);
            }
            for child in self.children(node).unwrap_or_default().iter().rev() {
                stack.push(*child);
            }
        }
        for node in texts {
            // A node merged into an earlier one, or removed as empty, has left the tree.
            let Some(parent) = self.parent(node) else {
                continue;
            };
            if self.node_length(node) == 0 {
                self.remove_child(parent, node)?;
                continue;
            }
            let Some(index) = self.index_in_parent(node) else {
                continue;
            };
            // The Text nodes that follow `node` directly. Nothing moves until
            // they are removed below, so the k-th one sits at `index + 1 + k`.
            let contiguous: Vec<NodeId> = {
                let siblings = self.children(parent).unwrap_or_default();
                siblings[index + 1..]
                    .iter()
                    .copied()
                    .take_while(|sibling| self.is_text_node(*sibling))
                    .collect()
            };
            if contiguous.is_empty() {
                continue;
            }
            let mut appended = String::new();
            for sibling in &contiguous {
                appended.push_str(self.character_data_text(*sibling)?);
            }
            // DOM §4.4 steps 5 to 7: one replace-data step appends the data,
            // then each merged node's boundary points move into `node`.
            let mut length = self.node_length(node);
            self.replace_data(node, length, 0, &appended)?;
            for (position, sibling) in contiguous.iter().copied().enumerate() {
                let sibling_length = self.node_length(sibling);
                self.update_ranges_for_merge(sibling, node, parent, index + 1 + position, length);
                length += sibling_length;
            }
            for sibling in contiguous {
                self.remove_child(parent, sibling)?;
            }
        }
        Ok(())
    }

    fn is_text_node(&self, node: NodeId) -> bool {
        matches!(self.node(node).map(Node::kind), Some(NodeKind::Text(_)))
    }

    /// Replace the data of a character data node. The caller has checked the
    /// node kind and the offsets.
    fn set_data_text(&mut self, node: NodeId, value: String) {
        if let Ok(target) = self.node_mut(node) {
            match &mut target.kind {
                NodeKind::Text(data)
                | NodeKind::Comment(data)
                | NodeKind::ProcessingInstruction { data, .. } => *data = value,
                _ => {}
            }
        }
    }

    /// Record a CharacterData mutation of `node` in the journal.
    fn record_character_data(&mut self, node: NodeId) {
        self.record_mutations([MutationKind::CharacterData { target: node }]);
    }

    /// The node's kind, or a `NotFound` error for an unknown node.
    fn node_kind_or_error(&self, node: NodeId) -> Result<&NodeKind, DomError> {
        self.node(node)
            .map(Node::kind)
            .ok_or_else(|| DomError::not_found("unknown node"))
    }

    /// The data of a character data node.
    pub(crate) fn character_data_text(&self, node: NodeId) -> Result<&str, DomError> {
        match self.node_kind_or_error(node)? {
            NodeKind::Text(data)
            | NodeKind::Comment(data)
            | NodeKind::ProcessingInstruction { data, .. } => Ok(data),
            _ => Err(DomError::invalid_node_type(
                "node does not implement CharacterData",
            )),
        }
    }
}
