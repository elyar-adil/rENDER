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
use crate::JsError;
use crate::JsSymbol;
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::runtime::convert::required_argument;
use crate::value::ArrayView;
use crate::value::NativeFunction;
use crate::value::ObjectHost;
use crate::value::TypedArrayKind;
use crate::value::TypedBuffer;
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
            NativeFunction::TypedArrayJoin => self.typed_array_join(receiver, arguments),
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
            return self.typed_array_over_buffer(
                dom,
                kind,
                buffer,
                arguments.get(1),
                arguments.get(2),
                prototype,
            );
        }
        let elements = match first {
            JsValue::Undefined | JsValue::Null => Vec::new(),
            JsValue::Object(_) => self.typed_array_source_values(dom, kind, first)?,
            other => {
                let length = self.typed_index(dom, other)?;
                return self.create_typed_array(kind, length, prototype);
            }
        };
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

    /// `TypedArray.from(source[, mapper[, thisArg]])` (ECMA-262 23.2.2.1): build
    /// one typed array from another typed array, an array-like, or a string's
    /// characters, with an optional mapping callback.
    pub(in crate::runtime) fn typed_array_from(
        &mut self,
        dom: &mut Dom,
        constructor: ObjectId,
        kind: TypedArrayKind,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let source = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let mut values = match &source {
            JsValue::Object(_) => self.typed_array_source_values(dom, kind, &source)?,
            // The string's code points are iterated as strings, and each one is
            // converted like any other element (23.2.2.1 steps 5 and 6).
            JsValue::String(text) => {
                let mut converted = Vec::new();
                for character in text.chars() {
                    let character = JsValue::String(character.to_string());
                    converted.push(self.typed_element_value(dom, kind, &character)?);
                }
                converted
            }
            _ => {
                return Err(JsError::type_error(
                    "TypedArray.from requires an array-like or iterable source",
                ));
            }
        };
        if let Some(JsValue::Object(mapper)) = arguments.get(1) {
            let mapper = Self::require_callable_object(&JsValue::Object(*mapper), &self.realm)?;
            let this_argument = arguments.get(2).cloned().unwrap_or(JsValue::Undefined);
            for (index, value) in values.iter_mut().enumerate() {
                let index_value = JsValue::Number(index as f64);
                let element = self.call_with_this(
                    dom,
                    mapper,
                    &[value.clone(), index_value],
                    this_argument.clone(),
                )?;
                *value = self.typed_element_value(dom, kind, &element)?;
            }
        }
        let prototype = self
            .realm
            .get_property(constructor, "prototype")
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            });
        self.create_typed_array_from_elements(kind, &values, prototype)
    }

    /// `TypedArray.of(...items)` (ECMA-262 23.2.2.2), for a concrete constructor.
    fn typed_array_of(
        &mut self,
        dom: &mut Dom,
        constructor: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let kind = match self.realm.host(constructor) {
            Some(ObjectHost::TypedArrayConstructor(kind)) => kind,
            _ => {
                return Err(JsError::type_error(
                    "TypedArray.of requires a typed-array constructor receiver",
                ));
            }
        };
        let mut values = Vec::with_capacity(arguments.len());
        for argument in arguments {
            values.push(self.typed_element_value(dom, kind, argument)?);
        }
        let prototype = self
            .realm
            .get_property(constructor, "prototype")
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            });
        self.create_typed_array_from_elements(kind, &values, prototype)
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
        let (kind, buffer, start, length) = self.typed_array_host(receiver)?;
        let source = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let offset = self.optional_integer_value(dom, arguments.get(1))?;
        if offset < 0.0 {
            return Err(self.range_error("offset is out of bounds"));
        }
        let values: Vec<JsValue> = match &source {
            JsValue::Object(object) => {
                if let Some(ObjectHost::TypedArray { .. }) = self.realm.host(*object) {
                    let (source_kind, source_buffer, source_start, source_length) =
                        self.typed_array_host(*object)?;
                    if source_kind.is_bigint() != kind.is_bigint() {
                        return Err(JsError::type_error(
                            "cannot mix BigInt and other types in typed array copies",
                        ));
                    }
                    // The source is read in full before any write, so a source
                    // over the same buffer copies correctly.
                    source_buffer.elements_value(source_kind, source_start, source_length)?
                } else {
                    // SetTypedArrayFromArrayLike: the length is `ToLength` of the
                    // object's `length`, and each element converts to the kind.
                    let count = self.array_like_length(dom, *object)?;
                    let mut values = Vec::with_capacity(count);
                    for index in 0..count {
                        let element = self.get_member(dom, *object, &index.to_string())?;
                        values.push(self.typed_element_value(dom, kind, &element)?);
                    }
                    values
                }
            }
            // A string is an array-like of its characters, each converted to the
            // element type like any other value.
            JsValue::String(text) => {
                let mut values = Vec::new();
                for character in text.chars() {
                    let character = JsValue::String(character.to_string());
                    values.push(self.typed_element_value(dom, kind, &character)?);
                }
                values
            }
            JsValue::Undefined | JsValue::Null => {
                return Err(JsError::type_error(
                    "typed-array set requires an array-like source",
                ));
            }
            // Other primitives box to an object with no `length`, so there are
            // no elements to copy.
            _ => Vec::new(),
        };
        if offset + values.len() as f64 > length as f64 {
            return Err(self.range_error("source is too large"));
        }
        store_typed_elements(&buffer, kind, start + offset as usize, &values);
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
    ) -> Result<ObjectId, JsError> {
        // SpeciesConstructor (ECMA-262 7.3.22).
        let default = self
            .realm
            .global(kind.name())
            .and_then(|value| match value {
                JsValue::Object(constructor) => Some(constructor),
                _ => None,
            });
        let species = match self.get_member(dom, exemplar, "constructor")? {
            JsValue::Undefined => default,
            JsValue::Object(constructor) => {
                match self.get_symbol_value(dom, constructor, &JsSymbol::well_known("@@species"))? {
                    JsValue::Undefined | JsValue::Null => default,
                    JsValue::Object(species) if self.is_constructor(species) => Some(species),
                    _ => return Err(JsError::type_error("species is not a constructor")),
                }
            }
            _ => return Err(JsError::type_error("constructor is not an object")),
        };
        let Some(species) = species else {
            return Err(JsError::type_error("no default typed-array constructor"));
        };
        // TypedArrayCreateFromConstructor (ECMA-262 23.2.4.2).
        let JsValue::Object(result) = self.construct(dom, species, arguments)? else {
            return Err(JsError::type_error(
                "species constructor returned a primitive",
            ));
        };
        let (result_kind, _, _, length) = self.typed_array_host(result)?;
        if result_kind.is_bigint() != kind.is_bigint() {
            return Err(JsError::type_error(
                "cannot mix BigInt and other types in typed array copies",
            ));
        }
        if let [JsValue::Number(requested)] = arguments
            && (length as f64) < *requested
        {
            return Err(JsError::type_error(
                "species constructor returned a typed array that is too short",
            ));
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
        let (kind, buffer, start, length) = self.typed_array_parts(target)?;
        let converted = self.typed_element_value(dom, kind, value)?;
        if length.is_some_and(|length| index < length) {
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
        Ok(JsValue::Object(
            self.typed_array_species_create(dom, receiver, kind, &args)?,
        ))
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
        let count = end - begin;
        let result =
            self.typed_array_species_create(dom, receiver, kind, &[JsValue::Number(count as f64)])?;
        for offset in 0..count {
            let value = self.typed_array_get_index(receiver, begin + offset);
            self.typed_array_set_index(dom, result, offset, &value)?;
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
        let (kind, buffer, start, length) = self.typed_array_host(receiver)?;
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
        )?;
        for (index, value) in kept.iter().enumerate() {
            self.typed_array_set_index(dom, result, index, value)?;
        }
        Ok(JsValue::Object(result))
    }

    pub(in crate::runtime) fn typed_array_join(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let (kind, buffer, start, length) = self.typed_array_host(receiver)?;
        let separator = match arguments.first() {
            None | Some(JsValue::Undefined) => ",".to_owned(),
            Some(value) => value.to_js_string(),
        };
        let elements = buffer.elements_value(kind, start, length)?;
        let parts = elements
            .iter()
            .map(JsValue::to_js_string)
            .collect::<Vec<_>>();
        Ok(JsValue::String(parts.join(&separator)))
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
        let (kind, buffer, start, length) = self.typed_array_host(receiver)?;
        let len = length as f64;
        let to = self.relative_position(dom, arguments.first(), len, 0.0)?;
        let from = self.relative_position(dom, arguments.get(1), len, 0.0)?;
        let end = self.relative_position(dom, arguments.get(2), len, len)?;
        let count = (end - from).min(len - to);
        if count > 0.0 {
            let (to, from, count) = (to as usize, from as usize, count as usize);
            let mut elements = buffer.elements_value(kind, start, length)?;
            // The source range is copied out first, so overlapping ranges copy
            // as if through a temporary.
            let copied: Vec<JsValue> = elements[from..from + count].to_vec();
            for (offset, value) in copied.into_iter().enumerate() {
                elements[to + offset] = value;
            }
            store_typed_elements(&buffer, kind, start, &elements);
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
        let (kind, buffer, start, length) = self.typed_array_host(receiver)?;
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
        let (kind, buffer, start, _) = self.typed_array_host(receiver)?;
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
        if index < 0.0 || index >= len {
            return Err(self.range_error("Invalid typed array index"));
        }
        let mut elements = self.typed_array_elements(receiver)?;
        elements[index as usize] = number;
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
            Ok((kind, _, _, _)) => JsValue::String(kind.name().to_owned()),
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
