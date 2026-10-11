//! `Temporal.PlainYearMonth`: a year and a month of the ISO calendar. It keeps
//! a reference day, which only matters when the value is turned into a date.

use super::format::{calendar_text, date_text, year_text};
use super::iso::{
    IsoDate, add_iso_date, date_within_limits, days_in_month, days_in_year, is_leap_year,
    month_code, regulate_date,
};
use super::options::{CalendarDisplay, UnitGroup};
use super::plain_date::order_dates;
use super::rounding::Unit;
use super::{Surface, TemporalKind, TemporalMethod, TemporalValue, argument, member};
use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::value::ObjectHost;
use render_dom::Dom;

impl JsRuntime {
    /// The year and month of `receiver`, or a `TypeError` for another value.
    fn year_month_of(&self, receiver: ObjectId) -> Result<IsoDate, JsError> {
        match self.temporal_host(receiver) {
            Some(TemporalValue::PlainYearMonth(date)) => Ok(date),
            _ => Err(JsError::type_error(
                "receiver is not a Temporal.PlainYearMonth",
            )),
        }
    }

    /// A new `PlainYearMonth` whose reference date is `date`, refused outside the range.
    fn year_month_value(&mut self, date: IsoDate) -> Result<JsValue, JsError> {
        if !date_within_limits(date) {
            return Err(
                self.range_error("Temporal.PlainYearMonth is outside the representable range")
            );
        }
        self.temporal_instance(
            TemporalKind::PlainYearMonth,
            TemporalValue::PlainYearMonth(date),
        )
    }

