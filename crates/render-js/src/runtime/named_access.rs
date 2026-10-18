//! Named access on a `Window` and on a `Document` (HTML §7.4.4 and §3.1.5).
//!
//! The named properties of a window are the elements that
//! [`render_dom::Dom::named_elements`] names under the property name. The lookup
//! runs only once the ordinary chain of the object has no property of that name,
//! so a name that any ordinary property holds, including one on
//! `Object.prototype`, is shadowed. That matches the visibility rule of the
//! `WebIDL` named property visibility algorithm for reads, `in` and `typeof`.
//!
//! The engine has no `Window` interface object, so there is no named properties
//! object in the prototype chain to hold these properties. The lookup is made
//! at the global object (and at a document) instead, which is the one place the
//! ordinary chain is known to be exhausted.
//!
//! The named value is the element itself, except that a window whose child
//! browsing context is named by an `iframe` yields that frame's `WindowProxy`
//! (HTML §7.4.4, the nested browsing context named by the name). Several
//! matches give a live `HTMLCollection` of them in tree order. The named
//! properties are never own properties, so they are not enumerated and
//! `hasOwnProperty` reports them absent.

use render_dom::{Dom, NodeId, NodeKind};

use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::value::{CollectionFilter, ObjectHost};

impl JsRuntime {
    /// The document whose elements a named lookup on `object` searches, and
    /// whether `object` is a window. `None` for every other object: only a
    /// window's global object and a document are named.
    fn named_access_scope(&self, object: ObjectId) -> Option<(NodeId, bool)> {
        if let Some(ObjectHost::Document(document)) = self.realm.host(object) {
            return Some((document, false));
        }
        let realm = self.realm_of_global(object)?;
        match self.realm.host(self.realm.document_of(realm)) {
            Some(ObjectHost::Document(document)) => Some((document, true)),
            _ => None,
        }
    }

    /// The named property `key` of `object`, or `None` when `object` names no
    /// element `key`. The caller has already found no ordinary property `key`.
    pub(in crate::runtime) fn named_access_value(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        key: &str,
    ) -> Result<Option<JsValue>, JsError> {
        if key.is_empty() {
            return Ok(None);
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
}
