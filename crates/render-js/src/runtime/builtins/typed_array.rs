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

use super::array::callback_this_argument;
use super::object::{AccessorField, PartialDescriptor, PropertyName};
use crate::JsError;
use crate::JsErrorKind;
use crate::JsSymbol;
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::runtime::convert::required_argument;
use crate::utf16::JsString;
use crate::value::ArrayView;
use crate::value::NativeFunction;
use crate::value::ObjectHost;
use crate::value::TypedArrayKind;
use crate::value::TypedBuffer;
use crate::value::TypedKey;
use render_dom::Dom;

/// What a callback scan over a typed array reports (ECMA-262 23.2.3.x).
#[derive(Clone, Copy)]
enum Scan {
    /// `every`: `false` at the first falsy result, otherwise `true`.
    Every,
    /// `some`: `true` at the first truthy result, otherwise `false`.
    Some,
    /// `find` and `findLast`: the first element whose result is truthy.
    Find,
    /// `findIndex` and `findLastIndex`: the index of that element, or -1.
    FindIndex,
}

impl JsRuntime {
    pub(in crate::runtime) fn dispatch_typed_array_native(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match function {
            NativeFunction::TypedArrayIntrinsic => Err(JsError::type_error(
                "Abstract class TypedArray not directly constructable",
            )),
            NativeFunction::TypedArraySet => self.typed_array_set(dom, receiver, arguments),
            NativeFunction::TypedArraySubarray => {
                self.typed_array_subarray(dom, receiver, arguments)
            }
            NativeFunction::TypedArraySlice => self.typed_array_slice(dom, receiver, arguments),
            NativeFunction::TypedArrayFill => self.typed_array_fill(dom, receiver, arguments),
            NativeFunction::TypedArrayIndexOf => {
                self.typed_array_index_of(dom, receiver, arguments)
            }
            NativeFunction::TypedArrayLastIndexOf => {
                self.typed_array_last_index_of(dom, receiver, arguments)
            }
            NativeFunction::TypedArrayIncludes => {
                self.typed_array_includes(dom, receiver, arguments)
            }
            NativeFunction::TypedArrayJoin => self.typed_array_join(dom, receiver, arguments),
            NativeFunction::TypedArrayToLocaleString => {
                // ECMA-262 23.2.3.32: the Array algorithm, over the view's length
                // after ValidateTypedArray.
                let (_, _, _, length) = self.typed_array_host(receiver)?;
                let length = length as f64;
                self.locale_string_of(dom, receiver, length)
            }
            NativeFunction::TypedArrayValues => {
                self.typed_array_host(receiver)?;
                self.array_view_iterator(receiver, ArrayView::Values)
            }
            NativeFunction::TypedArrayKeys => self.typed_array_keys(receiver),
            NativeFunction::TypedArrayEntries => self.typed_array_entries(receiver),
            NativeFunction::TypedArrayAt => self.typed_array_at(dom, receiver, arguments),
            NativeFunction::TypedArrayCopyWithin => {
                self.typed_array_copy_within(dom, receiver, arguments)
            }
            NativeFunction::TypedArrayEvery => {
                self.typed_array_scan(dom, receiver, arguments, Scan::Every, false)
            }
            NativeFunction::TypedArraySome => {
                self.typed_array_scan(dom, receiver, arguments, Scan::Some, false)
            }
            NativeFunction::TypedArrayFind => {
                self.typed_array_scan(dom, receiver, arguments, Scan::Find, false)
            }
            NativeFunction::TypedArrayFindIndex => {
                self.typed_array_scan(dom, receiver, arguments, Scan::FindIndex, false)
            }
            NativeFunction::TypedArrayFindLast => {
                self.typed_array_scan(dom, receiver, arguments, Scan::Find, true)
            }
            NativeFunction::TypedArrayFindLastIndex => {
                self.typed_array_scan(dom, receiver, arguments, Scan::FindIndex, true)
            }
            NativeFunction::TypedArrayReduce => {
                self.typed_array_reduce(dom, receiver, arguments, false)
            }
            NativeFunction::TypedArrayReduceRight => {
                self.typed_array_reduce(dom, receiver, arguments, true)
            }
            NativeFunction::TypedArrayReverse => self.typed_array_reverse(receiver),
            NativeFunction::TypedArraySort => self.typed_array_sort(dom, receiver, arguments),
            NativeFunction::TypedArrayToReversed => self.typed_array_to_reversed(receiver),
            NativeFunction::TypedArrayToSorted => {
                self.typed_array_to_sorted(dom, receiver, arguments)
            }
            NativeFunction::TypedArrayWith => self.typed_array_with(dom, receiver, arguments),
            NativeFunction::TypedArrayFrom => self.typed_array_from(dom, receiver, arguments),
            NativeFunction::TypedArrayFromBase64 => self.uint8_array_from_base64(dom, arguments),
            NativeFunction::TypedArrayFromHex => self.uint8_array_from_hex(arguments),
            NativeFunction::TypedArraySetFromBase64 => {
                self.uint8_array_set_from_base64(dom, receiver, arguments)
            }
            NativeFunction::TypedArraySetFromHex => {
                self.uint8_array_set_from_hex(receiver, arguments)
            }
            NativeFunction::TypedArrayToBase64 => {
                self.uint8_array_to_base64(dom, receiver, arguments)
            }
            NativeFunction::TypedArrayToHex => self.uint8_array_to_hex(receiver),
            NativeFunction::TypedArrayOf => self.typed_array_of(dom, receiver, arguments),
            NativeFunction::TypedArraySpecies => Ok(JsValue::Object(receiver)),
            NativeFunction::TypedArrayBufferGetter => {
                let (_, buffer, _, _) = self.typed_array_parts(receiver)?;
                Ok(JsValue::Object(self.array_buffer_object(&buffer)?))
            }
            NativeFunction::TypedArrayLengthGetter => self.typed_array_length(receiver),
            NativeFunction::TypedArrayByteLengthGetter => self.typed_array_byte_length(receiver),
            NativeFunction::TypedArrayByteOffsetGetter => self.typed_array_byte_offset(receiver),
            NativeFunction::TypedArrayToStringTagGetter => {
                Ok(self.typed_array_to_string_tag(receiver))
            }
            NativeFunction::TypedArrayForEach => {
                let callback = Self::require_callable_object(
                    required_argument(arguments, 0, "forEach")?,
                    &self.realm,
                )?;
                let this_argument = callback_this_argument(arguments);
                self.typed_array_for_each(dom, receiver, callback, &this_argument)
            }
            NativeFunction::TypedArrayMap => {
                let callback = Self::require_callable_object(
                    required_argument(arguments, 0, "map")?,
                    &self.realm,
                )?;
                let this_argument = callback_this_argument(arguments);
                self.typed_array_map_or_filter(dom, receiver, callback, &this_argument, true)
            }
            NativeFunction::TypedArrayFilter => {
                let callback = Self::require_callable_object(
                    required_argument(arguments, 0, "filter")?,
                    &self.realm,
                )?;
                let this_argument = callback_this_argument(arguments);
                self.typed_array_map_or_filter(dom, receiver, callback, &this_argument, false)
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
        let Some(first) = arguments.first() else {
            let prototype = self.new_target_prototype(dom, constructor)?;
            return Ok(JsValue::Object(self.realm.typed_array(
                kind,
                TypedBuffer::default(),
                0,
                Some(0),
                prototype,
            )));
        };
        // `new Uint8Array(buffer[, byteOffset[, length]])` shares the buffer, so
        // a `DataView` write over the same buffer is visible through the typed
        // array. The offset and length are byte positions the spec requires to
        // be element-aligned.
        if let Some(ObjectHost::ArrayBufferHost(buffer)) = match first {
            JsValue::Object(object) => self.realm.host(*object),
            _ => None,
        } {
            let prototype = self.new_target_prototype(dom, constructor)?;
            return self.typed_array_over_buffer(
                dom,
                kind,
                buffer,
                arguments.get(1),
                arguments.get(2),
                prototype,
            );
        }
        // The source is validated before the prototype is read, so a source that
        // throws a TypeError does so before any custom `prototype` is accessed.
        let elements = match first {
            JsValue::Undefined | JsValue::Null => Vec::new(),
            JsValue::Object(_) => self.typed_array_source_values(dom, kind, first)?,
            other => {
                let length = self.typed_index(dom, other)?;
                let prototype = self.new_target_prototype(dom, constructor)?;
                return self.create_typed_array(kind, length, prototype);
            }
        };
        let prototype = self.new_target_prototype(dom, constructor)?;
        self.create_typed_array_from_elements(kind, &elements, prototype)
    }

    /// A typed-array view over an existing `ArrayBuffer` (ECMA-262 23.2.5.1
    /// `InitializeTypedArrayFromArrayBuffer`). The view shares the buffer's store,
    /// so a write through either is visible through the other.
    fn typed_array_over_buffer(
        &mut self,
        dom: &mut Dom,
        kind: TypedArrayKind,
        buffer: TypedBuffer,
        byte_offset: Option<&JsValue>,
        length: Option<&JsValue>,
        prototype: Option<ObjectId>,
    ) -> Result<JsValue, JsError> {
        let element_size = kind.element_size();
        // Steps 6-7: `ToIndex(byteOffset)` must be a multiple of the element size.
        let offset = self.optional_integer_value(dom, byte_offset)?;
        if !offset.is_finite() || offset < 0.0 {
            return Err(self.range_error("byteOffset must be a non-negative integer"));
        }
        let offset_value = offset as usize;
        if !offset_value.is_multiple_of(element_size) {
            return Err(self.range_error("byteOffset must be a multiple of the element size"));
        }
        // Step 8: `ToIndex(length)` when it is given.
        let requested = match length {
            None | Some(JsValue::Undefined) => None,
            Some(value) => {
                let requested = self.optional_integer_value(dom, Some(value))?;
                if !requested.is_finite() || requested < 0.0 {
                    return Err(self.range_error("length must be a non-negative integer"));
                }
                Some(requested as usize)
            }
        };
        // Step 9: a detached buffer cannot back a view.
        buffer.ensure_attached()?;
        let total_bytes = buffer.byte_length();
        if offset_value > total_bytes {
            return Err(self.range_error("byteOffset is past the end of the buffer"));
        }
        // Steps 10-16: without a length, a resizable buffer gives a
        // length-tracking view; a fixed-length buffer gives the rest of itself,
        // which must be a whole number of elements.
        let length = match requested {
            None if buffer.max_byte_length().is_some() => None,
            None => {
                if !total_bytes.is_multiple_of(element_size) {
                    return Err(self.range_error(
                        "byte length of the buffer must be a multiple of the element size",
                    ));
                }
                Some((total_bytes - offset_value) / element_size)
            }
            Some(count) => {
                if offset_value + count * element_size > total_bytes {
                    return Err(self.range_error("length is past the end of the buffer"));
                }
                Some(count)
            }
        };
        self.ensure_heap_capacity(1)?;
        Ok(JsValue::Object(self.realm.typed_array(
            kind,
            buffer,
            offset_value / element_size,
            length,
            prototype,
        )))
    }

    /// Element values of a typed-array constructor/`from` source object: an
    /// element-wise copy of another typed array, or an array-like read
    /// through indexed gets.
    pub(in crate::runtime) fn typed_array_source_values(
        &mut self,
        dom: &mut Dom,
        kind: TypedArrayKind,
        source: &JsValue,
    ) -> Result<Vec<JsValue>, JsError> {
        let Some(JsValue::Object(source)) = Some(source) else {
            return Ok(Vec::new());
        };
        if let Some(ObjectHost::TypedArray { .. }) = self.realm.host(*source) {
            // InitializeTypedArrayFromTypedArray: a detached or out-of-bounds
            // source is a TypeError, and so is a source whose content type differs.
            let (source_kind, buffer, start, length) = self.typed_array_host(*source)?;
            if source_kind.is_bigint() != kind.is_bigint() {
                return Err(JsError::type_error(
                    "cannot mix BigInt and other types in typed array copies",
                ));
            }
            return buffer.elements_value(source_kind, start, length);
        }
        // ECMA-262 23.2.5.1 step 6: an object with `@@iterator` supplies its
        // values through the iterator; anything else is an array-like.
        let elements = self.iterate_values(dom, &JsValue::Object(*source))?;
        let mut values = Vec::with_capacity(elements.len());
        for element in &elements {
            values.push(self.typed_element_value(dom, kind, element)?);
        }
        Ok(values)
    }

    /// Converts one value for an element of `kind`: `ToBigInt` for the `BigInt`
    /// kinds and `ToNumber` for the rest, the conversion every typed array
    /// element write uses.
    pub(in crate::runtime) fn typed_element_value(
        &mut self,
        dom: &mut Dom,
        kind: TypedArrayKind,
        value: &JsValue,
    ) -> Result<JsValue, JsError> {
        if kind.is_bigint() {
            Ok(JsValue::BigInt(self.to_bigint_value(dom, value)?))
        } else {
            Ok(JsValue::Number(self.to_number_value(dom, value)?))
        }
    }

    /// `%TypedArray%.from(source[, mapfn[, thisArg]])` (ECMA-262 23.2.2.1) on any
    /// constructor `this`. A source with an `@@iterator` method is iterated and
    /// its values listed before the target is created; any other source is read
    /// as an array-like. The mapper sees each raw value, and each result is
    /// assigned to the target with `Set`, which applies the element conversion.
    pub(in crate::runtime) fn typed_array_from(
        &mut self,
        dom: &mut Dom,
        constructor: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        // Step 2: `this` must be a constructor.
        if !self.is_constructor(constructor) {
            return Err(JsError::type_error(
                "TypedArray.from requires a constructor receiver",
            ));
        }
        // Steps 3-4: a mapper that is not callable throws before the source is read.
        let source = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let mapper = match arguments.get(1) {
            None | Some(JsValue::Undefined) => None,
            Some(value) => Some(Self::require_callable_object(value, &self.realm)?),
        };
        let this_argument = arguments.get(2).cloned().unwrap_or(JsValue::Undefined);
        // Step 5: GetMethod(source, @@iterator). ToObject throws for undefined and null.
        let pinned = self.transient_roots.len();
        let object = self.to_object(&source)?;
        self.transient_roots.push(object);
        let method = self.get_symbol_value(dom, object, &JsSymbol::well_known("@@iterator"))?;
        let (length, iterated) = match method {
            JsValue::Undefined | JsValue::Null => (self.length_of_array_like(dom, object)?, None),
            JsValue::Object(method) if Self::is_callable_object(method, &self.realm) => {
                let values = self.typed_array_iterated_values(dom, &source, method)?;
                (values.len() as f64, Some(values))
            }
            _ => {
                return Err(JsError::type_error(
                    "TypedArray.from requires a callable @@iterator",
                ));
            }
        };
        // Step 9-10: the target is created with the length before any mapping runs.
        let target =
            self.typed_array_create_from_constructor(dom, constructor, &[JsValue::Number(length)])?;
        self.transient_roots.push(target);
        // `TypedArrayCreateFromConstructor(..., ~write~)`: a species constructor
        // may return a view over an immutable buffer, which cannot be filled.
        self.typed_array_write_host(target)?;
        for index in 0..length as usize {
            let value = match &iterated {
                Some(values) => values[index].clone(),
                None => self.get_member(dom, object, &index.to_string())?,
            };
            let value = match mapper {
                Some(mapper) => self.call_with_this(
                    dom,
                    mapper,
                    &[value, JsValue::Number(index as f64)],
                    this_argument.clone(),
                )?,
                None => value,
            };
            self.typed_array_set_element_or_throw(dom, target, index, value)?;
        }
        self.transient_roots.truncate(pinned);
        Ok(JsValue::Object(target))
    }

    /// `%TypedArray%.of(...items)` (ECMA-262 23.2.2.2) on any constructor `this`.
    fn typed_array_of(
        &mut self,
        dom: &mut Dom,
        constructor: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        if !self.is_constructor(constructor) {
            return Err(JsError::type_error(
                "TypedArray.of requires a constructor receiver",
            ));
        }
        let pinned = self.transient_roots.len();
        let target = self.typed_array_create_from_constructor(
            dom,
            constructor,
            &[JsValue::Number(arguments.len() as f64)],
        )?;
        self.transient_roots.push(target);
        // `TypedArrayCreateFromConstructor(..., ~write~)`.
        self.typed_array_write_host(target)?;
        for (index, value) in arguments.iter().enumerate() {
            self.typed_array_set_element_or_throw(dom, target, index, value.clone())?;
        }
        self.transient_roots.truncate(pinned);
        Ok(JsValue::Object(target))
    }

    /// The values an iterable `source` produces through `method`, as
    /// `IteratorToList` (ECMA-262 7.4.2). The iterator and each object value stay
    /// pinned in `transient_roots` for the caller, which truncates them.
    fn typed_array_iterated_values(
        &mut self,
        dom: &mut Dom,
        source: &JsValue,
        method: ObjectId,
    ) -> Result<Vec<JsValue>, JsError> {
        let iterator = match self.call_with_this(dom, method, &[], source.clone())? {
            JsValue::Object(iterator) => iterator,
            _ => return Err(JsError::type_error("iterator is not an object")),
        };
        self.transient_roots.push(iterator);
        let next = match self.get_member(dom, iterator, "next")? {
            JsValue::Object(next) if Self::is_callable_object(next, &self.realm) => next,
            _ => return Err(JsError::type_error("iterator has no callable 'next'")),
        };
        self.transient_roots.push(next);
        let mut values = Vec::new();
        while let Some(step) = self.iterator_next(dom, iterator, next)? {
            if let JsValue::Object(object) = step {
                self.transient_roots.push(object);
            }
            values.push(step);
        }
        Ok(values)
    }

    /// `TypedArrayCreateFromConstructor(constructor, argumentList)` (ECMA-262
    /// 23.2.4.2): `Construct` the constructor, then require a typed array in
    /// bounds, at least as long as a single length argument.
    pub(in crate::runtime) fn typed_array_create_from_constructor(
        &mut self,
        dom: &mut Dom,
        constructor: ObjectId,
        arguments: &[JsValue],
    ) -> Result<ObjectId, JsError> {
        let JsValue::Object(result) = self.construct(dom, constructor, arguments)? else {
            return Err(JsError::type_error("constructor returned a primitive"));
        };
        let (_, _, _, length) = self.typed_array_host(result)?;
        if let [JsValue::Number(requested)] = arguments
            && (length as f64) < *requested
        {
            return Err(JsError::type_error(
                "constructor returned a typed array that is too short",
            ));
        }
        Ok(result)
    }

    /// `Set(target, index, value, true)` for an element of a `from` or `of`
    /// target: a refused assignment is a `TypeError` (ECMA-262 7.3.4).
    fn typed_array_set_element_or_throw(
        &mut self,
        dom: &mut Dom,
        target: ObjectId,
        index: usize,
        value: JsValue,
    ) -> Result<(), JsError> {
        let key = PropertyName::String(index.to_string());
        if self.set_property_checked(dom, target, &key, value)? {
            Ok(())
        } else {
            Err(JsError::type_error(format!(
                "cannot assign to read only property '{index}'"
            )))
        }
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
        let buffer = TypedBuffer::new(vec![0; length * kind.element_size()]);
        self.ensure_heap_capacity(1)?;
        Ok(JsValue::Object(self.realm.typed_array(
            kind,
            buffer,
            0,
            Some(length),
            prototype,
        )))
    }

    pub(in crate::runtime) fn create_typed_array_from_values(
        &mut self,
        kind: TypedArrayKind,
        values: &[f64],
        prototype: Option<ObjectId>,
    ) -> Result<JsValue, JsError> {
        let elements: Vec<JsValue> = values.iter().map(|value| JsValue::Number(*value)).collect();
        self.create_typed_array_from_elements(kind, &elements, prototype)
    }

    /// A typed array of `kind` holding `values`, each already converted to the
    /// element type: a `BigInt` for the `BigInt` kinds, a Number otherwise.
    pub(in crate::runtime) fn create_typed_array_from_elements(
        &mut self,
        kind: TypedArrayKind,
        values: &[JsValue],
        prototype: Option<ObjectId>,
    ) -> Result<JsValue, JsError> {
        if values.len() > Self::MAX_TYPED_ARRAY_ELEMENTS {
            return Err(self.range_error("typed array length exceeds the engine bound"));
        }
        let buffer = TypedBuffer::new(vec![0; values.len() * kind.element_size()]);
        for (index, value) in values.iter().enumerate() {
            buffer.set_converted_element(kind, index, value);
        }
        self.ensure_heap_capacity(1)?;
        Ok(JsValue::Object(self.realm.typed_array(
            kind,
            buffer,
            0,
            Some(values.len()),
            prototype,
        )))
    }

    /// The view's kind, buffer, element start and current element count. The
    /// count is `None` when the view is out of bounds or its buffer is detached,
    /// so the accessors that report such a view as zero can use it directly.
    pub(in crate::runtime) fn typed_array_parts(
        &self,
        receiver: ObjectId,
    ) -> Result<(TypedArrayKind, TypedBuffer, usize, Option<usize>), JsError> {
        match self.realm.host(receiver) {
            Some(ObjectHost::TypedArray {
                kind,
                buffer,
                start,
                length,
            }) => {
                let current = buffer.view_length(kind.element_size(), start, length);
                Ok((kind, buffer, start, current))
            }
            _ => Err(JsError::type_error(
                "typed-array method called on a non-typed-array receiver",
            )),
        }
    }

    /// `ValidateTypedArray` (ECMA-262 23.2.4.4): the view's parts, or a
    /// `TypeError` when it is out of bounds of a detached or resized buffer.
    pub(in crate::runtime) fn typed_array_host(
        &self,
        receiver: ObjectId,
    ) -> Result<(TypedArrayKind, TypedBuffer, usize, usize), JsError> {
        let (kind, buffer, start, length) = self.typed_array_parts(receiver)?;
        buffer.ensure_attached()?;
        let length = length.ok_or_else(|| {
            JsError::type_error("typed array is out of bounds of its resized ArrayBuffer")
        })?;
        Ok((kind, buffer, start, length))
    }

    /// `ValidateTypedArray(receiver, ~write~)`: the attached, in-bounds view and
    /// a mutable backing buffer. The immutable-arraybuffer proposal makes every
    /// in-place mutator start here, so its `TypeError` precedes argument
    /// conversion.
    fn typed_array_write_host(
        &self,
        receiver: ObjectId,
    ) -> Result<(TypedArrayKind, TypedBuffer, usize, usize), JsError> {
        let parts = self.typed_array_host(receiver)?;
        parts.1.ensure_mutable()?;
        Ok(parts)
    }

    /// [[Get]] of element `index` (ECMA-262 10.4.5.15): `undefined` unless the
    /// index is valid for the view's current length. Callbacks read through this,
    /// so a buffer resized mid-iteration is seen at each visit.
    pub(in crate::runtime) fn typed_array_get_index(
        &self,
        receiver: ObjectId,
        index: usize,
    ) -> JsValue {
        match self.typed_array_parts(receiver) {
            Ok((kind, buffer, start, Some(length))) if index < length => buffer
                .element_value(kind, start + index)
                .unwrap_or(JsValue::Undefined),
            _ => JsValue::Undefined,
        }
    }

    pub(in crate::runtime) fn typed_array_elements(
        &self,
        receiver: ObjectId,
    ) -> Result<Vec<JsValue>, JsError> {
        let (kind, buffer, start, length) = self.typed_array_host(receiver)?;
        buffer.elements_value(kind, start, length)
    }

    /// `%TypedArray%.prototype.set(source[, offset])` (ECMA-262 23.2.3.26): the
    /// offset is converted before the source is read, and the method returns
    /// `undefined`.
    pub(in crate::runtime) fn typed_array_set(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        // ECMA-262 23.2.3.27 steps 2-5: the receiver must be a writable typed
        // array, and the offset converts before the source is examined.
        self.typed_array_write_host(receiver)?;
        let source = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let offset = self.optional_integer_value(dom, arguments.get(1))?;
        if offset < 0.0 {
            return Err(self.range_error("offset is out of bounds"));
        }
        if let JsValue::Object(source_object) = source
            && let Some(ObjectHost::TypedArray { .. }) = self.realm.host(source_object)
        {
            // SetTypedArrayFromTypedArray (ECMA-262 23.2.3.26.1): both views must be in
            // bounds, the content types must match, and the copy is taken in full
            // before any write, so a source over the same buffer copies correctly.
            let (target_kind, target_buffer, target_start, target_length) =
                self.typed_array_host(receiver)?;
            let (source_kind, source_buffer, source_start, source_length) =
                self.typed_array_host(source_object)?;
            if source_kind.is_bigint() != target_kind.is_bigint() {
                return Err(JsError::type_error(
                    "cannot mix BigInt and other types in typed array copies",
                ));
            }
            if offset == f64::INFINITY || source_length as f64 + offset > target_length as f64 {
                return Err(self.range_error("source is too large"));
            }
            let values = source_buffer.elements_value(source_kind, source_start, source_length)?;
            let first = target_start + offset as usize;
            store_typed_elements(&target_buffer, target_kind, first, &values);
            return Ok(JsValue::Undefined);
        }
        // SetTypedArrayFromArrayLike (ECMA-262 23.2.3.26.2): the target must be in
        // bounds, the source's length is checked against it before any element is
        // read, and each element is read and then written in order.
        let (_, _, _, target_length) = self.typed_array_host(receiver)?;
        let source_object = self.to_object(&source)?;
        let count = self.array_like_length(dom, source_object)?;
        if offset == f64::INFINITY || count as f64 + offset > target_length as f64 {
            return Err(self.range_error("source is too large"));
        }
        let first = offset as usize;
        for index in 0..count {
            let element = self.get_member(dom, source_object, &index.to_string())?;
            self.typed_array_set_index(dom, receiver, first + index, &element)?;
        }
        Ok(JsValue::Undefined)
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

    /// `TypedArraySpeciesCreate(exemplar, argumentList)` (ECMA-262 23.2.4.1): the
    /// result of `new` on the exemplar's species constructor. That result must be
    /// a typed array in bounds, of the exemplar's content type, and at least as
    /// long as a single length argument. The default constructor is the intrinsic
    /// for `kind`, which is what an absent or null species falls back to.
    pub(in crate::runtime) fn typed_array_species_create(
        &mut self,
        dom: &mut Dom,
        exemplar: ObjectId,
        kind: TypedArrayKind,
        arguments: &[JsValue],
        writable: bool,
    ) -> Result<ObjectId, JsError> {
        // SpeciesConstructor over the kind's intrinsic constructor (ECMA-262 7.3.22).
        let Some(JsValue::Object(default)) = self.realm.global(kind.name()) else {
            return Err(JsError::type_error("no default typed-array constructor"));
        };
        let species = self.species_constructor(dom, exemplar, default)?;
        let result = self.typed_array_create_from_constructor(dom, species, arguments)?;
        let (result_kind, result_buffer, _, _) = self.typed_array_host(result)?;
        if result_kind.is_bigint() != kind.is_bigint() {
            return Err(JsError::type_error(
                "cannot mix BigInt and other types in typed array copies",
            ));
        }
        // `TypedArraySpeciesCreate(..., ~write~)` validates the destination
        // before any element is visited.
        if writable {
            result_buffer.ensure_mutable()?;
        }
        Ok(result)
    }

    /// `Set(target, index, value, true)` on a typed array (ECMA-262 10.4.5.5):
    /// the value converts to the target's element type, and an index past the
    /// target's current length is ignored.
    pub(in crate::runtime) fn typed_array_set_index(
        &mut self,
        dom: &mut Dom,
        target: ObjectId,
        index: usize,
        value: &JsValue,
    ) -> Result<(), JsError> {
        // TypedArraySetElement (ECMA-262 10.4.5.16): the conversion runs first, and
        // the index is checked against the view as it is afterwards.
        let (kind, _, _, _) = self.typed_array_parts(target)?;
        let converted = self.typed_element_value(dom, kind, value)?;
        let (_, buffer, start, length) = self.typed_array_parts(target)?;
        if length.is_some_and(|length| index < length) {
            buffer.ensure_mutable()?;
            buffer.set_converted_element(kind, start + index, &converted);
        }
        Ok(())
    }

    /// `%TypedArray%.prototype.subarray` (ECMA-262 23.2.3.30). An out-of-bounds
    /// receiver has length zero rather than throwing, and a length-tracking
    /// receiver with no `end` gives a length-tracking result. The view comes from
    /// `TypedArraySpeciesCreate`, over the same buffer.
    pub(in crate::runtime) fn typed_array_subarray(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let (kind, buffer, start, length) = self.typed_array_parts(receiver)?;
        let (begin, end) = self.typed_range(dom, arguments, length.unwrap_or(0))?;
        let tracking = matches!(
            self.realm.host(receiver),
            Some(ObjectHost::TypedArray { length: None, .. })
        );
        let end_is_absent = matches!(arguments.get(1), None | Some(JsValue::Undefined));
        let begin_byte = (start + begin) * kind.element_size();
        let mut args = vec![
            JsValue::Object(self.array_buffer_object(&buffer)?),
            JsValue::Number(begin_byte as f64),
        ];
        if !(tracking && end_is_absent) {
            args.push(JsValue::Number((end - begin) as f64));
        }
        Ok(JsValue::Object(self.typed_array_species_create(
            dom, receiver, kind, &args, false,
        )?))
    }

    /// `%TypedArray%.prototype.slice(start, end)` (ECMA-262 23.2.3.27): the copy
    /// is made through `TypedArraySpeciesCreate` with the element count, and each
    /// element is stored through `Set`.
    pub(in crate::runtime) fn typed_array_slice(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let (kind, _, _, length) = self.typed_array_host(receiver)?;
        let (begin, end) = self.typed_range(dom, arguments, length)?;
        let result = self.typed_array_species_create(
            dom,
            receiver,
            kind,
            &[JsValue::Number((end - begin) as f64)],
            true,
        )?;
        if end <= begin {
            return Ok(JsValue::Object(result));
        }
        // ECMA-262 23.2.3.27 step 13: the species constructor and the coercions
        // may detach or shrink the source. A detached or out-of-bounds source
        // throws, and the copy stops at the source's current length.
        let (_, buffer, start, current) = self.typed_array_host(receiver)?;
        let end = end.min(current);
        if end <= begin {
            return Ok(JsValue::Object(result));
        }
        let count = end - begin;
        let (result_kind, result_buffer, result_start, _) = self.typed_array_host(result)?;
        if result_kind == kind {
            // Same content type: the element bits transfer unchanged, so a NaN
            // payload survives the copy (the same-type branch of step 13). That
            // branch copies a byte at a time, forward, so when both views share a
            // buffer an overlapping target reads the bytes it already wrote.
            let size = kind.element_size();
            let source_at = (start + begin) * size;
            let target_at = result_start * size;
            if buffer == result_buffer {
                for offset in 0..count * size {
                    let byte = buffer.read_bytes(source_at + offset, 1)?;
                    result_buffer.write_bytes(target_at + offset, &byte);
                }
            } else {
                let bytes = buffer.read_bytes(source_at, count * size)?;
                result_buffer.write_bytes(target_at, &bytes);
            }
        } else {
            for offset in 0..count {
                let value = self.typed_array_get_index(receiver, begin + offset);
                self.typed_array_set_index(dom, result, offset, &value)?;
            }
        }
        Ok(JsValue::Object(result))
    }

    /// The half-open element range `[begin, end)` that `subarray` and `slice`
    /// select from their first two arguments (ECMA-262 `relativeStart` and
    /// `relativeEnd`). An end before the begin is an empty range.
    pub(in crate::runtime) fn typed_range(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
        length: usize,
    ) -> Result<(usize, usize), JsError> {
        let len = length as f64;
        let begin = self.relative_position(dom, arguments.first(), len, 0.0)?;
        let end = self.relative_position(dom, arguments.get(1), len, len)?;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "both bounds are clamped into 0..=length"
        )]
        let (begin, end) = (begin as usize, end as usize);
        Ok((begin, end.max(begin)))
    }

    /// One relative index argument: `ToIntegerOrInfinity` of it, with a negative
    /// value counting back from `length`, clamped into `0..=length`. An absent or
    /// `undefined` argument selects `default`.
    fn relative_position(
        &mut self,
        dom: &mut Dom,
        value: Option<&JsValue>,
        length: f64,
        default: f64,
    ) -> Result<f64, JsError> {
        let relative = match value {
            None | Some(JsValue::Undefined) => return Ok(default),
            Some(value) => self.to_integer_value(dom, value)?,
        };
        Ok(if relative < 0.0 {
            (length + relative).max(0.0)
        } else {
            relative.min(length)
        })
    }

    pub(in crate::runtime) fn typed_array_fill(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let (kind, buffer, start, length) = self.typed_array_write_host(receiver)?;
        let value = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let fill_value = self.typed_element_value(dom, kind, &value)?;
        let rest = arguments.get(1..).unwrap_or(&[]);
        let (begin, end) = self.typed_range(dom, rest, length)?;
        // The conversions above can run user code that resizes the buffer, so the
        // view is validated again and the end is clamped to its new length
        // (ECMA-262 23.2.3.9 steps 7-9).
        let (_, _, _, current) = self.typed_array_host(receiver)?;
        let end = end.min(current);
        for index in begin..end {
            buffer.set_converted_element(kind, start + index, &fill_value);
        }
        Ok(JsValue::Object(receiver))
    }

    /// The number a search argument names. Only a number can equal an element,
    /// so any other value matches nothing.
    /// The search argument as an element of `kind`. Only a value of the element
    /// type can equal an element, so any other value matches nothing.
    fn typed_search_value(kind: TypedArrayKind, value: Option<&JsValue>) -> Option<JsValue> {
        match (kind.is_bigint(), value) {
            (false, Some(JsValue::Number(number))) => Some(JsValue::Number(*number)),
            (true, Some(JsValue::BigInt(bigint))) => Some(JsValue::BigInt(bigint.clone())),
            _ => None,
        }
    }

    /// Whether `element` equals `search`: strict equality, or `SameValueZero` for
    /// `includes`, where `NaN` finds `NaN`.
    fn element_equals(element: &JsValue, search: &JsValue, same_value_zero: bool) -> bool {
        match (element, search) {
            (JsValue::Number(left), JsValue::Number(right)) => {
                left == right || (same_value_zero && left.is_nan() && right.is_nan())
            }
            (JsValue::BigInt(left), JsValue::BigInt(right)) => left == right,
            _ => false,
        }
    }

    pub(in crate::runtime) fn typed_array_index_of(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let (kind, buffer, start, length) = self.typed_array_host(receiver)?;
        let from = self.typed_from_index(dom, arguments.get(1), length)?;
        let Some(search) = Self::typed_search_value(kind, arguments.first()) else {
            return Ok(JsValue::Number(-1.0));
        };
        let elements = buffer.elements_value(kind, start, length)?;
        for (delta, element) in elements[from..].iter().enumerate() {
            if Self::element_equals(element, &search, false) {
                return Ok(JsValue::Number((from + delta) as f64));
            }
        }
        Ok(JsValue::Number(-1.0))
    }

    /// `%TypedArray%.prototype.lastIndexOf(searchElement[, fromIndex])`
    /// (ECMA-262 23.2.3.18).
    pub(in crate::runtime) fn typed_array_last_index_of(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let (kind, buffer, start, length) = self.typed_array_host(receiver)?;
        if length == 0 {
            return Ok(JsValue::Number(-1.0));
        }
        let len = length as f64;
        let from = match arguments.get(1) {
            Some(value) => self.to_integer_value(dom, value)?,
            None => len - 1.0,
        };
        if from == f64::NEG_INFINITY {
            return Ok(JsValue::Number(-1.0));
        }
        let mut index = if from >= 0.0 {
            from.min(len - 1.0)
        } else {
            len + from
        };
        let Some(search) = Self::typed_search_value(kind, arguments.first()) else {
            return Ok(JsValue::Number(-1.0));
        };
        let elements = buffer.elements_value(kind, start, length)?;
        while index >= 0.0 {
            if Self::element_equals(&elements[index as usize], &search, false) {
                return Ok(JsValue::Number(index));
            }
            index -= 1.0;
        }
        Ok(JsValue::Number(-1.0))
    }

    /// Resolve an optional `fromIndex` argument shared by `indexOf` and
    /// `includes`: negative values count back from the end, and the result
    /// clamps into `0..=length`.
    fn typed_from_index(
        &mut self,
        dom: &mut Dom,
        value: Option<&JsValue>,
        length: usize,
    ) -> Result<usize, JsError> {
        let number = self.optional_integer_value(dom, value)?;
        let len = length as f64;
        let index = if number < 0.0 { len + number } else { number };
        Ok(index.clamp(0.0, len) as usize)
    }

    pub(in crate::runtime) fn typed_array_includes(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let (kind, buffer, start, length) = self.typed_array_host(receiver)?;
        let from = self.typed_from_index(dom, arguments.get(1), length)?;
        let Some(search) = Self::typed_search_value(kind, arguments.first()) else {
            return Ok(JsValue::Boolean(false));
        };
        let elements = buffer.elements_value(kind, start, length)?;
        for element in &elements[from..] {
            if Self::element_equals(element, &search, true) {
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
        this_argument: &JsValue,
    ) -> Result<JsValue, JsError> {
        let (_, _, _, length) = self.typed_array_host(receiver)?;
        for index in 0..length {
            // Each element is decoded at its visit, so user callbacks that write
            // back through the same view are seen by later visits.
            let element = self.typed_array_get_index(receiver, index);
            self.call_with_this(
                dom,
                callback,
                &[
                    element,
                    JsValue::Number(index as f64),
                    JsValue::Object(receiver),
                ],
                this_argument.clone(),
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
        this_argument: &JsValue,
        map: bool,
    ) -> Result<JsValue, JsError> {
        let (kind, _, _, length) = self.typed_array_host(receiver)?;
        // `map` creates its result before the visits, and `filter` creates its
        // result once the kept count is known (ECMA-262 23.2.3.21 and 23.2.3.10).
        let mapped_result = if map {
            Some(self.typed_array_species_create(
                dom,
                receiver,
                kind,
                &[JsValue::Number(length as f64)],
                true,
            )?)
        } else {
            None
        };
        let mut kept = Vec::new();
        for index in 0..length {
            let element = self.typed_array_get_index(receiver, index);
            let mapped = self.call_with_this(
                dom,
                callback,
                &[
                    element.clone(),
                    JsValue::Number(index as f64),
                    JsValue::Object(receiver),
                ],
                this_argument.clone(),
            )?;
            if let Some(result) = mapped_result {
                self.typed_array_set_index(dom, result, index, &mapped)?;
            } else if mapped.is_truthy() {
                kept.push(element);
            }
        }
        if let Some(result) = mapped_result {
            return Ok(JsValue::Object(result));
        }
        let result = self.typed_array_species_create(
            dom,
            receiver,
            kind,
            &[JsValue::Number(kept.len() as f64)],
            true,
        )?;
        for (index, value) in kept.iter().enumerate() {
            self.typed_array_set_index(dom, result, index, value)?;
        }
        Ok(JsValue::Object(result))
    }

    pub(in crate::runtime) fn typed_array_join(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let (kind, buffer, start, length) = self.typed_array_host(receiver)?;
        // ECMA-262 23.2.3.19: the separator is converted with ToString, which runs
        // user `toString` and `valueOf` code, even when the view is empty.
        let separator = match arguments.first() {
            None | Some(JsValue::Undefined) => JsString::from(","),
            Some(value) => self.to_string_value(dom, value)?,
        };
        let elements = buffer.elements_value(kind, start, length)?;
        // ECMA-262 23.2.3.19: each element is ToString'd and the parts are joined
        // by the separator's code units.
        let mut joined = JsString::empty();
        for (index, element) in elements.iter().enumerate() {
            if index > 0 {
                joined = joined.concat(&separator);
            }
            joined = joined.concat(&element.to_js_string());
        }
        Ok(JsValue::String(joined))
    }

    /// `%TypedArray%.prototype.keys()`: an iterator over the indices.
    /// `%TypedArray%.prototype.keys()` (ECMA-262 23.2.3.19): `ValidateTypedArray`,
    /// then a live iterator.
    fn typed_array_keys(&mut self, receiver: ObjectId) -> Result<JsValue, JsError> {
        self.typed_array_host(receiver)?;
        self.array_view_iterator(receiver, ArrayView::Keys)
    }

    /// `%TypedArray%.prototype.entries()` (ECMA-262 23.2.3.6): `ValidateTypedArray`,
    /// then a live iterator over `[index, value]`.
    fn typed_array_entries(&mut self, receiver: ObjectId) -> Result<JsValue, JsError> {
        self.typed_array_host(receiver)?;
        self.array_view_iterator(receiver, ArrayView::Entries)
    }

    /// `%TypedArray%.prototype.at(index)` (ECMA-262 23.2.3.1).
    fn typed_array_at(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let (_, _, _, length) = self.typed_array_host(receiver)?;
        let len = length as f64;
        let relative = self.optional_integer_value(dom, arguments.first())?;
        let index = if relative >= 0.0 {
            relative
        } else {
            len + relative
        };
        if index < 0.0 || index >= len {
            return Ok(JsValue::Undefined);
        }
        Ok(self.typed_array_get_index(receiver, index as usize))
    }

    /// `%TypedArray%.prototype.copyWithin(target, start[, end])` (ECMA-262
    /// 23.2.3.6): the copy behaves as if through a temporary, so overlapping
    /// ranges copy correctly.
    fn typed_array_copy_within(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let (kind, buffer, start, length) = self.typed_array_write_host(receiver)?;
        let len = length as f64;
        let to = self.relative_position(dom, arguments.first(), len, 0.0)?;
        let from = self.relative_position(dom, arguments.get(1), len, 0.0)?;
        let end = self.relative_position(dom, arguments.get(2), len, len)?;
        let count = (end - from).min(len - to);
        if count > 0.0 {
            // The coercions above can run user code that detaches or resizes the
            // buffer, so the view is validated again and the copy is clamped to its
            // new length (ECMA-262 23.2.3.5 steps 16.c-e).
            let (_, _, _, current) = self.typed_array_host(receiver)?;
            let count = count.min(current as f64 - to).min(current as f64 - from);
            if count > 0.0 {
                let (to, from, count) = (to as usize, from as usize, count as usize);
                let mut elements = buffer.elements_value(kind, start, current)?;
                // The source range is copied out first, so overlapping ranges copy
                // as if through a temporary.
                let copied: Vec<JsValue> = elements[from..from + count].to_vec();
                for (offset, value) in copied.into_iter().enumerate() {
                    elements[to + offset] = value;
                }
                store_typed_elements(&buffer, kind, start, &elements);
            }
        }
        Ok(JsValue::Object(receiver))
    }

    /// The shared loop of `every`, `some`, `find`, `findIndex`, `findLast` and
    /// `findLastIndex`: visit the elements in index order, or in reverse when
    /// `last` is set, and report according to `scan`.
    fn typed_array_scan(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
        scan: Scan,
        last: bool,
    ) -> Result<JsValue, JsError> {
        let (_, _, _, length) = self.typed_array_host(receiver)?;
        let callback = Self::require_callable_object(
            required_argument(arguments, 0, "callback")?,
            &self.realm,
        )?;
        let this_argument = callback_this_argument(arguments);
        let order: Vec<usize> = if last {
            (0..length).rev().collect()
        } else {
            (0..length).collect()
        };
        for index in order {
            let element = self.typed_array_get_index(receiver, index);
            let result = self.call_with_this(
                dom,
                callback,
                &[
                    element.clone(),
                    JsValue::Number(index as f64),
                    JsValue::Object(receiver),
                ],
                this_argument.clone(),
            )?;
            let truthy = result.is_truthy();
            match (scan, truthy) {
                (Scan::Every, false) => return Ok(JsValue::Boolean(false)),
                (Scan::Some, true) => return Ok(JsValue::Boolean(true)),
                (Scan::Find, true) => return Ok(element),
                (Scan::FindIndex, true) => return Ok(JsValue::Number(index as f64)),
                _ => {}
            }
        }
        Ok(match scan {
            Scan::Every => JsValue::Boolean(true),
            Scan::Some => JsValue::Boolean(false),
            Scan::Find => JsValue::Undefined,
            Scan::FindIndex => JsValue::Number(-1.0),
        })
    }

    /// `%TypedArray%.prototype.reduce` and `reduceRight` (ECMA-262 23.2.3.19).
    fn typed_array_reduce(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
        right: bool,
    ) -> Result<JsValue, JsError> {
        let (_, _, _, length) = self.typed_array_host(receiver)?;
        let callback =
            Self::require_callable_object(required_argument(arguments, 0, "reduce")?, &self.realm)?;
        let mut order: Vec<usize> = if right {
            (0..length).rev().collect()
        } else {
            (0..length).collect()
        };
        let mut accumulator = if let Some(initial) = arguments.get(1) {
            initial.clone()
        } else {
            if order.is_empty() {
                return Err(JsError::type_error(
                    "Reduce of empty typed array with no initial value",
                ));
            }
            let first = order.remove(0);
            self.typed_array_get_index(receiver, first)
        };
        for index in order {
            let element = self.typed_array_get_index(receiver, index);
            accumulator = self.call_with_this(
                dom,
                callback,
                &[
                    accumulator,
                    element,
                    JsValue::Number(index as f64),
                    JsValue::Object(receiver),
                ],
                JsValue::Undefined,
            )?;
        }
        Ok(accumulator)
    }

    /// `%TypedArray%.prototype.reverse()`: in place, and returns the receiver.
    fn typed_array_reverse(&mut self, receiver: ObjectId) -> Result<JsValue, JsError> {
        let (kind, buffer, start, length) = self.typed_array_write_host(receiver)?;
        let mut elements = buffer.elements_value(kind, start, length)?;
        elements.reverse();
        store_typed_elements(&buffer, kind, start, &elements);
        Ok(JsValue::Object(receiver))
    }

    /// The comparator argument of `sort` and `toSorted`: absent or `undefined`
    /// for the numeric default, otherwise it must be callable.
    fn typed_array_comparator(&self, value: Option<&JsValue>) -> Result<Option<ObjectId>, JsError> {
        match value {
            None | Some(JsValue::Undefined) => Ok(None),
            Some(JsValue::Object(object)) if Self::is_callable_object(*object, &self.realm) => {
                Ok(Some(*object))
            }
            Some(_) => Err(JsError::type_error(
                "The comparison function must be either a function or undefined",
            )),
        }
    }

    /// The signed order of two elements: negative when `left` sorts first. The
    /// default is numeric, with `NaN` last and `-0` before `+0`.
    fn typed_array_order(
        &mut self,
        dom: &mut Dom,
        comparator: Option<ObjectId>,
        left: &JsValue,
        right: &JsValue,
    ) -> Result<f64, JsError> {
        if let Some(function) = comparator {
            let result = self.call_with_this(
                dom,
                function,
                &[left.clone(), right.clone()],
                JsValue::Undefined,
            )?;
            let order = self.to_number_value(dom, &result)?;
            return Ok(if order.is_nan() { 0.0 } else { order });
        }
        Ok(match (left, right) {
            (JsValue::Number(left), JsValue::Number(right)) => number_order(*left, *right),
            (JsValue::BigInt(left), JsValue::BigInt(right)) => f64::from(left.cmp(right) as i8),
            _ => 0.0,
        })
    }

    /// A stable insertion sort of `elements` under [`Self::typed_array_order`].
    fn typed_array_sort_values(
        &mut self,
        dom: &mut Dom,
        comparator: Option<ObjectId>,
        elements: &mut [JsValue],
    ) -> Result<(), JsError> {
        for index in 1..elements.len() {
            let mut position = index;
            while position > 0 {
                let order = self.typed_array_order(
                    dom,
                    comparator,
                    &elements[position - 1],
                    &elements[position],
                )?;
                if order <= 0.0 {
                    break;
                }
                elements.swap(position - 1, position);
                position -= 1;
            }
        }
        Ok(())
    }

    /// `%TypedArray%.prototype.sort(comparefn)` (ECMA-262 23.2.3.29): sorts the
    /// receiver's elements in place.
    fn typed_array_sort(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let (kind, buffer, start, _) = self.typed_array_write_host(receiver)?;
        let comparator = self.typed_array_comparator(arguments.first())?;
        let mut elements = self.typed_array_elements(receiver)?;
        self.typed_array_sort_values(dom, comparator, &mut elements)?;
        store_typed_elements(&buffer, kind, start, &elements);
        Ok(JsValue::Object(receiver))
    }

    /// `%TypedArray%.prototype.toReversed()`: a reversed copy of the same kind.
    fn typed_array_to_reversed(&mut self, receiver: ObjectId) -> Result<JsValue, JsError> {
        let (kind, _, _, _) = self.typed_array_host(receiver)?;
        let mut elements = self.typed_array_elements(receiver)?;
        elements.reverse();
        let prototype = self.typed_array_derived_prototype(receiver);
        self.create_typed_array_from_elements(kind, &elements, prototype)
    }

    /// `%TypedArray%.prototype.toSorted(comparefn)`: a sorted copy of the same
    /// kind, leaving the receiver alone.
    fn typed_array_to_sorted(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let (kind, _, _, _) = self.typed_array_host(receiver)?;
        let comparator = self.typed_array_comparator(arguments.first())?;
        let mut elements = self.typed_array_elements(receiver)?;
        self.typed_array_sort_values(dom, comparator, &mut elements)?;
        let prototype = self.typed_array_derived_prototype(receiver);
        self.create_typed_array_from_elements(kind, &elements, prototype)
    }

    /// `%TypedArray%.prototype.with(index, value)` (ECMA-262 23.2.3.38): the
    /// value converts before the index is range-checked.
    fn typed_array_with(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let (kind, _, _, length) = self.typed_array_host(receiver)?;
        let len = length as f64;
        let relative = self.optional_integer_value(dom, arguments.first())?;
        let value = arguments.get(1).cloned().unwrap_or(JsValue::Undefined);
        let number = self.typed_element_value(dom, kind, &value)?;
        let index = if relative >= 0.0 {
            relative
        } else {
            len + relative
        };
        // The value's coercion can resize or detach the buffer, so the index is
        // checked against the length it leaves (IsValidIntegerIndex, ECMA-262
        // 23.2.3.38 step 9), and the copy reads past it as `undefined`.
        // A detached or out-of-bounds view has no valid index at all, so that
        // is the RangeError too, not a ValidateTypedArray TypeError.
        let current = self.typed_array_parts(receiver)?.3;
        if !current.is_some_and(|current| index >= 0.0 && index < current as f64) {
            return Err(self.range_error("Invalid typed array index"));
        }
        let current_elements = self.typed_array_elements(receiver)?;
        let mut elements = Vec::with_capacity(length);
        for position in 0..length {
            let element = if position as f64 == index {
                number.clone()
            } else if let Some(element) = current_elements.get(position) {
                element.clone()
            } else {
                self.typed_element_value(dom, kind, &JsValue::Undefined)?
            };
            elements.push(element);
        }
        let prototype = self.typed_array_derived_prototype(receiver);
        self.create_typed_array_from_elements(kind, &elements, prototype)
    }

    /// The `length` accessor of `%TypedArray%.prototype`. A view whose buffer is
    /// detached reports 0 rather than throwing (ECMA-262 23.2.4.1).
    /// The `length` accessor (ECMA-262 23.2.3.18): the current element count, or
    /// 0 when the view is out of bounds or detached.
    fn typed_array_length(&self, receiver: ObjectId) -> Result<JsValue, JsError> {
        let (_, _, _, length) = self.typed_array_parts(receiver)?;
        Ok(JsValue::Number(length.unwrap_or(0) as f64))
    }

    /// The `byteLength` accessor: the element count times the element size, or
    /// 0 for a view that is out of bounds or detached.
    fn typed_array_byte_length(&self, receiver: ObjectId) -> Result<JsValue, JsError> {
        let (kind, _, _, length) = self.typed_array_parts(receiver)?;
        Ok(JsValue::Number(
            (length.unwrap_or(0) * kind.element_size()) as f64,
        ))
    }

    /// The `byteOffset` accessor: the view's start, in bytes, or 0 for a view
    /// that is out of bounds or detached.
    fn typed_array_byte_offset(&self, receiver: ObjectId) -> Result<JsValue, JsError> {
        let (kind, _, start, length) = self.typed_array_parts(receiver)?;
        let start = if length.is_some() { start } else { 0 };
        Ok(JsValue::Number((start * kind.element_size()) as f64))
    }

    /// The `@@toStringTag` accessor: the constructor's name for a typed array,
    /// and `undefined` for anything else (ECMA-262 23.2.3.32).
    fn typed_array_to_string_tag(&self, receiver: ObjectId) -> JsValue {
        match self.typed_array_parts(receiver) {
            Ok((kind, _, _, _)) => JsValue::String((kind.name().to_owned()).into()),
            Err(_) => JsValue::Undefined,
        }
    }

    /// `ToIndex` for typed-array lengths and offsets: `NaN` clamps to zero and
    /// negative or non-finite values raise a `RangeError`.
    pub(in crate::runtime) fn typed_index(
        &mut self,
        dom: &mut Dom,
        value: &JsValue,
    ) -> Result<usize, JsError> {
        let number = self.to_integer_value(dom, value)?;
        if !number.is_finite() || number < 0.0 || number > Self::MAX_TYPED_ARRAY_ELEMENTS as f64 {
            return Err(self.range_error("invalid typed array index"));
        }
        Ok(number as usize)
    }

    /// `GetPrototypeFromConstructor(newTarget, ...)` (ECMA-262 10.1.14) for a
    /// built-in constructor: the `prototype` of the constructor `new` (or
    /// `Reflect.construct`) was applied to, which is `constructor` itself unless a
    /// subclass or an explicit new target says otherwise.
    pub(in crate::runtime) fn new_target_prototype(
        &mut self,
        dom: &mut Dom,
        constructor: ObjectId,
    ) -> Result<Option<ObjectId>, JsError> {
        let new_target = match self.new_target_stack.last() {
            Some(JsValue::Object(target)) => *target,
            _ => constructor,
        };
        match self.get_member(dom, new_target, "prototype")? {
            JsValue::Object(prototype) => Ok(Some(prototype)),
            // A `prototype` that is not an object selects the intrinsic default
            // of `constructor` (ECMA-262 10.1.14 step 3).
            _ => Ok(match self.realm.get_property(constructor, "prototype") {
                Some(JsValue::Object(prototype)) => Some(prototype),
                _ => None,
            }),
        }
    }

    /// `SpeciesConstructor(O, defaultConstructor)` (ECMA-262 7.3.22): the
    /// `constructor` of `exemplar`, then its `@@species` when that is a constructor,
    /// else `default`.
    pub(in crate::runtime) fn species_constructor(
        &mut self,
        dom: &mut Dom,
        exemplar: ObjectId,
        default: ObjectId,
    ) -> Result<ObjectId, JsError> {
        let constructor = match self.get_member(dom, exemplar, "constructor")? {
            JsValue::Undefined => return Ok(default),
            JsValue::Object(constructor) => constructor,
            _ => return Err(JsError::type_error("constructor is not an object")),
        };
        match self.get_symbol_value(dom, constructor, &JsSymbol::well_known("@@species"))? {
            JsValue::Undefined | JsValue::Null => Ok(default),
            JsValue::Object(species) if self.is_constructor(species) => Ok(species),
            _ => Err(JsError::type_error("species is not a constructor")),
        }
    }

    /// `TypedArraySetElement` (ECMA-262 10.4.5.16): the value converts first, and
    /// the element is written only if `key` is still a valid index afterwards,
    /// because a conversion may detach the buffer or shrink the view.
    pub(in crate::runtime) fn typed_array_set_element(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        key: &str,
        value: &JsValue,
    ) -> Result<(), JsError> {
        let (kind, buffer) = match self.realm.host(object) {
            Some(ObjectHost::TypedArray { kind, buffer, .. }) => (kind, buffer),
            _ => return Ok(()),
        };
        let converted = self.typed_element_value(dom, kind, value)?;
        if let TypedKey::Element(index) = self.realm.typed_key(object, key) {
            buffer.ensure_mutable()?;
            self.realm.typed_element_store(object, index, &converted);
        }
        Ok(())
    }

    /// `[[DefineOwnProperty]]` of a canonical numeric key on a typed array
    /// (ECMA-262 10.4.5.3): the key must be a valid index, the descriptor may not
    /// make the element non-configurable, non-enumerable, non-writable or an
    /// accessor, and a value is written through `TypedArraySetElement`.
    pub(in crate::runtime) fn typed_array_define_own_property(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        key: &str,
        descriptor: &PartialDescriptor,
    ) -> Result<bool, JsError> {
        if !matches!(self.realm.typed_key(object, key), TypedKey::Element(_)) {
            return Ok(false);
        }
        let accessor = !matches!(descriptor.get, AccessorField::Absent)
            || !matches!(descriptor.set, AccessorField::Absent);
        if accessor
            || descriptor.configurable == Some(false)
            || descriptor.enumerable == Some(false)
            || descriptor.writable == Some(false)
        {
            return Ok(false);
        }
        if let Some(value) = &descriptor.value {
            self.typed_array_set_element(dom, object, key, value)?;
        }
        Ok(true)
    }
}

/// The two base64 alphabets of RFC 4648 sections 4 and 5.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Base64Alphabet {
    Standard,
    Url,
}

/// `lastChunkHandling` for base64 decoding.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Base64LastChunkHandling {
    Loose,
    Strict,
    StopBeforePartial,
}

impl JsRuntime {
    /// `%Uint8Array.prototype%`, the prototype every result of the static
    /// `fromBase64`/`fromHex` decoders gets regardless of the receiver method.
    fn uint8_array_intrinsic_prototype(&self) -> Option<ObjectId> {
        self.realm
            .global("Uint8Array")
            .and_then(|value| match value {
                JsValue::Object(constructor) => self.realm.get_property(constructor, "prototype"),
                _ => None,
            })
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            })
    }

    /// `GetOptionsObject` from the arraybuffer-base64 proposal: `undefined` is
    /// an empty bag, an object is used as-is, and anything else is a `TypeError`.
    fn typed_array_options_object(options: Option<&JsValue>) -> Result<Option<ObjectId>, JsError> {
        match options {
            None | Some(JsValue::Undefined) => Ok(None),
            Some(JsValue::Object(object)) => Ok(Some(*object)),
            Some(_) => Err(JsError::type_error(
                "options must be an object or undefined",
            )),
        }
    }

    fn base64_alphabet_option(
        &mut self,
        dom: &mut Dom,
        options: Option<ObjectId>,
    ) -> Result<Base64Alphabet, JsError> {
        let value = match options {
            Some(options) => self.get_member(dom, options, "alphabet")?,
            None => JsValue::Undefined,
        };
        match value {
            JsValue::Undefined => Ok(Base64Alphabet::Standard),
            JsValue::String(text) if text == "base64" => Ok(Base64Alphabet::Standard),
            JsValue::String(text) if text == "base64url" => Ok(Base64Alphabet::Url),
            _ => Err(JsError::type_error(
                "alphabet must be \"base64\" or \"base64url\"",
            )),
        }
    }

    fn base64_last_chunk_option(
        &mut self,
        dom: &mut Dom,
        options: Option<ObjectId>,
    ) -> Result<Base64LastChunkHandling, JsError> {
        let value = match options {
            Some(options) => self.get_member(dom, options, "lastChunkHandling")?,
            None => JsValue::Undefined,
        };
        match value {
            JsValue::Undefined => Ok(Base64LastChunkHandling::Loose),
            JsValue::String(text) if text == "loose" => Ok(Base64LastChunkHandling::Loose),
            JsValue::String(text) if text == "strict" => Ok(Base64LastChunkHandling::Strict),
            JsValue::String(text) if text == "stop-before-partial" => {
                Ok(Base64LastChunkHandling::StopBeforePartial)
            }
            _ => Err(JsError::type_error(
                "lastChunkHandling must be \"loose\", \"strict\", or \"stop-before-partial\"",
            )),
        }
    }

    /// `ValidateUint8Array`: the receiver must carry the `Uint8Array` internal
    /// slot names. Unlike `ValidateTypedArray`, a detached buffer still passes;
    /// the byte reads and writes perform their own witness check.
    fn validate_uint8_array_receiver(&self, receiver: ObjectId) -> Result<(), JsError> {
        let (kind, _, _, _) = self.typed_array_parts(receiver)?;
        if kind != TypedArrayKind::Uint8 {
            return Err(JsError::type_error("receiver is not a Uint8Array"));
        }
        Ok(())
    }

    /// The receiver check of the two `setFrom*` methods: a `Uint8Array` whose
    /// backing buffer is mutable. The immutable check lives here, before either
    /// argument is read, because those methods write in place.
    fn validate_writable_uint8_array_receiver(&self, receiver: ObjectId) -> Result<(), JsError> {
        self.validate_uint8_array_receiver(receiver)?;
        let (_, buffer, _, _) = self.typed_array_parts(receiver)?;
        buffer.ensure_mutable()
    }

    fn create_uint8_array_from_bytes(&mut self, bytes: Vec<u8>) -> Result<JsValue, JsError> {
        let length = bytes.len();
        if length > Self::MAX_TYPED_ARRAY_ELEMENTS {
            return Err(self.range_error("typed array length exceeds the engine bound"));
        }
        let prototype = self.uint8_array_intrinsic_prototype();
        self.ensure_heap_capacity(1)?;
        Ok(JsValue::Object(self.realm.typed_array(
            TypedArrayKind::Uint8,
            TypedBuffer::new(bytes),
            0,
            Some(length),
            prototype,
        )))
    }

    fn base64_result_record(&mut self, read: usize, written: usize) -> JsValue {
        let result = self.realm.create_ordinary_object();
        self.realm
            .set_property(result, "read".to_owned(), JsValue::Number(read as f64));
        self.realm.set_property(
            result,
            "written".to_owned(),
            JsValue::Number(written as f64),
        );
        JsValue::Object(result)
    }

    fn uint8_array_from_base64(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let Some(JsValue::String(text)) = arguments.first() else {
            return Err(JsError::type_error(
                "Uint8Array.fromBase64 requires a string",
            ));
        };
        let text = text.clone();
        let options = Self::typed_array_options_object(arguments.get(1))?;
        let alphabet = self.base64_alphabet_option(dom, options)?;
        let last_chunk = self.base64_last_chunk_option(dom, options)?;
        let (_, bytes, error) = decode_base64(text.units(), alphabet, last_chunk, usize::MAX);
        if let Some(error) = error {
            return Err(error);
        }
        self.create_uint8_array_from_bytes(bytes)
    }

    fn uint8_array_from_hex(&mut self, arguments: &[JsValue]) -> Result<JsValue, JsError> {
        let Some(JsValue::String(text)) = arguments.first() else {
            return Err(JsError::type_error("Uint8Array.fromHex requires a string"));
        };
        let (_, bytes, error) = decode_hex(text.units(), usize::MAX);
        if let Some(error) = error {
            return Err(error);
        }
        self.create_uint8_array_from_bytes(bytes)
    }

    fn uint8_array_set_from_base64(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        self.validate_writable_uint8_array_receiver(receiver)?;
        let Some(JsValue::String(text)) = arguments.first() else {
            return Err(JsError::type_error(
                "Uint8Array.prototype.setFromBase64 requires a string",
            ));
        };
        let text = text.clone();
        let options = Self::typed_array_options_object(arguments.get(1))?;
        let alphabet = self.base64_alphabet_option(dom, options)?;
        let last_chunk = self.base64_last_chunk_option(dom, options)?;
        let (_, buffer, start, length) = self.typed_array_host(receiver)?;
        let (read, bytes, error) = decode_base64(text.units(), alphabet, last_chunk, length);
        let written = bytes.len();
        buffer.write_bytes(start, &bytes);
        if let Some(error) = error {
            return Err(error);
        }
        Ok(self.base64_result_record(read, written))
    }

    fn uint8_array_set_from_hex(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        self.validate_writable_uint8_array_receiver(receiver)?;
        let Some(JsValue::String(text)) = arguments.first() else {
            return Err(JsError::type_error(
                "Uint8Array.prototype.setFromHex requires a string",
            ));
        };
        let (_, buffer, start, length) = self.typed_array_host(receiver)?;
        let (read, bytes, error) = decode_hex(text.units(), length);
        let written = bytes.len();
        buffer.write_bytes(start, &bytes);
        if let Some(error) = error {
            return Err(error);
        }
        Ok(self.base64_result_record(read, written))
    }

    fn uint8_array_to_base64(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        self.validate_uint8_array_receiver(receiver)?;
        let options = Self::typed_array_options_object(arguments.first())?;
        let alphabet = self.base64_alphabet_option(dom, options)?;
        let omit_padding = match options {
            Some(options) => self.get_member(dom, options, "omitPadding")?.is_truthy(),
            None => false,
        };
        let (_, buffer, start, length) = self.typed_array_host(receiver)?;
        let bytes = buffer.read_bytes(start, length)?;
        Ok(JsValue::String(encode_base64(
            &bytes,
            alphabet,
            !omit_padding,
        )))
    }

    fn uint8_array_to_hex(&self, receiver: ObjectId) -> Result<JsValue, JsError> {
        self.validate_uint8_array_receiver(receiver)?;
        let (_, buffer, start, length) = self.typed_array_host(receiver)?;
        let bytes = buffer.read_bytes(start, length)?;
        Ok(JsValue::String(encode_hex(&bytes)))
    }
}

fn base64_syntax_error() -> JsError {
    JsError::new(JsErrorKind::Syntax, "invalid base64 string", None)
}

fn hex_syntax_error() -> JsError {
    JsError::new(JsErrorKind::Syntax, "invalid hex string", None)
}

fn skip_ascii_whitespace(input: &[u16], mut index: usize) -> usize {
    while let Some(&unit) = input.get(index) {
        if !matches!(unit, 0x0009 | 0x000A | 0x000C | 0x000D | 0x0020) {
            break;
        }
        index += 1;
    }
    index
}

/// A code unit of the standard base64 alphabet as its six-bit value.
fn standard_base64_value(unit: u16) -> Option<u8> {
    match unit {
        0x41..=0x5A => Some((unit - 0x41) as u8),
        0x61..=0x7A => Some((unit - 0x61 + 26) as u8),
        0x30..=0x39 => Some((unit - 0x30 + 52) as u8),
        0x2B => Some(62),
        0x2F => Some(63),
        _ => None,
    }
}

/// `DecodeBase64Chunk`: `chunk` has two, three or four standard alphabet
/// characters. Two- and three-character chunks are padded with `A` and, when
/// `throw_on_extra_bits` is set, their unused trailing bits must be zero.
fn decode_base64_chunk(chunk: &[u16], throw_on_extra_bits: bool) -> Result<Vec<u8>, JsError> {
    let chunk_length = chunk.len();
    let mut padded = [0x41u16; 4];
    padded[..chunk_length].copy_from_slice(chunk);
    let mut values = [0u8; 4];
    for (index, unit) in padded.iter().enumerate() {
        values[index] = standard_base64_value(*unit).ok_or_else(base64_syntax_error)?;
    }
    let bytes = [
        (values[0] << 2) | (values[1] >> 4),
        ((values[1] & 0x0F) << 4) | (values[2] >> 2),
        ((values[2] & 0x03) << 6) | values[3],
    ];
    match chunk_length {
        2 => {
            if throw_on_extra_bits && bytes[1] != 0 {
                return Err(base64_syntax_error());
            }
            Ok(vec![bytes[0]])
        }
        3 => {
            if throw_on_extra_bits && bytes[2] != 0 {
                return Err(base64_syntax_error());
            }
            Ok(vec![bytes[0], bytes[1]])
        }
        _ => Ok(bytes.to_vec()),
    }
}

/// `FromBase64`: the decoded prefix, the input position reached, and a
/// `SyntaxError` when the input is invalid. The prefix is still returned with
/// an error so `setFromBase64` can write it before throwing.
fn decode_base64(
    input: &[u16],
    alphabet: Base64Alphabet,
    last_chunk_handling: Base64LastChunkHandling,
    max_length: usize,
) -> (usize, Vec<u8>, Option<JsError>) {
    if max_length == 0 {
        return (0, Vec::new(), None);
    }
    let length = input.len();
    let mut read = 0;
    let mut bytes = Vec::new();
    let mut chunk: Vec<u16> = Vec::with_capacity(4);
    let mut index = 0;
    loop {
        index = skip_ascii_whitespace(input, index);
        if index == length {
            if !chunk.is_empty() {
                match last_chunk_handling {
                    Base64LastChunkHandling::StopBeforePartial => {
                        return (read, bytes, None);
                    }
                    Base64LastChunkHandling::Loose => {
                        if chunk.len() == 1 {
                            return (read, bytes, Some(base64_syntax_error()));
                        }
                        match decode_base64_chunk(&chunk, false) {
                            Ok(decoded) => bytes.extend(decoded),
                            Err(error) => return (read, bytes, Some(error)),
                        }
                    }
                    Base64LastChunkHandling::Strict => {
                        return (read, bytes, Some(base64_syntax_error()));
                    }
                }
            }
            return (length, bytes, None);
        }
        let mut character = input[index];
        index += 1;
        if character == u16::from(b'=') {
            if chunk.len() < 2 {
                return (read, bytes, Some(base64_syntax_error()));
            }
            index = skip_ascii_whitespace(input, index);
            if chunk.len() == 2 {
                if index == length {
                    if last_chunk_handling == Base64LastChunkHandling::StopBeforePartial {
                        return (read, bytes, None);
                    }
                    return (read, bytes, Some(base64_syntax_error()));
                }
                // The second `=` is inspected but not consumed here; the
                // whitespace skip below advances past it.
                if input[index] != u16::from(b'=') {
                    return (read, bytes, Some(base64_syntax_error()));
                }
                index = skip_ascii_whitespace(input, index + 1);
            }
            if index < length {
                return (read, bytes, Some(base64_syntax_error()));
            }
            let throw_on_extra_bits = last_chunk_handling == Base64LastChunkHandling::Strict;
            match decode_base64_chunk(&chunk, throw_on_extra_bits) {
                Ok(decoded) => bytes.extend(decoded),
                Err(error) => return (read, bytes, Some(error)),
            }
            return (length, bytes, None);
        }
        if alphabet == Base64Alphabet::Url {
            if character == u16::from(b'+') || character == u16::from(b'/') {
                return (read, bytes, Some(base64_syntax_error()));
            }
            if character == u16::from(b'-') {
                character = u16::from(b'+');
            } else if character == u16::from(b'_') {
                character = u16::from(b'/');
            }
        }
        if standard_base64_value(character).is_none() {
            return (read, bytes, Some(base64_syntax_error()));
        }
        let remaining = max_length - bytes.len();
        if (remaining == 1 && chunk.len() == 2) || (remaining == 2 && chunk.len() == 3) {
            return (read, bytes, None);
        }
        chunk.push(character);
        if chunk.len() == 4 {
            match decode_base64_chunk(&chunk, false) {
                Ok(decoded) => bytes.extend(decoded),
                Err(error) => return (read, bytes, Some(error)),
            }
            chunk.clear();
            read = index;
            if bytes.len() == max_length {
                return (read, bytes, None);
            }
        }
    }
}

/// `FromHex`: unlike base64 there is no whitespace skipping, and an odd input
/// length is rejected before any pair is decoded.
fn decode_hex(input: &[u16], max_length: usize) -> (usize, Vec<u8>, Option<JsError>) {
    let length = input.len();
    let mut read = 0;
    let mut bytes = Vec::new();
    if !length.is_multiple_of(2) {
        return (0, bytes, Some(hex_syntax_error()));
    }
    while read < length && bytes.len() < max_length {
        let Some(high) = hex_value(input[read]) else {
            return (read, bytes, Some(hex_syntax_error()));
        };
        let Some(low) = hex_value(input[read + 1]) else {
            return (read, bytes, Some(hex_syntax_error()));
        };
        read += 2;
        bytes.push((high << 4) | low);
    }
    (read, bytes, None)
}

fn hex_value(unit: u16) -> Option<u8> {
    match unit {
        0x30..=0x39 => Some((unit - 0x30) as u8),
        0x41..=0x46 => Some((unit - 0x41 + 10) as u8),
        0x61..=0x66 => Some((unit - 0x61 + 10) as u8),
        _ => None,
    }
}

fn encode_base64(bytes: &[u8], alphabet: Base64Alphabet, padding: bool) -> JsString {
    const STANDARD: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    const URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let table = match alphabet {
        Base64Alphabet::Standard => STANDARD,
        Base64Alphabet::Url => URL,
    };
    let mut units = Vec::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = u32::from(chunk[0]);
        let second = chunk.get(1).copied().map_or(0, u32::from);
        let third = chunk.get(2).copied().map_or(0, u32::from);
        let packed = (first << 16) | (second << 8) | third;
        units.push(u16::from(table[((packed >> 18) & 0x3F) as usize]));
        units.push(u16::from(table[((packed >> 12) & 0x3F) as usize]));
        if chunk.len() > 1 {
            units.push(u16::from(table[((packed >> 6) & 0x3F) as usize]));
        } else if padding {
            units.push(u16::from(b'='));
        }
        if chunk.len() > 2 {
            units.push(u16::from(table[(packed & 0x3F) as usize]));
        } else if padding {
            units.push(u16::from(b'='));
        }
    }
    JsString::from_unit_vec(units)
}

