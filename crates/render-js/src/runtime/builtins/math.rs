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
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::runtime::convert::to_number;
use crate::value::{MathOp, NativeFunction, NumberOp};
use render_dom::Dom;

impl JsRuntime {
    pub(in crate::runtime) fn dispatch_math_native(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match function {
            NativeFunction::MathRandom => {
                let mut state = self.random_state;
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                self.random_state = state;
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "53-bit mantissa division yields a uniform f64 in [0, 1)"
                )]
                let value = (state >> 11) as f64 / (1u64 << 53) as f64;
                Ok(JsValue::Number(value))
            }
            NativeFunction::MathAbs => Self::math_unary(arguments, f64::abs),
            NativeFunction::MathCeil => Self::math_unary(arguments, f64::ceil),
            NativeFunction::MathFloor => Self::math_unary(arguments, f64::floor),
            NativeFunction::MathMax => Self::math_min_max(arguments, f64::NEG_INFINITY, f64::max),
            NativeFunction::MathMin => Self::math_min_max(arguments, f64::INFINITY, f64::min),
            NativeFunction::MathPow => Self::math_pow(arguments),
            NativeFunction::MathRound => Self::math_unary(arguments, js_math_round),
            NativeFunction::MathSqrt => Self::math_unary(arguments, f64::sqrt),
            NativeFunction::MathOp(operation) => Self::math_operation(operation, arguments),
            NativeFunction::NumberOp(operation) => Ok(JsValue::Boolean(match operation {
                // §21.1.2: these never coerce; a non-number is simply false.
                _ if !matches!(arguments.first(), Some(JsValue::Number(_))) => false,
                NumberOp::IsNaN => {
                    matches!(arguments.first(), Some(JsValue::Number(n)) if n.is_nan())
                }
                NumberOp::IsFinite => {
                    matches!(arguments.first(), Some(JsValue::Number(n)) if n.is_finite())
                }
                NumberOp::IsInteger => {
                    matches!(arguments.first(), Some(JsValue::Number(n)) if n.is_finite() && n.trunc() == *n)
                }
                NumberOp::IsSafeInteger => matches!(
                    arguments.first(),
                    Some(JsValue::Number(n))
                        if n.is_finite() && n.trunc() == *n && n.abs() <= 9_007_199_254_740_991.0
                ),
            })),
            other => self.dispatch_json_native(dom, other, receiver, arguments),
        }
    }
}

pub(in crate::runtime) fn js_math_round(value: f64) -> f64 {
    if value.is_nan() || value.is_infinite() || value == 0.0 {
        return value;
    }
    // §21.3.2.28: round half toward +infinity. `floor(x + 0.5)` is wrong for
    // 0.49999999999999994 (the sum rounds up to 1) and loses the sign of -0.
    let floor = value.floor();
    let rounded = if value - floor >= 0.5 {
        floor + 1.0
    } else {
        floor
    };
    if rounded == 0.0 && value < 0.0 {
        -0.0
    } else {
        rounded
    }
}

impl JsRuntime {
    /// The pure-`f64` `Math` functions (ECMA-262 §21.3.2).
    fn math_operation(operation: MathOp, arguments: &[JsValue]) -> Result<JsValue, JsError> {
        let argument = |index: usize| -> Result<f64, JsError> {
            to_number(arguments.get(index).unwrap_or(&JsValue::Undefined))
        };
        let value = match operation {
            MathOp::Sin => argument(0)?.sin(),
            MathOp::Cos => argument(0)?.cos(),
            MathOp::Tan => argument(0)?.tan(),
            MathOp::Asin => argument(0)?.asin(),
            MathOp::Acos => argument(0)?.acos(),
            MathOp::Atan => argument(0)?.atan(),
            MathOp::Sinh => argument(0)?.sinh(),
            MathOp::Cosh => argument(0)?.cosh(),
            MathOp::Tanh => argument(0)?.tanh(),
            MathOp::Asinh => argument(0)?.asinh(),
            MathOp::Acosh => argument(0)?.acosh(),
            MathOp::Atanh => argument(0)?.atanh(),
            MathOp::Log => argument(0)?.ln(),
            MathOp::Log2 => argument(0)?.log2(),
            MathOp::Log10 => argument(0)?.log10(),
            MathOp::Log1p => argument(0)?.ln_1p(),
            MathOp::Exp => argument(0)?.exp(),
            MathOp::Expm1 => argument(0)?.exp_m1(),
            MathOp::Sign => {
                let value = argument(0)?;
                if value.is_nan() || value == 0.0 {
                    value
                } else {
                    value.signum()
                }
            }
            MathOp::Trunc => argument(0)?.trunc(),
            MathOp::Cbrt => argument(0)?.cbrt(),
            MathOp::Fround => f64::from(argument(0)? as f32),
            MathOp::Clz32 => f64::from(to_uint32(argument(0)?).leading_zeros()),
            MathOp::Imul => {
                f64::from((to_uint32(argument(0)?).wrapping_mul(to_uint32(argument(1)?))) as i32)
            }
            MathOp::Atan2 => argument(0)?.atan2(argument(1)?),
            MathOp::Hypot => {
                let mut sum = 0.0_f64;
                let mut infinite = false;
                let mut nan = false;
                for index in 0..arguments.len() {
                    let value = argument(index)?;
                    infinite |= value.is_infinite();
                    nan |= value.is_nan();
                    sum += value * value;
                }
                if infinite {
                    f64::INFINITY
                } else if nan {
                    f64::NAN
                } else {
                    sum.sqrt()
                }
            }
        };
        Ok(JsValue::Number(value))
    }

    pub(in crate::runtime) fn math_unary(
        arguments: &[JsValue],
        operation: impl FnOnce(f64) -> f64,
    ) -> Result<JsValue, JsError> {
        Ok(JsValue::Number(operation(to_number(
            arguments.first().unwrap_or(&JsValue::Undefined),
        )?)))
    }

    pub(in crate::runtime) fn math_min_max(
        arguments: &[JsValue],
        identity: f64,
        operation: impl Fn(f64, f64) -> f64,
    ) -> Result<JsValue, JsError> {
        let mut result = identity;
        for argument in arguments {
            let value = to_number(argument)?;
            if value.is_nan() {
                return Ok(JsValue::Number(f64::NAN));
            }
            result = operation(result, value);
        }
        Ok(JsValue::Number(result))
    }

    pub(in crate::runtime) fn math_pow(arguments: &[JsValue]) -> Result<JsValue, JsError> {
        let base = arguments.first().unwrap_or(&JsValue::Undefined);
        let exponent = arguments.get(1).unwrap_or(&JsValue::Undefined);
        Ok(JsValue::Number(to_number(base)?.powf(to_number(exponent)?)))
    }
}

/// ECMA-262 §7.1.7 `ToUint32` on an already-numeric value.
fn to_uint32(value: f64) -> u32 {
    if !value.is_finite() {
        return 0;
    }
    value.trunc().rem_euclid(4_294_967_296.0) as u32
}
