//! `DataView` and `ArrayBuffer` (ECMAScript 25.2.5, 25.1.5).
//!
//! `DataView` is the one buffer interface that reads and writes with an
//! explicit width and an explicit byte order, which is what binary formats
//! need. Both were absent while the whole `TypedArray` family was implemented,
//! so the area was inconsistent.
//!
//! **Endianness.** The `DataView` constructor takes three parameters - buffer,
//! byte offset, byte length - and has *no* endianness parameter, so every
//! accessor carries its own `littleEndian` and the default is **big-endian**.
//! That default is the detail worth stating because the mirror-image mistake
//! (a constructor-level `littleEndian`) is silently wrong rather than loud:
//! `new DataView(buf, 0, 4, true)` ignores its fourth argument in every
//! shipping engine, so a view built that way still reads big-endian unless each
//! call passes `true`.
//!
//! **Storage.** A buffer is one store of bytes that every view of it shares, so
//! `new Uint8Array(buffer)`, `new Float64Array(buffer)` and `new DataView(buffer)`
//! read and write the same bytes. Element conversion is `TypedArrayKind::load`
//! and `TypedArrayKind::store`, which the `DataView` accessors also use.

use super::array::MAX_SAFE_INTEGER;
use crate::JsBigInt;
use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::value::{NativeFunction, ObjectHost, TypedArrayKind, TypedBuffer};
use render_dom::Dom;

/// One accessor: the element width in bytes and the conversion to apply.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Access {
    Int8,
    Uint8,
    Int16,
    Uint16,
    Int32,
    Uint32,
    Float16,
    Float32,
    Float64,
    /// `getBigInt64` and `setBigInt64`: eight bytes read as a signed `BigInt`.
    BigInt64,
    /// `getBigUint64` and `setBigUint64`: eight bytes read as an unsigned `BigInt`.
    BigUint64,
}

impl Access {
    const fn width(self) -> usize {
        match self {
            Self::Int8 | Self::Uint8 => 1,
            Self::Int16 | Self::Uint16 | Self::Float16 => 2,
            Self::Int32 | Self::Uint32 | Self::Float32 => 4,
            Self::Float64 | Self::BigInt64 | Self::BigUint64 => 8,
        }
    }

    const fn is_bigint(self) -> bool {
        matches!(self, Self::BigInt64 | Self::BigUint64)
    }

    /// The typed-array element type this accessor converts with. `Float16` has
    /// no typed-array counterpart, so it converts through `f16_bits`.
    const fn kind(self) -> Option<TypedArrayKind> {
        Some(match self {
            Self::Int8 => TypedArrayKind::Int8,
            Self::Uint8 => TypedArrayKind::Uint8,
            Self::Int16 => TypedArrayKind::Int16,
            Self::Uint16 => TypedArrayKind::Uint16,
            Self::Int32 => TypedArrayKind::Int32,
            Self::Uint32 => TypedArrayKind::Uint32,
            Self::Float32 => TypedArrayKind::Float32,
            Self::Float64 => TypedArrayKind::Float64,
            Self::Float16 | Self::BigInt64 | Self::BigUint64 => return None,
        })
    }

    fn from_native(function: NativeFunction) -> Option<Self> {
        Some(match function {
            NativeFunction::DataViewGetInt8 | NativeFunction::DataViewSetInt8 => Self::Int8,
            NativeFunction::DataViewGetUint8 | NativeFunction::DataViewSetUint8 => Self::Uint8,
            NativeFunction::DataViewGetInt16 | NativeFunction::DataViewSetInt16 => Self::Int16,
            NativeFunction::DataViewGetUint16 | NativeFunction::DataViewSetUint16 => Self::Uint16,
            NativeFunction::DataViewGetInt32 | NativeFunction::DataViewSetInt32 => Self::Int32,
            NativeFunction::DataViewGetUint32 | NativeFunction::DataViewSetUint32 => Self::Uint32,
            NativeFunction::DataViewGetFloat16 | NativeFunction::DataViewSetFloat16 => {
                Self::Float16
            }
            NativeFunction::DataViewGetFloat32 | NativeFunction::DataViewSetFloat32 => {
                Self::Float32
            }
            NativeFunction::DataViewGetFloat64 | NativeFunction::DataViewSetFloat64 => {
                Self::Float64
            }
            NativeFunction::DataViewGetBigInt64 | NativeFunction::DataViewSetBigInt64 => {
                Self::BigInt64
            }
            NativeFunction::DataViewGetBigUint64 | NativeFunction::DataViewSetBigUint64 => {
                Self::BigUint64
            }
            _ => return None,
        })
    }
}

/// Whether a `DataView` native is a setter.
fn is_write(function: NativeFunction) -> bool {
    matches!(
        function,
        NativeFunction::DataViewSetInt8
            | NativeFunction::DataViewSetUint8
            | NativeFunction::DataViewSetInt16
            | NativeFunction::DataViewSetUint16
            | NativeFunction::DataViewSetInt32
            | NativeFunction::DataViewSetUint32
            | NativeFunction::DataViewSetFloat16
            | NativeFunction::DataViewSetFloat32
            | NativeFunction::DataViewSetFloat64
            | NativeFunction::DataViewSetBigInt64
            | NativeFunction::DataViewSetBigUint64
    )
}

/// The eight bytes that store a `BigInt` element: the value modulo 2^64, which
/// is `ToBigInt64` and `ToBigUint64` (ECMA-262 7.1.15, 7.1.16), reversed for a
/// big-endian accessor.
fn encode_bigint_bytes(value: &JsBigInt, big_endian: bool) -> Vec<u8> {
    let modulo = value
        .as_uint_n(64)
        .and_then(|wrapped| wrapped.magnitude_u64())
        .unwrap_or(0);
    let mut bytes = modulo.to_le_bytes().to_vec();
    if big_endian {
        bytes.reverse();
    }
    bytes
}

/// The `BigInt` an eight-byte element reads as, given in little-endian order.
fn decode_bigint(access: Access, bytes: &[u8]) -> JsBigInt {
    let mut raw = [0_u8; 8];
    raw.copy_from_slice(&bytes[..8]);
    match access {
        Access::BigInt64 => JsBigInt::from_i64(i64::from_le_bytes(raw)),
        _ => JsBigInt::from_u64(u64::from_le_bytes(raw)),
    }
}

/// The `width` bytes that store `value` for this accessor, in the requested
/// order. An element is little-endian in the buffer, so a big-endian accessor
/// reverses those bytes.
fn encode_bytes(access: Access, value: f64, big_endian: bool) -> Vec<u8> {
    let mut bytes = vec![0_u8; access.width()];
    match access.kind() {
        Some(kind) => kind.store(value, &mut bytes),
        None => bytes.copy_from_slice(&f16_bits(value).to_le_bytes()),
    }
    if big_endian {
        bytes.reverse();
    }
    bytes
}

/// The `Number` a getter returns for an element's bytes, given in little-endian
/// order. A float accessor reinterprets the bits; it does not convert them.
fn decode_number(access: Access, bytes: &[u8]) -> f64 {
    match access.kind() {
        Some(kind) => kind.load(bytes),
        None => f16_value(u16::from_le_bytes([bytes[0], bytes[1]])),
    }
}

