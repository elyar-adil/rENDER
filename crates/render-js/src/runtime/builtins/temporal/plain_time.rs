//! `Temporal.PlainTime`: a wall-clock time of day, with no date and no zone.

use super::duration::{DurationRecord, balance_time};
use super::format::time_text;
use super::iso::{IsoTime, NANOS_PER_DAY, time_from_nanos, time_nanos};
use super::options::UnitGroup;
use super::rounding::{RoundingMode, Unit, increment_is_valid, round_to_increment};
use super::{Surface, TemporalKind, TemporalMethod, TemporalValue, argument, member};
use crate::JsError;
use crate::JsValue;
use crate::ObjectId;
use crate::runtime::JsRuntime;
use render_dom::Dom;

/// The time fields a property bag may name, in the order they are read.
const TIME_FIELDS: [&str; 6] = [
    "hour",
    "microsecond",
    "millisecond",
    "minute",
    "nanosecond",
    "second",
];

/// The fields of a time in the order `hour` through `nanosecond`.
fn fields_of(time: IsoTime) -> [i64; 6] {
    [
        i64::from(time.hour),
        i64::from(time.minute),
        i64::from(time.second),
        i64::from(time.millisecond),
        i64::from(time.microsecond),
        i64::from(time.nanosecond),
    ]
}

/// A time of day from nanoseconds that may be any integer, wrapped into one day.
pub(super) fn wrap_day(nanos: i128) -> IsoTime {
    time_from_nanos(nanos.rem_euclid(NANOS_PER_DAY))
}

impl JsRuntime {
    /// The time of `receiver`, or a `TypeError` for another value.
    fn time_of(&self, receiver: ObjectId) -> Result<IsoTime, JsError> {
        match self.temporal_host(receiver) {
            Some(TemporalValue::PlainTime(time)) => Ok(time),
            _ => Err(JsError::type_error("receiver is not a Temporal.PlainTime")),
        }
    }

    fn plain_time_value(&mut self, time: IsoTime) -> Result<JsValue, JsError> {
        self.temporal_instance(TemporalKind::PlainTime, TemporalValue::PlainTime(time))
    }

    /// `RegulateTime` on fields that may be any integer: constrained into
    /// range, or refused when `constrain` is false.
    fn regulate_time_fields(
        &mut self,
        fields: [i64; 6],
        constrain: bool,
    ) -> Result<IsoTime, JsError> {
        super::iso::regulate_time(fields, constrain)
            .ok_or_else(|| self.range_error("time fields are out of range"))
    }

    /// The numeric time fields of a property bag, read in specification order.
    /// Returns the fields with `None` for each absent one.
    fn read_time_bag(
        &mut self,
        dom: &mut Dom,
        object: ObjectId,
    ) -> Result<[Option<f64>; 6], JsError> {
        let mut values = [None; 6];
        for name in TIME_FIELDS {
            let value = self.get_member(dom, object, name)?;
            if matches!(value, JsValue::Undefined) {
                continue;
            }
            let number = self.to_integer_with_truncation(dom, &value)?;
            let slot = match name {
                "hour" => 0,
                "minute" => 1,
                "second" => 2,
                "millisecond" => 3,
                "microsecond" => 4,
                _ => 5,
            };
            values[slot] = Some(number);
        }
        Ok(values)
    }

    /// `ToTemporalTime(item, options)`: a `PlainTime`, a `PlainDateTime`'s time, a
    /// property bag with at least one time field, or an ISO time string.
    pub(super) fn to_temporal_plain_time(
        &mut self,
        dom: &mut Dom,
        item: &JsValue,
        options: Option<ObjectId>,
    ) -> Result<IsoTime, JsError> {
        match item {
            JsValue::Object(object) => {
                if let Some(value) = self.temporal_host(*object) {
                    let time = match value {
                        TemporalValue::PlainTime(time) => time,
                        TemporalValue::PlainDateTime(datetime) => datetime.time,
                        _ => {
                            return Err(JsError::type_error("expected a Temporal time-like value"));
                        }
                    };
                    self.overflow_option(dom, options)?;
                    return Ok(time);
                }
                let calendar = self.get_member(dom, *object, "calendar")?;
                self.calendar_id(&calendar)?;
                let values = self.read_time_bag(dom, *object)?;
                if values.iter().all(Option::is_none) {
                    return Err(JsError::type_error(
                        "a time-like object needs at least one time field",
                    ));
                }
                let constrain = self.overflow_option(dom, options)?;
                let fields = values
                    .map(|value| value.unwrap_or(0.0))
                    .map(|value| value as i64);
                let fields = [
                    fields[0], fields[1], fields[2], fields[3], fields[4], fields[5],
                ];
                self.regulate_time_fields(fields, constrain)
            }
            JsValue::String(text) => {
                let constrain = self.overflow_option(dom, options)?;
                let Some(parsed) = super::parse::parse_time_string(text) else {
                    return Err(self.range_error("invalid ISO time string"));
                };
                if parsed.utc {
                    return Err(self.range_error("a time string may not have a UTC designator"));
                }
                if parsed
                    .calendar
                    .as_deref()
                    .is_some_and(|calendar| calendar != "iso8601")
                {
                    return Err(self.range_error("only the iso8601 calendar is supported"));
                }
                let Some(time) = parsed.time else {
                    return Err(self.range_error("invalid ISO time string"));
                };
                self.regulate_time_fields(fields_of(time), constrain)
            }
            _ => Err(JsError::type_error(
                "expected a Temporal.PlainTime, an object or an ISO string",
            )),
        }
    }

