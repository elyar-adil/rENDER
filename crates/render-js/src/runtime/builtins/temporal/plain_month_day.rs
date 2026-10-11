//! `Temporal.PlainMonthDay`: a month and a day of the ISO calendar, with a
//! reference year (1972 unless the value says otherwise) kept for conversion.

use super::format::{calendar_text, date_text};
use super::iso::{IsoDate, date_within_limits, month_code, regulate_date};
use super::options::CalendarDisplay;
use super::{Surface, TemporalKind, TemporalMethod, TemporalValue, argument, member};
use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use crate::value::ObjectHost;
use render_dom::Dom;

/// The reference year a month-day keeps when none is given.
const DEFAULT_REFERENCE_YEAR: i32 = 1972;

impl JsRuntime {
    /// The month and day of `receiver`, or a `TypeError` for another value.
    fn month_day_of(&self, receiver: ObjectId) -> Result<IsoDate, JsError> {
        match self.temporal_host(receiver) {
            Some(TemporalValue::PlainMonthDay(date)) => Ok(date),
            _ => Err(JsError::type_error(
                "receiver is not a Temporal.PlainMonthDay",
            )),
        }
    }

    /// A new `PlainMonthDay` of `date` (its year is the reference year).
    fn month_day_value(&mut self, date: IsoDate) -> Result<JsValue, JsError> {
        self.temporal_instance(
            TemporalKind::PlainMonthDay,
            TemporalValue::PlainMonthDay(date),
        )
    }

    /// `ToTemporalMonthDay(item, options)`: a `PlainMonthDay`, a date (its month
    /// and day), a property bag, or an ISO string.
    fn to_temporal_month_day(
        &mut self,
        dom: &mut Dom,
        item: &JsValue,
        options: Option<ObjectId>,
    ) -> Result<IsoDate, JsError> {
        match item {
            JsValue::Object(object) => {
                if let Some(value) = self.temporal_host(*object) {
                    let date = match value {
                        TemporalValue::PlainMonthDay(date) => date,
                        TemporalValue::PlainDate(date) => IsoDate {
                            year: DEFAULT_REFERENCE_YEAR,
                            ..date
                        },
                        TemporalValue::PlainDateTime(datetime) => IsoDate {
                            year: DEFAULT_REFERENCE_YEAR,
                            ..datetime.date
                        },
                        _ => {
                            return Err(JsError::type_error("expected a Temporal date-like value"));
                        }
                    };
                    self.overflow_option(dom, options)?;
                    return Ok(date);
                }
                let calendar = self.get_member(dom, *object, "calendar")?;
                self.calendar_id(&calendar)?;
                let bag = self.read_month_day_bag(dom, *object)?;
                let constrain = self.overflow_option(dom, options)?;
                self.month_day_from_bag(&bag, constrain)
            }
            JsValue::String(text) => {
                let Some(parsed) = super::parse::parse_month_day(&(text).to_lossy()) else {
                    return Err(self.range_error("invalid ISO month-day string"));
                };
                if parsed
                    .calendar
                    .as_deref()
                    .is_some_and(|calendar| calendar != "iso8601")
                {
                    return Err(self.range_error("only the iso8601 calendar is supported"));
                }
                self.overflow_option(dom, options)?;
                // The month and day must exist in the reference year; `overflow`
                // does not apply to an ISO string.
                regulate_date(
                    DEFAULT_REFERENCE_YEAR,
                    i64::from(parsed.date.month),
                    i64::from(parsed.date.day),
                    false,
                )
                .ok_or_else(|| self.range_error("month-day string is out of range"))
            }
            _ => Err(JsError::type_error(
                "expected a Temporal.PlainMonthDay, an object or an ISO string",
            )),
        }
    }

