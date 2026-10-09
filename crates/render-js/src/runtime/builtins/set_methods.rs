//! The ES2025 Set methods (ECMA-262 24.2.4): `union`, `intersection`,
//! `difference`, `symmetricDifference`, `isSubsetOf`, `isSupersetOf` and
//! `isDisjointFrom`. Each takes a set-like `other`, which `GetSetRecord` reads
//! once: its `size`, `has` and `keys`, in that order.
//!
//! The receiver's entries are walked by position. A callback that deletes an
//! entry can make the walk skip the entry after it, because stored entries
//! have no holes to keep positions fixed. The collection iterators here behave
//! the same way.

#![allow(
    clippy::cast_precision_loss,
    clippy::float_cmp,
    clippy::manual_let_else,
    clippy::too_many_lines
)]

use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::runtime::convert::same_value_zero;
use crate::value::CollectionKind;
use crate::value::NativeFunction;
use crate::value::ObjectHost;
use render_dom::Dom;

/// A set-like argument, read once (ECMA-262 24.2.1.2 `GetSetRecord`).
struct SetRecord {
    object: ObjectId,
    size: f64,
    has: ObjectId,
    keys: ObjectId,
}

/// `-0` is stored as `+0`, as a Set's value is normalized (ECMA-262 24.2.4.1).
fn normalize_zero(value: JsValue) -> JsValue {
    if let JsValue::Number(number) = value
        && number == 0.0
    {
        return JsValue::Number(0.0);
    }
    value
}

fn contains(values: &[JsValue], value: &JsValue) -> bool {
    values
        .iter()
        .any(|candidate| same_value_zero(candidate, value))
}

impl JsRuntime {
    /// The Set the method was called on. Any other receiver, including a Map,
    /// is a `TypeError`.
    fn set_receiver(&self, receiver: ObjectId) -> Result<ObjectId, JsError> {
        let target = self.collection_target(receiver);
        match self.realm.host(target) {
            Some(ObjectHost::Collection {
                kind: CollectionKind::Set,
                ..
            }) => Ok(target),
            _ => Err(JsError::type_error(
                "Set method called on an incompatible receiver",
            )),
        }
    }

    fn set_size(&self, set: ObjectId) -> usize {
        match self.realm.host(set) {
            Some(ObjectHost::Collection { entries, .. }) => entries.len(),
            _ => 0,
        }
    }

    fn set_value_at(&self, set: ObjectId, index: usize) -> Option<JsValue> {
        match self.realm.host(set) {
            Some(ObjectHost::Collection { entries, .. }) => {
                entries.get(index).map(|(key, _)| key.clone())
            }
            _ => None,
        }
    }

    fn set_values(&self, set: ObjectId) -> Vec<JsValue> {
        match self.realm.host(set) {
            Some(ObjectHost::Collection { entries, .. }) => {
                entries.iter().map(|(key, _)| key.clone()).collect()
            }
            _ => Vec::new(),
        }
    }

    fn set_has(&self, set: ObjectId, value: &JsValue) -> bool {
        match self.realm.host(set) {
            Some(ObjectHost::Collection { entries, .. }) => {
                entries.iter().any(|(key, _)| same_value_zero(key, value))
            }
            _ => false,
        }
    }

    /// `GetSetRecord`: the `size`, `has` and `keys` of a set-like, each checked as
    /// the spec requires before the next is read.
    fn get_set_record(&mut self, dom: &mut Dom, other: &JsValue) -> Result<SetRecord, JsError> {
        let JsValue::Object(object) = other else {
            return Err(JsError::type_error(
                "Set method argument must be a set-like object",
            ));
        };
        let object = *object;
        let raw_size = self.get_member(dom, object, "size")?;
        let number = self.to_number_value(dom, &raw_size)?;
        if number.is_nan() {
            return Err(JsError::type_error("set-like size is not a number"));
        }
        let size = number.trunc();
        if size < 0.0 {
            return Err(self.range_error("set-like size is negative"));
        }
        let has = self.set_like_method(dom, object, "has")?;
        let keys = self.set_like_method(dom, object, "keys")?;
        Ok(SetRecord {
            object,
            size,
            has,
            keys,
        })
    }

    fn set_like_method(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        name: &str,
    ) -> Result<ObjectId, JsError> {
        match self.get_member(dom, object, name)? {
            JsValue::Object(method) if Self::is_callable_object(method, &self.realm) => Ok(method),
            _ => Err(JsError::type_error(format!(
                "set-like {name} is not a function"
            ))),
        }
    }

