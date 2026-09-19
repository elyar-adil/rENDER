#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::float_cmp,
    clippy::if_not_else,
    clippy::manual_let_else,
    clippy::map_unwrap_or,
    clippy::match_same_arms,
    clippy::needless_ifs,
    clippy::too_many_lines,
    clippy::wrong_self_convention
)]

use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::runtime::convert::required_argument;
use crate::value::NativeFunction;
use crate::value::ObjectHost;
use render_dom::Dom;
use render_dom::NodeId;

impl JsRuntime {
    pub(in crate::runtime) fn dispatch_events_native(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match function {
            NativeFunction::EventPreventDefault => Ok(self.event_prevent_default(receiver)),
            NativeFunction::WindowAddEventListener => self.add_window_listener(receiver, arguments),
            NativeFunction::WindowRemoveEventListener => {
                self.remove_window_listener(receiver, arguments)
            }
            other => self.dispatch_array_native(dom, other, receiver, arguments),
        }
    }
}

impl JsRuntime {
    pub(in crate::runtime) fn event_constructor(
        &mut self,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let event_type = required_argument(arguments, 0, "Event")?.to_js_string();
        if event_type.is_empty() {
            return Err(JsError::type_error("Event type must not be empty"));
        }
        let options = arguments.get(1).and_then(|value| match value {
            JsValue::Object(object) => Some(*object),
            _ => None,
        });
        let bubbles = options
            .and_then(|object| self.realm.get_property(object, "bubbles"))
            .is_some_and(|value| value.is_truthy());
        let cancelable = options
            .and_then(|object| self.realm.get_property(object, "cancelable"))
            .is_some_and(|value| value.is_truthy());
        let constructor = self
            .realm
            .global("Event")
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            })
            .ok_or_else(|| JsError::type_error("Event constructor is unavailable"))?;
        let prototype = self
            .realm
            .get_property(constructor, "prototype")
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            });
        self.ensure_heap_capacity(1)?;
        let event = self.realm.create_object(prototype);
        for (name, value) in [
            ("type", JsValue::String(event_type)),
            ("bubbles", JsValue::Boolean(bubbles)),
            ("cancelable", JsValue::Boolean(cancelable)),
            ("defaultPrevented", JsValue::Boolean(false)),
            ("target", JsValue::Null),
            ("currentTarget", JsValue::Null),
        ] {
            self.realm.set_property(event, name.to_owned(), value);
        }
        Ok(JsValue::Object(event))
    }

    pub(in crate::runtime) fn event_target_node(
        &self,
        receiver: ObjectId,
    ) -> Result<NodeId, JsError> {
        match self.realm.host(receiver) {
            Some(ObjectHost::Document(node) | ObjectHost::Node(node)) => Ok(node),
            _ => Err(JsError::type_error(
                "incompatible EventTarget method receiver",
            )),
        }
    }

    pub(in crate::runtime) fn add_event_listener(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let target = self.event_target_node(receiver)?;
        let event_type = required_argument(arguments, 0, "addEventListener")?.to_js_string();
        let callback = match arguments.get(1) {
            None | Some(JsValue::Null | JsValue::Undefined) => return Ok(JsValue::Undefined),
            Some(value) => Self::require_callable_object(value, &self.realm)?,
        };
        let callbacks = self
            .event_listeners
            .entry(target)
            .or_default()
            .entry(event_type)
            .or_default();
        if !callbacks.contains(&callback) {
            callbacks.push(callback);
        }
        Ok(JsValue::Undefined)
    }

    pub(in crate::runtime) fn remove_event_listener(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let target = self.event_target_node(receiver)?;
        let event_type = required_argument(arguments, 0, "removeEventListener")?.to_js_string();
        let Some(JsValue::Object(callback)) = arguments.get(1) else {
            return Ok(JsValue::Undefined);
        };
        if let Some(callbacks) = self
            .event_listeners
            .get_mut(&target)
            .and_then(|listeners| listeners.get_mut(&event_type))
        {
            callbacks.retain(|candidate| candidate != callback);
        }
        Ok(JsValue::Undefined)
    }

    pub(in crate::runtime) fn dispatch_event(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let target = self.event_target_node(receiver)?;
        let event = Self::require_object(required_argument(arguments, 0, "dispatchEvent")?)?;
        let event_type = self
            .realm
            .get_property(event, "type")
            .map(|value| value.to_js_string())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| JsError::type_error("dispatchEvent argument is not an Event"))?;
        let bubbles = self
            .realm
            .get_property(event, "bubbles")
            .is_some_and(|value| value.is_truthy());
        let not_canceled =
            self.dispatch_prepared_event(dom, target, event, &event_type, bubbles)?;
        Ok(JsValue::Boolean(not_canceled))
    }

    pub(in crate::runtime) fn add_window_listener(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        if receiver != self.realm.global_object() {
            return Err(JsError::type_error(
                "window listeners must be added on the global object",
            ));
        }
        let event_type = required_argument(arguments, 0, "addEventListener")?.to_js_string();
        let callback = Self::require_callable_object(
            required_argument(arguments, 1, "addEventListener")?,
            &self.realm,
        )?;
        self.window_event_handlers
            .entry(event_type)
            .or_default()
            .push(callback);
        Ok(JsValue::Undefined)
    }

    pub(in crate::runtime) fn remove_window_listener(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        if receiver != self.realm.global_object() {
            return Err(JsError::type_error(
                "window listeners must be removed from the global object",
            ));
        }
        let event_type = required_argument(arguments, 0, "removeEventListener")?.to_js_string();
        let callback = Self::require_callable_object(
            required_argument(arguments, 1, "removeEventListener")?,
            &self.realm,
        )?;
        if let Some(listeners) = self.window_event_handlers.get_mut(&event_type) {
            listeners.retain(|candidate| *candidate != callback);
        }
        Ok(JsValue::Undefined)
    }

    pub(in crate::runtime) fn dispatch_prepared_event(
        &mut self,
        dom: &mut Dom,
        target: NodeId,
        event: ObjectId,
        event_type: &str,
        bubbles: bool,
    ) -> Result<bool, JsError> {
        let receiver_wrapper = if target == dom.document() {
            self.realm.document_object()
        } else {
            self.ensure_heap_capacity(1)?;
            self.realm.node_wrapper(target)
        };
        self.realm.set_property(
            event,
            "target".to_owned(),
            JsValue::Object(receiver_wrapper),
        );

        let mut path = vec![target];
        if bubbles {
            let mut ancestor = dom.parent(target);
            while let Some(node) = ancestor {
                path.push(node);
                ancestor = dom.parent(node);
            }
        }
        for node in path {
            let current_target = if node == dom.document() {
                self.realm.document_object()
            } else {
                self.ensure_heap_capacity(1)?;
                self.realm.node_wrapper(node)
            };
            self.realm.set_property(
                event,
                "currentTarget".to_owned(),
                JsValue::Object(current_target),
            );
            let callbacks = self
                .event_listeners
                .get(&node)
                .and_then(|listeners| listeners.get(event_type))
                .cloned()
                .unwrap_or_default();
            for callback in callbacks {
                self.call_with_this(
                    dom,
                    callback,
                    &[JsValue::Object(event)],
                    JsValue::Object(current_target),
                )?;
            }
            if let Some(callback) = self
                .event_handlers
                .get(&node)
                .and_then(|handlers| handlers.get(event_type))
                .copied()
            {
                self.call_with_this(
                    dom,
                    callback,
                    &[JsValue::Object(event)],
                    JsValue::Object(current_target),
                )?;
            }
        }
        // Events bubble to the window object last.
        let window_callbacks = self
            .window_event_handlers
            .get(event_type)
            .cloned()
            .unwrap_or_default();
        for callback in window_callbacks {
            self.realm.set_property(
                event,
                "currentTarget".to_owned(),
                JsValue::Object(self.realm.global_object()),
            );
            self.call_with_this(
                dom,
                callback,
                &[JsValue::Object(event)],
                JsValue::Object(self.realm.global_object()),
            )?;
        }
        self.realm
            .set_property(event, "currentTarget".to_owned(), JsValue::Null);
        let canceled = self
            .realm
            .get_property(event, "defaultPrevented")
            .is_some_and(|value| value.is_truthy());
        Ok(!canceled)
    }

    pub(in crate::runtime) fn event_prevent_default(&mut self, receiver: ObjectId) -> JsValue {
        let cancelable = self
            .realm
            .get_property(receiver, "cancelable")
            .is_some_and(|value| value.is_truthy());
        if cancelable {
            self.realm.set_property(
                receiver,
                "defaultPrevented".to_owned(),
                JsValue::Boolean(true),
            );
        }
        JsValue::Undefined
    }
}
