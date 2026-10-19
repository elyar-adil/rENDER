//! Named access on a `Window` and on a `Document` (HTML §7.4.4 and §3.1.5).
//!
//! The named properties of a window are the elements that
//! [`render_dom::Dom::named_elements`] names under the property name. They live
//! on the window's named properties object (`WindowProperties`, `WebIDL` §3.9),
//! which sits on the prototype chain between `Window.prototype` and
//! `EventTarget.prototype`:
//!
//! ```text
//! window -> Window.prototype -> WindowProperties -> EventTarget.prototype -> Object.prototype
//! ```
//!
//! A name is visible on that object only when no object beyond it has an own
//! property of the same name (the `WebIDL` named property visibility algorithm).
//! So a name that `EventTarget.prototype` or `Object.prototype` holds is shadowed
//! for reads, `in`, `typeof` and `hasOwnProperty`, and an own or inherited
//! property above the object shadows it too. A read therefore consults the
//! named properties only once the ordinary chain has no property of the name,
//! which is the same answer the visibility rule gives.
//!
//! A document names its elements on the document object itself (HTML §3.1.5).
//!
//! The named value is the element itself, except that a window whose child
//! browsing context is named by an `iframe` yields that frame's `WindowProxy`
//! (HTML §7.4.4, the nested browsing context named by the name). Several
//! matches give a live `HTMLCollection` of them in tree order. The named
//! properties are not own properties of the window, so they are not enumerated
//! and `hasOwnProperty` reports them absent on the window. They are own
//! properties of the named properties object, writable, not enumerable and
//! configurable, as `WebIDL` §3.9 requires. A document's named properties are
//! never own properties, so `hasOwnProperty` reports them absent too. The named
//! items of a form and of a collection are own, read-only properties instead;
//! see `builtins/dom_forms.rs`. A live collection's named items are answered
//! here too, by its own rule (`collection_named_rule`).

use render_dom::{Dom, NodeId, NodeKind};

use crate::JsError;
use crate::JsObject;
use crate::JsValue;
use crate::ObjectId;
use crate::PropertyDescriptor;
use crate::runtime::JsRuntime;
use crate::value::{CollectionFilter, ObjectHost};

impl JsRuntime {
    /// The document whose elements a named lookup on `object` searches, and
    /// whether the lookup is a window's. A document answers for itself. A window
    /// answers through its named properties object, which the lookup reaches by
    /// walking the prototype chain from `object`, so the global object, the
    /// `Window.prototype` and the named properties object all search the same
    /// document. `None` for every other object.
    fn named_access_scope(&self, object: ObjectId) -> Option<(NodeId, bool)> {
        if let Some(ObjectHost::Document(document)) = self.realm.host_ref(object) {
            return Some((*document, false));
        }
        let mut current = Some(object);
        while let Some(candidate) = current {
            match self.realm.host_ref(candidate) {
                Some(ObjectHost::WindowProperties(Some(document))) => {
                    return Some((*document, true));
                }
                Some(ObjectHost::Proxy { .. }) => return None,
                _ => {}
            }
            current = self.realm.object(candidate).and_then(JsObject::prototype);
        }
        None
    }

    /// The named property `key` of `object`, or `None` when `object` names no
    /// element `key`. The caller has already found no ordinary property `key`
    /// on the chain above the named properties object.
    pub(in crate::runtime) fn named_access_value(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        key: &str,
    ) -> Result<Option<JsValue>, JsError> {
        if key.is_empty() {
            return Ok(None);
        }
        // A live collection names its items by their rule (HTML §4.2.10.2 and
        // §4.10.3), which is not the window's or the document's.
        if let Some(rule) = self.collection_named_rule(dom, object) {
            return self.collection_named_lookup(dom, object, rule, key);
        }
        let Some((document, is_window)) = self.named_access_scope(object) else {
            return Ok(None);
        };
        if is_window {
            // The first nested browsing context whose name is `key` wins over
            // every element (HTML §7.4.4). Its name is the `name` attribute of
            // the `iframe` that embeds it, and a frame with no context is skipped.
            for element in dom.named_elements(document, key) {
                let names_frame = matches!(
                    dom.node(element).map(render_dom::Node::kind),
                    Some(NodeKind::Element(data)) if data.local_name == "iframe"
                ) && dom.attribute(element, "name").ok().flatten() == Some(key);
                if !names_frame {
                    continue;
                }
                let content = self.frame_content(dom, element, false);
                if matches!(content, JsValue::Object(_)) {
                    return Ok(Some(content));
                }
            }
        }
        let elements = dom.named_elements(document, key);
        match elements.as_slice() {
            [] => Ok(None),
            [element] => self.wrap_node(dom, *element).map(Some),
            _ => {
                self.ensure_heap_capacity(1)?;
                Ok(Some(JsValue::Object(self.realm.live_collection_wrapper(
                    document,
                    CollectionFilter::NamedItem(key.to_owned()),
                    "HTMLCollection",
                ))))
            }
        }
    }

    /// Whether the named property `key` of the named properties object `object`
    /// is visible (`WebIDL` §3.9, named property visibility): no object beyond it
    /// on its prototype chain has an own property `key`.
    fn window_named_visible(&self, object: ObjectId, key: &str) -> bool {
        let mut current = self.realm.object(object).and_then(JsObject::prototype);
        while let Some(candidate) = current {
            if self.realm.own_property(candidate, key).is_some() {
                return false;
            }
            current = self.realm.object(candidate).and_then(JsObject::prototype);
        }
        true
    }

    /// The own property descriptor that the named properties object `object`
    /// has for `key`, when `key` is a visible named property. It is a writable,
    /// non-enumerable, configurable data property (`WebIDL` §3.9). `None` for any
    /// other object, and for a name that is not visible or names nothing.
    pub(in crate::runtime) fn window_named_descriptor(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        key: &str,
    ) -> Result<Option<PropertyDescriptor>, JsError> {
        if !matches!(
            self.realm.host_ref(object),
            Some(ObjectHost::WindowProperties(Some(_)))
        ) || !self.window_named_visible(object, key)
        {
            return Ok(None);
        }
        Ok(self
            .named_access_value(dom, object, key)?
            .map(|value| PropertyDescriptor {
                getter: None,
                setter: None,
                value,
                writable: true,
                enumerable: false,
                configurable: true,
            }))
    }

    /// The names of the visible named properties of the named properties object
    /// `object`, in key order, for its `[[OwnPropertyKeys]]`. A name that an own
    /// property of the object already holds is not repeated.
    pub(in crate::runtime) fn window_named_keys(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
    ) -> Result<Vec<String>, JsError> {
        let Some(ObjectHost::WindowProperties(Some(document))) = self.realm.host(object) else {
            return Ok(Vec::new());
        };
        let mut keys = Vec::new();
        for key in dom.named_keys(document) {
            if self.realm.own_property(object, &key).is_none()
                && self.window_named_visible(object, &key)
                && self.named_access_value(dom, object, &key)?.is_some()
            {
                keys.push(key);
            }
        }
        Ok(keys)
    }
}
