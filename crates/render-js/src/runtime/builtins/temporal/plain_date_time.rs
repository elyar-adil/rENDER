//! `Temporal.PlainDateTime`: a date and a time of day, with no zone.

use super::duration::{DurationRecord, balance_time};
use super::format::{calendar_text, date_text, time_text};
use super::iso::{
    IsoDate, IsoDateTime, IsoTime, NANOS_PER_DAY, add_iso_date, datetime_within_limits,
    difference_iso_date, time_from_nanos, time_nanos,
};
use super::options::UnitGroup;
use super::plain_date::{DateBag, order_dates};
use super::rounding::{RoundingMode, Unit, increment_is_valid, round_to_increment};
use super::{Surface, TemporalKind, TemporalMethod, TemporalValue, argument, member};
use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::value::ObjectHost;
use render_dom::Dom;

impl JsRuntime {
    /// The date-time of `receiver`, or a `TypeError` for another value.
    pub(super) fn datetime_of(&self, receiver: ObjectId) -> Result<IsoDateTime, JsError> {
        match self.temporal_host(receiver) {
            Some(TemporalValue::PlainDateTime(datetime)) => Ok(datetime),
            _ => Err(JsError::type_error(
                "receiver is not a Temporal.PlainDateTime",
            )),
        }
    }

    /// A new `PlainDateTime`, refused outside the representable range.
    pub(super) fn plain_date_time_value(
        &mut self,
        datetime: IsoDateTime,
    ) -> Result<JsValue, JsError> {
        if !datetime_within_limits(datetime) {
            return Err(
                self.range_error("Temporal.PlainDateTime is outside the representable range")
            );
        }
        self.temporal_instance(
            TemporalKind::PlainDateTime,
            TemporalValue::PlainDateTime(datetime),
        )
    }

    /// The date and time fields of a property bag, read in the alphabetical
    /// order the specification uses for `PrepareCalendarFields`, each converted
    /// as it is read. Returns the date fields and the time fields (`None` when absent).
    pub(super) fn read_datetime_bag(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
    ) -> Result<(DateBag, [Option<f64>; 6]), JsError> {
        let calendar = self.get_member(dom, object, "calendar")?;
        self.calendar_id(&calendar)?;
        let mut date = DateBag::default();
        let mut time = [None; 6];
        for name in [
            "day",
            "hour",
            "microsecond",
            "millisecond",
            "minute",
            "month",
            "monthCode",
            "nanosecond",
            "second",
            "year",
        ] {
            let value = self.get_member(dom, object, name)?;
            if matches!(value, JsValue::Undefined) {
                continue;
            }
            let number = match name {
                "monthCode" => {
                    let code = self.to_string_checked(dom, &value)?;
                    self.check_month_code_syntax(&code)?;
                    date.month_code = Some(code);
                    continue;
                }
                _ => self.to_integer_with_truncation(dom, &value)?,
            };
            match name {
                "day" => date.day = Some(number),
                "month" => date.month = Some(number),
                "year" => date.year = Some(number),
                "hour" => time[0] = Some(number),
                "minute" => time[1] = Some(number),
                "second" => time[2] = Some(number),
                "millisecond" => time[3] = Some(number),
                "microsecond" => time[4] = Some(number),
                _ => time[5] = Some(number),
            }
        }
        Ok((date, time))
    }

