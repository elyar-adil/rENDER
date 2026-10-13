//! `AbstractRange`, `Range` and `StaticRange` (DOM §5) and
//! `Document.createRange()`.
//!
//! The boundary points and the range algorithms live in `render-dom`, which
//! updates every live range on each tree mutation. A script holds an object
//! whose host is `ObjectHost::Range`, naming one entry of that table, so a
//! range's state is the document's state and follows the tree as it changes.

use render_dom::{BoundaryPoint, Dom, NodeId, RangeId, RangeKind};

use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::runtime::builtins::dom_collections::unsigned_long;
use crate::runtime::convert::required_argument;
use crate::value::ObjectHost;
use crate::value::RangeOp;

/// An `unsigned long` offset as a JavaScript number.
fn offset_number(offset: usize) -> JsValue {
    JsValue::Number(f64::from(u32::try_from(offset).unwrap_or(u32::MAX)))
}

/// An `unsigned short` (`WebIDL` §3.2.4.8): the `unsigned long` conversion taken
/// modulo 2^16.
fn unsigned_short(number: f64) -> u16 {
    u16::try_from(unsigned_long(number) % 65_536).unwrap_or(0)
}

impl JsRuntime {
    /// Run one operation of `AbstractRange`, `Range` or `StaticRange`, or
    /// `Document.createRange()`. The getters accept a static range too; every
    /// method of `Range` requires a live one.
    #[allow(
        clippy::too_many_lines,
        reason = "one flat table of the range operations reads better than fragments"
    )]
    pub(in crate::runtime) fn range_native(
        &mut self,
        dom: &mut Dom,
        op: RangeOp,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match op {
            RangeOp::StartContainer => {
                let range = self.any_range(receiver)?;
                let point = dom.range_start(range)?;
                self.wrap_node(dom, point.node)
            }
            RangeOp::EndContainer => {
                let range = self.any_range(receiver)?;
                let point = dom.range_end(range)?;
                self.wrap_node(dom, point.node)
            }
            RangeOp::StartOffset => {
                let range = self.any_range(receiver)?;
                Ok(offset_number(dom.range_start(range)?.offset))
            }
            RangeOp::EndOffset => {
                let range = self.any_range(receiver)?;
                Ok(offset_number(dom.range_end(range)?.offset))
            }
            RangeOp::Collapsed => {
                let range = self.any_range(receiver)?;
                Ok(JsValue::Boolean(dom.range_is_collapsed(range)?))
            }
            RangeOp::CreateRange => {
                let document = self.require_document(receiver)?;
                let range = dom.create_live_range(document)?;
                self.wrap_range(dom, range)
            }
            other => self.range_method(dom, other, receiver, arguments),
        }
    }

    /// The methods of `Range`, which require a live range as the receiver.
    #[allow(
        clippy::too_many_lines,
        reason = "one flat table of the range methods reads better than fragments"
    )]
    fn range_method(
        &mut self,
        dom: &mut Dom,
        op: RangeOp,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let range = self.live_range(dom, receiver)?;
        match op {
            RangeOp::CommonAncestorContainer => {
                let node = dom.range_common_ancestor(range)?;
                self.wrap_node(dom, node)
            }
            RangeOp::SetStart => {
                let node = self.node_argument(arguments, 0, "setStart")?;
                let offset = self.unsigned_long_argument(dom, arguments, 1, "setStart")?;
                dom.range_set_start(range, node, offset)?;
                Ok(JsValue::Undefined)
            }
            RangeOp::SetEnd => {
                let node = self.node_argument(arguments, 0, "setEnd")?;
                let offset = self.unsigned_long_argument(dom, arguments, 1, "setEnd")?;
                dom.range_set_end(range, node, offset)?;
                Ok(JsValue::Undefined)
            }
            RangeOp::SetStartBefore => {
                let node = self.node_argument(arguments, 0, "setStartBefore")?;
                dom.range_set_start_before(range, node)?;
                Ok(JsValue::Undefined)
            }
            RangeOp::SetStartAfter => {
                let node = self.node_argument(arguments, 0, "setStartAfter")?;
                dom.range_set_start_after(range, node)?;
                Ok(JsValue::Undefined)
            }
            RangeOp::SetEndBefore => {
                let node = self.node_argument(arguments, 0, "setEndBefore")?;
                dom.range_set_end_before(range, node)?;
                Ok(JsValue::Undefined)
            }
            RangeOp::SetEndAfter => {
                let node = self.node_argument(arguments, 0, "setEndAfter")?;
                dom.range_set_end_after(range, node)?;
                Ok(JsValue::Undefined)
            }
            RangeOp::Collapse => {
                // `collapse(optional boolean toStart = false)`.
                let to_start = arguments.first().is_some_and(JsValue::is_truthy);
                dom.range_collapse(range, to_start)?;
                Ok(JsValue::Undefined)
            }
            RangeOp::SelectNode => {
                let node = self.node_argument(arguments, 0, "selectNode")?;
                dom.range_select_node(range, node)?;
                Ok(JsValue::Undefined)
            }
            RangeOp::SelectNodeContents => {
                let node = self.node_argument(arguments, 0, "selectNodeContents")?;
                dom.range_select_node_contents(range, node)?;
                Ok(JsValue::Undefined)
            }
            RangeOp::CompareBoundaryPoints => {
                let how =
                    self.unsigned_short_argument(dom, arguments, 0, "compareBoundaryPoints")?;
                let source =
                    self.source_range_argument(dom, arguments, 1, "compareBoundaryPoints")?;
                let order = dom.range_compare_boundary_points(range, how, source)?;
                Ok(JsValue::Number(f64::from(order)))
            }
            RangeOp::ComparePoint => {
                let node = self.node_argument(arguments, 0, "comparePoint")?;
                let offset = self.unsigned_long_argument(dom, arguments, 1, "comparePoint")?;
                let order = dom.range_compare_point(range, node, offset)?;
                Ok(JsValue::Number(f64::from(order)))
            }
            RangeOp::IsPointInRange => {
                let node = self.node_argument(arguments, 0, "isPointInRange")?;
                let offset = self.unsigned_long_argument(dom, arguments, 1, "isPointInRange")?;
                Ok(JsValue::Boolean(
                    dom.range_is_point_in_range(range, node, offset)?,
                ))
            }
            RangeOp::IntersectsNode => {
                let node = self.node_argument(arguments, 0, "intersectsNode")?;
                Ok(JsValue::Boolean(dom.range_intersects_node(range, node)?))
            }
            RangeOp::CloneRange => {
                let clone = dom.range_clone(range)?;
                self.wrap_range(dom, clone)
            }
            // `detach()` does nothing (DOM §5.2): a range is never released.
            RangeOp::Detach => Ok(JsValue::Undefined),
            RangeOp::DeleteContents => {
                dom.range_delete_contents(range)?;
                Ok(JsValue::Undefined)
            }
            RangeOp::ExtractContents => {
                let fragment = dom.range_extract_contents(range)?;
                self.wrap_node(dom, fragment)
            }
            RangeOp::CloneContents => {
                let fragment = dom.range_clone_contents(range)?;
                self.wrap_node(dom, fragment)
            }
            RangeOp::InsertNode => {
                let node = self.node_argument(arguments, 0, "insertNode")?;
                dom.range_insert_node(range, node)?;
                Ok(JsValue::Undefined)
            }
            RangeOp::SurroundContents => {
                let new_parent = self.node_argument(arguments, 0, "surroundContents")?;
                dom.range_surround_contents(range, new_parent)?;
                Ok(JsValue::Undefined)
            }
            RangeOp::ToString => {
                let text = dom.range_to_string(range)?;
                Ok(JsValue::String(text.into()))
            }
            // The getters and `createRange` are handled by `range_native`.
            RangeOp::StartContainer
            | RangeOp::StartOffset
            | RangeOp::EndContainer
            | RangeOp::EndOffset
            | RangeOp::Collapsed
            | RangeOp::CreateRange => {
                unreachable!("range_native dispatches the getters and createRange")
            }
        }
    }

    /// `new Range()` (DOM §5.2): a live range collapsed at the start of the
    /// global object's document.
    pub(in crate::runtime) fn range_constructor(
        &mut self,
        dom: &mut Dom,
    ) -> Result<JsValue, JsError> {
        let document_object = self.realm.document_object();
        let document = self.require_document(document_object)?;
        let range = dom.create_live_range(document)?;
        self.wrap_range(dom, range)
    }

    /// `new StaticRange(init)` (DOM §5.5). The dictionary members are read in
    /// the lexicographic order `WebIDL` §3.2.5 requires.
    pub(in crate::runtime) fn static_range_constructor(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let JsValue::Object(init) = required_argument(arguments, 0, "StaticRange")? else {
            return Err(JsError::type_error(
                "Failed to construct 'StaticRange': parameter 1 is not of type 'StaticRangeInit'",
            ));
        };
        let end_container = self.static_range_member(dom, *init, "endContainer")?;
        let end_offset = self.static_range_member(dom, *init, "endOffset")?;
        let start_container = self.static_range_member(dom, *init, "startContainer")?;
        let start_offset = self.static_range_member(dom, *init, "startOffset")?;
        let end = BoundaryPoint {
            node: self.value_as_node(&end_container)?,
            offset: self.offset_from_value(dom, &end_offset)?,
        };
        let start = BoundaryPoint {
            node: self.value_as_node(&start_container)?,
            offset: self.offset_from_value(dom, &start_offset)?,
        };
        let range = dom.create_static_range(start, end)?;
        self.wrap_range(dom, range)
    }

    /// A required `StaticRangeInit` member: `undefined` is a `TypeError`.
    fn static_range_member(
        &mut self,
        dom: &mut Dom,
        init: ObjectId,
        name: &str,
    ) -> Result<JsValue, JsError> {
        let value = self.get_value(dom, init, name)?;
        if matches!(value, JsValue::Undefined) {
            return Err(JsError::type_error(format!(
                "Failed to construct 'StaticRange': required member {name} is undefined"
            )));
        }
        Ok(value)
    }

    /// The range a wrapper object names, for any of the three interfaces.
    fn any_range(&self, object: ObjectId) -> Result<RangeId, JsError> {
        match self.realm.host(object) {
            Some(ObjectHost::Range(range)) => Ok(range),
            _ => Err(JsError::type_error("Illegal invocation: not a Range")),
        }
    }

    /// The live range a wrapper object names, for the methods of `Range`.
    fn live_range(&self, dom: &Dom, object: ObjectId) -> Result<RangeId, JsError> {
        let range = self.any_range(object)?;
        if dom.range_kind(range) == Some(RangeKind::Live) {
            Ok(range)
        } else {
            Err(JsError::type_error("Illegal invocation: not a live Range"))
        }
    }

    /// A `Range` argument of `compareBoundaryPoints` (`WebIDL` `Range`, so a
    /// `StaticRange` is a `TypeError`).
    fn source_range_argument(
        &mut self,
        dom: &Dom,
        arguments: &[JsValue],
        index: usize,
        method: &str,
    ) -> Result<RangeId, JsError> {
        match required_argument(arguments, index, method)? {
            JsValue::Object(object) => self.live_range(dom, *object),
            _ => Err(JsError::type_error(format!(
                "{method}: parameter {} is not of type 'Range'",
                index + 1
            ))),
        }
    }

    fn node_argument(
        &self,
        arguments: &[JsValue],
        index: usize,
        method: &str,
    ) -> Result<NodeId, JsError> {
        self.value_as_node(required_argument(arguments, index, method)?)
    }

    fn unsigned_long_argument(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
        index: usize,
        method: &str,
    ) -> Result<usize, JsError> {
        let value = required_argument(arguments, index, method)?;
        self.offset_from_value(dom, value)
    }

    fn unsigned_short_argument(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
        index: usize,
        method: &str,
    ) -> Result<u16, JsError> {
        let value = required_argument(arguments, index, method)?;
        let number = self.to_number_value(dom, value)?;
        Ok(unsigned_short(number))
    }

    fn offset_from_value(&mut self, dom: &mut Dom, value: &JsValue) -> Result<usize, JsError> {
        let number = self.to_number_value(dom, value)?;
        Ok(usize::try_from(unsigned_long(number)).unwrap_or(usize::MAX))
    }

    /// A new `Range` or `StaticRange` object for `range`.
    pub(in crate::runtime) fn wrap_range(
        &mut self,
        dom: &Dom,
        range: RangeId,
    ) -> Result<JsValue, JsError> {
        let interface = match dom.range_kind(range) {
            Some(RangeKind::Static) => "StaticRange",
            _ => "Range",
        };
        self.ensure_heap_capacity(1)?;
        Ok(JsValue::Object(self.realm.range_wrapper(range, interface)))
    }
}
