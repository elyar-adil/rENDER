//! DOM Standard §5 "Ranges": live and static ranges, boundary point comparison,
//! and the range algorithms that read or change the tree.
//!
//! [`Dom`] owns every range, because a range is state of the tree it was made
//! for. Each tree mutation updates the boundary points of the live ranges, as
//! the mutation algorithms of DOM §4.2 (insert, remove), §4.10 (replace data),
//! §4.11.1 (split text) and §4.4 (normalize) require, so a range always names a
//! position the tree still has. A [`RangeId`] names one entry of that table. A
//! static range (§5.5 `StaticRange`) shares the table as [`RangeKind::Static`],
//! and no mutation touches it.
//!
//! Offsets follow DOM §5.2. For character data they count UTF-16 code units,
//! and for every other node they are child indices. The mutation hooks keep
//! every offset at most its node's length.
//!
//! The arena keeps every range it creates for the life of the [`Dom`], so a
//! range that is no longer referenced by script still costs a mutation visit.

use std::cmp::Ordering;
use std::collections::BTreeSet;

use crate::{Dom, DomError, Node, NodeId, NodeKind};

/// A range in its [`Dom`]. Ids are never reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RangeId(usize);

/// A node and an offset within it (DOM §5.2 "boundary point").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoundaryPoint {
    pub node: NodeId,
    pub offset: usize,
}

/// Whether a range follows tree mutations (`Range`) or is fixed (`StaticRange`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeKind {
    Live,
    Static,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RangeRecord {
    kind: RangeKind,
    start: BoundaryPoint,
    end: BoundaryPoint,
}

/// Where one boundary point lies relative to another (DOM §5.2 "position of a
/// boundary point").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Position {
    Before,
    Equal,
    After,
}

/// The parts of a range's contents that the extract and clone algorithms split
/// on (DOM §5.5 "extract" and "clone contents", steps 5 to 9).
struct Shape {
    first_partial: Option<NodeId>,
    last_partial: Option<NodeId>,
    contained_children: Vec<NodeId>,
}

fn ordering_position(order: Ordering) -> Position {
    match order {
        Ordering::Less => Position::Before,
        Ordering::Equal => Position::Equal,
        Ordering::Greater => Position::After,
    }
}

impl Dom {
    /// `new Range()` and `Document.createRange()`: a live range collapsed at
    /// `(node, 0)`.
    ///
    /// # Errors
    ///
    /// Returns [`DomErrorKind::NotFound`](crate::DomErrorKind::NotFound) for an
    /// unknown node.
    pub fn create_live_range(&mut self, node: NodeId) -> Result<RangeId, DomError> {
        self.require_node(node)?;
        let point = BoundaryPoint { node, offset: 0 };
        Ok(self.push_range(RangeKind::Live, point, point))
    }

    /// `new StaticRange(init)` (DOM §5.5): a range that no mutation updates.
    ///
    /// # Errors
    ///
    /// Returns [`DomErrorKind::InvalidNodeType`](crate::DomErrorKind::InvalidNodeType)
    /// when either container is a `DocumentType`.
    pub fn create_static_range(
        &mut self,
        start: BoundaryPoint,
        end: BoundaryPoint,
    ) -> Result<RangeId, DomError> {
        self.require_not_doctype(start.node)?;
        self.require_not_doctype(end.node)?;
        Ok(self.push_range(RangeKind::Static, start, end))
    }

    /// The kind of `range`, or `None` for an id this document never issued.
    #[must_use]
    pub fn range_kind(&self, range: RangeId) -> Option<RangeKind> {
        self.ranges.get(range.0).map(|record| record.kind)
    }

    /// `startContainer` and `startOffset` (DOM §5.2).
    ///
    /// # Errors
    ///
    /// Returns [`DomErrorKind::NotFound`](crate::DomErrorKind::NotFound) for an
    /// unknown range.
    pub fn range_start(&self, range: RangeId) -> Result<BoundaryPoint, DomError> {
        Ok(self.range_record(range)?.start)
    }

    /// `endContainer` and `endOffset` (DOM §5.2).
    ///
    /// # Errors
    ///
    /// Returns [`DomErrorKind::NotFound`](crate::DomErrorKind::NotFound) for an
    /// unknown range.
    pub fn range_end(&self, range: RangeId) -> Result<BoundaryPoint, DomError> {
        Ok(self.range_record(range)?.end)
    }

    /// `collapsed` (DOM §5.2): the start and end are the same boundary point.
    ///
    /// # Errors
    ///
    /// Returns [`DomErrorKind::NotFound`](crate::DomErrorKind::NotFound) for an
    /// unknown range.
    pub fn range_is_collapsed(&self, range: RangeId) -> Result<bool, DomError> {
        let record = self.range_record(range)?;
        Ok(record.start == record.end)
    }

    /// `commonAncestorContainer` (DOM §5.2): the deepest node that contains both
    /// boundary points.
    ///
    /// # Errors
    ///
    /// Returns [`DomErrorKind::NotFound`](crate::DomErrorKind::NotFound) for an
    /// unknown range.
    pub fn range_common_ancestor(&self, range: RangeId) -> Result<NodeId, DomError> {
        let record = self.range_record(range)?;
        Ok(self.common_ancestor(record.start.node, record.end.node))
    }

