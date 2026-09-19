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

use render_dom::Dom;
use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::runtime::convert::required_argument;
use crate::runtime::convert::to_number;
use crate::value::NativeFunction;
use crate::value::ObjectHost;

impl JsRuntime {
    pub(in crate::runtime) fn dispatch_date_native(
        &mut self,
        dom: &mut Dom,
        function: NativeFunction,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        match function {
            NativeFunction::DateSetTime => {
                if let Some(ObjectHost::DateInstance(_ms)) = self.realm.host(receiver) {
                    let new_ms = to_number(required_argument(arguments, 0, "setTime")?)?;
                    self.realm.set_host_data_date(receiver, new_ms);
                    Ok(JsValue::Number(new_ms))
                } else {
                    Err(JsError::type_error("incompatible Date method receiver"))
                }
            }
            NativeFunction::DateGetFullYear
            | NativeFunction::DateGetMonth
            | NativeFunction::DateGetDate
            | NativeFunction::DateGetDay
            | NativeFunction::DateGetHours
            | NativeFunction::DateGetMinutes
            | NativeFunction::DateGetSeconds
            | NativeFunction::DateGetMilliseconds
            | NativeFunction::DateGetTimezoneOffset
            | NativeFunction::DateGetUTCFullYear
            | NativeFunction::DateGetUTCMonth
            | NativeFunction::DateGetUTCDate
            | NativeFunction::DateGetUTCDay
            | NativeFunction::DateGetUTCHours
            | NativeFunction::DateGetUTCMinutes
            | NativeFunction::DateGetUTCSeconds
            | NativeFunction::DateGetUTCMilliseconds => {
                let ms = self.require_date_value(receiver)?;
                Ok(Self::date_getter(function, ms))
            }
            NativeFunction::DateToISOString => {
                let ms = self.require_date_value(receiver)?;
                if !ms.is_finite() {
                    return Err(JsError::type_error("Invalid time value"));
                }
                Ok(JsValue::String(Self::format_date_iso(ms)))
            }
            NativeFunction::DateToJSON => {
                let ms = self.require_date_value(receiver)?;
                if !ms.is_finite() {
                    Ok(JsValue::Null)
                } else {
                    Ok(JsValue::String(Self::format_date_iso(ms)))
                }
            }
            NativeFunction::DateToDateString => {
                let ms = self.require_date_value(receiver)?;
                if !ms.is_finite() {
                    return Ok(JsValue::String("Invalid Date".to_owned()));
                }
                let (year, month, day, _, _, _, _, weekday) = Self::date_components(ms);
                Ok(JsValue::String(format!(
                    "{} {} {day:02} {year}",
                    DATE_WEEKDAYS[weekday as usize], DATE_MONTHS[month as usize]
                )))
            }
            NativeFunction::DateParse => {
                let input = required_argument(arguments, 0, "Date.parse")?.to_js_string();
                Ok(JsValue::Number(
                    Self::parse_date_string(&input).unwrap_or(f64::NAN),
                ))
            }
            NativeFunction::DateUTC => {
                Ok(JsValue::Number(Self::date_from_utc_arguments(arguments)?))
            }
            NativeFunction::DateNow => Ok(JsValue::Number(Self::now_ms())),
            NativeFunction::DateGetValue | NativeFunction::DateValueOf => {
                match self.realm.host(receiver) {
                    Some(ObjectHost::DateInstance(ms)) => Ok(JsValue::Number(ms)),
                    _ => Err(JsError::type_error("incompatible Date method receiver")),
                }
            }
            NativeFunction::DateToString | NativeFunction::DateToGMTString => {
                match self.realm.host(receiver) {
                    Some(ObjectHost::DateInstance(ms)) => {
                        Ok(JsValue::String(Self::format_date_utc(ms)))
                    }
                    _ => Err(JsError::type_error("incompatible Date method receiver")),
                }
            }
            other => self.dispatch_promise_native(dom, other, receiver, arguments),
        }
    }
}

