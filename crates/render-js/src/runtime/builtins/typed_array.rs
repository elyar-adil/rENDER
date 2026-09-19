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
use crate::runtime::convert::to_number;
use crate::value::NativeFunction;
use crate::value::ObjectHost;
use crate::value::TypedArrayKind;
use crate::value::TypedBuffer;

impl JsRuntime {
    pub(in crate::runtime) fn dispatch_typed_array_native(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match function {
            NativeFunction::TypedArraySet => self.typed_array_set(dom, receiver, arguments),
            NativeFunction::TypedArraySubarray => self.typed_array_subarray(receiver, arguments),
            NativeFunction::TypedArraySlice => self.typed_array_slice(receiver, arguments),
            NativeFunction::TypedArrayFill => self.typed_array_fill(receiver, arguments),
            NativeFunction::TypedArrayIndexOf => self.typed_array_index_of(receiver, arguments),
            NativeFunction::TypedArrayIncludes => self.typed_array_includes(receiver, arguments),
            NativeFunction::TypedArrayJoin => self.typed_array_join(receiver, arguments),
            NativeFunction::TypedArrayFrom => {
                let kind = match self.realm.host(receiver) {
                    Some(ObjectHost::TypedArrayConstructor(kind)) => kind,
                    _ => {
                        return Err(JsError::type_error(
                            "TypedArray.from requires a typed-array constructor receiver",
                        ));
                    }
                };
                self.typed_array_from(dom, receiver, kind, arguments)
            }
            NativeFunction::TypedArrayForEach => {
                let callback = Self::require_callable_object(
                    required_argument(arguments, 0, "forEach")?,
                    &self.realm,
                )?;
                self.typed_array_for_each(dom, receiver, callback)
            }
            NativeFunction::TypedArrayMap => {
                let callback = Self::require_callable_object(
                    required_argument(arguments, 0, "map")?,
                    &self.realm,
                )?;
                self.typed_array_map_or_filter(dom, receiver, callback, true)
            }
            NativeFunction::TypedArrayFilter => {
                let callback = Self::require_callable_object(
                    required_argument(arguments, 0, "filter")?,
                    &self.realm,
                )?;
                self.typed_array_map_or_filter(dom, receiver, callback, false)
            }
            other => self.dispatch_collections_native(dom, other, receiver, arguments),
        }
    }
}