    /// `setStart(node, offset)` (DOM §5.2).
    ///
    /// # Errors
    ///
    /// Returns [`DomErrorKind::InvalidNodeType`](crate::DomErrorKind::InvalidNodeType)
    /// for a `DocumentType`, and [`DomErrorKind::IndexSize`](crate::DomErrorKind::IndexSize)
    /// when `offset` is greater than the node's length.
    pub fn range_set_start(
        &mut self,
        range: RangeId,
        node: NodeId,
        offset: usize,
    ) -> Result<(), DomError> {
        let point = self.checked_point(node, offset)?;
        self.set_range_start(range, point)
    }

    /// `setEnd(node, offset)` (DOM §5.2).
    ///
    /// # Errors
    ///
    /// The same as [`Self::range_set_start`].
    pub fn range_set_end(
        &mut self,
        range: RangeId,
        node: NodeId,
        offset: usize,
    ) -> Result<(), DomError> {
        let point = self.checked_point(node, offset)?;
        self.set_range_end(range, point)
    }

    /// `setStartBefore(node)` (DOM §5.2).
    ///
    /// # Errors
    ///
    /// Returns [`DomErrorKind::InvalidNodeType`](crate::DomErrorKind::InvalidNodeType)
    /// when `node` has no parent.
    pub fn range_set_start_before(&mut self, range: RangeId, node: NodeId) -> Result<(), DomError> {
        let point = self.point_before(node)?;
        self.set_range_start(range, point)
    }

    /// `setStartAfter(node)` (DOM §5.2).
    ///
    /// # Errors
    ///
    /// The same as [`Self::range_set_start_before`].
    pub fn range_set_start_after(&mut self, range: RangeId, node: NodeId) -> Result<(), DomError> {
        let point = self.point_after(node)?;
        self.set_range_start(range, point)
    }

    /// `setEndBefore(node)` (DOM §5.2).
    ///
    /// # Errors
    ///
    /// The same as [`Self::range_set_start_before`].
    pub fn range_set_end_before(&mut self, range: RangeId, node: NodeId) -> Result<(), DomError> {
        let point = self.point_before(node)?;
        self.set_range_end(range, point)
    }

    /// `setEndAfter(node)` (DOM §5.2).
    ///
    /// # Errors
    ///
    /// The same as [`Self::range_set_start_before`].
    pub fn range_set_end_after(&mut self, range: RangeId, node: NodeId) -> Result<(), DomError> {
        let point = self.point_after(node)?;
        self.set_range_end(range, point)
    }

    /// `collapse(toStart)` (DOM §5.2).
    ///
    /// # Errors
    ///
    /// Returns [`DomErrorKind::NotFound`](crate::DomErrorKind::NotFound) for an
    /// unknown range.
    pub fn range_collapse(&mut self, range: RangeId, to_start: bool) -> Result<(), DomError> {
        let record = self.range_record_mut(range)?;
        if to_start {
            record.end = record.start;
        } else {
            record.start = record.end;
        }
        Ok(())
    }

    /// `selectNode(node)` (DOM §5.2).
    ///
    /// # Errors
    ///
    /// Returns [`DomErrorKind::InvalidNodeType`](crate::DomErrorKind::InvalidNodeType)
    /// when `node` has no parent.
    pub fn range_select_node(&mut self, range: RangeId, node: NodeId) -> Result<(), DomError> {
        let (parent, index) = self.parent_and_index(node)?;
        let record = self.range_record_mut(range)?;
        record.start = BoundaryPoint {
            node: parent,
            offset: index,
        };
        record.end = BoundaryPoint {
            node: parent,
            offset: index + 1,
        };
        Ok(())
    }

    /// `selectNodeContents(node)` (DOM §5.2).
    ///
    /// # Errors
    ///
    /// Returns [`DomErrorKind::InvalidNodeType`](crate::DomErrorKind::InvalidNodeType)
    /// for a `DocumentType`.
    pub fn range_select_node_contents(
        &mut self,
        range: RangeId,
        node: NodeId,
    ) -> Result<(), DomError> {
        self.require_not_doctype(node)?;
        let length = self.node_length(node);
        let record = self.range_record_mut(range)?;
        record.start = BoundaryPoint { node, offset: 0 };
        record.end = BoundaryPoint {
            node,
            offset: length,
        };
        Ok(())
    }

    /// `compareBoundaryPoints(how, sourceRange)` (DOM §5.2). Returns `-1`, `0`
    /// or `1`.
    ///
    /// # Errors
    ///
    /// Returns [`DomErrorKind::NotSupported`](crate::DomErrorKind::NotSupported)
    /// for a `how` above `END_TO_START`, and
    /// [`DomErrorKind::WrongDocument`](crate::DomErrorKind::WrongDocument) when
    /// the two ranges have different roots.
    pub fn range_compare_boundary_points(
        &self,
        range: RangeId,
        how: u16,
        other: RangeId,
    ) -> Result<i8, DomError> {
        let this = *self.range_record(range)?;
        let source = *self.range_record(other)?;
        let (this_point, other_point) = match how {
            // START_TO_START
            0 => (this.start, source.start),
            // START_TO_END
            1 => (this.end, source.start),
            // END_TO_END
            2 => (this.end, source.end),
            // END_TO_START
            3 => (this.start, source.end),
            _ => {
                return Err(DomError::not_supported(
                    "compareBoundaryPoints: unknown comparison type",
                ));
            }
        };
        if self.tree_root(this.start.node) != self.tree_root(source.start.node) {
            return Err(DomError::wrong_document("the ranges do not share a root"));
        }
        Ok(match self.position(this_point, other_point) {
            Position::Before => -1,
            Position::Equal => 0,
            Position::After => 1,
        })
    }

