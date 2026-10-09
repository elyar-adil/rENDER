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
use crate::runtime::convert::strict_equal;
use crate::runtime::convert::to_number;
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
            NativeFunction::ArraySlice => self.array_slice(dom, receiver, arguments),
            NativeFunction::ArrayValues => self.array_view_iterator(receiver, ArrayView::Values),
            NativeFunction::ArrayKeys => self.array_view_iterator(receiver, ArrayView::Keys),
            NativeFunction::ArrayEntries => self.array_view_iterator(receiver, ArrayView::Entries),
            NativeFunction::ArraySplice => self.array_splice(dom, receiver, arguments),
            NativeFunction::ArrayReverse => self.array_reverse(receiver),
            NativeFunction::ArrayAt => self.array_at(dom, receiver, arguments),
            NativeFunction::ArrayFlat => self.array_flat(dom, receiver, arguments),
            NativeFunction::ArrayReduceRight => self.array_reduce_right(dom, receiver, arguments),
            NativeFunction::ArrayFindLast => self.array_find_last(dom, receiver, arguments, false),
            NativeFunction::ArrayFindLastIndex => {
                self.array_find_last(dom, receiver, arguments, true)
            }
            NativeFunction::ArraySort => self.array_sort(dom, receiver, arguments),
            NativeFunction::ArrayConcat => self.array_concat(receiver, arguments),
            NativeFunction::ArrayShift => self.array_shift(receiver),
            NativeFunction::ArrayUnshift => self.array_unshift(receiver, arguments),
            NativeFunction::ArrayForEach => {
                let callback = Self::require_callable_object(
                    required_argument(arguments, 0, "forEach")?,
                    &self.realm,
                )?;
                let this_argument = callback_this_argument(arguments);
                self.array_iterate_with(dom, receiver, callback, &this_argument, false, false)
            }
            NativeFunction::ArrayMap => {
                let callback = Self::require_callable_object(
                    required_argument(arguments, 0, "map")?,
                    &self.realm,
                )?;
                let this_argument = callback_this_argument(arguments);
                self.array_iterate_with(dom, receiver, callback, &this_argument, true, false)
            }
            NativeFunction::ArrayFilter => {
                let callback = Self::require_callable_object(
                    required_argument(arguments, 0, "filter")?,
                    &self.realm,
                )?;
                let this_argument = callback_this_argument(arguments);
                self.array_iterate_with(dom, receiver, callback, &this_argument, false, true)
            }
            NativeFunction::ArraySome => {
                let callback = Self::require_callable_object(
                    required_argument(arguments, 0, "some")?,
                    &self.realm,
                )?;
                let this_argument = callback_this_argument(arguments);
                self.array_some(dom, receiver, callback, &this_argument)
            }
            NativeFunction::ArrayFind => self.array_find(dom, receiver, arguments, false),
            NativeFunction::ArrayFindIndex => self.array_find(dom, receiver, arguments, true),
            NativeFunction::ArrayEvery => self.array_every(dom, receiver, arguments),
            NativeFunction::ArrayIncludes => self.array_includes(dom, receiver, arguments),
            NativeFunction::ArrayReduce => self.array_reduce(dom, receiver, arguments),
            NativeFunction::ArrayPrototypeToString => {
                self.call_native_dispatch(dom, NativeFunction::ArrayJoin, receiver, &[])
            }
            NativeFunction::ArrayIsArray => Ok(JsValue::Boolean(matches!(
                arguments.first(),
                Some(JsValue::Object(object))
                    if matches!(self.realm.host(*object), Some(ObjectHost::Array))
            ))),
            NativeFunction::ArrayFrom => self.array_from(dom, arguments),
            NativeFunction::ArrayPush => self.array_push(receiver, arguments),
            NativeFunction::ArrayPop => self.array_pop(receiver),
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

