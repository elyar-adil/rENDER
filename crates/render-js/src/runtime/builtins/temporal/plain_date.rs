//! `Temporal.PlainDate`: a calendar date in the ISO 8601 calendar, with no
//! time of day and no zone.

use super::duration::{DurationRecord, balance_time};
use super::format::{calendar_text, date_text};
use super::iso::{
    IsoDate, IsoDateTime, MIDNIGHT, add_iso_date, date_within_limits, day_of_week, day_of_year,
    days_in_month, days_in_year, difference_iso_date, is_leap_year, iso_week, month_code,
    regulate_date,
};
use super::options::UnitGroup;
use super::rounding::Unit;
use super::{Surface, TemporalKind, TemporalMethod, TemporalValue, argument, member};
use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::utf16::JsString;
use render_dom::Dom;

/// The date fields a property bag contributes, after each has been read and
/// converted, in the order the specification reads them.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct DateBag {
    pub(super) day: Option<f64>,
    pub(super) month: Option<f64>,
    pub(super) month_code: Option<String>,
    pub(super) year: Option<f64>,
}

impl JsRuntime {
    /// The date of `receiver`, or a `TypeError` for another value.
    pub(super) fn date_of(&self, receiver: ObjectId) -> Result<IsoDate, JsError> {
        match self.temporal_host(receiver) {
            Some(TemporalValue::PlainDate(date)) => Ok(date),
            _ => Err(JsError::type_error("receiver is not a Temporal.PlainDate")),
        }
    }

    /// A new `PlainDate` of `date`, refused outside the representable range.
    pub(super) fn plain_date_value(&mut self, date: IsoDate) -> Result<JsValue, JsError> {
        if !date_within_limits(date) {
            return Err(self.range_error("Temporal.PlainDate is outside the representable range"));
        }
        self.temporal_instance(TemporalKind::PlainDate, TemporalValue::PlainDate(date))
    }

    /// The fields of a property bag, read as `PrepareCalendarFields` reads
    /// them for the ISO calendar: `calendar` first, then `day`, `month`,
    /// `monthCode` and `year`, each converted as it is read.
    pub(super) fn read_date_bag(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
    ) -> Result<DateBag, JsError> {
        let calendar = self.get_member(dom, object, "calendar")?;
        self.calendar_id(&calendar)?;
        self.read_date_fields(dom, object)
    }

    /// The date fields of a property bag, without the calendar.
    pub(super) fn read_date_fields(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
    ) -> Result<DateBag, JsError> {
        let mut bag = DateBag {
            day: self.read_integer_field(dom, object, "day")?,
            month: self.read_integer_field(dom, object, "month")?,
            ..DateBag::default()
        };
        let month_code = self.get_member(dom, object, "monthCode")?;
        if !matches!(month_code, JsValue::Undefined) {
            let code = self.to_string_checked(dom, &month_code)?;
            self.check_month_code_syntax(&code)?;
            bag.month_code = Some(code);
        }
        bag.year = self.read_integer_field(dom, object, "year")?;
        Ok(bag)
    }

    /// `Get` then `ToIntegerWithTruncation` for one numeric field, or `None`
    /// when the property is `undefined`.
    fn read_integer_field(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
        name: &str,
    ) -> Result<Option<f64>, JsError> {
        let value = self.get_member(dom, object, name)?;
        if matches!(value, JsValue::Undefined) {
            return Ok(None);
        }
        self.to_integer_with_truncation(dom, &value).map(Some)
    }

    /// The syntax of a `monthCode`: `M` and two digits, then an optional `L`.
    /// Read as each property is, so a malformed code is refused before later fields.
    pub(super) fn check_month_code_syntax(&mut self, code: &str) -> Result<(), JsError> {
        let digits = code
            .strip_prefix('M')
            .map(|rest| rest.strip_suffix('L').unwrap_or(rest));
        let well_formed = digits.is_some_and(|digits| {
            digits.len() == 2 && digits.bytes().all(|byte| byte.is_ascii_digit())
        });
        if well_formed {
            Ok(())
        } else {
            Err(self.range_error("monthCode must be M followed by two digits"))
        }
    }