/// Round a number to the nearest IEEE 754 binary16 value, ties to even, and
/// return its 16-bit pattern (ECMA-262 `Float16Round`). A magnitude from 65520
/// up is past the halfway point to 2^16, so it rounds to infinity.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "every value here is a small integer checked against the binary16 ranges"
)]
fn f16_bits(value: f64) -> u16 {
    if value.is_nan() {
        return 0x7E00;
    }
    let sign: u16 = if value.is_sign_negative() { 0x8000 } else { 0 };
    let magnitude = value.abs();
    if magnitude >= 65520.0 {
        return sign | 0x7C00;
    }
    if magnitude < 2f64.powi(-14) {
        // Subnormal: the quantum is 2^-24. A mantissa that rounds up to 1024
        // is the smallest normal, and that is exactly the bit pattern 1024.
        let mantissa = (magnitude * 2f64.powi(24)).round_ties_even();
        return sign | mantissa as u16;
    }
    // Normal: the exponent is the binary64 one, and the 11-bit significand
    // rounds to ten stored bits.
    let exponent = ((magnitude.to_bits() >> 52) & 0x7FF) as i32 - 1023;
    let scaled = magnitude * 2f64.powi(10 - exponent);
    let mut significand = scaled.round_ties_even() as u32;
    let mut biased = exponent + 15;
    if significand == 2048 {
        significand = 1024;
        biased += 1;
    }
    sign | ((biased as u16) << 10) | ((significand - 1024) as u16)
}

/// The number a binary16 bit pattern denotes.
fn f16_value(bits: u16) -> f64 {
    let sign = if bits & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exponent = i32::from((bits >> 10) & 0x1F);
    let significand = f64::from(bits & 0x3FF);
    let magnitude = match exponent {
        0 => significand * 2f64.powi(-24),
        31 if significand == 0.0 => f64::INFINITY,
        31 => f64::NAN,
        _ => (1024.0 + significand) * 2f64.powi(exponent - 25),
    };
    sign * magnitude
}

/// A byte or element count as a JavaScript Number. Buffer and view sizes are
/// bounded by the engine far below the 2^53 precision limit.
#[allow(
    clippy::cast_precision_loss,
    reason = "buffer and view sizes stay far below any precision boundary"
)]
pub(super) fn number_value(count: usize) -> JsValue {
    JsValue::Number(count as f64)
}

/// A validated `ToIndex` result as a `usize`. `to_index_value` has already
/// bounded it to `0..=2^53-1`, which fits a `usize` on every target.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the caller passes a value that to_index_value has bounded to a non-negative integer"
)]
fn usize_from(index: f64) -> usize {
    index as usize
}

/// The length of a `DataView` that must be in bounds: `IsViewOutOfBounds` is a
/// `TypeError` for the accessors that read or write through it.
fn view_in_bounds(
    buffer: &TypedBuffer,
    byte_offset: usize,
    byte_length: Option<usize>,
) -> Result<usize, JsError> {
    buffer
        .view_length(1, byte_offset, byte_length)
        .ok_or_else(|| JsError::type_error("DataView is out of bounds of its ArrayBuffer"))
}

impl JsRuntime {
    /// The `ArrayBuffer` store of `receiver`, or a `TypeError` naming the
    /// accessor that was called on something else.
    fn array_buffer_host(&self, receiver: ObjectId, member: &str) -> Result<TypedBuffer, JsError> {
        match self.realm.host(receiver) {
            Some(ObjectHost::ArrayBufferHost(buffer)) => Ok(buffer),
            _ => Err(JsError::type_error(format!(
                "ArrayBuffer.prototype.{member} called on an incompatible receiver"
            ))),
        }
    }

    pub(in crate::runtime) fn dispatch_data_view_native(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match function {
            NativeFunction::ArrayBufferSlice => self.array_buffer_slice(dom, receiver, arguments),
            NativeFunction::ArrayBufferIsView => Ok(JsValue::Boolean(
                self.is_array_buffer_view(arguments.first()),
            )),
            NativeFunction::ArrayBufferByteLengthGetter => self.array_buffer_byte_length(receiver),
            NativeFunction::ArrayBufferResizableGetter => {
                let buffer = self.array_buffer_host(receiver, "resizable")?;
                Ok(JsValue::Boolean(buffer.max_byte_length().is_some()))
            }
            NativeFunction::ArrayBufferMaxByteLengthGetter => {
                let buffer = self.array_buffer_host(receiver, "maxByteLength")?;
                let length = if buffer.is_detached() {
                    0
                } else {
                    buffer
                        .max_byte_length()
                        .unwrap_or_else(|| buffer.byte_length())
                };
                Ok(number_value(length))
            }
            NativeFunction::ArrayBufferDetachedGetter => {
                let buffer = self.array_buffer_host(receiver, "detached")?;
                Ok(JsValue::Boolean(buffer.is_detached()))
            }
            NativeFunction::ArrayBufferResize => self.array_buffer_resize(dom, receiver, arguments),
            NativeFunction::ArrayBufferTransfer => {
                self.array_buffer_transfer(dom, receiver, arguments, true)
            }
            NativeFunction::ArrayBufferTransferToFixedLength => {
                self.array_buffer_transfer(dom, receiver, arguments, false)
            }
            NativeFunction::DataViewBufferGetter => self.data_view_buffer(receiver),
            NativeFunction::DataViewByteLengthGetter => {
                let (buffer, byte_offset, byte_length) = self.data_view_host(receiver)?;
                let length = view_in_bounds(&buffer, byte_offset, byte_length)?;
                Ok(number_value(length))
            }
            NativeFunction::DataViewByteOffsetGetter => {
                let (buffer, byte_offset, byte_length) = self.data_view_host(receiver)?;
                view_in_bounds(&buffer, byte_offset, byte_length)?;
                Ok(number_value(byte_offset))
            }
            _ => self.data_view_access(dom, function, receiver, arguments),
        }
    }

    /// ECMA-262 `ToIndex`: `ToIntegerOrInfinity` of the value, through its
    /// `valueOf` when it is an object, and a `RangeError` for anything outside
    /// `0..=2^53-1`. An absent or `undefined` value reads as 0.
    fn to_index_value(&mut self, dom: &mut Dom, value: Option<&JsValue>) -> Result<f64, JsError> {
        let integer = match value {
            None | Some(JsValue::Undefined) => 0.0,
            Some(value) => self.to_integer_value(dom, value)?,
        };
        if !(0.0..=MAX_SAFE_INTEGER).contains(&integer) {
            return Err(self.range_error("index is outside the supported range"));
        }
        Ok(integer)
    }

