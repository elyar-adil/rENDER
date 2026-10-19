//! Form collections and form-control naming on the JS objects (HTML §3.1.5,
//! §4.10.2 to §4.10.4): `document.forms`, `form.elements`, `fieldset.elements`,
//! `form.length`, the indexed and named members of a form, `control.form`, and
//! `RadioNodeList.value`.
//!
//! Membership and naming are render-dom's rules (`Dom::form_control_members`,
//! `Dom::form_control_named`, `Dom::form_control_names`). This module gives them
//! JS objects. A control's form owner is read on demand, so removing a form from
//! the tree leaves its controls with no owner without any hook here.
//!
//! A form is `[LegacyOverrideBuiltIns]`: its named members answer before its
//! prototype chain, and only an own property shadows them. An `HTMLCollection`
//! is not, so its named members answer only where the prototype chain has no
//! property of that name. Both kinds of member are read-only, non-writable and
//! non-deletable, and the named ones are not enumerable (`WebIDL` §3.9).

use render_dom::{Dom, NodeId, NodeKind};

use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::runtime::builtins::dom_collections::NamedRule;
use crate::value::{CollectionFilter, ObjectHost, is_canonical_index};

/// An indexed or named member that an exotic object supplies under a property
/// name (`WebIDL` §3.9). Indexed members are enumerable; named ones are not.
pub(crate) struct ExoticMember {
    pub(crate) value: JsValue,
    pub(crate) enumerable: bool,
}

/// The `WebIDL` "unsigned long" range check for an indexed property name: a
/// canonical numeric string that is an array index.
fn index_of(name: &str) -> Option<usize> {
    if is_canonical_index(name) {
        name.parse::<usize>().ok()
    } else {
        None
    }
}

/// Whether `node` is an `input` in the Radio Button state (HTML §4.10.5.1.2),
/// and so a member a `RadioNodeList` reads and sets.
fn is_radio(dom: &Dom, node: NodeId) -> bool {
    matches!(
        dom.node(node).map(render_dom::Node::kind),
        Some(NodeKind::Element(element))
            if element.namespace == render_dom::Namespace::Html && element.local_name == "input"
    ) && dom
        .attribute(node, "type")
        .ok()
        .flatten()
        .is_some_and(|kind| kind.eq_ignore_ascii_case("radio"))
}

/// The `value` of a radio button: its `value` content attribute, or `on` when it
/// has none (HTML §4.10.5.4).
fn radio_value(dom: &Dom, node: NodeId) -> String {
    dom.attribute(node, "value")
        .ok()
        .flatten()
        .map_or_else(|| "on".to_owned(), str::to_owned)
}

impl JsRuntime {
    /// The `elements` collection of `form` (HTML §4.10.3), one object per form.
    fn form_controls_for(&mut self, form: NodeId) -> Result<ObjectId, JsError> {
        self.ensure_heap_capacity(1)?;
        Ok(self
            .realm
            .form_controls_wrapper(form, "HTMLFormControlsCollection"))
    }

    /// The indexed or named member that `object` supplies under `name`, or `None`
    /// when `name` is an ordinary property of `object`.
    ///
    /// A `form` supplies its controls by index, and by name (HTML §4.10.3). A
    /// named member of a live collection is its named item (HTML §4.2.10.2 and
    /// §4.10.3). A named member is supplied only when `object` has no own property
    /// of that name, and an `HTMLCollection`'s also only when its prototype chain
    /// has none.
    pub(crate) fn exotic_member(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        name: &str,
    ) -> Result<Option<ExoticMember>, JsError> {
        let form = match self.realm.host_ref(object) {
            Some(ObjectHost::Node(node)) if dom.is_form_element(*node) => Some(*node),
            Some(ObjectHost::LiveCollection { .. } | ObjectHost::StaticNodeList(_)) => None,
            _ => return Ok(None),
        };
        if let Some(index) = index_of(name) {
            let member = match form {
                Some(form) => {
                    let wrapper = self.form_controls_for(form)?;
                    self.collection_members_of(dom, wrapper)
                        .and_then(|members| members.get(index).copied())
                }
                None => self
                    .collection_members_of(dom, object)
                    .and_then(|members| members.get(index).copied()),
            };
            return match member {
                Some(node) => Ok(Some(ExoticMember {
                    value: self.wrap_node(dom, node)?,
                    enumerable: true,
                })),
                None => Ok(None),
            };
        }
        if name.is_empty() || self.realm.own_property(object, name).is_some() {
            return Ok(None);
        }
        if let Some(form) = form {
            let wrapper = self.form_controls_for(form)?;
            let value =
                self.collection_named_lookup(dom, wrapper, NamedRule::FormControls, name)?;
            return Ok(value.map(|value| ExoticMember {
                value,
                enumerable: false,
            }));
        }
        let Some(rule) = self.collection_named_rule(dom, object) else {
            return Ok(None);
        };
        if self.realm.get_property_with_origin(object, name).is_some() {
            return Ok(None);
        }
        let value = self.collection_named_lookup(dom, object, rule, name)?;
        Ok(value.map(|value| ExoticMember {
            value,
            enumerable: false,
        }))
    }