    /// `comparePoint(node, offset)` (DOM §5.2). Returns `-1`, `0` or `1`.
    ///
    /// # Errors
    ///
    /// Returns [`DomErrorKind::WrongDocument`](crate::DomErrorKind::WrongDocument)
    /// for a node in another tree, `InvalidNodeType` for a `DocumentType`, and
    /// `IndexSize` for an offset past the node's length.
    pub fn range_compare_point(
        &self,
        range: RangeId,
        node: NodeId,
        offset: usize,
    ) -> Result<i8, DomError> {
        let record = *self.range_record(range)?;
        if self.tree_root(node) != self.tree_root(record.start.node) {
            return Err(DomError::wrong_document("the node is in another tree"));
        }
        let point = self.checked_point(node, offset)?;
        Ok(if self.position(point, record.start) == Position::Before {
            -1
        } else if self.position(point, record.end) == Position::After {
            1
        } else {
            0
        })
    }

    /// `isPointInRange(node, offset)` (DOM §5.2).
    ///
    /// # Errors
    ///
    /// The same as [`Self::range_compare_point`], except that a node in another
    /// tree is `false` rather than an error.
    pub fn range_is_point_in_range(
        &self,
        range: RangeId,
        node: NodeId,
        offset: usize,
    ) -> Result<bool, DomError> {
        let record = *self.range_record(range)?;
        if self.tree_root(node) != self.tree_root(record.start.node) {
            return Ok(false);
        }
        let point = self.checked_point(node, offset)?;
        Ok(self.position(point, record.start) != Position::Before
            && self.position(point, record.end) != Position::After)
    }

    /// `intersectsNode(node)` (DOM §5.2).
    ///
    /// # Errors
    ///
    /// Returns [`DomErrorKind::NotFound`](crate::DomErrorKind::NotFound) for an
    /// unknown range.
    pub fn range_intersects_node(&self, range: RangeId, node: NodeId) -> Result<bool, DomError> {
        let record = *self.range_record(range)?;
        if self.tree_root(node) != self.tree_root(record.start.node) {
            return Ok(false);
        }
        let Some(parent) = self.parent(node) else {
            return Ok(true);
        };
        let index = self.index_or_zero(node);
        let before_end = self.position(
            BoundaryPoint {
                node: parent,
                offset: index,
            },
            record.end,
        ) == Position::Before;
        let after_start = self.position(
            BoundaryPoint {
                node: parent,
                offset: index + 1,
            },
            record.start,
        ) == Position::After;
        Ok(before_end && after_start)
    }

    /// `cloneRange()` (DOM §5.2): a new live range with the same boundary points.
    ///
    /// # Errors
    ///
    /// Returns [`DomErrorKind::NotFound`](crate::DomErrorKind::NotFound) for an
    /// unknown range.
    pub fn range_clone(&mut self, range: RangeId) -> Result<RangeId, DomError> {
        let record = *self.range_record(range)?;
        Ok(self.push_range(RangeKind::Live, record.start, record.end))
    }

    /// `toString()` (DOM §5.2): the text of the Text nodes in the range.
    ///
    /// # Errors
    ///
    /// Returns [`DomErrorKind::NotFound`](crate::DomErrorKind::NotFound) for an
    /// unknown range.
    pub fn range_to_string(&self, range: RangeId) -> Result<String, DomError> {
        // DOM §5.2 `toString()` reads Text nodes only, so a Comment or
        // ProcessingInstruction at a boundary contributes nothing.
        let record = *self.range_record(range)?;
        let (start, end) = (record.start, record.end);
        let is_text = |dom: &Self, node: NodeId| {
            matches!(dom.node(node).map(Node::kind), Some(NodeKind::Text(_)))
        };
        if start.node == end.node && is_text(self, start.node) {
            return self.utf16_substring(start.node, start.offset, end.offset);
        }
        let mut text = String::new();
        if is_text(self, start.node) {
            text.push_str(&self.utf16_substring(start.node, start.offset, usize::MAX)?);
        }
        for node in self.contained_nodes(start, end) {
            if let Some(NodeKind::Text(data)) = self.node(node).map(Node::kind) {
                text.push_str(data);
            }
        }
        if is_text(self, end.node) {
            text.push_str(&self.utf16_substring(end.node, 0, end.offset)?);
        }
        Ok(text)
    }

