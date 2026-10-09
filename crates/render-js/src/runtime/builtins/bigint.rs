//! The `BigInt` function and `BigInt.prototype` (ECMA-262 21.2), plus the
//! `BigInt` conversions and arithmetic the operators share.

use crate::JsBigInt;
use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::bigint::digit_count_fits;
use crate::parser::BinaryOp;
use crate::runtime::JsRuntime;
use crate::runtime::builtins::string::is_js_whitespace;
use crate::runtime::eval::PrimitiveHint;
use crate::value::NativeFunction;
use crate::value::ObjectHost;
use render_dom::Dom;

/// Outcome of ECMA-262 7.1.14 `StringToBigInt` on one string.
pub(crate) enum StringToBigInt {
    /// The string is a `StringIntegerLiteral` and this is its value.
    Value(JsBigInt),
    /// The string is not a `StringIntegerLiteral` (`undefined` in the spec).
    Invalid,
    /// The string is valid but its value exceeds the supported size.
    TooLarge,
}

/// `StringToBigInt`: surrounding white space is ignored, the empty string is
/// `0n`, a `0x`/`0o`/`0b` prefix selects an unsigned radix, and otherwise an
/// optional sign precedes decimal digits. Separators and fractions are invalid.
pub(crate) fn string_to_bigint(text: &str) -> StringToBigInt {
    let text = text.trim_matches(is_js_whitespace);
    if text.is_empty() {
        return StringToBigInt::Value(JsBigInt::zero());
    }
    let (radix, digits, negative) = match text.get(..2) {
        Some("0x" | "0X") => (16, &text[2..], false),
        Some("0o" | "0O") => (8, &text[2..], false),
        Some("0b" | "0B") => (2, &text[2..], false),
        _ => match text.strip_prefix('-') {
            Some(rest) => (10, rest, true),
            None => (10, text.strip_prefix('+').unwrap_or(text), false),
        },
    };
    if !digit_count_fits(digits.len()) {
        return StringToBigInt::TooLarge;
    }
    match JsBigInt::parse_digits(digits, radix) {
        Some(value) if negative => StringToBigInt::Value(value.negate()),
        Some(value) => StringToBigInt::Value(value),
        None => StringToBigInt::Invalid,
    }
}

impl JsRuntime {
    /// `BigInt(value)` (ECMA-262 21.2.1.1). A Number converts only when it is
    /// an integer; every other value takes `ToBigInt`. `new BigInt()` never
    /// reaches here, because `BigInt` is not a constructor.
    pub(in crate::runtime) fn bigint_function(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let value = arguments.first().cloned().unwrap_or(JsValue::Undefined);
        let primitive = self.to_primitive_with_hint(dom, value, PrimitiveHint::Number)?;
        if let JsValue::Number(number) = primitive {
            return match JsBigInt::from_integral_f64(number) {
                Some(value) => Ok(JsValue::BigInt(value)),
                None => Err(self.range_error(&format!(
                    "The number {} cannot be converted to a BigInt because it is not an integer",
                    crate::value::number_to_string(number)
                ))),
            };
        }
        self.bigint_from_primitive(&primitive).map(JsValue::BigInt)
    }

    /// ECMA-262 7.1.13 `ToBigInt` for any value.
    pub(in crate::runtime) fn to_bigint_value(
        &mut self,
        dom: &mut Dom,
        value: &JsValue,
    ) -> Result<JsBigInt, JsError> {
        let primitive = self.to_primitive_with_hint(dom, value.clone(), PrimitiveHint::Number)?;
        self.bigint_from_primitive(&primitive)
    }

    /// `ToBigInt` on a value that has already been through `ToPrimitive`.
    /// A Number is a `TypeError` here, unlike the `BigInt` function's integer
    /// conversion.
    fn bigint_from_primitive(&mut self, primitive: &JsValue) -> Result<JsBigInt, JsError> {
        match primitive {
            JsValue::BigInt(value) => Ok(value.clone()),
            JsValue::Boolean(flag) => Ok(JsBigInt::from_i64(i64::from(*flag))),
            JsValue::String(text) => match string_to_bigint(text) {
                StringToBigInt::Value(value) => Ok(value),
                StringToBigInt::Invalid => Err(JsError::syntax(
                    format!("Cannot convert {text} to a BigInt"),
                    0,
                )),
                StringToBigInt::TooLarge => Err(self.range_error("Maximum BigInt size exceeded")),
            },
            JsValue::Number(_) => Err(JsError::type_error("Cannot convert a Number to a BigInt")),
            JsValue::Undefined | JsValue::Null => Err(JsError::type_error(
                "Cannot convert undefined or null to a BigInt",
            )),
            JsValue::Symbol(_) => Err(JsError::type_error(
                "Cannot convert a Symbol value to a BigInt",
            )),
            JsValue::Object(_) => Err(JsError::type_error("Cannot convert an object to a BigInt")),
        }
    }

