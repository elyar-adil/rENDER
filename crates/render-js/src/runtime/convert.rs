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
use crate::runtime::builtins::string::is_js_whitespace;

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

/// ECMA-262 7.1.4 `ToNumber` over a value that is already primitive.
///
/// This has no runtime, so it cannot run `ToPrimitive`. Every caller passes a
/// primitive: either a `ToPrimitive` result, or the output of `ToNumeric` on an
/// operand. An object must go through `JsRuntime::to_number_value`. The object arm
/// stays an error so a call site that missed that conversion fails loudly.
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
        JsValue::String(value) => Ok(string_to_number(&value.to_lossy())),
        JsValue::Object(_) => Err(JsError::type_error(
            "object-to-primitive numeric conversion is not implemented",
        )),
    }
}

/// ECMA-262 7.1.4.1.1 `StringToNumber`. The text is trimmed of JavaScript
/// white space and must match `StringNumericLiteral`: the empty string is `0`;
/// an unsigned `0x`/`0o`/`0b` prefix introduces an integer in that radix; and
/// otherwise an optional sign precedes `Infinity` (case-sensitive) or a
/// decimal literal. Anything else is `NaN`. Rust's `f64` parser also accepts
/// `inf`, `nan` and other spellings outside this grammar, so the decimal form
/// is checked against the grammar before parsing.
pub(crate) fn string_to_number(text: &str) -> f64 {
    let text = text.trim_matches(is_js_whitespace);
    if text.is_empty() {
        return 0.0;
    }
    let radix = match text.get(..2) {
        Some("0x" | "0X") => Some(16),
        Some("0o" | "0O") => Some(8),
        Some("0b" | "0B") => Some(2),
        _ => None,
    };
    if let Some(radix) = radix {
        return radix_integer_value(&text[2..], radix);
    }
    let (negative, unsigned) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let magnitude = if unsigned == "Infinity" {
        f64::INFINITY
    } else if is_decimal_literal(unsigned) {
        unsigned.parse::<f64>().unwrap_or(f64::NAN)
    } else {
        f64::NAN
    };
    if negative { -magnitude } else { magnitude }
}

/// The value of `digits` read in `radix`, rounded once to the nearest double.
/// Empty text or a digit outside the radix is `NaN`.
fn radix_integer_value(digits: &str, radix: u32) -> f64 {
    if digits.is_empty() || !digits.chars().all(|digit| digit.is_digit(radix)) {
        return f64::NAN;
    }
    match u128::from_str_radix(digits, radix) {
        Ok(number) => number as f64,
        Err(_) => digits.chars().fold(0.0, |value, digit| {
            value * f64::from(radix) + f64::from(digit.to_digit(radix).unwrap_or(0))
        }),
    }
}

/// `StrUnsignedDecimalLiteral` without `Infinity`: digits with an optional
/// fraction (`1`, `1.`, `.5`, `1.5`) and an optional exponent (`e`/`E`, an
/// optional sign, then at least one digit). At least one mantissa digit is
/// required.
fn is_decimal_literal(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut index = 0;
    let digits_from = |index: &mut usize| {
        let start = *index;
        while *index < bytes.len() && bytes[*index].is_ascii_digit() {
            *index += 1;
        }
        *index - start
    };
    let mut mantissa_digits = digits_from(&mut index);
    if bytes.get(index) == Some(&b'.') {
        index += 1;
        mantissa_digits += digits_from(&mut index);
    }
    if mantissa_digits == 0 {
        return false;
    }
    if matches!(bytes.get(index), Some(b'e' | b'E')) {
        index += 1;
        if matches!(bytes.get(index), Some(b'+' | b'-')) {
            index += 1;
        }
        if digits_from(&mut index) == 0 {
            return false;
        }
    }
    index == bytes.len()
}

/// Decimal digit strings for the `Number.prototype` formatting methods
/// (ECMA-262 21.1.3.2 `toExponential`, 21.1.3.3 `toFixed`, 21.1.3.5
/// `toPrecision`). Every rounding here is taken on the exact binary value of the
/// double, and a tie rounds up to the larger candidate, as those steps require.
/// Rust's own `{:.N}` formatting rounds ties to even, so it is not used for the
/// rounding decision.
///
/// The exact expansion of a finite double has at most 1074 fractional digits
/// and 767 significant digits, so 1100 places of Rust's exact formatting carry
/// every digit that can take part in a rounding decision.
const EXACT_PLACES: usize = 1100;