    /// `new ArrayBuffer(byteLength[, options])`: one byte per slot, so a buffer
    /// is byte-granular and every view over it is byte-exact. A `maxByteLength`
    /// option makes the buffer resizable (ECMA-262 25.1.4.1).
    pub(in crate::runtime) fn array_buffer_constructor(
        &mut self,
        dom: &mut Dom,
        constructor: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        // §25.1.4.1 steps 2-3: `ToIndex(length)`, then the options.
        let length = usize_from(self.to_index_value(dom, arguments.first())?);
        let max_byte_length = self.array_buffer_max_option(dom, arguments.get(1))?;
        // §25.1.3.1 AllocateArrayBuffer: a length past the maximum is refused
        // before the object is created.
        if max_byte_length.is_some_and(|max| length > max) {
            return Err(self.range_error("ArrayBuffer length exceeds maxByteLength"));
        }
        let prototype = self
            .realm
            .get_property(constructor, "prototype")
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            });
        // CreateByteDataBlock runs after the object exists (§25.1.3.1 step 5),
        // and a size the engine cannot hold is a RangeError.
        if length > Self::MAX_TYPED_ARRAY_ELEMENTS
            || max_byte_length.is_some_and(|max| max > Self::MAX_TYPED_ARRAY_ELEMENTS)
        {
            return Err(self.range_error("ArrayBuffer size exceeds the engine bound"));
        }
        let bytes = vec![0; length];
        let buffer = match max_byte_length {
            Some(max) => TypedBuffer::new_resizable(bytes, max),
            None => TypedBuffer::new(bytes),
        };
        let object = self.new_array_buffer(prototype, &buffer)?;
        Ok(JsValue::Object(object))
    }

    /// §25.1.3.1 `GetArrayBufferMaxByteLengthOption`: the `maxByteLength` of an
    /// options object, through `ToIndex`, or `None` when there is none.
    fn array_buffer_max_option(
        &mut self,
        dom: &mut Dom,
        options: Option<&JsValue>,
    ) -> Result<Option<usize>, JsError> {
        let Some(JsValue::Object(options)) = options else {
            return Ok(None);
        };
        let max = self.get_member(dom, *options, "maxByteLength")?;
        if matches!(max, JsValue::Undefined) {
            return Ok(None);
        }
        let max = self.to_index_value(dom, Some(&max))?;
        Ok(Some(usize_from(max)))
    }

    /// `ArrayBuffer.prototype.resize(newLength)` (ECMA-262 25.1.6.6). Only a
    /// resizable buffer has the slot, so anything else is a `TypeError` before
    /// the argument is converted.
    fn array_buffer_resize(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let buffer = self.array_buffer_host(receiver, "resize")?;
        let Some(max) = buffer.max_byte_length() else {
            return Err(JsError::type_error(
                "ArrayBuffer.prototype.resize called on a fixed-length buffer",
            ));
        };
        let new_length = usize_from(self.to_index_value(dom, arguments.first())?);
        buffer.ensure_attached()?;
        if new_length > max || new_length > Self::MAX_TYPED_ARRAY_ELEMENTS {
            return Err(self.range_error("new length is outside the buffer's maxByteLength"));
        }
        buffer.resize(new_length);
        Ok(JsValue::Undefined)
    }

    /// `ArrayBuffer.prototype.transfer` and `transferToFixedLength`
    /// (ECMA-262 25.1.3.3 `ArrayBufferCopyAndDetach`): a new buffer holds the
    /// bytes, resized to `newLength`, and the receiver is detached. Only
    /// `transfer` keeps a resizable receiver resizable.
    fn array_buffer_transfer(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
        preserve_resizability: bool,
    ) -> Result<JsValue, JsError> {
        let member = if preserve_resizability {
            "transfer"
        } else {
            "transferToFixedLength"
        };
        let buffer = self.array_buffer_host(receiver, member)?;
        let new_length = match arguments.first() {
            None | Some(JsValue::Undefined) => buffer.byte_length(),
            Some(value) => usize_from(self.to_index_value(dom, Some(value))?),
        };
        buffer.ensure_attached()?;
        let max = if preserve_resizability {
            buffer.max_byte_length()
        } else {
            None
        };
        if max.is_some_and(|max| new_length > max) || new_length > Self::MAX_TYPED_ARRAY_ELEMENTS {
            return Err(self.range_error("new length is outside the buffer's maxByteLength"));
        }
        // The new buffer's object exists before the old one is detached, so an
        // allocation failure leaves the receiver intact.
        let prototype = self.array_buffer_prototype();
        self.ensure_heap_capacity(1)?;
        let mut bytes = buffer.bytes();
        bytes.resize(new_length, 0);
        let transferred = match max {
            Some(max) => TypedBuffer::new_resizable(bytes, max),
            None => TypedBuffer::new(bytes),
        };
        buffer.detach();
        let object = self.new_array_buffer(prototype, &transferred)?;
        Ok(JsValue::Object(object))
    }

    /// A new `ArrayBuffer` object over `buffer`. The object becomes the store's
    /// identity, so every view of the buffer reports this object as its `buffer`.
    fn new_array_buffer(
        &mut self,
        prototype: Option<ObjectId>,
        buffer: &TypedBuffer,
    ) -> Result<ObjectId, JsError> {
        self.ensure_heap_capacity(1)?;
        let object = self.realm.create_object(prototype);
        if let Some(host) = self.realm.host_mut(object) {
            *host = ObjectHost::ArrayBufferHost(buffer.clone());
        }
        buffer.identify(object);
        Ok(object)
    }

    /// The `ArrayBuffer` object of `buffer`. A store that a typed array made
    /// internally has no object until something asks for its `buffer`, so one is
    /// created then, and every later request returns that same object.
    pub(in crate::runtime) fn array_buffer_object(
        &mut self,
        buffer: &TypedBuffer,
    ) -> Result<ObjectId, JsError> {
        if let Some(object) = buffer.object() {
            return Ok(object);
        }
        let prototype = self.array_buffer_prototype();
        self.new_array_buffer(prototype, buffer)
    }

    /// `%ArrayBuffer.prototype%`, read from the global `ArrayBuffer`.
    fn array_buffer_prototype(&self) -> Option<ObjectId> {
        self.realm
            .global("ArrayBuffer")
            .and_then(|value| match value {
                JsValue::Object(constructor) => self.realm.get_property(constructor, "prototype"),
                _ => None,
            })
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            })
    }

    /// `DetachArrayBuffer` (ECMA-262 25.1.3.5) for an embedder, such as the
    /// test262 host's `$262.detachArrayBuffer`. Anything but an `ArrayBuffer` is a
    /// `TypeError`.
    ///
    /// # Errors
    ///
    /// Returns a `TypeError` when `value` is not an `ArrayBuffer`.
    pub fn detach_array_buffer(&mut self, value: &JsValue) -> Result<(), JsError> {
        match value {
            JsValue::Object(object) => match self.realm.host(*object) {
                Some(ObjectHost::ArrayBufferHost(buffer)) => {
                    buffer.detach();
                    Ok(())
                }
                _ => Err(JsError::type_error(
                    "detachArrayBuffer requires an ArrayBuffer",
                )),
            },
            _ => Err(JsError::type_error(
                "detachArrayBuffer requires an ArrayBuffer",
            )),
        }
    }

    /// The `ArrayBuffer.prototype.byteLength` getter. A detached buffer has no
    /// bytes, so it reports 0.
    fn array_buffer_byte_length(&self, receiver: ObjectId) -> Result<JsValue, JsError> {
        match self.realm.host(receiver) {
            Some(ObjectHost::ArrayBufferHost(buffer)) => {
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "buffer lengths stay far below any precision boundary"
                )]
                let length = buffer.byte_length() as f64;
                Ok(JsValue::Number(length))
            }
            _ => Err(JsError::type_error(
                "ArrayBuffer.prototype.byteLength called on an incompatible receiver",
            )),
        }
    }

    /// `ArrayBuffer.prototype.slice(start, end)`, which copies rather than
    /// shares, so writing the copy leaves the original alone. Both bounds go
    /// through `ToIntegerOrInfinity`, so an object's `valueOf` runs.
    fn array_buffer_slice(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let buffer = match self.realm.host(receiver) {
            Some(ObjectHost::ArrayBufferHost(buffer)) => buffer,
            _ => {
                return Err(JsError::type_error(
                    "ArrayBuffer.prototype.slice called on an incompatible receiver",
                ));
            }
        };
        // §25.1.6.7 step 4: a detached buffer cannot be sliced.
        buffer.ensure_attached()?;
        let total = buffer.byte_length();
        #[allow(
            clippy::cast_precision_loss,
            reason = "buffer lengths stay far below any precision boundary"
        )]
        let total_value = total as f64;
        // §25.1.6.5 steps 5-6: a negative relative index counts back from the
        // end, and the result clamps into `0..=total`.
        let relative = |integer: f64| -> f64 {
            if integer < 0.0 {
                (total_value + integer).max(0.0)
            } else {
                integer.min(total_value)
            }
        };
        let start_value = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let start = relative(self.to_integer_value(dom, &start_value)?);
        let end = match arguments.get(1) {
            None | Some(JsValue::Undefined) => total_value,
            Some(value) => relative(self.to_integer_value(dom, value)?),
        };
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "both bounds are clamped into 0..=total"
        )]
        // The conversions above can run user code, and that code may detach the
        // buffer before the copy is made.
        buffer.ensure_attached()?;
        let (start, end) = (start as usize, end as usize);
        let copied = buffer.read_bytes(start.min(end), end - start.min(end))?;
        let prototype = self.realm.get_prototype(receiver);
        let object = self.new_array_buffer(prototype, &TypedBuffer::new(copied))?;
        Ok(JsValue::Object(object))
    }

    /// Reads one `DataView` method: `GetViewValue` (§25.2.1.5) for a getter, and
    /// `SetViewValue` (§25.2.1.6) for a setter. The request index and, for a
    /// setter, the value are converted before the bounds check, as the spec
    /// orders them.
    fn data_view_access(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let Some(access) = Access::from_native(function) else {
            return Err(JsError::type_error(format!(
                "unsupported DataView native {function:?}"
            )));
        };
        let (buffer, byte_offset, byte_length) = self.data_view_host(receiver)?;
        let index = self.to_index_value(dom, arguments.first())?;
        if is_write(function) {
            let value = arguments.get(1).cloned().unwrap_or(JsValue::Undefined);
            // The per-access `littleEndian` argument defaults to false, so an
            // accessor that omits it is big-endian.
            let big_endian = !arguments.get(2).is_some_and(JsValue::is_truthy);
            let bytes = if access.is_bigint() {
                let value = self.to_bigint_value(dom, &value)?;
                encode_bigint_bytes(&value, big_endian)
            } else {
                let number = self.to_number_value(dom, &value)?;
                encode_bytes(access, number, big_endian)
            };
            let view_length = view_in_bounds(&buffer, byte_offset, byte_length)?;
            self.data_view_bounds(index, access.width(), view_length)?;
            buffer.write_bytes(byte_offset + index as usize, &bytes);
            return Ok(JsValue::Undefined);
        }
        let big_endian = !arguments.get(1).is_some_and(JsValue::is_truthy);
        let view_length = view_in_bounds(&buffer, byte_offset, byte_length)?;
        self.data_view_bounds(index, access.width(), view_length)?;
        // The bytes are read in view order, which is big-endian unless the
        // accessor asked otherwise; `decode_number` takes little-endian bytes.
        let mut bytes = buffer.read_bytes(byte_offset + index as usize, access.width())?;
        if big_endian {
            bytes.reverse();
        }
        if access.is_bigint() {
            return Ok(JsValue::BigInt(decode_bigint(access, &bytes)));
        }
        Ok(JsValue::Number(decode_number(access, &bytes)))
    }

    /// `GetViewValue` step 6 and `SetViewValue` step 10: the requested bytes must
    /// lie wholly inside the view, which is the check that makes a read at the
    /// tail a `RangeError` rather than a zero-filled answer.
    fn data_view_bounds(
        &mut self,
        index: f64,
        width: usize,
        byte_length: usize,
    ) -> Result<(), JsError> {
        #[allow(
            clippy::cast_precision_loss,
            reason = "view lengths stay far below any precision boundary"
        )]
        let view_length = byte_length as f64;
        if index + width as f64 > view_length {
            return Err(self.range_error("DataView request is outside the bounds of the view"));
        }
        Ok(())
    }

    /// §25.1.5.1 `ArrayBuffer.isView`: true for a `DataView` or a typed array,
    /// judged by their internal slots rather than by prototype.
    fn is_array_buffer_view(&self, argument: Option<&JsValue>) -> bool {
        let Some(JsValue::Object(object)) = argument else {
            return false;
        };
        matches!(self.realm.host(*object), Some(ObjectHost::DataView { .. }))
            || self.typed_array_parts(*object).is_ok()
    }

    /// The `DataView` host state of `receiver`, or a `TypeError` when it is not
    /// a `DataView`. The byte length is `None` for a length-tracking view.
    fn data_view_host(
        &self,
        receiver: ObjectId,
    ) -> Result<(TypedBuffer, usize, Option<usize>), JsError> {
        match self.realm.host(receiver) {
            Some(ObjectHost::DataView {
                buffer,
                byte_offset,
                byte_length,
                ..
            }) => Ok((buffer, byte_offset, byte_length)),
            _ => Err(JsError::type_error(
                "DataView method called on an incompatible receiver",
            )),
        }
    }

    /// The `DataView.prototype.buffer` getter: the `ArrayBuffer` the view was
    /// built on, by identity.
    fn data_view_buffer(&mut self, receiver: ObjectId) -> Result<JsValue, JsError> {
        let Some(ObjectHost::DataView { buffer, .. }) = self.realm.host(receiver) else {
            return Err(JsError::type_error(
                "DataView.prototype.buffer called on an incompatible receiver",
            ));
        };
        Ok(JsValue::Object(self.array_buffer_object(&buffer)?))
    }

    /// `new DataView(buffer[, byteOffset[, byteLength]])` (ECMA-262 25.2.2.1).
    /// The buffer must be an `ArrayBuffer`, and both offsets go through `ToIndex`
    /// before the range checks.
    pub(in crate::runtime) fn data_view_constructor(
        &mut self,
        dom: &mut Dom,
        constructor: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let Some(JsValue::Object(source)) = arguments.first() else {
            return Err(JsError::type_error(
                "DataView requires an ArrayBuffer argument",
            ));
        };
        let source = *source;
        let buffer = match self.realm.host(source) {
            Some(ObjectHost::ArrayBufferHost(buffer)) => buffer,
            _ => {
                return Err(JsError::type_error(
                    "DataView requires an ArrayBuffer argument",
                ));
            }
        };
        let offset = self.to_index_value(dom, arguments.get(1))?;
        // §25.2.2.1 step 5: a detached buffer cannot be viewed.
        buffer.ensure_attached()?;
        #[allow(
            clippy::cast_precision_loss,
            reason = "buffer lengths stay far below any precision boundary"
        )]
        let total_value = buffer.byte_length() as f64;
        if offset > total_value {
            return Err(self.range_error("DataView byteOffset extends past the end of the buffer"));
        }
        // ECMAScript 25.2.2.1 steps 8-10: an absent `byteLength` views the rest
        // of a fixed-length buffer, and tracks the length of a resizable one.
        // A present one that does not fit is a `RangeError`.
        let resizable = buffer.max_byte_length().is_some();
        let byte_length = match arguments.get(2) {
            None | Some(JsValue::Undefined) if resizable => None,
            None | Some(JsValue::Undefined) => Some(usize_from(total_value - offset)),
            Some(value) => {
                let requested = self.to_index_value(dom, Some(value))?;
                if offset + requested > total_value {
                    return Err(
                        self.range_error("DataView byteLength extends past the end of the buffer")
                    );
                }
                Some(usize_from(requested))
            }
        };
        let offset = usize_from(offset);
        let prototype = self
            .realm
            .get_property(constructor, "prototype")
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            });
        self.ensure_heap_capacity(1)?;
        // The `buffer` getter answers with `source`, which made this store.
        buffer.identify(source);
        let object = self.realm.create_object(prototype);
        if let Some(host) = self.realm.host_mut(object) {
            *host = ObjectHost::DataView {
                buffer,
                byte_offset: offset,
                byte_length,
            };
        }
        Ok(JsValue::Object(object))
    }
}

