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
use crate::value::NativeFunction;
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
            other => self.dispatch_json_native(dom, other, receiver, arguments),
        }
    }
}

pub(in crate::runtime) fn js_math_round(value: f64) -> f64 {
    if value.is_nan() || value.is_infinite() || value == 0.0 {
        return value;
    }
    (value + 0.5).floor()
}

impl JsRuntime {
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