    /// The month a well-formed `monthCode` names in the ISO calendar: `M01`
    /// through `M12`, without the leap-month `L` suffix.
    pub(super) fn month_from_code(&mut self, code: &str) -> Result<f64, JsError> {
        self.check_month_code_syntax(code)?;
        match code
            .strip_prefix('M')
            .and_then(|digits| digits.parse::<u8>().ok())
        {
            Some(month @ 1..=12) => Ok(f64::from(month)),
            _ => Err(self.range_error("monthCode is not a month of the ISO calendar")),
        }
    }

    /// `ISODateFromFields`: the date a bag names, with `overflow` deciding an
    /// out-of-range month or day (constrained when `constrain`, else refused).
    /// Missing required fields are a `TypeError`, checked before any range.
    pub(super) fn date_from_bag(
        &mut self,
        bag: &DateBag,
        constrain: bool,
    ) -> Result<IsoDate, JsError> {
        let Some(year) = bag.year else {
            return Err(JsError::type_error("a date needs a year"));
        };
        let Some(day) = bag.day else {
            return Err(JsError::type_error("a date needs a day"));
        };
        if bag.month.is_none() && bag.month_code.is_none() {
            return Err(JsError::type_error("a date needs a month or monthCode"));
        }
        let month = match (bag.month, bag.month_code.as_deref()) {
            (Some(month), Some(code)) => {
                let from_code = self.month_from_code(code)?;
                if from_code != month {
                    return Err(self.range_error("month and monthCode disagree"));
                }
                month
            }
            (Some(month), None) => month,
            (None, Some(code)) => self.month_from_code(code)?,
            (None, None) => return Err(JsError::type_error("a date needs a month or monthCode")),
        };
        if !(1.0..=1e9).contains(&day) || !(1.0..=1e9).contains(&month) {
            return Err(self.range_error("month and day must be positive"));
        }
        if !(-1e6..=1e6).contains(&year) {
            return Err(self.range_error("Temporal.PlainDate is outside the representable range"));
        }
        let Some(date) = regulate_date(year as i32, month as i64, day as i64, constrain) else {
            return Err(self.range_error("date fields are out of range"));
        };
        if !date_within_limits(date) {
            return Err(self.range_error("Temporal.PlainDate is outside the representable range"));
        }
        Ok(date)
    }

    /// `ToTemporalDate(item, options)`. A `PlainDate` or `PlainDateTime` is read
    /// for its date; a property bag is read field by field; a string is
    /// parsed. The `overflow` option is read in every object and string case,
    /// as the specification's order requires. `options` of `None` means the
    /// default, which constrains.
    pub(super) fn to_temporal_plain_date(
        &mut self,
        dom: &mut Dom,
        item: &JsValue,
        options: Option<ObjectId>,
    ) -> Result<IsoDate, JsError> {
        match item {
            JsValue::Object(object) => {
                if let Some(value) = self.temporal_host(*object) {
                    let date = match value {
                        TemporalValue::PlainDate(date) => date,
                        TemporalValue::PlainDateTime(datetime) => datetime.date,
                        _ => {
                            return Err(JsError::type_error("expected a Temporal date-like value"));
                        }
                    };
                    self.overflow_option(dom, options)?;
                    return Ok(date);
                }
                let bag = self.read_date_bag(dom, *object)?;
                let constrain = self.overflow_option(dom, options)?;
                self.date_from_bag(&bag, constrain)
            }
            JsValue::String(text) => {
                // The string is checked before the options are read, and its
                // date is always refused when impossible: `overflow` does not
                // apply to an ISO string.
                let Some(parsed) = super::parse::parse_date_time(&(text).to_lossy()) else {
                    return Err(self.range_error("invalid ISO date string"));
                };
                if parsed.utc {
                    return Err(self.range_error("a date string may not have a UTC designator"));
                }
                if parsed
                    .calendar
                    .as_deref()
                    .is_some_and(|calendar| calendar != "iso8601")
                {
                    return Err(self.range_error("only the iso8601 calendar is supported"));
                }
                self.overflow_option(dom, options)?;
                regulate_date(
                    parsed.date.year,
                    i64::from(parsed.date.month),
                    i64::from(parsed.date.day),
                    false,
                )
                .filter(|date| date_within_limits(*date))
                .ok_or_else(|| self.range_error("date string is out of range"))
            }
            _ => Err(JsError::type_error(
                "expected a Temporal.PlainDate, an object or an ISO string",
            )),
        }
    }

