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

use std::cmp::Ordering;

use crate::JsBigInt;
use crate::JsError;
use crate::JsValue;
use crate::parser::BinaryOp;
use crate::runtime::builtins::bigint::StringToBigInt;
use crate::runtime::builtins::bigint::string_to_bigint;

impl JsValue {
    pub(super) fn is_truthy(&self) -> bool {
        match self {
            Self::Undefined | Self::Null => false,
            Self::Boolean(value) => *value,
            Self::Number(value) => *value != 0.0 && !value.is_nan(),
            Self::BigInt(value) => !value.is_zero(),
            Self::String(value) => !value.is_empty(),
            Self::Symbol(_) => true,
            Self::Object(_) => true,
        }
    }
}

pub(super) fn to_number(value: &JsValue) -> Result<f64, JsError> {
    match value {
        JsValue::Undefined => Ok(f64::NAN),
        JsValue::Null => Ok(0.0),
        JsValue::Symbol(_) => Err(JsError::type_error(
            "Cannot convert a Symbol value to a number",
        )),
        // ECMA-262 7.1.4 `ToNumber` has no BigInt case: it throws, and only
        // `Number(value)` and the explicit `ToNumeric` path convert a BigInt.
        JsValue::BigInt(_) => Err(JsError::type_error(
            "Cannot convert a BigInt value to a number",
        )),
        JsValue::Boolean(value) => Ok(u8::from(*value).into()),
        JsValue::Number(value) => Ok(*value),
        JsValue::String(value) => {
            let value = value.trim();
            if value.is_empty() {
                Ok(0.0)
            } else {
                let radix_value = [
                    ("0x", 16),
                    ("0X", 16),
                    ("0b", 2),
                    ("0B", 2),
                    ("0o", 8),
                    ("0O", 8),
                ]
                .into_iter()
                .find_map(|(prefix, radix)| {
                    value.strip_prefix(prefix).map(|digits| {
                        u64::from_str_radix(digits, radix).map_or(f64::NAN, |number| number as f64)
                    })
                });
                Ok(radix_value.unwrap_or_else(|| value.parse().unwrap_or(f64::NAN)))
            }
        }
        JsValue::Object(_) => Err(JsError::type_error(
            "object-to-primitive numeric conversion is not implemented",
        )),
    }
}

pub(super) fn format_number_precision(value: f64, precision: usize) -> String {
    if !value.is_finite() {
        return crate::value::number_to_string(value);
    }
    if value == 0.0 {
        return if precision == 1 {
            "0".to_owned()
        } else {
            format!("0.{:0<width$}", "", width = precision - 1)
        };
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "finite binary64 base-10 exponents fit comfortably in i32"
    )]
    let exponent = value.abs().log10().floor() as i32;
    if exponent < -6 || exponent >= precision as i32 {
        let mut formatted = format!("{:.*e}", precision - 1, value);
        if let Some(marker) = formatted.find('e') {
            let raw_exponent = formatted[marker + 1..].parse::<i32>().unwrap_or(exponent);
            formatted.truncate(marker + 1);
            if raw_exponent >= 0 {
                formatted.push('+');
            }
            formatted.push_str(&raw_exponent.to_string());
        }
        formatted
    } else {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "precision is at most 100 and exponent selects fixed notation"
        )]
        let decimals = (precision as i32 - exponent - 1) as usize;
        format!("{value:.decimals$}")
    }
}

/// ECMA-262 §7.1.7 `ToUint32` on an already-numeric value.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub(super) fn uint32_of_number(number: f64) -> u32 {
    if !number.is_finite() {
        return 0;
    }
    number.trunc().rem_euclid(4_294_967_296.0) as u32
}

pub(super) fn to_uint32(value: &JsValue) -> Result<u32, JsError> {
    Ok(uint32_of_number(to_number(value)?))
}

#[allow(clippy::cast_possible_wrap)]
pub(super) fn to_int32(value: &JsValue) -> Result<i32, JsError> {
    Ok(to_uint32(value)? as i32)
}

pub(super) fn bitwise_binary(
    left: &JsValue,
    right: &JsValue,
    operation: impl FnOnce(i32, i32) -> i32,
) -> Result<JsValue, JsError> {
    Ok(JsValue::Number(f64::from(operation(
        to_int32(left)?,
        to_int32(right)?,
    ))))
}

pub(super) fn shift_count(value: &JsValue) -> Result<u32, JsError> {
    Ok(to_uint32(value)? & 0x1f)
}