    /// `GetKeysIterator`: call `other.keys()` and read the iterator's `next`.
    fn set_keys_iterator(
        &mut self,
        dom: &mut Dom,
        record: &SetRecord,
    ) -> Result<(ObjectId, ObjectId), JsError> {
        let iterator =
            self.call_with_this(dom, record.keys, &[], JsValue::Object(record.object))?;
        let JsValue::Object(iterator) = iterator else {
            return Err(JsError::type_error(
                "set-like keys() did not return an object",
            ));
        };
        let next = self.set_like_method(dom, iterator, "next")?;
        Ok((iterator, next))
    }

    /// Whether `other.has(value)` is true, through the record's own `has`.
    fn set_record_has(
        &mut self,
        dom: &mut Dom,
        record: &SetRecord,
        value: &JsValue,
    ) -> Result<bool, JsError> {
        Ok(self
            .call_with_this(
                dom,
                record.has,
                std::slice::from_ref(value),
                JsValue::Object(record.object),
            )?
            .is_truthy())
    }

    /// Creates a Set holding `values`, which the caller has kept distinct.
    fn create_set(&mut self, values: Vec<JsValue>) -> Result<JsValue, JsError> {
        self.ensure_heap_capacity(1)?;
        let prototype = match self.realm.global("Set") {
            Some(JsValue::Object(constructor)) => self
                .realm
                .get_property(constructor, "prototype")
                .and_then(|value| match value {
                    JsValue::Object(object) => Some(object),
                    _ => None,
                }),
            _ => None,
        };
        let set = self.realm.collection(CollectionKind::Set, prototype);
        if let Some(ObjectHost::Collection { entries, .. }) = self.realm.host_mut(set) {
            entries.extend(values.into_iter().map(|value| (value.clone(), value)));
        }
        Ok(JsValue::Object(set))
    }

    /// Dispatches one of the seven Set methods on `receiver`.
    pub(in crate::runtime) fn dispatch_set_method(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let set = self.set_receiver(receiver)?;
        let other = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        match function {
            NativeFunction::CollectionUnion => self.set_union(dom, set, &other),
            NativeFunction::CollectionIntersection => self.set_intersection(dom, set, &other),
            NativeFunction::CollectionDifference => self.set_difference(dom, set, &other),
            NativeFunction::CollectionSymmetricDifference => {
                self.set_symmetric_difference(dom, set, &other)
            }
            NativeFunction::CollectionIsSubsetOf => self.set_is_subset_of(dom, set, &other),
            NativeFunction::CollectionIsSupersetOf => self.set_is_superset_of(dom, set, &other),
            NativeFunction::CollectionIsDisjointFrom => self.set_is_disjoint_from(dom, set, &other),
            _ => Err(JsError::type_error("not a Set method")),
        }
    }

    /// ECMA-262 24.2.4.14 `Set.prototype.union`.
    fn set_union(
        &mut self,
        dom: &mut Dom,
        set: ObjectId,
        other: &JsValue,
    ) -> Result<JsValue, JsError> {
        let record = self.get_set_record(dom, other)?;
        let (iterator, next) = self.set_keys_iterator(dom, &record)?;
        let mut result = self.set_values(set);
        while let Some(value) = self.iterator_next(dom, iterator, next)? {
            let value = normalize_zero(value);
            if !contains(&result, &value) {
                result.push(value);
            }
        }
        self.create_set(result)
    }

    /// ECMA-262 24.2.4.8 `Set.prototype.intersection`. It walks whichever of
    /// the two sides is smaller, so the `has` or `keys` calls are the fewest.
    fn set_intersection(
        &mut self,
        dom: &mut Dom,
        set: ObjectId,
        other: &JsValue,
    ) -> Result<JsValue, JsError> {
        let record = self.get_set_record(dom, other)?;
        let mut result = Vec::new();
        if (self.set_size(set) as f64) <= record.size {
            let mut index = 0;
            while index < self.set_size(set) {
                let Some(value) = self.set_value_at(set, index) else {
                    break;
                };
                index += 1;
                if self.set_record_has(dom, &record, &value)? && !contains(&result, &value) {
                    result.push(value);
                }
            }
        } else {
            let (iterator, next) = self.set_keys_iterator(dom, &record)?;
            while let Some(value) = self.iterator_next(dom, iterator, next)? {
                let value = normalize_zero(value);
                if self.set_has(set, &value) && !contains(&result, &value) {
                    result.push(value);
                }
            }
        }
        self.create_set(result)
    }