    /// `ToTemporalDate` for `relativeTo` and `compare`: no options, so the
    /// overflow is constrained.
    pub(super) fn to_relative_plain_date(
        &mut self,
        dom: &mut Dom,
        item: &JsValue,
    ) -> Result<IsoDate, JsError> {
        self.to_temporal_plain_date(dom, item, None)
    }

    pub(super) fn plain_date_construct(
        &mut self,
        dom: &mut Dom,
        new_target: &JsValue,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let year = self.to_integer_with_truncation(dom, &argument(arguments, 0))?;
        let month = self.to_integer_with_truncation(dom, &argument(arguments, 1))?;
        let day = self.to_integer_with_truncation(dom, &argument(arguments, 2))?;
        let calendar = argument(arguments, 3);
        self.calendar_id(&calendar)?;
        if !(-1e6..=1e6).contains(&year)
            || !(-1e6..=1e6).contains(&month)
            || !(-1e6..=1e6).contains(&day)
        {
            return Err(self.range_error("date fields are out of range"));
        }
        let Some(date) = regulate_date(year as i32, month as i64, day as i64, false) else {
            return Err(self.range_error("date fields are out of range"));
        };
        if !date_within_limits(date) {
            return Err(self.range_error("Temporal.PlainDate is outside the representable range"));
        }
        let prototype = self.temporal_prototype_for(dom, TemporalKind::PlainDate, new_target)?;
        self.ensure_heap_capacity(1)?;
        Ok(JsValue::Object(self.realm.allocate_host(
            Some(prototype),
            crate::value::ObjectHost::Temporal(TemporalValue::PlainDate(date)),
        )))
    }

    /// `AddDate`, shared by `add` and `subtract`: the date fields of the
    /// duration plus its time part balanced into whole days.
    fn add_to_date(
        &mut self,
        dom: &mut Dom,
        date: IsoDate,
        item: &JsValue,
        options: Option<ObjectId>,
        sign: i64,
    ) -> Result<JsValue, JsError> {
        let duration = self.to_temporal_duration(dom, item)?;
        let constrain = self.overflow_option(dom, options)?;
        let balanced_days = balance_time(duration.time_nanos(), Unit::Day)[0];
        let exact = duration.exact();
        let result = add_iso_date(
            date,
            sign * exact[0] as i64,
            sign * exact[1] as i64,
            sign * exact[2] as i64,
            sign * (exact[3] as i64 + balanced_days as i64),
            constrain,
        )
        .ok_or_else(|| self.range_error("date arithmetic is out of range"))?;
        self.plain_date_value(result)
    }

    /// `until` and `since`: the difference as a Duration, rounded by the
    /// options (whose largest unit defaults to days).
    fn date_difference(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
        since: bool,
    ) -> Result<JsValue, JsError> {
        let date = self.date_of(receiver)?;
        let other = self.to_relative_plain_date(dom, &argument(arguments, 0))?;
        let options = Self::options_object(&argument(arguments, 1))?;
        let settings =
            self.difference_settings(dom, options, since, UnitGroup::Date, Unit::Day, Unit::Day)?;
        let (years, months, weeks, days) = difference_iso_date(date, other, settings.largest);
        let record = DurationRecord::from_array([
            years as f64,
            months as f64,
            weeks as f64,
            days as f64,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
        ]);
        let rounded = if settings.smallest == Unit::Day && settings.increment == 1 {
            record
        } else {
            self.round_record(
                record,
                settings.largest,
                settings.smallest,
                settings.increment,
                settings.mode,
                Some(date),
            )?
        };
        let rounded = if since { rounded.negated() } else { rounded };
        self.temporal_instance(TemporalKind::Duration, TemporalValue::Duration(rounded))
    }

