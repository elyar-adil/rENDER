//! `focus()` and `blur()` on elements, and the focus update steps they run
//! (HTML §6.6.3 "focusing steps" and "unfocusing steps"), with the focus events
//! the update fires (UI Events §5.2: `focus` and `blur` do not bubble, and
//! `focusin` and `focusout` do).
//!
//! The focused area is document state, so it lives on [`Dom`] beside the
//! focusability rules. This module runs the steps that change it and fires the
//! events, in the order the spec requires, so a listener sees the focused area
//! the spec says it should see.

use render_dom::{Dom, NodeId};

use crate::JsError;
use crate::JsValue;
use crate::runtime::JsRuntime;

impl JsRuntime {
    /// `focus()` (HTML §6.6.4): run the focusing steps for `target`, unless it is
    /// not a focusable area or already holds focus, in which case nothing happens.
    pub(in crate::runtime) fn focus_element(
        &mut self,
        dom: &mut Dom,
        target: NodeId,
    ) -> Result<(), JsError> {
        if dom.focused_element() == Some(target) || !dom.is_focusable_area(target) {
            return Ok(());
        }
        self.update_focused_area(dom, Some(target))
    }

    /// `blur()` (HTML §6.6.4): if `target` holds focus, run the unfocusing steps,
    /// which make the viewport the focused area. Otherwise nothing happens.
    pub(in crate::runtime) fn blur_element(
        &mut self,
        dom: &mut Dom,
        target: NodeId,
    ) -> Result<(), JsError> {
        if dom.focused_element() != Some(target) {
            return Ok(());
        }
        self.update_focused_area(dom, None)
    }

    /// The focus update steps (HTML §6.6.3). The old focused area loses focus
    /// first (`blur`, then `focusout`), and the viewport is the focused area while
    /// those listeners run, so `document.activeElement` reads `body` there. Then
    /// the new target gains focus (`focus`, then `focusin`).
    ///
    /// A listener that moves focus itself wins: the update stops once the old
    /// area's events have run and the focused area is no longer the viewport, and
    /// a target that stopped being focusable during `blur` is not focused.
    fn update_focused_area(
        &mut self,
        dom: &mut Dom,
        new_target: Option<NodeId>,
    ) -> Result<(), JsError> {
        let old_target = dom.focused_element();
        if let Some(old) = old_target {
            dom.set_focused_element(None);
            // `relatedTarget` of the events that leave `old` is the element gaining focus.
            self.dispatch_focus_event(dom, old, "blur", false, new_target)?;
            self.dispatch_focus_event(dom, old, "focusout", true, new_target)?;
            if dom.focused_element().is_some() {
                return Ok(());
            }
        }
        let Some(target) = new_target else {
            return Ok(());
        };
        if !dom.set_focused_element(Some(target)) {
            return Ok(());
        }
        // `relatedTarget` of the events that reach `target` is the element that lost focus.
        self.dispatch_focus_event(dom, target, "focus", false, old_target)?;
        self.dispatch_focus_event(dom, target, "focusin", true, old_target)
    }

    /// Dispatch one trusted, non-cancelable `FocusEvent` at `target`, with
    /// `related` as its `relatedTarget` (`null` when there is none).
    fn dispatch_focus_event(
        &mut self,
        dom: &mut Dom,
        target: NodeId,
        event_type: &str,
        bubbles: bool,
        related: Option<NodeId>,
    ) -> Result<(), JsError> {
        let event =
            self.create_event_object_of("FocusEvent", event_type, bubbles, false, true, true)?;
        let related = match related {
            Some(node) => self.wrap_node(dom, node)?,
            None => JsValue::Null,
        };
        self.realm
            .set_property(event, "relatedTarget".to_owned(), related);
        self.dispatch_prepared_event(dom, target, event, event_type, bubbles)?;
        Ok(())
    }
}
