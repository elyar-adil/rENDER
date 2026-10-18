//! `NodeList` and `HTMLCollection` (DOM §4.2.10).
//!
//! A live collection holds no members of its own. Its members are the nodes
//! its [`CollectionFilter`] selects under its root, read from the tree when
//! they are asked for. The members are memoised together with the DOM revision
//! they were computed at, so an indexed loop over `collection[i]` walks the
//! tree once per mutation rather than once per index. A static `NodeList`
//! (`querySelectorAll`) holds its members from creation and never recomputes.

use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::runtime::convert::required_argument;
use crate::value::CollectionFilter;
use crate::value::ObjectHost;
use crate::value::is_canonical_index;
use render_dom::Dom;
use render_dom::Namespace;
use render_dom::NodeId;
use render_dom::NodeKind;

/// The nodes `filter` selects under `root`, in tree order (DOM §4.2.10.1).
pub(crate) fn collection_members(
    dom: &Dom,
    root: NodeId,
    filter: &CollectionFilter,
) -> Vec<NodeId> {
    match filter {
        CollectionFilter::ChildNodes => dom.children(root).unwrap_or_default().to_vec(),
        CollectionFilter::ChildElements => dom
            .children(root)
            .unwrap_or_default()
            .iter()
            .copied()
            .filter(|child| is_element(dom, *child))
            .collect(),
        CollectionFilter::NamedItem(key) => dom.named_elements(root, key),
        _ => descendants(dom, root)
            .into_iter()
            .filter(|node| is_element(dom, *node) && element_matches(dom, *node, filter))
            .collect(),
    }
}

/// Every descendant of `root` in tree order, `root` excluded.
fn descendants(dom: &Dom, root: NodeId) -> Vec<NodeId> {
    let mut result = Vec::new();
    let mut pending: Vec<NodeId> = dom
        .children(root)
        .unwrap_or_default()
        .iter()
        .rev()
        .copied()
        .collect();
    while let Some(node) = pending.pop() {
        result.push(node);
        if let Some(children) = dom.children(node) {
            pending.extend(children.iter().rev().copied());
        }
    }
    result
}

fn is_element(dom: &Dom, node: NodeId) -> bool {
    matches!(
        dom.node(node).map(render_dom::Node::kind),
        Some(NodeKind::Element(_))
    )
}

/// Whether element `node` satisfies an element-selecting `filter`. The node is
/// already known to be an element.
fn element_matches(dom: &Dom, node: NodeId, filter: &CollectionFilter) -> bool {
    let Some(NodeKind::Element(element)) = dom.node(node).map(render_dom::Node::kind) else {
        return false;
    };
    match filter {
        CollectionFilter::TagName(name) => {
            if name == "*" {
                return true;
            }
            // An HTML element in an HTML document matches its lowercase name
            // ASCII-case-insensitively (DOM §4.2.6.2); every other element
            // matches the name exactly.
            let qualified = element.qualified_name();
            if element.namespace == Namespace::Html {
                qualified == name.to_ascii_lowercase()
            } else {
                qualified == *name
            }
        }
        CollectionFilter::TagNameNs { namespace, local } => {
            let namespace_matches = namespace
                .as_deref()
                .is_none_or(|wanted| element.namespace.uri().unwrap_or("") == wanted);
            let local_matches = local
                .as_deref()
                .is_none_or(|wanted| element.local_name == wanted);
            namespace_matches && local_matches
        }
        CollectionFilter::ClassNames(wanted) => {
            if wanted.is_empty() {
                return false;
            }
            let classes = dom
                .attribute(node, "class")
                .ok()
                .flatten()
                .unwrap_or_default();
            wanted
                .iter()
                .all(|token| classes.split_ascii_whitespace().any(|class| class == token))
        }
        CollectionFilter::NamedItem(key) => dom.names_element(node, key),
        CollectionFilter::ChildNodes | CollectionFilter::ChildElements => false,
    }
}