    /// `insertNode(node)` (DOM §5.2).
    ///
    /// # Errors
    ///
    /// Returns [`DomErrorKind::InvalidNodeType`](crate::DomErrorKind::InvalidNodeType)
    /// when the start node is a Comment or ProcessingInstruction, and the
    /// insertion errors of [`Self::insert_before`] otherwise.
    pub fn range_insert_node(&mut self, range: RangeId, node: NodeId) -> Result<(), DomError> {
        let start = self.range_record(range)?.start;
        if matches!(
            self.kind(start.node)?,
            NodeKind::Comment(_) | NodeKind::ProcessingInstruction { .. }
        ) {
            return Err(DomError::invalid_node_type(
                "insertNode: the start node is a Comment or ProcessingInstruction",
            ));
        }
        let is_text = matches!(self.kind(start.node)?, NodeKind::Text(_));
        let mut reference = if is_text {
            Some(start.node)
        } else {
            self.children(start.node)
                .and_then(|children| children.get(start.offset))
                .copied()
        };
        let parent = match reference {
            Some(child) => self.parent(child),
            None => Some(start.node),
        };
        let parent = parent
            .ok_or_else(|| DomError::hierarchy("insertNode: the insertion point has no parent"))?;
        self.pre_insert_checks(parent, node, reference)?;
        if is_text {
            reference = Some(self.split_text(start.node, start.offset)?);
        }
        if reference == Some(node) {
            reference = self.next_sibling(node);
        }
        if let Some(old_parent) = self.parent(node) {
            self.remove_child(old_parent, node)?;
        }
        let base = match reference {
            Some(child) => self.index_or_zero(child),
            None => self.node_length(parent),
        };
        let added = match self.kind(node)? {
            NodeKind::DocumentFragment => self.node_length(node),
            _ => 1,
        };
        self.insert_before(parent, node, reference)?;
        let record = self.range_record_mut(range)?;
        if record.start == record.end {
            record.end = BoundaryPoint {
                node: parent,
                offset: base + added,
            };
        }
        Ok(())
    }

    /// `deleteContents()` (DOM §5.2).
    ///
    /// # Errors
    ///
    /// Returns the errors of the replace-data and remove steps it runs.
    pub fn range_delete_contents(&mut self, range: RangeId) -> Result<(), DomError> {
        let record = *self.range_record(range)?;
        let point = self.delete_points(record.start, record.end)?;
        let record = self.range_record_mut(range)?;
        record.start = point;
        record.end = point;
        Ok(())
    }

    /// `extractContents()` (DOM §5.2). Returns the new `DocumentFragment`.
    ///
    /// # Errors
    ///
    /// Returns [`DomErrorKind::HierarchyRequest`](crate::DomErrorKind::HierarchyRequest)
    /// when the contents include a `DocumentType`.
    pub fn range_extract_contents(&mut self, range: RangeId) -> Result<NodeId, DomError> {
        let record = *self.range_record(range)?;
        let (fragment, point) = self.extract_points(record.start, record.end)?;
        let record = self.range_record_mut(range)?;
        record.start = point;
        record.end = point;
        Ok(fragment)
    }

    /// `cloneContents()` (DOM §5.2). Returns a new `DocumentFragment`.
    ///
    /// # Errors
    ///
    /// Returns the errors of appending the copies to the fragment.
    pub fn range_clone_contents(&mut self, range: RangeId) -> Result<NodeId, DomError> {
        let record = *self.range_record(range)?;
        self.clone_points(record.start, record.end)
    }

    /// `surroundContents(newParent)` (DOM §5.2).
    ///
    /// # Errors
    ///
    /// Returns [`DomErrorKind::InvalidState`](crate::DomErrorKind::InvalidState)
    /// when a non-Text node is partially contained, and
    /// [`DomErrorKind::InvalidNodeType`](crate::DomErrorKind::InvalidNodeType)
    /// for a Document, DocumentType or DocumentFragment `newParent`.
    pub fn range_surround_contents(
        &mut self,
        range: RangeId,
        new_parent: NodeId,
    ) -> Result<(), DomError> {
        let record = *self.range_record(range)?;
        let (start, end) = (record.start, record.end);
        let partially_contained = self
            .ancestors_inclusive(start.node)
            .into_iter()
            .chain(self.ancestors_inclusive(end.node))
            .filter(|node| self.partially_contained_in(start, end, *node));
        for node in partially_contained {
            if !matches!(self.kind(node)?, NodeKind::Text(_)) {
                return Err(DomError::invalid_state(
                    "surroundContents: a non-Text node is partially contained",
                ));
            }
        }
        if matches!(
            self.kind(new_parent)?,
            NodeKind::Document | NodeKind::DocumentType(_) | NodeKind::DocumentFragment
        ) {
            return Err(DomError::invalid_node_type(
                "surroundContents: the new parent cannot be a Document, DocumentType or DocumentFragment",
            ));
        }
        let fragment = self.range_extract_contents(range)?;
        let children = self.children(new_parent).unwrap_or_default().to_vec();
        for child in children {
            self.remove_child(new_parent, child)?;
        }
        self.range_insert_node(range, new_parent)?;
        self.insert_before(new_parent, fragment, None)?;
        self.range_select_node(range, new_parent)
    }

    /// The live ranges' boundary points after a child list insertion (DOM §4.2.3
    /// "insert", step 5): a point in `parent` past `child_index` moves by `count`.
    pub(crate) fn update_ranges_for_insert(
        &mut self,
        parent: NodeId,
        child_index: usize,
        count: usize,
    ) {
        self.for_each_live_point(|point| {
            if point.node == parent && point.offset > child_index {
                BoundaryPoint {
                    node: parent,
                    offset: point.offset + count,
                }
            } else {
                point
            }
        });
    }

    /// The live ranges' boundary points before `node` leaves `parent` (DOM §4.2.3
    /// "remove", steps 3 to 6). A point inside `node` moves to `(parent, index)`,
    /// and a point in `parent` past `index` moves back by one.
    pub(crate) fn update_ranges_for_remove(&mut self, node: NodeId, parent: NodeId, index: usize) {
        // The ancestor test reads the tree while the record table is being
        // written, so the table is walked by position rather than in a closure.
        for position in 0..self.ranges.len() {
            if self.ranges[position].kind != RangeKind::Live {
                continue;
            }
            let start = self.point_after_remove(self.ranges[position].start, node, parent, index);
            let end = self.point_after_remove(self.ranges[position].end, node, parent, index);
            self.ranges[position].start = start;
            self.ranges[position].end = end;
        }
    }