    /// `with(dateLike, options)`: the date with the named fields replaced.
    fn date_with(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let date = self.date_of(receiver)?;
        let JsValue::Object(object) = argument(arguments, 0) else {
            return Err(JsError::type_error("with() requires an object"));
        };
        let calendar = self.get_member(dom, object, "calendar")?;
        if !matches!(calendar, JsValue::Undefined) {
            return Err(JsError::type_error("with() does not accept a calendar"));
        }
        let mut bag = self.read_date_fields(dom, object)?;
        if bag.day.is_none()
            && bag.month.is_none()
            && bag.month_code.is_none()
            && bag.year.is_none()
        {
            return Err(JsError::type_error(
                "with() requires at least one date field",
            ));
        }
        let options = Self::options_of(arguments)?;
        let constrain = self.overflow_option(dom, options)?;
        // A month or a monthCode given replaces both of the receiver's month fields.
        let replaces_month = bag.month.is_some() || bag.month_code.is_some();
        let merged = DateBag {
            day: Some(bag.day.unwrap_or(f64::from(date.day))),
            month: if replaces_month {
                bag.month
            } else {
                Some(f64::from(date.month))
            },
            month_code: if replaces_month {
                bag.month_code.take()
            } else {
                Some(month_code(date.month))
            },
            year: Some(bag.year.unwrap_or(f64::from(date.year))),
        };
        let result = self.date_from_bag(&merged, constrain)?;
        self.plain_date_value(result)
    }

    /// The options bag argument at index 1, for the methods that take one.
    fn options_of(arguments: &[JsValue]) -> Result<Option<ObjectId>, JsError> {
        Self::options_object(&argument(arguments, 1))
    }

    /// `toString` with its `calendarName` option.
    fn date_to_string(
        &mut self,
        dom: &mut Dom,
        date: IsoDate,
        options: &JsValue,
    ) -> Result<String, JsError> {
        let options = Self::options_object(options)?;
        let display = self.calendar_name_option(dom, options)?;
        Ok(format!("{}{}", date_text(date), calendar_text(display)))
    }

    /// The `PlainDate` methods and statics, dispatched by name.
    pub(super) fn plain_date_native(
        &mut self,
        dom: &mut Dom,
        method: TemporalMethod,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let first = argument(arguments, 0);
        match method {
            TemporalMethod::From => {
                let options = Self::options_of(arguments)?;
                let date = self.to_temporal_plain_date(dom, &first, options)?;
                self.plain_date_value(date)
            }
            TemporalMethod::Compare => {
                let one = self.to_relative_plain_date(dom, &first)?;
                let two = self.to_relative_plain_date(dom, &argument(arguments, 1))?;
                Ok(JsValue::Number(f64::from(order_dates(one, two))))
            }
            TemporalMethod::Add | TemporalMethod::Subtract => {
                let date = self.date_of(receiver)?;
                let options = Self::options_of(arguments)?;
                let sign = if method == TemporalMethod::Add { 1 } else { -1 };
                self.add_to_date(dom, date, &first, options, sign)
            }
            TemporalMethod::Until => self.date_difference(dom, receiver, arguments, false),
            TemporalMethod::Since => self.date_difference(dom, receiver, arguments, true),
            TemporalMethod::With => self.date_with(dom, receiver, arguments),
            TemporalMethod::WithCalendar => {
                let date = self.date_of(receiver)?;
                self.calendar_id(&first)?;
                self.plain_date_value(date)
            }
            TemporalMethod::Equals => {
                let date = self.date_of(receiver)?;
                let other = self.to_relative_plain_date(dom, &first)?;
                Ok(JsValue::Boolean(date == other))
            }
            TemporalMethod::ToString => {
                let date = self.date_of(receiver)?;
                Ok(JsValue::String(JsString::from(
                    self.date_to_string(dom, date, &first)?,
                )))
            }
            TemporalMethod::ToJSON | TemporalMethod::ToLocaleString => {
                let date = self.date_of(receiver)?;
                Ok(JsValue::String((date_text(date)).into()))
            }
            TemporalMethod::ValueOf => Err(JsError::type_error(
                "Temporal.PlainDate cannot be converted to a primitive; use compare() or toString()",
            )),
            TemporalMethod::ToPlainDateTime => {
                let date = self.date_of(receiver)?;
                let time = if matches!(first, JsValue::Undefined) {
                    MIDNIGHT
                } else {
                    self.to_temporal_plain_time(dom, &first, None)?
                };
                self.temporal_instance(
                    TemporalKind::PlainDateTime,
                    TemporalValue::PlainDateTime(IsoDateTime { date, time }),
                )
            }
            TemporalMethod::ToPlainYearMonth => {
                let date = self.date_of(receiver)?;
                self.temporal_instance(
                    TemporalKind::PlainYearMonth,
                    TemporalValue::PlainYearMonth(IsoDate { day: 1, ..date }),
                )
            }
            TemporalMethod::ToPlainMonthDay => {
                let date = self.date_of(receiver)?;
                self.temporal_instance(
                    TemporalKind::PlainMonthDay,
                    TemporalValue::PlainMonthDay(IsoDate { year: 1972, ..date }),
                )
            }
            other => self.plain_date_getter(receiver, other),
        }
    }