impl JsRuntime {
    pub(in crate::runtime) fn typed_array_constructor(
        &mut self,
        dom: &mut Dom,
        constructor: ObjectId,
        kind: TypedArrayKind,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let prototype = self
            .realm
            .get_property(constructor, "prototype")
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            });
        let Some(first) = arguments.first() else {
            return Ok(JsValue::Object(self.realm.typed_array(
                kind,
                TypedBuffer::default(),
                0,
                0,
                prototype,
            )));
        };
        let elements = match first {
            JsValue::Undefined | JsValue::Null => Vec::new(),
            JsValue::Object(_) => self.typed_array_source_values(dom, first)?,
            other => {
                let length = self.typed_index(other)?;
                return self.create_typed_array(kind, length, prototype);
            }
        };
        self.create_typed_array_from_values(kind, &elements, prototype)
    }

    /// Element values of a typed-array constructor/`from` source object: an
    /// element-wise copy of another typed array, or an array-like read
    /// through indexed gets.
    pub(in crate::runtime) fn typed_array_source_values(
        &mut self,
        dom: &mut Dom,
        source: &JsValue,
    ) -> Result<Vec<f64>, JsError> {
        let Some(JsValue::Object(source)) = Some(source) else {
            return Ok(Vec::new());
        };
        if let Some(ObjectHost::TypedArray {
            buffer,
            start,
            length,
            ..
        }) = self.realm.host(*source)
        {
            return Ok(buffer.0.borrow()[start..start + length].to_vec());
        }
        let length = self.array_like_length(dom, *source)?;
        let mut values = Vec::new();
        for index in 0..length {
            let element = self.get_member(dom, *source, &index.to_string())?;
            values.push(to_number(&element)?);
        }
        Ok(values)
    }

    /// `TypedArray.from(source[, mapper])`: build one typed array from
    /// another typed array, an array-like, or a string's characters, with an
    /// optional mapping callback.
    pub(in crate::runtime) fn typed_array_from(
        &mut self,
        dom: &mut Dom,
        constructor: ObjectId,
        kind: TypedArrayKind,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let source = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let mut values = match &source {
            JsValue::Object(_) => self.typed_array_source_values(dom, &source)?,
            JsValue::String(text) => text
                .chars()
                .map(|character| f64::from(u32::from(character)))
                .collect(),
            _ => {
                return Err(JsError::type_error(
                    "TypedArray.from requires an array-like or iterable source",
                ));
            }
        };
        if let Some(JsValue::Object(mapper)) = arguments.get(1) {
            let mapper = Self::require_callable_object(&JsValue::Object(*mapper), &self.realm)?;
            for (index, value) in values.iter_mut().enumerate() {
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "source indices stay far below any precision boundary"
                )]
                let index_value = JsValue::Number(index as f64);
                let element = self.call(dom, mapper, &[JsValue::Number(*value), index_value])?;
                *value = to_number(&element)?;
            }
        }
        let prototype = self
            .realm
            .get_property(constructor, "prototype")
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            });
        self.create_typed_array_from_values(kind, &values, prototype)
    }

    pub(in crate::runtime) fn create_typed_array(
        &mut self,
        kind: TypedArrayKind,
        length: usize,
        prototype: Option<ObjectId>,
    ) -> Result<JsValue, JsError> {
        if length > Self::MAX_TYPED_ARRAY_ELEMENTS {
            return Err(self.range_error("typed array length exceeds the engine bound"));
        }
        let buffer = TypedBuffer(std::rc::Rc::new(std::cell::RefCell::new(vec![0.0; length])));
        self.ensure_heap_capacity(1)?;
        Ok(JsValue::Object(
            self.realm.typed_array(kind, buffer, 0, length, prototype),
        ))
    }

    pub(in crate::runtime) fn create_typed_array_from_values(
        &mut self,
        kind: TypedArrayKind,
        values: &[f64],
        prototype: Option<ObjectId>,
    ) -> Result<JsValue, JsError> {
        if values.len() > Self::MAX_TYPED_ARRAY_ELEMENTS {
            return Err(self.range_error("typed array length exceeds the engine bound"));
        }
        let encoded = values.iter().map(|value| kind.encode(*value)).collect();
        let buffer = TypedBuffer(std::rc::Rc::new(std::cell::RefCell::new(encoded)));
        #[allow(
            clippy::cast_precision_loss,
            reason = "typed-array lengths stay far below any precision boundary"
        )]
        let length = buffer.0.borrow().len();
        self.ensure_heap_capacity(1)?;
        Ok(JsValue::Object(
            self.realm.typed_array(kind, buffer, 0, length, prototype),
        ))
    }

    pub(in crate::runtime) fn typed_array_host(
        &self,
        receiver: ObjectId,
    ) -> Result<(TypedArrayKind, TypedBuffer, usize, usize), JsError> {
        match self.realm.host(receiver) {
            Some(ObjectHost::TypedArray {
                kind,
                buffer,
                start,
                length,
            }) => Ok((kind, buffer, start, length)),
            _ => Err(JsError::type_error(
                "typed-array method called on a non-typed-array receiver",
            )),
        }
    }

    pub(in crate::runtime) fn typed_array_elements(
        &self,
        receiver: ObjectId,
    ) -> Result<Vec<f64>, JsError> {
        let (_, buffer, start, length) = self.typed_array_host(receiver)?;
        Ok(buffer.0.borrow()[start..start + length].to_vec())
    }

    pub(in crate::runtime) fn typed_array_set(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let (kind, buffer, start, length) = self.typed_array_host(receiver)?;
        let Some(source) = arguments.first() else {
            return Err(JsError::type_error("set requires a source argument"));
        };
        let offset = match arguments.get(1) {
            None | Some(JsValue::Undefined) => 0,
            Some(value) => self.typed_index(value)?,
        };
        let values: Vec<f64> = match source {
            JsValue::Object(object) => match self.realm.host(*object) {
                Some(ObjectHost::TypedArray {
                    buffer: source_buffer,
                    start: source_start,
                    length: source_length,
                    ..
                }) => source_buffer.0.borrow()[source_start..source_start + source_length].to_vec(),
                Some(ObjectHost::Array) => self
                    .array_elements_for(*object)
                    .iter()
                    .map(to_number)
                    .collect::<Result<Vec<_>, _>>()?,
                _ => {
                    let count = self.array_like_length(dom, *object)?;
                    let mut values = Vec::new();
                    for index in 0..count {
                        let element = self.get_member(dom, *object, &index.to_string())?;
                        values.push(to_number(&element)?);
                    }
                    values
                }
            },
            _ => {
                return Err(JsError::type_error(
                    "typed-array set requires an array-like source",
                ));
            }
        };
        if offset
            .checked_add(values.len())
            .is_none_or(|end| end > length)
        {
            return Err(self.range_error("source is too large"));
        }
        {
            let mut elements = buffer.0.borrow_mut();
            for (delta, value) in values.iter().enumerate() {
                elements[start + offset + delta] = kind.encode(*value);
            }
        }
        Ok(JsValue::Object(receiver))
    }

    /// Prototype for views/copies derived from `receiver`'s constructor.
    pub(in crate::runtime) fn typed_array_derived_prototype(
        &self,
        receiver: ObjectId,
    ) -> Option<ObjectId> {
        self.realm
            .get_property(receiver, "constructor")
            .and_then(|value| match value {
                JsValue::Object(constructor) => self
                    .realm
                    .get_property(constructor, "prototype")
                    .and_then(|value| match value {
                        JsValue::Object(prototype) => Some(prototype),
                        _ => None,
                    }),
                _ => None,
            })
    }

    pub(in crate::runtime) fn typed_array_subarray(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let (kind, buffer, start, length) = self.typed_array_host(receiver)?;
        let (begin, end) = Self::typed_range(arguments, length)?;
        self.ensure_heap_capacity(1)?;
        let prototype = self.typed_array_derived_prototype(receiver);
        Ok(JsValue::Object(self.realm.typed_array(
            kind,
            buffer,
            start + begin,
            end - begin,
            prototype,
        )))
    }

    pub(in crate::runtime) fn typed_array_slice(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let (kind, _, _, length) = self.typed_array_host(receiver)?;
        let (begin, end) = Self::typed_range(arguments, length)?;
        let elements = self.typed_array_elements(receiver)?;
        let prototype = self.typed_array_derived_prototype(receiver);
        self.create_typed_array_from_values(kind, &elements[begin..end], prototype)
    }

    /// Resolve the half-open [begin, end) element range shared by `fill`,
    /// `subarray`, and `slice`, with clamping and negative relative indices.
    pub(in crate::runtime) fn typed_range(
        arguments: &[JsValue],
        length: usize,
    ) -> Result<(usize, usize), JsError> {
        let relative = |value: Option<&JsValue>, fallback: f64| -> Result<f64, JsError> {
            match value {
                None | Some(JsValue::Undefined) => Ok(fallback),
                Some(value) => Ok(to_number(value)?),
            }
        };
        let begin = relative(arguments.first(), 0.0)?;
        let end = relative(arguments.get(1), length as f64)?;
        let clamp_index = |value: f64| -> usize {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "range endpoints clamp into 0..=length"
            )]
            let index = if value < 0.0 {
                length as f64 + value
            } else {
                value
            };
            index.clamp(0.0, length as f64) as usize
        };
        let begin = clamp_index(begin);
        let end = clamp_index(end);
        Ok((begin.min(end), begin.max(end)))
    }

    pub(in crate::runtime) fn typed_array_fill(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let (kind, buffer, start, length) = self.typed_array_host(receiver)?;
        let fill_value = match arguments.first() {
            Some(value) => kind.encode(to_number(value)?),
            None => kind.encode(f64::NAN),
        };
        let rest = arguments.get(1..).unwrap_or(&[]);
        let (begin, end) = Self::typed_range(rest, length)?;
        {
            let mut elements = buffer.0.borrow_mut();
            for slot in &mut elements[start + begin..start + end] {
                *slot = fill_value;
            }
        }
        Ok(JsValue::Object(receiver))
    }

    pub(in crate::runtime) fn typed_array_index_of(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let (_, buffer, start, length) = self.typed_array_host(receiver)?;
        let Some(search) = arguments.first().and_then(|value| match to_number(value) {
            Ok(number) if !number.is_nan() => Some(number),
            _ => None,
        }) else {
            return Ok(JsValue::Number(-1.0));
        };
        let from = Self::typed_from_index(arguments.get(1), length)?;
        let elements = buffer.0.borrow();
        for (delta, element) in elements[start + from..start + length].iter().enumerate() {
            if *element == search {
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "index results stay far below any precision boundary"
                )]
                return Ok(JsValue::Number((from + delta) as f64));
            }
        }
        Ok(JsValue::Number(-1.0))
    }

    /// Resolve an optional `fromIndex` argument shared by `indexOf` and
    /// `includes`: negative values count back from the end, and the result
    /// clamps into `0..=length`.
    pub(in crate::runtime) fn typed_from_index(
        value: Option<&JsValue>,
        length: usize,
    ) -> Result<usize, JsError> {
        let Some(value) = value else {
            return Ok(0);
        };
        if matches!(value, JsValue::Undefined) {
            return Ok(0);
        }
        let number = to_number(value)?;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "negative from-index wraps relative to the array length"
        )]
        let index = if number < 0.0 {
            length as f64 + number
        } else {
            number
        };
        Ok(index.clamp(0.0, length as f64) as usize)
    }

    pub(in crate::runtime) fn typed_array_includes(
        &self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let (_, buffer, start, length) = self.typed_array_host(receiver)?;
        let Some(value) = arguments.first() else {
            return Ok(JsValue::Boolean(false));
        };
        let search = to_number(value)?;
        let from = Self::typed_from_index(arguments.get(1), length)?;
        let elements = buffer.0.borrow();
        for element in &elements[start + from..start + length] {
            // `includes` uses SameValueZero, so `NaN` finds `NaN`.
            if *element == search || (search.is_nan() && element.is_nan()) {
                return Ok(JsValue::Boolean(true));
            }
        }
        Ok(JsValue::Boolean(false))
    }

    pub(in crate::runtime) fn typed_array_for_each(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        callback: ObjectId,
    ) -> Result<JsValue, JsError> {
        let (_, buffer, start, length) = self.typed_array_host(receiver)?;
        for index in 0..length {
            // Read each element through a short borrow so user callbacks can
            // safely write back through the same view.
            let Some(element) = buffer.0.borrow().get(start + index).copied() else {
                break;
            };
            self.call(
                dom,
                callback,
                &[
                    JsValue::Number(element),
                    JsValue::Number(index as f64),
                    JsValue::Object(receiver),
                ],
            )?;
        }
        Ok(JsValue::Undefined)
    }

    /// Shared body of `map` and `filter`; both produce a same-species copy
    /// through the receiver's constructor.
    pub(in crate::runtime) fn typed_array_map_or_filter(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        callback: ObjectId,
        map: bool,
    ) -> Result<JsValue, JsError> {
        let (kind, buffer, start, length) = self.typed_array_host(receiver)?;
        let mut output = Vec::new();
        for index in 0..length {
            let Some(element) = buffer.0.borrow().get(start + index).copied() else {
                break;
            };
            let mapped = self.call(
                dom,
                callback,
                &[
                    JsValue::Number(element),
                    JsValue::Number(index as f64),
                    JsValue::Object(receiver),
                ],
            )?;
            if map {
                output.push(to_number(&mapped)?);
            } else if mapped.is_truthy() {
                output.push(element);
            }
        }
        let prototype = self.typed_array_derived_prototype(receiver);
        self.create_typed_array_from_values(kind, &output, prototype)
    }

    pub(in crate::runtime) fn typed_array_join(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let (_, buffer, start, length) = self.typed_array_host(receiver)?;
        let separator = match arguments.first() {
            None | Some(JsValue::Undefined) => ",".to_owned(),
            Some(value) => value.to_js_string(),
        };
        let elements = buffer.0.borrow();
        let parts = elements[start..start + length]
            .iter()
            .map(|element| JsValue::Number(*element).to_js_string())
            .collect::<Vec<_>>();
        Ok(JsValue::String(parts.join(&separator)))
    }

    /// `ToIndex` for typed-array lengths and offsets: `NaN` clamps to zero and
    /// negative or non-finite values raise a `RangeError`.
    pub(in crate::runtime) fn typed_index(
        &mut self,
        value: &JsValue,
    ) -> Result<usize, JsError> {
        let number = to_number(value)?;
        if number.is_nan() {
            return Ok(0);
        }
        if !number.is_finite() || number < 0.0 || number > Self::MAX_TYPED_ARRAY_ELEMENTS as f64 {
            return Err(self.range_error("invalid typed array index"));
        }
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "index is validated as a finite non-negative integer"
        )]
        Ok(number.trunc() as usize)
    }
}
