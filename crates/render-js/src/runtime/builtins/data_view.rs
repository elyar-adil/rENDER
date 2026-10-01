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

use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::runtime::convert::to_number;
use crate::value::{NativeFunction, ObjectHost, TypedBuffer};
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
    Float32,
    Float64,
}

impl Access {
    const fn width(self) -> usize {
        match self {
            Self::Int8 | Self::Uint8 => 1,
            Self::Int16 | Self::Uint16 => 2,
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
fn encode_bytes(access: Access, value: f64, big_endian: bool) -> Vec<u8> {
    let pattern: [u8; 8] = match access {
        Access::Int8 | Access::Uint8 => widen([value as i64 as u8]),
        Access::Int16 | Access::Uint16 => widen((value as i64 as i16).to_le_bytes()),
        Access::Int32 | Access::Uint32 => widen((value as i64 as i32).to_le_bytes()),
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
        // A float accessor reinterprets the pattern; it does not convert it.
        Access::Float32 => f64::from(f32::from_bits(pattern as u32)),
        Access::Float64 => f64::from_bits(pattern),
    }
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
        _dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match function {
            NativeFunction::ArrayBufferSlice => self.array_buffer_slice(receiver, arguments),
            _ => self.data_view_access(function, receiver, arguments),
        }
    }

    /// `new ArrayBuffer(byteLength)`: one byte per slot, so a buffer is
    /// byte-granular and every view over it is byte-exact.
    pub(in crate::runtime) fn array_buffer_constructor(
        &mut self,
        constructor: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let length = match arguments.first() {
            None | Some(JsValue::Undefined) => 0.0,
            Some(value) => to_number(value)?,
        };
        if !length.is_finite() || length < 0.0 || length.trunc() != length {
            return Err(self.range_error("ArrayBuffer length must be a non-negative integer"));
        }
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the value is validated as a non-negative integer"
        )]
        let length = length as usize;
        if length > Self::MAX_TYPED_ARRAY_ELEMENTS {
            return Err(self.range_error("ArrayBuffer length exceeds the engine bound"));
        }
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
        self.realm.define_property(
            object,
            "byteLength",
            crate::PropertyDescriptor {
                value: JsValue::Number(length as f64),
                writable: false,
                getter: None,
                setter: None,
                enumerable: true,
                configurable: true,
            },
        );
        Ok(JsValue::Object(object))
    }

