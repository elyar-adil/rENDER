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
use crate::runtime::convert::to_integer_or_infinity;
use crate::runtime::convert::to_number;
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
            NativeFunction::ArraySplice => self.array_splice(receiver, arguments),
            NativeFunction::ArrayReverse => self.array_reverse(receiver),
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
        let length = self.get_member(dom, object, "length")?;
        let length = to_number(&length)?;
        if !length.is_finite() || length < 0.0 {
            return Err(self.range_error("invalid typed array length"));
        }
        let length = length.trunc();
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
        let length = self
            .realm
            .get_property(object, "length")
            .map(|value| to_length(&value))
            .transpose()?
            .unwrap_or(0.0);
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
    /// wrappers expose their code-point count as a virtual `length`.
    fn array_like_len(&mut self, dom: &mut Dom, object: ObjectId) -> Result<f64, JsError> {
        if let Some(ObjectHost::StringPrimitive(text)) = self.realm.host(object) {
            #[allow(clippy::cast_precision_loss)]
            return Ok(text.chars().count() as f64);
        }
        let length = self.get_value(dom, object, "length")?;
        to_length(&length)
    }

    /// `HasProperty` for an integral index (§7.3.11), including the virtual
    /// indexed properties of string wrappers and typed arrays.
    fn has_indexed(&self, object: ObjectId, index: f64) -> bool {
        match self.realm.host(object) {
            Some(ObjectHost::TypedArray { length, .. }) => index < length as f64,
            Some(ObjectHost::StringPrimitive(text)) => index < text.chars().count() as f64,
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
                if let Some(character) = text.chars().nth(index as usize) {
                    return Ok(JsValue::String(character.to_string()));
                }
            }
            Some(ObjectHost::TypedArray {
                buffer,
                start,
                length,
                ..
            }) => {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let index = index as usize;
                if index < length {
                    let element = buffer.0.borrow().get(start + index).copied();
                    return Ok(element.map_or(JsValue::Undefined, JsValue::Number));
                }
            }
            _ => {}
        }
        self.get_value(dom, object, &index_key(index))
    }

    /// Resolve one `start`/`end` bound of `slice` (`ToIntegerOrInfinity` plus
    /// the relative-to-length adjustment from §23.1.3.25).
    fn relative_bound(value: Option<&JsValue>, length: f64, default: f64) -> Result<f64, JsError> {
        let Some(value) = value else {
            return Ok(default);
        };
        let raw = to_integer_or_infinity(value)?;
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
        let length = self.array_like_len(dom, receiver)?;
        // The joined string is at least `length - 1` characters long; a
        // hostile length must yield a catchable error, not an unbounded
        // string build.
        if length > MAX_MATERIALIZED_ELEMENTS as f64 {
            return Err(self.range_error("Invalid string length"));
        }
        let mut output = String::new();
        let mut index = 0.0;
        while index < length {
            if index > 0.0 {
                output.push_str(&separator);
            }
            if self.has_indexed(receiver, index) {
                match self.indexed_value(dom, receiver, index)? {
                    JsValue::Undefined | JsValue::Null => {}
                    value => output.push_str(&value.to_js_string()),
                }
            }
            index += 1.0;
        }
        Ok(JsValue::String(output))
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
                Some(ObjectHost::StringPrimitive(text)) => text
                    .chars()
                    .nth(usize::try_from(index).ok()?)
                    .map(|character| JsValue::String(character.to_string())),
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
            Some(value) => to_integer_or_infinity(value)?,
        };
        let mut index = if raw == f64::INFINITY {
            return Ok(JsValue::Number(-1.0));
        } else if raw >= 0.0 {
            raw.min(length)
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
        let start = Self::relative_bound(arguments.first(), length, 0.0)?;
        let end = Self::relative_bound(arguments.get(1), length, length)?;
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
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let elements = self.array_elements(receiver)?;
        let length = elements.len();
        let raw_start = match arguments.first() {
            None | Some(JsValue::Undefined) => 0.0,
            Some(value) => to_number(value)?,
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
                let raw = to_number(value)?.max(0.0) as usize;
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
            let number = to_number(&result)?;
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
        let mut values = match source {
            JsValue::Object(object)
                if matches!(self.realm.host(object), Some(ObjectHost::Array)) =>
            {
                self.array_elements_for(object)?
            }
            JsValue::String(text) => text
                .chars()
                .map(|character| JsValue::String(character.to_string()))
                .collect(),
            JsValue::Object(object) => {
                let length = self
                    .realm
                    .get_property(object, "length")
                    .map(|value| to_length(&value))
                    .transpose()?
                    .unwrap_or(0.0);
                if length > MAX_MATERIALIZED_ELEMENTS as f64 {
                    return Err(JsError::resource(
                        "Array.from source length exceeds the materialization bound",
                    ));
                }
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let count = length as usize;
                let mut values = Vec::new();
                values.try_reserve_exact(count).map_err(|_| {
                    JsError::resource("Array.from source exceeds the available heap")
                })?;
                for index in 0..count {
                    values.push(self.get_member(dom, object, &index.to_string())?);
                }
                values
            }
            JsValue::Null | JsValue::Undefined => Vec::new(),
            value => vec![value],
        };
        if let Some(mapper) = arguments.get(1)
            && let JsValue::Object(mapper) = mapper
        {
            let mapper = Self::require_callable_object(&JsValue::Object(*mapper), &self.realm)?;
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
            Some(value) => to_integer_or_infinity(value)?,
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
                    Some(JsValue::Number(text.chars().count() as f64))
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