    /// `ToTemporalDateTime(item, options)`: a `PlainDateTime`, a `PlainDate` (at
    /// midnight), a property bag, or an ISO date-time string.
    fn to_temporal_datetime(
        &mut self,
        dom: &mut Dom,
        item: &JsValue,
        options: Option<ObjectId>,
    ) -> Result<IsoDateTime, JsError> {
        match item {
            JsValue::Object(object) => {
                if let Some(value) = self.temporal_host(*object) {
                    let datetime = match value {
                        TemporalValue::PlainDateTime(datetime) => datetime,
                        TemporalValue::PlainDate(date) => IsoDateTime {
                            date,
                            time: IsoTime::default(),
                        },
                        _ => {
                            return Err(JsError::type_error("expected a Temporal date-time value"));
                        }
                    };
                    self.overflow_option(dom, options)?;
                    return Ok(datetime);
                }
                let (date_bag, time_bag) = self.read_datetime_bag(dom, *object)?;
                if date_bag.day.is_none()
                    && date_bag.month.is_none()
                    && date_bag.month_code.is_none()
                    && date_bag.year.is_none()
                {
                    return Err(JsError::type_error("a date-time needs date fields"));
                }
                let constrain = self.overflow_option(dom, options)?;
                let date = self.date_from_bag(&date_bag, constrain)?;
                let time = self.time_from_bag(&time_bag, constrain)?;
                Ok(IsoDateTime { date, time })
            }
            JsValue::String(text) => {
                let Some(parsed) = super::parse::parse_date_time(&(text).to_lossy()) else {
                    return Err(self.range_error("invalid ISO date-time string"));
                };
                if parsed.utc {
                    return Err(
                        self.range_error("a date-time string may not have a UTC designator")
                    );
                }
                if parsed
                    .calendar
                    .as_deref()
                    .is_some_and(|calendar| calendar != "iso8601")
                {
                    return Err(self.range_error("only the iso8601 calendar is supported"));
                }
                self.overflow_option(dom, options)?;
                let date = super::iso::regulate_date(
                    parsed.date.year,
                    i64::from(parsed.date.month),
                    i64::from(parsed.date.day),
                    false,
                )
                .ok_or_else(|| self.range_error("date-time string is out of range"))?;
                let time = match parsed.time {
                    Some(time) => time,
                    None => super::iso::MIDNIGHT,
                };
                let datetime = IsoDateTime { date, time };
                if !datetime_within_limits(datetime) {
                    return Err(self
                        .range_error("Temporal.PlainDateTime is outside the representable range"));
                }
                Ok(datetime)
            }
            _ => Err(JsError::type_error(
                "expected a Temporal.PlainDateTime, an object or an ISO string",
            )),
        }
    }

    /// The time of a bag's time fields, with absent fields as zero.
    pub(super) fn time_from_bag(
        &mut self,
        fields: &[Option<f64>; 6],
        constrain: bool,
    ) -> Result<IsoTime, JsError> {
        let mut values = [0_i64; 6];
        for (slot, value) in values.iter_mut().zip(fields) {
            if let Some(value) = value {
                if !(-1e15..=1e15).contains(value) {
                    return Err(self.range_error("time fields are out of range"));
                }
                *slot = *value as i64;
            }
        }
        super::iso::regulate_time(values, constrain)
            .ok_or_else(|| self.range_error("time fields are out of range"))
    }

    pub(super) fn plain_date_time_construct(
        &mut self,
        dom: &mut Dom,
        new_target: &JsValue,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let year = self.to_integer_with_truncation(dom, &argument(arguments, 0))?;
        let month = self.to_integer_with_truncation(dom, &argument(arguments, 1))?;
        let day = self.to_integer_with_truncation(dom, &argument(arguments, 2))?;
        let mut time_fields = [None; 6];
        for (index, slot) in time_fields.iter_mut().enumerate() {
            let value = argument(arguments, index + 3);
            if !matches!(value, JsValue::Undefined) {
                *slot = Some(self.to_integer_with_truncation(dom, &value)?);
            }
        }
        self.calendar_id(&argument(arguments, 9))?;
        if !(-1e6..=1e6).contains(&year)
            || !(-1e6..=1e6).contains(&month)
            || !(-1e6..=1e6).contains(&day)
        {
            return Err(self.range_error("date fields are out of range"));
        }
        let Some(date) = super::iso::regulate_date(year as i32, month as i64, day as i64, false)
        else {
            return Err(self.range_error("date fields are out of range"));
        };
        let time = self.time_from_bag(&time_fields, false)?;
        let datetime = IsoDateTime { date, time };
        if !datetime_within_limits(datetime) {
            return Err(
                self.range_error("Temporal.PlainDateTime is outside the representable range")
            );
        }
        let prototype =
            self.temporal_prototype_for(dom, TemporalKind::PlainDateTime, new_target)?;
        self.ensure_heap_capacity(1)?;
        Ok(JsValue::Object(self.realm.allocate_host(
            Some(prototype),
            ObjectHost::Temporal(TemporalValue::PlainDateTime(datetime)),
        )))
    }