pub(super) fn shift_left(left: &JsValue, right: &JsValue) -> Result<JsValue, JsError> {
    Ok(JsValue::Number(f64::from(
        to_int32(left)?.wrapping_shl(shift_count(right)?),
    )))
}

pub(super) fn shift_right(left: &JsValue, right: &JsValue) -> Result<JsValue, JsError> {
    Ok(JsValue::Number(f64::from(
        to_int32(left)? >> shift_count(right)?,
    )))
}

pub(super) fn unsigned_shift_right(left: &JsValue, right: &JsValue) -> Result<JsValue, JsError> {
    Ok(JsValue::Number(f64::from(
        to_uint32(left)? >> shift_count(right)?,
    )))
}

/// The truth of `left op right` for a relational operator over two primitives
/// (ECMA-262 13.10.1). An `undefined` comparison, from a NaN or a string that
/// is not a `BigInt`, makes all four operators false.
pub(super) fn relational_compare(
    operator: BinaryOp,
    left: &JsValue,
    right: &JsValue,
) -> Result<bool, JsError> {
    // `a > b` asks whether `b < a`, and `a <= b` is "not `b < a`". Negating
    // only the definite answer keeps `undefined` false in every operator.
    let (first, second, negate) = match operator {
        BinaryOp::Less => (left, right, false),
        BinaryOp::Greater => (right, left, false),
        BinaryOp::LessEqual => (right, left, true),
        BinaryOp::GreaterEqual => (left, right, true),
        _ => unreachable!("relational_compare takes only the relational operators"),
    };
    Ok(match less_than(first, second)? {
        Some(answer) => answer != negate,
        None => false,
    })
}

/// ECMA-262 7.2.13 `IsLessThan` over two primitives, `None` for `undefined`.
fn less_than(left: &JsValue, right: &JsValue) -> Result<Option<bool>, JsError> {
    Ok(match (left, right) {
        (JsValue::String(left), JsValue::String(right)) => Some(left < right),
        (JsValue::BigInt(left), JsValue::BigInt(right)) => Some(left < right),
        (JsValue::BigInt(left), JsValue::String(right)) => {
            bigint_from_text(right).map(|right| *left < right)
        }
        (JsValue::String(left), JsValue::BigInt(right)) => {
            bigint_from_text(left).map(|left| left < *right)
        }
        // A BigInt against a Number compares mathematical values exactly.
        (JsValue::BigInt(left), other) => left
            .cmp_f64(to_number(other)?)
            .map(|ordering| ordering == Ordering::Less),
        (other, JsValue::BigInt(right)) => right
            .cmp_f64(to_number(other)?)
            .map(|ordering| ordering == Ordering::Greater),
        _ => {
            let left = to_number(left)?;
            let right = to_number(right)?;
            (!left.is_nan() && !right.is_nan()).then_some(left < right)
        }
    })
}

/// `StringToBigInt` for a comparison or an equality test. A string that is not
/// a `BigInt` literal, or whose value is beyond the supported size, has no `BigInt`
/// to compare with, so the answer is `None`.
fn bigint_from_text(text: &str) -> Option<JsBigInt> {
    match string_to_bigint(text) {
        StringToBigInt::Value(value) => Some(value),
        StringToBigInt::Invalid | StringToBigInt::TooLarge => None,
    }
}

pub(super) fn strict_equal(left: &JsValue, right: &JsValue) -> bool {
    match (left, right) {
        (JsValue::Undefined, JsValue::Undefined) | (JsValue::Null, JsValue::Null) => true,
        (JsValue::Boolean(left), JsValue::Boolean(right)) => left == right,
        (JsValue::Number(left), JsValue::Number(right)) => number_equal(*left, *right),
        (JsValue::BigInt(left), JsValue::BigInt(right)) => left == right,
        (JsValue::String(left), JsValue::String(right)) => left == right,
        (JsValue::Symbol(left), JsValue::Symbol(right)) => left == right,
        (JsValue::Object(left), JsValue::Object(right)) => left == right,
        _ => false,
    }
}

#[allow(clippy::float_cmp)]
pub(super) fn number_equal(left: f64, right: f64) -> bool {
    // ECMAScript Number equality is exact IEEE-754 equality: NaN differs from
    // every value, while +0 and -0 compare equal. An epsilon comparison would
    // implement different language semantics.
    left == right
}

pub(super) fn same_value_zero(left: &JsValue, right: &JsValue) -> bool {
    match (left, right) {
        (JsValue::Number(left), JsValue::Number(right)) => {
            (left.is_nan() && right.is_nan()) || left == right
        }
        _ => left == right,
    }
}

