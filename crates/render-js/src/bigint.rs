//! Arbitrary-precision signed integers for the `BigInt` primitive (ECMA-262
//! 6.1.6.2).
//!
//! A value is a sign and a little-endian magnitude of base-2^32 limbs. The
//! magnitude never has a trailing zero limb and zero is never negative, so
//! derived equality is mathematical equality.
//!
//! Operations that can grow a value (`**`, `<<`, `BigInt.asUintN`) refuse to
//! produce more than [`MAX_BITS`] bits. The spec leaves the size limit to the
//! implementation; the runtime reports the refusal as a `RangeError`.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::float_cmp,
    reason = "limb arithmetic truncates and widens deliberately, and Number edges are exact IEEE comparisons"
)]

use std::cmp::Ordering;

/// Largest magnitude, in bits, that any operation may produce.
pub(crate) const MAX_BITS: u64 = 1 << 20;

const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";

/// Whether a run of `count` digits, in any radix up to 16, stays within
/// [`MAX_BITS`]. Checked before any digit is folded in, so an oversized
/// literal or string costs nothing to reject.
pub(crate) fn digit_count_fits(count: usize) -> bool {
    (count as u64).saturating_mul(4) <= MAX_BITS
}

/// A `BigInt` value: an exact integer of any size up to [`MAX_BITS`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsBigInt {
    negative: bool,
    limbs: Vec<u32>,
}

impl JsBigInt {
    /// The `BigInt` `0n`.
    #[must_use]
    pub fn zero() -> Self {
        Self {
            negative: false,
            limbs: Vec::new(),
        }
    }

    /// The exact `BigInt` for a signed 64-bit integer.
    #[must_use]
    pub fn from_i64(value: i64) -> Self {
        Self::from_parts(value < 0, Self::from_u64(value.unsigned_abs()).limbs)
    }

    /// The exact `BigInt` for an unsigned 64-bit integer.
    #[must_use]
    pub fn from_u64(value: u64) -> Self {
        Self::from_parts(false, vec![value as u32, (value >> 32) as u32])
    }

    /// The exact `BigInt` for an integral, finite Number. `None` for a
    /// fractional or non-finite value.
    #[must_use]
    pub fn from_integral_f64(value: f64) -> Option<Self> {
        if !value.is_finite() || value.fract() != 0.0 {
            return None;
        }
        if value == 0.0 {
            return Some(Self::zero());
        }
        let bits = value.to_bits();
        let negative = bits >> 63 == 1;
        let exponent = ((bits >> 52) & 0x7ff) as i64;
        let fraction = bits & ((1 << 52) - 1);
        let (mantissa, exponent) = if exponent == 0 {
            (fraction, -1074)
        } else {
            (fraction | (1 << 52), exponent - 1075)
        };
        // An integral value has no set bits below the binary point, so a
        // right shift by the negative exponent drops only zeros.
        let magnitude = if exponent >= 0 {
            shl_magnitude(&Self::from_u64(mantissa).limbs, exponent as u64)
        } else {
            shr_magnitude(&Self::from_u64(mantissa).limbs, exponent.unsigned_abs())
        };
        Some(Self::from_parts(negative, magnitude))
    }

    /// Parse a run of digits in `radix` (2 to 36). The run takes no sign,
    /// prefix or separator. `None` for an empty run or a digit outside the
    /// radix.
    #[must_use]
    pub fn parse_digits(digits: &str, radix: u32) -> Option<Self> {
        if digits.is_empty() {
            return None;
        }
        let mut limbs = Vec::new();
        // Accumulate a chunk of digits that fits one limb, then fold it in
        // with one multiply-add: linear work per chunk, not per digit.
        let mut chunk_value = 0_u32;
        let mut chunk_base = 1_u32;
        for character in digits.chars() {
            let digit = character.to_digit(radix)?;
            if chunk_base > u32::MAX / radix {
                mul_add_small(&mut limbs, chunk_base, chunk_value);
                chunk_value = 0;
                chunk_base = 1;
            }
            chunk_value = chunk_value * radix + digit;
            chunk_base *= radix;
        }
        mul_add_small(&mut limbs, chunk_base, chunk_value);
        Some(Self::from_parts(false, limbs))
    }

    /// Whether this value is `0n`.
    #[must_use]
    pub fn is_zero(&self) -> bool {
        self.limbs.is_empty()
    }

    /// Whether this value is strictly below zero.
    #[must_use]
    pub const fn is_negative(&self) -> bool {
        self.negative
    }