    /// `AddDateTime`: the time part is added to the time of day, and the whole
    /// days it carries join the date fields of the duration.
    fn datetime_add(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        item: &JsValue,
        options: &JsValue,
        sign: i64,
    ) -> Result<JsValue, JsError> {
        let datetime = self.datetime_of(receiver)?;
        let duration = self.to_temporal_duration(dom, item)?;
        let options = Self::options_object(options)?;
        let constrain = self.overflow_option(dom, options)?;
        let total = time_nanos(datetime.time) + i128::from(sign) * duration.time_nanos();
        let carry = total.div_euclid(NANOS_PER_DAY) as i64;
        let time = time_from_nanos(total.rem_euclid(NANOS_PER_DAY));
        let exact = duration.exact();
        let date = add_iso_date(
            datetime.date,
            sign * exact[0] as i64,
            sign * exact[1] as i64,
            sign * exact[2] as i64,
            sign * exact[3] as i64 + carry,
            constrain,
        )
        .ok_or_else(|| self.range_error("date-time arithmetic is out of range"))?;
        self.plain_date_time_value(IsoDateTime { date, time })
    }

    /// `DifferenceISODateTime`: the calendar difference of the dates, with the
    /// later date stepped back a day when the times of day run the other way.
    fn datetime_difference_record(
        from: IsoDateTime,
        to: IsoDateTime,
        largest: Unit,
    ) -> DurationRecord {
        let mut time_diff = time_nanos(to.time) - time_nanos(from.time);
        let time_sign = time_diff.signum() as i64;
        let date_sign = i64::from(order_dates(to.date, from.date));
        let mut adjusted = to.date;
        if time_sign != 0 && time_sign == -date_sign {
            adjusted = add_iso_date(to.date, 0, 0, 0, -time_sign, true).unwrap_or(to.date);
            time_diff += i128::from(time_sign) * NANOS_PER_DAY;
        }
        let date_largest = largest.min(Unit::Day);
        let (years, months, weeks, days) = difference_iso_date(from.date, adjusted, date_largest);
        let (days, parts) = if largest.nanos().is_some() {
            let total = i128::from(days) * NANOS_PER_DAY + time_diff;
            let parts = balance_time(total, largest);
            (parts[0], parts)
        } else {
            let parts = balance_time(time_diff, Unit::Day);
            (i128::from(days) + parts[0], parts)
        };
        let (years, months, weeks) = if largest.nanos().is_some() {
            (0, 0, 0)
        } else {
            (years, months, weeks)
        };
        let record = [
            years as f64,
            months as f64,
            weeks as f64,
            days as f64,
            parts[1] as f64,
            parts[2] as f64,
            parts[3] as f64,
            parts[4] as f64,
            parts[5] as f64,
            parts[6] as f64,
        ];
        DurationRecord::from_array(record)
    }

    /// `until` and `since` between two date-times.
    fn datetime_difference(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
        since: bool,
    ) -> Result<JsValue, JsError> {
        let from = self.datetime_of(receiver)?;
        let to = self.to_temporal_datetime(dom, &argument(arguments, 0), None)?;
        let options = Self::options_object(&argument(arguments, 1))?;
        let settings = self.difference_settings(
            dom,
            options,
            since,
            UnitGroup::DateTime,
            Unit::Nanosecond,
            Unit::Day,
        )?;
        let record = Self::datetime_difference_record(from, to, settings.largest);
        let rounded = if settings.smallest == Unit::Nanosecond && settings.increment == 1 {
            record
        } else {
            self.round_record(
                record,
                settings.largest,
                settings.smallest,
                settings.increment,
                settings.mode,
                Some(from.date),
            )?
        };
        let rounded = if since { rounded.negated() } else { rounded };
        self.temporal_instance(TemporalKind::Duration, TemporalValue::Duration(rounded))
    }