    /// The live ranges' boundary points for a replace-data step (DOM §4.10,
    /// steps 8 to 11): a point strictly inside the replaced span moves to
    /// `offset`, and a point after it moves by the change in length.
    pub(crate) fn update_ranges_for_replace_data(
        &mut self,
        node: NodeId,
        offset: usize,
        count: usize,
        inserted: usize,
    ) {
        self.for_each_live_point(|point| {
            if point.node != node || point.offset <= offset {
                point
            } else if point.offset <= offset + count {
                BoundaryPoint { node, offset }
            } else {
                BoundaryPoint {
                    node,
                    offset: point.offset - count + inserted,
                }
            }
        });
    }

    /// The live ranges' boundary points for the split-text step (DOM §4.11.1):
    /// a point in `node` past `offset` moves into `new_node`, and a point in
    /// `parent` just after `node` moves past `new_node`.
    pub(crate) fn update_ranges_for_split_text(
        &mut self,
        node: NodeId,
        new_node: NodeId,
        parent: NodeId,
        offset: usize,
        index: usize,
    ) {
        self.for_each_live_point(|point| {
            if point.node == node && point.offset > offset {
                BoundaryPoint {
                    node: new_node,
                    offset: point.offset - offset,
                }
            } else if point.node == parent && point.offset == index + 1 {
                BoundaryPoint {
                    node: parent,
                    offset: index + 2,
                }
            } else {
                point
            }
        });
    }

    /// The live ranges' boundary points for one contiguous Text node merged into
    /// `into` by normalize (DOM §4.4, step 7). `base` is the length of `into`'s
    /// data before the merged node's data, and `merged_index` is the merged
    /// node's index in `parent`.
    pub(crate) fn update_ranges_for_merge(
        &mut self,
        merged: NodeId,
        into: NodeId,
        parent: NodeId,
        merged_index: usize,
        base: usize,
    ) {
        self.for_each_live_point(|point| {
            if point.node == merged {
                BoundaryPoint {
                    node: into,
                    offset: point.offset + base,
                }
            } else if point.node == parent && point.offset == merged_index {
                BoundaryPoint {
                    node: into,
                    offset: base,
                }
            } else {
                point
            }
        });
    }

    /// The length of a node (DOM §4.2.1): the UTF-16 length of character data,
    /// zero for a DocumentType, and the child count otherwise.
    pub(crate) fn node_length(&self, node: NodeId) -> usize {
        match self.node(node).map(Node::kind) {
            Some(NodeKind::Text(data) | NodeKind::Comment(data)) => utf16_len(data),
            Some(NodeKind::ProcessingInstruction { data, .. }) => utf16_len(data),
            Some(NodeKind::DocumentType(_)) | None => 0,
            Some(_) => self.children(node).map_or(0, <[NodeId]>::len),
        }
    }

    /// Whether `node` is a Text, Comment or ProcessingInstruction node.
    pub(crate) fn is_character_data(&self, node: NodeId) -> bool {
        matches!(
            self.node(node).map(Node::kind),
            Some(NodeKind::Text(_) | NodeKind::Comment(_) | NodeKind::ProcessingInstruction { .. })
        )
    }

    /// The index of `node` among its parent's children, or `None` when it has no
    /// parent.
    pub(crate) fn index_in_parent(&self, node: NodeId) -> Option<usize> {
        let parent = self.parent(node)?;
        self.children(parent)?
            .iter()
            .position(|candidate| *candidate == node)
    }

    fn index_or_zero(&self, node: NodeId) -> usize {
        self.index_in_parent(node).unwrap_or(0)
    }

    fn push_range(&mut self, kind: RangeKind, start: BoundaryPoint, end: BoundaryPoint) -> RangeId {
        let id = RangeId(self.ranges.len());
        self.ranges.push(RangeRecord { kind, start, end });
        id
    }

    fn range_record(&self, range: RangeId) -> Result<&RangeRecord, DomError> {
        self.ranges
            .get(range.0)
            .ok_or_else(|| DomError::not_found("unknown range"))
    }

    fn range_record_mut(&mut self, range: RangeId) -> Result<&mut RangeRecord, DomError> {
        self.ranges
            .get_mut(range.0)
            .ok_or_else(|| DomError::not_found("unknown range"))
    }

    /// Apply `update` to both boundary points of every live range.
    fn for_each_live_point(&mut self, mut update: impl FnMut(BoundaryPoint) -> BoundaryPoint) {
        for record in self
            .ranges
            .iter_mut()
            .filter(|record| record.kind == RangeKind::Live)
        {
            record.start = update(record.start);
            record.end = update(record.end);
        }
    }

    fn point_after_remove(
        &self,
        point: BoundaryPoint,
        node: NodeId,
        parent: NodeId,
        index: usize,
    ) -> BoundaryPoint {
        if self.is_inclusive_ancestor(node, point.node) {
            BoundaryPoint {
                node: parent,
                offset: index,
            }
        } else if point.node == parent && point.offset > index {
            BoundaryPoint {
                node: parent,
                offset: point.offset - 1,
            }
        } else {
            point
        }
    }