pub(super) fn abstract_equal(left: &JsValue, right: &JsValue) -> Result<bool, JsError> {
    if strict_equal(left, right) {
        return Ok(true);
    }
    if matches!(
        (left, right),
        (JsValue::Null, JsValue::Undefined) | (JsValue::Undefined, JsValue::Null)
    ) {
        return Ok(true);
    }
    match (left, right) {
        (JsValue::Number(left), JsValue::String(_)) => Ok(number_equal(*left, to_number(right)?)),
        (JsValue::String(_), JsValue::Number(right)) => Ok(number_equal(to_number(left)?, *right)),
        // A BigInt equals a Number only when their mathematical values are
        // equal; NaN and the infinities match no BigInt.
        (JsValue::BigInt(left), JsValue::Number(right))
        | (JsValue::Number(right), JsValue::BigInt(left)) => {
            Ok(left.cmp_f64(*right) == Some(Ordering::Equal))
        }
        (JsValue::BigInt(left), JsValue::String(right))
        | (JsValue::String(right), JsValue::BigInt(left)) => {
            Ok(bigint_from_text(right).is_some_and(|right| right == *left))
        }
        (JsValue::Boolean(_), _) => abstract_equal(&JsValue::Number(to_number(left)?), right),
        (_, JsValue::Boolean(_)) => abstract_equal(left, &JsValue::Number(to_number(right)?)),
        _ => Ok(false),
    }
}

pub(super) fn required_argument<'a>(
    arguments: &'a [JsValue],
    index: usize,
    function: &str,
) -> Result<&'a JsValue, JsError> {
    arguments.get(index).ok_or_else(|| {
        JsError::type_error(format!(
            "{function} requires at least {} argument(s)",
            index.saturating_add(1)
        ))
    })
}

/// `ToIntegerOrInfinity` for an already-numeric value.
#[must_use]
pub(crate) fn integer_or_infinity(number: f64) -> f64 {
    if number.is_nan() {
        return 0.0;
    }
    number.trunc()
}

const RADIX_DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";

/// ECMA-262 6.1.6.1.9 `Number.prototype.toString` with a non-decimal radix.
///
/// The integer part is exact when binary64 holds it, and the fraction is
/// printed to the input's own precision: each step multiplies the remainder and
/// its "half an ulp" bound by the radix, and stops once the remainder falls
/// below that bound, rounding the final digit to even. That is the algorithm
/// every shipping engine uses, so `(0.5).toString(2)` is `0.1` and
/// `(1.5).toString(2)` is `1.1`, not a runaway digit tail.
pub(crate) fn number_to_radix_string(value: f64, radix: u32) -> String {
    debug_assert!((2..=36).contains(&radix));
    if radix == 10 {
        return crate::value::number_to_string(value);
    }
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value == 0.0 {
        return "0".to_owned();
    }
    if value.is_infinite() {
        return if value.is_sign_positive() {
            "Infinity".to_owned()
        } else {
            "-Infinity".to_owned()
        };
    }
    let negative = value.is_sign_negative();
    let magnitude = value.abs();
    let radix_value = f64::from(radix);
    let mut integer = magnitude.floor();
    let mut fraction = magnitude - integer;
    let mut fraction_digits: Vec<u8> = Vec::new();
    // Half an ulp of the input, never below the smallest denormal so a subnormal
    // still terminates.
    let mut delta = 0.5 * (next_double(magnitude) - magnitude);
    delta = delta.max(f64::from_bits(1));
    let mut round_up = false;
    while fraction >= delta {
        fraction *= radix_value;
        delta *= radix_value;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the scaled remainder is below the radix"
        )]
        let digit = fraction as u32;
        fraction -= f64::from(digit);
        fraction_digits.push(RADIX_DIGITS[digit as usize]);
        let half_even = fraction > 0.5 || (fraction == 0.5 && digit % 2 == 1);
        if half_even && fraction + delta > 1.0 {
            // The remaining fraction rounds the last digit up; carry after the
            // integer digits are known so the overflow reaches them too.
            round_up = true;
            break;
        }
    }
    let had_fraction = !fraction_digits.is_empty();
    let mut integer_digits: Vec<u8> = Vec::new();
    if integer <= 9_007_199_254_740_992.0 {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the bound above is 2^53 and fits in u64"
        )]
        let mut remaining = integer as u64;
        let base = u64::from(radix);
        loop {
            integer_digits.push(RADIX_DIGITS[(remaining % base) as usize]);
            remaining /= base;
            if remaining == 0 {
                break;
            }
        }
    } else {
        while integer >= 1.0 {
            integer /= radix_value;
            let remainder = integer - integer.floor();
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "the scaled remainder is below the radix"
            )]
            integer_digits.push(RADIX_DIGITS[(remainder * radix_value) as usize]);
        }
    }
    integer_digits.reverse();
    let mut all_digits = integer_digits;
    let fraction_length = fraction_digits.len();
    let integer_length = all_digits.len();
    all_digits.extend_from_slice(&fraction_digits);
    if round_up {
        increment_digits(&mut all_digits, radix);
    }
    // A carry out of the most significant digit is a new leading `1`.
    let overflow = all_digits.len() > integer_length + fraction_length;
    let mut output = Vec::with_capacity(all_digits.len() + 2);
    if negative {
        output.push(b'-');
    }
    if overflow {
        output.push(b'1');
    }
    let split = all_digits.len() - fraction_length;
    output.extend_from_slice(&all_digits[..split]);
    if had_fraction {
        output.push(b'.');
        output.extend_from_slice(&all_digits[split..]);
    }
    String::from_utf8(output).expect("radix digits are ASCII")
}

