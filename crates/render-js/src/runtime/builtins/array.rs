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
            NativeFunction::ArrayIndexOf => self.array_index_of(receiver, arguments),
            NativeFunction::ArraySlice => self.array_slice(receiver, arguments),
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
                self.array_iterate_with(dom, receiver, callback, false, false)
            }
            NativeFunction::ArrayMap => {
                let callback = Self::require_callable_object(
                    required_argument(arguments, 0, "map")?,
                    &self.realm,
                )?;
                self.array_iterate_with(dom, receiver, callback, true, false)
            }
            NativeFunction::ArrayFilter => {
                let callback = Self::require_callable_object(
                    required_argument(arguments, 0, "filter")?,
                    &self.realm,
                )?;
                self.array_iterate_with(dom, receiver, callback, false, true)
            }
            NativeFunction::ArraySome => {
                let callback = Self::require_callable_object(
                    required_argument(arguments, 0, "some")?,
                    &self.realm,
                )?;
                self.array_some(dom, receiver, callback)
            }
            NativeFunction::ArrayFind => self.array_find(dom, receiver, arguments, false),
            NativeFunction::ArrayFindIndex => self.array_find(dom, receiver, arguments, true),
            NativeFunction::ArrayEvery => self.array_every(dom, receiver, arguments),
            NativeFunction::ArrayIncludes => self.array_includes(receiver, arguments),
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
            NativeFunction::ArrayJoin => self.array_join(receiver, arguments),
            other => self.dispatch_typed_array_native(dom, other, receiver, arguments),
        }
    }
}