fn encode_hex(bytes: &[u8]) -> JsString {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut units = Vec::with_capacity(bytes.len() * 2);
    for byte in bytes {
        units.push(u16::from(HEX[usize::from(*byte >> 4)]));
        units.push(u16::from(HEX[usize::from(*byte & 0x0F)]));
    }
    JsString::from_unit_vec(units)
}

/// Store `values`, each already converted to `kind`, from element `first` on.
fn store_typed_elements(
    buffer: &TypedBuffer,
    kind: TypedArrayKind,
    first: usize,
    values: &[JsValue],
) {
    for (offset, value) in values.iter().enumerate() {
        buffer.set_converted_element(kind, first + offset, value);
    }
}

/// The signed order of two Numbers for the default `sort`: `NaN` last, and `-0`
/// before `+0`.
fn number_order(left: f64, right: f64) -> f64 {
    match (left.is_nan(), right.is_nan()) {
        (true, true) => 0.0,
        (true, false) => 1.0,
        (false, true) => -1.0,
        (false, false) => {
            if left < right {
                -1.0
            } else if left > right {
                1.0
            } else if left == 0.0 && right == 0.0 {
                // `-0` sorts before `+0`: a negative `left` with a positive
                // `right` is -1, and the reverse is +1.
                f64::from(u8::from(left.is_sign_positive()))
                    - f64::from(u8::from(right.is_sign_positive()))
            } else {
                0.0
            }
        }
    }
}
