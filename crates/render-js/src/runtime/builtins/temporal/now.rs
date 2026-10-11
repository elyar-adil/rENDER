//! `Temporal.Now`: the current instant, and the current date and time in UTC.
//! This engine resolves no other zone, so a zone argument must name UTC.

use super::iso::{IsoDateTime, datetime_from_epoch_nanos};
use super::{Surface, TemporalKind, TemporalMethod, TemporalValue, argument, member};
use crate::JsError;
use crate::JsValue;
use crate::runtime::JsRuntime;
use render_dom::Dom;

/// The current time as epoch nanoseconds, from the system clock.
fn now_nanos() -> i128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| i128::try_from(elapsed.as_nanos()).unwrap_or(0))
}

impl JsRuntime {
    /// The zone argument of a `Now` function: absent or `UTC` is accepted.
    fn check_utc_argument(&mut self, dom: &mut Dom, value: &JsValue) -> Result<(), JsError> {
        if matches!(value, JsValue::Undefined) {
            return Ok(());
        }
        let text = self.to_string_checked(dom, value)?;
        if text.eq_ignore_ascii_case("UTC") {
            Ok(())
        } else {
            Err(self.range_error("only the UTC time zone is supported"))
        }
    }

    /// The `Temporal.Now` functions, dispatched by name.
    pub(super) fn now_native(
        &mut self,
        dom: &mut Dom,
        method: TemporalMethod,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let nanos = now_nanos();
        let zone = argument(arguments, 0);
        match method {
            TemporalMethod::NowInstant => {
                self.temporal_instance(TemporalKind::Instant, TemporalValue::Instant(nanos))
            }
            TemporalMethod::NowTimeZoneId => Ok(JsValue::String(("UTC".to_owned()).into())),
            TemporalMethod::NowPlainDateIso => {
                self.check_utc_argument(dom, &zone)?;
                let date = datetime_from_epoch_nanos(nanos).date;
                self.temporal_instance(TemporalKind::PlainDate, TemporalValue::PlainDate(date))
            }
            TemporalMethod::NowPlainTimeIso => {
                self.check_utc_argument(dom, &zone)?;
                let time = datetime_from_epoch_nanos(nanos).time;
                self.temporal_instance(TemporalKind::PlainTime, TemporalValue::PlainTime(time))
            }
            TemporalMethod::NowPlainDateTimeIso => {
                self.check_utc_argument(dom, &zone)?;
                let datetime: IsoDateTime = datetime_from_epoch_nanos(nanos);
                self.temporal_instance(
                    TemporalKind::PlainDateTime,
                    TemporalValue::PlainDateTime(datetime),
                )
            }
            _ => Err(JsError::type_error("unsupported Temporal.Now member")),
        }
    }
}

/// The functions of `Temporal.Now`, installed on the `Now` namespace object.
pub(super) const SURFACE: Surface = Surface {
    constructor_length: None,
    statics: &[],
    methods: &[
        member("instant", TemporalMethod::NowInstant, 0),
        member("plainDateISO", TemporalMethod::NowPlainDateIso, 0),
        member("plainDateTimeISO", TemporalMethod::NowPlainDateTimeIso, 0),
        member("plainTimeISO", TemporalMethod::NowPlainTimeIso, 0),
        member("timeZoneId", TemporalMethod::NowTimeZoneId, 0),
    ],
    getters: &[],
};