    /// `round(roundTo)`: the date-time rounded to a day or a time unit.
    fn datetime_round(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let datetime = self.datetime_of(receiver)?;
        let round_to = argument(arguments, 0);
        let (increment, mode, smallest) = match &round_to {
            JsValue::Undefined => {
                return Err(JsError::type_error("round requires an options argument"));
            }
            JsValue::String(text) => match Unit::parse(&(text).to_lossy()) {
                Some(unit) => (1, RoundingMode::HalfExpand, Some(unit)),
                None => return Err(self.range_error("invalid smallestUnit")),
            },
            other => {
                let options = Self::options_object(other)?;
                let increment = self.rounding_increment_option(dom, options)?;
                let mode = self.rounding_mode_option(dom, options, RoundingMode::HalfExpand)?;
                let smallest =
                    self.unit_in_group(dom, options, "smallestUnit", UnitGroup::DayAndTime, false)?;
                (increment, mode, smallest)
            }
        };
        let Some(unit) = smallest else {
            return Err(self.range_error("round requires smallestUnit"));
        };
        if !increment_is_valid(unit, increment) {
            return Err(self.range_error("roundingIncrement is not valid for this unit"));
        }
        let unit_nanos = unit.nanos().unwrap_or(NANOS_PER_DAY);
        let rounded = round_to_increment(time_nanos(datetime.time), unit_nanos * increment, mode);
        let carry = rounded.div_euclid(NANOS_PER_DAY) as i64;
        let date = add_iso_date(datetime.date, 0, 0, 0, carry, true)
            .ok_or_else(|| self.range_error("date-time is outside the representable range"))?;
        let time = time_from_nanos(rounded.rem_euclid(NANOS_PER_DAY));
        self.plain_date_time_value(IsoDateTime { date, time })
    }

    /// `toString` and `toJSON`: the date, the time at the requested precision,
    /// and the calendar annotation, from the rounded date-time.
    fn datetime_to_string(
        &mut self,
        dom: &mut Dom,
        datetime: IsoDateTime,
        options: &JsValue,
    ) -> Result<String, JsError> {
        let options = Self::options_object(options)?;
        let display = self.calendar_name_option(dom, options)?;
        let digits = self.fractional_digits_option(dom, options)?;
        let mode = self.rounding_mode_option(dom, options, RoundingMode::Trunc)?;
        let smallest = self.unit_in_group(
            dom,
            options,
            "smallestUnit",
            UnitGroup::MinuteAndFiner,
            false,
        )?;
        let (precision, unit_nanos) = super::instant::seconds_precision(smallest, digits);
        let rounded = round_to_increment(time_nanos(datetime.time), unit_nanos, mode);
        let carry = rounded.div_euclid(NANOS_PER_DAY) as i64;
        let date = add_iso_date(datetime.date, 0, 0, 0, carry, true).ok_or_else(|| {
            self.range_error("Temporal.PlainDateTime is outside the representable range")
        })?;
        let time = time_from_nanos(rounded.rem_euclid(NANOS_PER_DAY));
        if !datetime_within_limits(IsoDateTime { date, time }) {
            return Err(
                self.range_error("Temporal.PlainDateTime is outside the representable range")
            );
        }
        Ok(format!(
            "{}T{}{}",
            date_text(date),
            time_text(time, precision),
            calendar_text(display)
        ))
    }

