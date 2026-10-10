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

//! `Array` and `Array.prototype`.
//!
//! The methods are the ECMA-262 algorithms written over the generic object
//! operations (`Get`, `HasProperty`, `Set`, `DeletePropertyOrThrow`,
//! `CreateDataPropertyOrThrow`), so arrays, array-likes, proxies and subclasses
//! observe the same reads and writes in the same order, and a failed write is a
//! `TypeError`. Only the iterator views (`keys`, `values`, `entries`) still
//! materialize their elements.

use crate::JsError;
use crate::JsSymbol;
use crate::JsValue;
use crate::ObjectId;
use crate::PropertyDescriptor;
use crate::runtime::JsRuntime;
use crate::runtime::builtins::object::{PartialDescriptor, PropertyName};
use crate::runtime::convert::required_argument;
use crate::runtime::convert::same_value_zero;
use crate::runtime::convert::strict_equal;
use crate::runtime::convert::to_number;
use crate::runtime::convert::uint32_of_number;
use crate::utf16;
use crate::value::NativeFunction;
use crate::value::ObjectHost;
use render_dom::Dom;

impl JsRuntime {
    pub(in crate::runtime) fn dispatch_array_native(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match function {
            NativeFunction::ArrayIndexOf => self.array_index_of(dom, receiver, arguments),
            NativeFunction::ArrayLastIndexOf => self.array_last_index_of(dom, receiver, arguments),
            NativeFunction::ArraySlice => self.array_slice(dom, receiver, arguments),
            NativeFunction::ArrayValues => self.array_view_iterator(receiver, ArrayView::Values),
            NativeFunction::ArrayKeys => self.array_view_iterator(receiver, ArrayView::Keys),
            NativeFunction::ArrayEntries => self.array_view_iterator(receiver, ArrayView::Entries),
            NativeFunction::ArraySplice => self.array_splice(dom, receiver, arguments),
            NativeFunction::ArrayReverse => self.array_reverse(dom, receiver),
            NativeFunction::ArrayAt => self.array_at(dom, receiver, arguments),
            NativeFunction::ArrayFlat => self.array_flat(dom, receiver, arguments),
            NativeFunction::ArrayFlatMap => self.array_flat_map(dom, receiver, arguments),
            NativeFunction::ArrayReduce => self.array_reduce(dom, receiver, arguments, false),
            NativeFunction::ArrayReduceRight => self.array_reduce(dom, receiver, arguments, true),
            NativeFunction::ArrayFind => self.array_find(dom, receiver, arguments, false, false),
            NativeFunction::ArrayFindIndex => {
                self.array_find(dom, receiver, arguments, false, true)
            }
            NativeFunction::ArrayFindLast => self.array_find(dom, receiver, arguments, true, false),
            NativeFunction::ArrayFindLastIndex => {
                self.array_find(dom, receiver, arguments, true, true)
            }
            NativeFunction::ArraySort => self.array_sort(dom, receiver, arguments),
            NativeFunction::ArrayToSorted => self.array_to_sorted(dom, receiver, arguments),
            NativeFunction::ArrayToReversed => self.array_to_reversed(dom, receiver),
            NativeFunction::ArrayToSpliced => self.array_to_spliced(dom, receiver, arguments),
            NativeFunction::ArrayWith => self.array_with(dom, receiver, arguments),
            NativeFunction::ArrayFill => self.array_fill(dom, receiver, arguments),
            NativeFunction::ArrayCopyWithin => self.array_copy_within(dom, receiver, arguments),
            NativeFunction::ArrayConcat => self.array_concat(dom, receiver, arguments),
            NativeFunction::ArrayShift => self.array_shift(dom, receiver),
            NativeFunction::ArrayUnshift => self.array_unshift(dom, receiver, arguments),
            NativeFunction::ArrayForEach => {
                self.array_visit(dom, receiver, arguments, Visit::ForEach, "forEach")
            }
            NativeFunction::ArrayMap => {
                self.array_visit(dom, receiver, arguments, Visit::Map, "map")
            }
            NativeFunction::ArrayFilter => {
                self.array_visit(dom, receiver, arguments, Visit::Filter, "filter")
            }
            NativeFunction::ArraySome => {
                self.array_visit(dom, receiver, arguments, Visit::Some, "some")
            }
            NativeFunction::ArrayEvery => {
                self.array_visit(dom, receiver, arguments, Visit::Every, "every")
            }
            NativeFunction::ArrayIncludes => self.array_includes(dom, receiver, arguments),
            NativeFunction::ArrayPrototypeToString => self.array_to_string(dom, receiver),
            NativeFunction::ArrayToLocaleString => self.array_to_locale_string(dom, receiver),
            NativeFunction::ArrayIsArray => {
                let value = arguments.first().cloned().unwrap_or(JsValue::Undefined);
                Ok(JsValue::Boolean(self.is_array_value(&value)?))
            }
            NativeFunction::ArrayFrom => self.array_from(dom, receiver, arguments),
            NativeFunction::ArrayOf => self.array_of(dom, receiver, arguments),
            NativeFunction::ArrayFromAsync => self.array_from_async(dom, receiver, arguments),
            NativeFunction::ArraySpecies => Ok(JsValue::Object(receiver)),
            NativeFunction::ArrayPush => self.array_push(dom, receiver, arguments),
            NativeFunction::ArrayPop => self.array_pop(dom, receiver),
            NativeFunction::ArrayJoin => self.array_join(dom, receiver, arguments),
            other => self.dispatch_typed_array_native(dom, other, receiver, arguments),
        }
    }
}

pub(in crate::runtime) fn array_index(property: &str) -> Option<u32> {
    let index = property.parse::<u32>().ok()?;
    (index.to_string() == property && index < u32::MAX).then_some(index)
}

/// The `thisArg` that every `callbackfn` parameter accepts as its second
/// argument (§23.1.3.x). `Array.prototype.map.call(nodes, render, this)` is how
/// borrowed-base helpers reuse their own methods, so dropping it silently
/// breaks each of them.
pub(in crate::runtime) fn callback_this_argument(arguments: &[JsValue]) -> JsValue {
    arguments.get(1).cloned().unwrap_or(JsValue::Undefined)
}

/// Which indexed projection `Array.prototype.keys/values/entries` produces.
#[derive(Clone, Copy)]
pub(in crate::runtime) enum ArrayView {
    Keys,
    Values,
    Entries,
}

/// The per-element behaviour shared by the callback-driven iteration methods.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Visit {
    ForEach,
    Map,
    Filter,
    Some,
    Every,
}

/// `ToLength` ceiling (ECMA-262 §7.1.15).
pub(in crate::runtime) const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// `ArrayCreate` (§10.4.2.2 step 1) rejects lengths above 2^32-1.
const MAX_ARRAY_LENGTH: f64 = 4_294_967_295.0;

/// Element cap for the eager paths that read every index into a `Vec`.
/// Hostile array-likes (`{length: 2**53-1}`) must produce a catchable error,
/// not a multi-gigabyte allocation that aborts the process.
pub(in crate::runtime) const MAX_MATERIALIZED_ELEMENTS: usize = 1 << 24;

/// Nesting limit for `flat`: a cyclic array flattened to infinite depth would
/// otherwise recurse until the native stack gives out.
const MAX_FLATTEN_DEPTH: usize = 4096;

/// The async abstract closure of `Array.fromAsync` (ES2026 §23.1.2.2), written
/// as an async function over the engine's `for await` and `await`. Parameters
/// after `thisArg` are the intrinsics it needs, captured when it is called.
const FROM_ASYNC_SOURCE: &str = r"(async function (C, asyncItems, mapfn, thisArg, isConstructor, asyncSymbol, syncSymbol, ArrayConstructor, defineProperty) {
  'use strict';
  if (mapfn !== undefined && typeof mapfn !== 'function') {
    throw new TypeError('Array.fromAsync: mapfn is not callable');
  }
  function methodOf(value, symbol) {
    var method = value[symbol];
    if (method === undefined || method === null) return undefined;
    if (typeof method !== 'function') throw new TypeError('Array.fromAsync: iterator method is not callable');
    return method;
  }
  function defineElement(target, key, value) {
    defineProperty(target, key, { value: value, writable: true, enumerable: true, configurable: true });
  }
  // An omitted thisArg is `undefined` for the callback itself, which `call` with
  // `undefined` does not reliably give to a strict function.
  function callMapper(value, index) {
    return thisArg === undefined ? mapfn(value, index) : mapfn.call(thisArg, value, index);
  }
  var asyncMethod = methodOf(asyncItems, asyncSymbol);
  var syncMethod = asyncMethod === undefined ? methodOf(asyncItems, syncSymbol) : undefined;
  if (asyncMethod !== undefined || syncMethod !== undefined) {
    var result = isConstructor ? new C() : new ArrayConstructor();
    var iterable = {};
    if (asyncMethod !== undefined) {
      iterable[asyncSymbol] = function () { return asyncMethod.call(asyncItems); };
    } else {
      iterable[syncSymbol] = function () { return syncMethod.call(asyncItems); };
    }
    var index = 0;
    for await (var value of iterable) {
      var mapped = value;
      if (mapfn !== undefined) mapped = await callMapper(value, index);
      defineElement(result, index, mapped);
      index++;
    }
    result.length = index;
    return result;
  }
  var arrayLike = Object(asyncItems);
  var length = Number(arrayLike.length);
  length = length !== length || length <= 0 ? 0 : Math.min(Math.floor(length), 9007199254740991);
  var built = isConstructor ? new C(length) : new ArrayConstructor(length);
  for (var k = 0; k < length; k++) {
    var kValue = await arrayLike[k];
    if (mapfn !== undefined) kValue = await callMapper(kValue, k);
    defineElement(built, k, kValue);
  }
  built.length = length;
  return built;
})";

/// `ToLength` (§7.1.15): clamp a length to the 0..=2^53-1 range.
pub(in crate::runtime) fn to_length(value: &JsValue) -> Result<f64, JsError> {
    let number = to_number(value)?;
    if number.is_nan() || number <= 0.0 {
        return Ok(0.0);
    }
    if number.is_infinite() {
        return Ok(MAX_SAFE_INTEGER);
    }
    Ok(number.trunc().min(MAX_SAFE_INTEGER))
}

/// Property key of an integral index.
fn index_key(index: f64) -> String {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let key = index as u64;
    key.to_string()
}

