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
use crate::runtime::types::TimerEntry;
use crate::runtime::types::TimerHandler;
use crate::runtime::types::TimerKind;
use crate::runtime::types::TimerRequest;
use crate::value::NativeFunction;
use render_dom::Dom;

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

/// The delay a timer function actually uses. The `timeout` argument is a `WebIDL`
/// `long`: a non-finite number becomes 0, any other value wraps modulo 2^32 into
/// the signed 32-bit range, and a negative result is 0 (HTML 8.6, timer
/// initialization steps).
#[must_use]
pub(crate) fn timer_delay_ms(delay: f64) -> f64 {
    if !delay.is_finite() {
        return 0.0;
    }
    let wrapped = delay.trunc().rem_euclid(4_294_967_296.0);
    let signed = if wrapped >= 2_147_483_648.0 {
        wrapped - 4_294_967_296.0
    } else {
        wrapped
    };
    signed.max(0.0)
}

impl JsRuntime {
    pub(in crate::runtime) fn register_timer_entry(
        &mut self,
        callback: ObjectId,
        delay_ms: f64,
        kind: TimerKind,
        arguments: Vec<JsValue>,
    ) -> f64 {
        self.register_timer_handler(TimerHandler::Callback(callback), delay_ms, kind, arguments)
    }

    pub(in crate::runtime) fn register_timer_handler(
        &mut self,
        handler: TimerHandler,
        delay_ms: f64,
        kind: TimerKind,
        arguments: Vec<JsValue>,
    ) -> f64 {
        let id = self.next_timer_id;
        self.next_timer_id += 1;
        self.timers.insert(
            id,
            TimerEntry {
                kind,
                handler,
                delay_ms,
                arguments,
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
        dom: &mut Dom,
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
        // WebIDL `TimerHandler` is `(Function or DOMString)`: a callable object is
        // the function form, and any other value is converted with ToString and
        // run as a script when the timer fires (HTML 8.6, timer initialization).
        let handler = match required_argument(arguments, 0, name)? {
            JsValue::Object(object) if Self::is_callable_object(*object, &self.realm) => {
                TimerHandler::Callback(*object)
            }
            value => TimerHandler::Script(self.to_string_value(dom, value)?.to_lossy()),
        };
        let delay = match arguments.get(1) {
            None | Some(JsValue::Undefined) => 0.0,
            Some(value) => self.to_number_value(dom, value)?,
        };
        // The string form takes no extra arguments: a script has no parameters.
        let extra = match handler {
            TimerHandler::Callback(_) => arguments.get(2..).unwrap_or_default().to_vec(),
            TimerHandler::Script(_) => Vec::new(),
        };
        Ok(JsValue::Number(self.register_timer_handler(
            handler,
            timer_delay_ms(delay),
            kind,
            extra,
        )))
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