    /// The value as a `u64`, for a magnitude that fits one. Used for shift
    /// counts and bit widths, where anything larger is out of range anyway.
    #[must_use]
    pub fn magnitude_u64(&self) -> Option<u64> {
        match self.limbs.as_slice() {
            [] => Some(0),
            [low] => Some(u64::from(*low)),
            [low, high] => Some((u64::from(*high) << 32) | u64::from(*low)),
            _ => None,
        }
    }

    /// Render the value in `radix` (2 to 36) with a leading `-` when negative.
    #[must_use]
    pub fn to_string_radix(&self, radix: u32) -> String {
        if self.is_zero() {
            return "0".to_owned();
        }
        // Divide by the largest power of the radix that fits one limb, so
        // each division peels off several digits at once.
        let mut chunk = radix;
        let mut width = 1_usize;
        while chunk <= u32::MAX / radix {
            chunk *= radix;
            width += 1;
        }
        let mut limbs = self.limbs.clone();
        let mut groups = Vec::new();
        while !limbs.is_empty() {
            groups.push(div_small(&mut limbs, chunk));
        }
        let mut output = String::new();
        if self.negative {
            output.push('-');
        }
        // The most significant group carries no leading zeros; every lower
        // group is padded to the full chunk width.
        for (index, group) in groups.iter().rev().enumerate() {
            let mut value = *group;
            let mut digits = Vec::with_capacity(width);
            while value > 0 {
                digits.push(char::from(DIGITS[(value % radix) as usize]));
                value /= radix;
            }
            if index > 0 {
                digits.resize(width, '0');
            }
            output.extend(digits.iter().rev());
        }
        output
    }

    /// The Number nearest to this value, rounding half to even (ECMA-262
    /// `Number(bigint)`). Values past the Number range become infinities.
    #[must_use]
    pub fn to_f64(&self) -> f64 {
        let bits = bit_length(&self.limbs);
        let magnitude = if bits <= 64 {
            self.magnitude_u64().unwrap_or(0) as f64
        } else {
            let shift = bits - 64;
            if shift > 1023 {
                f64::INFINITY
            } else {
                let top = shr_magnitude(&self.limbs, shift);
                let top = top.first().copied().unwrap_or(0) as u64
                    | (u64::from(top.get(1).copied().unwrap_or(0)) << 32);
                // Any bit below the 64-bit window is folded into bit 0. That
                // is below the rounding bit of the 53-bit result, so it
                // breaks ties exactly as the discarded bits would.
                let sticky = (0..shift).any(|index| bit(&self.limbs, index));
                let mantissa = top | u64::from(sticky);
                // 2^shift is exact as a binary64 power of two.
                mantissa as f64 * f64::from_bits((shift + 1023) << 52)
            }
        };
        if self.negative { -magnitude } else { magnitude }
    }

    /// The exact ordering of this integer against a Number. `None` when the
    /// Number is `NaN`, which orders with nothing.
    #[must_use]
    pub fn cmp_f64(&self, number: f64) -> Option<Ordering> {
        if number.is_nan() {
            return None;
        }
        if number == f64::INFINITY {
            return Some(Ordering::Less);
        }
        if number == f64::NEG_INFINITY {
            return Some(Ordering::Greater);
        }
        let floor = number.floor();
        // `floor` is integral and finite here, so the conversion is exact.
        let floor_value = Self::from_integral_f64(floor)?;
        Some(match self.cmp(&floor_value) {
            // A fractional Number sits strictly above its floor.
            Ordering::Equal if number != floor => Ordering::Less,
            ordering => ordering,
        })
    }

    /// `-self`.
    #[must_use]
    pub fn negate(&self) -> Self {
        Self::from_parts(!self.negative, self.limbs.clone())
    }

    /// `self + other`.
    #[must_use]
    pub fn add(&self, other: &Self) -> Self {
        if self.negative == other.negative {
            return Self::from_parts(self.negative, add_magnitude(&self.limbs, &other.limbs));
        }
        match cmp_magnitude(&self.limbs, &other.limbs) {
            Ordering::Less => {
                Self::from_parts(other.negative, sub_magnitude(&other.limbs, &self.limbs))
            }
            _ => Self::from_parts(self.negative, sub_magnitude(&self.limbs, &other.limbs)),
        }
    }

    /// `self - other`.
    #[must_use]
    pub fn sub(&self, other: &Self) -> Self {
        self.add(&other.negate())
    }

    /// `self * other`.
    #[must_use]
    pub fn mul(&self, other: &Self) -> Self {
        Self::from_parts(
            self.negative != other.negative,
            mul_magnitude(&self.limbs, &other.limbs),
        )
    }