    /// ECMA-262 7.1.22 `ToIndex`, the width argument of `BigInt.asIntN`.
    fn bigint_index(&mut self, dom: &mut Dom, value: Option<&JsValue>) -> Result<u64, JsError> {
        let integer = self.optional_integer_value(dom, value)?;
        if !(0.0..=9_007_199_254_740_991.0).contains(&integer) {
            return Err(self.range_error("Invalid value: not (convertible to) a safe integer"));
        }
        // The range check above keeps the value in u64 range.
        Ok(integer as u64)
    }

    /// The `BigInt` arm of the numeric operators (ECMA-262 6.1.6.2). Both
    /// operands are `BigInts`; mixing with a Number is rejected by the caller.
    pub(in crate::runtime) fn bigint_binary_operation(
        &mut self,
        operator: BinaryOp,
        left: &JsBigInt,
        right: &JsBigInt,
    ) -> Result<JsValue, JsError> {
        let value = match operator {
            BinaryOp::Add => left.add(right),
            BinaryOp::Subtract => left.sub(right),
            BinaryOp::Multiply => left.mul(right),
            BinaryOp::Divide | BinaryOp::Remainder => {
                let Some((quotient, remainder)) = left.div_rem(right) else {
                    return Err(self.range_error("Division by zero"));
                };
                if operator == BinaryOp::Divide {
                    quotient
                } else {
                    remainder
                }
            }
            BinaryOp::Exponentiate => {
                if right.is_negative() {
                    return Err(self.range_error("Exponent must be non-negative"));
                }
                match left.pow(right) {
                    Some(value) => value,
                    None => return Err(self.range_error("Maximum BigInt size exceeded")),
                }
            }
            BinaryOp::BitwiseAnd => left.bit_and(right),
            BinaryOp::BitwiseOr => left.bit_or(right),
            BinaryOp::BitwiseXor => left.bit_xor(right),
            BinaryOp::LeftShift | BinaryOp::RightShift => {
                // `x >> y` is `x << -y`; both directions share the size bound.
                let count = if operator == BinaryOp::RightShift {
                    right.negate()
                } else {
                    right.clone()
                };
                match left.shift(&count) {
                    Some(value) => value,
                    None => return Err(self.range_error("Maximum BigInt size exceeded")),
                }
            }
            BinaryOp::UnsignedRightShift => {
                return Err(JsError::type_error(
                    "BigInts have no unsigned right shift, use >> instead",
                ));
            }
            _ => unreachable!("only BigInt arithmetic operators reach here"),
        };
        Ok(JsValue::BigInt(value))
    }

    /// `BigInt.asIntN` and `BigInt.asUintN` (ECMA-262 21.2.2.1 and 21.2.2.2):
    /// the width is converted with `ToIndex` before the value with `ToBigInt`.
    fn bigint_as_n(
        &mut self,
        dom: &mut Dom,
        signed: bool,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let bits = self.bigint_index(dom, arguments.first())?;
        let value = self.to_bigint_value(dom, arguments.get(1).unwrap_or(&JsValue::Undefined))?;
        let wrapped = if signed {
            value.as_int_n(bits)
        } else {
            value.as_uint_n(bits)
        };
        match wrapped {
            Some(value) => Ok(JsValue::BigInt(value)),
            None => Err(self.range_error("Maximum BigInt size exceeded")),
        }
    }

    /// `thisBigIntValue` (ECMA-262 21.2.3): the primitive behind a `BigInt`
    /// wrapper, or a `TypeError` for any other receiver.
    fn this_bigint_value(&self, receiver: ObjectId, method: &str) -> Result<JsBigInt, JsError> {
        match self.realm.host(receiver) {
            Some(ObjectHost::BigIntPrimitive(value)) => Ok(value),
            _ => Err(JsError::type_error(format!(
                "BigInt.prototype.{method} requires that 'this' be a BigInt"
            ))),
        }
    }

    pub(in crate::runtime) fn dispatch_bigint_native(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match function {
            NativeFunction::BigIntAsIntN => self.bigint_as_n(dom, true, arguments),
            NativeFunction::BigIntAsUintN => self.bigint_as_n(dom, false, arguments),
            NativeFunction::BigIntValueOf => self
                .this_bigint_value(receiver, "valueOf")
                .map(JsValue::BigInt),
            // ECMA-262 21.2.3.3: the radix is ToIntegerOrInfinity'd and must lie
            // in 2..=36; an absent radix means 10.
            NativeFunction::BigIntToString => {
                let value = self.this_bigint_value(receiver, "toString")?;
                let radix = match arguments.first() {
                    None | Some(JsValue::Undefined) => 10.0,
                    Some(argument) => self.to_integer_value(dom, argument)?,
                };
                if !(2.0..=36.0).contains(&radix) {
                    return Err(self.range_error("toString() radix must be between 2 and 36"));
                }
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "the range check above bounds the radix to 2..=36"
                )]
                let radix = radix as u32;
                Ok(JsValue::String(value.to_string_radix(radix)))
            }
            _ => unreachable!("dispatch_bigint_native only receives BigInt built-ins"),
        }
    }
}