    fn require_not_doctype(&self, node: NodeId) -> Result<(), DomError> {
        if matches!(self.kind(node)?, NodeKind::DocumentType(_)) {
            Err(DomError::invalid_node_type(
                "a DocumentType cannot contain a boundary point",
            ))
        } else {
            Ok(())
        }
    }

    /// A boundary point for `(node, offset)`, after the checks of DOM §5.2
    /// `setStart`: not a DocumentType, and an offset within the node's length.
    fn checked_point(&self, node: NodeId, offset: usize) -> Result<BoundaryPoint, DomError> {
        self.require_not_doctype(node)?;
        if offset > self.node_length(node) {
            return Err(DomError::index_size(
                "the offset is greater than the node's length",
            ));
        }
        Ok(BoundaryPoint { node, offset })
    }

    fn parent_and_index(&self, node: NodeId) -> Result<(NodeId, usize), DomError> {
        let parent = self
            .parent(node)
            .ok_or_else(|| DomError::invalid_node_type("the node has no parent"))?;
        let index = self
            .index_in_parent(node)
            .ok_or_else(|| DomError::not_found("the node is not a child of its parent"))?;
        Ok((parent, index))
    }

    fn point_before(&self, node: NodeId) -> Result<BoundaryPoint, DomError> {
        let (parent, index) = self.parent_and_index(node)?;
        Ok(BoundaryPoint {
            node: parent,
            offset: index,
        })
    }

    fn point_after(&self, node: NodeId) -> Result<BoundaryPoint, DomError> {
        let (parent, index) = self.parent_and_index(node)?;
        Ok(BoundaryPoint {
            node: parent,
            offset: index + 1,
        })
    }

    /// DOM §5.2 "set the start of a live range": the end moves to `point` when
    /// `point` is in another root or after the end.
    fn set_range_start(&mut self, range: RangeId, point: BoundaryPoint) -> Result<(), DomError> {
        let record = *self.range_record(range)?;
        let moves_end = self.tree_root(record.start.node) != self.tree_root(point.node)
            || self.position(point, record.end) == Position::After;
        let record = self.range_record_mut(range)?;
        if moves_end {
            record.end = point;
        }
        record.start = point;
        Ok(())
    }

    /// DOM §5.2 "set the end of a live range": the start moves to `point` when
    /// `point` is in another root or before the start.
    fn set_range_end(&mut self, range: RangeId, point: BoundaryPoint) -> Result<(), DomError> {
        let record = *self.range_record(range)?;
        let moves_start = self.tree_root(record.start.node) != self.tree_root(point.node)
            || self.position(point, record.start) == Position::Before;
        let record = self.range_record_mut(range)?;
        if moves_start {
            record.start = point;
        }
        record.end = point;
        Ok(())
    }

    /// DOM §5.2 "position of a boundary point A relative to B". Both points
    /// must share a root for the answer to mean anything.
    fn position(&self, left: BoundaryPoint, right: BoundaryPoint) -> Position {
        if left.node == right.node {
            return ordering_position(left.offset.cmp(&right.offset));
        }
        if self.tree_order(left.node, right.node) == Some(Ordering::Greater) {
            return match self.position(right, left) {
                Position::Before => Position::After,
                Position::Equal | Position::After => Position::Before,
            };
        }
        if self.is_inclusive_ancestor(left.node, right.node) {
            let mut child = right.node;
            while let Some(parent) = self.parent(child) {
                if parent == left.node {
                    break;
                }
                child = parent;
            }
            return if self.index_or_zero(child) < left.offset {
                Position::After
            } else {
                Position::Before
            };
        }
        Position::Before
    }

    /// The order of two nodes in tree order (DOM §4.2.1 "preceding" and
    /// "following"), or `None` when they are in different trees.
    fn tree_order(&self, left: NodeId, right: NodeId) -> Option<Ordering> {
        if left == right {
            return Some(Ordering::Equal);
        }
        let left_path = self.path_from_root(left);
        let right_path = self.path_from_root(right);
        let shared = left_path
            .iter()
            .zip(&right_path)
            .take_while(|(a, b)| a == b)
            .count();
        if shared == 0 {
            return None;
        }
        if shared == left_path.len() {
            return Some(Ordering::Less);
        }
        if shared == right_path.len() {
            return Some(Ordering::Greater);
        }
        let left_child = *left_path.get(shared)?;
        let right_child = *right_path.get(shared)?;
        Some(
            self.index_in_parent(left_child)?
                .cmp(&self.index_in_parent(right_child)?),
        )
    }

    /// `node` and its ancestors, from the root down to `node`.
    fn path_from_root(&self, node: NodeId) -> Vec<NodeId> {
        let mut path = vec![node];
        let mut current = node;
        while let Some(parent) = self.parent(current) {
            path.push(parent);
            current = parent;
        }
        path.reverse();
        path
    }

    /// `node` and its ancestors, from `node` up to the root.
    fn ancestors_inclusive(&self, node: NodeId) -> Vec<NodeId> {
        let mut path = vec![node];
        let mut current = node;
        while let Some(parent) = self.parent(current) {
            path.push(parent);
            current = parent;
        }
        path
    }

    /// The deepest node containing both `start` and `end`.
    fn common_ancestor(&self, start: NodeId, end: NodeId) -> NodeId {
        let mut container = start;
        while !self.is_inclusive_ancestor(container, end) {
            match self.parent(container) {
                Some(parent) => container = parent,
                None => break,
            }
        }
        container
    }