    /// Truncating division and its remainder, which takes the sign of the
    /// dividend (ECMA-262 `/` and `%` on `BigInt`). `None` for a zero divisor.
    #[must_use]
    pub fn div_rem(&self, other: &Self) -> Option<(Self, Self)> {
        if other.is_zero() {
            return None;
        }
        let (quotient, remainder) = divmod_magnitude(&self.limbs, &other.limbs);
        Some((
            Self::from_parts(self.negative != other.negative, quotient),
            Self::from_parts(self.negative, remainder),
        ))
    }

    /// `self ** exponent` for a non-negative exponent. `None` when the result
    /// would exceed [`MAX_BITS`].
    #[must_use]
    pub fn pow(&self, exponent: &Self) -> Option<Self> {
        if exponent.is_zero() {
            return Some(Self::from_i64(1));
        }
        // |base| of 0 or 1 keeps the magnitude fixed, so the exponent can be
        // any size: only its parity matters for -1.
        if self.is_zero() || self.limbs.as_slice() == [1] {
            let odd = exponent.limbs.first().is_some_and(|low| low & 1 == 1);
            return Some(if self.negative && odd {
                self.clone()
            } else {
                Self::from_parts(false, self.limbs.clone())
            });
        }
        let mut exponent = exponent.magnitude_u64()?;
        // With |base| >= 2 the result has at least `exponent` more bits than
        // the bit length of the base's magnitude, so this bound is exact enough.
        let base_bits = u128::from(bit_length(&self.limbs));
        if base_bits * u128::from(exponent) > u128::from(MAX_BITS) {
            return None;
        }
        let mut result = Self::from_i64(1);
        let mut base = self.clone();
        while exponent > 0 {
            if exponent & 1 == 1 {
                result = result.mul(&base);
            }
            exponent >>= 1;
            if exponent > 0 {
                base = base.mul(&base);
            }
        }
        Some(result)
    }

    /// `self << count`. A negative `count` shifts right, rounding toward
    /// negative infinity. `None` when a left shift would exceed [`MAX_BITS`].
    #[must_use]
    pub fn shift(&self, count: &Self) -> Option<Self> {
        if self.is_zero() {
            return Some(Self::zero());
        }
        if count.negative {
            // Past the width of the value a right shift yields 0 or -1, so an
            // oversized count can clamp to the largest representable one.
            return Some(self.shift_right(count.magnitude_u64().unwrap_or(u64::MAX)));
        }
        let bits = count.magnitude_u64()?;
        if bit_length(&self.limbs).saturating_add(bits) > MAX_BITS {
            return None;
        }
        Some(Self::from_parts(
            self.negative,
            shl_magnitude(&self.limbs, bits),
        ))
    }

    /// Floor division by `2^bits`: `self >> bits` with arithmetic semantics.
    fn shift_right(&self, bits: u64) -> Self {
        if !self.negative {
            return Self::from_parts(false, shr_magnitude(&self.limbs, bits));
        }
        // floor(-m / 2^k) = -(floor((m - 1) / 2^k) + 1) for m >= 1.
        let shifted = shr_magnitude(&sub_magnitude(&self.limbs, &[1]), bits);
        Self::from_parts(true, add_magnitude(&shifted, &[1]))
    }

    /// `~self`, which is `-self - 1` in two's complement.
    #[must_use]
    pub fn bit_not(&self) -> Self {
        self.negate().sub(&Self::from_i64(1))
    }

    /// `self & other` with two's-complement semantics.
    #[must_use]
    pub fn bit_and(&self, other: &Self) -> Self {
        self.bitwise(other, |left, right| left & right)
    }

    /// `self | other` with two's-complement semantics.
    #[must_use]
    pub fn bit_or(&self, other: &Self) -> Self {
        self.bitwise(other, |left, right| left | right)
    }

    /// `self ^ other` with two's-complement semantics.
    #[must_use]
    pub fn bit_xor(&self, other: &Self) -> Self {
        self.bitwise(other, |left, right| left ^ right)
    }

    /// `BigInt.asUintN`: `self mod 2^bits`. `None` when the result would
    /// exceed [`MAX_BITS`] bits.
    #[must_use]
    pub fn as_uint_n(&self, bits: u64) -> Option<Self> {
        if bits == 0 {
            return Some(Self::zero());
        }
        if !self.negative && bit_length(&self.limbs) <= bits {
            return Some(self.clone());
        }
        if bits > MAX_BITS {
            return None;
        }
        // The low `bits` of the two's-complement form are `self mod 2^bits`.
        let mask = Self::from_parts(false, shl_magnitude(&[1], bits)).sub(&Self::from_i64(1));
        Some(self.bit_and(&mask))
    }