    /// The keys that `object`'s indexed and named members add to its own keys
    /// (`WebIDL` §3.9.4): the indices in order, then each name no own property
    /// shadows. Empty for an object with no such members.
    pub(crate) fn exotic_own_keys(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
    ) -> Result<Vec<String>, JsError> {
        let form = match self.realm.host_ref(object) {
            Some(ObjectHost::Node(node)) if dom.is_form_element(*node) => Some(*node),
            Some(ObjectHost::LiveCollection { .. } | ObjectHost::StaticNodeList(_)) => None,
            _ => return Ok(Vec::new()),
        };
        let count = match form {
            Some(form) => {
                let wrapper = self.form_controls_for(form)?;
                self.list_length(dom, wrapper).unwrap_or(0)
            }
            None => self.list_length(dom, object).unwrap_or(0),
        };
        let mut keys: Vec<String> = (0..count).map(|index| index.to_string()).collect();
        let names = match (form, self.collection_named_rule(dom, object)) {
            (Some(form), _) => dom.form_control_names(form),
            (None, Some(rule)) => self.collection_named_names(dom, object, rule),
            (None, None) => Vec::new(),
        };
        for name in names {
            if self.realm.own_property(object, &name).is_some()
                || (form.is_none() && self.realm.get_property_with_origin(object, &name).is_some())
            {
                continue;
            }
            keys.push(name);
        }
        Ok(keys)
    }

    /// The receiver of an accessor, which must be a node that `accept` takes.
    /// Any other receiver is an illegal invocation (`WebIDL` §3.7.6).
    fn node_receiver(
        &self,
        dom: &Dom,
        receiver: ObjectId,
        accept: impl Fn(&Dom, NodeId) -> bool,
    ) -> Result<NodeId, JsError> {
        match self.realm.host_ref(receiver) {
            Some(ObjectHost::Node(node)) if accept(dom, *node) => Ok(*node),
            _ => Err(JsError::type_error("Illegal invocation")),
        }
    }

    /// `HTMLFormElement.prototype.elements` (HTML §4.10.3).
    pub(crate) fn form_elements_native(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
    ) -> Result<JsValue, JsError> {
        let form = self.node_receiver(dom, receiver, Dom::is_form_element)?;
        Ok(JsValue::Object(self.form_controls_for(form)?))
    }

