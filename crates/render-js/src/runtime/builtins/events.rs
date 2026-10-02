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
use crate::runtime::types::{EventFlags, Listener};
use crate::value::NativeFunction;
use crate::value::ObjectHost;
use render_dom::Dom;
use render_dom::NodeId;

/// The phases of an event's travel (DOM Standard §2.2 `eventPhase`).
const AT_TARGET: f64 = 2.0;
const CAPTURING_PHASE: f64 = 1.0;
const BUBBLING_PHASE: f64 = 3.0;

/// One stop on an event's path.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stop {
    Node(NodeId),
    Window,
}

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
            NativeFunction::EventStopPropagation => {
                self.event_flags
                    .entry(receiver)
                    .or_default()
                    .stop_propagation = true;
                self.realm.set_property(
                    receiver,
                    "cancelBubble".to_owned(),
                    JsValue::Boolean(true),
                );
                Ok(JsValue::Undefined)
            }
            NativeFunction::EventStopImmediatePropagation => {
                let flags = self.event_flags.entry(receiver).or_default();
                flags.stop_propagation = true;
                flags.stop_immediate = true;
                self.realm.set_property(
                    receiver,
                    "cancelBubble".to_owned(),
                    JsValue::Boolean(true),
                );
                Ok(JsValue::Undefined)
            }
            NativeFunction::EventComposedPath => {
                let path: Vec<JsValue> = self
                    .event_flags
                    .get(&receiver)
                    .map(|flags| {
                        flags
                            .path
                            .iter()
                            .map(|object| JsValue::Object(*object))
                            .collect()
                    })
                    .unwrap_or_default();
                Ok(JsValue::Object(self.create_array_from_values(&path)?))
            }
            NativeFunction::WindowAddEventListener => self.add_window_listener(receiver, arguments),
            NativeFunction::WindowRemoveEventListener => {
                self.remove_window_listener(receiver, arguments)
            }
            other => self.dispatch_array_native(dom, other, receiver, arguments),
        }
    }
}