    /// `BigInt.asIntN`: `self` wrapped into `[-2^(bits-1), 2^(bits-1))`.
    /// `None` when the result would exceed [`MAX_BITS`] bits.
    #[must_use]
    pub fn as_int_n(&self, bits: u64) -> Option<Self> {
        if bits == 0 {
            return Some(Self::zero());
        }
        // A value already in range has fewer than `bits` bits, where a
        // negative value counts its magnitude minus one (two's complement).
        let width = if self.negative {
            bit_length(&sub_magnitude(&self.limbs, &[1]))
        } else {
            bit_length(&self.limbs)
        };
        if width < bits {
            return Some(self.clone());
        }
        let wrapped = self.as_uint_n(bits)?;
        if bit_length(&wrapped.limbs) == bits {
            Some(wrapped.sub(&Self::from_parts(false, shl_magnitude(&[1], bits))))
        } else {
            Some(wrapped)
        }
    }

    fn bitwise(&self, other: &Self, operation: impl Fn(u32, u32) -> u32) -> Self {
        // One limb past the longer operand holds the sign extension of both.
        let length = self.limbs.len().max(other.limbs.len()) + 1;
        let left = self.twos_complement(length);
        let right = other.twos_complement(length);
        Self::from_twos_complement(
            left.iter()
                .zip(&right)
                .map(|(left, right)| operation(*left, *right))
                .collect(),
        )
    }

    /// The low `length` limbs of this value in two's complement. `length`
    /// must cover the value's magnitude plus one sign limb.
    fn twos_complement(&self, length: usize) -> Vec<u32> {
        let mut limbs = if self.negative {
            sub_magnitude(&self.limbs, &[1])
        } else {
            self.limbs.clone()
        };
        limbs.resize(length, 0);
        if self.negative {
            for limb in &mut limbs {
                *limb = !*limb;
            }
        }
        limbs
    }

    fn from_twos_complement(mut limbs: Vec<u32>) -> Self {
        let negative = limbs.last().is_some_and(|top| top & 0x8000_0000 != 0);
        if negative {
            for limb in &mut limbs {
                *limb = !*limb;
            }
            limbs = add_magnitude(&limbs, &[1]);
        }
        Self::from_parts(negative, limbs)
    }

    /// Build a normalized value: trailing zero limbs are dropped and zero is
    /// never negative.
    fn from_parts(negative: bool, mut limbs: Vec<u32>) -> Self {
        trim(&mut limbs);
        Self {
            negative: negative && !limbs.is_empty(),
            limbs,
        }
    }
}

impl Ord for JsBigInt {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.negative, other.negative) {
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
            (false, false) => cmp_magnitude(&self.limbs, &other.limbs),
            (true, true) => cmp_magnitude(&other.limbs, &self.limbs),
        }
    }
}

