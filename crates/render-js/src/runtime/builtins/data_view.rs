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
//! **Buffer granularity.** A buffer is one byte per slot, and a view over it is
//! byte-exact, so `new Uint8Array(buffer)` and `new DataView(buffer)` see the
//! same bytes. A typed array of a wider element type over a buffer composes its
//! element from consecutive byte slots, which is the standard's model.

use super::array::MAX_SAFE_INTEGER;
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
}

impl Access {
    const fn width(self) -> usize {
        match self {
            Self::Int8 | Self::Uint8 => 1,
            Self::Int16 | Self::Uint16 | Self::Float16 => 2,
            Self::Int32 | Self::Uint32 | Self::Float32 => 4,
            Self::Float64 => 8,
        }
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
    )
}

/// Widen a narrow little-endian pattern into the 8-byte buffer the byte-order
/// step works on.
fn widen<const N: usize>(source: [u8; N]) -> [u8; 8] {
    let mut bytes = [0u8; 8];
    bytes[..N].copy_from_slice(&source);
    bytes
}

/// The `width` bytes of `value` in the requested order, produced from the
/// little-endian pattern the element's own representation uses.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "each integer is already wrapped into its element range by `encode`"
)]
fn encode_bytes(access: Access, value: f64, big_endian: bool) -> Vec<u8> {
    let pattern: [u8; 8] = match access {
        // The integer kinds wrap modulo 2^N as `ToInt8`..`ToUint32` do, which is
        // the same conversion a typed array store performs.
        Access::Int8 => widen([TypedArrayKind::Int8.encode(value) as i64 as u8]),
        Access::Uint8 => widen([TypedArrayKind::Uint8.encode(value) as u8]),
        Access::Int16 => widen((TypedArrayKind::Int16.encode(value) as i64 as i16).to_le_bytes()),
        Access::Uint16 => widen((TypedArrayKind::Uint16.encode(value) as u16).to_le_bytes()),
        Access::Int32 => widen((TypedArrayKind::Int32.encode(value) as i64 as i32).to_le_bytes()),
        Access::Uint32 => widen((TypedArrayKind::Uint32.encode(value) as u32).to_le_bytes()),
        Access::Float16 => widen(f16_bits(value).to_le_bytes()),
        Access::Float32 => {
            #[allow(
                clippy::cast_possible_truncation,
                reason = "a Float32 view accessor rounds to IEEE binary32"
            )]
            let narrowed = value as f32;
            widen(narrowed.to_bits().to_le_bytes())
        }
        Access::Float64 => value.to_bits().to_le_bytes(),
    };
    let mut bytes = pattern[..access.width()].to_vec();
    if big_endian {
        bytes.reverse();
    }
    bytes
}