/// The significant digits of a finite, positive double in scientific form, as
/// `(digits, exponent)` where `value` is `d.ddd` times `10^exponent`. The
/// digits carry no leading zero, and the exponent is the power of its first.
fn scientific_digits(text: &str) -> (Vec<u8>, i32) {
    let (mantissa, exponent) = text.split_once('e').unwrap_or((text, "0"));
    let digits = mantissa
        .bytes()
        .filter(u8::is_ascii_digit)
        .map(|byte| byte - b'0')
        .collect();
    (digits, exponent.parse().unwrap_or(0))
}

/// Adds one to a base-10 digit string, prepending a leading `1` on carry.
fn increment_decimal(digits: &mut Vec<u8>) {
    for digit in digits.iter_mut().rev() {
        if *digit == 9 {
            *digit = 0;
        } else {
            *digit += 1;
            return;
        }
    }
    digits.insert(0, 1);
}

/// The `count` leading significant digits of a finite, positive double, rounded
/// half up, with the exponent of the first. Rounding up over a run of nines
/// carries into a new leading digit, which moves the exponent by one.
fn significant_digits(value: f64, count: usize) -> (Vec<u8>, i32) {
    let places = EXACT_PLACES;
    let exact = format!("{value:.places$e}");
    let (mut digits, mut exponent) = scientific_digits(&exact);
    let round_up = digits.get(count).is_some_and(|digit| *digit >= 5);
    digits.resize(count, 0);
    if round_up {
        increment_decimal(&mut digits);
        if digits.len() > count {
            digits.truncate(count);
            exponent += 1;
        }
    }
    (digits, exponent)
}

/// The digits of the shortest decimal that round-trips to a finite, positive
/// double, as `Number::toString` chooses them (ECMA-262 6.1.6.1.20).
fn shortest_significant_digits(value: f64) -> (Vec<u8>, i32) {
    scientific_digits(&format!("{value:e}"))
}

/// `Number.prototype.toExponential` for a finite double with `fraction_digits`
/// digits after the point, or with the shortest round-trip digits when that
/// argument is absent (ECMA-262 21.1.3.2 steps 9 to 14).
pub(super) fn format_exponential(value: f64, fraction_digits: Option<usize>) -> String {
    let negative = value < 0.0;
    let magnitude = value.abs();
    let (digits, exponent) = if magnitude == 0.0 {
        (vec![0; fraction_digits.map_or(1, |digits| digits + 1)], 0)
    } else if let Some(digits) = fraction_digits {
        significant_digits(magnitude, digits + 1)
    } else {
        shortest_significant_digits(magnitude)
    };
    let mut text = String::new();
    if negative {
        text.push('-');
    }
    text.push(char::from(b'0' + digits[0]));
    if digits.len() > 1 {
        text.push('.');
        text.extend(digits[1..].iter().map(|digit| char::from(b'0' + digit)));
    }
    push_exponent(&mut text, exponent);
    text
}

/// `Number.prototype.toPrecision` for a finite double and a precision in
/// `1..=100` (ECMA-262 21.1.3.5 steps 9 to 13).
pub(super) fn format_precision(value: f64, precision: usize) -> String {
    let negative = value < 0.0;
    let magnitude = value.abs();
    let (digits, exponent) = if magnitude == 0.0 {
        (vec![0; precision], 0)
    } else {
        significant_digits(magnitude, precision)
    };
    let digit_text: String = digits
        .iter()
        .map(|digit| char::from(b'0' + digit))
        .collect();
    let mut text = String::new();
    if negative {
        text.push('-');
    }
    let exponent_form = exponent < -6 || exponent >= precision as i32;
    if exponent_form {
        text.push_str(&digit_text[..1]);
        if precision != 1 {
            text.push('.');
            text.push_str(&digit_text[1..]);
        }
        push_exponent(&mut text, exponent);
    } else if exponent >= 0 {
        let point = exponent as usize + 1;
        text.push_str(&digit_text[..point]);
        if point < precision {
            text.push('.');
            text.push_str(&digit_text[point..]);
        }
    } else {
        text.push_str("0.");
        text.extend(std::iter::repeat_n(
            '0',
            exponent.unsigned_abs() as usize - 1,
        ));
        text.push_str(&digit_text);
    }
    text
}