    /// `ToTemporalYearMonth(item, options)`: a `PlainYearMonth`, a date (its
    /// year and month), a property bag, or an ISO string.
    fn to_temporal_year_month(
        &mut self,
        dom: &mut Dom,
        item: &JsValue,
        options: Option<ObjectId>,
    ) -> Result<IsoDate, JsError> {
        match item {
            JsValue::Object(object) => {
                if let Some(value) = self.temporal_host(*object) {
                    let date = match value {
                        TemporalValue::PlainYearMonth(date) => date,
                        TemporalValue::PlainDate(date) => date,
                        TemporalValue::PlainDateTime(datetime) => datetime.date,
                        _ => {
                            return Err(JsError::type_error("expected a Temporal date-like value"));
                        }
                    };
                    self.overflow_option(dom, options)?;
                    return Ok(date);
                }
                let calendar = self.get_member(dom, *object, "calendar")?;
                self.calendar_id(&calendar)?;
                let bag = self.read_year_month_bag(dom, *object)?;
                let constrain = self.overflow_option(dom, options)?;
                self.year_month_from_bag(&bag, constrain)
            }
            JsValue::String(text) => {
                let Some(parsed) = super::parse::parse_year_month(&(text).to_lossy()) else {
                    return Err(self.range_error("invalid ISO year-month string"));
                };
                if parsed.utc {
                    return Err(
                        self.range_error("a year-month string may not have a UTC designator")
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
                // A full date in the string must exist, whatever `overflow` says;
                // the year-month keeps the reference day 1.
                regulate_date(
                    parsed.date.year,
                    i64::from(parsed.date.month),
                    i64::from(parsed.date.day),
                    false,
                )
                .filter(|date| date_within_limits(*date))
                .map(|date| IsoDate { day: 1, ..date })
                .ok_or_else(|| self.range_error("year-month string is out of range"))
            }
            _ => Err(JsError::type_error(
                "expected a Temporal.PlainYearMonth, an object or an ISO string",
            )),
        }
    }

    /// The year, month and monthCode of a year-month bag, in the order
    /// the specification reads them.
    fn read_year_month_bag(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
    ) -> Result<super::plain_date::DateBag, JsError> {
        let mut bag = super::plain_date::DateBag::default();
        let month = self.get_member(dom, object, "month")?;
        if !matches!(month, JsValue::Undefined) {
            bag.month = Some(self.to_integer_with_truncation(dom, &month)?);
        }
        let code = self.get_member(dom, object, "monthCode")?;
        if !matches!(code, JsValue::Undefined) {
            bag.month_code = Some(self.to_string_checked(dom, &code)?);
        }
        let year = self.get_member(dom, object, "year")?;
        if !matches!(year, JsValue::Undefined) {
            bag.year = Some(self.to_integer_with_truncation(dom, &year)?);
        }
        Ok(bag)
    }

    /// The year-month a bag names, with the reference day 1.
    fn year_month_from_bag(
        &mut self,
        bag: &super::plain_date::DateBag,
        constrain: bool,
    ) -> Result<IsoDate, JsError> {
        let reference = super::plain_date::DateBag {
            day: Some(1.0),
            ..bag.clone()
        };
        self.date_from_bag(&reference, constrain)
    }

    pub(super) fn plain_year_month_construct(
        &mut self,
        dom: &mut Dom,
        new_target: &JsValue,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let year = self.to_integer_with_truncation(dom, &argument(arguments, 0))?;
        let month = self.to_integer_with_truncation(dom, &argument(arguments, 1))?;
        self.calendar_id(&argument(arguments, 2))?;
        let reference = match argument(arguments, 3) {
            JsValue::Undefined => 1.0,
            value => self.to_integer_with_truncation(dom, &value)?,
        };
        if !(-1e6..=1e6).contains(&year)
            || !(-1e6..=1e6).contains(&month)
            || !(-1e6..=1e6).contains(&reference)
        {
            return Err(self.range_error("year-month fields are out of range"));
        }
        let Some(date) = regulate_date(year as i32, month as i64, reference as i64, false) else {
            return Err(self.range_error("year-month fields are out of range"));
        };
        if !date_within_limits(date) {
            return Err(
                self.range_error("Temporal.PlainYearMonth is outside the representable range")
            );
        }
        let prototype =
            self.temporal_prototype_for(dom, TemporalKind::PlainYearMonth, new_target)?;
        self.ensure_heap_capacity(1)?;
        Ok(JsValue::Object(self.realm.allocate_host(
            Some(prototype),
            ObjectHost::Temporal(TemporalValue::PlainYearMonth(date)),
        )))
    }

    /// `add` and `subtract` on the year and month, keeping the reference day.
    fn year_month_add(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
        sign: i64,
    ) -> Result<JsValue, JsError> {
        let date = self.year_month_of(receiver)?;
        let duration = self.to_temporal_duration(dom, &argument(arguments, 0))?;
        let options = Self::options_object(&argument(arguments, 1))?;
        let constrain = self.overflow_option(dom, options)?;
        let exact = duration.exact();
        let days = exact[3] as i64
            + super::duration::balance_time(duration.time_nanos(), Unit::Day)[0] as i64;
        let result = add_iso_date(
            date,
            sign * exact[0] as i64,
            sign * exact[1] as i64,
            sign * exact[2] as i64,
            sign * days,
            constrain,
        )
        .ok_or_else(|| self.range_error("year-month arithmetic is out of range"))?;
        let kept = IsoDate {
            day: date.day,
            ..result
        };
        self.year_month_value(kept)
    }

    /// `until` and `since`: the year and month difference, from the first of
    /// the two reference dates, as a Duration of years, months and days.
    fn year_month_difference(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
        since: bool,
    ) -> Result<JsValue, JsError> {
        let this = self.year_month_of(receiver)?;
        let other_value = self.to_temporal_year_month(dom, &argument(arguments, 0), None)?;
        let options = Self::options_object(&argument(arguments, 1))?;
        let settings = self.difference_settings(
            dom,
            options,
            since,
            UnitGroup::Date,
            Unit::Month,
            Unit::Year,
        )?;
        let from = IsoDate { day: 1, ..this };
        let to = IsoDate {
            day: 1,
            ..other_value
        };
        let (years, months, weeks, days) =
            super::iso::difference_iso_date(from, to, settings.largest);
        let record = super::duration::DurationRecord::from_array([
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
        let rounded = if settings.smallest == Unit::Month && settings.increment == 1 {
            record
        } else {
            self.round_record(
                record,
                settings.largest,
                settings.smallest,
                settings.increment,
                settings.mode,
                Some(from),
            )?
        };
        let rounded = if since { rounded.negated() } else { rounded };
        self.temporal_instance(TemporalKind::Duration, TemporalValue::Duration(rounded))
    }

    /// `toString`: `YYYY-MM`, with the reference day and calendar when
    /// `calendarName` asks for the calendar.
    fn year_month_to_string(
        &mut self,
        dom: &mut Dom,
        date: IsoDate,
        options: &JsValue,
    ) -> Result<String, JsError> {
        let options = Self::options_object(options)?;
        let display = self.calendar_name_option(dom, options)?;
        Ok(match display {
            CalendarDisplay::Auto | CalendarDisplay::Never => {
                format!("{}-{:02}", year_text(date.year), date.month)
            }
            _ => format!("{}{}", date_text(date), calendar_text(display)),
        })
    }

    /// The `PlainYearMonth` statics, getters and methods, dispatched by name.
    pub(super) fn plain_year_month_native(
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
                let date = self.to_temporal_year_month(dom, &first, options)?;
                self.year_month_value(date)
            }
            TemporalMethod::Compare => {
                let one = self.to_temporal_year_month(dom, &first, None)?;
                let two = self.to_temporal_year_month(dom, &argument(arguments, 1), None)?;
                Ok(JsValue::Number(f64::from(order_dates(
                    IsoDate { day: 1, ..one },
                    IsoDate { day: 1, ..two },
                ))))
            }
            TemporalMethod::With => {
                let date = self.year_month_of(receiver)?;
                let JsValue::Object(object) = first else {
                    return Err(JsError::type_error("with() requires an object"));
                };
                let calendar = self.get_member(dom, object, "calendar")?;
                if !matches!(calendar, JsValue::Undefined) {
                    return Err(JsError::type_error("with() does not accept a calendar"));
                }
                let bag = self.read_year_month_bag(dom, object)?;
                if bag.month.is_none() && bag.month_code.is_none() && bag.year.is_none() {
                    return Err(JsError::type_error("with() requires at least one field"));
                }
                let options = Self::options_object(&argument(arguments, 1))?;
                let constrain = self.overflow_option(dom, options)?;
                let replaces_month = bag.month.is_some() || bag.month_code.is_some();
                let merged = super::plain_date::DateBag {
                    day: Some(f64::from(date.day)),
                    month: if replaces_month {
                        bag.month
                    } else {
                        Some(f64::from(date.month))
                    },
                    month_code: if replaces_month {
                        bag.month_code
                    } else {
                        Some(month_code(date.month))
                    },
                    year: Some(bag.year.unwrap_or(f64::from(date.year))),
                };
                let result = self.date_from_bag(&merged, constrain)?;
                self.year_month_value(result)
            }
            TemporalMethod::Add => self.year_month_add(dom, receiver, arguments, 1),
            TemporalMethod::Subtract => self.year_month_add(dom, receiver, arguments, -1),
            TemporalMethod::Until => self.year_month_difference(dom, receiver, arguments, false),
            TemporalMethod::Since => self.year_month_difference(dom, receiver, arguments, true),
            TemporalMethod::Equals => {
                let date = self.year_month_of(receiver)?;
                let other = self.to_temporal_year_month(dom, &first, None)?;
                Ok(JsValue::Boolean(date == other))
            }
            TemporalMethod::ToString => {
                let date = self.year_month_of(receiver)?;
                Ok(JsValue::String(
                    self.year_month_to_string(dom, date, &first)?.into(),
                ))
            }
            TemporalMethod::ToJSON | TemporalMethod::ToLocaleString => {
                let date = self.year_month_of(receiver)?;
                Ok(JsValue::String(
                    (format!("{}-{:02}", year_text(date.year), date.month)).into(),
                ))
            }
            TemporalMethod::ValueOf => Err(JsError::type_error(
                "Temporal.PlainYearMonth cannot be converted to a primitive; use compare() or toString()",
            )),
            TemporalMethod::ToPlainDate => {
                let date = self.year_month_of(receiver)?;
                let JsValue::Object(object) = first else {
                    return Err(JsError::type_error(
                        "toPlainDate() requires an object with a day",
                    ));
                };
                let day = self.get_member(dom, object, "day")?;
                if matches!(day, JsValue::Undefined) {
                    return Err(JsError::type_error("toPlainDate() requires a day"));
                }
                let day = self.to_integer_with_truncation(dom, &day)?;
                let Some(full) = regulate_date(date.year, i64::from(date.month), day as i64, true)
                else {
                    return Err(self.range_error("day is out of range"));
                };
                self.temporal_instance(TemporalKind::PlainDate, TemporalValue::PlainDate(full))
            }
            other => {
                let date = self.year_month_of(receiver)?;
                plain_year_month_getter(date, other)
            }
        }
    }
}

/// The `PlainYearMonth` getters, derived from the year and month.
fn plain_year_month_getter(date: IsoDate, method: TemporalMethod) -> Result<JsValue, JsError> {
    Ok(match method {
        TemporalMethod::CalendarId => JsValue::String(("iso8601".to_owned()).into()),
        TemporalMethod::Era | TemporalMethod::EraYear => JsValue::Undefined,
        TemporalMethod::Year => JsValue::Number(f64::from(date.year)),
        TemporalMethod::Month => JsValue::Number(f64::from(date.month)),
        TemporalMethod::MonthCode => JsValue::String((month_code(date.month)).into()),
        TemporalMethod::DaysInYear => JsValue::Number(f64::from(days_in_year(date.year))),
        TemporalMethod::DaysInMonth => {
            JsValue::Number(f64::from(days_in_month(date.year, date.month)))
        }
        TemporalMethod::MonthsInYear => JsValue::Number(12.0),
        TemporalMethod::InLeapYear => JsValue::Boolean(is_leap_year(date.year)),
        _ => {
            return Err(JsError::type_error(
                "unsupported Temporal.PlainYearMonth member",
            ));
        }
    })
}

/// The members of `Temporal.PlainYearMonth` and its prototype.
pub(super) const SURFACE: Surface = Surface {
    constructor_length: Some(2),
    statics: &[
        member("from", TemporalMethod::From, 1),
        member("compare", TemporalMethod::Compare, 2),
    ],
    methods: &[
        member("with", TemporalMethod::With, 1),
        member("add", TemporalMethod::Add, 1),
        member("subtract", TemporalMethod::Subtract, 1),
        member("until", TemporalMethod::Until, 1),
        member("since", TemporalMethod::Since, 1),
        member("equals", TemporalMethod::Equals, 1),
        member("toString", TemporalMethod::ToString, 0),
        member("toJSON", TemporalMethod::ToJSON, 0),
        member("toLocaleString", TemporalMethod::ToLocaleString, 0),
        member("valueOf", TemporalMethod::ValueOf, 0),
        member("toPlainDate", TemporalMethod::ToPlainDate, 1),
    ],
    getters: &[
        ("calendarId", TemporalMethod::CalendarId),
        ("era", TemporalMethod::Era),
        ("eraYear", TemporalMethod::EraYear),
        ("year", TemporalMethod::Year),
        ("month", TemporalMethod::Month),
        ("monthCode", TemporalMethod::MonthCode),
        ("daysInYear", TemporalMethod::DaysInYear),
        ("daysInMonth", TemporalMethod::DaysInMonth),
        ("monthsInYear", TemporalMethod::MonthsInYear),
        ("inLeapYear", TemporalMethod::InLeapYear),
    ],
};
