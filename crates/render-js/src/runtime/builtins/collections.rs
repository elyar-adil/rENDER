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

use render_dom::Dom;
use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::runtime::convert::required_argument;
use crate::runtime::convert::same_value_zero;
use crate::value::CollectionKind;
use crate::value::NativeFunction;
use crate::value::ObjectHost;

impl JsRuntime {
    pub(in crate::runtime) fn dispatch_collections_native(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match function {
            NativeFunction::SetAttribute => {
                let node = self.require_node(receiver)?;
                let name = required_argument(arguments, 0, "setAttribute")?.to_js_string();
                let value = required_argument(arguments, 1, "setAttribute")?.to_js_string();
                dom.set_attribute(node, name, value)?;
                Ok(JsValue::Undefined)
            }
            NativeFunction::SetTimeout | NativeFunction::SetInterval => {
                self.register_timer(function, arguments)
            }
            NativeFunction::CollectionGet => self.collection_get(receiver, arguments),
            NativeFunction::CollectionSet => {
                let key = required_argument(arguments, 0, "Map.set")?.clone();
                let value = arguments.get(1).cloned().unwrap_or(JsValue::Undefined);
                self.collection_store(receiver, key, value, true)?;
                Ok(JsValue::Object(receiver))
            }
            NativeFunction::CollectionAdd => {
                let value = required_argument(arguments, 0, "Set.add")?.clone();
                self.collection_store(receiver, value.clone(), value, false)?;
                Ok(JsValue::Object(receiver))
            }
            NativeFunction::CollectionHas => self.collection_has(receiver, arguments),
            NativeFunction::CollectionDelete => self.collection_delete(receiver, arguments),
            NativeFunction::CollectionClear => self.collection_clear(receiver),
            NativeFunction::CollectionForEach => self.collection_for_each(dom, receiver, arguments),
            NativeFunction::CollectionKeys => {
                self.collection_iterator(receiver, CollectionView::Keys)
            }
            NativeFunction::CollectionValues => {
                self.collection_iterator(receiver, CollectionView::Values)
            }
            NativeFunction::CollectionEntries => {
                self.collection_iterator(receiver, CollectionView::Entries)
            }
            NativeFunction::CollectionIteratorNext => self.collection_iterator_next(receiver),
            other => self.dispatch_string_native(dom, other, receiver, arguments),
        }
    }
}

#[derive(Clone, Copy)]
pub(in crate::runtime) enum CollectionView {
    Keys,
    Values,
    Entries,
}

