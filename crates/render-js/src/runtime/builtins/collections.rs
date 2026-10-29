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
use crate::runtime::convert::same_value_zero;
use crate::value::CollectionKind;
use crate::value::NativeFunction;
use crate::value::ObjectHost;
use render_dom::Dom;

/// The entries of a collection, in insertion order.
type Entries = Vec<(JsValue, JsValue)>;

/// The groups `Map.groupBy` builds: each distinct key with the items it groups.
type Groups = Vec<(JsValue, Vec<JsValue>)>;

/// The first argument, or `undefined` when the call has none.
fn first_argument(arguments: &[JsValue]) -> JsValue {
    arguments.first().cloned().unwrap_or(JsValue::Undefined)
}

/// `-0` as a key or a set member is stored as `+0` (ECMA-262 24.1.3.9 step 4,
/// 24.2.3.1 step 5 and 24.1.2.1 step 6.e).
fn normalize_zero(value: JsValue) -> JsValue {
    if let JsValue::Number(number) = value
        && number == 0.0
    {
        return JsValue::Number(0.0);
    }
    value
}

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
                let mut name = required_argument(arguments, 0, "setAttribute")?
                    .to_js_string()
                    .to_lossy();
                let value = required_argument(arguments, 1, "setAttribute")?.to_js_string();
                // Legacy DOM compatibility: a few older virtual-DOM builds
                // (including the Preact copy Bing serves to clients without
                // its homepage bundle) pass the IDL name `className` to
                // `setAttribute`. HTML attribute names are case-insensitive
                // and the standard stores that as `classname`, but every
                // browser-side intent behind this spelling is the class
                // attribute. Keep lower-case `classname` untouched.
                if name == "className"
                    && matches!(
                        dom.node(node).map(render_dom::Node::kind),
                        Some(render_dom::NodeKind::Element(element))
                            if element.namespace == render_dom::Namespace::Html
                    )
                {
                    name.clear();
                    name.push_str("class");
                }
                dom.set_attribute(node, name, value.to_lossy())?;
                Ok(JsValue::Undefined)
            }
            NativeFunction::SetTimeout | NativeFunction::SetInterval => {
                self.register_timer(dom, function, arguments)
            }
            NativeFunction::CollectionGet(brand) => self.collection_get(receiver, brand, arguments),
            NativeFunction::CollectionSet(brand) => {
                let key = first_argument(arguments);
                let value = arguments.get(1).cloned().unwrap_or(JsValue::Undefined);
                self.collection_store(receiver, brand, key, value)?;
                Ok(JsValue::Object(receiver))
            }
            NativeFunction::CollectionGetOrInsert(brand) => {
                let key = first_argument(arguments);
                let value = arguments.get(1).cloned().unwrap_or(JsValue::Undefined);
                self.collection_get_or_insert(receiver, brand, key, value)
            }
            NativeFunction::CollectionGetOrInsertComputed(brand) => {
                let key = first_argument(arguments);
                let callback = arguments.get(1).cloned().unwrap_or(JsValue::Undefined);
                self.collection_get_or_insert_computed(dom, receiver, brand, key, &callback)
            }
            NativeFunction::CollectionAdd(brand) => {
                // A set member is its own value (ECMA-262 24.2.3.1 step 5).
                let value = normalize_zero(first_argument(arguments));
                self.collection_store(receiver, brand, value.clone(), value)?;
                Ok(JsValue::Object(receiver))
            }
            NativeFunction::CollectionHas(brand) => self.collection_has(receiver, brand, arguments),
            NativeFunction::CollectionDelete(brand) => {
                self.collection_delete(receiver, brand, arguments)
            }
            NativeFunction::CollectionClear(brand) => self.collection_clear(receiver, brand),
            NativeFunction::CollectionForEach(brand) => {
                self.collection_for_each(dom, receiver, brand, arguments)
            }
            NativeFunction::CollectionKeys(brand) => {
                self.collection_iterator(receiver, brand, CollectionView::Keys)
            }
            NativeFunction::CollectionValues(brand) => {
                self.collection_iterator(receiver, brand, CollectionView::Values)
            }
            NativeFunction::CollectionEntries(brand) => {
                self.collection_iterator(receiver, brand, CollectionView::Entries)
            }
            NativeFunction::CollectionSize(brand) => self.collection_size(receiver, brand),
            NativeFunction::CollectionSpecies => Ok(JsValue::Object(receiver)),
            NativeFunction::MapGroupBy => self.map_group_by(dom, arguments),
            NativeFunction::CollectionIteratorNext => self.collection_iterator_next(receiver),
            NativeFunction::CollectionUnion
            | NativeFunction::CollectionIntersection
            | NativeFunction::CollectionDifference
            | NativeFunction::CollectionSymmetricDifference
            | NativeFunction::CollectionIsSubsetOf
            | NativeFunction::CollectionIsSupersetOf
            | NativeFunction::CollectionIsDisjointFrom => {
                self.dispatch_set_method(dom, function, receiver, arguments)
            }
            other => match self.dispatch_iterator_native(dom, other, receiver, arguments) {
                Some(result) => result,
                None => self.dispatch_string_native(dom, other, receiver, arguments),
            },
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
    /// `RequireInternalSlot` for a collection method (ECMA-262 24.1.3.1 and its
    /// counterparts): the receiver must be a collection of exactly `brand`. A
    /// proxy is not looked through.
    fn branded_entries(
        &self,
        receiver: ObjectId,
        brand: CollectionKind,
    ) -> Result<&Entries, JsError> {
        match self.realm.host_ref(receiver) {
            Some(ObjectHost::Collection { kind, entries }) if *kind == brand => Ok(entries),
            _ => Err(JsError::type_error(format!(
                "method called on an incompatible receiver; expected a {brand:?}"
            ))),
        }
    }

    fn branded_entries_mut(
        &mut self,
        receiver: ObjectId,
        brand: CollectionKind,
    ) -> Result<&mut Entries, JsError> {
        match self.realm.host_mut(receiver) {
            Some(ObjectHost::Collection { kind, entries }) if *kind == brand => Ok(entries),
            _ => Err(JsError::type_error(format!(
                "method called on an incompatible receiver; expected a {brand:?}"
            ))),
        }
    }

    /// `CanBeHeldWeakly` (ECMA-262 9.13): an object, or a symbol that is not
    /// registered with `Symbol.for`.
    fn can_be_held_weakly(&self, value: &JsValue) -> bool {
        match value {
            JsValue::Object(_) => true,
            JsValue::Symbol(symbol) => !self
                .global_symbol_registry
                .values()
                .any(|id| *id == symbol.id()),
            _ => false,
        }
    }

    fn pin_entries(&mut self, entries: &Entries) {
        for (key, value) in entries {
            self.pin_value(key);
            self.pin_value(value);
        }
    }

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
        let iterable = first_argument(arguments);
        if matches!(iterable, JsValue::Null | JsValue::Undefined) {
            return Ok(JsValue::Object(collection));
        }
        // ECMA-262 24.1.1.1 steps 6-7: the adder is read once, and it must be callable.
        let adder_name = if kind.is_map() { "set" } else { "add" };
        let adder = match self.get_member(dom, collection, adder_name)? {
            JsValue::Object(adder) if Self::is_callable_object(adder, &self.realm) => adder,
            _ => {
                return Err(JsError::type_error(format!(
                    "the collection's '{adder_name}' is not a function"
                )));
            }
        };
        self.add_entries_from_iterable(dom, collection, kind, adder, &iterable)?;
        Ok(JsValue::Object(collection))
    }

    /// `AddEntriesFromIterable` (ECMA-262 24.1.1.2): each item is handed to the
    /// adder, and an abrupt item closes the iterator.
    fn add_entries_from_iterable(
        &mut self,
        dom: &mut Dom,
        collection: ObjectId,
        kind: CollectionKind,
        adder: ObjectId,
        iterable: &JsValue,
    ) -> Result<(), JsError> {
        let pinned = self.transient_roots.len();
        self.transient_roots.extend([collection, adder]);
        let outcome = self.add_entries_steps(dom, collection, kind, adder, iterable);
        self.transient_roots.truncate(pinned);
        outcome
    }

    fn add_entries_steps(
        &mut self,
        dom: &mut Dom,
        collection: ObjectId,
        kind: CollectionKind,
        adder: ObjectId,
        iterable: &JsValue,
    ) -> Result<(), JsError> {
        if let JsValue::String(_) = iterable {
            // A string's code points come from its built-in iterator, which nothing
            // else can observe, so they are gathered before the adder runs.
            for item in self.iterate_values(dom, iterable)? {
                self.add_entry(dom, collection, kind, adder, item)?;
            }
            return Ok(());
        }
        let Some((iterator, next)) = self.get_iterator(dom, iterable)? else {
            return Err(JsError::type_error(format!(
                "{} is not iterable",
                iterable.to_js_string()
            )));
        };
        let pinned = self.transient_roots.len();
        self.transient_roots.extend([iterator, next]);
        let outcome = self.add_entries_stepping(dom, collection, kind, adder, iterator, next);
        self.transient_roots.truncate(pinned);
        outcome
    }

    fn add_entries_stepping(
        &mut self,
        dom: &mut Dom,
        collection: ObjectId,
        kind: CollectionKind,
        adder: ObjectId,
        iterator: ObjectId,
        next: ObjectId,
    ) -> Result<(), JsError> {
        while let Some(item) = self.iterator_next(dom, iterator, next)? {
            if let Err(error) = self.add_entry(dom, collection, kind, adder, item) {
                // IfAbruptCloseIterator (ECMA-262 7.4.6): the iterator closes and the
                // error from the entry is the one that propagates.
                return self.iterator_close(dom, iterator, Err(error));
            }
        }
        Ok(())
    }

    /// One step of `AddEntriesFromIterable`: a Map entry is read as `[key, value]`
    /// and passed to the adder as two arguments; a Set value is passed as is.
    fn add_entry(
        &mut self,
        dom: &mut Dom,
        collection: ObjectId,
        kind: CollectionKind,
        adder: ObjectId,
        item: JsValue,
    ) -> Result<(), JsError> {
        let arguments = if kind.is_map() {
            let JsValue::Object(entry) = item else {
                return Err(JsError::type_error("iterator value is not an entry object"));
            };
            let key = self.get_member(dom, entry, "0")?;
            let value = self.get_member(dom, entry, "1")?;
            vec![key, value]
        } else {
            vec![item]
        };
        self.call_with_this(dom, adder, &arguments, JsValue::Object(collection))?;
        Ok(())
    }

    /// `Map.groupBy(items, callbackfn)` (ECMA-262 24.1.2.1 `GroupBy` with the zero
    /// key coercion): a new `%Map%` maps each distinct key to the array of items
    /// it groups, with keys compared by `SameValueZero`.
    fn map_group_by(&mut self, dom: &mut Dom, arguments: &[JsValue]) -> Result<JsValue, JsError> {
        let items = first_argument(arguments);
        if matches!(items, JsValue::Undefined | JsValue::Null) {
            return Err(JsError::type_error(
                "Map.groupBy called on null or undefined",
            ));
        }
        let callback_value = arguments.get(1).cloned().unwrap_or(JsValue::Undefined);
        let callback = Self::require_callable_object(&callback_value, &self.realm)?;
        let pinned = self.transient_roots.len();
        let outcome = self.build_group_by(dom, &items, callback);
        self.transient_roots.truncate(pinned);
        outcome
    }

    fn build_group_by(
        &mut self,
        dom: &mut Dom,
        items: &JsValue,
        callback: ObjectId,
    ) -> Result<JsValue, JsError> {
        let groups = self.group_items(dom, items, callback)?;
        self.ensure_heap_capacity(1)?;
        let prototype = self.realm.map_prototype();
        let map = self.realm.collection(CollectionKind::Map, Some(prototype));
        self.transient_roots.push(map);
        for (key, values) in groups {
            let array = self.create_array_from_values(&values)?;
            self.collection_store(map, CollectionKind::Map, key, JsValue::Object(array))?;
        }
        Ok(JsValue::Object(map))
    }

    /// The `GroupBy` walk: the callback keys each item, and an abrupt callback
    /// closes the iterator. Every object it keeps is pinned until the caller ends.
    fn group_items(
        &mut self,
        dom: &mut Dom,
        items: &JsValue,
        callback: ObjectId,
    ) -> Result<Groups, JsError> {
        let mut groups: Groups = Vec::new();
        if let JsValue::String(_) = items {
            for (index, value) in self.iterate_values(dom, items)?.into_iter().enumerate() {
                let key = self.call_with_this(
                    dom,
                    callback,
                    &[value.clone(), JsValue::Number(index as f64)],
                    JsValue::Undefined,
                )?;
                self.add_to_group(&mut groups, key, value);
            }
            return Ok(groups);
        }
        let Some((iterator, next)) = self.get_iterator(dom, items)? else {
            return Err(JsError::type_error(format!(
                "{} is not iterable",
                items.to_js_string()
            )));
        };
        self.transient_roots.extend([iterator, next]);
        let mut index = 0usize;
        loop {
            let Some(value) = self.iterator_next(dom, iterator, next)? else {
                return Ok(groups);
            };
            let arguments = [value.clone(), JsValue::Number(index as f64)];
            match self.call_with_this(dom, callback, &arguments, JsValue::Undefined) {
                Ok(key) => self.add_to_group(&mut groups, key, value),
                Err(error) => return self.iterator_close(dom, iterator, Err(error)),
            }
            index += 1;
        }
    }

    /// `AddValueToKeyedGroup` (ECMA-262 24.1.2.1.1): the item joins the group of
    /// its key, which is created on first use.
    fn add_to_group(&mut self, groups: &mut Groups, key: JsValue, value: JsValue) {
        let key = normalize_zero(key);
        self.pin_value(&key);
        self.pin_value(&value);
        match groups
            .iter_mut()
            .find(|(candidate, _)| same_value_zero(candidate, &key))
        {
            Some((_, values)) => values.push(value),
            None => groups.push((key, vec![value])),
        }
    }

    pub(in crate::runtime) fn collection_get(
        &self,
        receiver: ObjectId,
        brand: CollectionKind,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let key = first_argument(arguments);
        let entries = self.branded_entries(receiver, brand)?;
        Ok(entries
            .iter()
            .find(|(candidate, _)| same_value_zero(candidate, &key))
            .map_or(JsValue::Undefined, |(_, value)| value.clone()))
    }

    /// The store behind `set` and `add`: the brand is checked first, a weak
    /// collection takes only a value that `CanBeHeldWeakly`, and a `-0` key is
    /// stored as `+0`.
    pub(in crate::runtime) fn collection_store(
        &mut self,
        receiver: ObjectId,
        brand: CollectionKind,
        key: JsValue,
        value: JsValue,
    ) -> Result<(), JsError> {
        self.branded_entries(receiver, brand)?;
        if brand.is_weak() && !self.can_be_held_weakly(&key) {
            return Err(JsError::type_error(
                "invalid value used as a weak collection key",
            ));
        }
        let key = normalize_zero(key);
        let entries = self.branded_entries_mut(receiver, brand)?;
        match entries
            .iter_mut()
            .find(|(candidate, _)| same_value_zero(candidate, &key))
        {
            Some((_, stored)) => *stored = value,
            None => entries.push((key, value)),
        }
        Ok(())
    }

    /// The entry for `key` (already canonical) in a branded collection, if any.
    fn branded_value(
        &self,
        receiver: ObjectId,
        brand: CollectionKind,
        key: &JsValue,
    ) -> Result<Option<JsValue>, JsError> {
        let entries = self.branded_entries(receiver, brand)?;
        Ok(entries
            .iter()
            .find(|(candidate, _)| same_value_zero(candidate, key))
            .map(|(_, value)| value.clone()))
    }

    /// `getOrInsert(key, value)` (upsert proposal, `Map` and `WeakMap`): the stored
    /// value for `key`, or `value` once it is stored for a key that was absent.
    fn collection_get_or_insert(
        &mut self,
        receiver: ObjectId,
        brand: CollectionKind,
        key: JsValue,
        value: JsValue,
    ) -> Result<JsValue, JsError> {
        self.branded_entries(receiver, brand)?;
        if brand.is_weak() && !self.can_be_held_weakly(&key) {
            return Err(JsError::type_error(
                "invalid value used as a weak collection key",
            ));
        }
        let key = normalize_zero(key);
        if let Some(existing) = self.branded_value(receiver, brand, &key)? {
            return Ok(existing);
        }
        self.collection_store(receiver, brand, key, value.clone())?;
        Ok(value)
    }

    /// `getOrInsertComputed(key, callbackfn)` (upsert proposal): the stored value for
    /// `key`, or the callback's result, which is stored unless the callback itself
    /// stored a value for `key`, in which case that entry is overwritten.
    fn collection_get_or_insert_computed(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        brand: CollectionKind,
        key: JsValue,
        callback: &JsValue,
    ) -> Result<JsValue, JsError> {
        self.branded_entries(receiver, brand)?;
        let callback = Self::require_callable_object(callback, &self.realm)?;
        if brand.is_weak() && !self.can_be_held_weakly(&key) {
            return Err(JsError::type_error(
                "invalid value used as a weak collection key",
            ));
        }
        let key = normalize_zero(key);
        if let Some(existing) = self.branded_value(receiver, brand, &key)? {
            return Ok(existing);
        }
        let value = self.call_with_this(
            dom,
            callback,
            std::slice::from_ref(&key),
            JsValue::Undefined,
        )?;
        self.collection_store(receiver, brand, key, value.clone())?;
        Ok(value)
    }

    pub(in crate::runtime) fn collection_has(
        &self,
        receiver: ObjectId,
        brand: CollectionKind,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let key = first_argument(arguments);
        let entries = self.branded_entries(receiver, brand)?;
        Ok(JsValue::Boolean(
            entries
                .iter()
                .any(|(candidate, _)| same_value_zero(candidate, &key)),
        ))
    }

    pub(in crate::runtime) fn collection_delete(
        &mut self,
        receiver: ObjectId,
        brand: CollectionKind,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let key = first_argument(arguments);
        let entries = self.branded_entries_mut(receiver, brand)?;
        let Some(index) = entries
            .iter()
            .position(|(candidate, _)| same_value_zero(candidate, &key))
        else {
            return Ok(JsValue::Boolean(false));
        };
        entries.remove(index);
        Ok(JsValue::Boolean(true))
    }

    pub(in crate::runtime) fn collection_clear(
        &mut self,
        receiver: ObjectId,
        brand: CollectionKind,
    ) -> Result<JsValue, JsError> {
        self.branded_entries_mut(receiver, brand)?.clear();
        Ok(JsValue::Undefined)
    }

    /// `get size` (ECMA-262 24.1.3.10 and 24.2.3.9): the entry count.
    pub(in crate::runtime) fn collection_size(
        &self,
        receiver: ObjectId,
        brand: CollectionKind,
    ) -> Result<JsValue, JsError> {
        let entries = self.branded_entries(receiver, brand)?;
        Ok(JsValue::Number(entries.len() as f64))
    }

    pub(in crate::runtime) fn collection_for_each(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        brand: CollectionKind,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let entries = self.branded_entries(receiver, brand)?.clone();
        let callback_value = first_argument(arguments);
        let callback = Self::require_callable_object(&callback_value, &self.realm)?;
        let this_argument = arguments.get(1).cloned().unwrap_or(JsValue::Undefined);
        let pinned = self.transient_roots.len();
        self.pin_entries(&entries);
        let outcome = self.for_each_entry(dom, receiver, brand, callback, &this_argument, &entries);
        self.transient_roots.truncate(pinned);
        outcome?;
        Ok(JsValue::Undefined)
    }

    fn for_each_entry(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        brand: CollectionKind,
        callback: ObjectId,
        this_argument: &JsValue,
        entries: &Entries,
    ) -> Result<(), JsError> {
        for (key, value) in entries {
            let callback_arguments = if brand.is_map() {
                [value.clone(), key.clone(), JsValue::Object(receiver)]
            } else {
                [key.clone(), key.clone(), JsValue::Object(receiver)]
            };
            self.call_with_this(dom, callback, &callback_arguments, this_argument.clone())?;
        }
        Ok(())
    }

    pub(in crate::runtime) fn collection_iterator(
        &mut self,
        receiver: ObjectId,
        brand: CollectionKind,
        view: CollectionView,
    ) -> Result<JsValue, JsError> {
        let entries = self.branded_entries(receiver, brand)?.clone();
        self.ensure_heap_capacity(1)?;
        let pinned = self.transient_roots.len();
        self.pin_entries(&entries);
        let built = self.collection_view_values(entries, brand, view);
        self.transient_roots.truncate(pinned);
        let values = built?;
        self.ensure_heap_capacity(1)?;
        Ok(JsValue::Object(self.realm.collection_iterator(values)))
    }

    /// The values a keys, values or entries iterator yields, in insertion order.
    fn collection_view_values(
        &mut self,
        entries: Entries,
        brand: CollectionKind,
        view: CollectionView,
    ) -> Result<Vec<JsValue>, JsError> {
        let mut values = Vec::with_capacity(entries.len());
        for (key, value) in entries {
            let item = match view {
                CollectionView::Keys => key,
                CollectionView::Values if brand.is_map() => value,
                CollectionView::Values => key,
                CollectionView::Entries => {
                    JsValue::Object(self.create_array_from_values(&[key, value])?)
                }
            };
            self.pin_value(&item);
            values.push(item);
        }
        Ok(values)
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