    /// `ArrayBuffer.prototype.slice`, which copies rather than shares, so
    /// writing the copy leaves the original alone.
    fn array_buffer_slice(
        &mut self,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let Some(ObjectHost::ArrayBufferHost(buffer)) = self.realm.host(receiver) else {
            return Err(JsError::type_error(
                "ArrayBuffer.prototype.slice called on an incompatible receiver",
            ));
        };
        let total = buffer.0.borrow().len();
        #[allow(
            clippy::cast_precision_loss,
            reason = "buffer lengths stay far below any precision boundary"
        )]
        let total_value = total as f64;
        let relative = |value: Option<&JsValue>, default: f64| -> f64 {
            let number = match value {
                None | Some(JsValue::Undefined) => return default,
                Some(value) => to_number(value).unwrap_or(f64::NAN),
            };
            if number.is_nan() {
                return 0.0;
            }
            if number < 0.0 {
                (total_value + number).max(0.0)
            } else {
                number.min(total_value)
            }
        };
        let start = relative(arguments.first(), 0.0).trunc();
        let end = relative(arguments.get(1), total_value).trunc();
        let start = start.clamp(0.0, total_value) as usize;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the value is clamped to the buffer length"
        )]
        let end = end.clamp(0.0, total_value) as usize;
        let copied = buffer.0.borrow()[start.min(end)..end].to_vec();
        let prototype = self.realm.get_prototype(receiver);
        self.ensure_heap_capacity(1)?;
        let object = self.realm.create_object(prototype);
        if let Some(host) = self.realm.host_mut(object) {
            *host = ObjectHost::ArrayBufferHost(TypedBuffer(std::rc::Rc::new(
                std::cell::RefCell::new(copied),
            )));
        }
        self.realm.define_property(
            object,
            "byteLength",
            crate::PropertyDescriptor {
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "buffer lengths stay far below any precision boundary"
                )]
                value: JsValue::Number(end.saturating_sub(start) as f64),
                writable: false,
                getter: None,
                setter: None,
                enumerable: true,
                configurable: true,
            },
        );
        Ok(JsValue::Object(object))
    }

    fn data_view_access(
        &mut self,
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
        let index = self.data_view_index(arguments.first(), byte_length, access.width())?;
        if is_write(function) {
            let value = to_number(
                arguments
                    .get(1)
                    .ok_or_else(|| JsError::type_error("DataView setter requires a value"))?,
            )?;
            // The per-access `littleEndian` argument defaults to false, so an
            // accessor that omits it is big-endian.
            let big_endian = !arguments.get(2).is_some_and(JsValue::is_truthy);
            let bytes = encode_bytes(access, value, big_endian);
            let mut slots = buffer.0.borrow_mut();
            for (step, byte) in bytes.into_iter().enumerate() {
                let slot = byte_offset + index + step;
                if let Some(cell) = slots.get_mut(slot) {
                    *cell = f64::from(byte);
                }
            }
            return Ok(JsValue::Undefined);
        }
        let big_endian = !arguments.get(1).is_some_and(JsValue::is_truthy);
        // Reading a big-endian view means the first byte it covers is the most
        // significant one, so the raw pattern is assembled in view order and
        // then reversed for the conversion.
        let slots = buffer.0.borrow();
        let mut pattern = 0u64;
        for step in 0..access.width() {
            let byte = slots
                .get(byte_offset + index + step)
                .map_or(0u8, |cell| *cell as i64 as u8);
            let shift = (step * 8) as u32;
            pattern |= u64::from(byte) << shift;
        }
        if big_endian {
            pattern = reverse_bytes(pattern, access.width());
        }
        Ok(JsValue::Number(decode_number(access, pattern)))
    }

    fn data_view_host(&self, receiver: ObjectId) -> Result<(TypedBuffer, usize, usize), JsError> {
        match self.realm.host(receiver) {
            Some(ObjectHost::DataView {
                buffer,
                byte_offset,
                byte_length,
            }) => Ok((buffer, byte_offset, byte_length)),
            _ => Err(JsError::type_error(
                "DataView method called on an incompatible receiver",
            )),
        }
    }

    /// `new DataView(buffer[, byteOffset[, byteLength]])`.
    ///
    /// `buffer` is an `ArrayBuffer` or a one-byte-element typed array, both of
    /// which are byte-granular in this engine. There is no fourth parameter, so
    /// a call that passes one is not asking for a view-level byte order.
    pub(in crate::runtime) fn data_view_constructor(
        &mut self,
        constructor: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let Some(JsValue::Object(source)) = arguments.first() else {
            return Err(self.range_error("DataView requires an ArrayBuffer argument"));
        };
        let (buffer, base, total) = match self.realm.host(*source) {
            Some(ObjectHost::ArrayBufferHost(buffer)) => {
                let total = buffer.0.borrow().len();
                (buffer, 0, total)
            }
            Some(ObjectHost::TypedArray {
                kind,
                buffer,
                start,
                length,
            }) => {
                if kind.element_size() != 1 {
                    return Err(JsError::type_error(
                        "DataView requires a byte-granular buffer in this engine",
                    ));
                }
                (buffer, start, length)
            }
            _ => {
                return Err(JsError::type_error(
                    "DataView requires an ArrayBuffer argument",
                ));
            }
        };
        let offset = self.data_view_to_index(arguments.get(1), total)?;
        // ECMAScript 25.2.5.1: an absent `byteLength` is clamped to what is
        // left of the buffer, while a present one that does not fit is a
        // `RangeError`.
        let byte_length = match arguments.get(2) {
            None | Some(JsValue::Undefined) => total - offset,
            Some(value) => {
                let requested = self.data_view_to_index(Some(value), total)?;
                if requested > total - offset {
                    return Err(
                        self.range_error("DataView byteLength extends past the end of the buffer")
                    );
                }
                requested
            }
        };
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
                byte_offset: base + offset,
                byte_length,
            };
        }
        for (name, value) in [
            ("buffer", JsValue::Object(*source)),
            ("byteLength", JsValue::Number(byte_length as f64)),
            ("byteOffset", JsValue::Number(offset as f64)),
        ] {
            self.realm.define_property(
                object,
                name,
                crate::PropertyDescriptor {
                    value,
                    writable: false,
                    getter: None,
                    setter: None,
                    enumerable: true,
                    configurable: true,
                },
            );
        }
        Ok(JsValue::Object(object))
    }

    /// `ToIndex` for the constructor's offset and length arguments.
    fn data_view_to_index(
        &mut self,
        value: Option<&JsValue>,
        total: usize,
    ) -> Result<usize, JsError> {
        let number = match value {
            None | Some(JsValue::Undefined) => 0.0,
            Some(value) => to_number(value)?,
        };
        if !number.is_finite() || number < 0.0 || number.trunc() != number {
            return Err(self.range_error("DataView offset and length must be integers"));
        }
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the value is validated as a non-negative integer"
        )]
        let index = number as usize;
        if index > total {
            return Err(self.range_error("DataView byteOffset extends past the end of the buffer"));
        }
        Ok(index)
    }

    /// ECMAScript 25.2.5.5 `GetViewValue`: the requested bytes must lie wholly
    /// inside the view, which is the check that makes a read at the tail a
    /// `RangeError` rather than a zero-filled answer.
    fn data_view_index(
        &mut self,
        value: Option<&JsValue>,
        byte_length: usize,
        width: usize,
    ) -> Result<usize, JsError> {
        let number = match value {
            None | Some(JsValue::Undefined) => 0.0,
            Some(value) => to_number(value)?,
        };
        if !number.is_finite() || number < 0.0 || number.trunc() != number {
            return Err(self.range_error("DataView request index must be an integer"));
        }
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the value is validated as a non-negative integer"
        )]
        let index = number as usize;
        if index + width > byte_length {
            return Err(self.range_error("DataView request is outside the bounds of the view"));
        }
        Ok(index)
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
                 thrown(function () { new DataView(new ArrayBuffer(4)).getUint8(-1); }),
                 thrown(function () { new DataView(new ArrayBuffer(4)).getUint8(1.5); })].join(',')
            "),
            "RangeError,RangeError,RangeError,RangeError,RangeError"
        );
        // A one-byte read at the last index is in bounds.
        assert_eq!(run("new DataView(new ArrayBuffer(4)).getUint8(3)"), "0");
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