impl PartialOrd for JsBigInt {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn trim(limbs: &mut Vec<u32>) {
    while limbs.last() == Some(&0) {
        limbs.pop();
    }
}

fn bit_length(limbs: &[u32]) -> u64 {
    limbs.last().map_or(0, |top| {
        (limbs.len() as u64 - 1) * 32 + u64::from(32 - top.leading_zeros())
    })
}

fn bit(limbs: &[u32], index: u64) -> bool {
    limbs
        .get((index / 32) as usize)
        .is_some_and(|limb| (limb >> (index % 32)) & 1 == 1)
}

fn cmp_magnitude(left: &[u32], right: &[u32]) -> Ordering {
    left.len()
        .cmp(&right.len())
        .then_with(|| left.iter().rev().cmp(right.iter().rev()))
}

fn add_magnitude(left: &[u32], right: &[u32]) -> Vec<u32> {
    let (long, short) = if left.len() >= right.len() {
        (left, right)
    } else {
        (right, left)
    };
    let mut output = Vec::with_capacity(long.len() + 1);
    let mut carry = 0_u64;
    for (index, limb) in long.iter().enumerate() {
        let sum = u64::from(*limb) + u64::from(short.get(index).copied().unwrap_or(0)) + carry;
        output.push(sum as u32);
        carry = sum >> 32;
    }
    if carry != 0 {
        output.push(carry as u32);
    }
    trim(&mut output);
    output
}

/// `left - right` for `left >= right`.
fn sub_magnitude(left: &[u32], right: &[u32]) -> Vec<u32> {
    let mut output = Vec::with_capacity(left.len());
    let mut borrow = 0_i64;
    for (index, limb) in left.iter().enumerate() {
        let mut difference =
            i64::from(*limb) - i64::from(right.get(index).copied().unwrap_or(0)) - borrow;
        borrow = i64::from(difference < 0);
        if difference < 0 {
            difference += 1 << 32;
        }
        output.push(difference as u32);
    }
    trim(&mut output);
    output
}

fn mul_magnitude(left: &[u32], right: &[u32]) -> Vec<u32> {
    if left.is_empty() || right.is_empty() {
        return Vec::new();
    }
    let mut output = vec![0_u32; left.len() + right.len()];
    for (i, left_limb) in left.iter().enumerate() {
        let mut carry = 0_u64;
        for (j, right_limb) in right.iter().enumerate() {
            // The largest partial sum, (2^32-1)^2 + 2(2^32-1), still fits 64 bits.
            let total =
                u64::from(*left_limb) * u64::from(*right_limb) + u64::from(output[i + j]) + carry;
            output[i + j] = total as u32;
            carry = total >> 32;
        }
        let mut position = i + right.len();
        while carry != 0 {
            let total = u64::from(output[position]) + carry;
            output[position] = total as u32;
            carry = total >> 32;
            position += 1;
        }
    }
    trim(&mut output);
    output
}

/// Divide in place by a single non-zero limb, returning the remainder.
fn div_small(limbs: &mut Vec<u32>, divisor: u32) -> u32 {
    let mut remainder = 0_u64;
    let divisor = u64::from(divisor);
    for limb in limbs.iter_mut().rev() {
        let current = (remainder << 32) | u64::from(*limb);
        *limb = (current / divisor) as u32;
        remainder = current % divisor;
    }
    trim(limbs);
    remainder as u32
}

/// `limbs = limbs * factor + addend` in place.
fn mul_add_small(limbs: &mut Vec<u32>, factor: u32, addend: u32) {
    let mut carry = u64::from(addend);
    for limb in limbs.iter_mut() {
        let total = u64::from(*limb) * u64::from(factor) + carry;
        *limb = total as u32;
        carry = total >> 32;
    }
    if carry != 0 {
        limbs.push(carry as u32);
    }
    trim(limbs);
}

fn shl_magnitude(limbs: &[u32], bits: u64) -> Vec<u32> {
    if limbs.is_empty() {
        return Vec::new();
    }
    let limb_shift = (bits / 32) as usize;
    let bit_shift = (bits % 32) as u32;
    let mut output = vec![0_u32; limb_shift];
    if bit_shift == 0 {
        output.extend_from_slice(limbs);
    } else {
        let mut carry = 0_u32;
        for limb in limbs {
            output.push((limb << bit_shift) | carry);
            carry = limb >> (32 - bit_shift);
        }
        if carry != 0 {
            output.push(carry);
        }
    }
    trim(&mut output);
    output
}

/// Logical right shift of a magnitude.
fn shr_magnitude(limbs: &[u32], bits: u64) -> Vec<u32> {
    let Ok(limb_shift) = usize::try_from(bits / 32) else {
        return Vec::new();
    };
    if limb_shift >= limbs.len() {
        return Vec::new();
    }
    let bit_shift = (bits % 32) as u32;
    let source = &limbs[limb_shift..];
    let mut output = Vec::with_capacity(source.len());
    for (index, limb) in source.iter().enumerate() {
        if bit_shift == 0 {
            output.push(*limb);
        } else {
            let high = source.get(index + 1).copied().unwrap_or(0);
            output.push((limb >> bit_shift) | (high << (32 - bit_shift)));
        }
    }
    trim(&mut output);
    output
}

/// Quotient and remainder of two magnitudes, `divisor` non-zero. A single
/// limb divides in one pass; longer divisors use Knuth's Algorithm D (TAOCP
/// 4.3.1), one multiply-subtract per quotient limb.
fn divmod_magnitude(dividend: &[u32], divisor: &[u32]) -> (Vec<u32>, Vec<u32>) {
    if cmp_magnitude(dividend, divisor) == Ordering::Less {
        return (Vec::new(), dividend.to_vec());
    }
    if let [limb] = divisor {
        let mut quotient = dividend.to_vec();
        let remainder = div_small(&mut quotient, *limb);
        let remainder = if remainder == 0 {
            Vec::new()
        } else {
            vec![remainder]
        };
        return (quotient, remainder);
    }
    // Normalise so the divisor's top limb has its high bit set. Then each
    // quotient-digit estimate from the top two limbs is at most two too large.
    let shift = divisor.last().map_or(0, |top| top.leading_zeros());
    let mut scaled_divisor = shift_left_limbs(divisor, shift);
    scaled_divisor.pop();
    let mut scaled = shift_left_limbs(dividend, shift);
    let n = scaled_divisor.len();
    let top = u64::from(scaled_divisor[n - 1]);
    let second = u64::from(scaled_divisor[n - 2]);
    let base = 1_u64 << 32;
    let mut quotient = vec![0_u32; dividend.len() - n + 1];
    for j in (0..quotient.len()).rev() {
        let numerator = (u64::from(scaled[j + n]) << 32) | u64::from(scaled[j + n - 1]);
        let mut digit = numerator / top;
        let mut rest = numerator % top;
        // Knuth D3: refine the estimate with the next limb down.
        while digit >= base || digit * second > ((rest << 32) | u64::from(scaled[j + n - 2])) {
            digit -= 1;
            rest += top;
            if rest >= base {
                break;
            }
        }
        // Knuth D4: multiply the divisor by the digit and subtract it.
        let mut borrow = 0_i64;
        for i in 0..n {
            let product = digit * u64::from(scaled_divisor[i]);
            let difference = i64::from(scaled[i + j]) - borrow - (product & 0xFFFF_FFFF) as i64;
            scaled[i + j] = difference as u32;
            borrow = (product >> 32) as i64 - (difference >> 32);
        }
        let difference = i64::from(scaled[j + n]) - borrow;
        scaled[j + n] = difference as u32;
        // Knuth D5 and D6: the digit was one too large, so add the divisor back.
        if difference < 0 {
            digit -= 1;
            let mut carry = 0_u64;
            for i in 0..n {
                let sum = u64::from(scaled[i + j]) + u64::from(scaled_divisor[i]) + carry;
                scaled[i + j] = sum as u32;
                carry = sum >> 32;
            }
            scaled[j + n] = scaled[j + n].wrapping_add(carry as u32);
        }
        quotient[j] = digit as u32;
    }
    // The remainder is the low `n` limbs, shifted back down.
    let mut remainder: Vec<u32> = (0..n)
        .map(|index| {
            if shift == 0 {
                scaled[index]
            } else {
                (scaled[index] >> shift) | (scaled[index + 1] << (32 - shift))
            }
        })
        .collect();
    trim(&mut quotient);
    trim(&mut remainder);
    (quotient, remainder)
}

/// `limbs << shift` for `shift` below 32, one limb longer than the input so the
/// carry out of the top limb is kept.
fn shift_left_limbs(limbs: &[u32], shift: u32) -> Vec<u32> {
    let mut output = Vec::with_capacity(limbs.len() + 1);
    let mut carry = 0_u32;
    for limb in limbs {
        if shift == 0 {
            output.push(*limb);
        } else {
            output.push((limb << shift) | carry);
            carry = limb >> (32 - shift);
        }
    }
    output.push(carry);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn int(text: &str) -> JsBigInt {
        let (negative, digits) = text
            .strip_prefix('-')
            .map_or((false, text), |rest| (true, rest));
        let value = JsBigInt::parse_digits(digits, 10).expect("decimal digits");
        if negative { value.negate() } else { value }
    }

    /// A small deterministic generator for the division tests.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0 >> 32
        }