impl JsRuntime {
    /// A new `Event` object with the standard data properties.
    #[allow(
        clippy::fn_params_excessive_bools,
        reason = "the four flags are the Event initialisation dictionary's own booleans"
    )]
    pub(in crate::runtime) fn create_event_object(
        &mut self,
        event_type: &str,
        bubbles: bool,
        cancelable: bool,
        composed: bool,
        trusted: bool,
    ) -> Result<ObjectId, JsError> {
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
            ("type", JsValue::String(event_type.to_owned())),
            ("bubbles", JsValue::Boolean(bubbles)),
            ("cancelable", JsValue::Boolean(cancelable)),
            ("composed", JsValue::Boolean(composed)),
            ("defaultPrevented", JsValue::Boolean(false)),
            ("eventPhase", JsValue::Number(0.0)),
            ("isTrusted", JsValue::Boolean(trusted)),
            ("timeStamp", JsValue::Number(Self::monotonic_now_ms())),
            ("target", JsValue::Null),
            ("srcElement", JsValue::Null),
            ("currentTarget", JsValue::Null),
            ("returnValue", JsValue::Boolean(true)),
            ("cancelBubble", JsValue::Boolean(false)),
        ] {
            self.realm.set_property(event, name.to_owned(), value);
        }
        Ok(event)
    }

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
        let flag = |runtime: &Self, name: &str| {
            options
                .and_then(|object| runtime.realm.get_property(object, name))
                .is_some_and(|value| value.is_truthy())
        };
        let event = self.create_event_object(
            &event_type,
            flag(self, "bubbles"),
            flag(self, "cancelable"),
            flag(self, "composed"),
            false,
        )?;
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

    /// The `callback` and options of an `addEventListener` /
    /// `removeEventListener` call. `Ok(None)` means "no callback": a no-op.
    fn listener_from_arguments(
        &self,
        arguments: &[JsValue],
        method: &str,
    ) -> Result<Option<Listener>, JsError> {
        let callback = match arguments.get(1) {
            None | Some(JsValue::Null | JsValue::Undefined) => return Ok(None),
            Some(JsValue::Object(object)) => *object,
            Some(_) => {
                return Err(JsError::type_error(format!(
                    "{method}: the callback provided is not a function or an object"
                )));
            }
        };
        let (mut capture, mut once, mut passive) = (false, false, false);
        match arguments.get(2) {
            Some(JsValue::Object(options)) => {
                let read = |name: &str| {
                    self.realm
                        .get_property(*options, name)
                        .is_some_and(|value| value.is_truthy())
                };
                capture = read("capture");
                once = read("once");
                passive = read("passive");
            }
            Some(other) => capture = other.is_truthy(),
            None => {}
        }
        Ok(Some(Listener {
            callback,
            capture,
            once,
            passive,
        }))
    }

    /// Insert unless an identical (callback, capture) registration exists.
    fn insert_listener(listeners: &mut Vec<Listener>, listener: Listener) {
        if !listeners.iter().any(|existing| {
            existing.callback == listener.callback && existing.capture == listener.capture
        }) {
            listeners.push(listener);
        }
    }

    pub(in crate::runtime) fn add_event_listener(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let target = self.event_target_node(receiver)?;
        let event_type = required_argument(arguments, 0, "addEventListener")?.to_js_string();
        let Some(listener) = self.listener_from_arguments(arguments, "addEventListener")? else {
            return Ok(JsValue::Undefined);
        };
        let listeners = self
            .event_listeners
            .entry(target)
            .or_default()
            .entry(event_type)
            .or_default();
        Self::insert_listener(listeners, listener);
        Ok(JsValue::Undefined)
    }

    pub(in crate::runtime) fn remove_event_listener(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let target = self.event_target_node(receiver)?;
        let event_type = required_argument(arguments, 0, "removeEventListener")?.to_js_string();
        let Some(listener) = self.listener_from_arguments(arguments, "removeEventListener")? else {
            return Ok(JsValue::Undefined);
        };
        if let Some(listeners) = self
            .event_listeners
            .get_mut(&target)
            .and_then(|listeners| listeners.get_mut(&event_type))
        {
            listeners.retain(|candidate| {
                !(candidate.callback == listener.callback && candidate.capture == listener.capture)
            });
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
        let Some(listener) = self.listener_from_arguments(arguments, "addEventListener")? else {
            return Ok(JsValue::Undefined);
        };
        let listeners = self.window_event_handlers.entry(event_type).or_default();
        Self::insert_listener(listeners, listener);
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
        let Some(listener) = self.listener_from_arguments(arguments, "removeEventListener")? else {
            return Ok(JsValue::Undefined);
        };
        if let Some(listeners) = self.window_event_handlers.get_mut(&event_type) {
            listeners.retain(|candidate| {
                !(candidate.callback == listener.callback && candidate.capture == listener.capture)
            });
        }
        Ok(JsValue::Undefined)
    }

    fn stop_wrapper(&mut self, dom: &Dom, stop: Stop) -> Result<ObjectId, JsError> {
        match stop {
            Stop::Window => Ok(self.realm.global_object()),
            Stop::Node(node) if node == dom.document() => Ok(self.realm.document_object()),
            Stop::Node(node) => {
                self.ensure_heap_capacity(1)?;
                let interface = super::dom::dom_interface_name(dom, node);
                Ok(self.realm.node_wrapper(node, interface))
            }
        }
    }

    fn stop_listeners(&self, stop: Stop, event_type: &str) -> Vec<Listener> {
        match stop {
            Stop::Window => self.window_event_handlers.get(event_type),
            Stop::Node(node) => self
                .event_listeners
                .get(&node)
                .and_then(|listeners| listeners.get(event_type)),
        }
        .cloned()
        .unwrap_or_default()
    }

    /// Whether `listener` is still registered on `stop`: a listener removed by
    /// an earlier listener of the same dispatch must not run.
    fn listener_is_registered(&self, stop: Stop, event_type: &str, listener: &Listener) -> bool {
        self.stop_listeners(stop, event_type)
            .iter()
            .any(|candidate| {
                candidate.callback == listener.callback && candidate.capture == listener.capture
            })
    }

    fn remove_listener_record(&mut self, stop: Stop, event_type: &str, listener: &Listener) {
        let listeners = match stop {
            Stop::Window => self.window_event_handlers.get_mut(event_type),
            Stop::Node(node) => self
                .event_listeners
                .get_mut(&node)
                .and_then(|listeners| listeners.get_mut(event_type)),
        };
        if let Some(listeners) = listeners {
            listeners.retain(|candidate| {
                !(candidate.callback == listener.callback && candidate.capture == listener.capture)
            });
        }
    }

    /// Run the listeners of one stop for one phase. `capture_pass` selects the
    /// capture-flagged ones; at the target both kinds run (capturing first).
    fn invoke_stop(
        &mut self,
        dom: &mut Dom,
        stop: Stop,
        event: ObjectId,
        event_type: &str,
        phase: f64,
        capture_pass: bool,
    ) -> Result<(), JsError> {
        let current = self.stop_wrapper(dom, stop)?;
        self.realm
            .set_property(event, "currentTarget".to_owned(), JsValue::Object(current));
        self.realm
            .set_property(event, "eventPhase".to_owned(), JsValue::Number(phase));
        for listener in self.stop_listeners(stop, event_type) {
            if listener.capture != capture_pass {
                continue;
            }
            if self
                .event_flags
                .get(&event)
                .is_some_and(|flags| flags.stop_immediate)
                || !self.listener_is_registered(stop, event_type, &listener)
            {
                continue;
            }
            if listener.once {
                self.remove_listener_record(stop, event_type, &listener);
            }
            self.call_listener(dom, &listener, event, current, event_type)?;
        }
        // The `on…` content/IDL attribute handler runs with the non-capturing
        // listeners.
        if !capture_pass
            && let Stop::Node(node) = stop
            && let Some(callback) = self
                .event_handlers
                .get(&node)
                .and_then(|handlers| handlers.get(event_type))
                .copied()
            && !self
                .event_flags
                .get(&event)
                .is_some_and(|flags| flags.stop_immediate)
        {
            let listener = Listener {
                callback,
                capture: false,
                once: false,
                passive: false,
            };
            self.call_listener(dom, &listener, event, current, event_type)?;
        }
        Ok(())
    }

    /// Call one listener. A listener that throws is reported and the dispatch
    /// carries on (DOM Standard §2.10 "inner invoke"); only an exhausted
    /// resource budget aborts it.
    fn call_listener(
        &mut self,
        dom: &mut Dom,
        listener: &Listener,
        event: ObjectId,
        current: ObjectId,
        event_type: &str,
    ) -> Result<(), JsError> {
        self.event_flags
            .entry(event)
            .or_default()
            .in_passive_listener = listener.passive;
        let outcome = if Self::is_callable_object(listener.callback, &self.realm) {
            self.call_with_this(
                dom,
                listener.callback,
                &[JsValue::Object(event)],
                JsValue::Object(current),
            )
            .map(|_| ())
        } else {
            match self.get_member(dom, listener.callback, "handleEvent") {
                Ok(JsValue::Object(method)) if Self::is_callable_object(method, &self.realm) => {
                    self.call_with_this(
                        dom,
                        method,
                        &[JsValue::Object(event)],
                        JsValue::Object(listener.callback),
                    )
                    .map(|_| ())
                }
                Ok(_) => Ok(()),
                Err(error) => Err(error),
            }
        };
        self.event_flags
            .entry(event)
            .or_default()
            .in_passive_listener = false;
        match outcome {
            Err(error) if error.kind() == crate::JsErrorKind::ResourceLimit => Err(error),
            Err(error) => {
                // An `error` listener that itself throws would recurse forever.
                if event_type != "error" {
                    self.report_uncaught_error(dom, &error);
                }
                Ok(())
            }
            Ok(()) => Ok(()),
        }
    }

    pub(in crate::runtime) fn dispatch_prepared_event(
        &mut self,
        dom: &mut Dom,
        target: NodeId,
        event: ObjectId,
        event_type: &str,
        bubbles: bool,
    ) -> Result<bool, JsError> {
        let target_wrapper = self.stop_wrapper(dom, Stop::Node(target))?;
        self.realm
            .set_property(event, "target".to_owned(), JsValue::Object(target_wrapper));
        self.realm.set_property(
            event,
            "srcElement".to_owned(),
            JsValue::Object(target_wrapper),
        );

        // The path: the target, its ancestors up to the document, and then the
        // window. An event aimed at the document itself without bubbling (the
        // embedder's `load`) still reaches the window, as it always has here.
        let mut path = vec![Stop::Node(target)];
        let mut ancestor = dom.parent(target);
        while let Some(node) = ancestor {
            path.push(Stop::Node(node));
            ancestor = dom.parent(node);
        }
        let attached = path
            .last()
            .is_some_and(|stop| *stop == Stop::Node(dom.document()));
        if attached {
            path.push(Stop::Window);
        }
        let bubbles_to_window = bubbles || target == dom.document();
        let mut wrappers = Vec::with_capacity(path.len());
        for stop in &path {
            wrappers.push(self.stop_wrapper(dom, *stop)?);
        }
        self.event_flags.insert(
            event,
            EventFlags {
                path: wrappers,
                ..EventFlags::default()
            },
        );

        let stopped = |runtime: &Self| {
            runtime
                .event_flags
                .get(&event)
                .is_some_and(|flags| flags.stop_propagation)
        };

        let result = (|| -> Result<(), JsError> {
            // Capture: from the outermost stop down to the target's parent.
            for stop in path.iter().skip(1).rev() {
                if stopped(self) {
                    return Ok(());
                }
                self.invoke_stop(dom, *stop, event, event_type, CAPTURING_PHASE, true)?;
            }
            // At the target: capturing listeners, then the others.
            if stopped(self) {
                return Ok(());
            }
            self.invoke_stop(dom, path[0], event, event_type, AT_TARGET, true)?;
            if stopped(self) {
                return Ok(());
            }
            self.invoke_stop(dom, path[0], event, event_type, AT_TARGET, false)?;
            // Bubble: outward, only for events that bubble.
            for stop in path.iter().skip(1) {
                if stopped(self) {
                    return Ok(());
                }
                if !(bubbles || bubbles_to_window && *stop == Stop::Window) {
                    continue;
                }
                self.invoke_stop(dom, *stop, event, event_type, BUBBLING_PHASE, false)?;
            }
            Ok(())
        })();

        self.event_flags.remove(&event);
        self.realm
            .set_property(event, "currentTarget".to_owned(), JsValue::Null);
        self.realm
            .set_property(event, "eventPhase".to_owned(), JsValue::Number(0.0));
        result?;
        let canceled = self
            .realm
            .get_property(event, "defaultPrevented")
            .is_some_and(|value| value.is_truthy());
        self.realm
            .set_property(event, "returnValue".to_owned(), JsValue::Boolean(!canceled));
        Ok(!canceled)
    }

    pub(in crate::runtime) fn event_prevent_default(&mut self, receiver: ObjectId) -> JsValue {
        let cancelable = self
            .realm
            .get_property(receiver, "cancelable")
            .is_some_and(|value| value.is_truthy());
        // Inside a passive listener, `preventDefault` is ignored (§2.10).
        let passive = self
            .event_flags
            .get(&receiver)
            .is_some_and(|flags| flags.in_passive_listener);
        if cancelable && !passive {
            self.realm.set_property(
                receiver,
                "defaultPrevented".to_owned(),
                JsValue::Boolean(true),
            );
        }
        JsValue::Undefined
    }
}