pub(in crate::runtime) fn array_index(property: &str) -> Option<u32> {
    let index = property.parse::<u32>().ok()?;
    (index.to_string() == property && index < u32::MAX).then_some(index)
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

    /// Walk the event path from `target` upward, invoking matching listeners
    /// and `on*` handlers. Returns whether no listener called
    /// `preventDefault()`.
    /// `window.addEventListener`: listeners live outside the node tree.
    /// Read indexed elements from an object that may or may not be an Array.
    pub(in crate::runtime) fn array_elements_for(&mut self, object: ObjectId) -> Vec<JsValue> {
        if let Some(ObjectHost::TypedArray { .. }) = self.realm.host(object) {
            return self
                .typed_array_elements(object)
                .unwrap_or_default()
                .into_iter()
                .map(JsValue::Number)
                .collect();
        }
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "length is validated as a finite non-negative integer"
        )]
        let length = self
            .realm
            .get_property(object, "length")
            .and_then(|value| match &value {
                JsValue::Number(n) if n.is_finite() && *n >= 0.0 => Some(*n as usize),
                _ => None,
            })
            .unwrap_or(0);
        (0..length)
            .map(|index| self.realm.get_property(object, &index.to_string()))
            .map(|value| value.unwrap_or(JsValue::Undefined))
            .collect()
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
        &self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let separator = match arguments.first() {
            None | Some(JsValue::Undefined) => ",".to_owned(),
            Some(value) => value.to_js_string(),
        };
        let length = self.array_length(receiver)?;
        let mut output = String::new();
        for index in 0..length {
            if index != 0 {
                output.push_str(&separator);
            }
            match self.realm.get_property(receiver, &index.to_string()) {
                None | Some(JsValue::Undefined | JsValue::Null) => {}
                Some(value) => output.push_str(&value.to_js_string()),
            }
        }
        Ok(JsValue::String(output))
    }

    /// Read all indexed elements (holes become `undefined`). String wrappers
    /// expose indexed code points through the ordinary object-like array
    /// method contract even though those properties are virtual.
    pub(in crate::runtime) fn array_elements(
        &self,
        receiver: ObjectId,
    ) -> Result<Vec<JsValue>, JsError> {
        let length = self.array_length(receiver)?;
        Ok((0..length)
            .map(|index| self.array_element(receiver, index))
            .map(|value| value.unwrap_or(JsValue::Undefined))
            .collect())
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
        &self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let needle = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let elements = self.array_elements(receiver)?;
        let start = match arguments.get(1) {
            None | Some(JsValue::Undefined) => 0usize,
            Some(value) => {
                let raw = to_number(value)?;
                if raw < 0.0 {
                    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                    let from_end = (-raw) as usize;
                    elements.len().saturating_sub(from_end)
                } else {
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    {
                        raw as usize
                    }
                }
            }
        };
        for (index, element) in elements.iter().enumerate().skip(start) {
            if strict_equal(element, &needle) {
                #[allow(clippy::cast_precision_loss)]
                return Ok(JsValue::Number(index as f64));
            }
        }
        Ok(JsValue::Number(-1.0))
    }

    pub(in crate::runtime) fn array_slice(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let elements = self.array_elements(receiver)?;
        let resolve = |raw: f64, length: usize| -> usize {
            if raw < 0.0 {
                #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                let from_end = (-raw) as usize;
                length.saturating_sub(from_end)
            } else {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                {
                    (raw as usize).min(length)
                }
            }
        };
        let length = elements.len();
        let start = match arguments.first() {
            None | Some(JsValue::Undefined) => 0,
            Some(value) => resolve(to_number(value)?, length),
        };
        let end = match arguments.get(1) {
            None | Some(JsValue::Undefined) => length,
            Some(value) => resolve(to_number(value)?, length),
        };
        let clipped_start = start.min(end).min(length);
        let clipped_end = end.min(length);
        let picked = elements[clipped_start.min(clipped_end)..clipped_end].to_vec();
        Ok(JsValue::Object(self.create_array_from_values(&picked)?))
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
        map: bool,
        filter: bool,
    ) -> Result<JsValue, JsError> {
        let elements = self.array_elements(receiver)?;
        let mut mapped = Vec::with_capacity(elements.len());
        for (index, element) in elements.iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let keep = self.call(
                dom,
                callback,
                &[
                    element.clone(),
                    JsValue::Number(index as f64),
                    JsValue::Object(receiver),
                ],
            )?;
            if map {
                mapped.push(keep);
            } else if keep.is_truthy() {
                mapped.push(element.clone());
            }
        }
        if filter || map {
            Ok(JsValue::Object(self.create_array_from_values(&mapped)?))
        } else {
            Ok(JsValue::Undefined)
        }
    }

    pub(in crate::runtime) fn array_some(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        callback: ObjectId,
    ) -> Result<JsValue, JsError> {
        let elements = self.array_elements(receiver)?;
        for (index, element) in elements.iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let matches = self.call(
                dom,
                callback,
                &[
                    element.clone(),
                    JsValue::Number(index as f64),
                    JsValue::Object(receiver),
                ],
            )?;
            if matches.is_truthy() {
                return Ok(JsValue::Boolean(true));
            }
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
                self.array_elements_for(object)
            }
            JsValue::String(text) => text
                .chars()
                .map(|character| JsValue::String(character.to_string()))
                .collect(),
            JsValue::Object(object) => {
                let length = self
                    .realm
                    .get_property(object, "length")
                    .and_then(|value| to_number(&value).ok())
                    .unwrap_or(0.0)
                    .max(0.0) as usize;
                (0..length)
                    .map(|index| self.get_member(dom, object, &index.to_string()))
                    .collect::<Result<Vec<_>, _>>()?
            }
            JsValue::Null | JsValue::Undefined => Vec::new(),
            value => vec![value],
        };
        if let Some(mapper) = arguments.get(1)
            && let JsValue::Object(mapper) = mapper
        {
            let mapper = Self::require_callable_object(&JsValue::Object(*mapper), &self.realm)?;
            for (index, value) in values.iter_mut().enumerate() {
                #[allow(clippy::cast_precision_loss)]
                let index_value = JsValue::Number(index as f64);
                *value = self.call(dom, mapper, &[value.clone(), index_value])?;
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
        for (index, value) in self.array_elements(receiver)?.into_iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let matched = self.call(
                dom,
                callback,
                &[
                    value.clone(),
                    JsValue::Number(index as f64),
                    JsValue::Object(receiver),
                ],
            )?;
            if matched.is_truthy() {
                return if index_result {
                    #[allow(clippy::cast_precision_loss)]
                    Ok(JsValue::Number(index as f64))
                } else {
                    Ok(value)
                };
            }
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
        for (index, value) in self.array_elements(receiver)?.into_iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let matched = self.call(
                dom,
                callback,
                &[
                    value,
                    JsValue::Number(index as f64),
                    JsValue::Object(receiver),
                ],
            )?;
            if !matched.is_truthy() {
                return Ok(JsValue::Boolean(false));
            }
        }
        Ok(JsValue::Boolean(true))
    }

    pub(in crate::runtime) fn array_includes(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let search = arguments.first().unwrap_or(&JsValue::Undefined);
        let start = arguments
            .get(1)
            .and_then(|value| to_number(value).ok())
            .unwrap_or(0.0)
            .max(0.0) as usize;
        Ok(JsValue::Boolean(
            self.array_elements(receiver)?
                .iter()
                .skip(start)
                .any(|value| same_value_zero(value, search)),
        ))
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
        let values = self.array_elements(receiver)?;
        let mut index = 0_usize;
        let mut accumulator = if let Some(initial) = arguments.get(1) {
            initial.clone()
        } else {
            let Some(first) = values.first() else {
                return Err(JsError::type_error(
                    "Reduce of empty array with no initial value",
                ));
            };
            index = 1;
            first.clone()
        };
        while let Some(value) = values.get(index) {
            #[allow(clippy::cast_precision_loss)]
            let next = self.call(
                dom,
                callback,
                &[
                    accumulator,
                    value.clone(),
                    JsValue::Number(index as f64),
                    JsValue::Object(receiver),
                ],
            )?;
            accumulator = next;
            index = index.saturating_add(1);
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
        if number.is_infinite() {
            return Ok(u32::MAX);
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Ok(number.trunc().min(f64::from(u32::MAX)) as u32)
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