    /// DOM §5.2 "contained": in the same root, with `(node, 0)` after the start
    /// and `(node, length)` before the end.
    fn contained_in(&self, start: BoundaryPoint, end: BoundaryPoint, node: NodeId) -> bool {
        self.tree_root(node) == self.tree_root(start.node)
            && self.position(BoundaryPoint { node, offset: 0 }, start) == Position::After
            && self.position(
                BoundaryPoint {
                    node,
                    offset: self.node_length(node),
                },
                end,
            ) == Position::Before
    }

    /// DOM §5.2 "partially contained": an inclusive ancestor of exactly one of
    /// the two boundary nodes.
    fn partially_contained_in(
        &self,
        start: BoundaryPoint,
        end: BoundaryPoint,
        node: NodeId,
    ) -> bool {
        self.is_inclusive_ancestor(node, start.node) != self.is_inclusive_ancestor(node, end.node)
    }

    /// Every node contained in the range, in tree order.
    fn contained_nodes(&self, start: BoundaryPoint, end: BoundaryPoint) -> Vec<NodeId> {
        let common = self.common_ancestor(start.node, end.node);
        let mut found = Vec::new();
        let mut stack = vec![common];
        while let Some(node) = stack.pop() {
            if node != common && self.contained_in(start, end, node) {
                found.push(node);
            }
            for child in self.children(node).unwrap_or_default().iter().rev() {
                stack.push(*child);
            }
        }
        found
    }

    fn shape(&self, start: BoundaryPoint, end: BoundaryPoint) -> Shape {
        let common = self.common_ancestor(start.node, end.node);
        let children = self.children(common).unwrap_or_default();
        let start_contains_end = self.is_inclusive_ancestor(start.node, end.node);
        let end_contains_start = self.is_inclusive_ancestor(end.node, start.node);
        let first_partial = if start_contains_end {
            None
        } else {
            children
                .iter()
                .copied()
                .find(|child| self.partially_contained_in(start, end, *child))
        };
        let last_partial = if end_contains_start {
            None
        } else {
            children
                .iter()
                .rev()
                .copied()
                .find(|child| self.partially_contained_in(start, end, *child))
        };
        let contained_children = children
            .iter()
            .copied()
            .filter(|child| self.contained_in(start, end, *child))
            .collect();
        Shape {
            first_partial,
            last_partial,
            contained_children,
        }
    }

    /// The new boundary point after the contents are removed (DOM §5.5
    /// "delete", steps 5 and 6).
    fn boundary_after_delete(&self, start: BoundaryPoint, end: BoundaryPoint) -> BoundaryPoint {
        if self.is_inclusive_ancestor(start.node, end.node) {
            return start;
        }
        let mut reference = start.node;
        while let Some(parent) = self.parent(reference) {
            if self.is_inclusive_ancestor(parent, end.node) {
                break;
            }
            reference = parent;
        }
        match self.parent(reference) {
            Some(parent) => BoundaryPoint {
                node: parent,
                offset: self.index_or_zero(reference) + 1,
            },
            None => start,
        }
    }

    /// The UTF-16 substring `[start, end)` of a character data node, clamped to
    /// its length. Lone surrogates cannot be stored, so they become U+FFFD, as
    /// the tree stores all text.
    fn utf16_substring(&self, node: NodeId, start: usize, end: usize) -> Result<String, DomError> {
        let units: Vec<u16> = self.character_data(node)?.encode_utf16().collect();
        let end = end.min(units.len());
        let start = start.min(end);
        Ok(String::from_utf16_lossy(&units[start..end]))
    }

    /// The data of a character data node, or an error for any other node.
    fn character_data(&self, node: NodeId) -> Result<&str, DomError> {
        match self.kind(node)? {
            NodeKind::Text(data)
            | NodeKind::Comment(data)
            | NodeKind::ProcessingInstruction { data, .. } => Ok(data),
            _ => Err(DomError::invalid_node_type(
                "node does not implement CharacterData",
            )),
        }
    }

    /// A new character data node of the same kind as `node`, holding `data`.
    fn character_data_copy(&mut self, node: NodeId, data: String) -> Result<NodeId, DomError> {
        // The target is copied out first, so the borrow of the source ends
        // before the new node is allocated.
        let target = match self.kind(node)? {
            NodeKind::Text(_) | NodeKind::Comment(_) => None,
            NodeKind::ProcessingInstruction { target, .. } => Some(target.clone()),
            _ => {
                return Err(DomError::invalid_node_type(
                    "node does not implement CharacterData",
                ));
            }
        };
        let copy = match (target, self.kind(node)?) {
            (Some(target), _) => self.create_processing_instruction(target, data),
            (None, NodeKind::Comment(_)) => self.create_comment(data),
            (None, _) => self.create_text(data),
        };
        Ok(copy)
    }