/// Add one to the least significant digit of a most-significant-first digit
/// string, propagating the carry toward the front. `digits` is never empty.
fn increment_digits(digits: &mut [u8], radix: u32) {
    for position in (0..digits.len()).rev() {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "radix digits are below 36"
        )]
        let value = u32::from(match digits[position] {
            digit @ b'0'..=b'9' => digit - b'0',
            digit @ b'a'..=b'z' => digit - b'a' + 10,
            _ => continue,
        });
        if value + 1 < radix {
            digits[position] = RADIX_DIGITS[(value + 1) as usize];
            return;
        }
        digits[position] = b'0';
    }
}

fn digit_value(digit: u8) -> Option<u32> {
    match digit {
        b'0'..=b'9' => Some(u32::from(digit - b'0')),
        b'a'..=b'z' => Some(u32::from(digit - b'a') + 10),
        _ => None,
    }
}

/// The next representable binary64 above `value`, for the `toString` fraction
/// precision bound.
fn next_double(value: f64) -> f64 {
    if value.is_nan() || value == f64::INFINITY {
        return f64::INFINITY;
    }
    if value == 0.0 {
        return f64::from_bits(1);
    }
    let bits = value.to_bits();
    // Negative values count down, so step away from zero.
    let stepped = if value < 0.0 {
        bits.wrapping_sub(1)
    } else {
        bits.wrapping_add(1)
    };
    f64::from_bits(stepped)
}

/// ECMA-262 7.1.1.1 `parseInt(string, radix)`. The radix argument was
/// previously ignored, which made every `parseInt(hex, 16)` in a bundle `NaN`.
pub(crate) fn parse_int(text: &str, radix: Option<&JsValue>) -> Result<f64, JsError> {
    let trimmed = text.trim_start_matches(|c: char| c.is_whitespace() || c.is_control());
    let (negative, digits_source) = match trimmed.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, trimmed.strip_prefix('+').unwrap_or(trimmed)),
    };
    // `R = ToInt32(radix)`, with 0 meaning "infer from the input".
    let mut radix_value = match radix {
        None | Some(JsValue::Undefined) => 0u32,
        Some(value) => to_int32(value)? as u32,
    };
    if !(2..=36).contains(&radix_value) && radix_value != 0 {
        return Ok(f64::NAN);
    }
    let mut body = digits_source;
    if (radix_value == 16 || radix_value == 0)
        && let Some(rest) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X"))
    {
        body = rest;
        radix_value = 16;
    }
    if radix_value == 0 {
        radix_value = 10;
    }
    let significant = body
        .strip_prefix('+')
        .or_else(|| body.strip_prefix('-'))
        .unwrap_or(body);
    let end = significant
        .chars()
        .position(|c| digit_value(c as u8).is_none_or(|value| value >= radix_value))
        .unwrap_or(significant.len());
    if end == 0 {
        return Ok(f64::NAN);
    }
    let mut magnitude = 0.0f64;
    for character in significant[..end].chars() {
        let digit = f64::from(digit_value(character as u8).unwrap_or_default());
        // Horner accumulation keeps full binary64 precision for long inputs
        // where `u64` would overflow.
        magnitude = magnitude * f64::from(radix_value) + digit;
    }
    Ok(if negative { -magnitude } else { magnitude })
}