impl JsRuntime {
    pub(in crate::runtime) fn array_constructor(
        &mut self,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        self.ensure_heap_capacity(1)?;
        let array = self.realm.create_array();
        match arguments {
            [] => {}
            [JsValue::Number(length)] => {
                if !length.is_finite()
                    || *length < 0.0
                    || length.fract() != 0.0
                    || *length > f64::from(u32::MAX)
                {
                    return Err(JsError::type_error("invalid array length"));
                }
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                self.set_array_length(array, *length as u32)?;
            }
            [value] => {
                if !self
                    .realm
                    .set_property(array, "0".to_owned(), value.clone())
                {
                    return Err(JsError::type_error("could not define array element"));
                }
                self.set_array_length(array, 1)?;
            }
            values => {
                for (index, value) in values.iter().enumerate() {
                    self.realm
                        .set_property(array, index.to_string(), value.clone());
                }
                let length = u32::try_from(values.len()).map_err(|_| {
                    JsError::resource("array constructor exceeds the supported u32 range")
                })?;
                self.set_array_length(array, length)?;
            }
        }
        Ok(JsValue::Object(array))
    }

    /// Element count of an array-like object, honoring the typed-array
    /// element pool bound.
    pub(in crate::runtime) fn array_like_length(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
    ) -> Result<usize, JsError> {
        // ECMA-262 7.3.18 `LengthOfArrayLike`: `ToLength` of the `length`, so a
        // negative or `NaN` length reads as zero.
        let length = self.get_member(dom, object, "length")?;
        let length = self.to_length_value(dom, &length)?;
        if length > Self::MAX_TYPED_ARRAY_ELEMENTS as f64 {
            return Err(self.range_error("typed array length exceeds the engine bound"));
        }
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "length is validated as a finite non-negative integer"
        )]
        Ok(length as usize)
    }

    /// Dense indexed elements of an Array or array-like, for spread,
    /// destructuring, and constructor fallbacks. Reads through the ordinary
    /// property store and refuses lengths beyond the materialization bound
    /// with a catchable error instead of a multi-gigabyte allocation.
    pub(in crate::runtime) fn array_elements_for(
        &mut self,
        object: ObjectId,
    ) -> Result<Vec<JsValue>, JsError> {
        if let Some(ObjectHost::TypedArray { .. }) = self.realm.host(object) {
            return Ok(self
                .typed_array_elements(object)
                .unwrap_or_default()
                .into_iter()
                .collect());
        }
        let length = match self.realm.get_property(object, "length") {
            Some(value) => to_length(&value)?,
            None => 0.0,
        };
        if length > MAX_MATERIALIZED_ELEMENTS as f64 {
            return Err(JsError::resource(
                "array-like length exceeds the materialization bound",
            ));
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let length = length as usize;
        let mut elements = Vec::new();
        elements
            .try_reserve_exact(length)
            .map_err(|_| JsError::resource("array-like exceeds the available heap"))?;
        for index in 0..length {
            elements.push(
                self.realm
                    .get_property(object, &index.to_string())
                    .unwrap_or(JsValue::Undefined),
            );
        }
        Ok(elements)
    }

    pub(in crate::runtime) fn create_array_from_values(
        &mut self,
        values: &[JsValue],
    ) -> Result<ObjectId, JsError> {
        self.ensure_heap_capacity(1)?;
        let array = self.realm.create_array();
        for (index, value) in values.iter().enumerate() {
            self.realm
                .set_property(array, index.to_string(), value.clone());
        }
        let length = u32::try_from(values.len())
            .map_err(|_| JsError::resource("array result exceeds the supported u32 range"))?;
        self.set_array_length(array, length)?;
        Ok(array)
    }

    /// `ArrayCreate(length)` (§10.4.2.2): a fresh array whose length is set.
    /// Lengths above 2^32-1 are refused with a `RangeError`, exactly like a
    /// real engine's array-length cap.
    fn create_array_with_length(&mut self, length: f64) -> Result<ObjectId, JsError> {
        if length > MAX_ARRAY_LENGTH {
            return Err(self.range_error("Invalid array length"));
        }
        self.ensure_heap_capacity(1)?;
        let array = self.realm.create_array();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        self.set_array_length(array, length as u32)?;
        Ok(array)
    }

    /// `IsArray` (§7.2.2): an Array exotic object, or a Proxy whose target is one.
    pub(in crate::runtime) fn is_array_value(&self, value: &JsValue) -> Result<bool, JsError> {
        let JsValue::Object(object) = value else {
            return Ok(false);
        };
        match self.realm.host(*object) {
            Some(ObjectHost::Array) => Ok(true),
            Some(ObjectHost::Proxy { target, .. }) => {
                let target = JsValue::Object(target);
                self.is_array_value(&target)
            }
            _ => Ok(false),
        }
    }

    /// `LengthOfArrayLike` (§7.3.18): `ToLength(Get(O, "length"))`. String
    /// wrappers expose their code-unit count as a virtual `length`, because
    /// that is what `String.prototype.length` is.
    fn length_of_array_like(&mut self, dom: &mut Dom, object: ObjectId) -> Result<f64, JsError> {
        if let Some(ObjectHost::StringPrimitive(text)) = self.realm.host(object) {
            #[allow(clippy::cast_precision_loss)]
            return Ok(utf16::utf16_length(&text) as f64);
        }
        let length = self.get_member(dom, object, "length")?;
        self.to_length_value(dom, &length)
    }

    /// `HasProperty` for an integral index (§7.3.11), including the virtual
    /// indexed properties of string wrappers and typed arrays.
    fn has_index(&mut self, dom: &mut Dom, object: ObjectId, index: f64) -> Result<bool, JsError> {
        if matches!(self.realm.host(object), Some(ObjectHost::Proxy { .. })) {
            return self.proxy_has(dom, object, &index_key(index));
        }
        Ok(match self.realm.host(object) {
            Some(ObjectHost::TypedArray { buffer, length, .. }) => {
                index < length as f64 && !buffer.is_detached()
            }
            Some(ObjectHost::StringPrimitive(text)) => index < utf16::utf16_length(&text) as f64,
            _ => self.realm.get_property(object, &index_key(index)).is_some(),
        })
    }

    /// [[Get]] for an integral index; accessors and proxies run.
    fn get_index(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        index: f64,
    ) -> Result<JsValue, JsError> {
        self.get_member(dom, object, &index_key(index))
    }

    /// `Set(O, P, V, true)` for a key: a failed assignment is a `TypeError`.
    fn set_key_or_throw(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        key: &str,
        value: JsValue,
    ) -> Result<(), JsError> {
        if matches!(self.realm.host(object), Some(ObjectHost::Proxy { .. })) {
            let name = PropertyName::String(key.to_owned());
            if !self.proxy_set_property(dom, object, &name, value, JsValue::Object(object))? {
                return Err(JsError::type_error(format!(
                    "proxy set trap refused property '{key}'"
                )));
            }
            return Ok(());
        }
        if !self.ordinary_set_allowed(object, key) {
            return Err(JsError::type_error(format!(
                "cannot assign to read only property '{key}'"
            )));
        }
        self.set_member(dom, object, key, value)
    }

    fn set_index_or_throw(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        index: f64,
        value: JsValue,
    ) -> Result<(), JsError> {
        self.set_key_or_throw(dom, object, &index_key(index), value)
    }

    /// The checks of `OrdinarySet` (§10.1.9.2) that can refuse a write, without
    /// performing it: a non-writable own or inherited data property, an
    /// accessor without a setter, a non-extensible target, and an index past the
    /// end of an array whose `length` is read-only.
    fn ordinary_set_allowed(&self, object: ObjectId, key: &str) -> bool {
        match self.realm.host(object) {
            // Out-of-range typed array writes are ignored, not refused.
            Some(ObjectHost::TypedArray { .. }) => return true,
            Some(ObjectHost::StringPrimitive(text)) => {
                if let Ok(index) = key.parse::<usize>()
                    && index < utf16::utf16_length(&text)
                {
                    return false;
                }
            }
            _ => {}
        }
        if let Some(own) = self.realm.own_property(object, key) {
            return if own.is_accessor() {
                own.setter.is_some()
            } else {
                own.writable
            };
        }
        if matches!(self.realm.host(object), Some(ObjectHost::Array))
            && let Some(index) = array_index(key)
        {
            let length_writable = self
                .realm
                .own_property(object, "length")
                .is_some_and(|length| length.writable);
            if !length_writable && f64::from(index) >= self.array_length_or_zero(object) {
                return false;
            }
        }
        match self.realm.get_descriptor(object, key) {
            Some(inherited) if inherited.is_accessor() => inherited.setter.is_some(),
            Some(inherited) if !inherited.writable => false,
            _ => self.realm.is_extensible(object),
        }
    }

    fn array_length_or_zero(&self, object: ObjectId) -> f64 {
        self.array_length(object).map_or(0.0, f64::from)
    }

    /// `DeletePropertyOrThrow` (§7.3.10).
    fn delete_index_or_throw(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        index: f64,
    ) -> Result<(), JsError> {
        if self.delete_property_value(dom, object, &index_key(index))? {
            Ok(())
        } else {
            Err(JsError::type_error(format!(
                "cannot delete property '{}'",
                index_key(index)
            )))
        }
    }

    /// `Set(O, "length", length, true)` for any object; an Array's length also
    /// removes the elements it no longer covers.
    fn set_length_or_throw(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        length: f64,
    ) -> Result<(), JsError> {
        if matches!(self.realm.host(object), Some(ObjectHost::Array)) {
            return self.set_array_length_value(dom, object, &JsValue::Number(length));
        }
        self.set_key_or_throw(dom, object, "length", JsValue::Number(length))
    }

    /// `CreateDataPropertyOrThrow` (§7.3.7) for an index: a fresh or configurable
    /// own data property, and an Array's `length` grows to cover it.
    fn create_data_property_or_throw(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        index: f64,
        value: JsValue,
    ) -> Result<(), JsError> {
        // CreateDataProperty (ECMA-262 7.3.7): `[[DefineOwnProperty]]` with a
        // full data descriptor, which a Proxy answers through its trap.
        if matches!(self.realm.host(object), Some(ObjectHost::Proxy { .. })) {
            let key = PropertyName::String(index_key(index));
            let partial = PartialDescriptor {
                value: Some(value),
                writable: Some(true),
                enumerable: Some(true),
                configurable: Some(true),
                ..PartialDescriptor::default()
            };
            return if self.define_own_property(dom, object, &key, partial)? {
                Ok(())
            } else {
                Err(JsError::type_error(
                    "proxy defineProperty trap refused the property",
                ))
            };
        }
        let key = index_key(index);
        match self.realm.own_property(object, &key) {
            Some(existing) if !existing.configurable => {
                return Err(JsError::type_error(format!(
                    "cannot redefine property '{key}'"
                )));
            }
            Some(_) => {}
            None if !self.realm.is_extensible(object) => {
                return Err(JsError::type_error(format!(
                    "cannot define property '{key}' on a non-extensible object"
                )));
            }
            None => {}
        }
        let is_array = matches!(self.realm.host(object), Some(ObjectHost::Array));
        let grows = is_array && index >= self.array_length_or_zero(object);
        if grows && !self.array_length_writable(object) {
            return Err(JsError::type_error("array length is not writable"));
        }
        if !self
            .realm
            .define_property(object, key, PropertyDescriptor::data(value))
        {
            return Err(JsError::type_error("could not define array element"));
        }
        // An index at or past 2^32-1 is not an array index, so it never moves
        // the length.
        if grows && index < MAX_ARRAY_LENGTH {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            self.set_array_length(object, (index + 1.0) as u32)?;
        }
        Ok(())
    }

    fn array_length_writable(&self, object: ObjectId) -> bool {
        self.realm
            .own_property(object, "length")
            .is_some_and(|length| length.writable)
    }

    /// `ArraySpeciesCreate(originalArray, length)` (§10.4.2.3).
    fn array_species_create(
        &mut self,
        dom: &mut Dom,
        original: ObjectId,
        length: f64,
    ) -> Result<ObjectId, JsError> {
        if !self.is_array_value(&JsValue::Object(original))? {
            return self.create_array_with_length(length);
        }
        let mut constructor = self.get_member(dom, original, "constructor")?;
        if let JsValue::Object(candidate) = constructor {
            constructor =
                self.get_symbol_value(dom, candidate, &JsSymbol::well_known("@@species"))?;
            if matches!(constructor, JsValue::Null) {
                constructor = JsValue::Undefined;
            }
        }
        match constructor {
            JsValue::Undefined => self.create_array_with_length(length),
            JsValue::Object(species) if self.is_constructor(species) => {
                match self.construct(dom, species, &[JsValue::Number(length)])? {
                    JsValue::Object(result) => Ok(result),
                    _ => Err(JsError::type_error(
                        "species constructor returned a primitive",
                    )),
                }
            }
            _ => Err(JsError::type_error(
                "object.constructor[Symbol.species] is not a constructor",
            )),
        }
    }

    /// `ToIntegerOrInfinity` of a relative position argument, clamped into
    /// `0..=length` (§23.1.3.x `relativeStart` and friends). An absent or
    /// `undefined` argument reads as `default`.
    fn relative_bound(
        &mut self,
        dom: &mut Dom,
        value: Option<&JsValue>,
        length: f64,
        default: f64,
    ) -> Result<f64, JsError> {
        let value = match value {
            None | Some(JsValue::Undefined) => return Ok(default),
            Some(value) => value,
        };
        let raw = self.to_integer_value(dom, value)?;
        if raw == f64::NEG_INFINITY {
            return Ok(0.0);
        }
        if raw < 0.0 {
            return Ok((length + raw).max(0.0));
        }
        Ok(raw.min(length))
    }

    /// `Array.prototype.push` (§23.1.3.23).
    pub(in crate::runtime) fn array_push(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let length = self.length_of_array_like(dom, receiver)?;
        let count = arguments.len() as f64;
        if length + count > MAX_SAFE_INTEGER {
            return Err(JsError::type_error("array length exceeds 2^53 - 1"));
        }
        let mut length = length;
        for value in arguments {
            self.set_index_or_throw(dom, receiver, length, value.clone())?;
            length += 1.0;
        }
        self.set_length_or_throw(dom, receiver, length)?;
        Ok(JsValue::Number(length))
    }

    /// `Array.prototype.pop` (§23.1.3.22).
    pub(in crate::runtime) fn array_pop(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
    ) -> Result<JsValue, JsError> {
        let length = self.length_of_array_like(dom, receiver)?;
        if length == 0.0 {
            self.set_length_or_throw(dom, receiver, 0.0)?;
            return Ok(JsValue::Undefined);
        }
        let index = length - 1.0;
        let element = self.get_index(dom, receiver, index)?;
        self.delete_index_or_throw(dom, receiver, index)?;
        self.set_length_or_throw(dom, receiver, index)?;
        Ok(element)
    }

    /// `Array.prototype.join` (§23.1.3.18). A receiver that is already being
    /// joined contributes an empty string, which keeps a cyclic array from
    /// recursing forever.
    pub(in crate::runtime) fn array_join(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let length = self.length_of_array_like(dom, receiver)?;
        let separator = match arguments.first() {
            None | Some(JsValue::Undefined) => ",".to_owned(),
            Some(value) => self.to_string_value(dom, value)?,
        };
        if self.arrays_joining.contains(&receiver) {
            return Ok(JsValue::String(String::new()));
        }
        // The joined string is at least `length - 1` characters long; a
        // hostile length must yield a catchable error, not an unbounded
        // string build.
        if length > MAX_MATERIALIZED_ELEMENTS as f64 {
            return Err(self.range_error("Invalid string length"));
        }
        self.arrays_joining.push(receiver);
        let output = self.join_indexed(dom, receiver, length, &separator);
        self.arrays_joining.pop();
        Ok(JsValue::String(output?))
    }

    fn join_indexed(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        length: f64,
        separator: &str,
    ) -> Result<String, JsError> {
        let mut output = String::new();
        let mut index = 0.0;
        while index < length {
            if index > 0.0 {
                output.push_str(separator);
            }
            match self.get_index(dom, receiver, index)? {
                JsValue::Undefined | JsValue::Null => {}
                // ToString, not the primitive shortcut: a nested array or
                // any object element runs its own `toString`.
                value => output.push_str(&self.to_string_value(dom, &value)?),
            }
            index += 1.0;
        }
        Ok(output)
    }

    /// `Array.prototype.toString` (§23.1.3.36): `join` when it is callable,
    /// otherwise `Object.prototype.toString`.
    fn array_to_string(&mut self, dom: &mut Dom, receiver: ObjectId) -> Result<JsValue, JsError> {
        let join = self.get_member(dom, receiver, "join")?;
        if let JsValue::Object(join) = join
            && Self::is_callable_object(join, &self.realm)
        {
            return self.call_with_this(dom, join, &[], JsValue::Object(receiver));
        }
        let object_prototype = self.realm.object_prototype();
        let to_string = self.get_member(dom, object_prototype, "toString")?;
        match to_string {
            JsValue::Object(function) => {
                self.call_with_this(dom, function, &[], JsValue::Object(receiver))
            }
            _ => Err(JsError::type_error(
                "Object.prototype.toString is not callable",
            )),
        }
    }

    /// `Array.prototype.toLocaleString` (§23.1.3.32).
    fn array_to_locale_string(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
    ) -> Result<JsValue, JsError> {
        let length = self.length_of_array_like(dom, receiver)?;
        if length > MAX_MATERIALIZED_ELEMENTS as f64 {
            return Err(self.range_error("Invalid string length"));
        }
        let mut output = String::new();
        let mut index = 0.0;
        while index < length {
            if index > 0.0 {
                output.push(',');
            }
            let element = self.get_index(dom, receiver, index)?;
            if !matches!(element, JsValue::Undefined | JsValue::Null) {
                let object = self.to_object(&element)?;
                let method = self.get_member(dom, object, "toLocaleString")?;
                let JsValue::Object(method) = method else {
                    return Err(JsError::type_error("toLocaleString is not a function"));
                };
                let text = self.call_with_this(dom, method, &[], element)?;
                output.push_str(&self.to_string_value(dom, &text)?);
            }
            index += 1.0;
        }
        Ok(JsValue::String(output))
    }

    /// `Array.prototype.keys/values/entries`: a materialized indexed iterator
    /// whose prototype carries the iterator-helper methods.
    pub(in crate::runtime) fn array_view_iterator(
        &mut self,
        receiver: ObjectId,
        view: ArrayView,
    ) -> Result<JsValue, JsError> {
        let elements = self.array_elements(receiver)?;
        let values = match view {
            ArrayView::Values => elements,
            ArrayView::Keys => elements
                .iter()
                .enumerate()
                .map(|(index, _)| JsValue::Number(index as f64))
                .collect(),
            ArrayView::Entries => {
                let mut values = Vec::with_capacity(elements.len());
                for (index, element) in elements.iter().enumerate() {
                    let pair = self.create_array_from_values(&[
                        JsValue::Number(index as f64),
                        element.clone(),
                    ])?;
                    values.push(JsValue::Object(pair));
                }
                values
            }
        };
        self.ensure_heap_capacity(1)?;
        Ok(JsValue::Object(self.realm.collection_iterator(values)))
    }

    /// Read all indexed elements (holes become `undefined`), for the iterator
    /// views. Lengths above the materialization bound are a catchable error.
    pub(in crate::runtime) fn array_elements(
        &self,
        receiver: ObjectId,
    ) -> Result<Vec<JsValue>, JsError> {
        let length = self.array_length(receiver)?;
        if length as usize > MAX_MATERIALIZED_ELEMENTS {
            return Err(JsError::resource(
                "array length exceeds the materialization bound",
            ));
        }
        let mut elements = Vec::new();
        elements
            .try_reserve_exact(length as usize)
            .map_err(|_| JsError::resource("array exceeds the available heap"))?;
        for index in 0..length {
            elements.push(
                self.realm
                    .get_property(receiver, &index.to_string())
                    .or_else(|| match self.realm.host(receiver) {
                        Some(ObjectHost::StringPrimitive(text)) => utf16::utf16_units(&text)
                            .get(index as usize)
                            .copied()
                            .map(|unit| JsValue::String(utf16::string_from_unit(unit))),
                        _ => None,
                    })
                    .unwrap_or(JsValue::Undefined),
            );
        }
        Ok(elements)
    }

    /// `Array.prototype.indexOf` (§23.1.3.17).
    pub(in crate::runtime) fn array_index_of(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let needle = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let length = self.length_of_array_like(dom, receiver)?;
        if length == 0.0 {
            return Ok(JsValue::Number(-1.0));
        }
        let start = match arguments.get(1) {
            None => 0.0,
            Some(value) => self.to_integer_value(dom, value)?,
        };
        if start == f64::INFINITY {
            return Ok(JsValue::Number(-1.0));
        }
        // `+ 0.0` turns a `-0` start into `+0`, the index the result reports.
        let mut index = if start >= 0.0 {
            start + 0.0
        } else {
            (length + start).max(0.0)
        };
        while index < length {
            if self.has_index(dom, receiver, index)?
                && strict_equal(&self.get_index(dom, receiver, index)?, &needle)
            {
                return Ok(JsValue::Number(index));
            }
            index += 1.0;
        }
        Ok(JsValue::Number(-1.0))
    }

    /// `Array.prototype.lastIndexOf` (§23.1.3.20).
    fn array_last_index_of(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let needle = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let length = self.length_of_array_like(dom, receiver)?;
        if length == 0.0 {
            return Ok(JsValue::Number(-1.0));
        }
        let start = match arguments.get(1) {
            None => length - 1.0,
            Some(value) => self.to_integer_value(dom, value)?,
        };
        if start == f64::NEG_INFINITY {
            return Ok(JsValue::Number(-1.0));
        }
        let mut index = if start >= 0.0 {
            start.min(length - 1.0) + 0.0
        } else {
            length + start
        };
        while index >= 0.0 {
            if self.has_index(dom, receiver, index)?
                && strict_equal(&self.get_index(dom, receiver, index)?, &needle)
            {
                return Ok(JsValue::Number(index));
            }
            index -= 1.0;
        }
        Ok(JsValue::Number(-1.0))
    }

    /// `Array.prototype.includes` (§23.1.3.16): every index is read, holes
    /// included, and matched with `SameValueZero`.
    pub(in crate::runtime) fn array_includes(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let search = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let length = self.length_of_array_like(dom, receiver)?;
        if length == 0.0 {
            return Ok(JsValue::Boolean(false));
        }
        let start = match arguments.get(1) {
            None => 0.0,
            Some(value) => self.to_integer_value(dom, value)?,
        };
        if start == f64::INFINITY {
            return Ok(JsValue::Boolean(false));
        }
        let mut index = if start >= 0.0 {
            start
        } else {
            (length + start).max(0.0)
        };
        while index < length {
            let element = self.get_index(dom, receiver, index)?;
            if same_value_zero(&element, &search) {
                return Ok(JsValue::Boolean(true));
            }
            index += 1.0;
        }
        Ok(JsValue::Boolean(false))
    }

    /// `Array.prototype.slice` (§23.1.3.27).
    pub(in crate::runtime) fn array_slice(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let length = self.length_of_array_like(dom, receiver)?;
        let start = self.relative_bound(dom, arguments.first(), length, 0.0)?;
        let end = self.relative_bound(dom, arguments.get(1), length, length)?;
        let count = (end - start).max(0.0);
        let result = self.array_species_create(dom, receiver, count)?;
        let mut index = start;
        let mut target = 0.0;
        while index < end {
            if self.has_index(dom, receiver, index)? {
                let value = self.get_index(dom, receiver, index)?;
                self.create_data_property_or_throw(dom, result, target, value)?;
            }
            index += 1.0;
            target += 1.0;
        }
        self.set_length_or_throw(dom, result, target)?;
        Ok(JsValue::Object(result))
    }

    /// `Array.prototype.splice` (§23.1.3.31).
    pub(in crate::runtime) fn array_splice(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let length = self.length_of_array_like(dom, receiver)?;
        let start = self.relative_bound(dom, arguments.first(), length, 0.0)?;
        let item_count = arguments.len().saturating_sub(2) as f64;
        let delete_count = match arguments.len() {
            0 => 0.0,
            1 => length - start,
            _ => {
                let raw = self.to_integer_value(dom, &arguments[1])?;
                raw.clamp(0.0, length - start)
            }
        };
        if length + item_count - delete_count > MAX_SAFE_INTEGER {
            return Err(JsError::type_error("array length exceeds 2^53 - 1"));
        }
        let removed = self.array_species_create(dom, receiver, delete_count)?;
        let mut k = 0.0;
        while k < delete_count {
            let from = start + k;
            if self.has_index(dom, receiver, from)? {
                let value = self.get_index(dom, receiver, from)?;
                self.create_data_property_or_throw(dom, removed, k, value)?;
            }
            k += 1.0;
        }
        self.set_length_or_throw(dom, removed, delete_count)?;

        if item_count < delete_count {
            let mut k = start;
            while k < length - delete_count {
                let from = k + delete_count;
                let to = k + item_count;
                self.move_index(dom, receiver, from, to)?;
                k += 1.0;
            }
            let mut k = length;
            while k > length - delete_count + item_count {
                self.delete_index_or_throw(dom, receiver, k - 1.0)?;
                k -= 1.0;
            }
        } else if item_count > delete_count {
            let mut k = length - delete_count;
            while k > start {
                let from = k + delete_count - 1.0;
                let to = k + item_count - 1.0;
                self.move_index(dom, receiver, from, to)?;
                k -= 1.0;
            }
        }
        for (offset, item) in arguments.iter().skip(2).enumerate() {
            self.set_index_or_throw(dom, receiver, start + offset as f64, item.clone())?;
        }
        self.set_length_or_throw(dom, receiver, length - delete_count + item_count)?;
        Ok(JsValue::Object(removed))
    }

    /// One step of the relocation loops in `splice`, `copyWithin` and
    /// `unshift`: move `from` to `to` when `from` exists, otherwise delete `to`.
    fn move_index(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        from: f64,
        to: f64,
    ) -> Result<(), JsError> {
        if self.has_index(dom, object, from)? {
            let value = self.get_index(dom, object, from)?;
            self.set_index_or_throw(dom, object, to, value)
        } else {
            self.delete_index_or_throw(dom, object, to)
        }
    }

    /// `Array.prototype.reverse` (§23.1.3.24).
    pub(in crate::runtime) fn array_reverse(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
    ) -> Result<JsValue, JsError> {
        let length = self.length_of_array_like(dom, receiver)?;
        let middle = (length / 2.0).floor();
        let mut lower = 0.0;
        while lower != middle {
            let upper = length - lower - 1.0;
            let lower_exists = self.has_index(dom, receiver, lower)?;
            let lower_value = if lower_exists {
                self.get_index(dom, receiver, lower)?
            } else {
                JsValue::Undefined
            };
            let upper_exists = self.has_index(dom, receiver, upper)?;
            let upper_value = if upper_exists {
                self.get_index(dom, receiver, upper)?
            } else {
                JsValue::Undefined
            };
            match (lower_exists, upper_exists) {
                (true, true) => {
                    self.set_index_or_throw(dom, receiver, lower, upper_value)?;
                    self.set_index_or_throw(dom, receiver, upper, lower_value)?;
                }
                (false, true) => {
                    self.set_index_or_throw(dom, receiver, lower, upper_value)?;
                    self.delete_index_or_throw(dom, receiver, upper)?;
                }
                (true, false) => {
                    self.delete_index_or_throw(dom, receiver, lower)?;
                    self.set_index_or_throw(dom, receiver, upper, lower_value)?;
                }
                (false, false) => {}
            }
            lower += 1.0;
        }
        Ok(JsValue::Object(receiver))
    }

    /// ECMA-262 22.1.3.31 `Array.prototype.at`.
    ///
    /// `at` is the one indexed method whose negative argument counts *from the
    /// end*, and a polyfill that treats it as `elementAt` is off by one on every
    /// call with a negative index - which is most of the calls, since that is the
    /// only reason to use `at` at all.
    pub(in crate::runtime) fn array_at(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let length = self.length_of_array_like(dom, receiver)?;
        let relative = match arguments.first() {
            Some(value) => self.to_integer_value(dom, value)?,
            None => 0.0,
        };
        let index = if relative >= 0.0 {
            relative
        } else {
            length + relative
        };
        if index < 0.0 || index >= length {
            return Ok(JsValue::Undefined);
        }
        self.get_index(dom, receiver, index)
    }

    /// `Array.prototype.reduce` and `reduceRight` (§23.1.3.19, §23.1.3.20 in
    /// ES2024 numbering): the walk runs from the start or from the end, and the
    /// accumulator is the initial value when one is passed.
    fn array_reduce(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
        from_end: bool,
    ) -> Result<JsValue, JsError> {
        let name = if from_end { "reduceRight" } else { "reduce" };
        let length = self.length_of_array_like(dom, receiver)?;
        let callback =
            Self::require_callable_object(required_argument(arguments, 0, name)?, &self.realm)?;
        let step = if from_end { -1.0 } else { 1.0 };
        let in_range = |index: f64| {
            if from_end {
                index >= 0.0
            } else {
                index < length
            }
        };
        let mut index = if from_end { length - 1.0 } else { 0.0 };
        let mut accumulator = if arguments.len() >= 2 {
            arguments[1].clone()
        } else {
            let mut found = None;
            while in_range(index) {
                if self.has_index(dom, receiver, index)? {
                    found = Some(self.get_index(dom, receiver, index)?);
                    index += step;
                    break;
                }
                index += step;
            }
            found
                .ok_or_else(|| JsError::type_error("Reduce of empty array with no initial value"))?
        };
        while in_range(index) {
            if self.has_index(dom, receiver, index)? {
                let value = self.get_index(dom, receiver, index)?;
                accumulator = self.call(
                    dom,
                    callback,
                    &[
                        accumulator,
                        value,
                        JsValue::Number(index),
                        JsValue::Object(receiver),
                    ],
                )?;
            }
            index += step;
        }
        Ok(accumulator)
    }

    /// `find`, `findIndex`, `findLast` and `findLastIndex` (§23.1.3.x): every
    /// index is read, holes included, and the first truthy callback result wins.
    fn array_find(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
        from_end: bool,
        want_index: bool,
    ) -> Result<JsValue, JsError> {
        let name = match (from_end, want_index) {
            (false, false) => "Array.find",
            (false, true) => "Array.findIndex",
            (true, false) => "Array.findLast",
            (true, true) => "Array.findLastIndex",
        };
        let length = self.length_of_array_like(dom, receiver)?;
        let callback =
            Self::require_callable_object(required_argument(arguments, 0, name)?, &self.realm)?;
        let this_argument = callback_this_argument(arguments);
        let mut position = 0.0;
        while position < length {
            let index = if from_end {
                length - 1.0 - position
            } else {
                position
            };
            let value = self.get_index(dom, receiver, index)?;
            let matched = self.call_with_this(
                dom,
                callback,
                &[
                    value.clone(),
                    JsValue::Number(index),
                    JsValue::Object(receiver),
                ],
                this_argument.clone(),
            )?;
            if matched.is_truthy() {
                return Ok(if want_index {
                    JsValue::Number(index)
                } else {
                    value
                });
            }
            position += 1.0;
        }
        Ok(if want_index {
            JsValue::Number(-1.0)
        } else {
            JsValue::Undefined
        })
    }

    /// `forEach`, `map`, `filter`, `some` and `every` (§23.1.3.x): one walk over
    /// the present indices, with the per-method result built as it goes.
    fn array_visit(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
        visit: Visit,
        name: &str,
    ) -> Result<JsValue, JsError> {
        let length = self.length_of_array_like(dom, receiver)?;
        let callback =
            Self::require_callable_object(required_argument(arguments, 0, name)?, &self.realm)?;
        let this_argument = callback_this_argument(arguments);
        let result = match visit {
            Visit::Map => Some(self.array_species_create(dom, receiver, length)?),
            Visit::Filter => Some(self.array_species_create(dom, receiver, 0.0)?),
            _ => None,
        };
        let mut to = 0.0;
        let mut index = 0.0;
        while index < length {
            if self.has_index(dom, receiver, index)? {
                let value = self.get_index(dom, receiver, index)?;
                let mapped = self.call_with_this(
                    dom,
                    callback,
                    &[
                        value.clone(),
                        JsValue::Number(index),
                        JsValue::Object(receiver),
                    ],
                    this_argument.clone(),
                )?;
                match visit {
                    Visit::ForEach => {}
                    Visit::Map => {
                        if let Some(result) = result {
                            self.create_data_property_or_throw(dom, result, index, mapped)?;
                        }
                    }
                    Visit::Filter => {
                        if mapped.is_truthy()
                            && let Some(result) = result
                        {
                            self.create_data_property_or_throw(dom, result, to, value)?;
                            to += 1.0;
                        }
                    }
                    Visit::Some => {
                        if mapped.is_truthy() {
                            return Ok(JsValue::Boolean(true));
                        }
                    }
                    Visit::Every => {
                        if !mapped.is_truthy() {
                            return Ok(JsValue::Boolean(false));
                        }
                    }
                }
            }
            index += 1.0;
        }
        Ok(match visit {
            Visit::ForEach => JsValue::Undefined,
            Visit::Map | Visit::Filter => match result {
                Some(result) => JsValue::Object(result),
                None => JsValue::Undefined,
            },
            Visit::Some => JsValue::Boolean(false),
            Visit::Every => JsValue::Boolean(true),
        })
    }

    /// `Array.prototype.flat` (§23.1.3.12).
    pub(in crate::runtime) fn array_flat(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let length = self.length_of_array_like(dom, receiver)?;
        let depth = match arguments.first() {
            None | Some(JsValue::Undefined) => 1.0,
            Some(value) => self.to_integer_value(dom, value)?.max(0.0),
        };
        let result = self.array_species_create(dom, receiver, 0.0)?;
        self.flatten_into_array(dom, result, receiver, length, 0.0, depth, None, 0)?;
        Ok(JsValue::Object(result))
    }

    /// `Array.prototype.flatMap` (§23.1.3.13).
    fn array_flat_map(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let length = self.length_of_array_like(dom, receiver)?;
        let mapper = Self::require_callable_object(
            required_argument(arguments, 0, "flatMap")?,
            &self.realm,
        )?;
        let this_argument = callback_this_argument(arguments);
        let result = self.array_species_create(dom, receiver, 0.0)?;
        self.flatten_into_array(
            dom,
            result,
            receiver,
            length,
            0.0,
            1.0,
            Some((mapper, &this_argument)),
            0,
        )?;
        Ok(JsValue::Object(result))
    }

    /// `FlattenIntoArray` (§23.1.3.13.1): appends the present elements of
    /// `source` to `target`, descending into arrays while `depth` allows.
    #[allow(clippy::too_many_arguments)]
    fn flatten_into_array(
        &mut self,
        dom: &mut Dom,
        target: ObjectId,
        source: ObjectId,
        source_length: f64,
        start: f64,
        depth: f64,
        mapper: Option<(ObjectId, &JsValue)>,
        nesting: usize,
    ) -> Result<f64, JsError> {
        if nesting > MAX_FLATTEN_DEPTH {
            return Err(JsError::resource("flat nesting exceeds the engine bound"));
        }
        let mut target_index = start;
        let mut source_index = 0.0;
        while source_index < source_length {
            if self.has_index(dom, source, source_index)? {
                let mut element = self.get_index(dom, source, source_index)?;
                if let Some((mapper, this_argument)) = mapper {
                    element = self.call_with_this(
                        dom,
                        mapper,
                        &[
                            element,
                            JsValue::Number(source_index),
                            JsValue::Object(source),
                        ],
                        this_argument.clone(),
                    )?;
                }
                let nested = match element {
                    JsValue::Object(inner) if depth > 0.0 && self.is_array_value(&element)? => {
                        Some(inner)
                    }
                    _ => None,
                };
                if let Some(inner) = nested {
                    let inner_depth = if depth.is_infinite() {
                        depth
                    } else {
                        depth - 1.0
                    };
                    let inner_length = self.length_of_array_like(dom, inner)?;
                    target_index = self.flatten_into_array(
                        dom,
                        target,
                        inner,
                        inner_length,
                        target_index,
                        inner_depth,
                        None,
                        nesting + 1,
                    )?;
                } else {
                    if target_index >= MAX_SAFE_INTEGER {
                        return Err(JsError::type_error("array length exceeds 2^53 - 1"));
                    }
                    self.create_data_property_or_throw(dom, target, target_index, element)?;
                    target_index += 1.0;
                }
            }
            source_index += 1.0;
        }
        Ok(target_index)
    }

    /// `SortCompare` (§23.1.3.30.2) for two defined values.
    fn sort_compare(
        &mut self,
        dom: &mut Dom,
        comparator: Option<ObjectId>,
        left: &JsValue,
        right: &JsValue,
    ) -> Result<f64, JsError> {
        if let Some(function) = comparator {
            let result = self.call(dom, function, &[left.clone(), right.clone()])?;
            let number = self.to_number_value(dom, &result)?;
            return Ok(if number.is_nan() { 0.0 } else { number });
        }
        let left = utf16::utf16_units(&self.to_string_value(dom, left)?);
        let right = utf16::utf16_units(&self.to_string_value(dom, right)?);
        Ok(match left.cmp(&right) {
            std::cmp::Ordering::Less => -1.0,
            std::cmp::Ordering::Equal => 0.0,
            std::cmp::Ordering::Greater => 1.0,
        })
    }

    /// Stable merge sort of `values` under `SortCompare`. `undefined` is not
    /// compared: it is appended after every defined value, as the spec requires.
    fn sort_values(
        &mut self,
        dom: &mut Dom,
        values: Vec<JsValue>,
        comparator: Option<ObjectId>,
    ) -> Result<Vec<JsValue>, JsError> {
        let undefined_count = values
            .iter()
            .filter(|value| matches!(value, JsValue::Undefined))
            .count();
        let defined: Vec<JsValue> = values
            .into_iter()
            .filter(|value| !matches!(value, JsValue::Undefined))
            .collect();
        let mut sorted = self.merge_sort(dom, defined, comparator)?;
        sorted.extend(std::iter::repeat_n(JsValue::Undefined, undefined_count));
        Ok(sorted)
    }

    fn merge_sort(
        &mut self,
        dom: &mut Dom,
        mut values: Vec<JsValue>,
        comparator: Option<ObjectId>,
    ) -> Result<Vec<JsValue>, JsError> {
        if values.len() <= 1 {
            return Ok(values);
        }
        let right = values.split_off(values.len() / 2);
        let left = self.merge_sort(dom, values, comparator)?;
        let right = self.merge_sort(dom, right, comparator)?;
        let mut merged = Vec::with_capacity(left.len() + right.len());
        let mut left = left.into_iter().peekable();
        let mut right = right.into_iter().peekable();
        while let (Some(l), Some(r)) = (left.peek(), right.peek()) {
            if self.sort_compare(dom, comparator, l, r)? <= 0.0 {
                merged.extend(left.next());
            } else {
                merged.extend(right.next());
            }
        }
        merged.extend(left);
        merged.extend(right);
        Ok(merged)
    }

    fn comparator_argument(&self, arguments: &[JsValue]) -> Result<Option<ObjectId>, JsError> {
        match arguments.first() {
            None | Some(JsValue::Undefined) => Ok(None),
            Some(value) => Self::require_callable_object(value, &self.realm).map(Some),
        }
    }

    /// `Array.prototype.sort` (§23.1.3.30).
    pub(in crate::runtime) fn array_sort(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let comparator = self.comparator_argument(arguments)?;
        let length = self.length_of_array_like(dom, receiver)?;
        let mut items = Vec::new();
        let mut index = 0.0;
        while index < length {
            if self.has_index(dom, receiver, index)? {
                items.push(self.get_index(dom, receiver, index)?);
            }
            index += 1.0;
        }
        let item_count = items.len() as f64;
        let sorted = self.sort_values(dom, items, comparator)?;
        for (position, value) in sorted.into_iter().enumerate() {
            self.set_index_or_throw(dom, receiver, position as f64, value)?;
        }
        let mut index = item_count;
        while index < length {
            self.delete_index_or_throw(dom, receiver, index)?;
            index += 1.0;
        }
        Ok(JsValue::Object(receiver))
    }

    /// `Array.prototype.toSorted` (§23.1.3.33).
    fn array_to_sorted(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let comparator = self.comparator_argument(arguments)?;
        let length = self.length_of_array_like(dom, receiver)?;
        let result = self.create_array_with_length(length)?;
        let mut items = Vec::new();
        let mut index = 0.0;
        while index < length {
            items.push(self.get_index(dom, receiver, index)?);
            index += 1.0;
        }
        let sorted = self.sort_values(dom, items, comparator)?;
        for (position, value) in sorted.into_iter().enumerate() {
            self.create_data_property_or_throw(dom, result, position as f64, value)?;
        }
        Ok(JsValue::Object(result))
    }

    /// `Array.prototype.toReversed` (§23.1.3.34).
    fn array_to_reversed(&mut self, dom: &mut Dom, receiver: ObjectId) -> Result<JsValue, JsError> {
        let length = self.length_of_array_like(dom, receiver)?;
        let result = self.create_array_with_length(length)?;
        let mut index = 0.0;
        while index < length {
            let value = self.get_index(dom, receiver, length - index - 1.0)?;
            self.create_data_property_or_throw(dom, result, index, value)?;
            index += 1.0;
        }
        Ok(JsValue::Object(result))
    }

    /// `Array.prototype.toSpliced` (§23.1.3.35).
    fn array_to_spliced(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let length = self.length_of_array_like(dom, receiver)?;
        let start = self.relative_bound(dom, arguments.first(), length, 0.0)?;
        let item_count = arguments.len().saturating_sub(2) as f64;
        let skip_count = match arguments.len() {
            0 => 0.0,
            1 => length - start,
            _ => {
                let raw = self.to_integer_value(dom, &arguments[1])?;
                raw.clamp(0.0, length - start)
            }
        };
        let new_length = length + item_count - skip_count;
        if new_length > MAX_SAFE_INTEGER {
            return Err(JsError::type_error("array length exceeds 2^53 - 1"));
        }
        let result = self.create_array_with_length(new_length)?;
        let mut index = 0.0;
        while index < start {
            let value = self.get_index(dom, receiver, index)?;
            self.create_data_property_or_throw(dom, result, index, value)?;
            index += 1.0;
        }
        for item in arguments.iter().skip(2) {
            self.create_data_property_or_throw(dom, result, index, item.clone())?;
            index += 1.0;
        }
        let mut source = start + skip_count;
        while index < new_length {
            let value = self.get_index(dom, receiver, source)?;
            self.create_data_property_or_throw(dom, result, index, value)?;
            index += 1.0;
            source += 1.0;
        }
        Ok(JsValue::Object(result))
    }

    /// `Array.prototype.with` (§23.1.3.37).
    fn array_with(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let length = self.length_of_array_like(dom, receiver)?;
        let relative = match arguments.first() {
            Some(value) => self.to_integer_value(dom, value)?,
            None => 0.0,
        };
        let actual = if relative >= 0.0 {
            relative
        } else {
            length + relative
        };
        if actual >= length || actual < 0.0 {
            return Err(self.range_error("Invalid index"));
        }
        let replacement = arguments.get(1).cloned().unwrap_or(JsValue::Undefined);
        let result = self.create_array_with_length(length)?;
        let mut index = 0.0;
        while index < length {
            let value = if index == actual {
                replacement.clone()
            } else {
                self.get_index(dom, receiver, index)?
            };
            self.create_data_property_or_throw(dom, result, index, value)?;
            index += 1.0;
        }
        Ok(JsValue::Object(result))
    }

    /// `Array.prototype.fill` (§23.1.3.8).
    fn array_fill(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let length = self.length_of_array_like(dom, receiver)?;
        let value = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let start = self.relative_bound(dom, arguments.get(1), length, 0.0)?;
        let end = self.relative_bound(dom, arguments.get(2), length, length)?;
        let mut index = start;
        while index < end {
            self.set_index_or_throw(dom, receiver, index, value.clone())?;
            index += 1.0;
        }
        Ok(JsValue::Object(receiver))
    }

    /// `Array.prototype.copyWithin` (§23.1.3.4).
    fn array_copy_within(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let length = self.length_of_array_like(dom, receiver)?;
        let mut to = self.relative_bound(dom, arguments.first(), length, 0.0)?;
        let mut from = self.relative_bound(dom, arguments.get(1), length, 0.0)?;
        let final_index = self.relative_bound(dom, arguments.get(2), length, length)?;
        let mut count = (final_index - from).min(length - to);
        let direction = if from < to && to < from + count {
            from += count - 1.0;
            to += count - 1.0;
            -1.0
        } else {
            1.0
        };
        while count > 0.0 {
            self.move_index(dom, receiver, from, to)?;
            from += direction;
            to += direction;
            count -= 1.0;
        }
        Ok(JsValue::Object(receiver))
    }

    /// `Array.prototype.concat` (§23.1.3.2) with `IsConcatSpreadable`.
    fn array_concat(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let result = self.array_species_create(dom, receiver, 0.0)?;
        let mut next = 0.0;
        let items = std::iter::once(JsValue::Object(receiver)).chain(arguments.iter().cloned());
        for item in items {
            let spreadable = self.is_concat_spreadable(dom, &item)?;
            let object = match item {
                JsValue::Object(object) if spreadable => object,
                other => {
                    self.create_data_property_or_throw(dom, result, next, other)?;
                    next += 1.0;
                    continue;
                }
            };
            let length = self.length_of_array_like(dom, object)?;
            if next + length > MAX_SAFE_INTEGER {
                return Err(JsError::type_error("array length exceeds 2^53 - 1"));
            }
            let mut index = 0.0;
            while index < length {
                if self.has_index(dom, object, index)? {
                    let value = self.get_index(dom, object, index)?;
                    self.create_data_property_or_throw(dom, result, next, value)?;
                }
                next += 1.0;
                index += 1.0;
            }
        }
        self.set_length_or_throw(dom, result, next)?;
        Ok(JsValue::Object(result))
    }

    /// `IsConcatSpreadable` (§23.1.3.2.1).
    fn is_concat_spreadable(&mut self, dom: &mut Dom, value: &JsValue) -> Result<bool, JsError> {
        let JsValue::Object(object) = value else {
            return Ok(false);
        };
        let flag =
            self.get_symbol_value(dom, *object, &JsSymbol::well_known("@@isConcatSpreadable"))?;
        if !matches!(flag, JsValue::Undefined) {
            return Ok(flag.is_truthy());
        }
        self.is_array_value(value)
    }

    /// `Array.prototype.shift` (§23.1.3.27 in ES2024 numbering).
    pub(in crate::runtime) fn array_shift(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
    ) -> Result<JsValue, JsError> {
        let length = self.length_of_array_like(dom, receiver)?;
        if length == 0.0 {
            self.set_length_or_throw(dom, receiver, 0.0)?;
            return Ok(JsValue::Undefined);
        }
        let first = self.get_index(dom, receiver, 0.0)?;
        let mut index = 1.0;
        while index < length {
            self.move_index(dom, receiver, index, index - 1.0)?;
            index += 1.0;
        }
        self.delete_index_or_throw(dom, receiver, length - 1.0)?;
        self.set_length_or_throw(dom, receiver, length - 1.0)?;
        Ok(first)
    }

    /// `Array.prototype.unshift` (§23.1.3.37 in ES2024 numbering).
    pub(in crate::runtime) fn array_unshift(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let length = self.length_of_array_like(dom, receiver)?;
        let count = arguments.len() as f64;
        if count > 0.0 {
            if length + count > MAX_SAFE_INTEGER {
                return Err(JsError::type_error("array length exceeds 2^53 - 1"));
            }
            let mut index = length;
            while index > 0.0 {
                self.move_index(dom, receiver, index - 1.0, index + count - 1.0)?;
                index -= 1.0;
            }
            for (offset, value) in arguments.iter().enumerate() {
                self.set_index_or_throw(dom, receiver, offset as f64, value.clone())?;
            }
        }
        self.set_length_or_throw(dom, receiver, length + count)?;
        Ok(JsValue::Number(length + count))
    }

    /// `Array.from` (§23.1.2.1): an iterable is walked through its iterator, an
    /// array-like by index, and `this` decides the result's constructor.
    pub(in crate::runtime) fn array_from(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let items = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let mapper = match arguments.get(1) {
            None | Some(JsValue::Undefined) => None,
            Some(value) => Some(Self::require_callable_object(value, &self.realm)?),
        };
        let this_argument = arguments.get(2).cloned().unwrap_or(JsValue::Undefined);
        let constructor = self.is_constructor(receiver).then_some(receiver);
        let object = self.to_object(&items)?;
        let method = self.get_symbol_value(dom, object, &JsSymbol::well_known("@@iterator"))?;
        match method {
            JsValue::Undefined | JsValue::Null => {
                let length = self.length_of_array_like(dom, object)?;
                let result = self.array_from_constructor(dom, constructor, Some(length))?;
                let mut index = 0.0;
                while index < length {
                    let value = self.get_index(dom, object, index)?;
                    let value = self.map_array_value(dom, mapper, &this_argument, value, index)?;
                    self.create_data_property_or_throw(dom, result, index, value)?;
                    index += 1.0;
                }
                self.set_length_or_throw(dom, result, length)?;
                Ok(JsValue::Object(result))
            }
            JsValue::Object(method) if Self::is_callable_object(method, &self.realm) => {
                let result = self.array_from_constructor(dom, constructor, None)?;
                let iterator = match self.call_with_this(dom, method, &[], items.clone())? {
                    JsValue::Object(iterator) => iterator,
                    _ => return Err(JsError::type_error("iterator is not an object")),
                };
                let next = self.get_member(dom, iterator, "next")?;
                let mut index = 0.0;
                loop {
                    let JsValue::Object(next) = next.clone() else {
                        return Err(JsError::type_error("iterator has no callable 'next'"));
                    };
                    let step = self.call_with_this(dom, next, &[], JsValue::Object(iterator))?;
                    let JsValue::Object(step) = step else {
                        return Err(JsError::type_error("iterator result is not an object"));
                    };
                    if self.get_member(dom, step, "done")?.is_truthy() {
                        break;
                    }
                    let value = self.get_member(dom, step, "value")?;
                    let outcome = self
                        .map_array_value(dom, mapper, &this_argument, value, index)
                        .and_then(|mapped| {
                            self.create_data_property_or_throw(dom, result, index, mapped)
                        });
                    if let Err(error) = outcome {
                        let _ = self.close_iterator_object(dom, iterator);
                        return Err(error);
                    }
                    index += 1.0;
                }
                self.set_length_or_throw(dom, result, index)?;
                Ok(JsValue::Object(result))
            }
            _ => Err(JsError::type_error("Symbol.iterator is not a function")),
        }
    }

    /// `Array.fromAsync(asyncItems, mapfn, thisArg)` (ES2026 §23.1.2.2): the
    /// spec's async abstract closure, run as an async function built from
    /// [`FROM_ASYNC_SOURCE`]. Its awaits, promise jobs and iterator closing are
    /// the engine's own async machinery. The intrinsics it uses are passed in,
    /// so a later change to a global does not reach the closure, and the result
    /// is always a promise: every failure, including a bad `mapfn`, rejects it.
    pub(in crate::runtime) fn array_from_async(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let items = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let mapfn = arguments.get(1).cloned().unwrap_or(JsValue::Undefined);
        let this_argument = arguments.get(2).cloned().unwrap_or(JsValue::Undefined);
        let is_constructor = self.is_constructor(receiver);
        let JsValue::Object(closure) = self.evaluate_function_source(dom, FROM_ASYNC_SOURCE)?
        else {
            return Err(JsError::type_error("Array.fromAsync closure did not build"));
        };
        let array = self.realm.global("Array").unwrap_or(JsValue::Undefined);
        let define_property = match self.realm.global("Object") {
            Some(JsValue::Object(object)) => self.get_member(dom, object, "defineProperty")?,
            _ => JsValue::Undefined,
        };
        let closure_arguments = [
            JsValue::Object(receiver),
            items,
            mapfn,
            this_argument,
            JsValue::Boolean(is_constructor),
            JsValue::Symbol(JsSymbol::well_known("@@asyncIterator")),
            JsValue::Symbol(JsSymbol::well_known("@@iterator")),
            array,
            define_property,
        ];
        self.call(dom, closure, &closure_arguments)
    }

    /// `Array.of` (§23.1.2.3).
    fn array_of(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let constructor = self.is_constructor(receiver).then_some(receiver);
        let length = arguments.len() as f64;
        let result = self.array_from_constructor(dom, constructor, Some(length))?;
        for (index, value) in arguments.iter().enumerate() {
            self.create_data_property_or_throw(dom, result, index as f64, value.clone())?;
        }
        self.set_length_or_throw(dom, result, length)?;
        Ok(JsValue::Object(result))
    }

    /// The result object of `Array.from` / `Array.of`: `Construct(C, [length])`
    /// when `this` is a constructor, otherwise a plain array.
    fn array_from_constructor(
        &mut self,
        dom: &mut Dom,
        constructor: Option<ObjectId>,
        length: Option<f64>,
    ) -> Result<ObjectId, JsError> {
        match constructor {
            Some(constructor) => {
                let arguments: Vec<JsValue> = length.map(JsValue::Number).into_iter().collect();
                match self.construct(dom, constructor, &arguments)? {
                    JsValue::Object(result) => Ok(result),
                    _ => Err(JsError::type_error("constructor returned a primitive")),
                }
            }
            None => self.create_array_with_length(length.unwrap_or(0.0)),
        }
    }

    fn map_array_value(
        &mut self,
        dom: &mut Dom,
        mapper: Option<ObjectId>,
        this_argument: &JsValue,
        value: JsValue,
        index: f64,
    ) -> Result<JsValue, JsError> {
        match mapper {
            Some(mapper) => self.call_with_this(
                dom,
                mapper,
                &[value, JsValue::Number(index)],
                this_argument.clone(),
            ),
            None => Ok(value),
        }
    }

    /// The `length` of an Array exotic object as the engine's u32 length slot.
    /// A generic array-like that claims a longer (or non-finite) length cannot
    /// be represented here, so it is a catchable error.
    pub(in crate::runtime) fn array_length(&self, object: ObjectId) -> Result<u32, JsError> {
        let value = self
            .realm
            .get_property(object, "length")
            .or_else(|| match self.realm.host(object) {
                Some(ObjectHost::StringPrimitive(text)) =>
                {
                    #[allow(clippy::cast_precision_loss)]
                    Some(JsValue::Number(utf16::utf16_length(&text) as f64))
                }
                _ => None,
            })
            .unwrap_or(JsValue::Undefined);
        let number = to_number(&value)?;
        if number.is_nan() || number <= 0.0 {
            return Ok(0);
        }
        // The engine's arrays carry a u32 length. A generic array-like that
        // claims a longer (or non-finite) length cannot be represented; a
        // catchable error beats silently truncating to u32::MAX and then
        // materializing four billion elements.
        if !number.is_finite() || number > f64::from(u32::MAX) {
            return Err(JsError::resource(
                "array-like length exceeds the engine bound",
            ));
        }
        Ok(number.trunc() as u32)
    }

    pub(in crate::runtime) fn set_array_length_value(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        value: &JsValue,
    ) -> Result<(), JsError> {
        // ECMA-262 10.4.2.4 ArraySetLength: `newLen = ToUint32(value)` and then
        // `numberLen = ToNumber(value)`, so an object's `valueOf` runs twice.
        // A mismatch between the two is a RangeError.
        let uint32_length = uint32_of_number(self.to_number_value(dom, value)?);
        let number = self.to_number_value(dom, value)?;
        if f64::from(uint32_length) != number {
            return Err(self.range_error("Invalid array length"));
        }
        let length = uint32_length;
        let old_length = self.array_length(object)?;
        if length < old_length {
            for name in self.realm.own_property_names(object).unwrap_or_default() {
                if let Some(index) = array_index(&name)
                    && index >= length
                    && !self.realm.delete_property(object, &name)
                {
                    return Err(JsError::type_error("could not shrink array length"));
                }
            }
        }
        self.set_array_length(object, length)
    }

    pub(in crate::runtime) fn set_array_length(
        &mut self,
        object: ObjectId,
        length: u32,
    ) -> Result<(), JsError> {
        if self.realm.set_property(
            object,
            "length".to_owned(),
            JsValue::Number(f64::from(length)),
        ) {
            Ok(())
        } else {
            Err(JsError::type_error("array length is not writable"))
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::runtime::JsRuntime;
    use render_html::parse_document;

    fn run(source: &str) -> String {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        match runtime.execute(&mut parsed.dom, source) {
            Ok(outcome) => outcome.value.to_js_string(),
            Err(error) => format!("<threw {}>", error.message()),
        }
    }

    fn caught(expression: &str) -> String {
        run(&format!(
            "var out = 'no throw'; try {{ {expression} }} \
             catch (e) {{ out = e.name + ': ' + e.message; }} out"
        ))
    }

    /// `at` is the one indexed method whose negative argument counts from the end,
    /// and reading it as `elementAt` is off by one on exactly the calls that use
    /// it - a negative index is the only reason to call `at` at all.
    #[test]
    fn array_at_counts_negative_indices_from_the_end() {
        assert_eq!(
            run("[10,20,30].at(0) + ':' + [10,20,30].at(-1) + ':' + [10,20,30].at(2)"),
            "10:30:30"
        );
        assert_eq!(
            run("String([10,20,30].at(3)) + '|' + String([10,20,30].at(-4))"),
            "undefined|undefined"
        );
        assert_eq!(run("String([].at(0))"), "undefined");
        // A fractional index truncates, and `NaN` truncates to 0.
        assert_eq!(run("[1,2].at(0.9) + ':' + [1,2].at(NaN)"), "1:1");
        // A sparse array answers `undefined` for a hole rather than stopping.
        assert_eq!(
            run("var a = [1]; a[3] = 4; String(a.at(1)) + '|' + a.at(3)"),
            "undefined|4"
        );
    }

    /// `flat`'s `depth` argument is the part an engine that only answers `flat()`
    /// gets wrong, and it is wrong silently: the result is still an array. The
    /// `.length` assertions are the load-bearing ones, because `join` flattens
    /// nested arrays however deep they are.
    #[test]
    fn flat_honours_its_depth_argument() {
        // Three levels: one level removed leaves `[1, 2, [3]]`, which
        // `JSON.stringify` shows with its surviving inner array.
        assert_eq!(run("[1,[2,[3]]].flat().length"), "3");
        assert_eq!(run("[1,[2,[3]]].flat(1).length"), "3");
        assert_eq!(run("JSON.stringify([1,[2,[3]]].flat(1))"), "[1,2,[3]]");
        assert_eq!(run("[1,[2,[3]]].flat(2).join(',')"), "1,2,3");
        // Four levels, one step at a time: the depth is the number of levels
        // removed, and each level is observable.
        assert_eq!(run("var a = [1,[2,[3,[4]] ]]; a.flat(1).length"), "3");
        assert_eq!(run("var a = [1,[2,[3,[4]] ]]; a.flat(2).length"), "4");
        assert_eq!(run("var a = [1,[2,[3,[4]] ]]; a.flat(3).length"), "4");
        assert_eq!(run("var a = [1,[2,[3,[4]] ]]; a.flat(4).length"), "4");
        assert_eq!(run("var a = [1,[2,[3,[4]] ]]; a.flat(0).length"), "2");
        assert_eq!(run("[[1],[2]].flat(Infinity).length"), "2");
        // `flat` returns a new array and leaves the receiver alone.
        assert_eq!(run("var a = [1,[2]]; a.flat(); a.length"), "2");
        // Only a genuine array nests, which is the distinction `flat` exists for:
        // a string is spread as its characters, not descended into, and a hole
        // is removed.
        assert_eq!(run("[['ab']].flat().join('|')"), "ab");
        // A hole is not a present element, so `flat` drops it, while `map` keeps
        // the length and the hole.
        assert_eq!(run("var a = [1, , 2]; a.flat().length"), "2");
        assert_eq!(
            run("var b = [1, , 2]; b.map(function (v) { return v; }).length"),
            "3"
        );
        assert_eq!(run("var a = [1, , 2]; a.length"), "3");
        assert_eq!(run("String(1 in [1, , 2])"), "false");
    }

    /// `reduceRight` walks from the end, and the index the callback sees is the
    /// index it was called with. Reversing a copy and reducing would get the
    /// accumulator order right and the observed indices wrong.
    #[test]
    fn reduce_right_walks_from_the_end_and_reports_real_indices() {
        assert_eq!(
            run("['a','b','c'].reduceRight(function (acc, value, index) { \
                 acc.push(value + index); return acc; }, []).join(',')"),
            "c2,b1,a0"
        );
        assert_eq!(
            run("[1,2,3].reduceRight(function (a, b) { return a + b; })"),
            "6"
        );
        assert_eq!(
            run("[1,2,3].reduceRight(function (a, b) { return a + b; }, 10)"),
            "16"
        );
        // The accumulator is the *last* element, so the callback never runs and
        // an empty array with no initial value is the same TypeError `reduce`
        // throws.
        assert_eq!(
            caught("[].reduceRight(function (a, b) { return a + b; })"),
            "TypeError: Reduce of empty array with no initial value"
        );
        // A hole is treated exactly as `reduce` treats it, which is the property
        // that matters here: the two must not disagree about which positions the
        // callback is called for. (What they agree *on* is the engine's own
        // sparse-array model, which is a separate question.)
        assert_eq!(
            run("var a = [1, , 3]; a.reduce(function (x, y) { return x + y; })"),
            run("var b = [1, , 3]; b.reduceRight(function (x, y) { return x + y; })")
        );
    }

    /// `findLast`/`findLastIndex` are `find`/`findIndex` with the iteration order
    /// reversed, so the *last* match wins - which is the only reason to have
    /// them.
    #[test]
    fn find_last_reports_the_last_match() {
        assert_eq!(
            run("[1,2,3,2].findLast(function (v) { return v === 2; })"),
            "2"
        );
        assert_eq!(
            run("[1,2,3,2].findIndex(function (v) { return v === 2; })"),
            "1"
        );
        assert_eq!(
            run("[1,2,3,2].findLastIndex(function (v) { return v === 2; })"),
            "3"
        );
        // No match is `undefined` and `-1`, as the two halves each specify.
        assert_eq!(
            run("String([1].findLast(function () { return false; }))"),
            "undefined"
        );
        assert_eq!(
            run("[1].findLastIndex(function () { return false; })"),
            "-1"
        );
        // A truthy return is enough, and the index the callback sees is the
        // reversed one.
        assert_eq!(
            run("[10,20,30].findLast(function (v, i) { return i === 1; })"),
            "20"
        );
        // A missing callback is a TypeError from `Call`.
        assert_eq!(
            caught("[1].findLast()"),
            "TypeError: Array.findLast requires at least 1 argument(s)"
        );
    }

    /// The copy and search methods read through the same generic operations as
    /// the rest, so their results are the spec's, element by element.
    #[test]
    fn copy_and_search_methods_follow_the_spec() {
        assert_eq!(
            run("[1,2,3,2].lastIndexOf(2) + ':' + [1,2,3,2].lastIndexOf(2, -3)"),
            "3:1"
        );
        assert_eq!(run("[1,2,3,4,5].copyWithin(0, 3).join()"), "4,5,3,4,5");
        assert_eq!(run("[1,2,3].fill(0, 1).join()"), "1,0,0");
        assert_eq!(
            run("[1,[2,[3]]].flatMap(function (x) { return [x, x]; }).length"),
            "4"
        );
        assert_eq!(
            run("[3,1,2].toSorted().join() + '|' + [3,1,2].join()"),
            "1,2,3|3,1,2"
        );
        assert_eq!(
            run("[1,2,3].toReversed().join() + '|' + [1,2,3,4].toSpliced(1, 2, 'x').join()"),
            "3,2,1|1,x,4"
        );
        assert_eq!(run("[1,2,3].with(-1, 9).join()"), "1,2,9");
        assert_eq!(caught("[1].with(3, 0)"), "RangeError: Invalid index");
        // A `-0` start reports `+0`, which `Object.is` tells apart.
        assert_eq!(run("Object.is([1].indexOf(1, -0), 0)"), "true");
        // The methods work on an array-like through `Get`, `HasProperty` and `Set`.
        assert_eq!(
            run(
                "Array.prototype.map.call({length: 2, 0: 'a', 1: 'b'}, function (v) { return v + '!'; }).join()"
            ),
            "a!,b!"
        );
        // `reduce` with an explicit `undefined` initial value starts from it.
        assert_eq!(
            run("String([1, 2].reduce(function (a, b) { return a + b; }, undefined))"),
            "NaN"
        );
    }

    /// `map`, `filter`, `slice`, `splice`, `concat` and `flat` build their result
    /// with `ArraySpeciesCreate`: the receiver's `constructor[Symbol.species]`.
    #[test]
    fn species_decides_the_constructor_of_derived_results() {
        assert_eq!(
            run(
                "class Sub extends Array {} String(new Sub(1, 2, 3).map(function (x) { return x; }) instanceof Sub)"
            ),
            "true"
        );
        assert_eq!(
            run(
                "class Sub extends Array {} Object.defineProperty(Sub, Symbol.species, { value: Array }); \
                 String(new Sub(1, 2).filter(function () { return true; }) instanceof Sub)"
            ),
            "false"
        );
        assert_eq!(run("Array[Symbol.species] === Array"), "true");
        assert_eq!(run("Array.isArray(new Proxy([], {}))"), "true");
        assert_eq!(run("Array.isArray(new Proxy({}, {}))"), "false");
    }

    /// `concat` spreads what `IsConcatSpreadable` says to spread, and nothing else.
    #[test]
    fn concat_spreads_only_concat_spreadable_values() {
        assert_eq!(
            run(
                "var like = { length: 2, 0: 'a', 1: 'b', [Symbol.isConcatSpreadable]: true }; [].concat(like).join()"
            ),
            "a,b"
        );
        assert_eq!(
            run("var arr = [1, 2]; arr[Symbol.isConcatSpreadable] = false; [].concat(arr).length"),
            "1"
        );
        assert_eq!(run("[1].concat([2, [3]], 4).length"), "4");
    }

    /// `Array.of` and `Array.from` construct through `this` when it is a
    /// constructor, and read `length` back into the result.
    #[test]
    fn of_and_from_construct_through_this() {
        assert_eq!(
            run(
                "var made = Array.of.call(function (n) { this.arguments = n; }, 'a', 'b'); \
                 made.arguments + ':' + made[1] + ':' + made.length"
            ),
            "2:b:2"
        );
        assert_eq!(
            run(
                "var made = Array.from.call(function (n) { this.arguments = n; }, { length: 1, 0: 'z' }); \
                 made.arguments + ':' + made[0] + ':' + made.length"
            ),
            "1:z:1"
        );
        assert_eq!(
            run("Array.from('xy').join('|') + ' ' + Array.of(7).length"),
            "x|y 1"
        );
    }

    /// A write that the object refuses is a `TypeError`, as `Set(O, P, V, true)`
    /// requires, and so is a delete that fails.
    #[test]
    fn refused_writes_on_frozen_arrays_are_type_errors() {
        for expression in [
            "Object.freeze([1]).push(2)",
            "Object.freeze([3, 1]).sort()",
            "Object.freeze([1, 2]).reverse()",
            "Object.freeze([1]).pop()",
        ] {
            assert_eq!(
                run(&format!(
                    "var out = 'no throw'; try {{ {expression}; }} catch (e) {{ out = e.name; }} out"
                )),
                "TypeError",
                "{expression}"
            );
        }
    }

    /// `sort` puts `undefined` last without calling the comparator for it, and is
    /// stable: equal keys keep their input order.
    #[test]
    fn sort_is_stable_and_puts_undefined_last() {
        assert_eq!(run("[3, undefined, 1].sort().join()"), "1,3,");
        assert_eq!(
            run("[{k: 1, v: 'a'}, {k: 0, v: 'b'}, {k: 1, v: 'c'}]
                 .sort(function (x, y) { return x.k - y.k; })
                 .map(function (o) { return o.v; }).join('')"),
            "bac"
        );
    }

    /// Run `set_up`, drain the microtask queue, then read `read`: the result of an
    /// async builtin is a promise, and its settlement is observable only after
    /// the jobs it queued have run.
    fn settled(set_up: &str, read: &str) -> String {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        runtime
            .execute(&mut parsed.dom, set_up)
            .expect("the set-up script should execute");
        loop {
            let pending = runtime.take_pending_microtasks();
            if pending.is_empty() {
                break;
            }
            for microtask in pending {
                runtime
                    .invoke_microtask(&mut parsed.dom, microtask)
                    .expect("microtask executes");
            }
        }
        run_in(&mut runtime, &mut parsed.dom, read)
    }

    fn run_in(runtime: &mut JsRuntime, dom: &mut render_dom::Dom, source: &str) -> String {
        match runtime.execute(dom, source) {
            Ok(outcome) => outcome.value.to_js_string(),
            Err(error) => format!("<threw {}>", error.message()),
        }
    }

    /// `Array.fromAsync` settles a promise with the array built from an
    /// array-like, an async iterable, a sync iterable of promises, and a mapper
    /// whose results are awaited; a non-callable mapper rejects it.
    #[test]
    fn from_async_settles_with_the_built_array() {
        let track = "var out = 'pending'; ";
        assert_eq!(
            settled(
                &format!(
                    "{track}Array.fromAsync([1, 2, 3]).then(function (v) {{ out = 'ok:' + v.join(); }});"
                ),
                "out"
            ),
            "ok:1,2,3"
        );
        assert_eq!(
            settled(
                &format!(
                    "{track}Array.fromAsync({{length: 2, 0: 'a', 1: 'b'}}).then(function (v) {{ out = 'ok:' + v.join(); }});"
                ),
                "out"
            ),
            "ok:a,b"
        );
        assert_eq!(
            settled(
                &format!(
                    "{track}Array.fromAsync([1, Promise.resolve(2)], function (x) {{ return x * 10; }}).then(function (v) {{ out = 'ok:' + v.join(); }});"
                ),
                "out"
            ),
            "ok:10,20"
        );
        assert_eq!(
            settled(
                &format!(
                    "{track}async function* g() {{ yield 7; yield 8; }} Array.fromAsync(g()).then(function (v) {{ out = 'ok:' + v.join(); }});"
                ),
                "out"
            ),
            "ok:7,8"
        );
        assert_eq!(
            settled(
                &format!(
                    "{track}Array.fromAsync([1], 5).then(null, function (e) {{ out = 'rejected:' + e.name; }});"
                ),
                "out"
            ),
            "rejected:TypeError"
        );
        assert_eq!(
            run("Array.fromAsync([]) instanceof Promise ? 'promise' : 'other'"),
            "promise"
        );
    }

    /// `Array.prototype[Symbol.unscopables]` is a null-prototype object naming the
    /// methods a `with` block must not shadow.
    #[test]
    fn unscopables_is_a_null_prototype_object_naming_the_methods() {
        assert_eq!(
            run("var u = Array.prototype[Symbol.unscopables]; \
                 String(Object.getPrototypeOf(u)) + ':' + u.flat + ':' + u.values + ':' + String(u.push)"),
            "null:true:true:undefined"
        );
    }
}