/// The `Number` a getter returns for a little-endian byte pattern.
fn decode_number(access: Access, pattern: u64) -> f64 {
    match access {
        Access::Int8 => f64::from(pattern as u8 as i8),
        Access::Uint8 => f64::from(pattern as u8),
        Access::Int16 => f64::from(pattern as u16 as i16),
        Access::Uint16 => f64::from(pattern as u16),
        Access::Int32 => f64::from(pattern as u32 as i32),
        Access::Uint32 => f64::from(pattern as u32),
        Access::Float16 => f16_value(pattern as u16),
        // A float accessor reinterprets the pattern; it does not convert it.
        Access::Float32 => f64::from(f32::from_bits(pattern as u32)),
        Access::Float64 => f64::from_bits(pattern),
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

/// Reverse the low `width` bytes of a little-endian pattern.
fn reverse_bytes(pattern: u64, width: usize) -> u64 {
    let mut reversed = 0u64;
    for step in 0..width {
        let shift = (step * 8) as u32;
        reversed |= ((pattern >> shift) & 0xff) << ((width - 1 - step) * 8) as u32;
    }
    reversed
}

impl JsRuntime {
    pub(in crate::runtime) fn dispatch_data_view_native(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match function {
            NativeFunction::ArrayBufferSlice => self.array_buffer_slice(dom, receiver, arguments),
            NativeFunction::ArrayBufferByteLengthGetter => self.array_buffer_byte_length(receiver),
            NativeFunction::DataViewBufferGetter => self.data_view_buffer(receiver),
            NativeFunction::DataViewByteLengthGetter => {
                let (_, _, byte_length) = self.data_view_host(receiver)?;
                Ok(JsValue::Number(byte_length as f64))
            }
            NativeFunction::DataViewByteOffsetGetter => {
                let (_, byte_offset, _) = self.data_view_host(receiver)?;
                Ok(JsValue::Number(byte_offset as f64))
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

    /// `new ArrayBuffer(byteLength)`: one byte per slot, so a buffer is
    /// byte-granular and every view over it is byte-exact.
    pub(in crate::runtime) fn array_buffer_constructor(
        &mut self,
        dom: &mut Dom,
        constructor: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        // §25.1.4.1 step 2: `ToIndex(length)`.
        let length = self.to_index_value(dom, arguments.first())?;
        if length > Self::MAX_TYPED_ARRAY_ELEMENTS as f64 {
            return Err(self.range_error("ArrayBuffer length exceeds the engine bound"));
        }
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the value is validated as a non-negative integer within the engine bound"
        )]
        let length = length as usize;
        let prototype = self
            .realm
            .get_property(constructor, "prototype")
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            });
        let buffer = TypedBuffer(std::rc::Rc::new(std::cell::RefCell::new(vec![0.0; length])));
        self.ensure_heap_capacity(1)?;
        let object = self.realm.create_object(prototype);
        if let Some(host) = self.realm.host_mut(object) {
            *host = ObjectHost::ArrayBufferHost(buffer);
        }
        Ok(JsValue::Object(object))
    }

    /// The `ArrayBuffer.prototype.byteLength` getter.
    fn array_buffer_byte_length(&self, receiver: ObjectId) -> Result<JsValue, JsError> {
        match self.realm.host(receiver) {
            Some(ObjectHost::ArrayBufferHost(buffer)) => {
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "buffer lengths stay far below any precision boundary"
                )]
                let length = buffer.0.borrow().len() as f64;
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
        let total = buffer.0.borrow().len();
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
        let (start, end) = (start as usize, end as usize);
        let copied = buffer.0.borrow()[start.min(end)..end].to_vec();
        let prototype = self.realm.get_prototype(receiver);
        self.ensure_heap_capacity(1)?;
        let object = self.realm.create_object(prototype);
        if let Some(host) = self.realm.host_mut(object) {
            *host = ObjectHost::ArrayBufferHost(TypedBuffer(std::rc::Rc::new(
                std::cell::RefCell::new(copied),
            )));
        }
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
            let number = self.to_number_value(dom, &value)?;
            // The per-access `littleEndian` argument defaults to false, so an
            // accessor that omits it is big-endian.
            let big_endian = !arguments.get(2).is_some_and(JsValue::is_truthy);
            self.data_view_bounds(index, access.width(), byte_length)?;
            let bytes = encode_bytes(access, number, big_endian);
            let mut slots = buffer.0.borrow_mut();
            for (step, byte) in bytes.into_iter().enumerate() {
                let slot = byte_offset + index as usize + step;
                if let Some(cell) = slots.get_mut(slot) {
                    *cell = f64::from(byte);
                }
            }
            return Ok(JsValue::Undefined);
        }
        let big_endian = !arguments.get(1).is_some_and(JsValue::is_truthy);
        self.data_view_bounds(index, access.width(), byte_length)?;
        // Reading a big-endian view means the first byte it covers is the most
        // significant one, so the raw pattern is assembled in view order and
        // then reversed for the conversion.
        let slots = buffer.0.borrow();
        let mut pattern = 0u64;
        for step in 0..access.width() {
            let byte = slots
                .get(byte_offset + index as usize + step)
                .map_or(0u8, |cell| *cell as i64 as u8);
            let shift = (step * 8) as u32;
            pattern |= u64::from(byte) << shift;
        }
        if big_endian {
            pattern = reverse_bytes(pattern, access.width());
        }
        Ok(JsValue::Number(decode_number(access, pattern)))
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

    /// The `DataView` host state of `receiver`, or a `TypeError` when it is not
    /// a `DataView`.
    fn data_view_host(&self, receiver: ObjectId) -> Result<(TypedBuffer, usize, usize), JsError> {
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
    fn data_view_buffer(&self, receiver: ObjectId) -> Result<JsValue, JsError> {
        match self.realm.host(receiver) {
            Some(ObjectHost::DataView { buffer_object, .. }) => Ok(JsValue::Object(buffer_object)),
            _ => Err(JsError::type_error(
                "DataView.prototype.buffer called on an incompatible receiver",
            )),
        }
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
        let (buffer, total) = match self.realm.host(source) {
            Some(ObjectHost::ArrayBufferHost(buffer)) => {
                let total = buffer.0.borrow().len();
                (buffer, total)
            }
            _ => {
                return Err(JsError::type_error(
                    "DataView requires an ArrayBuffer argument",
                ));
            }
        };
        #[allow(
            clippy::cast_precision_loss,
            reason = "buffer lengths stay far below any precision boundary"
        )]
        let total_value = total as f64;
        let offset = self.to_index_value(dom, arguments.get(1))?;
        if offset > total_value {
            return Err(self.range_error("DataView byteOffset extends past the end of the buffer"));
        }
        // ECMAScript 25.2.5.1: an absent `byteLength` is clamped to what is
        // left of the buffer, while a present one that does not fit is a
        // `RangeError`.
        let byte_length = match arguments.get(2) {
            None | Some(JsValue::Undefined) => total_value - offset,
            Some(value) => {
                let requested = self.to_index_value(dom, Some(value))?;
                if offset + requested > total_value {
                    return Err(
                        self.range_error("DataView byteLength extends past the end of the buffer")
                    );
                }
                requested
            }
        };
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "both values are validated against the buffer length"
        )]
        let (offset, byte_length) = (offset as usize, byte_length as usize);
        let prototype = self
            .realm
            .get_property(constructor, "prototype")
            .and_then(|value| match value {
                JsValue::Object(object) => Some(object),
                _ => None,
            });
        self.ensure_heap_capacity(1)?;
        let object = self.realm.create_object(prototype);
        if let Some(host) = self.realm.host_mut(object) {
            *host = ObjectHost::DataView {
                buffer,
                buffer_object: source,
                byte_offset: offset,
                byte_length,
            };
        }
        Ok(JsValue::Object(object))
    }
}