/// `ToLength` ceiling (ECMA-262 §7.1.15).
pub(in crate::runtime) const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// `ArrayCreate` (§10.4.2.2 step 1) rejects lengths above 2^32-1.
const MAX_ARRAY_LENGTH: f64 = 4_294_967_295.0;

/// Element cap for the eager paths that read every index into a `Vec`.
/// Hostile array-likes (`{length: 2**53-1}`) must produce a catchable error,
/// not a multi-gigabyte allocation that aborts the process.
pub(in crate::runtime) const MAX_MATERIALIZED_ELEMENTS: usize = 1 << 24;

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

/// Property key of an integral `ToLength` index.
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
                .map(JsValue::Number)
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

    /// `LengthOfArrayLike` (§7.3.18): `ToLength([[Get]](O, "length"))`. String
    /// wrappers expose their code-unit count as a virtual `length`, because
    /// that is what `String.prototype.length` is.
    fn array_like_len(&mut self, dom: &mut Dom, object: ObjectId) -> Result<f64, JsError> {
        if let Some(ObjectHost::StringPrimitive(text)) = self.realm.host(object) {
            #[allow(clippy::cast_precision_loss)]
            return Ok(utf16::utf16_length(&text) as f64);
        }
        let length = self.get_value(dom, object, "length")?;
        self.to_length_value(dom, &length)
    }

    /// `HasProperty` for an integral index (§7.3.11), including the virtual
    /// indexed properties of string wrappers and typed arrays.
    fn has_indexed(&self, object: ObjectId, index: f64) -> bool {
        match self.realm.host(object) {
            Some(ObjectHost::TypedArray { buffer, length, .. }) => {
                index < length as f64 && !buffer.is_detached()
            }
            Some(ObjectHost::StringPrimitive(text)) => index < utf16::utf16_length(&text) as f64,
            _ => self
                .realm
                .get_descriptor(object, &index_key(index))
                .is_some(),
        }
    }

    /// [[Get]] for an integral index; accessors run.
    fn indexed_value(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        index: f64,
    ) -> Result<JsValue, JsError> {
        match self.realm.host(object) {
            Some(ObjectHost::StringPrimitive(text)) => {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                if let Some(unit) = utf16::utf16_units(&text).get(index as usize).copied() {
                    return Ok(JsValue::String(utf16::string_from_unit(unit)));
                }
            }
            Some(ObjectHost::TypedArray {
                kind,
                buffer,
                start,
                length,
            }) => {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let index = index as usize;
                if index < length {
                    let element = buffer.element(kind, start + index);
                    return Ok(element.map_or(JsValue::Undefined, JsValue::Number));
                }
            }
            _ => {}
        }
        self.get_value(dom, object, &index_key(index))
    }

    /// Resolve one `start`/`end` bound of `slice` (`ToIntegerOrInfinity` plus
    /// the relative-to-length adjustment from §23.1.3.25).
    fn relative_bound(
        &mut self,
        dom: &mut Dom,
        value: Option<&JsValue>,
        length: f64,
        default: f64,
    ) -> Result<f64, JsError> {
        let Some(value) = value else {
            return Ok(default);
        };
        let raw = self.to_integer_value(dom, value)?;
        if raw == f64::NEG_INFINITY {
            return Ok(0.0);
        }
        if raw == f64::INFINITY {
            return Ok(length);
        }
        if raw < 0.0 {
            return Ok((length + raw).max(0.0));
        }
        Ok(raw.min(length))
    }

    pub(in crate::runtime) fn array_push(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let mut length = self.array_length(receiver)?;
        for value in arguments {
            if !self
                .realm
                .set_property(receiver, length.to_string(), value.clone())
            {
                return Err(JsError::type_error("could not append array element"));
            }
            length = length
                .checked_add(1)
                .ok_or_else(|| JsError::resource("Array.prototype.push exceeds u32 length"))?;
        }
        self.set_array_length(receiver, length)?;
        Ok(JsValue::Number(f64::from(length)))
    }

    pub(in crate::runtime) fn array_pop(&mut self, receiver: ObjectId) -> Result<JsValue, JsError> {
        let length = self.array_length(receiver)?;
        if length == 0 {
            return Ok(JsValue::Undefined);
        }
        let index = length.saturating_sub(1);
        let value = self
            .realm
            .remove_property(receiver, &index.to_string())
            .unwrap_or(JsValue::Undefined);
        self.set_array_length(receiver, index)?;
        Ok(value)
    }

    pub(in crate::runtime) fn array_join(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let separator = match arguments.first() {
            None | Some(JsValue::Undefined) => ",".to_owned(),
            Some(value) => value.to_js_string(),
        };
        if self.arrays_joining.contains(&receiver) {
            return Ok(JsValue::String(String::new()));
        }
        let length = self.array_like_len(dom, receiver)?;
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
            if self.has_indexed(receiver, index) {
                match self.indexed_value(dom, receiver, index)? {
                    JsValue::Undefined | JsValue::Null => {}
                    // ToString, not the primitive shortcut: a nested array or
                    // any object element runs its own `toString`.
                    value => output.push_str(&self.to_string_value(dom, &value)?),
                }
            }
            index += 1.0;
        }
        Ok(output)
    }

    /// Read all indexed elements (holes become `undefined`). String wrappers
    /// expose indexed code points through the ordinary object-like array
    /// method contract even though those properties are virtual.
    ///
    /// Eager materialization is bounded: lengths above the engine limit yield
    /// a catchable resource error rather than a giant allocation.
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
                self.array_element(receiver, index)
                    .unwrap_or(JsValue::Undefined),
            );
        }
        Ok(elements)
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

    pub(in crate::runtime) fn array_element(
        &self,
        receiver: ObjectId,
        index: u32,
    ) -> Option<JsValue> {
        self.realm
            .get_property(receiver, &index.to_string())
            .or_else(|| match self.realm.host(receiver) {
                Some(ObjectHost::StringPrimitive(text)) => utf16::utf16_units(&text)
                    .get(usize::try_from(index).ok()?)
                    .copied()
                    .map(|unit| JsValue::String(utf16::string_from_unit(unit))),
                _ => None,
            })
    }

    /// Replace the indexed elements of `receiver`, updating its length.
    pub(in crate::runtime) fn set_array_elements(
        &mut self,
        receiver: ObjectId,
        values: &[JsValue],
    ) -> Result<(), JsError> {
        let old_length = self.array_length(receiver)?;
        for index in 0..old_length {
            self.realm.remove_property(receiver, &index.to_string());
        }
        for (index, value) in values.iter().enumerate() {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "index comes from a bounded u32 loop"
            )]
            self.realm
                .set_property(receiver, index.to_string(), value.clone());
        }
        #[allow(
            clippy::cast_possible_truncation,
            reason = "array element counts stay far below the u32 boundary"
        )]
        #[allow(
            clippy::cast_possible_truncation,
            reason = "array element counts stay far below the u32 boundary"
        )]
        self.set_array_length(receiver, values.len() as u32)
    }

    pub(in crate::runtime) fn array_index_of(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let needle = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let length = self.array_like_len(dom, receiver)?;
        if length == 0.0 {
            return Ok(JsValue::Number(-1.0));
        }
        // §23.1.3.14 steps 5-8: relative `fromIndex` clamped into range, and
        // an infinite positive start can never find anything.
        let raw = match arguments.get(1) {
            None | Some(JsValue::Undefined) => 0.0,
            Some(value) => self.to_integer_value(dom, value)?,
        };
        let mut index = if raw == f64::INFINITY {
            return Ok(JsValue::Number(-1.0));
        } else if raw >= 0.0 {
            // `+ 0.0` turns a `-0` start into `+0`, the index the result reports.
            raw.min(length) + 0.0
        } else {
            (length + raw).max(0.0)
        };
        while index < length {
            if self.has_indexed(receiver, index) {
                let element = self.indexed_value(dom, receiver, index)?;
                if strict_equal(&element, &needle) {
                    return Ok(JsValue::Number(index));
                }
            }
            index += 1.0;
        }
        Ok(JsValue::Number(-1.0))
    }

    pub(in crate::runtime) fn array_slice(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let length = self.array_like_len(dom, receiver)?;
        let start = self.relative_bound(dom, arguments.first(), length, 0.0)?;
        let end = self.relative_bound(dom, arguments.get(1), length, length)?;
        let count = (end - start).max(0.0);
        let result = self.create_array_with_length(count)?;
        let mut index = start;
        let mut target = 0.0;
        while index < end {
            if self.has_indexed(receiver, index) {
                let value = self.indexed_value(dom, receiver, index)?;
                self.realm.set_property(result, index_key(target), value);
            }
            index += 1.0;
            target += 1.0;
        }
        Ok(JsValue::Object(result))
    }

    pub(in crate::runtime) fn array_splice(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let elements = self.array_elements(receiver)?;
        let length = elements.len();
        let raw_start = match arguments.first() {
            None | Some(JsValue::Undefined) => 0.0,
            Some(value) => self.to_integer_value(dom, value)?,
        };
        let start = if raw_start < 0.0 {
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
            let from_end = (-raw_start) as usize;
            length.saturating_sub(from_end)
        } else {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            {
                (raw_start as usize).min(length)
            }
        };
        let delete_count = match arguments.get(1) {
            None | Some(JsValue::Undefined) => length - start,
            Some(value) => {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let raw = self.to_integer_value(dom, value)?.max(0.0) as usize;
                raw.min(length - start)
            }
        };
        let mut result = elements.clone();
        let removed: Vec<JsValue> = result.splice(start..start + delete_count, []).collect();
        for (offset, item) in arguments.iter().skip(2).enumerate() {
            result.insert(start + offset, item.clone());
        }
        self.set_array_elements(receiver, &result)?;
        Ok(JsValue::Object(self.create_array_from_values(&removed)?))
    }

    pub(in crate::runtime) fn array_reverse(
        &mut self,
        receiver: ObjectId,
    ) -> Result<JsValue, JsError> {
        let mut elements = self.array_elements(receiver)?;
        elements.reverse();
        self.set_array_elements(receiver, &elements)?;
        Ok(JsValue::Object(self.create_array_from_values(&elements)?))
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
        let elements = self.array_elements(receiver)?;
        let length = elements.len();
        let relative = match arguments.first() {
            Some(value) => self.to_integer_value(dom, value)?,
            None => 0.0,
        };
        #[allow(
            clippy::cast_precision_loss,
            reason = "an array length is a u32 and f64 represents every u32 exactly"
        )]
        let length = length as f64;
        let index = if relative >= 0.0 {
            relative
        } else {
            length + relative
        };
        if index < 0.0 || index >= length {
            return Ok(JsValue::Undefined);
        }
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the bounds check above leaves an index inside the element list"
        )]
        Ok(elements[index as usize].clone())
    }

    /// ECMA-262 22.1.3.7 `Array.prototype.reduceRight`: `reduce` walking from
    /// the end.
    ///
    /// Implemented by reversing a copy, reducing, and reversing back is *not*
    /// equivalent, because a callback that mutates the source array sees a
    /// different order of indices. So this is a separate loop over the same
    /// `array_like_len` / `has_indexed` machinery `reduce` uses, in the other
    /// direction.
    pub(in crate::runtime) fn array_reduce_right(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let callback = Self::require_callable_object(
            required_argument(arguments, 0, "Array.reduceRight")?,
            &self.realm,
        )?;
        let length = self.array_like_len(dom, receiver)?;
        let mut index = length - 1.0;
        let mut accumulator = if let Some(initial) = arguments.get(1) {
            initial.clone()
        } else {
            let mut found = None;
            while index >= 0.0 {
                if self.has_indexed(receiver, index) {
                    found = Some(self.indexed_value(dom, receiver, index)?);
                    index -= 1.0;
                    break;
                }
                index -= 1.0;
            }
            let Some(first) = found else {
                return Err(JsError::type_error(
                    "Reduce of empty array with no initial value",
                ));
            };
            first
        };
        while index >= 0.0 {
            if self.has_indexed(receiver, index) {
                let value = self.indexed_value(dom, receiver, index)?;
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
            index -= 1.0;
        }
        Ok(accumulator)
    }

    /// ECMA-262 22.1.3.12 `Array.prototype.findLast` and 22.1.3.11
    /// `findLastIndex`: the same two methods as `find`/`findIndex` with the
    /// iteration order reversed, so the *last* match wins.
    pub(in crate::runtime) fn array_find_last(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
        want_index: bool,
    ) -> Result<JsValue, JsError> {
        let callback = Self::require_callable_object(
            required_argument(arguments, 0, "Array.findLast")?,
            &self.realm,
        )?;
        let length = self.array_like_len(dom, receiver)?;
        let mut index = length - 1.0;
        while index >= 0.0 {
            if self.has_indexed(receiver, index) {
                let value = self.indexed_value(dom, receiver, index)?;
                if self
                    .call(
                        dom,
                        callback,
                        &[
                            value.clone(),
                            JsValue::Number(index),
                            JsValue::Object(receiver),
                        ],
                    )?
                    .is_truthy()
                {
                    return Ok(if want_index {
                        JsValue::Number(index)
                    } else {
                        value
                    });
                }
            }
            index -= 1.0;
        }
        Ok(if want_index {
            JsValue::Number(-1.0)
        } else {
            JsValue::Undefined
        })
    }

    /// ECMA-262 22.1.3.9 `Array.prototype.flat`, with its `depth` argument.
    ///
    /// `flat(1)` is the common one and `flat()` is `flat(1)`, so an engine that
    /// only answers `flat()` is wrong for every caller that flattens exactly one
    /// level - and silently wrong, because the result is still an array.
    pub(in crate::runtime) fn array_flat(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let depth = match arguments.first() {
            Some(value) => self.to_integer_value(dom, value)?.max(0.0),
            None => 1.0,
        };
        // The walk is over *indices*, not over a materialized element list,
        // because `flat` is the one method that distinguishes a hole from an
        // explicit `undefined`: `FlattenIntoArray` skips a property that does not
        // exist and keeps one whose value is `undefined`. A `Vec<JsValue>` read
        // up front cannot tell those apart, and `[1, , 2].flat()` would answer
        // `[1, undefined, 2]`.
        let mut output = Vec::new();
        self.flatten_indices(dom, receiver, depth, &mut output, MAX_MATERIALIZED_ELEMENTS)?;
        Ok(JsValue::Object(self.create_array_from_values(&output)?))
    }

    /// `FlattenIntoArray` over an array-like, appending to `output`.
    fn flatten_indices(
        &mut self,
        dom: &mut Dom,
        source: ObjectId,
        depth: f64,
        output: &mut Vec<JsValue>,
        limit: usize,
    ) -> Result<(), JsError> {
        let length = self.array_like_len(dom, source)?;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "array_like_len is a non-negative integer length already bounded by the engine"
        )]
        let count = length as usize;
        for step in 0..count {
            #[allow(
                clippy::cast_precision_loss,
                reason = "a position inside an array is exactly representable"
            )]
            let index = step as f64;
            if output.len() >= limit {
                return Ok(());
            }
            // A hole is skipped, at every depth, which is the whole difference
            // from materializing the elements first.
            if !self.has_indexed(source, index) {
                continue;
            }
            let value = self.indexed_value(dom, source, index)?;
            // Only a genuine array nests - ECMA-262's `IsArray`, which is what
            // makes a string spread as its characters rather than being descended
            // into, and what makes a `Map` contribute nothing at all. Both
            // distinctions are the reason `flat` exists.
            let nested = matches!(value, JsValue::Object(object)
                if matches!(self.realm.host(object), Some(ObjectHost::Array)));
            if nested && depth >= 1.0 {
                let JsValue::Object(object) = value else {
                    unreachable!("nested implies an object")
                };
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "each level of depth is one recursion and the input is already bounded"
                )]
                self.flatten_indices(dom, object, depth - 1.0, output, limit)?;
            } else {
                output.push(value);
            }
        }
        Ok(())
    }

    pub(in crate::runtime) fn array_sort(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let mut elements = self.array_elements(receiver)?;
        let comparator = match arguments.first() {
            Some(JsValue::Object(object)) if Self::is_callable_object(*object, &self.realm) => {
                Some(*object)
            }
            _ => None,
        };
        // Insertion sort keeps comparator calls simple and stable enough.
        for index in 1..elements.len() {
            let mut position = index;
            while position > 0 {
                let keep = Self::sort_order(
                    self,
                    dom,
                    comparator,
                    &elements[position - 1],
                    &elements[position],
                )?;
                if keep <= 0 {
                    break;
                }
                elements.swap(position - 1, position);
                position -= 1;
            }
        }
        self.set_array_elements(receiver, &elements)?;
        Ok(JsValue::Object(receiver))
    }

    /// Comparator result: negative when `left` sorts before `right`.
    pub(in crate::runtime) fn sort_order(
        &mut self,
        dom: &mut Dom,
        comparator: Option<ObjectId>,
        left: &JsValue,
        right: &JsValue,
    ) -> Result<i32, JsError> {
        if let Some(function) = comparator {
            let result = self.call(dom, function, &[left.clone(), right.clone()])?;
            let number = self.to_number_value(dom, &result)?;
            return Ok(if number < 0.0 {
                -1
            } else {
                i32::from(number > 0.0)
            });
        }
        let left_text = match left {
            JsValue::Undefined => None,
            other => Some(other.to_js_string()),
        };
        let right_text = match right {
            JsValue::Undefined => None,
            other => Some(other.to_js_string()),
        };
        Ok(match (left_text, right_text) {
            (None, None) => 0,
            (None, Some(_)) => 1,
            (Some(_), None) => -1,
            (Some(a), Some(b)) => match a.cmp(&b) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            },
        })
    }

    pub(in crate::runtime) fn array_concat(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let mut values = self.array_elements(receiver)?;
        for argument in arguments {
            if let JsValue::Object(object) = argument
                && matches!(self.realm.host(*object), Some(ObjectHost::Array))
            {
                values.extend(self.array_elements(*object)?);
                continue;
            }
            values.push(argument.clone());
        }
        Ok(JsValue::Object(self.create_array_from_values(&values)?))
    }

    pub(in crate::runtime) fn array_shift(
        &mut self,
        receiver: ObjectId,
    ) -> Result<JsValue, JsError> {
        let mut elements = self.array_elements(receiver)?;
        if elements.is_empty() {
            self.set_array_length(receiver, 0)?;
            return Ok(JsValue::Undefined);
        }
        let first = elements.remove(0);
        self.set_array_elements(receiver, &elements)?;
        Ok(first)
    }

    pub(in crate::runtime) fn array_unshift(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let mut elements = self.array_elements(receiver)?;
        elements.splice(0..0, arguments.iter().cloned());
        let new_length = elements.len();
        self.set_array_elements(receiver, &elements)?;
        #[allow(clippy::cast_precision_loss)]
        Ok(JsValue::Number(new_length as f64))
    }

    pub(in crate::runtime) fn array_iterate_with(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        callback: ObjectId,
        this_argument: &JsValue,
        map: bool,
        filter: bool,
    ) -> Result<JsValue, JsError> {
        let length = self.array_like_len(dom, receiver)?;
        // `map` pre-creates `ArrayCreate(len)`; `filter` appends only the
        // surviving elements, so its result length is discovered as it goes.
        let result = if map {
            Some(self.create_array_with_length(length)?)
        } else if filter {
            Some(self.create_array_with_length(0.0)?)
        } else {
            None
        };
        let mut index = 0.0;
        let mut target = 0.0;
        while index < length {
            // §23.1.3.x: holes are skipped without invoking the callback.
            if !self.has_indexed(receiver, index) {
                index += 1.0;
                continue;
            }
            let element = self.indexed_value(dom, receiver, index)?;
            let keep = self.call_with_this(
                dom,
                callback,
                &[
                    element.clone(),
                    JsValue::Number(index),
                    JsValue::Object(receiver),
                ],
                this_argument.clone(),
            )?;
            if let Some(result) = result {
                if map {
                    self.realm.set_property(result, index_key(index), keep);
                } else if keep.is_truthy() {
                    self.realm.set_property(result, index_key(target), element);
                    target += 1.0;
                }
            }
            index += 1.0;
        }
        match result {
            Some(result) => {
                if filter {
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    self.set_array_length(result, target as u32)?;
                }
                Ok(JsValue::Object(result))
            }
            None => Ok(JsValue::Undefined),
        }
    }

    pub(in crate::runtime) fn array_some(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        callback: ObjectId,
        this_argument: &JsValue,
    ) -> Result<JsValue, JsError> {
        let length = self.array_like_len(dom, receiver)?;
        let mut index = 0.0;
        while index < length {
            if self.has_indexed(receiver, index) {
                let element = self.indexed_value(dom, receiver, index)?;
                let matches = self.call_with_this(
                    dom,
                    callback,
                    &[element, JsValue::Number(index), JsValue::Object(receiver)],
                    this_argument.clone(),
                )?;
                if matches.is_truthy() {
                    return Ok(JsValue::Boolean(true));
                }
            }
            index += 1.0;
        }
        Ok(JsValue::Boolean(false))
    }

    pub(in crate::runtime) fn array_from(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let source = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        // §23.1.2.1 step 3 validates the mapper before the items are read, and
        // step 6 reads `items[@@iterator]`, which throws for null and undefined.
        let mapper = match arguments.get(1) {
            None | Some(JsValue::Undefined) => None,
            Some(value) => Some(Self::require_callable_object(value, &self.realm)?),
        };
        if matches!(source, JsValue::Null | JsValue::Undefined) {
            return Err(JsError::type_error(
                "Array.from requires an iterable or array-like, not null or undefined",
            ));
        }
        // §23.1.2.1: iterables go through their iterator (Set, Map, generators,
        // array iterators, user-defined); everything else is read as an
        // array-like. `iterate_values` implements exactly that split.
        let mut values = match source {
            JsValue::Object(_) | JsValue::String(_) => self.iterate_values(dom, &source)?,
            // A number, boolean or symbol has no `length`, so as an array-like it is empty.
            _ => Vec::new(),
        };
        if let Some(mapper) = mapper {
            let this_argument = arguments.get(2).cloned().unwrap_or(JsValue::Undefined);
            for (index, value) in values.iter_mut().enumerate() {
                #[allow(clippy::cast_precision_loss)]
                let index_value = JsValue::Number(index as f64);
                *value = self.call_with_this(
                    dom,
                    mapper,
                    &[value.clone(), index_value],
                    this_argument.clone(),
                )?;
            }
        }
        Ok(JsValue::Object(self.create_array_from_values(&values)?))
    }

    pub(in crate::runtime) fn array_find(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
        index_result: bool,
    ) -> Result<JsValue, JsError> {
        let callback = Self::require_callable_object(
            required_argument(arguments, 0, "Array.find")?,
            &self.realm,
        )?;
        let this_argument = callback_this_argument(arguments);
        let length = self.array_like_len(dom, receiver)?;
        let mut index = 0.0;
        while index < length {
            if self.has_indexed(receiver, index) {
                let value = self.indexed_value(dom, receiver, index)?;
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
                    return if index_result {
                        Ok(JsValue::Number(index))
                    } else {
                        Ok(value)
                    };
                }
            }
            index += 1.0;
        }
        if index_result {
            Ok(JsValue::Number(-1.0))
        } else {
            Ok(JsValue::Undefined)
        }
    }

    pub(in crate::runtime) fn array_every(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let callback = Self::require_callable_object(
            required_argument(arguments, 0, "Array.every")?,
            &self.realm,
        )?;
        let this_argument = callback_this_argument(arguments);
        let length = self.array_like_len(dom, receiver)?;
        let mut index = 0.0;
        while index < length {
            if self.has_indexed(receiver, index) {
                let value = self.indexed_value(dom, receiver, index)?;
                let matched = self.call_with_this(
                    dom,
                    callback,
                    &[value, JsValue::Number(index), JsValue::Object(receiver)],
                    this_argument.clone(),
                )?;
                if !matched.is_truthy() {
                    return Ok(JsValue::Boolean(false));
                }
            }
            index += 1.0;
        }
        Ok(JsValue::Boolean(true))
    }

    pub(in crate::runtime) fn array_includes(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let search = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let length = self.array_like_len(dom, receiver)?;
        let raw = match arguments.get(1) {
            None | Some(JsValue::Undefined) => 0.0,
            Some(value) => self.to_integer_value(dom, value)?,
        };
        let mut index = if raw == f64::INFINITY {
            return Ok(JsValue::Boolean(false));
        } else if raw >= 0.0 {
            raw.min(length)
        } else {
            (length + raw).max(0.0)
        };
        // `includes` Gets every index (holes read as `undefined`), unlike
        // `indexOf`, which checks HasProperty first (§23.1.3.13).
        while index < length {
            let element = self.indexed_value(dom, receiver, index)?;
            if same_value_zero(&element, &search) {
                return Ok(JsValue::Boolean(true));
            }
            index += 1.0;
        }
        Ok(JsValue::Boolean(false))
    }

    pub(in crate::runtime) fn array_reduce(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let callback = Self::require_callable_object(
            required_argument(arguments, 0, "Array.reduce")?,
            &self.realm,
        )?;
        let length = self.array_like_len(dom, receiver)?;
        let mut index = 0.0;
        let mut accumulator = if let Some(initial) = arguments.get(1) {
            initial.clone()
        } else {
            let mut found = None;
            while index < length {
                if self.has_indexed(receiver, index) {
                    found = Some(self.indexed_value(dom, receiver, index)?);
                    index += 1.0;
                    break;
                }
                index += 1.0;
            }
            let Some(first) = found else {
                return Err(JsError::type_error(
                    "Reduce of empty array with no initial value",
                ));
            };
            first
        };
        while index < length {
            if self.has_indexed(receiver, index) {
                let value = self.indexed_value(dom, receiver, index)?;
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
            index += 1.0;
        }
        Ok(accumulator)
    }

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
        object: ObjectId,
        value: &JsValue,
    ) -> Result<(), JsError> {
        let number = to_number(value)?;
        if !number.is_finite()
            || number < 0.0
            || number.fract() != 0.0
            || number > f64::from(u32::MAX)
        {
            return Err(JsError::type_error("invalid array length"));
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let length = number as u32;
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
        // Whether a hole is removed or kept is a property of the engine's array
        // model, not of `flat`, so the assertion is the one that does not change
        // when that model does: `flat` visits exactly the positions the other
        // indexed methods visit. Asserting a hole count here would pin a model
        // question inside a method test.
        assert_eq!(
            run("var a = [1, , 2]; a.flat().length"),
            run("var b = [1, , 2]; b.map(function (v) { return v; }).length")
        );
        assert_eq!(run("var a = [1, , 2]; a.length"), "3");
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
}