#[allow(clippy::cast_possible_wrap)]
pub(in crate::runtime) fn days_from_civil(year: i32, month: i32, day: i32) -> i64 {
    let year = i64::from(year) - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Weekday and month names for the minimal UTC date formatter.
pub(in crate::runtime) const DATE_WEEKDAYS: [&str; 7] =
    ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];

pub(in crate::runtime) const DATE_MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

impl JsRuntime {
    pub(in crate::runtime) fn require_date_value(
        &self,
        receiver: ObjectId,
    ) -> Result<f64, JsError> {
        match self.realm.host(receiver) {
            Some(ObjectHost::DateInstance(ms)) => Ok(ms),
            _ => Err(JsError::type_error("incompatible Date method receiver")),
        }
    }

    pub(in crate::runtime) fn date_getter(function: NativeFunction, ms: f64) -> JsValue {
        if !ms.is_finite() {
            return JsValue::Number(f64::NAN);
        }
        let (year, month, day, hour, minute, second, millis, weekday) = Self::date_components(ms);
        let value = match function {
            NativeFunction::DateGetFullYear | NativeFunction::DateGetUTCFullYear => year,
            NativeFunction::DateGetMonth | NativeFunction::DateGetUTCMonth => month,
            NativeFunction::DateGetDate | NativeFunction::DateGetUTCDate => day,
            NativeFunction::DateGetDay | NativeFunction::DateGetUTCDay => weekday,
            NativeFunction::DateGetHours | NativeFunction::DateGetUTCHours => hour,
            NativeFunction::DateGetMinutes | NativeFunction::DateGetUTCMinutes => minute,
            NativeFunction::DateGetSeconds | NativeFunction::DateGetUTCSeconds => second,
            NativeFunction::DateGetMilliseconds | NativeFunction::DateGetUTCMilliseconds => millis,
            NativeFunction::DateGetTimezoneOffset => 0,
            _ => 0,
        };
        JsValue::Number(f64::from(value))
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub(in crate::runtime) fn date_components(
        ms: f64,
    ) -> (i32, i32, i32, i32, i32, i32, i32, i32) {
        let total_millis = ms.floor() as i64;
        let days = total_millis.div_euclid(86_400_000);
        let day_millis = total_millis.rem_euclid(86_400_000);
        let seconds = day_millis / 1000;
        let millis = day_millis % 1000;
        let z = days + 719_468;
        let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
        let day_of_era = z - era * 146_097;
        let year_of_era =
            (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let mut year = year_of_era + era * 400;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let month_prelude = (5 * day_of_year + 2) / 153;
        let month = if month_prelude < 10 {
            month_prelude + 3
        } else {
            month_prelude - 9
        };
        let day = day_of_year - (153 * month_prelude + 2) / 5 + 1;
        year += i64::from(month <= 2);
        let month = month - 1;
        let hour = seconds / 3600;
        let minute = (seconds % 3600) / 60;
        let second = seconds % 60;
        let weekday = (days + 4).rem_euclid(7);
        (
            year as i32,
            month as i32,
            day as i32,
            hour as i32,
            minute as i32,
            second as i32,
            millis as i32,
            weekday as i32,
        )
    }

    pub(in crate::runtime) fn date_from_constructor_arguments(
        arguments: &[JsValue],
    ) -> Result<f64, JsError> {
        match arguments {
            [] | [JsValue::Undefined] => Ok(Self::now_ms()),
            [JsValue::String(value)] => Ok(Self::parse_date_string(value).unwrap_or(f64::NAN)),
            [value] => to_number(value),
            _ => Self::date_from_utc_arguments(arguments),
        }
    }

    pub(in crate::runtime) fn date_from_utc_arguments(
        arguments: &[JsValue],
    ) -> Result<f64, JsError> {
        let number = |index: usize, default: f64| -> Result<f64, JsError> {
            Ok(match arguments.get(index) {
                None | Some(JsValue::Undefined) => default,
                Some(value) => to_number(value)?,
            })
        };
        let mut year = number(0, 0.0)? as i32;
        let month = number(1, 0.0)? as i32;
        let day = number(2, 1.0)? as i32;
        let hour = number(3, 0.0)? as i32;
        let minute = number(4, 0.0)? as i32;
        let second = number(5, 0.0)? as i32;
        let millis = number(6, 0.0)? as i32;
        if (0..=99).contains(&year) {
            year += 1900;
        }
        let days = days_from_civil(year, month + 1, 1) + i64::from(day - 1);
        Ok((days * 86_400_000
            + i64::from(hour) * 3_600_000
            + i64::from(minute) * 60_000
            + i64::from(second) * 1_000
            + i64::from(millis)) as f64)
    }

    pub(in crate::runtime) fn parse_date_string(value: &str) -> Option<f64> {
        let value = value.trim();
        let (date, time) = value.split_once('T').or_else(|| value.split_once(' '))?;
        let mut date_parts = date.split('-');
        let year = date_parts.next()?.parse::<i32>().ok()?;
        let month = date_parts.next()?.parse::<i32>().ok()?;
        let day = date_parts.next()?.parse::<i32>().ok()?;
        let mut time_parts = time.trim_end_matches('Z').split(':');
        let hour = time_parts.next()?.parse::<i32>().ok()?;
        let minute = time_parts.next()?.parse::<i32>().ok()?;
        let second_part = time_parts.next().unwrap_or("0");
        let (second, millis) = if let Some((whole, fraction)) = second_part.split_once('.') {
            (
                whole.parse::<i32>().ok()?,
                format!("{fraction:0<3}")[..3].parse::<i32>().ok()?,
            )
        } else {
            (second_part.parse::<i32>().ok()?, 0)
        };
        let days = days_from_civil(year, month, day);
        Some(
            (days * 86_400_000
                + i64::from(hour) * 3_600_000
                + i64::from(minute) * 60_000
                + i64::from(second) * 1_000
                + i64::from(millis)) as f64,
        )
    }

    pub(in crate::runtime) fn format_date_iso(ms: f64) -> String {
        let (year, month, day, hour, minute, second, millis, _) = Self::date_components(ms);
        format!(
            "{year:04}-{:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z",
            month + 1
        )
    }

    /// Milliseconds since the Unix epoch.
    pub(in crate::runtime) fn now_ms() -> f64 {
        #[allow(
            clippy::cast_precision_loss,
            reason = "epoch milliseconds fit exactly in binary64 for millions of years"
        )]
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |duration| duration.as_millis() as f64)
    }

    pub(in crate::runtime) fn monotonic_now_ms() -> f64 {
        pub(in crate::runtime) static START: std::sync::OnceLock<std::time::Instant> =
            std::sync::OnceLock::new();
        START
            .get_or_init(std::time::Instant::now)
            .elapsed()
            .as_secs_f64()
            * 1_000.0
    }

    /// Minimal UTC formatting: `Mon Jan 01 2026 00:00:00 GMT+0000`.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "epoch seconds fit i64 comfortably; indices are bounded by construction"
    )]
    pub(in crate::runtime) fn format_date_utc(ms: f64) -> String {
        let total_seconds = (ms / 1000.0).floor();
        let days = (total_seconds / 86_400.0).floor() as i64;
        let seconds_of_day = total_seconds as i64 - days * 86_400;
        // Civil-from-days (Howard Hinnant's algorithm).
        let z = days + 719_468;
        let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
        let day_of_era = z - era * 146_097;
        let year_of_era =
            (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let mut year = year_of_era + era * 400;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let month_prelude = (5 * day_of_year + 2) / 153;
        let month = if month_prelude < 10 {
            month_prelude + 3
        } else {
            month_prelude - 9
        };
        year += i64::from(month <= 2);
        let month_index = (month - 1) as usize;
        let day_of_month = day_of_year - (153 * month_prelude + 2) / 5 + 1;
        let hour = seconds_of_day / 3600;
        let minute = (seconds_of_day % 3600) / 60;
        let second = seconds_of_day % 60;
        let weekday = (days + 4).rem_euclid(7) as usize;
        format!(
            "{:03} {} {:02} {} {:02}:{:02}:{:02} GMT+0000",
            DATE_WEEKDAYS[weekday],
            DATE_MONTHS[month_index.clamp(0, 11)],
            day_of_month,
            year,
            hour,
            minute,
            second,
        )
    }
}
