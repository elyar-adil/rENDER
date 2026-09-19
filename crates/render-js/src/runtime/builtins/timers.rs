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
use crate::runtime::convert::required_argument;
use crate::runtime::convert::to_number;
use crate::runtime::types::TimerEntry;
use crate::runtime::types::TimerKind;
use crate::runtime::types::TimerRequest;
use crate::value::NativeFunction;

/// Interpret a timer id argument; non-integral or out-of-range values match no
/// timer, mirroring the platform's lenient `clearTimeout` behavior.
pub(in crate::runtime) fn optional_timer_id(value: &JsValue) -> Option<u64> {
    match value {
        JsValue::Number(number)
            if number.is_finite() && number.fract() == 0.0 && *number >= 1.0 =>
        {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "guarded by the finite integral range check above"
            )]
            {
                Some(*number as u64)
            }
        }
        _ => None,
    }
}

impl JsRuntime {
    pub(in crate::runtime) fn register_timer_entry(
        &mut self,
        callback: ObjectId,
        delay_ms: f64,
        kind: TimerKind,
    ) -> f64 {
        let id = self.next_timer_id;
        self.next_timer_id += 1;
        self.timers.insert(
            id,
            TimerEntry {
                kind,
                callback,
                delay_ms,
            },
        );
        self.pending_timer_requests
            .push(TimerRequest::Schedule { id, delay_ms });
        #[allow(
            clippy::cast_precision_loss,
            reason = "timer ids stay far below the 2^53 exact-integer boundary"
        )]
        {
            id as f64
        }
    }

    pub(in crate::runtime) fn register_timer(
        &mut self,
        function: NativeFunction,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let kind = match function {
            NativeFunction::SetInterval => TimerKind::Interval,
            _ => TimerKind::Timeout,
        };
        let name = if kind == TimerKind::Timeout {
            "setTimeout"
        } else {
            "setInterval"
        };
        let callback =
            Self::require_callable_object(required_argument(arguments, 0, name)?, &self.realm)?;
        let delay = match arguments.get(1) {
            None | Some(JsValue::Undefined) => 0.0,
            Some(value) => to_number(value)?,
        };
        let delay_ms = if delay.is_nan() { 0.0 } else { delay.max(0.0) };
        Ok(JsValue::Number(
            self.register_timer_entry(callback, delay_ms, kind),
        ))
    }

    pub(in crate::runtime) fn cancel_timer(&mut self, arguments: &[JsValue]) -> JsValue {
        if let Some(id) = optional_timer_id(arguments.first().unwrap_or(&JsValue::Undefined))
            && self.timers.remove(&id).is_some()
        {
            self.pending_timer_requests
                .push(TimerRequest::Cancel { id });
        }
        JsValue::Undefined
    }
}