impl JsRuntime {
    pub(in crate::runtime) fn collection_constructor(
        &mut self,
        dom: &mut Dom,
        constructor: ObjectId,
        kind: CollectionKind,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        self.ensure_heap_capacity(1)?;
        let prototype = self
            .realm
            .get_property(constructor, "prototype")
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            });
        let collection = self.realm.collection(kind, prototype);
        let Some(iterable) = arguments.first() else {
            return Ok(JsValue::Object(collection));
        };
        if matches!(iterable, JsValue::Null | JsValue::Undefined) {
            return Ok(JsValue::Object(collection));
        }
        let iterable = Self::require_object(iterable)?;
        if !matches!(self.realm.host(iterable), Some(ObjectHost::Array)) {
            return Err(JsError::type_error(
                "collection constructor currently requires an Array iterable",
            ));
        }
        for item in self.array_elements_for(iterable) {
            if kind.is_map() {
                let pair = Self::require_object(&item)?;
                let key = self.get_member(dom, pair, "0")?;
                let value = self.get_member(dom, pair, "1")?;
                self.collection_store(collection, key, value, true)?;
            } else {
                self.collection_store(collection, item.clone(), item, false)?;
            }
        }
        Ok(JsValue::Object(collection))
    }

    pub(in crate::runtime) fn collection_get(
        &self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let key = required_argument(arguments, 0, "Map.get")?;
        let Some(ObjectHost::Collection { kind, entries }) = self.realm.host(receiver) else {
            return Err(JsError::type_error("incompatible Map receiver"));
        };
        if !kind.is_map() {
            return Err(JsError::type_error("Map.get called on a Set"));
        }
        Ok(entries
            .iter()
            .find(|(candidate, _)| same_value_zero(candidate, key))
            .map_or(JsValue::Undefined, |(_, value)| value.clone()))
    }

    pub(in crate::runtime) fn collection_store(
        &mut self,
        receiver: ObjectId,
        key: JsValue,
        value: JsValue,
        map_method: bool,
    ) -> Result<(), JsError> {
        let Some(ObjectHost::Collection { kind, .. }) = self.realm.host(receiver) else {
            return Err(JsError::type_error("incompatible collection receiver"));
        };
        if kind.is_map() != map_method {
            return Err(JsError::type_error(
                "collection method used with the wrong receiver",
            ));
        }
        if kind.is_weak() && !matches!(key, JsValue::Object(_)) {
            return Err(JsError::type_error("weak collection keys must be objects"));
        }
        let Some(ObjectHost::Collection { entries, .. }) = self.realm.host_mut(receiver) else {
            unreachable!("collection host checked above")
        };
        if let Some((_, stored)) = entries
            .iter_mut()
            .find(|(candidate, _)| same_value_zero(candidate, &key))
        {
            *stored = value;
        } else {
            entries.push((key, value));
        }
        Ok(())
    }

    pub(in crate::runtime) fn collection_has(
        &self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let key = required_argument(arguments, 0, "collection.has")?;
        let Some(ObjectHost::Collection { entries, .. }) = self.realm.host(receiver) else {
            return Err(JsError::type_error("incompatible collection receiver"));
        };
        Ok(JsValue::Boolean(
            entries
                .iter()
                .any(|(candidate, _)| same_value_zero(candidate, key)),
        ))
    }

    pub(in crate::runtime) fn collection_delete(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let key = required_argument(arguments, 0, "collection.delete")?;
        let Some(ObjectHost::Collection { entries, .. }) = self.realm.host_mut(receiver) else {
            return Err(JsError::type_error("incompatible collection receiver"));
        };
        let Some(index) = entries
            .iter()
            .position(|(candidate, _)| same_value_zero(candidate, key))
        else {
            return Ok(JsValue::Boolean(false));
        };
        entries.remove(index);
        Ok(JsValue::Boolean(true))
    }

    pub(in crate::runtime) fn collection_clear(
        &mut self,
        receiver: ObjectId,
    ) -> Result<JsValue, JsError> {
        let Some(ObjectHost::Collection { kind, entries }) = self.realm.host_mut(receiver) else {
            return Err(JsError::type_error("incompatible collection receiver"));
        };
        if kind.is_weak() {
            return Err(JsError::type_error("weak collections cannot be cleared"));
        }
        entries.clear();
        Ok(JsValue::Undefined)
    }

    pub(in crate::runtime) fn collection_for_each(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let callback = Self::require_callable_object(
            required_argument(arguments, 0, "collection.forEach")?,
            &self.realm,
        )?;
        let this_argument = arguments.get(1).cloned().unwrap_or(JsValue::Undefined);
        let Some(ObjectHost::Collection { kind, entries }) = self.realm.host(receiver) else {
            return Err(JsError::type_error("incompatible collection receiver"));
        };
        if kind.is_weak() {
            return Err(JsError::type_error("weak collections are not enumerable"));
        }
        for (key, value) in entries {
            let callback_arguments = if kind.is_map() {
                [value, key, JsValue::Object(receiver)]
            } else {
                [key.clone(), key, JsValue::Object(receiver)]
            };
            self.call_with_this(dom, callback, &callback_arguments, this_argument.clone())?;
        }
        Ok(JsValue::Undefined)
    }

    pub(in crate::runtime) fn collection_iterator(
        &mut self,
        receiver: ObjectId,
        view: CollectionView,
    ) -> Result<JsValue, JsError> {
        let Some(ObjectHost::Collection { kind, entries }) = self.realm.host(receiver) else {
            return Err(JsError::type_error("incompatible collection receiver"));
        };
        if kind.is_weak() {
            return Err(JsError::type_error("weak collections are not enumerable"));
        }
        let mut values = Vec::with_capacity(entries.len());
        for (key, value) in entries {
            values.push(match view {
                CollectionView::Keys => key,
                CollectionView::Values if kind.is_map() => value,
                CollectionView::Values => key,
                CollectionView::Entries => {
                    JsValue::Object(self.create_array_from_values(&[key, value])?)
                }
            });
        }
        self.ensure_heap_capacity(1)?;
        Ok(JsValue::Object(self.realm.collection_iterator(values)))
    }

    pub(in crate::runtime) fn collection_iterator_next(
        &mut self,
        receiver: ObjectId,
    ) -> Result<JsValue, JsError> {
        let value = {
            let Some(ObjectHost::CollectionIterator { values, index }) =
                self.realm.host_mut(receiver)
            else {
                return Err(JsError::type_error(
                    "incompatible collection iterator receiver",
                ));
            };
            let value = values.get(*index).cloned();
            *index = index.saturating_add(1);
            value
        };
        self.ensure_heap_capacity(1)?;
        let result = self.realm.create_ordinary_object();
        self.realm
            .set_property(result, "done".to_owned(), JsValue::Boolean(value.is_none()));
        self.realm.set_property(
            result,
            "value".to_owned(),
            value.unwrap_or(JsValue::Undefined),
        );
        Ok(JsValue::Object(result))
    }
}