#[cfg(test)]
mod tests {
    use super::{Access, decode_number, encode_bytes, is_write};
    use crate::runtime::JsRuntime;
    use crate::value::NativeFunction;
    use render_html::parse_document;

    /// Every expectation in this module was measured against Node v24.13.1
    /// running the same expression, so a divergence is an engine change rather
    /// than a hand-computed guess.
    fn run(source: &str) -> String {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let outcome = runtime
            .execute(&mut parsed.dom, source)
            .expect("buffer probe executes");
        outcome.value.to_js_string()
    }

    #[test]
    fn a_default_view_is_big_endian_and_the_argument_overrides_it() {
        assert_eq!(
            run(r"
                var buffer = new ArrayBuffer(4);
                var bytes = new Uint8Array(buffer);
                bytes[0] = 0x12; bytes[1] = 0x34; bytes[2] = 0x56; bytes[3] = 0x78;
                var view = new DataView(buffer);
                [view.getUint16(0) === 0x1234,
                 view.getUint32(0) === 0x12345678,
                 view.getUint16(0, true) === 0x3412,
                 view.getUint8(0) + ',' + view.getUint8(3)].join('|')
            "),
            "true|true|true|18,120"
        );
        // There is no constructor-level endianness: the fourth argument is not
        // a parameter, so a view built with one still reads big-endian.
        assert_eq!(
            run(r"
                var buffer = new ArrayBuffer(4);
                new Uint8Array(buffer).set([0x12, 0x34, 0x56, 0x78]);
                new DataView(buffer, 0, 4, true).getUint16(0) === 0x1234
            "),
            "true"
        );
    }

    #[test]
    fn a_write_goes_through_the_shared_buffer_in_the_requested_order() {
        assert_eq!(
            run(r"
                var buffer = new ArrayBuffer(4);
                new DataView(buffer).setUint16(0, 0x1234);
                Array.from(new Uint8Array(buffer)).join(',')
            "),
            "18,52,0,0"
        );
        assert_eq!(
            run(r"
                var buffer = new ArrayBuffer(4);
                new DataView(buffer).setUint16(0, 0x1234, true);
                Array.from(new Uint8Array(buffer)).join(',')
            "),
            "52,18,0,0"
        );
        // `setUint32` truncates to the element width rather than overflowing.
        assert_eq!(
            run(r"
                var buffer = new ArrayBuffer(4);
                new DataView(buffer).setUint32(0, 0x1FFFFFFFF);
                Array.from(new Uint8Array(buffer)).join(',')
            "),
            "255,255,255,255"
        );
        assert_eq!(
            run(r"
                var buffer = new ArrayBuffer(1);
                new DataView(buffer).setInt8(0, -1);
                Array.from(new Uint8Array(buffer)).join(',')
            "),
            "255"
        );
        // A signed read of a high byte is negative.
        assert_eq!(
            run(r"
                var buffer = new ArrayBuffer(1);
                new Uint8Array(buffer)[0] = 200;
                new DataView(buffer).getInt8(0)
            "),
            "-56"
        );
        assert_eq!(
            run("typeof new DataView(new ArrayBuffer(4)).setUint8(0, 1)"),
            "undefined"
        );
    }

    #[test]
    fn floats_use_their_own_representations() {
        assert_eq!(
            run(r"
                var buffer = new ArrayBuffer(4);
                new DataView(buffer).setFloat32(0, 0.5);
                Array.from(new Uint8Array(buffer)).join(',')
            "),
            "63,0,0,0"
        );
        assert_eq!(
            run(r"
                var buffer = new ArrayBuffer(8);
                new DataView(buffer).setFloat64(0, 1);
                Array.from(new Uint8Array(buffer)).join(',')
            "),
            "63,240,0,0,0,0,0,0"
        );
        assert_eq!(
            run(r"
                var buffer = new ArrayBuffer(4);
                new DataView(buffer).setFloat32(0, 0.5);
                new DataView(buffer).getFloat32(0)
            "),
            "0.5"
        );
    }

    #[test]
    fn a_view_is_a_window_that_reports_its_own_geometry() {
        assert_eq!(
            run(r"
                var buffer = new ArrayBuffer(4);
                new Uint8Array(buffer).set([0x12, 0x34, 0x56, 0x78]);
                var view = new DataView(buffer, 2);
                [view.byteOffset, view.byteLength, view.getUint16(0) === 0x5678].join(',')
            "),
            "2,2,true"
        );
        assert_eq!(
            run(r"
                var buffer = new ArrayBuffer(4);
                var view = new DataView(buffer);
                [view.byteOffset, view.byteLength, view.buffer === buffer].join(',')
            "),
            "0,4,true"
        );
        assert_eq!(
            run("Object.prototype.toString.call(new DataView(new ArrayBuffer(1)))"),
            "[object DataView]"
        );
        assert_eq!(
            run("Object.prototype.toString.call(new ArrayBuffer(1))"),
            "[object ArrayBuffer]"
        );
    }

    #[test]
    fn a_request_or_a_window_past_the_end_is_a_range_error() {
        assert_eq!(
            run(r"
                function thrown(fn) { try { fn(); return 'no throw'; } catch (e) { return e.name; } }
                [thrown(function () { new DataView(new ArrayBuffer(4)).getUint32(1); }),
                 thrown(function () { new DataView(new ArrayBuffer(4), 0, 5); }),
                 thrown(function () { new DataView(new ArrayBuffer(4), 5); }),
                 thrown(function () { new DataView(new ArrayBuffer(4)).getUint8(-1); })].join(',')
            "),
            "RangeError,RangeError,RangeError,RangeError"
        );
        // A one-byte read at the last index is in bounds.
        assert_eq!(run("new DataView(new ArrayBuffer(4)).getUint8(3)"), "0");
        // `ToIndex` truncates a fractional index rather than rejecting it.
        assert_eq!(run("new DataView(new ArrayBuffer(4)).getUint8(1.5)"), "0");
    }

    #[test]
    fn a_buffer_shares_its_bytes_with_a_typed_array_view() {
        assert_eq!(
            run(r"
                var buffer = new ArrayBuffer(8);
                var view = new Uint8Array(buffer);
                view[0] = 7;
                [view.length, view.byteLength, new Uint8Array(buffer)[0]].join(',')
            "),
            "8,8,7"
        );
        // A `DataView` write is visible through the typed array over the same
        // buffer, which is the whole reason a view shares storage.
        assert_eq!(
            run(r"
                var buffer = new ArrayBuffer(4);
                new DataView(buffer).setUint8(2, 9);
                new Uint8Array(buffer)[2]
            "),
            "9"
        );
        // A wider view reads the same bytes in little-endian order, and its
        // `buffer` is the object the buffer was made with.
        assert_eq!(
            run(r"
                var buffer = new ArrayBuffer(4);
                new Uint8Array(buffer).set([0x34, 0x12, 0, 0]);
                var wide = new Uint16Array(buffer);
                [wide[0], wide.length, wide.buffer === buffer].join(',')
            "),
            "4660,2,true"
        );
        assert_eq!(
            run(r"
                var buffer = new ArrayBuffer(4);
                var view = new Int16Array(buffer);
                view[1] = -2;
                Array.from(new Uint8Array(buffer)).join(',')
            "),
            "0,0,254,255"
        );
    }

    #[test]
    fn a_view_over_a_buffer_must_fit_its_element_size() {
        assert_eq!(
            run(r"
                function thrown(fn) { try { fn(); return 'no throw'; } catch (e) { return e.name; } }
                [thrown(function () { new Int16Array(new ArrayBuffer(3)); }),
                 thrown(function () { new Int16Array(new ArrayBuffer(4), 1); }),
                 new Int16Array(new ArrayBuffer(4), 2, 1).length].join(',')
            "),
            "RangeError,RangeError,1"
        );
    }

    #[test]
    fn a_float_view_decodes_the_bytes_a_dataview_wrote() {
        assert_eq!(
            run(r"
                var buffer = new ArrayBuffer(4);
                new DataView(buffer).setFloat32(0, 1.5, true);
                [new Float32Array(buffer)[0], new Uint8Array(buffer)[3]].join(',')
            "),
            "1.5,63"
        );
    }

    #[test]
    fn views_of_one_buffer_report_the_same_buffer_object() {
        assert_eq!(
            run(r"
                var made = new Float64Array(2);
                [made.buffer.byteLength,
                 made.buffer === made.buffer,
                 new Uint8Array(made.buffer).length,
                 new DataView(made.buffer).buffer === made.buffer,
                 new Uint16Array(made.buffer).buffer === made.buffer].join(',')
            "),
            "16,true,16,true,true"
        );
    }

    #[test]
    fn a_detached_buffer_is_empty_and_its_views_refuse_access() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        runtime
            .execute(
                &mut parsed.dom,
                r"var buffer = new ArrayBuffer(8);
                  var bytes = new Uint8Array(buffer);
                  var wide = new Float64Array(buffer);
                  var view = new DataView(buffer);",
            )
            .expect("views build over a fresh buffer");
        let buffer = runtime.realm().global("buffer").expect("buffer is defined");
        runtime
            .detach_array_buffer(&buffer)
            .expect("an ArrayBuffer detaches");
        let outcome = runtime
            .execute(
                &mut parsed.dom,
                r"function thrown(fn) { try { fn(); return 'no throw'; } catch (e) { return e.name; } }
                  [buffer.byteLength, bytes.length, bytes.byteLength, wide.length,
                   wide.byteOffset, wide[0] === undefined,
                   thrown(function () { bytes.fill(0); }),
                   thrown(function () { return view.byteLength; }),
                   thrown(function () { view.getUint8(0); }),
                   thrown(function () { buffer.slice(0); }),
                   thrown(function () { new DataView(buffer); }),
                   thrown(function () { new Uint8Array(buffer); }),
                   wide.buffer === buffer,
                   Object.prototype.toString.call(wide)].join(',')",
            )
            .expect("the detached views answer");
        assert_eq!(
            outcome.value.to_js_string(),
            "0,0,0,0,0,true,TypeError,TypeError,TypeError,TypeError,TypeError,TypeError,true,[object Float64Array]"
        );
    }

    #[test]
    fn detaching_something_that_is_not_a_buffer_is_a_type_error() {
        let mut parsed = parse_document("<!doctype html><p></p>");
        let mut runtime = JsRuntime::new(&parsed.dom);
        let plain = runtime
            .execute(&mut parsed.dom, "({})")
            .expect("an object literal evaluates")
            .value;
        assert!(runtime.detach_array_buffer(&plain).is_err());
    }

    #[test]
    fn a_buffer_slice_copies_and_clamps() {
        assert_eq!(
            run(r"
                var buffer = new ArrayBuffer(4);
                new Uint8Array(buffer)[0] = 1;
                [buffer.byteLength, buffer.slice(0, 2).byteLength, buffer.slice(-2).byteLength].join(',')
            "),
            "4,2,2"
        );
        assert_eq!(
            run(r"
                var buffer = new ArrayBuffer(4);
                new Uint8Array(buffer.slice(0, 2))[0] = 9;
                new Uint8Array(buffer)[0]
            "),
            "0"
        );
    }

    #[test]
    fn view_geometry_is_read_through_prototype_accessors() {
        assert_eq!(
            run(r"
                var buffer = new ArrayBuffer(8);
                var view = new DataView(buffer, 2, 4);
                [view.buffer === buffer, view.byteOffset, view.byteLength,
                 view.hasOwnProperty('byteLength'), buffer.byteLength,
                 typeof Object.getOwnPropertyDescriptor(DataView.prototype, 'byteOffset').get].join(',')
            "),
            "true,2,4,false,8,function"
        );
        // A typed array is not an `ArrayBuffer`, so it cannot back a view.
        assert_eq!(
            run("var r; try { new DataView(new Uint8Array(4)); } catch (e) { r = e.name; } r"),
            "TypeError"
        );
    }

    #[test]
    fn value_of_runs_before_the_range_check_and_lengths_are_to_index() {
        assert_eq!(
            run(r"
                var calls = [];
                var view = new DataView(new ArrayBuffer(4));
                var result = [];
                try {
                    view.setUint8(9, { valueOf: function () { calls.push('value'); return 1; } });
                } catch (e) { result.push(e.name); }
                result.push(calls.join('|'));
                result.push(new ArrayBuffer({ valueOf: function () { return 3; } }).byteLength);
                result.push(new DataView(new ArrayBuffer(4), { valueOf: function () { return 1; } }).byteOffset);
                result.join(',')
            "),
            "RangeError,value,3,1"
        );
        // An absent value converts as `undefined`, which is NaN and stores 0.
        assert_eq!(
            run("var v = new DataView(new ArrayBuffer(1)); v.setUint8(0); v.getUint8(0)"),
            "0"
        );
    }

    #[test]
    fn float16_accessors_round_to_binary16() {
        // 1.5 is 0x3E00 and 1 is 0x3C00, read big-endian by default.
        assert_eq!(
            run(r"
                var view = new DataView(new ArrayBuffer(4));
                view.setFloat16(0, 1.5);
                var one = new DataView(new ArrayBuffer(2));
                one.setUint8(0, 0x3C);
                [view.getFloat16(0), one.getFloat16(0), view.getUint8(0), view.getUint8(1)].join(',')
            "),
            "1.5,1,62,0"
        );
        // 65520 is the first value that rounds to infinity; 65519.99 rounds down
        // to the largest finite binary16, and the smallest subnormal is exact.
        assert_eq!(
            run(r"
                var view = new DataView(new ArrayBuffer(2));
                view.setFloat16(0, 65520);
                var overflow = view.getFloat16(0);
                view.setFloat16(0, 65519.99);
                var below = view.getFloat16(0);
                view.setFloat16(0, 5.960464477539063e-8);
                var smallest = view.getFloat16(0);
                [overflow, below, smallest === 5.960464477539063e-8].join(',')
            "),
            "Infinity,65504,true"
        );
        // 2049 is halfway between 2048 and 2050, and ties go to the even one.
        assert_eq!(
            run(
                "var view = new DataView(new ArrayBuffer(2)); view.setFloat16(0, 2049); view.getFloat16(0)"
            ),
            "2048"
        );
    }

    #[test]
    fn integer_setters_wrap_modulo_their_width() {
        // `ToInt8` and `ToUint8` reduce modulo 2^8, so 2^40 + 1 stores 1 and -1
        // stores 255.
        assert_eq!(
            run(
                "var v = new DataView(new ArrayBuffer(2)); v.setInt8(0, Math.pow(2, 40) + 1); v.setUint8(1, -1); [v.getInt8(0), v.getUint8(1), v.getUint8(0)].join(',')"
            ),
            "1,255,1"
        );
    }

    #[test]
    fn constructors_are_new_only() {
        assert_eq!(
            run(r"
                function thrown(fn) { try { fn(); return 'no throw'; } catch (e) { return e.name; } }
                [thrown(function () { DataView(); }),
                 thrown(function () { ArrayBuffer(); }),
                 typeof new DataView(new ArrayBuffer(1)),
                 typeof new ArrayBuffer(1)].join(',')
            "),
            "TypeError,TypeError,object,object"
        );
    }

    #[test]
    fn get_and_set_accessors_are_told_apart() {
        assert!(!is_write(NativeFunction::DataViewGetUint32));
        assert!(is_write(NativeFunction::DataViewSetUint32));
        assert_eq!(
            Access::from_native(NativeFunction::DataViewGetFloat64),
            Some(Access::Float64)
        );
        assert_eq!(
            Access::from_native(NativeFunction::DataViewSetInt16),
            Some(Access::Int16)
        );
        assert_eq!(Access::Uint8.width(), 1);
        assert_eq!(Access::Float32.width(), 4);
        assert_eq!(Access::Float64.width(), 8);
    }

    #[test]
    fn a_big_endian_pattern_puts_the_high_byte_first() {
        assert_eq!(encode_bytes(Access::Uint16, 4660.0, true), vec![0x12, 0x34]);
        assert_eq!(
            encode_bytes(Access::Uint16, 4660.0, false),
            vec![0x34, 0x12]
        );
        assert_eq!(
            encode_bytes(Access::Uint32, 305_419_896.0, true),
            vec![0x12, 0x34, 0x56, 0x78]
        );
    }

    #[test]
    fn patterns_round_trip_in_both_byte_orders() {
        for (access, value) in [
            (Access::Int8, -2.0),
            (Access::Uint8, 200.0),
            (Access::Int16, -300.0),
            (Access::Uint16, 60_000.0),
            (Access::Int32, -70_000.0),
            (Access::Uint32, 4_000_000_000.0),
        ] {
            for big_endian in [false, true] {
                let mut bytes = encode_bytes(access, value, big_endian);
                assert_eq!(bytes.len(), access.width());
                // A getter reads a big-endian view back to little-endian first.
                if big_endian {
                    bytes.reverse();
                }
                assert_eq!(decode_number(access, &bytes), value);
            }
        }
    }

    #[test]
    fn floats_round_trip_through_a_bit_pattern() {
        let mut bytes = encode_bytes(Access::Float64, 0.5, true);
        bytes.reverse();
        assert_eq!(decode_number(Access::Float64, &bytes), 0.5);
        // A float accessor reinterprets bits, so the pattern is the IEEE binary
        // encoding of the value rather than the value itself.
        let mut half = encode_bytes(Access::Float32, 0.5, true);
        assert_eq!(half, vec![0x3f, 0x00, 0x00, 0x00]);
        half.reverse();
        assert_eq!(decode_number(Access::Float32, &half), 0.5);
    }

    /// Expectations measured against Node v22 running the same probe, so a
    /// divergence is an engine change: resize growth and truncation, the
    /// length-tracking and fixed views that follow or leave bounds, the
    /// `maxByteLength` option, and `transfer` with its detach.
    #[test]
    fn a_resizable_buffer_resizes_and_its_views_follow_or_leave_bounds() {
        assert_eq!(
            run(r"
(function () {
function thrown(fn) {
  try { fn(); return 'none'; } catch (e) { return e.constructor.name; }
}
var out = [];
var rab = new ArrayBuffer(4, { maxByteLength: 8 });
out.push(rab.resizable, rab.maxByteLength, rab.byteLength, rab.detached);
var fixed = new ArrayBuffer(4);
out.push(fixed.resizable, fixed.maxByteLength, typeof fixed.resize, fixed.transfer.length, fixed.resize.length);
var u8 = new Uint8Array(rab);
var u8fixed = new Uint8Array(rab, 0, 4);
var u8off = new Uint8Array(rab, 2);
for (var i = 0; i < 4; i++) u8[i] = i + 1;
out.push(u8.length, u8fixed.length, u8off.length);
rab.resize(6);
out.push('grow', rab.byteLength, u8.length, u8fixed.length, u8off.length, u8[5], u8[4], Array.from(u8).join(','));
rab.resize(3);
out.push('shrink3', u8.length, u8fixed.length, u8off.length, u8fixed.byteLength, u8fixed.byteOffset, u8off.byteOffset, u8[2], u8[3]);
rab.resize(1);
out.push('shrink1', u8.length, u8.byteLength, u8.byteOffset, u8fixed.length, u8off.length, u8off.byteOffset, u8off.byteLength);
out.push('oob get', String(u8fixed[0]), 'set', (u8fixed[0] = 9, u8fixed[0]));
rab.resize(8);
out.push('regrow', u8fixed.length, u8off.length, u8[7], u8fixed[0], u8off[0]);
out.push('resize too big', thrown(function () { rab.resize(9); }));
out.push('resize fixed', thrown(function () { fixed.resize(2); }));
out.push('resize detached-like', thrown(function () { ArrayBuffer.prototype.resize.call(fixed, 1); }));
rab.resize(2);
var dv = new DataView(rab);
var dvfixed = new DataView(rab, 0, 2);
out.push('dv', dv.byteLength, dv.byteOffset, dvfixed.byteLength);
rab.resize(5);
out.push('dv grow', dv.byteLength, dvfixed.byteLength);
rab.resize(1);
out.push('dv oob', thrown(function () { return dvfixed.byteLength; }), thrown(function () { return dvfixed.byteOffset; }), thrown(function () { dvfixed.getUint8(0); }), dv.byteLength, dv.buffer === rab);
out.push('ctor oob', thrown(function () { new Uint8Array(rab, 2); }), thrown(function () { new DataView(rab, 2); }));
var lt = new Uint16Array(rab);
out.push('u16 tracking', thrown(function () { new Uint16Array(rab, 0); }) );
rab.resize(0);
out.push('empty tracking', lt.length, lt.byteLength);
rab.resize(8);
out.push('u16 fixed buffer rule', new Uint16Array(new ArrayBuffer(4)).length);
var t = new ArrayBuffer(4, { maxByteLength: 8 });
new Uint8Array(t).set([1, 2, 3, 4]);
var moved = t.transfer();
out.push('transfer', t.detached, t.byteLength, t.maxByteLength, moved.byteLength, moved.resizable, moved.maxByteLength, Array.from(new Uint8Array(moved)).join(','));
out.push('transfer detached', thrown(function () { t.transfer(); }), thrown(function () { t.slice(0); }));
var m2 = moved.transfer(6);
out.push('transfer grow', m2.byteLength, Array.from(new Uint8Array(m2)).join(','), m2.resizable);
var fx = m2.transferToFixedLength(2);
out.push('tofixed', fx.byteLength, fx.resizable, fx.maxByteLength, m2.detached);
out.push('maxByteLength detached', m2.maxByteLength, m2.byteLength, m2.detached);
out.push('max too large', thrown(function () { new ArrayBuffer(0, { maxByteLength: 9007199254740991 }); }));
out.push('len over max', thrown(function () { new ArrayBuffer(5, { maxByteLength: 4 }); }));
out.push('opt null', new ArrayBuffer(2, null).resizable, 'opt undef', new ArrayBuffer(2, { maxByteLength: undefined }).resizable);
out.push('transfer no arg', new ArrayBuffer(3, { maxByteLength: 8 }).transfer().byteLength);
return out.join(' | ');
})()
            "),
            "true | 8 | 4 | false | false | 4 | function | 0 | 1 | 4 | 4 | 2 | grow | 6 | 6 | 4 | 4 | 0 | 0 | 1,2,3,4,0,0 | shrink3 | 3 | 0 | 1 | 0 | 0 | 2 | 3 |  | shrink1 | 1 | 1 | 0 | 0 | 0 | 0 | 0 | oob get | undefined | set |  | regrow | 4 | 6 | 0 | 1 | 0 | resize too big | RangeError | resize fixed | TypeError | resize detached-like | TypeError | dv | 2 | 0 | 2 | dv grow | 5 | 2 | dv oob | TypeError | TypeError | TypeError | 1 | true | ctor oob | RangeError | RangeError | u16 tracking | none | empty tracking | 0 | 0 | u16 fixed buffer rule | 2 | transfer | true | 0 | 0 | 4 | true | 8 | 1,2,3,4 | transfer detached | TypeError | TypeError | transfer grow | 6 | 1,2,3,4,0,0 | true | tofixed | 2 | false | 2 | true | maxByteLength detached | 0 | 0 | true | max too large | RangeError | len over max | RangeError | opt null | false | opt undef | false | transfer no arg | 3"
        );
    }

    /// Array and typed-array iterators read their target's length at each step,
    /// so a write or a resize during iteration is seen (ECMA-262 23.1.5.2.1), and
    /// an out-of-bounds typed array is a `TypeError` at the step that reads it.
    /// Expectations were measured against Node v22 running the same program.
    #[test]
    fn array_iterators_read_the_live_length_at_each_step() {
        assert_eq!(
            run(r"
(function () {
function thrown(fn) {
  try { fn(); return 'none'; } catch (e) { return e.constructor.name; }
}
var out = [];
var a = [1, 2, 3];
var it = a.values();
it.next();
a.push(4);
out.push('arr live', it.next().value, it.next().value, it.next().done);
a.push(5);
out.push('done stays', it.next().done, it.next().value);
var k = [7, 8].keys();
out.push('keys', k.next().value, k.next().value, k.next().done);
var e = ['x'].entries();
out.push('entries', e.next().value.join(':'), e.next().done);
var rab = new ArrayBuffer(4, { maxByteLength: 8 });
var ta = new Uint8Array(rab);
for (var i = 0; i < 4; i++) ta[i] = i + 1;
var seen = [];
var vit = ta.values();
seen.push(vit.next().value);
rab.resize(2);
seen.push(String(vit.next().value), vit.next().done);
out.push('ta values', seen.join(','));
rab.resize(6);
var seen2 = [];
for (var x of ta.values()) seen2.push(x);
out.push('for-of resized', seen2.join(','));
var fixed = new Uint8Array(rab, 0, 4);
var fit = fixed.keys();
fit.next();
rab.resize(3);
out.push('fixed keys oob', thrown(function () { fit.next(); }));
var eit = new Uint8Array(rab).entries();
out.push('entries len', eit.next().value.join(':'));
var oob = fixed;
out.push('values oob at creation', thrown(function () { oob.values(); }), thrown(function () { oob.keys(); }));
var grow = new Uint8Array(new ArrayBuffer(2, { maxByteLength: 4 }));
var git = grow.values();
git.next();
grow.buffer.resize(4);
out.push('grow visit', git.next().value, git.next().value, git.next().done);
out.push('array-like done', [].values().next().done);
out.push('spread', [...new Uint8Array(new ArrayBuffer(3))].join(','));
out.push('from iterable', Array.from([5, 6].entries()).map(function (p) { return p.join(':'); }).join(' '));
return out.join(' | ');
})()
            "),
            "arr live | 2 | 3 | false | done stays | false |  | keys | 0 | 1 | true | entries | 0:x | true | ta values | 1,2,true | for-of resized | 1,2,0,0,0,0 | fixed keys oob | TypeError | entries len | 0:1 | values oob at creation | TypeError | TypeError | grow visit | 0 | 0 | false | array-like done | true | spread | 0,0,0 | from iterable | 0:5 1:6"
        );
    }

    /// `fill` converts its value and range before it writes, and those conversions
    /// can resize the buffer: a fixed-length view that is now out of bounds is a
    /// `TypeError`, and a length-tracking view clamps its end to the new length
    /// (ECMA-262 23.2.3.9 steps 7-9). Measured against Node v22.
    #[test]
    fn fill_revalidates_the_view_after_its_arguments_resize_the_buffer() {
        assert_eq!(
            run(r"
(function () {
var out = [];
function attempt(fn) { try { fn(); return 'none'; } catch (e) { return e.constructor.name; } }
var rab = new ArrayBuffer(4, { maxByteLength: 8 });
var fixed = new Uint8Array(rab, 0, 4);
out.push(attempt(function () {
  fixed.fill(3, { valueOf: function () { rab.resize(2); return 1; } }, 2);
}));
var rab2 = new ArrayBuffer(4, { maxByteLength: 8 });
var lt = new Uint8Array(rab2);
out.push(attempt(function () {
  lt.fill(3, 1, { valueOf: function () { rab2.resize(2); return 4; } });
}));
out.push(Array.from(lt).join(','));
out.push(attempt(function () {
  lt.fill({ valueOf: function () { rab2.resize(1); return 9; } }, 0, 2);
}));
out.push(Array.from(new Uint8Array(rab2)).join(','));
return out.join(' | ');
})()
            "),
            "TypeError | none | 0,3 | none | 9"
        );
    }
}