    pub(super) fn plain_time_construct(
        &mut self,
        dom: &mut Dom,
        new_target: &JsValue,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let mut fields = [0_i64; 6];
        for (index, slot) in fields.iter_mut().enumerate() {
            let value = argument(arguments, index);
            if !matches!(value, JsValue::Undefined) {
                let number = self.to_integer_with_truncation(dom, &value)?;
                if !(-1e15..=1e15).contains(&number) {
                    return Err(self.range_error("time fields are out of range"));
                }
                *slot = number as i64;
            }
        }
        let time = self.regulate_time_fields(fields, false)?;
        let prototype = self.temporal_prototype_for(dom, TemporalKind::PlainTime, new_target)?;
        self.ensure_heap_capacity(1)?;
        Ok(JsValue::Object(self.realm.allocate_host(
            Some(prototype),
            crate::value::ObjectHost::Temporal(TemporalValue::PlainTime(time)),
        )))
    }

    /// `with(timeLike, options)`: the time with the named fields replaced.
    fn time_with(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let time = self.time_of(receiver)?;
        let JsValue::Object(object) = argument(arguments, 0) else {
            return Err(JsError::type_error("with() requires an object"));
        };
        let values = self.read_time_bag(dom, object)?;
        if values.iter().all(Option::is_none) {
            return Err(JsError::type_error(
                "with() requires at least one time field",
            ));
        }
        let options = Self::options_object(&argument(arguments, 1))?;
        let constrain = self.overflow_option(dom, options)?;
        let base = fields_of(time);
        let mut merged = [0_i64; 6];
        for (index, slot) in merged.iter_mut().enumerate() {
            *slot = values[index].map_or(base[index], |value| value as i64);
        }
        let result = self.regulate_time_fields(merged, constrain)?;
        self.plain_time_value(result)
    }

    /// `add` and `subtract`: the time part of the duration (days are not
    /// part of a time of day) added to the time, wrapped into one day.
    fn time_add(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        item: &JsValue,
        sign: i128,
    ) -> Result<JsValue, JsError> {
        let time = self.time_of(receiver)?;
        let duration = self.to_temporal_duration(dom, item)?;
        let sum = time_nanos(time) + sign * duration.time_nanos();
        self.plain_time_value(wrap_day(sum))
    }

    /// `until` and `since`: the difference of two times, rounded and balanced
    /// up to hours.
    fn time_difference(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
        since: bool,
    ) -> Result<JsValue, JsError> {
        let from = self.time_of(receiver)?;
        let to = self.to_temporal_plain_time(dom, &argument(arguments, 0), None)?;
        let options = Self::options_object(&argument(arguments, 1))?;
        let settings = self.difference_settings(
            dom,
            options,
            since,
            UnitGroup::Time,
            Unit::Nanosecond,
            Unit::Hour,
        )?;
        let unit_nanos = settings.smallest.nanos().unwrap_or(1);
        let rounded = round_to_increment(
            time_nanos(to) - time_nanos(from),
            unit_nanos * settings.increment,
            settings.mode,
        );
        let signed = if since { -rounded } else { rounded };
        let parts = balance_time(signed, settings.largest);
        let record = DurationRecord::from_array([
            0.0,
            0.0,
            0.0,
            0.0,
            parts[1] as f64,
            parts[2] as f64,
            parts[3] as f64,
            parts[4] as f64,
            parts[5] as f64,
            parts[6] as f64,
        ]);
        self.temporal_instance(TemporalKind::Duration, TemporalValue::Duration(record))
    }