    /// DOM §5.5 "delete the contents" over boundary points. Returns the boundary
    /// point the range collapses to.
    fn delete_points(
        &mut self,
        start: BoundaryPoint,
        end: BoundaryPoint,
    ) -> Result<BoundaryPoint, DomError> {
        if start == end {
            return Ok(start);
        }
        if start.node == end.node && self.is_character_data(start.node) {
            self.replace_data(
                start.node,
                start.offset,
                end.offset.saturating_sub(start.offset),
                "",
            )?;
            return Ok(start);
        }
        let contained = self.contained_nodes(start, end);
        let in_list: BTreeSet<NodeId> = contained.iter().copied().collect();
        let to_remove: Vec<NodeId> = contained
            .into_iter()
            .filter(|node| {
                !self
                    .parent(*node)
                    .is_some_and(|parent| in_list.contains(&parent))
            })
            .collect();
        let point = self.boundary_after_delete(start, end);
        if self.is_character_data(start.node) {
            let length = self.node_length(start.node);
            self.replace_data(start.node, start.offset, length - start.offset, "")?;
        }
        for node in to_remove {
            if let Some(parent) = self.parent(node) {
                self.remove_child(parent, node)?;
            }
        }
        if self.is_character_data(end.node) {
            self.replace_data(end.node, 0, end.offset, "")?;
        }
        Ok(point)
    }

    /// DOM §5.5 "extract" over boundary points. Returns the fragment and the
    /// boundary point the range collapses to.
    fn extract_points(
        &mut self,
        start: BoundaryPoint,
        end: BoundaryPoint,
    ) -> Result<(NodeId, BoundaryPoint), DomError> {
        let fragment = self.create_document_fragment();
        if start == end {
            return Ok((fragment, start));
        }
        if start.node == end.node && self.is_character_data(start.node) {
            let data = self.utf16_substring(start.node, start.offset, end.offset)?;
            let copy = self.character_data_copy(start.node, data)?;
            self.append_child(fragment, copy)?;
            self.replace_data(start.node, start.offset, end.offset - start.offset, "")?;
            return Ok((fragment, start));
        }
        let shape = self.shape(start, end);
        for child in &shape.contained_children {
            if matches!(self.kind(*child)?, NodeKind::DocumentType(_)) {
                return Err(DomError::hierarchy(
                    "extractContents: the range contains a DocumentType",
                ));
            }
        }
        let point = self.boundary_after_delete(start, end);
        if let Some(first) = shape.first_partial {
            if self.is_character_data(first) {
                let length = self.node_length(start.node);
                let data = self.utf16_substring(start.node, start.offset, length)?;
                let copy = self.character_data_copy(start.node, data)?;
                self.append_child(fragment, copy)?;
                self.replace_data(start.node, start.offset, length - start.offset, "")?;
            } else {
                let copy = self.clone_node(first, false)?;
                self.append_child(fragment, copy)?;
                let inner_end = BoundaryPoint {
                    node: first,
                    offset: self.node_length(first),
                };
                let (inner, _) = self.extract_points(start, inner_end)?;
                self.append_child(copy, inner)?;
            }
        }
        for child in shape.contained_children {
            self.append_child(fragment, child)?;
        }
        if let Some(last) = shape.last_partial {
            if self.is_character_data(last) {
                let data = self.utf16_substring(end.node, 0, end.offset)?;
                let copy = self.character_data_copy(end.node, data)?;
                self.append_child(fragment, copy)?;
                self.replace_data(end.node, 0, end.offset, "")?;
            } else {
                let copy = self.clone_node(last, false)?;
                self.append_child(fragment, copy)?;
                let inner_start = BoundaryPoint {
                    node: last,
                    offset: 0,
                };
                let (inner, _) = self.extract_points(inner_start, end)?;
                self.append_child(copy, inner)?;
            }
        }
        Ok((fragment, point))
    }

    /// DOM §5.5 "clone the contents" over boundary points. Creates copies and
    /// leaves the tree as it is.
    fn clone_points(
        &mut self,
        start: BoundaryPoint,
        end: BoundaryPoint,
    ) -> Result<NodeId, DomError> {
        let fragment = self.create_document_fragment();
        if start == end {
            return Ok(fragment);
        }
        if start.node == end.node && self.is_character_data(start.node) {
            let data = self.utf16_substring(start.node, start.offset, end.offset)?;
            let copy = self.character_data_copy(start.node, data)?;
            self.append_child(fragment, copy)?;
            return Ok(fragment);
        }
        let shape = self.shape(start, end);
        if let Some(first) = shape.first_partial {
            if self.is_character_data(first) {
                let length = self.node_length(start.node);
                let data = self.utf16_substring(start.node, start.offset, length)?;
                let copy = self.character_data_copy(start.node, data)?;
                self.append_child(fragment, copy)?;
            } else {
                let copy = self.clone_node(first, false)?;
                self.append_child(fragment, copy)?;
                let inner_end = BoundaryPoint {
                    node: first,
                    offset: self.node_length(first),
                };
                let inner = self.clone_points(start, inner_end)?;
                self.append_child(copy, inner)?;
            }
        }
        for child in shape.contained_children {
            let copy = self.clone_node(child, true)?;
            self.append_child(fragment, copy)?;
        }
        if let Some(last) = shape.last_partial {
            if self.is_character_data(last) {
                let data = self.utf16_substring(end.node, 0, end.offset)?;
                let copy = self.character_data_copy(end.node, data)?;
                self.append_child(fragment, copy)?;
            } else {
                let copy = self.clone_node(last, false)?;
                self.append_child(fragment, copy)?;
                let inner_start = BoundaryPoint {
                    node: last,
                    offset: 0,
                };
                let inner = self.clone_points(inner_start, end)?;
                self.append_child(copy, inner)?;
            }
        }
        Ok(fragment)
    }
}

/// The UTF-16 length of `data` (DOM §4.10 "length").
pub(crate) fn utf16_len(data: &str) -> usize {
    data.encode_utf16().count()
}

#[cfg(test)]
mod tests;
