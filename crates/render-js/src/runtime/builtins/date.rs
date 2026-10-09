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
use crate::value::ErrorKind;
use crate::value::JsSymbol;
use crate::value::NativeFunction;
use crate::value::ObjectHost;
use render_dom::Dom;

/// Milliseconds in one day, as used by `MakeDate`/`TimeClip`.
const MS_PER_DAY: f64 = 86_400_000.0;
/// ECMA-262 time value limit: 100,000,000 days either side of the epoch.
const MAX_TIME_VALUE: f64 = 8.64e15;

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
                if let Some(ObjectHost::DateInstance(_)) = self.realm.host(receiver) {
                    let argument = arguments.first().cloned().unwrap_or(JsValue::Undefined);
                    let number = self.to_date_number(dom, &argument)?;
                    let clipped = Self::time_clip(number);
                    self.realm.set_host_data_date(receiver, clipped);
                    Ok(JsValue::Number(clipped))
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
                    let error =
                        self.construct_standard_error(ErrorKind::RangeError, "Invalid time value")?;
                    return Err(JsError::thrown(error));
                }
                Ok(JsValue::String(Self::format_date_iso(ms)))
            }
            NativeFunction::DateToJSON => {
                let primitive = self.date_to_primitive_number(dom, receiver)?;
                if let JsValue::Number(number) = primitive
                    && !number.is_finite()
                {
                    return Ok(JsValue::Null);
                }
                let method = self.get_member(dom, receiver, "toISOString")?;
                let JsValue::Object(method) = method else {
                    return Err(JsError::type_error("toISOString is not callable"));
                };
                if !Self::is_callable_object(method, &self.realm) {
                    return Err(JsError::type_error("toISOString is not callable"));
                }
                self.call_with_this(dom, method, &[], JsValue::Object(receiver))
            }
            NativeFunction::DateToDateString => {
                let ms = self.require_date_value(receiver)?;
                if !ms.is_finite() {
                    return Ok(JsValue::String("Invalid Date".to_owned()));
                }
                let (year, month, day, _, _, _, _, weekday) = Self::date_components(ms);
                Ok(JsValue::String(format!(
                    "{} {} {day:02} {}",
                    DATE_WEEKDAYS[weekday as usize],
                    DATE_MONTHS[month as usize],
                    format_date_year(year),
                )))
            }
            NativeFunction::DateParse => {
                let input = match arguments.first() {
                    Some(value) => self.to_string_value(dom, value)?,
                    None => "undefined".to_owned(),
                };
                Ok(JsValue::Number(
                    Self::parse_date_string(&input).unwrap_or(f64::NAN),
                ))
            }
            NativeFunction::DateUTC => {
                // `year` is required (a missing one coerces from `undefined`);
                // `month` defaults to +0 when absent, the rest to 1/0.
                let mut values = [f64::NAN, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
                for (index, slot) in values.iter_mut().enumerate() {
                    if let Some(argument) = arguments.get(index) {
                        *slot = self.to_date_number(dom, argument)?;
                    }
                }
                Ok(JsValue::Number(Self::make_date_time(
                    values[0], values[1], values[2], values[3], values[4], values[5], values[6],
                )))
            }
            NativeFunction::DateNow => Ok(JsValue::Number(Self::now_ms())),
            NativeFunction::DateGetValue | NativeFunction::DateValueOf => {
                Ok(JsValue::Number(self.require_date_value(receiver)?))
            }
            NativeFunction::DateToString | NativeFunction::DateToGMTString => {
                let ms = self.require_date_value(receiver)?;
                let text = if function == NativeFunction::DateToString {
                    Self::format_date_utc(ms)
                } else {
                    Self::format_date_to_utc_string(ms)
                };
                Ok(JsValue::String(text))
            }
            other => self.dispatch_promise_native(dom, other, receiver, arguments),
        }
    }

    /// `ToNumber` for Date arguments: objects run `ToPrimitive` (number hint)
    /// through the evaluator so user `valueOf`/`toString` and abrupt
    /// completions behave correctly.
    fn to_date_number(&mut self, dom: &mut Dom, value: &JsValue) -> Result<f64, JsError> {
        let primitive = match value {
            JsValue::Object(object) => self.date_to_primitive_number(dom, *object)?,
            _ => value.clone(),
        };
        to_number(&primitive)
    }

    /// ECMA-262 `ToPrimitive(O, hint number)` as needed by `Date.UTC`,
    /// `Date.prototype.setTime`, and `Date.prototype.toJSON`: an exotic
    /// `Symbol.toPrimitive` first, then `valueOf`/`toString`, with getters
    /// invoked and primitive results returned as-is.
    fn date_to_primitive_number(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
    ) -> Result<JsValue, JsError> {
        // Primitive wrapper hosts already carry their primitive value.
        match self.realm.host(object) {
            Some(ObjectHost::NumberPrimitive(number)) => return Ok(JsValue::Number(number)),
            Some(ObjectHost::StringPrimitive(text)) => return Ok(JsValue::String(text)),
            Some(ObjectHost::BooleanPrimitive(value)) => return Ok(JsValue::Boolean(value)),
            _ => {}
        }
        let to_primitive = JsSymbol::well_known("@@toPrimitive");
        if let Some(descriptor) = self.realm.get_symbol_descriptor(object, &to_primitive) {
            let method = if descriptor.is_accessor() {
                match descriptor.getter {
                    Some(getter) => {
                        self.call_with_this(dom, getter, &[], JsValue::Object(object))?
                    }
                    None => JsValue::Undefined,
                }
            } else {
                descriptor.value
            };
            match method {
                JsValue::Undefined | JsValue::Null => {}
                JsValue::Object(method) if Self::is_callable_object(method, &self.realm) => {
                    let hint = JsValue::String("number".to_owned());
                    let result =
                        self.call_with_this(dom, method, &[hint], JsValue::Object(object))?;
                    if !matches!(result, JsValue::Object(_)) {
                        return Ok(result);
                    }
                    return Err(JsError::type_error(
                        "Cannot convert object to primitive value",
                    ));
                }
                _ => {
                    return Err(JsError::type_error("Symbol.toPrimitive is not a function"));
                }
            }
        }
        for name in ["valueOf", "toString"] {
            let method = self.get_member(dom, object, name)?;
            let JsValue::Object(method) = method else {
                continue;
            };
            if !Self::is_callable_object(method, &self.realm) {
                continue;
            }
            let result = self.call_with_this(dom, method, &[], JsValue::Object(object))?;
            if !matches!(result, JsValue::Object(_)) {
                return Ok(result);
            }
        }
        Err(JsError::type_error(
            "Cannot convert object to primitive value",
        ))
    }

    /// ECMA-262 `TimeClip`: non-finite and out-of-range values become NaN and
    /// the result is an integer with negative zero normalized to positive.
    pub(in crate::runtime) fn time_clip(time: f64) -> f64 {
        if !time.is_finite() || time.abs() > MAX_TIME_VALUE {
            return f64::NAN;
        }
        let clipped = time.trunc();
        if clipped == 0.0 { 0.0 } else { clipped }
    }

    /// ECMA-262 `MakeTime`; keeps the spec's floating-point operation order.
    fn make_time(hour: f64, minute: f64, second: f64, millis: f64) -> f64 {
        if !hour.is_finite() || !minute.is_finite() || !second.is_finite() || !millis.is_finite() {
            return f64::NAN;
        }
        let hour = to_integer_or_infinity(hour);
        let minute = to_integer_or_infinity(minute);
        let second = to_integer_or_infinity(second);
        let millis = to_integer_or_infinity(millis);
        ((hour * 3_600_000.0 + minute * 60_000.0) + second * 1_000.0) + millis
    }

    /// ECMA-262 `MakeDay`; only calendar positions representable as a time
    /// value are accepted, everything else yields NaN.
    fn make_day(year: f64, month: f64, date: f64) -> f64 {
        if !year.is_finite() || !month.is_finite() || !date.is_finite() {
            return f64::NAN;
        }
        let year = to_integer_or_infinity(year);
        let month = to_integer_or_infinity(month);
        let date = to_integer_or_infinity(date);
        let month_index = (month / 12.0).floor();
        let year_month = year + month_index;
        let month_in_year = month - month_index * 12.0;
        if !year_month.is_finite() || year_month.abs() > 400_000.0 {
            return f64::NAN;
        }
        let day = days_from_civil(year_month as i32, month_in_year as i32 + 1, 1);
        day as f64 + date - 1.0
    }

    /// `MakeDate` for day counts and millisecond times.
    fn make_date(day: f64, time: f64) -> f64 {
        day * MS_PER_DAY + time
    }

    /// `Date.UTC`/multi-argument constructor pipeline:
    /// `TimeClip(MakeDate(MakeDay(yr, m, dt), MakeTime(h, min, s, milli)))`
    /// with the 0-99 year mapping applied to `year`.
    fn make_date_time(
        year: f64,
        month: f64,
        date: f64,
        hour: f64,
        minute: f64,
        second: f64,
        millis: f64,
    ) -> f64 {
        let year = if year.is_nan() {
            f64::NAN
        } else {
            let integer = to_integer_or_infinity(year);
            if (0.0..=99.0).contains(&integer) {
                1900.0 + integer
            } else {
                year
            }
        };
        let time = Self::make_time(hour, minute, second, millis);
        Self::time_clip(Self::make_date(Self::make_day(year, month, date), time))
    }

    pub(in crate::runtime) fn date_from_constructor_arguments(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
    ) -> Result<f64, JsError> {
        match arguments {
            [] => Ok(Self::now_ms()),
            [value] => match value {
                JsValue::String(text) => Ok(Self::parse_date_string(text).unwrap_or(f64::NAN)),
                JsValue::Undefined => Ok(f64::NAN),
                other => {
                    let number = self.to_date_number(dom, other)?;
                    Ok(Self::time_clip(number))
                }
            },
            _ => self.date_from_utc_arguments(dom, arguments),
        }
    }

    pub(in crate::runtime) fn date_from_utc_arguments(
        &mut self,
        dom: &mut Dom,
        arguments: &[JsValue],
    ) -> Result<f64, JsError> {
        let mut values = [f64::NAN, f64::NAN, 1.0, 0.0, 0.0, 0.0, 0.0];
        for (index, slot) in values.iter_mut().enumerate() {
            if let Some(value) = arguments.get(index) {
                *slot = self.to_date_number(dom, value)?;
            }
        }
        Ok(Self::make_date_time(
            values[0], values[1], values[2], values[3], values[4], values[5], values[6],
        ))
    }

    pub(in crate::runtime) fn require_date_value(
        &self,
        receiver: ObjectId,
    ) -> Result<f64, JsError> {
        match self.realm.host(receiver) {
            Some(ObjectHost::DateInstance(ms)) => Ok(ms),
            // ECMA-262 21.4.4.41.1 `thisTimeValue`: a `Date.prototype` method
            // reached through `.call(5)` must see a primitive receiver as `NaN`
            // rather than as a brand error, because the method never had a Date
            // this-value to begin with.
            Some(
                ObjectHost::NumberPrimitive(_)
                | ObjectHost::StringPrimitive(_)
                | ObjectHost::BooleanPrimitive(_),
            ) => Ok(f64::NAN),
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
    pub(in crate::runtime) fn date_components(ms: f64) -> (i32, i32, i32, i32, i32, i32, i32, i32) {
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

    /// ECMA-262 `Date.parse`: ISO 8601 date-time forms first, then the legacy
    /// shapes the engine's own formatters emit. Local time is UTC.
    pub(in crate::runtime) fn parse_date_string(value: &str) -> Option<f64> {
        let value = value.trim();
        if value.is_empty() {
            return None;
        }
        parse_iso_date(value).or_else(|| parse_legacy_date(value))
    }

    pub(in crate::runtime) fn format_date_iso(ms: f64) -> String {
        let (year, month, day, hour, minute, second, millis, _) = Self::date_components(ms);
        format!(
            "{}-{:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z",
            format_iso_year(year),
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

    /// ECMA-262 `ToDateString`: `Mon Jan 01 2026 00:00:00 GMT+0000`
    /// (local time equals UTC because the engine has no timezone database).
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "epoch seconds fit i64 comfortably; indices are bounded by construction"
    )]
    pub(in crate::runtime) fn format_date_utc(ms: f64) -> String {
        if !ms.is_finite() {
            return "Invalid Date".to_owned();
        }
        let (year, month, day, hour, minute, second, _, weekday) = Self::date_components(ms);
        let weekday = DATE_WEEKDAYS[weekday as usize];
        let month = DATE_MONTHS[month as usize];
        let year = format_date_year(year);
        format!("{weekday} {month} {day:02} {year} {hour:02}:{minute:02}:{second:02} GMT+0000")
    }

    /// ECMA-262 `Date.prototype.toUTCString`: `Mon, 01 Jan 2026 00:00:00 GMT`.
    pub(in crate::runtime) fn format_date_to_utc_string(ms: f64) -> String {
        if !ms.is_finite() {
            return "Invalid Date".to_owned();
        }
        let (year, month, day, hour, minute, second, _, weekday) = Self::date_components(ms);
        let weekday = DATE_WEEKDAYS[weekday as usize];
        let month = DATE_MONTHS[month as usize];
        let year = format_date_year(year);
        format!("{weekday}, {day:02} {month} {year} {hour:02}:{minute:02}:{second:02} GMT")
    }
}

/// Weekday names indexed by the Sunday-based `WeekDay` result.
pub(in crate::runtime) const DATE_WEEKDAYS: [&str; 7] =
    ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

pub(in crate::runtime) const DATE_MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// ECMA-262 `ToIntegerOrInfinity` on an already-coerced number.
fn to_integer_or_infinity(number: f64) -> f64 {
    if number.is_nan() || number == 0.0 {
        0.0
    } else {
        number.trunc()
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

/// Calendar year for `DateString`/`Date.prototype.toString`: negative years
/// carry a sign and are zero-padded to four digits.
fn format_date_year(year: i32) -> String {
    if year < 0 {
        format!("-{:04}", -year)
    } else {
        format!("{year:04}")
    }
}

/// Calendar year for the ISO format: expanded years use a sign and six digits.
fn format_iso_year(year: i32) -> String {
    if (0..=9999).contains(&year) {
        format!("{year:04}")
    } else if year > 9999 {
        format!("+{year:06}")
    } else {
        format!("-{:06}", -year)
    }
}

fn parse_fixed_digits(value: &str, start: usize, count: usize) -> Option<(i32, usize)> {
    let end = start.checked_add(count)?;
    let digits = value.get(start..end)?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some((digits.parse().ok()?, end))
}

fn parse_iso_year(value: &str, start: usize) -> Option<(i32, usize)> {
    let bytes = value.as_bytes();
    let mut index = start;
    let mut signed = false;
    let sign = match bytes.get(index) {
        Some(b'-') => {
            index += 1;
            signed = true;
            -1
        }
        Some(b'+') => {
            index += 1;
            signed = true;
            1
        }
        _ => 1,
    };
    let digit_start = index;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        index += 1;
    }
    let digits = value.get(digit_start..index)?;
    if digits.is_empty() || digits.len() > 6 {
        return None;
    }
    let year = digits.parse::<i64>().ok()? * i64::from(sign);
    // The year 0 is positive and must use `+`; `-000000` is invalid.
    if signed && sign < 0 && year == 0 {
        return None;
    }
    Some((year as i32, index))
}

fn parse_time_of_day(value: &str, start: usize) -> Option<(i32, i32, i32, i32, usize)> {
    let (hour, mut index) = parse_fixed_digits(value, start, 2)?;
    let bytes = value.as_bytes();
    if bytes.get(index) != Some(&b':') {
        return None;
    }
    index += 1;
    let (minute, next) = parse_fixed_digits(value, index, 2)?;
    index = next;
    let mut second = 0;
    let mut millis = 0;
    if bytes.get(index) == Some(&b':') {
        let (parsed, next) = parse_fixed_digits(value, index + 1, 2)?;
        second = parsed;
        index = next;
        if bytes.get(index) == Some(&b'.') {
            let digit_start = index + 1;
            let mut end = digit_start;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
            if end == digit_start {
                return None;
            }
            let mut fraction = value.get(digit_start..end)?.to_owned();
            while fraction.len() < 3 {
                fraction.push('0');
            }
            millis = fraction.get(..3)?.parse().ok()?;
            index = end;
        }
    }
    Some((hour, minute, second, millis, index))
}

/// ISO 8601 date-time parsing per the ECMA-262 Date Time String Format.
/// Missing time fields default to the start of the UTC day; a missing offset
/// on a date-time form means local time, which is UTC in this engine.
fn parse_iso_date(value: &str) -> Option<f64> {
    let bytes = value.as_bytes();
    let (year, mut index) = parse_iso_year(value, 0)?;
    let mut month = 1;
    let mut day = 1;
    if bytes.get(index) == Some(&b'-') {
        let (parsed, next) = parse_fixed_digits(value, index + 1, 2)?;
        month = parsed;
        index = next;
        if bytes.get(index) == Some(&b'-') {
            let (parsed, next) = parse_fixed_digits(value, index + 1, 2)?;
            day = parsed;
            index = next;
        }
    }
    let mut hour = 0;
    let mut minute = 0;
    let mut second = 0;
    let mut millis = 0;
    if matches!(bytes.get(index), Some(b'T' | b't')) {
        let (h, m, s, ms, next) = parse_time_of_day(value, index + 1)?;
        hour = h;
        minute = m;
        second = s;
        millis = ms;
        index = next;
    }
    let mut offset_minutes = 0_i64;
    match bytes.get(index) {
        None => {}
        Some(b'Z' | b'z') => index += 1,
        Some(sign @ (b'+' | b'-')) => {
            let sign = if *sign == b'-' { -1_i64 } else { 1 };
            let (hours, next) = parse_fixed_digits(value, index + 1, 2)?;
            index = next;
            let minutes = if bytes.get(index) == Some(&b':') {
                let (minutes, next) = parse_fixed_digits(value, index + 1, 2)?;
                index = next;
                minutes
            } else if bytes.get(index).is_some_and(u8::is_ascii_digit) {
                let (minutes, next) = parse_fixed_digits(value, index, 2)?;
                index = next;
                minutes
            } else {
                0
            };
            offset_minutes = sign * i64::from(hours * 60 + minutes);
        }
        Some(_) => return None,
    }
    if index != bytes.len() {
        return None;
    }
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    if !(0..=24).contains(&hour) || minute > 59 || second > 59 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let time = (f64::from(hour) * 3_600_000.0
        + f64::from(minute) * 60_000.0
        + f64::from(second) * 1_000.0)
        + f64::from(millis);
    let ms = days as f64 * MS_PER_DAY + time - offset_minutes as f64 * 60_000.0;
    Some(JsRuntime::time_clip(ms))
}

fn parse_time_token(token: &str) -> Option<(i64, i64, i64, i64)> {
    let (clock, fraction) = token
        .split_once('.')
        .map_or((token, None), |(c, f)| (c, Some(f)));
    let mut parts = clock.split(':');
    let hour = parts.next()?.parse::<i64>().ok()?;
    let minute = parts.next()?.parse::<i64>().ok()?;
    let second = match parts.next() {
        None => 0,
        Some(text) => text.parse::<i64>().ok()?,
    };
    if parts.next().is_some() {
        return None;
    }
    let millis = match fraction {
        None => 0,
        Some(digits) => {
            if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            let mut padded = digits.to_owned();
            while padded.len() < 3 {
                padded.push('0');
            }
            padded.get(..3)?.parse::<i64>().ok()?
        }
    };
    Some((hour, minute, second, millis))
}

fn parse_zone_token(token: &str) -> Option<i64> {
    if token == "Z" || token.eq_ignore_ascii_case("GMT") || token.eq_ignore_ascii_case("UTC") {
        return Some(0);
    }
    let upper = token.to_ascii_uppercase();
    let rest = if let Some(rest) = upper.strip_prefix("GMT") {
        rest
    } else if let Some(rest) = upper.strip_prefix("UTC") {
        rest
    } else if token.starts_with(['+', '-']) {
        token
    } else {
        return None;
    };
    if rest.is_empty() {
        return Some(0);
    }
    let sign = if rest.starts_with('-') { -1_i64 } else { 1 };
    let digits = rest.trim_start_matches(['+', '-']);
    let (hours, minutes) = if let Some((hours, minutes)) = digits.split_once(':') {
        (hours.parse::<i64>().ok()?, minutes.parse::<i64>().ok()?)
    } else if digits.len() == 4 {
        (digits[..2].parse().ok()?, digits[2..].parse().ok()?)
    } else {
        return None;
    };
    Some(sign * (hours * 60 + minutes))
}

fn is_weekday_token(token: &str) -> bool {
    const WEEKDAYS: [&str; 14] = [
        "Sun",
        "Mon",
        "Tue",
        "Wed",
        "Thu",
        "Fri",
        "Sat",
        "Sunday",
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
    ];
    WEEKDAYS
        .iter()
        .any(|weekday| token.eq_ignore_ascii_case(weekday))
}

/// Legacy `toString`/`toUTCString`/`toDateString` forms, kept permissive
/// because ECMA-262 leaves their exact grammar implementation-defined.
fn parse_legacy_date(value: &str) -> Option<f64> {
    let mut text = value;
    if let Some(open) = text.rfind('(')
        && text.ends_with(')')
    {
        text = &text[..open];
    }
    let normalized: String = text
        .chars()
        .map(|character| if character == ',' { ' ' } else { character })
        .collect();
    let mut month = None;
    let mut day = None;
    let mut year = None;
    let mut hour = 0_i64;
    let mut minute = 0_i64;
    let mut second = 0_i64;
    let mut millis = 0_i64;
    let mut offset_minutes = 0_i64;
    for token in normalized.split_whitespace() {
        if let Some(index) = DATE_MONTHS
            .iter()
            .position(|name| token.eq_ignore_ascii_case(name))
        {
            month = Some(index as i32 + 1);
            continue;
        }
        if is_weekday_token(token) {
            continue;
        }
        if token.contains(':') {
            let parsed = parse_time_token(token)?;
            hour = parsed.0;
            minute = parsed.1;
            second = parsed.2;
            millis = parsed.3;
            continue;
        }
        if let Some(offset) = parse_zone_token(token) {
            offset_minutes = offset;
            continue;
        }
        if token.bytes().all(|byte| byte.is_ascii_digit()) {
            if token.len() >= 4 {
                if year.is_none() {
                    year = token.parse().ok();
                    if year.is_some() {
                        continue;
                    }
                }
            } else if day.is_none() {
                day = token.parse().ok();
                if day.is_some() {
                    continue;
                }
            }
        }
        return None;
    }
    let month = month?;
    let day = day.unwrap_or(1);
    let year = year?;
    let days = days_from_civil(year, month, day);
    let time = ((hour * 3600 + minute * 60 + second) * 1000 + millis) as f64;
    Some(JsRuntime::time_clip(
        days as f64 * MS_PER_DAY + time - offset_minutes as f64 * 60_000.0,
    ))
}

#[cfg(test)]
mod tests {
    use super::MAX_TIME_VALUE;
    use crate::JsRuntime;
    use crate::JsValue;
    use render_dom::Dom;
    use render_html::parse_document;
    use url::Url;

    struct Harness {
        runtime: JsRuntime,
        dom: Dom,
    }

    impl Harness {
        fn new() -> Self {
            let mut parsed = parse_document("<!doctype html><p>date</p>");
            let url = Url::parse("https://example.test/date").expect("test URL");
            let runtime = JsRuntime::with_url(&parsed.dom, &url);
            let dom = std::mem::take(&mut parsed.dom);
            Self { runtime, dom }
        }

        fn eval(&mut self, source: &str) -> JsValue {
            self.runtime
                .execute(&mut self.dom, source)
                .map(|outcome| outcome.value)
                .expect("script executes")
        }

        /// Evaluate an expression and render it with `String(...)`.
        fn scalar(&mut self, expression: &str) -> String {
            match self.eval(&format!("String({expression})")) {
                JsValue::String(text) => text,
                other => panic!("script returned {other:?}, expected a string"),
            }
        }

        /// Execute a script and render its completion value as a string.
        fn string(&mut self, source: &str) -> String {
            self.eval(source).to_js_string()
        }
    }

    #[test]
    fn time_clip_normalizes_negative_zero_and_range() {
        assert_eq!(JsRuntime::time_clip(-0.0), 0.0);
        assert!(JsRuntime::time_clip(-0.0).is_sign_positive());
        assert_eq!(JsRuntime::time_clip(6.54321), 6.0);
        assert_eq!(JsRuntime::time_clip(-6.54321), -6.0);
        assert!(JsRuntime::time_clip(MAX_TIME_VALUE + 1.0).is_nan());
        assert!(JsRuntime::time_clip(-MAX_TIME_VALUE - 1.0).is_nan());
        assert!(JsRuntime::time_clip(f64::INFINITY).is_nan());
        assert!(JsRuntime::time_clip(f64::NAN).is_nan());
        assert_eq!(JsRuntime::time_clip(MAX_TIME_VALUE), MAX_TIME_VALUE);
    }

    #[test]
    fn make_date_time_follows_spec_arithmetic() {
        assert_eq!(
            JsRuntime::make_date_time(2016.0, 12.0, 1.0, 0.0, 0.0, 0.0, 0.0),
            1_483_228_800_000.0
        );
        assert_eq!(
            JsRuntime::make_date_time(99.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0),
            JsRuntime::make_date_time(1999.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0)
        );
        assert!(JsRuntime::make_date_time(f64::NAN, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0).is_nan());
        assert_eq!(
            JsRuntime::make_date_time(
                1970.0,
                0.0,
                1.0,
                80_063_993_375.0,
                29.0,
                1.0,
                -288_230_376_151_711_740.0,
            ),
            29_312.0
        );
        assert_eq!(
            JsRuntime::make_date_time(
                1970.0,
                0.0,
                213_503_982_336.0,
                0.0,
                0.0,
                0.0,
                -18_446_744_073_709_552_000.0,
            ),
            34_447_360.0
        );
    }

    #[test]
    fn parse_iso_dates_with_expanded_years_and_clipping() {
        assert_eq!(
            JsRuntime::parse_date_string("-271821-04-20T00:00:00.000Z"),
            Some(-MAX_TIME_VALUE)
        );
        assert_eq!(
            JsRuntime::parse_date_string("+275760-09-13T00:00:00.000Z"),
            Some(MAX_TIME_VALUE)
        );
        assert!(
            JsRuntime::parse_date_string("-271821-04-19T23:59:59.999Z")
                .expect("parsed")
                .is_nan()
        );
        assert!(
            JsRuntime::parse_date_string("+275760-09-13T00:00:00.001Z")
                .expect("parsed")
                .is_nan()
        );
        assert_eq!(JsRuntime::parse_date_string("-000000-03-31T00:45Z"), None);
        assert!(JsRuntime::parse_date_string("+000000-03-31T00:45Z").is_some());
    }

    #[test]
    fn parse_accepts_date_only_offsets_and_legacy_formats() {
        assert_eq!(JsRuntime::parse_date_string("1970"), Some(0.0));
        assert_eq!(JsRuntime::parse_date_string("1970-01-01"), Some(0.0));
        assert_eq!(
            JsRuntime::parse_date_string("1970-01-01T00:00:00"),
            Some(0.0)
        );
        assert_eq!(
            JsRuntime::parse_date_string("1970-01-01T01:00:00+01:00"),
            Some(0.0)
        );
        assert_eq!(
            JsRuntime::parse_date_string("Thu Jan 01 1970 00:00:00 GMT+0000"),
            Some(0.0)
        );
        assert_eq!(
            JsRuntime::parse_date_string("Thu, 01 Jan 1970 00:00:00 GMT"),
            Some(0.0)
        );
        assert_eq!(JsRuntime::parse_date_string("Thu Jan 01 1970"), Some(0.0));
        assert_eq!(JsRuntime::parse_date_string("not a date"), None);
    }

    #[test]
    fn formatters_emit_spec_shapes_and_pad_years() {
        assert_eq!(JsRuntime::format_date_iso(0.0), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            JsRuntime::format_date_iso(-MAX_TIME_VALUE),
            "-271821-04-20T00:00:00.000Z"
        );
        assert_eq!(
            JsRuntime::format_date_iso(MAX_TIME_VALUE),
            "+275760-09-13T00:00:00.000Z"
        );
        assert_eq!(
            JsRuntime::format_date_to_utc_string(0.0),
            "Thu, 01 Jan 1970 00:00:00 GMT"
        );
        assert_eq!(
            JsRuntime::format_date_utc(0.0),
            "Thu Jan 01 1970 00:00:00 GMT+0000"
        );
        assert_eq!(JsRuntime::format_date_utc(f64::NAN), "Invalid Date");
        assert_eq!(
            JsRuntime::format_date_to_utc_string(f64::NAN),
            "Invalid Date"
        );
        let twenty = JsRuntime::parse_date_string("0020-01-01T00:00:00Z").expect("parsed");
        assert_eq!(
            JsRuntime::format_date_utc(twenty),
            "Wed Jan 01 0020 00:00:00 GMT+0000"
        );
        assert_eq!(
            JsRuntime::format_date_to_utc_string(twenty),
            "Wed, 01 Jan 0020 00:00:00 GMT"
        );
    }

    #[test]
    fn own_formats_round_trip_through_parse() {
        assert_eq!(
            JsRuntime::parse_date_string(&JsRuntime::format_date_utc(0.0)),
            Some(0.0)
        );
        assert_eq!(
            JsRuntime::parse_date_string(&JsRuntime::format_date_to_utc_string(0.0)),
            Some(0.0)
        );
        assert_eq!(
            JsRuntime::parse_date_string(&JsRuntime::format_date_iso(0.0)),
            Some(0.0)
        );
    }

    #[test]
    fn date_constructor_clips_values_and_maps_two_digit_years() {
        let mut harness = Harness::new();
        assert_eq!(harness.scalar("new Date(-0).getTime()"), "0");
        assert_eq!(harness.scalar("new Date(6.54321).getTime()"), "6");
        assert_eq!(harness.scalar("new Date(-6.54321).getTime()"), "-6");
        assert_eq!(harness.scalar("new Date(8.64e15 + 1).getTime()"), "NaN");
        assert_eq!(harness.scalar("new Date(undefined).getTime()"), "NaN");
        assert_eq!(harness.scalar("new Date(99, 0, 1).getFullYear()"), "1999");
        assert_eq!(harness.scalar("new Date(0).getTimezoneOffset()"), "0");
    }

    #[test]
    fn date_utc_coerces_and_clips() {
        let mut harness = Harness::new();
        assert_eq!(
            harness.scalar("Date.UTC(1970, 0, 1, 80063993375, 29, 1, -288230376151711740)"),
            "29312"
        );
        assert_eq!(
            harness.scalar("Date.UTC(1970, 0, 213503982336, 0, 0, 0, -18446744073709552000)"),
            "34447360"
        );
        assert_eq!(harness.scalar("Date.UTC(2016, 13)"), "1485907200000");
        assert_eq!(harness.scalar("Date.UTC(1970)"), "0");
        assert_eq!(
            harness.scalar("Date.UTC(1970.9, 0.9, 1.9, 0.9, 0.9, 0.9, 0.9)"),
            "0"
        );
        assert_eq!(
            harness.scalar("Date.UTC(-1970.9, -0.9, -0.9, -0.9, -0.9, -0.9, -0.9)"),
            "-124334438400000"
        );
        assert_eq!(harness.scalar("Date.UTC(1970, 0, 1, 0, 0, 0, 1)"), "1");
        assert_eq!(harness.scalar("Date.UTC()"), "NaN");
        assert_eq!(harness.scalar("Date.UTC(0, 0, Infinity)"), "NaN");
        assert_eq!(harness.scalar("Date.UTC(275760, 8, 13, 0, 0, 0, 1)"), "NaN");
    }

    #[test]
    fn date_to_json_uses_generic_to_iso_string_lookup() {
        let mut harness = Harness::new();
        assert_eq!(
            harness.scalar(
                "Date.prototype.toJSON.call({ valueOf: function() { return NaN; }, \
                 get toISOString() { throw new Error('unused'); } })"
            ),
            "null"
        );
        assert_eq!(
            harness.scalar(
                "Date.prototype.toJSON.call({ valueOf: function() { return 1; }, \
                 toISOString: function() { return 'x'; } })"
            ),
            "x"
        );
        assert_eq!(
            harness.scalar("new Date(0).toJSON()"),
            "1970-01-01T00:00:00.000Z"
        );
        assert_eq!(
            harness.string(
                "var hint; var obj = {}; \
                 obj[Symbol.toPrimitive] = function(value) { hint = value; return 2; }; \
                 obj.toISOString = function() { return 'x'; }; \
                 Date.prototype.toJSON.call(obj); hint"
            ),
            "number"
        );
        assert_eq!(
            harness.string(
                "(function() { try { \
                     Date.prototype.toJSON.call({ get valueOf() { throw new RangeError('boom'); } }); \
                 } catch (error) { return error instanceof RangeError; } })()"
            ),
            "true"
        );
        assert_eq!(
            harness.string(
                "var num = new Number(-Infinity); \
                 num.toISOString = function() { throw new Error('unused'); }; \
                 Date.prototype.toJSON.call(num)"
            ),
            "null"
        );
        assert_eq!(harness.scalar("new Date(NaN).toJSON()"), "null");
    }

    #[test]
    fn date_parse_round_trips_own_formats() {
        let mut harness = Harness::new();
        assert_eq!(harness.scalar("Date.parse(new Date(0).toString())"), "0");
        assert_eq!(harness.scalar("Date.parse(new Date(0).toUTCString())"), "0");
        assert_eq!(harness.scalar("Date.parse(new Date(0).toISOString())"), "0");
        assert_eq!(
            harness.scalar("new Date('1970').toISOString()"),
            "1970-01-01T00:00:00.000Z"
        );
    }

    #[test]
    fn date_strings_handle_invalid_dates_and_negative_years() {
        let mut harness = Harness::new();
        assert_eq!(harness.string("new Date(NaN).toString()"), "Invalid Date");
        assert_eq!(
            harness.string("new Date(NaN).toUTCString()"),
            "Invalid Date"
        );
        assert_eq!(
            harness.string("new Date(NaN).toDateString()"),
            "Invalid Date"
        );
        assert_eq!(
            harness.string("new Date('-000001-07-01T00:00Z').toUTCString().split(' ')[3]"),
            "-0001"
        );
        assert_eq!(
            harness.string("new Date('0020-01-01T00:00:00Z').toDateString().split(' ')[3]"),
            "0020"
        );
    }

    #[test]
    fn set_time_clips_and_returns_the_clipped_value() {
        let mut harness = Harness::new();
        assert_eq!(
            harness.string("var d = new Date(0); d.setTime(8.64e15 + 1)"),
            "NaN"
        );
        assert_eq!(
            harness.string("var d = new Date(0); d.setTime('   +00200.000E-0002\\t')"),
            "2"
        );
        assert_eq!(harness.string("var d = new Date(0); d.setTime()"), "NaN");
        assert_eq!(
            harness.string("var d = new Date(0); d.setTime(3); d.getTime()"),
            "3"
        );
    }

    #[test]
    fn to_iso_string_throws_range_error_for_out_of_range_values() {
        let mut harness = Harness::new();
        assert_eq!(
            harness.string(
                "(function() { try { new Date(8.64e15 + 1).toISOString(); } \
                 catch (error) { return error instanceof RangeError; } })()"
            ),
            "true"
        );
    }
}