    /// `round(roundTo)`: the time rounded to `smallestUnit`, wrapped into one day.
    fn time_round(
        &mut self,
        dom: &mut Dom,
        receiver: ObjectId,
        arguments: &[JsValue],
    ) -> Result<JsValue, JsError> {
        let time = self.time_of(receiver)?;
        let round_to = argument(arguments, 0);
        let (increment, mode, smallest) = match &round_to {
            JsValue::Undefined => {
                return Err(JsError::type_error("round requires an options argument"));
            }
            JsValue::String(text) => match Unit::parse(text) {
                Some(unit) => (1, RoundingMode::HalfExpand, Some(unit)),
                None => return Err(self.range_error("invalid smallestUnit")),
            },
            other => {
                let options = Self::options_object(other)?;
                let increment = self.rounding_increment_option(dom, options)?;
                let mode = self.rounding_mode_option(dom, options, RoundingMode::HalfExpand)?;
                let smallest =
                    self.unit_in_group(dom, options, "smallestUnit", UnitGroup::Time, false)?;
                (increment, mode, smallest)
            }
        };
        let Some(unit) = smallest else {
            return Err(self.range_error("round requires smallestUnit"));
        };
        if !increment_is_valid(unit, increment) {
            return Err(self.range_error("roundingIncrement is not valid for this unit"));
        }
        let rounded = round_to_increment(
            time_nanos(time),
            unit.nanos().unwrap_or(1) * increment,
            mode,
        );
        self.plain_time_value(wrap_day(rounded))
    }

    /// `toString` and `toJSON`, with the precision and rounding the options set.
    fn time_to_string(
        &mut self,
        dom: &mut Dom,
        time: IsoTime,
        options: &JsValue,
    ) -> Result<String, JsError> {
        let options = Self::options_object(options)?;
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
        let rounded = wrap_day(round_to_increment(time_nanos(time), unit_nanos, mode));
        Ok(time_text(rounded, precision))
    }

    /// The `PlainTime` statics, getters and methods, dispatched by name.
    pub(super) fn plain_time_native(
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
                let time = self.to_temporal_plain_time(dom, &first, options)?;
                self.plain_time_value(time)
            }
            TemporalMethod::Compare => {
                let one = self.to_temporal_plain_time(dom, &first, None)?;
                let two = self.to_temporal_plain_time(dom, &argument(arguments, 1), None)?;
                Ok(JsValue::Number(
                    match time_nanos(one).cmp(&time_nanos(two)) {
                        std::cmp::Ordering::Less => -1.0,
                        std::cmp::Ordering::Equal => 0.0,
                        std::cmp::Ordering::Greater => 1.0,
                    },
                ))
            }
            TemporalMethod::With => self.time_with(dom, receiver, arguments),
            TemporalMethod::Add | TemporalMethod::Subtract => {
                let sign = if method == TemporalMethod::Add { 1 } else { -1 };
                self.time_add(dom, receiver, &first, sign)
            }
            TemporalMethod::Until => self.time_difference(dom, receiver, arguments, false),
            TemporalMethod::Since => self.time_difference(dom, receiver, arguments, true),
            TemporalMethod::Round => self.time_round(dom, receiver, arguments),
            TemporalMethod::Equals => {
                let time = self.time_of(receiver)?;
                let other = self.to_temporal_plain_time(dom, &first, None)?;
                Ok(JsValue::Boolean(time == other))
            }
            TemporalMethod::ToString => {
                let time = self.time_of(receiver)?;
                Ok(JsValue::String(self.time_to_string(dom, time, &first)?))
            }
            // `toJSON` and `toLocaleString` take no options, so none are read.
            TemporalMethod::ToJSON | TemporalMethod::ToLocaleString => {
                let time = self.time_of(receiver)?;
                Ok(JsValue::String(self.time_to_string(
                    dom,
                    time,
                    &JsValue::Undefined,
                )?))
            }
            TemporalMethod::ValueOf => Err(JsError::type_error(
                "Temporal.PlainTime cannot be converted to a primitive; use compare() or toString()",
            )),
            other => {
                let time = self.time_of(receiver)?;
                Ok(JsValue::Number(match other {
                    TemporalMethod::Hour => f64::from(time.hour),
                    TemporalMethod::Minute => f64::from(time.minute),
                    TemporalMethod::Second => f64::from(time.second),
                    TemporalMethod::Millisecond => f64::from(time.millisecond),
                    TemporalMethod::Microsecond => f64::from(time.microsecond),
                    TemporalMethod::Nanosecond => f64::from(time.nanosecond),
                    _ => return Err(JsError::type_error("unsupported Temporal.PlainTime member")),
                }))
            }
        }
    }
}

/// The members of `Temporal.PlainTime` and its prototype.
pub(super) const SURFACE: Surface = Surface {
    constructor_length: Some(0),
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
        member("round", TemporalMethod::Round, 1),
        member("equals", TemporalMethod::Equals, 1),
        member("toString", TemporalMethod::ToString, 0),
        member("toJSON", TemporalMethod::ToJSON, 0),
        member("toLocaleString", TemporalMethod::ToLocaleString, 0),
        member("valueOf", TemporalMethod::ValueOf, 0),
    ],
    getters: &[
        ("hour", TemporalMethod::Hour),
        ("minute", TemporalMethod::Minute),
        ("second", TemporalMethod::Second),
        ("millisecond", TemporalMethod::Millisecond),
        ("microsecond", TemporalMethod::Microsecond),
        ("nanosecond", TemporalMethod::Nanosecond),
    ],
};