#[cfg(test)]
mod tests {
    use super::{Access, decode_number, encode_bytes, is_write, reverse_bytes};
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
        // A wider view over a shared buffer is refused rather than mis-composed.
        assert_eq!(
            run(r"
                function thrown(fn) { try { fn(); return 'no throw'; } catch (e) { return e.name; } }
                [thrown(function () { return new Int32Array(new ArrayBuffer(8)); }),
                 thrown(function () { return new Int8Array(new ArrayBuffer(1)); })].join(',')
            "),
            "TypeError,TypeError"
        );
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
                let bytes = encode_bytes(access, value, big_endian);
                assert_eq!(bytes.len(), access.width());
                // The stored order is the pattern the getter assembles.
                let mut stored = 0u64;
                for (step, byte) in bytes.iter().enumerate() {
                    stored |= u64::from(*byte) << (step * 8);
                }
                let pattern = if big_endian {
                    reverse_bytes(stored, access.width())
                } else {
                    stored
                };
                assert_eq!(decode_number(access, pattern), value);
            }
        }
    }

    #[test]
    fn floats_round_trip_through_a_bit_pattern() {
        let value = 0.5_f64;
        let bytes = encode_bytes(Access::Float64, value, true);
        let mut stored = 0u64;
        for (step, byte) in bytes.iter().enumerate() {
            stored |= u64::from(*byte) << (step * 8);
        }
        assert_eq!(
            decode_number(Access::Float64, reverse_bytes(stored, 8)),
            value
        );
        // A float accessor reinterprets bits, so the pattern is the IEEE binary
        // encoding of the value rather than the value itself.
        let half = encode_bytes(Access::Float32, 0.5, true);
        assert_eq!(half, vec![0x3f, 0x00, 0x00, 0x00]);
        let mut half_stored = 0u64;
        for (step, byte) in half.iter().enumerate() {
            half_stored |= u64::from(*byte) << (step * 8);
        }
        assert_eq!(
            decode_number(Access::Float32, reverse_bytes(half_stored, 4)),
            0.5
        );
    }
}