    /// The `PlainDateTime` statics, getters and methods, dispatched by name.
    pub(super) fn plain_date_time_native(
        &mut self,
        dom: &mut Dom,
        method: TemporalMethod,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let first = argument(arguments, 0);
        match method {
            TemporalMethod::From => {
                let options = Self::options_object(&argument(arguments, 1))?;
                let datetime = self.to_temporal_datetime(dom, &first, options)?;
                self.plain_date_time_value(datetime)
            }
            TemporalMethod::Compare => {
                let one = self.to_temporal_datetime(dom, &first, None)?;
                let two = self.to_temporal_datetime(dom, &argument(arguments, 1), None)?;
                Ok(JsValue::Number(f64::from(order_datetimes(one, two))))
            }
            TemporalMethod::Add | TemporalMethod::Subtract => {
                let sign = if method == TemporalMethod::Add { 1 } else { -1 };
                self.datetime_add(dom, receiver, &first, &argument(arguments, 1), sign)
            }
            TemporalMethod::Until => self.datetime_difference(dom, receiver, arguments, false),
            TemporalMethod::Since => self.datetime_difference(dom, receiver, arguments, true),
            TemporalMethod::Round => self.datetime_round(dom, receiver, arguments),
            TemporalMethod::Equals => {
                let datetime = self.datetime_of(receiver)?;
                let other = self.to_temporal_datetime(dom, &first, None)?;
                Ok(JsValue::Boolean(datetime == other))
            }
            TemporalMethod::ToString => {
                let datetime = self.datetime_of(receiver)?;
                Ok(JsValue::String(
                    self.datetime_to_string(dom, datetime, &first)?.into(),
                ))
            }
            TemporalMethod::ToJSON | TemporalMethod::ToLocaleString => {
                let datetime = self.datetime_of(receiver)?;
                Ok(JsValue::String(
                    (format!(
                        "{}T{}",
                        date_text(datetime.date),
                        time_text(datetime.time, super::format::Precision::Auto)
                    ))
                    .into(),
                ))
            }
            TemporalMethod::ValueOf => Err(JsError::type_error(
                "Temporal.PlainDateTime cannot be converted to a primitive; use compare() or toString()",
            )),
            TemporalMethod::With => self.datetime_with(dom, receiver, arguments),
            TemporalMethod::WithPlainTime => {
                let datetime = self.datetime_of(receiver)?;
                let time = if matches!(first, JsValue::Undefined) {
                    super::iso::MIDNIGHT
                } else {
                    self.to_temporal_plain_time(dom, &first, None)?
                };
                self.plain_date_time_value(IsoDateTime {
                    date: datetime.date,
                    time,
                })
            }
            TemporalMethod::WithCalendar => {
                let datetime = self.datetime_of(receiver)?;
                self.calendar_id(&first)?;
                self.plain_date_time_value(datetime)
            }
            TemporalMethod::ToPlainDate => {
                let datetime = self.datetime_of(receiver)?;
                self.temporal_instance(
                    TemporalKind::PlainDate,
                    TemporalValue::PlainDate(datetime.date),
                )
            }
            TemporalMethod::ToPlainTime => {
                let datetime = self.datetime_of(receiver)?;
                self.temporal_instance(
                    TemporalKind::PlainTime,
                    TemporalValue::PlainTime(datetime.time),
                )
            }
            other => self.datetime_getter(receiver, other),
        }
    }

    /// `with(dateTimeLike, options)`: the date-time with the named fields replaced.
    fn datetime_with(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let datetime = self.datetime_of(receiver)?;
        let JsValue::Object(object) = argument(arguments, 0) else {
            return Err(JsError::type_error("with() requires an object"));
        };
        let calendar = self.get_member(dom, object, "calendar")?;
        if !matches!(calendar, JsValue::Undefined) {
            return Err(JsError::type_error("with() does not accept a calendar"));
        }
        let (date_bag, time_bag) = self.read_datetime_bag(dom, object)?;
        let any_time = time_bag.iter().any(Option::is_some);
        let any_date = date_bag.day.is_some()
            || date_bag.month.is_some()
            || date_bag.month_code.is_some()
            || date_bag.year.is_some();
        if !any_time && !any_date {
            return Err(JsError::type_error("with() requires at least one field"));
        }
        let options = Self::options_object(&argument(arguments, 1))?;
        let constrain = self.overflow_option(dom, options)?;
        let replaces_month = date_bag.month.is_some() || date_bag.month_code.is_some();
        let merged_date = DateBag {
            day: Some(date_bag.day.unwrap_or(f64::from(datetime.date.day))),
            month: if replaces_month {
                date_bag.month
            } else {
                Some(f64::from(datetime.date.month))
            },
            month_code: if replaces_month {
                date_bag.month_code
            } else {
                Some(super::iso::month_code(datetime.date.month))
            },
            year: Some(date_bag.year.unwrap_or(f64::from(datetime.date.year))),
        };
        let date = self.date_from_bag(&merged_date, constrain)?;
        let time = datetime.time;
        let base = [
            f64::from(time.hour),
            f64::from(time.minute),
            f64::from(time.second),
            f64::from(time.millisecond),
            f64::from(time.microsecond),
            f64::from(time.nanosecond),
        ];
        let mut merged_time = [None; 6];
        for (index, slot) in merged_time.iter_mut().enumerate() {
            *slot = Some(time_bag[index].unwrap_or(base[index]));
        }
        let time = self.time_from_bag(&merged_time, constrain)?;
        self.plain_date_time_value(IsoDateTime { date, time })
    }