/// `Number.prototype.toFixed` for a finite double with magnitude below 10^21
/// and `fraction_digits` in `0..=100` (ECMA-262 21.1.3.3 steps 8 to 11): the
/// integer `n` nearest `x * 10^f`, with a tie taking the larger `n`.
pub(super) fn format_fixed(value: f64, fraction_digits: usize) -> String {
    let negative = value < 0.0;
    let exact = format!("{:.EXACT_PLACES$}", value.abs());
    let (integer, fraction) = exact.split_once('.').unwrap_or((&exact, ""));
    let mut digits: Vec<u8> = integer
        .bytes()
        .map(|byte| byte - b'0')
        .chain(
            fraction
                .bytes()
                .chain(std::iter::repeat(b'0'))
                .take(fraction_digits)
                .map(|byte| byte - b'0'),
        )
        .collect();
    let round_up = fraction
        .as_bytes()
        .get(fraction_digits)
        .is_some_and(|byte| *byte >= b'5');
    if round_up {
        increment_decimal(&mut digits);
    }
    let integer_length = digits.len() - fraction_digits;
    let mut text = String::new();
    if negative {
        text.push('-');
    }
    text.extend(
        digits[..integer_length]
            .iter()
            .map(|digit| char::from(b'0' + digit)),
    );
    if fraction_digits > 0 {
        text.push('.');
        text.extend(
            digits[integer_length..]
                .iter()
                .map(|digit| char::from(b'0' + digit)),
        );
    }
    text
}

/// Appends the `e+N` or `e-N` suffix of an exponential form.
fn push_exponent(text: &mut String, exponent: i32) {
    text.push('e');
    text.push(if exponent < 0 { '-' } else { '+' });
    text.push_str(&exponent.unsigned_abs().to_string());
}

/// ECMA-262 §7.1.7 `ToUint32` on an already-numeric value.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub(super) fn uint32_of_number(number: f64) -> u32 {
    if !number.is_finite() {
        return 0;
    }
    number.trunc().rem_euclid(4_294_967_296.0) as u32
}

/// ECMA-262 7.1.7 `ToUint32` over a primitive. Its callers are the bitwise and
/// shift operators, which receive `ToNumeric` results, so no object reaches it.
/// An object operand needs `JsRuntime::to_number_value` first.
pub(super) fn to_uint32(value: &JsValue) -> Result<u32, JsError> {
    Ok(uint32_of_number(to_number(value)?))
}

/// ECMA-262 7.1.6 `ToInt32` over a primitive. Its callers are the bitwise
/// operators (through `ToNumeric`) and the unary `~`. `parseInt` converts its
/// radix through `JsRuntime::to_int32_value`, since the radix can be an object.
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
            bigint_from_text(&(right).to_lossy()).map(|right| *left < right)
        }
        (JsValue::String(left), JsValue::BigInt(right)) => {
            bigint_from_text(&(left).to_lossy()).map(|left| left < *right)
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

/// ECMA-262 7.2.14 `IsLooselyEqual` after the object operands are converted.
/// `JsRuntime::loose_equal` runs `ToPrimitive` on any object before it calls this,
/// so the `ToNumber` calls below only ever see primitives.
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
            Ok(bigint_from_text(&(right).to_lossy()).is_some_and(|right| right == *left))
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

/// ECMA-262 7.1.1.1 `parseInt(string, radix)`, over a string that is already
/// `ToString`'d and a radix that is already `ToInt32`'d. The caller converts
/// both, because each can be an object whose conversion runs user code. The
/// radix argument was previously ignored, which made every `parseInt(hex, 16)`
/// in a bundle `NaN`.
pub(crate) fn parse_int(text: &str, radix: i32) -> f64 {
    let trimmed = text.trim_start_matches(|c: char| c.is_whitespace() || c.is_control());
    let (negative, digits_source) = match trimmed.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, trimmed.strip_prefix('+').unwrap_or(trimmed)),
    };
    // `R = ToInt32(radix)`, with 0 meaning "infer from the input".
    let mut radix_value = radix as u32;
    if !(2..=36).contains(&radix_value) && radix_value != 0 {
        return f64::NAN;
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
        return f64::NAN;
    }
    let mut magnitude = 0.0f64;
    for character in significant[..end].chars() {
        let digit = f64::from(digit_value(character as u8).unwrap_or_default());
        // Horner accumulation keeps full binary64 precision for long inputs
        // where `u64` would overflow.
        magnitude = magnitude * f64::from(radix_value) + digit;
    }
    if negative { -magnitude } else { magnitude }
}