    /// The month, monthCode, day and year fields of a month-day bag.
    fn read_month_day_bag(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
    ) -> Result<super::plain_date::DateBag, JsError> {
        let mut bag = super::plain_date::DateBag::default();
        let day = self.get_member(dom, object, "day")?;
        if !matches!(day, JsValue::Undefined) {
            bag.day = Some(self.to_integer_with_truncation(dom, &day)?);
        }
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

    /// The month-day a bag names. A `year` in the bag sets the reference year.
    fn month_day_from_bag(
        &mut self,
        bag: &super::plain_date::DateBag,
        constrain: bool,
    ) -> Result<IsoDate, JsError> {
        let year = bag.year.map_or(DEFAULT_REFERENCE_YEAR, |year| year as i32);
        let reference = super::plain_date::DateBag {
            year: Some(f64::from(year)),
            ..bag.clone()
        };
        let date = self.date_from_bag(&reference, constrain)?;
        Ok(IsoDate {
            year: DEFAULT_REFERENCE_YEAR,
            ..date
        })
    }

    pub(super) fn plain_month_day_construct(
        &mut self,
        dom: &mut Dom,
        new_target: &JsValue,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let month = self.to_integer_with_truncation(dom, &argument(arguments, 0))?;
        let day = self.to_integer_with_truncation(dom, &argument(arguments, 1))?;
        self.calendar_id(&argument(arguments, 2))?;
        let reference = match argument(arguments, 3) {
            JsValue::Undefined => f64::from(DEFAULT_REFERENCE_YEAR),
            value => self.to_integer_with_truncation(dom, &value)?,
        };
        if !(-1e6..=1e6).contains(&month)
            || !(-1e6..=1e6).contains(&day)
            || !(-1e6..=1e6).contains(&reference)
        {
            return Err(self.range_error("month-day fields are out of range"));
        }
        let Some(date) = regulate_date(reference as i32, month as i64, day as i64, false) else {
            return Err(self.range_error("month-day fields are out of range"));
        };
        if !date_within_limits(date) {
            return Err(
                self.range_error("Temporal.PlainMonthDay is outside the representable range")
            );
        }
        let prototype =
            self.temporal_prototype_for(dom, TemporalKind::PlainMonthDay, new_target)?;
        self.ensure_heap_capacity(1)?;
        Ok(JsValue::Object(self.realm.allocate_host(
            Some(prototype),
            ObjectHost::Temporal(TemporalValue::PlainMonthDay(date)),
        )))
    }

    /// `toString`: `--MM-DD`, or with the reference year and calendar when
    /// `calendarName` asks for them.
    fn month_day_to_string(
        &mut self,
        dom: &mut Dom,
        date: IsoDate,
        options: &JsValue,
    ) -> Result<String, JsError> {
        let options = Self::options_object(options)?;
        let display = self.calendar_name_option(dom, options)?;
        Ok(match display {
            CalendarDisplay::Auto | CalendarDisplay::Never => {
                format!("--{:02}-{:02}", date.month, date.day)
            }
            _ => format!("{}{}", date_text(date), calendar_text(display)),
        })
    }

    /// The `PlainMonthDay` statics, getters and methods, dispatched by name.
    pub(super) fn plain_month_day_native(
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
                let date = self.to_temporal_month_day(dom, &first, options)?;
                self.month_day_value(date)
            }
            TemporalMethod::With => {
                let date = self.month_day_of(receiver)?;
                let JsValue::Object(object) = first else {
                    return Err(JsError::type_error("with() requires an object"));
                };
                let calendar = self.get_member(dom, object, "calendar")?;
                if !matches!(calendar, JsValue::Undefined) {
                    return Err(JsError::type_error("with() does not accept a calendar"));
                }
                let bag = self.read_month_day_bag(dom, object)?;
                if bag.day.is_none()
                    && bag.month.is_none()
                    && bag.month_code.is_none()
                    && bag.year.is_none()
                {
                    return Err(JsError::type_error("with() requires at least one field"));
                }
                let options = Self::options_object(&argument(arguments, 1))?;
                let constrain = self.overflow_option(dom, options)?;
                let replaces_month = bag.month.is_some() || bag.month_code.is_some();
                let merged = super::plain_date::DateBag {
                    day: Some(bag.day.unwrap_or(f64::from(date.day))),
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
                    year: Some(f64::from(DEFAULT_REFERENCE_YEAR)),
                };
                let result = self.date_from_bag(&merged, constrain)?;
                self.month_day_value(IsoDate {
                    year: DEFAULT_REFERENCE_YEAR,
                    ..result
                })
            }
            TemporalMethod::Equals => {
                let date = self.month_day_of(receiver)?;
                let other = self.to_temporal_month_day(dom, &first, None)?;
                Ok(JsValue::Boolean(date == other))
            }
            TemporalMethod::ToString => {
                let date = self.month_day_of(receiver)?;
                Ok(JsValue::String(
                    self.month_day_to_string(dom, date, &first)?.into(),
                ))
            }
            TemporalMethod::ToJSON | TemporalMethod::ToLocaleString => {
                let date = self.month_day_of(receiver)?;
                Ok(JsValue::String(
                    (format!("--{:02}-{:02}", date.month, date.day)).into(),
                ))
            }
            TemporalMethod::ValueOf => Err(JsError::type_error(
                "Temporal.PlainMonthDay cannot be converted to a primitive; use equals() or toString()",
            )),
            TemporalMethod::ToPlainDate => {
                let date = self.month_day_of(receiver)?;
                let JsValue::Object(object) = first else {
                    return Err(JsError::type_error(
                        "toPlainDate() requires an object with a year",
                    ));
                };
                let year = self.get_member(dom, object, "year")?;
                if matches!(year, JsValue::Undefined) {
                    return Err(JsError::type_error("toPlainDate() requires a year"));
                }
                let year = self.to_integer_with_truncation(dom, &year)?;
                if !(-1e6..=1e6).contains(&year) {
                    return Err(self.range_error("year is out of range"));
                }
                let Some(full) = regulate_date(
                    year as i32,
                    i64::from(date.month),
                    i64::from(date.day),
                    true,
                ) else {
                    return Err(self.range_error("date is out of range"));
                };
                self.plain_date_value(full)
            }
            other => {
                let date = self.month_day_of(receiver)?;
                Ok(match other {
                    TemporalMethod::CalendarId => JsValue::String(("iso8601".to_owned()).into()),
                    TemporalMethod::MonthCode => JsValue::String((month_code(date.month)).into()),
                    TemporalMethod::Day => JsValue::Number(f64::from(date.day)),
                    _ => {
                        return Err(JsError::type_error(
                            "unsupported Temporal.PlainMonthDay member",
                        ));
                    }
                })
            }
        }
    }
}

/// The members of `Temporal.PlainMonthDay` and its prototype.
pub(super) const SURFACE: Surface = Surface {
    constructor_length: Some(2),
    statics: &[member("from", TemporalMethod::From, 1)],
    methods: &[
        member("with", TemporalMethod::With, 1),
        member("equals", TemporalMethod::Equals, 1),
        member("toString", TemporalMethod::ToString, 0),
        member("toJSON", TemporalMethod::ToJSON, 0),
        member("toLocaleString", TemporalMethod::ToLocaleString, 0),
        member("valueOf", TemporalMethod::ValueOf, 0),
        member("toPlainDate", TemporalMethod::ToPlainDate, 1),
    ],
    getters: &[
        ("calendarId", TemporalMethod::CalendarId),
        ("monthCode", TemporalMethod::MonthCode),
        ("day", TemporalMethod::Day),
    ],
};