    /// ECMA-262 24.2.4.5 `Set.prototype.difference`.
    fn set_difference(
        &mut self,
        dom: &mut Dom,
        set: ObjectId,
        other: &JsValue,
    ) -> Result<JsValue, JsError> {
        let record = self.get_set_record(dom, other)?;
        let mut result = self.set_values(set);
        if (self.set_size(set) as f64) <= record.size {
            let mut index = 0;
            while index < self.set_size(set) {
                let Some(value) = self.set_value_at(set, index) else {
                    break;
                };
                index += 1;
                if self.set_record_has(dom, &record, &value)? {
                    result.retain(|candidate| !same_value_zero(candidate, &value));
                }
            }
        } else {
            let (iterator, next) = self.set_keys_iterator(dom, &record)?;
            while let Some(value) = self.iterator_next(dom, iterator, next)? {
                let value = normalize_zero(value);
                result.retain(|candidate| !same_value_zero(candidate, &value));
            }
        }
        self.create_set(result)
    }

    /// ECMA-262 24.2.4.12 `Set.prototype.symmetricDifference`.
    fn set_symmetric_difference(
        &mut self,
        dom: &mut Dom,
        set: ObjectId,
        other: &JsValue,
    ) -> Result<JsValue, JsError> {
        let record = self.get_set_record(dom, other)?;
        let (iterator, next) = self.set_keys_iterator(dom, &record)?;
        let mut result = self.set_values(set);
        while let Some(value) = self.iterator_next(dom, iterator, next)? {
            let value = normalize_zero(value);
            let position = result
                .iter()
                .position(|candidate| same_value_zero(candidate, &value));
            if self.set_has(set, &value) {
                if let Some(position) = position {
                    result.remove(position);
                }
            } else if position.is_none() {
                result.push(value);
            }
        }
        self.create_set(result)
    }

    /// ECMA-262 24.2.4.9 `Set.prototype.isSubsetOf`.
    fn set_is_subset_of(
        &mut self,
        dom: &mut Dom,
        set: ObjectId,
        other: &JsValue,
    ) -> Result<JsValue, JsError> {
        let record = self.get_set_record(dom, other)?;
        if (self.set_size(set) as f64) > record.size {
            return Ok(JsValue::Boolean(false));
        }
        let mut index = 0;
        while index < self.set_size(set) {
            let Some(value) = self.set_value_at(set, index) else {
                break;
            };
            index += 1;
            if !self.set_record_has(dom, &record, &value)? {
                return Ok(JsValue::Boolean(false));
            }
        }
        Ok(JsValue::Boolean(true))
    }

    /// ECMA-262 24.2.4.10 `Set.prototype.isSupersetOf`. Leaving early closes the
    /// keys iterator, as the spec's `IteratorClose` does.
    fn set_is_superset_of(
        &mut self,
        dom: &mut Dom,
        set: ObjectId,
        other: &JsValue,
    ) -> Result<JsValue, JsError> {
        let record = self.get_set_record(dom, other)?;
        if (self.set_size(set) as f64) < record.size {
            return Ok(JsValue::Boolean(false));
        }
        let (iterator, next) = self.set_keys_iterator(dom, &record)?;
        while let Some(value) = self.iterator_next(dom, iterator, next)? {
            if !self.set_has(set, &value) {
                self.close_iterator_object(dom, iterator)?;
                return Ok(JsValue::Boolean(false));
            }
        }
        Ok(JsValue::Boolean(true))
    }

    /// ECMA-262 24.2.4.11 `Set.prototype.isDisjointFrom`.
    fn set_is_disjoint_from(
        &mut self,
        dom: &mut Dom,
        set: ObjectId,
        other: &JsValue,
    ) -> Result<JsValue, JsError> {
        let record = self.get_set_record(dom, other)?;
        if (self.set_size(set) as f64) <= record.size {
            let mut index = 0;
            while index < self.set_size(set) {
                let Some(value) = self.set_value_at(set, index) else {
                    break;
                };
                index += 1;
                if self.set_record_has(dom, &record, &value)? {
                    return Ok(JsValue::Boolean(false));
                }
            }
        } else {
            let (iterator, next) = self.set_keys_iterator(dom, &record)?;
            while let Some(value) = self.iterator_next(dom, iterator, next)? {
                let value = normalize_zero(value);
                if self.set_has(set, &value) {
                    self.close_iterator_object(dom, iterator)?;
                    return Ok(JsValue::Boolean(false));
                }
            }
        }
        Ok(JsValue::Boolean(true))
    }
}