    /// The `PlainDate` getters, derived from the stored date.
    fn plain_date_getter(
        &mut self,
        receiver: ObjectId,
        method: TemporalMethod,
    ) -> Result<JsValue, JsError> {
        let date = self.date_of(receiver)?;
        plain_date_getter_value(date, method)
    }
}

/// The calendar getter of a date, which a `PlainDateTime` answers too.
pub(super) fn plain_date_getter_value(
    date: IsoDate,
    method: TemporalMethod,
) -> Result<JsValue, JsError> {
    Ok(match method {
        TemporalMethod::CalendarId => JsValue::String(("iso8601".to_owned()).into()),
        TemporalMethod::Era | TemporalMethod::EraYear => JsValue::Undefined,
        TemporalMethod::Year => JsValue::Number(f64::from(date.year)),
        TemporalMethod::Month => JsValue::Number(f64::from(date.month)),
        TemporalMethod::MonthCode => JsValue::String((month_code(date.month)).into()),
        TemporalMethod::Day => JsValue::Number(f64::from(date.day)),
        TemporalMethod::DayOfWeek => JsValue::Number(f64::from(day_of_week(date))),
        TemporalMethod::DayOfYear => JsValue::Number(f64::from(day_of_year(date))),
        TemporalMethod::WeekOfYear => JsValue::Number(f64::from(iso_week(date).1)),
        TemporalMethod::YearOfWeek => JsValue::Number(f64::from(iso_week(date).0)),
        TemporalMethod::DaysInWeek => JsValue::Number(7.0),
        TemporalMethod::DaysInMonth => {
            JsValue::Number(f64::from(days_in_month(date.year, date.month)))
        }
        TemporalMethod::DaysInYear => JsValue::Number(f64::from(days_in_year(date.year))),
        TemporalMethod::MonthsInYear => JsValue::Number(12.0),
        TemporalMethod::InLeapYear => JsValue::Boolean(is_leap_year(date.year)),
        _ => return Err(JsError::type_error("unsupported Temporal.PlainDate member")),
    })
}

/// `CompareISODate` as -1, 0 or 1.
pub(super) fn order_dates(one: IsoDate, two: IsoDate) -> i8 {
    match one.cmp(&two) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

/// The members of `Temporal.PlainDate` and its prototype.
pub(super) const SURFACE: Surface = Surface {
    constructor_length: Some(3),
    statics: &[
        member("from", TemporalMethod::From, 1),
        member("compare", TemporalMethod::Compare, 2),
    ],
    methods: &[
        member("with", TemporalMethod::With, 1),
        member("withCalendar", TemporalMethod::WithCalendar, 1),
        member("add", TemporalMethod::Add, 1),
        member("subtract", TemporalMethod::Subtract, 1),
        member("until", TemporalMethod::Until, 1),
        member("since", TemporalMethod::Since, 1),
        member("equals", TemporalMethod::Equals, 1),
        member("toString", TemporalMethod::ToString, 0),
        member("toJSON", TemporalMethod::ToJSON, 0),
        member("toLocaleString", TemporalMethod::ToLocaleString, 0),
        member("valueOf", TemporalMethod::ValueOf, 0),
        member("toPlainDateTime", TemporalMethod::ToPlainDateTime, 0),
        member("toPlainYearMonth", TemporalMethod::ToPlainYearMonth, 0),
        member("toPlainMonthDay", TemporalMethod::ToPlainMonthDay, 0),
    ],
    getters: &[
        ("calendarId", TemporalMethod::CalendarId),
        ("era", TemporalMethod::Era),
        ("eraYear", TemporalMethod::EraYear),
        ("year", TemporalMethod::Year),
        ("month", TemporalMethod::Month),
        ("monthCode", TemporalMethod::MonthCode),
        ("day", TemporalMethod::Day),
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