/// `WebIDL` §3.2.4.7 `unsigned long`: the value modulo 2^32, with NaN and the
/// infinities as zero.
pub(crate) fn unsigned_long(number: f64) -> u32 {
    if !number.is_finite() {
        return 0;
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the value is reduced modulo 2^32 before the cast"
    )]
    let wrapped = number.trunc().rem_euclid(4_294_967_296.0) as u32;
    wrapped
}

/// The elements an `HTMLCollection.namedItem` can match by `name` attribute:
/// the HTML elements that carry a `name` the collection exposes by name
/// (HTML §4.2.5 and the named-access rules of DOM §4.2.10.2).
const NAMED_ITEM_ELEMENTS: &[&str] = &[
    "a", "applet", "area", "embed", "form", "frame", "frameset", "iframe", "img", "object",
];

impl JsRuntime {
    /// Recompute the members of a live collection if the DOM has changed since
    /// they were last computed. Returns `false` when `object` is not a live
    /// collection.
    fn refresh_live_collection(&mut self, dom: &Dom, object: ObjectId) -> bool {
        let revision = dom.mutation_revision();
        let (root, filter) = match self.realm.host_ref(object) {
            Some(ObjectHost::LiveCollection { root, filter, memo }) => {
                if matches!(memo, Some((seen, _)) if *seen == revision) {
                    return true;
                }
                (*root, filter.clone())
            }
            _ => return false,
        };
        let members = collection_members(dom, root, &filter);
        if let Some(ObjectHost::LiveCollection { memo, .. }) = self.realm.host_mut(object) {
            *memo = Some((revision, members));
        }
        true
    }

    /// The current members of a `NodeList` or `HTMLCollection`, borrowed from
    /// the object. `None` when `object` is neither kind.
    pub(crate) fn collection_members_of(
        &mut self,
        dom: &Dom,
        object: ObjectId,
    ) -> Option<&[NodeId]> {
        if !self.refresh_live_collection(dom, object) {
            return match self.realm.host_ref(object) {
                Some(ObjectHost::StaticNodeList(members)) => Some(members.as_slice()),
                _ => None,
            };
        }
        match self.realm.host_ref(object) {
            Some(ObjectHost::LiveCollection {
                memo: Some((_, members)),
                ..
            }) => Some(members.as_slice()),
            _ => None,
        }
    }

    /// The number of members of a `NodeList`, `HTMLCollection` or `DOMTokenList`,
    /// or `None` when `object` is none of them (the `length` getter, `WebIDL`
    /// §3.10.4).
    pub(crate) fn list_length(&mut self, dom: &Dom, object: ObjectId) -> Option<usize> {
        if let Some(ObjectHost::ClassList(node)) = self.realm.host_ref(object) {
            let node = *node;
            return Self::class_list_tokens(dom, node)
                .ok()
                .map(|tokens| tokens.len());
        }
        self.collection_members_of(dom, object)
            .map(<[render_dom::NodeId]>::len)
    }

    /// The indexed property `index` of a `NodeList`, `HTMLCollection` or
    /// `DOMTokenList`, or `None` past the end or on any other object (`WebIDL`
    /// §3.10.4 and §3.7.6).
    pub(crate) fn indexed_list_member(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        index: usize,
    ) -> Result<Option<JsValue>, JsError> {
        if let Some(ObjectHost::ClassList(node)) = self.realm.host_ref(object) {
            let node = *node;
            let token = Self::class_list_tokens(dom, node)?.into_iter().nth(index);
            return Ok(token.map(|token| JsValue::String((token).into())));
        }
        let member = self
            .collection_members_of(dom, object)
            .and_then(|members| members.get(index).copied());
        match member {
            Some(node) => Ok(Some(self.wrap_node(dom, node)?)),
            None => Ok(None),
        }
    }

