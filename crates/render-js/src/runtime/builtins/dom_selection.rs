//! `Selection` (Selection API) and the two `getSelection()` entry points.
//!
//! A document owns one selection, and the selection owns at most one live
//! range. `render-dom` keeps that range on its mutation hooks, so removing,
//! inserting, splitting, and replacing data adjust the selection exactly as
//! they adjust a script-held `Range`. The anchor and focus are derived from the
//! range and the stored direction, which means a script that mutates the
//! range it passed to `addRange` also moves the selection, as the
//! specification requires.

use render_dom::{BoundaryPoint, Dom, NodeId};

use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::runtime::builtins::dom_exception::DomExceptionName;
use crate::runtime::convert::required_argument;
use crate::value::{ObjectHost, SelectionOp};

impl JsRuntime {
    /// Run one `Selection` operation, or `Document.getSelection()` /
    /// `Window.getSelection()`.
    #[allow(
        clippy::too_many_lines,
        reason = "one flat table of the selection operations reads better than fragments"
    )]
    pub(in crate::runtime) fn selection_native(
        &mut self,
        dom: &mut Dom,
        op: SelectionOp,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        if op == SelectionOp::GetSelection {
            let document = match self.realm.host(receiver) {
                Some(ObjectHost::Document(document)) => document,
                _ if receiver == self.realm.global_object() => {
                    let Some(document) = self.realm_document_node() else {
                        // A realm made by `CreateRealm` has no window and no
                        // document, so it has no selection.
                        return Ok(JsValue::Null);
                    };
                    document
                }
                _ => {
                    return Err(JsError::type_error("incompatible Document method receiver"));
                }
            };
            return self.document_selection(dom, document);
        }

        let document = match self.realm.host(receiver) {
            Some(ObjectHost::Selection(document)) => document,
            _ => return Err(JsError::type_error("Illegal invocation: not a Selection")),
        };
        match op {
            SelectionOp::AnchorNode => Ok(match dom.selection_anchor(document)? {
                Some(point) => self.wrap_node(dom, point.node)?,
                None => JsValue::Null,
            }),
            SelectionOp::AnchorOffset => Ok(offset_number(
                dom.selection_anchor(document)?
                    .map_or(0, |point| point.offset),
            )),
            SelectionOp::FocusNode => Ok(match dom.selection_focus(document)? {
                Some(point) => self.wrap_node(dom, point.node)?,
                None => JsValue::Null,
            }),
            SelectionOp::FocusOffset => Ok(offset_number(
                dom.selection_focus(document)?
                    .map_or(0, |point| point.offset),
            )),
            SelectionOp::IsCollapsed => Ok(JsValue::Boolean(dom.selection_is_collapsed(document)?)),
            SelectionOp::RangeCount => Ok(offset_number(dom.selection_range_count(document)?)),
            SelectionOp::Type => Ok(JsValue::String(dom.selection_type(document)?.into())),
            SelectionOp::Direction => Ok(JsValue::String(
                dom.selection_direction_name(document)?.into(),
            )),
            SelectionOp::GetRangeAt => {
                let value = required_argument(arguments, 0, "getRangeAt")?;
                let index = self.offset_from_value(dom, value)?;
                let Some(range) = dom.selection_range(document)? else {
                    return Err(self.dom_exception(
                        DomExceptionName::IndexSize,
                        "Selection.getRangeAt: the selection is empty",
                    ));
                };
                if index != 0 {
                    return Err(self.dom_exception(
                        DomExceptionName::IndexSize,
                        "Selection.getRangeAt: index must be 0",
                    ));
                }
                self.wrap_range(dom, range)
            }
            SelectionOp::AddRange => {
                let range = self.selection_range_argument(dom, arguments, 0, "addRange")?;
                dom.selection_add_range(document, range)?;
                Ok(JsValue::Undefined)
            }
            SelectionOp::RemoveRange => {
                let range = self.selection_range_argument(dom, arguments, 0, "removeRange")?;
                dom.selection_remove_range(document, range)?;
                Ok(JsValue::Undefined)
            }
            SelectionOp::RemoveAllRanges | SelectionOp::Empty => {
                dom.selection_remove_all_ranges(document)?;
                Ok(JsValue::Undefined)
            }
            SelectionOp::Collapse | SelectionOp::SetPosition => {
                let node = self.nullable_node_argument(arguments, 0)?;
                let offset = self.selection_offset_argument(dom, arguments, 1)?;
                dom.selection_collapse(document, node, offset)?;
                Ok(JsValue::Undefined)
            }
            SelectionOp::CollapseToStart => {
                dom.selection_collapse_to_start(document)?;
                Ok(JsValue::Undefined)
            }
            SelectionOp::CollapseToEnd => {
                dom.selection_collapse_to_end(document)?;
                Ok(JsValue::Undefined)
            }
            SelectionOp::Extend => {
                let node = self.selection_node_argument(arguments, 0, "extend")?;
                let offset = self.selection_offset_argument(dom, arguments, 1)?;
                dom.selection_extend(document, node, offset)?;
                Ok(JsValue::Undefined)
            }
            SelectionOp::SetBaseAndExtent => {
                let anchor_node = self.selection_node_argument(arguments, 0, "setBaseAndExtent")?;
                let anchor_offset = self.selection_offset_argument(dom, arguments, 1)?;
                let focus_node = self.selection_node_argument(arguments, 2, "setBaseAndExtent")?;
                let focus_offset = self.selection_offset_argument(dom, arguments, 3)?;
                dom.selection_set_base_and_extent(
                    document,
                    BoundaryPoint {
                        node: anchor_node,
                        offset: anchor_offset,
                    },
                    BoundaryPoint {
                        node: focus_node,
                        offset: focus_offset,
                    },
                )?;
                Ok(JsValue::Undefined)
            }
            SelectionOp::SelectAllChildren => {
                let node = self.selection_node_argument(arguments, 0, "selectAllChildren")?;
                dom.selection_select_all_children(document, node)?;
                Ok(JsValue::Undefined)
            }
            SelectionOp::DeleteFromDocument => {
                dom.selection_delete_from_document(document)?;
                Ok(JsValue::Undefined)
            }
            SelectionOp::ContainsNode => {
                let node = self.selection_node_argument(arguments, 0, "containsNode")?;
                let partial = arguments.get(1).is_some_and(JsValue::is_truthy);
                Ok(JsValue::Boolean(
                    dom.selection_contains_node(document, node, partial)?,
                ))
            }
            SelectionOp::ToString => Ok(JsValue::String(dom.selection_to_string(document)?.into())),
            SelectionOp::GetSelection => unreachable!("getSelection is dispatched above"),
        }
    }

    /// The live `Selection` object of `document`, made in the realm that owns
    /// the document, so `getSelection()` returns the same object every time.
    fn document_selection(&mut self, dom: &Dom, document: NodeId) -> Result<JsValue, JsError> {
        let realm = self.realm_of_node(dom, document);
        self.ensure_heap_capacity(1)?;
        Ok(JsValue::Object(self.with_realm(realm, |runtime| {
            runtime.realm.selection_wrapper(document)
        })))
    }

    /// A required `Node` argument.
    fn selection_node_argument(
        &self,
        arguments: &[JsValue],
        index: usize,
        method: &str,
    ) -> Result<NodeId, JsError> {
        self.value_as_node(required_argument(arguments, index, method)?)
    }

    /// A nullable `Node` argument: `null` and `undefined` are the null node.
    fn nullable_node_argument(
        &self,
        arguments: &[JsValue],
        index: usize,
    ) -> Result<Option<NodeId>, JsError> {
        match arguments.get(index) {
            None | Some(JsValue::Null | JsValue::Undefined) => Ok(None),
            Some(value) => Ok(Some(self.value_as_node(value)?)),
        }
    }

    /// An optional `unsigned long` argument, defaulting to zero.
    fn selection_offset_argument(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
        index: usize,
    ) -> Result<usize, JsError> {
        match arguments.get(index) {
            None | Some(JsValue::Undefined) => Ok(0),
            Some(value) => {
                let value = value.clone();
                self.offset_from_value(dom, &value)
            }
        }
    }

    /// A `Range` argument of `addRange` or `removeRange` (`WebIDL` `Range`, so
    /// a `StaticRange` is a `TypeError`).
    fn selection_range_argument(
        &mut self,
        dom: &Dom,
        arguments: &[JsValue],
        index: usize,
        method: &str,
    ) -> Result<render_dom::RangeId, JsError> {
        match required_argument(arguments, index, method)? {
            JsValue::Object(object) => self.live_range(dom, *object),
            _ => Err(JsError::type_error(format!(
                "{method}: parameter {} is not of type 'Range'",
                index + 1
            ))),
        }
    }
}

/// An `unsigned long` as a JavaScript number.
fn offset_number(offset: usize) -> JsValue {
    JsValue::Number(f64::from(u32::try_from(offset).unwrap_or(u32::MAX)))
}