    /// `HTMLFormElement.prototype.length`: the number of controls of the form
    /// (HTML §4.10.3).
    pub(crate) fn form_length_native(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
    ) -> Result<JsValue, JsError> {
        let form = self.node_receiver(dom, receiver, Dom::is_form_element)?;
        let wrapper = self.form_controls_for(form)?;
        let length = self.list_length(dom, wrapper).unwrap_or(0);
        #[allow(
            clippy::cast_precision_loss,
            reason = "control counts stay far below any precision boundary"
        )]
        Ok(JsValue::Number(length as f64))
    }

    /// `HTMLFieldSetElement.prototype.elements`: the listed descendants of the
    /// fieldset, as an `HTMLCollection` (HTML §4.10.4).
    pub(crate) fn fieldset_elements_native(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
    ) -> Result<JsValue, JsError> {
        let fieldset = self.node_receiver(dom, receiver, |dom, node| {
            matches!(
                dom.node(node).map(render_dom::Node::kind),
                Some(NodeKind::Element(element))
                    if element.namespace == render_dom::Namespace::Html
                        && element.local_name == "fieldset"
            )
        })?;
        self.ensure_heap_capacity(1)?;
        Ok(JsValue::Object(
            self.realm.form_controls_wrapper(fieldset, "HTMLCollection"),
        ))
    }

    /// `form` on each listed element's prototype: the form owner the element
    /// has, or `null` (HTML §4.10.2).
    pub(crate) fn control_form_native(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
    ) -> Result<JsValue, JsError> {
        let control = self.node_receiver(dom, receiver, Dom::is_listed_element)?;
        match dom.form_owner(control) {
            Some(form) => self.wrap_node(dom, form),
            None => Ok(JsValue::Null),
        }
    }

    /// `Document.prototype.forms` (HTML §3.1.5): a live collection of the
    /// document's `form` elements, one object per document.
    pub(crate) fn document_forms_native(
        &mut self,
        _dom: &mut Dom,
        receiver: ObjectId,
    ) -> Result<JsValue, JsError> {
        let document = match self.realm.host_ref(receiver) {
            Some(ObjectHost::Document(document)) => *document,
            _ => return Err(JsError::type_error("Illegal invocation")),
        };
        self.ensure_heap_capacity(1)?;
        Ok(JsValue::Object(self.realm.document_forms_wrapper(document)))
    }

    /// The members of a `RadioNodeList`, which is a live collection of the controls
    /// a form named one key gave (HTML §4.10.3). Any other receiver is an illegal
    /// invocation.
    fn radio_node_list_members(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
    ) -> Result<Vec<NodeId>, JsError> {
        let named = matches!(
            self.realm.host_ref(receiver),
            Some(ObjectHost::LiveCollection {
                filter: CollectionFilter::FormControlName(_),
                ..
            })
        );
        if !named {
            return Err(JsError::type_error("Illegal invocation"));
        }
        Ok(self
            .collection_members_of(dom, receiver)
            .map(<[NodeId]>::to_vec)
            .unwrap_or_default())
    }

    /// `RadioNodeList.prototype.value` getter: the value of the first checked radio
    /// button in the list, or the empty string when none is checked (HTML
    /// §4.10.3). A checked button is one with a `checked` attribute.
    pub(crate) fn radio_node_list_value_native(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
    ) -> Result<JsValue, JsError> {
        let members = self.radio_node_list_members(dom, receiver)?;
        let value = members
            .into_iter()
            .find(|node| {
                is_radio(dom, *node) && dom.attribute(*node, "checked").ok().flatten().is_some()
            })
            .map_or_else(String::new, |node| radio_value(dom, node));
        Ok(JsValue::String(value.into()))
    }

    /// `RadioNodeList.prototype.value` setter: checks the first radio button in the
    /// list whose value is the new value, and unchecks the other radio buttons of
    /// the list that share its name, since they are one group. Setting a value no
    /// button has does nothing (HTML §4.10.3).
    pub(crate) fn radio_node_list_set_value_native(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let members = self.radio_node_list_members(dom, receiver)?;
        let wanted = arguments
            .first()
            .map(|value| value.to_js_string().to_lossy())
            .unwrap_or_default();
        let Some(chosen) = members
            .iter()
            .copied()
            .find(|node| is_radio(dom, *node) && radio_value(dom, *node) == wanted)
        else {
            return Ok(JsValue::Undefined);
        };
        let group = dom
            .attribute(chosen, "name")
            .ok()
            .flatten()
            .map(str::to_owned);
        for node in &members {
            let same_group = is_radio(dom, *node)
                && dom
                    .attribute(*node, "name")
                    .ok()
                    .flatten()
                    .map(str::to_owned)
                    == group;
            if *node != chosen && same_group {
                dom.remove_attribute(*node, "checked")?;
            }
        }
        dom.set_attribute(chosen, "checked", "")?;
        Ok(JsValue::Undefined)
    }
}

/// Whether a `[[DefineOwnProperty]]` of the indexed member `current` with the
/// partial descriptor is compatible with it (`WebIDL` §3.9.3): an indexed member
/// accepts only a restatement of its own data, with no change to its value,
/// writability, enumerability or configurability.
pub(crate) fn indexed_definition_is_compatible(
    current: &ExoticMember,
    partial: &super::object::PartialDescriptor,
) -> bool {
    let accessor = !matches!(partial.get, super::object::AccessorField::Absent)
        || !matches!(partial.set, super::object::AccessorField::Absent);
    !accessor
        && partial.writable != Some(true)
        && partial.enumerable != Some(false)
        && partial.configurable != Some(false)
        && partial
            .value
            .as_ref()
            .is_none_or(|value| *value == current.value)
}