    /// The `length` getter that `NodeList`, `HTMLCollection` and `DOMTokenList`
    /// share. A receiver of another kind is an incompatible-receiver `TypeError`.
    pub(crate) fn collection_length_native(
        &mut self,
        dom: &Dom,
        receiver: ObjectId,
    ) -> Result<JsValue, JsError> {
        match self.list_length(dom, receiver) {
            Some(length) => {
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "list sizes stay far below any precision boundary"
                )]
                let length = length as f64;
                Ok(JsValue::Number(length))
            }
            None => Err(JsError::type_error("incompatible list receiver")),
        }
    }

    /// The indexed and `length` members of a `NodeList` or `HTMLCollection`
    /// (`WebIDL` §3.10.4): `None` for any other property, which then resolves on
    /// the prototype chain as usual.
    pub(crate) fn collection_property(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        property: &str,
    ) -> Result<Option<JsValue>, JsError> {
        let index = if property == "length" {
            None
        } else if is_canonical_index(property) {
            property.parse::<usize>().ok()
        } else {
            return Ok(None);
        };
        let member = {
            let Some(members) = self.collection_members_of(dom, object) else {
                return Ok(None);
            };
            match index {
                None => {
                    #[allow(
                        clippy::cast_precision_loss,
                        reason = "collection sizes stay far below any precision boundary"
                    )]
                    let length = members.len() as f64;
                    return Ok(Some(JsValue::Number(length)));
                }
                Some(index) => members.get(index).copied(),
            }
        };
        match member {
            Some(node) => Ok(Some(self.wrap_node(dom, node)?)),
            // An out-of-range index is not a property at all, so it reads as
            // `undefined` without consulting the prototype chain.
            None => Ok(Some(JsValue::Undefined)),
        }
    }

    /// `NodeList.prototype.item` and `HTMLCollection.prototype.item`: the member
    /// at an index, or `null` past the end (`WebIDL` §3.10.4).
    pub(crate) fn collection_item(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let number = self.to_number_value(dom, required_argument(arguments, 0, "item")?)?;
        let index = unsigned_long(number) as usize;
        let member = self
            .collection_members_of(dom, receiver)
            .and_then(|members| members.get(index).copied());
        match member {
            Some(node) => self.wrap_node(dom, node),
            None => Ok(JsValue::Null),
        }
    }

    /// `HTMLCollection.prototype.namedItem`: the first member whose `id` is
    /// `key`, else the first named element whose `name` is `key`. The empty
    /// string names nothing (HTML §4.2.5).
    pub(crate) fn collection_named_item(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let key = required_argument(arguments, 0, "namedItem")?
            .to_js_string()
            .to_lossy();
        if key.is_empty() {
            return Ok(JsValue::Null);
        }
        let Some(members) = self.collection_members_of(dom, receiver) else {
            return Err(JsError::type_error(
                "incompatible HTMLCollection method receiver",
            ));
        };
        let members = members.to_vec();
        let by_id = members
            .iter()
            .copied()
            .find(|node| dom.attribute(*node, "id").ok().flatten() == Some(key.as_str()));
        let found = by_id.or_else(|| {
            members.iter().copied().find(|node| {
                matches!(
                    dom.node(*node).map(render_dom::Node::kind),
                    Some(NodeKind::Element(element))
                        if element.namespace == Namespace::Html
                            && NAMED_ITEM_ELEMENTS.contains(&element.local_name.as_str())
                ) && dom.attribute(*node, "name").ok().flatten() == Some(key.as_str())
            })
        });
        match found {
            Some(node) => self.wrap_node(dom, node),
            None => Ok(JsValue::Null),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::unsigned_long;

    #[test]
    fn unsigned_long_wraps_modulo_two_to_the_32() {
        assert_eq!(unsigned_long(0.0), 0);
        assert_eq!(unsigned_long(2.9), 2);
        assert_eq!(unsigned_long(-1.0), u32::MAX);
        assert_eq!(unsigned_long(4_294_967_296.0), 0);
        assert_eq!(unsigned_long(f64::NAN), 0);
        assert_eq!(unsigned_long(f64::INFINITY), 0);
    }
}
