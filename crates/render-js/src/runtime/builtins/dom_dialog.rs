//! `HTMLDialogElement` (HTML §4.11.2): `show`, `showModal`, `close`,
//! `requestClose` and the `returnValue` IDL attribute.
//!
//! The `open` IDL attribute reflects through the ordinary reflection table.
//! `returnValue` is not a reflected attribute — it is internal state, which is
//! also what its tests require: `close()` sets the IDL value without invoking a
//! script-defined `returnValue` setter on the prototype or the instance.
//!
//! The `close` event is queued, not dispatched synchronously (HTML §4.11.2:
//! "queue an element task on the DOM manipulation task source"), so a test can
//! observe that it has not run yet. The queue here is the engine's microtask
//! queue, which the embedding drains; that is earlier than the DOM
//! manipulation task's position in a real event loop, and every WPT assertion
//! this engine can observe holds either way. The `cancel` event of
//! `requestClose` dispatches synchronously, as the standard's "fire an event"
//! means.

use render_dom::{Dom, NodeId};

use crate::JsError;
use crate::JsValue;
use crate::runtime::JsRuntime;
use crate::runtime::builtins::dom_exception::DomExceptionName;
use crate::runtime::convert::required_argument;
use crate::value::NativeFunction;

impl JsRuntime {
    /// One operation of `HTMLDialogElement`.
    pub(in crate::runtime) fn dialog_native(
        &mut self,
        dom: &mut Dom,
        op: NativeFunction,
        receiver: NodeId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let dialog = Self::dialog_element(dom, receiver, op)?;
        match op {
            NativeFunction::DialogShow => self.dialog_show(dom, dialog, false),
            NativeFunction::DialogShowModal => self.dialog_show(dom, dialog, true),
            NativeFunction::DialogClose => self.dialog_close(dom, dialog, arguments, false),
            NativeFunction::DialogRequestClose => self.dialog_close(dom, dialog, arguments, true),
            NativeFunction::DialogReturnValueGet => {
                Ok(JsValue::String(self.dialog_return_value(dialog).into()))
            }
            NativeFunction::DialogReturnValueSet => {
                let value = required_argument(arguments, 0, "returnValue")?
                    .to_js_string()
                    .to_lossy();
                self.dialog_return_values.insert(dialog, value);
                Ok(JsValue::Undefined)
            }
            _ => unreachable!("dialog_native only dispatches the dialog operations"),
        }
    }

    /// The dialog element a method was called on. A method that reaches here
    /// through a non-dialog element is the platform's "illegal invocation".
    fn dialog_element(dom: &Dom, receiver: NodeId, op: NativeFunction) -> Result<NodeId, JsError> {
        let name = match op {
            NativeFunction::DialogShow => "show",
            NativeFunction::DialogShowModal => "showModal",
            NativeFunction::DialogClose => "close",
            NativeFunction::DialogRequestClose => "requestClose",
            NativeFunction::DialogReturnValueGet | NativeFunction::DialogReturnValueSet => {
                "returnValue"
            }
            _ => unreachable!("dialog_native only dispatches the dialog operations"),
        };
        if crate::runtime::nested::is_html_element(dom, receiver, "dialog") {
            Ok(receiver)
        } else {
            Err(JsError::type_error(format!(
                "Failed to execute '{name}' on 'HTMLDialogElement': \
                 illegal invocation"
            )))
        }
    }

    /// `show()` and `showModal()`: both add the `open` attribute; the modal
    /// form refuses an already-open dialog and a disconnected one, because
    /// its next steps (the top layer) need the document. The dialog focusing
    /// steps are a stated gap: this engine's focus model has no viewport.
    fn dialog_show(
        &mut self,
        dom: &mut Dom,
        dialog: NodeId,
        modal: bool,
    ) -> Result<JsValue, JsError> {
        let name = if modal { "showModal" } else { "show" };
        if dom.attribute(dialog, "open")?.is_some() {
            if modal {
                return Err(self.dom_exception(
                    DomExceptionName::InvalidState,
                    format!(
                        "Failed to execute '{name}' on 'HTMLDialogElement': The element \
                         already has an 'open' attribute, and therefore cannot be opened \
                         modally."
                    ),
                ));
            }
            return Ok(JsValue::Undefined);
        }
        if modal && !Self::dialog_is_connected(dom, dialog) {
            return Err(self.dom_exception(
                DomExceptionName::InvalidState,
                format!(
                    "Failed to execute '{name}' on 'HTMLDialogElement': The element is not \
                     in a Document."
                ),
            ));
        }
        dom.set_attribute(dialog, "open", String::new())?;
        Ok(JsValue::Undefined)
    }

    /// `close(returnValue)` and `requestClose(returnValue)`. Both only act on
    /// an open dialog; both keep the previous `returnValue` when called
    /// without an argument. `requestClose` asks first with a synchronous
    /// `cancel` event, and a prevented cancel keeps the dialog open.
    fn dialog_close(
        &mut self,
        dom: &mut Dom,
        dialog: NodeId,
        arguments: &[JsValue],
        request: bool,
    ) -> Result<JsValue, JsError> {
        if dom.attribute(dialog, "open")?.is_none() {
            return Ok(JsValue::Undefined);
        }
        let return_value = match arguments.first() {
            None | Some(JsValue::Undefined) => None,
            Some(value) => Some(value.to_js_string().to_lossy()),
        };
        if request {
            // `dispatch_dom_event` answers whether the default action
            // survives, so a prevented cancel keeps the dialog open.
            let default_allowed =
                self.dispatch_dom_event(dom, dialog, "cancel", false, true, false, &[])?;
            if !default_allowed {
                return Ok(JsValue::Undefined);
            }
        }
        dom.remove_attribute(dialog, "open")?;
        if let Some(return_value) = return_value {
            self.dialog_return_values.insert(dialog, return_value);
        }
        // The close event is queued, so a caller can observe that it has not
        // run yet; see the module notes on the queue's timing.
        self.pending_microtasks
            .push(crate::runtime::types::JsMicrotask::DialogClosed(dialog));
        Ok(JsValue::Undefined)
    }

    /// The `returnValue` IDL value, which defaults to the empty string.
    fn dialog_return_value(&self, dialog: NodeId) -> String {
        self.dialog_return_values
            .get(&dialog)
            .cloned()
            .unwrap_or_default()
    }

    /// Whether `node`'s shadow-including root is a document of this arena.
    fn dialog_is_connected(dom: &Dom, node: NodeId) -> bool {
        let mut current = Some(node);
        while let Some(node) = current {
            if node == dom.document() {
                return true;
            }
            current = dom.parent(node);
        }
        false
    }
}
