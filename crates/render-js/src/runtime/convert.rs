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

impl JsValue {
    pub(super) fn is_truthy(&self) -> bool {
        match self {
            Self::Undefined | Self::Null => false,
            Self::Boolean(value) => *value,
            Self::Number(value) => *value != 0.0 && !value.is_nan(),
            Self::String(value) => !value.is_empty(),
            Self::Symbol(_) => true,
            Self::Object(_) => true,
        }
    }
}

/// Coerce an optional argument into a character index (negative counts from
/// the end, matching `String.prototype` slice semantics for the callers that
/// need it; `charAt`-style callers pass the raw value through).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "array indices stay far below any precision boundary"
)]
pub(super) fn optional_index(value: Option<&JsValue>) -> Result<f64, JsError> {
    match value {
        None | Some(JsValue::Undefined) => Ok(0.0),
        Some(other) => to_number(other),
    }
}

/// Resolve a slice/substring range: negative bounds count from the end and
/// are clamped. When `swap` is set (slice semantics) reversed bounds clamp to
/// an empty range; otherwise they are swapped (substring semantics).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "bounds are clamped before casting to usize"
)]
pub(super) fn slice_range(
    characters: &[char],
    start: f64,
    end: Option<f64>,
    swap: bool,
) -> std::ops::Range<usize> {
    let length = characters.len();
    let resolve = |value: f64| -> usize {
        if !value.is_finite() {
            return usize::MAX;
        }
        let mut value = value.floor();
        #[allow(clippy::cast_precision_loss, reason = "length fits exactly")]
        let length = length as f64;
        if value < 0.0 {
            value += length;
        }
        value.max(0.0).min(length) as usize
    };
    let mut start = resolve(start);
    let mut end = resolve(end.unwrap_or(if swap { f64::INFINITY } else { f64::MAX }));
    if end == usize::MAX {
        end = length;
    }
    if swap && end < start {
        return start..start;
    }
    if !swap && end < start {
        std::mem::swap(&mut start, &mut end);
    }
    start..end.min(length)
}

pub(super) fn to_number(value: &JsValue) -> Result<f64, JsError> {
    match value {
        JsValue::Undefined => Ok(f64::NAN),
        JsValue::Null => Ok(0.0),
        JsValue::Symbol(_) => Err(JsError::type_error(
            "Cannot convert a Symbol value to a number",
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

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub(super) fn to_uint32(value: &JsValue) -> Result<u32, JsError> {
    let number = to_number(value)?;
    if !number.is_finite() || number == 0.0 {
        return Ok(0);
    }
    let integer = number.trunc();
    let modulo = integer.rem_euclid(4_294_967_296.0);
    Ok(modulo as u32)
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

pub(super) fn compare(
    left: &JsValue,
    right: &JsValue,
    numeric: impl FnOnce(f64, f64) -> bool,
    string: impl FnOnce(&str, &str) -> bool,
) -> Result<JsValue, JsError> {
    if let (JsValue::String(left), JsValue::String(right)) = (left, right) {
        return Ok(JsValue::Boolean(string(left, right)));
    }
    Ok(JsValue::Boolean(numeric(
        to_number(left)?,
        to_number(right)?,
    )))
}

pub(super) fn strict_equal(left: &JsValue, right: &JsValue) -> bool {
    match (left, right) {
        (JsValue::Undefined, JsValue::Undefined) | (JsValue::Null, JsValue::Null) => true,
        (JsValue::Boolean(left), JsValue::Boolean(right)) => left == right,
        (JsValue::Number(left), JsValue::Number(right)) => number_equal(*left, *right),
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