    /// The `PlainDateTime` getters, derived from the stored date and time.
    fn datetime_getter(
        &mut self,
        receiver: ObjectId,
        method: TemporalMethod,
    ) -> Result<JsValue, JsError> {
        let datetime = self.datetime_of(receiver)?;
        let date: IsoDate = datetime.date;
        let time = datetime.time;
        Ok(match method {
            TemporalMethod::Hour => JsValue::Number(f64::from(time.hour)),
            TemporalMethod::Minute => JsValue::Number(f64::from(time.minute)),
            TemporalMethod::Second => JsValue::Number(f64::from(time.second)),
            TemporalMethod::Millisecond => JsValue::Number(f64::from(time.millisecond)),
            TemporalMethod::Microsecond => JsValue::Number(f64::from(time.microsecond)),
            TemporalMethod::Nanosecond => JsValue::Number(f64::from(time.nanosecond)),
            _ => return super::plain_date::plain_date_getter_value(date, method),
        })
    }
}

/// The date-time order as -1, 0 or 1, comparing dates and then times.
fn order_datetimes(one: IsoDateTime, two: IsoDateTime) -> i8 {
    match one.cmp(&two) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

/// The members of `Temporal.PlainDateTime` and its prototype.
pub(super) const SURFACE: Surface = Surface {
    constructor_length: Some(3),
    statics: &[
        member("from", TemporalMethod::From, 1),
        member("compare", TemporalMethod::Compare, 2),
    ],
    methods: &[
        member("with", TemporalMethod::With, 1),
        member("withPlainTime", TemporalMethod::WithPlainTime, 0),
        member("withCalendar", TemporalMethod::WithCalendar, 1),
        member("add", TemporalMethod::Add, 1),
        member("subtract", TemporalMethod::Subtract, 1),
        member("until", TemporalMethod::Until, 1),
        member("since", TemporalMethod::Since, 1),
        member("round", TemporalMethod::Round, 1),
        member("equals", TemporalMethod::Equals, 1),
        member("toString", TemporalMethod::ToString, 0),
        member("toJSON", TemporalMethod::ToJSON, 0),
        member("toLocaleString", TemporalMethod::ToLocaleString, 0),
        member("valueOf", TemporalMethod::ValueOf, 0),
        member("toPlainDate", TemporalMethod::ToPlainDate, 0),
        member("toPlainTime", TemporalMethod::ToPlainTime, 0),
    ],
    getters: &[
        ("calendarId", TemporalMethod::CalendarId),
        ("era", TemporalMethod::Era),
        ("eraYear", TemporalMethod::EraYear),
        ("year", TemporalMethod::Year),
        ("month", TemporalMethod::Month),
        ("monthCode", TemporalMethod::MonthCode),
        ("day", TemporalMethod::Day),
        ("hour", TemporalMethod::Hour),
        ("minute", TemporalMethod::Minute),
        ("second", TemporalMethod::Second),
        ("millisecond", TemporalMethod::Millisecond),
        ("microsecond", TemporalMethod::Microsecond),
        ("nanosecond", TemporalMethod::Nanosecond),
        ("dayOfWeek", TemporalMethod::DayOfWeek),
        ("dayOfYear", TemporalMethod::DayOfYear),
        ("weekOfYear", TemporalMethod::WeekOfYear),
        ("yearOfWeek", TemporalMethod::YearOfWeek),
        ("daysInWeek", TemporalMethod::DaysInWeek),
        ("daysInMonth", TemporalMethod::DaysInMonth),
        ("daysInYear", TemporalMethod::DaysInYear),
        ("monthsInYear", TemporalMethod::MonthsInYear),
        ("inLeapYear", TemporalMethod::InLeapYear),
    ],
};