        /// A signed value of up to `max_limbs` limbs, weighted toward the limb
        /// values that make the quotient-digit estimate need correcting.
        fn value(&mut self, max_limbs: u64) -> JsBigInt {
            let length = 1 + self.next() % max_limbs;
            let limbs = (0..length)
                .map(|_| match self.next() % 4 {
                    0 => 0,
                    1 => u32::MAX,
                    _ => self.next() as u32,
                })
                .collect();
            JsBigInt::from_parts(self.next() % 2 == 1, limbs)
        }
    }

    #[test]
    fn knuth_division_reconstructs_random_operands() {
        let mut random = Lcg(0x9E37_79B9_7F4A_7C15);
        for _ in 0..2000 {
            let dividend = random.value(9);
            let divisor = random.value(6);
            if divisor.is_zero() {
                continue;
            }
            let (quotient, remainder) = dividend.div_rem(&divisor).expect("non-zero divisor");
            assert_eq!(
                quotient.mul(&divisor).add(&remainder),
                dividend,
                "{dividend:?} / {divisor:?}"
            );
            assert_eq!(
                cmp_magnitude(&remainder.limbs, &divisor.limbs),
                Ordering::Less,
                "{dividend:?} % {divisor:?}"
            );
            assert!(remainder.is_zero() || remainder.negative == dividend.negative);
        }
    }

    #[test]
    fn division_of_million_bit_operands_is_linear_in_the_quotient() {
        // A one-digit quotient of two operands about a million bits long must
        // not cost a bit-by-bit pass over the whole dividend.
        let dividend = JsBigInt::from_i64(1)
            .shift(&JsBigInt::from_i64(1_000_000))
            .expect("fits");
        let divisor = JsBigInt::from_i64(1)
            .shift(&JsBigInt::from_i64(999_999))
            .expect("fits")
            .add(&JsBigInt::from_i64(1));
        let (quotient, remainder) = dividend.div_rem(&divisor).expect("non-zero");
        assert_eq!(quotient, JsBigInt::from_i64(1));
        assert_eq!(
            remainder,
            JsBigInt::from_i64(1)
                .shift(&JsBigInt::from_i64(999_999))
                .expect("fits")
                .sub(&JsBigInt::from_i64(1))
        );
    }

    #[test]
    fn decimal_round_trip_crosses_limb_and_chunk_boundaries() {
        for text in [
            "0",
            "1",
            "4294967295",
            "4294967296",
            "999999999999999999999999999999",
            "-340282366920938463463374607431768211456",
        ] {
            assert_eq!(int(text).to_string_radix(10), text);
        }
    }

    #[test]
    fn radix_conversion_matches_known_values() {
        assert_eq!(JsBigInt::from_i64(255).to_string_radix(16), "ff");
        assert_eq!(JsBigInt::from_i64(-8).to_string_radix(2), "-1000");
        assert_eq!(JsBigInt::from_i64(35).to_string_radix(36), "z");
        assert_eq!(JsBigInt::zero().to_string_radix(7), "0");
    }

    #[test]
    fn addition_and_subtraction_handle_signs_and_carries() {
        assert_eq!(
            int("4294967295").add(&int("1")).to_string_radix(10),
            "4294967296"
        );
        assert_eq!(int("5").sub(&int("7")).to_string_radix(10), "-2");
        assert_eq!(int("-5").add(&int("5")), JsBigInt::zero());
        assert!(!int("-5").add(&int("5")).is_negative());
    }

    #[test]
    fn multiplication_and_division_truncate_toward_zero() {
        let big = int("123456789012345678901234567890");
        let product = big.mul(&big);
        assert_eq!(
            product.to_string_radix(10),
            "15241578753238836750495351562536198787501905199875019052100"
        );
        let (quotient, remainder) = int("-7").div_rem(&int("2")).expect("non-zero");
        assert_eq!(quotient.to_string_radix(10), "-3");
        assert_eq!(remainder.to_string_radix(10), "-1");
        let (quotient, remainder) = product.div_rem(&big).expect("non-zero");
        assert_eq!(quotient, big);
        assert!(remainder.is_zero());
        assert!(int("1").div_rem(&JsBigInt::zero()).is_none());
    }

    #[test]
    fn multi_limb_division_matches_multiplication() {
        let divisor = int("98765432109876543210987");
        let quotient = int("12345678901234567890123456789");
        let dividend = quotient.mul(&divisor).add(&int("1234"));
        let (got_quotient, got_remainder) = dividend.div_rem(&divisor).expect("non-zero");
        assert_eq!(got_quotient, quotient);
        assert_eq!(got_remainder, int("1234"));
    }

    #[test]
    fn power_respects_the_size_bound() {
        assert_eq!(
            JsBigInt::from_i64(2)
                .pow(&JsBigInt::from_i64(64))
                .expect("fits")
                .to_string_radix(10),
            "18446744073709551616"
        );
        assert_eq!(
            JsBigInt::from_i64(-1)
                .pow(&int("1000000000000000000001"))
                .expect("|base| = 1 needs no size")
                .to_string_radix(10),
            "-1"
        );
        assert!(
            JsBigInt::from_i64(2)
                .pow(&JsBigInt::from_i64(1 << 25))
                .is_none()
        );
    }

    #[test]
    fn shifts_round_toward_negative_infinity() {
        assert_eq!(
            JsBigInt::from_i64(-5)
                .shift(&JsBigInt::from_i64(-1))
                .expect("right shift")
                .to_string_radix(10),
            "-3"
        );
        assert_eq!(
            JsBigInt::from_i64(-1)
                .shift(&JsBigInt::from_i64(-100))
                .expect("right shift")
                .to_string_radix(10),
            "-1"
        );
        assert_eq!(
            JsBigInt::from_i64(1)
                .shift(&JsBigInt::from_i64(40))
                .expect("left shift")
                .to_string_radix(10),
            "1099511627776"
        );
        assert!(
            JsBigInt::from_i64(1)
                .shift(&JsBigInt::from_u64(u64::MAX))
                .is_none()
        );
    }

    #[test]
    fn bitwise_operations_use_twos_complement() {
        assert_eq!(
            JsBigInt::from_i64(-1).bit_and(&int("255")),
            JsBigInt::from_i64(255)
        );
        assert_eq!(
            JsBigInt::from_i64(-6).bit_or(&JsBigInt::from_i64(3)),
            JsBigInt::from_i64(-5)
        );
        assert_eq!(
            JsBigInt::from_i64(-6).bit_xor(&JsBigInt::from_i64(3)),
            JsBigInt::from_i64(-7)
        );
        assert_eq!(JsBigInt::from_i64(5).bit_not(), JsBigInt::from_i64(-6));
        assert_eq!(
            int("4294967296").bit_and(&int("-4294967296")),
            int("4294967296")
        );
    }

    #[test]
    fn as_int_n_and_as_uint_n_wrap_at_the_width() {
        assert_eq!(
            JsBigInt::from_i64(-1).as_uint_n(8).expect("fits"),
            JsBigInt::from_i64(255)
        );
        assert_eq!(
            JsBigInt::from_i64(255).as_int_n(8).expect("fits"),
            JsBigInt::from_i64(-1)
        );
        assert_eq!(
            JsBigInt::from_i64(128).as_int_n(8).expect("fits"),
            JsBigInt::from_i64(-128)
        );
        assert_eq!(
            JsBigInt::from_i64(-129).as_int_n(8).expect("fits"),
            JsBigInt::from_i64(127)
        );
        assert_eq!(
            JsBigInt::from_i64(-1).as_int_n(1 << 53).expect("in range"),
            JsBigInt::from_i64(-1)
        );
        assert_eq!(
            JsBigInt::from_i64(7).as_uint_n(0).expect("zero bits"),
            JsBigInt::zero()
        );
    }

    #[test]
    fn to_f64_rounds_to_nearest_even() {
        assert_eq!(JsBigInt::from_i64(-3).to_f64(), -3.0);
        // 2^53 + 1 is a tie that rounds to the even 2^53.
        assert_eq!(int("9007199254740993").to_f64(), 9_007_199_254_740_992.0);
        // 2^53 + 3 is a tie that rounds up to the even 2^53 + 4.
        assert_eq!(int("9007199254740995").to_f64(), 9_007_199_254_740_996.0);
        // Above 2^64 the value is folded through the 64-bit window. 2^64 + 2^11
        // is an exact tie between 2^64 and 2^64 + 2^12 (ulp 2^12): even wins.
        assert_eq!(
            int("18446744073709553664").to_f64(),
            18_446_744_073_709_551_616.0
        );
        // One above the tie is nearer 2^64 + 2^12, which the sticky bit must see.
        assert_eq!(
            int("18446744073709553665").to_f64(),
            18_446_744_073_709_555_712.0
        );
        let huge = JsBigInt::from_i64(1)
            .shift(&JsBigInt::from_i64(1024))
            .expect("fits");
        assert_eq!(huge.to_f64(), f64::INFINITY);
    }

    #[test]
    fn exact_comparison_against_numbers() {
        assert_eq!(JsBigInt::from_i64(1).cmp_f64(1.5), Some(Ordering::Less));
        assert_eq!(
            JsBigInt::from_i64(-1).cmp_f64(-1.5),
            Some(Ordering::Greater)
        );
        assert_eq!(JsBigInt::from_i64(2).cmp_f64(2.0), Some(Ordering::Equal));
        assert_eq!(JsBigInt::zero().cmp_f64(-0.0), Some(Ordering::Equal));
        assert_eq!(JsBigInt::zero().cmp_f64(f64::NAN), None);
        assert_eq!(
            JsBigInt::from_i64(i64::MAX).cmp_f64(9_223_372_036_854_775_808.0),
            Some(Ordering::Less)
        );
    }

    #[test]
    fn integral_f64_conversion_is_exact() {
        assert_eq!(
            JsBigInt::from_integral_f64(9_007_199_254_740_992.0),
            Some(int("9007199254740992"))
        );
        assert_eq!(
            JsBigInt::from_integral_f64(-1e21).map(|value| value.to_string_radix(10)),
            Some("-1000000000000000000000".to_owned())
        );
        assert_eq!(JsBigInt::from_integral_f64(1.5), None);
        assert_eq!(JsBigInt::from_integral_f64(f64::INFINITY), None);
    }
}
